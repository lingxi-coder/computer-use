#!/usr/bin/env node
import { nativeFixtureBindingEvidence, buildNativeFixtureContextCompiler } from './mvp/native-fixture-continuity.mjs';
buildNativeFixtureContextCompiler();
// One absolute PointerPosition, only inside serial run-owned fixture PIDs.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: {
  'build-only': { type: 'boolean', default: false },
  'skip-build': { type: 'boolean', default: false }
} });
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = mkdtempSync(join(tmpdir(), 'cu-owned-position-'));
const record = join(root, 'artifacts/m2-native-pointer-position.json');
const source = join(root, 'target/debug/cu-macos-source');
const helper = join(root, 'target/debug/cu-macos-device-guard');
const device = join(root, 'target/debug/cu-device');
const examplePath = join(root, 'target/debug/examples/native_device_session');
const lostAckHelper = join(output, 'owned-position-missing-ack');
const children = [];
const proof = { ok: false, output, started_at: new Date().toISOString(), scenarios: [],
  real_provider_calls: 0, mvp_complete: false, physical_input_run: false, skip_build: values['skip-build'] };
let diagnosticFixture;
mkdirSync(dirname(record), { recursive: true });
if (existsSync(record)) {
  const previous = join(dirname(record), `m2-native-pointer-position.previous-${new Date().toISOString().replace(/[:.]/g, '-')}-${randomUUID()}.json`);
  copyFileSync(record, previous); proof.previous_artifact = previous;
}
const save = () => writeFileSync(record, JSON.stringify(proof, null, 2) + '\n');
const sha = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(read, reason, timeout = 6000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const value = await read(); if (value) return value; await pause(10); }
  throw new Error('Timeout: ' + reason);
}
function child(program, args, label) {
  const proc = spawn(program, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  const entry = { proc, label, lines: [], stdout: '', stderr: '', error: null };
  children.push(entry); proc.stdin.on('error', () => {});
  proc.on('error', error => { entry.error = String(error); });
  let pending = '';
  proc.stdout.on('data', chunk => {
    entry.stdout += chunk; pending += chunk;
    writeFileSync(join(output, label + '.stdout.jsonl'), entry.stdout);
    if (pending.length > 768 * 1024) { entry.error = 'child exceeded line budget'; return; }
    for (let i; (i = pending.indexOf('\n')) >= 0;) {
      try { entry.lines.push(JSON.parse(pending.slice(0, i))); }
      catch (error) { entry.error = String(error); }
      pending = pending.slice(i + 1);
    }
  });
  proc.stderr.on('data', chunk => {
    entry.stderr += chunk; writeFileSync(join(output, label + '.stderr.txt'), entry.stderr);
  });
  return entry;
}
function healthy(entry) {
  assert.equal(entry.error, null, entry.error);
  if (entry.proc.exitCode != null && entry.proc.exitCode !== 0) throw new Error(entry.stderr || `${entry.label} exited ${entry.proc.exitCode}`);
}
async function command(fixture, name) {
  healthy(fixture); assert.equal(fixture.proc.exitCode, null, 'fixture already exited');
  const start = fixture.lines.length; fixture.proc.stdin.write(name + '\n');
  return until(() => { healthy(fixture); return fixture.lines.slice(start).find(line => line.kind === name); }, fixture.label + ' ' + name);
}
async function closeFixture(fixture) {
  if (fixture.proc.exitCode == null && fixture.proc.signalCode == null) {
    fixture.proc.stdin.write('quit\n');
    await until(() => fixture.proc.exitCode != null || fixture.proc.signalCode != null, fixture.label + ' close');
  }
}
function observe(ready, label) {
  const assets = mkdtempSync(join(output, label + '-source-'));
  const request = { protocol_version: 1, command: 'observe', bundle_id: ready.bundle_id, pid: ready.pid, output_dir: assets, timeout_ms: 3000 };
  writeFileSync(join(assets, 'request.json'), JSON.stringify(request) + '\n');
  try {
    const stdout = execFileSync(source, [], { input: JSON.stringify(request), encoding: 'utf8', timeout: 6000, maxBuffer: 768 * 1024 });
    writeFileSync(join(assets, 'response.json'), stdout);
    const value = JSON.parse(stdout);
    proof.observations ??= []; proof.observations.push({ label, assets,
      hashes: Object.fromEntries(readdirSync(assets).map(name => [name, sha(join(assets, name))])),
      complete: value.structure?.complete ?? false, errors: value.errors ?? [] }); save();
    return value;
  } catch (error) {
    writeFileSync(join(assets, 'failure.txt'), String(error));
    if (error.stdout) writeFileSync(join(assets, 'response.json'), error.stdout);
    if (error.stderr) writeFileSync(join(assets, 'stderr.txt'), error.stderr);
    proof.source_read_failures ??= []; proof.source_read_failures.push({ label, assets, error: String(error) }); save(); throw error;
  }
}
async function freshObservation(ready, label, complete = true) {
  // Readonly retries retain every failed observation; physical input never retries.
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      const value = observe(ready, `${label}-${attempt}`);
      if (complete) assert.equal(value.structure?.complete, true, value.structure?.incomplete_reason);
      return value;
    } catch (error) {
      proof.source_read_failures ??= []; proof.source_read_failures.push({ label, attempt, error: String(error) }); save();
      if (attempt === 2) throw error;
      await pause(100);
    }
  }
}
const normalize = text => text.split(/[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u).filter(Boolean).join(' ').toLowerCase();
const round = n => Math.sign(n) * Math.floor(Math.abs(n) + 0.5);
function binding(structure, ready, scenario) {
  assert.equal(structure.app.pid, ready.pid); assert.equal(structure.app.bundle_id, ready.bundle_id);
  const objects = structure.objects.filter(object => object.role === 'AXScrollArea' && object.window_id === `window-${ready.window_id}` &&
    ['x', 'y', 'width', 'height'].every(axis => Math.abs(object.frame[axis] - ready.scroll_frame[axis]) < 0.5));
  assert.equal(objects.length, 1, 'unique fresh Source scroll-area binding');
  const object = objects[0], ancestors = []; let current = object.parent;
  while (current) {
    const parent = structure.objects.find(item => item.instance_ref === current); assert.ok(parent);
    ancestors.push(parent.role); current = parent.parent; assert.ok(ancestors.length <= 32);
  }
  const fingerprint = { app: structure.app.bundle_id, app_pid: structure.app.pid, window_id: object.window_id,
    ancestor_roles: ancestors.reverse(), group: object.group == null ? null : normalize(object.group), role: object.role,
    subrole: object.subrole ?? null, label: normalize(object.title ?? object.description ?? ''),
    sibling_index: structure.objects.filter(item => item.parent === object.parent && item.window_id === object.window_id).findIndex(item => item.instance_ref === object.instance_ref),
    frame: [object.frame.x, object.frame.y, object.frame.width, object.frame.height].map(round), scroll_container: null };
  return { binding: 'owned-position-scroll-area', instance: object.instance_ref, snapshot_id: 'owned-' + scenario,
    environment_revision: 1, layer_revision: 1, fingerprint, display_frame: object.frame, ...nativeFixtureBindingEvidence(structure, object, 1) };
}
function unchangedEffects(before, after) {
  for (const key of ['pointer_down_calls', 'pointer_up_calls', 'raw_scroll_calls', 'scroll_calls', 'navigation_calls',
    'document_navigation_calls', 'other_navigation_calls', 'destination_action_calls', 'value_calls', 'press_calls']) {
    assert.equal(before[key], 0, 'no setup input: ' + key); assert.equal(after[key], 0, 'no extra input: ' + key);
  }
  for (const key of ['offset_x', 'offset_y', 'visible_offset', 'page', 'page_label', 'value', 'destination_value']) assert.deepEqual(after[key], before[key], key + ' unchanged');
  assert.deepEqual(after.scroll_events, []); assert.deepEqual(after.scrollbar_values, before.scrollbar_values);
}
save();
try {
  if (values['skip-build']) {
    for (const path of [source, helper, device, examplePath]) assert.ok(existsSync(path), 'required existing binary missing: ' + path);
  } else {
    execFileSync('bash', ['scripts/build-macos-source.sh'], { cwd: root, stdio: 'inherit' });
    execFileSync('bash', ['scripts/build-macos-device.sh'], { cwd: root, stdio: 'inherit' });
    execFileSync('cargo', ['build', '--locked', '-p', 'device-macos', '--bin', 'cu-device', '--example', 'native_device_session'], { cwd: root, stdio: 'inherit' });
  }
  mkdirSync(join(root, 'target/native-macos'), { recursive: true });
  const bundleDirectory = mkdtempSync(join(root, 'target/native-macos/position-owned-'));
  const contents = join(bundleDirectory, 'HostScrollFixture.app/Contents');
  const fixturePath = join(contents, 'MacOS/HostScrollFixture'); mkdirSync(dirname(fixturePath), { recursive: true });
  writeFileSync(join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.lingxi.computer-user.guard-fixture</string>
<key>CFBundleExecutable</key><string>HostScrollFixture</string><key>CFBundleName</key><string>Computer Use Position Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
  const swiftFlags = ['-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache')];
  execFileSync('xcrun', ['swiftc', ...swiftFlags, join(root, 'native/device/fixtures/HostScrollFixture.swift'), '-o', fixturePath], { stdio: 'inherit' });
  execFileSync('xcrun', ['swiftc', ...swiftFlags, '-D', 'CU_OWNED_POINTER_DROP_ACK',
    ...readdirSync(join(root, 'native/device')).filter(name => name.endsWith('.swift')).sort().map(name => join(root, 'native/device', name)), '-o', lostAckHelper], { stdio: 'inherit' });
  proof.build = Object.fromEntries([source, helper, device, examplePath, fixturePath, lostAckHelper].map(path => [path, sha(path)]));
  const sourceFiles = ['scripts/smoke-macos-position.mjs', 'crates/device-macos/examples/native_device_session.rs',
    'crates/device-macos/examples/support/native_session.rs', 'crates/device-macos/src/guard/authority.rs',
    'crates/device-macos/src/backend/native.rs', 'crates/execution-core/src/core.rs',
    'crates/interaction-contracts/src/desktop/primitive.rs', 'contracts/desktop-v2/schema.json', 'native/device/fixtures/HostScrollFixture.swift',
    ...readdirSync(join(root, 'native/device')).filter(name => name.endsWith('.swift')).sort().map(name => 'native/device/' + name),
    ...readdirSync(join(root, 'native/macos')).filter(name => name.endsWith('.swift')).sort().map(name => 'native/macos/' + name)];
  proof.source_hashes = Object.fromEntries(sourceFiles.map(path => [path, sha(join(root, path))]));
  proof.git_head = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  proof.schema_sha256 = sha(join(root, 'contracts/desktop-v2/schema.json')); save();
  if (!values['build-only']) {
    proof.physical_input_run = true; save();
    for (const scenario of ['position-applied', 'position-occluded', 'position-no-release-ack']) {
      const fixture = child(fixturePath, [], scenario + '-fixture'); diagnosticFixture = fixture;
      const ready = await until(() => { healthy(fixture); return fixture.lines.find(line => line.kind === 'ready'); }, 'owned fixture ready');
      try {
        await command(fixture, 'focus-window'); const before = await command(fixture, 'stats');
        assert.equal(before.app_active, true); assert.equal(before.window_key, true);
        assert.deepEqual(before.input_events, [], 'retain all ambient input; no preparatory click');
        const observation = await freshObservation(ready, scenario + '-before');
        const target = binding(observation.structure, ready, scenario);
        const path = join(output, scenario + '-proof.json'); writeFileSync(path, JSON.stringify(target), { mode: 0o600 });
        const expectedPoint = { x: round(target.display_frame.x + target.display_frame.width / 2),
          y: round(target.display_frame.y + target.display_frame.height / 2) };
        if (scenario === 'position-occluded') await command(fixture, 'occlude');
        const selectedHelper = scenario === 'position-no-release-ack' ? lostAckHelper : helper;
        const driver = child(examplePath, [device, selectedHelper, path, scenario], scenario + '-session');
        const result = await until(() => { healthy(driver); return driver.lines.find(line => line.kind === 'production_result'); }, scenario + ' receipt', 8000);
        proof.current_result = { scenario, result }; save();
        await until(() => { healthy(driver); return driver.proc.exitCode === 0; }, scenario + ' session close');
        assert.equal(result.production_core_native_backend_connected, true); assert.equal(result.simulated_input_used, false);
        assert.equal(result.duplicate_not_replayed, true);
        await pause(250); const after = await command(fixture, 'stats');
        unchangedEffects(before, after);
        const rejected = scenario === 'position-occluded';
        assert.equal(after.input_events.length, rejected ? 0 : 1, 'all unfiltered raw input, including duplicate/new-intent settlement');
        if (rejected) {
          assert.equal(result.receipt_status, 'rejected'); assert.equal(result.submitted_count, 0); assert.equal(result.reason_code, 'target_occluded');
        } else {
          const event = after.input_events[0]; assert.equal(event.type, 5); assert.notEqual(event.marker, '0');
          assert.equal(event.flags, 0); assert.ok(Number.isFinite(event.timestamp) && event.timestamp > 0);
          assert.equal(result.submitted_count, 1);
          if (scenario === 'position-no-release-ack') {
            assert.equal(result.receipt_status, 'unknown'); assert.equal(result.reason_code, 'effect_unknown');
            assert.equal(result.channel_isolated, true); assert.equal(result.new_intent_rejected_after_unknown, true);
          } else {
            assert.equal(result.receipt_status, 'completed'); assert.ok(result.input_end_ms <= result.command_end_ms);
            assert.equal(result.real_native_pointer_position_input_verified, true);
          }
        }
        const effect = await freshObservation(ready, scenario + '-after', !rejected);
        if (!rejected) assert.ok(effect.structure.objects.some(object => [object.title, object.description, object.value?.value].includes(before.visible_offset)), 'fresh Source unchanged offset label');
        proof.scenarios.push({ scenario, target: ready, before, result, after, expected_authorized_point: expectedPoint,
          fixture_event_coordinates_available: false, independent_cursor_position_verified: false,
          native_helper_cursor_position_verified: scenario === 'position-applied',
          native_helper_verification_basis: 'Completed requires session-tap acknowledgement and exact CGEvent(source:nil) cursor match before original deadline; fixture records no event coordinates.' }); save();
      } catch (error) {
        try { proof.failure_diagnostics = await command(fixture, 'stats'); } catch (diagnostic) { proof.diagnostic_error = String(diagnostic); }
        save(); throw error;
      } finally { await closeFixture(fixture); diagnosticFixture = null; }
    }
  }
  proof.ok = true; proof.tested_at = new Date().toISOString(); proof.build_only = values['build-only'];
  proof.limits = ['Owned native fixture only; no preparatory click, cursor warp, button, scrolling, navigation or checkbox input.',
    'Fixture exposes no event coordinates or system cursor readback; exact position is checked by the native helper completion contract.',
    'No real model, user app, full Host workflow or signed packaging acceptance.'];
  save(); console.log(JSON.stringify({ ok: true, record, output, scenarios: proof.scenarios.length, build_only: proof.build_only }));
} catch (error) {
  proof.error = String(error);
  try { if (diagnosticFixture) proof.failure_diagnostics = await command(diagnosticFixture, 'stats'); } catch (diagnostic) { proof.diagnostic_error = String(diagnostic); }
  proof.children = children.map(entry => ({ label: entry.label, pid: entry.proc.pid, exit_code: entry.proc.exitCode,
    signal: entry.proc.signalCode, error: entry.error, lines: entry.lines, stderr: entry.stderr })); save(); throw error;
} finally {
  for (const entry of children) {
    entry.proc.stdin.destroy(); if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGKILL');
  }
  await Promise.all(children.map(entry => entry.proc.exitCode != null || entry.proc.signalCode != null ? undefined : new Promise(resolve => entry.proc.once('exit', resolve))));
}
