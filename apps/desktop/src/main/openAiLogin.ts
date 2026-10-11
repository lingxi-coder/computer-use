import { spawn, type ChildProcess } from 'node:child_process';
import { timingSafeEqual } from 'node:crypto';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import type { OpenAiOAuthSession } from './modelBootstrap.js';
import { hostEnvironment, validateOpenAiOAuthCredential, validateOpenAiOAuthSession } from './modelBootstrap.js';

const CALLBACK_HOST = '127.0.0.1';
const CALLBACK_PATH = '/auth/callback';
const CALLBACK_PORTS = [1455, 1457] as const;
const MAX_PRIVATE_LINE_BYTES = 32 * 1024;
const DEFAULT_TIMEOUT_MS = 10 * 60 * 1000;
const SUCCESS_PAGE = '<!doctype html><html><head><meta charset="utf-8"><title>Authorization received</title></head><body>Authorization received. Return to Computer Use to finish signing in.</body></html>';
const ERROR_PAGE = 'Sign-in could not be completed. Return to Computer Use.';

type JsonObject = Record<string, unknown>;
type SpawnHelper = typeof spawn;
export type OpenAiLogin = (request: { model: string }, signal: AbortSignal) => Promise<OpenAiOAuthSession>;
export type OpenAiOAuthRefresh = (session: OpenAiOAuthSession, signal: AbortSignal) => Promise<OpenAiOAuthSession>;

export interface CreateOpenAiLoginOptions {
  helperPath: string;
  openExternal: (url: string) => Promise<unknown>;
  /** Test seam; production always uses the two fixed callback ports. */
  callbackPorts?: readonly number[];
  /** Test seam; production uses the fixed ten-minute sign-in lifetime. */
  timeoutMs?: number;
  /** Test seam for a private helper process. */
  spawnHelper?: SpawnHelper;
}

export interface CreateOpenAiOAuthRefreshOptions {
  helperPath: string;
  /** Test seam; production uses a fixed 35-second helper deadline. */
  timeoutMs?: number;
  /** Test seam for the private helper process. */
  spawnHelper?: SpawnHelper;
}

function failed(): Error { return new Error('OpenAI sign-in could not be completed.'); }
function cancelled(): Error { return new Error('OpenAI sign-in was cancelled or timed out.'); }

function exactKeys(value: JsonObject, keys: readonly string[]): boolean {
  return Object.keys(value).sort().join(',') === [...keys].sort().join(',');
}

function parseSingleJsonLine(chunks: readonly Buffer[], maxBytes = MAX_PRIVATE_LINE_BYTES): unknown {
  const bytes = Buffer.concat(chunks);
  try {
    if (bytes.length === 0 || bytes.length > maxBytes || bytes.at(-1) !== 0x0a) throw failed();
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    const newline = text.indexOf('\n');
    if (newline !== text.length - 1 || text.slice(0, newline).includes('\r')) throw failed();
    return JSON.parse(text.slice(0, newline));
  } catch { throw failed(); }
  finally { bytes.fill(0); }
}

