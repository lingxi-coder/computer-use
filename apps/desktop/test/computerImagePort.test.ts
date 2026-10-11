import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { Duplex } from 'node:stream';
import { test } from 'node:test';

import type { ComputerActionProposal, ComputerFrameRequest, FrameMeta, UiProjection } from '@computer-use/desktop-api';
import {
  ComputerPortRequestDecoder,
  ComputerImagePort,
  computerSafetyRequestCanPrompt,
  computerImageRequestRejection,
  decodeComputerImageRejection,
  decodeComputerSafetyReply,
  encodeComputerFrameRequest,
  encodeComputerSafetyRequest,
  type ComputerSafetyRequest,
  type ComputerImagePortSnapshot,
} from '../src/main/computerImagePort.js';
import { decodeComputerImagePacket, encodeComputerImagePacket } from '../src/main/computerImageWire.js';
import {
  MAX_COMPUTER_IMAGE_REQUEST_BYTES,
  MAX_COMPUTER_SAFETY_REQUEST_BYTES,
  COMPUTER_IMAGE_REQUEST_MAGIC,
  COMPUTER_SAFETY_REQUEST_MAGIC,
} from '../src/main/computerImagePort.js';
import { computerImagePortRequested, hostPrivateStdioLayout, Supervisor } from '../src/main/supervisor.js';
import type { ImagePacket } from '../src/main/imageWire.js';

const metadata = JSON.parse(readFileSync(new URL('../../../contracts/desktop-v1/image-channel/metadata.json', import.meta.url), 'utf8')) as FrameMeta;
const png = readFileSync(new URL('../../../contracts/desktop-v1/image-channel/fixture.png', import.meta.url));
const clockOriginMs = 1_000_000;
const nowUnixMs = 1_000_510;
const targetBundleId = 'com.example.fixture';

function request(overrides: Partial<ComputerFrameRequest['fence']> = {}, frame = metadata): ComputerFrameRequest {
  return {
    fence: {
      request_id: 'computer-request-1', generation: 1, root_task_id: 'root-1', task_revision: 2,
      root_epoch: 3, snapshot_id: 'snapshot-4', structure_sequence: 5, deadline_ms: 2_000,
      ...overrides,
    },
    expected_frame: frame,
  };
}

function safetyRequest(overrides: Partial<ComputerFrameRequest['fence']> = {}): ComputerSafetyRequest {
  const fence = { ...request().fence, ...overrides };
  return {
    fence,
    pending_safety_checks: [{ id: 'safety-1', code: 'confirm', message: 'Review this action' }],
    proposal: { fence, actions: [{ action: 'wait' }] },
  };
}

function current(frame = metadata): ComputerImagePortSnapshot {
  const projection = {
    revision: 10,
    mode: 'execute',
    cleanup: 'not_needed',
    stop_status: 'not_stopped',
    timeline: [],
    root: { root_task_id: 'root-1', task_revision: 2, root_epoch: 3, state: 'running' },
    capabilities: { identity: { session_epoch: 7, surface: frame.surface, clock_domain: frame.clock_domain } },
    observation: {
      observed_target: {
        snapshot_id: 'snapshot-4', structure_sequence: 5, session_epoch: 7, app: targetBundleId,
        pid: 42, window_id: 'window-1', observed_ms: 0, valid_until_ms: 3_000,
      },
      frame: {
        healthy: true, status: 'complete', stream_id: 'stream-a', frame_id: 'image-a-1', geometry_revision: 1,
        width_px: 320, height_px: 200, pixel_age_ms: 490, confirmation_age_ms: 480, dropped_frames: 0,
      },
    },
  } as unknown as UiProjection;
  const image: ImagePacket = { meta: frame, bytes: png };
  return { projection, projectionAt: nowUnixMs - 30, imageControl: frame, imagePacket: image };
}

class TestDuplex extends Duplex {
  readonly written: Buffer[] = [];
  _read(): void {}
  _write(chunk: Buffer, _encoding: BufferEncoding, callback: (error?: Error | null) => void): void {
    this.written.push(Buffer.from(chunk));
    this.emit('written');
    callback();
  }
  send(bytes: Uint8Array): void { this.push(Buffer.from(bytes)); }
}

