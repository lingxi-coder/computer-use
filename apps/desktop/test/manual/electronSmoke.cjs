// Invoked explicitly by scripts/smoke-electron.mts; ordinary unit tests never open a window.
const fs = require('node:fs');
const path = require('node:path');
const { execFile, spawn } = require('node:child_process');
const { app, globalShortcut } = require('electron');

const root = process.env.CU_SMOKE_PROJECT_ROOT;
const artifactDir = process.env.CU_SMOKE_OUTPUT_DIR;
if (!root || !artifactDir) throw new Error('Run this fixture through scripts/smoke-electron.mts');
const nativePid = process.env.CU_SMOKE_NATIVE_PID;
const requireNativeFrame = process.env.CU_SMOKE_REQUIRE_NATIVE_FRAME === '1';
const inspectNativePermissions = process.env.CU_SMOKE_NATIVE_PERMISSIONS === '1';
const inspectNativeWatch = process.env.CU_SMOKE_NATIVE_WATCH_DIAGNOSTIC === '1';
const nativeUi = !!nativePid && !inspectNativeWatch;
const holdMs = Number(process.env.CU_SMOKE_HOLD_MS || 0);
const resultFile = path.join(artifactDir, 'result.json');
const screenshotFile = path.join(artifactDir, 'window.png');
const result = { electron: process.versions.electron, consoleErrors: [], navigations: [], checks: [] };
let finished = false;

app.setName('Computer Use Smoke');
app.setPath('userData', path.join(artifactDir, 'user-data'));
process.env.CU_BIN_DIR = path.join(root, 'target/debug');
process.env.CU_SOURCE_BACKEND = nativeUi ? 'native' : 'fixture';
if (nativeUi) {
  process.env.CU_TARGET_BUNDLE_ID = 'com.lingxi.computer-user.m2-fixture';
  process.env.CU_TARGET_PID = nativePid;
  process.env.CU_NATIVE_SOURCE_HELPER = path.join(root, 'target/debug/cu-macos-source');
}
if (process.env.CU_SMOKE_RENDERER_URL) process.env.ELECTRON_RENDERER_URL = process.env.CU_SMOKE_RENDERER_URL;
else delete process.env.ELECTRON_RENDERER_URL;

function check(condition, name) {
  if (!condition) throw new Error(name);
  result.checks.push(name);
}

async function until(read, description) {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    if (await read()) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error('Timed out: ' + description);
}

async function nativePermissions() {
  const output = await new Promise((resolve, reject) => {
    const child = execFile(path.join(root, 'target/debug/cu-macos-source'), [], { timeout: 5000, maxBuffer: 16_384, killSignal: 'SIGKILL' }, (error, stdout) => {
      if (error) reject(error);
      else resolve(stdout);
    });
    // This command only uses AXIsProcessTrusted / CGPreflightScreenCaptureAccess.
    // It never requests access and does not capture an application.
    child.stdin.end(JSON.stringify({ protocol_version: 1, command: 'permissions' }) + '\n');
  });
  const permissions = JSON.parse(output);
  check(permissions.protocol_version === 1 && permissions.kind === 'permissions'
    && typeof permissions.accessibility === 'boolean' && typeof permissions.screen_capture === 'boolean', 'native passive permission response validated');
  result.nativePermissions = { accessibility: permissions.accessibility, screen_capture: permissions.screen_capture };
}

