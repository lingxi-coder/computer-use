import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import test, { afterEach } from 'node:test';

import type { DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';
import { LocalDecisionProcess, type LocalDecisionProcessOptions } from '../src/main/localDecisionProcess.js';
import {
  assertUtilityConfig,
  decodeUtilityFrame,
  encodeUtilityFrame,
  MAX_UTILITY_FRAME_BYTES,
  toUtilityTransferable,
  UTILITY_PROTOCOL_VERSION,
  UtilityProtocolError,
  type UtilityMainMessage,
  type UtilityWorkerMessage,
} from '../../../packages/decision-worker/src/utilityProtocol.js';

const runners = new Set<LocalDecisionProcess>();

afterEach(async () => {
  await Promise.all([...runners].map((runner) => runner.close()));
  runners.clear();
});

function request(overrides: Partial<DecisionRequest> = {}): DecisionRequest {
  const state = {
    root_task_id: 'root-1',
    task_revision: 1,
    goal_summary: 'choose one safe next step',
    subgoal: 'observe',
    snapshot_revision: 1,
    objects: [],
    recent_results: [],
    missing_fields: [],
    omitted_items: [],
    candidate_set_digest: 'cs1:current',
    candidates: [{
      candidate_id: 'candidate-1',
      skill: 'observe_more',
      semantic_target: null,
      binding: null,
      parameters: {},
      effect_class: 'read_only' as const,
      precondition: { state: 'executable' as const },
      description: 'take another observation',
    }],
  };
  return {
    decision_id: 'decision-1',
    lane_id: 'lane-1',
    request_generation: 1,
    root_task_id: 'root-1',
    task_revision: 1,
    plan_revision: 1,
    root_epoch: 1,
    decision_program: 'ui.step@1',
    calibration_revision: 'none',
    created_ms: 0,
    result_deadline_ms: 10_000,
    state,
    questions: [{ id: 'q1', kind: 'choice', prompt: 'Which step?', options: [] }],
    ...overrides,
  };
}

function proposal(input: DecisionRequest, overrides: Partial<DecisionProposal> = {}): DecisionProposal {
  return {
    decision_id: input.decision_id,
    request_generation: input.request_generation,
    candidate_set_digest: input.state.candidate_set_digest,
    backend: 'laya-s1',
    source: 's1',
    model_revision: 'fixture-model-revision',
    answers: [],
    choice: { action: 'start_skill', candidate: 'candidate-1' },
    failure: null,
    truncated: false,
    queue_ms: 0,
    inference_ms: 2,
    ...overrides,
  };
}

function expectMainMessage(value: ArrayBuffer): UtilityMainMessage {
  const message = decodeUtilityFrame(value);
  if (message.type === 'ready' || message.type === 'proposal' || message.type === 'unavailable') {
    throw new Error('wrong direction');
  }
  return message;
}

class FakeUtilityProcess extends EventEmitter {
  readonly received: UtilityMainMessage[] = [];
  exited = false;

  postMessage(value: ArrayBuffer): void {
    const message = expectMainMessage(value);
    this.received.push(message);
    if (message.type === 'init') {
      queueMicrotask(() => this.send({
        version: UTILITY_PROTOCOL_VERSION,
        type: 'ready',
        model_revision: 'fixture-model-revision',
      }));
    } else if (message.type === 'close') {
      this.kill();
    }
  }

  send(message: UtilityWorkerMessage): void {
    this.emit('message', toUtilityTransferable(encodeUtilityFrame(message)));
  }

  kill(): boolean {
    this.exit(0);
    return true;
  }

  exit(code: number): void {
    if (!this.exited) {
      this.exited = true;
      queueMicrotask(() => this.emit('exit', code));
    }
  }

  requestMessages(): Extract<UtilityMainMessage, { type: 'request' }>[] {
    return this.received.filter((message): message is Extract<UtilityMainMessage, { type: 'request' }> => message.type === 'request');
  }
}

function options(
  child: FakeUtilityProcess,
  extras: Partial<LocalDecisionProcessOptions> = {},
): LocalDecisionProcessOptions {
  return {
    workerModulePath: '/Application/Computer Use.app/Contents/Resources/app/out/main/localDecisionWorker.js',
    bundleManifestPath: '/Application/Computer Use.app/Contents/Resources/laya/package-manifest.json',
    nowMs: () => 0,
    spawnUtilityProcess: (_path, _args, _options) => {
      setImmediate(() => child.emit('spawn'));
      return child;
    },
    readyTimeoutMs: 2_000,
    ...extras,
  };
}

async function start(child = new FakeUtilityProcess(), extras: Partial<LocalDecisionProcessOptions> = {}) {
  const runner = await LocalDecisionProcess.start(options(child, extras));
  runners.add(runner);
  return { child, runner };
}

test('utility transport is one bounded, versioned, length-prefixed JSON message', () => {
  const input = request();
  const frame = encodeUtilityFrame({
    version: UTILITY_PROTOCOL_VERSION,
    type: 'request',
    request_id: 1,
    epoch: 1,
    request: input,
  });
  const decoded = decodeUtilityFrame(toUtilityTransferable(frame));
  assert.equal(decoded.type, 'request');
  if (decoded.type === 'request') assert.deepEqual(decoded.request, input);

  const short = Buffer.from([0, 0, 0, 1, 0x7b]);
  assert.throws(() => decodeUtilityFrame(short), (error: unknown) =>
    error instanceof UtilityProtocolError && error.code === 'invalid_json');
  const trailing = Buffer.concat([frame, Buffer.from([0])]);
  assert.throws(() => decodeUtilityFrame(trailing), (error: unknown) =>
    error instanceof UtilityProtocolError && error.code === 'invalid_frame');
  assert.throws(() => decodeUtilityFrame(new Uint8Array(MAX_UTILITY_FRAME_BYTES + 1)), (error: unknown) =>
    error instanceof UtilityProtocolError && error.code === 'frame_too_large');
});

test('utility messages reject wrong versions, extra keys, and schema-invalid requests/proposals', () => {
  const valid = {
    version: UTILITY_PROTOCOL_VERSION,
    type: 'request',
    request_id: 1,
    epoch: 1,
    request: request(),
  } as const;
  assert.throws(() => encodeUtilityFrame({ ...valid, version: 2 } as never), UtilityProtocolError);
  assert.throws(() => encodeUtilityFrame({ ...valid, debug: true } as never), UtilityProtocolError);
  assert.throws(() => encodeUtilityFrame({
    ...valid,
    request: { ...valid.request, private_note: 'not in schema' },
  } as never), (error: unknown) => error instanceof UtilityProtocolError && error.code === 'invalid_request');
  assert.throws(() => encodeUtilityFrame({
    version: UTILITY_PROTOCOL_VERSION,
    type: 'proposal',
    request_id: 1,
    epoch: 1,
    proposal: { decision_id: 'missing required fields' },
  } as never), (error: unknown) => error instanceof UtilityProtocolError && error.code === 'invalid_proposal');
});

test('trusted worker and model paths are absolute fixed build artifacts with bounded CPU config', () => {
  assert.doesNotThrow(() => assertUtilityConfig({
    workerModulePath: '/Application/App.app/Contents/Resources/app/out/main/localDecisionWorker.js',
    bundleManifestPath: '/Application/App.app/Contents/Resources/laya/package-manifest.json',
    numThreads: 1,
    minimumConfidenceMilli: 800,
  }));
  for (const config of [
    { workerModulePath: 'localDecisionWorker.js', bundleManifestPath: '/models/package-manifest.json', numThreads: 1, minimumConfidenceMilli: 800 },
    { workerModulePath: '/tmp/arbitrary.js', bundleManifestPath: '/models/package-manifest.json', numThreads: 1, minimumConfidenceMilli: 800 },
    { workerModulePath: '/tmp/localDecisionWorker.js', bundleManifestPath: '/models/model.onnx', numThreads: 1, minimumConfidenceMilli: 800 },
    { workerModulePath: '/tmp/localDecisionWorker.js', bundleManifestPath: '/models/package-manifest.json', numThreads: 5, minimumConfidenceMilli: 800 },
    { workerModulePath: '/tmp/localDecisionWorker.js', bundleManifestPath: '/models/package-manifest.json', numThreads: 1, minimumConfidenceMilli: 799 },
  ]) {
    assert.throws(() => assertUtilityConfig(config), (error: unknown) =>
      error instanceof UtilityProtocolError && error.code === 'invalid_config');
  }
});

test('ready only confirms the backend is loaded and a valid proposal is fenced to its request', async () => {
  const { child, runner } = await start();
  assert.equal(runner.availability, 'ready');
  assert.equal(runner.loadedModelRevision, 'fixture-model-revision');
  assert.equal(child.requestMessages().length, 0);

  const input = request();
  const result = runner.submit(input);
  await new Promise<void>((resolve) => setImmediate(resolve));
  const sent = child.requestMessages()[0]!;
  child.send({
    version: UTILITY_PROTOCOL_VERSION,
    type: 'proposal',
    request_id: sent.request_id,
    epoch: sent.epoch,
    proposal: proposal(input),
  });
  const delivery = await result;
  assert.equal(delivery.kind, 'proposal');
  if (delivery.kind === 'proposal') {
    assert.equal(delivery.proposal.source, 's1');
    assert.equal(delivery.proposal.choice?.action, 'start_skill');
  }
  await runner.close();
});

test('request admission has a fixed active-plus-queue bound', async () => {
  const { child, runner } = await start();
  const pending = [
    runner.submit(request({ decision_id: 'decision-1' })),
    runner.submit(request({ decision_id: 'decision-2' })),
    runner.submit(request({ decision_id: 'decision-3' })),
    runner.submit(request({ decision_id: 'decision-4' })),
  ];
  assert.equal(runner.inFlight, 4);
  const overflow = await runner.submit(request({ decision_id: 'decision-5' }));
  assert.equal(overflow.kind, 'proposal');
  if (overflow.kind === 'proposal') {
    assert.equal(overflow.proposal.source, 's1');
    assert.equal(overflow.proposal.failure, 'unavailable');
  }
  assert.equal(child.requestMessages().length, 1);
  await runner.close();
  assert.ok((await Promise.all(pending)).every((delivery) => delivery.kind === 'dropped'));
});

test('worker exit drops pending work, ignores late proposals, and never silently respawns', async () => {
  const child = new FakeUtilityProcess();
  let spawnCount = 0;
  const { runner } = await start(child, {
    spawnUtilityProcess: (_path, _args, _options) => {
      spawnCount += 1;
      setImmediate(() => child.emit('spawn'));
      return child;
    },
  });
  const input = request();
  const pending = runner.submit(input);
  await new Promise<void>((resolve) => setImmediate(resolve));
  const sent = child.requestMessages()[0]!;
  child.exit(1);
  const delivery = await pending;
  assert.equal(delivery.kind, 'dropped');
  if (delivery.kind === 'dropped') assert.equal(delivery.reason, 'stale_epoch');
  child.send({
    version: UTILITY_PROTOCOL_VERSION,
    type: 'proposal',
    request_id: sent.request_id,
    epoch: sent.epoch,
    proposal: proposal(input),
  });
  assert.equal(spawnCount, 1);
  assert.equal(runner.availability, 'unavailable');
  assert.equal((await runner.submit(request({ decision_id: 'decision-after-exit' }))).kind, 'proposal');
  assert.equal(spawnCount, 1);
  await runner.close();
});

test('cancellation drops a pending proposal even when the backend answers late', async () => {
  const { child, runner } = await start();
  const input = request();
  const pending = runner.submit(input);
  await new Promise<void>((resolve) => setImmediate(resolve));
  const sent = child.requestMessages()[0]!;
  runner.cancel({ decision_id: input.decision_id, request_generation: input.request_generation });
  const delivery = await pending;
  assert.equal(delivery.kind, 'dropped');
  if (delivery.kind === 'dropped') assert.equal(delivery.reason, 'cancelled');
  child.send({
    version: UTILITY_PROTOCOL_VERSION,
    type: 'proposal',
    request_id: sent.request_id,
    epoch: sent.epoch,
    proposal: proposal(input),
  });
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert.equal(runner.inFlight, 0);
  await runner.close();
});

test('a cancelled inference with no reply retires on its independent drain bound and drops later work', async () => {
  const { child, runner } = await start(new FakeUtilityProcess(), { activeDrainTimeoutMs: 20 });
  const input = request({ result_deadline_ms: 10_000 });
  const pending = runner.submit(input);
  await new Promise<void>((resolve) => setImmediate(resolve));
  runner.cancel({ decision_id: input.decision_id, request_generation: input.request_generation });
  assert.equal((await pending).kind, 'dropped');

  const queued = runner.submit(request({ decision_id: 'decision-2', result_deadline_ms: 10_000 }));
  assert.equal(runner.inFlight, 2);
  await new Promise<void>((resolve) => setTimeout(resolve, 60));
  assert.equal(runner.availability, 'unavailable');
  assert.equal(runner.unavailableCode, 'worker_timeout');
  const queuedResult = await queued;
  assert.equal(queuedResult.kind, 'dropped');
  if (queuedResult.kind === 'dropped') assert.equal(queuedResult.reason, 'stale_epoch');
  assert.equal(child.exited, true);
  await runner.close();
});

test('close drops active and queued requests and does not deliver a late response', async () => {
  const { child, runner } = await start();
  const input = request();
  const pending = runner.submit(input);
  await new Promise<void>((resolve) => setImmediate(resolve));
  const sent = child.requestMessages()[0]!;
  const close = runner.close();
  const delivery = await pending;
  assert.equal(delivery.kind, 'dropped');
  if (delivery.kind === 'dropped') assert.equal(delivery.reason, 'stale_epoch');
  await close;
  child.send({
    version: UTILITY_PROTOCOL_VERSION,
    type: 'proposal',
    request_id: sent.request_id,
    epoch: sent.epoch,
    proposal: proposal(input),
  });
  assert.equal(runner.availability, 'closed');
});

test('task revision advance fences the prior result and only the new revision may deliver', async () => {
  const { child, runner } = await start();
  const oldRequest = request();
  const oldPending = runner.submit(oldRequest);
  await new Promise<void>((resolve) => setImmediate(resolve));
  const oldSent = child.requestMessages()[0]!;
  const nextRequest = request({
    decision_id: 'decision-2',
    task_revision: 2,
    state: { ...oldRequest.state, task_revision: 2 },
  });
  const nextPending = runner.submit(nextRequest);
  assert.equal((await oldPending).kind, 'dropped');
  const invalidation = child.received.filter((message) => message.type === 'invalidate').at(-1);
  assert.ok(invalidation && invalidation.type === 'invalidate');
  assert.equal(invalidation.epoch, oldSent.epoch + 1);
  child.send({
    version: UTILITY_PROTOCOL_VERSION,
    type: 'unavailable',
    request_id: oldSent.request_id,
    epoch: oldSent.epoch,
    code: 'worker_busy',
    proposal: unavailableForTest(oldRequest),
  });
  await new Promise<void>((resolve) => setImmediate(resolve));
  const newSent = child.requestMessages()[1]!;
  child.send({
    version: UTILITY_PROTOCOL_VERSION,
    type: 'proposal',
    request_id: newSent.request_id,
    epoch: newSent.epoch,
    proposal: proposal(nextRequest),
  });
  assert.equal((await nextPending).kind, 'proposal');
  await runner.close();
});

test('Main can close the lifecycle while model startup is still pending', async () => {
  const child = new FakeUtilityProcess();
  const runner = LocalDecisionProcess.open(options(child));
  assert.equal(runner.availability, 'starting');
  await runner.close();
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert.equal(runner.availability, 'closed');
  assert.equal(child.exited, true);
  assert.equal(child.requestMessages().length, 0);
  assert.equal((await runner.submit(request())).kind, 'dropped');
});

test('an explicit epoch invalidation admits a new Root without accepting an unsolicited Root switch', async () => {
  const { child, runner } = await start();
  const first = request();
  const pending = runner.submit(first);
  await new Promise<void>((resolve) => setImmediate(resolve));
  const sent = child.requestMessages()[0]!;
  child.send({ version: 1, type: 'proposal', request_id: sent.request_id, epoch: sent.epoch, proposal: proposal(first) });
  assert.equal((await pending).kind, 'proposal');
  const next = request({ decision_id: 'decision-next-root', root_task_id: 'root-2', root_epoch: 2,
    state: { ...first.state, root_task_id: 'root-2' } });
  assert.equal((await runner.submit(next)).kind, 'dropped');
  runner.invalidate();
  const nextPending = runner.submit(next);
  await new Promise<void>((resolve) => setImmediate(resolve));
  const nextSent = child.requestMessages().at(-1)!;
  assert.equal(nextSent.request.root_task_id, 'root-2');
  child.send({ version: 1, type: 'proposal', request_id: nextSent.request_id, epoch: nextSent.epoch, proposal: proposal(next) });
  assert.equal((await nextPending).kind, 'proposal');
  await runner.close();
});

test('unavailable model startup yields a fixed S1 unavailable proposal without a fallback planner', async () => {
  const child = new FakeUtilityProcess();
  child.postMessage = (value: ArrayBuffer): void => {
    const message = expectMainMessage(value);
    if (message.type === 'init') {
      child.received.push(message);
      queueMicrotask(() => child.send({
        version: UTILITY_PROTOCOL_VERSION,
        type: 'unavailable',
        code: 'backend_unavailable',
      }));
    } else if (message.type === 'close') {
      child.kill();
    } else {
      child.received.push(message);
    }
  };
  const { runner } = await start(child);
  assert.equal(runner.availability, 'unavailable');
  assert.equal(runner.unavailableCode, 'backend_unavailable');
  const delivery = await runner.submit(request());
  assert.equal(delivery.kind, 'proposal');
  if (delivery.kind === 'proposal') {
    assert.equal(delivery.proposal.source, 's1');
    assert.equal(delivery.proposal.failure, 'unavailable');
    assert.equal(delivery.proposal.choice, null);
    assert.deepEqual(delivery.proposal.answers, []);
  }
  await runner.close();
});

test('invalid build paths fail closed before the utility process can spawn', async () => {
  let calls = 0;
  const child = new FakeUtilityProcess();
  const { runner } = await start(child, {
    workerModulePath: '/tmp/untrusted-worker.js',
    spawnUtilityProcess: () => {
      calls += 1;
      return child;
    },
  });
  assert.equal(runner.availability, 'unavailable');
  assert.equal(runner.unavailableCode, 'invalid_config');
  assert.equal(calls, 0);
  await runner.close();
});

function unavailableForTest(input: DecisionRequest): DecisionProposal {
  return {
    decision_id: input.decision_id,
    request_generation: input.request_generation,
    candidate_set_digest: input.state.candidate_set_digest,
    backend: 'local-s1-unavailable',
    source: 's1',
    model_revision: 'none',
    answers: [],
    choice: null,
    failure: 'unavailable',
    truncated: false,
    queue_ms: 0,
    inference_ms: 0,
  };
}
