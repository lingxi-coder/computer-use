import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:net';
import { createHash } from 'node:crypto';
import type { FrameMeta, UiProjection } from '@computer-use/desktop-api';
import { frameStream, frameCapture } from '@computer-use/desktop-api';
import { ImageDecoder, MAX_IMAGE_BYTES, imageMetadata, validatePng } from '../src/main/imageWire.js';
import { ImageClient, imageAvailability, observationImage } from '../src/main/imageClient.js';

const golden = readFileSync(new URL('../../../contracts/desktop-v1/image-channel/packet.bin', import.meta.url));
const png = readFileSync(new URL('../../../contracts/desktop-v1/image-channel/fixture.png', import.meta.url));
const meta = JSON.parse(readFileSync(new URL('../../../contracts/desktop-v1/image-channel/metadata.json', import.meta.url), 'utf8')) as FrameMeta;
function singleCapture(): FrameMeta {
  const stream = frameStream(meta)!;
  return { ...meta, frame_id: 'capture-event-1', source_sequence: 2, source_ms: 50,
    scale_factor: 0.5, provenance: { kind: 'single_capture', capture: {
      watch_id: stream.stream_id, request_id: 'computer-capture-1', image_frame_id: 'capture-image-1',
      pid: stream.pid, window_id: stream.window_id, window_frame: stream.window_frame,
      content_rect: { x: 0, y: 0, width: 640, height: 400 }, content_scale: 0.5, source_clock: 'mach_absolute',
      capture_started_uptime_ns: 1040000000, capture_finished_uptime_ns: 1050000000,
      capture_started_ms: 50, capture_finished_ms: 60, clock_uncertainty_ms: 2, requested_after_ms: 40, deadline_ms: 1000,
    } } };
}
function encodeImage(meta: FrameMeta): Buffer {
  const body = Buffer.from(JSON.stringify(meta)); const header = Buffer.alloc(16);
  header.write('CUIMG001'); header.writeUInt32BE(body.length, 8); header.writeUInt32BE(png.length, 12);
  return Buffer.concat([header, body, png]);
}

test('single capture has operation bounds and exact preview identity, while idle only keeps the stream healthy', () => {
  const capture = singleCapture();
  assert.deepEqual(imageMetadata(capture), capture);
  assert.equal(frameStream(capture), null);
  const view = projection(); view.observation!.frame!.frame_id = 'capture-image-1'; view.observation!.frame!.status = 'idle';
  assert.equal(imageAvailability({ meta: capture }, capture, view, 1000, 1065, 1070).frame_id, 'capture-image-1');
  assert.equal(imageAvailability({ meta: capture }, meta, view, 1000, 1065, 1070).frame_id, null);
  const bad = structuredClone(capture); frameCapture(bad)!.capture_started_ms = 40;
  assert.throws(() => imageMetadata(bad), /provenance/);
  assert.throws(() => imageMetadata({ ...meta, provenance: null, stream: frameStream(meta) }), /schema/);
});

