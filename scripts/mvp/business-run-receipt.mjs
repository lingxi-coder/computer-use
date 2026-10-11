import assert from 'node:assert/strict';

const HASH = /^[a-f0-9]{64}$/;
const DRIVER_STATUSES = new Set([
  'completed', 'failed', 'timeout', 'unknown', 'unverified', 'cancelled', 'unsupported', 'rejected',
]);
const BUSINESS_STATUSES = new Set(['completed', 'failed', 'timeout', 'unknown', 'unverified', 'cancelled']);
const VERDICTS = new Set(['satisfied', 'not_satisfied', 'unknown']);
const AUDIT_STATUSES = new Set(['passed', 'failed', 'unknown']);
const DECISION_SOURCES = new Set(['teacher', 's1', 's2', 'reference', 'rule', 'none', 'unknown']);
const S2_OUTCOMES = new Set(['proposal', 'unavailable', 'missing', 'conflict', 'invalid', 'quarantined', 'cancelled', 'expired']);
const S1_OUTCOMES = new Set(['proposal', 'failed', 'unavailable', 'quarantined', 'cancelled', 'expired']);
const MAX_ARRAY_ITEMS = 20_000;
const MAX_TEXT_BYTES = 16_384;

function exactKeys(value, keys, label) {
  assert.ok(isRecord(value), `${label} must be an object`);
  assert.deepEqual(Object.keys(value).sort(), [...keys].sort(), `${label} has an unexpected shape`);
}

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function text(value, label, { nullable = false } = {}) {
  if (nullable && value === null) return null;
  assert.equal(typeof value, 'string', `${label} must be a string`);
  assert.ok(value.trim().length > 0 && Buffer.byteLength(value, 'utf8') <= MAX_TEXT_BYTES, `${label} is invalid`);
  return value;
}

function nullableText(value, label) {
  return value === null ? null : text(value, label);
}

function integer(value, label, { minimum = 0, maximum = Number.MAX_SAFE_INTEGER } = {}) {
  assert.ok(Number.isSafeInteger(value) && value >= minimum && value <= maximum, `${label} is invalid`);
  return value;
}

function finiteNonnegative(value, label) {
  assert.ok(typeof value === 'number' && Number.isFinite(value) && value >= 0, `${label} is invalid`);
  return value;
}

function auditStatus(value, label) {
  assert.ok(AUDIT_STATUSES.has(value), `${label} is invalid`);
  return value;
}

function validateIdentity(value, label = 'identity') {
  exactKeys(value, ['suite', 'task_id', 'seed'], label);
  assert.ok(value.suite === 'fixture' || value.suite === 'textedit', `${label} suite is invalid`);
  text(value.task_id, `${label} task_id`);
  integer(value.seed, `${label} seed`, { minimum: 1, maximum: 5 });
  if (value.suite === 'textedit') assert.equal(value.seed, 1, `${label} TextEdit seed must be 1`);
  return value;
}

function validateEvidenceRefs(value) {
  assert.ok(Array.isArray(value) && value.length <= 256, 'evidence_refs is invalid');
  const paths = new Set();
  for (const [index, reference] of value.entries()) {
    exactKeys(reference, ['path', 'sha256', 'bytes'], `evidence_refs[${index}]`);
    const path = text(reference.path, `evidence_refs[${index}].path`);
    assert.ok(!path.startsWith('/') && !path.includes('\\')
      && path.split('/').every(part => part && part !== '.' && part !== '..'),
    `evidence_refs[${index}].path must be a safe relative path`);
    assert.ok(!paths.has(path), `duplicate evidence reference: ${path}`);
    paths.add(path);
    assert.ok(typeof reference.sha256 === 'string' && HASH.test(reference.sha256), `evidence_refs[${index}].sha256 is invalid`);
    integer(reference.bytes, `evidence_refs[${index}].bytes`, { minimum: 1, maximum: 128 * 1024 * 1024 });
  }
  return paths;
}

