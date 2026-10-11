// Real three-process/Engine/Attempt path. Only the owned fixture receives input.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Supervisor } from '../apps/desktop/src/main/supervisor.js';
import { NativeSession } from '../apps/desktop/src/main/nativeSession.js';
import { DecisionRelay } from '../apps/desktop/src/main/decisionRelay.js';
import { DecisionWorker, ScriptedTeacher } from '@computer-use/decision-worker';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const record = join(root, 'artifacts/m3-native-host-engine.json');
const evidence: Record<string, unknown> = { status: 'running', started_at: new Date().toISOString(), live_models_verified: false, product_authorization_ui_verified: false };
mkdirSync(dirname(record), { recursive: true });
const save = () => writeFileSync(record, JSON.stringify(evidence, null, 2) + '\n');
save();
execFileSync('bash', ['scripts/build-macos-source.sh'], { cwd: root, stdio: 'inherit' });
execFileSync('bash', ['scripts/build-macos-device.sh'], { cwd: root, stdio: 'inherit' });
execFileSync('cargo', ['build', '--locked', '-p', 'desktop-host', '-p', 'device-macos', '-p', 'source-macos'], { cwd: root, stdio: 'inherit' });
const contents = join(root, 'target/native-macos/GuardFixture.app/Contents');
const binary = join(contents, 'MacOS/GuardFixture');
mkdirSync(dirname(binary), { recursive: true });
writeFileSync(join(contents, 'Info.plist'), '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.lingxi.computer-user.guard-fixture</string><key>CFBundleExecutable</key><string>GuardFixture</string><key>CFBundleName</key><string>Computer Use Guard Fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>');
execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors', '-parse-as-library', '-target', 'arm64-apple-macosx14.0', '-module-cache-path', join(root, 'target/native-macos/module-cache'), join(root, 'native/device/fixtures/GuardFixture.swift'), '-o', binary], { stdio: 'inherit' });
const pause = (ms: number) => new Promise((done) => setTimeout(done, ms));
async function until<T>(test: () => T | undefined | null | false, reason: string, timeout = 8000): Promise<T> {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const result = test(); if (result) return result; await pause(10); }
  throw new Error(`timeout: ${reason}`);
}
let supervisor: Supervisor | undefined;
let session: NativeSession | undefined;
let fixture: ChildProcess | undefined;
const logs: string[] = [], replies: Record<string, any>[] = [];
const binaries = { device: join(root, 'target/debug/cu-device'), source: join(root, 'target/debug/cu-source'), host: join(root, 'target/debug/cu-host') };
const source = { helperPath: join(root, 'target/debug/cu-macos-source'), guardHelperPath: join(root, 'target/debug/cu-macos-device-guard'), bundleId: 'com.lingxi.computer-user.guard-fixture' };
const sha = (file: string) => createHash('sha256').update(readFileSync(file)).digest('hex');
const sourceFiles = execFileSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', 'crates', 'native', 'apps/desktop/src', 'packages', 'Cargo.lock', 'scripts/smoke-macos-host-engine.mts'], { cwd: root, encoding: 'utf8' }).split('\0').filter(Boolean).sort();
const sourceHash = createHash('sha256');
for (const file of sourceFiles) sourceHash.update(file + '\0').update(readFileSync(join(root, file))).update('\0');
evidence.build = { captured_before_execution: true, head: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
  source_tree_sha256: sourceHash.digest('hex'), binaries: Object.fromEntries([...Object.values(binaries), source.helperPath, source.guardHelperPath, binary].map((path) => [path.slice(root.length + 1), sha(path)])),
  schema_sha256: sha(join(root, 'contracts/desktop-v2/schema.json')) };
