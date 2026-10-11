#!/usr/bin/env node
import { createHash, randomUUID } from 'node:crypto';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import {
  nativeFixtureAuthorityContext,
  nativeFixtureBindingEvidence,
} from './native-fixture-continuity.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const { values } = parseArgs({ options: {
  'task-id': { type: 'string' },
  'reset-root': { type: 'string' },
  'out-dir': { type: 'string' },
  'source-only': { type: 'boolean', default: false },
  'table-scope-file': { type: 'string' },
} });
if (values['task-id'] != null && !/^(?:FORM-02|LIST-0[1-4])$/.test(values['task-id'])) {
  throw new Error('--task-id must be FORM-02 or LIST-01..04');
}
let requestedTableScope = null;
let requestedTableScopeFileDigest = null;
if (values['table-scope-file']) {
  if (!values['source-only']) throw new Error('--table-scope-file requires --source-only');
  const path = values['table-scope-file'];
  const stat = lstatSync(path);
  if (resolve(path) !== path || !stat.isFile() || stat.isSymbolicLink()
    || stat.uid !== process.getuid() || stat.size <= 0 || stat.size > 4096) {
    throw new Error('--table-scope-file must be a bounded absolute owner-owned regular file');
  }
  const contents = readFileSync(path);
  if (contents.length <= 0 || contents.length > 4096) throw new Error('table scope byte budget changed during read');
  const value = JSON.parse(contents.toString('utf8'));
  const exact = (v, keys) => v && !Array.isArray(v) && typeof v === 'object'
    && Object.keys(v).sort().join(',') === [...keys].sort().join(',');
  const label = v => typeof v === 'string' && v.trim() && Buffer.byteLength(v, 'utf8') <= 256
    && !/[\u0000-\u001f\u007f-\u009f]/u.test(v);
  if (!exact(value, ['scroll_path', 'column_headers']) || !Array.isArray(value.scroll_path)
    || value.scroll_path.length < 1 || value.scroll_path.length > 8
    || value.scroll_path.some(part => !exact(part, ['role', 'label'])
      || part.role !== 'ax_scroll_area' || !label(part.label))
    || !Array.isArray(value.column_headers) || value.column_headers.length < 1
    || value.column_headers.length > 64 || value.column_headers.some(header => !label(header))
    || new Set(value.column_headers.map(header => normalizeLabel(header))).size !== value.column_headers.length) {
    throw new Error('table scope must contain only bounded public scroll ancestors and unique column headers');
  }
  requestedTableScope = value;
  requestedTableScopeFileDigest = createHash('sha256').update(contents).digest('hex');
}
for (const key of ['reset-root', 'out-dir']) {
  if (values[key] != null) {
    const path = values[key];
    const stat = lstatSync(path);
    if (resolve(path) !== path || !stat.isDirectory() || stat.isSymbolicLink()
      || stat.uid !== process.getuid()) {
      throw new Error(`--${key} must be an absolute owner-owned directory without a final symlink`);
    }
  }
}
const FIXTURE_APP = join(ROOT, 'target/native-macos/Interaction Fixture.app');
const FIXTURE_BINARY = join(FIXTURE_APP, 'Contents/MacOS/MvpFixture');
const SOURCE = join(ROOT, 'target/debug/cu-macos-source');
const DEVICE_GUARD = join(ROOT, 'target/debug/cu-macos-device-guard');
const CONTEXT_COMPILER = join(ROOT, 'target/debug/compile_device_context');
const CONTINUITY_HELPER = join(ROOT, 'scripts/mvp/native-fixture-continuity.mjs');
const FIXTURE_SOURCE = join(ROOT, 'native/mvp-fixture/MvpFixture.swift');
const RESET_ROOT = values['reset-root'] ?? '/private/tmp/cu-mvp-source-diagnostic-6U5ol4';
const CASES = values['task-id'] ? [values['task-id']] : ['FORM-02', 'LIST-02'];
const SESSION_EPOCH = 7;
const SURFACE = { id: 'main', epoch: SESSION_EPOCH };
const CLOCK_DOMAIN = 'macos/7/wall';
const MAX_SOURCE_BYTES = 2 * 1024 * 1024;
const MAX_DEVICE_STDOUT_BYTES = 1024 * 1024;
const MAX_DEVICE_STDERR_BYTES = 256 * 1024;
const ALLOWED_GUARD_COMMANDS = new Set([
  'configure_authority', 'lease', 'authority_probe', 'stop', 'shutdown',
]);

const artifactId = randomUUID();
const startedAt = new Date().toISOString();
const runDirectory = join(values['out-dir'] ?? join(ROOT, 'artifacts'), `m5-native-context-${artifactId}`);
const artifactPath = join(runDirectory, `native-context-${artifactId}.json`);
const pause = (ms) => new Promise((resolvePause) => setTimeout(resolvePause, ms));
const asBuffer = (value) => Buffer.isBuffer(value) ? value : Buffer.from(value ?? '');

function makePrivateDirectory(path) {
  mkdirSync(path, { recursive: true, mode: 0o700 });
  chmodSync(path, 0o700);
}

function writePrivate(path, contents, { exclusive = true } = {}) {
  makePrivateDirectory(dirname(path));
  writeFileSync(path, contents, { mode: 0o600, flag: exclusive ? 'wx' : 'w' });
  chmodSync(path, 0o600);
}

function secureTree(path) {
  let entries;
  try { entries = readdirSync(path, { withFileTypes: true }); } catch { return; }
  for (const entry of entries) {
    const child = join(path, entry.name);
    let stat;
    try { stat = lstatSync(child); } catch { continue; }
    if (stat.isSymbolicLink()) continue;
    if (stat.isDirectory()) {
      chmodSync(child, 0o700);
      secureTree(child);
    } else if (stat.isFile()) {
      chmodSync(child, 0o600);
    }
  }
}

