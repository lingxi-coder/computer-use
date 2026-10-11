import { isDeepStrictEqual as same } from 'node:util';

const keys = (value, expected) => Boolean(value && typeof value === 'object' && !Array.isArray(value)
  && same(Object.keys(value).sort(), [...expected].sort()));
const integer = value => Number.isSafeInteger(value) && value >= 0;

/** Verify one owned diagnostic. No provider, native query or input is performed. */
export function verifyScopedActivation({ receipt, rootTask, attempt, effect, source, sourceProof, target, sessionEpoch }) {
  const fail = reason => ({ ok: false, reason });
  const top = ['kind', 'category', 'protocol_version', 'response_kind', 'session_epoch', 'request_id',
    'outcome', 'reason', 'submitted', 'committed_ms', 'returned_ms', 'binding_digest', 'action', 'correlation', 'window_activation'];
  const correlation = ['intent_id', 'attempt_id', 'task_id', 'task_revision', 'root_epoch', 'session_epoch', 'request_id'];
  if (!keys(receipt, top) || !keys(receipt.correlation, correlation)
    || !keys(receipt.window_activation, ['table_scope', 'stable_root', 'source_reads'])) return fail('receipt_shape');
  const c = receipt.correlation, context = receipt.window_activation;
  if (!keys(context.stable_root, ['app', 'window', 'root', 'source_identity'])
    || !keys(context.source_reads, ['original', 'current'])) return fail('window_context_shape');
  const stable = context.stable_root, original = context.source_reads.original, current = context.source_reads.current;
  const readKeys = ['continuity', 'verified_source_sequence', 'verified_observed_ms', 'context_digest'];
  if (!keys(original, readKeys) || !keys(current, readKeys)
    || !keys(stable.source_identity, ['source_session_id', 'session_epoch', 'process', 'origin_window_id', 'origin_window_frame'])) {
    return fail('source_identity_shape');
  }
  if (!rootTask || !attempt || !effect || !target?.row || !source || !sourceProof) return fail('missing_evidence');
  if (receipt.kind !== 'owned_row_guard_diagnostic' || receipt.category !== 'window-activation-submit-receipt'
    || receipt.protocol_version !== 1 || receipt.response_kind !== 'atomic_submit' || receipt.action !== 'activate_window'
    || receipt.outcome !== 'applied' || receipt.submitted !== true
    || receipt.request_id !== c.request_id || receipt.session_epoch !== sessionEpoch || c.session_epoch !== sessionEpoch
    || c.task_id !== rootTask.root_id || c.attempt_id !== attempt.attempt_id || c.intent_id !== attempt.intent_id
    || c.task_revision !== attempt.task_revision || c.root_epoch !== attempt.root_epoch
    || attempt.root_id !== rootTask.root_id || attempt.skill !== 'ui.activate_window' || attempt.state !== 'settled'
    || !integer(attempt.activation_seq) || attempt.activation_seq === 0
    || !integer(receipt.committed_ms) || !integer(receipt.returned_ms) || receipt.returned_ms < receipt.committed_ms
    || typeof receipt.binding_digest !== 'string' || !receipt.binding_digest) return fail('receipt_correlation_or_outcome');
  if (!same(context.table_scope, target.row.table_scope)
    || !same(context.table_scope, source.table_scope_search?.requested_scope)
    || source.table_scope_search?.state?.state !== 'complete' || source.table_scope_search?.candidates?.length !== 1
    || !['notification_covered', 'read_scoped'].includes(current.continuity) || original.continuity !== current.continuity
    || !/^bc1:[0-9a-f]{64}$/.test(current.context_digest) || original.context_digest !== current.context_digest
    || !integer(original.verified_source_sequence) || original.verified_source_sequence === 0
    || !integer(current.verified_source_sequence) || current.verified_source_sequence < original.verified_source_sequence
    || !integer(original.verified_observed_ms) || !integer(current.verified_observed_ms)
    || current.verified_observed_ms < original.verified_observed_ms) return fail('scope_or_input_source');
  const identity = stable.source_identity, sourceContext = source.window_context;
  if (!sourceContext || source.complete !== false || source.session_epoch !== sessionEpoch
    || sourceContext.session_epoch !== sessionEpoch || identity.session_epoch !== sessionEpoch
    || source.app?.bundle_id !== stable.app?.bundle_id || source.app?.pid !== stable.app?.pid
    || identity.process?.pid !== stable.app.pid || identity.origin_window_id !== stable.window?.window_id
    || source.global_focus?.app?.state !== 'known' || !same(source.global_focus.app.value, source.app)
    || source.global_focus?.window_id?.state !== 'known' || source.global_focus.window_id.value !== identity.origin_window_id
    || sourceContext.origin_window_id !== identity.origin_window_id
    || sourceContext.source_session_id !== identity.source_session_id || !same(sourceContext.process, identity.process)
    || !same(sourceContext.origin_window_frame, identity.origin_window_frame)
    || stable.root?.role !== 'AXWindow' || stable.root?.subrole !== 'AXStandardWindow'
    || !integer(source.source_sequence) || source.source_sequence <= current.verified_source_sequence
    || sourceContext.verified_source_sequence !== source.source_sequence
    || sourceContext.verified_observed_ms !== source.observed_ms || !integer(source.observed_ms)
    || source.observed_ms <= receipt.returned_ms) return fail('fresh_same_window_source');
  if (sourceProof.continuity?.scope?.kind !== 'window_root'
    || !same(sourceProof.continuity.scope.table_scope, context.table_scope)
    || sourceProof.continuity.context_digest !== current.context_digest
    || sourceProof.continuity.source_sequence !== source.source_sequence
    || sourceProof.device_context?.scope !== 'window_root_v1'
    || !same(sourceProof.device_context.app, stable.app)
    || !same(sourceProof.device_context.window, stable.window)
    || !same(sourceProof.device_context.root, stable.root)
    || !same(sourceProof.device_context.source, sourceContext)) return fail('independent_source_derivation');
  if (effect.predicate !== 'window.active@1' || effect.status !== 'satisfied' || effect.confirmation_scope !== 'current_ui'
    || effect.root_task_id !== rootTask.root_id || effect.attempt_id !== attempt.attempt_id || effect.intent_id !== c.intent_id
    || !Array.isArray(effect.evidence_group) || !effect.evidence_group.includes(source.snapshot_id)
    || effect.observed_ms !== source.observed_ms || !integer(effect.input_span?.from_ms)
    || !integer(effect.input_span?.to_ms) || effect.input_span.from_ms > effect.input_span.to_ms
    || source.observed_ms <= effect.input_span.to_ms) return fail('effect_correlation_or_freshness');
  return { ok: true, intent_id: c.intent_id, attempt_id: attempt.attempt_id, request_id: c.request_id,
    source_snapshot_id: source.snapshot_id, source_sequence: source.source_sequence,
    committed_ms: receipt.committed_ms, returned_ms: receipt.returned_ms,
    observed_ms: source.observed_ms, native_primitive: 'activate_window', native_call_count: 'unknown',
    root_goal_complete: false, business_gate_eligible: false };
}
