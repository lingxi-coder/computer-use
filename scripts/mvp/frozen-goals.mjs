import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { closeSync, fstatSync, fsyncSync, lstatSync, openSync, unlinkSync, writeSync } from 'node:fs';
import { isAbsolute, resolve } from 'node:path';
import { fixtureTaskIds, textEditTaskIds } from './business-results.mjs';

const FORMAT_VERSION = 1;
const TASK_PROFILE_VERSION = 1;
const FIXTURE_SEEDS = Object.freeze([1, 2, 3, 4, 5]);
const IDENTITY_COUNT = 120;
const FIXTURE_RUNS = 100;
const TEXTEDIT_RUNS = 20;
const SHA256 = /^[a-f0-9]{64}$/;
const MAX_REQUEST_BYTES = 16 * 1024 * 1024;
const MAX_FREEZE_BYTES = 1024 * 1024;
const FORBIDDEN_GOAL_KEY = /^(?:initial_state|evaluator(?:_truth)?|truth|oracle|expected(?:_|$)|driver_answer|confirmation_id|workflow|navigation|steps|selectors?|coordinates?|reset|environment|seed|task_id|suite)$/i;
const FORBIDDEN_PATH_KEY = /path|file|directory|folder|url|uri/i;
const PRIVATE_STATE_KEY = /^(?:evaluator(?:_truth)?|truth|oracle|expected_(?:result|final_state|value)|driver_answer|confirmation_id)$/i;

const asObject = (value, label) => {
  assert.ok(value && typeof value === 'object' && !Array.isArray(value), `${label} must be an object`);
  return value;
};

