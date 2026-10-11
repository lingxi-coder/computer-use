import { EventEmitter } from 'node:events';
import { randomUUID } from 'node:crypto';
import type { ModelAuthorizationRequest, ModelAuthorizationStatus, ModelPurpose, ModelRoute, SavedModelConnection } from '@computer-use/desktop-api';
import {
  privateModelPrefix,
  supportsComputerRoute,
  supportsOpenAiOAuthRoute,
  validateModelKey,
  validateModelRoute,
  validateOpenAiOAuthCredential,
  type ModelAuthorization,
  type ModelCredential,
  type OpenAiOAuthSession,
} from './modelBootstrap.js';
import type { NativeSession } from './nativeSession.js';
import type { SupervisorOptions } from './supervisor.js';
import { ModelKeyStoreFailure, type ModelKeyStoreFailureCode } from './modelKeyStore.js';

export interface ModelKeyStore {
  readonly available: boolean;
  list(): Promise<SavedModelConnection[]>;
  save(route: ModelRoute, key: string): Promise<SavedModelConnection>;
  load(id: string): Promise<{ route: ModelRoute; api_key: string } | null>;
  forget(id: string): Promise<void>;
}
type Session = Pick<NativeSession, 'canConfigureModel' | 'lockConfiguration' | 'restartForModelChange' | 'on' | 'off'>;
type Confirm = (
  route: ModelRoute,
  storage: 'memory' | 'keychain',
  purposes: ModelPurpose[],
  signal: AbortSignal,
  credentialKind: 'api_key' | 'openai_oauth',
) => Promise<boolean>;
type OpenAiLogin = (request: { model: string }, signal: AbortSignal) => Promise<OpenAiOAuthSession>;
type LoadedAuthorization = { route: ModelRoute; credential: ModelCredential };
type CredentialLoader = (signal: AbortSignal) => Promise<LoadedAuthorization>;
type Events = { status: [ModelAuthorizationStatus]; configurationStop: [] };
const COMPUTER_PURPOSE = 'computer_proposals' satisfies ModelPurpose;
const MODEL_PURPOSES = ['s2_planning', 's1_teacher', COMPUTER_PURPOSE] as const satisfies readonly ModelPurpose[];
function clearCredential(value: ModelCredential): void {
  if (value.kind === 'api_key') value.api_key = '';
  else value.access_token = '';
}
function clearAuthorization(value: ModelAuthorization): void { clearCredential(value.credential); value.purposes.length = 0; value.account_scope = undefined; }
const unavailableStore: ModelKeyStore = { available: false, list: async () => [],
  save: async () => { throw new Error('Keychain is unavailable.'); }, load: async () => null, forget: async () => {} };
const unavailableOpenAiLogin: OpenAiLogin = async () => { throw new Error('OpenAI sign-in is unavailable.'); };

type ConnectionStage = 'consent' | 'saved_key_load' | 'credential_load' | 'oauth_login' | 'credential_validation' | 'host_replacement' | 'keychain_save';
const savedKeyReasons: Record<ModelKeyStoreFailureCode, string> = {
  auth_required: 'Keychain authentication is required to read the saved API key. No new task was sent.',
  locked: 'Keychain is locked or unavailable. No new task was sent.',
  not_found: 'The saved API key is missing from Keychain. Save it again before connecting. No new task was sent.',
  invalid: 'Keychain returned an invalid saved API key response. No new task was sent.',
  timeout: 'Keychain did not respond before the request timed out. No new task was sent.',
  unavailable: 'The saved API key could not be loaded from Keychain. No new task was sent.',
};
function connectionFailureReason(stage: ConnectionStage, error: unknown, cancelled: boolean): string {
  if (cancelled) return 'Model authorization cancelled. No new task was sent.';
  if (stage === 'saved_key_load') {
    if (error instanceof ModelKeyStoreFailure) {
      const code = error.code;
      return Object.hasOwn(savedKeyReasons, code) ? savedKeyReasons[code] : savedKeyReasons.unavailable;
    }
    return savedKeyReasons.unavailable;
  }
  if (stage === 'credential_validation') return 'The loaded model connection did not match the approved route. No new task was sent.';
  if (stage === 'host_replacement') return 'The model host could not be configured. No new task was sent.';
  if (stage === 'keychain_save') return 'The model connection could not be saved in Keychain. No new task was sent.';
  if (stage === 'oauth_login') return 'OpenAI sign-in could not be completed. No new task was sent.';
  if (stage === 'consent') return 'Native model consent could not be completed. No new task was sent.';
  return 'Model credentials could not be loaded. No new task was sent.';
}

