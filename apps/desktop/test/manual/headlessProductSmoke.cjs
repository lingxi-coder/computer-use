const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const { execFileSync } = require('node:child_process');
const path = require('node:path');
const { app } = require('electron');

const proofRules = require('./headlessProductSmokeProof.cjs');
const root = process.env.CU_SMOKE_PROJECT_ROOT;
const output = process.env.CU_SMOKE_OUTPUT_DIR;
const resultPath = output && path.join(output, 'result.json');
const ownershipPath = output && path.join(output, 'ownership.json');
const TASK_TEXT = 'Enable browser fixture checkbox';
const TASK_CONTROL_SCOPE = {
  application: 'Browser fixture',
  targets: [{ control: 'Background option', group: null, disambiguator: null }],
  no_unrelated_controls: true,
};
const MAX_OWNER_RECORDS = 4;
const MAX_SNAPSHOT_RECORDS = 128;
const MAX_EXECUTION_RECORDS = 8;
const result = {
  ok: false,
  checks: {},
  failure_step: null,
  browser_execution_count: 0,
  roots: [],
  browser_scopes: [],
  browser_reads: [],
  browser_executions: [],
  user_facing_dom: [],
  display_evidence: [],
  bounds: {
    browser_scope_json_bytes: proofRules.MAX_SCOPE_BYTES,
    browser_snapshot_json_bytes: proofRules.MAX_SNAPSHOT_BYTES,
    browser_submit_json_bytes: proofRules.MAX_SUBMIT_BYTES,
    browser_reply_json_bytes: proofRules.MAX_REPLY_BYTES,
    max_owner_records: MAX_OWNER_RECORDS,
    max_snapshot_records: MAX_SNAPSHOT_RECORDS,
    max_execution_records: MAX_EXECUTION_RECORDS,
  },
  ledger: null,
  last_projection: null,
  cleanup: null,
  renderer_errors: [],
};

if (!root || !output || !resultPath || !ownershipPath) {
  throw new Error('Run through scripts/smoke-headless-product.mts with its private output directory.');
}
if (process.versions.electron !== process.env.CU_SMOKE_EXPECTED_ELECTRON) {
  throw new Error('Electron version does not match the pinned runtime.');
}

app.setName('Computer Use Headless Product Smoke');
app.setPath('userData', path.join(output, 'user-data'));
process.env.CU_BIN_DIR = path.join(root, 'target/debug');
delete process.env.CU_TARGET_PID;
delete process.env.CU_TARGET_BUNDLE_ID;
delete process.env.CU_NATIVE_SOURCE_HELPER;
delete process.env.CU_NATIVE_REFERENCE_PLANNER;
for (const name of Object.keys(process.env)) {
  if (/(?:API.?KEY|ACCESS.?TOKEN|(?:OPENAI|ANTHROPIC)_|MODEL.*(?:KEY|TOKEN))/i.test(name)) delete process.env[name];
}

const ownerRecords = [];
const sourceSnapshots = [];
const executionRecords = [];
let window = null;
let activeStep = 'startup';
let finishing = false;
let finishRequested = false;
let ledgerCaptureError = null;
let browserPid = null;
let browserProfilePath = null;
let ledgerRoots = [];
let shutdownFallback;

fs.writeFileSync(ownershipPath, '[]\n', { flag: 'wx', mode: 0o600 });

