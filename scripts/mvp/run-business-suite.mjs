#!/usr/bin/env node
import assert from 'node:assert/strict';
import {
  closeSync, constants, existsSync, fstatSync, lstatSync, mkdirSync, openSync, readSync, realpathSync,
  statSync, unlinkSync, writeSync,
} from 'node:fs';
import { spawn } from 'node:child_process';
import { basename, dirname, isAbsolute, relative, resolve, sep } from 'node:path';
import { parseArgs } from 'node:util';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import {
  createOwnedTempScope, createProcessGroupWitness, probeOwnedResources,
} from './owned-resource-probe.mjs';

import { fixtureTaskIds, summarizeBusinessResults, textEditTaskIds } from './business-results.mjs';
import { assignTaskVariants, canonicalSha256, projectTaskRequestForModel, sha256 } from './task-profiles.mjs';
import {
  deriveRunEvidenceValidity, hasAcceptedModelAttribution, summarizeModelAttribution, toBusinessResultRun, validateDriverReceipt, validateEvaluatorResult,
} from './business-run-receipt.mjs';

const FORMAT_VERSION = 1;
const MAX_INPUT_BYTES = 8 * 1024 * 1024;
const MAX_RECEIPT_BYTES = 4 * 1024 * 1024;
const MAX_EVALUATOR_RESULT_BYTES = 1024 * 1024;
const MAX_EVIDENCE_BYTES = 128 * 1024 * 1024;
const MAX_LOG_BYTES = 32 * 1024;
const MAX_TEXT_BYTES = 16_384;
const DEFAULT_CHILD_TIMEOUT_MS = 30 * 60 * 1000;
const DEFAULT_TERM_GRACE_MS = 1500;
const DEFAULT_KILL_WAIT_MS = 4000;
const HASH = /^[a-f0-9]{64}$/;
const BASE_ENV_KEYS = ['HOME', 'LANG', 'LC_ALL', 'LC_CTYPE', 'LOGNAME', 'PATH', 'SHELL', 'TERM', 'TMPDIR', 'TMP', 'TEMP', 'USER'];
const TRUTH_ENV_NAME = /(?:TRUTH|EVALUATOR|ORACLE)/i;
const RESERVED_DRIVER_ARGUMENT = /(?:evaluator-truth|truth-file|oracle-file|task-requests\.json|task-profile-freeze\.json|evaluator\.json)/i;
const DRIVER_RESERVED_OPTION = /^--(?:suite|task-id|seed|request|launcher-reset-file|output-dir|receipt)(?:=|$)/i;
const EVALUATOR_RESERVED_OPTION = /^--(?:suite|task-id|seed|truth-file|driver-receipt-file|evaluation-input-file|run-directory|driver-phase-complete-file|result-file)(?:=|$)/i;

class SuiteError extends Error {
  constructor(code, message = code) {
    super(message);
    this.name = 'SuiteError';
    this.code = code;
  }
}

function plainRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
}

function exactKeys(value, keys, label) {
  assert.ok(plainRecord(value), `${label} must be an object`);
  assert.deepEqual(Object.keys(value).sort(), [...keys].sort(), `${label} has an unexpected shape`);
}

function assertAbsolutePath(path, label) {
  assert.equal(typeof path, 'string', `${label} is required`);
  assert.ok(isAbsolute(path) && resolve(path) === path, `${label} must be a canonical absolute path`);
}

function sameFile(left, right) {
  return left.dev === right.dev && left.ino === right.ino && left.size === right.size
    && left.mtimeMs === right.mtimeMs && left.ctimeMs === right.ctimeMs;
}

function assertRegular(path, label, maxBytes, { executable = false, privateFile = false } = {}) {
  assertAbsolutePath(path, label);
  const stat = lstatSync(path);
  assert.ok(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1, `${label} must be a single regular file`);
  assert.ok(stat.size > 0 && stat.size <= maxBytes, `${label} size is invalid`);
  assert.equal(realpathSync(path), path, `${label} path must not contain symlinks`);
  if (privateFile && typeof process.getuid === 'function') assert.equal(stat.uid, process.getuid(), `${label} owner differs`);
  if (privateFile) assert.equal(stat.mode & 0o077, 0, `${label} must be private`);
  if (executable) assert.ok((stat.mode & 0o111) !== 0, `${label} is not executable`);
  return stat;
}

function readStable(path, label, maxBytes = MAX_INPUT_BYTES, { privateFile = false } = {}) {
  const before = assertRegular(path, label, maxBytes, { privateFile });
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const opened = fstatSync(fd);
    assert.ok(sameFile(before, opened), `${label} changed before reading`);
    const data = Buffer.alloc(opened.size);
    let offset = 0;
    while (offset < data.length) {
      const count = readSync(fd, data, offset, data.length - offset, offset);
      assert.ok(count > 0, `${label} ended early`);
      offset += count;
    }
    assert.ok(sameFile(opened, fstatSync(fd)) && sameFile(opened, lstatSync(path)), `${label} changed while reading`);
    return data;
  } finally {
    closeSync(fd);
  }
}

function hashStable(path, label, maxBytes = MAX_INPUT_BYTES, { privateFile = false } = {}) {
  const before = assertRegular(path, label, maxBytes, { privateFile });
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const opened = fstatSync(fd);
    assert.ok(sameFile(before, opened), `${label} changed before hashing`);
    const digest = createHash('sha256');
    const chunk = Buffer.alloc(64 * 1024);
    let offset = 0;
    while (offset < opened.size) {
      const count = readSync(fd, chunk, 0, Math.min(chunk.length, opened.size - offset), offset);
      assert.ok(count > 0, `${label} ended early`);
      digest.update(chunk.subarray(0, count));
      offset += count;
    }
    assert.ok(offset === opened.size && sameFile(opened, fstatSync(fd)) && sameFile(opened, lstatSync(path)), `${label} changed while hashing`);
    return digest.digest('hex');
  } finally {
    closeSync(fd);
  }
}

function parseJson(data, label) {
  try { return JSON.parse(data.toString('utf8')); }
  catch { throw new SuiteError('INVALID_JSON', `${label} is not valid JSON`); }
}

function rejectPlaceholderStrings(value, location = 'environment') {
  if (typeof value === 'string') {
    assert.ok(!/^<.+>$/.test(value.trim())
      && !['synthetic-startup-only', 'local-evaluator-model', 'test-fixture-placeholder'].includes(value.toLowerCase()),
    `${location} contains a placeholder value`);
  } else if (Array.isArray(value)) {
    value.forEach((child, index) => rejectPlaceholderStrings(child, `${location}[${index}]`));
  } else if (plainRecord(value)) {
    for (const [key, child] of Object.entries(value)) rejectPlaceholderStrings(child, `${location}.${key}`);
  }
}

function assertNoPrivateTruthKeys(value, location = 'reset input') {
  if (Array.isArray(value)) {
    value.forEach((child, index) => assertNoPrivateTruthKeys(child, `${location}[${index}]`));
    return;
  }
  if (!plainRecord(value)) return;
  for (const [key, child] of Object.entries(value)) {
    assert.ok(!/^(?:evaluator(?:_truth)?|truth|oracle|expected_(?:result|final_state|value)|navigation|steps|workflow|selectors?|coordinates?)$/i.test(key),
      `${location} contains evaluator or navigation data`);
    assertNoPrivateTruthKeys(child, `${location}.${key}`);
  }
}

function validateVariant(variant, expected, label) {
  exactKeys(variant, ['language', 'theme', 'layout'], label);
  assert.ok(['en-US', 'zh-CN'].includes(variant.language), `${label}.language is invalid`);
  assert.ok(['light', 'dark'].includes(variant.theme), `${label}.theme is invalid`);
  assert.ok(['compact', 'standard', 'wide', 'two_column'].includes(variant.layout), `${label}.layout is invalid`);
  if (expected) assert.deepEqual(variant, expected, `${label} differs from its frozen assignment`);
}

function expectedIdentities() {
  return [
    ...fixtureTaskIds.flatMap(task_id => [1, 2, 3, 4, 5].map(seed => ({ suite: 'fixture', task_id, seed }))),
    ...textEditTaskIds.map(task_id => ({ suite: 'textedit', task_id, seed: 1 })),
  ];
}

function identityKey(identity) {
  return `${identity.suite}/${identity.task_id}/${identity.seed}`;
}

