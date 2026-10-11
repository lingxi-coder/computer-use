import { validateOperatorCommand, type FrozenTaskCatalogEntry, type FrozenTaskCatalogSnapshot,
  type FrozenTaskSelection, type ModelPurpose, type OperatorCommand, type OpenAiLoginRequest, type ScopedTaskRequest, type UiProjection } from '@computer-use/desktop-api';

import { Channels } from '../shared/channels.js';
import type { HostBridge } from './hostBridge.js';
import type { FrozenTaskCatalog, FrozenTaskResolution } from './frozenTaskCatalog.js';
import { modelPurposes, modelRequest } from './modelAccess.js';

/** The pages that may call main: the packaged renderer, or a loopback dev server. */
export interface TrustedOrigins {
  files: string[];
  devOrigin?: string;
}

const MAX_TASK_TEXT = 8_192;
const MAX_SCOPE_TEXT_BYTES = 256;
const MAX_SCOPE_CONTROL_BYTES = 512;
const SHA256 = /^[a-f0-9]{64}$/;
const TASK_TEXT_CONTROLS = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/;
const SCOPE_TEXT_CONTROLS = /[\u0000-\u001f\u007f-\u009f]/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && Object.getPrototypeOf(value) === Object.prototype;
}

function requireExactKeys(value: Record<string, unknown>, required: readonly string[], optional: readonly string[], label: string): void {
  const keys = Object.keys(value);
  if (required.some((key) => !keys.includes(key)) || keys.some((key) => !required.includes(key) && !optional.includes(key))) {
    throw new Error(`refused: invalid ${label} shape`);
  }
}

function frozenTaskSelection(value: unknown): FrozenTaskSelection {
  if (!isRecord(value)) throw new Error('refused: invalid frozen task selection');
  requireExactKeys(value, ['catalog_id', 'selector'], [], 'frozen task selection');
  const catalogId = value['catalog_id'];
  const selector = value['selector'];
  if (typeof catalogId !== 'string' || !SHA256.test(catalogId)
      || typeof selector !== 'string' || !SHA256.test(selector)) {
    throw new Error('refused: frozen task selection must use lowercase SHA-256 identifiers');
  }
  return { catalog_id: catalogId, selector };
}

function requireScopeText(value: unknown, label: string, maxBytes = MAX_SCOPE_TEXT_BYTES): asserts value is string {
  if (typeof value !== 'string' || value.trim() === '' || Buffer.byteLength(value, 'utf8') > maxBytes
      || SCOPE_TEXT_CONTROLS.test(value)) {
    throw new Error(`refused: invalid ${label}`);
  }
}

function scopedTaskRequest(value: unknown): ScopedTaskRequest {
  if (!isRecord(value)) throw new Error('refused: invalid scoped task request');
  requireExactKeys(value, ['objective', 'allowed_scope'], [], 'scoped task request');
  const objective = value['objective'];
  if (typeof objective !== 'string' || objective.trim() === '' || Buffer.byteLength(objective, 'utf8') > MAX_TASK_TEXT
      || TASK_TEXT_CONTROLS.test(objective)) {
    throw new Error('refused: a task needs some text of reasonable length');
  }

  const rawScope = value['allowed_scope'];
  if (!isRecord(rawScope)) throw new Error('refused: invalid allowed control scope');
  requireExactKeys(rawScope, ['application', 'targets', 'no_unrelated_controls'], [], 'allowed control scope');
  const application = rawScope['application'];
  requireScopeText(application, 'allowed application');
  if (rawScope['no_unrelated_controls'] !== true) {
    throw new Error('refused: scoped tasks must prohibit unrelated controls');
  }
  const targets = rawScope['targets'];
  if (!Array.isArray(targets) || targets.length !== 1) {
    throw new Error('refused: a scoped task must name exactly one control');
  }
  const rawTarget: unknown = targets[0];
  if (!isRecord(rawTarget)) throw new Error('refused: invalid allowed control target');
  requireExactKeys(rawTarget, ['control'], ['group', 'form', 'disambiguator'], 'allowed control target');
  const control = rawTarget['control'];
  requireScopeText(control, 'allowed control', MAX_SCOPE_CONTROL_BYTES);

  const group = rawTarget['group'];
  if (group !== undefined && group !== null) requireScopeText(group, 'allowed control group');
  const form = rawTarget['form'];
  if (form !== undefined && form !== null) requireScopeText(form, 'allowed control form');
  if (group !== undefined && group !== null && form !== undefined && form !== null) {
    throw new Error('refused: an allowed control cannot name both a group and a form');
  }
  const disambiguator = rawTarget['disambiguator'];
  if (disambiguator !== undefined && disambiguator !== null) {
    requireScopeText(disambiguator, 'allowed control disambiguator');
    if (group === undefined || group === null || disambiguator !== group) {
      throw new Error('refused: a disambiguator must exactly match the allowed group');
    }
  }

  const target: ScopedTaskRequest['allowed_scope']['targets'][number] = {
    control,
    ...(group !== undefined ? { group: group as string | null } : {}),
    ...(form !== undefined ? { form: form as string | null } : {}),
    ...(disambiguator !== undefined ? { disambiguator: disambiguator as string | null } : {}),
  };
  return {
    objective,
    allowed_scope: {
      application,
      targets: [target],
      no_unrelated_controls: true,
    },
  };
}