const hashPin = (path) => {
  try {
    const stat = lstatSync(path);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error('not a regular file');
    const contents = readFileSync(path);
    return { path, size: contents.length, sha256: createHash('sha256').update(contents).digest('hex') };
  } catch (error) {
    return { path, unavailable: String(error?.message ?? error) };
  }
};

const PIN_PATHS = [
  SOURCE,
  DEVICE_GUARD,
  CONTEXT_COMPILER,
  FIXTURE_BINARY,
  join(FIXTURE_APP, 'Contents/Info.plist'),
  FIXTURE_SOURCE,
  CONTINUITY_HELPER,
  fileURLToPath(import.meta.url),
  join(ROOT, 'scripts/smoke-macos-device.mjs'),
  join(ROOT, 'crates/source-macos/src/native.rs'),
  join(ROOT, 'crates/source-macos/src/config.rs'),
  join(ROOT, 'crates/source-macos/src/native/watch/tracker.rs'),
  join(ROOT, 'native/macos/AXTableScopeSearch.swift'),
  join(ROOT, 'native/macos/Accessibility.swift'),
  join(ROOT, 'native/macos/AXSession.swift'),
  join(ROOT, 'native/macos/AXSessionTests.swift'),
  join(ROOT, 'native/macos/Models.swift'),
  join(ROOT, 'native/macos/SourceMain.swift'),
  join(ROOT, 'native/macos/AXTableProjection.swift'),
  join(ROOT, 'native/macos/AXVisiblePrivacy.swift'),
  join(ROOT, 'native/macos/Capture.swift'),
  join(ROOT, 'native/macos/StreamCapture.swift'),
  join(ROOT, 'native/macos/WatchCapture.swift'),
  join(ROOT, 'native/macos/WindowContext.swift'),
  join(ROOT, 'crates/interaction-contracts/src/desktop/table_scope.rs'),
  join(ROOT, 'crates/device-macos/src/guard/authority.rs'),
  join(ROOT, 'crates/device-macos/src/guard.rs'),
  join(ROOT, 'crates/interaction-contracts/src/desktop/binding.rs'),
  join(ROOT, 'crates/interaction-contracts/src/desktop/source.rs'),
  join(ROOT, 'crates/interaction-contracts/src/bin/compile_device_context.rs'),
  ...CASES.map((task) => join(RESET_ROOT, task, 'reset.json')),
  ...(values['table-scope-file'] ? [values['table-scope-file']] : []),
];

function processStamp(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) return '';
  try {
    return execFileSync('/bin/ps', ['-p', String(pid), '-o', 'lstart=', '-o', 'comm='], {
      encoding: 'utf8', timeout: 1000, maxBuffer: 4096,
    }).trim() || null;
  } catch { return null; }
}

function processAlive(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) return false;
  try { process.kill(pid, 0); return true; }
  catch (error) { return error?.code === 'EPERM'; }
}

function processIsSame(pid, stamp, executable) {
  const current = processStamp(pid);
  return Boolean(stamp && current === stamp && current.endsWith(executable));
}

async function ownedProcessCleanup({ pid, stamp, executable, label, signalWaitMs = 2500, gracefulWaitMs = 0 }) {
  const report = { label, pid: pid ?? null, expected_ps_stamp: stamp ?? null,
    executable, termination_requested: false, forced_kill: false, exit_confirmed: false,
    before_ps_stamp: pid ? processStamp(pid) : null };
  if (!pid || !stamp) {
    report.refused_reason = 'missing captured pid or exact ps stamp';
    return report;
  }
  if (!processAlive(pid)) {
    report.exit_confirmed = true;
    return report;
  }
  if (!processIsSame(pid, stamp, executable)) {
    report.refused_reason = 'current pid stamp does not exactly match captured owned process';
    return report;
  }
  const gracefulDeadline = Date.now() + gracefulWaitMs;
  while (gracefulWaitMs > 0 && Date.now() < gracefulDeadline
    && processAlive(pid) && processIsSame(pid, stamp, executable)) await pause(50);
  if (!processAlive(pid)) {
    report.exit_confirmed = true;
    report.after_ps_stamp = processStamp(pid);
    return report;
  }
  if (!processIsSame(pid, stamp, executable)) {
    report.refused_reason = 'process identity changed during graceful shutdown window';
    report.after_ps_stamp = processStamp(pid);
    return report;
  }
  try {
    process.kill(pid, 'SIGTERM');
    report.termination_requested = true;
  } catch (error) {
    report.term_error = String(error?.message ?? error);
  }
  const deadline = Date.now() + signalWaitMs;
  // After signalling a verified process, wait for reaping as well as exit.
  // A zombie's comm can change; no further signal is sent without rechecking identity.
  while (Date.now() < deadline && processAlive(pid)) await pause(100);
  if (!processAlive(pid)) {
    report.exit_confirmed = true;
    report.after_ps_stamp = processStamp(pid);
    return report;
  }
  if (processIsSame(pid, stamp, executable)) {
    try {
      process.kill(pid, 'SIGKILL');
      report.forced_kill = true;
    } catch (error) {
      report.kill_error = String(error?.message ?? error);
    }
    const killDeadline = Date.now() + 1500;
    while (Date.now() < killDeadline && processAlive(pid)) await pause(100);
  }
  report.after_ps_stamp = processStamp(pid);
  report.exit_confirmed = !processAlive(pid);
  if (!report.exit_confirmed && report.after_ps_stamp !== stamp) {
    report.refused_followup_signal = 'pid identity changed after initial termination';
  }
  return report;
}

async function waitFor(test, timeoutMs, intervalMs = 50) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = test();
    if (value) return value;
    await pause(intervalMs);
  }
  return test() || null;
}

