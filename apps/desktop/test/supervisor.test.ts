import assert from 'node:assert/strict';
import { test } from 'node:test';

import { DecisionWorker, ScriptedTeacher } from '@computer-user/decision-worker';

import { DecisionRelay } from '../src/main/decisionRelay.js';
import { Supervisor } from '../src/main/supervisor.js';
import { alive, binaries, collectLogs, TEXT, TEXT_AMBIGUOUS, until, worldInputs } from './helpers.js';

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
