// Exercise the public frozen-task picker through the normal Main/preload/UI.
// The child harness imports the built production Main entry and does not
// replace Host construction or select a browser/native fixture.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  createReadStream,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs, promisify } from 'node:util';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: { runtime: { type: 'string' }, output: { type: 'string' } } });
assert(isAbsolute(values.runtime ?? '') && isAbsolute(values.output ?? ''),
  'Supply absolute --runtime and new --output paths.');
const runtime = resolve(values.runtime!);
const output = resolve(values.output!);
assert(lstatSync(runtime).isFile() && !lstatSync(runtime).isSymbolicLink() && realpathSync(runtime) === runtime,
  'Electron runtime must be a canonical regular file.');
assert(!existsSync(output), 'Output must be new.');
const outputParent = dirname(output);
assert.equal(realpathSync(outputParent), outputParent, 'Output parent must be canonical.');
mkdirSync(output, { mode: 0o700 });
assert.equal(realpathSync(output), output);
const outputStat = lstatSync(output);
assert(outputStat.isDirectory() && !outputStat.isSymbolicLink() && outputStat.uid === process.getuid!()
  && (outputStat.mode & 0o7777) === 0o700, 'Output must be a private task-owned directory.');

const userData = join(output, 'user-data');
// Unix socket addresses must stay below the macOS sockaddr_un limit.
// Use an exclusive, short, task-owned directory rather than the artifact path.
const privateTmp = mkdtempSync('/private/tmp/cu-frozen-');
mkdirSync(userData, { mode: 0o700 });
const identity = (path: string) => {
  const stat = lstatSync(path);
  assert(stat.isDirectory() && !stat.isSymbolicLink() && stat.uid === process.getuid!()
    && (stat.mode & 0o7777) === 0o700 && realpathSync(path) === path);
  return { dev: stat.dev, ino: stat.ino, uid: stat.uid, mode: stat.mode & 0o7777 };
};
const userDataIdentity = identity(userData);
const tempIdentity = identity(privateTmp);
const saveNew = (name: string, value: unknown) => {
  const path = join(output, name);
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  const stat = lstatSync(path);
  assert(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1 && stat.uid === process.getuid!()
    && (stat.mode & 0o077) === 0, `${name} must be a private new evidence file.`);
};

const expectedVersion: string = JSON.parse(readFileSync(join(root, 'node_modules/electron/package.json'), 'utf8')).version;
const frozenEnvNames = [
  'CU_FROZEN_TASK_GOALS_PATH',
  'CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH',
  'CU_FROZEN_TASK_GOALS_SHA256',
  'CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256',
] as const;
for (const name of frozenEnvNames) {
  assert(typeof process.env[name] === 'string' && process.env[name]!.trim() !== '',
    `Launch with the four parent-provided frozen catalog environment values; ${name} is missing.`);
}

const sourcePaths = [
  'scripts/smoke-frozen-task-catalog.mts',
  'scripts/test/frozenTaskCatalogMainSmoke.cjs',
  'apps/desktop/src/main/index.ts',
  'apps/desktop/src/main/ipc.ts',
  'apps/desktop/src/main/frozenTaskCatalog.ts',
  'apps/desktop/src/main/hostBridge.ts',
  'apps/desktop/src/main/supervisor.ts',
  'apps/desktop/src/preload/index.ts',
  'apps/desktop/src/renderer/App.tsx',
  'apps/desktop/src/renderer/viewModel.ts',
  'apps/desktop/src/shared/channels.ts',
  'packages/desktop-api/src/api.ts',
];

async function hashFile(path: string): Promise<string> {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer);
  return hash.digest('hex');
}

function builtFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))
    .flatMap((entry) => {
      const path = join(directory, entry.name);
      assert(!entry.isSymbolicLink(), 'Built Main/preload/renderer files must be regular resources.');
      return entry.isDirectory() ? builtFiles(path) : entry.isFile() ? [path] : [];
    });
}

async function pins(): Promise<Record<string, string>> {
  const framework = realpathSync(join(dirname(runtime), '../Frameworks/Electron Framework.framework/Electron Framework'));
  const paths = [...new Set([
    ...sourcePaths.map((path) => join(root, path)),
    ...builtFiles(join(root, 'apps/desktop/out')),
    join(root, 'target/debug/cu-host'),
    join(root, 'target/debug/cu-device'),
    join(root, 'target/debug/cu-source'),
    runtime,
    framework,
    realpathSync(process.execPath),
  ])];
  const entries = await Promise.all(paths.map(async (path) => [path, await hashFile(path)] as const));
  return Object.fromEntries(entries);
}