function validateEnvironment(environment) {
  exactKeys(environment, [
    'version', 'os', 'display', 'power_mode', 'hardware', 'app', 'textedit_app', 'model', 'graph',
    'preprocess', 'program', 'knowledge', 'artifact_hashes',
  ], 'frozen environment');
  assert.equal(environment.version, 1, 'environment version must be 1');
  exactKeys(environment.os, ['name', 'build'], 'environment OS');
  assert.equal(environment.os.name, 'macOS', 'suite requires the frozen macOS environment');
  assert.ok(typeof environment.os.build === 'string' && environment.os.build.trim(), 'macOS build is missing');
  exactKeys(environment.display, ['width_px', 'height_px', 'dpi_x', 'dpi_y'], 'environment display');
  for (const key of ['width_px', 'height_px']) assert.ok(Number.isSafeInteger(environment.display[key]) && environment.display[key] > 0, `display ${key} is invalid`);
  for (const key of ['dpi_x', 'dpi_y']) assert.ok(Number.isFinite(environment.display[key]) && environment.display[key] > 0, `display ${key} is invalid`);
  assert.ok(typeof environment.power_mode === 'string' && environment.power_mode.trim(), 'power mode is missing');
  exactKeys(environment.hardware, ['model', 'architecture', 'memory_gib'], 'environment hardware');
  assert.equal(environment.hardware.model, 'Apple M2 Max', 'suite requires the §26.7 reference hardware');
  assert.equal(environment.hardware.architecture, 'arm64', 'suite requires arm64 reference hardware');
  assert.equal(environment.hardware.memory_gib, 64, 'suite requires 64 GiB reference memory');
  exactKeys(environment.app, ['name', 'bundle_id', 'version'], 'fixture application');
  assert.equal(environment.app.bundle_id, 'com.lingxi.computer-user.mvp-fixture', 'fixture bundle ID differs');
  exactKeys(environment.textedit_app, ['name', 'bundle_id', 'version'], 'TextEdit application');
  assert.equal(environment.textedit_app.bundle_id, 'com.apple.TextEdit', 'TextEdit bundle ID differs');
  for (const [label, record] of [['fixture application', environment.app], ['TextEdit application', environment.textedit_app]]) {
    for (const [key, value] of Object.entries(record)) assert.ok(typeof value === 'string' && value.trim(), `${label}.${key} is missing`);
  }
  exactKeys(environment.knowledge, ['release', 'task_workflows_included'], 'knowledge release');
  assert.ok(typeof environment.knowledge.release === 'string' && environment.knowledge.release.trim(), 'knowledge release is missing');
  assert.equal(environment.knowledge.task_workflows_included, false, 'cold-start release includes task workflows');
  exactKeys(environment.model, ['name', 'version'], 'model release');
  assert.ok(typeof environment.model.name === 'string' && environment.model.name.trim()
    && typeof environment.model.version === 'string' && environment.model.version.trim(), 'model release is incomplete');
  for (const key of ['graph', 'preprocess', 'program']) {
    exactKeys(environment[key], ['release'], `${key} release`);
    assert.ok(typeof environment[key].release === 'string' && environment[key].release.trim(), `${key} release is missing`);
  }
  assert.ok(plainRecord(environment.artifact_hashes), 'artifact hashes are missing');
  const requiredAssets = ['app', 'textedit_app', 'model', 'graph', 'preprocess', 'program', 'knowledge'];
  const assetNames = Object.keys(environment.artifact_hashes);
  assert.ok(assetNames.length >= requiredAssets.length && assetNames.length <= 128
    && assetNames.every(name => /^[a-z][a-z0-9_-]{0,63}$/.test(name)), 'artifact hash names are invalid');
  for (const name of requiredAssets) assert.ok(HASH.test(environment.artifact_hashes[name] ?? ''), `frozen ${name} hash is invalid`);
  for (const value of Object.values(environment.artifact_hashes)) assert.ok(HASH.test(value), 'frozen artifact hash is invalid');
  rejectPlaceholderStrings(environment);
}

function validateFreeze(freeze, requestBytes, truthHash) {
  exactKeys(freeze, [
    'format_version', 'task_profile_version', 'plan_only', 'execution_started', 'business_results', 'environment',
    'environment_sha256', 'denominators', 'export_hashes', 'freeze_content_sha256',
  ], 'task-profile freeze');
  assert.equal(freeze.format_version, FORMAT_VERSION, 'freeze version is invalid');
  assert.equal(freeze.task_profile_version, 1, 'task profile version is invalid');
  assert.equal(freeze.plan_only, true, 'source freeze must be a pre-run plan');
  assert.equal(freeze.execution_started, false, 'source freeze already records execution');
  assert.equal(freeze.business_results, 'not_run', 'source freeze already records business results');
  exactKeys(freeze.denominators, ['fixture_tasks', 'fixture_seeds_per_task', 'fixture_runs', 'textedit_runs', 'total_records'], 'denominators');
  assert.deepEqual(freeze.denominators, {
    fixture_tasks: 20, fixture_seeds_per_task: 5, fixture_runs: 100, textedit_runs: 20, total_records: 120,
  }, 'freeze must contain the exact §26.7 denominator');
  validateEnvironment(freeze.environment);
  assert.equal(canonicalSha256(freeze.environment), freeze.environment_sha256, 'frozen environment hash mismatch');
  exactKeys(freeze.export_hashes, ['online_requests_sha256', 'evaluator_truth_sha256'], 'freeze export hashes');
  assert.equal(sha256(requestBytes), freeze.export_hashes.online_requests_sha256, 'request export hash differs from freeze');
  assert.equal(truthHash, freeze.export_hashes.evaluator_truth_sha256, 'evaluator truth hash differs from freeze');
  const { freeze_content_sha256: ignored, ...freezeBase } = freeze;
  assert.equal(canonicalSha256(freezeBase), freeze.freeze_content_sha256, 'freeze content hash mismatch');
}

function validateRequestExport(requests, environment) {
  exactKeys(requests, ['format_version', 'task_profile_version', 'records'], 'task request export');
  assert.equal(requests.format_version, FORMAT_VERSION, 'request export version is invalid');
  assert.equal(requests.task_profile_version, 1, 'request profile version is invalid');
  assert.ok(Array.isArray(requests.records) && requests.records.length === 120, 'request export must have exactly 120 records');
  const expected = expectedIdentities();
  const expectedByKey = new Map(expected.map(identity => [identityKey(identity), identity]));
  const records = new Map();
  for (const [index, record] of requests.records.entries()) {
    exactKeys(record, ['suite', 'task_id', 'seed', 'task_request'], `request record ${index}`);
    const identity = { suite: record.suite, task_id: record.task_id, seed: record.seed };
    const key = identityKey(identity);
    assert.ok(expectedByKey.has(key), `request record is outside the frozen denominator: ${key}`);
    assert.ok(!records.has(key), `duplicate request identity: ${key}`);
    const goal = projectTaskRequestForModel(record);
    assert.deepEqual(Object.keys(goal).sort(), ['allowed_scope', 'objective'], `model projection for ${key} has extra fields`);
    const initialState = record.task_request.initial_state;
    assert.ok(plainRecord(initialState), `initial state is missing for ${key}`);
    assertNoPrivateTruthKeys(initialState, `initial state for ${key}`);
    const expectedApplication = identity.suite === 'fixture' ? environment.app : environment.textedit_app;
    if (identity.suite === 'fixture') {
      assert.deepEqual(initialState.application, expectedApplication, `fixture app differs for ${key}`);
      validateVariant(initialState.variant, assignTaskVariants(fixtureTaskIds.indexOf(identity.task_id), identity.seed), `fixture variant for ${key}`);
      exactKeys(goal.allowed_scope, ['application', 'targets', 'no_unrelated_controls'], `fixture scope for ${key}`);
      assert.equal(goal.allowed_scope.application, expectedApplication.name, `fixture scope app differs for ${key}`);
      assert.equal(goal.allowed_scope.no_unrelated_controls, true, `fixture scope permits unrelated controls for ${key}`);
      assert.ok(Array.isArray(goal.allowed_scope.targets) && goal.allowed_scope.targets.length === 1, `fixture scope must have one target for ${key}`);
    } else {
      assert.deepEqual(initialState.application, expectedApplication, `TextEdit app differs for ${key}`);
      validateVariant(initialState.variant, null, `TextEdit variant for ${key}`);
      exactKeys(goal.allowed_scope, ['application', 'output_directory', 'file_name', 'document_format', 'operations', 'no_other_files'], `TextEdit scope for ${key}`);
      assert.equal(goal.allowed_scope.application, expectedApplication.name, `TextEdit scope app differs for ${key}`);
      assertAbsolutePath(goal.allowed_scope.output_directory, `TextEdit output directory for ${key}`);
      assert.equal(goal.allowed_scope.file_name, `cu-mvp-${identity.task_id.toLowerCase()}.txt`, `TextEdit filename differs for ${key}`);
      assert.equal(goal.allowed_scope.document_format, 'plain_text', `TextEdit format differs for ${key}`);
      assert.deepEqual(goal.allowed_scope.operations, ['write_requested_contents', 'save', 'close', 'reopen_and_read_back'], `TextEdit operations differ for ${key}`);
      assert.equal(goal.allowed_scope.no_other_files, true, `TextEdit scope permits unrelated files for ${key}`);
      assert.equal(initialState.output_directory, goal.allowed_scope.output_directory, `TextEdit reset directory differs for ${key}`);
      assert.equal(initialState.file_name, goal.allowed_scope.file_name, `TextEdit reset filename differs for ${key}`);
      assert.equal(initialState.document_format, 'plain_text', `TextEdit reset format differs for ${key}`);
      const existing = Number(identity.task_id.slice(-2)) >= 11;
      assert.equal(initialState.file_state, existing ? 'existing_disposable_test_file' : 'target_file_absent', `TextEdit initial file state differs for ${key}`);
      assert.equal(initialState.document_state, existing ? 'existing_file_open' : 'new_untitled_document', `TextEdit document state differs for ${key}`);
      assert.equal(initialState.save_dialog_variant, existing ? 'existing_file_overwrite_confirmation' : 'first_save_native_dialog', `TextEdit dialog variant differs for ${key}`);
      assert.equal(typeof initialState.initial_contents === 'string', existing, `TextEdit initial content differs for ${key}`);
    }
    records.set(key, { identity, record, goal });
  }
  assert.equal(records.size, 120, 'request export is missing frozen identities');
  for (const identity of expected) assert.ok(records.has(identityKey(identity)), `missing request ${identityKey(identity)}`);
  return { expected, records };
}

function validateTruthIdentityEnvelope(truth, expected) {
  exactKeys(truth, ['format_version', 'task_profile_version', 'audience', 'not_for_online_model', 'records'], 'evaluator truth export');
  assert.equal(truth.format_version, FORMAT_VERSION, 'truth export version is invalid');
  assert.equal(truth.task_profile_version, 1, 'truth profile version is invalid');
  assert.equal(truth.audience, 'independent_evaluator_only', 'truth audience is invalid');
  assert.equal(truth.not_for_online_model, true, 'truth export is not marked evaluator-only');
  assert.ok(Array.isArray(truth.records) && truth.records.length === expected.length, 'truth export has an invalid record count');
  const expectedKeys = new Set(expected.map(identityKey));
  const seen = new Set();
  const records = new Map();
  for (const [index, record] of truth.records.entries()) {
    exactKeys(record, ['suite', 'task_id', 'seed', 'evaluator_truth'], `truth record ${index}`);
    const key = identityKey(record);
    assert.ok(expectedKeys.has(key) && !seen.has(key), `truth identity is unknown or duplicated: ${key}`);
    assert.ok(plainRecord(record.evaluator_truth), `truth predicate is missing for ${key}`);
    seen.add(key);
    records.set(key, record);
  }
  assert.equal(seen.size, expected.length, 'truth export is missing identities');
  return records;
}

