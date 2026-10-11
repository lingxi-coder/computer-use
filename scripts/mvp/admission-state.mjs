const ROOT_STATES = new Set([
  'draft', 'compiling_goal', 'awaiting_confirmation', 'needs_input', 'ready', 'running',
  'waiting_evidence', 'waiting_plan', 'pausing', 'suspended', 'needs_revalidation',
  'cancelling', 'completed', 'failed', 'unverified', 'expired', 'cancelled',
]);

const TERMINAL_ROOT_STATES = new Set(['completed', 'failed', 'unverified', 'expired', 'cancelled']);
const READY_ROOT_STATES = new Set(['ready', 'waiting_plan']);

function validRoot(root) {
  return root !== null && typeof root === 'object' && !Array.isArray(root)
    && typeof root.root_task_id === 'string' && root.root_task_id.length > 0 && root.root_task_id.length <= 256
    && Number.isSafeInteger(root.task_revision) && root.task_revision >= 0
    && typeof root.state === 'string' && ROOT_STATES.has(root.state);
}

function rootIdentity(root) {
  return { root_task_id: root.root_task_id, task_revision: root.task_revision, state: root.state };
}

export function observeAdmissionRoot(previous, root) {
  if (root === null || root === undefined) return { observedRoot: previous, invalid: false, conflict: null };
  if (!validRoot(root)) return { observedRoot: previous, invalid: true, conflict: null };

  const current = rootIdentity(root);
  if (!previous) {
    return {
      observedRoot: {
        root_task_id: current.root_task_id,
        task_revision: current.task_revision,
        first_state: current.state,
        latest_state: current.state,
        states_observed: [current.state],
        observation_count: 1,
      },
      invalid: false,
      conflict: null,
    };
  }
  if (previous.root_task_id !== current.root_task_id || previous.task_revision !== current.task_revision) {
    return { observedRoot: previous, invalid: false, conflict: current };
  }

  const states = previous.states_observed.includes(current.state)
    ? previous.states_observed
    : [...previous.states_observed, current.state].slice(-16);
  return {
    observedRoot: {
      ...previous,
      latest_state: current.state,
      states_observed: states,
      observation_count: Math.min(Number.MAX_SAFE_INTEGER, previous.observation_count + 1),
    },
    invalid: false,
    conflict: null,
  };
}

export function classifyGoalAdmission(projection, observedRoot) {
  const root = projection?.root ?? null;
  if (root && TERMINAL_ROOT_STATES.has(root.state)) return { kind: 'terminal_root', terminalRoot: root };

  const prompt = projection?.pending_confirmation;
  if (prompt?.kind?.kind === 'goal') {
    if (!observedRoot || !root) return { kind: 'pending' };
    const matchesObserved = prompt.root_task_id === observedRoot.root_task_id
      && prompt.task_revision === observedRoot.task_revision
      && root.root_task_id === observedRoot.root_task_id
      && root.task_revision === observedRoot.task_revision;
    return matchesObserved ? { kind: 'goal_prompt', prompt } : { kind: 'goal_prompt_mismatch', prompt };
  }
  return { kind: 'pending' };
}

export async function waitForGoalAdmission({
  readProjection,
  readUiError = async () => null,
  onObservation = () => {},
  timeoutMs = 30_000,
  pollIntervalMs = 100,
}) {
  let observedRoot = null;
  let lastProjection = null;
  let lastError = null;
  const deadline = Date.now() + timeoutMs;

  while (Date.now() < deadline) {
    try {
      const projection = await readProjection();
      lastProjection = projection;
      const observation = observeAdmissionRoot(observedRoot, projection?.root ?? null);
      onObservation(observation);
      if (observation.invalid) return { kind: 'invalid_root_projection', projection, observedRoot };
      if (observation.conflict) return { kind: 'root_identity_conflict', projection, observedRoot, conflict: observation.conflict };
      observedRoot = observation.observedRoot;

      const classification = classifyGoalAdmission(projection, observedRoot);
      if (classification.kind !== 'pending') return { ...classification, projection, observedRoot };

      const refusal = (projection?.timeline ?? []).slice(-12)
        .find(item => typeof item?.text === 'string' && /refused/i.test(item.text));
      if (refusal) return { kind: 'explicit_refusal', projection, refusal, observedRoot };
      const uiError = await readUiError();
      if (typeof uiError === 'string' && /refus|unsupported|unavailable|cannot|could not/i.test(uiError)) {
        return { kind: 'explicit_refusal', projection, refusal: { text: uiError }, observedRoot };
      }
    } catch (error) {
      lastError = error;
    }
    await new Promise(resolve => setTimeout(resolve, pollIntervalMs));
  }

  return { kind: 'timeout', projection: lastProjection, observedRoot, error: lastError };
}

