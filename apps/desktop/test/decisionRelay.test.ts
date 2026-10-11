import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { test } from 'node:test';

import type { DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';
import { DecisionWorker, ScriptedTeacher } from '@computer-use/decision-worker';

import { DecisionRelay, type DecisionTransport } from '../src/main/decisionRelay.js';

class FakeTransport extends EventEmitter implements DecisionTransport {
  sent: DecisionProposal[] = [];

  sendDecisionProposal(proposal: DecisionProposal): void {
    this.sent.push(proposal);
  }
}

const request = (generation = 1): DecisionRequest => ({
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
    goal_summary: 'turn off the daily picks',
    subgoal: 'ensure',
    snapshot_revision: 1,
    objects: [],
    candidates: [
      {
        candidate_id: 'c1',
        skill: 'ui.ensure_value',
        semantic_target: null,
        binding: null,
        parameters: {},
        effect_class: 'reversible_edit',
        precondition: { state: 'executable' },
        description: 'Set "Daily picks" to off',
      },
    ],
    recent_results: [],
    missing_fields: [],
    omitted_items: [],
    candidate_set_digest: 'cs1:x',
  },
  questions: [{ id: 'q1', kind: 'choice', prompt: 'which', options: [] }],
  created_ms: 0,
  result_deadline_ms: 1_000,
});

const settle = (): Promise<void> => new Promise((done) => setTimeout(done, 40));

test('a request from the host is answered through the worker and sent back', async () => {
  const transport = new FakeTransport();
  new DecisionRelay(transport, new DecisionWorker(new ScriptedTeacher())).attach();
  transport.emit('decisionRequest', request());
  await settle();
  assert.equal(transport.sent.length, 1);
  assert.equal(transport.sent[0]?.decision_id, 'dec-1');
  assert.equal(transport.sent[0]?.source, 'teacher');
});

test('a cancel from the host stops the answer from being sent, however long the backend takes', async () => {
  const transport = new FakeTransport();
  const relay = new DecisionRelay(transport, new DecisionWorker(new ScriptedTeacher({ delayMs: 30 })));
  relay.attach();
  transport.emit('decisionRequest', request());
  transport.emit('decisionCancel', { decision_id: 'dec-1', request_generation: 1 });
  await new Promise((done) => setTimeout(done, 90));
  assert.deepEqual(transport.sent, []);
  assert.deepEqual(relay.trace, ['dropped dec-1 generation 1: cancelled']);
});

test('a worker that was lost delivers nothing that was in flight', async () => {
  const transport = new FakeTransport();
  const relay = new DecisionRelay(transport, new DecisionWorker(new ScriptedTeacher({ delayMs: 30 })));
  relay.attach();
  transport.emit('decisionRequest', request());
  relay.workerLost();
  await new Promise((done) => setTimeout(done, 90));
  assert.deepEqual(transport.sent, []);
  assert.match(relay.trace[0] ?? '', /stale_epoch/);
});

test('the relay makes no judgement of its own: a backend that cannot answer still answers', async () => {
  const transport = new FakeTransport();
  new DecisionRelay(transport, new DecisionWorker(new ScriptedTeacher({ unavailable: true }))).attach();
  transport.emit('decisionRequest', request());
  await settle();
  assert.equal(transport.sent[0]?.failure, 'unavailable');
});

test('a replacement worker accepts new-session IDs while an old in-flight answer stays fenced', async () => {
  const transport = new FakeTransport();
  const relay = new DecisionRelay(transport, new DecisionWorker(new ScriptedTeacher({ delayMs: 30 })));
  relay.attach(); transport.emit('decisionRequest', request());
  relay.replaceWorker(new DecisionWorker(new ScriptedTeacher()));
  transport.emit('decisionRequest', request());
  await new Promise((done) => setTimeout(done, 90));
  assert.equal(transport.sent.length, 1);
  assert.match(relay.trace[0] ?? '', /stale_epoch/);
  assert.equal(transport.listenerCount('decisionRequest'), 1);
});