function validateTypedLedger(value, requireDecision) {
  exactKeys(value, ['decisions', 'attempts', 'effects'], 'typed_ledger');
  for (const key of ['decisions', 'attempts', 'effects']) {
    assert.ok(Array.isArray(value[key]) && value[key].length <= MAX_ARRAY_ITEMS, `typed_ledger.${key} is invalid`);
  }
  if (requireDecision) assert.ok(value.decisions.length > 0, 'a completed Root must include at least one typed decision');
  const decisionIds = new Set();
  for (const [index, decision] of value.decisions.entries()) {
    exactKeys(decision, ['decision_id', 'source', 'selected_action_id', 'provider_response_id', 's2_request_ref', 'at_ms'], `typed_ledger.decisions[${index}]`);
    const id = text(decision.decision_id, `typed_ledger.decisions[${index}].decision_id`);
    assert.ok(!decisionIds.has(id), `duplicate decision id: ${id}`);
    decisionIds.add(id);
    assert.ok(DECISION_SOURCES.has(decision.source), `typed_ledger.decisions[${index}].source is invalid`);
    nullableText(decision.selected_action_id, `typed_ledger.decisions[${index}].selected_action_id`);
    nullableText(decision.provider_response_id, `typed_ledger.decisions[${index}].provider_response_id`);
    if (decision.source === 's2') {
      exactKeys(decision.s2_request_ref, ['root_id', 'request_id', 'generation'], `typed_ledger.decisions[${index}].s2_request_ref`);
      text(decision.s2_request_ref.root_id, `typed_ledger.decisions[${index}].s2_request_ref.root_id`);
      text(decision.s2_request_ref.request_id, `typed_ledger.decisions[${index}].s2_request_ref.request_id`);
      integer(decision.s2_request_ref.generation, `typed_ledger.decisions[${index}].s2_request_ref.generation`, { minimum: 1 });
    } else {
      assert.equal(decision.s2_request_ref, null, `non-S2 decision ${id} cannot claim an S2 request reference`);
    }
    integer(decision.at_ms, `typed_ledger.decisions[${index}].at_ms`);
  }

  const attemptIds = new Set();
  for (const [index, attempt] of value.attempts.entries()) {
    exactKeys(attempt, ['attempt_id', 'decision_id', 'state', 'receipt_status'], `typed_ledger.attempts[${index}]`);
    const id = text(attempt.attempt_id, `typed_ledger.attempts[${index}].attempt_id`);
    assert.ok(!attemptIds.has(id), `duplicate attempt id: ${id}`);
    attemptIds.add(id);
    nullableText(attempt.decision_id, `typed_ledger.attempts[${index}].decision_id`);
    if (attempt.decision_id !== null) assert.ok(decisionIds.has(attempt.decision_id), `attempt ${id} references an unknown decision`);
    text(attempt.state, `typed_ledger.attempts[${index}].state`);
    assert.ok(['completed', 'rejected', 'unknown', 'not_applicable'].includes(attempt.receipt_status),
      `typed_ledger.attempts[${index}].receipt_status is invalid`);
  }

  for (const [index, effect] of value.effects.entries()) {
    exactKeys(effect, ['attempt_id', 'verdict', 'source_sequence'], `typed_ledger.effects[${index}]`);
    text(effect.attempt_id, `typed_ledger.effects[${index}].attempt_id`);
    assert.ok(attemptIds.has(effect.attempt_id), `typed_ledger.effects[${index}] references an unknown attempt`);
    assert.ok(VERDICTS.has(effect.verdict), `typed_ledger.effects[${index}].verdict is invalid`);
    if (effect.source_sequence !== null) integer(effect.source_sequence, `typed_ledger.effects[${index}].source_sequence`, { minimum: 1 });
  }
}

