// Real read-only browser ingress through plain Node Supervisor and cu-host.
// Run only after the root-built bins exist: node --import tsx scripts/smoke-headless-source-ingress.mts --chrome <absolute Chrome> --output <new absolute directory>
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

import type { BrowserPageSnapshot, StopAck, UiProjection } from '@computer-use/desktop-api';
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
  join(root, 'scripts/smoke-headless-source-ingress.mts'),
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
let databaseCountsAtEnd: { root_tasks: number; attempts: number } | null = null;
let simulatedAncillaryObservationPresent: boolean | null = null;

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

function projectionFor(supervisorValue: Supervisor, snapshot: BrowserPageSnapshot) {
  const recorded = projectionBySnapshotId.get(snapshot.snapshot_id);
  if (recorded) return recorded;
  const current = supervisorValue.projection;
  const view = current?.browser_observation;
  return view?.snapshot_id === snapshot.snapshot_id ? view : undefined;
}

function assertReadOnlyProjection(view: NonNullable<UiProjection['browser_observation']>, snapshot: BrowserPageSnapshot): void {
  assert.equal(view.snapshot_id, snapshot.snapshot_id);
  assert.equal(view.source_sequence, snapshot.source_sequence);
  assert.equal(view.target_context_hash, snapshot.target_context_hash);
  assert.deepEqual(view.scope.context, snapshot.context);
  assert.equal(view.scope.clock_domain, snapshot.clock_domain);
  assert(view.readonly_binding_ref, 'Host must project the read-only browser binding reference');
  assert.equal(view.readonly_checked, false);
}

function currentTasklessProjection(value: UiProjection): void {
  assert.equal(value.mode, 'observe');
  assert.equal(value.capabilities?.backend, 'simulated');
  assert.equal(value.root ?? null, null);
  assert.equal(value.pending_confirmation ?? null, null);
  const browserDomain = value.capabilities?.domains.find(domain => domain.domain === 'headless_browser');
  assert.equal(browserDomain?.state, 'unavailable', 'browser observation must not advertise browser input');
}

async function databaseCounts(databasePath: string): Promise<{ root_tasks: number; attempts: number }> {
  const python = [
    'import json, pathlib, sqlite3, sys',
    'db = sqlite3.connect(pathlib.Path(sys.argv[1]).as_uri() + "?mode=ro", uri=True)',
    'names = ("root_tasks", "attempts")',
    'counts = {name: db.execute("SELECT COUNT(*) FROM " + name).fetchone()[0] for name in names}',
    'print(json.dumps(counts))',
  ].join('\n');
  const { stdout } = await run('python3', ['-c', python, databasePath], { encoding: 'utf8', timeout: 4_000, maxBuffer: 16_384 });
  return JSON.parse(stdout) as { root_tasks: number; attempts: number };
}

function waitForStopAck(target: Supervisor, timeoutMs = 5_000): Promise<StopAck> {
  return new Promise((resolveAck, rejectAck) => {
    const timer = setTimeout(() => {
      target.off('stopAck', onAck);
      rejectAck(new Error('timed out waiting for StopAtDevice acknowledgement'));
    }, timeoutMs);
    const onAck = (ack: StopAck): void => {
      clearTimeout(timer);
      resolveAck(ack);
    };
    target.once('stopAck', onAck);
  });
}

function onBrowserSnapshot(snapshot: Readonly<BrowserPageSnapshot>): void {
  try {
    const copied = structuredClone(snapshot) as BrowserPageSnapshot;
    snapshotAcks.push(copied);
    const index = String(snapshotAcks.length).padStart(2, '0');
    save(`snapshot-ack-${index}.json`, { acknowledgement: 'host accepted browser_page_snapshot', snapshot: copied });
  } catch (error) {
    callbackErrors.push(error instanceof Error ? error.message : 'snapshot artifact write failed');
  }
}

function onProjection(projection: Readonly<UiProjection>): void {
  const view = projection.browser_observation;
  if (!view) return;
  try { projectionBySnapshotId.set(view.snapshot_id, structuredClone(view)); }
  catch (error) { callbackErrors.push(error instanceof Error ? error.message : 'projection copy failed'); }
}

