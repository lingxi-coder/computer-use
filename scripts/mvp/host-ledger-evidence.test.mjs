import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import {
  chmodSync, copyFileSync, existsSync, linkSync, lstatSync, mkdirSync, mkdtempSync, readFileSync,
  readdirSync, realpathSync, rmSync, symlinkSync,
} from 'node:fs';
import test from 'node:test';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';

import { exportHostLedgerEvidence, writeHostLedgerEvidenceFile } from './host-ledger-evidence.mjs';

const ROOT_A = 'root-a';
const ROOT_B = 'root-b';
const SOURCES = ['plan', 'rule', 's1', 'teacher', 'llm', 'computer', 'human'];
const PRIVATE_SENTINEL = 'OTHER_ROOT_PRIVATE_CONTENT_9c32608a';

function databaseDir() {
  return mkdtempSync(join(realpathSync(tmpdir()), 'host-ledger-evidence-'));
}

function createSchema(db) {
  db.exec(`
    CREATE TABLE root_tasks (
      root_id TEXT PRIMARY KEY, seq INTEGER NOT NULL, user_text TEXT NOT NULL,
      state TEXT NOT NULL, state_origin TEXT, task_revision INTEGER NOT NULL,
      root_epoch INTEGER NOT NULL, draft_json TEXT, goal_json TEXT,
      plan_revision INTEGER NOT NULL DEFAULT 0, plan_json TEXT, created_ms INTEGER NOT NULL,
      frozen_ms INTEGER, deadline_ms INTEGER, cancel_requested INTEGER NOT NULL DEFAULT 0,
      cleanup TEXT NOT NULL DEFAULT 'not_needed', activation_seq INTEGER NOT NULL DEFAULT 0,
      failure TEXT, allowed_scope_json TEXT, allowed_scope_app TEXT, frozen_profile_json TEXT
    );
    CREATE TABLE event_log (
      seq INTEGER PRIMARY KEY AUTOINCREMENT, at_ms INTEGER NOT NULL, root_id TEXT,
      kind TEXT NOT NULL, detail TEXT NOT NULL
    );
    CREATE TABLE attempts (
      attempt_id TEXT PRIMARY KEY, root_id TEXT NOT NULL, decision_id TEXT NOT NULL,
      generation INTEGER NOT NULL, state TEXT NOT NULL, skill TEXT NOT NULL, subgoal TEXT NOT NULL,
      quota_actions INTEGER NOT NULL, quota_observations INTEGER NOT NULL,
      activation_seq INTEGER NOT NULL, root_epoch INTEGER NOT NULL, task_revision INTEGER NOT NULL,
      plan_revision INTEGER NOT NULL, deadline_ms INTEGER NOT NULL, effect_class TEXT NOT NULL,
      source TEXT NOT NULL, refunded INTEGER NOT NULL DEFAULT 0, created_ms INTEGER NOT NULL,
      intent_id TEXT
    );
    CREATE TABLE decisions (
      decision_id TEXT PRIMARY KEY, root_id TEXT NOT NULL, generation INTEGER NOT NULL,
      state TEXT NOT NULL, source TEXT, created_ms INTEGER NOT NULL, consumed_attempt TEXT,
      plan_revision INTEGER
    );
    CREATE TABLE attempt_map (
      decision_id TEXT PRIMARY KEY, generation INTEGER NOT NULL, attempt_id TEXT NOT NULL UNIQUE
    );
    CREATE TABLE admission_outbox (
      attempt_id TEXT PRIMARY KEY, payload TEXT NOT NULL, delivered INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE effect_results (
      result_id TEXT NOT NULL, revision INTEGER NOT NULL, root_id TEXT NOT NULL, json TEXT NOT NULL,
      PRIMARY KEY(result_id, revision)
    );
    CREATE TABLE plan_installations (
      root_id TEXT NOT NULL, plan_revision INTEGER NOT NULL, state TEXT NOT NULL,
      attribution_status TEXT NOT NULL, request_id TEXT, generation INTEGER, reason TEXT,
      installed_ms INTEGER, PRIMARY KEY(root_id, plan_revision)
    );
    CREATE TABLE model_calls (
      root_id TEXT NOT NULL, request_id TEXT NOT NULL, generation INTEGER NOT NULL,
      provider TEXT NOT NULL, model TEXT NOT NULL, started_ms INTEGER NOT NULL,
      finished_ms INTEGER, outcome TEXT, usage_json TEXT NOT NULL, reason TEXT,
      PRIMARY KEY(root_id, request_id, generation)
    );
    CREATE TABLE decision_model_calls (
      root_id TEXT NOT NULL, decision_id TEXT NOT NULL, generation INTEGER NOT NULL,
      provider TEXT NOT NULL, model TEXT NOT NULL, started_ms INTEGER NOT NULL,
      finished_ms INTEGER, outcome TEXT, usage_json TEXT NOT NULL,
      PRIMARY KEY(root_id, decision_id, generation)
    );
    CREATE TABLE computer_model_calls (
      root_id TEXT NOT NULL, request_id TEXT NOT NULL, generation INTEGER NOT NULL UNIQUE,
      task_revision INTEGER NOT NULL, root_epoch INTEGER NOT NULL, snapshot_id TEXT NOT NULL,
      structure_sequence INTEGER NOT NULL, deadline_ms INTEGER NOT NULL, plan_revision INTEGER NOT NULL,
      subgoal_id TEXT NOT NULL, created_ms INTEGER NOT NULL,
      stage TEXT NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL, started_ms INTEGER NOT NULL,
      finished_ms INTEGER, outcome TEXT, usage_json TEXT NOT NULL,
      PRIMARY KEY(root_id, request_id, generation)
    );
    PRAGMA user_version = 8;
  `);
}

