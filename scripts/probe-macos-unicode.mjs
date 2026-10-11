#!/usr/bin/env node
// Bounded AX/CG/TIS reads from one launcher-owned TextEdit instance. No input is posted.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const runId = randomUUID();
const output = mkdtempSync(join(tmpdir(), 'cu-owned-unicode-readonly-'));
const artifact = join(root, 'artifacts/m2-native-unicode-readonly-probe.json');
const textEditPath = '/System/Applications/TextEdit.app/Contents/MacOS/TextEdit';
const infoPlist = '/System/Applications/TextEdit.app/Contents/Info.plist';
const probeSource = join(root, 'native/device/fixtures/UnicodeReceiverProbe.swift');
const launcherSource = join(root, 'native/device/fixtures/OwnedTextEditLauncher.swift');
const probeBinary = join(output, 'UnicodeReceiverProbe');
const launcherBinary = join(output, 'OwnedTextEditLauncher');
const tempDocument = join(output, 'owned-unicode-fixture.txt');
const expectedText = 'Unicode receiver probe\nEnglish: hello from Computer Use\n中文：你好，世界\n';
const runMode = process.argv.includes('--run');
const buildOnly = process.argv.includes('--build-only');
const protocolCheck = process.argv.includes('--protocol-check');
assert([runMode, buildOnly, protocolCheck].filter(Boolean).length <= 1, 'choose one mode: --run, --build-only, or --protocol-check');

const evidence = {
  ok: false,
  run_id: runId,
  started_at: new Date().toISOString(),
  mode: runMode ? 'owned_textedit_readonly_run' : protocolCheck ? 'private_json_protocol_check' : 'strict_build_only',
  output,
  temp_document: null,
  textedit_pid: null,
  owned_launcher_pid: null,
  launch_receipt: null,
  input_posted: false,
  text_mutation_calls: 0,
  ax_press_calls: 0,
  set_focus_calls: 0,
  cg_post_event_calls: 0,
  window_activation_calls: 0,
  state_restore_arguments: ['-ApplePersistenceIgnoreState', 'YES'],
  persistent_preferences_mutated_by_launcher: false,
  user_document_paths_supplied: [],
  restored_user_document_observation: 'Unknown until the new instance AX window scope is read',
  mvp_complete: false,
  textedit_delivery_or_saved_mvp_claimed: false,
  ax_permission: 'Unknown until readonly helper reports AXIsProcessTrusted',
  cases: []
};

mkdirSync(dirname(artifact), { recursive: true });
if (existsSync(artifact)) {
  const previous = artifact.replace('.json', `.previous-${runId}.json`);
  copyFileSync(artifact, previous);
  evidence.previous_artifact = previous;
}
const save = () => writeFileSync(artifact, JSON.stringify(evidence, null, 2) + '\n');
const sha256 = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const pause = ms => new Promise(resolvePause => setTimeout(resolvePause, ms));
let ownedLauncher = null;
let launcherReady = null;
let launcherRecords = [];
let launcherWaiters = [];
let launcherExit = null;
let helper = null;
let helperClose = null;
let launcherClose = null;
let launcherStderr = '';

function compileProbe() {
  mkdirSync(join(output, 'module-cache'), { recursive: true });
  const flags = [
    '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(output, 'module-cache')
  ];
  try {
    const probeStdout = execFileSync('xcrun', ['swiftc', ...flags, probeSource, '-o', probeBinary], {
      encoding: 'utf8', timeout: 90_000, maxBuffer: 512 * 1024
    });
    const launcherStdout = execFileSync('xcrun', ['swiftc', ...flags, launcherSource, '-o', launcherBinary], {
      encoding: 'utf8', timeout: 90_000, maxBuffer: 512 * 1024
    });
    evidence.swift_compile = { ok: true, probe_stdout: probeStdout, launcher_stdout: launcherStdout };
  } catch (error) {
    evidence.swift_compile = {
      ok: false,
      status: error.status ?? null,
      signal: error.signal ?? null,
      stdout: String(error.stdout ?? ''),
      stderr: String(error.stderr ?? error.message ?? '').slice(0, 128 * 1024)
    };
    throw error;
  }
  evidence.probe_binary_sha256 = sha256(probeBinary);
  evidence.probe_source_sha256 = sha256(probeSource);
  evidence.launcher_binary_sha256 = sha256(launcherBinary);
  evidence.launcher_source_sha256 = sha256(launcherSource);
  evidence.script_source_sha256 = sha256(fileURLToPath(import.meta.url));
}

