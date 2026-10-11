import {
  closeSync, fchmodSync, fstatSync, fsyncSync, lstatSync, openSync, realpathSync,
  unlinkSync, writeFileSync,
} from 'node:fs';
import { basename, dirname, isAbsolute, join, normalize, parse, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { DatabaseSync } from 'node:sqlite';

const SCHEMA_VERSION = 8;
const DEFAULT_MAX_ROWS = 2_000;
const MAX_ALLOWED_ROWS = 10_000;
const DEFAULT_MAX_BYTES = 2 * 1024 * 1024;
const MAX_ALLOWED_BYTES = 16 * 1024 * 1024;
const MAX_FIELD_BYTES = 1_024;
const MAX_EFFECT_JSON_BYTES = 16 * 1_024;
const MAX_SCHEMA_TABLES = 128;
const MAX_SCHEMA_COLUMNS = 128;
const USAGE_KEYS = ['input_tokens', 'output_tokens', 'cost_micros'];
const U64_MAX = (1n << 64n) - 1n;
const MAX_SAFE_BIGINT = BigInt(Number.MAX_SAFE_INTEGER);

const EXPECTED_COLUMNS = Object.freeze({
  root_tasks: [
    'root_id', 'seq', 'user_text', 'state', 'state_origin', 'task_revision', 'root_epoch',
    'draft_json', 'goal_json', 'plan_revision', 'plan_json', 'created_ms', 'frozen_ms',
    'deadline_ms', 'cancel_requested', 'cleanup', 'activation_seq', 'failure',
    'allowed_scope_json', 'allowed_scope_app', 'frozen_profile_json',
  ],
  decisions: [
    'decision_id', 'root_id', 'generation', 'state', 'source', 'created_ms',
    'consumed_attempt', 'plan_revision',
  ],
  attempts: [
    'attempt_id', 'root_id', 'decision_id', 'generation', 'state', 'skill', 'subgoal',
    'quota_actions', 'quota_observations', 'activation_seq', 'root_epoch', 'task_revision',
    'plan_revision', 'deadline_ms', 'effect_class', 'source', 'refunded', 'created_ms', 'intent_id',
  ],
  attempt_map: ['decision_id', 'generation', 'attempt_id'],
  admission_outbox: ['attempt_id', 'payload', 'delivered'],
  effect_results: ['result_id', 'revision', 'root_id', 'json'],
  plan_installations: [
    'root_id', 'plan_revision', 'state', 'attribution_status', 'request_id', 'generation',
    'reason', 'installed_ms',
  ],
  model_calls: [
    'root_id', 'request_id', 'generation', 'provider', 'model', 'started_ms', 'finished_ms',
    'outcome', 'usage_json', 'reason',
  ],
  decision_model_calls: ['root_id', 'decision_id', 'generation', 'provider', 'model', 'started_ms', 'finished_ms', 'outcome', 'usage_json'],
  computer_model_calls: [
    'root_id', 'request_id', 'generation', 'task_revision', 'root_epoch', 'snapshot_id',
    'structure_sequence', 'deadline_ms', 'plan_revision', 'subgoal_id', 'created_ms', 'stage',
    'provider', 'model', 'started_ms', 'finished_ms', 'outcome', 'usage_json',
  ],
});

export class HostLedgerEvidenceError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'HostLedgerEvidenceError';
    this.code = code;
  }
}

function error(code, message) {
  return new HostLedgerEvidenceError(code, message);
}

function validateLimits({ maxRows = DEFAULT_MAX_ROWS, maxBytes = DEFAULT_MAX_BYTES } = {}) {
  if (!Number.isSafeInteger(maxRows) || maxRows < 1 || maxRows > MAX_ALLOWED_ROWS) {
    throw error('INVALID_LIMIT', `maxRows must be an integer from 1 to ${MAX_ALLOWED_ROWS}`);
  }
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1_024 || maxBytes > MAX_ALLOWED_BYTES) {
    throw error('INVALID_LIMIT', `maxBytes must be an integer from 1024 to ${MAX_ALLOWED_BYTES}`);
  }
  return { maxRows, maxBytes };
}

function validateRootId(rootId) {
  if (typeof rootId !== 'string' || rootId.trim().length === 0
    || Buffer.byteLength(rootId, 'utf8') > MAX_FIELD_BYTES
    || /[\u0000-\u001f\u007f]/u.test(rootId)) {
    throw error('INVALID_ROOT_ID', 'rootId must be a bounded non-empty identifier');
  }
  return rootId;
}

function currentUid() {
  if (typeof process.getuid !== 'function') throw error('PRIVATE_PATH_REQUIRED', 'Current-user ownership cannot be verified');
  return BigInt(process.getuid());
}

function validateAbsoluteNormalizedPath(value, code, label) {
  if (typeof value !== 'string' || !isAbsolute(value) || value.includes('\0') || resolve(value) !== value
    || normalize(value) !== value || value === parse(value).root) {
    throw error(code, `${label} must be a normalized absolute path`);
  }
}

function inspectComponents(absolutePath, code, label) {
  validateAbsoluteNormalizedPath(absolutePath, code, label);
  const parsed = parse(absolutePath);
  const parts = absolutePath.slice(parsed.root.length).split(sep).filter(Boolean);
  let current = parsed.root;
  let finalStat = null;
  for (const [index, part] of parts.entries()) {
    current = join(current, part);
    let stat;
    try {
      stat = lstatSync(current, { bigint: true });
    } catch {
      throw error(code, `${label} contains a missing path component`);
    }
    if (stat.isSymbolicLink()) throw error(code, `${label} cannot contain symbolic links`);
    const isFinal = index === parts.length - 1;
    if (!isFinal && !stat.isDirectory()) throw error(code, `${label} parent components must be directories`);
    finalStat = stat;
  }
  if (!finalStat) throw error(code, `${label} is invalid`);
  let canonicalPath;
  try {
    canonicalPath = realpathSync(absolutePath);
  } catch {
    throw error(code, `${label} could not be canonicalized`);
  }
  if (canonicalPath !== absolutePath) throw error(code, `${label} must use its canonical path`);
  return finalStat;
}

function privateDirectoryIdentity(directoryPath, code, label) {
  const stat = inspectComponents(directoryPath, code, label);
  if (!stat.isDirectory() || stat.uid !== currentUid() || (stat.mode & 0o077n) !== 0n
    || (stat.mode & 0o700n) !== 0o700n) {
    throw error(code, `${label} must be a private directory owned by the current user`);
  }
  return { dev: stat.dev, ino: stat.ino, uid: stat.uid, mode: stat.mode & 0o777n };
}

