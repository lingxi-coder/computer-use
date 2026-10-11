/** Private, in-memory main -> host authorization. Never a desktop IPC Message. */
import type { ModelPurpose, ModelRoute } from '@computer-use/desktop-api';
export type { ModelProvider, ModelRoute } from '@computer-use/desktop-api';
export interface ApiKeyCredential {
  kind: 'api_key';
  api_key: string;
}
/** Access-only credential sent to the private model host. */
export interface OpenAiOAuthCredential {
  kind: 'openai_oauth';
  access_token: string;
  expires_at: number;
  account_id?: string;
  fedramp: boolean;
}
/** Main-only persisted session. The refresh token never crosses into Host authorization. */
export interface OpenAiOAuthSession extends OpenAiOAuthCredential {
  refresh_token?: string;
}
export type ModelCredential = ApiKeyCredential | OpenAiOAuthCredential;
export interface ModelAuthorization {
  route: ModelRoute;
  credential: ModelCredential;
  purposes: ModelPurpose[];
  /** Main-generated non-secret SDK continuation scope; never persisted or projected. */
  account_scope?: string;
}

const COMPUTER_PURPOSE = 'computer_proposals' satisfies ModelPurpose;

export function validateModelRoute(value: unknown): ModelRoute {
  const invalid = () => new Error('invalid explicit model route');
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  const route = value as ModelRoute;
  const bounded = (s: unknown, max: number): s is string => typeof s === 'string' && s.trim().length > 0
    && Buffer.byteLength(s, 'utf8') <= max && !/\p{Cc}/u.test(s);
  let url: URL;
  try { url = new URL(route.base_url); } catch { throw invalid(); }
  const provider = route.provider_id;
  const protocols = ['anthropic_messages', 'open_ai_chat', 'open_ai_responses', 'gemini_generate_content'];
  const named = (value: unknown): boolean => value !== null && typeof value === 'object'
    && Object.keys(value).length === 1 && bounded((value as { name?: unknown }).name, 128);
  const providerValid = provider === 'anthropic_first_party' ? route.protocol === 'anthropic_messages'
    : provider === 'open_ai' ? ['open_ai_chat', 'open_ai_responses'].includes(route.protocol)
    : provider === 'gemini' ? route.protocol === 'gemini_generate_content'
    : typeof provider === 'object' && provider !== null && Object.keys(provider).length === 1
      && ('open_ai_compatible' in provider ? named(provider.open_ai_compatible) && (['open_ai_chat', 'open_ai_responses'].includes(route.protocol)
          || (route.preset_id !== undefined && route.protocol === 'anthropic_messages'))
        : 'custom' in provider && named(provider.custom));
  if ((route.preset_id !== undefined && !bounded(route.preset_id, 128)) || !providerValid || !protocols.includes(route.protocol) || !bounded(route.model, 256)
    || Buffer.byteLength(route.base_url, 'utf8') > 2048 || url.protocol !== 'https:' || !url.hostname
    || url.username || url.password || url.search || url.hash
    || !Number.isInteger(route.max_output_tokens) || route.max_output_tokens < 256 || route.max_output_tokens > 8192) throw invalid();
  return { provider_id: provider, protocol: route.protocol, base_url: route.base_url, model: route.model, max_output_tokens: route.max_output_tokens, ...(route.preset_id !== undefined ? { preset_id: route.preset_id } : {}) };
}

export function validateModelKey(value: unknown): string {
  if (typeof value !== 'string' || value.trim().length === 0 || Buffer.byteLength(value, 'utf8') > 16 * 1024 || /\p{Cc}/u.test(value)) {
    throw new Error('invalid private model authorization');
  }
  return value;
}

function validateOAuthSession(value: unknown, nowSeconds: number, allowExpired: boolean): OpenAiOAuthSession {
  const invalid = () => new Error('invalid private model authorization');
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  const row = value as Record<string, unknown>;
  const keys = Object.keys(row).sort();
  const expected = ['access_token', 'expires_at', 'fedramp', 'kind',
    ...(row.account_id === undefined ? [] : ['account_id']),
    ...(row.refresh_token === undefined ? [] : ['refresh_token'])].sort();
  if (keys.length !== expected.length || keys.some((key, index) => key !== expected[index]) || row.kind !== 'openai_oauth'
    || typeof row.access_token !== 'string' || row.access_token.length === 0
    || Buffer.byteLength(row.access_token, 'utf8') > 16 * 1024 || /[\u0000-\u001f\u007f]/.test(row.access_token)
    || !Number.isSafeInteger(row.expires_at) || (row.expires_at as number) <= 0
    || (!allowExpired && (row.expires_at as number) <= nowSeconds)
    || typeof row.fedramp !== 'boolean'
    || (row.account_id !== undefined && (typeof row.account_id !== 'string'
      || row.account_id.length === 0 || row.account_id.length > 512 || /[\u0000-\u001f\u007f]/.test(row.account_id)))
    || (row.refresh_token !== undefined && (typeof row.refresh_token !== 'string'
      || row.refresh_token.length === 0 || Buffer.byteLength(row.refresh_token, 'utf8') > 16 * 1024
      || /[\u0000-\u001f\u007f]/.test(row.refresh_token)))) {
    throw invalid();
  }
  const session: OpenAiOAuthSession = {
    kind: 'openai_oauth',
    access_token: row.access_token,
    expires_at: row.expires_at as number,
    ...(typeof row.account_id === 'string' ? { account_id: row.account_id } : {}),
    ...(typeof row.refresh_token === 'string' ? { refresh_token: row.refresh_token } : {}),
    fedramp: row.fedramp,
  };
  if (Buffer.byteLength(JSON.stringify(session), 'utf8') > 16 * 1024) throw invalid();
  return session;
}

