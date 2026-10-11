import assert from 'node:assert/strict';
import test from 'node:test';

import { EnvelopeFactory, validateEnvelope } from '@computer-use/desktop-api';

import {
  S1_QUALITY_CASE_COUNT,
  S1_QUALITY_CORPUS,
  S1_QUALITY_STRATA,
  fixedOptionPermutations,
  projectLayaFeatureInput,
  requestWithOptionOrder,
} from './s1-quality-corpus.mjs';

test('the manual corpus contains twenty balanced, schema-valid DecisionRequests', () => {
  assert.equal(S1_QUALITY_CORPUS.model_cases.length, S1_QUALITY_CASE_COUNT);
  assert.equal(S1_QUALITY_CORPUS.reference_labels.length, S1_QUALITY_CASE_COUNT);
  assert.deepEqual([...new Set(S1_QUALITY_CORPUS.model_cases.map((item) => item.case_id))].length, S1_QUALITY_CASE_COUNT);
  const counts = Object.fromEntries(S1_QUALITY_STRATA.map((stratum) => [stratum, 0]));
  const factory = new EnvelopeFactory('quality-corpus-test', () => 0);
  for (const qualityCase of S1_QUALITY_CORPUS.model_cases) {
    counts[qualityCase.stratum] += 1;
    const result = validateEnvelope(factory.make({ type: 'decision_request', payload: qualityCase.request }));
    assert.equal(result.ok, true, `${qualityCase.case_id}: ${result.errors?.join('; ')}`);
    assert.equal(qualityCase.request.questions.length, 1);
    assert.equal(qualityCase.request.questions[0].kind, 'choice');
  }
  assert.deepEqual(counts, { navigation: 5, disambiguation: 5, recovery: 5, bilingual: 5 });
});

test('reference labels are sidecar data and never enter the projected model features', () => {
  const labelsByCase = new Map(S1_QUALITY_CORPUS.reference_labels.map((label) => [label.case_id, label]));
  for (const qualityCase of S1_QUALITY_CORPUS.model_cases) {
    const label = labelsByCase.get(qualityCase.case_id);
    assert.ok(label);
    const request = qualityCase.request;
    const candidateIds = new Set(request.state.candidates.map((candidate) => candidate.candidate_id));
    assert.ok(candidateIds.has(label.expected_candidate_id));
    const optionIds = request.questions[0].options.map((option) => option.key);
    assert.ok(optionIds.includes(label.expected_candidate_id));
    assert.deepEqual(optionIds, request.state.candidates.map((candidate) => candidate.candidate_id));
    for (const option of request.questions[0].options) {
      const candidate = request.state.candidates.find((item) => item.candidate_id === option.key);
      assert.equal(candidate.description, option.description);
      assert.equal(candidate.precondition.state, 'executable');
    }

    const features = projectLayaFeatureInput(request);
    const featureText = JSON.stringify(features);
    assert.equal(featureText.includes(qualityCase.case_id), false);
    assert.equal(featureText.includes('expected_candidate_id'), false);
    assert.equal(featureText.includes('rationale'), false);
    assert.equal(featureText.includes(label.rationale), false);
    assert.deepEqual(Object.keys(features).sort(), ['question', 'state']);
    assert.deepEqual(Object.keys(features.question).sort(), ['crit', 'ins', 't']);
  }
  assert.equal(new Set(S1_QUALITY_CORPUS.model_cases.map(({ request }) => request.root_task_id)).size, 1);
});

test('fixed option-order arms are deterministic permutations and leave state untouched', () => {
  for (let optionCount = 2; optionCount <= 8; optionCount += 1) {
    const first = fixedOptionPermutations(optionCount);
    const second = fixedOptionPermutations(optionCount);
    assert.deepEqual(first, second);
    assert.equal(new Set(first.map((order) => order.join(','))).size, first.length);
    for (const order of first) {
      assert.deepEqual([...order].sort((a, b) => a - b), Array.from({ length: optionCount }, (_, index) => index));
    }
  }

  const original = S1_QUALITY_CORPUS.model_cases[0].request;
  const originalState = structuredClone(original.state);
  const reversed = requestWithOptionOrder(original, fixedOptionPermutations(2)[1]);
  assert.notDeepEqual(reversed.questions[0].options, original.questions[0].options);
  assert.deepEqual(reversed.state, originalState);
  assert.deepEqual(original.state, originalState);
  assert.throws(() => requestWithOptionOrder(original, [0, 0]), /complete permutation/);
});
