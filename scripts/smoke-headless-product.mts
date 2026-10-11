// Explicit real Electron smoke through the ordinary Main/preload/renderer.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs, promisify } from 'node:util';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: { runtime: { type: 'string' }, output: { type: 'string' } } });
assert(isAbsolute(values.runtime ?? '') && isAbsolute(values.output ?? ''), 'Supply absolute --runtime and new --output paths.');
const runtime = resolve(values.runtime!);
const output = resolve(values.output!);
assert(lstatSync(runtime).isFile() && !lstatSync(runtime).isSymbolicLink() && realpathSync(runtime) === runtime);
assert(!existsSync(output), 'Output must be new.');
mkdirSync(dirname(output), { recursive: true, mode: 0o700 });
assert.equal(realpathSync(dirname(output)), dirname(output));
mkdirSync(output, { mode: 0o700 });
const save = (name: string, value: unknown) => writeFileSync(join(output, name), JSON.stringify(value, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
const run = promisify(execFile);
const expectedVersion: string = JSON.parse(readFileSync(join(root, 'node_modules/electron/package.json'), 'utf8')).version;
const sourcePaths = [
  'scripts/smoke-headless-product.mts', 'apps/desktop/test/manual/headlessProductSmoke.cjs',
  'scripts/mvp/host-ledger-evidence.mjs',
  'apps/desktop/test/manual/headlessProductSmokeProof.cjs', 'apps/desktop/test/headlessProductSmokeProof.test.ts',
  'apps/desktop/src/main/index.ts', 'apps/desktop/src/main/browserDiagnostics.ts',
  'apps/desktop/src/main/browserProfile.ts', 'apps/desktop/src/preload/index.ts',
  'apps/desktop/src/renderer/App.tsx', 'apps/desktop/src/renderer/viewModel.ts', 'apps/desktop/test/viewModel.test.ts',
  'apps/desktop/out/main/index.js', 'apps/desktop/out/main/imageWorker.js',
  'apps/desktop/out/main/localDecisionWorker.js', 'apps/desktop/out/preload/index.cjs',
  'apps/desktop/out/renderer/index.html', 'fixtures/headless/readonly-page.html',
  'target/debug/cu-host', 'target/debug/cu-device', 'target/debug/cu-source',
  'target/native-macos/headless-display-probe',
];
async function pins() {
  function files(directory: string): string[] {
    return readdirSync(directory, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))
      .flatMap(entry => {
        const path = join(directory, entry.name);
        assert(!entry.isSymbolicLink(), 'Built app files must be regular resources.');
        return entry.isDirectory() ? files(path) : entry.isFile() ? [path] : [];
      });
  }
  const framework = realpathSync(join(dirname(runtime), '../Frameworks/Electron Framework.framework/Electron Framework'));
  const chrome = ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    join(process.env.HOME!, 'Applications/Google Chrome.app/Contents/MacOS/Google Chrome')].find(existsSync);
  assert(chrome, 'The normal product fixed profile requires installed Google Chrome.');
  const paths = [...new Set([...sourcePaths.map(path => join(root, path)),
    ...files(join(root, 'apps/desktop/out')), runtime, framework, realpathSync(chrome), realpathSync(process.execPath)])];
  const entries = await Promise.all(paths.map(async path => {
    const hash = createHash('sha256');
    for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer);
    return [path, hash.digest('hex')] as const;
  }));
  return Object.fromEntries(entries);
}
const before = await pins();
save('before.json', { created_at: new Date().toISOString(), expectedVersion, pins: before,
  scope: 'Real normal Electron product entry; deterministic fixed fixture planner; no model acceptance.' });
