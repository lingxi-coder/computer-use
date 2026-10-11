import { demoModels, demoProviders } from './demoProviderFixture.js';
import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { StopAck, StopSource } from '@computer-use/desktop-api';
import { DecisionWorker, ScriptedTeacher } from '@computer-use/decision-worker';

import { DecisionRelay } from '../src/main/decisionRelay.js';
import { HostBridge } from '../src/main/hostBridge.js';
import { Supervisor } from '../src/main/supervisor.js';
import { DEFAULT_ACCELERATOR, StopBridge, type HotkeyRegistry } from '../src/main/stopBridge.js';
import { toViewModel } from '../src/renderer/viewModel.js';
import { binaries, collectLogs, TEXT, TEXT_AMBIGUOUS, until, worldInputs } from './helpers.js';

class FakeHotkeys implements HotkeyRegistry {
  handlers = new Map<string, () => void>();

  register(accelerator: string, callback: () => void): boolean {
    this.handlers.set(accelerator, callback);
    return true;
  }

  unregister(accelerator: string): void {
    this.handlers.delete(accelerator);
  }

  trigger(accelerator: string): void {
    this.handlers.get(accelerator)?.();
  }
}

/** A session with a confirmed goal that has not started, so a stop has something to stop. */
async function confirmedButNotRunning() {
  // Observe mode never executes: the task waits at READY, deterministically.
  const supervisor = await Supervisor.start({ binaries: binaries(), mode: 'observe' });
  const logs = collectLogs(supervisor);
  const acks: StopAck[] = [];
  supervisor.on('stopAck', (ack) => acks.push(ack));
  const hotkeys = new FakeHotkeys();
  const stop = new StopBridge(supervisor, hotkeys, { confirmTimeoutMs: 5_000 });
  assert.equal(stop.start(), true);
  if (supervisor.projection) stop.observeProjection(supervisor.projection);
  supervisor.on('projection', (projection) => stop.observeProjection(projection));
  const bridge = new HostBridge(supervisor, stop, undefined, undefined, demoModels, undefined, demoProviders);
  bridge.createTask(TEXT);
  await until(() => supervisor.projection?.pending_confirmation != null, 'the goal to be ready', logs);
  bridge.confirm(supervisor.projection!.pending_confirmation!.confirmation_id, true);
  await until(() => supervisor.projection?.root?.state === 'ready', 'the task to be ready', logs);
  return { supervisor, logs, acks, hotkeys, stop, bridge };
}

const press: Record<StopSource & string, (s: Awaited<ReturnType<typeof confirmedButNotRunning>>) => void> = {
  operator_button: (s) => s.bridge.pressStop(),
  tray: (s) => s.stop.press('tray'),
  global_hotkey: (s) => s.hotkeys.trigger(DEFAULT_ACCELERATOR),
  device_local: () => undefined,
  human_input: () => undefined,
  supervisor: () => undefined,
  host_lease_lost: () => undefined,
  host: () => undefined,
};

for (const entry of ['operator_button', 'tray', 'global_hotkey'] as const) {
  test(`the ${entry} stop reaches the device, is confirmed by it, and the task follows`, async () => {
    const session = await confirmedButNotRunning();
    try {
      press[entry]!(session);
      await until(() => session.acks.some((ack) => ack.source === entry), 'the device to acknowledge', session.logs);
      const ack = session.acks.find((a) => a.source === entry)!;
      assert.equal(ack.disposition, 'cancel');
      assert.deepEqual(ack.scope, { scope: 'all' });
      await until(() => session.stop.status === 'confirmed', 'the stop to be confirmed', session.logs);
      await until(() => session.supervisor.projection?.root?.state === 'cancelled', 'the task to be cancelled', session.logs);
      assert.equal(session.supervisor.projection?.stop_status, 'stopped_confirmed');
    } finally {
      session.stop.dispose();
      await session.supervisor.shutdown();
    }
  });
}

