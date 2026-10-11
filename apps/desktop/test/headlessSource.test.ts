import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test, { type TestContext } from 'node:test';

import type {
  BrowserControlSubmit,
  BrowserPageSnapshot,
  BrowserReadScope,
  BrowserSourceControl,
} from '@computer-use/desktop-api';

import {
  HeadlessBrowserError,
  type HeadlessDiagnosticReason,
  type HeadlessObservation,
} from '../src/main/headless/cdpPipe.js';
import {
  BrowserReadBeforeFloorError,
  BrowserSourceController,
  type BrowserSourceClockPorts,
  type BrowserSourceSink,
  type BrowserSourceTestPorts,
  type BrowserSourceTrustedMainOptions,
} from '../src/main/headless/browserSource.js';
import { browserControlAckError } from '../src/main/supervisor.js';
import { HEADLESS_OBSERVATION_CLOCK_DOMAIN } from '../src/main/headless/pageSource.js';
import type {
  OwnedBrowserCheckboxAction,
  OwnedBrowserCheckboxResult,
  OwnedBrowserLaunchOptions,
  OwnedBrowserSession,
} from '../src/main/headless/ownedBrowser.js';

interface BrowserControlGoldens {
  submit: BrowserControlSubmit;
}

const controlGoldensPath = resolve(dirname(fileURLToPath(import.meta.url)), '../../../contracts/browser-v1/control-goldens.json');
const controlGoldenSubmit = (JSON.parse(readFileSync(controlGoldensPath, 'utf8')) as BrowserControlGoldens).submit;

type Deferred = { promise: Promise<void>; resolve: () => void; reject: (error: unknown) => void };

function deferred(): Deferred {
  let resolve!: () => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<void>((yes, no) => { resolve = () => yes(); reject = (error) => no(error); });
  return { promise, resolve, reject };
}

function temporaryFixture(t: TestContext): { root: string; fixturePath: string; profileParent: string } {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'cu-browser-source-')));
  const fixtureDirectory = join(root, 'fixtures', 'headless');
  const profileParent = join(root, 'profiles');
  mkdirSync(fixtureDirectory, { recursive: true });
  mkdirSync(profileParent);
  const fixturePath = join(fixtureDirectory, 'readonly-page.html');
  writeFileSync(fixturePath, '<!doctype html><title>readonly</title>');
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return { root, fixturePath, profileParent };
}

function trustedOptions(fixturePath: string, profileParent: string, clockOriginUnixMs: number): BrowserSourceTrustedMainOptions {
  return {
    chromeExecutable: '/test-only/fake-chrome',
    fixturePath,
    profileParent,
    sessionEpoch: 9,
    generation: 14,
    clockOriginUnixMs,
  };
}

function testClock(offset = 0) {
  const timeOriginUnixMs = 1_800_000_000_000;
  let monotonic = 100;
  const ports: BrowserSourceClockPorts = {
    timeOriginUnixMs,
    monotonicNowMs: () => monotonic,
    wallNowMs: () => timeOriginUnixMs + monotonic + offset,
    waitMs: async (milliseconds) => { monotonic += milliseconds; },
  };
  return {
    ports,
    timeOriginUnixMs,
    advance: (milliseconds: number) => { monotonic += milliseconds; },
    monotonic: () => monotonic,
  };
}

function observation(fixtureUrl: string, sequence: number, start: number, finish: number): HeadlessObservation {
  return {
    provenance: {
      browserSessionId: 'owned-browser-session',
      browserContextId: 'owned-browser-context',
      targetId: 'owned-page-target',
      pageId: 'owned-page-target',
      sessionId: 'owned-cdp-session',
      sequence,
      clockDomain: HEADLESS_OBSERVATION_CLOCK_DOMAIN,
      readStartedMonotonicMs: start,
      readFinishedMonotonicMs: finish,
    },
    document: { frameId: 'main-frame', loaderId: 'document-loader', url: fixtureUrl },
    rawAX: {
      nodes: [{
        nodeId: 'root-ax',
        ignored: false,
        role: { type: 'role', value: 'RootWebArea' },
        name: { type: 'computedString', value: 'readonly fixture' },
        childIds: ['checkbox-ax'],
        backendDOMNodeId: 1,
        frameId: 'main-frame',
      }, {
        nodeId: 'checkbox-ax',
        parentId: 'root-ax',
        ignored: false,
        role: { type: 'role', value: 'checkbox' },
        name: { type: 'computedString', value: 'Background option' },
        childIds: [],
        backendDOMNodeId: 17,
        frameId: 'main-frame',
        properties: [
          { name: 'checked', value: { type: 'boolean', value: false } },
          { name: 'disabled', value: { type: 'boolean', value: false } },
        ],
      }],
    },
    checkboxStates: [{
      backendDOMNodeId: 17,
      disabled: false,
      checked: false,
      indeterminate: false,
      documentUrl: fixtureUrl,
      connected: true,
      topDocument: true,
      nativeCheckbox: true,
      blockedByAriaOrInert: false,
    }],
    layoutMetrics: {},
  };
}

