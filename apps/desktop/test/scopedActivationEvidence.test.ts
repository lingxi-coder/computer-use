import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { test } from 'node:test';
import { verifyScopedActivation } from '../../../scripts/mvp/scoped-activation-evidence.mjs';

const repo = resolve(import.meta.dirname, '../../..');
function evidence() {
  const source = JSON.parse(readFileSync(resolve(repo, 'contracts/desktop-v1/native-binding/scoped-window-synthetic.json'), 'utf8'));
  source.snapshot_id = 'post-activation-source';
  source.source_sequence += 1;
  source.observed_ms += 20;
  source.window_context.verified_source_sequence = source.source_sequence;
  source.window_context.verified_observed_ms = source.observed_ms;
  const tableScope = source.table_scope_search.requested_scope;
  const root = source.objects.find((object: { parent: unknown; window_id: string }) => object.parent === null
    && object.window_id === source.window_context.origin_window_id);
  // This invokes only the production bounded Source DTO compiler. It neither
  // reads the current OS window nor authorizes native input.
  const sourceProof = JSON.parse(execFileSync(resolve(repo, 'target/debug/compile_device_context'), [], {
    input: JSON.stringify({ snapshot: source, target: root.instance_ref, scope: { kind: 'window_root', table_scope: tableScope } }),
    encoding: 'utf8', timeout: 2000, maxBuffer: 64 * 1024,
  }));
  const context = sourceProof.device_context;
  const before = { continuity: sourceProof.continuity.kind, verified_source_sequence: source.source_sequence - 1,
    verified_observed_ms: source.observed_ms - 20, context_digest: sourceProof.continuity.context_digest };
  const identity = { ...source.window_context };
  delete identity.verified_source_sequence;
  delete identity.verified_observed_ms;
  const receipt = {
    kind: 'owned_row_guard_diagnostic', category: 'window-activation-submit-receipt', protocol_version: 1,
    response_kind: 'atomic_submit', session_epoch: source.session_epoch, request_id: 'submit-1', outcome: 'applied', reason: '',
    submitted: true, committed_ms: source.observed_ms - 10, returned_ms: source.observed_ms - 5,
    binding_digest: 'validated-native-request-digest', action: 'activate_window',
    correlation: { intent_id: 'intent-1', attempt_id: 'attempt-1', task_id: 'root-1', task_revision: 1, root_epoch: 1,
      session_epoch: source.session_epoch, request_id: 'submit-1' },
    window_activation: { table_scope: tableScope, stable_root: { app: context.app, window: context.window, root: context.root,
      source_identity: identity }, source_reads: { original: before, current: { ...before } } },
  };
  return { receipt, source, sourceProof, sessionEpoch: source.session_epoch,
    rootTask: { root_id: 'root-1' },
    attempt: { attempt_id: 'attempt-1', root_id: 'root-1', intent_id: 'intent-1', skill: 'ui.activate_window',
      task_revision: 1, root_epoch: 1, activation_seq: 1, state: 'settled' },
    effect: { predicate: 'window.active@1', status: 'satisfied', confirmation_scope: 'current_ui', root_task_id: 'root-1',
      attempt_id: 'attempt-1', intent_id: 'intent-1', evidence_group: [source.snapshot_id], observed_ms: source.observed_ms,
      input_span: { from_ms: receipt.committed_ms, to_ms: receipt.returned_ms } },
    target: { row: { table_scope: structuredClone(tableScope) } },
  };
}

test('owned activation evidence joins one native receipt to an independently compiled fresh Source read', () => {
  const result = verifyScopedActivation(evidence());
  assert.equal(result.ok, true);
  assert.equal(result.native_call_count, 'unknown');
  assert.equal(result.root_goal_complete, false);
  assert.equal(result.business_gate_eligible, false);
});

test('unknown, cross-intent, changed kernel, stale Source and invented counts cannot establish activation', () => {
  const mutations = [
    (value: ReturnType<typeof evidence>) => { value.receipt.outcome = 'unknown'; },
    (value: ReturnType<typeof evidence>) => { value.receipt.correlation.intent_id = 'another-intent'; },
    (value: ReturnType<typeof evidence>) => { value.receipt.window_activation.stable_root.source_identity.process = {
      ...value.receipt.window_activation.stable_root.source_identity.process, start_seconds: 1 }; },
    (value: ReturnType<typeof evidence>) => { value.source.source_sequence = value.receipt.window_activation.source_reads.current.verified_source_sequence; },
    (value: ReturnType<typeof evidence>) => { value.source.observed_ms = value.receipt.returned_ms; },
    (value: ReturnType<typeof evidence>) => { value.sourceProof.continuity.context_digest = 'bc1:' + '0'.repeat(64); },
    (value: ReturnType<typeof evidence>) => { value.target.row.table_scope.column_headers = ['another column']; },
    (value: ReturnType<typeof evidence>) => { value.source.complete = true; },
    (value: ReturnType<typeof evidence>) => { value.source.global_focus.window_id = { state: 'unavailable' }; },
    (value: ReturnType<typeof evidence>) => { value.source.global_focus.window_id = { state: 'known', value: 'another-window' }; },
    (value: ReturnType<typeof evidence>) => { value.source.global_focus.app = { state: 'known', value: { ...value.source.app, pid: 1 } }; },
    (value: ReturnType<typeof evidence>) => { Object.assign(value.receipt, { ax_call_count: 1 }); },
    (value: ReturnType<typeof evidence>) => { value.effect.intent_id = 'another-intent'; },
  ];
  for (const mutate of mutations) {
    const value = structuredClone(evidence());
    mutate(value);
    assert.equal(verifyScopedActivation(value).ok, false);
  }
});
