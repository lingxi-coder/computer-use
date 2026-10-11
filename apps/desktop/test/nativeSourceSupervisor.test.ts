import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import type { RowTableScope } from '@computer-use/desktop-api';

import { Supervisor } from '../src/main/supervisor.js';

const FAKE_PEER = String.raw`#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const capture = __CAPTURE__;
let pending = Buffer.alloc(0);
function send(value) {
  const body = Buffer.from(JSON.stringify(value));
  const header = Buffer.alloc(4); header.writeUInt32BE(body.length);
  process.stdout.write(Buffer.concat([header, body]));
}
function consume() {
  while (pending.length >= 4) {
    const size = pending.readUInt32BE(0);
    if (size < 1 || size > (1 << 20)) process.exit(90);
    if (pending.length < size + 4) return;
    const body = pending.subarray(4, size + 4);
    pending = pending.subarray(size + 4);
    let envelope;
    try { envelope = JSON.parse(body.toString('utf8')); } catch { process.exit(91); }
    const message = envelope.message;
    if (message?.type === 'configure') {
      const role = message.payload.role;
      fs.writeFileSync(path.join(capture, role + '.json'), JSON.stringify(message.payload.settings));
      send({ protocol_version: 2, message_id: role + '-ready', in_reply_to: null,
        producer: role, producer_sequence: 1, emitted_ms: 1, trace_id: role + '-trace',
        message: { type: 'ready', payload: { role, notes: [] } } });
    } else if (message?.type === 'shutdown') process.exit(0);
  }
}
process.stdin.on('data', chunk => { pending = Buffer.concat([pending, chunk]); consume(); });
process.stdin.on('end', () => process.exit(0));
`;