class FakeOwnedBrowser implements OwnedBrowserSession {
  readonly pid = 1234;
  readonly browserSessionId = 'owned-browser-session';
  observeCount = 0;
  closeCount = 0;
  failAt: number | null = null;
  wrongClockAt: number | null = null;
  staleAt: number | null = null;
  foreignContextAt: number | null = null;
  foreignUrlAt: number | null = null;
  mutateFixtureAt: number | null = null;
  fixturePath = '';
  retireNowCount = 0;
  checkboxCalls = 0;
  lastCheckboxAction: OwnedBrowserCheckboxAction | null = null;
  checkboxResult: OwnedBrowserCheckboxResult | null = null;
  checkboxError: HeadlessBrowserError | null = null;
  readonly observeStarted = new Map<number, Deferred>();
  readonly readGates = new Map<number, Deferred>();

  constructor(
    private readonly clock: ReturnType<typeof testClock>,
    private readonly fixtureUrl: string,
  ) {}

  observe(): Promise<HeadlessObservation> {
    const sequence = ++this.observeCount;
    const start = this.clock.monotonic();
    this.observeStarted.get(sequence)?.resolve();
    const gate = this.readGates.get(sequence);
    return (async () => {
      if (gate) await gate.promise;
      if (this.failAt === sequence) throw new HeadlessBrowserError('protocol_error');
      this.clock.advance(1);
      if (this.mutateFixtureAt === sequence) writeFileSync(this.fixturePath, '<!doctype html><title>mutated</title>');
      const finish = this.clock.monotonic();
      const readStart = this.staleAt === sequence ? start - 5 : start;
      const result = observation(this.fixtureUrl, sequence, readStart, finish);
      if (this.wrongClockAt === sequence) {
        (result.provenance as { clockDomain: string }).clockDomain = 'foreign-clock-domain';
      }
      if (this.foreignContextAt === sequence) {
        (result.document as { loaderId: string }).loaderId = 'foreign-document-loader';
      }
      if (this.foreignUrlAt === sequence) {
        (result.document as { url: string }).url = 'file:///tmp/foreign.html';
      }
      return result;
    })();
  }

  get sourceSequence(): number { return this.observeCount; }

  async capturePng(): Promise<never> { throw new Error('read-only source controller does not capture PNG'); }

  async setCheckbox(action: OwnedBrowserCheckboxAction): Promise<OwnedBrowserCheckboxResult> {
    this.checkboxCalls += 1;
    this.lastCheckboxAction = action;
    if (this.checkboxError) throw this.checkboxError;
    return this.checkboxResult ?? { outcome: 'rejected', failureCode: 'source_invalid' };
  }

  retireNow(): void { this.retireNowCount += 1; }

  async close(): Promise<void> { this.closeCount += 1; }
}

class RecordingSink implements BrowserSourceSink {
  readonly controls: BrowserSourceControl[] = [];
  readonly snapshots: BrowserPageSnapshot[] = [];
  readonly events: string[] = [];
  installGate: Deferred | null = null;
  installSeen: Deferred | null = null;

  async sendControl(control: BrowserSourceControl): Promise<void> {
    this.controls.push(structuredClone(control));
    this.events.push(`control:${control.kind}`);
    if (control.kind === 'install' && this.installGate) {
      this.installSeen?.resolve();
      await this.installGate.promise;
    }
  }

  async sendSnapshot(snapshot: BrowserPageSnapshot): Promise<void> {
    this.snapshots.push(snapshot);
    this.events.push('snapshot');
  }
}

