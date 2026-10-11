// Real fixed-profile checkbox task through normal Root/Attempt and private Device fd3.
// Run only after the root-built bins exist: node --import tsx scripts/smoke-headless-root.mts --chrome <absolute Chrome> --output <new absolute directory>
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  accessSync,
  constants,
  createReadStream,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  realpathSync,
  writeFileSync,
} from 'node:fs';
import { dirname, isAbsolute, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs, promisify } from 'node:util';

import type { BrowserPageSnapshot, BrowserControlSubmit, BrowserControlReply, StopAck, UiProjection } from '@computer-use/desktop-api';
import { Supervisor } from '../apps/desktop/src/main/supervisor.js';
import { HeadlessBrowserError } from '../apps/desktop/src/main/headless/cdpPipe.js';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({
  options: {
    chrome: { type: 'string' },
    output: { type: 'string' },
  },
});

if (!isAbsolute(values.chrome ?? '') || !isAbsolute(values.output ?? '')) {
  throw new Error('Supply --chrome <absolute Chrome executable> and --output <new absolute directory>.');
}

const chromeExecutable = resolve(values.chrome!);
const output = resolve(values.output!);
if (existsSync(output)) throw new Error('Output must be a new absolute directory.');
mkdirSync(dirname(output), { recursive: true, mode: 0o700 });
if (realpathSync(dirname(output)) !== dirname(output)) throw new Error('Output parent must be a canonical directory.');
mkdirSync(output, { mode: 0o700 });
if (realpathSync(output) !== output) throw new Error('Output directory must not resolve through a symlink.');

const fixturePath = join(root, 'fixtures/headless/readonly-page.html');
const binaries = {
  host: join(root, 'target/debug/cu-host'),
  device: join(root, 'target/debug/cu-device'),
  source: join(root, 'target/debug/cu-source'),
};
const displayProbe = join(root, 'target/native-macos/headless-display-probe');
const sourcePaths = [
  join(root, 'scripts/smoke-headless-root.mts'),
  join(root, 'apps/desktop/src/main/supervisor.ts'),
  join(root, 'apps/desktop/src/main/headless/browserSource.ts'),
  join(root, 'apps/desktop/src/main/headless/ownedBrowser.ts'),
  join(root, 'apps/desktop/src/main/headless/cdpPipe.ts'),
  join(root, 'apps/desktop/src/main/headless/pageSource.ts'),
  join(root, 'packages/desktop-api/src/browserValidate.ts'),
  join(root, 'packages/desktop-api/src/generated/browser.ts'),
  join(root, 'packages/desktop-api/src/generated/browser-schema.ts'),
  join(root, 'packages/desktop-api/src/generated/envelope.ts'),
  join(root, 'contracts/browser-v1/schema.json'),
  join(root, 'crates/interaction-contracts/src/desktop/browser.rs'),
  join(root, 'crates/interaction-contracts/src/desktop/operator.rs'),
  join(root, 'crates/interaction-contracts/src/desktop/message.rs'),
  join(root, 'crates/interaction-engine/src/engine/browser.rs'),
  join(root, 'crates/interaction-engine/src/engine/mod.rs'),
  join(root, 'crates/interaction-engine/src/environment/browser.rs'),
  join(root, 'crates/device-macos/src/config.rs'),
  join(root, 'crates/source-macos/src/config.rs'),
  join(root, 'crates/desktop-host/src/shell.rs'),
  join(root, 'crates/desktop-host/src/bridge.rs'),
  join(root, 'native/headless/fixtures/DisplayProbe.swift'),
  join(root, 'apps/desktop/src/main/headless/deviceBrowserPort.ts'),
  join(root, 'apps/desktop/src/main/headless/browserControlWire.ts'),
  join(root, 'packages/desktop-api/src/browserControlValidate.ts'),
  join(root, 'crates/device-macos/src/backend/browser_control.rs'),
  join(root, 'crates/desktop-ipc/src/browser_control.rs'),
  join(root, 'crates/interaction-engine/src/engine/attempt.rs'),
  join(root, 'crates/interaction-engine/src/verification/mod.rs'),
  join(root, 'crates/interaction-engine/src/verification/browser_current_ui_verification.rs'),
  fixturePath,
];
const executablePaths = [chromeExecutable, ...Object.values(binaries), displayProbe];
const run = promisify(execFile);
const pause = (milliseconds: number) => new Promise<void>((done) => setTimeout(done, milliseconds));
const callbackErrors: string[] = [];
const snapshotAcks: BrowserPageSnapshot[] = [];
const projectionBySnapshotId = new Map<string, NonNullable<UiProjection['browser_observation']>>();
const checks: Record<string, boolean> = {};
const cleanupErrors: string[] = [];
let phase = 'preflight';
let failure: string | null = null;
let supervisor: Supervisor | undefined;
let supervisorShutdown = false;
let supervisorDir: string | undefined;
let profileParent: string | undefined;
let ownedProfile: string | undefined;
let browserPid: number | undefined;
let pidsAtStart: Record<string, number | undefined> | null = null;
let stopAck: StopAck | null = null;
let beforeDisplay: DisplayEvidence | undefined;
let duringDisplay: DisplayEvidence | undefined;
let afterDisplay: DisplayEvidence | undefined;
let pinsBefore: PinSet | undefined;
let pinsAfter: PinSet | undefined;
let worldLogBytesAtEnd: number | null = null;
let ledgerAtEnd: any = null;
const executions: { submission: BrowserControlSubmit; reply: BrowserControlReply }[] = [];
const projections: UiProjection[] = [];


