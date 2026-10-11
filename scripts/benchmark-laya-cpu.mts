#!/usr/bin/env node
// Real, offline Laya CPU latency scan. This is not an action trace or a quality gate.
import { fork } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createRequire } from 'node:module';
import { createReadStream } from 'node:fs';
import { mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import os from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

import type { DecisionRequest } from '@computer-use/desktop-api';
import type { OnnxRuntimeModule, OnnxSession, OnnxTensor } from '../packages/decision-worker/src/laya/layaBackend.js';
import type { InternalQ } from '../packages/decision-worker/src/laya/vendor/laya-ts/runtime/common.js';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REVISION = 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67';
const MODEL_DIR = join(ROOT, 'target/model-exports/laya-multilingual', REVISION);
const MANIFEST_PATH = join(MODEL_DIR, 'package-manifest.json');
const TOKENIZER_PATH = join(MODEL_DIR, 'bundle/tokenizer.json');
const EXPECTED_MANIFEST_SHA256 = 'c4e5cb4dc827f1ca76a47f3c19d88ef7cd47a53268e474b0c98df6c998399424';
const EXPECTED_TOKENIZER_SHA256 = '609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f';
const DEFAULT_THREADS = 2;
const ALLOWED_THREADS = new Set([1, 2, 4]);
const MINIMUM_CONFIDENCE_MILLI = 800;
const MAX_INPUT_TOKENS = 512;
const MAX_HEAD_TOKENS = 192;
const MAX_STATE_BYTES = 32 * 1024;
const MAX_QUESTION_BYTES = 32 * 1024;
const WARMUP_COUNT = 5;
const HOT_COUNT = 100;
const PROFILE_DEADLINE_MS = 180_000;
const TOTAL_DEADLINE_MS = 600_000;
const FINALIZATION_RESERVE_MS = 15_000;
const SAMPLE_BUDGET_MS = 30_000;
const CHILD_STOP_GRACE_MS = 1_500;
const PROFILE_SPECS = [
  { profile_id: 'short_choice_2', option_count: 2, fill_target: null, min_tokens: 250, max_tokens: 350 },
  { profile_id: 'medium_choice_2', option_count: 2, fill_target: 400, min_tokens: 370, max_tokens: 450 },
  { profile_id: 'near_512_choice_2', option_count: 2, fill_target: 475, min_tokens: 460, max_tokens: 510 },
] as const;
const SOURCE_FILES = [
  'scripts/benchmark-laya-cpu.mts',
  'packages/decision-worker/src/laya/layaBackend.ts',
  'packages/decision-worker/src/laya/vendor/laya-ts/runtime/common.js',
  'packages/decision-worker/src/laya/vendor/laya-ts/runtime/tokenizer.js',
  'packages/decision-worker/src/laya/vendor/laya-ts/runtime/common.d.ts',
  'packages/decision-worker/src/laya/vendor/laya-ts/runtime/tokenizer.d.ts',
] as const;
const SYNTHETIC_FILLER = 'synthetic';
const OUTPUT_DIR = join(ROOT, 'artifacts');
const require = createRequire(import.meta.url);

class ScanFailure extends Error {
  constructor(readonly code: string) { super(code); }
}

type Profile = {
  readonly profile_id: string;
  readonly option_count: number;
  readonly target_band_tokens: { readonly minimum: number; readonly maximum: number };
  readonly filler_count: number;
  readonly request_template: DecisionRequest;
  readonly request_template_sha256: string;
  readonly preflight: {
    readonly sequence_tokens: number;
    readonly sequence_sha256: string;
    readonly state_tokens_before_clamp: number;
    readonly state_tokens_used: number;
    readonly state_utf8_bytes: number;
    readonly question_utf8_bytes: number;
    readonly request_utf8_bytes: number;
    readonly markers: readonly number[];
    readonly option_marker_count: number;
    readonly option_tokens_per_option: number | null;
    readonly option_markers_distinct: number;
    readonly state_truncated: boolean;
    readonly pre_inference_failure_possible: false;
  };
};

type SampleTrace = {
  readonly profile_id: string;
  readonly phase: 'warmup' | 'hot';
  readonly sample_index: number;
  readonly trace_index: number;
  readonly request_sha256: string;
  readonly request_state_utf8_bytes: number;
  readonly sequence_tokens: number;
  readonly calls: { encoder: number; head: number };
  readonly graph_wall_ms: { encoder: number; head: number };
  readonly graph_errors: string[];
};

const cli = parseThreadArguments(process.argv.slice(2));
if (!cli) {
  console.error('Usage: node --import tsx scripts/benchmark-laya-cpu.mts [--threads 1|2|4]');
  process.exitCode = 2;
} else if (cli.remaining[0] === '--profile-worker') {
  if (cli.remaining.length !== 2 || !cli.explicit) {
    console.error('Invalid profile worker arguments.');
    process.exitCode = 2;
  } else {
    await runProfileWorker(cli.threads, cli.remaining[1]!);
  }
} else if (cli.remaining.length === 0) {
  await runCoordinator(cli.threads);
} else {
  console.error('Usage: node --import tsx scripts/benchmark-laya-cpu.mts [--threads 1|2|4]');
  process.exitCode = 2;
}

function parseThreadArguments(args: readonly string[]): { threads: number; explicit: boolean; remaining: string[] } | null {
  let threads = DEFAULT_THREADS;
  let explicit = false;
  const remaining: string[] = [];
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index]!;
    if (argument !== '--threads') {
      remaining.push(argument);
      continue;
    }
    const value = args[index + 1];
    if (explicit || value === undefined || !/^(1|2|4)$/.test(value) || !ALLOWED_THREADS.has(Number(value))) {
      return null;
    }
    threads = Number(value);
    explicit = true;
    index += 1;
  }
  return { threads, explicit, remaining };
}

