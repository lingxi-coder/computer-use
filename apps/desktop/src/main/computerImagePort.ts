import { Worker } from 'node:worker_threads';
import type { Duplex } from 'node:stream';

import {
  validateComputerBatchProposal,
  validateComputerFence,
  validateComputerFrameRequest,
  frameGeometry,
  frameWatchId,
  frameImageId,
  frameCapture,
  type ComputerBatchProposal,
  type ComputerFence,
  type ComputerFrameRequest,
  type FrameMeta,
  type UiProjection,
} from '@computer-use/desktop-api';

import type { ImagePacket } from './imageWire.js';

export const COMPUTER_IMAGE_REQUEST_MAGIC = 'CUCIREQ1';
export const COMPUTER_IMAGE_REJECTION_MAGIC = 'CUCIREJ1';
export const COMPUTER_SAFETY_REQUEST_MAGIC = 'CUCISAF1';
export const COMPUTER_SAFETY_REPLY_MAGIC = 'CUCISAR1';
export const MAX_COMPUTER_IMAGE_REQUEST_BYTES = 64 * 1024;
export const MAX_COMPUTER_IMAGE_REJECTION_BYTES = 4 * 1024;
export const MAX_COMPUTER_SAFETY_REQUEST_BYTES = 1024 * 1024;
export const MAX_COMPUTER_SAFETY_CHECKS = 32;
export const MAX_COMPUTER_SAFETY_ID_BYTES = 64;
export const MAX_COMPUTER_SAFETY_CODE_BYTES = 64;
export const MAX_COMPUTER_SAFETY_MESSAGE_BYTES = 4096;
export const MAX_COMPUTER_IMAGE_REQUEST_PACKET_BYTES = 12 + MAX_COMPUTER_IMAGE_REQUEST_BYTES;
export const MAX_COMPUTER_IMAGE_PIXEL_AGE_MS = 2_000;
const PREFIX_BYTES = 12;
const MAX_PORT_OPERATION_MS = 2_000;
// Leave headroom before Host's 30-second confirmation read timeout.
const MAX_SAFETY_PROMPT_MS = 27_000;
const MAX_REMEMBERED_REQUEST_IDS = 1_024;
const MAX_PORT_BUFFERED_BYTES = 2 * (PREFIX_BYTES + MAX_COMPUTER_SAFETY_REQUEST_BYTES);

export type ComputerImageRejectionReason =
  | 'source_unavailable'
  | 'stale_fence'
  | 'frame_unavailable'
  | 'frame_mismatch'
  | 'deadline_expired'
  | 'busy';

export interface ComputerImageRejection {
  fence: ComputerFence;
  reason: ComputerImageRejectionReason;
}

export interface ComputerSafetyCheck {
  id: string;
  code?: string;
  message?: string;
}

/** Private fd5 confirmation request; it is not part of desktop/API envelopes. */
export interface ComputerSafetyRequest {
  fence: ComputerFence;
  pending_safety_checks: ComputerSafetyCheck[];
  proposal?: ComputerBatchProposal;
}

export interface ComputerSafetyReply {
  fence: ComputerFence;
  approved: boolean;
}

export type ComputerPortRequest =
  | { kind: 'frame'; request: ComputerFrameRequest }
  | { kind: 'safety'; request: ComputerSafetyRequest };

export type ConfirmComputerSafety = (request: ComputerSafetyRequest, signal: AbortSignal) => Promise<boolean>;

export interface ComputerImagePortSnapshot {
  projection: UiProjection | null;
  projectionAt: number;
  imageControl: FrameMeta | null;
  imagePacket: ImagePacket | null;
}

export type ComputerImagePortEncoder = (
  request: ComputerFrameRequest,
  image: ImagePacket,
  nowMs: number,
) => Promise<Uint8Array>;

export interface ComputerImagePortOptions {
  clockOriginMs: number;
  targetBundleId: string;
  targetPid: number;
  targetWindowId: string;
  isComputerAuthorizationCurrent: () => boolean;
  confirmComputerSafety: ConfirmComputerSafety;
  current: () => ComputerImagePortSnapshot;
  nowUnixMs?: () => number;
  encoder?: ComputerImagePortEncoder;
}

function fail(message: string): never {
  throw new Error(message);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function exactKeys(value: Record<string, unknown>, names: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const expected = [...names].sort();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${canonical(record[key])}`).join(',')}}`;
  }
  const encoded = JSON.stringify(value);
  if (encoded === undefined) throw new Error('value is not JSON encodable');
  return encoded;
}

