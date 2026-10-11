/// <reference path="../src/renderer/env.d.ts" />

import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ModelAuthorizationStatus } from '@computer-use/desktop-api';
import { ModelConnectionPanel } from '../src/renderer/ModelConnectionPanel.js';

test('computer proposal consent is separate and explicitly grants screenshots only', () => {
  const status: ModelAuthorizationStatus = {
    phase: 'not_configured',
    route: null,
    credential_kind: null,
    storage: null,
    authorized_purposes: [],
    can_authorize: true,
    can_save: false,
    planning_available: false,
    saved: [],
    reason: null,
  };
  const html = renderToStaticMarkup(createElement(ModelConnectionPanel, { status }));
  const computerPurpose = html.match(/<input[^>]*aria-label="Computer proposals purpose"[^>]*>/)?.[0] ?? '';
  assert.match(computerPurpose, /type="checkbox"/);
  assert.doesNotMatch(computerPurpose, /checked/);
  assert.match(html, /Send full screenshots of the authorized application, including visible window content, to OpenAI Responses \(https:\/\/api\.openai\.com\/v1\) for computer-use proposals/);
  assert.match(html, /does not allow input or action execution/);
  assert.match(html, /Model-purpose approval alone does not authorize a window/);
  assert.match(html, /separately authorize a window for this session before screenshots can be sent/);
});

test('OpenAI account login defaults to S2/S1 and keeps API-key fields outside its flow', () => {
  const status: ModelAuthorizationStatus = {
    phase: 'not_configured', route: null, credential_kind: null, storage: null,
    authorized_purposes: [], can_authorize: true, can_save: false,
    planning_available: false, saved: [], reason: null,
  };
  const html = renderToStaticMarkup(createElement(ModelConnectionPanel, { status }));
  const login = html.match(/<form[^>]*aria-label="OpenAI account login"[^>]*>[\s\S]*?<\/form>/)?.[0] ?? '';
  assert.match(login, /value="gpt-6-luna"/);
  for (const label of ['OpenAI account S2 task planning purpose', 'OpenAI account S1 semantic decision purpose']) {
    const purpose = login.match(new RegExp(`<input[^>]*aria-label="${label}"[^>]*>`))?.[0] ?? '';
    assert.match(purpose, /checked/);
  }
  assert.doesNotMatch(login, /computer proposals|API key|Save.*key/i);
  assert.match(login, /Continue in your system browser/);
  assert.match(login, /account session is stored in this app’s Keychain/);
});

test('an OpenAI OAuth status never presents API-key storage as its credential kind', () => {
  const status: ModelAuthorizationStatus = {
    phase: 'authorized', route: {
      provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://api.openai.com/v1',
      model: 'gpt-6-luna', max_output_tokens: 4096,
    }, credential_kind: 'openai_oauth', storage: 'keychain', authorized_purposes: ['s2_planning', 's1_teacher'],
    can_authorize: false, can_save: false, planning_available: true, saved: [], reason: null,
  };
  const html = renderToStaticMarkup(createElement(ModelConnectionPanel, { status }));
  assert.match(html, /OpenAI account session configured/);
  assert.match(html, /OpenAI account session stored in this app’s Keychain/);
  assert.doesNotMatch(html, /Key saved on this Mac|API key kept for this run/);
});
