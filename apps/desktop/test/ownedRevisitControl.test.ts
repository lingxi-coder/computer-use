import assert from 'node:assert/strict';
import { Duplex } from 'node:stream';
import test from 'node:test';
import { OwnedRevisitControl } from '../src/main/ownedRevisitControl.js';

class Pipe extends Duplex {
  frames: Buffer[] = [];
  _read(): void {}
  _write(chunk: Buffer, _encoding: BufferEncoding, callback: (error?: Error | null) => void): void {
    this.frames.push(Buffer.from(chunk)); callback();
  }
  request(): Record<string, unknown> { return JSON.parse(this.frames.at(-1)!.subarray(4).toString('utf8')); }
  reply(value: unknown, split = false): void {
    const body = Buffer.from(JSON.stringify(value));
    const header = Buffer.alloc(4); header.writeUInt32BE(body.length);
    const frame = Buffer.concat([header, body]);
    if (split) for (const byte of frame) this.push(Buffer.from([byte]));
    else this.push(frame);
  }
}

test('private control handles fragmented frames and matching continuation', async () => {
  const pipe = new Pipe(); const control = new OwnedRevisitControl(pipe);
  const prepared = control.prepare(); const first = pipe.request();
  assert.deepEqual(Object.keys(first).sort(), ['request_id', 'type', 'version']);
  const transition = '123e4567-e89b-42d3-a456-426614174000';
  pipe.reply({ version: 1, type: 'prepared', request_id: first.request_id, challenge: { transition_id: transition } }, true);
  assert.deepEqual((await prepared).challenge, { transition_id: transition });
  const finished = control.finish({ transition_id: transition }, 'a'.repeat(32), 'b'.repeat(32));
  pipe.reply({ version: 1, type: 'finished', request_id: pipe.request().request_id, transition_id: transition });
  await finished; control.close();
});

test('oversized or invalid UTF8 frame closes the control before accepting content', async () => {
  for (const frame of [Buffer.from([0, 0, 128, 1]), Buffer.from([0, 0, 0, 2, 0xc0, 0xaf])]) {
    const pipe = new Pipe(); const control = new OwnedRevisitControl(pipe);
    const result = assert.rejects(control.prepare(), /closed/);
    pipe.push(frame); await result;
    assert.equal(pipe.destroyed, true);
  }
});

test('unknown fields, mismatched requests, replies and transition IDs fail closed', async () => {
  for (const mutation of [
    { version: 2 }, { request_id: '123e4567-e89b-42d3-a456-426614174000' },
    { unexpected: 'private-data' }, { type: 'finished', transition_id: 'x' }, { challenge: [] },
  ]) {
    const pipe = new Pipe(); const control = new OwnedRevisitControl(pipe);
    const result = assert.rejects(control.prepare(), /closed/);
    pipe.reply({ version: 1, type: 'prepared', request_id: pipe.request().request_id, challenge: {}, ...mutation });
    await result; assert.equal(pipe.destroyed, true);
  }
  const pipe = new Pipe(); const control = new OwnedRevisitControl(pipe);
  const result = assert.rejects(control.finish({ transition_id: '123e4567-e89b-42d3-a456-426614174000' }, 'a'.repeat(32), 'b'.repeat(32)), /closed/);
  pipe.reply({ version: 1, type: 'finished', request_id: pipe.request().request_id, transition_id: '123e4567-e89b-42d3-a456-426614174001' });
  await result;
});

test('only one request can wait and timeout prevents any subsequent retry', async () => {
  const pipe = new Pipe(); const control = new OwnedRevisitControl(pipe);
  const waiting = assert.rejects(control.prepare(10), /closed/);
  await assert.rejects(control.prepare(), /unavailable/);
  await waiting;
  await assert.rejects(control.prepare(), /unavailable/);
  assert.equal(pipe.frames.length, 1);
});

test('replayed response closes channel without completing another operation', async () => {
  const pipe = new Pipe(); const control = new OwnedRevisitControl(pipe);
  const prepared = control.prepare(); const reply = { version: 1, type: 'prepared', request_id: pipe.request().request_id, challenge: {} };
  pipe.reply(reply); await prepared; pipe.reply(reply);
  await new Promise(done => setImmediate(done));
  assert.equal(pipe.destroyed, true);
  await assert.rejects(control.prepare(), /unavailable/);
});

test('a rejected request exposes only a fixed error and permits explicit next request', async () => {
  const pipe = new Pipe(); const control = new OwnedRevisitControl(pipe);
  const rejected = assert.rejects(control.prepare(), error => error instanceof Error && error.message === 'private revisit request rejected');
  pipe.reply({ version: 1, type: 'rejected', request_id: pipe.request().request_id, code: 'not_settled' });
  await rejected;
  const pending = assert.rejects(control.prepare(), /closed/); control.close(); await pending;
});