async function until(predicate: () => boolean): Promise<void> {
  for (let i = 0; i < 100 && !predicate(); i += 1) await new Promise((resolve) => setTimeout(resolve, 2));
  assert.equal(predicate(), true, 'port response arrives');
}

test('fd5 request decoder accepts fragmented bounded requests and rejects oversized framing', () => {
  const expected = request();
  const encoded = Buffer.from(encodeComputerFrameRequest(expected));
  const decoder = new ComputerPortRequestDecoder();
  const delivered: unknown[] = [];
  decoder.push(encoded.subarray(0, 7), (value) => delivered.push(value));
  assert.equal(decoder.buffered, 7);
  decoder.push(encoded.subarray(7), (value) => delivered.push(value));
  assert.deepEqual(delivered, [{ kind: 'frame', request: expected }]);
  assert.equal(decoder.buffered, 0);

  const oversized = Buffer.alloc(12);
  oversized.write(COMPUTER_IMAGE_REQUEST_MAGIC, 0, 'ascii');
  oversized.writeUInt32BE(MAX_COMPUTER_IMAGE_REQUEST_BYTES + 1, 8);
  assert.throws(() => new ComputerPortRequestDecoder().push(oversized, () => undefined), /length exceeds bounds/);
  const truncated = new ComputerPortRequestDecoder();
  truncated.push(encoded.subarray(0, -1), () => undefined);
  assert.equal(truncated.buffered, encoded.length - 1);

  const safety = Buffer.from(encodeComputerSafetyRequest(safetyRequest()));
  const safetyDecoder = new ComputerPortRequestDecoder();
  const safetyDelivered: unknown[] = [];
  safetyDecoder.push(safety.subarray(0, 11), (value) => safetyDelivered.push(value));
  safetyDecoder.push(safety.subarray(11), (value) => safetyDelivered.push(value));
  assert.deepEqual(safetyDelivered, [{ kind: 'safety', request: safetyRequest() }]);

  const oversizedSafety = Buffer.alloc(12);
  oversizedSafety.write(COMPUTER_SAFETY_REQUEST_MAGIC, 0, 'ascii');
  oversizedSafety.writeUInt32BE(MAX_COMPUTER_SAFETY_REQUEST_BYTES + 1, 8);
  assert.throws(() => new ComputerPortRequestDecoder().push(oversizedSafety, () => undefined), /length exceeds bounds/);
});

test('the screenshot gate requires the current authorized root, AX snapshot sequence, controller frame and fresh pixels', () => {
  const expected = request();
  assert.equal(computerImageRequestRejection(expected, current(), clockOriginMs, targetBundleId, nowUnixMs), null);
  assert.equal(computerImageRequestRejection(expected, current(), clockOriginMs, targetBundleId, 1_003_000), 'deadline_expired');

  const staleSnapshot = current();
  const target = staleSnapshot.projection!.observation!.observed_target as unknown as { structure_sequence: number };
  target.structure_sequence += 1;
  assert.equal(computerImageRequestRejection(expected, staleSnapshot, clockOriginMs, targetBundleId, nowUnixMs), 'stale_fence');

  const changedImage = current({ ...metadata, source_sequence: metadata.source_sequence + 1 });
  assert.equal(computerImageRequestRejection(expected, changedImage, clockOriginMs, targetBundleId, nowUnixMs), 'frame_mismatch');

  const aged = nowUnixMs + 2_000;
  const longDeadline = request({ deadline_ms: 4_000 });
  assert.equal(computerImageRequestRejection(longDeadline, current(), clockOriginMs, targetBundleId, aged), 'frame_unavailable');
});

