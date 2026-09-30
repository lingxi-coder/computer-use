import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  encodeFrame,
  EnvelopeFactory,
  FrameDecoder,
  FrameError,
  LENGTH_PREFIX_BYTES,
  MAX_FRAME_BYTES,
  type Envelope,
} from '../src/index.js';

const factory = new EnvelopeFactory('host', () => 10);
const envelope = (text: string): Envelope =>
  factory.make({ type: 'operator_command', payload: { command: 'create_task', text } });

function frameOf(payload: string): Uint8Array {
  const bytes = new TextEncoder().encode(payload);
  const out = new Uint8Array(LENGTH_PREFIX_BYTES + bytes.length);
  new DataView(out.buffer).setUint32(0, bytes.length, false);
  out.set(bytes, LENGTH_PREFIX_BYTES);
  return out;
}

function header(length: number): Uint8Array {
  const out = new Uint8Array(LENGTH_PREFIX_BYTES);
  new DataView(out.buffer).setUint32(0, length, false);
  return out;
}

test('a frame split across any boundary is read once it is complete', () => {
  const bytes = encodeFrame(envelope('split'));
  const decoder = new FrameDecoder();
  for (let i = 0; i < bytes.length - 1; i += 1) {
    decoder.push(bytes.subarray(i, i + 1));
    assert.equal(decoder.next(), null, `incomplete after ${i + 1} bytes`);
  }
  decoder.push(bytes.subarray(bytes.length - 1));
  assert.equal(decoder.next()?.message.type, 'operator_command');
  assert.equal(decoder.buffered, 0);
});

test('two frames in one chunk come out in order', () => {
  const a = encodeFrame(envelope('a'));
  const b = encodeFrame(envelope('b'));
  const both = new Uint8Array(a.length + b.length);
  both.set(a, 0);
  both.set(b, a.length);
  const decoder = new FrameDecoder();
  decoder.push(both);
  const first = decoder.next();
  const second = decoder.next();
  assert.equal(first?.message.type, 'operator_command');
  assert.ok(first && second && second.producer_sequence > first.producer_sequence);
  assert.equal(decoder.next(), null);
});

test('a hostile length is refused before anything is waited for', () => {
  const decoder = new FrameDecoder();
  decoder.push(header(0xffffffff));
  assert.throws(() => decoder.next(), (error: unknown) => error instanceof FrameError && error.kind === 'too_large');
  // The decoder stays failed, and more bytes change nothing.
  decoder.push(encodeFrame(envelope('late')));
  assert.throws(() => decoder.next(), FrameError);

  const small = new FrameDecoder(16);
  small.push(header(17));
  assert.throws(() => small.next(), (error: unknown) => error instanceof FrameError && error.kind === 'too_large');
});

test('an empty frame, bad JSON and a schema-invalid message each fail for their own reason', () => {
  const cases: [Uint8Array, string][] = [
    [header(0), 'empty'],
    [frameOf('{not json'), 'malformed'],
    [frameOf('{"protocol_version":1}'), 'invalid'],
    [frameOf(JSON.stringify({ ...envelope('x'), protocol_version: 9 })), 'invalid'],
  ];
  for (const [bytes, kind] of cases) {
    const decoder = new FrameDecoder();
    decoder.push(bytes);
    assert.throws(() => decoder.next(), (error: unknown) => error instanceof FrameError && error.kind === kind, kind);
  }
});

test('an envelope too large for a frame is refused on the way out', () => {
  const huge = envelope('x'.repeat(MAX_FRAME_BYTES));
  assert.throws(() => encodeFrame(huge), (error: unknown) => error instanceof FrameError && error.kind === 'too_large');
});

test('multi-byte text survives framing, which counts bytes and not characters', () => {
  const decoder = new FrameDecoder();
  decoder.push(encodeFrame(envelope('关闭每日推荐 🔕')));
  const message = decoder.next()?.message;
  assert.ok(message && message.type === 'operator_command');
  if (message.type === 'operator_command' && message.payload.command === 'create_task') {
    assert.equal(message.payload.text, '关闭每日推荐 🔕');
  }
});
