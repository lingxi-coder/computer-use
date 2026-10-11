import test from 'node:test';
import assert from 'node:assert/strict';

import {
  deriveRunEvidenceValidity, hasAcceptedModelAttribution, summarizeModelAttribution, toBusinessResultRun, validateDriverReceipt, validateEvaluatorResult,
} from './business-run-receipt.mjs';

const identity = { suite: 'fixture', task_id: 'SET-02', seed: 1 };
const evidencePath = 'evidence/final-read.json';
const auditPass = { trace: 'passed', scope: 'passed', current_ui: 'passed', stop_cleanup: 'passed' };

function driverReceipt(overrides = {}) {
  return {
    version: 1,
    ...identity,
    root_id: 'root-01',
    admission: { accepted: true, rejection_reason: null },
    status: 'unknown',
    system_reported_completed: false,
    cold_start: true,
    typed_ledger: {
      decisions: [{ decision_id: 'decision-01', source: 'teacher', selected_action_id: null, provider_response_id: 'provider-response-01', s2_request_ref: null, at_ms: 10 }],
      attempts: [{ attempt_id: 'attempt-01', decision_id: 'decision-01', state: 'settled', receipt_status: 'unknown' }],
      effects: [{ attempt_id: 'attempt-01', verdict: 'unknown', source_sequence: 2 }],
    },
    local_inference_revisions: [],
    evidence_refs: [{ path: evidencePath, sha256: 'a'.repeat(64), bytes: 10 }],
    model_call_attribution: {
      admitted_remote_call_rows: 1, actual_network_requests: 1,
      calls: [{ scope: 's1_decision', root_id: 'root-01', request_id: null, decision_id: 'decision-01', generation: 1,
        provider: 'provider', model: 'model', provider_response_id: 'provider-response-01', started_ms: 9, finished_ms: 11,
        outcome: 'proposal', usage: { input_tokens: 12, output_tokens: 3, cost_micros: 0 }, cost_usd: 0 }],
    },
    stop_cleanup: { stop_status: 'released', cleanup_status: 'clean', owned_processes_remaining: [], owned_directories_remaining: [] },
    final_read: { status: 'current_ui', source_sequence: 2, observed_at_ms: 12, evidence_ref: evidencePath },
    auditors: { ...auditPass },
    ...overrides,
  };
}

function evaluatorResult(overrides = {}) {
  return {
    version: 1,
    ...identity,
    verdict: 'unknown',
    evidence_validity: 'passed',
    auditors: { independent_truth: 'passed', trace: 'passed', current_ui: 'passed', scope: 'passed', stop_cleanup: 'passed' },
    reason_codes: [],
    ...overrides,
  };
}

test('receipt intake binds identity and requires a typed decision before completion', () => {
  const receipt = driverReceipt();
  assert.equal(validateDriverReceipt(receipt, identity), receipt);
  assert.throws(() => validateDriverReceipt(receipt, { ...identity, seed: 2 }), /identity does not match/);
  assert.throws(() => validateDriverReceipt({ ...receipt, root_id: null }, identity), /root_id/);
  assert.throws(() => validateDriverReceipt({ ...receipt, status: 'completed', system_reported_completed: true,
    typed_ledger: { ...receipt.typed_ledger, decisions: [] } }, identity), /completed Root must include at least one typed decision/);
});

test('S2 failure is retained as a scoped request row without fabricating a DecisionBook ID', () => {
  const receipt = driverReceipt({
    status: 'failed',
    typed_ledger: { decisions: [], attempts: [], effects: [] },
    model_call_attribution: {
      admitted_remote_call_rows: 1,
      actual_network_requests: null,
      calls: [{ scope: 's2_request', root_id: 'root-01', request_id: 'request-01', decision_id: null, generation: 1,
        provider: 'provider', model: 'model', provider_response_id: null, started_ms: 9, finished_ms: 11,
        outcome: 'unavailable', usage: { input_tokens: null, output_tokens: null, cost_micros: null }, cost_usd: null }],
    },
  });
  validateDriverReceipt(receipt, identity);
  assert.equal(receipt.typed_ledger.decisions.length, 0);
  assert.equal(receipt.model_call_attribution.admitted_remote_call_rows, 1);
  assert.equal(receipt.model_call_attribution.actual_network_requests, null);
  assert.equal(receipt.model_call_attribution.calls[0].decision_id, null);
  assert.equal(receipt.model_call_attribution.calls[0].cost_usd, null);
});

