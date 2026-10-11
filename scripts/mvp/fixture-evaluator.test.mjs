import assert from 'node:assert/strict';
import test from 'node:test';

import { fixtureTaskIds } from './business-results.mjs';
import { evaluateFixture } from './fixture-evaluator.mjs';

// These are synthetic evaluator inputs only; none represents an executed task.
const EVIDENCE = Object.freeze({
  accepted: true,
  system_reported_completed: true,
  current_ui_verified: true,
  fresh_revisit_verified: true,
  unknown_receipt: false,
  scope_valid: true,
  trace_validated: true,
});
const CHANGE_COUNT_KEYS = Object.freeze({
  'SET-01': ['email_alerts'],
  'SET-02': ['weekly_digest'],
  'SET-03': ['include_archived_items'],
  'SET-04': ['quiet_hours'],
  'SET-05': ['compact_rows'],
  'SET-06': ['system_show_alerts', 'workspace_show_alerts'],
  'FORM-01': ['display_name'],
  'FORM-02': ['notes'],
  'FORM-03': ['reminder_cadence'],
  'FORM-04': ['summary_grouping'],
  'FORM-05': ['notify_on_change'],
  'FORM-06': ['region_code', 'validation_state'],
  'FORM-07': ['alert_threshold'],
  'LIST-01': ['selected_row'],
  'LIST-02': ['selected_row'],
  'LIST-03': ['selected_row'],
  'LIST-04': ['selected_row', 'target_status'],
  'MODAL-01': ['modal_choice', 'background_alerts'],
  'MODAL-02': ['reminder_category', 'background_alerts'],
  'MODAL-03': ['modal_choice', 'background_alerts'],
});

function syntheticTruth(taskId, seed) {
  const truth = {
    evidence: ['independent_post_state', 'required_persistence_receipt_when_specified'],
    forbidden_side_effects: ['unrelated_controls_changed', 'background_modal_controls_changed'],
  };
  switch (taskId) {
    case 'SET-01': Object.assign(truth, { required_final_state: { email_alerts: false }, required_change_count: 0 }); break;
    case 'SET-02': truth.required_final_state = { weekly_digest: false }; break;
    case 'SET-03': Object.assign(truth, { required_final_state: { include_archived_items: true }, persistence: 'explicit_save_receipt' }); break;
    case 'SET-04': Object.assign(truth, { required_final_state: { quiet_hours: true }, persistence: 'autosave_completed_before_finish' }); break;
    case 'SET-05': Object.assign(truth, { required_final_state: { compact_rows: true }, required_fresh_readback: true }); break;
    case 'SET-06': Object.assign(truth, {
      required_final_state: { system_show_alerts: false, workspace_show_alerts: false },
      unchanged_controls: ['workspace_show_alerts'],
    }); break;
    case 'FORM-01': truth.required_final_state = { display_name: 'Synthetic name' }; break;
    case 'FORM-02': Object.assign(truth, { required_final_state: { notes: 'First line\nSecond line' }, line_count: 2 }); break;
    case 'FORM-03': truth.required_final_state = { reminder_cadence: 'Weekly' }; break;
    case 'FORM-04': truth.required_final_state = { summary_grouping: 'By team' }; break;
    case 'FORM-05': truth.required_final_state = { notify_on_change: true }; break;
    case 'FORM-06': Object.assign(truth, { required_final_state: { region_code: 'US' }, validation_state: 'valid' }); break;
    case 'FORM-07': truth.required_final_state = { alert_threshold: 8 }; break;
    case 'LIST-01': truth.required_selected_row = `quarterly_review_${String(seed).padStart(2, '0')}`; break;
    case 'LIST-02': Object.assign(truth, { selected_context: { owner: 'Mina', status: 'Review' }, unchanged_other_row: true }); break;
    case 'LIST-03': Object.assign(truth, {
      required_selected_row: `archived_item_${100 + seed}`,
      target_position_after_20: true,
    }); break;
    case 'LIST-04': Object.assign(truth, {
      required_final_state: { target_status: 'Reviewed' },
      row_identity_must_survive_reorder: true,
    }); break;
    case 'MODAL-01': Object.assign(truth, { modal_open: false, background_unchanged: true }); break;
    case 'MODAL-02': Object.assign(truth, {
      modal_control: { reminder_category: 'Other' },
      modal_remains_open: true,
    }); break;
    case 'MODAL-03': Object.assign(truth, {
      modal_choice: 'keep_current_selection',
      background_controls_unchanged: true,
    }); break;
    default: throw new TypeError(`unknown synthetic fixture ${taskId}`);
  }
  return { suite: 'fixture', task_id: taskId, seed, evaluator_truth: truth };
}