function validateLocalInferenceRevisions(value, rootId, decisions) {
  assert.ok(Array.isArray(value) && value.length <= 10_000, 'local_inference_revisions is invalid');
  const revisionKeys = new Set();
  const promotedDecisions = new Set();
  for (const [index, revision] of value.entries()) {
    exactKeys(revision, [
      'root_id', 'decision_id', 'revision', 'disposition', 'promoted_action_id', 'promotion_sequence',
    ], `local_inference_revisions[${index}]`);
    assert.equal(text(revision.root_id, `local_inference_revisions[${index}].root_id`), rootId,
      `local inference revision ${index} references a different Root`);
    const decisionId = text(revision.decision_id, `local_inference_revisions[${index}].decision_id`);
    const decision = decisions.get(decisionId);
    assert.ok(decision, `local inference revision ${index} references an unknown decision`);
    assert.equal(decision.source, 's1', `local inference revision ${index} must belong to a local S1 decision`);
    const revisionNumber = integer(revision.revision, `local_inference_revisions[${index}].revision`, { minimum: 1 });
    const key = JSON.stringify([revision.root_id, decisionId, revisionNumber]);
    assert.ok(!revisionKeys.has(key), `duplicate local inference revision identity at row ${index}`);
    revisionKeys.add(key);
    assert.ok(['promoted', 'superseded', 'rejected'].includes(revision.disposition),
      `local inference revision ${index} disposition is invalid`);
    if (revision.disposition === 'promoted') {
      assert.ok(!promotedDecisions.has(decisionId), `decision ${decisionId} has multiple promoted local revisions`);
      promotedDecisions.add(decisionId);
      text(revision.promoted_action_id, `local_inference_revisions[${index}].promoted_action_id`);
      assert.equal(revision.promoted_action_id, decision.selected_action_id,
        `local inference revision ${index} promoted a different action than its Decision`);
      integer(revision.promotion_sequence, `local_inference_revisions[${index}].promotion_sequence`, { minimum: 1 });
    } else {
      assert.equal(revision.promoted_action_id, null, `non-promoted local revision ${index} cannot claim an action`);
      assert.equal(revision.promotion_sequence, null, `non-promoted local revision ${index} cannot claim promotion`);
    }
  }
}