test('actual image worker preserves a capture PNG when a later stream image arrives', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'cu-cap')); const path = join(dir, 'i.sock');
  const capture = singleCapture();
  const later = structuredClone(meta); later.frame_id = 'later-event'; later.source_sequence = 3; later.source_ms = 190;
  const stream = frameStream(later)!; stream.image_frame_id = 'later-image'; stream.callback_ms = 200;
  stream.display_time_ticks = 1200000000; stream.display_time_ns = 1200000000; stream.received_uptime_ns = 1210000000;
  const server = createServer((socket) => {
    let auth = Buffer.alloc(0);
    socket.on('data', (bytes) => {
      auth = Buffer.concat([auth, bytes]); if (auth.length < 48) return;
      socket.removeAllListeners('data');
      socket.write(Buffer.concat([Buffer.from('CUIREADY'), golden, encodeImage(capture), encodeImage(later)]));
    });
  });
  await new Promise<void>((done) => server.listen(path, done));
  const client = new ImageClient(() => {}, (reason) => assert.fail(reason));
  try {
    await client.start(path, 'a'.repeat(32), 7, 42);
    const deadline = Date.now() + 1800;
    while (client.packet?.meta.frame_id !== 'later-event' && Date.now() < deadline) {
      await new Promise((done) => setTimeout(done, 10));
    }
    assert.equal(client.packet!.meta.frame_id, 'later-event');
    assert.equal(client.packetFor('capture-image-1')!.meta.frame_id, 'capture-event-1');
    assert.deepEqual(Buffer.from(client.packetFor('capture-image-1')!.bytes), png);
  } finally { await client.close(); await new Promise<void>((done) => server.close(() => done())); rmSync(dir, { recursive: true }); }
});
function projection(): UiProjection {
  return { observation: { frame: { stream_id: frameStream(meta)!.stream_id, frame_id: frameStream(meta)!.image_frame_id,
    healthy: true, geometry_revision: 1, width_px: 320, height_px: 200 } } } as UiProjection;
}
test('shared Rust/TypeScript packet survives one-byte fragmentation with original pixel identity', () => {
  const decoder = new ImageDecoder(); let count = 0;
  for (const byte of golden) decoder.push(Uint8Array.of(byte), (packet) => {
    count++; assert.deepEqual(packet.meta, meta); assert.deepEqual(Buffer.from(packet.bytes), png);
  });
  assert.equal(count, 1); assert.equal(decoder.incomplete, false); assert.equal(decoder.buffered, 0);
});
test('hostile lengths fail at the prefix and failure stays terminal', () => {
  for (const field of [8, 12]) {
    const decoder = new ImageDecoder(); const prefix = Buffer.from(golden.subarray(0, 16)); prefix.writeUInt32BE(0xffffffff, field);
    assert.throws(() => decoder.push(prefix, () => assert.fail()), /bounds/);
    assert.throws(() => decoder.push(golden, () => assert.fail()), /failed/);
    assert.ok(decoder.buffered <= 16);
  }
  assert.ok(MAX_IMAGE_BYTES <= 12 * 1024 * 1024);
});
test('PNG dimensions, expanded pixel budget, checksum and animation are checked before display', () => {
  const bad = Buffer.from(png); bad[45]! ^= 1;
  assert.throws(() => validatePng(meta, bad), /checksum/);
  assert.throws(() => validatePng({ ...meta, width_px: 321 }, png), /dimensions/);
  assert.throws(() => imageMetadata({ ...meta, width_px: 4096, height_px: 4096 }), /bounds/);
  const animation = Buffer.concat([png.subarray(0, 33), Buffer.from([0,0,0,0,97,99,84,76,0,0,0,0]), png.subarray(33)]);
  assert.throws(() => validatePng({ ...meta, byte_length: animation.length,
    checksum: createHash('sha256').update(animation).digest('hex') }, animation), /animation/);
  assert.throws(() => imageMetadata({ ...meta, status: 'idle' }), /bounds/);
});
test('bytes require an exact control/projection match and both callback and host leases', () => {
  const packet = { meta, bytes: png }; const view = projection();
  view.observation!.sources = [{ source: 'ax', state: 'degraded', reason: 'observer unavailable', at_ms: 30, provenance: null }, { source: 'screenshot', state: 'available', reason: null, at_ms: 30, provenance: null }];
  const status = imageAvailability(packet, meta, view, 1000, 1030, 1050);
  assert.equal(status.frame_id, 'image-a-1');
  assert.equal(observationImage(packet, status, 'another-image'), null);
  assert.equal(observationImage(packet, status, 'image-a-1')?.bytes.length, png.length);
  const changes: Partial<FrameMeta>[] = [
    { checksum: '0'.repeat(64) }, { session_epoch: 8 }, { source_ms: 11 }, { geometry_revision: 2 },
    { provenance: { kind: 'stream', stream: { ...frameStream(meta)!, stream_id: 'retired' } } },
    { provenance: { kind: 'stream', stream: { ...frameStream(meta)!, pid: 43 } } }, { provenance: { kind: 'stream', stream: { ...frameStream(meta)!, display_time_ns: 1000000001 } } },
  ];
  for (const change of changes) assert.equal(imageAvailability(packet, { ...meta, ...change }, view, 1000, 1030, 1050).frame_id, null);
  assert.equal(imageAvailability(packet, meta, view, 1000, 1030, 3030).frame_id, null);
  assert.equal(imageAvailability(packet, meta, { observation: { frame: { ...view.observation!.frame!, healthy: false } } } as UiProjection,
    1000, 1030, 1050).frame_id, null);
  assert.equal(imageAvailability(packet, meta, view, 1000, 1030, 1020).frame_id, null, 'wall rollback fails closed');
});
test('idle renews callback confirmation, without replacing source time or image bytes', () => {
  const idle: FrameMeta = { ...meta, status: 'idle', byte_length: 0,
    provenance: { kind: 'stream', stream: { ...frameStream(meta)!, callback_ms: 3000, received_uptime_ns: 4000000000 } } };
  const packet = { meta, bytes: png };
  const current = imageAvailability(packet, idle, projection(), 1000, 4000, 4050);
  assert.equal(current.frame_id, 'image-a-1'); assert.equal(packet.meta.source_ms, 10);
  assert.equal(imageAvailability(packet, idle, projection(), 1000, 1000, 4050).frame_id, null, 'a frozen host cannot renew preview');
});
test('actual consumer worker authenticates a separate socket and transfers the shared PNG', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'cui')); const path = join(dir, 'i.sock');
  const server = createServer((socket) => {
    let auth = Buffer.alloc(0);
    socket.on('data', (bytes) => {
      auth = Buffer.concat([auth, bytes]); if (auth.length < 48) return;
      assert.equal(auth.subarray(0,8).toString(), 'CUIAUTH1'); assert.equal(auth.subarray(8,40).toString(), 'a'.repeat(32));
      assert.equal(auth.readBigUInt64BE(40), 7n); socket.removeAllListeners('data');
      socket.write(Buffer.concat([Buffer.from('CUIREADY'), golden]));
    });
  });
  await new Promise<void>((done) => server.listen(path, done));
  const client = new ImageClient(() => {}, () => {});
  try {
    await client.start(path, 'a'.repeat(32), 7, 42);
    const until = Date.now() + 3000;
    while (!client.packet && Date.now() < until) await new Promise((done) => setTimeout(done, 10));
    assert.deepEqual(Buffer.from(client.packet!.bytes), png);
    assert.equal(frameStream(client.packet!.meta)?.image_frame_id, 'image-a-1');
  } finally { await client.close(); await new Promise<void>((done) => server.close(() => done())); rmSync(dir, { recursive: true }); }
});

