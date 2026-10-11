import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import test from 'node:test';
import type { ModelAuthorizationRequest, ModelAuthorizationStatus, ModelPurpose, OperatorCommand, UiProjection } from '@computer-use/desktop-api';
import { ModelAccess, modelRequest, type ModelKeyStore } from '../src/main/modelAccess.js';
import { ModelKeyStoreFailure } from '../src/main/modelKeyStore.js';
import { NativeSession } from '../src/main/nativeSession.js';
import type { OpenAiOAuthCredential } from '../src/main/modelBootstrap.js';
import type { SupervisorOptions, SupervisorEvents } from '../src/main/supervisor.js';
import { nativeUnavailableDomains } from './capabilityFixtures.js';

const KEY = 'only-this-explicit-test-key';
const NATIVE_BUNDLE = 'com.lingxi.computer-user.guard-fixture';
const request = (): ModelAuthorizationRequest => ({ api_key: KEY, storage: 'memory', purposes: ['s2_planning'], route: {
  provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://example.invalid/v1', model: 'test-model', max_output_tokens: 4096,
} });
function nativeView(): UiProjection {
  return {
    revision: 1, mode: 'observe', root: null, pending_confirmation: null, timeline: [], stop_status: 'not_stopped', cleanup: 'not_needed',
    capabilities: { backend: 'macos', execution_domain: 'foreground_native', domains: nativeUnavailableDomains(), input: 'unavailable', human_input: 'available', hit_test: 'available', policy: 'atomic_only', max_uninterruptible_ms: 100,
      identity: { session_epoch: 1, surface: { id: 'desktop', epoch: 1 }, clock_domain: 'macos/1/wall', dependencies: {} } },
    observation: { sources: [], frame: null, target_app: NATIVE_BUNDLE, target_name: 'Test window', active_window: 'Main', observed_target: {
      snapshot_id: 'snap-1', structure_sequence: 1, session_epoch: 1, app: NATIVE_BUNDLE, pid: 42,
      window_id: 'window-9', window_title: 'Main', observed_ms: 900, valid_until_ms: 2900, contains_private_fields: true,
    } },
  };
}
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((done) => { resolve = done; }); return { promise, resolve }; }
class Pipeline extends EventEmitter<SupervisorEvents> {
  readonly clockOriginUnixMs = 9_000;
  projection: UiProjection = { revision: 1, mode: 'observe', root: null, stop_status: 'not_stopped', cleanup: 'not_needed', pending_confirmation: null, timeline: [], capabilities: null, observation: null };
  closed = false; stops = 0; commands: OperatorCommand[] = []; close = async () => {};
  operator(value: OperatorCommand) { this.commands.push(value); }
  stopAtDevice() { this.stops += 1; }
  sendDecisionProposal() {}
  image() { return null; }
  async shutdown() { await this.close(); this.closed = true; }
}
async function setup(
  confirm: ConstructorParameters<typeof ModelAccess>[0] = async () => true,
  store?: ModelKeyStore,
  nativeWindow = false,
  openAiLogin?: (request: { model: string }, signal: AbortSignal) => Promise<OpenAiOAuthCredential>,
) {
  const models = new ModelAccess(confirm, store, false, openAiLogin);
  await models.initialize();
  const runs: Pipeline[] = [], configs: SupervisorOptions[] = [];
  const session = await NativeSession.start({
    binaries: { host: '/host', source: '/source', device: '/device' },
    ...(nativeWindow ? { nativeSource: { helperPath: '/source-helper', bundleId: NATIVE_BUNDLE, pid: 42 } } : {}),
  }, async () => nativeWindow, async (options) => {
    if (runs.length) assert.equal(runs.at(-1)!.closed, true);
    configs.push(models.supervisorOptions(options)); const run = new Pipeline();
    if (nativeWindow) run.projection = nativeView();
    runs.push(run); return run;
  }, () => 10_000);
  models.attach(session);
  return { models, session, runs, configs, shutdown: () => models.shutdown(() => session.shutdown()) };
}
test('startup keys are absent, consent cancellation keeps the existing session', async () => {
  const f = await setup(async () => false);
  assert.equal(f.configs[0]!.modelAuthorization, undefined);
  await f.models.authorize(request());
  assert.equal(f.runs.length, 1); assert.equal(f.models.authorization.phase, 'not_configured');
  assert.equal(f.models.authorization.planning_available, false); assert.equal(f.models.authorization.reason, null); await f.shutdown();
});
test('native outbound consent receives routing facts, never credentials', async () => {
  let prompt = '', consentedPurposes: string[] = [];
  const f = await setup(async (route, storage, purposes) => { prompt = JSON.stringify({ route, storage }); consentedPurposes = purposes; return true; });
  await f.models.authorize(request());
  assert(!prompt.includes(KEY)); assert.deepEqual(consentedPurposes, ['s2_planning']);
  assert.deepEqual(f.configs[1]!.modelAuthorization!.credential, { kind: 'api_key', api_key: KEY });
  assert.deepEqual(f.configs[1]!.modelAuthorization!.purposes, ['s2_planning']);
  assert.equal(f.configs[1]!.modelAuthorization!.account_scope, undefined);
  assert.equal(f.configs[1]!.mode, 'observe'); assert.equal(f.runs[0]!.closed, true);
  assert.equal(f.models.authorization.phase, 'authorized'); assert.equal(f.models.authorization.planning_available, true);
  assert(!JSON.stringify(f.models.authorization).includes(KEY)); await f.shutdown();
});
test('legacy OpenAI login IPC is directed to Provider Settings and cannot create a memory session', async () => {
  let confirmations = 0, logins = 0;
  const f = await setup(async () => { confirmations += 1; return true; }, undefined, false, async () => {
    logins += 1;
    return { kind: 'openai_oauth', access_token: 'must-not-be-used', expires_at: Math.floor(Date.now() / 1000) + 600, fedramp: false };
  });
  await assert.rejects(f.models.authorizeOpenAiLogin({ model: 'gpt-6.1-sol', purposes: ['s1_teacher', 's2_planning'] }), /Provider Settings/);
  assert.equal(confirmations, 0);
  assert.equal(logins, 0);
  assert.equal(f.runs.length, 1);
  assert.equal(f.models.authorization.phase, 'not_configured');
  await f.shutdown();
});
test('OpenAI login refuses computer purpose, malformed model, and unknown request fields before consent', async () => {
  let consents = 0, logins = 0;
  const f = await setup(async () => { consents += 1; return true; }, undefined, false, async () => {
    logins += 1;
    return { kind: 'openai_oauth', access_token: 'token', expires_at: Math.floor(Date.now() / 1000) + 600, fedramp: false };
  });
  for (const request of [
    { model: 'test', purposes: ['computer_proposals'] },
    { model: ' test', purposes: ['s2_planning'] },
    { model: 'test', purposes: ['s2_planning'], extra: 'secret' },
    { model: 'test', purposes: [] },
  ]) await assert.rejects(f.models.authorizeOpenAiLogin(request as { model: string; purposes: ModelPurpose[] }));
  assert.equal(consents, 0); assert.equal(logins, 0); assert.equal(f.runs.length, 1);
  await f.shutdown();
});
test('provider selection activates a preconsented route without prompting or saving the credential again', async () => {
  let confirmations = 0, approve = true;
  const f = await setup(async () => { confirmations += 1; return approve; });
  const credential = { kind: 'api_key' as const, api_key: 'configured-provider-key' };
  const route = { ...request().route, preset_id: 'provider-profile-fixture' };
  await f.models.activateProvider(route, credential, ['s2_planning'], 'memory', 'provider-connection-fixture');
  assert.equal(confirmations, 0);
  assert.equal(f.models.authorization.phase, 'authorized');
  assert.equal(f.models.authorization.storage, 'memory');
  assert.equal(f.models.providerConnectionId, 'provider-connection-fixture');
  assert.deepEqual(f.configs[1]!.modelAuthorization!.route, route);
  assert.deepEqual(f.configs[1]!.modelAuthorization!.credential, credential);
  assert.equal(f.configs[1]!.modelAuthorization!.purposes.includes('s2_planning'), true);
  assert.equal(f.models.authorization.credential_kind, 'api_key');
  assert(!JSON.stringify(f.models.authorization).includes('provider-connection-fixture'));

  approve = false;
  await f.models.authorize({ ...request(), route, api_key: 'declined-legacy-key' });
  assert.equal(f.models.providerConnectionId, 'provider-connection-fixture', 'declined replacement preserves the active vault identity');

  approve = true;
  await f.models.authorize({ ...request(), route, api_key: 'legacy-key-for-same-route' });
  assert.equal(f.models.providerConnectionId, null, 'a legacy authorization no longer belongs to the provider vault record');
  await f.models.revoke();
  assert.equal(f.models.providerConnectionId, null);
  await f.shutdown();
});
test('declined replacement consent re-arms an active Keychain OAuth access-token expiry timer', async () => {
  const delayedConsent = deferred<boolean>();
  const f = await setup(async () => delayedConsent.promise);
  const route = { provider_id: { open_ai_compatible: { name: 'openai-chatgpt' } },
    protocol: 'open_ai_responses' as const, base_url: 'https://chatgpt.com/backend-api/codex',
    model: 'test-model', max_output_tokens: 2048, preset_id: 'openai-chatgpt' };
  const expiresAt = Math.floor(Date.now() / 1000) + 3;
  await f.models.activateProvider(route, { kind: 'openai_oauth', access_token: 'short-lived-token', expires_at: expiresAt, fedramp: false },
    ['s2_planning'], 'keychain', 'oauth-connection');
  const replacement = f.models.authorize(request());
  await new Promise((done) => setTimeout(done, 10));
  delayedConsent.resolve(false);
  await replacement;
  assert.equal(f.models.authorization.phase, 'authorized');
  assert.equal(f.models.authorization.credential_kind, 'openai_oauth');
  const phase = (): ModelAuthorizationStatus['phase'] => f.models.authorization.phase;
  while (Date.now() < expiresAt * 1000 + 50) await new Promise((done) => setTimeout(done, 10));
  for (let i = 0; i < 100 && phase() !== 'not_configured'; i++) await new Promise((done) => setTimeout(done, 5));
  assert.equal(f.models.authorization.phase, 'not_configured');
  assert.equal(f.configs.at(-1)!.modelAuthorization, undefined);
  await f.shutdown();
});
test('a task, ambiguous cleanup and another authorization cannot be bypassed', async () => {
  const f = await setup();
  f.runs[0]!.projection.root = { root_task_id: 'root', root_epoch: 1, task_revision: 1, state: 'running', headline: '', attempts_used: 0, attempts_max: 4 };
  await assert.rejects(f.models.authorize(request()), /Finish or stop/);
  f.runs[0]!.projection.root = null; f.runs[0]!.projection.cleanup = 'unknown';
  await assert.rejects(f.models.authorize(request()), /Finish or stop/);
  assert.equal(f.runs.length, 1); await f.shutdown();
});
test('a stop closes pending consent and a late allow cannot authorize a key', async () => {
  const pending = deferred<boolean>(); let signal: AbortSignal | undefined;
  const f = await setup(async (_route, _storage, _purposes, value) => { signal = value; return pending.promise; });
  const result = f.models.authorize(request()); const rejected = assert.rejects(result, /cancelled/);
  assert.equal(f.models.authorization.phase, 'authorizing');
  assert.throws(() => f.session.operator({ command: 'create_task', text: 'must wait' }), /changing/);
  f.session.stopAtDevice({ scope: 'all' }, 'cancel', 'global_hotkey');
  assert.equal(signal?.aborted, true); pending.resolve(true); await rejected;
  assert.equal(f.runs.length, 1); assert.equal(f.models.authorization.phase, 'not_configured');
  assert.equal(f.models.authorization.reason, 'Model authorization cancelled.'); await f.shutdown();
});
test('revocation stops active work, closes old keys and restarts without model or window grants', async () => {
  const f = await setup(); await f.models.authorize(request());
  f.runs[1]!.projection.root = { root_task_id: 'root', root_epoch: 1, task_revision: 1, state: 'running', headline: '', attempts_used: 0, attempts_max: 4 };
  await f.models.revoke();
  assert(f.runs[1]!.stops > 0); assert(f.runs[1]!.commands.some((c) => c.command === 'stop_task'));
  assert.equal(f.configs[2]!.modelAuthorization, undefined); assert.equal(f.configs[2]!.mode, 'observe');
  assert.equal(f.models.authorization.route, null); assert.equal(f.models.authorization.phase, 'not_configured'); await f.shutdown();
});
test('a stop during host replacement never admits the pending model connection', async () => {
  const hold = deferred<void>(); const f = await setup(); f.runs[0]!.close = () => hold.promise;
  const result = f.models.authorize(request()); const rejected = assert.rejects(result);
  await new Promise((done) => setTimeout(done, 0));
  f.session.stopAtDevice({ scope: 'all' }, 'cancel', 'global_hotkey'); hold.resolve();
  await rejected;
  for (let i = 0; i < 100 && f.models.authorization.phase === 'switching'; i++) await new Promise((done) => setTimeout(done, 2));
  assert.equal(f.models.authorization.phase, 'not_configured');
  assert(f.configs.slice(1).every((c) => c.modelAuthorization === undefined)); await f.shutdown();
});
test('saved key is read only after consent, and never included in status', async () => {
  const saved = { id: 'a'.repeat(64), route: request().route }; let reads = 0;
  const store: ModelKeyStore = { available: true, list: async () => [saved], save: async () => saved,
    load: async () => { reads += 1; return { route: saved.route, api_key: KEY }; }, forget: async () => {} };
  const f = await setup(async () => false, store); await f.models.authorizeSaved(saved.id, ['s2_planning']); assert.equal(reads, 0); await f.shutdown();
  const yes = await setup(async () => true, store); await yes.models.authorizeSaved(saved.id, ['s2_planning']);
  assert.equal(reads, 1); assert(!JSON.stringify(yes.models.authorization).includes(KEY)); await yes.shutdown();
});
test('saved-key failures publish only finite Keychain diagnostics and never exception text', async () => {
  const saved = { id: 'a'.repeat(64), route: request().route };
  const cases: Array<[string, Error, string]> = [
    ['auth_required', new ModelKeyStoreFailure('auth_required'), 'Keychain authentication is required to read the saved API key. No new task was sent.'],
    ['locked', new ModelKeyStoreFailure('locked'), 'Keychain is locked or unavailable. No new task was sent.'],
    ['not_found', new ModelKeyStoreFailure('not_found'), 'The saved API key is missing from Keychain. Save it again before connecting. No new task was sent.'],
    ['invalid', new ModelKeyStoreFailure('invalid'), 'Keychain returned an invalid saved API key response. No new task was sent.'],
    ['unavailable', new ModelKeyStoreFailure('unavailable'), 'The saved API key could not be loaded from Keychain. No new task was sent.'],
    ['unknown', new Error(`native exception contains ${KEY}`), 'The saved API key could not be loaded from Keychain. No new task was sent.'],
  ];
  for (const [label, failure, reason] of cases) {
    const store: ModelKeyStore = { available: true, list: async () => [saved], save: async () => saved,
      load: async () => { throw failure; }, forget: async () => {} };
    const f = await setup(async () => true, store);
    await assert.rejects(f.models.authorizeSaved(saved.id, ['s2_planning']), /could not be opened/, label);
    const status = JSON.stringify(f.models.authorization);
    assert.equal(f.models.authorization.reason, reason, label);
    assert(!status.includes(KEY) && !status.includes('native exception'), label);
    assert.equal(f.runs.length, 1, label);
    await f.shutdown();
  }
});
test('failed key saving closes the newly opened host before reporting unavailable', async () => {
  const store: ModelKeyStore = { available: true, list: async () => [], save: async () => { throw new Error('secret-error-' + KEY); }, load: async () => null, forget: async () => {} };
  const f = await setup(async () => true, store); await assert.rejects(f.models.authorize({ ...request(), storage: 'keychain' }), /could not be opened/);
  assert.equal(f.configs.at(-1)!.modelAuthorization, undefined); assert(!JSON.stringify(f.models.authorization).includes(KEY));
  assert.equal(f.runs[1]!.closed, true); assert.equal(f.models.authorization.phase, 'unavailable');
  assert.equal(f.models.authorization.reason, 'The model connection could not be saved in Keychain. No new task was sent.');
  assert(!JSON.stringify(f.models.authorization).includes('secret-error')); await f.shutdown();
});
test('host replacement failures report their stage without exposing exception details', async () => {
  const f = await setup();
  f.session.restartForModelChange = async () => { throw new Error(`private host failure ${KEY}`); };
  await assert.rejects(f.models.authorize(request()), /could not be opened/);
  assert.equal(f.models.authorization.phase, 'unavailable');
  assert.equal(f.models.authorization.reason, 'The model host could not be configured. No new task was sent.');
  assert(!JSON.stringify(f.models.authorization).includes(KEY));
  assert.equal(f.runs.length, 1); await f.shutdown();
});
test('a changed saved route cannot reuse consent for another destination', async () => {
  const saved = { id: 'a'.repeat(64), route: request().route };
  const loaded = { route: { ...saved.route, base_url: 'https://changed.invalid/v1' }, api_key: KEY };
  const store: ModelKeyStore = { available: true, list: async () => [saved], save: async () => saved,
    load: async () => loaded, forget: async () => {} };
  const f = await setup(async () => true, store);
  await assert.rejects(f.models.authorizeSaved(saved.id, ['s2_planning']), /could not be opened/);
  assert.equal(loaded.api_key, ''); assert.equal(f.runs.length, 1);
  assert.equal(f.models.authorization.phase, 'not_configured'); await f.shutdown();
});
test('Forget waits for a cancelled save so a late write cannot restore the deleted key', async () => {
  const saved = { id: 'a'.repeat(64), route: request().route };
  const writing = deferred<void>(), entered = deferred<void>(); let exists = true;
  const order: string[] = [];
  const store: ModelKeyStore = { available: true, list: async () => exists ? [saved] : [],
    save: async () => { entered.resolve(); await writing.promise; exists = true; order.push('save'); return saved; },
    load: async () => null, forget: async () => { exists = false; order.push('forget'); } };
  const f = await setup(async () => true, store);
  const connecting = f.models.authorize({ ...request(), storage: 'keychain' });
  const rejected = assert.rejects(connecting, /cancelled/); await entered.promise;
  const forgetting = f.models.forgetSaved(saved.id);
  await new Promise(done => setTimeout(done, 0)); assert.deepEqual(order, []);
  writing.resolve(); await rejected; await forgetting;
  assert.deepEqual(order, ['save', 'forget']); assert.equal(f.models.authorization.saved.length, 0);
  assert.equal(f.models.authorization.phase, 'not_configured'); assert.equal(f.configs.at(-1)!.modelAuthorization, undefined);
  await f.shutdown();
});
test('purposes are explicit, nonempty, unique and strict at the invoke boundary', () => {
  for (const value of [
    { ...request(), purposes: undefined }, { ...request(), purposes: [] },
    { ...request(), purposes: ['s2_planning', 's2_planning'] }, { ...request(), purposes: ['s3_execute'] },
  ]) assert.throws(() => modelRequest(value), /Invalid/);
  assert.deepEqual(modelRequest({ ...request(), purposes: ['s1_teacher', 's2_planning'] }).purposes, ['s2_planning', 's1_teacher']);
});
test('teacher-only authorization is native-consented and does not enable S2 planning', async () => {
  let consented: string[] = [];
  const f = await setup(async (_route, _storage, purposes) => { consented = purposes; return true; });
  await f.models.authorize({ ...request(), purposes: ['s1_teacher'] });
  assert.deepEqual(consented, ['s1_teacher']);
  assert.deepEqual(f.configs.at(-1)!.modelAuthorization?.purposes, ['s1_teacher']);
  assert.deepEqual(f.models.authorization.authorized_purposes, ['s1_teacher']);
  assert.equal(f.models.authorization.planning_available, false);
  await f.models.revoke();
  assert.equal(f.configs.at(-1)!.modelAuthorization, undefined);
  assert.deepEqual(f.models.authorization.authorized_purposes, []); await f.shutdown();
});
test('computer proposals require explicit consent and use a fresh private scope per approval', async () => {
  const approved: ModelPurpose[] = [];
  let consentFacts = '';
  const f = await setup(async (route, storage, purposes) => {
    approved.push(...purposes); consentFacts = JSON.stringify({ route, storage, purposes }); return true;
  }, undefined, true);
  const computerRequest: ModelAuthorizationRequest = {
    ...request(),
    route: { ...request().route, base_url: 'https://api.openai.com/v1' },
    purposes: ['computer_proposals'],
  };
  await f.models.authorize(computerRequest);
  assert.deepEqual(approved, ['computer_proposals']);
  assert(!consentFacts.includes(KEY));
  assert(!consentFacts.includes('account_scope'));
  assert.equal(f.configs[1]!.modelAuthorization, undefined, 'computer-only authorization sends neither purpose nor key before window permission');
  assert.equal(f.configs[1]!.nativeSource?.authorizedWindowId, undefined);
  await f.session.authorizeNativeWindow('snap-1');
  assert.equal(f.configs[2]!.nativeSource?.authorizedWindowId, 'window-9');
  const first = f.configs[2]!.modelAuthorization!;
  const firstScope = first.account_scope;
  assert.match(firstScope ?? '', /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.notEqual(firstScope, KEY);
  assert.deepEqual(first.purposes, ['computer_proposals']);
  assert.deepEqual(f.models.authorization.authorized_purposes, ['computer_proposals']);
  assert(!JSON.stringify(f.models.authorization).includes(firstScope!));
  assert(!JSON.stringify(f.models.authorization).includes(KEY));
  assert.equal(f.models.supervisorOptions(f.configs[2]!).modelAuthorization?.account_scope, firstScope);

  await f.models.revoke();
  assert.equal(f.configs.at(-1)!.modelAuthorization, undefined);
  assert.equal(f.configs.at(-1)!.nativeSource?.authorizedWindowId, undefined);
  await f.models.authorize({
    ...computerRequest,
    purposes: ['s2_planning', 's1_teacher', 'computer_proposals'],
  });
  const beforeWindow = f.configs.at(-1)!.modelAuthorization;
  assert.deepEqual(beforeWindow?.purposes, ['s2_planning', 's1_teacher']);
  assert.equal(beforeWindow?.account_scope, undefined);
  assert.equal(f.configs.at(-1)!.nativeSource?.authorizedWindowId, undefined);
  await f.session.authorizeNativeWindow('snap-1');
  const nextAuthorization = f.configs.at(-1)!.modelAuthorization;
  assert.equal(f.configs.at(-1)!.nativeSource?.authorizedWindowId, 'window-9');
  const nextScope = nextAuthorization?.account_scope;
  assert.match(nextScope ?? '', /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.notEqual(nextScope, firstScope);
  assert.deepEqual(nextAuthorization?.purposes, ['s2_planning', 's1_teacher', 'computer_proposals']);
  await f.shutdown();
});
test('computer continuation scope is never persisted with a saved model key', async () => {
  const savedEntries: { route: ModelAuthorizationRequest['route']; key: string }[] = [];
  const store: ModelKeyStore = {
    available: true,
    list: async () => [],
    save: async (route, key) => {
      savedEntries.push({ route, key });
      return { id: 'c'.repeat(64), route };
    },
    load: async () => null,
    forget: async () => {},
  };
  const f = await setup(async () => true, store, true);
  await f.models.authorize({
    ...request(),
    route: { ...request().route, base_url: 'https://api.openai.com/v1' },
    purposes: ['computer_proposals'],
    storage: 'keychain',
  });
  assert.equal(savedEntries.length, 1);
  const stored = savedEntries[0]!;
  assert.equal(f.configs[1]!.modelAuthorization, undefined);
  await f.session.authorizeNativeWindow('snap-1');
  assert.deepEqual(Object.keys(stored).sort(), ['key', 'route']);
  assert(!JSON.stringify(stored).includes('account_scope'));
  assert(!JSON.stringify(stored).includes(f.configs.at(-1)!.modelAuthorization!.account_scope!));
  await f.shutdown();
});
test('Stop revokes computer scopes but preserves ordinary S2 and S1 authorization', async () => {
  for (const purpose of ['s2_planning', 's1_teacher'] as const) {
    const f = await setup();
    await f.models.authorize({ ...request(), purposes: [purpose] });
    const configurationCount = f.configs.length;
    f.session.stopAtDevice({ scope: 'all' }, 'cancel', 'operator_button');
    assert.equal(f.models.authorization.phase, 'authorized');
    assert.deepEqual(f.models.authorization.authorized_purposes, [purpose]);
    assert.equal(f.configs.length, configurationCount);
    await f.shutdown();
  }

  const f = await setup(async () => true, undefined, true);
  const computerRequest: ModelAuthorizationRequest = {
    ...request(),
    route: { ...request().route, base_url: 'https://api.openai.com/v1' },
    purposes: ['computer_proposals'],
  };
  await f.models.authorize(computerRequest);
  assert.equal(f.configs.at(-1)!.modelAuthorization, undefined);
  assert.equal(f.configs.at(-1)!.nativeSource?.authorizedWindowId, undefined);
  await f.session.authorizeNativeWindow('snap-1');
  const previousScope = f.configs.at(-1)!.modelAuthorization!.account_scope;
  f.session.stopAtDevice({ scope: 'all' }, 'cancel', 'operator_button');
  await f.models.revoke();
  assert.equal(f.models.authorization.phase, 'not_configured');
  assert.equal(f.configs.at(-1)!.modelAuthorization, undefined);
  assert.equal(f.configs.at(-1)!.nativeSource?.authorizedWindowId, undefined);
  await f.models.authorize(computerRequest);
  assert.equal(f.configs.at(-1)!.modelAuthorization, undefined);
  assert.equal(f.configs.at(-1)!.nativeSource?.authorizedWindowId, undefined);
  await f.session.authorizeNativeWindow('snap-1');
  assert.equal(f.configs.at(-1)!.nativeSource?.authorizedWindowId, 'window-9');
  const nextScope = f.configs.at(-1)!.modelAuthorization!.account_scope;
  assert.notEqual(nextScope, previousScope);
  await f.shutdown();
});
test('invalid/extra fields and insecure routes cannot reach consent', () => {
  for (const value of [null, {}, { ...request(), extra: KEY }, { ...request(), purposes: undefined }, { ...request(), api_key: '' },
    { ...request(), route: { ...request().route, api_key: KEY } }, { ...request(), route: { ...request().route, base_url: 'http://example.invalid' } },
    { ...request(), account_scope: 'renderer-supplied-scope' },
    { ...request(), purposes: ['computer_proposals'] },
    { ...request(), route: { ...request().route, base_url: 'https://compatible.invalid/v1' }, purposes: ['computer_proposals'] },
    { ...request(), route: { ...request().route, protocol: 'open_ai_chat' }, purposes: ['computer_proposals'] },
  ]) {
    assert.throws(() => modelRequest(value), /Invalid/);
  }
});
test('an exited process closes provider authorization before a new observation session', async () => {
  const f = await setup(); await f.models.authorize(request());
  f.runs[1]!.emit('exit', 'source', 1, null);
  for (let i = 0; i < 100 && f.models.authorization.phase === 'switching'; i++) await new Promise((done) => setTimeout(done, 2));
  assert.equal(f.models.authorization.phase, 'not_configured'); assert.equal(f.configs.at(-1)!.modelAuthorization, undefined);
  assert.equal(f.runs[1]!.closed, true); await f.shutdown();
});
