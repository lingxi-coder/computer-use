// One real frozen fixture run through the ordinary Main, preload and renderer.
// This harness deliberately has no request/reset paths or objective in its env:
// Main resolves the selected opaque catalog pair and owns the online goal.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { execFile, execFileSync } = require('node:child_process');
const { promisify } = require('node:util');
const { pathToFileURL } = require('node:url');
const { app, dialog } = require('electron');

const execFileAsync = promisify(execFile);
const root = process.env.CU_PRODUCT_ROOT;
const resultPath = process.env.CU_PRODUCT_MAIN_RESULT_PATH;
const finalReadPath = process.env.CU_PRODUCT_FINAL_READ_PATH;
const hostLedgerPath = process.env.CU_PRODUCT_HOST_LEDGER_PATH;
const fixturePid = Number(process.env.CU_PRODUCT_FIXTURE_PID);
const fixtureWindowNumber = Number(process.env.CU_PRODUCT_FIXTURE_WINDOW_NUMBER);
const taskId = process.env.CU_PRODUCT_FIXTURE_TASK_ID;
const seed = Number(process.env.CU_PRODUCT_FIXTURE_SEED);
const catalogId = process.env.CU_PRODUCT_CATALOG_ID;
const selector = process.env.CU_PRODUCT_SELECTOR;
const consentDriver = process.env.CU_PRODUCT_CONSENT_DRIVER;
const nodeRuntime = process.env.CU_PRODUCT_NODE_RUNTIME;
const tmpRoot = process.env.TMPDIR;
const bundleId = 'com.lingxi.computer-user.mvp-fixture';
const MAX_RESULT_BYTES = 8 * 1024 * 1024;
const SHA256 = /^[a-f0-9]{64}$/;

const result = {
  version: 1,
  status: 'running',
  root_id: null,
  root_state: null,
  rejection_reason: null,
  selection_verified: false,
  root_frozen_profile_matches: false,
  current_ui_ready: false,
  source_target_matches_fixture: false,
  cold_start: false,
  stop_confirmed: false,
  stop_status: 'not_requested',
  host_ledger_path: null,
  native_child_pids: [],
  supervisor_temp_directories: [],
  model_authorization: null,
  native_authorization: null,
  goal_confirmation: null,
  goal_confirmation_received: false,
  goal_confirmation_accepted: false,
  observed_root: null,
  observed_root_conflicts: [],
  unclassified_temp_entries: [],
  unclassified_temp_entries_truncated: false,
  terminal_timeline: [],
  terminal_root_headline: null,
  root_diagnostic_projection: null,
  root_diagnostic_reason: null,
  host_ledger_export_failure: null,
  execute_selected: false,
  pre_execute_source_gate: null,
  final_source_gate: null,
  final_current_ui: null,
  renderer_errors: [],
  normal_main_visibility: null,
  consent_dialogs: [],
  consent_driver_failures: [],
  failure_step: null,
};

if (!root || !resultPath || !finalReadPath || !hostLedgerPath
    || !Number.isSafeInteger(fixturePid) || fixturePid <= 0
    || !Number.isSafeInteger(fixtureWindowNumber) || fixtureWindowNumber <= 0
    || !/^(?:SET-0[1-6]|FORM-0[1-7])$/.test(taskId || '') || !Number.isInteger(seed) || seed < 1 || seed > 5
    || !SHA256.test(catalogId || '') || !SHA256.test(selector || '')
    || !consentDriver || !nodeRuntime || !tmpRoot || !path.isAbsolute(root)
    || !path.isAbsolute(nodeRuntime) || !path.isAbsolute(tmpRoot)) {
  throw new Error('Frozen fixture harness requires the bounded product-driver environment.');
}
const nodeRuntimeStat = fs.lstatSync(nodeRuntime);
assert(nodeRuntimeStat.isFile() && !nodeRuntimeStat.isSymbolicLink() && (nodeRuntimeStat.mode & 0o111) !== 0
  && fs.realpathSync(nodeRuntime) === nodeRuntime, 'Pinned system Node runtime is unavailable or unsafe.');