try {
  fixture = spawn(binary, ['--text-mode'], { stdio: ['pipe', 'pipe', 'pipe'] });
  fixture.stdin!.on('error', () => {});
  let pending = '';
  fixture.stdout!.on('data', (data: Buffer) => { pending += data; for (let i; (i = pending.indexOf('\n')) >= 0;) { replies.push(JSON.parse(pending.slice(0, i))); pending = pending.slice(i + 1); } });
  const ready = await until(() => replies.find((r) => r.kind === 'ready'), 'fixture');
  const nativeSource = { ...source, pid: ready.pid as number, referencePlanner: true };
  let approved = false, confirmations = 0;
  session = await NativeSession.start({ binaries, nativeSource, mode: 'observe' }, async () => { confirmations += 1; return approved; }, async (options) => {
    supervisor = await Supervisor.start(options); return supervisor;
  });
  session.on('log', (line) => { logs.push(line); if (logs.length > 128) logs.shift(); });
  const relay = new DecisionRelay(session, new DecisionWorker(new ScriptedTeacher())); relay.attach();
  session.on('sessionChanged', () => relay.replaceWorker(new DecisionWorker(new ScriptedTeacher())));
  const first = await until(() => supervisor!.projection?.observation?.observed_target, 'current private window metadata');
  await until(() => {
    const current = supervisor!.projection?.observation?.observed_target;
    return current && current.observed_ms > first.observed_ms + 1500 ? current : undefined;
  }, 'AX metadata remains live while private pixels are withheld');
  assert.equal(supervisor.projection?.capabilities?.input, 'unavailable');
  assert.equal(supervisor.projection?.observation?.frame?.frame_id ?? null, null);
  assert.equal(first.contains_private_fields, true);
  assert.equal(first.pid, ready.pid); assert.equal(first.window_id, `window-${ready.window_id}`);
  evidence.private_ax_metadata_stays_live = true;
  const oldDir = supervisor!.dir;
  assert.throws(() => session!.operator({ command: 'set_mode', mode: 'execute' }), /Authorize/);
  await session.authorizeNativeWindow(session.authorization.candidate!.snapshot_id);
  assert.equal(supervisor!.dir, oldDir); assert.equal(session.authorization.phase, 'observing');
  fixture.stdin!.write('stats\n');
  assert.equal((await until(() => replies.find((r) => r.kind === 'stats'), 'readonly fixture stats')).value_calls, 0);
  approved = true;
  await session.authorizeNativeWindow(session.authorization.candidate!.snapshot_id);
  assert.equal(existsSync(oldDir), false); assert.equal(session.authorization.phase, 'authorized');
  assert.equal(confirmations, 2); evidence.main_authorization_session_verified = true;
  await until(() => supervisor!.projection?.capabilities?.input === 'available', 'scoped native backend');
  session.operator({ command: 'create_task', text: 'Fill document: MVP 中文\nHello 👋' });
  const prompt = await until(() => supervisor!.projection?.pending_confirmation, 'ordinary goal confirmation');
  assert.equal(prompt.kind.kind, 'goal');
  session.operator({ command: 'confirm_goal', confirmation_id: prompt.confirmation_id, approved: true });
  await until(() => supervisor!.projection?.root?.state === 'ready', 'confirmed root stays ready in observe mode');
  assert.equal(existsSync(supervisor.worldLogPath) ? readFileSync(supervisor.worldLogPath).length : 0, 0);
  session.operator({ command: 'set_mode', mode: 'execute' });
  const completed = await until(() => {
    const p = supervisor!.projection;
    if (p?.root && ['failed', 'suspended', 'cancelled'].includes(p.root.state)) throw new Error(JSON.stringify(p));
    return p?.root?.state === 'completed' ? p : undefined;
  }, 'full native host/Engine/Attempt completion', 35000);
  assert.equal(existsSync(supervisor.worldLogPath) ? readFileSync(supervisor.worldLogPath).length : 0, 0);
  replies.length = 0; fixture.stdin!.write('stats\n');
  const stats = await until(() => replies.find((r) => r.kind === 'stats'), 'completed fixture stats');
  assert.equal(stats.value_calls, 1); assert.equal(stats.navigation_calls, 2); assert.equal(stats.press_calls, 0);
  evidence.root = completed.root; evidence.fixture_stats = stats;
  const ledger = JSON.parse(execFileSync('python3', ['-c', `import sqlite3,json,sys\nc=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True)\nc.row_factory=sqlite3.Row\ntables=['attempts','decisions','attempt_map','admission_outbox','effect_results','root_tasks']\nd={t:[dict(r) for r in c.execute('select * from '+t)] for t in tables}\nfor r in d['effect_results']:r['result']=json.loads(r.pop('json'))\nfor r in d['admission_outbox']:r['payload']=json.loads(r['payload'])\nprint(json.dumps(d))`, join(supervisor!.dir, 'host.db')], { encoding: 'utf8' }));
  evidence.ledger = ledger; save();
  assert.ok(ledger.attempts.length >= 3);
  assert.equal(ledger.attempt_map.length, ledger.attempts.length);
  assert.equal(ledger.admission_outbox.length, ledger.attempts.length);
  assert.ok(ledger.admission_outbox.some((row: any) => row.delivered === 1));
  assert.ok(ledger.admission_outbox.every((row: any) => row.delivered === 1 || ledger.attempts.some((a: any) => a.attempt_id === row.attempt_id && a.refunded === 1 && ['settled', 'preempted', 'failed', 'cancelled'].includes(a.state))));
  assert.ok(ledger.attempt_map.every((row: any) => ledger.decisions.some((d: any) => d.decision_id === row.decision_id && d.consumed_attempt === row.attempt_id && d.state === 'consumed')));
  assert.ok(ledger.effect_results.some((row: any) => row.result.status === 'satisfied' && row.result.confirmation_scope === 'ui_revisit' && row.result.evidence_group.length > 0 && row.result.causality === 'unknown'));
  assert.ok(ledger.effect_results.every((row: any) => !row.result.input_span || row.result.observed_ms >= row.result.input_span.to_ms));
  assert.equal(ledger.root_tasks[0].state, 'completed');
  assert.equal(JSON.parse(ledger.root_tasks[0].goal_json).desired.value, 'MVP 中文\nHello 👋');
  evidence.ledger = ledger;
  await session.revokeNativeWindow();
  await until(() => session!.projection?.capabilities?.input === 'unavailable', 'revoked input capability');
  assert.equal(session.authorization.phase, 'observing'); assert.equal(session.projection?.root ?? null, null);
  assert.throws(() => session!.operator({ command: 'set_mode', mode: 'execute' }), /Authorize/);
  await pause(1200); replies.length = 0; fixture.stdin!.write('stats\n');
  const afterRevoke = await until(() => replies.find((r) => r.kind === 'stats'), 'revoked fixture stats');
  assert.equal(afterRevoke.value_calls, 1); assert.equal(afterRevoke.navigation_calls, 2);
  evidence.revoked_without_replay = true;
  evidence.status = 'owned_native_host_engine_verified'; evidence.verified_at = new Date().toISOString(); save();
  console.log(JSON.stringify({ status: evidence.status, verified_at: evidence.verified_at, root: evidence.root, stats, attempts: ledger.attempts.length, effects: ledger.effect_results.length, record }));
} catch (error) { evidence.status = 'failed'; evidence.error = String(error); evidence.logs = logs; evidence.projection = supervisor?.projection; save(); throw error; }
finally { await session?.shutdown(); if (!session) await supervisor?.shutdown(); if (fixture && fixture.exitCode == null && fixture.signalCode == null) { fixture.kill('SIGKILL'); await new Promise((done) => fixture!.once('exit', done)); } }