test('failed S1 remote calls count as admitted rows even with an unknown provider response ID', () => {
  const receipt = driverReceipt({
    status: 'failed',
    typed_ledger: {
      decisions: [{ decision_id: 'decision-01', source: 'teacher', selected_action_id: null, provider_response_id: null, s2_request_ref: null, at_ms: 10 }],
      attempts: [], effects: [],
    },
    model_call_attribution: {
      admitted_remote_call_rows: 1,
      actual_network_requests: 1,
      calls: [{ scope: 's1_decision', root_id: 'root-01', request_id: null, decision_id: 'decision-01', generation: 2,
        provider: 'provider', model: 'model', provider_response_id: null, started_ms: 9, finished_ms: 12,
        outcome: 'failed', usage: { input_tokens: 22, output_tokens: null, cost_micros: null }, cost_usd: null }],
    },
  });
  validateDriverReceipt(receipt, identity);
  assert.equal(receipt.model_call_attribution.admitted_remote_call_rows, 1);
  assert.equal(receipt.model_call_attribution.calls[0].provider_response_id, null);
  assert.equal(receipt.model_call_attribution.calls[0].cost_usd, null);
});

test('scoped call identities are unique and response IDs do not determine call counts', () => {
  const receipt = driverReceipt();
  assert.equal(validateDriverReceipt(receipt, identity).model_call_attribution.admitted_remote_call_rows, 1);
  const duplicate = { ...receipt.model_call_attribution.calls[0], generation: 1 };
  assert.throws(() => validateDriverReceipt({
    ...receipt,
    model_call_attribution: { ...receipt.model_call_attribution, calls: [receipt.model_call_attribution.calls[0], duplicate], admitted_remote_call_rows: 2 },
  }, identity), /duplicate model-call identity/);
  assert.throws(() => validateDriverReceipt({
    ...receipt,
    model_call_attribution: { ...receipt.model_call_attribution, admitted_remote_call_rows: 0 },
  }, identity), /row count does not match/);
});

