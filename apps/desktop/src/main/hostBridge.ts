import type { ProviderSettingsSnapshot, ConfigureProviderRequest, TaskModelSelection } from '@computer-use/desktop-api';
import type { ProviderSettings } from './providerSettings.js';
import type { OperatorCommand, RunMode, UiProjection, ObservationImage, NativeAuthorization, ModelAuthorizationStatus, ModelAuthorizationRequest, OpenAiLoginRequest, ModelPurpose, OwnedDocumentStatus, ScopedTaskRequest } from '@computer-use/desktop-api';

import type { StopBridge } from './stopBridge.js';

type CreateFrozenTaskCommand = Extract<OperatorCommand, { command: 'create_frozen_task' }>;

/** What the bridge needs of the link to cu-host. */
export interface HostLink {
  operator(command: OperatorCommand): void | Promise<void>;
  readonly projection: UiProjection | null;
  image?(frameId: string): ObservationImage | null;
}

export interface NativeControl {
  readonly authorization: NativeAuthorization;
  authorizeNativeWindow(snapshotId: string): Promise<void>;
  refreshNativeObservation(): Promise<void>;
  revokeNativeWindow(): Promise<void>;
  /** Optional so existing observe-only controls and test doubles remain valid. */
  readonly ownedDocumentStatus?: OwnedDocumentStatus;
  createOwnedDocument?(): Promise<void>;
  authorizeOwnedDocumentEdit?(): Promise<void>;
  authorizeOwnedDocumentSave?(): Promise<void>;
  revisitOwnedDocument?(): Promise<void>;
}
export interface ModelControl {
  /** Main-only vault source identity; never renderer supplied. */
  readonly providerConnectionId?: string | null;
  readonly authorization: ModelAuthorizationStatus;
  authorize(request: ModelAuthorizationRequest): Promise<void>;
  authorizeOpenAiLogin(request: OpenAiLoginRequest): Promise<void>;
  authorizeSaved(id: string, purposes: ModelPurpose[]): Promise<void>;
  revoke(): Promise<void>;
  forgetSaved(id: string): Promise<void>;
}

/**
 * The operator's commands, as calls. It builds them; it does not decide
 * anything about the task, and the stop button goes through the stop bridge so
 * that it reaches the device first.
 */
export class HostBridge {
  constructor(
    private readonly host: HostLink,
    private readonly stop: StopBridge,
    private readonly native?: NativeControl,
    private readonly currentPresentation: () => 'console' | 'indicator' = () => 'console',
    private readonly models?: ModelControl,
    private readonly selectBrowserPageProfile?: () => Promise<void>,
    private readonly providers?: ProviderSettings,
  ) {}

  /** Starts only Main's fixed local browser fixture; renderer input never names a profile or path. */
  selectFixtureBrowserProfile(): Promise<void> {
    if (!this.selectBrowserPageProfile) throw new Error('The fixture browser profile is unavailable.');
    const model = this.models?.authorization;
    if (!model || model.phase !== 'not_configured' || model.authorized_purposes.length !== 0) {
      throw new Error('Revoke the model connection before selecting the fixture browser profile.');
    }
    return this.selectBrowserPageProfile();
  }

  private requireTaskModel(): void {
    if (!this.providers) throw new Error('Provider settings are unavailable.');
    const settings = this.providers.snapshot;
    if (settings.connections.length === 0) {
      const demo = this.models?.authorization;
      if (settings.busy || settings.reason || demo?.phase !== 'not_configured' || demo.route !== null
        || demo.credential_kind !== null || !demo.planning_available) {
        throw new Error('Choose a connected provider and task model before starting a task.');
      }
      return;
    }
    const selection = settings.selected;
    const connection = selection && settings.connections.find((item) => item.id === selection.connection_id && item.available);
    const provider = connection && settings.providers.find((item) => item.id === connection.provider_id);
    const actual = this.models?.authorization;
    const expected = provider && selection ? { ...provider.route, model: selection.model } : null;
    if (settings.busy || !connection || this.models?.providerConnectionId !== connection.id || !expected || actual?.phase !== 'authorized' || !actual.route
      || !actual.planning_available || !actual.authorized_purposes.includes('s2_planning')
      || !connection.purposes.includes('s2_planning') || !connection.models.some((model) => model.id === selection?.model && model.selectable)
      || actual.storage !== connection.storage
      || actual.credential_kind !== (provider?.auth === 'openai_oauth' ? 'openai_oauth' : 'api_key')
      || actual.route.model !== expected.model || actual.route.protocol !== expected.protocol
      || actual.route.base_url !== expected.base_url || actual.route.preset_id !== expected.preset_id
      || actual.route.max_output_tokens !== expected.max_output_tokens
      || JSON.stringify(actual.route.provider_id) !== JSON.stringify(expected.provider_id)) {
      throw new Error('Choose a connected provider and task model before starting a task.');
    }
  }
  createTask(text: string): void {
    this.requireTaskModel();
    this.host.operator({ command: 'create_task', text });
  }

  createScopedTask(request: ScopedTaskRequest): void {
    this.requireTaskModel();
    this.host.operator({ command: 'create_scoped_task', objective: request.objective, allowed_scope: request.allowed_scope });
  }

  createFrozenTask(command: CreateFrozenTaskCommand): void | Promise<void> {
    this.requireTaskModel();
    return this.host.operator(command);
  }

  confirm(confirmationId: string, approved: boolean): void {
    this.host.operator({ command: 'confirm_goal', confirmation_id: confirmationId, approved });
  }

  pause(rootTaskId: string): void {
    this.host.operator({ command: 'pause_task', root_task_id: rootTaskId });
  }

