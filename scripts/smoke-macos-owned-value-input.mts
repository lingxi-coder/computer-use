// Explicit local pipeline diagnostic, restricted to a freshly owned document.
// A CurrentUi setter effect cannot satisfy the root's independent UiRevisit.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DecisionWorker, ScriptedTeacher } from '@computer-use/decision-worker';
import { DecisionRelay } from '../apps/desktop/src/main/decisionRelay.js';
import { Supervisor } from '../apps/desktop/src/main/supervisor.js';
import { authorizeOwnedDocumentValueInput } from '../apps/desktop/src/main/ownedDocumentInput.js';
import { auditOwnedInputJournal, readOwnedInputJournalFile } from './mvp/owned-input-journal.mjs';
import { activateOwnedDocumentForObservation, createOwnedDocumentLaunchRunner,
  encodeOwnedDocumentSourceBootstrap, type OwnedDocumentRegistration } from '../apps/desktop/src/main/ownedDocumentLaunch.js';

assert.deepEqual(process.argv.slice(2), ['--execute'], 'this diagnostic requires explicit --execute');
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const artifact = join(root, 'artifacts', `m3-owned-value-input-${randomUUID()}.json`);
const initialText = 'Owned input diagnostic\n初始自有正文\n';
const expectedText = 'Owned setter verified\n中文 👩🏽‍💻\tCafé\n';
const runner = createOwnedDocumentLaunchRunner();
const hash = (path: string) => createHash('sha256').update(readFileSync(path)).digest('hex');
const binaries = { host: join(root, 'target/debug/cu-host'), device: join(root, 'target/debug/cu-device'),
  source: join(root, 'target/debug/cu-source') };
const paths = { ...binaries, helper: join(root, 'target/debug/cu-macos-source'),
  guard: join(root, 'target/debug/cu-macos-device-guard'), launcher: join(root, 'target/debug/cu-macos-owned-document-launcher') };
const binaryHashes = Object.fromEntries(Object.entries(paths).map(([role, path]) => [role, hash(path)]));
const sourceFiles = execFileSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '--',
  'crates', 'native', 'apps/desktop/src', 'packages', 'Cargo.lock', 'contracts/desktop-v2/schema.json',
  'scripts/smoke-macos-owned-value-input.mts', 'scripts/mvp/owned-input-journal.mts'],
  { cwd: root, encoding: 'utf8' }).split('\0').filter(Boolean).sort();
const treeHash = () => {
  const digest = createHash('sha256');
  for (const file of sourceFiles) digest.update(file + '\0').update(readFileSync(join(root, file))).update('\0');
  return digest.digest('hex');
};
const initialTreeHash = treeHash();
const evidence: Record<string, unknown> = { kind: 'owned_value_input_pipeline_diagnostic', schema_version: 1,
  started_at: new Date().toISOString(), ok: false, input_grant: true,
  binary_sha256: binaryHashes, source_tree_sha256: initialTreeHash,
  script_sha256: hash(fileURLToPath(import.meta.url)), native_editing_verified: false,
  root_confirmation_scope: 'ui_revisit', independent_revisit_verified: false, save_reopen_verified: false,
  live_models_verified: false, business_suite_verified: false, mvp_complete: false };
let supervisor: Supervisor | null = null;
let relay: DecisionRelay | null = null;
let ownedRegistration: OwnedDocumentRegistration | null = null;
let db: string | null = null;
const scopes: Array<{ source_sequence: number; binding_current: boolean }> = [];
const exitedRoles: string[] = [];
let unexpectedRootCompletion = false;
let phase = 'launch';
const deviceDiagnostics = { owned_enqueue_failed: false, native_refused: false };
const nativeRejections: Array<{ outcome: string; reason: string }> = [];
const sql = (query: string): Array<Record<string, unknown>> => JSON.parse(execFileSync('/usr/bin/sqlite3',
  ['-json', db!, query], { encoding: 'utf8', timeout: 2_000 }).trim() || '[]');