type DisplayEvidence = {
  foreground_pid: number;
  foreground_visible_window_ids: number[];
  foreground_visible_window_state: 'none' | 'present';
  owned_browser_onscreen_window_count: number | null;
  focused_window_evidence: string;
};

type PinSet = {
  captured_at: string;
  executables: Record<string, { path: string; sha256: string | null }>;
  sources: Record<string, { path: string; sha256: string | null }>;
};

function save(name: string, value: unknown): void {
  writeFileSync(join(output, name), `${JSON.stringify(value, null, 2)}\n`, { flag: 'w', mode: 0o600 });
}

async function sha256(path: string): Promise<string | null> {
  try {
    const hash = createHash('sha256');
    for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer);
    return hash.digest('hex');
  } catch {
    return null;
  }
}

async function capturePins(): Promise<PinSet> {
  const capture = async (paths: readonly string[]): Promise<Record<string, { path: string; sha256: string | null }>> => {
    const entries = await Promise.all(paths.map(async path => [path, { path, sha256: await sha256(path) }] as const));
    return Object.fromEntries(entries);
  };
  return {
    captured_at: new Date().toISOString(),
    executables: await capture(executablePaths),
    sources: await capture(sourcePaths),
  };
}

function pinsMatch(before: PinSet, after: PinSet): boolean {
  const matches = (left: PinSet['executables'], right: PinSet['executables']): boolean =>
    Object.keys(left).length === Object.keys(right).length
    && Object.entries(left).every(([key, value]) => value.sha256 !== null && value.sha256 === right[key]?.sha256);
  return matches(before.executables, after.executables) && matches(before.sources, after.sources);
}

function assertExecutable(path: string): void {
  const stat = lstatSync(path);
  assert.equal(stat.isFile(), true, `expected a regular executable: ${path}`);
  assert.equal(stat.isSymbolicLink(), false, `executable must not be a symlink: ${path}`);
  assert.equal(realpathSync(path), path, `executable path must be canonical: ${path}`);
  accessSync(path, constants.X_OK);
}

