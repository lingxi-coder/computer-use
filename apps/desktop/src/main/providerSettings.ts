import { randomUUID } from 'node:crypto';
import { EventEmitter } from 'node:events';
import type {
  ConfigureProviderRequest,
  ModelPurpose,
  ModelProviderDefinition,
  ProviderConnection,
  ProviderModel,
  ProviderSettingsSnapshot,
  SavedModelConnection,
  TaskModelSelection,
} from '@computer-use/desktop-api';
import { ModelAccess, modelPurposes, modelRoute, type ModelKeyStore } from './modelAccess.js';
import { connectionId } from './modelKeyStore.js';
import {
  supportsComputerRoute,
  supportsOpenAiOAuthRoute,
  validateModelKey,
  validateOpenAiOAuthCredential,
  validateOpenAiOAuthSession,
  type ModelCredential,
  type OpenAiOAuthCredential,
  type OpenAiOAuthSession,
} from './modelBootstrap.js';
import type { OpenAiOAuthRefresh } from './openAiLogin.js';

type Storage = 'memory' | 'keychain';
type ConsentProvider = Pick<ModelProviderDefinition, 'id' | 'label' | 'auth' | 'route'>;
type ConfirmProvider = (
  provider: ConsentProvider,
  storage: Storage,
  purposes: ModelPurpose[],
  signal: AbortSignal,
  credentialKind: ModelCredential['kind'],
) => Promise<boolean>;
export type ProviderCatalogLoader = () => Promise<ModelProviderDefinition[]>;
export type ProviderModelRefresh = (
  provider: ModelProviderDefinition,
  credential: ModelCredential,
  signal: AbortSignal,
) => Promise<ProviderModel[]>;
export type ProviderCredential = { kind: 'api_key'; api_key: string } | OpenAiOAuthSession;
export type ProviderLogin = (provider: ModelProviderDefinition, signal: AbortSignal) => Promise<ProviderCredential>;
type Events = { status: [ProviderSettingsSnapshot] };

interface ProviderRecord {
  id: string;
  providerId: string;
  storage: Storage;
  storageId: string | null;
  purposes: ModelPurpose[];
  credential: ProviderCredential | null;
  models: ProviderModel[];
  refreshedAt: number | null;
  catalogSource: 'sdk' | 'live';
  reason: string | null;
  expiryTimer: NodeJS.Timeout | null;
}

const MAX_PROVIDERS = 128;
const MAX_MODELS = 4096;
const PROVIDER_CREDENTIAL_MODEL = '__provider_credentials__';
const EXPIRED_REASON = 'The provider sign-in expired. Configure this provider again.';
const REFRESHABLE_EXPIRED_REASON = 'The OpenAI access token expired and will refresh before the next connection is used.';
const CATALOG_REFRESH_REASON = 'The live model list could not be refreshed. The last available list is retained.';
const CANCELLED_REASON = 'Provider configuration was cancelled.';
const UNAVAILABLE_REASON = 'The provider credential is unavailable. Configure this provider again.';
const SAFE_INTEGER_MAX_MS = 2_147_000_000;
const OAUTH_REFRESH_SKEW_SECONDS = 120;

function safeText(value: unknown, maxBytes: number): value is string {
  return typeof value === 'string' && value.length > 0 && value.trim() === value
    && Buffer.byteLength(value, 'utf8') <= maxBytes && !/\p{Cc}/u.test(value);
}

function exactKeys(value: Record<string, unknown>, required: readonly string[], optional: readonly string[] = []): boolean {
  const keys = Object.keys(value).sort();
  const allowed = new Set([...required, ...optional]);
  return required.every((key) => keys.includes(key)) && keys.every((key) => allowed.has(key));
}

function routeWithModel(provider: ModelProviderDefinition, model: string) {
  return modelRoute({ ...provider.route, model });
}

function storageAnchor(provider: ModelProviderDefinition) {
  return routeWithModel(provider, PROVIDER_CREDENTIAL_MODEL);
}

function sameRoute(left: SavedModelConnection['route'], right: SavedModelConnection['route']): boolean {
  return JSON.stringify(left.provider_id) === JSON.stringify(right.provider_id)
    && left.protocol === right.protocol && left.base_url === right.base_url
    && left.model === right.model && left.max_output_tokens === right.max_output_tokens
    && left.preset_id === right.preset_id;
}

