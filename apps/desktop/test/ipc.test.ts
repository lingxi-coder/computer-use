import { demoModels, demoProviders } from './demoProviderFixture.js';
import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { FrozenTaskCatalogEntry, OperatorCommand } from '@computer-use/desktop-api';

import { HostBridge, type ModelControl, type NativeControl } from '../src/main/hostBridge.js';
import type { ProviderSettings } from '../src/main/providerSettings.js';
import { isTrustedSender, registerIpc, type FrozenTaskCatalogPort, type IpcEventLike, type TrustedOrigins } from '../src/main/ipc.js';
import type { StopBridge } from '../src/main/stopBridge.js';
import type { FrozenTaskCanonicalGoal } from '../src/main/frozenTaskCatalog.js';
import { Channels, INVOKE_CHANNELS } from '../src/shared/channels.js';

const trusted: TrustedOrigins = { files: ['/app/out/renderer/index.html'], devOrigin: 'http://localhost:5173' };
const good: IpcEventLike = { senderFrame: { url: 'file:///app/out/renderer/index.html' } };

function setup(models?: ModelControl, native?: NativeControl,
  operator?: (command: OperatorCommand) => void | Promise<void>,
  selectBrowserPageProfile?: () => Promise<void>,
  frozenTasks?: FrozenTaskCatalogPort,
  providers?: ProviderSettings) {
  const commands: OperatorCommand[] = [];
  let stops = 0;
  const bridge = new HostBridge(
    { operator: operator ?? ((command) => { commands.push(command); }), projection: null },
    { press: () => (stops += 1) } as unknown as StopBridge,
    native, undefined, models ?? demoModels, selectBrowserPageProfile, providers ?? demoProviders,
  );
  const handlers = new Map<string, (event: IpcEventLike, ...args: unknown[]) => unknown>();
  const dispose = registerIpc(
    {
      handle: (channel, listener) => handlers.set(channel, listener),
      removeHandler: (channel) => handlers.delete(channel),
    },
    bridge,
    trusted,
    frozenTasks,
  );
  const call = (channel: string, event: IpcEventLike, ...args: unknown[]): unknown => handlers.get(channel)?.(event, ...args);
  return { commands, call, handlers, dispose, stops: () => stops };
}

test('only the packaged page or the loopback dev server may call main', () => {
  assert.equal(isTrustedSender('file:///app/out/renderer/index.html', trusted), true);
  assert.equal(isTrustedSender('file:///app/out/renderer/index.html#/task', trusted), true);
  assert.equal(isTrustedSender('http://localhost:5173/', trusted), true);
  for (const bad of [
    undefined,
    null,
    '',
    'not a url',
    'file:///tmp/evil.html',
    'http://localhost:9999/',
    'https://example.com/',
    'file:///app/out/renderer/../../evil.html',
  ]) {
    assert.equal(isTrustedSender(bad, trusted), false, String(bad));
  }
  assert.equal(isTrustedSender('http://localhost:5173/', { files: [] }), false, 'no dev origin, no dev access');
});

test('exactly the listed channels are registered', () => {
  const { handlers } = setup();
  assert.deepEqual([...handlers.keys()].sort(), [...INVOKE_CHANNELS].sort());
  // What main sends to the renderer is not something the renderer can invoke.
  assert.equal(handlers.has(Channels.projection), false);
  assert.equal(handlers.has(Channels.stopStatus), false);
  assert.equal(handlers.has(Channels.ownedDocumentStatusChanged), false);
});

test('fixed browser profile selection is trusted, zero-argument, and Main-owned', async () => {
  let selections = 0;
  const noModel = { authorization: { phase: 'not_configured', authorized_purposes: [] } } as unknown as ModelControl;
  const { call, commands } = setup(noModel, undefined, undefined, async () => { selections += 1; });
  const evil: IpcEventLike = { senderFrame: { url: 'https://example.com/' } };
  assert.throws(() => call(Channels.selectBrowserPageProfile, evil), /untrusted sender/);
  assert.throws(() => call(Channels.selectBrowserPageProfile, good, '/tmp/page.html'), /takes no arguments/);
  assert.throws(() => call(Channels.selectBrowserPageProfile, good, { kind: 'arbitrary' }), /takes no arguments/);
  assert.equal(selections, 0);
  await call(Channels.selectBrowserPageProfile, good) as Promise<void>;
  assert.equal(selections, 1);
  assert.deepEqual(commands, [], 'profile selection is a trusted Main configuration action, not an operator payload');

  const activeModel = { authorization: { phase: 'authorized', authorized_purposes: ['s2_planning'] } } as unknown as ModelControl;
  const blocked = setup(activeModel, undefined, undefined, async () => { selections += 1; });
  assert.throws(() => blocked.call(Channels.selectBrowserPageProfile, good), /Revoke the model connection/);
  assert.equal(selections, 1, 'a connected model cannot cross into the fixed browser profile');
});