function openAiLoginRequest(value: unknown): OpenAiLoginRequest {
  const invalid = () => new Error('refused: invalid OpenAI account login request');
  if (!isRecord(value) || Object.keys(value).sort().join(',') !== 'model,purposes') throw invalid();
  const model = value['model'];
  if (typeof model !== 'string' || model.trim() === '' || Buffer.byteLength(model, 'utf8') > 256
      || /\p{Cc}/u.test(model)) throw invalid();
  let purposes: ModelPurpose[];
  try { purposes = modelPurposes(value['purposes']); } catch { throw invalid(); }
  if (purposes.includes('computer_proposals')) throw invalid();
  return { model: model.trim(), purposes };
}

function deepFrozen(value: unknown, visited = new Set<object>()): boolean {
  if (value === null || typeof value !== 'object') return true;
  if (visited.has(value)) return true;
  visited.add(value);
  return Object.isFrozen(value) && Object.values(value).every((child) => deepFrozen(child, visited));
}

const EMPTY_FROZEN_TASK_CATALOG: FrozenTaskCatalogSnapshot = Object.freeze({
  status: Object.freeze({ state: 'empty' }),
  tasks: Object.freeze([]),
});

function safeFrozenTaskCatalogSnapshot(catalog?: FrozenTaskCatalogPort): FrozenTaskCatalogSnapshot {
  if (!catalog) return EMPTY_FROZEN_TASK_CATALOG;
  const snapshot = catalog.snapshot();
  const state = snapshot?.status?.state;
  if (state !== 'empty' && state !== 'available' && state !== 'unavailable') {
    return { status: { state: 'unavailable', reason: 'The frozen task catalog is invalid.' }, tasks: [] };
  }
  if (!Array.isArray(snapshot.tasks)) {
    return { status: { state: 'unavailable', reason: 'The frozen task catalog is invalid.' }, tasks: [] };
  }
  const tasks: FrozenTaskCatalogEntry[] = [];
  for (const entry of snapshot.tasks) {
    if (!entry || !SHA256.test(entry.catalog_id) || !SHA256.test(entry.selector)
        || typeof entry.task_id !== 'string' || typeof entry.seed !== 'string' || typeof entry.suite !== 'string'
        || !['control', 'form', 'list', 'modal', 'textedit'].includes(entry.scope_family)
        || typeof entry.scope_compatible !== 'boolean'
        || (!entry.scope_compatible && typeof entry.unsupported_reason !== 'string')
        || (entry.scope_compatible && entry.unsupported_reason !== undefined)) {
      return { status: { state: 'unavailable', reason: 'The frozen task catalog is invalid.' }, tasks: [] };
    }
    tasks.push({
      catalog_id: entry.catalog_id,
      selector: entry.selector,
      task_id: entry.task_id,
      seed: entry.seed,
      suite: entry.suite,
      scope_family: entry.scope_family,
      scope_compatible: entry.scope_compatible,
      ...(entry.unsupported_reason !== undefined ? { unsupported_reason: entry.unsupported_reason } : {}),
    });
  }
  if ((state === 'empty' || state === 'unavailable') && tasks.length !== 0
      || state === 'available' && tasks.length !== 120) {
    return { status: { state: 'unavailable', reason: 'The frozen task catalog is incomplete.' }, tasks: [] };
  }
  const reason = snapshot.status.reason;
  return {
    status: {
      state,
      ...(typeof reason === 'string' && reason.length > 0 && reason.length <= 512 ? { reason } : {}),
    },
    tasks,
  };
}

