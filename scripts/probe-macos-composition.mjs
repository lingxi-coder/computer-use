#!/usr/bin/env node
// Owned standard NSTextView marked-range readback. The fixture performs setup;
// KeyReadProbe only reads AX/CG/TIS facts and never posts input.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const runId = randomUUID();
const nativeOutputRoot = join(root, 'target/native-macos');
mkdirSync(nativeOutputRoot, { recursive: true });
const output = mkdtempSync(join(nativeOutputRoot, 'composition-probe-owned-'));
const artifact = join(root, 'artifacts/m2-native-composition-readonly-probe.json');
const contents = join(output, 'KeyFixture.app/Contents');
const fixturePath = join(contents, 'MacOS/KeyFixture');
const probePath = join(output, 'KeyReadProbe');
const fixtureSource = join(root, 'native/device/fixtures/OwnedCompositionFixture.swift');
const probeSource = join(root, 'native/device/fixtures/KeyReadProbe.swift');
const fixtureBundleID = 'com.lingxi.computer-user.guard-fixture';
const scenarios = ['focus-text', 'mark-text', 'unmark-text'];
const runMode = process.argv.includes('--run');
const buildOnly = process.argv.includes('--build-only') || !runMode;
assert(!(runMode && process.argv.includes('--build-only')), 'choose --run or --build-only');

const evidence = {
  ok: false,
  run_id: runId,
  started_at: new Date().toISOString(),
  mode: runMode ? 'owned_nstextview_public_marked_range_readonly' : 'strict_build_only',
  output,
  fixture_binary: fixturePath,
  key_read_probe_binary: probePath,
  fixture_bundle_id: fixtureBundleID,
  scenarios,
  input_posted: false,
  cg_post_event_calls: 0,
  ax_set_value_calls: 0,
  ax_press_calls: 0,
  set_focus_calls: 0,
  composition_verified: false,
  composition_conclusion: 'Unknown',
  composition_claimed: false,
  cases: [],
  first_failure: null,
};

mkdirSync(dirname(artifact), { recursive: true });
if (existsSync(artifact)) {
  const previous = artifact.replace('.json', `.previous-${runId}.json`);
  copyFileSync(artifact, previous);
  evidence.previous_artifact = previous;
}
const save = () => writeFileSync(artifact, JSON.stringify(evidence, null, 2) + '\n', { mode: 0o600 });
const sha = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const pause = ms => new Promise(done => setTimeout(done, ms));
const failOnce = (stage, error) => {
  evidence.first_failure ??= { stage, error: String(error?.stack ?? error).slice(0, 12 * 1024) };
  save();
};

let stage = 'initialization';
let fixture;

function createChild(program, args, label) {
  const proc = spawn(program, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  const entry = { proc, label, lines: [], stdout: '', stderr: '', error: null, pending: '' };
  proc.stdin.on('error', () => {});
  proc.on('error', error => { entry.error = String(error); });
  proc.stdout.on('data', chunk => {
    if (entry.stdout.length + chunk.length > 512 * 1024) {
      entry.error = 'stdout budget exceeded';
      proc.kill('SIGTERM');
      return;
    }
    entry.stdout += chunk.toString('utf8');
    entry.pending += chunk.toString('utf8');
    writeFileSync(join(output, `${label}.stdout.jsonl`), entry.stdout, { mode: 0o600 });
    for (let newline; (newline = entry.pending.indexOf('\n')) >= 0;) {
      const line = entry.pending.slice(0, newline);
      entry.pending = entry.pending.slice(newline + 1);
      if (!line) continue;
      try { entry.lines.push(JSON.parse(line)); }
      catch (error) { entry.error = String(error); }
    }
  });
  proc.stderr.on('data', chunk => {
    if (entry.stderr.length + chunk.length > 64 * 1024) {
      entry.error = 'stderr budget exceeded';
      proc.kill('SIGTERM');
      return;
    }
    entry.stderr += chunk.toString('utf8');
    writeFileSync(join(output, `${label}.stderr.txt`), entry.stderr, { mode: 0o600 });
  });
  return entry;
}

function healthy(entry) {
  assert.equal(entry.error, null, entry.error);
  assert.equal(entry.proc.signalCode, null, `${entry.label} terminated by signal`);
  if (entry.proc.exitCode != null && entry.proc.exitCode !== 0) {
    throw new Error(entry.stderr || `${entry.label} exited ${entry.proc.exitCode}`);
  }
}

async function until(read, reason, timeoutMs = 6000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const result = read();
    if (result) return result;
    await pause(10);
  }
  throw new Error(`timed out: ${reason}`);
}

