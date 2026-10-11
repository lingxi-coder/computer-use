// Explicit owned-app UI smoke. Invoked only by smoke-macos-operator-authorization.mts.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { spawn, execFile, execFileSync } = require('node:child_process');
const { app, dialog, globalShortcut } = require('electron');
const root = process.env.CU_AUTH_ROOT, output = process.env.CU_AUTH_OUTPUT;
const executeTask = process.env.CU_AUTH_EXECUTE === '1';
const toggleTask = process.env.CU_AUTH_TOGGLE === '1';
const scrollTask = process.env.CU_AUTH_SCROLL === '1';
const menuTask = process.env.CU_AUTH_MENU === '1';
const keyboardTask = process.env.CU_AUTH_KEYBOARD === '1';
const pointerOnly = process.env.CU_AUTH_POINTER_ONLY === '1';
if (!root || !output) throw new Error('Run through the explicit owned Operator smoke.');
assert.ok(!scrollTask || (executeTask && !toggleTask && !pointerOnly && !menuTask && !keyboardTask), 'scroll requires exclusive Execute mode');
assert.ok(!menuTask || (executeTask && !toggleTask && !pointerOnly && !scrollTask && !keyboardTask), 'menu requires exclusive Execute mode');
assert.ok(!keyboardTask || (executeTask && !toggleTask && !pointerOnly && !scrollTask && !menuTask), 'keyboard requires exclusive Execute mode');
if (keyboardTask) assert.ok(path.isAbsolute(process.env.CU_NATIVE_DEVICE_GUARD || '') && path.basename(process.env.CU_NATIVE_DEVICE_GUARD).startsWith('owned-key-'), 'keyboard smoke needs the explicit owned helper override');
app.setName('Computer Use Permission Smoke'); app.setPath('userData', path.join(output, 'user-data'));
const proof = { checks: [], console_errors: [], live_models_verified: false, task_execution_verified: false };
if (scrollTask || menuTask || keyboardTask) proof.fresh_per_input_permits_directly_logged = false;
proof.electron = process.versions.electron;
assert.equal(proof.electron, JSON.parse(fs.readFileSync(path.join(root, 'node_modules/electron/package.json'), 'utf8')).version);
const fixtureBinary = process.env.CU_AUTH_FIXTURE_BINARY || path.join(root, 'target/native-macos/GuardFixture.app/Contents/MacOS/GuardFixture');
const fixture = spawn(fixtureBinary, scrollTask || menuTask || keyboardTask ? [] : toggleTask ? ['--text-mode', '--toggle-mode', ...(pointerOnly ? ['--toggle-pointer-only'] : [])] : ['--text-mode'], { stdio: ['pipe', 'pipe', 'pipe'] });
fixture.stdin.on('error', () => {});
let fixtureStderr = '';
fixture.stderr.on('data', (chunk) => { fixtureStderr = (fixtureStderr + chunk).slice(-4096); });
fixture.on('exit', (code, signal) => { proof.fixture_exit = { code, signal }; });
const replies = []; let buffer = '', finished = false, dialogAction = 'cancel';
let sessionDirectory;
let sourceAssetTimer;
let preservedSourceBytes = 0;
const sourceAssetHashes = new Set();
const sourceAssetVersions = new Map();
const stderrWrite = process.stderr.write.bind(process.stderr);
process.stderr.write = (...args) => {
  const line = String(args[0]);
  const match = /cu-source: native watch manifest ([^\n]+)\/native-assets\//.exec(line);
  if (match) sessionDirectory = match[1];
  return stderrWrite(...args);
};
fixture.stdout.on('data', (chunk) => { buffer += chunk; let i; while ((i = buffer.indexOf('\n')) >= 0) { replies.push(JSON.parse(buffer.slice(0, i))); buffer = buffer.slice(i + 1); } });
async function until(read, why, timeout = 15000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const value = await read(); if (value) return value; await new Promise((done) => setTimeout(done, 30)); }
  throw new Error('Timeout: ' + why);
}
function check(value, why) { assert.ok(value, why); proof.checks.push(why); }
function destinationVisible(stats) {
  const target = stats.destination_frame, viewport = stats.viewport_frame;
  return stats.page === 'Document' && target.x >= viewport.x && target.y >= viewport.y &&
    target.x + target.width <= viewport.x + viewport.width && target.y + target.height <= viewport.y + viewport.height;
}
function checkUntouchedScrollDestination(stats) {
  check(stats.value === false && stats.destination_value === false && stats.value_calls === 0 && stats.press_calls === 0 &&
    stats.pointer_down_calls === 0 && stats.pointer_up_calls === 0 && stats.destination_action_calls === 0,
    'the real scroll destination remains unchecked with no setter, AX press or pointer click');
}
function checkSelectedMenu(stats) {
  check(stats.action_calls === 1 && stats.selection_calls === 1 && stats.selected_value === 'Fixture menu second item' &&
    stats.selected_label === 'Fixture menu second item' && stats.selection_label === 'Selection: Fixture menu second item',
    'one production ShowMenu and one selection set the actual second item value');
  assert.deepEqual(stats.item_states, [0, 1], 'the actual first item is off and second item is on');
  check(stats.navigation_calls === 2 && stats.document_navigation_calls === 1 && stats.other_navigation_calls === 1 &&
    stats.page === 'Document' && stats.page_label === 'Page: Document' && stats.tracking === false && !stats.menu_visibility_verified,
    'exactly two AX navigation calls return to Document with the native menu closed');
}
function checkKeyboardValue(stats) {
  check(stats.value === true && stats.toggle_label === 'Toggle: on' && stats.toggle_activation_calls === 1 && stats.toggle_ax_press_calls === 0,
    'one actual keyboard activation enables the toggle without AXPress');
  check(stats.navigation_calls === 2 && stats.page === 'Document' && stats.page_label === 'Page: Document',
    'two actual AX navigation calls return to Document');
  check(stats.text_value === 'before' && stats.composition_setup_calls === 0 && !stats.secure_visible,
    'the keyboard profile leaves actual text and composition setup unchanged');
  check(stats.raw_key_event_count === 2 && !stats.input_events_truncated && stats.input_events.length === 2,
    'the unfiltered AppKit trace contains exactly one keyboard down/up pair');
  const [down, up] = stats.input_events;
  check(down.type === 10 && up.type === 11 && down.cg_type === 10 && up.cg_type === 11 &&
    down.key_code === 49 && up.key_code === 49 && down.cg_event_available && up.cg_event_available &&
    typeof down.marker === 'string' && down.marker !== '0' && down.marker === up.marker &&
    Number(down.timestamp) > 0 && Number(up.timestamp) >= Number(down.timestamp) &&
    typeof down.cg_timestamp === 'string' && typeof up.cg_timestamp === 'string' &&
    BigInt(down.cg_timestamp) > 0n && BigInt(up.cg_timestamp) > BigInt(down.cg_timestamp) &&
    down.flags === 0 && up.flags === 0 && down.autorepeat === false && up.autorepeat === false &&
    !down.unicode_withheld && !up.unicode_withheld,
    'one actual Space key down/up carries the same nonzero marker, real timestamps, no modifiers and no autorepeat');
}
function preserveSourceAssets(stage) {
  if (!executeTask || !sessionDirectory) return;
  const directory = path.join(sessionDirectory, 'native-assets');
  if (!fs.existsSync(directory)) return;
  const queue = [directory];
  let inspected = 0;
  while (queue.length && inspected < 256) {
    const currentDirectory = queue.shift();
    let entries;
    try { entries = fs.readdirSync(currentDirectory, { withFileTypes: true }); } catch { continue; }
    for (const entry of entries) {
      inspected += 1;
      if (inspected > 256) break;
      const file = path.join(currentDirectory, entry.name);
      if (entry.isDirectory()) { queue.push(file); continue; }
      if (!entry.isFile() || !/\.(json|png|jpg|jpeg|webp)$/i.test(entry.name)) continue;
      try {
        const stat = fs.statSync(file), size = stat.size;
        const version = `${stat.mtimeMs}:${stat.ctimeMs}:${size}`;
        if (sourceAssetVersions.get(file) === version) continue;
        if (size > 12 * 1024 * 1024 || preservedSourceBytes + size > 128 * 1024 * 1024) {
          proof.source_asset_limit_reached = true; continue;
        }
        const data = fs.readFileSync(file), hash = createHash('sha256').update(data).digest('hex');
        const after = fs.statSync(file);
        if (after.size !== size || data.length !== size || after.mtimeMs !== stat.mtimeMs || after.ctimeMs !== stat.ctimeMs) continue;
        sourceAssetVersions.set(file, version);
        if (sourceAssetHashes.has(hash)) continue;
        const targetDirectory = path.join(output, 'source-assets');
        fs.mkdirSync(targetDirectory, { recursive: true });
        const retained = path.join(targetDirectory, hash + path.extname(entry.name));
        fs.writeFileSync(retained, data); sourceAssetHashes.add(hash); preservedSourceBytes += data.length;
        proof.source_assets ??= [];
        proof.source_assets.push({ stage, source: file, retained, sha256: hash, bytes: data.length });
      } catch (error) {
        // Source retention and watch replacement can remove an asset during a
        // readonly copy; already retained bytes and their hashes stay intact.
        proof.source_asset_copy_errors ??= [];
        if (proof.source_asset_copy_errors.length < 16) proof.source_asset_copy_errors.push(String(error));
      }
    }
  }
}
async function fixtureStats() {
  const start = replies.length;
  fixture.stdin.write('stats\n');
  return until(() => replies.slice(start).find(r => r.kind === 'stats'), 'owned fixture diagnostics', 1000);
}
async function toggleStats() {
  const start = replies.length;
  fixture.stdin.write('stats\n');
  return until(() => replies.slice(start).find(r => r.kind === 'toggle_stats'), 'owned toggle diagnostics', 1000);
}
async function prepareOwnedWindow() {
  if (menuTask) {
    const start = replies.length;
    fixture.stdin.write('accept-menu\n');
    await until(() => replies.slice(start).find(r => r.kind === 'accept-menu'), 'owned menu setup while observing', 1000);
  }
  const start = replies.length;
  fixture.stdin.write('focus-window\n');
  await until(() => replies.slice(start).find(r => r.kind === 'focus-window'), 'owned fixture setup focus', 1000);
  if (keyboardTask) {
    const focusStart = replies.length;
    fixture.stdin.write('focus-toggle\n');
    await until(() => replies.slice(focusStart).find(r => r.kind === 'focus-toggle'), 'owned toggle setup while observing', 1000);
  }
  const stats = await until(async () => {
    const current = await fixtureStats(); return current.app_active && current.window_key ? current : false;
  }, 'owned fixture foreground before authorization', 2000);
  proof.authorization_precondition = stats;
}
function readLedger() {
  if (!sessionDirectory || !fs.existsSync(path.join(sessionDirectory, 'host.db'))) return undefined;
  return JSON.parse(execFileSync('python3', ['-c', 'import sqlite3,json,sys\nc=sqlite3.connect("file:"+sys.argv[1]+"?mode=ro",uri=True)\nc.row_factory=sqlite3.Row\nd={t:[dict(r) for r in c.execute("select * from "+t)] for t in ["attempts","decisions","attempt_map","admission_outbox","effect_results","root_tasks"]}\nfor r in d["effect_results"]:r["result"]=json.loads(r.pop("json"))\nprint(json.dumps(d))', path.join(sessionDirectory, 'host.db')], { encoding: 'utf8', timeout: 1000 }));
}
async function windowProbe() {
  return new Promise((resolve, reject) => execFile(path.join(root, 'target/native-macos/window-read-probe'), [String(fixture.pid)], { timeout: 2000 }, (probeError, stdout) => {
    if (probeError) reject(probeError); else resolve(JSON.parse(stdout));
  }));
}
const showDialog = dialog.showMessageBox.bind(dialog);
dialog.showMessageBox = async (...args) => {
  const options = args.at(-1); check(options.cancelId === 0 && options.defaultId === 0, 'native consent defaults to Cancel');
  check(options.detail.includes('com.lingxi.computer-user.guard-fixture') && options.detail.includes('Process ' + fixture.pid), 'native dialog identifies the observed owned process');
  const response = showDialog(...args);
  const pressed = new Promise((resolve, reject) => execFile(path.join(root, 'target/native-macos/consent-dialog-driver'), [dialogAction], { timeout: 10000 }, (error, stdout) => {
    if (error) reject(error); else resolve(JSON.parse(stdout));
  }));
  const [result, driver] = await Promise.all([response, pressed]);
  check(driver.status === 'pressed', 'real native consent button pressed by the parent-only test driver');
  proof.dialogs ??= []; proof.dialogs.push({ response: result.response, button: driver.button }); return result;
};
async function finish(error, window) {
  if (finished) return; finished = true; proof.ok = !error;
  if (error) proof.error = error.stack || String(error);
  if (sourceAssetTimer) clearInterval(sourceAssetTimer);
  try { preserveSourceAssets(error ? 'failure' : 'finished'); } catch (diagnostic) { proof.source_asset_diagnostic_error = String(diagnostic); }
  proof.fixture_pid = fixture.pid;
  proof.fixture_binary = fixtureBinary;
  proof.fixture_ready = replies.find(r => r.kind === 'ready');
  proof.fixture_stderr = fixtureStderr;
  if (menuTask || keyboardTask) proof.fixture_transcript = replies;
  if (error) {
    if (window && !window.isDestroyed()) {
      try { proof.failure_authorization = await window.webContents.executeJavaScript('window.desktop.nativeAuthorization()'); } catch {}
      try { proof.failure_projection = await window.webContents.executeJavaScript('window.desktop.snapshot()'); } catch {}
    }
    try { proof.fixture_failure_stats = await fixtureStats(); } catch (diagnostic) { proof.fixture_diagnostic_error = String(diagnostic); }
    if (toggleTask) {
      try { proof.toggle_failure_stats = await toggleStats(); } catch (diagnostic) { proof.toggle_diagnostic_error = String(diagnostic); }
    }
    try {
      proof.fixture_ax_probe = await windowProbe();
    } catch (diagnostic) { proof.fixture_ax_probe_error = String(diagnostic); }
    try { proof.failure_ledger = readLedger(); } catch (diagnostic) { proof.ledger_diagnostic_error = String(diagnostic); }
  }
  if (window && !window.isDestroyed()) try { fs.writeFileSync(path.join(output, 'permission-window.png'), (await window.webContents.capturePage()).toPNG()); } catch {}
  fs.writeFileSync(path.join(output, 'result.json'), JSON.stringify(proof, null, 2));
  fixture.kill('SIGKILL'); app.quit(); setTimeout(() => app.exit(error ? 1 : 0), 5000).unref();
}
process.on('uncaughtException', (error) => void finish(error));
setTimeout(() => void finish(new Error('Owned Operator smoke timed out')), 55000).unref();
app.on('browser-window-created', (_event, window) => {
  const contents = window.webContents;
  contents.on('console-message', (details) => { if (details.level === 'error') proof.console_errors.push(details.message); });
  contents.on('preload-error', (_event, _path, error) => proof.console_errors.push(String(error)));
  contents.once('did-finish-load', () => void (async () => {
    await until(() => contents.executeJavaScript('window.desktop?.nativeAuthorization().then(s => s.can_authorize)'), 'fresh supported owned window');
    check((await contents.executeJavaScript('window.desktop.snapshot()')).capabilities.input === 'unavailable', 'startup native input is disabled');
    check(await contents.executeJavaScript('document.querySelector("option[value=execute]").disabled'), 'Execute is disabled before permission');
    await until(() => contents.executeJavaScript('(() => { const button = document.querySelector(".native-authorization button.primary"); if (!button || button.disabled) return false; button.click(); return true; })()'), 'enabled initial authorization button clicked');
    await until(() => proof.dialogs?.length === 1, 'real Cancel dialog');
    check((await contents.executeJavaScript('window.desktop.nativeAuthorization()')).phase === 'observing', 'Cancel keeps observation only');
    // Independent setup while observing; never restore focus during Execute.
    await prepareOwnedWindow();
    dialogAction = 'allow';
    await until(() => contents.executeJavaScript('(() => { const button = document.querySelector(".native-authorization button.primary"); if (!button || button.disabled) return false; button.click(); return true; })()'), 'enabled authorization button after Cancel clicked');
    await until(() => contents.executeJavaScript('window.desktop.nativeAuthorization().then(s => s.phase === "authorized")'), 'real Allow dialog and scoped input restart');
    await until(() => contents.executeJavaScript('!document.querySelector("option[value=execute]").disabled'), 'Execute enabled after guarded capabilities');
    const authorized = await contents.executeJavaScript('window.desktop.nativeAuthorization()');
    proof.target = authorized.authorized;
    proof.fixture_authorized_stats = await fixtureStats();
    check(authorized.authorized.pid === fixture.pid && authorized.authorized.app === 'com.lingxi.computer-user.guard-fixture', 'permission stays on the exact owned process and window');
    fs.writeFileSync(path.join(output, 'authorized-window.png'), (await contents.capturePage()).toPNG());
    await contents.executeJavaScript('window.desktop.createTask(' + JSON.stringify(keyboardTask ? 'Enable fixture keyboard toggle' : menuTask ? 'Select fixture menu second item' : scrollTask ? 'Reveal fixture scroll destination' : toggleTask ? 'Enable fixture toggle' : 'Fill document: owned Operator draft') + ')');
    await until(() => contents.executeJavaScript('!!document.querySelector(".prompt")'), 'ordinary goal prompt');
    await contents.executeJavaScript('Array.from(document.querySelectorAll(".prompt button")).find(b => b.textContent === "Yes, do it").click()');
    await until(() => contents.executeJavaScript('window.desktop.snapshot().then(s => s.root?.state === "ready" && s.mode === "observe")'), 'goal confirmation remains ready in observe');
    if (keyboardTask) {
      proof.fixture_execution_baseline = await fixtureStats();
      const baseline = proof.fixture_execution_baseline;
      check(baseline.value === false && baseline.toggle_activation_calls === 0 && baseline.toggle_ax_press_calls === 0 &&
        baseline.navigation_calls === 0 && baseline.raw_key_event_count === 0 && baseline.input_events.length === 0 && !baseline.input_events_truncated,
        'authorization and the confirmed keyboard goal submit zero keyboard, toggle or navigation input before Execute');
      check(baseline.page === 'Document' && baseline.page_label === 'Page: Document' && baseline.text_value === 'before',
        'the actual keyboard fixture starts on Document with untouched text');
      const readyLedger = readLedger();
      assert.ok(readyLedger?.root_tasks.length === 1, 'confirmed keyboard root is persisted');
      proof.keyboard_frozen_goal_json = readyLedger.root_tasks[0].goal_json;
      const goal = proof.keyboard_frozen_goal = JSON.parse(proof.keyboard_frozen_goal_json);
      check(goal.user_text === 'Enable fixture keyboard toggle' && goal.desired.type === 'bool' && goal.desired.value === true &&
        goal.confirmation_scope === 'ui_revisit' && goal.max_effect_class === 'reversible_edit' &&
        goal.target.semantic_target === 'keyboard-toggle' && goal.target.label === 'Key fixture toggle' &&
        goal.target.group === 'Key fixture group' && goal.target.role_hint === 'AXCheckBox' && JSON.stringify(goal.required).includes('revisited'),
        'the keyboard goal freezes the registered checkbox, true value and UI revisit scope');
      proof.keyboard_ready_transcript_length = replies.length;
      preserveSourceAssets('ready');
      sourceAssetTimer = setInterval(() => {
        try { preserveSourceAssets('execution'); } catch (error) { proof.source_asset_diagnostic_error = String(error); }
      }, 100);
      sourceAssetTimer.unref();
    }
    if (toggleTask) {
      proof.fixture_execution_baseline = await fixtureStats();
      proof.toggle_execution_baseline = await toggleStats();
      check(proof.toggle_execution_baseline.value_calls === 0 && proof.toggle_execution_baseline.press_calls === 0 && proof.toggle_execution_baseline.pointer_down_calls === 0 && proof.toggle_execution_baseline.pointer_up_calls === 0,
        'observation and confirmed ready goal have not submitted toggle input');
    }
    if (scrollTask) {
      proof.fixture_execution_baseline = await fixtureStats();
      const baseline = proof.fixture_execution_baseline;
      check(baseline.input_events.length === 0 && baseline.raw_scroll_calls === 0 && baseline.scroll_calls === 0 && baseline.navigation_calls === 0,
        'authorization and the confirmed ready scroll goal submit zero fixture input before Execute');
      check(baseline.page === 'Document' && baseline.page_label === 'Page: Document' && baseline.offset_x === 0 && baseline.offset_y === 0 && !destinationVisible(baseline),
        'the real destination starts outside the visible document viewport at offset zero');
      checkUntouchedScrollDestination(baseline);
      const readyLedger = readLedger();
      assert.ok(readyLedger?.root_tasks.length === 1, 'confirmed scroll root is persisted');
      proof.scroll_frozen_goal = JSON.parse(readyLedger.root_tasks[0].goal_json);
      proof.scroll_frozen_goal_json = readyLedger.root_tasks[0].goal_json;
      check(proof.scroll_frozen_goal.desired.type === 'bool' && proof.scroll_frozen_goal.desired.value === false && proof.scroll_frozen_goal.confirmation_scope === 'ui_revisit' &&
        proof.scroll_frozen_goal.target.label === 'MVP scroll destination',
        'the confirmed scroll goal freezes false and requires UI revisit');
    }
    if (menuTask) {
      proof.fixture_execution_baseline = await fixtureStats();
      const baseline = proof.fixture_execution_baseline;
      check(baseline.action_calls === 0 && baseline.selection_calls === 0 && baseline.navigation_calls === 0 &&
        baseline.selected_value === 'No selection' && baseline.selected_label === null && baseline.tracking === false,
        'authorization and the confirmed ready menu goal have no ShowMenu, selection or navigation input');
      assert.deepEqual(baseline.item_states, [0, 0], 'both actual native menu items start off');
      check(baseline.page === 'Document' && baseline.page_label === 'Page: Document', 'menu task starts on the actual Document page');
      const readyLedger = readLedger();
      assert.ok(readyLedger?.root_tasks.length === 1, 'confirmed menu root is persisted');
      proof.menu_frozen_goal_json = readyLedger.root_tasks[0].goal_json;
      proof.menu_frozen_goal = JSON.parse(proof.menu_frozen_goal_json);
      const goal = proof.menu_frozen_goal;
      check(goal.user_text === 'Select fixture menu second item' && goal.desired.type === 'text' && goal.desired.value === 'Fixture menu second item' &&
        goal.confirmation_scope === 'ui_revisit' && goal.max_effect_class === 'reversible_edit' && goal.target.semantic_target === 'menu-control' &&
        goal.target.label === 'Fixture native menu' && goal.target.group === 'Menu fixture group' && goal.target.role_hint === 'AXMenuButton',
        'the confirmed menu goal freezes its exact choice, opener identity and UI revisit scope');
      check(JSON.stringify(goal.required).includes('revisited'), 'the menu root already requires revisit before Execute');
      preserveSourceAssets('ready');
      sourceAssetTimer = setInterval(() => {
        try { preserveSourceAssets('execution'); } catch (error) { proof.source_asset_diagnostic_error = String(error); }
      }, 100);
      sourceAssetTimer.unref();
    }
    if (executeTask) {
      if (!sourceAssetTimer) {
        preserveSourceAssets('ready');
        sourceAssetTimer = setInterval(() => {
          try { preserveSourceAssets('execution'); } catch (error) { proof.source_asset_diagnostic_error = String(error); }
        }, 100);
        sourceAssetTimer.unref();
      }
      await contents.executeJavaScript('(() => { const select = document.querySelector(".mode select"); select.value = "execute"; select.dispatchEvent(new Event("change", { bubbles: true })); })()');
      await until(() => contents.executeJavaScript('window.desktop.presentation().then(s => s === "indicator")'), 'non-focusing execution indicator');
      check(!window.isFocusable(), 'execution indicator cannot take keyboard focus');
      proof.indicator_bounds = window.getBounds();
      const [tx, ty, tw, th] = authorized.authorized.frame_points;
      const b = proof.indicator_bounds;
      check(!window.isVisible() || !(b.x < tx + tw && tx < b.x + b.width && b.y < ty + th && ty < b.y + b.height), 'execution indicator does not cover the authorized window');
      fs.writeFileSync(path.join(output, 'execution-indicator.png'), (await contents.capturePage()).toPNG());
      proof.root = await until(async () => {
        const snapshot = await contents.executeJavaScript('window.desktop.snapshot()'); proof.last_projection = snapshot;
        if (['failed', 'suspended', 'unverified', 'cancelled'].includes(snapshot.root?.state)) throw new Error(JSON.stringify(snapshot));
        return snapshot.root?.state === 'completed' ? snapshot.root : false;
      }, 'GUI native task completion', 35000);
      proof.task_execution_verified = true;
      assert.ok(sessionDirectory);
      const ledger = readLedger();
      proof.ledger = ledger;
      check(ledger.attempts.some(a => a.skill === 'ui.activate_window' && a.state === 'settled'), 'registered window activation uses a settled Attempt');
      check(ledger.effect_results.some(r => r.result.predicate === 'window.active@1' && r.result.status === 'satisfied' && r.result.confirmation_scope === 'current_ui' && r.result.evidence_group.length > 0), 'activation effect is proved by a new AX observation and persisted');
      check(ledger.effect_results.some(r => r.result.predicate === 'goal.required@1' && r.result.status === 'satisfied' && r.result.confirmation_scope === 'ui_revisit'), 'root completion keeps UI revisit confirmation');
      check(ledger.attempt_map.length === ledger.attempts.length && ledger.admission_outbox.length === ledger.attempts.length, 'every GUI Attempt has one decision mapping and outbox entry');
      if (keyboardTask) {
        const keyAttempts = ledger.attempts.filter(attempt => attempt.skill === 'ui.ensure_value');
        check(keyAttempts.length === 1 && keyAttempts[0].state === 'settled', 'the keyboard toggle settles within one ui.ensure_value Attempt');
        const key = proof.keyboard_attempt = keyAttempts[0];
        check(key.intent_id === `intent-${key.attempt_id}-1` && key.quota_actions === 1,
          'the keyboard Attempt has exactly one intent under the one-action quota');
        const revisit = ledger.attempts.filter(attempt => attempt.skill === 'ui.revisit');
        check(revisit.length === 1 && revisit[0].state === 'settled' && revisit[0].intent_id === `intent-${revisit[0].attempt_id}-2` && revisit[0].quota_actions === 2,
          'one settled revisit Attempt performs its two registered AX navigation inputs under the original quota');
        const current = ledger.effect_results.filter(row => row.result.predicate === 'keyboard.value@1' && row.result.status === 'satisfied');
        check(current.length === 1 && current[0].result.confirmation_scope === 'current_ui' &&
          current[0].result.attempt_id === key.attempt_id && current[0].result.intent_id === key.intent_id &&
          current[0].result.target === 'keyboard-toggle' && current[0].result.evidence_group.length > 0 &&
          current[0].result.input_span && current[0].result.observed_ms > current[0].result.input_span.to_ms,
          'independent fresh CurrentUi Source evidence verifies the keyboard value strictly after input end');
        const currentEffect = proof.keyboard_value_effect = current[0].result;
        const rootEffect = ledger.effect_results.find(row => row.result.predicate === 'goal.required@1' && row.result.status === 'satisfied' &&
          row.result.confirmation_scope === 'ui_revisit' && row.result.observed_ms > currentEffect.observed_ms &&
          row.result.evidence_group.some(id => !currentEffect.evidence_group.includes(id)))?.result;
        check(!!rootEffect, 'later independent UI revisit evidence completes the frozen root after keyboard CurrentUi');
        proof.keyboard_root_effect = rootEffect;
        check(ledger.root_tasks.length === 1 && ledger.root_tasks[0].state === 'completed' && ledger.root_tasks[0].goal_json === proof.keyboard_frozen_goal_json,
          'keyboard completion preserves the exact frozen UI revisit goal');
        const plan = JSON.parse(ledger.root_tasks[0].plan_json);
        assert.deepEqual(plan.subgoals.map(subgoal => subgoal.skill), ['ui.ensure_value', 'ui.revisit'], 'keyboard plan ensures the true value and revisits');
        check(plan.subgoals[0].target === 'keyboard-toggle' && plan.subgoals[1].target === 'keyboard-toggle',
          'the keyboard plan keeps the same registered target through revisit');
        proof.keyboard_completed_stats = await fixtureStats(); checkKeyboardValue(proof.keyboard_completed_stats);
        const navigation = replies.slice(proof.keyboard_ready_transcript_length).filter(reply => reply.kind === 'navigation');
        assert.deepEqual(navigation.map(reply => reply.page), ['Other', 'Document'], 'actual callbacks navigate away and return once each');
        await new Promise(done => setTimeout(done, 300));
        proof.keyboard_stable_stats = await fixtureStats(); checkKeyboardValue(proof.keyboard_stable_stats);
        assert.deepEqual(proof.keyboard_stable_stats.input_events, proof.keyboard_completed_stats.input_events, 'completed keyboard input is stable without replay');
        preserveSourceAssets('completed-before-revoke');
        check(proof.source_assets?.length > 0, 'actual keyboard Source assets are retained before revocation');
      }
      if (scrollTask) {
        const scrollAttempts = ledger.attempts.filter(a => a.skill === 'ui.scroll_to');
        check(scrollAttempts.length === 1 && scrollAttempts[0].state === 'settled', 'all scroll inputs settle within one ui.scroll_to Attempt');
        const scroll = scrollAttempts[0];
        proof.scroll_attempt = scroll;
        check(scroll.intent_id === `intent-${scroll.attempt_id}-4` && scroll.quota_actions >= 4,
          'the same scroll Attempt submits four move and scroll intents within its action quota');
        check(ledger.effect_results.some(r => r.result.predicate === 'scroll.target_visible_bound@1' && r.result.status === 'satisfied' &&
          r.result.confirmation_scope === 'current_ui' && r.result.attempt_id === scroll.attempt_id && r.result.intent_id === scroll.intent_id && r.result.evidence_group.length > 0 &&
          r.result.input_span && r.result.observed_ms >= r.result.input_span.to_ms),
          'independent fresh CurrentUi evidence binds the scroll destination for the settled Attempt');
        check(ledger.root_tasks.length === 1 && ledger.root_tasks[0].state === 'completed' && ledger.root_tasks[0].goal_json === proof.scroll_frozen_goal_json,
          'root completion preserves the exact frozen UI revisit goal');
        const plan = JSON.parse(ledger.root_tasks[0].plan_json);
        assert.deepEqual(plan.subgoals.map(subgoal => subgoal.skill), ['ui.scroll_to', 'ui.ensure_value', 'ui.revisit'], 'the frozen plan reveals, checks false, and revisits the destination');
        const navigation = ledger.effect_results.find(r => r.result.predicate === 'scroll.target_visible_bound@1').result;
        check(ledger.effect_results.some(r => r.result.predicate === 'goal.required@1' && r.result.status === 'satisfied' && r.result.confirmation_scope === 'ui_revisit' &&
          r.result.observed_ms > navigation.observed_ms && r.result.evidence_group.length > 0),
          'later independent UI revisit evidence satisfies the root after the navigation postcondition');
        proof.scroll_completed_stats = await fixtureStats();
        await new Promise((done) => setTimeout(done, 300));
        proof.scroll_stable_stats = await fixtureStats();
        check(destinationVisible(proof.scroll_completed_stats) && destinationVisible(proof.scroll_stable_stats) &&
          proof.scroll_completed_stats.offset_x === proof.scroll_stable_stats.offset_x && proof.scroll_completed_stats.offset_y === proof.scroll_stable_stats.offset_y &&
          proof.scroll_stable_stats.offset_y > 0, 'the real scroll destination is visible and the clip offset is stable after UI revisit');
        checkUntouchedScrollDestination(proof.scroll_stable_stats);
      }
      if (menuTask) {
        const menuAttempts = ledger.attempts.filter(attempt => attempt.skill === 'ui.select_by_label');
        check(menuAttempts.length === 1 && menuAttempts[0].state === 'settled', 'menu expansion and selection settle in one ui.select_by_label Attempt');
        const menu = menuAttempts[0];
        proof.menu_attempt = menu;
        check(menu.intent_id === `intent-${menu.attempt_id}-2` && menu.quota_actions === 2,
          'the menu Attempt ends with its second intent under the original two-action quota');
        const mapping = ledger.attempt_map.filter(row => row.attempt_id === menu.attempt_id);
        const decision = ledger.decisions.filter(row => row.decision_id === menu.decision_id);
        const outbox = ledger.admission_outbox.filter(row => row.attempt_id === menu.attempt_id);
        check(mapping.length === 1 && mapping[0].decision_id === menu.decision_id && mapping[0].generation === menu.generation &&
          decision.length === 1 && decision[0].consumed_attempt === menu.attempt_id && outbox.length === 1 && outbox[0].delivered === 1,
          'the menu Attempt has one consumed decision, durable mapping and delivered admission outbox');
        const selection = ledger.effect_results.find(row => row.result.predicate === 'menu.selected_value@1' &&
          row.result.status === 'satisfied' && row.result.confirmation_scope === 'current_ui' && row.result.attempt_id === menu.attempt_id &&
          row.result.intent_id === menu.intent_id && row.result.target === 'menu-control' && row.result.evidence_group.length > 0 &&
          row.result.input_span && row.result.observed_ms >= row.result.input_span.to_ms)?.result;
        check(!!selection, 'independent fresh CurrentUi evidence verifies the selected menu value after its second input');
        proof.menu_selection_effect = selection;
        const rootEffect = ledger.effect_results.find(row => row.result.predicate === 'goal.required@1' && row.result.status === 'satisfied' &&
          row.result.confirmation_scope === 'ui_revisit' && row.result.observed_ms > selection.observed_ms && row.result.evidence_group.length > 0 &&
          row.result.evidence_group.some(id => !selection.evidence_group.includes(id)))?.result;
        check(!!rootEffect, 'later independent UI revisit evidence satisfies the frozen root after the menu selection effect');
        proof.menu_root_effect = rootEffect;
        check(ledger.root_tasks.length === 1 && ledger.root_tasks[0].state === 'completed' && ledger.root_tasks[0].goal_json === proof.menu_frozen_goal_json,
          'menu root completion preserves the exact frozen UI revisit goal without downgrading it');
        const plan = JSON.parse(ledger.root_tasks[0].plan_json);
        assert.deepEqual(plan.subgoals.map(subgoal => subgoal.skill), ['ui.select_by_label', 'ui.revisit'], 'the menu plan selects and then revisits');
        check(plan.subgoals[0].target === 'menu-control' && plan.subgoals[0].select.choice_label === 'Fixture menu second item',
          'the installed selection profile keeps the literal second-item choice and the opener target');
        proof.menu_completed_stats = await fixtureStats();
        checkSelectedMenu(proof.menu_completed_stats);
        await new Promise((done) => setTimeout(done, 300));
        proof.menu_stable_stats = await fixtureStats();
        checkSelectedMenu(proof.menu_stable_stats);
        preserveSourceAssets('completed-before-revoke');
        check(proof.source_assets?.length > 0, 'actual Source assets are retained with content hashes before revocation');
      }
    }
    await contents.executeJavaScript('document.querySelector(".native-authorization button").click()');
    await until(() => contents.executeJavaScript('window.desktop.nativeAuthorization().then(s => s.phase === "observing")'), 'Revoke returns to observation');
    check(await contents.executeJavaScript('document.querySelector("option[value=execute]").disabled'), 'Execute disabled after revocation');
    const revoked = await contents.executeJavaScript('window.desktop.snapshot()');
    check(revoked.root == null && revoked.capabilities.input === 'unavailable', 'revocation closes the old task and input session');
    await new Promise((done) => setTimeout(done, 1200));
    proof.fixture_stats = await fixtureStats();
    if (keyboardTask) {
      checkKeyboardValue(proof.fixture_stats);
      const before = proof.keyboard_stable_stats, after = proof.fixture_stats;
      assert.deepEqual(after.input_events, before.input_events, 'revocation adds no raw keyboard delivery or replay');
      check(after.raw_key_event_count === before.raw_key_event_count && after.toggle_activation_calls === before.toggle_activation_calls &&
        after.toggle_ax_press_calls === before.toggle_ax_press_calls && after.navigation_calls === before.navigation_calls &&
        after.value === before.value && after.page === before.page,
        'revocation adds no key, toggle or navigation input');
      proof.keyboard_execution_verified = true; proof.revoked_without_replay = true;
    } else if (menuTask) {
      checkSelectedMenu(proof.fixture_stats);
      const before = proof.menu_stable_stats, after = proof.fixture_stats;
      check(after.action_calls === before.action_calls && after.selection_calls === before.selection_calls &&
        after.navigation_calls === before.navigation_calls && after.document_navigation_calls === before.document_navigation_calls &&
        after.other_navigation_calls === before.other_navigation_calls && after.popup_returns === before.popup_returns &&
        after.selected_value === before.selected_value && after.page === before.page,
        'revocation adds no ShowMenu, selection, popup or navigation input');
      proof.menu_lifecycle_after_revoke = {
        before: { opened: before.menu_will_open_calls, closed: before.menu_did_close_calls },
        after: { opened: after.menu_will_open_calls, closed: after.menu_did_close_calls },
        input_evidence: false,
      };
      assert.deepEqual(after.item_states, before.item_states, 'revocation preserves the actual native item selection state');
      proof.menu_selection_verified = true; proof.revoked_without_replay = true;
    } else if (scrollTask) {
      const baseline = proof.fixture_execution_baseline.input_events;
      assert.deepEqual(proof.fixture_stats.input_events.slice(0, baseline.length), baseline, 'the exact original raw input prefix is retained');
      const events = proof.fixture_stats.input_events.slice(baseline.length);
      check(events.length === 4 && events.every((event, index) => event.type === (index % 2 === 0 ? 5 : 22) && event.marker !== '0' && Number(event.timestamp) > 0 &&
        (index === 0 || event.timestamp >= events[index - 1].timestamp)),
        'the raw unfiltered AppKit baseline suffix contains exactly two marked mouseMoved/scrollWheel pairs and no clicks');
      check(proof.fixture_stats.raw_scroll_calls === 2 && proof.fixture_stats.scroll_calls === 2 && proof.fixture_stats.scroll_events.length === 2 &&
        proof.fixture_stats.scroll_events.every(event => event.type === 22 && event.raw_dx === 0 && event.raw_dy === -120 && event.marker !== '0' && event.timestamp > 0),
        'two real native vertical scroll deliveries use the bounded 120 point delta');
      check(proof.fixture_stats.navigation_calls === 2 && proof.fixture_stats.document_navigation_calls === 1 && proof.fixture_stats.other_navigation_calls === 1 &&
        proof.fixture_stats.page === 'Document' && proof.fixture_stats.page_label === 'Page: Document',
        'exactly two AX navigation presses revisit Other and Document');
      checkUntouchedScrollDestination(proof.fixture_stats);
      assert.deepEqual(proof.fixture_stats.input_events, proof.scroll_stable_stats.input_events, 'revocation adds no raw input or replay');
      check(proof.fixture_stats.navigation_calls === proof.scroll_stable_stats.navigation_calls && proof.fixture_stats.offset_x === proof.scroll_stable_stats.offset_x &&
        proof.fixture_stats.offset_y === proof.scroll_stable_stats.offset_y && destinationVisible(proof.fixture_stats), 'revocation preserves the verified visible destination without navigation replay');
      proof.scroll_navigation_verified = true; proof.revoked_without_replay = true;
    } else if (toggleTask) {
      proof.toggle_stats = await toggleStats();
      check(proof.toggle_stats.value === true && proof.toggle_stats.value_calls === (pointerOnly ? 0 : 1) && proof.toggle_stats.press_calls === 0 && proof.toggle_stats.pointer_down_calls === 1 && proof.toggle_stats.pointer_up_calls === 1,
        pointerOnly ? 'one pointer pair enables the toggle with no AX setter' : 'one AX no-effect setter and one pointer pair enable the toggle without replay');
      const baseline = proof.fixture_execution_baseline.pointer_input_events;
      assert.deepEqual(proof.fixture_stats.pointer_input_events.slice(0, baseline.length), baseline, 'the original event baseline is retained');
      const events = proof.fixture_stats.pointer_input_events.slice(baseline.length);
      check(events.length === 2 && events[0].down && !events[1].down && events[0].marker === events[1].marker && events[0].marker !== '0',
        'independent AppKit events after Execute contain exactly one marked down/up pair');
      check(proof.fixture_stats.value_calls === 0 && proof.fixture_stats.navigation_calls === 2 && proof.fixture_stats.press_calls === 0,
        'toggle task returns through two navigation calls and does not edit text');
      proof.pointer_fallback_verified = !pointerOnly; proof.pointer_only_verified = pointerOnly;
    } else if (executeTask) {
      const ledger = proof.ledger;
      assert.ok(ledger, 'the completed text task has a captured ledger');
      check(proof.fixture_stats.value_calls === 1 && proof.fixture_stats.navigation_calls === 2 && proof.fixture_stats.press_calls === 0,
        'GUI text task uses one setter and two navigation calls without replay');
      const textAttempts = ledger.attempts.filter(attempt => attempt.skill === 'ui.enter_text' && attempt.intent_id);
      check(textAttempts.length === 1 && textAttempts[0].state === 'settled',
        'the default text task has one settled AX text-input Attempt');
      const textAttempt = proof.text_input_attempt = textAttempts[0];
      const root = ledger.root_tasks.find(task => task.root_id === textAttempt.root_id);
      assert.ok(root, 'the text Attempt belongs to the persisted root');
      const goal = JSON.parse(root.goal_json);
      const textEffects = ledger.effect_results.filter(row => row.result.predicate === 'text.value@1');
      const textEffect = textEffects.length === 1 && textEffects[0].result.status === 'satisfied'
        ? textEffects[0].result
        : undefined;
      const span = textEffect?.input_span;
      check(!!textEffect && textEffect.confirmation_scope === 'current_ui' && textEffect.causality === 'unknown' &&
        textEffect.attempt_id === textAttempt.attempt_id && textEffect.intent_id === textAttempt.intent_id &&
        textEffect.target === goal.target.semantic_target && textEffect.evidence_group.length > 0 &&
        span && Number.isSafeInteger(span.from_ms) && Number.isSafeInteger(span.to_ms) && span.to_ms >= span.from_ms &&
        Number.isSafeInteger(textEffect.observed_ms) && textEffect.observed_ms > span.to_ms,
        'fresh Source text.value@1 CurrentUi evidence matches the AX input Attempt and target, has Unknown causality, and follows its actual input end');
      proof.text_value_effect = textEffect;
      const rootEffect = ledger.effect_results.find(row => row.result.predicate === 'goal.required@1' &&
        row.result.status === 'satisfied' && row.result.confirmation_scope === 'ui_revisit' &&
        row.result.target === goal.target.semantic_target && row.result.attempt_id !== textAttempt.attempt_id &&
        row.result.observed_ms > textEffect.observed_ms && row.result.evidence_group.length > 0 &&
        row.result.evidence_group.some(id => !textEffect.evidence_group.includes(id)))?.result;
      check(!!rootEffect, 'a later independent UI revisit result completes the frozen root after the separate text CurrentUi effect');
      proof.text_root_effect = rootEffect;
    } else {
      check(proof.fixture_stats.value_calls === (executeTask ? 1 : 0) && proof.fixture_stats.navigation_calls === (executeTask ? 2 : 0) && proof.fixture_stats.press_calls === 0,
        executeTask ? 'GUI task uses one setter and two navigation calls without replay' : 'permission and observe-mode task perform no fixture input');
    }
    check(globalShortcut.isRegistered('CommandOrControl+Shift+Escape'), 'global Stop survives session replacements');
    check(proof.console_errors.length === 0, 'no renderer, preload or CSP errors');
    proof.native_dialog_rendering_verified = true; proof.product_authorization_ui_verified = true;
    await finish(null, window);
  })().catch((error) => finish(error, window)));
});
void (async () => {
  await until(() => replies.find(r => r.kind === 'ready'), 'owned fixture');
  process.env.CU_BIN_DIR = path.join(root, 'target/debug'); process.env.CU_SOURCE_BACKEND = 'native';
  process.env.CU_TARGET_BUNDLE_ID = 'com.lingxi.computer-user.guard-fixture'; process.env.CU_TARGET_PID = String(fixture.pid);
  process.env.CU_NATIVE_REFERENCE_PLANNER = '1'; delete process.env.ELECTRON_RENDERER_URL;
  await import(path.join(root, 'apps/desktop/out/main/index.js'));
})().catch((error) => finish(error));
