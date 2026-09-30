import type { DecisionCancel, DecisionProposal, DecisionRequest } from '@computer-user/desktop-api';
import type { Delivery } from '@computer-user/decision-worker';

/** What the relay needs of the supervisor: requests in, proposals out. */
export interface DecisionTransport {
  on(event: 'decisionRequest', listener: (request: DecisionRequest) => void): unknown;
  on(event: 'decisionCancel', listener: (cancel: DecisionCancel) => void): unknown;
  sendDecisionProposal(proposal: DecisionProposal): void;
}

/** What the relay needs of the worker. */
export interface DecisionRunner {
  submit(request: DecisionRequest): Promise<Delivery>;
  cancel(cancel: DecisionCancel): void;
  invalidate(): number;
}

/**
 * Carries fixed-schema decision messages between cu-host and the decision
 * worker and does nothing else (design 4.6). It performs no admission: what
 * the host makes of a proposal is the host's. Its one judgement is that a result
 * which the worker says must not be delivered is not delivered.
 */
export class DecisionRelay {
  readonly trace: string[] = [];

  constructor(
    private readonly transport: DecisionTransport,
    private readonly worker: DecisionRunner,
  ) {}

  attach(): void {
    this.transport.on('decisionRequest', (request) => void this.handle(request));
    this.transport.on('decisionCancel', (cancel) => this.worker.cancel(cancel));
  }

  private async handle(request: DecisionRequest): Promise<void> {
    const delivery = await this.worker.submit(request);
    if (delivery.kind === 'proposal') {
      this.transport.sendDecisionProposal(delivery.proposal);
    } else {
      this.trace.push(`dropped ${delivery.decisionId} generation ${delivery.generation}: ${delivery.reason}`);
    }
  }

  /** The worker process died or the relay was cut: nothing in flight may be delivered. */
  workerLost(): void {
    this.worker.invalidate();
  }
}