async function fixtureCommand(command) {
  healthy(fixture);
  assert.equal(fixture.proc.exitCode, null, 'owned composition fixture is not running');
  const start = fixture.lines.length;
  fixture.proc.stdin.write(command + '\n');
  return until(() => {
    healthy(fixture);
    return fixture.lines.slice(start).find(line => line.kind === command);
  }, `fixture command ${command}`);
}

async function readMarkedRangeProbe(ready, setup) {
  const request = {
    pid: ready.pid,
    bundle_id: ready.bundle_id,
    executable_path: fixturePath,
    parent_pid: process.pid,
    window_id: ready.window_id,
    setup,
  };
  const label = `readonly-${setup}`;
  writeFileSync(join(output, `${label}.request.json`), JSON.stringify(request, null, 2) + '\n', { mode: 0o600 });
  const probe = createChild(probePath, [], label);
  const closed = new Promise(done => probe.proc.once('close', (code, signal) => done({ code, signal })));
  probe.proc.stdin.end(JSON.stringify(request));
  const exit = await Promise.race([closed, pause(8000).then(() => null)]);
  if (!exit) {
    probe.proc.kill('SIGTERM');
    await Promise.race([closed, pause(1000)]);
    if (probe.proc.exitCode == null && probe.proc.signalCode == null) probe.proc.kill('SIGKILL');
    throw new Error(`${label} timed out; private output retained`);
  }
  healthy(probe);
  assert.equal(exit.code, 0, `${label} exited ${exit.code ?? exit.signal}: ${probe.stderr}`);
  assert.equal(probe.lines.length, 1, `${label} must emit one JSON result`);
  const response = probe.lines[0];
  assert.equal(response.kind, 'key_read_probe');
  assert.equal(response.pid, ready.pid);
  assert.equal(response.process_identity_revalidated, true);
  assert.equal(response.input_authorized, false);
  return { request, response };
}