function safeText(value) {
  return String(value ?? '').replace(/(?:file:\/\/)?\/(?:Users|private|var|tmp|Volumes)\/[^\s"']+/g, '[path]')
    .replace(/[0-9a-f]{64}/gi, '[digest]').slice(0, 768);
}

function captureRootDiagnosticProjection(projection) {
  const rootView = projection?.root ?? null;
  const timeline = (projection?.timeline ?? []).slice(-16).map(item => ({
    at_ms: Number.isSafeInteger(item?.at_ms) && item.at_ms >= 0 ? item.at_ms : null,
    text: typeof item?.text === 'string' ? safeText(item.text).slice(0, 768) : '',
  }));
  result.terminal_timeline = timeline;
  result.terminal_root_headline = typeof rootView?.headline === 'string'
    ? safeText(rootView.headline).slice(0, 512) : null;
  result.root_diagnostic_projection = {
    revision: Number.isSafeInteger(projection?.revision) ? projection.revision : null,
    mode: typeof projection?.mode === 'string' ? projection.mode : null,
    root: rootView ? {
      root_task_id: typeof rootView.root_task_id === 'string' ? rootView.root_task_id : null,
      task_revision: Number.isSafeInteger(rootView.task_revision) ? rootView.task_revision : null,
      root_epoch: Number.isSafeInteger(rootView.root_epoch) ? rootView.root_epoch : null,
      state: typeof rootView.state === 'string' ? rootView.state : null,
      headline: result.terminal_root_headline,
    } : null,
    timeline,
  };
  const relevant = [...timeline].reverse().find(item => /fail|error|expired|unverified|cancel|suspend|timeout/i.test(item.text));
  result.root_diagnostic_reason = relevant?.text ?? timeline.at(-1)?.text ?? result.terminal_root_headline;
  return result.root_diagnostic_reason;
}

function statusForRootState(state) {
  if (state === 'completed') return 'completed';
  if (state === 'failed') return 'failed';
  if (state === 'unverified') return 'unverified';
  if (state === 'expired') return 'timeout';
  if (state === 'cancelled') return 'cancelled';
  return 'unknown';
}

function sourceTargetSummary(target) {
  if (!target || typeof target !== 'object') return null;
  return {
    pid: Number.isSafeInteger(target.pid) ? target.pid : null,
    app: typeof target.app === 'string' ? safeText(target.app).slice(0, 256) : null,
    window_id: typeof target.window_id === 'string' ? safeText(target.window_id).slice(0, 128) : null,
    snapshot_id: typeof target.snapshot_id === 'string' ? safeText(target.snapshot_id).slice(0, 256) : null,
    structure_sequence: Number.isSafeInteger(target.structure_sequence) ? target.structure_sequence : null,
    observed_ms: Number.isSafeInteger(target.observed_ms) ? target.observed_ms : null,
    valid_until_ms: Number.isSafeInteger(target.valid_until_ms) ? target.valid_until_ms : null,
  };
}

function preExecuteSourceSummary(projection, classification, baselineTarget, activationAttempted, status = classification?.kind ?? 'unknown') {
  const rootView = projection?.root ?? null;
  return {
    status,
    reason: classification?.reason ?? null,
    activation_attempted: activationAttempted,
    projection_revision: Number.isSafeInteger(projection?.revision) ? projection.revision : null,
    mode: typeof projection?.mode === 'string' ? projection.mode : null,
    root: rootView ? {
      root_task_id: typeof rootView.root_task_id === 'string' ? rootView.root_task_id : null,
      task_revision: Number.isSafeInteger(rootView.task_revision) ? rootView.task_revision : null,
      state: typeof rootView.state === 'string' ? rootView.state : null,
    } : null,
    pending_confirmation_kind: projection?.pending_confirmation?.kind?.kind ?? null,
    stop_status: projection?.stop_status ?? null,
    baseline_target: sourceTargetSummary(baselineTarget),
    source_target: sourceTargetSummary(classification?.target ?? projection?.observation?.observed_target),
  };
}

function failPreExecuteSourceGate(outcome, baselineTarget, activationAttempted) {
  const projection = outcome?.projection ?? null;
  const classification = outcome?.kind === 'timeout' ? outcome.lastClassification : outcome;
  if (projection) captureRootDiagnosticProjection(projection);
  result.pre_execute_source_gate = preExecuteSourceSummary(
    projection, classification, baselineTarget, activationAttempted, outcome?.kind ?? classification?.kind,
  );
  result.status = 'unknown';
  const failureSteps = {
    terminal_root: 'pre_execute_root_terminal',
    human_suspended: 'pre_execute_human_suspended',
    awaiting_user_input: 'pre_execute_user_input_required',
    activation_failed: 'pre_execute_activation_failed',
    mode_changed: 'pre_execute_mode_changed',
    stop_requested: 'pre_execute_stop_requested',
    root_identity_mismatch: 'pre_execute_root_mismatch',
    invalid_root: 'pre_execute_invalid_root',
    invalid_projection: 'pre_execute_invalid_projection',
    root_not_ready: 'pre_execute_root_not_ready',
    timeout: 'pre_execute_source_timeout',
  };
  result.failure_step = failureSteps[outcome?.kind] ?? 'pre_execute_source_unavailable';
  result.rejection_reason = (outcome?.error ? safeText(outcome.error?.message ?? outcome.error) : null)
    ?? result.root_diagnostic_reason
    ?? result.pre_execute_source_gate.reason
    ?? `Pre-Execute Source gate ended in ${result.pre_execute_source_gate.status}`;
}

function writeNewJson(file, value, maximum = MAX_RESULT_BYTES) {
  const bytes = Buffer.from(`${JSON.stringify(value, null, 2)}\n`, 'utf8');
  assert(bytes.length > 0 && bytes.length <= maximum, 'Evidence JSON exceeds its bound.');
  const fd = fs.openSync(file, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
  try { fs.fchmodSync(fd, 0o600); fs.writeFileSync(fd, bytes); }
  finally { fs.closeSync(fd); }
  const stat = fs.lstatSync(file);
  assert(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1
    && stat.uid === process.getuid() && (stat.mode & 0o077) === 0);
  return { bytes: bytes.length, sha256: crypto.createHash('sha256').update(bytes).digest('hex') };
}

function readStablePrivate(file, maximum, label) {
  const before = fs.lstatSync(file);
  assert(before.isFile() && !before.isSymbolicLink() && before.nlink === 1
    && before.uid === process.getuid() && (before.mode & 0o077) === 0
    && before.size > 0 && before.size <= maximum, `${label} is outside its private read bounds`);
  assert.equal(fs.realpathSync(file), file, `${label} path contains a symbolic link`);
  const bytes = fs.readFileSync(file);
  const after = fs.lstatSync(file);
  assert(after.isFile() && !after.isSymbolicLink() && after.nlink === 1
    && after.dev === before.dev && after.ino === before.ino && after.size === before.size
    && after.mtimeMs === before.mtimeMs && after.ctimeMs === before.ctimeMs && bytes.length === after.size,
  `${label} changed while reading`);
  return { bytes, sha256: crypto.createHash('sha256').update(bytes).digest('hex') };
}

function delay(ms) { return new Promise(resolve => setTimeout(resolve, ms)); }

async function until(read, label, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try { const value = await read(); if (value) return value; }
    catch (error) { lastError = error; }
    await delay(100);
  }
  throw new Error(`Timed out waiting for ${label}${lastError ? ` (${safeText(lastError)})` : ''}.`);
}

async function js(source) {
  if (!window || window.isDestroyed()) throw new Error('Normal product Main window is unavailable.');
  return window.webContents.executeJavaScript(source);
}

async function snapshot() {
  const projection = await js('window.desktop.snapshot()');
  assert(projection && typeof projection === 'object' && !Array.isArray(projection), 'Product projection is invalid.');
  const bytes = Buffer.byteLength(JSON.stringify(projection), 'utf8');
  assert(bytes <= 1024 * 1024, 'Product projection exceeds its byte bound.');
  return projection;
}

function tempDirectories() {
  const names = fs.readdirSync(tmpRoot);
  const owned = [];
  const unclassified = [];
  for (const name of names.slice(0, 64)) {
    const candidate = /^cu[A-Za-z0-9]{6}$/.test(name);
    const full = path.join(tmpRoot, name);
    try {
      const stat = fs.lstatSync(full);
      const privateDirectory = stat.isDirectory() && !stat.isSymbolicLink()
        && fs.realpathSync(full) === full && stat.uid === process.getuid() && (stat.mode & 0o077) === 0;
      if (!candidate || !privateDirectory) {
        unclassified.push({ name: safeText(name), reason: candidate ? 'unverified_supervisor_candidate' : 'unrelated_temp_entry' });
        continue;
      }
      const database = path.join(full, 'host.db');
      if (!fs.existsSync(database)) {
        unclassified.push({ name: safeText(name), reason: 'private_candidate_without_host_database' });
        continue;
      }
      const databaseStat = fs.lstatSync(database);
      if (!databaseStat.isFile() || databaseStat.isSymbolicLink() || databaseStat.nlink !== 1
          || databaseStat.uid !== process.getuid() || (databaseStat.mode & 0o033) !== 0
          || (databaseStat.mode & 0o400) === 0
          || fs.realpathSync(database) !== database) {
        unclassified.push({ name: safeText(name), reason: 'invalid_host_database_entry' });
        continue;
      }
      owned.push(full);
    } catch {
      unclassified.push({ name: safeText(name), reason: 'unavailable_temp_entry' });
    }
  }
  result.unclassified_temp_entries = [...new Map(
    [...result.unclassified_temp_entries, ...unclassified].map(item => [item.name, item]),
  ).values()].slice(-64);
  result.unclassified_temp_entries_truncated ||= names.length > 64;
  return owned;
}

function recordTempDirectories() {
  const paths = tempDirectories();
  result.supervisor_temp_directories = [...new Set([...result.supervisor_temp_directories, ...paths])];
  return paths;
}

function findHostDatabase() {
  const candidates = tempDirectories().flatMap(directory => {
    const file = path.join(directory, 'host.db');
    return fs.existsSync(file) ? [file] : [];
  });
  assert.equal(candidates.length, 1, 'Expected one exact Main-owned Host database.');
  const stat = fs.lstatSync(candidates[0]);
  assert(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1 && stat.uid === process.getuid());
  assert.equal(fs.realpathSync(candidates[0]), candidates[0]);
  return candidates[0];
}

function captureNativeChildren() {
  const binDir = path.join(root, 'target/debug');
  const roles = ['cu-host', 'cu-device', 'cu-source'];
  const stdout = execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,command='], {
    encoding: 'utf8', timeout: 5_000, maxBuffer: 8 * 1024 * 1024,
  });
  const matches = stdout.split('\n').flatMap(line => {
    const match = /^\s*(\d+)\s+(\d+)\s+(.*)$/.exec(line);
    if (!match || Number(match[2]) !== process.pid) return [];
    const role = roles.find(name => match[3].split(/\s+/)[0] === path.join(binDir, name));
    return role ? [{ role, pid: Number(match[1]) }] : [];
  });
  assert(matches.length <= 8, 'Main native child count exceeds its bound.');
  result.native_child_pids = [...new Map([...result.native_child_pids, ...matches].map(item => [item.pid, item])).values()];
  return matches;
}