function validateModelAttribution(value, rootId) {
  exactKeys(value, ['admitted_remote_call_rows', 'actual_network_requests', 'calls'], 'model_call_attribution');
  integer(value.admitted_remote_call_rows, 'model_call_attribution.admitted_remote_call_rows', { maximum: 10_000 });
  if (value.actual_network_requests !== null) {
    integer(value.actual_network_requests, 'model_call_attribution.actual_network_requests', { maximum: 1_000_000 });
  }
  assert.ok(Array.isArray(value.calls) && value.calls.length <= 10_000, 'model_call_attribution.calls is invalid');
  assert.equal(value.calls.length, value.admitted_remote_call_rows, 'admitted remote call row count does not match call records');
  const scopedIdentities = new Set();
  for (const [index, call] of value.calls.entries()) {
    exactKeys(call, [
      'scope', 'root_id', 'request_id', 'decision_id', 'generation', 'provider', 'model',
      'provider_response_id', 'started_ms', 'finished_ms', 'outcome', 'usage', 'cost_usd',
    ], `model_call_attribution.calls[${index}]`);
    assert.ok(call.scope === 's2_request' || call.scope === 's1_decision', `model_call_attribution.calls[${index}].scope is invalid`);
    assert.equal(text(call.root_id, `model_call_attribution.calls[${index}].root_id`), rootId,
      `model call ${index} references a different Root`);
    const requestId = nullableText(call.request_id, `model_call_attribution.calls[${index}].request_id`);
    const decisionId = nullableText(call.decision_id, `model_call_attribution.calls[${index}].decision_id`);
    integer(call.generation, `model_call_attribution.calls[${index}].generation`, { minimum: 1 });
    if (call.scope === 's2_request') {
      text(requestId, `model_call_attribution.calls[${index}].request_id`);
      assert.equal(decisionId, null, `S2 request call ${index} cannot claim a Decision ID`);
    } else {
      text(decisionId, `model_call_attribution.calls[${index}].decision_id`);
      assert.equal(requestId, null, `S1 decision call ${index} cannot claim a request ID`);
    }
    const scopedIdentity = JSON.stringify([call.scope, call.root_id, requestId, decisionId, call.generation]);
    assert.ok(!scopedIdentities.has(scopedIdentity), `duplicate model-call identity at row ${index}`);
    scopedIdentities.add(scopedIdentity);
    text(call.provider, `model_call_attribution.calls[${index}].provider`);
    text(call.model, `model_call_attribution.calls[${index}].model`);
    nullableText(call.provider_response_id, `model_call_attribution.calls[${index}].provider_response_id`);
    const started = integer(call.started_ms, `model_call_attribution.calls[${index}].started_ms`);
    if (call.finished_ms !== null) integer(call.finished_ms, `model_call_attribution.calls[${index}].finished_ms`, { minimum: started });
    if (call.scope === 's2_request') {
      if (call.outcome !== null) assert.ok(S2_OUTCOMES.has(call.outcome), `model-call outcome ${index} is invalid for S2`);
    } else if (call.outcome !== null) {
      assert.ok(S1_OUTCOMES.has(call.outcome), `model-call outcome ${index} is invalid for S1`);
    }
    assert.equal(call.finished_ms === null, call.outcome === null, `model call ${index} must keep open finish time and outcome together`);
    exactKeys(call.usage, ['input_tokens', 'output_tokens', 'cost_micros'], `model_call_attribution.calls[${index}].usage`);
    for (const key of ['input_tokens', 'output_tokens', 'cost_micros']) {
      if (call.usage[key] !== null) integer(call.usage[key], `model_call_attribution.calls[${index}].usage.${key}`);
    }
    if (call.cost_usd !== null) finiteNonnegative(call.cost_usd, `model_call_attribution.calls[${index}].cost_usd`);
  }
}

function validateS2DecisionCallCorrelations(receipt) {
  const calls = new Map(receipt.model_call_attribution.calls
    .filter(call => call.scope === 's2_request')
    .map(call => [JSON.stringify([call.root_id, call.request_id, call.generation]), call]));
  for (const decision of receipt.typed_ledger.decisions) {
    if (decision.source !== 's2') continue;
    const ref = decision.s2_request_ref;
    assert.equal(ref.root_id, receipt.root_id, `S2 decision ${decision.decision_id} request Root differs from the admitted Root`);
    const key = JSON.stringify([ref.root_id, ref.request_id, ref.generation]);
    assert.ok(calls.has(key), `S2 decision ${decision.decision_id} has no matching scoped request call`);
  }
}

export function hasAcceptedModelAttribution(receipt, scope) {
  if (!receipt?.admission.accepted || !receipt.typed_ledger.decisions.length) return false;
  const sources = receipt.typed_ledger.decisions.map(decision => decision.source);
  if (sources.some(source => !['teacher', 's1', 's2'].includes(source))) return false;
  if (scope === 'm3-fixture-diagnostic' && !sources.includes('teacher')) return false;
  const calls = receipt.model_call_attribution.calls;
  const successfulModelCall = call => Boolean(call?.provider && call.model && call.outcome === 'proposal');
  const s1ProposalDecisionIds = new Set(calls
    .filter(call => call.scope === 's1_decision' && successfulModelCall(call))
    .map(call => call.decision_id));
  const s2CallByIdentity = new Map(calls
    .filter(call => call.scope === 's2_request')
    .map(call => [JSON.stringify([call.root_id, call.request_id, call.generation]), call]));
  const promotedLocalDecisionIds = new Set(receipt.local_inference_revisions
    .filter(revision => revision.disposition === 'promoted')
    .map(revision => revision.decision_id));
  return receipt.typed_ledger.decisions.every(decision => {
    if (decision.source === 'teacher') return s1ProposalDecisionIds.has(decision.decision_id);
    if (decision.source === 's1') return s1ProposalDecisionIds.has(decision.decision_id)
      || promotedLocalDecisionIds.has(decision.decision_id);
    if (decision.source === 's2') {
      const ref = decision.s2_request_ref;
      const call = s2CallByIdentity.get(JSON.stringify([ref.root_id, ref.request_id, ref.generation]));
      return successfulModelCall(call);
    }
    return false;
  });
}

