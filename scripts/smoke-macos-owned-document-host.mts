// Product Supervisor -> trusted Host -> Engine owned read-only admission.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Supervisor } from '../apps/desktop/src/main/supervisor.js';
import { activateOwnedDocumentForObservation, createOwnedDocumentLaunchRunner,
  encodeOwnedDocumentSourceBootstrap } from '../apps/desktop/src/main/ownedDocumentLaunch.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
assert.ok(args.length === 0 || (args.length === 1 && args[0] === '--value-readback'), 'unsupported smoke arguments');
const requireValueReadback = args.includes('--value-readback');
const initialText = requireValueReadback ? 'Owned Host scope\n只读 Engine 验收 👩🏽‍💻\tCafé\n' : 'Owned Host scope\n只读 Engine 验收\n';
const artifact = join(root, 'artifacts', `m3-owned-document-host-${randomUUID()}.json`);
const runner = createOwnedDocumentLaunchRunner();
const hash = (path: string) => createHash('sha256').update(readFileSync(path)).digest('hex');
const paths = {
  host: join(root, 'target/debug/cu-host'), source: join(root, 'target/debug/cu-source'),
  device: join(root, 'target/debug/cu-device'), helper: join(root, 'target/debug/cu-macos-source'),
};
const hashes = Object.fromEntries(Object.entries(paths).map(([role, path]) => [role, hash(path)]));
const sourcePaths = ['apps/desktop/src/main/supervisor.ts', 'apps/desktop/src/main/ownedDocumentLaunch.ts',
  'crates/interaction-contracts/src/desktop/owned_registration.rs',
  'crates/interaction-contracts/src/desktop/owned_document.rs',
  'crates/interaction-engine/src/environment/owned_document.rs',
  'crates/source-macos/src/native/watch/tracker.rs',
  'crates/desktop-host/src/shell.rs',
  'native/macos/AXSession.swift', 'native/macos/OwnedDocumentReceiver.swift'];
const sourceHashes = Object.fromEntries(sourcePaths.map(path => [path, hash(join(root, path))]));
const evidence: Record<string, unknown> = { kind: 'product_owned_document_host_readonly_smoke',
  schema_version: 1, started_at: new Date().toISOString(), ok: false, input_grant: false,
  binary_sha256: hashes, source_sha256: sourceHashes, script_sha256: hash(fileURLToPath(import.meta.url)),
  value_readback_required: requireValueReadback, value_readback_verified: false,
  editing_verified: false, save_reopen_verified: false, mvp_complete: false };
let supervisor: Supervisor | null = null;
const scopes: Array<{ source_sequence: number; binding_current: boolean; input_grant: boolean;
  value_readback_present: boolean; value_readback_utf8_byte_count: number | null;
  value_readback_matches_initial_file: boolean | null }> = [];
const accepted = () => scopes.filter(scope => scope.binding_current && (!requireValueReadback ||
  (scope.value_readback_present === true && scope.value_readback_matches_initial_file === true &&
    scope.value_readback_utf8_byte_count === Buffer.byteLength(initialText, 'utf8'))));
const exitedRoles: string[] = [];
try {
  const registration = await runner.launch(initialText);
  assert.equal(await activateOwnedDocumentForObservation(registration), true);
  const prefix = encodeOwnedDocumentSourceBootstrap(registration);
  const claims = JSON.parse(prefix.subarray(4).toString('utf8'));
  prefix.fill(0);
  supervisor = await Supervisor.start({
    binaries: { host: paths.host, device: paths.device, source: paths.source },
    readyTimeoutMs: 12_000, ownedDocument: registration,
    nativeSource: { helperPath: paths.helper, bundleId: 'com.apple.TextEdit', pid: claims.pid },
  });
  supervisor.on('exit', role => exitedRoles.push(role));
  supervisor.on('log', line => {
    const start = line.indexOf('{');
    if (!line.startsWith('host: ') || start < 0) return;
    try {
      const value = JSON.parse(line.slice(start));
      if (value.kind === 'owned_document_readonly_scope' && Number.isSafeInteger(value.source_sequence)) {
        scopes.push({ source_sequence: value.source_sequence, binding_current: value.binding_current,
          input_grant: value.input_grant, value_readback_present: value.value_readback_present,
          value_readback_utf8_byte_count: value.value_readback_utf8_byte_count,
          value_readback_matches_initial_file: value.value_readback_matches_initial_file });
      }
    } catch { /* Other bounded process diagnostics are not acceptance records. */ }
  });
  const deadline = Date.now() + 20_000;
  while (new Set(accepted().map(scope => scope.source_sequence)).size < 3
    && Date.now() < deadline && exitedRoles.length === 0) {
    await new Promise(done => setTimeout(done, 40));
  }
  assert.deepEqual(exitedRoles, [], 'a product peer exited before admission');
  assert.ok(new Set(accepted().map(scope => scope.source_sequence)).size >= 3,
    requireValueReadback ? 'Host did not establish three exact owned UI value readbacks' :
      'Host did not establish three current owned read-only bindings');
  assert.ok(scopes.every(scope => scope.input_grant === false));
  assert.equal(supervisor.projection?.mode, 'observe');
  const publicProjection = JSON.stringify(supervisor.projection);
  for (const value of [claims.registration_id, claims.document_path, claims.document_sha256]) {
    assert.equal(publicProjection.includes(value), false, 'private registration leaked into projection');
  }
  const counts = execFileSync('/usr/bin/sqlite3', [join(supervisor.dir, 'host.db'),
    'SELECT (SELECT COUNT(*) FROM attempts),(SELECT COUNT(*) FROM root_tasks),'
      + '(SELECT COUNT(*) FROM decision_model_calls),(SELECT COUNT(*) FROM model_calls);'],
  { encoding: 'utf8', timeout: 2_000 }).trim().split('|').map(Number);
  assert.deepEqual(counts, [0, 0, 0, 0]);
  evidence.ledger_counts = { attempts: counts[0], roots: counts[1], s1_calls: counts[2], s2_calls: counts[3] };
  evidence.input_calls = 0;
  evidence.host_owned_binding_verified = true;
  evidence.value_readback_verified = requireValueReadback;
  if (requireValueReadback) evidence.value_readback_utf8_byte_count = Buffer.byteLength(initialText, 'utf8');
  evidence.private_registration_not_projected = true;
  evidence.mode = supervisor.projection.mode;
  evidence.ok = true;
} catch (error) {
  evidence.failure = error instanceof Error ? error.message : 'Host read-only acceptance failed';
  process.exitCode = 1;
} finally {
  await supervisor?.shutdown();
  evidence.owned_cleanup = await runner.stopAll();
  if (!(evidence.owned_cleanup as string[]).every(result => result === 'stopped')) {
    evidence.ok = false; process.exitCode = 1;
  }
  evidence.scopes = scopes;
  evidence.exited_roles = exitedRoles;
  evidence.binary_hashes_unchanged = Object.entries(paths).every(([role, path]) => hash(path) === hashes[role]);
  evidence.source_hashes_unchanged = sourcePaths.every(path => hash(join(root, path)) === sourceHashes[path]);
  if (!evidence.binary_hashes_unchanged || !evidence.source_hashes_unchanged) { evidence.ok = false; process.exitCode = 1; }
  evidence.finished_at = new Date().toISOString();
  writeFileSync(artifact, `${JSON.stringify(evidence, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ ok: evidence.ok, artifact, scopes: scopes.length, cleanup: evidence.owned_cleanup }));
}