function sameJson(left: unknown, right: unknown): boolean {
  return canonical(left) === canonical(right);
}

export function encodeComputerFrameRequest(request: ComputerFrameRequest): Uint8Array {
  if (!validateComputerFrameRequest(request).ok) fail('computer frame request is invalid');
  const body = Buffer.from(JSON.stringify(request), 'utf8');
  if (body.length < 2 || body.length > MAX_COMPUTER_IMAGE_REQUEST_BYTES) {
    fail('computer frame request exceeds bounds');
  }
  const packet = Buffer.allocUnsafe(PREFIX_BYTES + body.length);
  packet.write(COMPUTER_IMAGE_REQUEST_MAGIC, 0, 'ascii');
  packet.writeUInt32BE(body.length, 8);
  body.copy(packet, PREFIX_BYTES);
  return packet;
}

function validBoundedText(value: unknown, maxBytes: number, allowBlank: boolean): value is string {
  return typeof value === 'string' && Buffer.byteLength(value, 'utf8') <= maxBytes
    && (allowBlank || value.trim().length > 0) && !/\p{Cc}/u.test(value);
}

export function validateComputerSafetyRequest(value: unknown): value is ComputerSafetyRequest {
  if (!isRecord(value) || !validateComputerFence(value['fence']).ok
    || !Array.isArray(value['pending_safety_checks'])
    || value['pending_safety_checks'].length < 1
    || value['pending_safety_checks'].length > MAX_COMPUTER_SAFETY_CHECKS) return false;
  const expectedKeys = value['proposal'] === undefined ? ['fence', 'pending_safety_checks']
    : ['fence', 'pending_safety_checks', 'proposal'];
  if (!exactKeys(value, expectedKeys)) return false;

  const seenIds = new Set<string>();
  for (const check of value['pending_safety_checks']) {
    if (!isRecord(check)) return false;
    const id = check['id'];
    const checkKeys = ['id', ...(check['code'] === undefined ? [] : ['code']),
      ...(check['message'] === undefined ? [] : ['message'])];
    if (!validBoundedText(id, MAX_COMPUTER_SAFETY_ID_BYTES, false)
      || !exactKeys(check, checkKeys) || seenIds.has(id)) return false;
    seenIds.add(id);
    if (check['code'] !== undefined && !validBoundedText(check['code'], MAX_COMPUTER_SAFETY_CODE_BYTES, false)) return false;
    if (check['message'] !== undefined && !validBoundedText(check['message'], MAX_COMPUTER_SAFETY_MESSAGE_BYTES, true)) return false;
  }
  if (value['proposal'] !== undefined) {
    const proposal = value['proposal'];
    if (!validateComputerBatchProposal(proposal).ok || !isRecord(proposal)
      || !sameJson(proposal['fence'], value['fence'])) return false;
  }
  return true;
}

export function encodeComputerSafetyRequest(request: ComputerSafetyRequest): Uint8Array {
  if (!validateComputerSafetyRequest(request)) fail('computer safety request is invalid');
  const body = Buffer.from(JSON.stringify(request), 'utf8');
  if (body.length < 2 || body.length > MAX_COMPUTER_SAFETY_REQUEST_BYTES) {
    fail('computer safety request exceeds bounds');
  }
  const packet = Buffer.allocUnsafe(PREFIX_BYTES + body.length);
  packet.write(COMPUTER_SAFETY_REQUEST_MAGIC, 0, 'ascii');
  packet.writeUInt32BE(body.length, 8);
  body.copy(packet, PREFIX_BYTES);
  return packet;
}

export function encodeComputerSafetyReply(reply: ComputerSafetyReply): Uint8Array {
  if (!validateComputerFence(reply.fence).ok || typeof reply.approved !== 'boolean') {
    fail('computer safety reply is invalid');
  }
  const body = Buffer.from(JSON.stringify(reply), 'utf8');
  if (body.length < 2 || body.length > MAX_COMPUTER_IMAGE_REJECTION_BYTES) {
    fail('computer safety reply exceeds bounds');
  }
  const packet = Buffer.allocUnsafe(PREFIX_BYTES + body.length);
  packet.write(COMPUTER_SAFETY_REPLY_MAGIC, 0, 'ascii');
  packet.writeUInt32BE(body.length, 8);
  body.copy(packet, PREFIX_BYTES);
  return packet;
}

