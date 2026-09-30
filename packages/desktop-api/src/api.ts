import type { RunMode, UiProjection } from './generated/envelope.js';

/**
 * What a stop has reached. The button, the tray and the hotkey all send the
 * stop straight to the device; a click alone never becomes "stopped".
 */
export type BridgeStopStatus = 'idle' | 'stopping' | 'confirmed' | 'unconfirmed';

/**
 * The whole surface the renderer sees (design 4.6). It has no channel name, no
 * path, no shell and no native input: only these calls, each of which main
 * checks against the published schema before doing anything.
 */
export interface DesktopApi {
  createTask(text: string): Promise<void>;
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
}
