#!/usr/bin/env node
import { createHash } from 'node:crypto';
import {
  closeSync, constants as fsConstants, fchmodSync, fstatSync, lstatSync, mkdirSync,
  mkdtempSync, openSync, readSync, realpathSync, statSync, writeFileSync,
  type Stats,
} from 'node:fs';
import { spawn, execFileSync, type ChildProcess } from 'node:child_process';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { loadFixtureReset } from './mvp/fixture-reset.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: {
  runtime: { type: 'string' }, guard: { type: 'string' }, 'target-spec': { type: 'string' },
  'task-id': { type: 'string' }, seed: { type: 'string' },
  'request-file': { type: 'string' }, 'freeze-file': { type: 'string' },
  'reset-profile': { type: 'string' }, record: { type: 'string' }, execute: { type: 'boolean' },
  'activation-only': { type: 'boolean' },
} });

const bundleId = 'com.lingxi.computer-user.mvp-fixture';
const runtimePath = values.runtime;
const guardInput = values.guard;
const targetSpecPath = values['target-spec'];
const taskId = values['task-id'];
const seed = Number(values.seed);
if (!values.execute) throw new Error('This driver requires explicit --execute; it never authorizes a window or confirms a task without it.');
if (!runtimePath || !isAbsolute(runtimePath) || !statSync(runtimePath, { throwIfNoEntry: false })?.isFile()) {
  throw new Error('--runtime must be an absolute existing Electron executable.');
}
if (!guardInput || !isAbsolute(guardInput) || !statSync(guardInput, { throwIfNoEntry: false })?.isFile()
    || (statSync(guardInput).mode & 0o111) === 0 || !/^owned-row-[A-Za-z0-9][A-Za-z0-9._-]*$/.test(basename(realpathSync(guardInput)))) {
  throw new Error('--guard must be an executable named owned-row-* in an absolute path.');
}
if (!targetSpecPath || !isAbsolute(targetSpecPath)) throw new Error('--target-spec must name a separate absolute public TargetSpec JSON file.');
if (!taskId || !/^LIST-0[1-4]$/.test(taskId) || !Number.isInteger(seed) || seed < 1 || seed > 5) {
  throw new Error('--task-id must be LIST-01..04 and --seed must be 1..5; these values are used only to reset the fixture.');
}
const frozenMode = Boolean(values['request-file'] || values['freeze-file']);
const diagnosticMode = Boolean(values['reset-profile']);
if (frozenMode === diagnosticMode || (frozenMode && (!values['request-file'] || !values['freeze-file']))) {
  throw new Error('Provide exactly one reset source: --request-file with --freeze-file, or --reset-profile.');
}