export function decodeComputerSafetyReply(bytes: Uint8Array, expectedFence?: ComputerFence): ComputerSafetyReply {
  if (bytes.byteLength < PREFIX_BYTES || bytes.byteLength > PREFIX_BYTES + MAX_COMPUTER_IMAGE_REJECTION_BYTES) {
    fail('computer safety reply length exceeds bounds');
  }
  const packet = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (packet.subarray(0, 8).toString('ascii') !== COMPUTER_SAFETY_REPLY_MAGIC) fail('computer safety reply magic is invalid');
  const length = packet.readUInt32BE(8);
  if (length < 2 || length > MAX_COMPUTER_IMAGE_REJECTION_BYTES || packet.length !== PREFIX_BYTES + length) {
    fail('computer safety reply body length is invalid');
  }
  let value: unknown;
  try { value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(packet.subarray(PREFIX_BYTES))); }
  catch { fail('computer safety reply JSON is invalid'); }
  if (!isRecord(value) || !exactKeys(value, ['fence', 'approved']) || !validateComputerFence(value['fence']).ok
    || typeof value['approved'] !== 'boolean') fail('computer safety reply contract is invalid');
  const reply = value as unknown as ComputerSafetyReply;
  if (expectedFence && !sameJson(reply.fence, expectedFence)) fail('computer safety reply correlation is invalid');
  return reply;
}

export class ComputerPortRequestDecoder {
  private bufferedBytes = Buffer.alloc(0);

  get buffered(): number { return this.bufferedBytes.length; }

  reset(): void { this.bufferedBytes = Buffer.alloc(0); }

  push(chunk: Uint8Array, deliver: (request: ComputerPortRequest) => void): void {
    if (chunk.byteLength > MAX_PORT_BUFFERED_BYTES
      || this.bufferedBytes.length + chunk.byteLength > MAX_PORT_BUFFERED_BYTES) {
      this.reset(); fail('computer port request batch exceeds bounds');
    }
    if (chunk.byteLength) this.bufferedBytes = Buffer.concat([this.bufferedBytes, Buffer.from(chunk)]);
    while (this.bufferedBytes.length >= PREFIX_BYTES) {
      const packet = this.bufferedBytes;
      const magic = packet.subarray(0, 8).toString('ascii');
      const maxLength = magic === COMPUTER_IMAGE_REQUEST_MAGIC ? MAX_COMPUTER_IMAGE_REQUEST_BYTES
        : magic === COMPUTER_SAFETY_REQUEST_MAGIC ? MAX_COMPUTER_SAFETY_REQUEST_BYTES : 0;
      if (!maxLength) { this.reset(); fail('computer port request magic is invalid'); }
      const length = packet.readUInt32BE(8);
      if (length < 2 || length > maxLength) {
        this.reset(); fail('computer port request length exceeds bounds');
      }
      const packetLength = PREFIX_BYTES + length;
      if (packet.length < packetLength) return;
      let decoded: unknown;
      try {
        decoded = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(packet.subarray(PREFIX_BYTES, packetLength)));
      } catch {
        this.reset(); fail('computer port request JSON is invalid');
      }
      let delivered: ComputerPortRequest;
      if (magic === COMPUTER_IMAGE_REQUEST_MAGIC) {
        if (!validateComputerFrameRequest(decoded).ok) { this.reset(); fail('computer frame request contract is invalid'); }
        delivered = { kind: 'frame', request: decoded as ComputerFrameRequest };
      } else {
        if (!validateComputerSafetyRequest(decoded)) { this.reset(); fail('computer safety request contract is invalid'); }
        delivered = { kind: 'safety', request: decoded };
      }
      this.bufferedBytes = packet.subarray(packetLength);
      deliver(delivered);
    }
  }
}

export function encodeComputerImageRejection(rejection: ComputerImageRejection): Uint8Array {
  if (!validateComputerFence(rejection.fence).ok
    || !['source_unavailable', 'stale_fence', 'frame_unavailable', 'frame_mismatch', 'deadline_expired', 'busy'].includes(rejection.reason)) {
    fail('computer image rejection is invalid');
  }
  const body = Buffer.from(JSON.stringify(rejection), 'utf8');
  if (body.length < 2 || body.length > MAX_COMPUTER_IMAGE_REJECTION_BYTES) {
    fail('computer image rejection exceeds bounds');
  }
  const packet = Buffer.allocUnsafe(PREFIX_BYTES + body.length);
  packet.write(COMPUTER_IMAGE_REJECTION_MAGIC, 0, 'ascii');
  packet.writeUInt32BE(body.length, 8);
  body.copy(packet, PREFIX_BYTES);
  return packet;
}

