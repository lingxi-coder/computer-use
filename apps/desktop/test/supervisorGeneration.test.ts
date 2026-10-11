import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import test from 'node:test';
import { encodeFrame, EnvelopeFactory, FrameDecoder } from '@computer-use/desktop-api';
import { Supervisor } from '../src/main/supervisor.js';

test('retired peer bytes cannot resolve the replacement Ready', () => {
  const emitter = new EventEmitter();
  const current = { role: 'source', open: true, expectedExit: false, decoder: new FrameDecoder() };
  const retired = { role: 'source', open: true, expectedExit: false, decoder: new FrameDecoder() };
  let ready = 0;
  const receiver = Object.assign(emitter, {
    children: new Map([['source', current]]),
    ready: new Map([['source', () => { ready += 1; }]]),
    onMessage: (Supervisor.prototype as unknown as { onMessage: Function }).onMessage,
  });
  const receive = (Supervisor.prototype as unknown as { onData: (child: unknown, bytes: Buffer) => void }).onData;
  const factory = new EnvelopeFactory('source', () => 1);
  const frame = Buffer.from(encodeFrame(factory.make({ type: 'ready', payload: { role: 'source', notes: [] } })));
  receive.call(receiver, retired, frame);
  assert.equal(ready, 0);
  receive.call(receiver, current, frame);
  assert.equal(ready, 1);
  current.expectedExit = true;
  receive.call(receiver, current, frame);
  assert.equal(ready, 1);
  current.expectedExit = false; current.open = false;
  receive.call(receiver, current, frame);
  assert.equal(ready, 1);
});

test('Root revisit cannot create a private control channel without owned Save authorization', async () => {
  await assert.rejects(Supervisor.start({
    binaries: { host: '/nonexistent/cu-host', device: '/nonexistent/cu-device', source: '/nonexistent/cu-source' },
    ownedDocumentRootRevisit: true,
  }), /owned Root revisit requires owned Save grants/);
});

test('a synchronous shutdown or replacement between coalesced frames rejects the remaining frame', () => {
  for (const replace of [false, true]) {
    const current = { role: 'source', open: true, expectedExit: false, decoder: new FrameDecoder() };
    let ready = 0;
    const receiver = {
      shuttingDown: false,
      children: new Map<string, unknown>([['source', current]]),
      onMessage: (Supervisor.prototype as unknown as { onMessage: Function }).onMessage,
      ready: new Map([['source', () => {
        ready += 1;
        if (replace) receiver.children.set('source', {});
        else receiver.shuttingDown = true;
      }]]),
    };
    const factory = new EnvelopeFactory('source', () => 1);
    const frame = () => Buffer.from(encodeFrame(factory.make({ type: 'ready', payload: { role: 'source', notes: [] } })));
    const receive = (Supervisor.prototype as unknown as { onData: (child: unknown, bytes: Buffer) => void }).onData;
    receive.call(receiver, current, Buffer.concat([frame(), frame()]));
    assert.equal(ready, 1);
    receive.call(receiver, current, frame());
    assert.equal(ready, 1);
  }
});