export function summarizeModelAttribution(receipt) {
  if (!receipt?.admission.accepted) return null;
  const calls = receipt.model_call_attribution.calls;
  const teacherCallRows = calls.filter(call => call.scope === 's1_decision').length;
  const localS1PromotedDecisionCount = new Set(receipt.local_inference_revisions
    .filter(revision => revision.disposition === 'promoted')
    .map(revision => revision.decision_id)).size;
  const s2RequestRows = calls.filter(call => call.scope === 's2_request').length;
  const attributionChannels = [];
  if (teacherCallRows > 0) attributionChannels.push('remote_teacher');
  if (localS1PromotedDecisionCount > 0) attributionChannels.push('local_s1_inference');
  if (s2RequestRows > 0) attributionChannels.push('s2_planning');
  return {
    admitted_remote_call_rows: receipt.model_call_attribution.admitted_remote_call_rows,
    actual_network_requests: receipt.model_call_attribution.actual_network_requests,
    teacher_call_rows: teacherCallRows,
    zero_teacher_call_rows: teacherCallRows === 0,
    actual_zero_teacher_requests_confirmed: null,
    local_s1_promoted_decisions: localS1PromotedDecisionCount,
    s2_request_rows: s2RequestRows,
    attribution_channels: attributionChannels,
    attribution_mode: attributionChannels.length === 0 ? 'none' : attributionChannels.length === 1 ? attributionChannels[0] : 'mixed',
  };
}