export function decodeComputerImageRejection(
  bytes: Uint8Array,
  expectedFence?: ComputerFence,
): ComputerImageRejection {
  if (bytes.byteLength < PREFIX_BYTES || bytes.byteLength > PREFIX_BYTES + MAX_COMPUTER_IMAGE_REJECTION_BYTES) {
    fail('computer image rejection length exceeds bounds');
  }
  const packet = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (packet.subarray(0, 8).toString('ascii') !== COMPUTER_IMAGE_REJECTION_MAGIC) {
    fail('computer image rejection magic is invalid');
  }
  const length = packet.readUInt32BE(8);
  if (length < 2 || length > MAX_COMPUTER_IMAGE_REJECTION_BYTES || packet.length !== PREFIX_BYTES + length) {
    fail('computer image rejection body length is invalid');
  }
  let value: unknown;
  try { value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(packet.subarray(PREFIX_BYTES))); }
  catch { fail('computer image rejection JSON is invalid'); }
  if (!isRecord(value) || !exactKeys(value, ['fence', 'reason']) || !validateComputerFence(value['fence']).ok
    || !['source_unavailable', 'stale_fence', 'frame_unavailable', 'frame_mismatch', 'deadline_expired', 'busy'].includes(String(value['reason']))) {
    fail('computer image rejection contract is invalid');
  }
  const rejection = value as unknown as ComputerImageRejection;
  if (expectedFence && !sameJson(rejection.fence, expectedFence)) {
    fail('computer image rejection correlation is invalid');
  }
  return rejection;
}

const INPUT_ROOT_STATES = new Set(['running', 'waiting_evidence', 'waiting_plan']);

/** Gate one request against Main's current Host projection, Source metadata, and pixels. */
export function computerImageRequestRejection(
  request: ComputerFrameRequest,
  snapshot: ComputerImagePortSnapshot,
  clockOriginMs: number,
  targetBundleId: string,
  nowUnixMs: number,
): ComputerImageRejectionReason | null {
  const validation = validateComputerFrameRequest(request);
  if (!validation.ok) return 'frame_mismatch';
  const nowMs = nowUnixMs - clockOriginMs;
  if (!Number.isSafeInteger(nowUnixMs) || !Number.isSafeInteger(clockOriginMs) || nowMs >= request.fence.deadline_ms) {
    return 'deadline_expired';
  }
  const projection = snapshot.projection;
  const root = projection?.root;
  if (!projection || !root || !INPUT_ROOT_STATES.has(root.state)) return 'stale_fence';
  const fence = request.fence;
  if (root.root_task_id !== fence.root_task_id || root.task_revision !== fence.task_revision || root.root_epoch !== fence.root_epoch) {
    return 'stale_fence';
  }

  type CurrentTarget = NonNullable<NonNullable<UiProjection['observation']>['observed_target']> & {
    structure_sequence?: number;
  };
  const target = projection.observation?.observed_target as CurrentTarget | null | undefined;
  if (!target || !Number.isSafeInteger(target.structure_sequence)) return 'source_unavailable';
  if (target.snapshot_id !== fence.snapshot_id || target.structure_sequence !== fence.structure_sequence) return 'stale_fence';

  const frame = request.expected_frame;
  const geometry = frameGeometry(frame);
  const identity = projection.capabilities?.identity;
  if (!geometry || !identity || target.app !== targetBundleId || target.session_epoch !== frame.session_epoch
    || identity.session_epoch !== frame.session_epoch || identity.surface.id !== frame.surface.id
    || identity.surface.epoch !== frame.surface.epoch || identity.clock_domain !== frame.clock_domain
    || target.pid !== geometry.pid || target.window_id !== geometry.window_id) return 'stale_fence';
  if ((frameCapture(frame)?.capture_finished_ms ?? frame.source_ms) > nowMs) return 'frame_unavailable';
  if (nowMs >= target.valid_until_ms) return 'stale_fence';

  const imageControl = snapshot.imageControl;
  const imagePacket = snapshot.imagePacket;
  if (!imageControl || !imagePacket) return 'source_unavailable';
  if (imageControl.status !== 'complete' || !sameJson(imageControl, frame)
    || !sameJson(imagePacket.meta, frame)) return 'frame_mismatch';

  const view = projection.observation?.frame;
  if (!view || !view.healthy || !['complete', 'idle'].includes(view.status) || view.stream_id !== frameWatchId(frame)
    || view.frame_id !== frameImageId(frame) || view.geometry_revision !== frame.geometry_revision
    || view.width_px !== frame.width_px || view.height_px !== frame.height_px
    || view.pixel_age_ms === null || view.pixel_age_ms === undefined || view.pixel_age_ms >= MAX_COMPUTER_IMAGE_PIXEL_AGE_MS
    || nowUnixMs < snapshot.projectionAt || nowUnixMs - snapshot.projectionAt >= MAX_COMPUTER_IMAGE_PIXEL_AGE_MS) {
    return 'frame_unavailable';
  }
  const pixelAgeMs = nowMs - frame.source_ms;
  if (pixelAgeMs < 0 || pixelAgeMs >= MAX_COMPUTER_IMAGE_PIXEL_AGE_MS) return 'frame_unavailable';
  return null;
}