function validateModels(value: unknown): ProviderModel[] {
  if (!Array.isArray(value) || value.length > MAX_MODELS) throw new Error('invalid provider model list');
  const seen = new Set<string>();
  return value.map((item) => {
    if (!item || typeof item !== 'object' || Array.isArray(item)) throw new Error('invalid provider model list');
    const row = item as Record<string, unknown>;
    if (!exactKeys(row, ['id', 'label', 'selectable']) || !safeText(row.id, 256)
      || !safeText(row.label, 512) || typeof row.selectable !== 'boolean' || seen.has(row.id)) {
      throw new Error('invalid provider model list');
    }
    seen.add(row.id);
    return { id: row.id, label: row.label, selectable: row.selectable };
  });
}

function validateProviderCatalog(value: unknown): ModelProviderDefinition[] {
  if (!Array.isArray(value) || value.length > MAX_PROVIDERS) throw new Error('invalid provider catalog');
  const ids = new Set<string>();
  return value.map((item) => {
    if (!item || typeof item !== 'object' || Array.isArray(item)) throw new Error('invalid provider catalog');
    const row = item as Record<string, unknown>;
    if (!exactKeys(row, ['id', 'label', 'description', 'route', 'auth', 'models', 'directory_supported'])
      || !safeText(row.id, 128) || !/^[a-z0-9][a-z0-9._-]*$/.test(row.id)
      || !safeText(row.label, 256) || typeof row.description !== 'string'
      || Buffer.byteLength(row.description, 'utf8') > 2048 || /\p{Cc}/u.test(row.description)
      || (row.auth !== 'api_key' && row.auth !== 'openai_oauth') || typeof row.directory_supported !== 'boolean'
      || !row.route || typeof row.route !== 'object' || Array.isArray(row.route) || ids.has(row.id)) {
      throw new Error('invalid provider catalog');
    }
    const routeRow = row.route as Record<string, unknown>;
    if (!exactKeys(routeRow, ['provider_id', 'protocol', 'base_url', 'max_output_tokens', 'preset_id'])
      || routeRow.preset_id !== row.id) {
      throw new Error('invalid provider catalog');
    }
    const route = modelRoute({ ...routeRow, model: PROVIDER_CREDENTIAL_MODEL });
    if ((row.auth === 'openai_oauth' && !supportsOpenAiOAuthRoute(route))
      || (row.auth === 'api_key' && supportsOpenAiOAuthRoute(route))) {
      throw new Error('invalid provider catalog');
    }
    const models = validateModels(row.models);
    ids.add(row.id);
    return {
      id: row.id,
      label: row.label,
      description: row.description,
      route: {
        provider_id: route.provider_id,
        protocol: route.protocol,
        base_url: route.base_url,
        max_output_tokens: route.max_output_tokens,
        ...(route.preset_id === undefined ? {} : { preset_id: route.preset_id }),
      },
      auth: row.auth,
      models,
      directory_supported: row.directory_supported,
    };
  });
}

function validateConfigureRequest(value: unknown): {
  provider_id: string; storage: Storage; purposes: ModelPurpose[]; api_key?: string; reauthenticate?: boolean;
} {
  const invalid = () => new Error('Invalid provider configuration.');
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  const row = value as Record<string, unknown>;
  if (!exactKeys(row, ['provider_id', 'storage', 'purposes'], ['api_key', 'reauthenticate'])
    || !safeText(row.provider_id, 128) || (row.storage !== 'memory' && row.storage !== 'keychain')) throw invalid();
  let purposes: ModelPurpose[];
  try { purposes = modelPurposes(row.purposes); } catch { throw invalid(); }
  if (row.api_key !== undefined && typeof row.api_key !== 'string') throw invalid();
  if (row.reauthenticate !== undefined && typeof row.reauthenticate !== 'boolean') throw invalid();
  return {
    provider_id: row.provider_id,
    storage: row.storage,
    purposes,
    ...(typeof row.api_key === 'string' ? { api_key: row.api_key } : {}),
    ...(typeof row.reauthenticate === 'boolean' ? { reauthenticate: row.reauthenticate } : {}),
  };
}

function cloneCredential(value: ProviderCredential): ProviderCredential {
  if (value.kind === 'api_key') return { kind: 'api_key', api_key: validateModelKey(value.api_key) };
  return validateOpenAiOAuthSession(value);
}