function assertFixture(path: string): void {
  const suffix = [ 'fixtures', 'headless', 'readonly-page.html' ].join(sep);
  const stat = lstatSync(path);
  assert(path.endsWith(`${sep}${suffix}`), 'fixture path must be the fixed readonly page');
  assert.equal(stat.isFile(), true);
  assert.equal(stat.isSymbolicLink(), false);
  assert.equal(realpathSync(path), path);
  assert(stat.size > 0 && stat.size <= 1_048_576);
}

function assertAllPinsPresent(pins: PinSet): void {
  for (const collection of [pins.executables, pins.sources]) {
    for (const value of Object.values(collection)) assert(value.sha256, `missing pre-run pin: ${value.path}`);
  }
}

async function display(pid?: number): Promise<DisplayEvidence> {
  const args = pid === undefined ? [] : [String(pid)];
  const { stdout } = await run(displayProbe, args, { encoding: 'utf8', timeout: 4_000, maxBuffer: 16_384 });
  const evidence = JSON.parse(stdout) as DisplayEvidence;
  assert(Number.isSafeInteger(evidence.foreground_pid) && evidence.foreground_pid > 0);
  assert(Array.isArray(evidence.foreground_visible_window_ids));
  assert(evidence.foreground_visible_window_ids.length <= 512);
  assert(evidence.foreground_visible_window_ids.every(id => Number.isSafeInteger(id) && id > 0));
  assert.equal(new Set(evidence.foreground_visible_window_ids).size, evidence.foreground_visible_window_ids.length);
  assert.equal(evidence.foreground_visible_window_state, evidence.foreground_visible_window_ids.length ? 'present' : 'none');
  return evidence;
}

function outputProfile(profileDirectory: string): boolean {
  try {
    const stat = lstatSync(profileDirectory);
    return stat.isDirectory() && !stat.isSymbolicLink() && realpathSync(profileDirectory) === profileDirectory;
  } catch {
    return false;
  }
}

function findProfile(supervisor: Supervisor): { parent: string; profile: string } {
  const parent = join(realpathSync(supervisor.dir), 'headless-profiles');
  const entries = readdirSync(parent).filter(name => name.startsWith('cu-headless-owned-'));
  assert.equal(entries.length, 1, 'Supervisor must own exactly one Chrome profile');
  const profile = join(parent, entries[0]!);
  assert(outputProfile(profile));
  return { parent, profile };
}

async function chromePidsForProfile(profileDirectory: string, topLevelOnly: boolean): Promise<number[]> {
  // `ps` output is held only in memory and discarded after exact executable/profile filtering.
  const { stdout } = await run('ps', ['-axo', 'pid=,args='], { encoding: 'utf8', timeout: 4_000, maxBuffer: 8 * 1024 * 1024 });
  const executablePrefixes = [chromeExecutable, `"${chromeExecutable}"`, `'${chromeExecutable}'`];
  const profileArgument = `--user-data-dir=${profileDirectory}`;
  return stdout.split('\n').flatMap(line => {
    const match = /^\s*(\d+)\s+(.*)$/.exec(line);
    if (!match) return [];
    const command = match[2]!;
    const exactExecutable = executablePrefixes.some(prefix => command === prefix || command.startsWith(`${prefix} `));
    if (!exactExecutable || !command.includes(profileArgument)
      || (topLevelOnly && /(?:^|\s)--type=/.test(command))) return [];
    return [Number(match[1])];
  });
}

function processExists(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code !== 'ESRCH';
  }
}

async function until<T>(read: () => T | undefined | null | false, reason: string, timeoutMs = 10_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = read();
    if (value) return value;
    await pause(20);
  }
  throw new Error(`timeout waiting for ${reason}`);
}

async function untilAsync<T>(read: () => Promise<T | undefined | null | false>, reason: string, timeoutMs = 10_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = await read();
    if (value) return value;
    await pause(100);
  }
  throw new Error(`timeout waiting for ${reason}`);
}

