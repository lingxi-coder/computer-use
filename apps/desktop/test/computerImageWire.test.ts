import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import type { FrameMeta } from '@computer-use/desktop-api';
import {
  COMPUTER_IMAGE_MAGIC,
  MAX_COMPUTER_FENCE_BYTES,
  MAX_COMPUTER_IMAGE_PACKET_BYTES,
  decodeComputerImagePacket,
  encodeComputerImagePacket,
  selectComputerImage,
  type ComputerFrameRequest,
} from '../src/main/computerImageWire.js';
import { MAX_IMAGE_BYTES } from '../src/main/imageWire.js';

const png = readFileSync(new URL('../../../contracts/desktop-v1/image-channel/fixture.png', import.meta.url));
const meta = JSON.parse(readFileSync(new URL('../../../contracts/desktop-v1/image-channel/metadata.json', import.meta.url), 'utf8')) as FrameMeta;

function request(overrides: Partial<ComputerFrameRequest['fence']> = {}, frame = meta): ComputerFrameRequest {
  return {
    fence: {
      request_id: 'computer-image-1',
      generation: 1,
      root_task_id: 'root-1',
      task_revision: 3,
      root_epoch: 2,
      snapshot_id: 'snapshot-8',
      structure_sequence: 8,
      deadline_ms: 20_000,
      ...overrides,
    },
    expected_frame: frame,
  };
}

function imagePacket(frame = meta, bytes: Uint8Array = png) {
  return { meta: frame, bytes };
}

test('computer image wire round-trips one exact COMPLETE ImageClient frame and its fence', () => {
  const asked = request();
  const encoded = encodeComputerImagePacket(asked, imagePacket(), 10_000);
  assert.equal(Buffer.from(encoded).subarray(0, 8).toString('ascii'), COMPUTER_IMAGE_MAGIC);
  const decoded = decodeComputerImagePacket(encoded, asked, 10_001);
  assert.deepEqual(decoded.fence, asked.fence);
  assert.deepEqual(decoded.image.meta, meta);
  assert.deepEqual(Buffer.from(decoded.image.bytes), png);
});

test('selection requires a complete exact metadata match and checksum-valid pixels', () => {
  const mismatch = { ...meta, source_sequence: meta.source_sequence + 1 };
  assert.throws(() => selectComputerImage(request(), imagePacket(mismatch), 10_000), /does not match/);
  const badBytes = Uint8Array.from(png);
  const badChecksumIndex = badBytes.length - 1;
  badBytes[badChecksumIndex] = badBytes[badChecksumIndex]! ^ 1;
  assert.throws(() => selectComputerImage(request(), imagePacket(meta, badBytes), 10_000), /checksum/);
  const idle = { ...meta, status: 'idle' } as FrameMeta;
  assert.throws(() => selectComputerImage(request({}, idle), imagePacket(idle), 10_000), /metadata|COMPLETE/);
});

test('a packet cannot be attached to another outstanding correlation fence', () => {
  const encoded = encodeComputerImagePacket(request(), imagePacket(), 10_000);
  assert.throws(() => decodeComputerImagePacket(encoded, request({ generation: 2 }), 10_001), /correlation/);
});

test('fence deadlines reject selection and decode at or after expiry', () => {
  const asked = request({ deadline_ms: 10_000 });
  assert.throws(() => encodeComputerImagePacket(asked, imagePacket(), 10_000), /expired/);
  const encoded = encodeComputerImagePacket(request(), imagePacket(), 10_000);
  assert.throws(() => decodeComputerImagePacket(encoded, request(), 20_000), /expired/);
  assert.throws(() => encodeComputerImagePacket(request({ deadline_ms: 0 }), imagePacket(), 0), /fence values/);
});

test('outer and nested lengths reject oversize and truncated packets before returning pixels', () => {
  const oversizedFence = Buffer.alloc(12);
  oversizedFence.write(COMPUTER_IMAGE_MAGIC, 0, 'ascii');
  oversizedFence.writeUInt32BE(MAX_COMPUTER_FENCE_BYTES + 1, 8);
  assert.throws(() => decodeComputerImagePacket(oversizedFence, request(), 10_000), /length exceeds bounds/);

  const truncated = encodeComputerImagePacket(request(), imagePacket(), 10_000).slice(0, -1);
  assert.throws(() => decodeComputerImagePacket(truncated, request(), 10_001), /truncated|bounds|checksum/);
  assert.throws(() => decodeComputerImagePacket(Uint8Array.of(1, 2, 3), request(), 10_000), /prefix/);

  const outer = Buffer.from(encodeComputerImagePacket(request(), imagePacket(), 10_000));
  const imageStart = 12 + outer.readUInt32BE(8);
  outer.writeUInt32BE(MAX_IMAGE_BYTES + 1, imageStart + 12);
  assert.throws(() => decodeComputerImagePacket(outer, request(), 10_001), /bounds/);

  const oversizedPacket = Buffer.alloc(MAX_COMPUTER_IMAGE_PACKET_BYTES + 1);
  oversizedPacket.write(COMPUTER_IMAGE_MAGIC, 0, 'ascii');
  assert.throws(() => decodeComputerImagePacket(oversizedPacket, request(), 10_001), /packet exceeds bounds/);
});

test('a second nested frame is rejected immediately rather than accumulated', () => {
  const single = Buffer.from(encodeComputerImagePacket(request(), imagePacket(), 10_000));
  const imageStart = 12 + single.readUInt32BE(8);
  const double = Buffer.concat([single.subarray(0, imageStart), single.subarray(imageStart), single.subarray(imageStart)]);
  assert.throws(() => decodeComputerImagePacket(double, request(), 10_001), /multiple frames/);
});

test('a corrupted nested PNG fails its metadata checksum', () => {
  const encoded = Buffer.from(encodeComputerImagePacket(request(), imagePacket(), 10_000));
  const imageStart = 12 + encoded.readUInt32BE(8);
  const imageMetaLength = encoded.readUInt32BE(imageStart + 8);
  const pngStart = imageStart + 16 + imageMetaLength;
  const corruptedPngIndex = pngStart + 40;
  encoded[corruptedPngIndex] = encoded[corruptedPngIndex]! ^ 1;
  assert.throws(() => decodeComputerImagePacket(encoded, request(), 10_001), /checksum/);
  assert.equal(meta.checksum, createHash('sha256').update(png).digest('hex'));
});

test('the shared computer packet used by Rust retains the same fence and PNG in TypeScript', () => {
  const path = new URL('../../../contracts/computer-v1/image-channel/', import.meta.url);
  const asked: ComputerFrameRequest = JSON.parse(readFileSync(new URL('request.json', path), 'utf8'));
  const wire = readFileSync(new URL('packet.bin', path));
  const decoded = decodeComputerImagePacket(wire, asked, 10_000);
  assert.deepEqual(decoded.fence, asked.fence);
  assert.deepEqual(decoded.image.meta, asked.expected_frame);
  assert.deepEqual(Buffer.from(decoded.image.bytes), png);
});
