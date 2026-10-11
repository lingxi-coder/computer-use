#!/usr/bin/env node
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import {
  closeSync, constants, existsSync, fstatSync, lstatSync, mkdirSync, mkdtempSync, openSync, readSync,
  realpathSync, readdirSync, rmdirSync, unlinkSync, writeFileSync,
} from 'node:fs';
import { spawn, type ChildProcess } from 'node:child_process';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { fixtureTaskIds, textEditTaskIds } from './business-results.mjs';
import { frozenGoalSha256, frozenGoalStableJson } from './frozen-goals.mjs';
import { validateDriverReceipt } from './business-run-receipt.mjs';
import {
  fixtureGoalConfirmationMatchesRequest,
  fixtureScopeFamilyForTaskId,
  validateFixtureAllowedScope,
} from './frozen-fixture-scope.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const MAIN_HARNESS = resolve(ROOT, 'scripts/test/frozenFixtureMainSmoke.cjs');
const FIXTURE_BUNDLE_ID = 'com.lingxi.computer-user.mvp-fixture';
const FIXTURE_BINARY = resolve(ROOT, 'target/native-macos/Interaction Fixture.app/Contents/MacOS/MvpFixture');
const ELECTRON_RUNTIME = resolve(ROOT, 'node_modules/electron/dist/Electron.app/Contents/MacOS/Electron');
const CONSENT_DRIVER = resolve(ROOT, 'target/native-macos/consent-dialog-driver');
const NATIVE_GUARD = resolve(ROOT, 'target/debug/cu-macos-device-guard');
const MAX_GOALS_BYTES = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES = 8 * 1024;
const MAX_INPUT_BYTES = 128 * 1024;
const MAX_OUTPUT_BYTES = 8 * 1024 * 1024;
const MAX_LOG_BYTES = 16 * 1024;
const SYSTEM_PRIVATE_TEMP_ROOT = '/private/tmp';
const DRIVER_TIMEOUT_MS = 25 * 60 * 1000;
const TERM_GRACE_MS = 1_500;
const KILL_WAIT_MS = 4_000;
const SHA256 = /^[a-f0-9]{64}$/;
const FORBIDDEN_DATA_KEY = /^(?:evaluator(?:_truth)?|truth|oracle|expected(?:_|$)|driver_answer|workflow|navigation|steps|selectors?|coordinates?|confirmation_id)$/i;

export class ProductDriverError extends Error {
  readonly code: string;

  constructor(code: string, message = code) {
    super(message);
    this.name = 'ProductDriverError';
    this.code = code;
  }
}

type Identity = { suite: 'fixture' | 'textedit'; task_id: string; seed: number };
type DriverArgs = {
  taskGoalsFile: string;
  goalManifestFile: string;
  taskGoalsSha256: string;
  goalManifestSha256: string;
  suite: 'fixture' | 'textedit';
  taskId: string;
  seed: number;
  requestFile: string;
  resetFile: string;
  outputDirectory: string;
  receiptPath: string;
};

function record(value: unknown): value is Record<string, any> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
}

function exactKeys(value: unknown, keys: readonly string[], label: string): asserts value is Record<string, any> {
  assert.ok(record(value), `${label} must be an object`);
  assert.deepEqual(Object.keys(value).sort(), [...keys].sort(), `${label} has an unexpected shape`);
}

function sha256(value: Buffer | string): string {
  return createHash('sha256').update(value).digest('hex');
}

function canonicalSha256(value: unknown): string {
  return frozenGoalSha256(Buffer.from(frozenGoalStableJson(value), 'utf8'));
}

function sameFile(a: { dev: number; ino: number; nlink: number; size: number; mtimeMs: number; ctimeMs: number },
  b: { dev: number; ino: number; nlink: number; size: number; mtimeMs: number; ctimeMs: number }): boolean {
  return a.dev === b.dev && a.ino === b.ino && a.nlink === b.nlink && a.size === b.size
    && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs;
}

function readPrivateRegularFile(path: string, label: string, maximum: number): Buffer {
  assert.ok(isAbsolute(path) && resolve(path) === path, `${label} must be a canonical absolute path`);
  const before = lstatSync(path);
  assert.ok(before.isFile() && !before.isSymbolicLink() && before.nlink === 1,
    `${label} must be a single regular file`);
  assert.equal(realpathSync(path), path, `${label} path may not contain symlinks`);
  assert.ok(before.size > 0 && before.size <= maximum, `${label} exceeds its byte bound`);
  if (typeof process.getuid === 'function') assert.equal(before.uid, process.getuid(), `${label} owner differs`);
  assert.equal(before.mode & 0o077, 0, `${label} must be private`);
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const opened = fstatSync(fd);
    assert.ok(sameFile(before, opened), `${label} changed before reading`);
    const bytes = Buffer.alloc(opened.size);
    let offset = 0;
    while (offset < bytes.length) {
      const count = readFileChunk(fd, bytes, offset);
      assert.ok(count > 0, `${label} ended early`);
      offset += count;
    }
    assert.ok(sameFile(opened, fstatSync(fd)) && sameFile(opened, lstatSync(path)), `${label} changed while reading`);
    return bytes;
  } finally { closeSync(fd); }
}

function hashStableRegularFile(path: string, label: string, maximum: number, executable = false): string {
  assert.ok(isAbsolute(path) && resolve(path) === path, `${label} must be a canonical absolute path`);
  const before = lstatSync(path);
  assert.ok(before.isFile() && !before.isSymbolicLink() && before.nlink === 1,
    `${label} must be a single regular file`);
  assert.equal(realpathSync(path), path, `${label} path may not contain symlinks`);
  assert.ok(before.size > 0 && before.size <= maximum, `${label} exceeds its byte bound`);
  if (executable) assert.ok((before.mode & 0o111) !== 0, `${label} is not executable`);
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const opened = fstatSync(fd);
    assert.ok(sameFile(before, opened), `${label} changed before hashing`);
    const hash = createHash('sha256');
    const chunk = Buffer.alloc(1024 * 1024);
    let offset = 0;
    while (offset < opened.size) {
      const count = readSync(fd, chunk, 0, Math.min(chunk.length, opened.size - offset), offset);
      assert.ok(count > 0, `${label} ended early`);
      hash.update(chunk.subarray(0, count));
      offset += count;
    }
    assert.ok(sameFile(opened, fstatSync(fd)) && sameFile(opened, lstatSync(path)), `${label} changed while hashing`);
    return hash.digest('hex');
  } finally { closeSync(fd); }
}

function readFileChunk(fd: number, bytes: Buffer, offset: number): number {
  // Positional reads keep this helper independent of shared descriptor offsets.
  return readSync(fd, bytes, offset, bytes.length - offset, offset);
}

function parseJson(bytes: Buffer, label: string): any {
  try { return JSON.parse(bytes.toString('utf8')); }
  catch { throw new ProductDriverError('INVALID_JSON', `${label} is invalid JSON`); }
}

function assertNoForbiddenKeys(value: unknown, label: string, depth = 0, nodes = { count: 0 }): void {
  assert.ok(depth <= 20, `${label} nesting is too deep`);
  nodes.count += 1;
  assert.ok(nodes.count <= 20_000, `${label} has too many values`);
  if (Array.isArray(value)) {
    assert.ok(value.length <= 256, `${label} array exceeds its bound`);
    for (const child of value) assertNoForbiddenKeys(child, label, depth + 1, nodes);
    return;
  }
  if (!record(value)) return;
  for (const [key, child] of Object.entries(value)) {
    assert.ok(!FORBIDDEN_DATA_KEY.test(key), `${label} contains private evaluator or workflow data`);
    assertNoForbiddenKeys(child, label, depth + 1, nodes);
  }
}

function expectedIdentities(): Identity[] {
  return [
    ...fixtureTaskIds.flatMap(task_id => [1, 2, 3, 4, 5].map(seed => ({ suite: 'fixture' as const, task_id, seed }))),
    ...textEditTaskIds.map(task_id => ({ suite: 'textedit' as const, task_id, seed: 1 })),
  ];
}

function selectorFor(sourceFreezeSha256: string, identity: Identity, goal: unknown): string {
  const goalHash = frozenGoalSha256(Buffer.from(frozenGoalStableJson(goal), 'utf8'));
  const input = `computer-use-frozen-task-selector-v1\n${sourceFreezeSha256}\n${identity.suite}\n${identity.task_id}\n${identity.seed}\n${goalHash}`;
  return sha256(input);
}

