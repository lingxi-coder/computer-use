import { performance } from 'node:perf_hooks';

import type { DecisionCancel, DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';
import type { Delivery } from '@computer-use/decision-worker';

export interface DecisionRunner {
  submit(request: DecisionRequest): Promise<Delivery>;
  cancel(cancel: DecisionCancel): void;
  invalidate(): number;
}

export interface ClosableDecisionRunner extends DecisionRunner {
  close?(): void | Promise<void>;
}

export interface ShadowDecisionStats {
  readonly submitted: number;
  readonly completed: number;
  readonly unavailable: number;
  readonly refused: number;
  readonly dropped: number;
  readonly disagreements: number;
  readonly latency_count: number;
  readonly latency_total_ms: number;
  readonly latency_max_ms: number;
}

export interface ShadowDecisionRunnerOptions {
  /** Maximum shadow observations alive at once. Excess work is refused, never queued. */
  maxInFlight?: number;
  /** Bound waiting for the shadow runner's asynchronous close. */
  closeTimeoutMs?: number;
  /** Monotonic clock injection for deterministic tests. */
  clock?: () => number;
}

interface MutableStats {
  submitted: number;
  completed: number;
  unavailable: number;
  refused: number;
  dropped: number;
  disagreements: number;
  latency_count: number;
  latency_total_ms: number;
  latency_max_ms: number;
}

interface ShadowTask {
  readonly epoch: number;
  readonly decisionId: string;
  readonly generation: number;
  readonly candidateSetDigest: string;
  startedAt: number | null;
  invalidated: boolean;
  cancelled: boolean;
  primarySettled: boolean;
  primaryDelivery?: Delivery;
  shadowSettled: boolean;
  shadowDelivery?: Delivery;
  shadowFailed: boolean;
}

const DEFAULT_MAX_IN_FLIGHT = 2;
const MAX_IN_FLIGHT = 16;
const DEFAULT_CLOSE_TIMEOUT_MS = 100;
const MAX_CLOSE_TIMEOUT_MS = 500;
const EMPTY_STATS: MutableStats = {
  submitted: 0, completed: 0, unavailable: 0, refused: 0, dropped: 0,
  disagreements: 0, latency_count: 0, latency_total_ms: 0, latency_max_ms: 0,
};

/**
 * Observes a second decision runner without changing the decision returned to
 * the caller. Shadow work is concurrent, bounded, cancelable and never sent
 * through the host transport.
 */
export class ShadowDecisionRunner implements DecisionRunner {
  private readonly tasks = new Set<ShadowTask>();
  private readonly stats: MutableStats = { ...EMPTY_STATS };
  private readonly maxInFlight: number;
  private readonly closeTimeoutMs: number;
  private readonly clock: () => number;
  private epoch = 0;
  private closed = false;
  private closeTask: Promise<void> | null = null;

  constructor(
    private readonly primary: DecisionRunner,
    private readonly shadow: ClosableDecisionRunner,
    options: ShadowDecisionRunnerOptions = {},
  ) {
    this.maxInFlight = boundedInteger(options.maxInFlight, DEFAULT_MAX_IN_FLIGHT, 1, MAX_IN_FLIGHT);
    this.closeTimeoutMs = boundedInteger(options.closeTimeoutMs, DEFAULT_CLOSE_TIMEOUT_MS, 0, MAX_CLOSE_TIMEOUT_MS);
    this.clock = options.clock ?? (() => performance.now());
  }

  get statistics(): ShadowDecisionStats {
    return Object.freeze({ ...this.stats });
  }

  get shadowInFlight(): number { return this.tasks.size; }

  submit(request: DecisionRequest): Promise<Delivery> {
    const reservation = this.reserveShadowObservation(request);
    let primaryDelivery: Promise<Delivery>;
    try { primaryDelivery = this.primary.submit(request); }
    catch (error) {
      if (reservation) {
        reservation.task.invalidated = true;
        this.tasks.delete(reservation.task);
      }
      throw error;
    }
    if (reservation) this.startShadowObservation(reservation.task, reservation.request, primaryDelivery);
    return primaryDelivery;
  }

  cancel(cancel: DecisionCancel): void {
    for (const task of this.tasks) {
      if (task.decisionId === cancel.decision_id && cancel.request_generation >= task.generation) {
        task.cancelled = true;
      }
    }
    try { this.primary.cancel(cancel); }
    finally { try { this.shadow.cancel(cancel); } catch { /* Observer failures never affect primary control. */ } }
  }

  invalidate(): number {
    this.epoch += 1;
    for (const task of this.tasks) task.invalidated = true;
    try { return this.primary.invalidate(); }
    finally { try { this.shadow.invalidate(); } catch { /* Observer failures never affect primary invalidation. */ } }
  }

  /** Invalidates first, then waits only a bounded time for optional shadow cleanup. */
  close(): Promise<void> {
    if (this.closeTask) return this.closeTask;
    this.closed = true;
    let invalidationFailed = false;
    let invalidationError: unknown;
    try { this.invalidate(); }
    catch (error) {
      invalidationFailed = true;
      invalidationError = error;
    }
    this.closeTask = new Promise<void>((resolve, reject) => {
      let finished = false;
      const finish = (): void => {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        if (invalidationFailed) reject(invalidationError);
        else resolve();
      };
      const timer = setTimeout(finish, this.closeTimeoutMs);
      void Promise.resolve().then(() => this.shadow.close?.()).then(finish, finish);
    });
    return this.closeTask;
  }

  private reserveShadowObservation(request: DecisionRequest): { task: ShadowTask; request: DecisionRequest } | null {
    if (this.closed || this.tasks.size >= this.maxInFlight) {
      increment(this.stats, 'refused');
      return null;
    }

    let shadowRequest: DecisionRequest;
    try { shadowRequest = structuredClone(request); }
    catch {
      increment(this.stats, 'refused');
      return null;
    }

    const task: ShadowTask = {
      epoch: this.epoch,
      decisionId: request.decision_id,
      generation: request.request_generation,
      candidateSetDigest: request.state.candidate_set_digest,
      startedAt: null,
      invalidated: false,
      cancelled: false,
      primarySettled: false,
      shadowSettled: false,
      shadowFailed: false,
    };
    for (const older of this.tasks) {
      if (older.decisionId === task.decisionId && task.generation >= older.generation) {
        older.invalidated = true;
        try { this.shadow.cancel({ decision_id: older.decisionId, request_generation: older.generation }); }
        catch { /* Superseded observer work cannot affect the primary result. */ }
      }
    }
    this.tasks.add(task);
    return { task, request: shadowRequest };
  }

  private startShadowObservation(task: ShadowTask, shadowRequest: DecisionRequest, primaryPromise: Promise<Delivery>): void {
    if (task.invalidated || task.cancelled || task.epoch !== this.epoch || this.closed) {
      this.tasks.delete(task);
      return;
    }
    void primaryPromise.then(
      (delivery) => { task.primaryDelivery = delivery; task.primarySettled = true; this.finishIfReady(task); },
      () => { task.primarySettled = true; this.finishIfReady(task); },
    );

    // Defer even the runner's synchronous submit prelude so it cannot delay primary delivery.
    void Promise.resolve().then(() => {
      if (task.invalidated || task.cancelled || task.epoch !== this.epoch || this.closed) {
        task.shadowSettled = true;
        this.finishIfReady(task);
        return;
      }
      task.startedAt = this.readClock();
      increment(this.stats, 'submitted');
      let shadowPromise: Promise<Delivery>;
      try { shadowPromise = Promise.resolve(this.shadow.submit(shadowRequest)); }
      catch { shadowPromise = Promise.reject(new Error('shadow submit failed')); }
      void shadowPromise.then(
        (delivery) => {
          task.shadowDelivery = delivery;
          task.shadowSettled = true;
          this.finishIfReady(task);
        },
        () => {
          task.shadowFailed = true;
          task.shadowSettled = true;
          this.finishIfReady(task);
        },
      );
    }).catch(() => {
      task.shadowFailed = true;
      task.shadowSettled = true;
      this.finishIfReady(task);
    });
  }

  private finishIfReady(task: ShadowTask): void {
    if (!task.primarySettled || !task.shadowSettled || !this.tasks.has(task)) return;
    this.tasks.delete(task);
    if (task.invalidated || task.cancelled || task.epoch !== this.epoch || this.closed) return;

    const endedAt = this.readClock();
    if (task.startedAt !== null && endedAt !== null && endedAt >= task.startedAt) {
      const elapsed = endedAt - task.startedAt;
      increment(this.stats, 'latency_count');
      this.stats.latency_total_ms = boundedAdd(this.stats.latency_total_ms, elapsed);
      this.stats.latency_max_ms = Math.max(this.stats.latency_max_ms, elapsed);
    }

    if (task.shadowFailed || !task.shadowDelivery) {
      increment(this.stats, 'unavailable');
      return;
    }
    if (task.shadowDelivery.kind === 'dropped') {
      increment(this.stats, 'dropped');
      return;
    }

    increment(this.stats, 'completed');
    const proposal = task.shadowDelivery.proposal;
    if (proposal.failure === 'unavailable') increment(this.stats, 'unavailable');
    if (proposal.failure === 'refused') increment(this.stats, 'refused');

    if (task.primaryDelivery?.kind === 'proposal'
      && proposalMatchesTask(task.primaryDelivery.proposal, task)
      && proposalMatchesTask(proposal, task)
      && comparable(task.primaryDelivery.proposal) && comparable(proposal)
      && !sameDecision(task.primaryDelivery.proposal, proposal)) {
      increment(this.stats, 'disagreements');
    }
  }

  private readClock(): number | null {
    try {
      const value = this.clock();
      return Number.isFinite(value) ? value : null;
    } catch { return null; }
  }
}

export function createShadowDecisionRunner(
  primary: DecisionRunner,
  shadow: ClosableDecisionRunner,
  options?: ShadowDecisionRunnerOptions,
): ShadowDecisionRunner {
  return new ShadowDecisionRunner(primary, shadow, options);
}

function comparable(proposal: DecisionProposal): boolean { return proposal.failure == null; }

function sameDecision(left: DecisionProposal, right: DecisionProposal): boolean {
  if (!sameChoice(left.choice ?? null, right.choice ?? null) || left.answers.length !== right.answers.length) return false;
  for (let index = 0; index < left.answers.length; index += 1) {
    const a = left.answers[index]!;
    const b = right.answers[index]!;
    if (a.question_id !== b.question_id || (a.selected ?? null) !== (b.selected ?? null)) return false;
  }
  return true;
}

function proposalMatchesTask(proposal: DecisionProposal, task: ShadowTask): boolean {
  return proposal.decision_id === task.decisionId
    && proposal.request_generation === task.generation
    && proposal.candidate_set_digest === task.candidateSetDigest;
}

function sameChoice(left: DecisionProposal['choice'], right: DecisionProposal['choice']): boolean {
  if (left == null || right == null) return left == null && right == null;
  if (left.action !== right.action) return false;
  switch (left.action) {
    case 'start_skill': return right.action === left.action && right.candidate === left.candidate;
    case 'continue_skill': return right.action === left.action && right.attempt === left.attempt;
    case 'observe_more': return right.action === left.action && sameStrings(left.facts, right.facts);
    case 'request_planning':
    case 'stop_or_yield': return right.action === left.action && right.reason === left.reason;
  }
}

function sameStrings(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function boundedInteger(value: number | undefined, fallback: number, min: number, max: number): number {
  if (!Number.isSafeInteger(value)) return fallback;
  return Math.max(min, Math.min(max, value!));
}

function increment(stats: MutableStats, key: keyof MutableStats): void {
  const current = stats[key];
  if (typeof current === 'number' && Number.isSafeInteger(current) && current < Number.MAX_SAFE_INTEGER) {
    (stats[key] as number) = current + 1;
  }
}

function boundedAdd(current: number, addition: number): number {
  if (!Number.isFinite(addition) || addition < 0) return current;
  return Math.min(Number.MAX_SAFE_INTEGER, current + addition);
}
