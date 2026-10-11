import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import {
  type BrowserControlBootstrap,
  type BrowserControlHello,
  type BrowserControlReply,
  type BrowserControlSubmit,
  validateBrowserControlReady,
} from '@computer-use/desktop-api';

import {
  BROWSER_CONTROL_BOOTSTRAP_MAGIC,
  BROWSER_CONTROL_HELLO_MAGIC,
  BROWSER_CONTROL_READY_MAGIC,
  BROWSER_CONTROL_REPLY_MAGIC,
  BROWSER_CONTROL_SUBMIT_MAGIC,
  BrowserControlFrameDecoder,
  encodeBrowserControlBootstrap,
  encodeBrowserControlReady,
  encodeBrowserControlReply,
} from '../src/main/headless/browserControlWire.js';

interface ControlGoldens {
  bootstrap: BrowserControlBootstrap;
  hello: BrowserControlHello;
  ready: { version: number; channel_generation: number; session_epoch: number; scope_id: string;
    scope_generation: number; next_sequence: number };
  submit: BrowserControlSubmit;
  reply: BrowserControlReply;
}

const fixturePath = resolve(dirname(fileURLToPath(import.meta.url)), '../../../contracts/browser-v1/control-goldens.json');
const goldens = JSON.parse(readFileSync(fixturePath, 'utf8')) as ControlGoldens;
const headerLength = 12;

function frame(magic: string, body: Uint8Array): Buffer {
  const packet = Buffer.alloc(headerLength + body.byteLength);
  packet.write(magic, 0, 'ascii');
  packet.writeUInt32BE(body.byteLength, 8);
  Buffer.from(body).copy(packet, headerLength);
  return packet;
}

function jsonFrame(magic: string, value: unknown): Buffer {
  return frame(magic, Buffer.from(JSON.stringify(value), 'utf8'));
}

function bodyOf(packet: Buffer, magic: string): string {
  assert.equal(packet.subarray(0, 8).toString('ascii'), magic);
  const length = packet.readUInt32BE(8);
  assert.equal(packet.length, headerLength + length);
  return packet.subarray(headerLength).toString('utf8');
}

test('Main encoders match Rust-generated browser-control DTO field order and framing', async () => {
  const bootstrap = encodeBrowserControlBootstrap(goldens.bootstrap);
  assert.equal(bodyOf(bootstrap, BROWSER_CONTROL_BOOTSTRAP_MAGIC), JSON.stringify(goldens.bootstrap));

  const ready = encodeBrowserControlReady(goldens.ready);
  assert.equal(bodyOf(ready, BROWSER_CONTROL_READY_MAGIC), JSON.stringify(goldens.ready));
  assert.equal(validateBrowserControlReady(goldens.ready, goldens.hello).ok, true);

  const reply = await encodeBrowserControlReply(goldens.reply, goldens.submit);
  assert.equal(bodyOf(reply, BROWSER_CONTROL_REPLY_MAGIC), JSON.stringify(goldens.reply));
});

test('FD3 decoder accepts only authenticated Hello then one correlated increasing Submit at a time', async () => {
  const decoder = new BrowserControlFrameDecoder(goldens.bootstrap);
  const delivered: string[] = [];
  const hello = jsonFrame(BROWSER_CONTROL_HELLO_MAGIC, goldens.hello);
  await decoder.push(hello.subarray(0, 7), async (message) => { delivered.push(message.kind); });
  assert.equal(decoder.buffered, 7, 'partial header bytes remain bounded');
  await decoder.push(hello.subarray(7), async (message) => {
    assert.equal(message.kind, 'hello');
    if (message.kind === 'hello') assert.equal(validateBrowserControlReady(goldens.ready, message.value).ok, true);
    // In production this callback writes the matching Ready before it resolves.
    delivered.push(message.kind);
  });
  await decoder.push(jsonFrame(BROWSER_CONTROL_SUBMIT_MAGIC, goldens.submit), async (message) => {
    assert.equal(message.kind, 'submit');
    if (message.kind === 'submit') await encodeBrowserControlReply(goldens.reply, message.value);
    // In production this callback writes the reply before the decoder advances sequence.
    delivered.push(message.kind);
  });
  assert.deepEqual(delivered, ['hello', 'submit']);
  assert.equal(decoder.buffered, 0);
  decoder.dispose();
});