async function runCoordinator(threads: number): Promise<void> {
  const runStarted = performance.now();
  const runId = randomUUID();
  const artifactPath = join(OUTPUT_DIR, `m0-laya-cpu-latency-${runId}.json`);
  const artifact: Record<string, unknown> = {
    schema_version: 1,
    kind: 'm0_laya_cpu_latency_scan',
    run_id: runId,
    started_at: new Date().toISOString(),
    finished_at: null,
    status: 'running',
    model_id: 'convaiinnovations/laya-multilingual',
    model_revision: REVISION,
    runtime: {
      package: 'onnxruntime-node',
      required_version: '1.30.0',
      installed_package_version: null,
      loaded_runtime_version: null,
      execution_provider: 'cpu',
      intra_op_threads: threads,
      inter_op_threads: 1,
    },
    decision_policy: {
      minimum_confidence_milli: MINIMUM_CONFIDENCE_MILLI,
      minimum_action_probability: 0.5,
    },
    warmup_count_per_profile: WARMUP_COUNT,
    hot_count_per_profile: HOT_COUNT,
    profile_deadline_ms: PROFILE_DEADLINE_MS,
    total_deadline_ms: TOTAL_DEADLINE_MS,
    profile_definitions: PROFILE_SPECS.map(({ profile_id, option_count, fill_target, min_tokens, max_tokens }) => ({
      profile_id,
      question_count: 1,
      question_kind: 'choice',
      option_count,
      target_fill_tokens: fill_target,
      accepted_sequence_token_band: { minimum: min_tokens, maximum: max_tokens },
      max_input_tokens: MAX_INPUT_TOKENS,
      max_head_tokens: MAX_HEAD_TOKENS,
    })),
    profiles: [],
    protocol_revision: 2,
    capacity_preflight: [],
    configuration_sha256: null,
    package_manifest_sha256_before: null,
    source_manifest_runtime_hashes: { before: null, after: null, stable: null },
    environment: hostEnvironment(),
    scope: {
      offline_only: true,
      provider_calls: false,
      host_or_device_started: false,
      physical_input_executed: false,
      joint_load_action_trace: false,
      static_action_trace: false,
      model_quality_verified: false,
      business_quality_verified: false,
      stop_or_observe_gate_verified: false,
      mvp_complete: false,
      power_profile_status: 'not_sampled; root may attach a separate host record reference',
    },
    resource_sample_count: 0,
    coordinator_peak_rss_bytes: null,
    failure_code: null,
    failure_phase: null,
  };

  let profiles: Profile[] = [];
  let beforeHashes: HashInventory | null = null;
  let failure: { phase: string; code: string } | null = null;
  try {
    await mkdir(OUTPUT_DIR, { recursive: true });
    artifact.failure_phase = 'source_manifest_runtime_hash_before';
    beforeHashes = await hashInventory();
    artifact.source_manifest_runtime_hashes = { before: beforeHashes, after: null, stable: null };
    artifact.package_manifest_sha256_before = beforeHashes.manifest_sha256;
    (artifact.runtime as Record<string, unknown>).installed_package_version = beforeHashes.runtime_package_version;
    if (beforeHashes.manifest_sha256 !== EXPECTED_MANIFEST_SHA256) {
      throw new ScanFailure('package_manifest_pin_mismatch');
    }
    if (beforeHashes.runtime_package_version !== '1.30.0') {
      throw new ScanFailure('onnxruntime_package_version_mismatch');
    }

    artifact.failure_phase = 'tokenizer_preflight';
    const preflightStarted = performance.now();
    profiles = await prepareProfiles((capacity) => { artifact.capacity_preflight = capacity; });
    artifact['profile_preflight_ms'] = elapsed(preflightStarted);
    artifact['profiles'] = profiles.map((profile) => ({
      profile_id: profile.profile_id,
      status: 'not_started',
      option_count: profile.option_count,
      filler_count: profile.filler_count,
      request_template_sha256: profile.request_template_sha256,
      preflight: profile.preflight,
      worker_start_ms: null,
      backend_create_ms: null,
      estimated_bundle_verification_and_setup_ms: null,
      graph_load_ms: null,
      first_decide_wall_ms: null,
      session_release_ms: null,
      worker_peak_rss_bytes: null,
      warmup_samples: [],
      hot_samples: [],
      latency_summary: null,
      failure_code: null,
    }));
    artifact.configuration_sha256 = sha256(Buffer.from(stableJson({
      model_revision: REVISION,
      runtime: artifact.runtime,
      decision_policy: artifact.decision_policy,
      warmup_count: WARMUP_COUNT,
      hot_count: HOT_COUNT,
      profiles: profiles.map(({ profile_id, option_count, filler_count, request_template_sha256, preflight }) => ({
        profile_id, option_count, filler_count, request_template_sha256, preflight,
      })),
    })));

    artifact.failure_phase = 'profile_runs';
    let globalResourceSampleIndex = 0;
    for (let index = 0; index < profiles.length; index += 1) {
      const remaining = TOTAL_DEADLINE_MS - FINALIZATION_RESERVE_MS - elapsed(runStarted);
      if (remaining <= 0) {
        failure = { phase: 'profile_runs', code: 'total_deadline_exceeded' };
        break;
      }
      const profile = profiles[index]!;
      const resultRecord = (artifact.profiles as Array<Record<string, unknown>>)[index]!;
      const deadlineBudget = Math.min(PROFILE_DEADLINE_MS, remaining);
      const workerStarted = performance.now();
        const childResult = await executeProfile(profile, deadlineBudget, threads, (event) => {
        if (event.kind === 'profile_started') {
          resultRecord['status'] = 'running';
          resultRecord['worker_start_ms'] = elapsed(workerStarted);
        } else if (event.kind === 'sample') {
          const sample = event.sample as Record<string, unknown>;
          sample['resource_sample_index'] = globalResourceSampleIndex;
          sample['trace_sample_index'] = globalResourceSampleIndex;
          globalResourceSampleIndex += 1;
          artifact.resource_sample_count = globalResourceSampleIndex;
          const phase = sample['phase'] === 'warmup' ? 'warmup_samples' : 'hot_samples';
          (resultRecord[phase] as Array<Record<string, unknown>>).push(sample);
          const running = resultRecord['running_sample'];
          if (isRecord(running) && running['phase'] === sample['phase']
            && running['sample_index'] === sample['sample_index']) delete resultRecord['running_sample'];
        } else if (event.kind === 'sample_started') {
          resultRecord['running_sample'] = event.sample;
        } else if (event.kind === 'progress') {
          console.log(JSON.stringify({ kind: 'laya_cpu_scan_progress', profile_id: profile.profile_id,
            completed_samples: event.completed, planned_samples: WARMUP_COUNT + HOT_COUNT }));
        } else if (event.kind === 'summary') {
          const summary = event.summary as Record<string, unknown>;
          Object.assign(resultRecord, summary);
          if (typeof summary['loaded_runtime_version'] === 'string') {
            (artifact.runtime as Record<string, unknown>).loaded_runtime_version = summary['loaded_runtime_version'];
          }
        } else if (event.kind === 'failure') {
          resultRecord['failure_code'] = event.code;
        }
      });
      if (childResult.deadlineCode) {
        resultRecord['status'] = childResult.deadlineCode;
        resultRecord['failure_code'] = childResult.deadlineCode;
        resultRecord['session_cleanup_status'] = 'worker_terminated; operating-system process exit releases its native handles';
        const running = resultRecord['running_sample'];
        if (isRecord(running) && (running['phase'] === 'warmup' || running['phase'] === 'hot')
          && typeof running['sample_index'] === 'number' && typeof running['trace_index'] === 'number') {
          const phaseKey = running['phase'] === 'warmup' ? 'warmup_samples' : 'hot_samples';
          const resourceSampleIndex = globalResourceSampleIndex++;
          (resultRecord[phaseKey] as Array<Record<string, unknown>>).push({
            profile_id: profile.profile_id,
            phase: running['phase'],
            sample_index: running['sample_index'],
            trace_index: running['trace_index'],
            resource_sample_index: resourceSampleIndex,
            trace_sample_index: resourceSampleIndex,
            wall_latency_ms: null,
            backend_inference_ms: null,
            backend_queue_ms: null,
            backend_failure: null,
            status: childResult.deadlineCode,
            timeout: true,
            error_code: childResult.deadlineCode,
            pre_inference_fast_return: false,
            latency_eligible_both_graphs: false,
            calls: { encoder: null, head: null },
            both_graphs_ran: null,
            graph_wall_ms: { encoder: null, head: null },
            graph_errors: [],
            rss_bytes: null,
            calls_unknown_due_to_process_termination: true,
          });
          artifact.resource_sample_count = globalResourceSampleIndex;
        }
        failure = { phase: 'profile_runs', code: childResult.deadlineCode };
      } else if (childResult.exitCode !== 0) {
        resultRecord['status'] = resultRecord['failure_code'] ? 'failed' : 'worker_failed';
        resultRecord['failure_code'] ??= 'worker_failed';
      }
      delete resultRecord['running_sample'];
      resultRecord['latency_summary'] = summarizeHot(resultRecord['hot_samples'] as Array<Record<string, unknown>>);
      if (childResult.deadlineCode) break;
    }

    const completed = (artifact.profiles as Array<Record<string, unknown>>).every((profile) =>
      profile['status'] === 'completed' &&
      (profile['warmup_samples'] as unknown[]).length === WARMUP_COUNT &&
      (profile['hot_samples'] as unknown[]).length === HOT_COUNT);
    if (!failure && completed) artifact.status = 'completed_latency_scan';
    else {
      artifact.status = 'incomplete';
      failure ??= { phase: 'profile_runs', code: 'one_or_more_profiles_incomplete' };
    }
  } catch (error) {
    failure = {
      phase: typeof artifact.failure_phase === 'string' ? artifact.failure_phase : 'scan',
      code: error instanceof ScanFailure ? error.code : 'scan_failed',
    };
    artifact.status = 'failed';
  } finally {
    try {
      const after = await hashInventory();
      const stable = beforeHashes !== null && stableJson(beforeHashes) === stableJson(after);
      artifact.source_manifest_runtime_hashes = { before: beforeHashes, after, stable };
      if (beforeHashes && !stable) {
        artifact['integrity_changed_during_scan'] = true;
        artifact.status = 'failed';
        failure ??= { phase: 'source_manifest_runtime_hash_after', code: 'source_or_runtime_hash_changed' };
      }
    } catch {
      artifact['source_manifest_runtime_hashes'] = { before: beforeHashes, after: null, stable: false };
      artifact.status = 'failed';
      failure ??= { phase: 'source_manifest_runtime_hash_after', code: 'post_scan_hash_failed' };
    }
    artifact.finished_at = new Date().toISOString();
    artifact['elapsed_ms'] = elapsed(runStarted);
    artifact['coordinator_peak_rss_bytes'] = process.resourceUsage().maxRSS * 1024;
    artifact['failure_phase'] = failure?.phase ?? null;
    artifact['failure_code'] = failure?.code ?? null;
    await writeArtifactOnce(artifactPath, artifact);
  }
  console.log(JSON.stringify({ kind: 'laya_cpu_scan_finished', status: artifact.status, artifact: relative(ROOT, artifactPath),
    failure_code: artifact.failure_code }));
  if (artifact.status !== 'completed_latency_scan') process.exitCode = 1;
}