function parseReady(path) {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > 1024) {
    throw new Error('fixture ready record is absent or outside its 1024-byte bound');
  }
  const value = JSON.parse(readFileSync(path, 'utf8'));
  if (!value || Array.isArray(value) || typeof value !== 'object'
    || Object.keys(value).sort().join(',') !== 'pid,window_number'
    || !Number.isSafeInteger(value.pid) || value.pid <= 0
    || !Number.isSafeInteger(value.window_number) || value.window_number <= 0) {
    throw new Error('fixture ready record must contain only positive pid and window_number');
  }
  return value;
}

function launchFixture(task, caseDirectory) {
  const resetSource = join(RESET_ROOT, task, 'reset.json');
  const reset = join(caseDirectory, 'reset.json');
  const snapshot = join(caseDirectory, 'fixture-snapshot.json');
  const readyFile = join(caseDirectory, 'fixture-ready.stdout');
  const stderrFile = join(caseDirectory, 'fixture-stderr.log');
  if (!existsSync(resetSource)) throw new Error(`fixture reset profile unavailable: ${resetSource}`);
  copyFileSync(resetSource, reset);
  chmodSync(reset, 0o600);
  writePrivate(snapshot, '');
  writePrivate(readyFile, '');
  writePrivate(stderrFile, '');
  const launch = spawn('/usr/bin/open', ['-n', '-W', '-a', FIXTURE_APP,
    '--stdout', readyFile, '--stderr', stderrFile, '--args',
    '--profile', reset, '--snapshot', snapshot], { stdio: 'ignore' });
  const launchStamp = launch.pid ? processStamp(launch.pid) : '';
  return { launch, launchStamp, reset, snapshot, readyFile, stderrFile };
}