test('a call from an untrusted page is refused before anything happens', () => {
  const { call, commands, stops } = setup();
  const evil: IpcEventLike = { senderFrame: { url: 'https://example.com/' } };
  for (const channel of INVOKE_CHANNELS) {
    assert.throws(() => call(channel, evil, 'x', true), /untrusted sender/, channel);
    assert.throws(() => call(channel, { senderFrame: null }, 'x', true), /untrusted sender/, channel);
  }
  assert.deepEqual(commands, []);
  assert.equal(stops(), 0);
});

test('a valid call becomes exactly the operator command it names', () => {
  const { call, commands, stops } = setup();
  call(Channels.createTask, good, 'turn off the daily picks');
  const allowedScope = {
    application: 'Interaction Fixture',
    targets: [{ control: 'Weekly digest', group: 'Notifications' }],
    no_unrelated_controls: true,
  };
  call(Channels.createScopedTask, good, { objective: 'Turn off Weekly digest.', allowed_scope: allowedScope });
  call(Channels.confirm, good, 'conf-1', true);
  call(Channels.pause, good, 'root-1');
  call(Channels.resume, good, 'root-1');
  call(Channels.setMode, good, 'execute');
  call(Channels.stop, good);
  assert.deepEqual(commands, [
    { command: 'create_task', text: 'turn off the daily picks' },
    { command: 'create_scoped_task', objective: 'Turn off Weekly digest.', allowed_scope: allowedScope },
    { command: 'confirm_goal', confirmation_id: 'conf-1', approved: true },
    { command: 'pause_task', root_task_id: 'root-1' },
    { command: 'resume_task', root_task_id: 'root-1' },
    { command: 'set_mode', mode: 'execute' },
  ]);
  assert.equal(stops(), 1, 'the stop goes through the stop bridge');
  assert.equal(call(Channels.snapshot, good), null);
});

test('arguments that are not what the schema allows are refused, and nothing is sent', () => {
  const { call, commands } = setup();
  const refused: [string, unknown[]][] = [
    [Channels.createTask, ['']],
    [Channels.createTask, ['   ']],
    [Channels.createTask, [42]],
    [Channels.createTask, ['x'.repeat(8_193)]],
    [Channels.createTask, ['中'.repeat(2_731)]],
    [Channels.createTask, ['text\u0000']],
    [Channels.confirm, ['conf-1', 'yes']],
    [Channels.confirm, [7, true]],
    [Channels.confirm, ['', true]],
    [Channels.pause, [null]],
    [Channels.resume, [{ id: 'root-1' }]],
    [Channels.setMode, ['root']],
    [Channels.setMode, [undefined]],
  ];
  for (const [channel, args] of refused) {
    assert.throws(() => call(channel, good, ...args), /refused/, `${channel} ${JSON.stringify(args)}`);
  }
  assert.deepEqual(commands, []);
});

