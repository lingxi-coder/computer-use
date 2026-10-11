const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawn, execFileSync } = require('node:child_process');
const { createHash, randomUUID } = require('node:crypto');
const { app, dialog } = require('electron');

const root = process.env.CU_OWNED_DOCUMENT_ROOT;
const output = process.env.CU_OWNED_DOCUMENT_OUTPUT;
const phase = process.env.CU_OWNED_DOCUMENT_PHASE;
const driver = process.env.CU_OWNED_DOCUMENT_DRIVER;
if (!root || !output || !driver || !['create', 'reopen'].includes(phase)) {
  throw new Error('Run the explicit owned-document authorization smoke.');
}

const key = `owned-document-consent-test-only-${randomUUID()}`;
const model = `owned-document-consent-${randomUUID()}`;
app.setName('Computer Use Owned Document Permission Smoke');
app.setPath('userData', path.join(output, 'user-data'));

const proof = {
  ok: false,
  checks: {},
  dialogs: [],
  renderer_errors: 0,
  failure_code: null,
  screenshot_sha256: null,
};
assert.equal(process.versions.electron,
  JSON.parse(fs.readFileSync(path.join(root, 'node_modules/electron/package.json'), 'utf8')).version);

let window;
let currentDialog = null;
let activeStep = 'startup';
let finished = false;
let ownedDirectories = [];
let hostDirectories = [];
const ledgerRows = [];
const removedOwnedDirectories = [];

function mark(name, condition) {
  assert.ok(condition, name);
  proof.checks[name] = true;
}

function captureLedger(directory) {
  const file = path.join(directory, 'host.db');
  if (!fs.existsSync(file)) {
    ledgerRows.push({ schema_available: false, root_tasks: -1, attempts: -1, s1_decision_model_calls: -1, s2_model_calls: -1, budget_attempts_used: -1 });
    return;
  }
  const source = [
    'import json,sqlite3,sys',
    'c=sqlite3.connect("file:"+sys.argv[1]+"?mode=ro",uri=True)',
    'names={r[0] for r in c.execute("select name from sqlite_master where type=\'table\'")}',
    'wanted={"root_tasks","attempts","model_calls","decision_model_calls","budget_ledger"}',
    'ok=wanted.issubset(names)',
    'count=lambda name: c.execute("select count(*) from "+name).fetchone()[0] if name in names else -1',
    'spent=c.execute("select coalesce(sum(attempts_used),0) from budget_ledger").fetchone()[0] if "budget_ledger" in names else -1',
    'print(json.dumps({"schema_available":ok,"root_tasks":count("root_tasks"),"attempts":count("attempts"),"s1_decision_model_calls":count("decision_model_calls"),"s2_model_calls":count("model_calls"),"budget_attempts_used":spent}))',
  ].join('\n');
  const row = JSON.parse(execFileSync('python3', ['-c', source, file], { encoding: 'utf8' }));
  ledgerRows.push(row);
}

const originalMkdtempSync = fs.mkdtempSync;
const originalRmSync = fs.rmSync;
const originalRmdirSync = fs.rmdirSync;
fs.mkdtempSync = (...args) => {
  const directory = originalMkdtempSync(...args);
  const prefix = args[0];
  if (typeof prefix === 'string' && path.basename(prefix) === 'cu') hostDirectories.push(directory);
  if (typeof prefix === 'string' && path.basename(prefix).startsWith('cu-owned-document-')) ownedDirectories.push(directory);
  return directory;
};
fs.rmSync = (...args) => {
  if (typeof args[0] === 'string' && hostDirectories.includes(args[0])) {
    try { captureLedger(args[0]); }
    catch { ledgerRows.push({ schema_available: false, root_tasks: -1, attempts: -1, s1_decision_model_calls: -1, s2_model_calls: -1, budget_attempts_used: -1 }); }
  }
  return originalRmSync(...args);
};
fs.rmdirSync = (...args) => {
  if (typeof args[0] === 'string' && ownedDirectories.includes(args[0])) removedOwnedDirectories.push(args[0]);
  return originalRmdirSync(...args);
};
require('node:module').syncBuiltinESMExports();

function delay(ms) { return new Promise(resolve => setTimeout(resolve, ms)); }
async function until(read, why, timeout = 15000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    const value = await read();
    if (value) return value;
    await delay(30);
  }
  throw new Error(`Timeout: ${why}`);
}

