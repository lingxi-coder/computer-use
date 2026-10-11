import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import test from 'node:test';
import { auditOwnedInputJournal } from './owned-input-journal.mjs';

const expected = { action: 'ax_set_owned_document_value' as const, root: 'root-test', attempt: 'attempt-test', intent: 'intent-test', session: 9,
  inputStart: 1305, inputEnd: 1320, registration: 'registration-test' };
const prepared = {
  action: expected.action, attempt_id: expected.attempt, clock_domain: 'macos/9/wall', command_end_ms: 1400,
  dependencies: { authorization: 1, focus: 2, owned_scope: 3, window: 4 }, duration_ms: 100,
  evidence_id: 'evidence-test', guard_revision: 7, intent_evidence_id: 'original-test',
  intent_id: expected.intent, kind: 'prepared', latest_start_ms: 1310,
  native_deadline_uptime_ms: 9000, observed_ms: 1000, permit_generation: 1, permit_id: 'permit-test',
  previous_checksum: '0'.repeat(64), registration_id: 'registration-test', request_id: 'request-test',
  root_epoch: 2, scope_digest: 'b'.repeat(64), sequence: 1, session_epoch: 9, source_sequence: 5,
  started_ms: 1300, stop_seq: 6, surface_epoch: 1, surface_id: 'main', task_id: expected.root,
  task_revision: 3, valid_until_ms: 1390,
};
function sorted(value: unknown): unknown {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? Object.fromEntries(Object.keys(value).sort().map(key => [key, sorted((value as Record<string, unknown>)[key])])) : value;
}
function entry(payload: Record<string, unknown>) {
  const checksum = createHash('sha256').update(JSON.stringify(sorted(payload))).digest('hex');
  return { checksum, payload: sorted(payload) };
}
function records(change: Record<string, unknown> = {}): Buffer {
  const first = entry(prepared);
  const second = entry({ ...prepared, kind: 'terminal', sequence: 2, previous_checksum: first.checksum,
    outcome: 'applied', submitted: true, committed_source_ms: expected.inputStart,
    returned_source_ms: expected.inputEnd, ...change });
  return Buffer.from(`${JSON.stringify(first)}\n${JSON.stringify(second)}\n`);
}

test('independent owned journal audit requires an exact durably recorded single input span', () => {
  assert.equal(auditOwnedInputJournal(records(), expected).applied_terminal, true);
  assert.ok(records().toString().split('\n').every(line => Buffer.byteLength(line) + 1 <= 4096));
});
test('prepared death, incomplete tail and extra input cannot be accepted', () => {
  const lines = records().toString().split('\n');
  for (const bytes of [Buffer.from(`${lines[0]}\n`), records().subarray(0, -1), Buffer.concat([records(), records()])]) {
    assert.throws(() => auditOwnedInputJournal(bytes, expected));
  }
});
test('unknown, changed IDs, authority or input timestamps fail even with recomputed checksums', () => {
  for (const change of [
    { action: 'ax_save_document' }, { action: 'keypress' }, { outcome: 'unknown' }, { submitted: false }, { intent_id: 'replacement' }, { registration_id: 'replacement' },
    { session_epoch: 10 }, { committed_source_ms: expected.inputStart + 1 }, { returned_source_ms: expected.inputEnd + 1 },
    { dependencies: { ...prepared.dependencies, focus: 8 } }, { previous_checksum: 'c'.repeat(64) },
    { value: 'unexpected body' }, { document_path: '/private/tmp/forged/document.txt' },
    { valid_until_ms: 1500 }, { command_end_ms: 1401 },
  ]) assert.throws(() => auditOwnedInputJournal(records(change), expected));
  assert.throws(() => auditOwnedInputJournal(records(), { ...expected, registration: 'other-launch' }));
});
test('raw tampering and duplicate keys cannot survive canonical checksum verification', () => {
  const original = records().toString();
  for (const text of [original.replace('permit-test', 'permit-fake'),
    original.replace('"kind":"prepared"', '"kind":"ignored","kind":"prepared"'),
    original.replace('"checksum":', '"checksum": "ignored","checksum":')]) {
    assert.throws(() => auditOwnedInputJournal(Buffer.from(text), expected));
  }
});

test('Save has a distinct action and cannot masquerade as a setter', () => {
  const saveExpected = { ...expected, action: 'ax_save_document' as const };
  const first = entry({ ...prepared, action: saveExpected.action });
  const second = entry({ ...prepared, action: saveExpected.action, kind: 'terminal', sequence: 2,
    previous_checksum: first.checksum, outcome: 'applied', submitted: true,
    committed_source_ms: expected.inputStart, returned_source_ms: expected.inputEnd });
  const bytes = Buffer.from(`${JSON.stringify(first)}\n${JSON.stringify(second)}\n`);
  assert.deepEqual(auditOwnedInputJournal(bytes, saveExpected).actions, ['ax_save_document']);
  assert.throws(() => auditOwnedInputJournal(bytes, expected));
  assert.throws(() => auditOwnedInputJournal(records(), saveExpected));
});

test('ordered setter and Save require two distinct exact authority pairs on the same chain', () => {
  const firstPair = records().toString().trim().split('\n');
  const firstTerminal = JSON.parse(firstPair[1]!);
  const saveExpected = { ...expected, action: 'ax_save_document' as const, attempt: 'save-attempt',
    intent: 'save-intent', inputStart: 1350, inputEnd: 1360 };
  const savePrepared = { ...prepared, action: saveExpected.action, attempt_id: saveExpected.attempt,
    intent_id: saveExpected.intent, request_id: 'save-request', sequence: 3,
    previous_checksum: firstTerminal.checksum, started_ms: 1340, duration_ms: 60, latest_start_ms: 1355 };
  const third = entry(savePrepared);
  const fourth = entry({ ...savePrepared, kind: 'terminal', sequence: 4, previous_checksum: third.checksum,
    outcome: 'applied', submitted: true, committed_source_ms: saveExpected.inputStart,
    returned_source_ms: saveExpected.inputEnd });
  const bytes = Buffer.from([...firstPair, JSON.stringify(third), JSON.stringify(fourth)].join('\n') + '\n');
  assert.equal(auditOwnedInputJournal(bytes, [expected, saveExpected]).records, 4);
  assert.throws(() => auditOwnedInputJournal(bytes, [saveExpected, expected]));
  assert.throws(() => auditOwnedInputJournal(bytes, [expected, { ...saveExpected, intent: expected.intent }]));
  assert.throws(() => auditOwnedInputJournal(bytes, [expected, { ...saveExpected, attempt: expected.attempt }]));
  assert.throws(() => auditOwnedInputJournal(bytes, expected));
});

test('journal file reads reject oversized, symlinked and exposed files before auditing', async t => {
  const { mkdtempSync, writeFileSync, chmodSync, symlinkSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { readOwnedInputJournalFile } = await import('./owned-input-journal.mjs');
  const directory = mkdtempSync(join(tmpdir(), 'cu-owned-journal-read-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const file = join(directory, 'journal.jsonl');
  writeFileSync(file, records(), { mode: 0o600 });
  assert.equal(auditOwnedInputJournal(readOwnedInputJournalFile(file), expected).applied_terminal, true);
  chmodSync(file, 0o644);
  assert.throws(() => readOwnedInputJournalFile(file));
  chmodSync(file, 0o600);
  symlinkSync(file, join(directory, 'link.jsonl'));
  assert.throws(() => readOwnedInputJournalFile(join(directory, 'link.jsonl')));
  writeFileSync(file, Buffer.alloc(256 * 1024 + 1));
  assert.throws(() => readOwnedInputJournalFile(file));
});