/** Require the live Root and a fresh, monotonic source scope before a safety dialog. */
export function computerSafetyRequestCanPrompt(
  request: ComputerSafetyRequest,
  snapshot: ComputerImagePortSnapshot,
  clockOriginMs: number,
  targetBundleId: string,
  targetPid: number,
  targetWindowId: string,
  authorizationIsCurrent: () => boolean,
  nowUnixMs: number,
): boolean {
  const nowMs = nowUnixMs - clockOriginMs;
  if (!validateComputerSafetyRequest(request) || !Number.isSafeInteger(nowUnixMs)
    || !Number.isSafeInteger(clockOriginMs) || !Number.isSafeInteger(nowMs) || nowMs < 0
    || nowMs >= request.fence.deadline_ms) return false;
  try { if (!authorizationIsCurrent()) return false; } catch { return false; }
  const projection = snapshot.projection;
  const root = projection?.root;
  if (!projection || !root || !INPUT_ROOT_STATES.has(root.state)) return false;
  const fence = request.fence;
  if (root.root_task_id !== fence.root_task_id || root.task_revision !== fence.task_revision
    || root.root_epoch !== fence.root_epoch) return false;

  type CurrentTarget = NonNullable<NonNullable<UiProjection['observation']>['observed_target']> & {
    structure_sequence?: number;
  };
  const target = projection.observation?.observed_target as CurrentTarget | null | undefined;
  if (!target || !Number.isSafeInteger(target.structure_sequence)
    || typeof target.snapshot_id !== 'string' || target.snapshot_id.trim().length === 0
    || target.structure_sequence < fence.structure_sequence
    || (target.structure_sequence === fence.structure_sequence && target.snapshot_id !== fence.snapshot_id)
    || target.app !== targetBundleId || target.pid !== targetPid || target.window_id !== targetWindowId
    || !Number.isSafeInteger(target.session_epoch) || target.session_epoch < 1
    || !Number.isSafeInteger(target.valid_until_ms) || nowMs >= target.valid_until_ms) return false;
  if (!Number.isSafeInteger(snapshot.projectionAt) || nowUnixMs < snapshot.projectionAt
    || nowUnixMs - snapshot.projectionAt >= MAX_COMPUTER_IMAGE_PIXEL_AGE_MS) return false;
  const identity = projection.capabilities?.identity;
  return identity !== undefined && Number.isSafeInteger(identity.session_epoch)
    && identity.session_epoch === target.session_epoch;
}

interface ActiveRequest {
  request: ComputerPortRequest;
  epoch: number;
  timer: ReturnType<typeof setTimeout> | null;
  writing: boolean;
  closeAfterWrite: boolean;
  safetyAbort?: AbortController;
}

interface QueuedBusyRequest {
  request: ComputerPortRequest;
  fresh: boolean;
}