export function validateGoalCatalog({ goalsBytes, manifestBytes, goalsSha256, manifestSha256, identity, request }:
  { goalsBytes: Buffer; manifestBytes: Buffer; goalsSha256: string; manifestSha256: string; identity: Identity; request: unknown }) {
  assert.match(goalsSha256, SHA256, 'task-goals SHA-256 is invalid');
  assert.match(manifestSha256, SHA256, 'goal manifest SHA-256 is invalid');
  assert.equal(sha256(goalsBytes), goalsSha256, 'task-goals bytes differ from the pinned SHA-256');
  assert.equal(sha256(manifestBytes), manifestSha256, 'goal manifest bytes differ from the pinned SHA-256');
  const goals = parseJson(goalsBytes, 'task-goals file');
  const manifest = parseJson(manifestBytes, 'goal manifest');
  assert.equal(goalsBytes.toString('utf8'), frozenGoalStableJson(goals), 'task-goals file is not canonical');
  assert.equal(manifestBytes.toString('utf8'), frozenGoalStableJson(manifest), 'goal manifest is not canonical');
  exactKeys(goals, ['format_version', 'task_profile_version', 'records'], 'task-goals file');
  assert.equal(goals.format_version, 1);
  assert.equal(goals.task_profile_version, 1);
  assert.ok(Array.isArray(goals.records) && goals.records.length === 120, 'task-goals must preserve all 120 identities');
  exactKeys(manifest, [
    'format_version', 'task_profile_version', 'source_freeze_sha256', 'source_freeze_content_sha256',
    'source_request_sha256', 'task_goals_sha256', 'identities_sha256', 'identity_count', 'denominators', 'selector_scheme',
  ], 'goal manifest');
  assert.equal(manifest.format_version, 1);
  assert.equal(manifest.task_profile_version, 1);
  assert.equal(manifest.task_goals_sha256, goalsSha256);
  assert.equal(manifest.identity_count, 120);
  assert.equal(manifest.selector_scheme, 'sha256/domain-separated-v1');
  exactKeys(manifest.denominators, ['fixture_runs', 'textedit_runs', 'total_records'], 'goal denominator');
  assert.deepEqual(manifest.denominators, { fixture_runs: 100, textedit_runs: 20, total_records: 120 });
  for (const key of ['source_freeze_sha256', 'source_freeze_content_sha256', 'source_request_sha256', 'task_goals_sha256', 'identities_sha256']) {
    assert.match(manifest[key], SHA256, `goal manifest ${key} is invalid`);
  }
  const expected = expectedIdentities();
  const identities: Array<[string, string, number]> = [];
  let selected: any = null;
  for (const [index, item] of goals.records.entries()) {
    exactKeys(item, ['selector', 'suite', 'task_id', 'seed', 'goal'], `task-goals record ${index}`);
    const expectedIdentity = expected[index]!;
    assert.deepEqual({ suite: item.suite, task_id: item.task_id, seed: item.seed }, expectedIdentity,
      `task-goals identity order differs at ${index}`);
    assert.match(item.selector, SHA256, `task-goals selector ${index} is invalid`);
    exactKeys(item.goal, ['objective', 'allowed_scope'], `task-goals goal ${index}`);
    assert.equal(typeof item.goal.objective, 'string');
    assert.ok(item.goal.objective.trim() && Buffer.byteLength(item.goal.objective, 'utf8') <= 32_768);
    assertNoForbiddenKeys(item.goal, `task-goals goal ${index}`);
    assert.equal(item.selector, selectorFor(manifest.source_freeze_sha256, expectedIdentity, item.goal),
      `task-goals selector ${index} does not match the frozen goal`);
    identities.push([item.suite, item.task_id, item.seed]);
    if (item.suite === identity.suite && item.task_id === identity.task_id && item.seed === identity.seed) {
      assert.equal(selected, null, 'selected frozen identity is duplicated');
      selected = item;
    }
  }
  assert.equal(canonicalSha256(identities), manifest.identities_sha256, 'goal identity digest does not match');
  assert.ok(selected, 'selected frozen identity is missing from the goal catalog');
  assert.deepEqual(selected.goal, request, 'per-run request differs from the Main-owned frozen goal');
  return { goals, manifest, selected, catalogId: manifestSha256 };
}

export function validateProductDriverInputs({ identity, request, reset }: { identity: Identity; request: unknown; reset: unknown }) {
  exactKeys(request, ['objective', 'allowed_scope'], 'per-run request');
  assert.equal(typeof request.objective, 'string');
  assert.ok(request.objective.trim() && Buffer.byteLength(request.objective, 'utf8') <= 32_768);
  assertNoForbiddenKeys(request, 'per-run request');
  if (identity.suite === 'fixture' && fixtureScopeFamilyForTaskId(identity.task_id)) {
    validateFixtureAllowedScope(request.allowed_scope, identity.task_id);
  }
  if (identity.suite === 'fixture') {
    exactKeys(reset, ['version', 'task_id', 'seed', 'initial_state'], 'fixture reset');
    assert.equal(reset.version, 1);
    assert.equal(reset.task_id, identity.task_id);
    assert.equal(reset.seed, identity.seed);
    assertNoForbiddenKeys(reset, 'fixture reset');
    if (identity.task_id.startsWith('SET-')) {
      const extraStateKeys = identity.task_id === 'SET-03' || identity.task_id === 'SET-04'
        ? ['persistence', 'seed'] : identity.task_id === 'SET-05' ? ['persistence'] : [];
      exactKeys(reset.initial_state, ['application', 'variant', 'controls', ...extraStateKeys], 'fixture initial_state');
      exactKeys(reset.initial_state.application, ['bundle_id', 'name', 'version'], 'fixture application');
      assert.equal(reset.initial_state.application?.bundle_id, FIXTURE_BUNDLE_ID);
      assert.equal(reset.initial_state.application?.name, 'Interaction Fixture');
      assert.equal(reset.initial_state.application?.version, '1.0.0');
      exactKeys(reset.initial_state.variant, ['language', 'theme', 'layout'], 'fixture variant');
      assert.deepEqual(reset.initial_state.variant, expectedFixtureVariant(identity.task_id, identity.seed));
      assert.ok(Array.isArray(reset.initial_state.controls)
        && reset.initial_state.controls.length === (identity.task_id === 'SET-06' ? 2 : 1));
      for (const control of reset.initial_state.controls) {
        const keys = ['SET-01', 'SET-06'].includes(identity.task_id) ? ['group', 'label', 'value'] : ['label', 'value'];
        exactKeys(control, keys, 'fixture control');
        assert.equal(typeof control.label, 'string');
        assert.ok(control.label.trim() && Buffer.byteLength(control.label, 'utf8') <= 256);
        assert.equal(typeof control.value, 'boolean');
        if ('group' in control) assert.equal(typeof control.group, 'string');
      }
      if (identity.task_id === 'SET-03') {
        assert.equal(reset.initial_state.persistence, 'manual_save_required');
        assert.equal(reset.initial_state.seed, identity.seed);
      }
      if (identity.task_id === 'SET-04' || identity.task_id === 'SET-05') {
        assert.equal(reset.initial_state.persistence, 'automatic');
      }
      if (identity.task_id === 'SET-04') assert.equal(reset.initial_state.seed, identity.seed);
    } else if (identity.task_id.startsWith('FORM-')) {
      validateFormFixtureReset(reset.initial_state, request.allowed_scope, identity);
    }
  } else {
    exactKeys(reset, ['version', 'suite', 'task_id', 'seed', 'initial_state'], 'TextEdit reset');
    assert.equal(reset.version, 1);
    assert.equal(reset.suite, 'textedit');
    assert.equal(reset.task_id, identity.task_id);
    assert.equal(reset.seed, identity.seed);
    assertNoForbiddenKeys(reset, 'TextEdit reset');
  }
}

function validateFormFixtureReset(initialState: any, allowedScope: any, identity: Identity): void {
  const extraStateKeys = identity.task_id === 'FORM-06' ? ['validation_state'] : [];
  exactKeys(initialState, ['application', 'variant', 'controls', ...extraStateKeys], 'form fixture initial state');
  exactKeys(initialState.application, ['bundle_id', 'name', 'version'], 'fixture application');
  assert.equal(initialState.application.bundle_id, FIXTURE_BUNDLE_ID);
  assert.equal(initialState.application.name, 'Interaction Fixture');
  assert.equal(initialState.application.version, '1.0.0');
  exactKeys(initialState.variant, ['language', 'theme', 'layout'], 'fixture variant');
  assert.deepEqual(initialState.variant, expectedFixtureVariant(identity.task_id, identity.seed));
  assert.ok(Array.isArray(initialState.controls) && initialState.controls.length === 1,
    'form fixture must reset exactly one control');
  const control = initialState.controls[0];
  const inputKinds: Record<string, string> = {
    'FORM-01': 'single_line', 'FORM-02': 'multiline', 'FORM-03': 'dropdown',
    'FORM-04': 'radio_group', 'FORM-05': 'checkbox', 'FORM-06': 'single_line', 'FORM-07': 'number',
  };
  const expectedKeys = identity.task_id === 'FORM-07'
    ? ['initially_in_viewport', 'input_kind', 'label', 'value'] : ['input_kind', 'label', 'value'];
  exactKeys(control, expectedKeys, 'form fixture control');
  assert.equal(control.label, allowedScope.targets[0].control, 'form reset primary control differs from frozen scope');
  assert.equal(control.input_kind, inputKinds[identity.task_id], 'form reset input kind differs from task profile');
  if (identity.task_id === 'FORM-05') assert.equal(typeof control.value, 'boolean');
  else if (identity.task_id === 'FORM-07') assert.ok(typeof control.value === 'number' && Number.isFinite(control.value));
  else {
    assert.equal(typeof control.value, 'string');
    assert.ok(Buffer.byteLength(control.value, 'utf8') <= 8_192, 'form reset text exceeds its bound');
  }
  if (identity.task_id === 'FORM-06') assert.equal(initialState.validation_state, 'invalid');
  if (identity.task_id === 'FORM-07') assert.equal(control.initially_in_viewport, false);
}