export async function requestPrivateHelper(
  helperPath: string,
  request: JsonObject,
  signal: AbortSignal,
  spawnHelper: SpawnHelper = spawn,
  maxOutputBytes = MAX_PRIVATE_LINE_BYTES,
): Promise<unknown> {
  if (signal.aborted) throw cancelled();
  const input = Buffer.from(`${JSON.stringify(request)}\n`, 'utf8');
  if (input.length > MAX_PRIVATE_LINE_BYTES) { input.fill(0); throw failed(); }
  let child: ChildProcess | null = null;
  const chunks: Buffer[] = [];
  let outputBytes = 0;
  let oversized = false;
  let inputFailed = false;
  try {
    try {
      child = spawnHelper(helperPath, [], {
        stdio: ['pipe', 'pipe', 'ignore'],
        env: hostEnvironment(process.env),
        windowsHide: true,
        shell: false,
      });
    } catch { throw failed(); }
    if (!child.stdin || !child.stdout) throw failed();
    child.stdout.on('data', (chunk: Buffer) => {
      outputBytes += chunk.length;
      if (outputBytes > maxOutputBytes) {
        oversized = true;
        chunk.fill(0);
        child?.kill('SIGKILL');
      } else chunks.push(Buffer.from(chunk));
    });
    child.stdin.on('error', () => { inputFailed = true; child?.kill('SIGKILL'); });
    const abortChild = (): void => { child?.kill('SIGKILL'); };
    signal.addEventListener('abort', abortChild, { once: true });
    if (signal.aborted) abortChild();
    const processResult = new Promise<number | null>((resolve) => child!.once('close', resolve));
    const processError = new Promise<never>((_resolve, reject) => child!.once('error', () => reject(failed())));
    child.stdin.end(input, () => input.fill(0));
    let exitCode: number | null;
    try { exitCode = await Promise.race([processResult, processError]); }
    finally { signal.removeEventListener('abort', abortChild); }
    if (signal.aborted) throw cancelled();
    if (oversized || inputFailed || exitCode !== 0) throw failed();
    return parseSingleJsonLine(chunks, maxOutputBytes);
  } catch (error) {
    if (signal.aborted) throw cancelled();
    if (error instanceof Error && error.message === 'OpenAI sign-in could not be completed.') throw error;
    throw failed();
  } finally {
    input.fill(0);
    for (const chunk of chunks) chunk.fill(0);
    if (child && child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  }
}

function validPkceValue(value: unknown): value is string {
  return typeof value === 'string' && /^[A-Za-z0-9_-]{43,128}$/.test(value);
}

interface StartResult { url: string; verifier: string; state: string; redirect_uri: string }

function parseStartResult(value: unknown, port: number): StartResult {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw failed();
  const row = value as JsonObject;
  if (!exactKeys(row, ['url', 'verifier', 'state', 'redirect_uri'])
    || typeof row.url !== 'string' || Buffer.byteLength(row.url, 'utf8') > 4096
    || !validPkceValue(row.verifier) || typeof row.state !== 'string' || !/^[A-Za-z0-9_-]{22}$/.test(row.state)
    || typeof row.redirect_uri !== 'string' || Buffer.byteLength(row.redirect_uri, 'utf8') > 256) throw failed();
  let authorizationUrl: URL;
  let redirect: URL;
  try {
    authorizationUrl = new URL(row.url);
    redirect = new URL(row.redirect_uri);
  } catch { throw failed(); }
  if (authorizationUrl.protocol !== 'https:' || authorizationUrl.hostname !== 'auth.openai.com'
    || authorizationUrl.username || authorizationUrl.password || authorizationUrl.pathname !== '/oauth/authorize'
    || authorizationUrl.searchParams.get('response_type') !== 'code'
    || authorizationUrl.searchParams.get('code_challenge_method') !== 'S256'
    || !authorizationUrl.searchParams.get('code_challenge')
    || authorizationUrl.searchParams.get('state') !== row.state
    || authorizationUrl.searchParams.get('redirect_uri') !== row.redirect_uri
    || redirect.protocol !== 'http:' || redirect.hostname !== 'localhost'
    || redirect.port !== String(port) || redirect.username || redirect.password
    || redirect.pathname !== CALLBACK_PATH || redirect.search || redirect.hash) throw failed();
  return { url: row.url, verifier: row.verifier, state: row.state, redirect_uri: row.redirect_uri };
}

function callbackResponse(response: ServerResponse, status: number, body: string, html = false): void {
  response.statusCode = status;
  response.setHeader('Cache-Control', 'no-store');
  response.setHeader('Content-Security-Policy', "default-src 'none'; base-uri 'none'; frame-ancestors 'none'");
  response.setHeader('Referrer-Policy', 'no-referrer');
  response.setHeader('X-Content-Type-Options', 'nosniff');
  response.setHeader('Connection', 'close');
  response.setHeader('Content-Type', html ? 'text/html; charset=utf-8' : 'text/plain; charset=utf-8');
  response.end(body);
}

function handleCallback(
  request: IncomingMessage,
  response: ServerResponse,
  port: number,
  getState: () => string | null,
  takeCode: (code: string) => void,
  failCallback: (error: Error) => void,
  accepted: { value: boolean },
): void {
  if (Buffer.byteLength(request.url ?? '', 'utf8') > 16 * 1024) {
    request.resume(); callbackResponse(response, 413, ERROR_PAGE); return;
  }
  let url: URL;
  try { url = new URL(request.url ?? '/', `http://localhost:${port}`); }
  catch { request.resume(); callbackResponse(response, 400, ERROR_PAGE); return; }
  if (request.method !== 'GET' || url.pathname !== CALLBACK_PATH) {
    request.resume(); callbackResponse(response, request.method === 'GET' ? 404 : 405, ERROR_PAGE); return;
  }
  const expectedState = getState();
  const states = url.searchParams.getAll('state');
  if (!expectedState || states.length !== 1) { request.resume(); callbackResponse(response, 400, ERROR_PAGE); return; }
  const supplied = Buffer.from(states[0]!, 'utf8');
  const expected = Buffer.from(expectedState, 'utf8');
  let matches = false;
  try { matches = supplied.length === expected.length && timingSafeEqual(supplied, expected); }
  finally { supplied.fill(0); expected.fill(0); }
  if (!matches) { request.resume(); callbackResponse(response, 400, ERROR_PAGE); return; }
  if (accepted.value) { request.resume(); callbackResponse(response, 409, ERROR_PAGE); return; }
  accepted.value = true;
  const codes = url.searchParams.getAll('code');
  const errors = url.searchParams.getAll('error');
  if (errors.length !== 0 || codes.length !== 1 || codes[0]!.trim().length === 0
    || Buffer.byteLength(codes[0]!, 'utf8') > 8192 || /\p{Cc}/u.test(codes[0]!)) {
    request.resume(); callbackResponse(response, 400, ERROR_PAGE); failCallback(failed()); return;
  }
  request.resume();
  callbackResponse(response, 200, SUCCESS_PAGE, true);
  takeCode(codes[0]!);
}

function listen(server: Server, port: number): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const listening = (): void => { server.removeListener('error', error); resolve(); };
    const error = (reason: NodeJS.ErrnoException): void => { server.removeListener('listening', listening); reject(reason); };
    server.once('listening', listening);
    server.once('error', error);
    try { server.listen(port, CALLBACK_HOST); }
    catch (reason) { server.removeListener('listening', listening); server.removeListener('error', error); reject(reason); }
  });
}