const js = code => window.webContents.executeJavaScript(code);
const owned = () => js('window.desktop.ownedDocumentStatus()');
const modelStatus = () => js('window.desktop.modelAuthorization()');

async function visibleOwnedButton(prefix) {
  const encoded = JSON.stringify(prefix.toLowerCase());
  return js(`(() => {
    const panel = document.querySelector('.owned-document');
    if (!panel || !panel.getClientRects().length || getComputedStyle(panel).visibility === 'hidden') return null;
    const button = [...panel.querySelectorAll('button')].find(e => e.textContent.trim().toLowerCase().startsWith(${encoded}));
    if (!button || !button.getClientRects().length || getComputedStyle(button).visibility === 'hidden') return null;
    return { enabled: !button.disabled };
  })()`);
}

async function clickOwned(prefix, checkEnabled = true) {
  const button = await until(async () => {
    const value = await visibleOwnedButton(prefix);
    return value && value.enabled === checkEnabled ? value : null;
  }, `owned-document button ${prefix} ready`);
  if (checkEnabled) mark(`ui_${prefix.toLowerCase().replace(/[^a-z0-9]+/g, '_')}_enabled`, button.enabled);
  else mark(`ui_${prefix.toLowerCase().replace(/[^a-z0-9]+/g, '_')}_disabled`, !button.enabled);
  if (button.enabled) {
    const encoded = JSON.stringify(prefix.toLowerCase());
    await js(`(() => [...document.querySelector('.owned-document').querySelectorAll('button')]
      .find(e => e.textContent.trim().toLowerCase().startsWith(${encoded})).click())()`);
  }
}

async function waitOwned(phaseName, why = phaseName) {
  return until(async () => {
    const status = await owned();
    return status.phase === phaseName ? status : null;
  }, why);
}

async function noTaskAndObservation(name, requireNoInput = false) {
  const snapshot = await until(() => js('window.desktop.snapshot()'), `${name} projection`);
  mark(`${name}_no_root`, snapshot.root === null);
  mark(`${name}_observe_mode`, snapshot.mode === 'observe');
  if (requireNoInput) mark(`${name}_no_input`, snapshot.capabilities?.input === 'unavailable');
  return snapshot;
}

function consentDetails(kind) {
  return {
    'owned-edit': {
      message: 'Allow Computer Use to edit its temporary TextEdit document?',
      button: 'Allow editing',
    },
    'owned-save': {
      message: 'Allow Computer Use to save and reopen its temporary TextEdit document?',
      button: 'Allow saving and reopening',
    },
    model: {
      message: 'Allow Computer Use to use this model for the selected purposes?',
      button: 'Allow selected purposes',
    },
  }[kind];
}

const originalShowMessageBox = dialog.showMessageBox.bind(dialog);
dialog.showMessageBox = async (...args) => {
  const options = args.at(-1);
  const intent = currentDialog;
  if (!intent) throw new Error('Unexpected native dialog.');
  const exact = consentDetails(intent.kind);
  assert.ok(exact && options.message === exact.message);
  assert.equal(options.cancelId, 0);
  assert.equal(options.defaultId, 0);
  assert.deepEqual(options.buttons, ['Cancel', exact.button]);
  assert.ok(!String(options.detail).includes(key));
  if (intent.kind === 'owned-edit' || intent.kind === 'owned-save') {
    assert.ok(!/(\/private\/|\/Users\/|\/var\/folders\/|document_path|registration|sha256|document\.txt|document body)/i.test(String(options.detail)));
  }
  if (intent.kind === 'model') {
    assert.ok(options.detail.includes('https://example.invalid/v1'));
    assert.ok(options.detail.includes(model));
    assert.ok(options.detail.includes('S2 planning:'));
    assert.ok(!options.detail.includes('S1 teacher:'));
  }

  const responsePromise = originalShowMessageBox(...args);
  let result;
  if (intent.action === 'stop') {
    await delay(150);
    const stopEnabled = await js('(() => { const b = document.querySelector("button.stop"); return !!b && !b.disabled; })()');
    assert.ok(stopEnabled, 'Stop remains available while consent is pending.');
    await js('document.querySelector("button.stop").click()');
    result = await responsePromise;
    assert.equal(result.response, 0, 'Stop aborts pending consent with Cancel.');
    proof.dialogs.push({ kind: intent.kind, action: intent.action, response: 0, real_button_pressed: false });
  } else {
    const driverResult = await new Promise((resolve, reject) => {
      const child = spawn(driver, [intent.action, intent.kind], { stdio: ['ignore', 'pipe', 'ignore'], timeout: 10000 });
      let stdout = '';
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', value => { stdout += value; });
      child.once('error', reject);
      child.once('close', code => {
        if (code !== 0) return reject(new Error('Native consent driver failed.'));
        try { resolve(JSON.parse(stdout)); } catch { reject(new Error('Native consent driver returned invalid status.')); }
      });
    });
    result = await responsePromise;
    assert.equal(driverResult.status, 'pressed');
    assert.equal(driverResult.button, intent.action === 'allow' ? exact.button : 'Cancel');
    assert.equal(result.response, intent.action === 'allow' ? 1 : 0);
    proof.dialogs.push({ kind: intent.kind, action: intent.action, response: result.response, real_button_pressed: true });
  }
  currentDialog = null;
  return result;
};

