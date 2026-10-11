#!/usr/bin/env node
// Read-only AX attribute status and AXObserver registration coverage for one owned TextEdit temp document.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const runId = randomUUID();
const output = mkdtempSync(join(tmpdir(), 'cu-owned-textedit-coverage-'));
const artifact = join(root, 'artifacts/m2-textedit-observer-attribute-coverage.json');
const textEditApp = '/System/Applications/TextEdit.app';
const textEditPath = join(textEditApp, 'Contents/MacOS/TextEdit');
const infoPlist = join(textEditApp, 'Contents/Info.plist');
const readinessMaxWaitMs = 8_000;
const readinessPerProbeTimeoutCapMs = 650;
const readinessCleanupReserveMs = 300;
const readinessPollIntervalMs = 150;
const launcherSource = join(root, 'native/device/fixtures/OwnedTextEditLauncher.swift');
const probeSource = join(root, 'native/device/fixtures/OwnedTextEditCoverageProbe.swift');
const launcherBinary = join(output, 'OwnedTextEditLauncher');
const probeBinary = join(output, 'OwnedTextEditCoverageProbe');
const tempDocument = join(output, 'owned-unicode-fixture.txt');
const fixtureText = 'Read-only coverage fixture\nEnglish: observer coverage\n中文：覆盖检查\n';
const modes = ['--build-only', '--protocol-check', '--run'];
const selectedModes = modes.filter(mode => process.argv.includes(mode));
assert.ok(selectedModes.length <= 1, `choose one mode: ${modes.join(', ')}`);
const mode = selectedModes[0] ?? '--build-only';
const evidence = {
  ok: false,
  kind: 'textedit_owned_observer_attribute_coverage_artifact',
  run_id: runId,
  mode: mode.slice(2),
  started_at: new Date().toISOString(),
  output,
  artifact,
  target_bundle_id: 'com.apple.TextEdit',
  temp_document: null,
  launch_receipt: null,
  activation_observation: null,
  probe_result: null,
  ax_set_value_calls: 0,
  ax_press_calls: 0,
  set_focus_calls: 0,
  cg_post_event_calls: 0,
  input_posted: false,
  no_input_authority: true,
  source_or_public_authority_mutated: false,
  mvp_complete: false,
  type_text_or_saved_mvp_claimed: false
};

mkdirSync(dirname(artifact), { recursive: true });
if (existsSync(artifact)) {
  const previous = artifact.replace('.json', `.previous-${runId}.json`);
  copyFileSync(artifact, previous);
  evidence.previous_artifact = previous;
}
const save = () => writeFileSync(artifact, `${JSON.stringify(evidence, null, 2)}\n`);
const sha256 = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const pause = milliseconds => new Promise(resolvePause => setTimeout(resolvePause, milliseconds));
async function waitBounded(promise, milliseconds) {
  let timer;
  const result = await Promise.race([
    promise,
    new Promise(resolveTimeout => { timer = setTimeout(() => resolveTimeout(null), milliseconds); })
  ]);
  clearTimeout(timer);
  return result;
}

let launcher = null;
let launcherClose = null;
let launcherExit = null;
let launcherStderr = '';
let launcherRecords = [];
let launcherWaiters = [];

function compile() {
  mkdirSync(join(output, 'module-cache'), { recursive: true });
  const flags = [
    '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(output, 'module-cache')
  ];
  try {
    const launcherStdout = execFileSync('xcrun', ['swiftc', ...flags, launcherSource, '-o', launcherBinary], {
      encoding: 'utf8', timeout: 90_000, maxBuffer: 512 * 1024
    });
    const probeStdout = execFileSync('xcrun', ['swiftc', ...flags, probeSource, '-o', probeBinary], {
      encoding: 'utf8', timeout: 90_000, maxBuffer: 512 * 1024
    });
    evidence.swift_compile = { ok: true, launcher_stdout: launcherStdout, probe_stdout: probeStdout };
  } catch (error) {
    evidence.swift_compile = {
      ok: false, status: error.status ?? null, signal: error.signal ?? null,
      stdout: String(error.stdout ?? ''), stderr: String(error.stderr ?? error.message ?? '').slice(0, 128 * 1024)
    };
    throw error;
  }
  evidence.launcher_binary_sha256 = sha256(launcherBinary);
  evidence.launcher_source_sha256 = sha256(launcherSource);
  evidence.probe_binary_sha256 = sha256(probeBinary);
  evidence.probe_source_sha256 = sha256(probeSource);
  evidence.script_source_sha256 = sha256(fileURLToPath(import.meta.url));
}

