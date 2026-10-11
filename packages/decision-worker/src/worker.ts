import type { DecisionCancel, DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';

/**
 * Whatever makes the choice: a scripted stand-in here, a local model or a cloud
 * teacher later. It may ignore the abort signal; the worker does not depend on
 * it (design 11.1: cancelling does not stop the computation, so a late result
 * must still be isolated).
 */
export interface DecisionBackend {
  readonly name: string;
  readonly source?: DecisionProposal['source'];
  decide(request: DecisionRequest, signal: AbortSignal): Promise<DecisionProposal>;
}

export type DropReason = 'cancelled' | 'superseded' | 'stale_generation' | 'stale_epoch' | 'mismatched_identity';

/** What a request came to: a proposal to deliver, or a reason it must not be. */
export type Delivery =
  | { kind: 'proposal'; proposal: DecisionProposal }
  | { kind: 'dropped'; reason: DropReason; decisionId: string; generation: number };

/** How many finished decisions are remembered for refusing a stale generation. */
const MAX_REMEMBERED = 1024;

interface Active {
  generation: number;
  epoch: number;
  controller: AbortController;
  /** Why it was aborted, when it was. */
  ended: DropReason | null;
}

/**
 * Runs decisions for the host, one at a time per logical decision.
 *
 * The identity of a result is the host's: a proposal is delivered only while
 * its request is still the current one at the current epoch and only if it
 * answers exactly what was asked. Everything else, including a result that
 * arrives after its request was cancelled or replaced, is dropped and reported
 * as dropped, never delivered.
 */
export class DecisionWorker {
  private readonly active = new Map<string, Active>();
  /** The newest generation seen per decision, kept after it finishes so an older one is still refused. */
  private readonly latest = new Map<string, number>();
  private epochNumber: number;

  constructor(
    private readonly backend: DecisionBackend,
    epoch = 1,
  ) {
    this.epochNumber = epoch;
  }

  get epoch(): number {
    return this.epochNumber;
  }

  get inFlight(): number {
    return this.active.size;
  }

  /** The worker process is replaced or the relay was cut: nothing in flight may be delivered. */
  invalidate(): number {
    this.epochNumber += 1;
    for (const entry of this.active.values()) {
      entry.ended ??= 'stale_epoch';
      entry.controller.abort();
    }
    return this.epochNumber;
  }

  cancel(cancel: DecisionCancel): void {
    const entry = this.active.get(cancel.decision_id);
    // A cancel for an older generation must not cancel the request that replaced it.
    if (!entry || cancel.request_generation < entry.generation) return;
    entry.ended ??= 'cancelled';
    entry.controller.abort();
  }

  async submit(request: DecisionRequest): Promise<Delivery> {
    const id = request.decision_id;
    const generation = request.request_generation;
    const newest = this.latest.get(id);
    if (newest !== undefined && newest >= generation) {
      return { kind: 'dropped', reason: 'stale_generation', decisionId: id, generation };
    }
    this.latest.set(id, generation);
    if (this.latest.size > MAX_REMEMBERED) {
      // Oldest first: a Map iterates in insertion order.
      const oldest = this.latest.keys().next().value;
      if (oldest !== undefined) this.latest.delete(oldest);
    }
    const previous = this.active.get(id);
    if (previous) {
      previous.ended ??= 'superseded';
      previous.controller.abort();
    }
    const entry: Active = {
      generation,
      epoch: this.epochNumber,
      controller: new AbortController(),
      ended: null,
    };
    this.active.set(id, entry);
    try {
      let proposal: DecisionProposal;
      try {
        proposal = await this.backend.decide(request, entry.controller.signal);
      } catch {
        // The backend could not answer. That is an answer the host understands.
        proposal = unavailable(request, this.backend.name, this.backend.source ?? 'teacher');
      }
      const dropped = (reason: DropReason): Delivery => ({ kind: 'dropped', reason, decisionId: id, generation });
      if (entry.ended) return dropped(entry.ended);
      if (entry.epoch !== this.epochNumber) return dropped('stale_epoch');
      if (this.active.get(id) !== entry) return dropped('superseded');
      if (
        proposal.decision_id !== id ||
        proposal.request_generation !== generation ||
        proposal.candidate_set_digest !== request.state.candidate_set_digest
      ) {
        return dropped('mismatched_identity');
      }
      return { kind: 'proposal', proposal };
    } finally {
      if (this.active.get(id) === entry) this.active.delete(id);
    }
  }
}

/** "I could not answer", in the terms the host waits on. */
export function unavailable(request: DecisionRequest, backend: string, source: DecisionProposal['source'] = 'teacher'): DecisionProposal {
  return {
    decision_id: request.decision_id,
    request_generation: request.request_generation,
    candidate_set_digest: request.state.candidate_set_digest,
    backend,
    source,
    model_revision: 'none',
    answers: [],
    choice: null,
    failure: 'unavailable',
    truncated: false,
    queue_ms: 0,
    inference_ms: 0,
  };
}