const child = spawn(runtime, [join(root, 'apps/desktop/test/manual/headlessProductSmoke.cjs')], {
  cwd: root, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env,
    CU_SMOKE_PROJECT_ROOT: root, CU_SMOKE_OUTPUT_DIR: output, CU_SMOKE_EXPECTED_ELECTRON: expectedVersion,
    CU_SMOKE_NODE_RUNTIME: realpathSync(process.execPath),
    CU_BIN_DIR: join(root, 'target/debug'), CU_SOURCE_BACKEND: 'fixture', ELECTRON_RENDERER_URL: '',
  },
});
let diagnostics = '';
for (const stream of [child.stdout, child.stderr]) stream?.on('data', chunk => {
  diagnostics = (diagnostics + String(chunk)).slice(-32_768);
});
let timedOut = false;
const timer = setTimeout(() => { timedOut = true; child.kill('SIGTERM'); }, 90_000);
const killTimer = setTimeout(() => child.kill('SIGKILL'), 100_000);
let failure: string | null = null;
let code: number | null = null;
try {
  code = await new Promise<number | null>((done, reject) => {
    child.once('error', reject); child.once('exit', done);
  });
  assert(!timedOut, 'Electron product smoke timed out.');
  assert.equal(code, 0, 'Electron process failed.');
  const result = JSON.parse(readFileSync(join(output, 'result.json'), 'utf8'));
  assert.equal(result.ok, true, 'Product harness failed.');
} catch (error) { failure = error instanceof Error ? error.message : String(error); }
finally { clearTimeout(timer); clearTimeout(killTimer); }
const auditErrors: string[] = [];
function auditFailure(stage: string, error: unknown): void {
  const message = `${stage}: ${error instanceof Error ? error.message : String(error)}`;
  auditErrors.push(message);
  failure ??= message;
}
let after: Record<string, string> | null = null;
try { after = await pins(); } catch (error) { auditFailure('pins after', error); }
const changed = Object.keys(before).filter(path => before[path] !== after?.[path]);
if (changed.length) failure ??= 'Pinned sources or binaries changed during product smoke.';
// Audit only exact PIDs and owned profile paths; discard unrelated ps records in memory.
const resultPath = join(output, 'result.json');
let result: any = null;
try { if (existsSync(resultPath)) result = JSON.parse(readFileSync(resultPath, 'utf8')); }
catch (error) { auditFailure('result record', error); }
const ownerPath = join(output, 'ownership.json');
let records: any[] = [];
try {
  const owners = JSON.parse(readFileSync(ownerPath, 'utf8'));
  assert(Array.isArray(owners) && owners.length <= 256 && owners.every(record => record
    && typeof record === 'object' && typeof record.supervisorDir === 'string'
    && typeof record.profileParent === 'string' && record.pids && typeof record.pids === 'object'));
  records = owners;
} catch (error) { auditFailure('ownership record', error); }
const hostFactAudits: unknown[] = [];
try {
  const references = result?.host_ledger_evidence;
  assert(Array.isArray(references) && references.length === 2, 'Two completed Roots require two independent fact exports.');
  const seen = new Set<string>();
  for (const reference of references) {
    assert(reference && typeof reference.root_id === 'string' && !seen.has(reference.root_id));
    seen.add(reference.root_id);
    assert(/^host-ledger-evidence-[01]\.json$/.test(reference.path));
    const path = join(output, reference.path);
    const stat = lstatSync(path);
    assert(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1 && stat.size === reference.bytes
      && stat.size <= 8 * 1024 * 1024 && (stat.mode & 0o077) === 0 && stat.uid === process.getuid!());
    const bytes = readFileSync(path);
    assert.equal(createHash('sha256').update(bytes).digest('hex'), reference.sha256);
    const facts = JSON.parse(bytes.toString('utf8'));
    const legacyRoot = result.ledger.roots.find((entry: any) => entry.root_id === reference.root_id);
    assert(legacyRoot && facts.kind === 'host_ledger_evidence' && facts.scope.root_id === reference.root_id
      && facts.scope.other_root_rows_exported === false && facts.root.root_id === reference.root_id);
    assert.equal(facts.root.state, legacyRoot.state);
    assert(facts.database.opened_read_only && facts.database.query_only);
    assert.equal(facts.assessment.mvp_complete, false);
    assert.equal(facts.call_rows.length, 0);
    assert.equal(facts.computer_call_rows.length, 0);
    assert.equal(facts.attempts.length, result.ledger.attempts.filter((entry: any) => entry.root_id === reference.root_id).length);
    assert(facts.cross_references.attempts.every((entry: any) => entry.decision_reference_status === 'matched'
      && entry.admission_outbox_status === 'matched'));
    assert(facts.cross_references.decisions.every((entry: any) => entry.consumed_attempt === null
      || (entry.consumed_attempt_status === 'matched' && entry.attempt_map_status === 'matched')));
    hostFactAudits.push({ root_id: reference.root_id, sha256: reference.sha256,
      decisions: facts.decisions.length, attempts: facts.attempts.length, raw_decision_sources: facts.decisions.map((entry: any) => entry.source),
      model_call_rows: facts.call_rows.length, computer_call_rows: facts.computer_call_rows.length,
      raw_cross_references_checked: true, business_acceptance: false });
  }
} catch (error) { auditFailure('root-scoped Host fact export', error); }
const ownedPids = new Set<number>([child.pid!, result?.chrome?.pid, result?.browser_owner?.chrome_pid,
  ...records.flatMap(record => Object.values(record.pids ?? {}))]
  .filter((pid): pid is number => typeof pid === 'number' && Number.isSafeInteger(pid) && pid > 0));
const profiles = records.map(record => record.profileParent).filter((value): value is string => typeof value === 'string');
let remaining: number[] | null = null;
try {
  const { stdout } = await run('ps', ['-axo', 'pid=,args='], { encoding: 'utf8', timeout: 4_000, maxBuffer: 8 * 1024 * 1024 });
  remaining = stdout.split('\n').flatMap(line => {
  const match = /^\s*(\d+)\s+(.*)$/.exec(line);
  if (!match) return [];
  const pid = Number(match[1]);
  if (ownedPids.has(pid) || profiles.some(path => match[2]!.includes(`--user-data-dir=${path}/cu-headless-owned-`))) return [pid];
  return [];
  });
} catch (error) { auditFailure('owned process audit', error); }
const directoriesRemaining = [...new Set(records.flatMap(record => [record.supervisorDir, record.profileParent]))]
  .filter((path): path is string => typeof path === 'string').filter(existsSync);
if (remaining?.length || directoriesRemaining.length) failure ??= 'Owned process or directory cleanup is incomplete.';
if (!records.length) failure ??= 'Missing Main ownership evidence.';
save('runner.json', { ok: failure === null, failure, exit_code: code, timed_out: timedOut,
  electron_pid: child.pid ?? null,
  pins_before: before, pins_after: after, changed, audit_errors: auditErrors, remaining_pids: remaining,
  directories_remaining: directoriesRemaining, harness_ok: result?.ok ?? false,
  host_fact_audits: hostFactAudits,
  provider_calls: 0, os_desktop_input_calls: 0, actual_browser_input: result?.browser_execution_count ?? null,
  diagnostics });
console.log(JSON.stringify({ ok: failure === null, failure, output }));
if (failure) process.exitCode = 1;