function privateFileIdentity(filePath, code, label) {
  const stat = inspectComponents(filePath, code, label);
  if (!stat.isFile() || stat.uid !== currentUid() || stat.nlink !== 1n
    || (stat.mode & 0o033n) !== 0n || (stat.mode & 0o400n) === 0n) {
    throw error(code, `${label} must be an owner-owned, single-link regular file without group/other write or execute permission`);
  }
  return { dev: stat.dev, ino: stat.ino, uid: stat.uid, nlink: stat.nlink, mode: stat.mode & 0o777n };
}

function sameIdentity(left, right, includeLinks = true) {
  return left.dev === right.dev && left.ino === right.ino && left.uid === right.uid
    && (!includeLinks || left.nlink === right.nlink) && left.mode === right.mode;
}

function validateDatabasePath(dbPath) {
  validateAbsoluteNormalizedPath(dbPath, 'INVALID_DATABASE_PATH', 'dbPath');
  const parent = dirname(dbPath);
  return {
    path: dbPath,
    file: privateFileIdentity(dbPath, 'INVALID_DATABASE_PATH', 'dbPath'),
    parent: privateDirectoryIdentity(parent, 'INVALID_DATABASE_PATH', 'dbPath parent'),
  };
}

function assertDatabaseIdentity(expected) {
  const current = validateDatabasePath(expected.path);
  if (!sameIdentity(expected.file, current.file) || !sameIdentity(expected.parent, current.parent, false)) {
    throw error('DATABASE_IDENTITY_CHANGED', 'Host database file identity changed during export');
  }
}

function asJsonInteger(value, label) {
  if (typeof value === 'bigint') {
    if (value >= -MAX_SAFE_BIGINT && value <= MAX_SAFE_BIGINT) return Number(value);
    return value.toString();
  }
  if (Number.isSafeInteger(value)) return value;
  throw error('INVALID_DATABASE_VALUE', `${label} is not an exact SQLite integer`);
}

function boundedTextExpression(column, alias) {
  return `CASE WHEN ${column} IS NULL OR length(CAST(${column} AS BLOB)) <= ?2 THEN ${column} ELSE NULL END AS ${alias}, length(CAST(${column} AS BLOB)) AS ${alias}__bytes`;
}

function boundedText(row, name, { nullable = false, maxBytes = MAX_FIELD_BYTES } = {}) {
  const bytes = row[`${name}__bytes`];
  const value = row[name];
  if (bytes === null) {
    if (!nullable || value !== null) throw error('INVALID_DATABASE_VALUE', `${name} is unexpectedly null`);
    return null;
  }
  const byteLength = asJsonInteger(bytes, `${name} byte length`);
  if (typeof byteLength !== 'number' || byteLength > maxBytes) {
    throw error('FIELD_LIMIT', 'A selected ledger field exceeds the field-size limit');
  }
  if (typeof value !== 'string' || Buffer.byteLength(value, 'utf8') !== byteLength) {
    throw error('INVALID_DATABASE_VALUE', `${name} is not valid bounded text`);
  }
  return value;
}

function queryRows(db, sql, rootId, budget, { single = false, maxFieldBytes = MAX_FIELD_BYTES } = {}) {
  const remaining = budget.maxRows - budget.rows;
  if (remaining < 0) throw error('ROW_LIMIT', 'Ledger row limit exceeded');
  const limit = Math.min(remaining + 1, single ? 2 : remaining + 1);
  const statement = db.prepare(sql);
  statement.setReadBigInts(true);
  const rows = [];
  for (const row of statement.iterate(rootId, maxFieldBytes, limit)) {
    if (rows.length >= remaining) throw error('ROW_LIMIT', 'Ledger row limit exceeded');
    budget.rows += 1;
    rows.push(row);
  }
  return rows;
}

function tableColumns(db, tableName) {
  const statement = db.prepare(`PRAGMA table_info('${tableName}')`);
  const columns = statement.all().map(row => row.name);
  if (columns.length > MAX_SCHEMA_COLUMNS) throw error('SCHEMA_LIMIT', 'A schema table has too many columns');
  return columns;
}

function inspectSchema(db) {
  const versionRow = db.prepare('PRAGMA user_version').get();
  const userVersion = Number(versionRow.user_version);
  if (userVersion !== SCHEMA_VERSION) {
    throw error('SCHEMA_VERSION', `Expected Host schema ${SCHEMA_VERSION}; found ${String(versionRow.user_version)}`);
  }

  const tableNamesStatement = db.prepare(
    "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?",
  );
  const tableNames = tableNamesStatement.all(MAX_SCHEMA_TABLES + 1).map(row => row.name);
  if (tableNames.length > MAX_SCHEMA_TABLES) throw error('SCHEMA_LIMIT', 'The database has too many tables');
  const present = new Set(tableNames);
  const tables = {};
  for (const [name, expectedColumns] of Object.entries(EXPECTED_COLUMNS)) {
    const columns = present.has(name) ? tableColumns(db, name) : [];
    const missingColumns = expectedColumns.filter(column => !columns.includes(column));
    tables[name] = { present: present.has(name), columns, missing_columns: missingColumns };
    if (!present.has(name) || missingColumns.length > 0) {
      throw error('SCHEMA_MISMATCH', `Required Host schema table ${name} is missing or incomplete`);
    }
  }

  return {
    user_version: userVersion,
    expected_user_version: SCHEMA_VERSION,
    tables,
    optional_tables: {
      local_inference_revisions: {
        present: present.has('local_inference_revisions'),
        exported: false,
      },
    },
  };
}

/** Admission identities contain no objective, scope, reset, or provider payload. */
function frozenProfileMetadata(text) {
  if (text === null) return null;
  let value;
  try { value = JSON.parse(text); } catch { throw error('FROZEN_PROFILE', 'Invalid frozen admission metadata'); }
  const hashKeys = ['catalog_id', 'selector', 'source_freeze_sha256', 'source_request_sha256', 'goals_sha256'];
  const keys = [...hashKeys, 'suite', 'task_id', 'seed'].sort();
  if (!value || Array.isArray(value) || typeof value !== 'object'
    || JSON.stringify(Object.keys(value).sort()) !== JSON.stringify(keys)
    || !hashKeys.every(key => typeof value[key] === 'string' && /^[a-f0-9]{64}$/.test(value[key]))) {
    throw error('FROZEN_PROFILE', 'Invalid frozen admission metadata');
  }
  const fixtureId = /^(SET-0[1-6]|FORM-0[1-7]|LIST-0[1-4]|MODAL-0[1-3])$/;
  const texteditId = /^TEXTEDIT-(0[1-9]|1[0-9]|20)$/;
  if (!Number.isInteger(value.seed) || !(value.suite === 'fixture'
    ? fixtureId.test(value.task_id) && value.seed >= 1 && value.seed <= 5
    : value.suite === 'textedit' && texteditId.test(value.task_id) && value.seed === 1)) {
    throw error('FROZEN_PROFILE', 'Frozen admission identity is outside the complete denominator');
  }
  return value;
}