function controllerFor(
  fixturePath: string,
  profileParent: string,
  clock: ReturnType<typeof testClock>,
  sink = new RecordingSink(),
  session = new FakeOwnedBrowser(clock, pathToFileURL(fixturePath).href),
) {
  session.fixturePath = fixturePath;
  const options = trustedOptions(fixturePath, profileParent, clock.timeOriginUnixMs - 10_000);
  const launchOptions: OwnedBrowserLaunchOptions[] = [];
  const ports: BrowserSourceTestPorts = {
    clock: clock.ports,
    launchSession: async (value) => { launchOptions.push(value); return session; },
  };
  return {
    controller: BrowserSourceController.forTests(options, sink, ports),
    sink,
    session,
    launchOptions,
  };
}

function submissionFor(
  scope: BrowserReadScope,
  snapshot: BrowserPageSnapshot,
  currentMs: number,
): BrowserControlSubmit {
  const submit = structuredClone(controlGoldenSubmit);
  const node = snapshot.nodes.find((candidate) => candidate.role === 'checkbox');
  if (!node || typeof node.backend_dom_node_id !== 'number'
    || typeof node.role !== 'string' || typeof node.name !== 'string') {
    throw new Error('source snapshot did not contain a fully observed checkbox');
  }
  const commandStartMs = Math.max(currentMs, snapshot.read_finished_ms);
  const commandEndMs = commandStartMs + 100;
  const evidence = structuredClone(submit.intent.evidence);
  evidence.observation_id = snapshot.snapshot_id;
  evidence.surface = { id: snapshot.context.page_target_id, epoch: snapshot.context.session_epoch };
  evidence.scope = structuredClone(scope);
  evidence.binding_ref = `browser-bind-${snapshot.snapshot_id}`;
  evidence.binding = {
    snapshot_id: snapshot.snapshot_id,
    source_sequence: snapshot.source_sequence,
    observed_ms: snapshot.observed_ms,
    read_finished_ms: snapshot.read_finished_ms,
    clock_domain: snapshot.clock_domain,
    context: structuredClone(snapshot.context),
    target_context_hash: snapshot.target_context_hash,
    ax_node_id: node.ax_node_id,
    backend_dom_node_id: node.backend_dom_node_id,
    role: node.role,
    name: node.name,
    disabled: node.disabled,
    checked: node.checked === 'true',
  };
  evidence.valid_until_ms = commandEndMs;
  evidence.dependencies = { browser_scope: scope.generation };

  submit.session_epoch = scope.context.session_epoch;
  submit.scope_id = scope.scope_id;
  submit.scope_generation = scope.generation;
  submit.intent.session_epoch = scope.context.session_epoch;
  submit.intent.latest_start_ms = commandStartMs;
  submit.intent.duration_ms = commandEndMs - commandStartMs;
  submit.intent.evidence = evidence;
  submit.authority.command_start_ms = commandStartMs;
  submit.authority.command_end_ms = commandEndMs;
  submit.authority.valid_until_ms = commandEndMs;
  submit.authority.reply_deadline_ms = commandEndMs + 500;
  submit.authority_evidence = structuredClone(evidence);
  return submit;
}

test('start pins one fixture, installs scope, then publishes only the fresh read in the same identity', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  t.after(() => owned.controller.stop());

  const snapshot = await owned.controller.start();
  assert.equal(owned.session.observeCount, 2);
  assert.equal(owned.sink.controls[0]?.kind, 'install');
  assert.equal(owned.sink.snapshots.length, 1);
  assert.equal(owned.sink.snapshots[0]?.snapshot_id, snapshot.snapshot_id);
  assert.equal(snapshot.source_sequence, 2);
  assert.equal(snapshot.context.fixture_file_url, pathToFileURL(fixture.fixturePath).href);
  assert.equal(snapshot.context.session_epoch, 9);
  assert.equal(snapshot.clock_domain, 'headless/9/wall');
  const install = owned.sink.controls[0]!;
  assert.equal(install.kind, 'install');
  if (install.kind === 'install') {
    assert.equal(install.scope.generation, 14);
    assert.equal(install.scope.context.browser_session_id, snapshot.context.browser_session_id);
    assert.equal(install.scope.context.document_loader_id, snapshot.context.document_loader_id);
    assert.ok(snapshot.observed_ms >= install.scope.issued_ms);
  }
  assert.equal(owned.controller.getSnapshot()?.snapshot_id, snapshot.snapshot_id);
  assert.equal(owned.controller.getStatus().phase, 'ready');
  assert.deepEqual(Object.keys(owned.launchOptions[0]!).sort(), ['chromeExecutable', 'fixturePath', 'profileParent']);
});