async function prepareProfiles(recordCapacity: (capacity: Array<Record<string, unknown>>) => void): Promise<Profile[]> {
  const manifestBytes = await readFile(MANIFEST_PATH).catch(() => { throw new ScanFailure('package_manifest_unavailable'); });
  const manifest = JSON.parse(manifestBytes.toString('utf8')) as Record<string, unknown>;
  if (manifest['model_revision'] !== REVISION || sha256(manifestBytes) !== EXPECTED_MANIFEST_SHA256) {
    throw new ScanFailure('package_manifest_pin_mismatch');
  }
  const tokenizerBytes = await readFile(TOKENIZER_PATH).catch(() => { throw new ScanFailure('pinned_tokenizer_unavailable'); });
  if (sha256(tokenizerBytes) !== EXPECTED_TOKENIZER_SHA256) throw new ScanFailure('pinned_tokenizer_hash_mismatch');

  const [{ parseTokenizerJson, encodeWithData }, { buildSequence, renderOptions, serializeState }] = await Promise.all([
    import('../packages/decision-worker/src/laya/vendor/laya-ts/runtime/tokenizer.js'),
    import('../packages/decision-worker/src/laya/vendor/laya-ts/runtime/common.js'),
  ]);
  let tokenizerData: ReturnType<typeof parseTokenizerJson>;
  try { tokenizerData = parseTokenizerJson(JSON.parse(tokenizerBytes.toString('utf8'))); }
  catch { throw new ScanFailure('pinned_tokenizer_parse_failed'); }
  tokenizerBytes.fill(0);
  if (!tokenizerData || JSON.stringify(tokenizerData.ids) !== JSON.stringify({ cls: 2, sep: 1, mask: 4, pad: 0, unk: 3 })) {
    throw new ScanFailure('pinned_tokenizer_ids_mismatch');
  }
  const data = tokenizerData;
  const tokenizer = {
    clsId: data.ids.cls,
    sepId: data.ids.sep,
    maskId: data.ids.mask,
    padId: data.ids.pad,
    maskToken: data.maskToken,
    encode: (text: string): number[] => encodeWithData(data, text),
  };

  const profiles: Profile[] = [];
  recordCapacity([8, 16].map(optionCount => {
    const request = makeRequest(optionCount, `capacity_choice_${optionCount}`, []);
    const built = buildSequence(tokenizer, serializeState(request.state), {
      t: 'choice', ins: request.questions[0]!.prompt,
      crit: Object.fromEntries(request.questions[0]!.options.map(option => [option.key, option.description])),
    }, MAX_INPUT_TOKENS, MAX_HEAD_TOKENS);
    return { option_count: optionCount, request_sha256: sha256(Buffer.from(JSON.stringify(request))),
      sequence_tokens: built.ids.length, stats: built.stats, inference_executed: false,
      latency_samples: 0, supported_without_truncation: !built.stats.truncated };
  }));
  for (const spec of PROFILE_SPECS) {
    const recent: string[] = [];
    let chosen: { request: DecisionRequest; stateText: string; built: ReturnType<typeof buildSequence>; qbytes: number } | null = null;
    for (let fill = 0; fill <= 512; fill += 1) {
      const request = makeRequest(spec.option_count, spec.profile_id, recent);
      const stateText = serializeState(request.state);
      const internalQuestion: InternalQ = {
        t: 'choice',
        ins: request.questions[0]!.prompt,
        crit: Object.fromEntries(request.questions[0]!.options.map((option) => [option.key, option.description])),
      };
      const built = buildSequence(tokenizer, stateText, internalQuestion, MAX_INPUT_TOKENS, MAX_HEAD_TOKENS);
      const question = request.questions[0]!;
      const qbytes = Buffer.byteLength(question.prompt, 'utf8') + question.options.reduce((sum, option) =>
        sum + Buffer.byteLength(option.key, 'utf8') + Buffer.byteLength(option.description, 'utf8'), 0);
      const stateBytes = Buffer.byteLength(stateText, 'utf8');
      if (stateBytes > MAX_STATE_BYTES || qbytes > MAX_QUESTION_BYTES) throw new ScanFailure('profile_payload_limit_exceeded');
      const inBand = built.ids.length >= spec.min_tokens && built.ids.length <= spec.max_tokens;
      const reachedTarget = spec.fill_target === null || built.ids.length >= spec.fill_target;
      if (reachedTarget && inBand) {
        chosen = { request, stateText, built, qbytes };
        break;
      }
      if (built.stats.truncated || built.ids.length > spec.max_tokens) break;
      if (spec.fill_target === null) break;
      recent.push(SYNTHETIC_FILLER);
    }
    if (!chosen) throw new ScanFailure(`profile_preflight_out_of_band_${spec.profile_id}`);
    const { request, stateText, built, qbytes } = chosen;
    const rendered = renderOptions({ t: 'choice', ins: request.questions[0]!.prompt,
      crit: Object.fromEntries(request.questions[0]!.options.map((option) => [option.key, option.description])) });
    if (request.questions.length !== 1 || request.questions[0]!.kind !== 'choice'
      || request.questions[0]!.options.length !== spec.option_count
      || request.state.candidates.length !== spec.option_count
      || built.stats.truncated || built.ids.length < spec.min_tokens || built.ids.length > spec.max_tokens
      || built.markers.length !== spec.option_count || built.stats.options.distinct !== spec.option_count
      || built.stats.options.tokens_per_option !== null || rendered.length !== spec.option_count
      || request.questions[0]!.options.some((option) => !request.state.candidates.some((candidate) =>
        candidate.candidate_id === option.key && candidate.description === option.description
          && candidate.precondition.state === 'executable'))) {
      throw new ScanFailure(`profile_preflight_invalid_${spec.profile_id}`);
    }
    const requestJson = Buffer.from(JSON.stringify(request));
    profiles.push({
      profile_id: spec.profile_id,
      option_count: spec.option_count,
      target_band_tokens: { minimum: spec.min_tokens, maximum: spec.max_tokens },
      filler_count: request.state.recent_results.length,
      request_template: request,
      request_template_sha256: sha256(requestJson),
      preflight: {
        sequence_tokens: built.ids.length,
        sequence_sha256: sha256(Buffer.from(built.ids.join(','), 'utf8')),
        state_tokens_before_clamp: built.stats.state_tokens,
        state_tokens_used: built.stats.state_tokens_used,
        state_utf8_bytes: Buffer.byteLength(stateText, 'utf8'),
        question_utf8_bytes: qbytes,
        request_utf8_bytes: requestJson.length,
        markers: built.markers,
        option_marker_count: built.markers.length,
        option_tokens_per_option: built.stats.options.tokens_per_option,
        option_markers_distinct: built.stats.options.distinct,
        state_truncated: built.stats.truncated,
        pre_inference_failure_possible: false,
      },
    });
  }
  return profiles;
}

