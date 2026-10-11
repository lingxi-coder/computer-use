import assert from 'node:assert/strict';
import { spawn, type SpawnOptions, type ChildProcess } from 'node:child_process';
import test from 'node:test';
import { createOpenAiOAuthRefresher } from '../src/main/openAiLogin.js';
import { privateModelPrefix, type OpenAiOAuthSession } from '../src/main/modelBootstrap.js';

const prior = (): OpenAiOAuthSession => ({ kind: 'openai_oauth', access_token: 'expired-access',
  refresh_token: 'private-refresh', expires_at: 1, account_id: 'account-one', fedramp: true });
function refresher(response: Record<string, unknown>, children: ChildProcess[] = [], hang = false) {
  const script = `let input='';process.stdin.setEncoding('utf8');process.stdin.on('data',v=>input+=v);
process.stdin.on('end',()=>{const request=JSON.parse(input);
if(request.operation!=='refresh'||request.refresh_token!=='private-refresh'||Object.keys(request).length!==2){process.exit(2);}
${hang ? `setTimeout(()=>process.stdout.write(JSON.stringify(${JSON.stringify(response)})+'\\n'),30);` : `process.stdout.write(JSON.stringify(${JSON.stringify(response)})+'\\n');`}});`;
  const spawnHelper = ((_: string, __: string[], options: SpawnOptions) => {
    const child = spawn(process.execPath, ['-e', script], options); children.push(child); return child;
  }) as typeof spawn;
  return createOpenAiOAuthRefresher({ helperPath: '/test/private-helper', timeoutMs: 2000, spawnHelper });
}

test('refresh retains omitted identity and refresh token without mutating the saved session', async () => {
  const original = prior(); const before = structuredClone(original);
  const next = await refresher({ access_token: 'new-access', expires_at: Math.floor(Date.now() / 1000) + 600 })(original, new AbortController().signal);
  assert.deepEqual(original, before);
  assert.equal(next.refresh_token, before.refresh_token);
  assert.equal(next.account_id, before.account_id); assert.equal(next.fedramp, true);
  assert.equal(next.access_token, 'new-access');
});

test('refresh rotation replaces tokens but the model authorization never includes the refresh token', async () => {
  const next = await refresher({ access_token: 'rotated-access', refresh_token: 'rotated-refresh',
    expires_at: Math.floor(Date.now() / 1000) + 600, account_id: 'account-one', fedramp: true })(prior(), new AbortController().signal);
  assert.equal(next.refresh_token, 'rotated-refresh');
  const bytes = privateModelPrefix({ route: {
    provider_id: { open_ai_compatible: { name: 'openai-chatgpt' } }, protocol: 'open_ai_responses',
    base_url: 'https://chatgpt.com/backend-api/codex', model: 'test-model', max_output_tokens: 2048,
  }, credential: next, purposes: ['s2_planning'] });
  try {
    const frame = JSON.parse(bytes.subarray(4).toString('utf8'));
    assert.equal(frame.credential.access_token, 'rotated-access');
    assert.equal(frame.credential.refresh_token, undefined);
    assert.equal(bytes.includes(Buffer.from('rotated-refresh')), false);
  } finally { bytes.fill(0); }
});

test('invalid refresh results leave the original session intact and expose only a fixed error', async () => {
  for (const response of [{ access_token: 'invalid-secret', expires_at: 1 },
    { access_token: 'invalid-secret', expires_at: Math.floor(Date.now() / 1000) + 600, extra: 'injected' },
    { access_token: 'invalid-secret', expires_at: Math.floor(Date.now() / 1000) + 600, refresh_token: '' }]) {
    const original = prior(); const before = structuredClone(original);
    await assert.rejects(refresher(response)(original, new AbortController().signal), error =>
      error instanceof Error && !error.message.includes('invalid-secret') && !error.message.includes('private-refresh'));
    assert.deepEqual(original, before);
  }
});

test('a dispatched refresh settles after Stop so Main can persist server rotation', async () => {
  const original = prior(); const before = structuredClone(original); const children: ChildProcess[] = [];
  const abort = new AbortController();
  const run = refresher({ access_token: 'new-access', refresh_token: 'new-rotation',
    expires_at: Math.floor(Date.now() / 1000) + 600 }, children, true)(original, abort.signal);
  abort.abort();
  const result = await run;
  assert.equal(result.refresh_token, 'new-rotation');
  assert.equal(children.length, 1);
  assert.deepEqual(original, before);
});

test('a refresh cancelled before dispatch never starts its helper', async () => {
  const children: ChildProcess[] = []; const abort = new AbortController(); abort.abort();
  await assert.rejects(refresher({}, children)(prior(), abort.signal), /cancelled|timed out/);
  assert.equal(children.length, 0);
});