export function validateFrozenSuiteFiles({ requestFile, freezeFile, truthFile }) {
  const requestBytes = readStable(requestFile, 'request file', MAX_INPUT_BYTES, { privateFile: true });
  const freezeBytes = readStable(freezeFile, 'freeze file', MAX_INPUT_BYTES, { privateFile: true });
  const truthHash = hashStable(truthFile, 'evaluator truth file', MAX_INPUT_BYTES, { privateFile: true });
  const requests = parseJson(requestBytes, 'request file');
  const freeze = parseJson(freezeBytes, 'freeze file');
  validateFreeze(freeze, requestBytes, truthHash);
  const { expected, records } = validateRequestExport(requests, freeze.environment);
  return {
    expected,
    records,
    requests,
    freeze,
    request_sha256: sha256(requestBytes),
    freeze_sha256: sha256(freezeBytes),
    truth_sha256: truthHash,
    truth_file: truthFile,
  };
}

function validateExecutable(path, label) {
  const stat = assertRegular(path, label, 1024 * 1024 * 1024, { executable: true });
  return { path, sha256: hashStable(path, label, 1024 * 1024 * 1024), size: stat.size };
}

function validateCommandArtifactClosure({ manifestFile, kind, executable, args, frozenProductDecisionProgramSha256 }) {
  const manifestBytes = readStable(manifestFile, `${kind} command manifest`, MAX_INPUT_BYTES, { privateFile: true });
  const manifest = parseJson(manifestBytes, `${kind} command manifest`);
  exactKeys(manifest, [
    'version', 'kind', 'frozen_product_decision_program_sha256', 'launch_executable_sha256',
    'launch_argv_prefix', 'declared_artifacts',
  ], `${kind} command manifest`);
  assert.equal(manifest.version, FORMAT_VERSION, `${kind} command manifest version is invalid`);
  assert.equal(manifest.kind, kind, `${kind} command manifest kind differs`);
  assert.equal(manifest.frozen_product_decision_program_sha256, kind === 'driver' ? frozenProductDecisionProgramSha256 : null,
    `${kind} command manifest DecisionProgram binding differs from the frozen product hash`);
  assert.equal(manifest.launch_executable_sha256, executable.sha256, `${kind} launch executable digest differs`);
  assert.ok(Array.isArray(manifest.launch_argv_prefix) && manifest.launch_argv_prefix.length <= 256
    && manifest.launch_argv_prefix.every(argument => typeof argument === 'string'), `${kind} launch argv prefix is invalid`);
  assert.deepEqual(manifest.launch_argv_prefix, args, `${kind} launch argv prefix differs from the declared command`);
  assert.ok(Array.isArray(manifest.declared_artifacts) && manifest.declared_artifacts.length > 0 && manifest.declared_artifacts.length <= 256,
    `${kind} declared command artifact set is missing`);
  const seenPaths = new Set();
  let entry = null;
  const artifacts = manifest.declared_artifacts.map((artifact, index) => {
    exactKeys(artifact, ['role', 'path', 'sha256'], `${kind} command artifact ${index}`);
    assert.ok(['entry', 'script', 'import'].includes(artifact.role), `${kind} command artifact ${index} role is invalid`);
    assertAbsolutePath(artifact.path, `${kind} command artifact ${index} path`);
    assert.ok(!seenPaths.has(artifact.path), `${kind} command artifact path is duplicated`);
    seenPaths.add(artifact.path);
    assert.ok(HASH.test(artifact.sha256), `${kind} command artifact ${index} digest is invalid`);
    const actualSha256 = hashStable(artifact.path, `${kind} command artifact ${index}`, 1024 * 1024 * 1024);
    assert.equal(actualSha256, artifact.sha256, `${kind} command artifact ${index} digest mismatch`);
    if (artifact.role === 'entry') {
      assert.equal(entry, null, `${kind} command manifest must name exactly one entry artifact`);
      entry = artifact.path;
    }
    return { role: artifact.role, path: artifact.path, sha256: artifact.sha256 };
  });
  assert.ok(entry, `${kind} command manifest must name one entry artifact`);
  const executableName = basename(executable.path).toLowerCase();
  const interpreter = /^(?:node(?:js)?(?:[-.]?\d+(?:\.\d+)*)?|python(?:\d+(?:\.\d+)*)?|ruby|perl|bash|sh|zsh|deno|bun)(?:\.exe)?$/.test(executableName);
  if (interpreter) {
    assert.equal(args[0], entry, `${kind} interpreter entry must be argv[0]; startup code options are not allowed before it`);
  } else {
    assert.equal(entry, executable.path, `${kind} non-interpreter entry must be the launched executable`);
  }
  return {
    manifestFile,
    manifestSha256: sha256(manifestBytes),
    kind,
    frozenProductDecisionProgramSha256: manifest.frozen_product_decision_program_sha256,
    launchExecutableSha256: manifest.launch_executable_sha256,
    launchArgvPrefix: [...manifest.launch_argv_prefix],
    artifacts,
  };
}

function commandArtifactClosureMatches(closure) {
  try {
    if (hashStable(closure.manifestFile, `${closure.kind} command manifest`, MAX_INPUT_BYTES, { privateFile: true }) !== closure.manifestSha256) return false;
    return closure.artifacts.every(artifact => hashStable(artifact.path, `${closure.kind} command artifact`, 1024 * 1024 * 1024) === artifact.sha256);
  } catch {
    return false;
  }
}

function commandArtifactClosureSummary(closure, matchesDeclaredHashes) {
  return {
    manifest_file: closure.manifestFile,
    manifest_sha256: closure.manifestSha256,
    kind: closure.kind,
    frozen_product_decision_program_sha256: closure.frozenProductDecisionProgramSha256,
    launch_executable_sha256: closure.launchExecutableSha256,
    launch_argv_prefix: [...closure.launchArgvPrefix],
    declared_artifact_set_matches_hashes: matchesDeclaredHashes,
    completeness_independently_verified: false,
    declared_artifact_set: closure.artifacts.map(artifact => ({ ...artifact })),
  };
}

function validateOutputParent(path) {
  assertAbsolutePath(path, 'output directory');
  assert.ok(!existsSync(path), 'output directory must be new; existing paths are never overwritten');
  const parent = dirname(path);
  const parentStat = lstatSync(parent);
  assert.ok(parentStat.isDirectory() && !parentStat.isSymbolicLink(), 'output parent must be a real directory');
  assert.equal(realpathSync(parent), parent, 'output parent must not contain symlinks');
}

function validateExtraArgs(args, label, forbiddenPatterns = []) {
  assert.ok(Array.isArray(args), `${label} must be an array`);
  for (const [index, arg] of args.entries()) {
    assert.equal(typeof arg, 'string', `${label}[${index}] must be a string`);
    assert.ok(Buffer.byteLength(arg, 'utf8') <= MAX_TEXT_BYTES, `${label}[${index}] is too long`);
    for (const pattern of forbiddenPatterns) assert.ok(!pattern.test(arg), `${label}[${index}] contains a reserved data route`);
  }
}

export function selectSuiteIdentities(identities, scope = 'm5-full', diagnosticSeed = 1) {
  assert.ok(scope === 'm5-full' || scope === 'm3-fixture-diagnostic', 'unknown run scope');
  if (scope === 'm5-full') return identities.map(identity => ({ ...identity }));
  assert.ok(Number.isInteger(diagnosticSeed) && diagnosticSeed >= 1 && diagnosticSeed <= 5, 'diagnostic seed is invalid');
  return identities.filter(identity => identity.suite === 'fixture' && identity.seed === diagnosticSeed)
    .map(identity => ({ ...identity }));
}

function writeExclusive(path, bytes, mode = 0o600) {
  const fd = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, mode);
  try {
    let offset = 0;
    while (offset < bytes.length) {
      const count = writeSync(fd, bytes, offset, bytes.length - offset, offset);
      assert.ok(count > 0, `write made no progress: ${path}`);
      offset += count;
    }
  } finally {
    closeSync(fd);
  }
}

function writeJsonExclusive(path, value) {
  writeExclusive(path, Buffer.from(`${JSON.stringify(value, null, 2)}\n`, 'utf8'));
}

function makePrivateDirectory(path) {
  mkdirSync(path, { mode: 0o700 });
  const stat = lstatSync(path);
  assert.ok(stat.isDirectory() && !stat.isSymbolicLink(), `directory is not a real directory: ${path}`);
  if (typeof process.getuid === 'function') assert.equal(stat.uid, process.getuid(), `directory owner differs: ${path}`);
  assert.equal(stat.mode & 0o777, 0o700, `directory mode is not private: ${path}`);
}

function makeChildEnvironment() {
  const env = Object.create(null);
  for (const key of BASE_ENV_KEYS) {
    if (process.env[key] !== undefined && !TRUTH_ENV_NAME.test(key)) env[key] = process.env[key];
  }
  return env;
}

function makeScopedChildEnvironment(baseEnvironment, tempScope) {
  return {
    ...baseEnvironment,
    TMPDIR: tempScope.path,
    TMP: tempScope.path,
    TEMP: tempScope.path,
  };
}

