import assert from 'node:assert/strict';
import test from 'node:test';
import {
  classifyConfirmedRootReadiness,
  classifyExecutionProgress,
  classifyGoalAdmission,
  classifyPreExecuteSource,
  observeAdmissionRoot,
  waitForConfirmedRootReady,
  waitForExecutionProgress,
  waitForFreshPreExecuteSource,
  waitForGoalAdmission,
} from './admission-state.mjs';

const root = (state, root_task_id = 'root-1', task_revision = 3) => ({ root_task_id, task_revision, state });
const prompt = (root_task_id = 'root-1', task_revision = 3) => ({
  root_task_id,
  task_revision,
  kind: { kind: 'goal', goal: { user_text: 'Frozen objective' } },
});

test('compiling Root remains pending and first identity/state are retained until a matching Goal prompt appears', () => {
  const first = observeAdmissionRoot(null, root('compiling_goal'));
  assert.deepEqual(first.observedRoot, {
    root_task_id: 'root-1', task_revision: 3, first_state: 'compiling_goal', latest_state: 'compiling_goal',
    states_observed: ['compiling_goal'], observation_count: 1,
  });
  assert.equal(classifyGoalAdmission({ root: root('compiling_goal') }, first.observedRoot).kind, 'pending');

  const advanced = observeAdmissionRoot(first.observedRoot, root('awaiting_confirmation'));
  assert.equal(advanced.observedRoot.first_state, 'compiling_goal');
  assert.equal(advanced.observedRoot.latest_state, 'awaiting_confirmation');
  assert.deepEqual(advanced.observedRoot.states_observed, ['compiling_goal', 'awaiting_confirmation']);
  assert.equal(classifyGoalAdmission({ root: root('awaiting_confirmation'), pending_confirmation: prompt() }, advanced.observedRoot).kind,
    'goal_prompt');
});

test('terminal Root is distinct from a prompt and its final state is retained', () => {
  const first = observeAdmissionRoot(null, root('compiling_goal'));
  const terminal = observeAdmissionRoot(first.observedRoot, root('failed'));
  assert.equal(classifyGoalAdmission({ root: root('failed') }, terminal.observedRoot).kind, 'terminal_root');
  assert.equal(terminal.observedRoot.first_state, 'compiling_goal');
  assert.equal(terminal.observedRoot.latest_state, 'failed');
});

test('Goal prompt must match both the current and first observed Root identity', () => {
  const observed = observeAdmissionRoot(null, root('awaiting_confirmation')).observedRoot;
  assert.equal(classifyGoalAdmission({ root: root('awaiting_confirmation'), pending_confirmation: prompt('root-2') }, observed).kind,
    'goal_prompt_mismatch');
  assert.equal(classifyGoalAdmission({ root: root('awaiting_confirmation', 'root-1', 4), pending_confirmation: prompt() }, observed).kind,
    'goal_prompt_mismatch');
});

test('later Root identity changes and invalid projections are not silently adopted', () => {
  const observed = observeAdmissionRoot(null, root('compiling_goal')).observedRoot;
  const changed = observeAdmissionRoot(observed, root('failed', 'root-2', 9));
  assert.deepEqual(changed.conflict, { root_task_id: 'root-2', task_revision: 9, state: 'failed' });
  assert.equal(changed.observedRoot.root_task_id, 'root-1');
  assert.equal(observeAdmissionRoot(observed, root('imaginary')).invalid, true);
});

test('waits through compiling Root states until the real matching Goal prompt arrives', async () => {
  const snapshots = [
    { root: root('compiling_goal') },
    { root: root('compiling_goal') },
    { root: root('awaiting_confirmation'), pending_confirmation: prompt() },
  ];
  const observations = [];
  const outcome = await waitForGoalAdmission({
    readProjection: async () => snapshots.shift() ?? { root: root('awaiting_confirmation'), pending_confirmation: prompt() },
    readUiError: async () => null,
    onObservation: value => observations.push(value),
    timeoutMs: 500,
    pollIntervalMs: 1,
  });
  assert.equal(outcome.kind, 'goal_prompt');
  assert.equal(observations[0].observedRoot.first_state, 'compiling_goal');
  assert.equal(outcome.observedRoot.first_state, 'compiling_goal');
  assert.equal(outcome.observedRoot.latest_state, 'awaiting_confirmation');
  assert.equal(observations.length, 3);
});

