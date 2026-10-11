import {
  validateBrowserControlBootstrap,
  validateBrowserControlHello,
  validateBrowserControlReady,
  validateBrowserControlReply,
  validateBrowserControlSubmit,
  type BrowserControlBootstrap,
  type BrowserControlHello,
  type BrowserControlReady,
  type BrowserControlReply,
  type BrowserControlSubmit,
} from '@computer-use/desktop-api';

const PREFIX_BYTES = 12;
const MAX_BUFFERED_BYTES = PREFIX_BYTES + 64 * 1024;

export const BROWSER_CONTROL_BOOTSTRAP_MAGIC = 'CUBRBOOT';
export const BROWSER_CONTROL_HELLO_MAGIC = 'CUBRHELO';
export const BROWSER_CONTROL_READY_MAGIC = 'CUBRREDY';
export const BROWSER_CONTROL_SUBMIT_MAGIC = 'CUBRSUBM';
export const BROWSER_CONTROL_REPLY_MAGIC = 'CUBRREPL';
export const MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES = 16 * 1024;
export const MAX_BROWSER_CONTROL_SUBMIT_BYTES = 64 * 1024;
export const MAX_BROWSER_CONTROL_REPLY_BYTES = 4 * 1024;
const INBOUND_MAGICS = [BROWSER_CONTROL_HELLO_MAGIC, BROWSER_CONTROL_SUBMIT_MAGIC] as const;

export type BrowserControlInbound =
  | { kind: 'hello'; value: BrowserControlHello }
  | { kind: 'submit'; value: BrowserControlSubmit };

function fail(): never {
  // Do not include decoded payloads here: bootstrap diagnostics must never echo the token.
  throw new Error('browser control FD3 frame is invalid');
}

function canonicalScope(scope: BrowserControlBootstrap['scope']): BrowserControlBootstrap['scope'] {
  const context = scope.context;
  return {
    scope_id: scope.scope_id,
    generation: scope.generation,
    context: {
      browser_session_id: context.browser_session_id,
      browser_context_id: context.browser_context_id,
      page_target_id: context.page_target_id,
      cdp_session_id: context.cdp_session_id,
      frame_id: context.frame_id,
      document_loader_id: context.document_loader_id,
      fixture_file_url: context.fixture_file_url,
      session_epoch: context.session_epoch,
    },
    clock_domain: scope.clock_domain,
    issued_ms: scope.issued_ms,
  };
}

function encodeFrame(magic: string, value: unknown, maximum: number): Buffer {
  const body = Buffer.from(JSON.stringify(value), 'utf8');
  if (body.length < 2 || body.length > maximum) fail();
  const frame = Buffer.allocUnsafe(PREFIX_BYTES + body.length);
  frame.write(magic, 0, 'ascii');
  frame.writeUInt32BE(body.length, 8);
  body.copy(frame, PREFIX_BYTES);
  return frame;
}

/** Main-to-Device first frame. The token is carried only inside this private FD3 frame. */
export function encodeBrowserControlBootstrap(value: BrowserControlBootstrap): Buffer {
  const validation = validateBrowserControlBootstrap(value);
  if (!validation.ok) fail();
  const bootstrap: BrowserControlBootstrap = {
    version: value.version,
    token: value.token,
    channel_generation: value.channel_generation,
    browser_page_profile: { kind: 'fixture_background_option' },
    scope: canonicalScope(value.scope),
  };
  return encodeFrame(BROWSER_CONTROL_BOOTSTRAP_MAGIC, bootstrap, MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES);
}

/** Main-to-Device readiness frame. Callers correlate it to Hello before writing. */
export function encodeBrowserControlReady(value: BrowserControlReady): Buffer {
  const validation = validateBrowserControlReady(value);
  if (!validation.ok) fail();
  const ready: BrowserControlReady = {
    version: value.version,
    channel_generation: value.channel_generation,
    session_epoch: value.session_epoch,
    scope_id: value.scope_id,
    scope_generation: value.scope_generation,
    next_sequence: value.next_sequence,
  };
  return encodeFrame(BROWSER_CONTROL_READY_MAGIC, ready, MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES);
}

