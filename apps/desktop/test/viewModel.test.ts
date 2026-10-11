import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { RootState, UiProjection } from '@computer-use/desktop-api';

import { isTerminal, observationDisplay, stateLabel, stopBanner, toViewModel } from '../src/renderer/viewModel.js';
import { nativeUnavailableDomains, simulatedOnlyDomains } from './capabilityFixtures.js';

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
    observation: null,
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
  // A current stop must not be hidden behind a previous release confirmation.
  assert.equal(stopBanner('stopping', 'confirmed').level, 'pending');
  assert.equal(stopBanner('stopped_confirmed', 'stopping').level, 'pending');
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
            allowed_scope: {
              application: 'test.settings',
              targets: [{ control: 'Daily picks', group: 'Push' }],
              no_unrelated_controls: true,
            },
            scoped_revisit: null,
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
  assert.match(text, /Allowed application: test\.settings/);
  assert.match(text, /Allowed control: Daily picks/);
  assert.match(text, /Allowed group: Push/);
  assert.match(text, /Unrelated controls are outside this task/);
  assert.match(text, /Wanted value: off/);
  assert.match(text, /re-entering the page/);
  assert.match(text, /reversible edit/);
});

test('manual scoped commit names the exact Save control and context without AX or prefix fields', () => {
  const source = projection('awaiting_confirmation', {
    pending_confirmation: {
      confirmation_id: 'conf-save', root_task_id: 'root-save', task_revision: 1,
      kind: { kind: 'goal', goal: {
        user_text: 'Change the display name and save it', app: 'test.settings', target_label: 'Display name', group: 'Profile',
        allowed_scope: { application: 'test.settings', targets: [{ control: 'Display name', form: 'Profile' }], no_unrelated_controls: true },
        desired: { type: 'text', value: 'Ada' }, scoped_revisit: null, confirmation_scope: 'commit', max_effect_class: 'reversible_edit',
      } },
    },
  });
  const pending = source.pending_confirmation!;
  if (pending.kind.kind === 'goal') Object.assign(pending.kind.goal, { scoped_commit: {
    action: { label: 'Save', group: 'Profile', semantic_target: 'settings.profile.save', role_hint: 'AXButton', row: null },
    receipt: { group: 'Profile', role_hint: 'AXButton', description: 'AXPress the Profile Save control', kind: 'manual', text_prefix: 'private value prefix' },
  } });

  const text = toViewModel(source, 'idle').prompt?.lines.join('\n') ?? '';
  assert.match(text, /Allowed control: Display name/);
  assert.match(text, /Manual save: press "Save" in "Profile"\./);
  assert.match(text, /Done when the change is saved/);
  assert.doesNotMatch(text, /settings\.profile\.save|AXButton|AXPress|private value prefix/);
});

test('automatic scoped commit asks for the app save confirmation without showing internal receipt fields', () => {
  const source = projection('awaiting_confirmation', {
    pending_confirmation: {
      confirmation_id: 'conf-auto-save', root_task_id: 'root-auto-save', task_revision: 1,
      kind: { kind: 'goal', goal: {
        user_text: 'Change the display name', app: 'test.settings', target_label: 'Display name', group: 'Profile',
        desired: { type: 'text', value: 'Ada' }, scoped_revisit: null, confirmation_scope: 'commit', max_effect_class: 'reversible_edit',
      } },
    },
  });
  const pending = source.pending_confirmation!;
  if (pending.kind.kind === 'goal') Object.assign(pending.kind.goal, { scoped_commit: {
    action: null,
    receipt: { group: 'Profile', role_hint: 'AXTextField', description: 'Autosave after 800 ms', kind: 'automatic', text_prefix: 'private value prefix' },
  } });

  const text = toViewModel(source, 'idle').prompt?.lines.join('\n') ?? '';
  assert.match(text, /The app saves changes automatically in "Profile"\./);
  assert.match(text, /Wait for the app’s automatic save confirmation/);
  assert.match(text, /Done when the change is saved/);
  assert.doesNotMatch(text, /AXTextField|800 ms|private value prefix/);
});

