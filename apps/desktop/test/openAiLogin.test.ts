import assert from 'node:assert/strict';
import { spawn, type ChildProcess, type SpawnOptions } from 'node:child_process';
import { createServer } from 'node:http';
import * as http from 'node:http';
import test from 'node:test';
import { createOpenAiLogin } from '../src/main/openAiLogin.js';

// The SDK generates 16 random bytes: 22 base64url characters without padding.
const STATE = 'S'.repeat(22);
const VERIFIER = 'V'.repeat(43);
const MODEL = 'test-openai-model';

const validHelper = String.raw`
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', () => {
  const request = JSON.parse(input);
  if (request.operation === 'start') {
    const redirect_uri = 'http://localhost:' + request.port + '/auth/callback';
    const auth = new URL('https://auth.openai.com/oauth/authorize');
    auth.searchParams.set('response_type', 'code');
    auth.searchParams.set('client_id', 'fixed-test-client');
    auth.searchParams.set('redirect_uri', redirect_uri);
    auth.searchParams.set('code_challenge', 'C'.repeat(43));
    auth.searchParams.set('code_challenge_method', 'S256');
    auth.searchParams.set('state', '${STATE}');
    process.stdout.write(JSON.stringify({ url: auth.href, verifier: '${VERIFIER}', state: '${STATE}', redirect_uri }) + '\n');
    return;
  }
  if (request.operation === 'exchange' && request.code === 'fixture-code'
      && request.verifier === '${VERIFIER}' && request.redirect_uri.startsWith('http://localhost:')) {
    process.stdout.write(JSON.stringify({ access_token: 'fixture-access-token', expires_at: Math.floor(Date.now() / 1000) + 600,
      account_id: 'fixture-account', fedramp: false }) + '\n');
    return;
  }
  process.exitCode = 1;
});`;

function fakeSpawn(script = validHelper, children: ChildProcess[] = []): typeof spawn {
  return ((_: string, __: string[], options: SpawnOptions) => {
    const child = spawn(process.execPath, ['-e', script], options);
    children.push(child);
    return child;
  }) as unknown as typeof spawn;
}

async function postCallback(urlText: string, method = 'GET'): Promise<{ status: number; body: string }> {
  const url = new URL(urlText);
  return new Promise((resolve, reject) => {
    const request = http.request({
      hostname: '127.0.0.1',
      port: Number(url.port),
      path: `${url.pathname}${url.search}`,
      method,
      headers: { Host: `localhost:${url.port}` },
    }, (response) => {
      const chunks: Buffer[] = [];
      response.on('data', (chunk: Buffer) => chunks.push(Buffer.from(chunk)));
      response.on('end', () => resolve({ status: response.statusCode ?? 0, body: Buffer.concat(chunks).toString('utf8') }));
    });
    request.on('error', reject);
    request.end();
  });
}

function callbackUrl(authorizationUrl: string, state: string, code = 'fixture-code'): string {
  const auth = new URL(authorizationUrl);
  const callback = new URL(auth.searchParams.get('redirect_uri')!);
  callback.searchParams.set('state', state);
  callback.searchParams.set('code', code);
  return callback.href;
}

function loginOptions(openExternal: (url: string) => Promise<unknown>, overrides: Partial<Parameters<typeof createOpenAiLogin>[0]> = {}) {
  return {
    helperPath: '/private/cu-auth-test-helper',
    openExternal,
    callbackPorts: [0],
    timeoutMs: 2_000,
    spawnHelper: fakeSpawn(),
    ...overrides,
  };
}

