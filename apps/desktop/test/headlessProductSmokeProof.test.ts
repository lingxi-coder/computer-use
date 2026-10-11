import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import { test } from 'node:test';

const require = createRequire(import.meta.url);
const proof = require('./manual/headlessProductSmokeProof.cjs') as {
  validateScope(value: unknown): unknown;
  validateProjection(value: unknown): Record<string, any>;
  validateOwnerRecord(value: unknown, realpathSync: typeof import('node:fs').realpathSync): unknown;
  hasCompleteOwnerPids(value: unknown): boolean;
  validateSnapshot(value: unknown): unknown;
  validateExecution(value: unknown): unknown;
  sameScope(left: Record<string, any>, right: Record<string, any>): boolean;
  contextIdentity(value: Record<string, any>): Record<string, unknown>;
  validateLedger(ledger: unknown, firstRootId: string, secondRootId: string, executions: any[], sourceReads: any[]): unknown;
};

function scope() {
  return {
    scope_id: '11111111-1111-4111-8111-111111111111', generation: 1,
    clock_domain: 'headless/1/wall', issued_ms: 100,
    context: {
      browser_session_id: 'browser-1', browser_context_id: 'context-1', page_target_id: 'target-1',
      cdp_session_id: 'cdp-1', frame_id: 'frame-1', document_loader_id: 'document-1',
      fixture_file_url: 'file:///tmp/owned/fixtures/headless/readonly-page.html', session_epoch: 1,
    },
  };
}

function execution() {
  const currentScope = scope();
  const binding = {
    ax_node_id: 'ax-checkbox', backend_dom_node_id: 7, checked: false, clock_domain: currentScope.clock_domain,
    context: currentScope.context, disabled: 'false', name: 'Background option', observed_ms: 140,
    read_finished_ms: 150, role: 'checkbox', snapshot_id: 'snapshot-action', source_sequence: 2,
    target_context_hash: `tc1:${'a'.repeat(64)}`,
  };
  const evidence = {
    observation_id: 'observation-action', surface: { id: 'target-1', epoch: 1 }, scope: currentScope,
    environment_revision: 3, binding_ref: 'binding-action', binding, valid_until_ms: 400,
    freshness: 'verified', dependencies: {},
  };
  const submission = {
    version: 1, request_id: '22222222-2222-4222-8222-222222222222', channel_generation: 1,
    session_epoch: 1, scope_id: currentScope.scope_id, scope_generation: 1, sequence: 1,
    intent: {
      intent_id: 'intent-action', attempt_id: 'attempt-1', task_id: 'root-1', task_revision: 1,
      root_epoch: 1, session_epoch: 1, permit: { id: 'permit-1', generation: 1 }, evidence,
      latest_start_ms: 200, duration_ms: 25, action: { type: 'set_checkbox', checked: true },
    },
    authority: {
      task_id: 'root-1', task_revision: 1, root_epoch: 1, attempt_id: 'attempt-1',
      permit: { id: 'permit-1', generation: 1 }, stop_seq: 0, command_start_ms: 160,
      command_end_ms: 185, valid_until_ms: 185, reply_deadline_ms: 200,
    },
    authority_evidence: evidence,
  };
  return { submission, reply: {
    version: 1, request_id: submission.request_id, channel_generation: 1, session_epoch: 1,
    scope_id: currentScope.scope_id, scope_generation: 1, sequence: 1, intent_id: 'intent-action',
    outcome: 'completed', submitted_at_ms: 165, input_end_ms: 178, failure_code: null,
  } };
}

function checkedRead(snapshotId: string, sourceSequence: number, observedMs: number) {
  const currentScope = scope();
  return {
    snapshot_id: snapshotId,
    source_sequence: sourceSequence,
    observed_ms: observedMs,
    read_finished_ms: observedMs + 2,
    clock_domain: currentScope.clock_domain,
    context_identity: proof.contextIdentity(currentScope.context),
    target_context_hash: execution().submission.intent.evidence.binding.target_context_hash,
    checkbox: {
      ax_node_id: 'ax-checkbox', backend_dom_node_id: 7,
      role: 'checkbox', name: 'Background option', checked: 'true', disabled: 'false', ignored: false,
    },
  };
}

