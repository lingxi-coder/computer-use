import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { PassThrough } from 'node:stream';
import test from 'node:test';
import { encodeFrame, EnvelopeFactory, FrameDecoder, type ModelPurpose, type StopScope,
  type StopDisposition, type StopSource } from '@computer-use/desktop-api';
import { Supervisor, type TeacherObservationHandler } from '../src/main/supervisor.js';
import type { TeacherObservation } from '../src/main/teacherShadowObserver.js';
import { binaries, collectLogs, until } from './helpers.js';

const methods = Supervisor.prototype as unknown as {
  attachTeacherObservation(child: unknown): void;
  closeTeacherObservation(child: unknown): void;
  onData(child: unknown, bytes: Buffer): void;
  stopAtDevice(scope: StopScope, disposition: StopDisposition, source: StopSource): void;
};
const golden = (): TeacherObservation[] => JSON.parse(readFileSync(
  new URL('../../../contracts/teacher-observation-v1/fixtures.json', import.meta.url), 'utf8')).messages;
function privateFrame(message: TeacherObservation): Buffer {
  const body = Buffer.from(JSON.stringify(message));
  const prefix = Buffer.alloc(4); prefix.writeUInt32BE(body.length);
  return Buffer.concat([prefix, body]);
}
function receiver(handler: TeacherObservationHandler) {
  const stream = new PassThrough();
  const child = { role: 'host', open: true, expectedExit: false, decoder: new FrameDecoder(), process: { stdio: [null, null, null, null, stream] } };
  const received: unknown[] = [];
  const state = {
    shuttingDown: false, clockOriginMs: Date.now(), children: new Map<string, unknown>([['host', child]]),
    options: { teacherObservationFactory: () => handler },
    closeTeacherObservation: methods.closeTeacherObservation,
    onMessage: (_role: unknown, message: unknown) => received.push(message),
  };
  methods.attachTeacherObservation.call(state, child);
  return { state, child, stream, received };
}

test('private teacher data is observed independently and retired Host bytes cannot reach its replacement', () => {
  const messages: TeacherObservation[] = [];
  let closed = 0;
  const { state, child, stream } = receiver({ observe: message => messages.push(message), close: () => { closed++; } });
  stream.write(privateFrame(golden()[0]!));
  assert.equal(messages.length, 1);
  state.children.set('host', {});
  stream.write(privateFrame(golden()[1]!));
  assert.equal(messages.length, 1);
  assert.equal(closed, 1);
  assert.equal(child.open, true);
  assert.equal(stream.destroyed, true);
});

test('bad private frames or an observer exception retire observation while ordinary Host control remains live', () => {
  for (const badFrame of [false, true]) {
    let closed = 0;
    const { state, child, stream, received } = receiver({
      observe: () => { throw new Error('private observer failure'); }, close: () => { closed++; },
    });
    stream.write(badFrame ? Buffer.from([255, 255, 255, 255]) : privateFrame(golden()[0]!));
    assert.equal(closed, 1);
    assert.equal(stream.destroyed, true);
    assert.equal(child.open, true);
    const factory = new EnvelopeFactory('host', () => 1);
    methods.onData.call(state, child, Buffer.from(encodeFrame(factory.make({ type: 'ready', payload: { role: 'host', notes: [] } }))));
    assert.equal(received.length, 1);
  }
});

test('direct device Stop is sent before optional observer cleanup and does not wait for it', () => {
  const actions: string[] = [];
  let browserStopped = false;
  const host = {};
  const device = {};
  const state = { children: new Map([['device', device], ['host', host]]),
    write: () => { actions.push('device_stop'); },
    stopBrowserSource: () => {
      browserStopped = true;
      actions.push('browser_cleanup_started');
      return new Promise<void>(() => undefined);
    },
    closeComputerImagePort: () => undefined,
    closeTeacherObservation: () => { actions.push('observer_close'); },
  };
  methods.stopAtDevice.call(state, { scope: 'all' }, 'pause', 'operator_button');
  assert.equal(browserStopped, true, 'browser cleanup revokes synchronously');
  assert.deepEqual(actions, ['browser_cleanup_started', 'device_stop', 'observer_close']);
});

test('only an explicitly authorized S1 purpose creates a real Host observation lifecycle', async () => {
  const cases: ModelPurpose[][] = [[], ['s2_planning'], ['s1_teacher']];
  for (const purposes of cases) {
    let created = 0; let closed = 0;
    const supervisor = await Supervisor.start({ binaries: binaries(),
      teacherObservationFactory: () => { created++; return { observe: () => undefined, close: () => { closed++; } }; },
      ...(purposes.length ? { modelAuthorization: {
        route: { provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://example.invalid/v1',
          model: 'fixture-model', max_output_tokens: 4096 }, credential: { kind: 'api_key' as const, api_key: 'teacher-observation-test-key' }, purposes: [...purposes],
      } } : {}),
    });
    const logs = collectLogs(supervisor);
    try {
      assert.equal(created, purposes.includes('s1_teacher') ? 1 : 0);
      await until(() => supervisor.projection !== null, 'initial observation projection', logs);
      assert.equal(supervisor.projection?.root, null);
      assert.equal(logs.join('\n').includes('teacher-observation-test-key'), false);
    } finally { await supervisor.shutdown(); }
    assert.equal(closed, created);
  }
});
