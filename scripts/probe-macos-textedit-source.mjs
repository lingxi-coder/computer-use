#!/usr/bin/env node
// Read the owned temporary TextEdit document through the product cu-macos-source watch path.
// This probe never edits AX values, focuses controls, or posts events.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import {
  appendFileSync, copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync,
  readFileSync, readdirSync, realpathSync, statSync, writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { StringDecoder } from 'node:string_decoder';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const runId = randomUUID();
const output = mkdtempSync(join(tmpdir(), 'cu-textedit-source-readonly-'));
const artifact = join(root, 'artifacts/m2-textedit-product-source-probe.json');
const sourceBinary = join(root, 'target/debug/cu-macos-source');
const textEditPath = '/System/Applications/TextEdit.app/Contents/MacOS/TextEdit';
const infoPlist = '/System/Applications/TextEdit.app/Contents/Info.plist';
const launcherSource = join(root, 'native/device/fixtures/OwnedTextEditLauncher.swift');
const probeSource = join(root, 'native/device/fixtures/UnicodeReceiverProbe.swift');
const launcherBinary = join(output, 'OwnedTextEditLauncher');
const probeBinary = join(output, 'UnicodeReceiverProbe');
const tempDocument = join(output, 'owned-unicode-fixture.txt');
const sourceAssets = join(output, 'source-assets');
const sourceStdoutPath = join(output, 'source-watch.stdout.ndjson');
const sourceStderrPath = join(output, 'source-watch.stderr.txt');
const expectedText = 'Unicode receiver probe\nEnglish: hello from Computer Use\n中文：你好，世界\n';
const runMode = process.argv.includes('--run');
const buildOnly = process.argv.includes('--build-only');
assert([runMode, buildOnly].filter(Boolean).length <= 1, 'choose one mode: --run or --build-only');

const evidence = {
  ok: false,
  run_id: runId,
  started_at: new Date().toISOString(),
  mode: runMode ? 'owned_textedit_product_source_watch' : 'strict_build_only',
  output,
  source_binary: sourceBinary,
  source_assets_directory: sourceAssets,
  source_stdout_path: sourceStdoutPath,
  source_stderr_path: sourceStderrPath,
  temp_document: null,
  textedit_pid: null,
  launch_receipt: null,
  window_readiness: null,
  activation: null,
  receiver_probe: null,
  source_watch_request: null,
  source_process: null,
  source_records: [],
  source_observations: [],
  source_observation_failures: [],
  source_observation_first_failure: null,
  source_scope_check: null,
  source_text_controls: [],
  source_asset_files: [],
  source_input_authority: false,
  input_posted: false,
  ax_set_value_calls: 0,
  ax_press_calls: 0,
  set_focus_calls: 0,
  cg_post_event_calls: 0,
  textedit_delivery_or_saved_mvp_claimed: false,
  mvp_complete: false,
  first_failure: null,
};

mkdirSync(dirname(artifact), { recursive: true });
if (existsSync(artifact)) {
  const previous = artifact.replace('.json', `.previous-${runId}.json`);
  copyFileSync(artifact, previous);
  evidence.previous_artifact = previous;
}
const save = () => writeFileSync(artifact, JSON.stringify(evidence, null, 2) + '\n', { mode: 0o600 });
const sha256 = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const pause = ms => new Promise(resolvePause => setTimeout(resolvePause, ms));
const failOnce = (stage, error) => {
  evidence.first_failure ??= { stage, error: String(error?.stack ?? error).slice(0, 12 * 1024) };
  evidence.last_failure = { stage, error: String(error?.message ?? error).slice(0, 4096) };
  save();
};
const samePath = (a, b) => realpathSync(a) === realpathSync(b);

let stage = 'initialization';
let ownedLauncher = null;
let launcherReady = null;
let launcherRecords = [];
let launcherWaiters = [];
let launcherExit = null;
let launcherClose = null;
let launcherStderr = '';
let textEditReceipt = null;
let sourceChild = null;
let sourceClose = null;
let sourceStopReason = null;
let sourceOutputBytes = 0;
let sourceOutputOverflow = false;
let sourcePending = '';
let sourceDecoder = new StringDecoder('utf8');
let targetWindowID = null;
let targetObservationCount = 0;
let targetCompleteObservationCount = 0;
const sourceRawLimitBytes = 16 * 1024 * 1024;

