import type { DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';
import { PROTOCOL_VERSION, validateEnvelope } from '@computer-use/desktop-api';
import type { Delivery } from '@computer-use/decision-worker';

export const TEACHER_OBSERVATION_MAX_FRAME_BYTES = 128 * 1024;
const MAX_BODY_BYTES = TEACHER_OBSERVATION_MAX_FRAME_BYTES - 4;

const MAX_MESSAGES_PER_PUSH = 16;
const MAX_ACTIVE_TASKS = 4;
const MAX_REMEMBERED_DECISIONS = 1024;
const MAX_REMEMBERED_ROOTS = 1024;
const MAX_TIMER_SLICE_MS = 1000;
const DEFAULT_DEADLINE_POLL_MS = 100;
const DEFAULT_CLOSE_TIMEOUT_MS = 100;
const MAX_CLOSE_TIMEOUT_MS = 500;
const UTF8 = new TextDecoder('utf-8', { fatal: true });
const IDENTIFIER_CONTROL = /\p{Cc}/u;

export interface TeacherObservationScope {
  decision_id: string;
  request_generation: number;
  root_task_id: string;
  root_epoch: number;
  task_revision: number;
  candidate_set_digest: string;
}

export type TeacherObservation =
  | { version: 1; type: 'started'; request: DecisionRequest }
  | { version: 1; type: 'finished'; scope: TeacherObservationScope; proposal: DecisionProposal }
  | { version: 1; type: 'cancelled'; scope: TeacherObservationScope;
      reason: 'cancelled' | 'expired' | 'quarantined' | 'accounting_failed' | 'scope_mismatch' | 'shutdown' };

export class TeacherObservationProtocolError extends Error {
  constructor(message = 'invalid private teacher observation frame') {
    super(message);
    this.name = 'TeacherObservationProtocolError';
  }
}

function fail(): never {
  throw new TeacherObservationProtocolError();
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function exactKeys(value: Record<string, unknown>, names: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const expected = [...names].sort();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}

function boundedId(value: unknown): value is string {
  return typeof value === 'string' && value.trim().length > 0
    && Buffer.byteLength(value, 'utf8') <= 256 && !IDENTIFIER_CONTROL.test(value);
}

function boundedDigest(value: unknown): value is string {
  return typeof value === 'string' && value.trim().length > 0
    && Buffer.byteLength(value, 'utf8') <= 512 && !IDENTIFIER_CONTROL.test(value);
}

function uint(value: unknown, positive = false): value is number {
  return Number.isSafeInteger(value) && (value as number) >= (positive ? 1 : 0);
}

function validEmbeddedPayload(type: 'decision_request' | 'decision_proposal', payload: unknown): boolean {
  try {
    return validateEnvelope({
      protocol_version: PROTOCOL_VERSION,
      message_id: 'shadow-1',
      in_reply_to: null,
      producer: 'host',
      producer_sequence: 1,
      emitted_ms: 0,
      trace_id: 'trace-shadow-1',
      message: { type, payload },
    }).ok;
  } catch { return false; }
}

function validateRequest(value: unknown): value is DecisionRequest {
  if (!validEmbeddedPayload('decision_request', value) || !isRecord(value)) return false;
  const state = value['state'];
  if (!isRecord(state) || !Array.isArray(state['candidates']) || state['candidates'].length > 256
    || !Array.isArray(state['objects']) || state['objects'].length > 512
    || !Array.isArray(value['questions']) || value['questions'].length === 0 || value['questions'].length > 128) return false;
  const questionIds = new Set<string>();
  for (const questionValue of value['questions']) {
    if (!isRecord(questionValue)) return false;
    const questionId = questionValue['id'];
    const prompt = questionValue['prompt'];
    if (!boundedId(questionId) || questionIds.has(questionId)
      || typeof prompt !== 'string' || Buffer.byteLength(prompt, 'utf8') > 8 * 1024
      || !Array.isArray(questionValue['options']) || questionValue['options'].length > 128) return false;
    questionIds.add(questionId);
    const optionIds = new Set<string>();
    for (const optionValue of questionValue['options']) {
      if (!isRecord(optionValue)) return false;
      const optionId = optionValue['key'];
      if (!boundedId(optionId) || optionIds.has(optionId)) return false;
      optionIds.add(optionId);
    }
  }
  const candidateIds = new Set<string>();
  for (const candidateValue of state['candidates']) {
    if (!isRecord(candidateValue)) return false;
    const candidateId = candidateValue['candidate_id'];
    if (!boundedId(candidateId) || candidateIds.has(candidateId)) return false;
    candidateIds.add(candidateId);
  }
  for (const objectValue of state['objects']) {
    if (!isRecord(objectValue) || !uint(objectValue['age_ms'])) return false;
  }
  return boundedId(value['decision_id']) && uint(value['request_generation'], true)
    && boundedId(value['root_task_id']) && uint(value['root_epoch'], true)
    && uint(value['task_revision'], true) && uint(value['plan_revision'])
    && boundedId(state['root_task_id']) && uint(state['task_revision'], true)
    && uint(state['snapshot_revision'])
    && value['root_task_id'] === state['root_task_id']
    && value['task_revision'] === state['task_revision']
    && boundedDigest(state['candidate_set_digest'])
    && uint(value['created_ms']) && uint(value['result_deadline_ms'])
    && (value['result_deadline_ms'] as number) > (value['created_ms'] as number);
}

function validateScope(value: unknown): value is TeacherObservationScope {
  if (!isRecord(value) || !exactKeys(value, [
    'decision_id', 'request_generation', 'root_task_id', 'root_epoch', 'task_revision', 'candidate_set_digest',
  ])) return false;
  return boundedId(value['decision_id']) && uint(value['request_generation'], true)
    && boundedId(value['root_task_id']) && uint(value['root_epoch'], true)
    && uint(value['task_revision'], true) && boundedDigest(value['candidate_set_digest']);
}

function scopeFor(request: DecisionRequest): TeacherObservationScope {
  return {
    decision_id: request.decision_id,
    request_generation: request.request_generation,
    root_task_id: request.root_task_id,
    root_epoch: request.root_epoch,
    task_revision: request.task_revision,
    candidate_set_digest: request.state.candidate_set_digest,
  };
}

function sameScope(left: TeacherObservationScope, right: TeacherObservationScope): boolean {
  return left.decision_id === right.decision_id
    && left.request_generation === right.request_generation
    && left.root_task_id === right.root_task_id
    && left.root_epoch === right.root_epoch
    && left.task_revision === right.task_revision
    && left.candidate_set_digest === right.candidate_set_digest;
}

function scopeKey(scope: TeacherObservationScope): string {
  return JSON.stringify([
    scope.root_task_id, scope.root_epoch, scope.task_revision,
    scope.decision_id, scope.request_generation, scope.candidate_set_digest,
  ]);
}

/** Validates one unframed JSON observation, including the normal desktop payload schemas. */
export function decodeTeacherObservation(value: unknown): TeacherObservation {
  if (!isRecord(value) || value['version'] !== 1 || typeof value['type'] !== 'string') return fail();
  if (value['type'] === 'started') {
    if (!exactKeys(value, ['version', 'type', 'request']) || !validateRequest(value['request'])) return fail();
    return value as unknown as TeacherObservation;
  }
  if (value['type'] === 'finished') {
    if (!exactKeys(value, ['version', 'type', 'scope', 'proposal']) || !validateScope(value['scope'])
      || !validEmbeddedPayload('decision_proposal', value['proposal']) || !isRecord(value['proposal'])) return fail();
    const scope = value['scope'];
    const proposal = value['proposal'];
    if (proposal['source'] !== 'teacher' || proposal['decision_id'] !== scope.decision_id
      || proposal['request_generation'] !== scope.request_generation
      || proposal['candidate_set_digest'] !== scope.candidate_set_digest) return fail();
    return value as unknown as TeacherObservation;
  }
  if (value['type'] === 'cancelled') {
    if (!exactKeys(value, ['version', 'type', 'scope', 'reason']) || !validateScope(value['scope'])
      || !['cancelled', 'expired', 'quarantined', 'accounting_failed', 'scope_mismatch', 'shutdown']
        .includes(String(value['reason']))) return fail();
    return value as unknown as TeacherObservation;
  }
  return fail();
}

/** Incremental parser; the 128 KiB maximum includes the 4-byte length prefix. */
export class TeacherObservationDecoder {
  private readonly header = new Uint8Array(4);
  private headerBytes = 0;
  private body: Uint8Array | null = null;
  private bodyBytes = 0;
  private disposed = false;
  private failure: TeacherObservationProtocolError | null = null;

  push(chunk: Uint8Array): TeacherObservation[] {
    if (this.failure) throw this.failure;
    if (this.disposed) throw new TeacherObservationProtocolError('teacher observation decoder is disposed');
    const messages: TeacherObservation[] = [];
    let offset = 0;
    while (offset < chunk.byteLength) {
      if (this.body === null) {
        const count = Math.min(4 - this.headerBytes, chunk.byteLength - offset);
        this.header.set(chunk.subarray(offset, offset + count), this.headerBytes);
        this.headerBytes += count;
        offset += count;
        if (this.headerBytes < 4) continue;
        const length = new DataView(this.header.buffer).getUint32(0, false);
        this.header.fill(0);
        this.headerBytes = 0;
        if (length === 0 || length > MAX_BODY_BYTES) return this.poison();
        this.body = new Uint8Array(length);
        this.bodyBytes = 0;
      }

      const count = Math.min(this.body.byteLength - this.bodyBytes, chunk.byteLength - offset);
      this.body.set(chunk.subarray(offset, offset + count), this.bodyBytes);
      this.bodyBytes += count;
      offset += count;
      if (this.bodyBytes === this.body.byteLength) {
        if (messages.length >= MAX_MESSAGES_PER_PUSH) return this.poison();
        let decoded: unknown;
        try { decoded = JSON.parse(UTF8.decode(this.body)); }
        catch { return this.poison(); }
        this.body.fill(0);
        this.body = null;
        this.bodyBytes = 0;
        try { messages.push(decodeTeacherObservation(decoded)); }
        catch { return this.poison(); }
      }
    }
    return messages;
  }

  dispose(): void {
    this.disposed = true;
    this.header.fill(0);
    this.body?.fill(0);
    this.body = null;
    this.bodyBytes = 0;
    this.headerBytes = 0;
  }

  private poison(): never {
    this.header.fill(0);
    this.body?.fill(0);
    this.body = null;
    this.bodyBytes = 0;
    this.headerBytes = 0;
    this.failure = new TeacherObservationProtocolError();
    throw this.failure;
  }
}

export interface TeacherShadowRunner {
  submit(request: DecisionRequest): Promise<Delivery>;
  cancel(cancel: { decision_id: string; request_generation: number }): void;
  invalidate(): number;
  close?(): void | Promise<void>;
}

export interface TeacherShadowObserverOptions {
  /** Monotonic host clock projection used by request deadlines. */
  nowMs: () => number;
  maxInFlight?: number;
  deadlinePollMs?: number;
  closeTimeoutMs?: number;
}

export interface TeacherShadowStatistics {
  readonly started: number;
  readonly verified: number;
  readonly disagreements: number;
  readonly unavailable: number;
  readonly refused: number;
  readonly expired: number;
  readonly dropped: number;
  readonly in_flight: number;
}

interface MutableStatistics {
  started: number;
  verified: number;
  disagreements: number;
  unavailable: number;
  refused: number;
  expired: number;
  dropped: number;
}

interface RootFence {
  rootTaskId: string;
  rootEpoch: number;
  taskRevision: number;
}

interface ActiveTask {
  readonly key: string;
  readonly scope: TeacherObservationScope;
  readonly request: DecisionRequest;
  readonly observerEpoch: number;
  readonly deadlineMs: number;
  timer: ReturnType<typeof setTimeout> | null;
  active: boolean;
  primary: DecisionProposal | null;
  shadow: DecisionProposal | null;
}

const EMPTY_STATISTICS: MutableStatistics = {
  started: 0, verified: 0, disagreements: 0, unavailable: 0,
  refused: 0, expired: 0, dropped: 0,
};

/** Compares Host's actual teacher proposal with a private local S1 runner only. */
export class TeacherShadowObserver {
  private readonly tasks = new Map<string, ActiveTask>();
  private readonly latestGeneration = new Map<string, number>();
  private readonly seenRoots = new Set<string>();
  private readonly stats: MutableStatistics = { ...EMPTY_STATISTICS };
  private readonly maxInFlight: number;
  private readonly deadlinePollMs: number;
  private readonly closeTimeoutMs: number;
  private currentRoot: RootFence | null = null;
  private observerEpoch = 0;
  private closed = false;
  private disabled = false;
  private closeTask: Promise<void> | null = null;

  constructor(private readonly runner: TeacherShadowRunner, private readonly options: TeacherShadowObserverOptions) {
    this.maxInFlight = boundedInteger(options.maxInFlight, MAX_ACTIVE_TASKS, 1, MAX_ACTIVE_TASKS);
    this.deadlinePollMs = boundedInteger(options.deadlinePollMs, DEFAULT_DEADLINE_POLL_MS, 5, MAX_TIMER_SLICE_MS);
    this.closeTimeoutMs = boundedInteger(options.closeTimeoutMs, DEFAULT_CLOSE_TIMEOUT_MS, 0, MAX_CLOSE_TIMEOUT_MS);
  }

  get statistics(): TeacherShadowStatistics {
    return Object.freeze({ ...this.stats, in_flight: this.tasks.size });
  }

  /** This observer never throws into Host or sends a proposal to an execution channel. */
  observe(value: TeacherObservation): void {
    let message: TeacherObservation;
    try { message = decodeTeacherObservation(value); }
    catch { increment(this.stats, 'dropped'); return; }
    if (this.closed) { increment(this.stats, 'dropped'); return; }

    switch (message.type) {
      case 'started': this.start(message.request); break;
      case 'finished': this.finish(message.scope, message.proposal); break;
      case 'cancelled': this.cancel(message.scope, message.reason); break;
    }
  }

  close(): Promise<void> {
    if (this.closeTask) return this.closeTask;
    this.closed = true;
    this.invalidateTasks();
    this.closeTask = closeRunnerBounded(this.runner, this.closeTimeoutMs);
    return this.closeTask;
  }

  private start(request: DecisionRequest): void {
    if (this.disabled) { increment(this.stats, 'dropped'); return; }
    const scope = scopeFor(request);
    if (!this.acceptRoot(scope)) { increment(this.stats, 'dropped'); return; }
    const now = this.readClock();
    if (now === null) { this.clockUnavailable(); return; }
    if (now >= request.result_deadline_ms) { increment(this.stats, 'expired'); return; }

    const generationKey = `${scope.root_task_id}\u0000${scope.decision_id}`;
    const latest = this.latestGeneration.get(generationKey);
    if (latest !== undefined && latest >= scope.request_generation) {
      increment(this.stats, 'dropped');
      return;
    }
    if (latest === undefined && this.latestGeneration.size >= MAX_REMEMBERED_DECISIONS) {
      increment(this.stats, 'refused');
      return;
    }
    this.latestGeneration.set(generationKey, scope.request_generation);
    for (const older of [...this.tasks.values()]) {
      if (older.scope.root_task_id === scope.root_task_id && older.scope.decision_id === scope.decision_id
        && scope.request_generation > older.scope.request_generation) this.retireTask(older, true);
    }
    if (this.tasks.size >= this.maxInFlight) { increment(this.stats, 'refused'); return; }

    let copied: DecisionRequest;
    try { copied = structuredClone(request); }
    catch { increment(this.stats, 'dropped'); return; }
    const task: ActiveTask = {
      key: scopeKey(scope), scope, request: copied, observerEpoch: this.observerEpoch,
      deadlineMs: request.result_deadline_ms, timer: null, active: true, primary: null, shadow: null,
    };
    this.tasks.set(task.key, task);
    increment(this.stats, 'started');
    this.scheduleDeadline(task);
    if (!this.isCurrentTask(task)) return;

    let submitted: Promise<Delivery>;
    try { submitted = Promise.resolve(this.runner.submit(structuredClone(copied))); }
    catch { submitted = Promise.reject(new Error('shadow submit failed')); }
    void submitted.then(
      (delivery) => {
        try { this.onShadow(task, delivery); }
        catch { this.onShadowFailure(task); }
      },
      () => this.onShadowFailure(task),
    );
  }

  private finish(scope: TeacherObservationScope, proposal: DecisionProposal): void {
    const task = this.tasks.get(scopeKey(scope));
    if (!task || !this.isCurrentTask(task) || !sameScope(task.scope, scope)) {
      increment(this.stats, 'dropped');
      return;
    }
    const now = this.readClock();
    if (now === null) { this.clockUnavailable(); return; }
    if (now >= task.deadlineMs) { this.expire(task); return; }
    if (task.primary) { increment(this.stats, 'dropped'); return; }
    if (proposal.failure !== null || proposal.failure === undefined || proposal.truncated) {
      this.classifyFailure(proposal.failure);
      this.retireTask(task, true);
      return;
    }
    if (!successfulProposalMatchesRequest(proposal, task.request, 'teacher')) {
      this.retireTask(task, true);
      increment(this.stats, 'dropped');
      return;
    }
    task.primary = proposal;
    this.compareIfReady(task);
  }

  private cancel(scope: TeacherObservationScope, reason: Extract<TeacherObservation, { type: 'cancelled' }>['reason']): void {
    const task = this.tasks.get(scopeKey(scope));
    if (!task || !sameScope(task.scope, scope)) { increment(this.stats, 'dropped'); return; }
    this.retireTask(task, true);
    if (reason === 'expired') increment(this.stats, 'expired');
    else if (reason === 'quarantined' || reason === 'accounting_failed' || reason === 'scope_mismatch') {
      increment(this.stats, 'unavailable');
    } else increment(this.stats, 'dropped');
  }

  private onShadow(task: ActiveTask, delivery: Delivery): void {
    if (!this.isCurrentTask(task)) return;
    const now = this.readClock();
    if (now === null) { this.clockUnavailable(); return; }
    if (now >= task.deadlineMs) { this.expire(task); return; }
    if (delivery.kind === 'dropped') {
      this.retireTask(task, false);
      if (delivery.reason === 'cancelled') increment(this.stats, 'dropped');
      else increment(this.stats, 'unavailable');
      return;
    }
    if (!validEmbeddedPayload('decision_proposal', delivery.proposal) || !isRecord(delivery.proposal)
      || delivery.proposal['source'] !== 's1'
      || delivery.proposal['decision_id'] !== task.scope.decision_id
      || delivery.proposal['request_generation'] !== task.scope.request_generation
      || delivery.proposal['candidate_set_digest'] !== task.scope.candidate_set_digest) {
      this.retireTask(task, true);
      increment(this.stats, 'dropped');
      return;
    }
    if (delivery.proposal.failure !== null || delivery.proposal.failure === undefined || delivery.proposal.truncated) {
      this.classifyFailure(delivery.proposal.failure);
      this.retireTask(task, false);
      return;
    }
    if (!successfulProposalMatchesRequest(delivery.proposal, task.request, 's1')) {
      this.retireTask(task, true);
      increment(this.stats, 'dropped');
      return;
    }
    task.shadow = delivery.proposal;
    this.compareIfReady(task);
  }

  private onShadowFailure(task: ActiveTask): void {
    if (!this.isCurrentTask(task)) return;
    this.retireTask(task, false);
    increment(this.stats, 'unavailable');
  }

  private compareIfReady(task: ActiveTask): void {
    if (!task.primary || !task.shadow || !this.isCurrentTask(task)) return;
    const now = this.readClock();
    if (now === null) { this.clockUnavailable(); return; }
    if (now >= task.deadlineMs) { this.expire(task); return; }
    this.retireTask(task, false);
    increment(this.stats, 'verified');
    if (!sameDecision(task.primary, task.shadow)) increment(this.stats, 'disagreements');
  }

  private acceptRoot(scope: TeacherObservationScope): boolean {
    const current = this.currentRoot;
    if (!current) {
      this.currentRoot = { rootTaskId: scope.root_task_id, rootEpoch: scope.root_epoch, taskRevision: scope.task_revision };
      this.seenRoots.add(scope.root_task_id);
      return true;
    }
    if (current.rootTaskId !== scope.root_task_id) {
      if (this.seenRoots.has(scope.root_task_id)) return false;
      if (this.seenRoots.size >= MAX_REMEMBERED_ROOTS) {
        this.invalidateTasks();
        this.currentRoot = null;
        this.disabled = true;
        return false;
      }
      this.invalidateTasks();
      this.currentRoot = { rootTaskId: scope.root_task_id, rootEpoch: scope.root_epoch, taskRevision: scope.task_revision };
      this.seenRoots.add(scope.root_task_id);
      this.latestGeneration.clear();
      return true;
    }
    if (scope.root_epoch < current.rootEpoch || scope.task_revision < current.taskRevision) return false;
    if (scope.root_epoch > current.rootEpoch || scope.task_revision > current.taskRevision) {
      this.invalidateTasks();
      this.currentRoot = { rootTaskId: scope.root_task_id, rootEpoch: scope.root_epoch, taskRevision: scope.task_revision };
      this.latestGeneration.clear();
    }
    return true;
  }

  private scheduleDeadline(task: ActiveTask): void {
    if (!this.isCurrentTask(task)) return;
    const now = this.readClock();
    if (now === null) { this.clockUnavailable(); return; }
    const remaining = task.deadlineMs - now;
    if (remaining <= 0) { this.expire(task); return; }
    task.timer = setTimeout(() => {
      task.timer = null;
      if (!this.isCurrentTask(task)) return;
      const checked = this.readClock();
      if (checked === null) { this.clockUnavailable(); return; }
      if (checked >= task.deadlineMs) this.expire(task);
      else this.scheduleDeadline(task);
    }, Math.max(1, Math.min(remaining, this.deadlinePollMs, MAX_TIMER_SLICE_MS)));
    task.timer.unref?.();
  }

  private expire(task: ActiveTask): void {
    if (!this.isCurrentTask(task)) return;
    this.retireTask(task, true);
    increment(this.stats, 'expired');
  }

  private clockUnavailable(): void {
    increment(this.stats, 'unavailable');
    this.invalidateTasks();
  }

  private readClock(): number | null {
    try {
      const now = this.options.nowMs();
      return Number.isSafeInteger(now) && now >= 0 ? now : null;
    } catch { return null; }
  }

  private isCurrentTask(task: ActiveTask): boolean {
    const current = this.currentRoot;
    return !this.closed && task.active && this.tasks.get(task.key) === task
      && task.observerEpoch === this.observerEpoch && current !== null
      && task.scope.root_task_id === current.rootTaskId
      && task.scope.root_epoch === current.rootEpoch
      && task.scope.task_revision === current.taskRevision;
  }

  private invalidateTasks(): void {
    this.observerEpoch += 1;
    for (const task of this.tasks.values()) {
      this.retireTask(task, false);
      increment(this.stats, 'dropped');
    }
    try { this.runner.invalidate(); } catch { /* A stale observer can only reduce shadow coverage. */ }
  }

  private retireTask(task: ActiveTask, cancel: boolean): void {
    if (!task.active) return;
    task.active = false;
    if (task.timer) clearTimeout(task.timer);
    task.timer = null;
    if (this.tasks.get(task.key) === task) this.tasks.delete(task.key);
    if (cancel) {
      try { this.runner.cancel({ decision_id: task.scope.decision_id, request_generation: task.scope.request_generation }); }
      catch { /* Cancellation is best-effort; task fencing still rejects late results. */ }
    }
  }

  private classifyFailure(failure: DecisionProposal['failure']): void {
    if (failure === 'refused' || failure === 'unsupported_question' || failure === 'truncated') {
      increment(this.stats, 'refused');
    } else increment(this.stats, 'unavailable');
  }
}

function sameDecision(left: DecisionProposal, right: DecisionProposal): boolean {
  if (!sameChoice(left.choice, right.choice) || left.answers.length !== right.answers.length) return false;
  for (let index = 0; index < left.answers.length; index += 1) {
    const a = left.answers[index]!;
    const b = right.answers[index]!;
    if (a.question_id !== b.question_id || (a.selected ?? null) !== (b.selected ?? null)) return false;
  }
  return true;
}

function successfulProposalMatchesRequest(
  proposal: DecisionProposal,
  request: DecisionRequest,
  source: 'teacher' | 's1',
): boolean {
  if (!isRecord(proposal) || !validEmbeddedPayload('decision_proposal', proposal)
    || proposal['source'] !== source || proposal['decision_id'] !== request.decision_id
    || proposal['request_generation'] !== request.request_generation
    || proposal['candidate_set_digest'] !== request.state.candidate_set_digest
    || proposal['failure'] !== null || proposal['truncated'] !== false
    || !uint(proposal['queue_ms']) || !uint(proposal['inference_ms'])
    || proposal['choice'] == null || !Array.isArray(proposal['answers'])
    || proposal['answers'].length !== request.questions.length) return false;

  const questions = new Map(request.questions.map((question) => [question.id, question]));
  const answered = new Set<string>();
  for (const answerValue of proposal['answers']) {
    if (!isRecord(answerValue) || !boundedId(answerValue['question_id']) || answered.has(answerValue['question_id'])) return false;
    const question = questions.get(answerValue['question_id']);
    if (!question) return false;
    answered.add(answerValue['question_id']);
    const selected = answerValue['selected'] ?? null;
    if (selected !== null && (!boundedId(selected) || !question.options.some((option) => option.key === selected))) return false;
    if (question.kind === 'choice' && selected === null) return false;
    if (question.kind === 'noul' && selected !== null) return false;
    const confidence = answerValue['confidence_milli'];
    if (confidence != null && (!uint(confidence) || confidence > 1000)) return false;
  }
  if (answered.size !== questions.size) return false;

  const choice = proposal['choice'];
  if (!isRecord(choice)) return false;
  switch (choice['action']) {
    case 'start_skill': {
      const candidateId = choice['candidate'];
      return boundedId(candidateId) && request.state.candidates.some((candidate) =>
        candidate.candidate_id === candidateId && candidate.precondition.state === 'executable');
    }
    case 'observe_more':
      return Array.isArray(choice['facts']) && choice['facts'].length <= 16
        && choice['facts'].every((fact) => typeof fact === 'string' && fact.length > 0
          && Buffer.byteLength(fact, 'utf8') <= 512);
    case 'request_planning':
    case 'stop_or_yield': {
      const reason = choice['reason'];
      return typeof reason === 'string' && reason.length > 0 && Buffer.byteLength(reason, 'utf8') <= 512
        && !IDENTIFIER_CONTROL.test(reason);
    }
    default: return false;
  }
}

function sameChoice(left: DecisionProposal['choice'], right: DecisionProposal['choice']): boolean {
  if (left === null || left === undefined || right === null || right === undefined) {
    return (left === null || left === undefined) && (right === null || right === undefined);
  }
  if (left.action !== right.action) return false;
  switch (left.action) {
    case 'start_skill': return right.action === left.action && right.candidate === left.candidate;
    case 'continue_skill': return right.action === left.action && right.attempt === left.attempt;
    case 'observe_more': return right.action === left.action && equalStrings(left.facts, right.facts);
    case 'request_planning':
    case 'stop_or_yield': return right.action === left.action && right.reason === left.reason;
  }
}

function equalStrings(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function boundedInteger(value: number | undefined, fallback: number, min: number, max: number): number {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value < min || value > max) return fallback;
  return value;
}

function increment(stats: MutableStatistics, key: keyof MutableStatistics): void {
  if (stats[key] < Number.MAX_SAFE_INTEGER) stats[key] += 1;
}

async function closeRunnerBounded(runner: TeacherShadowRunner, timeoutMs: number): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    await Promise.race([
      Promise.resolve().then(() => runner.close?.()).then(() => undefined, () => undefined),
      new Promise<void>((resolve) => { timer = setTimeout(resolve, timeoutMs); }),
    ]);
  } catch { /* Closing the private observer must never escape into Host control. */ }
  finally { if (timer) clearTimeout(timer); }
}