function seedDatabase(db) {
  const rootInsert = db.prepare(`INSERT INTO root_tasks(
    root_id, seq, user_text, state, state_origin, task_revision, root_epoch, draft_json,
    goal_json, plan_revision, plan_json, created_ms, frozen_ms, deadline_ms,
    cancel_requested, cleanup, activation_seq, failure
  ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`);
  rootInsert.run(ROOT_A, 1, 'ROOT_A_PRIVATE_GOAL', 'active', 'plan', 2, 1, 'ROOT_A_DRAFT', 'ROOT_A_GOAL', 3,
    '{"s2_request_mapping":"not stored here"}', 100, null, 500, 0, 'not_needed', 1, null);
  rootInsert.run(ROOT_B, 2, PRIVATE_SENTINEL, 'failed', null, 8, 4, PRIVATE_SENTINEL, PRIVATE_SENTINEL, 9,
    PRIVATE_SENTINEL, 200, null, 700, 1, 'pending', 4, PRIVATE_SENTINEL);

  const decisionInsert = db.prepare(`INSERT INTO decisions(
    decision_id, root_id, generation, state, source, created_ms, consumed_attempt, plan_revision
  ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`);
  SOURCES.forEach((source, index) => {
    const generation = index + 1;
    decisionInsert.run(`decision-${source}`, ROOT_A, generation, source === 'teacher' ? 'consumed' : 'open',
      source, 100 + generation, source === 'teacher' ? 'attempt-teacher' : null,
      source === 'teacher' ? 2 : source === 'plan' ? null : 3);
  });
  decisionInsert.run('decision-other-root', ROOT_B, 1, 'open', 'computer', 200, 'attempt-other-root', 9);

  const planInstallInsert = db.prepare(`INSERT INTO plan_installations(
    root_id, plan_revision, state, attribution_status, request_id, generation, reason, installed_ms
  ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`);
  planInstallInsert.run(ROOT_A, 2, 'installed', 'model_request', 'request-plan', 7, 'escalation', 150);
  planInstallInsert.run(ROOT_A, 3, 'installed', 'unknown', null, null, null, 180);
  planInstallInsert.run(ROOT_B, 9, 'installed', 'unknown', null, null, null, 220);

  const attemptInsert = db.prepare(`INSERT INTO attempts(
    attempt_id, root_id, decision_id, generation, state, skill, subgoal, quota_actions,
    quota_observations, activation_seq, root_epoch, task_revision, plan_revision, deadline_ms,
    effect_class, source, refunded, created_ms, intent_id
  ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`);
  attemptInsert.run('attempt-teacher', ROOT_A, 'decision-teacher', 4, 'settled', 'ui.toggle', 'private subgoal',
    3, 2, 1, 1, 2, 3, 500, 'reversible_edit', 'teacher', 0, 150, 'intent-a');
  attemptInsert.run('attempt-other-root', ROOT_B, 'decision-other-root', 1, 'settled', 'private.skill', PRIVATE_SENTINEL,
    1, 1, 1, 4, 8, 9, 700, 'external_effect', 'computer', 0, 210, 'intent-b');

  db.prepare('INSERT INTO attempt_map(decision_id, generation, attempt_id) VALUES (?, ?, ?)')
    .run('decision-teacher', 4, 'attempt-teacher');
  db.prepare('INSERT INTO attempt_map(decision_id, generation, attempt_id) VALUES (?, ?, ?)')
    .run('decision-human', 7, 'attempt-other-root');
  db.prepare('INSERT INTO admission_outbox(attempt_id, payload, delivered) VALUES (?, ?, ?)')
    .run('attempt-teacher', 'OUTBOX_PRIVATE_PAYLOAD', 0);
  db.prepare('INSERT INTO admission_outbox(attempt_id, payload, delivered) VALUES (?, ?, ?)')
    .run('attempt-other-root', PRIVATE_SENTINEL, 1);
  db.prepare('INSERT INTO effect_results(result_id, revision, root_id, json) VALUES (?, ?, ?, ?)')
    .run('effect-a', 1, ROOT_A, JSON.stringify({
      result_id: 'effect-a', revision: 1, root_task_id: ROOT_A, attempt_id: 'attempt-teacher',
      intent_id: 'intent-a', predicate: 'goal.required@1', target: 'PRIVATE_EFFECT_TARGET',
      status: 'satisfied', evidence_group: ['snapshot-a'], input_span: null, observed_ms: 160,
      confirmation_scope: 'current_ui', causality: 'confirmed',
    }));
  db.prepare('INSERT INTO effect_results(result_id, revision, root_id, json) VALUES (?, ?, ?, ?)')
    .run('effect-b', 1, ROOT_B, PRIVATE_SENTINEL);
  db.prepare('INSERT INTO event_log(at_ms, root_id, kind, detail) VALUES (?, ?, ?, ?)')
    .run(120, ROOT_A, 'private', 'EVENT_LOG_PRIVATE_PAYLOAD');

  const usageUnknown = '{"input_tokens":null,"output_tokens":null,"cost_micros":null}';
  const decisionCallInsert = db.prepare(`INSERT INTO decision_model_calls(
    root_id, decision_id, generation, provider, model, started_ms, finished_ms, outcome, usage_json
  ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`);
  decisionCallInsert.run(ROOT_A, 'decision-teacher', 4, 'open_ai', 'model-open', 104, null, null, usageUnknown);
  decisionCallInsert.run(ROOT_A, 'decision-llm', 5, 'open_ai', 'model-failed', 105, 106, 'failed', usageUnknown);
  decisionCallInsert.run(ROOT_A, 'decision-s1', 3, 'open_ai', 'model-cancelled', 103, 107, 'cancelled', usageUnknown);
  decisionCallInsert.run(ROOT_A, 'decision-plan', 1, 'open_ai', 'model-partial', 101, 109, 'unavailable',
    '{"input_tokens":12,"output_tokens":null,"cost_micros":null}');
  decisionCallInsert.run(ROOT_B, 'decision-other-root', 1, PRIVATE_SENTINEL, PRIVATE_SENTINEL,
    200, 201, 'proposal', '{"input_tokens":9,"output_tokens":9,"cost_micros":1}');

  const modelCallInsert = db.prepare(`INSERT INTO model_calls(
    root_id, request_id, generation, provider, model, started_ms, finished_ms, outcome, usage_json, reason
  ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`);
  modelCallInsert.run(ROOT_A, 'request-open', 1, 'open_ai', 'planner-open', 101, null, null, usageUnknown, 'goal_compile');
  modelCallInsert.run(ROOT_A, 'request-cancelled', 2, 'open_ai', 'planner-cancelled', 102, 108, 'cancelled', usageUnknown, 'escalation');
  modelCallInsert.run(ROOT_A, 'request-plan', 7, 'open_ai', 'planner-plan', 120, 149, 'proposal', usageUnknown, 'escalation');
  modelCallInsert.run(ROOT_B, 'request-other-root', 1, PRIVATE_SENTINEL, PRIVATE_SENTINEL,
    200, 201, 'proposal', '{"input_tokens":9,"output_tokens":9,"cost_micros":1}', 'goal_compile');

  const computerCallInsert = db.prepare(`INSERT INTO computer_model_calls(
    root_id, request_id, generation, task_revision, root_epoch, snapshot_id, structure_sequence,
    deadline_ms, plan_revision, subgoal_id, created_ms, stage, provider, model, started_ms,
    finished_ms, outcome, usage_json
  ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`);
  computerCallInsert.run(ROOT_A, 'computer-request-open', 11, 2, 1, 'snapshot-a', 14,
    500, 3, 'subgoal-a', 110, 'proposal', 'open_ai', 'computer-open', 111, null, null, usageUnknown);
  computerCallInsert.run(ROOT_B, 'computer-request-other-root', 12, 8, 4, PRIVATE_SENTINEL, 20,
    700, 9, PRIVATE_SENTINEL, 210, 'proposal', PRIVATE_SENTINEL, PRIVATE_SENTINEL,
    211, 212, 'completed', '{"input_tokens":9,"output_tokens":9,"cost_micros":1}');
}

