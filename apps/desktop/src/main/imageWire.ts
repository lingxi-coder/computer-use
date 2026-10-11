import { createHash } from 'node:crypto';
import { EnvelopeFactory, validateEnvelope, frameGeometry, frameStream, frameWatchId, frameImageId, frameCapture, validSingleCapture, type FrameMeta } from '@computer-use/desktop-api';

export const IMAGE_MAGIC = 'CUIMG001';
export const IMAGE_AUTH = 'CUIAUTH1';
export const IMAGE_READY = 'CUIREADY';
export const MAX_IMAGE_METADATA = 64 * 1024;
export const MAX_IMAGE_BYTES = 12 * 1024 * 1024;
export const MAX_IMAGE_PIXELS = 4 * 1024 * 1024;
export interface ImagePacket { meta: FrameMeta; bytes: Uint8Array }

export function imageMetadata(value: unknown): FrameMeta {
  const envelope = new EnvelopeFactory('image-reader', () => 0).make({ type: 'frame_meta', payload: value as FrameMeta });
  const result = validateEnvelope(envelope);
  if (!result.ok || result.value.message.type !== 'frame_meta') throw new Error('invalid image metadata schema');
  const meta = result.value.message.payload;
  const geometry = frameGeometry(meta);
  const stream = frameStream(meta);
  const bounded = (s: string) => !!s.trim() && Buffer.byteLength(s) <= 256 && !/[\u0000-\u001f\u007f-\u009f]/.test(s);
  const rect = (r: { x: number; y: number; width: number; height: number }, empty = false) =>
    [r.x, r.y, r.width, r.height].every(Number.isFinite) && r.width >= 0 && r.height >= 0 && (empty || (r.width > 0 && r.height > 0));
  if (!geometry || meta.status !== 'complete' || meta.session_epoch <= 0 || meta.source_sequence <= 0
    || !bounded(meta.frame_id) || !bounded(meta.surface.id) || meta.surface.epoch <= 0 || !bounded(meta.clock_domain)
    || !bounded(frameWatchId(meta)!) || !bounded(geometry.window_id) || !frameImageId(meta) || !bounded(frameImageId(meta)!)
    || geometry.pid <= 0 || meta.width_px <= 0 || meta.height_px <= 0 || meta.width_px > 4096 || meta.height_px > 4096
    || meta.width_px * meta.height_px > MAX_IMAGE_PIXELS || meta.byte_length < 57 || meta.byte_length > MAX_IMAGE_BYTES
    || meta.geometry_revision <= 0 || !/^[0-9a-f]{64}$/.test(meta.checksum)
    || !Number.isFinite(meta.scale_factor) || meta.scale_factor <= 0 || !Number.isFinite(geometry.content_scale) || geometry.content_scale <= 0
    || !rect(geometry.window_frame) || !rect(geometry.content_rect)
    || (stream && (stream.dirty_rects.length > 256 || !stream.dirty_rects.every((r) => rect(r, true))
      || stream.callback_ms < meta.source_ms || stream.display_time_ticks <= 0 || stream.display_time_ns <= 0
      || stream.received_uptime_ns < stream.display_time_ns || stream.source_clock !== 'mach_absolute'))
    || (frameCapture(meta) && !validSingleCapture(meta))) {
    throw new Error('image metadata exceeds bounds or lacks complete provenance');
  }
  return meta;
}

export function validatePng(meta: FrameMeta, bytes: Uint8Array): void {
  if (bytes.length !== meta.byte_length || bytes.length < 57 || Buffer.from(bytes.subarray(0, 8)).toString('hex') !== '89504e470d0a1a0a') {
    throw new Error('PNG signature or length does not match metadata');
  }
  const data = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = 8; let hasData = false; let end = false;
  while (offset + 12 <= bytes.length) {
    const length = data.getUint32(offset); const next = offset + length + 12;
    if (next > bytes.length) throw new Error('PNG chunk exceeds bounds');
    const kind = String.fromCharCode(...bytes.subarray(offset + 4, offset + 8));
    if (offset === 8) {
      if (kind !== 'IHDR' || length !== 13 || data.getUint32(offset + 8) !== meta.width_px
        || data.getUint32(offset + 12) !== meta.height_px || bytes[offset + 16] !== 8
        || ![2, 6].includes(bytes[offset + 17]!) || bytes[offset + 18] !== 0
        || bytes[offset + 19] !== 0 || bytes[offset + 20] !== 0) throw new Error('PNG dimensions or format do not match metadata');
    } else if (['IHDR', 'acTL', 'fcTL', 'fdAT'].includes(kind)) throw new Error('PNG contains another image or animation');
    else if (kind === 'IDAT') hasData = true;
    else if (kind === 'IEND') {
      if (length !== 0 || next !== bytes.length) throw new Error('PNG has trailing content');
      end = true;
    }
    offset = next;
  }
  if (offset !== bytes.length || !hasData || !end || createHash('sha256').update(bytes).digest('hex') !== meta.checksum) {
    throw new Error('PNG is incomplete or checksum does not match');
  }
}

/** Incremental allocation: validate the fixed prefix, then metadata, then the
 * bounded payload. State and failure persist across fragmented socket reads. */
export class ImageDecoder {
  private prefix = new Uint8Array(16);
  private header: Uint8Array | null = null;
  private bytes: Uint8Array | null = null;
  private meta: FrameMeta | null = null;
  private used = 0;
  private length = 0;
  private failed = false;
  get buffered(): number { return this.used + (this.header ? 16 : 0) + (this.bytes ? this.header!.length : 0); }
  get incomplete(): boolean { return this.used !== 0 || this.header !== null; }
  push(chunk: Uint8Array, deliver: (packet: ImagePacket) => void): void {
    if (this.failed) throw new Error('image decoder is failed');
    try {
      let offset = 0;
      while (offset < chunk.length) {
        const target = this.bytes ?? this.header ?? this.prefix;
        const count = Math.min(target.length - this.used, chunk.length - offset);
        target.set(chunk.subarray(offset, offset + count), this.used); this.used += count; offset += count;
        if (this.used !== target.length) continue;
        this.used = 0;
        if (!this.header) {
          const data = new DataView(this.prefix.buffer);
          const headerLength = data.getUint32(8); this.length = data.getUint32(12);
          if (String.fromCharCode(...this.prefix.subarray(0, 8)) !== IMAGE_MAGIC || headerLength < 2
            || headerLength > MAX_IMAGE_METADATA || this.length < 57 || this.length > MAX_IMAGE_BYTES) throw new Error('image packet prefix exceeds bounds');
          this.header = new Uint8Array(headerLength);
        } else if (!this.bytes) {
          this.meta = imageMetadata(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(this.header)));
          if (this.meta.byte_length !== this.length) throw new Error('image packet length disagrees with metadata');
          this.bytes = new Uint8Array(this.length);
        } else {
          const packet = { meta: this.meta!, bytes: this.bytes };
          validatePng(packet.meta, packet.bytes);
          this.header = null; this.bytes = null; this.meta = null;
          deliver(packet);
        }
      }
    } catch (error) { this.failed = true; this.header = null; this.bytes = null; throw error; }
  }
}