function fail(message: string): never { throw new Error(message); }
function sameFile(a: Stats, b: Stats): boolean {
  return a.dev === b.dev && a.ino === b.ino && a.nlink === b.nlink && a.size === b.size
    && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs;
}
function readStable(path: string, label: string, maxBytes: number, privateMode = false): Buffer {
  if (!isAbsolute(path)) fail(`${label} path must be absolute`);
  const before = lstatSync(path);
  if (!before.isFile() || before.isSymbolicLink() || before.nlink !== 1 || before.size <= 0 || before.size > maxBytes
      || before.uid !== process.getuid?.()) fail(`${label} must be a bounded owner-owned regular file`);
  if (privateMode && (before.mode & 0o777) !== 0o600) fail(`${label} must have mode 0600`);
  if (realpathSync(path) !== path) fail(`${label} path may not contain symlinks`);
  const fd = openSync(path, fsConstants.O_RDONLY | fsConstants.O_NOFOLLOW);
  try {
    const opened = fstatSync(fd);
    if (!sameFile(before, opened)) fail(`${label} changed before read`);
    const bytes = Buffer.alloc(opened.size);
    let offset = 0;
    while (offset < bytes.length) {
      const count = readSync(fd, bytes, offset, bytes.length - offset, offset);
      if (count <= 0) fail(`${label} ended early`);
      offset += count;
    }
    if (!sameFile(opened, fstatSync(fd)) || !sameFile(opened, lstatSync(path))) fail(`${label} changed while read`);
    return bytes;
  } finally { closeSync(fd); }
}
function exactKeys(value: unknown, required: string[], optional: string[] = []): asserts value is Record<string, any> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail('TargetSpec must be a JSON object');
  const keys = Object.keys(value as object);
  if (required.some(key => !keys.includes(key)) || keys.some(key => !required.includes(key) && !optional.includes(key))) {
    fail(`Unexpected TargetSpec keys: ${keys.join(',')}`);
  }
}
function boundedString(value: unknown, max: number, label: string, allowControls = false): asserts value is string {
  if (typeof value !== 'string' || Buffer.byteLength(value, 'utf8') < 1 || Buffer.byteLength(value, 'utf8') > max
      || (!allowControls && /[\u0000-\u001f\u007f-\u009f]/u.test(value)) || !value.trim()) fail(`${label} is invalid`);
}
function validateTargetSpec(value: unknown): Record<string, any> {
  exactKeys(value, ['semantic_target', 'label'], ['group', 'role_hint', 'row']);
  boundedString(value.semantic_target, 256, 'semantic_target');
  boundedString(value.label, 256, 'target label');
  if (value.group !== undefined && value.group !== null) boundedString(value.group, 256, 'target group');
  if (value.role_hint !== undefined && value.role_hint !== null && value.role_hint !== 'AXRow') fail('row target role_hint, if present, must be AXRow');
  if (!value.row || typeof value.row !== 'object' || Array.isArray(value.row)) fail('TargetSpec must independently include a row selector');
  exactKeys(value.row, ['table_scope', 'conditions']);
  exactKeys(value.row.table_scope, ['scroll_path', 'column_headers']);
  const scope = value.row.table_scope;
  if (!Array.isArray(scope.scroll_path) || scope.scroll_path.length < 1 || scope.scroll_path.length > 8) fail('row scroll_path must contain 1..8 public ancestors');
  for (const part of scope.scroll_path) {
    exactKeys(part, ['role', 'label']);
    if (part.role !== 'ax_scroll_area') fail('row scroll_path only supports ax_scroll_area');
    boundedString(part.label, 256, 'scroll path label');
  }
  if (!Array.isArray(scope.column_headers) || scope.column_headers.length < 1 || scope.column_headers.length > 64) fail('column_headers must contain 1..64 entries');
  const headers = new Set<string>();
  for (const header of scope.column_headers) {
    boundedString(header, 256, 'column header');
    const key = header.trim().toLocaleLowerCase('en-US');
    if (headers.has(key)) fail('column_headers contains duplicate normalized headers');
    headers.add(key);
  }
  if (!Array.isArray(value.row.conditions) || value.row.conditions.length < 1 || value.row.conditions.length > 32) fail('row conditions must contain 1..32 entries');
  const conditions = new Set<string>();
  for (const condition of value.row.conditions) {
    exactKeys(condition, ['column_name', 'value']);
    boundedString(condition.column_name, 256, 'condition column_name');
    if (!scope.column_headers.includes(condition.column_name)) fail('condition column must be an exact member of column_headers');
    const key = condition.column_name.trim().toLocaleLowerCase('en-US');
    if (conditions.has(key)) fail('row conditions contain a duplicate column');
    conditions.add(key);
    exactKeys(condition.value, ['type', 'value']);
    if (condition.value.type === 'text') boundedString(condition.value.value, 2048, 'condition text', true);
    else if (condition.value.type === 'bool') {
      if (typeof condition.value.value !== 'boolean') fail('condition bool value is invalid');
    } else if (condition.value.type === 'number') {
      if (!Number.isSafeInteger(condition.value.value) || Math.abs(condition.value.value) > 1_000_000_000_000) fail('condition number must be a bounded integer');
    } else fail('condition value type must be text, bool, or number');
  }
  return value;
}
function validateResetProfile(value: unknown): Record<string, any> {
  exactKeys(value, ['version', 'task_id', 'seed', 'initial_state']);
  if (value.version !== 1 || value.task_id !== taskId || value.seed !== seed || !value.initial_state || typeof value.initial_state !== 'object') {
    fail('diagnostic reset identity does not match the requested LIST task and seed');
  }
  const state = value.initial_state;
  if (!state.application || state.application.name !== 'Interaction Fixture' || state.application.bundle_id !== bundleId
      || state.application.version !== '1.0.0' || !state.list || typeof state.list !== 'object') fail('diagnostic reset is not a list-profile reset for the stable MVP fixture');
  function noOracleKeys(node: unknown, depth = 0): void {
    if (depth > 20) fail('reset profile is too deeply nested');
    if (Array.isArray(node)) { if (node.length > 256) fail('reset array exceeds bound'); node.forEach(child => noOracleKeys(child, depth + 1)); return; }
    if (node && typeof node === 'object') {
      const entries = Object.entries(node as Record<string, unknown>);
      if (entries.length > 128) fail('reset object exceeds bound');
      for (const [key, child] of entries) {
        if (/^(?:evaluator(?:_truth)?|truth|oracle|expected_.*|navigation|steps|workflow|selectors?|coordinates?|driver_answer)$/i.test(key)) fail('diagnostic reset contains evaluator, expected-answer, or workflow material');
        noOracleKeys(child, depth + 1);
      }
    }
  }
  noOracleKeys(value);
  return value;
}
function hash(path: string): string {
  const digest = createHash('sha256');
  const data = readStable(path, 'pinned binary', 512 * 1024 * 1024);
  digest.update(data);
  return digest.digest('hex');
}
function write0600(path: string, bytes: Buffer | string): void {
  const fd = openSync(path, fsConstants.O_WRONLY | fsConstants.O_CREAT | fsConstants.O_EXCL | fsConstants.O_NOFOLLOW, 0o600);
  try {
    fchmodSync(fd, 0o600);
    writeFileSync(fd, bytes);
  } finally { closeSync(fd); }
}

