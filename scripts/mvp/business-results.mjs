import assert from 'node:assert/strict';

// Business denominators and fixture IDs from complete-design.md §26.7. The
// TextEdit IDs below identify report slots; that section does not name its runs.
// This module does not execute tasks or establish evidence validity.
export const fixtureTaskIds = Object.freeze([
  ...Array.from({ length: 6 }, (_, i) => `SET-${String(i + 1).padStart(2, '0')}`),
  ...Array.from({ length: 7 }, (_, i) => `FORM-${String(i + 1).padStart(2, '0')}`),
  ...Array.from({ length: 4 }, (_, i) => `LIST-${String(i + 1).padStart(2, '0')}`),
  ...Array.from({ length: 3 }, (_, i) => `MODAL-${String(i + 1).padStart(2, '0')}`),
]);
export const textEditTaskIds = Object.freeze(
  Array.from({ length: 20 }, (_, i) => `TEXTEDIT-${String(i + 1).padStart(2, '0')}`),
);
const statuses = new Set(['completed', 'failed', 'timeout', 'unknown', 'unverified', 'cancelled']);
const verdicts = new Set(['satisfied', 'not_satisfied', 'unknown']);

export function summarizeBusinessResults(report) {
  assert.equal(report?.version, 1, 'report version must be 1');
  assert.ok(['fixture', 'textedit'].includes(report.suite), 'unknown suite');
  assert.ok(Array.isArray(report.runs), 'runs must be an array');
  const fixture = report.suite === 'fixture';
  const ids = fixture ? fixtureTaskIds : textEditTaskIds;
  const seeds = fixture ? [1, 2, 3, 4, 5] : [1];
  const expected = new Set(ids.flatMap(id => seeds.map(seed => `${id}/${seed}`)));
  const seen = new Set();
  const counts = { accepted: 0, rejected_before_acceptance: 0, completed: 0, false_completed: 0, unverified_completed: 0,
    failed: 0, timeout: 0, unknown: 0, unverified: 0, cancelled: 0, not_cold_start: 0 };
  const byTask = Object.fromEntries(ids.map(id => [id, { accepted: 0, completed: 0, rejected: 0 }]));

  for (const run of report.runs) {
    assert.equal(typeof run.task_id, 'string', 'task id must be a string');
    assert.ok(Number.isInteger(run.seed), 'seed must be an integer');
    const key = `${run.task_id}/${run.seed}`;
    assert.ok(expected.has(key), `run is outside the frozen denominator: ${key}`);
    assert.ok(!seen.has(key), `duplicate run: ${key}`);
    seen.add(key);
    assert.equal(typeof run.accepted, 'boolean', `missing acceptance fact: ${key}`);
    if (!run.accepted) {
      assert.equal(run.system_reported_completed, false, `rejected run claims completion: ${key}`);
      assert.ok(typeof run.rejection_reason === 'string' && run.rejection_reason.trim(), `missing rejection reason: ${key}`);
      counts.rejected_before_acceptance += 1;
      byTask[run.task_id].rejected += 1;
      continue;
    }
    assert.ok(statuses.has(run.status), `unknown admitted status: ${key}`);
    assert.equal(typeof run.system_reported_completed, 'boolean', `missing system result: ${key}`);
    assert.ok(verdicts.has(run.independent_verdict), `missing independent verdict: ${key}`);
    assert.equal(run.system_reported_completed, run.status === 'completed', `inconsistent system result: ${key}`);
    assert.equal(typeof run.cold_start, 'boolean', `missing cold-start fact: ${key}`);
    assert.equal(typeof run.knowledge_has_task_workflows, 'boolean', `missing release-scope fact: ${key}`);
    counts.accepted += 1;
    byTask[run.task_id].accepted += 1;
    const cold = run.cold_start && !run.knowledge_has_task_workflows;
    if (!cold) counts.not_cold_start += 1;
    if (run.system_reported_completed && run.independent_verdict === 'not_satisfied') counts.false_completed += 1;
    if (run.system_reported_completed && run.independent_verdict === 'unknown') counts.unverified_completed += 1;
    if (cold && run.status === 'completed' && run.independent_verdict === 'satisfied') {
      counts.completed += 1;
      byTask[run.task_id].completed += 1;
    } else if (run.status !== 'completed') {
      counts[run.status] += 1;
    }
  }

  const missing = [...expected].filter(key => !seen.has(key));
  const denominator = expected.size;
  const completeCoverage = missing.length === 0 && counts.accepted === denominator;
  const perTaskMinimum = !fixture || ids.every(id => byTask[id].completed >= 4);
  return {
    version: 1, suite: report.suite, expected_runs: denominator,
    observed_runs: seen.size, missing_runs: missing, ...counts,
    admitted_completion_rate: counts.accepted === 0 ? null : counts.completed / counts.accepted,
    frozen_denominator_completion_rate: counts.completed / denominator,
    rejection_rate: counts.rejected_before_acceptance / denominator,
    per_task: byTask,
    business_quality_thresholds_met: completeCoverage && counts.not_cold_start === 0
      && counts.completed / denominator >= 0.95 && perTaskMinimum
      && counts.false_completed === 0 && counts.unverified_completed === 0,
    aggregation_only: true,
    evidence_validity_audited: false,
    mvp_complete: false,
  };
}
