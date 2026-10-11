import { connect } from 'node:net';
import { parentPort, workerData } from 'node:worker_threads';
import { IMAGE_AUTH, IMAGE_READY, ImageDecoder, type ImagePacket } from './imageWire.js';
import { frameCapture, frameGeometry, frameStream, frameWatchId } from '@computer-use/desktop-api';

const port = parentPort!;
const { path, token, epoch, pid } = workerData as { path: string; token: string; epoch: number; pid?: number };
const stream = connect(path);
const decoder = new ImageDecoder();
let ready = false; let ack = Buffer.alloc(0); let inFlight = false;
let pending: ImagePacket | null = null;
let pendingCapture: ImagePacket | null = null;
let rateWindow = performance.now(); let rateCount = 0;
let streamId: string | null = null; let sequence = 0; let display = 0;
const retired = new Set<string>();
let closed = false;
let partialTimer: ReturnType<typeof setTimeout> | null = null;
function fail(reason: string): void {
  if (closed) return;
  closed = true; pending = null; pendingCapture = null;
  if (partialTimer) clearTimeout(partialTimer);
  stream.destroy(); port.postMessage({ kind: 'closed', reason }); port.close();
}
function flush(): void {
  if (!inFlight && (pendingCapture || pending) && !closed) {
    const frame = pendingCapture ?? pending!;
    if (pendingCapture) pendingCapture = null; else pending = null;
    inFlight = true;
    port.postMessage({ kind: 'image', meta: frame.meta, bytes: frame.bytes.buffer }, [frame.bytes.buffer as ArrayBuffer]);
  }
}
port.on('message', (message: { kind: string }) => {
  if (message.kind === 'ack') { inFlight = false; flush(); }
  else if (message.kind === 'close') fail('image consumer closed');
});
stream.setTimeout(2000); // Handshake and partial packets only; idle sends no PNG.
stream.on('connect', () => {
  const auth = Buffer.alloc(48); auth.write(IMAGE_AUTH); auth.write(token, 8, 32, 'ascii'); auth.writeBigUInt64BE(BigInt(epoch), 40);
  stream.write(auth);
});
stream.on('data', (chunk: Buffer) => {
  try {
    if (!ready) {
      const count = Math.min(8 - ack.length, chunk.length);
      ack = Buffer.concat([ack, chunk.subarray(0, count)]); chunk = chunk.subarray(count);
      if (ack.length < 8) return;
      if (ack.toString('ascii') !== IMAGE_READY) throw new Error('image authentication refused');
      ready = true; port.postMessage({ kind: 'ready' });
    }
    decoder.push(chunk, (frame) => {
      if (partialTimer) clearTimeout(partialTimer);
      partialTimer = null;
      const info = frameGeometry(frame.meta)!;
      const watch = frameWatchId(frame.meta)!;
      const capture = frameCapture(frame.meta);
      if (frame.meta.session_epoch !== epoch || (pid !== undefined && info.pid !== pid)) throw new Error('image changed device session or target PID');
      if (performance.now() - rateWindow >= 1000) { rateWindow = performance.now(); rateCount = 0; }
      if (++rateCount > 8) throw new Error('image rate exceeds limit');
      if (capture && watch !== streamId) throw new Error('capture belongs to an unestablished watch');
      if (watch !== streamId) {
        if (retired.has(watch) || retired.size >= 128) throw new Error('image replayed a retired stream');
        if (streamId) retired.add(streamId);
        streamId = watch; sequence = 0; display = 0; pendingCapture = null;
      }
      const streamInfo = frameStream(frame.meta);
      if (frame.meta.source_sequence <= sequence || (streamInfo && streamInfo.display_time_ns <= display)) throw new Error('image sequence or original display time replayed');
      sequence = frame.meta.source_sequence;
      if (streamInfo) display = streamInfo.display_time_ns;
      if (capture) {
        if (pendingCapture) throw new Error('capture retention is full');
        pendingCapture = frame;
      } else pending = frame;
      flush();
    });
    stream.setTimeout(decoder.incomplete ? 2000 : 0);
    if (decoder.incomplete && !partialTimer) partialTimer = setTimeout(() => fail('image partial packet exceeded its total deadline'), 2000);
    if (!decoder.incomplete && partialTimer) { clearTimeout(partialTimer); partialTimer = null; }
  } catch (error) { fail(error instanceof Error ? error.message : String(error)); }
});
stream.on('timeout', () => fail('image handshake or partial packet timed out'));
stream.on('error', () => fail('image connection failed'));
stream.on('end', () => fail(decoder.incomplete ? 'image packet truncated' : 'image source closed'));
