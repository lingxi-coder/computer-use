import type { AllowedControlScope, RunMode, UiProjection } from './generated/envelope.js';

/**
 * What a stop has reached. The button, the tray and the hotkey all send the
 * stop straight to the device; a click alone never becomes "stopped".
 */
export type BridgeStopStatus = 'idle' | 'stopping' | 'confirmed' | 'unconfirmed';

/** Binary PNG preview, copied through structured clone, never a file path. */
export interface ObservationImage {
  frame_id: string;
  stream_id: string;
  width_px: number;
  height_px: number;
  expires_unix_ms: number;
  bytes: Uint8Array;
}
export interface ImageAvailability {
  frame_id: string | null;
  expires_unix_ms: number | null;
}

/** Metadata selected by main from a fresh native observation, never a renderer-supplied PID. */
export interface NativeWindowTarget {
  snapshot_id: string;
  app: string;
  pid: number;
  window_id: string;
  window_title: string | null;
  frame_points?: readonly [number, number, number, number] | null;
  contains_private_fields: boolean;
  expires_unix_ms: number;
}
export interface NativeAuthorization {
  phase: 'not_native' | 'observing' | 'authorizing' | 'switching' | 'authorized' | 'unavailable';
  candidate: NativeWindowTarget | null;
  candidate_domain: 'foreground_native' | 'background_ax' | null;
  authorized: NativeWindowTarget | null;
  authorized_domain: 'foreground_native' | 'background_ax' | null;
  can_authorize: boolean;
  can_refresh_observation: boolean;
  reason: string | null;
}

export type ModelProvider = 'anthropic_first_party' | 'open_ai' | 'gemini'
  | { open_ai_compatible: { name: string } } | { custom: { name: string } };
export interface ModelRoute {
  /** Trusted SDK profile identity for a built-in provider route. */
  preset_id?: string;
  provider_id: ModelProvider;
  protocol: 'anthropic_messages' | 'open_ai_chat' | 'open_ai_responses' | 'gemini_generate_content';
  base_url: string;
  model: string;
  max_output_tokens: number;
}
/** Explicit outbound purposes; persisted model keys do not retain this consent. */
export type ModelPurpose = 's2_planning' | 's1_teacher' | 'computer_proposals';
/** Sensitive invoke-only data: never part of a projection or desktop Message. */
export interface ModelAuthorizationRequest { route: ModelRoute; api_key: string; storage: 'memory' | 'keychain'; purposes: ModelPurpose[] }
/** Main-owned OpenAI account login. The renderer supplies no credential, URL, helper path, or storage choice. */
export interface OpenAiLoginRequest { model: string; purposes: ModelPurpose[] }
export interface SavedModelConnection { id: string; route: ModelRoute }
export interface ModelAuthorizationStatus {
  phase: 'not_configured' | 'authorizing' | 'switching' | 'authorized' | 'unavailable';
  route: ModelRoute | null;
  credential_kind: 'api_key' | 'openai_oauth' | null;
  storage: 'memory' | 'keychain' | null;
  authorized_purposes: ModelPurpose[];
  can_authorize: boolean;
  can_save: boolean;
  planning_available: boolean;
  saved: SavedModelConnection[];
  reason: string | null;
}

/** Safe SDK-owned provider catalog. Model choice belongs to task submission. */
export interface ProviderModel { id: string; label: string; selectable: boolean }
export interface ModelProviderDefinition {
  id: string; label: string; description: string;
  route: Omit<ModelRoute, 'model'>;
  auth: 'api_key' | 'openai_oauth';
  models: ProviderModel[];
  directory_supported: boolean;
}
export interface ProviderConnection {
  id: string; provider_id: string; storage: 'memory' | 'keychain';
  purposes: ModelPurpose[]; available: boolean;
  models: ProviderModel[]; refreshed_at: number | null;
  catalog_source: 'sdk' | 'live'; reason: string | null;
}
export interface ProviderSettingsSnapshot {
  providers: ModelProviderDefinition[];
  connections: ProviderConnection[];
  selected: { connection_id: string; model: string } | null;
  /** Whether the main process has a native Keychain helper available. */
  keychain_available: boolean;
  busy: boolean; can_configure: boolean; reason: string | null;
}
export interface ConfigureProviderRequest {
  provider_id: string; api_key?: string;
  storage: 'memory' | 'keychain'; purposes: ModelPurpose[];
  /** Open a fresh OpenAI browser login instead of reusing a saved Keychain session. */
  reauthenticate?: boolean;
}
export interface TaskModelSelection { connection_id: string; model: string }

/** Main-owned state for Computer Use's private temporary TextEdit document. */
export interface OwnedDocumentStatus {
  phase: 'not_created' | 'creating' | 'observing' | 'authorizing_edit' | 'edit_authorized'
    | 'authorizing_save' | 'save_authorized' | 'revisiting' | 'completed' | 'unavailable';
  can_create: boolean;
  can_authorize_edit: boolean;
  can_authorize_save: boolean;
  can_revisit: boolean;
  reason: string | null;
}