test('the goal prompt shows only the Host-frozen away and return targets for verification', () => {
  const model = toViewModel(projection('awaiting_confirmation', {
    pending_confirmation: {
      confirmation_id: 'conf-revisit', root_task_id: 'root-revisit', task_revision: 1,
      kind: { kind: 'goal', goal: {
        user_text: 'Turn off Weekly digest', app: 'test.settings', target_label: 'Weekly digest', group: 'Notifications',
        allowed_scope: {
          application: 'test.settings',
          targets: [{ control: 'Weekly digest', group: 'Notifications' }],
          no_unrelated_controls: true,
        },
        scoped_revisit: {
          home: {
            semantic_target: 'settings.home', label: 'Settings', group: null, role_hint: 'AXRadioButton', row: null,
          },
          away: {
            semantic_target: 'settings.overview', label: 'Overview', group: null, role_hint: 'AXRadioButton', row: null,
          },
        },
        desired: { type: 'bool', value: false }, confirmation_scope: 'ui_revisit', max_effect_class: 'reversible_edit',
      } },
    },
  }), 'idle');

  const text = model.prompt?.lines.join('\n') ?? '';
  assert.match(text, /may switch to "Overview" and return to "Settings"/);
  assert.match(text, /only allowed detour/);
  assert.match(text, /task control scope remains unchanged/);
  assert.doesNotMatch(text, /settings\.home|settings\.overview/);
});

test('the goal prompt identifies a typed form context without presenting it as a group', () => {
  const model = toViewModel(projection('awaiting_confirmation', {
    pending_confirmation: {
      confirmation_id: 'conf-form', root_task_id: 'root-form', task_revision: 1,
      kind: { kind: 'goal', goal: {
        user_text: 'Update the display name in Profile.', app: 'test.settings', target_label: 'Display name', group: 'Profile',
        allowed_scope: {
          application: 'test.settings',
          targets: [{ control: 'Display name', form: 'Profile' }],
          no_unrelated_controls: true,
        },
        scoped_revisit: {
          home: { semantic_target: 'settings.home', label: 'Settings', group: null, role_hint: 'AXRadioButton', row: null },
          away: { semantic_target: 'settings.profile', label: 'Profile', group: null, role_hint: 'AXRadioButton', row: null },
        },
        desired: { type: 'text', value: 'Ada' },
        confirmation_scope: 'ui_revisit',
        max_effect_class: 'reversible_edit',
      } },
    },
  }), 'idle');

  const text = model.prompt?.lines.join('\n') ?? '';
  assert.match(text, /Control: "Display name" in form "Profile"/);
  assert.match(text, /Allowed form: Profile/);
  assert.doesNotMatch(text, /Allowed group: Profile/);
  assert.match(text, /Allowed control: Display name/);
  assert.match(text, /only allowed detour/);
});

test('CurrentUi confirmation describes a fresh current-page read', () => {
  const model = toViewModel(projection('awaiting_confirmation', {
    pending_confirmation: {
      confirmation_id: 'conf-browser', root_task_id: 'root-1', task_revision: 1,
      kind: { kind: 'goal', goal: {
        user_text: 'Enable browser fixture checkbox', app: 'browser.fixture',
        target_label: 'Background option', group: null, desired: { type: 'bool', value: true },
        scoped_revisit: null, confirmation_scope: 'current_ui', max_effect_class: 'reversible_edit',
      } },
    },
  }), 'idle');
  const text = model.prompt?.lines.join('\n') ?? '';
  assert.match(text, /fresh read.*current page/);
  assert.doesNotMatch(text, /re-entering|saved/);
  assert.doesNotMatch(text, /only allowed detour/);
});