function activateExactFixture() {
  execFileSync('/usr/bin/osascript', ['-e',
    `tell application "System Events" to set frontmost of first application process whose unix id is ${fixturePid} to true`],
  { encoding: 'utf8', timeout: 3_000, stdio: ['ignore', 'pipe', 'pipe'] });
}

async function driveConsent(action, kind, options) {
  assert.equal(action, 'allow');
  assert(options && options.defaultId === 0 && options.cancelId === 0, 'Native/product consent must default to Cancel.');
  const shown = originalShowMessageBox(...dialogArgsForConsent);
  assert(modelAuthorizationPolicy, 'Native consent helper contract is unavailable.');
  const helperArgs = modelAuthorizationPolicy.nativeConsentDriverArgs(action, kind);
  const helperKind = helperArgs[1];
  const driven = execFileAsync(consentDriver, helperArgs, { encoding: 'utf8', timeout: 10_000, maxBuffer: 16 * 1024 })
    .catch(error => {
      const stderr = Buffer.isBuffer(error?.stderr) ? error.stderr.toString('utf8')
        : typeof error?.stderr === 'string' ? error.stderr : '';
      if (result.consent_driver_failures.length < 8) {
        result.consent_driver_failures.push({
          kind,
          helper_kind: helperKind,
          error_code: error?.code === undefined ? null : String(error.code).slice(0, 64),
          signal: typeof error?.signal === 'string' ? error.signal : null,
          stderr: safeText(stderr).slice(0, 512),
        });
      }
      throw error;
    });
  const [response, driver] = await Promise.all([shown, driven]);
  const receipt = JSON.parse(driver.stdout);
  assert.equal(receipt.status, 'pressed', `${kind} consent button was not pressed through its native dialog`);
  result.consent_dialogs.push({ kind, response: response.response, button: action, helper_status: receipt.status });
  assert.equal(response.response, 1, `${kind} consent did not receive the explicit Allow response`);
  return response;
}

const originalShowMessageBox = dialog.showMessageBox.bind(dialog);
let dialogArgsForConsent = [];
dialog.showMessageBox = async (...args) => {
  const options = args.at(-1);
  const title = String(options?.title ?? '');
  const message = String(options?.message ?? '');
  const detail = String(options?.detail ?? '');
  let kind;
  if (message === 'Allow Computer Use to use this model for the selected purposes?') {
    assert(modelAuthorizationPolicy, 'Model authorization policy is unavailable for native consent.');
    assert(modelAuthorizationPolicy.productPurposeConsentMatches(detail),
      'Native model consent must list exactly S2 and S1, without screenshot computer proposals.');
    kind = 'model';
  } else if (detail.includes(bundleId) && detail.includes(`Process ${fixturePid}`)) {
    kind = 'fixture_window';
  } else {
    // Unknown dialogs stay on their default Cancel path; do not authorize them.
    return originalShowMessageBox(...args);
  }
  dialogArgsForConsent = args;
  try { return await driveConsent('allow', kind, options); }
  finally { dialogArgsForConsent = []; }
};

let window = null;
let finishStarted = false;
let finalizationPromise = null;
let timeoutHandle;
let modelAuthorizationPolicy = null;
let waitForNormalMainWindowVisible = null;