function clearCredential(value: ProviderCredential | null): void {
  if (!value) return;
  if (value.kind === 'api_key') value.api_key = '';
  else { value.access_token = ''; if ('refresh_token' in value) value.refresh_token = ''; }
}

function validateCredential(value: unknown, auth: ModelProviderDefinition['auth']): ProviderCredential {
  if (auth === 'api_key') {
    if (!value || typeof value !== 'object' || Array.isArray(value)
      || Object.keys(value).sort().join(',') !== 'api_key,kind'
      || (value as Record<string, unknown>).kind !== 'api_key') throw new Error('invalid provider credential');
    return { kind: 'api_key', api_key: validateModelKey((value as { api_key: unknown }).api_key) };
  }
  const credential = validateOpenAiOAuthSession(value);
  if (credential.kind !== 'openai_oauth') throw new Error('invalid provider credential');
  return credential;
}

function accessCredential(value: ProviderCredential): ModelCredential {
  if (value.kind === 'api_key') return { kind: 'api_key', api_key: validateModelKey(value.api_key) };
  const access: OpenAiOAuthCredential = validateOpenAiOAuthCredential({
    kind: value.kind,
    access_token: value.access_token,
    expires_at: value.expires_at,
    ...(value.account_id === undefined ? {} : { account_id: value.account_id }),
    fedramp: value.fedramp,
  });
  return access;
}

function encodeOAuthSession(value: OpenAiOAuthSession): string {
  return validateModelKey(JSON.stringify(validateOpenAiOAuthSession(value)));
}

function decodeOAuthSession(value: string): OpenAiOAuthSession {
  if (typeof value !== 'string' || Buffer.byteLength(value, 'utf8') > 16 * 1024) {
    throw new Error('The saved OpenAI account session is unavailable.');
  }
  let parsed: unknown;
  try { parsed = JSON.parse(value); } catch { throw new Error('The saved OpenAI account session is invalid.'); }
  try { return validateOpenAiOAuthSession(parsed); }
  catch { throw new Error('The saved OpenAI account session is invalid.'); }
}

function validatePurposesForProvider(provider: ModelProviderDefinition, purposes: ModelPurpose[]): void {
  if (purposes.includes('computer_proposals') && !supportsComputerRoute(routeWithModel(provider, PROVIDER_CREDENTIAL_MODEL))) {
    throw new Error('Computer proposals require the official OpenAI Responses provider.');
  }
}

function publicConsentProvider(provider: ModelProviderDefinition): ConsentProvider {
  return {
    id: provider.id,
    label: provider.label,
    auth: provider.auth,
    route: structuredClone(provider.route),
  };
}

function publicConnection(record: ProviderRecord): ProviderConnection {
  const credentialAvailable = record.credential !== null
    && (record.credential.kind !== 'openai_oauth'
      || record.credential.expires_at > Math.floor(Date.now() / 1000)
      || Boolean(record.credential.refresh_token));
  return {
    id: record.id,
    provider_id: record.providerId,
    storage: record.storage,
    purposes: [...record.purposes],
    available: credentialAvailable,
    models: structuredClone(record.models),
    refreshed_at: record.refreshedAt,
    catalog_source: record.catalogSource,
    reason: record.reason,
  };
}

/** Main-owned vault. It publishes only provider/model metadata, never credentials or credential routes. */
export class ProviderSettings extends EventEmitter<Events> {
  private providers: ModelProviderDefinition[] = [];
  private records = new Map<string, ProviderRecord>();
  private savedMetadata: SavedModelConnection[] = [];
  private selected: ProviderSettingsSnapshot['selected'] = null;
  private busy = false;
  private closing = false;
  private reason: string | null = null;
  private operationController: AbortController | null = null;
  private operation: Promise<unknown> | null = null;

  constructor(
    private readonly models: ModelAccess,
    private readonly store: ModelKeyStore,
    private readonly catalogLoader: ProviderCatalogLoader,
    private readonly refreshLoader: ProviderModelRefresh,
    private readonly loginProvider: ProviderLogin,
    private readonly confirmProvider: ConfirmProvider,
    private readonly refreshOAuth?: OpenAiOAuthRefresh,
  ) {
    super();
    this.models.on('configurationStop', this.onStop);
    this.models.on('status', this.onModelStatus);
  }

