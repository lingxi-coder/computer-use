import type {
  DecisionCancel,
  DecisionProposal,
  DecisionRequest,
  Envelope,
  Message,
} from '@computer-use/desktop-api';
import { basename, isAbsolute, resolve } from 'node:path';
import { PROTOCOL_VERSION, validateEnvelope } from '@computer-use/desktop-api';

export const UTILITY_PROTOCOL_VERSION = 1 as const;
export const MAX_UTILITY_FRAME_BYTES = 128 * 1024;
const LENGTH_PREFIX_BYTES = 4;

export type UtilityFailureCode =
  | 'backend_unavailable'
  | 'invalid_config'
  | 'invalid_message'
  | 'invalid_request'
  | 'invalid_proposal'
  | 'worker_busy'
  | 'worker_timeout'
  | 'worker_exit'
  | 'worker_spawn_failed'
  | 'closed'
  | 'queue_full';

export type UtilityMainMessage =
  | {
      version: typeof UTILITY_PROTOCOL_VERSION;
      type: 'init';
      bundleManifestPath: string;
      numThreads: number;
      minimumConfidenceMilli: number;
    }
  | {
      version: typeof UTILITY_PROTOCOL_VERSION;
      type: 'request';
      request_id: number;
      epoch: number;
      request: DecisionRequest;
    }
  | {
      version: typeof UTILITY_PROTOCOL_VERSION;
      type: 'cancel';
      request_id: number;
      decision_id: string;
      request_generation: number;
    }
  | {
      version: typeof UTILITY_PROTOCOL_VERSION;
      type: 'invalidate';
      epoch: number;
    }
  | { version: typeof UTILITY_PROTOCOL_VERSION; type: 'close' };

export type UtilityWorkerMessage =
  | {
      version: typeof UTILITY_PROTOCOL_VERSION;
      type: 'ready';
      model_revision: string;
    }
  | {
      version: typeof UTILITY_PROTOCOL_VERSION;
      type: 'proposal';
      request_id: number;
      epoch: number;
      proposal: DecisionProposal;
    }
  | {
      version: typeof UTILITY_PROTOCOL_VERSION;
      type: 'unavailable';
      code: UtilityFailureCode;
    }
  | {
      version: typeof UTILITY_PROTOCOL_VERSION;
      type: 'unavailable';
      request_id: number;
      epoch: number;
      code: UtilityFailureCode;
      proposal: DecisionProposal;
    };

export type UtilityProtocolMessage = UtilityMainMessage | UtilityWorkerMessage;

export class UtilityProtocolError extends Error {
  constructor(readonly code: 'invalid_frame' | 'frame_too_large' | 'invalid_json' | UtilityFailureCode) {
    super(code);
    this.name = 'UtilityProtocolError';
  }
}

function fail(code: UtilityProtocolError['code']): never {
  throw new UtilityProtocolError(code);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}