function makeDatabase({ seed = true } = {}) {
  const dir = databaseDir();
  const dbPath = join(dir, 'host.sqlite');
  const db = new DatabaseSync(dbPath);
  createSchema(db);
  if (seed) seedDatabase(db);
  db.close();
  chmodSync(dbPath, 0o600);
  return { dir, dbPath, cleanup: () => rmSync(dir, { recursive: true, force: true }) };
}

function sha256(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

function assertCode(callback, expectedCode) {
  assert.throws(callback, cause => cause?.code === expectedCode);
}

test('exports a bounded, read-only Host snapshot for only the requested Root', () => {
  const fixture = makeDatabase();
  try {
    const beforeHash = sha256(fixture.dbPath);
    const beforeStat = readFileSync(fixture.dbPath).byteLength;
    const beforeFiles = readdirSync(fixture.dir).sort();
    const evidence = exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A });
    assert.equal(sha256(fixture.dbPath), beforeHash);
    assert.equal(readFileSync(fixture.dbPath).byteLength, beforeStat);
    assert.deepEqual(readdirSync(fixture.dir).sort(), beforeFiles, 'read-only export must not create sidecar files');

    assert.equal(evidence.scope.root_id, ROOT_A);
    assert.equal(evidence.scope.root_filter, 'exact_root_id');
    assert.equal(evidence.database.opened_read_only, true);
    assert.equal(evidence.database.query_only, true);
    assert.equal(evidence.schema.user_version, 8);
    assert.equal(evidence.root.state, 'active');
    assert.equal(evidence.root.plan_revision, 3);
    assert.equal(evidence.assessment.mvp_complete, false);
    assert.equal(evidence.assessment.model_quality_passed, null);
    assert.equal(evidence.assessment.business_quality_passed, null);

    assert.deepEqual(evidence.decisions.map(row => row.source), SOURCES,
      'raw Host DecisionSource values are preserved without remapping');
    assert.equal(evidence.attempts.length, 1);
    assert.equal(evidence.attempts[0].source, 'teacher');
    assert.equal(evidence.attempt_map.length, 2);
    assert.equal(evidence.admission_outbox.length, 1);
    assert.equal(evidence.admission_outbox[0].delivered, 0);
    assert.equal(evidence.admission_outbox[0].payload_included, false);
    assert.equal(evidence.effect_results.length, 1);
    assert.equal(evidence.effect_results[0].json_included, false);
    assert.equal(evidence.effect_results[0].effect_status, 'satisfied');
    assert.equal(evidence.effect_results[0].attempt_id, 'attempt-teacher');
    assert.equal(evidence.effect_results[0].attempt_reference_status, 'matched');
    assert.equal(evidence.effect_results[0].source_sequence, null);
    assert.equal(evidence.effect_results[0].source_sequence_status, 'not_stored_in_current_schema');
    assert.equal(evidence.plan_installations.length, 2);
    assert.equal(evidence.plan_installations[0].request_id, 'request-plan');
    assert.equal(evidence.plan_installations[1].attribution_status, 'unknown');

    const call = evidence.call_rows.find(row => row.scope === 's1_decision' && row.decision_id === 'decision-teacher');
    assert.deepEqual(call.usage, {
      status: 'unknown', input_tokens: null, output_tokens: null, cost_micros: null,
    });
    assert.equal(call.outcome, null);
    assert.equal(call.finished_ms, null);
    assert.equal(call.provider_response_id, null);
    assert.equal(call.actual_network_requests, null);
    assert.equal(call.decision_source, 'teacher');
    assert.equal(call.decision_reference_status, 'matched');

    for (const outcome of ['failed', 'cancelled']) {
      const row = evidence.call_rows.find(callRow => callRow.outcome === outcome);
      assert.ok(row);
      assert.deepEqual(row.usage, {
        status: 'unknown', input_tokens: null, output_tokens: null, cost_micros: null,
      }, `${outcome} must not become zero usage`);
    }
    const partial = evidence.call_rows.find(row => row.model === 'model-partial');
    assert.deepEqual(partial.usage, {
      status: 'partial', input_tokens: 12, output_tokens: null, cost_micros: null,
    });
    assert.ok(evidence.cross_references.decisions.some(row => row.consumed_attempt_status === 'matched'));
    assert.ok(evidence.cross_references.decisions.some(row => row.attempt_map_status === 'attempt_not_found_within_selected_root'));
    assert.equal(evidence.cross_references.attempts[0].admission_outbox_status, 'matched');
    assert.equal(evidence.cross_references.attempts[0].admission_outbox_delivered, 0);
    assert.equal(evidence.cross_references.s2_plan_request.status, 'partial_or_unknown');
    assert.equal(evidence.cross_references.s2_plan_request.matched_model_request_count, 1);
    assert.equal(evidence.cross_references.s2_plan_request.unknown_attribution_count, 1);
    assert.equal(evidence.cross_references.s2_plan_request.time_based_matching_used, false);
    assert.equal(evidence.cross_references.local_inference_revisions.status, 'missing_in_current_schema');
    assert.equal(evidence.cross_references.effect_results_to_attempts.status, 'exact_attempt_links_present');
    assert.deepEqual(evidence.cross_references.effect_results, [{
      result_id: 'effect-a', revision: 1, attempt_id: 'attempt-teacher', attempt_reference_status: 'matched',
    }]);
    assert.equal(evidence.cross_references.effect_results_to_attempts.source_sequence_status, 'not_stored_in_current_schema');
    assert.equal(evidence.cross_references.effect_results_to_attempts.separate_verdict_status, 'not_stored_as_a_separate_record');
    assert.equal(evidence.cross_references.decisions.find(row => row.decision_id === 'decision-teacher')
      .s2_request_reference_status, 'matched');
    assert.equal(evidence.cross_references.decisions.find(row => row.decision_id === 'decision-rule')
      .s2_request_reference_status, 'unknown');
    assert.equal(evidence.cross_references.decisions.find(row => row.decision_id === 'decision-plan')
      .s2_request_reference_status, 'missing_legacy_plan_revision');
    assert.equal(evidence.call_rows.find(row => row.scope === 's2_request' && row.request_id === 'request-plan')
      .plan_installation_reference_status, 'matched');
    assert.equal(evidence.cross_references.actual_network_requests.value, null);

    const serialized = JSON.stringify(evidence);
    for (const secret of [PRIVATE_SENTINEL, 'ROOT_A_PRIVATE_GOAL', 'ROOT_A_DRAFT', 'OUTBOX_PRIVATE_PAYLOAD',
      'EFFECT_PRIVATE_PAYLOAD_WITH_ATTEMPT_ID', 'PRIVATE_EFFECT_TARGET', 'EVENT_LOG_PRIVATE_PAYLOAD']) {
      assert.equal(serialized.includes(secret), false, `export must not include ${secret}`);
    }
    assert.equal(evidence.call_rows.some(row => row.root_id === ROOT_B), false);
    assert.equal(evidence.plan_installations.some(row => row.root_id === ROOT_B), false);
    assert.equal(evidence.computer_call_rows.length, 1);
    assert.deepEqual(evidence.computer_call_rows[0], {
      scope: 'computer_request',
      root_id: ROOT_A,
      request_id: 'computer-request-open',
      generation: 11,
      task_revision: 2,
      root_epoch: 1,
      snapshot_id: 'snapshot-a',
      structure_sequence: 14,
      deadline_ms: 500,
      plan_revision: 3,
      subgoal_id: 'subgoal-a',
      created_ms: 110,
      stage: 'proposal',
      provider: 'open_ai',
      model: 'computer-open',
      started_ms: 111,
      finished_ms: null,
      outcome: null,
      usage: { status: 'unknown', input_tokens: null, output_tokens: null, cost_micros: null },
      provider_response_id: null,
      provider_response_id_status: 'not_stored_in_current_schema',
      actual_network_requests: null,
      actual_network_requests_status: 'not_stored_in_current_schema',
    });
    assert.ok(evidence.limits.encoded_bytes <= evidence.limits.max_bytes);
  } finally {
    fixture.cleanup();
  }
});

