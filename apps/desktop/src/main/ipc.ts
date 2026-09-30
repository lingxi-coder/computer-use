import { validateOperatorCommand, type UiProjection } from '@computer-user/desktop-api';

import { Channels } from '../shared/channels.js';
import type { HostBridge } from './hostBridge.js';

/** The pages that may call main: the packaged renderer, or a loopback dev server. */
export interface TrustedOrigins {
  files: string[];
  devOrigin?: string;
}

const MAX_TASK_TEXT = 2_000;

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
export function registerIpc(ipc: IpcMainLike, bridge: HostBridge, trusted: TrustedOrigins): () => void {
  const guard = (event: IpcEventLike): void => {
    if (!isTrustedSender(event.senderFrame?.url, trusted)) throw new Error('refused: untrusted sender');
  };
  const command = (value: unknown): void => {
    const result = validateOperatorCommand(value);
    if (!result.ok) throw new Error(`refused: ${result.errors.join('; ')}`);
  };
  const handlers: Record<string, (event: IpcEventLike, ...args: unknown[]) => unknown> = {
    [Channels.createTask]: (event, text) => {
      guard(event);
      if (typeof text !== 'string' || text.trim() === '' || text.length > MAX_TASK_TEXT) {
        throw new Error('refused: a task needs some text of reasonable length');
      }
      command({ command: 'create_task', text });
      bridge.createTask(text);
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
    [Channels.stop]: (event) => {
      guard(event);
      bridge.pressStop();
    },
    [Channels.setMode]: (event, mode) => {
      guard(event);
      command({ command: 'set_mode', mode });
      bridge.setMode(mode as 'observe' | 'shadow' | 'execute');
    },
    [Channels.snapshot]: (event): UiProjection | null => {
      guard(event);
      return bridge.snapshot();
    },
  };
  for (const [channel, handler] of Object.entries(handlers)) ipc.handle(channel, handler);
  return () => {
    for (const channel of Object.keys(handlers)) ipc.removeHandler(channel);
  };
}
