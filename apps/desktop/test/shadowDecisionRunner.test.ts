import assert from 'node:assert/strict';
import test from 'node:test';

import type { DecisionCancel, DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';
import type { Delivery } from '@computer-use/decision-worker';
import { ShadowDecisionRunner, type DecisionRunner } from '../src/main/shadowDecisionRunner.js';

function request(decisionId = 'decision-private-id', generation = 1): DecisionRequest {
  return {
    decision_id: decisionId, lane_id: 'lane-1', request_generation: generation,
    root_task_id: 'root-1', task_revision: 1, plan_revision: 1, root_epoch: 1,
    decision_program: 'ui.step@1', calibration_revision: 'none', created_ms: 1, result_deadline_ms: 1000,
    questions: [{ id: 'q-secret', kind: 'choice', prompt: 'select a candidate', options: [] }],
    state: {
      root_task_id: 'root-1', task_revision: 1, goal_summary: 'private goal text', subgoal: 'ensure',
      snapshot_revision: 1, objects: [], candidates: [
        { candidate_id: 'candidate-private-a', skill: 'ui.ensure_value', semantic_target: null, binding: null,
          parameters: {}, effect_class: 'reversible_edit', precondition: { state: 'executable' }, description: 'private candidate text' },
        { candidate_id: 'candidate-private-b', skill: 'ui.ensure_value', semantic_target: null, binding: null,
          parameters: {}, effect_class: 'reversible_edit', precondition: { state: 'executable' }, description: 'another private candidate' },
      ], recent_results: [], missing_fields: [], omitted_items: [], candidate_set_digest: 'cs1:private',
    },
  };
}

function proposal(value: DecisionRequest, candidate: string | null, failure: DecisionProposal['failure'] = null): DecisionProposal {
  return {
    decision_id: value.decision_id, request_generation: value.request_generation,
    candidate_set_digest: value.state.candidate_set_digest, backend: 'shadow-test', source: 's1', model_revision: 'm0',
    answers: candidate ? [{ question_id: 'q-secret', selected: candidate, confidence_milli: 700 }] : [],
    choice: candidate ? { action: 'start_skill', candidate } : null,
    failure, truncated: false, queue_ms: 0, inference_ms: 1,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
const tick = (): Promise<void> => new Promise((resolve) => setImmediate(resolve));

class StubRunner implements DecisionRunner {
  readonly requests: DecisionRequest[] = [];
  readonly cancellations: DecisionCancel[] = [];
  invalidations = 0;
  submitFn: (request: DecisionRequest) => Promise<Delivery> = async (value) => ({ kind: 'proposal', proposal: proposal(value, 'candidate-private-a') });
  cancel(_cancel: DecisionCancel): void { this.cancellations.push(_cancel); }
  invalidate(): number { this.invalidations += 1; return this.invalidations; }
  submit(value: DecisionRequest): Promise<Delivery> { this.requests.push(value); return this.submitFn(value); }
}

test('returns the primary Delivery unchanged and gives shadow an independent request copy', async () => {
  const primary = new StubRunner();
  const shadow = new StubRunner();
  const primaryDelivery: Delivery = { kind: 'proposal', proposal: proposal(request(), 'candidate-private-a') };
  primary.submitFn = (value) => {
    // A synchronous primary mutation must not contaminate the already-cloned shadow request.
    value.state.goal_summary = 'primary mutation';
    return Promise.resolve(primaryDelivery);
  };
  let shadowInitialGoal: string | null = null;
  shadow.submitFn = async (value) => {
    shadowInitialGoal = value.state.goal_summary;
    value.state.goal_summary = 'shadow mutation';
    return { kind: 'proposal', proposal: proposal(value, 'candidate-private-b') };
  };
  const input = request();
  const runner = new ShadowDecisionRunner(primary, shadow);
  const delivered = await runner.submit(input);
  assert.equal(delivered, primaryDelivery);
  assert.equal(primary.requests[0], input);
  assert.notEqual(shadow.requests[0], input);
  assert.equal(shadowInitialGoal, 'private goal text');
  assert.equal(shadow.requests[0]?.state.goal_summary, 'shadow mutation');
  assert.equal(input.state.goal_summary, 'primary mutation');
  await tick();
  assert.deepEqual(runner.statistics, {
    submitted: 1, completed: 1, unavailable: 0, refused: 0, dropped: 0,
    disagreements: 1, latency_count: 1, latency_total_ms: runner.statistics.latency_total_ms,
    latency_max_ms: runner.statistics.latency_max_ms,
  });
  assert.deepEqual(Object.keys(runner.statistics).sort(), [
    'completed', 'disagreements', 'dropped', 'latency_count', 'latency_max_ms', 'latency_total_ms',
    'refused', 'submitted', 'unavailable',
  ]);
  assert.equal(JSON.stringify(runner.statistics).includes('candidate-private'), false);
  assert.equal(JSON.stringify(runner.statistics).includes('private goal'), false);
});

test('an unavailable primary remains unavailable even when shadow has a proposal', async () => {
  const primary = new StubRunner();
  const shadow = new StubRunner();
  const input = request();
  const unavailable: Delivery = { kind: 'proposal', proposal: proposal(input, null, 'unavailable') };
  primary.submitFn = async () => unavailable;
  shadow.submitFn = async (value) => ({ kind: 'proposal', proposal: proposal(value, 'candidate-private-b') });
  const runner = new ShadowDecisionRunner(primary, shadow);
  assert.equal(await runner.submit(input), unavailable);
  await tick();
  assert.equal(runner.statistics.completed, 1);
  assert.equal(runner.statistics.unavailable, 0, 'the shadow succeeded; the primary result is not shadow accounting');
  assert.equal(runner.statistics.disagreements, 0, 'an unavailable primary is not treated as an observed choice');
});

test('confidence differences are not decision disagreements', async () => {
  const primary = new StubRunner();
  const shadow = new StubRunner();
  primary.submitFn = async (value) => {
    const result = proposal(value, 'candidate-private-a');
    result.answers[0]!.confidence_milli = 100;
    return { kind: 'proposal', proposal: result };
  };
  shadow.submitFn = async (value) => {
    const result = proposal(value, 'candidate-private-a');
    result.answers[0]!.confidence_milli = 900;
    return { kind: 'proposal', proposal: result };
  };
  const runner = new ShadowDecisionRunner(primary, shadow);
  await runner.submit(request());
  await tick();
  assert.equal(runner.statistics.disagreements, 0);
});

test('proposals with mismatched task identity are not compared as a disagreement', async () => {
  const primary = new StubRunner();
  const shadow = new StubRunner();
  shadow.submitFn = async (value) => {
    const result = proposal(value, 'candidate-private-b');
    result.request_generation += 1;
    return { kind: 'proposal', proposal: result };
  };
  const runner = new ShadowDecisionRunner(primary, shadow);
  await runner.submit(request());
  await tick();
  assert.equal(runner.statistics.completed, 1);
  assert.equal(runner.statistics.disagreements, 0);
});

test('shadow failure is isolated and primary Delivery still resolves normally', async () => {
  const primary = new StubRunner();
  const shadow = new StubRunner();
  const delivery: Delivery = { kind: 'proposal', proposal: proposal(request(), 'candidate-private-a') };
  primary.submitFn = async () => delivery;
  shadow.submitFn = async () => { throw new Error('private shadow diagnostic must not escape'); };
  const runner = new ShadowDecisionRunner(primary, shadow);
  assert.equal(await runner.submit(request()), delivery);
  await tick();
  assert.equal(runner.statistics.submitted, 1);
  assert.equal(runner.statistics.unavailable, 1);
  assert.equal(runner.statistics.disagreements, 0);
});

test('cancel and invalidate fan out; late results from either epoch are not counted', async () => {
  const primary = new StubRunner();
  const shadow = new StubRunner();
  const primaryResult = deferred<Delivery>();
  const shadowResult = deferred<Delivery>();
  primary.submitFn = () => primaryResult.promise;
  shadow.submitFn = () => shadowResult.promise;
  const runner = new ShadowDecisionRunner(primary, shadow);
  const input = request();
  const delivery = runner.submit(input);
  await tick();
  runner.cancel({ decision_id: input.decision_id, request_generation: input.request_generation });
  assert.equal(primary.cancellations.length, 1);
  assert.equal(shadow.cancellations.length, 1);
  primaryResult.resolve({ kind: 'dropped', reason: 'cancelled', decisionId: input.decision_id, generation: 1 });
  shadowResult.resolve({ kind: 'proposal', proposal: proposal(input, 'candidate-private-b') });
  await delivery;
  await tick();
  assert.equal(runner.statistics.submitted, 1);
  assert.equal(runner.statistics.completed + runner.statistics.dropped + runner.statistics.unavailable, 0);
  assert.equal(runner.statistics.latency_count, 0);

  const primaryResult2 = deferred<Delivery>();
  const shadowResult2 = deferred<Delivery>();
  primary.submitFn = () => primaryResult2.promise;
  shadow.submitFn = () => shadowResult2.promise;
  const input2 = request('decision-second', 1);
  const delivery2 = runner.submit(input2);
  await tick();
  runner.invalidate();
  assert.equal(primary.invalidations, 1);
  assert.equal(shadow.invalidations, 1);
  primaryResult2.resolve({ kind: 'dropped', reason: 'stale_epoch', decisionId: input2.decision_id, generation: 1 });
  shadowResult2.resolve({ kind: 'proposal', proposal: proposal(input2, 'candidate-private-a') });
  await delivery2;
  await tick();
  assert.equal(runner.statistics.completed + runner.statistics.dropped + runner.statistics.unavailable, 0);
  assert.equal(runner.statistics.latency_count, 0);
});

test('in-flight shadow observations are bounded and excess requests are refused without delaying primary', async () => {
  const primary = new StubRunner();
  const shadow = new StubRunner();
  const shadowResult = deferred<Delivery>();
  shadow.submitFn = () => shadowResult.promise;
  const runner = new ShadowDecisionRunner(primary, shadow, { maxInFlight: 1 });
  const first = request('first', 1);
  const second = request('second', 1);
  const primaryFirst = await runner.submit(first);
  const primarySecond = await runner.submit(second);
  assert.equal(primaryFirst.kind, 'proposal');
  assert.equal(primarySecond.kind, 'proposal');
  assert.equal(shadow.requests.length, 1);
  assert.equal(runner.shadowInFlight, 1);
  assert.equal(runner.statistics.submitted, 1);
  assert.equal(runner.statistics.refused, 1);
  shadowResult.resolve({ kind: 'proposal', proposal: proposal(first, 'candidate-private-a') });
  await tick();
  assert.equal(runner.shadowInFlight, 0);
});

test('close invalidates both runners and bounds waiting for shadow shutdown', async () => {
  const primary = new StubRunner();
  const shadow = Object.assign(new StubRunner(), { close: () => new Promise<void>(() => undefined) });
  const runner = new ShadowDecisionRunner(primary, shadow, { closeTimeoutMs: 10 });
  const started = performance.now();
  await runner.close();
  assert.ok(performance.now() - started < 500);
  assert.equal(primary.invalidations, 1);
  assert.equal(shadow.invalidations, 1);
  assert.equal(runner.statistics.submitted, 0);
});

test('primary control errors propagate while shadow cancellation and invalidation still run', () => {
  const primary = new StubRunner();
  const shadow = new StubRunner();
  const cancelError = new Error('primary cancel');
  const invalidateError = new Error('primary invalidate');
  primary.cancel = () => { throw cancelError; };
  primary.invalidate = () => { throw invalidateError; };
  const runner = new ShadowDecisionRunner(primary, shadow);

  assert.throws(() => runner.cancel({ decision_id: 'decision', request_generation: 1 }), (error) => error === cancelError);
  assert.equal(shadow.cancellations.length, 1);
  assert.throws(() => runner.invalidate(), (error) => error === invalidateError);
  assert.equal(shadow.invalidations, 1);
});

test('close still attempts shadow cleanup if primary invalidation throws', async () => {
  const primary = new StubRunner();
  const invalidationError = new Error('primary invalidate');
  primary.invalidate = () => { throw invalidationError; };
  let shadowClosed = false;
  const shadow = Object.assign(new StubRunner(), { close: () => { shadowClosed = true; } });
  const runner = new ShadowDecisionRunner(primary, shadow);

  await assert.rejects(runner.close(), (error) => error === invalidationError);
  assert.equal(shadow.invalidations, 1);
  assert.equal(shadowClosed, true);
});

test('observer clock failures leave the primary promise unchanged and omit invalid timing', async () => {
  for (const clock of [() => { throw new Error('private clock error'); }, () => Number.NaN]) {
    const input = request();
    const primary = new StubRunner();
    const expected: Delivery = { kind: 'proposal', proposal: proposal(input, 'candidate-private-a') };
    const delivered = Promise.resolve(expected);
    primary.submit = () => delivered;
    const runner = new ShadowDecisionRunner(primary, new StubRunner(), { clock });
    assert.equal(runner.submit(input), delivered);
    assert.equal(await delivered, expected);
    await tick();
    assert.equal(runner.statistics.completed, 1);
    assert.equal(runner.statistics.latency_count, 0);
    assert.equal(runner.statistics.latency_total_ms, 0);
    assert.equal(runner.statistics.latency_max_ms, 0);
    await runner.close();
  }
});

test('cancellation before deferred observer startup never submits shadow work', async () => {
  const input = request();
  const shadow = new StubRunner();
  const runner = new ShadowDecisionRunner(new StubRunner(), shadow);
  const result = runner.submit(input);
  runner.cancel({ decision_id: input.decision_id, request_generation: input.request_generation });
  assert.equal((await result).kind, 'proposal');
  await tick();
  assert.equal(shadow.requests.length, 0);
  assert.equal(runner.statistics.submitted, 0);
  assert.equal(runner.statistics.completed, 0);
  assert.equal(runner.shadowInFlight, 0);
  await runner.close();
});