test('effect JSON is bounded and only strict typed metadata is projected', () => {
  const malformed = makeDatabase();
  try {
    const db = new DatabaseSync(malformed.dbPath);
    db.prepare('UPDATE effect_results SET json = ? WHERE root_id = ?')
      .run(JSON.stringify({ root_task_id: ROOT_A, attempt_id: 'attempt-teacher', secret: 'do-not-export' }), ROOT_A);
    db.close();
    const evidence = exportHostLedgerEvidence({ dbPath: malformed.dbPath, rootId: ROOT_A });
    assert.equal(evidence.effect_results[0].projection_status, 'invalid_shape');
    assert.equal(evidence.effect_results[0].effect_status, null);
    assert.equal(evidence.effect_results[0].attempt_id, null);
    assert.equal(evidence.effect_results[0].attempt_reference_status, 'invalid_shape');
    assert.deepEqual(evidence.cross_references.effect_results, [{
      result_id: 'effect-a', revision: 1, attempt_reference_status: 'invalid_shape',
    }]);
    assert.equal(JSON.stringify(evidence).includes('do-not-export'), false);
  } finally {
    malformed.cleanup();
  }

  const oversized = makeDatabase();
  try {
    const db = new DatabaseSync(oversized.dbPath);
    db.prepare('UPDATE effect_results SET json = ? WHERE root_id = ?')
      .run('x'.repeat(16 * 1_024 + 1), ROOT_A);
    db.close();
    assertCode(() => exportHostLedgerEvidence({ dbPath: oversized.dbPath, rootId: ROOT_A }), 'FIELD_LIMIT');
  } finally {
    oversized.cleanup();
  }
});

