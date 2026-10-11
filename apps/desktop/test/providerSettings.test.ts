import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import test from 'node:test';
import type {
  ModelAuthorizationStatus,
  ModelPurpose,
  ModelProviderDefinition,
  ModelRoute,
  ProviderModel,
  SavedModelConnection,
} from '@computer-use/desktop-api';
import { ModelAccess, type ModelKeyStore } from '../src/main/modelAccess.js';
import { connectionId } from '../src/main/modelKeyStore.js';
import type { ModelCredential } from '../src/main/modelBootstrap.js';
import { ProviderSettings, type ProviderCredential } from '../src/main/providerSettings.js';
import type { OpenAiOAuthRefresh } from '../src/main/openAiLogin.js';

const API_KEY_A = 'provider-a-test-key';
const API_KEY_B = 'provider-b-test-key';
const OAUTH_TOKEN = 'provider-oauth-test-token';
const PURPOSES: ModelPurpose[] = ['s2_planning'];

const providerA: ModelProviderDefinition = {
  id: 'provider-a', label: 'OpenAI', description: 'OpenAI API provider',
  route: { provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://api.openai.com/v1', max_output_tokens: 2048, preset_id: 'provider-a' },
  auth: 'api_key', models: [{ id: 'gpt-test-a', label: 'GPT Test A', selectable: true },
    { id: 'not-selectable', label: 'Hidden Model', selectable: false }], directory_supported: true,
};
const providerB: ModelProviderDefinition = {
  id: 'provider-b', label: 'Anthropic', description: 'Anthropic provider',
  route: { provider_id: 'anthropic_first_party', protocol: 'anthropic_messages', base_url: 'https://api.anthropic.com/v1', max_output_tokens: 2048, preset_id: 'provider-b' },
  auth: 'api_key', models: [{ id: 'claude-test-b', label: 'Claude Test B', selectable: true }], directory_supported: false,
};
const openAiOAuth: ModelProviderDefinition = {
  id: 'openai-chatgpt', label: 'OpenAI account', description: 'OpenAI account sign-in',
  route: { provider_id: { open_ai_compatible: { name: 'openai-chatgpt' } }, protocol: 'open_ai_responses',
    base_url: 'https://chatgpt.com/backend-api/codex', max_output_tokens: 2048, preset_id: 'openai-chatgpt' },
  auth: 'openai_oauth', models: [{ id: 'gpt-account-test', label: 'Account model', selectable: true }], directory_supported: false,
};

class Store implements ModelKeyStore {
  available = true;
  readonly rows: SavedModelConnection[] = [];
  readonly keys = new Map<string, string>();
  readonly calls: string[] = [];
  async list(): Promise<SavedModelConnection[]> { this.calls.push('list'); return structuredClone(this.rows); }
  async save(route: ModelRoute, key: string): Promise<SavedModelConnection> {
    this.calls.push('save');
    const saved = { id: connectionId(route), route: structuredClone(route) };
    this.keys.set(saved.id, key);
    const index = this.rows.findIndex((row) => row.id === saved.id);
    if (index >= 0) this.rows.splice(index, 1);
    this.rows.push(saved);
    return structuredClone(saved);
  }
  async load(id: string): Promise<{ route: ModelRoute; api_key: string } | null> {
    this.calls.push('load');
    const row = this.rows.find((item) => item.id === id);
    const key = this.keys.get(id);
    return row && key ? { route: structuredClone(row.route), api_key: key } : null;
  }
  async forget(id: string): Promise<void> {
    this.calls.push('forget');
    this.keys.delete(id);
    const index = this.rows.findIndex((row) => row.id === id);
    if (index >= 0) this.rows.splice(index, 1);
  }
}

class ModelStub extends EventEmitter {
  authorization: ModelAuthorizationStatus = {
    phase: 'not_configured', route: null, credential_kind: null, storage: null,
    authorized_purposes: [], can_authorize: true, can_save: true, planning_available: false,
    saved: [], reason: null,
  };
  providerConnectionId: string | null = null;
  readonly activations: Array<{ route: ModelRoute; credential: ModelCredential; purposes: ModelPurpose[] }> = [];
  revoked = 0;
  async activateProvider(route: ModelRoute, credential: ModelCredential, purposes: ModelPurpose[], storage: 'memory' | 'keychain', providerConnectionId: string): Promise<void> {
    this.activations.push({ route: structuredClone(route), credential: structuredClone(credential), purposes: [...purposes] });
    this.providerConnectionId = providerConnectionId;
    this.authorization = { ...this.authorization, phase: 'authorized', route: structuredClone(route),
      credential_kind: credential.kind, storage, authorized_purposes: [...purposes] };
    this.emit('status', this.authorization);
  }
  async revoke(): Promise<void> {
    this.revoked += 1;
    this.providerConnectionId = null;
    this.authorization = { ...this.authorization, phase: 'not_configured', route: null,
      credential_kind: null, storage: null, authorized_purposes: [] };
    this.emit('status', this.authorization);
  }
}

function setup(options: {
  providers?: ModelProviderDefinition[];
  store?: Store;
  confirm?: (provider: unknown, storage: string, purposes: ModelPurpose[], signal: AbortSignal, kind: string) => Promise<boolean>;
  refresh?: (provider: ModelProviderDefinition, credential: ModelCredential, signal: AbortSignal) => Promise<ProviderModel[]>;
  login?: (provider: ModelProviderDefinition, signal: AbortSignal) => Promise<ProviderCredential>;
  refreshOAuth?: OpenAiOAuthRefresh;
} = {}) {
  const models = new ModelStub();
  const store = options.store ?? new Store();
  const events: string[] = [];
  const settings = new ProviderSettings(
    models as unknown as ModelAccess,
    store,
    async () => options.providers ?? [providerA, providerB, openAiOAuth],
    async (provider, credential, signal) => {
      events.push(`refresh:${provider.id}`);
      return options.refresh ? options.refresh(provider, credential, signal) : provider.models;
    },
    async (provider, signal) => {
      events.push(`login:${provider.id}`);
      return options.login ? options.login(provider, signal) : {
        kind: 'openai_oauth', access_token: OAUTH_TOKEN,
        expires_at: Math.floor(Date.now() / 1000) + 600, fedramp: false,
      };
    },
    async (provider, storage, purposes, signal, kind) => {
      events.push(`consent:${(provider as { id: string }).id}`);
      return options.confirm ? options.confirm(provider, storage, purposes, signal, kind) : true;
    },
    options.refreshOAuth,
  );
  return { settings, store, models, events };
}

test('initialization loads only SDK catalog and saved metadata without reading credentials', async () => {
  const store = new Store();
  const anchor = { ...providerA.route, model: '__provider_credentials__' } as ModelRoute;
  const saved = { id: connectionId(anchor), route: anchor };
  store.rows.push(saved); store.keys.set(saved.id, API_KEY_A);
  const f = setup({ store });
  await f.settings.initialize();
  assert.equal(f.store.calls.includes('load'), false);
  const restored = f.settings.snapshot.connections.find((item) => item.id === saved.id)!;
  assert.equal(restored.available, false);
  assert.deepEqual(restored.models, providerA.models);
  assert.equal(restored.catalog_source, 'sdk');
  assert(!JSON.stringify(f.settings.snapshot).includes(API_KEY_A));
  assert(!('route' in restored));
  await f.settings.shutdown();
});

test('multi-provider configuration is consent-first, does not activate a model, and projects no credentials', async () => {
  const f = setup();
  await f.settings.initialize();
  await f.settings.configure({ provider_id: providerA.id, api_key: API_KEY_A, storage: 'memory', purposes: PURPOSES });
  await f.settings.configure({ provider_id: providerB.id, api_key: API_KEY_B, storage: 'memory', purposes: ['s1_teacher'] });
  assert.deepEqual(f.events, [
    'consent:provider-a', 'refresh:provider-a', 'consent:provider-b',
  ]);
  assert.equal(f.models.activations.length, 0, 'configuration never selects or restarts a model host');
  const snapshot = f.settings.snapshot;
  assert.equal(snapshot.connections.length, 2);
  assert(snapshot.connections.every((connection) => connection.available));
  assert(!JSON.stringify(snapshot).includes(API_KEY_A));
  assert(!JSON.stringify(snapshot).includes(API_KEY_B));
  assert(snapshot.connections.every((connection) => !('route' in connection) && !('credential' in connection)));
  await f.settings.shutdown();
});

test('only a connection and currently selectable SDK model can activate the frozen provider route', async () => {
  let approved = true;
  const f = setup({ confirm: async () => approved });
  await f.settings.initialize();
  await f.settings.configure({ provider_id: providerA.id, api_key: API_KEY_A, storage: 'memory', purposes: PURPOSES });
  const id = f.settings.snapshot.connections[0]!.id;
  await assert.rejects(f.settings.select({ connection_id: id, model: 'not-selectable' }), /could not be activated/);
  await assert.rejects(f.settings.select({ connection_id: 'tampered-id', model: 'gpt-test-a' }), /could not be activated/);
  assert.equal(f.models.activations.length, 0);
  await f.settings.select({ connection_id: id, model: 'gpt-test-a' });
  assert.equal(f.models.activations.length, 1);
  assert.deepEqual(f.models.activations[0]!.route, { ...providerA.route, model: 'gpt-test-a' });
  assert.equal(f.models.providerConnectionId, id);
  assert.deepEqual(f.settings.snapshot.selected, { connection_id: id, model: 'gpt-test-a' });
  approved = false;
  await assert.rejects(f.settings.configure({ provider_id: providerA.id, api_key: 'replacement-key', storage: 'memory', purposes: PURPOSES }), /cancelled/);
  assert.equal(f.models.providerConnectionId, id);
  assert.deepEqual(f.settings.snapshot.selected, { connection_id: id, model: 'gpt-test-a' });

  // A legacy Main authorization may have the same route, storage and purposes,
  // but it no longer points at this vault record and must clear the selection.
  f.models.authorization = { ...f.models.authorization, route: { ...providerA.route, model: 'gpt-test-a' },
    storage: 'memory', authorized_purposes: [...PURPOSES] };
  f.models.providerConnectionId = null;
  f.models.emit('status', f.models.authorization);
  assert.equal(f.settings.snapshot.selected, null);
  assert(!JSON.stringify(f.settings.snapshot).includes(API_KEY_A));
  await f.settings.shutdown();
});

test('Keychain credentials are loaded only after provider purpose consent', async () => {
  const store = new Store();
  const first = setup({ store });
  await first.settings.initialize();
  await first.settings.configure({ provider_id: providerA.id, api_key: API_KEY_A, storage: 'keychain', purposes: PURPOSES });
  const savedId = first.settings.snapshot.connections[0]!.id;
  await first.settings.shutdown();
  store.calls.length = 0;

  const second = setup({ store });
  await second.settings.initialize();
  assert.equal(store.calls.includes('load'), false);
  const unavailable = second.settings.snapshot.connections.find((item) => item.id === savedId)!;
  assert.equal(unavailable.available, false);
  await second.settings.configure({ provider_id: providerA.id, storage: 'keychain', purposes: ['s2_planning'] });
  assert.deepEqual(store.calls.slice(0, 2), ['list', 'load']);
  assert.equal(second.settings.snapshot.connections.find((item) => item.id === savedId)!.available, true);
  assert(second.events.indexOf('consent:provider-a') < second.events.indexOf('refresh:provider-a'));
  await second.settings.select({ connection_id: savedId, model: 'gpt-test-a' });
  assert.equal(second.models.authorization.storage, 'keychain');
  assert(!JSON.stringify(second.settings.snapshot).includes(API_KEY_A));
  await second.settings.shutdown();
});

test('invalid provider route and selection payloads cannot reach consent or activation', async () => {
  let consents = 0;
  const f = setup({ confirm: async () => { consents += 1; return true; } });
  await f.settings.initialize();
  const request = { provider_id: providerA.id, api_key: API_KEY_A, storage: 'memory', purposes: PURPOSES,
    route: { ...providerA.route, base_url: 'https://attacker.invalid' } } as never;
  await assert.rejects(f.settings.configure(request), /Invalid provider configuration/);
  assert.equal(consents, 0);
  assert.equal(f.settings.snapshot.connections.length, 0);
  await f.settings.shutdown();
});

test('Stop aborts pending provider consent, scrubs the request key, and leaves the vault unchanged', async () => {
  let signal: AbortSignal | undefined;
  const f = setup({ confirm: async (_provider, _storage, _purposes, receivedSignal) => {
    signal = receivedSignal;
    return new Promise<boolean>((_resolve, reject) => {
      receivedSignal.addEventListener('abort', () => reject(new Error('dialog cancelled')), { once: true });
    });
  } });
  await f.settings.initialize();
  const request = { provider_id: providerA.id, api_key: API_KEY_A, storage: 'memory', purposes: PURPOSES } as const;
  const operation = f.settings.configure(request);
  await new Promise((done) => setTimeout(done, 0));
  assert.equal(f.settings.snapshot.busy, true);
  f.models.emit('configurationStop');
  await assert.rejects(operation, /cancelled/);
  assert.equal(signal?.aborted, true);
  assert.equal(f.settings.snapshot.connections.length, 0);
  assert(!JSON.stringify(f.settings.snapshot).includes(API_KEY_A));
  await f.settings.shutdown();
});

test('OpenAI OAuth sessions persist in Keychain, refresh before activation, and keep refresh tokens Main-only', async () => {
  const refreshes: string[] = [];
  const f = setup({ providers: [openAiOAuth], login: async () => ({
    kind: 'openai_oauth', access_token: OAUTH_TOKEN,
    refresh_token: 'provider-oauth-refresh-token',
    expires_at: Math.floor(Date.now() / 1000) + 1, fedramp: false,
  }), refreshOAuth: async (session) => {
    refreshes.push(session.refresh_token ?? '');
    return { ...session, access_token: 'provider-oauth-refreshed-access',
      refresh_token: 'provider-oauth-rotated-refresh', expires_at: Math.floor(Date.now() / 1000) + 600 };
  } });
  await f.settings.initialize();
  await f.settings.configure({ provider_id: openAiOAuth.id, storage: 'keychain', purposes: PURPOSES });
  assert.equal(f.store.calls.includes('save'), true);
  const connection = f.settings.snapshot.connections[0]!;
  const stored = JSON.parse(f.store.keys.get(connection.id)!) as { refresh_token?: string };
  assert.equal(stored.refresh_token, 'provider-oauth-refresh-token');
  await f.settings.select({ connection_id: connection.id, model: 'gpt-account-test' });
  assert(!JSON.stringify(f.settings.snapshot).includes(OAUTH_TOKEN));
  assert.deepEqual(refreshes, ['provider-oauth-refresh-token']);
  assert.equal(f.models.activations[0]!.credential.kind, 'openai_oauth');
  assert(!('refresh_token' in f.models.activations[0]!.credential));
  const rotated = JSON.parse(f.store.keys.get(connection.id)!) as { refresh_token?: string };
  assert.equal(rotated.refresh_token, 'provider-oauth-rotated-refresh');
  assert.equal(f.settings.snapshot.connections[0]!.available, true);
  await f.settings.remove(connection.id);
  assert.equal(f.store.keys.has(connection.id), false);
  assert.equal(f.settings.snapshot.connections.length, 0);
  await f.settings.shutdown();
});

test('Stop cancels a pending OAuth login and permits a fresh sign-in', async () => {
  let attempts = 0;
  let entered!: () => void;
  const ready = new Promise<void>((resolve) => { entered = resolve; });
  const f = setup({ login: async (_provider, signal) => {
    if (++attempts === 1) {
      entered();
      return new Promise<ModelCredential>((_resolve, reject) => {
        signal.addEventListener('abort', () => reject(new Error('cancelled')), { once: true });
      });
    }
    return { kind: 'openai_oauth', access_token: OAUTH_TOKEN, expires_at: Math.floor(Date.now() / 1000) + 600, fedramp: false };
  } });
  await f.settings.initialize();
  const request = { provider_id: openAiOAuth.id, storage: 'keychain' as const, purposes: ['s2_planning' as const] };
  const first = f.settings.configure(request);
  const rejected = assert.rejects(first);
  await ready;
  f.models.emit('configurationStop');
  await rejected;
  assert.equal(f.settings.snapshot.busy, false);
  assert.equal(f.settings.snapshot.connections.length, 0);
  await f.settings.configure(request);
  assert.equal(attempts, 2);
  assert.equal(f.settings.snapshot.connections[0]?.available, true);
  await f.settings.shutdown();
});