test('S2 decisions correlate to their own Root request generation and outcome', () => {
  const receipt = driverReceipt({
    status: 'completed',
    system_reported_completed: true,
    typed_ledger: {
      decisions: [
        { decision_id: 'decision-s2-1', source: 's2', selected_action_id: 'action-1', provider_response_id: null,
          s2_request_ref: { root_id: 'root-01', request_id: 'request-01', generation: 1 }, at_ms: 10 },
        { decision_id: 'decision-s2-2', source: 's2', selected_action_id: 'action-2', provider_response_id: null,
          s2_request_ref: { root_id: 'root-01', request_id: 'request-02', generation: 2 }, at_ms: 12 },
      ],
      attempts: [], effects: [],
    },
    model_call_attribution: {
      admitted_remote_call_rows: 2,
      actual_network_requests: 2,
      calls: [
        { scope: 's2_request', root_id: 'root-01', request_id: 'request-01', decision_id: null, generation: 1,
          provider: 'provider', model: 'model', provider_response_id: null, started_ms: 8, finished_ms: 9,
          outcome: 'proposal', usage: { input_tokens: 1, output_tokens: 1, cost_micros: null }, cost_usd: null },
        { scope: 's2_request', root_id: 'root-01', request_id: 'request-02', decision_id: null, generation: 2,
          provider: 'provider', model: 'model', provider_response_id: null, started_ms: 11, finished_ms: 12,
          outcome: 'unavailable', usage: { input_tokens: null, output_tokens: null, cost_micros: null }, cost_usd: null },
      ],
    },
  });
  validateDriverReceipt(receipt, identity);
  assert.equal(hasAcceptedModelAttribution(receipt, 'm5-full'), false,
    'a proposal for one request cannot justify another S2 decision');
  const mismatchedGeneration = structuredClone(receipt);
  mismatchedGeneration.typed_ledger.decisions[1].s2_request_ref.generation = 1;
  assert.throws(() => validateDriverReceipt(mismatchedGeneration, identity), /no matching scoped request call/);
  const mismatchedRoot = structuredClone(receipt);
  mismatchedRoot.typed_ledger.decisions[0].s2_request_ref.root_id = 'different-root';
  assert.throws(() => validateDriverReceipt(mismatchedRoot, identity), /request Root differs from the admitted Root/);
  const sharedRequest = structuredClone(receipt);
  sharedRequest.model_call_attribution = {
    admitted_remote_call_rows: 1,
    actual_network_requests: 1,
    calls: [receipt.model_call_attribution.calls[0]],
  };
  sharedRequest.typed_ledger.decisions[1].s2_request_ref = structuredClone(sharedRequest.typed_ledger.decisions[0].s2_request_ref);
  validateDriverReceipt(sharedRequest, identity);
  assert.equal(hasAcceptedModelAttribution(sharedRequest, 'm5-full'), true,
    'multiple planned decisions may share one matching S2 request row');
});

test('promoted local S1 inference qualifies without a remote teacher call', () => {
  const receipt = driverReceipt({
    status: 'completed',
    system_reported_completed: true,
    typed_ledger: {
      decisions: [{ decision_id: 'decision-local-1', source: 's1', selected_action_id: 'action-local-1',
        provider_response_id: null, s2_request_ref: null, at_ms: 10 }],
      attempts: [], effects: [],
    },
    local_inference_revisions: [{ root_id: 'root-01', decision_id: 'decision-local-1', revision: 1,
      disposition: 'promoted', promoted_action_id: 'action-local-1', promotion_sequence: 2 }],
    model_call_attribution: { admitted_remote_call_rows: 0, actual_network_requests: null, calls: [] },
  });
  validateDriverReceipt(receipt, identity);
  assert.equal(hasAcceptedModelAttribution(receipt, 'm5-full'), true);
  assert.equal(hasAcceptedModelAttribution(receipt, 'm3-fixture-diagnostic'), false,
    'the teacher-specific diagnostic cannot be qualified by local S1 evidence');
  assert.equal(summarizeModelAttribution(receipt).zero_teacher_call_rows, true);
  assert.equal(summarizeModelAttribution(receipt).actual_zero_teacher_requests_confirmed, null);
  assert.equal(summarizeModelAttribution(receipt).attribution_mode, 'local_s1_inference');
  const unpromoted = structuredClone(receipt);
  unpromoted.local_inference_revisions[0].disposition = 'superseded';
  unpromoted.local_inference_revisions[0].promoted_action_id = null;
  unpromoted.local_inference_revisions[0].promotion_sequence = null;
  validateDriverReceipt(unpromoted, identity);
  assert.equal(hasAcceptedModelAttribution(unpromoted, 'm5-full'), false);

  const mixed = structuredClone(receipt);
  mixed.model_call_attribution = {
    admitted_remote_call_rows: 1,
    actual_network_requests: 1,
    calls: [{ scope: 's1_decision', root_id: 'root-01', request_id: null, decision_id: 'decision-local-1', generation: 1,
      provider: 'provider', model: 'model', provider_response_id: null, started_ms: 11, finished_ms: 12,
      outcome: 'failed', usage: { input_tokens: null, output_tokens: null, cost_micros: null }, cost_usd: null }],
  };
  validateDriverReceipt(mixed, identity);
  assert.equal(hasAcceptedModelAttribution(mixed, 'm5-full'), true,
    'a failed remote Teacher row does not suppress a separate promoted local decision');
  assert.equal(summarizeModelAttribution(mixed).zero_teacher_call_rows, false);
  assert.equal(summarizeModelAttribution(mixed).actual_zero_teacher_requests_confirmed, null);
  assert.equal(summarizeModelAttribution(mixed).attribution_mode, 'mixed');
});

