import type { OperatorCommand, RunMode, UiProjection } from '@computer-user/desktop-api';

import type { StopBridge } from './stopBridge.js';

/** What the bridge needs of the link to cu-host. */
export interface HostLink {
  operator(command: OperatorCommand): void;
  readonly projection: UiProjection | null;
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
  ) {}

  createTask(text: string): void {
    this.host.operator({ command: 'create_task', text });
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

  setMode(mode: RunMode): void {
    this.host.operator({ command: 'set_mode', mode });
  }

  pressStop(): void {
    this.stop.press('operator_button');
  }

  snapshot(): UiProjection | null {
    return this.host.projection;
  }
}
