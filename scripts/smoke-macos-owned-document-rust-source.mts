// Exercise Main's opaque owned-document registration through the Rust Source
// process and its authenticated Host socket. This is observation-only.
import assert from 'node:assert/strict';
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import {
  chmodSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync,
} from 'node:fs';
import { connect as connectUnix, type Socket } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  encodeFrame, EnvelopeFactory, FrameDecoder, PROTOCOL_VERSION,
  type Envelope, type SourceHealth, type StructureSnapshot,
} from '@computer-use/desktop-api';
import {
  activateOwnedDocumentForObservation, createOwnedDocumentLaunchRunner,
  encodeOwnedDocumentSourceBootstrap,
} from '../apps/desktop/src/main/ownedDocumentLaunch.js';
import type { OwnedDocumentSourceBootstrap } from '../apps/desktop/src/main/ownedDocumentBootstrap.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const runId = randomUUID();
const artifact = join(root, 'artifacts', `m3-owned-document-rust-source-${runId}.json`);
const clockOriginMs = Date.now();
const clockDomain = 'macos/1/wall';
const tempBase = realpathSync(tmpdir());
const sourceRunDir = mkdtempSync(join(tempBase, 'cu-owned-rust-source-'));
chmodSync(sourceRunDir, 0o700);
const assetsDir = join(sourceRunDir, 'native-assets');
mkdirSync(assetsDir, { mode: 0o700 });
chmodSync(assetsDir, 0o700);

type QueuedWaiter = { resolve: (envelope: Envelope) => void; reject: (error: Error) => void; timer: NodeJS.Timeout };

class EnvelopeQueue {
  private readonly decoder = new FrameDecoder();
  private readonly queued: Envelope[] = [];
  private readonly waiters: QueuedWaiter[] = [];
  private failure: Error | null = null;

  push(bytes: Buffer): void {
    if (this.failure) return;
    try {
      this.decoder.push(bytes);
      for (let envelope = this.decoder.next(); envelope; envelope = this.decoder.next()) {
        const waiter = this.waiters.shift();
        if (waiter) {
          clearTimeout(waiter.timer);
          waiter.resolve(envelope);
        } else {
          if (this.queued.length >= 16) throw new Error('queue_limit');
          this.queued.push(envelope);
        }
      }
    } catch {
      this.fail(new Error('invalid_desktop_ipc_frame'));
    }
  }

  fail(error: Error): void {
    if (this.failure) return;
    this.failure = error;
    for (const waiter of this.waiters.splice(0)) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
  }

  next(timeoutMs: number): Promise<Envelope> {
    const envelope = this.queued.shift();
    if (envelope) return Promise.resolve(envelope);
    if (this.failure) return Promise.reject(this.failure);
    return new Promise((resolveNext, rejectNext) => {
      const waiter: QueuedWaiter = {
        resolve: resolveNext,
        reject: rejectNext,
        timer: setTimeout(() => {
          this.waiters.splice(this.waiters.indexOf(waiter), 1);
          rejectNext(new Error('desktop_ipc_timeout'));
        }, timeoutMs),
      };
      this.waiters.push(waiter);
    });
  }
}

function sha256(path: string): string {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

function writeBuffer(stream: NodeJS.WritableStream, buffer: Buffer): Promise<void> {
  return new Promise((resolveWrite, rejectWrite) => {
    stream.write(buffer, (error?: Error | null) => error ? rejectWrite(new Error('pipe_write_failed')) : resolveWrite());
  });
}

function textEditPids(): number[] {
  const output = requireProcessList();
  return output.split('\n').flatMap((line) => {
    const match = line.trim().match(/^(\d+)\s+(.+)$/);
    return match?.[2] === '/System/Applications/TextEdit.app/Contents/MacOS/TextEdit'
      ? [Number(match[1])]
      : [];
  }).sort((left, right) => left - right);
}

function requireProcessList(): string {
  // Process-list data is used only for local cleanup checks and is never saved or printed.
  return execFileSync('/bin/ps', ['-axo', 'pid=,comm='], {
    encoding: 'utf8', timeout: 2_000, maxBuffer: 2 * 1024 * 1024,
  });
}

function processExists(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code !== 'ESRCH';
  }
}