test('preview callers receive owned bytes and cannot mutate the retained packet', () => {
  const packet = { meta, bytes: Uint8Array.from(png) };
  const image = observationImage(packet, { frame_id: 'image-a-1', expires_unix_ms: 3000 }, 'image-a-1')!;
  image.bytes[0] = 0;
  assert.equal(packet.bytes[0], 137);
});

test('a trickling partial image cannot renew its total delivery deadline', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'cui')); const path = join(dir, 'i.sock');
  let drip: ReturnType<typeof setInterval> | undefined;
  const server = createServer((socket) => {
    let auth = Buffer.alloc(0);
    socket.on('data', (bytes) => {
      auth = Buffer.concat([auth, bytes]); if (auth.length < 48) return;
      socket.removeAllListeners('data');
      const payloadStart = 16 + golden.readUInt32BE(8);
      socket.write(Buffer.concat([Buffer.from('CUIREADY'), golden.subarray(0, payloadStart + 1)]));
      let index = payloadStart + 1;
      drip = setInterval(() => socket.write(golden.subarray(index, ++index)), 300);
      socket.on('close', () => { if (drip) clearInterval(drip); });
    });
  });
  await new Promise<void>((done) => server.listen(path, done));
  const reasons: string[] = []; const client = new ImageClient(() => {}, (reason) => reasons.push(reason));
  try {
    await client.start(path, 'a'.repeat(32), 7, 42);
    const deadline = Date.now() + 3000;
    while (!reasons.length && Date.now() < deadline) await new Promise((done) => setTimeout(done, 20));
    assert.ok(reasons.some((reason) => reason.includes('total deadline')), reasons.join('; '));
    assert.equal(client.packet, null);
  } finally { if (drip) clearInterval(drip); await client.close(); await new Promise<void>((done) => server.close(() => done())); rmSync(dir, { recursive: true }); }
});