function selectedRoot(db, rootId, budget) {
  const columns = [
    boundedTextExpression('state', 'state'),
    boundedTextExpression('state_origin', 'state_origin'),
    boundedTextExpression('cleanup', 'cleanup'),
    boundedTextExpression('frozen_profile_json', 'frozen_profile_json'),
  ];
  const sql = `SELECT ${columns.join(', ')}, seq, task_revision, root_epoch, plan_revision,
    created_ms, frozen_ms, deadline_ms, cancel_requested, activation_seq
    FROM root_tasks WHERE root_id = ?1 LIMIT ?3`;
  const rows = queryRows(db, sql, rootId, budget, { single: true });
  if (rows.length === 0) throw error('ROOT_NOT_FOUND', 'No Host Root matches rootId');
  const row = rows[0];
  return {
    root_id: rootId,
    frozen_profile: frozenProfileMetadata(boundedText(row, 'frozen_profile_json', { nullable: true })),
    seq: asJsonInteger(row.seq, 'root seq'),
    state: boundedText(row, 'state'),
    state_origin: boundedText(row, 'state_origin', { nullable: true }),
    task_revision: asJsonInteger(row.task_revision, 'task_revision'),
    root_epoch: asJsonInteger(row.root_epoch, 'root_epoch'),
    plan_revision: asJsonInteger(row.plan_revision, 'plan_revision'),
    created_ms: asJsonInteger(row.created_ms, 'created_ms'),
    frozen_ms: row.frozen_ms === null ? null : asJsonInteger(row.frozen_ms, 'frozen_ms'),
    deadline_ms: row.deadline_ms === null ? null : asJsonInteger(row.deadline_ms, 'deadline_ms'),
    cancel_requested: asJsonInteger(row.cancel_requested, 'cancel_requested'),
    cleanup: boundedText(row, 'cleanup'),
    activation_seq: asJsonInteger(row.activation_seq, 'activation_seq'),
  };
}

function selectDecisions(db, rootId, budget) {
  const sql = `SELECT ${[
    boundedTextExpression('decision_id', 'decision_id'),
    boundedTextExpression('state', 'state'),
    boundedTextExpression('source', 'source'),
    boundedTextExpression('consumed_attempt', 'consumed_attempt'),
  ].join(', ')}, generation, created_ms, plan_revision
    FROM decisions WHERE root_id = ?1 ORDER BY created_ms, generation, decision_id LIMIT ?3`;
  return queryRows(db, sql, rootId, budget).map(row => ({
    decision_id: boundedText(row, 'decision_id'),
    generation: asJsonInteger(row.generation, 'decision generation'),
    state: boundedText(row, 'state'),
    source: boundedText(row, 'source', { nullable: true }),
    created_ms: asJsonInteger(row.created_ms, 'decision created_ms'),
    consumed_attempt: boundedText(row, 'consumed_attempt', { nullable: true }),
    plan_revision: row.plan_revision === null ? null : asJsonInteger(row.plan_revision, 'decision plan_revision'),
  }));
}

function selectPlanInstallations(db, rootId, budget) {
  const sql = `SELECT ${[
    boundedTextExpression('state', 'state'),
    boundedTextExpression('attribution_status', 'attribution_status'),
    boundedTextExpression('request_id', 'request_id'),
    boundedTextExpression('reason', 'reason'),
  ].join(', ')}, plan_revision, generation, installed_ms
    FROM plan_installations WHERE root_id = ?1 ORDER BY plan_revision LIMIT ?3`;
  return queryRows(db, sql, rootId, budget).map(row => {
    const state = boundedText(row, 'state');
    const attributionStatus = boundedText(row, 'attribution_status');
    const requestId = boundedText(row, 'request_id', { nullable: true });
    const generation = row.generation === null ? null : asJsonInteger(row.generation, 'plan request generation');
    const reason = boundedText(row, 'reason', { nullable: true });
    const installedMs = row.installed_ms === null ? null : asJsonInteger(row.installed_ms, 'plan installed_ms');
    if (!['proposed', 'installed'].includes(state)
      || !['model_request', 'unknown'].includes(attributionStatus)
      || (state === 'proposed') !== (installedMs === null)
      || (attributionStatus === 'model_request'
        ? requestId === null || generation === null || reason === null
        : requestId !== null || generation !== null || reason !== null)) {
      throw error('INVALID_DATABASE_VALUE', 'plan_installations row has an invalid attribution shape');
    }
    return {
      root_id: rootId,
      plan_revision: asJsonInteger(row.plan_revision, 'installed plan_revision'),
      state,
      attribution_status: attributionStatus,
      request_id: requestId,
      generation,
      reason,
      installed_ms: installedMs,
    };
  });
}

function selectAttempts(db, rootId, budget) {
  const sql = `SELECT ${[
    boundedTextExpression('attempt_id', 'attempt_id'),
    boundedTextExpression('decision_id', 'decision_id'),
    boundedTextExpression('state', 'state'),
    boundedTextExpression('skill', 'skill'),
    boundedTextExpression('effect_class', 'effect_class'),
    boundedTextExpression('source', 'source'),
    boundedTextExpression('intent_id', 'intent_id'),
  ].join(', ')}, generation, quota_actions, quota_observations, activation_seq,
    root_epoch, task_revision, plan_revision, deadline_ms, refunded, created_ms
    FROM attempts WHERE root_id = ?1 ORDER BY created_ms, generation, attempt_id LIMIT ?3`;
  return queryRows(db, sql, rootId, budget).map(row => ({
    attempt_id: boundedText(row, 'attempt_id'),
    decision_id: boundedText(row, 'decision_id'),
    generation: asJsonInteger(row.generation, 'attempt generation'),
    state: boundedText(row, 'state'),
    skill: boundedText(row, 'skill'),
    quota_actions: asJsonInteger(row.quota_actions, 'quota_actions'),
    quota_observations: asJsonInteger(row.quota_observations, 'quota_observations'),
    activation_seq: asJsonInteger(row.activation_seq, 'activation_seq'),
    root_epoch: asJsonInteger(row.root_epoch, 'attempt root_epoch'),
    task_revision: asJsonInteger(row.task_revision, 'attempt task_revision'),
    plan_revision: asJsonInteger(row.plan_revision, 'attempt plan_revision'),
    deadline_ms: asJsonInteger(row.deadline_ms, 'attempt deadline_ms'),
    effect_class: boundedText(row, 'effect_class'),
    source: boundedText(row, 'source'),
    refunded: asJsonInteger(row.refunded, 'refunded'),
    created_ms: asJsonInteger(row.created_ms, 'attempt created_ms'),
    intent_id: boundedText(row, 'intent_id', { nullable: true }),
  }));
}

