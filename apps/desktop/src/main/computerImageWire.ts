import type { ComputerFence, ComputerFrameRequest, FrameMeta } from '@computer-use/desktop-api';
import { validateComputerFence, validateComputerFrameRequest } from '@computer-use/desktop-api';

import {
  IMAGE_MAGIC,
  ImageDecoder,
  MAX_IMAGE_BYTES,
  MAX_IMAGE_METADATA,
  imageMetadata,
  validatePng,
  type ImagePacket,
} from './imageWire.js';

export type { ComputerFence, ComputerFrameRequest } from '@computer-use/desktop-api';

export interface ComputerImagePacket {
  fence: ComputerFence;
  image: ImagePacket;
}

export const COMPUTER_IMAGE_MAGIC = 'CUCIMG01';
export const MAX_COMPUTER_FENCE_BYTES = 16 * 1024;
const OUTER_PREFIX_BYTES = 12;
const IMAGE_PREFIX_BYTES = 16;
export const MAX_COMPUTER_IMAGE_PACKET_BYTES = OUTER_PREFIX_BYTES + MAX_COMPUTER_FENCE_BYTES
  + IMAGE_PREFIX_BYTES + MAX_IMAGE_METADATA + MAX_IMAGE_BYTES;

function fail(message: string): never {
  throw new Error(message);
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${canonical(record[key])}`).join(',')}}`;
  }
  const encoded = JSON.stringify(value);
  if (encoded === undefined) throw new Error('value is not JSON encodable');
  return encoded;
}

function sameJson(left: unknown, right: unknown): boolean {
  return canonical(left) === canonical(right);
}

function validateFence(value: unknown, nowMs: number): ComputerFence {
  if (!Number.isSafeInteger(nowMs) || nowMs < 0) fail('computer image clock is invalid');
  const result = validateComputerFence(value);
  if (!result.ok) fail('computer image fence values are invalid');
  if (nowMs >= result.value.deadline_ms) fail('computer image request expired');
  return result.value;
}

function checkedMeta(value: unknown): FrameMeta {
  // imageMetadata rejects non-COMPLETE frames and unknown FrameMeta fields.
  return imageMetadata(value);
}

function validateRequest(request: ComputerFrameRequest, nowMs: number): ComputerFrameRequest {
  const fence = validateFence(request?.fence, nowMs);
  if (!validateComputerFrameRequest(request).ok) fail('computer frame metadata is invalid');
  const expected = checkedMeta(request?.expected_frame);
  if (expected.status !== 'complete') fail('computer image request needs a COMPLETE frame');
  return { fence, expected_frame: expected };
}

/**
 * Selects only the exact COMPLETE packet already validated by ImageClient.
 * IDLE metadata can never be promoted into a new capture.
 */
export function selectComputerImage(
  request: ComputerFrameRequest,
  source: ImagePacket | null,
  nowMs: number,
): ComputerImagePacket {
  const validRequest = validateRequest(request, nowMs);
  if (!source) fail('the current ImageClient packet is unavailable');
  const meta = checkedMeta(source.meta);
  if (meta.status !== 'complete' || !sameJson(meta, validRequest.expected_frame)) {
    fail('ImageClient packet does not match the requested COMPLETE frame');
  }
  validatePng(meta, source.bytes);
  if (nowMs >= validRequest.fence.deadline_ms) fail('computer image request expired');
  return {
    fence: validRequest.fence,
    image: { meta, bytes: source.bytes.slice() },
  };
}

function encodeImageFrame(image: ImagePacket): Buffer {
  validatePng(image.meta, image.bytes);
  const metadata = Buffer.from(JSON.stringify(image.meta), 'utf8');
  if (metadata.length < 2 || metadata.length > MAX_IMAGE_METADATA || image.bytes.length > MAX_IMAGE_BYTES) {
    fail('computer image payload exceeds bounds');
  }
  const packet = Buffer.allocUnsafe(IMAGE_PREFIX_BYTES + metadata.length + image.bytes.length);
  packet.write(IMAGE_MAGIC, 0, 'ascii');
  packet.writeUInt32BE(metadata.length, 8);
  packet.writeUInt32BE(image.bytes.length, 12);
  metadata.copy(packet, IMAGE_PREFIX_BYTES);
  Buffer.from(image.bytes).copy(packet, IMAGE_PREFIX_BYTES + metadata.length);
  return packet;
}

/** `CUCIMG01` + bounded ComputerFence JSON + the existing `CUIMG001` frame. */
export function encodeComputerImagePacket(
  request: ComputerFrameRequest,
  source: ImagePacket | null,
  nowMs: number,
): Uint8Array {
  const selected = selectComputerImage(request, source, nowMs);
  const fenceBytes = Buffer.from(JSON.stringify(selected.fence), 'utf8');
  if (fenceBytes.length < 2 || fenceBytes.length > MAX_COMPUTER_FENCE_BYTES) {
    fail('computer image fence exceeds bounds');
  }
  const imageBytes = encodeImageFrame(selected.image);
  const packet = Buffer.allocUnsafe(OUTER_PREFIX_BYTES + fenceBytes.length + imageBytes.length);
  packet.write(COMPUTER_IMAGE_MAGIC, 0, 'ascii');
  packet.writeUInt32BE(fenceBytes.length, 8);
  fenceBytes.copy(packet, OUTER_PREFIX_BYTES);
  imageBytes.copy(packet, OUTER_PREFIX_BYTES + fenceBytes.length);
  return packet;
}

/** Parses exactly one image packet and ties it to the caller's outstanding request. */
export function decodeComputerImagePacket(
  bytes: Uint8Array,
  request: ComputerFrameRequest,
  nowMs: number,
): ComputerImagePacket {
  const validRequest = validateRequest(request, nowMs);
  if (bytes.byteLength > MAX_COMPUTER_IMAGE_PACKET_BYTES) {
    fail('computer image packet exceeds bounds');
  }
  const packet = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (packet.length < OUTER_PREFIX_BYTES || packet.subarray(0, 8).toString('ascii') !== COMPUTER_IMAGE_MAGIC) {
    fail('computer image packet prefix is invalid');
  }
  const fenceLength = packet.readUInt32BE(8);
  if (fenceLength < 2 || fenceLength > MAX_COMPUTER_FENCE_BYTES
    || OUTER_PREFIX_BYTES + fenceLength + IMAGE_PREFIX_BYTES > packet.length) {
    fail('computer image fence length exceeds bounds');
  }
  let decodedFence: unknown;
  try {
    decodedFence = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(
      packet.subarray(OUTER_PREFIX_BYTES, OUTER_PREFIX_BYTES + fenceLength),
    ));
  } catch {
    fail('computer image fence is not valid UTF-8 JSON');
  }
  const fence = validateFence(decodedFence, nowMs);
  if (!sameJson(fence, validRequest.fence)) fail('computer image correlation does not match the request');

  const imageWire = packet.subarray(OUTER_PREFIX_BYTES + fenceLength);
  const decoder = new ImageDecoder();
  const images: ImagePacket[] = [];
  decoder.push(imageWire, (frame) => {
    if (images.length !== 0) fail('computer image packet contains multiple frames');
    images.push(frame);
  });
  if (images.length !== 1 || decoder.incomplete || decoder.buffered !== 0) {
    fail('computer image packet is truncated or contains trailing frames');
  }
  const image = images[0]!;
  if (!sameJson(image.meta, validRequest.expected_frame)) {
    fail('computer image metadata does not match the requested frame');
  }
  validatePng(image.meta, image.bytes);
  if (nowMs >= fence.deadline_ms) fail('computer image request expired');
  return { fence, image };
}