test('loopback callback exchanges through the private helper and returns only the in-memory credential', async () => {
  let helperCalls = 0;
  const spawnHelper = ((_: string, __: string[], options: SpawnOptions) => {
    helperCalls += 1;
    return spawn(process.execPath, ['-e', validHelper], options);
  }) as unknown as typeof spawn;
  let browserOrigin = '';
  const login = createOpenAiLogin(loginOptions(async (url) => {
    const authorization = new URL(url);
    browserOrigin = authorization.origin;
    const redirect = callbackUrl(url, STATE);
    const wrong = await postCallback(callbackUrl(url, 'X'.repeat(22)));
    assert.equal(wrong.status, 400);
    assert(!wrong.body.includes(STATE));
    const wrongMethod = await postCallback(redirect, 'POST');
    assert.equal(wrongMethod.status, 405);
    const accepted = await postCallback(redirect);
    assert.equal(accepted.status, 200);
    assert.match(accepted.body, /Authorization received\. Return to LingXi Control to finish signing in\./);
    assert(!accepted.body.includes('fixture-access-token'));
    const duplicate = await postCallback(redirect);
    assert.equal(duplicate.status, 409);
  }, { spawnHelper }));

  const credential = await login({ model: MODEL }, new AbortController().signal);
  assert.equal(browserOrigin, 'https://auth.openai.com');
  assert.equal(helperCalls, 2, 'one start and one exchange helper process');
  assert.deepEqual(credential, {
    kind: 'openai_oauth', access_token: 'fixture-access-token',
    expires_at: credential.expires_at, account_id: 'fixture-account', fedramp: false,
  });
});

test('an occupied preferred callback port falls back before starting the helper', async () => {
  const occupied = createServer();
  await new Promise<void>((resolve) => occupied.listen(0, '127.0.0.1', resolve));
  const occupiedPort = (occupied.address() as { port: number }).port;
  try {
    let callbackPort = 0;
    const login = createOpenAiLogin(loginOptions(async (url) => {
      callbackPort = Number(new URL(new URL(url).searchParams.get('redirect_uri')!).port);
      assert.notEqual(callbackPort, occupiedPort);
      assert.equal((await postCallback(callbackUrl(url, STATE))).status, 200);
    }, { callbackPorts: [occupiedPort, 0] }));
    await login({ model: MODEL }, new AbortController().signal);
    assert(callbackPort > 0);
  } finally {
    await new Promise<void>((resolve) => occupied.close(() => resolve()));
  }
});

test('a wrong authorization origin is rejected without exposing the URL', async () => {
  const hostile = String.raw`
const request = JSON.parse(require('fs').readFileSync(0, 'utf8'));
const redirect_uri = 'http://localhost:' + request.port + '/auth/callback';
process.stdout.write(JSON.stringify({ url: 'https://evil.invalid/oauth?token=must-not-leak', verifier: '${VERIFIER}', state: '${STATE}', redirect_uri }) + '\n');`;
  let opened = false;
  const login = createOpenAiLogin(loginOptions(async () => { opened = true; }, { spawnHelper: fakeSpawn(hostile) }));
  await assert.rejects(login({ model: MODEL }, new AbortController().signal), (error: Error) => {
    assert(!error.message.includes('evil.invalid'));
    assert(!error.message.includes('must-not-leak'));
    return true;
  });
  assert.equal(opened, false);
});

test('the ten-minute lifecycle cancellation retires an in-flight browser launch and closes the listener', async () => {
  let opened = 0;
  const login = createOpenAiLogin(loginOptions(async () => {
    opened += 1;
    return new Promise<never>(() => undefined);
  }, { timeoutMs: 500 }));
  await assert.rejects(login({ model: MODEL }, new AbortController().signal), /cancelled or timed out/);
  assert.equal(opened, 1);
});

test('abort kills a helper that has not produced a bounded response', async () => {
  const pendingHelper = String.raw`process.stdin.resume(); process.stdin.on('end', () => setInterval(() => {}, 1000));`;
  const children: ChildProcess[] = [];
  const controller = new AbortController();
  const login = createOpenAiLogin(loginOptions(async () => { throw new Error('browser must not open'); }, {
    spawnHelper: fakeSpawn(pendingHelper, children), timeoutMs: 2_000,
  }));
  const result = login({ model: MODEL }, controller.signal);
  setTimeout(() => controller.abort(), 25);
  await assert.rejects(result, /cancelled or timed out/);
  assert.equal(children.length, 1);
  assert(children[0]!.signalCode !== null || children[0]!.exitCode !== null);
});