function selectAttemptMap(db, rootId, budget) {
  const sql = `SELECT ${[
    boundedTextExpression('decision_id', 'decision_id'),
    boundedTextExpression('attempt_id', 'attempt_id'),
  ].join(', ')}, generation
    FROM attempt_map WHERE decision_id IN (SELECT decision_id FROM decisions WHERE root_id = ?1)
    ORDER BY decision_id LIMIT ?3`;
  return queryRows(db, sql, rootId, budget).map(row => ({
    decision_id: boundedText(row, 'decision_id'),
    generation: asJsonInteger(row.generation, 'attempt_map generation'),
    attempt_id: boundedText(row, 'attempt_id'),
  }));
}

function selectAdmissionOutbox(db, rootId, budget) {
  const sql = `SELECT ${boundedTextExpression('attempt_id', 'attempt_id')}, delivered
    FROM admission_outbox WHERE attempt_id IN (SELECT attempt_id FROM attempts WHERE root_id = ?1)
    ORDER BY attempt_id LIMIT ?3`;
  return queryRows(db, sql, rootId, budget).map(row => ({
    attempt_id: boundedText(row, 'attempt_id'),
    delivered: asJsonInteger(row.delivered, 'outbox delivered'),
  }));
}

function selectEffectResults(db, rootId, budget) {
  const sql = `SELECT ${[
    boundedTextExpression('result_id', 'result_id'),
    boundedTextExpression('json', 'json'),
  ].join(', ')}, revision
    FROM effect_results WHERE root_id = ?1 ORDER BY result_id, revision LIMIT ?3`;
  return queryRows(db, sql, rootId, budget, { maxFieldBytes: MAX_EFFECT_JSON_BYTES }).map(row => {
    const resultId = boundedText(row, 'result_id');
    const revision = asJsonInteger(row.revision, 'effect revision');
    const projection = projectEffectResult(
      boundedText(row, 'json', { maxBytes: MAX_EFFECT_JSON_BYTES }),
      rootId,
      resultId,
      revision,
    );
    return {
      root_id: rootId,
      result_id: resultId,
      revision,
      ...projection,
      json_included: false,
    };
  });
}

function projectEffectResult(json, rootId, resultId, revision) {
  const unknown = {
    effect_status: null,
    attempt_id: null,
    intent_id: null,
    observed_ms: null,
    causality: null,
    evidence_count: null,
    source_sequence: null,
    source_sequence_status: 'not_stored_in_current_schema',
    projection_status: 'invalid_json',
  };
  let value;
  try { value = JSON.parse(json); } catch { return unknown; }
  const expectedKeys = [
    'attempt_id', 'causality', 'confirmation_scope', 'evidence_group', 'input_span', 'intent_id',
    'observed_ms', 'predicate', 'result_id', 'revision', 'root_task_id', 'status', 'target',
  ].sort();
  if (!value || Array.isArray(value) || typeof value !== 'object'
    || JSON.stringify(Object.keys(value).sort()) !== JSON.stringify(expectedKeys)) {
    return { ...unknown, projection_status: 'invalid_shape' };
  }
  const validOptionalId = item => item === null || (typeof item === 'string'
    && Buffer.byteLength(item, 'utf8') <= MAX_FIELD_BYTES && !/[\u0000-\u001f\u007f]/u.test(item));
  const observedMs = Number.isSafeInteger(value.observed_ms) && value.observed_ms >= 0
    ? value.observed_ms : null;
  const valid = value.root_task_id === rootId
    && value.result_id === resultId
    && value.revision === revision
    && validOptionalId(value.attempt_id)
    && validOptionalId(value.intent_id)
    && ['satisfied', 'unsatisfied', 'unknown'].includes(value.status)
    && ['confirmed', 'unknown', 'not_applicable'].includes(value.causality)
    && observedMs !== null
    && Array.isArray(value.evidence_group)
    && value.evidence_group.length <= MAX_ALLOWED_ROWS
    && value.evidence_group.every(item => typeof item === 'string'
      && Buffer.byteLength(item, 'utf8') <= MAX_FIELD_BYTES
      && !/[\u0000-\u001f\u007f]/u.test(item))
    && typeof value.predicate === 'string'
    && Buffer.byteLength(value.predicate, 'utf8') <= MAX_FIELD_BYTES
    && ['current_ui', 'ui_revisit', 'commit'].includes(value.confirmation_scope)
    && (value.target === null || typeof value.target === 'string')
    && (value.input_span === null || (value.input_span !== null
      && typeof value.input_span === 'object' && !Array.isArray(value.input_span)
      && Object.keys(value.input_span).sort().join(',') === 'from_ms,to_ms'
      && Number.isSafeInteger(value.input_span.from_ms) && value.input_span.from_ms >= 0
      && Number.isSafeInteger(value.input_span.to_ms) && value.input_span.to_ms >= value.input_span.from_ms));
  if (!valid) return { ...unknown, projection_status: 'invalid_shape' };
  return {
    effect_status: value.status,
    attempt_id: value.attempt_id,
    intent_id: value.intent_id,
    observed_ms: observedMs,
    causality: value.causality,
    evidence_count: value.evidence_group.length,
    source_sequence: null,
    source_sequence_status: 'not_stored_in_current_schema',
    projection_status: 'typed_effect_result',
  };
}