function safeError(error) {
  const message = error instanceof Error ? error.message : String(error);
  return message.replace(/(?:file:\/\/)?\/(?:Users|private|var|tmp|Volumes)\/[^\s"']+/g, '[path]')
    .replace(/[0-9a-f]{64}/g, '[credential]')
    .slice(0, 512);
}

function mark(name, condition, detail) {
  assert.ok(condition, name);
  result.checks[name] = true;
  if (detail !== undefined) result.checks[`${name}_detail`] = detail;
}

function writeOwnership() {
  fs.writeFileSync(ownershipPath, JSON.stringify(ownerRecords, null, 2) + '\n', { mode: 0o600 });
}

function summaryScope(scope) {
  proofRules.validateScope(scope);
  return {
    scope_id: scope.scope_id,
    generation: scope.generation,
    clock_domain: scope.clock_domain,
    issued_ms: scope.issued_ms,
    context: {
      browser_session_id: scope.context.browser_session_id,
      browser_context_id: scope.context.browser_context_id,
      page_target_id: scope.context.page_target_id,
      cdp_session_id: scope.context.cdp_session_id,
      frame_id: scope.context.frame_id,
      document_loader_id: scope.context.document_loader_id,
      fixture_file_url_suffix: '/fixtures/headless/readonly-page.html',
      fixture_file_url_sha256: crypto.createHash('sha256').update(scope.context.fixture_file_url).digest('hex'),
      session_epoch: scope.context.session_epoch,
    },
  };
}

function summarizeProjectionObservation(projection) {
  const observation = proofRules.validateProjection(projection);
  return {
    ...summaryScope(observation.scope),
    snapshot_id: observation.snapshot_id,
    revision: observation.revision,
    source_sequence: observation.source_sequence,
    observed_ms: observation.observed_ms,
    read_finished_ms: observation.read_finished_ms,
    target_context_hash: observation.target_context_hash,
    node_count: observation.node_count,
    readonly_binding_ref: observation.readonly_binding_ref,
    readonly_checked: observation.readonly_checked,
  };
}

function summarizeSnapshot(snapshot) {
  proofRules.validateSnapshot(snapshot);
  const checkbox = snapshot.nodes.find((node) => node.role === 'checkbox' && node.name === 'Background option');
  if (!checkbox) throw new Error('real page snapshot has no named fixture checkbox');
  return {
    snapshot_id: snapshot.snapshot_id,
    source_sequence: snapshot.source_sequence,
    observed_ms: snapshot.observed_ms,
    read_finished_ms: snapshot.read_finished_ms,
    clock_domain: snapshot.clock_domain,
    target_context_hash: snapshot.target_context_hash,
    context_identity: proofRules.contextIdentity(snapshot.context),
    page_target_id: snapshot.context.page_target_id,
    document_loader_id: snapshot.context.document_loader_id,
    node_count: snapshot.nodes.length,
    checkbox: {
      ax_node_id: checkbox.ax_node_id,
      backend_dom_node_id: checkbox.backend_dom_node_id ?? null,
      role: checkbox.role,
      name: checkbox.name,
      checked: checkbox.checked,
      disabled: checkbox.disabled,
      ignored: checkbox.ignored,
    },
  };
}

function summarizeExecution(value) {
  proofRules.validateExecution(value);
  const { submission, reply } = value;
  const evidence = submission.intent.evidence;
  const authorityEvidence = submission.authority_evidence;
  return {
    submission: {
      version: submission.version,
      request_id: submission.request_id,
      channel_generation: submission.channel_generation,
      session_epoch: submission.session_epoch,
      scope_id: submission.scope_id,
      scope_generation: submission.scope_generation,
      sequence: submission.sequence,
      intent_id: submission.intent.intent_id,
      attempt_id: submission.intent.attempt_id,
      task_id: submission.intent.task_id,
      task_revision: submission.intent.task_revision,
      root_epoch: submission.intent.root_epoch,
      action: submission.intent.action,
      execution_evidence: {
        observation_id: evidence.observation_id,
        snapshot_id: evidence.binding.snapshot_id,
        source_sequence: evidence.binding.source_sequence,
        observed_ms: evidence.binding.observed_ms,
        read_finished_ms: evidence.binding.read_finished_ms,
        target_context_hash: evidence.binding.target_context_hash,
        page_target_id: evidence.binding.context.page_target_id,
        ax_node_id: evidence.binding.ax_node_id,
        backend_dom_node_id: evidence.binding.backend_dom_node_id,
        role: evidence.binding.role,
        name: evidence.binding.name,
        disabled: evidence.binding.disabled,
        checked: evidence.binding.checked,
        scope: summaryScope(evidence.scope),
      },
      original_authority_evidence: {
        observation_id: authorityEvidence.observation_id,
        snapshot_id: authorityEvidence.binding.snapshot_id,
        source_sequence: authorityEvidence.binding.source_sequence,
        observed_ms: authorityEvidence.binding.observed_ms,
        read_finished_ms: authorityEvidence.binding.read_finished_ms,
        target_context_hash: authorityEvidence.binding.target_context_hash,
        page_target_id: authorityEvidence.binding.context.page_target_id,
        ax_node_id: authorityEvidence.binding.ax_node_id,
        backend_dom_node_id: authorityEvidence.binding.backend_dom_node_id,
        role: authorityEvidence.binding.role,
        name: authorityEvidence.binding.name,
        disabled: authorityEvidence.binding.disabled,
        checked: authorityEvidence.binding.checked,
      },
      authority: {
        command_start_ms: submission.authority.command_start_ms,
        command_end_ms: submission.authority.command_end_ms,
        valid_until_ms: submission.authority.valid_until_ms,
        reply_deadline_ms: submission.authority.reply_deadline_ms,
      },
    },
    reply: {
      version: reply.version,
      request_id: reply.request_id,
      channel_generation: reply.channel_generation,
      session_epoch: reply.session_epoch,
      scope_id: reply.scope_id,
      scope_generation: reply.scope_generation,
      sequence: reply.sequence,
      intent_id: reply.intent_id,
      outcome: reply.outcome,
      submitted_at_ms: reply.submitted_at_ms,
      input_end_ms: reply.input_end_ms,
      failure_code: reply.failure_code,
    },
  };
}

function captureLedger() {
  if (result.ledger) return result.ledger;
  const owner = ownerRecords.at(-1);
  const dbPath = owner && fs.realpathSync(path.join(owner.supervisorDir, 'host.db'));
  if (!dbPath || !fs.existsSync(dbPath)) throw new Error('read-only Host database is missing before product shutdown');
  const python = [
    'import json, sqlite3, sys, urllib.parse',
    'db = sys.argv[1]',
    'root_ids = json.loads(sys.argv[2])',
    'uri = "file:" + urllib.parse.quote(db, safe="/") + "?mode=ro"',
    'conn = sqlite3.connect(uri, uri=True, timeout=3)',
    'conn.execute("PRAGMA query_only=ON")',
    'names = {row[0] for row in conn.execute("SELECT name FROM sqlite_master WHERE type=\'table\'")}',
    'required = {"root_tasks", "attempts", "effect_results", "model_calls", "decision_model_calls"}',
    'assert required.issubset(names), "Host ledger schema is incomplete"',
    'roots = []',
    'for root_id in root_ids:',
    ' row = conn.execute("SELECT root_id,user_text,state,task_revision,root_epoch,goal_json,allowed_scope_json,allowed_scope_app FROM root_tasks WHERE root_id=?", (root_id,)).fetchone()',
    ' if row is None: continue',
    ' goal = json.loads(row[5]) if row[5] else None',
    ' roots.append({"root_id":row[0],"user_text":row[1],"state":row[2],"task_revision":row[3],"root_epoch":row[4],"goal":goal,"admitted_allowed_scope":json.loads(row[6]) if row[6] else None,"admitted_allowed_scope_app":row[7]})',
    'attempts = []',
    'effects = []',
    'for root_id in root_ids:',
    ' for row in conn.execute("SELECT attempt_id,root_id,state,skill,subgoal,deadline_ms,intent_id,activation_seq,refunded,created_ms FROM attempts WHERE root_id=? ORDER BY created_ms", (root_id,)):',
    '  attempts.append({"attempt_id":row[0],"root_id":row[1],"state":row[2],"skill":row[3],"subgoal":row[4],"deadline_ms":row[5],"intent_id":row[6],"activation_seq":row[7],"refunded":row[8],"created_ms":row[9]})',
    ' for row in conn.execute("SELECT root_id,json FROM effect_results WHERE root_id=? ORDER BY revision", (root_id,)):',
    '  value = json.loads(row[1]); effects.append({"root_id":row[0],"root_task_id":value.get("root_task_id"),"result_id":value.get("result_id"),"revision":value.get("revision"),"attempt_id":value.get("attempt_id"),"intent_id":value.get("intent_id"),"predicate":value.get("predicate"),"target":value.get("target"),"status":value.get("status"),"evidence_group":value.get("evidence_group"),"input_span":value.get("input_span"),"observed_ms":value.get("observed_ms"),"confirmation_scope":value.get("confirmation_scope"),"causality":value.get("causality")})',
    'count = lambda table: conn.execute("SELECT COUNT(*) FROM " + table).fetchone()[0]',
    'print(json.dumps({"roots":roots,"attempts":attempts,"effects":effects,"model_calls":count("model_calls"),"decision_model_calls":count("decision_model_calls")}))',
    'conn.close()',
  ].join('\n');
  result.ledger = JSON.parse(execFileSync('python3', ['-c', python, dbPath, JSON.stringify(ledgerRoots)], {
    encoding: 'utf8', timeout: 7000, maxBuffer: 128 * 1024,
  }));
  const node = process.env.CU_SMOKE_NODE_RUNTIME;
  assert(node && path.isAbsolute(node), 'Explicit system Node runtime is required for the read-only Host fact exporter');
  result.host_ledger_evidence = ledgerRoots.map((rootId, index) => {
    const evidencePath = path.join(output, `host-ledger-evidence-${index}.json`);
    execFileSync(node, [path.join(root, 'scripts/mvp/host-ledger-evidence.mjs'),
      '--db', dbPath, '--root', rootId, '--output', evidencePath], {
      encoding: 'utf8', timeout: 7000, maxBuffer: 16 * 1024,
    });
    const bytes = fs.readFileSync(evidencePath);
    const facts = JSON.parse(bytes);
    assert.equal(facts.kind, 'host_ledger_evidence');
    assert.equal(facts.scope.root_id, rootId);
    assert.equal(facts.scope.other_root_rows_exported, false);
    assert.equal(facts.root.root_id, rootId);
    assert(facts.database.opened_read_only && facts.database.query_only);
    assert.equal(facts.call_rows.length, 0, 'Reference diagnostic cannot count as a model call');
    assert.equal(facts.computer_call_rows.length, 0, 'Reference diagnostic cannot contain computer model call rows');
    assert.equal(facts.assessment.mvp_complete, false);
    assert.equal(facts.attempts.length, result.ledger.attempts.filter(attempt => attempt.root_id === rootId).length);
    return { root_id: rootId, path: path.basename(evidencePath), bytes: bytes.length,
      sha256: crypto.createHash('sha256').update(bytes).digest('hex') };
  });
  mark('actual_root_scoped_readonly_host_facts_exported', result.host_ledger_evidence.length === ledgerRoots.length);
  return result.ledger;
}

function pidIsAlive(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) return false;
  try { process.kill(pid, 0); return true; }
  catch (error) { return error && error.code === 'EPERM'; }
}

function writeFinalResult() {
  result.browser_reads = sourceSnapshots.slice();
  try { result.browser_executions = executionRecords.map(summarizeExecution); }
  catch (error) { result.diagnostic_summary_error = safeError(error); }
  result.browser_execution_count = executionRecords.length;
  if (ledgerCaptureError) result.ledger_error = ledgerCaptureError;
  const owner = ownerRecords.at(-1);
  const completeOwner = [...ownerRecords].reverse().find((candidate) => proofRules.hasCompleteOwnerPids(candidate.pids));
  const cleanupOwner = completeOwner ?? owner;
  const rolePids = completeOwner ? Object.values(completeOwner.pids) : [];
  const cleanup = {
    supervisor_dir_removed: !!cleanupOwner && !fs.existsSync(cleanupOwner.supervisorDir),
    profile_parent_removed: !!cleanupOwner && !fs.existsSync(cleanupOwner.profileParent),
    owner_pids_complete: !!completeOwner,
    role_processes_exited: rolePids.length === 3 && rolePids.every((pid) => !pidIsAlive(pid)),
    chrome_process_exited: browserPid !== null && !pidIsAlive(browserPid),
    profile_directory_removed: browserProfilePath !== null && !fs.existsSync(browserProfilePath),
    database_captured_before_cleanup: result.ledger !== null,
  };
  result.cleanup = cleanup;
  result.browser_execution_count = executionRecords.length;
  result.ok = finishRequested && !ledgerCaptureError
    && Object.values(result.checks).every((value) => value === true)
    && Object.values(cleanup).every((value) => value === true)
    && result.renderer_errors.length === 0
    && executionRecords.length === 1;
  if (!result.ok && !result.failure_step) result.failure_step = 'process_or_profile_cleanup';
  try { fs.writeFileSync(resultPath, JSON.stringify(result, null, 2) + '\n', { mode: 0o600 }); }
  catch { /* The wrapper treats a missing final record as a failed smoke. */ }
}

function finish(error, step = activeStep) {
  if (finishing) return;
  finishing = true;
  finishRequested = error === null;
  if (error) {
    result.failure_step = /^[a-z_]{1,80}$/u.test(step) ? step : 'harness_error';
    result.error = safeError(error);
  }
  ledgerRoots = result.roots.map((entry) => entry.root_task_id).filter((id) => typeof id === 'string');
  if (ownerRecords.length && !result.ledger) {
    try { captureLedger(); } catch (captureError) { ledgerCaptureError = safeError(captureError); }
  }
  try { app.quit(); }
  catch { writeFinalResult(); }
  shutdownFallback = setTimeout(() => {
    result.failure_step ||= 'product_shutdown_timeout';
    finishRequested = false;
    app.exit(1);
  }, 12_000);
  shutdownFallback.unref();
}

function listenDiagnostic(name, onValue) {
  process.on(name, (value) => {
    try { onValue(value); }
    catch (error) { ledgerCaptureError = safeError(error); void finish(error, 'diagnostic_validation'); }
  });
}

listenDiagnostic('computer-use:browser-owner', (value) => {
  const owner = proofRules.validateOwnerRecord(value, fs.realpathSync);
  if (ownerRecords.length >= MAX_OWNER_RECORDS) throw new Error('owner diagnostic count exceeds its bound');
  ownerRecords.push(JSON.parse(JSON.stringify(owner)));
  writeOwnership();
});
listenDiagnostic('computer-use:browser-snapshot', (value) => {
  const summary = summarizeSnapshot(value);
  if (sourceSnapshots.length >= MAX_SNAPSHOT_RECORDS) throw new Error('browser read diagnostic count exceeds its bound');
  sourceSnapshots.push(summary);
});
listenDiagnostic('computer-use:browser-execution', (value) => {
  const checked = proofRules.validateExecution(value);
  if (executionRecords.length >= MAX_EXECUTION_RECORDS) throw new Error('browser execution diagnostic count exceeds its bound');
  executionRecords.push(checked);
});

app.on('browser-window-created', (_event, created) => {
  window = created;
  // Main still creates and owns this normal product window. Only its presentation
  // is kept inactive so this smoke never activates the app or another window.
  created.show = () => created.showInactive();
  const contents = created.webContents;
  contents.on('console-message', (details) => {
    if (details.level === 'error' && result.renderer_errors.length < 32) result.renderer_errors.push(String(details.message).slice(0, 256));
  });
  contents.on('preload-error', (_event, preloadPath, error) => {
    if (result.renderer_errors.length < 32) result.renderer_errors.push(`preload error: ${path.basename(preloadPath)} ${safeError(error)}`);
  });
  contents.on('will-navigate', (event) => {
    if (!event.defaultPrevented) result.renderer_errors.push('unexpected renderer navigation');
  });
  contents.once('did-fail-load', (_event, code, description) => {
    if (code !== -3) void finish(new Error(`renderer failed to load: ${description}`), 'renderer_load');
  });
  contents.once('did-finish-load', () => void runProductFlow().catch((error) => finish(error)));
});

function delay(ms) { return new Promise((resolve) => setTimeout(resolve, ms)); }

async function until(read, why, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = await read();
    if (value) return value;
    await delay(100);
  }
  throw new Error(`Timeout waiting for ${why}`);
}

