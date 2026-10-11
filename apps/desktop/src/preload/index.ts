import { contextBridge, ipcRenderer } from 'electron';

import type {
  BridgeStopStatus,
  DesktopApi,
  RunMode,
  UiProjection,
  ObservationImage,
  ImageAvailability,
  NativeAuthorization,
  ModelPurpose,
  ProviderSettingsSnapshot,
  OpenAiLoginRequest,
  OwnedDocumentStatus,
  FrozenTaskSelection,
  FrozenTaskCatalogSnapshot,
  ScopedTaskRequest,
} from '@computer-use/desktop-api';

import { Channels } from '../shared/channels';
import type { ModelAuthorizationStatus } from '@computer-use/desktop-api';

function subscribe<T>(channel: string, listener: (value: T) => void): () => void {
  const handler = (_event: unknown, value: T): void => listener(value);
  ipcRenderer.on(channel, handler);
  return () => {
    ipcRenderer.removeListener(channel, handler);
  };
}

/**
 * The whole surface the renderer gets. There is no generic send, no channel
 * name to choose and nothing that reaches a file, a shell or native input; main
 * checks every call again before acting on it.
 */
const api: DesktopApi = {
  selectBrowserPageProfile: () => ipcRenderer.invoke(Channels.selectBrowserPageProfile) as Promise<void>,
  createTask: (text: string) => ipcRenderer.invoke(Channels.createTask, text) as Promise<void>,
  createScopedTask: (request: ScopedTaskRequest) => ipcRenderer.invoke(Channels.createScopedTask, request) as Promise<void>,
  frozenTaskCatalog: () => ipcRenderer.invoke(Channels.frozenTaskCatalog) as Promise<FrozenTaskCatalogSnapshot>,
  createFrozenTask: (selection: FrozenTaskSelection) => ipcRenderer.invoke(Channels.createFrozenTask, selection) as Promise<void>,
  confirm: (confirmationId: string, approved: boolean) =>
    ipcRenderer.invoke(Channels.confirm, confirmationId, approved) as Promise<void>,
  pause: (rootTaskId: string) => ipcRenderer.invoke(Channels.pause, rootTaskId) as Promise<void>,
  resume: (rootTaskId: string) => ipcRenderer.invoke(Channels.resume, rootTaskId) as Promise<void>,
  stop: () => ipcRenderer.invoke(Channels.stop) as Promise<void>,
  setMode: (mode: RunMode) => ipcRenderer.invoke(Channels.setMode, mode) as Promise<void>,
  snapshot: () => ipcRenderer.invoke(Channels.snapshot) as Promise<UiProjection | null>,
  image: (frameId: string) => ipcRenderer.invoke(Channels.image, frameId) as Promise<ObservationImage | null>,
  onImageAvailability: (listener) => subscribe<ImageAvailability>(Channels.imageAvailability, listener),
  onProjection: (listener) => subscribe<UiProjection>(Channels.projection, listener),
  onStopStatus: (listener) => subscribe<BridgeStopStatus>(Channels.stopStatus, listener),
  nativeAuthorization: () => ipcRenderer.invoke(Channels.nativeAuthorization) as Promise<NativeAuthorization>,
  authorizeNativeWindow: (snapshotId) => ipcRenderer.invoke(Channels.authorizeNativeWindow, snapshotId) as Promise<void>,
  refreshNativeObservation: () => ipcRenderer.invoke(Channels.refreshNativeObservation) as Promise<void>,
  revokeNativeWindow: () => ipcRenderer.invoke(Channels.revokeNativeWindow) as Promise<void>,
  onNativeAuthorization: (listener) => subscribe<NativeAuthorization>(Channels.nativeAuthorizationChanged, listener),
  presentation: () => ipcRenderer.invoke(Channels.presentation) as Promise<'console' | 'indicator'>,
  onPresentation: (listener) => subscribe<'console' | 'indicator'>(Channels.presentationChanged, listener),
  providerSettings: () => ipcRenderer.invoke(Channels.providerSettings) as Promise<ProviderSettingsSnapshot>,
  configureProvider: (request) => ipcRenderer.invoke(Channels.configureProvider, request) as Promise<void>,
  refreshProviderModels: (id) => ipcRenderer.invoke(Channels.refreshProviderModels, id) as Promise<void>,
  removeProvider: (id) => ipcRenderer.invoke(Channels.removeProvider, id) as Promise<void>,
  selectTaskModel: (selection) => ipcRenderer.invoke(Channels.selectTaskModel, selection) as Promise<void>,
  onProviderSettings: (listener) => subscribe<ProviderSettingsSnapshot>(Channels.providerSettingsChanged, listener),
  modelAuthorization: () => ipcRenderer.invoke(Channels.modelAuthorization) as Promise<ModelAuthorizationStatus>,
  authorizeModel: (request) => ipcRenderer.invoke(Channels.authorizeModel, request) as Promise<void>,
  authorizeOpenAiLogin: (request: OpenAiLoginRequest) => ipcRenderer.invoke(Channels.authorizeOpenAiLogin, request) as Promise<void>,
  authorizeSavedModel: (id: string, purposes: ModelPurpose[]) => ipcRenderer.invoke(Channels.authorizeSavedModel, id, purposes) as Promise<void>,
  revokeModel: () => ipcRenderer.invoke(Channels.revokeModel) as Promise<void>,
  forgetSavedModel: (id) => ipcRenderer.invoke(Channels.forgetSavedModel, id) as Promise<void>,
  onModelAuthorization: (listener) => subscribe<ModelAuthorizationStatus>(Channels.modelAuthorizationChanged, listener),
  ownedDocumentStatus: () => ipcRenderer.invoke(Channels.ownedDocumentStatus) as Promise<OwnedDocumentStatus>,
  createOwnedDocument: () => ipcRenderer.invoke(Channels.createOwnedDocument) as Promise<void>,
  authorizeOwnedDocumentEdit: () => ipcRenderer.invoke(Channels.authorizeOwnedDocumentEdit) as Promise<void>,
  authorizeOwnedDocumentSave: () => ipcRenderer.invoke(Channels.authorizeOwnedDocumentSave) as Promise<void>,
  revisitOwnedDocument: () => ipcRenderer.invoke(Channels.revisitOwnedDocument) as Promise<void>,
  onOwnedDocumentStatus: (listener) => subscribe<OwnedDocumentStatus>(Channels.ownedDocumentStatusChanged, listener),
};

contextBridge.exposeInMainWorld('desktop', api);
