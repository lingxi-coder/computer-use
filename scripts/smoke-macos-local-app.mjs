// Explicit installed-app diagnostic. The normal packaged entry runs unchanged.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { existsSync, lstatSync, mkdirSync, realpathSync, writeFileSync } from 'node:fs';
import { isAbsolute, join, resolve } from 'node:path';
import { parseArgs, promisify } from 'node:util';

const { values } = parseArgs({ options: { app: { type: 'string' }, output: { type: 'string' } } });
assert(isAbsolute(values.app ?? '') && isAbsolute(values.output ?? ''), 'Supply absolute --app and new --output.');
const bundle = realpathSync(values.app);
const output = resolve(values.output);
assert(!existsSync(output), 'Output must not exist.');
mkdirSync(output, { mode: 0o700 });
mkdirSync(join(output, 'user-data'), { mode: 0o700 });
const runtime = join(bundle, 'Contents/MacOS/Computer Use');
assert(lstatSync(runtime).isFile());
const resources = join(bundle, 'Contents/Resources');
const record = { ok: false, app: bundle, input_calls: 0, provider_calls: 0, owners: [], reads: [], checks: {}, failure: null };
const save = () => writeFileSync(join(output, 'result.json'), JSON.stringify(record, null, 2) + '\n', { mode: 0o600 });
const delay = ms => new Promise(done => setTimeout(done, ms));
const run = promisify(execFile);
const env = { ...process.env, CU_SOURCE_BACKEND: 'fixture', ELECTRON_RENDERER_URL: '' };
for (const key of Object.keys(env)) {
  if (key.startsWith('CU_') && key !== 'CU_SOURCE_BACKEND'
    || /API.?KEY|ACCESS.?TOKEN|(?:OPENAI|ANTHROPIC)_|NODE_OPTIONS|ELECTRON_RUN_AS_NODE|DYLD_/i.test(key)) delete env[key];
}
const child = spawn(runtime, ['--inspect-brk=127.0.0.1:0'], { cwd: output, env, stdio: ['ignore', 'pipe', 'pipe'] });
record.electron_pid = child.pid;
let exited = false, exitCode = null, stderr = '', ws;
const exit = new Promise(done => child.once('exit', code => { exited = true; exitCode = code; done(); }));
child.once('error', error => { record.failure = error.message; exited = true; save(); });
child.stderr.on('data', bytes => { stderr = (stderr + bytes.toString()).slice(-32_768); });
child.stdout.on('data', () => {});
const timeout = setTimeout(() => child.kill('SIGTERM'), 110_000);
const killTimeout = setTimeout(() => child.kill('SIGKILL'), 120_000);
const ownedPids = new Set([child.pid]);
let client;
let stage = 'private_inspector';
async function until(read, label, ms = 30_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const value = await read();
    if (value) return value;
    assert(!exited, `Installed app exited while waiting for ${label}.`);
    await delay(100);
  }
  throw new Error(`Timeout: ${label}`);
}
function inspector(socket) {
  let sequence = 0;
  let paused = false;
  let frame = null;
  const scripts = new Map();
  const pending = new Map();
  socket.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.method === 'Debugger.scriptParsed') scripts.set(message.params.scriptId, message.params.url);
    if (message.method === 'Debugger.paused') { paused = true; frame = message.params.callFrames[0]; }
    if (message.method === 'Debugger.resumed') paused = false;
    const wait = pending.get(message.id);
    if (!wait) return;
    pending.delete(message.id); clearTimeout(wait.timer);
    message.error ? wait.reject(new Error(message.error.message)) : wait.resolve(message.result);
  });
  socket.addEventListener('close', () => {
    for (const wait of pending.values()) { clearTimeout(wait.timer); wait.reject(new Error('Inspector closed.')); }
    pending.clear();
  });
  const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Inspector timeout: ${method}`)); }, 45_000);
    pending.set(id, { resolve, reject, timer }); socket.send(JSON.stringify({ id, method, params }));
  });
  call.isPaused = () => paused;
  call.pausedFrame = () => frame;
  call.scriptUrl = id => scripts.get(id);
  return call;
}
async function evaluate(expression) {
  // Inspector keeps promises weakly. Retain the pending native/renderer promise
  // in the inspected context until its response has been delivered.
  const reply = await client('Runtime.evaluate', {
    expression: `globalThis.__cuInspectorPending = Promise.resolve((${expression}))`,
    awaitPromise: true, returnByValue: true,
  });
  assert(!reply.exceptionDetails, reply.exceptionDetails?.exception?.description?.slice(0, 1024)
    ?? reply.exceptionDetails?.text ?? 'Inspector evaluation failed.');
  return reply.result?.value;
}
const electron = `process.getBuiltinModule('module').createRequire(${JSON.stringify(join(resources, 'app/out/main/index.js'))})('electron')`;
const renderer = source => evaluate(`(${electron}).BrowserWindow.getAllWindows()[0].webContents.executeJavaScript(${JSON.stringify(source)})`);
async function census() {
  const { stdout } = await run('ps', ['-axo', 'pid=,args='], { timeout: 4000, maxBuffer: 8 * 1024 * 1024 });
  return stdout.split('\n').flatMap(line => {
    const match = /^\s*(\d+)\s+(.*)$/.exec(line);
    if (!match) return [];
    const pid = Number(match[1]);
    const owned = ownedPids.has(pid) || match[2].includes(`${resources}/bin/cu-`)
      || record.owners.some(owner => match[2].includes(`--user-data-dir=${owner.profileParent}/cu-headless-owned-`));
    if (owned) ownedPids.add(pid);
    return owned ? [{ pid, args: match[2] }] : [];
  });
}
try {
  const url = await until(() => /Debugger listening on (ws:\/\/127\.0\.0\.1:\d+\/[a-z0-9-]+)/.exec(stderr)?.[1], 'private inspector');
  ws = new WebSocket(url);
  await new Promise((done, reject) => { ws.addEventListener('open', done, { once: true }); ws.addEventListener('error', reject, { once: true }); });
  client = inspector(ws);
  await client('Runtime.enable');
  await client('Debugger.enable');
  await client('Runtime.runIfWaitingForDebugger');
  await until(() => client.isPaused(), 'application entry breakpoint');
  const frame = client.pausedFrame();
  stage = 'isolated_test_setup';
  record.entry_breakpoint = { url: frame.url || client.scriptUrl(frame.location.scriptId), function_name: frame.functionName, location: frame.location };
  const setup = await client('Debugger.evaluateOnCallFrame', { callFrameId: frame.callFrameId,
    returnByValue: true, expression: `(() => {
    const {app}=(${electron}); app.setPath('userData', ${JSON.stringify(join(output, 'user-data'))});
    app.on('browser-window-created',(_event,window)=>{window.show=()=>window.showInactive()});
    globalThis.__cuPackageEvidence={owners:[],reads:[],executions:0,utility_pids:[]};
    process.on('computer-use:browser-owner',v=>{if(__cuPackageEvidence.owners.length<8)__cuPackageEvidence.owners.push(v)});
    process.on('computer-use:browser-snapshot',v=>{if(__cuPackageEvidence.reads.length<16)__cuPackageEvidence.reads.push({source_sequence:v.source_sequence,observed_ms:v.observed_ms,read_finished_ms:v.read_finished_ms,fixture_file_url:v.context.fixture_file_url,node_count:v.nodes.length})});
    process.on('computer-use:browser-execution',()=>__cuPackageEvidence.executions++);
    return {packaged:app.isPackaged,resources:process.resourcesPath,version:process.versions.electron,
      test_console_presentation:'showInactive',ordinary_startup_focus_claimed:false};
  })()` });
  assert(!setup.exceptionDetails, setup.exceptionDetails?.exception?.description ?? setup.exceptionDetails?.text);
  record.runtime = setup.result?.value;
  assert.equal(record.runtime?.packaged, true); assert.equal(record.runtime?.resources, resources);
  await client('Debugger.resume');
  await client('Debugger.disable');
  stage = 'electron_app_ready';
  // The first paused module can be a bundled dependency. BrowserWindow's lazy
  // getter must not initialize native window classes during Electron bootstrap.
  await until(() => evaluate(`(${electron}).app.isReady()`), 'Electron app readiness');
  stage = 'packaged_renderer';
  await until(() => evaluate(`(${electron}).BrowserWindow.getAllWindows().some(w=>!w.webContents.isLoading())`), 'packaged renderer');
  await until(() => renderer("typeof window.desktop === 'object' && !!window.desktop.snapshot"), 'real preload');
  await until(() => renderer('window.desktop.snapshot()'), 'initial Host projection');
  stage = 'installed_local_model';
  // This uses the shipped utility worker and its real initialization protocol.
  record.local_model = await evaluate(`(async () => {
    const {utilityProcess}=(${electron});
    const worker=utilityProcess.fork(${JSON.stringify(join(resources, 'app/out/main/localDecisionWorker.js'))},[],{stdio:'ignore',serviceName:'Package Laya initialization',env:{PATH:process.env.PATH,HOME:process.env.HOME,TMPDIR:process.env.TMPDIR}});
    const frame=v=>{const json=Buffer.from(JSON.stringify(v));const b=Buffer.alloc(json.length+4);b.writeUInt32BE(json.length);json.copy(b,4);return b.buffer.slice(b.byteOffset,b.byteOffset+b.byteLength)};
    return await new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>{worker.kill();reject(new Error('Model initialization timeout'))},40000);
      worker.on('error',e=>{clearTimeout(timer);worker.kill();reject(e)});
      worker.once('spawn',()=>{globalThis.__cuPackageEvidence.utility_pids.push(worker.pid);worker.postMessage(frame({version:1,type:'init',bundleManifestPath:${JSON.stringify(join(resources, 'models/laya-multilingual/e4e9ddf21a7b1903b7acffd8814ad4307bf63a67/package-manifest.json'))},numThreads:2,minimumConfidenceMilli:950}))});
      worker.once('message',message=>{const b=Buffer.from(message);const reply=JSON.parse(b.subarray(4).toString());const pid=worker.pid;clearTimeout(timer);worker.once('exit',code=>resolve({reply,pid,exit_code:code}));worker.postMessage(frame({version:1,type:'close'}));setTimeout(()=>worker.kill(),3000).unref()});
    });
  })()`);
  ownedPids.add(record.local_model.pid);
  assert.equal(record.local_model.reply.type, 'ready');
  assert.equal(record.local_model.reply.model_revision, 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67');
  assert.equal(record.local_model.exit_code, 0);
  record.checks.packaged_onnx_model_initialization = true;
  record.native_processes = (await census()).filter(p => p.args.includes(`${resources}/bin/cu-`))
    .map(p => ({ pid: p.pid, executable: ['cu-host', 'cu-device', 'cu-source']
      .map(name => join(resources, 'bin', name)).find(path => p.args.includes(path)) }));
  assert.equal(record.native_processes.length, 3);
  stage = 'installed_browser_source';
  await renderer(`(() => {const b=[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Use fixture browser'); if(!b||b.disabled)throw new Error('No profile picker');b.click()})()`);
  record.browser_projection = await until(async () => {
    const p = await renderer('window.desktop.snapshot()');
    return p?.browser_observation ? p : null;
  }, 'installed fixture Source');
  const evidence = await evaluate('globalThis.__cuPackageEvidence');
  record.owners = evidence.owners; record.reads = evidence.reads;
  assert.equal(evidence.executions, 0); assert(record.reads.length > 0);
  assert(record.reads.every(r => r.fixture_file_url === new URL(`file://${resources}/fixtures/headless/readonly-page.html`).href));
  await census();
  record.checks.installed_source = true;
  stage = 'confirmed_stop';
  await renderer('window.desktop.stop()');
  record.stop = await until(async () => {
    const state = await renderer(`(async () => ({ projection: await window.desktop.snapshot(),
      banner: document.querySelector('.banner.ok')?.textContent ?? '' }))()`);
    return state.projection?.browser_observation == null && state.banner.includes('Stopped')
      ? { browser_scope_invalidated: true, visible_confirmed_banner: state.banner } : null;
  }, 'Device-confirmed Stop and scope invalidation');
  record.checks.public_stop = true;
} catch (error) {
  record.failure_stage = stage;
  record.failure = error instanceof Error ? error.message : String(error);
} finally {
  // Collect ownership even when profile startup or model initialization failed.
  try {
    if (client && !exited) {
      const evidence = await evaluate('globalThis.__cuPackageEvidence');
      if (evidence) {
        record.owners = evidence.owners; record.reads = evidence.reads;
        record.utility_pids = evidence.utility_pids;
        for (const pid of evidence.utility_pids) if (Number.isSafeInteger(pid) && pid > 0) ownedPids.add(pid);
      }
      await census();
      record.final_ui = await renderer(`document.body.innerText.slice(0,4096)`);
      record.final_projection = await renderer('window.desktop.snapshot()');
    }
  } catch (error) { record.failure ??= `Ownership capture: ${error.message}`; }
  try { if (client && !exited) { await evaluate(`(${electron}).app.quit()`); ws.close(); } }
  catch (error) { record.failure ??= `Quit: ${error.message}`; ws?.close(); }
  await Promise.race([exit, delay(15_000)]);
  if (!exited) { record.failure ??= 'Installed app shutdown timed out.'; child.kill('SIGTERM'); await Promise.race([exit, delay(5000)]); }
  if (!exited) { child.kill('SIGKILL'); await Promise.race([exit, delay(5000)]); }
  clearTimeout(timeout); clearTimeout(killTimeout);
  try {
    const remaining = await census();
    record.cleanup = { exit_code: exitCode, remaining_pids: remaining.map(p=>p.pid), owned_pids: [...ownedPids],
      remaining_directories: record.owners.flatMap(o=>[o.profileParent,o.supervisorDir]).filter(existsSync) };
    assert(exited && exitCode === 0 && remaining.length === 0 && record.cleanup.remaining_directories.length === 0, 'Owned cleanup failed.');
  } catch (error) { record.failure ??= error.message; }
  record.ok = record.failure === null && record.checks.installed_source && record.checks.packaged_onnx_model_initialization && record.checks.public_stop;
  record.diagnostic_stderr_tail = stderr.slice(-8192).replace(/[a-f0-9]{32,}/gi,'[redacted]');
  save();
}
console.log(JSON.stringify({ ok: record.ok, output, failure: record.failure }));
if (!record.ok) process.exitCode = 1;