function expectedFixtureVariant(taskId: string, seed: number) {
  const taskIndex = fixtureTaskIds.indexOf(taskId);
  assert.ok(taskIndex >= 0, 'fixture task id is outside the frozen set');
  const seedIndex = seed - 1;
  const languages = ['en-US', 'zh-CN'];
  const themes = ['light', 'dark'];
  const layouts = ['compact', 'standard', 'wide', 'two_column'];
  return {
    language: languages[(taskIndex + seedIndex) % languages.length],
    theme: themes[(taskIndex + Math.floor(seedIndex / 2)) % themes.length],
    layout: layouts[(taskIndex + seedIndex) % layouts.length],
  };
}

export function parseProductDriverArgs(argv = process.argv.slice(2)): DriverArgs {
  const parsed = parseArgs({ args: argv, strict: true, allowPositionals: false, options: {
    'task-goals-file': { type: 'string' },
    'goal-catalog-manifest-file': { type: 'string' },
    'task-goals-sha256': { type: 'string' },
    'goal-catalog-manifest-sha256': { type: 'string' },
    suite: { type: 'string' },
    'task-id': { type: 'string' },
    seed: { type: 'string' },
    request: { type: 'string' },
    'launcher-reset-file': { type: 'string' },
    'output-dir': { type: 'string' },
    receipt: { type: 'string' },
  } });
  const v = parsed.values;
  const required = [
    'task-goals-file', 'goal-catalog-manifest-file', 'task-goals-sha256', 'goal-catalog-manifest-sha256',
    'suite', 'task-id', 'seed', 'request', 'launcher-reset-file', 'output-dir', 'receipt',
  ] as const;
  for (const key of required) assert.equal(typeof v[key], 'string', `missing --${key}`);
  assert.ok(v.suite === 'fixture' || v.suite === 'textedit', '--suite is invalid');
  const seed = Number(v.seed);
  assert.ok(Number.isSafeInteger(seed) && seed >= 1 && seed <= 5, '--seed is invalid');
  if (v.suite === 'textedit') assert.equal(seed, 1, 'TextEdit seed must be 1');
  const result: DriverArgs = {
    taskGoalsFile: v['task-goals-file']!,
    goalManifestFile: v['goal-catalog-manifest-file']!,
    taskGoalsSha256: v['task-goals-sha256']!,
    goalManifestSha256: v['goal-catalog-manifest-sha256']!,
    suite: v.suite,
    taskId: v['task-id']!,
    seed,
    requestFile: v.request!,
    resetFile: v['launcher-reset-file']!,
    outputDirectory: v['output-dir']!,
    receiptPath: v.receipt!,
  };
  for (const [label, path] of Object.entries({
    'task-goals-file': result.taskGoalsFile,
    'goal-catalog-manifest-file': result.goalManifestFile,
    '--request': result.requestFile,
    '--launcher-reset-file': result.resetFile,
    '--output-dir': result.outputDirectory,
    '--receipt': result.receiptPath,
  })) assert.ok(isAbsolute(path) && resolve(path) === path, `${label} must be a canonical absolute path`);
  assert.match(result.taskGoalsSha256, SHA256);
  assert.match(result.goalManifestSha256, SHA256);
  assert.match(result.taskId, /^[A-Z]+-[0-9]{2}$/);
  return result;
}

function assertPrivateOutputDirectory(path: string): void {
  const stat = lstatSync(path);
  assert.ok(stat.isDirectory() && !stat.isSymbolicLink());
  assert.equal(realpathSync(path), path);
  if (typeof process.getuid === 'function') assert.equal(stat.uid, process.getuid());
  assert.equal(stat.mode & 0o777, 0o700);
}

function writeJsonExclusive(path: string, value: unknown, maximum = MAX_OUTPUT_BYTES): { bytes: number; sha256: string } {
  const bytes = Buffer.from(`${JSON.stringify(value, null, 2)}\n`, 'utf8');
  assert.ok(bytes.length > 0 && bytes.length <= maximum, 'output JSON exceeds its bound');
  const fd = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try { writeFileSync(fd, bytes); }
  finally { closeSync(fd); }
  const stat = lstatSync(path);
  assert.ok(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1);
  if (typeof process.getuid === 'function') assert.equal(stat.uid, process.getuid());
  assert.equal(stat.mode & 0o077, 0);
  return { bytes: bytes.length, sha256: sha256(bytes) };
}

function removePrivateOutputDirectory(path: string, allowedFiles: readonly string[]): boolean {
  if (!existsSync(path)) return true;
  try {
    assertPrivateOutputDirectory(path);
    const entries = readdirSync(path);
    if (entries.some(name => !allowedFiles.includes(name))) return false;
    for (const name of entries) {
      const file = resolve(path, name);
      const stat = lstatSync(file);
      assert.ok(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1);
      if (typeof process.getuid === 'function') assert.equal(stat.uid, process.getuid());
      assert.equal(stat.mode & 0o077, 0);
      assert.equal(realpathSync(file), file);
      unlinkSync(file);
    }
    rmdirSync(path);
    return true;
  } catch { return false; }
}

function removeEmptyPrivateDirectory(path: string): boolean {
  if (!existsSync(path)) return true;
  try {
    assertPrivateOutputDirectory(path);
    rmdirSync(path);
    return true;
  } catch { return false; }
}

function writePrivateCopy(path: string, bytes: Buffer, maximum = MAX_OUTPUT_BYTES): void {
  assert.ok(bytes.length > 0 && bytes.length <= maximum, 'copied evidence exceeds its byte bound');
  const fd = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try { writeFileSync(fd, bytes); }
  finally { closeSync(fd); }
  const stat = lstatSync(path);
  assert.ok(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1);
  if (typeof process.getuid === 'function') assert.equal(stat.uid, process.getuid());
  assert.equal(stat.mode & 0o077, 0);
}

function fileEvidence(path: string, baseDirectory: string) {
  const resolved = resolve(path);
  const rel = relative(baseDirectory, resolved);
  assert.ok(rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel), 'evidence path escapes the run directory');
  const bytes = readPrivateRegularFile(resolved, 'driver evidence', MAX_OUTPUT_BYTES);
  return { path: rel.split(sep).join('/'), sha256: sha256(bytes), bytes: bytes.length };
}

export function stageNormalMainFinalRead(mainOutputDirectory: string, workDirectory: string): { bytes: Buffer; path: string } | null {
  const sourcePath = resolve(mainOutputDirectory, 'final-current-ui.json');
  if (!existsSync(sourcePath)) return null;
  const bytes = readPrivateRegularFile(sourcePath, 'Main final current-UI evidence', MAX_OUTPUT_BYTES);
  const stagedPath = resolve(workDirectory, 'final-current-ui.json');
  writePrivateCopy(stagedPath, bytes, MAX_OUTPUT_BYTES);
  return { bytes, path: stagedPath };
}

export function stageNormalMainResult(mainOutputDirectory: string, workDirectory: string) {
  const sourcePath = resolve(mainOutputDirectory, 'normal-main-result.json');
  const bytes = readPrivateRegularFile(sourcePath, 'normal Main result', MAX_OUTPUT_BYTES);
  const result = parseJson(bytes, 'normal Main result');
  assert.ok(result && typeof result.status === 'string', 'normal Main result has no status');
  const stagedPath = resolve(workDirectory, 'normal-main-result.json');
  writePrivateCopy(stagedPath, bytes, MAX_OUTPUT_BYTES);
  const unconfirmedMainFailure = typeof result.failure_step === 'string' && result.goal_confirmation_accepted !== true
    ? new ProductDriverError('NORMAL_MAIN_HARNESS_FAILED', `normal Main did not complete an accepted Goal flow (${result.failure_step})`)
    : null;
  return { bytes, result, path: stagedPath, unconfirmedMainFailure };
}

