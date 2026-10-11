import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { test } from 'node:test';

import type { BackgroundWindowTarget, DecisionProposal, NativeWindowTarget, OperatorCommand, RowTableScope,
  ScopedWindowTarget, UiProjection } from '@computer-use/desktop-api';
import { NativeSession } from '../src/main/nativeSession.js';
import type { SupervisorEvents, SupervisorOptions } from '../src/main/supervisor.js';
import { nativeUnavailableDomains } from './capabilityFixtures.js';

const bundle = 'com.lingxi.computer-user.guard-fixture';
const mvpFixtureBundle = 'com.lingxi.computer-user.mvp-fixture';
const now = 10_000;
const scopedTable: RowTableScope = {
  scroll_path: [{ role: 'ax_scroll_area', label: 'Archive' }],
  column_headers: ['Archive item'],
};
function view(targetBundle = bundle, windowId = 'window-9'): UiProjection {
  return { revision: 1, mode: 'observe', root: null, pending_confirmation: null, timeline: [], stop_status: 'not_stopped', cleanup: 'not_needed',
    capabilities: { backend: 'macos', execution_domain: 'foreground_native', domains: nativeUnavailableDomains(), input: 'unavailable', human_input: 'available', hit_test: 'available', policy: 'atomic_only', max_uninterruptible_ms: 100,
      identity: { session_epoch: 1, surface: { id: 'desktop', epoch: 1 }, clock_domain: 'macos/1/wall', dependencies: {} } },
    observation: { sources: [], frame: null, target_app: targetBundle, target_name: 'Owned fixture', active_window: 'Proof',
      observed_target: {
        snapshot_id: 'snap-1', structure_sequence: 1, session_epoch: 1, app: targetBundle, pid: 42,
        window_id: windowId, window_title: 'Proof', observed_ms: 900, valid_until_ms: 2900,
        contains_private_fields: true,
      } } };
}
class Pipeline extends EventEmitter<SupervisorEvents> {
  readonly clockOriginUnixMs = 9000;
  projection: UiProjection = view();
  commands: OperatorCommand[] = [];
  stops = 0;
  closed = false;
  close = async () => {};
  operator(command: OperatorCommand) { this.commands.push(command); }
  stopAtDevice() { this.stops += 1; }
  sendDecisionProposal(_proposal: DecisionProposal) {}
  image() { return null; }
  async shutdown() { await this.close(); this.closed = true; }
}
const options: SupervisorOptions = { binaries: { host: '/host', source: '/source', device: '/device' },
  nativeSource: { helperPath: '/source-helper', bundleId: bundle, pid: 42 } };
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((done) => { resolve = done; }); return { promise, resolve }; }
async function setup(confirm: (target: NativeWindowTarget) => Promise<boolean> = async () => true, targetBundle = bundle, windowId = 'window-9',
  sourceOverrides: Partial<NonNullable<SupervisorOptions['nativeSource']>> = {},
  projectionFactory: (targetBundle: string, windowId: string) => UiProjection = view) {
  const runs: Pipeline[] = [], configurations: SupervisorOptions[] = [];
  const initialOptions = { ...options, nativeSource: { ...options.nativeSource!, bundleId: targetBundle, ...sourceOverrides } };
  const session = await NativeSession.start(initialOptions, confirm, async (config) => {
    if (runs.length) assert.equal(runs.at(-1)!.closed, true, 'old device must exit before a new scope starts');
    configurations.push(config); const run = new Pipeline(); run.projection = projectionFactory(targetBundle, windowId); runs.push(run); return run;
  }, () => now);
  return { session, runs, configurations, first: runs[0]! };
}

function scopedView(targetBundle = mvpFixtureBundle, windowId = 'window-9', update?: (value: ScopedWindowTarget) => void): UiProjection {
  const projection = view(targetBundle, windowId);
  const scoped: ScopedWindowTarget = {
    target: {
      snapshot_id: 'scoped-snap-1', structure_sequence: 5, session_epoch: 1, app: targetBundle, pid: 42,
      window_id: windowId, window_title: 'Proof', frame_points: [-22, 20, 641, 481],
      observed_ms: 900, valid_until_ms: 2900, contains_private_fields: false,
    },
    table_scope: scopedTable,
    source: {
      source_session_id: 'source-session-1', session_epoch: 1,
      process: { pid: 42, start_seconds: 1_790_000_000, start_micros: 123_456 },
      origin_window_id: windowId,
      origin_window_frame: { x: -21.5, y: 20, width: 640.5, height: 480.5 },
      verified_source_sequence: 5, verified_observed_ms: 900,
    },
    root_identity_hash: 'a'.repeat(64),
  };
  update?.(scoped);
  projection.observation!.observed_target = null;
  projection.observation!.scoped_window_target = scoped;
  return projection;
}

function backgroundTarget(targetBundle = mvpFixtureBundle, windowId = 'window-9', update?: (value: BackgroundWindowTarget) => void): BackgroundWindowTarget {
  const target: BackgroundWindowTarget = {
    snapshot_id: 'background-snap-1',
    app: targetBundle,
    pid: 42,
    window_id: windowId,
    window_title: 'Background Proof',
    frame_points: [-22, 20, 641, 481],
    structure_sequence: 5,
    session_epoch: 1,
    observed_ms: 900,
    valid_until_ms: 2_900,
    source: {
      window: {
        source_session_id: 'background-source-session-1',
        session_epoch: 1,
        process: { pid: 42, start_seconds: 1_790_000_000, start_micros: 123_456 },
        origin_window_id: windowId,
        origin_window_frame: { x: -21.5, y: 20, width: 640.5, height: 480.5 },
        verified_source_sequence: 5,
        verified_observed_ms: 900,
      },
      hidden: { state: 'known', value: false },
      minimized: { state: 'known', value: false },
      target_context_hash: `tc1:${'a'.repeat(64)}`,
    },
  };
  update?.(target);
  return target;
}

function backgroundView(targetBundle = mvpFixtureBundle, windowId = 'window-9', update?: (value: BackgroundWindowTarget) => void): UiProjection {
  const projection = view(targetBundle, windowId);
  projection.observation!.background_window_target = backgroundTarget(targetBundle, windowId, update);
  return projection;
}