async function planningAuthorization() {
  assert(modelAuthorizationPolicy, 'Model authorization policy is unavailable.');
  let status = await js('window.desktop.modelAuthorization()');
  result.model_authorization = modelAuthorizationPolicy.summarizeProductModelAuthorization(status);
  if (status.phase === 'authorized') {
    if (modelAuthorizationPolicy.productModelAuthorizationReady(status)) return status;
    result.model_authorization.blocked = 'existing_model_authorization_lacks_exact_s2_s1_purposes';
    return { blocked: result.model_authorization.blocked };
  }
  if (status.planning_available === true) {
    result.model_authorization.blocked = 'planning_available_without_saved_product_model_authorization';
    return { blocked: result.model_authorization.blocked };
  }
  if (!Array.isArray(status.saved) || status.saved.length === 0) {
    result.model_authorization.blocked = 'product_model_not_configured';
    return { blocked: result.model_authorization.blocked };
  }
  if (status.saved.length !== 1) {
    result.model_authorization.blocked = 'multiple_saved_model_connections_require_product_selection';
    return { blocked: result.model_authorization.blocked };
  }
  if (status.can_authorize !== true) {
    result.model_authorization.blocked = 'product_model_authorization_unavailable';
    return { blocked: result.model_authorization.blocked };
  }
  const selectedPurposes = await js(`(() => {
    const s2 = document.querySelector('input[aria-label="S2 task planning purpose"]');
    const s1 = document.querySelector('input[aria-label="S1 semantic decision purpose"]');
    const computer = document.querySelector('input[aria-label="Computer proposals purpose"]');
    if (!s2 || !s1 || !computer || s2.disabled || s1.disabled || computer.checked) return false;
    if (!s2.checked) s2.click();
    if (!s1.checked) s1.click();
    return true;
  })()`);
  if (!selectedPurposes) {
    result.model_authorization.blocked = 'required_s2_s1_purpose_controls_unavailable_or_computer_purpose_selected';
    return { blocked: result.model_authorization.blocked };
  }
  const exactPurposesSelected = await until(() => js(`(() => {
    const s2 = document.querySelector('input[aria-label="S2 task planning purpose"]');
    const s1 = document.querySelector('input[aria-label="S1 semantic decision purpose"]');
    const computer = document.querySelector('input[aria-label="Computer proposals purpose"]');
    return !!s2 && !!s1 && !!computer && s2.checked && s1.checked && !computer.checked;
  })()`), 'exact S2 and S1 purpose selection');
  if (!exactPurposesSelected) {
    result.model_authorization.blocked = 'required_s2_s1_purpose_selection_not_retained';
    return { blocked: result.model_authorization.blocked };
  }
  const clicked = await js(`(() => {
    const row = document.querySelector('.saved-connections .saved-connection');
    const button = row?.querySelector('button');
    if (!button || button.disabled || !button.textContent.includes('Use saved key')) return false;
    button.click(); return true;
  })()`);
  if (!clicked) {
    result.model_authorization.blocked = 'saved_model_authorization_control_unavailable';
    return { blocked: result.model_authorization.blocked };
  }
  status = await until(async () => {
    const current = await js('window.desktop.modelAuthorization()');
    if (current.phase === 'authorized') {
      return modelAuthorizationPolicy.productModelAuthorizationReady(current)
        ? current : { blocked: 'saved_model_authorization_missing_exact_s2_s1_purposes' };
    }
    if (current.phase === 'unavailable') return { blocked: 'saved_model_authorization_unavailable' };
    if (current.phase === 'not_configured' && current.reason) return { blocked: 'saved_model_authorization_unavailable' };
    return false;
  }, 'normal S2 and S1 model-purpose authorization', 20_000);
  if (status.blocked) {
    const finalStatus = await js('window.desktop.modelAuthorization()');
    result.model_authorization = {
      ...modelAuthorizationPolicy.summarizeProductModelAuthorization(finalStatus),
      blocked: status.blocked,
    };
    return status;
  }
  result.model_authorization = modelAuthorizationPolicy.summarizeProductModelAuthorization(status);
  return status;
}

async function authorizeFixtureWindow() {
  const state = await until(async () => {
    activateExactFixture();
    const status = await js('window.desktop.nativeAuthorization()');
    const target = status.authorized ?? status.candidate;
    if (status.phase === 'authorized' && target?.pid === fixturePid && target?.app === bundleId) return status;
    if (status.phase === 'unavailable' || status.phase === 'not_native') return { blocked: 'fixture_native_source_unavailable' };
    if (target?.pid === fixturePid && target?.app === bundleId && status.can_authorize) return status;
    return false;
  }, 'fresh native fixture candidate', 25_000);
  if (state.blocked) return state;
  if (state.phase === 'authorized') {
    result.native_authorization = { phase: state.phase, pid: state.authorized.pid, app: state.authorized.app };
    return state;
  }
  const clicked = await js(`(() => {
    const panel = document.querySelector('.native-authorization');
    const button = panel?.querySelector('button.primary');
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`);
  if (!clicked) return { blocked: 'fixture_window_authorization_control_unavailable' };
  const authorized = await until(async () => {
    const status = await js('window.desktop.nativeAuthorization()');
    if (status.phase === 'authorized' && status.authorized?.pid === fixturePid && status.authorized?.app === bundleId) return status;
    if (status.phase === 'unavailable') return { blocked: 'fixture_window_authorization_unavailable' };
    return false;
  }, 'normal fixture window authorization', 20_000);
  if (authorized.blocked) return authorized;
  result.native_authorization = { phase: authorized.phase, pid: authorized.authorized.pid, app: authorized.authorized.app };
  return authorized;
}

async function listCatalog() {
  const { fixtureScopeFamilyForTaskId } = await import(
    pathToFileURL(path.join(root, 'scripts/mvp/frozen-fixture-scope.mjs')).href,
  );
  const catalog = await js('window.desktop.frozenTaskCatalog()');
  assert.equal(catalog?.status?.state, 'available', 'normal Main did not load the frozen goal catalog');
  assert(Array.isArray(catalog.tasks) && catalog.tasks.length === 120, 'normal Main must expose all 120 frozen identities');
  const sameCatalog = new Set(catalog.tasks.map(task => task.catalog_id));
  assert.deepEqual([...sameCatalog], [catalogId], 'public catalog ID differs from the pinned manifest');
  const matches = catalog.tasks.filter(task => task.suite === 'fixture' && task.task_id === taskId && task.seed === String(seed));
  assert.equal(matches.length, 1, 'normal Main did not expose exactly one requested frozen identity');
  const selected = matches[0];
  assert.equal(selected.selector, selector, 'normal Main selector differs from the frozen goal catalog');
  const expectedFamily = fixtureScopeFamilyForTaskId(taskId);
  assert(expectedFamily === 'control' || expectedFamily === 'form', 'requested task has no driver-supported fixture scope');
  assert.equal(selected.scope_family, expectedFamily, 'normal Main scope family differs from the frozen task identity');
  result.selection_verified = true;
  if (!selected.scope_compatible) return { blocked: selected.unsupported_reason ?? 'frozen_scope_not_compatible' };
  result.selection = {
    suite: selected.suite,
    task_id: selected.task_id,
    seed: Number(selected.seed),
    catalog_id: selected.catalog_id,
    selector: selected.selector,
    scope_family: selected.scope_family,
    scope_compatible: selected.scope_compatible,
    index: catalog.tasks.indexOf(selected),
  };
  return { catalog, selected };
}