const before = await pins();
saveNew('before.json', {
  created_at: new Date().toISOString(),
  expected_electron: expectedVersion,
  main_entry: 'apps/desktop/out/main/index.js',
  source_and_runtime_pins: before,
  frozen_catalog_inputs_present: true,
  frozen_source_files_opened_by_runner: false,
  private_tmp_path: privateTmp,
  private_tmp_identity: tempIdentity,
  business_acceptance: false,
});

const secretEnvironment = { ...process.env };
for (const name of Object.keys(secretEnvironment)) {
  if (/(?:API.?KEY|ACCESS.?TOKEN|(?:OPENAI|ANTHROPIC)_|MODEL.*(?:KEY|TOKEN))/i.test(name)) delete secretEnvironment[name];
}
for (const name of [
  'CU_SOURCE_BACKEND', 'CU_TARGET_PID', 'CU_TARGET_BUNDLE_ID', 'CU_NATIVE_SOURCE_HELPER',
  'CU_NATIVE_REFERENCE_PLANNER', 'CU_NATIVE_DEVICE_GUARD', 'CU_SOURCE_READ_WINDOW_ID',
  'CU_SOURCE_BACKGROUND_WINDOW_ID', 'CU_SOURCE_TABLE_SCOPE', 'CU_BROWSER_PAGE_PROFILE',
  'ELECTRON_RENDERER_URL',
]) delete secretEnvironment[name];
const child = spawn(runtime, [join(root, 'scripts/test/frozenTaskCatalogMainSmoke.cjs')], {
  cwd: root,
  stdio: ['ignore', 'pipe', 'pipe'],
  env: {
    ...secretEnvironment,
    CU_SMOKE_PROJECT_ROOT: root,
    CU_SMOKE_OUTPUT_DIR: output,
    CU_SMOKE_USER_DATA_DIR: userData,
    CU_SMOKE_TMP_DIR: privateTmp,
    CU_SMOKE_EXPECTED_ELECTRON: expectedVersion,
    CU_SMOKE_NODE_RUNTIME: realpathSync(process.execPath),
    CU_BIN_DIR: join(root, 'target/debug'),
    TMPDIR: privateTmp,
  },
});

let diagnostics = '';
for (const stream of [child.stdout, child.stderr]) stream?.on('data', (chunk) => {
  diagnostics = (diagnostics + String(chunk)).slice(-32_768);
});
let timedOut = false;
const timer = setTimeout(() => { timedOut = true; child.kill('SIGTERM'); }, 70_000);
const killTimer = setTimeout(() => child.kill('SIGKILL'), 80_000);
const run = promisify(execFile);
let failure: string | null = null;
let code: number | null = null;
let result: any = null;
const auditErrors: string[] = [];
const auditFailure = (stage: string, error: unknown) => {
  const message = `${stage}: ${error instanceof Error ? error.message : String(error)}`;
  auditErrors.push(message);
  failure ??= message;
};
try {
  code = await new Promise<number | null>((done, reject) => {
    child.once('error', reject);
    child.once('exit', done);
  });
  assert(!timedOut, 'Normal Electron diagnostic timed out.');
  assert.equal(code, 0, 'Normal Electron diagnostic exited unsuccessfully.');
  const resultPath = join(output, 'main-result.json');
  const resultStat = lstatSync(resultPath);
  assert(resultStat.isFile() && !resultStat.isSymbolicLink() && resultStat.nlink === 1
    && resultStat.size > 0 && resultStat.size <= 1024 * 1024
    && resultStat.uid === process.getuid!() && (resultStat.mode & 0o077) === 0);
  result = JSON.parse(readFileSync(resultPath, 'utf8'));
  assert.equal(result.ok, true, result.error ?? result.failure_step ?? 'Main diagnostic failed.');
  assert.equal(result.business_acceptance, false, 'Diagnostic evidence cannot count as business acceptance.');
  assert.equal(result.host_response?.outcome, 'host_refused_scope');
  assert.equal(result.stop_status, 'confirmed');
  assert.equal(result.cleanup?.normal_main_quit_requested, true);
} catch (error) {
  failure = error instanceof Error ? error.message : String(error);
} finally {
  clearTimeout(timer);
  clearTimeout(killTimer);
}

let after: Record<string, string> | null = null;
try { after = await pins(); } catch (error) { auditFailure('pins after', error); }
const changed = Object.keys(before).filter((path) => before[path] !== after?.[path]);
if (changed.length) failure ??= 'Pinned source, build output, Electron or Node binaries changed during the diagnostic.';