test('scoped task IPC preserves typed group or form context and rejects mixed context', () => {
  const { call, commands } = setup();
  const goodRequest = {
    objective: 'Turn off Weekly digest.',
    allowed_scope: {
      application: 'Interaction Fixture',
      targets: [{ control: 'Weekly digest', group: 'Notifications' }],
      no_unrelated_controls: true,
    },
  };
  assert.throws(() => call(Channels.createScopedTask, good), /exactly one argument/);
  assert.throws(() => call(Channels.createScopedTask, good, goodRequest, 'extra'), /exactly one argument/);
  assert.throws(() => call(Channels.createScopedTask, good, { ...goodRequest, path: '/tmp/file' }), /invalid scoped task request shape/);
  assert.throws(() => call(Channels.createScopedTask, good, { ...goodRequest, objective: 'x'.repeat(8_193) }), /reasonable length/);
  assert.throws(() => call(Channels.createScopedTask, good, { ...goodRequest, objective: 'x\u0000' }), /reasonable length/);
  for (const allowed_scope of [
    { ...goodRequest.allowed_scope, no_unrelated_controls: false },
    { ...goodRequest.allowed_scope, targets: [] },
    { ...goodRequest.allowed_scope, targets: [...goodRequest.allowed_scope.targets, { control: 'Other' }] },
    { ...goodRequest.allowed_scope, targets: [{ control: 'Weekly digest', group: 'Notifications', form: 'Preferences' }] },
    { ...goodRequest.allowed_scope, targets: [{ control: 'Weekly digest', form: 'Preferences', disambiguator: 'Preferences' }] },
    { ...goodRequest.allowed_scope, targets: [{ control: 'Weekly digest', group: 'Notifications', disambiguator: 'Other' }] },
    { ...goodRequest.allowed_scope, targets: [{ control: 'Weekly digest', group: 'Notifications', disambiguator: 'Notifications', selector: '#unsafe' }] },
    { ...goodRequest.allowed_scope, application: '中'.repeat(86) },
    { ...goodRequest.allowed_scope, targets: [{ control: '中'.repeat(171) }] },
    { ...goodRequest.allowed_scope, targets: [{ control: 'Weekly digest', group: '中'.repeat(86) }] },
    { ...goodRequest.allowed_scope, targets: [{ control: 'Weekly digest', form: '中'.repeat(86) }] },
    { ...goodRequest.allowed_scope, targets: [{ control: 'Weekly digest', group: '中'.repeat(86), disambiguator: '中'.repeat(86) }] },
  ]) {
    assert.throws(() => call(Channels.createScopedTask, good, { objective: goodRequest.objective, allowed_scope }), /refused/);
  }
  assert.deepEqual(commands, []);

  call(Channels.createScopedTask, good, goodRequest);
  const exactByteBoundaries = {
    objective: 'Preserve exact bounded labels.',
    allowed_scope: {
      application: `${'中'.repeat(85)}a`,
      targets: [{ control: `${'中'.repeat(170)}ab`, group: `${'中'.repeat(85)}a` }],
      no_unrelated_controls: true,
    },
  };
  call(Channels.createScopedTask, good, exactByteBoundaries);
  const formRequest = {
    objective: 'Update the display name in Profile.',
    allowed_scope: {
      application: 'Interaction Fixture',
      targets: [{ control: 'Display name', form: 'Profile' }],
      no_unrelated_controls: true,
    },
  };
  call(Channels.createScopedTask, good, formRequest);
  assert.deepEqual(commands, [
    { command: 'create_scoped_task', ...goodRequest },
    { command: 'create_scoped_task', ...exactByteBoundaries },
    { command: 'create_scoped_task', ...formRequest },
  ]);
});

const catalogId = 'a'.repeat(64);
const selector = 'b'.repeat(64);
const unsupportedSelector = 'c'.repeat(64);
const frozenGoal = Object.freeze({
  objective: 'Turn off Weekly digest.',
  allowed_scope: Object.freeze({
    application: 'Interaction Fixture',
    targets: Object.freeze([Object.freeze({ control: 'Weekly digest', group: 'Notifications' })]),
    no_unrelated_controls: true,
  }),
});
const frozenProfile = Object.freeze({
  catalog_id: catalogId,
  selector,
  source_freeze_sha256: 'd'.repeat(64),
  source_request_sha256: 'e'.repeat(64),
  goals_sha256: 'f'.repeat(64),
  suite: 'fixture',
  task_id: 'FIX-01',
  seed: 1,
});
const compatibleEntry = Object.freeze({ catalog_id: catalogId, selector, task_id: 'FIX-01', seed: '1', suite: 'fixture', scope_family: 'control' as const, scope_compatible: true });
const unsupportedEntry = Object.freeze({ catalog_id: catalogId, selector: unsupportedSelector, task_id: 'TEXTEDIT-01', seed: '1', suite: 'textedit', scope_family: 'textedit' as const, scope_compatible: false, unsupported_reason: 'textedit_scope_not_supported' });
const defaultCatalogRows: readonly FrozenTaskCatalogEntry[] = Object.freeze(Array.from({ length: 120 }, (_, index) => index === 0 ? compatibleEntry
  : index === 119 ? unsupportedEntry
    : { ...compatibleEntry, selector: index.toString(16).padStart(64, '0'), task_id: `FIX-${String(index + 1).padStart(2, '0')}` }));