const before = {
  created_at: new Date().toISOString(),
  root,
  output,
  binaries,
  chrome_executable: chromeExecutable,
  fixture_path: fixturePath,
  display_probe: displayProbe,
  run_shape: {
    supervisor: 'plain Node Supervisor.start',
    mode: 'observe',
    browser_source_explicit: true,
    native_source: false,
    model_authorization: false,
    provider_calls: 0,
    task_commands_sent: 0,
    decision_proposals_sent: 0,
    browser_action_commands_sent: 0,
    desktop_input_commands_sent: 0,
    browser_source_api: 'observation-only; start/refresh/stop, no page action route',
    business_gate_eligible: false,
    ancillary_device: 'default simulated backend, no action submitted',
    ancillary_source: 'default fixture backend, no input log events expected',
  },
};

try {
  phase = 'pin-before';
  pinsBefore = await capturePins();
  save('before.json', { ...before, pins: pinsBefore });
  assertAllPinsPresent(pinsBefore);
  assertExecutable(chromeExecutable);
  for (const path of Object.values(binaries)) assertExecutable(path);
  assertExecutable(displayProbe);
  assertFixture(fixturePath);
  checks.fixed_bins_and_fixture_pinned = true;

  phase = 'display-before';
  beforeDisplay = await display();
  save('display-before.json', { probe: displayProbe, ...beforeDisplay });

  phase = 'supervisor-start';
  supervisor = await Supervisor.start({
    binaries,
    mode: 'observe',
    browserSource: { chromeExecutable, fixturePath },
    observeBrowserSourceOwner: owner => {
      supervisorDir = owner.supervisorDir;
      profileParent = owner.profileParent;
      pidsAtStart = { ...owner.pids };
      save('startup-owner.json', owner);
    },
    observeBrowserSnapshot: onBrowserSnapshot,
    observeProjection: onProjection,
  });
  const activeSupervisorDir = supervisor.dir;
  supervisorDir = activeSupervisorDir;
  const owned = findProfile(supervisor);
  const activeProfile = owned.profile;
  profileParent = owned.parent;
  ownedProfile = activeProfile;
  pidsAtStart = supervisor.pids;
  for (const pid of Object.values(pidsAtStart)) assert(Number.isSafeInteger(pid) && pid! > 0);
  const sourceStatus = supervisor.browserObservationStatus();
  save('supervisor-owner.json', {
    supervisor_dir: supervisorDir,
    pids: pidsAtStart,
    profile_parent: profileParent,
    owned_profile: ownedProfile,
    browser_source_status: sourceStatus,
    host_scope_control_acknowledged: Boolean(sourceStatus?.phase === 'ready' && sourceStatus.scopeId),
    host_snapshot_control_acknowledgement_callback: 'observeBrowserSnapshot runs after browser_page_snapshot command_ack accepted',
  });
  assert(sourceStatus && sourceStatus.scopeId);
  assert.equal(sourceStatus.phase, 'ready');
  checks.host_scope_install_acknowledged = true;

  phase = 'snapshot-projections';
  const first = await until(() => snapshotAcks[0], 'first host-acknowledged browser DTO');
  const firstView = await until(() => projectionFor(supervisor!, first), 'first UiProjection.browser_observation');
  assertReadOnlyProjection(firstView, first);
  const firstProjection = supervisor.projection;
  assert(firstProjection);
  currentTasklessProjection(firstProjection);

  await supervisor.refreshBrowserObservation();
  const second = await until(() => snapshotAcks[1], 'second host-acknowledged browser DTO');
  const secondView = await until(() => projectionFor(supervisor!, second), 'second UiProjection.browser_observation');
  assertReadOnlyProjection(secondView, second);
  const secondProjection = supervisor.projection;
  assert(secondProjection);
  currentTasklessProjection(secondProjection);
  simulatedAncillaryObservationPresent = Boolean(secondProjection.observation);

  assert.deepEqual(second.context, first.context);
  assert.equal(second.target_context_hash, first.target_context_hash);
  assert(second.source_sequence > first.source_sequence);
  assert.notEqual(second.snapshot_id, first.snapshot_id);
  assert.equal(secondView.readonly_checked, false);
  checks.two_acknowledged_same_context_reads_project_readonly_false = true;
  checks.ancillary_device_simulated = true;
  checks.ancillary_source_fixture_mode = true;
  save('projection-01.json', { acknowledgement: 'host accepted snapshot', snapshot: first, browser_observation: firstView });
  save('projection-02.json', { acknowledgement: 'host accepted snapshot', snapshot: second, browser_observation: secondView });

  const browserPids = await chromePidsForProfile(activeProfile, true);
  assert.equal(browserPids.length, 1, 'ps must identify one top-level Chrome process by exact executable and owned profile');
  browserPid = browserPids[0]!;
  phase = 'display-during';
  duringDisplay = await display(browserPid);
  save('display-during.json', { probe: displayProbe, browser_pid: browserPid, profile: activeProfile, ...duringDisplay });
  assert.equal(duringDisplay.owned_browser_onscreen_window_count, 0);
  assert.equal(duringDisplay.foreground_pid, beforeDisplay.foreground_pid);
  assert.deepEqual(duringDisplay.foreground_visible_window_ids, beforeDisplay.foreground_visible_window_ids);
  checks.headless_chrome_has_no_visible_window = true;
  checks.foreground_window_unchanged_during_owned_read = true;

  phase = 'no-task-attempt-input-check';
  const worldLogBytes = existsSync(supervisor.worldLogPath) ? readFileSync(supervisor.worldLogPath).length : 0;
  const counts = await databaseCounts(join(activeSupervisorDir, 'host.db'));
  databaseCountsAtEnd = counts;
  assert.equal(worldLogBytes, 0);
  assert.deepEqual(counts, { root_tasks: 0, attempts: 0 });
  assert.equal(callbackErrors.length, 0);
  save('no-business-or-input.json', {
    mode: 'observe',
    native_source_configured: false,
    model_authorization_supplied: false,
    input_authorization: 'none',
    host_task_commands_sent: 0,
    decision_proposals_sent: 0,
    browser_action_commands_sent: 0,
    desktop_input_commands_sent: 0,
    browser_capability: 'unavailable',
    simulation_log_is_not_browser_input_evidence: true,
    simulated_ancillary_observation_present: Boolean(secondProjection.observation),
    simulated_ancillary_observation_is_separate_from_browser_evidence: true,
    world_log_bytes: worldLogBytes,
    database_counts: counts,
    callback_errors: callbackErrors,
    business_gate_eligible: false,
  });
  checks.simulation_log_empty_and_no_root_attempt = true;
  checks.no_provider_or_input_route_used = true;
  worldLogBytesAtEnd = worldLogBytes;

  phase = 'stop-at-device';
  const stopAckTask = waitForStopAck(supervisor);
  supervisor.stopAtDevice({ scope: 'all' }, 'cancel', 'supervisor', 'readonly browser ingress diagnostic complete');
  stopAck = await stopAckTask;
  assert.equal(stopAck.scope.scope, 'all');
  assert.equal(stopAck.disposition, 'cancel');
  assert.equal(stopAck.source, 'supervisor');
  assert(['not_needed', 'released'].includes(stopAck.cleanup));
  save('stop-at-device.json', { acknowledgement: stopAck, browser_source_status: supervisor.browserObservationStatus() });
  await until(() => supervisor!.projection?.browser_observation == null ? true : undefined, 'Host projection clears browser evidence');
  const stoppedProfile = await untilAsync(async () => {
    const remainingPids = await chromePidsForProfile(activeProfile, false);
    return !existsSync(activeProfile) && remainingPids.length === 0 ? remainingPids : undefined;
  }, 'owned Chrome profile and exact-profile process cleanup');
  assert.deepEqual(stoppedProfile, []);
  checks.stop_at_device_clears_projection_and_owned_chrome = true;

  const stoppedLogBytes = existsSync(supervisor.worldLogPath) ? readFileSync(supervisor.worldLogPath).length : 0;
  const stoppedCounts = await databaseCounts(join(activeSupervisorDir, 'host.db'));
  databaseCountsAtEnd = stoppedCounts;
  assert.equal(stoppedLogBytes, 0);
  assert.deepEqual(stoppedCounts, { root_tasks: 0, attempts: 0 });
  save('after-stop-ingress.json', { world_log_bytes: stoppedLogBytes, database_counts: stoppedCounts,
    browser_observation: supervisor.projection?.browser_observation ?? null });
  worldLogBytesAtEnd = stoppedLogBytes;

  phase = 'supervisor-shutdown';
  await supervisor.shutdown();
  supervisorShutdown = true;
  const stoppedPids = Object.fromEntries(Object.entries(pidsAtStart!).map(([role, pid]) => [role,
    pid === undefined ? false : processExists(pid)]));
  assert(Object.values(stoppedPids).every(alive => alive === false));
  assert.equal(existsSync(activeProfile), false);
  assert.equal(existsSync(activeSupervisorDir), false);
  checks.supervisor_children_and_profile_closed = true;
  save('owned-process-cleanup.json', {
    supervisor_pids_alive_after_shutdown: stoppedPids,
    browser_pid: browserPid,
    profile_removed: !existsSync(activeProfile),
    supervisor_directory_removed: !existsSync(activeSupervisorDir),
  });

  phase = 'display-after';
  afterDisplay = await display();
  save('display-after.json', { probe: displayProbe, ...afterDisplay });
  assert.equal(afterDisplay.foreground_pid, beforeDisplay.foreground_pid);
  assert.deepEqual(afterDisplay.foreground_visible_window_ids, beforeDisplay.foreground_visible_window_ids);
  checks.foreground_window_unchanged_after_cleanup = true;

  phase = 'pin-after';
  pinsAfter = await capturePins();
  checks.executable_and_source_pins_unchanged = pinsMatch(pinsBefore, pinsAfter);
  assert.equal(checks.executable_and_source_pins_unchanged, true);
  save('pins-after.json', pinsAfter);
  phase = 'complete';
} catch (error) {
  failure = error instanceof Error ? error.message : 'unknown failure';
  save('failure.json', {
    phase,
    error: failure,
    diagnostic: error instanceof HeadlessBrowserError ? error.diagnostic ?? null : null,
    browser_source_status: supervisor?.browserObservationStatus() ?? null,
    projection: supervisor?.projection?.browser_observation ?? null,
    snapshot_ack_count: snapshotAcks.length,
  });
} finally {
  if (supervisor && !supervisorShutdown) {
    try {
      await supervisor.shutdown();
      supervisorShutdown = true;
    } catch (error) {
      cleanupErrors.push(error instanceof Error ? error.message : 'Supervisor shutdown failed');
    }
  }
  if (!afterDisplay) {
    try {
      afterDisplay = await display();
      save('display-after.json', { probe: displayProbe, ...afterDisplay });
    } catch (error) {
      cleanupErrors.push(`after display probe: ${error instanceof Error ? error.message : 'failed'}`);
    }
  }
  if (!pinsAfter) {
    pinsAfter = await capturePins();
    if (pinsBefore) checks.executable_and_source_pins_unchanged = pinsMatch(pinsBefore, pinsAfter);
    save('pins-after.json', pinsAfter);
  }
  if (ownedProfile) checks.profile_removed_after_cleanup = !existsSync(ownedProfile);
  if (supervisorDir) checks.supervisor_directory_removed_after_cleanup = !existsSync(supervisorDir);
  if (pidsAtStart) {
    checks.supervisor_children_exited_after_cleanup = Object.values(pidsAtStart)
      .every(pid => pid === undefined || !processExists(pid));
  }
}