function runEncoderWorker(request: ComputerFrameRequest, image: ImagePacket, nowMs: number): {
  worker: Worker;
  result: Promise<Uint8Array>;
} {
  const extension = import.meta.url.endsWith('.ts') ? 'ts' : 'js';
  const worker = new Worker(new URL(`./computerImagePortWorker.${extension}`, import.meta.url));
  const copiedPixels = Uint8Array.from(image.bytes);
  const result = new Promise<Uint8Array>((resolve, reject) => {
    let settled = false;
    const finish = (callback: () => void): void => {
      if (settled) return;
      settled = true;
      callback();
    };
    worker.once('message', (message: { kind?: string; bytes?: ArrayBuffer }) => {
      const bytes = message.bytes;
      if (message.kind !== 'encoded' || !(bytes instanceof ArrayBuffer)) {
        finish(() => reject(new Error('computer image worker rejected the frame')));
      } else finish(() => resolve(new Uint8Array(bytes)));
    });
    worker.once('error', () => finish(() => reject(new Error('computer image worker failed'))));
    worker.once('exit', (code) => {
      if (code !== 0) finish(() => reject(new Error('computer image worker exited')));
    });
    try {
      worker.postMessage({ request, image: { meta: image.meta, bytes: copiedPixels.buffer }, nowMs }, [copiedPixels.buffer]);
    } catch {
      finish(() => reject(new Error('computer image worker could not start')));
    }
  });
  return { worker, result };
}

/** Private fd5 service for exact images and native safety prompts. */
export class ComputerImagePort {
  private readonly decoder = new ComputerPortRequestDecoder();
  private active: ActiveRequest | null = null;
  private queuedBusy: QueuedBusyRequest | null = null;
  private worker: Worker | null = null;
  private partialTimer: ReturnType<typeof setTimeout> | null = null;
  private epoch = 1;
  private lastFrameGeneration = 0;
  private readonly seenFrameRequestIds = new Set<string>();
  private readonly frameRequestOrder: string[] = [];
  private lastSafetyGeneration = 0;
  private readonly seenSafetyFences = new Set<string>();
  private readonly safetyFenceOrder: string[] = [];
  private closed = false;

  constructor(private readonly stream: Duplex, private readonly options: ComputerImagePortOptions) {
    stream.on('data', this.onData);
    stream.on('error', this.onStreamError);
    stream.on('close', this.onStreamClose);
    stream.on('end', this.onStreamClose);
  }

  private readonly onData = (chunk: Buffer): void => {
    if (this.closed) return;
    try {
      this.decoder.push(chunk, (request) => this.accept(request));
      if (this.decoder.buffered > 0 && !this.partialTimer) {
        this.partialTimer = setTimeout(() => this.close(), MAX_PORT_OPERATION_MS);
        this.partialTimer.unref?.();
      } else if (this.decoder.buffered === 0 && this.partialTimer) {
        clearTimeout(this.partialTimer); this.partialTimer = null;
      }
    }
    catch { this.close(); }
  };

  private readonly onStreamError = (): void => this.close(false);
  private readonly onStreamClose = (): void => this.close(false);

  private consumeFrameGeneration(request: ComputerFrameRequest): boolean {
    const { generation, request_id: requestId } = request.fence;
    if (generation <= this.lastFrameGeneration || this.seenFrameRequestIds.has(requestId)) return false;
    this.lastFrameGeneration = generation;
    this.seenFrameRequestIds.add(requestId);
    this.frameRequestOrder.push(requestId);
    if (this.frameRequestOrder.length > MAX_REMEMBERED_REQUEST_IDS) {
      const retired = this.frameRequestOrder.shift()!;
      this.seenFrameRequestIds.delete(retired);
    }
    return true;
  }

  private consumeSafetyFence(request: ComputerSafetyRequest): boolean {
    const { generation } = request.fence;
    const key = canonical(request.fence);
    if (generation <= this.lastSafetyGeneration || this.seenSafetyFences.has(key)) return false;
    this.lastSafetyGeneration = generation;
    this.seenSafetyFences.add(key);
    this.safetyFenceOrder.push(key);
    if (this.safetyFenceOrder.length > MAX_REMEMBERED_REQUEST_IDS) {
      const retired = this.safetyFenceOrder.shift()!;
      this.seenSafetyFences.delete(retired);
    }
    return true;
  }

  private accept(message: ComputerPortRequest): void {
    if (this.closed) return;
    const fresh = message.kind === 'frame'
      ? this.consumeFrameGeneration(message.request)
      : this.consumeSafetyFence(message.request);
    if (this.active) {
      if (this.queuedBusy) { this.close(); return; }
      this.queuedBusy = { request: message, fresh };
      this.stream.pause();
      return;
    }
    if (message.kind === 'frame') {
      const rejection = fresh
        ? this.currentRejection(message.request, (this.options.nowUnixMs ?? Date.now)())
        : 'stale_fence';
      this.begin(message, rejection);
    } else {
      const canPrompt = fresh && this.currentSafetyCanPrompt(message.request, (this.options.nowUnixMs ?? Date.now)());
      this.begin(message, canPrompt ? null : 'stale_fence');
    }
  }