function catalogPort(rows: readonly FrozenTaskCatalogEntry[] = defaultCatalogRows, state: 'empty' | 'available' | 'unavailable' = 'available'): FrozenTaskCatalogPort {
  const snapshot = Object.freeze({ status: Object.freeze({ state }), tasks: Object.freeze([...rows]) });
  return {
    snapshot: () => snapshot,
    resolve: (selection) => {
      if (selection.catalog_id !== catalogId) return null;
      if (selection.selector === selector) return { status: 'available', goal: frozenGoal, profile: frozenProfile, profileMetadata: compatibleEntry };
      if (selection.selector === unsupportedSelector) return { status: 'unsupported', reason: unsupportedEntry.unsupported_reason, profileMetadata: unsupportedEntry };
      return null;
    },
  } as FrozenTaskCatalogPort;
}

test('missing catalog preserves ordinary task creation and reports an empty catalog', () => {
  const { call, commands } = setup();
  assert.deepEqual(call(Channels.frozenTaskCatalog, good), { status: { state: 'empty' }, tasks: [] });
  call(Channels.createTask, good, 'turn off the daily picks');
  assert.deepEqual(commands, [{ command: 'create_task', text: 'turn off the daily picks' }]);
});

test('frozen catalog exposes every safe metadata row, including unsupported records', () => {
  const rows = Array.from({ length: 120 }, (_, index) => index === 119
    ? { ...unsupportedEntry, selector: 'c'.repeat(63) + 'd' }
    : { ...compatibleEntry, selector: index.toString(16).padStart(64, '0') });
  const { call, commands } = setup(undefined, undefined, undefined, undefined, catalogPort(rows));
  const snapshot = call(Channels.frozenTaskCatalog, good) as { status: { state: string }; tasks: Record<string, unknown>[] };
  assert.equal(snapshot.status.state, 'available');
  assert.equal(snapshot.tasks.length, 120);
  assert.equal(snapshot.tasks.filter((task) => task['scope_compatible'] === false).length, 1);
  assert.ok(snapshot.tasks.every((task) => !('objective' in task) && !('allowed_scope' in task) && !('path' in task)));
  assert.throws(() => call(Channels.frozenTaskCatalog, good, 'extra'), /takes no arguments/);
  assert.deepEqual(commands, []);
});

test('frozen selection resolves only through the catalog and maps its immutable goal and profile', () => {
  const { call, commands } = setup(undefined, undefined, undefined, undefined, catalogPort());
  call(Channels.createFrozenTask, good, { catalog_id: catalogId, selector });
  assert.deepEqual(commands, [{ command: 'create_frozen_task', ...frozenGoal, profile: frozenProfile }]);
});

test('frozen FORM selection preserves the exact form field into Host admission', () => {
  const formSelector = '9'.repeat(64);
  const formAllowedScope: FrozenTaskCanonicalGoal['allowed_scope'] = {
    application: 'Interaction Fixture',
    targets: [{ control: 'Display name', form: 'Profile' }],
    no_unrelated_controls: true,
  };
  Object.freeze(formAllowedScope.targets[0]);
  Object.freeze(formAllowedScope.targets);
  Object.freeze(formAllowedScope);
  const formGoal: FrozenTaskCanonicalGoal = Object.freeze({
    objective: 'Update the display name in the Profile form.',
    allowed_scope: formAllowedScope,
  });
  const formProfile = Object.freeze({ ...frozenProfile, selector: formSelector, task_id: 'FORM-01' });
  const formEntry = Object.freeze({ ...compatibleEntry, selector: formSelector, task_id: 'FORM-01', scope_family: 'form' as const });
  const rows = Object.freeze([formEntry, ...defaultCatalogRows.slice(1)]);
  const formCatalog: FrozenTaskCatalogPort = {
    snapshot: () => Object.freeze({ status: Object.freeze({ state: 'available' as const }), tasks: rows }),
    resolve: (selection) => selection.catalog_id === catalogId && selection.selector === formSelector
      ? { status: 'available', goal: formGoal, profile: formProfile, profileMetadata: formEntry }
      : null,
  };
  const { call, commands } = setup(undefined, undefined, undefined, undefined, formCatalog);
  call(Channels.createFrozenTask, good, { catalog_id: catalogId, selector: formSelector });
  assert.deepEqual(commands, [{
    command: 'create_frozen_task',
    ...formGoal,
    profile: formProfile,
  }]);
  assert.deepEqual((commands[0] as Extract<OperatorCommand, { command: 'create_frozen_task' }>).allowed_scope.targets[0], {
    control: 'Display name', form: 'Profile',
  });
});