function safeCounter(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function validateDesktopPayload<T extends Message['payload']>(type: Message['type'], payload: unknown): T {
  const envelope: Envelope = {
    protocol_version: PROTOCOL_VERSION,
    message_id: 'utility-protocol-validation',
    in_reply_to: null,
    producer: 'decision-utility-protocol',
    producer_sequence: 1,
    emitted_ms: 0,
    trace_id: 'decision-utility-protocol',
    message: { type, payload } as Message,
  };
  const validation = validateEnvelope(envelope);
  if (!validation.ok) {
    fail(type === 'decision_request' ? 'invalid_request' : 'invalid_proposal');
  }
  const message = validation.value.message;
  if (message.type !== type) fail(type === 'decision_request' ? 'invalid_request' : 'invalid_proposal');
  return message.payload as T;
}

function validateRequest(value: unknown): DecisionRequest {
  const request = validateDesktopPayload<DecisionRequest>('decision_request', value);
  if (
    !safeCounter(request.request_generation)
    || !safeCounter(request.root_epoch)
    || !safeCounter(request.task_revision)
    || !safeCounter(request.created_ms)
    || !safeCounter(request.result_deadline_ms)
    || request.result_deadline_ms <= request.created_ms
    || request.state.root_task_id !== request.root_task_id
    || request.state.task_revision !== request.task_revision
  ) {
    fail('invalid_request');
  }
  return request;
}

function validateCancel(value: unknown): DecisionCancel {
  const cancel = validateDesktopPayload<DecisionCancel>('decision_cancel', value);
  if (!safeCounter(cancel.request_generation)) fail('invalid_message');
  return cancel;
}

function validateProposal(value: unknown): DecisionProposal {
  const proposal = validateDesktopPayload<DecisionProposal>('decision_proposal', value);
  if (
    !safeCounter(proposal.request_generation)
    || !safeCounter(proposal.queue_ms)
    || !safeCounter(proposal.inference_ms)
  ) {
    fail('invalid_proposal');
  }
  return proposal;
}

function validCommon(record: Record<string, unknown>): boolean {
  return record.version === UTILITY_PROTOCOL_VERSION;
}

function validFailureCode(value: unknown): value is UtilityFailureCode {
  return typeof value === 'string' && [
    'backend_unavailable', 'invalid_config', 'invalid_message', 'invalid_request', 'invalid_proposal',
    'worker_busy', 'worker_timeout', 'worker_exit', 'worker_spawn_failed', 'closed', 'queue_full',
  ].includes(value);
}

export function decodeUtilityMessage(value: unknown): UtilityProtocolMessage {
  if (!isRecord(value) || !validCommon(value) || typeof value.type !== 'string') fail('invalid_message');
  switch (value.type) {
    case 'init': {
      if (!hasExactKeys(value, ['version', 'type', 'bundleManifestPath', 'numThreads', 'minimumConfidenceMilli'])) {
        fail('invalid_message');
      }
      if (
        typeof value.bundleManifestPath !== 'string'
        || value.bundleManifestPath.length === 0
        || value.bundleManifestPath.length > 4096
        || !isAbsolute(value.bundleManifestPath)
        || basename(value.bundleManifestPath) !== 'package-manifest.json'
        || resolve(value.bundleManifestPath) !== value.bundleManifestPath
        || !safeCounter(value.numThreads)
        || value.numThreads < 1
        || value.numThreads > 4
        || !safeCounter(value.minimumConfidenceMilli)
        || value.minimumConfidenceMilli < 800
        || value.minimumConfidenceMilli > 1000
      ) fail('invalid_config');
      return value as unknown as UtilityMainMessage;
    }
    case 'request': {
      if (!hasExactKeys(value, ['version', 'type', 'request_id', 'epoch', 'request'])) fail('invalid_message');
      if (!safeCounter(value.request_id) || value.request_id === 0 || !safeCounter(value.epoch) || value.epoch === 0) {
        fail('invalid_message');
      }
      return {
        version: UTILITY_PROTOCOL_VERSION,
        type: 'request',
        request_id: value.request_id,
        epoch: value.epoch,
        request: validateRequest(value.request),
      };
    }
    case 'cancel': {
      if (!hasExactKeys(value, ['version', 'type', 'request_id', 'decision_id', 'request_generation'])) {
        fail('invalid_message');
      }
      if (!safeCounter(value.request_id) || value.request_id === 0) fail('invalid_message');
      const cancel = validateCancel({ decision_id: value.decision_id, request_generation: value.request_generation });
      return {
        version: UTILITY_PROTOCOL_VERSION,
        type: 'cancel',
        request_id: value.request_id,
        decision_id: cancel.decision_id,
        request_generation: cancel.request_generation,
      };
    }
    case 'invalidate': {
      if (!hasExactKeys(value, ['version', 'type', 'epoch']) || !safeCounter(value.epoch) || value.epoch === 0) {
        fail('invalid_message');
      }
      return value as unknown as UtilityMainMessage;
    }
    case 'close':
      if (!hasExactKeys(value, ['version', 'type'])) fail('invalid_message');
      return value as unknown as UtilityMainMessage;
    case 'ready': {
      if (!hasExactKeys(value, ['version', 'type', 'model_revision'])) fail('invalid_message');
      if (typeof value.model_revision !== 'string' || value.model_revision.length === 0 || value.model_revision.length > 256) {
        fail('invalid_message');
      }
      return value as unknown as UtilityWorkerMessage;
    }
    case 'proposal': {
      if (!hasExactKeys(value, ['version', 'type', 'request_id', 'epoch', 'proposal'])) fail('invalid_message');
      if (!safeCounter(value.request_id) || value.request_id === 0 || !safeCounter(value.epoch) || value.epoch === 0) {
        fail('invalid_message');
      }
      return {
        version: UTILITY_PROTOCOL_VERSION,
        type: 'proposal',
        request_id: value.request_id,
        epoch: value.epoch,
        proposal: validateProposal(value.proposal),
      };
    }
    case 'unavailable': {
      if (!validFailureCode(value.code)) fail('invalid_message');
      if (hasExactKeys(value, ['version', 'type', 'code'])) {
        return value as unknown as UtilityWorkerMessage;
      }
      if (!hasExactKeys(value, ['version', 'type', 'request_id', 'epoch', 'code', 'proposal'])) {
        fail('invalid_message');
      }
      if (!safeCounter(value.request_id) || value.request_id === 0 || !safeCounter(value.epoch) || value.epoch === 0) {
        fail('invalid_message');
      }
      return {
        version: UTILITY_PROTOCOL_VERSION,
        type: 'unavailable',
        request_id: value.request_id,
        epoch: value.epoch,
        code: value.code,
        proposal: validateProposal(value.proposal),
      };
    }
    default:
      return fail('invalid_message');
  }
}

export function encodeUtilityFrame(message: UtilityProtocolMessage): Buffer {
  const validated = decodeUtilityMessage(message);
  const body = Buffer.from(JSON.stringify(validated), 'utf8');
  const totalBytes = LENGTH_PREFIX_BYTES + body.byteLength;
  if (totalBytes > MAX_UTILITY_FRAME_BYTES) fail('frame_too_large');
  const frame = Buffer.allocUnsafe(totalBytes);
  frame.writeUInt32BE(body.byteLength, 0);
  body.copy(frame, LENGTH_PREFIX_BYTES);
  return frame;
}

export function decodeUtilityFrame(frame: unknown): UtilityProtocolMessage {
  const byteView = frame instanceof Uint8Array
    ? frame
    : frame instanceof ArrayBuffer
      ? new Uint8Array(frame)
      : null;
  if (!byteView) fail('invalid_frame');
  if (byteView.byteLength > MAX_UTILITY_FRAME_BYTES) fail('frame_too_large');
  if (byteView.byteLength <= LENGTH_PREFIX_BYTES) fail('invalid_frame');
  const bytes = Buffer.from(byteView.buffer, byteView.byteOffset, byteView.byteLength);
  const bodyBytes = bytes.readUInt32BE(0);
  if (bodyBytes === 0 || bodyBytes + LENGTH_PREFIX_BYTES !== bytes.byteLength) fail('invalid_frame');
  let parsed: unknown;
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(LENGTH_PREFIX_BYTES));
    parsed = JSON.parse(text) as unknown;
  } catch {
    fail('invalid_json');
  }
  return decodeUtilityMessage(parsed);
}

