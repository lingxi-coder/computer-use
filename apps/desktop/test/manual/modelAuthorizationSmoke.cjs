const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawn, execFile, execFileSync } = require('node:child_process');
const { app, dialog } = require('electron');
const root = process.env.CU_MODEL_AUTH_ROOT, output = process.env.CU_MODEL_AUTH_OUTPUT;
const model = process.env.CU_MODEL_AUTH_MODEL, phase = process.env.CU_MODEL_AUTH_PHASE;
if (!root || !output || !model || !['create', 'reopen'].includes(phase)) throw new Error('Run the explicit owned model smoke.');
const key = 'computer-use-owned-model-consent-test-only';
app.setName('Computer Use Model Permission Smoke'); app.setPath('userData', path.join(output, 'user-data'));
const proof = { checks: [], dialogs: [], unexpected_connection_dialogs: [], console_errors: [], electron: process.versions.electron, live_provider_verified: false };
assert.equal(proof.electron, JSON.parse(fs.readFileSync(path.join(root, 'node_modules/electron/package.json'), 'utf8')).version);
const fixture = spawn(path.join(root, 'target/native-macos/GuardFixture.app/Contents/MacOS/GuardFixture'), ['--text-mode'], { stdio: ['pipe', 'pipe', 'pipe'] });
fixture.stderr.resume(); fixture.stdin.on('error', () => {});
let fixtureReady = false, buffer = '', finished = false, dialogAction = 'cancel', window;
const directories = [];
proof.ledger_call_counts = [];
proof.ledger_errors = [];
proof.purpose_runs = [];
function captureLedger(directory) {
  const file = path.join(directory, 'host.db');
  if (!fs.existsSync(file)) return;
  const source = 'import sqlite3,sys,json\nc=sqlite3.connect("file:"+sys.argv[1]+"?mode=ro",uri=True)\nprint(json.dumps({"model_calls":c.execute("select count(*) from model_calls").fetchone()[0],"decision_model_calls":c.execute("select count(*) from decision_model_calls").fetchone()[0]}))';
  const calls = JSON.parse(execFileSync('python3', ['-c', source, file], { encoding: 'utf8' }));
  proof.ledger_call_counts.push({ directory, ...calls });
}
const createDirectory = fs.mkdtempSync, removeDirectory = fs.rmSync;
fs.mkdtempSync = (...args) => {
  const directory = createDirectory(...args);
  if (typeof args[0] === 'string' && path.basename(args[0]) === 'cu') directories.push(directory);
  return directory;
};
fs.rmSync = (...args) => {
  if (directories.includes(args[0])) {
    try { captureLedger(args[0]); }
    catch (error) { proof.ledger_errors.push({ directory: args[0], error: String(error) }); }
  }
  return removeDirectory(...args);
};
require('node:module').syncBuiltinESMExports();
fixture.stdout.on('data', chunk => { buffer += chunk; let i; while ((i = buffer.indexOf('\n')) >= 0) {
  const row = JSON.parse(buffer.slice(0, i)); buffer = buffer.slice(i + 1); if (row.kind === 'ready') fixtureReady = true;
} });
async function until(read, why, timeout = 15000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const v = await read(); if (v) return v; await new Promise(done => setTimeout(done, 30)); }
  throw new Error('Timeout: ' + why);
}
function check(value, why) { assert.ok(value, why); proof.checks.push(why); }
const js = code => window.webContents.executeJavaScript(code);
const state = () => js('window.desktop.modelAuthorization()');
const showDialog = dialog.showMessageBox.bind(dialog);
dialog.showMessageBox = async (...args) => {
  const options = args.at(-1);
  const connectionTitles = ['Allow selected model purposes?', 'Allow OpenAI account model purposes?', 'Allow provider connection?'];
  if (connectionTitles.includes(options.title)) {
    proof.unexpected_connection_dialogs.push(options.title);
    throw new Error('The explicit Connect action must not open a second connection confirmation dialog.');
  }
  check(options.cancelId === 0 && options.defaultId === 0, 'native consent defaults to Cancel');
  check(!options.detail.includes(key), 'native consent contains no credential');
  check(options.detail.includes('Process ' + fixture.pid), 'window consent identifies the owned fixture');
  const response = showDialog(...args);
  const pressed = new Promise((resolve, reject) => execFile(path.join(root, 'target/native-macos/consent-dialog-driver'), [dialogAction, 'window'], { timeout: 10000 }, (error, stdout) => {
    if (error) reject(error); else resolve(JSON.parse(stdout));
  }));
  const [result, driver] = await Promise.all([response, pressed]);
  check(driver.status === 'pressed', 'real native permission button was pressed in the owned parent');
  proof.dialogs.push({ kind: 'window', button: driver.button, response: result.response }); return result;
};
async function fillForm(remember = false, purposes = ['s2_planning']) {
  await js(`(() => {
    document.querySelector('.connection-form').open = true;
    const set = (label, value) => { const e = document.querySelector('input[aria-label="'+label+'"]'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(e, value); e.dispatchEvent(new Event('input', { bubbles: true })); };
    set('Model endpoint', 'https://example.invalid/v1'); set('Model identifier', ${JSON.stringify(model)}); set('Model API key', ${JSON.stringify(key)});
    const remember = document.querySelector('.remember-key input'); if (remember.checked !== ${remember}) remember.click();
    const choosePurpose = (label, wanted) => { const e = document.querySelector('input[aria-label="'+label+'"]'); if (e.checked !== wanted) e.click(); };
    choosePurpose('S2 task planning purpose', ${JSON.stringify(purposes.includes('s2_planning'))});
    choosePurpose('S1 semantic decision purpose', ${JSON.stringify(purposes.includes('s1_teacher'))});
    if (document.querySelector('input[aria-label="S2 task planning purpose"]').checked !== ${JSON.stringify(purposes.includes('s2_planning'))}
      || document.querySelector('input[aria-label="S1 semantic decision purpose"]').checked !== ${JSON.stringify(purposes.includes('s1_teacher'))}) throw new Error('Purpose checkboxes did not match the requested selection');
    document.querySelector('.model-connection').scrollIntoView();
  })()`);
  await until(() => js('!document.querySelector(".connection-form button.primary").disabled'), 'model form ready');
}
async function submit(remember = false, purposes = ['s2_planning']) {
  await fillForm(remember, purposes);
  const dialogCount = proof.dialogs.length;
  await js('document.querySelector(".connection-form form").requestSubmit()');
  await until(async () => !['authorizing', 'switching'].includes((await state()).phase), 'model session settled');
  check(proof.dialogs.length === dialogCount, 'selected-purpose connection does not open another native confirmation dialog');
  check(proof.unexpected_connection_dialogs.length === 0, 'no model or provider connection confirmation dialog was requested');
  check(await js(`document.querySelector('input[aria-label="Model API key"]').value === ''`), 'submitted API key is cleared from the form');
}
async function revoke() {
  await js('document.querySelector(".connection-revoke").click()');
  await until(async () => (await state()).phase === 'not_configured', 'model revoked');
}
async function noTaskOrInput() {
  const [s, native, modelStatus] = await Promise.all([js('window.desktop.snapshot()'), js('window.desktop.nativeAuthorization()'), state()]);
  check(s.root === null && s.mode === 'observe' && s.capabilities.input === 'unavailable', 'model changes create no task and return to observation without input');
  check(native.phase === 'observing' && native.authorized === null, 'model authorization does not grant window input');
  check(!JSON.stringify({ s, modelStatus }).includes(key), 'projections contain no API key');
}
async function finish(error) {
  if (finished) return; finished = true; proof.ok = !error;
  if (error) proof.error = String(error.stack || error);
  try {
    if (window && !window.isDestroyed()) fs.writeFileSync(path.join(output, phase + '-window.png'), (await window.webContents.capturePage()).toPNG());
    for (const directory of directories) captureLedger(directory);
    check(proof.ledger_errors.length === 0 && proof.ledger_call_counts.length > 0
      && proof.ledger_call_counts.every(r => r.model_calls === 0 && r.decision_model_calls === 0),
      'all real host ledgers contain zero S2 and S1 model calls');
  } catch (value) { proof.ok = false; proof.error ??= String(value); }
  fs.writeFileSync(path.join(output, phase + '-result.json'), JSON.stringify(proof, null, 2));
  fixture.kill('SIGKILL'); app.quit(); setTimeout(() => app.exit(proof.ok ? 0 : 1), 5000).unref();
}
process.on('uncaughtException', error => void finish(error));
process.on('SIGTERM', () => void finish(new Error('Owned model smoke terminated')));
setTimeout(() => void finish(new Error('Owned model smoke timed out')), 80000).unref();
app.on('browser-window-created', (_event, created) => {
  window = created; const contents = window.webContents;
  contents.on('console-message', details => { if (details.level === 'error') proof.console_errors.push(details.message); });
  contents.on('preload-error', (_event, _file, error) => proof.console_errors.push(String(error)));
  contents.once('did-finish-load', () => void (async () => {
    await until(async () => (await state()).can_authorize && (await js('window.desktop.nativeAuthorization()')).can_authorize, 'fresh owned observation and model settings');
    const initial = await state();
    check(initial.phase === 'not_configured' && initial.route === null && initial.authorized_purposes.length === 0
      && !initial.planning_available && initial.can_save, 'startup does not restore saved purpose consent or an environment key');
    if (phase === 'create') {
      check(initial.saved.length === 0, 'isolated product profile starts without saved keys');
      dialogAction = 'allow'; await js('document.querySelector(".native-authorization button.primary").click()');
      await until(() => js('window.desktop.nativeAuthorization().then(s => s.phase === "authorized")'), 'owned window permission');
      await submit(false, ['s2_planning']);
      check((await state()).phase === 'authorized' && (await state()).storage === 'memory', 'the explicit Connect action opens the memory model connection');
      check((await state()).planning_available && JSON.stringify((await state()).authorized_purposes) === '["s2_planning"]', 'the selected purpose authorizes only S2 planning');
      proof.purpose_runs.push({ selected: ['s2_planning'], authorized: (await state()).authorized_purposes, planning_available: (await state()).planning_available });
      await noTaskOrInput(); await revoke();
      check((await state()).authorized_purposes.length === 0 && !(await state()).planning_available, 'revoking S2 clears its in-memory purpose consent');
      await submit(false, ['s1_teacher']);
      const teacherOnly = await state();
      check(teacherOnly.phase === 'authorized' && !teacherOnly.planning_available
        && JSON.stringify(teacherOnly.authorized_purposes) === '["s1_teacher"]', 'an S1-only connection cannot enable S2 planning');
      proof.purpose_runs.push({ selected: ['s1_teacher'], authorized: teacherOnly.authorized_purposes, planning_available: teacherOnly.planning_available });
      await noTaskOrInput(); await revoke();
      check((await state()).authorized_purposes.length === 0, 'revoking S1 clears its in-memory purpose consent');
      await submit(false, ['s2_planning', 's1_teacher']);
      const both = await state();
      check(both.phase === 'authorized' && both.planning_available
        && JSON.stringify(both.authorized_purposes) === '["s2_planning","s1_teacher"]', 'the connection applies both explicitly selected purposes');
      proof.purpose_runs.push({ selected: ['s2_planning', 's1_teacher'], authorized: both.authorized_purposes, planning_available: both.planning_available });
      await noTaskOrInput(); await revoke();
      check((await state()).authorized_purposes.length === 0, 'revoking combined consent clears both purposes');
      await submit(true, ['s2_planning']);
      const saved = await state();
      check(saved.phase === 'authorized' && saved.storage === 'keychain' && saved.saved.length === 1, 'owned dummy key is saved through the product form and native Keychain helper');
      const file = path.join(output, 'user-data/model-access/connections.json');
      check(!fs.readFileSync(file, 'utf8').includes(key), 'disk metadata contains routing facts without the API key');
      check(!fs.readFileSync(file, 'utf8').includes('s1_teacher') && !fs.readFileSync(file, 'utf8').includes('s2_planning'), 'saved route metadata contains no purpose consent');
      check((fs.statSync(file).mode & 0o777) === 0o600, 'saved route metadata has private file permissions');
      await noTaskOrInput(); proof.saved_id = saved.saved[0].id;
    } else {
      check(initial.saved.length === 1 && initial.saved[0].route.model === model, 'saved routing facts survive quitting without restoring model authorization');
      check(initial.authorized_purposes.length === 0 && !initial.planning_available, 'reopen restores no purpose authorization');
      check(await js('document.querySelector(\'input[aria-label="S2 task planning purpose"]\').checked && !document.querySelector(\'input[aria-label="S1 semantic decision purpose"]\').checked'),
        'saved-key reconnect starts with only the default S2 choice selected');
      await js('document.querySelector(".saved-connection button").click()');
      await until(async () => (await state()).phase === 'authorized', 'saved key loaded after the explicit Connect action');
      check((await state()).planning_available && JSON.stringify((await state()).authorized_purposes) === '["s2_planning"]', 'saved-key reconnect applies only the selected S2 purpose');
      await noTaskOrInput(); proof.saved_id = initial.saved[0].id;
      await js('document.querySelector(".saved-connection button:last-child").click()');
      await until(async () => (await state()).saved.length === 0 && (await state()).phase === 'not_configured', 'Forget closes the session and deletes the owned saved key');
      check((await state()).authorized_purposes.length === 0, 'forget/revoke leaves no in-memory purpose consent');
      const reply = JSON.parse(execFileSync(path.join(root, 'target/debug/cu-macos-keychain'), [], { input: JSON.stringify({ operation: 'load', account: initial.saved[0].id }), encoding: 'utf8' }));
      check(reply.ok && reply.key == null, 'the exact owned test account is absent from Keychain after Forget');
    }
    check(proof.unexpected_connection_dialogs.length === 0, 'connection flows use the explicit action and selected purposes without a second dialog');
    check(await js('document.documentElement.scrollWidth <= window.innerWidth'), 'model settings fit the narrow product window without horizontal overflow');
    check(proof.console_errors.length === 0, 'renderer, preload and CSP report no errors');
    proof.native_dialog_rendering_verified = true; proof.product_model_authorization_verified = true;
    await finish();
  })().catch(error => finish(error)));
});
void (async () => {
  await until(() => fixtureReady, 'owned fixture');
  process.env.CU_BIN_DIR = path.join(root, 'target/debug'); process.env.CU_SOURCE_BACKEND = 'native';
  process.env.CU_TARGET_BUNDLE_ID = 'com.lingxi.computer-user.guard-fixture'; process.env.CU_TARGET_PID = String(fixture.pid);
  delete process.env.CU_NATIVE_REFERENCE_PLANNER; delete process.env.ELECTRON_RENDERER_URL;
  await import(path.join(root, 'apps/desktop/out/main/index.js'));
})().catch(error => finish(error));