test('action accepts its exact evidence snapshot with a real nonzero read interval', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  t.after(() => owned.controller.stop());

  const snapshot = await owned.controller.start();
  assert.ok(snapshot.observed_ms < snapshot.read_finished_ms);
  const install = owned.sink.controls.find((control) => control.kind === 'install');
  assert.ok(install?.kind === 'install');
  if (install?.kind !== 'install') throw new Error('source scope was not installed');
  const submittedAt = clock.monotonic();
  owned.session.checkboxResult = {
    outcome: 'completed',
    submittedMonotonicMs: submittedAt,
    inputEndMonotonicMs: submittedAt,
  };

  const origin = clock.timeOriginUnixMs - 10_000;
  const reply = await owned.controller.executeDeviceSubmission(
    submissionFor(install.scope, snapshot, clock.ports.wallNowMs() - origin),
  );

  assert.equal(reply.outcome, 'completed');
  assert.equal(owned.session.checkboxCalls, 1);
  assert.equal(owned.session.lastCheckboxAction?.expectedSourceSequence, snapshot.source_sequence);
});

test('action rejects a newer read that starts before evidence finished or a reversed sequence', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  t.after(() => owned.controller.stop());

  const snapshot = await owned.controller.start();
  const install = owned.sink.controls.find((control) => control.kind === 'install');
  assert.ok(install?.kind === 'install');
  if (install?.kind !== 'install') throw new Error('source scope was not installed');
  const origin = clock.timeOriginUnixMs - 10_000;
  const currentMs = clock.ports.wallNowMs() - origin;
  const submit = submissionFor(install.scope, snapshot, currentMs);
  const sourceInternals = owned.controller as unknown as { latestSnapshot: BrowserPageSnapshot | null };

  sourceInternals.latestSnapshot = {
    ...snapshot,
    snapshot_id: '550e8400-e29b-41d4-a716-446655440099',
    source_sequence: snapshot.source_sequence + 1,
    observed_ms: snapshot.read_finished_ms - 1,
  };
  const staleNewer = await owned.controller.executeDeviceSubmission(submit);
  assert.equal(staleNewer.outcome, 'rejected');
  assert.equal(staleNewer.failure_code, 'evidence_expired');
  assert.equal(owned.session.checkboxCalls, 0);

  sourceInternals.latestSnapshot = {
    ...snapshot,
    snapshot_id: '550e8400-e29b-41d4-a716-446655440098',
    source_sequence: snapshot.source_sequence - 1,
  };
  const reversed = await owned.controller.executeDeviceSubmission(submit);
  assert.equal(reversed.outcome, 'rejected');
  assert.equal(reversed.failure_code, 'evidence_expired');
  assert.equal(owned.session.checkboxCalls, 0);
});

test('action accepts a later source read only when its start follows evidence finish', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  t.after(() => owned.controller.stop());

  const evidenceSnapshot = await owned.controller.start();
  const install = owned.sink.controls.find((control) => control.kind === 'install');
  assert.ok(install?.kind === 'install');
  if (install?.kind !== 'install') throw new Error('source scope was not installed');
  const latestSnapshot = await owned.controller.refreshAfter(evidenceSnapshot.read_finished_ms);
  assert.ok(latestSnapshot.source_sequence > evidenceSnapshot.source_sequence);
  assert.ok(latestSnapshot.observed_ms >= evidenceSnapshot.read_finished_ms);
  const submittedAt = clock.monotonic();
  owned.session.checkboxResult = {
    outcome: 'completed',
    submittedMonotonicMs: submittedAt,
    inputEndMonotonicMs: submittedAt,
  };

  const origin = clock.timeOriginUnixMs - 10_000;
  const reply = await owned.controller.executeDeviceSubmission(
    submissionFor(install.scope, evidenceSnapshot, clock.ports.wallNowMs() - origin),
  );

  assert.equal(reply.outcome, 'completed');
  assert.equal(owned.session.checkboxCalls, 1);
  assert.equal(owned.session.lastCheckboxAction?.expectedSourceSequence, latestSnapshot.source_sequence);
});