function parseUsage(usageJson) {
  const unknown = { input_tokens: null, output_tokens: null, cost_micros: null };
  let decoded;
  try {
    decoded = JSON.parse(usageJson);
  } catch {
    return { status: 'invalid_json', ...unknown };
  }
  if (decoded === null || typeof decoded !== 'object' || Array.isArray(decoded)
    || Object.keys(decoded).sort().join(',') !== [...USAGE_KEYS].sort().join(',')) {
    return { status: 'invalid_shape', ...unknown };
  }

  const projected = {};
  let unrepresentable = false;
  for (const key of USAGE_KEYS) {
    const token = new RegExp(`"${key}"\\s*:\\s*(null|(?:0|[1-9][0-9]*))(?=\\s*[,}])`, 'u').exec(usageJson)?.[1];
    if (decoded[key] === null && token === 'null') {
      projected[key] = null;
      continue;
    }
    if (typeof decoded[key] !== 'number' || !Number.isInteger(decoded[key]) || token === undefined) {
      return { status: 'invalid_shape', ...unknown };
    }
    const exact = BigInt(token);
    if (exact > U64_MAX) return { status: 'invalid_integer', ...unknown };
    if (exact > MAX_SAFE_BIGINT) {
      projected[key] = exact.toString();
      unrepresentable = true;
    } else {
      projected[key] = Number(exact);
    }
  }
  const present = USAGE_KEYS.filter(key => projected[key] !== null).length;
  return {
    status: unrepresentable ? 'exact_decimal_string' : present === 0 ? 'unknown' : present === USAGE_KEYS.length ? 'recorded' : 'partial',
    ...projected,
  };
}

function selectCallRows(db, table, rootId, budget) {
  const request = table === 'model_calls';
  const identityName = request ? 'request_id' : 'decision_id';
  const reasonExpression = request
    ? boundedTextExpression('reason', 'reason')
    : 'NULL AS reason, NULL AS reason__bytes';
  const sql = `SELECT ${[
    boundedTextExpression(identityName, identityName),
    boundedTextExpression('provider', 'provider'),
    boundedTextExpression('model', 'model'),
    boundedTextExpression('outcome', 'outcome'),
    boundedTextExpression('usage_json', 'usage_json'),
    reasonExpression,
  ].join(', ')}, generation, started_ms, finished_ms
    FROM ${table} WHERE root_id = ?1 ORDER BY started_ms, generation, ${identityName} LIMIT ?3`;
  return queryRows(db, sql, rootId, budget).map(row => ({
    scope: request ? 's2_request' : 's1_decision',
    root_id: rootId,
    request_id: request ? boundedText(row, 'request_id') : null,
    decision_id: request ? null : boundedText(row, 'decision_id'),
    generation: asJsonInteger(row.generation, 'call generation'),
    provider: boundedText(row, 'provider'),
    model: boundedText(row, 'model'),
    reason: request ? boundedText(row, 'reason', { nullable: true }) : null,
    provider_response_id: null,
    provider_response_id_status: 'not_stored_in_current_schema',
    started_ms: asJsonInteger(row.started_ms, 'call started_ms'),
    finished_ms: row.finished_ms === null ? null : asJsonInteger(row.finished_ms, 'call finished_ms'),
    outcome: boundedText(row, 'outcome', { nullable: true }),
    usage: parseUsage(boundedText(row, 'usage_json')),
    cost_usd: null,
    cost_usd_status: 'not_stored_in_current_schema',
    actual_network_requests: null,
    actual_network_requests_status: 'not_stored_in_current_schema',
  }));
}

function selectComputerCallRows(db, rootId, budget) {
  const sql = `SELECT ${[
    boundedTextExpression('request_id', 'request_id'),
    boundedTextExpression('snapshot_id', 'snapshot_id'),
    boundedTextExpression('subgoal_id', 'subgoal_id'),
    boundedTextExpression('stage', 'stage'),
    boundedTextExpression('provider', 'provider'),
    boundedTextExpression('model', 'model'),
    boundedTextExpression('outcome', 'outcome'),
    boundedTextExpression('usage_json', 'usage_json'),
  ].join(', ')}, generation, task_revision, root_epoch, structure_sequence,
    deadline_ms, plan_revision, created_ms, started_ms, finished_ms
    FROM computer_model_calls WHERE root_id = ?1
    ORDER BY started_ms, generation, request_id LIMIT ?3`;
  return queryRows(db, sql, rootId, budget).map(row => ({
    scope: 'computer_request',
    root_id: rootId,
    request_id: boundedText(row, 'request_id'),
    generation: asJsonInteger(row.generation, 'computer call generation'),
    task_revision: asJsonInteger(row.task_revision, 'computer task_revision'),
    root_epoch: asJsonInteger(row.root_epoch, 'computer root_epoch'),
    snapshot_id: boundedText(row, 'snapshot_id'),
    structure_sequence: asJsonInteger(row.structure_sequence, 'computer structure_sequence'),
    deadline_ms: asJsonInteger(row.deadline_ms, 'computer deadline_ms'),
    plan_revision: asJsonInteger(row.plan_revision, 'computer plan_revision'),
    subgoal_id: boundedText(row, 'subgoal_id'),
    created_ms: asJsonInteger(row.created_ms, 'computer created_ms'),
    stage: boundedText(row, 'stage'),
    provider: boundedText(row, 'provider'),
    model: boundedText(row, 'model'),
    started_ms: asJsonInteger(row.started_ms, 'computer started_ms'),
    finished_ms: row.finished_ms === null ? null : asJsonInteger(row.finished_ms, 'computer finished_ms'),
    outcome: boundedText(row, 'outcome', { nullable: true }),
    usage: parseUsage(boundedText(row, 'usage_json')),
    provider_response_id: null,
    provider_response_id_status: 'not_stored_in_current_schema',
    actual_network_requests: null,
    actual_network_requests_status: 'not_stored_in_current_schema',
  }));
}