function syntheticCase(taskId, seed = 1) {
  const profileTruth = syntheticTruth(taskId, seed);
  const truth = profileTruth.evaluator_truth;
  const values = structuredClone(truth.required_final_state ?? {});
  if (truth.validation_state) values.validation_state = truth.validation_state;
  if (truth.line_count) values.notes = truth.required_final_state.notes;
  const snapshot = {
    version: 1,
    task_id: taskId,
    seed,
    values,
    selected_row: null,
    modal: { open: false, choice: 'dismiss', controls: {} },
    persistence: { saved: true, kind: 'none', explicit_save_count: 0, autosave_count: 0, receipt: null },
    audit: {
      change_counts: Object.fromEntries(CHANGE_COUNT_KEYS[taskId].map(key => [key, 0])),
      background_change_count: 0,
      other_row_change_count: 0,
      revision: 8,
      revisit_count: 0,
      row_rebind_count: 0,
    },
  };
  if (taskId === 'SET-03') {
    snapshot.persistence = {
      saved: true, kind: 'manual', explicit_save_count: 1, autosave_count: 0,
      receipt: { kind: 'manual', revision: 8 },
    };
  } else if (taskId === 'SET-04') {
    snapshot.persistence = {
      saved: true, kind: 'automatic', explicit_save_count: 0, autosave_count: 1,
      receipt: { kind: 'automatic', revision: 8 },
    };
  }
  if (taskId === 'SET-05') snapshot.audit.revisit_count = 1;
  if (taskId === 'SET-06') snapshot.audit.change_counts.workspace_show_alerts = 0;
  if (taskId.startsWith('LIST-')) {
    const row = {
      id: taskId === 'LIST-01' ? truth.required_selected_row
        : taskId === 'LIST-03' ? truth.required_selected_row
          : taskId === 'LIST-04' ? 'record-03' : 'synthetic-list-row',
      label: taskId === 'LIST-02' ? 'Quarterly review' : 'Synthetic row',
      owner: taskId === 'LIST-02' ? truth.selected_context.owner : 'Synthetic owner',
      status: taskId === 'LIST-02' ? truth.selected_context.status : 'Synthetic status',
      position: taskId === 'LIST-03' ? 21 : taskId === 'LIST-04' ? 1 : 1,
    };
    snapshot.selected_row = row;
    if (taskId === 'LIST-03' || taskId === 'LIST-04') snapshot.audit.row_rebind_count = 1;
  }
  if (taskId === 'MODAL-01') snapshot.modal = { open: false, choice: 'dismiss', controls: {} };
  if (taskId === 'MODAL-02') snapshot.modal = { open: true, choice: null, controls: { reminder_category: 'Other' } };
  if (taskId === 'MODAL-03') snapshot.modal = { open: false, choice: 'keep_current_selection', controls: {} };
  const evidence = { version: 1, task_id: taskId, seed, ...EVIDENCE };
  return { profileTruth, appSnapshot: snapshot, evidence };
}

function evaluate(caseData) {
  return evaluateFixture(caseData);
}

test('synthetic snapshots satisfy each of the 20 fixture truth shapes without claiming business completion', () => {
  assert.equal(fixtureTaskIds.length, 20);
  for (const taskId of fixtureTaskIds) {
    const result = evaluate(syntheticCase(taskId));
    assert.equal(result.verdict, 'satisfied', taskId);
    assert.deepEqual(result.reason_codes, [], taskId);
    assert.equal(result.artifact_only, true);
    assert.equal(result.evidence_claims_only, true);
    assert.equal(result.evidence_validity_audited, false);
    assert.equal(Object.hasOwn(result, 'mvp_complete'), false);
    assert.deepEqual(Object.keys(result), [
      'version', 'suite', 'task_id', 'seed', 'verdict', 'reason_codes',
      'artifact_only', 'evidence_claims_only', 'evidence_validity_audited',
    ]);
  }
});