test('safety dialogs require the exact live Root, structure scope, approved window and model scope', () => {
  const expected = safetyRequest();
  assert.equal(computerSafetyRequestCanPrompt(expected, current(), clockOriginMs, targetBundleId, 42,
    'window-1', () => true, nowUnixMs), true);
  assert.equal(computerSafetyRequestCanPrompt(expected, current(), clockOriginMs, targetBundleId, 42,
    'window-1', () => true, 1_003_000), false);
  assert.equal(computerSafetyRequestCanPrompt(expected, current(), clockOriginMs, targetBundleId, 42,
    'window-2', () => true, nowUnixMs), false);
  assert.equal(computerSafetyRequestCanPrompt(expected, current(), clockOriginMs, targetBundleId, 42,
    'window-1', () => false, nowUnixMs), false);

  const advanced = current();
  const advancedTarget = advanced.projection!.observation!.observed_target as unknown as {
    snapshot_id: string; structure_sequence: number;
  };
  advancedTarget.snapshot_id = 'snapshot-6';
  advancedTarget.structure_sequence = 6;
  assert.equal(computerSafetyRequestCanPrompt(expected, advanced, clockOriginMs, targetBundleId, 42,
    'window-1', () => true, nowUnixMs), true);

  const rolledBack = current();
  (rolledBack.projection!.observation!.observed_target as unknown as { structure_sequence: number }).structure_sequence = 4;
  assert.equal(computerSafetyRequestCanPrompt(expected, rolledBack, clockOriginMs, targetBundleId, 42,
    'window-1', () => true, nowUnixMs), false);

  const changed = current();
  (changed.projection!.observation!.observed_target as unknown as { snapshot_id: string }).snapshot_id = 'snapshot-other';
  assert.equal(computerSafetyRequestCanPrompt(expected, changed, clockOriginMs, targetBundleId, 42,
    'window-1', () => true, nowUnixMs), false);

  const otherRoot = current();
  (otherRoot.projection!.root as unknown as { root_epoch: number }).root_epoch += 1;
  assert.equal(computerSafetyRequestCanPrompt(expected, otherRoot, clockOriginMs, targetBundleId, 42,
    'window-1', () => true, nowUnixMs), false);

  const staleProjection = current();
  staleProjection.projectionAt = nowUnixMs - 2_000;
  assert.equal(computerSafetyRequestCanPrompt(expected, staleProjection, clockOriginMs, targetBundleId, 42,
    'window-1', () => true, nowUnixMs), false);
});

test('safety request bounds reject oversized typed review content without truncation', () => {
  const oversized = safetyRequest();
  const longTypeAction: ComputerActionProposal = { action: 'type', text: 'x'.repeat(65_536) };
  oversized.proposal!.actions[0] = longTypeAction;
  for (let index = 1; index < 17; index += 1) oversized.proposal!.actions.push(longTypeAction);
  assert.throws(() => encodeComputerSafetyRequest(oversized), /exceeds bounds/);

  const duplicate = safetyRequest();
  duplicate.pending_safety_checks.push({ ...duplicate.pending_safety_checks[0]! });
  assert.throws(() => encodeComputerSafetyRequest(duplicate), /invalid/);

  const nullField = safetyRequest();
  (nullField.pending_safety_checks[0] as unknown as { code: unknown }).code = null;
  assert.throws(() => encodeComputerSafetyRequest(nullField), /invalid/);
});

test('fd5 returns a worker-encoded exact current image; it does not send image bytes on control APIs', async () => {
  const expected = request();
  const stream = new TestDuplex();
  const port = new ComputerImagePort(stream, {
    clockOriginMs, targetBundleId, targetPid: 42, targetWindowId: 'window-1',
    isComputerAuthorizationCurrent: () => true, confirmComputerSafety: async () => false,
    nowUnixMs: () => nowUnixMs, current: () => current(),
    encoder: async (ask, image, time) => encodeComputerImagePacket(ask, image, time),
  });
  stream.send(encodeComputerFrameRequest(expected));
  await until(() => stream.written.length === 1);
  const response = decodeComputerImagePacket(stream.written[0]!, expected, nowUnixMs - clockOriginMs + 1);
  assert.deepEqual(response.fence, expected.fence);
  assert.deepEqual(response.image.meta, expected.expected_frame);
  assert.deepEqual(Buffer.from(response.image.bytes), png);
  port.close();
  assert.equal(stream.destroyed, true);
});