async function selectAndSubmit(selection) {
  const clicked = await js(`(() => {
    const list = document.querySelector('[aria-label="Frozen task catalog"]');
    const row = list?.querySelectorAll(':scope > li')[${selection.index}];
    const button = row?.querySelector('button');
    if (!button || button.disabled) throw new Error('frozen task row is unavailable');
    button.click(); return true;
  })()`);
  assert.equal(clicked, true);
  await until(async () => {
    const selected = await js(`document.querySelector('[aria-label="Frozen task catalog"]')?.querySelectorAll(':scope > li')[${selection.index}]?.querySelector('button')?.getAttribute('aria-pressed') === 'true'`);
    return selected ? true : false;
  }, 'selected frozen catalog identity');
  const submit = await js(`(() => {
    const list = document.querySelector('[aria-label="Frozen task catalog"]');
    const panel = list?.closest('section');
    const button = panel ? [...panel.querySelectorAll('button')].find(item => !list.contains(item)) : null;
    if (!button || button.disabled || button.textContent.trim() !== 'Submit selected task') return false;
    button.click(); return true;
  })()`);
  assert.equal(submit, true, 'normal frozen-task submit button was unavailable');
}

async function waitForAdmissionOrRefusal(admissionState) {
  return admissionState.waitForGoalAdmission({
    readProjection: snapshot,
    readUiError: () => js(`(() => [...document.querySelectorAll('[role="alert"]')]
      .map(item => item.innerText?.trim()).find(text => text && text.length > 0) ?? null)()`),
    onObservation: observation => {
      result.observed_root = observation.observedRoot;
      if (observation.observedRoot) {
        result.root_id = observation.observedRoot.root_task_id;
        result.root_task_revision = observation.observedRoot.task_revision;
      }
      if (observation.conflict) {
        result.observed_root_conflicts = [...result.observed_root_conflicts, observation.conflict].slice(-4);
      }
    },
    timeoutMs: 30_000,
    pollIntervalMs: 100,
  });
}

async function exportRootHostLedger(rootId, taskRevision) {
  captureNativeChildren();
  const tempDirs = recordTempDirectories();
  assert.equal(tempDirs.length, 1, 'Expected one active Main-owned supervisor directory for the exact Root ledger.');
  const hostDb = path.join(tempDirs[0], 'host.db');
  const { reuseOrExportRootLedger } = await import(
    pathToFileURL(path.join(root, 'scripts/mvp/root-ledger-reuse.mjs')).href,
  );
  const hostEvidence = await reuseOrExportRootLedger({
    rootId,
    taskRevision,
    readExisting: () => {
      try { fs.lstatSync(hostLedgerPath); }
      catch (error) { if (error?.code === 'ENOENT') return null; throw error; }
      return JSON.parse(readStablePrivate(hostLedgerPath, 8 * 1024 * 1024, 'Root Host evidence').bytes.toString('utf8'));
    },
    exportNew: () => execFileSync(nodeRuntime, [path.join(root, 'scripts/mvp/host-ledger-evidence.mjs'),
      '--db', hostDb, '--root', rootId, '--output', hostLedgerPath, '--max-rows', '10000', '--max-bytes', String(8 * 1024 * 1024)], {
      encoding: 'utf8', timeout: 10_000, maxBuffer: 16 * 1024,
    }),
  });
  result.host_ledger_path = path.basename(hostLedgerPath);
  result.host_ledger_export_failure = null;
  return hostEvidence;
}

function stopForExecutionInterruption(outcome) {
  const projection = outcome?.projection ?? null;
  if (projection) {
    captureRootDiagnosticProjection(projection);
    result.root_state = projection.root?.state ?? result.root_state;
  }
  result.status = 'unknown';
  result.failure_step = outcome?.kind === 'timeout' ? 'root_terminal_timeout'
    : outcome?.kind === 'root_identity_mismatch' ? 'execution_root_identity_mismatch'
      : outcome?.kind === 'needs_user_action' ? 'execution_needs_user_action' : 'execution_progress_invalid';
  result.rejection_reason = outcome?.kind === 'needs_user_action'
    ? `Execution stopped for user attention (${safeText(outcome.reason)}); no confirmation was approved.`
    : outcome?.kind === 'timeout'
      ? 'Timed out waiting for a terminal Root state.'
      : outcome?.kind === 'root_identity_mismatch'
        ? 'The Root identity changed while waiting for terminal state.'
        : 'The post-Execute projection could not be safely classified.';
}

