import Ajv2020 from 'ajv/dist/2020.js';
import type { ValidateFunction } from 'ajv';

import type { ComputerBatchProposal, ComputerFence, ComputerFrameRequest } from './generated/computer.js';
import { computerWireSchema } from './generated/computer-schema.js';
import type { Validation } from './validate.js';
import { frameCapture, frameGeometry, frameImageId, frameStream, frameWatchId, validSingleCapture } from './sourceProvenance.js';

const ajv = new Ajv2020({ strict: true, allErrors: true });
function formats(node: unknown): void {
  if (Array.isArray(node)) node.forEach(formats);
  else if (node && typeof node === 'object') {
    for (const [key, value] of Object.entries(node)) {
      if (key === 'format' && typeof value === 'string') ajv.addFormat(value, true);
      else formats(value);
    }
  }
}
formats(computerWireSchema);
ajv.addSchema(computerWireSchema, 'computer-v1');
const fenceSchema = ajv.getSchema('computer-v1#/$defs/ComputerFence') as ValidateFunction;
const batchSchema = ajv.getSchema('computer-v1#/$defs/ComputerBatchProposal') as ValidateFunction;
const frameSchema = ajv.getSchema('computer-v1#/$defs/ComputerFrameRequest') as ValidateFunction;

function schemaError<T>(validator: ValidateFunction): Validation<T> {
  return { ok: false, errors: (validator.errors ?? []).map((e) => `${e.instancePath || '/'} ${e.message ?? 'is invalid'}`) };
}
function invalid<T>(reason: string): Validation<T> { return { ok: false, errors: [reason] }; }
const encoder = new TextEncoder();
const byteLength = (s: string) => encoder.encode(s).byteLength;
const id = (s: string) => !!s.trim() && byteLength(s) <= 256 && !/[\u0000-\u001f\u007f-\u009f]/.test(s);
const safe = (n: number) => Number.isSafeInteger(n) && n >= 0;
const withinPixelTolerance = (value: number, pixels: number) => Number.isFinite(value) && Math.abs(value - pixels) <= 1;

/** Structural schema validation is followed by the Rust contract's byte and
 * identity rules. A successful result never grants execution authority. */
export function validateComputerFence(value: unknown): Validation<ComputerFence> {
  if (!fenceSchema(value)) return schemaError(fenceSchema);
  const fence = value as ComputerFence;
  if (![fence.request_id, fence.root_task_id, fence.snapshot_id].every(id)) return invalid('computer fence identifiers are invalid');
  if (![fence.generation, fence.root_epoch, fence.task_revision, fence.structure_sequence, fence.deadline_ms]
    .every((n) => safe(n) && n > 0)) return invalid('computer fence integers must be positive and lossless');
  return { ok: true, value: fence };
}

export function validateComputerBatchProposal(value: unknown): Validation<ComputerBatchProposal> {
  if (!batchSchema(value)) return schemaError(batchSchema);
  const batch = value as ComputerBatchProposal;
  const fence = validateComputerFence(batch.fence);
  if (!fence.ok) return fence;
  for (const action of batch.actions) {
    if ('keys' in action && action.keys != null && !action.keys.every((key) =>
      !!key.trim() && byteLength(key) <= 64 && !/[\u0000-\u001f\u007f-\u009f]/.test(key))) {
      return invalid('computer key names exceed semantic bounds');
    }
    if (action.action === 'type' && (action.text.includes('\0') || byteLength(action.text) > 64 * 1024)) {
      return invalid('computer text exceeds semantic bounds');
    }
  }
  return { ok: true, value: batch };
}

/** This validates request metadata only. The image channel separately validates
 * PNG bytes/checksum, exact correlation, freshness, cancellation and deadline. */
export function validateComputerFrameRequest(value: unknown): Validation<ComputerFrameRequest> {
  if (!frameSchema(value)) return schemaError(frameSchema);
  const request = value as ComputerFrameRequest;
  const fence = validateComputerFence(request.fence);
  if (!fence.ok) return fence;
  const frame = request.expected_frame;
  const geometry = frameGeometry(frame);
  const stream = frameStream(frame);
  const capture = frameCapture(frame);
  if (frame.status !== 'complete' || !geometry || !frameImageId(frame) || !frameWatchId(frame)
    || ![frame.frame_id, frame.surface.id, frame.clock_domain, frameWatchId(frame)!, geometry.window_id, geometry.source_clock, frameImageId(frame)!].every(id)
    || frame.session_epoch === 0 || frame.source_sequence === 0 || frame.surface.epoch === 0
    || frame.geometry_revision === 0 || geometry.pid === 0) return invalid('computer frame lacks complete native identity');
  if (![frame.session_epoch, frame.surface.epoch, frame.source_sequence, frame.geometry_revision, frame.source_ms,
    frame.byte_length, ...(stream ? [stream.callback_ms, stream.display_time_ticks, stream.display_time_ns, stream.received_uptime_ns,
      stream.received_unix_ms, stream.dropped_frames, stream.capture_interval_ms] : [])].every(safe)) {
    return invalid('computer frame identity integers must be lossless');
  }
  if (capture && (!validSingleCapture(frame) || capture.request_id !== request.fence.request_id
    || capture.deadline_ms !== request.fence.deadline_ms
    || capture.content_rect.x !== 0 || capture.content_rect.y !== 0
    || capture.window_frame.width <= 0 || capture.window_frame.height <= 0
    || capture.content_rect.width <= 0 || capture.content_rect.height <= 0
    || !Number.isFinite(frame.scale_factor) || frame.scale_factor <= 0
    || !withinPixelTolerance(capture.content_rect.width * frame.scale_factor, frame.width_px)
    || !withinPixelTolerance(capture.content_rect.height * frame.scale_factor, frame.height_px)
    || !withinPixelTolerance(capture.window_frame.width * frame.scale_factor * capture.content_scale, frame.width_px)
    || !withinPixelTolerance(capture.window_frame.height * frame.scale_factor * capture.content_scale, frame.height_px)
    || !withinPixelTolerance((capture.window_frame.width * capture.content_scale - capture.content_rect.width)
      * frame.scale_factor, 0)
    || !withinPixelTolerance((capture.window_frame.height * capture.content_scale - capture.content_rect.height)
      * frame.scale_factor, 0))) {
    return invalid('computer capture is outside its original fence or full-window transform');
  }
  return { ok: true, value: request };
}
