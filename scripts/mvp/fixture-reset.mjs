import assert from 'node:assert/strict';
import { closeSync, constants, fstatSync, lstatSync, openSync, readSync, realpathSync } from 'node:fs';
import { isAbsolute } from 'node:path';
import { fixtureTaskIds, textEditTaskIds } from './business-results.mjs';
import { assignTaskVariants, canonicalSha256, projectTaskRequestForModel, sha256 } from './task-profiles.mjs';

export const FIXTURE_APPLICATION = Object.freeze({
  name: 'Interaction Fixture', bundle_id: 'com.lingxi.computer-user.mvp-fixture', version: '1.0.0',
});
const MAX_EXPORT_BYTES = 1024 * 1024;
const MAX_RESET_BYTES = 128 * 1024;
const HASH = /^[a-f0-9]{64}$/;
const FORBIDDEN = /^(?:evaluator(?:_truth)?|truth|oracle|expected_.*|navigation|steps|workflow|selectors?|coordinates?|driver_answer|__proto__|constructor|prototype)$/i;

function exact(value, keys, label) {
  assert.ok(value && typeof value === 'object' && !Array.isArray(value), `${label} must be an object`);
  assert.deepEqual(Object.keys(value).sort(), [...keys].sort(), `${label} has an unexpected shape`);
}

function validateTree(value, depth = 0) {
  assert.ok(depth <= 20, 'profile nesting exceeds bound');
  if (typeof value === 'string') {
    assert.ok(Buffer.byteLength(value) <= 32768 && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u.test(value), 'profile string is invalid');
  } else if (typeof value === 'number') assert.ok(Number.isFinite(value), 'profile number is invalid');
  else if (Array.isArray(value)) {
    assert.ok(value.length <= 256, 'profile array exceeds bound');
    value.forEach(child => validateTree(child, depth + 1));
  } else if (value && typeof value === 'object') {
    assert.ok(Object.keys(value).length <= 128, 'profile object exceeds bound');
    for (const [key, child] of Object.entries(value)) {
      assert.ok(!FORBIDDEN.test(key), 'driver profile contains evaluator or navigation material');
      validateTree(child, depth + 1);
    }
  }
}

function sameFile(a, b) {
  return a.dev === b.dev && a.ino === b.ino && a.size === b.size
    && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs;
}

function readBounded(path) {
  assert.ok(typeof path === 'string' && isAbsolute(path), 'profile file path must be absolute');
  const before = lstatSync(path);
  assert.ok(before.isFile() && !before.isSymbolicLink() && before.nlink === 1
    && before.size > 0 && before.size <= MAX_EXPORT_BYTES, 'profile must be a bounded regular file');
  assert.equal(realpathSync(path), path, 'profile path must not contain symbolic links');
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const opened = fstatSync(fd);
    assert.ok(sameFile(before, opened), 'profile changed before reading');
    const bytes = Buffer.alloc(opened.size);
    let offset = 0;
    while (offset < bytes.length) {
      const count = readSync(fd, bytes, offset, bytes.length - offset, offset);
      assert.ok(count > 0, 'profile ended early');
      offset += count;
    }
    assert.ok(sameFile(opened, fstatSync(fd)) && sameFile(opened, lstatSync(path)), 'profile changed during reading');
    return bytes;
  } finally { closeSync(fd); }
}