async function runProductFlow() {
  const { assertGoalConfirmationRootBinding } = await import(
    pathToFileURL(path.join(root, 'scripts/mvp/goal-confirmation-binding.mjs')).href,
  );
  modelAuthorizationPolicy = await import(
    pathToFileURL(path.join(root, 'scripts/mvp/model-authorization-policy.mjs')).href,
  );
  const { validateFixtureGoalPrompt } = await import(
    pathToFileURL(path.join(root, 'scripts/mvp/frozen-fixture-scope.mjs')).href,
  );
  const admissionState = await import(
    pathToFileURL(path.join(root, 'scripts/mvp/admission-state.mjs')).href,
  );
  const initialProjection = await snapshot();
  captureNativeChildren();
  recordTempDirectories();
  assert.equal(initialProjection.mode, 'observe', 'normal product must begin in observe mode');
  assert.equal(initialProjection.root ?? null, null, 'fresh product session already has a Root');
  result.cold_start = initialProjection.mode === 'observe' && initialProjection.root == null;

  const planning = await planningAuthorization();
  if (planning.blocked) {
    result.status = 'blocked';
    result.rejection_reason = planning.blocked;
    result.model_authorization = { ...result.model_authorization, blocked: planning.blocked };
    return;
  }
  const native = await authorizeFixtureWindow();
  if (native.blocked) {
    result.status = 'blocked';
    result.rejection_reason = native.blocked;
    return;
  }
  const { selected } = await listCatalog();
  if (selected.blocked) {
    result.status = 'rejected';
    result.rejection_reason = selected.blocked;
    return;
  }
  await selectAndSubmit(result.selection);
  const admission = await waitForAdmissionOrRefusal(admissionState);
  if (admission.kind === 'explicit_refusal' && !admission.observedRoot) {
    result.status = 'rejected';
    result.rejection_reason = safeText(admission.refusal.text);
    return;
  }

  if (admission.kind !== 'goal_prompt') {
    if (admission.observedRoot) {
      result.root_id = admission.observedRoot.root_task_id;
      result.root_task_revision = admission.observedRoot.task_revision;
      result.root_state = admission.terminalRoot?.state ?? admission.observedRoot.latest_state;
      captureRootDiagnosticProjection(admission.projection);
      try {
        await exportRootHostLedger(admission.observedRoot.root_task_id, admission.observedRoot.task_revision);
      } catch (error) {
        result.host_ledger_export_failure = safeText(error?.message ?? error);
      }
    }
    result.status = 'failed';
    result.failure_step = admission.kind === 'timeout' ? 'admission_timeout' : 'goal_confirmation_missing';
    result.rejection_reason = admission.kind === 'explicit_refusal'
      ? safeText(admission.refusal.text)
      : admission.kind === 'terminal_root'
        ? `Root entered ${admission.terminalRoot.state} before Goal confirmation`
        : admission.kind === 'root_identity_conflict'
          ? 'Root identity changed while waiting for Goal confirmation'
          : admission.kind === 'invalid_root_projection'
            ? 'Root projection had an invalid identity or state'
            : admission.kind === 'goal_prompt_mismatch'
              ? 'Goal confirmation did not match the first observed Root'
              : admission.kind === 'timeout'
                ? 'Timed out waiting for a Goal confirmation or typed terminal/refusal'
                : 'Normal Goal confirmation was not produced';
    return;
  }
  const prompt = admission.prompt;
  result.goal_confirmation_received = true;
  const goalText = prompt.kind.goal?.user_text ?? prompt.kind.goal?.text ?? null;
  assert.equal(typeof goalText, 'string', 'normal Goal confirmation does not expose its user text');
  const confirmedScope = validateFixtureGoalPrompt(prompt.kind.goal, taskId);
  assert.equal(result.selection.scope_family, confirmedScope.family,
    'Goal confirmation scope family differs from the selected frozen task');
  const goalTextSha256 = crypto.createHash('sha256').update(goalText).digest('hex');
  assertGoalConfirmationRootBinding(prompt, admission.projection.root, 'Goal confirmation prompt');
  const promptDom = await js(`(() => {
    const panel = document.querySelector('.prompt');
    const yes = panel && [...panel.querySelectorAll('button')].find(button => button.textContent.trim() === 'Yes, do it');
    return { text: panel?.innerText ?? null, allow_enabled: !!yes && !yes.disabled };
  })()`);
  assert(promptDom && promptDom.allow_enabled === true, 'normal Goal confirmation UI is unavailable');
  result.goal_confirmation = {
    confirmation_id: prompt.confirmation_id,
    root_task_id: prompt.root_task_id,
    task_revision: prompt.task_revision,
    kind: prompt.kind.kind,
    objective_sha256: goalTextSha256,
    scope_family: confirmedScope.family,
    allowed_scope_canonical: confirmedScope.allowed_scope,
    allowed_scope_wire: confirmedScope.allowed_scope_wire,
    application: prompt.kind.goal.app,
    target_label: prompt.kind.goal.target_label,
    visible_text: String(promptDom.text ?? '').slice(0, 2048),
    user_approved: false,
  };
  await js(`(() => {
    const panel = document.querySelector('.prompt');
    const yes = panel && [...panel.querySelectorAll('button')].find(button => button.textContent.trim() === 'Yes, do it');
    if (!yes || yes.disabled) throw new Error('Goal confirmation Allow button is unavailable');
    yes.click(); return true;
  })()`);
  result.root_id = prompt.root_task_id;
  result.root_task_revision = prompt.task_revision;
  result.goal_confirmation.user_approved = true;
  result.goal_confirmation_accepted = true;
  const readyOutcome = await admissionState.waitForConfirmedRootReady({
    readProjection: snapshot,
    rootTaskId: prompt.root_task_id,
    taskRevision: prompt.task_revision,
    timeoutMs: 120_000,
    pollIntervalMs: 100,
  });
  if (readyOutcome.kind !== 'ready') {
    if (readyOutcome.projection) captureRootDiagnosticProjection(readyOutcome.projection);
    if (readyOutcome.root && readyOutcome.root.root_task_id === prompt.root_task_id
        && readyOutcome.root.task_revision === prompt.task_revision) {
      result.root_state = readyOutcome.root.state;
      result.status = readyOutcome.root.state === 'completed' ? 'unknown' : statusForRootState(readyOutcome.root.state);
    } else {
      result.status = 'unknown';
    }
    result.failure_step = readyOutcome.kind === 'terminal_root' ? 'confirmed_root_terminal_before_ready'
      : readyOutcome.kind === 'not_ready' ? 'confirmed_root_not_ready'
        : readyOutcome.kind === 'timeout' ? 'confirmed_root_ready_timeout'
          : 'confirmed_root_identity_mismatch';
    result.rejection_reason = (readyOutcome.error
      ? safeText(readyOutcome.error?.message ?? readyOutcome.error)
      : null) ?? result.root_diagnostic_reason
      ?? (readyOutcome.kind === 'terminal_root'
        ? `Confirmed Root entered ${readyOutcome.root.state} before Ready`
        : readyOutcome.kind === 'timeout' ? 'Timed out waiting for the confirmed Root to become Ready'
          : 'Confirmed Root did not match the expected Ready identity/state');
    return;
  }
  assertGoalConfirmationRootBinding(prompt, readyOutcome.root, 'ready Root');
  result.root_state = readyOutcome.root.state;
  const beforeExecute = await snapshot();
  assert.equal(beforeExecute.mode, 'observe', 'normal product left Observe before Execute was selected');
  assertGoalConfirmationRootBinding(prompt, beforeExecute.root, 'pre-Execute Root');
  const preExecuteOptions = {
    rootTaskId: prompt.root_task_id,
    taskRevision: prompt.task_revision,
    fixturePid,
    bundleId,
    windowId: `window-${fixtureWindowNumber}`,
  };
  const baselineTarget = beforeExecute.observation?.observed_target ?? null;
  const preActivationGate = admissionState.classifyPreExecuteSource(beforeExecute, {
    ...preExecuteOptions, baselineTarget: null, requireFreshSource: false,
  });
  if (!['ready', 'pending'].includes(preActivationGate.kind)) {
    failPreExecuteSourceGate({ ...preActivationGate, projection: beforeExecute }, baselineTarget, false);
    return;
  }
  result.pre_execute_source_gate = preExecuteSourceSummary(
    beforeExecute, { ...preActivationGate, target: baselineTarget }, baselineTarget, false, 'activation_requested',
  );
  result.pre_execute_source_gate.activation_attempted = true;
  try { activateExactFixture(); }
  catch (error) {
    const activationProjection = await snapshot().catch(() => beforeExecute);
    failPreExecuteSourceGate({ kind: 'activation_failed', projection: activationProjection, error }, baselineTarget, true);
    return;
  }
  const freshSource = await admissionState.waitForFreshPreExecuteSource({
    readProjection: snapshot,
    ...preExecuteOptions,
    baselineTarget,
    timeoutMs: 15_000,
    pollIntervalMs: 100,
  });
  if (freshSource.kind !== 'ready') {
    failPreExecuteSourceGate(freshSource, baselineTarget, true);
    return;
  }
  const executeGateProjection = await snapshot();
  const executeGate = admissionState.classifyPreExecuteSource(executeGateProjection, {
    ...preExecuteOptions,
    baselineTarget,
    minimumTarget: freshSource.target,
    requireFreshSource: false,
  });
  if (executeGate.kind !== 'ready') {
    failPreExecuteSourceGate({ ...executeGate, projection: executeGateProjection }, baselineTarget, true);
    return;
  }
  result.pre_execute_source_gate = preExecuteSourceSummary(
    executeGateProjection, executeGate, baselineTarget, true, 'passed',
  );
  const minSourceSequence = freshSource.target.structure_sequence;
  const minSourceObservedMs = freshSource.target.observed_ms;
  const expectedRootId = JSON.stringify(prompt.root_task_id);
  const expectedBundleId = JSON.stringify(bundleId);
  const executed = await js(`(async () => {
    const projection = await window.desktop.snapshot();
    const root = projection?.root;
    const target = projection?.observation?.observed_target;
    if (!projection || projection.mode !== 'observe'
      || root?.root_task_id !== ${expectedRootId} || root?.task_revision !== ${prompt.task_revision}
      || !['ready', 'waiting_plan'].includes(root.state)
      || projection.pending_confirmation != null || projection.stop_status !== 'not_stopped'
      || target?.pid !== ${fixturePid} || target?.app !== ${expectedBundleId}
      || target?.window_id !== ${JSON.stringify(`window-${fixtureWindowNumber}`)}
      || !Number.isSafeInteger(target.structure_sequence) || target.structure_sequence < ${minSourceSequence}
      || !Number.isSafeInteger(target.observed_ms) || target.observed_ms < ${minSourceObservedMs}
      || !Number.isSafeInteger(target.valid_until_ms) || target.valid_until_ms <= target.observed_ms
      || target.valid_until_ms - target.observed_ms > 2000) {
      return { ok: false, reason: 'pre_execute_projection_changed', projection };
    }
    const select = document.querySelector('.mode select');
    const option = select?.querySelector('option[value="execute"]');
    if (!select || select.disabled || select.value !== 'observe' || !option || option.disabled) {
      return { ok: false, reason: 'normal_execute_control_changed', projection };
    }
    select.value = 'execute'; select.dispatchEvent(new Event('change', { bubbles: true }));
    return { ok: true, projection };
  })()`);
  if (!executed?.ok) {
    const failedProjection = executed?.projection ?? await snapshot();
    captureRootDiagnosticProjection(failedProjection);
    result.pre_execute_source_gate = preExecuteSourceSummary(
      failedProjection,
      { kind: 'execute_gate_changed', reason: executed?.reason ?? 'normal_execute_control_changed',
        target: failedProjection?.observation?.observed_target ?? null },
      baselineTarget, true, 'execute_gate_changed',
    );
    result.status = 'unknown';
    result.failure_step = 'pre_execute_gate_changed';
    result.rejection_reason = result.pre_execute_source_gate.reason;
    return;
  }
  result.execute_selected = true;
  await until(async () => {
    const projection = await snapshot();
    if (projection.mode !== 'execute' || projection.root?.root_task_id !== result.root_id
        || projection.root?.task_revision !== result.root_task_revision) return false;
    return projection;
  }, 'normal Execute mode transition', 15_000);
  const executionDeadline = Date.now() + 20 * 60 * 1000;
  const remainingExecutionMs = () => Math.max(0, executionDeadline - Date.now());
  let progress = await admissionState.waitForExecutionProgress({
    readProjection: snapshot,
    rootTaskId: prompt.root_task_id,
    taskRevision: prompt.task_revision,
    timeoutMs: remainingExecutionMs(),
    pollIntervalMs: 100,
  });
  if (progress.kind !== 'terminal_root') {
    stopForExecutionInterruption(progress);
    return;
  }
  const terminal = progress.projection;
  result.root_state = progress.root.state;
  if (progress.root.state === 'completed') result.status = 'completed';
  else {
    result.status = statusForRootState(progress.root.state);
    captureRootDiagnosticProjection(terminal);
    result.rejection_reason = result.root_diagnostic_reason ?? `Root ended in ${progress.root.state}`;
  }
  const finalSourceBaseline = result.pre_execute_source_gate.source_target;
  let lastFinalProjection = null;
  let current;
  try {
    current = await until(async () => {
      const projection = await snapshot();
      lastFinalProjection = projection;
      if (projection.root?.root_task_id !== result.root_id
          || projection.root?.task_revision !== result.root_task_revision) return false;
      const target = projection.observation?.observed_target;
      return target && target.pid === fixturePid && target.app === bundleId
        && target.window_id === `window-${fixtureWindowNumber}`
        && Number.isSafeInteger(target.structure_sequence) && target.structure_sequence > 0
        && Number.isSafeInteger(target.observed_ms) && Number.isSafeInteger(target.valid_until_ms)
        && target.valid_until_ms > target.observed_ms && target.valid_until_ms - target.observed_ms <= 2_000
        && (target.structure_sequence > finalSourceBaseline.structure_sequence
          || target.observed_ms > finalSourceBaseline.observed_ms)
        ? { projection, target } : false;
    }, 'final current-UI Source read of the owned fixture', 15_000);
  } catch (error) {
    if (lastFinalProjection) captureRootDiagnosticProjection(lastFinalProjection);
    result.final_source_gate = {
      status: 'unknown',
      activation_attempted: false,
      baseline_target: finalSourceBaseline,
      last_source_target: sourceTargetSummary(lastFinalProjection?.observation?.observed_target),
      projection_revision: Number.isSafeInteger(lastFinalProjection?.revision) ? lastFinalProjection.revision : null,
      failure: safeText(error?.message ?? error),
    };
    throw error;
  }
  result.final_source_gate = {
    status: 'passed',
    activation_attempted: false,
    baseline_target: finalSourceBaseline,
    source_target: sourceTargetSummary(current.target),
    projection_revision: Number.isSafeInteger(current.projection?.revision) ? current.projection.revision : null,
  };
  result.final_current_ui = {
    source_sequence: current.target.structure_sequence,
    observed_at_ms: current.target.observed_ms,
    snapshot_id: current.target.snapshot_id,
    pid: current.target.pid,
    app: current.target.app,
    window_id: current.target.window_id,
    target_matches_fixture: current.target.pid === fixturePid && current.target.app === bundleId,
    root_state: terminal.root.state,
  };
  result.current_ui_ready = true;
  result.source_target_matches_fixture = result.final_current_ui.target_matches_fixture;
  writeNewJson(finalReadPath, result.final_current_ui, 1024 * 1024);

  result.stop_status = 'unknown';
  result.stop_confirmed = false;
  const stopStatus = await requestConfirmedStop();
  result.stop_status = stopStatus;
  result.stop_confirmed = stopStatus === 'confirmed';
  assert.equal(result.stop_confirmed, true, 'Device Stop was not confirmed after the Root finished');

  const hostEvidence = await exportRootHostLedger(result.root_id, result.root_task_revision);
  assertGoalConfirmationRootBinding(result.goal_confirmation, hostEvidence.root, 'Host ledger Root');
  const frozenProfile = hostEvidence.root.frozen_profile;
  result.root_frozen_profile_matches = !!frozenProfile
    && frozenProfile.catalog_id === catalogId
    && frozenProfile.selector === selector
    && frozenProfile.suite === 'fixture'
    && frozenProfile.task_id === taskId
    && frozenProfile.seed === seed;
  result.status = result.root_state === 'completed' ? 'completed' : result.status;
}