/** Opaque identifiers for one immutable record in Main's frozen task catalog. */
export interface FrozenTaskSelection {
  readonly catalog_id: string;
  readonly selector: string;
}

/** Safe display metadata only; scope_compatible means schema-expressible, not executable for an app or business domain. */
export interface FrozenTaskCatalogEntry extends FrozenTaskSelection {
  readonly task_id: string;
  readonly seed: string;
  readonly suite: string;
  readonly scope_family: 'control' | 'form' | 'list' | 'modal' | 'textedit';
  readonly scope_compatible: boolean;
  readonly unsupported_reason?: string;
}

export type FrozenTaskCatalogState = 'empty' | 'available' | 'unavailable';

export interface FrozenTaskCatalogSnapshot {
  readonly status: {
    readonly state: FrozenTaskCatalogState;
    readonly reason?: string;
  };
  readonly tasks: readonly FrozenTaskCatalogEntry[];
}

/** A user-authored objective with one explicit control boundary. */
export interface ScopedTaskRequest {
  readonly objective: string;
  readonly allowed_scope: AllowedControlScope;
}

/**
 * The whole surface the renderer sees (design 4.6). It has no channel name, no
 * path, no shell and no native input: only these calls, each of which main
 * checks against the published schema before doing anything.
 */
export interface DesktopApi {
  /** Starts the one Main-configured local fixture browser profile. */
  selectBrowserPageProfile(): Promise<void>;
  createTask(text: string): Promise<void>;
  /** Creates a user-authored one-control task when no frozen catalog is configured. */
  createScopedTask(request: ScopedTaskRequest): Promise<void>;
  /** Returns safe metadata for the Main-configured frozen task catalog. */
  frozenTaskCatalog(): Promise<FrozenTaskCatalogSnapshot>;
  /** Main resolves this opaque selection to its immutable goal and scope. */
  createFrozenTask(selection: FrozenTaskSelection): Promise<void>;
  confirm(confirmationId: string, approved: boolean): Promise<void>;
  pause(rootTaskId: string): Promise<void>;
  resume(rootTaskId: string): Promise<void>;
  /** The stop button. Also reachable from the tray and a global hotkey, without the renderer. */
  stop(): Promise<void>;
  setMode(mode: RunMode): Promise<void>;
  /** The current view, for a renderer that has just loaded. */
  snapshot(): Promise<UiProjection | null>;
  onProjection(listener: (projection: UiProjection) => void): () => void;
  onStopStatus(listener: (status: BridgeStopStatus) => void): () => void;
  image(frameId: string): Promise<ObservationImage | null>;
  onImageAvailability(listener: (status: ImageAvailability) => void): () => void;
  nativeAuthorization(): Promise<NativeAuthorization>;
  /** Opens main's native consent dialog for the current observed window. */
  authorizeNativeWindow(snapshotId: string): Promise<void>;
  /** Explicitly replaces a safe native observation session without granting input. */
  refreshNativeObservation(): Promise<void>;
  revokeNativeWindow(): Promise<void>;
  onNativeAuthorization(listener: (status: NativeAuthorization) => void): () => void;
  presentation(): Promise<'console' | 'indicator'>;
  onPresentation(listener: (value: 'console' | 'indicator') => void): () => void;
  modelAuthorization(): Promise<ModelAuthorizationStatus>;
  providerSettings(): Promise<ProviderSettingsSnapshot>;
  configureProvider(request: ConfigureProviderRequest): Promise<void>;
  refreshProviderModels(id: string): Promise<void>;
  removeProvider(id: string): Promise<void>;
  selectTaskModel(selection: TaskModelSelection): Promise<void>;
  onProviderSettings(listener: (snapshot: ProviderSettingsSnapshot) => void): () => void;
  authorizeModel(request: ModelAuthorizationRequest): Promise<void>;
  authorizeOpenAiLogin(request: OpenAiLoginRequest): Promise<void>;
  authorizeSavedModel(id: string, purposes: ModelPurpose[]): Promise<void>;
  revokeModel(): Promise<void>;
  forgetSavedModel(id: string): Promise<void>;
  onModelAuthorization(listener: (status: ModelAuthorizationStatus) => void): () => void;
  /** Read-only product status. It includes no temporary file identity or contents. */
  ownedDocumentStatus(): Promise<OwnedDocumentStatus>;
  createOwnedDocument(): Promise<void>;
  authorizeOwnedDocumentEdit(): Promise<void>;
  authorizeOwnedDocumentSave(): Promise<void>;
  revisitOwnedDocument(): Promise<void>;
  onOwnedDocumentStatus(listener: (status: OwnedDocumentStatus) => void): () => void;
}
