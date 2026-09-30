import type { Envelope } from './generated/envelope.js';
import { validateEnvelope } from './validate.js';

/** A frame is a 4-byte big-endian length and that many bytes of UTF-8 JSON (design 4.6). */
export const LENGTH_PREFIX_BYTES = 4;
/** Control messages only. Images travel on a separate bounded channel. */
export const MAX_FRAME_BYTES = 1 << 20;

export type FrameErrorKind = 'too_large' | 'empty' | 'malformed' | 'invalid';

export class FrameError extends Error {
  constructor(
    readonly kind: FrameErrorKind,
    message: string,
  ) {
    super(message);
    this.name = 'FrameError';
  }
}

export function encodeFrame(envelope: Envelope): Uint8Array {
  const payload = new TextEncoder().encode(JSON.stringify(envelope));
  if (payload.length > MAX_FRAME_BYTES) {
    throw new FrameError('too_large', `frame of ${payload.length} bytes exceeds the ${MAX_FRAME_BYTES} limit`);
  }
  const out = new Uint8Array(LENGTH_PREFIX_BYTES + payload.length);
  new DataView(out.buffer).setUint32(0, payload.length, false);
  out.set(payload, LENGTH_PREFIX_BYTES);
  return out;
}

/**
 * Accumulates bytes and yields complete envelopes. The length is checked before
 * any payload is waited for, so a hostile length cannot make it buffer. After
 * any error it stays failed: there is no telling where the next frame starts, so
 * the connection has to be dropped.
 */
export class FrameDecoder {
  private chunks: Uint8Array[] = [];
  private size = 0;
  private failure: FrameError | null = null;

  constructor(private readonly maxFrame = MAX_FRAME_BYTES) {}

  push(bytes: Uint8Array): void {
    if (this.failure || bytes.length === 0) return;
    this.chunks.push(bytes);
    this.size += bytes.length;
  }

  get buffered(): number {
    return this.size;
  }

  private take(count: number): Uint8Array {
    const out = new Uint8Array(count);
    let filled = 0;
    while (filled < count) {
      const head = this.chunks[0]!;
      const need = count - filled;
      if (head.length <= need) {
        out.set(head, filled);
        filled += head.length;
        this.chunks.shift();
      } else {
        out.set(head.subarray(0, need), filled);
        this.chunks[0] = head.subarray(need);
        filled += need;
      }
    }
    this.size -= count;
    return out;
  }

  private peekLength(): number {
    const header = new Uint8Array(LENGTH_PREFIX_BYTES);
    let filled = 0;
    for (const chunk of this.chunks) {
      const take = Math.min(chunk.length, LENGTH_PREFIX_BYTES - filled);
      header.set(chunk.subarray(0, take), filled);
      filled += take;
      if (filled === LENGTH_PREFIX_BYTES) break;
    }
    return new DataView(header.buffer).getUint32(0, false);
  }

  private fail(error: FrameError): never {
    this.failure = error;
    throw error;
  }

  /** The next complete envelope, or `null` when more bytes are needed. */
  next(): Envelope | null {
    if (this.failure) throw this.failure;
    if (this.size < LENGTH_PREFIX_BYTES) return null;
    const length = this.peekLength();
    if (length === 0) this.fail(new FrameError('empty', 'empty frame'));
    if (length > this.maxFrame) {
      this.fail(new FrameError('too_large', `frame of ${length} bytes exceeds the ${this.maxFrame} limit`));
    }
    if (this.size < LENGTH_PREFIX_BYTES + length) return null;
    this.take(LENGTH_PREFIX_BYTES);
    const payload = this.take(length);
    let value: unknown;
    try {
      value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(payload));
    } catch (error) {
      this.fail(new FrameError('malformed', `malformed frame: ${(error as Error).message}`));
    }
    const result = validateEnvelope(value);
    if (!result.ok) this.fail(new FrameError('invalid', `invalid message: ${result.errors.join('; ')}`));
    return result.value;
  }
}
