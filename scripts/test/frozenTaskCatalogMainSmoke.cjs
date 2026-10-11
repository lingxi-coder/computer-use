// Instrumented entry for the real desktop Main module. This file never
// constructs a replacement Main/Host; it imports the ordinary compiled entry.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const { execFileSync } = require('node:child_process');
const path = require('node:path');
const { app } = require('electron');

const root = process.env.CU_SMOKE_PROJECT_ROOT;
const output = process.env.CU_SMOKE_OUTPUT_DIR;
const userData = process.env.CU_SMOKE_USER_DATA_DIR;
const privateTmp = process.env.CU_SMOKE_TMP_DIR;
const resultPath = output && path.join(output, 'main-result.json');
const catalogPath = output && path.join(output, 'catalog.json');
const projectionPaths = output && {
  initial: path.join(output, 'projection-initial.json'),
  afterHost: path.join(output, 'projection-after-host.json'),
  afterStop: path.join(output, 'projection-after-stop.json'),
};
const frozenConfigNames = [
  'CU_FROZEN_TASK_GOALS_PATH',
  'CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH',
  'CU_FROZEN_TASK_GOALS_SHA256',
  'CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256',
];
const MAX_CATALOG_BYTES = 512 * 1024;
const MAX_PROJECTION_BYTES = 1024 * 1024;
const MAX_TIMELINE_ITEMS = 64;
const result = {
  ok: false,
  business_acceptance: false,
  main_entry: 'apps/desktop/out/main/index.js',
  checks: {},
  ipc_rejections: [],
  ui: null,
  host_response: null,
  stop_status: null,
  database_before_stop: null,
  database_after_stop: null,
  native_child_pids: [],
  temp_directories_before_quit: [],
  renderer_errors: [],
  failure_step: null,
  cleanup: { normal_main_quit_requested: false },
};

if (!root || !output || !resultPath || !projectionPaths || !userData || !privateTmp) {
  throw new Error('Run through scripts/smoke-frozen-task-catalog.mts with its private output directory.');
}
if (process.versions.electron !== process.env.CU_SMOKE_EXPECTED_ELECTRON) {
  throw new Error('Electron version does not match the pinned runtime.');
}
if (!path.isAbsolute(root) || !path.isAbsolute(output) || !path.isAbsolute(userData) || !path.isAbsolute(privateTmp)) {
  throw new Error('Smoke-owned paths must be absolute.');
}
for (const name of frozenConfigNames) {
  if (typeof process.env[name] !== 'string' || process.env[name].trim() === '') {
    throw new Error(`The frozen catalog input ${name} is missing.`);
  }
}

function assertPrivateDirectory(directory) {
  assert.equal(fs.realpathSync(directory), directory, 'Smoke directories must be canonical.');
  const stat = fs.lstatSync(directory);
  assert(stat.isDirectory() && !stat.isSymbolicLink(), 'Smoke paths must be directories, not links.');
  assert.equal(stat.uid, process.getuid(), 'Smoke directories must belong to the current user.');
  assert.equal(stat.mode & 0o7777, 0o700, 'Smoke directories must be private 0700 directories.');
  return { dev: stat.dev, ino: stat.ino, uid: stat.uid, mode: stat.mode & 0o7777 };
}

assertPrivateDirectory(output);
assertPrivateDirectory(userData);
assertPrivateDirectory(privateTmp);
for (const file of [resultPath, catalogPath, ...Object.values(projectionPaths)]) {
  assert(!fs.existsSync(file), 'Smoke evidence files must be new.');
}
fs.writeFileSync(resultPath, '', { flag: 'wx', mode: 0o600 });

app.setName('Computer Use Frozen Task Catalog Diagnostic');
app.setPath('userData', userData);
assert.equal(fs.realpathSync(app.getPath('userData')), userData);
process.env.CU_BIN_DIR = path.join(root, 'target/debug');
process.env.TMPDIR = privateTmp;
delete process.env.ELECTRON_RENDERER_URL;
for (const name of [
  'CU_SOURCE_BACKEND', 'CU_TARGET_PID', 'CU_TARGET_BUNDLE_ID', 'CU_NATIVE_SOURCE_HELPER',
  'CU_NATIVE_REFERENCE_PLANNER', 'CU_NATIVE_DEVICE_GUARD', 'CU_SOURCE_READ_WINDOW_ID',
  'CU_SOURCE_BACKGROUND_WINDOW_ID', 'CU_SOURCE_TABLE_SCOPE', 'CU_BROWSER_PAGE_PROFILE',
]) delete process.env[name];
for (const name of Object.keys(process.env)) {
  if (/(?:API.?KEY|ACCESS.?TOKEN|(?:OPENAI|ANTHROPIC)_|MODEL.*(?:KEY|TOKEN))/i.test(name)) {
    delete process.env[name];
  }
}