const targetBytes = readStable(targetSpecPath, 'TargetSpec', 64 * 1024);
const targetText = targetBytes.toString('utf8').replace(/\r?\n$/, '');
if (targetText !== targetText.trim()) fail('TargetSpec JSON may have only one final newline, with no other leading or trailing whitespace.');
const target = validateTargetSpec(JSON.parse(targetText));
const targetHash = createHash('sha256').update(targetBytes).digest('hex');
const fixtureApp = join(root, 'target/native-macos/Interaction Fixture.app');
const fixtureBinary = join(fixtureApp, 'Contents/MacOS/MvpFixture');
const guardPath = realpathSync(guardInput);
const productionGuard = join(root, 'target/debug/cu-macos-device-guard');
if (hash(guardPath) !== hash(productionGuard)) fail('owned-row guard must be byte-identical to the pinned production device guard');
const consentDriver = join(root, 'target/native-macos/consent-dialog-driver');
const requiredFiles = [
  fixtureBinary, consentDriver, join(root, 'apps/desktop/out/main/index.js'), join(root, 'apps/desktop/out/renderer/index.html'),
  join(root, 'apps/desktop/out/preload/index.cjs'), join(root, 'target/debug/cu-host'), join(root, 'target/debug/cu-device'),
  join(root, 'target/debug/cu-source'), join(root, 'target/debug/cu-macos-source'), productionGuard,
  join(root, 'target/debug/compile_device_context'), join(root, 'scripts/mvp/scoped-activation-evidence.mjs'),
];
for (const file of requiredFiles) if (!statSync(file, { throwIfNoEntry: false })?.isFile()) fail(`Required prebuilt file is missing; this driver never builds: ${file}`);