/** Main-to-Device reply for the one outstanding, semantically checked submit. */
export async function encodeBrowserControlReply(
  value: BrowserControlReply,
  submit: BrowserControlSubmit,
): Promise<Buffer> {
  const validation = await validateBrowserControlReply(value, submit);
  if (!validation.ok) fail();
  const reply: BrowserControlReply = {
    version: value.version,
    request_id: value.request_id,
    channel_generation: value.channel_generation,
    session_epoch: value.session_epoch,
    scope_id: value.scope_id,
    scope_generation: value.scope_generation,
    sequence: value.sequence,
    intent_id: value.intent_id,
    outcome: value.outcome,
    submitted_at_ms: value.submitted_at_ms ?? null,
    input_end_ms: value.input_end_ms ?? null,
    failure_code: value.failure_code ?? null,
  };
  return encodeFrame(BROWSER_CONTROL_REPLY_MAGIC, reply, MAX_BROWSER_CONTROL_REPLY_BYTES);
}

function skipWhitespace(source: string, from: number): number {
  let index = from;
  while (index < source.length && /[\u0020\u0009\u000a\u000d]/.test(source[index]!)) index += 1;
  return index;
}

/** Scan JSON before JSON.parse so escaped duplicate field names cannot shadow DTO fields. */
function assertUniqueObjectKeys(source: string): void {
  let index = 0;

  const string = (): string => {
    const start = index;
    if (source[index] !== '"') fail();
    index += 1;
    while (index < source.length) {
      const char = source[index]!;
      if (char === '"') {
        index += 1;
        try { return JSON.parse(source.slice(start, index)) as string; }
        catch { return fail(); }
      }
      if (char === '\\') {
        index += 1;
        if (index >= source.length) fail();
      } else if (char < ' ') fail();
      index += 1;
    }
    return fail();
  };

  const value = (depth: number): void => {
    if (depth > 64) fail();
    index = skipWhitespace(source, index);
    const char = source[index];
    if (char === '"') { string(); return; }
    if (char === '{') {
      index += 1;
      index = skipWhitespace(source, index);
      if (source[index] === '}') { index += 1; return; }
      const keys = new Set<string>();
      while (index < source.length) {
        index = skipWhitespace(source, index);
        const key = string();
        if (keys.has(key)) fail();
        keys.add(key);
        index = skipWhitespace(source, index);
        if (source[index] !== ':') fail();
        index += 1;
        value(depth + 1);
        index = skipWhitespace(source, index);
        if (source[index] === '}') { index += 1; return; }
        if (source[index] !== ',') fail();
        index += 1;
      }
      return fail();
    }
    if (char === '[') {
      index += 1;
      index = skipWhitespace(source, index);
      if (source[index] === ']') { index += 1; return; }
      while (index < source.length) {
        value(depth + 1);
        index = skipWhitespace(source, index);
        if (source[index] === ']') { index += 1; return; }
        if (source[index] !== ',') fail();
        index += 1;
      }
      return fail();
    }
    const start = index;
    while (index < source.length && !/[\u0020\u0009\u000a\u000d,\]}]/.test(source[index]!)) index += 1;
    if (index === start) fail();
  };

  value(0);
  if (skipWhitespace(source, index) !== source.length) fail();
}

function parseJson(bytes: Buffer): unknown {
  let source: string;
  // Preserve a BOM as U+FEFF so JSON.parse rejects it instead of silently
  // normalizing a byte sequence that serde_json would refuse.
  try { source = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes); }
  catch { return fail(); }
  try {
    assertUniqueObjectKeys(source);
    return JSON.parse(source) as unknown;
  } catch {
    return fail();
  }
}

/**
 * Main-side decoder for Device-to-Main private control traffic. It requires an
 * exact Hello for the Main bootstrap, then Ready is written before any Submit
 * can be accepted. The callback must finish writing each reply before the next
 * submit is delivered, enforcing one outstanding action and strict sequencing.
 */
export class BrowserControlFrameDecoder {
  private bufferedBytes = Buffer.alloc(0);
  private processing: Promise<void> = Promise.resolve();
  private hello: BrowserControlHello | null = null;
  private nextSequence = 1;
  private disposed = false;
  private readonly bootstrap: BrowserControlBootstrap;

