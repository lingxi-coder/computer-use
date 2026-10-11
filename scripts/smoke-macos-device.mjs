#!/usr/bin/env node
import { nativeFixtureBindingEvidence, buildNativeFixtureContextCompiler, nativeFixtureAuthorityContext } from './mvp/native-fixture-continuity.mjs';
buildNativeFixtureContextCompiler();
// Passive native guards and system-wide AX hit tests against owned fixtures.
// No CGEvent is posted and no AX action is performed.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Supervisor } from '../apps/desktop/src/main/supervisor.ts';
import { resolveBinaries } from '../apps/desktop/src/main/binaries.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const helper = join(root, 'target/debug/cu-macos-device-guard');
const artifactPath = join(root, 'artifacts/m2-native-device-guards.json');
writeFileSync(artifactPath, JSON.stringify({ started_at: new Date().toISOString(), verified_at: null,
  status: 'running', physical_input_sent: false }, null, 2) + '\n');
execFileSync('cargo', ['build', '--locked', '-p', 'device-macos', '--example', 'native_authority_probe'],
  { cwd: root, stdio: 'inherit' });
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
const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };
const children = [];
function child(program, args = []) {
  const proc = spawn(program, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  children.push(proc);
  const records = []; let pending = ''; let diagnostics = '';
  proc.stdin.on('error', (error) => { diagnostics = (diagnostics + `stdin: ${error.message}`).slice(-4096); });
  proc.stderr.on('data', (data) => { diagnostics = (diagnostics + data.toString()).slice(-4096); });
  proc.stdout.on('data', (data) => {
    pending += data.toString();
    assert.ok(pending.length < 16384, 'bounded native response');
    for (let i; (i = pending.indexOf('\n')) >= 0;) {
      const line = pending.slice(0, i); pending = pending.slice(i + 1);
      records.push(JSON.parse(line)); if (records.length > 64) records.shift();
    }
  });
  return { proc, records, diagnostics: () => diagnostics };
}
async function until(test, description, ms = 4000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) { const value = test(); if (value) return value; await pause(10); }
  throw new Error(`timeout: ${description}`);
}
const ownGuardPid = (devicePid) => execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,comm='], { encoding: 'utf8' })
  .split('\n').map((l) => l.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/))
  .find((r) => r && Number(r[2]) === devicePid && r[3] === helper)?.[1];
let supervisor; let frozen; let frozenFixture;
const evidence = { physical_input_sent: false, full_binding_fingerprint_verified: false, execution_binding_authorized: false,
  physical_hotkey_tested: false, signed_package_permissions_tested: false };