/** Validate and canonically order the explicit, nonempty purpose set. */
export function modelPurposes(value: unknown): ModelPurpose[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > MODEL_PURPOSES.length
    || value.some((item) => typeof item !== 'string' || !MODEL_PURPOSES.includes(item as ModelPurpose))) {
    throw new Error('Invalid model connection. Check the endpoint, model and API key.');
  }
  const unique = new Set(value as ModelPurpose[]);
  if (unique.size !== value.length) throw new Error('Invalid model connection. Check the endpoint, model and API key.');
  return MODEL_PURPOSES.filter((purpose) => unique.has(purpose));
}

/** Validate the public route without granting or persisting any purpose. */
export function modelRoute(value: unknown): ModelRoute {
  const invalid = () => new Error('Invalid model connection. Check the endpoint, model and API key.');
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  const route = value as Record<string, unknown>;
  const keys = Object.keys(route).sort().join(',');
  if (keys !== 'base_url,max_output_tokens,model,protocol,provider_id'
    && keys !== 'base_url,max_output_tokens,model,preset_id,protocol,provider_id') throw invalid();
  try { return validateModelRoute(value); } catch { throw invalid(); }
}

/** Strict invoke boundary. Unknown fields cannot carry secrets into route metadata. */
export function modelRequest(value: unknown): ModelAuthorizationRequest {
  const invalid = () => new Error('Invalid model connection. Check the endpoint, model and API key.');
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  const v = value as Record<string, unknown>;
  if (Object.keys(v).sort().join(',') !== 'api_key,purposes,route,storage' || (v.storage !== 'memory' && v.storage !== 'keychain')
    || typeof v.api_key !== 'string') throw invalid();
  try {
    const route = modelRoute(v.route);
    const purposes = modelPurposes(v.purposes);
    validatePurposeRoute(route, purposes);
    return { route, api_key: validateModelKey(v.api_key), storage: v.storage, purposes };
  } catch { throw invalid(); }
}

function openAiLoginRequest(value: unknown): { model: string; purposes: ModelPurpose[] } {
  const invalid = () => new Error('Invalid OpenAI sign-in request.');
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  const request = value as Record<string, unknown>;
  if (Object.keys(request).sort().join(',') !== 'model,purposes'
    || typeof request.model !== 'string' || request.model.trim() !== request.model
    || request.model.length === 0 || Buffer.byteLength(request.model, 'utf8') > 256
    || /\p{Cc}/u.test(request.model)) throw invalid();
  let purposes: ModelPurpose[];
  try { purposes = modelPurposes(request.purposes); } catch { throw invalid(); }
  if (purposes.some((purpose) => purpose === COMPUTER_PURPOSE)) throw invalid();
  return { model: request.model, purposes };
}

function credentialExpired(value: ModelAuthorization | null): boolean {
  return value?.credential.kind === 'openai_oauth'
    && value.credential.expires_at <= Math.floor(Date.now() / 1000);
}

function validatePurposeRoute(route: ModelRoute, purposes: readonly ModelPurpose[]): void {
  if (purposes.includes(COMPUTER_PURPOSE) && !supportsComputerRoute(route)) {
    throw new Error('Computer proposals require the official OpenAI Responses endpoint.');
  }
}

/** Main owns outbound consent, in-memory credentials and native-session replacement.
 * Projections contain only routing facts; startup and saved keys never authorize calls. */