const js = (source) => {
  if (!window || window.isDestroyed()) throw new Error('normal Main renderer window is unavailable');
  return window.webContents.executeJavaScript(source);
};

async function snapshot() {
  return until(async () => {
    const value = await js('window.desktop.snapshot()');
    // A normal session replacement briefly has no projection while startup ACKs arrive.
    if (value === null) return null;
    if (!value || typeof value !== 'object') throw new Error('renderer did not return the public product projection');
    proofRules.bytes(value, 1024 * 1024, 'public product projection');
    result.last_projection = value;
    return value;
  }, 'public product projection during session startup', 30_000);
}

async function domFacts() {
  return js(`(() => {
    const text = (node) => node ? (node.innerText || node.textContent || '').trim().replace(/\\s+/g, ' ').slice(0, 1024) : null;
    const visible = (node) => !!node && node.getClientRects().length > 0 && getComputedStyle(node).visibility !== 'hidden';
    const buttons = [...document.querySelectorAll('button')];
    const taskButton = buttons.find((node) => node.textContent.trim() === 'Start');
    const execute = document.querySelector('.mode select option[value="execute"]');
    const browserPanel = document.querySelector('[aria-label="Fixture browser observation"]');
    const picker = buttons.find((node) => node.textContent.trim() === 'Use fixture browser');
    const prompt = document.querySelector('.prompt');
    const state = document.querySelector('.state');
    const headline = document.querySelector('.headline');
    return {
      title: document.title,
      productVisible: visible(document.querySelector('.app')),
      device: text(document.querySelector('.device')),
      state: text(state),
      headline: text(headline),
      browserPanel: visible(browserPanel) ? text(browserPanel) : null,
      browserPickerVisible: visible(picker),
      browserPickerEnabled: !!picker && !picker.disabled,
      taskText: document.querySelector('textarea[aria-label="Task"]')?.value ?? null,
      taskScopeEnabled: document.querySelector('input[aria-label="Limit task to one control"]')?.checked ?? false,
      taskScopeApplication: document.querySelector('input[aria-label="Allowed application"]')?.value ?? null,
      taskScopeControl: document.querySelector('input[aria-label="Allowed control"]')?.value ?? null,
      startEnabled: !!taskButton && !taskButton.disabled,
      executeDisabled: !!execute && execute.disabled,
      promptVisible: visible(prompt),
      promptText: visible(prompt) ? text(prompt) : null,
      stopDisabled: !!document.querySelector('button.stop')?.disabled,
      stopBanner: text(document.querySelector('.banner.ok')),
    };
  })()`);
}

