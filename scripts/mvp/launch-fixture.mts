#!/usr/bin/env node
import { createHash, randomUUID } from 'node:crypto';
import {
  accessSync,
  closeSync,
  constants as fsConstants,
  fchmodSync,
  fstatSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readSync,
  readdirSync,
  realpathSync,
  writeSync,
} from 'node:fs';
import { spawn, type ChildProcess } from 'node:child_process';
import type { Stats } from 'node:fs';
import { dirname, isAbsolute, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { loadFixtureReset, type FixtureReset } from './fixture-reset.mjs';

const SCRIPT_DIRECTORY = dirname(fileURLToPath(import.meta.url));
const REPOSITORY_DIRECTORY = resolve(SCRIPT_DIRECTORY, '../..');
const FIXTURE_APP = resolve(REPOSITORY_DIRECTORY, 'target/native-macos/Interaction Fixture.app');
const FIXTURE_BINARY = resolve(FIXTURE_APP, 'Contents/MacOS/MvpFixture');
const FIXTURE_SOURCE = resolve(REPOSITORY_DIRECTORY, 'native/mvp-fixture/MvpFixture.swift');
const MAX_HASHED_INPUT_BYTES = 1024 * 1024;
const MAX_SNAPSHOT_BYTES = 128 * 1024;
const MAX_READY_STDOUT_BYTES = 1024;
const STARTUP_TIMEOUT_MS = 15_000;
const SNAPSHOT_TIMEOUT_MS = 15_000;
const SNAPSHOT_STABILITY_DELAY_MS = 120;
const TERM_WAIT_MS = 2_000;
const KILL_WAIT_MS = 4_000;
const HEX_SHA256 = /^[a-f0-9]{64}$/;
const SNAPSHOT_KEYS = ['version', 'task_id', 'seed', 'values', 'selected_row', 'modal', 'persistence', 'audit'];

type HashResult = { sha256: string; size: number };
type HashUnavailable = { unavailable: true; code: string };
type SnapshotRead = { contents: Buffer; sha256: string; size: number; stat: Stats };
type ReadyRecord = { pid: number; window_number: number };
type ChildCloseResult = { code: number | null; signal: NodeJS.Signals | null };
type StopReport = Partial<ChildCloseResult> & {
  exit_confirmed: boolean;
  termination_requested: boolean;
  forced_kill: boolean;
};
type ChildState = {
  closed: boolean;
  closeResult?: ChildCloseResult;
  closedPromise: Promise<ChildCloseResult>;
};
type StdoutState = { protocolError: boolean; stderrBytes: number };
type ProtectedHashes = {
  request_file?: HashResult;
  freeze_file?: HashResult;
  binary?: HashResult;
  source?: HashResult;
};

class LaunchFailure extends Error {
  code: string;

  constructor(code: string) {
    super(code);
    this.code = code;
  }
}

function fail(code: string): never { throw new LaunchFailure(code); }
const isPlainRecord = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value);
const delay = (ms: number): Promise<void> => new Promise(resolveDelay => setTimeout(resolveDelay, ms));
const isErrnoCode = (error: unknown, code: string): boolean => error instanceof Error && (error as NodeJS.ErrnoException).code === code;

function processUid(): number {
  if (typeof process.getuid !== 'function') fail('UNSUPPORTED_PLATFORM');
  return process.getuid();
}

function sameFileIdentity(left: Stats, right: Stats): boolean {
  return left.dev === right.dev && left.ino === right.ino;
}

function sameStableMetadata(left: Stats, right: Stats): boolean {
  return sameFileIdentity(left, right)
    && left.size === right.size
    && left.mtimeMs === right.mtimeMs
    && left.ctimeMs === right.ctimeMs;
}

function assertRegularFile(stat: Stats, label: string, requirePrivate = false, expectedUid = processUid()): void {
  if (!stat.isFile() || stat.nlink !== 1 || stat.uid !== expectedUid) fail(`${label}_NOT_REGULAR_FILE`);
  if (requirePrivate && (stat.mode & 0o777) !== 0o600) fail(`${label}_NOT_PRIVATE_REGULAR_FILE`);
}

