import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { DecisionWorker, ScriptedTeacher } from '@computer-use/decision-worker';
import {
  encodeFrame,
  EnvelopeFactory,
  FrameDecoder,
  type BrowserPageSnapshot,
  type BrowserSourceControl,
  type Message,
  type OperatorCommand,
} from '@computer-use/desktop-api';

import { DecisionRelay } from '../src/main/decisionRelay.js';
import { Supervisor, type SupervisorOptions } from '../src/main/supervisor.js';
import { BrowserSourceController, type BrowserSourceSink } from '../src/main/headless/browserSource.js';
import { alive, binaries, collectLogs, TEXT, TEXT_AMBIGUOUS, until, worldInputs } from './helpers.js';

const browserSupervisorMethods = Supervisor.prototype as unknown as {
  browserControl(control: BrowserSourceControl): Promise<void>;
  browserRequest(message: Message): Promise<void>;
  onData(child: unknown, bytes: Buffer): void;
  rejectBrowserRequests(host: unknown): void;
  stopBrowserSource(): Promise<void>;
  startBrowserSource(): Promise<void>;
  createBrowserSource(): BrowserSourceController;
  observe<T>(observer: ((value: Readonly<T>) => void) | undefined, value: T): void;
  operator(command: OperatorCommand): void;
};

function browserSnapshot(): BrowserPageSnapshot[] {
  return JSON.parse(readFileSync(new URL('../../../contracts/browser-v1/goldens.json', import.meta.url), 'utf8')) as BrowserPageSnapshot[];
}

function browserInstallControl(): BrowserSourceControl {
  return {
    kind: 'install',
    scope: {
      scope_id: '550e8400-e29b-41d4-a716-446655440000',
      generation: 1,
      context: {
        browser_session_id: 'session-1',
        browser_context_id: 'context-1',
        page_target_id: 'target-1',
        cdp_session_id: 'cdp-1',
        frame_id: 'frame-1',
        document_loader_id: 'loader-1',
        fixture_file_url: 'file:///tmp/computer-use/fixtures/headless/readonly-page.html',
        session_epoch: 7,
      },
      clock_domain: 'headless/7/wall',
      issued_ms: 100,
    },
  };
}

function browserPeer() {
  const frames: Buffer[] = [];
  const factory = new EnvelopeFactory('supervisor', () => 100);
  const stdin = {
    writable: true,
    write(frame: Uint8Array, callback?: (error?: Error | null) => void) {
      frames.push(Buffer.from(frame));
      callback?.(null);
      return true;
    },
  };
  const child = {
    role: 'host',
    open: true,
    expectedExit: false,
    decoder: new FrameDecoder(),
    factory,
    process: { stdin },
  };
  const routed: Message[] = [];
  const receiver = Object.assign(new EventEmitter(), {
    children: new Map<string, unknown>([['host', child]]),
    shuttingDown: false,
    browserControlWaiters: new Map<string, unknown>(),
    onMessage: (_role: string, message: Message) => routed.push(message),
  });
  return { child, frames, receiver, routed };
}

function readEnvelope(frame: Uint8Array) {
  const decoder = new FrameDecoder();
  decoder.push(frame);
  const envelope = decoder.next();
  assert.ok(envelope);
  return envelope;
}

const hostAckFactory = new EnvelopeFactory('host', () => 101);
function hostAck(inReplyTo: string, accepted = true): Buffer {
  const envelope = hostAckFactory.make({ type: 'command_ack', payload: { accepted } }, inReplyTo);
  return Buffer.from(encodeFrame(envelope));
}

async function startSession(teacher = new ScriptedTeacher()) {
  const supervisor = await Supervisor.start({ binaries: binaries(), mode: 'execute' });
  const logs = collectLogs(supervisor);
  const relay = new DecisionRelay(supervisor, new DecisionWorker(teacher));
  relay.attach();
  return { supervisor, logs, relay };
}

async function confirmGoal(supervisor: Supervisor, logs: string[], text: string): Promise<void> {
  supervisor.operator({ command: 'create_task', text });
  await until(() => supervisor.projection?.pending_confirmation != null, 'the goal to be ready', logs);
  const pending = supervisor.projection?.pending_confirmation;
  assert.equal(pending?.kind.kind, 'goal');
  supervisor.operator({ command: 'confirm_goal', confirmation_id: pending!.confirmation_id, approved: true });
}

const finished = (supervisor: Supervisor): boolean =>
  ['completed', 'failed', 'unverified', 'expired', 'cancelled'].includes(supervisor.projection?.root?.state ?? '');

