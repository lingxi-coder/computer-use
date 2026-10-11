import assert from 'node:assert/strict';
import test from 'node:test';
import type { DecisionRequest, NativeAuthorization, OwnedDocumentStatus } from '@computer-use/desktop-api';
import { createSessionDecisionBackend } from '../src/main/decisionBackend.js';

const request = (): DecisionRequest => ({
  decision_id: 'decision-1', lane_id: 'lane-1', request_generation: 1,
  root_task_id: 'root-1', task_revision: 1, plan_revision: 1, root_epoch: 1,
  decision_program: 'ui.step@1', calibration_revision: 'none',
  created_ms: 0, result_deadline_ms: 1000,
  state: {
    root_task_id: 'root-1', task_revision: 1, goal_summary: 'turn the switch off',
    subgoal: 'ensure', snapshot_revision: 1, objects: [], recent_results: [],
    missing_fields: [], omitted_items: [], candidate_set_digest: 'cs1:current',
    candidates: [{ candidate_id: 'candidate-1', skill: 'ui.ensure_value',
      semantic_target: null, binding: null, parameters: {}, effect_class: 'reversible_edit',
      precondition: { state: 'executable' }, description: 'turn the switch off' }],
  },
  questions: [{ id: 'q1', kind: 'choice', prompt: 'which?', options: [] }],
});

test('only an explicit fixture session receives a scripted decision', async () => {
  let native: NativeAuthorization['phase'] = 'not_native';
  const backend = createSessionDecisionBackend(() => ({ native, owned: 'not_created' }));
  const scripted = await backend.decide(request(), new AbortController().signal);
  assert.equal(scripted.backend, 'teacher-scripted');
  assert.deepEqual(scripted.choice, { action: 'start_skill', candidate: 'candidate-1' });

  for (native of ['observing', 'authorizing', 'authorized', 'switching', 'unavailable'] as const) {
    const proposal = await backend.decide(request(), new AbortController().signal);
    assert.equal(proposal.backend, 'local-s1-unavailable');
    assert.equal(proposal.source, 's1');
    assert.equal(proposal.model_revision, 'none');
    assert.equal(proposal.choice, null);
    assert.deepEqual(proposal.answers, []);
    assert.equal(proposal.failure, 'unavailable');
    assert.equal(proposal.decision_id, 'decision-1');
    assert.equal(proposal.candidate_set_digest, 'cs1:current');
  }
});

test('owned lifecycle states and an absent session cannot select the fixture teacher', async () => {
  const phases: OwnedDocumentStatus['phase'][] = ['creating', 'observing', 'authorizing_edit',
    'edit_authorized', 'authorizing_save', 'save_authorized', 'revisiting', 'completed', 'unavailable'];
  for (const owned of phases) {
    const backend = createSessionDecisionBackend(() => ({ native: 'not_native', owned }));
    assert.equal((await backend.decide(request(), new AbortController().signal)).choice, null);
  }
  assert.equal((await createSessionDecisionBackend(() => null)
    .decide(request(), new AbortController().signal)).choice, null);
});

test('an aborted fixture request cannot produce a reference action', async () => {
  const controller = new AbortController(); controller.abort();
  const proposal = await createSessionDecisionBackend(() => ({ native: 'not_native', owned: 'not_created' }))
    .decide(request(), controller.signal);
  assert.equal(proposal.choice, null);
  assert.equal(proposal.failure, 'unavailable');
});
