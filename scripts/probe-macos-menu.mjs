#!/usr/bin/env node
import { nativeFixtureBindingEvidence, buildNativeFixtureContextCompiler } from './mvp/native-fixture-continuity.mjs';
buildNativeFixtureContextCompiler();
// One production ShowMenu; subsequent AX/WindowServer relationship reads only.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: {
  'skip-build': { type: 'boolean', default: true }, 'build-only': { type: 'boolean', default: false }
} });
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = mkdtempSync(join(tmpdir(), 'cu-owned-menu-probe-'));
const record = join(root, 'artifacts/m3-native-menu-relationship-probe.json');
const source = join(root, 'target/debug/cu-macos-source');
const helper = join(root, 'target/debug/cu-macos-device-guard');
const device = join(root, 'target/debug/cu-device');
const example = join(root, 'target/debug/examples/native_device_session');
const evidence = { ok: false, output, started_at: new Date().toISOString(), skip_build: values['skip-build'],
  real_provider_calls: 0, mvp_complete: false, menu_selection_verified: false, input_authorized_by_probe: false,
  physical_input_run: false, observations: [] };
const children = [];
let fixture;
mkdirSync(dirname(record), { recursive: true });
if (existsSync(record)) {
  const previous = join(dirname(record), `m3-native-menu-relationship-probe.previous-${new Date().toISOString().replace(/[:.]/g, '-')}-${randomUUID()}.json`);
  copyFileSync(record, previous); evidence.previous_artifact = previous;
}
const save = () => writeFileSync(record, JSON.stringify(evidence, null, 2) + '\n');
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
    entry.stdout += chunk; pending += chunk; writeFileSync(join(output, label + '.stdout.jsonl'), entry.stdout);
    if (pending.length > 768 * 1024) { entry.error = 'child exceeded line budget'; return; }
    for (let i; (i = pending.indexOf('\n')) >= 0;) {
      try { entry.lines.push(JSON.parse(pending.slice(0, i))); } catch (error) { entry.error = String(error); }
      pending = pending.slice(i + 1);
    }
  });
  proc.stderr.on('data', chunk => { entry.stderr += chunk; writeFileSync(join(output, label + '.stderr.txt'), entry.stderr); });
  return entry;
}
function healthy(entry) {
  assert.equal(entry.error, null, entry.error);
  if (entry.proc.exitCode != null && entry.proc.exitCode !== 0) throw new Error(entry.stderr || `${entry.label} exited ${entry.proc.exitCode}`);
}
async function command(name) {
  healthy(fixture); assert.equal(fixture.proc.exitCode, null, 'owned fixture already exited');
  const start = fixture.lines.length; fixture.proc.stdin.write(name + '\n');
  return until(() => { healthy(fixture); return fixture.lines.slice(start).find(line => line.kind === name); }, 'owned fixture ' + name);
}
function observe(ready, label) {
  const assets = mkdtempSync(join(output, label + '-source-'));
  const request = { protocol_version: 1, command: 'observe', bundle_id: ready.bundle_id, pid: ready.pid,
    output_dir: assets, timeout_ms: 3000 };
  writeFileSync(join(assets, 'request.json'), JSON.stringify(request) + '\n');
  try {
    const stdout = execFileSync(source, [], { input: JSON.stringify(request), encoding: 'utf8', timeout: 6000, maxBuffer: 768 * 1024 });
    writeFileSync(join(assets, 'response.json'), stdout); const value = JSON.parse(stdout);
    evidence.observations.push({ label, assets, complete: value.structure?.complete ?? false,
      errors: value.errors ?? [], hashes: Object.fromEntries(readdirSync(assets).map(name => [name, sha(join(assets, name))])) }); save();
    return value;
  } catch (error) {
    writeFileSync(join(assets, 'failure.txt'), String(error));
    if (error.stdout) writeFileSync(join(assets, 'response.json'), error.stdout);
    if (error.stderr) writeFileSync(join(assets, 'stderr.txt'), error.stderr);
    evidence.source_failures ??= []; evidence.source_failures.push({ assets, label, error: String(error) }); save(); throw error;
  }
}
async function freshSource(ready) {
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try { const value = observe(ready, 'before-' + attempt); assert.equal(value.structure?.complete, true, value.structure?.incomplete_reason); return value; }
    catch (error) {
      evidence.source_failures ??= []; evidence.source_failures.push({ attempt, error: String(error) }); save();
      if (attempt === 2) throw error;
      await pause(100);
    }
  }
}
const normalize = text => text.split(/[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u).filter(Boolean).join(' ').toLowerCase();
const round = n => Math.sign(n) * Math.floor(Math.abs(n) + 0.5);
function binding(structure, ready) {
  assert.equal(structure.app.pid, ready.pid); assert.equal(structure.app.bundle_id, ready.bundle_id);
  const matches = structure.objects.filter(object => object.role === 'AXMenuButton' && object.window_id === `window-${ready.window_id}` &&
    ['x', 'y', 'width', 'height'].every(axis => Math.abs(object.frame[axis] - ready.frame[axis]) < 0.5));
  assert.equal(matches.length, 1, 'unique fresh Source AXMenuButton');
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
  return { binding: 'owned-menu-probe-trigger', instance: object.instance_ref, snapshot_id: 'owned-menu-probe-before',
    environment_revision: 1, layer_revision: 1, fingerprint, display_frame: object.frame, ...nativeFixtureBindingEvidence(structure, object, 1) };
}
save();
try {
  // The final production binaries are intentionally reused without Cargo or
  // shared-helper builds. Only this run's fixture and readonly probe compile.
  for (const path of [source, helper, device, example]) assert.ok(existsSync(path), 'required existing binary missing: ' + path);
  mkdirSync(join(root, 'target/native-macos'), { recursive: true });
  const directory = mkdtempSync(join(root, 'target/native-macos/menu-probe-owned-'));
  const contents = join(directory, 'MenuFixture.app/Contents');
  const fixturePath = join(contents, 'MacOS/MenuFixture'), probePath = join(directory, 'MenuReadProbe');
  mkdirSync(dirname(fixturePath), { recursive: true });
  writeFileSync(join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.lingxi.computer-user.guard-fixture</string>
<key>CFBundleExecutable</key><string>MenuFixture</string><key>CFBundleName</key><string>Computer Use Menu Probe Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
  const flags = ['-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache')];
  for (const [name, binary] of [['MenuFixture', fixturePath], ['MenuReadProbe', probePath]]) {
    execFileSync('xcrun', ['swiftc', ...flags, join(root, 'native/device/fixtures', name + '.swift'), '-o', binary], { stdio: 'inherit' });
  }
  evidence.build = Object.fromEntries([source, helper, device, example, fixturePath, probePath].map(path => [path, sha(path)]));
  const files = ['scripts/probe-macos-menu.mjs', 'native/device/fixtures/MenuFixture.swift', 'native/device/fixtures/MenuReadProbe.swift',
    'crates/device-macos/examples/native_device_session.rs', 'crates/device-macos/examples/support/native_session.rs',
    'crates/device-macos/src/guard/authority.rs', 'crates/device-macos/src/backend/native.rs', 'crates/execution-core/src/core.rs',
    'contracts/desktop-v2/schema.json', ...readdirSync(join(root, 'native/device')).filter(name => name.endsWith('.swift')).sort().map(name => 'native/device/' + name),
    ...readdirSync(join(root, 'native/macos')).filter(name => name.endsWith('.swift')).sort().map(name => 'native/macos/' + name)];
  evidence.source_hashes = Object.fromEntries(files.map(path => [path, sha(join(root, path))]));
  evidence.git_head = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  evidence.schema_sha256 = sha(join(root, 'contracts/desktop-v2/schema.json')); save();
  if (!values['build-only']) {
    fixture = child(fixturePath, [], 'menu-fixture');
    const ready = await until(() => { healthy(fixture); return fixture.lines.find(line => line.kind === 'ready'); }, 'owned menu ready');
    assert.equal(ready.pid, fixture.proc.pid); assert.equal(ready.bundle_id, 'com.lingxi.computer-user.guard-fixture');
    evidence.target = ready;
    await command('accept-menu'); await command('focus-window'); evidence.before = await command('stats'); save();
    assert.equal(evidence.before.action_calls, 0); assert.equal(evidence.before.selection_calls, 0);
    assert.equal(evidence.before.app_active, true); assert.equal(evidence.before.window_key, true);
    const observed = await freshSource(ready), target = binding(observed.structure, ready);
    const path = join(output, 'show-menu-proof.json'); writeFileSync(path, JSON.stringify(target), { mode: 0o600 });
    evidence.source_binding = target; evidence.physical_input_run = true; save();
    const driver = child(example, [device, helper, path, 'menu-applied'], 'show-menu-session');
    evidence.production_receipt = await until(() => { healthy(driver); return driver.lines.find(line => line.kind === 'production_result'); }, 'single ShowMenu production receipt', 8000); save();
    await until(() => { healthy(driver); return driver.proc.exitCode === 0; }, 'production session closed');
    assert.equal(evidence.production_receipt.receipt_status, 'completed'); assert.equal(evidence.production_receipt.submitted_count, 1);
    assert.equal(evidence.production_receipt.duplicate_not_replayed, true); assert.equal(evidence.production_receipt.simulated_input_used, false);
    evidence.after_open = await until(async () => { const stats = await command('stats'); return stats.menu_visibility_verified ? stats : false; }, 'actual visible native menu'); save();
    assert.equal(evidence.after_open.action_calls, 1); assert.equal(evidence.after_open.selection_calls, 0);
    const request = { pid: ready.pid, bundle_id: ready.bundle_id, executable_path: fixturePath,
      parent_pid: process.pid, origin_window_id: ready.window_id };
    writeFileSync(join(output, 'probe-request.json'), JSON.stringify(request) + '\n');
    try {
      const raw = execFileSync(probePath, [], { input: JSON.stringify(request), encoding: 'utf8', timeout: 7000, maxBuffer: 768 * 1024 });
      writeFileSync(join(output, 'probe-response.json'), raw); evidence.probe = JSON.parse(raw);
      assert.equal(evidence.probe.input_authorized, false); assert.equal(evidence.probe.process_identity_revalidated, true);
    } catch (error) {
      if (error.stdout) writeFileSync(join(output, 'probe-response.json'), error.stdout);
      if (error.stderr) writeFileSync(join(output, 'probe-stderr.txt'), error.stderr);
      evidence.probe_failure = String(error); save(); throw error;
    }
    evidence.after_probe = await command('stats');
    assert.equal(evidence.after_probe.action_calls, 1); assert.equal(evidence.after_probe.selection_calls, 0);
    // This source read is preserved even when a native menu leaves Source's
    // active standard-window projection incomplete. It grants no authority.
    evidence.source_after_menu = observe(ready, 'after-menu');
    await command('close-menu'); evidence.after_close = await command('stats');
    assert.equal(evidence.after_close.action_calls, 1); assert.equal(evidence.after_close.selection_calls, 0);
    assert.equal(evidence.after_close.tracking, false);
    evidence.probe_hashes = Object.fromEntries(['probe-request.json', 'probe-response.json', 'show-menu-proof.json'].map(name => [name, sha(join(output, name))]));
    // Refuse to describe another build as the binaries used by this run.
    for (const [binary, hash] of Object.entries(evidence.build)) assert.equal(sha(binary), hash, 'binary changed during menu diagnosis: ' + binary);
    save();
  }
  evidence.ok = true; evidence.tested_at = new Date().toISOString(); evidence.build_only = values['build-only'];
  evidence.limits = ['Relationships are actual readonly facts, including unsupported AX attributes and foreign-PID hit failures.',
    'Process-local diagnostic IDs do not authorize menu input or establish a new binding.', 'No menu selection, click, key input, cursor warp, model call or full MVP acceptance.'];
  save(); console.log(JSON.stringify({ ok: true, record, output, build_only: evidence.build_only }));
} catch (error) {
  evidence.error = String(error);
  try { if (fixture) evidence.failure_stats = await command('stats'); } catch (diagnostic) { evidence.diagnostic_error = String(diagnostic); }
  evidence.children = children.map(entry => ({ label: entry.label, pid: entry.proc.pid, exit_code: entry.proc.exitCode,
    signal: entry.proc.signalCode, error: entry.error, lines: entry.lines, stderr: entry.stderr })); save(); throw error;
} finally {
  if (fixture?.proc.exitCode == null && fixture?.proc.signalCode == null) {
    try { fixture.proc.stdin.write('quit\n'); await until(() => fixture.proc.exitCode != null || fixture.proc.signalCode != null, 'owned fixture close'); } catch {}
  }
  for (const entry of children) {
    entry.proc.stdin.destroy(); if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGKILL');
  }
  await Promise.all(children.map(entry => entry.proc.exitCode != null || entry.proc.signalCode != null ? undefined : new Promise(resolve => entry.proc.once('exit', resolve))));
}
