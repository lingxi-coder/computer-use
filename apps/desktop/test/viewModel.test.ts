import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { RootState, UiProjection } from '@computer-user/desktop-api';

import { isTerminal, stateLabel, stopBanner, toViewModel } from '../src/renderer/viewModel.js';

const STATES: RootState[] = [
  'draft', 'compiling_goal', 'awaiting_confirmation', 'needs_input', 'ready', 'running', 'waiting_evidence',
  'waiting_plan', 'pausing', 'suspended', 'needs_revalidation', 'cancelling', 'completed', 'failed',
  'unverified', 'expired', 'cancelled',
];

function projection(state: RootState | null, extra: Partial<UiProjection> = {}): UiProjection {
  return {
    revision: 1,
    mode: 'execute',
    root:
      state === null
        ? null
        : {
            root_task_id: 'root-1',
            task_revision: 1,
            root_epoch: 1,
            state,
            headline: 'turn off the daily picks',
            attempts_used: 1,
            attempts_max: 12,
          },
    stop_status: 'not_stopped',
    cleanup: 'not_needed',
    pending_confirmation: null,
    capabilities: null,
    timeline: [],
    ...extra,
  };
}

test('every task state has words, and only the final ones are terminal', () => {
  for (const state of STATES) assert.ok(stateLabel(state).length > 0, state);
  assert.deepEqual(STATES.filter(isTerminal), ['completed', 'failed', 'unverified', 'expired', 'cancelled']);
});

test('what can be pressed follows the task state', () => {
  const model = (state: RootState | null) => toViewModel(projection(state), 'idle');
  assert.equal(model(null).canCreate, true);
  assert.equal(model('completed').canCreate, true);
  assert.equal(model('running').canCreate, false);
  assert.equal(model('running').canPause, true);
  assert.equal(model('running').canStop, true);
  assert.equal(model('running').canResume, false);
  assert.equal(model('suspended').canResume, true);
  assert.equal(model('suspended').canPause, false);
  assert.equal(model('needs_revalidation').canResume, true);
  assert.equal(model('cancelled').canStop, false);
  assert.equal(model('cancelling').canStop, true, 'stop can be pressed again');
  assert.equal(toViewModel(null, 'idle').canCreate, true);
  assert.equal(toViewModel(null, 'idle').stateLabel, 'No task');
});

test('a stop is shown as confirmed only when something confirmed it, and a failure to confirm is a warning', () => {
  assert.equal(stopBanner('not_stopped', 'idle').level, 'none');
  assert.equal(stopBanner('stopping', 'idle').level, 'pending');
  assert.equal(stopBanner('not_stopped', 'stopping').level, 'pending');
  assert.equal(stopBanner('not_stopped', 'confirmed').level, 'ok');
  assert.equal(stopBanner('stopped_confirmed', 'idle').level, 'ok');
  // The device said it let go while the host is still catching up: that is confirmed.
  assert.equal(stopBanner('stopping', 'confirmed').level, 'ok');
  // Either side failing to confirm wins over everything.
  assert.equal(stopBanner('stop_unconfirmed', 'confirmed').level, 'warning');
  assert.equal(stopBanner('stopping', 'unconfirmed').level, 'warning');
  assert.match(stopBanner('stop_unconfirmed', 'idle').text, /Do not assume it stopped/);
  assert.equal(stopBanner(undefined, 'idle').level, 'none');
});

test('the goal prompt says what will be done and what counts as done, in plain words', () => {
  const model = toViewModel(
    projection('awaiting_confirmation', {
      pending_confirmation: {
        confirmation_id: 'conf-1',
        root_task_id: 'root-1',
        task_revision: 1,
        kind: {
          kind: 'goal',
          goal: {
            user_text: 'turn off the daily picks in Push',
            app: 'test.settings',
            target_label: 'Daily picks',
            group: 'Push',
            desired: { type: 'bool', value: false },
            confirmation_scope: 'ui_revisit',
            max_effect_class: 'reversible_edit',
          },
        },
      },
    }),
    'idle',
  );
  assert.equal(model.prompt?.kind, 'goal');
  assert.equal(model.prompt?.id, 'conf-1');
  const text = model.prompt?.lines.join('\n') ?? '';
  assert.match(text, /"Daily picks" in Push/);
  assert.match(text, /Wanted value: off/);
  assert.match(text, /re-entering the page/);
  assert.match(text, /reversible edit/);
});

test('an effect prompt names the one action and says its answer covers only that', () => {
  const model = toViewModel(
    projection('needs_input', {
      pending_confirmation: {
        confirmation_id: 'conf-2',
        root_task_id: 'root-1',
        task_revision: 1,
        kind: {
          kind: 'effect',
          action: {
            skill: 'ui.activate',
            target_label: 'Apply',
            description: 'Press "Apply" in the dialog',
            effect_class: 'commit',
          },
        },
      },
    }),
    'idle',
  );
  assert.equal(model.prompt?.kind, 'effect');
  const text = model.prompt?.lines.join('\n') ?? '';
  assert.match(text, /Press "Apply"/);
  assert.match(text, /Effect: commit/);
  assert.match(text, /this one action only/);
});

test('progress, the device and the latest timeline entries are carried through', () => {
  const entries = Array.from({ length: 20 }, (_, i) => ({ at_ms: i, text: `step ${i}` }));
  const model = toViewModel(
    projection('running', {
      timeline: entries,
      capabilities: {
        identity: { session_epoch: 2, surface: { id: 'main', epoch: 2 }, clock_domain: 'sim/2/monotonic', dependencies: {} },
        backend: 'simulated',
        policy: 'atomic_only',
        human_input: 'available',
        hit_test: 'available',
        input: 'available',
        max_uninterruptible_ms: 100,
      },
    }),
    'idle',
  );
  assert.deepEqual(model.progress, { used: 1, max: 12 });
  assert.equal(model.capability, 'Simulated device · session 2');
  assert.equal(model.timeline.length, 12);
  assert.equal(model.timeline.at(-1), 'step 19');
  assert.equal(toViewModel(null, 'idle').capability, 'No device connected');
});