function assertPathMatchesDescriptor(path: string, descriptorStat: Stats, label: string, requirePrivate = false): void {
  let pathStat: Stats;
  try { pathStat = lstatSync(path); } catch { fail(`${label}_CHANGED_DURING_READ`); }
  if (pathStat.isSymbolicLink() || !sameStableMetadata(pathStat, descriptorStat)) {
    fail(`${label}_CHANGED_DURING_READ`);
  }
  assertRegularFile(pathStat, label, requirePrivate);
}

function openStableRegular(path: string, label: string, maxBytes: number, requirePrivate = false): { descriptor: number; before: Stats } {
  let initialPathStat: Stats;
  try { initialPathStat = lstatSync(path); } catch { fail(`${label}_UNAVAILABLE`); }
  if (initialPathStat.isSymbolicLink() || !initialPathStat.isFile() || initialPathStat.nlink !== 1
      || initialPathStat.uid !== processUid() || initialPathStat.size <= 0 || initialPathStat.size > maxBytes) {
    fail(`${label}_INVALID_FILE`);
  }

  let descriptor: number;
  try { descriptor = openSync(path, fsConstants.O_RDONLY | fsConstants.O_NOFOLLOW); }
  catch { fail(`${label}_UNAVAILABLE`); }
  try {
    const before = fstatSync(descriptor);
    assertRegularFile(before, label, requirePrivate);
    if (!sameStableMetadata(initialPathStat, before) || before.size <= 0 || before.size > maxBytes) {
      fail(`${label}_CHANGED_DURING_READ`);
    }
    return { descriptor, before };
  } catch (error) {
    closeSync(descriptor);
    throw error;
  }
}

function hashRegularFile(path: string, label: string, maxBytes = MAX_HASHED_INPUT_BYTES): HashResult {
  const { descriptor, before } = openStableRegular(path, label, maxBytes);
  try {
    const digest = createHash('sha256');
    const buffer = Buffer.allocUnsafe(64 * 1024);
    let total = 0;
    while (total < before.size) {
      const requested = Math.min(buffer.length, before.size - total);
      let count: number;
      try { count = readSync(descriptor, buffer, 0, requested, null); }
      catch (error) {
        if (isErrnoCode(error, 'EINTR')) continue;
        fail(`${label}_READ_FAILED`);
      }
      if (count <= 0) fail(`${label}_CHANGED_DURING_READ`);
      digest.update(buffer.subarray(0, count));
      total += count;
    }
    const after = fstatSync(descriptor);
    if (!sameStableMetadata(before, after) || total !== before.size) fail(`${label}_CHANGED_DURING_READ`);
    assertPathMatchesDescriptor(path, after, label);
    return { sha256: digest.digest('hex'), size: total };
  } finally {
    closeSync(descriptor);
  }
}

function readBoundedRegularFile(path: string, label: string, maxBytes: number, requirePrivate = false): { contents: Buffer; sha256: string; size: number; stat: Stats } {
  const { descriptor, before } = openStableRegular(path, label, maxBytes, requirePrivate);
  try {
    const contents = Buffer.alloc(before.size);
    let offset = 0;
    while (offset < contents.length) {
      let count: number;
      try { count = readSync(descriptor, contents, offset, contents.length - offset, null); }
      catch (error) {
        if (isErrnoCode(error, 'EINTR')) continue;
        fail(`${label}_READ_FAILED`);
      }
      if (count <= 0) fail(`${label}_CHANGED_DURING_READ`);
      offset += count;
    }
    const after = fstatSync(descriptor);
    if (!sameStableMetadata(before, after) || offset !== before.size) fail(`${label}_CHANGED_DURING_READ`);
    assertPathMatchesDescriptor(path, after, label, requirePrivate);
    return { contents, sha256: createHash('sha256').update(contents).digest('hex'), size: offset, stat: after };
  } finally {
    closeSync(descriptor);
  }
}

