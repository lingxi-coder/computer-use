import { contextBridge, ipcRenderer } from 'electron';

import type { BridgeStopStatus, DesktopApi, RunMode, UiProjection } from '@computer-user/desktop-api';

import { Channels } from '../shared/channels';

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
  createTask: (text: string) => ipcRenderer.invoke(Channels.createTask, text) as Promise<void>,
  confirm: (confirmationId: string, approved: boolean) =>
    ipcRenderer.invoke(Channels.confirm, confirmationId, approved) as Promise<void>,
  pause: (rootTaskId: string) => ipcRenderer.invoke(Channels.pause, rootTaskId) as Promise<void>,
  resume: (rootTaskId: string) => ipcRenderer.invoke(Channels.resume, rootTaskId) as Promise<void>,
  stop: () => ipcRenderer.invoke(Channels.stop) as Promise<void>,
  setMode: (mode: RunMode) => ipcRenderer.invoke(Channels.setMode, mode) as Promise<void>,
  snapshot: () => ipcRenderer.invoke(Channels.snapshot) as Promise<UiProjection | null>,
  onProjection: (listener) => subscribe<UiProjection>(Channels.projection, listener),
  onStopStatus: (listener) => subscribe<BridgeStopStatus>(Channels.stopStatus, listener),
};

contextBridge.exposeInMainWorld('desktop', api);