test('effect attempt references stay within the selected Root', () => {
  const fixture = makeDatabase();
  try {
    const db = new DatabaseSync(fixture.dbPath);
    const raw = JSON.parse(db.prepare('SELECT json FROM effect_results WHERE root_id = ?').get(ROOT_A).json);
    raw.attempt_id = 'attempt-other-root';
    raw.intent_id = 'intent-b';
    db.prepare('UPDATE effect_results SET json = ? WHERE root_id = ?').run(JSON.stringify(raw), ROOT_A);
    db.close();

    const evidence = exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A });
    assert.equal(evidence.attempts.some(attempt => attempt.attempt_id === 'attempt-other-root'), false);
    assert.equal(evidence.effect_results[0].attempt_reference_status, 'not_found_within_selected_root');
    assert.deepEqual(evidence.cross_references.effect_results, [{
      result_id: 'effect-a', revision: 1, attempt_id: 'attempt-other-root',
      attempt_reference_status: 'not_found_within_selected_root',
    }]);
    assert.equal(evidence.cross_references.effect_results_to_attempts.status, 'partial_or_unknown');
  } finally {
    fixture.cleanup();
  }
});

test('reads an active WAL database without requiring a stable file size', () => {
  const fixture = makeDatabase();
  const writer = new DatabaseSync(fixture.dbPath);
  try {
    writer.exec('PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;');
    writer.prepare('UPDATE root_tasks SET seq = seq + 10 WHERE root_id = ?').run(ROOT_A);
    const before = lstatSync(fixture.dbPath, { bigint: true });
    const beforeFiles = readdirSync(fixture.dir).sort();

    const evidence = exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A });
    const after = lstatSync(fixture.dbPath, { bigint: true });
    assert.equal(after.dev, before.dev);
    assert.equal(after.ino, before.ino);
    assert.equal(evidence.root.seq, 11);
    assert.deepEqual(readdirSync(fixture.dir).sort(), beforeFiles);

    writer.prepare('UPDATE root_tasks SET seq = seq + 1 WHERE root_id = ?').run(ROOT_A);
    const afterWalGrowth = lstatSync(fixture.dbPath, { bigint: true });
    assert.equal(afterWalGrowth.dev, before.dev);
    assert.equal(afterWalGrowth.ino, before.ino);
  } finally {
    writer.close();
    fixture.cleanup();
  }
});

