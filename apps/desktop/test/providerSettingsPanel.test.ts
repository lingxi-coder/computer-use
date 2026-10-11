/// <reference path="../src/renderer/env.d.ts" />

import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ProviderSettingsSnapshot } from '@computer-use/desktop-api';
import { ProviderSettingsPanel } from '../src/renderer/ProviderSettingsPanel.js';
import {
  isCurrentCredentialTransaction,
  keychainConfigureProviderRequest,
  persistProviderCredentialInput,
  runCurrentProviderTransaction,
} from '../src/renderer/electron-settings/providerSettingsLogic.js';

const model = (id: string, selectable = true) => ({ id, label: id, selectable });

function settings(auth: 'api_key' | 'openai_oauth' = 'api_key'): ProviderSettingsSnapshot {
  return {
    providers: [{
      id: 'openai', label: 'OpenAI SDK label', description: 'Provider description from the SDK.',
      route: { preset_id: 'openai-responses-standard', provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://api.openai.com/v1', max_output_tokens: 4096 },
      auth, models: [model('sdk-model')], directory_supported: true,
    }],
    connections: [{
      id: 'connection-1', provider_id: 'openai', storage: 'keychain',
      purposes: ['s2_planning'], available: true,
      models: [model('live-model'), model('retired-model', false)], refreshed_at: 1_791_600_000_000,
      catalog_source: 'live', reason: null,
    }],
    selected: { connection_id: 'connection-1', model: 'live-model' },
    busy: false, can_configure: true, keychain_available: true, reason: null,
  };
}

const props = (snapshot: ProviderSettingsSnapshot) => ({
  snapshot,
  loadError: null,
  configure: async () => undefined,
  refreshModels: async () => undefined,
  removeConnection: async () => undefined,
  cancelSignIn: async () => undefined,
});

test('provider settings open on a list and use the Electron provider-row layout', () => {
  const snapshot = settings();
  snapshot.providers.push({
    id: 'other-sdk-profile', label: 'Other provider profile', description: 'Another SDK profile.',
    route: { preset_id: 'other-sdk-profile', provider_id: { custom: { name: 'other-provider' } }, protocol: 'open_ai_responses', base_url: 'https://example.test/v1', max_output_tokens: 4096 },
    auth: 'api_key', models: [model('other-sdk-model')], directory_supported: true,
  });
  snapshot.connections.push({
    id: 'connection-2', provider_id: 'other-sdk-profile', storage: 'memory', purposes: ['s2_planning'], available: true,
    models: [model('other-live-model')], refreshed_at: 1_791_600_000_000, catalog_source: 'live', reason: null,
  });

  const html = renderToStaticMarkup(createElement(ProviderSettingsPanel, props(snapshot)));
  assert.match(html, /Provider connections/);
  assert.match(html, /provider-settings-card-body/);
  assert.match(html, /provider-settings-provider-row/);
  assert.match(html, /OpenAI SDK label/);
  assert.match(html, /Other provider profile/);
  assert.match(html, /Connected/);
  assert.doesNotMatch(html, /Provider API key|Model directory|live-model/);
  assert.doesNotMatch(html, /aria-label="Task model"/);
});

test('provider detail keeps configuration controls and shows only the newest read-only catalog', () => {
  const snapshot = settings();
  snapshot.connections.push({
    id: 'connection-newest', provider_id: 'openai', storage: 'memory', purposes: ['s2_planning', 's1_teacher'], available: true,
    models: [model('current-model')], refreshed_at: 1_791_700_000_000, catalog_source: 'live', reason: null,
  });
  const html = renderToStaticMarkup(createElement(ProviderSettingsPanel, {
    ...props(snapshot), initialProviderId: 'openai',
  }));
  assert.match(html, /provider-list-back/);
  assert.match(html, /Provider API key/);
  assert.match(html, /Credential storage/);
  assert.match(html, /macOS Keychain/);
  assert.doesNotMatch(html, /provider-storage|This app session only/);
  assert.match(html, /Allow this provider to/);
  assert.match(html, /Plan tasks and propose actions/);
  assert.match(html, /Make semantic decisions/);
  assert.match(html, /Refresh directory/);
  assert.match(html, /Remove OpenAI SDK label connection/);
  assert.match(html, /current-model/);
  assert.doesNotMatch(html, /live-model|retired-model/);
  assert.match(html, /Live provider directory/);
  assert.match(html, /Refreshed /);
  assert.match(html, /Read-only list/);
  assert.doesNotMatch(html, /Use saved key|credentialPreview|credential-preview/i);
  assert.doesNotMatch(html, /Task model|Model identifier|name="model"/i);
});

test('provider detail uses the bundled catalog when no connection has a model list', () => {
  const snapshot = settings();
  snapshot.connections = [];
  snapshot.selected = null;
  const html = renderToStaticMarkup(createElement(ProviderSettingsPanel, {
    ...props(snapshot), initialProviderId: 'openai',
  }));
  assert.match(html, /Bundled SDK catalog/);
  assert.match(html, /sdk-model/);
  assert.doesNotMatch(html, /live-model|retired-model/);
});

test('OAuth detail offers re-login and sign-out for its saved Keychain account', () => {
  const html = renderToStaticMarkup(createElement(ProviderSettingsPanel, {
    ...props(settings('openai_oauth')), initialProviderId: 'openai',
  }));
  assert.match(html, /Re-login/);
  assert.match(html, /Sign out/);
  assert.match(html, /Credential saved in macOS Keychain/);
  assert.match(html, /Credential storage/);
  assert.doesNotMatch(html, /Provider API key|Memory only|session only/i);
  assert.match(html, /Live provider directory/);
});

test('inactive saved OAuth account can reconnect without re-login, and sign-out restores fresh sign-in UI', () => {
  const snapshot = settings('openai_oauth');
  snapshot.connections[0] = { ...snapshot.connections[0]!, available: false };
  const savedHtml = renderToStaticMarkup(createElement(ProviderSettingsPanel, {
    ...props(snapshot), initialProviderId: 'openai',
  }));
  assert.match(savedHtml, /Connect saved account/);
  assert.match(savedHtml, /Re-login/);
  assert.match(savedHtml, /Sign out/);

  snapshot.connections = [];
  snapshot.selected = null;
  const signedOutHtml = renderToStaticMarkup(createElement(ProviderSettingsPanel, {
    ...props(snapshot), initialProviderId: 'openai',
  }));
  assert.match(signedOutHtml, /Sign in with OpenAI SDK label/);
  assert.doesNotMatch(signedOutHtml, /Connect saved account|Re-login|Sign out/);
});

test('provider configuration requests always select Keychain for API keys and OAuth', () => {
  assert.deepEqual(keychainConfigureProviderRequest('openai', ['s2_planning'], { apiKey: 'secret-value' }), {
    provider_id: 'openai', storage: 'keychain', purposes: ['s2_planning'], api_key: 'secret-value',
  });
  assert.deepEqual(keychainConfigureProviderRequest('openai', ['s1_teacher']), {
    provider_id: 'openai', storage: 'keychain', purposes: ['s1_teacher'],
  });
  assert.deepEqual(keychainConfigureProviderRequest('openai', ['s1_teacher'], { reauthenticate: true }), {
    provider_id: 'openai', storage: 'keychain', purposes: ['s1_teacher'], reauthenticate: true,
  });
});

test('unavailable Keychain blocks provider configuration and explains why', () => {
  const snapshot = settings();
  snapshot.keychain_available = false;
  const html = renderToStaticMarkup(createElement(ProviderSettingsPanel, {
    ...props(snapshot), initialProviderId: 'openai',
  }));
  assert.match(html, /macOS Keychain is unavailable/);
  assert.match(html, /Credential storage/);
  assert.match(html, /name="provider-api-key"|id="provider-api-key"/);
  assert.match(html, /<input[^>]*id="provider-api-key"[^>]*disabled/);
  assert.match(html, /<button[^>]*disabled[^>]*>Save provider connection/);
});

test('provider and catalog errors stay visible without showing a stale SDK catalog beside a connection', () => {
  const snapshot = { ...settings(), reason: 'Live directory refresh is unavailable.' };
  const html = renderToStaticMarkup(createElement(ProviderSettingsPanel, {
    ...props(snapshot),
    loadError: 'Provider settings could not be loaded.',
    initialProviderId: 'openai',
  }));
  assert.match(html, /Provider settings could not be loaded/);
  assert.match(html, /Live directory refresh is unavailable/);
  assert.match(html, /Live provider directory/);
  assert.doesNotMatch(html, /Bundled SDK catalog|sdk-model/);
});

test('an unavailable Keychain connection offers a Main-owned reconnect without displaying the secret', () => {
  const snapshot = settings();
  snapshot.connections[0] = { ...snapshot.connections[0]!, available: false, storage: 'keychain' };
  const html = renderToStaticMarkup(createElement(ProviderSettingsPanel, {
    ...props(snapshot), initialProviderId: 'openai',
  }));
  assert.match(html, /Reconnect saved key/);
  assert.match(html, /Credential saved in macOS Keychain/);
  assert.doesNotMatch(html, /provider-credential-preview|credentialPreview|••••/);
  assert.match(html, /provider-settings-card-title/);
});

test('API-key persistence trims input and clears the renderer secret after success or failure', async () => {
  const persisted: string[] = [];
  let cleared = 0;
  await persistProviderCredentialInput('  secret-value  ', async (credential) => { persisted.push(credential); }, () => { cleared += 1; });
  assert.deepEqual(persisted, ['secret-value']);
  assert.equal(cleared, 1);

  await assert.rejects(persistProviderCredentialInput(' another-secret ', async () => {
    throw new Error('storage failed');
  }, () => { cleared += 1; }));
  assert.equal(cleared, 2);

  const empty = await persistProviderCredentialInput('  ', async () => {
    assert.fail('empty credentials must not be persisted');
  }, () => { cleared += 1; });
  assert.equal(empty, false);
  assert.equal(cleared, 2);
});

test('late provider transactions ignore results after their generation becomes stale', async () => {
  let resolveOperation!: (value: string) => void;
  const operation = new Promise<string>((resolve) => { resolveOperation = resolve; });
  let generation = 7;
  const events: string[] = [];
  const completion = runCurrentProviderTransaction(
    () => isCurrentCredentialTransaction(true, 7, generation),
    () => operation,
    (value) => events.push(`success:${value}`),
    () => events.push('failure'),
    () => events.push('finally'),
  );
  generation += 1;
  resolveOperation('late result');
  await completion;
  assert.deepEqual(events, []);
});
