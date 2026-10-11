import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { chmodSync, closeSync, lstatSync, mkdirSync, mkdtempSync, openSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test, { after, before } from 'node:test';

import type { DecisionProposal, OperatorCommand, UiProjection } from '@computer-use/desktop-api';
import { NativeSession, type OwnedDocumentSessionServices } from '../src/main/nativeSession.js';
import type { SupervisorEvents, SupervisorOptions } from '../src/main/supervisor.js';
import { createOwnedDocumentLaunchRunnerForTesting, type OwnedDocumentLaunchRunner } from '../src/main/ownedDocumentLaunch.js';
import { nativeUnavailableDomains } from './capabilityFixtures.js';

const HELPER_NAME = 'cu-macos-owned-document-launcher';
const HELPER = join(resolve(dirname(fileURLToPath(import.meta.url)), '../../..'), 'target', 'debug', HELPER_NAME);
const TEXTEDIT_EXEC = '/System/Applications/TextEdit.app/Contents/MacOS/TextEdit';
let helperCreated: { dev: number; ino: number } | null = null;

before(() => {
  mkdirSync(dirname(HELPER), { recursive: true });
  try {
    const fd = openSync(HELPER, 'wx', 0o700);
    try { writeFileSync(fd, '#!/bin/sh\nexit 0\n'); } finally { closeSync(fd); }
    chmodSync(HELPER, 0o700);
    const stat = lstatSync(HELPER); helperCreated = { dev: stat.dev, ino: stat.ino };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
  }
});
after(() => {
  if (!helperCreated) return;
  try {
    const stat = lstatSync(HELPER);
    if (stat.isFile() && !stat.isSymbolicLink() && stat.dev === helperCreated.dev && stat.ino === helperCreated.ino) rmSync(HELPER);
  } catch { /* A concurrent build or test owns a changed helper path. */ }
});

class LaunchChild extends EventEmitter {
  readonly pid = 430_000;
  readonly stdout = new EventEmitter();
  readonly stderr = new EventEmitter();
  readonly launcherId = '10101010-2020-4020-8020-101010101010';
  readonly launcherIdentity: Record<string, unknown>;
  readonly stdin = {
    write: (line: string, callback?: (error?: Error | null) => void): boolean => {
      this.command(JSON.parse(line) as Record<string, unknown>);
      setImmediate(() => callback?.(null));
      return true;
    },
    end: (): void => { setImmediate(() => this.close()); },
  };
  targetPid = this.pid + 1;
  targetStartMs = 0;
  runId = '';
  documentPath = '';
  sha = '';
  activationCalls = 0;
  closed = false;
  receiptGate: Promise<void> | null = null;
  activationGate: Promise<void> | null = null;

  constructor(helperPath: string, receiptGate: Promise<void> | null = null) {
    super();
    this.receiptGate = receiptGate;
    this.launcherIdentity = { pid: this.pid, parent_pid: process.pid, start_seconds: Math.floor(Date.now() / 1000),
      start_microseconds: 0, executable_path: helperPath, parent_executable_path: process.execPath };
    setImmediate(() => this.line({ kind: 'ready', launcher_id: this.launcherId, launcher_identity: this.launcherIdentity }));
  }
  kill(): boolean { setImmediate(() => this.close()); return true; }
  private command(value: Record<string, unknown>): void {
    if (value['op'] === 'launch') {
      this.runId = String(value['run_id']); this.documentPath = String(value['document_path']); this.sha = String(value['document_sha256']);
      this.targetStartMs = Date.now();
      const target = { pid: this.targetPid, parent_pid: 1, start_seconds: Math.floor(this.targetStartMs / 1000),
        start_microseconds: (this.targetStartMs % 1000) * 1000, executable_path: TEXTEDIT_EXEC,
        parent_executable_path: '/sbin/launchd' };
      const receipt = { kind: 'launch_receipt', launcher_id: this.launcherId, receipt: {
        version: 1, run_id: this.runId, launcher_id: this.launcherId, launcher_identity: this.launcherIdentity,
        target_identity: target, bundle_identifier: 'com.apple.TextEdit', application_path: '/System/Applications/TextEdit.app',
        executable_path: TEXTEDIT_EXEC, document_path: this.documentPath, document_sha256: this.sha,
        preexisting_textedit_pids: [], preexisting_text_edit_applications: [],
        launch_start_unix_milliseconds: this.targetStartMs - 1, launch_finish_unix_milliseconds: this.targetStartMs,
        new_pid_excluded_from_preexisting_set: true, target_started_within_launch_interval: true,
        creates_new_application_instance: true, adds_to_recent_items: false, activates_new_instance: false,
        arguments: ['-ApplePersistenceIgnoreState', 'YES'], target_parent_is_launchd: true, no_input_authority: true,
      } };
      if (this.receiptGate) void this.receiptGate.then(() => this.line(receipt));
      else this.line(receipt);
    } else if (value['op'] === 'activate') {
      this.activationCalls += 1;
      const response = { kind: 'activation_result', launcher_id: this.launcherId, run_id: this.runId,
        target_pid: this.targetPid, status: 'activated' };
      if (this.activationGate) void this.activationGate.then(() => this.line(response));
      else this.line(response);
    } else if (value['op'] === 'stop') {
      this.line({ kind: 'stop_result', launcher_id: this.launcherId, run_id: this.runId,
        target_pid: this.targetPid, status: 'terminated' });
    } else if (value['op'] === 'quit') {
      this.line({ kind: 'quit_result', launcher_id: this.launcherId, status: 'terminated' });
    }
  }
  private line(value: unknown): void { this.stdout.emit('data', Buffer.from(`${JSON.stringify(value)}\n`)); }
  private close(): void { if (!this.closed) { this.closed = true; this.emit('close', 0, null); } }
}

function ownedRunnerFixture(t: { after(callback: () => void): void }, configureChild?: (child: LaunchChild) => void): { runner: OwnedDocumentLaunchRunner; children: LaunchChild[] } {
  const base = mkdtempSync(join(tmpdir(), 'cu-native-session-owned-'));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const children: LaunchChild[] = [];
  const runner = createOwnedDocumentLaunchRunnerForTesting((helperPath) => {
    const child = new LaunchChild(helperPath); child.targetPid += children.length; configureChild?.(child); children.push(child); return child;
  }, base);
  return { runner, children };
}

function projection(): UiProjection {
  return { revision: 1, mode: 'observe', root: null, pending_confirmation: null, timeline: [], stop_status: 'not_stopped', cleanup: 'not_needed',
    capabilities: { backend: 'macos', execution_domain: 'foreground_native', domains: nativeUnavailableDomains(), input: 'unavailable', human_input: 'available', hit_test: 'unavailable', policy: 'atomic_only',
      max_uninterruptible_ms: 100, identity: { session_epoch: 1, surface: { id: 'desktop', epoch: 1 },
        clock_domain: 'macos/1/wall', dependencies: {} } },
    observation: { sources: [], frame: null, target_app: 'com.apple.TextEdit', target_name: null, active_window: null,
      observed_target: null } };
}

class TestPipeline extends EventEmitter<SupervisorEvents> {
  readonly clockOriginUnixMs = 9_990;
  projection: UiProjection = projection();
  commands: OperatorCommand[] = [];
  stops = 0;
  closed = false;
  revisit: (() => Promise<import('../src/main/ownedDocumentLaunch.js').OwnedDocumentRegistration>) | null = null;
  operator(command: OperatorCommand): void { this.commands.push(command); }
  stopAtDevice(): void { this.stops += 1; }
  sendDecisionProposal(_proposal: DecisionProposal): void {}
  image(): null { return null; }
  async shutdown(): Promise<void> { this.closed = true; }
  async revisitOwnedRoot() {
    if (!this.revisit) throw new Error('missing fixture revisit');
    return this.revisit();
  }
  update(value: UiProjection): void { this.projection = value; this.emit('projection', value); }
}

const baseOptions: SupervisorOptions = { binaries: { host: '/host', source: '/bin/cu-source', device: '/device' }, mode: 'observe' };
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

async function setup(t: { after(callback: () => void): void }, config: {
  canPlan?: () => boolean;
  confirmEdit?: (signal: AbortSignal) => Promise<boolean>;
  confirmSave?: (signal: AbortSignal) => Promise<boolean>;
  configureChild?: (child: LaunchChild) => void;
  start?: (options: SupervisorOptions, index: number) => Promise<TestPipeline>;
} = {}) {
  const fixture = ownedRunnerFixture(t, config.configureChild);
  const runs: TestPipeline[] = [];
  const configurations: SupervisorOptions[] = [];
  const services: OwnedDocumentSessionServices = {
    createRunner: () => fixture.runner,
    confirmEdit: config.confirmEdit ?? (async () => true),
    confirmSave: config.confirmSave ?? (async () => true),
    canPlan: config.canPlan ?? (() => true),
  };
  const session = await NativeSession.start(baseOptions, async () => false, async (options) => {
    configurations.push(options);
    const index = configurations.length - 1;
    const pipeline = config.start ? await config.start(options, index) : new TestPipeline();
    if (runs.length) assert.equal(runs.at(-1)!.closed, true, 'old pipeline must close before replacement');
    runs.push(pipeline);
    return pipeline;
  }, () => 10_000, services);
  return { session, fixture, runs, configurations, services };
}

const saveNote = 'the owned Save input completed; independent saved-file and root revisit evidence is still required';
function rootProjection(rootId: string, atMs: number, note?: boolean): UiProjection {
  const value = projection();
  value.root = { root_task_id: rootId, task_revision: 1, root_epoch: 1, state: 'running', headline: 'owned text', attempts_used: 1, attempts_max: 1 };
  value.timeline = note ? [{ at_ms: atMs, text: saveNote }] : [];
  return value;
}

test('owned creation starts an empty private document in observation mode without a reference planner', async (t) => {
  const f = await setup(t, { canPlan: () => false });
  assert.equal(f.session.ownedDocumentStatus.phase, 'not_created');
  assert.equal(f.session.ownedDocumentStatus.can_create, true);
  await f.session.createOwnedDocument();
  assert.equal(f.session.ownedDocumentStatus.phase, 'observing');
  const started = f.configurations.at(-1)!;
  assert.equal(started.mode, 'observe');
  assert.equal(started.nativeSource?.bundleId, 'com.apple.TextEdit');
  assert.equal(started.nativeSource?.pid, f.fixture.children[0]!.targetPid);
  assert.equal(started.nativeSource?.authorizedWindowId, undefined);
  assert.equal(started.nativeSource?.referencePlanner, undefined);
  assert.equal(started.ownedDocumentValueAuthorization, undefined);
  assert.equal(started.ownedDocumentSaveAuthorization, undefined);
  assert.equal(started.ownedDocumentRootRevisit, undefined);
  assert.equal(started.ownedDocumentReferencePlanner, undefined);
  await f.session.shutdown();
});

test('separate edit and Save approvals gate task creation and execution; execution activates the owned TextEdit instance', async (t) => {
  let allowPlanning = false;
  const f = await setup(t, { canPlan: () => allowPlanning });
  await f.session.createOwnedDocument();
  await assert.rejects(f.session.authorizeOwnedDocumentSave(), /S2 planning model/);
  assert.throws(() => f.session.operator({ command: 'create_task', text: 'fill the document' }), /approved editing/);
  await f.session.authorizeOwnedDocumentEdit();
  const edit = f.configurations.at(-1)!;
  assert.ok(edit.ownedDocumentValueAuthorization);
  assert.equal(edit.ownedDocumentSaveAuthorization, undefined);
  assert.equal(edit.mode, 'observe');
  assert.throws(() => f.session.operator({ command: 'set_mode', mode: 'execute' }), /approved editing/);
  allowPlanning = true;
  await f.session.authorizeOwnedDocumentSave();
  const save = f.configurations.at(-1)!;
  assert.ok(save.ownedDocumentValueAuthorization);
  assert.ok(save.ownedDocumentSaveAuthorization);
  assert.equal(save.ownedDocumentRootRevisit, true);
  assert.equal(save.ownedDocumentReferencePlanner, undefined);
  assert.equal(save.nativeSource?.referencePlanner, undefined);
  assert.equal(save.nativeSource?.authorizedWindowId, undefined);
  assert.equal(save.mode, 'observe');
  const active = f.runs.at(-1)!;
  await f.session.operator({ command: 'set_mode', mode: 'execute' });
  assert.equal(f.fixture.children[0]!.activationCalls, 1);
  assert.deepEqual(active.commands.at(-1), { command: 'set_mode', mode: 'execute' });
  await f.session.operator({ command: 'create_task', text: 'fill the document' });
  assert.equal(active.commands.at(-1)?.command, 'create_task');
  await f.session.shutdown();
});

test('stop aborts a pending native edit confirmation and a late approval cannot start input', async (t) => {
  let resolveApproval!: (approved: boolean) => void;
  const signals: AbortSignal[] = [];
  const f = await setup(t, { confirmEdit: (value) => { signals.push(value); return new Promise((resolve) => { resolveApproval = resolve; }); } });
  await f.session.createOwnedDocument();
  const pending = f.session.authorizeOwnedDocumentEdit();
  f.session.stopAtDevice({ scope: 'all' }, 'cancel', 'operator_button');
  assert.equal(signals[0]?.aborted, true);
  resolveApproval(true);
  await assert.rejects(pending, /could not be authorized safely|cancelled/);
  assert.equal(f.configurations.length, 2, 'a late dialog result cannot start an input-configured pipeline');
  assert.equal(f.session.ownedDocumentStatus.phase, 'observing');
  await f.session.shutdown();
});

test('stop fences edit or Save approval while its replacement pipeline is still starting', async (t) => {
  for (const action of ['edit', 'save'] as const) {
    const deferredStart = deferred<TestPipeline>();
    const deferredIndex = action === 'edit' ? 2 : 3;
    const f = await setup(t, { start: async (_options, index) => index === deferredIndex ? deferredStart.promise : new TestPipeline() });
    await f.session.createOwnedDocument();
    if (action === 'save') await f.session.authorizeOwnedDocumentEdit();
    const pending = action === 'edit' ? f.session.authorizeOwnedDocumentEdit() : f.session.authorizeOwnedDocumentSave();
    for (let tries = 0; tries < 100 && f.configurations.length <= deferredIndex; tries += 1) {
      await new Promise<void>((resolve) => setTimeout(resolve, 2));
    }
    assert.equal(f.configurations.length, deferredIndex + 1);
    f.session.operator({ command: 'stop_task', root_task_id: null });
    const latePipeline = new TestPipeline();
    deferredStart.resolve(latePipeline);
    await assert.rejects(pending, /safely|cancelled|changed/);
    assert.equal(latePipeline.closed, true, 'a pipeline from an obsolete approval generation must be closed');
    assert.equal(f.session.ownedDocumentStatus.can_revisit, false);
    assert.notEqual(f.session.ownedDocumentStatus.phase, 'save_authorized');
    await f.session.shutdown();
  }
});

test('stop and model change fence a pending TextEdit activation before Execute is forwarded', async (t) => {
  for (const invalidation of ['stop', 'model-change'] as const) {
    const gate = deferred<void>();
    const f = await setup(t, { configureChild: (child) => { child.activationGate = gate.promise; } });
    await f.session.createOwnedDocument();
    await f.session.authorizeOwnedDocumentEdit();
    await f.session.authorizeOwnedDocumentSave();
    const oldPipeline = f.runs.at(-1)!;
    const executing = f.session.operator({ command: 'set_mode', mode: 'execute' });
    for (let tries = 0; tries < 100 && f.fixture.children[0]!.activationCalls === 0; tries += 1) {
      await new Promise<void>((resolve) => setTimeout(resolve, 2));
    }
    assert.equal(f.fixture.children[0]!.activationCalls, 1);
    const restarting = invalidation === 'model-change' ? f.session.restartForModelChange() : null;
    if (invalidation === 'stop') f.session.operator({ command: 'stop_task', root_task_id: null });
    if (restarting) await restarting;
    gate.resolve();
    assert.ok(executing instanceof Promise, 'owned Execute waits for the native activation fence');
    await assert.rejects(Promise.resolve(executing), /safely|changed/);
    assert.equal(oldPipeline.commands.some((item) => item.command === 'set_mode' && item.mode === 'execute'), false);
    assert.equal(f.runs.some((run) => run.commands.some((item) => item.command === 'set_mode' && item.mode === 'execute')), false);
    if (invalidation === 'model-change') {
      assert.equal(f.configurations.at(-1)?.ownedDocumentValueAuthorization, undefined);
      assert.equal(f.configurations.at(-1)?.ownedDocumentSaveAuthorization, undefined);
      assert.equal(f.session.ownedDocumentStatus.phase, 'observing');
    }
    await f.session.shutdown();
  }
});

test('unknown cleanup remains sticky across owned permission revocation', async (t) => {
  const f = await setup(t);
  await f.session.createOwnedDocument();
  await f.session.authorizeOwnedDocumentEdit();
  f.runs.at(-1)!.projection.cleanup = 'unknown';
  assert.equal(f.session.canConfigureModel, false);
  assert.equal(f.session.ownedDocumentStatus.can_authorize_save, false);
  await assert.rejects(f.session.authorizeOwnedDocumentSave(), /S2 planning model|unavailable/);
  await f.session.revokeNativeWindow();
  assert.equal(f.session.projection?.cleanup, 'unknown');
  assert.equal(f.session.ownedDocumentStatus.can_authorize_edit, false);
  assert.equal(f.configurations.at(-1)?.ownedDocumentValueAuthorization, undefined);
  assert.equal(f.configurations.at(-1)?.ownedDocumentSaveAuthorization, undefined);
  await f.session.shutdown();
});

test('a prior Root Save note cannot ready a different root, but a fresh note can', async (t) => {
  const f = await setup(t);
  await f.session.createOwnedDocument();
  await f.session.authorizeOwnedDocumentEdit();
  await f.session.authorizeOwnedDocumentSave();
  const pipeline = f.runs.at(-1)!;
  pipeline.update(rootProjection('root-1', 0, false));
  pipeline.update(rootProjection('root-1', 20, true));
  assert.equal(f.session.ownedDocumentStatus.can_revisit, true);
  f.session.stopAtDevice({ scope: 'root', root_id: 'root-1' }, 'cancel', 'operator_button');
  pipeline.update(rootProjection('root-2', 20, true));
  assert.equal(f.session.ownedDocumentStatus.can_revisit, false, 'timeline note from the old root is below the new root high-water mark');
  pipeline.update(rootProjection('root-2', 30, true));
  assert.equal(f.session.ownedDocumentStatus.can_revisit, true);

  const before = f.runs.length;
  let sessionChanges = 0;
  f.session.on('sessionChanged', () => { sessionChanges += 1; });
  const reopened = ownedRunnerFixture(t);
  t.after(async () => { await reopened.runner.stopAll(); });
  pipeline.revisit = async () => reopened.runner.launch('');
  await f.session.revisitOwnedDocument();
  assert.equal(f.runs.length, before, 'Root revisit keeps the original Host/Supervisor pipeline');
  assert.equal(sessionChanges, 0, 'Root revisit does not replace the public session');
  assert.equal(f.session.ownedDocumentStatus.phase, 'observing');
  assert.equal(f.session.ownedDocumentStatus.can_authorize_save, false);
  assert.equal(f.session.ownedDocumentStatus.can_revisit, false);
  await f.session.shutdown();
});

test('quit waits for an in-flight owned launcher receipt and then cleans the late registration', async (t) => {
  const gate = deferred<void>();
  let child: LaunchChild | null = null;
  const f = await setup(t);
  // The session owns the actual runner; delay its fake launch receipt without
  // changing the opaque registration issuance path.
  const raceDir = mkdtempSync(join(tmpdir(), 'cu-owned-session-launch-race-'));
  t.after(() => rmSync(raceDir, { recursive: true, force: true }));
  const runner = createOwnedDocumentLaunchRunnerForTesting((helperPath) => {
    const created = new LaunchChild(helperPath, gate.promise); child = created;
    return created;
  }, raceDir);
  // Swap only the injected Main runner factory before creating the document.
  (f.services as { createRunner: () => OwnedDocumentLaunchRunner }).createRunner = () => runner;
  const creating = f.session.createOwnedDocument();
  for (let tries = 0; tries < 50 && !child; tries += 1) await new Promise<void>((resolve) => setTimeout(resolve, 2));
  assert.ok(child);
  const quitting = f.session.shutdown();
  gate.resolve();
  await assert.rejects(creating, /could not be started safely/);
  await quitting;
  assert.equal(f.runs.length, 1, 'a late launcher receipt must not start a new source pipeline');
  assert.equal(child!.closed, true);
});