test('rejects incomplete or mismatched Host schemas before exporting Root data', () => {
  const fixture = makeDatabase({ seed: false });
  try {
    const db = new DatabaseSync(fixture.dbPath);
    db.exec('DROP TABLE decisions; PRAGMA user_version = 8;');
    db.close();
    assertCode(() => exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A }), 'SCHEMA_MISMATCH');
  } finally {
    fixture.cleanup();
  }

  const incomplete = databaseDir();
  const dbPath = join(incomplete, 'incomplete.sqlite');
  const db = new DatabaseSync(dbPath);
  db.exec('CREATE TABLE root_tasks(root_id TEXT PRIMARY KEY); PRAGMA user_version = 8;');
  db.close();
  chmodSync(dbPath, 0o600);
  try {
    assertCode(() => exportHostLedgerEvidence({ dbPath, rootId: ROOT_A }), 'SCHEMA_MISMATCH');
  } finally {
    rmSync(incomplete, { recursive: true, force: true });
  }
});

test('rejects a pre-scope Host schema without accepting metadata as current evidence', () => {
  const fixture = makeDatabase();
  try {
    const db = new DatabaseSync(fixture.dbPath);
    db.exec('PRAGMA user_version = 5;');
    db.close();
    assertCode(() => exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A }), 'SCHEMA_VERSION');
  } finally {
    fixture.cleanup();
  }
});