test('FD3 decoder rejects wrong direction, duplicate JSON keys, invalid UTF-8, oversized lengths and replay', async () => {
  const wrongDirection = new BrowserControlFrameDecoder(goldens.bootstrap);
  await assert.rejects(wrongDirection.push(jsonFrame(BROWSER_CONTROL_BOOTSTRAP_MAGIC, goldens.bootstrap), () => {}), /frame is invalid/);

  const nonAsciiMagic = new BrowserControlFrameDecoder(goldens.bootstrap);
  const highBitHeader = jsonFrame(BROWSER_CONTROL_HELLO_MAGIC, goldens.hello);
  highBitHeader[0] = highBitHeader[0]! | 0x80;
  await assert.rejects(nonAsciiMagic.push(highBitHeader, () => {}), /frame is invalid/,
    'the eight-byte magic must match exact ASCII bytes');

  const duplicate = new BrowserControlFrameDecoder(goldens.bootstrap);
  const helloJson = JSON.stringify(goldens.hello).replace('{"version":1,', '{"version":1,"version":1,');
  assert.notEqual(helloJson, JSON.stringify(goldens.hello));
  await assert.rejects(duplicate.push(frame(BROWSER_CONTROL_HELLO_MAGIC, Buffer.from(helloJson)), () => {}), /frame is invalid/);

  const invalidUtf8 = new BrowserControlFrameDecoder(goldens.bootstrap);
  await assert.rejects(invalidUtf8.push(frame(BROWSER_CONTROL_HELLO_MAGIC, Buffer.from([0xff, 0xfe])), () => {}), /frame is invalid/);

  const bom = new BrowserControlFrameDecoder(goldens.bootstrap);
  await assert.rejects(bom.push(frame(BROWSER_CONTROL_HELLO_MAGIC,
    Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from(JSON.stringify(goldens.hello))])), () => {}),
  /frame is invalid/, 'JSON framing must not normalize a UTF-8 BOM');

  const oversized = new BrowserControlFrameDecoder(goldens.bootstrap);
  const header = Buffer.alloc(headerLength);
  header.write(BROWSER_CONTROL_HELLO_MAGIC, 0, 'ascii');
  header.writeUInt32BE(16 * 1024 + 1, 8);
  await assert.rejects(oversized.push(header, () => {}), /frame is invalid/);

  const replay = new BrowserControlFrameDecoder(goldens.bootstrap);
  await replay.push(jsonFrame(BROWSER_CONTROL_HELLO_MAGIC, goldens.hello), async () => {});
  await replay.push(jsonFrame(BROWSER_CONTROL_SUBMIT_MAGIC, goldens.submit), async () => {});
  await assert.rejects(replay.push(jsonFrame(BROWSER_CONTROL_SUBMIT_MAGIC, goldens.submit), async () => {}), /frame is invalid/);

  const pipelined = new BrowserControlFrameDecoder(goldens.bootstrap);
  await pipelined.push(jsonFrame(BROWSER_CONTROL_HELLO_MAGIC, goldens.hello), async () => {});
  const nextSubmit = { ...goldens.submit, sequence: goldens.submit.sequence + 1 };
  const queuedSubmits = Buffer.concat([
    jsonFrame(BROWSER_CONTROL_SUBMIT_MAGIC, goldens.submit),
    jsonFrame(BROWSER_CONTROL_SUBMIT_MAGIC, nextSubmit),
  ]);
  await assert.rejects(pipelined.push(queuedSubmits, async () => {}), /frame is invalid/,
    'Device cannot queue a second request before receiving Main’s reply');
});

test('FD3 failures never print the bootstrap credential', async () => {
  const token = goldens.bootstrap.token;
  const mismatched = { ...goldens.hello, token: '22'.repeat(32) };
  const decoder = new BrowserControlFrameDecoder(goldens.bootstrap);
  await assert.rejects(decoder.push(jsonFrame(BROWSER_CONTROL_HELLO_MAGIC, mismatched), () => {}), (error: unknown) => {
    assert.ok(error instanceof Error);
    assert.equal(error.message.includes(token), false);
    assert.equal(error.message.includes(mismatched.token), false);
    return true;
  });
});