async function nativeWatchDiagnostic() {
  const assets = path.join(artifactDir, 'native-watch-assets');
  fs.mkdirSync(assets, { mode: 0o700 });
  try {
    await new Promise((resolve, reject) => {
      const child = spawn(path.join(root, 'target/debug/cu-macos-source'), [], { stdio: ['pipe', 'pipe', 'pipe'] });
      let buffer = '';
      let bytes = 0;
      let failure;
      const terminate = setTimeout(() => child.kill('SIGTERM'), 5000);
      const kill = setTimeout(() => child.kill('SIGKILL'), 8000);
      const fail = (error) => { failure = error; child.kill('SIGTERM'); };
      child.stdout.on('data', (chunk) => {
        bytes += chunk.length;
        if (bytes > 1_048_576) return fail(new Error('Diagnostic watch output exceeded its bound'));
        buffer += chunk;
        let newline;
        while ((newline = buffer.indexOf('\n')) !== -1) {
          const line = buffer.slice(0, newline);
          buffer = buffer.slice(newline + 1);
          if (!line) continue;
          try {
            const record = JSON.parse(line);
            if (record.kind === 'observation') {
              if (record.structure.app.bundle_id !== 'com.lingxi.computer-user.m2-fixture' || record.structure.app.pid !== Number(nativePid)) throw new Error('Diagnostic watch did not observe the owned fixture');
              const reading = {
                received_unix_ms: record.received_unix_ms, permissions: record.permissions,
                complete: record.structure.complete, incomplete_reason: record.structure.incomplete_reason ?? null,
                active_layer: record.structure.active_layer ?? null, windows: record.structure.windows,
                window_roots: record.structure.objects.filter((object) => object.parent == null).map(({ window_id, role, subrole }) => ({ window_id, role, subrole })),
                observer_available: record.observer_available, observer_reason: record.observer_reason ?? null,
              };
              result.nativeWatchDiagnostics ??= { ...reading, initial_observations: [], health: [] };
              if (result.nativeWatchDiagnostics.initial_observations.length < 3) result.nativeWatchDiagnostics.initial_observations.push(reading);
            } else if (record.kind === 'health' && result.nativeWatchDiagnostics) {
              result.nativeWatchDiagnostics.health.push({ source: record.source, state: record.state, reason: record.reason ?? null });
            } else if (record.kind === 'frame' && record.frame.status === 'complete' && result.nativeWatchDiagnostics) {
              result.nativeWatchDiagnostics.first_frame = 'complete';
              child.kill('SIGTERM');
            }
          } catch (error) { fail(error); }
        }
      });
      child.stderr.resume();
      child.once('error', (error) => { clearTimeout(terminate); clearTimeout(kill); reject(error); });
      child.once('close', () => {
        clearTimeout(terminate); clearTimeout(kill);
        if (failure) reject(failure);
        else if (!result.nativeWatchDiagnostics) reject(new Error('Diagnostic watch returned no owned AX observation'));
        else resolve();
      });
      child.stdin.end(JSON.stringify({ protocol_version: 1, command: 'watch', bundle_id: 'com.lingxi.computer-user.m2-fixture', pid: Number(nativePid), output_dir: assets, timeout_ms: 1500, interval_ms: 1000 }) + '\n');
    });
    check(!!result.nativeWatchDiagnostics, 'owned native watch first AX diagnostic retained only in temporary result');
  } finally { fs.rmSync(assets, { recursive: true, force: true }); }
}