function checkReferences({ decisions, attempts, attemptMap, outbox, effectResults, planInstallations, callRows, schema }) {
  const attemptsById = new Map(attempts.map(attempt => [attempt.attempt_id, attempt]));
  const decisionsById = new Map(decisions.map(decision => [decision.decision_id, decision]));
  const mapByDecision = new Map(attemptMap.map(row => [row.decision_id, row]));
  const outboxByAttempt = new Map(outbox.map(row => [row.attempt_id, row]));
  const planByRevision = new Map(planInstallations.map(row => [row.plan_revision, row]));
  const modelCallsByIdentity = new Map(callRows
    .filter(call => call.scope === 's2_request')
    .map(call => [`${call.request_id}\u0000${call.generation}`, call]));

  const planInstallationChecks = planInstallations.map(installation => {
    if (installation.state === 'proposed') {
      return {
        plan_revision: installation.plan_revision,
        state: installation.state,
        attribution_status: installation.attribution_status,
        request_reference_status: 'not_installed',
      };
    }
    if (installation.attribution_status === 'unknown') {
      return {
        plan_revision: installation.plan_revision,
        state: installation.state,
        attribution_status: installation.attribution_status,
        request_reference_status: 'unknown',
      };
    }
    const call = modelCallsByIdentity.get(`${installation.request_id}\u0000${installation.generation}`);
    const requestReferenceStatus = call === undefined
      ? 'not_found_within_selected_root'
      : call.reason === installation.reason && call.outcome === 'proposal' && call.finished_ms !== null
        ? 'matched'
        : 'identity_or_outcome_mismatch';
    return {
      plan_revision: installation.plan_revision,
      state: installation.state,
      attribution_status: installation.attribution_status,
      request_id: installation.request_id,
      generation: installation.generation,
      reason: installation.reason,
      request_reference_status: requestReferenceStatus,
    };
  });

  const decisionChecks = decisions.map(decision => {
    const consumed = decision.consumed_attempt === null ? null : attemptsById.get(decision.consumed_attempt);
    const map = mapByDecision.get(decision.decision_id) ?? null;
    const mappedAttempt = map === null ? null : attemptsById.get(map.attempt_id);
    const installation = decision.plan_revision === null
      ? null
      : planByRevision.get(decision.plan_revision) ?? null;
    let s2RequestReferenceStatus = decision.plan_revision === null
      ? 'missing_legacy_plan_revision'
      : installation === null
        ? 'plan_installation_not_found_within_selected_root'
        : installation.state !== 'installed'
          ? 'plan_not_installed'
          : installation.attribution_status === 'unknown'
            ? 'unknown'
            : planInstallationChecks.find(row => row.plan_revision === installation.plan_revision)
              ?.request_reference_status ?? 'unknown';
    const linkedRequest = installation?.attribution_status === 'model_request'
      && s2RequestReferenceStatus === 'matched'
      ? { request_id: installation.request_id, generation: installation.generation, reason: installation.reason }
      : null;
    return {
      decision_id: decision.decision_id,
      source: decision.source,
      consumed_attempt: decision.consumed_attempt,
      plan_revision: decision.plan_revision,
      plan_installation_reference_status: decision.plan_revision === null
        ? 'missing_legacy_plan_revision'
        : installation === null
          ? 'not_found_within_selected_root'
          : installation.state === 'installed' ? 'matched' : 'not_installed',
      s2_request_reference_status: s2RequestReferenceStatus,
      s2_request: linkedRequest,
      consumed_attempt_status: decision.consumed_attempt === null
        ? 'absent'
        : consumed === undefined
          ? 'not_found_within_selected_root'
          : consumed.decision_id === decision.decision_id && consumed.generation === decision.generation
            ? 'matched'
            : 'identity_mismatch',
      attempt_map_status: map === null
        ? 'absent'
        : map.generation !== decision.generation
          ? 'generation_mismatch'
          : mappedAttempt === undefined
            ? 'attempt_not_found_within_selected_root'
            : mappedAttempt.decision_id === decision.decision_id && mappedAttempt.generation === map.generation
              ? 'matched'
              : 'attempt_identity_mismatch',
    };
  });

  const attemptChecks = attempts.map(attempt => {
    const decision = decisionsById.get(attempt.decision_id);
    const outboxRow = outboxByAttempt.get(attempt.attempt_id);
    return {
      attempt_id: attempt.attempt_id,
      decision_reference_status: decision === undefined
        ? 'not_found_within_selected_root'
        : decision.generation === attempt.generation
          ? 'matched'
          : 'generation_mismatch',
      admission_outbox_status: outboxRow === undefined ? 'absent' : 'matched',
      admission_outbox_delivered: outboxRow?.delivered ?? null,
    };
  });

  const decisionCalls = callRows
    .filter(call => call.scope === 's1_decision')
    .map(call => {
      const decision = decisionsById.get(call.decision_id);
      call.decision_source = decision?.source ?? null;
      call.decision_reference_status = decision === undefined
        ? 'not_found_within_selected_root'
        : decision.generation === call.generation
          ? 'matched'
          : 'generation_mismatch';
      return {
        decision_id: call.decision_id,
        generation: call.generation,
        source: call.decision_source,
        status: call.decision_reference_status,
      };
    });
  for (const call of callRows.filter(row => row.scope === 's2_request')) {
    const links = planInstallations.filter(installation => installation.state === 'installed'
      && installation.attribution_status === 'model_request'
      && installation.request_id === call.request_id
      && installation.generation === call.generation);
    call.plan_installation_reference_status = links.length === 0 ? 'not_linked_in_selected_root' : 'matched';
    call.plan_installation_revisions = links.map(installation => installation.plan_revision);
  }

  const installedPlans = planInstallations.filter(row => row.state === 'installed');
  const linkedPlanCount = planInstallationChecks.filter(row => row.state === 'installed'
    && row.attribution_status === 'model_request' && row.request_reference_status === 'matched').length;
  const unknownPlanCount = installedPlans.filter(row => row.attribution_status === 'unknown').length;
  const unlinkedPlanCount = installedPlans.length - linkedPlanCount - unknownPlanCount;
  const effectChecks = effectResults.map(effect => {
    if (effect.projection_status !== 'typed_effect_result') {
      return { result_id: effect.result_id, revision: effect.revision, attempt_reference_status: effect.projection_status };
    }
    if (effect.attempt_id === null) {
      return { result_id: effect.result_id, revision: effect.revision, attempt_reference_status: 'no_attempt_reference' };
    }
    const attempt = attemptsById.get(effect.attempt_id);
    const attemptReferenceStatus = attempt === undefined
      ? 'not_found_within_selected_root'
      : attempt.intent_id === effect.intent_id ? 'matched' : 'intent_mismatch';
    return {
      result_id: effect.result_id,
      revision: effect.revision,
      attempt_id: effect.attempt_id,
      attempt_reference_status: attemptReferenceStatus,
    };
  });
  const matchedEffects = effectChecks.filter(row => row.attempt_reference_status === 'matched').length;
  return {
    decisions: decisionChecks,
    attempts: attemptChecks,
    decision_model_calls: decisionCalls,
    plan_installations: planInstallationChecks,
    s2_plan_request: {
      status: installedPlans.length === 0
        ? 'no_installed_plan_rows'
        : unlinkedPlanCount === 0 && unknownPlanCount === 0 ? 'exact_links_present' : 'partial_or_unknown',
      installed_plan_count: installedPlans.length,
      matched_model_request_count: linkedPlanCount,
      unknown_attribution_count: unknownPlanCount,
      unmatched_model_request_count: unlinkedPlanCount,
      proposed_not_installed_count: planInstallations.length - installedPlans.length,
      time_based_matching_used: false,
    },
    local_inference_revisions: {
      status: schema.optional_tables.local_inference_revisions.present ? 'present_not_exported' : 'missing_in_current_schema',
    },
    effect_results_to_attempts: {
      status: effectResults.length === 0 ? 'no_effect_results'
        : matchedEffects === effectResults.length ? 'exact_attempt_links_present' : 'partial_or_unknown',
      rows: effectResults.length,
      matched_attempt_rows: matchedEffects,
      source_sequence_status: 'not_stored_in_current_schema',
      separate_verdict_status: 'not_stored_as_a_separate_record',
    },
    effect_results: effectChecks,
    provider_response_ids: { status: 'not_stored_in_current_schema', value: null },
    actual_network_requests: { status: 'not_stored_in_current_schema', value: null },
  };
}