function makeRequest(optionCount: number, profileId: string, recentResults: string[]): DecisionRequest {
  const candidates = Array.from({ length: optionCount }, (_, index) => {
    const suffix = String(index + 1).padStart(2, '0');
    const candidateId = `c${suffix}`;
    return {
      candidate_id: candidateId,
      skill: 'ui.ensure_value',
      semantic_target: null,
      binding: null,
      parameters: {},
      effect_class: 'read_only' as const,
      precondition: { state: 'executable' as const },
      description: `Select option ${suffix}`,
    };
  });
  const options = candidates.map((candidate) => ({ key: candidate.candidate_id, description: candidate.description }));
  return {
    decision_id: `latency-template-${profileId}`,
    lane_id: 'offline-laya-latency-scan',
    request_generation: 1,
    root_task_id: `synthetic-${profileId}`,
    task_revision: 1,
    plan_revision: 1,
    root_epoch: 1,
    decision_program: 'ui.ensure_value@1',
    calibration_revision: 'latency-scan-fixed-profile',
    state: {
      root_task_id: `synthetic-${profileId}`,
      task_revision: 1,
      goal_summary: 'Synthetic choice profile.',
      subgoal: 'Select one listed option.',
      snapshot_revision: 1,
      objects: [],
      candidates,
      recent_results: [...recentResults],
      missing_fields: [],
      omitted_items: [],
      candidate_set_digest: `cs1:cpu-latency-${profileId}`,
    },
    questions: [{
      id: 'q_synthetic_choice',
      kind: 'choice',
      prompt: 'Choose one option.',
      options,
    }],
    created_ms: 0,
    result_deadline_ms: SAMPLE_BUDGET_MS,
  };
}