  get snapshot(): ProviderSettingsSnapshot {
    return {
      providers: structuredClone(this.providers),
      connections: [...this.records.values()].map(publicConnection),
      selected: this.selected ? { ...this.selected } : null,
      keychain_available: this.store.available,
      busy: this.busy,
      can_configure: !this.closing && !this.busy && this.models.authorization.can_authorize,
      reason: this.reason,
    };
  }

  private publish(): void { this.emit('status', this.snapshot); }

  private readonly onStop = (): void => { this.operationController?.abort(); };

  private readonly onModelStatus = (): void => {
    if (!this.busy) this.reconcileSelected();
    this.publish();
  };

  private reconcileSelected(): void {
    if (!this.selected) return;
    const record = this.records.get(this.selected.connection_id);
    const provider = record && this.provider(record.providerId);
    const active = this.models.authorization;
    const expected = record && provider ? routeWithModel(provider, this.selected.model) : null;
    if (!record || !provider || active.phase === 'not_configured' || active.phase === 'unavailable'
      || (active.phase === 'authorized' && (JSON.stringify(active.route) !== JSON.stringify(expected)
        || this.models.providerConnectionId !== this.selected.connection_id))) {
      this.selected = null;
    }
  }

  private provider(id: string): ModelProviderDefinition | undefined { return this.providers.find((item) => item.id === id); }

  private async run<T>(work: (signal: AbortSignal) => Promise<T>): Promise<T> {
    if (this.closing || this.busy) throw new Error('Provider settings are busy.');
    if (!this.models.authorization.can_authorize) throw new Error('Finish or stop the current task before changing provider settings.');
    this.busy = true;
    this.reason = null;
    const controller = new AbortController();
    this.operationController = controller;
    this.publish();
    const operation = Promise.resolve().then(() => work(controller.signal));
    this.operation = operation;
    try { return await operation; }
    catch (error) {
      if (controller.signal.aborted) throw new Error(CANCELLED_REASON);
      throw error;
    } finally {
      if (this.operation === operation) this.operation = null;
      if (this.operationController === controller) this.operationController = null;
      this.busy = false;
      this.publish();
    }
  }

  private ensureCurrent(signal: AbortSignal): void {
    if (signal.aborted || this.closing) throw new Error(CANCELLED_REASON);
    if (!this.models.authorization.can_authorize) throw new Error('Provider settings are no longer available.');
  }

  async initialize(): Promise<void> {
    const [catalogResult, savedResult] = await Promise.allSettled([this.catalogLoader(), this.store.list()]);
    if (catalogResult.status === 'fulfilled') {
      try { this.providers = validateProviderCatalog(catalogResult.value); }
      catch { this.providers = []; this.reason = 'The provider catalog is invalid.'; }
    } else {
      this.providers = [];
      this.reason = 'The provider catalog is unavailable.';
    }
    this.savedMetadata = savedResult.status === 'fulfilled' ? savedResult.value : [];
    if (savedResult.status === 'rejected' && !this.reason) this.reason = 'Saved provider credentials are unavailable.';
    if (this.providers.length > 0) {
      for (const saved of this.savedMetadata) {
        const provider = this.providers.find((item) => sameRoute(saved.route, storageAnchor(item)));
        if (!provider || this.records.has(saved.id)) continue;
        const oauth = provider.auth === 'openai_oauth';
        this.records.set(saved.id, {
          id: saved.id,
          providerId: provider.id,
          storage: 'keychain',
          storageId: saved.id,
          purposes: [],
          credential: null,
          models: structuredClone(provider.models),
          refreshedAt: null,
          catalogSource: 'sdk',
          reason: oauth
            ? 'Reconnect after reviewing provider purposes to load this saved OpenAI account session.'
            : 'Reconnect after reviewing provider purposes to load this saved credential.',
          expiryTimer: null,
        });
      }
    }
    this.publish();
  }

  private rememberSaved(saved: SavedModelConnection): void {
    this.savedMetadata = [...this.savedMetadata.filter((row) => row.id !== saved.id), structuredClone(saved)];
  }