async function setupBackground(confirm: (target: NativeWindowTarget, domain?: string) => Promise<boolean> = async () => true,
  update?: (value: BackgroundWindowTarget) => void) {
  return setup(confirm, mvpFixtureBundle, 'window-9', { backgroundWindowId: 'window-9' },
    (targetBundle, windowId) => backgroundView(targetBundle, windowId, update));
}

async function setupScoped(confirm: (target: NativeWindowTarget) => Promise<boolean> = async () => true) {
  const f = await setup(confirm, mvpFixtureBundle, 'window-9', { sourceReadWindowId: 'window-9', tableScope: scopedTable });
  f.first.projection = scopedView();
  return f;
}

test('startup permission is discarded; renderer mode cannot enable native input', async () => {
  let config: SupervisorOptions | undefined;
  const session = await NativeSession.start({ ...options, mode: 'execute', nativeSource: { ...options.nativeSource!, authorizedWindowId: 'window-9',
    authorizedProcessIdentity: { pid: 42, start_seconds: 1_790_000_000, start_micros: 123_456 } } }, async () => true,
    async (value) => { config = value; return new Pipeline(); }, () => now);
  assert.equal(config?.mode, 'observe'); assert.equal(config?.nativeSource?.authorizedWindowId, undefined);
  assert.equal(config?.nativeSource?.authorizedProcessIdentity, undefined);
  assert.throws(() => session.operator({ command: 'set_mode', mode: 'execute' }), /Authorize/);
  await session.shutdown();
});

test('startup rejects conflicting Source-read and preexisting authorized windows before discarding consent', async () => {
  let starts = 0;
  await assert.rejects(NativeSession.start({ ...options, nativeSource: {
    ...options.nativeSource!, sourceReadWindowId: 'window-9', authorizedWindowId: 'window-10',
  } }, async () => true, async () => { starts += 1; return new Pipeline(); }, () => now), /must match/);
  assert.equal(starts, 0);
});

test('selecting the fixed browser profile replaces and cancels the prior Root in a fresh observing session', async () => {
  const runs: Pipeline[] = [];
  const configurations: SupervisorOptions[] = [];
  const session = await NativeSession.start({ binaries: options.binaries }, async () => true,
    async (config) => {
      if (runs.length) assert.equal(runs.at(-1)!.closed, true, 'the previous process tree closes before browser startup');
      configurations.push(config);
      const next = new Pipeline();
      runs.push(next);
      return next;
    }, () => now);
  runs[0]!.projection.root = {
    root_task_id: 'old-root', task_revision: 1, root_epoch: 1,
    state: 'running', headline: 'old work', attempts_used: 1, attempts_max: 3,
  };

  await session.startBrowserProfile({
    browserPageProfile: { kind: 'fixture_background_option' },
    browserSource: { chromeExecutable: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
      fixturePath: '/checkout/fixtures/headless/readonly-page.html' },
  });

  assert.equal(runs.length, 2);
  assert.ok(runs[0]!.stops > 0, 'the old Device receives a direct stop before replacement');
  assert.ok(runs[0]!.commands.some((command) => command.command === 'stop_task' && command.root_task_id === null),
    'the old Root is cancelled rather than resumed under the browser profile');
  assert.equal(configurations[1]!.mode, 'observe');
  assert.deepEqual(configurations[1]!.browserPageProfile, { kind: 'fixture_background_option' });
  assert.deepEqual(configurations[1]!.browserSource, {
    chromeExecutable: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    fixturePath: '/checkout/fixtures/headless/readonly-page.html',
  });
  assert.equal(session.authorization.phase, 'not_native');
  assert.equal(session.ownedDocumentStatus.can_create, false, 'owned-document startup is closed in browser mode');
  assert.equal(session.canConfigureModel, false, 'model authorization is closed in browser mode');
  await session.shutdown();
});

test('a fixed browser profile cannot be mixed into a native session', async () => {
  const f = await setup();
  await assert.rejects(f.session.startBrowserProfile({
    browserPageProfile: { kind: 'fixture_background_option' },
    browserSource: { chromeExecutable: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
      fixturePath: '/checkout/fixtures/headless/readonly-page.html' },
  }), /cannot be mixed/);
  assert.equal(f.runs.length, 1);
  assert.equal(f.first.closed, false);
  await f.session.shutdown();
});

test('background read metadata never becomes foreground consent and startup clears all input grants', async () => {
  const f = await setup(async () => true, mvpFixtureBundle, 'window-9', {
    backgroundWindowId: 'window-9',
    authorizedBackgroundWindowId: 'window-9',
    authorizedProcessIdentity: { pid: 42, start_seconds: 1_790_000_000, start_micros: 123_456 },
  });
  assert.equal(f.configurations[0]!.mode, 'observe');
  assert.equal(f.configurations[0]!.nativeSource?.backgroundWindowId, 'window-9');
  assert.equal(f.configurations[0]!.nativeSource?.authorizedWindowId, undefined);
  assert.equal(f.configurations[0]!.nativeSource?.authorizedBackgroundWindowId, undefined);
  assert.equal(f.configurations[0]!.nativeSource?.authorizedProcessIdentity, undefined);
  assert.equal(f.session.authorization.candidate, null, 'foreground observed_target cannot authorize the background domain');
  assert.equal(f.session.authorization.candidate_domain, null);
  assert.equal(f.session.authorization.can_authorize, false);
  await f.session.shutdown();
});