function writeExclusive0600(path: string, contents: string | Buffer, label: string): void {
  let descriptor: number | undefined;
  try {
    descriptor = openSync(path, fsConstants.O_WRONLY | fsConstants.O_CREAT | fsConstants.O_EXCL | fsConstants.O_NOFOLLOW, 0o600);
    fchmodSync(descriptor, 0o600);
    const bytes = Buffer.isBuffer(contents) ? contents : Buffer.from(contents, 'utf8');
    let offset = 0;
    while (offset < bytes.length) {
      const count = writeSync(descriptor, bytes, offset, bytes.length - offset);
      if (count <= 0) fail(`${label}_WRITE_FAILED`);
      offset += count;
    }
    fsyncSync(descriptor);
    const stat = fstatSync(descriptor);
    if (!stat.isFile() || stat.nlink !== 1 || stat.uid !== processUid() || (stat.mode & 0o777) !== 0o600) {
      fail(`${label}_NOT_PRIVATE_REGULAR_FILE`);
    }
  } catch (error) {
    if (error instanceof LaunchFailure) throw error;
    fail(`${label}_WRITE_FAILED`);
  } finally {
    if (descriptor !== undefined) closeSync(descriptor);
  }
}

function createPrivateOutputDirectory(path: string): string {
  if (!isAbsolute(path)) fail('OUT_DIR_MUST_BE_ABSOLUTE');
  const outputDirectory = resolve(path);
  const parent = dirname(outputDirectory);
  let parentStat: Stats;
  try { parentStat = lstatSync(parent); } catch { fail('OUT_DIR_PARENT_UNAVAILABLE'); }
  if (!parentStat.isDirectory() || parentStat.isSymbolicLink()) fail('OUT_DIR_PARENT_INVALID');
  try {
    if (realpathSync(parent) !== parent) fail('OUT_DIR_PARENT_INVALID');
  } catch { fail('OUT_DIR_PARENT_INVALID'); }

  try { mkdirSync(outputDirectory, { mode: 0o700 }); }
  catch { fail('OUT_DIR_MUST_BE_NEW'); }
  let outputStat: Stats;
  try { outputStat = lstatSync(outputDirectory); } catch { fail('OUT_DIR_CREATE_FAILED'); }
  if (outputStat.isSymbolicLink() || !outputStat.isDirectory() || outputStat.uid !== processUid()) {
    fail('OUT_DIR_NOT_PRIVATE_EMPTY_DIRECTORY');
  }
  let directoryDescriptor: number;
  try { directoryDescriptor = openSync(outputDirectory, fsConstants.O_RDONLY | fsConstants.O_DIRECTORY | fsConstants.O_NOFOLLOW); }
  catch { fail('OUT_DIR_NOT_PRIVATE_EMPTY_DIRECTORY'); }
  try {
    const openedDirectory = fstatSync(directoryDescriptor);
    if (!sameFileIdentity(outputStat, openedDirectory) || openedDirectory.uid !== processUid() || !openedDirectory.isDirectory()) {
      fail('OUT_DIR_NOT_PRIVATE_EMPTY_DIRECTORY');
    }
    fchmodSync(directoryDescriptor, 0o700);
    outputStat = fstatSync(directoryDescriptor);
    if ((outputStat.mode & 0o777) !== 0o700 || readdirSync(outputDirectory).length !== 0) {
      fail('OUT_DIR_NOT_PRIVATE_EMPTY_DIRECTORY');
    }
  } finally {
    closeSync(directoryDescriptor);
  }
  return outputDirectory;
}

type CliOptions = {
  'request-file'?: string;
  'freeze-file'?: string;
  'task-id'?: string;
  seed?: string;
  'out-dir'?: string;
};

type CliArgs = { requestFile: string; freezeFile: string; taskId: string; seed: number; outDirectory: string };

function parseCli(): CliArgs {
  let values: CliOptions;
  try {
    ({ values } = parseArgs({
      options: {
        'request-file': { type: 'string' },
        'freeze-file': { type: 'string' },
        'task-id': { type: 'string' },
        seed: { type: 'string' },
        'out-dir': { type: 'string' },
      },
      strict: true,
      allowPositionals: false,
    }));
  } catch { fail('INVALID_ARGUMENTS'); }

  const requestFile = values['request-file'];
  const freezeFile = values['freeze-file'];
  const taskId = values['task-id'];
  const seed = values.seed;
  const outDirectory = values['out-dir'];
  if (!requestFile || !freezeFile || !taskId || !seed || !outDirectory) {
    fail('INVALID_ARGUMENTS');
  }
  const paths: { requestFile: string; freezeFile: string; outDirectory: string } = {
    requestFile,
    freezeFile,
    outDirectory,
  };
  if (Object.values(paths).some(path => !isAbsolute(path))) fail('INPUT_PATHS_MUST_BE_ABSOLUTE');
  if (!/^[A-Z]+-\d{2}$/.test(taskId)) fail('INVALID_TASK_ID');
  if (!/^[1-5]$/.test(seed)) fail('INVALID_SEED');
  return {
    requestFile: resolve(paths.requestFile),
    freezeFile: resolve(paths.freezeFile),
    taskId,
    seed: Number(seed),
    outDirectory: paths.outDirectory,
  };
}