test('source status retains only fixed CDP diagnostic fields after a parser failure', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  t.after(() => owned.controller.stop());

  const snapshot = await owned.controller.start();
  const install = owned.sink.controls.find((control) => control.kind === 'install');
  assert.ok(install?.kind === 'install');
  if (install?.kind !== 'install') throw new Error('source scope was not installed');
  owned.session.checkboxError = new HeadlessBrowserError('protocol_error', {
    stage: 'checkbox_action_ax',
    reason: 'raw_sensitive_value' as unknown as HeadlessDiagnosticReason,
    commandMethod: 'Accessibility.getPartialAXTree',
    nativeErrorCode: -32_000,
  });

  const origin = clock.timeOriginUnixMs - 10_000;
  const reply = await owned.controller.executeDeviceSubmission(
    submissionFor(install.scope, snapshot, clock.ports.wallNowMs() - origin),
  );

  assert.equal(reply.outcome, 'rejected');
  assert.equal(reply.failure_code, 'source_invalid');
  assert.equal(owned.controller.getStatus().phase, 'failed');
  assert.deepEqual(owned.controller.getStatus().diagnostic, {
    stage: 'checkbox_action_ax',
    commandMethod: 'Accessibility.getPartialAXTree',
    responseCode: -32_000,
  });
  assert.doesNotMatch(JSON.stringify(owned.controller.getStatus().diagnostic), /raw_sensitive_value/);
  assert.equal(JSON.stringify(owned.controller.getStatus().diagnostic).includes('protocol_error'), false);
});

test('source status retains diagnostics returned with a safe source_invalid result', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  t.after(() => owned.controller.stop());

  const snapshot = await owned.controller.start();
  const install = owned.sink.controls.find((control) => control.kind === 'install');
  assert.ok(install?.kind === 'install');
  if (install?.kind !== 'install') throw new Error('source scope was not installed');
  owned.session.checkboxResult = {
    outcome: 'rejected',
    failureCode: 'source_invalid',
    diagnostic: {
      stage: 'checkbox_action_snapshot',
      reason: 'layout_row_style',
      commandMethod: 'DOMSnapshot.captureSnapshot',
      nativeErrorCode: -32_001,
    },
  };

  const origin = clock.timeOriginUnixMs - 10_000;
  const reply = await owned.controller.executeDeviceSubmission(
    submissionFor(install.scope, snapshot, clock.ports.wallNowMs() - origin),
  );
  assert.equal(reply.outcome, 'rejected');
  assert.equal(reply.failure_code, 'source_invalid');
  assert.equal(owned.controller.getStatus().phase, 'ready');
  assert.deepEqual(owned.controller.getStatus().diagnostic, {
    stage: 'checkbox_action_snapshot',
    reason: 'layout_row_style',
    commandMethod: 'DOMSnapshot.captureSnapshot',
    responseCode: -32_001,
  });

  await owned.controller.retireForActionOutcome('source_unavailable');
  assert.equal(owned.controller.getStatus().phase, 'failed');
  assert.deepEqual(owned.controller.getStatus().diagnostic, {
    stage: 'checkbox_action_snapshot',
    reason: 'layout_row_style',
    commandMethod: 'DOMSnapshot.captureSnapshot',
    responseCode: -32_001,
  });
});

test('prepare freezes the first actual identity without installing; start reuses it and publishes a fresh read', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  t.after(() => owned.controller.stop());

  const prepared = await owned.controller.prepare();
  assert.equal(owned.session.observeCount, 1);
  assert.equal(owned.sink.controls.length, 0);
  assert.equal(owned.sink.snapshots.length, 0);
  assert.equal(owned.controller.getStatus().phase, 'prepared');
  assert.equal(prepared.context.page_target_id, 'owned-page-target');

  const snapshot = await owned.controller.start();
  assert.equal(owned.session.observeCount, 2);
  assert.equal(owned.sink.controls.filter((control) => control.kind === 'install').length, 1);
  assert.equal(owned.sink.snapshots.length, 1);
  const install = owned.sink.controls.find((control) => control.kind === 'install');
  assert.ok(install?.kind === 'install');
  if (install?.kind === 'install') assert.equal(install.scope.scope_id, prepared.scope_id);
  assert.ok(snapshot.source_sequence > 1);
});

