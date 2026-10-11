#!/usr/bin/env node
// Owned helper-death acceptance. Building helpers is the default; live input
// requires --run and a previously recorded --owned-helper-dir.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import {
  copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync,
  readdirSync, statSync, writeFileSync
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: {
  run: { type: 'boolean', default: false },
  'build-only': { type: 'boolean', default: false },
  'stream-probe': { type: 'boolean', default: false },
  'pre-kill-capture': { type: 'boolean', default: false },
  phase: { type: 'string' },
  'owned-helper-dir': { type: 'string' }
} });
assert.ok(!(values.run && values['build-only']), '--run and --build-only are mutually exclusive');
assert.ok(!values.run || values['owned-helper-dir'], '--run requires --owned-helper-dir from a prior build-only run');
assert.ok(!values['owned-helper-dir'] || values.run, '--owned-helper-dir is only used with --run');
assert.ok(!values['stream-probe'] || values.run, '--stream-probe requires --run');

const root = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), '..'));
const recordPath = join(root, 'artifacts/m2-native-keyboard-death.json');
const device = join(root, 'target/debug/cu-device');
const source = join(root, 'target/debug/cu-macos-source');
const runner = join(root, 'target/debug/examples/native_key_session');
const phases = ['before-down', 'after-up', 'waiting-ack', 'between-down-up'];
assert.ok(!values.phase || values.run, '--phase requires --run');
assert.ok(!values.phase || phases.includes(values.phase), 'unknown owned death phase');
assert.ok(!values['pre-kill-capture'] || (values.run && values['stream-probe'] && values.phase === 'waiting-ack'),
  '--pre-kill-capture requires single waiting-ack with the independent stream probe');
const runPhases = values.phase ? [values.phase] : phases;
const output = realpathSync(values.run
  ? resolve(values['owned-helper-dir'])
  : mkdtempSync(join(tmpdir(), 'cu-owned-key-death-')));
const fixturePath = join(output, 'KeyFixture.app/Contents/MacOS/KeyFixture');
const probePath = join(output, 'KeyboardStateProbe');
const streamProbePath = join(output, 'OwnedKeyStreamProbe');
const manifestPath = join(output, 'owned-key-death-build.json');
const record = {
  ok: false,
  output,
  started_at: new Date().toISOString(),
  matrix_order: runPhases,
  diagnostic_single_phase: Boolean(values.phase),
  pre_kill_capture_requested: values['pre-kill-capture'],
  scenarios: [],
  production_keyboard_enabled: false,
  mvp_complete: false,
  real_provider_calls: 0,
  live_matrix_requested: Boolean(values.run)
};
const children = [];
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const sha = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const saveRecord = () => writeFileSync(recordPath, JSON.stringify(record, null, 2) + '\n');

function nativeSources() {
  return readdirSync(join(root, 'native/device')).filter(name => name.endsWith('.swift')).sort()
    .map(name => join(root, 'native/device', name));
}

function sourcePaths() {
  return [...new Set([
    'scripts/smoke-macos-keyboard-death.mjs',
    'native/device/AtomicKey.swift',
    'native/device/fixtures/KeyFixture.swift',
    'native/device/fixtures/KeyboardStateProbe.swift',
    'native/device/fixtures/OwnedKeyStreamProbe.swift',
    'crates/device-macos/examples/native_key_session.rs',
    'crates/device-macos/src/guard.rs',
    ...nativeSources().map(path => path.slice(root.length + 1))
  ])].sort();
}

function sourceHashes() {
  return Object.fromEntries(sourcePaths().map(path => [path, sha(join(root, path))]));
}

function swiftFlags(directory) {
  return [
    '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(directory, 'module-cache')
  ];
}

function buildOwnedInputs() {
  mkdirSync(dirname(fixturePath), { recursive: true });
  mkdirSync(join(output, 'helpers'), { recursive: true });
  const contents = join(output, 'KeyFixture.app/Contents');
  writeFileSync(join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.lingxi.computer-user.guard-fixture</string>
<key>CFBundleExecutable</key><string>KeyFixture</string><key>CFBundleName</key><string>Computer Use Key Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);

  const flags = swiftFlags(output);
  const sources = nativeSources();
  const helperPaths = {};
  let compiledHelper;
  for (const phase of phases) {
    const helper = join(output, 'helpers', `owned-key-${phase}`);
    // The death phase comes from the exact executable basename at startup;
    // all four paths use the same source and compilation definitions.
    if (compiledHelper) {
      copyFileSync(compiledHelper, helper);
    } else {
      execFileSync('xcrun', [
        'swiftc', ...flags,
        '-D', 'CU_OWNED_KEY_ACCEPTANCE', '-D', 'CU_OWNED_KEY_DEATH_POINTS',
        ...sources, '-o', helper
      ], { cwd: root, encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });
      compiledHelper = helper;
    }
    helperPaths[phase] = helper;
  }
  execFileSync('xcrun', ['swiftc', ...flags,
    join(root, 'native/device/fixtures/KeyFixture.swift'), '-o', fixturePath],
  { cwd: root, encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });
  execFileSync('xcrun', ['swiftc', ...flags,
    join(root, 'native/device/fixtures/KeyboardStateProbe.swift'), '-o', probePath],
  { cwd: root, encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });
  execFileSync('xcrun', ['swiftc', ...flags,
    join(root, 'native/device/fixtures/OwnedKeyStreamProbe.swift'), '-o', streamProbePath],
  { cwd: root, encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });

  const assets = {
    output,
    created_at: new Date().toISOString(),
    helper_paths: helperPaths,
    helper_hashes: Object.fromEntries(phases.map(phase => [phase, sha(helperPaths[phase])])),
    fixture_path: fixturePath,
    fixture_hash: sha(fixturePath),
    fixture_info_plist: join(contents, 'Info.plist'),
    fixture_info_plist_hash: sha(join(contents, 'Info.plist')),
    probe_path: probePath,
    probe_hash: sha(probePath),
    stream_probe_path: streamProbePath,
    stream_probe_hash: sha(streamProbePath),
    source_hashes: sourceHashes()
  };
  writeFileSync(manifestPath, JSON.stringify(assets, null, 2) + '\n');
  return assets;
}

