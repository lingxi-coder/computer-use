#!/usr/bin/env node
import { nativeFixtureBindingEvidence, buildNativeFixtureContextCompiler } from './mvp/native-fixture-continuity.mjs';
buildNativeFixtureContextCompiler();
// Real AX actions are restricted to this smoke's owned fixture PID/bundle.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const helper = join(root, 'target/debug/cu-macos-device-guard');
const artifactPath = join(root, 'artifacts/m2-native-text-values.json');
const evidence = { status: 'running', started_at: new Date().toISOString(), verified_at: null,
  default_operator_native_input_enabled: false, production_core_native_backend_connected: false, native_cgevent_posted: false, business_effect_verified: false };
mkdirSync(dirname(artifactPath), { recursive: true });
const save = () => writeFileSync(artifactPath, JSON.stringify(evidence, null, 2) + '\n');
save();
execFileSync('bash', ['scripts/build-macos-source.sh'], { cwd: root, stdio: 'inherit' });
execFileSync('bash', ['scripts/build-macos-device.sh'], { cwd: root, stdio: 'inherit' });
execFileSync('cargo', ['build', '--locked', '-p', 'device-macos', '--bin', 'cu-device', '--example', 'native_device_session', '--example', 'native_atomic_submit'], { cwd: root, stdio: 'inherit' });
const bundle = join(root, 'target/native-macos/GuardFixture.app/Contents');
const binary = join(bundle, 'MacOS/GuardFixture');
mkdirSync(dirname(binary), { recursive: true });
writeFileSync(join(bundle, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.lingxi.computer-user.guard-fixture</string>
<key>CFBundleExecutable</key><string>GuardFixture</string><key>CFBundleName</key><string>Computer Use Guard Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors',
  '-parse-as-library', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'),
  join(root, 'native/device/fixtures/GuardFixture.swift'), '-o', binary], { stdio: 'inherit' });