async function waitForProcessExit(child: ChildProcess, timeoutMs: number): Promise<{ code: number | null; signal: NodeJS.Signals | null } | null> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return { code: child.exitCode, signal: child.signalCode };
  }
  return new Promise((resolveExit) => {
    const timer = setTimeout(() => {
      child.off('close', onClose);
      resolveExit(null);
    }, timeoutMs);
    const onClose = (code: number | null, signal: NodeJS.Signals | null): void => {
      clearTimeout(timer);
      resolveExit({ code, signal });
    };
    child.once('close', onClose);
  });
}

async function waitForPidExit(pid: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!processExists(pid)) return true;
    await new Promise((done) => setTimeout(done, 50));
  }
  return !processExists(pid);
}

const evidence: Record<string, unknown> = {
  schema_version: 1,
  kind: 'product_owned_document_rust_source_smoke',
  run_id: runId,
  started_at: new Date().toISOString(),
  ok: false,
  input_calls: 0,
  snapshots: [],
  source_configured_over_desktop_ipc: false,
  host_socket_authenticated: false,
  generic_completeness_preserved: false,
  generic_health_preserved: false,
  private_clock_mapping_verified: false,
  private_identity_stable: false,
  source_stopped: false,
  owned_text_edit_cleaned: false,
};

const ownedDirectories = (): string[] => readdirSync(tempBase)
  .filter((name) => name.startsWith('cu-owned-document-')).sort();
const ownedDirectoriesBefore = ownedDirectories();
const textEditPidsBefore = textEditPids();
const runner = createOwnedDocumentLaunchRunner();
let registration: Awaited<ReturnType<typeof runner.launch>> | null = null;
let source: ChildProcess | null = null;
let sourceRunFactory: EnvelopeFactory | null = null;
let sourceClosed = false;
let sourceExit: Promise<{ code: number | null; signal: NodeJS.Signals | null }> | null = null;
let sourceSocket: Socket | null = null;
const sourceQueue = new EnvelopeQueue();
const hostQueue = new EnvelopeQueue();
const observed: StructureSnapshot[] = [];
const health: SourceHealth[] = [];
let structureRecordsSeen = 0;
let structuresWithoutOwnedSidecar = 0;
let targetPid: number | null = null;
let stage = 'startup';
let timeoutExpired = false;
const deadline = setTimeout(() => {
  timeoutExpired = true;
  sourceQueue.fail(new Error('smoke_deadline'));
  hostQueue.fail(new Error('smoke_deadline'));
  sourceSocket?.destroy();
  source?.kill('SIGKILL');
}, 60_000);
deadline.unref();