function ledgerFixture() {
  return {
    roots: [
      { root_id: 'root-1', user_text: 'Enable browser fixture checkbox', state: 'completed', goal: fixtureGoal(),
        admitted_allowed_scope: fixtureGoal().allowed_scope, admitted_allowed_scope_app: 'browser.fixture' },
      { root_id: 'root-2', user_text: 'Enable browser fixture checkbox', state: 'completed', goal: fixtureGoal(),
        admitted_allowed_scope: fixtureGoal().allowed_scope, admitted_allowed_scope_app: 'browser.fixture' },
    ],
    attempts: [
      { root_id: 'root-1', attempt_id: 'attempt-1', intent_id: 'intent-action', state: 'settled', activation_seq: 1, refunded: 0, created_ms: 160 },
      { root_id: 'root-2', attempt_id: 'attempt-no-input', intent_id: null, state: 'settled', activation_seq: 1, refunded: 1, created_ms: 190 },
    ],
    effects: [
      { root_id: 'root-1', status: 'satisfied', predicate: 'browser.checkbox_state@1', target: 'browser.fixture.background_option',
        intent_id: 'intent-action', attempt_id: 'attempt-1', evidence_group: ['snapshot-input-verification'],
        input_span: { from_ms: 165, to_ms: 178 }, observed_ms: 180, confirmation_scope: 'current_ui', causality: 'unknown' },
      { root_id: 'root-2', status: 'satisfied', predicate: 'browser.checkbox_state@1', target: 'browser.fixture.background_option',
        intent_id: null, attempt_id: 'attempt-no-input', evidence_group: ['snapshot-no-input-verification'],
        input_span: null, observed_ms: 200, confirmation_scope: 'current_ui', causality: 'not_applicable' },
    ],
    model_calls: 0, decision_model_calls: 0,
  };
}

function fixtureGoal() {
  return {
    allowed_scope: { application: 'Browser fixture', targets: [{ control: 'Background option', group: null, disambiguator: null }], no_unrelated_controls: true },
    scope: { app: 'browser.fixture', account: null },
    execution_domain: 'headless_browser', browser_page_profile: { kind: 'fixture_background_option' },
    confirmation_scope: 'current_ui', target: { semantic_target: 'browser.fixture.background_option', label: 'Background option', role_hint: 'checkbox' },
  };
}

test('the product smoke validates the exact fixed fixture scope and rejects foreign targets', () => {
  const fixed = scope();
  assert.equal(proof.validateScope(fixed), fixed);
  assert.equal(proof.sameScope(fixed, structuredClone(fixed)), true);

  const foreign = structuredClone(fixed);
  foreign.context.fixture_file_url = 'https://example.invalid/page';
  assert.throws(() => proof.validateScope(foreign), /fixed local fixture/);
  const extra = structuredClone(fixed) as Record<string, unknown>;
  extra['native_window'] = { id: 'window-1' };
  assert.throws(() => proof.validateScope(extra), /invalid object shape/);
});