function normalizeLabel(text) {
  return String(text).split(/[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u)
    .filter(Boolean).join(' ').toLowerCase();
}

function roundAwayFromZero(value) {
  return Math.sign(value) * Math.floor(Math.abs(value) + 0.5);
}

function objectAncestors(structure, object) {
  const byRef = new Map(structure.objects.map((candidate) => [candidate.instance_ref, candidate]));
  const ancestors = [];
  const seen = new Set([object.instance_ref]);
  let parentRef = object.parent;
  while (parentRef != null) {
    if (seen.has(parentRef)) throw new Error('target ancestry contains a cycle');
    seen.add(parentRef);
    const parent = byRef.get(parentRef);
    if (!parent) throw new Error(`target ancestry is incomplete at ${String(parentRef)}`);
    ancestors.push(parent);
    parentRef = parent.parent;
    if (ancestors.length > 32) throw new Error('target ancestry exceeds 32 objects');
  }
  return ancestors.reverse();
}

const NON_ORDINARY_ANCESTOR_ROLES = new Set([
  'AXMenu', 'AXMenuBar', 'AXMenuItem', 'AXTable', 'AXRow', 'AXOutline', 'AXList',
  'AXDialog', 'AXPopover',
]);

function selectionCandidateReport(structure) {
  if (structure?.complete !== true || !Array.isArray(structure.objects)) {
    return { object: null, reason: 'source_structure_incomplete_or_missing', examined: [] };
  }
  if (typeof structure.active_layer !== 'string' || !structure.active_layer) {
    return { object: null, reason: 'source_has_no_active_layer', examined: [] };
  }
  const activeWindow = structure.windows?.find((window) => window.window_id === structure.active_layer);
  const ordinaryWindow = activeWindow?.kind === 'window' && activeWindow.modal === false;
  const ownedSheet = activeWindow?.kind === 'sheet' && activeWindow.modal === true;
  if (!activeWindow || (!ordinaryWindow && !ownedSheet)) {
    return { object: null, reason: 'active_layer_is_not_an_ordinary_window_or_modal_sheet', examined: [] };
  }
  if (structure.menu_context) {
    return { object: null, reason: 'a menu context is active in this Source read', examined: [] };
  }
  const focusTargets = new Set([
    structure.focus_context?.target_instance,
    structure.focus_context?.focused_instance,
  ].filter((value) => typeof value === 'string'));
  const menuOpeners = new Set((structure.menu_anchors ?? [])
    .map((anchor) => anchor?.opener_instance).filter((value) => typeof value === 'string'));
  const examined = [];
  const objects = [...structure.objects].sort((left, right) =>
    String(left.instance_ref) < String(right.instance_ref) ? -1
      : String(left.instance_ref) > String(right.instance_ref) ? 1 : 0);
  for (const object of objects) {
    if (object.role !== 'AXButton' || object.window_id !== structure.active_layer) continue;
    const reasons = [];
    if (object.enabled !== true) reasons.push('not_enabled');
    if (object.in_view !== true) reasons.push('not_in_view');
    if (!Array.isArray(object.actions) || !object.actions.includes('press')) reasons.push('Press_not_declared');
    if (object.focused === true || focusTargets.has(object.instance_ref)) reasons.push('is_focus_target');
    if (menuOpeners.has(object.instance_ref)) reasons.push('is_menu_opener');
    if (object.secure === true) reasons.push('secure_object');
    let ancestors = [];
    try { ancestors = objectAncestors(structure, object); }
    catch (error) { reasons.push(String(error?.message ?? error)); }
    if (ancestors.some((ancestor) => NON_ORDINARY_ANCESTOR_ROLES.has(ancestor.role))) {
      reasons.push('menu_table_or_special_layer_ancestry');
    }
    const root = ancestors[0];
    const expectedRoot = ordinaryWindow
      ? root?.role === 'AXWindow' && root.subrole === 'AXStandardWindow'
      : root?.role === 'AXSheet';
    if (!root || !expectedRoot || root.window_id !== structure.active_layer) {
      reasons.push('target_is_not_under_the_actual_active_window_or_sheet_root');
    }
    const siblings = structure.objects.filter((candidate) =>
      candidate.parent === object.parent && candidate.window_id === object.window_id);
    if (siblings.length < 2) reasons.push('no_direct_sibling_for_required_label_change');
    if (!object.frame || ![object.frame.x, object.frame.y, object.frame.width, object.frame.height]
      .every(Number.isFinite) || object.frame.width <= 0 || object.frame.height <= 0) {
      reasons.push('invalid_target_frame');
    }
    examined.push({ instance_ref: object.instance_ref, reasons });
    if (reasons.length === 0) return { object, reason: null, examined };
  }
  return { object: null, reason: 'no_suitable_active_layer_ordinary_AXButton_or_modal_sheet_button', examined };
}

function instanceFingerprint(structure, object) {
  const ancestors = objectAncestors(structure, object);
  const siblingIndex = structure.objects.filter((candidate) =>
    candidate.parent === object.parent && candidate.window_id === object.window_id)
    .findIndex((candidate) => candidate.instance_ref === object.instance_ref);
  if (siblingIndex < 0 || siblingIndex > 255) throw new Error('target sibling index is outside native fingerprint bounds');
  const label = object.title ?? object.description ?? '';
  return {
    app: structure.app.bundle_id,
    app_pid: structure.app.pid,
    window_id: object.window_id,
    ancestor_roles: ancestors.map((ancestor) => ancestor.role),
    group: object.group == null ? null : normalizeLabel(object.group),
    role: object.role,
    subrole: object.subrole ?? null,
    label: normalizeLabel(label),
    sibling_index: siblingIndex,
    frame: [object.frame.x, object.frame.y, object.frame.width, object.frame.height].map(roundAwayFromZero),
    scroll_container: null,
  };
}

function fingerprintDigest(fingerprint) {
  return `fp1:${createHash('sha256').update(JSON.stringify(fingerprint)).digest('hex')}`;
}

function createSourceProof(structure, object, sourceSequence, evidence, diagnosticIds) {
  const proof = {
    binding: `diagnostic-binding-${diagnosticIds.binding}`,
    instance: object.instance_ref,
    snapshot_id: typeof structure.snapshot_id === 'string' && structure.snapshot_id
      ? structure.snapshot_id : `diagnostic-source-${sourceSequence}`,
    environment_revision: 1,
    layer_revision: 1,
    fingerprint: instanceFingerprint(structure, object),
    display_frame: structuredClone(object.frame),
    continuity: evidence.continuity,
    device_context: evidence.device_context,
    menu_scope: null,
    focus_scope: null,
  };
  if (proof.continuity.kind !== 'read_scoped' || proof.continuity.source_sequence !== sourceSequence) {
    throw new Error('Rust closure compiler returned continuity for a different diagnostic read');
  }
  return proof;
}

function startGuard(caseDirectory) {
  const proc = spawn(DEVICE_GUARD, ['--epoch', String(SESSION_EPOCH), '--marker', '123'], {
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const stdoutChunks = [];
  const stderrChunks = [];
  const records = [];
  let stdoutBytes = 0;
  let stderrBytes = 0;
  let stdoutOverflow = false;
  let stderrOverflow = false;
  let pending = '';
  let parseErrors = [];
  let stdinErrors = [];
  const rawLines = [];
  proc.stdout.on('data', (chunk) => {
    const bytes = asBuffer(chunk);
    stdoutBytes += bytes.length;
    if (stdoutBytes <= MAX_DEVICE_STDOUT_BYTES) stdoutChunks.push(bytes);
    else stdoutOverflow = true;
    pending += bytes.toString('utf8');
    if (pending.length > 64 * 1024) {
      parseErrors.push('native guard emitted an unterminated record over 64 KiB');
      pending = pending.slice(-64 * 1024);
    }
    for (let newline; (newline = pending.indexOf('\n')) >= 0;) {
      const line = pending.slice(0, newline);
      pending = pending.slice(newline + 1);
      if (!line) continue;
      rawLines.push(line);
      try { records.push(JSON.parse(line)); }
      catch (error) { parseErrors.push(String(error?.message ?? error)); }
    }
  });
  proc.stderr.on('data', (chunk) => {
    const bytes = asBuffer(chunk);
    stderrBytes += bytes.length;
    if (stderrBytes <= MAX_DEVICE_STDERR_BYTES) stderrChunks.push(bytes);
    else stderrOverflow = true;
  });
  proc.stdin.on('error', (error) => stdinErrors.push(String(error?.message ?? error)));
  const stamp = proc.pid ? processStamp(proc.pid) : '';
  return {
    proc,
    stamp,
    records,
    rawLines,
    diagnostics: () => ({ stdout: Buffer.concat(stdoutChunks), stderr: Buffer.concat(stderrChunks),
      stdout_bytes: stdoutBytes, stderr_bytes: stderrBytes, stdout_overflow: stdoutOverflow,
      stderr_overflow: stderrOverflow, parse_errors: parseErrors, stdin_errors: stdinErrors,
      trailing_stdout: pending }),
    writeDiagnostics() {
      const captured = this.diagnostics();
      writePrivate(join(caseDirectory, 'device-stdout.ndjson'), captured.stdout, { exclusive: false });
      writePrivate(join(caseDirectory, 'device-stderr.log'), captured.stderr, { exclusive: false });
      writePrivate(join(caseDirectory, 'device-stdin.ndjson'), this.outbound ?? '', { exclusive: false });
    },
    outbound: '',
  };
}

function sendGuardCommand(guard, command) {
  if (!ALLOWED_GUARD_COMMANDS.has(command.command)) {
    throw new Error(`probe script attempted disallowed native guard command: ${String(command.command)}`);
  }
  const envelope = { protocol_version: 1, session_epoch: SESSION_EPOCH, ...command };
  const line = `${JSON.stringify(envelope)}\n`;
  guard.outbound += line;
  if (guard.proc.exitCode != null || guard.proc.signalCode != null || !guard.proc.stdin.writable) {
    throw new Error(`native guard is unavailable before ${command.command}`);
  }
  guard.proc.stdin.write(line);
  return envelope;
}

async function runAuthorityProbe(guard, caseDirectory, label, proof, sourceSequence, object, clockOrigin) {
  const requestId = `request-${randomUUID()}`;
  const intentId = `intent-${randomUUID()}`;
  const evidenceId = `observation-${randomUUID()}`;
  const intentEvidenceId = `intent-evidence-${randomUUID()}`;
  const permitId = `permit-${randomUUID()}`;
  const now = Date.now() - clockOrigin;
  const guardRecord = [...guard.records].reverse().find((record) => record.kind === 'guard');
  if (!guardRecord) throw new Error('native guard has no authority snapshot for an exact guard_revision');
  const witnessContext = nativeFixtureAuthorityContext(proof);
  if ('device_context' in witnessContext.original_proof || 'device_context' in witnessContext.current_proof) {
    throw new Error('NativeSourceProof duplicated the top-level context_witness');
  }
  const fingerprint = proof.fingerprint;
  const frame = proof.display_frame;
  const request = {
    request_id: requestId,
    intent_id: intentId,
    task_id: `native-context-${label}`,
    task_revision: 1,
    root_epoch: 1,
    permit: { id: permitId, generation: 1 },
    stop_seq: 5,
    guard_revision: guardRecord.stop_revision,
    clock_domain: CLOCK_DOMAIN,
    surface: SURFACE,
    evidence_id: evidenceId,
    intent_evidence_id: intentEvidenceId,
    source_sequence: sourceSequence,
    observed_ms: now,
    valid_until_ms: now + 1000,
    command_end_ms: now + 1000,
    started_ms: now,
    duration_ms: 1000,
    latest_start_ms: now,
    ...witnessContext,
    binding_digest: fingerprintDigest(fingerprint),
    dependencies: { focus: 1, layer: 1, binding: 1, authorization: 1, geometry: 1 },
    action: 'ax_press',
    x: frame.x + frame.width / 2,
    y: frame.y + frame.height / 2,
  };
  const envelope = { protocol_version: 1, session_epoch: SESSION_EPOCH,
    command: 'authority_probe', probe: request };
  const requestPath = join(caseDirectory, `request-${label}.json`);
  const replyPath = join(caseDirectory, `reply-${label}.json`);
  writePrivate(requestPath, `${JSON.stringify(envelope)}\n`);
  const result = { label, request_id: requestId, intent_id: intentId, request_path: requestPath,
    reply_path: replyPath, outcome: null, input_authorized: null, sent: false, error: null };
  try {
    sendGuardCommand(guard, { command: 'authority_probe', probe: request });
    result.sent = true;
    const response = await waitFor(() => [...guard.records].reverse().find((record) =>
      record.kind === 'authority_probe' && record.request_id === requestId), 5000, 20);
    if (!response) throw new Error('no correlated authority_probe reply before timeout');
    const rawResponse = [...guard.rawLines].reverse().find((line) => {
      try {
        const candidate = JSON.parse(line);
        return candidate.kind === 'authority_probe' && candidate.request_id === requestId;
      } catch { return false; }
    });
    if (rawResponse == null) throw new Error('correlated authority_probe response has no preserved raw wire line');
    writePrivate(replyPath, `${rawResponse}\n`);
    result.outcome = response.outcome ?? null;
    result.input_authorized = response.input_authorized ?? null;
    result.reply = response;
  } catch (error) {
    result.error = String(error?.stack ?? error?.message ?? error);
    writePrivate(join(caseDirectory, `failure-${label}.txt`), result.error);
  }
  return result;
}

function oneSourceRead(task, ready, caseDirectory, onReadStarted) {
  const captureDirectory = join(caseDirectory, 'source-capture');
  makePrivateDirectory(captureDirectory);
  const request = {
    protocol_version: 1,
    command: 'observe',
    bundle_id: 'com.lingxi.computer-user.mvp-fixture',
    pid: ready.pid,
    output_dir: captureDirectory,
    timeout_ms: 3000,
    text_policy: { kind: 'registered_editor', window_id: `window-${ready.window_number}` },
    ...(requestedTableScope ? { table_scope: requestedTableScope } : {}),
  };
  const requestBytes = Buffer.from(`${JSON.stringify(request)}\n`);
  writePrivate(join(caseDirectory, 'source-request.json'), requestBytes);
  onReadStarted();
  const result = spawnSync(SOURCE, [], { input: requestBytes, timeout: 6000,
    maxBuffer: MAX_SOURCE_BYTES, encoding: null });
  const stdout = asBuffer(result.stdout);
  const stderr = asBuffer(result.stderr);
  writePrivate(join(caseDirectory, 'source-stdout.json'), stdout);
  writePrivate(join(caseDirectory, 'source-stderr.log'), stderr);
  secureTree(captureDirectory);
  if (result.error) throw new Error(`one-shot Source observe failed: ${result.error.message}`);
  if (result.status !== 0) {
    throw new Error(`one-shot Source observe exited ${String(result.status)} ${String(result.signal ?? '')}: ${stderr.toString('utf8')}`);
  }
  let response;
  try { response = JSON.parse(stdout.toString('utf8')); }
  catch (error) { throw new Error(`one-shot Source stdout is not valid JSON: ${String(error?.message ?? error)}`); }
  if (!response?.structure || typeof response.structure !== 'object') {
    throw new Error('one-shot Source response did not contain structure');
  }
  return { response, structure: response.structure, request, raw_stdout_bytes: stdout.length,
    raw_stderr_bytes: stderr.length, task };
}

function siblingLabelMutation(witness) {
  if (witness?.scope !== 'whole_structure_v1' || !Array.isArray(witness.siblings)
    || !Number.isSafeInteger(witness.target_sibling_index)) {
    throw new Error('compiled whole-structure witness has no ordered siblings');
  }
  const siblingIndex = witness.siblings.findIndex((_, index) => index !== witness.target_sibling_index);
  if (siblingIndex < 0) throw new Error('compiled target has no non-target direct sibling');
  const originalLabel = witness.siblings[siblingIndex].label;
  const newLabel = originalLabel === 'x' ? 'y' : 'x';
  if (newLabel === originalLabel) throw new Error('could not form a distinct bounded sibling label');
  const changed = structuredClone(witness);
  changed.siblings[siblingIndex].label = newLabel;
  if (Buffer.byteLength(newLabel, 'utf8') > 512 || Buffer.byteLength(JSON.stringify(changed), 'utf8') > 4096) {
    throw new Error('sibling label mutation would exceed the native witness wire budget');
  }
  return { witness: changed, sibling_index: siblingIndex, old_label: originalLabel, new_label: newLabel };
}

async function runCase(task) {
  const caseDirectory = join(runDirectory, task);
  makePrivateDirectory(caseDirectory);
  const record = {
    task,
    artifact_id: artifactId,
    one_fresh_source_read: true,
    source_read_count: 0,
    physical_inputs: 0,
    provider_calls: 0,
    input_authorized: false,
    source_to_device_identity_acceptance: false,
    business_acceptance: false,
    mvp_acceptance: false,
    diagnostics: [],
    failures: [],
    probes: [],
  };
  let fixture;
  let fixtureStamp = '';
  let ready;
  let launcher;
  let guard;
  let leaseTimer;
  let clockOrigin;
  try {
    fixture = launchFixture(task, caseDirectory);
    launcher = fixture.launch;
    fixture.launchStamp = await waitFor(() => fixture.launch.pid
      ? processStamp(fixture.launch.pid) : null, 1000, 20) ?? '';
    record.fixture_ready_path = fixture.readyFile;
    const readyResult = await waitFor(() => {
      if (fixture.launch.exitCode != null || fixture.launch.signalCode != null) {
        return { launch_exit: true };
      }
      try { return parseReady(fixture.readyFile); } catch { return null; }
    }, 15_000, 100);
    if (!readyResult || readyResult.launch_exit) throw new Error('LaunchServices fixture readiness failed');
    ready = readyResult;
    fixtureStamp = processStamp(ready.pid) ?? '';
    record.fixture_process = { pid: ready.pid, window_number: ready.window_number,
      ps_stamp: fixtureStamp, executable_matches: fixtureStamp.endsWith(FIXTURE_BINARY) };
    if (!record.fixture_process.executable_matches) {
      throw new Error(`ready pid did not match exact fixture executable stamp: ${fixtureStamp}`);
    }
    await pause(400);

    const observed = oneSourceRead(task, ready, caseDirectory,
      () => { record.source_read_count += 1; });
    record.source_snapshot_id = observed.structure.snapshot_id ?? null;
    record.source_source_sequence = observed.structure.source_sequence ?? null;
    record.source_complete = observed.structure.complete === true;
    record.source_observer_coverage = observed.structure.observer_coverage ?? null;
    record.fixture_snapshot_not_used_for_target_selection = true;

    if (values['source-only']) {
      record.status = 'source_observed';
      record.source_incomplete_reason = observed.structure.incomplete_reason ?? null;
      record.source_object_count = observed.structure.objects?.length ?? null;
      record.source_table_viewports = observed.structure.table_viewports ?? null;
      record.requested_table_scope = requestedTableScope;
      record.source_table_scope_search = observed.structure.table_scope_search ?? null;
      record.source_frame_available = observed.response.frame != null;
      record.source_ocr_complete = observed.response.ocr?.complete ?? false;
      record.source_capture_errors = observed.response.errors ?? [];
      return record;
    }

    const selected = selectionCandidateReport(observed.structure);
    record.selection = { reason: selected.reason, examined: selected.examined };
    if (!selected.object) {
      record.status = 'no_suitable_object';
      record.failures.push({ phase: 'target_selection', message: selected.reason });
      return record;
    }
    const object = selected.object;
    record.selection.target = { instance_ref: object.instance_ref, role: object.role,
      window_id: object.window_id, title: object.title ?? null, description: object.description ?? null,
      active_layer_kind: observed.structure.windows.find((window) => window.window_id === object.window_id)?.kind ?? null,
      frame: object.frame, enabled: object.enabled, in_view: object.in_view,
      press_declared: object.actions.includes('press'), focused: object.focused };

    const hasPublicSourceSequence = Number.isSafeInteger(observed.structure.source_sequence)
      && observed.structure.source_sequence > 0;
    const sourceSequence = hasPublicSourceSequence ? observed.structure.source_sequence : 1;
    record.diagnostic_source_sequence = sourceSequence;
    record.source_sequence_origin = hasPublicSourceSequence
      ? 'one-shot StructureSnapshot.source_sequence'
      : 'single-read diagnostic ordinal 1; one-shot Source has no public source_sequence; not a Host/Source sequence claim';
    record.public_source_sequence_available = hasPublicSourceSequence;
    const compilerEvidence = nativeFixtureBindingEvidence(observed.structure, object, sourceSequence);
    writePrivate(join(caseDirectory, 'closure-compiler-evidence.json'),
      `${JSON.stringify(compilerEvidence, null, 2)}\n`);
    record.compiler_scope = compilerEvidence.device_context.scope;
    record.continuity = compilerEvidence.continuity;
    if (record.compiler_scope !== 'whole_structure_v1') {
      throw new Error(`shared Rust compiler returned unsupported witness ${String(record.compiler_scope)}`);
    }
    if (compilerEvidence.device_context.focus != null) {
      throw new Error('ordinary active-layer AXButton probe must not carry Source focus authority');
    }
    record.device_context_focus = null;
    const ids = { binding: randomUUID() };
    const proof = createSourceProof(observed.structure, object, sourceSequence, compilerEvidence, ids);
    const positiveContext = nativeFixtureAuthorityContext(proof);
    if ('device_context' in positiveContext.original_proof || 'device_context' in positiveContext.current_proof) {
      throw new Error('compiled context witness must appear exactly once at probe top level');
    }
    record.proof_summary = { binding: proof.binding, instance: proof.instance, snapshot_id: proof.snapshot_id,
      snapshot_id_origin: observed.structure.snapshot_id === proof.snapshot_id
        ? 'native public snapshot_id' : 'shared compiler diagnostic header; no Host snapshot claim',
      environment_and_layer_revisions_are_diagnostic: true,
      fingerprint: proof.fingerprint, display_frame: proof.display_frame, continuity: proof.continuity,
      original_current_proof_equal: JSON.stringify(positiveContext.original_proof)
        === JSON.stringify(positiveContext.current_proof),
      proof_contains_context_witness: false };

    const negative = siblingLabelMutation(positiveContext.context_witness);
    record.negative_mutation = { kind: 'one_non_target_direct_sibling_label',
      sibling_index: negative.sibling_index, old_label: negative.old_label, new_label: negative.new_label,
      source_object_identity_changed: false, proof_identity_changed: false };
    const negativeProof = { ...proof, device_context: negative.witness };

    guard = startGuard(caseDirectory);
    guard.stamp = await waitFor(() => guard.proc.pid ? processStamp(guard.proc.pid) : null,
      1000, 20) ?? '';
    if (!guard.stamp.endsWith(DEVICE_GUARD)) {
      throw new Error(`native guard child ps stamp did not match helper binary: ${guard.stamp}`);
    }
    clockOrigin = Date.now();
    sendGuardCommand(guard, { command: 'configure_authority', clock_origin_ms: clockOrigin,
      surface: SURFACE, clock_domain: CLOCK_DOMAIN });
    const renew = () => {
      if (guard.proc.exitCode == null && guard.proc.signalCode == null) {
        for (const lane of ['host', 'supervisor']) {
          try { sendGuardCommand(guard, { command: 'lease', lane, duration_ms: 500 }); }
          catch (error) { record.diagnostics.push(`lease ${lane}: ${String(error?.message ?? error)}`); }
        }
      }
    };
    leaseTimer = setInterval(renew, 100);
    renew();
    const activeGuard = await waitFor(() => [...guard.records].reverse().find((item) =>
      item.kind === 'guard' && item.authority_clock_ready === true
        && item.host_alive && item.supervisor_alive), 4000, 20);
    record.guard_ready = activeGuard ? { accessibility: activeGuard.accessibility,
      human_input: activeGuard.human_input, input_available: activeGuard.input_available,
      authority_clock_ready: activeGuard.authority_clock_ready,
      host_alive: activeGuard.host_alive, supervisor_alive: activeGuard.supervisor_alive,
      stop_revision: activeGuard.stop_revision } : null;
    if (!activeGuard) record.failures.push({ phase: 'native_guard_setup',
      message: 'no guard snapshot showed authority_clock_ready=true and both configured leases alive' });

    if (activeGuard) {
      record.probes.push(await runAuthorityProbe(guard, caseDirectory, 'positive', proof,
        sourceSequence, object, clockOrigin));
      record.probes.push(await runAuthorityProbe(guard, caseDirectory, 'sibling-label-negative', negativeProof,
        sourceSequence, object, clockOrigin));
      for (const probe of record.probes) {
        if (probe.error) record.failures.push({ phase: `authority_probe:${probe.label}`, message: probe.error });
        if (probe.input_authorized === true) {
          record.failures.push({ phase: `authority_probe:${probe.label}`,
            message: 'read-only authority_probe unexpectedly reported input_authorized=true' });
        }
      }
    }
    record.status = record.failures.length === 0 ? 'requests_sent' : 'requests_sent_with_failures';
  } catch (error) {
    record.status = 'failed';
    record.failures.push({ phase: 'case', message: String(error?.stack ?? error?.message ?? error) });
    try { writePrivate(join(caseDirectory, 'failure-case.txt'), record.failures.at(-1).message); } catch { /* keep the original failure */ }
  } finally {
    if (leaseTimer) clearInterval(leaseTimer);
    if (guard) {
      try {
        if (guard.proc.exitCode == null && guard.proc.signalCode == null && guard.proc.stdin.writable) {
          sendGuardCommand(guard, { command: 'stop' });
        }
      } catch (error) {
        record.diagnostics.push(`guard stop: ${String(error?.message ?? error)}`);
      }
      try { guard.proc.stdin.end(); } catch { /* process close is verified below */ }
      if (guard.proc.pid) {
        const cleanup = await ownedProcessCleanup({ pid: guard.proc.pid, stamp: guard.stamp,
          executable: DEVICE_GUARD, label: 'native-device-guard', signalWaitMs: 1500, gracefulWaitMs: 750 });
        record.guard_cleanup = cleanup;
      }
      try { guard.writeDiagnostics(); } catch (error) {
        record.diagnostics.push(`guard output preservation: ${String(error?.message ?? error)}`);
      }
    }
    if (ready && fixtureStamp) {
      record.fixture_cleanup = await ownedProcessCleanup({ pid: ready.pid, stamp: fixtureStamp,
        executable: FIXTURE_BINARY, label: 'LaunchServices Interaction Fixture.app' });
    } else if (ready) {
      record.fixture_cleanup = { label: 'LaunchServices Interaction Fixture.app', pid: ready.pid,
        expected_ps_stamp: fixtureStamp || null, termination_requested: false,
        exit_confirmed: !processAlive(ready.pid),
        refused_reason: 'fixture pid had no captured exact ps stamp; termination was refused' };
    } else {
      record.fixture_cleanup = { label: 'LaunchServices Interaction Fixture.app',
        termination_requested: false, exit_confirmed: false,
        refused_reason: 'fixture process never had a verified ready pid and exact ps stamp' };
    }
    if (launcher?.pid) {
      const currentStamp = processStamp(launcher.pid);
      record.launcher_cleanup = currentStamp && currentStamp === fixture?.launchStamp
        ? await ownedProcessCleanup({ pid: launcher.pid, stamp: fixture.launchStamp,
          executable: '/usr/bin/open', label: 'owned LaunchServices open launcher', signalWaitMs: 500 })
        : { pid: launcher.pid, expected_ps_stamp: fixture?.launchStamp ?? '',
          current_ps_stamp: currentStamp, termination_requested: false,
          exit_confirmed: !processAlive(launcher.pid),
          refused_reason: processAlive(launcher.pid) ? 'launcher ps stamp could not be revalidated exactly' : null };
    }
    try {
      record.fixture_ready_stdout = readFileSync(fixture?.readyFile ?? '', 'utf8');
    } catch { /* readiness error is already captured in record.failures */ }
    try {
      record.fixture_stderr = readFileSync(fixture?.stderrFile ?? '', 'utf8');
    } catch { /* launch may not have created stderr */ }
    secureTree(caseDirectory);
  }
  return record;
}

async function main() {
  makePrivateDirectory(runDirectory);
  const artifact = {
    kind: 'm5_native_context_witness_probe',
    version: 1,
    artifact_id: artifactId,
    started_at: startedAt,
    finished_at: null,
    status: 'running',
    semantic_relation_diagnostic_only: true,
    source_probe_only: values['source-only'],
    input_authorized: false,
    physical_inputs: 0,
    provider_calls: 0,
    source_to_device_identity_acceptance: false,
    business_acceptance: false,
    mvp_acceptance: false,
    allowed_guard_commands: values['source-only'] ? [] : [...ALLOWED_GUARD_COMMANDS],
    forbidden_guard_commands: ['atomic_submit', 'hit_test', 'revoke_authority'],
    fixture_ax_tree_used_for_target_selection: false,
    source_read_policy: 'exactly one fresh one-shot Source observe per fixture case; no retry',
    witness_policy: values['source-only'] ? 'none; no witness compiler or Native Guard request'
      : 'shared Rust closure compiler; one top-level context_witness only',
    before_pins: PIN_PATHS.map(hashPin),
    after_pins: [],
    pins_stable: false,
    artifact_path: artifactPath,
    private_run_directory: runDirectory,
    cases: [],
    failures: [],
  };
  try {
    if (requestedTableScopeFileDigest && artifact.before_pins.find(pin =>
      pin.path === values['table-scope-file'])?.sha256 !== requestedTableScopeFileDigest) {
      throw new Error('table scope file changed after its request was parsed');
    }
    for (const task of CASES) artifact.cases.push(await runCase(task));
  } catch (error) {
    artifact.failures.push({ phase: 'driver', message: String(error?.stack ?? error?.message ?? error) });
  } finally {
    artifact.after_pins = PIN_PATHS.map(hashPin);
    artifact.pins_stable = artifact.before_pins.every((pin) => typeof pin.sha256 === 'string')
      && JSON.stringify(artifact.before_pins) === JSON.stringify(artifact.after_pins);
    artifact.finished_at = new Date().toISOString();
    const allCasesProcessed = artifact.cases.length === CASES.length
      && artifact.cases.every((item) => values['source-only'] ? item.status === 'source_observed'
        : ['requests_sent', 'requests_sent_with_failures', 'no_suitable_object', 'failed'].includes(item.status));
    const hasCaseFailures = artifact.cases.some((item) => item.failures.length > 0);
    artifact.status = artifact.failures.length > 0 || !allCasesProcessed
      || artifact.cases.some((item) => item.status === 'failed' || item.status === 'requests_sent_with_failures')
      || (values['source-only'] && (!artifact.pins_stable || artifact.cases.some((item) =>
        item.fixture_cleanup?.exit_confirmed !== true || item.launcher_cleanup?.exit_confirmed !== true)))
      ? 'failed' : hasCaseFailures ? 'completed_with_case_outcomes' : 'completed';
    try {
      secureTree(runDirectory);
      writePrivate(artifactPath, `${JSON.stringify(artifact, null, 2)}\n`);
    } catch (error) {
      artifact.status = 'artifact_write_failed';
      console.error(`Could not preserve native context probe artifact: ${String(error?.message ?? error)}`);
    }
    console.log(JSON.stringify({ artifact_id: artifactId, artifact_path: artifactPath,
      status: artifact.status, cases: artifact.cases.map((item) => ({ task: item.task, status: item.status,
        source_read_count: item.source_read_count, probes: item.probes.map((probe) => ({
          label: probe.label, outcome: probe.outcome, input_authorized: probe.input_authorized,
        })) })), physical_inputs: 0, provider_calls: 0,
      source_to_device_identity_acceptance: false, business_acceptance: false, mvp_acceptance: false }));
    if (values['source-only'] && artifact.status !== 'completed') process.exitCode = 1;
  }
}

await main();
