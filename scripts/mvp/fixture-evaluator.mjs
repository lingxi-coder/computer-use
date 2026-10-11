#!/usr/bin/env node
import { readFileSync, statSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { fixtureTaskIds } from './business-results.mjs';

const SEEDS = Object.freeze([1, 2, 3, 4, 5]);
const MAX_JSON_BYTES = 512 * 1024;
const MAX_MAP_ENTRIES = 128;
const MAX_TEXT_BYTES = 8192;
const MAX_ID_BYTES = 256;
const MAX_COUNTER = 1_000_000;
const BAD_CONTROLS = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/;
const BAD_ANY_CONTROLS = /[\u0000-\u001f\u007f-\u009f]/;
const FORBIDDEN_SNAPSHOT_KEY = /^(?:goalexpected|matched|success)$/i;

const BASE_TRUTH = ['evidence', 'forbidden_side_effects'];
const TRUTH_KEYS = Object.freeze({
  'SET-01': ['required_final_state', 'required_change_count'],
  'SET-02': ['required_final_state'],
  'SET-03': ['required_final_state', 'persistence'],
  'SET-04': ['required_final_state', 'persistence'],
  'SET-05': ['required_final_state', 'required_fresh_readback'],
  'SET-06': ['required_final_state', 'unchanged_controls'],
  'FORM-01': ['required_final_state'],
  'FORM-02': ['required_final_state', 'line_count'],
  'FORM-03': ['required_final_state'],
  'FORM-04': ['required_final_state'],
  'FORM-05': ['required_final_state'],
  'FORM-06': ['required_final_state', 'validation_state'],
  'FORM-07': ['required_final_state'],
  'LIST-01': ['required_selected_row'],
  'LIST-02': ['selected_context', 'unchanged_other_row'],
  'LIST-03': ['required_selected_row', 'target_position_after_20'],
  'LIST-04': ['required_final_state', 'row_identity_must_survive_reorder'],
  'MODAL-01': ['modal_open', 'background_unchanged'],
  'MODAL-02': ['modal_control', 'modal_remains_open'],
  'MODAL-03': ['modal_choice', 'background_controls_unchanged'],
});
const FINAL_STATE_KEYS = Object.freeze({
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
  'FORM-06': ['region_code'],
  'FORM-07': ['alert_threshold'],
  'LIST-04': ['target_status'],
});
const TASK_CHANGE_COUNT_KEYS = Object.freeze({
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
const ALLOWED_CHANGE_KEYS = Object.freeze({
  'SET-01': ['email_alerts'],
  'SET-02': ['weekly_digest'],
  'SET-03': ['include_archived_items'],
  'SET-04': ['quiet_hours'],
  'SET-05': ['compact_rows'],
  'SET-06': ['system_show_alerts'],
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
  'MODAL-01': ['modal_choice'],
  'MODAL-02': ['reminder_category'],
  'MODAL-03': ['modal_choice'],
});
const TASK_VALUE_KEYS = Object.freeze(Object.fromEntries(fixtureTaskIds.map(taskId => [
  taskId,
  [...(FINAL_STATE_KEYS[taskId] ?? []), ...(taskId === 'FORM-06' ? ['validation_state'] : [])],
])));

const REASON_ORDER = Object.freeze([
  'task_not_accepted',
  'scope_invalid',
  'scope_unverified',
  'trace_unverified',
  'system_not_completed',
  'current_ui_unverified',
  'unknown_receipt',
  'fresh_revisit_unverified',
  'snapshot_unknown',
  'condition_not_satisfied',
  'change_count_mismatch',
  'persistence_unverified',
  'persistence_not_satisfied',
  'side_effect_detected',
]);

function isRecord(value) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function assertRecord(value, label) {
  if (!isRecord(value)) throw new TypeError(`invalid ${label}`);
}

function assertAllowedKeys(value, allowed, label) {
  assertRecord(value, label);
  const unknown = Object.keys(value).filter(key => !allowed.includes(key));
  if (unknown.length > 0) throw new TypeError(`invalid ${label}`);
}

function assertExactKeys(value, keys, label) {
  assertAllowedKeys(value, keys, label);
  if (Object.keys(value).length !== keys.length || keys.some(key => !Object.hasOwn(value, key))) {
    throw new TypeError(`invalid ${label}`);
  }
}

function boundedString(value, maxBytes = MAX_TEXT_BYTES, allowNewlines = true) {
  return typeof value === 'string' && value.trim().length > 0
    && Buffer.byteLength(value, 'utf8') <= maxBytes
    && !(allowNewlines ? BAD_CONTROLS : BAD_ANY_CONTROLS).test(value);
}

function boundedId(value) {
  return boundedString(value, MAX_ID_BYTES, false);
}

function safeCounter(value, { positive = false, maximum = Number.MAX_SAFE_INTEGER } = {}) {
  return Number.isSafeInteger(value) && value >= (positive ? 1 : 0) && value <= maximum;
}

function scalar(value) {
  if (value === null || typeof value === 'boolean') return true;
  if (typeof value === 'string') return Buffer.byteLength(value, 'utf8') <= MAX_TEXT_BYTES && !BAD_CONTROLS.test(value);
  return typeof value === 'number' && Number.isFinite(value) && Number.isSafeInteger(value)
    && Math.abs(value) <= 1_000_000_000;
}

function hasOwn(record, key) {
  return isRecord(record) && Object.hasOwn(record, key);
}

function sameValue(left, right) {
  return Object.is(left, right);
}

function validateTruth(profileTruth) {
  assertExactKeys(profileTruth, ['suite', 'task_id', 'seed', 'evaluator_truth'], 'profile truth');
  if (profileTruth.suite !== 'fixture' || !fixtureTaskIds.includes(profileTruth.task_id)
    || !Number.isSafeInteger(profileTruth.seed) || !SEEDS.includes(profileTruth.seed)) {
    throw new TypeError('invalid fixture truth identity');
  }
  const expectedKeys = [...BASE_TRUTH, ...TRUTH_KEYS[profileTruth.task_id]];
  const truth = profileTruth.evaluator_truth;
  assertExactKeys(truth, expectedKeys, 'evaluator truth');
  if (JSON.stringify(truth.evidence) !== JSON.stringify(['independent_post_state', 'required_persistence_receipt_when_specified'])
    || JSON.stringify(truth.forbidden_side_effects) !== JSON.stringify(['unrelated_controls_changed', 'background_modal_controls_changed'])) {
    throw new TypeError('invalid evaluator truth metadata');
  }

  const finalKeys = FINAL_STATE_KEYS[profileTruth.task_id];
  if (finalKeys) {
    assertExactKeys(truth.required_final_state, finalKeys, 'required final state');
    for (const expected of Object.values(truth.required_final_state)) {
      if (!scalar(expected) || expected === null) throw new TypeError('invalid required final state');
    }
  }
  switch (profileTruth.task_id) {
    case 'SET-01':
      if (truth.required_change_count !== 0) throw new TypeError('invalid required change count');
      break;
    case 'SET-03':
      if (truth.persistence !== 'explicit_save_receipt') throw new TypeError('invalid persistence truth');
      break;
    case 'SET-04':
      if (truth.persistence !== 'autosave_completed_before_finish') throw new TypeError('invalid persistence truth');
      break;
    case 'SET-05':
      if (truth.required_fresh_readback !== true) throw new TypeError('invalid fresh readback truth');
      break;
    case 'SET-06':
      if (JSON.stringify(truth.unchanged_controls) !== JSON.stringify(['workspace_show_alerts'])) throw new TypeError('invalid unchanged controls');
      break;
    case 'FORM-02':
      if (truth.line_count !== 2) throw new TypeError('invalid line count truth');
      break;
    case 'FORM-06':
      if (truth.validation_state !== 'valid') throw new TypeError('invalid validation truth');
      break;
    case 'LIST-01':
    case 'LIST-03':
      if (!boundedId(truth.required_selected_row)) throw new TypeError('invalid selected row truth');
      if (profileTruth.task_id === 'LIST-03' && truth.target_position_after_20 !== true) throw new TypeError('invalid virtual target truth');
      break;
    case 'LIST-02':
      assertExactKeys(truth.selected_context, ['owner', 'status'], 'selected row context');
      if (!boundedString(truth.selected_context.owner, MAX_ID_BYTES, false)
        || !boundedString(truth.selected_context.status, MAX_ID_BYTES, false)
        || truth.unchanged_other_row !== true) throw new TypeError('invalid list context truth');
      break;
    case 'LIST-04':
      if (!boundedString(truth.required_final_state.target_status, MAX_ID_BYTES, false)
        || truth.row_identity_must_survive_reorder !== true) throw new TypeError('invalid reorder truth');
      break;
    case 'MODAL-01':
      if (truth.modal_open !== false || truth.background_unchanged !== true) throw new TypeError('invalid modal truth');
      break;
    case 'MODAL-02':
      assertExactKeys(truth.modal_control, ['reminder_category'], 'modal control truth');
      if (!boundedString(truth.modal_control.reminder_category, MAX_ID_BYTES, false)
        || truth.modal_remains_open !== true) throw new TypeError('invalid modal control truth');
      break;
    case 'MODAL-03':
      if (truth.modal_choice !== 'keep_current_selection' || truth.background_controls_unchanged !== true) {
        throw new TypeError('invalid modal choice truth');
      }
      break;
  }
  return { taskId: profileTruth.task_id, seed: profileTruth.seed, truth };
}

function rejectEvaluatorFields(value, depth = 0, budget = { count: 0 }) {
  if (depth > 16 || ++budget.count > 20_000) throw new TypeError('invalid app snapshot bounds');
  if (Array.isArray(value)) {
    for (const child of value) rejectEvaluatorFields(child, depth + 1, budget);
    return;
  }
  if (!isRecord(value)) return;
  for (const [key, child] of Object.entries(value)) {
    const compactKey = key.replace(/[_-]/g, '').toLowerCase();
    if (FORBIDDEN_SNAPSHOT_KEY.test(compactKey)) throw new TypeError('app snapshot contains evaluator fields');
    rejectEvaluatorFields(child, depth + 1, budget);
  }
}

function validateSnapshot(snapshot, taskId, seed) {
  if (snapshot === undefined || snapshot === null) return null;
  assertAllowedKeys(snapshot, ['version', 'task_id', 'seed', 'values', 'selected_row', 'modal', 'persistence', 'audit'], 'app snapshot');
  if ((snapshot.version != null && snapshot.version !== 1)
    || (snapshot.task_id != null && snapshot.task_id !== taskId)
    || (snapshot.seed != null && snapshot.seed !== seed)) {
    throw new TypeError('app snapshot identity mismatch');
  }
  rejectEvaluatorFields(snapshot);
  if (hasOwn(snapshot, 'values') && snapshot.values != null) {
    assertRecord(snapshot.values, 'snapshot values');
    if (Object.keys(snapshot.values).length > MAX_MAP_ENTRIES) throw new TypeError('invalid snapshot values bounds');
    for (const [key, value] of Object.entries(snapshot.values)) {
      if (!TASK_VALUE_KEYS[taskId].includes(key) || !scalar(value)) throw new TypeError('invalid snapshot value');
    }
  }
  if (hasOwn(snapshot, 'selected_row') && snapshot.selected_row !== null) {
    assertAllowedKeys(snapshot.selected_row, ['id', 'label', 'owner', 'status', 'position'], 'selected row');
    for (const key of ['id', 'label', 'owner', 'status']) {
      const value = snapshot.selected_row[key];
      if (value !== null && value !== undefined && !boundedString(value, MAX_ID_BYTES, false)) throw new TypeError('invalid selected row field');
    }
    const position = snapshot.selected_row.position;
    if (position !== null && position !== undefined && !safeCounter(position, { positive: true, maximum: 10_000_000 })) {
      throw new TypeError('invalid selected row position');
    }
  }
  if (hasOwn(snapshot, 'modal') && snapshot.modal != null) {
    assertAllowedKeys(snapshot.modal, ['open', 'choice', 'controls'], 'modal snapshot');
    if (snapshot.modal.open !== undefined && snapshot.modal.open !== null && typeof snapshot.modal.open !== 'boolean') {
      throw new TypeError('invalid modal open state');
    }
    if (snapshot.modal.choice !== undefined && snapshot.modal.choice !== null
      && !boundedString(snapshot.modal.choice, MAX_ID_BYTES, false)) throw new TypeError('invalid modal choice');
    if (hasOwn(snapshot.modal, 'controls') && snapshot.modal.controls != null) {
      validateScalarMap(snapshot.modal.controls, taskId === 'MODAL-02' ? ['reminder_category'] : [], 'modal controls');
    }
  }
  if (hasOwn(snapshot, 'persistence') && snapshot.persistence != null) validatePersistence(snapshot.persistence);
  if (hasOwn(snapshot, 'audit') && snapshot.audit != null) validateAudit(snapshot.audit, taskId);
  return snapshot;
}

function validateScalarMap(value, allowedKeys, label) {
  assertRecord(value, label);
  if (Object.keys(value).length > MAX_MAP_ENTRIES) throw new TypeError(`invalid ${label} bounds`);
  for (const [key, item] of Object.entries(value)) {
    if (!allowedKeys.includes(key) || !scalar(item)) throw new TypeError(`invalid ${label}`);
  }
}

function validatePersistence(value) {
  assertAllowedKeys(value, ['saved', 'kind', 'explicit_save_count', 'autosave_count', 'receipt'], 'persistence snapshot');
  if (value.saved !== undefined && value.saved !== null && typeof value.saved !== 'boolean') throw new TypeError('invalid persistence state');
  if (value.kind !== undefined && value.kind !== null && !['manual', 'automatic', 'none'].includes(value.kind)) {
    throw new TypeError('invalid persistence kind');
  }
  for (const key of ['explicit_save_count', 'autosave_count']) {
    if (value[key] !== undefined && value[key] !== null && !safeCounter(value[key], { maximum: MAX_COUNTER })) {
      throw new TypeError('invalid persistence count');
    }
  }
  if (hasOwn(value, 'receipt') && value.receipt !== null) {
    assertAllowedKeys(value.receipt, ['kind', 'revision'], 'persistence receipt');
    if (value.receipt.kind !== undefined && value.receipt.kind !== null
      && !['manual', 'automatic'].includes(value.receipt.kind)) throw new TypeError('invalid persistence receipt');
    if (value.receipt.revision !== undefined && value.receipt.revision !== null
      && !safeCounter(value.receipt.revision, { positive: true })) throw new TypeError('invalid persistence receipt');
  }
}

function validateAudit(value, taskId) {
  assertAllowedKeys(value, [
    'change_counts', 'background_change_count', 'other_row_change_count', 'revision', 'revisit_count', 'row_rebind_count',
  ], 'audit snapshot');
  if (hasOwn(value, 'change_counts') && value.change_counts != null) {
    assertRecord(value.change_counts, 'change counts');
    if (Object.keys(value.change_counts).length > MAX_MAP_ENTRIES) throw new TypeError('invalid change count bounds');
    if (Object.keys(value.change_counts).some(key => !TASK_CHANGE_COUNT_KEYS[taskId].includes(key))) {
      throw new TypeError('invalid task change count fields');
    }
    for (const [key, count] of Object.entries(value.change_counts)) {
      if (!/^[a-z][a-z0-9_]{0,63}$/.test(key)
        || (count !== null && !safeCounter(count, { maximum: MAX_COUNTER }))) {
        throw new TypeError('invalid change count');
      }
    }
  }
  for (const key of ['background_change_count', 'other_row_change_count', 'revisit_count', 'row_rebind_count']) {
    if (value[key] !== undefined && value[key] !== null && !safeCounter(value[key], { maximum: MAX_COUNTER })) {
      throw new TypeError('invalid audit counter');
    }
  }
  if (value.revision !== undefined && value.revision !== null && !safeCounter(value.revision)) {
    throw new TypeError('invalid audit revision');
  }
}

function checkValue(snapshot, key, expected, checks) {
  if (!hasOwn(snapshot.values, key) || snapshot.values[key] === null) {
    checks.unknown.add('snapshot_unknown');
    return;
  }
  if (!sameValue(snapshot.values[key], expected)) checks.failed.add('condition_not_satisfied');
}

function checkZeroAudit(snapshot, field, checks, reason) {
  const value = snapshot.audit?.[field];
  if (!safeCounter(value, { maximum: MAX_COUNTER })) checks.unknown.add('snapshot_unknown');
  else if (value !== 0) checks.failed.add(reason);
}

function checkUnrelatedChangeCounts(taskId, snapshot, checks) {
  const counts = snapshot.audit?.change_counts;
  if (!isRecord(counts)) {
    checks.unknown.add('snapshot_unknown');
    return;
  }
  for (const key of TASK_CHANGE_COUNT_KEYS[taskId]) {
    if (!hasOwn(counts, key) || !safeCounter(counts[key], { maximum: MAX_COUNTER })) {
      checks.unknown.add('snapshot_unknown');
    } else if (!ALLOWED_CHANGE_KEYS[taskId].includes(key) && counts[key] !== 0) {
      checks.failed.add('side_effect_detected');
    }
  }
}

function checkPersistence(snapshot, expectedKind, evidence, checks) {
  const persistence = snapshot.persistence;
  const receipt = persistence?.receipt;
  const expectedReceiptKind = expectedKind === 'explicit_save_receipt' ? 'manual' : 'automatic';
  const countKey = expectedReceiptKind === 'manual' ? 'explicit_save_count' : 'autosave_count';
  const count = persistence?.[countKey];
  const revision = snapshot.audit?.revision;
  if (evidence.unknown_receipt !== false) checks.unknown.add('unknown_receipt');
  if (persistence?.saved === false || persistence?.kind === 'none'
    || (persistence?.kind !== undefined && persistence.kind !== null && persistence.kind !== expectedReceiptKind)
    || (safeCounter(count, { maximum: MAX_COUNTER }) && count === 0)) checks.failed.add('persistence_not_satisfied');
  else if (persistence?.saved !== true || persistence.kind == null
    || !safeCounter(count, { positive: true, maximum: MAX_COUNTER })) checks.unknown.add('persistence_unverified');

  if (!isRecord(receipt)) {
    checks.unknown.add('persistence_unverified');
    return;
  }
  if (receipt.kind !== undefined && receipt.kind !== null && receipt.kind !== expectedReceiptKind) {
    checks.failed.add('persistence_not_satisfied');
  } else if (receipt.kind == null) checks.unknown.add('persistence_unverified');
  if (!safeCounter(revision, { positive: true }) || !safeCounter(receipt.revision, { positive: true })) {
    checks.unknown.add('persistence_unverified');
  } else if (receipt.revision !== revision) checks.failed.add('persistence_not_satisfied');
}

function addTruthChecks(taskId, truth, snapshot, evidence, checks) {
  if (!isRecord(snapshot.values) || !isRecord(snapshot.audit)) checks.unknown.add('snapshot_unknown');
  checkUnrelatedChangeCounts(taskId, snapshot, checks);
  const required = truth.required_final_state;
  if (required) for (const [key, expected] of Object.entries(required)) checkValue(snapshot, key, expected, checks);

  if (truth.required_change_count === 0) {
    const counts = snapshot.audit?.change_counts;
    const targetKey = Object.keys(truth.required_final_state)[0];
    if (!isRecord(counts) || !safeCounter(counts[targetKey], { maximum: MAX_COUNTER })) checks.unknown.add('snapshot_unknown');
    else if (counts[targetKey] !== 0) checks.failed.add('change_count_mismatch');
  }
  if (truth.persistence) checkPersistence(snapshot, truth.persistence, evidence, checks);
  if (truth.required_fresh_readback === true) {
    if (evidence.fresh_revisit_verified !== true || !safeCounter(snapshot.audit?.revisit_count, { positive: true, maximum: MAX_COUNTER })) {
      checks.unknown.add('fresh_revisit_unverified');
    }
  }
  if (Array.isArray(truth.unchanged_controls)) {
    const counts = snapshot.audit?.change_counts;
    for (const key of truth.unchanged_controls) {
      if (!hasOwn(snapshot.values, key) || snapshot.values[key] === null || !isRecord(counts) || !safeCounter(counts[key], { maximum: MAX_COUNTER })) {
        checks.unknown.add('snapshot_unknown');
      } else if (counts[key] !== 0) checks.failed.add('side_effect_detected');
    }
  }
  if (truth.line_count !== undefined) {
    const value = snapshot.values?.notes;
    if (typeof value !== 'string') checks.unknown.add('snapshot_unknown');
    else if (value.split(/\r\n|\n|\r/).length !== truth.line_count) checks.failed.add('condition_not_satisfied');
  }
  if (truth.validation_state !== undefined) checkValue(snapshot, 'validation_state', truth.validation_state, checks);

  if (truth.required_selected_row !== undefined) {
    const row = snapshot.selected_row;
    if (!isRecord(row) || !boundedId(row.id)) checks.unknown.add('snapshot_unknown');
    else if (row.id !== truth.required_selected_row) checks.failed.add('condition_not_satisfied');
  }
  if (truth.selected_context) {
    const row = snapshot.selected_row;
    if (!isRecord(row) || !boundedId(row.id) || !boundedString(row.owner, MAX_ID_BYTES, false)
      || !boundedString(row.status, MAX_ID_BYTES, false)) checks.unknown.add('snapshot_unknown');
    else if (row.owner !== truth.selected_context.owner || row.status !== truth.selected_context.status) {
      checks.failed.add('condition_not_satisfied');
    }
    checkZeroAudit(snapshot, 'other_row_change_count', checks, 'side_effect_detected');
  }
  if (truth.target_position_after_20 === true) {
    const row = snapshot.selected_row;
    if (!isRecord(row) || !boundedId(row.id) || !safeCounter(row.position, { positive: true, maximum: 10_000_000 })
      || !safeCounter(snapshot.audit?.row_rebind_count, { positive: true, maximum: MAX_COUNTER })) {
      checks.unknown.add('snapshot_unknown');
    } else if (row.id !== truth.required_selected_row || row.position <= 20) checks.failed.add('condition_not_satisfied');
  }
  if (truth.row_identity_must_survive_reorder === true) {
    const row = snapshot.selected_row;
    if (!isRecord(row) || !boundedId(row.id) || !safeCounter(row.position, { positive: true, maximum: 10_000_000 })
      || !safeCounter(snapshot.audit?.row_rebind_count, { positive: true, maximum: MAX_COUNTER })) {
      checks.unknown.add('snapshot_unknown');
    } else if (row.id !== 'record-03' || row.position !== 1) checks.failed.add('condition_not_satisfied');
    checkZeroAudit(snapshot, 'other_row_change_count', checks, 'side_effect_detected');
  }

  if (truth.modal_open !== undefined) checkModalOpen(snapshot, truth.modal_open, checks);
  if (truth.modal_remains_open === true) checkModalOpen(snapshot, true, checks);
  if (truth.modal_control) {
    for (const [key, expected] of Object.entries(truth.modal_control)) checkModalControl(snapshot, key, expected, checks);
  }
  if (truth.modal_choice !== undefined) {
    const choice = snapshot.modal?.choice;
    if (typeof choice !== 'string') checks.unknown.add('snapshot_unknown');
    else if (choice !== truth.modal_choice) checks.failed.add('condition_not_satisfied');
  }
  if (truth.background_unchanged === true || truth.background_controls_unchanged === true
    || truth.forbidden_side_effects.includes('background_modal_controls_changed')) {
    checkZeroAudit(snapshot, 'background_change_count', checks, 'side_effect_detected');
  }
  if (truth.unchanged_other_row === true) checkZeroAudit(snapshot, 'other_row_change_count', checks, 'side_effect_detected');
  if (taskId === 'MODAL-01' && truth.modal_open === false && snapshot.modal?.choice !== undefined
    && snapshot.modal.choice !== null && snapshot.modal.choice !== 'dismiss') {
    checks.failed.add('condition_not_satisfied');
  }
}

function checkModalOpen(snapshot, expected, checks) {
  const actual = snapshot.modal?.open;
  if (typeof actual !== 'boolean') checks.unknown.add('snapshot_unknown');
  else if (actual !== expected) checks.failed.add('condition_not_satisfied');
}

function checkModalControl(snapshot, key, expected, checks) {
  const controls = snapshot.modal?.controls;
  if (!hasOwn(controls, key) || controls[key] === null) checks.unknown.add('snapshot_unknown');
  else if (!sameValue(controls[key], expected)) checks.failed.add('condition_not_satisfied');
}

function validateEvidence(evidence, taskId, seed) {
  const allowed = ['version', 'task_id', 'seed', 'accepted', 'system_reported_completed', 'current_ui_verified',
    'fresh_revisit_verified', 'unknown_receipt', 'scope_valid', 'trace_validated'];
  if (evidence === undefined || evidence === null) return {};
  assertAllowedKeys(evidence, allowed, 'trace evidence');
  if ((evidence.version != null && evidence.version !== 1)
    || (evidence.task_id != null && evidence.task_id !== taskId)
    || (evidence.seed != null && evidence.seed !== seed)) {
    throw new TypeError('trace evidence identity mismatch');
  }
  for (const key of allowed.slice(3)) {
    if (evidence[key] != null && typeof evidence[key] !== 'boolean') throw new TypeError('invalid trace evidence');
  }
  return evidence;
}

function finalize(checks, taskId, seed) {
  const reasonCodes = REASON_ORDER.filter(code => checks.failed.has(code) || checks.unknown.has(code));
  const verdict = checks.failed.size > 0 ? 'not_satisfied' : checks.unknown.size > 0 ? 'unknown' : 'satisfied';
  return {
    version: 1,
    suite: 'fixture',
    task_id: taskId,
    seed,
    verdict,
    reason_codes: reasonCodes,
    artifact_only: true,
    evidence_claims_only: true,
    evidence_validity_audited: false,
  };
}

/** Offline, single-record evaluator. It never consumes online requests or model output. */
export function evaluateFixture({ profileTruth, appSnapshot, evidence } = {}) {
  const identity = validateTruth(profileTruth);
  const snapshot = validateSnapshot(appSnapshot, identity.taskId, identity.seed);
  const trace = validateEvidence(evidence, identity.taskId, identity.seed);
  const checks = { failed: new Set(), unknown: new Set() };
  const snapshotIdentityValid = snapshot?.version === 1 && snapshot.task_id === identity.taskId && snapshot.seed === identity.seed;
  const traceIdentityValid = trace.version === 1 && trace.task_id === identity.taskId && trace.seed === identity.seed;
  const traceClaimsValid = traceIdentityValid && trace.trace_validated === true;

  if (!traceClaimsValid) {
    checks.unknown.add('trace_unverified');
    checks.unknown.add('task_not_accepted');
    checks.unknown.add('system_not_completed');
    checks.unknown.add('current_ui_unverified');
    checks.unknown.add('unknown_receipt');
    checks.unknown.add('scope_unverified');
  } else {
    if (trace.trace_validated !== true) checks.unknown.add('trace_unverified');
    if (trace.accepted === false) checks.failed.add('task_not_accepted');
    else if (trace.accepted !== true) checks.unknown.add('task_not_accepted');
    if (trace.system_reported_completed === false) checks.failed.add('system_not_completed');
    else if (trace.system_reported_completed !== true) checks.unknown.add('system_not_completed');
    if (trace.current_ui_verified !== true) checks.unknown.add('current_ui_unverified');
    if (trace.unknown_receipt !== false) checks.unknown.add('unknown_receipt');
    if (trace.scope_valid === false && trace.trace_validated === true) checks.failed.add('scope_invalid');
    else if (trace.scope_valid !== true) checks.unknown.add('scope_unverified');
  }
  if (identity.truth.required_fresh_readback === true && (!traceClaimsValid || trace.fresh_revisit_verified !== true)) {
    checks.unknown.add('fresh_revisit_unverified');
  }
  if (!snapshotIdentityValid) checks.unknown.add('snapshot_unknown');

  if (traceClaimsValid && snapshotIdentityValid && trace.current_ui_verified === true
    && trace.accepted === true && trace.scope_valid === true) {
    addTruthChecks(identity.taskId, identity.truth, snapshot, trace, checks);
  }
  return finalize(checks, identity.taskId, identity.seed);
}

function readJsonFile(path) {
  const stat = statSync(path);
  if (!stat.isFile() || stat.size > MAX_JSON_BYTES) throw new TypeError('input JSON exceeds evaluator bounds');
  return JSON.parse(readFileSync(path, 'utf8'));
}

if (process.argv[1] && resolve(fileURLToPath(import.meta.url)) === resolve(process.argv[1])) {
  if (process.argv.length !== 5) {
    process.stderr.write('Usage: node scripts/mvp/fixture-evaluator.mjs <profile-truth.json> <app-snapshot.json> <trace-evidence.json>\n');
    process.exitCode = 2;
  } else {
    try {
      const result = evaluateFixture({
        profileTruth: readJsonFile(process.argv[2]),
        appSnapshot: readJsonFile(process.argv[3]),
        evidence: readJsonFile(process.argv[4]),
      });
      process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
    } catch {
      process.stderr.write('fixture evaluation input is invalid or incomplete\n');
      process.exitCode = 1;
    }
  }
}