export function stageNormalMainHostLedger(mainOutputDirectory: string, workDirectory: string): { bytes: Buffer; path: string } | null {
  const sourcePath = resolve(mainOutputDirectory, 'host-ledger-evidence.json');
  if (!existsSync(sourcePath)) return null;
  const bytes = readPrivateRegularFile(sourcePath, 'Root-scoped Host evidence', MAX_OUTPUT_BYTES);
  const stagedPath = resolve(workDirectory, 'host-ledger-evidence.json');
  writePrivateCopy(stagedPath, bytes, MAX_OUTPUT_BYTES);
  return { bytes, path: stagedPath };
}

function nullableSafeInteger(value: unknown): number | null {
  return Number.isSafeInteger(value) && (value as number) >= 0 ? value as number : null;
}

function matchingTeacherCall(host: any, decision: any): any | null {
  return (Array.isArray(host.call_rows) ? host.call_rows : []).find((call: any) =>
    call.scope === 's1_decision' && call.root_id === host.scope?.root_id
      && call.decision_id === decision.decision_id && call.generation === decision.generation
      && call.decision_reference_status === 'matched' && call.decision_source === decision.source
      && call.outcome === 'proposal' && call.finished_ms !== null) ?? null;
}

function s2ReferenceForDecision(host: any, decision: any): { root_id: string; request_id: string; generation: number } | null {
  const crossReference = (host.cross_references?.decisions ?? [])
    .find((item: any) => item.decision_id === decision.decision_id);
  const request = crossReference?.s2_request;
  if (!crossReference || !request || crossReference.plan_installation_reference_status !== 'matched'
      || crossReference.s2_request_reference_status !== 'matched'
      || host.cross_references?.s2_plan_request?.time_based_matching_used !== false
      || !Number.isSafeInteger(decision.plan_revision)) return null;
  const plan = (host.plan_installations ?? []).find((item: any) => item.plan_revision === decision.plan_revision);
  const planReference = (host.cross_references?.plan_installations ?? [])
    .find((item: any) => item.plan_revision === decision.plan_revision);
  if (!plan || plan.root_id !== host.scope?.root_id || plan.state !== 'installed'
      || plan.attribution_status !== 'model_request'
      || plan.request_id !== request.request_id || plan.generation !== request.generation
      || plan.reason !== request.reason || !planReference
      || planReference.state !== 'installed' || planReference.attribution_status !== 'model_request'
      || planReference.request_reference_status !== 'matched') return null;
  const call = (host.call_rows ?? []).find((item: any) => item.scope === 's2_request'
    && item.root_id === host.scope?.root_id && item.request_id === request.request_id
    && item.generation === request.generation && item.reason === request.reason
    && item.outcome === 'proposal' && item.finished_ms !== null
    && item.plan_installation_reference_status === 'matched'
    && Array.isArray(item.plan_installation_revisions) && item.plan_installation_revisions.includes(decision.plan_revision));
  if (!call) return null;
  return { root_id: host.scope.root_id, request_id: request.request_id, generation: request.generation };
}

function typedDecisionSource(host: any, decision: any): { source: string; s2RequestRef: any } {
  if (decision.source === 'plan' || decision.source === 's2') {
    const s2RequestRef = s2ReferenceForDecision(host, decision);
    return s2RequestRef ? { source: 's2', s2RequestRef } : { source: 'unknown', s2RequestRef: null };
  }
  if (decision.source === 'teacher' || decision.source === 's1') {
    return matchingTeacherCall(host, decision)
      ? { source: decision.source, s2RequestRef: null }
      : { source: 'unknown', s2RequestRef: null };
  }
  if (decision.source === 'rule' || decision.source === 'human' || decision.source === 'none' || decision.source === 'reference') {
    return { source: decision.source, s2RequestRef: null };
  }
  return { source: 'unknown', s2RequestRef: null };
}

function effectReferenceStatus(host: any, effect: any): string | null {
  const row = (host.cross_references?.effect_results ?? []).find((item: any) =>
    item.result_id === effect.result_id && item.revision === effect.revision);
  return row?.attempt_reference_status ?? effect.attempt_reference_status ?? null;
}

export function typedLedgerFromEvidence(host: any) {
  const decisions = Array.isArray(host.decisions) ? host.decisions : [];
  const attempts = Array.isArray(host.attempts) ? host.attempts : [];
  const decisionRows = decisions.map((item: any) => {
    const source = typedDecisionSource(host, item);
    return {
      decision_id: item.decision_id,
      source: source.source,
      selected_action_id: null,
      provider_response_id: null,
      s2_request_ref: source.s2RequestRef,
      at_ms: nullableSafeInteger(item.created_ms) ?? 0,
    };
  });
  const attemptIds = new Set(attempts.map((item: any) => item.attempt_id));
  const effects = (Array.isArray(host.effect_results) ? host.effect_results : []).flatMap((item: any) => {
    if (item.projection_status !== 'typed_effect_result' || !['satisfied', 'unsatisfied', 'unknown'].includes(item.effect_status)
        || typeof item.attempt_id !== 'string' || !attemptIds.has(item.attempt_id)
        || effectReferenceStatus(host, item) !== 'matched') return [];
    const verdict = item.effect_status === 'unsatisfied' ? 'not_satisfied' : item.effect_status;
    return [{ attempt_id: item.attempt_id, verdict, source_sequence: null }];
  });
  return {
    decisions: decisionRows,
    attempts: attempts.map((item: any) => ({
      attempt_id: item.attempt_id,
      decision_id: typeof item.decision_id === 'string' ? item.decision_id : null,
      state: item.state,
      receipt_status: item.state === 'settled' || item.state === 'completed'
        ? 'completed' as const
        : ['failed', 'cancelled', 'rejected', 'preempted'].includes(item.state) ? 'rejected' as const : 'unknown' as const,
    })),
    effects,
  };
}

function modelCallOutcome(call: any): string | null {
  const known = call.scope === 's2_request'
    ? ['proposal', 'unavailable', 'missing', 'conflict', 'invalid', 'quarantined', 'cancelled', 'expired']
    : ['proposal', 'failed', 'unavailable', 'quarantined', 'cancelled', 'expired'];
  if (call.outcome === null) return null;
  assert.ok(known.includes(call.outcome), `unrecognized Host model-call outcome: ${String(call.outcome)}`);
  return call.outcome;
}

export function modelAttributionFromEvidence(host: any) {
  const raw = (Array.isArray(host.call_rows) ? host.call_rows : [])
    .filter((call: any) => call.scope === 's2_request' || call.scope === 's1_decision');
  const calls = raw.map((call: any) => ({
    scope: call.scope,
    root_id: call.root_id,
    request_id: call.scope === 's2_request' ? call.request_id : null,
    decision_id: call.scope === 's1_decision' ? call.decision_id : null,
    generation: call.generation,
    provider: call.provider,
    model: call.model,
    provider_response_id: null,
    started_ms: call.started_ms,
    finished_ms: call.finished_ms,
    outcome: modelCallOutcome(call),
    usage: {
      input_tokens: nullableSafeInteger(call.usage?.input_tokens),
      output_tokens: nullableSafeInteger(call.usage?.output_tokens),
      cost_micros: nullableSafeInteger(call.usage?.cost_micros),
    },
    cost_usd: null,
  }));
  return { admitted_remote_call_rows: calls.length, actual_network_requests: null, calls };
}

export function traceAuditPassed(host: any, typedLedger: any): boolean {
  const cross = host.cross_references;
  if (!record(cross) || cross.s2_plan_request?.time_based_matching_used !== false) return false;
  if (typedLedger.decisions.some((decision: any) => decision.source === 'unknown')) return false;
  const decisionReferences = new Map((cross.decisions ?? []).map((item: any) => [item.decision_id, item]));
  for (const decision of host.decisions ?? []) {
    const reference: any = decisionReferences.get(decision.decision_id);
    if (!reference || !['matched', 'absent'].includes(reference.consumed_attempt_status)
        || !['matched', 'absent'].includes(reference.attempt_map_status)) return false;
    if (decision.source === 'plan' || decision.source === 's2') {
      if (reference.plan_installation_reference_status !== 'matched'
          || reference.s2_request_reference_status !== 'matched' || !reference.s2_request) return false;
      const correlated = s2ReferenceForDecision(host, decision);
      if (!correlated || correlated.request_id !== reference.s2_request.request_id
          || correlated.generation !== reference.s2_request.generation) return false;
    }
  }
  const attemptReferences = cross.attempts ?? [];
  if (attemptReferences.length !== (host.attempts ?? []).length
      || attemptReferences.some((item: any) => !['matched'].includes(item.decision_reference_status)
        || !['matched', 'absent'].includes(item.admission_outbox_status))) return false;
  const modelRows = host.call_rows ?? [];
  if (modelRows.some((call: any) => call.scope === 's1_decision'
      && call.decision_reference_status !== 'matched')) return false;
  const planChecks = cross.plan_installations ?? [];
  const installedPlans = (host.plan_installations ?? []).filter((item: any) => item.state === 'installed');
  if (planChecks.filter((item: any) => item.state === 'installed').length !== installedPlans.length
      || planChecks.some((item: any) => item.state === 'installed'
        && (item.attribution_status !== 'model_request' || item.request_reference_status !== 'matched'))) return false;
  const effectRows = host.effect_results ?? [];
  const effectReferences = cross.effect_results ?? [];
  const effectSummary = cross.effect_results_to_attempts;
  if (effectRows.length !== effectReferences.length
      || effectReferences.some((item: any) => item.attempt_reference_status !== 'matched')
      || typedLedger.effects.length !== effectRows.length) return false;
  if (effectRows.length > 0 && effectSummary?.source_sequence_status !== 'recorded') return false;
  if (effectRows.length === 0 && effectSummary?.status !== 'no_effect_results') return false;
  if (cross.local_inference_revisions?.status === 'present_not_exported') return false;
  return true;
}