export function classifyConfirmedRootReadiness(projection, rootTaskId, taskRevision) {
  const root = projection?.root ?? null;
  if (!root) return { kind: 'pending' };
  if (!validRoot(root)) return { kind: 'invalid_root', root };
  if (root.root_task_id !== rootTaskId || root.task_revision !== taskRevision) {
    return { kind: 'root_identity_mismatch', root };
  }
  if (TERMINAL_ROOT_STATES.has(root.state)) return { kind: 'terminal_root', root };
  if (root.state === 'suspended') return { kind: 'not_ready', root };
  if (READY_ROOT_STATES.has(root.state)) return { kind: 'ready', root };
  return { kind: 'pending' };
}

export async function waitForConfirmedRootReady({
  readProjection,
  rootTaskId,
  taskRevision,
  timeoutMs = 120_000,
  pollIntervalMs = 100,
}) {
  let lastProjection = null;
  let lastError = null;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const projection = await readProjection();
      lastProjection = projection;
      const classification = classifyConfirmedRootReadiness(projection, rootTaskId, taskRevision);
      if (classification.kind !== 'pending') return { ...classification, projection };
    } catch (error) {
      lastError = error;
    }
    await new Promise(resolve => setTimeout(resolve, pollIntervalMs));
  }
  return { kind: 'timeout', projection: lastProjection, error: lastError };
}

export function classifyPreExecuteSource(projection, {
  rootTaskId,
  taskRevision,
  fixturePid,
  bundleId,
  windowId,
  baselineTarget = null,
  minimumTarget = null,
  requireFreshSource = true,
}) {
  if (!projection || typeof projection !== 'object' || Array.isArray(projection)) return { kind: 'invalid_projection' };
  const root = projection.root ?? null;
  if (!validRoot(root)) return { kind: 'invalid_root', root };
  if (root.root_task_id !== rootTaskId || root.task_revision !== taskRevision) {
    return { kind: 'root_identity_mismatch', root };
  }
  if (TERMINAL_ROOT_STATES.has(root.state)) return { kind: 'terminal_root', root };
  if (root.state === 'suspended') return { kind: 'human_suspended', root };
  if (root.state === 'needs_input' || projection.pending_confirmation != null) {
    return { kind: 'awaiting_user_input', root, pendingConfirmation: projection.pending_confirmation ?? null };
  }
  if (projection.mode !== 'observe') return { kind: 'mode_changed', root, mode: projection.mode ?? null };
  if (projection.stop_status !== 'not_stopped') return { kind: 'stop_requested', root, stopStatus: projection.stop_status ?? null };
  if (!READY_ROOT_STATES.has(root.state)) return { kind: 'root_not_ready', root };

  const target = projection.observation?.observed_target ?? null;
  if (!target || target.pid !== fixturePid || target.app !== bundleId || target.window_id !== windowId) {
    return { kind: 'pending', reason: 'fixture_source_target_not_current', root, target };
  }
  if (typeof target.snapshot_id !== 'string' || target.snapshot_id.length === 0 || target.snapshot_id.length > 256
      || !Number.isSafeInteger(target.structure_sequence) || target.structure_sequence <= 0
      || !Number.isSafeInteger(target.observed_ms) || target.observed_ms < 0
      || !Number.isSafeInteger(target.valid_until_ms) || target.valid_until_ms <= target.observed_ms
      || target.valid_until_ms - target.observed_ms > 2_000) {
    return { kind: 'pending', reason: 'fixture_source_target_invalid_or_stale', root, target };
  }

  const baselineHasIdentity = baselineTarget
    && (Number.isSafeInteger(baselineTarget.structure_sequence) || Number.isSafeInteger(baselineTarget.observed_ms));
  const advanced = !baselineHasIdentity
    || (Number.isSafeInteger(baselineTarget.structure_sequence) && target.structure_sequence > baselineTarget.structure_sequence)
    || (Number.isSafeInteger(baselineTarget.observed_ms) && target.observed_ms > baselineTarget.observed_ms);
  if (requireFreshSource && !advanced) {
    return { kind: 'pending', reason: 'fixture_source_target_not_fresh_after_activation', root, target };
  }

  const doesNotRegress = !minimumTarget
    || ((!Number.isSafeInteger(minimumTarget.structure_sequence) || target.structure_sequence >= minimumTarget.structure_sequence)
      && (!Number.isSafeInteger(minimumTarget.observed_ms) || target.observed_ms >= minimumTarget.observed_ms));
  if (!doesNotRegress) return { kind: 'pending', reason: 'fixture_source_target_regressed', root, target };
  return { kind: 'ready', root, target };
}