test('safety confirmation may reuse the preceding frame fence generation but only once', async () => {
  const expectedFrame = request({ generation: 2, request_id: 'same-scope' });
  const expectedSafety = safetyRequest({ generation: 2, request_id: 'same-scope' });
  const stream = new TestDuplex();
  let prompts = 0;
  const port = new ComputerImagePort(stream, {
    clockOriginMs, targetBundleId, targetPid: 42, targetWindowId: 'window-1',
    isComputerAuthorizationCurrent: () => true,
    confirmComputerSafety: async () => { prompts += 1; return true; },
    nowUnixMs: () => nowUnixMs, current: () => current(),
    encoder: async (ask, image, time) => encodeComputerImagePacket(ask, image, time),
  });

  stream.send(encodeComputerFrameRequest(expectedFrame));
  await until(() => stream.written.length === 1);
  assert.equal(decodeComputerImagePacket(stream.written[0]!, expectedFrame, nowUnixMs - clockOriginMs + 1).fence.generation, 2);

  stream.send(encodeComputerSafetyRequest(expectedSafety));
  await until(() => stream.written.length === 2);
  assert.equal(decodeComputerSafetyReply(stream.written[1]!, expectedSafety.fence).approved, true);
  assert.equal(prompts, 1);

  stream.send(encodeComputerSafetyRequest(expectedSafety));
  await until(() => stream.written.length === 3);
  assert.equal(decodeComputerSafetyReply(stream.written[2]!, expectedSafety.fence).approved, false);
  assert.equal(prompts, 1);
  port.close();
});

test('single outstanding work returns one bounded busy rejection and consumes both generations', async () => {
  const first = request();
  const second = request({ request_id: 'computer-request-2', generation: 2 });
  const stream = new TestDuplex();
  let release!: (encoded: Uint8Array) => void;
  const port = new ComputerImagePort(stream, {
    clockOriginMs, targetBundleId, targetPid: 42, targetWindowId: 'window-1',
    isComputerAuthorizationCurrent: () => true, confirmComputerSafety: async () => false,
    nowUnixMs: () => nowUnixMs, current: () => current(),
    encoder: () => new Promise((resolve) => { release = resolve; }),
  });
  stream.send(Buffer.concat([Buffer.from(encodeComputerFrameRequest(first)), Buffer.from(encodeComputerFrameRequest(second))]));
  await until(() => typeof release === 'function');
  release(encodeComputerImagePacket(first, current().imagePacket!, nowUnixMs - clockOriginMs));
  await until(() => stream.written.length === 2);
  await until(() => stream.destroyed);
  assert.equal(decodeComputerImagePacket(stream.written[0]!, first, nowUnixMs - clockOriginMs + 1).fence.generation, 1);
  const rejected = decodeComputerImageRejection(stream.written[1]!, second.fence);
  assert.equal(rejected.reason, 'busy');
  assert.equal(stream.destroyed, true);
  port.close(false);
});

test('safety request displays the exact bounded checks and only returns correlated approval', async () => {
  const expected = safetyRequest();
  const stream = new TestDuplex();
  let shown: ComputerSafetyRequest | null = null;
  const prompt = { signal: undefined as AbortSignal | undefined };
  const port = new ComputerImagePort(stream, {
    clockOriginMs, targetBundleId, targetPid: 42, targetWindowId: 'window-1',
    isComputerAuthorizationCurrent: () => true,
    confirmComputerSafety: async (value, signal) => {
      shown = value;
      prompt.signal = signal;
      return true;
    },
    nowUnixMs: () => nowUnixMs, current: () => current(),
  });
  stream.send(encodeComputerSafetyRequest(expected));
  await until(() => stream.written.length === 1);
  const reply = decodeComputerSafetyReply(stream.written[0]!, expected.fence);
  assert.deepEqual(shown, expected);
  assert.equal(prompt.signal?.aborted, true); // resolved callbacks are retired after the reply is written
  assert.equal(reply.approved, true);
  port.close();
});

test('stop invalidation aborts pending safety UI and a late approval cannot ACK', async () => {
  const expected = safetyRequest();
  const stream = new TestDuplex();
  let release!: (value: boolean) => void;
  const prompt = { signal: undefined as AbortSignal | undefined };
  const port = new ComputerImagePort(stream, {
    clockOriginMs, targetBundleId, targetPid: 42, targetWindowId: 'window-1',
    isComputerAuthorizationCurrent: () => true,
    confirmComputerSafety: (_value, signal) => {
      prompt.signal = signal;
      return new Promise((resolve) => { release = resolve; });
    },
    nowUnixMs: () => nowUnixMs, current: () => current(),
  });
  stream.send(encodeComputerSafetyRequest(expected));
  await until(() => typeof release === 'function');
  port.invalidate('stale_fence');
  await until(() => stream.written.length === 1);
  assert.equal(prompt.signal?.aborted, true);
  assert.equal(decodeComputerSafetyReply(stream.written[0]!, expected.fence).approved, false);
  release(true);
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(stream.written.length, 1);
  port.close();
});