try {
  const fixture = child(binary);
  const ready = await until(() => fixture.records.find((r) => r.kind === 'ready'), 'owned fixture readiness');
  await pause(250);
  // Obtain the expected binding from the independent source projection, not
  // from the device under test or a fixture-supplied AX hierarchy.
  const assets = mkdtempSync(join(tmpdir(), 'cu-guard-proof-'));
  let source;
  try {
    for (let attempt = 0; attempt < 3; attempt++) {
      source = JSON.parse(execFileSync(join(root, 'target/debug/cu-macos-source'), [], {
        input: JSON.stringify({ protocol_version: 1, command: 'observe', bundle_id: ready.bundle_id,
          pid: ready.pid, output_dir: assets, timeout_ms: 3000 }), encoding: 'utf8', timeout: 5000,
        maxBuffer: 768 * 1024,
      }));
      if (source.structure?.complete) break;
      await pause(250);
    }
  } finally { rmSync(assets, { recursive: true, force: true }); }
  assert.equal(source.structure.complete, true, JSON.stringify(source.structure.incomplete_reason));
  const object = source.structure.objects.find((o) => o.role === 'AXButton' && o.title === ready.label
    && Math.abs(o.frame.x - ready.frame.x) < 0.5 && Math.abs(o.frame.y - ready.frame.y) < 0.5);
  assert.ok(object, 'source observes the intended button');
  const normalize = (text) => text.split(/[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u)
    .filter(Boolean).join(' ').toLowerCase();
  const round = (n) => Math.sign(n) * Math.floor(Math.abs(n) + 0.5);
  const ancestors = []; let cursor = object.parent;
  while (cursor) {
    const parent = source.structure.objects.find((o) => o.instance_ref === cursor);
    assert.ok(parent); ancestors.push(parent.role); cursor = parent.parent;
    assert.ok(ancestors.length <= 32);
  }
  const binding = { app: source.structure.app.bundle_id, app_pid: source.structure.app.pid,
    window_id: object.window_id, ancestor_roles: ancestors.reverse(), group: object.group == null ? null : normalize(object.group),
    role: object.role, subrole: object.subrole ?? null, label: normalize(object.title ?? object.description ?? ''),
    sibling_index: source.structure.objects.filter((o) => o.parent === object.parent && o.window_id === object.window_id)
      .findIndex((o) => o.instance_ref === object.instance_ref),
    frame: [object.frame.x, object.frame.y, object.frame.width, object.frame.height].map(round), scroll_container: null };
  assert.equal(binding.group, 'fixture group'); assert.ok(binding.ancestor_roles.includes('AXGroup'));
  const digest = (proof) => 'fp1:' + createHash('sha256').update(JSON.stringify(proof)).digest('hex');
  const golden = JSON.parse(readFileSync(join(root, 'contracts/desktop-v1/native-binding/golden.json'), 'utf8'));
  for (const entry of golden.fingerprints) assert.equal(digest(entry.fingerprint), entry.digest);
  for (const entry of golden.labels) assert.equal(normalize(entry.raw), entry.normalized);
  const originalProof = { binding: 'fixture-binding', instance: object.instance_ref,
    snapshot_id: 'fixture-source-1', environment_revision: 1, layer_revision: 1,
    fingerprint: binding, display_frame: object.frame, ...nativeFixtureBindingEvidence(source.structure, object, 1) };
  const rustAssets = mkdtempSync(join(tmpdir(), 'cu-native-authority-'));
  try {
    const proofPath = join(rustAssets, 'proof.json'); writeFileSync(proofPath, JSON.stringify(originalProof), { mode: 0o600 });
    const reply = JSON.parse(execFileSync(join(root, 'target/debug/examples/native_authority_probe'), [helper, proofPath],
      { encoding: 'utf8', timeout: 5000 }));
    Object.assign(evidence, reply);
    assert.equal(reply.probe_did_not_consume_guard_notice, true);
  } finally { rmSync(rustAssets, { recursive: true, force: true }); }
  const clockOrigin = Date.now();
  const guard = child(helper, ['--epoch', '7', '--marker', '123']);
  const send = (value) => guard.proc.stdin.write(JSON.stringify({ protocol_version: 1, session_epoch: 7, ...value }) + '\n');
  send({ command: 'configure_authority', clock_origin_ms: clockOrigin,
    surface: { id: 'main', epoch: 7 }, clock_domain: 'macos/7/wall' });
  let host = true, main = true;
  const renew = () => {
    if (host) send({ command: 'lease', lane: 'host', duration_ms: 500 });
    if (main) send({ command: 'lease', lane: 'supervisor', duration_ms: 500 });
  };
  const leaseTimer = setInterval(renew, 100); renew();
  try {
    const snapshot = await until(() => {
      if (guard.proc.exitCode != null || guard.proc.signalCode != null) {
        throw new Error(`guard exited ${guard.proc.exitCode}/${guard.proc.signalCode}: ${guard.diagnostics()}`);
      }
      return guard.records.findLast((r) => r.kind === 'guard' && r.host_alive && r.supervisor_alive);
    }, 'both native leases');
    assert.equal(snapshot.accessibility, true, 'AX permission required; this test never requests it');
    evidence.human_input_available = snapshot.human_input;
    assert.equal(snapshot.input_available, false);
    let hitId = 0;
    async function hit(overrides = {}) {
      const id = `hit-${++hitId}`;
      const { kind: _kind, role: _role, label: _label, ...descriptor } = ready;
      const proof = { ...binding, ...overrides.binding };
      const request = { ...descriptor, request_id: id, binding: proof, binding_digest: digest(proof), ...overrides };
      // Allow individual negative proof fields while retaining canonical order.
      request.binding = proof;
      send({ command: 'hit_test', hit: request });
      return until(() => {
        if (guard.proc.exitCode != null || guard.proc.signalCode != null) {
          throw new Error(`hit guard exited ${guard.proc.exitCode}/${guard.proc.signalCode}: ${guard.diagnostics()} descriptor ${JSON.stringify(descriptor)}`);
        }
        return guard.records.find((r) => r.kind === 'hit_test' && r.request_id === id);
      }, 'native hit result');
    }
    let probeId = 0;
    const probeBody = (overrides = {}) => {
      const now = Date.now() - clockOrigin;
      const id = `native-probe-${++probeId}`;
      return { request_id: id, intent_id: id, task_id: 'fixture-root', task_revision: 1, root_epoch: 1,
        permit: { id: id, generation: 1 }, stop_seq: 5,
        guard_revision: guard.records.findLast((r) => r.kind === 'guard').stop_revision,
        clock_domain: 'macos/7/wall', surface: { id: 'main', epoch: 7 }, evidence_id: 'fixture-evidence',
        intent_evidence_id: 'fixture-original', source_sequence: 1, observed_ms: now,
        valid_until_ms: now + 1000, command_end_ms: now + 1000, started_ms: now, duration_ms: 1000, latest_start_ms: now,
        ...nativeFixtureAuthorityContext(originalProof), binding_digest: digest(binding),
        dependencies: { focus: 1, layer: 1, binding: 1, authorization: 1, geometry: 1 },
        action: 'ax_press', x: ready.x, y: ready.y, ...overrides };
    };
    async function probe(request) {
      send({ command: 'authority_probe', probe: request });
      const reply = await until(() => guard.records.find((r) => r.kind === 'authority_probe' && r.request_id === request.request_id), 'native authority probe');
      assert.equal(reply.input_authorized, false);
      if (reply.outcome !== 'eligible') assert.equal(reply.binding_digest, undefined);
      return reply;
    }
    const permitted = probeBody();
    assert.equal((await probe(permitted)).outcome, 'eligible'); evidence.native_authority_probe_eligible = true;
    assert.equal((await probe({ ...permitted, request_id: 'replay' })).outcome, 'rejected'); evidence.intent_probe_replay_rejected = true;
    assert.equal((await probe(probeBody({ stop_seq: 4 }))).outcome, 'stop_latched'); evidence.native_stop_floor_rejected = true;
    assert.equal((await probe(probeBody({ current_proof: { ...nativeFixtureAuthorityContext(originalProof).current_proof, instance: 'replacement' } }))).outcome, 'context_unresolved');
    evidence.native_source_instance_mismatch_rejected = true;
    const late = probeBody(); late.duration_ms = 50; late.valid_until_ms = late.command_end_ms = late.started_ms + 50;
    await pause(80);
    assert.equal((await probe(late)).outcome, 'expired'); evidence.delayed_request_did_not_gain_ttl = true;
    const beforeRevoke = probeBody(); send({ command: 'revoke_authority', permit: beforeRevoke.permit });
    assert.equal((await probe(beforeRevoke)).outcome, 'revoked'); evidence.native_revocation_before_query = true;
    const duringRevoke = probeBody();
    frozenFixture = fixture.proc.pid; process.kill(frozenFixture, 'SIGSTOP');
    const revokeTimer = setTimeout(() => send({ command: 'revoke_authority', permit: duringRevoke.permit }), 10);
    try { assert.equal((await probe(duringRevoke)).outcome, 'revoked'); evidence.native_revocation_during_query = true; }
    finally { clearTimeout(revokeTimer); process.kill(frozenFixture, 'SIGCONT'); frozenFixture = undefined; }
    const duringExpiry = probeBody(); duringExpiry.duration_ms = 15; duringExpiry.valid_until_ms = duringExpiry.command_end_ms = duringExpiry.started_ms + 15;
    frozenFixture = fixture.proc.pid; process.kill(frozenFixture, 'SIGSTOP');
    try { assert.equal((await probe(duringExpiry)).outcome, 'expired'); evidence.native_expiry_during_query = true; }
    finally { process.kill(frozenFixture, 'SIGCONT'); frozenFixture = undefined; }
    const stoppedProbe = probeBody(); send({ command: 'stop' });
    assert.equal((await probe(stoppedProbe)).outcome, 'stop_latched'); evidence.native_stop_before_query = true;

    const matched = await hit();
    assert.equal(matched.outcome, 'matched', JSON.stringify(matched));
    assert.equal(matched.binding_digest, digest(binding)); assert.equal(matched.input_authorized, false);
    evidence.fixture_hit_matched = true; evidence.full_binding_fingerprint_verified = true;
    evidence.source_to_device_digest = digest(binding);
    for (const changed of [{ label: 'different button' }, { group: 'different group' },
      { subrole: 'AXToggle' }, { sibling_index: binding.sibling_index + 1 }, { ancestor_roles: ['AXWindow'] }]) {
      const rejected = await hit({ binding: changed });
      assert.equal(rejected.outcome, 'target_occluded', JSON.stringify(rejected));
      assert.equal(rejected.input_authorized, false); assert.equal(rejected.binding_digest, undefined);
    }
    assert.equal((await hit({ window_id: ready.window_id + 1,
      binding: { window_id: `window-${ready.window_id + 1}` } })).outcome, 'target_occluded');
    async function control(command) {
      const count = fixture.records.filter((r) => r.kind === command).length;
      fixture.proc.stdin.write(command + '\n');
      await until(() => fixture.records.filter((r) => r.kind === command).length > count, command);
      await pause(60);
    }
    await control('rename-group');
    assert.equal((await hit()).outcome, 'target_occluded'); evidence.live_group_change_rejected = true;
    await control('restore-group'); assert.equal((await hit()).outcome, 'matched');
    await control('reorder');
    assert.equal((await hit()).outcome, 'target_occluded'); evidence.live_sibling_reorder_rejected = true;
    await control('restore-order'); assert.equal((await hit()).outcome, 'matched');
    await control('swap-buttons');
    assert.equal((await hit()).outcome, 'target_occluded'); evidence.same_label_replacement_rejected = true;
    await control('swap-buttons'); assert.equal((await hit()).outcome, 'matched');
    for (const broken of ['inconsistent-parent', 'duplicate-child', 'overfull-children']) {
      await control(broken);
      const unresolved = await hit();
      assert.equal(unresolved.outcome, 'context_unresolved', JSON.stringify(unresolved));
      assert.equal(unresolved.binding_digest, undefined); assert.equal(unresolved.input_authorized, false);
      await control(broken === 'inconsistent-parent' ? 'restore-parent' : 'restore-order');
      assert.equal((await hit()).outcome, 'matched');
    }
    evidence.inconsistent_parent_rejected = true;
    evidence.duplicate_child_rejected = true; evidence.overfull_children_rejected = true;
    fixture.proc.stdin.write('occlude\n');
    await until(() => fixture.records.some((r) => r.kind === 'occlude'), 'owned occluder');
    await pause(100);
    assert.equal((await hit()).outcome, 'target_occluded'); evidence.occlusion_rejected = true;
    fixture.proc.stdin.write('restore\n');
    await until(() => fixture.records.some((r) => r.kind === 'restore'), 'fixture restored');
    await pause(100);
    assert.equal((await hit()).outcome, 'matched');
    // Freeze only this test's target process. AX must reject an unresponsive
    // target within its bound while the independent control reader still stops.
    const slowStarted = Date.now();
    frozenFixture = fixture.proc.pid; process.kill(frozenFixture, 'SIGSTOP');
    const stopDuringQuery = setTimeout(() => send({ command: 'stop' }), 10);
    let slow;
    try { slow = await hit(); }
    finally {
      clearTimeout(stopDuringQuery);
      if (alive(frozenFixture)) process.kill(frozenFixture, 'SIGCONT');
      frozenFixture = undefined;
    }
    assert.ok(['context_unresolved', 'stop_latched'].includes(slow.outcome), JSON.stringify(slow));
    assert.ok(Date.now() - slowStarted < 1000, 'bounded unresponsive AX query');
    evidence.unresponsive_target_rejected = true;
    evidence.unresponsive_target_report_ms = Date.now() - slowStarted;
    assert.equal(slow.outcome, 'stop_latched', 'stop must win over a late query result');
    assert.equal(slow.binding_digest, undefined); assert.equal(slow.input_authorized, false);
    evidence.stop_during_query_verified = true;
    const before = guard.records.findLast((r) => r.kind === 'guard');
    send({ command: 'stop' });
    const stopped = await until(() => guard.records.findLast((r) => r.kind === 'guard' && r.stop_revision > before.stop_revision), 'direct stop latch');
    evidence.stop_latched = true;
    const loss = stopped.lease_loss_sequence;
    host = false;
    const hostLost = await until(() => guard.records.findLast((r) => r.kind === 'guard' && !r.host_alive && r.supervisor_alive && r.lease_loss_sequence > loss), 'independent host lease');
    host = true;
    await until(() => guard.records.findLast((r) => r.kind === 'guard' && r.host_alive && r.stop_revision >= hostLost.stop_revision), 'host renewal preserves stop');
    main = false;
    await until(() => guard.records.findLast((r) => r.kind === 'guard' && r.host_alive && !r.supervisor_alive && r.lease_loss_sequence > hostLost.lease_loss_sequence), 'independent supervisor lease');
    evidence.independent_host_and_supervisor_expiry = true;
    // No following heartbeat bytes may be needed to flush a short command.
    clearInterval(leaseTimer);
    const beforeShort = guard.records.findLast((r) => r.kind === 'guard');
    const shortStarted = Date.now();
    send({ command: 'stop' });
    await until(() => guard.records.findLast((r) => r.kind === 'guard'
      && r.stop_revision > beforeShort.stop_revision && r.reason === 'operator stop'), 'standalone short stop', 400);
    evidence.short_stop_without_following_bytes = true;
    evidence.short_stop_report_ms = Date.now() - shortStarted;
    await control('replace-window');
    assert.equal((await hit()).outcome, 'target_occluded'); evidence.live_window_replacement_rejected = true;
  } finally { clearInterval(leaseTimer); guard.proc.stdin.end(); }
  await until(() => !alive(guard.proc.pid), 'control EOF exits guard'); evidence.control_eof_exits = true;
  const invalid = child(helper, ['--epoch', '8', '--marker', '123']);
  invalid.proc.stdin.write('{"protocol_version":1,"session_epoch":7,"command":"stop"}\n');
  await until(() => !alive(invalid.proc.pid), 'wrong epoch closes guard'); evidence.stale_control_epoch_rejected = true;

  const { kind: _kind, role: _role, label: _label, ...base } = ready;
  const invalidBindingRequests = [
    { ...base, request_id: 'invalid', binding, binding_digest: 'fp1:' + '0'.repeat(64) },
    { ...base, request_id: 'invalid', binding: { ...binding, unexpected: true }, binding_digest: digest(binding) },
    { ...base, request_id: 'invalid', binding, binding_digest: digest(binding), frame: { ...ready.frame, x: 1e300 } },
    { ...base, request_id: 'invalid', binding: { ...binding, frame: [] }, binding_digest: digest(binding) },
    { ...base, request_id: 'invalid', binding: { ...binding, sibling_index: 256 }, binding_digest: digest(binding) },
    { ...base, request_id: 'invalid', binding: { ...binding, ancestor_roles: Array(33).fill('AXWindow') }, binding_digest: digest(binding) },
  ];
  for (const request of invalidBindingRequests) {
    const rejected = child(helper, ['--epoch', '7', '--marker', '123']);
    rejected.proc.stdin.write(JSON.stringify({ protocol_version: 1, session_epoch: 7, command: 'hit_test', hit: request }) + '\n');
    await until(() => rejected.proc.exitCode != null || rejected.proc.signalCode != null, 'invalid proof closes guard');
    assert.equal(rejected.proc.exitCode, 0, rejected.diagnostics());
    assert.equal(rejected.proc.signalCode, null, 'malformed proof must not trap');
  }
  evidence.invalid_binding_protocol_rejected = invalidBindingRequests.length;

  supervisor = await Supervisor.start({ binaries: resolveBinaries({ CU_BIN_DIR: join(root, 'target/debug') }),
    nativeSource: { helperPath: join(root, 'target/debug/cu-macos-source'), bundleId: ready.bundle_id,
      pid: ready.pid, guardHelperPath: helper }, mode: 'observe' });
  const logs = []; supervisor.on('log', (line) => { logs.push(line); if (logs.length > 20) logs.shift(); });
  const nativePid = Number(await until(() => ownGuardPid(supervisor.devicePid), 'guard belongs to cu-device'));
  await until(() => supervisor.projection?.capabilities?.backend === 'macos', 'native capability manifest');
  assert.equal(supervisor.projection.capabilities.input, 'unavailable');
  assert.equal(supervisor.projection.capabilities.hit_test, 'unavailable');
  const stopEvents = []; supervisor.on('stopAck', (ack) => stopEvents.push(ack));
  frozen = nativePid; process.kill(nativePid, 'SIGSTOP');
  await until(() => stopEvents.some((ack) => ack.source === 'device_local'), 'stale guard closes device', 4000);
  await until(() => !alive(nativePid), 'frozen owned guard is reaped'); frozen = undefined;
  await until(() => supervisor.projection?.capabilities?.human_input === 'unavailable', 'guard capability invalidated');
  assert.equal(existsSync(supervisor.worldLogPath), false, 'native observation sent no fixture-world input');
  evidence.frozen_guard_reaped = true; evidence.guard_loss_stops_device = true;
  await supervisor.shutdown(); supervisor = undefined;

  supervisor = await Supervisor.start({ binaries: resolveBinaries({ CU_BIN_DIR: join(root, 'target/debug') }),
    nativeSource: { helperPath: join(root, 'target/debug/cu-macos-source'), bundleId: ready.bundle_id,
      pid: ready.pid, guardHelperPath: helper }, mode: 'observe' });
  const orphan = Number(await until(() => ownGuardPid(supervisor.devicePid), 'second owned guard'));
  process.kill(supervisor.devicePid, 'SIGKILL');
  await until(() => !alive(orphan), 'device death closes its guard pipe'); evidence.device_death_exits_guard = true;
  await supervisor.shutdown(); supervisor = undefined;
  fixture.proc.stdin.write('stats\n');
  const stats = await until(() => fixture.records.find((r) => r.kind === 'stats'), 'fixture action counter');
  assert.equal(stats.press_calls, 0, 'read-only probes must perform no AX action');
  evidence.fixture_ax_action_count = 0;
  writeFileSync(artifactPath, JSON.stringify({
    verified_at: new Date().toISOString(), status: 'passive_guards_verified', ...evidence,
  }, null, 2) + '\n');
  console.log(JSON.stringify(evidence));
} catch (error) {
  writeFileSync(artifactPath, JSON.stringify({ verified_at: null, status: 'failed',
    physical_input_sent: false, reason: String(error).slice(0, 512) }, null, 2) + '\n');
  throw error;
} finally {
  if (frozenFixture && alive(frozenFixture)) process.kill(frozenFixture, 'SIGCONT');
  if (frozen && alive(frozen)) process.kill(frozen, 'SIGCONT');
  await supervisor?.shutdown();
  for (const proc of children) { proc.stdin?.end(); if (alive(proc.pid)) proc.kill('SIGTERM'); }
  await Promise.all(children.map((proc) => proc.exitCode != null || proc.signalCode != null ? undefined : new Promise((done) => proc.once('close', done))));
}