async function executeProfile(profile: Profile, timeoutMs: number, threads: number,
                               onEvent: (event: Record<string, unknown>) => void): Promise<{
  exitCode: number | null;
  deadlineCode: string | null;
}> {
  const encoded = Buffer.from(JSON.stringify(profile), 'utf8').toString('base64');
  let child: ReturnType<typeof fork>;
  try {
    child = fork(fileURLToPath(import.meta.url), ['--profile-worker', encoded, '--threads', String(threads)], {
      cwd: ROOT,
      silent: true,
      env: safeChildEnvironment(),
    });
  } catch {
    onEvent({ kind: 'failure', code: 'profile_worker_start_failed' });
    return { exitCode: 1, deadlineCode: 'profile_worker_start_failed' };
  }
  let deadlineCode: string | null = null;
  let exitCode: number | null = null;
  let hardKillTimer: NodeJS.Timeout | null = null;
  let closeFallbackTimer: NodeJS.Timeout | null = null;
  let settled = false;
  let resolveWait!: () => void;
  let timer!: NodeJS.Timeout;
  const finish = (code: number | null, signal: NodeJS.Signals | null): void => {
    if (settled) return;
    settled = true;
    exitCode = code;
    clearTimeout(timer);
    if (hardKillTimer) clearTimeout(hardKillTimer);
    if (closeFallbackTimer) clearTimeout(closeFallbackTimer);
    if (signal && !deadlineCode) onEvent({ kind: 'failure', code: 'profile_worker_terminated' });
    resolveWait();
  };
  timer = setTimeout(() => {
    deadlineCode = timeoutMs < PROFILE_DEADLINE_MS ? 'total_deadline_exceeded' : 'profile_deadline_exceeded';
    child.kill('SIGTERM');
    hardKillTimer = setTimeout(() => {
      child.kill('SIGKILL');
      closeFallbackTimer = setTimeout(() => finish(null, null), 500);
    }, CHILD_STOP_GRACE_MS);
    hardKillTimer.unref();
  }, timeoutMs);

  child.on('message', (message: unknown) => {
    if (isRecord(message) && typeof message['kind'] === 'string') onEvent(message);
  });
  child.on('error', () => {
    onEvent({ kind: 'failure', code: 'profile_worker_start_failed' });
    deadlineCode = 'profile_worker_start_failed';
    child.kill('SIGKILL');
    closeFallbackTimer = setTimeout(() => finish(null, null), CHILD_STOP_GRACE_MS);
  });
  child.stdout?.on('data', () => {});
  child.stderr?.on('data', () => {});
  await new Promise<void>((resolvePromise) => {
    resolveWait = resolvePromise;
    child.once('exit', finish);
    child.once('close', finish);
  });
  return { exitCode, deadlineCode };
}