function encodedBytes(value) {
  return Buffer.byteLength(JSON.stringify(value), 'utf8');
}

function finalizeByteLimit(exported, maxBytes) {
  exported.limits.encoded_bytes = 0;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const bytes = encodedBytes(exported);
    if (bytes > maxBytes) throw error('BYTE_LIMIT', 'Encoded Host ledger evidence exceeds maxBytes');
    if (exported.limits.encoded_bytes === bytes) return exported;
    exported.limits.encoded_bytes = bytes;
  }
  if (encodedBytes(exported) > maxBytes) throw error('BYTE_LIMIT', 'Encoded Host ledger evidence exceeds maxBytes');
  return exported;
}

/**
 * Export bounded, root-scoped Host ledger facts from an existing SQLite file.
 * Payload-bearing columns and event_log details are deliberately not selected.
 */
export function exportHostLedgerEvidence({ dbPath, rootId, maxRows, maxBytes } = {}) {
  const limits = validateLimits({ maxRows, maxBytes });
  const selectedRootId = validateRootId(rootId);
  if (typeof DatabaseSync.prototype.enableDefensive !== 'function') {
    throw error('UNSUPPORTED_SQLITE_RUNTIME', 'This Node.js runtime does not support defensive SQLite mode');
  }
  const databaseIdentity = validateDatabasePath(dbPath);
  let db;
  let transactionOpen = false;
  try {
    db = new DatabaseSync(databaseIdentity.path, { readOnly: true, timeout: 2_000 });
    assertDatabaseIdentity(databaseIdentity);
    db.enableDefensive(true);
    db.exec('PRAGMA query_only = ON');
    const queryOnly = db.prepare('PRAGMA query_only').get().query_only === 1;
    if (!queryOnly) throw error('READ_ONLY_REQUIRED', 'SQLite query_only could not be enabled');
    db.exec('BEGIN DEFERRED TRANSACTION');
    transactionOpen = true;

    const schema = inspectSchema(db);
    const budget = { maxRows: limits.maxRows, rows: 0 };
    const root = selectedRoot(db, selectedRootId, budget);
    const decisions = selectDecisions(db, selectedRootId, budget);
    const planInstallations = selectPlanInstallations(db, selectedRootId, budget);
    const attempts = selectAttempts(db, selectedRootId, budget);
    const attemptMap = selectAttemptMap(db, selectedRootId, budget);
    const admissionOutbox = selectAdmissionOutbox(db, selectedRootId, budget);
    const effectResults = selectEffectResults(db, selectedRootId, budget);
    const callRows = [
      ...selectCallRows(db, 'model_calls', selectedRootId, budget),
      ...selectCallRows(db, 'decision_model_calls', selectedRootId, budget),
    ];
    const computerCallRows = selectComputerCallRows(db, selectedRootId, budget);
    const crossReferences = checkReferences({
      decisions, attempts, attemptMap, outbox: admissionOutbox, effectResults,
      planInstallations, callRows, schema,
    });
    const effectReferenceByKey = new Map(crossReferences.effect_results.map(row => [
      `${row.result_id}\u0000${row.revision}`,
      row.attempt_reference_status,
    ]));

    const exported = {
      version: 1,
      kind: 'host_ledger_evidence',
      scope: {
        root_id: selectedRootId,
        root_filter: 'exact_root_id',
        other_root_rows_exported: false,
      },
      assessment: {
        ledger_facts_only: true,
        mvp_complete: false,
        model_quality_passed: null,
        business_quality_passed: null,
      },
      database: {
        file_path_disclosed: false,
        opened_read_only: true,
        query_only: true,
        transaction: 'read_only_snapshot',
      },
      schema,
      root,
      decisions,
      plan_installations: planInstallations,
      attempts,
      attempt_map: attemptMap,
      admission_outbox: admissionOutbox.map(row => ({
        attempt_id: row.attempt_id,
        delivered: row.delivered,
        payload_included: false,
      })),
      effect_results: effectResults.map(row => ({
        ...row,
        attempt_reference_status: effectReferenceByKey.get(`${row.result_id}\u0000${row.revision}`) ?? 'unknown',
        json_included: false,
      })),
      call_rows: callRows,
      computer_call_rows: computerCallRows,
      cross_references: crossReferences,
      excluded_content: [
        'root_tasks.user_text', 'root_tasks.draft_json', 'root_tasks.goal_json', 'root_tasks.plan_json',
        'root_tasks.failure', 'admission_outbox.payload', 'raw effect_results.json payload', 'event_log.detail',
        'provider request/response payloads', 'credentials and keys',
      ],
      limits: {
        max_rows: limits.maxRows,
        rows_read: budget.rows,
        max_bytes: limits.maxBytes,
        max_field_bytes: MAX_FIELD_BYTES,
        max_effect_json_bytes: MAX_EFFECT_JSON_BYTES,
        integer_encoding: 'safe_integer_number_or_exact_decimal_string',
        encoded_bytes: 0,
      },
    };
    assertDatabaseIdentity(databaseIdentity);
    db.exec('COMMIT');
    transactionOpen = false;
    assertDatabaseIdentity(databaseIdentity);
    return finalizeByteLimit(exported, limits.maxBytes);
  } catch (cause) {
    if (db && transactionOpen) {
      try { db.exec('ROLLBACK'); } catch { /* Preserve the original failure. */ }
    }
    if (cause instanceof HostLedgerEvidenceError) throw cause;
    throw error('DATABASE_READ_FAILED', 'Host ledger evidence could not be read safely');
  } finally {
    db?.close();
  }
}