function compilePrivateHelpers() {
  const moduleCache = join(output, 'module-cache');
  mkdirSync(moduleCache, { recursive: true, mode: 0o700 });
  const flags = [
    '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', moduleCache,
  ];
  try {
    const launcherStdout = execFileSync('xcrun', ['swiftc', ...flags, launcherSource, '-o', launcherBinary], {
      encoding: 'utf8', timeout: 90_000, maxBuffer: 512 * 1024,
    });
    const probeStdout = execFileSync('xcrun', ['swiftc', ...flags, probeSource, '-o', probeBinary], {
      encoding: 'utf8', timeout: 90_000, maxBuffer: 512 * 1024,
    });
    evidence.private_helper_build = { ok: true, launcher_stdout: launcherStdout, probe_stdout: probeStdout };
  } catch (error) {
    evidence.private_helper_build = {
      ok: false, status: error.status ?? null, signal: error.signal ?? null,
      stdout: String(error.stdout ?? ''), stderr: String(error.stderr ?? error.message ?? '').slice(0, 128 * 1024),
    };
    throw error;
  }
  evidence.launcher_binary_sha256 = sha256(launcherBinary);
  evidence.launcher_source_sha256 = sha256(launcherSource);
  evidence.probe_binary_sha256 = sha256(probeBinary);
  evidence.probe_source_sha256 = sha256(probeSource);
  evidence.script_source_sha256 = sha256(fileURLToPath(import.meta.url));
  assert.ok(existsSync(sourceBinary), 'current product target/debug/cu-macos-source is missing');
  evidence.source_binary_realpath = realpathSync(sourceBinary);
  evidence.source_binary_sha256 = sha256(sourceBinary);
  evidence.source_binary_stat = {
    size_bytes: statSync(sourceBinary).size,
    mode_octal: (statSync(sourceBinary).mode & 0o777).toString(8).padStart(3, '0'),
  };
}

function acceptLauncherLine(line) {
  let record;
  try { record = JSON.parse(line); }
  catch (error) { evidence.launcher_json_error = String(error); save(); return; }
  for (let index = 0; index < launcherWaiters.length; index += 1) {
    const waiter = launcherWaiters[index];
    if (!waiter.predicate(record)) continue;
    launcherWaiters.splice(index, 1);
    clearTimeout(waiter.timer);
    waiter.resolve(record);
    return;
  }
  launcherRecords.push(record);
  if (launcherRecords.length > 64) launcherRecords.shift();
}

function waitForLauncherRecord(predicate, label, timeoutMs = 15_000) {
  const index = launcherRecords.findIndex(predicate);
  if (index >= 0) return Promise.resolve(launcherRecords.splice(index, 1)[0]);
  if (launcherExit) return Promise.reject(new Error(`owned launcher exited before ${label}`));
  return new Promise((resolvePromise, rejectPromise) => {
    const waiter = { predicate, resolve: resolvePromise, reject: rejectPromise, timer: null };
    waiter.timer = setTimeout(() => {
      launcherWaiters = launcherWaiters.filter(item => item !== waiter);
      rejectPromise(new Error(`timed out waiting for owned launcher ${label}`));
    }, timeoutMs);
    launcherWaiters.push(waiter);
  });
}

async function startOwnedLauncher() {
  ownedLauncher = spawn(launcherBinary, [], { stdio: ['pipe', 'pipe', 'pipe'] });
  launcherClose = new Promise(resolveClose => ownedLauncher.once('close', (code, signal) => {
    launcherExit = { code, signal };
    for (const waiter of launcherWaiters.splice(0)) {
      clearTimeout(waiter.timer);
      waiter.reject(new Error(`owned launcher exited (${code ?? signal})`));
    }
    resolveClose(launcherExit);
  }));
  let pending = '';
  let stdoutBytes = 0;
  ownedLauncher.stdout.on('data', chunk => {
    stdoutBytes += chunk.length;
    if (stdoutBytes > 512 * 1024) { evidence.launcher_stdout_budget_exceeded = true; return; }
    pending += chunk.toString('utf8');
    for (let newline; (newline = pending.indexOf('\n')) >= 0;) {
      const line = pending.slice(0, newline);
      pending = pending.slice(newline + 1);
      if (line) acceptLauncherLine(line);
    }
  });
  ownedLauncher.stderr.on('data', chunk => {
    if (launcherStderr.length + chunk.length <= 64 * 1024) launcherStderr += chunk.toString('utf8');
  });
  ownedLauncher.on('error', error => { evidence.launcher_spawn_error = String(error); });
  const ready = await waitForLauncherRecord(record => record.kind === 'ready' || record.kind === 'launcher_error', 'ready');
  assert.equal(ready.kind, 'ready', ready.error ?? 'owned launcher failed before ready');
  const identity = ready.launcher_identity;
  assert.equal(identity?.pid, ownedLauncher.pid, 'launcher ready PID differs from direct child PID');
  assert.equal(identity?.parent_pid, process.pid, 'launcher is not a direct child of this Node process');
  assert.ok(samePath(identity.executable_path, launcherBinary), 'launcher executable identity mismatch');
  launcherReady = ready;
  evidence.owned_launcher_pid = identity.pid;
  evidence.owned_launcher_identity = identity;
  evidence.launcher_id = ready.launcher_id;
}

