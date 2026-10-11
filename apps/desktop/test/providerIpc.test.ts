import assert from 'node:assert/strict';
import test from 'node:test';
import type { ProviderSettingsSnapshot, ModelAuthorizationStatus, OperatorCommand } from '@computer-use/desktop-api';
import { HostBridge, type ModelControl } from '../src/main/hostBridge.js';
import type { ProviderSettings } from '../src/main/providerSettings.js';
import type { StopBridge } from '../src/main/stopBridge.js';
import { registerIpc, type IpcEventLike } from '../src/main/ipc.js';
import { Channels } from '../src/shared/channels.js';

const route = { provider_id: 'open_ai' as const, protocol: 'open_ai_responses' as const,
  preset_id: 'openai', base_url: 'https://api.openai.com/v1', model: 'test-model', max_output_tokens: 4096 };
function fixture() {
  const commands: OperatorCommand[] = [], calls: unknown[] = [];
  const { model: _model, ...providerRoute } = route;
  const snapshot: ProviderSettingsSnapshot = { providers: [{ id: 'openai', label: 'OpenAI', description: 'Test',
    route: providerRoute, auth: 'api_key', models: [{ id: route.model, label: 'Test', selectable: true }], directory_supported: true }],
    connections: [{ id: 'connection', provider_id: 'openai', storage: 'memory', purposes: ['s2_planning'], available: true,
      models: [{ id: route.model, label: 'Test', selectable: true }], refreshed_at: null, catalog_source: 'sdk', reason: null }],
    selected: null, busy: false, can_configure: true, keychain_available: true, reason: null };
  const authorization: ModelAuthorizationStatus = { phase: 'authorized', route: { ...route }, storage: 'memory',
    credential_kind: 'api_key', authorized_purposes: ['s2_planning'], can_authorize: true, can_save: true,
    planning_available: true, saved: [], reason: null };
  const providers = { snapshot, configure: async (v: unknown) => { calls.push(v); }, refresh: async (v: unknown) => { calls.push(v); },
    remove: async (v: unknown) => { calls.push(v); }, select: async (v: unknown) => { calls.push(v); } } as unknown as ProviderSettings;
  const bridge = new HostBridge({ operator: (v) => { commands.push(v); }, projection: null },
    { press: () => {} } as unknown as StopBridge, undefined, undefined, { authorization, providerConnectionId: 'connection' } as ModelControl, undefined, providers);
  const handlers = new Map<string, (event: IpcEventLike, ...args: unknown[]) => unknown>();
  registerIpc({ handle: (c, h) => handlers.set(c, h), removeHandler: () => {} }, bridge, { files: ['/app/index.html'] });
  const good = { senderFrame: { url: 'file:///app/index.html' } };
  return { snapshot, authorization, bridge, commands, calls, handlers,
    call: (c: string, ...args: unknown[]) => handlers.get(c)!(good, ...args) };
}

test('Main requires exact selected provider, S2 purposes and credential state before task creation', () => {
  const f = fixture();
  assert.throws(() => f.bridge.createTask('test'), /Choose a connected provider/);
  assert.equal(f.commands.length, 0);
  f.snapshot.selected = { connection_id: 'connection', model: route.model };
  f.bridge.createTask('test');
  assert.equal(f.commands.length, 1);
  for (const patch of [{ route: { ...route, preset_id: 'other' } }, { route: { ...route, max_output_tokens: 2048 } }, { authorized_purposes: [] },
    { storage: 'keychain' }, { credential_kind: 'openai_oauth' }, { planning_available: false }]) {
    const before = { ...f.authorization };
    Object.assign(f.authorization, patch);
    assert.throws(() => f.bridge.createTask('test'), /Choose a connected provider/);
    Object.assign(f.authorization, before);
  }
  f.snapshot.connections[0]!.models[0]!.selectable = false;
  assert.throws(() => f.bridge.createTask('test'), /Choose a connected provider/);
  f.snapshot.connections[0]!.models[0]!.selectable = true;
  f.snapshot.connections[0]!.models = [];
  assert.throws(() => f.bridge.createTask('test'), /Choose a connected provider/);
  f.snapshot.connections[0]!.available = false;
  assert.throws(() => f.bridge.createTask('test'), /Choose a connected provider/);
  f.snapshot.connections = [];
  f.snapshot.selected = null;
  assert.throws(() => f.bridge.createTask('test'), /Choose a connected provider/, 'direct legacy authorization cannot bypass provider selection');
  f.authorization.phase = 'not_configured'; f.authorization.route = null; f.authorization.credential_kind = null;
  f.bridge.createTask('demo');
  f.snapshot.reason = 'Catalog unavailable';
  assert.throws(() => f.bridge.createTask('test'), /Choose a connected provider/);
  assert.equal(f.commands.length, 2);
});

test('provider IPC refuses injected route/model/secret fields before invoking the vault', async () => {
  const f = fixture(), config = { provider_id: 'openai', storage: 'memory', purposes: ['s2_planning'] };
  for (const v of [{ ...config, route }, { ...config, model: 'injected' }, { ...config, helper_path: '/tmp/evil' }, { ...config, refresh_token: 'must-not-cross-ipc' }, { ...config, reauthenticate: 'true' }]) {
    assert.throws(() => f.call(Channels.configureProvider, v), /invalid provider (settings|reauthentication)/);
  }
  assert.throws(() => f.call(Channels.selectTaskModel, { connection_id: 'connection', model: route.model, api_key: 'secret' }), /invalid task model/);
  assert.throws(() => f.call(Channels.providerSettings, 'extra'), /takes no arguments/);
  assert.throws(() => f.handlers.get(Channels.configureProvider)!({ senderFrame: { url: 'https://evil.invalid' } }, config), /untrusted/);
  assert.equal(f.calls.length, 0);
  await f.call(Channels.configureProvider, config);
  const relogin = { provider_id: 'openai-chatgpt', storage: 'keychain', purposes: ['s2_planning'], reauthenticate: true };
  await f.call(Channels.configureProvider, relogin);
  await f.call(Channels.selectTaskModel, { connection_id: 'connection', model: route.model });
  assert.deepEqual(f.calls, [config, relogin, { connection_id: 'connection', model: route.model }]);
});
