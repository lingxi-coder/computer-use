#!/usr/bin/env node
// Frozen, sequential CPU intra-op thread comparison for the pinned Laya backend.
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { lstat, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import os from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ARTIFACTS_DIR = join(ROOT, 'artifacts');
const BENCHMARK_PATH = join(ROOT, 'scripts/benchmark-laya-cpu.mts');
const BASELINE_PATH = join(ARTIFACTS_DIR,
  'm0-laya-cpu-latency-5a42ee43-ea71-46c4-9419-aba8180737cc.json');
const BASELINE_SHA256 = '91127307967b0b55cc59fa49ad4fa0e9270b4124ce36474d4fcb0f52bab0aee6';
const MODEL_ID = 'convaiinnovations/laya-multilingual';
const MODEL_REVISION = 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67';
const MODEL_PACKAGE_MANIFEST_SHA256 = 'c4e5cb4dc827f1ca76a47f3c19d88ef7cd47a53268e474b0c98df6c998399424';
const ORT_VERSION = '1.30.0';
const EXECUTION_PROVIDER = 'cpu';
const INTER_OP_THREADS = 1;
const CONFIDENCE_THRESHOLD_MILLI = 800;
const ACTION_PROBABILITY_THRESHOLD = 0.5;
const THREAD_ARMS = [1, 2, 4] as const;
const PROFILE_IDS = ['short_choice_2', 'medium_choice_2', 'near_512_choice_2'] as const;
const PROFILE_EXPECTATIONS = [
  {
    profile_id: 'short_choice_2',
    sequence_tokens: 264,
    request_template_sha256: '286dd9b1cf5cc2834015f22837e9ad4680dc2669db3a4bad40568b43abbe24c8',
    sequence_sha256: '2e531c4c91bb165272887fce5ee7c07e19558250f402ef20f7a9a3622db127a4',
  },
  {
    profile_id: 'medium_choice_2',
    sequence_tokens: 401,
    request_template_sha256: 'af30a2405a5072d85590c634486064e0bb810377bd152a2205dcaf6eef4baec7',
    sequence_sha256: 'e89c2187d38cce14f14c8d35becd7b942dfe9a708c8625036e8ab5c89d2cf0ea',
  },
  {
    profile_id: 'near_512_choice_2',
    sequence_tokens: 475,
    request_template_sha256: 'f81abd86130ea93795beddca8a217f68de5cf67d1c74458eda3da98af204f2f7',
    sequence_sha256: '40e7a5662aedfd0674c23b84dd008c06c6a84e478914166a6eeaf35ec8526cc5',
  },
] as const;
const WARMUP_COUNT = 5;
const HOT_COUNT = 100;
const PROFILE_DEADLINE_MS = 180_000;
const ARM_DEADLINE_MS = 600_000;
const CHILD_CLEANUP_GRACE_MS = 1_500;
const COMPARISON_DEADLINE_MS = 1_860_000;
const MAX_CAPTURED_OUTPUT_BYTES = 64 * 1024;
const REPORT_NAME = /^m0-laya-cpu-latency-[0-9a-f-]{36}\.json$/;

type ThreadCount = typeof THREAD_ARMS[number];
type JsonRecord = Record<string, unknown>;
type ReportFile = {
  path: string;
  sha256: string;
  bytes: number;
  json_valid: boolean;
  report_status: string | null;
  report_run_id: string | null;
  intra_op_threads: number | null;
};
type ProfileValidation = {
  profile_id: string;
  valid: boolean;
  request_template_sha256: string | null;
  sequence_sha256: string | null;
  sequence_tokens: number | null;
  warmup_samples: number;
  hot_samples: number;
  hot_both_graphs_count: number;
  hot_refusal_count: number;
  p50_wall_ms: number | null;
  p95_wall_ms: number | null;
  max_wall_ms: number | null;
  issues: string[];
};
type ArmRecord = {
  order_index: number;
  intra_op_threads: ThreadCount;
  status: string;
  started_at: string | null;
  finished_at: string | null;
  elapsed_ms: number | null;
  exit_code: number | null;
  exit_signal: string | null;
  timed_out: boolean;
  interrupted: string | null;
  report: ReportFile | null;
  discovered_reports: ReportFile[];
  report_identity_valid: boolean;
  host_identity_valid: boolean;
  source_runtime_integrity_valid: boolean;
  source_runtime_identity: JsonRecord | null;
  profile_validations: ProfileValidation[];
  all_profiles_p95_at_or_below_500_ms: boolean;
  worst_profile_p95_ms: number | null;
  refusal_counts_by_profile: Record<string, number>;
  stdout_tail: string;
  stderr_tail: string;
  runner_error: string | null;
};

const SIGINT_CODE = 130;
const SIGTERM_CODE = 143;
let interruptionSignal: NodeJS.Signals | null = null;
let stopCurrentArm: ((signal: NodeJS.Signals) => void) | null = null;
let baselineHostIdentity: JsonRecord | null = null;

function onSignal(signal: NodeJS.Signals): void {
  interruptionSignal ??= signal;
  stopCurrentArm?.(signal);
}

process.on('SIGINT', onSigint);
process.on('SIGTERM', onSigterm);

if (process.argv.length > 2) {
  console.error('Usage: node --import tsx scripts/compare-laya-cpu-threads.mts');
  process.exitCode = 2;
} else {
  await runComparison();
}

async function runComparison(): Promise<void> {
  const runId = randomUUID();
  const planPath = join(ARTIFACTS_DIR, `m0-laya-cpu-thread-sweep-plan-${runId}.json`);
  const comparisonPath = join(ARTIFACTS_DIR, `m0-laya-cpu-thread-sweep-comparison-${runId}.json`);
  const start = performance.now();
  const startedAt = new Date().toISOString();
  const initialHost = hostEnvironment();
  let baselineBytes: Buffer | null = null;
  let baselineSha256: string | null = null;
  let baseline: JsonRecord | null = null;
  let baselineVerificationError: string | null = null;
  let sourcePreflightError: string | null = null;
  let benchmarkSourceSha256: string | null = null;
  let comparatorSourceSha256: string | null = null;

  await mkdir(ARTIFACTS_DIR, { recursive: true });
  try {
    const baselineStat = await lstat(BASELINE_PATH);
    if (baselineStat.isSymbolicLink() || !baselineStat.isFile()) throw new Error('baseline_report_not_regular_file');
    baselineBytes = await readFile(BASELINE_PATH);
    baselineSha256 = sha256(baselineBytes);
    if (baselineSha256 !== BASELINE_SHA256) throw new Error('baseline_report_hash_mismatch');
    baseline = parseRecord(baselineBytes);
    if (baseline['kind'] !== 'm0_laya_cpu_latency_scan' || baseline['status'] !== 'completed_latency_scan') {
      throw new Error('baseline_report_identity_mismatch');
    }
  } catch (error) {
    baselineVerificationError = error instanceof Error ? error.message : 'baseline_verification_failed';
  }
  if (!baselineVerificationError) {
    try {
      benchmarkSourceSha256 = await hashFile(BENCHMARK_PATH);
      comparatorSourceSha256 = await hashFile(fileURLToPath(import.meta.url));
    } catch (error) {
      sourcePreflightError = error instanceof Error ? error.message : 'source_preflight_failed';
    }
  }

  const expectedHost = baseline ? stableHostIdentity(baseline['environment']) : null;
  const actualHost = stableHostIdentity(initialHost);
  const hostPreflightValid = expectedHost !== null && stableJson(expectedHost) === stableJson(actualHost);
  baselineHostIdentity = expectedHost;
  const plan: JsonRecord = {
    schema_version: 1,
    kind: 'm0_laya_cpu_thread_sweep_plan',
    run_id: runId,
    frozen_at: new Date().toISOString(),
    status: 'frozen_before_measurement',
    order: [...THREAD_ARMS],
    execution: {
      sequential: true,
      repeat_count_per_arm: 1,
      parallel_arms: false,
      execution_provider: EXECUTION_PROVIDER,
      model_id: MODEL_ID,
      model_revision: MODEL_REVISION,
      package_manifest_sha256: MODEL_PACKAGE_MANIFEST_SHA256,
      onnxruntime_version: ORT_VERSION,
      inter_op_threads: INTER_OP_THREADS,
      warmup_count_per_profile: WARMUP_COUNT,
      hot_count_per_profile: HOT_COUNT,
      profile_deadline_ms: PROFILE_DEADLINE_MS,
      arm_deadline_ms: ARM_DEADLINE_MS,
      child_cleanup_grace_ms: CHILD_CLEANUP_GRACE_MS,
      comparison_deadline_ms: COMPARISON_DEADLINE_MS,
      confidence_threshold_milli: CONFIDENCE_THRESHOLD_MILLI,
      action_probability_threshold: ACTION_PROBABILITY_THRESHOLD,
      profile_ids: [...PROFILE_IDS],
      profile_input_identities: PROFILE_EXPECTATIONS,
      sample_request_hashes: 'Preserved per sample in each raw report; request-template and sequence hashes are the frozen cross-arm content identity.',
      p95_target_ms: 500,
      selection_rule: 'Only a complete, identity-valid arm with p95 <= 500 ms for all three profiles qualifies. Select the qualifying arm with the lowest worst-profile p95; an exact tie prefers 2 threads. If none qualifies, make no offline recommendation. Product runtime remains at 2 threads in all cases.',
      no_retries_or_result_driven_repeats: true,
      no_threshold_changes_or_input_trimming: true,
      synthetic_quality_notice: 'This is offline synthetic latency only. Refusals remain in latency samples; they do not establish semantic or business quality.',
      joint_load_or_physical_input: false,
      product_runtime_auto_change: false,
    },
    source_freeze: {
      benchmark_script_sha256: benchmarkSourceSha256,
      comparator_script_sha256: comparatorSourceSha256,
      preflight_error: sourcePreflightError,
    },
    baseline_reference: {
      path: relative(ROOT, BASELINE_PATH),
      expected_sha256: BASELINE_SHA256,
      actual_sha256: baselineSha256,
      verified: baselineVerificationError === null && baseline !== null,
      verification_error: baselineVerificationError,
      profile_input_identities: PROFILE_EXPECTATIONS,
      runtime: {
        package: 'onnxruntime-node',
        version: ORT_VERSION,
        execution_provider: EXECUTION_PROVIDER,
        baseline_intra_op_threads: 2,
        inter_op_threads: INTER_OP_THREADS,
      },
      expected_host_identity: expectedHost,
    },
    host_preflight: {
      actual: initialHost,
      valid: hostPreflightValid,
    },
  };

  await writeArtifactOnce(planPath, plan);
  const planBytes = await readFile(planPath);
  const planSha256 = sha256(planBytes);
  const arms: ArmRecord[] = THREAD_ARMS.map((threads, orderIndex) => ({
    order_index: orderIndex + 1,
    intra_op_threads: threads,
    status: 'not_started',
    started_at: null,
    finished_at: null,
    elapsed_ms: null,
    exit_code: null,
    exit_signal: null,
    timed_out: false,
    interrupted: null,
    report: null,
    discovered_reports: [],
    report_identity_valid: false,
    host_identity_valid: false,
    source_runtime_integrity_valid: false,
    source_runtime_identity: null,
    profile_validations: [],
    all_profiles_p95_at_or_below_500_ms: false,
    worst_profile_p95_ms: null,
    refusal_counts_by_profile: {},
    stdout_tail: '',
    stderr_tail: '',
    runner_error: null,
  }));

  let comparisonFailure: string | null = null;
  let artifactsBefore: Set<string> = new Set();
  const baselineVerified = baselineVerificationError === null && baseline !== null;
  if (!baselineVerified) comparisonFailure = 'baseline_reference_unverified';
  else if (sourcePreflightError !== null || benchmarkSourceSha256 === null || comparatorSourceSha256 === null) {
    comparisonFailure = 'runner_source_preflight_failed';
  }
  else if (!hostPreflightValid) comparisonFailure = 'host_identity_mismatch';
  if (baselineVerified && sourcePreflightError === null && benchmarkSourceSha256 !== null
    && comparatorSourceSha256 !== null && hostPreflightValid) {
    try {
      artifactsBefore = await listLatencyReports();
      for (const arm of arms) {
        if (interruptionSignal) {
          arm.status = 'not_started_interrupted';
          arm.interrupted = interruptionSignal;
          continue;
        }
        const elapsedBeforeArm = performance.now() - start;
        const remaining = COMPARISON_DEADLINE_MS - elapsedBeforeArm;
        if (remaining <= CHILD_CLEANUP_GRACE_MS) {
          arm.status = 'not_started_comparison_deadline';
          comparisonFailure ??= 'comparison_deadline_exceeded';
          continue;
        }
        try {
          await runArm(arm, Math.min(ARM_DEADLINE_MS, remaining - CHILD_CLEANUP_GRACE_MS), artifactsBefore, benchmarkSourceSha256);
        } catch (error) {
          arm.status = 'runner_exception';
          arm.finished_at = new Date().toISOString();
          arm.runner_error = error instanceof Error ? error.message : 'arm_runner_failed';
          try {
            const afterException = await listLatencyReports();
            for (const name of afterException) {
              if (artifactsBefore.has(name)) continue;
              const report = await loadReport(join(ARTIFACTS_DIR, name));
              if (report) arm.discovered_reports.push(report);
            }
            if (arm.discovered_reports.length === 1) arm.report = arm.discovered_reports[0]!;
          } catch {
            arm.runner_error += '; partial_report_discovery_failed';
          }
        }
        artifactsBefore = await listLatencyReports();
        if (arm.status !== 'completed') comparisonFailure ??= arm.status;
      }
    } catch (error) {
      comparisonFailure ??= error instanceof Error ? error.message : 'comparison_runner_failed';
    }
  }

  const sourceHashesAfter: { benchmark: string | null; comparator: string | null } = {
    benchmark: null,
    comparator: null,
  };
  try { sourceHashesAfter.benchmark = await hashFile(BENCHMARK_PATH); }
  catch { comparisonFailure ??= 'benchmark_source_post_hash_failed'; }
  try { sourceHashesAfter.comparator = await hashFile(fileURLToPath(import.meta.url)); }
  catch { comparisonFailure ??= 'comparator_source_post_hash_failed'; }
  const sourceFreezeStable = benchmarkSourceSha256 !== null && comparatorSourceSha256 !== null
    && sourceHashesAfter.benchmark === benchmarkSourceSha256
    && sourceHashesAfter.comparator === comparatorSourceSha256;
  if (!sourceFreezeStable) comparisonFailure ??= 'runner_source_changed_during_comparison';

  const crossArmIdentity = verifyCrossArmIdentity(arms);
  if (!crossArmIdentity.valid) comparisonFailure ??= 'cross_arm_model_runtime_or_input_identity_mismatch';
  const eligibleArms = sourceFreezeStable && crossArmIdentity.valid ? arms.filter((arm) => arm.status === 'completed'
    && arm.report_identity_valid && arm.host_identity_valid && arm.source_runtime_integrity_valid
    && arm.all_profiles_p95_at_or_below_500_ms) : [];
  const winner = selectWinner(eligibleArms);
  const allArmsComplete = arms.every((arm) => arm.status === 'completed');
  const completedAt = new Date().toISOString();
  const comparison: JsonRecord = {
    schema_version: 1,
    kind: 'm0_laya_cpu_thread_sweep_comparison',
    run_id: runId,
    plan: { path: relative(ROOT, planPath), sha256: planSha256 },
    status: interruptionSignal ? 'interrupted' : allArmsComplete ? 'completed' : 'incomplete',
    started_at: startedAt,
    finished_at: completedAt,
    elapsed_ms: performance.now() - start,
    order: [...THREAD_ARMS],
    arm_deadline_ms: ARM_DEADLINE_MS,
    comparison_deadline_ms: COMPARISON_DEADLINE_MS,
    source_freeze: {
      benchmark_before_sha256: benchmarkSourceSha256,
      benchmark_after_sha256: sourceHashesAfter.benchmark,
      comparator_before_sha256: comparatorSourceSha256,
      comparator_after_sha256: sourceHashesAfter.comparator,
      stable: sourceFreezeStable,
    },
    cross_arm_identity: crossArmIdentity,
    arms,
    selection: {
      qualifying_thread_counts: eligibleArms.map((arm) => arm.intra_op_threads),
      offline_latency_recommendation: winner?.intra_op_threads ?? null,
      winner_worst_profile_p95_ms: winner?.worst_profile_p95_ms ?? null,
      p95_target_ms: 500,
      product_runtime_intra_op_threads: 2,
      product_runtime_changed: false,
      auto_applied: false,
      reason: winner
        ? 'An offline latency arm met the frozen all-profile p95 rule; product configuration remains unchanged pending separate review.'
        : 'No complete, identity-valid arm met the frozen 500 ms p95 target for all three profiles; product configuration remains at 2 threads.',
    },
    scope: {
      offline_cpu_inference_only: true,
      real_encoder_and_head_graphs: true,
      synthetic_profiles_only: true,
      refusals_included_in_latency: true,
      model_quality_verified: false,
      business_quality_verified: false,
      formal_mvp_verified: false,
      joint_load: false,
      physical_input: false,
      gpu_execution: false,
    },
    failure_code: interruptionSignal ? `interrupted_${interruptionSignal}` : comparisonFailure,
  };

  await writeArtifactOnce(comparisonPath, comparison);
  console.log(JSON.stringify({
    kind: 'laya_cpu_thread_comparison_finished',
    status: comparison.status,
    plan: relative(ROOT, planPath),
    comparison: relative(ROOT, comparisonPath),
    recommendation: winner?.intra_op_threads ?? null,
    failure_code: comparison.failure_code,
  }));
  process.off('SIGINT', onSigint);
  process.off('SIGTERM', onSigterm);
  if (interruptionSignal) process.exitCode = interruptionSignal === 'SIGINT' ? SIGINT_CODE : SIGTERM_CODE;
  else if (comparison.status !== 'completed' || comparisonFailure !== null) process.exitCode = 1;
}

async function runArm(arm: ArmRecord, timeoutMs: number, alreadyDiscovered: Set<string>, benchmarkSourceSha256: string): Promise<void> {
  arm.status = 'running';
  arm.started_at = new Date().toISOString();
  const started = performance.now();
  const beforeReports = new Set(alreadyDiscovered);
  let stdoutTail = '';
  let stderrTail = '';
  let stdoutPending = '';
  let reportedArtifact: string | null = null;
  let spawnError: string | null = null;
  let timedOut = false;
  let armInterrupted: string | null = null;
  let terminationStarted = false;
  let forceKillTimer: NodeJS.Timeout | null = null;
  let deadlineTimer: NodeJS.Timeout | null = null;

  const child = spawn(process.execPath,
    ['--import', 'tsx', BENCHMARK_PATH, '--threads', String(arm.intra_op_threads)], {
      cwd: ROOT,
      env: safeChildEnvironment(),
      stdio: ['ignore', 'pipe', 'pipe'],
      detached: process.platform !== 'win32',
    });

  const addTail = (previous: string, chunk: string): string => {
    const combined = previous + chunk;
    return Buffer.byteLength(combined) <= MAX_CAPTURED_OUTPUT_BYTES
      ? combined
      : Buffer.from(combined).subarray(Buffer.byteLength(combined) - MAX_CAPTURED_OUTPUT_BYTES).toString('utf8');
  };

  const handleLine = (line: string): void => {
    stdoutTail = addTail(stdoutTail, `${line}\n`);
    let parsed: unknown;
    try { parsed = JSON.parse(line); }
    catch { return; }
    if (!isRecord(parsed)) return;
    if (parsed['kind'] === 'laya_cpu_scan_progress'
      && typeof parsed['profile_id'] === 'string' && typeof parsed['completed_samples'] === 'number') {
      console.log(JSON.stringify({ kind: 'laya_cpu_thread_comparison_progress',
        order_index: arm.order_index, intra_op_threads: arm.intra_op_threads,
        profile_id: parsed['profile_id'], completed_samples: parsed['completed_samples'] }));
    } else if (parsed['kind'] === 'laya_cpu_scan_finished' && typeof parsed['artifact'] === 'string') {
      reportedArtifact = parsed['artifact'];
    }
  };

  child.stdout?.setEncoding('utf8');
  child.stdout?.on('data', (chunk: string) => {
    stdoutPending += chunk;
    let newline = stdoutPending.indexOf('\n');
    while (newline >= 0) {
      handleLine(stdoutPending.slice(0, newline));
      stdoutPending = stdoutPending.slice(newline + 1);
      newline = stdoutPending.indexOf('\n');
    }
  });
  child.stderr?.setEncoding('utf8');
  child.stderr?.on('data', (chunk: string) => { stderrTail = addTail(stderrTail, chunk); });

  const stop = (kind: 'timeout' | `signal:${NodeJS.Signals}`): void => {
    if (terminationStarted) return;
    terminationStarted = true;
    if (kind === 'timeout') timedOut = true;
    else armInterrupted = kind.slice('signal:'.length);
    signalChildTree(child, 'SIGTERM');
    forceKillTimer = setTimeout(() => signalChildTree(child, 'SIGKILL'), CHILD_CLEANUP_GRACE_MS);
    forceKillTimer.unref();
  };
  stopCurrentArm = (signal) => stop(`signal:${signal}`);
  if (interruptionSignal) stopCurrentArm(interruptionSignal);
  deadlineTimer = setTimeout(() => stop('timeout'), timeoutMs);

  const exit = await new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolveExit) => {
    child.once('error', (error) => { spawnError = error.message; });
    child.once('close', (code, signal) => resolveExit({ code, signal }));
  });
  clearTimeout(deadlineTimer);
  if (forceKillTimer) clearTimeout(forceKillTimer);
  stopCurrentArm = null;
  if (stdoutPending) handleLine(stdoutPending);

  arm.exit_code = exit.code;
  arm.exit_signal = exit.signal;
  arm.timed_out = timedOut;
  arm.interrupted = armInterrupted;
  arm.elapsed_ms = performance.now() - started;
  arm.finished_at = new Date().toISOString();
  arm.stdout_tail = stdoutTail;
  arm.stderr_tail = stderrTail;
  arm.runner_error = spawnError;

  const afterReports = await listLatencyReports();
  const discoveredNames = [...afterReports].filter((name) => !beforeReports.has(name));
  const reportedName = reportedArtifact ? basename(resolve(ROOT, reportedArtifact)) : null;
  const orderedNames = reportedName && discoveredNames.includes(reportedName)
    ? [reportedName, ...discoveredNames.filter((name) => name !== reportedName)]
    : discoveredNames;
  for (const name of orderedNames) {
    const reportFile = await loadReport(join(ARTIFACTS_DIR, name));
    if (reportFile) arm.discovered_reports.push(reportFile);
  }
  if (arm.discovered_reports.length === 1) arm.report = arm.discovered_reports[0]!;
  else if (arm.discovered_reports.length > 1) arm.runner_error ??= 'multiple_new_latency_reports_for_one_arm';

  if (timedOut) arm.status = 'arm_deadline_exceeded';
  else if (armInterrupted) arm.status = 'interrupted';
  else if (spawnError) arm.status = 'runner_spawn_failed';
  else if (!arm.report) arm.status = exit.code === 0 ? 'completed_without_raw_report' : 'worker_failed_without_raw_report';
  else if (!arm.report.json_valid) arm.status = 'raw_report_invalid_json';
  else if (exit.code !== 0) arm.status = 'benchmark_nonzero_exit';
  else if (arm.report.report_status !== 'completed_latency_scan') arm.status = 'benchmark_report_incomplete';
  else arm.status = 'completed';

  if (arm.report?.json_valid) {
    const bytes = await readFile(resolve(ROOT, arm.report.path));
    const report = parseRecord(bytes);
    const validation = validateArmReport(report, arm.intra_op_threads, benchmarkSourceSha256, baselineHostIdentity);
    arm.report_identity_valid = validation.reportIdentityValid;
    arm.host_identity_valid = validation.hostIdentityValid;
    arm.source_runtime_integrity_valid = validation.sourceRuntimeIntegrityValid;
    arm.source_runtime_identity = validation.sourceRuntimeIdentity;
    arm.profile_validations = validation.profileValidations;
    arm.refusal_counts_by_profile = validation.refusalCountsByProfile;
    arm.worst_profile_p95_ms = validation.worstProfileP95Ms;
    arm.all_profiles_p95_at_or_below_500_ms = validation.allProfilesP95AtOrBelowTarget;
    if (arm.status === 'completed' && !validation.reportIdentityValid) arm.status = 'report_identity_invalid';
    else if (arm.status === 'completed' && !validation.hostIdentityValid) arm.status = 'host_identity_mismatch';
    else if (arm.status === 'completed' && !validation.sourceRuntimeIntegrityValid) arm.status = 'report_integrity_invalid';
    else if (arm.status === 'completed' && arm.profile_validations.some((profile) => !profile.valid)) {
      arm.status = 'profile_validation_failed';
    }
  }
  console.log(JSON.stringify({ kind: 'laya_cpu_thread_arm_finished', order_index: arm.order_index,
    intra_op_threads: arm.intra_op_threads, status: arm.status,
    report: arm.report?.path ?? null, report_sha256: arm.report?.sha256 ?? null,
    worst_profile_p95_ms: arm.worst_profile_p95_ms,
    all_profiles_p95_at_or_below_500_ms: arm.all_profiles_p95_at_or_below_500_ms }));
}