/** Driver-only reset. The app receives initial state; the model receives only the user goal. */
export function projectFixtureReset({ requestBytes, freezeBytes, taskId, seed }) {
  assert.ok(Buffer.isBuffer(requestBytes) && requestBytes.length > 0 && requestBytes.length <= MAX_EXPORT_BYTES, 'request export exceeds bound');
  assert.ok(Buffer.isBuffer(freezeBytes) && freezeBytes.length > 0 && freezeBytes.length <= MAX_EXPORT_BYTES, 'freeze export exceeds bound');
  assert.ok(fixtureTaskIds.includes(taskId) && Number.isInteger(seed) && seed >= 1 && seed <= 5, 'fixture identity is outside the frozen denominator');
  const freeze = JSON.parse(freezeBytes.toString('utf8'));
  exact(freeze, ['format_version', 'task_profile_version', 'plan_only', 'execution_started', 'business_results',
    'environment', 'environment_sha256', 'denominators', 'export_hashes', 'freeze_content_sha256'], 'freeze');
  assert.equal(freeze.format_version, 1);
  assert.equal(freeze.task_profile_version, 1);
  assert.equal(freeze.plan_only, true);
  assert.equal(freeze.execution_started, false);
  assert.equal(freeze.business_results, 'not_run');
  assert.deepEqual(freeze.denominators, { fixture_tasks: 20, fixture_seeds_per_task: 5, fixture_runs: 100, textedit_runs: 20, total_records: 120 });
  exact(freeze.export_hashes, ['online_requests_sha256', 'evaluator_truth_sha256'], 'export hashes');
  for (const hash of [...Object.values(freeze.export_hashes), freeze.environment_sha256, freeze.freeze_content_sha256]) assert.ok(HASH.test(hash), 'freeze hash is invalid');
  assert.equal(canonicalSha256(Object.fromEntries(Object.entries(freeze).filter(([key]) => key !== 'freeze_content_sha256'))), freeze.freeze_content_sha256, 'freeze content hash mismatch');
  assert.equal(canonicalSha256(freeze.environment), freeze.environment_sha256, 'environment hash mismatch');
  assert.deepEqual(freeze.environment.app, FIXTURE_APPLICATION, 'freeze targets a different fixture application');
  assert.equal(freeze.environment.textedit_app.bundle_id, 'com.apple.TextEdit');
  assert.equal(freeze.environment.knowledge.task_workflows_included, false);
  assert.equal(sha256(requestBytes), freeze.export_hashes.online_requests_sha256, 'request export hash mismatch');
  const requests = JSON.parse(requestBytes.toString('utf8'));
  exact(requests, ['format_version', 'task_profile_version', 'records'], 'request export');
  assert.equal(requests.format_version, 1);
  assert.equal(requests.task_profile_version, 1);
  assert.ok(Array.isArray(requests.records) && requests.records.length === 120, 'request export must preserve all 120 records');
  validateTree(requests);
  const expected = new Set([
    ...fixtureTaskIds.flatMap(id => [1, 2, 3, 4, 5].map(s => `fixture/${id}/${s}`)),
    ...textEditTaskIds.map(id => `textedit/${id}/1`),
  ]);
  let selected;
  for (const record of requests.records) {
    exact(record, ['suite', 'task_id', 'seed', 'task_request'], 'request record');
    const key = `${record.suite}/${record.task_id}/${record.seed}`;
    assert.ok(expected.delete(key), 'duplicate or unknown task identity');
    assert.ok(Number.isInteger(record.seed), 'record seed must be an integer');
    const modelGoal = projectTaskRequestForModel(record);
    const initial = record.task_request.initial_state;
    assert.ok(initial && typeof initial === 'object' && !Array.isArray(initial), 'initial state must be an object');
    if (record.suite === 'fixture') {
      assert.deepEqual(initial.application, FIXTURE_APPLICATION, 'reset targets a different fixture application');
      assert.deepEqual(initial.variant, assignTaskVariants(fixtureTaskIds.indexOf(record.task_id), record.seed), 'reset variant differs from the frozen assignment');
      exact(modelGoal.allowed_scope, ['application', 'targets', 'no_unrelated_controls'], 'fixture scope');
      assert.equal(modelGoal.allowed_scope.application, FIXTURE_APPLICATION.name);
      assert.equal(modelGoal.allowed_scope.no_unrelated_controls, true);
      assert.ok(Array.isArray(modelGoal.allowed_scope.targets) && modelGoal.allowed_scope.targets.length === 1, 'fixture requires one bounded target scope');
      if (record.task_id === taskId && record.seed === seed) selected = { record, modelGoal };
    } else assert.deepEqual(initial.application, freeze.environment.textedit_app, 'TextEdit reset application mismatch');
  }
  assert.equal(expected.size, 0, 'frozen denominator is incomplete');
  assert.ok(selected, 'selected fixture profile is missing');
  const resetProfile = { version: 1, task_id: taskId, seed, initial_state: structuredClone(selected.record.task_request.initial_state) };
  assert.ok(Buffer.byteLength(JSON.stringify(resetProfile)) <= MAX_RESET_BYTES, 'fixture reset exceeds app input bound');
  return {
    resetProfile,
    modelGoal: selected.modelGoal,
    pins: { request_sha256: sha256(requestBytes), freeze_sha256: sha256(freezeBytes), freeze_content_sha256: freeze.freeze_content_sha256 },
  };
}

export function loadFixtureReset({ requestFile, freezeFile, taskId, seed }) {
  return projectFixtureReset({ requestBytes: readBounded(requestFile), freezeBytes: readBounded(freezeFile), taskId, seed });
}