async function authorizeOwned(kind, action) {
  currentDialog = { kind, action };
  const previous = proof.dialogs.length;
  const buttonPrefix = kind === 'owned-edit' ? 'Allow editing' : 'Allow saving';
  await clickOwned(buttonPrefix);
  await until(() => proof.dialogs.length > previous, `${kind} native consent`);
}

async function fillModelForm() {
  await js(`(() => {
    const form = document.querySelector('.connection-form');
    if (!form) throw new Error('Model connection form is unavailable.');
    form.open = true;
    const set = (label, value) => {
      const e = document.querySelector('input[aria-label="' + label + '"]');
      if (!e) throw new Error('Expected model field is unavailable.');
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(e, value);
      e.dispatchEvent(new Event('input', { bubbles: true }));
    };
    set('Model endpoint', 'https://example.invalid/v1');
    set('Model identifier', ${JSON.stringify(model)});
    set('Model API key', ${JSON.stringify(key)});
    const remember = document.querySelector('.remember-key input');
    if (remember && remember.checked) remember.click();
    const planning = document.querySelector('input[aria-label="S2 task planning purpose"]');
    const teacher = document.querySelector('input[aria-label="S1 semantic decision purpose"]');
    if (!planning || !teacher) throw new Error('Model purpose choices are unavailable.');
    if (!planning.checked) planning.click();
    if (teacher.checked) teacher.click();
    document.querySelector('.model-connection').scrollIntoView();
  })()`);
  await until(() => js('!document.querySelector(".connection-form button.primary").disabled'), 'model form ready');
}

async function authorizeS2Model() {
  await fillModelForm();
  currentDialog = { kind: 'model', action: 'allow' };
  const previous = proof.dialogs.length;
  await js('document.querySelector(".connection-form form").requestSubmit()');
  await until(() => proof.dialogs.length > previous, 'S2 model consent');
  await until(async () => !['authorizing', 'switching'].includes((await modelStatus()).phase), 'model session settled');
  mark('model_key_cleared', await js(`document.querySelector('input[aria-label="Model API key"]').value === ''`));
  mark('model_key_not_rendered', !(await js(`document.body.innerText.includes(${JSON.stringify(key)})`)));
  const status = await modelStatus();
  mark('s2_only_authorized', status.phase === 'authorized' && status.storage === 'memory'
    && JSON.stringify(status.authorized_purposes) === '["s2_planning"]' && status.planning_available);
  await noTaskAndObservation('after_s2_consent');
}

async function verifyRendererBoundary() {
  const facts = await js(`(() => {
    const keys = Object.keys(window.desktop);
    return {
      keysSafe: keys.every(key => !/(path|registration|hash|body|api.?key|file)/i.test(key)),
      hasNoSensitiveNames: !keys.some(key => /(path|registration|hash|body)/i.test(key)),
    };
  })()`);
  mark('desktop_api_has_no_private_document_fields', facts.keysSafe && facts.hasNoSensitiveNames);
  const status = await owned();
  mark('owned_status_has_no_private_document_fields', !['path', 'registration', 'hash', 'body', 'document_path', 'sha256']
    .some(field => Object.hasOwn(status, field)));
  mark('owned_status_has_no_private_document_identity', !/(\/private\/|\/Users\/|\/var\/folders\/|document_path|registration|sha256)/i.test(JSON.stringify(status)));
}