const scratch = realpathSync(mkdtempSync(join(tmpdir(), 'cu-mvp-row-owned-')));
const scratchStat = statSync(scratch);
if ((scratchStat.mode & 0o777) !== 0o700) fail('owned scratch directory must be private');
const diagnosticDir = join(scratch, 'row-diagnostics');
mkdirSync(diagnosticDir, { mode: 0o700 });
const resetPath = join(scratch, 'reset-profile.json');
const snapshotPath = join(scratch, 'fixture-snapshot.json');
const targetPath = join(scratch, 'independent-target-spec.json');
write0600(targetPath, targetBytes);

let resetPins: Record<string, string> = {};
let resetProfile: unknown;
if (diagnosticMode) {
  resetProfile = validateResetProfile(JSON.parse(readStable(values['reset-profile']!, 'diagnostic reset', 128 * 1024, true).toString('utf8')));
  resetPins = { diagnostic_reset_sha256: createHash('sha256').update(readStable(values['reset-profile']!, 'diagnostic reset', 128 * 1024, true)).digest('hex') };
} else {
  const loaded = loadFixtureReset({ requestFile: values['request-file']!, freezeFile: values['freeze-file']!, taskId, seed });
  // The model goal from the frozen export is deliberately discarded. The only goal text comes from --target-spec.
  resetProfile = loaded.resetProfile;
  resetPins = loaded.pins;
}
write0600(resetPath, JSON.stringify(resetProfile));

const filesToPin = [
  'scripts/smoke-macos-mvp-row.mts', 'apps/desktop/test/manual/mvpRowSmoke.cjs',
  'scripts/mvp/scoped-activation-evidence.mjs', 'target/debug/compile_device_context',
  'native/mvp-fixture/MvpFixture.swift', 'apps/desktop/out/main/index.js', 'apps/desktop/out/renderer/index.html',
  'apps/desktop/out/preload/index.cjs', 'target/debug/cu-host', 'target/debug/cu-device', 'target/debug/cu-source',
  'target/debug/cu-macos-source', 'target/debug/cu-macos-device-guard', fixtureBinary, consentDriver, guardPath, runtimePath,
].map(file => isAbsolute(file) ? file : resolve(root, file));
const pins = Object.fromEntries(filesToPin.map(file => [file, hash(file)]));
const sourceHead = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
const outputFile = join(scratch, 'manual-result.json');
const childEnv = {
  ...process.env,
  CU_MVP_ROW_ROOT: root,
  CU_MVP_ROW_SCRATCH: scratch,
  CU_MVP_ROW_OUTPUT: outputFile,
  CU_MVP_ROW_PROFILE: resetPath,
  CU_MVP_ROW_SNAPSHOT: snapshotPath,
  CU_MVP_ROW_TARGET: targetPath,
  CU_MVP_ROW_TARGET_SHA256: targetHash,
  CU_MVP_ROW_TASK_ID: taskId,
  CU_MVP_ROW_SEED: String(seed),
  CU_MVP_ROW_EXECUTE: '1',
  CU_MVP_ROW_ACTIVATION_ONLY: values['activation-only'] ? '1' : '0',
  CU_MVP_ROW_GUARD: guardPath,
  CU_MVP_ROW_DIAGNOSTICS: diagnosticDir,
  CU_MVP_ROW_CONSENT_DRIVER: consentDriver,
  CU_MVP_ROW_FIXTURE_BINARY: fixtureBinary,
  CU_NATIVE_DEVICE_GUARD: guardPath,
  CU_OWNED_ROW_DIAGNOSTICS_DIR: diagnosticDir,
  CU_BIN_DIR: join(root, 'target/debug'),
  CU_SOURCE_BACKEND: 'native',
  CU_TARGET_BUNDLE_ID: bundleId,
  CU_NATIVE_REFERENCE_PLANNER: '1',
};
delete (childEnv as Record<string, string | undefined>).ELECTRON_RENDERER_URL;