const ownedPids = new Set<number>([
  child.pid,
  ...(Array.isArray(result?.native_child_pids)
    ? result.native_child_pids.map((record: any) => record?.pid)
    : []),
].filter((pid): pid is number => typeof pid === 'number' && Number.isSafeInteger(pid) && pid > 0));
let remainingProcesses: number[] | null = null;
try {
  const { stdout } = await run('ps', ['-axo', 'pid=,command='], {
    encoding: 'utf8', timeout: 5_000, maxBuffer: 8 * 1024 * 1024,
  });
  remainingProcesses = stdout.split('\n').flatMap((line) => {
    const match = /^\s*(\d+)\s+(.*)$/.exec(line);
    if (!match) return [];
    const pid = Number(match[1]);
    if (!ownedPids.has(pid)) return [];
    const command = match[2]!;
    if (pid === child.pid) return [pid];
    const roleRecord = result?.native_child_pids?.find((record: any) => record.pid === pid);
    const rolePath = roleRecord ? join(root, 'target/debug', roleRecord.role) : '';
    return rolePath && command.includes(rolePath) ? [pid] : [];
  });
  assert.equal(remainingProcesses.length, 0, 'Main or one of its tracked native children is still running.');
} catch (error) { auditFailure('owned PID audit', error); }

let tempChildren: string[] = [];
try {
  const stat = lstatSync(privateTmp);
  assert(stat.isDirectory() && !stat.isSymbolicLink() && stat.dev === tempIdentity.dev
    && stat.ino === tempIdentity.ino && stat.uid === tempIdentity.uid
    && (stat.mode & 0o7777) === 0o700);
  tempChildren = readdirSync(privateTmp);
  assert(tempChildren.length <= 16, 'Temporary directory entry count exceeds its bound.');
  assert.equal(tempChildren.length, 0, 'Normal Main did not remove its supervisor temporary directory.');
} catch (error) { auditFailure('supervisor temporary directory audit', error); }

let userDataRemoved = false;
let privateTmpRemoved = false;
if (failure === null) {
  try {
    const stat = lstatSync(userData);
    assert(stat.isDirectory() && !stat.isSymbolicLink() && stat.dev === userDataIdentity.dev
      && stat.ino === userDataIdentity.ino && stat.uid === userDataIdentity.uid
      && (stat.mode & 0o7777) === 0o700 && realpathSync(userData) === userData);
    function verifyOwnedTree(path: string): void {
      for (const entry of readdirSync(path, { withFileTypes: true })) {
        const childPath = join(path, entry.name);
        const childStat = lstatSync(childPath);
        assert(!childStat.isSymbolicLink(), 'Isolated user-data tree contains an unexpected symbolic link.');
        assert.equal(childStat.uid, process.getuid!());
        if (entry.isDirectory()) verifyOwnedTree(childPath);
        else assert(childStat.isFile(), 'Isolated user-data tree contains an unexpected special file.');
      }
    }
    verifyOwnedTree(userData);
    rmSync(userData, { recursive: true, force: false });
    userDataRemoved = !existsSync(userData);
    assert(userDataRemoved);
    rmdirSync(privateTmp);
    privateTmpRemoved = !existsSync(privateTmp);
    assert(privateTmpRemoved);
  } catch (error) { auditFailure('owned private directory cleanup', error); }
}

if (auditErrors.length && failure === null) failure = auditErrors[0]!;
saveNew('runner.json', {
  ok: failure === null,
  failure,
  exit_code: code,
  timed_out: timedOut,
  main_pid: child.pid ?? null,
  main_entry: result?.main_entry ?? 'apps/desktop/out/main/index.js',
  pins_before: before,
  pins_after: after,
  changed,
  audit_errors: auditErrors,
  remaining_owned_processes: remainingProcesses,
  private_tmp_children_after_main_exit: tempChildren,
  isolated_user_data_removed: userDataRemoved,
  private_tmp_removed: privateTmpRemoved,
  main_result: result ? 'main-result.json' : null,
  catalog_public_file: existsSync(join(output, 'catalog.json')) ? 'catalog.json' : null,
  projection_files: ['projection-initial.json', 'projection-after-host.json', 'projection-after-stop.json']
    .filter((name) => existsSync(join(output, name))),
  diagnostics: diagnostics
    .replace(/(?:file:\/\/)?\/(?:Users|private|var|tmp|Volumes)\/[^\s"']+/g, '[path]')
    .replace(/[0-9a-f]{64}/gi, '[digest]')
    .slice(-32_768),
  provider_calls_requested: 0,
  browser_or_native_fixture_started: false,
  business_acceptance: false,
});
console.log(JSON.stringify({ ok: failure === null, failure, output }));
if (failure) process.exitCode = 1;