test('scope and publication wait for Host wall time without rewriting the captured read interval', async (t) => {
  for (const wallOffset of [0, -25]) {
    const fixture = temporaryFixture(t);
    const base = testClock(wallOffset);
    const clock = { ...base, ports: { ...base.ports, monotonicNowMs: () => base.monotonic() + 0.25 } };
    const sink = new RecordingSink();
    const session = new FakeOwnedBrowser(clock, pathToFileURL(fixture.fixturePath).href);
    const captured: HeadlessObservation[] = [];
    const read = session.observe.bind(session);
    session.observe = async () => {
      const raw = await read();
      const result = { ...raw, provenance: { ...raw.provenance,
        readStartedMonotonicMs: raw.provenance.readStartedMonotonicMs + 0.25,
        readFinishedMonotonicMs: raw.provenance.readFinishedMonotonicMs + 0.25,
      } };
      captured.push(structuredClone(result));
      return result;
    };
    const origin = base.timeOriginUnixMs - 10_000;
    const control = sink.sendControl.bind(sink);
    sink.sendControl = async value => {
      if (value.kind === 'install') assert.ok(base.ports.wallNowMs() - origin >= value.scope.issued_ms);
      await control(value);
    };
    const publish = sink.sendSnapshot.bind(sink);
    sink.sendSnapshot = async value => {
      assert.ok(base.ports.wallNowMs() - origin >= value.read_finished_ms);
      await publish(value);
    };
    const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock, sink, session);
    t.after(() => owned.controller.stop());
    const snapshot = await owned.controller.start();
    const original = captured.at(-1)!;
    assert.equal(snapshot.observed_ms, Math.floor(10_000 + original.provenance.readStartedMonotonicMs));
    assert.equal(snapshot.read_finished_ms, Math.ceil(10_000 + original.provenance.readFinishedMonotonicMs));
    await owned.controller.stop();
  }
});

test('install acknowledgement is a barrier before the second read and publication', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const sink = new RecordingSink();
  sink.installGate = deferred();
  sink.installSeen = deferred();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock, sink);
  t.after(() => owned.controller.stop());

  const started = owned.controller.start();
  await sink.installSeen.promise;
  assert.equal(owned.session.observeCount, 1);
  assert.equal(sink.snapshots.length, 0);
  sink.installGate.resolve();
  await started;
  assert.equal(owned.session.observeCount, 2);
  assert.equal(sink.snapshots.length, 1);
});

test('stop during an unacknowledged install invalidates immediately and again after the ACK race', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const sink = new RecordingSink();
  sink.installGate = deferred();
  sink.installSeen = deferred();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock, sink);

  const started = owned.controller.start();
  await sink.installSeen.promise;
  assert.equal(owned.session.observeCount, 1);
  const stopping = owned.controller.stop();
  assert.equal(sink.controls.at(-1)?.kind, 'invalidate');
  assert.equal(owned.session.closeCount, 0);

  sink.installGate.resolve();
  await assert.rejects(started);
  await stopping;
  assert.equal(owned.session.observeCount, 1);
  assert.equal(sink.snapshots.length, 0);
  assert.equal(sink.controls.filter((control) => control.kind === 'invalidate').length, 2);
  assert.equal(owned.session.closeCount, 1);
});

test('stop retries invalidation after an install ACK is lost', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const sink = new RecordingSink();
  sink.installGate = deferred();
  sink.installSeen = deferred();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock, sink);

  const started = owned.controller.start();
  await sink.installSeen.promise;
  const stopping = owned.controller.stop();
  assert.equal(sink.controls.at(-1)?.kind, 'invalidate');

  sink.installGate.reject(new Error('install ACK was lost'));
  await assert.rejects(started);
  await stopping;
  assert.equal(sink.controls.filter((control) => control.kind === 'invalidate').length, 2);
  assert.equal(sink.snapshots.length, 0);
  assert.equal(owned.session.closeCount, 1);
});