function checkHelperResult(
  result: unknown,
  pinsFromFiles: { requestSha256: string; freezeSha256: string },
): asserts result is FixtureReset {
  if (!isPlainRecord(result) || !isPlainRecord(result.resetProfile) || !isPlainRecord(result.modelGoal)
      || !isPlainRecord(result.pins)
      || typeof result.pins.request_sha256 !== 'string' || !HEX_SHA256.test(result.pins.request_sha256)
      || typeof result.pins.freeze_sha256 !== 'string' || !HEX_SHA256.test(result.pins.freeze_sha256)
      || typeof result.pins.freeze_content_sha256 !== 'string' || !HEX_SHA256.test(result.pins.freeze_content_sha256)
      || result.pins.request_sha256 !== pinsFromFiles.requestSha256
      || result.pins.freeze_sha256 !== pinsFromFiles.freezeSha256) {
    fail('FIXTURE_RESET_REJECTED');
  }
  if (result.resetProfile.version !== 1 || typeof result.resetProfile.task_id !== 'string'
      || !Number.isSafeInteger(result.resetProfile.seed) || !isPlainRecord(result.resetProfile.initial_state)
      || typeof result.modelGoal.objective !== 'string' || !isPlainRecord(result.modelGoal.allowed_scope)) {
    fail('FIXTURE_RESET_REJECTED');
  }
}

function verifyFixturePaths(): { binary: HashResult; source: HashResult } {
  let appStat: Stats;
  try { appStat = lstatSync(FIXTURE_APP); } catch { fail('FIXTURE_APP_UNAVAILABLE'); }
  if (appStat.isSymbolicLink() || !appStat.isDirectory()) fail('FIXTURE_APP_INVALID');
  try {
    if (realpathSync(FIXTURE_APP) !== FIXTURE_APP) fail('FIXTURE_APP_INVALID');
    if (realpathSync(FIXTURE_BINARY) !== FIXTURE_BINARY || realpathSync(FIXTURE_SOURCE) !== FIXTURE_SOURCE) {
      fail('FIXTURE_BINARY_OR_SOURCE_PATH_INVALID');
    }
    accessSync(FIXTURE_BINARY, fsConstants.X_OK);
  } catch { fail('FIXTURE_BINARY_UNAVAILABLE'); }
  const binary = hashRegularFile(FIXTURE_BINARY, 'FIXTURE_BINARY_HASH', 1024 * 1024 * 1024);
  const source = hashRegularFile(FIXTURE_SOURCE, 'FIXTURE_SOURCE_HASH', 8 * 1024 * 1024);
  return { binary, source };
}

function writeJsonExclusive(path: string, value: unknown, label: string): void {
  writeExclusive0600(path, `${JSON.stringify(value)}\n`, label);
}

function validateSnapshotBytes(bytes: Buffer, expectedTaskId: string, expectedSeed: number): Record<string, unknown> {
  let value: unknown;
  try { value = JSON.parse(bytes.toString('utf8')); } catch { fail('SNAPSHOT_INVALID_JSON'); }
  if (!isPlainRecord(value) || Object.keys(value).sort().join('\0') !== [...SNAPSHOT_KEYS].sort().join('\0')
      || value.version !== 1 || value.task_id !== expectedTaskId || value.seed !== expectedSeed) {
    fail('SNAPSHOT_INVALID_BASELINE');
  }
  return value;
}

function readSnapshotOnce(path: string): SnapshotRead {
  const snapshot = readBoundedRegularFile(path, 'SNAPSHOT', MAX_SNAPSHOT_BYTES, true);
  return {
    contents: snapshot.contents,
    sha256: snapshot.sha256,
    size: snapshot.size,
    stat: snapshot.stat,
  };
}