function acceptLauncherLine(line) {
  let record;
  try { record = JSON.parse(line); }
  catch (error) { evidence.launcher_json_error = String(error); return; }
  const index = launcherWaiters.findIndex(waiter => waiter.predicate(record));
  if (index >= 0) {
    const [waiter] = launcherWaiters.splice(index, 1);
    clearTimeout(waiter.timer);
    waiter.resolve(record);
  } else launcherRecords.push(record);
}

function waitForLauncherRecord(predicate, label, timeoutMs = 12_000) {
  const index = launcherRecords.findIndex(predicate);
  if (index >= 0) return Promise.resolve(launcherRecords.splice(index, 1)[0]);
  if (launcherExit) return Promise.reject(new Error(`launcher exited before ${label}`));
  return new Promise((resolvePromise, rejectPromise) => {
    const waiter = { predicate, resolve: resolvePromise, reject: rejectPromise, timer: null };
    waiter.timer = setTimeout(() => {
      launcherWaiters = launcherWaiters.filter(item => item !== waiter);
      rejectPromise(new Error(`timed out waiting for launcher ${label}`));
    }, timeoutMs);
    launcherWaiters.push(waiter);
  });
}

async function startLauncher() {
  launcher = spawn(launcherBinary, [], { stdio: ['pipe', 'pipe', 'pipe'] });
  launcherClose = new Promise(resolveClose => launcher.once('close', (code, signal) => {
    launcherExit = { code, signal };
    for (const waiter of launcherWaiters.splice(0)) {
      clearTimeout(waiter.timer);
      waiter.reject(new Error(`launcher exited (${code ?? signal})`));
    }
    resolveClose(launcherExit);
  }));
  let pending = '';
  let stdoutBytes = 0;
  launcher.stdout.on('data', chunk => {
    stdoutBytes += chunk.length;
    if (stdoutBytes > 128 * 1024) { evidence.launcher_output_budget_exceeded = true; return; }
    pending += chunk.toString('utf8');
    for (let newline; (newline = pending.indexOf('\n')) >= 0;) {
      const line = pending.slice(0, newline);
      pending = pending.slice(newline + 1);
      if (line) acceptLauncherLine(line);
    }
  });
  launcher.stderr.on('data', chunk => {
    if (launcherStderr.length + chunk.length <= 32 * 1024) launcherStderr += chunk.toString('utf8');
  });
  launcher.on('error', error => { evidence.launcher_spawn_error = String(error); });
  const ready = await waitForLauncherRecord(record => record.kind === 'ready' || record.kind === 'launcher_error', 'ready');
  assert.equal(ready.kind, 'ready', ready.error ?? 'owned launcher failed');
  const identity = ready.launcher_identity;
  assert.equal(identity?.pid, launcher.pid, 'launcher ready PID must be this run-owned child');
  assert.equal(identity?.parent_pid, process.pid, 'launcher must be a direct child of this Node process');
  assert.equal(realpathSync(identity.executable_path), realpathSync(launcherBinary), 'launcher executable mismatch');
  evidence.owned_launcher_identity = identity;
  evidence.owned_launcher_pid = identity.pid;
  evidence.launcher_id = ready.launcher_id;
}