/** Validate the product driver's post-run receipt. This schema contains no evaluator truth. */
export function validateDriverReceipt(receipt, expectedIdentity) {
  validateIdentity(expectedIdentity, 'expected identity');
  exactKeys(receipt, [
    'version', 'suite', 'task_id', 'seed', 'root_id', 'admission', 'status', 'system_reported_completed',
    'cold_start', 'typed_ledger', 'local_inference_revisions', 'evidence_refs', 'model_call_attribution',
    'stop_cleanup', 'final_read', 'auditors',
  ], 'driver receipt');
  assert.equal(receipt.version, 1, 'driver receipt version must be 1');
  const identity = validateIdentity({ suite: receipt.suite, task_id: receipt.task_id, seed: receipt.seed }, 'driver receipt identity');
  assert.deepEqual(identity, expectedIdentity, 'driver receipt identity does not match the frozen task');
  exactKeys(receipt.admission, ['accepted', 'rejection_reason'], 'admission');
  assert.equal(typeof receipt.admission.accepted, 'boolean', 'admission.accepted must be Boolean');
  nullableText(receipt.admission.rejection_reason, 'admission.rejection_reason');
  assert.equal(typeof receipt.system_reported_completed, 'boolean', 'system_reported_completed must be Boolean');
  assert.equal(typeof receipt.cold_start, 'boolean', 'cold_start must be Boolean');
  assert.ok(DRIVER_STATUSES.has(receipt.status), 'driver receipt status is invalid');
  assert.equal(receipt.system_reported_completed, receipt.status === 'completed', 'system completion contradicts driver status');

  if (receipt.admission.accepted) {
    text(receipt.root_id, 'root_id');
    assert.notEqual(receipt.status, 'rejected', 'an admitted task cannot have rejected status');
    assert.equal(receipt.admission.rejection_reason, null, 'an admitted task cannot include a rejection reason');
  } else {
    assert.equal(receipt.root_id, null, 'a pre-admission rejection cannot claim a Root ID');
    assert.equal(receipt.status, 'rejected', 'a pre-admission rejection must use rejected status');
    assert.equal(receipt.system_reported_completed, false, 'a pre-admission rejection cannot claim completion');
    text(receipt.admission.rejection_reason, 'admission.rejection_reason');
  }

  validateTypedLedger(receipt.typed_ledger, receipt.admission.accepted && receipt.status === 'completed');
  const evidencePaths = validateEvidenceRefs(receipt.evidence_refs);
  const decisionsById = new Map(receipt.typed_ledger.decisions.map(decision => [decision.decision_id, decision]));
  validateLocalInferenceRevisions(receipt.local_inference_revisions, receipt.root_id, decisionsById);
  validateModelAttribution(receipt.model_call_attribution, receipt.root_id);
  validateS2DecisionCallCorrelations(receipt);

  exactKeys(receipt.stop_cleanup, [
    'stop_status', 'cleanup_status', 'owned_processes_remaining', 'owned_directories_remaining',
  ], 'stop_cleanup');
  assert.ok(['released', 'not_requested', 'unknown', 'failed'].includes(receipt.stop_cleanup.stop_status), 'stop status is invalid');
  assert.ok(['clean', 'incomplete', 'unknown'].includes(receipt.stop_cleanup.cleanup_status), 'cleanup status is invalid');
  assert.ok(Array.isArray(receipt.stop_cleanup.owned_processes_remaining)
    && receipt.stop_cleanup.owned_processes_remaining.length <= 256, 'remaining process list is invalid');
  for (const [index, pid] of receipt.stop_cleanup.owned_processes_remaining.entries()) {
    integer(pid, `stop_cleanup.owned_processes_remaining[${index}]`, { minimum: 1, maximum: 0xffff_ffff });
  }
  assert.ok(Array.isArray(receipt.stop_cleanup.owned_directories_remaining)
    && receipt.stop_cleanup.owned_directories_remaining.length <= 256, 'remaining directory list is invalid');
  for (const [index, path] of receipt.stop_cleanup.owned_directories_remaining.entries()) {
    text(path, `stop_cleanup.owned_directories_remaining[${index}]`);
  }

  exactKeys(receipt.final_read, ['status', 'source_sequence', 'observed_at_ms', 'evidence_ref'], 'final_read');
  assert.ok(['current_ui', 'unknown', 'not_read'].includes(receipt.final_read.status), 'final_read.status is invalid');
  if (receipt.final_read.status === 'current_ui') {
    integer(receipt.final_read.source_sequence, 'final_read.source_sequence', { minimum: 1 });
    integer(receipt.final_read.observed_at_ms, 'final_read.observed_at_ms');
    text(receipt.final_read.evidence_ref, 'final_read.evidence_ref');
    assert.ok(evidencePaths.has(receipt.final_read.evidence_ref), 'final read must reference a declared evidence file');
  } else {
    assert.equal(receipt.final_read.source_sequence, null, 'non-current final read cannot claim a Source sequence');
    assert.equal(receipt.final_read.observed_at_ms, null, 'non-current final read cannot claim an observation time');
    assert.equal(receipt.final_read.evidence_ref, null, 'non-current final read cannot reference evidence');
  }

  exactKeys(receipt.auditors, ['trace', 'scope', 'current_ui', 'stop_cleanup'], 'auditors');
  for (const [key, value] of Object.entries(receipt.auditors)) auditStatus(value, `auditors.${key}`);
  return receipt;
}

