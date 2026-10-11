import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import {
  validateComputerBatchProposal,
  validateComputerFence,
  validateComputerFrameRequest,
  type ComputerBatchProposal,
  type ComputerFence,
  type ComputerFrameRequest,
} from '../src/index.js';

function fence(): ComputerFence {
  return { request_id: 'computer-1', generation: 1, root_task_id: 'root-1', task_revision: 1,
    root_epoch: 1, snapshot_id: 'snapshot-1', structure_sequence: 1, deadline_ms: 1000 };
}
function batch(): ComputerBatchProposal { return { fence: fence(), actions: [{ action: 'screenshot' }] }; }

test('computer batches retain action order and reject every invalid member before admission', () => {
  const value = batch();
  value.actions = [{ action: 'click', button: 'left', x: 5.25, y: 8.5 }, { action: 'type', text: '字' }];
  assert.deepEqual(validateComputerBatchProposal(value), { ok: true, value });
  for (const actions of [[], [{ action: 'screenshot' }, { action: 'unknown' }], [{ action: 'drag', path: [] }],
    [{ action: 'move', x: Infinity, y: 0 }], Array(129).fill({ action: 'screenshot' })]) {
    assert.equal(validateComputerBatchProposal({ ...value, actions }).ok, false);
  }
});

test('computer schema consumers also enforce UTF-8 byte and control-character semantics', () => {
  for (const key of [' ', 'A\0', '键'.repeat(22)]) {
    assert.equal(validateComputerBatchProposal({ ...batch(), actions: [{ action: 'keypress', keys: [key] }] }).ok, false);
  }
  for (const text of ['a\0b', '字'.repeat(21846)]) {
    assert.equal(validateComputerBatchProposal({ ...batch(), actions: [{ action: 'type', text }] }).ok, false);
  }
  for (const snapshot_id of [' ', 'A\0', '字'.repeat(86)]) {
    assert.equal(validateComputerFence({ ...fence(), snapshot_id }).ok, false);
  }
  assert.equal(validateComputerBatchProposal({ ...batch(), actions: [{ action: 'type', text: 'a'.repeat(65536) }] }).ok, true);
});

test('computer fence integers cannot round to another identity through JSON', () => {
  assert.equal(validateComputerFence({ ...fence(), structure_sequence: Number.MAX_SAFE_INTEGER }).ok, true);
  for (const structure_sequence of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, 1e30]) {
    assert.equal(validateComputerFence({ ...fence(), structure_sequence }).ok, false);
  }
  assert.equal(validateComputerFence({ ...fence(), provider_call_id: 'untrusted' }).ok, false);
});

test('computer frame identity keeps independent event and image IDs and rejects idle or lossy metadata', () => {
  const expected_frame = JSON.parse(readFileSync(new URL('../../../contracts/desktop-v1/image-channel/metadata.json', import.meta.url), 'utf8'));
  const request: ComputerFrameRequest = { fence: fence(), expected_frame };
  assert.equal(validateComputerFrameRequest(request).ok, true);
  assert.equal(validateComputerFrameRequest({ ...request, expected_frame: { ...expected_frame, status: 'idle' } }).ok, false);
  assert.equal(validateComputerFrameRequest({ ...request, expected_frame: {
    ...expected_frame, provenance: { kind: 'stream', stream: { ...expected_frame.provenance.stream, display_time_ns: Number.MAX_SAFE_INTEGER + 1 } },
  } }).ok, false);
});

test('single-capture frame accepts Retina downsampling and rejects padded or mismatched transforms', () => {
  const streamFrame = JSON.parse(readFileSync(new URL('../../../contracts/desktop-v1/image-channel/metadata.json', import.meta.url), 'utf8'));
  const captureRequest = (): ComputerFrameRequest => ({
    fence: fence(),
    expected_frame: {
      ...streamFrame,
      frame_id: 'capture-event-9',
      source_ms: 200,
      width_px: 1160,
      height_px: 768,
      scale_factor: 2,
      provenance: {
        kind: 'single_capture',
        capture: {
          watch_id: 'watch-a',
          request_id: 'computer-1',
          image_frame_id: 'png-content-9',
          pid: 42,
          window_id: 'window-2',
          window_frame: { x: 20, y: 30, width: 1160, height: 768 },
          content_rect: { x: 0, y: 0, width: 580, height: 384 },
          content_scale: 0.5,
          source_clock: 'mach_absolute',
          capture_started_uptime_ns: 1_000,
          capture_finished_uptime_ns: 1_250,
          capture_started_ms: 200,
          capture_finished_ms: 210,
          clock_uncertainty_ms: 10,
          requested_after_ms: 190,
          deadline_ms: 1000,
        },
      },
    },
  });
  const valid = captureRequest();
  assert.equal(validateComputerFrameRequest(valid).ok, true);

  const capture = (request: ComputerFrameRequest) => {
    const provenance = request.expected_frame.provenance;
    assert.equal(provenance?.kind, 'single_capture');
    if (provenance?.kind !== 'single_capture') throw new Error('expected capture provenance');
    return provenance.capture;
  };
  const rejects = (mutate: (request: ComputerFrameRequest) => void) => {
    const request = captureRequest();
    mutate(request);
    assert.equal(validateComputerFrameRequest(request).ok, false);
  };
  rejects((request) => { capture(request).content_rect.x = 1; });
  rejects((request) => { capture(request).content_rect.width -= 2; });
  rejects((request) => { capture(request).content_scale = 0.75; });
  rejects((request) => { request.expected_frame.scale_factor = 1; });
  rejects((request) => { capture(request).window_frame.width = 0; });
  rejects((request) => { capture(request).content_rect.height = 0; });
  rejects((request) => { capture(request).content_scale = Number.MAX_VALUE; });
  rejects((request) => { request.expected_frame.scale_factor = Number.MAX_VALUE; });
});
