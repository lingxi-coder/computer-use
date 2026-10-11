// Lifecycle foundation only: close and reopen the same private saved file.
// This does not attest a controlled Save or complete a Root UiRevisit goal.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Supervisor } from '../apps/desktop/src/main/supervisor.js';
import { activateOwnedDocumentForObservation, createOwnedDocumentLaunchRunner,
  encodeOwnedDocumentSourceBootstrap, OwnedDocumentLifecycleError,
  type OwnedDocumentRegistration } from '../apps/desktop/src/main/ownedDocumentLaunch.js';

assert.deepEqual(process.argv.slice(2), ['--revisit']);
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const artifact = join(root, 'artifacts', `m3-owned-document-revisit-${randomUUID()}.json`);
const text = 'Revisit owned file\n中文 👩🏽‍💻\tCafé\n';
const runner = createOwnedDocumentLaunchRunner();
const paths = { host: join(root, 'target/debug/cu-host'), device: join(root, 'target/debug/cu-device'),
  source: join(root, 'target/debug/cu-source'), helper: join(root, 'target/debug/cu-macos-source'),
  launcher: join(root, 'target/debug/cu-macos-owned-document-launcher') };
const hash = (path: string) => createHash('sha256').update(readFileSync(path)).digest('hex');
const hashes = Object.fromEntries(Object.entries(paths).map(([key, path]) => [key, hash(path)]));
const mainPath = join(root, 'apps/desktop/src/main/ownedDocumentLaunch.ts');
const mainHash = hash(mainPath);
const evidence: Record<string, unknown> = { kind: 'owned_document_revisit_lifecycle_foundation', schema_version: 1,
  started_at: new Date().toISOString(), ok: false, input_calls: 0, controlled_save_verified: false,
  root_revisit_verified: false, mvp_complete: false, binary_sha256: hashes,
  main_sha256: mainHash, script_sha256: hash(fileURLToPath(import.meta.url)) };
const session: { current: Supervisor | null } = { current: null };
let phase = 'launch';
let peerExits = 0;
const claims = (registration: OwnedDocumentRegistration) => {
  const prefix = encodeOwnedDocumentSourceBootstrap(registration);
  try { return JSON.parse(prefix.subarray(4).toString('utf8')); }
  finally { prefix.fill(0); }
};
async function readValue(registration: OwnedDocumentRegistration): Promise<void> {
  const identity = claims(registration);
  assert.equal(await activateOwnedDocumentForObservation(registration), true);
  const supervisor = await Supervisor.start({ binaries: { host: paths.host, device: paths.device, source: paths.source },
    readyTimeoutMs: 12_000, ownedDocument: registration,
    nativeSource: { helperPath: paths.helper, bundleId: 'com.apple.TextEdit', pid: identity.pid } });
  session.current = supervisor;
  supervisor.on('exit', () => { peerExits += 1; });
  const current = new Set<number>();
  let everGranted = false;
  supervisor.on('log', line => {
    const start = line.indexOf('{');
    if (!line.startsWith('host: ') || start < 0) return;
    try {
      const value = JSON.parse(line.slice(start));
      if (value.kind !== 'owned_document_readonly_scope' || !Number.isSafeInteger(value.source_sequence)) return;
      everGranted ||= value.input_grant === true;
      if (value.binding_current === true && value.value_readback_present === true
        && value.value_readback_matches_initial_file === true
        && value.value_readback_utf8_byte_count === Buffer.byteLength(text, 'utf8')) {
        current.add(value.source_sequence);
      }
    } catch { /* Only fixed typed metadata is consumed. */ }
  });
  const deadline = Date.now() + 15_000;
  while (current.size < 3 && Date.now() < deadline) {
    assert.equal(peerExits, 0);
    await new Promise(done => setTimeout(done, 30));
  }
  assert.ok(current.size >= 3);
  assert.equal(everGranted, false);
  assert.equal(supervisor.projection?.mode, 'observe');
  const counts = execFileSync('/usr/bin/sqlite3', [join(supervisor.dir, 'host.db'),
    'SELECT (SELECT COUNT(*) FROM attempts),(SELECT COUNT(*) FROM root_tasks),'
    + '(SELECT COUNT(*) FROM decision_model_calls),(SELECT COUNT(*) FROM model_calls);'],
    { encoding: 'utf8', timeout: 2000 }).trim();
  assert.equal(counts, '0|0|0|0');
  const publicProjection = JSON.stringify(supervisor.projection);
  for (const secret of [identity.registration_id, identity.document_path, identity.document_sha256]) {
    assert.equal(publicProjection.includes(secret), false);
  }
  await supervisor.shutdown(); session.current = null;
}
try {
  const old = await runner.launch(text);
  const before = claims(old);
  phase = 'old_value'; await readValue(old);
  phase = 'retain';
  const ticket = await old.retainForRevisit(text);
  assert.deepEqual(Object.keys(ticket), []);
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(old));
  phase = 'reopen';
  const fresh = await ticket.reopen();
  const after = claims(fresh);
  assert.equal(after.document_path, before.document_path);
  assert.equal(after.document_sha256, before.document_sha256);
  assert.equal(after.directory_dev, before.directory_dev); assert.equal(after.directory_ino, before.directory_ino);
  assert.notEqual(after.registration_id, before.registration_id);
  assert.ok(after.pid !== before.pid || after.start_seconds !== before.start_seconds || after.start_micros !== before.start_micros);
  await assert.rejects(ticket.reopen());
  phase = 'reopened_value'; await readValue(fresh);
  evidence.old_and_reopened_ui_values_verified = true;
  evidence.same_private_file_preserved = true;
  evidence.new_process_and_registration_verified = true;
  evidence.old_registration_retired = true;
  evidence.single_consumption_verified = true;
  evidence.input_grant = false;
  evidence.sdk_calls = 0;
  evidence.ok = true;
} catch (error) {
  evidence.failure_phase = phase;
  evidence.failure_type = error instanceof Error ? error.name : 'unknown';
  if (error instanceof OwnedDocumentLifecycleError) evidence.failure_code = error.code;
  process.exitCode = 1;
} finally {
  try { await session.current?.shutdown(); }
  catch { evidence.supervisor_cleanup_failed = true; evidence.ok = false; process.exitCode = 1; }
  try { evidence.owned_cleanup = await runner.stopAll(); }
  catch { evidence.owned_cleanup = ['unknown']; }
  evidence.peer_exits = peerExits;
  evidence.binary_hashes_unchanged = Object.entries(paths).every(([key, path]) => hash(path) === hashes[key]);
  evidence.main_hash_unchanged = hash(mainPath) === mainHash;
  if (!(evidence.owned_cleanup as string[]).every(result => result === 'stopped') || peerExits
    || !evidence.binary_hashes_unchanged || !evidence.main_hash_unchanged) {
    evidence.ok = false; process.exitCode = 1;
  }
  evidence.finished_at = new Date().toISOString();
  writeFileSync(artifact, JSON.stringify(evidence, null, 2) + '\n', { mode: 0o600, flag: 'wx' });
  console.log(JSON.stringify({ ok: evidence.ok, artifact, cleanup: evidence.owned_cleanup, mvp_complete: false }));
}