async function bindCallbackServer(server: Server, ports: readonly number[]): Promise<number> {
  for (const port of ports) {
    try {
      await listen(server, port);
      const address = server.address();
      if (!address || typeof address === 'string') throw failed();
      return address.port;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'EADDRINUSE') continue;
      throw failed();
    }
  }
  throw failed();
}

function validModel(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && value.trim() === value
    && Buffer.byteLength(value, 'utf8') <= 256 && !/\p{Cc}/u.test(value);
}

function abortableExternal(openExternal: (url: string) => Promise<unknown>, url: string, signal: AbortSignal): Promise<unknown> {
  if (signal.aborted) return Promise.reject(cancelled());
  let abortListener: (() => void) | undefined;
  const aborted = new Promise<never>((_resolve, reject) => {
    abortListener = () => reject(cancelled());
    signal.addEventListener('abort', abortListener, { once: true });
  });
  return Promise.race([Promise.resolve().then(() => openExternal(url)), aborted])
    .finally(() => { if (abortListener) signal.removeEventListener('abort', abortListener); });
}

async function closeServer(server: Server): Promise<void> {
  if (!server.listening) return;
  await new Promise<void>((resolve) => {
    server.close(() => resolve());
    server.closeAllConnections();
  });
}

/** Main-only OAuth callback orchestration; PKCE and token exchange stay in the private Rust helper. */
export function createOpenAiLogin(options: CreateOpenAiLoginOptions): OpenAiLogin {
  const ports = options.callbackPorts ?? CALLBACK_PORTS;
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const spawnHelper = options.spawnHelper ?? spawn;
  if (!Array.isArray(ports) || ports.length === 0 || ports.length > 2
    || ports.some((port) => !Number.isInteger(port) || port < 0 || port > 65535)
    || !Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > DEFAULT_TIMEOUT_MS) {
    throw new Error('OpenAI sign-in configuration is invalid.');
  }
  return async (request, signal) => {
    if (!validModel(request.model)) throw failed();
    if (signal.aborted) throw cancelled();
    const abort = new AbortController();
    const relayAbort = (): void => abort.abort();
    signal.addEventListener('abort', relayAbort, { once: true });
    if (signal.aborted) relayAbort();
    const timeout = setTimeout(relayAbort, timeoutMs);
    timeout.unref?.();
    let state: string | null = null;
    let callbackCode = '';
    let rejectCode!: (error: Error) => void;
    let resolveCode!: (code: string) => void;
    let callbackSettled = false;
    const callbackCodePromise = new Promise<string>((resolve, reject) => {
      resolveCode = resolve; rejectCode = reject;
    });
    void callbackCodePromise.catch(() => undefined);
    const settleCode = (code: string): void => {
      if (callbackSettled) return;
      callbackSettled = true; callbackCode = code; resolveCode(code);
    };
    const settleFailure = (error: Error): void => {
      if (callbackSettled) return;
      callbackSettled = true; rejectCode(error);
    };
    const onAbort = (): void => settleFailure(cancelled());
    abort.signal.addEventListener('abort', onAbort, { once: true });
    const accepted = { value: false };
    let listening = false;
    let callbackPort = 0;
    const server = createServer((incoming, response) => {
      handleCallback(incoming, response, callbackPort, () => state, settleCode, settleFailure, accepted);
    });
    server.on('error', () => { if (listening) settleFailure(failed()); });
    let verifier = '';
    let redirectUri = '';
    let authorizationUrl = '';
    try {
      if (abort.signal.aborted) throw cancelled();
      callbackPort = await bindCallbackServer(server, ports);
      listening = true;
      const startValue = await requestPrivateHelper(options.helperPath, {
        operation: 'start', port: callbackPort,
      }, abort.signal, spawnHelper);
      let start: StartResult;
      try { start = parseStartResult(startValue, callbackPort); }
      finally {
        if (startValue && typeof startValue === 'object' && !Array.isArray(startValue)) {
          const raw = startValue as JsonObject;
          for (const key of ['url', 'verifier', 'state', 'redirect_uri']) if (typeof raw[key] === 'string') raw[key] = '';
        }
      }
      verifier = start.verifier;
      state = start.state;
      redirectUri = start.redirect_uri;
      authorizationUrl = start.url;
      start.url = ''; start.verifier = ''; start.state = ''; start.redirect_uri = '';
      if (abort.signal.aborted) throw cancelled();
      await abortableExternal(options.openExternal, authorizationUrl, abort.signal);
      authorizationUrl = '';
      const code = await callbackCodePromise;
      const exchange = { operation: 'exchange', code, verifier, redirect_uri: redirectUri };
      callbackCode = '';
      let responseValue: unknown;
      try { responseValue = await requestPrivateHelper(options.helperPath, exchange, abort.signal, spawnHelper); }
      finally { exchange.code = ''; exchange.verifier = ''; exchange.redirect_uri = ''; verifier = ''; }
      if (abort.signal.aborted) throw cancelled();
      if (!responseValue || typeof responseValue !== 'object' || Array.isArray(responseValue)) throw failed();
      const response = responseValue as JsonObject;
      try {
        if (!(exactKeys(response, ['access_token', 'expires_at', 'fedramp'])
          || exactKeys(response, ['access_token', 'account_id', 'expires_at', 'fedramp'])
          || exactKeys(response, ['access_token', 'expires_at', 'fedramp', 'refresh_token'])
          || exactKeys(response, ['access_token', 'account_id', 'expires_at', 'fedramp', 'refresh_token']))) throw failed();
        const credential = validateOpenAiOAuthSession({
          kind: 'openai_oauth',
          access_token: response.access_token,
          ...(response.refresh_token === undefined ? {} : { refresh_token: response.refresh_token }),
          expires_at: response.expires_at,
          ...(response.account_id === undefined ? {} : { account_id: response.account_id }),
          fedramp: response.fedramp,
        });
        validateOpenAiOAuthCredential({
          kind: credential.kind, access_token: credential.access_token, expires_at: credential.expires_at,
          ...(credential.account_id === undefined ? {} : { account_id: credential.account_id }), fedramp: credential.fedramp,
        });
        if (abort.signal.aborted) { credential.access_token = ''; credential.refresh_token = ''; throw cancelled(); }
        return credential;
      } finally {
        if (typeof response.access_token === 'string') response.access_token = '';
        if (typeof response.refresh_token === 'string') response.refresh_token = '';
      }
    } catch {
      throw abort.signal.aborted ? cancelled() : failed();
    } finally {
      clearTimeout(timeout);
      signal.removeEventListener('abort', relayAbort);
      abort.signal.removeEventListener('abort', onAbort);
      if (callbackCode) callbackCode = '';
      state = null; verifier = ''; redirectUri = ''; authorizationUrl = '';
      await closeServer(server);
    }
  };
}