test('only the closed snapshot rejection ACK allows a superseded-read retry', () => {
  assert(browserControlAckError('browser_page_snapshot', null, 'browser_read_before_floor') instanceof BrowserReadBeforeFloorError);
  for (const error of [
    browserControlAckError('browser_source_control', null, 'browser_read_before_floor'),
    browserControlAckError('operator_command', null, 'browser_read_before_floor'),
    browserControlAckError('browser_page_snapshot', 'stale_binding', 'browser_read_before_floor'),
    browserControlAckError('browser_page_snapshot', null, 'browser page read rejected'),
  ]) assert(!(error instanceof BrowserReadBeforeFloorError));
});

test('a superseded in-flight read retains scope and publishes only a later acknowledged read', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock(200); // Host wall time leads the accepted performance clock.
  let rejected: BrowserPageSnapshot | null = null;
  let hostFloor = 0;
  class ModeFloorSink extends RecordingSink {
    override async sendSnapshot(snapshot: BrowserPageSnapshot): Promise<void> {
      if (snapshot.source_sequence === 3) {
        rejected = snapshot;
        clock.advance(10); // The mode boundary and its ACK occur after this read.
        hostFloor = clock.ports.wallNowMs() - (clock.timeOriginUnixMs - 10_000);
        throw new BrowserReadBeforeFloorError();
      }
      if (hostFloor && snapshot.observed_ms <= hostFloor) throw new BrowserReadBeforeFloorError();
      return super.sendSnapshot(snapshot);
    }
  }
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock, new ModeFloorSink());
  t.after(() => owned.controller.stop());
  const first = await owned.controller.start();
  const refreshed = await owned.controller.refresh();
  assert(rejected);
  assert.equal(refreshed.source_sequence, 4);
  assert(refreshed.observed_ms > (rejected as BrowserPageSnapshot).read_finished_ms);
  assert(refreshed.observed_ms > hostFloor);
  assert.equal(owned.controller.getStatus().phase, 'ready');
  assert.equal(owned.sink.controls.length, 1);
  assert.equal(owned.session.closeCount, 0);
  assert.equal(owned.session.checkboxCalls, 0);
  assert.deepEqual(owned.sink.snapshots.map(snapshot => snapshot.source_sequence), [first.source_sequence, 4]);
  assert.equal(owned.controller.getSnapshot()?.snapshot_id, refreshed.snapshot_id);
});

test('repeated superseded reads are bounded and ordinary read rejection remains fatal', async (t) => {
  for (const error of [new BrowserReadBeforeFloorError(), new Error('browser_read_before_floor')]) {
    const fixture = temporaryFixture(t);
    const clock = testClock();
    class RejectingSink extends RecordingSink {
      override async sendSnapshot(snapshot: BrowserPageSnapshot): Promise<void> {
        if (snapshot.source_sequence > 2) throw error;
        return super.sendSnapshot(snapshot);
      }
    }
    const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock, new RejectingSink());
    await owned.controller.start();
    await assert.rejects(owned.controller.refresh());
    assert.equal(owned.session.observeCount, error instanceof BrowserReadBeforeFloorError ? 5 : 3);
    assert.equal(owned.controller.getStatus().phase, 'failed');
    assert.equal(owned.controller.getSnapshot(), null);
    assert.equal(owned.session.closeCount, 1);
    assert.equal(owned.session.checkboxCalls, 0);
    assert.equal(owned.sink.controls.at(-1)?.kind, 'invalidate');
  }
});

test('refresh calls are serial and burst-coalesced into one follow-up read', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  t.after(() => owned.controller.stop());
  await owned.controller.start();

  const started = deferred();
  const gate = deferred();
  owned.session.observeStarted.set(3, started);
  owned.session.readGates.set(3, gate);
  const first = owned.controller.refresh();
  await started.promise;
  const second = owned.controller.refresh();
  const third = owned.controller.refresh();
  assert.equal(first, second);
  assert.equal(second, third);
  assert.equal(owned.session.observeCount, 3);

  gate.resolve();
  const [one, two, three] = await Promise.all([first, second, third]);
  assert.equal(owned.session.observeCount, 4);
  assert.equal(one.snapshot_id, two.snapshot_id);
  assert.equal(two.snapshot_id, three.snapshot_id);
  assert.equal(one.source_sequence, 4);
  assert.equal(owned.sink.snapshots.length, 3);
});