function createOwnedDocument() {
  writeFileSync(tempDocument, fixtureText, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
  const stat = statSync(tempDocument);
  const docHash = sha256(tempDocument);
  evidence.temp_document = {
    path: tempDocument, basename: 'owned-unicode-fixture.txt', bytes: Buffer.byteLength(fixtureText, 'utf8'),
    sha256: docHash, actual_mode_octal: (stat.mode & 0o777).toString(8).padStart(3, '0'),
    owner_uid: stat.uid, run_owned: true
  };
  assert.equal(evidence.temp_document.actual_mode_octal, '600', 'owned temp document must be mode 0600');
  assert.equal(stat.uid, process.getuid(), 'owned temp document owner must match current uid');
  return docHash;
}

async function requestLaunch(documentHash) {
  const pending = waitForLauncherRecord(record => record.kind === 'launch_receipt' || record.kind === 'launch_error', 'launch receipt');
  launcher.stdin.write(`${JSON.stringify({
    op: 'launch', run_id: runId, document_path: tempDocument, document_sha256: documentHash
  })}\n`);
  const response = await pending;
  assert.equal(response.kind, 'launch_receipt', response.error ?? 'owned TextEdit instance launch failed');
  const receipt = response.receipt;
  assert.equal(response.launcher_id, evidence.launcher_id);
  assert.equal(receipt.launcher_id, evidence.launcher_id);
  assert.equal(receipt.run_id, runId);
  assert.equal(receipt.bundle_identifier, 'com.apple.TextEdit');
  assert.equal(realpathSync(receipt.executable_path), realpathSync(textEditPath));
  assert.equal(receipt.document_sha256, documentHash);
  assert.equal(receipt.new_pid_excluded_from_preexisting_set, true);
  assert.equal(receipt.target_started_within_launch_interval, true);
  assert.equal(receipt.creates_new_application_instance, true);
  assert.equal(receipt.adds_to_recent_items, false);
  assert.equal(receipt.activates_new_instance, false);
  assert.deepEqual(receipt.arguments, ['-ApplePersistenceIgnoreState', 'YES']);
  assert.equal(receipt.no_input_authority, true);
  assert.equal(receipt.preexisting_textedit_pids.includes(receipt.target_identity.pid), false);
  evidence.launch_receipt = receipt;
  evidence.textedit_pid = receipt.target_identity.pid;
  evidence.preexisting_textedit_pids = receipt.preexisting_textedit_pids;
  return receipt;
}

function coverageRequest(receipt, activationObservation = undefined) {
  return {
    target_pid: receipt.target_identity.pid,
    probe_parent_pid: process.pid,
    launcher_pid: receipt.launcher_identity.pid,
    launcher_id: evidence.launcher_id,
    executable_path: realpathSync(textEditPath),
    bundle_id: 'com.apple.TextEdit',
    document_path: tempDocument,
    document_sha256: receipt.document_sha256,
    run_id: runId,
    launch_receipt: receipt,
    ...(activationObservation ? { activation_observation: activationObservation } : {})
  };
}

async function runHelper(args, request, label, timeoutMs = 10_000, details = null) {
  const child = spawn(probeBinary, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '';
  let stderr = '';
  let overflow = false;
  child.stdout.on('data', chunk => {
    if (stdout.length + chunk.length > 256 * 1024) { overflow = true; child.kill('SIGTERM'); return; }
    stdout += chunk.toString('utf8');
  });
  child.stderr.on('data', chunk => {
    if (stderr.length + chunk.length <= 32 * 1024) stderr += chunk.toString('utf8');
  });
  const closed = new Promise(resolveClose => child.once('close', (code, signal) => resolveClose({ code, signal })));
  child.stdin.end(`${JSON.stringify(request)}\n`);
  const result = await waitBounded(closed, timeoutMs);
  if (!result) {
    child.kill('SIGTERM');
    let cleanupResult = await waitBounded(closed, 100);
    if (!cleanupResult) {
      child.kill('SIGKILL');
      cleanupResult = await waitBounded(closed, 150);
    }
    if (details) Object.assign(details, {
      exit: cleanupResult, timed_out: true,
      stdout, stderr, overflow
    });
    throw new Error(`${label} exceeded ${timeoutMs}ms`);
  }
  if (details) Object.assign(details, { exit: result, timed_out: false, stdout, stderr, overflow });
  assert.equal(result.code, 0, `${label} exited ${result.code ?? result.signal}: ${stderr}`);
  assert.equal(overflow, false, `${label} exceeded output budget`);
  const lines = stdout.split(/\r?\n/u).filter(Boolean);
  assert.equal(lines.length, 1, `${label} must emit exactly one JSON line`);
  return JSON.parse(lines[0]);
}

function kernelIdentityMatches(actual, expected) {
  return actual?.pid === expected?.pid
    && actual?.parent_pid === expected?.parent_pid
    && actual?.start_seconds === expected?.start_seconds
    && actual?.start_microseconds === expected?.start_microseconds
    && actual?.executable_path && expected?.executable_path
    && realpathSync(actual.executable_path) === realpathSync(expected.executable_path)
    && (actual.parent_executable_path == null || expected.parent_executable_path == null
      ? actual.parent_executable_path === expected.parent_executable_path
      : realpathSync(actual.parent_executable_path) === realpathSync(expected.parent_executable_path));
}

function ownedTempDocumentMatches(receipt) {
  try {
    const stat = statSync(tempDocument);
    return realpathSync(tempDocument) === realpathSync(receipt.document_path)
      && sha256(tempDocument) === receipt.document_sha256
      && (stat.mode & 0o777) === 0o600 && stat.uid === process.getuid();
  } catch { return false; }
}

function ownedWindowReadinessVerified(receipt, readiness) {
  const expectedTitle = 'owned-unicode-fixture.txt';
  return readiness?.kind === 'textedit_coverage_window_readiness'
    && readiness.run_id === receipt.run_id
    && readiness.target_pid === receipt.target_identity.pid
    && readiness.process_identity_revalidated === true
    && kernelIdentityMatches(readiness.process_identity_before, receipt.target_identity)
    && ownedTempDocumentMatches(receipt)
    && readiness.window_verified === true
    && readiness.cg_window_count === 1 && readiness.ax_window_count === 1
    && readiness.frames_match === true && Number.isInteger(readiness.cg_window_id)
    && readiness.cg_window_id > 0
    && readiness.cg_title_matches_owned_document === true
    && readiness.cg_title?.includes(expectedTitle)
    && readiness.ax_windows?.length === 1
    && readiness.ax_windows[0]?.window_pid === receipt.target_identity.pid
    && readiness.ax_windows[0]?.title_matches_owned_document === true
    && readiness.ax_title?.includes(expectedTitle)
    && !receipt.preexisting_textedit_pids.includes(receipt.target_identity.pid);
}

function readinessHelperTimeoutMs(remainingMs) {
  return Math.floor(Math.min(readinessPerProbeTimeoutCapMs, remainingMs - readinessCleanupReserveMs));
}

async function pollOwnedWindowReadiness(receipt, request) {
  const started = performance.now();
  const deadline = started + readinessMaxWaitMs;
  const attempts = [];
  evidence.window_readiness_attempts = attempts;
  evidence.readiness_poll = {
    max_wait_ms: readinessMaxWaitMs, attempts: 0, verified: false,
    started_at: new Date().toISOString(), target_pid: receipt.target_identity.pid,
    run_id: receipt.run_id, document_path: receipt.document_path,
    document_sha256: receipt.document_sha256, deadline_monotonic_ms: deadline
  };

  let stopReason = 'readiness_deadline_elapsed';
  while (performance.now() < deadline) {
    const remainingMs = deadline - performance.now();
    const helperTimeoutMs = readinessHelperTimeoutMs(remainingMs);
    if (helperTimeoutMs < 1) { stopReason = 'insufficient_time_for_bounded_helper_cleanup'; break; }

    const attemptStarted = performance.now();
    const details = {};
    let result = null;
    let error = null;
    try {
      result = await runHelper(['--window-ready-only'], request,
        `owned TextEdit readiness attempt ${attempts.length + 1}`, helperTimeoutMs, details);
    } catch (cause) { error = String(cause?.stack ?? cause); }

    const verified = performance.now() < deadline
      && result != null && ownedWindowReadinessVerified(receipt, result);
    const attempt = {
      attempt: attempts.length + 1,
      elapsed_ms: Math.round(performance.now() - started),
      attempt_elapsed_ms: Math.round(performance.now() - attemptStarted),
      helper_timeout_ms: helperTimeoutMs,
      request_run_id: receipt.run_id,
      request_target_pid: receipt.target_identity.pid,
      request_document_path: receipt.document_path,
      request_document_sha256: receipt.document_sha256,
      raw_result: result,
      helper_stdout: details.stdout?.slice(0, 8 * 1024) ?? '',
      helper_stderr: details.stderr?.slice(0, 8 * 1024) ?? '',
      helper_exit: details.exit ?? null,
      helper_timed_out: details.timed_out ?? false,
      helper_output_overflow: details.overflow ?? false,
      document_identity_revalidated: ownedTempDocumentMatches(receipt),
      process_identity_matches_receipt: result
        ? kernelIdentityMatches(result.process_identity_before, receipt.target_identity) : false,
      readiness_verified: verified,
      error: error?.slice(0, 2 * 1024) ?? null
    };
    attempts.push(attempt);
    if (result) evidence.window_readiness = result;
    evidence.readiness_poll.attempts = attempts.length;
    evidence.readiness_poll.elapsed_ms = Math.round(performance.now() - started);
    evidence.readiness_poll.deadline_reached = performance.now() >= deadline;
    evidence.readiness_poll.verified = verified;
    save();

    if (verified && performance.now() < deadline) return result;
    if (verified) {
      stopReason = 'readiness_verified_after_poll_deadline';
      evidence.readiness_poll.verified = false;
      evidence.readiness_poll.deadline_reached = true;
      evidence.readiness_poll.stop_reason = stopReason;
      save();
      break;
    }
    if (result && result.process_identity_revalidated === false) {
      stopReason = 'target_process_identity_no_longer_matches_receipt';
      break;
    }
    const left = deadline - performance.now();
    if (left <= 0) break;
    await pause(Math.min(readinessPollIntervalMs, left));
  }

  evidence.readiness_poll.elapsed_ms = Math.round(performance.now() - started);
  evidence.readiness_poll.deadline_reached = performance.now() >= deadline;
  evidence.readiness_poll.verified = false;
  evidence.readiness_poll.stop_reason = stopReason;
  evidence.readiness_poll.activation_block_reason = 'readiness predicate or wait deadline did not permit activation';
  save();
  return evidence.window_readiness ?? null;
}

async function activateOwned(receipt, readiness) {
  const poll = evidence.readiness_poll;
  const stillWithinWaitBudget = poll?.verified === true
    && Number.isFinite(poll.deadline_monotonic_ms) && performance.now() < poll.deadline_monotonic_ms;
  const verified = stillWithinWaitBudget && ownedWindowReadinessVerified(receipt, readiness);
  let response = null;
  if (verified) {
    const pending = waitForLauncherRecord(record => record.kind === 'activation_result', 'owned app activation', 5000);
    launcher.stdin.write(`${JSON.stringify({
      op: 'activate_owned', run_id: receipt.run_id,
      target_pid: receipt.target_identity.pid,
      target_start_seconds: receipt.target_identity.start_seconds,
      target_start_microseconds: receipt.target_identity.start_microseconds,
      cg_window_count: readiness.cg_window_count,
      ax_window_count: readiness.ax_window_count,
      cg_window_id: readiness.cg_window_id,
      cg_title: readiness.cg_title,
      ax_title: readiness.ax_title,
      frames_match: readiness.frames_match,
      system_focused_application_pid_before_activation: readiness.system_focused_application_pid_at_window_check ?? null
    })}\n`);
    response = await pending;
    assert.equal(response.launcher_id, evidence.launcher_id);
  }
  const observation = {
    attempted: response?.attempted === true,
    attempt_count: Number.isInteger(response?.attempt_count) ? response.attempt_count : 0,
    return_value: typeof response?.return_value === 'boolean' ? response.return_value : null,
    status: response?.status ?? (verified ? 'activation_result_unavailable'
      : poll?.verified === true ? 'readiness_wait_budget_expired_before_activation' : 'window_readiness_not_verified'),
    window_verified: verified,
    cg_window_count: readiness?.cg_window_count ?? 0,
    ax_window_count: readiness?.ax_window_count ?? 0,
    cg_window_id: readiness?.cg_window_id ?? null,
    frames_match: readiness?.frames_match === true,
    system_focused_application_pid_before_activation: verified
      ? readiness?.system_focused_application_pid_at_window_check ?? null : null
  };
  evidence.activation_result = response;
  evidence.activation_observation = observation;
  return observation;
}

function sampleProtocolRequest(documentHash) {
  const fakeTargetPID = 12345;
  const fakeLauncherPID = 23456;
  const launcherIdentity = {
    pid: fakeLauncherPID, parent_pid: process.pid, start_seconds: 1791000000, start_microseconds: 123456,
    executable_path: launcherBinary, parent_executable_path: process.execPath
  };
  const targetIdentity = {
    pid: fakeTargetPID, parent_pid: 1, start_seconds: 1791000001, start_microseconds: 654321,
    executable_path: textEditPath, parent_executable_path: '/sbin/launchd'
  };
  const receipt = {
    version: 1, run_id: `protocol-${runId}`, launcher_id: 'protocol-launcher',
    launcher_identity: launcherIdentity, target_identity: targetIdentity,
    bundle_identifier: 'com.apple.TextEdit', executable_path: textEditPath,
    document_path: tempDocument, document_sha256: documentHash,
    preexisting_textedit_pids: [54321], new_pid_excluded_from_preexisting_set: true,
    target_started_within_launch_interval: true, creates_new_application_instance: true,
    adds_to_recent_items: false, activates_new_instance: false,
    arguments: ['-ApplePersistenceIgnoreState', 'YES'], no_input_authority: true
  };
  return {
    target_pid: fakeTargetPID, probe_parent_pid: process.pid, launcher_pid: fakeLauncherPID,
    launcher_id: receipt.launcher_id, executable_path: textEditPath, bundle_id: 'com.apple.TextEdit',
    document_path: tempDocument, document_sha256: documentHash, run_id: receipt.run_id,
    launch_receipt: receipt
  };
}

async function protocolCheck() {
  const documentHash = createOwnedDocument();
  const request = sampleProtocolRequest(documentHash);
  const result = await runHelper(['--protocol-check'], request, 'private receipt and status-classification protocol check');
  const receipt = request.launch_receipt;
  const readinessFixture = {
    kind: 'textedit_coverage_window_readiness', run_id: receipt.run_id,
    target_pid: receipt.target_identity.pid, process_identity_revalidated: true,
    process_identity_before: receipt.target_identity, window_verified: true,
    cg_window_count: 1, ax_window_count: 1, frames_match: true, cg_window_id: 77,
    cg_title_matches_owned_document: true, cg_title: 'owned-unicode-fixture.txt — TextEdit',
    ax_title: 'owned-unicode-fixture.txt',
    ax_windows: [{ window_pid: receipt.target_identity.pid, title_matches_owned_document: true }]
  };
  const readinessPredicate = ownedWindowReadinessVerified(receipt, readinessFixture);
  const readinessRejectsWrongScope = !ownedWindowReadinessVerified(receipt, {
    ...readinessFixture, run_id: `${receipt.run_id}-other`
  }) && !ownedWindowReadinessVerified(receipt, {
    ...readinessFixture, ax_window_count: 2
  }) && !ownedWindowReadinessVerified(receipt, {
    ...readinessFixture,
    process_identity_before: { ...receipt.target_identity, start_microseconds: receipt.target_identity.start_microseconds + 1 }
  });
  const readinessTimeoutBounds = readinessHelperTimeoutMs(8_000) === readinessPerProbeTimeoutCapMs
    && readinessHelperTimeoutMs(301) === 1 && readinessHelperTimeoutMs(300) === 0
    && readinessMaxWaitMs === 8_000;
  assert.equal(result.kind, 'protocol_check');
  assert.equal(result.decoded, true);
  assert.equal(result.receipt_fields_match, true);
  assert.equal(result.unsupported_remains_non_success, true);
  assert.equal(result.timeout_budget_and_late_success_cases_passed, true);
  assert.equal(result.timeout_setup_failure_or_late_return_blocks_query, true);
  assert.equal(result.optional_children_no_value_leaf_cases_passed, true);
  assert.equal(result.children_no_value_is_not_relabelled_ax_success, true);
  assert.equal(result.children_array_shape_cases_passed, true);
  assert.equal(result.array_shape_errors_do_not_read_as_success, true);
  assert.equal(result.passed, true);
  assert.equal(readinessPredicate, true);
  assert.equal(readinessRejectsWrongScope, true);
  assert.equal(readinessTimeoutBounds, true);
  evidence.protocol_check = result;
  evidence.readiness_protocol_check = {
    synthetic_predicate_only: true, owned_window_predicate: readinessPredicate,
    wrong_run_or_window_or_process_start_rejected: readinessRejectsWrongScope,
    timeout_bounds_passed: readinessTimeoutBounds
  };
  evidence.ok = true;
}

async function runOwnedProbe() {
  const plistBundle = execFileSync('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleIdentifier', infoPlist], {
    encoding: 'utf8', timeout: 5000
  }).trim();
  assert.equal(plistBundle, 'com.apple.TextEdit');
  assert.equal(existsSync(textEditPath), true, 'TextEdit executable not at fixed path');
  evidence.textedit_executable_sha256 = sha256(textEditPath);
  const documentHash = createOwnedDocument();
  await startLauncher();
  const receipt = await requestLaunch(documentHash);
  const request = coverageRequest(receipt);
  const readiness = await pollOwnedWindowReadiness(receipt, request);
  const activation = await activateOwned(receipt, readiness);
  if (!ownedWindowReadinessVerified(receipt, readiness) || activation.window_verified !== true) {
    evidence.probe_result = {
      kind: 'textedit_owned_observer_attribute_coverage',
      status: activation.status === 'readiness_wait_budget_expired_before_activation'
        ? 'readiness_budget_expired_before_activation' : 'readiness_not_verified',
      coverage_attempted: false,
      activation_observation: activation,
      last_readiness_result: readiness,
      readiness_attempt_count: evidence.readiness_poll?.attempts ?? 0
    };
    evidence.ok = false;
    return;
  }
  await pause(300);
  evidence.probe_result = await runHelper(
    ['--coverage'], coverageRequest(receipt, activation), 'owned TextEdit AX/observer coverage probe', 12_000
  );
  evidence.ok = evidence.probe_result.kind === 'textedit_owned_observer_attribute_coverage'
    && evidence.probe_result.scope_verified === true
    && evidence.probe_result.scope_unchanged_before_after === true
    && evidence.probe_result.process_identity_unchanged === true
    && evidence.probe_result.launcher_identity_unchanged === true
    && evidence.probe_result.deadline_exceeded === false;
}

async function closeOwnedTextEdit() {
  if (!launcher) return;
  if (launcherExit) {
    evidence.owned_launcher_exit = { ...launcherExit, exited_before_close_request: true };
    return;
  }
  evidence.close_attempt = 'quit sent to private launcher; it revalidates only the receipt-bound PID/start/path before terminating its returned instance';
  const pending = waitForLauncherRecord(record => record.kind === 'quit_result' || record.kind === 'stdin_closed', 'owned TextEdit close result');
  launcher.stdin.end(`${JSON.stringify({ op: 'quit' })}\n`);
  try {
    const response = await pending;
    evidence.textedit_close = response.close;
  } catch (error) { evidence.close_error = String(error); }
  const closed = await waitBounded(launcherClose, 2500);
  evidence.owned_launcher_exit = closed ?? { wait_timed_out: true };
  if (launcherStderr) evidence.launcher_stderr = launcherStderr;
}

save();
try {
  compile();
  if (mode === '--protocol-check') await protocolCheck();
  else if (mode === '--run') await runOwnedProbe();
  else {
    evidence.ok = true;
    evidence.note = 'Build-only is the default. --run is reserved for the parent-owned serial GUI pass.';
  }
} catch (error) {
  evidence.error = String(error?.stack ?? error);
  process.exitCode = 1;
} finally {
  await closeOwnedTextEdit();
  evidence.finished_at = new Date().toISOString();
  save();
  process.stdout.write(`${JSON.stringify({
    ok: evidence.ok, mode: evidence.mode, artifact, output,
    probe_source_sha256: evidence.probe_source_sha256 ?? null,
    launcher_source_sha256: evidence.launcher_source_sha256 ?? null,
    error: evidence.error ?? null
  })}\n`);
}
