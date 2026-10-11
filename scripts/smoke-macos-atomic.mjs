#!/usr/bin/env node
import { nativeFixtureBindingEvidence, buildNativeFixtureContextCompiler, nativeFixtureAuthorityContext } from './mvp/native-fixture-continuity.mjs';
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
const artifactPath = join(root, 'artifacts/m2-native-atomic-submit.json');
const evidence = { status: 'running', started_at: new Date().toISOString(), verified_at: null,
  production_native_input_enabled: false, native_cgevent_posted: false, business_effect_verified: false };
const save = () => writeFileSync(artifactPath, JSON.stringify(evidence, null, 2) + '\n');
save();
execFileSync('bash', ['scripts/build-macos-device.sh'], { cwd: root, stdio: 'inherit' });
execFileSync('cargo', ['build', '--locked', '-p', 'device-macos', '--example', 'native_atomic_submit'], { cwd: root, stdio: 'inherit' });
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
let frozen;
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
  const assets = mkdtempSync(join(tmpdir(), 'cu-atomic-proof-'));
  try {
    const path = join(assets, 'proof.json'); writeFileSync(path, JSON.stringify(proof), { mode: 0o600 });
    await fixtureCommand('accept-press');
    const applied = JSON.parse(execFileSync(join(root, 'target/debug/examples/native_atomic_submit'), [helper, path, 'applied'], { encoding: 'utf8', timeout: 6000 }));
    assert.equal(applied.duplicate_rejected, true); assert.equal(applied.revoked_rejected, true);
    assert.equal((await fixtureCommand('stats')).press_calls, 1);
    assert.ok(observe().objects.some((o) => o.role === 'AXButton' && o.title === 'Pressed by native AX'));
    evidence.core_authority_reached_native_submit = true; evidence.applied_receipt_verified = true;
    evidence.same_intent_replay_rejected = true; evidence.fixture_ui_effect_observed_after_applied = true;
    await fixtureCommand('delay-press');
    const unknown = JSON.parse(execFileSync(join(root, 'target/debug/examples/native_atomic_submit'), [helper, path, 'unknown'], { encoding: 'utf8', timeout: 6000 }));
    assert.equal(unknown.duplicate_rejected, true);
    await pause(250);
    assert.equal((await fixtureCommand('stats')).press_calls, 2);
    assert.ok(observe().objects.some((o) => o.role === 'AXButton' && o.title === 'Pressed by native AX'));
    evidence.ax_timeout_recorded_unknown = true; evidence.unknown_not_replayed = true;
    evidence.fixture_ui_effect_observed_after_unknown = true;
  } finally { rmSync(assets, { recursive: true, force: true }); }
  await fixtureCommand('accept-press');
  let id = 0;
  async function session(armed = true) {
    const origin = Date.now();
    const guard = child(helper, ['--epoch', '7', '--marker', '123', ...(armed ? ['--execute-pid', String(ready.pid), '--execute-bundle', ready.bundle_id] : [])]);
    const send = (value) => guard.proc.stdin.write(JSON.stringify({ protocol_version: 1, session_epoch: 7, ...value }) + '\n');
    send({ command: 'configure_authority', clock_origin_ms: origin, clock_domain: 'macos/7/wall', surface: { id: 'main', epoch: 7 } });
    const renew = () => { for (const lane of ['host', 'supervisor']) send({ command: 'lease', lane, duration_ms: 750 }); };
    renew(); const timer = setInterval(renew, 100); timers.push(timer);
    const state = await until(() => {
      if (guard.proc.exitCode != null || guard.proc.signalCode != null) throw new Error(`guard exited: ${guard.errors()}`);
      return guard.records.findLast((r) => r.kind === 'guard' && r.authority_clock_ready && r.host_alive && r.supervisor_alive);
    }, 'authority bootstrap');
    assert.equal(state.accessibility, true); assert.equal(state.human_input, true);
    const request = (overrides = {}) => {
      const now = Date.now() - origin, unique = `atomic-${++id}`;
      return { request_id: unique, intent_id: unique, task_id: 'owned-root', task_revision: 1, root_epoch: 1,
        permit: { id: unique, generation: 1 }, stop_seq: 0, guard_revision: state.stop_revision,
        clock_domain: 'macos/7/wall', surface: { id: 'main', epoch: 7 }, evidence_id: unique, intent_evidence_id: unique,
        source_sequence: 1, observed_ms: now, valid_until_ms: now + 1000, command_end_ms: now + 1000,
        started_ms: now, duration_ms: 1000, latest_start_ms: now, ...nativeFixtureAuthorityContext(proof),
        binding_digest: digest(fp), dependencies: { focus: 1, layer: 1, binding: 1, authorization: 1, geometry: 1 },
        action: 'ax_press', x: ready.x, y: ready.y, ...overrides };
    };
    const reply = async (r) => { send({ command: 'atomic_submit', probe: r }); return until(() => guard.records.find((v) => v.kind === 'atomic_submit' && v.request_id === r.request_id), 'atomic reply'); };
    return { guard, send, request, reply, end: () => { clearInterval(timer); guard.proc.stdin.end(); } };
  }
  const readonly = await session(false);
  assert.equal((await readonly.reply(readonly.request())).outcome, 'rejected'); readonly.end();
  evidence.readonly_session_rejected_submission = true;
  const unsupported = await session();
  assert.equal((await unsupported.reply(unsupported.request({ action: 'click' }))).outcome, 'rejected'); unsupported.end();
  evidence.unsupported_primitive_rejected = true;
  const revoked = await session(); const revokedRequest = revoked.request();
  revoked.send({ command: 'revoke_authority', permit: revokedRequest.permit });
  assert.equal((await revoked.reply(revokedRequest)).outcome, 'revoked'); revoked.end();
  evidence.revocation_before_submit_rejected = true;
  const stopped = await session(); const stoppedRequest = stopped.request(); stopped.send({ command: 'stop' });
  assert.equal((await stopped.reply(stoppedRequest)).outcome, 'stop_latched'); stopped.end();
  evidence.stop_before_submit_rejected = true;
  const expired = await session(); const short = expired.request();
  short.valid_until_ms = short.started_ms + 30; await pause(70);
  assert.equal((await expired.reply(short)).outcome, 'expired'); expired.end();
  evidence.expired_submit_rejected = true;
  const scope = await session(); const foreign = structuredClone(proof);
  foreign.fingerprint.app_pid = ready.pid + 1;
  assert.equal((await scope.reply(scope.request({ ...nativeFixtureAuthorityContext(foreign), binding_digest: digest(foreign.fingerprint) }))).outcome, 'rejected'); scope.end();
  evidence.foreign_pid_rejected = true;
  await fixtureCommand('occlude');
  const occluded = await session();
  assert.equal((await occluded.reply(occluded.request())).outcome, 'target_occluded'); occluded.end();
  await fixtureCommand('restore'); evidence.occluded_submit_rejected = true;
  // Stop and revoke arrive while the worker is in a bounded AX query.
  for (const command of ['revoke_authority', 'stop']) {
    const race = await session(), request = race.request();
    frozen = fixture.proc.pid; process.kill(frozen, 'SIGSTOP');
    race.send({ command: 'atomic_submit', probe: request }); await pause(10);
    race.send(command === 'stop' ? { command } : { command, permit: request.permit });
    const response = await until(() => race.guard.records.find((r) => r.kind === 'atomic_submit' && r.request_id === request.request_id), 'query cancellation');
    assert.equal(response.submitted, false);
    assert.ok(['context_unresolved', 'revoked', 'stop_latched'].includes(response.outcome), response.outcome);
    process.kill(frozen, 'SIGCONT'); frozen = undefined; await pause(80); race.end();
    evidence[command === 'stop' ? 'stop_during_query_no_action' : 'revoke_during_query_no_action'] = true;
  }
  await fixtureCommand('delay-press');
  const during = await session(), duringRequest = during.request();
  during.send({ command: 'atomic_submit', probe: duringRequest });
  await until(() => fixture.records.find((r) => r.kind === 'press_started'), 'real AX call started');
  during.send({ command: 'stop' });
  const duringReply = await until(() => during.guard.records.find((r) => r.kind === 'atomic_submit' && r.request_id === duringRequest.request_id), 'receipt after stop');
  assert.equal(duringReply.submitted, true); assert.equal(duringReply.outcome, 'unknown');
  await until(() => during.guard.records.find((r) => r.kind === 'guard' && r.stop_revision > duringRequest.guard_revision), 'ordered stop recorded');
  during.end(); await pause(250);
  assert.equal((await fixtureCommand('stats')).press_calls, 3);
  evidence.stop_during_ax_call_preserved_unknown = true;
  await fixtureCommand('delay-press');
  const lostAssets = mkdtempSync(join(tmpdir(), 'cu-atomic-lost-'));
  try {
    const proofPath = join(lostAssets, 'proof.json'); writeFileSync(proofPath, JSON.stringify(proof), { mode: 0o600 });
    const example = child(join(root, 'target/debug/examples/native_atomic_submit'), [helper, proofPath, 'transport-unknown']);
    const launched = await until(() => example.records.find((r) => r.kind === 'native_guard_started'), 'Rust child identity');
    await until(() => fixture.records.find((r) => r.kind === 'press_started'), 'AX callback before death');
    process.kill(launched.pid, 'SIGKILL');
    const lost = await until(() => example.records.find((r) => r.kind === 'transport_result'), 'Rust unknown result');
    assert.equal(lost.native_transport_unknown, true); assert.equal(lost.unknown_not_replayed, true);
    await until(() => example.proc.exitCode === 0, 'Rust example cleanup');
    await pause(250);
    assert.ok(observe().objects.some((o) => o.role === 'AXButton' && o.title === 'Pressed by native AX'));
    evidence.helper_death_after_ax_started_unknown = true; evidence.lost_receipt_not_replayed = true;
  } finally { rmSync(lostAssets, { recursive: true, force: true }); }
  const stats = await fixtureCommand('stats'); assert.equal(stats.press_calls, 4);
  evidence.fixture_ax_action_count = stats.press_calls;
  evidence.status = 'owned_native_ax_submit_verified'; evidence.verified_at = new Date().toISOString(); save();
  console.log(JSON.stringify(evidence));
} catch (error) {
  evidence.status = 'failed'; evidence.error = String(error); save(); throw error;
} finally {
  if (frozen) { try { process.kill(frozen, 'SIGCONT'); } catch {} }
  for (const timer of timers) clearInterval(timer);
  for (const proc of children) { proc.stdin.destroy(); if (proc.exitCode == null && proc.signalCode == null) proc.kill('SIGKILL'); }
  await Promise.all(children.map((proc) => proc.exitCode != null || proc.signalCode != null ? undefined : new Promise((done) => proc.once('exit', done))));
}