export class ModelAccess extends EventEmitter<Events> {
  private session: Session | null = null;
  private active: ModelAuthorization | null = null;
  private enabled = false;
  private phase: ModelAuthorizationStatus['phase'] = 'not_configured';
  private storage: 'memory' | 'keychain' | null = null;
  private activeProviderConnectionId: string | null = null;
  private saved: SavedModelConnection[] = [];
  private reason: string | null = null;
  private generation = 0;
  private prompt: AbortController | null = null;
  private closing = false;
  private change: Promise<void> | null = null;
  private connection: Promise<void> | null = null;
  private revokeTask: Promise<void> | null = null;
  private oauthExpiryTimer: NodeJS.Timeout | null = null;
  private detach: (() => void) | null = null;

  constructor(
    private readonly confirm: Confirm,
    private readonly store: ModelKeyStore = unavailableStore,
    private readonly fallbackPlanning = false,
    private readonly openAiLogin: OpenAiLogin = unavailableOpenAiLogin,
  ) { super(); }
  async initialize(): Promise<void> {
    try { this.saved = await this.store.list(); }
    catch { this.reason = 'Saved connections are unavailable. You can use an API key for this run.'; }
    this.publish();
  }
  attach(session: Session): void {
    this.session = session;
    const stop = (): void => {
      this.emit('configurationStop');
      if (this.phase === 'authorizing') {
        this.generation += 1; this.prompt?.abort(); this.reason = 'Model authorization cancelled.'; this.publish();
        if (this.active?.purposes.includes(COMPUTER_PURPOSE)) void this.revoke().catch(() => undefined);
      }
      else if (this.phase === 'switching' && !this.closing) void this.revoke().catch(() => undefined);
      else if (this.active?.purposes.includes(COMPUTER_PURPOSE)) void this.revoke().catch(() => undefined);
    };
    const project = (): void => this.publish();
    const exit = (): void => { if (this.active && !this.closing) void this.revoke().catch(() => undefined); };
    session.on('controlStop', stop); session.on('projection', project); session.on('nativeAuthorization', project); session.on('exit', exit);
    this.detach = () => { session.off('controlStop', stop); session.off('projection', project); session.off('nativeAuthorization', project); session.off('exit', exit); };
    this.publish();
  }
  get authorization(): ModelAuthorizationStatus {
    return { phase: this.phase, route: this.active ? structuredClone(this.active.route) : null, storage: this.storage,
      credential_kind: this.active?.credential.kind ?? null,
      authorized_purposes: this.phase === 'authorized' && this.active ? [...this.active.purposes] : [],
      can_authorize: !this.closing && this.phase !== 'authorizing' && this.phase !== 'switching' && this.session?.canConfigureModel === true,
      can_save: this.store.available, planning_available: !this.closing && ((this.phase === 'authorized' && this.active?.purposes.includes('s2_planning') === true) || (this.phase === 'not_configured' && this.fallbackPlanning)), saved: structuredClone(this.saved), reason: this.reason };
  }
  /** Main-only identity for the provider vault record backing the active host. */
  get providerConnectionId(): string | null { return this.activeProviderConnectionId; }
  private publish(): void { this.emit('status', this.authorization); }
  /** Only the main-owned process factory can obtain a startup key. */
  supervisorOptions(options: SupervisorOptions): SupervisorOptions {
    if (this.enabled && credentialExpired(this.active)) void this.revoke().catch(() => undefined);
    const active = this.enabled && !credentialExpired(this.active) ? this.active : null;
    if (!active) return { ...options, modelAuthorization: undefined };
    // Main retains the complete purpose consent and scope. The Host receives
    // the computer purpose only in the NativeSession replacement that carries
    // this session's freshly authorized window ID.
    const authorizedWindowId = options.nativeSource?.authorizedWindowId;
    const computerWindowAuthorized = typeof authorizedWindowId === 'string' && authorizedWindowId.length > 0;
    const purposes = computerWindowAuthorized
      ? [...active.purposes]
      : active.purposes.filter((purpose) => purpose !== COMPUTER_PURPOSE);
    if (purposes.length === 0) return { ...options, modelAuthorization: undefined };
    return { ...options, modelAuthorization: {
      route: structuredClone(active.route),
      credential: structuredClone(active.credential),
      purposes,
      ...(computerWindowAuthorized && active.account_scope ? { account_scope: active.account_scope } : {}),
    } };
  }
  private check(generation: number): void {
    if (this.closing || generation !== this.generation) throw new Error('Model authorization cancelled.');
  }
  async authorize(value: ModelAuthorizationRequest): Promise<void> {
    const request = modelRequest(value);
    if (request.storage === 'keychain' && !this.store.available) throw new Error('Keychain is unavailable. Use this API key in memory.');
    try {
      await this.connect(request.route, request.storage, request.purposes, async () => ({
        route: request.route,
        credential: { kind: 'api_key', api_key: request.api_key },
      }));
    }
    finally { request.api_key = ''; request.purposes.length = 0; }
  }
  async authorizeOpenAiLogin(value: { model: string; purposes: ModelPurpose[] }): Promise<void> {
    if (this.openAiLogin === unavailableOpenAiLogin) throw new Error('OpenAI sign-in is unavailable.');
    const request = openAiLoginRequest(value);
    request.purposes.length = 0;
    throw new Error('OpenAI account sign-in is managed by Provider Settings.');
  }
  /** Main-only activation for a provider already configured with native purpose consent.
   * The only caller is ProviderSettings after it has retained that approval. */
  async activateProvider(routeValue: ModelRoute, credentialValue: ModelCredential, purposesValue: ModelPurpose[], storage: 'memory' | 'keychain', providerConnectionId: string): Promise<void> {
    const route = modelRoute(routeValue);
    const purposes = modelPurposes(purposesValue);
    validatePurposeRoute(route, purposes);
    if (typeof providerConnectionId !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(providerConnectionId)) {
      purposes.length = 0;
      throw new Error('The configured provider identity is unavailable.');
    }
    if (storage !== 'memory' && storage !== 'keychain') {
      purposes.length = 0;
      throw new Error('The configured provider storage choice is unavailable.');
    }
    let credential: ModelCredential;
    if (credentialValue?.kind === 'api_key') {
      credential = { kind: 'api_key', api_key: validateModelKey(credentialValue.api_key) };
    } else if (credentialValue?.kind === 'openai_oauth') {
      credential = validateOpenAiOAuthCredential(credentialValue);
    } else {
      purposes.length = 0;
      throw new Error('The configured provider credential is unavailable.');
    }
    if (credential.kind === 'openai_oauth' && storage !== 'keychain') {
      clearCredential(credential);
      purposes.length = 0;
      throw new Error('OpenAI account sessions must use Keychain storage.');
    }
    try {
      await this.connect(route, storage, purposes, async () => ({
        route,
        credential: structuredClone(credential),
      }), false, 'credential_load', credential.kind, true, providerConnectionId);
    } finally {
      clearCredential(credential);
      purposes.length = 0;
    }
  }
  async authorizeSaved(id: string, purposesValue: unknown): Promise<void> {
    const purposes = modelPurposes(purposesValue);
    try {
      const saved = this.saved.find((s) => s.id === id);
      if (!saved) throw new Error('Saved connection is unavailable.');
      if (supportsOpenAiOAuthRoute(saved.route)) {
        throw new Error('Reconnect the saved OpenAI account from Provider Settings.');
      }
      // Read the key only after native outbound consent. It never enters renderer.
      await this.connect(saved.route, 'keychain', purposes, async () => {
        const key = await this.store.load(id);
        if (!key) throw new ModelKeyStoreFailure('not_found');
        if (JSON.stringify(key.route) !== JSON.stringify(saved.route)) {
          key.api_key = '';
          throw new Error('Saved connection changed. Review it again.');
        }
        return { route: key.route, credential: { kind: 'api_key', api_key: key.api_key } };
      }, false, 'saved_key_load');
    } finally { purposes.length = 0; }
  }
  private async connect(
    route: ModelRoute,
    storage: 'memory' | 'keychain',
    purposes: ModelPurpose[],
    load: CredentialLoader,
    save = true,
    loadStage: 'credential_load' | 'saved_key_load' | 'oauth_login' = 'credential_load',
    credentialKind: 'api_key' | 'openai_oauth' = 'api_key',
    skipConsent = false,
    providerConnectionId: string | null = null,
  ): Promise<void> {
    if (this.connection) throw new Error('Model configuration is busy.');
    const connection = this.connectInner(route, storage, purposes, load, save, loadStage, credentialKind, skipConsent, providerConnectionId); this.connection = connection;
    try { await connection; } finally { if (this.connection === connection) this.connection = null; }
  }
  private async connectInner(
    route: ModelRoute,
    storage: 'memory' | 'keychain',
    purposes: ModelPurpose[],
    load: CredentialLoader,
    save: boolean,
    loadStage: 'credential_load' | 'saved_key_load' | 'oauth_login',
    credentialKind: 'api_key' | 'openai_oauth',
    skipConsent: boolean,
    providerConnectionId: string | null,
  ): Promise<void> {
    validatePurposeRoute(route, purposes);
    if (!this.authorization.can_authorize || !this.session) throw new Error('Finish or stop the current task before changing the model connection.');
    const previousActive = this.active;
    const release = this.session.lockConfiguration();
    const generation = ++this.generation;
    const prompt = new AbortController(); this.prompt = prompt;
    this.phase = 'authorizing'; this.reason = null; this.publish();
    let next: ModelAuthorization | null = null;
    let stage: ConnectionStage = 'consent';
    try {
      const approved = skipConsent || await this.confirm(structuredClone(route), storage, [...purposes], prompt.signal, credentialKind);
      this.check(generation);
      if (!approved) return;
      stage = loadStage;
      const loaded = await load(prompt.signal);
      stage = 'credential_validation';
      let credential: ModelCredential;
      try {
        if (JSON.stringify(modelRoute(loaded.route)) !== JSON.stringify(modelRoute(route))) {
          throw new Error('The loaded model route does not match the approved route.');
        }
        if (loaded.credential.kind === 'api_key') {
          credential = { kind: 'api_key', api_key: validateModelKey(loaded.credential.api_key) };
        } else {
          credential = validateOpenAiOAuthCredential(loaded.credential);
        }
      } finally { clearCredential(loaded.credential); }
      next = {
        route: modelRoute(loaded.route),
        credential,
        purposes: [...purposes],
        ...(purposes.includes(COMPUTER_PURPOSE) ? { account_scope: randomUUID() } : {}),
      };
      this.check(generation);
      privateModelPrefix(next).fill(0);
      this.phase = 'switching'; this.enabled = false; this.publish();
      stage = 'host_replacement';
      // Keep old key alive only until its host has cleaned up. The factory
      // receives the new key when the old pipes have closed.
      const previous = this.active;
      this.active = next; this.enabled = true; this.storage = storage;
      this.scheduleOAuthExpiry(next, generation);
      const change = this.session.restartForModelChange(); this.change = change;
      try { await change; } finally { if (this.change === change) this.change = null; if (previous) clearAuthorization(previous); }
      this.check(generation);
      if (storage === 'keychain' && save && next.credential.kind === 'api_key') {
        stage = 'keychain_save'; await this.store.save(route, next.credential.api_key); this.saved = await this.store.list(); this.check(generation);
      }
      this.activeProviderConnectionId = providerConnectionId;
      this.phase = 'authorized'; this.reason = null; this.publish();
    } catch (error) {
      if (generation === this.generation) {
        if (this.phase === 'switching') {
          this.enabled = false;
          await this.session.restartForModelChange().catch(() => undefined);
          if (this.active) clearAuthorization(this.active); this.active = null; this.storage = null; this.activeProviderConnectionId = null; this.phase = 'unavailable';
        }
        else this.phase = this.active ? 'authorized' : 'not_configured';
        this.reason = connectionFailureReason(stage, error, prompt.signal.aborted || this.closing || generation !== this.generation); this.publish();
      }
      throw new Error('Model connection was cancelled or could not be opened.');
    } finally {
      if (next && next !== this.active) clearAuthorization(next);
      if (this.prompt === prompt) this.prompt = null;
      if (this.phase === 'authorizing') this.phase = this.active ? 'authorized' : 'not_configured';
      if (previousActive && this.active === previousActive && this.phase === 'authorized' && !this.closing) {
        this.scheduleOAuthExpiry(previousActive, this.generation);
      }
      purposes.length = 0; release(); this.publish();
    }
  }
  private scheduleOAuthExpiry(value: ModelAuthorization, generation: number): void {
    if (this.oauthExpiryTimer) clearTimeout(this.oauthExpiryTimer);
    this.oauthExpiryTimer = null;
    if (value.credential.kind !== 'openai_oauth') return;
    const expiresAt = value.credential.expires_at;
    const expire = (): void => {
      if (this.active !== value || this.generation !== generation) return;
      const remaining = expiresAt * 1000 - Date.now();
      if (remaining <= 0) {
        void this.revoke().catch(() => undefined);
        return;
      }
      this.oauthExpiryTimer = setTimeout(expire, Math.min(remaining, 2_147_000_000));
      this.oauthExpiryTimer.unref?.();
    };
    expire();
  }
  revoke(): Promise<void> {
    if (this.revokeTask) return this.revokeTask;
    const task = this.revokeConnection(); this.revokeTask = task;
    void task.finally(() => { if (this.revokeTask === task) this.revokeTask = null; }).catch(() => undefined);
    return task;
  }
  private async revokeConnection(): Promise<void> {
    if (this.closing || !this.session) return;
    const generation = ++this.generation; this.prompt?.abort(); this.enabled = false;
    if (this.oauthExpiryTimer) clearTimeout(this.oauthExpiryTimer); this.oauthExpiryTimer = null;
    const release = this.session.lockConfiguration(true);
    this.phase = 'switching'; this.reason = 'Closing the model connection and cancelling the task…'; this.publish();
    const previous = this.active;
    try {
      // Invalidate any replacement in progress, then close its host as well.
      await this.session.restartForModelChange();
      // Let a cancelled Keychain write finish before Forget can delete its
      // account. Otherwise a late save could resurrect an explicitly removed key.
      await this.connection?.catch(() => undefined); this.check(generation);
      this.active = null; this.storage = null; this.activeProviderConnectionId = null; this.phase = 'not_configured'; this.reason = null;
    } catch {
      if (generation === this.generation) { this.active = null; this.storage = null; this.activeProviderConnectionId = null; this.phase = 'unavailable'; this.reason = 'Connection closed. Restart observation before connecting again.'; }
      throw new Error('The model connection is closed; observation could not restart.');
    } finally { if (previous) clearAuthorization(previous); release(); this.publish(); }
  }
  async forgetSaved(id: string): Promise<void> {
    if (!this.saved.some((s) => s.id === id)) throw new Error('Saved connection is unavailable.');
    await this.revoke();
    await this.store.forget(id); this.saved = await this.store.list(); this.publish();
  }
  async shutdown(closeSession: () => Promise<void>): Promise<void> {
    this.closing = true; this.generation += 1; this.prompt?.abort(); this.enabled = false; this.detach?.(); this.detach = null;
    if (this.oauthExpiryTimer) clearTimeout(this.oauthExpiryTimer); this.oauthExpiryTimer = null;
    await closeSession(); await this.change?.catch(() => undefined); await this.connection?.catch(() => undefined); await this.revokeTask?.catch(() => undefined);
    if (this.active) clearAuthorization(this.active); this.active = null; this.storage = null; this.activeProviderConnectionId = null;
  }
}