test('Supervisor bootstraps a scoped read-only Source without authorizing Device or Host input', async (t) => {
  const root = mkdtempSync(join(tmpdir(), 'cu-native-source-scope-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const capture = join(root, 'capture');
  const bin = join(root, 'bin');
  const helperPath = join(bin, 'cu-macos-source');
  const guardPath = join(bin, 'cu-macos-device-guard');
  const binaries = {
    host: join(bin, 'cu-host'),
    device: join(bin, 'cu-device'),
    source: join(bin, 'cu-source'),
  };
  mkdirSync(capture, { mode: 0o700 });
  mkdirSync(bin, { mode: 0o700 });
  const peer = FAKE_PEER.replace('__CAPTURE__', JSON.stringify(capture));
  for (const path of [...Object.values(binaries), helperPath, guardPath]) {
    writeFileSync(path, peer, { encoding: 'utf8', mode: 0o700 });
    chmodSync(path, 0o700);
  }

  const scope: RowTableScope = {
    scroll_path: [
      { role: 'ax_scroll_area', label: 'Preferences content' },
      { role: 'ax_scroll_area', label: 'Archive' },
    ],
    column_headers: ['Archive item'],
  };
  const supervisor = await Supervisor.start({
    binaries,
    mode: 'observe',
    readyTimeoutMs: 3_000,
    nativeSource: {
      helperPath,
      bundleId: 'com.lingxi.computer-user.mvp-fixture',
      pid: 42,
      sourceReadWindowId: 'window-9',
      tableScope: scope,
    },
  });
  try {
    const source = JSON.parse(readFileSync(join(capture, 'source.json'), 'utf8')) as Record<string, string>;
    const device = JSON.parse(readFileSync(join(capture, 'device.json'), 'utf8')) as Record<string, string>;
    const host = JSON.parse(readFileSync(join(capture, 'host.json'), 'utf8')) as Record<string, string>;
    assert.equal(source.native_text_policy, 'registered_editor');
    assert.equal(source.native_text_window_id, 'window-9');
    assert.deepEqual(JSON.parse(source.native_table_scope!), scope);
    assert.equal(device.backend, 'macos-observe');
    assert.equal(device.native_target_window_id, undefined);
    assert.equal(device.native_target_process_start_seconds, undefined);
    assert.equal(device.native_target_process_start_micros, undefined);
    assert.equal(host.native_target_window_id, undefined);
    assert.equal(host.native_target_process_start_seconds, undefined);
    assert.equal(host.native_target_process_start_micros, undefined);
    assert.equal(host.planning_backend, 'unconfigured');
    assert.equal(supervisor.projection?.root, undefined);
  } finally {
    await supervisor.shutdown();
  }
});

test('Supervisor sends an authorized kernel birth stamp to Device only', async (t) => {
  const root = mkdtempSync(join(tmpdir(), 'cu-native-process-identity-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const capture = join(root, 'capture');
  const bin = join(root, 'bin');
  const helperPath = join(bin, 'cu-macos-source');
  const guardPath = join(bin, 'cu-macos-device-guard');
  const binaries = {
    host: join(bin, 'cu-host'),
    device: join(bin, 'cu-device'),
    source: join(bin, 'cu-source'),
  };
  mkdirSync(capture, { mode: 0o700 });
  mkdirSync(bin, { mode: 0o700 });
  const peer = FAKE_PEER.replace('__CAPTURE__', JSON.stringify(capture));
  for (const path of [...Object.values(binaries), helperPath, guardPath]) {
    writeFileSync(path, peer, { encoding: 'utf8', mode: 0o700 });
    chmodSync(path, 0o700);
  }

  const scope: RowTableScope = {
    scroll_path: [{ role: 'ax_scroll_area', label: 'Archive' }],
    column_headers: ['Archive item'],
  };
  const supervisor = await Supervisor.start({
    binaries,
    mode: 'observe',
    readyTimeoutMs: 3_000,
    nativeSource: {
      helperPath,
      bundleId: 'com.lingxi.computer-user.mvp-fixture',
      pid: 42,
      authorizedWindowId: 'window-9',
      sourceReadWindowId: 'window-9',
      tableScope: scope,
      authorizedProcessIdentity: { pid: 42, start_seconds: 1_790_000_000, start_micros: 0 },
    },
  });
  try {
    const source = JSON.parse(readFileSync(join(capture, 'source.json'), 'utf8')) as Record<string, string>;
    const device = JSON.parse(readFileSync(join(capture, 'device.json'), 'utf8')) as Record<string, string>;
    const host = JSON.parse(readFileSync(join(capture, 'host.json'), 'utf8')) as Record<string, string>;
    assert.equal(device.native_target_pid, '42');
    assert.equal(device.native_target_window_id, 'window-9');
    assert.equal(device.native_target_process_start_seconds, '1790000000');
    assert.equal(device.native_target_process_start_micros, '0');
    assert.equal(source.native_table_scope, JSON.stringify(scope));
    assert.equal(source.native_target_process_start_seconds, undefined);
    assert.equal(source.native_target_process_start_micros, undefined);
    assert.equal(host.native_target_process_start_seconds, undefined);
    assert.equal(host.native_target_process_start_micros, undefined);
  } finally {
    await supervisor.shutdown();
  }
});

test('Supervisor keeps the BackgroundAx target separate across passive and consented Configure scopes', async (t) => {
  const root = mkdtempSync(join(tmpdir(), 'cu-native-background-scope-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const capture = join(root, 'capture');
  const bin = join(root, 'bin');
  const helperPath = join(bin, 'cu-macos-source');
  const guardPath = join(bin, 'cu-macos-device-guard');
  const binaries = {
    host: join(bin, 'cu-host'),
    device: join(bin, 'cu-device'),
    source: join(bin, 'cu-source'),
  };
  mkdirSync(capture, { mode: 0o700 });
  mkdirSync(bin, { mode: 0o700 });
  const peer = FAKE_PEER.replace('__CAPTURE__', JSON.stringify(capture));
  for (const path of [...Object.values(binaries), helperPath, guardPath]) {
    writeFileSync(path, peer, { encoding: 'utf8', mode: 0o700 });
    chmodSync(path, 0o700);
  }

  const backgroundSource = {
    helperPath,
    bundleId: 'com.lingxi.computer-user.guard-fixture',
    pid: 42,
    backgroundWindowId: 'window-9',
    referencePlanner: true,
  };
  const passive = await Supervisor.start({ binaries, mode: 'observe', readyTimeoutMs: 3_000,
    nativeSource: backgroundSource });
  try {
    const source = JSON.parse(readFileSync(join(capture, 'source.json'), 'utf8')) as Record<string, string>;
    const device = JSON.parse(readFileSync(join(capture, 'device.json'), 'utf8')) as Record<string, string>;
    const host = JSON.parse(readFileSync(join(capture, 'host.json'), 'utf8')) as Record<string, string>;
    assert.equal(source.native_background_window_id, 'window-9');
    assert.equal(source.native_text_policy, undefined);
    assert.equal(source.native_text_window_id, undefined);
    assert.equal(source.native_table_scope, undefined);
    assert.equal(source.native_target_process_start_seconds, undefined);
    assert.equal(device.backend, 'macos-background-ax', 'passive selection retains the separate BG domain');
    assert.equal(device.native_target_pid, '42');
    assert.equal(device.native_target_bundle, backgroundSource.bundleId);
    assert.equal(device.native_target_window_id, 'window-9');
    assert.equal(device.native_target_process_start_seconds, undefined, 'passive selection carries no process grant');
    assert.equal(device.native_target_process_start_micros, undefined);
    assert.equal(host.native_background_window_id, 'window-9');
    assert.equal(host.native_target_pid, '42');
    assert.equal(host.native_target_window_id, undefined, 'Host must not confuse the background scope with foreground window scope');
    assert.equal(host.native_target_process_start_seconds, undefined);
    assert.equal(host.native_target_process_start_micros, undefined);
    assert.equal(host.planning_backend, 'native_reference', 'the explicit fixture planner is selected by Host only');
  } finally {
    await passive.shutdown();
  }

  const consented = await Supervisor.start({ binaries, mode: 'observe', readyTimeoutMs: 3_000,
    nativeSource: { ...backgroundSource, authorizedBackgroundWindowId: 'window-9',
      authorizedProcessIdentity: { pid: 42, start_seconds: 1_790_000_000, start_micros: 123_456 } } });
  try {
    const source = JSON.parse(readFileSync(join(capture, 'source.json'), 'utf8')) as Record<string, string>;
    const device = JSON.parse(readFileSync(join(capture, 'device.json'), 'utf8')) as Record<string, string>;
    const host = JSON.parse(readFileSync(join(capture, 'host.json'), 'utf8')) as Record<string, string>;
    assert.equal(source.native_background_window_id, 'window-9');
    assert.equal(source.native_target_process_start_seconds, '1790000000');
    assert.equal(source.native_target_process_start_micros, '123456');
    assert.equal(source.native_text_window_id, undefined);
    assert.equal(device.backend, 'macos-background-ax');
    assert.equal(device.native_target_pid, '42');
    assert.equal(device.native_target_bundle, backgroundSource.bundleId);
    assert.equal(device.native_target_window_id, 'window-9');
    assert.equal(device.native_target_process_start_seconds, '1790000000');
    assert.equal(device.native_target_process_start_micros, '123456');
    assert.equal(host.native_background_window_id, 'window-9');
    assert.equal(host.native_target_pid, '42');
    assert.equal(host.native_target_bundle, backgroundSource.bundleId);
    assert.equal(host.native_target_window_id, undefined);
    assert.equal(host.native_target_process_start_seconds, '1790000000');
    assert.equal(host.native_target_process_start_micros, '123456');
    assert.equal(host.planning_backend, 'native_reference');
  } finally {
    await consented.shutdown();
  }
});

test('startup observers see messages before Ready and cannot mutate retained projection or abort startup', async (t) => {
  const root = mkdtempSync(join(tmpdir(), 'cu-startup-observers-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const capture = join(root, 'capture');
  mkdirSync(capture, { mode: 0o700 });
  const binary = join(root, 'peer');
  const injection = String.raw`
      const payload = role === 'source'
        ? { source: 'ocr', state: 'unavailable', reason: 'not captured', at_ms: 1,
            provenance: { session_epoch: 1, stream_id: 'test-stream', source_sequence: 1 } }
        : { revision: 1, mode: 'observe', root: null, stop_status: 'not_stopped',
            cleanup: 'not_needed', pending_confirmation: null, capabilities: null,
            observation: null, timeline: [] };
      if (role === 'source' || role === 'host') send({
        protocol_version: 2, message_id: role + '-observed', in_reply_to: null,
        producer: role, producer_sequence: 1, emitted_ms: 1, trace_id: role + '-trace',
        message: { type: role === 'source' ? 'source_health' : 'ui_projection', payload } });
  `;
  const peer = FAKE_PEER.replace('__CAPTURE__', JSON.stringify(capture))
    .replace('producer_sequence: 1, emitted_ms: 1', 'producer_sequence: 2, emitted_ms: 1')
    .replace('      send({ protocol_version: 2, message_id:', `${injection}\n      send({ protocol_version: 2, message_id:`);
  writeFileSync(binary, peer, { mode: 0o700 });
  let resolved = false;
  let healthCount = 0;
  let projectionCount = 0;
  const supervisor = await Supervisor.start({
    binaries: { host: binary, source: binary, device: binary },
    readyTimeoutMs: 3_000,
    observeSourceHealth: (health) => {
      assert.equal(resolved, false);
      assert.equal(health.reason, 'not captured');
      healthCount += 1;
      throw new Error('observer failure');
    },
    observeProjection: (projection) => {
      assert.equal(resolved, false);
      projectionCount += 1;
      projection.timeline.length = 1;
      throw new Error('observer failure');
    },
  });
  resolved = true;
  try {
    assert.equal(healthCount, 1);
    assert.equal(projectionCount, 1);
    assert.deepEqual(supervisor.projection?.timeline, []);
    assert.equal(supervisor.projection?.mode, 'observe');
  } finally {
    await supervisor.shutdown();
  }
});