async function until<T>(test: () => T | null | undefined | false, failure: string, timeout = 15_000): Promise<T> {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    assert.equal(exitedRoles.length, 0, 'a product peer exited');
    const value = test(); if (value) return value;
    await new Promise(done => setTimeout(done, 30));
  }
  throw new Error(failure);
}
try {
  const registration = await runner.launch(initialText);
  ownedRegistration = registration;
  assert.equal(await activateOwnedDocumentForObservation(registration), true);
  const prefix = encodeOwnedDocumentSourceBootstrap(registration);
  const claims = JSON.parse(prefix.subarray(4).toString('utf8'));
  prefix.fill(0);
  supervisor = await Supervisor.start({ binaries, readyTimeoutMs: 12_000, ownedDocument: registration,
    ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(registration),
    ownedDocumentReferencePlanner: true,
    nativeSource: { helperPath: paths.helper, guardHelperPath: paths.guard, bundleId: 'com.apple.TextEdit', pid: claims.pid } });
  db = join(supervisor.dir, 'host.db');
  phase = 'observe';
  supervisor.on('exit', role => exitedRoles.push(role));
  supervisor.on('projection', projection => {
    if (projection.root?.state === 'completed') unexpectedRootCompletion = true;
  });
  supervisor.on('log', line => {
    if (line.startsWith('device: ')) {
      deviceDiagnostics.owned_enqueue_failed ||= line.includes('owned value submission could not enqueue:');
      deviceDiagnostics.native_refused ||= line.includes('native submission refused:');
      const match = /^device: native submission refused: ([a-z_]+): (.+)$/.exec(line);
      const outcomes = ['rejected', 'stop_latched', 'expired', 'revoked', 'context_unresolved', 'unknown'];
      const reasons = ['owned_scope_unavailable', 'owned_channel_closed', 'owned_permissions_unavailable',
        'owned_lease_expired', 'owned_guard_revision_changed', 'owned_dependency_revision_changed',
        'owned_dependency_not_valid', ...outcomes.map(outcome => `owned_authority_${outcome}`),
        ...['initial', 'process', 'window_inventory', 'focus', 'window', 'tree', 'receiver', 'frame', 'marked', 'revalidation', 'final', 'verified',
          'focus_timeout', 'focus_application', 'focus_application_match', 'focus_window', 'focus_system_receiver',
          'focus_app_receiver', 'focus_receiver_match', 'focus_windows',
          'focus_application_late', 'focus_application_error', 'focus_application_type',
          'focus_application_cannot_complete', 'focus_application_no_value', 'focus_application_api_disabled']
          .concat(['window_membership', 'window_attributes', 'window_document', 'window_sheets', 'window_frame', 'window_match'])
          .map(stage => `owned_receiver_${stage}`)];
      const messages = new Map([
        ['independent owned receiver verification failed', 'owned_receiver_verification_failed'],
        ['owned document receiver is unavailable', 'owned_receiver_unavailable'],
        ['owned value deadline expired before AX verification', 'owned_preflight_expired'],
        ['owned value deadline expired before setter', 'owned_setter_expired'],
        ['AX setter timeout could not be established', 'owned_setter_timeout_unavailable'],
        ['owned input guard, lease or dependency changed before setter', 'owned_commit_guard_changed'],
        ['owned request was revoked or already consumed', 'owned_commit_revoked'],
        ['owned input journal could not durably prepare this request', 'owned_prepare_failed'],
      ]);
      const reason = match && (reasons.includes(match[2]!) ? match[2]! : messages.get(match[2]!));
      if (match && outcomes.includes(match[1]!) && reason && nativeRejections.length < 8) {
        nativeRejections.push({ outcome: match[1]!, reason });
      }
    }
    if (!line.startsWith('host: ')) return;
    const start = line.indexOf('{'); if (start < 0) return;
    try {
      const value = JSON.parse(line.slice(start));
      if (value.kind === 'owned_document_readonly_scope' && Number.isSafeInteger(value.source_sequence)) {
        scopes.push({ source_sequence: value.source_sequence, binding_current: value.binding_current === true });
      }
    } catch { /* Other process diagnostics are not acceptance evidence. */ }
  });
  relay = new DecisionRelay(supervisor, new DecisionWorker(new ScriptedTeacher())); relay.attach();
  await until(() => new Set(scopes.filter(s => s.binding_current).map(s => s.source_sequence)).size >= 3,
    'three private owned source reads were not accepted');
  await until(() => supervisor!.projection?.capabilities?.input === 'available', 'owned input capability unavailable');
  assert.equal(supervisor.projection?.mode, 'observe');
  const publicProjection = JSON.stringify(supervisor.projection);
  for (const value of [claims.registration_id, claims.document_path, claims.document_sha256]) {
    assert.equal(publicProjection.includes(value), false, 'private registration entered the public projection');
  }
  supervisor.operator({ command: 'create_task', text: `Fill owned document: ${expectedText}` });
  phase = 'goal_confirmation';
  const confirmation = await until(() => supervisor!.projection?.pending_confirmation, 'goal confirmation missing');
  assert.equal(confirmation.kind.kind, 'goal');
  supervisor.operator({ command: 'confirm_goal', confirmation_id: confirmation.confirmation_id, approved: true });
  await until(() => supervisor!.projection?.root?.state === 'ready', 'frozen root is not ready');
  supervisor.operator({ command: 'set_mode', mode: 'execute' });
  phase = 'input_and_effect';
  const effect = await until(() => {
    if (nativeRejections.length > 0) throw new Error('native owned admission rejected');
    const row = sql("SELECT json FROM effect_results WHERE json_extract(json,'$.predicate')='text.value@1' ORDER BY revision DESC LIMIT 1;")[0];
    if (!row) return null;
    const value = JSON.parse(String(row.json));
    if (value.status === 'unknown' || value.status === 'violated') throw new Error('owned CurrentUi effect did not succeed');
    return value.status === 'satisfied' ? value : null;
  }, 'input was not verified by new owned UI evidence', 20_000);
  assert.equal(effect.confirmation_scope, 'current_ui');
  assert.ok(effect.input_span && effect.input_span.to_ms >= effect.input_span.from_ms);
  assert.ok(effect.observed_ms > effect.input_span.to_ms);
  assert.ok(effect.attempt_id && effect.intent_id && effect.evidence_group.length >= 1);
  // Give later engine ticks/source reads a chance to expose accidental root
  // completion or input replay before auditing the single durable input pair.
  await new Promise(done => setTimeout(done, 750));
  assert.deepEqual(exitedRoles, [], 'a product peer exited after input verification');
  assert.equal(unexpectedRootCompletion, false, 'CurrentUi was incorrectly promoted to root completion');
  assert.notEqual(supervisor.projection?.root?.state, 'completed', 'CurrentUi cannot satisfy UiRevisit root');
  const counts = sql('SELECT (SELECT COUNT(*) FROM attempts) AS attempts,(SELECT COUNT(*) FROM attempt_map) AS mappings,'
    + '(SELECT COUNT(*) FROM admission_outbox WHERE delivered=1) AS activated_outbox,'
    + '(SELECT COUNT(*) FROM decision_model_calls) AS s1_calls,(SELECT COUNT(*) FROM model_calls) AS s2_calls;')[0]!;
  assert.equal(counts.attempts, 1);
  assert.equal(counts.mappings, 1);
  assert.equal(counts.activated_outbox, 1);
  assert.equal(counts.s1_calls, 0); assert.equal(counts.s2_calls, 0);
  phase = 'journal_audit';
  evidence.input_journal = auditOwnedInputJournal(readOwnedInputJournalFile(join(supervisor.dir, 'owned-input-journal.jsonl')), {
    action: 'ax_set_owned_document_value', root: effect.root_task_id, attempt: effect.attempt_id, intent: effect.intent_id,
    session: supervisor.projection!.capabilities!.identity.session_epoch,
    registration: claims.registration_id,
    inputStart: effect.input_span.from_ms, inputEnd: effect.input_span.to_ms,
  });
  evidence.native_editing_verified = true;
  evidence.current_ui_effect = { status: effect.status, confirmation_scope: effect.confirmation_scope,
    causality: effect.causality, input_span: effect.input_span, observed_ms: effect.observed_ms,
    evidence_count: effect.evidence_group.length, attempt_and_intent_present: true };
  evidence.root_state_before_stop = supervisor.projection?.root?.state;
  evidence.ledger_counts = counts;
  evidence.expected_utf8_byte_count = Buffer.byteLength(expectedText, 'utf8');
  evidence.private_registration_not_projected = true;
  assert.deepEqual(exitedRoles, [], 'a product peer exited before final acceptance');
  evidence.ok = true;
} catch (error) {
  // Assertion and process errors may carry private registration IDs or paths.
  evidence.failure = 'owned input pipeline diagnostic failed';
  evidence.failure_phase = phase;
  evidence.failure_type = error instanceof Error ? error.name : 'unknown';
  process.exitCode = 1;
} finally {
  // Capture bounded metadata before shutdown removes the session database.
  // No raw receipt, registration, body, path or error message is retained.
  evidence.device_diagnostics = deviceDiagnostics;
  evidence.native_rejections = nativeRejections;
  evidence.source_reads = { total: scopes.length, current: scopes.filter(s => s.binding_current).length,
    unique_current: new Set(scopes.filter(s => s.binding_current).map(s => s.source_sequence)).size,
    latest_current: scopes.at(-1)?.binding_current ?? false };
  evidence.last_projection = { root_state: supervisor?.projection?.root?.state ?? null,
    mode: supervisor?.projection?.mode ?? null, input_capability: supervisor?.projection?.capabilities?.input ?? null };
  if (db && existsSync(db)) {
    try {
      evidence.diagnostic_counts = sql('SELECT (SELECT COUNT(*) FROM attempts) AS attempts,'
        + '(SELECT COUNT(*) FROM attempts WHERE intent_id IS NOT NULL) AS attempts_with_intent,'
        + '(SELECT COUNT(*) FROM attempt_map) AS mappings,'
        + '(SELECT COUNT(*) FROM admission_outbox WHERE delivered=1) AS activated_outbox,'
        + '(SELECT COUNT(*) FROM effect_results) AS effects;')[0];
      evidence.attempt_states = sql('SELECT state,COUNT(*) AS count FROM attempts GROUP BY state;');
      evidence.effect_diagnostics = sql('SELECT json FROM effect_results ORDER BY revision;').slice(-8).map(row => {
        const value = JSON.parse(String(row.json));
        return { predicate: ['text.value@1', 'goal@1'].includes(value.predicate) ? value.predicate : 'redacted',
          status: ['satisfied', 'violated', 'unknown'].includes(value.status) ? value.status : 'redacted',
          confirmation_scope: ['current_ui', 'ui_revisit', 'commit'].includes(value.confirmation_scope) ? value.confirmation_scope : 'redacted',
          input_span_present: value.input_span != null, evidence_count: Array.isArray(value.evidence_group) ? value.evidence_group.length : null };
      });
      evidence.engine_event_counts = sql('SELECT kind,COUNT(*) AS count FROM event_log GROUP BY kind;')
        .map(row => ({ kind: typeof row.kind === 'string' && /^[a-z_]{1,64}$/.test(row.kind) ? row.kind : 'redacted', count: row.count }));
    } catch { evidence.database_diagnostics_unavailable = true; }
  }
  if (supervisor) {
    try {
      const bytes = readOwnedInputJournalFile(join(supervisor.dir, 'owned-input-journal.jsonl'));
      if (bytes.length <= 256 * 1024) {
        evidence.journal_diagnostics = bytes.toString('utf8').trim().split('\n').filter(Boolean).slice(0, 8)
          .map(line => {
            const payload = JSON.parse(line).payload;
            return { kind: ['prepared', 'terminal'].includes(payload.kind) ? payload.kind : 'redacted',
              outcome: ['applied', 'rejected', 'unknown', 'expired', 'revoked', 'stop_latched', 'context_unresolved'].includes(payload.outcome) ? payload.outcome : null,
              submitted: typeof payload.submitted === 'boolean' ? payload.submitted : null };
          });
      } else evidence.journal_diagnostics_oversized = true;
    } catch { evidence.journal_diagnostics_unavailable = true; }
  }
  try { supervisor?.operator({ command: 'stop_task', root_task_id: null }); }
  catch { evidence.stop_command_sent = false; evidence.ok = false; process.exitCode = 1; }
  try { relay?.workerLost(); }
  catch { evidence.relay_cleanup_failed = true; evidence.ok = false; process.exitCode = 1; }
  try { await supervisor?.shutdown(); }
  catch { evidence.supervisor_cleanup_failed = true; evidence.ok = false; process.exitCode = 1; }
  evidence.false_root_completion = unexpectedRootCompletion;
  if (unexpectedRootCompletion) { evidence.ok = false; process.exitCode = 1; }
  try {
    const diagnosticStop = ownedRegistration ? [await ownedRegistration.stopDiagnostic(expectedText)] : [];
    evidence.owned_cleanup = [...diagnosticStop, ...await runner.stopAll()];
  }
  catch { evidence.owned_cleanup = ['unknown']; }
  if (!(evidence.owned_cleanup as string[]).every(result => result === 'stopped')) { evidence.ok = false; process.exitCode = 1; }
  evidence.exited_roles = exitedRoles;
  if (exitedRoles.length > 0) { evidence.ok = false; process.exitCode = 1; }
  try {
    evidence.binary_hashes_unchanged = Object.entries(paths).every(([role, path]) => hash(path) === binaryHashes[role]);
    evidence.source_hashes_unchanged = treeHash() === initialTreeHash;
  } catch { evidence.binary_hashes_unchanged = false; evidence.source_hashes_unchanged = false; }
  if (!evidence.binary_hashes_unchanged || !evidence.source_hashes_unchanged) { evidence.ok = false; process.exitCode = 1; }
  evidence.finished_at = new Date().toISOString();
  writeFileSync(artifact, `${JSON.stringify(evidence, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  console.log(JSON.stringify({ ok: evidence.ok, artifact, editing_verified: evidence.native_editing_verified,
    cleanup: evidence.owned_cleanup, mvp_complete: false }));
}