test('wait returns typed terminal and explicit refusal instead of accepting a created Root', async () => {
  const terminal = await waitForGoalAdmission({
    readProjection: async () => ({ root: root('failed') }), timeoutMs: 100, pollIntervalMs: 1,
  });
  assert.equal(terminal.kind, 'terminal_root');
  assert.equal(terminal.terminalRoot.state, 'failed');
  assert.equal(terminal.observedRoot.first_state, 'failed');

  const refusal = await waitForGoalAdmission({
    readProjection: async () => ({ root: root('compiling_goal'), timeline: [{ text: 'Frozen task refused' }] }),
    timeoutMs: 100, pollIntervalMs: 1,
  });
  assert.equal(refusal.kind, 'explicit_refusal');
  assert.equal(refusal.observedRoot.latest_state, 'compiling_goal');
});

test('wait timeout retains the compiling Root rather than treating it as admitted', async () => {
  let observedImmediately = null;
  const outcome = await waitForGoalAdmission({
    readProjection: async () => ({ root: root('compiling_goal') }),
    onObservation: observation => { observedImmediately = observation.observedRoot; },
    timeoutMs: 12,
    pollIntervalMs: 1,
  });
  assert.equal(outcome.kind, 'timeout');
  assert.equal(observedImmediately.first_state, 'compiling_goal');
  assert.equal(outcome.observedRoot.latest_state, 'compiling_goal');
});

test('post-confirm readiness returns every typed terminal projection immediately', async () => {
  for (const state of ['completed', 'failed', 'unverified', 'expired', 'cancelled']) {
    const projection = { revision: 19, mode: 'observe', root: root(state), timeline: [{ at_ms: 18, text: `Root ${state}` }] };
    const outcome = await waitForConfirmedRootReady({
      readProjection: async () => projection, rootTaskId: 'root-1', taskRevision: 3,
      timeoutMs: 100, pollIntervalMs: 1,
    });
    assert.equal(outcome.kind, 'terminal_root', state);
    assert.equal(outcome.root.state, state);
    assert.equal(outcome.projection, projection);
  }
});

test('post-confirm readiness waits through compilation and keeps the ready projection', async () => {
  const snapshots = [
    { revision: 20, root: root('compiling_goal') },
    { revision: 21, root: root('awaiting_confirmation') },
    { revision: 22, root: root('ready') },
  ];
  const ready = await waitForConfirmedRootReady({
    readProjection: async () => snapshots.shift(), rootTaskId: 'root-1', taskRevision: 3,
    timeoutMs: 500, pollIntervalMs: 1,
  });
  assert.equal(ready.kind, 'ready');
  assert.equal(ready.projection.revision, 22);
});

test('post-confirm readiness preserves mismatched, suspended and timeout projections', async () => {
  const mismatchedProjection = { revision: 23, root: root('failed', 'root-other', 4), timeline: [{ text: 'Other Root failed' }] };
  const mismatch = classifyConfirmedRootReadiness(mismatchedProjection, 'root-1', 3);
  assert.equal(mismatch.kind, 'root_identity_mismatch');
  assert.equal(mismatch.root.root_task_id, 'root-other');

  const suspendedProjection = { revision: 24, root: root('suspended') };
  const suspended = classifyConfirmedRootReadiness(suspendedProjection, 'root-1', 3);
  assert.equal(suspended.kind, 'not_ready');
  assert.equal(suspended.root.state, 'suspended');

  const timeoutProjection = { revision: 25, root: root('compiling_goal') };
  const timeout = await waitForConfirmedRootReady({
    readProjection: async () => timeoutProjection, rootTaskId: 'root-1', taskRevision: 3,
    timeoutMs: 12, pollIntervalMs: 1,
  });
  assert.equal(timeout.kind, 'timeout');
  assert.equal(timeout.projection, timeoutProjection);

  const readError = await waitForConfirmedRootReady({
    readProjection: async () => { throw new Error('source projection read failed'); },
    rootTaskId: 'root-1', taskRevision: 3,
    timeoutMs: 12, pollIntervalMs: 1,
  });
  assert.equal(readError.kind, 'timeout');
  assert.match(readError.error.message, /source projection read failed/);
});