test('a same-name row confirmation shows the frozen table and public disambiguating columns', () => {
  const model = toViewModel(
    projection('awaiting_confirmation', {
      pending_confirmation: {
        confirmation_id: 'conf-row',
        root_task_id: 'root-row',
        task_revision: 1,
        kind: {
          kind: 'goal',
          goal: {
            user_text: 'Open Weekly report owned by Mina',
            app: 'com.lingxi.computer-user.mvp-fixture',
            target_label: 'Weekly report',
            group: null,
            row: {
              table_scope: {
                scroll_path: [
                  { role: 'ax_scroll_area', label: 'Preferences content' },
                  { role: 'ax_scroll_area', label: 'Projects' },
                ],
                column_headers: ['Name', 'Owner', 'Status'],
              },
              conditions: [
                { column_name: 'Name', value: { type: 'text', value: 'Weekly report' } },
                { column_name: 'Owner', value: { type: 'text', value: 'Mina' } },
              ],
            },
            desired: { type: 'bool', value: true },
            scoped_revisit: null,
            confirmation_scope: 'ui_revisit',
            max_effect_class: 'navigation',
          },
        },
      },
    }),
    'idle',
  );
  assert.equal(model.prompt?.id, 'conf-row');
  const text = model.prompt?.lines.join('\n') ?? '';
  assert.match(text, /Table: Preferences content → Projects/);
  assert.match(text, /Columns: Name, Owner, Status/);
  assert.match(text, /Name: "Weekly report"/);
  assert.match(text, /Owner: "Mina"/);
  assert.match(text, /Wanted state: this row is selected/);
  assert.match(text, /re-entering the page/);
  assert.doesNotMatch(text, /Wanted value: on|AXIndex|instance_ref|private_record_id/);
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
        execution_domain: 'simulated',
        domains: simulatedOnlyDomains(),
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
  assert.equal(model.canExecute, true);
  assert.equal(model.timeline.length, 12);
  assert.equal(model.timeline.at(-1), 'step 19');
  assert.equal(toViewModel(null, 'idle').capability, 'No device connected');
});

test('browser execution uses only the dedicated HeadlessBrowser action capability', () => {
  const active = projection('ready', {
    capabilities: {
      identity: { session_epoch: 7, surface: { id: 'fixture-page', epoch: 7 }, clock_domain: 'headless/7/wall', dependencies: {} },
      backend: 'headless_browser',
      execution_domain: 'headless_browser',
      domains: [
        { domain: 'foreground_native', state: 'unavailable', actions: [] },
        { domain: 'background_ax', state: 'unavailable', actions: [] },
        { domain: 'headless_browser', state: 'available', actions: ['headless_browser_action'] },
        { domain: 'app_integration', state: 'unavailable', actions: [] },
        { domain: 'simulated', state: 'unavailable', actions: [] },
      ],
      policy: 'atomic_only',
      human_input: 'unavailable',
      hit_test: 'unavailable',
      input: 'unavailable',
      max_uninterruptible_ms: 0,
    },
  });
  const model = toViewModel(active, 'idle');
  assert.equal(model.canExecute, true, 'browser execution does not borrow native input or hit-test claims');
  assert.equal(model.browserProfileActive, true);
  assert.equal(model.canSelectBrowserPageProfile, false);
  assert.deepEqual(model.browserObservation, { checked: null, readOnly: true });

  const domains = active.capabilities!.domains;
  const missingAction = projection('ready', {
    capabilities: {
      ...active.capabilities!,
      input: 'available',
      human_input: 'available',
      hit_test: 'available',
      domains: [
        domains[0],
        domains[1],
        { domain: 'headless_browser', state: 'unavailable', actions: [] },
        domains[3],
        domains[4],
      ],
    },
  });
  assert.equal(toViewModel(missingAction, 'idle').canExecute, false,
    'native-like flags cannot substitute for the unavailable browser action capability');
});

test('the profile picker is available only from an idle simulated session with browser action unavailable', () => {
  const base = projection(null, { capabilities: {
    identity: { session_epoch: 1, surface: { id: 'main', epoch: 1 }, clock_domain: 'sim/1/monotonic', dependencies: {} },
    backend: 'simulated', execution_domain: 'simulated', domains: simulatedOnlyDomains(),
    policy: 'atomic_only', human_input: 'available', hit_test: 'available', input: 'available', max_uninterruptible_ms: 100,
  } });
  assert.equal(toViewModel(base, 'idle').canSelectBrowserPageProfile, true);
  assert.equal(toViewModel(projection('running', { capabilities: base.capabilities }), 'idle').canSelectBrowserPageProfile, false);
});

const nativeCapabilities: NonNullable<UiProjection['capabilities']> = {
  identity: { session_epoch: 1, surface: { id: 'main', epoch: 1 }, clock_domain: 'macos/1/wall', dependencies: {} },
  backend: 'macos', execution_domain: 'foreground_native', domains: nativeUnavailableDomains(), policy: 'atomic_only', human_input: 'unavailable', hit_test: 'unavailable', input: 'unavailable', max_uninterruptible_ms: 0,
};

