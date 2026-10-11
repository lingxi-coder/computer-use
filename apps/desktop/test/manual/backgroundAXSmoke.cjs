// Owned diagnostic through Main consent, frozen Goal and the normal Root/Attempt lifecycle.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { spawn, execFile, execFileSync } = require('node:child_process');
const { app, dialog } = require('electron');

const root = process.env.CU_BACKGROUND_ROOT;
const output = process.env.CU_BACKGROUND_OUTPUT;
const guard = process.env.CU_NATIVE_DEVICE_GUARD;
if (process.env.CU_BACKGROUND_EXECUTE !== '1' || !path.isAbsolute(root || '')
    || !path.isAbsolute(output || '') || !path.basename(guard || '').startsWith('owned-background-')) {
  throw new Error('Use the explicit owned Background AX smoke launcher.');
}
app.setName('Computer Use Background AX Diagnostic');
app.setPath('userData', path.join(output, 'user-data'));
const bundle = 'com.lingxi.computer-user.guard-fixture';
const proof = { kind: 'background_ax_root_diagnostic', ok: false, business_gate_eligible: false,
  mvp_complete: false, live_provider_calls: 0, native_ax_call_count: { status: 'unknown', count: null } };
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
let fixture, fixtureReady, window, session, finished = false, archiveTimer;
let sourceBytes = 0;
const sourceHistory = new Map();
const fixtureSnapshot = path.join(output, 'fixture-state.json');
const diagnostics = path.join(output, 'native-receipts');
fs.mkdirSync(diagnostics, { mode: 0o700 });
const stderrWrite = process.stderr.write.bind(process.stderr);
process.stderr.write = (...args) => {
  const match = /cu-source: native watch manifest ([^\n]+)\/native-assets\//.exec(String(args[0]));
  if (match) session = match[1];
  return stderrWrite(...args);
};
async function until(read, label, timeout = 15000) {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) {
    try { const value = await read(); if (value) return value; } catch (error) { last = error; }
    await new Promise(resolve => setTimeout(resolve, 40));
  }
  throw new Error(`Timeout: ${label}${last ? ` (${last})` : ''}`);
}
function stableJson(file) {
  const before = fs.lstatSync(file);
  assert.ok(before.isFile() && !before.isSymbolicLink() && before.uid === process.getuid()
    && before.nlink === 1 && before.size > 0 && before.size <= 1024 * 1024);
  const bytes = fs.readFileSync(file), after = fs.lstatSync(file);
  assert.ok(before.ino === after.ino && before.size === bytes.length && after.size === bytes.length
    && before.mtimeMs === after.mtimeMs && before.ctimeMs === after.ctimeMs);
  return { value: JSON.parse(bytes), bytes, sha256: hash(bytes) };
}
function captureSource() {
  if (!session) return;
  const base = path.join(session, 'native-assets');
  if (!fs.existsSync(base)) return;
  const realBase = fs.realpathSync(base);
  const queue = [{ dir: realBase, depth: 0 }];
  let visited = 0;
  while (queue.length && visited < 256) {
    const current = queue.shift();
    if (current.depth > 4) continue;
    for (const entry of fs.readdirSync(current.dir, { withFileTypes: true })) {
      if (++visited > 256) break;
      const file = path.join(current.dir, entry.name);
      if (entry.isDirectory()) { queue.push({ dir: file, depth: current.depth + 1 }); continue; }
      if (!entry.isFile() || !['manifest.json', 'latest.json', 'watch-manifest.json'].includes(entry.name)) continue;
      try {
        let item = stableJson(file);
        if (typeof item.value.manifest === 'string') {
          const referenced = fs.realpathSync(path.resolve(current.dir, item.value.manifest));
          if (!referenced.startsWith(realBase + path.sep)) continue;
          item = stableJson(referenced);
        }
        const snapshot = item.value.snapshot;
        if (snapshot?.app?.bundle_id !== bundle || snapshot.app.pid !== fixture.pid
            || !snapshot.snapshot_id || sourceHistory.has(snapshot.snapshot_id)) continue;
        const bytes = Buffer.from(JSON.stringify(snapshot));
        if (sourceHistory.size >= 256 || sourceBytes + bytes.length > 32 * 1024 * 1024) {
          proof.source_archive_truncated = true; continue;
        }
        const archive = path.join(output, 'source-snapshots');
        fs.mkdirSync(archive, { recursive: true, mode: 0o700 });
        const filename = hash(Buffer.from(snapshot.snapshot_id)) + '.json';
        fs.writeFileSync(path.join(archive, filename), bytes, { flag: 'wx', mode: 0o600 });
        sourceBytes += bytes.length;
        sourceHistory.set(snapshot.snapshot_id, { snapshot, path: path.join(archive, filename), sha256: hash(bytes) });
      } catch { /* Rotating or removed files are not evidence. */ }
    }
  }
}
function ledger() {
  if (!session) return null;
  return JSON.parse(execFileSync('python3', ['-c',
    'import sqlite3,json,sys\nc=sqlite3.connect("file:"+sys.argv[1]+"?mode=ro",uri=True)\nc.row_factory=sqlite3.Row\nd={t:[dict(r) for r in c.execute("select * from "+t)] for t in ["root_tasks","attempts","decisions","attempt_map","admission_outbox","effect_results"]}\nfor r in d["effect_results"]:r["result"]=json.loads(r.pop("json"))\nprint(json.dumps(d))',
    path.join(session, 'host.db')], { encoding: 'utf8', timeout: 2000 }));
}
function receipts() {
  return fs.readdirSync(diagnostics).filter(name => name.endsWith('.json'))
    .map(name => ({ filename: name, ...stableJson(path.join(diagnostics, name)).value }));
}
function focus() {
  // The foreground app may be this Electron parent. Its event loop must remain
  // free to answer the probe's real AX requests.
  return new Promise((resolve, reject) => execFile(path.join(root, 'target/native-macos/background-focus-probe'), [],
    { encoding: 'utf8', timeout: 2000 }, (error, stdout, stderr) => {
      if (error) return reject(new Error(`Independent focus read failed: ${String(stderr || error.message).trim()}`));
      try { resolve(JSON.parse(stdout)); } catch (error) { reject(error); }
    }));
}
async function finish(error) {
  if (finished) return;
  finished = true;
  if (archiveTimer) clearInterval(archiveTimer);
  proof.ok = !error;
  if (error) proof.error = error.stack || String(error);
  try { captureSource(); proof.ledger ??= ledger(); } catch (error) { proof.ledger_error = String(error); }
  try { proof.native_receipts = receipts(); } catch (error) { proof.receipt_error = String(error); }
  if (proof.focus_before && !proof.focus_after) {
    try { proof.focus_after = await focus(); } catch (error) { proof.focus_after_error = String(error); }
  }
  const submits = (proof.native_receipts || []).filter(r => r.category === 'ax-press-submit-receipt');
  if (submits.length === 1 && submits[0].submitted === true && submits[0].ax_trace?.ax_call_count === 1) {
    proof.native_ax_call_count = { status: 'verified', count: 1 };
  }
  try { proof.final_fixture = stableJson(fixtureSnapshot).value; } catch (error) { proof.fixture_error = String(error); }
  if (window && !window.isDestroyed()) {
    try { proof.final_projection = await window.webContents.executeJavaScript('window.desktop.snapshot()'); } catch {}
    try {
      const state = await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()');
      if (state.phase === 'authorized') await window.webContents.executeJavaScript('window.desktop.revokeNativeWindow()');
    } catch (error) { proof.cleanup_error = String(error); proof.ok = false; }
  }
  proof.source_snapshots = [...sourceHistory.values()].map(({ snapshot, ...entry }) => ({
    ...entry, snapshot_id: snapshot.snapshot_id, source_sequence: snapshot.source_sequence,
  }));
  if (fixture && fixture.exitCode === null) {
    fixture.kill('SIGTERM');
    await until(() => fixture.exitCode !== null || fixture.signalCode !== null, 'owned fixture exited', 3000)
      .catch(() => { fixture.kill('SIGKILL'); proof.cleanup_error = 'owned fixture needed SIGKILL'; proof.ok = false; });
  }
  proof.fixture_exit = fixture ? { code: fixture.exitCode, signal: fixture.signalCode } : null;
  fs.writeFileSync(path.join(output, 'result.json'), JSON.stringify(proof, null, 2) + '\n', { mode: 0o600 });
  app.quit();
  setTimeout(() => app.exit(proof.ok ? 0 : 1), 2000).unref();
}
process.on('uncaughtException', error => void finish(error));
process.on('unhandledRejection', error => void finish(error));
setTimeout(() => void finish(new Error('background diagnostic timeout')), 90000).unref();