  constructor(bootstrap: BrowserControlBootstrap) {
    if (!validateBrowserControlBootstrap(bootstrap).ok) fail();
    const scope = canonicalScope(bootstrap.scope);
    Object.freeze(scope.context);
    Object.freeze(scope);
    this.bootstrap = Object.freeze({
      version: bootstrap.version,
      token: bootstrap.token,
      channel_generation: bootstrap.channel_generation,
      browser_page_profile: Object.freeze({ kind: 'fixture_background_option' }),
      scope,
    });
  }

  get buffered(): number { return this.bufferedBytes.length; }

  dispose(): void {
    this.disposed = true;
    this.bufferedBytes = Buffer.alloc(0);
  }

  push(chunk: Uint8Array, deliver: (message: BrowserControlInbound) => void | Promise<void>): Promise<void> {
    const operation = this.processing.then(() => this.consume(chunk, deliver));
    this.processing = operation.catch(() => undefined);
    return operation;
  }

  private async consume(chunk: Uint8Array, deliver: (message: BrowserControlInbound) => void | Promise<void>): Promise<void> {
    if (this.disposed) fail();
    try {
      if (chunk.byteLength > MAX_BUFFERED_BYTES
        || this.bufferedBytes.length + chunk.byteLength > MAX_BUFFERED_BYTES) fail();
      if (chunk.byteLength) {
        // push() is serialized across async delivery callbacks. Copy the bytes
        // now so a caller cannot mutate a queued chunk before it is decoded.
        const copied = Buffer.from(chunk);
        this.bufferedBytes = Buffer.concat([this.bufferedBytes, copied]);
      }
      while (!this.disposed && this.bufferedBytes.length >= PREFIX_BYTES) {
        const packet = this.bufferedBytes;
        const magic = INBOUND_MAGICS.find((candidate) =>
          packet.subarray(0, 8).equals(Buffer.from(candidate, 'ascii')));
        if (!magic) fail();
        const maximum = magic === BROWSER_CONTROL_HELLO_MAGIC ? MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES
          : magic === BROWSER_CONTROL_SUBMIT_MAGIC ? MAX_BROWSER_CONTROL_SUBMIT_BYTES : 0;
        if (maximum === 0) fail();
        const bodyLength = packet.readUInt32BE(8);
        if (bodyLength < 2 || bodyLength > maximum) fail();
        const frameLength = PREFIX_BYTES + bodyLength;
        if (packet.length < frameLength) return;
        const decoded = parseJson(packet.subarray(PREFIX_BYTES, frameLength));
        this.bufferedBytes = packet.subarray(frameLength);

        if (magic === BROWSER_CONTROL_HELLO_MAGIC) {
          // Bytes already following Hello arrived before Main could send Ready.
          if (this.hello || this.bufferedBytes.length !== 0) fail();
          const validation = validateBrowserControlHello(decoded, this.bootstrap);
          if (!validation.ok) fail();
          this.hello = validation.value;
          await deliver({ kind: 'hello', value: validation.value });
          continue;
        }

        if (!this.hello) fail();
        // The Device may send no second request until it receives Main's reply.
        // Bytes already queued behind this submit violate that single-flight
        // channel even if the next sequence number is otherwise valid.
        if (this.bufferedBytes.length !== 0) fail();
        const validation = await validateBrowserControlSubmit(decoded);
        if (!validation.ok) fail();
        const submit = validation.value;
        if (submit.channel_generation !== this.bootstrap.channel_generation
          || submit.session_epoch !== this.bootstrap.scope.context.session_epoch
          || submit.scope_id !== this.bootstrap.scope.scope_id
          || submit.scope_generation !== this.bootstrap.scope.generation
          || submit.sequence !== this.nextSequence) fail();
        await deliver({ kind: 'submit', value: submit });
        if (this.nextSequence === Number.MAX_SAFE_INTEGER) this.nextSequence = 0;
        else this.nextSequence += 1;
      }
    } catch {
      this.dispose();
      fail();
    }
  }
}