async function waitForStableSnapshot(
  path: string,
  taskId: string,
  seed: number,
  childState: ChildState,
  stdoutState: StdoutState,
): Promise<SnapshotRead> {
  const deadline = Date.now() + SNAPSHOT_TIMEOUT_MS;
  let previous: SnapshotRead | undefined;
  let lastError: unknown;
  while (Date.now() < deadline) {
    if (stdoutState.protocolError) fail('FIXTURE_STDOUT_PROTOCOL_INVALID');
    if (childState.closed) fail('FIXTURE_EXITED_BEFORE_SNAPSHOT');
    try {
      const current = readSnapshotOnce(path);
      if (previous && sameStableMetadata(previous.stat, current.stat)
          && previous.sha256 === current.sha256 && previous.size === current.size) {
        validateSnapshotBytes(current.contents, taskId, seed);
        return current;
      }
      previous = current;
      lastError = undefined;
    } catch (error) {
      if (error instanceof LaunchFailure && !['SNAPSHOT_UNAVAILABLE', 'SNAPSHOT_INVALID_FILE', 'SNAPSHOT_CHANGED_DURING_READ'].includes(error.code)) throw error;
      previous = undefined;
      lastError = error;
    }
    await delay(SNAPSHOT_STABILITY_DELAY_MS);
  }
  if (lastError instanceof LaunchFailure && lastError.code.startsWith('SNAPSHOT_')) throw lastError;
  fail('SNAPSHOT_TIMEOUT');
}

function waitForClose(child: ChildProcess): Promise<ChildCloseResult> {
  return new Promise(resolveClose => {
    let settled = false;
    const finish = (code: number | null, signal: NodeJS.Signals | null): void => {
      if (settled) return;
      settled = true;
      resolveClose({ code, signal });
    };
    child.once('close', finish);
    child.once('error', () => { /* the following close event confirms process termination */ });
  });
}

function parseReadyLine(line: string, childPid: number | undefined): ReadyRecord {
  let value: unknown;
  try { value = JSON.parse(line); } catch { fail('FIXTURE_STDOUT_PROTOCOL_INVALID'); }
  if (!isPlainRecord(value) || Object.keys(value).sort().join('\0') !== ['pid', 'window_number'].join('\0')
      || typeof value.pid !== 'number' || !Number.isSafeInteger(value.pid) || value.pid !== childPid
      || typeof value.window_number !== 'number' || !Number.isSafeInteger(value.window_number) || value.window_number <= 0) {
    fail('FIXTURE_STDOUT_PROTOCOL_INVALID');
  }
  return { pid: value.pid, window_number: value.window_number };
}

type StartupOutput = {
  stdoutState: StdoutState;
  readyPromise: Promise<ReadyRecord>;
  observeLaterOutput: () => void;
};

function collectStartupOutput(child: ChildProcess, childState: ChildState): StartupOutput {
  const stdout = child.stdout;
  const stderr = child.stderr;
  if (!stdout || !stderr) fail('FIXTURE_STDIO_UNAVAILABLE');
  let bytes = Buffer.alloc(0);
  let readyResolve: (ready: ReadyRecord) => void = () => undefined;
  let readyReject: (reason?: unknown) => void = () => undefined;
  let settled = false;
  const readyPromise = new Promise<ReadyRecord>((resolveReady, rejectReady) => {
    readyResolve = resolveReady;
    readyReject = rejectReady;
  });
  const stdoutState: StdoutState = { protocolError: false, stderrBytes: 0 };

  const rejectStartup = (error: unknown): void => {
    stdoutState.protocolError = true;
    if (!settled) {
      settled = true;
      readyReject(error);
    }
    if (!childState.closed) {
      try { child.kill('SIGTERM'); } catch { /* cleanup also runs in the caller */ }
    }
  };

  stdout.on('data', (chunk: Buffer) => {
    if (stdoutState.protocolError) return;
    if (bytes.length + chunk.length > MAX_READY_STDOUT_BYTES) {
      rejectStartup(new LaunchFailure('FIXTURE_STDOUT_PROTOCOL_INVALID'));
      return;
    }
    bytes = Buffer.concat([bytes, chunk]);
    const firstNewline = bytes.indexOf(0x0a);
    if (firstNewline < 0) return;
    if (firstNewline === 0 || bytes.indexOf(0x0a, firstNewline + 1) !== -1 || firstNewline !== bytes.length - 1) {
      rejectStartup(new LaunchFailure('FIXTURE_STDOUT_PROTOCOL_INVALID'));
      return;
    }
    try {
      const ready = parseReadyLine(bytes.subarray(0, firstNewline).toString('utf8'), child.pid);
      if (!settled) {
        settled = true;
        readyResolve(ready);
      }
    } catch (error) {
      rejectStartup(error);
    }
  });
  stderr.on('data', (chunk: Buffer) => { stdoutState.stderrBytes += chunk.length; });

  return {
    stdoutState,
    readyPromise,
    observeLaterOutput() {
      if (bytes.length > 0 && (bytes[bytes.length - 1] !== 0x0a || bytes.filter(byte => byte === 0x0a).length !== 1)) {
        stdoutState.protocolError = true;
        try { if (!childState.closed) child.kill('SIGTERM'); } catch { /* cleanup also runs in the caller */ }
      }
    },
  };
}