save();
try {
  if (process.platform !== 'darwin') throw new Error('macOS is required');
  stage = 'compile_strict_private_helpers';
  mkdirSync(join(contents, 'MacOS'), { recursive: true, mode: 0o700 });
  writeFileSync(join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>${fixtureBundleID}</string>
<key>CFBundleExecutable</key><string>KeyFixture</string>
<key>CFBundleName</key><string>Computer Use Composition Probe Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string></dict></plist>\n`, { mode: 0o600 });
  const moduleCache = join(output, 'module-cache');
  mkdirSync(moduleCache, { recursive: true, mode: 0o700 });
  const flags = ['-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library',
    '-target', 'arm64-apple-macosx14.0', '-module-cache-path', moduleCache];
  for (const [source, binary] of [[fixtureSource, fixturePath], [probeSource, probePath]]) {
    execFileSync('xcrun', ['swiftc', ...flags, source, '-o', binary], {
      encoding: 'utf8', timeout: 90_000, maxBuffer: 512 * 1024,
    });
  }
  evidence.source_hashes = {
    fixture: sha(fixtureSource),
    key_read_probe: sha(probeSource),
    script: sha(fileURLToPath(import.meta.url)),
  };
  evidence.binary_hashes = { fixture: sha(fixturePath), key_read_probe: sha(probePath) };
  evidence.git_head = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  evidence.build_complete = true;

  if (buildOnly) {
    evidence.ok = true;
    evidence.status = 'strict_build_complete';
  } else {
    stage = 'start_owned_composition_fixture';
    fixture = createChild(fixturePath, [], 'owned-composition-fixture');
    const ready = await until(() => {
      healthy(fixture);
      return fixture.lines.find(line => line.kind === 'ready');
    }, 'owned composition fixture ready');
    assert.equal(ready.pid, fixture.proc.pid);
    assert.equal(ready.bundle_id, fixtureBundleID);
    assert.ok(Number.isInteger(ready.window_id) && ready.window_id > 0);
    evidence.target = ready;
    save();

    for (const setup of scenarios) {
      stage = `fixture_setup_${setup}`;
      const before = await fixtureCommand(setup);
      assert.equal(before.input_posted, false);
      assert.equal(before.key_event_count, 0, 'no keyboard events may enter the owned fixture');
      assert.equal(before.ax_set_value_calls, 0);
      assert.equal(before.ax_press_calls, 0);
      assert.equal(before.set_focus_calls, 0);
      assert.equal(before.cg_post_event_calls, 0);

      stage = `readonly_public_marked_range_${setup}`;
      const { request, response } = await readMarkedRangeProbe(ready, setup);
      const after = await fixtureCommand('stats');
      assert.equal(after.input_posted, false);
      assert.equal(after.key_event_count, 0, 'probe must not deliver keyboard events');
      assert.equal(after.ax_set_value_calls, 0);
      assert.equal(after.ax_press_calls, 0);
      assert.equal(after.set_focus_calls, 0);
      assert.equal(after.cg_post_event_calls, 0);

      const ax = response.public_text_input_marked_range ?? null;
      const internalBefore = before.marked_range ?? null;
      const internalAfter = after.marked_range ?? null;
      const axConvertedRange = ax?.range_conversion_succeeded === true ? {
        location: ax.location_decimal_string ?? null,
        length: ax.length_decimal_string ?? null,
      } : null;
      const internalTruthStable = JSON.stringify(internalBefore) === JSON.stringify(internalAfter) &&
        before.has_marked_text === after.has_marked_text;
      const rangeMatchesFixtureTruth = internalTruthStable && axConvertedRange != null &&
        axConvertedRange.location === internalAfter?.location && axConvertedRange.length === internalAfter?.length;
      const item = {
        setup,
        request,
        fixture_internal_before: before,
        probe_response: response,
        public_marked_range: ax,
        fixture_internal_after: after,
        comparison: {
          fixture_truth_stable_across_read: internalTruthStable,
          ax_range_converted: axConvertedRange != null,
          ax_range_matches_fixture_internal_range: rangeMatchesFixtureTruth,
          fixture_marked_state_is_truth_only: true,
          composition_verified: false,
        },
        input_posted: false,
        composition_verified: false,
      };
      evidence.cases.push(item);
      save();
    }
    evidence.status = 'readonly_public_marked_range_cases_captured';
    evidence.ok = true;
  }
} catch (error) {
  evidence.ok = false;
  evidence.status = 'failed';
  failOnce(stage, error);
  process.exitCode = 1;
} finally {
  if (fixture && fixture.proc.exitCode == null && fixture.proc.signalCode == null) {
    fixture.proc.stdin.end('quit\n');
    const closed = new Promise(done => fixture.proc.once('close', done));
    await Promise.race([closed, pause(2500)]);
    if (fixture.proc.exitCode == null && fixture.proc.signalCode == null) {
      fixture.proc.kill('SIGTERM');
      await Promise.race([closed, pause(1500)]);
    }
  }
  evidence.fixture_exit = fixture ? { code: fixture.proc.exitCode, signal: fixture.proc.signalCode } : null;
  evidence.finished_at = new Date().toISOString();
  evidence.composition_verified = false;
  evidence.composition_conclusion = 'Unknown';
  evidence.composition_claimed = false;
  save();
  process.stdout.write(JSON.stringify({
    ok: evidence.ok,
    status: evidence.status,
    artifact,
    output,
    cases: evidence.cases.length,
    composition_verified: false,
    error: evidence.first_failure?.error ?? null,
  }) + '\n');
}
