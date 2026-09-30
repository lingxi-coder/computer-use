import { EventEmitter } from 'node:events';

import type { BridgeStopStatus, OperatorCommand, StopAck, StopDisposition, StopScope, StopSource } from '@computer-user/desktop-api';

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
  }

  private set(status: BridgeStopStatus): void {
    if (this.current === status) return;
    this.current = status;
    this.emit('status', status);
  }
}
