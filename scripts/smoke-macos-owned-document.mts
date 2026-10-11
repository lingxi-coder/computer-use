// Exercise the product launch runner with one fresh private document and no AX/input calls.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { lstatSync, readFileSync, readdirSync, realpathSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createOwnedDocumentLaunchRunner } from '../apps/desktop/src/main/ownedDocumentLaunch.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const tempBase = realpathSync(tmpdir());
const helper = join(root, 'target/debug/cu-macos-owned-document-launcher');
const runId = randomUUID();
const artifact = join(root, 'artifacts', `m2-owned-document-launch-${runId}.json`);
const text = 'Owned launch verification\n中文：独立临时文档\nemoji: 🧪\ttab\r\n';
const sha256 = (path: string): string => createHash('sha256').update(readFileSync(path)).digest('hex');
const ownedDirectories = (): string[] => readdirSync(tempBase).filter(name => name.startsWith('cu-owned-document-')).sort();
const textEditPIDs = (): number[] => execFileSync('/bin/ps', ['-axo', 'pid=,comm='], {
  encoding: 'utf8', timeout: 2_000, maxBuffer: 2 * 1024 * 1024,
}).split('\n').flatMap(line => {
  const match = line.trim().match(/^(\d+)\s+(.+)$/);
  return match?.[2] === '/System/Applications/TextEdit.app/Contents/MacOS/TextEdit' ? [Number(match[1])] : [];
}).sort((left, right) => left - right);

const evidence: Record<string, unknown> = {
  schema_version: 1, kind: 'product_owned_document_launch_smoke', run_id: runId,
  started_at: new Date().toISOString(), ok: false, input_calls: 0,
  text_editing_verified: false, save_reopen_verified: false, mvp_complete: false,
  helper_sha256: sha256(helper),
  main_runner_sha256: sha256(join(root, 'apps/desktop/src/main/ownedDocumentLaunch.ts')),
  native_launcher_source_sha256: sha256(join(root, 'native/owned-document/OwnedDocumentLauncher.swift')),
  smoke_source_sha256: sha256(fileURLToPath(import.meta.url)),
  text_sha256: createHash('sha256').update(text).digest('hex'),
};
const directoriesBefore = ownedDirectories();
const pidsBefore = textEditPIDs();
evidence.preexisting_text_edit_pids = pidsBefore;
const runner = createOwnedDocumentLaunchRunner();
const baselineRequested = process.argv.includes('--preexisting-owned');
const baselineRunner = baselineRequested ? createOwnedDocumentLaunchRunner() : null;
const directoriesOriginal = ownedDirectories();
let finished = false;
// A failed private receipt can deliberately retain an unresolved child. Bound
// this diagnostic process; its exit closes only the helper's own stdin pipe.
const watchdog = setTimeout(() => {
  evidence.ok = false;
  evidence.diagnostic_deadline_exceeded = true;
  evidence.finished_at = new Date().toISOString();
  writeFileSync(artifact, `${JSON.stringify(evidence, null, 2)}\n`, { mode: 0o600 });
  console.log(JSON.stringify({ ok: false, artifact, diagnostic_deadline_exceeded: true }));
  process.exit(1);
}, 55_000);
try {
  if (baselineRunner) {
    await baselineRunner.launch('Owned baseline instance\n自有基线，保持未编辑\n');
    const baselinePIDs = textEditPIDs().filter(pid => !pidsBefore.includes(pid));
    assert.equal(baselinePIDs.length, 1);
    pidsBefore.push(baselinePIDs[0]!);
    directoriesBefore.push(...ownedDirectories().filter(name => !directoriesBefore.includes(name)));
    evidence.preexisting_owned_baseline_pids = baselinePIDs;
  }
  const registration = await runner.launch(text);
  assert.deepEqual(Object.keys(registration), [], 'Main registration exposed identity fields');
  const directoriesAfterLaunch = ownedDirectories();
  const created = directoriesAfterLaunch.filter(name => !directoriesBefore.includes(name));
  assert.equal(created.length, 1, 'expected one private document directory');
  const directory = join(tempBase, created[0]!);
  const document = join(directory, 'document.txt');
  const directoryStat = lstatSync(directory);
  const documentStat = lstatSync(document);
  assert.equal(directoryStat.mode & 0o777, 0o700);
  assert.equal(documentStat.mode & 0o777, 0o600);
  assert.equal(documentStat.nlink, 1);
  assert.equal(documentStat.uid, process.getuid?.());
  assert.deepEqual(readFileSync(document), Buffer.from(text, 'utf8'));
  const pidsAfterLaunch = textEditPIDs();
  const newPIDs = pidsAfterLaunch.filter(pid => !pidsBefore.includes(pid));
  assert.equal(newPIDs.length, 1, 'expected one fresh TextEdit process');
  evidence.owned_private_directory = directory;
  evidence.observed_new_text_edit_pids = newPIDs;
  evidence.private_file_bytes_preserved = true;
  evidence.private_modes_verified = true;
  // Process-list observations are diagnostic only; the runner validates its private native receipt.
  evidence.process_list_observation_grants_authority = false;
  evidence.stop_result = await registration.stop();
  assert.equal(evidence.stop_result, 'stopped');
  assert.deepEqual(await runner.stopAll(), []);
  const remaining = ownedDirectories();
  assert.ok(!remaining.includes(created[0]!));
  const pidsAfterStop = textEditPIDs();
  assert.ok(newPIDs.every(pid => !pidsAfterStop.includes(pid)));
  assert.ok(pidsBefore.every(pid => pidsAfterStop.includes(pid)), 'a preexisting TextEdit process disappeared');
  evidence.private_directory_removed = true;
  evidence.new_instance_gone = true;
  evidence.preexisting_instances_preserved = true;
  if (baselineRunner) {
    assert.deepEqual(await baselineRunner.stopAll(), ['stopped']);
    const baselinePIDs = evidence.preexisting_owned_baseline_pids as number[];
    assert.ok(baselinePIDs.every(pid => !textEditPIDs().includes(pid)));
    assert.deepEqual(ownedDirectories(), directoriesOriginal);
    evidence.preexisting_owned_baseline_cleanup = true;
  }
  evidence.helper_hash_unchanged = sha256(helper) === evidence.helper_sha256;
  assert.equal(evidence.helper_hash_unchanged, true);
  evidence.ok = true;
  finished = true;
} catch (error) {
  evidence.failure = error instanceof assert.AssertionError ? error.message : 'Product owned document launch did not complete.';
  process.exitCode = 1;
} finally {
  if (!finished) {
    try {
      evidence.failure_cleanup_results = [
        ...await runner.stopAll(),
        ...(baselineRunner ? await baselineRunner.stopAll() : []),
      ];
    }
    catch { evidence.failure_cleanup_results = ['unknown']; }
  }
  evidence.finished_at = new Date().toISOString();
  writeFileSync(artifact, `${JSON.stringify(evidence, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ ok: evidence.ok, artifact, stop_result: evidence.stop_result ?? null }));
  if (finished || (Array.isArray(evidence.failure_cleanup_results) && evidence.failure_cleanup_results.length === 0)) clearTimeout(watchdog);
}
