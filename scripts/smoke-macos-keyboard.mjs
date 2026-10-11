#!/usr/bin/env node
// Physical keyboard input is confined to the fixture owned by this process.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: { 'skip-build': { type: 'boolean', default: false },
  'build-only': { type: 'boolean', default: false }, 'owned-helper-dir': { type: 'string' } } });
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = mkdtempSync(join(tmpdir(), 'cu-owned-keyboard-'));
const record = join(root, 'artifacts/m2-native-keyboard.json');
const device = join(root, 'target/debug/cu-device'), source = join(root, 'target/debug/cu-macos-source');
const runner = join(root, 'target/debug/examples/native_key_session');
const production = join(root, 'target/debug/cu-macos-device-guard');
const owned = join(output, 'owned-key-acceptance'), missingAck = join(output, 'owned-key-drop-ack');
const evidence = { ok: false, output, started_at: new Date().toISOString(), scenarios: [],
  production_keyboard_enabled: false, mvp_complete: false, real_provider_calls: 0 };
const children = [];
mkdirSync(dirname(record), { recursive: true });
if (existsSync(record)) {
  const previous = record.replace('.json', `.previous-${randomUUID()}.json`);
  copyFileSync(record, previous); evidence.previous_artifact = previous;
}
const save = () => writeFileSync(record, JSON.stringify(evidence, null, 2) + '\n');
const sha = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(read, reason, timeout = 6000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const value = read(); if (value) return value; await pause(5); }
  throw new Error('Timeout: ' + reason);
}
function child(program, args, label) {
  const proc = spawn(program, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  const entry = { proc, label, lines: [], stdout: '', stderr: '', error: null };
  children.push(entry); proc.stdin.on('error', () => {});
  proc.on('error', error => { entry.error = String(error); });
  let pending = '';
  proc.stdout.on('data', chunk => {
    if (entry.stdout.length + chunk.length > 2 * 1024 * 1024) { entry.error = 'stdout budget exceeded'; proc.kill(); return; }
    entry.stdout += chunk; pending += chunk; writeFileSync(join(output, label + '.stdout.jsonl'), entry.stdout);
    for (let i; (i = pending.indexOf('\n')) >= 0;) {
      try { entry.lines.push(JSON.parse(pending.slice(0, i))); } catch (error) { entry.error = String(error); }
      pending = pending.slice(i + 1);
    }
  });
  proc.stderr.on('data', chunk => {
    if (entry.stderr.length + chunk.length > 64 * 1024) { entry.error = 'stderr budget exceeded'; proc.kill(); return; }
    entry.stderr += chunk; writeFileSync(join(output, label + '.stderr.txt'), entry.stderr);
  });
  return entry;
}
function healthy(entry) {
  assert.equal(entry.error, null, entry.error);
  assert.equal(entry.proc.signalCode, null, entry.label + ' terminated by signal');
  if (entry.proc.exitCode != null && entry.proc.exitCode !== 0) throw new Error(entry.stderr || `${entry.label} exited ${entry.proc.exitCode}`);
}
async function line(entry, kind, timeout = 6000) {
  return until(() => { healthy(entry); return entry.lines.find(item => item.kind === kind); }, entry.label + ':' + kind, timeout);
}
async function command(fixture, name) {
  healthy(fixture); const start = fixture.lines.length; fixture.proc.stdin.write(name + '\n');
  return until(() => { healthy(fixture); return fixture.lines.slice(start).find(item => item.kind === name); }, name);
}
async function readSystem(ready, fixturePath, probePath, label) {
  const request = { pid: ready.pid, bundle_id: ready.bundle_id, executable_path: fixturePath,
    parent_pid: process.pid, window_id: ready.window_id };
  writeFileSync(join(output, label + '.request.json'), JSON.stringify(request) + '\n');
  const probe = child(probePath, [], label); probe.proc.stdin.end(JSON.stringify(request));
  await until(() => { healthy(probe); return probe.proc.exitCode != null; }, label + ' exit');
  assert.equal(probe.lines.length, 1); const response = probe.lines[0];
  assert.equal(response.pid, ready.pid); assert.equal(response.process_identity_revalidated, true);
  for (const phase of ['system_keyboard_before', 'system_keyboard_after']) {
    assert.ok(response[phase], 'independent system state missing');
    for (const sourceState of Object.values(response[phase])) {
      if (typeof sourceState !== 'object' || sourceState == null || !('pressed_virtual_codes' in sourceState)) continue;
      assert.equal(sourceState.capture_complete, true);
      assert.deepEqual(sourceState.pressed_virtual_codes, [], 'system key remains held');
      assert.equal(sourceState.flags_unchanged, true);
      assert.equal(sourceState.flags_before & 0xfb0000, 0, 'momentary modifier or Caps Lock is set');
    }
  }
  return response;
}
function preserveAssets(path, label) {
  assert.ok(path && path.includes('/source-assets'), 'missing actual Source asset directory');
  const destination = join(output, label + '-source-assets'); cpSync(path, destination, { recursive: true, dereference: false });
  const hashes = {};
  for (const name of readdirSync(destination)) {
    const item = join(destination, name); if (statSync(item).isFile()) hashes[name] = sha(item);
  }
  return { path: destination, hashes };
}
function preserveDiagnostics(path, label) {
  if (!path) return null;
  assert.ok(path.includes('/cu-native-') && path.endsWith('/key-diagnostics'));
  const destination = join(output, label + '-key-diagnostics');
  cpSync(path, destination, { recursive: true, dereference: false,
    filter: entry => entry === path || entry.endsWith('.json') });
  const records = {};
  for (const name of readdirSync(destination)) {
    if (!name.endsWith('.json')) continue;
    const file = join(destination, name);
    assert.ok(statSync(file).isFile());
    records[name] = { sha256: sha(file), value: JSON.parse(readFileSync(file, 'utf8')) };
  }
  return { path: destination, records };
}
save();
try {
  mkdirSync(join(root, 'target/native-macos'), { recursive: true });
  const directory = mkdtempSync(join(root, 'target/native-macos/key-acceptance-owned-'));
  const contents = join(directory, 'KeyFixture.app/Contents');
  const fixturePath = join(contents, 'MacOS/KeyFixture'), probePath = join(directory, 'KeyReadProbe');
  mkdirSync(dirname(fixturePath), { recursive: true });
  writeFileSync(join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.lingxi.computer-user.guard-fixture</string>
<key>CFBundleExecutable</key><string>KeyFixture</string><key>CFBundleName</key><string>Computer Use Key Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
  const flags = ['-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(output, 'module-cache')];
  if (!values['skip-build']) {
    execFileSync('cargo', ['build', '--locked', '--offline', '-p', 'device-macos', '--bin', 'cu-device', '--example', 'native_key_session'],
      { cwd: root, encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });
    execFileSync('bash', ['scripts/build-macos-device.sh'], { cwd: root, encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });
    execFileSync('bash', ['scripts/build-macos-source.sh'], { cwd: root, encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });
  }
  const nativeSources = readdirSync(join(root, 'native/device')).filter(name => name.endsWith('.swift')).sort().map(name => join(root, 'native/device', name));
  if (values['owned-helper-dir']) {
    const directory = resolve(values['owned-helper-dir']);
    assert.ok(directory.includes('/cu-owned-keyboard-'), 'only a previous owned build may be reused');
    const matching = readdirSync(dirname(record)).filter(name => name.startsWith('m2-native-keyboard') && name.endsWith('.json'))
      .map(name => JSON.parse(readFileSync(join(dirname(record), name), 'utf8'))).find(value => value.output === directory && value.build_hashes);
    assert.ok(matching, 'previous owned build record required');
    for (const path of nativeSources) assert.equal(sha(path), matching.source_hashes[path.slice(root.length + 1)], 'native source changed since owned build');
    for (const destination of [owned, missingAck]) {
      const old = join(directory, destination.slice(output.length + 1));
      assert.equal(sha(old), matching.build_hashes[old], 'previous helper binary changed'); copyFileSync(old, destination);
    }
    evidence.reused_owned_build = directory;
  } else {
    for (const [binary, definitions] of [[owned, ['CU_OWNED_KEY_ACCEPTANCE']], [missingAck, ['CU_OWNED_KEY_ACCEPTANCE', 'CU_OWNED_KEY_DROP_ACK']]]) {
      execFileSync('xcrun', ['swiftc', ...flags, ...definitions.flatMap(name => ['-D', name]), ...nativeSources, '-o', binary],
        { encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });
    }
  }
  for (const [name, binary] of [['KeyFixture', fixturePath], ['KeyReadProbe', probePath]]) {
    execFileSync('xcrun', ['swiftc', ...flags, join(root, 'native/device/fixtures', name + '.swift'), '-o', binary],
      { encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });
  }
  evidence.build_hashes = Object.fromEntries([device, source, runner, production, owned, missingAck, fixturePath, probePath].map(path => [path, sha(path)]));
  evidence.source_hashes = Object.fromEntries(['scripts/smoke-macos-keyboard.mjs', 'native/device/fixtures/KeyFixture.swift',
    'native/device/fixtures/KeyReadProbe.swift', 'crates/device-macos/examples/native_key_session.rs',
    'crates/device-macos/src/guard.rs', ...nativeSources.map(path => path.slice(root.length + 1))].map(path => [path, sha(join(root, path))]));
  evidence.schema_sha256 = sha(join(root, 'contracts/desktop-v2/schema.json')); save();
  if (!values['build-only']) {
    for (const [label, helper, scenario, fault] of [
      ['normal', owned, 'key-success', null],
      ['production-disabled', production, 'key-unavailable', null],
      ['focus-changed', owned, 'key-rejected', 'focus-text'],
      ['missing-ack', missingAck, 'key-replay', null]
    ]) {
      const item = { label, helper, scenario, ok: false }; evidence.scenarios.push(item); save();
      const fixture = child(fixturePath, [], label + '-fixture');
      item.fixture_pid = fixture.proc.pid; save();
      // A fresh AppKit bundle can take longer to register with LaunchServices.
      // This startup wait precedes all input tickets and does not extend them.
      const ready = await line(fixture, 'ready', 20000); assert.equal(ready.pid, fixture.proc.pid);
      await command(fixture, 'focus-window'); item.before = await command(fixture, 'focus-toggle');
      assert.equal(item.before.raw_key_event_count, 0); assert.equal(item.before.value, false);
      item.system_before = await readSystem(ready, fixturePath, probePath, label + '-before');
      const scopePath = join(output, label + '-scope.json');
      writeFileSync(scopePath, JSON.stringify({ pid: ready.pid, bundle_id: ready.bundle_id, window_id: `window-${ready.window_id}`,
        label: 'Key fixture toggle', session_epoch: 7 }) + '\n');
      const session = child(runner, [device, helper, source, scopePath, scenario], label + '-runner');
      const pending = await until(() => {
        const failure = session.lines.find(value => value.kind === 'key_session_result' && value.error);
        if (failure) return failure;
        healthy(session); return session.lines.find(value => value.kind === 'key_ready');
      // Let the Source's own 12s read-only acquisition deadline return its
      // diagnostic result before the outer harness closes the child.
      }, label + ' key ready', 15000);
      if (pending.error) {
        item.result = pending; item.assets = preserveAssets(pending.source_assets_dir, label);
        item.diagnostics = preserveDiagnostics(pending.key_diagnostics_dir, label); save();
        session.proc.stdin.write('finish\n'); throw new Error(pending.error);
      }
      item.key_ready = pending; save();
      if (fault) item.fault_setup = await command(fixture, fault);
      session.proc.stdin.write('submit\n');
      const result = await line(session, 'key_session_result'); item.result = result;
      item.after = await command(fixture, 'stats');
      item.assets = preserveAssets(result.source_assets_dir, label);
      item.diagnostics = preserveDiagnostics(result.key_diagnostics_dir, label);
      item.system_after = await readSystem(ready, fixturePath, probePath, label + '-after'); save();
      session.proc.stdin.write('finish\n');
      await until(() => { healthy(session); return session.proc.exitCode != null; }, label + ' finished');
      assert.equal(result.error, undefined, result.error);
      const expectedInput = label === 'normal' || label === 'missing-ack';
      assert.equal(item.after.raw_key_event_count, expectedInput ? 2 : 0, 'actual fixture key delivery count');
      assert.equal(item.after.toggle_ax_press_calls, 0, 'keyboard acceptance cannot use AXPress');
      assert.equal(item.after.toggle_activation_calls, expectedInput ? 1 : 0);
      assert.equal(item.after.value, expectedInput);
      if (expectedInput) {
        const [down, up] = item.after.input_events;
        assert.equal(down.cg_type, 10); assert.equal(up.cg_type, 11);
        assert.equal(down.key_code, 49); assert.equal(up.key_code, 49);
        assert.notEqual(down.marker, '0'); assert.equal(down.marker, up.marker);
        assert.ok(BigInt(up.cg_timestamp) > BigInt(down.cg_timestamp), 'actual pair timestamps must increase');
        assert.equal(down.flags, up.flags); assert.equal(down.autorepeat, false); assert.equal(up.autorepeat, false);
      }
      assert.equal(result.receipt.status, label === 'missing-ack' ? 'unknown' : expectedInput ? 'completed' : 'rejected');
      if (label === 'normal') assert.ok(Number.isSafeInteger(result.input_end_ms), 'known actual input end missing');
      if (label === 'missing-ack') { assert.equal(result.isolation, true); assert.equal(result.no_replay, true); }
      item.ok = true; save();
      fixture.proc.stdin.end('quit\n'); await until(() => fixture.proc.exitCode != null, label + ' fixture exited');
    }
    evidence.owned_keyboard_matrix_passed = true;
  }
  evidence.ok = true;
} catch (error) { evidence.error = String(error?.stack ?? error); process.exitCode = 1;
} finally {
  for (const entry of children.reverse()) {
    if (entry.proc.exitCode == null && entry.proc.signalCode == null) {
      entry.proc.stdin.end('finish\nquit\n'); await pause(100);
      if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGTERM');
    }
  }
  evidence.child_outcomes = children.map(entry => ({ label: entry.label, pid: entry.proc.pid,
    exit_code: entry.proc.exitCode, signal: entry.proc.signalCode, error: entry.error }));
  evidence.finished_at = new Date().toISOString(); save();
  process.stdout.write(JSON.stringify({ ok: evidence.ok, record, output, scenarios: evidence.scenarios.length, error: evidence.error }) + '\n');
}