test('synthetic negative cases independently exercise the 20 task conditions', () => {
  const cases = [
    ['SET-01', c => { c.appSnapshot.audit.change_counts.email_alerts = 1; }, 'change_count_mismatch'],
    ['SET-02', c => { c.appSnapshot.values.weekly_digest = true; }, 'condition_not_satisfied'],
    ['SET-03', c => { c.appSnapshot.persistence.receipt.kind = 'automatic'; }, 'persistence_not_satisfied'],
    ['SET-04', c => { c.appSnapshot.persistence.autosave_count = 0; }, 'persistence_not_satisfied'],
    ['SET-05', c => { c.appSnapshot.audit.revisit_count = 0; }, 'fresh_revisit_unverified'],
    ['SET-06', c => { c.appSnapshot.audit.change_counts.workspace_show_alerts = 1; }, 'side_effect_detected'],
    ['FORM-01', c => { c.appSnapshot.values.display_name = 'Different'; }, 'condition_not_satisfied'],
    ['FORM-02', c => { c.appSnapshot.values.notes = 'Only one line'; }, 'condition_not_satisfied'],
    ['FORM-03', c => { c.appSnapshot.values.reminder_cadence = 'Daily'; }, 'condition_not_satisfied'],
    ['FORM-04', c => { c.appSnapshot.values.summary_grouping = 'By date'; }, 'condition_not_satisfied'],
    ['FORM-05', c => { c.appSnapshot.values.notify_on_change = false; }, 'condition_not_satisfied'],
    ['FORM-06', c => { c.appSnapshot.values.validation_state = 'invalid'; }, 'condition_not_satisfied'],
    ['FORM-07', c => { c.appSnapshot.values.alert_threshold = 9; }, 'condition_not_satisfied'],
    ['LIST-01', c => { c.appSnapshot.selected_row.id = 'other-row'; }, 'condition_not_satisfied'],
    ['LIST-02', c => { c.appSnapshot.audit.other_row_change_count = 1; }, 'side_effect_detected'],
    ['LIST-03', c => { c.appSnapshot.selected_row.position = 20; }, 'condition_not_satisfied'],
    ['LIST-04', c => { c.appSnapshot.selected_row.id = 'replacement-row'; }, 'condition_not_satisfied'],
    ['MODAL-01', c => { c.appSnapshot.modal.open = true; }, 'condition_not_satisfied'],
    ['MODAL-02', c => { c.appSnapshot.modal.controls.reminder_category = 'Work'; }, 'condition_not_satisfied'],
    ['MODAL-03', c => { c.appSnapshot.modal.choice = 'dismiss'; }, 'condition_not_satisfied'],
  ];
  for (const [taskId, mutate, reason] of cases) {
    const caseData = structuredClone(syntheticCase(taskId));
    mutate(caseData);
    const result = evaluate(caseData);
    assert.equal(result.verdict, reason === 'fresh_revisit_unverified' ? 'unknown' : 'not_satisfied', taskId);
    assert.ok(result.reason_codes.includes(reason), taskId);
  }

  const wrongReceiptRevision = structuredClone(syntheticCase('SET-03'));
  wrongReceiptRevision.appSnapshot.persistence.receipt.revision = 7;
  assert.equal(evaluate(wrongReceiptRevision).verdict, 'not_satisfied');

  const wrongReceiptIdentity = structuredClone(syntheticCase('SET-04'));
  wrongReceiptIdentity.appSnapshot.persistence.receipt.kind = 'manual';
  assert.equal(evaluate(wrongReceiptIdentity).verdict, 'not_satisfied');

  const modalBackground = structuredClone(syntheticCase('MODAL-02'));
  modalBackground.appSnapshot.audit.change_counts.background_alerts = 1;
  assert.equal(evaluate(modalBackground).verdict, 'not_satisfied');

  const modalAudit = structuredClone(syntheticCase('MODAL-03'));
  modalAudit.appSnapshot.audit.background_change_count = 1;
  assert.equal(evaluate(modalAudit).verdict, 'not_satisfied');

  const reorderedOtherRowChanged = structuredClone(syntheticCase('LIST-04'));
  reorderedOtherRowChanged.appSnapshot.audit.other_row_change_count = 1;
  assert.equal(evaluate(reorderedOtherRowChanged).verdict, 'not_satisfied');
});