async function stopOwnedProcess(child: ChildProcess, childState: ChildState): Promise<StopReport> {
  let terminationRequested = false;
  let forcedKill = false;
  if (!childState.closed) {
    try { terminationRequested = child.kill('SIGTERM'); } catch { /* escalate below */ }
    const exitedAfterTerm = await Promise.race([
      childState.closedPromise.then(() => true),
      delay(TERM_WAIT_MS).then(() => false),
    ]);
    if (!exitedAfterTerm && !childState.closed) {
      try { forcedKill = child.kill('SIGKILL'); } catch { /* confirm below */ }
    }
  }
  if (!childState.closed) {
    await Promise.race([
      childState.closedPromise,
      delay(KILL_WAIT_MS),
    ]);
  }
  return {
    ...(childState.closeResult ?? {}),
    exit_confirmed: Boolean(childState.closed && childState.closeResult),
    termination_requested: terminationRequested,
    forced_kill: forcedKill,
  };
}

function collectAfterHashes(
  paths: Array<['request_file' | 'freeze_file' | 'binary' | 'source', string, string]>,
): Record<'request_file' | 'freeze_file' | 'binary' | 'source', HashResult | HashUnavailable> {
  const result = {} as Record<'request_file' | 'freeze_file' | 'binary' | 'source', HashResult | HashUnavailable>;
  for (const [key, path, label] of paths) {
    try { result[key] = hashRegularFile(path, label, key === 'binary' ? 1024 * 1024 * 1024 : key === 'source' ? 8 * 1024 * 1024 : MAX_HASHED_INPUT_BYTES); }
    catch (error: unknown) { result[key] = { unavailable: true, code: error instanceof LaunchFailure ? error.code : `${label}_UNAVAILABLE` }; }
  }
  return result;
}

function createArtifact(outputDirectory: string, record: Record<string, unknown>): string {
  const artifactId = randomUUID();
  const artifactPath = resolve(outputDirectory, `${artifactId}.json`);
  writeJsonExclusive(artifactPath, { ...record, artifact_id: artifactId }, 'ARTIFACT');
  return artifactPath;
}

