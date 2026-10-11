import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Duplex } from 'node:stream';
import test from 'node:test';

import type {
  BrowserControlBootstrap,
  BrowserControlHello,
  BrowserControlReply,
  BrowserControlSubmit,
} from '@computer-use/desktop-api';
import type { ChildProcess } from 'node:child_process';

import {
  BROWSER_CONTROL_BOOTSTRAP_MAGIC,
  BROWSER_CONTROL_HELLO_MAGIC,
  BROWSER_CONTROL_READY_MAGIC,
  BROWSER_CONTROL_REPLY_MAGIC,
  BROWSER_CONTROL_SUBMIT_MAGIC,
} from '../src/main/headless/browserControlWire.js';
import { DeviceBrowserPort } from '../src/main/headless/deviceBrowserPort.js';

interface ControlGoldens {
  readonly bootstrap: BrowserControlBootstrap;
  readonly hello: BrowserControlHello;
  readonly submit: BrowserControlSubmit;
  readonly reply: BrowserControlReply;
}

const goldenPath = resolve(dirname(fileURLToPath(import.meta.url)), '../../../contracts/browser-v1/control-goldens.json');
const goldens = JSON.parse(readFileSync(goldenPath, 'utf8')) as ControlGoldens;

class FakeFd3 extends Duplex {
  readonly frames: Buffer[] = [];

  override _read(): void {}

  override _write(chunk: Buffer, _encoding: BufferEncoding, callback: (error?: Error | null) => void): void {
    this.frames.push(Buffer.from(chunk));
    callback();
  }

  receive(frame: Buffer): void { this.push(Buffer.from(frame)); }
}

class FakeDevice extends EventEmitter {
  readonly pid = 43_210;
  readonly exitCode: number | null = null;
  readonly signalCode: NodeJS.Signals | null = null;
  readonly killed = false;
}

function frame(magic: string, value: unknown): Buffer {
  const body = Buffer.from(JSON.stringify(value), 'utf8');
  const result = Buffer.alloc(12 + body.length);
  result.write(magic, 0, 'ascii');
  result.writeUInt32BE(body.length, 8);
  body.copy(result, 12);
  return result;
}

function body(packet: Buffer, magic: string): string {
  assert.equal(packet.subarray(0, 8).toString('ascii'), magic);
  const length = packet.readUInt32BE(8);
  assert.equal(packet.length, 12 + length);
  return packet.subarray(12).toString('utf8');
}