function validateArmReport(report: JsonRecord, threads: ThreadCount, expectedBenchmarkSha256: string | null,
                          expectedHostIdentity: JsonRecord | null): {
  reportIdentityValid: boolean;
  hostIdentityValid: boolean;
  sourceRuntimeIntegrityValid: boolean;
  sourceRuntimeIdentity: JsonRecord;
  profileValidations: ProfileValidation[];
  refusalCountsByProfile: Record<string, number>;
  worstProfileP95Ms: number | null;
  allProfilesP95AtOrBelowTarget: boolean;
} {
  const runtime = asRecord(report['runtime']);
  const policy = asRecord(report['decision_policy']);
  const scope = asRecord(report['scope']);
  const integrity = asRecord(report['source_manifest_runtime_hashes']);
  const before = asRecord(integrity['before']);
  const after = asRecord(integrity['after']);
  const beforeSources = asRecord(before['source_sha256']);
  const afterSources = asRecord(after['source_sha256']);
  const beforeNative = asRecord(before['runtime_native_sha256']);
  const afterNative = asRecord(after['runtime_native_sha256']);
  const hostValid = expectedHostIdentity !== null
    && stableJson(stableHostIdentity(report['environment'])) === stableJson(expectedHostIdentity);
  const reportIdentityValid = report['kind'] === 'm0_laya_cpu_latency_scan'
    && report['status'] === 'completed_latency_scan'
    && report['model_id'] === MODEL_ID && report['model_revision'] === MODEL_REVISION
    && report['package_manifest_sha256_before'] === MODEL_PACKAGE_MANIFEST_SHA256
    && runtime['package'] === 'onnxruntime-node'
    && runtime['required_version'] === ORT_VERSION
    && runtime['installed_package_version'] === ORT_VERSION
    && runtime['loaded_runtime_version'] === ORT_VERSION
    && runtime['execution_provider'] === EXECUTION_PROVIDER
    && runtime['intra_op_threads'] === threads
    && runtime['inter_op_threads'] === INTER_OP_THREADS
    && policy['minimum_confidence_milli'] === CONFIDENCE_THRESHOLD_MILLI
    && policy['minimum_action_probability'] === ACTION_PROBABILITY_THRESHOLD
    && report['warmup_count_per_profile'] === WARMUP_COUNT
    && report['hot_count_per_profile'] === HOT_COUNT
    && scope['offline_only'] === true && scope['provider_calls'] === false
    && scope['physical_input_executed'] === false && scope['joint_load_action_trace'] === false
    && scope['model_quality_verified'] === false && scope['business_quality_verified'] === false
    && scope['mvp_complete'] === false;
  const integrityValid = integrity['stable'] === true
    && stableJson(beforeSources) === stableJson(afterSources)
    && stableJson(beforeNative) === stableJson(afterNative)
    && before['manifest_sha256'] === MODEL_PACKAGE_MANIFEST_SHA256
    && before['runtime_package_version'] === ORT_VERSION
    && beforeSources['scripts/benchmark-laya-cpu.mts'] === expectedBenchmarkSha256;
  const sourceRuntimeIdentity: JsonRecord = {
    source_sha256: beforeSources,
    manifest_sha256: before['manifest_sha256'] ?? null,
    runtime_package_version: before['runtime_package_version'] ?? null,
    runtime_native_sha256: beforeNative,
  };

  const profiles = Array.isArray(report['profiles']) ? report['profiles'].filter(isRecord) : [];
  const byId = new Map<string, JsonRecord>();
  for (const profile of profiles) {
    if (typeof profile['profile_id'] === 'string') byId.set(profile['profile_id'], profile);
  }
  const profileValidations: ProfileValidation[] = [];
  const refusalCountsByProfile: Record<string, number> = {};
  for (const expected of PROFILE_EXPECTATIONS) {
    const profile = byId.get(expected.profile_id);
    const issues: string[] = [];
    const preflight = profile ? asRecord(profile['preflight']) : {};
    const summary = profile ? asRecord(profile['latency_summary']) : {};
    const warmup = profile && Array.isArray(profile['warmup_samples']) ? profile['warmup_samples'].filter(isRecord) : [];
    const hot = profile && Array.isArray(profile['hot_samples']) ? profile['hot_samples'].filter(isRecord) : [];
    const allSamples = [...warmup, ...hot];
    const hotBothGraphs = hot.filter((sample) => sample['latency_eligible_both_graphs'] === true
      && sample['both_graphs_ran'] === true && asRecord(sample['calls'])['encoder'] === 1
      && asRecord(sample['calls'])['head'] === 1).length;
    const hotRefusals = hot.filter((sample) => sample['backend_failure'] === 'refused').length;
    const p50 = finiteNumber(summary['p50_wall_ms']);
    const p95 = finiteNumber(summary['p95_wall_ms']);
    const maximum = finiteNumber(summary['max_wall_ms']);
    if (!profile) issues.push('profile_missing');
    if (!profile || profile['status'] !== 'completed') issues.push('profile_not_completed');
    if (!profile || profile['pinned_model_package_verified_by_backend'] !== true) issues.push('model_package_not_verified');
    if (!profile || profile['request_template_sha256'] !== expected.request_template_sha256) issues.push('request_template_hash_mismatch');
    if (preflight['sequence_sha256'] !== expected.sequence_sha256) issues.push('sequence_hash_mismatch');
    if (preflight['sequence_tokens'] !== expected.sequence_tokens) issues.push('sequence_length_mismatch');
    if (preflight['state_truncated'] !== false) issues.push('state_truncation');
    if (warmup.length !== WARMUP_COUNT || hot.length !== HOT_COUNT) issues.push('sample_count_mismatch');
    if (hotBothGraphs !== HOT_COUNT || summary['hot_latency_eligible_both_graphs'] !== HOT_COUNT) issues.push('hot_graph_coverage_mismatch');
    if (allSamples.some((sample) => sample['pre_inference_fast_return'] !== false
      || sample['timeout'] === true || sample['status'] === 'error'
      || sample['sequence_tokens'] !== expected.sequence_tokens
      || sample['backend_identity_ok'] !== true
      || (sample['backend_failure'] !== null && sample['backend_failure'] !== 'refused')
      || asRecord(sample['calls'])['encoder'] !== 1 || asRecord(sample['calls'])['head'] !== 1
      || !Array.isArray(sample['graph_errors']) || (sample['graph_errors'] as unknown[]).length !== 0)) {
      issues.push('sample_not_full_graph_inference');
    }
    if (p50 === null || p95 === null || maximum === null) issues.push('latency_summary_missing');
    refusalCountsByProfile[expected.profile_id] = hotRefusals;
    profileValidations.push({
      profile_id: expected.profile_id,
      valid: issues.length === 0,
      request_template_sha256: typeof profile?.['request_template_sha256'] === 'string' ? profile['request_template_sha256'] : null,
      sequence_sha256: typeof preflight['sequence_sha256'] === 'string' ? preflight['sequence_sha256'] : null,
      sequence_tokens: finiteNumber(preflight['sequence_tokens']),
      warmup_samples: warmup.length,
      hot_samples: hot.length,
      hot_both_graphs_count: hotBothGraphs,
      hot_refusal_count: hotRefusals,
      p50_wall_ms: p50,
      p95_wall_ms: p95,
      max_wall_ms: maximum,
      issues,
    });
  }
  const profileSetValid = profiles.length === PROFILE_EXPECTATIONS.length
    && profiles.every((profile) => PROFILE_IDS.includes(profile['profile_id'] as typeof PROFILE_IDS[number]));
  if (!profileSetValid && profileValidations.every((profile) => profile.valid)) {
    profileValidations[0]!.valid = false;
    profileValidations[0]!.issues.push('unexpected_profile_set');
  }
  const allProfilesP95AtOrBelowTarget = profileValidations.length === PROFILE_EXPECTATIONS.length
    && profileValidations.every((profile) => profile.valid && profile.p95_wall_ms !== null && profile.p95_wall_ms <= 500);
  const p95Values = profileValidations.map((profile) => profile.p95_wall_ms)
    .filter((value): value is number => value !== null);
  return {
    reportIdentityValid,
    hostIdentityValid: hostValid,
    sourceRuntimeIntegrityValid: integrityValid,
    sourceRuntimeIdentity,
    profileValidations,
    refusalCountsByProfile,
    worstProfileP95Ms: p95Values.length === PROFILE_EXPECTATIONS.length ? Math.max(...p95Values) : null,
    allProfilesP95AtOrBelowTarget,
  };
}