function loadOwnedInputs() {
  assert.ok(output.startsWith(realpathSync(tmpdir()) + '/cu-owned-key-death-'), 'owned helper directory must be a prior temporary build');
  assert.ok(existsSync(manifestPath), 'owned build manifest missing');
  const assets = JSON.parse(readFileSync(manifestPath, 'utf8'));
  assert.equal(assets.output, output, 'owned build directory identity changed');
  assert.deepEqual(assets.source_hashes, sourceHashes(), 'native or harness source changed since helper build');
  assert.equal(assets.fixture_path, fixturePath);
  assert.equal(assets.fixture_hash, sha(fixturePath), 'fixture binary changed since helper build');
  assert.equal(assets.probe_path, probePath);
  assert.equal(assets.probe_hash, sha(probePath), 'system-only probe changed since helper build');
  assert.equal(assets.stream_probe_path, streamProbePath);
  assert.equal(assets.stream_probe_hash, sha(streamProbePath), 'independent stream probe changed since helper build');
  for (const phase of phases) {
    const helper = assets.helper_paths[phase];
    assert.equal(helper, join(output, 'helpers', `owned-key-${phase}`));
    assert.equal(sha(helper), assets.helper_hashes[phase], `owned helper changed: ${phase}`);
  }
  return assets;
}

function child(program, args, label) {
  const proc = spawn(program, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  const entry = { proc, label, lines: [], stdout: '', stderr: '', error: null };
  children.push(entry);
  proc.stdin.on('error', () => {});
  proc.on('error', error => { entry.error = String(error); });
  let pending = '';
  proc.stdout.on('data', chunk => {
    if (entry.stdout.length + chunk.length > 4 * 1024 * 1024) {
      entry.error = 'stdout budget exceeded'; proc.kill(); return;
    }
    const text = chunk.toString('utf8');
    entry.stdout += text; pending += text;
    writeFileSync(join(output, `${label}.stdout.jsonl`), entry.stdout);
    for (let i; (i = pending.indexOf('\n')) >= 0;) {
      const line = pending.slice(0, i); pending = pending.slice(i + 1);
      if (!line) continue;
      try { entry.lines.push(JSON.parse(line)); }
      catch (error) { entry.error = `${label} emitted invalid JSON: ${String(error)}`; }
    }
  });
  proc.stderr.on('data', chunk => {
    if (entry.stderr.length + chunk.length > 128 * 1024) {
      entry.error = 'stderr budget exceeded'; proc.kill(); return;
    }
    entry.stderr += chunk.toString('utf8');
    writeFileSync(join(output, `${label}.stderr.txt`), entry.stderr);
  });
  return entry;
}

function healthy(entry) {
  assert.equal(entry.error, null, entry.error);
  assert.equal(entry.proc.signalCode, null, `${entry.label} terminated by signal`);
  if (entry.proc.exitCode != null && entry.proc.exitCode !== 0) {
    throw new Error(entry.stderr || `${entry.label} exited ${entry.proc.exitCode}`);
  }
}

async function until(read, reason, timeout = 6000, interval = 3) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    const value = read();
    if (value) return value;
    await pause(interval);
  }
  throw new Error(`Timeout: ${reason}`);
}

async function waitLine(entry, predicate, reason, timeout = 6000) {
  return until(() => {
    healthy(entry);
    return entry.lines.find(predicate);
  }, `${entry.label}:${reason}`, timeout);
}

async function fixtureCommand(fixture, name) {
  healthy(fixture);
  const start = fixture.lines.length;
  fixture.proc.stdin.write(name + '\n');
  return waitLine(fixture, item => fixture.lines.indexOf(item) >= start && item.kind === name, name);
}

async function streamCommand(probe, request, kind) {
  healthy(probe);
  const start = probe.lines.length;
  probe.proc.stdin.write(JSON.stringify(request) + '\n');
  const response = await waitLine(probe, item => probe.lines.indexOf(item) >= start
    && (item.kind === kind || item.kind === 'probe_error'), kind, 3000);
  assert.equal(response.kind, kind, response.error ?? 'stream probe failed');
  return response;
}

