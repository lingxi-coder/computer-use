// Explicit owned MVP row diagnostic. It is launched only by the CLI wrapper.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { createHash, randomUUID } = require('node:crypto');
const { isDeepStrictEqual } = require('node:util');
const { spawn, execFileSync, execFile } = require('node:child_process');
const { app, dialog, globalShortcut } = require('electron');

const root = process.env.CU_MVP_ROW_ROOT;
const scratch = process.env.CU_MVP_ROW_SCRATCH;
const outputPath = process.env.CU_MVP_ROW_OUTPUT;
const profilePath = process.env.CU_MVP_ROW_PROFILE;
const snapshotPath = process.env.CU_MVP_ROW_SNAPSHOT;
const targetPath = process.env.CU_MVP_ROW_TARGET;
const taskId = process.env.CU_MVP_ROW_TASK_ID;
const seed = Number(process.env.CU_MVP_ROW_SEED);
const bundleId = 'com.lingxi.computer-user.mvp-fixture';
const fixtureBinary = process.env.CU_MVP_ROW_FIXTURE_BINARY;
const guardPath = process.env.CU_MVP_ROW_GUARD;
const diagnosticsDirectory = process.env.CU_MVP_ROW_DIAGNOSTICS;
const activationOnly = process.env.CU_MVP_ROW_ACTIVATION_ONLY === '1';
const consentDriver = process.env.CU_MVP_ROW_CONSENT_DRIVER;
if (!root || !scratch || !outputPath || !profilePath || !snapshotPath || !targetPath
    || process.env.CU_MVP_ROW_EXECUTE !== '1' || !/^LIST-0[1-4]$/.test(taskId || '')
    || !Number.isInteger(seed) || seed < 1 || seed > 5
    || path.basename(guardPath || '') !== path.basename(process.env.CU_NATIVE_DEVICE_GUARD || '')
    || !/^owned-row-[A-Za-z0-9][A-Za-z0-9._-]*$/.test(path.basename(guardPath || ''))) {
  throw new Error('Run only through the explicit smoke-macos-mvp-row CLI with --execute, owned-row guard, and an independent TargetSpec.');
}

