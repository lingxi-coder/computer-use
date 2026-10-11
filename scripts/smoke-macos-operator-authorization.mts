import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, renameSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: { runtime: { type: 'string' }, execute: { type: 'boolean' }, toggle: { type: 'boolean' }, scroll: { type: 'boolean' }, menu: { type: 'boolean' }, keyboard: { type: 'boolean' }, 'keyboard-guard-path': { type: 'string' }, 'pointer-only': { type: 'boolean' } } });
if (values.toggle && !values.execute) throw new Error('--toggle requires --execute');
if (values.scroll && !values.execute) throw new Error('--scroll requires --execute');
if (values.menu && !values.execute) throw new Error('--menu requires --execute');
if (values.scroll && (values.toggle || values['pointer-only'])) throw new Error('--scroll is exclusive with --toggle and --pointer-only');
if (values.menu && (values.scroll || values.toggle || values['pointer-only'])) throw new Error('--menu is exclusive with --scroll, --toggle and --pointer-only');
if (values['pointer-only'] && !values.toggle) throw new Error('--pointer-only requires --toggle');
if (values.keyboard && (!values.execute || values.menu || values.scroll || values.toggle || values['pointer-only'])) throw new Error('--keyboard requires exclusive --execute');
if (values['keyboard-guard-path'] && !values.keyboard) throw new Error('--keyboard-guard-path requires --keyboard');
const requestedKeyboardGuard = values['keyboard-guard-path'];
if (values.keyboard && (!requestedKeyboardGuard || !isAbsolute(requestedKeyboardGuard) || !existsSync(requestedKeyboardGuard) || !statSync(requestedKeyboardGuard).isFile() || (statSync(requestedKeyboardGuard).mode & 0o111) === 0 || !basename(realpathSync(requestedKeyboardGuard)).startsWith('owned-key-'))) throw new Error('--keyboard requires an absolute existing owned-key-* guard executable');
const keyboardGuard = requestedKeyboardGuard ? realpathSync(requestedKeyboardGuard) : undefined;
if (!values.runtime || !isAbsolute(values.runtime) || !existsSync(values.runtime)) throw new Error('--runtime must be an existing Electron executable');
const output = mkdtempSync(join(tmpdir(), 'cu-operator-auth-'));
const keyboardDiagnostics = values.keyboard ? join(output, 'key-diagnostics') : undefined;
if (keyboardDiagnostics) mkdirSync(keyboardDiagnostics, { mode: 0o700 });
const record = join(root, values.keyboard ? 'artifacts/m2-native-operator-keyboard.json' : values.menu ? 'artifacts/m3-native-operator-menu.json' : values.scroll ? 'artifacts/m3-native-operator-scroll.json' : values['pointer-only'] ? 'artifacts/m3-native-operator-toggle-pointer.json' : values.toggle ? 'artifacts/m3-native-operator-toggle.json' : values.execute ? 'artifacts/m3-native-operator-execution.json' : 'artifacts/m3-native-operator-authorization.json');
mkdirSync(dirname(record), { recursive: true });
if (existsSync(record)) {
  let previousTime: string | undefined;
  try { previousTime = JSON.parse(readFileSync(record, 'utf8')).tested_at; } catch {}
  const stamp = String(previousTime ?? new Date().toISOString()).replace(/[^0-9A-Za-z-]/g, '-');
  renameSync(record, record.replace(/\.json$/, `-${stamp}-${Date.now()}.json`));
}
const fixtureName = values.keyboard ? 'KeyFixture' : values.menu ? 'MenuFixture' : values.scroll ? 'HostScrollFixture' : 'GuardFixture';
const fixtureSource = join(root, `native/device/fixtures/${fixtureName}.swift`);
const fixtureBinary = values.keyboard ? join(output, `${fixtureName}.app/Contents/MacOS/${fixtureName}`) : join(root, `target/native-macos/${fixtureName}.app/Contents/MacOS/${fixtureName}`);
let build: Record<string, unknown> = { captured_before_execution: false };
let log = '', code: number | null = null;
let result: Record<string, any> = { ok: false };
try {
  const hash = (path: string) => createHash('sha256').update(readFileSync(path)).digest('hex');
  const sourceFiles = execFileSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', 'crates', 'native', 'apps/desktop/src', 'packages', 'Cargo.lock', 'contracts/desktop-v2/schema.json', 'scripts/smoke-macos-operator-authorization.mts', 'apps/desktop/test/manual/nativeAuthorizationSmoke.cjs'], { cwd: root, encoding: 'utf8' }).split('\0').filter(Boolean).sort();
  const sourceHash = createHash('sha256');
  for (const file of sourceFiles) sourceHash.update(file + '\0').update(readFileSync(join(root, file))).update('\0');
  Object.assign(build, { head: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(), source_tree_sha256: sourceHash.digest('hex'), wrapper_source_sha256: hash(fileURLToPath(import.meta.url)),
    fixture_binary: fixtureBinary, fixture_source_sha256: hash(fixtureSource), smoke_source_sha256: hash(join(root, 'apps/desktop/test/manual/nativeAuthorizationSmoke.cjs')) });
  mkdirSync(join(root, 'target/native-macos/module-cache'), { recursive: true });
  if (values.scroll || values.menu || values.keyboard) {
    const contents = dirname(dirname(fixtureBinary));
    mkdirSync(dirname(fixtureBinary), { recursive: true });
    writeFileSync(join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.lingxi.computer-user.guard-fixture</string><key>CFBundleExecutable</key><string>${fixtureName}</string><key>CFBundleName</key><string>Computer Use ${values.keyboard ? 'Key' : values.menu ? 'Menu' : 'Host Scroll'} Fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
    execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'), fixtureSource, '-o', fixtureBinary], { stdio: 'inherit' });
  }
  execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'), join(root, 'native/device/fixtures/ConsentDialogDriver.swift'), '-o', join(root, 'target/native-macos/consent-dialog-driver')], { stdio: 'inherit' });
  execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'), join(root, 'native/device/fixtures/WindowReadProbe.swift'), '-o', join(root, 'target/native-macos/window-read-probe')], { stdio: 'inherit' });
  Object.assign(build, { main_sha256: hash(join(root, 'apps/desktop/out/main/index.js')), renderer_sha256: hash(join(root, 'apps/desktop/out/renderer/index.html')), driver_sha256: hash(join(root, 'target/native-macos/consent-dialog-driver')) });
  Object.assign(build, { preload_sha256: hash(join(root, 'apps/desktop/out/preload/index.cjs')), runtime_sha256: hash(values.runtime) });
  Object.assign(build, { fixture_binary: fixtureBinary, fixture_sha256: hash(fixtureBinary),
    fixture_source_sha256: hash(fixtureSource),
    diagnostic_probe_sha256: hash(join(root, 'target/native-macos/window-read-probe')),
    smoke_source_sha256: hash(join(root, 'apps/desktop/test/manual/nativeAuthorizationSmoke.cjs')) });
  Object.assign(build, { renderer_assets: Object.fromEntries(readdirSync(join(root, 'apps/desktop/out/renderer/assets')).sort().map(file => [file, hash(join(root, 'apps/desktop/out/renderer/assets', file))])),
    binaries: Object.fromEntries(['cu-host', 'cu-device', 'cu-source', 'cu-macos-source', 'cu-macos-device-guard'].map(file => [file, hash(join(root, 'target/debug', file))])), schema_sha256: hash(join(root, 'contracts/desktop-v2/schema.json')) });
  if (keyboardGuard) Object.assign(build, { keyboard_guard_path: keyboardGuard, keyboard_guard_sha256: hash(keyboardGuard), owned_keyboard_guard_override: true });
  build.captured_before_execution = true;
  const child = spawn(values.runtime, [join(root, 'apps/desktop/test/manual/nativeAuthorizationSmoke.cjs')], { cwd: root,
    env: { ...process.env, CU_AUTH_ROOT: root, CU_AUTH_OUTPUT: output, CU_AUTH_EXECUTE: values.execute ? '1' : '', CU_AUTH_TOGGLE: values.toggle ? '1' : '', CU_AUTH_SCROLL: values.scroll ? '1' : '', CU_AUTH_MENU: values.menu ? '1' : '', CU_AUTH_KEYBOARD: values.keyboard ? '1' : '', ...(keyboardGuard ? { CU_NATIVE_DEVICE_GUARD: keyboardGuard } : {}), ...(keyboardDiagnostics ? { CU_OWNED_KEY_DIAGNOSTICS_DIR: keyboardDiagnostics, CU_OWNED_KEY_DIAGNOSTICS_MODE: 'receipts' } : {}), CU_AUTH_FIXTURE_BINARY: fixtureBinary, CU_AUTH_POINTER_ONLY: values['pointer-only'] ? '1' : '' }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stdout.on('data', (data) => { log = (log + data).slice(-8192); }); child.stderr.on('data', (data) => { log = (log + data).slice(-8192); });
  const timeout = setTimeout(() => child.kill('SIGTERM'), 65000);
  code = await new Promise<number | null>((done, fail) => { child.once('error', fail); child.once('close', done); }).finally(() => clearTimeout(timeout));
  result = existsSync(join(output, 'result.json')) ? JSON.parse(readFileSync(join(output, 'result.json'), 'utf8')) : { ok: false, error: log };
  if (values.execute) {
    const expected = build.binaries as Record<string, string>;
    const unchanged = hash(fixtureBinary) === build.fixture_sha256 && Object.entries(expected).every(([file, before]) => hash(join(root, 'target/debug', file)) === before);
    build.binaries_unchanged_during_execution = unchanged;
    if (!unchanged) throw new Error('A production binary or owned fixture changed during the acceptance run');
    if (keyboardGuard) {
      build.keyboard_guard_unchanged_during_execution = hash(keyboardGuard) === build.keyboard_guard_sha256;
      if (!build.keyboard_guard_unchanged_during_execution) throw new Error('The owned keyboard guard changed during the acceptance run');
    }
  }
} catch (error) { result = { ...result, ok: false, error: error instanceof Error ? error.stack : String(error) }; }
if (keyboardDiagnostics) {
  result.owned_key_diagnostics = {
    path: keyboardDiagnostics,
    records: Object.fromEntries(readdirSync(keyboardDiagnostics).filter(name => name.endsWith('.json')).map(name => [name, JSON.parse(readFileSync(join(keyboardDiagnostics, name), 'utf8'))]))
  };
}
writeFileSync(record, JSON.stringify({ ...result, build, output, runtime_exit_code: code, tested_at: new Date().toISOString() }, null, 2) + '\n');
console.log(JSON.stringify({ ok: result.ok, code, record, output, error: result.error }));
writeFileSync(join(output, 'runtime.log'), log);
if (!result.ok || code !== 0) throw new Error('Owned Operator authorization smoke failed: ' + (result.error ?? log));