test('rejects a nonexistent database path without creating a file', () => {
  const dir = databaseDir();
  const missing = join(dir, 'missing.sqlite');
  try {
    assertCode(() => exportHostLedgerEvidence({ dbPath: missing, rootId: ROOT_A }), 'INVALID_DATABASE_PATH');
    assert.equal(existsSync(missing), false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('allows ordinary SQLite read permissions only inside a private parent', () => {
  const fixture = makeDatabase();
  try {
    chmodSync(fixture.dbPath, 0o644);
    const evidence = exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A });
    assert.equal(evidence.root.root_id, ROOT_A);

    chmodSync(fixture.dbPath, 0o640);
    assert.equal(exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A }).root.root_id, ROOT_A);

    chmodSync(fixture.dbPath, 0o666);
    assertCode(() => exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A }), 'INVALID_DATABASE_PATH');
  } finally {
    fixture.cleanup();
  }
});

test('rejects overlong cleanup before materializing an artifact', () => {
  const fixture = makeDatabase();
  const outputPath = join(fixture.dir, 'must-not-exist.json');
  const db = new DatabaseSync(fixture.dbPath);
  db.prepare('UPDATE root_tasks SET cleanup = ? WHERE root_id = ?').run('x'.repeat(1_025), ROOT_A);
  db.close();
  try {
    assertCode(() => writeHostLedgerEvidenceFile({
      dbPath: fixture.dbPath, rootId: ROOT_A, outputPath,
    }), 'FIELD_LIMIT');
    assert.equal(existsSync(outputPath), false);
  } finally {
    fixture.cleanup();
  }
});

test('reports unsupported defensive SQLite runtime before opening the database', () => {
  const fixture = makeDatabase();
  const original = DatabaseSync.prototype.enableDefensive;
  try {
    DatabaseSync.prototype.enableDefensive = undefined;
    assertCode(() => exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A }), 'UNSUPPORTED_SQLITE_RUNTIME');
  } finally {
    DatabaseSync.prototype.enableDefensive = original;
    fixture.cleanup();
  }
});

test('CLI writes a new mode-0600 file for the requested Root and refuses replacement', () => {
  const fixture = makeDatabase();
  try {
    const cliPath = fileURLToPath(new URL('./host-ledger-evidence.mjs', import.meta.url));
    const outputPath = join(fixture.dir, 'root-evidence.json');
    const result = spawnSync(process.execPath, [
      cliPath, '--db', fixture.dbPath, '--root', ROOT_A, '--output', outputPath,
    ], { encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, '');
    const evidence = JSON.parse(readFileSync(outputPath, 'utf8'));
    const outputStat = lstatSync(outputPath, { bigint: true });
    assert.equal(evidence.scope.root_id, ROOT_A);
    assert.equal(outputStat.isFile(), true);
    assert.equal(outputStat.uid, BigInt(process.getuid()));
    assert.equal(outputStat.nlink, 1n);
    assert.equal(outputStat.mode & 0o777n, 0o600n);
    assert.equal(JSON.stringify(evidence).includes(fixture.dbPath), false, 'the private database path is not disclosed');
    const before = sha256(outputPath);
    assertCode(() => writeHostLedgerEvidenceFile({ dbPath: fixture.dbPath, rootId: ROOT_A, outputPath }), 'OUTPUT_EXISTS');
    assert.equal(sha256(outputPath), before, 'an existing output file is never replaced');
  } finally {
    fixture.cleanup();
  }
});

test('CLI leaves no output behind when schema validation fails', () => {
  const fixture = makeDatabase({ seed: false });
  const db = new DatabaseSync(fixture.dbPath);
  db.exec('DROP TABLE decisions; PRAGMA user_version = 8;');
  db.close();
  const outputPath = join(fixture.dir, 'must-not-exist.json');
  try {
    const cliPath = fileURLToPath(new URL('./host-ledger-evidence.mjs', import.meta.url));
    const result = spawnSync(process.execPath, [
      cliPath, '--db', fixture.dbPath, '--root', ROOT_A, '--output', outputPath,
    ], { encoding: 'utf8' });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /SCHEMA_MISMATCH/u);
    assert.equal(existsSync(outputPath), false);
  } finally {
    fixture.cleanup();
  }
});