void app.whenReady().then(async () => {
  fixture = spawn(path.join(root, 'target/native-macos/Background AX Fixture.app/Contents/MacOS/BackgroundAXFixture'),
    ['--snapshot', fixtureSnapshot], { stdio: ['ignore', 'pipe', 'pipe'] });
  let buffer = '';
  fixture.stdout.on('data', data => {
    buffer += data.toString('utf8');
    assert.ok(buffer.length < 4096);
    const newline = buffer.indexOf('\n');
    if (newline >= 0 && !fixtureReady) fixtureReady = JSON.parse(buffer.slice(0, newline));
  });
  fixture.stderr.on('data', data => { proof.fixture_stderr = String(data).slice(-4096); });
  await until(() => fixtureReady, 'background fixture ready');
  assert.equal(fixtureReady.pid, fixture.pid);
  assert.ok(Number.isSafeInteger(fixtureReady.window_number) && fixtureReady.window_number > 0);
  fs.writeFileSync(path.join(output, 'fixture-owner.json'), JSON.stringify({
    pid: fixture.pid,
    executable: path.join(root, 'target/native-macos/Background AX Fixture.app/Contents/MacOS/BackgroundAXFixture'),
    snapshot: fixtureSnapshot,
  }) + '\n', { flag: 'wx', mode: 0o600 });
  proof.fixture_ready = fixtureReady;
  proof.initial_fixture = stableJson(fixtureSnapshot).value;
  process.env.CU_BIN_DIR = path.join(root, 'target/debug');
  process.env.CU_SOURCE_BACKEND = 'native';
  process.env.CU_TARGET_BUNDLE_ID = bundle;
  process.env.CU_TARGET_PID = String(fixture.pid);
  process.env.CU_SOURCE_BACKGROUND_WINDOW_ID = `window-${fixtureReady.window_number}`;
  process.env.CU_NATIVE_REFERENCE_PLANNER = '1';
  process.env.CU_OWNED_BACKGROUND_DIAGNOSTICS_DIR = diagnostics;
  delete process.env.CU_SOURCE_TABLE_SCOPE;
  delete process.env.CU_SOURCE_READ_WINDOW_ID;
  delete process.env.ELECTRON_RENDERER_URL;
  const shown = dialog.showMessageBox.bind(dialog);
  dialog.showMessageBox = async (...args) => {
    const options = args.at(-1);
    assert.equal(options.cancelId, 0);
    assert.equal(options.defaultId, 0);
    assert.ok(options.detail.includes(bundle) && options.detail.includes(`Process ${fixture.pid}`));
    const action = proof.dialogs?.length ? 'allow' : 'cancel';
    const response = shown(...args);
    const driven = new Promise((resolve, reject) => execFile(path.join(root, 'target/native-macos/consent-dialog-driver'),
      [action, 'background-ax'], { timeout: 10000 }, (error, stdout) => error ? reject(error) : resolve(JSON.parse(stdout))));
    const [result, driver] = await Promise.all([response, driven]);
    assert.equal(driver.status, 'pressed');
    proof.dialogs = (proof.dialogs || []).concat([{ action, response: result.response, driver }]);
    return result;
  };
  const created = new Promise(resolve => app.once('browser-window-created', (_, w) => resolve(w)));
  await import(path.join(root, 'apps/desktop/out/main/index.js'));
  window = await created;
  await until(() => !window.webContents.isLoading(), 'Operator loaded');
  archiveTimer = setInterval(() => { try { captureSource(); } catch {} }, 100);
  const js = source => window.webContents.executeJavaScript(source);
  await until(async () => (await js('window.desktop.nativeAuthorization()')).can_authorize, 'fresh BG candidate', 25000);
  assert.equal((await js('window.desktop.snapshot()')).capabilities.execution_domain, 'background_ax');
  assert.equal((await js('window.desktop.snapshot()')).capabilities.input, 'unavailable');
  const clickAuthorize = '(() => { const b = document.querySelector(".native-authorization button.primary"); if (!b || b.disabled) return false; b.click(); return true; })()';
  assert.equal(await js(clickAuthorize), true);
  await until(() => proof.dialogs?.length === 1, 'actual Cancel');
  assert.equal(proof.dialogs[0].response, 0);
  assert.equal((await js('window.desktop.nativeAuthorization()')).phase, 'observing');
  await until(async () => (await js('window.desktop.nativeAuthorization()')).can_authorize, 'fresh candidate after Cancel');
  assert.equal(await js(clickAuthorize), true);
  await until(async () => (await js('window.desktop.nativeAuthorization()')).phase === 'authorized', 'actual Allow and guarded restart');
  proof.authorization = await js('window.desktop.nativeAuthorization()');
  assert.equal(proof.authorization.authorized_domain, 'background_ax');
  await until(async () => (await js('window.desktop.snapshot()')).capabilities.input === 'available', 'BGPress ready');
  await js('window.desktop.createTask("Enable background fixture toggle")');
  await until(() => js('!!document.querySelector(".prompt")'), 'explicit typed Goal confirmation');
  await js('Array.from(document.querySelectorAll(".prompt button")).find(b => b.textContent === "Yes, do it").click()');
  await until(async () => (await js('window.desktop.snapshot()')).root?.state === 'ready', 'frozen Root ready in Observe');
  // Allow replaces the passive peer session. Device Ready and task planning can
  // finish before the new Source reports its first native watch directory.
  await until(() => session && fs.existsSync(path.join(session, 'host.db')), 'current guarded session ledger');
  const before = ledger();
  assert.equal(before.root_tasks.length, 1);
  const frozenGoal = JSON.parse(before.root_tasks[0].goal_json);
  assert.equal(frozenGoal.execution_domain, 'background_ax');
  assert.equal(frozenGoal.confirmation_scope, 'current_ui');
  proof.goal = frozenGoal;
  assert.deepEqual(stableJson(fixtureSnapshot).value, proof.initial_fixture);
  assert.equal(receipts().filter(r => r.category === 'ax-press-submit-receipt').length, 0);
  // Keep the visible Operator in front after its real consent and Goal dialogs.
  // Hiding its only window can leave the OS without a focused window to prove.
  proof.focus_before = await until(async () => { const f = await focus(); return f.pid !== fixture.pid ? f : false; }, 'independent foreground identity after setup');
  await js('window.desktop.setMode("execute")');
  const rootState = await until(async () => {
    const state = (await js('window.desktop.snapshot()')).root?.state;
    return ['completed', 'failed', 'expired', 'stopped', 'suspended'].includes(state) ? state : false;
  }, 'BG Root completion', 25000);
  assert.equal(rootState, 'completed', `BG Root ended before completion: ${rootState}`);
  proof.focus_after = await focus();
  assert.deepEqual(proof.focus_after, proof.focus_before, 'foreground app/window preserved through execution');
  const after = stableJson(fixtureSnapshot).value;
  assert.equal(after.value, true);
  assert.equal(after.action_count, 1);
  assert.equal(after.pointer_event_count, 0);
  assert.equal(after.key_event_count, 0);
  proof.ledger = ledger();
  const attempts = proof.ledger.attempts.filter(a => a.activation_seq > 0);
  assert.equal(attempts.length, 1);
  assert.equal(attempts[0].skill, 'ui.ensure_value');
  assert.equal(attempts[0].state, 'settled');
  assert.equal(proof.ledger.attempt_map.filter(m => m.attempt_id === attempts[0].attempt_id).length, 1);
  assert.equal(proof.ledger.admission_outbox.filter(m => m.attempt_id === attempts[0].attempt_id && m.delivered === 1).length, 1);
  const native = receipts().filter(r => r.category === 'ax-press-submit-receipt');
  assert.equal(native.length, 1);
  assert.equal(native[0].correlation?.attempt_id ?? native[0].attempt_id, attempts[0].attempt_id);
  assert.equal(native[0].ax_trace?.ax_call_count, 1);
  assert.equal(native[0].ax_trace?.ax_error_code, 0);
  proof.native_ax_call_count = { status: 'verified', count: 1 };
  const effect = proof.ledger.effect_results.map(r => r.result).find(r => r.predicate === 'goal.required@1' && r.status === 'satisfied');
  assert.ok(effect?.input_span && effect.confirmation_scope === 'current_ui');
  const original = await until(() => { captureSource(); return [...sourceHistory.values()].find(r => effect.evidence_group.includes(r.snapshot.snapshot_id)); }, 'original fresh Source referenced by effect');
  assert.ok(original.snapshot.observed_ms > effect.input_span.to_ms);
  assert.equal(original.snapshot.target_context?.kind, 'native_window');
  const target = original.snapshot.objects.filter(o => o.role === 'AXCheckBox' && o.title === 'Background toggle');
  assert.equal(target.length, 1);
  assert.deepEqual(target[0].value, { type: 'bool', value: true });
  proof.independent_source_proof = JSON.parse(execFileSync(path.join(root, 'target/debug/compile_device_context'), [], { encoding: 'utf8', timeout: 2000,
    input: JSON.stringify({ snapshot: original.snapshot, target: target[0].instance_ref,
      scope: { kind: 'background_ax', target_context_hash: original.snapshot.target_context.source.target_context_hash } }) }));
  proof.goal_effect = effect;
  proof.post_input_source = { path: original.path, sha256: original.sha256, snapshot_id: original.snapshot.snapshot_id };
  await finish();
}).catch(error => finish(error));