export function validateOpenAiOAuthCredential(value: unknown, nowSeconds = Math.floor(Date.now() / 1000)): OpenAiOAuthCredential {
  const session = validateOAuthSession(value, nowSeconds, false);
  return {
    kind: 'openai_oauth', access_token: session.access_token, expires_at: session.expires_at,
    ...(session.account_id === undefined ? {} : { account_id: session.account_id }), fedramp: session.fedramp,
  };
}

/** Strict Keychain session parser. Expired access tokens remain valid here so Main can refresh them. */
export function validateOpenAiOAuthSession(value: unknown): OpenAiOAuthSession {
  return validateOAuthSession(value, Math.floor(Date.now() / 1000), true);
}

function validateModelCredential(value: unknown): ModelCredential {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('invalid private model authorization');
  }
  const row = value as Record<string, unknown>;
  if (row.kind === 'api_key' && Object.keys(row).sort().join(',') === 'api_key,kind') {
    return { kind: 'api_key', api_key: validateModelKey(row.api_key) };
  }
  if (row.kind === 'openai_oauth') return validateOpenAiOAuthCredential(value);
  throw new Error('invalid private model authorization');
}

function clearModelCredential(credential: ModelCredential): void {
  if (credential.kind === 'api_key') credential.api_key = '';
  else credential.access_token = '';
}

export function privateModelPrefix(authorization: ModelAuthorization): Buffer {
  const route = validateModelRoute(authorization.route);
  const credential = validateModelCredential(authorization.credential);
  try {
    // Copy only known fields. Unknown properties cannot become a secret sidecar
    // inside routing configuration or escape through a generic object spread.
    const purposes = validPurposes(authorization.purposes);
    validatePurposeRoute(route, purposes);
    if ((credential.kind === 'openai_oauth'
      && (purposes.includes(COMPUTER_PURPOSE) || !supportsOpenAiOAuthRoute(route)))
      || (credential.kind === 'api_key' && supportsOpenAiOAuthRoute(route))) {
      throw new Error('invalid private model authorization');
    }
    const hasComputerPurpose = purposes.includes(COMPUTER_PURPOSE);
    const accountScope = hasComputerPurpose ? validateAccountScope(authorization.account_scope) : undefined;
    if (!hasComputerPurpose && authorization.account_scope !== undefined) {
      throw new Error('invalid private model authorization');
    }
    const bytes = Buffer.from(JSON.stringify({
      purposes,
      route,
      credential,
      ...(accountScope ? { account_scope: accountScope } : {}),
    }), 'utf8');
    try {
      if (bytes.length > 32 * 1024) throw new Error('private model authorization exceeds quota');
      const result = Buffer.allocUnsafe(bytes.length + 4);
      result.writeUInt32BE(bytes.length); bytes.copy(result, 4);
      return result;
    } finally { bytes.fill(0); }
  } finally {
    clearModelCredential(credential);
  }
}

export function supportsOpenAiOAuthRoute(route: ModelRoute): boolean {
  return typeof route.provider_id === 'object'
    && 'open_ai_compatible' in route.provider_id
    && route.provider_id.open_ai_compatible.name === 'openai-chatgpt'
    && route.protocol === 'open_ai_responses'
    && route.base_url === 'https://chatgpt.com/backend-api/codex'
    && route.max_output_tokens === 2048;
}

function validPurposes(value: unknown): ModelPurpose[] {
  const allowed = ['s2_planning', 's1_teacher', 'computer_proposals'] as const satisfies readonly ModelPurpose[];
  if (!Array.isArray(value) || value.length === 0 || value.some((item) => !allowed.includes(item))) {
    throw new Error('invalid private model authorization');
  }
  const unique = [...new Set(value as ModelPurpose[])];
  if (unique.length !== value.length) throw new Error('invalid private model authorization');
  return allowed.filter((item) => unique.includes(item));
}

function validatePurposeRoute(route: ModelRoute, purposes: readonly ModelPurpose[]): void {
  if (purposes.includes(COMPUTER_PURPOSE) && !supportsComputerRoute(route)) {
    throw new Error('invalid private model authorization');
  }
}

export function supportsComputerRoute(route: ModelRoute): boolean {
  return route.provider_id === 'open_ai' && route.protocol === 'open_ai_responses'
    && route.base_url === 'https://api.openai.com/v1';
}

function validateAccountScope(value: unknown): string {
  if (typeof value !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value)) {
    throw new Error('invalid private model authorization');
  }
  return value;
}

/** Provider keys, HOME, proxies and route override variables are not inherited. */
export function hostEnvironment(parent: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const result: NodeJS.ProcessEnv = {};
  for (const name of ['PATH', 'TMPDIR', 'LANG', 'LC_ALL', 'LC_CTYPE', 'TZ']) {
    if (typeof parent[name] === 'string') result[name] = parent[name];
  }
  return result;
}