app.setName('Computer Use MVP Row Diagnostic');
app.setPath('userData', path.join(scratch, 'user-data'));
const proof = {
  kind: activationOnly ? 'scoped_window_activation_operator_diagnostic' : 'mvp_row_operator_diagnostic', ok: false, live_model_calls: 0,
  activation_only: activationOnly,
  business_gate_eligible: false, mvp_complete: false, explicit_execute: true,
  operator_text_sent: false, window_permission_cancel_path_verified: false,
  native_row_setter_call_count: { status: 'unknown', count: null },
  navigation_native_input_count: { status: 'unknown', count: null },
  stop_before_row_setter: { status: 'not_exercised', setter_count: 'unknown' },
  console_errors: [],
};
let fixture;
let fixtureClosed = false;
let fixtureReady;
let window;
let finished = false;
let sessionDirectory;
let sourceErrorTail = '';
let sourcePolling;
let sourceArchiveBytes = 0;
let sourceArchiveCount = 0;
const sourceHistory = new Map();
let finalProjection;
let dialogCount = 0;
const observationRefreshLabels = new Set();
let fixtureStdoutBuffer = '';
let fixtureStderr = '';
const stderrWrite = process.stderr.write.bind(process.stderr);
process.stderr.write = (...args) => {
  const text = Buffer.isBuffer(args[0]) ? args[0].toString('utf8') : String(args[0]);
  const matches = [...text.matchAll(/cu-source: native watch manifest ([^\n]+)\/native-assets\//g)];
  if (matches.length) sessionDirectory = matches[matches.length - 1][1];
  sourceErrorTail = (sourceErrorTail + text).slice(-8192);
  return stderrWrite(...args);
};

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
async function until(read, label, timeoutMs = 15000) {
  const end = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < end) {
    try { const value = await read(); if (value) return value; } catch (error) { lastError = error; }
    await delay(35);
  }
  throw new Error(`Timeout: ${label}${lastError ? ` (${String(lastError)})` : ''}`);
}
function readStable(file, maximum, privateFile = false) {
  const before = fs.lstatSync(file);
  if (!before.isFile() || before.isSymbolicLink() || before.nlink !== 1 || before.size <= 0
      || before.size > maximum || before.uid !== process.getuid()) throw new Error('bounded owner-owned file unavailable');
  if (privateFile && (before.mode & 0o777) !== 0o600) throw new Error('private file permissions changed');
  const data = fs.readFileSync(file);
  const after = fs.lstatSync(file);
  if (before.dev !== after.dev || before.ino !== after.ino || before.size !== after.size
      || before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs || data.length !== after.size) {
    throw new Error('file changed while being read');
  }
  return { data, sha256: hash(data) };
}
function parseTarget() {
  const { data, sha256 } = readStable(targetPath, 64 * 1024, true);
  if (sha256 !== process.env.CU_MVP_ROW_TARGET_SHA256) throw new Error('independent TargetSpec changed after wrapper validation');
  const text = data.toString('utf8').replace(/\r?\n$/, '');
  const target = JSON.parse(text);
  if (!target || typeof target !== 'object' || !target.row || !Array.isArray(target.row.conditions)) throw new Error('TargetSpec must include an independent row selector');
  return { target, text, sha256 };
}
function readFixtureSnapshot() {
  const { data, sha256 } = readStable(snapshotPath, 128 * 1024);
  const snapshot = JSON.parse(data.toString('utf8'));
  if (snapshot.version !== 1 || snapshot.task_id !== taskId || snapshot.seed !== seed || !snapshot.audit) throw new Error('fixture snapshot identity is invalid');
  return { snapshot, sha256 };
}
function fixtureSummary(value) {
  return {
    snapshot_sha256: value.sha256,
    task_id: value.snapshot.task_id,
    seed: value.snapshot.seed,
    selected_row: value.snapshot.selected_row ?? null,
    selected_row_change_count: value.snapshot.audit.change_counts?.selected_row ?? null,
    revisit_count: value.snapshot.audit.revisit_count ?? null,
    revision: value.snapshot.audit.revision ?? null,
    // row_rebind_count is deliberately omitted: it is not stable identity evidence.
  };
}
function activateFixture() {
  if (!Number.isSafeInteger(fixture?.pid) || fixture.pid <= 0) throw new Error('fixture PID is unavailable');
  // Bring only the exact spawned app process forward; this is window setup, not a task input.
  execFileSync('/usr/bin/osascript', ['-e', `tell application "System Events" to set frontmost of first application process whose unix id is ${fixture.pid} to true`], {
    encoding: 'utf8', timeout: 3000, stdio: ['ignore', 'pipe', 'pipe'],
  });
}
async function freshNativeCandidate(read, label, timeoutMs = 25000) {
  try { return await until(read, label, timeoutMs); }
  catch (error) {
    const state = await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()');
    const projection = await window.webContents.executeJavaScript('window.desktop.snapshot()');
    if (observationRefreshLabels.has(label) || observationRefreshLabels.size >= 2
        || state?.can_refresh_observation !== true || state.phase !== 'observing'
        || projection?.root || !['not_needed', 'released'].includes(projection?.cleanup)) throw error;
    const entry = { label, initial_error: String(error), before: projection, status: state, input_authority: false };
    proof.observation_refreshes = (proof.observation_refreshes || []).concat([entry]);
    observationRefreshLabels.add(label);
    const clicked = await window.webContents.executeJavaScript(`(() => {
      const button = document.querySelector('button[aria-label="Refresh observation"]');
      if (!button || button.disabled) return false;
      button.click(); return true;
    })()`);
    assert.equal(clicked, true, 'the idle observation refresh uses its actual product button');
    await until(async () => {
      activateFixture();
      const next = await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()');
      return next.phase === 'observing' && next.can_authorize ? next : false;
    }, 'fresh read-only candidate after explicit observation refresh', 20000);
    entry.after = await window.webContents.executeJavaScript('window.desktop.snapshot()');
    assert.equal(entry.after?.capabilities?.input, 'unavailable', 'refreshing observations grants no input authority');
    return await until(read, label, 5000);
  }
}
async function waitForFixtureSourceReadiness(target) {
  const expectedWindow = `window-${fixtureReady.window_number}`;
  const watchId = `fixture-readiness-${randomUUID()}`;
  const directory = path.join(scratch, watchId);
  const request = {
    protocol_version: 1,
    command: 'watch',
    bundle_id: bundleId,
    pid: fixture.pid,
    output_dir: directory,
    timeout_ms: 1500,
    max_nodes: 512,
    max_depth: 12,
    max_ocr_blocks: 1,
    interval_ms: 250,
    clock_origin_unix_ms: Date.now(),
    text_policy: { kind: 'registered_editor', window_id: expectedWindow },
    table_scope: target.row.table_scope,
    session_epoch: 1,
    watch_id: watchId,
    surface: { id: 'main', epoch: 1 },
  };
  const maximumDurationMs = 10_000;
  const maximumRecords = 32;
  const maximumArchiveBytes = 4 * 1024 * 1024;
  const maximumRecordBytes = 768 * 1024;
  const maximumStderrBytes = 8 * 1024;
  fs.mkdirSync(directory, { mode: 0o700 });
  proof.fixture_source_readiness = {
    kind: 'readonly_fixture_setup',
    watch_id: watchId,
    session_epoch: request.session_epoch,
    surface: request.surface,
    reads: [],
    ok: false,
    input_authority: false,
    source_session_reused_for_product: false,
    maximum_duration_ms: maximumDurationMs,
    maximum_records: maximumRecords,
    maximum_archive_bytes: maximumArchiveBytes,
  };

  const child = spawn(path.join(root, 'target/debug/cu-macos-source'), [], {
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let settled = false;
  let lastSequence = 0;
  let pending = Buffer.alloc(0);
  let archiveBytes = 0;
  let stderrBytes = 0;
  let stderrTruncated = false;
  const stderrPrefix = [];
  let closeInfo = null;
  let resolveReadiness;
  let rejectReadiness;
  const readiness = new Promise((resolve, reject) => {
    resolveReadiness = resolve;
    rejectReadiness = reject;
  });
  const fail = error => {
    if (settled) return;
    settled = true;
    rejectReadiness(error instanceof Error ? error : new Error(String(error)));
  };
  const closePromise = new Promise(resolve => {
    child.once('close', (code, signal) => {
      closeInfo = { code, signal };
      if (!settled) fail(new Error(`native readiness watch exited before strict readiness (code=${code}, signal=${signal})`));
      resolve(closeInfo);
    });
  });
  child.once('error', error => fail(new Error(`native readiness watch could not start: ${String(error)}`)));
  child.stdin.on('error', error => fail(new Error(`native readiness watch input failed: ${String(error)}`)));
  child.stderr.on('data', chunk => {
    const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    const keep = Math.min(bytes.length, maximumStderrBytes - stderrBytes);
    if (keep > 0) {
      stderrPrefix.push(Buffer.from(bytes.subarray(0, keep)));
      stderrBytes += keep;
    }
    if (keep < bytes.length) stderrTruncated = true;
  });

  const observeRecord = raw => {
    if (settled) return;
    if (raw.length > maximumRecordBytes) {
      fail(new Error('native readiness watch record exceeded its byte bound'));
      return;
    }
    if (proof.fixture_source_readiness.reads.length >= maximumRecords
        || archiveBytes + raw.length >= maximumArchiveBytes) {
      fail(new Error('native readiness watch exceeded its record archive bound'));
      return;
    }

    const index = proof.fixture_source_readiness.reads.length;
    const file = path.join(directory, `record-${String(index).padStart(2, '0')}.ndjson`);
    const bytes = Buffer.from(raw);
    const recordEvidence = { file, sha256: hash(bytes), bytes: bytes.length, kind: null, sequence: null, ready: false };
    try { fs.writeFileSync(file, bytes, { flag: 'wx', mode: 0o600 }); }
    catch (error) {
      proof.fixture_source_readiness.archive_error = String(error).slice(0, 512);
      fail(new Error(`native readiness record could not be archived: ${String(error).slice(0, 512)}`));
      return;
    }
    archiveBytes += bytes.length;
    proof.fixture_source_readiness.archive_bytes = archiveBytes;

    let record;
    try { record = JSON.parse(bytes.toString('utf8')); }
    catch {
      proof.fixture_source_readiness.reads.push(recordEvidence);
      fail(new Error('native readiness watch emitted malformed NDJSON'));
      return;
    }
    recordEvidence.kind = typeof record.kind === 'string' ? record.kind : null;
    recordEvidence.sequence = Number.isSafeInteger(record.sequence) ? record.sequence : null;
    proof.fixture_source_readiness.reads.push(recordEvidence);
    if (record.kind === 'error') {
      fail(new Error(`native readiness watch returned an error: ${String(record.error ?? 'unknown').slice(0, 512)}`));
      return;
    }
    if (record.protocol_version !== 1 || record.stream_id !== watchId
        || !Number.isSafeInteger(record.sequence) || record.sequence <= lastSequence
        || !['observation', 'frame', 'health', 'gap', 'capture_result'].includes(record.kind)) {
      fail(new Error('native readiness watch event identity or shape was invalid'));
      return;
    }
    lastSequence = record.sequence;
    if (record.kind !== 'observation') return;

    const structure = record.structure;
    const census = structure?.table_scope_search;
    const windows = Array.isArray(structure?.windows)
      ? structure.windows.filter(item => item.window_id === expectedWindow) : [];
    const roots = Array.isArray(structure?.objects)
      ? structure.objects.filter(item => item.window_id === expectedWindow && item.parent == null) : [];
    const context = structure?.window_context;
    const process = context?.process;
    const ready = structure?.app?.pid === fixture.pid && structure.app.bundle_id === bundleId
      && structure.active_layer === expectedWindow && windows.length === 1
      && windows[0].kind === 'window' && windows[0].owner_bundle_id === bundleId && !windows[0].modal
      && roots.length === 1 && roots[0].role === 'AXWindow' && roots[0].subrole === 'AXStandardWindow'
      && roots[0].in_view === true && roots[0].secure === false
      && isDeepStrictEqual(roots[0].frame, windows[0].frame)
      && !structure.objects.some(item => item.secure === true)
      && structure.incomplete_reason === 'AX table row subtree omitted for scoped census'
      && census?.state?.state === 'complete' && census.candidates?.length === 1
      && census.process_id === fixture.pid && census.window_id === expectedWindow
      && isDeepStrictEqual(census.requested_scope, target.row.table_scope)
      && typeof context?.source_session_id === 'string' && context.source_session_id.trim().length > 0
      && context.source_session_id.length <= 256 && context.session_epoch === request.session_epoch
      && process?.pid === fixture.pid && Number.isSafeInteger(process.start_seconds) && process.start_seconds > 0
      && Number.isSafeInteger(process.start_micros) && process.start_micros >= 0 && process.start_micros < 1_000_000
      && context.origin_window_id === expectedWindow
      && isDeepStrictEqual(context.origin_window_frame, windows[0].frame)
      && context.verified_source_sequence === record.sequence
      && context.verified_observed_ms === record.structure_finished_unix_ms;
    recordEvidence.ready = Boolean(ready);
    recordEvidence.incomplete_reason = structure?.incomplete_reason ?? null;
    if (ready) {
      settled = true;
      resolveReadiness({ source_sequence: record.sequence, observed_ms: record.structure_finished_unix_ms });
    }
  };

  child.stdout.on('data', chunk => {
    if (settled) return;
    pending = Buffer.concat([pending, Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)]);
    if (!pending.includes(0x0a) && pending.length > maximumRecordBytes) {
      fail(new Error('native readiness watch unterminated record exceeded its byte bound'));
      return;
    }
    let newline;
    while (!settled && (newline = pending.indexOf(0x0a)) >= 0) {
      const raw = pending.subarray(0, newline + 1);
      pending = pending.subarray(newline + 1);
      observeRecord(raw);
      if (!pending.includes(0x0a) && pending.length > maximumRecordBytes) {
        fail(new Error('native readiness watch unterminated record exceeded its byte bound'));
      }
    }
  });
  child.stdout.on('end', () => {
    if (!settled && pending.length > 0) fail(new Error('native readiness watch ended with an incomplete JSON record'));
  });

  const deadline = setTimeout(() => fail(new Error('native readiness watch exceeded 10 seconds')), maximumDurationMs);
  let strictRead = null;
  let failure = null;
  try {
    activateFixture();
    child.stdin.write(`${JSON.stringify(request)}\n`);
    strictRead = await readiness;
  } catch (error) {
    failure = error instanceof Error ? error : new Error(String(error));
  } finally {
    clearTimeout(deadline);
    if (!child.stdin.destroyed && !child.stdin.writableEnded) child.stdin.end();
    if (!closeInfo && child.pid) {
      child.kill('SIGTERM');
      closeInfo = await Promise.race([closePromise, delay(750).then(() => null)]);
      if (!closeInfo) {
        child.kill('SIGKILL');
        closeInfo = await Promise.race([closePromise, delay(750).then(() => null)]);
      }
    } else if (!closeInfo && !child.pid) {
      closeInfo = await Promise.race([closePromise, delay(250).then(() => null)]);
    }
    const readinessProof = proof.fixture_source_readiness;
    readinessProof.stderr_prefix = Buffer.concat(stderrPrefix).toString('utf8');
    readinessProof.stderr_prefix_bytes = stderrBytes;
    readinessProof.stderr_prefix_truncated = stderrTruncated;
    readinessProof.helper_exit_confirmed = closeInfo !== null;
    readinessProof.helper_exit = closeInfo;
    if (closeInfo === null && !failure) failure = new Error('native readiness watch exit could not be confirmed');
    if (closeInfo === null && failure) failure = new AggregateError([failure], 'native readiness watch failed and its exit could not be confirmed');
  }
  if (failure) {
    proof.fixture_source_readiness.error = String(failure).slice(0, 1024);
    throw failure;
  }
  proof.fixture_source_readiness.strict_read = strictRead;
  proof.fixture_source_readiness.ok = true;
}
function readLedger() {
  if (!sessionDirectory) return { status: 'unknown', reason: 'host_session_directory_not_observed' };
  const db = path.join(sessionDirectory, 'host.db');
  if (!fs.existsSync(db)) return { status: 'unknown', reason: 'host_database_not_found' };
  const script = [
    'import sqlite3,json,sys',
    'c=sqlite3.connect("file:"+sys.argv[1]+"?mode=ro",uri=True)',
    'c.row_factory=sqlite3.Row',
    'tables=["root_tasks","attempts","decisions","attempt_map","admission_outbox","effect_results"]',
    'd={t:[dict(r) for r in c.execute("select * from "+t)] for t in tables}',
    'for r in d["effect_results"]: r["result"]=json.loads(r.pop("json"))',
    'print(json.dumps(d))',
  ].join('\n');
  const output = execFileSync('python3', ['-c', script, db], { encoding: 'utf8', timeout: 2000, maxBuffer: 2 * 1024 * 1024 });
  return { status: 'available', ...JSON.parse(output) };
}
function readNativeReceipts(category) {
  if (!['row-submit-receipt', 'ax-press-submit-receipt', 'click-submit-receipt', 'window-activation-submit-receipt'].includes(category)) throw new Error('unsupported receipt category');
  let names;
  try {
    const directory = fs.lstatSync(diagnosticsDirectory);
    if (!directory.isDirectory() || directory.isSymbolicLink() || directory.uid !== process.getuid()
        || (directory.mode & 0o777) !== 0o700) throw new Error('diagnostic directory is not private');
    names = fs.readdirSync(diagnosticsDirectory).filter(name => new RegExp(`^\\d{2}-${category}\\.json$`).test(name)).sort();
  }
  catch { return { status: 'unknown', reason: 'row_diagnostic_directory_unavailable', records: [] }; }
  const records = [];
  for (const name of names) {
    try {
      const { data, sha256 } = readStable(path.join(diagnosticsDirectory, name), 16 * 1024, true);
      records.push({ file: name, sha256, value: JSON.parse(data.toString('utf8')) });
    } catch (error) { records.push({ file: name, error: String(error) }); }
  }
  return { status: 'available', records };
}
function readRowReceipts() { return readNativeReceipts('row-submit-receipt'); }
function exactKeys(value, keys) {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value)
    && JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...keys].sort()));
}
function validateNativeReceipt(record, rootTask, attempt, sessionEpoch, traceKind) {
  if (!record?.value || !rootTask || !attempt) return { ok: false, reason: 'missing_request_or_attempt_correlation' };
  const receipt = record.value;
  const row = traceKind === 'row';
  const traceField = row ? 'row_trace' : 'ax_trace';
  const category = row ? 'row-submit-receipt' : 'ax-press-submit-receipt';
  const topKeys = ['kind', 'category', 'protocol_version', 'response_kind', 'session_epoch', 'request_id', 'outcome', 'reason',
    'submitted', 'committed_ms', 'returned_ms', 'binding_digest', traceField, 'correlation'];
  const correlationKeys = ['intent_id', 'attempt_id', 'task_id', 'task_revision', 'root_epoch', 'session_epoch', 'request_id'];
  const traceKeys = ['primitive', 'ax_call_count', 'ax_error_code', 'clock_domain', 'call_started_uptime_ns', 'call_finished_uptime_ns'];
  if (!exactKeys(receipt, topKeys) || !exactKeys(receipt.correlation, correlationKeys) || !exactKeys(receipt[traceField], traceKeys)) {
    return { ok: false, reason: 'unexpected_or_incomplete_native_receipt_shape' };
  }
  const c = receipt.correlation, t = receipt[traceField];
  if (receipt.kind !== 'owned_row_guard_diagnostic' || receipt.category !== category
      || receipt.protocol_version !== 1 || receipt.response_kind !== 'atomic_submit'
      || receipt.request_id !== c.request_id || receipt.session_epoch !== c.session_epoch
      || receipt.session_epoch !== sessionEpoch || c.intent_id !== attempt.intent_id
      || c.attempt_id !== attempt.attempt_id || c.task_id !== rootTask.root_id
      || c.task_revision !== attempt.task_revision || c.root_epoch !== attempt.root_epoch
      || receipt.submitted !== true || !(row ? receipt.outcome === 'applied' : ['applied', 'unknown'].includes(receipt.outcome))
      || !Number.isSafeInteger(receipt.committed_ms)
      || !(receipt.returned_ms === null && !row && receipt.outcome === 'unknown'
        || Number.isSafeInteger(receipt.returned_ms) && receipt.returned_ms >= receipt.committed_ms)
      || typeof receipt.binding_digest !== 'string' || !receipt.binding_digest
      || t.primitive !== (row ? 'ax_set_selected_true' : 'ax_press') || t.ax_call_count !== 1
      || !Number.isInteger(t.ax_error_code) || t.ax_error_code < -2147483648 || t.ax_error_code > 2147483647
      || ((row || receipt.outcome === 'applied') && t.ax_error_code !== 0)
      || t.clock_domain !== 'dispatch_time_uptime'
      || !Number.isSafeInteger(t.call_started_uptime_ns) || !Number.isSafeInteger(t.call_finished_uptime_ns)
      || t.call_started_uptime_ns <= 0 || t.call_finished_uptime_ns < t.call_started_uptime_ns
      || t.call_finished_uptime_ns - t.call_started_uptime_ns > 1000000000) {
    return { ok: false, reason: 'native_receipt_does_not_match_one_recorded_attempt' };
  }
  return { ok: true, request_id: receipt.request_id, attempt_id: attempt.attempt_id, intent_id: attempt.intent_id,
    session_epoch: receipt.session_epoch, committed_ms: receipt.committed_ms, returned_ms: receipt.returned_ms,
    binding_digest: receipt.binding_digest, outcome: receipt.outcome, [traceField]: t };
}
function validateRowReceipt(record, rootTask, attempt, sessionEpoch) {
  return validateNativeReceipt(record, rootTask, attempt, sessionEpoch, 'row');
}
function validateClickReceipt(record, rootTask, attempt, sessionEpoch) {
  const r = record?.value, c = r?.correlation;
  const fields = ['kind', 'category', 'protocol_version', 'response_kind', 'session_epoch', 'request_id', 'outcome', 'reason',
    'submitted', 'committed_ms', 'returned_ms', 'binding_digest', 'primitive', 'correlation'];
  const identityFields = ['intent_id', 'attempt_id', 'task_id', 'task_revision', 'root_epoch', 'session_epoch', 'request_id'];
  if (!rootTask || !attempt || !exactKeys(r, fields) || !exactKeys(c, identityFields)
      || r.kind !== 'owned_row_guard_diagnostic' || r.category !== 'click-submit-receipt' || r.primitive !== 'click'
      || r.protocol_version !== 1 || r.response_kind !== 'atomic_submit' || r.submitted !== true || r.outcome !== 'applied'
      || r.session_epoch !== sessionEpoch || c.session_epoch !== sessionEpoch || c.request_id !== r.request_id
      || c.task_id !== rootTask.root_id || c.attempt_id !== attempt.attempt_id
      || c.task_revision !== attempt.task_revision || c.root_epoch !== attempt.root_epoch
      || typeof c.intent_id !== 'string' || !c.intent_id || c.intent_id.length > 256 || /[\u0000-\u001f]/u.test(c.intent_id)
      || !Number.isSafeInteger(r.committed_ms) || !Number.isSafeInteger(r.returned_ms) || r.returned_ms < r.committed_ms
      || typeof r.binding_digest !== 'string' || !r.binding_digest) {
    return { ok: false, reason: 'click_receipt_missing_or_not_correlated_to_the_frozen_attempt' };
  }
  return { ok: true, request_id: r.request_id, intent_id: c.intent_id, attempt_id: c.attempt_id,
    committed_ms: r.committed_ms, returned_ms: r.returned_ms, binding_digest: r.binding_digest,
    intent_matches_latest_ledger: c.intent_id === attempt.intent_id,
    correlation_source: 'trusted_native_guard_pending_request', os_event_count: 'unknown' };
}
function captureRowReceiptProof(ledger, projection) {
  const frozenRoot = proof.root_before_execute;
  const rootTask = ledger?.root_tasks?.find(item => item.root_id === frozenRoot?.root_task_id);
  const attempts = (ledger?.attempts || []).filter(item => item.root_id === rootTask?.root_id
    && item.skill === 'ui.select_row');
  const attempt = attempts.length === 1 ? attempts[0] : null;
  const unchangedGoal = rootTask && proof.frozen_goal
    && JSON.stringify(JSON.parse(rootTask.goal_json)) === JSON.stringify(proof.frozen_goal);
  const frozenAttempt = unchangedGoal && attempt?.activation_seq > 0
    && attempt.task_revision === frozenRoot.task_revision && attempt.root_epoch === frozenRoot.root_epoch;
  const sessionEpoch = proof.native_session_epoch ?? projection?.capabilities?.identity?.session_epoch;
  proof.native_row_receipt_checks = (proof.row_receipts?.records || []).map(record =>
    validateRowReceipt(record, frozenAttempt ? rootTask : null, attempt, sessionEpoch));
  const validReceipts = proof.native_row_receipt_checks.filter(item => item.ok);
  proof.native_row_setter_call_count = validReceipts.length === 1 && proof.row_receipts.records.length === 1
    ? { status: 'verified', count: 1, primitive: 'ax_set_selected_true', response_outcome: 'applied', receipt: validReceipts[0] }
    : { status: 'unknown', count: null, reason: 'missing_duplicate_or_unmatched_native_row_receipt' };
}
function sourceManifest(session) {
  if (!session) return { status: 'unavailable', reason: 'native_source_session_directory_not_observed' };
  const assetRoot = path.join(session, 'native-assets');
  let realRoot;
  try { realRoot = fs.realpathSync(assetRoot); } catch { return { status: 'unavailable', reason: 'native_assets_directory_missing' }; }
  const queue = [{ directory: realRoot, depth: 0 }];
  const candidates = [];
  let visited = 0;
  let bytesRead = 0;
  while (queue.length && visited < 512) {
    const current = queue.shift();
    if (current.depth > 4) continue;
    let entries;
    try { entries = fs.readdirSync(current.directory, { withFileTypes: true }); } catch { continue; }
    for (const entry of entries) {
      if (++visited > 512) break;
      const file = path.join(current.directory, entry.name);
      if (entry.isDirectory()) { queue.push({ directory: file, depth: current.depth + 1 }); continue; }
      if (!entry.isFile() || !['manifest.json', 'latest.json', 'watch-manifest.json'].includes(entry.name)) continue;
      try {
        const real = fs.realpathSync(file);
        if (!real.startsWith(realRoot + path.sep)) continue;
        const read = readStable(real, 2 * 1024 * 1024);
        bytesRead += read.data.length;
        if (bytesRead > 24 * 1024 * 1024) break;
        const parsed = JSON.parse(read.data.toString('utf8'));
        if (typeof parsed.manifest === 'string') {
          const referenced = fs.realpathSync(path.resolve(path.dirname(real), parsed.manifest));
          if (!referenced.startsWith(realRoot + path.sep)) continue;
          const linked = readStable(referenced, 2 * 1024 * 1024);
          bytesRead += linked.data.length;
          if (bytesRead > 24 * 1024 * 1024) break;
          candidates.push({ file: referenced, sha256: linked.sha256, value: JSON.parse(linked.data.toString('utf8')), mtime: fs.statSync(referenced).mtimeMs });
        } else candidates.push({ file: real, sha256: read.sha256, value: parsed, mtime: fs.statSync(real).mtimeMs });
      } catch { /* partial rotation is not evidence */ }
    }
  }
  const applicable = candidates.filter(item => item.value?.snapshot?.app?.bundle_id === bundleId
    && item.value.snapshot.app.pid === fixture.pid && Array.isArray(item.value.snapshot.table_viewports));
  if (!applicable.length) return { status: 'unavailable', reason: 'fresh_native_structure_manifest_not_found', inspected_files: candidates.length };
  applicable.sort((a, b) => b.mtime - a.mtime);
  return { status: 'available', candidates: applicable.slice(0, 32).map(item => ({
    manifest: item.file, sha256: item.sha256, mtime: item.mtime,
    snapshot_id: item.value.snapshot.snapshot_id ?? null,
    snapshot: item.value.snapshot,
  })) };
}
function valueMatches(actual, expected) {
  return Boolean(actual && actual.type === expected.type && Object.is(actual.value, expected.value));
}
function rowSourceEvidence(source, target, rootEffect, expectedWindowId = null) {
  if (source.status !== 'available') return { status: 'unknown', reason: source.reason };
  const evidenceIds = new Set(Array.isArray(rootEffect?.evidence_group) ? rootEffect.evidence_group : []);
  const usable = source.candidates.filter(item => item.snapshot_id && evidenceIds.has(item.snapshot_id));
  const candidate = usable[0] ?? source.candidates[0];
  const snapshot = candidate.snapshot;
  const objects = new Map((snapshot.objects ?? []).map(object => [object.instance_ref, object]));
  const requiredHeaders = target.row.table_scope.column_headers;
  const tables = [];
  for (const viewport of snapshot.table_viewports) {
    if (viewport.columns_complete !== true || viewport.visible_rows_complete !== true
        || !Array.isArray(viewport.columns) || !Array.isArray(viewport.visible_rows)) continue;
    const headers = viewport.columns.map(column => column.header).filter(header => typeof header === 'string');
    if (viewport.process_id !== fixture.pid || headers.length !== viewport.columns.length
        || headers.length !== requiredHeaders.length || new Set(headers).size !== headers.length
        || requiredHeaders.some(header => !headers.includes(header))) continue;
    const table = objects.get(viewport.table_instance_ref);
    if (table?.role !== 'AXTable' || (expectedWindowId && table.window_id !== expectedWindowId)) continue;
    const actualScrollPath = [];
    let parent = table;
    let depth = 0;
    while (parent?.parent && depth++ < 32) {
      const ancestor = objects.get(parent.parent);
      if (!ancestor) break;
      if (ancestor.role === 'AXScrollArea') actualScrollPath.push(ancestor.description ?? ancestor.title ?? null);
      parent = ancestor;
    }
    actualScrollPath.reverse();
    const expectedPath = target.row.table_scope.scroll_path;
    if (actualScrollPath.length !== expectedPath.length || expectedPath.some((part, index) =>
      part.role !== 'ax_scroll_area' || actualScrollPath[index] !== part.label)) continue;
    const matched = viewport.visible_rows.filter(row => {
      if (!Array.isArray(row.cells)) return false;
      const cells = new Map(row.cells.map(cell => [cell.column_header, cell.value]));
      return target.row.conditions.every(condition => valueMatches(cells.get(condition.column_name), condition.value));
    });
    tables.push({ viewport, matched });
  }
  if (tables.length !== 1 || tables[0].matched.length !== 1) {
    return { status: 'unknown', reason: 'native_source_table_or_row_is_not_unique', table_count: tables.length,
      matching_row_count: tables.length === 1 ? tables[0].matched.length : null };
  }
  const row = tables[0].matched[0];
  const rowObject = objects.get(row.instance_ref);
  const window = snapshot.windows?.find(item => item.window_id === expectedWindowId);
  const inGoalEvidence = evidenceIds.has(snapshot.snapshot_id);
  const exactSelectedRow = row.selected === true && row.ax_selected_settable === true
    && Number.isInteger(row.ax_index) && row.ax_index >= 0 && row.ax_index <= 0xffff_ffff
    && rowObject?.role === 'AXRow' && rowObject.window_id === expectedWindowId
    && snapshot.active_layer === expectedWindowId && window?.owner_bundle_id === bundleId;
  return {
    status: exactSelectedRow && inGoalEvidence ? 'verified' : 'incomplete',
    manifest_sha256: candidate.sha256,
    snapshot_id: snapshot.snapshot_id ?? null,
    source_sequence: snapshot.source_sequence ?? null,
    observed_ms: snapshot.observed_ms ?? null,
    active_layer: snapshot.active_layer ?? null,
    window_id: expectedWindowId,
    window_title: window?.title ?? null,
    app: { bundle_id: snapshot.app.bundle_id, pid: snapshot.app.pid },
    observer_coverage: snapshot.observer_coverage ?? null,
    table_column_headers: tables[0].viewport.columns.map(column => column.header),
    target_row_ax_index: row.ax_index ?? null,
    target_selected: row.selected ?? null,
    target_ax_selected_settable: row.ax_selected_settable ?? null,
    target_role: rowObject?.role ?? null,
    target_window_id: rowObject?.window_id ?? null,
    source_snapshot_is_in_completed_root_effect: inGoalEvidence,
  };
}
function compactSource(source) {
  if (source.status !== 'available') return source;
  return { status: source.status, candidates: source.candidates.map(item => ({
    manifest_name: path.basename(item.manifest), sha256: item.sha256,
    snapshot_id: item.snapshot_id,
    source_sequence: item.snapshot.source_sequence ?? null,
    observed_ms: item.snapshot.observed_ms ?? null,
    app: { bundle_id: item.snapshot.app.bundle_id, pid: item.snapshot.app.pid },
    active_layer: item.snapshot.active_layer ?? null,
  })) };
}
function rememberSourceSnapshots(source) {
  if (source.status !== 'available') return;
  for (const item of source.candidates) if (item.snapshot_id) sourceHistory.set(item.snapshot_id, item);
  while (sourceHistory.size > 128) sourceHistory.delete(sourceHistory.keys().next().value);
}
function captureSourceHistory() {
  const current = sourceManifest(sessionDirectory);
  rememberSourceSnapshots(current);
  const archiveDirectory = path.join(scratch, 'source-snapshots');
  if (!fs.existsSync(archiveDirectory)) fs.mkdirSync(archiveDirectory, { mode: 0o700 });
  for (const item of sourceHistory.values()) {
    const filename = `${hash(Buffer.from(item.snapshot_id)).slice(0, 24)}.json`;
    const destination = path.join(archiveDirectory, filename);
    if (fs.existsSync(destination)) continue;
    const serialized = Buffer.from(JSON.stringify(item.snapshot));
    if (serialized.length > 1024 * 1024 || sourceArchiveBytes + serialized.length > 16 * 1024 * 1024 || sourceArchiveCount >= 16) {
      proof.source_snapshot_archive_truncated = true;
      continue;
    }
    try {
      const fd = fs.openSync(destination, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
      try { fs.fchmodSync(fd, 0o600); fs.writeFileSync(fd, serialized); } finally { fs.closeSync(fd); }
      sourceArchiveBytes += serialized.length;
      sourceArchiveCount += 1;
    } catch { /* a concurrent capture may already have published this id */ }
  }
  return current;
}
function sourceHistoryForRoot(rootEffect) {
  const current = captureSourceHistory();
  const currentCandidates = current.status === 'available' ? current.candidates : [];
  const byId = new Map(currentCandidates.filter(item => item.snapshot_id).map(item => [item.snapshot_id, item]));
  for (const [id, item] of sourceHistory) if (!byId.has(id)) byId.set(id, item);
  const evidenceIds = new Set(Array.isArray(rootEffect?.evidence_group) ? rootEffect.evidence_group : []);
  const ordered = [...byId.values()].sort((a, b) => b.mtime - a.mtime);
  const evidence = ordered.filter(item => evidenceIds.has(item.snapshot_id));
  return { status: ordered.length ? 'available' : current.status, candidates: (evidence.length ? evidence : ordered).slice(0, 128) };
}
function backupHostDatabase() {
  if (!sessionDirectory) return { status: 'unknown', reason: 'host_session_directory_not_observed' };
  const database = path.join(sessionDirectory, 'host.db');
  try {
    const metadata = fs.lstatSync(database);
    if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.nlink !== 1 || metadata.uid !== process.getuid()
        || metadata.size <= 0 || metadata.size > 64 * 1024 * 1024) return { status: 'unknown', reason: 'host_database_outside_readonly_backup_bounds' };
    const destination = path.join(scratch, 'host-database.sqlite');
    const script = [
      'import sqlite3,sys,os',
      'source=sqlite3.connect("file:"+sys.argv[1]+"?mode=ro",uri=True)',
      'target=sqlite3.connect(sys.argv[2])',
      'source.backup(target,pages=128,sleep=0.01)',
      'target.close(); source.close()',
      'os.chmod(sys.argv[2],0o600)',
    ].join('\n');
    execFileSync('python3', ['-c', script, database, destination], { timeout: 5000, maxBuffer: 1024 * 1024 });
    const copied = readStable(destination, 64 * 1024 * 1024, true);
    return { status: 'available', path: destination, sha256: copied.sha256, bytes: copied.data.length, source_size: metadata.size };
  } catch (error) { return { status: 'unknown', reason: `read_only_sqlite_backup_failed:${String(error)}` }; }
}
function archiveDeviceInputJournalIfPresent() {
  if (!sessionDirectory) return { status: 'unknown', reason: 'host_session_directory_not_observed' };
  const source = path.join(sessionDirectory, 'owned-input-journal.jsonl');
  try {
    if (!fs.existsSync(source)) return { status: 'unavailable', reason: 'no_owned_device_input_journal_for_row_scope' };
    const copied = readStable(source, 1024 * 1024, true);
    const destination = path.join(scratch, 'device-input-journal.jsonl');
    const fd = fs.openSync(destination, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
    try { fs.fchmodSync(fd, 0o600); fs.writeFileSync(fd, copied.data); } finally { fs.closeSync(fd); }
    return { status: 'available', path: destination, sha256: copied.sha256, bytes: copied.data.length };
  } catch (error) { return { status: 'unknown', reason: String(error) }; }
}
async function closeFixture() {
  if (!fixture || fixtureClosed) return { exit_confirmed: fixtureClosed, termination_requested: false, forced_kill: false };
  let terminationRequested = false, forcedKill = false;
  if (fixture.exitCode === null && fixture.signalCode === null) {
    terminationRequested = fixture.kill('SIGTERM');
    await Promise.race([new Promise(resolve => fixture.once('exit', resolve)), delay(1800)]);
    if (fixture.exitCode === null && fixture.signalCode === null) {
      forcedKill = fixture.kill('SIGKILL');
      await Promise.race([new Promise(resolve => fixture.once('exit', resolve)), delay(2200)]);
    }
  }
  fixtureClosed = fixture.exitCode !== null || fixture.signalCode !== null;
  return { exit_confirmed: fixtureClosed, termination_requested: terminationRequested, forced_kill: forcedKill };
}
async function requestStop(reason) {
  proof.stop_request = { requested: false, reason };
  if (!window || window.isDestroyed()) return;
  try {
    await window.webContents.executeJavaScript('window.desktop.stop()');
    proof.stop_request.requested = true;
    proof.stop_request.last_projection = await until(async () => {
      const current = await window.webContents.executeJavaScript('window.desktop.snapshot()');
      return current?.root && ['cancelled', 'failed', 'completed'].includes(current.root.state) ? current : false;
    }, 'Stop terminal projection', 5000);
  } catch (error) { proof.stop_request.error = String(error); }
}
async function finish(error) {
  if (finished) return;
  finished = true;
  if (sourcePolling) clearInterval(sourcePolling);
  if (error) { proof.error = error?.stack || String(error); await requestStop('diagnostic_failure_or_cancel'); }
  proof.fixture_stderr = fixtureStderr;
  proof.fixture_stdout_tail = fixtureStdoutBuffer.slice(-4096);
  try { if (window && !window.isDestroyed()) finalProjection = await window.webContents.executeJavaScript('window.desktop.snapshot()'); }
  catch (diagnostic) { proof.final_projection_error = String(diagnostic); }
  proof.final_projection = finalProjection ?? null;
  try { proof.window_activation_receipts ??= readNativeReceipts('window-activation-submit-receipt'); }
  catch (diagnostic) { proof.window_activation_receipts_error = String(diagnostic); }
  try { proof.final_fixture_snapshot ??= fixtureSummary(readFixtureSnapshot()); }
  catch (diagnostic) { proof.fixture_snapshot_error = String(diagnostic); }
  try { proof.row_receipts ??= readRowReceipts(); } catch (diagnostic) { proof.row_receipt_error = String(diagnostic); }
  try { proof.ledger ??= readLedger(); } catch (diagnostic) { proof.ledger_error = String(diagnostic); }
  try { captureRowReceiptProof(proof.ledger, finalProjection ?? proof.stop_request?.last_projection); }
  catch (diagnostic) { proof.native_row_receipt_error = String(diagnostic); }
  try {
    proof.ax_press_receipts = readNativeReceipts('ax-press-submit-receipt');
    const frozenRoot = proof.root_before_execute;
    const rootTask = proof.ledger?.root_tasks?.find(item => item.root_id === frozenRoot?.root_task_id);
    proof.ax_press_receipt_checks = proof.ax_press_receipts.records.map(record => {
      const attempt = proof.ledger?.attempts?.find(item => item.root_id === rootTask?.root_id
        && item.attempt_id === record.value?.correlation?.attempt_id && item.skill === 'ui.revisit'
        && item.root_epoch === frozenRoot?.root_epoch && item.task_revision === frozenRoot?.task_revision
        && item.activation_seq > 0);
      return validateNativeReceipt(record, rootTask, attempt, proof.native_session_epoch
        ?? finalProjection?.capabilities?.identity?.session_epoch, 'ax_press');
    });
  } catch (diagnostic) { proof.ax_press_receipt_error = String(diagnostic); }
  try {
    proof.click_receipts = readNativeReceipts('click-submit-receipt');
    const rootTask = proof.ledger?.root_tasks?.find(item => item.root_id === proof.root_before_execute?.root_task_id);
    proof.click_receipt_checks = proof.click_receipts.records.map(record => {
      const attempt = proof.ledger?.attempts?.find(item => item.root_id === rootTask?.root_id && item.skill === 'ui.revisit'
        && item.attempt_id === record.value?.correlation?.attempt_id && item.activation_seq > 0
        && item.root_epoch === proof.root_before_execute?.root_epoch && item.task_revision === proof.root_before_execute?.task_revision);
      return validateClickReceipt(record, rootTask, attempt, proof.native_session_epoch);
    });
    const checks = proof.click_receipt_checks;
    proof.navigation_native_click_receipts = { status: checks.length === 2 && checks.every(item => item.ok)
      && new Set(checks.map(item => item.request_id)).size === 2 && new Set(checks.map(item => item.intent_id)).size === 2
      ? 'verified' : 'incomplete', applied_request_count: checks.filter(item => item.ok).length, os_event_count: 'unknown' };
  } catch (diagnostic) { proof.click_receipt_error = String(diagnostic); }
  try {
    const source = sourceHistoryForRoot(proof.goal_effect);
    proof.native_source ??= compactSource(source);
    proof.source_observation_history = [...sourceHistory.values()].map(item => ({
      snapshot_id: item.snapshot_id,
      source_sequence: item.snapshot.source_sequence ?? null,
      observed_ms: item.snapshot.observed_ms ?? null,
      app: { bundle_id: item.snapshot.app.bundle_id, pid: item.snapshot.app.pid },
      active_layer: item.snapshot.active_layer ?? null,
      sha256: item.sha256,
    }));
  } catch (diagnostic) { proof.native_source_error = String(diagnostic); }
  proof.source_session_directory_observed = Boolean(sessionDirectory);
  proof.source_stderr_tail = sourceErrorTail;
  // Supervisor.close removes its private session directory. Preserve a
  // read-only SQLite backup and any applicable device journal before revoke.
  proof.host_database_backup = backupHostDatabase();
  proof.device_input_journal = archiveDeviceInputJournalIfPresent();
  try {
    const archiveDirectory = path.join(scratch, 'source-snapshots');
    proof.source_snapshot_archive = fs.existsSync(archiveDirectory)
      ? fs.readdirSync(archiveDirectory).filter(name => name.endsWith('.json')).map(name => {
        const file = path.join(archiveDirectory, name), item = readStable(file, 1024 * 1024, true);
        return { file, sha256: item.sha256, bytes: item.data.length };
      }) : [];
  } catch (diagnostic) { proof.source_snapshot_archive_error = String(diagnostic); }
  try {
    if (window && !window.isDestroyed() && (await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()')).phase === 'authorized') {
      await window.webContents.executeJavaScript('window.desktop.revokeNativeWindow()');
      proof.permission_revoked_after_capture = await until(async () =>
        (await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()')).phase === 'observing', 'permission revocation', 8000);
    }
  } catch (diagnostic) { proof.permission_revoke_error = String(diagnostic); }
  proof.fixture_cleanup = await closeFixture();
  proof.fixture_cleanup.exit_code = fixture?.exitCode ?? null;
  proof.fixture_cleanup.signal = fixture?.signalCode ?? null;
  proof.ok = !error && proof.root_completed === true && proof.native_row_setter_call_count.status === 'verified'
    && proof.native_row_setter_call_count.count === 1 && proof.native_row_selection?.status === 'verified'
    && proof.fixture_selection?.selection_changed_once === true && proof.fixture_selection?.selected_row_label_matches_target === true
    && proof.fixture_selection?.revisit_count_delta === 1 && proof.fixture_cleanup.exit_confirmed
    && proof.window_permission_cancel_path_verified === true && proof.window_permission_allow_path_verified === true
    && proof.no_row_action_before_execute === true && proof.execute_selected === true;
  if (activationOnly) {
    proof.ok = !error && proof.window_activation?.ok === true && proof.fixture_cleanup.exit_confirmed
      && proof.window_permission_cancel_path_verified === true && proof.window_permission_allow_path_verified === true
      && proof.no_row_action_before_execute === true && proof.execute_selected === true
      && proof.permission_revoked_after_capture === true && proof.stop_request?.requested === true;
  }
  try {
    const fd = fs.openSync(outputPath, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
    try { fs.fchmodSync(fd, 0o600); fs.writeFileSync(fd, JSON.stringify(proof, null, 2) + '\n'); } finally { fs.closeSync(fd); }
  } catch (writeError) { proof.ok = false; proof.result_write_error = String(writeError); }
  app.once('will-quit', () => app.exit(proof.ok ? 0 : 1));
  app.quit();
}

async function run() {
  const { target, text: targetJson, sha256: targetHash } = parseTarget();
  proof.target_spec = target;
  proof.target_spec_sha256 = targetHash;
  proof.operator_text = `Select fixture row: ${targetJson}`;
  proof.operator_text_sha256 = hash(Buffer.from(proof.operator_text, 'utf8'));
  proof.fixture_binary = fixtureBinary;
  proof.guard_binary = guardPath;

  fixture = spawn(fixtureBinary, ['--profile', profilePath, '--snapshot', snapshotPath], { stdio: ['ignore', 'pipe', 'pipe'] });
  fixture.stdout.on('data', chunk => {
    fixtureStdoutBuffer += chunk.toString('utf8');
    let split;
    while ((split = fixtureStdoutBuffer.indexOf('\n')) >= 0) {
      const line = fixtureStdoutBuffer.slice(0, split);
      fixtureStdoutBuffer = fixtureStdoutBuffer.slice(split + 1);
      try { fixtureReady = JSON.parse(line); } catch { fixtureStdoutBuffer = ''; }
    }
  });
  fixture.stderr.on('data', chunk => { fixtureStderr = (fixtureStderr + chunk.toString('utf8')).slice(-4096); });
  proof.fixture_pid = fixture.pid;
  proof.fixture_ready = await until(() => fixtureReady, 'MVP fixture ready record', 15000);
  assert.equal(proof.fixture_ready.pid, fixture.pid, 'fixture ready record identifies the spawned PID');
  proof.fixture_before = fixtureSummary(await until(() => { try { return readFixtureSnapshot(); } catch { return false; } }, 'initial fixture snapshot', 8000));
  proof.fixture_stderr = fixtureStderr;

  process.env.CU_BIN_DIR = path.join(root, 'target/debug');
  process.env.CU_SOURCE_BACKEND = 'native';
  process.env.CU_TARGET_BUNDLE_ID = bundleId;
  process.env.CU_TARGET_PID = String(fixture.pid);
  assert.ok(Number.isSafeInteger(fixtureReady.window_number) && fixtureReady.window_number > 0
    && fixtureReady.window_number <= 0xffff_ffff, 'the owned fixture exposes its actual CG window number');
  // The independent public TargetSpec selects only a bounded read scope. Input
  // still requires a fresh native consent dialog and a new authorized session.
  process.env.CU_SOURCE_READ_WINDOW_ID = `window-${fixtureReady.window_number}`;
  process.env.CU_SOURCE_TABLE_SCOPE = JSON.stringify(target.row.table_scope);
  process.env.CU_NATIVE_REFERENCE_PLANNER = '1';
  process.env.CU_NATIVE_DEVICE_GUARD = guardPath;
  process.env.CU_OWNED_ROW_DIAGNOSTICS_DIR = diagnosticsDirectory;
  delete process.env.ELECTRON_RENDERER_URL;
  // Establish the owned fixture's visible window before Source starts. A real
  // startup AX failure must continue to clear retained scope and authority.
  activateFixture();
  await waitForFixtureSourceReadiness(target);

  const createdWindow = new Promise(resolve => app.once('browser-window-created', (_event, created) => resolve(created)));
  app.on('browser-window-created', (_event, created) => {
    created.webContents.on('console-message', details => { if (details.level === 'error') proof.console_errors.push(details.message); });
    created.webContents.on('preload-error', (_event, _preload, error) => proof.console_errors.push(String(error)));
  });
  const originalShowDialog = dialog.showMessageBox.bind(dialog);
  dialog.showMessageBox = async (...args) => {
    const options = args.at(-1);
    const detail = typeof options?.detail === 'string' ? options.detail : '';
    assert.equal(options?.cancelId, 0, 'native permission defaults to Cancel');
    assert.equal(options?.defaultId, 0, 'native permission default button is Cancel');
    assert.ok(detail.includes(bundleId) && detail.includes(`Process ${fixture.pid}`), 'native permission names the exact fixture process');
    if (dialogCount >= 2) throw new Error('unexpected extra native permission dialog');
    const action = dialogCount++ === 0 ? 'cancel' : 'allow';
    const shown = originalShowDialog(...args);
    const driven = new Promise((resolve, reject) => execFile(consentDriver, [action], { timeout: 10000 }, (error, stdout) => {
      if (error) reject(error); else resolve(JSON.parse(stdout));
    }));
    const [response, driver] = await Promise.all([shown, driven]);
    assert.equal(driver.status, 'pressed', `real native ${action} button was pressed`);
    if (action === 'cancel') proof.window_permission_cancel_path_verified = response.response === 0;
    else proof.window_permission_allow_path_verified = response.response !== 0;
    proof.native_permission_dialogs = (proof.native_permission_dialogs || []).concat([{ action, response: response.response, driver }]);
    return response;
  };
  await import(path.join(root, 'apps/desktop/out/main/index.js'));
  window = await createdWindow;
  await until(() => !window.isDestroyed() && !window.webContents.isLoading(), 'Operator UI loaded');
  await window.webContents.executeJavaScript('window.desktop.snapshot()');
  sourcePolling = setInterval(() => { try { captureSourceHistory(); } catch {} }, 120);
  sourcePolling.unref();
  activateFixture();
  const candidate = await freshNativeCandidate(async () => {
    activateFixture();
    const state = await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()');
    const projection = await window.webContents.executeJavaScript('window.desktop.snapshot()');
    const observation = projection?.observation;
    const diagnostic = { phase: state?.phase, can_authorize: state?.can_authorize,
      whole_target: observation?.observed_target ?? null, scoped_target: observation?.scoped_window_target ?? null,
      sources: observation?.sources ?? [], frame: observation?.frame ?? null,
      timeline: (projection?.timeline ?? []).slice(-4) };
    proof.candidate_diagnostics ??= [];
    if (JSON.stringify(proof.candidate_diagnostics.at(-1)) !== JSON.stringify(diagnostic)) {
      proof.candidate_diagnostics.push(diagnostic);
      if (proof.candidate_diagnostics.length > 32) proof.candidate_diagnostics.shift();
    }
    const screenshotReady = observation?.frame
      && observation.sources?.some(source => source.source === 'screenshot' && source.state === 'available');
    return state?.can_authorize && screenshotReady ? state : false;
  }, 'fresh supported native window candidate', 25000);
  assert.equal(candidate.candidate.app, bundleId);
  assert.equal(candidate.candidate.pid, fixture.pid);
  proof.observed_candidate = candidate.candidate;
  const prePermission = await window.webContents.executeJavaScript('window.desktop.snapshot()');
  proof.product_source_ready_before_permission = { frame: prePermission?.observation?.frame ?? null,
    screenshot: prePermission?.observation?.sources?.find(source => source.source === 'screenshot') ?? null };
  proof.scoped_consent_source = prePermission?.observation?.scoped_window_target?.source ?? null;
  proof.input_unavailable_before_permission = prePermission?.capabilities?.input === 'unavailable';
  assert.equal(proof.input_unavailable_before_permission, true, 'input is closed before window authorization');

  const clickPermissionButton = () => window.webContents.executeJavaScript(`(() => {
    const button = document.querySelector('.native-authorization button.primary');
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`);
  await until(clickPermissionButton, 'native window permission action');
  await until(() => proof.native_permission_dialogs?.length === 1, 'real Cancel dialog');
  assert.equal((await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()')).phase, 'observing');
  activateFixture();
  await freshNativeCandidate(async () => (await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()')).can_authorize, 'fresh candidate after Cancel', 5000);
  await freshNativeCandidate(clickPermissionButton, 'second window permission action', 5000);
  const authorized = await until(async () => {
    const state = await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()');
    return state.phase === 'authorized' ? state : false;
  }, 'Allow scoped to exact window', 20000);
  assert.equal(proof.window_permission_cancel_path_verified, true, 'Cancel leaves observation only');
  assert.equal(proof.window_permission_allow_path_verified, true, 'Allow follows a fresh second consent');
  assert.equal(authorized.authorized.app, bundleId);
  assert.equal(authorized.authorized.pid, fixture.pid);
  proof.authorized_window = authorized.authorized;
  const afterPermission = await until(async () => {
    const projection = await window.webContents.executeJavaScript('window.desktop.snapshot()');
    return projection?.capabilities?.input === 'available' ? projection : false;
  }, 'authorized device input capability ready', 10000);
  proof.native_session_epoch = afterPermission.capabilities.identity.session_epoch;
  proof.input_available_after_permission = afterPermission.capabilities.input === 'available';
  assert.equal(proof.input_available_after_permission, true);
  const guardProcesses = execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,command='], { encoding: 'utf8' })
    .split('\n').map(line => line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/))
    .filter(row => row && row[3].split(' ')[0] === guardPath);
  assert.equal(guardProcesses.length, 1, 'the exact owned native guard process is unique');
  const guardArgs = guardProcesses[0][3].split(' ');
  const argument = name => guardArgs[guardArgs.indexOf(name) + 1];
  const expectedProcess = proof.scoped_consent_source?.process;
  assert.ok(expectedProcess, 'the consent projection preserves the actual kernel process identity');
  assert.equal(argument('--execute-pid'), String(fixture.pid));
  assert.equal(argument('--execute-window'), proof.authorized_window.window_id);
  assert.equal(argument('--execute-start-seconds'), String(expectedProcess.start_seconds));
  assert.equal(argument('--execute-start-micros'), String(expectedProcess.start_micros));
  proof.native_guard_process_identity = { pid: Number(guardProcesses[0][1]),
    parent_pid: Number(guardProcesses[0][2]), argv: guardArgs, confirmed_process: expectedProcess,
    matched_confirmed_birth: true };
  activateFixture();
  const preTaskSource = captureSourceHistory();
  proof.source_before_execute_initial = compactSource(preTaskSource);

  await window.webContents.executeJavaScript(`(() => {
    const field = document.querySelector('textarea[aria-label="Task"]');
    if (!field || field.disabled) throw new Error('task field unavailable');
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(field, ${JSON.stringify(proof.operator_text)});
    field.dispatchEvent(new Event('input', { bubbles: true }));
    const start = [...document.querySelectorAll('button.primary')].find(button => button.textContent.trim() === 'Start');
    if (!start || start.disabled) throw new Error('Start is unavailable');
    start.click();
  })()`);
  proof.operator_text_sent = true;
  await until(() => window.webContents.executeJavaScript('!!document.querySelector(".prompt[role=alertdialog]")'), 'typed row-goal confirmation');
  const prompt = await window.webContents.executeJavaScript('document.querySelector(".prompt").innerText');
  proof.goal_confirmation_text = prompt;
  assert.ok(prompt.includes(target.label), 'the confirmation names the independently supplied target');
  await window.webContents.executeJavaScript(`(() => {
    const yes = [...document.querySelectorAll('.prompt button')].find(button => button.textContent.trim() === 'Yes, do it');
    if (!yes || yes.disabled) throw new Error('goal confirmation is unavailable');
    yes.click();
  })()`);
  const ready = await until(async () => {
    const projection = await window.webContents.executeJavaScript('window.desktop.snapshot()');
    return projection?.root && ['ready', 'waiting_plan', 'running'].includes(projection.root.state) ? projection : false;
  }, 'confirmed Root before Execute', 25000);
  proof.root_before_execute = ready.root;
  const readyLedger = await until(() => {
    try {
      const ledger = readLedger();
      return ledger.status === 'available' && ledger.root_tasks.some(item => item.root_id === ready.root.root_task_id && item.goal_json) ? ledger : false;
    } catch { return false; }
  }, 'durable confirmed Root goal', 10000);
  const rootTask = readyLedger.root_tasks.find(item => item.root_id === ready.root.root_task_id);
  const frozenGoal = JSON.parse(rootTask.goal_json);
  proof.frozen_goal = frozenGoal;
  assert.equal(frozenGoal.user_text, proof.operator_text);
  assert.equal(frozenGoal.scope.app, bundleId);
  assert.equal(frozenGoal.target.semantic_target, target.semantic_target);
  assert.equal(frozenGoal.target.label, target.label);
  assert.equal(frozenGoal.target.group ?? null, target.group ?? null);
  assert.equal(frozenGoal.target.role_hint ?? null, target.role_hint ?? null);
  assert.deepEqual(frozenGoal.target.row, target.row, 'the independent row conditions and public table scope are frozen unchanged');
  assert.deepEqual(frozenGoal.desired, { type: 'bool', value: true });
  assert.equal(frozenGoal.confirmation_scope, 'ui_revisit');
  proof.fixture_before_execute = fixtureSummary(readFixtureSnapshot());
  proof.no_row_action_before_execute = proof.fixture_before.selected_row_change_count === proof.fixture_before_execute.selected_row_change_count
    && proof.fixture_before.selected_row?.id === proof.fixture_before_execute.selected_row?.id;
  assert.equal(proof.no_row_action_before_execute, true, 'permission and goal confirmation submit no row input');
  proof.row_receipts_before_execute = readRowReceipts();
  assert.equal(proof.row_receipts_before_execute.records.length, 0, 'there is no row-submit receipt before Execute');
  const preInputSource = sourceHistoryForRoot(null);
  proof.pre_input_row_source = rowSourceEvidence(preInputSource, target, null, proof.authorized_window.window_id);
  proof.pre_input_source_sequence = Number.isSafeInteger(proof.pre_input_row_source.source_sequence)
    ? proof.pre_input_row_source.source_sequence : null;

  activateFixture();
  await window.webContents.executeJavaScript(`(() => {
    const select = document.querySelector('.mode select');
    if (!select || select.disabled) throw new Error('Run mode is unavailable');
    const executeOption = select.querySelector('option[value="execute"]');
    if (!executeOption || executeOption.disabled) throw new Error('Execute is disabled');
    select.value = 'execute'; select.dispatchEvent(new Event('change', { bubbles: true }));
  })()`);
  proof.execute_selected = true;
  await until(async () => (await window.webContents.executeJavaScript('window.desktop.presentation()')) === 'indicator', 'execution indicator', 10000);
  if (activationOnly) {
    const verified = await until(() => {
      const ledger = readLedger();
      if (ledger.status !== 'available') return false;
      const effect = ledger.effect_results.map(item => item.result).find(item => item.root_task_id === rootTask.root_id
        && item.predicate === 'window.active@1' && item.status === 'satisfied' && item.confirmation_scope === 'current_ui');
      const attempt = ledger.attempts.find(item => item.root_id === rootTask.root_id && item.skill === 'ui.activate_window'
        && item.attempt_id === effect?.attempt_id && item.state === 'settled' && item.activation_seq > 0);
      return effect && attempt ? { effect, attempt } : false;
    }, 'registered window activation and independent fresh Source effect', 25000);
    const receipts = await until(() => {
      const value = readNativeReceipts('window-activation-submit-receipt');
      return value.records.length ? value : false;
    }, 'matching native window-activation receipt', 3000);
    assert.equal(receipts.records.length, 1, 'one native window activation receipt is recorded');
    const source = await until(() => {
      captureSourceHistory();
      const item = [...sourceHistory.values()].find(item => verified.effect.evidence_group.includes(item.snapshot_id));
      return item ?? false;
    }, 'original post-input Source snapshot referenced by the persisted window effect', 4000);
    const snapshot = source.snapshot;
    const rootObject = snapshot.objects.find(object => object.parent === null
      && object.window_id === snapshot.window_context?.origin_window_id && object.role === 'AXWindow'
      && object.subrole === 'AXStandardWindow');
    assert.ok(rootObject, 'the post-input Source contains the same standard window root');
    const sourceProof = JSON.parse(execFileSync(path.join(root, 'target/debug/compile_device_context'), [], {
      input: JSON.stringify({ snapshot, target: rootObject.instance_ref,
        scope: { kind: 'window_root', table_scope: target.row.table_scope } }),
      encoding: 'utf8', timeout: 2000, maxBuffer: 64 * 1024,
    }));
    const { verifyScopedActivation } = await import(path.join(root, 'scripts/mvp/scoped-activation-evidence.mjs'));
    proof.window_activation = verifyScopedActivation({ receipt: receipts.records[0].value, rootTask,
      attempt: verified.attempt, effect: verified.effect, source: snapshot, sourceProof,
      target, sessionEpoch: proof.native_session_epoch });
    proof.window_activation_receipts = receipts;
    proof.window_activation_effect = verified.effect;
    proof.window_activation_attempt = verified.attempt;
    proof.window_activation_source_proof = sourceProof;
    proof.window_activation_source_snapshot_id = snapshot.snapshot_id;
    assert.equal(proof.window_activation.ok, true, proof.window_activation.reason);
    const currentRoot = readLedger().root_tasks?.find(item => item.root_id === rootTask.root_id);
    assert.ok(currentRoot, 'the activation phase retains its durable Root');
    proof.activation_root_state = currentRoot.state;
    proof.activation_root_goal_complete = currentRoot.state === 'completed';
    assert.equal(proof.activation_root_goal_complete, false, 'window activation does not complete the row goal');
    await requestStop('activation_only_diagnostic_complete');
    return;
  }
  const terminalProjection = await until(async () => {
    finalProjection = await window.webContents.executeJavaScript('window.desktop.snapshot()');
    return ['completed', 'failed', 'suspended', 'unverified', 'cancelled'].includes(finalProjection?.root?.state)
      ? finalProjection : false;
  }, 'row selection and independent revisit completion', 60000);
  proof.root_terminal_projection = terminalProjection;
  if (terminalProjection.root.state !== 'completed') throw new Error(`Root stopped in ${terminalProjection.root.state}`);
  proof.root_completed_projection = terminalProjection;
  proof.root_completed = true;
  proof.stop_shortcut_registered = globalShortcut.isRegistered('CommandOrControl+Shift+Escape');

  const ledger = readLedger();
  proof.ledger = ledger;
  assert.equal(ledger.status, 'available');
  const completedRoot = ledger.root_tasks.find(item => item.root_id === rootTask.root_id);
  assert.ok(completedRoot?.state === 'completed' && completedRoot.goal_json === rootTask.goal_json, 'the unchanged frozen Root completed');
  proof.row_attempts = ledger.attempts.filter(item => item.root_id === rootTask.root_id && item.skill === 'ui.select_row');
  proof.revisit_attempts = ledger.attempts.filter(item => item.root_id === rootTask.root_id && item.skill === 'ui.revisit');
  assert.equal(proof.row_attempts.length, 1, 'one row-selection Attempt was admitted');
  assert.equal(proof.revisit_attempts.length, 1, 'one revisit Attempt was admitted');
  proof.goal_effect = ledger.effect_results.map(item => item.result).filter(item => item.root_task_id === rootTask.root_id
    && item.predicate === 'goal.required@1' && item.status === 'satisfied' && item.confirmation_scope === 'ui_revisit')
    .sort((a, b) => b.revision - a.revision)[0] || null;
  assert.ok(proof.goal_effect, 'Root completion has a persisted satisfied revisit effect');
  const after = await until(() => { try { return readFixtureSnapshot(); } catch { return false; } }, 'independent after fixture snapshot', 5000);
  proof.fixture_after = fixtureSummary(after);
  const beforeCount = proof.fixture_before.selected_row_change_count;
  const afterCount = proof.fixture_after.selected_row_change_count;
  proof.fixture_selection = {
    selected_row: proof.fixture_after.selected_row,
    selection_changed_once: Number.isInteger(beforeCount) && Number.isInteger(afterCount) && afterCount === beforeCount + 1,
    selected_row_label_matches_target: proof.fixture_after.selected_row?.label === target.label,
    revisit_count_delta: Number.isInteger(proof.fixture_before.revisit_count) && Number.isInteger(proof.fixture_after.revisit_count)
      ? proof.fixture_after.revisit_count - proof.fixture_before.revisit_count : null,
  };
  proof.row_receipts = await until(() => {
    const receipts = readRowReceipts();
    return receipts.records.length > 0 ? receipts : false;
  }, 'bounded native row-submit receipt', 1800).catch(() => readRowReceipts());
  captureRowReceiptProof(ledger, finalProjection);
  const nativeSource = sourceHistoryForRoot(proof.goal_effect);
  proof.native_source = compactSource(nativeSource);
  proof.native_row_selection = rowSourceEvidence(nativeSource, target, proof.goal_effect, proof.authorized_window.window_id);
  const nativeReceipt = proof.native_row_setter_call_count.status === 'verified'
    ? proof.native_row_setter_call_count.receipt : null;
  const sourceSequenceAdvanced = Number.isSafeInteger(proof.pre_input_source_sequence)
    && Number.isSafeInteger(proof.native_row_selection.source_sequence)
    && proof.native_row_selection.source_sequence > proof.pre_input_source_sequence;
  const sourceObservedAfterSetter = Number.isSafeInteger(proof.native_row_selection.observed_ms)
    && Number.isSafeInteger(nativeReceipt?.returned_ms)
    && proof.native_row_selection.observed_ms > nativeReceipt.returned_ms;
  proof.native_row_selection.input_freshness = {
    source_sequence_advanced: sourceSequenceAdvanced,
    observed_ms_strictly_after_setter_return: sourceObservedAfterSetter,
    baseline_source_sequence: proof.pre_input_source_sequence,
    setter_returned_ms: nativeReceipt?.returned_ms ?? null,
  };
  if (proof.native_row_selection.status === 'verified' && (!sourceSequenceAdvanced || !sourceObservedAfterSetter)) {
    proof.native_row_selection.status = 'incomplete';
    proof.native_row_selection.reason = 'source evidence is not proven newer than both pre-input observation and setter return';
  }
  proof.row_action_has_no_click_or_axpress_fallback = proof.native_row_setter_call_count.status === 'verified'
    ? { status: 'verified', primitive: 'ax_set_selected_true' }
    : { status: 'unknown', reason: 'no correlated native primitive receipt' };
  proof.navigation_native_input_count = {
    status: 'unknown', count: null,
    reason: 'revisit_count and quota_actions show navigation outcome and allowance, not exact AXPress receipts',
    revisit_attempt_quota_actions: proof.revisit_attempts[0]?.quota_actions ?? null,
    fixture_revisit_count_delta: proof.fixture_selection.revisit_count_delta,
  };
  proof.stop_before_row_setter = { status: 'not_exercised', setter_count: 'unknown' };
  proof.fixture_stderr = fixtureStderr;
  proof.ok = proof.native_row_setter_call_count.status === 'verified'
    && proof.native_row_selection.status === 'verified'
    && proof.fixture_selection.selection_changed_once === true
    && proof.fixture_selection.selected_row_label_matches_target === true
    && proof.fixture_selection.revisit_count_delta === 1;
}

process.on('uncaughtException', error => { void finish(error); });
process.on('unhandledRejection', error => { void finish(error); });
process.once('SIGINT', () => { void finish(new Error('SIGINT: Stop requested')); });
process.once('SIGTERM', () => { void finish(new Error('SIGTERM: Stop requested')); });
setTimeout(() => { void finish(new Error('owned row smoke timed out')); }, 105000).unref();
void run().then(() => finish(null)).catch(error => finish(error));