function boundedCapture(stream, limit = MAX_LOG_BYTES) {
  const chunks = [];
  let bytes = 0;
  let discarded = 0;
  stream?.on('data', chunk => {
    const buffer = Buffer.from(chunk);
    const accepted = Math.min(buffer.length, limit - bytes);
    if (accepted > 0) {
      chunks.push(buffer.subarray(0, accepted));
      bytes += accepted;
    }
    discarded += buffer.length - accepted;
  });
  return { bytes: () => Buffer.concat(chunks), discarded: () => discarded };
}

function delay(ms) {
  return new Promise(resolveDelay => setTimeout(resolveDelay, ms));
}

function waitForClose(child) {
  return new Promise(resolveClose => {
    let settled = false;
    const finish = result => {
      if (settled) return;
      settled = true;
      resolveClose(result);
    };
    child.once('close', (code, signal) => finish({ closed: true, code, signal, error: null }));
    child.once('error', error => {
      if (child.pid === undefined) finish({ closed: true, code: null, signal: null, error: String(error.message ?? error) });
    });
  });
}

async function stopExactChild(child, closedPromise, graceMs, killWaitMs) {
  let termRequested = false;
  let killRequested = false;
  if (child.exitCode !== null || child.signalCode !== null) {
    return { close: await closedPromise, term_requested: false, kill_requested: false, exit_confirmed: true };
  }
  try { termRequested = child.kill('SIGTERM'); } catch { termRequested = false; }
  let close = await Promise.race([closedPromise, delay(graceMs).then(() => null)]);
  if (!close && child.exitCode === null && child.signalCode === null) {
    try { killRequested = child.kill('SIGKILL'); } catch { killRequested = false; }
    close = await Promise.race([closedPromise, delay(killWaitMs).then(() => null)]);
  }
  return { close, term_requested: termRequested, kill_requested: killRequested, exit_confirmed: close?.closed === true };
}

