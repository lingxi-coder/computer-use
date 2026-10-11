// Explicit manual smoke. Uses an existing runtime and never downloads or installs one.
// node --import tsx scripts/smoke-electron.mts --runtime /path/to/Electron --archive /path/to/electron-v43.1.1-darwin-arm64.zip [--dev] [--native-permissions] [--native-pid PID --require-native-frame]
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

import react from '@vitejs/plugin-react';
import { createServer, type ViteDevServer } from 'vite';

import { developmentCspPlugin } from '../apps/desktop/src/main/rendererSecurity.js';

import { reconcileNativeFrameEvidence } from './electron-evidence.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: {
  runtime: { type: 'string' }, archive: { type: 'string' }, dev: { type: 'boolean' }, 'native-pid': { type: 'string' }, 'require-native-frame': { type: 'boolean' },
  'native-permissions': { type: 'boolean' },
  'native-watch-diagnostic': { type: 'boolean' },
  'hold-ms': { type: 'string' },
} });
if (!values.runtime || !isAbsolute(values.runtime) || !existsSync(values.runtime)) throw new Error('--runtime must be an existing absolute Electron executable path');
const expectedVersion = JSON.parse(readFileSync(join(root, 'node_modules/electron/package.json'), 'utf8')).version;
let archiveSha: string | null = null;
if (values.archive) {
  const checksums = JSON.parse(readFileSync(join(root, 'node_modules/electron/checksums.json'), 'utf8'));
  const expectedSha = checksums[basename(values.archive)];
  if (!expectedSha) throw new Error('The archive is not listed by the pinned Electron package');
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(values.archive)) hash.update(chunk);
  archiveSha = hash.digest('hex');
  if (archiveSha !== expectedSha) throw new Error('Electron archive checksum mismatch');
}
const nativePid = values['native-pid'];
if (nativePid && (!/^\d+$/.test(nativePid) || Number(nativePid) < 1 || Number(nativePid) > 0xffff_ffff)) throw new Error('--native-pid must select the owned M2 fixture by its positive PID');
if (values['require-native-frame'] && !nativePid) throw new Error('--require-native-frame requires --native-pid');
if (values['native-watch-diagnostic'] && (!nativePid || values['require-native-frame'])) throw new Error('--native-watch-diagnostic requires --native-pid and cannot be used with --require-native-frame');
const holdMs = Number(values['hold-ms'] ?? 0);
if (values['hold-ms'] && (!/^\d+$/.test(values['hold-ms']) || !Number.isSafeInteger(holdMs) || holdMs > 30_000)) throw new Error('--hold-ms must be between 0 and 30000');
const outputDir = mkdtempSync(join(tmpdir(), 'cu-electron-smoke-'));
let server: ViteDevServer | undefined;
try {
  let rendererUrl: string | undefined;
  if (values.dev) {
    server = await createServer({
      configFile: false, root: join(root, 'apps/desktop/src/renderer'),
      cacheDir: join(outputDir, 'vite-cache'),
      plugins: [react(), developmentCspPlugin()], server: { host: '127.0.0.1', port: 0, strictPort: true },
    });
    await server.listen();
    const address = server.httpServer!.address();
    if (!address || typeof address === 'string') throw new Error('No development server address');
    rendererUrl = `http://127.0.0.1:${address.port}`;
  }
  const child = spawn(values.runtime, [join(root, 'apps/desktop/test/manual/electronSmoke.cjs')], {
    cwd: root, stdio: ['ignore', 'pipe', 'pipe'],
    env: {
      ...process.env, CU_SMOKE_PROJECT_ROOT: root, CU_SMOKE_OUTPUT_DIR: outputDir, CU_SMOKE_EXPECTED_ELECTRON: expectedVersion,
      ...(rendererUrl ? { CU_SMOKE_RENDERER_URL: rendererUrl } : { CU_SMOKE_RENDERER_URL: '' }),
      ...(nativePid ? { CU_SMOKE_NATIVE_PID: nativePid } : { CU_SMOKE_NATIVE_PID: '' }),
      CU_SMOKE_REQUIRE_NATIVE_FRAME: values['require-native-frame'] ? '1' : '',
      CU_SMOKE_NATIVE_PERMISSIONS: values['native-permissions'] ? '1' : '',
      CU_SMOKE_NATIVE_WATCH_DIAGNOSTIC: values['native-watch-diagnostic'] ? '1' : '',
      CU_SMOKE_HOLD_MS: String(holdMs),
    },
  });
  let diagnostics = '';
  child.stdout.on('data', (chunk) => {
    diagnostics = (diagnostics + chunk).slice(-16_384);
    if (chunk.toString().includes('CU_SMOKE_HOLDING')) console.log('CU_SMOKE_HOLDING');
  });
  child.stderr.on('data', (chunk) => { diagnostics = (diagnostics + chunk).slice(-16_384); });
  const timeout = setTimeout(() => child.kill('SIGTERM'), 60_000 + holdMs);
  const code = await new Promise<number | null>((done, fail) => {
    child.once('error', fail);
    child.once('close', done);
  }).finally(() => clearTimeout(timeout));
  const resultPath = join(outputDir, 'result.json');
  if (!existsSync(resultPath)) throw new Error(`Electron smoke exited (${code}) without a result: ${diagnostics}`);
  const result = JSON.parse(readFileSync(resultPath, 'utf8'));
  if (!result.ok) writeFileSync(join(outputDir, 'diagnostics.log'), diagnostics);
  const proofPath = join(root, 'artifacts/m2-electron-runtime.json');
  const proof = existsSync(proofPath) ? JSON.parse(readFileSync(proofPath, 'utf8')) : { runs: {} };
  proof.runtime = { version: result.electron, pinned_version: expectedVersion, archive_sha256: archiveSha, archive_verified: archiveSha !== null };
  const runKind = values['native-watch-diagnostic'] ? '_native_watch_diagnostic'
    : nativePid ? values['require-native-frame'] ? '_native_frame' : '_native_observation'
    : values['native-permissions'] ? '_permissions' : '_fixture';
  const runName = `${values.dev ? 'development' : 'production'}${runKind}`;
  const previousRun = proof.runs[runName];
  const run = {
    ok: result.ok, checks: result.checks, renderer_errors: result.consoleErrors.length,
    requires_native_frame: values['require-native-frame'] ?? false,
    native_permissions: result.nativePermissions ?? null,
    native_watch_diagnostic_received: !!result.nativeWatchDiagnostics,
    hold_ms: holdMs,
    binary_image: result.binaryImage ?? null,
    observation: result.observation ?? null, screenshot_created: !!result.screenshot,
    healthy_frame_screenshot_created: !!result.observationScreenshot, tested_at_utc: new Date().toISOString(),
  };
  proof.runs[runName] = run;
  reconcileNativeFrameEvidence(proof, runName, previousRun, run, code);
  mkdirSync(dirname(proofPath), { recursive: true });
  writeFileSync(proofPath, JSON.stringify(proof, null, 2) + '\n');
  console.log(JSON.stringify({ ok: result.ok, proof: proofPath, screenshot: result.screenshot, observationScreenshot: result.observationScreenshot, error: result.error }));
  if (!result.ok || code !== 0) throw new Error(`Electron smoke failed: ${result.error ?? diagnostics}`);
} finally {
  await server?.close();
}
