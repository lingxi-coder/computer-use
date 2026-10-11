import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { closeSync, constants, fstatSync, lstatSync, openSync, readSync, type Stats } from 'node:fs';

const AUTHORITY_KEYS = ['action', 'attempt_id', 'clock_domain', 'command_end_ms', 'dependencies', 'duration_ms',
  'evidence_id', 'guard_revision', 'intent_evidence_id', 'intent_id', 'latest_start_ms',
  'native_deadline_uptime_ms', 'observed_ms', 'permit_generation', 'permit_id', 'registration_id',
  'request_id', 'root_epoch', 'scope_digest', 'session_epoch', 'source_sequence', 'started_ms',
  'stop_seq', 'surface_epoch', 'surface_id', 'task_id', 'task_revision', 'valid_until_ms'].sort();
const ENVELOPE_KEYS = ['kind', 'previous_checksum', 'sequence'];
const TERMINAL_KEYS = ['committed_source_ms', 'outcome', 'returned_source_ms', 'submitted'];
const HEX = /^[0-9a-f]{64}$/;
const ZERO = '0'.repeat(64);
const MAX_JOURNAL_BYTES = 256 * 1024;

/** Bound allocation before reading, and reject replacement or an incomplete snapshot. */
export function readOwnedInputJournalFile(path: string): Buffer {
  let fd: number | null = null;
  const bytes = Buffer.alloc(MAX_JOURNAL_BYTES + 1);
  try {
    const eligible = (value: Stats): boolean => value.isFile() && !value.isSymbolicLink()
      && value.uid === process.getuid?.() && value.nlink === 1 && (value.mode & 0o777) === 0o600
      && value.size > 0 && value.size <= MAX_JOURNAL_BYTES;
    const same = (a: Stats, b: Stats): boolean => eligible(b) && a.dev === b.dev && a.ino === b.ino
      && a.size === b.size && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs;
    const before = lstatSync(path);
    assert.ok(eligible(before), 'journal file is outside its private size or identity policy');
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = fstatSync(fd);
    assert.ok(same(before, opened), 'journal file changed before opening');
    const count = readSync(fd, bytes, 0, bytes.length, 0);
    assert.ok(count === opened.size && same(opened, fstatSync(fd)) && same(opened, lstatSync(path)),
      'journal file changed during the bounded read');
    return Buffer.from(bytes.subarray(0, count));
  } finally { bytes.fill(0); if (fd !== null) closeSync(fd); }
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical((value as Record<string, unknown>)[key])}`).join(',')}}`;
  }
  const text = JSON.stringify(value);
  assert.ok(text !== undefined, 'journal has unsupported JSON value');
  return text;
}
function checksum(payload: unknown): string { return createHash('sha256').update(canonical(payload)).digest('hex'); }
function exactKeys(value: Record<string, unknown>, keys: string[]): void {
  assert.deepEqual(Object.keys(value).sort(), [...keys].sort(), 'journal schema mismatch');
}
function positive(value: unknown): asserts value is number {
  assert.ok(Number.isSafeInteger(value) && (value as number) > 0, 'journal positive integer invalid');
}

export type OwnedInputAction = 'ax_set_owned_document_value' | 'ax_save_document';

export interface ExpectedOwnedInput {
  action: OwnedInputAction;
  root: string;
  attempt: string;
  intent: string;
  session: number;
  registration: string;
  inputStart: number;
  inputEnd: number;
}