function recordDom(label, facts) {
  result.user_facing_dom.push({ step: label, ...facts });
  if (result.user_facing_dom.length > 16) throw new Error('user-facing DOM evidence exceeds its record bound');
}

async function setTaskText(text) {
  await js(`(() => {
    const field = document.querySelector('textarea[aria-label="Task"]');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(field, ${JSON.stringify(text)});
    field.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  const facts = await until(async () => {
    const value = await domFacts();
    return value.startEnabled && value.taskText === text ? value : null;
  }, 'task entry enabled');
  recordDom('task_ready', facts);
  await js(`(() => [...document.querySelectorAll('button')].find((button) => button.textContent.trim() === 'Start').click())()`);
}

async function setTaskControlScope() {
  await js(`(() => {
    const toggle = document.querySelector('input[aria-label="Limit task to one control"]');
    if (!toggle) throw new Error('The normal product control scope entry is missing');
    if (!toggle.checked) toggle.click();
  })()`);
  await until(async () => js(`!!document.querySelector('input[aria-label="Allowed application"]')`), 'normal control scope fields');
  for (const [label, value] of [['Allowed application', TASK_CONTROL_SCOPE.application], ['Allowed control', TASK_CONTROL_SCOPE.targets[0].control], ['Allowed group', '']]) {
    await js(`(() => {
      const field = document.querySelector('input[aria-label=${JSON.stringify(label)}]');
      if (!field) throw new Error('The normal product scope field is missing');
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(field, ${JSON.stringify(value)});
      field.dispatchEvent(new Event('input', { bubbles: true }));
    })()`);
  }
  const facts = await until(async () => {
    const facts = await domFacts();
    return facts.taskScopeEnabled && facts.taskScopeApplication === TASK_CONTROL_SCOPE.application
      && facts.taskScopeControl === TASK_CONTROL_SCOPE.targets[0].control ? facts : null;
  }, 'normal scoped task entry');
  recordDom('scoped_task_fields_ready', facts);
  mark('normal_ui_scoped_task_entry_is_visible', true);
}

async function verifyScopedRequestRejections() {
  const requests = [
    { objective: TASK_TEXT, allowed_scope: { ...TASK_CONTROL_SCOPE, no_unrelated_controls: false } },
    { objective: TASK_TEXT, allowed_scope: { ...TASK_CONTROL_SCOPE, targets: [{ control: 'Background option', group: 'A', disambiguator: 'B' }] } },
    { objective: TASK_TEXT, allowed_scope: TASK_CONTROL_SCOPE, initial_state: { checked: false } },
    { objective: TASK_TEXT, allowed_scope: { ...TASK_CONTROL_SCOPE, output_directory: '/not-an-authorized-file-path' } },
  ];
  const before = await snapshot();
  assert.equal(before.root, null, 'Scope rejection checks require a new session');
  for (const request of requests) {
    const rejected = await js(`(async () => {
      try { await window.desktop.createScopedTask(${JSON.stringify(request)}); return false; }
      catch (error) { return String(error).includes('refused:'); }
    })()`);
    assert.equal(rejected, true, 'Malformed scope must be refused by the normal Main IPC handler');
    const current = await snapshot();
    assert.equal(current.root, null, 'Rejected scope created a Root');
    assert.equal(current.pending_confirmation, null, 'Rejected scope created a confirmation');
    assert.equal(executionRecords.length, 0, 'Rejected scope submitted input');
  }
  mark('actual_main_rejects_scope_widening_and_extra_payloads', true);
}

async function confirmCurrentGoal(label) {
  const pending = await until(async () => {
    const value = await snapshot();
    const facts = await domFacts();
    return value.pending_confirmation && facts.promptVisible ? { projection: value, facts } : null;
  }, 'real goal confirmation prompt');
  recordDom(`${label}_confirmation`, pending.facts);
  const goal = pending.projection.pending_confirmation.kind;
  assert.equal(goal.kind, 'goal');
  assert.deepEqual(goal.goal.allowed_scope, TASK_CONTROL_SCOPE,
    'Host confirmation must contain the exact operator scope');
  assert(pending.facts.promptText.includes(TASK_CONTROL_SCOPE.application)
    && pending.facts.promptText.includes(TASK_CONTROL_SCOPE.targets[0].control),
    'The normal confirmation must show the Host scope');
  mark(`${label}_confirmed_host_control_scope`, true);
  const confirmationId = pending.projection.pending_confirmation.confirmation_id;
  if (typeof confirmationId !== 'string' || confirmationId.length < 1 || confirmationId.length > 128) {
    throw new Error('real confirmation ID is invalid');
  }
  result.roots.push({ root_task_id: pending.projection.root?.root_task_id ?? null, confirmation_id: confirmationId });
  await js(`window.desktop.confirm(${JSON.stringify(confirmationId)}, true)`);
  return until(async () => {
    const value = await snapshot();
    return value.pending_confirmation === null && value.root?.state === 'ready' ? value : null;
  }, `${label} confirmed Root readiness`);
}

async function setMode(mode) {
  await js(`(() => {
    const select = document.querySelector('.mode select');
    const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set;
    setter.call(select, ${JSON.stringify(mode)});
    select.dispatchEvent(new Event('change', { bubbles: true }));
  })()`);
  await until(async () => (await snapshot()).mode === mode, `mode ${mode}`);
}

async function readAndRecordScope(label) {
  const projection = await snapshot();
  const observation = proofRules.validateProjection(projection);
  const summary = summarizeProjectionObservation(projection);
  if (result.browser_scopes.length === 0) {
    result.browser_scopes.push({ step: label, ...summary });
  } else {
    const first = result.browser_scopes[0];
    if (first.scope_id !== summary.scope_id || first.generation !== summary.generation
      || first.context.page_target_id !== summary.context.page_target_id
      || first.target_context_hash !== summary.target_context_hash) {
      throw new Error('same target read changed its installed scope or document identity');
    }
    result.browser_scopes.push({ step: label, ...summary });
  }
  if (result.browser_scopes.length > 8) throw new Error('scope evidence exceeds its record bound');
  return { projection, observation };
}

function captureChromeProcess() {
  const owner = ownerRecords.at(-1);
  if (!owner) throw new Error('browser ownership diagnostics were not emitted');
  const processes = execFileSync('ps', ['-axo', 'pid=,args='], { encoding: 'utf8', timeout: 4000, maxBuffer: 8 * 1024 * 1024 });
  const prefix = `--user-data-dir=${owner.profileParent}${path.sep}cu-headless-owned-`;
  const matches = processes.split('\n').flatMap((line) => {
    const match = /^\s*(\d+)\s+(.*)$/.exec(line);
    if (!match || !match[2].includes(prefix)) return [];
    const pid = Number(match[1]);
    const profileArgument = match[2].slice(match[2].indexOf(prefix)).split(/\s/u, 1)[0];
    return [{ pid, args: match[2], profileArgument }];
  });
  const main = matches.find((entry) => !entry.args.includes('--type='));
  if (!main || !Number.isSafeInteger(main.pid) || main.pid <= 0) throw new Error('owned Chrome main PID was not found');
  browserPid = main.pid;
  browserProfilePath = main.profileArgument.slice('--user-data-dir='.length);
  result.chrome = {
    pid: browserPid,
    profile_path_sha256: crypto.createHash('sha256').update(browserProfilePath).digest('hex'),
    matching_process_count: matches.length,
  };
}

function captureDisplay(step, ownedPid) {
  const value = JSON.parse(execFileSync(path.join(root, 'target/native-macos/headless-display-probe'),
    ownedPid == null ? [] : [String(ownedPid)], { encoding: 'utf8', timeout: 4000, maxBuffer: 16384 }));
  assert(Number.isSafeInteger(value.foreground_pid) && value.foreground_pid > 0);
  assert(Array.isArray(value.foreground_visible_window_ids) && value.foreground_visible_window_ids.length <= 512);
  if (ownedPid != null) assert.equal(value.owned_browser_onscreen_window_count, 0);
  const baseline = result.display_evidence[0];
  if (baseline) {
    assert.equal(value.foreground_pid, baseline.foreground_pid, 'foreground application changed during the browser task');
    assert.deepEqual(value.foreground_visible_window_ids, baseline.foreground_visible_window_ids,
      'foreground visible windows changed during the browser task');
  }
  result.display_evidence.push({ step, ...value });
}

async function runProductFlow() {
  activeStep = 'renderer_ready';
  await until(async () => {
    const facts = await domFacts();
    return facts.productVisible && facts.browserPickerVisible ? facts : null;
  }, 'normal renderer and preload API');
  mark('renderer_uses_real_preload', await js(`typeof window.desktop?.selectBrowserPageProfile === 'function'
    && window.desktop.selectBrowserPageProfile.length === 0
    && typeof window.desktop.snapshot === 'function'
    && typeof window.require === 'undefined' && typeof window.process === 'undefined'`));
  const initialDom = await domFacts();
  recordDom('initial_simulated_product', initialDom);
  mark('fixed_profile_picker_is_user_visible', initialDom.browserPickerVisible && initialDom.browserPickerEnabled);
  await until(async () => window.isVisible(), 'Main-owned inactive product window');
  captureDisplay('console_ready_before_profile', null);

  activeStep = 'select_fixed_profile';
  await js(`(() => [...document.querySelectorAll('button')]
    .find((button) => button.textContent.trim() === 'Use fixture browser').click())()`);
  const initial = await until(async () => {
    const projection = await snapshot();
    return projection.capabilities?.backend === 'headless_browser'
      && projection.capabilities.execution_domain === 'headless_browser'
      && projection.browser_observation ? projection : null;
  }, 'Main-owned browser scope and action capability');
  const initialObservation = proofRules.validateProjection(initial);
  if (initialObservation.readonly_checked !== false) throw new Error('new fixture checkbox must begin unchecked');
  recordDom('browser_observing', await domFacts());
  await readAndRecordScope('initial_read');
  captureChromeProcess();
  captureDisplay('owned_headless_profile_observing', browserPid);
  const browserDomain = initial.capabilities.domains.find((item) => item.domain === 'headless_browser');
  const otherDomainsUnavailable = initial.capabilities.domains
    .filter((item) => item.domain !== 'headless_browser')
    .every((item) => item.state === 'unavailable' && item.actions.length === 0);
  mark('browser_action_is_dedicated', initial.capabilities.input === 'available'
    && initial.capabilities.human_input === 'unavailable'
    && initial.capabilities.hit_test === 'unavailable'
    && browserDomain?.state === 'available'
    && browserDomain.actions.length === 1 && browserDomain.actions[0] === 'headless_browser_action'
    && otherDomainsUnavailable);

  activeStep = 'first_goal';
  await verifyScopedRequestRejections();
  await setTaskControlScope();
  await setTaskText(TASK_TEXT);
  await confirmCurrentGoal('first');
  const firstRoot = result.roots[0]?.root_task_id;
  if (!firstRoot) throw new Error('first real confirmation did not have a Root ID');
  mark('execute_option_is_enabled_for_browser_domain', !(await domFacts()).executeDisabled);
  await setMode('execute');
  const completedFirst = await until(async () => {
    const projection = await snapshot();
    return projection.root?.root_task_id === firstRoot && projection.root.state === 'completed' ? projection : null;
  }, 'first browser Root completion', 45_000);
  const afterFirst = proofRules.validateProjection(completedFirst);
  if (afterFirst.readonly_checked !== true || afterFirst.scope.scope_id !== initialObservation.scope.scope_id
    || afterFirst.source_sequence <= initialObservation.source_sequence) {
    throw new Error('first Root did not verify the same checkbox on a newer read');
  }
  recordDom('first_root_completed', await domFacts());
  await readAndRecordScope('first_root_verified');
  mark('actual_first_root_completed', completedFirst.root.state === 'completed' && completedFirst.mode === 'execute');
  captureDisplay('first_input_verified', browserPid);
  mark('actual_fixture_dom_checked_on', (await domFacts()).browserPanel?.includes('Background option: on'));
  if (executionRecords.length !== 1) throw new Error('first Root did not produce exactly one actual browser execution receipt');

  activeStep = 'same_target_no_input';
  await setMode('observe');
  const executionCountBeforeNoInput = executionRecords.length;
  await setTaskText(TASK_TEXT);
  await confirmCurrentGoal('second');
  const secondRoot = result.roots[1]?.root_task_id;
  if (!secondRoot || secondRoot === firstRoot) throw new Error('second no-input goal did not create an independent Root');
  await setMode('execute');
  const completedSecond = await until(async () => {
    const projection = await snapshot();
    return projection.root?.root_task_id === secondRoot && projection.root.state === 'completed' ? projection : null;
  }, 'same-target no-input Root completion', 45_000);
  const afterSecond = proofRules.validateProjection(completedSecond);
  if (afterSecond.readonly_checked !== true || afterSecond.scope.scope_id !== initialObservation.scope.scope_id
    || afterSecond.source_sequence <= afterFirst.source_sequence) {
    throw new Error('no-input Root did not retain the same target scope and read it freshly');
  }
  if (executionRecords.length !== executionCountBeforeNoInput) throw new Error('same-target Root submitted extra browser input');
  recordDom('same_target_no_input_completed', await domFacts());
  await readAndRecordScope('same_target_no_input_verified');
  mark('same_target_completed_without_new_input', completedSecond.root.state === 'completed'
    && completedSecond.mode === 'execute' && executionRecords.length === 1);
  mark('same_target_still_on_in_user_dom', (await domFacts()).browserPanel?.includes('Background option: on'));
  captureDisplay('second_root_no_input_verified', browserPid);
  mark('owned_browser_has_no_onscreen_window', true);
  mark('foreground_unchanged_during_browser_tasks', true);
  const screenshot = await window.webContents.capturePage();
  fs.writeFileSync(path.join(output, 'headless-product-window.png'), screenshot.toPNG(), { flag: 'wx', mode: 0o600 });
  result.screenshot_sha256 = crypto.createHash('sha256').update(screenshot.toPNG()).digest('hex');

  activeStep = 'stop_scope_invalidation';
  await js('window.desktop.stop()');
  const stopped = await until(async () => {
    const projection = await snapshot();
    const facts = await domFacts();
    return projection.browser_observation == null && facts.stopBanner?.includes('Stopped') ? { projection, facts } : null;
  }, 'device-confirmed Stop and invalidated browser scope');
  if (stopped.projection.root?.root_task_id !== secondRoot || stopped.projection.root.state !== 'completed') {
    throw new Error('Stop changed or lost the completed Root identity');
  }
  recordDom('stopped_scope_invalidated', stopped.facts);
  mark('stop_invalidated_browser_scope', stopped.projection.browser_observation == null);
  mark('stop_ack_is_visible', stopped.facts.stopBanner?.includes('Stopped'));
  mark('source_diagnostics_include_real_page_reads', sourceSnapshots.length >= 3);
  if (sourceSnapshots.length < 3) throw new Error('Main did not emit enough real browser page read diagnostics');
  if (ownerRecords.length < 2 || !proofRules.hasCompleteOwnerPids(ownerRecords.at(-1)?.pids)) {
    throw new Error('Main did not report complete browser owner PID diagnostics before cleanup');
  }
  ledgerRoots = [firstRoot, secondRoot];
  result.browser_reads = sourceSnapshots.slice();
  result.browser_execution_count = executionRecords.length;
  result.product_evidence = {
    first_root_state: completedFirst.root.state,
    second_root_state: completedSecond.root.state,
    stop_scope: 'invalidated',
    provider_calls_expected: 0,
    desktop_input_calls_expected: 0,
  };
  try {
    captureLedger();
    proofRules.validateLedger(result.ledger, firstRoot, secondRoot, executionRecords, sourceSnapshots);
    result.browser_executions = executionRecords.map(summarizeExecution);
    mark('actual_receipt_matches_effect_input_span', true);
    mark('same_target_effect_completed_without_input', true);
    mark('no_model_provider_calls', true);
  } catch (error) {
    ledgerCaptureError = safeError(error);
    finish(error, 'ledger_validation');
    return;
  }
  activeStep = 'app_quit';
  finishing = true;
  finishRequested = true;
  app.quit();
}

process.on('uncaughtException', (error) => finish(error, activeStep));
process.on('unhandledRejection', (error) => finish(error, activeStep));
process.on('SIGTERM', () => {
  if (finishing) {
    result.failure_step ||= 'runner_termination_during_shutdown';
    finishRequested = false;
    app.exit(1);
    return;
  }
  finish(new Error('smoke runner requested graceful termination'), 'runner_termination');
});
process.on('exit', () => {
  if (!finishRequested && !result.failure_step) result.failure_step = 'unexpected_process_exit';
  writeFinalResult();
});

setTimeout(() => finish(new Error('headless product smoke timed out'), activeStep), 78_000).unref();

async function importProductMain() {
  activeStep = 'normal_main_import';
  await import(pathToFileURL(path.join(root, 'apps/desktop/out/main/index.js')).href);
}

const { pathToFileURL } = require('node:url');
void importProductMain().catch((error) => finish(error, 'normal_main_import'));