async function requestConfirmedStop() {
  let settled = false;
  let unsubscribe = () => {};
  const statusPromise = new Promise(resolve => {
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      unsubscribe();
      resolve('unknown');
    }, 5_000);
    unsubscribe = window.desktop.onStopStatus(status => {
      if (status !== 'confirmed' && status !== 'unconfirmed') return;
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      unsubscribe();
      resolve(status);
    });
  });
  await js('window.desktop.stop()');
  return statusPromise;
}

function writeResult() {
  try { writeNewJson(resultPath, result); }
  catch { /* Outer wrapper reports a missing or malformed result. */ }
}

function finish(error = null, step = 'complete') {
  if (finishStarted) return;
  finishStarted = true;
  if (error) {
    result.status = 'failed';
    result.failure_step = /^[a-z0-9_-]{1,80}$/i.test(step) ? step : 'harness_error';
    result.rejection_reason = safeText(error?.message ?? error);
  }
  clearTimeout(timeoutHandle);
  writeResult();
  if (error) process.exitCode = 1;
  try { app.quit(); }
  catch { app.exit(error ? 1 : 0); }
  const fallback = setTimeout(() => app.exit(error ? 1 : 0), 15_000);
  fallback.unref();
}

function finalize(error = null, step = 'complete') {
  if (finalizationPromise) return finalizationPromise;
  finalizationPromise = (async () => {
    if (result.root_id && Number.isSafeInteger(result.root_task_revision)) {
      try {
        await exportRootHostLedger(result.root_id, result.root_task_revision);
      } catch (exportError) {
        result.host_ledger_export_failure = result.host_ledger_export_failure
          ?? safeText(exportError?.message ?? exportError);
      }
    }
    finish(error, step);
  })();
  return finalizationPromise;
}