test('frozen creation rejects mutable catalog values and mismatched profile provenance', () => {
  const valid = catalogPort();
  const mutable = {
    snapshot: () => valid.snapshot(),
    resolve: () => ({ status: 'available', goal: { objective: frozenGoal.objective, allowed_scope: frozenGoal.allowed_scope },
      profile: frozenProfile, profileMetadata: compatibleEntry }),
  } as unknown as FrozenTaskCatalogPort;
  const mutableCall = setup(undefined, undefined, undefined, undefined, mutable);
  assert.throws(() => mutableCall.call(Channels.createFrozenTask, good, { catalog_id: catalogId, selector }), /mutable task data/);
  assert.deepEqual(mutableCall.commands, []);

  const mismatched = {
    snapshot: () => valid.snapshot(),
    resolve: () => ({ status: 'available', goal: frozenGoal,
      profile: Object.freeze({ ...frozenProfile, selector: '9'.repeat(64) }), profileMetadata: compatibleEntry }),
  } as unknown as FrozenTaskCatalogPort;
  const mismatchedCall = setup(undefined, undefined, undefined, undefined, mismatched);
  assert.throws(() => mismatchedCall.call(Channels.createFrozenTask, good, { catalog_id: catalogId, selector }), /provenance does not match/);
  assert.deepEqual(mismatchedCall.commands, []);

  const oversized = {
    snapshot: () => valid.snapshot(),
    resolve: () => ({ status: 'available', goal: Object.freeze({ objective: 'x'.repeat(8_193), allowed_scope: frozenGoal.allowed_scope }),
      profile: frozenProfile, profileMetadata: compatibleEntry }),
  } as unknown as FrozenTaskCatalogPort;
  const oversizedCall = setup(undefined, undefined, undefined, undefined, oversized);
  assert.throws(() => oversizedCall.call(Channels.createFrozenTask, good, { catalog_id: catalogId, selector }), /reasonable length/);
  assert.deepEqual(oversizedCall.commands, []);
});

test('frozen creation rejects renderer payloads, malformed hashes, absent records, and unsupported scopes', () => {
  const { call: noCatalog, commands: noCatalogCommands } = setup();
  assert.throws(() => noCatalog(Channels.createFrozenTask, good, { catalog_id: catalogId, selector }), /not configured/);
  assert.deepEqual(noCatalogCommands, []);

  const { call, commands } = setup(undefined, undefined, undefined, undefined, catalogPort());
  assert.throws(() => call(Channels.createFrozenTask, good), /exactly one argument/);
  assert.throws(() => call(Channels.createFrozenTask, good, { catalog_id: catalogId, selector }, 'extra'), /exactly one argument/);
  for (const extra of [
    { objective: 'changed' }, { allowed_scope: frozenGoal.allowed_scope }, { path: '/tmp/task.json' },
    { reset: true }, { initial_state: { checked: true } },
  ]) assert.throws(() => call(Channels.createFrozenTask, good, { catalog_id: catalogId, selector, ...extra }), /invalid frozen task selection shape/);
  for (const bad of [null, 5]) {
    assert.throws(() => call(Channels.createFrozenTask, good, bad), /invalid frozen task selection/);
  }
  for (const bad of [{ catalog_id: 'A'.repeat(64), selector }, { catalog_id: 'a'.repeat(63), selector },
    { catalog_id: catalogId, selector: 'z'.repeat(64) }, { catalog_id: catalogId, selector: '/tmp/selector' }]) {
    assert.throws(() => call(Channels.createFrozenTask, good, bad), /SHA-256/);
  }
  assert.throws(() => call(Channels.createFrozenTask, good, { catalog_id: catalogId, selector: '9'.repeat(64) }), /not in the configured catalog/);
  assert.throws(() => call(Channels.createFrozenTask, good, { catalog_id: catalogId, selector: unsupportedSelector }), /textedit_scope_not_supported/);
  assert.deepEqual(commands, []);
});

