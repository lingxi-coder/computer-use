import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import {
  constants, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

type Counts = {
  schema_available: boolean;
  root_tasks: number;
  attempts: number;
  s1_decision_model_calls: number;
  s2_model_calls: number;
  budget_attempts_used: number;
};
type PrivateRunInfo = {
  owned_directories: string[];
  removed_owned_directories: string[];
  host_directory_count: number;
  ledger_rows: Counts[];
};
type ManualProof = {
  ok: boolean;
  checks: Record<string, boolean>;
  dialogs: { kind: string; action: string; response: number; real_button_pressed: boolean }[];
  renderer_errors: number;
  failure_code: string | null;
  screenshot_sha256: string | null;
};

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({ options: { runtime: { type: 'string' } } });
if (!values.runtime || !isAbsolute(values.runtime) || !existsSync(values.runtime)) {
  throw new Error('--runtime must be an existing absolute Electron executable.');
}

const runtime = resolve(values.runtime);
const output = mkdtempSync(join(tmpdir(), 'cu-owned-document-auth-'));
const runId = randomUUID();
const driver = join(output, 'consent-dialog-driver');
const manual = join(root, 'apps/desktop/test/manual/ownedDocumentAuthorizationSmoke.cjs');
const driverSource = join(root, 'native/device/fixtures/ConsentDialogDriver.swift');
const outputArtifact = join(root, 'artifacts', `owned-document-authorization-${runId}.json`);
const screenshotArtifact = join(root, 'artifacts', `owned-document-authorization-${runId}.png`);
const phases: { ok: boolean; runtime_completed_normally: boolean; check_count: number; dialog_count: number; failure_step: string | null; renderer_errors: number | null }[] = [];
const allChecks: Record<string, boolean> = {};
const allDialogs: ManualProof['dialogs'] = [];
const ledgerRows: Counts[] = [];
let ownedDirectories: string[] = [];
let removedOwnedDirectories: string[] = [];
let hostDirectoryCount = 0;
let screenshotSha256: string | null = null;
let runtimeSha256: string | null = null;
let driverSha256: string | null = null;
let productHashes: Record<string, string | string[] | null> = {};
let wrapperFailure = false;
let screenshotCopied = false;
let outputCleanupVerified = false;
let runtimeFailureCode: 'setup' | 'create' | 'reopen' | 'cleanup' | null = null;

function hashFile(file: string): string {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

function maybeHash(file: string): string | null {
  return existsSync(file) ? hashFile(file) : null;
}

function captureProductHashes(): Record<string, string | string[] | null> {
  const assetDirectory = join(root, 'apps/desktop/out/renderer/assets');
  const assets = existsSync(assetDirectory)
    ? readdirSync(assetDirectory).filter(file => !file.startsWith('.')).sort().map(file => hashFile(join(assetDirectory, file)))
    : [];
  return {
    main: maybeHash(join(root, 'apps/desktop/out/main/index.js')),
    preload: maybeHash(join(root, 'apps/desktop/out/preload/index.cjs')),
    renderer_html: maybeHash(join(root, 'apps/desktop/out/renderer/index.html')),
    renderer_assets: assets,
    host: maybeHash(join(root, 'target/debug/cu-host')),
    device: maybeHash(join(root, 'target/debug/cu-device')),
    source: maybeHash(join(root, 'target/debug/cu-source')),
    native_source: maybeHash(join(root, 'target/debug/cu-macos-source')),
    native_device_guard: maybeHash(join(root, 'target/debug/cu-macos-device-guard')),
  };
}

function productHashesPresent(hashes: Record<string, string | string[] | null>): boolean {
  return ['main', 'preload', 'renderer_html', 'host', 'device', 'source', 'native_source', 'native_device_guard']
    .every(name => typeof hashes[name] === 'string')
    && Array.isArray(hashes['renderer_assets']) && hashes['renderer_assets'].length > 0;
}

function readManualProof(phase: 'create' | 'reopen'): ManualProof | null {
  const file = join(output, `${phase}-result.json`);
  if (!existsSync(file)) return null;
  try { return JSON.parse(readFileSync(file, 'utf8')) as ManualProof; }
  catch { return null; }
}

function readPrivateRunInfo(phase: 'create' | 'reopen'): PrivateRunInfo | null {
  const file = join(output, `${phase}-internal.json`);
  if (!existsSync(file)) return null;
  try { return JSON.parse(readFileSync(file, 'utf8')) as PrivateRunInfo; }
  catch { return null; }
}

function checkPrivateDirectory(directory: string): boolean {
  if (!isAbsolute(directory) || !basename(directory).startsWith('cu-owned-document-')) return false;
  try {
    const canonicalParent = realpathSync(dirname(directory));
    const canonicalTemp = realpathSync(tmpdir());
    return canonicalParent === canonicalTemp && !existsSync(directory);
  } catch { return false; }
}

async function runRuntime(phase: 'create' | 'reopen', timeoutMs: number): Promise<number | null> {
  const childEnv: NodeJS.ProcessEnv = {
    ...process.env,
    CU_OWNED_DOCUMENT_ROOT: root,
    CU_OWNED_DOCUMENT_OUTPUT: output,
    CU_OWNED_DOCUMENT_PHASE: phase,
    CU_OWNED_DOCUMENT_DRIVER: driver,
  };
  delete childEnv.CU_TARGET_PID;
  delete childEnv.CU_TARGET_BUNDLE_ID;
  delete childEnv.CU_SOURCE_BACKEND;
  delete childEnv.CU_NATIVE_REFERENCE_PLANNER;
  delete childEnv.ELECTRON_RENDERER_URL;
  for (const name of Object.keys(childEnv)) {
    if (/(?:API.?KEY|ACCESS.?TOKEN|(?:OPENAI|ANTHROPIC)_|MODEL.*(?:KEY|TOKEN))/i.test(name)) delete childEnv[name];
  }

  const child = spawn(runtime, [manual], { cwd: root, env: childEnv, stdio: ['ignore', 'ignore', 'ignore'] });
  let timedOut = false;
  let forceTimer: ReturnType<typeof setTimeout> | undefined;
  const timeout = setTimeout(() => {
    timedOut = true;
    child.kill('SIGTERM');
    forceTimer = setTimeout(() => child.kill('SIGKILL'), 5000);
    forceTimer.unref();
  }, timeoutMs);
  timeout.unref();
  try {
    const code = await new Promise<number | null>((resolveCode, reject) => {
      child.once('error', reject);
      child.once('close', resolveCode);
    });
    return timedOut ? null : code;
  } finally {
    clearTimeout(timeout);
    if (forceTimer) clearTimeout(forceTimer);
  }
}

function privateCountsAreZero(rows: Counts[]): boolean {
  return rows.length > 0 && rows.every(row => row.schema_available
    && row.root_tasks === 0 && row.attempts === 0
    && row.s1_decision_model_calls === 0 && row.s2_model_calls === 0 && row.budget_attempts_used === 0);
}

function consentFlags(dialogs: ManualProof['dialogs']): Record<string, boolean> {
  const response = (kind: string, action: string, value: number) => dialogs.some(row => row.kind === kind
    && row.action === action && row.response === value && row.real_button_pressed);
  return {
    pending_edit_stop_cancelled: dialogs.some(row => row.kind === 'owned-edit' && row.action === 'stop' && row.response === 0),
    edit_cancel_pressed: response('owned-edit', 'cancel', 0),
    edit_allow_pressed: response('owned-edit', 'allow', 1),
    save_cancel_pressed: response('owned-save', 'cancel', 0),
    save_allow_pressed: response('owned-save', 'allow', 1),
    s2_model_allow_pressed: response('model', 'allow', 1),
  };
}

async function main(): Promise<void> {
  try {
    mkdirSync(join(root, 'artifacts'), { recursive: true });
    runtimeSha256 = hashFile(runtime);
    productHashes = captureProductHashes();
    if (!productHashesPresent(productHashes)) throw new Error('required prebuilt product artifact is unavailable');
    mkdirSync(join(output, 'swift-module-cache'), { recursive: true });
    execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors',
      '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(output, 'swift-module-cache'),
      driverSource, '-o', driver], { stdio: 'inherit', timeout: 30000 });
    driverSha256 = hashFile(driver);
    const deadline = Date.now() + 86000;

    for (const phase of ['create', 'reopen'] as const) {
      const remaining = deadline - Date.now();
      if (remaining < 1000) throw new Error('runtime budget exhausted');
      const code = await runRuntime(phase, Math.min(80000, remaining - 500));
      const proof = readManualProof(phase);
      const info = readPrivateRunInfo(phase);
      const checks = proof?.checks ?? {};
      for (const [name, value] of Object.entries(checks)) allChecks[`${phase}_${name}`] = value;
      if (proof) {
        allDialogs.push(...proof.dialogs);
        screenshotSha256 ??= proof.screenshot_sha256;
      }
      if (info) {
        ownedDirectories.push(...info.owned_directories);
        removedOwnedDirectories.push(...info.removed_owned_directories);
        hostDirectoryCount += info.host_directory_count;
        ledgerRows.push(...info.ledger_rows);
      }
      const phaseOk = proof?.ok === true && code === 0 && proof.renderer_errors === 0
        && Object.keys(checks).length > 0 && Object.values(checks).every(Boolean) && info !== null;
      const safeFailureStep = proof?.failure_code && /^[a-z_]{1,80}$/.test(proof.failure_code)
        ? proof.failure_code : null;
      phases.push({ ok: phaseOk, runtime_completed_normally: code === 0, check_count: Object.keys(checks).length,
        dialog_count: proof?.dialogs.length ?? 0, failure_step: safeFailureStep,
        renderer_errors: proof?.renderer_errors ?? null });
      if (!phaseOk) {
        runtimeFailureCode = phase;
        wrapperFailure = true;
        break;
      }
    }

    const consent = consentFlags(allDialogs);
    const cleanupFlags = {
      one_owned_document_directory_created: ownedDirectories.length === 1,
      exact_owned_document_directory_removed: ownedDirectories.length === 1 && removedOwnedDirectories.includes(ownedDirectories[0]!),
      owned_document_directory_absent: ownedDirectories.length === 1 && ownedDirectories.every(checkPrivateDirectory),
      every_host_session_directory_had_a_ledger: hostDirectoryCount > 0 && ledgerRows.length === hostDirectoryCount,
      all_root_attempt_and_model_ledgers_zero: privateCountsAreZero(ledgerRows),
    };
    const rendererFlags = {
      ...consent,
      initial_create_observation_and_no_input: allChecks['create_created_document_no_root'] === true
        && allChecks['create_created_document_observe_mode'] === true && allChecks['create_created_document_no_input'] === true,
      new_app_restores_no_document_permission: allChecks['reopen_new_app_restores_no_document_permission'] === true,
      new_app_restores_no_model_consent: allChecks['reopen_new_app_restores_no_model_consent'] === true,
      desktop_bridge_has_no_private_document_fields: allChecks['create_desktop_api_has_no_private_document_fields'] === true
        && allChecks['reopen_desktop_api_has_no_private_document_fields'] === true,
      reopen_disabled_without_saved_version: allChecks['create_reopen_disabled_without_saved_version'] === true,
      revoke_edit_clears_save: allChecks['create_revoke_edit_clears_save'] === true,
      revoke_model_clears_edit_and_save: allChecks['create_revoke_model_clears_edit_and_save'] === true,
      model_revoke_returns_to_observe_without_input: allChecks['create_after_model_revoke_no_root'] === true
        && allChecks['create_after_model_revoke_observe_mode'] === true
        && allChecks['create_after_model_revoke_no_input'] === true,
      model_key_cleared_and_not_rendered: allChecks['create_model_key_cleared'] === true
        && allChecks['create_model_key_not_rendered'] === true,
      s2_only_and_no_task: allChecks['create_s2_only_authorized'] === true
        && allChecks['create_after_s2_consent_no_root'] === true
        && allChecks['create_after_s2_consent_observe_mode'] === true,
    };
    const allLedgerCountsZero = cleanupFlags['all_root_attempt_and_model_ledgers_zero'];
    const allConsentPassed = Object.values(consent).every(Boolean);
    const allRendererChecksPassed = Object.values(rendererFlags).every(Boolean)
      && Object.values(allChecks).every(Boolean) && Object.keys(allChecks).length > 0;

    let screenshotHashMatches = false;
    if (!wrapperFailure && screenshotSha256 && existsSync(join(output, 'create-window.png'))
      && allChecks['create_secret_absent_before_capture'] === true) {
      const screenshotSource = join(output, 'create-window.png');
      screenshotHashMatches = hashFile(screenshotSource) === screenshotSha256;
      if (screenshotHashMatches) {
        copyFileSync(screenshotSource, screenshotArtifact, constants.COPYFILE_EXCL);
        screenshotCopied = true;
      }
    }

    const okBeforeOutputCleanup = !wrapperFailure && phases.length === 2 && phases.every(row => row.ok)
      && allConsentPassed && allRendererChecksPassed && allLedgerCountsZero
      && Object.values(cleanupFlags).every(Boolean) && screenshotCopied && screenshotHashMatches;
    try {
      rmSync(output, { recursive: true, force: true });
      outputCleanupVerified = !existsSync(output);
    } catch { outputCleanupVerified = false; }
    const ok = okBeforeOutputCleanup && outputCleanupVerified;
    if (!ok && runtimeFailureCode === null) runtimeFailureCode = 'cleanup';

    const record = {
      format_version: 1,
      ok,
      checks: {
        app_create_phase: phases[0]?.ok === true,
        new_app_reopen_phase: phases[1]?.ok === true,
        renderer_checks_passed: allRendererChecksPassed,
        real_native_consent_buttons_driven: allConsentPassed,
        session_shutdown_cleanup_passed: Object.values(cleanupFlags).every(Boolean),
        isolated_run_output_removed: outputCleanupVerified,
        fake_provider_calls: 0,
        live_provider_verified: false,
      },
      counts: {
        root_tasks: ledgerRows.reduce((sum, row) => sum + row.root_tasks, 0),
        attempts: ledgerRows.reduce((sum, row) => sum + row.attempts, 0),
        s1_decision_model_calls: ledgerRows.reduce((sum, row) => sum + row.s1_decision_model_calls, 0),
        s2_model_calls: ledgerRows.reduce((sum, row) => sum + row.s2_model_calls, 0),
        budget_attempts_used: ledgerRows.reduce((sum, row) => sum + row.budget_attempts_used, 0),
        host_session_ledgers: ledgerRows.length,
        owned_document_directories_created: ownedDirectories.length,
        owned_document_directories_removed: removedOwnedDirectories.length,
        dialogs: allDialogs.length,
        real_native_button_presses: allDialogs.filter(row => row.real_button_pressed).length,
      },
      consent,
      renderer_checks: allChecks,
      cleanup: cleanupFlags,
      phases,
      hashes: {
        runtime: runtimeSha256,
        driver: driverSha256,
        manual_smoke: hashFile(manual),
        wrapper: hashFile(fileURLToPath(import.meta.url)),
        product: productHashes,
        screenshot: screenshotCopied ? screenshotSha256 : null,
      },
    };
    writeFileSync(outputArtifact, JSON.stringify(record, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
    console.log(JSON.stringify({ ok, artifact: basename(outputArtifact), screenshot: screenshotCopied ? basename(screenshotArtifact) : null }));
    if (!ok) throw new Error(`Owned-document authorization smoke failed (${runtimeFailureCode ?? 'checks'}).`);
  } catch {
    wrapperFailure = true;
    if (runtimeFailureCode === null) runtimeFailureCode = 'setup';
    // Never print a runtime log or a path, route, registration, document body or credential.
    console.error(`Owned-document authorization smoke failed (${runtimeFailureCode}).`);
    if (existsSync(output)) {
      try { rmSync(output, { recursive: true, force: true }); } catch { /* Exact isolated output cleanup is retried by the finally path. */ }
    }
    if (!existsSync(outputArtifact)) {
      const safeFailure = {
        format_version: 1,
        ok: false,
        checks: { setup_or_runtime_failed: true, isolated_run_output_removed: !existsSync(output), fake_provider_calls: 0, live_provider_verified: false },
        counts: {
          root_tasks: ledgerRows.reduce((sum, row) => sum + row.root_tasks, 0),
          attempts: ledgerRows.reduce((sum, row) => sum + row.attempts, 0),
          s1_decision_model_calls: ledgerRows.reduce((sum, row) => sum + row.s1_decision_model_calls, 0),
          s2_model_calls: ledgerRows.reduce((sum, row) => sum + row.s2_model_calls, 0),
          budget_attempts_used: ledgerRows.reduce((sum, row) => sum + row.budget_attempts_used, 0),
        },
        phases,
        hashes: { runtime: runtimeSha256, driver: driverSha256, manual_smoke: maybeHash(manual), wrapper: maybeHash(fileURLToPath(import.meta.url)), product: productHashes },
      };
      try { writeFileSync(outputArtifact, JSON.stringify(safeFailure, null, 2) + '\n', { flag: 'wx', mode: 0o600 }); } catch { /* Keep historical records untouched. */ }
    }
    throw new Error('Owned-document authorization smoke failed.');
  }
}

await main();