export function toUtilityTransferable(frame: Buffer): ArrayBuffer {
  return frame.buffer.slice(frame.byteOffset, frame.byteOffset + frame.byteLength) as ArrayBuffer;
}

export function assertUtilityConfig(config: {
  workerModulePath: string;
  bundleManifestPath: string;
  numThreads: number;
  minimumConfidenceMilli: number;
}): void {
  if (
    typeof config.workerModulePath !== 'string'
    || config.workerModulePath.length > 4096
    || !isAbsolute(config.workerModulePath)
    || basename(config.workerModulePath) !== 'localDecisionWorker.js'
    || resolve(config.workerModulePath) !== config.workerModulePath
    || typeof config.bundleManifestPath !== 'string'
    || config.bundleManifestPath.length === 0
    || config.bundleManifestPath.length > 4096
    || !isAbsolute(config.bundleManifestPath)
    || basename(config.bundleManifestPath) !== 'package-manifest.json'
    || resolve(config.bundleManifestPath) !== config.bundleManifestPath
    || !Number.isInteger(config.numThreads)
    || config.numThreads < 1
    || config.numThreads > 4
    || !Number.isInteger(config.minimumConfidenceMilli)
    || config.minimumConfidenceMilli < 800
    || config.minimumConfidenceMilli > 1000
  ) {
    fail('invalid_config');
  }
}