function matchesSelection(resolution: Extract<FrozenTaskResolution, { status: 'available' }>, selection: FrozenTaskSelection,
  selectedEntry: FrozenTaskCatalogEntry): boolean {
  const row = resolution.profileMetadata;
  const profile = resolution.profile;
  return row.scope_compatible && row.unsupported_reason === undefined
    && row.catalog_id === selectedEntry.catalog_id && row.selector === selectedEntry.selector
    && row.task_id === selectedEntry.task_id && row.seed === selectedEntry.seed && row.suite === selectedEntry.suite
    && row.scope_family === selectedEntry.scope_family
    && row.catalog_id === selection.catalog_id && row.selector === selection.selector
    && profile.catalog_id === selection.catalog_id && profile.selector === selection.selector
    && SHA256.test(profile.source_freeze_sha256) && SHA256.test(profile.source_request_sha256) && SHA256.test(profile.goals_sha256)
    && profile.task_id === row.task_id && String(profile.seed) === row.seed && profile.suite === row.suite;
}

export type FrozenTaskCatalogPort = Pick<FrozenTaskCatalog, 'snapshot' | 'resolve'>;

export function isTrustedSender(url: string | null | undefined, trusted: TrustedOrigins): boolean {
  if (!url) return false;
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  if (parsed.protocol === 'file:') return trusted.files.includes(decodeURIComponent(parsed.pathname));
  if (trusted.devOrigin) return parsed.origin === new URL(trusted.devOrigin).origin;
  return false;
}

export interface IpcEventLike {
  senderFrame?: { url: string } | null;
}

export interface IpcMainLike {
  handle(channel: string, listener: (event: IpcEventLike, ...args: unknown[]) => unknown): void;
  removeHandler(channel: string): void;
}

/**
 * Register every channel the renderer may use. Each handler first checks who is
 * calling, then checks the arguments against the published schema, and only then
 * does anything; a failed check throws and nothing reaches a native process.
 */
