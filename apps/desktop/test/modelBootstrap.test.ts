import assert from 'node:assert/strict';
import test from 'node:test';
import { hostEnvironment, privateModelPrefix, type ModelAuthorization } from '../src/main/modelBootstrap.js';

const authorization = (): ModelAuthorization => ({ route: {
  provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://example.invalid/v1',
  model: 'fixture-model', max_output_tokens: 4096,
}, credential: { kind: 'api_key', api_key: 'private-authorization-test-key' }, purposes: ['s2_planning'] });
const ACCOUNT_SCOPE = '6ba7b810-9dad-4e7b-8a2a-123456789abc';

test('the private prefix contains only routing and its separately authorized key', () => {
  const input = authorization(); const bytes = privateModelPrefix(input);
  assert.equal(bytes.readUInt32BE(0), bytes.length - 4);
  assert.deepEqual(JSON.parse(bytes.subarray(4).toString()), input);
  assert.equal(input.credential.kind === 'api_key' && input.credential.api_key, 'private-authorization-test-key'); bytes.fill(0);
  const dual = authorization(); dual.purposes = ['s1_teacher', 's2_planning'];
  const dualBytes = privateModelPrefix(dual);
  assert.deepEqual(JSON.parse(dualBytes.subarray(4).toString()).purposes, ['s2_planning', 's1_teacher']); dualBytes.fill(0);
});
test('empty credentials, insecure endpoints and ambiguous provider routes are refused', () => {
  for (const endpoint of ['http://example.invalid', 'https://key:secret@example.invalid', 'https://example.invalid?key=secret', 'https://example.invalid#secret']) {
    const input = authorization(); input.route.base_url = endpoint; assert.throws(() => privateModelPrefix(input), /invalid/);
  }
  const empty = authorization();
  if (empty.credential.kind === 'api_key') empty.credential.api_key = '';
  assert.throws(() => privateModelPrefix(empty), /invalid/);
  const wrong = authorization(); wrong.route.provider_id = 'anthropic_first_party'; assert.throws(() => privateModelPrefix(wrong), /invalid/);
  for (const purposes of [[], ['s2_planning', 's2_planning'], ['s3_execute'], ['computer_proposals']]) {
    const invalid = authorization(); invalid.purposes = purposes as ModelAuthorization['purposes'];
    assert.throws(() => privateModelPrefix(invalid), /invalid/);
  }
});
test('OpenAI OAuth is encoded as a bounded private credential and never as an API key', () => {
  const input: ModelAuthorization = {
    route: {
      provider_id: { open_ai_compatible: { name: 'openai-chatgpt' } },
      protocol: 'open_ai_responses', base_url: 'https://chatgpt.com/backend-api/codex',
      model: 'fixture-model', max_output_tokens: 2048,
    },
    credential: {
      kind: 'openai_oauth', access_token: 'private-oauth-test-token',
      expires_at: Math.floor(Date.now() / 1000) + 600, account_id: 'acct-123', fedramp: false,
    },
    purposes: ['s2_planning', 's1_teacher'],
  };
  const bytes = privateModelPrefix(input);
  const payload = JSON.parse(bytes.subarray(4).toString());
  assert.equal(payload.api_key, undefined);
  assert.deepEqual(payload.credential, input.credential);
  assert.deepEqual(payload.purposes, ['s2_planning', 's1_teacher']);
  bytes.fill(0);
  assert(input.credential.kind === 'openai_oauth' && input.credential.access_token === 'private-oauth-test-token');
});
test('expired OAuth, wrong OAuth route, and computer purpose are refused', () => {
  const make = (): ModelAuthorization => ({
    route: {
      provider_id: { open_ai_compatible: { name: 'openai-chatgpt' } },
      protocol: 'open_ai_responses', base_url: 'https://chatgpt.com/backend-api/codex',
      model: 'fixture-model', max_output_tokens: 2048,
    },
    credential: { kind: 'openai_oauth', access_token: 'token', expires_at: Math.floor(Date.now() / 1000) + 600, fedramp: false },
    purposes: ['s2_planning'],
  });
  const expired = make();
  if (expired.credential.kind === 'openai_oauth') expired.credential.expires_at = Math.floor(Date.now() / 1000);
  assert.throws(() => privateModelPrefix(expired), /invalid/);
  const wrongRoute = make(); wrongRoute.route.base_url = 'https://api.openai.com/v1';
  assert.throws(() => privateModelPrefix(wrongRoute), /invalid/);
  const computer = make(); computer.purposes = ['computer_proposals'];
  assert.throws(() => privateModelPrefix(computer), /invalid/);
});
test('OpenAI OAuth route cannot be downgraded to API-key credentials', () => {
  const input = authorization();
  input.route = {
    provider_id: { open_ai_compatible: { name: 'openai-chatgpt' } },
    protocol: 'open_ai_responses', base_url: 'https://chatgpt.com/backend-api/codex',
    model: 'fixture-model', max_output_tokens: 2048,
  };
  assert.throws(() => privateModelPrefix(input), /invalid/);
});
test('computer proposals need a main-generated scope and the official OpenAI Responses route', () => {
  const input = authorization();
  input.route.base_url = 'https://api.openai.com/v1';
  input.purposes = ['computer_proposals'];
  input.account_scope = ACCOUNT_SCOPE;
  const bytes = privateModelPrefix(input);
  const payload = JSON.parse(bytes.subarray(4).toString());
  assert.equal(payload.account_scope, ACCOUNT_SCOPE);
  assert.deepEqual(payload.purposes, ['computer_proposals']);
  bytes.fill(0);

  const missing = { ...input, account_scope: undefined };
  assert.throws(() => privateModelPrefix(missing), /invalid/);
  const malformed = { ...input, account_scope: 'key-derived-or-malformed' };
  assert.throws(() => privateModelPrefix(malformed), /invalid/);
  const extra = { ...authorization(), account_scope: ACCOUNT_SCOPE };
  assert.throws(() => privateModelPrefix(extra), /invalid/);
  for (const route of [
    { ...input.route, provider_id: 'anthropic_first_party' as const, protocol: 'anthropic_messages' as const },
    { ...input.route, protocol: 'open_ai_chat' as const },
    { ...input.route, base_url: 'https://compatible.invalid/v1' },
  ]) {
    assert.throws(() => privateModelPrefix({ ...input, route }), /invalid/);
  }
});
test('child environment strips provider secrets, overrides and proxy variables', () => {
  const parent = { PATH: '/usr/bin', TMPDIR: '/tmp', LANG: 'en_US.UTF-8', OPENAI_API_KEY: 'inherited-key',
    ANTHROPIC_API_KEY: 'other-key', ANTHROPIC_BASE_URL: 'https://other.invalid', HOME: '/another-product', HTTPS_PROXY: 'https://proxy.invalid', CU_MODEL: 'other-model' };
  assert.deepEqual(hostEnvironment(parent), { PATH: '/usr/bin', TMPDIR: '/tmp', LANG: 'en_US.UTF-8' });
});