  /** A Keychain helper write cannot be interrupted once it starts. Restore the previous item if Stop lands during it. */
  private async rollbackCancelledWrite(
    id: string,
    route: SavedModelConnection['route'],
    previous: { route: SavedModelConnection['route']; api_key: string } | null,
  ): Promise<void> {
    let restored = false;
    let restoredPrevious = false;
    if (previous) {
      try {
        const saved = await this.store.save(previous.route, previous.api_key);
        restored = saved.id === id && sameRoute(saved.route, route);
        restoredPrevious = restored;
      } catch { /* The record below is marked unavailable if restoration fails. */ }
    }
    if (!restored) {
      try { await this.store.forget(id); restored = true; }
      catch { /* Keep metadata but clear any stale in-memory credential. */ }
    }
    if (restoredPrevious) this.rememberSaved({ id, route: structuredClone(route) });
    else if (restored) this.savedMetadata = this.savedMetadata.filter((row) => row.id !== id);
    const record = this.records.get(id);
    if (record) {
      this.clearRecord(record);
      record.reason = 'The saved credential changed during cancellation. Reconnect before using it.';
    }
  }

  private async saveOAuthSession(
    provider: ModelProviderDefinition,
    session: OpenAiOAuthSession,
    signal: AbortSignal,
    previous: { route: SavedModelConnection['route']; api_key: string } | null,
    persistRotatedToken = false,
  ): Promise<SavedModelConnection> {
    const route = storageAnchor(provider);
    const id = connectionId(route);
    const saved = await this.store.save(route, encodeOAuthSession(session));
    if (saved.id !== id || !sameRoute(saved.route, route)) {
      await this.rollbackCancelledWrite(id, route, previous);
      throw new Error('The OpenAI account session could not be saved.');
    }
    if (signal.aborted && !persistRotatedToken) {
      await this.rollbackCancelledWrite(id, route, previous);
      throw new Error(CANCELLED_REASON);
    }
    this.rememberSaved(saved);
    const record = this.records.get(id);
    if (record) {
      this.clearRecord(record);
      record.storageId = saved.id;
      if (signal.aborted && persistRotatedToken) {
        record.reason = 'The saved OpenAI account session was refreshed. Review purposes and reconnect before using it.';
      } else {
        record.credential = cloneCredential(session);
        record.reason = null;
        this.scheduleExpiry(record);
      }
    }
    return saved;
  }

  private async refreshOAuthSession(
    provider: ModelProviderDefinition,
    session: OpenAiOAuthSession,
    signal: AbortSignal,
  ): Promise<OpenAiOAuthSession> {
    if (!session.refresh_token || !this.refreshOAuth) throw new Error(UNAVAILABLE_REASON);
    const input = cloneCredential(session);
    if (input.kind !== 'openai_oauth') throw new Error(UNAVAILABLE_REASON);
    let refreshed: OpenAiOAuthSession | null = null;
    try {
      refreshed = validateOpenAiOAuthSession(await this.refreshOAuth(input, signal));
      const active = accessCredential(refreshed); // Reject an expired/malformed helper response before saving it.
      clearCredential(active);
      // A provider may rotate refresh tokens even if Stop arrives just after the reply.
      // Persist the rotation before honoring cancellation so the previous token is never lost.
      await this.saveOAuthSession(provider, refreshed, signal, null, true);
      return refreshed;
    } catch {
      if (refreshed) clearCredential(refreshed);
      throw signal.aborted ? new Error(CANCELLED_REASON) : new Error('The saved OpenAI account could not be refreshed.');
    } finally { clearCredential(input); }
  }

