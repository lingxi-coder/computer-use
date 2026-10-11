import { spawn, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: { execute: { type: 'boolean' }, runtime: { type: 'string' }, output: { type: 'string' } } });
if (!values.execute || !isAbsolute(values.runtime || '') || !lstatSync(values.runtime).isFile()) {
  throw new Error('Supply --execute and an absolute existing Electron executable for the owned background diagnostic.');
}
const output = values.output || mkdtempSync(join(tmpdir(), 'cu-background-ax-'));
if (!isAbsolute(output) || (existsSync(output) && readdirSync(output).length)) throw new Error('Output must be an absolute new or empty directory.');
mkdirSync(output, { recursive: true, mode: 0o700 });
const ownedGuard = join(output, 'owned-background-guard');
copyFileSync(join(root, 'target/debug/cu-macos-device-guard'), ownedGuard);
chmodSync(ownedGuard, 0o700);
const sha = file => createHash('sha256').update(readFileSync(file)).digest('hex');
const files = [
  'target/debug/cu-host', 'target/debug/cu-device', 'target/debug/cu-source', 'target/debug/cu-macos-source',
  'target/debug/cu-macos-device-guard', 'target/debug/compile_device_context',
  'target/native-macos/Background AX Fixture.app/Contents/MacOS/BackgroundAXFixture',
  'target/native-macos/background-focus-probe', 'target/native-macos/consent-dialog-driver',
  'apps/desktop/out/main/index.js', 'apps/desktop/out/preload/index.cjs', 'apps/desktop/out/renderer/index.html',
  'apps/desktop/test/manual/backgroundAXSmoke.cjs', 'contracts/desktop-v2/schema.json',
];
const pins = Object.fromEntries(files.map(file => [file, sha(join(root, file))]));
pins[values.runtime] = sha(values.runtime);
pins[ownedGuard] = sha(ownedGuard);
const before = { created_at: new Date().toISOString(), head: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
  pins, input_scope: 'one owned native fixture window, Main consent, one AXPress',
  live_provider_calls: 0, business_gate_eligible: false, mvp_complete: false };
writeFileSync(join(output, 'before.json'), JSON.stringify(before, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
const log = [];
const child = spawn(values.runtime, [join(root, 'apps/desktop/test/manual/backgroundAXSmoke.cjs')], {
  cwd: root, env: { ...process.env, CU_BACKGROUND_ROOT: root, CU_BACKGROUND_OUTPUT: output,
    CU_BACKGROUND_EXECUTE: '1', CU_NATIVE_DEVICE_GUARD: ownedGuard }, stdio: ['ignore', 'pipe', 'pipe'],
});
let logBytes = 0, truncated = false;
for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => {
  if (logBytes + bytes.length <= 4 * 1024 * 1024) { log.push(bytes); logBytes += bytes.length; } else truncated = true;
});
const timer = setTimeout(() => child.kill('SIGTERM'), 105000);
const exitCode = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); }).finally(() => clearTimeout(timer));
writeFileSync(join(output, 'runtime.log'), Buffer.concat(log), { flag: 'wx', mode: 0o600 });
// Main can reject startup before the Electron diagnostic's finish handler runs.
// Recover only the child identified by this run's unique, private snapshot path.
let fixtureCleanup = { status: 'no_owner_record', confirmed: false };
const ownerPath = join(output, 'fixture-owner.json');
if (existsSync(ownerPath)) {
  const stat = lstatSync(ownerPath);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.uid !== process.getuid() || stat.nlink !== 1 || stat.size > 4096) {
    throw new Error('Invalid fixture ownership record');
  }
  const owner = JSON.parse(readFileSync(ownerPath, 'utf8'));
  const executable = join(root, 'target/native-macos/Background AX Fixture.app/Contents/MacOS/BackgroundAXFixture');
  if (!Number.isSafeInteger(owner.pid) || owner.pid <= 0 || owner.executable !== executable || owner.snapshot !== join(output, 'fixture-state.json')) {
    throw new Error('Fixture ownership record does not match this run');
  }
  const currentCommand = () => {
    try { return execFileSync('ps', ['-ww', '-p', String(owner.pid), '-o', 'args='], { encoding: 'utf8' }).trim(); }
    catch (error) { if (error.status === 1) return null; throw error; }
  };
  const command = currentCommand();
  if (command === null) fixtureCleanup = { status: 'already_exited', confirmed: true, pid: owner.pid };
  else if (command === `${executable} --snapshot ${owner.snapshot}`) {
    process.kill(owner.pid, 'SIGTERM');
    for (let i = 0; i < 30 && currentCommand() !== null; i++) await new Promise(resolve => setTimeout(resolve, 100));
    fixtureCleanup = { status: 'terminated_owned_child', confirmed: currentCommand() === null, pid: owner.pid };
  } else fixtureCleanup = { status: 'identity_changed_no_signal', confirmed: false, pid: owner.pid };
}
const unchanged = Object.entries(pins).every(([file, hash]) => sha(isAbsolute(file) ? file : join(root, file)) === hash);
const result = existsSync(join(output, 'result.json')) ? JSON.parse(readFileSync(join(output, 'result.json'), 'utf8')) : null;
const record = { tested_at: new Date().toISOString(), exit_code: exitCode, binaries_unchanged: unchanged,
  runtime_log_truncated: truncated, runtime_log_sha256: sha(join(output, 'runtime.log')),
  result_sha256: result ? sha(join(output, 'result.json')) : null, ok: Boolean(result?.ok && exitCode === 0 && unchanged && !truncated && fixtureCleanup.confirmed),
  owned_fixture_cleanup: fixtureCleanup,
  output, error: result ? result.error ?? null : 'Electron exited without a diagnostic result', business_gate_eligible: false, mvp_complete: false };
writeFileSync(join(output, 'run.json'), JSON.stringify(record, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
console.log(JSON.stringify(record));
if (!record.ok) process.exitCode = 1;
