import { randomUUID } from 'node:crypto';
import type { Duplex } from 'node:stream';

const MAX_BYTES = 32 * 1024;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const utf8 = new TextDecoder('utf-8', { fatal: true });
type RecordValue = Record<string, unknown>;
type ReplyType = 'prepared' | 'finished' | 'aborted';

function object(value: unknown): value is RecordValue {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function keys(value: RecordValue, expected: string[]): boolean {
  return Object.keys(value).sort().join(',') === expected.sort().join(',');
}

/** Main/Host private FD3 only. Never connected to the renderer or public IPC. */
export class OwnedRevisitControl {
  private readonly header = Buffer.alloc(4);
  private headerBytes = 0;
  private body: Buffer | null = null;
  private bodyBytes = 0;
  private closed = false;
  private pending: {
    id: string; type: ReplyType; transition: string | null;
    resolve: (value: RecordValue) => void; reject: (error: Error) => void;
    timer: ReturnType<typeof setTimeout>;
  } | null = null;

  constructor(private readonly stream: Duplex) {
    stream.on('data', (chunk: Buffer) => this.receive(chunk));
    stream.once('error', () => this.fail());
    stream.once('end', () => this.fail());
    stream.once('close', () => this.fail());
  }

  prepare(timeoutMs = 5_000): Promise<RecordValue> {
    return this.request('prepare', 'prepared', {}, null, timeoutMs);
  }

  finish(completion: RecordValue, deviceToken: string, sourceToken: string, timeoutMs = 10_000): Promise<RecordValue> {
    const transition = completion.transition_id;
    if (typeof transition !== 'string' || !UUID.test(transition)
      || !/^[0-9a-f]{32}$/.test(deviceToken) || !/^[0-9a-f]{32}$/.test(sourceToken)) {
      return Promise.reject(new Error('invalid private revisit completion'));
    }
    return this.request('finish', 'finished', {
      completion, new_device_token: deviceToken, new_source_token: sourceToken,
    }, transition, timeoutMs);
  }

  abort(transition: string): Promise<RecordValue> {
    if (!UUID.test(transition)) return Promise.reject(new Error('invalid private revisit transition'));
    return this.request('abort', 'aborted', { transition_id: transition }, transition, 2_000);
  }

  close(): void { this.fail(); }

  private request(type: string, replyType: ReplyType, payload: RecordValue,
    transition: string | null, timeoutMs: number): Promise<RecordValue> {
    if (this.closed || this.pending) return Promise.reject(new Error('private revisit control unavailable'));
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 30_000) {
      return Promise.reject(new Error('invalid private revisit timeout'));
    }
    const id = randomUUID();
    const body = Buffer.from(JSON.stringify({ version: 1, type, request_id: id, ...payload }));
    if (body.length === 0 || body.length > MAX_BYTES) {
      body.fill(0);
      return Promise.reject(new Error('private revisit request exceeds limit'));
    }
    const frame = Buffer.alloc(4 + body.length);
    frame.writeUInt32BE(body.length); body.copy(frame, 4); body.fill(0);
    return new Promise<RecordValue>((resolve, reject) => {
      this.pending = { id, type: replyType, transition, resolve, reject,
        timer: setTimeout(() => this.fail(), timeoutMs) };
      try {
        this.stream.write(frame, (error?: Error | null) => {
          frame.fill(0);
          if (error) this.fail();
        });
      } catch { frame.fill(0); this.fail(); }
    });
  }

  private receive(chunk: Buffer): void {
    if (this.closed) return;
    let offset = 0;
    while (offset < chunk.length && !this.closed) {
      if (!this.body) {
        const count = Math.min(4 - this.headerBytes, chunk.length - offset);
        chunk.copy(this.header, this.headerBytes, offset, offset + count);
        this.headerBytes += count; offset += count;
        if (this.headerBytes < 4) continue;
        const size = this.header.readUInt32BE();
        this.header.fill(0); this.headerBytes = 0;
        if (size === 0 || size > MAX_BYTES) { this.fail(); return; }
        this.body = Buffer.alloc(size);
      }
      const count = Math.min(this.body.length - this.bodyBytes, chunk.length - offset);
      chunk.copy(this.body, this.bodyBytes, offset, offset + count);
      this.bodyBytes += count; offset += count;
      if (this.bodyBytes === this.body.length) {
        let value: unknown;
        try { value = JSON.parse(utf8.decode(this.body)); }
        catch { this.fail(); return; }
        this.body.fill(0); this.body = null; this.bodyBytes = 0;
        this.accept(value);
      }
    }
  }

  private accept(value: unknown): void {
    const pending = this.pending;
    if (!pending || !object(value) || value.version !== 1 || value.request_id !== pending.id) {
      this.fail(); return;
    }
    const base = ['version', 'type', 'request_id'];
    if (value.type === 'rejected') {
      if (!keys(value, [...base, 'code']) || typeof value.code !== 'string'
        || !/^[a-z_]{1,64}$/.test(value.code)) { this.fail(); return; }
      clearTimeout(pending.timer); this.pending = null;
      pending.reject(new Error('private revisit request rejected'));
      return;
    }
    const valid = value.type === pending.type && (value.type === 'prepared'
      ? keys(value, [...base, 'challenge']) && object(value.challenge)
      : keys(value, [...base, 'transition_id']) && value.transition_id === pending.transition);
    if (!valid) { this.fail(); return; }
    clearTimeout(pending.timer); this.pending = null;
    pending.resolve(value);
  }

  private fail(): void {
    if (this.closed) return;
    this.closed = true;
    this.header.fill(0); this.body?.fill(0); this.body = null;
    if (this.pending) {
      clearTimeout(this.pending.timer);
      this.pending.reject(new Error('private revisit control closed'));
      this.pending = null;
    }
    this.stream.destroy();
  }
}