export function createRejectedReceipt(identity: Identity, reason: string, cleanup = { stop_status: 'not_requested', cleanup_status: 'clean', owned_processes_remaining: [], owned_directories_remaining: [] }) {
  return {
    version: 1,
    suite: identity.suite,
    task_id: identity.task_id,
    seed: identity.seed,
    root_id: null,
    admission: { accepted: false, rejection_reason: reason },
    status: 'rejected',
    system_reported_completed: false,
    cold_start: false,
    typed_ledger: { decisions: [], attempts: [], effects: [] },
    local_inference_revisions: [],
    evidence_refs: [],
    model_call_attribution: { admitted_remote_call_rows: 0, actual_network_requests: null, calls: [] },
    stop_cleanup: cleanup,
    final_read: { status: 'not_read', source_sequence: null, observed_at_ms: null, evidence_ref: null },
    auditors: { trace: 'unknown', scope: 'unknown', current_ui: 'unknown', stop_cleanup: cleanup.cleanup_status === 'clean' ? 'passed' : 'unknown' },
  };
}

function processAlive(pid: number): boolean {
  try { process.kill(pid, 0); return true; }
  catch (error) { return (error as NodeJS.ErrnoException).code === 'EPERM'; }
}

function minimalProductEnvironment(): NodeJS.ProcessEnv {
  const names = ['HOME', 'LANG', 'LC_ALL', 'LC_CTYPE', 'LOGNAME', 'PATH', 'SHELL', 'TERM', 'USER', 'TMPDIR', 'TMP', 'TEMP'];
  return Object.fromEntries(names.flatMap(name => process.env[name] === undefined ? [] : [[name, process.env[name]!] ]));
}

function waitForChildClose(child: ChildProcess, timeoutMs: number): Promise<{ code: number | null; signal: NodeJS.Signals | null; timedOut: boolean }> {
  return new Promise((resolvePromise, rejectPromise) => {
    let settled = false;
    const finish = (value: { code: number | null; signal: NodeJS.Signals | null; timedOut: boolean }) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.removeListener('error', onError);
      resolvePromise(value);
    };
    const onError = (error: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      rejectPromise(error);
    };
    const timer = setTimeout(() => finish({ code: null, signal: null, timedOut: true }), timeoutMs);
    timer.unref?.();
    child.once('error', onError);
    child.once('close', (code, signal) => finish({ code, signal, timedOut: false }));
  });
}

async function stopExactChild(child: ChildProcess): Promise<{ code: number | null; signal: NodeJS.Signals | null; exitConfirmed: boolean }> {
  if (child.exitCode === null && child.signalCode === null) {
    try { child.kill('SIGTERM'); } catch { /* confirm below */ }
    await Promise.race([new Promise<void>(resolvePromise => child.once('close', () => resolvePromise())), new Promise(resolvePromise => setTimeout(resolvePromise, TERM_GRACE_MS))]);
  }
  if (child.exitCode === null && child.signalCode === null) {
    try { child.kill('SIGKILL'); } catch { /* confirm below */ }
    await Promise.race([new Promise<void>(resolvePromise => child.once('close', () => resolvePromise())), new Promise(resolvePromise => setTimeout(resolvePromise, KILL_WAIT_MS))]);
  }
  return {
    code: child.exitCode,
    signal: child.signalCode,
    exitConfirmed: child.exitCode !== null || child.signalCode !== null,
  };
}

function waitForFixtureReady(child: ChildProcess, timeoutMs = 15_000): Promise<{ pid: number; window_number: number }> {
  return new Promise((resolvePromise, rejectPromise) => {
    let buffer = '';
    let settled = false;
    const fail = (error: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      rejectPromise(error);
    };
    const timer = setTimeout(() => fail(new ProductDriverError('FIXTURE_STARTUP_TIMEOUT')), timeoutMs);
    timer.unref?.();
    child.once('error', fail);
    child.once('close', (code, signal) => fail(new ProductDriverError('FIXTURE_EXITED_BEFORE_READY', `fixture exited before readiness (${code ?? signal})`)));
    child.stdout?.on('data', chunk => {
      if (settled) return;
      try {
        buffer += Buffer.from(chunk).toString('utf8');
        assert.ok(Buffer.byteLength(buffer, 'utf8') <= 1024, 'fixture startup output exceeds its bound');
        const newline = buffer.indexOf('\n');
        if (newline < 0) return;
        assert.equal(newline, buffer.length - 1, 'fixture startup output must contain one exact line');
        const ready = parseJson(Buffer.from(buffer.slice(0, newline)), 'fixture ready record');
        exactKeys(ready, ['pid', 'window_number'], 'fixture ready record');
        assert.equal(ready.pid, child.pid, 'fixture ready PID differs from the owned child');
        assert.ok(Number.isSafeInteger(ready.window_number) && ready.window_number > 0 && ready.window_number <= 0xffff_ffff);
        settled = true;
        clearTimeout(timer);
        resolvePromise({ pid: ready.pid, window_number: ready.window_number });
      } catch (error) {
        fail(error instanceof Error ? error : new Error(String(error)));
      }
    });
  });
}

async function readSnapshot(path: string, identity: Identity, maximum = 128 * 1024) {
  const bytes = readPrivateRegularFile(path, 'fixture snapshot', maximum);
  const snapshot = parseJson(bytes, 'fixture snapshot');
  assert.equal(snapshot.version, 1);
  assert.equal(snapshot.task_id, identity.task_id);
  assert.equal(snapshot.seed, identity.seed);
  assert.ok(record(snapshot.values) && record(snapshot.audit));
  return { bytes, snapshot, sha256: sha256(bytes) };
}

function fixtureBaselineIsUntouched(snapshot: any): boolean {
  const audit = snapshot.audit;
  const counts = audit?.change_counts;
  return audit?.revision === 0
    && record(counts) && Object.keys(counts).length > 0
    && Object.values(counts).every(value => value === 0)
    && audit.background_change_count === 0
    && audit.other_row_change_count === 0
    && audit.revisit_count === 0
    && audit.row_rebind_count === 0;
}

function ensureWithin(parent: string, child: string, label: string): string {
  const resolved = resolve(child);
  const rel = relative(parent, resolved);
  assert.ok(rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel), `${label} escapes the run directory`);
  return resolved;
}

function assertNewOutputPath(path: string): void {
  assert.ok(!existsSync(path), 'driver receipt path already exists');
  assert.equal(realpathSync(dirname(path)), dirname(path), 'receipt directory must be canonical');
}