test('BackgroundAx candidate uses only its fresh TargetContext window and fixed Source birth identity', async () => {
  const f = await setupBackground();
  const candidate = f.session.authorization.candidate;
  assert.equal(f.session.authorization.candidate_domain, 'background_ax');
  assert.deepEqual(candidate && {
    snapshot_id: candidate.snapshot_id, app: candidate.app, pid: candidate.pid,
    window_id: candidate.window_id, window_title: candidate.window_title, frame_points: candidate.frame_points,
  }, {
    snapshot_id: 'background-snap-1', app: mvpFixtureBundle, pid: 42,
    window_id: 'window-9', window_title: 'Background Proof', frame_points: [-22, 20, 641, 481],
  });
  f.first.projection.observation!.active_window = 'Different foreground window';
  f.first.projection.observation!.observed_target!.app = 'foreign.foreground';
  assert.equal(f.session.authorization.candidate_domain, 'background_ax',
    'GlobalFocus and observed_target do not select or replace the logical background target');

  const invalid: Array<[string, (value: BackgroundWindowTarget) => void]> = [
    ['stale', (value) => { value.valid_until_ms = 1_000; }],
    ['future', (value) => { value.observed_ms = 1_001; value.source.window.verified_observed_ms = 1_001; }],
    ['long TTL', (value) => { value.valid_until_ms = value.observed_ms + 2_001; }],
    ['epoch', (value) => { value.session_epoch = 2; }],
    ['PID', (value) => { value.pid = 43; value.source.window.process.pid = 43; }],
    ['window', (value) => { value.window_id = 'window-10'; value.source.window.origin_window_id = 'window-10'; }],
    ['hash', (value) => { value.source.target_context_hash = `TC1:${'a'.repeat(64)}`; }],
    ['unsafe kernel seconds', (value) => { value.source.window.process.start_seconds = Number.MAX_SAFE_INTEGER + 1; }],
    ['invalid kernel micros', (value) => { value.source.window.process.start_micros = 1_000_000; }],
    ['title', (value) => { value.window_title = '  '; }],
    ['frame', (value) => { value.frame_points[0] = -21; }],
    ['source session', (value) => { value.source.window.source_session_id = ''; }],
    ['sequence provenance', (value) => { value.source.window.verified_source_sequence = 4; }],
    ['time provenance', (value) => { value.source.window.verified_observed_ms = 899; }],
    ['hidden', (value) => { value.source.hidden = { state: 'unavailable' }; }],
    ['minimized', (value) => { value.source.minimized = { state: 'known', value: true }; }],
  ];
  for (const [label, update] of invalid) {
    const stale = await setupBackground(async () => true, update);
    assert.equal(stale.session.authorization.candidate, null, `${label} background context must fail closed`);
    assert.equal(stale.session.authorization.candidate_domain, null);
    await assert.rejects(stale.session.authorizeNativeWindow('background-snap-1'), /stale/);
    await stale.session.shutdown();
  }
  await f.session.shutdown();
});

test('fresh monotonic BackgroundAx evidence opens only after Main confirmation and pins its exact birth stamp', async () => {
  const approval = deferred<boolean>();
  const confirmations: Array<{ window_id: string; domain?: string }> = [];
  const f = await setupBackground(async (target, domain) => {
    confirmations.push({ window_id: target.window_id, domain });
    return approval.promise;
  });
  const pending = f.session.authorizeNativeWindow('background-snap-1');
  f.first.projection = backgroundView(mvpFixtureBundle, 'window-9', (value) => {
    value.snapshot_id = 'background-snap-2';
    value.structure_sequence = 6;
    value.observed_ms = 901;
    value.valid_until_ms = 2_901;
    value.source.window.verified_source_sequence = 6;
    value.source.window.verified_observed_ms = 901;
  });
  approval.resolve(true);
  await pending;

  assert.deepEqual(confirmations, [{ window_id: 'window-9', domain: 'background_ax' }]);
  assert.equal(f.runs.length, 2);
  assert.equal(f.configurations[1]!.mode, 'observe');
  const authorized = f.configurations[1]!.nativeSource!;
  assert.equal(authorized.backgroundWindowId, 'window-9');
  assert.equal(authorized.authorizedWindowId, undefined, 'background consent must not open foreground AXPress');
  assert.equal(authorized.authorizedBackgroundWindowId, 'window-9');
  assert.deepEqual(authorized.authorizedProcessIdentity, {
    pid: 42, start_seconds: 1_790_000_000, start_micros: 123_456,
  });
  assert.equal(f.session.authorization.phase, 'authorized');
  assert.equal(f.session.authorization.authorized_domain, 'background_ax');

  await f.session.revokeNativeWindow();
  assert.equal(f.configurations[2]!.nativeSource?.backgroundWindowId, 'window-9');
  assert.equal(f.configurations[2]!.nativeSource?.authorizedWindowId, undefined);
  assert.equal(f.configurations[2]!.nativeSource?.authorizedBackgroundWindowId, undefined);
  assert.equal(f.configurations[2]!.nativeSource?.authorizedProcessIdentity, undefined);
  assert.equal(f.session.authorization.authorized_domain, null);
  await f.session.shutdown();
});

test('BackgroundAx confirmation rejects changes to process, Source session, context hash, title, frame, epoch and monotonic provenance', async () => {
  const changes: Array<[string, (value: BackgroundWindowTarget, projection: UiProjection) => void]> = [
    ['Source session', (value) => { value.source.window.source_session_id = 'background-source-session-2'; value.source.target_context_hash = `tc1:${'b'.repeat(64)}`; }],
    ['kernel start stamp', (value) => { value.source.window.process.start_micros += 1; value.source.target_context_hash = `tc1:${'b'.repeat(64)}`; }],
    ['target-context hash', (value) => { value.source.target_context_hash = `tc1:${'b'.repeat(64)}`; }],
    ['window title', (value) => { value.window_title = 'Renamed'; }],
    ['frame', (value) => {
      value.source.window.origin_window_frame.x = -20.5;
      value.frame_points[0] = -21;
      value.source.target_context_hash = `tc1:${'b'.repeat(64)}`;
    }],
    ['window identity', (value) => { value.window_id = 'window-10'; value.source.window.origin_window_id = 'window-10'; }],
    ['PID', (value) => { value.pid = 43; value.source.window.process.pid = 43; }],
    ['session epoch', (value, projection) => {
      value.session_epoch = 2; value.source.window.session_epoch = 2;
      projection.capabilities!.identity.session_epoch = 2;
      value.source.target_context_hash = `tc1:${'b'.repeat(64)}`;
    }],
    ['source sequence regression', (value) => { value.structure_sequence = 4; value.source.window.verified_source_sequence = 4; }],
    ['source time regression', (value) => {
      value.observed_ms = 899; value.valid_until_ms = 2_899;
      value.source.window.verified_observed_ms = 899;
    }],
  ];
  for (const [label, update] of changes) {
    const approval = deferred<boolean>();
    const f = await setupBackground(() => approval.promise);
    const pending = f.session.authorizeNativeWindow('background-snap-1');
    f.first.projection = backgroundView(mvpFixtureBundle, 'window-9', (value) => update(value, f.first.projection));
    approval.resolve(true);
    await assert.rejects(pending, /changed/ , `${label} must invalidate the background consent anchor`);
    assert.equal(f.runs.length, 1, `${label} must not start a BackgroundAx Device session`);
    assert.equal(f.session.authorization.authorized, null);
    assert.equal(f.session.authorization.authorized_domain, null);
    await f.session.shutdown();
  }
});