  async configure(value: ConfigureProviderRequest): Promise<void> {
    const raw = value && typeof value === 'object' ? value as { api_key?: unknown } : null;
    let request: ReturnType<typeof validateConfigureRequest>;
    try { request = validateConfigureRequest(value); }
    catch {
      if (raw && typeof raw.api_key === 'string') raw.api_key = '';
      throw new Error('Invalid provider configuration.');
    }
    let pendingCredential: ProviderCredential | null = null;
    try {
      await this.run(async (signal) => {
        const provider = this.provider(request.provider_id);
        if (!provider) throw new Error('The provider is unavailable.');
        validatePurposesForProvider(provider, request.purposes);
        if (provider.auth === 'openai_oauth' && (request.storage !== 'keychain' || request.api_key !== undefined)) {
          throw new Error('OpenAI account sessions must use Keychain storage.');
        }
        if (request.reauthenticate === true && provider.auth !== 'openai_oauth') {
          throw new Error('Only OpenAI account sessions can be reauthenticated.');
        }
        if (request.storage === 'keychain' && !this.store.available) {
          throw new Error('Keychain storage is unavailable.');
        }
        if (provider.auth === 'api_key' && request.api_key === undefined && request.storage !== 'keychain') {
          throw new Error('Enter an API key for this provider.');
        }
        if (provider.auth === 'api_key' && request.api_key !== undefined) validateModelKey(request.api_key);
        const anchor = storageAnchor(provider);
        const existingKeychainId = request.storage === 'keychain' ? connectionId(anchor) : null;
        const selectedRecord = this.selected ? this.records.get(this.selected.connection_id) : undefined;
        if (existingKeychainId && (this.selected?.connection_id === existingKeychainId
          || selectedRecord?.providerId === provider.id)) {
          throw new Error('Select another provider before replacing this active credential.');
        }
        const consent = await this.confirmProvider(
          publicConsentProvider(provider), request.storage, [...request.purposes], signal,
          provider.auth === 'api_key' ? 'api_key' : 'openai_oauth',
        );
        this.ensureCurrent(signal);
        if (!consent) {
          this.reason = CANCELLED_REASON;
          throw new Error(CANCELLED_REASON);
        }

        let credential!: ProviderCredential;
        let storageId: string | null = null;
        let publicId: string;
        if (provider.auth === 'openai_oauth') {
          const saved = this.savedMetadata.find((row) => sameRoute(row.route, anchor)) ?? null;
          let previous: { route: SavedModelConnection['route']; api_key: string } | null = null;
          let session: OpenAiOAuthSession | null = null;
          try {
            if (saved) {
              previous = await this.store.load(saved.id);
              if (previous && !sameRoute(previous.route, anchor)) {
                previous.api_key = '';
                throw new Error('The saved OpenAI account route changed.');
              }
            }
            if (previous && request.reauthenticate !== true) {
              session = decodeOAuthSession(previous.api_key);
              const shouldRefresh = session.expires_at <= Math.floor(Date.now() / 1000) + OAUTH_REFRESH_SKEW_SECONDS;
              if (shouldRefresh) {
                if (session.refresh_token) {
                  const refreshed = await this.refreshOAuthSession(provider, session, signal);
                  clearCredential(session);
                  session = refreshed;
                } else {
                  clearCredential(session);
                  session = null;
                }
              }
            }
            if (!session) {
              this.ensureCurrent(signal);
              const supplied = await this.loginProvider(structuredClone(provider), signal);
              try { credential = validateCredential(supplied, provider.auth); }
              finally { clearCredential(supplied); }
              pendingCredential = credential;
              if (credential.kind !== 'openai_oauth') throw new Error('The OpenAI account session is invalid.');
              const activeCredential = accessCredential(credential);
              clearCredential(activeCredential);
              this.ensureCurrent(signal);
              const persisted = await this.saveOAuthSession(provider, credential, signal, previous);
              storageId = persisted.id;
              publicId = persisted.id;
            } else {
              if (!saved) throw new Error('The saved OpenAI account session is unavailable.');
              credential = session;
              session = null;
              pendingCredential = credential;
              this.ensureCurrent(signal);
              storageId = saved!.id;
              publicId = saved!.id;
            }
          } finally {
            if (session) clearCredential(session);
            if (previous) previous.api_key = '';
          }
        } else if (request.api_key !== undefined) {
          credential = validateCredential({ kind: 'api_key', api_key: request.api_key }, provider.auth);
          pendingCredential = credential;
          if (request.storage === 'keychain') {
            if (credential.kind !== 'api_key') throw new Error('The provider credential is invalid.');
            const id = connectionId(anchor);
            const previous = await this.store.load(id);
            try {
              if (previous && !sameRoute(previous.route, anchor)) throw new Error('The saved provider route changed.');
              const saved = await this.store.save(anchor, credential.api_key);
              storageId = saved.id;
              if (!sameRoute(saved.route, anchor) || saved.id !== connectionId(anchor)) {
                await this.rollbackCancelledWrite(id, anchor, previous);
                throw new Error('The provider credential could not be saved.');
              }
              if (signal.aborted) {
                await this.rollbackCancelledWrite(id, anchor, previous);
                throw new Error(CANCELLED_REASON);
              }
              this.rememberSaved(saved);
              publicId = saved.id;
            } finally {
              if (previous) previous.api_key = '';
            }
          } else {
            publicId = randomUUID();
          }
          this.ensureCurrent(signal);
        } else {
          const saved = this.savedMetadata.find((row) => sameRoute(row.route, anchor));
          if (!saved) throw new Error('No saved credential is available for this provider.');
          const loaded = await this.store.load(saved.id);
          if (!loaded) throw new Error('The saved provider credential is missing.');
          try {
            if (!sameRoute(loaded.route, anchor)) throw new Error('The saved provider route changed.');
            credential = validateCredential({ kind: 'api_key', api_key: loaded.api_key }, provider.auth);
          } finally { loaded.api_key = ''; }
          pendingCredential = credential;
          this.ensureCurrent(signal);
          storageId = saved.id;
          publicId = saved.id;
        }

        const prior = this.records.get(publicId);
        const record: ProviderRecord = {
          id: publicId,
          providerId: provider.id,
          storage: request.storage,
          storageId,
          purposes: [...request.purposes],
          credential,
          models: structuredClone(provider.models),
          refreshedAt: null,
          catalogSource: 'sdk',
          reason: null,
          expiryTimer: null,
        };
        this.clearRecord(prior);
        for (const [id, existing] of this.records) {
          if (id !== publicId && existing.providerId === provider.id) {
            this.clearRecord(existing);
            this.records.delete(id);
          }
        }
        this.records.set(record.id, record);
        pendingCredential = null;
        this.scheduleExpiry(record);
        if (record.storage !== 'keychain') this.savedMetadata = this.savedMetadata.filter((row) => row.id !== record.id);
        this.reason = null;
        this.publish();
        if (provider.directory_supported) await this.refreshRecord(record, provider, signal);
      });
    } catch (error) {
      this.reason = error instanceof Error && error.message === CANCELLED_REASON
        ? CANCELLED_REASON : 'Provider configuration could not be completed.';
      this.publish();
      throw new Error(this.reason);
    } finally {
      clearCredential(pendingCredential);
      request.api_key = undefined;
      if (raw && typeof raw.api_key === 'string') raw.api_key = '';
      request.purposes.length = 0;
    }
  }