test('missing and explicitly unknown completion, UI, revisit, scope, and receipt evidence never satisfy a task', () => {
  const mutations = [
    c => { delete c.evidence.accepted; },
    c => { delete c.evidence.system_reported_completed; },
    c => { delete c.evidence.current_ui_verified; },
    c => { delete c.evidence.trace_validated; },
    c => { delete c.evidence.scope_valid; },
    c => { delete c.evidence.unknown_receipt; },
    c => { c.evidence.current_ui_verified = false; },
    c => { c.evidence.trace_validated = false; },
    c => { c.evidence.unknown_receipt = true; },
  ];
  for (const mutate of mutations) {
    const caseData = structuredClone(syntheticCase('SET-02'));
    mutate(caseData);
    assert.notEqual(evaluate(caseData).verdict, 'satisfied');
  }

  const noFreshReadback = structuredClone(syntheticCase('SET-05'));
  delete noFreshReadback.evidence.fresh_revisit_verified;
  assert.equal(evaluate(noFreshReadback).verdict, 'unknown');

  const missingReceiptKind = structuredClone(syntheticCase('SET-03'));
  delete missingReceiptKind.appSnapshot.persistence.receipt.kind;
  assert.equal(evaluate(missingReceiptKind).verdict, 'unknown');

  const missingReceiptRevision = structuredClone(syntheticCase('SET-04'));
  delete missingReceiptRevision.appSnapshot.persistence.receipt.revision;
  assert.equal(evaluate(missingReceiptRevision).verdict, 'unknown');

  const missingUiValue = structuredClone(syntheticCase('FORM-01'));
  delete missingUiValue.appSnapshot.values.display_name;
  assert.equal(evaluate(missingUiValue).verdict, 'unknown');

  const missingCounter = structuredClone(syntheticCase('SET-01'));
  delete missingCounter.appSnapshot.audit.change_counts.email_alerts;
  assert.equal(evaluate(missingCounter).verdict, 'unknown');

  const missingSnapshot = syntheticCase('SET-02');
  missingSnapshot.appSnapshot = null;
  missingSnapshot.evidence = null;
  assert.equal(evaluate(missingSnapshot).verdict, 'unknown');

  const missingSnapshotIdentity = structuredClone(syntheticCase('SET-02'));
  delete missingSnapshotIdentity.appSnapshot.version;
  assert.equal(evaluate(missingSnapshotIdentity).verdict, 'unknown');

  const missingEvidenceIdentity = structuredClone(syntheticCase('SET-02'));
  delete missingEvidenceIdentity.evidence.version;
  assert.equal(evaluate(missingEvidenceIdentity).verdict, 'unknown');

  const nullUiMap = structuredClone(syntheticCase('SET-02'));
  nullUiMap.appSnapshot.values = null;
  assert.equal(evaluate(nullUiMap).verdict, 'unknown');
});

test('known rejection, incomplete scope and invalid app evidence stay bounded and cannot be merged across tasks', () => {
  const rejected = structuredClone(syntheticCase('SET-02'));
  rejected.evidence.accepted = false;
  assert.equal(evaluate(rejected).verdict, 'not_satisfied');
  assert.ok(evaluate(rejected).reason_codes.includes('task_not_accepted'));

  const incomplete = structuredClone(syntheticCase('SET-02'));
  incomplete.evidence.system_reported_completed = false;
  assert.equal(evaluate(incomplete).verdict, 'not_satisfied');

  const invalidScope = structuredClone(syntheticCase('SET-02'));
  invalidScope.evidence.scope_valid = false;
  assert.equal(evaluate(invalidScope).verdict, 'not_satisfied');
  assert.ok(evaluate(invalidScope).reason_codes.includes('scope_invalid'));

  const outside = structuredClone(syntheticCase('SET-02'));
  outside.profileTruth.records = [outside.profileTruth, outside.profileTruth];
  assert.throws(() => evaluate(outside), TypeError);

  const mismatchedSeed = structuredClone(syntheticCase('SET-02'));
  mismatchedSeed.appSnapshot.seed = 2;
  assert.throws(() => evaluate(mismatchedSeed), TypeError);

  const mismatchedSnapshotVersion = structuredClone(syntheticCase('SET-02'));
  mismatchedSnapshotVersion.appSnapshot.version = 2;
  assert.throws(() => evaluate(mismatchedSnapshotVersion), TypeError);

  const mismatchedEvidence = structuredClone(syntheticCase('SET-02'));
  mismatchedEvidence.evidence.task_id = 'SET-03';
  assert.throws(() => evaluate(mismatchedEvidence), TypeError);

  const mismatchedEvidenceVersion = structuredClone(syntheticCase('SET-02'));
  mismatchedEvidenceVersion.evidence.version = 2;
  assert.throws(() => evaluate(mismatchedEvidenceVersion), TypeError);

  const evaluatorLabel = structuredClone(syntheticCase('SET-02'));
  evaluatorLabel.appSnapshot.values.goal_expected = false;
  assert.throws(() => evaluate(evaluatorLabel), TypeError);

  const unknownAudit = structuredClone(syntheticCase('SET-02'));
  unknownAudit.appSnapshot.audit.change_counts.unrelated_control = 0;
  assert.throws(() => evaluate(unknownAudit), TypeError);

  const controlText = structuredClone(syntheticCase('LIST-01'));
  controlText.appSnapshot.selected_row.id = 'bad\nrow';
  assert.throws(() => evaluate(controlText), TypeError);

  const nonFiniteValue = structuredClone(syntheticCase('FORM-07'));
  nonFiniteValue.appSnapshot.values.alert_threshold = Number.NaN;
  assert.throws(() => evaluate(nonFiniteValue), TypeError);

  const longValue = structuredClone(syntheticCase('FORM-01'));
  longValue.appSnapshot.values.display_name = 'x'.repeat(8193);
  assert.throws(() => evaluate(longValue), TypeError);
});