app.setName('Computer Use');
app.on('browser-window-created', (_event, created) => {
  window = created;
  created.webContents.on('console-message', details => {
    if (details.level === 'error' && result.renderer_errors.length < 32) result.renderer_errors.push(safeText(details.message));
  });
  created.webContents.once('did-fail-load', (_event, code, description) => {
    if (code !== -3) void finalize(new Error(`renderer failed to load: ${safeText(description)}`), 'renderer_load');
  });
  assert.equal(typeof waitForNormalMainWindowVisible, 'function', 'normal Main visibility gate was not loaded');
  waitForNormalMainWindowVisible(created)
    .then(() => runProductFlow().then(() => finalize()).catch(error => finalize(error, 'product_flow')))
    .catch(error => {
      const diagnostics = error?.diagnostics;
      if (diagnostics && ['ready_to_show', 'renderer_loaded', 'show_event_seen', 'visible', 'destroyed']
        .every(key => typeof diagnostics[key] === 'boolean')) {
        result.normal_main_visibility = diagnostics;
      }
      return finalize(error, 'normal_main_visibility');
    });
});

process.on('uncaughtException', error => { void finalize(error, 'uncaught_exception'); });
process.on('unhandledRejection', error => { void finalize(error, 'unhandled_rejection'); });
process.once('exit', writeResult);
timeoutHandle = setTimeout(() => { void finalize(new Error('product task exceeded the harness deadline'), 'task_timeout'); }, 24 * 60 * 1000);
timeoutHandle.unref();

async function importNormalMain() {
  const readiness = await import(pathToFileURL(path.join(root, 'scripts/mvp/normal-main-window-gate.mjs')).href);
  waitForNormalMainWindowVisible = readiness.waitForNormalMainWindowVisible;
  const mainPath = path.join(root, 'apps/desktop/out/main/index.js');
  const stat = fs.lstatSync(mainPath);
  assert(stat.isFile() && !stat.isSymbolicLink(), 'Compiled normal Main entry is missing or unsafe.');
  assert.equal(fs.realpathSync(mainPath), mainPath, 'Compiled normal Main path contains a symbolic link.');
  await import(pathToFileURL(mainPath).href);
}

void importNormalMain().catch(error => { void finalize(error, 'normal_main_import'); });