function acceptLauncherLine(line) {
  let record;
  try { record = JSON.parse(line); }
  catch (error) { evidence.launcher_json_error = String(error); return; }
  for (let index = 0; index < launcherWaiters.length; index += 1) {
    const waiter = launcherWaiters[index];
    if (!waiter.predicate(record)) continue;
    launcherWaiters.splice(index, 1);
    clearTimeout(waiter.timer);
    waiter.resolve(record);
    return;
  }
  launcherRecords.push(record);
}

function waitForLauncherRecord(predicate, label, timeoutMs = 20_000) {
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
  launcherReady = await waitForLauncherRecord(record => record.kind === 'ready' || record.kind === 'launcher_error', 'ready');
  assert.equal(launcherReady.kind, 'ready', launcherReady.error ?? 'owned launcher failed before ready');
  const launcherIdentity = launcherReady.launcher_identity;
  assert.equal(launcherIdentity?.pid, ownedLauncher.pid, 'ready record PID must be this run-owned launcher child');
  assert.equal(launcherIdentity?.parent_pid, process.pid, 'native launcher must remain a direct child of this Node process');
  assert.equal(realpathSync(launcherIdentity.executable_path), realpathSync(launcherBinary), 'launcher executable path mismatch');
  evidence.owned_launcher_pid = launcherIdentity.pid;
  evidence.owned_launcher_identity = launcherIdentity;
  evidence.launcher_id = launcherReady.launcher_id;
}

async function requestOwnedTextEdit() {
  const pending = waitForLauncherRecord(record => record.kind === 'launch_receipt' || record.kind === 'launch_error', 'TextEdit launch receipt');
  ownedLauncher.stdin.write(JSON.stringify({
    op: 'launch', run_id: runId, document_path: tempDocument, document_sha256: sha256(tempDocument)
  }) + '\n');
  const response = await pending;
  if (response.kind !== 'launch_receipt') {
    evidence.launch_error = response;
    throw new Error(response.error ?? 'NSWorkspace did not return a new TextEdit instance');
  }
  const receipt = response.receipt;
  assert.equal(response.launcher_id, launcherReady.launcher_id);
  assert.equal(receipt?.launcher_id, launcherReady.launcher_id);
  assert.equal(receipt?.run_id, runId);
  assert.deepEqual(receipt?.launcher_identity, launcherReady.launcher_identity);
  assert.equal(receipt?.bundle_identifier, 'com.apple.TextEdit');
  assert.equal(realpathSync(receipt.executable_path), realpathSync(textEditPath));
  assert.equal(receipt?.document_sha256, sha256(tempDocument));
  assert.equal(receipt?.new_pid_excluded_from_preexisting_set, true);
  assert.equal(receipt?.target_started_within_launch_interval, true);
  assert.equal(receipt?.creates_new_application_instance, true);
  assert.equal(receipt?.adds_to_recent_items, false);
  assert.equal(receipt?.activates_new_instance, false, 'activation is deferred until the owned document window is verified');
  assert.deepEqual(receipt?.arguments, evidence.state_restore_arguments);
  assert.equal(receipt?.no_input_authority, true);
  assert.equal(receipt?.preexisting_textedit_pids?.includes(receipt?.target_identity?.pid), false);
  evidence.launch_receipt = receipt;
  evidence.textedit_pid = receipt.target_identity.pid;
  evidence.preexisting_textedit_pids = receipt.preexisting_textedit_pids;
  return receipt;
}