async function ledger(path: string): Promise<any> {
  const script = [
    'import json, pathlib, sqlite3, sys',
    'db = sqlite3.connect(pathlib.Path(sys.argv[1]).as_uri() + "?mode=ro", uri=True)',
    'db.row_factory = sqlite3.Row',
    'tables = ("root_tasks", "attempts", "decisions", "attempt_map", "admission_outbox", "effect_results", "event_log", "model_calls", "decision_model_calls")',
    'data = {name: [dict(row) for row in db.execute("SELECT * FROM " + name)] for name in tables}',
    'for row in data["effect_results"]: row["result"] = json.loads(row.pop("json"))',
    'print(json.dumps(data))',
  ].join('\n');
  const { stdout } = await run('python3', ['-c', script, path], { encoding: 'utf8', timeout: 4_000, maxBuffer: 4 * 1024 * 1024 });
  return JSON.parse(stdout);
}

function onProjection(value: Readonly<UiProjection>): void {
  try {
    const copy = structuredClone(value) as UiProjection;
    projections.push(copy);
    if (copy.browser_observation) projectionBySnapshotId.set(copy.browser_observation.snapshot_id, copy.browser_observation);
  } catch { callbackErrors.push('projection copy failed'); }
}

function onSnapshot(value: Readonly<BrowserPageSnapshot>): void {
  try {
    const copy = structuredClone(value) as BrowserPageSnapshot;
    snapshotAcks.push(copy);
    save(`snapshot-ack-${String(snapshotAcks.length).padStart(2, '0')}.json`, copy);
  } catch { callbackErrors.push('snapshot copy failed'); }
}

function waitForStop(target: Supervisor): Promise<StopAck> {
  return new Promise((resolveAck, reject) => {
    const timer = setTimeout(() => { target.off('stopAck', receive); reject(new Error('StopAtDevice timed out')); }, 5_000);
    const receive = (ack: StopAck): void => { clearTimeout(timer); resolveAck(ack); };
    target.once('stopAck', receive);
  });
}

async function task(text: string, previousRoot?: string): Promise<UiProjection> {
  const current = supervisor!;
  current.operator({ command: 'create_task', text });
  const confirmation = await until(() => current.projection?.pending_confirmation, 'ordinary goal confirmation');
  assert.equal(confirmation.kind.kind, 'goal');
  save(`confirmation-${previousRoot ? '02' : '01'}.json`, confirmation);
  current.operator({ command: 'confirm_goal', confirmation_id: confirmation.confirmation_id, approved: true });
  current.operator({ command: 'set_mode', mode: 'execute' });
  const completed = await until(() => {
    const projection = current.projection;
    if (!projection?.root || projection.root.root_task_id === previousRoot) return undefined;
    if (['failed', 'suspended', 'cancelled'].includes(projection.root.state)) {
      save('failed-root-projection.json', projection);
      throw new Error(`Root ${projection.root.state}: ${projection.root.headline}`);
    }
    return projection.root.state === 'completed' ? projection : undefined;
  }, 'normal Root completion', 35_000);
  return completed;
}