function verifyCrossArmIdentity(arms: ArmRecord[]): { valid: boolean; issues: string[] } {
  const issues: string[] = [];
  const validReports = arms.filter((arm) => arm.report?.json_valid);
  if (validReports.length !== THREAD_ARMS.length) issues.push('one_or_more_raw_reports_missing_or_invalid');
  const identities = validReports.map((arm) => arm.profile_validations.map((profile) => ({
    profile_id: profile.profile_id,
    request_template_sha256: profile.request_template_sha256,
    sequence_sha256: profile.sequence_sha256,
    sequence_tokens: profile.sequence_tokens,
  })));
  if (identities.length > 1 && identities.some((identity) => stableJson(identity) !== stableJson(identities[0]))) {
    issues.push('request_or_sequence_identity_differs_between_arms');
  }
  const sourceRuntimeIdentities = validReports.map((arm) => arm.source_runtime_identity);
  if (sourceRuntimeIdentities.length > 1
    && sourceRuntimeIdentities.some((identity) => identity === null
      || stableJson(identity) !== stableJson(sourceRuntimeIdentities[0]))) {
    issues.push('source_or_runtime_hashes_differ_between_arms');
  }
  if (arms.some((arm) => arm.report?.json_valid && !arm.source_runtime_integrity_valid)) {
    issues.push('source_or_runtime_integrity_failed');
  }
  if (arms.some((arm) => arm.report?.json_valid && !arm.host_identity_valid)) issues.push('host_identity_differs_from_baseline');
  return { valid: issues.length === 0, issues };
}