async function decodeOnlyProbe(request) {
  const proc = spawn(probeBinary, ['--decode-only'], { stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '';
  let stderr = '';
  proc.stdout.on('data', chunk => { if (stdout.length + chunk.length <= 64 * 1024) stdout += chunk.toString('utf8'); });
  proc.stderr.on('data', chunk => { if (stderr.length + chunk.length <= 16 * 1024) stderr += chunk.toString('utf8'); });
  const closed = new Promise(resolveClose => proc.once('close', (code, signal) => resolveClose({ code, signal })));
  proc.stdin.end(JSON.stringify(request));
  const result = await Promise.race([closed, pause(5000).then(() => null)]);
  if (!result) {
    proc.kill('SIGTERM');
    throw new Error('decode-only probe exceeded 5 seconds');
  }
  assert.equal(result.code, 0, `decode-only probe failed (${result.code ?? result.signal}): ${stderr}`);
  const lines = stdout.split(/\r?\n/u).filter(Boolean);
  assert.equal(lines.length, 1, 'decode-only probe must emit one record');
  return JSON.parse(lines[0]);
}

async function runProbeRecord(args, request, label, timeoutMs = 5000) {
  const proc = spawn(probeBinary, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '';
  let stderr = '';
  let overflow = false;
  proc.stdout.on('data', chunk => {
    if (stdout.length + chunk.length > 256 * 1024) { overflow = true; proc.kill('SIGTERM'); return; }
    stdout += chunk.toString('utf8');
  });
  proc.stderr.on('data', chunk => {
    if (stderr.length + chunk.length <= 32 * 1024) stderr += chunk.toString('utf8');
  });
  const closed = new Promise(resolveClose => proc.once('close', (code, signal) => resolveClose({ code, signal })));
  proc.stdin.end(JSON.stringify(request));
  let timer;
  const result = await Promise.race([
    closed,
    new Promise(resolveTimeout => { timer = setTimeout(() => resolveTimeout(null), timeoutMs); })
  ]);
  clearTimeout(timer);
  if (!result) {
    proc.kill('SIGTERM');
    await Promise.race([closed, pause(1000)]);
    throw new Error(`${label} exceeded ${timeoutMs} ms`);
  }
  assert.equal(result.code, 0, `${label} exited ${result.code ?? result.signal}: ${stderr}`);
  assert.equal(overflow, false, `${label} exceeded its output budget`);
  const lines = stdout.split(/\r?\n/u).filter(Boolean);
  assert.equal(lines.length, 1, `${label} must emit exactly one JSON record`);
  return JSON.parse(lines[0]);
}

function makeProbeRequest(receipt, activationObservation = undefined) {
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
    ...(activationObservation ? { activation_observation: activationObservation } : {})
  };
}

async function runWindowReadinessProbe(receipt) {
  const record = await runProbeRecord(
    ['--window-ready-only'], makeProbeRequest(receipt), 'owned temp-window readiness probe', 5000
  );
  assert.equal(record.kind, 'unicode_temp_window_readiness');
  assert.equal(record.run_id, runId);
  assert.equal(record.target_pid, receipt.target_identity.pid);
  evidence.window_readiness = record;
  return record;
}

async function activateOwnedTextEdit(receipt, readiness) {
  const windowVerified = readiness.window_verified === true &&
    readiness.process_identity_revalidated === true &&
    readiness.target_pid_was_preexisting === false &&
    readiness.cg_window_count === 1 && readiness.ax_window_count === 1 &&
    readiness.frames_match === true && readiness.cg_window_id > 0 &&
    readiness.cg_title?.includes('owned-unicode-fixture.txt') &&
    readiness.ax_title?.includes('owned-unicode-fixture.txt');
  let response = null;
  if (windowVerified) {
    const pending = waitForLauncherRecord(record => record.kind === 'activation_result', 'owned-instance activation result', 5000);
    ownedLauncher.stdin.write(JSON.stringify({
      op: 'activate_owned',
      run_id: receipt.run_id,
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
    }) + '\n');
    response = await pending;
    assert.equal(response.launcher_id, launcherReady.launcher_id);
  }

  const observation = {
    attempted: response?.attempted === true,
    attempt_count: Number.isInteger(response?.attempt_count) ? response.attempt_count : 0,
    return_value: typeof response?.return_value === 'boolean' ? response.return_value : null,
    status: response?.status ?? (windowVerified ? 'activation_result_unavailable' : 'window_readiness_not_verified'),
    window_verified: windowVerified,
    cg_window_count: readiness.cg_window_count ?? 0,
    ax_window_count: readiness.ax_window_count ?? 0,
    cg_window_id: readiness.cg_window_id ?? null,
    cg_title: readiness.cg_title ?? null,
    ax_title: readiness.ax_title ?? null,
    frames_match: readiness.frames_match === true,
    system_focused_application_pid_before_activation: windowVerified
      ? readiness.system_focused_application_pid_at_window_check ?? null
      : null
  };
  evidence.activation = response ?? { attempted: false, attempt_count: 0, status: observation.status };
  evidence.activation_observation = observation;
  evidence.window_activation_calls = observation.attempt_count;
  evidence.window_activation_return_value = observation.return_value;
  return observation;
}

async function runProtocolCheck() {
  await startOwnedLauncher();
  const checkRunId = `protocol-${runId}`;
  const documentHash = 'a'.repeat(64);
  const pending = waitForLauncherRecord(
    record => record.kind === 'protocol_check' || record.kind === 'launch_error', 'snake-case launcher fields'
  );
  ownedLauncher.stdin.write(JSON.stringify({
    op: 'decode_check', run_id: checkRunId, document_path: tempDocument, document_sha256: documentHash,
    target_pid: 12345,
    target_start_seconds: 1791000000,
    target_start_microseconds: 123456,
    cg_window_count: 1,
    ax_window_count: 1,
    cg_window_id: 77,
    cg_title: 'owned-unicode-fixture.txt - TextEdit',
    ax_title: 'owned-unicode-fixture.txt - TextEdit',
    frames_match: true,
    system_focused_application_pid_before_activation: 28700
  }) + '\n');
  const response = await pending;
  assert.equal(response.kind, 'protocol_check', response.error ?? 'launcher JSON decode failed');
  assert.equal(response.accepted, true);
  assert.equal(response.run_id, checkRunId);
  assert.equal(response.document_sha256, documentHash);
  assert.deepEqual(response.activation_command_fields, {
    target_pid: 12345,
    target_start_seconds: 1791000000,
    target_start_microseconds: 123456,
    cg_window_count: 1,
    ax_window_count: 1,
    cg_window_id: 77,
    cg_title: 'owned-unicode-fixture.txt - TextEdit',
    ax_title: 'owned-unicode-fixture.txt - TextEdit',
    frames_match: true,
    system_focused_application_pid_before_activation: 28700
  });
  const receipt = response.receipt;
  assert.equal(receipt?.run_id, checkRunId);
  assert.equal(receipt?.document_sha256, documentHash);
  assert.equal(receipt?.preexisting_textedit_pids?.[0], 54321);
  assert.equal(receipt?.launcher_identity?.parent_pid, process.pid);
  assert.equal(receipt?.target_identity?.start_microseconds, 123456);
  assert.equal(receipt?.target_identity?.parent_pid, 1);
  const decoded = await decodeOnlyProbe({
    target_pid: receipt.target_identity.pid,
    probe_parent_pid: process.pid,
    launcher_pid: receipt.launcher_identity.pid,
    launcher_id: receipt.launcher_id,
    executable_path: realpathSync(textEditPath),
    bundle_id: 'com.apple.TextEdit',
    document_path: tempDocument,
    document_sha256: documentHash,
    expected_text: expectedText,
    run_id: checkRunId,
    launch_receipt: receipt,
    activation_observation: {
      attempted: false,
      attempt_count: 0,
      return_value: null,
      status: 'protocol_check_only',
      window_verified: false,
      cg_window_count: 0,
      ax_window_count: 0,
      cg_window_id: null,
      cg_title: null,
      ax_title: null,
      frames_match: false,
      system_focused_application_pid_before_activation: null
    }
  });
  assert.equal(decoded.decoded, true);
  assert.equal(decoded.receipt_run_id, checkRunId);
  assert.equal(decoded.receipt_document_sha256, documentHash);
  assert.equal(decoded.preexisting_textedit_pids[0], 54321);
  assert.equal(decoded.launcher_parent_pid, process.pid);
  assert.equal(decoded.target_start_microseconds, 123456);
  assert.equal(decoded.target_parent_pid, 1);
  assert.equal(decoded.target_executable_path, realpathSync(textEditPath));
  assert.equal(decoded.activation_attempted, false);
  assert.equal(decoded.activation_attempt_count, 0);
  assert.equal(decoded.activation_return_value, null);
  evidence.protocol_check = { launcher_command_decoded: true, launch_receipt_encoded_and_decoded: true, probe_request_decoded: true, sample: decoded };
  evidence.ok = true;
}

async function runReadonlyHelper(activationObservation) {
  const receipt = evidence.launch_receipt;
  assert.equal(typeof receipt?.target_identity?.pid, 'number', 'missing verified TextEdit launch receipt');
  helper = spawn(probeBinary, [], { stdio: ['pipe', 'pipe', 'pipe'] });
  helperClose = new Promise(resolveClose => helper.once('close', (code, signal) => resolveClose({ code, signal })));
  let stdout = '';
  let stderr = '';
  let overflow = false;
  helper.stdout.on('data', chunk => {
    if (stdout.length + chunk.length > 512 * 1024) {
      overflow = true;
      helper.kill('SIGTERM');
      return;
    }
    stdout += chunk.toString('utf8');
  });
  helper.stderr.on('data', chunk => {
    if (stderr.length + chunk.length <= 64 * 1024) stderr += chunk.toString('utf8');
  });
  const request = makeProbeRequest(receipt, activationObservation);
  evidence.helper_request = {
    target_pid: request.target_pid,
    probe_parent_pid: request.probe_parent_pid,
    launcher_pid: request.launcher_pid,
    launcher_id: request.launcher_id,
    executable_path: request.executable_path,
    bundle_id: request.bundle_id,
    document_path: request.document_path,
    expected_text_utf8_bytes: Buffer.byteLength(expectedText, 'utf8'),
    activation_attempt_count: activationObservation.attempt_count,
    activation_api_return_value: activationObservation.return_value,
    system_focused_application_pid_before_activation: activationObservation.system_focused_application_pid_before_activation,
    run_id: runId
  };
  helper.stdin.end(JSON.stringify(request));
  let timedOut = false;
  const closed = await Promise.race([
    helperClose,
    pause(15_000).then(async () => {
      timedOut = true;
      helper.kill('SIGTERM');
      return await Promise.race([helperClose, pause(1000).then(() => null)]);
    })
  ]);
  evidence.helper_process = {
    exit_code: closed?.code ?? null,
    signal: closed?.signal ?? null,
    stderr,
    output_budget_exceeded: overflow,
    timed_out: timedOut
  };
  const lines = stdout.split(/\r?\n/u).filter(Boolean);
  if (lines.length === 1) {
    try { evidence.cases.push(JSON.parse(lines[0])); }
    catch (error) { evidence.helper_json_error = String(error); evidence.helper_stdout = stdout.slice(0, 16 * 1024); }
  } else {
    evidence.helper_stdout = stdout.slice(0, 16 * 1024);
    evidence.helper_line_count = lines.length;
  }
  const trustFact = evidence.cases[0]?.ax_process_trusted_before ?? evidence.cases[0]?.ax_process_trusted;
  evidence.ax_permission = trustFact === true ? 'trusted' : trustFact === false ? 'not_trusted' : 'Unknown';
  assert.equal(timedOut, false, 'readonly helper exceeded the wall-clock budget');
  assert.equal(closed?.code, 0, `readonly helper exited ${closed?.code ?? closed?.signal ?? 'unknown'}`);
  assert.equal(overflow, false, 'readonly helper exceeded output budget');
  assert.equal(evidence.cases.length, 1, 'readonly helper did not emit exactly one JSON record');
}

async function closeOwnedTextEdit() {
  if (!ownedLauncher) return;
  if (launcherExit) {
    evidence.owned_launcher_exit = { ...launcherExit, exited_before_close_request: true };
    return;
  }
  evidence.close_attempt = 'quit sent to the private launcher; it revalidates the receipt PID/start/path before NSRunningApplication.terminate()';
  const responsePending = waitForLauncherRecord(
    record => record.kind === 'quit_result' || record.kind === 'stdin_closed',
    'owned instance close result', 12_000
  );
  ownedLauncher.stdin.end(JSON.stringify({ op: 'quit' }) + '\n');
  try {
    const response = await responsePending;
    evidence.textedit_close = response.close;
  } catch (error) {
    evidence.close_error = String(error);
  }
  if (launcherStderr) evidence.launcher_stderr = launcherStderr;
  const closed = await Promise.race([launcherClose, pause(2500).then(() => null)]);
  evidence.owned_launcher_exit = closed ?? { wait_timed_out: true };
}

save();
try {
  compileProbe();
  if (buildOnly) {
    evidence.ok = true;
  } else if (protocolCheck) {
    await runProtocolCheck();
  } else if (runMode) {
    const plistBundle = execFileSync('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleIdentifier', infoPlist], {
      encoding: 'utf8', timeout: 5000
    }).trim();
    assert.equal(plistBundle, 'com.apple.TextEdit');
    assert.equal(existsSync(textEditPath), true, 'TextEdit executable is unavailable at the expected path');
    evidence.textedit_bundle_id = plistBundle;
    evidence.textedit_executable_sha256 = sha256(textEditPath);
    writeFileSync(tempDocument, expectedText, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
    const documentStat = statSync(tempDocument);
    evidence.temp_document = {
      path: tempDocument,
      extension: '.txt',
      bytes: Buffer.byteLength(expectedText, 'utf8'),
      sha256: sha256(tempDocument),
      permissions: '0600 requested at creation',
      actual_mode_octal: (documentStat.mode & 0o777).toString(8).padStart(3, '0'),
      owner_uid: documentStat.uid,
      run_owned: true
    };
    await startOwnedLauncher();
    save();
    const receipt = await requestOwnedTextEdit();
    evidence.open_configuration = {
      creates_new_application_instance: receipt.creates_new_application_instance,
      adds_to_recent_items: receipt.adds_to_recent_items,
      activates_new_instance: receipt.activates_new_instance,
      arguments: receipt.arguments,
      no_input_authority: receipt.no_input_authority
    };
    save();
    const readiness = await runWindowReadinessProbe(receipt);
    const activationObservation = await activateOwnedTextEdit(receipt, readiness);
    await pause(300);
    await runReadonlyHelper(activationObservation);
    const observation = evidence.cases[0];
    evidence.additional_textedit_window_observed = observation.ax_window_count > 1;
    evidence.restored_user_document_observation = observation.one_temp_document_window_verified === true
      ? 'No additional AX window was observed in the new instance; this does not establish document delivery or save behavior.'
      : 'Unknown; the unique temporary-document AX window check did not pass.';
    evidence.ok = observation.process_identity_revalidated === true &&
      observation.original_cg_window_unchanged_after === true &&
      observation.launcher_process_identity_revalidated === true;
    evidence.note = observation.composition_conclusion === 'Unknown'
      ? 'Composition remains Unknown; selection and TIS metadata are recorded independently.'
      : 'Composition conclusion is reported only as returned by direct marked-property reads.';
  } else {
    evidence.ok = true;
    evidence.note = 'Compile-only is the default. Pass --run to launch one new TextEdit executable child and read it.';
  }
} catch (error) {
  evidence.error = String(error?.stack ?? error);
  process.exitCode = 1;
} finally {
  await closeOwnedTextEdit();
  if (launcherStderr) evidence.launcher_stderr = launcherStderr;
  evidence.finished_at = new Date().toISOString();
  save();
  process.stdout.write(JSON.stringify({
    ok: evidence.ok,
    mode: evidence.mode,
    artifact,
    output,
    textedit_pid: evidence.textedit_pid,
    owned_launcher_pid: evidence.owned_launcher_pid,
    ax_permission: evidence.ax_permission,
    error: evidence.error ?? null
  }) + '\n');
}