async function createPhase() {
  activeStep = 'create_initial';
  const initial = await until(async () => {
    const status = await owned();
    return status.phase === 'not_created' ? status : null;
  }, 'initial owned-document state');
  mark('fresh_document_not_created', initial.can_create && !initial.can_authorize_edit && !initial.can_authorize_save && !initial.can_revisit);
  await clickOwned('Create temporary document');
  await waitOwned('observing', 'created TextEdit observation');
  await noTaskAndObservation('created_document', true);
  const observed = await owned();
  mark('create_has_no_document_grants', observed.phase === 'observing'
    && !observed.can_authorize_save && !observed.can_revisit);
  await verifyRendererBoundary();

  activeStep = 'model_s2_authorize';
  await authorizeS2Model();

  activeStep = 'stop_pending_edit';
  currentDialog = { kind: 'owned-edit', action: 'stop' };
  const beforeStop = proof.dialogs.length;
  await clickOwned('Allow editing');
  await until(() => proof.dialogs.length > beforeStop, 'Stop cancellation of pending edit consent');
  const afterStop = await waitOwned('observing', 'edit permission remains closed after Stop');
  mark('stop_pending_edit_did_not_grant', !afterStop.can_authorize_save && !afterStop.can_revisit);

  activeStep = 'edit_cancel';
  await authorizeOwned('owned-edit', 'cancel');
  await waitOwned('observing', 'cancelled edit consent');
  mark('edit_cancel_keeps_grant_closed', !(await owned()).can_authorize_save);

  activeStep = 'edit_allow';
  await authorizeOwned('owned-edit', 'allow');
  await waitOwned('edit_authorized', 'approved edit consent');
  const editOnly = await owned();
  mark('edit_approval_is_separate', editOnly.phase === 'edit_authorized'
    && editOnly.can_authorize_save && !editOnly.can_revisit);

  activeStep = 'save_cancel';
  await authorizeOwned('owned-save', 'cancel');
  const afterSaveCancel = await waitOwned('edit_authorized', 'cancelled save consent preserves edit only');
  mark('save_cancel_keeps_save_closed', afterSaveCancel.can_authorize_save && !afterSaveCancel.can_revisit);

  activeStep = 'save_allow';
  await authorizeOwned('owned-save', 'allow');
  await waitOwned('save_authorized', 'approved save consent');
  const saveAuthorized = await owned();
  mark('save_approval_is_separate', saveAuthorized.phase === 'save_authorized' && !saveAuthorized.can_revisit);

  activeStep = 'reopen_unavailable_without_saved_version';
  const reopen = await until(() => visibleOwnedButton('Reopen'), 'visible Reopen to verify control');
  mark('reopen_disabled_without_saved_version', !reopen.enabled && !(await owned()).can_revisit);

  activeStep = 'revoke_edit';
  const revokeVisible = await js(`(() => { const b=document.querySelector('.owned-document-revoke'); return !!b && b.getClientRects().length>0 && !b.disabled; })()`);
  mark('edit_revoke_control_visible', revokeVisible);
  await js('document.querySelector(".owned-document-revoke").click()');
  await waitOwned('observing', 'edit and save grants revoked');
  mark('revoke_edit_clears_save', !(await owned()).can_authorize_save && !(await owned()).can_revisit);
  const modelAfterEditRevoke = await until(async () => {
    const value = await modelStatus();
    return value.phase === 'authorized' ? value : null;
  }, 'S2 consent remains explicit after editing permission is revoked');
  mark('edit_revoke_preserves_s2_consent', JSON.stringify(modelAfterEditRevoke.authorized_purposes) === '["s2_planning"]');

  activeStep = 'edit_reapprove';
  await authorizeOwned('owned-edit', 'allow');
  await waitOwned('edit_authorized', 'edit reapproved before model revoke case');
  activeStep = 'save_reapprove';
  await authorizeOwned('owned-save', 'allow');
  await waitOwned('save_authorized', 'save reapproved before model revoke case');

  activeStep = 'model_revoke';
  await js('document.querySelector(".connection-revoke").click()');
  await until(async () => (await modelStatus()).phase === 'not_configured', 'model consent revoked');
  const afterModelRevoke = await waitOwned('observing', 'model revoke clears edit and save permissions');
  mark('revoke_model_clears_edit_and_save', !afterModelRevoke.can_authorize_save && !afterModelRevoke.can_revisit);
  await noTaskAndObservation('after_model_revoke', true);
  await verifyRendererBoundary();
}