export async function waitForFreshPreExecuteSource({
  readProjection,
  rootTaskId,
  taskRevision,
  fixturePid,
  bundleId,
  windowId,
  baselineTarget = null,
  timeoutMs = 15_000,
  pollIntervalMs = 100,
}) {
  let lastProjection = null;
  let lastClassification = null;
  let lastError = null;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const projection = await readProjection();
      lastProjection = projection;
      lastClassification = classifyPreExecuteSource(projection, {
        rootTaskId, taskRevision, fixturePid, bundleId, windowId, baselineTarget, requireFreshSource: true,
      });
      if (lastClassification.kind !== 'pending') return { ...lastClassification, projection };
    } catch (error) {
      lastError = error;
    }
    await new Promise(resolve => setTimeout(resolve, pollIntervalMs));
  }
  return { kind: 'timeout', projection: lastProjection, lastClassification, error: lastError };
}

function needsUserAction(reason, projection) {
  return { kind: 'needs_user_action', reason, projection };
}

/** Classify execution progress without granting approval to any action prompt. */
export function classifyExecutionProgress(projection, { rootTaskId, taskRevision } = {}) {
  if (typeof rootTaskId !== 'string' || rootTaskId.length === 0 || rootTaskId.length > 256
      || !Number.isSafeInteger(taskRevision) || taskRevision < 1) {
    return needsUserAction('invalid_expected_root_identity', projection ?? null);
  }
  if (!projection || typeof projection !== 'object' || Array.isArray(projection)) {
    return needsUserAction('invalid_projection', projection ?? null);
  }
  if (!Number.isSafeInteger(projection.revision) || projection.revision < 0) {
    return needsUserAction('invalid_projection_revision', projection);
  }
  const root = projection.root ?? null;
  if (!validRoot(root)) return needsUserAction('invalid_root', projection);
  if (root.root_task_id !== rootTaskId || root.task_revision !== taskRevision) {
    return { kind: 'root_identity_mismatch', root, projection };
  }
  if (projection.pending_confirmation !== null && projection.pending_confirmation !== undefined) {
    return needsUserAction('unexpected_confirmation', projection);
  }
  if (TERMINAL_ROOT_STATES.has(root.state) || root.state === 'suspended') {
    return { kind: 'terminal_root', root, projection };
  }
  if (projection.mode !== 'execute') return needsUserAction('mode_changed', projection);
  if (projection.stop_status !== 'not_stopped') return needsUserAction('stop_requested', projection);
  if (root.state === 'needs_input' || root.state === 'awaiting_confirmation') {
    return needsUserAction('confirmation_missing', projection);
  }
  return { kind: 'pending', projection };
}

/** Wait up to a bounded interval; any prompt ends the driver wait immediately. */
export async function waitForExecutionProgress({
  readProjection,
  rootTaskId,
  taskRevision,
  timeoutMs = 20 * 60 * 1000,
  pollIntervalMs = 100,
}) {
  const timeout = Number.isFinite(timeoutMs) ? Math.min(Math.max(timeoutMs, 0), 20 * 60 * 1000) : 20 * 60 * 1000;
  const interval = Number.isFinite(pollIntervalMs) ? Math.min(Math.max(pollIntervalMs, 1), 1_000) : 100;
  let lastProjection = null;
  let lastError = null;
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    try {
      const projection = await readProjection();
      lastProjection = projection;
      const classification = classifyExecutionProgress(projection, { rootTaskId, taskRevision });
      if (classification.kind !== 'pending') return { ...classification, projection };
    } catch (error) {
      lastError = error;
    }
    await new Promise(resolve => setTimeout(resolve, interval));
  }
  return { kind: 'timeout', projection: lastProjection, error: lastError };
}