const binarySha = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
evidence.binaries_sha256 = {
  source: binarySha(join(root, 'target/debug/cu-macos-source')),
  guard: binarySha(helper),
  device: binarySha(join(root, 'target/debug/cu-device')),
  example: binarySha(join(root, 'target/debug/examples/native_device_session')),
  fixture: binarySha(binary),
};
save();
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
const children = [], timers = [];
function child(program, args = []) {
  const proc = spawn(program, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  children.push(proc);
  const records = []; let pending = '', errors = '';
  proc.stdin.on('error', () => {});
  proc.stderr.on('data', (data) => { errors = (errors + data).slice(-4096); });
  proc.stdout.on('data', (data) => {
    pending += data; assert.ok(pending.length < 16384);
    for (let i; (i = pending.indexOf('\n')) >= 0;) {
      records.push(JSON.parse(pending.slice(0, i))); pending = pending.slice(i + 1);
      if (records.length > 128) records.shift();
    }
  });
  return { proc, records, errors: () => errors };
}
async function until(test, what, ms = 4000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { const result = test(); if (result) return result; await pause(5); }
  throw new Error(`timeout: ${what}`);
}
const normalize = (text) => text.split(/[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u).filter(Boolean).join(' ').toLowerCase();
const round = (n) => Math.sign(n) * Math.floor(Math.abs(n) + 0.5);
try {
  const fixture = child(binary, ['--text-mode']);
  const ready = await until(() => fixture.records.find((r) => r.kind === 'ready'), 'owned fixture');
  async function fixtureCommand(command) {
    fixture.records.length = 0; fixture.proc.stdin.write(command + '\n');
    return until(() => fixture.records.find((r) => r.kind === command), command);
  }
  function observe(policy = true, window = ready.window_id) {
    const assets = mkdtempSync(join(tmpdir(), 'cu-atomic-source-'));
    try {
      const value = JSON.parse(execFileSync(join(root, 'target/debug/cu-macos-source'), [], {
        input: JSON.stringify({ protocol_version: 1, command: 'observe', bundle_id: ready.bundle_id,
          pid: ready.pid, output_dir: assets, timeout_ms: 3000, ...(policy ? { text_policy: { kind: 'registered_editor', window_id: `window-${window}` } } : {}) }), encoding: 'utf8', timeout: 5000, maxBuffer: 768 * 1024,
      }));
      assert.equal(value.structure.complete, true, value.structure.incomplete_reason);
      return value.structure;
    } finally { rmSync(assets, { recursive: true, force: true }); }
  }
  await pause(250);
  const source = observe();
  const object = source.objects.find((o) => o.role === 'AXTextField' && o.title === 'MVP document');
  assert.ok(object);
  const ancestors = []; let cursor = object.parent;
  while (cursor) { const parent = source.objects.find((o) => o.instance_ref === cursor); assert.ok(parent); ancestors.push(parent.role); cursor = parent.parent; assert.ok(ancestors.length <= 32); }
  const fp = { app: source.app.bundle_id, app_pid: source.app.pid, window_id: object.window_id,
    ancestor_roles: ancestors.reverse(), group: object.group == null ? null : normalize(object.group),
    role: object.role, subrole: object.subrole ?? null, label: normalize(object.title ?? object.description ?? ''),
    sibling_index: source.objects.filter((o) => o.parent === object.parent && o.window_id === object.window_id).findIndex((o) => o.instance_ref === object.instance_ref),
    frame: [object.frame.x, object.frame.y, object.frame.width, object.frame.height].map(round), scroll_container: null };
  const proof = { binding: 'fixture-binding', instance: object.instance_ref, snapshot_id: 'owned-source', environment_revision: 1, layer_revision: 1, fingerprint: fp, display_frame: object.frame, ...nativeFixtureBindingEvidence(source, object, 1) };
  assert.equal(object.secure, false); assert.equal(object.value.type, 'text'); assert.equal(object.value.value, 'before');
  const withoutPolicy = observe(false).objects.find((o) => o.role === 'AXTextField');
  assert.equal(withoutPolicy.secure, true); assert.equal(withoutPolicy.value ?? null, null); assert.equal(withoutPolicy.title ?? null, null);
  const wrongWindow = observe(true, ready.window_id + 1).objects.find((o) => o.role === 'AXTextField');
  assert.equal(wrongWindow.secure, true); assert.equal(wrongWindow.value ?? null, null);
  evidence.default_text_private = true; evidence.wrong_window_text_private = true;
  const device = join(root, 'target/debug/cu-device'), examplePath = join(root, 'target/debug/examples/native_device_session');
  evidence.scenarios = []; let calls = 0;
  const prefix = '👩‍👩‍👧‍👦\r\n中文';
  const boundary = prefix + 'a'.repeat(2048 - Buffer.byteLength(prefix));
  assert.equal(Buffer.byteLength(boundary), 2048);
  for (const scenario of ['set-text', 'set-text-boundary', 'set-text-unknown', 'set-text-unfocused', 'set-text-oversize', 'set-text-secure']) {
    await fixtureCommand(scenario === 'set-text-unknown' ? 'delay-value' : 'accept-value');
    const fresh = observe(), field = fresh.objects.find((o) => o.role === 'AXTextField' && o.title === 'MVP document');
    assert.ok(field); assert.equal(field.value.value, 'before');
    const currentProof = { ...proof, instance: field.instance_ref, snapshot_id: `source-${scenario}` };
    if (scenario === 'set-text-unfocused') await fixtureCommand('focus-button');
    if (scenario === 'set-text-secure') currentProof.fingerprint = { ...fp, subrole: 'AXSecureTextField' };
    const assets = mkdtempSync(join(tmpdir(), 'cu-text-values-'));
    try {
      const path = join(assets, 'proof.json'); writeFileSync(path, JSON.stringify(currentProof), { mode: 0o600 });
      const example = child(examplePath, [device, helper, path, scenario]);
      const result = await until(() => {
        if (example.proc.exitCode != null && example.proc.exitCode !== 0) throw new Error(example.errors());
        return example.records.find((r) => r.kind === 'production_result');
      }, 'real text value result', 7000);
      const changed = ['set-text', 'set-text-boundary', 'set-text-unknown'].includes(scenario);
      assert.equal(result.receipt_status, changed ? scenario === 'set-text-unknown' ? 'unknown' : 'completed' : 'rejected');
      assert.equal(result.simulated_input_used, false); assert.equal(result.duplicate_not_replayed, true);
      await until(() => example.proc.exitCode === 0, 'text session cleanup'); await pause(250);
      if (changed) calls++;
      const stats = await fixtureCommand('stats'); assert.equal(stats.value_calls, calls); assert.equal(stats.press_calls, 0);
      const after = observe().objects.find((o) => o.role === 'AXTextField' && o.title === 'MVP document');
      assert.equal(after.value.value, scenario === 'set-text-boundary' ? boundary : changed ? 'MVP 中文\nHello 👋' : 'before');
      evidence.scenarios.push({ scenario, ...result, source_value_equals_expected: true, source_value_utf8_bytes: Buffer.byteLength(after.value.value), fixture_value_calls: calls });
    } finally { rmSync(assets, { recursive: true, force: true }); }
  }
  evidence.fixture_value_calls = calls; evidence.fixture_ax_press_calls = 0;
  evidence.production_core_native_backend_connected = true;
  evidence.status = 'owned_native_text_values_verified'; evidence.verified_at = new Date().toISOString(); save();
  console.log(JSON.stringify(evidence));
} catch (error) {
  evidence.status = 'failed'; evidence.error = String(error); save(); throw error;
} finally {
  for (const timer of timers) clearInterval(timer);
  for (const proc of children) { proc.stdin.destroy(); if (proc.exitCode == null && proc.signalCode == null) proc.kill('SIGKILL'); }
  await Promise.all(children.map((proc) => proc.exitCode != null || proc.signalCode != null ? undefined : new Promise((done) => proc.once('exit', done))));
}
