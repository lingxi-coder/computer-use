#!/usr/bin/env node
import { nativeFixtureBindingEvidence, buildNativeFixtureContextCompiler } from './mvp/native-fixture-continuity.mjs';
buildNativeFixtureContextCompiler();
// Real AX actions are restricted to this smoke's owned fixture PID/bundle.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const helper = join(root, 'target/debug/cu-macos-device-guard');
const artifactPath = join(root, 'artifacts/m2-native-device-pipeline.json');
const evidence = { status: 'running', started_at: new Date().toISOString(), verified_at: null,
  default_operator_native_input_enabled: false, production_core_native_backend_connected: false, native_cgevent_posted: false, business_effect_verified: false };
const save = () => writeFileSync(artifactPath, JSON.stringify(evidence, null, 2) + '\n');
save();
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
const digest = (fp) => 'fp1:' + createHash('sha256').update(JSON.stringify(fp)).digest('hex');
const normalize = (text) => text.split(/[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u).filter(Boolean).join(' ').toLowerCase();
const round = (n) => Math.sign(n) * Math.floor(Math.abs(n) + 0.5);
try {
  const fixture = child(binary);
  const ready = await until(() => fixture.records.find((r) => r.kind === 'ready'), 'owned fixture');
  async function fixtureCommand(command) {
    fixture.records.length = 0; fixture.proc.stdin.write(command + '\n');
    return until(() => fixture.records.find((r) => r.kind === command), command);
  }
  function observe() {
    const assets = mkdtempSync(join(tmpdir(), 'cu-atomic-source-'));
    try {
      const value = JSON.parse(execFileSync(join(root, 'target/debug/cu-macos-source'), [], {
        input: JSON.stringify({ protocol_version: 1, command: 'observe', bundle_id: ready.bundle_id,
          pid: ready.pid, output_dir: assets, timeout_ms: 3000 }), encoding: 'utf8', timeout: 5000, maxBuffer: 768 * 1024,
      }));
      assert.equal(value.structure.complete, true, value.structure.incomplete_reason);
      return value.structure;
    } finally { rmSync(assets, { recursive: true, force: true }); }
  }
  await pause(250);
  const source = observe();
  const object = source.objects.find((o) => o.role === 'AXButton' && o.title === ready.label && Math.abs(o.frame.x - ready.frame.x) < 0.5 && Math.abs(o.frame.y - ready.frame.y) < 0.5);
  assert.ok(object);
  const ancestors = []; let cursor = object.parent;
  while (cursor) { const parent = source.objects.find((o) => o.instance_ref === cursor); assert.ok(parent); ancestors.push(parent.role); cursor = parent.parent; assert.ok(ancestors.length <= 32); }
  const fp = { app: source.app.bundle_id, app_pid: source.app.pid, window_id: object.window_id,
    ancestor_roles: ancestors.reverse(), group: object.group == null ? null : normalize(object.group),
    role: object.role, subrole: object.subrole ?? null, label: normalize(object.title ?? object.description ?? ''),
    sibling_index: source.objects.filter((o) => o.parent === object.parent && o.window_id === object.window_id).findIndex((o) => o.instance_ref === object.instance_ref),
    frame: [object.frame.x, object.frame.y, object.frame.width, object.frame.height].map(round), scroll_container: null };
  const proof = { binding: 'fixture-binding', instance: object.instance_ref, snapshot_id: 'owned-source', environment_revision: 1, layer_revision: 1, fingerprint: fp, display_frame: object.frame, ...nativeFixtureBindingEvidence(source, object, 1) };
  const device = join(root, 'target/debug/cu-device');
  const examplePath = join(root, 'target/debug/examples/native_device_session');
  let actionCount = 0;
  evidence.scenarios = [];
  for (const scenario of ['applied', 'unknown', 'stop-inflight', 'guard-loss']) {
    await fixtureCommand(scenario === 'applied' ? 'accept-press' : 'delay-press');
    const fresh = observe();
    const current = fresh.objects.find((o) => o.role === 'AXButton' && o.title === ready.label && Math.abs(o.frame.x - ready.frame.x) < 0.5 && Math.abs(o.frame.y - ready.frame.y) < 0.5);
    assert.ok(current);
    const currentProof = { ...proof, instance: current.instance_ref, snapshot_id: `source-${scenario}`, ...nativeFixtureBindingEvidence(fresh, current, 1) };
    const assets = mkdtempSync(join(tmpdir(), 'cu-native-pipeline-'));
    try {
      const path = join(assets, 'proof.json'); writeFileSync(path, JSON.stringify(currentProof), { mode: 0o600 });
      const example = child(examplePath, [device, helper, path, scenario]);
      const started = await until(() => example.records.find((r) => r.kind === 'device_started'), 'real cu-device started');
      await until(() => example.records.find((r) => r.kind === 'device_request_enqueued'), 'production intent enqueued');
      if (scenario === 'stop-inflight' || scenario === 'guard-loss') {
        await until(() => fixture.records.find((r) => r.kind === 'press_started'), 'real AX callback began');
        if (scenario === 'stop-inflight') example.proc.stdin.write('stop\n');
        else {
          const ownGuard = execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,comm='], { encoding: 'utf8' }).split('\n')
            .map((line) => line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/))
            .find((r) => r && Number(r[2]) === started.pid && r[3] === helper);
          assert.ok(ownGuard, 'exact cu-device child guard'); process.kill(Number(ownGuard[1]), 'SIGKILL');
        }
      }
      const result = await until(() => {
        if (example.proc.exitCode != null && example.proc.exitCode !== 0) throw new Error(example.errors());
        return example.records.find((r) => r.kind === 'production_result');
      }, 'production native result', 7000);
      assert.equal(result.production_core_native_backend_connected, true);
      assert.equal(result.simulated_input_used, false); assert.equal(result.duplicate_not_replayed, true);
      assert.equal(result.receipt_status, scenario === 'applied' ? 'completed' : 'unknown');
      assert.equal(result.pending_running_receipt_seen, true);
      if (scenario === 'stop-inflight') assert.ok(result.stop_ack_ms != null && result.stop_ack_ms <= 150);
      await until(() => example.proc.exitCode === 0, 'native session cleanup');
      await pause(250); actionCount++;
      assert.equal((await fixtureCommand('stats')).press_calls, actionCount);
      assert.ok(observe().objects.some((o) => o.role === 'AXButton' && o.title === 'Pressed by native AX'));
      evidence.scenarios.push({ scenario, ...result, fixture_ax_action_count: actionCount });
    } finally { rmSync(assets, { recursive: true, force: true }); }
  }
  await fixtureCommand('accept-press');
  await fixtureCommand('occlude');
  const occlusionAssets = mkdtempSync(join(tmpdir(), 'cu-native-occlusion-'));
  try {
    const path = join(occlusionAssets, 'proof.json'); writeFileSync(path, JSON.stringify(proof), { mode: 0o600 });
    const example = child(examplePath, [device, helper, path, 'occluded']);
    const result = await until(() => {
      if (example.proc.exitCode != null && example.proc.exitCode !== 0) throw new Error(example.errors());
      return example.records.find((r) => r.kind === 'production_result');
    }, 'production occlusion rejection', 7000);
    assert.equal(result.receipt_status, 'rejected'); assert.equal(result.reason_code, 'target_occluded');
    assert.equal(result.submitted_count, 0); assert.equal(result.real_native_ax_input_verified, false);
    await until(() => example.proc.exitCode === 0, 'occluded session cleanup');
    assert.equal((await fixtureCommand('stats')).press_calls, actionCount);
    evidence.production_occlusion_reason_preserved = true;
  } finally { rmSync(occlusionAssets, { recursive: true, force: true }); await fixtureCommand('restore'); }
  const revisionAssets = mkdtempSync(join(tmpdir(), 'cu-native-revision-'));
  try {
    const path = join(revisionAssets, 'proof.json'); writeFileSync(path, JSON.stringify(proof), { mode: 0o600 });
    const result = JSON.parse(execFileSync(join(root, 'target/debug/examples/native_atomic_submit'), [helper, path, 'revision-mismatch'], { encoding: 'utf8', timeout: 5000 }));
    assert.equal(result.native_revision_mismatch_rejected, true);
    assert.equal(result.native_ax_action_called, false);
    assert.equal((await fixtureCommand('stats')).press_calls, actionCount);
    evidence.native_revision_mismatch_rejected = true;
  } finally { rmSync(revisionAssets, { recursive: true, force: true }); }
  evidence.fixture_ax_action_count = actionCount; evidence.production_core_native_backend_connected = true;
  evidence.real_native_ax_input_verified = true; evidence.simulated_input_used = false;
  evidence.status = 'owned_production_native_pipeline_verified'; evidence.verified_at = new Date().toISOString(); save();
  console.log(JSON.stringify(evidence));
} catch (error) {
  evidence.status = 'failed'; evidence.error = String(error); save(); throw error;
} finally {
  for (const timer of timers) clearInterval(timer);
  for (const proc of children) { proc.stdin.destroy(); if (proc.exitCode == null && proc.signalCode == null) proc.kill('SIGKILL'); }
  await Promise.all(children.map((proc) => proc.exitCode != null || proc.signalCode != null ? undefined : new Promise((done) => proc.once('exit', done))));
}