async function runProbe(probePath, request, label, timeout = 2500) {
  const probe = child(probePath, [], label);
  probe.proc.stdin.end(JSON.stringify(request));
  await until(() => probe.proc.exitCode != null || probe.proc.signalCode != null,
    `${label} exit`, timeout, 1);
  healthy(probe);
  assert.equal(probe.lines.length, 1, `${label} must return one JSON record`);
  const value = probe.lines[0];
  assert.equal(value.kind, 'keyboard_state_probe');
  assert.equal(value.error, undefined, value.error);
  assert.equal(value.process_identity_revalidated, true);
  assert.equal(value.input_authorized, false);
  assert.equal(value.accessibility_used, false);
  assert.equal(value.appkit_used, false);
  return value;
}

function fixtureRequest(ready) {
  return {
    target_kind: 'fixture', pid: ready.pid, expected_parent_pid: process.pid,
    expected_executable_path: fixturePath, owner_directory: output,
    bundle_id: ready.bundle_id, window_id: ready.window_id
  };
}

async function sampleFixture(probePath, ready, label) {
  const sample = await runProbe(probePath, fixtureRequest(ready), label);
  assert.equal(sample.pid, ready.pid);
  assert.equal(Number(sample.kernel_pid), ready.pid);
  assert.equal(sample.parent_pid, process.pid);
  assert.equal(realpathSync(sample.executable_path), realpathSync(fixturePath));
  assert.equal(sample.bundle_id, ready.bundle_id);
  assert.equal(sample.window_id, ready.window_id);
  for (const phase of ['window_cg_before', 'window_cg_after']) {
    assert.equal(Number(sample[phase]?.window_id), ready.window_id);
    assert.equal(Number(sample[phase]?.owner_pid), ready.pid);
    assert.equal(Number(sample[phase]?.layer), 0);
  }
  for (const phase of ['system_keyboard_before', 'system_keyboard_after']) {
    const snapshot = sample[phase];
    assert.ok(snapshot, `${phase} state missing`);
    for (const [name, state] of Object.entries(snapshot)) {
      assert.equal(state.capture_complete, true, `${phase}.${name} did not query all 128 keys`);
      assert.equal(state.key_query_count, 128);
      assert.equal(state.key_query_error_reporting_available, false);
      assert.equal(state.flags_unchanged, true, `${phase}.${name} flags changed during the sample`);
      assert.ok(Array.isArray(state.unobservable) && state.unobservable.length >= 2);
    }
  }
  return sample;
}

function assertKeyboardClear(sample, where) {
  const modifierMask = 0xfb0000;
  for (const phase of ['system_keyboard_before', 'system_keyboard_after']) {
    for (const [name, state] of Object.entries(sample[phase])) {
      assert.deepEqual(state.pressed_virtual_codes, [], `${where}.${phase}.${name} has a pressed key`);
      assert.equal(state.flags_before & modifierMask, 0, `${where}.${phase}.${name} has a modifier or Caps Lock flag`);
      assert.equal(state.flags_after & modifierMask, 0, `${where}.${phase}.${name} has a modifier or Caps Lock flag`);
    }
  }
}

function diagnosticFiles(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory).filter(name => name.endsWith('.json')).sort().map(name => {
    const path = join(directory, name);
    return { name, path, value: JSON.parse(readFileSync(path, 'utf8')) };
  });
}

function stderrDeathPoint(directory, phase) {
  const record = diagnosticFiles(directory).find(item => item.value.kind === 'owned_key_guard_stderr_prefix');
  if (!record) return null;
  assert.equal(record.value.encoding, 'base64');
  const bytes = Buffer.from(record.value.raw_stderr_base64, 'base64');
  assert.ok(bytes.length > 1 && bytes.length <= 1024 && bytes.at(-1) === 10, 'stderr prefix must contain one bounded complete line');
  const text = bytes.subarray(0, bytes.length - 1).toString('utf8');
  const diagnostic = JSON.parse(text);
  assert.equal(diagnostic.kind, 'owned_key_death_point');
  assert.equal(diagnostic.phase, phase);
  assert.ok(Number.isSafeInteger(diagnostic.helper_pid) && diagnostic.helper_pid > 0);
  assert.ok(Number.isSafeInteger(diagnostic.controller_pid) && diagnostic.controller_pid > 0);
  assert.equal(record.value.truncated, false);
  return { record: record.value, file: record.name, bytes: bytes.toString('base64'), diagnostic };
}

async function waitDeathPoint(directory, phase) {
  return until(() => stderrDeathPoint(directory, phase), `${phase} live stderr death point`, 4000, 1);
}

function preserveDiagnostics(directory, label) {
  assert.ok(directory.startsWith('/private/tmp/cu-native-') && directory.endsWith('/key-diagnostics'));
  const metadata = statSync(directory);
  assert.ok(metadata.isDirectory() && (metadata.mode & 0o777) === 0o700, 'diagnostics directory is not private');
  const destination = join(output, `${label}-key-diagnostics`);
  cpSync(directory, destination, { recursive: true, dereference: false,
    filter: entry => entry === directory || entry.endsWith('.json') });
  const records = {};
  for (const item of diagnosticFiles(destination)) {
    records[item.name] = { sha256: sha(item.path), value: item.value };
  }
  return { path: destination, records };
}

function preserveSourceAssets(directory, label) {
  assert.ok(directory.startsWith('/private/tmp/cu-native-') && directory.endsWith('/source-assets'));
  const destination = join(output, `${label}-source-assets`);
  cpSync(directory, destination, { recursive: true, dereference: false });
  const hashes = {};
  const visit = current => {
    for (const name of readdirSync(current)) {
      const path = join(current, name);
      const metadata = statSync(path);
      if (metadata.isDirectory()) visit(path);
      else if (metadata.isFile()) hashes[path.slice(destination.length + 1)] = sha(path);
    }
  };
  visit(destination);
  return { path: destination, hashes };
}

