import { createHash, randomUUID } from 'node:crypto';
import { lstatSync, readFileSync, realpathSync } from 'node:fs';
import { isAbsolute, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';

import {
  validateBrowserReadScope,
  validateBrowserSourceControl,
  validateBrowserControlSubmit,
  type BrowserControlReply,
  type BrowserControlSubmit,
  type BrowserPageSnapshot,
  type BrowserReadScope,
  type BrowserSourceControl,
} from '@computer-use/desktop-api';

import {
  HeadlessBrowserError,
  type HeadlessBrowserDiagnostic,
  type HeadlessCdpCommandMethod,
  type HeadlessDiagnosticReason,
  type HeadlessDiagnosticStage,
  type HeadlessObservation,
} from './cdpPipe.js';
import {
  createBrowserPageSnapshot,
  HEADLESS_OBSERVATION_CLOCK_DOMAIN,
} from './pageSource.js';
import {
  launchOwnedBrowser,
  OwnedBrowserActionOutcomeUnknown,
  type OwnedBrowserLaunchOptions,
  type OwnedBrowserSession,
} from './ownedBrowser.js';

const FIXED_FIXTURE_SUFFIX = `${sep}fixtures${sep}headless${sep}readonly-page.html`;
const MAX_FIXTURE_BYTES = 1_048_576;
const MAX_CLOCK_DRIFT_MS = 250;
const BROWSER_SOURCE_DIAGNOSTIC_REASONS: ReadonlySet<HeadlessDiagnosticReason> = new Set([
  'top_level',
  'document_url_index',
  'node_tables',
  'node_row',
  'attributes',
  'root_graph',
  'shadow_data',
  'checked_data',
  'layout_tables',
  'layout_row_style',
]);

export interface BrowserSourceTrustedMainOptions {
  readonly chromeExecutable: string;
  readonly fixturePath: string;
  readonly profileParent: string;
  readonly sessionEpoch: number;
  readonly generation: number;
  readonly clockOriginUnixMs: number;
}

export interface BrowserSourceSink {
  /** Resolves only after the authenticated host has applied and acknowledged the control. */
  sendControl(control: BrowserSourceControl): Promise<void>;
  /** The supervisor authenticates and routes this snapshot to the host. */
  sendSnapshot(snapshot: BrowserPageSnapshot): Promise<void>;
}

export interface BrowserSourceClockPorts {
  readonly timeOriginUnixMs: number;
  readonly monotonicNowMs: () => number;
  readonly wallNowMs: () => number;
  readonly waitMs: (milliseconds: number) => Promise<void>;
}

/** Only the test constructor accepts fake sessions and clocks. */
export interface BrowserSourceTestPorts {
  readonly launchSession: (options: OwnedBrowserLaunchOptions) => Promise<OwnedBrowserSession>;
  readonly clock: BrowserSourceClockPorts;
}

export type BrowserSourcePhase = 'idle' | 'preparing' | 'prepared' | 'starting' | 'ready' | 'stopping' | 'stopped' | 'failed';
export type BrowserSourceErrorCode = 'invalid_input' | 'source_unavailable' | 'document_changed' | 'session_closed' | 'stopped';

export interface BrowserSourceStatus {
  readonly phase: BrowserSourcePhase;
  readonly generation: number;
  readonly scopeId: string | null;
  readonly lastSequence: number | null;
  readonly errorCode: BrowserSourceErrorCode | null;
  readonly diagnostic: BrowserSourceDiagnostic | null;
}

export interface BrowserSourceDiagnostic {
  readonly stage: HeadlessDiagnosticStage;
  readonly reason?: HeadlessDiagnosticReason;
  readonly commandMethod?: HeadlessCdpCommandMethod;
  readonly responseCode?: number;
}

export class BrowserSourceControllerError extends Error {
  constructor(readonly code: BrowserSourceErrorCode) {
    super(code);
    this.name = 'BrowserSourceControllerError';
  }
}

/** Only an authenticated negative ACK for a superseded read creates this error. */
export class BrowserReadBeforeFloorError extends Error {
  constructor() {
    super('browser_read_before_floor');
    this.name = 'BrowserReadBeforeFloorError';
  }
}

function requireOptions(options: BrowserSourceTrustedMainOptions): BrowserSourceTrustedMainOptions {
  if (!options || typeof options !== 'object'
    || !Number.isSafeInteger(options.sessionEpoch) || options.sessionEpoch <= 0
    || !Number.isSafeInteger(options.generation) || options.generation <= 0
    || !Number.isSafeInteger(options.clockOriginUnixMs) || options.clockOriginUnixMs < 0
    || typeof options.chromeExecutable !== 'string' || options.chromeExecutable.length === 0
    || typeof options.profileParent !== 'string' || options.profileParent.length === 0
    || typeof options.fixturePath !== 'string' || !isAbsolute(options.fixturePath)
    || !options.fixturePath.endsWith(FIXED_FIXTURE_SUFFIX)) {
    throw new BrowserSourceControllerError('invalid_input');
  }
  return Object.freeze({ ...options });
}

function fixtureSha256(path: string): string {
  try {
    const stat = lstatSync(path);
    const canonical = realpathSync(path);
    if (!stat.isFile() || stat.isSymbolicLink() || canonical !== path || stat.size <= 0 || stat.size > MAX_FIXTURE_BYTES) {
      throw new BrowserSourceControllerError('source_unavailable');
    }
    const bytes = readFileSync(path);
    if (bytes.length !== stat.size || bytes.length > MAX_FIXTURE_BYTES) {
      throw new BrowserSourceControllerError('source_unavailable');
    }
    return createHash('sha256').update(bytes).digest('hex');
  } catch (error) {
    if (error instanceof BrowserSourceControllerError) throw error;
    throw new BrowserSourceControllerError('source_unavailable');
  }
}

function sameContext(left: BrowserPageSnapshot['context'], right: BrowserPageSnapshot['context']): boolean {
  return left.browser_session_id === right.browser_session_id
    && left.browser_context_id === right.browser_context_id
    && left.page_target_id === right.page_target_id
    && left.cdp_session_id === right.cdp_session_id
    && left.frame_id === right.frame_id
    && left.document_loader_id === right.document_loader_id
    && left.fixture_file_url === right.fixture_file_url
    && left.session_epoch === right.session_epoch;
}

function actionSnapshotMatchesBinding(
  snapshot: BrowserPageSnapshot,
  binding: BrowserControlSubmit['intent']['evidence']['binding'],
): boolean {
  if (snapshot.source_sequence === binding.source_sequence) {
    return snapshot.snapshot_id === binding.snapshot_id
      && snapshot.observed_ms === binding.observed_ms
      && snapshot.read_finished_ms === binding.read_finished_ms
      && snapshot.clock_domain === binding.clock_domain;
  }
  return snapshot.source_sequence > binding.source_sequence
    && snapshot.snapshot_id !== binding.snapshot_id
    && snapshot.clock_domain === binding.clock_domain
    && snapshot.observed_ms >= binding.read_finished_ms;
}

function sameScope(left: BrowserReadScope, right: BrowserReadScope): boolean {
  return left.scope_id === right.scope_id && left.generation === right.generation
    && left.clock_domain === right.clock_domain && left.issued_ms === right.issued_ms
    && sameContext(left.context, right.context);
}

function makeBrowserControlReply(
  submit: BrowserControlSubmit,
  outcome: BrowserControlReply['outcome'],
  submittedAtMs: number | null,
  inputEndMs: number | null,
  failureCode: BrowserControlReply['failure_code'],
): BrowserControlReply {
  return {
    version: 1,
    request_id: submit.request_id,
    channel_generation: submit.channel_generation,
    session_epoch: submit.session_epoch,
    scope_id: submit.scope_id,
    scope_generation: submit.scope_generation,
    sequence: submit.sequence,
    intent_id: submit.intent.intent_id,
    outcome,
    submitted_at_ms: submittedAtMs,
    input_end_ms: inputEndMs,
    failure_code: failureCode,
  };
}

function freezeSnapshot(snapshot: BrowserPageSnapshot): BrowserPageSnapshot {
  for (const node of snapshot.nodes) {
    Object.freeze(node.child_ax_node_ids);
    Object.freeze(node);
  }
  Object.freeze(snapshot.nodes);
  Object.freeze(snapshot.context);
  return Object.freeze(snapshot) as BrowserPageSnapshot;
}

function sourceErrorCode(error: unknown): BrowserSourceErrorCode {
  if (error instanceof BrowserSourceControllerError) return error.code;
  if (error instanceof HeadlessBrowserError) {
    if (error.code === 'document_changed') return 'document_changed';
    if (error.code === 'closed' || error.code === 'child_exit_unconfirmed') return 'session_closed';
  }
  return 'source_unavailable';
}

function invalidationReason(code: BrowserSourceErrorCode): Extract<BrowserSourceControl, { kind: 'invalidate' }>['reason'] {
  if (code === 'document_changed') return 'document_changed';
  if (code === 'session_closed') return 'session_closed';
  return 'source_unavailable';
}

function boundedBrowserSourceDiagnostic(diagnostic: HeadlessBrowserDiagnostic | undefined): BrowserSourceDiagnostic | null {
  if (!diagnostic) return null;
  const { stage, commandMethod, nativeErrorCode } = diagnostic;
  const reason = stage === 'checkbox_action_snapshot'
    && diagnostic.reason !== undefined && BROWSER_SOURCE_DIAGNOSTIC_REASONS.has(diagnostic.reason)
    ? diagnostic.reason : undefined;
  return Object.freeze({
    stage,
    ...(reason !== undefined ? { reason } : {}),
    ...(commandMethod !== undefined ? { commandMethod } : {}),
    ...(typeof nativeErrorCode === 'number' && Number.isSafeInteger(nativeErrorCode)
      && nativeErrorCode >= -1_000_000 && nativeErrorCode <= 1_000_000
      ? { responseCode: nativeErrorCode } : {}),
  });
}

function browserSourceDiagnosticFromError(error: unknown): BrowserSourceDiagnostic | null {
  return error instanceof HeadlessBrowserError ? boundedBrowserSourceDiagnostic(error.diagnostic) : null;
}

/**
 * Main-owned fixed-fixture resource. Observation remains the normal path; the
 * only action entry is consumed by the private Device fd controller.
 */
export class BrowserSourceController {
  static create(options: BrowserSourceTrustedMainOptions, sink: BrowserSourceSink): BrowserSourceController {
    return new BrowserSourceController(options, sink, {
      launchSession: launchOwnedBrowser,
      clock: {
        timeOriginUnixMs: performance.timeOrigin,
        monotonicNowMs: () => performance.now(),
        wallNowMs: () => Date.now(),
        waitMs: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
      },
    });
  }

  /** @internal Test seam: production callers use `create`, which fixes the browser and clocks. */
  static forTests(
    options: BrowserSourceTrustedMainOptions,
    sink: BrowserSourceSink,
    ports: BrowserSourceTestPorts,
  ): BrowserSourceController {
    return new BrowserSourceController(options, sink, ports);
  }

  private readonly options: BrowserSourceTrustedMainOptions;
  private readonly fixtureUrl: string;
  private readonly fixturePin: string;
  private readonly clockDomain: string;
  private phase: BrowserSourcePhase = 'idle';
  private active = false;
  private scope: BrowserReadScope | null = null;
  private initialSnapshot: BrowserPageSnapshot | null = null;
  private installStarted = false;
  private installAcknowledged = false;
  private session: OwnedBrowserSession | null = null;
  private sessionCloseTask: Promise<void> | null = null;
  private prepareTask: Promise<BrowserReadScope> | null = null;
  private startTask: Promise<BrowserPageSnapshot> | null = null;
  private refreshTask: Promise<BrowserPageSnapshot> | null = null;
  private stopTask: Promise<void> | null = null;
  private failureTask: Promise<void> | null = null;
  private refreshRequested = false;
  private refreshRequestedAfterMs: number | null = null;
  private latestSnapshot: BrowserPageSnapshot | null = null;
  private lastErrorCode: BrowserSourceErrorCode | null = null;
  private lastDiagnostic: BrowserSourceDiagnostic | null = null;

  private constructor(
    options: BrowserSourceTrustedMainOptions,
    private readonly sink: BrowserSourceSink,
    private readonly ports: BrowserSourceTestPorts,
  ) {
    this.options = requireOptions(options);
    this.fixturePin = fixtureSha256(this.options.fixturePath);
    this.fixtureUrl = pathToFileURL(this.options.fixturePath).href;
    this.clockDomain = `headless/${this.options.sessionEpoch}/wall`;
  }

  /** Prepare the owned page and mint its exact scope before Device or Host startup. */
  prepare(): Promise<BrowserReadScope> {
    if (this.scope && this.initialSnapshot && ['prepared', 'starting', 'ready'].includes(this.phase)) {
      return Promise.resolve(this.scope);
    }
    if (this.prepareTask) return this.prepareTask;
    if (this.phase !== 'idle') return Promise.reject(new BrowserSourceControllerError('stopped'));
    this.phase = 'preparing';
    this.active = true;
    const task = this.prepareOwnedSource();
    this.prepareTask = task;
    return task;
  }

  /** Preserve the observation-only startup path: install then publish a post-ACK read. */
  start(): Promise<BrowserPageSnapshot> {
    if (this.startTask) return this.startTask;
    if (!['idle', 'preparing', 'prepared'].includes(this.phase)) {
      return Promise.reject(new BrowserSourceControllerError('stopped'));
    }
    const prepared = this.phase === 'prepared' ? Promise.resolve(this.scope!) : this.prepare();
    const task = prepared.then(() => this.activatePreparedSource());
    this.startTask = task;
    return task;
  }

  refresh(): Promise<BrowserPageSnapshot> {
    if (this.phase !== 'ready' || !this.active) {
      return Promise.reject(new BrowserSourceControllerError(this.phase === 'stopping' || this.phase === 'stopped'
        ? 'stopped' : 'source_unavailable'));
    }
    const latest = this.latestSnapshot;
    if (!latest) return Promise.reject(new BrowserSourceControllerError('source_unavailable'));
    return this.refreshAfter(latest.read_finished_ms);
  }

  /** Read and publish only after a genuine read start later than the supplied logical time. */
  refreshAfter(afterMs: number): Promise<BrowserPageSnapshot> {
    if (!Number.isSafeInteger(afterMs) || afterMs < 0 || afterMs >= Number.MAX_SAFE_INTEGER) {
      return Promise.reject(new BrowserSourceControllerError('source_unavailable'));
    }
    if (this.phase !== 'ready' || !this.active) {
      return Promise.reject(new BrowserSourceControllerError(this.phase === 'stopping' || this.phase === 'stopped'
        ? 'stopped' : 'source_unavailable'));
    }
    this.refreshRequested = true;
    this.refreshRequestedAfterMs = Math.max(this.refreshRequestedAfterMs ?? 0, afterMs);
    if (this.refreshTask) return this.refreshTask;
    const task = this.drainRefreshRequests();
    this.refreshTask = task;
    const clearTask = () => {
      if (this.refreshTask === task) this.refreshTask = null;
    };
    void task.then(clearTask, clearTask);
    return task;
  }

  stop(): Promise<void> {
    if (this.stopTask) return this.stopTask;
    this.active = false;
    this.phase = 'stopping';
    this.latestSnapshot = null;
    const scope = this.installStarted ? this.scope : null;
    const installWasPending = this.installStarted && !this.installAcknowledged;
    this.stopTask = this.stopOwnedSource(scope, installWasPending);
    return this.stopTask;
  }

  getSnapshot(): BrowserPageSnapshot | null {
    return this.latestSnapshot;
  }

  /** Main-internal leaf operation. Supervisor/Renderer never receives this method as an action route. */
  async executeDeviceSubmission(value: unknown): Promise<BrowserControlReply> {
    const validation = await validateBrowserControlSubmit(value);
    if (!validation.ok) throw new BrowserSourceControllerError('source_unavailable');
    const submit = validation.value;
    const rejected = (failureCode: NonNullable<BrowserControlReply['failure_code']>): BrowserControlReply =>
      makeBrowserControlReply(submit, 'rejected', null, null, failureCode);
    const scope = this.scope;
    if (!this.active || this.phase !== 'ready' || !scope || !this.installAcknowledged) return rejected('source_invalid');
    if (submit.session_epoch !== this.options.sessionEpoch || submit.scope_id !== scope.scope_id
      || submit.scope_generation !== scope.generation || !sameScope(submit.intent.evidence.scope, scope)
      || !sameScope(submit.authority_evidence.scope, scope)) return rejected('source_invalid');

    const authority = submit.authority;
    let nowMs = this.guardClock();
    if (nowMs > authority.valid_until_ms || nowMs > authority.reply_deadline_ms
      || nowMs > submit.intent.latest_start_ms) return rejected('evidence_expired');
    if (nowMs < authority.command_start_ms) {
      if (authority.command_start_ms - nowMs > 1_000) return rejected('evidence_expired');
      await this.waitForLogicalBoundary(authority.command_start_ms);
      nowMs = this.guardClock();
    }
    if (!this.active || this.phase !== 'ready') throw new BrowserSourceControllerError('stopped');
    if (nowMs > authority.command_end_ms || nowMs > authority.valid_until_ms
      || nowMs > authority.reply_deadline_ms) return rejected('evidence_expired');

    const latest = this.latestSnapshot;
    const binding = submit.intent.evidence.binding;
    const session = this.session;
    if (!latest || !session || !sameContext(latest.context, scope.context)
      || latest.target_context_hash !== submit.intent.evidence.binding.target_context_hash
      || !actionSnapshotMatchesBinding(latest, binding)
      || binding.disabled !== 'false') return rejected('evidence_expired');
    const node = latest.nodes.find((candidate) => candidate.ax_node_id === binding.ax_node_id);
    if (!node || node.ignored || node.role !== 'checkbox' || node.name !== binding.name
      || node.backend_dom_node_id !== binding.backend_dom_node_id
      || node.disabled !== 'false'
      || node.checked !== (binding.checked ? 'true' : 'false')) return rejected('evidence_expired');

    const inputDeadlineMonotonicMs = this.options.clockOriginUnixMs + authority.command_end_ms
      - this.ports.clock.timeOriginUnixMs;
    if (!Number.isFinite(inputDeadlineMonotonicMs) || inputDeadlineMonotonicMs < 0) {
      return rejected('evidence_expired');
    }

    let gestureCompleted = false;
    try {
      this.assertFixtureUnchanged();
      const action = await session.setCheckbox({
        expectedDocument: {
          frameId: scope.context.frame_id,
          loaderId: scope.context.document_loader_id,
          url: this.fixtureUrl,
        },
        backendDOMNodeId: binding.backend_dom_node_id,
        expectedChecked: binding.checked,
        desiredChecked: submit.intent.action.checked,
        expectedSourceSequence: latest.source_sequence,
        inputDeadlineMonotonicMs,
        isSourceActive: () => {
          if (!this.active || this.phase !== 'ready') return false;
          try {
            this.guardClock();
            return true;
          } catch {
            return false;
          }
        },
      });
      if (action.outcome === 'rejected') {
        this.lastDiagnostic = boundedBrowserSourceDiagnostic(action.diagnostic) ?? this.lastDiagnostic;
        if (!this.active || this.phase !== 'ready') throw new BrowserSourceControllerError('stopped');
        return rejected(action.failureCode);
      }
      gestureCompleted = true;
      if (!this.active || this.phase !== 'ready') {
        const submittedAt = this.mapMonotonicTime(action.submittedMonotonicMs, 'floor');
        const inputEnd = this.mapMonotonicTime(action.inputEndMonotonicMs, 'ceil');
        void this.retireForActionOutcome('source_unavailable').catch(() => undefined);
        return makeBrowserControlReply(submit, 'unknown', submittedAt, inputEnd, 'effect_unknown');
      }
      const submittedAt = this.mapMonotonicTime(action.submittedMonotonicMs, 'floor');
      const inputEnd = this.mapMonotonicTime(action.inputEndMonotonicMs, 'ceil');
      if (submittedAt < authority.command_start_ms || inputEnd < submittedAt
        || inputEnd > authority.command_end_ms || inputEnd > authority.reply_deadline_ms) {
        void this.retireForActionOutcome('source_unavailable').catch(() => undefined);
        return makeBrowserControlReply(submit, 'unknown', submittedAt, inputEnd, 'effect_unknown');
      }
      return makeBrowserControlReply(submit, 'completed', submittedAt, inputEnd, null);
    } catch (error) {
      this.lastDiagnostic = browserSourceDiagnosticFromError(error) ?? this.lastDiagnostic;
      if (gestureCompleted) {
        // Once CDP acknowledged the tap, a failed clock conversion cannot be
        // reported as Rejected: the physical input may already have happened.
        void this.retireForActionOutcome('source_unavailable').catch(() => undefined);
        throw error;
      }
      if (error instanceof OwnedBrowserActionOutcomeUnknown) {
        const submittedAt = this.mapMonotonicTime(error.submittedMonotonicMs, 'floor');
        const inputEnd = error.inputEndMonotonicMs === null ? null
          : this.mapMonotonicTime(error.inputEndMonotonicMs, 'ceil');
        void this.retireForActionOutcome('source_unavailable').catch(() => undefined);
        return makeBrowserControlReply(submit, 'unknown', submittedAt, inputEnd, 'effect_unknown');
      }
      if (error instanceof BrowserSourceControllerError && error.code === 'stopped') throw error;
      void this.retireForActionOutcome('source_unavailable').catch(() => undefined);
      return rejected('source_invalid');
    }
  }

  /**
   * Synchronous authority retirement for a Device-port timeout, close, or
   * ambiguous submitted gesture. Host invalidation and exact Chrome cleanup
   * continue asynchronously; callers must never await this from Stop.
   */
  retireForActionOutcome(reason: 'source_unavailable' | 'document_changed' | 'session_closed' | 'stopped'):
    Promise<void> {
    if (this.phase === 'stopped') return Promise.resolve();
    this.active = false;
    this.phase = reason === 'stopped' ? 'stopping' : 'failed';
    this.latestSnapshot = null;
    this.lastErrorCode = reason;
    const scope = this.installStarted ? this.scope : null;
    this.session?.retireNow();
    if (this.failureTask) return this.failureTask;
    this.failureTask = (async () => {
      if (scope) {
        try {
          await this.sendControl({
            kind: 'invalidate',
            scope_id: scope.scope_id,
            generation: scope.generation,
            reason: reason === 'stopped' ? 'stopped' : invalidationReason(reason),
          });
        } catch { /* Scope is locally retired even if the Host channel is gone. */ }
      }
      try { await this.closeOwnedSession(); }
      catch { this.lastErrorCode = 'source_unavailable'; }
      if (reason === 'stopped') this.phase = 'stopped';
    })();
    void this.failureTask.catch(() => undefined);
    return this.failureTask;
  }

  getStatus(): BrowserSourceStatus {
    return Object.freeze({
      phase: this.phase,
      generation: this.options.generation,
      scopeId: this.scope?.scope_id ?? null,
      lastSequence: this.latestSnapshot?.source_sequence ?? null,
      errorCode: this.lastErrorCode,
      diagnostic: this.lastDiagnostic,
    });
  }

  private async prepareOwnedSource(): Promise<BrowserReadScope> {
    try {
      this.assertActive();
      this.assertFixtureUnchanged();
      this.guardClock();
      const launchOptions: OwnedBrowserLaunchOptions = {
        chromeExecutable: this.options.chromeExecutable,
        fixturePath: this.options.fixturePath,
        profileParent: this.options.profileParent,
      };
      this.session = await this.ports.launchSession(launchOptions);
      this.assertActive();

      const initial = await this.readSnapshot();
      this.assertActive();
      await this.waitForLogicalBoundary(initial.read_finished_ms);
      await this.waitForHostBoundary(initial.read_finished_ms);
      const issuedMs = Math.floor(this.guardClock());
      await this.waitForHostBoundary(issuedMs);
      const scope: BrowserReadScope = {
        scope_id: randomUUID(),
        generation: this.options.generation,
        context: { ...initial.context },
        clock_domain: this.clockDomain,
        issued_ms: issuedMs,
      };
      const scopeValidation = validateBrowserReadScope(scope);
      if (!scopeValidation.ok) throw new BrowserSourceControllerError('source_unavailable');
      Object.freeze(scopeValidation.value.context);
      const installedScope = Object.freeze(scopeValidation.value);
      this.scope = installedScope;
      this.initialSnapshot = initial;
      this.phase = 'prepared';
      return installedScope;
    } catch (error) {
      await this.failUnlessStopping(error);
      throw error;
    }
  }

  private async activatePreparedSource(): Promise<BrowserPageSnapshot> {
    try {
      this.assertActive();
      const initial = this.initialSnapshot;
      const scope = this.scope;
      if (!initial || !scope || this.phase !== 'prepared') {
        throw new BrowserSourceControllerError('source_unavailable');
      }
      this.phase = 'starting';
      this.installStarted = true;
      await this.sendControl({ kind: 'install', scope });
      this.installAcknowledged = true;
      this.assertActive();

      const readFloor = Math.max(scope.issued_ms, initial.read_finished_ms);
      if (readFloor >= Number.MAX_SAFE_INTEGER) throw new BrowserSourceControllerError('source_unavailable');
      await this.waitForLogicalBoundary(readFloor + 1);
      const fresh = await this.readSnapshot();
      this.assertActive();
      this.requireFreshSnapshot(fresh, initial, readFloor + 1);
      await this.publish(fresh);
      this.phase = 'ready';
      return this.latestSnapshot!;
    } catch (error) {
      await this.failUnlessStopping(error);
      throw error;
    }
  }

  private async drainRefreshRequests(): Promise<BrowserPageSnapshot> {
    try {
      let latest = this.latestSnapshot;
      let supersededReads = 0;
      while (this.refreshRequested && this.active) {
        this.refreshRequested = false;
        const previous = this.latestSnapshot;
        if (!previous || !this.scope) throw new BrowserSourceControllerError('source_unavailable');
        const requestedAfter = this.refreshRequestedAfterMs ?? previous.read_finished_ms;
        this.refreshRequestedAfterMs = null;
        const readFloor = Math.max(previous.read_finished_ms, requestedAfter);
        if (readFloor >= Number.MAX_SAFE_INTEGER) throw new BrowserSourceControllerError('source_unavailable');
        await this.waitForLogicalBoundary(readFloor + 1);
        const fresh = await this.readSnapshot();
        this.assertActive();
        this.requireFreshSnapshot(fresh, previous, readFloor + 1);
        try {
          await this.publish(fresh);
        } catch (error) {
          if (!(error instanceof BrowserReadBeforeFloorError) || ++supersededReads > 2) throw error;
          // This read was never admitted. Keep the previous anchor and capture
          // a new read after the ACK and any pending Host freshness barrier.
          this.assertActive();
          this.refreshRequested = true;
          this.refreshRequestedAfterMs = Math.max(this.refreshRequestedAfterMs ?? 0,
            fresh.read_finished_ms, Math.floor(this.guardClock()),
            this.ports.clock.wallNowMs() - this.options.clockOriginUnixMs);
          continue;
        }
        latest = this.latestSnapshot;
      }
      if (!latest) throw new BrowserSourceControllerError('source_unavailable');
      return latest;
    } catch (error) {
      await this.failUnlessStopping(error);
      throw error;
    }
  }

  private async readSnapshot(): Promise<BrowserPageSnapshot> {
    this.assertActive();
    this.assertFixtureUnchanged();
    this.guardClock();
    const session = this.session;
    if (!session) throw new BrowserSourceControllerError('session_closed');
    const observation: HeadlessObservation = await session.observe();
    const afterRead = this.guardClock();
    this.assertFixtureUnchanged();
    this.assertActive();
    if (observation.document.url !== this.fixtureUrl) {
      throw new BrowserSourceControllerError('document_changed');
    }
    const snapshot = await createBrowserPageSnapshot(observation, {
      fixtureFileUrl: this.fixtureUrl,
      sourceClockDomain: HEADLESS_OBSERVATION_CLOCK_DOMAIN,
      targetClockDomain: this.clockDomain,
      sessionEpoch: this.options.sessionEpoch,
      mapMonotonicMs: (value) => this.ports.clock.timeOriginUnixMs + value - this.options.clockOriginUnixMs,
    });
    this.assertActive();
    if (snapshot.read_finished_ms > Math.ceil(afterRead)) {
      throw new BrowserSourceControllerError('source_unavailable');
    }
    return snapshot;
  }

  private requireFreshSnapshot(
    snapshot: BrowserPageSnapshot,
    previous: BrowserPageSnapshot,
    earliestObservedMs: number,
  ): void {
    if (!sameContext(snapshot.context, previous.context)
      || snapshot.target_context_hash !== previous.target_context_hash
      || snapshot.clock_domain !== this.clockDomain) {
      throw new BrowserSourceControllerError('document_changed');
    }
    if (snapshot.snapshot_id === previous.snapshot_id
      || snapshot.source_sequence <= previous.source_sequence
      || snapshot.observed_ms < earliestObservedMs
      || snapshot.read_finished_ms < snapshot.observed_ms) {
      throw new BrowserSourceControllerError('source_unavailable');
    }
  }

  private async publish(snapshot: BrowserPageSnapshot): Promise<void> {
    this.assertActive();
    if (!this.scope || !this.installAcknowledged) throw new BrowserSourceControllerError('source_unavailable');
    // Retain the captured interval. The Host's clock floors wall milliseconds,
    // so its current time must cover the original, rounded-up read completion.
    await this.waitForHostBoundary(snapshot.read_finished_ms);
    this.assertActive();
    const immutable = freezeSnapshot(snapshot);
    await this.sink.sendSnapshot(immutable);
    this.assertActive();
    this.latestSnapshot = immutable;
  }

  private assertFixtureUnchanged(): void {
    if (fixtureSha256(this.options.fixturePath) !== this.fixturePin) {
      throw new BrowserSourceControllerError('source_unavailable');
    }
  }

  /** Returns the trusted logical current time after checking the host wall clock against performance.now(). */
  private guardClock(): number {
    const monotonicMs = this.ports.clock.monotonicNowMs();
    const wallNowMs = this.ports.clock.wallNowMs();
    const performanceUnixMs = this.ports.clock.timeOriginUnixMs + monotonicMs;
    const logicalPerformanceMs = performanceUnixMs - this.options.clockOriginUnixMs;
    const logicalWallMs = wallNowMs - this.options.clockOriginUnixMs;
    if (!Number.isFinite(this.ports.clock.timeOriginUnixMs) || !Number.isFinite(monotonicMs)
      || !Number.isSafeInteger(wallNowMs) || !Number.isFinite(performanceUnixMs)
      || Math.abs(performanceUnixMs - wallNowMs) > MAX_CLOCK_DRIFT_MS
      || !Number.isSafeInteger(Math.floor(logicalPerformanceMs)) || logicalPerformanceMs < 0
      || !Number.isSafeInteger(logicalWallMs) || logicalWallMs < 0) {
      throw new BrowserSourceControllerError('source_unavailable');
    }
    return logicalPerformanceMs;
  }

  private mapMonotonicTime(value: number, rounding: 'floor' | 'ceil'): number {
    this.guardClock();
    if (!Number.isFinite(value) || value < 0) throw new BrowserSourceControllerError('source_unavailable');
    const logical = this.ports.clock.timeOriginUnixMs + value - this.options.clockOriginUnixMs;
    const mapped = Math[rounding](logical);
    if (!Number.isSafeInteger(mapped) || mapped < 0) throw new BrowserSourceControllerError('source_unavailable');
    return mapped;
  }

  private async waitForLogicalBoundary(targetMs: number): Promise<void> {
    if (!Number.isSafeInteger(targetMs) || targetMs < 0) throw new BrowserSourceControllerError('source_unavailable');
    for (let waitedMs = 0; waitedMs <= 1_000; waitedMs += 1) {
      this.assertActive();
      if (Math.floor(this.guardClock()) >= targetMs) return;
      await this.ports.clock.waitMs(1);
    }
    throw new BrowserSourceControllerError('source_unavailable');
  }

  private async waitForHostBoundary(targetMs: number): Promise<void> {
    if (!Number.isSafeInteger(targetMs) || targetMs < 0) throw new BrowserSourceControllerError('source_unavailable');
    for (let waitedMs = 0; waitedMs <= 1_000; waitedMs += 1) {
      this.assertActive();
      this.guardClock();
      if (this.ports.clock.wallNowMs() - this.options.clockOriginUnixMs >= targetMs) return;
      await this.ports.clock.waitMs(1);
    }
    throw new BrowserSourceControllerError('source_unavailable');
  }

  private assertActive(): void {
    if (!this.active) throw new BrowserSourceControllerError('stopped');
  }

  private async failUnlessStopping(error: unknown): Promise<void> {
    if (this.phase === 'stopping' || this.phase === 'stopped') return;
    if (this.failureTask) return this.failureTask;
    this.lastDiagnostic = browserSourceDiagnosticFromError(error) ?? this.lastDiagnostic;
    this.active = false;
    this.phase = 'failed';
    this.latestSnapshot = null;
    this.lastErrorCode = sourceErrorCode(error);
    this.failureTask = (async () => {
      const scope = this.installStarted ? this.scope : null;
      if (scope) {
        const control: BrowserSourceControl = {
          kind: 'invalidate',
          scope_id: scope.scope_id,
          generation: scope.generation,
          reason: invalidationReason(this.lastErrorCode ?? 'source_unavailable'),
        };
        try { await this.sendControl(control); } catch { /* Cleanup still owns the exact launched session. */ }
      }
      try { await this.closeOwnedSession(); }
      catch { this.lastErrorCode = 'source_unavailable'; }
    })();
    return this.failureTask;
  }

  private stopOwnedSource(scope: BrowserReadScope | null, installWasPending: boolean): Promise<void> {
    return (async () => {
      let invalidationError: unknown;
      if (scope) {
        try {
          await this.sendControl({
            kind: 'invalidate', scope_id: scope.scope_id, generation: scope.generation, reason: 'stopped',
          });
        } catch (error) {
          invalidationError = error;
        }
      }

      const pending: Promise<unknown>[] = [];
      if (this.prepareTask) pending.push(this.prepareTask);
      if (this.startTask) pending.push(this.startTask);
      if (this.refreshTask) pending.push(this.refreshTask);
      await Promise.allSettled(pending);
      if (scope && installWasPending) {
        try {
          await this.sendControl({
            kind: 'invalidate', scope_id: scope.scope_id, generation: scope.generation, reason: 'stopped',
          });
          invalidationError = undefined;
        } catch (error) {
          invalidationError ??= error;
        }
      }
      let cleanupError: unknown;
      try { await this.closeOwnedSession(); }
      catch (error) { cleanupError = error; }
      this.phase = 'stopped';
      if (invalidationError || cleanupError) {
        this.lastErrorCode = 'source_unavailable';
        throw new BrowserSourceControllerError('source_unavailable');
      }
    })();
  }

  private closeOwnedSession(): Promise<void> {
    if (this.sessionCloseTask) return this.sessionCloseTask;
    const session = this.session;
    if (!session) return Promise.resolve();
    this.sessionCloseTask = session.close().finally(() => {
      if (this.session === session) this.session = null;
    });
    return this.sessionCloseTask;
  }

  private async sendControl(control: BrowserSourceControl): Promise<void> {
    const validation = validateBrowserSourceControl(control);
    if (!validation.ok) throw new BrowserSourceControllerError('source_unavailable');
    await this.sink.sendControl(validation.value);
  }
}
