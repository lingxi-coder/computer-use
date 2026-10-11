import { EventEmitter } from 'node:events';

import type { BridgeStopStatus, OperatorCommand, StopAck, StopDisposition, StopScope, StopSource, UiProjection } from '@computer-use/desktop-api';

/** The three ways in (design 14.8). None of them depends on the renderer. */
export type StopEntry = Extract<StopSource, 'operator_button' | 'tray' | 'global_hotkey'>;

export interface StopTransport {
  stopAtDevice(scope: StopScope, disposition: StopDisposition, source: StopSource, reason?: string): void;
  operator(command: OperatorCommand): void;
  on(event: 'stopAck', listener: (ack: StopAck) => void): unknown;
  off(event: 'stopAck', listener: (ack: StopAck) => void): unknown;
}

/** Electron's globalShortcut, or a stand-in in tests. */
export interface HotkeyRegistry {
  register(accelerator: string, callback: () => void): boolean;
  unregister(accelerator: string): void;
}

export interface StopBridgeOptions {
  accelerator?: string;
  /** How long the device may take to confirm before the stop is shown as unconfirmed. */
  confirmTimeoutMs?: number;
}

export const DEFAULT_ACCELERATOR = 'CommandOrControl+Shift+Escape';

/**
 * The stop control. It sends the stop to the device first and directly, then
 * tells the host so the task can follow, and reports what has actually been
 * confirmed: pressing the button is not the same as having stopped.
 */
export class StopBridge extends EventEmitter<{ status: [BridgeStopStatus] }> {
  private current: BridgeStopStatus = 'idle';
  private timer: NodeJS.Timeout | null = null;
  private waitingFor: StopEntry | null = null;
  private projection: UiProjection | null = null;
  private stoppedContext: { rootId: string | null; deviceEpoch: number | null } | null = null;
  private resumedSinceStop = false;
  private registered = false;
  private readonly accelerator: string;
  private readonly confirmTimeoutMs: number;
  private readonly onAck = (ack: StopAck): void => this.acknowledged(ack);

  constructor(
    private readonly transport: StopTransport,
    private readonly hotkeys: HotkeyRegistry,
    options: StopBridgeOptions = {},
  ) {
    super();
    this.accelerator = options.accelerator ?? DEFAULT_ACCELERATOR;
    this.confirmTimeoutMs = options.confirmTimeoutMs ?? 2_000;
  }

  get status(): BridgeStopStatus {
    return this.current;
  }

  /** A replaced session cannot inherit the previous device's release confirmation. */
  resetSession(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null; this.waitingFor = null; this.stoppedContext = null;
    this.projection = null; this.resumedSinceStop = false;
    this.set(this.current === 'unconfirmed' || this.current === 'stopping' ? 'unconfirmed' : 'idle');
  }

  /** An old release confirmation is not the state of a new or explicitly resumed task. */
  observeProjection(projection: UiProjection): void {
    const previous = this.projection;
    this.projection = projection;
    if (projection.root?.root_task_id === previous?.root?.root_task_id
      && previous?.root?.state === 'suspended'
      && projection.root?.state === 'needs_revalidation') this.resumedSinceStop = true;
    this.clearObsoleteConfirmation();
  }

  private clearObsoleteConfirmation(): void {
    const projection = this.projection;
    if (!projection) return;
    if (this.current !== 'confirmed' || projection.stop_status !== 'not_stopped' || !this.stoppedContext) return;
    const rootId = projection.root?.root_task_id ?? null;
    const deviceEpoch = projection.capabilities?.identity.session_epoch ?? null;
    const newContext = rootId !== this.stoppedContext.rootId || deviceEpoch !== this.stoppedContext.deviceEpoch;
    if (!newContext && !this.resumedSinceStop) return;
    this.waitingFor = null;
    this.stoppedContext = null;
    this.set('idle');
  }

  /** Register the hotkey. `false` means the system refused it; the button and the tray still work. */
  start(): boolean {
    this.transport.on('stopAck', this.onAck);
    this.registered = this.hotkeys.register(this.accelerator, () => this.press('global_hotkey'));
    return this.registered;
  }

  dispose(): void {
    if (this.registered) this.hotkeys.unregister(this.accelerator);
    this.registered = false;
    this.transport.off('stopAck', this.onAck);
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  press(entry: StopEntry): void {
    this.resumedSinceStop = false;
    this.stoppedContext = {
      rootId: this.projection?.root?.root_task_id ?? null,
      deviceEpoch: this.projection?.capabilities?.identity.session_epoch ?? null,
    };
    // The device first: it latches the stop whether or not anything else is
    // listening. Then the host, so the task's own state follows.
    this.transport.stopAtDevice({ scope: 'all' }, 'cancel', entry, 'the operator pressed stop');
    this.transport.operator({ command: 'stop_task', root_task_id: null });
    this.waitingFor = entry;
    this.set('stopping');
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      // A click is not a stop: if the device has not said it let go, say so.
      if (this.current === 'stopping') this.set('unconfirmed');
    }, this.confirmTimeoutMs);
    this.timer.unref();
  }

  private acknowledged(ack: StopAck): void {
    if (this.waitingFor === null || ack.disposition !== 'cancel' || ack.source !== this.waitingFor) return;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    // The latch is set, but it only counts as stopped if the input was let go.
    this.set(ack.cleanup === 'released' || ack.cleanup === 'not_needed' ? 'confirmed' : 'unconfirmed');
    this.clearObsoleteConfirmation();
  }

  private set(status: BridgeStopStatus): void {
    if (this.current === status) return;
    this.current = status;
    this.emit('status', status);
  }
}