async function launchOwnedTextEdit() {
  const pending = waitForLauncherRecord(
    record => record.kind === 'launch_receipt' || record.kind === 'launch_error', 'TextEdit launch receipt'
  );
  ownedLauncher.stdin.write(JSON.stringify({
    op: 'launch', run_id: runId, document_path: tempDocument, document_sha256: sha256(tempDocument),
  }) + '\n');
  const response = await pending;
  if (response.kind !== 'launch_receipt') throw new Error(response.error ?? 'NSWorkspace launch receipt missing');
  const receipt = response.receipt;
  assert.equal(response.launcher_id, launcherReady.launcher_id);
  assert.equal(receipt?.launcher_id, launcherReady.launcher_id);
  assert.equal(receipt?.run_id, runId);
  assert.deepEqual(receipt?.launcher_identity, launcherReady.launcher_identity);
  assert.equal(receipt?.bundle_identifier, 'com.apple.TextEdit');
  assert.ok(samePath(receipt.executable_path, textEditPath));
  assert.equal(receipt?.document_sha256, sha256(tempDocument));
  assert.equal(receipt?.new_pid_excluded_from_preexisting_set, true);
  assert.equal(receipt?.target_started_within_launch_interval, true);
  assert.equal(receipt?.creates_new_application_instance, true);
  assert.equal(receipt?.adds_to_recent_items, false);
  assert.equal(receipt?.activates_new_instance, false);
  assert.deepEqual(receipt?.arguments, ['-ApplePersistenceIgnoreState', 'YES']);
  assert.equal(receipt?.no_input_authority, true);
  assert.equal(receipt?.preexisting_textedit_pids?.includes(receipt?.target_identity?.pid), false);
  textEditReceipt = receipt;
  evidence.launch_receipt = receipt;
  evidence.textedit_pid = receipt.target_identity.pid;
  evidence.preexisting_textedit_pids = receipt.preexisting_textedit_pids;
  return receipt;
}

function makeUnicodeProbeRequest(receipt, activationObservation = undefined) {
  return {
    target_pid: receipt.target_identity.pid,
    probe_parent_pid: process.pid,
    launcher_pid: receipt.launcher_identity.pid,
    launcher_id: receipt.launcher_id,
    executable_path: realpathSync(textEditPath),
    bundle_id: 'com.apple.TextEdit',
    document_path: tempDocument,
    document_sha256: receipt.document_sha256,
    expected_text: expectedText,
    run_id: runId,
    launch_receipt: receipt,
    ...(activationObservation ? { activation_observation: activationObservation } : {}),
  };
}

