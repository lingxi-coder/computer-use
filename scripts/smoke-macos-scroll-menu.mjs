#!/usr/bin/env node
import { nativeFixtureBindingEvidence, buildNativeFixtureContextCompiler } from './mvp/native-fixture-continuity.mjs';
buildNativeFixtureContextCompiler();
// Serial physical acceptance only against run-owned Scroll/Menu fixtures.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: { 'build-only': { type: 'boolean', default: false }, 'skip-build': { type: 'boolean', default: false } } });
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = mkdtempSync(join(tmpdir(), 'cu-owned-scroll-menu-'));
const record = join(root, 'artifacts/m3-native-scroll-menu.json');
const helper = join(root, 'target/debug/cu-macos-device-guard');
const device = join(root, 'target/debug/cu-device');
const source = join(root, 'target/debug/cu-macos-source');
const examplePath = join(root, 'target/debug/examples/native_device_session');
const lostAckHelper = join(output, 'owned-scroll-missing-ack');
const children = [];
const proof = { ok: false, started_at: new Date().toISOString(), output, scenarios: [],
  real_provider_calls: 0, mvp_complete: false, physical_input_run: false, source_menu_authority_verified: false };
let diagnosticFixture;
mkdirSync(dirname(record), { recursive: true });
if (existsSync(record)) {
  const previous = join(dirname(record), `m3-native-scroll-menu.previous-${new Date().toISOString().replace(/[:.]/g, '-')}-${randomUUID()}.json`);
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
  const entry = { proc, lines: [], stdout: '', stderr: '', parse_error: null, label };
  children.push(entry); proc.stdin.on('error', () => {});
  proc.on('error', error => { entry.parse_error = String(error); });
  let pending = '';
  proc.stdout.on('data', chunk => {
    entry.stdout += chunk; pending += chunk;
    writeFileSync(join(output, label + '.stdout.jsonl'), entry.stdout);
    if (pending.length > 768 * 1024) { entry.parse_error = 'child output exceeded line budget'; return; }
    for (let i; (i = pending.indexOf('\n')) >= 0;) {
      try { entry.lines.push(JSON.parse(pending.slice(0, i))); }
      catch (error) { entry.parse_error = String(error); }
      pending = pending.slice(i + 1);
    }
  });
  proc.stderr.on('data', chunk => {
    entry.stderr += chunk; writeFileSync(join(output, label + '.stderr.txt'), entry.stderr);
  });
  return entry;
}
function healthy(entry) {
  assert.equal(entry.parse_error, null, entry.parse_error);
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
function buildFixture(directory, name) {
  const contents = join(directory, name + '.app/Contents');
  const binary = join(contents, 'MacOS', name);
  mkdirSync(dirname(binary), { recursive: true });
  writeFileSync(join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.lingxi.computer-user.guard-fixture</string>
<key>CFBundleExecutable</key><string>${name}</string><key>CFBundleName</key><string>Computer Use ${name}</string>
<key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
  execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'),
    join(root, 'native/device/fixtures', name + '.swift'), '-o', binary], { stdio: 'inherit' });
  return binary;
}
const normalize = text => text.split(/[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u).filter(Boolean).join(' ').toLowerCase();
const round = n => Math.sign(n) * Math.floor(Math.abs(n) + 0.5);
function observe(ready, label) {
  const assets = mkdtempSync(join(output, label + '-source-'));
  const request = { protocol_version: 1, command: 'observe', bundle_id: ready.bundle_id, pid: ready.pid, output_dir: assets, timeout_ms: 3000 };
  writeFileSync(join(assets, 'request.json'), JSON.stringify(request) + '\n');
  let stdout;
  try {
    stdout = execFileSync(source, [], { input: JSON.stringify(request), encoding: 'utf8', timeout: 6000, maxBuffer: 768 * 1024 });
    writeFileSync(join(assets, 'response.json'), stdout);
    const value = JSON.parse(stdout);
    const hashes = Object.fromEntries(readdirSync(assets).map(name => [name, sha(join(assets, name))]));
    proof.observations ??= []; proof.observations.push({ label, assets, hashes, complete: value.structure?.complete ?? false, errors: value.errors ?? [] }); save();
    return value;
  } catch (error) {
    writeFileSync(join(assets, 'failure.txt'), String(error));
    if (error.stdout) writeFileSync(join(assets, 'response.json'), error.stdout);
    if (error.stderr) writeFileSync(join(assets, 'stderr.txt'), error.stderr);
    proof.source_read_failures ??= []; proof.source_read_failures.push({ label, assets, error: String(error) }); save(); throw error;
  }
}
async function freshObservation(ready, label, requireComplete = true) {
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      const value = observe(ready, `${label}-${attempt}`);
      if (requireComplete) assert.equal(value.structure?.complete, true, value.structure?.incomplete_reason);
      return value;
    } catch (error) {
      proof.source_read_failures ??= []; proof.source_read_failures.push({ label, attempt, error: String(error) }); save();
      if (attempt === 2) throw error;
      await pause(100);
    }
  }
}
function binding(structure, ready, role, scenario) {
  assert.equal(structure.app.pid, ready.pid); assert.equal(structure.app.bundle_id, ready.bundle_id);
  const matches = structure.objects.filter(object => object.role === role && object.window_id === `window-${ready.window_id}` &&
    ['x', 'y', 'width', 'height'].every(axis => Math.abs(object.frame[axis] - ready.frame[axis]) < 0.5));
  assert.equal(matches.length, 1, 'unique fresh Source target');
  const object = matches[0], ancestors = []; let current = object.parent;
  while (current) {
    const parent = structure.objects.find(item => item.instance_ref === current); assert.ok(parent);
    ancestors.push(parent.role); current = parent.parent; assert.ok(ancestors.length <= 32);
  }
  const fingerprint = { app: structure.app.bundle_id, app_pid: structure.app.pid, window_id: object.window_id,
    ancestor_roles: ancestors.reverse(), group: object.group == null ? null : normalize(object.group), role: object.role,
    subrole: object.subrole ?? null, label: normalize(object.title ?? object.description ?? ''),
    sibling_index: structure.objects.filter(item => item.parent === object.parent && item.window_id === object.window_id).findIndex(item => item.instance_ref === object.instance_ref),
    frame: [object.frame.x, object.frame.y, object.frame.width, object.frame.height].map(round), scroll_container: null };
  return { binding: 'owned-' + role.toLowerCase(), instance: object.instance_ref, snapshot_id: 'owned-' + scenario,
    environment_revision: 1, layer_revision: 1, fingerprint, display_frame: object.frame, ...nativeFixtureBindingEvidence(structure, object, 1) };
}
async function runInput(fixture, ready, role, scenario, afterProof, selectedHelper = helper) {
  const label = fixture.label + '-' + scenario;
  const observed = await freshObservation(ready, label + '-before');
  const path = join(output, label + '-proof.json');
  writeFileSync(path, JSON.stringify(binding(observed.structure, ready, role, label)), { mode: 0o600 });
  if (afterProof) await afterProof();
  const driver = child(examplePath, [device, selectedHelper, path, scenario], label + '-session');
  const result = await until(() => { healthy(driver); return driver.lines.find(line => line.kind === 'production_result'); }, scenario + ' native receipt', 8000);
  proof.current_result = { scenario, result }; save();
  await until(() => { healthy(driver); return driver.proc.exitCode === 0; }, scenario + ' session close');
  assert.equal(result.production_core_native_backend_connected, true);
  assert.equal(result.simulated_input_used, false); assert.equal(result.duplicate_not_replayed, true);
  return result;
}
function assertOffsetSource(value, stats) {
  const areas = value.structure.objects.filter(object => object.role === 'AXScrollArea');
  assert.equal(areas.length, 1);
  for (const axis of ['horizontal', 'vertical']) {
    const position = areas[0].scroll?.[axis];
    assert.ok(position && Number.isFinite(position.value), 'Source preserves actual NSScroller position');
    assert.ok(Math.abs(position.value - stats.scrollbar_values[axis]) < 1e-9, 'Source AXValue matches independent actual NSScroller.doubleValue');
    assert.ok(position.min == null && position.max == null, 'unsupported NSScroller bounds stay unknown');
  }
  assert.ok(value.structure.objects.some(object => [object.title, object.description, object.value?.value].includes(stats.visible_offset)),
    'fresh Source contains the actual NSScrollView offset label');
}
save();
try {
  if (values['skip-build']) {
    for (const path of [source, helper, device, examplePath]) assert.ok(existsSync(path), 'existing binary required: ' + path);
  } else {
    execFileSync('bash', ['scripts/build-macos-source.sh'], { cwd: root, stdio: 'inherit' });
    execFileSync('bash', ['scripts/build-macos-device.sh'], { cwd: root, stdio: 'inherit' });
    execFileSync('cargo', ['build', '--locked', '-p', 'device-macos', '--bin', 'cu-device', '--example', 'native_device_session'], { cwd: root, stdio: 'inherit' });
  }
  proof.skip_build = values['skip-build'];
  mkdirSync(join(root, 'target/native-macos'), { recursive: true });
  const bundleDirectory = mkdtempSync(join(root, 'target/native-macos/scroll-menu-owned-'));
  const scrollFixture = buildFixture(bundleDirectory, 'ScrollFixture');
  const menuFixture = buildFixture(bundleDirectory, 'MenuFixture');
  execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-D', 'CU_OWNED_POINTER_DROP_ACK', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'),
    ...readdirSync(join(root, 'native/device')).filter(name => name.endsWith('.swift')).sort().map(name => join(root, 'native/device', name)), '-o', lostAckHelper], { stdio: 'inherit' });
  proof.build = Object.fromEntries([source, helper, device, examplePath, scrollFixture, menuFixture, lostAckHelper].map(path => [path, sha(path)]));
  const sourceFiles = ['scripts/smoke-macos-scroll-menu.mjs', 'crates/device-macos/examples/native_device_session.rs',
    'crates/device-macos/src/guard/authority.rs', 'crates/device-macos/src/backend/native.rs',
    'crates/execution-core/src/core.rs', 'contracts/desktop-v2/schema.json',
    ...readdirSync(join(root, 'native/device')).filter(name => name.endsWith('.swift')).sort().map(name => 'native/device/' + name),
    'native/device/fixtures/ScrollFixture.swift', 'native/device/fixtures/MenuFixture.swift',
    ...readdirSync(join(root, 'native/macos')).filter(name => name.endsWith('.swift')).sort().map(name => 'native/macos/' + name)];
  proof.source_hashes = Object.fromEntries(sourceFiles.map(path => [path, sha(join(root, path))]));
  proof.git_head = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  proof.schema_sha256 = sha(join(root, 'contracts/desktop-v2/schema.json')); save();
  if (!values['build-only']) {
    proof.physical_input_run = true; save();
    const deltas = { 'scroll-down': [0, -120], 'scroll-up': [0, 120], 'scroll-right': [-120, 0], 'scroll-left': [120, 0], 'scroll-no-release-ack': [0, -120] };
    for (const scenario of [...Object.keys(deltas), 'scroll-occluded', 'scroll-stale', 'scroll-invalid']) {
      const fixture = child(scrollFixture, [], scenario + '-fixture'); diagnosticFixture = fixture;
      const ready = await until(() => { healthy(fixture); return fixture.lines.find(line => line.kind === 'ready'); }, 'owned scroll fixture ready');
      try {
        await command(fixture, 'reset'); await command(fixture, 'focus-window');
        const beforePreparation = await command(fixture, 'stats');
        assert.equal(beforePreparation.prepared, false);
        const preparation = await runInput(fixture, ready, 'AXButton', 'click-applied');
        assert.equal(preparation.receipt_status, 'completed'); assert.equal(preparation.submitted_count, 1);
        const prepared = await until(() => fixture.lines.find(line => line.kind === 'prepared'), 'actual preparation click release');
        assert.deepEqual(prepared.frame, ready.frame, 'exposed scroll area keeps the preparation click geometry');
        assert.equal(prepared.window_id, ready.window_id); assert.equal(prepared.pid, ready.pid);
        const before = await command(fixture, 'stats');
        assert.equal(before.pointer_down_calls, 1); assert.equal(before.pointer_up_calls, 1);
        assert.equal(before.raw_scroll_calls, 0); assert.equal(before.scroll_calls, 0);
        assert.equal(before.app_active, true); assert.equal(before.window_key, true);
        assertOffsetSource(await freshObservation(ready, scenario + '-prepared'), before);
        const result = await runInput(fixture, ready, 'AXScrollArea', scenario,
          scenario === 'scroll-occluded' ? () => command(fixture, 'occlude') : scenario === 'scroll-stale' ? () => command(fixture, 'rename-scroll') : undefined,
          scenario === 'scroll-no-release-ack' ? lostAckHelper : helper);
        const applied = scenario in deltas;
        if (applied) await until(() => fixture.lines.some(line => line.kind === 'scroll_event'), 'actual scroll delivery');
        // Receipts have already covered duplicate and post-unknown new input
        // rejection. This settlement read also catches any queued extra input.
        await pause(250);
        const after = await command(fixture, 'stats');
        assert.equal(after.pointer_down_calls, 1); assert.equal(after.pointer_up_calls, 1);
        assert.equal(after.raw_scroll_calls, applied ? 1 : 0); assert.equal(after.scroll_calls, applied ? 1 : 0);
        assert.deepEqual(after.input_events.map(event => event.type), applied ? [1, 2, 22] : [1, 2]);
        if (applied) {
          assert.equal(result.receipt_status, scenario === 'scroll-no-release-ack' ? 'unknown' : 'completed'); assert.equal(result.submitted_count, 1);
          if (scenario === 'scroll-no-release-ack') {
            assert.equal(result.channel_isolated, true); assert.equal(result.reason_code, 'effect_unknown');
            assert.equal(result.new_intent_rejected_after_unknown, true);
          } else assert.ok(result.input_end_ms <= result.command_end_ms);
          const [dx, dy] = deltas[scenario], event = after.scroll_events[0];
          assert.equal(after.scroll_events.length, 1); assert.equal(event.type, 22); assert.notEqual(event.marker, '0');
          assert.equal(event.marker, after.input_events.at(-1).marker); assert.equal(event.flags, 0);
          assert.equal(event.raw_dx, dx); assert.equal(event.raw_dy, dy); assert.equal(event.precise, true);
          assert.equal(event.phase, 0); assert.equal(event.momentum_phase, 0);
          if (dx) { assert.equal(after.offset_y, before.offset_y); assert.ok((after.offset_x - before.offset_x) * -dx > 0); }
          if (dy) { assert.equal(after.offset_x, before.offset_x); assert.ok((after.offset_y - before.offset_y) * -dy > 0); }
          assert.notEqual(after.visible_offset, before.visible_offset);
          assertOffsetSource(await freshObservation(ready, scenario + '-effect'), after);
        } else {
          assert.equal(result.receipt_status, 'rejected'); assert.equal(result.submitted_count, 0);
          assert.equal(after.offset_x, before.offset_x); assert.equal(after.offset_y, before.offset_y);
          if (scenario === 'scroll-occluded') assert.equal(result.reason_code, 'target_occluded');
        }
        proof.scenarios.push({ scenario, target: ready, preparation, before, result, after }); save();
      } finally { await closeFixture(fixture); diagnosticFixture = null; }
    }
    for (const scenario of ['menu-applied', 'menu-no-effect', 'menu-delay', 'menu-occluded']) {
      const fixture = child(menuFixture, [], scenario + '-fixture'); diagnosticFixture = fixture;
      const ready = await until(() => { healthy(fixture); return fixture.lines.find(line => line.kind === 'ready'); }, 'owned menu fixture ready');
      try {
        await command(fixture, scenario === 'menu-no-effect' ? 'no-effect-menu' : scenario === 'menu-delay' ? 'delay-menu' : 'accept-menu');
        await command(fixture, 'focus-window'); const before = await command(fixture, 'stats');
        assert.equal(before.action_calls, 0); assert.equal(before.tracking, false);
        assert.equal(before.menu_visibility_verified, false); assert.equal(before.visible_native_item_count, 0);
        assert.equal(before.app_active, true); assert.equal(before.window_key, true);
        const result = await runInput(fixture, ready, 'AXMenuButton', scenario,
          scenario === 'menu-occluded' ? () => command(fixture, 'occlude') : undefined);
        await pause(250);
        let after = await command(fixture, 'stats');
        if (scenario === 'menu-applied') after = await until(async () => {
          const stats = await command(fixture, 'stats'); return stats.menu_visibility_verified ? stats : false;
        }, 'real native menu visibility');
        assert.equal(after.action_calls, scenario === 'menu-occluded' ? 0 : 1);
        const menuSource = await freshObservation(ready, scenario + '-effect', false);
        if (scenario === 'menu-delay') {
          assert.equal(result.receipt_status, 'unknown'); assert.equal(result.submitted_count, 1);
          assert.equal(result.channel_isolated, true); assert.equal(result.reason_code, 'effect_unknown');
          assert.equal(result.new_intent_rejected_after_unknown, true);
        } else if (scenario === 'menu-occluded') {
          assert.equal(result.receipt_status, 'rejected'); assert.equal(result.submitted_count, 0);
          assert.equal(result.reason_code, 'target_occluded'); assert.equal(after.tracking, false);
          assert.equal(after.menu_visibility_verified, false); assert.equal(after.visible_native_item_count, 0);
        } else {
          assert.equal(result.receipt_status, 'completed'); assert.equal(result.submitted_count, 1);
          assert.ok(result.input_end_ms <= result.command_end_ms);
          if (scenario === 'menu-applied') {
            assert.equal(after.menu_visibility_verified, true);
            assert.equal(after.visible_native_item_count, 2); assert.ok(after.native_menu_windows.length > 0);
            assert.deepEqual(after.native_menu_ax_items.map(item => item.role), ['AXMenuItem', 'AXMenuItem']);
          } else {
            assert.equal(after.tracking, false);
            assert.equal(after.menu_visibility_verified, false); assert.equal(after.visible_native_item_count, 0);
          }
        }
        await command(fixture, 'close-menu'); await pause(100);
        const settled = await command(fixture, 'stats');
        assert.equal(settled.action_calls, after.action_calls); assert.equal(settled.selection_calls, 0);
        assert.equal(settled.tracking, false);
        // AppKit's readonly menu-item frame/layout queries can invoke menu
        // lifecycle delegates. Retain their raw baseline and deltas as
        // diagnostics; only action_calls and real visibility verify input.
        proof.scenarios.push({ scenario, target: ready, before, result, after, settled,
          menu_lifecycle_deltas: { opened: after.menu_will_open_calls - before.menu_will_open_calls,
            closed: after.menu_did_close_calls - before.menu_did_close_calls },
          lifecycle_callbacks_used_as_action_evidence: false,
          source_menu_items: menuSource.structure?.objects?.filter(object => object.role === 'AXMenuItem') ?? [],
          source_complete_during_menu: menuSource.structure?.complete ?? false }); save();
      } finally { await closeFixture(fixture); diagnosticFixture = null; }
    }
  }
  proof.ok = true; proof.tested_at = new Date().toISOString(); proof.build_only = values['build-only'];
  proof.limits = ['Owned serial native fixtures only; no Host business workflow, user app or provider call.',
    'Native menu visibility does not establish auxiliary menu-window authority for subsequent selection.',
    'Full MVP gates, real-model paths, TextEdit acceptance, recovery and signed packaging remain separate.'];
  save(); console.log(JSON.stringify({ ok: true, record, output, scenarios: proof.scenarios.length, build_only: proof.build_only }));
} catch (error) {
  proof.error = String(error);
  try { if (diagnosticFixture) proof.failure_diagnostics = await command(diagnosticFixture, 'stats'); }
  catch (diagnostic) { proof.diagnostic_error = String(diagnostic); }
  proof.children = children.map(entry => ({ label: entry.label, pid: entry.proc.pid, exit_code: entry.proc.exitCode,
    signal: entry.proc.signalCode, parse_error: entry.parse_error, lines: entry.lines, stderr: entry.stderr }));
  save(); throw error;
} finally {
  for (const entry of children) {
    entry.proc.stdin.destroy();
    if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGKILL');
  }
  await Promise.all(children.map(entry => entry.proc.exitCode != null || entry.proc.signalCode != null ? undefined : new Promise(resolve => entry.proc.once('exit', resolve))));
}
