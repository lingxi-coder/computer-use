import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { closeSync, constants, existsSync, mkdtempSync, mkdirSync, openSync, readFileSync, rmdirSync, unlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import {
  createRejectedReceipt,
  modelAttributionFromEvidence,
  parseProductDriverArgs,
  stageNormalMainFinalRead,
  stageNormalMainHostLedger,
  stageNormalMainResult,
  traceAuditPassed,
  typedLedgerFromEvidence,
  validateGoalCatalog,
  validateProductDriverInputs,
} from './product-driver.mts';
import { fixtureTaskIds, textEditTaskIds } from './business-results.mjs';
import { frozenGoalSha256, frozenGoalStableJson } from './frozen-goals.mjs';
import {
  fixtureGoalConfirmationMatchesRequest,
  fixtureScopeFamilyForTaskId,
  validateFixtureAllowedScope,
  validateFixtureGoalPrompt,
} from './frozen-fixture-scope.mjs';
import { validateDriverReceipt } from './business-run-receipt.mjs';

const sha256 = (value: Buffer | string) => createHash('sha256').update(value).digest('hex');
const canonicalSha256 = (value: unknown) => frozenGoalSha256(Buffer.from(frozenGoalStableJson(value), 'utf8'));
const sourceFreezeSha256 = 'a'.repeat(64);

function identities() {
  return [
    ...fixtureTaskIds.flatMap(task_id => [1, 2, 3, 4, 5].map(seed => ({ suite: 'fixture' as const, task_id, seed }))),
    ...textEditTaskIds.map(task_id => ({ suite: 'textedit' as const, task_id, seed: 1 })),
  ];
}

function goalFor(index: number, identity: ReturnType<typeof identities>[number]) {
  if (identity.suite === 'fixture' && identity.task_id === 'SET-02' && identity.seed === 1) {
    return {
      objective: '关闭“每周摘要”。',
      allowed_scope: {
        application: 'Interaction Fixture',
        targets: [{ control: '每周摘要', group: '通知' }],
        no_unrelated_controls: true,
      },
    };
  }
  return {
    objective: `Frozen request ${index}`,
    allowed_scope: { application: identity.suite === 'fixture' ? 'Interaction Fixture' : 'TextEdit' },
  };
}

function selector(identity: ReturnType<typeof identities>[number], goal: unknown) {
  const goalHash = canonicalSha256(goal);
  return sha256(`computer-use-frozen-task-selector-v1\n${sourceFreezeSha256}\n${identity.suite}\n${identity.task_id}\n${identity.seed}\n${goalHash}`);
}

function catalog({ tamperSelector = false, dropLast = false } = {}) {
  const ids = identities();
  const records = ids.map((identity, index) => {
    const goal = goalFor(index, identity);
    return { selector: selector(identity, goal), ...identity, goal };
  });
  if (tamperSelector) records[0]!.selector = 'b'.repeat(64);
  if (dropLast) records.pop();
  const goals = { format_version: 1, task_profile_version: 1, records };
  const goalsBytes = Buffer.from(frozenGoalStableJson(goals), 'utf8');
  const manifest = {
    format_version: 1,
    task_profile_version: 1,
    source_freeze_sha256: sourceFreezeSha256,
    source_freeze_content_sha256: 'c'.repeat(64),
    source_request_sha256: 'd'.repeat(64),
    task_goals_sha256: sha256(goalsBytes),
    identities_sha256: canonicalSha256(ids.map(item => [item.suite, item.task_id, item.seed])),
    identity_count: 120,
    denominators: { fixture_runs: 100, textedit_runs: 20, total_records: 120 },
    selector_scheme: 'sha256/domain-separated-v1',
  };
  const manifestBytes = Buffer.from(frozenGoalStableJson(manifest), 'utf8');
  const identity = { suite: 'fixture' as const, task_id: 'SET-02', seed: 1 };
  return {
    goalsBytes,
    manifestBytes,
    identity,
    request: goalFor(ids.findIndex(item => item.suite === identity.suite && item.task_id === identity.task_id && item.seed === identity.seed), identity),
    goalsSha256: sha256(goalsBytes),
    manifestSha256: sha256(manifestBytes),
  };
}

function set02Reset() {
  return {
    version: 1,
    task_id: 'SET-02',
    seed: 1,
    initial_state: {
      application: { bundle_id: 'com.lingxi.computer-user.mvp-fixture', name: 'Interaction Fixture', version: '1.0.0' },
      variant: { language: 'zh-CN', theme: 'dark', layout: 'standard' },
      controls: [{ label: '每周摘要', value: true }],
    },
  };
}

function expectedVariant(taskId: string, seed: number) {
  const taskIndex = fixtureTaskIds.indexOf(taskId);
  const seedIndex = seed - 1;
  return {
    language: ['en-US', 'zh-CN'][(taskIndex + seedIndex) % 2],
    theme: ['light', 'dark'][(taskIndex + Math.floor(seedIndex / 2)) % 2],
    layout: ['compact', 'standard', 'wide', 'two_column'][(taskIndex + seedIndex) % 4],
  };
}

test('validates the full 120-record frozen catalog and selected request equality', () => {
  const input = catalog();
  const checked = validateGoalCatalog(input);
  assert.equal(checked.goals.records.length, 120);
  assert.deepEqual(checked.selected.goal, input.request);
  assert.equal(checked.catalogId, input.manifestSha256);
  assert.equal(checked.selected.selector, selector(input.identity, input.request));
});

test('rejects a request that differs from its frozen selected identity', () => {
  const input = catalog();
  assert.throws(() => validateGoalCatalog({ ...input, request: { ...input.request, objective: `${input.request.objective} ` } }));
});

test('rejects selector tampering and a catalog that drops denominator rows', () => {
  const invalidSelector = catalog({ tamperSelector: true });
  assert.throws(() => validateGoalCatalog(invalidSelector), /selector/);
  const incomplete = catalog({ dropLast: true });
  assert.throws(() => validateGoalCatalog(incomplete), /120 identities/);
});

test('parses exactly the four pinned catalog args and coordinator-owned per-run paths', () => {
  const args = parseProductDriverArgs([
    '--task-goals-file', '/private/tmp/catalog/task-goals.json',
    '--goal-catalog-manifest-file', '/private/tmp/catalog/goal-catalog-manifest.json',
    '--task-goals-sha256', 'a'.repeat(64),
    '--goal-catalog-manifest-sha256', 'b'.repeat(64),
    '--suite', 'fixture', '--task-id', 'SET-02', '--seed', '1',
    '--request', '/private/tmp/run/request.json',
    '--launcher-reset-file', '/private/tmp/run/launcher-reset.json',
    '--output-dir', '/private/tmp/run', '--receipt', '/private/tmp/run/driver-receipt.json',
  ]);
  assert.equal(args.taskGoalsSha256, 'a'.repeat(64));
  assert.equal(args.goalManifestSha256, 'b'.repeat(64));
  assert.equal(args.taskId, 'SET-02');
  assert.throws(() => parseProductDriverArgs([
    '--task-goals-file', '/private/tmp/catalog/task-goals.json',
    '--goal-catalog-manifest-file', '/private/tmp/catalog/goal-catalog-manifest.json',
    '--task-goals-sha256', 'a'.repeat(64), '--goal-catalog-manifest-sha256', 'b'.repeat(64),
    '--suite', 'fixture', '--task-id', 'SET-02', '--seed', '1',
    '--request', '/private/tmp/run/request.json', '--launcher-reset-file', '/private/tmp/run/launcher-reset.json',
    '--output-dir', '/private/tmp/run', '--receipt', '/private/tmp/run/driver-receipt.json', '--truth-file', '/secret/truth.json',
  ]));
});

test('validates SET-02 reset as a separate launcher-only input and rejects truth/workflow material', () => {
  const input = catalog();
  assert.doesNotThrow(() => validateProductDriverInputs({ identity: input.identity, request: input.request, reset: set02Reset() }));
  assert.throws(() => validateProductDriverInputs({
    identity: input.identity,
    request: { ...input.request, workflow: ['turn it off'] },
    reset: set02Reset(),
  }));
  const wrongVariant = set02Reset();
  wrongVariant.initial_state.variant.language = 'en-US';
  assert.throws(() => validateProductDriverInputs({ identity: input.identity, request: input.request, reset: wrongVariant }));
});

test('accepts exact primary/form scope and validates form fixture reset structure', () => {
  const formCases = [
    { task_id: 'FORM-01', control: 'Display name', form: 'Profile', kind: 'single_line', value: 'Old name' },
    { task_id: 'FORM-02', control: 'Notes', form: 'Profile', kind: 'multiline', value: '' },
    { task_id: 'FORM-03', control: 'Reminder cadence', form: 'Preferences', kind: 'dropdown', value: 'Daily' },
    { task_id: 'FORM-04', control: 'Summary grouping', form: 'Preferences', kind: 'radio_group', value: 'By day' },
    { task_id: 'FORM-05', control: 'Notify on change', form: 'Preferences', kind: 'checkbox', value: false },
    { task_id: 'FORM-06', control: 'Region code', form: 'Account', kind: 'single_line', value: '??' },
    { task_id: 'FORM-07', control: 'Alert threshold', form: 'Advanced preferences', kind: 'number', value: 10 },
  ];
  assert.deepEqual(formCases.map(item => fixtureScopeFamilyForTaskId(item.task_id)), formCases.map(() => 'form'));
  for (const formCase of formCases) {
    const identity = { suite: 'fixture' as const, task_id: formCase.task_id, seed: 1 };
    const allowed_scope = {
      application: 'Interaction Fixture',
      targets: [{ control: formCase.control, form: formCase.form }],
      no_unrelated_controls: true,
    };
    const request = { objective: `Update ${formCase.control}.`, allowed_scope };
    const control = { label: formCase.control, value: formCase.value, input_kind: formCase.kind,
      ...(formCase.task_id === 'FORM-07' ? { initially_in_viewport: false } : {}) };
    const reset = {
      version: 1,
      task_id: identity.task_id,
      seed: identity.seed,
      initial_state: {
        application: { bundle_id: 'com.lingxi.computer-user.mvp-fixture', name: 'Interaction Fixture', version: '1.0.0' },
        variant: expectedVariant(identity.task_id, identity.seed),
        controls: [control],
        ...(formCase.task_id === 'FORM-06' ? { validation_state: 'invalid' } : {}),
      },
    };
    assert.deepEqual(validateFixtureAllowedScope(allowed_scope, identity.task_id), {
      family: 'form', target: allowed_scope.targets[0],
    });
    assert.doesNotThrow(() => validateProductDriverInputs({ identity, request, reset }), formCase.task_id);
  }
  assert.throws(() => validateFixtureAllowedScope({
    application: 'Interaction Fixture', targets: [{ control: 'Display name', group: 'Profile' }],
    no_unrelated_controls: true,
  }, 'FORM-01'));
  const badReset = {
    version: 1, task_id: 'FORM-01', seed: 1,
    initial_state: {
      application: { bundle_id: 'com.lingxi.computer-user.mvp-fixture', name: 'Interaction Fixture', version: '1.0.0' },
      variant: expectedVariant('FORM-01', 1),
      controls: [{ label: 'Display name', value: 'Old name', input_kind: 'number' }],
    },
  };
  assert.throws(() => validateProductDriverInputs({
    identity: { suite: 'fixture', task_id: 'FORM-01', seed: 1 },
    request: { objective: 'Set the name.', allowed_scope: {
      application: 'Interaction Fixture', targets: [{ control: 'Display name', form: 'Profile' }], no_unrelated_controls: true,
    } }, reset: badReset,
  }), /input kind/);
});

test('requires Goal confirmation to preserve the frozen form primary and context exactly', () => {
  const frozenScope = {
    application: 'Interaction Fixture',
    targets: [{ control: 'Notes', form: 'Profile' }],
    no_unrelated_controls: true,
  };
  const allowed_scope_wire = {
    application: 'Interaction Fixture',
    targets: [{ control: 'Notes', group: null, form: 'Profile', disambiguator: null }],
    no_unrelated_controls: true,
  };
  const goal = {
    user_text: 'Set the multiline Notes field.',
    app: 'com.lingxi.computer-user.mvp-fixture',
    target_label: 'Notes',
    group: 'Profile',
    row: null,
    desired: { type: 'text', value: 'A note' },
    confirmation_scope: 'ui_revisit',
    max_effect_class: 'reversible_edit',
    allowed_scope: allowed_scope_wire,
    scoped_revisit: null,
    browser_page_profile: null,
  };
  assert.deepEqual(validateFixtureGoalPrompt(goal, 'FORM-02').allowed_scope, frozenScope);
  assert.throws(() => validateFixtureAllowedScope(allowed_scope_wire, 'FORM-02'), /unexpected shape/);
  const confirmation = { scope_family: 'form', application: goal.app, target_label: goal.target_label, allowed_scope_wire };
  assert.equal(fixtureGoalConfirmationMatchesRequest(confirmation, 'FORM-02', frozenScope), true);
  assert.throws(() => validateFixtureGoalPrompt({ ...goal, app: 'Interaction Fixture' }, 'FORM-02'), /registered fixture bundle/);
  assert.equal(fixtureGoalConfirmationMatchesRequest({ ...confirmation, target_label: 'Other field' }, 'FORM-02', frozenScope), false);
  assert.equal(fixtureGoalConfirmationMatchesRequest({
    ...confirmation,
    allowed_scope_wire: { ...allowed_scope_wire, targets: [{ control: 'Notes', group: null, form: 'Preferences', disambiguator: null }] },
  }, 'FORM-02', frozenScope), false);
  assert.equal(fixtureGoalConfirmationMatchesRequest({
    ...confirmation,
    allowed_scope_wire: { ...allowed_scope_wire, targets: [{ control: 'Notes', group: 'Notifications', form: 'Profile', disambiguator: null }] },
  }, 'FORM-02', frozenScope), false);
});

test('normalizes Rust SET Goal scope wire nulls without weakening the frozen scope shape', () => {
  const frozenScope = {
    application: 'Interaction Fixture',
    targets: [{ control: 'Weekly digest', group: 'Notifications' }],
    no_unrelated_controls: true,
  };
  const compiledGoal = {
    user_text: 'Turn off Weekly digest.',
    app: 'com.lingxi.computer-user.mvp-fixture',
    target_label: 'Weekly digest',
    group: 'Notifications',
    row: null,
    desired: { type: 'bool', value: false },
    confirmation_scope: 'ui_revisit',
    max_effect_class: 'reversible_edit',
    allowed_scope: {
      application: 'Interaction Fixture',
      targets: [{ control: 'Weekly digest', group: 'Notifications', disambiguator: null }],
      no_unrelated_controls: true,
    },
    scoped_revisit: null,
    browser_page_profile: null,
  };
  assert.deepEqual(validateFixtureGoalPrompt(compiledGoal, 'SET-02').allowed_scope, frozenScope);
  assert.throws(() => validateFixtureGoalPrompt({
    ...compiledGoal,
    allowed_scope: { ...compiledGoal.allowed_scope, targets: [{ ...compiledGoal.allowed_scope.targets[0], form: null }] },
  }, 'SET-02'), /unexpected shape/);
});

test('receipt projection keeps missing provenance unknown and reports admitted row counts without claiming network counts', () => {
  const ledger = typedLedgerFromEvidence({
    decisions: [{ decision_id: 'decision-1', source: 'plan', created_ms: 12 }],
    attempts: [],
    effects: [{ attempt_id: 'attempt-1', result_count: 1 }],
  });
  assert.equal(ledger.decisions[0]?.source, 'unknown');
  assert.deepEqual(ledger.effects, []);
  const attribution = modelAttributionFromEvidence({ call_rows: [{
    scope: 's2_request', root_id: 'root-1', request_id: 'request-1', decision_id: null, generation: 1,
    provider: 'open_ai', model: 'configured-model', started_ms: 1, finished_ms: 2,
    outcome: 'unavailable', usage: { input_tokens: null, output_tokens: null, cost_micros: null },
  }] });
  assert.equal(attribution.admitted_remote_call_rows, 1);
  assert.equal(attribution.actual_network_requests, null);
  assert.equal(attribution.calls[0]?.provider_response_id, null);
  assert.equal(attribution.calls[0]?.cost_usd, null);
});

test('maps a plan decision to S2 only through the exact installed revision and request generation', () => {
  const host = {
    scope: { root_id: 'root-1' },
    decisions: [{ decision_id: 'decision-1', source: 'plan', generation: 1, plan_revision: 4, created_ms: 10 }],
    attempts: [],
    plan_installations: [{
      root_id: 'root-1', plan_revision: 4, state: 'installed', attribution_status: 'model_request',
      request_id: 'request-7', generation: 2, reason: 'proposal', installed_ms: 20,
    }],
    call_rows: [{
      scope: 's2_request', root_id: 'root-1', request_id: 'request-7', generation: 2, reason: 'proposal',
      outcome: 'proposal', finished_ms: 19, plan_installation_reference_status: 'matched',
      plan_installation_revisions: [4], provider: 'open_ai', model: 'configured-model',
      started_ms: 11, usage: { input_tokens: null, output_tokens: null, cost_micros: null },
    }],
    cross_references: {
      s2_plan_request: { time_based_matching_used: false },
      decisions: [{
        decision_id: 'decision-1', consumed_attempt_status: 'absent', attempt_map_status: 'absent',
        plan_installation_reference_status: 'matched', s2_request_reference_status: 'matched',
        s2_request: { request_id: 'request-7', generation: 2, reason: 'proposal' },
      }],
      plan_installations: [{ plan_revision: 4, state: 'installed', attribution_status: 'model_request', request_reference_status: 'matched' }],
      attempts: [],
      effect_results: [],
      effect_results_to_attempts: { status: 'no_effect_results', source_sequence_status: 'not_stored_in_current_schema' },
      local_inference_revisions: { status: 'missing_in_current_schema' },
    },
    effect_results: [],
  };
  const ledger = typedLedgerFromEvidence(host);
  assert.equal(ledger.decisions[0]?.source, 's2');
  assert.deepEqual(ledger.decisions[0]?.s2_request_ref, {
    root_id: 'root-1', request_id: 'request-7', generation: 2,
  });
  assert.equal(traceAuditPassed(host, ledger), true);

  const unrelated = structuredClone(host);
  unrelated.call_rows[0].request_id = 'different-request';
  unrelated.call_rows[0].plan_installation_revisions = [];
  const unknownLedger = typedLedgerFromEvidence(unrelated);
  assert.equal(unknownLedger.decisions[0]?.source, 'unknown');
  assert.equal(traceAuditPassed(unrelated, unknownLedger), false);
});

test('projects typed effect status only with an exact public attempt-reference match', () => {
  const host = {
    decisions: [{ decision_id: 'decision-1', source: 'rule', generation: 1, created_ms: 1 }],
    attempts: [{ attempt_id: 'attempt-1', decision_id: 'decision-1', intent_id: 'intent-1', state: 'settled' }],
    effect_results: [{
      result_id: 'result-1', revision: 1, projection_status: 'typed_effect_result', effect_status: 'satisfied',
      attempt_id: 'attempt-1', intent_id: 'intent-1', source_sequence: null,
    }],
    cross_references: { effect_results: [{ result_id: 'result-1', revision: 1, attempt_id: 'attempt-1', attempt_reference_status: 'matched' }] },
  };
  const ledger = typedLedgerFromEvidence(host);
  assert.deepEqual(ledger.effects, [{ attempt_id: 'attempt-1', verdict: 'satisfied', source_sequence: null }]);
  host.cross_references.effect_results[0].attempt_reference_status = 'intent_mismatch';
  assert.deepEqual(typedLedgerFromEvidence(host).effects, []);
});

test('stages normal Main current-UI output from its separate private directory', () => {
  const parent = mkdtempSync('/private/tmp/mvp-main-output-stage-');
  const mainOutput = join(parent, 'main-output');
  const runOutput = join(parent, 'run-output');
  mkdirSync(mainOutput, { mode: 0o700 });
  mkdirSync(runOutput, { mode: 0o700 });
  const source = join(mainOutput, 'final-current-ui.json');
  const expected = Buffer.from('{"source_sequence":9,"observed_at_ms":20}\n');
  const fd = openSync(source, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try { writeFileSync(fd, expected); }
  finally { closeSync(fd); }
  const staged = join(runOutput, 'final-current-ui.json');
  try {
    const result = stageNormalMainFinalRead(mainOutput, runOutput);
    assert.ok(result);
    assert.equal(result.path, staged);
    assert.deepEqual(result.bytes, expected);
    assert.deepEqual(readFileSync(staged), expected);
    assert.notEqual(result.path, source);
  } finally {
    if (existsSync(source)) unlinkSync(source);
    if (existsSync(staged)) unlinkSync(staged);
    rmdirSync(mainOutput);
    rmdirSync(runOutput);
    rmdirSync(parent);
  }
});

test('retains Main and Host diagnostics for an observed Root without accepted Goal confirmation', () => {
  const parent = mkdtempSync('/private/tmp/mvp-main-result-stage-');
  const mainOutput = join(parent, 'main-output');
  const runOutput = join(parent, 'run-output');
  mkdirSync(mainOutput, { mode: 0o700 });
  mkdirSync(runOutput, { mode: 0o700 });
  const source = join(mainOutput, 'normal-main-result.json');
  const ledgerSource = join(mainOutput, 'host-ledger-evidence.json');
  const expected = Buffer.from(`${JSON.stringify({
    status: 'blocked',
    root_id: 'root-1',
    root_task_revision: 3,
    failure_step: 'goal_confirmation_missing',
    goal_confirmation_received: false,
    goal_confirmation_accepted: false,
    observed_root: { root_task_id: 'root-1', task_revision: 3, first_state: 'compiling_goal', latest_state: 'failed' },
    consent_driver_failures: [{ code: 'native_dialog_not_found', detail: 'bounded diagnostic' }],
    native_child_pids: [{ pid: 43210, role: 'source_worker' }],
  })}\n`);
  const fd = openSync(source, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try { writeFileSync(fd, expected); }
  finally { closeSync(fd); }
  const ledgerBytes = Buffer.from('{"scope":{"root_id":"root-1"},"call_rows":[{"scope":"s2_request","outcome":"failed"}]}\n');
  const ledgerFd = openSync(ledgerSource, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try { writeFileSync(ledgerFd, ledgerBytes); }
  finally { closeSync(ledgerFd); }
  const stagedPath = join(runOutput, 'normal-main-result.json');
  const stagedLedgerPath = join(runOutput, 'host-ledger-evidence.json');
  const confirmedOutput = join(parent, 'confirmed-output');
  const confirmedRun = join(parent, 'confirmed-run');
  mkdirSync(confirmedOutput, { mode: 0o700 });
  mkdirSync(confirmedRun, { mode: 0o700 });
  const confirmedSource = join(confirmedOutput, 'normal-main-result.json');
  const confirmedBytes = Buffer.from(`${JSON.stringify({
    status: 'failed', root_id: 'root-2', failure_step: 'product_flow',
    goal_confirmation_received: true, goal_confirmation_accepted: true,
    goal_confirmation: { objective_sha256: 'a'.repeat(64) },
  })}\n`);
  const confirmedFd = openSync(confirmedSource, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try { writeFileSync(confirmedFd, confirmedBytes); }
  finally { closeSync(confirmedFd); }
  try {
    const staged = stageNormalMainResult(mainOutput, runOutput);
    assert.equal(staged.unconfirmedMainFailure?.code, 'NORMAL_MAIN_HARNESS_FAILED');
    assert.equal(staged.result.failure_step, 'goal_confirmation_missing');
    assert.equal(staged.result.root_id, 'root-1', 'an observed Root is retained without marking Goal confirmation accepted');
    assert.equal(staged.result.goal_confirmation_received, false);
    assert.deepEqual(staged.result.consent_driver_failures, [{ code: 'native_dialog_not_found', detail: 'bounded diagnostic' }]);
    assert.deepEqual(staged.result.native_child_pids, [{ pid: 43210, role: 'source_worker' }]);
    assert.equal(staged.path, stagedPath);
    assert.deepEqual(readFileSync(stagedPath), expected);
    assert.ok(existsSync(stagedPath), 'staged failure evidence must survive the subsequent pre-Root rejection');
    const stagedLedger = stageNormalMainHostLedger(mainOutput, runOutput);
    assert.ok(stagedLedger);
    assert.equal(stagedLedger.path, stagedLedgerPath);
    assert.deepEqual(stagedLedger.bytes, ledgerBytes);
    assert.deepEqual(readFileSync(stagedLedgerPath), ledgerBytes);
    const stagedConfirmed = stageNormalMainResult(confirmedOutput, confirmedRun);
    assert.equal(stagedConfirmed.unconfirmedMainFailure, null,
      'a post-confirmation product failure still proceeds to objective/scope/accounting checks');
  } finally {
    if (existsSync(source)) unlinkSync(source);
    if (existsSync(ledgerSource)) unlinkSync(ledgerSource);
    if (existsSync(stagedPath)) unlinkSync(stagedPath);
    if (existsSync(stagedLedgerPath)) unlinkSync(stagedLedgerPath);
    if (existsSync(confirmedSource)) unlinkSync(confirmedSource);
    const confirmedStaged = join(confirmedRun, 'normal-main-result.json');
    if (existsSync(confirmedStaged)) unlinkSync(confirmedStaged);
    rmdirSync(mainOutput);
    rmdirSync(runOutput);
    rmdirSync(confirmedOutput);
    rmdirSync(confirmedRun);
    rmdirSync(parent);
  }
});

test('explicit pre-admission refusal is a valid rejected receipt with no invented Root', () => {
  const identity = { suite: 'fixture' as const, task_id: 'SET-02', seed: 1 };
  const receipt = createRejectedReceipt(identity, 'product_model_not_configured');
  assert.equal(validateDriverReceipt(receipt, identity).root_id, null);
  assert.equal(receipt.admission.accepted, false);
  assert.equal(receipt.status, 'rejected');
});
