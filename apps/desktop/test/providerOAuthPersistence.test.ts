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
import type { ModelCredential, OpenAiOAuthSession } from '../src/main/modelBootstrap.js';
import type { OpenAiOAuthRefresh } from '../src/main/openAiLogin.js';
import { ProviderSettings } from '../src/main/providerSettings.js';

const ACCESS_A = 'oauth-access-session-a';
const ACCESS_B = 'oauth-access-session-b';
const REFRESH_A = 'oauth-refresh-session-a';
const REFRESH_B = 'oauth-refresh-session-b';
const PURPOSES: ModelPurpose[] = ['s2_planning'];
const model: ProviderModel = { id: 'gpt-account-test', label: 'Account model', selectable: true };
const openAiOAuth: ModelProviderDefinition = {
  id: 'openai-chatgpt', label: 'OpenAI account', description: 'OpenAI account sign-in',
  route: {
    provider_id: { open_ai_compatible: { name: 'openai-chatgpt' } },
    protocol: 'open_ai_responses',
    base_url: 'https://chatgpt.com/backend-api/codex',
    max_output_tokens: 2048,
    preset_id: 'openai-chatgpt',
  },
  auth: 'openai_oauth', models: [model], directory_supported: false,
};

function accountSession(
  accessToken: string,
  refreshToken: string,
  expiresAt = Math.floor(Date.now() / 1000) + 600,
): OpenAiOAuthSession {
  return {
    kind: 'openai_oauth', access_token: accessToken, expires_at: expiresAt,
    refresh_token: refreshToken, fedramp: false,
  };
}

function storageRoute(): ModelRoute {
  return { ...openAiOAuth.route, model: '__provider_credentials__' };
}

class KeychainStore implements ModelKeyStore {
  available = true;
  readonly rows: SavedModelConnection[] = [];
  readonly values = new Map<string, string>();
  readonly calls: string[] = [];
  failSave = false;

  async list(): Promise<SavedModelConnection[]> {
    this.calls.push('list');
    return structuredClone(this.rows);
  }

  async save(route: ModelRoute, apiKey: string): Promise<SavedModelConnection> {
    this.calls.push('save');
    if (this.failSave) throw new Error('keychain write failed');
    const saved = { id: connectionId(route), route: structuredClone(route) };
    this.values.set(saved.id, apiKey);
    const previous = this.rows.findIndex((row) => row.id === saved.id);
    if (previous >= 0) this.rows.splice(previous, 1);
    this.rows.push(saved);
    return structuredClone(saved);
  }

  async load(id: string): Promise<{ route: ModelRoute; api_key: string } | null> {
    this.calls.push('load');
    const row = this.rows.find((item) => item.id === id);
    const value = this.values.get(id);
    return row && value ? { route: structuredClone(row.route), api_key: value } : null;
  }