test('the hotkey stop reaches the device while the host is frozen, and the task follows when it wakes', async () => {
  const session = await confirmedButNotRunning();
  const hostPid = session.supervisor.hostPid!;
  try {
    process.kill(hostPid, 'SIGSTOP');
    session.hotkeys.trigger(DEFAULT_ACCELERATOR);
    // The device acknowledges without the host: the direct path does not depend on it.
    await until(() => session.acks.some((ack) => ack.source === 'global_hotkey'), 'the device to acknowledge', session.logs);
    await until(() => session.stop.status === 'confirmed', 'the stop to be confirmed', session.logs);
    assert.notEqual(session.supervisor.projection?.root?.state, 'cancelled', 'the frozen host has not heard yet');

    process.kill(hostPid, 'SIGCONT');
    await until(() => session.supervisor.projection?.root?.state === 'cancelled', 'the task to be cancelled', session.logs);
    assert.equal(worldInputs(session.supervisor).length, 0);
  } finally {
    try {
      process.kill(hostPid, 'SIGCONT');
    } catch {
      // already gone
    }
    session.stop.dispose();
    await session.supervisor.shutdown();
  }
});

test('a stop while a decision is pending isolates the late answer, and no input follows', async () => {
  // The backend is slow, so the task waits on it long enough to be stopped.
  const supervisor = await Supervisor.start({ binaries: binaries(), mode: 'execute' });
  const logs = collectLogs(supervisor);
  const relay = new DecisionRelay(supervisor, new DecisionWorker(new ScriptedTeacher({ delayMs: 600, preferWord: 'Push' })));
  relay.attach();
  const hotkeys = new FakeHotkeys();
  const stop = new StopBridge(supervisor, hotkeys, { confirmTimeoutMs: 5_000 });
  stop.start();
  try {
    const requested = new Promise<void>((done) => supervisor.once('decisionRequest', () => done()));
    const bridge = new HostBridge(supervisor, stop, undefined, undefined, demoModels, undefined, demoProviders);
    bridge.createTask(TEXT_AMBIGUOUS);
    await until(() => supervisor.projection?.pending_confirmation != null, 'the goal', logs);
    bridge.confirm(supervisor.projection!.pending_confirmation!.confirmation_id, true);
    await requested;

    hotkeys.trigger(DEFAULT_ACCELERATOR);
    await until(() => supervisor.projection?.root?.state === 'cancelled', 'the task to be cancelled', logs);
    // The teacher finishes anyway; what it says is not delivered.
    await until(() => relay.trace.length > 0, 'the late answer to be dropped', logs);
    assert.match(relay.trace[0] ?? '', /dropped dec-\d+ generation 1: cancelled/);
    await new Promise((done) => setTimeout(done, 300));
    assert.equal(worldInputs(supervisor).length, 0, 'nothing reached the application');
    assert.equal(supervisor.projection?.root?.state, 'cancelled');
  } finally {
    stop.dispose();
    await supervisor.shutdown();
  }
});

test('a new task producing inputs does not inherit the previous task’s stopped banner', async () => {
  const session = await confirmedButNotRunning();
  try {
    session.stop.press('operator_button');
    await until(() => session.supervisor.projection?.root?.state === 'cancelled', 'the first task to stop', session.logs);
    await until(() => session.stop.status === 'confirmed', 'the direct acknowledgement', session.logs);
    const oldRoot = session.supervisor.projection?.root?.root_task_id;
    session.bridge.createTask(TEXT);
    await until(() => session.supervisor.projection?.pending_confirmation != null, 'the new goal', session.logs);
    assert.notEqual(session.supervisor.projection?.root?.root_task_id, oldRoot);
    session.bridge.confirm(session.supervisor.projection!.pending_confirmation!.confirmation_id, true);
    session.bridge.setMode('execute');
    await until(() => worldInputs(session.supervisor).length > 0, 'new task input', session.logs);
    assert.equal(session.stop.status, 'idle');
    assert.equal(session.supervisor.projection?.stop_status, 'not_stopped');
    assert.equal(toViewModel(session.supervisor.projection, session.stop.status).stop.level, 'none');
  } finally {
    session.stop.dispose();
    await session.supervisor.shutdown();
  }
});