test('BackgroundAx cancel, active Root and uncertain cleanup never open an input session', async () => {
  const declined = await setupBackground(async () => false);
  await declined.session.authorizeNativeWindow('background-snap-1');
  assert.equal(declined.runs.length, 1);
  assert.equal(declined.session.authorization.phase, 'observing');
  assert.equal(declined.session.authorization.authorized_domain, null);
  await declined.session.shutdown();

  const busy = await setupBackground();
  busy.first.projection.root = { root_task_id: 'busy-root', root_epoch: 1, task_revision: 1,
    state: 'ready', headline: '', attempts_used: 0, attempts_max: 1 };
  assert.equal(busy.session.authorization.can_authorize, false);
  await assert.rejects(busy.session.authorizeNativeWindow('background-snap-1'), /busy/);
  assert.equal(busy.runs.length, 1);
  assert.equal(busy.first.stops, 0);
  await busy.session.shutdown();

  const cleanup = await setupBackground();
  cleanup.first.projection.cleanup = 'unknown';
  assert.equal(cleanup.session.authorization.can_authorize, false);
  await assert.rejects(cleanup.session.authorizeNativeWindow('background-snap-1'), /busy/);
  assert.equal(cleanup.runs.length, 1);
  await cleanup.session.shutdown();

  const cleanupDuringClose = await setupBackground();
  cleanupDuringClose.first.close = async () => { cleanupDuringClose.first.projection.cleanup = 'unknown'; };
  await assert.rejects(cleanupDuringClose.session.authorizeNativeWindow('background-snap-1'), /cleanup is unconfirmed/);
  assert.equal(cleanupDuringClose.runs.length, 1);
  assert.equal(cleanupDuringClose.session.authorization.authorized_domain, null);
  await cleanupDuringClose.session.shutdown();
});

test('Refresh observation restores a lost BackgroundAx read lease without carrying its input grant', async () => {
  const lost = await setupBackground();
  lost.first.projection.observation!.background_window_target = null;
  assert.equal(lost.session.authorization.candidate, null);
  assert.equal(lost.session.authorization.can_refresh_observation, true);
  await lost.session.refreshNativeObservation();
  assert.equal(lost.runs.length, 2);
  assert.equal(lost.first.stops, 0, 'idle read-only refresh does not dispatch Device stop');
  assert.equal(lost.configurations[1]!.mode, 'observe');
  assert.equal(lost.configurations[1]!.nativeSource?.backgroundWindowId, 'window-9');
  assert.equal(lost.configurations[1]!.nativeSource?.authorizedWindowId, undefined);
  assert.equal(lost.configurations[1]!.nativeSource?.authorizedBackgroundWindowId, undefined);
  assert.equal(lost.configurations[1]!.nativeSource?.authorizedProcessIdentity, undefined);
  await lost.session.shutdown();

  const unavailable = await setupBackground();
  unavailable.first.emit('exit', 'source', 1, null);
  assert.equal(unavailable.session.authorization.phase, 'unavailable');
  assert.equal(unavailable.session.authorization.can_refresh_observation, true);
  await unavailable.session.refreshNativeObservation();
  assert.equal(unavailable.runs.length, 2);
  assert.equal(unavailable.configurations[1]!.nativeSource?.backgroundWindowId, 'window-9');
  assert.equal(unavailable.configurations[1]!.nativeSource?.authorizedBackgroundWindowId, undefined);
  assert.equal(unavailable.configurations[1]!.nativeSource?.authorizedProcessIdentity, undefined);
  await unavailable.session.shutdown();

  const busy = await setupBackground();
  busy.first.projection.observation!.background_window_target = null;
  busy.first.projection.root = { root_task_id: 'busy-root', root_epoch: 1, task_revision: 1,
    state: 'ready', headline: '', attempts_used: 0, attempts_max: 1 };
  assert.equal(busy.session.authorization.can_refresh_observation, false);
  await assert.rejects(busy.session.refreshNativeObservation(), /cannot be refreshed/);
  assert.equal(busy.runs.length, 1);
  assert.equal(busy.first.stops, 0);
  await busy.session.shutdown();

  const uncertain = await setupBackground();
  uncertain.first.projection.observation!.background_window_target = null;
  uncertain.first.projection.cleanup = 'unknown';
  assert.equal(uncertain.session.authorization.can_refresh_observation, false);
  await assert.rejects(uncertain.session.refreshNativeObservation(), /cannot be refreshed/);
  assert.equal(uncertain.runs.length, 1);
  await uncertain.session.shutdown();
});

test('only current registered metadata can open the native consent dialog', async () => {
  let prompts = 0;
  const f = await setup(async () => { prompts += 1; return true; });
  assert.equal(f.session.authorization.candidate_domain, 'foreground_native');
  await assert.rejects(f.session.authorizeNativeWindow('forged'), /stale/);
  f.first.projection.observation!.observed_target!.valid_until_ms = 1000;
  await assert.rejects(f.session.authorizeNativeWindow('snap-1'), /stale/);
  f.first.projection = view(); f.first.projection.observation!.observed_target!.app = 'unknown.app';
  await assert.rejects(f.session.authorizeNativeWindow('snap-1'), /stale/);
  assert.equal(prompts, 0); assert.equal(f.runs.length, 1); await f.session.shutdown();
});