/** Independent audit of ordered actions; returns no private body, path or identity. */
export function auditOwnedInputJournal(bytes: Buffer, expectedInput: ExpectedOwnedInput | readonly ExpectedOwnedInput[]) {
  const expectedActions = Array.isArray(expectedInput) ? expectedInput as readonly ExpectedOwnedInput[] : [expectedInput as ExpectedOwnedInput];
  assert.ok(expectedActions.length > 0 && expectedActions.length <= 128, 'expected action bound violated');
  assert.equal(new Set(expectedActions.map(value => value.intent)).size, expectedActions.length, 'input intent repeated');
  assert.equal(new Set(expectedActions.map(value => value.attempt)).size, expectedActions.length, 'input attempt repeated');
  assert.ok(bytes.length > 0 && bytes.length <= MAX_JOURNAL_BYTES, 'journal file bound violated');
  assert.equal(bytes.at(-1), 10, 'journal has a partial final record');
  const lines = bytes.toString('utf8').slice(0, -1).split('\n');
  assert.equal(lines.length, expectedActions.length * 2, 'each action requires exactly one prepared/terminal pair');
  let previous = ZERO;
  const payloads: Array<Record<string, unknown>> = [];
  for (let index = 0; index < lines.length; index++) {
    const expected = expectedActions[Math.floor(index / 2)]!;
    assert.ok(['ax_set_owned_document_value', 'ax_save_document'].includes(expected.action), 'expected action invalid');
    const terminalRecord = index % 2 === 1;
    const line = lines[index]!;
    assert.ok(Buffer.byteLength(line, 'utf8') + 1 <= 4096, 'journal record bound violated');
    const entry = JSON.parse(line) as Record<string, unknown>;
    exactKeys(entry, ['checksum', 'payload']);
    assert.equal(canonical(entry), line, 'journal is not canonical or contains duplicate keys');
    assert.ok(typeof entry.checksum === 'string' && HEX.test(entry.checksum), 'journal checksum invalid');
    assert.ok(entry.payload && typeof entry.payload === 'object' && !Array.isArray(entry.payload), 'journal payload invalid');
    const payload = entry.payload as Record<string, unknown>;
    exactKeys(payload, [...AUTHORITY_KEYS, ...ENVELOPE_KEYS, ...(terminalRecord ? TERMINAL_KEYS : [])]);
    assert.equal(payload.kind, terminalRecord ? 'terminal' : 'prepared');
    assert.equal(payload.action, expected.action, 'journal action differs from the authorized primitive');
    assert.equal(payload.sequence, index + 1);
    assert.equal(payload.previous_checksum, previous, 'journal chain broken');
    assert.equal(checksum(payload), entry.checksum, 'journal checksum mismatch');
    assert.equal(payload.task_id, expected.root);
    assert.equal(payload.attempt_id, expected.attempt);
    assert.equal(payload.intent_id, expected.intent);
    assert.equal(payload.session_epoch, expected.session);
    assert.equal(payload.registration_id, expected.registration);
    for (const key of ['request_id', 'task_id', 'attempt_id', 'intent_id', 'permit_id', 'registration_id',
      'evidence_id', 'intent_evidence_id', 'clock_domain', 'surface_id']) {
      assert.ok(typeof payload[key] === 'string' && (payload[key] as string).length > 0
        && Buffer.byteLength(payload[key] as string, 'utf8') <= 256
        && !/[\u0000-\u001f\u007f-\u009f]/.test(payload[key] as string), 'journal identifier invalid');
    }
    assert.ok(typeof payload.scope_digest === 'string' && HEX.test(payload.scope_digest));
    for (const key of ['root_epoch', 'task_revision', 'permit_generation', 'session_epoch', 'source_sequence',
      'surface_epoch', 'started_ms', 'observed_ms', 'valid_until_ms', 'command_end_ms', 'duration_ms',
      'latest_start_ms', 'native_deadline_uptime_ms']) positive(payload[key]);
    for (const key of ['guard_revision', 'stop_seq']) {
      assert.ok(Number.isSafeInteger(payload[key]) && (payload[key] as number) >= 0);
    }
    assert.ok(payload.dependencies && typeof payload.dependencies === 'object' && !Array.isArray(payload.dependencies));
    const deps = payload.dependencies as Record<string, unknown>;
    exactKeys(deps, ['authorization', 'focus', 'window', 'owned_scope']);
    for (const revision of Object.values(deps)) positive(revision);
    assert.ok((payload.observed_ms as number) <= (payload.started_ms as number));
    assert.ok((payload.started_ms as number) <= (payload.latest_start_ms as number));
    assert.ok((payload.valid_until_ms as number) <= (payload.command_end_ms as number));
    assert.equal(payload.command_end_ms, (payload.started_ms as number) + (payload.duration_ms as number));
    assert.ok((payload.duration_ms as number) <= 1500, 'owned action duration exceeded its policy');
    payloads.push(payload); previous = entry.checksum;
  }
  assert.equal(new Set(payloads.filter((_, index) => index % 2 === 0).map(payload => payload.request_id)).size,
    expectedActions.length, 'journal request replayed');
  for (let index = 0; index < expectedActions.length; index++) {
    const expected = expectedActions[index]!;
    const prepared = payloads[index * 2]!, terminal = payloads[index * 2 + 1]!;
    for (const key of AUTHORITY_KEYS) assert.deepEqual(terminal[key], prepared[key], 'terminal authority changed');
    assert.equal(terminal.outcome, 'applied', 'journal does not prove an applied owned action');
    assert.equal(terminal.submitted, true);
    positive(expected.inputStart); positive(expected.inputEnd);
    positive(terminal.committed_source_ms); positive(terminal.returned_source_ms);
    assert.equal(terminal.committed_source_ms, expected.inputStart);
    assert.equal(terminal.returned_source_ms, expected.inputEnd);
    assert.ok(expected.inputStart <= expected.inputEnd && expected.inputStart >= (prepared.started_ms as number)
      && expected.inputStart < (prepared.valid_until_ms as number)
      && expected.inputEnd <= (prepared.valid_until_ms as number) + 500);
    if (index > 0) {
      const prior = expectedActions[index - 1]!;
      assert.equal(expected.root, prior.root, 'owned diagnostic root changed');
      assert.equal(expected.session, prior.session, 'owned diagnostic session changed');
      assert.equal(expected.registration, prior.registration, 'owned diagnostic registration changed');
      assert.ok(expected.inputStart >= prior.inputEnd, 'owned actions overlapped or reversed');
    }
  }
  return { records: lines.length, actions: expectedActions.map(value => value.action),
    checksum_chain_verified: true, exact_authority_pair: true,
    input_span_matches_effect: true, applied_terminal: true };
}