test('a task completes across the real processes, and shutting down leaves none of them behind', async () => {
  const { supervisor, logs } = await startSession();
  try {
    await confirmGoal(supervisor, logs, TEXT);
    await until(() => finished(supervisor), 'the task to finish', logs);
    assert.equal(supervisor.projection?.root?.state, 'completed', logs.join('\n'));
    assert.equal(supervisor.projection?.root?.attempts_used, 2);
    assert.equal(supervisor.projection?.capabilities?.backend, 'simulated');
    // The switch was set once, then the page was left and entered again.
    assert.equal(worldInputs(supervisor).length, 3);
  } finally {
    const pids = Object.values(supervisor.pids);
    await supervisor.shutdown();
    assert.equal(pids.length, 3);
    for (const pid of pids) assert.equal(alive(pid), false, `process ${pid} was left running`);
  }
});

test('an ambiguous target is answered by the decision worker through main', async () => {
  const { supervisor, logs, relay } = await startSession(new ScriptedTeacher({ preferWord: 'Push' }));
  try {
    const requests: string[] = [];
    supervisor.on('decisionRequest', (request) => requests.push(request.decision_program));
    await confirmGoal(supervisor, logs, TEXT_AMBIGUOUS);
    await until(() => finished(supervisor), 'the task to finish', logs);
    assert.equal(supervisor.projection?.root?.state, 'completed', logs.join('\n'));
    assert.deepEqual(requests, ['ui.step@1'], 'one question crossed the relay');
    assert.deepEqual(relay.trace, [], 'nothing was dropped');
  } finally {
    await supervisor.shutdown();
  }
});

test('a process that is asked to start but cannot be reached is an error, and nothing is left running', async () => {
  const real = binaries();
  await assert.rejects(
    Supervisor.start({ binaries: { ...real, host: '/nonexistent/cu-host' }, readyTimeoutMs: 2_000 }),
    /ENOENT|did not become ready/,
  );
});

test('browser ingress waits for its correlated ACK from the current Host peer', async () => {
  const { child, frames, receiver, routed } = browserPeer();
  const request = browserSupervisorMethods.browserRequest.call(receiver, {
    type: 'browser_source_control', payload: browserInstallControl(),
  });
  const sent = readEnvelope(frames[0]!);
  assert.equal(sent.producer, 'supervisor');
  assert.equal(receiver.browserControlWaiters.size, 1);

  let settled = false;
  void request.then(() => { settled = true; }, () => { settled = true; });
  const replacement = { role: 'host', open: true, expectedExit: false, decoder: new FrameDecoder() };
  receiver.children.set('host', replacement);
  browserSupervisorMethods.onData.call(receiver, child, hostAck(sent.message_id));
  assert.equal(receiver.browserControlWaiters.size, 1, 'retired Host bytes cannot settle the request');

  browserSupervisorMethods.onData.call(receiver, replacement, hostAck(sent.message_id));
  assert.equal(receiver.browserControlWaiters.size, 1, 'a replacement Host does not own the original pending request');
  assert.equal(routed.length, 1, 'the replacement ACK is not consumed as the pending ACK');

  receiver.children.set('host', child);
  browserSupervisorMethods.onData.call(receiver, child, hostAck('unrelated-message'));
  assert.equal(receiver.browserControlWaiters.size, 1, 'an unrelated correlation id cannot settle the request');
  assert.equal(routed.length, 2);
  await Promise.resolve();
  assert.equal(settled, false);

  browserSupervisorMethods.onData.call(receiver, child, hostAck(sent.message_id));
  await request;
  assert.equal(receiver.browserControlWaiters.size, 0);
  assert.equal(settled, true);

  const rejected = browserSupervisorMethods.browserRequest.call(receiver, {
    type: 'browser_source_control', payload: browserInstallControl(),
  });
  const rejectedEnvelope = readEnvelope(frames[1]!);
  browserSupervisorMethods.onData.call(receiver, child, hostAck(rejectedEnvelope.message_id, false));
  await assert.rejects(rejected, /scope was rejected/);
  assert.equal(receiver.browserControlWaiters.size, 0);
});

test('oversized browser ingress is rejected before consuming a Host envelope sequence', async () => {
  const { frames, receiver } = browserPeer();
  const oversized = {
    type: 'browser_page_snapshot',
    payload: { oversized: 'x'.repeat(1 << 20) },
  } as unknown as Message;
  await assert.rejects(
    browserSupervisorMethods.browserRequest.call(receiver, oversized),
    /control frame limit/,
  );
  assert.equal(frames.length, 0);
  assert.equal(receiver.browserControlWaiters.size, 0);

  const request = browserSupervisorMethods.browserRequest.call(receiver, {
    type: 'browser_source_control', payload: browserInstallControl(),
  });
  const sent = readEnvelope(frames[0]!);
  assert.equal(sent.producer_sequence, 1);
  assert.equal(sent.message_id, 'supervisor-1');
  browserSupervisorMethods.onData.call(receiver, receiver.children.get('host'), hostAck(sent.message_id));
  await request;
});