  private currentRejection(request: ComputerFrameRequest, nowUnixMs: number): ComputerImageRejectionReason | null {
    try {
      return computerImageRequestRejection(request, this.options.current(), this.options.clockOriginMs,
        this.options.targetBundleId, nowUnixMs);
    } catch {
      return 'source_unavailable';
    }
  }

  private currentSafetyCanPrompt(request: ComputerSafetyRequest, nowUnixMs: number): boolean {
    try {
      return computerSafetyRequestCanPrompt(request, this.options.current(), this.options.clockOriginMs,
        this.options.targetBundleId, this.options.targetPid, this.options.targetWindowId,
        this.options.isComputerAuthorizationCurrent, nowUnixMs);
    } catch {
      return false;
    }
  }

  private begin(message: ComputerPortRequest, rejection: ComputerImageRejectionReason | null): void {
    if (this.closed) return;
    this.stream.pause();
    const active: ActiveRequest = { request: message, epoch: this.epoch, timer: null, writing: false, closeAfterWrite: false };
    this.active = active;
    if (message.kind === 'safety') {
      if (rejection) { this.writeSafetyReply(active, false); return; }
      this.beginSafetyPrompt(active, message.request);
      return;
    }
    this.beginFrameRequest(active, message.request, rejection);
  }

  private beginFrameRequest(
    active: ActiveRequest,
    request: ComputerFrameRequest,
    rejection: ComputerImageRejectionReason | null,
  ): void {
    if (rejection) { this.writeRejection(active, rejection); return; }
    let snapshot: ComputerImagePortSnapshot;
    try { snapshot = this.options.current(); }
    catch { this.writeRejection(active, 'source_unavailable'); return; }
    const nowUnixMs = (this.options.nowUnixMs ?? Date.now)();
    const nowMs = nowUnixMs - this.options.clockOriginMs;
    const pixelRemaining = MAX_COMPUTER_IMAGE_PIXEL_AGE_MS - (nowMs - request.expected_frame.source_ms);
    const deadlineRemaining = request.fence.deadline_ms - nowMs;
    const timeoutMs = Math.max(1, Math.min(MAX_PORT_OPERATION_MS, pixelRemaining, deadlineRemaining));
    active.timer = setTimeout(() => {
      const currentNow = (this.options.nowUnixMs ?? Date.now)() - this.options.clockOriginMs;
      this.writeRejection(active, currentNow >= request.fence.deadline_ms ? 'deadline_expired' : 'frame_unavailable');
    }, timeoutMs);
    active.timer.unref?.();

    const image = snapshot.imagePacket;
    if (!image) { this.writeRejection(active, 'source_unavailable'); return; }
    let task: { worker: Worker | null; result: Promise<Uint8Array> };
    try {
      task = this.options.encoder
        ? { worker: null, result: this.options.encoder(request, image, nowMs) }
        : runEncoderWorker(request, image, nowMs);
    } catch {
      this.writeRejection(active, 'frame_unavailable'); return;
    }
    this.worker = task.worker;
    void task.result.then((bytes) => {
      if (this.closed || this.active !== active || active.epoch !== this.epoch) return;
      this.worker?.terminate().catch(() => undefined);
      this.worker = null;
      const now = (this.options.nowUnixMs ?? Date.now)();
      const currentReason = this.currentRejection(request, now);
      if (currentReason) { this.writeRejection(active, currentReason); return; }
      this.write(active, bytes);
    }).catch(() => {
      if (this.closed || this.active !== active || active.epoch !== this.epoch) return;
      this.writeRejection(active, 'frame_unavailable');
    });
  }

  private beginSafetyPrompt(active: ActiveRequest, request: ComputerSafetyRequest): void {
    if (active.request.kind !== 'safety') return;
    const nowUnixMs = (this.options.nowUnixMs ?? Date.now)();
    const nowMs = nowUnixMs - this.options.clockOriginMs;
    const remaining = request.fence.deadline_ms - nowMs;
    if (remaining <= 0 || !this.currentSafetyCanPrompt(request, nowUnixMs)) {
      this.writeSafetyReply(active, false);
      return;
    }

    const controller = new AbortController();
    active.safetyAbort = controller;
    active.timer = setTimeout(() => {
      controller.abort();
      this.writeSafetyReply(active, false);
    }, Math.max(1, Math.min(MAX_SAFETY_PROMPT_MS, remaining)));
    active.timer.unref?.();

    let confirmation: Promise<boolean>;
    try { confirmation = this.options.confirmComputerSafety(request, controller.signal); }
    catch {
      this.writeSafetyReply(active, false);
      return;
    }
    void confirmation.then((approved) => {
      if (this.closed || this.active !== active || active.epoch !== this.epoch || controller.signal.aborted) return;
      const now = (this.options.nowUnixMs ?? Date.now)();
      const current = approved === true && this.currentSafetyCanPrompt(request, now);
      this.writeSafetyReply(active, current);
    }).catch(() => {
      if (this.closed || this.active !== active || active.epoch !== this.epoch || controller.signal.aborted) return;
      this.writeSafetyReply(active, false);
    });
  }