async function runProfileWorker(threads: number, encoded: string): Promise<void> {
  let profile: Profile;
  try {
    if (!encoded) throw new ScanFailure('profile_worker_bootstrap_invalid');
    profile = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8')) as Profile;
  } catch {
    await sendWorkerMessage({ kind: 'failure', code: 'profile_worker_bootstrap_invalid' });
    process.disconnect();
    return;
  }

  const profileStarted = performance.now();
  let backend: import('../packages/decision-worker/src/laya/layaBackend.js').LayaDecisionBackend | null = null;
  let activeTrace: SampleTrace | null = null;
  let firstDecideWallMs: number | null = null;
  let backendCreateMs: number | null = null;
  let releaseMs: number | null = null;
  const graphLoad = { encoder_ms: 0, head_ms: 0 };
  let completedCount = 0;
  let bothGraphCallCount = 0;
  let hotBothGraphCallCount = 0;
  let failureCode: string | null = null;
  let outcome: 'completed' | 'failed' | 'incomplete' = 'incomplete';
  await sendWorkerMessage({ kind: 'profile_started', profile_id: profile.profile_id });
  try {
    const [backendModule, runtimeModule] = await Promise.all([
      import('../packages/decision-worker/src/laya/layaBackend.js'),
      import('onnxruntime-node'),
    ]);
    const actualRuntime = runtimeModule as unknown as OnnxRuntimeModule & { env?: { versions?: { node?: string } } };
    if (actualRuntime.env?.versions?.node !== '1.30.0') throw new ScanFailure('onnxruntime_version_mismatch');
    const countedRuntime: OnnxRuntimeModule = {
      Tensor: actualRuntime.Tensor,
      InferenceSession: {
        async create(path, options) {
          if (options.executionProviders.length !== 1 || options.executionProviders[0] !== 'cpu'
            || options.intraOpNumThreads !== threads || options.interOpNumThreads !== 1) {
            throw new ScanFailure('runtime_options_mismatch');
          }
          const graph = basename(path) === 'encoder.onnx' ? 'encoder' : basename(path) === 'head.onnx' ? 'head' : 'unknown';
          const started = performance.now();
          const session = await actualRuntime.InferenceSession.create(path, options);
          if (graph === 'encoder') graphLoad.encoder_ms += elapsed(started);
          if (graph === 'head') graphLoad.head_ms += elapsed(started);
          return countedSession(session, graph, () => activeTrace);
        },
      },
    };

    const createStarted = performance.now();
    backend = await backendModule.LayaDecisionBackend.create({
      bundleManifestPath: MANIFEST_PATH,
      numThreads: threads,
      minimumConfidenceMilli: MINIMUM_CONFIDENCE_MILLI,
      runtime: countedRuntime,
    });
    backendCreateMs = elapsed(createStarted);
    await sendWorkerMessage({ kind: 'summary', summary: {
      status: 'running',
      model_revision_from_backend: backend.modelRevision,
      backend_name: backend.name,
      pinned_model_package_verified_by_backend: true,
      loaded_runtime_version: actualRuntime.env?.versions?.node ?? 'unknown',
      execution_provider: 'cpu',
      intra_op_threads: threads,
      inter_op_threads: 1,
      backend_create_ms: backendCreateMs,
      estimated_bundle_verification_and_setup_ms: Math.max(0, backendCreateMs - graphLoad.encoder_ms - graphLoad.head_ms),
      graph_load_ms: { ...graphLoad, total_ms: graphLoad.encoder_ms + graphLoad.head_ms },
    } });

    const phases: Array<{ phase: 'warmup' | 'hot'; count: number }> = [
      { phase: 'warmup', count: WARMUP_COUNT },
      { phase: 'hot', count: HOT_COUNT },
    ];
    for (const { phase, count } of phases) {
      for (let sampleIndex = 0; sampleIndex < count; sampleIndex += 1) {
        const traceIndex = completedCount;
        const request = structuredClone(profile.request_template);
        request.decision_id = `latency-${profile.profile_id}-${phase}-${sampleIndex}`;
        request.request_generation = traceIndex + 1;
        request.created_ms = 0;
        request.result_deadline_ms = SAMPLE_BUDGET_MS;
        const requestJson = Buffer.from(JSON.stringify(request), 'utf8');
        const trace: SampleTrace = {
          profile_id: profile.profile_id,
          phase,
          sample_index: sampleIndex,
          trace_index: traceIndex,
          request_sha256: sha256(requestJson),
          request_state_utf8_bytes: profile.preflight.state_utf8_bytes,
          sequence_tokens: profile.preflight.sequence_tokens,
          calls: { encoder: 0, head: 0 },
          graph_wall_ms: { encoder: 0, head: 0 },
          graph_errors: [],
        };
        activeTrace = trace;
        await sendWorkerMessage({ kind: 'sample_started', sample: { phase, sample_index: sampleIndex, trace_index: traceIndex } });
        const sampleStarted = performance.now();
        let proposal: import('@computer-use/desktop-api').DecisionProposal | null = null;
        let sampleError: string | null = null;
        try {
          proposal = await backend.decide(request, new AbortController().signal);
        } catch {
          sampleError = 'decision_call_failed';
        }
        const wallLatencyMs = elapsed(sampleStarted);
        if (firstDecideWallMs === null) firstDecideWallMs = wallLatencyMs;
        activeTrace = null;
        const bothGraphsRan = trace.calls.encoder === 1 && trace.calls.head === 1;
        const failure = proposal?.failure ?? null;
        const preInferenceFastReturn = !bothGraphsRan && (failure === 'unsupported_question' || failure === 'truncated');
        const backendIdentityOk = proposal === null || (proposal.source === 's1'
          && proposal.model_revision === REVISION && proposal.backend === `laya-onnx-cpu/${REVISION}`);
        const sample = {
          profile_id: profile.profile_id,
          phase,
          sample_index: sampleIndex,
          trace_index: traceIndex,
          request_sha256: trace.request_sha256,
          request_state_utf8_bytes: trace.request_state_utf8_bytes,
          sequence_tokens: trace.sequence_tokens,
          wall_latency_ms: wallLatencyMs,
          backend_inference_ms: proposal?.inference_ms ?? null,
          backend_queue_ms: proposal?.queue_ms ?? null,
          backend_failure: failure,
          proposal_source: proposal?.source ?? null,
          proposal_model_revision: proposal?.model_revision ?? null,
          proposal_backend: proposal?.backend ?? null,
          backend_identity_ok: backendIdentityOk,
          status: sampleError ? 'error' : failure === 'refused' ? 'refused' : failure ?? 'success',
          timeout: failure === 'timeout',
          error_code: sampleError,
          pre_inference_fast_return: preInferenceFastReturn,
          latency_eligible_both_graphs: bothGraphsRan,
          calls: { ...trace.calls },
          both_graphs_ran: bothGraphsRan,
          graph_wall_ms: { ...trace.graph_wall_ms },
          graph_errors: [...trace.graph_errors],
          rss_bytes: process.memoryUsage().rss,
        };
        await sendWorkerMessage({ kind: 'sample', sample });
        completedCount += 1;
        if (bothGraphsRan) {
          bothGraphCallCount += 1;
          if (phase === 'hot') hotBothGraphCallCount += 1;
        }
        if (completedCount % 10 === 0) {
          await sendWorkerMessage({ kind: 'progress', completed: completedCount });
        }
        if (sampleError || failure === 'timeout' || preInferenceFastReturn || trace.graph_errors.length > 0
          || !bothGraphsRan || !backendIdentityOk) {
          failureCode = sampleError ? 'decision_call_failed'
            : failure === 'timeout' ? 'backend_sample_timeout'
              : preInferenceFastReturn ? 'pre_inference_profile_rejection'
                : !backendIdentityOk ? 'backend_identity_mismatch'
                  : trace.graph_errors.length > 0 ? 'onnx_graph_error' : 'both_graph_call_coverage_incomplete';
          outcome = 'incomplete';
          break;
        }
      }
      if (failureCode) break;
    }
    if (!failureCode && completedCount === WARMUP_COUNT + HOT_COUNT
      && bothGraphCallCount === WARMUP_COUNT + HOT_COUNT && hotBothGraphCallCount === HOT_COUNT) outcome = 'completed';
    else if (!failureCode) failureCode = 'sample_count_incomplete';
  } catch (error) {
    failureCode = error instanceof ScanFailure ? error.code : 'profile_backend_unavailable';
    outcome = 'failed';
    await sendWorkerMessage({ kind: 'failure', code: failureCode });
  } finally {
    activeTrace = null;
    if (backend) {
      const closeStarted = performance.now();
      try {
        await backend.close();
        releaseMs = elapsed(closeStarted);
      } catch {
        failureCode ??= 'session_release_failed';
        outcome = 'failed';
      }
    }
    const allSamples = completedCount;
    const summary = {
      status: outcome,
      completed_samples: allSamples,
      both_graph_call_count: bothGraphCallCount,
      hot_both_graph_call_count: hotBothGraphCallCount,
      warmup_completed: Math.min(allSamples, WARMUP_COUNT),
      hot_completed: Math.max(0, allSamples - WARMUP_COUNT),
      first_decide_wall_ms: firstDecideWallMs,
      session_release_ms: releaseMs,
      worker_peak_rss_bytes: process.resourceUsage().maxRSS * 1024,
      resources_released: backend !== null && releaseMs !== null,
      failure_code: failureCode,
      profile_worker_elapsed_ms: elapsed(profileStarted),
    };
    await sendWorkerMessage({ kind: 'summary', summary });
    process.disconnect();
  }
}