function safeText(value) {
  return String(value ?? '')
    .replace(/(?:file:\/\/)?\/(?:Users|private|var|tmp|Volumes)\/[^\s"']+/g, '[path]')
    .replace(/[0-9a-f]{64}/gi, '[digest]')
    .slice(0, 768);
}

function writeNewJson(file, value) {
  const bytes = Buffer.from(`${JSON.stringify(value, null, 2)}\n`, 'utf8');
  assert(bytes.length <= MAX_PROJECTION_BYTES, 'Evidence JSON exceeds its output bound.');
  fs.writeFileSync(file, bytes, { flag: 'wx', mode: 0o600 });
  const stat = fs.lstatSync(file);
  assert(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1 && (stat.mode & 0o077) === 0);
}

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function ensureNoPrivateCatalogKeys(value, depth = 0, count = { value: 0 }) {
  assert(depth <= 12, 'Public catalog nesting is unexpectedly deep.');
  count.value += 1;
  assert(count.value <= 20_000, 'Public catalog contains too many values.');
  if (Array.isArray(value)) {
    for (const child of value) ensureNoPrivateCatalogKeys(child, depth + 1, count);
    return;
  }
  if (!isRecord(value)) return;
  for (const [key, child] of Object.entries(value)) {
    assert(!/^(?:goal|objective|allowed_scope|profile|path|reset|truth|payload)$/i.test(key),
      `Public catalog exposed forbidden field ${key}.`);
    ensureNoPrivateCatalogKeys(child, depth + 1, count);
  }
}

function assertCatalog(catalog) {
  assert(isRecord(catalog) && isRecord(catalog.status) && Array.isArray(catalog.tasks));
  assert.deepEqual(Object.keys(catalog).sort(), ['status', 'tasks']);
  assert.deepEqual(Object.keys(catalog.status).sort(), ['state']);
  assert.equal(catalog.status.state, 'available', 'Normal Main did not load the frozen catalog.');
  ensureNoPrivateCatalogKeys(catalog);
  const bytes = Buffer.from(JSON.stringify(catalog), 'utf8');
  assert(bytes.length <= MAX_CATALOG_BYTES, 'Public catalog exceeds its byte bound.');
  assert(!/(?:objective|allowed_scope|initial_state|output_directory|file_path|reset|truth)/i.test(bytes.toString('utf8')),
    'Public catalog contains a private goal, path, reset or truth field.');
  assert.equal(catalog.tasks.length, 120, 'The complete frozen denominator must be exposed.');
  const expected = new Set();
  for (let id = 1; id <= 6; id += 1) {
    for (let seed = 1; seed <= 5; seed += 1) expected.add(`fixture:SET-${String(id).padStart(2, '0')}:${seed}`);
  }
  for (let id = 1; id <= 7; id += 1) {
    for (let seed = 1; seed <= 5; seed += 1) expected.add(`fixture:FORM-${String(id).padStart(2, '0')}:${seed}`);
  }
  for (let id = 1; id <= 4; id += 1) {
    for (let seed = 1; seed <= 5; seed += 1) expected.add(`fixture:LIST-${String(id).padStart(2, '0')}:${seed}`);
  }
  for (let id = 1; id <= 3; id += 1) {
    for (let seed = 1; seed <= 5; seed += 1) expected.add(`fixture:MODAL-${String(id).padStart(2, '0')}:${seed}`);
  }
  for (let id = 1; id <= 20; id += 1) expected.add(`textedit:TEXTEDIT-${String(id).padStart(2, '0')}:1`);
  const actual = new Set();
  const selectors = new Set();
  const catalogIds = new Set();
  for (const task of catalog.tasks) {
    assert(isRecord(task));
    const keys = Object.keys(task).sort();
    const expectedKeys = ['catalog_id', 'scope_compatible', 'scope_family', 'seed', 'selector', 'suite', 'task_id'];
    if (Object.hasOwn(task, 'unsupported_reason')) expectedKeys.push('unsupported_reason');
    assert.deepEqual(keys, expectedKeys.sort(), 'Public task metadata has an unexpected field.');
    assert.equal(typeof task.catalog_id, 'string');
    assert(/^[a-f0-9]{64}$/.test(task.catalog_id));
    assert.equal(typeof task.selector, 'string');
    assert(/^[a-f0-9]{64}$/.test(task.selector));
    catalogIds.add(task.catalog_id);
    assert(!selectors.has(task.selector), 'Frozen task selectors must be unique.');
    selectors.add(task.selector);
    assert.equal(typeof task.task_id, 'string');
    assert.equal(typeof task.seed, 'string');
    assert.equal(typeof task.suite, 'string');
    assert(['control', 'form', 'list', 'modal', 'textedit'].includes(task.scope_family));
    assert.equal(typeof task.scope_compatible, 'boolean');
    if (!task.scope_compatible) assert.equal(typeof task.unsupported_reason, 'string');
    const identity = `${task.suite}:${task.task_id}:${task.seed}`;
    assert(expected.has(identity), `Unexpected public task identity ${identity}.`);
    assert(!actual.has(identity), `Duplicate public task identity ${identity}.`);
    actual.add(identity);
  }
  assert.equal(catalogIds.size, 1, 'All public records must belong to one frozen catalog.');
  assert.equal(actual.size, expected.size);
  assert.deepEqual([...actual].sort(), [...expected].sort());
  const scopeCompatible = catalog.tasks.filter((task) => task.scope_compatible);
  const scopeIncompatible = catalog.tasks.filter((task) => !task.scope_compatible);
  assert.equal(scopeCompatible.length, 30);
  assert.equal(scopeIncompatible.length, 90);
  const selected = catalog.tasks.find((task) => task.suite === 'fixture' && task.task_id === 'SET-02' && task.seed === '1');
  assert(selected?.scope_compatible, 'SET-02 seed 1 must have a schema-expressible control scope.');
  const form = catalog.tasks.find((task) => task.suite === 'fixture' && task.task_id === 'FORM-01' && task.seed === '1');
  assert(form && !form.scope_compatible && form.unsupported_reason === 'form_scope_not_supported');
  return {
    bytes: bytes.length,
    scopeCompatibleCount: scopeCompatible.length,
    incompatibleScopeCount: scopeIncompatible.length,
    selected,
    form,
  };
}

let window = null;
let activeStep = 'startup';
let finishing = false;
let finishRequested = false;
let shutdownFallback;
let finalProjection = null;
let initialTimelineLength = 0;

function mark(name, condition, detail) {
  assert.ok(condition, name);
  result.checks[name] = true;
  if (detail !== undefined) result.checks[`${name}_detail`] = detail;
}

function js(source) {
  if (!window || window.isDestroyed()) throw new Error('The normal Main renderer window is unavailable.');
  return window.webContents.executeJavaScript(source);
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function until(read, why, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = await read();
    if (value) return value;
    await delay(100);
  }
  throw new Error(`Timed out waiting for ${why}.`);
}

async function readProjection() {
  return until(async () => {
    const projection = await js('window.desktop.snapshot()');
    if (projection === null) return null;
    assert(isRecord(projection), 'The public product projection is invalid.');
    const bytes = Buffer.from(JSON.stringify(projection), 'utf8');
    assert(bytes.length <= MAX_PROJECTION_BYTES, 'Public product projection exceeds its byte bound.');
    return projection;
  }, 'public product projection');
}

async function domFacts() {
  return js(`(() => {
    const text = (node) => node ? (node.innerText || node.textContent || '').trim().replace(/\\s+/g, ' ').slice(0, 1024) : null;
    const visible = (node) => !!node && node.getClientRects().length > 0 && getComputedStyle(node).visibility !== 'hidden';
    const list = document.querySelector('[aria-label="Frozen task catalog"]');
    const rows = list ? [...list.querySelectorAll(':scope > li')] : [];
    const panel = list?.closest('section');
    const taskButton = panel ? [...panel.querySelectorAll('button')].find((button) => !list.contains(button)) : null;
    const heading = panel?.querySelector('h1');
    const catalogNote = panel?.querySelector(':scope > p');
    const scopeIncompatibleRows = rows.map((row) => {
      const button = row.querySelector('button');
      const note = [...row.querySelectorAll('p')].map(text).find(Boolean) || null;
      return { disabled: !!button?.disabled, pressed: button?.getAttribute('aria-pressed') === 'true', note };
    });
    return {
      title: document.title,
      appVisible: visible(document.querySelector('.app')),
      heading: text(heading),
      catalogNote: text(catalogNote),
      optionCount: rows.length,
      scopeIncompatibleCount: scopeIncompatibleRows.filter((row) => row.disabled && row.note).length,
      rowStates: scopeIncompatibleRows,
      createButtonVisible: visible(taskButton),
      createButtonEnabled: !!taskButton && !taskButton.disabled,
      freeformTaskFieldVisible: !!document.querySelector('textarea[aria-label="Task"]'),
      hostAlert: text(document.querySelector('[role="alert"]')),
      timeline: [...document.querySelectorAll('.timeline li')].slice(-16).map(text),
      stopDisabled: !!document.querySelector('button.stop')?.disabled,
    };
  })()`);
}

function listTempDirectories() {
  const names = fs.readdirSync(privateTmp);
  assert(names.length <= 16, 'Temporary directory entry count exceeds its bound.');
  return names.map((name) => {
    assert(/^[A-Za-z0-9_-]{1,128}$/.test(name), 'Unexpected temporary child name.');
    const full = path.join(privateTmp, name);
    const stat = fs.lstatSync(full);
    assert(stat.isDirectory() && !stat.isSymbolicLink(), 'Temporary children must be owned directories.');
    assert.equal(fs.realpathSync(full), full);
    assert.equal(stat.uid, process.getuid());
    assert.equal(stat.mode & 0o077, 0);
    return full;
  });
}

function pidIsAlive(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) { return error && error.code === 'EPERM'; }
}

function processTable() {
  const stdout = execFileSync('ps', ['-axo', 'pid=,ppid=,command='], {
    encoding: 'utf8', timeout: 5_000, maxBuffer: 8 * 1024 * 1024,
  });
  return stdout.split('\n').flatMap((line) => {
    const match = /^\s*(\d+)\s+(\d+)\s+(.*)$/.exec(line);
    return match ? [{ pid: Number(match[1]), ppid: Number(match[2]), command: match[3] }] : [];
  });
}

function captureNativeChildren() {
  const binDir = path.join(root, 'target/debug');
  const roles = ['cu-host', 'cu-device', 'cu-source'];
  const matches = processTable().flatMap((entry) => {
    if (entry.ppid !== process.pid) return [];
    const role = roles.find((name) => entry.command.includes(path.join(binDir, name)));
    return role ? [{ role, pid: entry.pid }] : [];
  });
  assert(matches.length <= 8, 'Native child process count exceeds its bound.');
  result.native_child_pids = matches;
  assert.deepEqual([...new Set(matches.map((entry) => entry.role))].sort(), [...roles].sort(),
    'Normal Main did not start exactly the expected Host, Device and Source roles.');
  return matches;
}

function findHostDatabase() {
  const directories = listTempDirectories();
  assert.equal(directories.length, 1, 'Normal Main should own one temporary supervisor directory.');
  const matches = directories.map((directory) => path.join(directory, 'host.db')).filter(fs.existsSync);
  assert.equal(matches.length, 1, 'Expected exactly one normal Host database in the private temporary directory.');
  const database = matches[0];
  const stat = fs.lstatSync(database);
  assert(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1);
  assert.equal(fs.realpathSync(database), database);
  assert(database.startsWith(`${privateTmp}${path.sep}`));
  result.temp_directories_before_quit = directories.map((directory) => path.basename(directory));
  return database;
}

function readOnlyLedgerSummary(database) {
  const python = [
    'import json, sqlite3, sys, urllib.parse',
    'db = sys.argv[1]',
    'uri = "file:" + urllib.parse.quote(db, safe="/") + "?mode=ro"',
    'conn = sqlite3.connect(uri, uri=True, timeout=3)',
    'conn.execute("PRAGMA query_only=ON")',
    'query_only = conn.execute("PRAGMA query_only").fetchone()[0] == 1',
    'conn.execute("BEGIN")',
    'table_rows = conn.execute("SELECT name FROM sqlite_master WHERE type=\'table\' LIMIT 129").fetchall()',
    'assert len(table_rows) <= 128, "Host schema table count exceeds its bound"',
    'tables = {row[0] for row in table_rows}',
    'required = {"root_tasks", "attempts", "attempt_map", "admission_outbox", "effect_results", "model_calls", "decision_model_calls", "computer_model_calls"}',
    'missing = sorted(required - tables)',
    'assert not missing, "Host schema lacks required evidence tables: " + ",".join(missing)',
    'row_presence = {name: conn.execute("SELECT EXISTS(SELECT 1 FROM " + name + " LIMIT 1)").fetchone()[0] for name in sorted(required)}',
    'roots = [{"state": row[0]} for row in conn.execute("SELECT state FROM root_tasks LIMIT 16")]',
    'assert query_only',
    'print(json.dumps({"read_only_uri": True, "query_only": query_only, "tables_checked": sorted(required), "row_presence": row_presence, "root_states": roots}))',
    'conn.rollback()',
    'conn.close()',
  ].join('\n');
  const before = fs.lstatSync(database);
  assert(before.isFile() && !before.isSymbolicLink() && before.nlink === 1);
  const stdout = execFileSync('python3', ['-c', python, database], {
    encoding: 'utf8', timeout: 8_000, maxBuffer: 64 * 1024,
  });
  const after = fs.lstatSync(database);
  assert(after.isFile() && !after.isSymbolicLink() && after.nlink === 1
    && after.dev === before.dev && after.ino === before.ino, 'Host database identity changed during read-only audit.');
  const summary = JSON.parse(stdout);
  assert(summary.read_only_uri && summary.query_only);
  assert(summary.row_presence.root_tasks === 0, 'Unexpected Host Root was created.');
  for (const table of ['attempts', 'attempt_map', 'admission_outbox', 'effect_results']) {
    assert(summary.row_presence[table] === 0, `Unexpected Host input/admission rows in ${table}.`);
  }
  for (const table of ['model_calls', 'decision_model_calls', 'computer_model_calls']) {
    assert(summary.row_presence[table] === 0, `Unexpected model call rows in ${table}.`);
  }
  assert.equal(summary.root_states.length, 0);
  return summary;
}

async function invokeFrozenTask(selection) {
  const encoded = JSON.stringify(selection);
  return js(`(async () => {
    try { await window.desktop.createFrozenTask(${encoded}); return { resolved: true }; }
    catch (error) { return { resolved: false, message: String(error?.message || error).slice(0, 768) }; }
  })()`);
}

async function expectIpcRefusal(name, selection, reasonPattern = /refused/i) {
  const outcome = await invokeFrozenTask(selection);
  assert.equal(outcome?.resolved, false, `${name} was not rejected by normal Main IPC.`);
  const message = safeText(outcome.message);
  assert(reasonPattern.test(message), `${name} did not return the expected Main refusal.`);
  result.ipc_rejections.push({ name, refused_by_main: true, message });
  const projection = await readProjection();
  assert.equal(projection.root ?? null, null, `${name} unexpectedly created a Root.`);
}

async function waitForHostRefusal() {
  const started = Date.now();
  while (Date.now() - started < 8_000) {
    const projection = await readProjection();
    const entries = Array.isArray(projection.timeline) ? projection.timeline : [];
    const refusal = entries.slice(initialTimelineLength).find((entry) => typeof entry?.text === 'string'
      && entry.text.startsWith('scoped task was refused:'));
    if (refusal) return { projection, refusal };
    if (projection.root) return { projection, refusal: null };
    await delay(100);
  }
  const projection = await readProjection();
  return { projection, refusal: null };
}

async function runDiagnostic() {
  activeStep = 'normal_main_window';
  await until(async () => window && !window.isDestroyed() ? true : null, 'normal Main window');
  await until(async () => {
    const catalog = await js('window.desktop.frozenTaskCatalog()');
    if (!catalog || catalog.status?.state === 'empty') return null;
    return catalog;
  }, 'configured frozen task catalog');
  const catalog = await js('window.desktop.frozenTaskCatalog()');
  const catalogFacts = assertCatalog(catalog);
  writeNewJson(catalogPath, catalog);
  result.catalog = {
    status: catalog.status,
    task_count: catalog.tasks.length,
    scope_compatible_count: catalogFacts.scopeCompatibleCount,
    incompatible_scope_count: catalogFacts.incompatibleScopeCount,
    scope_compatibility_meaning: 'schema expressibility only; not app or business support',
    public_file: path.basename(catalogPath),
    public_bytes: catalogFacts.bytes,
    public_sha256: crypto.createHash('sha256').update(JSON.stringify(catalog)).digest('hex'),
    contains_goal_path_reset_truth: false,
  };
  mark('normal_renderer_public_catalog_has_all_120', catalog.tasks.length === 120);

  const initialProjection = await readProjection();
  assert.equal(initialProjection.root ?? null, null, 'The isolated normal Main must begin without a Root.');
  assert.equal(initialProjection.mode, 'observe');
  initialTimelineLength = Array.isArray(initialProjection.timeline) ? initialProjection.timeline.length : 0;
  writeNewJson(projectionPaths.initial, initialProjection);
  const initialDom = await until(async () => {
    const facts = await domFacts();
    return facts.optionCount === 120 ? facts : null;
  }, 'normal renderer catalog picker');
  assert.equal(initialDom.appVisible, true);
  assert(typeof initialDom.heading === 'string' && initialDom.heading.length > 0);
  assert(typeof initialDom.catalogNote === 'string' && initialDom.catalogNote.length > 0);
  assert.equal(initialDom.optionCount, 120);
  assert.equal(initialDom.scopeIncompatibleCount, 90);
  assert(catalog.tasks.every((task, index) => task.scope_compatible
    || (initialDom.rowStates[index]?.disabled && typeof initialDom.rowStates[index]?.note === 'string'
      && initialDom.rowStates[index].note.length > 0)),
  'Every scope-incompatible public task must be disabled and have an explicit UI explanation.');
  assert.equal(initialDom.freeformTaskFieldVisible, false);
  mark('normal_ui_picker_has_120_options_and_explicit_scope_incompatibility_notes', true, {
    option_count: initialDom.optionCount,
    explicitly_scope_incompatible: initialDom.scopeIncompatibleCount,
  });

  const selected = catalogFacts.selected;
  const fakeSelector = 'f'.repeat(64);
  const fakeCatalogId = '0'.repeat(64);
  assert(!catalog.tasks.some((task) => task.selector === fakeSelector));
  assert.notEqual(selected.catalog_id, fakeCatalogId);
  const malformedCases = [
    { name: 'unknown_selector', selection: { catalog_id: selected.catalog_id, selector: fakeSelector } },
    { name: 'unknown_catalog', selection: { catalog_id: fakeCatalogId, selector: selected.selector } },
    { name: 'goal_scope_injection', selection: { catalog_id: selected.catalog_id, selector: selected.selector,
      objective: 'injected objective', allowed_scope: { application: 'injected' } } },
    { name: 'initial_state_injection', selection: { catalog_id: selected.catalog_id, selector: selected.selector,
      initial_state: { checked: false } } },
    { name: 'path_injection', selection: { catalog_id: selected.catalog_id, selector: selected.selector,
      path: '/not-an-authorized-path', output_directory: '/not-an-authorized-path' } },
  ];
  for (const testCase of malformedCases) await expectIpcRefusal(testCase.name, testCase.selection);
  mark('main_ipc_rejects_goal_catalog_initial_state_and_path_injection', result.ipc_rejections.length === malformedCases.length);

  const tasks = catalog.tasks;
  const formIndex = tasks.indexOf(catalogFacts.form);
  const set02Index = tasks.indexOf(selected);
  assert(formIndex >= 0 && set02Index >= 0);
  const beforeSelection = await domFacts();
  assert(beforeSelection.rowStates[formIndex]?.disabled, 'Scope-incompatible FORM option must remain disabled in the UI.');
  assert(typeof beforeSelection.rowStates[formIndex]?.note === 'string'
    && beforeSelection.rowStates[formIndex].note.length > 0);
  assert.equal(beforeSelection.rowStates[set02Index]?.disabled, false,
    'Schema-compatible SET-02 must be selectable in the normal UI.');

  await js(`(() => {
    const rows = document.querySelectorAll('[aria-label="Frozen task catalog"] > li');
    const option = rows[${set02Index}]?.querySelector('button');
    if (!option || option.disabled) throw new Error('SET-02 option is not enabled in the normal UI');
    option.click();
  })()`);
  const selectedDom = await until(async () => {
    const facts = await domFacts();
    return facts.rowStates[set02Index]?.pressed && facts.createButtonEnabled ? facts : null;
  }, 'normal UI selection of SET-02');

  const formOutcome = await invokeFrozenTask({
    catalog_id: catalogFacts.form.catalog_id,
    selector: catalogFacts.form.selector,
  });
  assert.equal(formOutcome?.resolved, false, 'Scope-incompatible FORM was not rejected by Main.');
  assert(/refused: form_scope_not_supported/i.test(safeText(formOutcome.message)),
    'Main did not report the expected FORM scope incompatibility refusal.');
  result.ipc_rejections.push({ name: 'scope_incompatible_form', refused_by_main: true,
    message: safeText(formOutcome.message), ui_button_disabled: true });
  const retainedSelection = await domFacts();
  assert.equal(retainedSelection.rowStates[set02Index]?.pressed, true,
    'Rejected FORM request must leave the selected SET-02 entry intact.');
  assert.equal((await readProjection()).root ?? null, null);
  mark('scope_incompatible_form_is_disabled_and_main_rejection_preserves_set02_selection', true);

  activeStep = 'normal_ui_create_frozen_task';
  await js(`(() => {
    const list = document.querySelector('[aria-label="Frozen task catalog"]');
    const panel = list?.closest('section');
    const button = panel ? [...panel.querySelectorAll('button')].find((item) => !list.contains(item)) : null;
    if (!button || button.disabled) throw new Error('normal UI create action button is unavailable');
    button.click();
  })()`);
  result.ui = {
    selection_path: 'normal renderer task-row button then normal create action button',
    task_id: selected.task_id,
    seed: selected.seed,
    catalog_id: selected.catalog_id,
    selector_sha256: selected.selector,
    ui_confirmed_selected_before_click: true,
  };
  const hostResult = await waitForHostRefusal();
  finalProjection = hostResult.projection;
  writeNewJson(projectionPaths.afterHost, finalProjection);
  const afterHostDom = await domFacts();
  result.host_response = {
    outcome: hostResult.refusal ? 'host_refused_scope' : (finalProjection.root ? 'root_created' : 'no_root_no_note'),
    user_note: hostResult.refusal ? safeText(hostResult.refusal.text) : null,
    projection_revision: finalProjection.revision,
    root_present: finalProjection.root != null,
    ui_alert: safeText(afterHostDom.hostAlert),
  };
  assert(hostResult.refusal,
    'A schema-compatible SET-02 scope must still receive a visible normal Sim app-binding decision.');
  assert.equal(finalProjection.root ?? null, null, 'Normal Sim refusal must not create a Root.');
  assert.equal(finalProjection.mode, 'observe');
  assert.equal(finalProjection.stop_status, 'not_stopped',
    'Host refusal must leave the normal session unstopped before the explicit stop probe.');
  mark('set02_reaches_host_and_is_refused_without_root_or_session_fault', true);

  const laterCatalog = await js('window.desktop.frozenTaskCatalog()');
  assert.equal(laterCatalog.status.state, 'available');
  assert.equal(laterCatalog.tasks.length, 120);
  const laterProjection = await readProjection();
  assert.equal(laterProjection.root ?? null, null);
  mark('normal_projection_and_catalog_remain_available_after_refusal', true);

  activeStep = 'readonly_host_database';
  captureNativeChildren();
  const database = findHostDatabase();
  result.database_before_stop = readOnlyLedgerSummary(database);
  mark('readonly_host_db_confirms_zero_root_input_and_model_rows', true, result.database_before_stop.row_presence);

  activeStep = 'stop_confirmation';
  const stopStatus = await js(`new Promise((resolve, reject) => {
    let settled = false;
    let remove = () => {};
    const timer = setTimeout(() => { if (!settled) { settled = true; remove(); reject(new Error('Stop confirmation timed out')); } }, 5000);
    remove = window.desktop.onStopStatus((status) => {
      if (status !== 'confirmed' && status !== 'unconfirmed') return;
      if (settled) return;
      settled = true; clearTimeout(timer); remove(); resolve(status);
    });
    window.desktop.stop().catch((error) => {
      if (settled) return;
      settled = true; clearTimeout(timer); remove(); reject(error);
    });
  })`);
  result.stop_status = stopStatus;
  assert.equal(stopStatus, 'confirmed', 'Device Stop was not confirmed.');
  const afterStop = await readProjection();
  assert.equal(afterStop.root ?? null, null);
  writeNewJson(projectionPaths.afterStop, afterStop);
  result.database_after_stop = readOnlyLedgerSummary(findHostDatabase());
  assert.deepEqual(result.database_after_stop.row_presence, result.database_before_stop.row_presence,
    'Explicit Stop must not create a Root, input, or model row.');
  mark('device_stop_confirmed_without_root_or_input', true);

  result.cleanup = {
    normal_main_quit_requested: false,
    user_data_directory: path.basename(userData),
    user_data_private_before_start: true,
    private_tmp_directory: path.basename(privateTmp),
    temp_directories_before_quit: result.temp_directories_before_quit,
    database_read_only_before_shutdown: true,
    main_pid: process.pid,
    native_child_roles: result.native_child_pids.map((entry) => entry.role).sort(),
  };
  result.ok = Object.entries(result.checks)
    .filter(([name]) => !name.endsWith('_detail'))
    .every(([, value]) => value === true)
    && result.stop_status === 'confirmed'
    && result.host_response?.outcome === 'host_refused_scope'
    && result.renderer_errors.length === 0
    && result.business_acceptance === false;
}

function writeFinalResult() {
  try {
    result.cleanup.normal_main_quit_requested = finishing && finishRequested;
    const bytes = Buffer.from(`${JSON.stringify(result, null, 2)}\n`, 'utf8');
    assert(bytes.length <= MAX_PROJECTION_BYTES);
    fs.writeFileSync(resultPath, bytes, { mode: 0o600 });
  } catch { /* The outer runner treats an empty or malformed result as failure. */ }
}

function finish(error, step = activeStep) {
  if (finishing) return;
  finishing = true;
  finishRequested = error === null;
  if (error) {
    result.ok = false;
    result.failure_step = /^[a-z_]{1,80}$/u.test(step) ? step : 'harness_error';
    result.error = safeText(error instanceof Error ? error.message : error);
  }
  try { app.quit(); }
  catch { writeFinalResult(); }
  shutdownFallback = setTimeout(() => {
    result.ok = false;
    result.failure_step ||= 'product_shutdown_timeout';
    finishRequested = false;
    app.exit(1);
  }, 15_000);
  shutdownFallback.unref();
}

app.on('browser-window-created', (_event, created) => {
  window = created;
  created.show = () => created.showInactive();
  created.webContents.on('console-message', (details) => {
    if (details.level === 'error' && result.renderer_errors?.length < 32) {
      result.renderer_errors.push(safeText(details.message));
    }
  });
  created.webContents.once('did-fail-load', (_event, code, description) => {
    if (code !== -3) void finish(new Error(`renderer failed to load: ${safeText(description)}`), 'renderer_load');
  });
  created.webContents.once('did-finish-load', () => {
    void runDiagnostic().then(() => finish(null)).catch((error) => finish(error));
  });
});

process.on('uncaughtException', (error) => finish(error, activeStep));
process.on('unhandledRejection', (error) => finish(error, activeStep));
process.on('SIGTERM', () => {
  if (finishing) {
    result.ok = false;
    result.failure_step ||= 'runner_termination_during_shutdown';
    finishRequested = false;
    app.exit(1);
    return;
  }
  finish(new Error('Outer runner requested graceful termination.'), 'runner_termination');
});
process.on('exit', (code) => {
  if (shutdownFallback) clearTimeout(shutdownFallback);
  result.exit_code = code;
  writeFinalResult();
});

setTimeout(() => finish(new Error('frozen catalog Main smoke timed out.'), activeStep), 55_000).unref();

async function importNormalMain() {
  const mainPath = path.join(root, 'apps/desktop/out/main/index.js');
  assert(fs.lstatSync(mainPath).isFile() && !fs.lstatSync(mainPath).isSymbolicLink());
  activeStep = 'normal_main_import';
  await import(require('node:url').pathToFileURL(mainPath).href);
}

void importNormalMain().catch((error) => finish(error, 'normal_main_import'));