async function run(args: DriverArgs): Promise<number> {
  const identity: Identity = { suite: args.suite, task_id: args.taskId, seed: args.seed };
  const requestFile = ensureWithin(args.outputDirectory, args.requestFile, 'request file');
  const resetFile = ensureWithin(args.outputDirectory, args.resetFile, 'launcher reset file');
  const receiptPath = ensureWithin(args.outputDirectory, args.receiptPath, 'receipt path');
  assertPrivateOutputDirectory(args.outputDirectory);
  assertNewOutputPath(receiptPath);
  assert.equal(requestFile, resolve(args.outputDirectory, 'request.json'));
  assert.equal(resetFile, resolve(args.outputDirectory, 'launcher-reset.json'));
  assert.equal(receiptPath, resolve(args.outputDirectory, 'driver-receipt.json'));

  const requestBytes = readPrivateRegularFile(requestFile, 'per-run request', MAX_INPUT_BYTES);
  const resetBytes = readPrivateRegularFile(resetFile, 'launcher reset', MAX_INPUT_BYTES);
  const request = parseJson(requestBytes, 'per-run request');
  const reset = parseJson(resetBytes, 'launcher reset');
  validateProductDriverInputs({ identity, request, reset });
  const goalsBytes = readPrivateRegularFile(args.taskGoalsFile, 'task-goals file', MAX_GOALS_BYTES);
  const manifestBytes = readPrivateRegularFile(args.goalManifestFile, 'goal catalog manifest', MAX_MANIFEST_BYTES);
  const selected = validateGoalCatalog({
    goalsBytes,
    manifestBytes,
    goalsSha256: args.taskGoalsSha256,
    manifestSha256: args.goalManifestSha256,
    identity,
    request,
  });
  if (args.suite !== 'fixture') {
    const receipt = createRejectedReceipt(identity, 'textedit_requires_the_product_owned_document_driver');
    validateDriverReceipt(receipt, identity);
    writeJsonExclusive(receiptPath, receipt, 4 * 1024 * 1024);
    return 0;
  }
  const fixtureScopeFamily = fixtureScopeFamilyForTaskId(args.taskId);
  if (!fixtureScopeFamily) {
    const receipt = createRejectedReceipt(identity, 'fixture_scope_is_not_implemented_by_this_product_driver');
    validateDriverReceipt(receipt, identity);
    writeJsonExclusive(receiptPath, receipt, 4 * 1024 * 1024);
    return 0;
  }
  validateFixtureAllowedScope(selected.selected.goal.allowed_scope, args.taskId);

  for (const [path, label] of [[FIXTURE_BINARY, 'MVP fixture binary'], [ELECTRON_RUNTIME, 'Electron runtime'],
    [NATIVE_GUARD, 'native guard'], [CONSENT_DRIVER, 'native consent driver'], [MAIN_HARNESS, 'normal Main harness']] as const) {
    const stat = lstatSync(path);
    assert.ok(stat.isFile() && !stat.isSymbolicLink() && realpathSync(path) === path, `${label} is not a canonical regular file`);
    if (label !== 'normal Main harness') assert.ok((stat.mode & 0o111) !== 0, `${label} is not executable`);
  }

  const workDirectory = resolve(args.outputDirectory, 'product-run');
  mkdirSync(workDirectory, { mode: 0o700 });
  assertPrivateOutputDirectory(workDirectory);
  const fixtureSnapshotPath = resolve(workDirectory, 'fixture-live-snapshot.json');
  const tempRoot = process.env.TMPDIR;
  assert.ok(tempRoot && isAbsolute(tempRoot), 'coordinator private TMPDIR is missing');
  const tempStat = lstatSync(tempRoot);
  assert.ok(tempStat.isDirectory() && !tempStat.isSymbolicLink() && realpathSync(tempRoot) === tempRoot);
  assert.equal(tempStat.uid, process.getuid?.());
  assert.equal(tempStat.mode & 0o777, 0o700);

  const originalHashes = {
    request: sha256(requestBytes), reset: sha256(resetBytes), goals: args.taskGoalsSha256,
    manifest: args.goalManifestSha256,
    fixture: hashStableRegularFile(FIXTURE_BINARY, 'MVP fixture binary', 512 * 1024 * 1024, true),
    electron: hashStableRegularFile(ELECTRON_RUNTIME, 'Electron runtime', 512 * 1024 * 1024, true),
    guard: hashStableRegularFile(NATIVE_GUARD, 'native guard', 512 * 1024 * 1024, true),
    consentDriver: hashStableRegularFile(CONSENT_DRIVER, 'native consent driver', 128 * 1024 * 1024, true),
    mainHarness: hashStableRegularFile(MAIN_HARNESS, 'normal Main harness', MAX_OUTPUT_BYTES),
  };
  let fixture: ChildProcess | null = null;
  let electron: ChildProcess | null = null;
  let mainTmpRoot: string | null = null;
  let mainOutputDirectory: string | null = null;
  let mainResultPath = '';
  let stagedMainResultPath: string | null = null;
  let stagedHostLedgerPath: string | null = null;
  let finalReadPath = '';
  let hostLedgerPath = '';
  let fixtureReady: { pid: number; window_number: number } | null = null;
  let electronExit: { code: number | null; signal: NodeJS.Signals | null; timedOut: boolean } | null = null;
  let fixtureExit: { code: number | null; signal: NodeJS.Signals | null; exitConfirmed: boolean } | null = null;
  let mainResult: any = null;
  let tempDirectoriesBeforeShutdown: string[] = [];
  let ownedProcessIds: number[] = [];
  let stderr = '';
  try {
    fixture = spawn(FIXTURE_BINARY, ['--profile', resetFile, '--snapshot', fixtureSnapshotPath], {
      cwd: workDirectory,
      env: { ...minimalProductEnvironment(), TMPDIR: tempRoot, TMP: tempRoot, TEMP: tempRoot },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    fixture.stderr?.on('data', chunk => { stderr = (stderr + Buffer.from(chunk).toString('utf8')).slice(-MAX_LOG_BYTES); });
    fixtureReady = await waitForFixtureReady(fixture);
    const initialSnapshot = await waitForStableSnapshot(fixtureSnapshotPath, identity);
    assert.ok(fixtureBaselineIsUntouched(initialSnapshot.snapshot), 'fixture did not start at an untouched frozen baseline');
    assert.equal(process.platform, 'darwin', 'the native product driver requires macOS');
    assert.equal(realpathSync(SYSTEM_PRIVATE_TEMP_ROOT), SYSTEM_PRIVATE_TEMP_ROOT, 'system temporary root is not canonical');
    mainTmpRoot = mkdtempSync(resolve(SYSTEM_PRIVATE_TEMP_ROOT, 'cu-product-main-tmp-'));
    mainOutputDirectory = mkdtempSync(resolve(SYSTEM_PRIVATE_TEMP_ROOT, 'cu-product-main-output-'));
    assertPrivateOutputDirectory(mainTmpRoot);
    assertPrivateOutputDirectory(mainOutputDirectory);
    mainResultPath = resolve(mainOutputDirectory, 'normal-main-result.json');
    finalReadPath = resolve(mainOutputDirectory, 'final-current-ui.json');
    hostLedgerPath = resolve(mainOutputDirectory, 'host-ledger-evidence.json');
    const env: NodeJS.ProcessEnv = {
      ...minimalProductEnvironment(),
      TMPDIR: mainTmpRoot,
      TMP: mainTmpRoot,
      TEMP: mainTmpRoot,
      CU_PRODUCT_ROOT: ROOT,
      CU_PRODUCT_MAIN_RESULT_PATH: mainResultPath,
      CU_PRODUCT_FINAL_READ_PATH: finalReadPath,
      CU_PRODUCT_HOST_LEDGER_PATH: hostLedgerPath,
      CU_PRODUCT_NODE_RUNTIME: realpathSync(process.execPath),
      CU_PRODUCT_FIXTURE_PID: String(fixture.pid),
      CU_PRODUCT_FIXTURE_WINDOW_NUMBER: String(fixtureReady.window_number),
      CU_PRODUCT_FIXTURE_TASK_ID: identity.task_id,
      CU_PRODUCT_FIXTURE_SEED: String(identity.seed),
      CU_PRODUCT_CATALOG_ID: selected.catalogId,
      CU_PRODUCT_SELECTOR: selected.selected.selector,
      CU_PRODUCT_CONSENT_DRIVER: CONSENT_DRIVER,
      CU_BIN_DIR: resolve(ROOT, 'target/debug'),
      CU_SOURCE_BACKEND: 'native',
      CU_TARGET_BUNDLE_ID: FIXTURE_BUNDLE_ID,
      CU_TARGET_PID: String(fixture.pid),
      CU_SOURCE_READ_WINDOW_ID: `window-${fixtureReady.window_number}`,
      CU_NATIVE_DEVICE_GUARD: NATIVE_GUARD,
      CU_FROZEN_TASK_GOALS_PATH: args.taskGoalsFile,
      CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH: args.goalManifestFile,
      CU_FROZEN_TASK_GOALS_SHA256: args.taskGoalsSha256,
      CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256: args.goalManifestSha256,
    };
    delete (env as Record<string, string | undefined>).CU_NATIVE_REFERENCE_PLANNER;
    delete (env as Record<string, string | undefined>).ELECTRON_RENDERER_URL;
    for (const name of Object.keys(env)) {
      if (/(?:API.?KEY|ACCESS.?TOKEN|(?:OPENAI|ANTHROPIC)_|MODEL.*(?:KEY|TOKEN))/i.test(name)) {
        delete (env as Record<string, string | undefined>)[name];
      }
    }
    electron = spawn(ELECTRON_RUNTIME, [MAIN_HARNESS], {
      cwd: ROOT,
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    ownedProcessIds = [fixture.pid, electron.pid]
      .filter((pid): pid is number => Number.isSafeInteger(pid) && pid > 0);
    electron.stdout?.on('data', () => {});
    electron.stderr?.on('data', () => {});
    electronExit = await waitForChildClose(electron, DRIVER_TIMEOUT_MS);
    if (electronExit.timedOut) {
      await stopExactChild(electron);
      throw new ProductDriverError('NORMAL_MAIN_TIMEOUT');
    }
    if (electronExit.code !== 0 || electronExit.signal !== null) {
      throw new ProductDriverError('NORMAL_MAIN_NONZERO_EXIT', `normal Main harness exited (${electronExit.code ?? electronExit.signal})`);
    }
    const stagedMainResult = stageNormalMainResult(mainOutputDirectory!, workDirectory);
    mainResult = stagedMainResult.result;
    stagedMainResultPath = stagedMainResult.path;
    tempDirectoriesBeforeShutdown = Array.isArray(mainResult.supervisor_temp_directories)
      ? mainResult.supervisor_temp_directories.map(String) : [];
    ownedProcessIds = [fixture.pid, electron.pid, ...(Array.isArray(mainResult.native_child_pids)
      ? mainResult.native_child_pids.map((item: any) => Number(item.pid)) : [])]
      .filter((pid): pid is number => Number.isSafeInteger(pid) && pid > 0);
    const stagedFinalRead = mainOutputDirectory
      ? stageNormalMainFinalRead(mainOutputDirectory, workDirectory) : null;
    if (mainResult.goal_confirmation_received === true) {
      assert.equal(mainResult.goal_confirmation?.objective_sha256, sha256(request.objective),
        'normal Goal confirmation text differs from the coordinator-verified frozen objective');
      if (stagedMainResult.unconfirmedMainFailure) {
        assert.equal(fixtureGoalConfirmationMatchesRequest(
          mainResult.goal_confirmation, identity.task_id, selected.selected.goal.allowed_scope,
        ), true, 'unaccepted Goal prompt differs from the selected frozen scope');
      }
    }
    if (stagedMainResult.unconfirmedMainFailure) throw stagedMainResult.unconfirmedMainFailure;
    const finalSnapshot = await readStableSnapshot(fixtureSnapshotPath, identity);
    writeJsonExclusive(resolve(workDirectory, 'fixture-initial.json'), initialSnapshot.snapshot, 2 * 1024 * 1024);
    writeJsonExclusive(resolve(workDirectory, 'fixture-final.json'), finalSnapshot.snapshot, 2 * 1024 * 1024);
    const mainResultRef = fileEvidence(stagedMainResult.path, args.outputDirectory);
    const initialSnapshotRef = fileEvidence(resolve(workDirectory, 'fixture-initial.json'), args.outputDirectory);
    const finalSnapshotRef = fileEvidence(resolve(workDirectory, 'fixture-final.json'), args.outputDirectory);
    const evidenceRefs = [mainResultRef, initialSnapshotRef, finalSnapshotRef];
    let hostEvidence: any = null;
    let hostEvidenceRef: ReturnType<typeof fileEvidence> | null = null;
    if (typeof mainResult.root_id === 'string' && mainResult.root_id.trim()) {
      assert.ok(existsSync(hostLedgerPath), 'accepted Root has no read-only Host evidence export');
      const hostBytes = readPrivateRegularFile(hostLedgerPath, 'Root-scoped Host evidence', MAX_OUTPUT_BYTES);
      hostEvidence = parseJson(hostBytes, 'Root-scoped Host evidence');
      assert.equal(hostEvidence.scope?.root_id, mainResult.root_id, 'Host evidence belongs to a different Root');
      assert.equal(hostEvidence.root?.task_revision, mainResult.root_task_revision,
        'Host ledger Root task revision differs from the confirmed Goal revision');
      stagedHostLedgerPath = resolve(workDirectory, 'host-ledger-evidence.json');
      writePrivateCopy(stagedHostLedgerPath, hostBytes, MAX_OUTPUT_BYTES);
      hostEvidenceRef = fileEvidence(stagedHostLedgerPath, args.outputDirectory);
      evidenceRefs.push(hostEvidenceRef);
    }

    const fileHashesAfter = {
      request: sha256(readPrivateRegularFile(requestFile, 'per-run request after child', MAX_INPUT_BYTES)),
      reset: sha256(readPrivateRegularFile(resetFile, 'launcher reset after child', MAX_INPUT_BYTES)),
      goals: sha256(readPrivateRegularFile(args.taskGoalsFile, 'task-goals after child', MAX_GOALS_BYTES)),
      manifest: sha256(readPrivateRegularFile(args.goalManifestFile, 'goal manifest after child', MAX_MANIFEST_BYTES)),
      fixture: hashStableRegularFile(FIXTURE_BINARY, 'MVP fixture binary after child', 512 * 1024 * 1024, true),
      electron: hashStableRegularFile(ELECTRON_RUNTIME, 'Electron runtime after child', 512 * 1024 * 1024, true),
      guard: hashStableRegularFile(NATIVE_GUARD, 'native guard after child', 512 * 1024 * 1024, true),
      consentDriver: hashStableRegularFile(CONSENT_DRIVER, 'native consent driver after child', 128 * 1024 * 1024, true),
      mainHarness: hashStableRegularFile(MAIN_HARNESS, 'normal Main harness after child', MAX_OUTPUT_BYTES),
    };
    assert.deepEqual(fileHashesAfter, originalHashes, 'pinned request/reset/catalog/product input changed during execution');
    fixtureExit = await stopExactChild(fixture);
    const mainOutputClean = mainOutputDirectory !== null && removePrivateOutputDirectory(mainOutputDirectory, [
      'normal-main-result.json', 'final-current-ui.json', 'host-ledger-evidence.json',
    ]);
    const mainTmpClean = mainTmpRoot !== null && removeEmptyPrivateDirectory(mainTmpRoot);
    await delay(250);
    const remainingPids: number[] = [];
    for (const pid of ownedProcessIds) if (processAlive(pid)) remainingPids.push(pid);
    const remainingDirectories = [
      ...tempDirectoriesBeforeShutdown.filter(path => existsSync(path)),
      ...(mainOutputDirectory && existsSync(mainOutputDirectory) ? [mainOutputDirectory] : []),
      ...(mainTmpRoot && existsSync(mainTmpRoot) ? [mainTmpRoot] : []),
    ];
    const cleanupStatus = fixtureExit.exitConfirmed && !electronExit.timedOut && mainOutputClean && mainTmpClean
      && remainingPids.length === 0
      && remainingDirectories.length === 0 ? 'clean' : 'incomplete';
    const mainStatus = mainResult.status;
    const rootId = typeof mainResult.root_id === 'string' ? mainResult.root_id : null;
    const goalScopeMatchesRequest = rootId !== null && fixtureGoalConfirmationMatchesRequest(
      mainResult.goal_confirmation, identity.task_id, selected.selected.goal.allowed_scope,
    );
    let receipt: any;
    if (!rootId) {
      const reason = typeof mainResult.rejection_reason === 'string' ? mainResult.rejection_reason
        : mainStatus === 'blocked' ? 'product_model_or_target_authorization_blocked'
          : mainStatus === 'rejected' ? 'host_refused_frozen_task_before_root_creation'
            : 'product_task_not_admitted';
      receipt = createRejectedReceipt(identity, reason, {
        stop_status: 'not_requested',
        cleanup_status: cleanupStatus,
        owned_processes_remaining: remainingPids,
        owned_directories_remaining: remainingDirectories,
      });
      receipt.evidence_refs = evidenceRefs;
      receipt.auditors.stop_cleanup = cleanupStatus === 'clean' ? 'passed' : 'failed';
      receipt.auditors.current_ui = mainResult.current_ui_ready === true ? 'passed' : 'unknown';
      receipt.auditors.scope = mainResult.selection_verified === true && goalScopeMatchesRequest ? 'passed' : 'unknown';
    } else {
      assert.ok(hostEvidence, 'accepted Root requires read-only Host ledger evidence');
      const typedLedger = typedLedgerFromEvidence(hostEvidence);
      const modelAttribution = modelAttributionFromEvidence(hostEvidence);
      const finalReadBytes = stagedFinalRead?.bytes ?? null;
      const finalRead = finalReadBytes ? parseJson(finalReadBytes, 'Main final current-UI evidence') : null;
      let finalReadRef: ReturnType<typeof fileEvidence> | null = null;
      let finalReadProjection: any = null;
      if (finalRead && Number.isSafeInteger(finalRead.source_sequence) && finalRead.source_sequence > 0
          && Number.isSafeInteger(finalRead.observed_at_ms) && finalRead.observed_at_ms >= 0
          && JSON.stringify(finalRead) === JSON.stringify(mainResult.final_current_ui)) {
        finalReadProjection = finalRead;
        finalReadRef = fileEvidence(stagedFinalRead!.path, args.outputDirectory);
        evidenceRefs.push(finalReadRef);
      }
      const tracePassed = traceAuditPassed(hostEvidence, typedLedger);
      const completed = mainStatus === 'completed' && mainResult.root_state === 'completed';
      const status = completed ? 'completed'
        : mainStatus === 'cancelled' ? 'cancelled'
          : mainStatus === 'timeout' ? 'timeout'
            : mainStatus === 'unverified' ? 'unverified'
          : mainStatus === 'failed' ? 'failed' : 'unknown';
      receipt = {
        version: 1,
        suite: identity.suite,
        task_id: identity.task_id,
        seed: identity.seed,
        root_id: rootId,
        admission: { accepted: true, rejection_reason: null },
        status,
        system_reported_completed: completed,
        cold_start: mainResult.cold_start === true && fixtureBaselineIsUntouched(initialSnapshot.snapshot),
        typed_ledger: typedLedger,
        local_inference_revisions: [],
        evidence_refs: evidenceRefs,
        model_call_attribution: modelAttribution,
        stop_cleanup: {
          stop_status: ['released', 'not_requested'].includes(mainResult.stop_status) ? mainResult.stop_status
            : mainResult.stop_status === 'confirmed' ? 'released' : 'unknown',
          cleanup_status: cleanupStatus,
          owned_processes_remaining: remainingPids,
          owned_directories_remaining: remainingDirectories,
        },
        final_read: finalReadRef && finalReadProjection ? {
          status: 'current_ui',
          source_sequence: finalReadProjection.source_sequence,
          observed_at_ms: finalReadProjection.observed_at_ms,
          evidence_ref: finalReadRef.path,
        } : { status: 'unknown', source_sequence: null, observed_at_ms: null, evidence_ref: null },
        auditors: {
          trace: tracePassed ? 'passed' : 'unknown',
          scope: mainResult.selection_verified === true && mainResult.root_frozen_profile_matches === true
            && goalScopeMatchesRequest ? 'passed' : 'unknown',
          current_ui: finalReadRef && mainResult.source_target_matches_fixture === true ? 'passed' : 'unknown',
          stop_cleanup: cleanupStatus === 'clean' && mainResult.stop_confirmed === true ? 'passed' : 'unknown',
        },
      };
    }
    validateDriverReceipt(receipt, identity);
    writeJsonExclusive(receiptPath, receipt, 4 * 1024 * 1024);
    const diagnostic = {
      version: 1,
      identity,
      request_sha256: originalHashes.request,
      reset_sha256: originalHashes.reset,
      task_goals_sha256: originalHashes.goals,
      goal_manifest_sha256: originalHashes.manifest,
      catalog_id: selected.catalogId,
      selector: selected.selected.selector,
      fixture_pid: fixture.pid,
      fixture_ready: fixtureReady,
      electron_pid: electron.pid,
      electron_exit: electronExit,
      fixture_exit: fixtureExit,
      cleanup_status: cleanupStatus,
      main_output_directory_removed: mainOutputClean,
      main_tmp_root_removed: mainTmpClean,
      rejected_or_blocked_reason: receipt.admission.accepted ? null : receipt.admission.rejection_reason,
      evidence_refs: evidenceRefs,
      stderr_tail: stderr.slice(-MAX_LOG_BYTES),
      main_status: mainStatus,
      goal_scope_matches_request: goalScopeMatchesRequest,
      initial_fixture_baseline_verified: fixtureBaselineIsUntouched(initialSnapshot.snapshot),
    };
    writeJsonExclusive(resolve(workDirectory, 'driver-diagnostic.json'), diagnostic, MAX_OUTPUT_BYTES);
    return 0;
  } catch (error) {
    let electronStop: { code: number | null; signal: NodeJS.Signals | null; exitConfirmed: boolean } | null = null;
    if (electron) electronStop = await stopExactChild(electron);
    if (fixture) fixtureExit = await stopExactChild(fixture);
    if (electron || fixture) await delay(250);
    if (!stagedMainResultPath && mainOutputDirectory && existsSync(mainResultPath)) {
      try {
        const stagedMainResult = stageNormalMainResult(mainOutputDirectory, workDirectory);
        stagedMainResultPath = stagedMainResult.path;
        mainResult = stagedMainResult.result;
        tempDirectoriesBeforeShutdown = Array.isArray(mainResult.supervisor_temp_directories)
          ? mainResult.supervisor_temp_directories.map(String) : [];
        ownedProcessIds = [fixture?.pid, electron?.pid, ...(Array.isArray(mainResult.native_child_pids)
          ? mainResult.native_child_pids.map((item: any) => Number(item.pid)) : [])]
          .filter((pid): pid is number => Number.isSafeInteger(pid) && pid > 0);
      } catch { /* Preserve the original failure if a bounded Main result is unavailable. */ }
    }
    if (!stagedHostLedgerPath && mainOutputDirectory) {
      try {
        const stagedHostLedger = stageNormalMainHostLedger(mainOutputDirectory, workDirectory);
        stagedHostLedgerPath = stagedHostLedger?.path ?? null;
      } catch { /* A missing or malformed ledger remains an explicit evidence gap. */ }
    }
    const remainingPids = ownedProcessIds.filter(pid => processAlive(pid));
    const mainOutputClean = mainOutputDirectory !== null && removePrivateOutputDirectory(mainOutputDirectory, [
      'normal-main-result.json', 'final-current-ui.json', 'host-ledger-evidence.json',
    ]);
    const mainTmpClean = mainTmpRoot !== null && removeEmptyPrivateDirectory(mainTmpRoot);
    const remainingDirectories = [
      ...tempDirectoriesBeforeShutdown.filter(path => existsSync(path)),
      ...(mainOutputDirectory && existsSync(mainOutputDirectory) ? [mainOutputDirectory] : []),
      ...(mainTmpRoot && existsSync(mainTmpRoot) ? [mainTmpRoot] : []),
    ];
    let mainResultEvidence: ReturnType<typeof fileEvidence> | null = null;
    if (stagedMainResultPath && existsSync(stagedMainResultPath)) {
      try { mainResultEvidence = fileEvidence(stagedMainResultPath, args.outputDirectory); } catch { /* Preserve failure details without masking the run error. */ }
    }
    let hostLedgerEvidence: ReturnType<typeof fileEvidence> | null = null;
    if (stagedHostLedgerPath && existsSync(stagedHostLedgerPath)) {
      try { hostLedgerEvidence = fileEvidence(stagedHostLedgerPath, args.outputDirectory); } catch { /* Preserve failure details without masking the run error. */ }
    }
    const message = error instanceof Error ? error.message : String(error);
    try {
      writeJsonExclusive(resolve(workDirectory, 'driver-failure.json'), {
        version: 1,
        identity,
        failure_code: error instanceof ProductDriverError ? error.code : 'PRODUCT_DRIVER_FAILED',
        message: message.slice(0, 1024),
        fixture_pid: fixture?.pid ?? null,
        fixture_exit: fixtureExit,
        electron_pid: electron?.pid ?? null,
        electron_exit: electronExit,
        electron_stop: electronStop,
        main_result_evidence: mainResultEvidence,
        host_ledger_evidence: hostLedgerEvidence,
        main_result_summary: mainResult ? {
          status: mainResult.status,
          root_id: typeof mainResult.root_id === 'string' ? mainResult.root_id : null,
          failure_step: typeof mainResult.failure_step === 'string' ? mainResult.failure_step : null,
          observed_root: mainResult.observed_root ?? null,
          goal_confirmation_received: mainResult.goal_confirmation_received === true,
          goal_confirmation_accepted: mainResult.goal_confirmation_accepted === true,
          host_ledger_export_failure: mainResult.host_ledger_export_failure ?? null,
        } : null,
        owned_processes_remaining: remainingPids,
        owned_directories_remaining: remainingDirectories,
        main_output_directory_removed: mainOutputClean,
        main_tmp_root_removed: mainTmpClean,
      }, 64 * 1024);
    } catch { /* Keep the original process failure as the coordinator result. */ }
    throw error;
  }
}

async function waitForStableSnapshot(path: string, identity: Identity, timeoutMs = 12_000) {
  const end = Date.now() + timeoutMs;
  let lastError: unknown;
  while (Date.now() < end) {
    try { return await readSnapshot(path, identity); }
    catch (error) { lastError = error; await new Promise(resolvePromise => setTimeout(resolvePromise, 100)); }
  }
  throw new ProductDriverError('FIXTURE_SNAPSHOT_TIMEOUT', `fixture snapshot not ready: ${String(lastError ?? 'unavailable')}`);
}

async function readStableSnapshot(path: string, identity: Identity) {
  return readSnapshot(path, identity);
}

async function main(): Promise<void> {
  try {
    const args = parseProductDriverArgs();
    await run(args);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`Product driver rejected the run: ${message.slice(0, 768)}\n`);
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  void main();
}