function observation(extra: Partial<NonNullable<UiProjection['observation']>> = {}): NonNullable<UiProjection['observation']> {
  return {
    target_app: 'com.lingxi.computer-user.m2-fixture', target_name: 'M2 Observation Fixture', active_window: 'Computer Use M2 Fixture',
    sources: ['screenshot', 'ax', 'ocr'].map((source) => ({ source: source as 'screenshot' | 'ax' | 'ocr', state: 'available', reason: null, at_ms: 10 })),
    frame: {
      stream_id: 'not-for-the-ui', frame_id: 'not-for-the-ui-either', status: 'complete', geometry_revision: 4,
      width_px: 1280, height_px: 720, pixel_age_ms: 320, confirmation_age_ms: 10, dropped_frames: 2, healthy: true,
    },
    ...extra,
  };
}

test('native observation shows source health and frame ages while keeping execution disabled', () => {
  const model = toViewModel(projection(null, { capabilities: nativeCapabilities, observation: observation() }), 'idle');
  assert.equal(model.canExecute, false);
  assert.equal(model.observation?.readOnly, true);
  assert.equal(model.observation?.level, 'ready');
  assert.equal(model.observation?.app, 'M2 Observation Fixture');
  assert.equal(model.observation?.window, 'Computer Use M2 Fixture');
  assert.equal(model.observation?.frame?.dimensions, '1280 × 720 px');
  assert.equal(model.observation?.frame?.imageAge, '320 ms');
  assert.equal(model.observation?.frame?.confirmationAge, '10 ms');
  assert.equal(model.observation?.frame?.dropped, 2);
  assert.equal(JSON.stringify(model.observation).includes('not-for-the-ui'), false, 'wire identities stay out of the console');
});

test('source failure is visible even when previous frame metadata remains', () => {
  const display = observationDisplay(projection(null, { capabilities: nativeCapabilities, observation: observation({
    sources: [
      { source: 'screenshot', state: 'unavailable', reason: 'Screen recording permission is unavailable', at_ms: 20 },
      { source: 'ax', state: 'available', reason: null, at_ms: 20 },
      { source: 'ocr', state: 'degraded', reason: 'No image is available for text recognition', at_ms: 20 },
    ],
  }) }));
  assert.equal(display?.level, 'warning');
  assert.equal(display?.title, 'Observation partly available');
  assert.equal(display?.sources[0]?.reason, 'Screen recording permission is unavailable');
  assert.equal(display?.sources[2]?.stateLabel, 'Degraded');
});

test('an old or unconfirmed frame never makes observation look ready', () => {
  const frame = observation().frame!;
  const display = observationDisplay(projection(null, { capabilities: nativeCapabilities, observation: observation({
    frame: { ...frame, healthy: false, pixel_age_ms: 61_500, confirmation_age_ms: null },
  }) }));
  assert.equal(display?.level, 'pending');
  assert.equal(display?.title, 'Waiting for a fresh image');
  assert.equal(display?.frame?.imageAge, '1 min 1 s');
  assert.equal(display?.frame?.confirmationAge, 'Unknown');
});

test('idle liveness does not disguise the older pixel age', () => {
  const frame = observation().frame!;
  const display = observationDisplay(projection(null, { capabilities: nativeCapabilities, observation: observation({
    frame: { ...frame, status: 'idle', pixel_age_ms: 5_000, confirmation_age_ms: 20 },
  }) }));
  assert.equal(display?.frame?.status, 'No screen change');
  assert.equal(display?.frame?.imageAge, '5.0 s');
  assert.equal(display?.frame?.confirmationAge, '20 ms');
});

test('native startup awaits observation without claiming a healthy source', () => {
  const display = observationDisplay(projection(null, { capabilities: nativeCapabilities }));
  assert.equal(display?.level, 'pending');
  assert.equal(display?.title, 'Waiting for observation');
  assert.equal(display?.sources.length, 0);
  assert.equal(display?.frame, null);
  assert.equal(observationDisplay(null), null);
});