function selectWinner(arms: ArmRecord[]): ArmRecord | null {
  if (arms.length === 0) return null;
  return [...arms].sort((left, right) => {
    const p95Difference = (left.worst_profile_p95_ms ?? Number.POSITIVE_INFINITY)
      - (right.worst_profile_p95_ms ?? Number.POSITIVE_INFINITY);
    if (p95Difference !== 0) return p95Difference;
    if (left.intra_op_threads === 2) return -1;
    if (right.intra_op_threads === 2) return 1;
    return left.intra_op_threads - right.intra_op_threads;
  })[0]!;
}

async function listLatencyReports(): Promise<Set<string>> {
  const names = await readdir(ARTIFACTS_DIR).catch(() => [] as string[]);
  return new Set(names.filter((name) => REPORT_NAME.test(name)));
}

async function loadReport(path: string): Promise<ReportFile | null> {
  try {
    const info = await lstat(path);
    if (info.isSymbolicLink() || !info.isFile()) return null;
    const bytes = await readFile(path);
    let report: JsonRecord | null = null;
    try { report = parseRecord(bytes); }
    catch { report = null; }
    const runtime = report ? asRecord(report['runtime']) : {};
    return {
      path: relative(ROOT, path),
      sha256: sha256(bytes),
      bytes: bytes.byteLength,
      json_valid: report !== null,
      report_status: typeof report?.['status'] === 'string' ? report['status'] : null,
      report_run_id: typeof report?.['run_id'] === 'string' ? report['run_id'] : null,
      intra_op_threads: finiteNumber(runtime['intra_op_threads']),
    };
  } catch {
    return null;
  }
}