async function run(): Promise<void> {
  let cli: CliArgs;
  let outputDirectory: string;
  try {
    cli = parseCli();
    outputDirectory = createPrivateOutputDirectory(cli.outDirectory);
  } catch {
    process.stderr.write('Fixture launch rejected before an owned output directory was created.\n');
    process.exitCode = 1;
    return;
  }

  const artifactBase: Record<string, unknown> = {
    schema_version: 1,
    baseline_only: true,
    business_runs: 0,
    provider_calls: 0,
    physical_inputs: 0,
    mvp_complete: false,
    environment_assets_verified: false,
    formal_business_freeze_accepted: false,
    task_id: cli.taskId,
    seed: cli.seed,
    status: 'failed',
    failure_code: 'LAUNCH_ABORTED',
    pins: null,
    input_hashes: null,
    fixture_hashes: null,
    ready: null,
    snapshot: null,
    owned_process: { launched: false, exited: true, exit_code: null, signal: null },
  };

  let child: ChildProcess | undefined;
  let childState: ChildState | undefined;
  let output: StartupOutput | undefined;
  let failureCode: string | undefined;
  let ready: ReadyRecord | undefined;
  let snapshot: SnapshotRead | undefined;
  let helperResult: FixtureReset | undefined;
  let beforeHashes: ProtectedHashes = {};
  let afterHashes: Record<'request_file' | 'freeze_file' | 'binary' | 'source', HashResult | HashUnavailable> | undefined;
  let closeResult: StopReport | undefined;

  try {
    const requestBefore = hashRegularFile(cli.requestFile, 'REQUEST_FILE_HASH');
    beforeHashes.request_file = requestBefore;
    const freezeBefore = hashRegularFile(cli.freezeFile, 'FREEZE_FILE_HASH');
    beforeHashes.freeze_file = freezeBefore;
    const fixtureBefore = verifyFixturePaths();
    beforeHashes.binary = fixtureBefore.binary;
    beforeHashes.source = fixtureBefore.source;

    helperResult = loadFixtureReset({
      requestFile: cli.requestFile,
      freezeFile: cli.freezeFile,
      taskId: cli.taskId,
      seed: cli.seed,
    });
    checkHelperResult(helperResult, {
      requestSha256: requestBefore.sha256,
      freezeSha256: freezeBefore.sha256,
    });
    if (helperResult.resetProfile.task_id !== cli.taskId || helperResult.resetProfile.seed !== cli.seed) {
      fail('FIXTURE_RESET_IDENTITY_MISMATCH');
    }

    const resetPath = resolve(outputDirectory, 'reset.json');
    const goalPath = resolve(outputDirectory, 'model-goal.json');
    const snapshotPath = resolve(outputDirectory, 'snapshot.json');
    writeJsonExclusive(resetPath, helperResult.resetProfile, 'RESET_PROFILE');
    writeJsonExclusive(goalPath, helperResult.modelGoal, 'MODEL_GOAL');

    const justBeforeSpawn = verifyFixturePaths();
    const inputBeforeSpawn = {
      request_file: hashRegularFile(cli.requestFile, 'REQUEST_FILE_HASH'),
      freeze_file: hashRegularFile(cli.freezeFile, 'FREEZE_FILE_HASH'),
    };
    if (justBeforeSpawn.binary.sha256 !== fixtureBefore.binary.sha256
        || justBeforeSpawn.source.sha256 !== fixtureBefore.source.sha256
        || inputBeforeSpawn.request_file.sha256 !== requestBefore.sha256
        || inputBeforeSpawn.freeze_file.sha256 !== freezeBefore.sha256) {
      fail('PROTECTED_INPUT_CHANGED_BEFORE_SPAWN');
    }

    child = spawn(FIXTURE_BINARY, ['--profile', resetPath, '--snapshot', snapshotPath], {
      cwd: REPOSITORY_DIRECTORY,
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    });
    childState = {
      closed: false,
      closedPromise: Promise.resolve({ code: null, signal: null }),
    };
    const spawnedState = childState;
    childState.closedPromise = waitForClose(child).then(result => {
      spawnedState.closed = true;
      spawnedState.closeResult = result;
      return result;
    });
    output = collectStartupOutput(child, childState);

    let startupTimer: NodeJS.Timeout | undefined;
    const readyTimeout = new Promise<never>((_, reject) => {
      startupTimer = setTimeout(() => reject(new LaunchFailure('FIXTURE_STARTUP_TIMEOUT')), STARTUP_TIMEOUT_MS);
      startupTimer.unref?.();
    });
    const exitedBeforeReady = childState.closedPromise.then(() => { throw new LaunchFailure('FIXTURE_EXITED_BEFORE_READY'); });
    try { ready = await Promise.race([output.readyPromise, readyTimeout, exitedBeforeReady]); }
    finally { if (startupTimer) clearTimeout(startupTimer); }
    if (output.stdoutState.protocolError) fail('FIXTURE_STDOUT_PROTOCOL_INVALID');

    snapshot = await waitForStableSnapshot(snapshotPath, cli.taskId, cli.seed, childState, output.stdoutState);
    output.observeLaterOutput();
    if (output.stdoutState.protocolError) fail('FIXTURE_STDOUT_PROTOCOL_INVALID');
  } catch (error) {
    failureCode = error instanceof LaunchFailure ? error.code : 'FIXTURE_LAUNCH_FAILED';
  } finally {
    if (child && childState) {
      closeResult = await stopOwnedProcess(child, childState);
    }
    if (snapshot && childState?.closed && closeResult) {
      try {
        const stoppedSnapshot = readSnapshotOnce(resolve(outputDirectory, 'snapshot.json'));
        validateSnapshotBytes(stoppedSnapshot.contents, cli.taskId, cli.seed);
        if (!sameStableMetadata(stoppedSnapshot.stat, snapshot.stat)
            || stoppedSnapshot.sha256 !== snapshot.sha256 || stoppedSnapshot.size !== snapshot.size) {
          failureCode ??= 'SNAPSHOT_CHANGED_AFTER_EXIT';
        }
        snapshot = stoppedSnapshot;
      } catch {
        failureCode ??= 'SNAPSHOT_UNAVAILABLE_AFTER_EXIT';
        snapshot = undefined;
      }
    }
    afterHashes = collectAfterHashes([
      ['request_file', cli.requestFile, 'REQUEST_FILE_HASH'],
      ['freeze_file', cli.freezeFile, 'FREEZE_FILE_HASH'],
      ['binary', FIXTURE_BINARY, 'FIXTURE_BINARY_HASH'],
      ['source', FIXTURE_SOURCE, 'FIXTURE_SOURCE_HASH'],
    ]);
  }

  if (!failureCode && (!closeResult?.exit_confirmed
      || !(closeResult.code === 0
        || ((closeResult.termination_requested || closeResult.forced_kill)
          && ['SIGTERM', 'SIGKILL'].includes(closeResult.signal ?? ''))))) {
    failureCode = 'FIXTURE_EXIT_NOT_CLEAN';
  }
  if (!failureCode && output?.stdoutState.protocolError) failureCode = 'FIXTURE_STDOUT_PROTOCOL_INVALID';
  if (!failureCode && afterHashes) {
    for (const key of ['request_file', 'freeze_file', 'binary', 'source'] as const) {
      const before = beforeHashes[key];
      const after = afterHashes[key];
      if (!before || !after || 'unavailable' in after || after.sha256 !== before.sha256) {
        failureCode = 'PROTECTED_INPUT_CHANGED';
        break;
      }
    }
  }

  if (helperResult) artifactBase.pins = helperResult.pins;
  artifactBase.input_hashes = {
    request_file: { before: beforeHashes.request_file ?? null, after: afterHashes?.request_file ?? null },
    freeze_file: { before: beforeHashes.freeze_file ?? null, after: afterHashes?.freeze_file ?? null },
  };
  artifactBase.fixture_hashes = {
    binary: { before: beforeHashes.binary ?? null, after: afterHashes?.binary ?? null },
    source: { before: beforeHashes.source ?? null, after: afterHashes?.source ?? null },
  };
  if (ready) artifactBase.ready = ready;
  if (snapshot) {
    const parsed = validateSnapshotBytes(snapshot.contents, cli.taskId, cli.seed);
    artifactBase.snapshot = {
      path: resolve(outputDirectory, 'snapshot.json'),
      sha256: snapshot.sha256,
      bytes: snapshot.size,
      version: parsed.version,
      task_id: parsed.task_id,
      seed: parsed.seed,
    };
  }
  artifactBase.owned_process = {
    launched: Boolean(child),
    exited: !child || Boolean(closeResult?.exit_confirmed),
    exit_code: closeResult?.code ?? null,
    signal: closeResult?.signal ?? null,
    termination_requested: closeResult?.termination_requested ?? false,
    forced_kill: closeResult?.forced_kill ?? false,
  };
  artifactBase.status = failureCode ? 'failed' : 'baseline_captured';
  artifactBase.failure_code = failureCode ?? null;

  let artifactPath: string;
  try { artifactPath = createArtifact(outputDirectory, artifactBase); }
  catch {
    process.stderr.write('Fixture launcher failed to preserve its result artifact.\n');
    process.exitCode = 1;
    return;
  }

  process.stdout.write(`${artifactPath}\n`);
  if (failureCode) {
    process.stderr.write(`Fixture baseline capture failed (${failureCode}); owned artifact: ${artifactPath}\n`);
    process.exitCode = 1;
  }
}

run().catch(() => {
  process.stderr.write('Fixture launcher failed unexpectedly.\n');
  process.exitCode = 1;
});