async function runProbeRecord(args, request, label, timeoutMs = 8_000) {
  const proc = spawn(probeBinary, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  const stdoutChunks = [];
  let stdoutBytes = 0;
  let stderr = '';
  let overflow = false;
  proc.stdout.on('data', chunk => {
    stdoutBytes += chunk.length;
    if (stdoutBytes <= 512 * 1024) stdoutChunks.push(chunk);
    else overflow = true;
  });
  proc.stderr.on('data', chunk => { if (stderr.length + chunk.length <= 32 * 1024) stderr += chunk.toString('utf8'); });
  const closed = new Promise(resolveClose => proc.once('close', (code, signal) => resolveClose({ code, signal })));
  proc.stdin.end(JSON.stringify(request));
  let timer;
  const result = await Promise.race([closed, new Promise(resolveTimeout => { timer = setTimeout(() => resolveTimeout(null), timeoutMs); })]);
  clearTimeout(timer);
  const stdout = Buffer.concat(stdoutChunks).toString('utf8');
  const rawPath = join(output, `${label.replace(/[^a-z0-9-]/giu, '-')}.stdout.jsonl`);
  writeFileSync(rawPath, stdout, { mode: 0o600 });
  evidence[`${label}_raw_stdout_path`] = rawPath;
  evidence[`${label}_stderr`] = stderr;
  if (!result) {
    proc.kill('SIGTERM');
    await Promise.race([closed, pause(1000)]);
    if (proc.exitCode == null && proc.signalCode == null) proc.kill('SIGKILL');
    throw new Error(`${label} timed out after ${timeoutMs}ms; raw stdout retained at ${rawPath}`);
  }
  assert.equal(result.code, 0, `${label} exited ${result.code ?? result.signal}: ${stderr}`);
  assert.equal(overflow, false, `${label} exceeded the output budget`);
  const lines = stdout.split(/\r?\n/u).filter(Boolean);
  assert.equal(lines.length, 1, `${label} must emit one JSON record; raw stdout retained at ${rawPath}`);
  return JSON.parse(lines[0]);
}

async function verifyOwnedWindow(receipt) {
  const record = await runProbeRecord(
    ['--window-ready-only'], makeUnicodeProbeRequest(receipt), 'window-readiness'
  );
  evidence.window_readiness = record;
  assert.equal(record.kind, 'unicode_temp_window_readiness');
  assert.equal(record.run_id, runId);
  assert.equal(record.target_pid, receipt.target_identity.pid);
  return record;
}

async function activateOwnedTextEdit(receipt, readiness) {
  const expectedTitle = 'owned-unicode-fixture.txt';
  const verified = readiness.window_verified === true &&
    readiness.process_identity_revalidated === true &&
    readiness.target_pid_was_preexisting === false &&
    readiness.cg_window_count === 1 && readiness.ax_window_count === 1 &&
    readiness.frames_match === true && readiness.cg_window_id > 0 &&
    readiness.cg_title?.includes(expectedTitle) && readiness.ax_title?.includes(expectedTitle);
  if (!verified) {
    return {
      attempted: false, attempt_count: 0, return_value: null,
      status: 'owned_window_readiness_not_verified', window_verified: false,
      cg_window_count: readiness.cg_window_count ?? 0, ax_window_count: readiness.ax_window_count ?? 0,
      cg_window_id: readiness.cg_window_id ?? null, cg_title: readiness.cg_title ?? null,
      ax_title: readiness.ax_title ?? null, frames_match: readiness.frames_match === true,
      system_focused_application_pid_before_activation: readiness.system_focused_application_pid_at_window_check ?? null,
    };
  }
  const pending = waitForLauncherRecord(record => record.kind === 'activation_result', 'owned TextEdit activation result', 5000);
  ownedLauncher.stdin.write(JSON.stringify({
    op: 'activate_owned', run_id: receipt.run_id,
    target_pid: receipt.target_identity.pid,
    target_start_seconds: receipt.target_identity.start_seconds,
    target_start_microseconds: receipt.target_identity.start_microseconds,
    cg_window_count: readiness.cg_window_count, ax_window_count: readiness.ax_window_count,
    cg_window_id: readiness.cg_window_id, cg_title: readiness.cg_title,
    ax_title: readiness.ax_title, frames_match: readiness.frames_match,
    system_focused_application_pid_before_activation: readiness.system_focused_application_pid_at_window_check ?? null,
  }) + '\n');
  const response = await pending;
  assert.equal(response.launcher_id, launcherReady.launcher_id);
  const observation = {
    attempted: response.attempted === true,
    attempt_count: Number.isInteger(response.attempt_count) ? response.attempt_count : 0,
    return_value: typeof response.return_value === 'boolean' ? response.return_value : null,
    status: response.status ?? 'activation_result_missing_status',
    window_verified: verified,
    cg_window_count: readiness.cg_window_count, ax_window_count: readiness.ax_window_count,
    cg_window_id: readiness.cg_window_id, cg_title: readiness.cg_title,
    ax_title: readiness.ax_title, frames_match: readiness.frames_match === true,
    system_focused_application_pid_before_activation: readiness.system_focused_application_pid_at_window_check ?? null,
  };
  evidence.activation = response;
  evidence.window_activation_calls = observation.attempt_count;
  evidence.window_activation_return_value = observation.return_value;
  return observation;
}

function analyzeSourceObservation(record, receipt, windowID) {
  const structure = record.structure ?? {};
  const appMatches = structure.app?.bundle_id === 'com.apple.TextEdit' &&
    structure.app?.pid === receipt.target_identity.pid;
  const windows = Array.isArray(structure.windows) ? structure.windows : [];
  const matchingWindows = windows.filter(window => window.window_id === windowID &&
    window.owner_bundle_id === 'com.apple.TextEdit');
  const objects = Array.isArray(structure.objects) ? structure.objects : [];
  const textControls = objects.filter(object => object.window_id === windowID &&
    ['AXTextArea', 'AXTextField'].includes(object.role));
  const publicNormalControls = textControls.filter(object => object.secure === false &&
    (object.subrole == null || object.subrole === '') && object.value?.type === 'text');
  const focus = structure.focus_context ?? null;
  const controls = textControls.map(object => {
    const value = object.value?.type === 'text' && typeof object.value.value === 'string' ? object.value.value : null;
    return {
      instance_ref: object.instance_ref,
      window_id: object.window_id,
      role: object.role,
      subrole: object.subrole ?? null,
      title: object.title ?? null,
      description: object.description ?? null,
      title_ax_error: 'Unknown; product Source does not export per-attribute AX error codes.',
      description_ax_error: 'Unknown; product Source does not associate structure.incomplete_reason with an object.',
      secure: object.secure,
      enabled: object.enabled,
      enabled_ax_error: 'Unknown; product Source wire does not export the AXEnabled query result/error separately.',
      enabled_false_proves_ax_disabled: null,
      focused: object.focused,
      value_type: object.value?.type ?? null,
      value_utf8_bytes: value == null ? null : Buffer.byteLength(value, 'utf8'),
      value_sha256: value == null ? null : createHash('sha256').update(value, 'utf8').digest('hex'),
      value_equals_preexisting_temp_document: value == null ? null : value === expectedText,
      actions: object.actions,
      set_value_action_exposed: Array.isArray(object.actions) && object.actions.includes('set_value'),
      source_focus_context_points_to_object: focus?.focused_instance === object.instance_ref &&
        focus?.relation === 'exact_control' && focus?.origin_window_id === windowID,
    };
  });
  return {
    stream_id: record.stream_id,
    sequence: record.sequence,
    source_app_bundle_matches: structure.app?.bundle_id === 'com.apple.TextEdit',
    source_app_pid_matches_receipt: structure.app?.pid === receipt.target_identity.pid,
    source_app_identity_matches_receipt: appMatches,
    structure_complete: structure.complete === true,
    structure_incomplete_reason: structure.incomplete_reason ?? null,
    incomplete_reason_owner: 'Unknown; product Source reports an aggregate reason without an element reference.',
    active_layer: structure.active_layer ?? null,
    registered_window_id: windowID,
    window_readiness_verified_for_registered_policy: windowID != null,
    matching_owned_window_count: matchingWindows.length,
    matching_owned_window: matchingWindows.length === 1 ? matchingWindows[0] : null,
    all_window_ids: windows.map(window => window.window_id),
    source_window_scope_valid: appMatches && matchingWindows.length === 1,
    observer_available: record.observer_available === true,
    observer_reason: record.observer_reason ?? null,
    observer_revision: record.observer_revision ?? null,
    focus_context: focus,
    focus_context_present: focus != null,
    focus_points_to_registered_window: focus?.origin_window_id === windowID,
    focus_points_to_one_source_object: focus != null && objects.filter(object => object.instance_ref === focus.focused_instance).length === 1,
    focus_composition: focus?.composition ?? null,
    keyboard_context: focus?.keyboard ?? null,
    text_control_count_in_registered_window: textControls.length,
    public_normal_text_control_count: publicNormalControls.length,
    text_controls: controls,
    input_authorized: false,
    input_posted: false,
    ax_set_value_calls: 0,
    ax_press_calls: 0,
    set_focus_calls: 0,
    cg_post_event_calls: 0,
  };
}

function recordSourceObservationFailures(record, report) {
  const failures = [];
  if (!report.source_window_scope_valid) failures.push({
    gate: 'owned_window_scope',
    reason: 'Source did not return exactly one owner-bundle window matching the readiness window id and receipt PID.',
  });
  if (!report.structure_complete) failures.push({
    gate: 'structure_complete',
    reason: report.structure_incomplete_reason ?? 'Source structure.complete was false without an incomplete_reason.',
  });
  if (!report.observer_available) failures.push({
    gate: 'observer_available',
    reason: report.observer_reason ?? 'Source observer_available was false without an observer_reason.',
  });
  if (!report.focus_context_present) failures.push({
    gate: 'focus_context',
    reason: report.structure_complete
      ? 'Source omitted focus_context from a complete observation.'
      : 'Source emitted structure.complete=false; AXSession clears focus_context and window_context for incomplete reads.',
    reason_source: report.structure_complete ? 'current product Source observation' : 'inferred from current native/macos/AXSession.swift incomplete-read branch',
  });
  if (failures.length === 0) return;
  const fact = {
    stream_id: record.stream_id ?? null,
    sequence: record.sequence ?? null,
    first_failed_gate: failures[0].gate,
    first_failed_reason: failures[0].reason,
    failures,
  };
  evidence.source_observation_failures.push(fact);
  if (evidence.source_observation_failures.length > 32) evidence.source_observation_failures.shift();
  evidence.source_observation_first_failure ??= fact;
  failOnce('product_source_observation', new Error(`${fact.first_failed_gate}: ${fact.first_failed_reason}`));
}

function acceptSourceLine(line, receipt, policyWindowID) {
  if (!line) return;
  let record;
  try { record = JSON.parse(line); }
  catch (error) {
    evidence.source_json_errors ??= [];
    if (evidence.source_json_errors.length < 16) evidence.source_json_errors.push(String(error));
    failOnce('parse_product_source_ndjson', error);
    return;
  }
  const summary = { kind: record.kind ?? 'unknown', sequence: record.sequence ?? null, stream_id: record.stream_id ?? null };
  if (record.kind === 'health') Object.assign(summary, { source: record.source ?? null, state: record.state, reason: record.reason ?? null });
  if (record.kind === 'gap') Object.assign(summary, { reason: record.reason, dropped_frames: record.dropped_frames });
  if (record.kind === 'frame') Object.assign(summary, { status: record.frame?.status, frame_id: record.frame?.frame_id ?? null, png_file: record.frame?.png_file ?? null });
  if (record.kind === 'observation') {
    const parsedApp = record.structure?.app;
    const matches = parsedApp?.bundle_id === 'com.apple.TextEdit' && parsedApp?.pid === receipt.target_identity.pid;
    const targetWindow = (record.structure?.windows ?? []).some(window => window.window_id === policyWindowID && window.owner_bundle_id === 'com.apple.TextEdit');
    Object.assign(summary, {
      app_pid: parsedApp?.pid ?? null, app_bundle_id: parsedApp?.bundle_id ?? null,
      structure_complete: record.structure?.complete === true,
      structure_incomplete_reason: record.structure?.incomplete_reason ?? null,
      observer_available: record.observer_available === true,
      observer_reason: record.observer_reason ?? null,
      target_window_present: targetWindow,
    });
    if (matches && (policyWindowID == null || targetWindow)) {
      targetObservationCount += 1;
      if (record.structure.complete === true) targetCompleteObservationCount += 1;
      evidence.source_observations.push(record);
      if (evidence.source_observations.length > 3) evidence.source_observations.shift();
      evidence.source_scope_check = analyzeSourceObservation(record, receipt, policyWindowID);
      evidence.source_text_controls = evidence.source_scope_check.text_controls;
      recordSourceObservationFailures(record, evidence.source_scope_check);
    }
  }
  evidence.source_records.push(summary);
  if (evidence.source_records.length > 160) evidence.source_records.shift();
  if (record.kind === 'observation') save();
}

async function runProductSourceWatch(receipt, readiness) {
  const readinessVerified = readiness?.window_verified === true &&
    readiness.process_identity_revalidated === true && readiness.target_pid_was_preexisting === false &&
    readiness.cg_window_count === 1 && readiness.ax_window_count === 1 && readiness.frames_match === true &&
    Number.isInteger(readiness.cg_window_id) && readiness.cg_window_id > 0;
  targetWindowID = readinessVerified ? `window-${readiness.cg_window_id}` : null;
  mkdirSync(sourceAssets, { recursive: true, mode: 0o700 });
  writeFileSync(sourceStdoutPath, '', { mode: 0o600, flag: 'wx' });
  writeFileSync(sourceStderrPath, '', { mode: 0o600, flag: 'wx' });
  const sessionEpoch = Number(BigInt(`0x${runId.replaceAll('-', '').slice(0, 12)}`));
  const request = {
    protocol_version: 1,
    command: 'watch',
    watch_id: randomUUID(),
    clock_origin_unix_ms: Date.now(),
    surface: { id: 'main', epoch: sessionEpoch },
    bundle_id: 'com.apple.TextEdit',
    pid: receipt.target_identity.pid,
    output_dir: sourceAssets,
    timeout_ms: 1500,
    interval_ms: 1000,
    max_nodes: 512,
    max_depth: 12,
    max_ocr_blocks: 128,
    session_epoch: sessionEpoch,
    ...(targetWindowID ? { text_policy: { kind: 'registered_editor', window_id: targetWindowID } } : {}),
  };
  evidence.source_watch_request = request;
  evidence.registered_text_policy_applied = targetWindowID != null;
  evidence.source_watch_launch_reason = readinessVerified
    ? 'exact owned temporary-document window was verified by the private AX/CG probe'
    : 'window identity was not verified; watch is still scoped to the receipt PID and runs without registered text exposure';
  sourceChild = spawn(sourceBinary, [], { stdio: ['pipe', 'pipe', 'pipe'] });
  sourceClose = new Promise(resolveClose => sourceChild.once('close', (code, signal) => resolveClose({ code, signal })));
  let sourceStderr = '';
  sourceChild.stdout.on('data', chunk => {
    sourceOutputBytes += chunk.length;
    if (sourceOutputBytes <= sourceRawLimitBytes) appendFileSync(sourceStdoutPath, chunk);
    else {
      sourceOutputOverflow = true;
      if (!sourceStopReason) sourceStopReason = 'source stdout exceeded 16 MiB';
      if (sourceChild.exitCode == null && sourceChild.signalCode == null) sourceChild.kill('SIGTERM');
    }
    sourcePending += sourceDecoder.write(chunk);
    for (let newline; (newline = sourcePending.indexOf('\n')) >= 0;) {
      const line = sourcePending.slice(0, newline);
      sourcePending = sourcePending.slice(newline + 1);
      if (Buffer.byteLength(line, 'utf8') > 768 * 1024) {
        failOnce('product_source_record_budget', new Error('source NDJSON line exceeded 768 KiB'));
        sourceChild.kill('SIGTERM');
      } else acceptSourceLine(line, receipt, targetWindowID);
    }
  });
  sourceChild.stdout.on('end', () => {
    sourcePending += sourceDecoder.end();
    if (sourcePending) acceptSourceLine(sourcePending, receipt, targetWindowID);
    sourcePending = '';
  });
  sourceChild.stderr.on('data', chunk => {
    if (sourceStderr.length + chunk.length <= 128 * 1024) sourceStderr += chunk.toString('utf8');
  });
  sourceChild.on('error', error => { evidence.source_spawn_error = String(error); failOnce('spawn_product_source', error); });
  sourceChild.stdin.on('error', error => { evidence.source_stdin_error = String(error); });
  evidence.source_process = {
    pid: sourceChild.pid ?? null,
    requested_executable: sourceBinary,
    executable_realpath: realpathSync(sourceBinary),
    direct_child_of_node: sourceChild.pid != null,
    pid_is_owned_by_this_run: true,
  };
  sourceChild.stdin.write(JSON.stringify(request) + '\n');
  save();

  const deadline = Date.now() + 18_000;
  while (Date.now() < deadline && !sourceOutputOverflow &&
         targetObservationCount < 2 && sourceChild.exitCode == null && sourceChild.signalCode == null) {
    await pause(25);
  }
  sourceStopReason ??= sourceOutputOverflow ? 'stdout_budget_exceeded' :
    targetObservationCount >= 2 ? 'two receipt-PID observations captured' :
      sourceChild.exitCode != null || sourceChild.signalCode != null ? 'source process exited before observation target' :
        'source observation wait deadline elapsed';
  if (sourceChild.exitCode == null && sourceChild.signalCode == null) sourceChild.kill('SIGTERM');
  const closed = await Promise.race([sourceClose, pause(3000).then(() => null)]);
  if (!closed && sourceChild.exitCode == null && sourceChild.signalCode == null) {
    sourceChild.kill('SIGKILL');
    await sourceClose;
  }
  appendFileSync(sourceStderrPath, sourceStderr);
  evidence.source_process.exit = await sourceClose;
  evidence.source_process.stop_reason = sourceStopReason;
  evidence.source_process.stdout_bytes = sourceOutputBytes;
  evidence.source_process.stdout_budget_exceeded = sourceOutputOverflow;
  evidence.source_process.stderr_bytes = Buffer.byteLength(sourceStderr, 'utf8');
  evidence.source_process.stderr = sourceStderr;
  evidence.source_observation_count_in_target_scope = targetObservationCount;
  evidence.source_complete_observation_count_in_target_scope = targetCompleteObservationCount;
  if (evidence.source_process.exit?.code != null && evidence.source_process.exit.code !== 0) {
    failOnce('product_source_exit', new Error(`cu-macos-source exited with code ${evidence.source_process.exit.code}`));
  }
  if (!targetObservationCount) failOnce('product_source_target_observation', new Error('normal watch produced no observation for the receipt PID and verified window'));
  save();
}

function inventorySourceAssets() {
  const files = [];
  let totalBytes = 0;
  function walk(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const relative = path.slice(sourceAssets.length + 1);
      const stat = lstatSync(path);
      if (entry.isSymbolicLink()) {
        files.push({ relative_path: relative, kind: 'symlink_not_followed', mode_octal: (stat.mode & 0o777).toString(8) });
      } else if (entry.isDirectory()) walk(path);
      else if (entry.isFile()) {
        totalBytes += stat.size;
        files.push({
          relative_path: relative, kind: 'file', bytes: stat.size,
          sha256: sha256(path), mode_octal: (stat.mode & 0o777).toString(8).padStart(3, '0'),
          owner_uid: stat.uid,
        });
      } else files.push({ relative_path: relative, kind: 'other', mode_octal: (stat.mode & 0o777).toString(8) });
    }
  }
  if (existsSync(sourceAssets)) walk(sourceAssets);
  evidence.source_assets_total_bytes = totalBytes;
  evidence.source_asset_files = files;
}