test('stopping during a read invalidates first and prevents a late snapshot publication', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  t.after(() => owned.controller.stop());
  await owned.controller.start();

  const started = deferred();
  const gate = deferred();
  owned.session.observeStarted.set(3, started);
  owned.session.readGates.set(3, gate);
  const refreshing = owned.controller.refresh();
  await started.promise;
  const stopping = owned.controller.stop();
  assert.equal(owned.sink.controls.at(-1)?.kind, 'invalidate');
  assert.equal(owned.session.closeCount, 0);
  gate.resolve();
  await assert.rejects(refreshing);
  await stopping;

  assert.equal(owned.sink.snapshots.length, 1);
  assert.equal(owned.sink.controls.at(-1)?.kind, 'invalidate');
  assert.equal(owned.session.closeCount, 1);
  assert.equal(owned.controller.getSnapshot(), null);
  assert.equal(owned.controller.getStatus().phase, 'stopped');
});

test('stale time, foreign clock, and changed document identity invalidate the installed scope', async (t) => {
  for (const scenario of ['stale', 'foreign_clock', 'foreign_document', 'foreign_url'] as const) {
    await t.test(scenario, async (subtest) => {
      const fixture = temporaryFixture(subtest);
      const clock = testClock();
      const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
      if (scenario === 'stale') owned.session.staleAt = 2;
      else if (scenario === 'foreign_clock') owned.session.wrongClockAt = 2;
      else if (scenario === 'foreign_document') owned.session.foreignContextAt = 2;
      else owned.session.foreignUrlAt = 2;

      await assert.rejects(owned.controller.start());
      assert.equal(owned.sink.snapshots.length, 0);
      assert.equal(owned.session.closeCount, 1);
      const invalidation = owned.sink.controls.at(-1);
      assert.equal(invalidation?.kind, 'invalidate');
      if (invalidation?.kind === 'invalidate') {
        assert.equal(invalidation.reason,
          scenario === 'foreign_document' || scenario === 'foreign_url' ? 'document_changed' : 'source_unavailable');
      }
      assert.equal(owned.controller.getStatus().phase, 'failed');
    });
  }
});

test('clock drift beyond the host bound fails before launching the owned browser', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock(1_000);
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);

  await assert.rejects(owned.controller.start());
  assert.equal(owned.launchOptions.length, 0);
  assert.equal(owned.sink.controls.length, 0);
  assert.equal(owned.controller.getStatus().phase, 'failed');
});

test('controller rejects any fixture path outside the fixed readonly page suffix', (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const otherPath = join(fixture.root, 'other.html');
  writeFileSync(otherPath, '<!doctype html><title>other</title>');
  const ports: BrowserSourceTestPorts = {
    clock: clock.ports,
    launchSession: async () => { throw new Error('must not launch'); },
  };

  assert.throws(() => BrowserSourceController.forTests(
    trustedOptions(otherPath, fixture.profileParent, clock.timeOriginUnixMs - 10_000),
    new RecordingSink(),
    ports,
  ), (error: unknown) => error instanceof Error && error.message === 'invalid_input');
});

test('fixture content mutation invalidates the scope and closes the owned session', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  await owned.controller.start();
  owned.session.mutateFixtureAt = 3;

  await assert.rejects(owned.controller.refresh());
  assert.equal(owned.sink.snapshots.length, 1);
  const invalidation = owned.sink.controls.at(-1);
  assert.equal(invalidation?.kind, 'invalidate');
  if (invalidation?.kind === 'invalidate') assert.equal(invalidation.reason, 'source_unavailable');
  assert.equal(owned.session.closeCount, 1);
  assert.equal(owned.controller.getSnapshot(), null);
  assert.equal(owned.controller.getStatus().phase, 'failed');
});

test('session failure invalidates the scope and closes exactly that owned session', async (t) => {
  const fixture = temporaryFixture(t);
  const clock = testClock();
  const owned = controllerFor(fixture.fixturePath, fixture.profileParent, clock);
  owned.session.failAt = 2;

  await assert.rejects(owned.controller.start(), (error: unknown) =>
    error instanceof HeadlessBrowserError && error.code === 'protocol_error');
  const invalidation = owned.sink.controls.at(-1);
  assert.equal(invalidation?.kind, 'invalidate');
  if (invalidation?.kind === 'invalidate') assert.equal(invalidation.reason, 'source_unavailable');
  assert.equal(owned.session.closeCount, 1);
  assert.equal(owned.controller.getSnapshot(), null);
});
