import assert from 'node:assert/strict';
import test from 'node:test';
import { providerCatalog, providerModels } from '../src/main/providerCatalog.js';
import { connectionId } from '../src/main/modelKeyStore.js';

const provider = {
  id: 'openai', label: 'OpenAI', description: 'SDK catalog', auth: 'api_key', directory_supported: true,
  route: { provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://api.openai.com/v1', max_output_tokens: 4096, preset_id: 'openai' },
  models: [{ id: 'test-model', label: 'Test model', selectable: true }],
};

test('catalog retains exact SDK profile identity without projecting credentials', () => {
  const catalog = providerCatalog({ providers: [provider] });
  assert.equal(catalog[0]?.route.preset_id, 'openai');
  assert.deepEqual(catalog[0]?.models, provider.models);
  assert.throws(() => providerCatalog({ providers: [{ ...provider, api_key: 'must-not-project' }] }), /unavailable/);
  assert.throws(() => providerCatalog({ providers: [{ ...provider, route: { ...provider.route, model: 'execution-model' } }] }), /unavailable/);
  assert.throws(() => providerCatalog({ providers: [{ ...provider, route: { ...provider.route, preset_id: 'other' } }] }), /unavailable/);
});

test('bad and duplicate model rows refuse the entire directory', () => {
  for (const rows of [[...provider.models, ...provider.models], [{ id: '', label: 'Empty', selectable: true }],
    [{ id: 'model', label: 'Label', selectable: true, token: 'must-not-project' }]]) {
    assert.throws(() => providerModels(rows), /unavailable/);
  }
  assert.deepEqual(providerModels([]), []);
  assert.deepEqual(providerModels([{ id: 'unknown-live-model', label: 'Unknown model', selectable: false }]),
    [{ id: 'unknown-live-model', label: 'Unknown model', selectable: false }]);
});

test('Gemini gets its native protocol and profile-specific storage identity', () => {
  const gemini = { ...provider, id: 'gemini', route: { ...provider.route, preset_id: 'gemini', provider_id: 'gemini',
    protocol: 'gemini_generate_content', base_url: 'https://generativelanguage.googleapis.com/v1beta' } };
  const catalog = providerCatalog({ providers: [gemini] });
  assert.equal(catalog[0]?.route.protocol, 'gemini_generate_content');
  const route = { ...catalog[0]!.route, model: '__provider_credentials__' };
  assert.notEqual(connectionId(route), connectionId({ ...route, preset_id: 'another-profile' }));
  assert.throws(() => providerCatalog({ providers: [{ ...gemini, route: { ...gemini.route, protocol: 'open_ai_chat' } }] }));
});

test('Anthropic-compatible SDK profiles keep their provider identity while generic routes remain refused', () => {
  const compatible = { ...provider, id: 'glm-coding', route: { ...provider.route, preset_id: 'glm-coding',
    provider_id: { open_ai_compatible: { name: 'zhipu' } }, protocol: 'anthropic_messages', base_url: 'https://open.bigmodel.cn/api/anthropic' } };
  const catalog = providerCatalog({ providers: [compatible] });
  assert.equal(catalog[0]?.route.protocol, 'anthropic_messages');
  const { preset_id: _preset, ...generic } = compatible.route;
  assert.throws(() => providerCatalog({ providers: [{ ...compatible, route: generic }] }), /unavailable/);
});
