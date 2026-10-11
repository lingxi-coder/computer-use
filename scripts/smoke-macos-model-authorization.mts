import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

// Only an owned dummy key, a non-resolving endpoint and this product's exact
// Keychain account are used. No model task or provider request is created.
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: { runtime: { type: 'string' } } });
if (!values.runtime || !isAbsolute(values.runtime) || !existsSync(values.runtime)) throw new Error('--runtime must be an existing Electron executable');
const output = mkdtempSync(join(tmpdir(), 'cu-model-auth-'));
const runId = `${new Date().toISOString().replace(/[:.]/g, '-')}-${randomUUID()}`;
const model = 'owned-consent-' + randomUUID();
const route = { provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://example.invalid/v1', model, max_output_tokens: 4096 };
const account = createHash('sha256').update(JSON.stringify({ provider_id: route.provider_id, protocol: route.protocol, base_url: route.base_url, model })).digest('hex');
execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'), join(root, 'native/device/fixtures/ConsentDialogDriver.swift'), '-o', join(root, 'target/native-macos/consent-dialog-driver')], { stdio: 'inherit' });
const hash = (path: string) => createHash('sha256').update(readFileSync(path)).digest('hex');
const build = { captured_before_execution: true, main_sha256: hash(join(root, 'apps/desktop/out/main/index.js')),
  preload_sha256: hash(join(root, 'apps/desktop/out/preload/index.cjs')), runtime_sha256: hash(values.runtime),
  renderer_assets: Object.fromEntries(readdirSync(join(root, 'apps/desktop/out/renderer/assets')).sort().map(file => [file, hash(join(root, 'apps/desktop/out/renderer/assets', file))])),
  binaries: Object.fromEntries(['cu-host', 'cu-device', 'cu-source', 'cu-macos-source', 'cu-macos-device-guard', 'cu-macos-keychain'].map(file => [file, hash(join(root, 'target/debug', file))])),
  driver_sha256: hash(join(root, 'target/native-macos/consent-dialog-driver')),
  runner_sha256: hash(fileURLToPath(import.meta.url)),
  manual_smoke_sha256: hash(join(root, 'apps/desktop/test/manual/modelAuthorizationSmoke.cjs')) };
const runs: unknown[] = [];
let error: unknown;
let accountCleanupVerified = false;
let accountCleanupError: string | null = null;
try {
  for (const phase of ['create', 'reopen']) {
    const child = spawn(values.runtime, [join(root, 'apps/desktop/test/manual/modelAuthorizationSmoke.cjs')], { cwd: root,
      env: { ...process.env, CU_MODEL_AUTH_ROOT: root, CU_MODEL_AUTH_OUTPUT: output, CU_MODEL_AUTH_MODEL: model, CU_MODEL_AUTH_PHASE: phase }, stdio: ['ignore', 'pipe', 'pipe'] });
    let log = ''; child.stdout.on('data', d => { log = (log + d).slice(-8192); }); child.stderr.on('data', d => { log = (log + d).slice(-8192); });
    const timeout = setTimeout(() => child.kill('SIGTERM'), 90_000);
    const code = await new Promise<number | null>((done, fail) => { child.once('error', fail); child.once('close', done); }).finally(() => clearTimeout(timeout));
    const file = join(output, phase + '-result.json');
    const result = existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : { ok: false, error: log };
    runs.push({ phase, ...result, runtime_exit_code: code });
    if (!result.ok || code !== 0) { writeFileSync(join(output, phase + '-runtime.log'), log); throw new Error('Owned model authorization smoke failed.'); }
  }
} catch (value) { error = value; }
finally {
  // Cleanup precisely the unique dummy-key account, also after a GUI failure.
  try {
    const reply = JSON.parse(execFileSync(join(root, 'target/debug/cu-macos-keychain'), [], { input: JSON.stringify({ operation: 'forget', account }), encoding: 'utf8' }));
    accountCleanupVerified = reply.ok === true;
    if (!reply.ok) throw new Error('Owned test account cleanup was not confirmed.');
  } catch {
    accountCleanupError = 'Owned test account cleanup unavailable or unconfirmed.';
    error ??= new Error(accountCleanupError);
  }
}
const previousRecord = join(root, 'artifacts/m4-model-authorization.json');
const previousRecordSha256 = existsSync(previousRecord) ? hash(previousRecord) : null;
const recordName = `m4-model-authorization-purposes-${runId}.json`;
const indexName = `m4-model-authorization-purpose-index-${runId}.json`;
const record = join(root, 'artifacts', recordName);
const index = join(root, 'artifacts', indexName);
writeFileSync(record, JSON.stringify({ format_version: 1, ok: error === undefined, tested_at: new Date().toISOString(),
  git_head: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(), uncommitted_work: true,
  cargo_lock_sha256: hash(join(root, 'Cargo.lock')), build, output, runs,
  account_cleanup_verified: accountCleanupVerified, account_cleanup_error: accountCleanupError,
  test_account: account, real_provider_calls: 0, live_provider_verified: false, signed_keychain_access_verified: false,
  supersedes: { path: 'artifacts/m4-model-authorization.json', sha256: previousRecordSha256 } }, null, 2) + '\n');
writeFileSync(index, JSON.stringify({ format_version: 1, purpose_smoke: recordName, purpose_smoke_sha256: hash(record),
  superseded_historical_record: 'artifacts/m4-model-authorization.json', superseded_historical_record_sha256: previousRecordSha256 }, null, 2) + '\n');
console.log(JSON.stringify({ ok: error === undefined, record, index, output }));
if (error) throw error;