test('scoped consent candidate requires its exact fixed Source policy and complete current anchor', async () => {
  const f = await setupScoped();
  assert.deepEqual(f.session.authorization.candidate && {
    snapshot_id: f.session.authorization.candidate.snapshot_id,
    app: f.session.authorization.candidate.app,
    pid: f.session.authorization.candidate.pid,
    window_id: f.session.authorization.candidate.window_id,
    window_title: f.session.authorization.candidate.window_title,
    frame_points: f.session.authorization.candidate.frame_points,
  }, {
    snapshot_id: 'scoped-snap-1', app: mvpFixtureBundle, pid: 42, window_id: 'window-9',
    window_title: 'Proof', frame_points: [-22, 20, 641, 481],
  });
  const badEvidence: Array<[string, (value: ScopedWindowTarget) => void]> = [
    ['stale', (value) => { value.target.valid_until_ms = 1000; }],
    ['future', (value) => { value.target.observed_ms = 1001; value.source.verified_observed_ms = 1001; }],
    ['epoch', (value) => { value.target.session_epoch = 2; }],
    ['PID', (value) => { value.target.pid = 43; value.source.process.pid = 43; }],
    ['window', (value) => { value.target.window_id = 'window-10'; value.source.origin_window_id = 'window-10'; }],
    ['scope', (value) => { value.table_scope = { ...scopedTable, column_headers: ['Other'] }; }],
    ['hash', (value) => { value.root_identity_hash = 'A'.repeat(64); }],
    ['kernel PID', (value) => { value.source.process.pid = 43; }],
    ['unsafe kernel seconds', (value) => { value.source.process.start_seconds = Number.MAX_SAFE_INTEGER + 1; }],
    ['invalid kernel micros', (value) => { value.source.process.start_micros = 1_000_000; }],
    ['title', (value) => { value.target.window_title = null; }],
    ['frame', (value) => { value.target.frame_points = [-21, 20, 641, 481]; }],
    ['source session', (value) => { value.source.source_session_id = ''; }],
    ['provenance sequence', (value) => { value.source.verified_source_sequence = 4; }],
  ];
  for (const [label, update] of badEvidence) {
    const stale = await setupScoped();
    stale.first.projection = scopedView(mvpFixtureBundle, 'window-9', update);
    assert.equal(stale.session.authorization.candidate, null, `${label} evidence must not open consent`);
    await assert.rejects(stale.session.authorizeNativeWindow('scoped-snap-1'), /stale/);
    await stale.session.shutdown();
  }
  await f.session.shutdown();
});

test('a fresh same-root Source read can approve only after the real confirm callback and preserves read-only scope', async () => {
  const approval = deferred<boolean>();
  const f = await setupScoped(() => approval.promise);
  const pending = f.session.authorizeNativeWindow('scoped-snap-1');
  assert.throws(() => f.session.operator({ command: 'create_task', text: 'new' }), /changing/);
  f.first.projection = scopedView(mvpFixtureBundle, 'window-9', (value) => {
    value.target.snapshot_id = 'scoped-snap-2';
    value.target.structure_sequence = 6;
    value.target.observed_ms = 901;
    value.target.valid_until_ms = 2901;
    value.source.verified_source_sequence = 6;
    value.source.verified_observed_ms = 901;
  });
  approval.resolve(true);
  await pending;
  assert.equal(f.runs.length, 2);
  assert.equal(f.configurations[1]!.mode, 'observe');
  assert.deepEqual(f.configurations[1]!.nativeSource, {
    helperPath: '/source-helper', bundleId: mvpFixtureBundle, pid: 42,
    sourceReadWindowId: 'window-9', tableScope: scopedTable, authorizedWindowId: 'window-9',
    authorizedProcessIdentity: { pid: 42, start_seconds: 1_790_000_000, start_micros: 123_456 },
  });
  assert.equal(f.session.authorization.phase, 'authorized');
  assert.equal(f.session.authorization.authorized_domain, 'foreground_native');
  await f.session.revokeNativeWindow();
  assert.equal(f.session.authorization.authorized_domain, null);
  assert.equal(f.configurations[2]!.nativeSource?.authorizedWindowId, undefined);
  assert.equal(f.configurations[2]!.nativeSource?.authorizedProcessIdentity, undefined);
  await f.session.shutdown();
});

test('scoped consent rechecks the root, source session, process start, title, frame, scope and monotonic source anchor', async () => {
  const changes: Array<[string, (value: ScopedWindowTarget) => void]> = [
    ['root identity', (value) => { value.root_identity_hash = 'b'.repeat(64); }],
    ['Source session', (value) => { value.source.source_session_id = 'source-session-2'; }],
    ['kernel start stamp', (value) => { value.source.process.start_micros += 1; }],
    ['window title', (value) => { value.target.window_title = 'Renamed'; }],
    ['frame', (value) => {
      value.source.origin_window_frame.x = -21.6;
    }],
    ['table scope', (value) => { value.table_scope = { ...scopedTable, column_headers: ['Other'] }; }],
    ['source sequence regression', (value) => {
      value.target.structure_sequence = 4; value.source.verified_source_sequence = 4;
    }],
    ['source time regression', (value) => {
      value.target.observed_ms = 899; value.target.valid_until_ms = 2899; value.source.verified_observed_ms = 899;
    }],
    ['window identity', (value) => { value.target.window_id = 'window-10'; value.source.origin_window_id = 'window-10'; }],
    ['process PID', (value) => { value.target.pid = 43; value.source.process.pid = 43; }],
    ['session epoch', (value) => { value.target.session_epoch = 2; value.source.session_epoch = 2; }],
  ];
  for (const [label, update] of changes) {
    const approval = deferred<boolean>();
    const f = await setupScoped(() => approval.promise);
    const pending = f.session.authorizeNativeWindow('scoped-snap-1');
    f.first.projection = scopedView(mvpFixtureBundle, 'window-9', update);
    approval.resolve(true);
    await assert.rejects(pending, /changed/, `${label} must invalidate the consent anchor`);
    assert.equal(f.runs.length, 1, `${label} must not start an input session`);
    assert.equal(f.session.authorization.authorized, null);
    await f.session.shutdown();
  }
});