/** Refreshes a saved Main-only session through the same private SDK helper used for login. */
export function createOpenAiOAuthRefresher(options: CreateOpenAiOAuthRefreshOptions): OpenAiOAuthRefresh {
  const timeoutMs = options.timeoutMs ?? 35_000;
  const spawnHelper = options.spawnHelper ?? spawn;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 35_000) {
    throw new Error('OpenAI refresh configuration is invalid.');
  }
  return async (sessionValue, signal) => {
    let prior: OpenAiOAuthSession | null = null;
    let next: OpenAiOAuthSession | null = null;
    let responseValue: unknown;
    try {
      if (signal.aborted) throw cancelled();
      prior = validateOpenAiOAuthSession(sessionValue);
      if (!prior.refresh_token) throw failed();
      const timeout = AbortSignal.timeout(timeoutMs);
      const request = { operation: 'refresh', refresh_token: prior.refresh_token };
      try {
        // Once refresh is sent the provider may rotate its one-use refresh token.
        // Stop prevents activation, while this bounded exchange settles so Main
        // can persist any rotation before honoring cancellation.
        responseValue = await requestPrivateHelper(options.helperPath, request, timeout, spawnHelper);
      } finally { request.refresh_token = ''; }
      if (timeout.aborted) throw cancelled();
      if (!responseValue || typeof responseValue !== 'object' || Array.isArray(responseValue)) throw failed();
      const response = responseValue as JsonObject;
      const validShape = exactKeys(response, ['access_token', 'expires_at'])
        || exactKeys(response, ['access_token', 'expires_at', 'refresh_token'])
        || exactKeys(response, ['access_token', 'account_id', 'expires_at'])
        || exactKeys(response, ['access_token', 'expires_at', 'fedramp'])
        || exactKeys(response, ['access_token', 'expires_at', 'refresh_token', 'fedramp'])
        || exactKeys(response, ['access_token', 'account_id', 'expires_at', 'fedramp'])
        || exactKeys(response, ['access_token', 'account_id', 'expires_at', 'refresh_token'])
        || exactKeys(response, ['access_token', 'account_id', 'expires_at', 'refresh_token', 'fedramp']);
      if (!validShape) throw failed();
      next = validateOpenAiOAuthSession({
        kind: 'openai_oauth',
        access_token: response.access_token,
        ...(response.refresh_token === undefined
          ? (prior.refresh_token === undefined ? {} : { refresh_token: prior.refresh_token })
          : { refresh_token: response.refresh_token }),
        expires_at: response.expires_at,
        ...(response.account_id === undefined
          ? (prior.account_id === undefined ? {} : { account_id: prior.account_id })
          : { account_id: response.account_id }),
        fedramp: response.fedramp === undefined ? prior.fedramp : response.fedramp,
      });
      validateOpenAiOAuthCredential({
        kind: next.kind, access_token: next.access_token, expires_at: next.expires_at,
        ...(next.account_id === undefined ? {} : { account_id: next.account_id }), fedramp: next.fedramp,
      });
      if (timeout.aborted) throw cancelled();
      const result = next;
      next = null;
      return result;
    } catch {
      throw signal.aborted ? cancelled() : failed();
    } finally {
      if (prior) { prior.access_token = ''; prior.refresh_token = ''; }
      if (next) { next.access_token = ''; next.refresh_token = ''; }
      if (responseValue && typeof responseValue === 'object' && !Array.isArray(responseValue)) {
        const response = responseValue as JsonObject;
        if (typeof response.access_token === 'string') response.access_token = '';
        if (typeof response.refresh_token === 'string') response.refresh_token = '';
      }
    }
  };
}