try {
  phase = 'pin-before';
  pinsBefore = await capturePins();
  save('before.json', { created_at: new Date().toISOString(), root, binaries, fixturePath, chromeExecutable,
    pins: pinsBefore, normal_root: true, fixed_profile: 'fixture_background_option', provider_calls: 0,
    ancillary_source: 'fixture only; its native observation is not browser evidence', mvp_complete: false });
  assertAllPinsPresent(pinsBefore);
  executablePaths.forEach(assertExecutable);
  assertFixture(fixturePath);
  checks.preflight_pins = true;
  phase = 'display-before';
  beforeDisplay = await display(); save('display-before.json', beforeDisplay);
  phase = 'supervisor-start';
  supervisor = await Supervisor.start({ binaries, mode: 'observe',
    browserSource: { chromeExecutable, fixturePath }, browserPageProfile: { kind: 'fixture_background_option' },
    observeBrowserSourceOwner: owner => {
      supervisorDir = owner.supervisorDir; profileParent = owner.profileParent; pidsAtStart = { ...owner.pids };
      save('startup-owner.json', owner);
    }, observeBrowserSnapshot: onSnapshot, observeProjection: onProjection,
    observeBrowserExecution: value => {
      try { executions.push(structuredClone(value)); save('private-executions.json', executions); }
      catch { callbackErrors.push('execution copy failed'); }
    },
  });
  supervisorDir = supervisor.dir;
  const profile = findProfile(supervisor); profileParent = profile.parent; ownedProfile = profile.profile;
  pidsAtStart = supervisor.pids;
  const chromePids = await chromePidsForProfile(ownedProfile, true);
  assert.equal(chromePids.length, 1); browserPid = chromePids[0]!;
  save('supervisor-owner.json', { supervisor_dir: supervisorDir, profile_parent: profileParent,
    owned_profile: ownedProfile, browser_pid: browserPid, pids: pidsAtStart });
  const manifest = await until(() => supervisor!.projection?.capabilities, 'Device capability manifest');
  assert.equal(manifest.backend, 'headless_browser');
  const domain = manifest.domains.find(item => item.domain === 'headless_browser');
  assert.equal(domain?.state, 'available'); assert.deepEqual(domain?.actions, ['headless_browser_action']);
  save('capability-manifest.json', manifest);
  await until(() => supervisor!.projection?.browser_observation?.readonly_checked === false ? true : undefined, 'real unchecked state');
  assert.equal(supervisor.projection?.root ?? null, null);
  duringDisplay = await display(browserPid); save('display-before-input.json', duringDisplay);
  assert.equal(duringDisplay.owned_browser_onscreen_window_count, 0);
  assert.equal(duringDisplay.foreground_pid, beforeDisplay.foreground_pid);
  assert.deepEqual(duringDisplay.foreground_visible_window_ids, beforeDisplay.foreground_visible_window_ids);
  checks.headless_and_foreground_before_input = true;

  phase = 'normal-root-input';
  const first = await task('Enable browser fixture checkbox');
  save('completed-root-01.json', first);
  assert.equal(executions.length, 1, 'one actual Device-authorized page gesture');
  const actual = executions[0]!;
  assert.equal(actual.submission.intent.action.type, 'set_checkbox');
  assert.equal(actual.submission.intent.action.checked, true);
  assert.equal(actual.reply.outcome, 'completed');
  assert(Number.isSafeInteger(actual.reply.submitted_at_ms)); assert(Number.isSafeInteger(actual.reply.input_end_ms));
  assert(actual.reply.input_end_ms! >= actual.reply.submitted_at_ms!);
  assert(actual.reply.input_end_ms! <= actual.submission.authority.command_end_ms);
  ledgerAtEnd = await ledger(join(supervisorDir, 'host.db')); save('ledger-after-input.json', ledgerAtEnd);
  const effects = ledgerAtEnd.effect_results.map((item: any) => item.result);
  const effect = effects.find((item: any) => item.root_task_id === first.root!.root_task_id
    && item.predicate === 'browser.checkbox_state@1' && item.status === 'satisfied');
  assert(effect); assert.equal(effect.confirmation_scope, 'current_ui'); assert.equal(effect.causality, 'unknown');
  assert.equal(effect.intent_id, actual.submission.intent.intent_id);
  assert.equal(effect.attempt_id, actual.submission.intent.attempt_id);
  assert.deepEqual(effect.input_span, { from_ms: actual.reply.submitted_at_ms, to_ms: actual.reply.input_end_ms });
  const witness = snapshotAcks.find(snapshot => effect.evidence_group.includes(snapshot.snapshot_id));
  assert(witness, 'effect uses a real Host-admitted source snapshot');
  assert(witness.observed_ms > actual.reply.input_end_ms!);
  assert(witness.source_sequence > actual.submission.intent.evidence.binding.source_sequence);
  assert.deepEqual(witness.context, actual.submission.intent.evidence.scope.context);
  assert.equal(witness.nodes.find(node => node.name === 'Background option' && node.role === 'checkbox')?.checked, 'true');
  save('input-and-independent-witness.json', { execution: actual, effect, witness });
  checks.actual_input_and_independent_current_ui = true;
  duringDisplay = await display(browserPid); save('display-during.json', duringDisplay);
  assert.equal(duringDisplay.owned_browser_onscreen_window_count, 0);
  assert.equal(duringDisplay.foreground_pid, beforeDisplay.foreground_pid);
  assert.deepEqual(duringDisplay.foreground_visible_window_ids, beforeDisplay.foreground_visible_window_ids);
  checks.foreground_unchanged_after_input = true;

  phase = 'already-satisfied-root';
  const second = await task('Enable browser fixture checkbox', first.root!.root_task_id);
  save('completed-root-02.json', second);
  assert.equal(executions.length, 1, 'already satisfied task must not submit another gesture');
  ledgerAtEnd = await ledger(join(supervisorDir, 'host.db')); save('durable-ledger.json', ledgerAtEnd);
  const zeroEffect = ledgerAtEnd.effect_results.map((item: any) => item.result)
    .find((item: any) => item.root_task_id === second.root!.root_task_id && item.status === 'satisfied');
  assert(zeroEffect); assert.equal(zeroEffect.predicate, 'browser.checkbox_state@1');
  assert.equal(zeroEffect.intent_id, null); assert.equal(zeroEffect.input_span, null);
  assert.equal(ledgerAtEnd.root_tasks.length, 2);
  assert(ledgerAtEnd.root_tasks.every((item: any) => item.state === 'completed'));
  assert(ledgerAtEnd.attempts.length >= 2);
  assert.equal(ledgerAtEnd.attempt_map.length, ledgerAtEnd.attempts.length);
  assert.equal(ledgerAtEnd.admission_outbox.length, ledgerAtEnd.attempts.length);
  assert(ledgerAtEnd.admission_outbox.every((item: any) => item.delivered === 1
    || ledgerAtEnd.attempts.some((attempt: any) => attempt.attempt_id === item.attempt_id
      && attempt.state === 'settled' && attempt.refunded === 1 && attempt.intent_id === null
      && attempt.activation_seq > 0)));
  const inputAdmission = ledgerAtEnd.admission_outbox.find((item: any) => item.attempt_id === actual.submission.intent.attempt_id);
  assert.equal(inputAdmission?.delivered, 1, 'the actual input attempt must deliver its durable admission');
  const zeroAttempt = ledgerAtEnd.attempts.find((item: any) => item.attempt_id === zeroEffect.attempt_id);
  assert(zeroAttempt && zeroAttempt.activation_seq > 0 && zeroAttempt.intent_id === null
    && zeroAttempt.state === 'settled' && zeroAttempt.refunded === 1);
  assert(ledgerAtEnd.attempt_map.every((item: any) => ledgerAtEnd.decisions.some((decision: any) =>
    decision.decision_id === item.decision_id && decision.state === 'consumed' && decision.consumed_attempt === item.attempt_id)));
  assert.equal(ledgerAtEnd.model_calls.length, 0); assert.equal(ledgerAtEnd.decision_model_calls.length, 0);
  worldLogBytesAtEnd = existsSync(supervisor.worldLogPath) ? readFileSync(supervisor.worldLogPath).length : 0;
  assert.equal(worldLogBytesAtEnd, 0, 'simulated source cannot count as actual page input');
  checks.already_satisfied_fresh_zero_input = true; checks.durable_normal_attempt_admission = true;
  assert.equal(callbackErrors.length, 0);
  save('projections.json', projections);

  phase = 'stop-at-device';
  const pendingStop = waitForStop(supervisor);
  supervisor.stopAtDevice({ scope: 'all' }, 'cancel', 'supervisor', 'headless Root diagnostic complete');
  stopAck = await pendingStop; save('stop-at-device.json', stopAck);
  await until(() => supervisor!.projection?.browser_observation == null ? true : undefined, 'scope invalidation');
  await untilAsync(async () => !existsSync(ownedProfile!) && (await chromePidsForProfile(ownedProfile!, false)).length === 0 ? true : undefined,
    'exact owned Chrome/profile cleanup');
  checks.stop_invalidates_scope_and_closes_chrome = true;
  await supervisor.shutdown(); supervisorShutdown = true;
  assert(Object.values(pidsAtStart).every(pid => pid !== undefined && !processExists(pid)));
  assert.equal(existsSync(supervisorDir), false);
  assert.equal(processExists(browserPid), false);
  checks.all_owned_processes_closed = true;
  save('owned-process-cleanup.json', { pids: pidsAtStart, browser_pid: browserPid, all_exited: true,
    profile_removed: !existsSync(ownedProfile), supervisor_directory_removed: !existsSync(supervisorDir) });
  afterDisplay = await display(); save('display-after.json', afterDisplay);
  assert.equal(afterDisplay.foreground_pid, beforeDisplay.foreground_pid);
  assert.deepEqual(afterDisplay.foreground_visible_window_ids, beforeDisplay.foreground_visible_window_ids);
  checks.foreground_unchanged_after_cleanup = true;
  phase = 'complete';
} catch (error) {
  failure = error instanceof Error ? error.message : 'unknown failure';
  save('failure.json', { phase, error: failure,
    diagnostic: error instanceof HeadlessBrowserError ? error.diagnostic ?? null : null,
    browser_source_status: supervisor?.browserObservationStatus() ?? null, projection: supervisor?.projection ?? null });
  if (supervisor && existsSync(join(supervisor.dir, 'host.db'))) {
    try { save('failed-durable-ledger.json', await ledger(join(supervisor.dir, 'host.db'))); }
    catch { cleanupErrors.push('failure ledger could not be read'); }
  }
} finally {
  if (supervisor && !supervisorShutdown) {
    try { await supervisor.shutdown(); supervisorShutdown = true; }
    catch (error) { cleanupErrors.push(error instanceof Error ? error.message : 'shutdown failed'); }
  }
  pinsAfter = await capturePins(); save('pins-after.json', pinsAfter);
  if (pinsBefore) checks.pins_unchanged = pinsMatch(pinsBefore, pinsAfter);
  if (ownedProfile) checks.profile_removed = !existsSync(ownedProfile);
  if (supervisorDir) checks.supervisor_directory_removed = !existsSync(supervisorDir);
  if (pidsAtStart) checks.children_exited = Object.values(pidsAtStart).every(pid => pid === undefined || !processExists(pid));
  save('private-executions.json', executions); save('projections.json', projections);
}
const result = { phase, ok: failure === null && supervisorShutdown && cleanupErrors.length === 0 && Object.values(checks).every(Boolean),
  error: failure, checks, cleanup_errors: cleanupErrors, snapshot_ack_count: snapshotAcks.length,
  owned_profile: ownedProfile ?? null, browser_pid: browserPid ?? null, supervisor_pids: pidsAtStart,
  provider_calls: 0, desktop_input_calls: 0, private_device_completed_gestures: executions.filter(item => item.reply.outcome === 'completed').length,
  normal_root_completed: ledgerAtEnd?.root_tasks.filter((item: any) => item.state === 'completed').length ?? 0,
  mvp_complete: false, stop_acknowledged: stopAck !== null, pins_before: pinsBefore, pins_after: pinsAfter };
save('result.json', result); console.log(JSON.stringify({ ...result, pins_before: undefined, pins_after: undefined }));
if (!result.ok) process.exitCode = 1;