async function closeOwnedTextEdit() {
  if (!ownedLauncher) return;
  if (launcherExit) {
    evidence.owned_launcher_exit = { ...launcherExit, exited_before_close_request: true };
    return;
  }
  if (ownedLauncher.stdin.destroyed || ownedLauncher.stdin.writableEnded) {
    evidence.launcher_close_error = 'private launcher stdin unavailable for owned-instance close request';
  } else {
    const pending = waitForLauncherRecord(record => record.kind === 'quit_result' || record.kind === 'stdin_closed', 'owned TextEdit close result', 12_000);
    ownedLauncher.stdin.end(JSON.stringify({ op: 'quit' }) + '\n');
    try {
      const response = await pending;
      evidence.textedit_close = response.close ?? null;
      evidence.textedit_close_kind = response.kind;
    } catch (error) { evidence.launcher_close_error = String(error); }
  }
  const closed = await Promise.race([launcherClose, pause(2500).then(() => null)]);
  evidence.owned_launcher_exit = closed ?? { wait_timed_out: true };
  if (!closed && ownedLauncher.exitCode == null && ownedLauncher.signalCode == null) {
    ownedLauncher.kill('SIGTERM');
    evidence.launcher_killed_after_close_timeout = true;
    await Promise.race([launcherClose, pause(1500)]);
  }
}