const result = {
  phase,
  ok: failure === null && supervisorShutdown && cleanupErrors.length === 0
    && Object.values(checks).length > 0 && Object.values(checks).every(Boolean),
  error: failure,
  cleanup_errors: cleanupErrors,
  checks,
  snapshot_ack_count: snapshotAcks.length,
  browser_pid: browserPid ?? null,
  owned_profile: ownedProfile ?? null,
  supervisor_pids: pidsAtStart,
  stop_acknowledged: stopAck !== null,
  display_before: beforeDisplay ?? null,
  display_during: duringDisplay ?? null,
  display_after: afterDisplay ?? null,
  pins_before: pinsBefore ?? null,
  pins_after: pinsAfter ?? null,
  simulated_world_log_bytes: worldLogBytesAtEnd,
  root_attempts_created: databaseCountsAtEnd
    ? databaseCountsAtEnd.root_tasks > 0 || databaseCountsAtEnd.attempts > 0 : null,
  provider_calls: 0,
  target_input_calls: 0,
  desktop_input_calls: 0,
  business_gate_eligible: false,
  live_headless_chrome: browserPid !== undefined,
  host_scope_and_snapshot_acknowledged: checks.host_scope_install_acknowledged === true
    && checks.two_acknowledged_same_context_reads_project_readonly_false === true,
  ancillary_device_and_source_are_simulated: checks.ancillary_device_simulated === true
    && checks.ancillary_source_fixture_mode === true,
  simulated_ancillary_observation_present: simulatedAncillaryObservationPresent,
  simulated_ancillary_observation_is_browser_evidence: false,
};
save('result.json', result);
console.log(JSON.stringify(result));
if (!result.ok) process.exitCode = 1;