function validateNewOutputPath(outputPath) {
  validateAbsoluteNormalizedPath(outputPath, 'INVALID_OUTPUT_PATH', 'outputPath');
  const outputName = basename(outputPath);
  const parentPath = dirname(outputPath);
  if (!outputName || outputName === '.' || outputName === '..') {
    throw error('INVALID_OUTPUT_PATH', 'outputPath must name a new file in a private directory');
  }
  const parent = privateDirectoryIdentity(parentPath, 'INVALID_OUTPUT_PATH', 'output parent');
  try {
    lstatSync(outputPath, { bigint: true });
    throw error('OUTPUT_EXISTS', 'outputPath already exists');
  } catch (cause) {
    if (cause instanceof HostLedgerEvidenceError) throw cause;
    if (cause?.code !== 'ENOENT') throw error('INVALID_OUTPUT_PATH', 'outputPath cannot be inspected safely');
  }
  return { path: outputPath, parentPath, parent };
}

function assertOutputParent(expected) {
  const current = privateDirectoryIdentity(expected.parentPath, 'OUTPUT_IDENTITY_CHANGED', 'output parent');
  if (!sameIdentity(expected.parent, current, false)) {
    throw error('OUTPUT_IDENTITY_CHANGED', 'Output directory identity changed during export');
  }
}

function identityFromStat(stat) {
  return { dev: stat.dev, ino: stat.ino, uid: stat.uid, nlink: stat.nlink, mode: stat.mode & 0o777n };
}

function assertOutputFileIdentity(output, expected) {
  const pathIdentity = privateFileIdentity(output.path, 'OUTPUT_IDENTITY_CHANGED', 'output file');
  if (!sameIdentity(expected, pathIdentity) || pathIdentity.mode !== 0o600n) {
    throw error('OUTPUT_IDENTITY_CHANGED', 'Output file identity or permissions changed during export');
  }
  assertOutputParent(output);
}

function removePartialOutput(outputPath, createdIdentity) {
  if (!createdIdentity) return;
  try {
    const current = lstatSync(outputPath, { bigint: true });
    if (current.isFile() && !current.isSymbolicLink() && sameIdentity(createdIdentity, identityFromStat(current))) {
      unlinkSync(outputPath);
    }
  } catch { /* Preserve the original failure and never remove a replacement file. */ }
}

/** Write one new mode-0600 evidence file without replacing an existing path. */
export function writeHostLedgerEvidenceFile({ outputPath, ...request } = {}) {
  const output = validateNewOutputPath(outputPath);
  const evidence = exportHostLedgerEvidence(request);
  const contents = `${JSON.stringify(evidence)}\n`;
  const byteLength = Buffer.byteLength(contents, 'utf8');
  if (byteLength > evidence.limits.max_bytes) throw error('BYTE_LIMIT', 'Encoded Host ledger evidence exceeds maxBytes');

  assertOutputParent(output);
  let fd;
  let createdIdentity;
  try {
    try {
      fd = openSync(output.path, 'wx', 0o600);
    } catch (cause) {
      if (cause?.code === 'EEXIST') throw error('OUTPUT_EXISTS', 'outputPath already exists');
      throw error('OUTPUT_WRITE_FAILED', 'Could not create the new private output file');
    }
    fchmodSync(fd, 0o600);
    const opened = fstatSync(fd, { bigint: true });
    createdIdentity = identityFromStat(opened);
    if (!opened.isFile() || opened.uid !== currentUid() || opened.nlink !== 1n
      || (opened.mode & 0o777n) !== 0o600n) {
      throw error('OUTPUT_WRITE_FAILED', 'New output file did not meet private file requirements');
    }
    assertOutputFileIdentity(output, createdIdentity);

    writeFileSync(fd, contents, 'utf8');
    fsyncSync(fd);
    const afterWrite = fstatSync(fd, { bigint: true });
    if (!sameIdentity(createdIdentity, identityFromStat(afterWrite))) {
      throw error('OUTPUT_IDENTITY_CHANGED', 'Output file identity changed while writing');
    }
    closeSync(fd);
    fd = undefined;
    assertOutputFileIdentity(output, createdIdentity);
    return { root_id: evidence.scope.root_id, bytes: byteLength };
  } catch (cause) {
    if (fd !== undefined) {
      try { closeSync(fd); } catch { /* Preserve the original failure. */ }
      fd = undefined;
    }
    removePartialOutput(output.path, createdIdentity);
    if (cause instanceof HostLedgerEvidenceError) throw cause;
    throw error('OUTPUT_WRITE_FAILED', 'Host ledger evidence could not be written safely');
  }
}

function parseCli(argv) {
  const options = {};
  const names = new Map([
    ['--db', 'dbPath'],
    ['--root', 'rootId'],
    ['--output', 'outputPath'],
    ['--max-rows', 'maxRows'],
    ['--max-bytes', 'maxBytes'],
  ]);
  for (let index = 0; index < argv.length; index += 1) {
    const key = names.get(argv[index]);
    if (!key || options[key] !== undefined || argv[index + 1] === undefined) {
      throw error('CLI_USAGE', 'Usage: node host-ledger-evidence.mjs --db /absolute/file.sqlite --root ROOT --output /absolute/new-file.json [--max-rows N] [--max-bytes N]');
    }
    const value = argv[++index];
    options[key] = key === 'maxRows' || key === 'maxBytes' ? Number(value) : value;
  }
  if (!options.dbPath || !options.rootId || !options.outputPath) {
    throw error('CLI_USAGE', 'Usage: node host-ledger-evidence.mjs --db /absolute/file.sqlite --root ROOT --output /absolute/new-file.json [--max-rows N] [--max-bytes N]');
  }
  return options;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    writeHostLedgerEvidenceFile(parseCli(process.argv.slice(2)));
  } catch (cause) {
    const code = cause?.code ?? 'HOST_LEDGER_EXPORT_FAILED';
    process.stderr.write(`${code}: ${cause?.message ?? 'Host ledger export failed'}\n`);
    process.exitCode = 1;
  }
}
