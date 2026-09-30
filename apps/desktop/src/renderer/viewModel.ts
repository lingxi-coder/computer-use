import type { BridgeStopStatus, ExpectedValue, RootState, RunMode, UiProjection } from '@computer-user/desktop-api';

export type StopLevel = 'none' | 'pending' | 'ok' | 'warning';

export interface Prompt {
  kind: 'goal' | 'effect';
  id: string;
  title: string;
  lines: string[];
}

export interface ViewModel {
  mode: RunMode;
  rootTaskId: string | null;
  stateLabel: string;
  headline: string;
  progress: { used: number; max: number } | null;
  canCreate: boolean;
  canPause: boolean;
  canResume: boolean;
  canStop: boolean;
  prompt: Prompt | null;
  stop: { level: StopLevel; text: string };
  capability: string;
  timeline: string[];
}

const STATE_LABELS: Record<RootState, string> = {
  draft: 'Draft',
  compiling_goal: 'Understanding the task',
  awaiting_confirmation: 'Waiting for you to confirm the goal',
  needs_input: 'Waiting for your decision',
  ready: 'Ready',
  running: 'Running',
  waiting_evidence: 'Looking at the screen',
  waiting_plan: 'Waiting for a new plan',
  pausing: 'Pausing',
  suspended: 'Paused',
  needs_revalidation: 'Paused: checking before it continues',
  cancelling: 'Stopping',
  completed: 'Done',
  failed: 'Failed',
  unverified: 'Finished, but the result could not be confirmed',
  expired: 'Ran out of time',
  cancelled: 'Stopped',
};

const TERMINAL: ReadonlySet<RootState> = new Set(['completed', 'failed', 'unverified', 'expired', 'cancelled']);

export function stateLabel(state: RootState): string {
  return STATE_LABELS[state];
}

export function isTerminal(state: RootState): boolean {
  return TERMINAL.has(state);
}

function valueText(value: ExpectedValue): string {
  switch (value.type) {
    case 'bool':
      return value.value ? 'on' : 'off';
    case 'text':
      return `"${value.value}"`;
    case 'number':
      return String(value.value);
  }
}

function promptOf(projection: UiProjection): Prompt | null {
  const pending = projection.pending_confirmation;
  if (!pending) return null;
  if (pending.kind.kind === 'goal') {
    const goal = pending.kind.goal;
    const where = goal.group ? `"${goal.target_label}" in ${goal.group}` : `"${goal.target_label}"`;
    return {
      kind: 'goal',
      id: pending.confirmation_id,
      title: 'Is this what you meant?',
      lines: [
        `Your words: ${goal.user_text}`,
        `Application: ${goal.app}`,
        `Control: ${where}`,
        `Wanted value: ${valueText(goal.desired)}`,
        goal.confirmation_scope === 'commit'
          ? 'Done when the change is saved, not only shown.'
          : 'Done when the value still holds after leaving and re-entering the page.',
        `It may change things up to: ${goal.max_effect_class.replace('_', ' ')}`,
      ],
    };
  }
  const action = pending.kind.action;
  return {
    kind: 'effect',
    id: pending.confirmation_id,
    title: 'Allow this one action?',
    lines: [
      `Press "${action.target_label}"`,
      action.description,
      `Effect: ${action.effect_class.replace('_', ' ')}`,
      'Your answer covers this one action only.',
    ],
  };
}

/** A click is not a stop: what is shown is what has been confirmed. */
export function stopBanner(host: UiProjection['stop_status'] | undefined, bridge: BridgeStopStatus): ViewModel['stop'] {
  if (bridge === 'unconfirmed' || host === 'stop_unconfirmed') {
    return {
      level: 'warning',
      text: 'Stop not confirmed: the device has not said it released the input. Do not assume it stopped.',
    };
  }
  if (bridge === 'confirmed' || host === 'stopped_confirmed') {
    return { level: 'ok', text: 'Stopped: the device confirmed it released the input.' };
  }
  if (bridge === 'stopping' || host === 'stopping') return { level: 'pending', text: 'Stopping…' };
  return { level: 'none', text: '' };
}

export function toViewModel(projection: UiProjection | null, bridge: BridgeStopStatus): ViewModel {
  const root = projection?.root ?? null;
  const state = root?.state ?? null;
  const active = state !== null && !isTerminal(state);
  return {
    mode: projection?.mode ?? 'observe',
    rootTaskId: root?.root_task_id ?? null,
    stateLabel: state ? stateLabel(state) : 'No task',
    headline: root?.headline ?? '',
    progress: root && root.attempts_max > 0 ? { used: root.attempts_used, max: root.attempts_max } : null,
    canCreate: !active,
    canPause: state === 'running' || state === 'waiting_evidence' || state === 'waiting_plan' || state === 'needs_input',
    canResume: state === 'suspended' || state === 'needs_revalidation',
    canStop: active,
    prompt: projection ? promptOf(projection) : null,
    stop: stopBanner(projection?.stop_status, bridge),
    capability: projection?.capabilities
      ? `${projection.capabilities.backend === 'simulated' ? 'Simulated device' : 'Device'} · session ${projection.capabilities.identity.session_epoch}`
      : 'No device connected',
    timeline: (projection?.timeline ?? []).slice(-12).map((entry) => entry.text),
  };
}
