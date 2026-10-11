#!/usr/bin/env node
// Run with node --import tsx. Only the disposable project fixture is observed.
// SIGSTOP affects the helper owned by this supervisor, never a user process.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

import { Supervisor } from '../apps/desktop/src/main/supervisor.ts';
import { resolveBinaries } from '../apps/desktop/src/main/binaries.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
if (process.platform !== 'darwin') throw new Error('macOS is required');
const helperPath = join(root, 'target/debug/cu-macos-source');
const permission = JSON.parse(execFileSync(helperPath, [], {
  input: '{"protocol_version":1,"command":"permissions"}', encoding: 'utf8', timeout: 5_000,
}));
if (!permission.accessibility || !permission.screen_capture) {
  throw new Error('AX and Screen Recording permissions are required; no permission is requested by this test');
}
const bundleId = 'com.lingxi.computer-user.m2-fixture';
const app = join(root, 'target/native-macos/M2ObservationFixture.app');
const contents = join(app, 'Contents');
const fixturePath = join(contents, 'MacOS/M2ObservationFixture');
mkdirSync(dirname(fixturePath), { recursive: true });
writeFileSync(join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>${bundleId}</string>
<key>CFBundleExecutable</key><string>M2ObservationFixture</string>
<key>CFBundleName</key><string>Computer Use M2 Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>\n`);
execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete',
  '-warnings-as-errors', '-parse-as-library', '-target', 'arm64-apple-macosx14.0',
  '-module-cache-path', join(root, 'target/native-macos/module-cache'),
  join(root, 'native/macos/fixtures/ObservationFixture.swift'), '-o', fixturePath], { stdio: 'inherit' });

const fixture = spawn(fixturePath, [], { stdio: ['ignore', 'pipe', 'ignore'] });
let fixtureExited = false;
const fixtureExit = new Promise((done) => fixture.once('exit', () => { fixtureExited = true; done(); }));
let supervisor;
let stoppedHelper;
const logs = [];
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
const fixtureReady = new Promise((done, fail) => {
  const timer = setTimeout(() => fail(new Error('fixture window did not initialize')), 5_000);
  let response = '';
  const read = (data) => {
    response += data.toString('utf8');
    if (response === 'READY\n' || response.length > 6 || response.includes('\n')) {
      clearTimeout(timer);
      fixture.stdout.off('data', read);
      if (response === 'READY\n') done();
      else fail(new Error('unexpected fixture readiness response'));
    }
  };
  fixture.stdout.on('data', read);
  fixture.once('error', (error) => { clearTimeout(timer); fail(error); });
});
const ownedHelperPid = () => {
  const sourcePid = supervisor?.pids.source;
  if (!sourcePid) return undefined;
  return execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,comm='], { encoding: 'utf8' })
    .split('\n').map((line) => line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/))
    .filter((row) => row && Number(row[2]) === sourcePid && row[3] === helperPath)
    .map((row) => Number(row[1]))[0];
};
try {
  await fixtureReady;
  // AppKit exposes titlebar AX descendants after the window is created.
  await pause(250);
  supervisor = await Supervisor.start({
    binaries: resolveBinaries({ CU_BIN_DIR: join(root, 'target/debug') }), mode: 'observe',
    nativeSource: { helperPath, bundleId, pid: fixture.pid },
  });
  supervisor.on('log', (line) => { logs.push(line); if (logs.length > 30) logs.shift(); });
  const latestPath = join(supervisor.dir, 'native-assets/latest.json');
  const deadline = Date.now() + 30_000;
  let observation;
  let last;
  while (Date.now() < deadline) {
    if (fixtureExited) throw new Error('the observation fixture exited unexpectedly');
    if (existsSync(latestPath)) {
      const pointer = JSON.parse(readFileSync(latestPath, 'utf8'));
      // The private manifest is atomically replaced; a record contains both
      // historical pixels and the current stream's validity separately.
      try {
        const manifest = JSON.parse(readFileSync(pointer.manifest, 'utf8'));
        last = manifest;
        const complete = manifest.last_complete;
        const native = manifest.last_pixel_observation;
        if (complete && native?.structure.complete && native.ocr.complete
          && native.frame_ref === complete.frame_meta.stream.image_frame_id
          && manifest.current_image_frame_id === native.frame_ref) {
          const png = readFileSync(join(dirname(pointer.manifest), complete.record.frame.png_file));
          observation = { manifest, png, native, complete };
          break;
        }
      } catch (error) {
        if (error.code !== 'ENOENT') throw error;
      }
    }
    await pause(100);
  }
  assert.ok(observation, `native observation did not complete: ${JSON.stringify({
    incomplete: last?.latest_observation?.structure.incomplete_reason,
    record: last?.record?.kind, reason: last?.record?.reason, health: last?.health,
    errors: last?.latest_observation?.errors, logs,
  })}`);
  const { png, native, complete } = observation;
  assert.equal(native.structure.app.bundle_id, bundleId);
  assert.equal(native.structure.app.pid, fixture.pid);
  assert.ok(native.structure.objects.length > 0);
  assert.equal(complete.frame_meta.clock_domain, 'macos/1/wall');
  assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
  const text = native.ocr.blocks.map((block) => block.text).join(' ');
  assert.ok(text.includes('Computer Use M2 Fixture'), 'Vision must recognize the fixture label');
  assert.equal(supervisor.projection.capabilities.backend, 'macos');
  assert.equal(supervisor.projection.capabilities.input, 'unavailable');
  assert.ok(!existsSync(supervisor.worldLogPath), 'native observations must not create fixture input');
  let idle;
  let idleComplete;
  const idleDeadline = Date.now() + 10_000;
  while (Date.now() < idleDeadline) {
    const pointer = JSON.parse(readFileSync(latestPath, 'utf8'));
    const current = JSON.parse(readFileSync(pointer.manifest, 'utf8'));
    last = current;
    // SCK can report another genuine Complete after startup. IDLE must refer
    // to the most recent Complete, rather than the first frame the test saw.
    const currentComplete = current.last_complete?.frame_meta;
    if (current.frame_meta?.status === 'idle' && currentComplete
      && current.current_image_frame_id === currentComplete.stream.image_frame_id) {
      idle = current.frame_meta;
      idleComplete = currentComplete;
      break;
    }
    await pause(50);
  }
  assert.ok(idle, `no valid idle after complete: ${JSON.stringify({ record: last?.record, logs })}`);
  assert.equal(idle.source_ms, idleComplete.source_ms, 'idle must retain original pixel capture time');
  assert.equal(idle.checksum, idleComplete.checksum);
  assert.equal(idle.byte_length, 0);
  assert.ok(idle.stream.callback_ms > idleComplete.stream.callback_ms);
  const projectionDeadline = Date.now() + 5_000;
  while (!supervisor.projection?.observation?.frame?.healthy && Date.now() < projectionDeadline) await pause(50);
  assert.equal(supervisor.projection?.observation?.frame?.healthy, true, 'frame must reach the host projection');
  let binaryImage;
  const binaryDeadline = Date.now() + 5_000;
  while (Date.now() < binaryDeadline) {
    const id = supervisor.projection?.observation?.frame?.frame_id;
    const image = id ? supervisor.image(id) : null;
    if (image) {
      const pointer = JSON.parse(readFileSync(latestPath, 'utf8'));
      const manifest = JSON.parse(readFileSync(pointer.manifest, 'utf8'));
      const current = manifest.last_complete?.frame_meta;
      if (current?.stream.image_frame_id === image.frame_id) {
        assert.equal(createHash('sha256').update(image.bytes).digest('hex'), current.checksum);
        assert.equal(image.width_px, current.width_px); assert.equal(image.height_px, current.height_px);
        binaryImage = image; break;
      }
    }
    await pause(50);
  }
  assert.ok(binaryImage, 'verified pixels must arrive over the independent binary channel');
  stoppedHelper = ownedHelperPid();
  assert.ok(stoppedHelper, 'the continuous helper must belong to the source process');
  process.kill(stoppedHelper, 'SIGSTOP');
  const freezeDeadline = Date.now() + 8_000;
  while (Date.now() < freezeDeadline) {
    const view = supervisor.projection?.observation;
    if (view?.frame && !view.frame.healthy
      && view.sources.some((source) => source.source === 'screenshot' && source.state === 'unavailable')) break;
    await pause(50);
  }
  const frozen = supervisor.projection?.observation;
  assert.equal(supervisor.image(binaryImage.frame_id), null, 'frozen source cannot provide a current image');
  assert.equal(frozen?.frame?.healthy, false, 'missing callbacks must invalidate the frame lease');
  assert.ok(frozen?.sources.some((source) => source.source === 'screenshot' && source.state === 'unavailable'),
    'source timeout must be visible to the operator');
  // The host heartbeat lease closes before the source process's independent
  // helper watchdog. Both deadlines must hold; they need not fire together.
  while (ownedHelperPid() === stoppedHelper && Date.now() < freezeDeadline) await pause(50);
  assert.equal(ownedHelperPid(), undefined, 'a frozen helper must be killed and reaped without an automatic restart');
  await pause(500);
  assert.equal(ownedHelperPid(), undefined, 'source recovery must require an explicit refresh');
  const evidence = {
    verified_at: new Date().toISOString(), fixture_bundle: bundleId,
    permissions: { accessibility: permission.accessibility, screen_capture: permission.screen_capture },
    source_backend: 'native', device_backend: 'macos', input: 'unavailable',
    capture_mode: 'watch', structure_complete: native.structure.complete, object_count: native.structure.objects.length,
    observer_available: native.observer_available, observer_reason: native.observer_reason ?? null,
    clock_domain: complete.frame_meta.clock_domain,
    screenshot: { width_px: complete.frame_meta.width_px, height_px: complete.frame_meta.height_px,
      byte_length: png.length, sha256: createHash('sha256').update(png).digest('hex') },
    ocr_complete: native.ocr.complete, ocr_blocks: native.ocr.blocks.length,
    complete_then_idle: true, idle_preserves_image_time_and_checksum: true,
    binary_image_received: true, binary_image_checksum_verified: true, binary_preview_cleared_on_freeze: true,
    frame_reached_host_projection: true, freeze_invalidation_visible: true, frozen_helper_reaped: true,
    freeze_reason: frozen.sources.find((source) => source.source === 'screenshot')?.reason,
    fixture_label_recognized: true, physical_input_sent: false,
    limitation: 'Owned fixture only; no physical input, MediaPipe or full M2 acceptance.',
  };
  const output = join(root, 'artifacts/m2-native-observation.json');
  mkdirSync(dirname(output), { recursive: true });
  writeFileSync(output, `${JSON.stringify(evidence, null, 2)}\n`);
  console.log(JSON.stringify(evidence));
} finally {
  if (stoppedHelper && ownedHelperPid() === stoppedHelper) {
    try { process.kill(stoppedHelper, 'SIGCONT'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
  }
  await supervisor?.shutdown();
  if (!fixtureExited) fixture.kill('SIGTERM');
  await Promise.race([fixtureExit, new Promise((done) => setTimeout(done, 2_000))]);
  if (!fixtureExited) { fixture.kill('SIGKILL'); await fixtureExit; }
}