function preExecuteProjection({ state = 'ready', mode = 'observe', target = null, pending = null, stop = 'not_stopped' } = {}) {
  return {
    revision: 31,
    mode,
    root: root(state),
    stop_status: stop,
    pending_confirmation: pending,
    observation: { observed_target: target },
  };
}

function fixtureTarget(sequence, observedMs, pid = 7001, app = 'com.lingxi.computer-user.mvp-fixture') {
  return {
    snapshot_id: `snapshot-${sequence}`,
    structure_sequence: sequence,
    observed_ms: observedMs,
    valid_until_ms: observedMs + 1_000,
    pid,
    app,
    window_id: 'window-1',
  };
}

test('pre-Execute gate requires a fresh exact Source target after one activation checkpoint', async () => {
  const baselineTarget = fixtureTarget(10, 200);
  const stale = preExecuteProjection({ target: fixtureTarget(10, 200) });
  assert.equal(classifyPreExecuteSource(stale, {
    rootTaskId: 'root-1', taskRevision: 3, fixturePid: 7001,
    bundleId: 'com.lingxi.computer-user.mvp-fixture', windowId: 'window-1', baselineTarget,
  }).kind, 'pending');

  const snapshots = [
    preExecuteProjection({ target: fixtureTarget(11, 210, 9000, 'com.other.app') }),
    stale,
    preExecuteProjection({ target: fixtureTarget(12, 220) }),
  ];
  const outcome = await waitForFreshPreExecuteSource({
    readProjection: async () => snapshots.shift(),
    rootTaskId: 'root-1', taskRevision: 3, fixturePid: 7001,
    bundleId: 'com.lingxi.computer-user.mvp-fixture', windowId: 'window-1', baselineTarget,
    timeoutMs: 500, pollIntervalMs: 1,
  });
  assert.equal(outcome.kind, 'ready');
  assert.equal(outcome.target.structure_sequence, 12);
});

test('pre-Execute source gate stops for terminal, suspended, user-input, mode or Stop changes', () => {
  const options = {
    rootTaskId: 'root-1', taskRevision: 3, fixturePid: 7001,
    bundleId: 'com.lingxi.computer-user.mvp-fixture', windowId: 'window-1',
  };
  assert.equal(classifyPreExecuteSource(preExecuteProjection({ state: 'failed' }), options).kind, 'terminal_root');
  assert.equal(classifyPreExecuteSource(preExecuteProjection({ state: 'suspended' }), options).kind, 'human_suspended');
  assert.equal(classifyPreExecuteSource(preExecuteProjection({ pending: { kind: { kind: 'effect' } } }), options).kind,
    'awaiting_user_input');
  assert.equal(classifyPreExecuteSource(preExecuteProjection({ mode: 'shadow' }), options).kind, 'mode_changed');
  assert.equal(classifyPreExecuteSource(preExecuteProjection({ stop: 'stopping' }), options).kind, 'stop_requested');
});

test('pre-Execute freshness timeout retains the last mismatched Source projection', async () => {
  const projection = preExecuteProjection({ target: fixtureTarget(12, 220, 9000, 'com.other.app') });
  const outcome = await waitForFreshPreExecuteSource({
    readProjection: async () => projection,
    rootTaskId: 'root-1', taskRevision: 3, fixturePid: 7001,
    bundleId: 'com.lingxi.computer-user.mvp-fixture', windowId: 'window-1',
    baselineTarget: fixtureTarget(10, 200), timeoutMs: 12, pollIntervalMs: 1,
  });
  assert.equal(outcome.kind, 'timeout');
  assert.equal(outcome.lastClassification.reason, 'fixture_source_target_not_current');
  assert.equal(outcome.projection, projection);
});

