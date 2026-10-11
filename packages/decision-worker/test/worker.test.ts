import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { Candidate, DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';

import { DecisionWorker, ScriptedTeacher, type DecisionBackend } from '../src/index.js';

function candidate(id: string, skill: string, description: string, state: 'executable' | 'rejected' = 'executable'): Candidate {
  return {
    candidate_id: id,
    skill,
    semantic_target: null,
    binding: null,
    parameters: {},
    effect_class: 'reversible_edit',
    precondition: state === 'executable' ? { state } : { state, reason: 'not allowed' },
    description,
  };
}

function request(generation = 1, candidates?: Candidate[], goal = 'turn off the daily picks in Push'): DecisionRequest {
  return {
    decision_id: 'dec-1',
    lane_id: 'lane-1',
    request_generation: generation,
    root_task_id: 'root-1',
    task_revision: 1,
    plan_revision: 1,
    root_epoch: 1,
    decision_program: 'ui.step@1',
    calibration_revision: 'none',
    state: {
      root_task_id: 'root-1',
      task_revision: 1,
      goal_summary: goal,
      subgoal: 'ensure',
      snapshot_revision: 3,
      objects: [],
      candidates: candidates ?? [
        candidate('c1', 'ui.ensure_value', 'Set "Daily picks" (switch in Email, currently on) to off'),
        candidate('c2', 'ui.ensure_value', 'Set "Daily picks" (switch in Push, currently on) to off'),
        candidate('c3', 'observe_more', 'Take a fresh observation before deciding'),
      ],
      recent_results: [],
      missing_fields: [],
      omitted_items: [],
      candidate_set_digest: 'cs1:abc',
    },
    questions: [{ id: 'q1', kind: 'choice', prompt: 'which?', options: [] }],
    created_ms: 0,
    result_deadline_ms: 1000,
  };
}

/** A backend whose answers the test releases by hand. */
class Manual implements DecisionBackend {
  readonly name = 'manual';
  readonly pending: { request: DecisionRequest; release: (p: DecisionProposal) => void; signal: AbortSignal }[] = [];

  decide(req: DecisionRequest, signal: AbortSignal): Promise<DecisionProposal> {
    return new Promise((release) => this.pending.push({ request: req, release, signal }));
  }

  async answer(index: number, overrides: Partial<DecisionProposal> = {}): Promise<void> {
    const entry = this.pending[index]!;
    entry.release({
      decision_id: entry.request.decision_id,
      request_generation: entry.request.request_generation,
      candidate_set_digest: entry.request.state.candidate_set_digest,
      backend: 'manual',
      source: 'teacher',
      model_revision: 'm',
      answers: [],
      choice: { action: 'observe_more', facts: [] },
      failure: null,
      truncated: false,
      queue_ms: 0,
      inference_ms: 0,
      ...overrides,
    });
    await new Promise((done) => setImmediate(done));
  }
}

test('a failing local S1 backend stays an unavailable S1 result and exposes no raw error', async () => {
  const worker = new DecisionWorker({
    name: 'laya', source: 's1',
    async decide() { throw new Error('private model path and payload must stay private'); },
  });
  const delivery = await worker.submit(request());
  assert.equal(delivery.kind, 'proposal');
  if (delivery.kind !== 'proposal') return;
  assert.equal(delivery.proposal.source, 's1');
  assert.equal(delivery.proposal.failure, 'unavailable');
  assert.equal(delivery.proposal.choice, null);
  assert.equal(delivery.proposal.backend, 'laya');
  assert.ok(!JSON.stringify(delivery).includes('private model path'));
});

test('the scripted teacher picks the candidate its preference names, and is reproducible', async () => {
  const teacher = new ScriptedTeacher({ preferWord: 'Push' });
  const proposal = await teacher.decide(request());
  assert.equal(proposal.choice?.action, 'start_skill');
  if (proposal.choice?.action === 'start_skill') assert.equal(proposal.choice.candidate, 'c2');
  assert.equal(proposal.source, 'teacher');
  assert.equal(proposal.decision_id, 'dec-1');
  assert.equal(proposal.candidate_set_digest, 'cs1:abc');
  assert.deepEqual(await teacher.decide(request()), proposal);
});

test('without a preference ties go to the smaller id, controls are skipped, and rejected candidates are never chosen', async () => {
  const teacher = new ScriptedTeacher();
  // The goal names Push, so the Push candidate has more in common with it.
  const named = await teacher.decide(request());
  assert.equal(named.choice?.action === 'start_skill' && named.choice.candidate, 'c2');
  // Without a group in the goal the two tie, and the smaller id wins.
  const tie = await teacher.decide(request(1, undefined, 'turn off the daily picks'));
  assert.equal(tie.choice?.action === 'start_skill' && tie.choice.candidate, 'c1');

  const only = await teacher.decide(
    request(1, [candidate('c1', 'ui.activate', 'Discard', 'rejected'), candidate('c2', 'observe_more', 'look again')]),
  );
  // Nothing but a control path is executable: the stand-in takes the first executable one.
  assert.equal(only.choice?.action === 'start_skill' && only.choice.candidate, 'c2');

  const nothing = await teacher.decide(request(1, [candidate('c1', 'ui.activate', 'Discard', 'rejected')]));
  assert.equal(nothing.choice?.action, 'observe_more');
});

test('a proposal for the current request is delivered', async () => {
  const worker = new DecisionWorker(new ScriptedTeacher());
  const delivery = await worker.submit(request());
  assert.equal(delivery.kind, 'proposal');
  assert.equal(worker.inFlight, 0);
});

test('a cancelled request delivers nothing even when the backend finishes anyway', async () => {
  const backend = new Manual();
  const worker = new DecisionWorker(backend);
  const pending = worker.submit(request());
  assert.equal(worker.inFlight, 1);
  worker.cancel({ decision_id: 'dec-1', request_generation: 1 });
  assert.equal(backend.pending[0]!.signal.aborted, true, 'the backend is told');
  await backend.answer(0); // it finishes regardless: cancelling does not stop the computation
  assert.deepEqual(await pending, { kind: 'dropped', reason: 'cancelled', decisionId: 'dec-1', generation: 1 });
});

test('a newer generation of the same decision replaces the older, whose answer is then dropped', async () => {
  const backend = new Manual();
  const worker = new DecisionWorker(backend);
  const first = worker.submit(request(1));
  const second = worker.submit(request(2));
  assert.equal(backend.pending[0]!.signal.aborted, true);
  await backend.answer(0);
  await backend.answer(1);
  assert.deepEqual(await first, { kind: 'dropped', reason: 'superseded', decisionId: 'dec-1', generation: 1 });
  assert.equal((await second).kind, 'proposal');

  // An older generation arriving afterwards is refused outright.
  const late = await worker.submit(request(1));
  assert.deepEqual(late, { kind: 'dropped', reason: 'stale_generation', decisionId: 'dec-1', generation: 1 });
});

test('cancelling an old generation does not cancel the request that replaced it', async () => {
  const backend = new Manual();
  const worker = new DecisionWorker(backend);
  const first = worker.submit(request(1));
  const second = worker.submit(request(2));
  worker.cancel({ decision_id: 'dec-1', request_generation: 1 });
  assert.equal(backend.pending[1]!.signal.aborted, false);
  await backend.answer(0);
  await backend.answer(1);
  assert.equal((await first).kind, 'dropped');
  assert.equal((await second).kind, 'proposal');
});

test('a new epoch isolates everything in flight', async () => {
  const backend = new Manual();
  const worker = new DecisionWorker(backend, 4);
  const pending = worker.submit(request());
  assert.equal(worker.invalidate(), 5);
  await backend.answer(0);
  assert.deepEqual(await pending, { kind: 'dropped', reason: 'stale_epoch', decisionId: 'dec-1', generation: 1 });
  // What is asked after the change is fine.
  const fresh = worker.submit(request(2));
  await backend.answer(1);
  assert.equal((await fresh).kind, 'proposal');
});

test('an answer that is not for what was asked is dropped, not delivered', async () => {
  for (const wrong of [
    { decision_id: 'dec-other' },
    { request_generation: 9 },
    { candidate_set_digest: 'cs1:other' },
  ]) {
    const backend = new Manual();
    const worker = new DecisionWorker(backend);
    const pending = worker.submit(request());
    await backend.answer(0, wrong);
    const delivery = await pending;
    assert.equal(delivery.kind === 'dropped' && delivery.reason, 'mismatched_identity', JSON.stringify(wrong));
  }
});

test('a backend that fails answers as unavailable, which the host knows how to wait on', async () => {
  const worker = new DecisionWorker({
    name: 'broken',
    decide: () => Promise.reject(new Error('no route to the model')),
  });
  const delivery = await worker.submit(request());
  assert.equal(delivery.kind, 'proposal');
  if (delivery.kind === 'proposal') {
    assert.equal(delivery.proposal.failure, 'unavailable');
    assert.equal(delivery.proposal.choice, null);
    assert.equal(delivery.proposal.decision_id, 'dec-1');
  }
  const offline = await new DecisionWorker(new ScriptedTeacher({ unavailable: true })).submit(request());
  assert.equal(offline.kind === 'proposal' && offline.proposal.failure, 'unavailable');
});

test('a slow backend is still answered in order: the delay is the backend\'s, not the worker\'s', async () => {
  const worker = new DecisionWorker(new ScriptedTeacher({ delayMs: 20, preferWord: 'Push' }));
  const started = Date.now();
  const delivery = await worker.submit(request());
  assert.ok(Date.now() - started >= 15);
  assert.equal(delivery.kind, 'proposal');
});