function rawEvents(fixture, start) {
  return fixture.lines.slice(start).filter(value => value.kind === 'raw_key_event');
}

async function waitRawEvents(fixture, start, phase) {
  if (phase === 'before-down') {
    await pause(80);
    const events = rawEvents(fixture, start);
    assert.equal(events.length, 0, 'before-down helper death unexpectedly delivered a fixture key event');
    return events;
  }
  const expectedDown = 1;
  await until(() => rawEvents(fixture, start).some(event => event.cg_type === 10 && event.key_code === 49),
    `${phase} actual fixture key-down`, 1200, 3);
  if (phase !== 'between-down-up') {
    await until(() => {
      const events = rawEvents(fixture, start);
      return events.some(event => event.cg_type === 10 && event.key_code === 49)
        && events.some(event => event.cg_type === 11 && event.key_code === 49);
    }, `${phase} actual fixture key-up`, 1200, 3);
    // Observation after terminal input does not extend an input deadline.
    // Keep the receiver alive to expose a delayed second delivery/effect.
    await pause(350);
  }
  const events = rawEvents(fixture, start);
  assert.equal(events.filter(event => event.cg_type === 10 && event.key_code === 49).length, expectedDown);
  if (phase === 'between-down-up') {
    assert.equal(events.length, 1, 'partial-pair case observed unexpected fixture key events');
    assert.equal(events.filter(event => event.cg_type === 11 && event.key_code === 49).length, 0);
  } else {
    assert.equal(events.length, 2, 'complete-pair case observed unexpected fixture key events');
    assert.equal(events.filter(event => event.cg_type === 11 && event.key_code === 49).length, 1);
    const down = events.find(event => event.cg_type === 10 && event.key_code === 49);
    const up = events.find(event => event.cg_type === 11 && event.key_code === 49);
    assert.notEqual(down.marker, '0');
    assert.equal(down.marker, up.marker);
    assert.ok(BigInt(up.cg_timestamp) > BigInt(down.cg_timestamp), 'actual key pair timestamps must increase');
    assert.equal(down.flags, up.flags);
    assert.equal(down.autorepeat, false);
    assert.equal(up.autorepeat, false);
  }
  return events;
}