async function waitFor(condition: () => boolean): Promise<void> {
  for (let turn = 0; turn < 50; turn += 1) {
    if (condition()) return;
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
  assert.fail('browser control port did not produce its expected frame');
}

function createPort(
  stream: FakeFd3,
  device: FakeDevice,
  onSubmit: (submit: BrowserControlSubmit) => Promise<BrowserControlReply>,
  onRetire: () => void,
) {
  return new DeviceBrowserPort({
    stream,
    device: device as unknown as ChildProcess,
    scope: goldens.bootstrap.scope,
    sessionEpoch: goldens.bootstrap.scope.context.session_epoch,
    channelGeneration: goldens.bootstrap.channel_generation,
    browserPageProfile: goldens.bootstrap.browser_page_profile,
    // A pure transport clock well before the golden reply deadline avoids a
    // 30 ms timer while exercising the exact, unmodified contract frame.
    nowMs: () => 0,
    resource: {
      submit: onSubmit,
      retireNow: async () => { onRetire(); },
    },
  });
}

test('FD3 starts by writing only the private bootstrap, acknowledges Hello, and completes one correlated submit', async () => {
  const stream = new FakeFd3();
  const device = new FakeDevice();
  const submissions: BrowserControlSubmit[] = [];
  let retired = 0;
  const port = createPort(stream, device, async (value) => {
    submissions.push(value);
    return structuredClone(goldens.reply);
  }, () => { retired += 1; });

  const ready = port.start();
  assert.equal(stream.frames.length, 1, 'bootstrap write begins synchronously before Configure');
  assert.equal(stream.frames[0]!.subarray(0, 8).toString('ascii'), BROWSER_CONTROL_BOOTSTRAP_MAGIC);
  const bootstrap = JSON.parse(body(stream.frames[0]!, BROWSER_CONTROL_BOOTSTRAP_MAGIC)) as BrowserControlBootstrap;
  assert.equal(bootstrap.token.length, 64);
  assert.notEqual(bootstrap.token, goldens.bootstrap.token, 'Main mints a per-channel token');

  const hello = { ...goldens.hello, token: bootstrap.token };
  stream.receive(frame(BROWSER_CONTROL_HELLO_MAGIC, hello));
  await ready;
  assert.equal(port.isReady(), true);
  assert.equal(stream.frames.length, 2);
  assert.equal(stream.frames[1]!.subarray(0, 8).toString('ascii'), BROWSER_CONTROL_READY_MAGIC);

  const submit = goldens.submit;
  stream.receive(frame(BROWSER_CONTROL_SUBMIT_MAGIC, submit));
  await waitFor(() => stream.frames.length === 3);
  assert.equal(submissions[0]?.request_id, submit.request_id);
  assert.equal(stream.frames[2]!.subarray(0, 8).toString('ascii'), BROWSER_CONTROL_REPLY_MAGIC);
  assert.equal(JSON.parse(body(stream.frames[2]!, BROWSER_CONTROL_REPLY_MAGIC)).outcome, 'completed');
  assert.equal(port.isReady(), true, 'a valid Completed gesture reply does not imply page effect');

  await port.retire('stopped');
  assert.equal(port.isReady(), false);
  assert.equal(port.isRetired(), true);
  assert.equal(retired, 1);
});

test('FD3 EOF before Hello rejects readiness and retires the one-use source', async () => {
  const stream = new FakeFd3();
  const device = new FakeDevice();
  let retired = 0;
  const port = createPort(stream, device, async () => structuredClone(goldens.reply), () => { retired += 1; });
  const ready = port.start();

  stream.push(null);
  await assert.rejects(ready, /retired before readiness/);
  assert.equal(port.isReady(), false);
  assert.equal(port.isRetired(), true);
  assert.equal(retired, 1);
});

test('Unknown gesture reply is sent once, then synchronously retires FD3 and the source', async () => {
  const stream = new FakeFd3();
  const device = new FakeDevice();
  let retired = 0;
  const unknownReply: BrowserControlReply = {
    ...goldens.reply,
    outcome: 'unknown',
    failure_code: 'effect_unknown',
  };
  const port = createPort(stream, device, async () => structuredClone(unknownReply), () => { retired += 1; });
  const ready = port.start();
  const bootstrap = JSON.parse(body(stream.frames[0]!, BROWSER_CONTROL_BOOTSTRAP_MAGIC)) as BrowserControlBootstrap;
  stream.receive(frame(BROWSER_CONTROL_HELLO_MAGIC, { ...goldens.hello, token: bootstrap.token }));
  await ready;

  stream.receive(frame(BROWSER_CONTROL_SUBMIT_MAGIC, goldens.submit));
  await waitFor(() => stream.frames.length === 3 && stream.destroyed && port.isRetired());
  assert.equal(JSON.parse(body(stream.frames[2]!, BROWSER_CONTROL_REPLY_MAGIC)).outcome, 'unknown');
  assert.equal(port.isReady(), false);
  assert.equal(stream.destroyed, true);
  assert.equal(retired, 1);
});

test('Device cannot send a Submit before Main has written Ready', async () => {
  const stream = new FakeFd3();
  const device = new FakeDevice();
  let retired = 0;
  const port = createPort(stream, device, async () => structuredClone(goldens.reply), () => { retired += 1; });
  const ready = port.start();
  const bootstrap = JSON.parse(body(stream.frames[0]!, BROWSER_CONTROL_BOOTSTRAP_MAGIC)) as BrowserControlBootstrap;

  // Coalescing both inbound frames models an early or pipelined Device peer.
  stream.receive(Buffer.concat([
    frame(BROWSER_CONTROL_HELLO_MAGIC, { ...goldens.hello, token: bootstrap.token }),
    frame(BROWSER_CONTROL_SUBMIT_MAGIC, goldens.submit),
  ]));
  await assert.rejects(ready);
  assert.equal(port.isRetired(), true);
  assert.equal(retired, 1);
});