test('rejects symlinked paths, hard-linked databases, and non-private parents', () => {
  const fixture = makeDatabase();
  try {
    const linkedPath = join(fixture.dir, 'hard-link.sqlite');
    linkSync(fixture.dbPath, linkedPath);
    assertCode(() => exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A }), 'INVALID_DATABASE_PATH');
    assertCode(() => exportHostLedgerEvidence({ dbPath: linkedPath, rootId: ROOT_A }), 'INVALID_DATABASE_PATH');

    const privateFixture = makeDatabase();
    try {
      const databaseLink = join(privateFixture.dir, 'database-link.sqlite');
      symlinkSync(privateFixture.dbPath, databaseLink);
      assertCode(() => exportHostLedgerEvidence({ dbPath: databaseLink, rootId: ROOT_A }), 'INVALID_DATABASE_PATH');

      const parentLink = join(privateFixture.dir, 'parent-link');
      symlinkSync(privateFixture.dir, parentLink, 'dir');
      assertCode(() => exportHostLedgerEvidence({
        dbPath: join(parentLink, 'host.sqlite'), rootId: ROOT_A,
      }), 'INVALID_DATABASE_PATH');
      assertCode(() => writeHostLedgerEvidenceFile({
        dbPath: privateFixture.dbPath,
        rootId: ROOT_A,
        outputPath: join(parentLink, 'evidence.json'),
      }), 'INVALID_OUTPUT_PATH');

      const publicParent = join(privateFixture.dir, 'group-visible-parent');
      mkdirSync(publicParent, 0o755);
      chmodSync(publicParent, 0o755);
      const publicDb = join(publicParent, 'host.sqlite');
      copyFileSync(privateFixture.dbPath, publicDb);
      chmodSync(publicDb, 0o600);
      assertCode(() => exportHostLedgerEvidence({ dbPath: publicDb, rootId: ROOT_A }), 'INVALID_DATABASE_PATH');
    } finally {
      privateFixture.cleanup();
    }
  } finally {
    fixture.cleanup();
  }
});

test('row and encoded-byte limits fail closed', () => {
  const fixture = makeDatabase();
  try {
    assertCode(() => exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A, maxRows: 1 }), 'ROW_LIMIT');
    assertCode(() => exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A, maxBytes: 1_024 }), 'BYTE_LIMIT');
  } finally {
    fixture.cleanup();
  }
});

test('exports only frozen admission identities for the selected Root and rejects corrupted metadata', () => {
  const fixture = makeDatabase();
  try {
    const profile = {
      catalog_id: 'a'.repeat(64), selector: 'b'.repeat(64),
      source_freeze_sha256: 'c'.repeat(64), source_request_sha256: 'd'.repeat(64),
      goals_sha256: 'e'.repeat(64), suite: 'fixture', task_id: 'SET-02', seed: 1,
    };
    const db = new DatabaseSync(fixture.dbPath);
    db.prepare('UPDATE root_tasks SET frozen_profile_json = ? WHERE root_id = ?').run(JSON.stringify(profile), ROOT_A);
    db.prepare('UPDATE root_tasks SET frozen_profile_json = ? WHERE root_id = ?').run(PRIVATE_SENTINEL, ROOT_B);
    db.close();
    const evidence = exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A });
    assert.deepEqual(evidence.root.frozen_profile, profile);
    assert.equal(JSON.stringify(evidence).includes(PRIVATE_SENTINEL), false);
    const update = new DatabaseSync(fixture.dbPath);
    update.prepare('UPDATE root_tasks SET frozen_profile_json = ? WHERE root_id = ?').run(JSON.stringify({ ...profile, initial_state: {} }), ROOT_A);
    update.close();
    assertCode(() => exportHostLedgerEvidence({ dbPath: fixture.dbPath, rootId: ROOT_A }), 'FROZEN_PROFILE');
  } finally { fixture.cleanup(); }
});