async function runScenario(assets, fixture, fixtureReady, phase, phaseIndex) {
  const label = `death-${phase}`;
  const helper = assets.helper_paths[phase];
  const helperHash = assets.helper_hashes[phase];
  const scopePath = join(output, `${label}-scope.json`);
  const before = await sampleFixture(assets.probe_path, fixtureReady, `${label}-before-probe`);
  assertKeyboardClear(before, `${label} preflight`);
  const item = {
    phase, phase_index: phaseIndex, helper, helper_sha256: helperHash,
    device_sha256: record.build.device_binary_sha256,
    runner_sha256: record.build.runner_binary_sha256,
    source_sha256: record.build.source_binary_sha256,
    runner_scenario: 'key-unknown', ok: false,
    phase_semantics: phase === 'waiting-ack'
      ? 'SIGKILL occurs after down/up posts and before confirmation polling; ACK absence is not inferred.'
      : phase === 'between-down-up'
        ? 'SIGKILL occurs after down post and before up post; pair completion and release remain unknown.'
        : null
  };
  record.scenarios.push(item); saveRecord();
  writeFileSync(scopePath, JSON.stringify({
    pid: fixtureReady.pid, bundle_id: fixtureReady.bundle_id,
    window_id: `window-${fixtureReady.window_id}`, label: 'Key fixture toggle', session_epoch: 7
  }) + '\n');

  const stream = values['stream-probe']
    ? child(assets.stream_probe_path, [], `${label}-stream-probe`) : null;
  if (stream) {
    item.stream_probe_ready = await waitLine(stream, value => value.kind === 'ready', 'ready', 3000);
    assert.equal(item.stream_probe_ready.probe_pid, stream.proc.pid);
  }

  const session = child(runner, [device, helper, source, scopePath, 'key-unknown'], `${label}-runner`);
  item.runner_pid = session.proc.pid;
  const started = await waitLine(session, value => value.kind === 'device_started', 'device_started', 5000);
  item.device_started = started;
  item.actual_device = await runProbe(assets.probe_path, {
    target_kind: 'device', pid: started.pid, expected_parent_pid: session.proc.pid,
    expected_executable_path: device, owner_directory: root
  }, `${label}-device-probe`);
  assert.equal(item.actual_device.executable_path, device);
  if (stream) {
    item.stream_probe_configured = await streamCommand(stream, {
      op: 'configure', fixture_pid: fixtureReady.pid, fixture_parent_pid: process.pid,
      fixture_executable_path: assets.fixture_path, window_id: fixtureReady.window_id,
      owner_directory: output, helper_parent_pid: started.pid,
      helper_parent_executable_path: device, helper_executable_path: helper
    }, 'configured');
    const configured = item.stream_probe_configured;
    assert.equal(configured.input_authorized, false);
    assert.equal(configured.input_posted, false);
    assert.equal(configured.fixture_identity?.pid, fixtureReady.pid);
    assert.equal(configured.fixture_window_id, fixtureReady.window_id);
    assert.equal(configured.fixture_window_count, 1);
    assert.equal(configured.helper_parent_identity?.pid, started.pid);
    assert.equal(configured.helper_child_identity_unique, true);
    assert.equal(configured.tap_location, 'cg_session');
    assert.equal(configured.tap_option, 'listen_only');
    assert.equal(configured.tap_enabled, true);
  }
  const ready = await waitLine(session, value => value.kind === 'ready', 'ready', 10000);
  const keyReady = await waitLine(session, value => value.kind === 'key_ready'
    || value.kind === 'key_session_result', 'key_ready or preparation failure', 15000);
  if (keyReady.kind === 'key_session_result') {
    item.result = keyReady;
    const prefix = 'CU_SOURCE_PREPARATION_GATE_DIAGNOSTIC ';
    const diagnostic = session.stderr.split(/\r?\n/).find(line => line.startsWith(prefix));
    if (diagnostic) {
      try {
        assert.ok(Buffer.byteLength(diagnostic, 'utf8') <= 2048);
        item.source_preparation_gate_diagnostic = JSON.parse(diagnostic.slice(prefix.length));
      } catch (error) {
        item.source_preparation_gate_diagnostic_error = String(error);
      }
    }
    item.raw_fixture_events = rawEvents(fixture, 0);
    if (keyReady.source_assets_dir) item.source_assets = preserveSourceAssets(keyReady.source_assets_dir, label);
    if (keyReady.key_diagnostics_dir) item.diagnostics = preserveDiagnostics(keyReady.key_diagnostics_dir, label);
    saveRecord();
    throw new Error(keyReady.error ?? 'key preparation ended without an input ticket');
  }
  item.ready = ready;
  item.key_ready = keyReady;
  assert.equal(keyReady.physical_input_submitted, false);
  assert.equal(keyReady.device_pid, started.pid, 'key_ready did not identify this actual cu-device child');
  assert.ok(Number.isSafeInteger(keyReady.device_pid) && keyReady.device_pid > 0);
  const diagnosticsDir = keyReady.key_diagnostics_dir;
  const sourceAssetsDir = keyReady.source_assets_dir;
  assert.ok(diagnosticsDir.startsWith('/private/tmp/cu-native-') && diagnosticsDir.endsWith('/key-diagnostics'));
  assert.ok(sourceAssetsDir.startsWith('/private/tmp/cu-native-') && sourceAssetsDir.endsWith('/source-assets'));
  assert.equal(item.actual_device.pid, keyReady.device_pid);
  const killStartRaw = fixture.lines.length;
  session.proc.stdin.write('submit\n');
  const death = await waitDeathPoint(diagnosticsDir, phase);
  item.death_point = death;
  const diagnostic = death.diagnostic;
  assert.equal(diagnostic.controller_pid, keyReady.device_pid,
    'death point parent is not the actual cu-device process');
  assert.equal(diagnostic.actor_pid, diagnostic.helper_pid);
  assert.equal(diagnostic.key_code, 49);
  assert.ok(typeof diagnostic.nonce === 'string' && BigInt(diagnostic.nonce) > 0n);
  assert.equal(diagnostic.down_post_calls, phase === 'before-down' ? 0 : 1);
  assert.equal(diagnostic.up_post_calls, phase === 'after-up' || phase === 'waiting-ack' ? 1 : 0);
  const observedAt = Date.now();
  const helperProbe = await runProbe(assets.probe_path, {
    target_kind: 'helper', pid: diagnostic.helper_pid,
    expected_parent_pid: keyReady.device_pid,
    expected_executable_path: helper, owner_directory: output
  }, `${label}-helper-probe`, 100);
  item.actual_helper = {
    pid: diagnostic.helper_pid,
    parent_pid: helperProbe.parent_pid,
    executable_path: helperProbe.executable_path,
    executable_sha256: helperHash,
    process_start_seconds: helperProbe.process_start_seconds,
    process_start_microseconds: helperProbe.process_start_microseconds,
    identity: helperProbe
  };
  if (stream) {
    const identity = item.stream_probe_configured.helper_identity;
    assert.equal(identity.pid, diagnostic.helper_pid);
    assert.equal(identity.parent_pid, keyReady.device_pid);
    assert.equal(realpathSync(identity.executable_path), realpathSync(helper));
    assert.equal(Number(identity.start_seconds), helperProbe.process_start_seconds);
    assert.equal(Number(identity.start_microseconds), helperProbe.process_start_microseconds);
  }
  assert.equal(item.actual_helper.executable_sha256, helperHash);
  if (values['pre-kill-capture']) {
    // Readonly settling stays inside the existing death-point kill bound.
    // Neither the original input deadline nor the ACK cutoff is renewed.
    await pause(50);
    const [streamResult, fixtureResult] = await Promise.allSettled([
      streamCommand(stream, { op: 'snapshot', label: 'helper-alive-before-kill' }, 'snapshot'),
      fixtureCommand(fixture, 'stats')
    ]);
    item.pre_kill_capture = {
      readonly_settle_ms: 50,
      elapsed_since_death_point_observed_ms: Date.now() - observedAt,
      independent_stream_snapshot: streamResult.status === 'fulfilled' ? streamResult.value : null,
      fixture_stats: fixtureResult.status === 'fulfilled' ? fixtureResult.value : null,
      raw_fixture_events: rawEvents(fixture, killStartRaw),
      errors: [streamResult, fixtureResult].filter(value => value.status === 'rejected').map(value => String(value.reason))
    };
    const captured = item.pre_kill_capture.independent_stream_snapshot;
    const scoped = captured?.helper_identity_live_after_snapshot === true &&
      captured.event_stream_capture_complete === true && captured.tap_enabled === true &&
      captured.target_scope_revalidated === true && captured.capture_overflow === false &&
      captured.capture_window_expired === false && captured.tap_disabled_type === null &&
      captured.source_pid_unresolved_count === 0;
    item.pre_kill_capture.capture_resolution = scoped ? 'scoped_capture_complete' : 'capture_unresolved';
    item.pre_kill_capture.lifecycle_attribution_allowed = scoped;
    saveRecord();
    if (streamResult.status === 'rejected') throw streamResult.reason;
    if (fixtureResult.status === 'rejected') throw fixtureResult.reason;
    assert.equal(streamResult.value.helper_identity_live_after_snapshot, true,
      'pre-kill capture did not prove the original helper is still alive');
  }
  assert.ok(Date.now() - observedAt < 150, 'live stderr was not acted on within the death-point window');
  process.kill(diagnostic.helper_pid, 'SIGKILL');
  item.actual_helper.kill_signal_sent = 9;
  item.actual_helper.parent_is_actual_cu_device = true;
  saveRecord();

  const resultPromise = waitLine(session, value => value.kind === 'key_session_result', 'key_session_result', 10000);
  const rawPromise = waitRawEvents(fixture, killStartRaw, phase);
  const [resultSettled, rawSettled] = await Promise.allSettled([resultPromise, rawPromise]);
  const actualEvents = rawEvents(fixture, killStartRaw);
  item.raw_fixture_events = actualEvents;
  item.returned_sender_post_calls = {
    down: diagnostic.down_post_calls, up: diagnostic.up_post_calls,
    nonce: diagnostic.nonce, down_timestamp_ns: diagnostic.down_timestamp_ns,
    up_timestamp_ns: diagnostic.up_timestamp_ns,
    measurement: 'pair-owned cumulative counters incremented after each CGEvent.post call returns; delivery is independently observed'
  };
  item.fixture_nonces_match_counted_pair = actualEvents.every(event => event.marker === diagnostic.nonce);
  item.source_assets = preserveSourceAssets(sourceAssetsDir, label);
  item.diagnostics = preserveDiagnostics(diagnosticsDir, label);
  if (resultSettled.status === 'fulfilled') item.result = resultSettled.value;
  if (rawSettled.status === 'rejected') item.raw_event_error = String(rawSettled.reason);
  saveRecord();
  assert.equal(item.fixture_nonces_match_counted_pair, true,
    'fixture event nonce differs from the live counted pair');
  if (resultSettled.status === 'rejected') throw resultSettled.reason;
  const result = resultSettled.value;
  item.receipt = result.receipt;
  assert.equal(result.error, undefined, result.error);
  assert.equal(result.receipt?.status, 'unknown', 'helper death must settle the submitted action as unknown');
  assert.equal(result.isolation, true, 'unknown key action must isolate the channel');
  assert.equal(result.channel_isolated, true);
  assert.equal(result.no_replay, true);
  assert.equal(result.input_replayed, false);
  item.no_replay = result.no_replay && !result.input_replayed;
  item.isolation = result.isolation;
  item.packet = {
    intent_id: keyReady.intent.intent_id,
    submitted_to_device: session.lines.some(value => value.kind === 'device_request_enqueued'
      && value.intent_id === keyReady.intent.intent_id),
    helper_death_point: diagnostic.phase,
    core_message_kinds: session.lines.filter(value => value.kind === 'core_message')
      .map(value => value.message?.Receipt?.status ?? value.message?.kind ?? Object.keys(value.message ?? {})[0])
  };
  assert.equal(item.packet.submitted_to_device, true);

  const exitRecord = await until(() => diagnosticFiles(diagnosticsDir)
    .map(value => value.value)
    .find(value => value.kind === 'owned_key_guard_helper_exit'),
  `${phase} parent wait status`, 2500, 2);
  assert.equal(exitRecord.signal, 9, 'actual cu-device wait status did not report SIGKILL');
  item.actual_helper.exit = {
    source: 'actual cu-device Child::try_wait status',
    category: exitRecord.category,
    signal: exitRecord.signal,
    exit_code: exitRecord.exit_code,
    verified: true
  };
  saveRecord();
  if (stream) {
    item.independent_stream_snapshot = await streamCommand(stream, { op: 'snapshot', label: 'post-kill' }, 'snapshot');
    saveRecord();
  }
  const after = await sampleFixture(assets.probe_path, fixtureReady, `${label}-after-probe`);
  item.system_keyboard_before_after = { before, after };
  item.fixture_process_identity_stable = before.process_start_seconds === after.process_start_seconds
    && before.process_start_microseconds === after.process_start_microseconds
    && before.pid === after.pid && before.parent_pid === after.parent_pid
    && before.executable_path === after.executable_path
    && before.window_id === after.window_id;
  assert.equal(item.fixture_process_identity_stable, true);
  if (phase !== 'between-down-up') {
    assertKeyboardClear(after, `${label} postflight`);
  }
  item.source_assets = preserveSourceAssets(result.source_assets_dir ?? sourceAssetsDir, label);
  item.diagnostics = preserveDiagnostics(diagnosticsDir, label);
  saveRecord();
  if (rawSettled.status === 'rejected') throw rawSettled.reason;
  item.reported_pair_missing = phase === 'between-down-up';
  item.release_confirmed = phase !== 'between-down-up'
    && actualEvents.some(value => value.cg_type === 10 && value.key_code === 49)
    && actualEvents.some(value => value.cg_type === 11 && value.key_code === 49);
  if (phase === 'between-down-up') {
    item.incomplete = true;
    item.release_confirmed = false;
    item.system_clear_does_not_prove_release = true;
    item.no_following_input_started = true;
    item.isolation = true;
  } else if (phase === 'before-down') {
    item.no_key_event_delivered = actualEvents.length === 0;
    item.release_required = false;
    assert.equal(item.no_key_event_delivered, true);
  } else {
    assert.equal(item.release_confirmed, true, `${phase} fixture did not observe the complete raw down/up pair`);
  }
  session.proc.stdin.write('finish\n');
  await until(() => session.proc.exitCode != null || session.proc.signalCode != null,
    `${phase} runner finish`, 4000, 3);
  healthy(session);
  item.runner_exit_code = session.proc.exitCode;
  if (stream) {
    item.independent_stream_final_snapshot = await streamCommand(stream, { op: 'snapshot', label: 'after-runner-exit' }, 'snapshot');
    item.stream_probe_quit = await streamCommand(stream, { op: 'quit' }, 'quit_result');
  }
  item.ok = phase === 'between-down-up' ? item.incomplete && item.isolation && item.no_following_input_started
    : result.receipt.status === 'unknown' && item.isolation && item.no_replay
      && (phase === 'before-down' ? item.no_key_event_delivered : item.release_confirmed);
  saveRecord();
  return item;
}

