#!/usr/bin/env node
// Fixture setup and AX/TIS reads only; this probe never posts keyboard input.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = mkdtempSync(join(tmpdir(), 'cu-owned-key-focus-'));
const record = join(root, 'artifacts/m2-native-keyboard-focus-probe.json');
const evidence = { ok: false, output, started_at: new Date().toISOString(), input_posted: false,
  mvp_complete: false, composition_verified: false, cases: [] };
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
  while (Date.now() < end) { const value = read(); if (value) return value; await pause(10); }
  throw new Error('Timeout: ' + reason);
}
function child(program, label) {
  const proc = spawn(program, [], { stdio: ['pipe', 'pipe', 'pipe'] });
  const entry = { proc, label, lines: [], stdout: '', stderr: '', error: null };
  children.push(entry); proc.stdin.on('error', () => {});
  proc.on('error', error => { entry.error = String(error); });
  let pending = '';
  proc.stdout.on('data', chunk => {
    if (entry.stdout.length + chunk.length > 768 * 1024) { entry.error = 'stdout budget exceeded'; proc.kill(); return; }
    entry.stdout += chunk; pending += chunk;
    writeFileSync(join(output, label + '.stdout.jsonl'), entry.stdout);
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
let fixture;
async function command(name) {
  healthy(fixture); assert.equal(fixture.proc.exitCode, null);
  const start = fixture.lines.length; fixture.proc.stdin.write(name + '\n');
  return until(() => { healthy(fixture); return fixture.lines.slice(start).find(line => line.kind === name); }, name);
}
async function readFocus(ready, path, probePath, label, setup) {
  const request = { pid: ready.pid, bundle_id: ready.bundle_id, executable_path: path,
    parent_pid: process.pid, window_id: ready.window_id, setup, points: [
      { name: 'toggle', x: ready.x, y: ready.y },
      { name: 'text', x: ready.text_frame.x + ready.text_frame.width / 2, y: ready.text_frame.y + ready.text_frame.height / 2 }
    ] };
  writeFileSync(join(output, label + '.request.json'), JSON.stringify(request, null, 2) + '\n');
  const probe = child(probePath, label); probe.proc.stdin.end(JSON.stringify(request));
  await until(() => { healthy(probe); return probe.proc.exitCode != null; }, label + ' exit');
  assert.equal(probe.lines.length, 1); const response = probe.lines[0];
  assert.equal(response.pid, ready.pid); assert.equal(response.process_identity_revalidated, true);
  assert.equal(response.input_authorized, false);
  return response;
}
save();
try {
  mkdirSync(join(root, 'target/native-macos'), { recursive: true });
  const directory = mkdtempSync(join(root, 'target/native-macos/key-probe-owned-'));
  const contents = join(directory, 'KeyFixture.app/Contents');
  const fixturePath = join(contents, 'MacOS/KeyFixture'), probePath = join(directory, 'KeyReadProbe');
  mkdirSync(dirname(fixturePath), { recursive: true });
  writeFileSync(join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.lingxi.computer-user.guard-fixture</string>
<key>CFBundleExecutable</key><string>KeyFixture</string><key>CFBundleName</key><string>Computer Use Key Probe Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
  const flags = ['-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(output, 'module-cache')];
  for (const [name, binary] of [['KeyFixture', fixturePath], ['KeyReadProbe', probePath]]) {
    const source = join(root, 'native/device/fixtures', name + '.swift');
    execFileSync('xcrun', ['swiftc', ...flags, source, '-o', binary], { encoding: 'utf8', timeout: 60000, maxBuffer: 256 * 1024 });
  }
  evidence.build_hashes = Object.fromEntries([fixturePath, probePath].map(path => [path, sha(path)]));
  evidence.source_hashes = Object.fromEntries(['scripts/probe-macos-keyboard.mjs',
    'native/device/fixtures/KeyFixture.swift', 'native/device/fixtures/KeyReadProbe.swift'].map(path => [path, sha(join(root, path))]));
  evidence.git_head = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(); save();
  if (!process.argv.includes('--build-only')) {
    fixture = child(fixturePath, 'key-fixture');
    const ready = await until(() => { healthy(fixture); return fixture.lines.find(line => line.kind === 'ready'); }, 'fixture ready');
    assert.equal(ready.pid, fixture.proc.pid); evidence.target = ready;
    await command('focus-window');
    const setups = ['focus-toggle', 'focus-text', 'mark-text', 'unmark-text', 'focus-secure',
      'focus-none', 'focus-toggle', 'replace-control', 'focus-toggle'];
    for (const [index, setup] of setups.entries()) {
      const before = await command(setup);
      assert.equal(before.raw_key_event_count, 0, 'probe must not post keyboard input');
      assert.equal(before.toggle_activation_calls, 0); assert.equal(before.toggle_ax_press_calls, 0);
      const response = await readFocus(ready, fixturePath, probePath, `${index}-${setup}`, setup);
      const after = await command('stats');
      assert.equal(after.raw_key_event_count, 0); assert.equal(after.toggle_activation_calls, 0);
      const primary = response.focused_element.element;
      const exactControls = response.control_reads.filter(control => control.same_as_focused_cf === true);
      const item = { setup, before, response, after, observed_focus_role: primary?.role?.string ?? null,
        exact_focused_control_count: exactControls.length, raw_key_input_count: after.raw_key_event_count,
        public_text_input_marked_range: response.public_text_input_marked_range, failures: [] };
      evidence.cases.push(item); save();
      if (setup === 'focus-toggle') {
        const checks = [
          [response.focused_application.same_owned_pid === true, 'owned focused application'],
          [response.focused_element.error === 0, 'focused receiver available'],
          [primary?.role?.string === 'AXCheckBox', 'focused checkbox role'],
          [primary?.focused?.strict_cf_boolean === true, 'strict focused boolean'],
          [exactControls.length === 1, 'focused receiver belongs to current control tree']
        ];
        item.failures = checks.filter(([passed]) => !passed).map(([, reason]) => reason); save();
      }
    }
    assert.equal(evidence.cases.flatMap(item => item.failures).length, 0, 'focus relationship checks failed; all cases retained');
    evidence.readonly_focus_probe_passed = true;
  }
  evidence.ok = true;
} catch (error) { evidence.error = String(error?.stack ?? error); process.exitCode = 1;
} finally {
  for (const entry of children.reverse()) {
    if (entry.proc.exitCode == null && entry.proc.signalCode == null) {
      entry.proc.stdin.end('quit\n');
      await pause(100);
      if (entry.proc.exitCode == null && entry.proc.signalCode == null) entry.proc.kill('SIGTERM');
    }
  }
  evidence.finished_at = new Date().toISOString(); save();
  process.stdout.write(JSON.stringify({ ok: evidence.ok, record, output, cases: evidence.cases.length, error: evidence.error }) + '\n');
}