let child: ChildProcess | undefined;
let stdout = '';
let stderr = '';
let exitCode: number | null = null;
let driverResult: Record<string, any> | null = null;
let failure: string | undefined;
let recordPath = values.record ? resolve(root, values.record) : join(root, 'artifacts', `m2-mvp-row-owned-${Date.now()}.json`);
try {
  child = spawn(runtimePath, [join(root, 'apps/desktop/test/manual/mvpRowSmoke.cjs')], {
    cwd: root, env: childEnv, stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout?.on('data', chunk => { stdout = (stdout + chunk.toString('utf8')).slice(-16 * 1024); });
  child.stderr?.on('data', chunk => { stderr = (stderr + chunk.toString('utf8')).slice(-16 * 1024); });
  const timeout = setTimeout(() => child?.kill('SIGTERM'), 110_000);
  exitCode = await new Promise<number | null>((resolveExit, reject) => {
    child!.once('error', reject);
    child!.once('close', resolveExit);
  }).finally(() => clearTimeout(timeout));
  if (statSync(outputFile, { throwIfNoEntry: false })?.isFile()) driverResult = JSON.parse(readStable(outputFile, 'manual result', 2 * 1024 * 1024).toString('utf8'));
  if (exitCode !== 0 || driverResult?.ok !== true) failure = driverResult?.error ?? `manual Electron driver exited ${exitCode}`;
} catch (error) { failure = error instanceof Error ? error.stack : String(error); }
finally {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill('SIGTERM');
    await new Promise(resolveWait => { const timer = setTimeout(resolveWait, 3_000); child!.once('close', () => { clearTimeout(timer); resolveWait(undefined); }); });
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  }
}

let binariesUnchanged = true;
for (const [file, expected] of Object.entries(pins)) {
  try { if (hash(file) !== expected) binariesUnchanged = false; } catch { binariesUnchanged = false; }
}
const record = {
  kind: values['activation-only'] ? 'scoped_window_activation_owned_operator_diagnostic' : 'mvp_row_owned_operator_diagnostic',
  protocol_version: 1,
  ok: !failure && binariesUnchanged && driverResult?.ok === true,
  ...(failure ? { error: failure } : {}),
  tested_at: new Date().toISOString(),
  source_head: sourceHead,
  reset: { mode: diagnosticMode ? 'diagnostic_reset' : 'frozen_request_reset', task_id: taskId, seed, pins: resetPins,
    business_gate_eligible: false },
  private_scratch_directory: scratch,
  private_native_diagnostics_directory: diagnosticDir,
  target_spec: target,
  target_spec_sha256: targetHash,
  exact_operator_text_sha256: createHash('sha256').update(`Select fixture row: ${targetText}`).digest('hex'),
  execute_flag_required_and_set: true,
  live_model_calls: 0,
  business_gate_eligible: false,
  activation_only: Boolean(values['activation-only']),
  mvp_complete: false,
  binaries_unchanged_during_execution: binariesUnchanged,
  pinned_files: pins,
  runtime_exit_code: exitCode,
  runtime_stdout_tail: stdout,
  runtime_stderr_tail: stderr,
  driver: driverResult,
  limitations: [
    'This is one owned fixture diagnostic, not the frozen business denominator or MVP completion.',
    'Navigation primitive count is unknown unless an exact native receipt exists; revisit count and quota are not setter receipts.',
    'Missing or uncorrelated row-submit receipt leaves AX setter count unknown.',
  ],
};
mkdirSync(dirname(recordPath), { recursive: true });
if (lstatSync(dirname(recordPath)).isSymbolicLink()) fail('record directory may not be a symlink');
if (statSync(recordPath, { throwIfNoEntry: false })) fail(`record path already exists: ${recordPath}`);
const recordFd = openSync(recordPath, fsConstants.O_WRONLY | fsConstants.O_CREAT | fsConstants.O_EXCL | fsConstants.O_NOFOLLOW, 0o600);
try { fchmodSync(recordFd, 0o600); writeFileSync(recordFd, JSON.stringify(record, null, 2) + '\n'); } finally { closeSync(recordFd); }
console.log(JSON.stringify({ ok: record.ok, record: recordPath, fixture_pid: driverResult?.fixture_pid, error: record.error }));
if (!record.ok) process.exitCode = 1;