async function reopenPhase() {
  activeStep = 'fresh_process_state';
  const status = await until(async () => {
    const value = await owned();
    return value.phase === 'not_created' ? value : null;
  }, 'new app owned-document state');
  mark('new_app_restores_no_document_permission', status.can_create && !status.can_authorize_edit
    && !status.can_authorize_save && !status.can_revisit);
  const model = await modelStatus();
  mark('new_app_restores_no_model_consent', model.phase === 'not_configured'
    && model.authorized_purposes.length === 0);
  await noTaskAndObservation('new_app');
  await until(() => visibleOwnedButton('Create temporary document'), 'visible owned-document panel in the new app');
  await verifyRendererBoundary();
}

async function finish(error, code) {
  if (finished) return;
  finished = true;
  proof.ok = !error;
  proof.failure_code = error ? code || activeStep : null;
  try {
    if (window && !window.isDestroyed()) {
      const clean = await js(`(() => {
        const keyInput = document.querySelector('input[aria-label="Model API key"]');
        return (!keyInput || keyInput.value === '') && !document.body.innerText.includes(${JSON.stringify(key)});
      })()`);
      mark('secret_absent_before_capture', clean);
      const png = (await window.webContents.capturePage()).toPNG();
      proof.screenshot_sha256 = createHash('sha256').update(png).digest('hex');
      fs.writeFileSync(path.join(output, `${phase}-window.png`), png, { flag: 'wx' });
    }
  } catch {
    proof.ok = false;
    proof.failure_code ||= 'screenshot_or_secret_check';
  }
  try { fs.writeFileSync(path.join(output, `${phase}-result.json`), JSON.stringify(proof, null, 2), { flag: 'wx' }); }
  catch { proof.ok = false; }
  app.quit();
  setTimeout(() => app.exit(proof.ok ? 0 : 1), 7000).unref();
}

process.on('uncaughtException', error => void finish(error));
process.on('unhandledRejection', error => void finish(error));
process.on('SIGTERM', () => void finish(new Error('Smoke interrupted.'), 'interrupted'));
process.on('exit', () => {
  try {
    fs.writeFileSync(path.join(output, `${phase}-internal.json`), JSON.stringify({
      owned_directories: ownedDirectories,
      removed_owned_directories: removedOwnedDirectories,
      host_directory_count: hostDirectories.length,
      ledger_rows: ledgerRows,
    }), { flag: 'wx' });
  } catch { /* The wrapper treats a missing private record as failed cleanup evidence. */ }
});
setTimeout(() => void finish(new Error('Smoke timed out.'), 'timeout'), 76000).unref();

app.on('browser-window-created', (_event, created) => {
  window = created;
  const contents = created.webContents;
  contents.on('console-message', details => { if (details.level === 'error') proof.renderer_errors += 1; });
  contents.on('preload-error', () => { proof.renderer_errors += 1; });
  contents.once('did-finish-load', () => void (async () => {
    activeStep = 'renderer_ready';
    await until(() => js("!!window.desktop && !!document.querySelector('.owned-document')"), 'owned-document panel rendered');
    await until(async () => ['not_created', 'observing'].includes((await owned()).phase), 'owned-document API initialized');
    if (phase === 'create') await createPhase();
    else await reopenPhase();
    activeStep = 'renderer_checks';
    mark('renderer_has_no_errors', proof.renderer_errors === 0);
    proof.ok = true;
    await finish(null);
  })().catch(error => finish(error)));
});

void (async () => {
  const env = process.env;
  env.CU_BIN_DIR = path.join(root, 'target/debug');
  delete env.CU_SOURCE_BACKEND;
  delete env.CU_TARGET_BUNDLE_ID;
  delete env.CU_TARGET_PID;
  delete env.CU_NATIVE_REFERENCE_PLANNER;
  delete env.ELECTRON_RENDERER_URL;
  await import(path.join(root, 'apps/desktop/out/main/index.js'));
})().catch(error => finish(error, 'main_startup'));