async function finish(error, window) {
  if (finished) return;
  finished = true;
  result.ok = !error;
  if (error) result.error = error.stack || String(error);
  if (window && !window.isDestroyed()) {
    try {
      fs.writeFileSync(screenshotFile, (await window.webContents.capturePage()).toPNG());
      result.screenshot = screenshotFile;
    } catch (captureError) { result.screenshotError = String(captureError); }
  }
  fs.writeFileSync(resultFile, JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
  app.quit();
  setTimeout(() => app.exit(error ? 1 : 0), 5000).unref();
}

process.on('uncaughtException', (error) => void finish(error));
setTimeout(() => void finish(new Error('Smoke timed out')), 45000 + holdMs).unref();

app.on('browser-window-created', (_event, window) => {
  const contents = window.webContents;
  contents.on('will-navigate', (event, url) => queueMicrotask(() => {
    result.navigations.push({ url, prevented: event.defaultPrevented });
  }));
  contents.on('console-message', (details) => {
    if (details.level === 'error') result.consoleErrors.push(details.message);
  });
  contents.on('preload-error', (_event, preloadPath, error) => {
    result.consoleErrors.push('preload: ' + preloadPath + ': ' + String(error));
  });
  contents.once('did-finish-load', () => void (async () => {
    await until(() => contents.executeJavaScript('!!document.querySelector(".app") && typeof window.desktop?.snapshot === "function"'), 'renderer and preload');
    check(process.versions.electron === process.env.CU_SMOKE_EXPECTED_ELECTRON, 'runtime matches the pinned Electron version');
    const preferences = contents.getLastWebPreferences();
    check(preferences.sandbox === true && preferences.contextIsolation === true && preferences.nodeIntegration === false, 'renderer remains sandboxed and isolated');
    check(await contents.executeJavaScript('typeof window.require === "undefined" && typeof window.process === "undefined"'), 'renderer has no Node access');
    check(globalShortcut.isRegistered('CommandOrControl+Shift+Escape'), 'global stop shortcut registered');
    if (inspectNativePermissions || inspectNativeWatch) await nativePermissions();
    if (inspectNativeWatch) await nativeWatchDiagnostic();
    if (holdMs) {
      console.log('CU_SMOKE_HOLDING');
      await new Promise((resolve) => setTimeout(resolve, holdMs));
    }
    if (nativeUi) {
      await until(() => contents.executeJavaScript('window.desktop.snapshot().then((snapshot) => snapshot.observation?.sources.length > 0)'), 'native source health projection');
      let snapshot;
      if (requireNativeFrame) {
        await until(async () => {
          snapshot = await contents.executeJavaScript('window.desktop.snapshot()');
          // Specific terminal causes are retained only in the temporary result; the proof
          // artifact contains source states and owned-fixture frame dimensions, never text.
          result.nativeDiagnostics = snapshot.observation?.sources.map(({ source, state, reason }) => ({ source, state, reason })) ?? [];
          return snapshot.observation?.target_app === 'com.lingxi.computer-user.m2-fixture'
            && snapshot.observation.frame?.healthy === true
            && snapshot.observation.sources.some(({ source, state }) => source === 'screenshot' && state === 'available');
        }, 'healthy owned native fixture frame');
      }
      snapshot ??= await contents.executeJavaScript('window.desktop.snapshot()');
      check(snapshot.capabilities.backend === 'macos' && snapshot.capabilities.input === 'unavailable', 'native observation has no input capability');
      check(await contents.executeJavaScript('document.querySelector(".read-only")?.textContent === "Read only" && document.querySelector("option[value=execute]").disabled'), 'read-only state is displayed and Execute is disabled');
      check(await contents.executeJavaScript('document.querySelectorAll(".observation .source").length > 0'), 'native source health is rendered');
      result.observation = {
        sources: snapshot.observation.sources.map(({ source, state }) => ({ source, state })),
        frameStatus: snapshot.observation.frame?.status ?? null,
        ownsTarget: snapshot.observation.target_app === 'com.lingxi.computer-user.m2-fixture',
        healthyFrame: snapshot.observation.frame?.healthy ?? false,
        dimensions: snapshot.observation.frame ? { width: snapshot.observation.frame.width_px, height: snapshot.observation.frame.height_px } : null,
      };
      if (requireNativeFrame) {
        const frame = snapshot.observation.frame;
        check(result.observation.ownsTarget && frame.healthy && snapshot.observation.sources.some(({ source, state }) => source === 'screenshot' && state === 'available'), 'healthy owned-fixture screenshot reaches the Operator');
        check(frame.width_px > 0 && frame.height_px > 0 && frame.pixel_age_ms != null && frame.confirmation_age_ms != null, 'native frame has dimensions and both ages');
        await until(() => contents.executeJavaScript('document.querySelector(".frame-facts") && !document.querySelector(".frame-facts").textContent.includes("freshness unconfirmed")'), 'healthy frame rendered');
        const size = `${frame.width_px} × ${frame.height_px} px`;
        check(await contents.executeJavaScript(`Array.from(document.querySelectorAll(".frame-facts dd")).some((item) => item.textContent === ${JSON.stringify(size)})`), 'native frame dimensions are rendered');
        check(await contents.executeJavaScript('Array.from(document.querySelectorAll(".frame-facts dt")).map((item) => item.textContent).includes("Capture checked") && Array.from(document.querySelectorAll(".frame-facts dd")).every((item) => item.textContent !== "Unknown")'), 'native image and confirmation ages are rendered');
        const partial = snapshot.observation.sources.some(({ state }) => state !== 'available');
        if (partial) {
          check(await contents.executeJavaScript('document.querySelector(".observation-state.warning")?.textContent === "Observation partly available" && document.querySelector(".source.degraded, .source.unavailable") !== null'), 'partial source availability remains visible with a healthy frame');
          const reasons = snapshot.observation.sources.filter(({ state, reason }) => state !== 'available' && reason).map(({ reason }) => reason);
          if (reasons.length) check(await contents.executeJavaScript(`(() => { const visible = Array.from(document.querySelectorAll(".source-reason")).map((item) => item.textContent); return ${JSON.stringify(reasons)}.every((reason) => visible.includes(reason)); })()`), 'specific source degradation reasons are rendered');
        } else check(await contents.executeJavaScript('document.querySelector(".observation-state.ready")?.textContent === "Observing"'), 'available observation is rendered');
        await until(() => contents.executeJavaScript('(() => { const image = document.querySelector(".observation-image"); return image?.complete && image.naturalWidth > 0 && image.naturalHeight > 0 && image.src.startsWith("blob:"); })()'), 'binary PNG preview decoded');
        const preview = await contents.executeJavaScript('(() => { const image = document.querySelector(".observation-image"); return { width: image.naturalWidth, height: image.naturalHeight }; })()');
        check(preview.width === frame.width_px && preview.height === frame.height_px, 'native PNG pixels are decoded through the binary channel');
        result.binaryImage = { transport: 'CUIMG001/png', decoded: true, dimensions: preview };
        result.observationScreenshot = path.join(artifactDir, 'observation-window.png');
        fs.writeFileSync(result.observationScreenshot, (await contents.capturePage()).toPNG());
      }
    } else {
      await contents.executeJavaScript('window.desktop.createTask(\'Turn off "Daily picks" in "Push"\')');
      await until(() => contents.executeJavaScript('!!document.querySelector(".prompt")'), 'goal confirmation rendered');
      await contents.executeJavaScript('Array.from(document.querySelectorAll("button")).find((button) => button.textContent.trim() === "Yes, do it").click()');
      await until(() => contents.executeJavaScript('document.querySelector(".state")?.textContent === "Ready"'), 'observe-mode task stays ready');
    }
    window.close();
    await until(() => !window.isVisible(), 'window hides on close');
    check(!window.isDestroyed(), 'closing preserves the window and renderer');
    check((await contents.executeJavaScript('window.desktop.snapshot()')).mode === 'observe', 'IPC works while the console is hidden');
    app.emit('activate');
    await until(() => window.isVisible(), 'reopened via activation');
    check(window.isVisible(), 'closed console can reopen');
    if (nativeUi) await contents.executeJavaScript('window.desktop.stop()');
    else {
      await contents.executeJavaScript('document.querySelector("button.stop").scrollIntoView(); document.querySelector("button.stop").click()');
      await until(() => contents.executeJavaScript('window.desktop.snapshot().then((snapshot) => snapshot.root?.state === "cancelled")'), 'stop acknowledgement and cancellation');
    }
    await until(() => contents.executeJavaScript('document.querySelector(".banner.ok")?.textContent.includes("Stopped")'), 'device-confirmed stop banner');
    check(await contents.executeJavaScript('document.querySelector(".banner.ok")?.textContent.includes("Stopped")'), 'device stop acknowledgement is rendered');
    check((await contents.executeJavaScript('window.desktop.snapshot()')).mode === 'observe', 'smoke never enables actual input');
    check(result.consoleErrors.length === 0, 'no renderer, preload or CSP errors');
    await finish(null, window);
  })().catch((error) => finish(error, window)));
});

import(path.join(root, 'apps/desktop/out/main/index.js')).catch((error) => finish(error));