test('scoped cancellation, a busy root, or unknown cleanup never grants input', async () => {
  const declined = await setupScoped(async () => false);
  await declined.session.authorizeNativeWindow('scoped-snap-1');
  assert.equal(declined.runs.length, 1);
  assert.equal(declined.session.authorization.phase, 'observing');
  await declined.session.shutdown();

  for (const state of ['root', 'cleanup'] as const) {
    const approval = deferred<boolean>();
    const f = await setupScoped(() => approval.promise);
    const pending = f.session.authorizeNativeWindow('scoped-snap-1');
    if (state === 'root') f.first.projection.root = {
      root_task_id: 'busy-root', root_epoch: 1, task_revision: 1, state: 'ready', headline: '', attempts_used: 0, attempts_max: 1,
    };
    else f.first.projection.cleanup = 'unknown';
    approval.resolve(true);
    await assert.rejects(pending, state === 'root' ? /busy/ : /cleanup is unconfirmed/);
    assert.equal(f.runs.length, 1);
    assert.equal(f.session.authorization.authorized, null);
    await f.session.shutdown();
  }

  const approval = deferred<boolean>();
  const stopped = await setupScoped(() => approval.promise);
  const pending = stopped.session.authorizeNativeWindow('scoped-snap-1');
  stopped.session.stopAtDevice({ scope: 'all' }, 'cancel', 'global_hotkey');
  approval.resolve(true);
  await assert.rejects(pending, /cancelled/);
  assert.equal(stopped.runs.length, 1);
  assert.equal(stopped.session.authorization.authorized, null);
  await stopped.session.shutdown();

  const shutdownApproval = deferred<boolean>();
  const shuttingDown = await setupScoped(() => shutdownApproval.promise);
  const authorization = shuttingDown.session.authorizeNativeWindow('scoped-snap-1');
  const shutdown = shuttingDown.session.shutdown();
  shutdownApproval.resolve(true);
  await assert.rejects(authorization, /cancelled/);
  await shutdown;
  assert.equal(shuttingDown.runs.length, 1);

  const cleanup = await setupScoped();
  cleanup.first.close = async () => { cleanup.first.projection.cleanup = 'unknown'; };
  await assert.rejects(cleanup.session.authorizeNativeWindow('scoped-snap-1'), /cleanup is unconfirmed/);
  assert.equal(cleanup.runs.length, 1, 'unknown cleanup during replacement must prevent authorized startup');
  assert.equal(cleanup.session.authorization.authorized, null);
  await cleanup.session.shutdown();

  const expiry = await setupScoped();
  expiry.first.close = async () => {
    expiry.first.projection = scopedView(mvpFixtureBundle, 'window-9', (value) => { value.target.valid_until_ms = 1000; });
  };
  await assert.rejects(expiry.session.authorizeNativeWindow('scoped-snap-1'), /changed or expired/);
  assert.equal(expiry.runs.length, 1, 'expired evidence during old-process cleanup must prevent authorized startup');
  await expiry.session.shutdown();

  const busyDuringClose = await setupScoped();
  busyDuringClose.first.close = async () => {
    busyDuringClose.first.projection.root = {
      root_task_id: 'busy-root', root_epoch: 1, task_revision: 1, state: 'ready', headline: '', attempts_used: 0, attempts_max: 1,
    };
  };
  await assert.rejects(busyDuringClose.session.authorizeNativeWindow('scoped-snap-1'), /became busy/);
  assert.equal(busyDuringClose.runs.length, 1);
  await busyDuringClose.session.shutdown();
});

test('refresh observation restores the fixed read-only Source lease after its scoped census is cleared', async () => {
  const f = await setupScoped();
  f.first.projection.observation!.scoped_window_target = null;
  assert.equal(f.session.authorization.candidate, null, 'cleared scoped evidence must not be reused as consent');
  assert.equal(f.session.authorization.can_refresh_observation, true);

  await f.session.refreshNativeObservation();

  assert.equal(f.runs.length, 2);
  assert.equal(f.first.closed, true);
  assert.equal(f.first.stops, 0, 'refresh must not dispatch a task stop');
  assert.equal(f.configurations[1]!.mode, 'observe');
  assert.deepEqual(f.configurations[1]!.nativeSource, {
    helperPath: '/source-helper', bundleId: mvpFixtureBundle, pid: 42,
    sourceReadWindowId: 'window-9', tableScope: scopedTable,
    authorizedWindowId: undefined, authorizedProcessIdentity: undefined,
  });
  assert.equal(f.session.authorization.phase, 'observing');
  assert.equal(f.session.authorization.authorized, null);
  assert.equal(f.session.authorization.can_authorize, false, 'the new generic projection carries no scoped candidate');
  await f.session.shutdown();
});

test('refresh observation recovers an unavailable Source process with no input-window or process-birth grant', async () => {
  const f = await setupScoped();
  f.first.emit('exit', 'source', 1, null);
  assert.equal(f.session.authorization.phase, 'unavailable');
  assert.equal(f.session.authorization.can_refresh_observation, true);

  await f.session.refreshNativeObservation();

  assert.equal(f.runs.length, 2);
  assert.equal(f.configurations[1]!.mode, 'observe');
  assert.equal(f.configurations[1]!.nativeSource?.sourceReadWindowId, 'window-9');
  assert.deepEqual(f.configurations[1]!.nativeSource?.tableScope, scopedTable);
  assert.equal(f.configurations[1]!.nativeSource?.authorizedWindowId, undefined);
  assert.equal(f.configurations[1]!.nativeSource?.authorizedProcessIdentity, undefined);
  assert.equal(f.session.authorization.phase, 'observing');
  assert.equal(f.session.authorization.authorized, null);
  await f.session.shutdown();
});

test('refresh refuses active roots, uncertain cleanup and model configuration locks without stopping or replacing', async () => {
  const active = await setupScoped();
  active.first.projection.root = { root_task_id: 'busy-root', root_epoch: 1, task_revision: 1,
    state: 'ready', headline: '', attempts_used: 0, attempts_max: 1 };
  assert.equal(active.session.authorization.can_refresh_observation, false);
  await assert.rejects(active.session.refreshNativeObservation(), /cannot be refreshed/);
  assert.equal(active.runs.length, 1);
  assert.equal(active.first.stops, 0, 'refresh refusal must not send a device stop to an active root');
  assert.equal(active.first.closed, false);
  await active.session.shutdown();

  const cleanup = await setupScoped();
  cleanup.first.projection.cleanup = 'unknown';
  assert.equal(cleanup.session.authorization.can_refresh_observation, false);
  await assert.rejects(cleanup.session.refreshNativeObservation(), /cannot be refreshed/);
  assert.equal(cleanup.runs.length, 1);
  assert.equal(cleanup.first.stops, 0);
  await cleanup.session.shutdown();

  const locked = await setupScoped();
  const release = locked.session.lockConfiguration();
  assert.equal(locked.session.authorization.can_refresh_observation, false);
  await assert.rejects(locked.session.refreshNativeObservation(), /cannot be refreshed/);
  assert.equal(locked.runs.length, 1);
  release();
  await locked.session.shutdown();
});