  private writeRejection(active: ActiveRequest, reason: ComputerImageRejectionReason): void {
    if (this.active !== active || this.closed || active.request.kind !== 'frame') return;
    this.worker?.terminate().catch(() => undefined);
    this.worker = null;
    const bytes = encodeComputerImageRejection({ fence: active.request.request.fence, reason });
    this.write(active, bytes);
  }

  private writeSafetyReply(active: ActiveRequest, approved: boolean): void {
    if (this.active !== active || this.closed || active.request.kind !== 'safety') return;
    const request = active.request.request;
    const nowUnixMs = (this.options.nowUnixMs ?? Date.now)();
    const finalApproval = approved && this.currentSafetyCanPrompt(request, nowUnixMs);
    if (!finalApproval) active.safetyAbort?.abort();
    const bytes = encodeComputerSafetyReply({ fence: request.fence, approved: finalApproval });
    this.write(active, bytes);
  }

  private write(active: ActiveRequest, bytes: Uint8Array): void {
    if (this.active !== active || this.closed || active.writing) return;
    active.writing = true;
    if (active.timer) clearTimeout(active.timer);
    active.timer = setTimeout(() => this.close(), MAX_PORT_OPERATION_MS);
    active.timer.unref?.();
    try {
      this.stream.write(Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength), (error?: Error | null) => {
        if (active.timer) clearTimeout(active.timer);
        if (error) { this.close(); return; }
        if (this.active !== active || this.closed) return;
        this.active = null;
        this.worker = null;
        active.safetyAbort?.abort();
        if (active.closeAfterWrite) { this.close(); return; }
        const queued = this.queuedBusy;
        this.queuedBusy = null;
        if (queued) {
          const active: ActiveRequest = {
            request: queued.request, epoch: this.epoch, timer: null, writing: false, closeAfterWrite: true,
          };
          this.active = active;
          this.stream.pause();
          if (queued.request.kind === 'frame') {
            this.writeRejection(active, queued.fresh ? 'busy' : 'stale_fence');
          } else this.writeSafetyReply(active, false);
        } else this.stream.resume();
      });
    } catch { this.close(); }
  }

  /** Cancel work at a soft source boundary; terminal Stop/revoke closes fd5. */
  invalidate(reason: ComputerImageRejectionReason = 'stale_fence'): void {
    if (this.closed) return;
    this.epoch += 1;
    this.decoder.reset();
    if (this.partialTimer) clearTimeout(this.partialTimer);
    this.partialTimer = null;
    this.queuedBusy = null;
    const active = this.active;
    if (!active) return;
    if (active.writing) { this.close(); return; }
    if (active.timer) clearTimeout(active.timer);
    active.timer = null;
    active.safetyAbort?.abort();
    this.worker?.terminate().catch(() => undefined);
    this.worker = null;
    if (active.request.kind === 'frame') this.writeRejection(active, reason);
    else this.writeSafetyReply(active, false);
  }

  close(destroy = true): void {
    if (this.closed) return;
    this.closed = true;
    this.epoch += 1;
    this.decoder.reset();
    if (this.partialTimer) clearTimeout(this.partialTimer);
    this.partialTimer = null;
    this.queuedBusy = null;
    this.active?.safetyAbort?.abort();
    if (this.active?.timer) clearTimeout(this.active.timer);
    this.active = null;
    this.worker?.terminate().catch(() => undefined);
    this.worker = null;
    this.stream.removeListener('data', this.onData);
    this.stream.removeListener('error', this.onStreamError);
    this.stream.removeListener('close', this.onStreamClose);
    this.stream.removeListener('end', this.onStreamClose);
    if (destroy && !this.stream.destroyed) this.stream.destroy();
  }
}