function buildRecordForRun(assets) {
  return {
    owned_build_directory: output,
    owned_helper_hashes: assets.helper_hashes,
    fixture_path: assets.fixture_path,
    fixture_sha256: assets.fixture_hash,
    keyboard_state_probe_path: assets.probe_path,
    keyboard_state_probe_sha256: assets.probe_hash,
    independent_stream_probe_path: assets.stream_probe_path,
    independent_stream_probe_sha256: assets.stream_probe_hash,
    source_hashes: assets.source_hashes,
    device_binary_sha256: sha(device),
    source_binary_sha256: sha(source),
    runner_binary_sha256: sha(runner)
  };
}

async function runMatrix(assets) {
  for (const path of [device, source, runner]) assert.ok(existsSync(path), `required prebuilt executable missing: ${path}`);
  const fixture = child(assets.fixture_path, [], 'keyboard-death-fixture');
  const ready = await waitLine(fixture, value => value.kind === 'ready', 'fixture ready', 20000);
  assert.equal(ready.pid, fixture.proc.pid);
  assert.equal(ready.bundle_id, 'com.lingxi.computer-user.guard-fixture');
  const before = await sampleFixture(assets.probe_path, ready, 'keyboard-death-initial-probe');
  assertKeyboardClear(before, 'initial keyboard preflight');
  const baselineRawEvents = rawEvents(fixture, 0);
  assert.equal(baselineRawEvents.length, 0, 'fixture observed ambient keyboard input before the matrix');
  record.fixture = {
    ready,
    executable_path: assets.fixture_path,
    executable_sha256: assets.fixture_hash,
    bundle_id: ready.bundle_id,
    pid: ready.pid,
    parent_pid: process.pid,
    initial_probe: before
  };
  record.build_and_process_identity = record.build;
  record.fixture_root = {
    output,
    app_bundle: join(output, 'KeyFixture.app'),
    info_plist_sha256: assets.fixture_info_plist_hash,
    helper_directory: join(output, 'helpers')
  };
  saveRecord();
  try {
    await fixtureCommand(fixture, 'focus-window');
    await fixtureCommand(fixture, 'focus-toggle');
    const focused = await fixtureCommand(fixture, 'stats');
    assert.equal(focused.toggle_focused, true);
    assert.equal(focused.value, false);
    assert.equal(rawEvents(fixture, 0).length, 0, 'fixture setup unexpectedly received raw keyboard input');
    for (const [index, phase] of runPhases.entries()) {
      await runScenario(assets, fixture, ready, phase, index);
      if (phase !== 'between-down-up') {
        await fixtureCommand(fixture, 'focus-window');
        await fixtureCommand(fixture, 'focus-toggle');
      }
    }
    record.owned_keyboard_death_matrix_passed = !values.phase;
    record.owned_keyboard_death_diagnostic_passed = Boolean(values.phase);
    record.ok = true;
  } finally {
    // No key-up is synthesized here. The partial-pair case is last and stays
    // incomplete even when an independent system-state sample looks clear.
    if (fixture.proc.exitCode == null && fixture.proc.signalCode == null) {
      fixture.proc.stdin.end('quit\n');
      await pause(100);
      if (fixture.proc.exitCode == null && fixture.proc.signalCode == null) fixture.proc.kill('SIGTERM');
    }
  }
}

