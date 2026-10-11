#!/usr/bin/env node
import { nativeFixtureBindingEvidence, buildNativeFixtureContextCompiler } from './mvp/native-fixture-continuity.mjs';
buildNativeFixtureContextCompiler();
// Physical input only inside the GuardFixture created by this process.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: { death: { type: 'boolean' } } });
const death = values.death === true;

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = mkdtempSync(join(tmpdir(), 'cu-owned-click-'));
const record = join(root, death ? 'artifacts/m2-native-click-death.json' : 'artifacts/m2-native-click.json');
const helper = join(root, 'target/debug/cu-macos-device-guard');
const lostAckHelper = join(output, 'owned-missing-release-ack');
const device = join(root, 'target/debug/cu-device');
const fixturePath = join(root, 'target/native-macos/GuardFixture.app/Contents/MacOS/GuardFixture');
const examplePath = join(root, 'target/debug/examples/native_device_session');
const proof = { ok: false, output, scenarios: [], real_provider_calls: 0, mvp_complete: false };
const save = () => writeFileSync(record, JSON.stringify(proof, null, 2) + '\n');
const children = [];
let diagnostics;
const sha = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(read, reason, timeout = 5000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const value = await read(); if (value) return value; await pause(5); }
  throw new Error('Timeout: ' + reason);
}
function child(program, args = []) {
  const proc = spawn(program, args, { stdio: ['pipe', 'pipe', 'pipe'] }); children.push(proc);
  const lines = []; let buffer = '', stderr = '';
  proc.stdin.on('error', () => {});
  proc.stdout.on('data', chunk => {
    buffer += chunk; assert.ok(buffer.length < 65536);
    for (let i; (i = buffer.indexOf('\n')) >= 0;) { lines.push(JSON.parse(buffer.slice(0, i))); buffer = buffer.slice(i + 1); }
  });
  proc.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-8192); });
  return { proc, lines, stderr: () => stderr };
}
const normalize = text => text.split(/[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u).filter(Boolean).join(' ').toLowerCase();
const round = n => Math.sign(n) * Math.floor(Math.abs(n) + 0.5);
save();
try {
  execFileSync('bash', ['scripts/build-macos-device.sh'], { cwd: root, stdio: 'inherit' });
  execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library', '-D', 'CU_OWNED_POINTER_DROP_ACK', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'), ...readdirSync(join(root, 'native/device')).filter(file => file.endsWith('.swift')).sort().map(file => join(root, 'native/device', file)), '-o', lostAckHelper], { stdio: 'inherit' });
  if (death) {
    const fault = join(output, 'owned-before-down');
    execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library', '-D', 'CU_OWNED_POINTER_DEATH_POINTS', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'), ...readdirSync(join(root, 'native/device')).filter(file => file.endsWith('.swift')).sort().map(file => join(root, 'native/device', file)), '-o', fault], { stdio: 'inherit' });
    for (const point of ['between-down-up', 'after-up']) {
      const destination = join(output, 'owned-' + point); writeFileSync(destination, readFileSync(fault), { mode: 0o700 });
    }
  }
  execFileSync('cargo', ['build', '--locked', '-p', 'device-macos', '--bin', 'cu-device', '--example', 'native_device_session'], { cwd: root, stdio: 'inherit' });
  execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'), join(root, 'native/device/fixtures/GuardFixture.swift'), '-o', fixturePath], { stdio: 'inherit' });
  proof.build = Object.fromEntries([helper, device, examplePath, fixturePath, join(root, 'target/debug/cu-macos-source')].map(path => [path.slice(root.length + 1), sha(path)]));
  proof.fault_helper_sha256 = sha(lostAckHelper);
  const fixture = child(fixturePath);
  const ready = await until(() => fixture.lines.find(line => line.kind === 'ready'), 'owned fixture');
  proof.target = { pid: ready.pid, window_id: ready.window_id, bundle: ready.bundle_id };
  async function command(name) {
    const start = fixture.lines.length; fixture.proc.stdin.write(name + '\n');
    return until(() => fixture.lines.slice(start).find(line => line.kind === name), 'fixture ' + name);
  }
  function observe() {
    const assets = mkdtempSync(join(output, 'observation-'));
    try {
      const value = JSON.parse(execFileSync(join(root, 'target/debug/cu-macos-source'), [], { input: JSON.stringify({ protocol_version: 1, command: 'observe', bundle_id: ready.bundle_id, pid: ready.pid, output_dir: assets, timeout_ms: 3000 }), encoding: 'utf8', timeout: 5000, maxBuffer: 768 * 1024 }));
      assert.equal(value.structure.complete, true, value.structure.incomplete_reason); return value.structure;
    } finally { rmSync(assets, { recursive: true, force: true }); }
  }
  async function freshObservation() {
    // These are readonly pre-admission/effect reads, never input retries.
    // Every incomplete result stays in the evidence record.
    for (let attempt = 0; attempt < 3; attempt += 1) {
      try { return observe(); }
      catch (error) {
        proof.source_read_failures ??= []; proof.source_read_failures.push(String(error)); save();
        if (attempt === 2) throw error;
        await pause(100);
      }
    }
  }
  function binding(source, scenario) {
    const object = source.objects.find(object => object.role === 'AXButton' && Math.abs(object.frame.x - ready.frame.x) < 0.5 && Math.abs(object.frame.y - ready.frame.y) < 0.5);
    assert.ok(object);
    const ancestors = []; let current = object.parent;
    while (current) { const parent = source.objects.find(object => object.instance_ref === current); assert.ok(parent); ancestors.push(parent.role); current = parent.parent; assert.ok(ancestors.length <= 32); }
    const fingerprint = { app: source.app.bundle_id, app_pid: source.app.pid, window_id: object.window_id, ancestor_roles: ancestors.reverse(), group: object.group == null ? null : normalize(object.group), role: object.role, subrole: object.subrole ?? null, label: normalize(object.title ?? object.description ?? ''), sibling_index: source.objects.filter(sibling => sibling.parent === object.parent && sibling.window_id === object.window_id).findIndex(sibling => sibling.instance_ref === object.instance_ref), frame: [object.frame.x, object.frame.y, object.frame.width, object.frame.height].map(round), scroll_container: null };
    return { binding: 'owned-click', instance: object.instance_ref, snapshot_id: 'owned-' + scenario, environment_revision: 1, layer_revision: 1, fingerprint, display_frame: object.frame, ...nativeFixtureBindingEvidence(source, object, 1) };
  }
  diagnostics = async () => {
    const result = { stats: await command('stats'), events: fixture.lines.filter(line => ['pointer_down', 'pointer_up'].includes(line.kind)) };
    try { result.structure = observe(); } catch (error) { result.structure_error = String(error); }
    return result;
  };
  let clicks = 0;
  const scenarios = death ? ['click-death-before-down', 'click-death-between-down-up', 'click-death-after-up'] : ['click-applied', 'click-right', 'click-middle', 'click-modified', 'click-control', 'click-occluded', 'click-no-release-ack'];
  for (const scenario of scenarios) {
    await command('reset-press');
    await command('focus-window');
    const precondition = await until(async () => {
      const stats = await command('stats'); return stats.app_active && stats.window_key ? stats : false;
    }, 'owned fixture foreground before admission');
    proof.preconditions ??= []; proof.preconditions.push({ scenario, stats: precondition }); save();
    const path = join(output, scenario + '-proof.json'); writeFileSync(path, JSON.stringify(binding(await freshObservation(), scenario)), { mode: 0o600 });
    if (scenario === 'click-occluded') await command('occlude');
    try {
      const point = scenario.slice('click-death-'.length);
      const deathPointPath = join(output, 'death-point.json');
      if (death) rmSync(deathPointPath, { force: true });
      const selectedHelper = death ? join(output, 'owned-' + point) : scenario === 'click-no-release-ack' ? lostAckHelper : helper;
      const example = child(examplePath, [device, selectedHelper, path, scenario]);
      if (death) {
        let checkpoint;
        try {
          checkpoint = await until(() => {
            if (example.proc.exitCode != null && example.proc.exitCode !== 0) throw new Error(example.stderr());
            try { return JSON.parse(readFileSync(deathPointPath, 'utf8')); } catch { return false; }
          }, 'exact owned death point');
        } catch (error) {
          proof.death_setup_failure = { scenario, exit_code: example.proc.exitCode, lines: example.lines, stderr: example.stderr() }; save(); throw error;
        }
        const started = example.lines.find(line => line.kind === 'device_started');
        assert.ok(started); assert.equal(checkpoint.device_pid, started.pid); assert.equal(checkpoint.point, point);
        const processRows = execFileSync('/bin/ps', ['-p', String(checkpoint.helper_pid), '-o', 'pid=,ppid=,comm='], { encoding: 'utf8' }).split('\n').map(line => line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/)).filter(Boolean);
        assert.ok(processRows.some(row => Number(row[1]) === checkpoint.helper_pid && Number(row[2]) === started.pid && row[3] === selectedHelper), 'exact device child helper');
        process.kill(started.pid, 'SIGKILL');
        const result = await until(() => example.lines.find(line => line.kind === 'death_observer_result'), 'device death observer');
        await until(() => example.proc.exitCode === 0, 'death observer close');
        assert.equal(result.device_sigkill_verified, true); assert.equal(result.terminal_device_receipt_available, false); assert.equal(result.input_outcome, 'unknown');
        assert.equal(result.old_connection_closed, true);
        await pause(300);
        let remainingHelper = '';
        try { remainingHelper = execFileSync('/bin/ps', ['-p', String(checkpoint.helper_pid), '-o', 'comm='], { encoding: 'utf8' }).trim(); }
        catch (error) { if (error.status !== 1) throw error; }
        assert.notEqual(remainingHelper, selectedHelper, 'old helper closed after bounded release drain');
        result.native_helper_exit_verified = true;
        const beforeRecovery = await command('stats');
        proof.current_death = { scenario, checkpoint, result, beforeRecovery }; save();
        if (point !== 'before-down') clicks += 1;
        assert.equal(beforeRecovery.pointer_down_calls, clicks); assert.equal(beforeRecovery.pointer_up_calls, clicks);
        assert.equal(beforeRecovery.press_calls, 0); assert.equal(beforeRecovery.pointer_input_events.length, clicks * 2);
        assert.deepEqual(beforeRecovery.pointer_input_events.map(event => event.down), Array.from({ length: clicks }, () => [true, false]).flat());
        const recovery = child(examplePath, [device, helper, path, 'click-recovery-observe']);
        const observed = await until(() => {
          if (recovery.proc.exitCode != null && recovery.proc.exitCode !== 0) throw new Error(recovery.stderr());
          return recovery.lines.find(line => line.kind === 'recovery_observer_result');
        }, 'new epoch readonly query');
        await until(() => recovery.proc.exitCode === 0, 'recovery observer close');
        assert.equal(observed.new_epoch, 2); assert.equal(observed.normal_input_submitted, false);
        assert.equal(observed.input_capability_unavailable_verified, true);
        assert.equal(observed.query_response.type, 'command_ack'); assert.equal(observed.query_response.payload.accepted, false);
        assert.equal(observed.query_response.payload.reason, 'the device has no record of that intent');
        const afterRecovery = await command('stats');
        assert.equal(afterRecovery.pointer_down_calls, beforeRecovery.pointer_down_calls); assert.equal(afterRecovery.pointer_up_calls, beforeRecovery.pointer_up_calls);
        proof.scenarios.push({ scenario, checkpoint, helper_sha256: sha(selectedHelper), result, beforeRecovery, observed, afterRecovery }); save();
        continue;
      }
      const result = await until(() => {
        if (example.proc.exitCode != null && example.proc.exitCode !== 0) throw new Error(example.stderr());
        return example.lines.find(line => line.kind === 'production_result');
      }, scenario, 7000);
      proof.current_result = { scenario, result }; save();
      await until(() => example.proc.exitCode === 0, 'session close');
      if (scenario !== 'click-occluded') { clicks += 1; await until(() => fixture.lines.filter(line => line.kind === 'pointer_up').length === clicks, 'actual pointer up'); }
      const stats = await command('stats');
      assert.equal(stats.pointer_down_calls, clicks); assert.equal(stats.pointer_up_calls, clicks); assert.equal(stats.press_calls, 0);
      assert.equal(result.duplicate_not_replayed, true); assert.equal(result.simulated_input_used, false);
      if (scenario === 'click-occluded') { assert.equal(result.receipt_status, 'rejected'); assert.equal(result.submitted_count, 0); assert.equal(result.reason_code, 'target_occluded'); }
      else {
        assert.equal(result.receipt_status, scenario === 'click-no-release-ack' ? 'unknown' : 'completed'); assert.equal(result.submitted_count, 1);
        if (scenario === 'click-no-release-ack') { assert.equal(result.reason_code, 'effect_unknown'); assert.equal(result.channel_isolated, true); }
        assert.ok(result.input_end_ms <= result.command_end_ms);
        const events = stats.pointer_events.slice(-2); assert.equal(events.length, 2); assert.deepEqual(events.map(event => event.down), [true, false]);
        assert.equal(events[0].marker, events[1].marker); assert.notEqual(events[0].marker, '0');
        assert.equal(events[0].flags, events[1].flags);
        assert.equal(events[0].button, scenario === 'click-right' ? 1 : scenario === 'click-middle' ? 2 : 0);
        assert.equal(events[0].flags, scenario === 'click-modified' ? (1 << 17) | (1 << 19) | (1 << 20) : scenario === 'click-control' ? (1 << 18) : 0);
        assert.ok((await freshObservation()).objects.some(object => object.title === 'Clicked by native pointer'));
      }
      proof.scenarios.push({ scenario, result, stats }); save();
    } finally { if (scenario === 'click-occluded') await command('restore'); }
  }
  proof.ok = true; proof.tested_at = new Date().toISOString(); proof.cu_device_death_matrix_verified = death; proof.native_helper_sigkill_verified = false; proof.missing_release_ack_verified = !death;
  proof.limits = death ? ['exact cu-device SIGKILL with owned fault helper; actual input helper remains a separate process', 'SIGKILL of the input helper itself, persistent recovery and signed packaging remain open'] : ['device/core owned fixture acceptance; no Host/Engine business task or live model', 'input helper death between down/up, system/UI reset and signed packaging remain open'];
  save(); console.log(JSON.stringify({ ok: true, record, scenarios: proof.scenarios.length }));
} catch (error) { proof.error = String(error); try { proof.failure_diagnostics = await diagnostics?.(); } catch (diagnostic) { proof.diagnostic_error = String(diagnostic); } save(); throw error; }
finally {
  for (const proc of children) { proc.stdin.destroy(); if (proc.exitCode == null && proc.signalCode == null) proc.kill('SIGKILL'); }
  await Promise.all(children.map(proc => proc.exitCode != null || proc.signalCode != null ? undefined : new Promise(resolve => proc.once('exit', resolve))));
}