test('owner and browser snapshot diagnostics stay fixed-shape and bounded', () => {
  const root = mkdtempSync(join(realpathSync(tmpdir()), 'product-smoke-owner-'));
  const supervisorDir = join(root, 'cu-owned');
  mkdirSync(supervisorDir);
  try {
    const owner = {
      supervisorDir,
      profileParent: join(realpathSync(supervisorDir), 'headless-profiles'),
      pids: {},
    };
    assert.equal(proof.validateOwnerRecord(owner, realpathSync), owner);
    assert.equal(proof.hasCompleteOwnerPids(owner.pids), false);
    assert.equal(proof.hasCompleteOwnerPids({ host: 11, device: 12, source: 13 }), true);
    assert.throws(() => proof.validateOwnerRecord({ ...owner, profileParent: root }, realpathSync), /outside/);

    const snapshot = {
      snapshot_id: 'snapshot-1', source_sequence: 1, observed_ms: 110, read_finished_ms: 120,
      clock_domain: 'headless/1/wall', context: scope().context,
      target_context_hash: `tc1:${'a'.repeat(64)}`, root_ax_node_id: 'root',
      nodes: [
        { ax_node_id: 'root', parent_ax_node_id: null, child_ax_node_ids: ['checkbox'], role: 'RootWebArea', name: null,
          disabled: 'unknown', checked: 'unknown', ignored: false },
        { ax_node_id: 'checkbox', parent_ax_node_id: 'root', child_ax_node_ids: [], role: 'checkbox', name: 'Background option',
          disabled: 'false', checked: 'false', ignored: false },
      ],
    };
    assert.equal(proof.validateSnapshot(snapshot), snapshot);
    assert.throws(() => proof.validateSnapshot({ ...snapshot, nodes: [
      { ...snapshot.nodes[0], value: 'not allowed' }, snapshot.nodes[1],
    ] }), /unsupported field/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('real browser execution diagnostics correlate one completed changed checkbox action', () => {
  const valid = execution();
  assert.equal(proof.validateExecution(valid), valid);
  const mismatch = structuredClone(valid);
  mismatch.reply.intent_id = 'other-intent';
  assert.throws(() => proof.validateExecution(mismatch), /mismatched/);
  const noChange = structuredClone(valid);
  noChange.submission.intent.action.checked = false;
  assert.throws(() => proof.validateExecution(noChange), /does not target/);
});

test('ledger evidence distinguishes receipt-backed input from a satisfied same-target no-input root', () => {
  const actual = execution();
  const ledger = ledgerFixture();
  const reads = [checkedRead('snapshot-input-verification', 3, 180), checkedRead('snapshot-no-input-verification', 4, 200)];
  assert.equal(proof.validateLedger(ledger, 'root-1', 'root-2', [actual], reads), ledger);
  const forgedNoInput = structuredClone(ledger);
  forgedNoInput.effects[1]!.input_span = { from_ms: 200, to_ms: 210 };
  assert.throws(() => proof.validateLedger(forgedNoInput, 'root-1', 'root-2', [actual], reads), /no-input/);

  const wrongPredicate = structuredClone(ledger);
  wrongPredicate.effects[0]!.predicate = 'goal.required@1';
  assert.throws(() => proof.validateLedger(wrongPredicate, 'root-1', 'root-2', [actual], reads), /input effect/);

  const wrongTarget = structuredClone(ledger);
  wrongTarget.effects[0]!.target = 'another.checkbox';
  assert.throws(() => proof.validateLedger(wrongTarget, 'root-1', 'root-2', [actual], reads), /input effect/);

  const wrongConfirmationScope = structuredClone(ledger);
  wrongConfirmationScope.effects[1]!.confirmation_scope = 'document';
  assert.throws(() => proof.validateLedger(wrongConfirmationScope, 'root-1', 'root-2', [actual], reads), /no-input effect/);

  const wrongCausality = structuredClone(ledger);
  wrongCausality.effects[0]!.causality = 'confirmed';
  assert.throws(() => proof.validateLedger(wrongCausality, 'root-1', 'root-2', [actual], reads), /input effect/);

  const staleEvidence = structuredClone(ledger);
  staleEvidence.effects[1]!.evidence_group = ['snapshot-input-verification'];
  assert.throws(() => proof.validateLedger(staleEvidence, 'root-1', 'root-2', [actual], reads), /no-input effect/);

  const missingNoInputAdmission = structuredClone(ledger);
  missingNoInputAdmission.attempts[1]!.refunded = 0;
  assert.throws(() => proof.validateLedger(missingNoInputAdmission, 'root-1', 'root-2', [actual], reads), /attempt ledger/);
});

test('ledger evidence requires the persisted scope of both confirmed Roots', () => {
  const actual = execution();
  const reads = [checkedRead('snapshot-input-verification', 3, 180), checkedRead('snapshot-no-input-verification', 4, 200)];
  for (const change of [
    (goal: any) => { goal.allowed_scope = null; },
    (goal: any) => { goal.allowed_scope.application = 'Another app'; },
    (goal: any) => { goal.allowed_scope.targets[0].control = 'Another control'; },
    (goal: any) => { goal.allowed_scope.targets[0].group = 'Other group'; },
    (goal: any) => { goal.allowed_scope.no_unrelated_controls = false; },
    (goal: any) => { goal.allowed_scope.targets.push(structuredClone(goal.allowed_scope.targets[0])); },
  ]) {
    for (const index of [0, 1]) {
      const forged = ledgerFixture();
      change(forged.roots[index]!.goal);
      assert.throws(() => proof.validateLedger(forged, 'root-1', 'root-2', [actual], reads), /completed fixed fixture goals/);
    }
  }
  for (const index of [0, 1]) {
    const wrongApplication = ledgerFixture();
    wrongApplication.roots[index]!.admitted_allowed_scope_app = 'another.app';
    assert.throws(() => proof.validateLedger(wrongApplication, 'root-1', 'root-2', [actual], reads), /completed fixed fixture goals/);
    const wrongAdmittedScope = ledgerFixture();
    wrongAdmittedScope.roots[index]!.admitted_allowed_scope.targets[0]!.control = 'Another control';
    assert.throws(() => proof.validateLedger(wrongAdmittedScope, 'root-1', 'root-2', [actual], reads), /completed fixed fixture goals/);
  }
});
