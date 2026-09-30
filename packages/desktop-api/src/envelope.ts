import type { Envelope, Message } from './generated/envelope.js';

/** The protocol version this build speaks. */
export const PROTOCOL_VERSION = 1;

/**
 * Builds envelopes for one producer. Message ids and sequence numbers are the
 * producer's own and strictly increasing, so a gap on the other side means
 * messages were lost.
 */
export class EnvelopeFactory {
  private sequence = 0;

  constructor(
    private readonly producer: string,
    private readonly now: () => number,
  ) {}

  make(message: Message, inReplyTo: string | null = null): Envelope {
    this.sequence += 1;
    const id = `${this.producer}-${this.sequence}`;
    return {
      protocol_version: PROTOCOL_VERSION,
      message_id: id,
      in_reply_to: inReplyTo,
      producer: this.producer,
      producer_sequence: this.sequence,
      emitted_ms: Math.max(0, Math.trunc(this.now())),
      trace_id: `trace-${id}`,
      message,
    };
  }
}