try {
  assert.equal(process.platform, 'darwin', 'macOS is required');
  stage = 'resolve_product_binaries';
  const sourceBinary = join(root, 'target/debug/cu-source');
  const nativeHelper = join(root, 'target/debug/cu-macos-source');
  const launcher = join(root, 'target/debug/cu-macos-owned-document-launcher');
  Object.assign(evidence, {
    source_binary_sha256: sha256(sourceBinary),
    native_helper_sha256: sha256(nativeHelper),
    launcher_sha256: sha256(launcher),
    main_owned_document_api_sha256: sha256(join(root, 'apps/desktop/src/main/ownedDocumentLaunch.ts')),
    rust_source_smoke_sha256: sha256(fileURLToPath(import.meta.url)),
  });

  stage = 'main_owned_launch';
  registration = await runner.launch('Owned Rust Source observation\n只读临时文档\n');
  assert.deepEqual(Object.keys(registration), [], 'Main registration must remain opaque');
  stage = 'main_activation';
  assert.equal(await activateOwnedDocumentForObservation(registration), true, 'Main activation did not succeed');
  stage = 'private_bootstrap';
  const prefix = encodeOwnedDocumentSourceBootstrap(registration);
  let claims: OwnedDocumentSourceBootstrap;
  try {
    claims = JSON.parse(prefix.subarray(4).toString('utf8')) as OwnedDocumentSourceBootstrap;
  } catch {
    prefix.fill(0);
    throw new Error('main_private_bootstrap_invalid');
  }
  targetPid = claims.pid;
  assert.ok(!textEditPidsBefore.includes(targetPid), 'owned target must be a new TextEdit process');

  stage = 'rust_source_spawn';
  sourceRunFactory = new EnvelopeFactory('supervisor', () => Date.now() - clockOriginMs);
  const settings = {
    socket_dir: sourceRunDir,
    host_token: randomBytes(32).toString('hex'),
    clock_origin_ms: String(clockOriginMs),
    world_log: join(sourceRunDir, 'world.log'),
    source_backend: 'native',
    native_capture_mode: 'watch',
    native_helper_path: nativeHelper,
    target_bundle_id: claims.bundle_id,
    target_pid: String(claims.pid),
    native_assets_dir: assetsDir,
    native_timeout_ms: '5000',
    interval_ms: '250',
    clock_domain: clockDomain,
  };
  const configureFrame = Buffer.from(encodeFrame(sourceRunFactory.make({
    type: 'configure', payload: { role: 'source', session_epoch: 1, settings },
  })));
  source = spawn(sourceBinary, ['--owned-document-bootstrap'], {
    cwd: root,
    env: { PATH: '/usr/bin:/bin:/usr/sbin:/sbin', TMPDIR: tempBase, LANG: 'en_US.UTF-8' },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  sourceExit = new Promise((resolveExit) => {
    source!.once('close', (code, signal) => {
      sourceClosed = true;
      resolveExit({ code, signal });
    });
  });
  source.stdout!.on('data', (bytes: Buffer) => sourceQueue.push(bytes));
  source.stderr!.on('data', () => { /* Drain bounded process diagnostics without retaining private text. */ });
  source.once('error', () => sourceQueue.fail(new Error('rust_source_start_failed')));
  source.once('close', () => {
    if (observed.length < 3) sourceQueue.fail(new Error('rust_source_closed_early'));
  });
  const privatePipe = Buffer.concat([prefix, configureFrame]);
  prefix.fill(0);
  try {
    await writeBuffer(source.stdin!, privatePipe);
  } finally {
    privatePipe.fill(0);
    configureFrame.fill(0);
  }
  evidence.source_configured_over_desktop_ipc = true;
  const sourceReady = await sourceQueue.next(15_000);
  assert.equal(sourceReady.message.type, 'ready', 'Rust Source did not send Desktop Ready');
  assert.equal(sourceReady.message.payload.role, 'source');

  stage = 'authenticated_host_socket';
  const hostToken = settings.host_token;
  const socketPath = join(sourceRunDir, 'source.sock');
  sourceSocket = connectUnix({ path: socketPath });
  sourceSocket.on('data', (bytes: Buffer) => hostQueue.push(bytes));
  sourceSocket.on('error', () => hostQueue.fail(new Error('host_socket_failed')));
  sourceSocket.on('close', () => hostQueue.fail(new Error('source_host_socket_closed')));
  await new Promise<void>((resolveConnect, rejectConnect) => {
    const timer = setTimeout(() => rejectConnect(new Error('host_socket_connect_timeout')), 5_000);
    sourceSocket!.once('connect', () => { clearTimeout(timer); resolveConnect(); });
    sourceSocket!.once('error', () => { clearTimeout(timer); rejectConnect(new Error('host_socket_connect_failed')); });
  });
  const hostFactory = new EnvelopeFactory('host', () => Date.now() - clockOriginMs);
  await writeBuffer(sourceSocket, Buffer.from(encodeFrame(hostFactory.make({
    type: 'hello',
    payload: {
      protocol_min: PROTOCOL_VERSION, protocol_max: PROTOCOL_VERSION, role: 'host', build_revision: '0.1.0',
      process: { pid: process.pid, start_time_ms: clockOriginMs },
      session_epoch: 1, worker_epoch: 1, token: hostToken,
    },
  }))));
  const helloAck = await hostQueue.next(5_000);
  assert.equal(helloAck.message.type, 'hello_ack');
  assert.equal(helloAck.message.payload.role, 'host');
  assert.equal(helloAck.message.payload.session_epoch, 1);
  const hostReady = await hostQueue.next(5_000);
  assert.equal(hostReady.message.type, 'ready');
  assert.equal(hostReady.message.payload.role, 'source');
  evidence.host_socket_authenticated = true;

  stage = 'collect_owned_snapshots';
  const collectionDeadline = Date.now() + 20_000;
  while (observed.length < 3 && Date.now() < collectionDeadline) {
    const envelope = await hostQueue.next(Math.max(1, collectionDeadline - Date.now()));
    if (envelope.message.type === 'source_health') {
      health.push(envelope.message.payload);
      continue;
    }
    if (envelope.message.type === 'structure_snapshot') {
      structureRecordsSeen += 1;
      if (!envelope.message.payload.owned_document_evidence) {
        structuresWithoutOwnedSidecar += 1;
        observed.length = 0;
        continue;
      }
      observed.push(envelope.message.payload);
      continue;
    }
    if (envelope.message.type === 'shutdown') throw new Error('source_shutdown_before_three_snapshots');
  }
  assert.equal(observed.length, 3, 'expected three native owned-document snapshots');

  const sourceHealth = (snapshot: StructureSnapshot): SourceHealth | undefined => health
    .filter((entry) => entry.source === 'ax'
      && entry.provenance?.source_sequence === snapshot.source_sequence)
    .at(-1);
  const privateFacts = observed.map((snapshot) => {
    const proof = snapshot.owned_document_evidence;
    assert.ok(proof, 'Rust Source did not publish the typed private sidecar');
    assert.equal(snapshot.complete, false, 'private proof must not upgrade generic structure completeness');
    assert.equal(snapshot.identity_boundary ?? null, null, 'private identity must remain separate from generic identity');
    assert.equal(snapshot.clock_domain, clockDomain);
    assert.ok(snapshot.incomplete_reason, 'generic incompleteness reason must remain visible');
    const axHealth = sourceHealth(snapshot);
    assert.ok(axHealth, 'missing per-record generic AX health');
    assert.equal(axHealth.state, 'degraded', 'private proof must not upgrade generic AX health');
    assert.equal(proof.registration_id, claims.registration_id);
    assert.equal(proof.process.pid, claims.pid);
    assert.equal(proof.scope_complete, true);
    assert.equal(proof.profile_eligible, true);
    assert.equal(proof.document_binding_verified, true);
    assert.equal(proof.observer_state, 'continuous');
    assert.equal(proof.secure, false);
    assert.equal(proof.provenance.snapshot_id, snapshot.snapshot_id);
    assert.equal(proof.provenance.source_sequence, snapshot.source_sequence);
    assert.equal(proof.provenance.clock_domain, snapshot.clock_domain, 'raw native clock was not mapped to Source clock');
    const nowMs = Date.now() - clockOriginMs;
    assert.ok(proof.provenance.observed_ms <= nowMs);
    assert.ok(nowMs < proof.provenance.valid_until_ms, 'owned proof expired before the read was checked');
    assert.ok(snapshot.observed_ms <= nowMs && snapshot.observed_ms < proof.provenance.valid_until_ms);
    const readTimes = [
      proof.document_binding.before.observed_ms,
      proof.document_binding.after.observed_ms,
      proof.marked_range_before.observed_ms,
      proof.marked_range_after.observed_ms,
    ];
    assert.ok(readTimes.every((time) => time >= snapshot.observed_ms && time <= nowMs
      && time >= proof.provenance.observed_ms && time < proof.provenance.valid_until_ms),
    'owned read timestamps were not mapped or fell outside the live source window');
    assert.ok(proof.document_binding.before.observed_ms <= proof.document_binding.after.observed_ms);
    assert.ok(proof.marked_range_before.observed_ms <= proof.marked_range_after.observed_ms);
    assert.equal(proof.document_binding.before.status, 'value');
    assert.equal(proof.document_binding.after.status, 'value');
    assert.equal(proof.document_binding.before.api_code, 0);
    assert.equal(proof.document_binding.after.api_code, 0);
    assert.equal(proof.document_binding.before.registered_path_matches, true);
    assert.equal(proof.document_binding.after.registered_path_matches, true);
    assert.equal(proof.marked_range_before.receiver_object_ref, proof.actual_object_ref);
    assert.equal(proof.marked_range_after.receiver_object_ref, proof.actual_object_ref);
    assert.equal(proof.marked_range_before.receiver_same_cf_as_actual_object_ref, true);
    assert.equal(proof.marked_range_after.receiver_same_cf_as_actual_object_ref, true);
    return {
      sequence: snapshot.source_sequence,
      snapshot_id_sha256: createHash('sha256').update(snapshot.snapshot_id).digest('hex'),
      generic_complete: snapshot.complete,
      generic_ax_health: axHealth.state,
      clock_domain: snapshot.clock_domain,
      snapshot_observed_ms: snapshot.observed_ms,
      private_observed_ms: proof.provenance.observed_ms,
      private_valid_until_ms: proof.provenance.valid_until_ms,
      expiry_remaining_ms: proof.provenance.valid_until_ms - nowMs,
      document_binding_verified: proof.document_binding_verified,
      marked_range_stable_unmarked: proof.marked_range_stable_unmarked,
      private_boundary_generation: proof.provenance.identity_boundary.generation,
    };
  });
  const firstProof = observed[0]!.owned_document_evidence!;
  assert.deepEqual(observed.map((snapshot) => snapshot.source_sequence), [
    observed[0]!.source_sequence,
    observed[0]!.source_sequence + 1,
    observed[0]!.source_sequence + 2,
  ], 'Source did not publish a consecutive snapshot sequence');
  assert.ok(observed.every((snapshot) => {
    const proof = snapshot.owned_document_evidence!;
    return proof.provenance.identity_boundary.namespace === firstProof.provenance.identity_boundary.namespace
      && proof.provenance.identity_boundary.generation === firstProof.provenance.identity_boundary.generation
      && proof.actual_object_ref === firstProof.actual_object_ref
      && proof.window_object_ref === firstProof.window_object_ref
      && proof.process.pid === firstProof.process.pid
      && proof.process.start_seconds === firstProof.process.start_seconds
      && proof.process.start_micros === firstProof.process.start_micros;
  }), 'private receiver identity changed during the resident watch');
  evidence.snapshots = privateFacts;
  evidence.snapshot_count = observed.length;
  evidence.source_structure_records_seen = structureRecordsSeen;
  evidence.structures_without_owned_sidecar = structuresWithoutOwnedSidecar;
  evidence.generic_completeness_preserved = true;
  evidence.generic_health_preserved = true;
  evidence.private_clock_mapping_verified = true;
  evidence.private_identity_stable = true;

  stage = 'stop_rust_source';
  const shutdownFrame = Buffer.from(encodeFrame(sourceRunFactory.make({
    type: 'shutdown', payload: { reason: 'owned-document observation smoke complete' },
  })));
  await writeBuffer(source.stdin!, shutdownFrame);
  shutdownFrame.fill(0);
  source.stdin!.end();
  const exit = await sourceExit!;
  assert.equal(exit.code, 0, 'Rust Source did not stop cleanly');
  assert.equal(exit.signal, null);
  sourceClosed = true;
  evidence.source_stopped = true;

  stage = 'owned_text_edit_cleanup';
  const stopResult = await registration.stop();
  assert.equal(stopResult, 'stopped');
  evidence.owned_document_cleanup = 'stopped';
  registration = null;
  const remainingRegistrations = await runner.stopAll();
  assert.deepEqual(remainingRegistrations, []);
  assert.equal(await waitForPidExit(claims.pid, 5_000), true, 'owned TextEdit process remained after stop');
  assert.deepEqual(textEditPids().filter((pid) => textEditPidsBefore.includes(pid)), textEditPidsBefore);
  assert.deepEqual(ownedDirectories(), ownedDirectoriesBefore, 'private temporary document directory remained');
  evidence.owned_text_edit_cleaned = true;
  evidence.private_document_directory_removed = true;
  evidence.preexisting_text_edit_processes_preserved = true;
  evidence.ok = true;
} catch (error) {
  evidence.failure_stage = stage;
  evidence.failure_kind = error instanceof assert.AssertionError ? 'assertion_failed' : 'smoke_step_failed';
  if (timeoutExpired) evidence.deadline_exceeded = true;
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  if (source && !sourceClosed) {
    try {
      const shutdownFrame = sourceRunFactory
        ? Buffer.from(encodeFrame(sourceRunFactory.make({
          type: 'shutdown', payload: { reason: 'owned-document observation cleanup' },
        })))
        : null;
      if (shutdownFrame && source.stdin?.writable) {
        await writeBuffer(source.stdin, shutdownFrame);
        source.stdin.end();
      }
      shutdownFrame?.fill(0);
      let exit = await waitForProcessExit(source, 5_000);
      if (!exit) {
        source.kill('SIGTERM');
        exit = await waitForProcessExit(source, 2_000);
      }
      if (!exit) {
        source.kill('SIGKILL');
        exit = await waitForProcessExit(source, 2_000);
      }
      sourceClosed = exit !== null;
      evidence.source_cleanup = exit ? 'stopped' : 'unknown';
    } catch {
      source.kill('SIGKILL');
      evidence.source_cleanup = 'unknown';
    }
  }
  sourceSocket?.destroy();
  if (registration) {
    try {
      const result = await registration.stop();
      const remaining = await runner.stopAll();
      evidence.owned_document_cleanup = result === 'stopped' && remaining.length === 0 ? 'stopped' : 'unknown';
      if (result !== 'stopped' || remaining.length !== 0) {
        evidence.ok = false;
        process.exitCode = 1;
      }
      const exited = targetPid === null || await waitForPidExit(targetPid, 5_000);
      if (!exited) {
        evidence.ok = false;
        process.exitCode = 1;
      }
    } catch {
      evidence.owned_document_cleanup = 'unknown';
      evidence.ok = false;
      process.exitCode = 1;
    }
  } else {
    try { await runner.stopAll(); } catch { evidence.owned_document_cleanup = 'unknown'; }
  }
  try { rmSync(sourceRunDir, { recursive: true, force: true }); } catch { evidence.source_workdir_removed = false; }
  evidence.source_workdir_removed ??= true;
  if (evidence.source_workdir_removed !== true) {
    evidence.ok = false;
    process.exitCode = 1;
  }
  try { mkdirSync(dirname(artifact), { recursive: true, mode: 0o700 }); } catch { /* Artifact write below reports failure. */ }
  evidence.finished_at = new Date().toISOString();
  try {
    writeFileSync(artifact, `${JSON.stringify(evidence, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  } catch {
    process.exitCode = 1;
    console.log(JSON.stringify({ ok: false, artifact_write_failed: true }));
    throw new Error('unique_artifact_write_failed');
  }
  console.log(JSON.stringify({
    ok: evidence.ok,
    artifact,
    snapshots: observed.length,
    cleanup: evidence.owned_document_cleanup ?? 'unknown',
  }));
}