/** Validate a separate evaluator process result produced only after all driver children have exited. */
export function validateEvaluatorResult(result, expectedIdentity) {
  validateIdentity(expectedIdentity, 'expected identity');
  exactKeys(result, ['version', 'suite', 'task_id', 'seed', 'verdict', 'evidence_validity', 'auditors', 'reason_codes'], 'evaluator result');
  assert.equal(result.version, 1, 'evaluator result version must be 1');
  assert.deepEqual({ suite: result.suite, task_id: result.task_id, seed: result.seed }, expectedIdentity,
    'evaluator result identity does not match the frozen task');
  assert.ok(VERDICTS.has(result.verdict), 'evaluator verdict is invalid');
  auditStatus(result.evidence_validity, 'evidence_validity');
  exactKeys(result.auditors, ['independent_truth', 'trace', 'current_ui', 'scope', 'stop_cleanup'], 'evaluator auditors');
  for (const [key, value] of Object.entries(result.auditors)) auditStatus(value, `evaluator auditors.${key}`);
  assert.ok(Array.isArray(result.reason_codes) && result.reason_codes.length <= 128, 'evaluator reason_codes is invalid');
  for (const [index, code] of result.reason_codes.entries()) text(code, `evaluator reason_codes[${index}]`);
  return result;
}

export function deriveRunEvidenceValidity({
  receipt,
  evaluatorResult,
  evidenceFilesVerified,
  independentCleanupProbePassed = false,
  productOwnedResourceCoverageVerified = false,
}) {
  if (!receipt || !receipt.admission.accepted || !evaluatorResult || evidenceFilesVerified !== true
    || independentCleanupProbePassed !== true || productOwnedResourceCoverageVerified !== true) return false;
  const driverAuditorsPass = Object.values(receipt.auditors).every(status => status === 'passed');
  const evaluatorAuditorsPass = Object.values(evaluatorResult.auditors).every(status => status === 'passed');
  const cleanStop = receipt.stop_cleanup.cleanup_status === 'clean'
    && receipt.stop_cleanup.owned_processes_remaining.length === 0
    && receipt.stop_cleanup.owned_directories_remaining.length === 0
    && ['released', 'not_requested'].includes(receipt.stop_cleanup.stop_status);
  return driverAuditorsPass && evaluatorAuditorsPass && cleanStop
    && receipt.final_read.status === 'current_ui'
    && evaluatorResult.evidence_validity === 'passed';
}

export function toBusinessResultRun({ receipt, evaluatorResult, identity, knowledgeHasTaskWorkflows }) {
  validateIdentity(identity);
  assert.equal(typeof knowledgeHasTaskWorkflows, 'boolean', 'knowledgeHasTaskWorkflows must be Boolean');
  if (!receipt) {
    return {
      task_id: identity.task_id,
      seed: identity.seed,
      accepted: true,
      status: 'unknown',
      status_detail: 'driver_receipt_missing_or_invalid',
      system_reported_completed: false,
      independent_verdict: 'unknown',
      cold_start: false,
      knowledge_has_task_workflows: knowledgeHasTaskWorkflows,
    };
  }
  validateDriverReceipt(receipt, identity);
  if (!receipt.admission.accepted) {
    return {
      task_id: identity.task_id,
      seed: identity.seed,
      accepted: false,
      system_reported_completed: false,
      rejection_reason: receipt.admission.rejection_reason,
    };
  }
  const unsupported = receipt.status === 'unsupported';
  const status = unsupported ? 'unknown' : receipt.status;
  assert.ok(BUSINESS_STATUSES.has(status), 'driver status cannot be projected into the business aggregator');
  return {
    task_id: identity.task_id,
    seed: identity.seed,
    accepted: true,
    status,
    ...(unsupported ? { status_detail: 'unsupported' } : {}),
    system_reported_completed: receipt.system_reported_completed,
    independent_verdict: evaluatorResult?.verdict ?? 'unknown',
    cold_start: receipt.cold_start,
    knowledge_has_task_workflows: knowledgeHasTaskWorkflows,
  };
}