export function registerIpc(ipc: IpcMainLike, bridge: HostBridge, trusted: TrustedOrigins, frozenTasks?: FrozenTaskCatalogPort): () => void {
  const guard = (event: IpcEventLike): void => {
    if (!isTrustedSender(event.senderFrame?.url, trusted)) throw new Error('refused: untrusted sender');
  };
  const command = (value: unknown): void => {
    const result = validateOperatorCommand(value);
    if (!result.ok) throw new Error(`refused: ${result.errors.join('; ')}`);
  };
  const noArgs = (args: unknown[]): void => {
    if (args.length !== 0) throw new Error('refused: this operation takes no arguments');
  };
  const handlers: Record<string, (event: IpcEventLike, ...args: unknown[]) => unknown> = {
    [Channels.selectBrowserPageProfile]: (event, ...args) => {
      guard(event); noArgs(args);
      return bridge.selectFixtureBrowserProfile();
    },
    [Channels.createTask]: (event, text) => {
      guard(event);
      if (safeFrozenTaskCatalogSnapshot(frozenTasks).status.state !== 'empty') {
        throw new Error('refused: free-form task creation is disabled while a frozen catalog is configured');
      }
      if (typeof text !== 'string' || text.trim() === '' || Buffer.byteLength(text, 'utf8') > MAX_TASK_TEXT || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/.test(text)) {
        throw new Error('refused: a task needs some text of reasonable length');
      }
      command({ command: 'create_task', text });
      bridge.createTask(text);
    },
    [Channels.createScopedTask]: (event, ...args) => {
      guard(event);
      if (args.length !== 1) throw new Error('refused: scoped task creation takes exactly one argument');
      if (safeFrozenTaskCatalogSnapshot(frozenTasks).status.state !== 'empty') {
        throw new Error('refused: user-authored scoped tasks are disabled while a frozen catalog is configured');
      }
      const validated = scopedTaskRequest(args[0]);
      const value = { command: 'create_scoped_task', ...validated };
      command(value);
      bridge.createScopedTask(validated);
    },
    [Channels.frozenTaskCatalog]: (event, ...args) => {
      guard(event); noArgs(args);
      return safeFrozenTaskCatalogSnapshot(frozenTasks);
    },
    [Channels.createFrozenTask]: (event, ...args) => {
      guard(event);
      if (args.length !== 1) throw new Error('refused: frozen task creation takes exactly one argument');
      const selection = frozenTaskSelection(args[0]);
      if (!frozenTasks) throw new Error('refused: the frozen task catalog is not configured');
      const publicCatalog = safeFrozenTaskCatalogSnapshot(frozenTasks);
      if (publicCatalog.status.state !== 'available') {
        throw new Error('refused: the frozen task catalog is unavailable');
      }
      const selectedEntry = publicCatalog.tasks.find((entry) => entry.catalog_id === selection.catalog_id && entry.selector === selection.selector);
      if (!selectedEntry) throw new Error('refused: this frozen task is not in the configured catalog');
      if (!selectedEntry.scope_compatible) {
        throw new Error(`refused: ${selectedEntry.unsupported_reason ?? 'this task scope is unsupported'}`);
      }
      const resolution = frozenTasks.resolve(selection);
      if (!resolution) throw new Error('refused: this frozen task is not in the configured catalog');
      if (resolution.status === 'unsupported') {
        throw new Error(`refused: ${resolution.reason}`);
      }
      if (!matchesSelection(resolution, selection, selectedEntry)) {
        throw new Error('refused: frozen task provenance does not match the selected catalog entry');
      }
      if (!deepFrozen(resolution.goal) || !deepFrozen(resolution.profile)) {
        throw new Error('refused: the frozen task catalog returned mutable task data');
      }
      scopedTaskRequest(resolution.goal);
      const value = {
        command: 'create_frozen_task',
        objective: resolution.goal.objective,
        allowed_scope: resolution.goal.allowed_scope,
        profile: resolution.profile,
      } satisfies Extract<OperatorCommand, { command: 'create_frozen_task' }>;
      command(value);
      return bridge.createFrozenTask(value);
    },
    [Channels.confirm]: (event, id, approved) => {
      guard(event);
      command({ command: 'confirm_goal', confirmation_id: id, approved });
      bridge.confirm(id as string, approved as boolean);
    },
    [Channels.pause]: (event, root) => {
      guard(event);
      command({ command: 'pause_task', root_task_id: root });
      bridge.pause(root as string);
    },
    [Channels.resume]: (event, root) => {
      guard(event);
      command({ command: 'resume_task', root_task_id: root });
      bridge.resume(root as string);
    },
    [Channels.stop]: (event, ...args) => {
      guard(event); noArgs(args);
      bridge.pressStop();
    },
    [Channels.setMode]: (event, mode) => {
      guard(event);
      command({ command: 'set_mode', mode });
      return bridge.setMode(mode as 'observe' | 'shadow' | 'execute');
    },
    [Channels.image]: (event, frameId) => {
      guard(event);
      if (typeof frameId !== 'string' || !frameId.trim() || Buffer.byteLength(frameId) > 256 || /[\u0000-\u001f\u007f-\u009f]/.test(frameId)) {
        throw new Error('refused: invalid image frame identity');
      }
      return bridge.image(frameId);
    },
    [Channels.snapshot]: (event, ...args): UiProjection | null => {
      guard(event); noArgs(args);
      return bridge.snapshot();
    },
    [Channels.nativeAuthorization]: (event, ...args) => { guard(event); noArgs(args); return bridge.nativeAuthorization(); },
    [Channels.presentation]: (event, ...args) => { guard(event); noArgs(args); return bridge.presentation(); },
    [Channels.authorizeNativeWindow]: (event, snapshotId) => {
      guard(event);
      if (typeof snapshotId !== 'string' || !snapshotId.trim() || Buffer.byteLength(snapshotId) > 256 || /[\u0000-\u001f\u007f-\u009f]/.test(snapshotId)) {
        throw new Error('refused: invalid native observation identity');
      }
      return bridge.authorizeNativeWindow(snapshotId);
    },
    [Channels.refreshNativeObservation]: (event, ...args) => {
      guard(event); noArgs(args); return bridge.refreshNativeObservation();
    },
    [Channels.revokeNativeWindow]: (event, ...args) => { guard(event); noArgs(args); return bridge.revokeNativeWindow(); },
    [Channels.providerSettings]: (event, ...args) => { guard(event); noArgs(args); return bridge.providerSettings(); },
    [Channels.configureProvider]: (event, ...args) => {
      guard(event);
      if (args.length !== 1 || !isRecord(args[0])) throw new Error('refused: invalid provider settings');
      const value = args[0];
      requireExactKeys(value, ['provider_id', 'storage', 'purposes'], ['api_key', 'reauthenticate'], 'provider settings');
      requireScopeText(value['provider_id'], 'provider identity', 128);
      if (value['storage'] !== 'memory' && value['storage'] !== 'keychain') throw new Error('refused: invalid provider storage');
      if (value['api_key'] !== undefined && (typeof value['api_key'] !== 'string' || Buffer.byteLength(value['api_key']) > 16384)) throw new Error('refused: invalid provider credential');
      if (value['reauthenticate'] !== undefined && typeof value['reauthenticate'] !== 'boolean') throw new Error('refused: invalid provider reauthentication flag');
      return bridge.configureProvider({ provider_id: value['provider_id'], storage: value['storage'], purposes: modelPurposes(value['purposes']),
        ...(typeof value['api_key'] === 'string' ? { api_key: value['api_key'] } : {}),
        ...(typeof value['reauthenticate'] === 'boolean' ? { reauthenticate: value['reauthenticate'] } : {}) });
    },
    [Channels.refreshProviderModels]: (event, ...args) => {
      guard(event); if (args.length !== 1) throw new Error('refused: invalid provider identity');
      requireScopeText(args[0], 'provider connection identity', 128); return bridge.refreshProviderModels(args[0]);
    },
    [Channels.removeProvider]: (event, ...args) => {
      guard(event); if (args.length !== 1) throw new Error('refused: invalid provider identity');
      requireScopeText(args[0], 'provider connection identity', 128); return bridge.removeProvider(args[0]);
    },
    [Channels.selectTaskModel]: (event, ...args) => {
      guard(event); if (args.length !== 1 || !isRecord(args[0])) throw new Error('refused: invalid task model');
      const value = args[0]; requireExactKeys(value, ['connection_id', 'model'], [], 'task model');
      requireScopeText(value['connection_id'], 'provider connection identity', 128);
      requireScopeText(value['model'], 'model identity', 256);
      return bridge.selectTaskModel({ connection_id: value['connection_id'], model: value['model'] });
    },
    [Channels.modelAuthorization]: (event, ...args) => { guard(event); noArgs(args); return bridge.modelAuthorization(); },
    [Channels.authorizeModel]: (event, value) => { guard(event); return bridge.authorizeModel(modelRequest(value)); },
    [Channels.authorizeOpenAiLogin]: (event, ...args) => {
      guard(event);
      if (args.length !== 1) throw new Error('refused: OpenAI account login takes exactly one request');
      return bridge.authorizeOpenAiLogin(openAiLoginRequest(args[0]));
    },
    [Channels.authorizeSavedModel]: (event, id, purposes) => { guard(event); if (typeof id !== 'string' || !/^[a-f0-9]{64}$/.test(id)) throw new Error('Invalid saved connection.'); return bridge.authorizeSavedModel(id, modelPurposes(purposes)); },
    [Channels.forgetSavedModel]: (event, id) => { guard(event); if (typeof id !== 'string' || !/^[a-f0-9]{64}$/.test(id)) throw new Error('Invalid saved connection.'); return bridge.forgetSavedModel(id); },
    [Channels.revokeModel]: (event, ...args) => { guard(event); noArgs(args); return bridge.revokeModel(); },
    [Channels.ownedDocumentStatus]: (event, ...args) => {
      guard(event); noArgs(args); return bridge.ownedDocumentStatus();
    },
    [Channels.createOwnedDocument]: (event, ...args) => {
      guard(event); noArgs(args); return bridge.createOwnedDocument();
    },
    [Channels.authorizeOwnedDocumentEdit]: (event, ...args) => {
      guard(event); noArgs(args); return bridge.authorizeOwnedDocumentEdit();
    },
    [Channels.authorizeOwnedDocumentSave]: (event, ...args) => {
      guard(event); noArgs(args); return bridge.authorizeOwnedDocumentSave();
    },
    [Channels.revisitOwnedDocument]: (event, ...args) => {
      guard(event); noArgs(args); return bridge.revisitOwnedDocument();
    },
  };
  for (const [channel, handler] of Object.entries(handlers)) ipc.handle(channel, handler);
  return () => {
    for (const channel of Object.keys(handlers)) ipc.removeHandler(channel);
  };
}
