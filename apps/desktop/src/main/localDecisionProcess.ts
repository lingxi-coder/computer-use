import type { DecisionCancel, DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';
import { unavailable, type Delivery, type DropReason } from '@computer-use/decision-worker';
import { lstatSync } from 'node:fs';
import type { DecisionRunner } from './decisionRelay.js';
import {
  assertUtilityConfig,
  decodeUtilityFrame,
  encodeUtilityFrame,
  toUtilityTransferable,
  UTILITY_PROTOCOL_VERSION,
  type UtilityFailureCode,
  type UtilityMainMessage,
  type UtilityWorkerMessage,
} from '../../../../packages/decision-worker/src/utilityProtocol.js';

const MAX_QUEUED_DECISIONS = 4;
const MAX_REMEMBERED_DECISIONS = 1_024;
const DEFAULT_READY_TIMEOUT_MS = 30_000;
const DEFAULT_ACTIVE_DRAIN_TIMEOUT_MS = 5_000;
const CLOSE_GRACE_MS = 750;
const CLOSE_KILL_WAIT_MS = 750;
const MAX_TIMER_DELAY_MS = 2_147_000_000;

type UtilityProcessHandle = {
  on(event: 'message', listener: (message: unknown) => void): unknown;
  on(event: 'spawn', listener: () => void): unknown;
  on(event: 'exit', listener: (code: number) => void): unknown;
  on(event: 'error', listener: (...args: unknown[]) => void): unknown;
  postMessage(message: ArrayBuffer): void;
  kill(): boolean;
  pid?: number;
};

type UtilityProcessSpawner = (
  modulePath: string,
  args: string[],
  options: { stdio: 'ignore'; serviceName: string; env: Record<string, string> },
) => UtilityProcessHandle | Promise<UtilityProcessHandle>;

export interface LocalDecisionProcessOptions {
  /** Main-derived path to the electron-vite localDecisionWorker.js output. */
  workerModulePath: string;
  /** Main-private, pinned package-manifest.json path. Never accepted from a renderer. */
  bundleManifestPath: string;
  /** Main's shared-clock projection, used to enforce each request deadline. */
  nowMs: () => number;
  numThreads?: number;
  minimumConfidenceMilli?: number;
  readyTimeoutMs?: number;
  /** Test-only override for the fixed bound on an aborted active native inference. */
  activeDrainTimeoutMs?: number;
  /** Test seam. Production uses Electron utilityProcess.fork with the fixed output. */
  spawnUtilityProcess?: UtilityProcessSpawner;
}

type ProcessState = 'starting' | 'ready' | 'unavailable' | 'closed';

interface RootFence {
  rootTaskId: string;
  rootEpoch: number;
  taskRevision: number;
}

interface PendingRequest {
  request_id: number;
  epoch: number;
  request: DecisionRequest;
  resolve: (delivery: Delivery) => void;
  timer: NodeJS.Timeout | null;
  drainTimer: NodeJS.Timeout | null;
  settled: boolean;
  sent: boolean;
}

function minimalWorkerEnvironment(): Record<string, string> {
  const allowlisted = ['PATH', 'HOME', 'TMPDIR', 'LANG', 'LC_ALL', 'DYLD_LIBRARY_PATH', 'DYLD_FALLBACK_LIBRARY_PATH'];
  const env: Record<string, string> = {};
  for (const key of allowlisted) {
    const value = process.env[key];
    if (value !== undefined) env[key] = value;
  }
  return env;
}

async function spawnElectronUtilityProcess(
  modulePath: string,
  args: string[],
  options: { stdio: 'ignore'; serviceName: string; env: Record<string, string> },
): Promise<UtilityProcessHandle> {
  const { utilityProcess } = await import('electron');
  return utilityProcess.fork(modulePath, args, options) as unknown as UtilityProcessHandle;
}

function rejected(reason: DropReason, request: DecisionRequest): Delivery {
  return {
    kind: 'dropped',
    reason,
    decisionId: request.decision_id,
    generation: request.request_generation,
  };
}

function invalidRequestDrop(request: DecisionRequest): Delivery {
  const value = request as unknown as Record<string, unknown>;
  const id = typeof value?.decision_id === 'string' && value.decision_id.length > 0 && value.decision_id.length <= 256
    ? value.decision_id
    : 'invalid-decision';
  const generation = typeof value?.request_generation === 'number'
    && Number.isSafeInteger(value.request_generation) && value.request_generation >= 0
    ? value.request_generation
    : 0;
  return { kind: 'dropped', reason: 'cancelled', decisionId: id, generation };
}

function fixedUnavailable(request: DecisionRequest): Delivery {
  return {
    kind: 'proposal',
    proposal: unavailable(request, 'local-s1-unavailable', 's1'),
  };
}

function validClockValue(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 0;
}

/**
 * The Main-owned lifecycle for the isolated local S1 planner. A utility exit is
 * terminal for this instance: pending work is dropped and the process is never
 * silently recreated or replayed.
 */
export class LocalDecisionProcess implements DecisionRunner {
  private state: ProcessState = 'starting';
  private failureCode: UtilityFailureCode | null = null;
  private modelRevision: string | null = null;
  private child: UtilityProcessHandle | null = null;
  private epoch = 1;
  private workerEpoch = 1;
  private nextRequestId = 1;
  private readonly latestGeneration = new Map<string, number>();
  private readonly queue: PendingRequest[] = [];
  private active: PendingRequest | null = null;
  private rootFence: RootFence | null = null;
  private readyTimer: NodeJS.Timeout | null = null;
  private initSent = false;
  private resolveStarted: (() => void) | null = null;
  private readonly started: Promise<void>;
  private closeTask: Promise<void> | null = null;
  private readonly config: {
    workerModulePath: string;
    bundleManifestPath: string;
    numThreads: number;
    minimumConfidenceMilli: number;
    nowMs: () => number;
    readyTimeoutMs: number;
    activeDrainTimeoutMs: number;
    spawn: UtilityProcessSpawner;
    hasInjectedSpawner: boolean;
  };

  private constructor(options: LocalDecisionProcessOptions) {
    this.config = {
      workerModulePath: options.workerModulePath,
      bundleManifestPath: options.bundleManifestPath,
      numThreads: options.numThreads ?? 1,
      minimumConfidenceMilli: options.minimumConfidenceMilli ?? 800,
      nowMs: options.nowMs,
      readyTimeoutMs: options.readyTimeoutMs ?? DEFAULT_READY_TIMEOUT_MS,
      activeDrainTimeoutMs: options.activeDrainTimeoutMs ?? DEFAULT_ACTIVE_DRAIN_TIMEOUT_MS,
      spawn: options.spawnUtilityProcess ?? spawnElectronUtilityProcess,
      hasInjectedSpawner: options.spawnUtilityProcess !== undefined,
    };
    this.started = new Promise((resolve) => { this.resolveStarted = resolve; });
  }

  static async start(options: LocalDecisionProcessOptions): Promise<LocalDecisionProcess> {
    const runner = LocalDecisionProcess.open(options);
    await runner.started;
    return runner;
  }

  /** Main keeps the lifecycle handle while the model loads, so Stop can close it immediately. */
  static open(options: LocalDecisionProcessOptions): LocalDecisionProcess {
    const runner = new LocalDecisionProcess(options);
    void runner.launch().catch(() => runner.becomeUnavailable('worker_spawn_failed', true));
    return runner;
  }

  get availability(): 'starting' | 'ready' | 'unavailable' | 'closed' {
    return this.state;
  }

  get unavailableCode(): UtilityFailureCode | null {
    return this.failureCode;
  }

  get loadedModelRevision(): string | null {
    return this.modelRevision;
  }

  get inFlight(): number {
    return (this.active ? 1 : 0) + this.queue.length;
  }

  private async launch(): Promise<void> {
    try {
      assertUtilityConfig({
        workerModulePath: this.config.workerModulePath,
        bundleManifestPath: this.config.bundleManifestPath,
        numThreads: this.config.numThreads,
        minimumConfidenceMilli: this.config.minimumConfidenceMilli,
      });
      if (!this.config.hasInjectedSpawner && !lstatSync(this.config.workerModulePath).isFile()) {
        throw new Error('invalid_config');
      }
      if (!Number.isSafeInteger(this.config.readyTimeoutMs)
        || this.config.readyTimeoutMs < 1 || this.config.readyTimeoutMs > MAX_TIMER_DELAY_MS) {
        throw new Error('invalid_config');
      }
      if (!Number.isSafeInteger(this.config.activeDrainTimeoutMs)
        || this.config.activeDrainTimeoutMs < 1 || this.config.activeDrainTimeoutMs > MAX_TIMER_DELAY_MS) {
        throw new Error('invalid_config');
      }
    } catch {
      this.becomeUnavailable('invalid_config', false);
      return;
    }

    let child: UtilityProcessHandle;
    try {
      child = await this.config.spawn(this.config.workerModulePath, [], {
        stdio: 'ignore',
        serviceName: 'Computer Use local S1 planner',
        env: minimalWorkerEnvironment(),
      });
    } catch {
      this.becomeUnavailable('worker_spawn_failed', false);
      return;
    }
    if (this.state === 'closed') {
      child.kill();
      return;
    }
    this.child = child;
    child.on('message', (message) => this.onMessage(message));
    child.on('spawn', () => this.sendInit());
    child.on('exit', () => this.onExit());
    child.on('error', () => this.onProcessError());
    if (child.pid !== undefined) this.sendInit();
    this.readyTimer = setTimeout(() => this.becomeUnavailable('worker_timeout', true), this.config.readyTimeoutMs);
    this.readyTimer.unref?.();
    await this.started;
  }

  private resolveStart(): void {
    this.clearReadyTimer();
    this.resolveStarted?.();
    this.resolveStarted = null;
  }

  private clearReadyTimer(): void {
    if (this.readyTimer) clearTimeout(this.readyTimer);
    this.readyTimer = null;
  }

  private sendInit(): void {
    if (this.state !== 'starting' || this.initSent) return;
    this.initSent = true;
    this.send({
      version: UTILITY_PROTOCOL_VERSION,
      type: 'init',
      bundleManifestPath: this.config.bundleManifestPath,
      numThreads: this.config.numThreads,
      minimumConfidenceMilli: this.config.minimumConfidenceMilli,
    });
  }

  private send(message: UtilityMainMessage): boolean {
    if (!this.child || this.state === 'closed') return false;
    try {
      const frame = encodeUtilityFrame(message);
      this.child.postMessage(toUtilityTransferable(frame));
      return true;
    } catch {
      this.becomeUnavailable('worker_exit', true);
      return false;
    }
  }

  private onMessage(value: unknown): void {
    if (this.state === 'closed' || this.state === 'unavailable') return;
    let message: UtilityWorkerMessage;
    try {
      message = decodeUtilityFrame(value) as UtilityWorkerMessage;
    } catch {
      this.becomeUnavailable('invalid_message', true);
      return;
    }
    if (message.type === 'ready') {
      if (this.state !== 'starting') {
        this.becomeUnavailable('invalid_message', true);
        return;
      }
      this.state = 'ready';
      this.modelRevision = message.model_revision;
      this.resolveStart();
      this.syncWorkerEpoch();
      this.pump();
      return;
    }
    if (message.type === 'unavailable' && !('request_id' in message)) {
      if (this.state !== 'starting') {
        this.becomeUnavailable('invalid_message', true);
        return;
      }
      this.becomeUnavailable(message.code, false);
      return;
    }
    if (message.type !== 'proposal' && message.type !== 'unavailable') {
      this.becomeUnavailable('invalid_message', true);
      return;
    }
    this.onRequestResult(message);
  }

  private onRequestResult(
    message: Extract<UtilityWorkerMessage, { type: 'proposal' | 'unavailable' }> & { request_id: number; epoch: number; proposal: DecisionProposal },
  ): void {
    const active = this.active;
    if (!active) return; // Late messages after a terminal fence are discarded.
    if (message.request_id !== active.request_id || message.epoch !== active.epoch) {
      this.becomeUnavailable('invalid_message', true);
      return;
    }
    this.active = null;
    if (active.timer) clearTimeout(active.timer);
    active.timer = null;
    if (active.drainTimer) clearTimeout(active.drainTimer);
    active.drainTimer = null;
    if (!active.settled) {
      const currentNow = this.currentTime();
      if (active.epoch !== this.epoch) {
        this.settle(active, rejected('stale_epoch', active.request));
      } else if (currentNow === null || currentNow >= active.request.result_deadline_ms) {
        this.settle(active, rejected('cancelled', active.request));
      } else if (!this.matchesRequest(active.request, message.proposal)) {
        this.becomeUnavailable('invalid_proposal', true);
        this.settle(active, rejected('mismatched_identity', active.request));
      } else {
        this.settle(active, { kind: 'proposal', proposal: message.proposal });
      }
    }
    this.pump();
  }

  private matchesRequest(request: DecisionRequest, proposal: DecisionProposal): boolean {
    return proposal.source === 's1'
      && proposal.decision_id === request.decision_id
      && proposal.request_generation === request.request_generation
      && proposal.candidate_set_digest === request.state.candidate_set_digest;
  }

  private syncWorkerEpoch(): void {
    while (this.state === 'ready' && this.workerEpoch < this.epoch) {
      const next = this.workerEpoch + 1;
      if (!this.send({ version: UTILITY_PROTOCOL_VERSION, type: 'invalidate', epoch: next })) return;
      this.workerEpoch = next;
    }
  }

  private currentTime(): number | null {
    try {
      const now = this.config.nowMs();
      return validClockValue(now) ? now : null;
    } catch {
      return null;
    }
  }

  private rememberGeneration(request: DecisionRequest): boolean {
    const newest = this.latestGeneration.get(request.decision_id);
    if (newest !== undefined && newest >= request.request_generation) return false;
    if (newest === undefined && this.latestGeneration.size >= MAX_REMEMBERED_DECISIONS) return false;
    this.latestGeneration.set(request.decision_id, request.request_generation);
    return true;
  }

  private sameRoot(request: DecisionRequest): boolean {
    return this.rootFence === null
      || (this.rootFence.rootTaskId === request.root_task_id && this.rootFence.rootEpoch === request.root_epoch);
  }

  private acceptRootRevision(request: DecisionRequest): boolean {
    if (!this.sameRoot(request)) return false;
    if (!this.rootFence) {
      this.rootFence = {
        rootTaskId: request.root_task_id,
        rootEpoch: request.root_epoch,
        taskRevision: request.task_revision,
      };
      return true;
    }
    if (request.task_revision < this.rootFence.taskRevision) return false;
    if (request.task_revision > this.rootFence.taskRevision) {
      const nextFence = { ...this.rootFence, taskRevision: request.task_revision };
      this.invalidate();
      this.rootFence = nextFence;
    }
    return true;
  }

  submit(value: DecisionRequest): Promise<Delivery> {
    let request: DecisionRequest;
    try {
      const checked = decodeUtilityFrame(encodeUtilityFrame({
        version: UTILITY_PROTOCOL_VERSION,
        type: 'request',
        request_id: 1,
        epoch: this.epoch,
        request: value,
      }));
      if (checked.type !== 'request') return Promise.resolve(invalidRequestDrop(value));
      request = checked.request;
    } catch {
      return Promise.resolve(invalidRequestDrop(value));
    }

    if (this.state === 'closed') return Promise.resolve(rejected('cancelled', request));
    const now = this.currentTime();
    if (now === null || now >= request.result_deadline_ms) return Promise.resolve(rejected('cancelled', request));
    if (!this.acceptRootRevision(request)) return Promise.resolve(rejected('stale_epoch', request));
    if (!this.rememberGeneration(request)) {
      return Promise.resolve(this.latestGeneration.has(request.decision_id)
        ? rejected('stale_generation', request)
        : fixedUnavailable(request));
    }

    this.supersedeOlder(request);
    if (this.state === 'unavailable') return Promise.resolve(fixedUnavailable(request));
    if (this.inFlight >= MAX_QUEUED_DECISIONS) return Promise.resolve(fixedUnavailable(request));

    const requestId = this.takeRequestId();
    if (requestId === null) {
      this.becomeUnavailable('worker_exit', true);
      return Promise.resolve(rejected('stale_epoch', request));
    }
    return new Promise<Delivery>((resolve) => {
      const pending: PendingRequest = {
        request_id: requestId,
        epoch: this.epoch,
        request,
        resolve,
        timer: null,
        drainTimer: null,
        settled: false,
        sent: false,
      };
      this.scheduleDeadline(pending);
      if (pending.settled) return;
      this.queue.push(pending);
      this.pump();
    });
  }

  private takeRequestId(): number | null {
    if (!Number.isSafeInteger(this.nextRequestId) || this.nextRequestId < 1) return null;
    const id = this.nextRequestId;
    this.nextRequestId += 1;
    return id;
  }

  private scheduleDeadline(pending: PendingRequest): void {
    const schedule = (): void => {
      if (pending.settled && this.active !== pending) return;
      const now = this.currentTime();
      if (now === null || now >= pending.request.result_deadline_ms) {
        if (this.active === pending) {
          this.settle(pending, rejected('cancelled', pending.request));
          this.send({
            version: UTILITY_PROTOCOL_VERSION,
            type: 'cancel',
            request_id: pending.request_id,
            decision_id: pending.request.decision_id,
            request_generation: pending.request.request_generation,
          });
          this.becomeUnavailable('worker_timeout', true);
        } else {
          this.removeQueued(pending);
          this.settle(pending, rejected('cancelled', pending.request));
        }
        return;
      }
      const delay = Math.max(1, Math.min(MAX_TIMER_DELAY_MS, pending.request.result_deadline_ms - now));
      pending.timer = setTimeout(schedule, delay);
      pending.timer.unref?.();
    };
    schedule();
  }

  private pump(): void {
    if (this.state !== 'ready' || this.active || !this.child) return;
    while (this.queue.length > 0) {
      const pending = this.queue.shift()!;
      if (pending.settled) continue;
      if (pending.epoch !== this.epoch) {
        this.settle(pending, rejected('stale_epoch', pending.request));
        continue;
      }
      if (!this.send({
        version: UTILITY_PROTOCOL_VERSION,
        type: 'request',
        request_id: pending.request_id,
        epoch: pending.epoch,
        request: pending.request,
      })) {
        this.settle(pending, rejected('stale_epoch', pending.request));
        return;
      }
      pending.sent = true;
      this.active = pending;
      return;
    }
  }

  cancel(value: DecisionCancel): void {
    let cancel: DecisionCancel;
    try {
      const checked = decodeUtilityFrame(encodeUtilityFrame({
        version: UTILITY_PROTOCOL_VERSION,
        type: 'cancel',
        request_id: 1,
        decision_id: value.decision_id,
        request_generation: value.request_generation,
      }));
      if (checked.type !== 'cancel') return;
      cancel = checked;
    } catch {
      return;
    }
    for (const queued of [...this.queue]) {
      if (queued.request.decision_id === cancel.decision_id
        && cancel.request_generation >= queued.request.request_generation) {
        this.removeQueued(queued);
        this.settle(queued, rejected('cancelled', queued.request));
      }
    }
    const active = this.active;
    if (active && active.request.decision_id === cancel.decision_id
      && cancel.request_generation >= active.request.request_generation) {
      this.retireActive(active, rejected('cancelled', active.request));
      this.send({
        version: UTILITY_PROTOCOL_VERSION,
        type: 'cancel',
        request_id: active.request_id,
        decision_id: cancel.decision_id,
        request_generation: cancel.request_generation,
      });
    }
    this.pump();
  }

  invalidate(): number {
    this.rootFence = null;
    if (this.epoch >= Number.MAX_SAFE_INTEGER) {
      this.becomeUnavailable('worker_exit', true);
      return this.epoch;
    }
    this.epoch += 1;
    this.invalidatePending();
    if (this.state === 'ready') {
      if (this.send({ version: UTILITY_PROTOCOL_VERSION, type: 'invalidate', epoch: this.epoch })) {
        this.workerEpoch = this.epoch;
      }
    }
    return this.epoch;
  }

  private invalidatePending(): void {
    for (const queued of this.queue.splice(0)) this.settle(queued, rejected('stale_epoch', queued.request));
    if (this.active) {
      this.retireActive(this.active, rejected('stale_epoch', this.active.request));
    }
  }

  private supersedeOlder(request: DecisionRequest): void {
    for (const queued of [...this.queue]) {
      if (queued.request.decision_id === request.decision_id
        && request.request_generation > queued.request.request_generation) {
        this.removeQueued(queued);
        this.settle(queued, rejected('superseded', queued.request));
      }
    }
    const active = this.active;
    if (active && active.request.decision_id === request.decision_id
      && request.request_generation > active.request.request_generation) {
      this.retireActive(active, rejected('superseded', active.request));
      this.send({
        version: UTILITY_PROTOCOL_VERSION,
        type: 'cancel',
        request_id: active.request_id,
        decision_id: request.decision_id,
        request_generation: request.request_generation,
      });
    }
  }

  private removeQueued(pending: PendingRequest): void {
    const index = this.queue.indexOf(pending);
    if (index >= 0) this.queue.splice(index, 1);
  }

  private settle(pending: PendingRequest, delivery: Delivery, retainActiveDeadline = false): void {
    if (pending.settled) return;
    pending.settled = true;
    if (!retainActiveDeadline) {
      if (pending.timer) clearTimeout(pending.timer);
      pending.timer = null;
      if (pending.drainTimer) clearTimeout(pending.drainTimer);
      pending.drainTimer = null;
    }
    pending.resolve(delivery);
  }

  private retireActive(pending: PendingRequest, delivery: Delivery): void {
    this.settle(pending, delivery, true);
    if (pending.drainTimer || this.active !== pending) return;
    pending.drainTimer = setTimeout(() => {
      if (this.active === pending) this.becomeUnavailable('worker_timeout', true);
    }, this.config.activeDrainTimeoutMs);
    pending.drainTimer.unref?.();
  }

  private becomeUnavailable(code: UtilityFailureCode, terminate: boolean): void {
    if (this.state === 'closed' || this.state === 'unavailable') return;
    const wasStarting = this.state === 'starting';
    this.state = 'unavailable';
    this.failureCode = code;
    this.modelRevision = null;
    this.clearReadyTimer();
    if (this.epoch < Number.MAX_SAFE_INTEGER) this.epoch += 1;
    this.invalidatePending();
    this.clearActiveDeadline();
    this.active = null;
    if (terminate) {
      try { this.child?.kill(); } catch { /* fixed failure state */ }
    }
    if (wasStarting) this.resolveStart();
  }

  private onExit(): void {
    this.child = null;
    if (this.state === 'closed' || this.state === 'unavailable') {
      this.active = null;
      return;
    }
    this.becomeUnavailable('worker_exit', false);
  }

  private onProcessError(): void {
    if (this.state === 'closed' || this.state === 'unavailable') return;
    this.becomeUnavailable(this.state === 'starting' ? 'worker_spawn_failed' : 'worker_exit', true);
  }

  async close(): Promise<void> {
    if (this.closeTask) return this.closeTask;
    this.closeTask = this.closeInner();
    return this.closeTask;
  }

  private async closeInner(): Promise<void> {
    if (this.state === 'closed') return;
    const child = this.child;
    const wasStarting = this.state === 'starting';
    this.state = 'closed';
    this.failureCode = 'closed';
    this.clearReadyTimer();
    if (this.epoch < Number.MAX_SAFE_INTEGER) this.epoch += 1;
    this.invalidatePending();
    this.clearActiveDeadline();
    this.active = null;
    if (wasStarting) this.resolveStart();
    if (!child) return;

    const exited = new Promise<void>((resolve) => {
      child.on('exit', () => resolve());
    });
    try {
      const frame = encodeUtilityFrame({ version: UTILITY_PROTOCOL_VERSION, type: 'close' });
      child.postMessage(toUtilityTransferable(frame));
    } catch {
      try { child.kill(); } catch { /* bounded close continues */ }
    }
    const graceful = new Promise<void>((resolve) => {
      const timeout = setTimeout(resolve, CLOSE_GRACE_MS);
      timeout.unref?.();
    });
    await Promise.race([exited, graceful]);
    try { child.kill(); } catch { /* bounded close continues */ }
    const forced = new Promise<void>((resolve) => {
      const timeout = setTimeout(resolve, CLOSE_KILL_WAIT_MS);
      timeout.unref?.();
    });
    await Promise.race([exited, forced]);
    this.child = null;
  }

  private clearActiveDeadline(): void {
    if (this.active?.timer) clearTimeout(this.active.timer);
    if (this.active?.drainTimer) clearTimeout(this.active.drainTimer);
    if (this.active) {
      this.active.timer = null;
      this.active.drainTimer = null;
    }
  }
}