function countedSession(session: OnnxSession, graph: string, active: () => SampleTrace | null): OnnxSession {
  return {
    inputNames: session.inputNames,
    outputNames: session.outputNames,
    async run(feeds: Record<string, OnnxTensor>) {
      const trace = active();
      if (trace && (graph === 'encoder' || graph === 'head')) trace.calls[graph] += 1;
      const started = performance.now();
      try {
        return await session.run(feeds);
      } catch {
        if (trace) trace.graph_errors.push(`${graph}_run_failed`);
        throw new ScanFailure('onnx_graph_run_failed');
      } finally {
        if (trace && (graph === 'encoder' || graph === 'head')) trace.graph_wall_ms[graph] += elapsed(started);
      }
    },
    async release() {
      if (session.release) await session.release();
      else await session.dispose?.();
    },
  };
}

function summarizeHot(samples: Array<Record<string, unknown>>): Record<string, unknown> {
  const eligible = samples.filter((sample) => sample['latency_eligible_both_graphs'] === true
    && typeof sample['wall_latency_ms'] === 'number' && Number.isFinite(sample['wall_latency_ms']))
    .map((sample) => sample['wall_latency_ms'] as number).sort((left, right) => left - right);
  const p = (fraction: number): number | null => eligible.length
    ? eligible[Math.max(0, Math.ceil(fraction * eligible.length) - 1)]! : null;
  return {
    hot_samples_recorded: samples.length,
    hot_latency_eligible_both_graphs: eligible.length,
    p50_wall_ms: p(0.50),
    p95_wall_ms: p(0.95),
    max_wall_ms: eligible.length ? eligible[eligible.length - 1] : null,
    timeout_count: samples.filter((sample) => sample['timeout'] === true).length,
    refusal_count: samples.filter((sample) => sample['status'] === 'refused').length,
    error_count: samples.filter((sample) => sample['status'] === 'error'
      || (sample['backend_failure'] !== null && sample['backend_failure'] !== undefined
        && sample['backend_failure'] !== 'refused' && sample['backend_failure'] !== 'timeout')).length,
    non_success_failure_count: samples.filter((sample) => sample['backend_failure'] !== null
      && sample['backend_failure'] !== undefined && sample['backend_failure'] !== 'refused'
      && sample['backend_failure'] !== 'timeout').length,
    pre_inference_fast_return_count: samples.filter((sample) => sample['pre_inference_fast_return'] === true).length,
    percentile_scope: 'completed hot samples where exactly one encoder and one head call were observed; refused results remain included',
  };
}