test('configured or unavailable catalogs disable the free-form IPC route', () => {
  for (const state of ['available', 'unavailable'] as const) {
    const { call, commands } = setup(undefined, undefined, undefined, undefined, catalogPort(undefined, state));
    assert.throws(() => call(Channels.createTask, good, 'arbitrary objective'), /free-form task creation is disabled/);
    assert.throws(() => call(Channels.createScopedTask, good, {
      objective: 'arbitrary objective',
      allowed_scope: { application: 'Interaction Fixture', targets: [{ control: 'Other' }], no_unrelated_controls: true },
    }), /user-authored scoped tasks are disabled/);
    assert.deepEqual(commands, []);
  }
});

test('disposing removes every handler', () => {
  const { handlers, dispose } = setup();
  assert.ok(handlers.size > 0);
  dispose();
  assert.equal(handlers.size, 0);
});

test('image requests validate identity and expose neither paths nor generic IO', () => {
  const { call, commands } = setup();
  for (const bad of [null, 3, '', ' ', 'x'.repeat(257), 'image\u0000id']) {
    assert.throws(() => call(Channels.image, good, bad), /invalid image/);
  }
  assert.equal(call(Channels.image, good, '/etc/passwd'), null);
  assert.deepEqual(commands, []);
});

test('native permission accepts only observation identity and keeps consent in main', async () => {
  const { call } = setup();
  for (const invalid of [null, 42, { pid: 42, window_id: 'window-9' }, '', 'x'.repeat(257), 'snap\u0000']) {
    assert.throws(() => call(Channels.authorizeNativeWindow, good, invalid), /invalid native observation/);
  }
  assert.throws(() => call(Channels.authorizeNativeWindow, good, 'snap-1'), /not configured/);
  assert.deepEqual(call(Channels.nativeAuthorization, good), { phase: 'not_native', candidate: null, authorized: null,
    candidate_domain: null, authorized_domain: null, can_authorize: false, can_refresh_observation: false, reason: null });
  assert.throws(() => call(Channels.refreshNativeObservation, good), /not configured/);
});

test('refresh observation is a trusted zero-argument invoke gated by Main status', async () => {
  const calls: string[] = [];
  const native: NativeControl = {
    authorization: { phase: 'observing', candidate: null, authorized: null, can_authorize: false,
      candidate_domain: null, authorized_domain: null, can_refresh_observation: true, reason: null },
    authorizeNativeWindow: async () => {},
    refreshNativeObservation: async () => { calls.push('refresh'); },
    revokeNativeWindow: async () => {},
  };
  const { call } = setup(undefined, native);
  const evil: IpcEventLike = { senderFrame: { url: 'https://example.com/' } };
  assert.throws(() => call(Channels.refreshNativeObservation, evil), /untrusted sender/);
  assert.throws(() => call(Channels.refreshNativeObservation, good, 'window-9'), /takes no arguments/);
  await call(Channels.refreshNativeObservation, good);
  assert.deepEqual(calls, ['refresh']);

  const denied = setup(undefined, { ...native, authorization: { ...native.authorization, can_refresh_observation: false } });
  assert.throws(() => denied.call(Channels.refreshNativeObservation, good), /cannot be refreshed/);
  assert.deepEqual(calls, ['refresh'], 'a stale renderer cannot bypass the current Main status');
});

