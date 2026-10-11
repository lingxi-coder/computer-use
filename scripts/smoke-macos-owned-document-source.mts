// One fresh owned document; exercise the private Main -> resident AX Source pipe.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  activateOwnedDocumentForObservation, createOwnedDocumentLaunchRunner,
  encodeOwnedDocumentSourceBootstrap,
} from '../apps/desktop/src/main/ownedDocumentLaunch.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const runID = randomUUID();
const artifact = join(root, 'artifacts', `m2-owned-document-source-${runID}.json`);
const sha = (path: string): string => createHash('sha256').update(readFileSync(path)).digest('hex');
const helper = join(root, 'target/debug/cu-macos-source');
const runner = createOwnedDocumentLaunchRunner();
const evidence: Record<string, unknown> = {
  schema_version: 1, kind: 'owned_document_private_source_smoke', run_id: runID,
  started_at: new Date().toISOString(), ok: false, input_calls: 0,
  text_editing_verified: false, save_reopen_verified: false, mvp_complete: false,
  source_helper_sha256: sha(helper),
  launcher_sha256: sha(join(root, 'target/debug/cu-macos-owned-document-launcher')),
  main_source_sha256: sha(join(root, 'apps/desktop/src/main/ownedDocumentLaunch.ts')),
  bootstrap_source_sha256: sha(join(root, 'native/macos/OwnedDocumentBootstrap.swift')),
  smoke_source_sha256: sha(fileURLToPath(import.meta.url)),
};
let source: ReturnType<typeof spawn> | null = null;
let closed = false;
let stderrBytes = 0;
let stderr = '';
const records: Array<Record<string, any>> = [];
const eligibleRecords: Array<Record<string, any>> = [];
const watchdog = setTimeout(() => {
  source?.kill('SIGKILL');
  evidence.failure = 'Private Source diagnostic deadline exceeded.';
  evidence.records = records;
  evidence.finished_at = new Date().toISOString();
  writeFileSync(artifact, `${JSON.stringify(evidence, null, 2)}\n`, { mode: 0o600 });
  process.exit(1);
}, 55_000);

try {
  const registration = await runner.launch('Owned Source fixture\n中文与 English 🧪\n');
  evidence.activation_api_returned = await activateOwnedDocumentForObservation(registration);
  assert.equal(evidence.activation_api_returned, true);
  const prefix = encodeOwnedDocumentSourceBootstrap(registration);
  const claims = JSON.parse(prefix.subarray(4).toString('utf8'));
  evidence.registration_id = claims.registration_id;
  evidence.owned_target_pid = claims.pid;
  source = spawn(helper, ['--owned-document-bootstrap'], {
    cwd: root, env: { TMPDIR: tmpdir(), LANG: 'en_US.UTF-8' }, stdio: ['pipe', 'pipe', 'pipe'],
  });
  source.once('close', (code, signal) => { closed = true; evidence.source_exit = { code, signal }; });
  source.stdin!.on('error', () => { /* The write callback or early-close gate reports this failure. */ });
  source.stderr!.on('data', (bytes: Buffer) => {
    stderrBytes += bytes.length;
    if (stderrBytes <= 16 * 1024) stderr += bytes.toString('utf8');
  });
  let buffered = '';
  const acquired = new Promise<void>((done, fail) => {
    const deadline = setTimeout(() => fail(new Error('Source observations exceeded their deadline.')), 20_000);
    const complete = () => { clearTimeout(deadline); done(); };
    const reject = (error: Error) => { clearTimeout(deadline); fail(error); };
    source!.once('error', () => reject(new Error('Source process start failed.')));
    source!.once('close', () => { if (eligibleRecords.length < 3) reject(new Error('Source closed before three eligible observations.')); });
    source!.stdout!.on('data', (bytes: Buffer) => {
      buffered += bytes.toString('utf8');
      if (Buffer.byteLength(buffered) > 768 * 1024) { reject(new Error('Source response exceeded its limit.')); return; }
      while (buffered.includes('\n')) {
        const index = buffered.indexOf('\n');
        const line = buffered.slice(0, index); buffered = buffered.slice(index + 1);
        try {
          const record = JSON.parse(line);
          if (record.kind !== 'observation') { reject(new Error('Source returned a protocol error.')); return; }
          if (records.length < 8) records.push(record);
          if (record.structure?.owned_document_evidence?.profile_eligible) eligibleRecords.push(record);
          else eligibleRecords.length = 0;
          if (eligibleRecords.length >= 3) complete();
        } catch { reject(new Error('Source returned malformed JSON.')); }
      }
    });
  });
  // A child can close while the prefix write is still pending.
  void acquired.catch(() => undefined);
  await new Promise<void>((done, fail) => source!.stdin!.write(prefix, error => {
    prefix.fill(0);
    if (error) fail(new Error('Private Source prefix write failed.'));
    else done();
  }));
  source.stdin!.end(`${JSON.stringify({ protocol_version: 1, command: 'watch', bundle_id: claims.bundle_id,
    pid: claims.pid, timeout_ms: 5000, interval_ms: 250, max_nodes: 512, max_depth: 12 })}\n`);
  await acquired;
  assert.ok(records.every(record => record.structure.complete === false));
  const proofs = eligibleRecords.map(record => record.structure.owned_document_evidence);
  assert.ok(proofs.every(proof => proof?.profile_eligible && proof.scope_complete && proof.document_binding_verified),
    'Source did not provide independently eligible owned-document scope');
  assert.ok(proofs.every(proof => proof.registration_id === claims.registration_id && proof.process.pid === claims.pid));
  assert.ok(eligibleRecords.every((record, index) => proofs[index].provenance.snapshot_id === `${record.stream_id}-tree-${record.sequence}`
    && proofs[index].provenance.source_sequence === record.sequence
    && proofs[index].provenance.valid_until_ms > record.received_unix_ms));
  assert.ok(proofs.every(proof => proof.provenance.identity_boundary.namespace === proofs[0].provenance.identity_boundary.namespace
    && proof.provenance.identity_boundary.generation === proofs[0].provenance.identity_boundary.generation
    && proof.actual_object_ref === proofs[0].actual_object_ref), 'Stable owned receiver lost its resident identity');
  evidence.generic_complete_remained_false = true;
  evidence.owned_scope_eligible = true;
  evidence.resident_receiver_identity_stable = true;
  evidence.eligible_observations = eligibleRecords.length;
  evidence.ok = true;
} catch (error) {
  evidence.failure = error instanceof Error ? error.message : 'Private owned Source observation failed.';
  process.exitCode = 1;
} finally {
  if (source && !closed) {
    source.kill('SIGTERM');
    await new Promise<void>(done => {
      const timer = setTimeout(() => { if (!closed) source?.kill('SIGKILL'); done(); }, 3_000);
      source!.once('close', () => { clearTimeout(timer); done(); });
    });
  }
  evidence.records = records;
  evidence.eligible_records = eligibleRecords;
  evidence.source_stderr = stderr;
  evidence.source_stderr_bytes = stderrBytes;
  evidence.cleanup_results = await runner.stopAll();
  if (!(evidence.cleanup_results as string[]).every(result => result === 'stopped')) {
    evidence.ok = false; process.exitCode = 1;
  }
  evidence.source_hash_unchanged = sha(helper) === evidence.source_helper_sha256;
  evidence.finished_at = new Date().toISOString();
  writeFileSync(artifact, `${JSON.stringify(evidence, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  clearTimeout(watchdog);
  console.log(JSON.stringify({ ok: evidence.ok, artifact, records: records.length, cleanup: evidence.cleanup_results }));
}