test('unsupported admitted work remains denominator-visible as unknown', () => {
  const receipt = driverReceipt({ status: 'unsupported', system_reported_completed: false });
  validateDriverReceipt(receipt, identity);
  const row = toBusinessResultRun({ receipt, evaluatorResult: null, identity, knowledgeHasTaskWorkflows: false });
  assert.equal(row.accepted, true);
  assert.equal(row.status, 'unknown');
  assert.equal(row.status_detail, 'unsupported');
  assert.equal(row.system_reported_completed, false);
});

test('pre-admission rejection has no Root and remains a counted rejection', () => {
  const rejected = driverReceipt({
    root_id: null,
    admission: { accepted: false, rejection_reason: 'model_not_configured' },
    status: 'rejected',
    system_reported_completed: false,
    typed_ledger: { decisions: [], attempts: [], effects: [] },
    local_inference_revisions: [],
    evidence_refs: [],
    model_call_attribution: { admitted_remote_call_rows: 0, actual_network_requests: null, calls: [] },
    stop_cleanup: { stop_status: 'not_requested', cleanup_status: 'clean', owned_processes_remaining: [], owned_directories_remaining: [] },
    final_read: { status: 'not_read', source_sequence: null, observed_at_ms: null, evidence_ref: null },
    auditors: { trace: 'unknown', scope: 'unknown', current_ui: 'unknown', stop_cleanup: 'passed' },
  });
  validateDriverReceipt(rejected, identity);
  const row = toBusinessResultRun({ receipt: rejected, evaluatorResult: null, identity, knowledgeHasTaskWorkflows: false });
  assert.equal(row.accepted, false);
  assert.equal(row.rejection_reason, 'model_not_configured');
  assert.equal(row.system_reported_completed, false);
});

test('process-group cleanup alone cannot pass the evidence gate without product-resource coverage', () => {
  const receipt = driverReceipt();
  const evaluation = evaluatorResult();
  assert.equal(deriveRunEvidenceValidity({ receipt, evaluatorResult: evaluation, evidenceFilesVerified: true }), false);
  assert.equal(deriveRunEvidenceValidity({ receipt, evaluatorResult: evaluation, evidenceFilesVerified: false }), false);
  assert.equal(deriveRunEvidenceValidity({
    receipt, evaluatorResult: evaluation, evidenceFilesVerified: true,
    independentCleanupProbePassed: true,
  }), false);
  assert.equal(deriveRunEvidenceValidity({
    receipt, evaluatorResult: evaluation, evidenceFilesVerified: true,
    independentCleanupProbePassed: true, productOwnedResourceCoverageVerified: true,
  }), true);
  const failedAudit = evaluatorResult({ auditors: { ...evaluatorResult().auditors, trace: 'unknown' } });
  assert.equal(deriveRunEvidenceValidity({
    receipt, evaluatorResult: failedAudit, evidenceFilesVerified: true,
    independentCleanupProbePassed: true, productOwnedResourceCoverageVerified: true,
  }), false);
});

test('evaluator output is identity-bound and its verdict remains three-valued', () => {
  const result = evaluatorResult({ verdict: 'not_satisfied', reason_codes: ['condition_not_satisfied'] });
  assert.equal(validateEvaluatorResult(result, identity), result);
  assert.throws(() => validateEvaluatorResult({ ...result, seed: 2 }, identity), /identity/);
  assert.throws(() => validateEvaluatorResult({ ...result, verdict: 'success' }, identity), /verdict/);
});
