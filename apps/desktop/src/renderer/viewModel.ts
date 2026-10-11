import type { BridgeStopStatus, ExpectedValue, FrameStatus, GoalSummary, RootState, RunMode, SourceState, UiProjection } from '@computer-use/desktop-api';

export type StopLevel = 'none' | 'pending' | 'ok' | 'warning';

export interface Prompt {
  kind: 'goal' | 'effect';
  id: string;
  title: string;
  lines: string[];
}

export interface ObservationDisplay {
  level: 'ready' | 'pending' | 'warning';
  title: string;
  readOnly: boolean;
  app: string | null;
  window: string | null;
  sources: { label: string; state: SourceState; stateLabel: string; reason: string | null }[];
  frame: { status: string; dimensions: string; imageAge: string; confirmationAge: string; dropped: number; healthy: boolean } | null;
}

export interface BrowserObservationDisplay {
  checked: boolean | null;
  readOnly: true;
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
  canExecute: boolean;
  canSelectBrowserPageProfile: boolean;
  browserProfileActive: boolean;
  browserObservation: BrowserObservationDisplay | null;
  prompt: Prompt | null;
  stop: { level: StopLevel; text: string };
  capability: string;
  observation: ObservationDisplay | null;
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
const SOURCE_LABELS = { screenshot: 'Screen', ax: 'Accessibility', ax_observer: 'AX event coverage', ocr: 'Text recognition' };
const SOURCE_STATES: Record<SourceState, string> = { available: 'Available', degraded: 'Degraded', unavailable: 'Unavailable' };
const FRAME_LABELS: Record<FrameStatus, string> = {
  complete: 'Frame captured', idle: 'No screen change', blank: 'Blank image',
  suspended: 'Capture paused', started: 'Capture starting', stopped: 'Capture stopped',
};

function ageText(ms: number | null | undefined): string {
  if (ms == null) return 'Unknown';
  if (ms < 1_000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1_000).toFixed(1)} s`;
  return `${Math.floor(ms / 60_000)} min ${Math.floor((ms % 60_000) / 1_000)} s`;
}

export function observationDisplay(projection: UiProjection | null): ObservationDisplay | null {
  const observation = projection?.observation;
  const native = projection?.capabilities?.backend === 'macos';
  if (!observation && !native) return null;
  const sources = (['screenshot', 'ax', 'ax_observer', 'ocr'] as const).flatMap((kind) => {
    const source = observation?.sources.find((item) => item.source === kind);
    return source ? [{
      label: SOURCE_LABELS[kind], state: source.state,
      stateLabel: SOURCE_STATES[source.state], reason: source.reason ?? null,
    }] : [];
  });
  const incomplete = sources.some((source) => source.state !== 'available');
  const anyAvailable = sources.some((source) => source.state !== 'unavailable');
  const frame = observation?.frame;
  const ready = sources.length > 0 && !incomplete && frame?.healthy === true;
  return {
    level: incomplete ? 'warning' : ready ? 'ready' : 'pending',
    title: incomplete ? anyAvailable ? 'Observation partly available' : 'Observation unavailable'
      : ready ? 'Observing' : frame ? 'Waiting for a fresh image' : 'Waiting for observation',
    readOnly: native && projection?.capabilities?.input !== 'available',
    app: observation?.target_name ?? observation?.target_app ?? null,
    window: observation?.active_window ?? null,
    sources,
    frame: frame ? {
      status: FRAME_LABELS[frame.status], dimensions: `${frame.width_px} × ${frame.height_px} px`,
      imageAge: ageText(frame.pixel_age_ms), confirmationAge: ageText(frame.confirmation_age_ms),
      dropped: frame.dropped_frames, healthy: frame.healthy,
    } : null,
  };
}

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

function scopedTargetText(target: { label: string }): string {
  return `"${target.label}"`;
}

function scopedCommitLines(commit: GoalSummary['scoped_commit']): string[] {
  if (!commit) return [];
  const group = commit.receipt.group || commit.action?.group || null;
  const context = group ? ` in "${group}"` : '';
  if (commit.receipt.kind === 'automatic') {
    return [
      `The app saves changes automatically${context}.`,
      'Wait for the app’s automatic save confirmation before the task is considered done.',
    ];
  }
  return commit.action
    ? [`Manual save: press "${commit.action.label}"${context}.`]
    : [`A manual save is required${context}; use the app’s Save control.`];
}

function promptOf(projection: UiProjection): Prompt | null {
  const pending = projection.pending_confirmation;
  if (!pending) return null;
  if (pending.kind.kind === 'goal') {
    const goal = pending.kind.goal;
    const scopedCommit = goal.scoped_commit;
    const allowedScope = goal.allowed_scope;
    const allowedTarget = allowedScope?.targets[0];
    const where = allowedTarget?.form
      ? `"${goal.target_label}" in form "${allowedTarget.form}"`
      : goal.group ? `"${goal.target_label}" in ${goal.group}` : `"${goal.target_label}"`;
    return {
      kind: 'goal',
      id: pending.confirmation_id,
      title: 'Is this what you meant?',
      lines: [
        `Your words: ${goal.user_text}`,
        `Application: ${goal.app}`,
        `Control: ${where}`,
        ...(allowedScope && allowedTarget
          ? [
              `Allowed application: ${allowedScope.application}`,
              `Allowed control: ${allowedTarget.control}`,
              ...(allowedTarget.group ? [`Allowed group: ${allowedTarget.group}`] : []),
              ...(allowedTarget.form ? [`Allowed form: ${allowedTarget.form}`] : []),
              ...(allowedTarget.disambiguator ? [`Allowed disambiguator: ${allowedTarget.disambiguator}`] : []),
              ...(allowedScope.no_unrelated_controls ? ['Unrelated controls are outside this task.'] : []),
            ]
          : []),
        ...(goal.scoped_revisit
          ? [
              `To verify the change, it may switch to ${scopedTargetText(goal.scoped_revisit.away)} and return to ${scopedTargetText(goal.scoped_revisit.home)}.`,
              'This is the only allowed detour; the task control scope remains unchanged.',
            ]
          : []),
        ...(goal.row
          ? [
              `Table: ${goal.row.table_scope.scroll_path.map(container => container.label).join(' → ')}`,
              `Columns: ${goal.row.table_scope.column_headers.join(', ')}`,
              ...goal.row.conditions.map(condition => `${condition.column_name}: ${valueText(condition.value)}`),
              'Wanted state: this row is selected.',
            ]
          : [`Wanted value: ${valueText(goal.desired)}`]),
        ...scopedCommitLines(scopedCommit),
        goal.confirmation_scope === 'commit'
          ? 'Done when the change is saved, not only shown.'
          : goal.confirmation_scope === 'current_ui'
            ? 'Done when a fresh read confirms this value on the current page.'
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
  if (bridge === 'stopping' || host === 'stopping') return { level: 'pending', text: 'Stopping…' };
  if (bridge === 'confirmed' || host === 'stopped_confirmed') {
    return { level: 'ok', text: 'Stopped: the device confirmed it released the input.' };
  }
  return { level: 'none', text: '' };
}

export function toViewModel(projection: UiProjection | null, bridge: BridgeStopStatus): ViewModel {
  const root = projection?.root ?? null;
  const state = root?.state ?? null;
  const active = state !== null && !isTerminal(state);
  const capabilities = projection?.capabilities;
  const browserProfileActive = capabilities?.backend === 'headless_browser'
    && capabilities.execution_domain === 'headless_browser';
  const browserDomain = capabilities?.domains.find((domain) => domain.domain === 'headless_browser');
  const browserActionAvailable = browserProfileActive
    && browserDomain?.state === 'available'
    && browserDomain.actions.some((action) => action === 'headless_browser_action');
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
    canExecute: browserActionAvailable || (capabilities?.execution_domain !== 'headless_browser'
      && capabilities?.input === 'available'
      && capabilities.human_input === 'available' && capabilities.hit_test === 'available'),
    canSelectBrowserPageProfile: !active && capabilities?.backend === 'simulated'
      && capabilities.execution_domain === 'simulated'
      && browserDomain?.state === 'unavailable',
    browserProfileActive,
    browserObservation: browserProfileActive ? {
      checked: projection?.browser_observation?.readonly_checked ?? null,
      readOnly: true,
    } : null,
    prompt: projection ? promptOf(projection) : null,
    stop: stopBanner(projection?.stop_status, bridge),
    capability: capabilities
      ? `${browserProfileActive ? 'Fixture browser' : capabilities.backend === 'simulated' ? 'Simulated device' : 'Device'} · session ${capabilities.identity.session_epoch}`
      : 'No device connected',
    observation: observationDisplay(projection),
    timeline: (projection?.timeline ?? []).slice(-12).map((entry) => entry.text),
  };
}
