import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import {
  chmodSync, closeSync, lstatSync, mkdirSync, mkdtempSync, openSync, readFileSync, readdirSync, rmSync, writeFileSync,
  realpathSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test, { after, before } from 'node:test';

import { createOwnedDocumentLaunchRunnerForTesting, type OwnedDocumentLaunchRunner } from '../src/main/ownedDocumentLaunch.js';
import { Supervisor } from '../src/main/supervisor.js';
import type { OwnedRevisitControl } from '../src/main/ownedRevisitControl.js';
import { NativeSession } from '../src/main/nativeSession.js';
import type { SupervisorOptions, SupervisorEvents } from '../src/main/supervisor.js';
import type { UiProjection } from '@computer-use/desktop-api';
import { authorizeOwnedDocumentValueInput, authorizeOwnedDocumentSave, consumeOwnedDocumentSaveAuthorization,
  type OwnedDocumentValueAuthorization, type OwnedDocumentSaveAuthorization } from '../src/main/ownedDocumentInput.js';
import { nativeUnavailableDomains } from './capabilityFixtures.js';

const TEXTEDIT_EXEC = '/System/Applications/TextEdit.app/Contents/MacOS/TextEdit';
const OWNED_LAUNCHER_NAME = 'cu-macos-owned-document-launcher';
const PROJECT_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const OWNED_LAUNCHER = join(PROJECT_ROOT, 'target', 'debug', OWNED_LAUNCHER_NAME);
const CAPTURE_ENV = 'CU_SUPERVISOR_OWNED_TEST_CAPTURE';
let createdLauncherIdentity: { dev: number; ino: number } | null = null;

before(() => {
  mkdirSync(dirname(OWNED_LAUNCHER), { recursive: true });
  let created = false;
  try {
    const fd = openSync(OWNED_LAUNCHER, 'wx', 0o700);
    try { writeFileSync(fd, '#!/bin/sh\nexit 0\n'); }
    finally { closeSync(fd); }
    chmodSync(OWNED_LAUNCHER, 0o700);
    created = true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
  }
  const stat = lstatSync(OWNED_LAUNCHER);
  assert.equal(stat.isFile(), true);
  assert.equal(stat.isSymbolicLink(), false);
  assert.notEqual(stat.mode & 0o111, 0);
  if (created) createdLauncherIdentity = { dev: stat.dev, ino: stat.ino };
});

after(() => {
  if (!createdLauncherIdentity) return;
  try {
    const stat = lstatSync(OWNED_LAUNCHER);
    if (stat.isFile() && !stat.isSymbolicLink() && stat.dev === createdLauncherIdentity.dev && stat.ino === createdLauncherIdentity.ino) {
      rmSync(OWNED_LAUNCHER);
    }
  } catch { /* A concurrent build or cleanup owns a changed path. */ }
});

class LaunchFixtureChild extends EventEmitter {
  readonly pid = 610_001;
  readonly stdout = new EventEmitter();
  readonly stderr = new EventEmitter();
  readonly stdin = {
    write: (line: string, callback?: (error?: Error | null) => void): boolean => {
      this.handle(JSON.parse(line) as Record<string, unknown>);
      setImmediate(() => callback?.(null));
      return true;
    },
    end: (): void => { setImmediate(() => this.close()); },
  };
  readonly launcherID = '10101010-2020-4020-8020-101010101010';
  readonly launcherIdentity: Record<string, unknown>;
  readonly targetPID = 610_002;
  readonly targetExecutable = TEXTEDIT_EXEC;
  readonly startSeconds = Math.floor(Date.now() / 1000);
  readonly startMicros = (Date.now() % 1000) * 1000;
  closed = false;
  runID = '';
  documentPath = '';

  constructor(helperPath: string) {
    super();
    this.launcherIdentity = {
      pid: this.pid, parent_pid: process.pid, start_seconds: this.startSeconds,
      start_microseconds: this.startMicros, executable_path: helperPath, parent_executable_path: process.execPath,
    };
    setImmediate(() => this.emitLine({ kind: 'ready', launcher_id: this.launcherID, launcher_identity: this.launcherIdentity }));
  }

  kill(): boolean { setImmediate(() => this.close()); return true; }

  private handle(command: Record<string, unknown>): void {
    if (command.op === 'launch') {
      this.runID = String(command.run_id);
      this.documentPath = String(command.document_path);
      const now = Date.now();
      const startSeconds = Math.floor(now / 1000);
      const startMicros = (now % 1000) * 1000;
      const receipt = {
        version: 1, run_id: this.runID, launcher_id: this.launcherID,
        launcher_identity: this.launcherIdentity,
        target_identity: {
          pid: this.targetPID, parent_pid: 1, start_seconds: startSeconds,
          start_microseconds: startMicros, executable_path: this.targetExecutable,
          parent_executable_path: '/sbin/launchd',
        },
        bundle_identifier: 'com.apple.TextEdit', application_path: '/System/Applications/TextEdit.app',
        executable_path: TEXTEDIT_EXEC, document_path: this.documentPath,
        document_sha256: String(command.document_sha256), preexisting_textedit_pids: [],
        preexisting_text_edit_applications: [], launch_start_unix_milliseconds: now - 1,
        launch_finish_unix_milliseconds: now, new_pid_excluded_from_preexisting_set: true,
        target_started_within_launch_interval: true, creates_new_application_instance: true,
        adds_to_recent_items: false, activates_new_instance: false,
        arguments: ['-ApplePersistenceIgnoreState', 'YES'], target_parent_is_launchd: true, no_input_authority: true,
      };
      this.emitLine({ kind: 'launch_receipt', launcher_id: this.launcherID, receipt });
      return;
    }
    if (command.op === 'stop') {
      this.emitLine({ kind: 'stop_result', launcher_id: this.launcherID, run_id: this.runID,
        status: 'terminated', target_pid: this.targetPID });
      return;
    }
    if (command.op === 'activate') {
      this.emitLine({ kind: 'activation_result', launcher_id: this.launcherID, run_id: this.runID,
        status: 'activated', target_pid: this.targetPID });
      return;
    }
    if (command.op === 'quit') this.emitLine({ kind: 'quit_result', launcher_id: this.launcherID, status: 'terminated' });
  }

  private emitLine(value: unknown): void { this.stdout.emit('data', Buffer.from(`${JSON.stringify(value)}\n`)); }
  private close(): void { if (this.closed) return; this.closed = true; this.emit('close', 0, null); }
}

function ownedRegistration(t: { after(callback: () => void): void }): Promise<{ registration: Awaited<ReturnType<OwnedDocumentLaunchRunner['launch']>>; child: LaunchFixtureChild }> {
  const base = mkdtempSync(join(tmpdir(), 'cu-supervisor-owned-document-'));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  let child: LaunchFixtureChild | null = null;
  const runner = createOwnedDocumentLaunchRunnerForTesting((helperPath) => {
    child = new LaunchFixtureChild(helperPath);
    return child;
  }, base);
  return runner.launch('owned source fixture\n').then((registration) => {
    assert.ok(child);
    return { registration, child };
  });
}

const FAKE_NATIVE_PEER = String.raw`#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
const prefixKinds = args.filter(arg => arg !== '--owned-revisit-control')
  .map(arg => arg === '--owned-document-bootstrap' ? 'bootstrap' : 'model_bootstrap');
const captureDir = process.env.CU_SUPERVISOR_OWNED_TEST_CAPTURE;
const events = [];
const sourceReadyDelayMs = 0;
if (captureDir) fs.writeFileSync(path.join(captureDir, 'started-' + process.pid + '.json'), JSON.stringify({ args }));
let bytes = Buffer.alloc(0);
let prefixesRead = 0;
function writeFrame(value) {
  const body = Buffer.from(JSON.stringify(value));
  const header = Buffer.alloc(4); header.writeUInt32BE(body.length);
  const frame = Buffer.concat([header, body]);
  if (value.message.type === 'ready' && value.message.payload.role === 'source' && sourceReadyDelayMs) {
    setTimeout(() => process.stdout.write(frame), sourceReadyDelayMs);
  } else process.stdout.write(frame);
}
function attachPrivateControl(payload) {
  if (!args.includes('--owned-revisit-control')) return;
  const socket = new (require('node:net').Socket)({ fd: 3, readable: true, writable: true });
  let incoming = Buffer.alloc(0);
  const reply = value => {
    const body = Buffer.from(JSON.stringify(value)); const header = Buffer.alloc(4);
    header.writeUInt32BE(body.length); socket.write(Buffer.concat([header, body]));
  };
  socket.on('data', chunk => {
    incoming = Buffer.concat([incoming, chunk]);
    while (incoming.length >= 4 && incoming.length >= incoming.readUInt32BE() + 4) {
      const size = incoming.readUInt32BE(); const message = JSON.parse(incoming.subarray(4, 4 + size).toString('utf8'));
      incoming = incoming.subarray(4 + size);
      if (message.type === 'prepare') reply({ version: 1, type: 'prepared', request_id: message.request_id, challenge: {
        version: 1, transition_id: '123e4567-e89b-42d3-a456-426614174001', root_task_id: 'root-1', task_revision: 1, root_epoch: 1,
        old_registration_id: events[0].record.registration_id, save_attempt_id: 'attempt-2', save_intent_id: 'intent-2',
        save_span: { from_ms: 1, to_ms: 2 }, expected_sha256: events[0].record.document_sha256,
        expected_byte_length: Buffer.byteLength('owned source fixture\n'), old_device_session_epoch: 1,
        expected_device_session_epoch: 2, old_source_namespace: 'source-1', clock_domain: 'macos/1/wall',
        deadline_ms: Date.now() - Number(payload.settings.clock_origin_ms) + 30000,
      } });
      else if (message.type === 'finish') {
        if (captureDir) fs.writeFileSync(path.join(captureDir, 'private-finish.json'), JSON.stringify(message));
        reply({ version: 1, type: 'finished', request_id: message.request_id, transition_id: message.completion.transition_id });
      } else if (message.type === 'abort') {
        if (captureDir) fs.writeFileSync(path.join(captureDir, 'private-abort.json'), '{}');
        reply({ version: 1, type: 'aborted', request_id: message.request_id, transition_id: message.transition_id });
      }
    }
  });
}
function consume() {
  while (bytes.length >= 4) {
    const size = bytes.readUInt32BE(0);
    if (size === 0 || size > (1 << 20)) process.exit(90);
    if (bytes.length < size + 4) return;
    const body = bytes.subarray(4, size + 4); bytes = bytes.subarray(size + 4);
    let value;
    try { value = JSON.parse(body.toString('utf8')); } catch { process.exit(91); }
    if (prefixesRead < prefixKinds.length) {
      events.push({ kind: prefixKinds[prefixesRead++], record: value, frame_length: size });
      continue;
    }
    if (value.message && value.message.type === 'configure') {
      const payload = value.message.payload;
      events.push({ kind: 'configure', role: payload.role, settings: payload.settings });
      if (captureDir) fs.writeFileSync(path.join(captureDir, payload.role + '.json'), JSON.stringify({ args, events }));
      attachPrivateControl(payload);
      writeFrame({ protocol_version: 2, message_id: payload.role + '-1', in_reply_to: null,
        producer: payload.role, producer_sequence: 1, emitted_ms: 1, trace_id: 'trace-' + payload.role + '-1',
        message: { type: 'ready', payload: { notes: [], role: payload.role } } });
    } else if (value.message && value.message.type === 'shutdown') process.exit(0);
  }
}
process.stdin.on('data', (chunk) => { bytes = Buffer.concat([bytes, chunk]); consume(); });
process.stdin.on('end', () => process.exit(0));
`;

function fakeBinaries(t: { after(callback: () => void): void }, sourceReadyDelayMs = 0): { binaries: { host: string; device: string; source: string }; helperPath: string; captureDir: string } {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'cu-supervisor-owned-processes-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  const captureDir = join(root, 'capture');
  mkdirSync(bin, { mode: 0o700 }); mkdirSync(captureDir, { mode: 0o700 });
  const paths = {
    host: join(bin, 'cu-host'), device: join(bin, 'cu-device'), source: join(bin, 'cu-source'),
  };
  const helperPath = join(bin, 'cu-macos-source');
  for (const path of [...Object.values(paths), helperPath, join(bin, 'cu-macos-device-guard')]) {
    // Host has a sanitized environment; the fixture's capture destination is
    // part of its executable instead of adding a production environment grant.
    const peer = FAKE_NATIVE_PEER.replace('const captureDir = process.env.CU_SUPERVISOR_OWNED_TEST_CAPTURE;',
      `const captureDir = ${JSON.stringify(captureDir)};`)
      .replace('const sourceReadyDelayMs = 0;', `const sourceReadyDelayMs = ${sourceReadyDelayMs};`);
    writeFileSync(path, peer, { encoding: 'utf8', mode: 0o700 });
    chmodSync(path, 0o700);
  }
  return { binaries: paths, helperPath, captureDir };
}

function withCapture<T>(captureDir: string, action: () => Promise<T>): Promise<T> {
  const previous = process.env[CAPTURE_ENV];
  process.env[CAPTURE_ENV] = captureDir;
  return action().finally(() => {
    if (previous === undefined) delete process.env[CAPTURE_ENV];
    else process.env[CAPTURE_ENV] = previous;
  });
}

for (const delayedDelivery of [false, true]) {
  test(`owned revisit retains Host and starts only observing peers${delayedDelivery ? ' when the committed reply crosses the deadline' : ''}`, async t => {
    const owned = await ownedRegistration(t);
    const fake = fakeBinaries(t);
    let reopened: Awaited<ReturnType<OwnedDocumentLaunchRunner['launch']>> | null = null;
    const supervisor = await Supervisor.start({
      binaries: fake.binaries, readyTimeoutMs: 4_000, ownedDocument: owned.registration,
      ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(owned.registration),
      ownedDocumentSaveAuthorization: authorizeOwnedDocumentSave(owned.registration),
      ownedDocumentReferencePlanner: true, ownedDocumentRootRevisit: true,
      nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
    });
    const internal = supervisor as unknown as { latest: UiProjection; clockOriginMs: number;
      ownedRevisitControl: OwnedRevisitControl; imageTimer: ReturnType<typeof setInterval> | null;
      startNativeImages: (...args: unknown[]) => Promise<void> };
    internal.latest = { root: { root_task_id: 'root-1', task_revision: 1, root_epoch: 1,
      state: 'waiting_evidence', headline: 'task', attempts_used: 2, attempts_max: 4 } } as UiProjection;
    const host = supervisor.hostPid;
    const oldPeers = supervisor.pids;
    const oldHostStartup = readFileSync(join(fake.captureDir, 'host.json'), 'utf8');
    const exits: string[] = []; supervisor.on('exit', role => exits.push(role));
    let hostCommitted = false;
    const finishControl = internal.ownedRevisitControl.finish.bind(internal.ownedRevisitControl);
    internal.ownedRevisitControl.finish = async (...args) => {
      const committed = await finishControl(...args);
      hostCommitted = true;
      if (delayedDelivery) internal.clockOriginMs = Date.now() - 60_000;
      return committed;
    };
    const startImages = internal.startNativeImages.bind(internal);
    internal.startNativeImages = async (...args) => {
      assert.equal(hostCommitted, true, 'optional image connection must follow Host acceptance of the fresh Source');
      return startImages(...args);
    };
    try {
      reopened = await supervisor.revisitOwnedRoot();
      assert.equal(supervisor.hostPid, host);
      assert.notEqual(internal.imageTimer, null, 'new image channel lifecycle must restart');
      assert.notEqual(supervisor.pids.device, oldPeers.device);
      assert.notEqual(supervisor.pids.source, oldPeers.source);
      assert.deepEqual(exits, []);
      assert.equal(readFileSync(join(fake.captureDir, 'host.json'), 'utf8'), oldHostStartup);
      const device = JSON.parse(readFileSync(join(fake.captureDir, 'device.json'), 'utf8'));
      assert.deepEqual(device.args, []);
      assert.deepEqual(device.events.map((event: { kind: string }) => event.kind), ['configure']);
      assert.equal(device.events[0].settings.backend, 'macos-observe');
      assert.equal(device.events[0].settings.owned_save, undefined);
      const source = JSON.parse(readFileSync(join(fake.captureDir, 'source.json'), 'utf8'));
      assert.deepEqual(source.args, ['--owned-document-bootstrap']);
      const finish = JSON.parse(readFileSync(join(fake.captureDir, 'private-finish.json'), 'utf8'));
      assert.equal(finish.completion.new_device_session_epoch, 2);
      assert.equal(finish.completion.root_task_id, 'root-1');
      const { issued_unix_ms: sourceIssued, ...sourceRegistration } = source.events[0].record;
      const { issued_unix_ms: hostIssued, ...hostRegistration } = finish.completion.new_registration;
      assert.deepEqual(hostRegistration, sourceRegistration);
      assert.ok(sourceIssued >= hostIssued);
      assert.deepEqual(finish.completion.file_lineage.before_close, finish.completion.file_lineage.after_close);
      assert.deepEqual(finish.completion.file_lineage.before_close, finish.completion.file_lineage.after_reopen);
      assert.equal(readdirSync(fake.captureDir).includes('private-abort.json'), false);
      await assert.rejects(supervisor.revisitOwnedRoot(), /unavailable/);
    } finally {
      await supervisor.shutdown();
      assert.equal(await (reopened ?? owned.registration).stop(), 'stopped');
    }
  });
}

test('owned source gets exact bootstrap frame before configure and remains observe-only', async (t) => {
  const owned = await ownedRegistration(t);
  const fake = fakeBinaries(t);
  let supervisor: Supervisor | null = null;
  try {
    supervisor = await withCapture(fake.captureDir, () => Supervisor.start({
      binaries: fake.binaries,
      nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
      ownedDocument: owned.registration,
      readyTimeoutMs: 4_000,
    }));
    const source = JSON.parse(readFileSync(join(fake.captureDir, 'source.json'), 'utf8')) as {
      args: string[]; events: Array<{ kind: string; record?: Record<string, unknown>; frame_length?: number; settings?: Record<string, string> }>;
    };
    assert.deepEqual(source.args, ['--owned-document-bootstrap']);
    assert.deepEqual(source.events.map((event) => event.kind), ['bootstrap', 'configure']);
    const bootstrap = source.events[0]!;
    assert.equal(bootstrap.frame_length, Buffer.byteLength(JSON.stringify(bootstrap.record), 'utf8'));
    assert.equal(bootstrap.record?.pid, owned.child.targetPID);
    assert.equal(bootstrap.record?.bundle_id, 'com.apple.TextEdit');
    assert.equal(bootstrap.record?.document_path, owned.child.documentPath);
    assert.equal(bootstrap.record?.registration_id, owned.child.runID);
    const sourceSettings = source.events[1]!.settings!;
    assert.equal(sourceSettings.source_backend, 'native');
    assert.equal(sourceSettings.native_helper_path, fake.helperPath);
    assert.equal(sourceSettings.target_bundle_id, 'com.apple.TextEdit');
    assert.equal(sourceSettings.target_pid, String(owned.child.targetPID));
    assert.equal(sourceSettings.native_text_policy, undefined);
    assert.equal(sourceSettings.native_text_window_id, undefined);
    assert.equal(Object.keys(sourceSettings).some((key) => /bootstrap|document_(sha|path|dev|ino)|registration_id/.test(key)), false);

    const host = JSON.parse(readFileSync(join(fake.captureDir, 'host.json'), 'utf8')) as typeof source;
    assert.deepEqual(host.args, ['--owned-document-bootstrap']);
    assert.deepEqual(host.events.map(event => event.kind), ['bootstrap', 'configure']);
    const hostRegistration = host.events[0]!.record!;
    assert.equal(hostRegistration.registration_id, bootstrap.record?.registration_id);
    assert.equal(hostRegistration.pid, bootstrap.record?.pid);
    assert.equal(hostRegistration.document_path, bootstrap.record?.document_path);
    assert.equal(hostRegistration.document_sha256, bootstrap.record?.document_sha256);
    assert.ok((hostRegistration.issued_unix_ms as number) >= (bootstrap.record?.issued_unix_ms as number));
    assert.equal(host.events[1]!.settings?.initial_mode, 'observe');
    assert.equal(host.events[1]!.settings?.native_target_pid, String(owned.child.targetPID));
    assert.equal(host.events[1]!.settings?.native_target_window_id, undefined);
    assert.equal(Object.keys(host.events[1]!.settings!).some(key => /bootstrap|document_(sha|path|dev|ino)|registration_id/.test(key)), false);

    const device = JSON.parse(readFileSync(join(fake.captureDir, 'device.json'), 'utf8')) as typeof source;
    assert.deepEqual(device.args, ['--owned-document-bootstrap']);
    assert.deepEqual(device.events.map(event => event.kind), ['bootstrap', 'configure']);
    assert.equal(device.events[0]!.record?.registration_id, bootstrap.record?.registration_id);
    assert.equal(device.events[1]!.settings?.backend, 'macos-observe');
    assert.equal(device.events[1]!.settings?.native_target_pid, undefined);
    assert.equal(device.events[1]!.settings?.native_target_window_id, undefined);
    assert.equal(supervisor.projection, null);
    const firstIssueTime = bootstrap.record?.issued_unix_ms as number;
    await supervisor.shutdown();
    supervisor = null;
    await new Promise((done) => setTimeout(done, 3));
    supervisor = await withCapture(fake.captureDir, () => Supervisor.start({
      binaries: fake.binaries,
      nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
      ownedDocument: owned.registration,
      readyTimeoutMs: 4_000,
    }));
    const renewedSource = JSON.parse(readFileSync(join(fake.captureDir, 'source.json'), 'utf8')) as {
      events: Array<{ kind: string; record?: Record<string, unknown> }>;
    };
    assert.ok((renewedSource.events[0]!.record?.issued_unix_ms as number) > firstIssueTime);
  } finally {
    await supervisor?.shutdown();
    await owned.registration.stop();
  }
});

test('Host receives owned registration before purpose-bound model authorization', async t => {
  for (const purposes of [['s1_teacher'], ['s1_teacher', 's2_planning']] as const) {
    const owned = await ownedRegistration(t);
    const fake = fakeBinaries(t);
    let supervisor: Supervisor | null = null;
    try {
      supervisor = await Supervisor.start({
        binaries: fake.binaries, readyTimeoutMs: 4_000, ownedDocument: owned.registration,
        nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
        modelAuthorization: { credential: { kind: 'api_key' as const, api_key: 'private-transport-fixture-key' }, purposes: [...purposes], route: {
          provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://example.invalid/v1',
          model: 'transport-fixture', max_output_tokens: 4096,
        } },
      });
      const host = JSON.parse(readFileSync(join(fake.captureDir, 'host.json'), 'utf8'));
      assert.deepEqual(host.args, ['--owned-document-bootstrap', '--model-bootstrap']);
      assert.deepEqual(host.events.map((event: { kind: string }) => event.kind), ['bootstrap', 'model_bootstrap', 'configure']);
      assert.equal(host.events[0].record.registration_id, owned.child.runID);
      assert.deepEqual([...host.events[1].record.purposes].sort(), [...purposes].sort());
      assert.equal(host.events[1].record.credential.api_key, 'private-transport-fixture-key');
      assert.equal(host.events[2].settings.planning_backend, purposes.length === 1 ? 'unconfigured' : 'model');
      assert.equal(host.events[2].settings.initial_mode, 'observe');
      assert.equal(JSON.stringify(host.events[2]).includes('private-transport-fixture-key'), false);
      const device = JSON.parse(readFileSync(join(fake.captureDir, 'device.json'), 'utf8'));
      assert.deepEqual(device.args, ['--owned-document-bootstrap']);
      assert.equal(device.events[0].record.registration_id, owned.child.runID);
      assert.equal(device.events[1].settings.backend, 'macos-observe');
      assert.equal(device.events[1].settings.native_target_window_id, undefined);
    } finally {
      await supervisor?.shutdown();
      await owned.registration.stop();
    }
  }
});

test('a stopped registration is refused before Host starts after Source startup', async t => {
  const owned = await ownedRegistration(t);
  const fake = fakeBinaries(t, 400);
  const starting = Supervisor.start({
    binaries: fake.binaries, readyTimeoutMs: 4_000, ownedDocument: owned.registration,
    nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
  });
  void starting.catch(() => undefined);
  const sourceCapture = join(fake.captureDir, 'source.json');
  const until = Date.now() + 3_000;
  while (!readdirSync(fake.captureDir).includes('source.json') && Date.now() < until) {
    await new Promise(done => setTimeout(done, 10));
  }
  assert.equal(readFileSync(sourceCapture, 'utf8').includes('configure'), true);
  assert.equal(await owned.registration.stop(), 'stopped');
  await assert.rejects(starting, /no longer eligible for source bootstrap/);
  assert.equal(readdirSync(fake.captureDir).includes('host.json'), false);
});

test('owned source mismatches fail before any child process starts', async (t) => {
  const owned = await ownedRegistration(t);
  const fake = fakeBinaries(t);
  const alternateHelper = join(dirname(fake.helperPath), 'different-helper');
  writeFileSync(alternateHelper, FAKE_NATIVE_PEER, { encoding: 'utf8', mode: 0o700 });
  chmodSync(alternateHelper, 0o700);
  const base = {
    binaries: fake.binaries,
    nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
    ownedDocument: owned.registration,
    readyTimeoutMs: 1_000,
  };
  await withCapture(fake.captureDir, async () => {
    await assert.rejects(Supervisor.start({ ...base, ownedDocument: { pid: owned.child.targetPID } as never }), /invalid owned document registration/);
    await assert.rejects(Supervisor.start({ ...base, mode: 'execute' }), /observation mode/);
    await assert.rejects(Supervisor.start({ ...base, nativeSource: { ...base.nativeSource, pid: owned.child.targetPID + 1 } }), /does not match/);
    await assert.rejects(Supervisor.start({ ...base, nativeSource: { ...base.nativeSource, authorizedWindowId: 'window-1' } }), /observation mode/);
    await assert.rejects(Supervisor.start({ ...base, nativeSource: { ...base.nativeSource, helperPath: alternateHelper } }), /fixed product source helper/);
  });
  assert.deepEqual(readdirSync(fake.captureDir), []);
  await owned.registration.stop();
});

test('owned input requires a separate opaque one-start grant and keeps Host observing', async t => {
  const owned = await ownedRegistration(t);
  const fake = fakeBinaries(t);
  const authorization = authorizeOwnedDocumentValueInput(owned.registration);
  assert.deepEqual(Object.keys(authorization), []);
  assert.equal(JSON.stringify(authorization), '{}');
  const options = {
    binaries: fake.binaries, readyTimeoutMs: 4_000, ownedDocument: owned.registration,
    ownedDocumentValueAuthorization: authorization,
    nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
  };
  const supervisor = await Supervisor.start(options);
  try {
    const device = JSON.parse(readFileSync(join(fake.captureDir, 'device.json'), 'utf8'));
    assert.equal(device.events[1].settings.backend, 'macos-owned-value');
    assert.deepEqual(device.args, ['--owned-document-bootstrap']);
    assert.equal(device.events[1].settings.native_target_pid, undefined);
    assert.equal(device.events[1].settings.native_target_window_id, undefined);
    assert.equal(device.events[1].settings.owned_save, undefined);
    assert.equal(Object.keys(device.events[1].settings).some(key => /document|registration|authorization/.test(key)), false);
    const host = JSON.parse(readFileSync(join(fake.captureDir, 'host.json'), 'utf8'));
    assert.equal(host.events[1].settings.initial_mode, 'observe');
    assert.equal(host.events[1].settings.owned_save, undefined);
    await assert.rejects(Supervisor.start(options), /already consumed/);
  } finally {
    await supervisor.shutdown();
    await owned.registration.stop();
  }
});

test('Save needs a separate opaque grant and only enables owned Host and Device startup', async t => {
  const owned = await ownedRegistration(t);
  const fake = fakeBinaries(t);
  const save = authorizeOwnedDocumentSave(owned.registration);
  assert.deepEqual(Object.keys(save), []);
  assert.equal(JSON.stringify(save), '{}');
  const options = {
    binaries: fake.binaries, readyTimeoutMs: 4_000, ownedDocument: owned.registration,
    ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(owned.registration),
    ownedDocumentSaveAuthorization: save,
    ownedDocumentReferencePlanner: true,
    nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
  };
  const supervisor = await Supervisor.start(options);
  try {
    for (const role of ['device', 'host']) {
      const peer = JSON.parse(readFileSync(join(fake.captureDir, `${role}.json`), 'utf8'));
      assert.equal(peer.events[1].settings.owned_save, '1');
    }
    const source = JSON.parse(readFileSync(join(fake.captureDir, 'source.json'), 'utf8'));
    assert.equal(source.events[1].settings.owned_save, undefined);
    await assert.rejects(Supervisor.start({ ...options,
      ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(owned.registration) }), /save authorization is invalid/);
  } finally { await supervisor.shutdown(); await owned.registration.stop(); }
});

test('forged, mismatched, expired and clock-reversed Save grants cannot start peers', async t => {
  const owned = await ownedRegistration(t);
  const another = await ownedRegistration(t);
  const fake = fakeBinaries(t);
  const options = {
    binaries: fake.binaries, readyTimeoutMs: 1_000, ownedDocument: owned.registration,
    ownedDocumentReferencePlanner: true,
    nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
  };
  for (const save of [{} as OwnedDocumentSaveAuthorization, authorizeOwnedDocumentSave(another.registration)]) {
    await assert.rejects(Supervisor.start({ ...options,
      ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(owned.registration),
      ownedDocumentSaveAuthorization: save }), /save authorization is invalid/);
  }
  for (const shift of [15_001, -1_000]) {
    const save = authorizeOwnedDocumentSave(owned.registration);
    const issued = Date.now();
    t.mock.method(Date, 'now', () => issued + shift);
    assert.throws(() => consumeOwnedDocumentSaveAuthorization(save, owned.registration), /save authorization is invalid/);
    t.mock.restoreAll();
  }
  const saveOnly = authorizeOwnedDocumentSave(owned.registration);
  await assert.rejects(Supervisor.start({ ...options, ownedDocumentSaveAuthorization: saveOnly }), /requires a separate owned input diagnostic authorization/);
  assert.deepEqual(readdirSync(fake.captureDir), []);
  await owned.registration.stop(); await another.registration.stop();
});

test('owned reference diagnostic is explicit and never a credential or input fallback', async t => {
  const owned = await ownedRegistration(t);
  const fake = fakeBinaries(t);
  const options = {
    binaries: fake.binaries, readyTimeoutMs: 4_000, ownedDocument: owned.registration,
    nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
    ownedDocumentReferencePlanner: true,
  };
  await assert.rejects(Supervisor.start(options), /separate owned input diagnostic authorization/);
  assert.deepEqual(readdirSync(fake.captureDir), []);
  const supervisor = await Supervisor.start({ ...options,
    ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(owned.registration) });
  try {
    const host = JSON.parse(readFileSync(join(fake.captureDir, 'host.json'), 'utf8'));
    assert.equal(host.events[1].settings.planning_backend, 'owned_reference');
    assert.equal(host.events[1].settings.initial_mode, 'observe');
    assert.deepEqual(host.args, ['--owned-document-bootstrap']);
  } finally { await supervisor.shutdown(); await owned.registration.stop(); }
});

test('forged, mismatched, revoked or unsafe owned input grants cannot start any peer', async t => {
  const owned = await ownedRegistration(t);
  const another = await ownedRegistration(t);
  const fake = fakeBinaries(t);
  const options = {
    binaries: fake.binaries, readyTimeoutMs: 1_000, ownedDocument: owned.registration,
    nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
  };
  await assert.rejects(Supervisor.start({ ...options, ownedDocumentValueAuthorization: {} as OwnedDocumentValueAuthorization }), /authorization is invalid/);
  await assert.rejects(Supervisor.start({ ...options, ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(another.registration) }), /authorization is invalid/);
  const grant = authorizeOwnedDocumentValueInput(owned.registration);
  const alternate = join(dirname(fake.helperPath), 'alternate-guard');
  writeFileSync(alternate, FAKE_NATIVE_PEER, { mode: 0o700 });
  await assert.rejects(Supervisor.start({ ...options, ownedDocumentValueAuthorization: grant,
    nativeSource: { ...options.nativeSource, guardHelperPath: alternate } }), /fixed product device guard/);
  const stopped = authorizeOwnedDocumentValueInput(owned.registration);
  await owned.registration.stop();
  await assert.rejects(Supervisor.start({ ...options, ownedDocumentValueAuthorization: stopped }), /no longer eligible/);
  assert.deepEqual(readdirSync(fake.captureDir), []);
  await another.registration.stop();
});

test('expired and clock-reversed owned input grants fail before startup', async t => {
  const owned = await ownedRegistration(t);
  const fake = fakeBinaries(t);
  const options = {
    binaries: fake.binaries, readyTimeoutMs: 1_000, ownedDocument: owned.registration,
    nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
  };
  for (const shift of [15_001, -1_000]) {
    const grant = authorizeOwnedDocumentValueInput(owned.registration);
    const issued = Date.now();
    t.mock.method(Date, 'now', () => issued + shift);
    await assert.rejects(Supervisor.start({ ...options, ownedDocumentValueAuthorization: grant }), /authorization is invalid/);
    t.mock.restoreAll();
  }
  assert.deepEqual(readdirSync(fake.captureDir), []);
  await owned.registration.stop();
});

test('owned NativeSession refuses generic window consent for private TextEdit input', async t => {
  const owned = await ownedRegistration(t);
  const fake = fakeBinaries(t);
  const options: SupervisorOptions = {
    binaries: fake.binaries, ownedDocument: owned.registration,
    ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(owned.registration),
    ownedDocumentSaveAuthorization: authorizeOwnedDocumentSave(owned.registration),
    nativeSource: { helperPath: fake.helperPath, bundleId: 'com.apple.TextEdit', pid: owned.child.targetPID },
  };
  const now = Date.now();
  class Pipeline extends EventEmitter<SupervisorEvents> {
    readonly clockOriginUnixMs = now - 100;
    projection: UiProjection = {
      revision: 1, mode: 'observe', root: null, pending_confirmation: null, timeline: [],
      stop_status: 'not_stopped', cleanup: 'not_needed',
      capabilities: { backend: 'macos', execution_domain: 'foreground_native', domains: nativeUnavailableDomains(), input: 'unavailable', human_input: 'available', hit_test: 'unavailable',
        policy: 'atomic_only', max_uninterruptible_ms: 100,
        identity: { session_epoch: 1, surface: { id: 'desktop', epoch: 1 }, clock_domain: 'macos/1/wall', dependencies: {} } },
      observation: { sources: [], frame: null, target_app: 'com.apple.TextEdit', target_name: null, active_window: null,
        observed_target: {
          snapshot_id: 'owned-snapshot', structure_sequence: 1, session_epoch: 1,
          app: 'com.apple.TextEdit', pid: owned.child.targetPID, window_id: 'window-9',
          window_title: null, observed_ms: 1, valid_until_ms: 1_000, contains_private_fields: false,
        } },
    };
    closed = false;
    operator() {}
    stopAtDevice() {}
    sendDecisionProposal() {}
    image() { return null; }
    async shutdown() { this.closed = true; }
  }
  const configurations: SupervisorOptions[] = [];
  const runs: Pipeline[] = [];
  let confirmationCalls = 0;
  const session = await NativeSession.start(options, async () => {
    confirmationCalls += 1;
    return true;
  }, async config => {
    assert.ok(runs.length === 0 || runs.at(-1)!.closed);
    configurations.push(config);
    const run = new Pipeline(); runs.push(run); return run;
  }, () => now);
  try {
    assert.equal(configurations[0]!.ownedDocumentValueAuthorization, undefined);
    assert.equal(configurations[0]!.ownedDocumentSaveAuthorization, undefined);
    await assert.rejects(
      session.authorizeNativeWindow('owned-snapshot'),
      /separate private-document permission/,
    );
    assert.equal(confirmationCalls, 0, 'generic window consent is never shown for owned private-document input');
    assert.equal(configurations.length, 1, 'rejection does not replace the observing pipeline');
    assert.equal(runs.length, 1);
    assert.equal(configurations[0]!.ownedDocumentValueAuthorization, undefined);
    assert.equal(session.authorization.phase, 'observing');
  } finally { await session.shutdown(); await owned.registration.stop(); }
});