test('model credentials use the dedicated main boundary and never become operator commands', async () => {
  const calls: string[] = [];
  const request = { route: { provider_id: 'open_ai' as const, protocol: 'open_ai_responses' as const,
    base_url: 'https://example.invalid/v1', model: 'fixture', max_output_tokens: 4096 }, api_key: 'dedicated-test-key', storage: 'memory' as const, purposes: ['s2_planning'] as const };
  const authorized: unknown[] = [], savedPurposes: unknown[] = [];
  const models: ModelControl = {
    authorization: { phase: 'not_configured', route: null, credential_kind: null, storage: null, authorized_purposes: [], can_authorize: true, can_save: false, planning_available: false, saved: [], reason: null },
    authorize: async (value) => { authorized.push(value); calls.push('authorize'); },
    authorizeOpenAiLogin: async () => {},
    authorizeSaved: async (id, purposes) => { assert.equal(id, 'a'.repeat(64)); savedPurposes.push(purposes); calls.push('saved'); },
    revoke: async () => { calls.push('revoke'); }, forgetSaved: async () => { calls.push('forget'); },
  };
  const { call, commands } = setup(models);
  assert.deepEqual(call(Channels.modelAuthorization, good), models.authorization);
  for (const value of [null, { ...request, path: '/private/key' }, { ...request, route: { ...request.route, api_key: request.api_key } },
    { ...request, purposes: [] }, { ...request, purposes: ['s1_teacher', 's1_teacher'] }]) {
    assert.throws(() => call(Channels.authorizeModel, good, value), /Invalid/);
  }
  assert.throws(() => call(Channels.authorizeModel, good, { ...request, purposes: ['s3_execute'] }), /Invalid/);
  for (const id of [null, '/private/key', 'a'.repeat(63), 'A'.repeat(64)]) {
    assert.throws(() => call(Channels.authorizeSavedModel, good, id, ['s2_planning']), /Invalid/);
    assert.throws(() => call(Channels.forgetSavedModel, good, id), /Invalid/);
  }
  for (const purposes of [undefined, [], ['s1_teacher', 's1_teacher'], ['s3_execute']]) {
    assert.throws(() => call(Channels.authorizeSavedModel, good, 'a'.repeat(64), purposes), /Invalid/);
  }
  await call(Channels.authorizeModel, good, request);
  await call(Channels.authorizeModel, good, { ...request, purposes: ['s1_teacher'] });
  await call(Channels.authorizeSavedModel, good, 'a'.repeat(64), ['s2_planning']);
  await call(Channels.authorizeSavedModel, good, 'a'.repeat(64), ['s1_teacher']);
  await call(Channels.revokeModel, good); await call(Channels.forgetSavedModel, good, 'a'.repeat(64));
  assert.deepEqual(calls, ['authorize', 'authorize', 'saved', 'saved', 'revoke', 'forget']);
  assert.deepEqual(authorized, [request, { ...request, purposes: ['s1_teacher'] }]);
  assert.deepEqual(savedPurposes, [['s2_planning'], ['s1_teacher']]); assert.deepEqual(commands, []);
});

test('OpenAI account login IPC validates a bounded model and S2/S1 purposes before Provider Settings delegation', async () => {
  const calls: unknown[] = [];
  const providers = {
    snapshot: { providers: [{ id: 'openai-chatgpt', auth: 'openai_oauth', models: [{ id: 'gpt-6-luna', selectable: true }] }] },
    configure: async (request: unknown) => { calls.push(request); },
  } as unknown as ProviderSettings;
  const { call, commands } = setup(undefined, undefined, undefined, undefined, undefined, providers);
  const request = { model: 'gpt-6-luna', purposes: ['s2_planning', 's1_teacher'] };
  assert.throws(() => call(Channels.authorizeOpenAiLogin, good), /exactly one request/);
  assert.throws(() => call(Channels.authorizeOpenAiLogin, good, request, 'extra'), /exactly one request/);
  for (const invalid of [
    null,
    { ...request, api_key: 'must-not-cross-ipc' },
    { ...request, purposes: [] },
    { ...request, purposes: ['s2_planning', 's2_planning'] },
    { ...request, purposes: ['computer_proposals'] },
    { ...request, model: '' },
    { ...request, model: 'x'.repeat(257) },
    { ...request, model: 'gpt-6-luna\nsecret' },
  ]) {
    assert.throws(() => call(Channels.authorizeOpenAiLogin, good, invalid), /invalid OpenAI account login request/);
  }
  await call(Channels.authorizeOpenAiLogin, good, request);
  assert.deepEqual(calls, [{ provider_id: 'openai-chatgpt', storage: 'keychain', purposes: ['s2_planning', 's1_teacher'], reauthenticate: true }]);
  assert.deepEqual(commands, [], 'account login never becomes an operator command');
});