test('Host closure rejects browser ingress, stops its controller, and prevents later snapshots from publishing', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'cu-browser-supervisor-'));
  const originalCreate = BrowserSourceController.create;
  const snapshot = browserSnapshot()[0]!;
  let sink: BrowserSourceSink | undefined;
  let stopCount = 0;
  const fakeController = {
    start: async () => snapshot,
    refresh: async () => snapshot,
    stop: async () => { stopCount += 1; },
  } as unknown as BrowserSourceController;
  BrowserSourceController.create = ((_options: Parameters<typeof BrowserSourceController.create>[0], sourceSink: BrowserSourceSink) => {
    sink = sourceSink;
    return fakeController;
  }) as typeof BrowserSourceController.create;

  const { child, frames } = browserPeer();
  const published: string[] = [];
  let receiver: EventEmitter & Record<string, any>;
  receiver = Object.assign(new EventEmitter(), {
    options: {
      browserSource: { chromeExecutable: '/unused/chrome', fixturePath: '/unused/fixtures/headless/readonly-page.html' },
      observeBrowserSnapshot: (value: BrowserPageSnapshot) => published.push(value.snapshot_id),
    },
    dir: directory,
    clockOriginMs: Date.now(),
    sessionEpoch: 7,
    browserSource: null,
    browserTimer: null,
    browserStopTask: null,
    children: new Map<string, unknown>([['host', child]]),
    browserControlWaiters: new Map<string, unknown>(),
    shuttingDown: false,
    browserControl: (control: BrowserSourceControl) => browserSupervisorMethods.browserControl.call(receiver, control),
    browserRequest: (message: Message) => browserSupervisorMethods.browserRequest.call(receiver, message),
    retireDeviceBrowserPort: () => undefined,
    createBrowserSource: () => browserSupervisorMethods.createBrowserSource.call(receiver),
    observe: browserSupervisorMethods.observe,
  });

  try {
    await browserSupervisorMethods.startBrowserSource.call(receiver);
    assert.ok(sink);

    const install = sink!.sendControl(browserInstallControl());
    const installEnvelope = readEnvelope(frames[0]!);
    browserSupervisorMethods.onData.call(receiver, child, hostAck(installEnvelope.message_id));
    await install;

    const acknowledgedSnapshot = sink!.sendSnapshot(snapshot);
    const snapshotEnvelope = readEnvelope(frames[1]!);
    browserSupervisorMethods.onData.call(receiver, child, hostAck(snapshotEnvelope.message_id));
    await acknowledgedSnapshot;
    assert.deepEqual(published, [snapshot.snapshot_id], 'Main publishes only after the current Host ACK');

    const pendingSnapshot = sink!.sendSnapshot(snapshot);
    const pendingRejection = assert.rejects(pendingSnapshot, /Host closed/);
    assert.equal(receiver.browserControlWaiters.size, 1);

    child.open = false;
    browserSupervisorMethods.rejectBrowserRequests.call(receiver, child);
    await browserSupervisorMethods.stopBrowserSource.call(receiver);
    await pendingRejection;
    assert.equal(stopCount, 1);
    assert.equal(receiver.browserSource, null);
    assert.equal(receiver.browserTimer, null);

    const frameCountAtClose = frames.length;
    await assert.rejects(sink!.sendSnapshot(snapshot), /channel is unavailable/);
    assert.equal(frames.length, frameCountAtClose, 'the stopped controller cannot publish another snapshot');
    assert.deepEqual(published, [snapshot.snapshot_id]);
  } finally {
    await browserSupervisorMethods.stopBrowserSource.call(receiver);
    BrowserSourceController.create = originalCreate;
    rmSync(directory, { recursive: true, force: true });
  }
});

test('browser observation startup rejects execute or mixed native sessions and its operator seam rejects tasks', async () => {
  const browserSource = {
    chromeExecutable: '/unused/chrome',
    fixturePath: '/unused/fixtures/headless/readonly-page.html',
  };
  await assert.rejects(
    Supervisor.start({ binaries: binaries(), browserSource, mode: 'execute' }),
    /own observe-only session/,
  );
  await assert.rejects(
    Supervisor.start({ binaries: binaries(), browserSource, nativeSource: {} as NonNullable<SupervisorOptions['nativeSource']> }),
    /own observe-only session/,
  );

  const forwarded: OperatorCommand[] = [];
  const receiver = {
    options: { browserSource },
    revisitInProgress: false,
    revisitCancelled: false,
    toHost: (message: { payload: OperatorCommand }) => forwarded.push(message.payload),
  };
  const commands: OperatorCommand[] = [
    { command: 'create_task', text: 'select the checkbox' },
    { command: 'confirm_goal', confirmation_id: 'goal-1', approved: true },
    { command: 'resume_task', root_task_id: 'root-1' },
    { command: 'set_mode', mode: 'shadow' },
    { command: 'set_mode', mode: 'execute' },
  ];
  for (const command of commands) {
    assert.throws(() => browserSupervisorMethods.operator.call(receiver, command), /input is unavailable/);
  }
  browserSupervisorMethods.operator.call(receiver, { command: 'set_mode', mode: 'observe' });
  assert.deepEqual(forwarded, [{ command: 'set_mode', mode: 'observe' }]);
});