  resume(rootTaskId: string): void {
    this.host.operator({ command: 'resume_task', root_task_id: rootTaskId });
  }

  setMode(mode: RunMode): void | Promise<void> {
    return this.host.operator({ command: 'set_mode', mode });
  }

  pressStop(): void {
    this.stop.press('operator_button');
  }

  image(frameId: string): ObservationImage | null { return this.host.image?.(frameId) ?? null; }

  snapshot(): UiProjection | null {
    return this.host.projection;
  }

  nativeAuthorization(): NativeAuthorization {
    return this.native?.authorization ?? { phase: 'not_native', candidate: null, authorized: null, can_authorize: false,
      candidate_domain: null, authorized_domain: null, can_refresh_observation: false, reason: null };
  }

  presentation(): 'console' | 'indicator' { return this.currentPresentation(); }
  authorizeNativeWindow(snapshotId: string): Promise<void> {
    if (!this.native) throw new Error('Native observation is not configured.');
    return this.native.authorizeNativeWindow(snapshotId);
  }
  refreshNativeObservation(): Promise<void> {
    if (!this.native) throw new Error('Native observation is not configured.');
    if (!this.native.authorization.can_refresh_observation) throw new Error('Native observation cannot be refreshed now.');
    return this.native.refreshNativeObservation();
  }
  revokeNativeWindow(): Promise<void> {
    if (!this.native) throw new Error('Native observation is not configured.');
    return this.native.revokeNativeWindow();
  }
  providerSettings(): ProviderSettingsSnapshot {
    return this.providers?.snapshot ?? { providers: [], connections: [], selected: null, keychain_available: false, busy: false, can_configure: false, reason: 'Provider settings are unavailable.' };
  }
  configureProvider(request: ConfigureProviderRequest): Promise<void> {
    if (!this.providers) throw new Error('Provider settings are unavailable.');
    return this.providers.configure(request);
  }
  refreshProviderModels(id: string): Promise<void> {
    if (!this.providers) throw new Error('Provider settings are unavailable.');
    return this.providers.refresh(id);
  }
  removeProvider(id: string): Promise<void> {
    if (!this.providers) throw new Error('Provider settings are unavailable.');
    return this.providers.remove(id);
  }
  selectTaskModel(selection: TaskModelSelection): Promise<void> {
    if (!this.providers) throw new Error('Provider settings are unavailable.');
    return this.providers.select(selection);
  }
  modelAuthorization(): ModelAuthorizationStatus {
    return this.models?.authorization ?? { phase: 'not_configured', route: null, credential_kind: null, storage: null, authorized_purposes: [], can_authorize: false, can_save: false, planning_available: false, saved: [], reason: null };
  }
  authorizeModel(request: ModelAuthorizationRequest): Promise<void> {
    if (!this.models) throw new Error('Model configuration is unavailable.');
    return this.models.authorize(request);
  }
  authorizeOpenAiLogin(request: OpenAiLoginRequest): Promise<void> {
    if (!this.providers) throw new Error('Provider settings are unavailable.');
    const provider = this.providers.snapshot.providers.find((item) => item.auth === 'openai_oauth');
    if (!provider || !provider.models.some((item) => item.id === request.model && item.selectable)) {
      throw new Error('The requested OpenAI account model is unavailable.');
    }
    return this.providers.configure({
      provider_id: provider.id,
      storage: 'keychain',
      purposes: [...request.purposes],
      reauthenticate: true,
    });
  }
  authorizeSavedModel(id: string, purposes: ModelPurpose[]): Promise<void> {
    if (!this.models) throw new Error('Model configuration is unavailable.'); return this.models.authorizeSaved(id, purposes);
  }
  revokeModel(): Promise<void> {
    if (!this.models) throw new Error('Model configuration is unavailable.'); return this.models.revoke();
  }
  forgetSavedModel(id: string): Promise<void> {
    if (!this.models) throw new Error('Model configuration is unavailable.'); return this.models.forgetSaved(id);
  }

  ownedDocumentStatus(): OwnedDocumentStatus {
    return this.native?.ownedDocumentStatus ?? {
      phase: 'not_created', can_create: false, can_authorize_edit: false,
      can_authorize_save: false, can_revisit: false,
      reason: 'Temporary TextEdit documents are unavailable in this session.',
    };
  }

  createOwnedDocument(): Promise<void> {
    if (!this.native?.createOwnedDocument) throw new Error('Temporary TextEdit documents are unavailable.');
    if (!this.ownedDocumentStatus().can_create) throw new Error('A temporary TextEdit document cannot be created now.');
    return this.native.createOwnedDocument();
  }

  authorizeOwnedDocumentEdit(): Promise<void> {
    if (!this.native?.authorizeOwnedDocumentEdit) throw new Error('Temporary TextEdit editing is unavailable.');
    if (!this.ownedDocumentStatus().can_authorize_edit) throw new Error('Temporary TextEdit editing cannot be authorized now.');
    return this.native.authorizeOwnedDocumentEdit();
  }

  authorizeOwnedDocumentSave(): Promise<void> {
    if (!this.native?.authorizeOwnedDocumentSave) throw new Error('Temporary TextEdit Save is unavailable.');
    if (!this.ownedDocumentStatus().can_authorize_save) throw new Error('Temporary TextEdit Save cannot be authorized now.');
    return this.native.authorizeOwnedDocumentSave();
  }

  revisitOwnedDocument(): Promise<void> {
    if (!this.native?.revisitOwnedDocument) throw new Error('Temporary TextEdit verification is unavailable.');
    if (!this.ownedDocumentStatus().can_revisit) throw new Error('The temporary TextEdit document cannot be reopened now.');
    return this.native.revisitOwnedDocument();
  }
}
