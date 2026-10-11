const path = require('node:path');

const MAX_SCOPE_BYTES = 8 * 1024;
const MAX_SNAPSHOT_BYTES = 2 * 1024 * 1024;
const MAX_SUBMIT_BYTES = 64 * 1024;
const MAX_REPLY_BYTES = 4 * 1024;
const MAX_NODES = 2048;
const MAX_EDGES = 8192;
const MAX_CHILDREN = 2048;
const MAX_SOURCE_READS = 128;
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/u;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const FIXTURE_TARGET = 'browser.fixture.background_option';
const FIXTURE_PREDICATE = 'browser.checkbox_state@1';
const crypto = require('node:crypto');

function record(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function exactKeys(value, expected, label) {
  if (!record(value) || Object.keys(value).sort().join(',') !== expected.slice().sort().join(',')) {
    throw new Error(`${label} has an invalid object shape`);
  }
}

function bytes(value, maximum, label) {
  let encoded;
  try { encoded = Buffer.from(JSON.stringify(value), 'utf8'); }
  catch { throw new Error(`${label} is not JSON encodable`); }
  if (encoded.length > maximum) throw new Error(`${label} exceeds its byte bound`);
  return encoded.length;
}

function safeInteger(value, positive = false) {
  return Number.isSafeInteger(value) && (positive ? value > 0 : value >= 0);
}

function boundedText(value, maximum, allowBlank = false) {
  return typeof value === 'string' && Buffer.byteLength(value, 'utf8') <= maximum
    && (allowBlank || value.trim().length > 0) && !CONTROL.test(value);
}

function validateTargetContext(context) {
  exactKeys(context, [
    'browser_session_id', 'browser_context_id', 'page_target_id', 'cdp_session_id',
    'frame_id', 'document_loader_id', 'fixture_file_url', 'session_epoch',
  ], 'browser target context');
  for (const key of ['browser_session_id', 'browser_context_id', 'page_target_id', 'cdp_session_id', 'frame_id', 'document_loader_id']) {
    if (!boundedText(context[key], 256)) throw new Error(`browser context ${key} is invalid`);
  }
  if (!safeInteger(context.session_epoch, true) || !boundedText(context.fixture_file_url, 4096)) {
    throw new Error('browser context session or fixture URL is invalid');
  }
  let fixtureUrl;
  try { fixtureUrl = new URL(context.fixture_file_url); }
  catch { throw new Error('browser context fixture URL is invalid'); }
  if (fixtureUrl.protocol !== 'file:' || fixtureUrl.search || fixtureUrl.hash
    || !fixtureUrl.pathname.endsWith('/fixtures/headless/readonly-page.html')
    || context.fixture_file_url.includes('\\') || context.fixture_file_url.includes(' ')) {
    throw new Error('browser context does not identify the fixed local fixture');
  }
  return context;
}

function validateScope(scope) {
  exactKeys(scope, ['scope_id', 'generation', 'context', 'clock_domain', 'issued_ms'], 'browser read scope');
  if (typeof scope.scope_id !== 'string' || !UUID.test(scope.scope_id)
    || !safeInteger(scope.generation, true) || !safeInteger(scope.issued_ms)
    || !boundedText(scope.clock_domain, 128)) throw new Error('browser read scope identity is invalid');
  validateTargetContext(scope.context);
  bytes(scope, MAX_SCOPE_BYTES, 'browser read scope');
  return scope;
}

function validateProjection(projection) {
  if (!record(projection) || !record(projection.capabilities)
    || projection.capabilities.backend !== 'headless_browser'
    || projection.capabilities.execution_domain !== 'headless_browser') {
    throw new Error('projection does not identify the fixture browser execution domain');
  }
  const domain = projection.capabilities.domains?.find((entry) => entry.domain === 'headless_browser');
  if (!domain || domain.state !== 'available' || !domain.actions.includes('headless_browser_action')) {
    throw new Error('projection does not advertise the bounded browser action');
  }
  const observation = projection.browser_observation;
  if (!record(observation)) throw new Error('projection lacks a current browser observation');
  exactKeys(observation, [
    'scope', 'snapshot_id', 'revision', 'source_sequence', 'observed_ms', 'read_finished_ms',
    'target_context_hash', 'node_count', 'readonly_binding_ref', 'readonly_checked',
  ], 'browser observation projection');
  validateScope(observation.scope);
  if (!boundedText(observation.snapshot_id, 256)
    || !safeInteger(observation.revision, true)
    || !safeInteger(observation.source_sequence, true)
    || !safeInteger(observation.observed_ms)
    || !safeInteger(observation.read_finished_ms)
    || observation.read_finished_ms < observation.observed_ms
    || observation.observed_ms < observation.scope.issued_ms
    || !/^tc1:[0-9a-f]{64}$/.test(observation.target_context_hash)
    || !safeInteger(observation.node_count, true) || observation.node_count > MAX_NODES
    || !(observation.readonly_checked === null || typeof observation.readonly_checked === 'boolean')
    || !(observation.readonly_binding_ref === null || boundedText(observation.readonly_binding_ref, 256))) {
    throw new Error('browser observation projection is malformed or stale');
  }
  if ((observation.readonly_checked === null) !== (observation.readonly_binding_ref === null)) {
    throw new Error('browser observation does not preserve unknown binding state');
  }
  bytes(observation, MAX_SCOPE_BYTES, 'browser observation projection');
  return observation;
}

function hasCompleteOwnerPids(pids) {
  return record(pids) && Object.keys(pids).sort().join(',') === 'device,host,source'
    && Object.values(pids).every((pid) => safeInteger(pid, true) && pid <= 0x7fffffff);
}

function validateOwnerRecord(value, realpathSync, requireCompletePids = false) {
  exactKeys(value, ['supervisorDir', 'profileParent', 'pids'], 'browser owner diagnostic');
  if (!boundedText(value.supervisorDir, 4096) || !path.isAbsolute(value.supervisorDir)
    || !boundedText(value.profileParent, 4096) || !path.isAbsolute(value.profileParent)
    || !record(value.pids)) throw new Error('browser owner paths or process IDs are invalid');
  const roles = ['host', 'device', 'source'];
  if (Object.keys(value.pids).some((role) => !roles.includes(role))) {
    throw new Error('browser owner process list has an unknown role');
  }
  if (requireCompletePids && !hasCompleteOwnerPids(value.pids)) {
    throw new Error('browser owner process list is incomplete at finalization');
  }
  for (const pid of Object.values(value.pids)) {
    if (pid !== undefined && pid !== null && (!safeInteger(pid, true) || pid > 0x7fffffff)) {
      throw new Error('browser owner PID is invalid');
    }
  }
  let canonicalSupervisor;
  try { canonicalSupervisor = realpathSync(value.supervisorDir); }
  catch { throw new Error('browser supervisor directory is unavailable'); }
  if (value.profileParent !== path.join(canonicalSupervisor, 'headless-profiles')) {
    throw new Error('browser profile parent is outside the Main-owned supervisor directory');
  }
  bytes(value, 16 * 1024, 'browser owner diagnostic');
  return value;
}

function validateSnapshot(snapshot) {
  exactKeys(snapshot, [
    'snapshot_id', 'source_sequence', 'observed_ms', 'read_finished_ms', 'clock_domain',
    'context', 'target_context_hash', 'root_ax_node_id', 'nodes',
  ], 'browser page snapshot');
  validateTargetContext(snapshot.context);
  if (!boundedText(snapshot.snapshot_id, 256) || !safeInteger(snapshot.source_sequence, true)
    || !safeInteger(snapshot.observed_ms) || !safeInteger(snapshot.read_finished_ms)
    || snapshot.read_finished_ms < snapshot.observed_ms || !boundedText(snapshot.clock_domain, 128)
    || !/^tc1:[0-9a-f]{64}$/.test(snapshot.target_context_hash)
    || !boundedText(snapshot.root_ax_node_id, 256)
    || !Array.isArray(snapshot.nodes) || snapshot.nodes.length === 0 || snapshot.nodes.length > MAX_NODES) {
    throw new Error('browser page snapshot is malformed');
  }
  let edges = 0;
  const ids = new Set();
  for (const node of snapshot.nodes) {
    if (!record(node) || !boundedText(node.ax_node_id, 256) || ids.has(node.ax_node_id)
      || !Array.isArray(node.child_ax_node_ids) || node.child_ax_node_ids.length > MAX_CHILDREN
      || !['unknown', 'false', 'true'].includes(node.checked)
      || !['unknown', 'false', 'true'].includes(node.disabled)) {
      throw new Error('browser page node is malformed');
    }
    for (const forbidden of ['value', 'text', 'action', 'bounds', 'frame', 'native_window']) {
      if (Object.hasOwn(node, forbidden)) throw new Error('browser page snapshot includes an unsupported field');
    }
    ids.add(node.ax_node_id);
    edges += node.child_ax_node_ids.length;
    if (edges > MAX_EDGES) throw new Error('browser page snapshot exceeds its edge bound');
  }
  bytes(snapshot, MAX_SNAPSHOT_BYTES, 'browser page snapshot');
  return snapshot;
}

function validateExecution(value) {
  exactKeys(value, ['submission', 'reply'], 'browser execution diagnostic');
  const { submission, reply } = value;
  bytes(submission, MAX_SUBMIT_BYTES, 'browser submission');
  bytes(reply, MAX_REPLY_BYTES, 'browser reply');
  exactKeys(reply, [
    'version', 'request_id', 'channel_generation', 'session_epoch', 'scope_id', 'scope_generation',
    'sequence', 'intent_id', 'outcome', 'submitted_at_ms', 'input_end_ms', 'failure_code',
  ], 'browser reply');
  const intent = submission?.intent;
  const action = intent?.action;
  const evidence = intent?.evidence;
  const authority = submission?.authority;
  if (!record(submission) || !record(intent) || !record(action) || !record(evidence)
    || action.type !== 'set_checkbox' || action.checked !== true
    || !record(evidence.binding) || evidence.binding.role !== 'checkbox'
    || evidence.binding.name !== 'Background option' || evidence.binding.disabled !== 'false'
    || evidence.binding.checked !== false || evidence.freshness !== 'verified'
    || !record(authority) || !record(submission.authority_evidence)) {
    throw new Error('browser submission does not target the observed enabled fixture checkbox');
  }
  validateScope(evidence.scope);
  validateScope(submission.authority_evidence.scope);
  const expectedContext = evidence.scope.context;
  if (submission.authority_evidence.scope.scope_id !== evidence.scope.scope_id
    || submission.authority_evidence.scope.generation !== evidence.scope.generation
    || submission.authority_evidence.binding.context.page_target_id !== expectedContext.page_target_id
    || evidence.binding.context.page_target_id !== expectedContext.page_target_id
    || reply.request_id !== submission.request_id
    || reply.channel_generation !== submission.channel_generation
    || reply.session_epoch !== submission.session_epoch
    || reply.scope_id !== submission.scope_id
    || reply.scope_generation !== submission.scope_generation
    || reply.sequence !== submission.sequence
    || reply.intent_id !== intent.intent_id
    || reply.outcome !== 'completed' || reply.failure_code !== null
    || !safeInteger(reply.submitted_at_ms) || !safeInteger(reply.input_end_ms)
    || reply.input_end_ms < reply.submitted_at_ms
    || reply.submitted_at_ms < authority.command_start_ms
    || reply.input_end_ms > authority.command_end_ms
    || reply.input_end_ms > authority.reply_deadline_ms) {
    throw new Error('browser reply is incomplete, out of bounds, or mismatched');
  }
  return value;
}

function sameScope(left, right) {
  return left.scope_id === right.scope_id && left.generation === right.generation
    && left.clock_domain === right.clock_domain
    && JSON.stringify(left.context) === JSON.stringify(right.context);
}

function contextIdentity(context) {
  return {
    browser_session_id: context.browser_session_id,
    browser_context_id: context.browser_context_id,
    page_target_id: context.page_target_id,
    cdp_session_id: context.cdp_session_id,
    frame_id: context.frame_id,
    document_loader_id: context.document_loader_id,
    fixture_file_url_suffix: '/fixtures/headless/readonly-page.html',
    fixture_file_url_sha256: require('node:crypto').createHash('sha256').update(context.fixture_file_url).digest('hex'),
    session_epoch: context.session_epoch,
  };
}

function isFixtureGoal(root) {
  const goal = root.goal;
  return record(goal)
    && goal.execution_domain === 'headless_browser'
    && goal.browser_page_profile?.kind === 'fixture_background_option'
    && goal.confirmation_scope === 'current_ui'
    && goal.target?.semantic_target === FIXTURE_TARGET
    && goal.target?.label === 'Background option'
    && goal.target?.role_hint === 'checkbox'
    && goal.scope?.app === 'browser.fixture'
    && isFixtureControlScope(goal.allowed_scope)
    && isFixtureControlScope(root.admitted_allowed_scope)
    && root.admitted_allowed_scope_app === goal.scope.app;
}

function isFixtureControlScope(scope) {
  if (!record(scope) || scope.application !== 'Browser fixture' || scope.no_unrelated_controls !== true
    || Object.keys(scope).sort().join(',') !== 'application,no_unrelated_controls,targets'
    || !Array.isArray(scope.targets) || scope.targets.length !== 1) return false;
  const target = scope.targets[0];
  return record(target) && target.control === 'Background option'
    && target.group === null && target.disambiguator === null
    && Object.keys(target).sort().join(',') === 'control,disambiguator,group';
}

function readForEffect(effect, reads, scope, targetContextHash, afterMs, afterSequence, axNodeId, backendDomNodeId) {
  if (!Array.isArray(effect.evidence_group) || effect.evidence_group.length !== 1) return null;
  const read = reads.find((item) => item.snapshot_id === effect.evidence_group[0]);
  if (!read || read.clock_domain !== scope.clock_domain
    || JSON.stringify(read.context_identity) !== JSON.stringify(contextIdentity(scope.context))
    || read.target_context_hash !== targetContextHash
    || read.observed_ms <= afterMs || read.source_sequence <= afterSequence
    || read.read_finished_ms < read.observed_ms || read.observed_ms < scope.issued_ms
    || effect.observed_ms !== read.observed_ms
    || !record(read.checkbox) || read.checkbox.role !== 'checkbox'
    || read.checkbox.name !== 'Background option' || read.checkbox.checked !== 'true'
    || read.checkbox.disabled !== 'false' || read.checkbox.ignored !== false
    || read.checkbox.ax_node_id !== axNodeId || read.checkbox.backend_dom_node_id !== backendDomNodeId) return null;
  return read;
}

function validateLedger(ledger, firstRootId, secondRootId, executions, sourceReads) {
  if (!record(ledger) || !Array.isArray(ledger.roots) || ledger.roots.length !== 2
    || !Array.isArray(ledger.attempts) || ledger.attempts.length > 16
    || !Array.isArray(ledger.effects) || ledger.effects.length > 16
    || !Array.isArray(sourceReads) || sourceReads.length > MAX_SOURCE_READS
    || ledger.model_calls !== 0 || ledger.decision_model_calls !== 0) {
    throw new Error('read-only Host ledger is incomplete, unbounded, or contains model calls');
  }
  const first = ledger.roots.find((root) => root.root_id === firstRootId);
  const second = ledger.roots.find((root) => root.root_id === secondRootId);
  if (!first || !second || first.state !== 'completed' || second.state !== 'completed'
    || first.user_text !== 'Enable browser fixture checkbox'
    || second.user_text !== 'Enable browser fixture checkbox'
    || !isFixtureGoal(first) || !isFixtureGoal(second)) {
    throw new Error('Host ledger does not contain both completed fixed fixture goals');
  }
  const firstEffects = ledger.effects.filter((effect) => effect.root_id === firstRootId);
  const secondEffects = ledger.effects.filter((effect) => effect.root_id === secondRootId);
  const completedExecution = executions.find((entry) => entry.reply.outcome === 'completed');
  if (!completedExecution || executions.length !== 1 || firstEffects.length !== 1 || secondEffects.length !== 1) {
    throw new Error('Host ledger lacks one actual completed input and two satisfied effects');
  }
  const [firstEffect] = firstEffects;
  const [secondEffect] = secondEffects;
  const intent = completedExecution.submission.intent;
  const evidence = intent.evidence;
  const firstRead = readForEffect(firstEffect, sourceReads, evidence.scope,
    evidence.binding.target_context_hash, completedExecution.reply.input_end_ms,
    evidence.binding.source_sequence, evidence.binding.ax_node_id, evidence.binding.backend_dom_node_id);
  const secondRead = firstRead && readForEffect(secondEffect, sourceReads, evidence.scope,
    evidence.binding.target_context_hash, Math.max(completedExecution.reply.input_end_ms, firstRead.observed_ms),
    firstRead.source_sequence, firstRead.checkbox.ax_node_id, firstRead.checkbox.backend_dom_node_id);
  const exactEffect = (effect) => effect.predicate === FIXTURE_PREDICATE
    && effect.target === FIXTURE_TARGET && effect.status === 'satisfied'
    && effect.confirmation_scope === 'current_ui';
  if (!exactEffect(firstEffect) || firstEffect.causality !== 'unknown'
    || firstEffect.intent_id !== intent.intent_id || firstEffect.attempt_id !== intent.attempt_id
    || firstEffect.input_span?.from_ms !== completedExecution.reply.submitted_at_ms
    || firstEffect.input_span?.to_ms !== completedExecution.reply.input_end_ms
    || !firstRead) {
    throw new Error('input effect is not linked to its exact current-UI predicate and fresh checked-on source read');
  }
  if (!exactEffect(secondEffect) || secondEffect.causality !== 'not_applicable'
    || secondEffect.intent_id !== null || secondEffect.input_span !== null
    || !secondRead) {
    throw new Error('no-input effect is not linked to the exact fresh same-scope checkbox read');
  }
  const firstAttempts = ledger.attempts.filter((attempt) => attempt.root_id === firstRootId);
  const secondAttempts = ledger.attempts.filter((attempt) => attempt.root_id === secondRootId);
  const firstAttempt = firstAttempts[0];
  const secondAttempt = secondAttempts[0];
  if (firstAttempts.length !== 1 || secondAttempts.length !== 1
    || firstAttempt.attempt_id !== intent.attempt_id || firstAttempt.intent_id !== intent.intent_id
    || firstAttempt.state !== 'settled' || !safeInteger(firstAttempt.activation_seq, true) || firstAttempt.refunded !== 0
    || !safeInteger(firstAttempt.created_ms)
    || secondAttempt.attempt_id !== secondEffect.attempt_id || secondAttempt.intent_id !== null
    || secondAttempt.state !== 'settled' || !safeInteger(secondAttempt.activation_seq, true)
    || secondAttempt.refunded !== 1 || !safeInteger(secondAttempt.created_ms)
    || secondAttempt.created_ms <= firstEffect.observed_ms || secondRead.observed_ms <= secondAttempt.created_ms) {
    throw new Error('attempt ledger does not match the submitted action and durable no-input admission');
  }
  bytes(ledger, 64 * 1024, 'Host ledger summary');
  return ledger;
}

module.exports = {
  MAX_SCOPE_BYTES,
  MAX_SNAPSHOT_BYTES,
  MAX_SUBMIT_BYTES,
  MAX_REPLY_BYTES,
  bytes,
  validateScope,
  validateProjection,
  validateOwnerRecord,
  validateSnapshot,
  validateExecution,
  sameScope,
  contextIdentity,
  hasCompleteOwnerPids,
  validateLedger,
};
