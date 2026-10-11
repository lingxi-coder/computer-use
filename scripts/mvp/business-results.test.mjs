import assert from 'node:assert/strict';
import test from 'node:test';
import { fixtureTaskIds, textEditTaskIds, summarizeBusinessResults } from './business-results.mjs';

function report(suite = 'fixture') {
  const ids = suite === 'fixture' ? fixtureTaskIds : textEditTaskIds;
  const seeds = suite === 'fixture' ? [1, 2, 3, 4, 5] : [1];
  return { version: 1, suite, runs: ids.flatMap(task_id => seeds.map(seed => ({
    task_id, seed, accepted: true, status: 'completed', system_reported_completed: true,
    independent_verdict: 'satisfied', cold_start: true, knowledge_has_task_workflows: false,
  }))) };
}

test('separate canonical denominators never establish full MVP completion', () => {
  for (const [suite, expected] of [['fixture', 100], ['textedit', 20]]) {
    const result = summarizeBusinessResults(report(suite));
    assert.equal(result.expected_runs, expected);
    assert.equal(result.business_quality_thresholds_met, true);
    assert.equal(result.mvp_complete, false);
    assert.equal(result.evidence_validity_audited, false);
  }
});

test('unknown remains admitted in the denominator and is not a success', () => {
  const r = report();
  Object.assign(r.runs[0], { status: 'unknown', system_reported_completed: false, independent_verdict: 'satisfied' });
  const result = summarizeBusinessResults(r);
  assert.equal(result.accepted, 100);
  assert.equal(result.completed, 99);
  assert.equal(result.unknown, 1);
});

test('missing and pre-acceptance rejected cases cannot shrink the frozen gate', () => {
  const r = report();
  r.runs.pop();
  Object.assign(r.runs[0], { accepted: false, system_reported_completed: false, rejection_reason: 'unsupported' });
  const result = summarizeBusinessResults(r);
  assert.equal(result.expected_runs, 100);
  assert.equal(result.accepted, 98);
  assert.equal(result.rejected_before_acceptance, 1);
  assert.equal(result.missing_runs.length, 1);
  assert.equal(result.business_quality_thresholds_met, false);
});

test('even one false or unverified completion blocks the quality gate', () => {
  for (const verdict of ['not_satisfied', 'unknown']) {
    const r = report();
    r.runs[0].independent_verdict = verdict;
    const result = summarizeBusinessResults(r);
    assert.equal(result.false_completed, verdict === 'not_satisfied' ? 1 : 0);
    assert.equal(result.unverified_completed, verdict === 'unknown' ? 1 : 0);
    assert.equal(result.business_quality_thresholds_met, false);
  }
});

test('global 95 percent cannot hide a task with only three of five completions', () => {
  const r = report();
  for (const run of r.runs.slice(0, 2)) Object.assign(run, {
    status: 'failed', system_reported_completed: false, independent_verdict: 'not_satisfied',
  });
  const result = summarizeBusinessResults(r);
  assert.equal(result.completed, 98);
  assert.equal(result.business_quality_thresholds_met, false);
});

test('task workflows and warm runs cannot qualify as cold-start acceptance', () => {
  const r = report();
  r.runs[0].cold_start = false;
  r.runs[1].knowledge_has_task_workflows = true;
  const result = summarizeBusinessResults(r);
  assert.equal(result.not_cold_start, 2);
  assert.equal(result.completed, 98);
  assert.equal(result.business_quality_thresholds_met, false);
});

test('duplicates, out-of-scope runs and contradictory receipts are rejected', () => {
  const duplicate = report(); duplicate.runs.push({ ...duplicate.runs[0] });
  assert.throws(() => summarizeBusinessResults(duplicate), /duplicate/);
  const unknown = report(); unknown.runs[0].task_id = 'EASY-01';
  assert.throws(() => summarizeBusinessResults(unknown), /outside/);
  const contradictory = report(); contradictory.runs[0].system_reported_completed = false;
  assert.throws(() => summarizeBusinessResults(contradictory), /inconsistent/);
  const textualSeed = report(); textualSeed.runs[0].seed = '1';
  assert.throws(() => summarizeBusinessResults(textualSeed), /integer/);
});