async function runChild({ executable, args, cwd, env, timeoutMs, graceMs, killWaitMs, resourceToken }) {
  let child;
  try {
    child = spawn(executable, args, { cwd, env, shell: false, detached: true, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  } catch (error) {
    return { spawned: false, spawn_error: String(error?.message ?? error), timed_out: false, exit_confirmed: true, code: null, signal: null,
      term_requested: false, kill_requested: false, process_group_witness: null,
      stdout: Buffer.alloc(0), stderr: Buffer.alloc(0), stdout_discarded: 0, stderr_discarded: 0 };
  }
  const processGroupWitness = child.pid === undefined ? null : createProcessGroupWitness({ pid: child.pid, token: resourceToken });
  const stdout = boundedCapture(child.stdout);
  const stderr = boundedCapture(child.stderr);
  const closedPromise = waitForClose(child);
  let timer;
  let timeoutResolve;
  const timeout = new Promise(resolveTimeout => { timeoutResolve = resolveTimeout; });
  timer = setTimeout(() => timeoutResolve(true), timeoutMs);
  timer.unref?.();
  const race = await Promise.race([closedPromise.then(close => ({ close })), timeout.then(() => ({ timeout: true }))]);
  clearTimeout(timer);
  let close = race.close;
  let timedOut = false;
  let termination = { term_requested: false, kill_requested: false, exit_confirmed: close?.closed === true };
  if (race.timeout) {
    timedOut = true;
    termination = await stopExactChild(child, closedPromise, graceMs, killWaitMs);
    close = termination.close;
  }
  const stdoutBytes = stdout.bytes();
  const stderrBytes = stderr.bytes();
  return {
    spawned: child.pid !== undefined,
    pid: child.pid ?? null,
    process_group_witness: processGroupWitness,
    spawn_error: close?.error ?? null,
    timed_out: timedOut,
    exit_confirmed: close?.closed === true,
    code: close?.code ?? child.exitCode ?? null,
    signal: close?.signal ?? child.signalCode ?? null,
    term_requested: termination.term_requested,
    kill_requested: termination.kill_requested,
    stdout: stdoutBytes,
    stderr: stderrBytes,
    stdout_discarded: stdout.discarded(),
    stderr_discarded: stderr.discarded(),
  };
}

function safeRunName(identity) {
  return `${identity.suite}-${identity.task_id.toLowerCase()}-${identity.seed}`;
}

function writeDriverInputs(runDirectory, entry) {
  const goalPath = resolve(runDirectory, 'request.json');
  const resetPath = resolve(runDirectory, 'launcher-reset.json');
  writeJsonExclusive(goalPath, entry.goal);
  const reset = entry.identity.suite === 'fixture'
    ? { version: 1, task_id: entry.identity.task_id, seed: entry.identity.seed, initial_state: structuredClone(entry.record.task_request.initial_state) }
    : { version: 1, suite: 'textedit', task_id: entry.identity.task_id, seed: 1, initial_state: structuredClone(entry.record.task_request.initial_state) };
  writeJsonExclusive(resetPath, reset);
  return { goalPath, resetPath };
}

function verifyEvidenceFiles(receipt, runDirectory) {
  for (const reference of receipt.evidence_refs) {
    const path = resolve(runDirectory, reference.path);
    const rel = relative(runDirectory, path);
    if (rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) throw new SuiteError('EVIDENCE_PATH_ESCAPES_RUN_DIRECTORY');
    const stat = assertRegular(path, `evidence ${reference.path}`, MAX_EVIDENCE_BYTES, { privateFile: true });
    if ((stat.mode & 0o077) !== 0) throw new SuiteError('EVIDENCE_FILE_NOT_PRIVATE');
    if (stat.size !== reference.bytes || hashStable(path, `evidence ${reference.path}`, MAX_EVIDENCE_BYTES) !== reference.sha256) {
      throw new SuiteError('EVIDENCE_HASH_MISMATCH');
    }
  }
  return true;
}

function readDriverReceipt(path, identity, runDirectory) {
  if (!existsSync(path)) throw new SuiteError('DRIVER_RECEIPT_MISSING');
  const stat = assertRegular(path, 'driver receipt', MAX_RECEIPT_BYTES, { privateFile: true });
  if ((stat.mode & 0o077) !== 0) throw new SuiteError('DRIVER_RECEIPT_NOT_PRIVATE');
  const receipt = parseJson(readStable(path, 'driver receipt', MAX_RECEIPT_BYTES), 'driver receipt');
  validateDriverReceipt(receipt, identity);
  const evidenceFilesVerified = verifyEvidenceFiles(receipt, runDirectory);
  return { receipt, evidenceFilesVerified };
}

function hasSelfReportedCleanupReceipt(receipt) {
  return Boolean(receipt)
    && ['released', 'not_requested'].includes(receipt.stop_cleanup.stop_status)
    && receipt.stop_cleanup.cleanup_status === 'clean'
    && receipt.stop_cleanup.owned_processes_remaining.length === 0
    && receipt.stop_cleanup.owned_directories_remaining.length === 0
    && receipt.auditors.stop_cleanup === 'passed';
}

function driverSelfReportedCleanupClean(record) {
  if (!record.driver_process.spawned) {
    return record.driver_process.exit_confirmed && !record.driver_process.timed_out;
  }
  return record.driver_process.exit_confirmed
    && !record.driver_process.timed_out
    && hasSelfReportedCleanupReceipt(record.receipt);
}

function validateEvaluatorTruth(truthMap, identity) {
  const record = truthMap.get(identityKey(identity));
  if (!record) throw new SuiteError('TRUTH_RECORD_MISSING');
  return record;
}

function writeRunProcessResult(path, record) {
  writeJsonExclusive(path, record);
}

function saveBoundedLog(runDirectory, name, bytes, discarded) {
  if (bytes.length) writeExclusive(resolve(runDirectory, name), bytes);
  return { saved_bytes: bytes.length, discarded_bytes: discarded };
}

function fallbackBusinessRun(identity, reason, { timedOut = false, started = false } = {}) {
  if (started) {
    return {
      task_id: identity.task_id, seed: identity.seed, accepted: true,
      status: timedOut ? 'timeout' : 'unknown', status_detail: reason,
      system_reported_completed: false, independent_verdict: 'unknown', cold_start: false,
      knowledge_has_task_workflows: false,
    };
  }
  return {
    task_id: identity.task_id, seed: identity.seed, accepted: false,
    system_reported_completed: false, rejection_reason: reason,
  };
}

function reportBusinessRun(entry) {
  return entry.business_run;
}

function noTruthArgs(args, truthFile) {
  const truthNames = new Set([basename(truthFile).toLowerCase(), 'evaluator-truth.json', 'truth.json']);
  for (const arg of args) {
    assert.ok(!RESERVED_DRIVER_ARGUMENT.test(arg), 'driver arguments cannot name aggregate requests, freeze, or truth inputs');
    assert.ok(!arg.includes(truthFile), 'driver arguments cannot include the evaluator truth path');
    assert.ok(!truthNames.has(basename(arg).toLowerCase()), 'driver arguments cannot reference an evaluator truth file');
    assert.ok(!/(?:api[_-]?key|secret|token|credential|password)/i.test(arg), 'driver credentials must come from product metadata authorization');
  }
}

function makeStandardArgs(kind, identity, inputPaths, runDirectory, receiptPath, phaseCompletePath) {
  if (kind === 'driver') return [
    '--suite', identity.suite,
    '--task-id', identity.task_id,
    '--seed', String(identity.seed),
    '--request', inputPaths.goalPath,
    '--launcher-reset-file', inputPaths.resetPath,
    '--output-dir', runDirectory,
    '--receipt', receiptPath,
  ];
  return [
    '--suite', identity.suite,
    '--task-id', identity.task_id,
    '--seed', String(identity.seed),
    '--truth-file', inputPaths.truthPath,
    '--driver-receipt-file', inputPaths.receiptPath,
    '--evaluation-input-file', inputPaths.evaluationInputPath,
    '--run-directory', inputPaths.runDirectory,
    '--driver-phase-complete-file', phaseCompletePath,
    '--result-file', inputPaths.resultPath,
  ];
}

async function executeDriverEntry({ entry, rootDirectory, driver, driverArgs, driverEnv, driverCommandClosure, timeoutMs, graceMs, killWaitMs, runsDirectory }) {
  const runDirectory = resolve(runsDirectory, safeRunName(entry.identity));
  makePrivateDirectory(runDirectory);
  const inputPaths = writeDriverInputs(runDirectory, entry);
  const tempScope = createOwnedTempScope(runDirectory, 'driver');
  const receiptPath = resolve(runDirectory, 'driver-receipt.json');
  const standardArgs = makeStandardArgs('driver', entry.identity, inputPaths, runDirectory, receiptPath);
  const result = await runChild({
    executable: driver.path,
    args: [...driverArgs, ...standardArgs],
    cwd: runDirectory,
    env: makeScopedChildEnvironment(driverEnv, tempScope),
    timeoutMs,
    graceMs,
    killWaitMs,
    resourceToken: tempScope.token,
  });
  const ownedResourceProbe = await probeOwnedResources({
    processWitness: result.process_group_witness,
    tempScope,
    directChildClosed: result.exit_confirmed,
  });
  const commandArtifactClosureUnchangedAfterChild = commandArtifactClosureMatches(driverCommandClosure);
  const record = {
    identity: entry.identity,
    run_directory: runDirectory,
    driver_process: {
      pid: result.pid,
      exit_code: result.code,
      signal: result.signal,
      spawned: result.spawned,
      spawn_error: result.spawn_error,
      timed_out: result.timed_out,
      exit_confirmed: result.exit_confirmed,
      term_requested: result.term_requested,
      kill_requested: result.kill_requested,
      owned_resource_scope: {
        token: tempScope.token,
        temp_path: tempScope.path,
        temp_device: tempScope.device,
        temp_inode: tempScope.inode,
        process_group_witness: result.process_group_witness,
      },
      owned_resource_probe: ownedResourceProbe,
      declared_command_artifact_set_matches_hashes_after_child: commandArtifactClosureUnchangedAfterChild,
      stdout: saveBoundedLog(runDirectory, 'driver.stdout.log', result.stdout, result.stdout_discarded),
      stderr: saveBoundedLog(runDirectory, 'driver.stderr.log', result.stderr, result.stderr_discarded),
    },
    receipt_path: receiptPath,
    receipt: null,
    evidence_files_verified: false,
    failure_code: null,
    evaluator: null,
    evidence_validity: false,
    business_run: null,
  };
  if (!result.spawned) {
    record.failure_code = 'DRIVER_SPAWN_FAILED';
    record.business_run = fallbackBusinessRun(entry.identity, 'driver_spawn_failed_before_child_start');
  } else if (!result.exit_confirmed) {
    record.failure_code = 'DRIVER_EXIT_UNCONFIRMED';
    record.business_run = fallbackBusinessRun(entry.identity, 'driver_exit_unconfirmed_after_exact_child_termination', { started: true, timedOut: result.timed_out });
  } else if (result.timed_out) {
    record.failure_code = 'DRIVER_TIMEOUT';
    record.business_run = fallbackBusinessRun(entry.identity, 'driver_timeout', { started: true, timedOut: true });
  } else {
    try {
      const accepted = readDriverReceipt(receiptPath, entry.identity, runDirectory);
      record.receipt = accepted.receipt;
      record.evidence_files_verified = accepted.evidenceFilesVerified;
      if (result.code !== 0) {
        record.failure_code = 'DRIVER_NONZERO_EXIT';
        record.business_run = fallbackBusinessRun(entry.identity, 'driver_nonzero_exit', { started: true });
      } else {
        record.business_run = toBusinessResultRun({
          receipt: accepted.receipt,
          evaluatorResult: null,
          identity: entry.identity,
          knowledgeHasTaskWorkflows: false,
        });
      }
    } catch (error) {
      record.failure_code = error instanceof SuiteError ? error.code : 'DRIVER_RECEIPT_INVALID';
      record.business_run = fallbackBusinessRun(entry.identity, `invalid_or_missing_driver_receipt:${record.failure_code}`, { started: true });
    }
  }
  if (!commandArtifactClosureUnchangedAfterChild) record.failure_code = 'DRIVER_COMMAND_ARTIFACT_SET_CHANGED';
  if (!record.failure_code && ownedResourceProbe.status !== 'passed') {
    record.failure_code = 'DRIVER_OWNED_RESOURCE_CLEANUP_PROBE_NOT_PASSED';
  }
  writeRunProcessResult(resolve(runDirectory, 'coordinator-driver-result.json'), {
    version: 1,
    identity: entry.identity,
    process: record.driver_process,
    receipt_path: record.receipt ? relative(rootDirectory, receiptPath) : null,
    failure_code: record.failure_code,
    owned_resource_probe: ownedResourceProbe,
  });
  return record;
}

async function evaluateDriverEntry({ entry, record, truthRecords, evaluator, evaluatorArgs, evaluatorEnv, evaluatorCommandClosure, timeoutMs, graceMs, killWaitMs, evaluationDirectory, phaseCompletePath, scope }) {
  if (record.failure_code || !record.receipt || !record.receipt.admission.accepted || !record.evidence_files_verified) {
    record.evaluator = { status: 'not_run', verdict: 'unknown', evidence_validity: 'unknown', failure_code: record.failure_code ?? 'NO_ACCEPTED_EVIDENCE' };
    record.business_run = toBusinessResultRun({ receipt: record.receipt, evaluatorResult: null, identity: entry.identity,
      knowledgeHasTaskWorkflows: false });
    return record;
  }
  if (!commandArtifactClosureMatches(evaluatorCommandClosure)) {
    record.evaluator = { status: 'not_run', verdict: 'unknown', evidence_validity: 'unknown', failure_code: 'EVALUATOR_COMMAND_ARTIFACT_SET_CHANGED' };
    record.business_run = toBusinessResultRun({ receipt: record.receipt, evaluatorResult: null, identity: entry.identity,
      knowledgeHasTaskWorkflows: false });
    return record;
  }
  const evaluationRunDirectory = resolve(evaluationDirectory, safeRunName(entry.identity));
  makePrivateDirectory(evaluationRunDirectory);
  const tempScope = createOwnedTempScope(evaluationRunDirectory, 'evaluator');
  const truthRecord = validateEvaluatorTruth(truthRecords, entry.identity);
  const truthPath = resolve(evaluationRunDirectory, 'evaluator-truth.json');
  const evaluatorInputPath = resolve(evaluationRunDirectory, 'evaluation-input.json');
  const resultPath = resolve(evaluationRunDirectory, 'evaluation-result.json');
  writeJsonExclusive(truthPath, truthRecord);
  const evaluatorInput = {
    version: 1,
    suite: entry.identity.suite,
    task_id: entry.identity.task_id,
    seed: entry.identity.seed,
    driver_receipt_file: record.receipt_path,
    evidence_refs: record.receipt.evidence_refs,
    evidence_files_verified: record.evidence_files_verified,
  };
  writeJsonExclusive(evaluatorInputPath, evaluatorInput);
  const args = [
    ...evaluatorArgs,
    ...makeStandardArgs('evaluator', entry.identity, {
      truthPath,
      receiptPath: record.receipt_path,
      evaluationInputPath: evaluatorInputPath,
      runDirectory: record.run_directory,
      resultPath,
    }, record.run_directory, record.receipt_path, phaseCompletePath),
  ];
  const processResult = await runChild({
    executable: evaluator.path,
    args,
    cwd: evaluationRunDirectory,
    env: makeScopedChildEnvironment(evaluatorEnv, tempScope),
    timeoutMs,
    graceMs,
    killWaitMs,
    resourceToken: tempScope.token,
  });
  const ownedResourceProbe = await probeOwnedResources({
    processWitness: processResult.process_group_witness,
    tempScope,
    directChildClosed: processResult.exit_confirmed,
  });
  const evaluatorCommandClosureStableAfterChild = commandArtifactClosureMatches(evaluatorCommandClosure);
  const processSummary = {
    pid: processResult.pid,
    exit_code: processResult.code,
    signal: processResult.signal,
    spawned: processResult.spawned,
    spawn_error: processResult.spawn_error,
    timed_out: processResult.timed_out,
    exit_confirmed: processResult.exit_confirmed,
    term_requested: processResult.term_requested,
    kill_requested: processResult.kill_requested,
    owned_resource_scope: {
      token: tempScope.token,
      temp_path: tempScope.path,
      temp_device: tempScope.device,
      temp_inode: tempScope.inode,
      process_group_witness: processResult.process_group_witness,
    },
    owned_resource_probe: ownedResourceProbe,
    stdout: saveBoundedLog(evaluationRunDirectory, 'evaluator.stdout.log', processResult.stdout, processResult.stdout_discarded),
    stderr: saveBoundedLog(evaluationRunDirectory, 'evaluator.stderr.log', processResult.stderr, processResult.stderr_discarded),
  };
  record.evaluator = { status: 'failed', process: processSummary, verdict: 'unknown', evidence_validity: 'unknown', failure_code: null };
  if (!evaluatorCommandClosureStableAfterChild) {
    record.evaluator.failure_code = 'EVALUATOR_COMMAND_ARTIFACT_SET_CHANGED';
  } else if (ownedResourceProbe.status !== 'passed') {
    record.evaluator.failure_code = 'EVALUATOR_OWNED_RESOURCE_CLEANUP_PROBE_NOT_PASSED';
  } else if (processResult.spawned && processResult.exit_confirmed && !processResult.timed_out && processResult.code === 0) {
    try {
      const resultPathStat = assertRegular(resultPath, 'evaluator result', MAX_EVALUATOR_RESULT_BYTES, { privateFile: true });
      if ((resultPathStat.mode & 0o077) !== 0) throw new SuiteError('EVALUATOR_RESULT_NOT_PRIVATE');
      const evaluatorResult = validateEvaluatorResult(parseJson(readStable(resultPath, 'evaluator result', MAX_EVALUATOR_RESULT_BYTES), 'evaluator result'), entry.identity);
      record.evaluator = {
        status: 'completed',
        process: processSummary,
        verdict: evaluatorResult.verdict,
        evidence_validity: evaluatorResult.evidence_validity,
        result: evaluatorResult,
        result_path: relative(evaluationDirectory, resultPath),
        failure_code: null,
      };
      record.evidence_validity = deriveRunEvidenceValidity({
        receipt: record.receipt,
        evaluatorResult,
        evidenceFilesVerified: record.evidence_files_verified,
        independentCleanupProbePassed: record.driver_process.owned_resource_probe?.status === 'passed'
          && ownedResourceProbe.status === 'passed',
        productOwnedResourceCoverageVerified: false,
      }) && hasAcceptedModelAttribution(record.receipt, scope);
    } catch (error) {
      record.evaluator.failure_code = error instanceof SuiteError ? error.code : 'EVALUATOR_RESULT_INVALID';
    }
  } else {
    record.evaluator.failure_code = processResult.timed_out ? 'EVALUATOR_TIMEOUT'
      : !processResult.spawned ? 'EVALUATOR_SPAWN_FAILED'
        : !processResult.exit_confirmed ? 'EVALUATOR_EXIT_UNCONFIRMED' : 'EVALUATOR_NONZERO_EXIT';
  }
  record.business_run = toBusinessResultRun({
    receipt: record.receipt,
    evaluatorResult: record.evaluator.result ?? null,
    identity: entry.identity,
    knowledgeHasTaskWorkflows: false,
  });
  return record;
}

function summarizeSuites(records) {
  const bySuite = suite => summarizeBusinessResults({
    version: 1,
    suite,
    runs: records.filter(record => record.identity.suite === suite).map(reportBusinessRun),
  });
  return { fixture: bySuite('fixture'), textedit: bySuite('textedit') };
}

function evidenceGate(records, selectedIdentities, { commandArtifactClosuresValid = false } = {}) {
  const byKey = new Map(records.map(record => [identityKey(record.identity), record]));
  const missing = [];
  const invalid = [];
  for (const identity of selectedIdentities) {
    const record = byKey.get(identityKey(identity));
    if (!record) missing.push(identityKey(identity));
    else if (!record.evidence_validity) invalid.push(identityKey(identity));
  }
  const coordinatorScopeCleanupProbeComplete = selectedIdentities.length > 0 && missing.length === 0
    && selectedIdentities.every(identity => byKey.get(identityKey(identity))?.driver_process.owned_resource_probe?.status === 'passed');
  const productOwnedResourceCoverageVerified = selectedIdentities.length > 0 && missing.length === 0
    && selectedIdentities.every(identity => byKey.get(identityKey(identity))?.driver_process.owned_resource_probe?.product_owned_resource_coverage_verified === true);
  return {
    passed: missing.length === 0 && invalid.length === 0 && commandArtifactClosuresValid
      && coordinatorScopeCleanupProbeComplete && productOwnedResourceCoverageVerified,
    expected_run_count: selectedIdentities.length,
    evidence_valid_run_count: selectedIdentities.length - missing.length - invalid.length,
    missing_runs: missing,
    invalid_runs: invalid,
    declared_command_artifact_sets_match_hashes: commandArtifactClosuresValid,
    cleanup_evidence_mode: 'independent_coordinator_process_group_and_private_temp_subtree',
    coordinator_scope_cleanup_probe_complete: coordinatorScopeCleanupProbeComplete,
    product_owned_resource_coverage_verified: productOwnedResourceCoverageVerified,
    product_owned_resource_coverage_status: productOwnedResourceCoverageVerified ? 'verified' : 'unknown',
    note: 'The independent probe covers only the coordinator-created process group and private temp subtree. Product-owned resources outside that scope remain unverified and keep the evidence gate closed.',
  };
}

function markNotRunAfterStop(expected, selected, records, reason) {
  const observed = new Set(records.map(record => identityKey(record.identity)));
  const pending = selected.filter(identity => !observed.has(identityKey(identity)));
  return { reason, pending_identities: pending.map(identity => ({ ...identity })) };
}

function assertNoOutputCollision(outDirectory) {
  validateOutputParent(outDirectory);
}

function parseCli(argv = process.argv.slice(2)) {
  const parsed = parseArgs({ args: argv, strict: true, allowPositionals: false, options: {
    'request-file': { type: 'string' },
    'freeze-file': { type: 'string' },
    'truth-file': { type: 'string' },
    'out-dir': { type: 'string' },
    driver: { type: 'string' },
    'driver-command-manifest': { type: 'string' },
    'driver-arg': { type: 'string', multiple: true, default: [] },
    evaluator: { type: 'string' },
    'evaluator-command-manifest': { type: 'string' },
    'evaluator-arg': { type: 'string', multiple: true, default: [] },
    scope: { type: 'string', default: 'm5-full' },
    'diagnostic-seed': { type: 'string', default: '1' },
    'timeout-ms': { type: 'string', default: String(DEFAULT_CHILD_TIMEOUT_MS) },
    'term-grace-ms': { type: 'string', default: String(DEFAULT_TERM_GRACE_MS) },
    'kill-wait-ms': { type: 'string', default: String(DEFAULT_KILL_WAIT_MS) },
  } });
  const values = parsed.values;
  for (const key of ['request-file', 'freeze-file', 'truth-file', 'out-dir', 'driver', 'driver-command-manifest', 'evaluator', 'evaluator-command-manifest']) {
    if (!values[key]) throw new SuiteError('USAGE', `missing --${key}`);
    assertAbsolutePath(values[key], `--${key}`);
  }
  assert.ok(values.scope === 'm5-full' || values.scope === 'm3-fixture-diagnostic', 'invalid --scope');
  const positiveOption = (raw, label, maximum) => {
    const value = Number(raw);
    assert.ok(Number.isSafeInteger(value) && value > 0 && value <= maximum, `${label} is invalid`);
    return value;
  };
  const diagnosticSeed = positiveOption(values['diagnostic-seed'], '--diagnostic-seed', 5);
  assertOutputTimerOptions(values, positiveOption);
  noTruthArgs(values['driver-arg'], values['truth-file']);
  validateExtraArgs(values['driver-arg'], '--driver-arg', [DRIVER_RESERVED_OPTION, /--(?:truth|evaluator|oracle|requests-file|freeze-file)(?:=|$)/i]);
  validateExtraArgs(values['evaluator-arg'], '--evaluator-arg', [EVALUATOR_RESERVED_OPTION, /--(?:requests-file|freeze-file)(?:=|$)/i]);
  assertOutputCollision(values['out-dir']);
  return {
    requestFile: values['request-file'], freezeFile: values['freeze-file'], truthFile: values['truth-file'],
    outDirectory: values['out-dir'], driverPath: values.driver, driverCommandManifest: values['driver-command-manifest'], driverArgs: values['driver-arg'],
    evaluatorPath: values.evaluator, evaluatorCommandManifest: values['evaluator-command-manifest'], evaluatorArgs: values['evaluator-arg'], scope: values.scope, diagnosticSeed,
    timeoutMs: positiveOption(values['timeout-ms'], '--timeout-ms', 60 * 60 * 1000),
    graceMs: positiveOption(values['term-grace-ms'], '--term-grace-ms', 60_000),
    killWaitMs: positiveOption(values['kill-wait-ms'], '--kill-wait-ms', 60_000),
  };
}

function assertOutputTimerOptions(values, positiveOption) {
  positiveOption(values['timeout-ms'], '--timeout-ms', 60 * 60 * 1000);
  positiveOption(values['term-grace-ms'], '--term-grace-ms', 60_000);
  positiveOption(values['kill-wait-ms'], '--kill-wait-ms', 60_000);
}

function assertOutputCollision(path) {
  try { validateOutputParent(path); }
  catch (error) { throw new SuiteError('OUTPUT_PATH_INVALID', error.message); }
}

export async function runBusinessSuite(options) {
  const validated = validateFrozenSuiteFiles({ requestFile: options.requestFile, freezeFile: options.freezeFile, truthFile: options.truthFile });
  const driver = validateExecutable(options.driverPath, 'driver executable');
  const evaluator = validateExecutable(options.evaluatorPath, 'evaluator executable');
  const timeoutMs = options.timeoutMs ?? DEFAULT_CHILD_TIMEOUT_MS;
  const graceMs = options.graceMs ?? DEFAULT_TERM_GRACE_MS;
  const killWaitMs = options.killWaitMs ?? DEFAULT_KILL_WAIT_MS;
  assert.ok(Number.isSafeInteger(timeoutMs) && timeoutMs > 0, 'timeoutMs is invalid');
  assert.ok(Number.isSafeInteger(graceMs) && graceMs > 0, 'graceMs is invalid');
  assert.ok(Number.isSafeInteger(killWaitMs) && killWaitMs > 0, 'killWaitMs is invalid');
  const scope = options.scope ?? 'm5-full';
  const diagnosticSeed = options.diagnosticSeed ?? 1;
  const selected = selectSuiteIdentities(validated.expected, scope, diagnosticSeed);
  const driverArgs = options.driverArgs ?? [];
  const evaluatorArgs = options.evaluatorArgs ?? [];
  noTruthArgs(driverArgs, options.truthFile);
  validateExtraArgs(driverArgs, 'driverArgs', [DRIVER_RESERVED_OPTION, /--(?:truth|evaluator|oracle|requests-file|freeze-file)(?:=|$)/i]);
  validateExtraArgs(evaluatorArgs, 'evaluatorArgs', [EVALUATOR_RESERVED_OPTION, /--(?:requests-file|freeze-file)(?:=|$)/i]);
  const frozenProductDecisionProgramSha256 = validated.freeze.environment.artifact_hashes.program;
  const driverCommandClosure = validateCommandArtifactClosure({
    manifestFile: options.driverCommandManifest,
    kind: 'driver',
    executable: driver,
    args: driverArgs,
    frozenProductDecisionProgramSha256,
  });
  const evaluatorCommandClosure = validateCommandArtifactClosure({
    manifestFile: options.evaluatorCommandManifest,
    kind: 'evaluator',
    executable: evaluator,
    args: evaluatorArgs,
    frozenProductDecisionProgramSha256,
  });
  const driverEnv = makeChildEnvironment();
  const evaluatorEnv = makeChildEnvironment();
  validateOutputParent(options.outDirectory);

  const driverAfterHash = () => hashStable(driver.path, 'driver executable', 1024 * 1024 * 1024);
  const evaluatorAfterHash = () => hashStable(evaluator.path, 'evaluator executable', 1024 * 1024 * 1024);
  const requestAfterHash = () => hashStable(options.requestFile, 'request file', MAX_INPUT_BYTES, { privateFile: true });
  const freezeAfterHash = () => hashStable(options.freezeFile, 'freeze file', MAX_INPUT_BYTES, { privateFile: true });
  const truthAfterHash = () => hashStable(options.truthFile, 'evaluator truth file', MAX_INPUT_BYTES, { privateFile: true });
  const manifest = {
    version: FORMAT_VERSION,
    scope,
    business_gate_eligible: scope === 'm5-full',
    diagnostic_seed: scope === 'm3-fixture-diagnostic' ? diagnosticSeed : null,
    started_at: new Date().toISOString(),
    source_freeze_content_sha256: validated.freeze.freeze_content_sha256,
    source_environment_sha256: validated.freeze.environment_sha256,
    source_request_sha256: validated.request_sha256,
    source_truth_sha256: validated.truth_sha256,
    denominator: { fixture_runs: 100, textedit_runs: 20, total_records: 120 },
    planned_identities: validated.expected,
    selected_identities: selected,
    selection_rule: scope === 'm5-full' ? 'all frozen identities' : `all 20 fixture IDs at seed ${diagnosticSeed}; diagnostic only`,
    frozen_product_decision_program_sha256: frozenProductDecisionProgramSha256,
    driver: {
      path: driver.path, sha256: driver.sha256, extra_arguments: driverArgs,
      declared_command_artifact_set: commandArtifactClosureSummary(driverCommandClosure, true),
      environment_policy: 'minimal allowlist; no credentials or truth/evaluator variables',
    },
    evaluator: {
      path: evaluator.path, sha256: evaluator.sha256, extra_arguments: evaluatorArgs,
      declared_command_artifact_set: commandArtifactClosureSummary(evaluatorCommandClosure, true),
    },
    execution_started: false,
  };

  // All validation, hashes, identities, and output-path checks are complete before creating artifacts.
  mkdirSync(options.outDirectory, { mode: 0o700 });
  const rootStat = lstatSync(options.outDirectory);
  assert.ok(rootStat.isDirectory() && !rootStat.isSymbolicLink(), 'output path is not a real directory');
  if (typeof process.getuid === 'function') assert.equal(rootStat.uid, process.getuid(), 'output directory owner differs');
  assert.equal(rootStat.mode & 0o777, 0o700, 'output directory must be private');
  const runsDirectory = resolve(options.outDirectory, 'runs');
  const evaluationDirectory = resolve(options.outDirectory, 'evaluation');
  makePrivateDirectory(runsDirectory);
  makePrivateDirectory(evaluationDirectory);
  manifest.execution_started = true;
  manifest.started_at = new Date().toISOString();
  writeJsonExclusive(resolve(options.outDirectory, 'suite-start.json'), manifest);

  const driverRecords = [];
  let suiteStoppedReason = null;
  for (const identity of selected) {
    if (!commandArtifactClosureMatches(driverCommandClosure)) {
      suiteStoppedReason = 'driver declared command artifact set changed before the next task';
      break;
    }
    const entry = validated.records.get(identityKey(identity));
    const record = await executeDriverEntry({
      entry, rootDirectory: options.outDirectory, driver, driverArgs, driverEnv, driverCommandClosure, timeoutMs, graceMs, killWaitMs, runsDirectory,
    });
    driverRecords.push(record);
    const cleanupReportedClean = driverSelfReportedCleanupClean(record);
    if (!record.driver_process.declared_command_artifact_set_matches_hashes_after_child) {
      suiteStoppedReason = 'driver declared command artifact set changed during the task';
      break;
    }
    if (!record.driver_process.exit_confirmed) {
      suiteStoppedReason = record.driver_process.timed_out
        ? 'timed-out owned driver child did not exit after exact-child termination escalation'
        : 'owned driver child exit was not confirmed';
      break;
    }
    if (record.driver_process.owned_resource_probe?.status !== 'passed') {
      suiteStoppedReason = 'driver process-group or private temp cleanup probe failed or is unknown';
      break;
    }
    if (record.driver_process.timed_out || !record.receipt || !cleanupReportedClean) {
      suiteStoppedReason = record.driver_process.timed_out
        ? 'driver timed out; no valid cleanup receipt permits another task to start'
        : 'driver receipt or owned-resource cleanup is missing or unknown';
      break;
    }
  }

  const driverCommandClosureStableAfterDriverPhase = commandArtifactClosureMatches(driverCommandClosure);
  const evaluatorCommandClosureStableBeforeTruthLoad = commandArtifactClosureMatches(evaluatorCommandClosure);
  const phaseCompletePath = resolve(options.outDirectory, 'driver-phase-complete.json');
  const phaseComplete = {
    version: FORMAT_VERSION,
    all_driver_children_terminated: driverRecords.every(record => !record.driver_process.spawned || record.driver_process.exit_confirmed),
    all_owned_resources_cleanup_self_reported_clean: driverRecords.every(driverSelfReportedCleanupClean),
    cleanup_evidence_mode: 'independent_coordinator_process_group_and_private_temp_subtree',
    all_group_cleanup_probes_passed: driverRecords.length === selected.length
      && driverRecords.every(record => record.driver_process.owned_resource_probe?.group_cleanup_probe_passed === true),
    coordinator_scope_cleanup_probe_complete: driverRecords.length === selected.length
      && driverRecords.every(record => record.driver_process.owned_resource_probe?.status === 'passed'),
    product_owned_resource_coverage_verified: false,
    product_owned_resource_coverage_status: 'unknown',
    driver_declared_command_artifact_set_matches_hashes_after_phase: driverCommandClosureStableAfterDriverPhase,
    evaluator_declared_command_artifact_set_matches_hashes_before_truth_load: evaluatorCommandClosureStableBeforeTruthLoad,
    driver_child_count: driverRecords.filter(record => record.driver_process.spawned).length,
    selected_identity_count: selected.length,
    completed_driver_phase_at: new Date().toISOString(),
    stopped_reason: suiteStoppedReason,
  };
  writeJsonExclusive(phaseCompletePath, phaseComplete);

  const unrun = markNotRunAfterStop(validated.expected, selected, driverRecords, suiteStoppedReason ?? 'not_selected_or_driver_phase_incomplete');
  const truthPhaseGuardPassed = phaseComplete.all_driver_children_terminated
    && phaseComplete.all_owned_resources_cleanup_self_reported_clean
    && phaseComplete.coordinator_scope_cleanup_probe_complete
    && phaseComplete.driver_declared_command_artifact_set_matches_hashes_after_phase
    && phaseComplete.evaluator_declared_command_artifact_set_matches_hashes_before_truth_load;
  let truthRecords = null;
  let truthPinStable = false;
  let truthFailureCode = truthPhaseGuardPassed ? null
    : !phaseComplete.all_driver_children_terminated ? 'DRIVER_CHILD_CLEANUP_UNCONFIRMED'
      : !phaseComplete.driver_declared_command_artifact_set_matches_hashes_after_phase ? 'DRIVER_COMMAND_ARTIFACT_SET_CHANGED'
        : !phaseComplete.evaluator_declared_command_artifact_set_matches_hashes_before_truth_load ? 'EVALUATOR_COMMAND_ARTIFACT_SET_CHANGED'
          : !phaseComplete.all_owned_resources_cleanup_self_reported_clean ? 'DRIVER_OWNED_RESOURCE_CLEANUP_NOT_SELF_REPORTED_CLEAN'
            : !phaseComplete.coordinator_scope_cleanup_probe_complete ? 'DRIVER_OWNED_RESOURCE_CLEANUP_PROBE_NOT_PASSED'
              : 'DRIVER_CHILD_CLEANUP_UNCONFIRMED';
  if (!truthPhaseGuardPassed && phaseComplete.all_driver_children_terminated) {
    if (!phaseComplete.driver_declared_command_artifact_set_matches_hashes_after_phase) {
      truthFailureCode = 'DRIVER_COMMAND_ARTIFACT_SET_CHANGED';
    } else if (!phaseComplete.evaluator_declared_command_artifact_set_matches_hashes_before_truth_load) {
      truthFailureCode = 'EVALUATOR_COMMAND_ARTIFACT_SET_CHANGED';
    } else if (!phaseComplete.all_owned_resources_cleanup_self_reported_clean) {
      truthFailureCode = 'DRIVER_OWNED_RESOURCE_CLEANUP_NOT_SELF_REPORTED_CLEAN';
    } else if (!phaseComplete.coordinator_scope_cleanup_probe_complete) {
      truthFailureCode = 'DRIVER_OWNED_RESOURCE_CLEANUP_PROBE_NOT_PASSED';
    }
  }
  if (truthPhaseGuardPassed) {
    try {
      const truthBytes = readStable(options.truthFile, 'evaluator truth file', MAX_INPUT_BYTES, { privateFile: true });
      truthPinStable = sha256(truthBytes) === validated.truth_sha256 && truthAfterHash() === validated.truth_sha256;
      if (truthPinStable) {
        truthRecords = validateTruthIdentityEnvelope(parseJson(truthBytes, 'evaluator truth file'), validated.expected);
        truthFailureCode = null;
      } else {
        truthFailureCode = 'TRUTH_PIN_MISMATCH';
      }
    } catch (error) {
      truthPinStable = false;
      truthFailureCode = error instanceof SuiteError ? error.code : 'TRUTH_INPUT_INVALID';
    }
  }

  const recordsByKey = new Map(driverRecords.map(record => [identityKey(record.identity), record]));
  let evaluatorPhaseStoppedReason = null;
  if (truthRecords) {
    for (const identity of selected) {
      const record = recordsByKey.get(identityKey(identity));
      if (!record) continue;
      const entry = validated.records.get(identityKey(identity));
      await evaluateDriverEntry({
        entry, record, truthRecords, evaluator, evaluatorArgs, evaluatorEnv, timeoutMs, graceMs, killWaitMs,
        evaluatorCommandClosure, evaluationDirectory, phaseCompletePath, scope,
      });
      if (['EVALUATOR_COMMAND_ARTIFACT_SET_CHANGED', 'EVALUATOR_OWNED_RESOURCE_CLEANUP_PROBE_NOT_PASSED'].includes(record.evaluator?.failure_code)) {
        evaluatorPhaseStoppedReason = record.evaluator.failure_code === 'EVALUATOR_COMMAND_ARTIFACT_SET_CHANGED'
          ? 'evaluator declared command artifact set changed during the evaluation phase'
          : 'evaluator process-group or private temp cleanup probe failed or is unknown';
        break;
      }
    }
    if (evaluatorPhaseStoppedReason) {
      for (const record of driverRecords) {
        if (record.evaluator) continue;
        record.evaluator = { status: 'not_run', verdict: 'unknown', evidence_validity: 'unknown', failure_code: evaluatorPhaseStoppedReason.includes('artifact set')
          ? 'EVALUATOR_COMMAND_ARTIFACT_SET_CHANGED' : 'EVALUATOR_PHASE_STOPPED_AFTER_RESOURCE_CLEANUP_UNKNOWN' };
        record.business_run = toBusinessResultRun({ receipt: record.receipt, evaluatorResult: null, identity: record.identity,
          knowledgeHasTaskWorkflows: false });
      }
    }
  } else {
    for (const record of driverRecords) {
      record.evaluator = { status: 'not_run', verdict: 'unknown', evidence_validity: 'unknown', failure_code: truthFailureCode ?? 'TRUTH_INPUT_UNAVAILABLE' };
      if (record.receipt) {
        record.business_run = toBusinessResultRun({ receipt: record.receipt, evaluatorResult: null, identity: record.identity,
          knowledgeHasTaskWorkflows: false });
      }
    }
  }

  const pinStillMatches = (hashFunction, expected) => {
    try { return hashFunction() === expected; } catch { return false; }
  };
  const requestUnchanged = pinStillMatches(requestAfterHash, validated.request_sha256);
  const freezeUnchanged = pinStillMatches(freezeAfterHash, validated.freeze_sha256);
  const truthUnchanged = truthPhaseGuardPassed && truthPinStable
    && pinStillMatches(truthAfterHash, validated.truth_sha256);
  const driverUnchanged = pinStillMatches(driverAfterHash, driver.sha256);
  const evaluatorUnchanged = pinStillMatches(evaluatorAfterHash, evaluator.sha256);
  const driverCommandClosureUnchanged = driverCommandClosureStableAfterDriverPhase
    && commandArtifactClosureMatches(driverCommandClosure);
  const evaluatorCommandClosureUnchanged = evaluatorCommandClosureStableBeforeTruthLoad
    && commandArtifactClosureMatches(evaluatorCommandClosure);
  const summaries = summarizeSuites(driverRecords);
  const commandArtifactClosuresValid = driverCommandClosureUnchanged && evaluatorCommandClosureUnchanged;
  const gate = evidenceGate(driverRecords, selected, { commandArtifactClosuresValid });
  const selectedBySuite = suite => selected.filter(identity => identity.suite === suite);
  const suiteEvidenceGate = suite => evidenceGate(
    driverRecords.filter(record => record.identity.suite === suite), selectedBySuite(suite), { commandArtifactClosuresValid },
  );
  const gatedBusinessResults = Object.fromEntries(['fixture', 'textedit'].map(suite => {
    const raw = summaries[suite];
    const suiteEvidence = suiteEvidenceGate(suite);
    const threshold = scope === 'm5-full' && raw.business_quality_thresholds_met && suiteEvidence.passed
      && requestUnchanged && freezeUnchanged && truthUnchanged && driverUnchanged && evaluatorUnchanged
      && commandArtifactClosuresValid;
    return [suite, {
      aggregator: raw,
      aggregator_quality_thresholds_met: raw.business_quality_thresholds_met,
      business_quality_thresholds_met: threshold,
      evidence_validity_gate: suiteEvidence,
    }];
  }));
  const allFullDenominatorsObserved = summaries.fixture.missing_runs.length === 0 && summaries.textedit.missing_runs.length === 0
    && summaries.fixture.expected_runs === 100 && summaries.textedit.expected_runs === 20;
  const businessGatePassed = scope === 'm5-full' && allFullDenominatorsObserved
    && gatedBusinessResults.fixture.business_quality_thresholds_met && gatedBusinessResults.textedit.business_quality_thresholds_met
    && gate.passed && requestUnchanged && freezeUnchanged && truthUnchanged && driverUnchanged && evaluatorUnchanged
    && commandArtifactClosuresValid;
  const output = {
    version: FORMAT_VERSION,
    scope,
    business_gate_eligible: scope === 'm5-full',
    business_gate_passed: businessGatePassed,
    denominator: { fixture_runs: 100, textedit_runs: 20, total_records: 120 },
    planned_identity_count: validated.expected.length,
    selected_identity_count: selected.length,
    executed_driver_count: driverRecords.length,
    unrun,
    source_pins: {
      request_sha256: validated.request_sha256,
      freeze_sha256: validated.freeze_sha256,
      freeze_content_sha256: validated.freeze.freeze_content_sha256,
      truth_sha256: validated.truth_sha256,
      request_unchanged: requestUnchanged,
      freeze_unchanged: freezeUnchanged,
      truth_unchanged: truthUnchanged,
      driver_sha256_before: driver.sha256,
      driver_sha256_after: driverUnchanged ? driver.sha256 : null,
      evaluator_sha256_before: evaluator.sha256,
      evaluator_sha256_after: evaluatorUnchanged ? evaluator.sha256 : null,
      frozen_product_decision_program_sha256: frozenProductDecisionProgramSha256,
    driver_declared_command_artifact_set: commandArtifactClosureSummary(driverCommandClosure, driverCommandClosureUnchanged),
    evaluator_declared_command_artifact_set: commandArtifactClosureSummary(evaluatorCommandClosure, evaluatorCommandClosureUnchanged),
    },
    driver_phase: phaseComplete,
    evaluator_phase: {
      stop_reason: evaluatorPhaseStoppedReason,
      declared_command_artifact_set_matches_hashes: evaluatorCommandClosureUnchanged,
    },
    evidence_validity_gate: gate,
    business_results: gatedBusinessResults,
    runs: driverRecords.map(record => ({
      identity: record.identity,
      status: record.failure_code ?? record.receipt?.status ?? record.business_run?.status ?? 'not_run',
      driver_process: record.driver_process,
      driver_receipt_path: record.receipt ? relative(options.outDirectory, record.receipt_path) : null,
      model_attribution: summarizeModelAttribution(record.receipt),
      evaluator: record.evaluator,
      evidence_files_verified: record.evidence_files_verified,
      evidence_validity: record.evidence_validity,
      business_result_run: record.business_run,
    })),
    acceptance_boundary: {
      independent_truth_loaded_after_all_driver_children_exited: truthPhaseGuardPassed && truthRecords !== null,
      truth_load_failure_code: truthFailureCode,
      direct_args_env_goal_only_excludes_truth: true,
      same_uid_isolation_enforced: false,
      trusted_driver_required: true,
      cleanup_evidence_mode: 'independent_coordinator_process_group_and_private_temp_subtree',
      coordinator_scope_cleanup_probe_complete: phaseComplete.coordinator_scope_cleanup_probe_complete,
      product_owned_resource_coverage_verified: false,
      product_owned_resource_coverage_status: 'unknown',
      runner_direct_truth_routing_to_statecore_s1_harness_knowledge: false,
      decision_source_mapping: 'receipt s2 is a provisional normalization of Host plan; actual-driver validation is pending',
      quality_gate_eligible_for_subset: scope === 'm5-full',
      external_evidence_audit_complete: false,
    },
    mvp_complete: false,
  };
  writeJsonExclusive(resolve(options.outDirectory, 'business-suite-result.json'), output);
  return output;
}

function cliOptions(argv = process.argv.slice(2)) {
  return parseCli(argv);
}

async function main() {
  try {
    const cli = cliOptions();
    await runBusinessSuite({
      requestFile: cli.requestFile,
      freezeFile: cli.freezeFile,
      truthFile: cli.truthFile,
      outDirectory: cli.outDirectory,
      driverPath: cli.driverPath,
      driverCommandManifest: cli.driverCommandManifest,
      driverArgs: cli.driverArgs,
      evaluatorPath: cli.evaluatorPath,
      evaluatorCommandManifest: cli.evaluatorCommandManifest,
      evaluatorArgs: cli.evaluatorArgs,
      scope: cli.scope,
      diagnosticSeed: cli.diagnosticSeed,
      timeoutMs: cli.timeoutMs,
      graceMs: cli.graceMs,
      killWaitMs: cli.killWaitMs,
    });
  } catch (error) {
    const code = error instanceof SuiteError ? error.code : 'SUITE_REJECTED';
    process.stderr.write(`Business suite rejected (${code}). No existing output path was modified.\n`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main();