function exactKeys(value, expected, label) {
  asObject(value, label);
  assert.deepEqual(Object.keys(value).sort(), [...expected].sort(), `${label} has an unexpected shape`);
}

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`;
  }
  const serialized = JSON.stringify(value);
  assert.notEqual(serialized, undefined, 'unsupported JSON value');
  return serialized;
}

export function frozenGoalStableJson(value) { return `${canonical(value)}\n`; }

export function frozenGoalSha256(value) { return createHash('sha256').update(value).digest('hex'); }

const canonicalSha256 = value => frozenGoalSha256(frozenGoalStableJson(value));

function validateJsonTree(value, label, depth = 0) {
  assert.ok(depth <= 24, `${label} nesting exceeds the profile bound`);
  if (typeof value === 'string') {
    assert.ok(Buffer.byteLength(value, 'utf8') <= 32768
      && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/.test(value), `${label} string is invalid`);
  } else if (typeof value === 'number') assert.ok(Number.isFinite(value), `${label} number is invalid`);
  else if (Array.isArray(value)) {
    assert.ok(value.length <= 256, `${label} array exceeds the profile bound`);
    for (const child of value) validateJsonTree(child, label, depth + 1);
  } else if (value && typeof value === 'object') {
    assert.ok(Object.keys(value).length <= 128, `${label} object exceeds the profile bound`);
    for (const [key, child] of Object.entries(value)) {
      assert.ok(Buffer.byteLength(key, 'utf8') <= 256, `${label} key is too long`);
      validateJsonTree(child, label, depth + 1);
    }
  }
}

function rejectPrivateStateKeys(value, location = 'initial state') {
  if (Array.isArray(value)) {
    for (const child of value) rejectPrivateStateKeys(child, location);
  } else if (value && typeof value === 'object') {
    for (const [key, child] of Object.entries(value)) {
      assert.ok(!PRIVATE_STATE_KEY.test(key), `${location} contains evaluator-only material`);
      rejectPrivateStateKeys(child, location);
    }
  }
}

function parseCanonicalJson(bytes, label, maxBytes) {
  assert.ok(Buffer.isBuffer(bytes) && bytes.length > 0 && bytes.length <= maxBytes, `${label} bytes are missing or exceed the bound`);
  const value = JSON.parse(bytes.toString('utf8'));
  validateJsonTree(value, label);
  assert.deepEqual(Buffer.from(frozenGoalStableJson(value), 'utf8'), bytes, `${label} must use canonical JSON bytes`);
  return value;
}

function nonemptyString(value, label, maxBytes = 4096) {
  assert.equal(typeof value, 'string', `${label} must be a string`);
  assert.ok(value.trim().length > 0 && Buffer.byteLength(value, 'utf8') <= maxBytes
    && !/[\u0000-\u001f\u007f-\u009f]/.test(value), `${label} is invalid`);
  return value;
}

function validateObjective(value) {
  assert.equal(typeof value, 'string', 'goal objective must be a string');
  assert.ok(value.trim().length > 0 && Buffer.byteLength(value, 'utf8') <= 32768
    && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/.test(value), 'goal objective is invalid');
}

function rejectForbiddenGoalMaterial(value, location = 'goal') {
  if (Array.isArray(value)) {
    for (let index = 0; index < value.length; index += 1) rejectForbiddenGoalMaterial(value[index], `${location}[${index}]`);
    return;
  }
  if (!value || typeof value !== 'object') return;
  for (const [key, child] of Object.entries(value)) {
    assert.ok(!FORBIDDEN_GOAL_KEY.test(key), `${location} contains reset, evaluator, workflow, or identity material`);
    const allowedScopedPathKey = location === 'goal.allowed_scope'
      && (['file_name', 'output_directory'].includes(key) || (key === 'no_other_files' && child === true));
    if (FORBIDDEN_PATH_KEY.test(key) && !allowedScopedPathKey) {
      assert.fail(`${location}.${key} contains an unapproved path-like field`);
    }
    rejectForbiddenGoalMaterial(child, `${location}.${key}`);
  }
}

function expectedFixtureTargetKeys(taskId) {
  if (taskId.startsWith('SET-')) return taskId === 'SET-06' ? ['control', 'disambiguator', 'group'] : ['control', 'group'];
  if (taskId.startsWith('FORM-')) return ['control', 'form'];
  if (taskId === 'LIST-02') return ['context', 'list', 'target'];
  if (taskId.startsWith('LIST-')) return ['list', 'target'];
  if (taskId === 'MODAL-02') return ['control', 'modal'];
  if (taskId.startsWith('MODAL-')) return ['background', 'modal'];
  assert.fail(`unsupported frozen fixture identity ${taskId}`);
}

function validateFixtureGoalScope(scope, taskId, appName) {
  exactKeys(scope, ['application', 'no_unrelated_controls', 'targets'], 'fixture allowed scope');
  assert.equal(scope.application, appName, 'fixture scope application does not match frozen profile');
  assert.equal(scope.no_unrelated_controls, true, 'fixture scope must exclude unrelated controls');
  assert.ok(Array.isArray(scope.targets) && scope.targets.length === 1, 'fixture scope must contain exactly one target');
  const target = scope.targets[0];
  exactKeys(target, expectedFixtureTargetKeys(taskId), 'fixture target scope');
  for (const [key, value] of Object.entries(target)) nonemptyString(value, `fixture target ${key}`, 256);
}

function validateTextEditGoalScope(scope, taskId, textEditAppName, objective) {
  exactKeys(scope, ['application', 'document_format', 'file_name', 'no_other_files', 'operations', 'output_directory'], 'TextEdit allowed scope');
  assert.equal(scope.application, textEditAppName, 'TextEdit scope application does not match frozen profile');
  assert.equal(scope.document_format, 'plain_text', 'TextEdit scope format is not the frozen plain-text format');
  assert.equal(scope.no_other_files, true, 'TextEdit scope must exclude other files');
  const expectedFileName = `cu-mvp-${taskId.toLowerCase()}.txt`;
  assert.equal(scope.file_name, expectedFileName, 'TextEdit scope file name does not match frozen identity');
  nonemptyString(scope.output_directory, 'TextEdit output directory', 4096);
  assert.ok(isAbsolute(scope.output_directory) && !scope.output_directory.split(/[\\/]/).includes('..'), 'TextEdit output directory must be absolute and traversal-free');
  assert.deepEqual(scope.operations, ['write_requested_contents', 'save', 'close', 'reopen_and_read_back'], 'TextEdit operations differ from the frozen operation set');
  assert.ok(objective.includes(`${scope.output_directory}/${scope.file_name}`), 'TextEdit objective does not name the scoped output file');
}

function validateGoal(goal, suite, taskId, freeze) {
  exactKeys(goal, ['allowed_scope', 'objective'], 'projected task goal');
  validateObjective(goal.objective);
  rejectForbiddenGoalMaterial(goal);
  if (suite === 'fixture') validateFixtureGoalScope(goal.allowed_scope, taskId, freeze.environment.app.name);
  else validateTextEditGoalScope(goal.allowed_scope, taskId, freeze.environment.textedit_app.name, goal.objective);
}

function validateFreezeAndRequests(requestBytes, freezeBytes) {
  const requestExport = parseCanonicalJson(requestBytes, 'task request export', MAX_REQUEST_BYTES);
  const freeze = parseCanonicalJson(freezeBytes, 'task profile freeze', MAX_FREEZE_BYTES);
  exactKeys(requestExport, ['format_version', 'records', 'task_profile_version'], 'task request export');
  exactKeys(freeze, ['business_results', 'denominators', 'environment', 'environment_sha256', 'execution_started', 'export_hashes', 'format_version', 'freeze_content_sha256', 'plan_only', 'task_profile_version'], 'task profile freeze');
  assert.equal(requestExport.format_version, FORMAT_VERSION, 'task request export format is unsupported');
  assert.equal(requestExport.task_profile_version, TASK_PROFILE_VERSION, 'task request profile version is unsupported');
  assert.equal(freeze.format_version, FORMAT_VERSION, 'task profile freeze format is unsupported');
  assert.equal(freeze.task_profile_version, TASK_PROFILE_VERSION, 'task profile freeze version is unsupported');
  assert.equal(freeze.plan_only, true, 'task profile freeze must remain plan-only');
  assert.equal(freeze.execution_started, false, 'task profile freeze cannot claim execution');
  assert.equal(freeze.business_results, 'not_run', 'task profile freeze cannot contain business results');
  exactKeys(freeze.denominators, ['fixture_runs', 'fixture_seeds_per_task', 'fixture_tasks', 'textedit_runs', 'total_records'], 'frozen denominators');
  assert.deepEqual(freeze.denominators, {
    fixture_tasks: fixtureTaskIds.length,
    fixture_seeds_per_task: FIXTURE_SEEDS.length,
    fixture_runs: FIXTURE_RUNS,
    textedit_runs: TEXTEDIT_RUNS,
    total_records: IDENTITY_COUNT,
  }, 'frozen denominators differ from the complete task catalog');
  exactKeys(freeze.export_hashes, ['evaluator_truth_sha256', 'online_requests_sha256'], 'frozen export hashes');
  assert.ok(SHA256.test(freeze.export_hashes.online_requests_sha256), 'frozen request hash is invalid');
  assert.ok(SHA256.test(freeze.export_hashes.evaluator_truth_sha256), 'frozen evaluator hash marker is invalid');
  assert.equal(frozenGoalSha256(requestBytes), freeze.export_hashes.online_requests_sha256, 'task request bytes do not match the frozen request hash');
  assert.ok(SHA256.test(freeze.environment_sha256), 'frozen environment hash is invalid');
  assert.equal(canonicalSha256(freeze.environment), freeze.environment_sha256, 'frozen environment hash does not match');
  assert.ok(SHA256.test(freeze.freeze_content_sha256), 'freeze content hash is invalid');
  const { freeze_content_sha256: _hash, ...freezeContent } = freeze;
  assert.equal(canonicalSha256(freezeContent), freeze.freeze_content_sha256, 'freeze content hash does not match');
  exactKeys(freeze.environment, ['app', 'artifact_hashes', 'display', 'graph', 'hardware', 'knowledge', 'model', 'os', 'power_mode', 'preprocess', 'program', 'textedit_app', 'version'], 'frozen environment');
  exactKeys(freeze.environment.app, ['bundle_id', 'name', 'version'], 'frozen fixture application');
  exactKeys(freeze.environment.textedit_app, ['bundle_id', 'name', 'version'], 'frozen TextEdit application');
  nonemptyString(freeze.environment.app.name, 'fixture application name', 256);
  nonemptyString(freeze.environment.textedit_app.name, 'TextEdit application name', 256);
  assert.equal(freeze.environment.textedit_app.bundle_id, 'com.apple.TextEdit', 'frozen TextEdit bundle identifier is invalid');
  exactKeys(freeze.environment.knowledge, ['release', 'task_workflows_included'], 'frozen knowledge release');
  nonemptyString(freeze.environment.knowledge.release, 'frozen knowledge release', 256);
  assert.equal(freeze.environment.knowledge.task_workflows_included, false, 'frozen cold-start knowledge includes task workflows');
  assert.ok(freeze.environment.artifact_hashes && typeof freeze.environment.artifact_hashes === 'object'
    && !Array.isArray(freeze.environment.artifact_hashes), 'frozen artifact hashes are missing');
  for (const [name, hash] of Object.entries(freeze.environment.artifact_hashes)) {
    assert.ok(/^[a-z][a-z0-9_-]{0,63}$/.test(name) && SHA256.test(hash), 'frozen artifact hash entry is invalid');
  }

  assert.ok(Array.isArray(requestExport.records) && requestExport.records.length === IDENTITY_COUNT, 'frozen request export must contain all 120 identities');
  const expected = [
    ...fixtureTaskIds.flatMap(taskId => FIXTURE_SEEDS.map(seed => ['fixture', taskId, seed])),
    ...textEditTaskIds.map(taskId => ['textedit', taskId, 1]),
  ];
  for (let index = 0; index < expected.length; index += 1) {
    const record = requestExport.records[index];
    exactKeys(record, ['seed', 'suite', 'task_id', 'task_request'], `task request record ${index}`);
    assert.deepEqual([record.suite, record.task_id, record.seed], expected[index], `task request identity at index ${index} is not frozen`);
    exactKeys(record.task_request, ['allowed_scope', 'initial_state', 'objective'], `task request body ${index}`);
    assert.ok(record.task_request.initial_state && typeof record.task_request.initial_state === 'object'
      && !Array.isArray(record.task_request.initial_state), `task request reset record ${index} is malformed`);
    rejectPrivateStateKeys(record.task_request.initial_state);
  }
  return { requestExport, freeze };
}

function selectorFor(sourceFreezeSha256, suite, taskId, seed, goal) {
  const goalSha256 = frozenGoalSha256(Buffer.from(frozenGoalStableJson(goal), 'utf8'));
  const input = `computer-use-frozen-task-selector-v1\n${sourceFreezeSha256}\n${suite}\n${taskId}\n${seed}\n${goalSha256}`;
  return frozenGoalSha256(Buffer.from(input, 'utf8'));
}

/**
 * Produces the Main-readable catalog from the frozen request and freeze bytes.
 * It never opens evaluator-truth.json; only the freeze's syntactic hash marker
 * is checked. The projection callback must be the task-profile request projector.
 */
export function buildFrozenGoalCatalog({ requestBytes, freezeBytes, projectGoal }) {
  assert.equal(typeof projectGoal, 'function', 'trusted task goal projector is required');
  const { requestExport, freeze } = validateFreezeAndRequests(requestBytes, freezeBytes);
  const sourceFreezeSha256 = frozenGoalSha256(freezeBytes);
  const identities = [];
  const records = requestExport.records.map(record => {
    const goal = projectGoal(record);
    validateGoal(goal, record.suite, record.task_id, freeze);
    const selector = selectorFor(sourceFreezeSha256, record.suite, record.task_id, record.seed, goal);
    identities.push([record.suite, record.task_id, record.seed]);
    return {
      selector,
      suite: record.suite,
      task_id: record.task_id,
      seed: record.seed,
      goal,
    };
  });
  assert.equal(records.length, IDENTITY_COUNT, 'goal catalog lost frozen task identities');

  const taskGoals = { format_version: FORMAT_VERSION, task_profile_version: TASK_PROFILE_VERSION, records };
  const taskGoalsBytes = Buffer.from(frozenGoalStableJson(taskGoals), 'utf8');
  const manifest = {
    format_version: FORMAT_VERSION,
    task_profile_version: TASK_PROFILE_VERSION,
    source_freeze_sha256: sourceFreezeSha256,
    source_freeze_content_sha256: freeze.freeze_content_sha256,
    source_request_sha256: frozenGoalSha256(requestBytes),
    task_goals_sha256: frozenGoalSha256(taskGoalsBytes),
    identities_sha256: canonicalSha256(identities),
    identity_count: IDENTITY_COUNT,
    denominators: { fixture_runs: FIXTURE_RUNS, textedit_runs: TEXTEDIT_RUNS, total_records: IDENTITY_COUNT },
    selector_scheme: 'sha256/domain-separated-v1',
  };
  const manifestBytes = Buffer.from(frozenGoalStableJson(manifest), 'utf8');
  const files = {
    'task-goals.json': taskGoalsBytes,
    'goal-catalog-manifest.json': manifestBytes,
  };
  return {
    taskGoals,
    manifest,
    files,
    hashes: Object.fromEntries(Object.entries(files).map(([name, bytes]) => [name, frozenGoalSha256(bytes)])),
    catalogId: frozenGoalSha256(manifestBytes),
  };
}

/** Writes only goal-catalog artifacts to a new pair of filenames. */
export function writeFrozenGoalCatalog(outputDirectory, catalog) {
  assert.ok(typeof outputDirectory === 'string' && isAbsolute(outputDirectory), 'goal catalog output directory must be absolute');
  const outputStat = lstatSync(outputDirectory);
  assert.ok(outputStat.isDirectory() && !outputStat.isSymbolicLink(), 'goal catalog output must be a real directory');
  exactKeys(catalog?.files, ['goal-catalog-manifest.json', 'task-goals.json'], 'goal catalog files');
  const names = Object.keys(catalog.files).sort();
  for (const name of names) {
    assert.ok(Buffer.isBuffer(catalog.files[name]), `goal catalog ${name} bytes are invalid`);
    assert.equal(frozenGoalSha256(catalog.files[name]), catalog.hashes?.[name], `goal catalog ${name} bytes changed after validation`);
  }
  assert.equal(catalog.catalogId, frozenGoalSha256(catalog.files['goal-catalog-manifest.json']), 'goal catalog ID does not match its manifest bytes');
  const manifest = parseCanonicalJson(catalog.files['goal-catalog-manifest.json'], 'goal catalog manifest', MAX_FREEZE_BYTES);
  assert.equal(manifest.task_goals_sha256, frozenGoalSha256(catalog.files['task-goals.json']), 'goal catalog manifest does not bind goal bytes');
  for (const name of names) assert.throws(() => lstatSync(resolve(outputDirectory, name)), { code: 'ENOENT' }, 'goal catalog output already exists');
  const written = [];
  try {
    for (const name of names) {
      const target = resolve(outputDirectory, name);
      const fd = openSync(target, 'wx', 0o600);
      const identity = fstatSync(fd);
      written.push({ name, dev: identity.dev, ino: identity.ino });
      try {
        const bytes = catalog.files[name];
        let offset = 0;
        while (offset < bytes.length) {
          const count = writeSync(fd, bytes, offset, bytes.length - offset, offset);
          assert.ok(count > 0, 'goal catalog write made no progress');
          offset += count;
        }
        fsyncSync(fd);
      } finally { closeSync(fd); }
    }
  } catch (error) {
    for (const item of written.reverse()) {
      const target = resolve(outputDirectory, item.name);
      try {
        const stat = lstatSync(target);
        if (stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1
          && stat.dev === item.dev && stat.ino === item.ino) unlinkSync(target);
      } catch { /* Leave uncertain files for inspection. */ }
    }
    throw error;
  }
  return { files: names, hashes: { ...catalog.hashes }, catalog_id: catalog.catalogId, frozen_without_overwrite: true };
}