  async forget(id: string): Promise<void> {
    this.calls.push('forget');
    this.values.delete(id);
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

  async activateProvider(
    route: ModelRoute,
    credential: ModelCredential,
    purposes: ModelPurpose[],
    storage: 'memory' | 'keychain',
    providerConnectionId: string,
  ): Promise<void> {
    this.activations.push({ route: structuredClone(route), credential: structuredClone(credential), purposes: [...purposes] });
    this.providerConnectionId = providerConnectionId;
    this.authorization = {
      ...this.authorization,
      phase: 'authorized', route: structuredClone(route), credential_kind: credential.kind,
      storage, authorized_purposes: [...purposes],
    };
    this.emit('status', this.authorization);
  }

  async revoke(): Promise<void> {
    this.revoked += 1;
    this.providerConnectionId = null;
    this.authorization = {
      ...this.authorization,
      phase: 'not_configured', route: null, credential_kind: null, storage: null, authorized_purposes: [],
    };
    this.emit('status', this.authorization);
  }
}

function fixture(options: {
  store?: KeychainStore;
  login?: (provider: ModelProviderDefinition, signal: AbortSignal) => Promise<ModelCredential>;
  refreshOAuth?: OpenAiOAuthRefresh;
} = {}) {
  const models = new ModelStub();
  const store = options.store ?? new KeychainStore();
  const events: string[] = [];
  let loginCalls = 0;
  const settings = new ProviderSettings(
    models as unknown as ModelAccess,
    store,
    async () => [openAiOAuth],
    async (_provider, _credential, _signal) => [model],
    async (provider, signal) => {
      loginCalls += 1;
      events.push(`login:${provider.id}`);
      return options.login
        ? options.login(provider, signal)
        : accountSession(ACCESS_A, REFRESH_A);
    },
    async (provider, _storage, _purposes, _signal, _credentialKind) => {
      events.push(`consent:${provider.id}`);
      return true;
    },
    options.refreshOAuth,
  );
  return { settings, store, models, events, loginCalls: () => loginCalls };
}

function seedSession(store: KeychainStore, session: OpenAiOAuthSession): string {
  const route = storageRoute();
  const id = connectionId(route);
  store.rows.push({ id, route });
  store.values.set(id, JSON.stringify(session));
  return id;
}

function configureRequest(overrides: { reauthenticate?: boolean } = {}) {
  return {
    provider_id: openAiOAuth.id,
    storage: 'keychain' as const,
    purposes: [...PURPOSES],
    ...overrides,
  };
}

test('OAuth browser login saves the full session in Keychain without publishing it or activating Host', async () => {
  const f = fixture();
  try {
    await f.settings.initialize();
    await f.settings.configure(configureRequest());

    const id = connectionId(storageRoute());
    const saved = JSON.parse(f.store.values.get(id) ?? 'null') as OpenAiOAuthSession | null;
    assert.equal(saved?.access_token, ACCESS_A);
    assert.equal(saved?.refresh_token, REFRESH_A);
    assert.deepEqual(f.events, ['consent:openai-chatgpt', 'login:openai-chatgpt']);
    assert.equal(f.store.calls.filter((call) => call === 'save').length, 1);
    assert.equal(f.settings.snapshot.connections[0]?.storage, 'keychain');
    assert.equal(f.settings.snapshot.connections[0]?.available, true);
    assert.equal(f.models.activations.length, 0, 'provider setup does not activate the Host');
    assert(!JSON.stringify(f.settings.snapshot).includes(ACCESS_A));
    assert(!JSON.stringify(f.settings.snapshot).includes(REFRESH_A));
  } finally {
    await f.settings.shutdown();
  }
});

test('fresh initialization restores saved OAuth metadata without loading the Keychain secret', async () => {
  const store = new KeychainStore();
  const id = seedSession(store, accountSession(ACCESS_A, REFRESH_A));
  const f = fixture({ store });
  try {
    await f.settings.initialize();

    const restored = f.settings.snapshot.connections.find((connection) => connection.id === id);
    assert.equal(store.calls.filter((call) => call === 'list').length, 1);
    assert.equal(store.calls.includes('load'), false);
    assert.equal(restored?.storage, 'keychain');
    assert.equal(restored?.available, false);
    assert.deepEqual(restored?.purposes, []);
    assert(!JSON.stringify(f.settings.snapshot).includes(ACCESS_A));
    assert(!JSON.stringify(f.settings.snapshot).includes(REFRESH_A));
  } finally {
    await f.settings.shutdown();
  }
});

test('purpose-approved reconnect reuses a saved OAuth session without browser login', async () => {
  const store = new KeychainStore();
  const id = seedSession(store, accountSession(ACCESS_A, REFRESH_A));
  const f = fixture({ store });
  try {
    await f.settings.initialize();
    store.calls.length = 0;
    await f.settings.configure(configureRequest());

    assert.deepEqual(f.events, ['consent:openai-chatgpt']);
    assert.equal(f.loginCalls(), 0);
    assert.deepEqual(store.calls, ['load']);
    assert.equal(f.settings.snapshot.connections.find((connection) => connection.id === id)?.available, true);
    assert.equal(f.models.activations.length, 0);
  } finally {
    await f.settings.shutdown();
  }
});

test('near-expiry reconnect rotates the saved refresh session and gives Host access-only credentials', async () => {
  const store = new KeychainStore();
  const id = seedSession(store, accountSession(ACCESS_A, REFRESH_A, Math.floor(Date.now() / 1000) + 60));
  let refreshCalls = 0;
  const f = fixture({
    store,
    refreshOAuth: async (session) => {
      refreshCalls += 1;
      assert.equal(session.refresh_token, REFRESH_A);
      return accountSession(ACCESS_B, REFRESH_B);
    },
  });
  try {
    await f.settings.initialize();
    const oldSavedValue = f.store.values.get(id);
    await f.settings.configure(configureRequest());

    assert.equal(refreshCalls, 1);
    assert.equal(f.loginCalls(), 0);
    const persisted = JSON.parse(f.store.values.get(id) ?? 'null') as OpenAiOAuthSession | null;
    assert.equal(persisted?.access_token, ACCESS_B);
    assert.equal(persisted?.refresh_token, REFRESH_B);
    assert.notEqual(f.store.values.get(id), oldSavedValue);

    await f.settings.select({ connection_id: id, model: model.id });
    const activated = f.models.activations[0]?.credential;
    assert.equal(activated?.kind, 'openai_oauth');
    assert.equal(activated?.access_token, ACCESS_B);
    assert(!('refresh_token' in (activated ?? {})), 'refresh token must stay in Main and Keychain');
    assert(!JSON.stringify(f.settings.snapshot).includes(ACCESS_B));
    assert(!JSON.stringify(f.settings.snapshot).includes(REFRESH_B));
  } finally {
    await f.settings.shutdown();
  }
});

test('Stop during a dispatched refresh saves rotation without marking the saved account connected', async () => {
  const store = new KeychainStore();
  const id = seedSession(store, accountSession(ACCESS_A, REFRESH_A, Math.floor(Date.now() / 1000) + 60));
  let enterRefresh!: () => void;
  let resolveRefresh!: (session: OpenAiOAuthSession) => void;
  let refreshSignal: AbortSignal | undefined;
  const refreshStarted = new Promise<void>((resolve) => { enterRefresh = resolve; });
  const refreshResult = new Promise<OpenAiOAuthSession>((resolve) => { resolveRefresh = resolve; });
  const f = fixture({
    store,
    refreshOAuth: async (_session, signal) => {
      refreshSignal = signal;
      enterRefresh();
      // Like the bounded SDK helper, settle an already-dispatched refresh even
      // after Stop so its rotated one-use refresh token can be persisted.
      return refreshResult;
    },
  });
  try {
    await f.settings.initialize();
    store.calls.length = 0;
    const operation = f.settings.configure(configureRequest());
    const rejected = assert.rejects(operation, /cancelled/i);
    await refreshStarted;

    f.models.emit('configurationStop');
    assert.equal(refreshSignal?.aborted, true);
    resolveRefresh(accountSession(ACCESS_B, REFRESH_B));
    await rejected;

    const persisted = JSON.parse(store.values.get(id) ?? 'null') as OpenAiOAuthSession | null;
    assert.equal(persisted?.access_token, ACCESS_B);
    assert.equal(persisted?.refresh_token, REFRESH_B);
    assert.deepEqual(store.calls, ['load', 'save']);
    assert.equal(f.loginCalls(), 0);
    assert.equal(f.models.activations.length, 0);
    assert.equal(f.models.providerConnectionId, null);
    assert.equal(f.settings.snapshot.selected, null);
    const savedConnection = f.settings.snapshot.connections.find((connection) => connection.id === id);
    assert.equal(savedConnection?.storage, 'keychain');
    assert.equal(savedConnection?.available, false);
    assert.match(savedConnection?.reason ?? '', /reconnect|review/i);
    assert(!JSON.stringify(f.settings.snapshot).includes(ACCESS_B));
    assert(!JSON.stringify(f.settings.snapshot).includes(REFRESH_B));
  } finally {
    await f.settings.shutdown();
  }
});

test('cancelled reauthentication leaves the prior saved OAuth session intact', async () => {
  const store = new KeychainStore();
  const id = seedSession(store, accountSession(ACCESS_A, REFRESH_A));
  const previousValue = store.values.get(id);
  let enterLogin!: () => void;
  const loginStarted = new Promise<void>((resolve) => { enterLogin = resolve; });
  const f = fixture({
    store,
    login: async (_provider, signal) => {
      enterLogin();
      return new Promise<ModelCredential>((_resolve, reject) => {
        signal.addEventListener('abort', () => reject(new Error('cancelled by Stop')), { once: true });
      });
    },
  });
  try {
    await f.settings.initialize();
    store.calls.length = 0;
    const operation = f.settings.configure(configureRequest({ reauthenticate: true }));
    const rejected = assert.rejects(operation, /cancelled/i);
    await loginStarted;
    f.models.emit('configurationStop');
    await rejected;

    assert.equal(f.loginCalls(), 1);
    assert.equal(store.values.get(id), previousValue);
    assert.equal(store.calls.filter((call) => call === 'save').length, 0);
    assert.equal(store.calls.filter((call) => call === 'forget').length, 0);
    assert.equal(f.settings.snapshot.connections.find((connection) => connection.id === id)?.storage, 'keychain');
  } finally {
    await f.settings.shutdown();
  }
});

test('signing out removes the saved OAuth session and its projected connection', async () => {
  const store = new KeychainStore();
  const id = seedSession(store, accountSession(ACCESS_A, REFRESH_A));
  const f = fixture({ store });
  try {
    await f.settings.initialize();
    assert.equal(f.store.values.has(id), true);
    await f.settings.remove(id);

    assert.equal(f.store.values.has(id), false);
    assert.equal(f.store.rows.some((row) => row.id === id), false);
    assert.equal(f.store.calls.filter((call) => call === 'forget').length, 1);
    assert.equal(f.settings.snapshot.connections.length, 0);
  } finally {
    await f.settings.shutdown();
  }
});

test('a failed Keychain write does not create a connected OAuth provider', async () => {
  const store = new KeychainStore();
  store.failSave = true;
  const f = fixture({ store });
  try {
    await f.settings.initialize();
    await assert.rejects(f.settings.configure(configureRequest()), /could not be completed/i);

    assert.equal(f.settings.snapshot.connections.length, 0);
    assert.equal(f.store.rows.length, 0);
    assert.equal(f.store.values.size, 0);
    assert.equal(f.models.activations.length, 0);
    assert.equal(f.models.providerConnectionId, null);
    assert(!JSON.stringify(f.settings.snapshot).includes(ACCESS_A));
    assert(!JSON.stringify(f.settings.snapshot).includes(REFRESH_A));
  } finally {
    await f.settings.shutdown();
  }
});