test('refresh rechecks Root and cleanup after closing the old Source session', async () => {
  const busyDuringClose = await setupScoped();
  busyDuringClose.first.projection.observation!.scoped_window_target = null;
  busyDuringClose.first.close = async () => {
    busyDuringClose.first.projection.root = { root_task_id: 'late-root', root_epoch: 1, task_revision: 1,
      state: 'ready', headline: '', attempts_used: 0, attempts_max: 1 };
  };
  await assert.rejects(busyDuringClose.session.refreshNativeObservation(), /busy or cleanup/);
  assert.equal(busyDuringClose.runs.length, 1);
  assert.equal(busyDuringClose.first.stops, 0);
  await busyDuringClose.session.shutdown();

  const cleanupDuringClose = await setupScoped();
  cleanupDuringClose.first.projection.observation!.scoped_window_target = null;
  cleanupDuringClose.first.close = async () => { cleanupDuringClose.first.projection.cleanup = 'unknown'; };
  await assert.rejects(cleanupDuringClose.session.refreshNativeObservation(), /busy or cleanup/);
  assert.equal(cleanupDuringClose.runs.length, 1);
  await cleanupDuringClose.session.shutdown();
});

test('a concurrent refresh is rejected while the previous session replacement is changing', async () => {
  const f = await setupScoped();
  f.first.projection.observation!.scoped_window_target = null;
  const close = deferred<void>();
  f.first.close = () => close.promise;
  const refresh = f.session.refreshNativeObservation();
  assert.equal(f.session.authorization.phase, 'switching');
  await assert.rejects(f.session.refreshNativeObservation(), /cannot be refreshed/);
  close.resolve();
  await refresh;
  assert.equal(f.runs.length, 2);
  await f.session.shutdown();
});

test('refresh refuses a consent transition and a closing session', async () => {
  const approval = deferred<boolean>();
  const authorizing = await setupScoped(() => approval.promise);
  const authorization = authorizing.session.authorizeNativeWindow('scoped-snap-1');
  assert.equal(authorizing.session.authorization.can_refresh_observation, false);
  await assert.rejects(authorizing.session.refreshNativeObservation(), /cannot be refreshed/);
  approval.resolve(false);
  await authorization;
  assert.equal(authorizing.runs.length, 1);
  await authorizing.session.shutdown();

  const closing = await setupScoped();
  const close = deferred<void>();
  closing.first.close = () => close.promise;
  const shutdown = closing.session.shutdown();
  assert.equal(closing.session.authorization.can_refresh_observation, false);
  await assert.rejects(closing.session.refreshNativeObservation(), /cannot be refreshed/);
  close.resolve();
  await shutdown;
  assert.equal(closing.runs.length, 1);
});

test('an active task or unknown cleanup forbids replacing its input scope', async () => {
  const f = await setup();
  f.first.projection.root = { root_task_id: 'r', root_epoch: 1, task_revision: 1, state: 'ready', headline: '', attempts_used: 0, attempts_max: 1 };
  await assert.rejects(f.session.authorizeNativeWindow('snap-1'), /busy/);
  f.first.projection.root = null; f.first.projection.cleanup = 'unknown';
  await assert.rejects(f.session.authorizeNativeWindow('snap-1'), /busy/);
  assert.equal(f.runs.length, 1); await f.session.shutdown();
});

test('replacement cannot erase held, unknown or failed cleanup from the old device', async () => {
  for (const cleanup of ['held', 'unknown', 'failed'] as const) {
    const f = await setup();
    f.first.projection.cleanup = cleanup;
    assert.equal(f.session.canConfigureModel, false);
    let projected: UiProjection | undefined;
    f.session.on('projection', (value) => { projected = value; });
    await f.session.revokeNativeWindow();
    assert.equal(f.runs[1]!.projection.cleanup, 'not_needed');
    assert.equal(f.session.projection?.cleanup, 'unknown'); assert.equal(projected?.cleanup, 'unknown');
    assert.equal(f.session.authorization.can_authorize, false); assert.equal(f.session.canConfigureModel, false);
    assert.throws(() => f.session.operator({ command: 'set_mode', mode: 'execute' }), /cleanup is unconfirmed/);
    await f.session.shutdown();
  }
});

test('declining permission leaves the same observation session and never starts input', async () => {
  const f = await setup(async () => false);
  await f.session.authorizeNativeWindow('snap-1');
  assert.equal(f.session.authorization.phase, 'observing'); assert.equal(f.runs.length, 1);
  await f.session.shutdown();
});

test('fresh metadata for the same window can confirm; the new session is fixed and starts observing', async () => {
  const approval = deferred<boolean>(); const f = await setup(() => approval.promise);
  const pending = f.session.authorizeNativeWindow('snap-1');
  assert.throws(() => f.session.operator({ command: 'create_task', text: 'new' }), /changing/);
  f.first.projection.observation!.observed_target!.snapshot_id = 'snap-2';
  approval.resolve(true); await pending;
  assert.equal(f.session.authorization.phase, 'authorized'); assert.equal(f.configurations[1]!.nativeSource?.authorizedWindowId, 'window-9');
  assert.equal(f.configurations[1]!.mode, 'observe'); assert.equal(f.first.closed, true);
  await f.session.revokeNativeWindow();
  assert.equal(f.configurations[2]!.nativeSource?.authorizedWindowId, undefined); assert.equal(f.session.authorization.phase, 'observing');
  await f.session.shutdown();
});

test('the explicitly approved MVP fixture produces only its exact fixed native window scope', async () => {
  let prompted: { app: string; pid: number; window_id: string; snapshot_id: string } | undefined;
  const f = await setup(async (target: NativeWindowTarget) => {
    prompted = { app: target.app, pid: target.pid, window_id: target.window_id, snapshot_id: target.snapshot_id };
    return true;
  }, mvpFixtureBundle, 'window-9');
  assert.deepEqual(f.session.authorization.candidate && {
    app: f.session.authorization.candidate.app,
    pid: f.session.authorization.candidate.pid,
    window_id: f.session.authorization.candidate.window_id,
    snapshot_id: f.session.authorization.candidate.snapshot_id,
  }, { app: mvpFixtureBundle, pid: 42, window_id: 'window-9', snapshot_id: 'snap-1' });

  await f.session.authorizeNativeWindow('snap-1');
  assert.deepEqual(prompted, { app: mvpFixtureBundle, pid: 42, window_id: 'window-9', snapshot_id: 'snap-1' });
  assert.equal(f.configurations.length, 2);
  assert.equal(f.configurations[1]!.mode, 'observe');
  assert.deepEqual(f.configurations[1]!.nativeSource, {
    helperPath: '/source-helper', bundleId: mvpFixtureBundle, pid: 42, authorizedWindowId: 'window-9',
  });
  assert.equal(f.session.authorization.authorized?.app, mvpFixtureBundle);
  await f.session.shutdown();
});