test('owned-document channels require a trusted zero-argument invoke', async () => {
  const { call, commands, stops } = setup();
  const channels = [Channels.ownedDocumentStatus, Channels.createOwnedDocument,
    Channels.authorizeOwnedDocumentEdit, Channels.authorizeOwnedDocumentSave, Channels.revisitOwnedDocument];
  const evil: IpcEventLike = { senderFrame: { url: 'https://example.com/' } };
  for (const channel of channels) {
    assert.throws(() => call(channel, evil), /untrusted sender/, channel);
    assert.throws(() => call(channel, good, 'unexpected'), /takes no arguments/, channel);
  }
  for (const channel of [Channels.stop, Channels.snapshot, Channels.nativeAuthorization, Channels.refreshNativeObservation, Channels.presentation,
    Channels.revokeNativeWindow, Channels.modelAuthorization, Channels.revokeModel]) {
    assert.throws(() => call(channel, good, 'unexpected'), /takes no arguments/, channel);
  }
  assert.deepEqual(commands, []);
  assert.equal(stops(), 0);
  assert.deepEqual(call(Channels.ownedDocumentStatus, good), {
    phase: 'not_created', can_create: false, can_authorize_edit: false,
    can_authorize_save: false, can_revisit: false,
    reason: 'Temporary TextEdit documents are unavailable in this session.',
  });
  for (const channel of channels.slice(1)) {
    assert.throws(() => call(channel, good), /unavailable|cannot be/i, channel);
  }

  const legacyNative: NativeControl = {
    authorization: { phase: 'observing', candidate: null, authorized: null, can_authorize: false,
      candidate_domain: null, authorized_domain: null, can_refresh_observation: false, reason: null },
    authorizeNativeWindow: async () => {}, refreshNativeObservation: async () => {}, revokeNativeWindow: async () => {},
  };
  const legacy = setup(undefined, legacyNative);
  const legacyStatus = legacy.call(Channels.ownedDocumentStatus, good) as { phase: string; can_create: boolean; can_authorize_edit: boolean; can_authorize_save: boolean; can_revisit: boolean };
  assert.equal(legacyStatus.phase, 'not_created');
  assert.deepEqual([legacyStatus.can_create, legacyStatus.can_authorize_edit,
    legacyStatus.can_authorize_save, legacyStatus.can_revisit], [false, false, false, false]);
});

test('owned-document calls delegate only when the optional native control grants that step', async () => {
  const calls: string[] = [];
  const status = {
    phase: 'edit_authorized' as const, can_create: false, can_authorize_edit: false,
    can_authorize_save: true, can_revisit: false, reason: null,
  };
  const native: NativeControl = {
    authorization: { phase: 'observing', candidate: null, authorized: null, can_authorize: false,
      candidate_domain: null, authorized_domain: null, can_refresh_observation: false, reason: null },
    authorizeNativeWindow: async () => {}, refreshNativeObservation: async () => {}, revokeNativeWindow: async () => {},
    ownedDocumentStatus: status,
    createOwnedDocument: async () => { calls.push('create'); },
    authorizeOwnedDocumentEdit: async () => { calls.push('edit'); },
    authorizeOwnedDocumentSave: async () => { calls.push('save'); },
    revisitOwnedDocument: async () => { calls.push('revisit'); },
  };
  const { call } = setup(undefined, native);
  assert.equal(call(Channels.ownedDocumentStatus, good), status);
  for (const channel of [Channels.createOwnedDocument, Channels.authorizeOwnedDocumentEdit, Channels.revisitOwnedDocument]) {
    assert.throws(() => call(channel, good), /cannot be authorized|cannot be created|cannot be reopened/i);
  }
  await call(Channels.authorizeOwnedDocumentSave, good);
  assert.deepEqual(calls, ['save']);
});

test('setMode invoke waits for and propagates native activation failure', async () => {
  const activationFailure = new Error('temporary TextEdit activation was refused');
  let dispatched = 0;
  const { call, commands } = setup(undefined, undefined, (command) => {
    dispatched += 1;
    if (command.command === 'set_mode' && command.mode === 'execute') return Promise.reject(activationFailure);
  });

  const result = call(Channels.setMode, good, 'execute');
  assert.ok(result instanceof Promise, 'renderer invoke receives the activation promise');
  await assert.rejects(result as Promise<void>, (error) => error === activationFailure);
  assert.equal(dispatched, 1);
  assert.deepEqual(commands, [], 'a failed activation is not reported as an accepted mode change');
});