test('a dialog result arriving at the fence deadline is returned as denial', async () => {
  const expected = safetyRequest();
  const stream = new TestDuplex();
  let release!: (value: boolean) => void;
  let now = nowUnixMs;
  const port = new ComputerImagePort(stream, {
    clockOriginMs, targetBundleId, targetPid: 42, targetWindowId: 'window-1',
    isComputerAuthorizationCurrent: () => true,
    confirmComputerSafety: () => new Promise((resolve) => { release = resolve; }),
    nowUnixMs: () => now, current: () => current(),
  });
  stream.send(encodeComputerSafetyRequest(expected));
  await until(() => typeof release === 'function');
  now = clockOriginMs + expected.fence.deadline_ms;
  release(true);
  await until(() => stream.written.length === 1);
  assert.equal(decodeComputerSafetyReply(stream.written[0]!, expected.fence).approved, false);
  port.close();
});

test('closing fd5 aborts pending safety UI and ignores its late result', async () => {
  const expected = safetyRequest();
  const stream = new TestDuplex();
  let release!: (value: boolean) => void;
  const prompt = { signal: undefined as AbortSignal | undefined };
  const port = new ComputerImagePort(stream, {
    clockOriginMs, targetBundleId, targetPid: 42, targetWindowId: 'window-1',
    isComputerAuthorizationCurrent: () => true,
    confirmComputerSafety: (_value, signal) => {
      prompt.signal = signal;
      return new Promise((resolve) => { release = resolve; });
    },
    nowUnixMs: () => nowUnixMs, current: () => current(),
  });
  stream.send(encodeComputerSafetyRequest(expected));
  await until(() => typeof release === 'function');
  port.close();
  assert.equal(prompt.signal?.aborted, true);
  release(true);
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(stream.written.length, 0);
});

test('fd indexes keep owned control and teacher observation stable while adding computer image at fd5', () => {
  assert.deepEqual(hostPrivateStdioLayout(false, false, false), ['pipe', 'pipe', 'pipe']);
  assert.deepEqual(hostPrivateStdioLayout(true, false, false), ['pipe', 'pipe', 'pipe', 'pipe']);
  assert.deepEqual(hostPrivateStdioLayout(false, true, false), ['pipe', 'pipe', 'pipe', 'ignore', 'pipe']);
  assert.deepEqual(hostPrivateStdioLayout(false, false, true), ['pipe', 'pipe', 'pipe', 'ignore', 'ignore', 'pipe']);
  assert.deepEqual(hostPrivateStdioLayout(true, true, true), ['pipe', 'pipe', 'pipe', 'pipe', 'pipe', 'pipe']);
});

test('computer proposals require a native window grant before Supervisor starts', async () => {
  const modelAuthorization = {
    route: { provider_id: 'open_ai' as const, protocol: 'open_ai_responses' as const,
      base_url: 'https://api.openai.com/v1', model: 'fixture-model', max_output_tokens: 1024 },
    credential: { kind: 'api_key' as const, api_key: 'computer-purpose-test-key' }, purposes: ['computer_proposals' as const], account_scope: 'scope-1',
  };
  assert.equal(computerImagePortRequested({ modelAuthorization, nativeSource: undefined }), false);
  assert.equal(computerImagePortRequested({ modelAuthorization, nativeSource: {
    helperPath: '/fixture/helper', bundleId: targetBundleId, pid: 42, authorizedWindowId: 'window-1',
  } }), true);
  assert.equal(computerImagePortRequested({ modelAuthorization, nativeSource: {
    helperPath: '/fixture/helper', bundleId: targetBundleId, pid: 42,
    backgroundWindowId: 'window-1', authorizedBackgroundWindowId: 'window-1',
    authorizedProcessIdentity: { pid: 42, start_seconds: 1_790_000_000, start_micros: 123_456 },
  } }), false, 'a BackgroundAx window grant cannot enable the foreground computer-proposal model purpose');
  await assert.rejects(Supervisor.start({
    binaries: { host: '/not-used/cu-host', device: '/not-used/cu-device', source: '/not-used/cu-source' },
    modelAuthorization,
  }), /selecting a native window and authorizing it/);
});