test('MVP authorization still rejects foreign or invalid window metadata and a declined prompt', async () => {
  let prompts = 0;
  const f = await setup(async () => { prompts += 1; return false; }, mvpFixtureBundle, 'window-9');
  f.first.projection.observation!.observed_target!.app = 'foreign.app';
  await assert.rejects(f.session.authorizeNativeWindow('snap-1'), /stale/);
  f.first.projection = view(mvpFixtureBundle, 'window-4294967296');
  await assert.rejects(f.session.authorizeNativeWindow('snap-1'), /stale/);
  f.first.projection = view(mvpFixtureBundle, 'window-9');
  await f.session.authorizeNativeWindow('snap-1');

  assert.equal(prompts, 1);
  assert.equal(f.runs.length, 1, 'declining consent must not configure another device scope');
  assert.equal(f.session.authorization.phase, 'observing');
  assert.equal(f.session.authorization.authorized, null);
  await f.session.shutdown();
});

test('stopping during an MVP fixture consent prompt prevents the fixed scope from starting', async () => {
  const approval = deferred<boolean>();
  const f = await setup(() => approval.promise, mvpFixtureBundle, 'window-9');
  const pending = f.session.authorizeNativeWindow('snap-1');
  f.session.stopAtDevice({ scope: 'all' }, 'cancel', 'global_hotkey');
  approval.resolve(true);
  await assert.rejects(pending, /cancelled/);
  assert.equal(f.runs.length, 1);
  assert.equal(f.session.authorization.authorized, null);
  assert.equal(f.configurations.some(config => config.nativeSource?.authorizedWindowId !== undefined), false);
  await f.session.shutdown();
});

test('the MVP fixture window must remain the same throughout consent', async () => {
  const approval = deferred<boolean>();
  const f = await setup(() => approval.promise, mvpFixtureBundle, 'window-9');
  const pending = f.session.authorizeNativeWindow('snap-1');
  f.first.projection.observation!.observed_target!.window_id = 'window-10';
  approval.resolve(true);
  await assert.rejects(pending, /changed/);
  assert.equal(f.runs.length, 1);
  assert.equal(f.session.authorization.authorized, null);
  assert.equal(f.configurations.some(config => config.nativeSource?.authorizedWindowId !== undefined), false);
  await f.session.shutdown();
});

test('a changed window, expiry or process exit during confirmation cannot grant permission', async () => {
  for (const change of ['window', 'expiry', 'exit']) {
    const approval = deferred<boolean>(); const f = await setup(() => approval.promise);
    const pending = f.session.authorizeNativeWindow('snap-1');
    if (change === 'window') f.first.projection.observation!.observed_target!.window_id = 'window-10';
    if (change === 'expiry') f.first.projection.observation!.observed_target!.valid_until_ms = 1000;
    if (change === 'exit') f.first.emit('exit', 'source', 1, null);
    approval.resolve(true); await assert.rejects(pending, /changed|cancelled/);
    assert.equal(f.runs.length, 1); assert.equal(f.session.authorization.authorized, null); await f.session.shutdown();
  }
});

test('Stop invalidates a pending native dialog even if it later approves', async () => {
  const approval = deferred<boolean>(); const f = await setup(() => approval.promise);
  const pending = f.session.authorizeNativeWindow('snap-1');
  f.session.stopAtDevice({ scope: 'all' }, 'cancel', 'global_hotkey');
  approval.resolve(true); await assert.rejects(pending, /cancelled/);
  assert.equal(f.runs.length, 1); assert.ok(f.first.stops > 0); await f.session.shutdown();
});

test('revocation fences late approval and replaces the session without restoring tasks', async () => {
  const approval = deferred<boolean>(); const f = await setup(() => approval.promise);
  const pending = f.session.authorizeNativeWindow('snap-1');
  await f.session.revokeNativeWindow(); approval.resolve(true); await assert.rejects(pending, /cancelled/);
  assert.equal(f.runs.length, 2); assert.equal(f.configurations[1]!.nativeSource?.authorizedWindowId, undefined);
  assert.equal(f.session.projection?.root, null); await f.session.shutdown();
});

test('revocation while the old process is closing never starts the approved input session', async () => {
  const closed = deferred<void>(); const f = await setup(); f.first.close = () => closed.promise;
  const pending = f.session.authorizeNativeWindow('snap-1'); await Promise.resolve();
  const revoke = f.session.revokeNativeWindow(); closed.resolve(); await pending; await revoke;
  assert.equal(f.runs.length, 2); assert.equal(f.configurations[1]!.nativeSource?.authorizedWindowId, undefined);
  assert.equal(f.session.authorization.phase, 'observing'); await f.session.shutdown();
});

test('old projections and release acknowledgements are detached from the replacement', async () => {
  const f = await setup(); let acknowledgements = 0;
  f.session.on('stopAck', () => { acknowledgements += 1; });
  await f.session.authorizeNativeWindow('snap-1');
  f.first.emit('projection', { ...view(), mode: 'execute' });
  f.first.emit('stopAck', {} as SupervisorEvents['stopAck'][0]);
  assert.equal(f.session.projection?.mode, 'observe'); assert.equal(acknowledgements, 0);
  assert.equal(f.first.listenerCount('projection'), 0); await f.session.shutdown();
});

test('a failed replacement closes admission and never falls back to fixture input', async () => {
  let starts = 0;
  const first = new Pipeline();
  const session = await NativeSession.start(options, async () => true, async () => { if (starts++) throw new Error('device unavailable'); return first; }, () => now);
  await assert.rejects(session.authorizeNativeWindow('snap-1'), /device unavailable/);
  assert.equal(first.closed, true); assert.equal(session.authorization.phase, 'unavailable');
  assert.throws(() => session.operator({ command: 'create_task', text: 'new' }), /unavailable/);
  await session.shutdown(); assert.equal(starts, 2);
});

test('concurrent shutdown waits for a replacement already starting and closes that process', async () => {
  const starting = deferred<Pipeline>(); const next = new Pipeline(); let starts = 0;
  const session = await NativeSession.start(options, async () => true, async () => starts++ ? starting.promise : new Pipeline(), () => now);
  const pending = session.authorizeNativeWindow('snap-1');
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  const closing = session.shutdown(); assert.equal(session.shutdown(), closing);
  starting.resolve(next); await pending; await closing;
  assert.equal(next.closed, true); assert.equal(session.projection, null);
});