save();
try {
  stage = 'compile_private_helpers';
  compilePrivateHelpers();
  evidence.ok = true;
  if (buildOnly) {
    evidence.status = 'strict_build_complete';
  } else if (runMode) {
    stage = 'textedit_preflight';
    if (process.platform !== 'darwin') throw new Error('macOS is required');
    const bundleID = execFileSync('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleIdentifier', infoPlist], {
      encoding: 'utf8', timeout: 5000,
    }).trim();
    assert.equal(bundleID, 'com.apple.TextEdit');
    assert.ok(existsSync(textEditPath));
    evidence.textedit_bundle_id = bundleID;
    evidence.textedit_executable_sha256 = sha256(textEditPath);
    mkdirSync(sourceAssets, { recursive: true, mode: 0o700 });
    writeFileSync(tempDocument, expectedText, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
    const documentStat = statSync(tempDocument);
    assert.equal(documentStat.uid, process.getuid(), 'temporary document owner must be this process');
    assert.equal(documentStat.mode & 0o777, 0o600, 'temporary document must remain private');
    evidence.temp_document = {
      path: tempDocument, extension: '.txt', bytes: Buffer.byteLength(expectedText, 'utf8'),
      sha256: sha256(tempDocument), mode_octal: (documentStat.mode & 0o777).toString(8).padStart(3, '0'),
      owner_uid: documentStat.uid, run_owned: true,
    };

    stage = 'start_private_launcher';
    await startOwnedLauncher();
    save();
    stage = 'launch_owned_textedit';
    const receipt = await launchOwnedTextEdit();
    save();

    let readiness = null;
    stage = 'verify_owned_textedit_window';
    try {
      readiness = await verifyOwnedWindow(receipt);
      const passed = readiness.window_verified === true && readiness.process_identity_revalidated === true &&
        readiness.target_pid_was_preexisting === false && readiness.cg_window_count === 1 && readiness.ax_window_count === 1 &&
        readiness.frames_match === true && readiness.cg_window_id > 0;
      if (!passed) failOnce(stage, new Error('owned temp-document AX/CG window did not pass the exact readiness check'));
    } catch (error) { failOnce(stage, error); }
    save();

    let activation = {
      attempted: false, attempt_count: 0, return_value: null, status: 'readiness_unavailable', window_verified: false,
    };
    if (readiness) {
      stage = 'activate_only_owned_textedit';
      try { activation = await activateOwnedTextEdit(receipt, readiness); }
      catch (error) { failOnce(stage, error); activation.status = 'activation_result_failed'; }
    }
    evidence.activation ??= activation;
    evidence.activation_observation = activation;
    evidence.window_activation_calls = activation.attempt_count ?? 0;
    evidence.window_activation_return_value = activation.return_value ?? null;
    save();

    if (readiness) {
      stage = 'direct_readonly_receiver_crosscheck';
      try {
        const record = await runProbeRecord([], makeUnicodeProbeRequest(receipt, activation), 'receiver-crosscheck', 12_000);
        evidence.receiver_probe = record;
        evidence.receiver_scope_crosscheck = {
          probe_complete: record.probe_complete === true,
          process_identity_revalidated: record.process_identity_revalidated === true,
          original_cg_window_unchanged_after: record.original_cg_window_unchanged_after === true,
          one_temp_document_window_verified: record.one_temp_document_window_verified === true,
          receiver_is_current_system_receiver: record.receiver_is_current_system_receiver === true,
          receiver_is_verified_for_temp_document_readback: record.receiver_is_verified_for_temp_document_readback === true,
          system_focused_application_pid: record.system_focused_application_pid ?? null,
          system_focused_application_pid_after: record.system_focused_application_pid_after ?? null,
          receiver_role: record.receiver_role ?? null,
          receiver_window_relation_same_cf: record.receiver_window_relation_same_cf ?? null,
          composition_conclusion: record.composition_conclusion ?? 'Unknown',
          text_mutation_calls: record.ax_set_value_calls ?? null,
          cg_post_event_calls: record.cg_post_event_calls ?? null,
        };
        if (record.probe_complete !== true) failOnce(stage, new Error('readonly receiver helper did not complete its identity/window checks'));
      } catch (error) { failOnce(stage, error); }
    }
    save();

    stage = 'product_source_normal_watch';
    try { await runProductSourceWatch(receipt, readiness); }
    catch (error) { failOnce(stage, error); }
    inventorySourceAssets();
    const scope = evidence.source_scope_check;
    evidence.source_probe_complete = scope?.source_window_scope_valid === true && scope?.structure_complete === true;
    evidence.source_raw_text_value_matches_preexisting_temp_document = scope?.text_controls?.some(control =>
      control.value_equals_preexisting_temp_document === true) ?? false;
    evidence.registered_text_value_read = evidence.source_probe_complete &&
      evidence.source_raw_text_value_matches_preexisting_temp_document;
    evidence.raw_text_value_present_in_incomplete_snapshot = scope?.structure_complete === false &&
      evidence.source_raw_text_value_matches_preexisting_temp_document;
    evidence.raw_set_value_action_exposed_in_source_snapshot = scope?.text_controls?.some(control =>
      control.set_value_action_exposed === true) ?? false;
    evidence.registered_text_set_value_action_observed = evidence.source_probe_complete &&
      evidence.raw_set_value_action_exposed_in_source_snapshot;
    evidence.product_source_snapshot_captured = evidence.source_observations.length > 0;
    evidence.status = evidence.source_probe_complete ? 'product_source_owned_scope_observed' : 'product_source_owned_scope_incomplete';
    evidence.ok = evidence.source_probe_complete;
    evidence.note = 'This is a read-only product Source watch of a prewritten temporary document. It does not prove TypeText delivery, AXSetValue behavior, TextEdit saving, or reopening.';
  }
} catch (error) {
  evidence.ok = false;
  evidence.status = 'failed';
  failOnce(stage, error);
} finally {
  if (sourceChild && sourceChild.exitCode == null && sourceChild.signalCode == null) {
    sourceChild.kill('SIGTERM');
    await Promise.race([sourceClose, pause(2500)]);
    if (sourceChild.exitCode == null && sourceChild.signalCode == null) {
      sourceChild.kill('SIGKILL');
      await sourceClose;
    }
  }
  if (sourceChild) {
    evidence.source_process ??= { pid: sourceChild.pid ?? null, requested_executable: sourceBinary };
    evidence.source_process.exit ??= await sourceClose;
    evidence.source_process.stop_reason ??= sourceStopReason;
  }
  try { inventorySourceAssets(); } catch (error) { evidence.source_asset_inventory_error = String(error); }
  await closeOwnedTextEdit();
  if (launcherStderr) evidence.launcher_stderr = launcherStderr;
  evidence.finished_at = new Date().toISOString();
  evidence.source_assets_directory_stat = existsSync(sourceAssets) ? {
    mode_octal: (statSync(sourceAssets).mode & 0o777).toString(8).padStart(3, '0'),
    owner_uid: statSync(sourceAssets).uid,
  } : null;
  evidence.source_assets_preserved = existsSync(sourceAssets);
  evidence.temp_document_preserved = existsSync(tempDocument);
  evidence.textedit_delivery_or_saved_mvp_claimed = false;
  evidence.mvp_complete = false;
  save();
  process.stdout.write(JSON.stringify({
    ok: evidence.ok,
    status: evidence.status ?? (buildOnly ? 'strict_build_complete' : 'not_run'),
    artifact,
    output,
    textedit_pid: evidence.textedit_pid,
    source_pid: evidence.source_process?.pid ?? null,
    source_scope_valid: evidence.source_scope_check?.source_window_scope_valid ?? false,
    first_failure: evidence.first_failure,
  }) + '\n');
}