async function main() {
  mkdirSync(dirname(recordPath), { recursive: true });
  if (!values.run) {
    const assets = buildOwnedInputs();
    record.build_only = true;
    record.owned_build_manifest = manifestPath;
    record.helper_hashes = assets.helper_hashes;
    record.fixture_hash = assets.fixture_hash;
    record.probe_hash = assets.probe_hash;
    record.source_hashes = assets.source_hashes;
    record.ok = true;
    writeFileSync(join(output, 'build-only-record.json'), JSON.stringify(record, null, 2) + '\n');
    process.stdout.write(JSON.stringify({ ok: true, build_only: true, output, manifest: manifestPath,
      helper_paths: assets.helper_paths, helper_hashes: assets.helper_hashes, fixture_path: fixturePath,
      probe_path: probePath }) + '\n');
    return;
  }

  if (existsSync(recordPath)) {
    const previous = recordPath.replace('.json', `.previous-${randomUUID()}.json`);
    copyFileSync(recordPath, previous);
    record.previous_artifact = previous;
  }
  const assets = loadOwnedInputs();
  record.build = buildRecordForRun(assets);
  saveRecord();
  await runMatrix(assets);
}

try {
  await main();
} catch (error) {
  record.error = String(error?.stack ?? error);
  record.finished_at = new Date().toISOString();
  if (values.run) saveRecord();
  process.stderr.write(record.error + '\n');
  process.exitCode = 1;
} finally {
  for (const entry of children.slice().reverse()) {
    if (entry.proc.exitCode != null || entry.proc.signalCode != null) continue;
    if (entry.label.endsWith('-runner')) {
      entry.proc.stdin.end('finish\n');
      await pause(1500);
      if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGTERM');
    } else if (entry.label.endsWith('-stream-probe')) {
      entry.proc.stdin.end(JSON.stringify({ op: 'quit' }) + '\n');
      await pause(200);
      if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGTERM');
    } else if (entry.label === 'keyboard-death-fixture') {
      entry.proc.stdin.end('quit\n');
      await pause(100);
      if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGTERM');
    } else {
      entry.proc.kill('SIGTERM');
    }
  }
  if (values.run) {
    const fixture = children.find(entry => entry.label === 'keyboard-death-fixture');
    if (fixture) {
      const events = rawEvents(fixture, 0);
      const occurrences = new Map();
      for (const event of events) {
        const identity = JSON.stringify([event.marker, event.cg_type, event.key_code,
          event.cg_timestamp, event.flags, event.autorepeat, event.window_id]);
        occurrences.set(identity, (occurrences.get(identity) ?? 0) + 1);
      }
      record.final_raw_fixture_events = events;
      record.final_toggle_activations = fixture.lines.filter(value => value.kind === 'toggle_activation');
      record.repeated_delivery_detected = [...occurrences.values()].some(count => count > 1);
      for (const item of record.scenarios) {
        const nonces = new Set((item.raw_fixture_events ?? []).map(event => event.marker));
        item.final_raw_fixture_events = events.filter(event => nonces.has(event.marker));
        item.repeated_delivery_detected = item.final_raw_fixture_events.some(event => {
          const identity = JSON.stringify([event.marker, event.cg_type, event.key_code,
            event.cg_timestamp, event.flags, event.autorepeat, event.window_id]);
          return occurrences.get(identity) > 1;
        });
        if (item.actual_helper?.kill_signal_sent === 9) {
          const expectedDown = item.phase === 'before-down' ? 0 : 1;
          const expectedUp = item.phase === 'after-up' || item.phase === 'waiting-ack' ? 1 : 0;
          item.final_delivery_counts = {
            down: item.final_raw_fixture_events.filter(event => event.cg_type === 10 && event.key_code === 49).length,
            up: item.final_raw_fixture_events.filter(event => event.cg_type === 11 && event.key_code === 49).length,
            expected_down: expectedDown, expected_up: expectedUp
          };
          item.final_delivery_count_mismatch = item.final_delivery_counts.down !== expectedDown
            || item.final_delivery_counts.up !== expectedUp
            || item.final_raw_fixture_events.length !== expectedDown + expectedUp;
        }
        if (item.repeated_delivery_detected || item.final_delivery_count_mismatch) item.ok = false;
      }
      const expectedRawCount = record.scenarios.filter(item => item.actual_helper?.kill_signal_sent === 9)
        .reduce((count, item) => count + (item.phase === 'before-down' ? 0
          : item.phase === 'between-down-up' ? 1 : 2), 0);
      record.final_raw_fixture_event_count_expected = expectedRawCount;
      record.final_delivery_count_mismatch = events.length !== expectedRawCount
        || record.scenarios.some(item => item.final_delivery_count_mismatch);
      if (record.repeated_delivery_detected || record.final_delivery_count_mismatch) {
        record.ok = false;
        record.owned_keyboard_death_matrix_passed = false;
        record.physical_no_repeated_delivery_verified = false;
        record.error ??= 'Final receiver delivery counts differ from the single committed phase.';
      }
    }
    record.finished_at ??= new Date().toISOString();
    saveRecord();
    process.stdout.write(JSON.stringify({ ok: record.ok, record_path: recordPath,
      output, scenarios: record.scenarios.length, error: record.error }) + '\n');
  }
}