  async refresh(id: string): Promise<void> {
    await this.run(async (signal) => {
      const record = this.records.get(id);
      const provider = record && this.provider(record.providerId);
      if (!record || !provider || !record.credential || !provider.directory_supported) {
        throw new Error('The provider model list cannot be refreshed.');
      }
      this.ensureCredential(record);
      await this.refreshRecord(record, provider, signal);
    }).catch((error: unknown) => {
      this.reason = error instanceof Error && error.message === CANCELLED_REASON
        ? CANCELLED_REASON : 'The provider model list could not be refreshed.';
      this.publish();
      throw new Error(this.reason);
    });
  }

  private async refreshRecord(record: ProviderRecord, provider: ModelProviderDefinition, signal: AbortSignal): Promise<void> {
    if (!record.credential || !provider.directory_supported) return;
    this.ensureCurrent(signal);
    const credential = cloneCredential(record.credential);
    let requestCredential: ModelCredential | null = null;
    try {
      requestCredential = accessCredential(credential);
      const models = validateModels(await this.refreshLoader(structuredClone(provider), requestCredential, signal));
      this.ensureCurrent(signal);
      record.models = models;
      record.refreshedAt = Date.now();
      record.catalogSource = 'live';
      record.reason = null;
    } catch {
      if (signal.aborted) {
        record.reason = 'The model list refresh was cancelled. The current list is retained.';
      } else {
        record.reason = CATALOG_REFRESH_REASON;
      }
    } finally {
      if (requestCredential) clearCredential(requestCredential);
      clearCredential(credential);
    }
    this.publish();
  }

