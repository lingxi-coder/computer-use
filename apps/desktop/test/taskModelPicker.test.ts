/// <reference path="../src/renderer/env.d.ts" />

import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ModelAuthorizationStatus, ProviderSettingsSnapshot } from '@computer-use/desktop-api';
import { TaskModelPicker, taskModelSelectionReady } from '../src/renderer/TaskModelPicker.js';

const snapshot: ProviderSettingsSnapshot = {
  providers: [{
    id: 'provider-sdk-id', label: 'SDK provider label', description: 'No protocol details belong in the task picker.',
    route: { preset_id: 'openai-responses-standard', provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://api.openai.com/v1', max_output_tokens: 4096 },
    auth: 'api_key', models: [], directory_supported: true,
  }],
  connections: [
    { id: 'connection-first', provider_id: 'provider-sdk-id', storage: 'memory', purposes: ['s2_planning'], available: true,
      models: [{ id: 'duplicate-model', label: 'Shared model label', selectable: true }, { id: 'not-selectable', label: 'Unavailable model', selectable: false }], refreshed_at: null, catalog_source: 'sdk', reason: null },
    { id: 'connection-second', provider_id: 'provider-sdk-id', storage: 'keychain', purposes: ['s2_planning'], available: true,
      models: [{ id: 'duplicate-model', label: 'Shared model label', selectable: true }], refreshed_at: null, catalog_source: 'sdk', reason: null },
  ],
  selected: { connection_id: 'connection-second', model: 'duplicate-model' },
  busy: false, can_configure: true, keychain_available: true, reason: null,
};

const authorized: ModelAuthorizationStatus = {
  phase: 'authorized',
  route: { preset_id: 'openai-responses-standard', provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://api.openai.com/v1', model: 'duplicate-model', max_output_tokens: 4096 },
  credential_kind: 'api_key', storage: 'keychain', authorized_purposes: ['s2_planning'],
  can_authorize: false, can_save: true, planning_available: true, saved: [], reason: null,
};

test('task picker preserves the connection identity for duplicate model IDs and only lists selectable models', () => {
  assert.equal(taskModelSelectionReady(snapshot, authorized), true);
  const html = renderToStaticMarkup(createElement(TaskModelPicker, {
    snapshot,
    authorization: authorized,
    demoAvailable: false,
    disabled: false,
    pending: false,
    error: null,
    loadError: null,
    onSelect: () => undefined,
  }));
  assert.match(html, /SDK provider label · Connection 2 · Shared model label/);
  assert.match(html, /connection-first/);
  assert.match(html, /connection-second/);
  assert.doesNotMatch(html, /not-selectable|Unavailable model/);
  assert.doesNotMatch(html, /api key|https:\/\/api\.openai\.com|open_ai_responses/i);
});

test('task picker requires a valid selection when an available provider connection exists', () => {
  assert.equal(taskModelSelectionReady({ ...snapshot, selected: null }, authorized), false);
  assert.equal(taskModelSelectionReady({ ...snapshot, selected: { connection_id: 'connection-first', model: 'not-selectable' } }, authorized), false);
  assert.equal(taskModelSelectionReady({ ...snapshot, selected: { connection_id: 'removed-connection', model: 'duplicate-model' } }, authorized), false);
  assert.equal(taskModelSelectionReady({ ...snapshot, selected: { connection_id: 'connection-first', model: 'duplicate-model' } }, {
    ...authorized,
    route: { ...authorized.route!, model: 'a-different-current-route' },
  }), false);
  assert.equal(taskModelSelectionReady({ ...snapshot, selected: { connection_id: 'connection-first', model: 'duplicate-model' } }, {
    ...authorized,
    route: { ...authorized.route!, preset_id: 'different-sdk-profile' },
  }), false);
});

test('task picker preserves demo planning only when no provider connection exists', () => {
  const unconfigured = { ...snapshot, connections: [], selected: null };
  const demoAuthorization: ModelAuthorizationStatus = {
    ...authorized, phase: 'not_configured', route: null, credential_kind: null, storage: null,
    authorized_purposes: [], planning_available: true,
  };
  assert.equal(taskModelSelectionReady(unconfigured, { ...demoAuthorization, planning_available: false }), false);
  assert.equal(taskModelSelectionReady(unconfigured, demoAuthorization), true);
  assert.equal(taskModelSelectionReady({ ...snapshot, connections: snapshot.connections.map((connection) => ({ ...connection, available: false })), selected: null }, {
    ...authorized,
    phase: 'not_configured',
    route: null,
    planning_available: true,
  }), false);
  const html = renderToStaticMarkup(createElement(TaskModelPicker, {
    snapshot: unconfigured,
    authorization: demoAuthorization,
    demoAvailable: true,
    disabled: false,
    pending: false,
    error: null,
    loadError: null,
    onSelect: () => undefined,
  }));
  assert.match(html, /Demo planning/);
  assert.match(html, /no provider model is selected/);
});