async function hashInventory(): Promise<HashInventory> {
  const sourceHashes: Record<string, string> = {};
  for (const file of SOURCE_FILES) sourceHashes[file] = await hashFile(join(ROOT, file));
  const manifestBytes = await readFile(MANIFEST_PATH);
  const runtimePackageJson = require.resolve('onnxruntime-node/package.json');
  const runtimeRoot = dirname(runtimePackageJson);
  const runtimePackage = JSON.parse(await readFile(runtimePackageJson, 'utf8')) as { version?: string };
  const nativeDirectory = join(runtimeRoot, 'bin', 'napi-v6', process.platform, process.arch);
  const entries = await readdir(nativeDirectory).catch(() => { throw new ScanFailure('runtime_native_files_unavailable'); });
  const nativeNames = entries.filter((name) => name.endsWith('.node') || /^libonnxruntime.*\.(dylib|so(?:\.\d+)*|dll)$/.test(name)).sort();
  if (!nativeNames.some((name) => name.endsWith('.node'))) throw new ScanFailure('runtime_binding_unavailable');
  const runtimeNativeHashes: Record<string, string> = {};
  for (const name of nativeNames) {
    const path = join(nativeDirectory, name);
    const info = await stat(path).catch(() => null);
    if (!info?.isFile()) throw new ScanFailure('runtime_native_file_invalid');
    runtimeNativeHashes[relative(ROOT, path)] = await hashFile(path);
  }
  return {
    source_sha256: sourceHashes,
    manifest_sha256: sha256(manifestBytes),
    runtime_package_version: runtimePackage.version ?? 'unknown',
    runtime_native_sha256: runtimeNativeHashes,
  };
}

type HashInventory = {
  source_sha256: Record<string, string>;
  manifest_sha256: string;
  runtime_package_version: string;
  runtime_native_sha256: Record<string, string>;
};

async function hashFile(path: string): Promise<string> {
  const digest = createHash('sha256');
  const stream = createReadStream(path);
  for await (const part of stream) digest.update(part as Buffer);
  return digest.digest('hex');
}

async function writeArtifactOnce(path: string, value: Record<string, unknown>): Promise<void> {
  const bytes = `${JSON.stringify(value, null, 2)}\n`;
  await writeFile(path, bytes, { flag: 'wx' });
}

async function sendWorkerMessage(value: Record<string, unknown>): Promise<void> {
  if (!process.send) return;
  await new Promise<void>((resolvePromise) => {
    process.send!(value, () => resolvePromise());
  });
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

function hostEnvironment(): Record<string, unknown> {
  const cpu = os.cpus();
  return {
    platform: os.platform(),
    os_release: os.release(),
    architecture: os.arch(),
    cpu_model: cpu[0]?.model ?? 'unknown',
    logical_cpu_count: cpu.length,
    total_memory_bytes: os.totalmem(),
    load_average_at_start: os.loadavg(),
    node_version: process.version,
    hardware_power_record_reference: null,
    hardware_power_record_status: 'not_sampled; root may attach a separate host record reference',
  };
}

function sha256(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex');
}

function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(',')}]`;
  if (isRecord(value)) {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value) ?? 'null';
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function elapsed(start: number): number {
  return Math.max(0, performance.now() - start);
}