  async select(value: TaskModelSelection): Promise<void> {
    if (!value || typeof value !== 'object' || Array.isArray(value)
      || Object.keys(value).sort().join(',') !== 'connection_id,model'
      || !safeText(value.connection_id, 128) || !safeText(value.model, 256)) {
      throw new Error('Invalid task model selection.');
    }
    await this.run(async (signal) => {
      const record = this.records.get(value.connection_id);
      const provider = record && this.provider(record.providerId);
      if (!record || !provider || !record.credential) throw new Error(UNAVAILABLE_REASON);
      this.ensureCredential(record);
      const model = record.models.find((item) => item.id === value.model && item.selectable);
      if (!model) throw new Error('Choose a selectable model from this provider.');
      const route = routeWithModel(provider, model.id);
      validatePurposesForProvider(provider, record.purposes);
      this.ensureCurrent(signal);
      let credential = cloneCredential(record.credential);
      let activeCredential: ModelCredential | null = null;
      try {
        if (credential.kind === 'openai_oauth'
          && credential.expires_at <= Math.floor(Date.now() / 1000) + OAUTH_REFRESH_SKEW_SECONDS) {
          if (!credential.refresh_token) throw new Error(UNAVAILABLE_REASON);
          const refreshed = await this.refreshOAuthSession(provider, credential, signal);
          clearCredential(credential);
          credential = refreshed;
          this.ensureCurrent(signal);
        }
        activeCredential = accessCredential(credential);
        await this.models.activateProvider(route, activeCredential, [...record.purposes], record.storage, record.id);
      } finally {
        if (activeCredential) clearCredential(activeCredential);
        clearCredential(credential);
      }
      this.ensureCurrent(signal);
      if (this.models.providerConnectionId !== record.id) throw new Error('The selected provider connection did not become active.');
      this.selected = { connection_id: record.id, model: model.id };
      this.reason = null;
      this.publish();
    }).catch((error: unknown) => {
      this.reason = error instanceof Error && error.message === CANCELLED_REASON
        ? CANCELLED_REASON : 'The selected model could not be activated.';
      this.reconcileSelected();
      this.publish();
      throw new Error(this.reason);
    });
  }

  async remove(id: string): Promise<void> {
    if (!safeText(id, 128)) throw new Error('Invalid provider connection.');
    if (this.busy) throw new Error('Provider settings are busy.');
    const record = this.records.get(id);
    if (!record) throw new Error('Provider connection is unavailable.');
    this.busy = true;
    this.publish();
    try {
      if (this.selected?.connection_id === id) {
        await this.models.revoke();
        this.selected = null;
      }
      if (record.storage === 'keychain' && record.storageId) {
        await this.store.forget(record.storageId);
        this.savedMetadata = this.savedMetadata.filter((row) => row.id !== record.storageId);
      }
      this.clearRecord(record);
      this.records.delete(id);
      this.reason = null;
    } catch {
      this.reason = 'The provider connection could not be removed.';
      throw new Error(this.reason);
    } finally { this.busy = false; this.publish(); }
  }

  private ensureCredential(record: ProviderRecord): void {
    if (record.credential?.kind === 'openai_oauth'
      && record.credential.expires_at <= Math.floor(Date.now() / 1000)
      && !record.credential.refresh_token) {
      const selected = this.selected?.connection_id === record.id;
      this.clearRecord(record);
      record.reason = EXPIRED_REASON;
      if (selected) {
        this.selected = null;
        void this.models.revoke().catch(() => undefined);
      }
      this.publish();
      throw new Error(UNAVAILABLE_REASON);
    }
    if (!record.credential) throw new Error(UNAVAILABLE_REASON);
  }

  private scheduleExpiry(record: ProviderRecord): void {
    if (record.expiryTimer) clearTimeout(record.expiryTimer);
    record.expiryTimer = null;
    if (record.credential?.kind !== 'openai_oauth') return;
    const expiresAt = record.credential.expires_at;
    const expire = (): void => {
      if (this.records.get(record.id) !== record || record.credential?.kind !== 'openai_oauth') return;
      const remaining = expiresAt * 1000 - Date.now();
      if (remaining > 0) {
        record.expiryTimer = setTimeout(expire, Math.min(remaining, SAFE_INTEGER_MAX_MS));
        record.expiryTimer.unref?.();
        return;
      }
      if (record.credential.refresh_token) {
        record.reason = REFRESHABLE_EXPIRED_REASON;
        this.publish();
        return;
      }
      const selected = this.selected?.connection_id === record.id;
      this.clearRecord(record);
      record.reason = EXPIRED_REASON;
      if (selected) {
        this.selected = null;
        void this.models.revoke().catch(() => undefined);
      }
      this.publish();
    };
    expire();
  }

  private clearRecord(record: ProviderRecord | undefined): void {
    if (!record) return;
    if (record.expiryTimer) clearTimeout(record.expiryTimer);
    record.expiryTimer = null;
    clearCredential(record.credential);
    record.credential = null;
  }

  async shutdown(): Promise<void> {
    this.closing = true;
    this.operationController?.abort();
    await this.operation?.catch(() => undefined);
    this.models.off('configurationStop', this.onStop);
    this.models.off('status', this.onModelStatus);
    for (const record of this.records.values()) this.clearRecord(record);
    this.records.clear();
    this.selected = null;
    this.savedMetadata = [];
    this.providers = [];
    this.reason = null;
    this.publish();
  }
}