function confirmation(kind = 'effect') {
  return {
    confirmation_id: 'confirmation-1',
    root_task_id: 'root-1',
    task_revision: 3,
    kind: kind === 'effect'
      ? { kind, action: { skill: 'ui.activate', effect_class: 'commit', target_label: 'Save' } }
      : { kind: 'goal', goal: { user_text: 'Unexpected goal' } },
  };
}

function executionProjection({
  revision = 40,
  state = 'running',
  mode = 'execute',
  stop = 'not_stopped',
  pending = null,
} = {}) {
  return {
    revision,
    mode,
    root: root(state),
    stop_status: stop,
    pending_confirmation: pending,
  };
}

test('execution progress ends for any Goal or effect confirmation without action approval', () => {
  const options = { rootTaskId: 'root-1', taskRevision: 3 };
  for (const pending of [confirmation('goal'), confirmation('effect')]) {
    const outcome = classifyExecutionProgress(executionProjection({ pending }), options);
    assert.equal(outcome.kind, 'needs_user_action');
    assert.equal(outcome.reason, 'unexpected_confirmation');
  }
});

test('execution progress preserves terminal states, suspension, Root conflict, and malformed projections', () => {
  const options = { rootTaskId: 'root-1', taskRevision: 3 };
  assert.equal(classifyExecutionProgress(executionProjection({ state: 'completed' }), options).kind, 'terminal_root');
  assert.equal(classifyExecutionProgress(executionProjection({ state: 'completed', pending: confirmation() }), options).reason,
    'unexpected_confirmation');
  assert.equal(classifyExecutionProgress(executionProjection({ state: 'suspended' }), options).kind, 'terminal_root');
  const modeChanged = classifyExecutionProgress(executionProjection({ mode: 'observe' }), options);
  assert.equal(modeChanged.reason, 'mode_changed');
  assert.equal(classifyExecutionProgress({ ...executionProjection(), root: root('suspended') }, options).root.state, 'suspended');
  assert.equal(classifyExecutionProgress({ ...executionProjection(), root: root('running', 'root-2', 4) }, options).kind,
    'root_identity_mismatch');
  assert.equal(classifyExecutionProgress({ ...executionProjection(), revision: Number.MAX_SAFE_INTEGER + 1 }, options).reason,
    'invalid_projection_revision');
  assert.equal(classifyExecutionProgress(null, options).reason, 'invalid_projection');
});

test('execution wait tracks the exact Root through to a terminal state', async () => {
  const snapshots = [
    executionProjection({ revision: 40 }),
    executionProjection({ revision: 41 }),
    executionProjection({ revision: 42, state: 'completed' }),
  ];
  const outcome = await waitForExecutionProgress({
    readProjection: async () => snapshots.shift(),
    rootTaskId: 'root-1', taskRevision: 3,
    timeoutMs: 500, pollIntervalMs: 1,
  });
  assert.equal(outcome.kind, 'terminal_root');
  assert.equal(outcome.root.state, 'completed');
  assert.equal(outcome.projection.revision, 42);
});

test('execution wait stops at any unexpected confirmation on its first projection', async () => {
  let reads = 0;
  const outcome = await waitForExecutionProgress({
    readProjection: async () => {
      reads += 1;
      return executionProjection({ pending: confirmation('effect') });
    },
    rootTaskId: 'root-1', taskRevision: 3,
    timeoutMs: 500, pollIntervalMs: 1,
  });
  assert.equal(outcome.kind, 'needs_user_action');
  assert.equal(outcome.reason, 'unexpected_confirmation');
  assert.equal(reads, 1);
});