function stableHostIdentity(value: unknown): JsonRecord | null {
  if (!isRecord(value)) return null;
  const fields = ['platform', 'os_release', 'architecture', 'cpu_model', 'logical_cpu_count',
    'total_memory_bytes', 'node_version'];
  const identity: JsonRecord = {};
  for (const field of fields) identity[field] = value[field] ?? null;
  return identity;
}

function hostEnvironment(): JsonRecord {
  const cpus = os.cpus();
  return {
    platform: os.platform(),
    os_release: os.release(),
    architecture: os.arch(),
    cpu_model: cpus[0]?.model ?? 'unknown',
    logical_cpu_count: cpus.length,
    total_memory_bytes: os.totalmem(),
    load_average_at_start: os.loadavg(),
    node_version: process.version,
    hardware_power_record_reference: null,
    hardware_power_record_status: 'not_sampled',
  };
}

function safeChildEnvironment(): NodeJS.ProcessEnv {
  const allowed = ['PATH', 'TMPDIR', 'TEMP', 'TMP', 'DYLD_LIBRARY_PATH', 'DYLD_FALLBACK_LIBRARY_PATH', 'LD_LIBRARY_PATH', 'LANG'];
  const result: NodeJS.ProcessEnv = { PATH: '/usr/bin:/bin:/usr/sbin:/sbin' };
  for (const name of allowed) {
    const value = process.env[name];
    if (value !== undefined) result[name] = value;
  }
  return result;
}

function signalChildTree(child: ReturnType<typeof spawn>, signal: NodeJS.Signals): void {
  if (!child.pid) return;
  if (process.platform !== 'win32') {
    try { process.kill(-child.pid, signal); return; }
    catch { /* Fall back to the direct child if its group is already gone. */ }
  }
  try { child.kill(signal); }
  catch { /* The child may have exited between the deadline and signal. */ }
}

function parseRecord(bytes: Uint8Array): JsonRecord {
  const parsed: unknown = JSON.parse(Buffer.from(bytes).toString('utf8'));
  if (!isRecord(parsed)) throw new Error('expected_json_object');
  return parsed;
}

function asRecord(value: unknown): JsonRecord {
  return isRecord(value) ? value : {};
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function finiteNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(',')}]`;
  if (isRecord(value)) {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value) ?? 'null';
}

function sha256(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex');
}

async function hashFile(path: string): Promise<string> {
  return sha256(await readFile(path));
}

async function writeArtifactOnce(path: string, value: JsonRecord): Promise<void> {
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' });
}

function onSigint(): void { onSignal('SIGINT'); }
function onSigterm(): void { onSignal('SIGTERM'); }
