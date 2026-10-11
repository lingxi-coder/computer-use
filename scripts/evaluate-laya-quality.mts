#!/usr/bin/env node
// Frozen, offline M0 quality diagnostic for the pinned local Laya choice backend.
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { createRequire } from 'node:module';
import { mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import os from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

import type { DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';
import type {
  LayaDecisionBackend,
  OnnxRuntimeModule,
  OnnxSession,
  OnnxTensor,
} from '../packages/decision-worker/src/laya/layaBackend.js';
import {
  answerConfidence,
  buildSequence,
  serializeState,
  softmax,
  type InternalQ,
} from '../packages/decision-worker/src/laya/vendor/laya-ts/runtime/common.js';
import {
  encodeWithData,
  parseTokenizerJson,
  type TokenizerData,
  type TokenizerLike,
} from '../packages/decision-worker/src/laya/vendor/laya-ts/runtime/tokenizer.js';
import {
  S1_QUALITY_CASE_COUNT,
  S1_QUALITY_CORPUS,
  fixedOptionPermutations,
  projectLayaFeatureInput,
  requestWithOptionOrder,
} from './mvp/s1-quality-corpus.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REVISION = 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67';
const MODEL_DIR = join(ROOT, 'target/model-exports/laya-multilingual', REVISION);
const MANIFEST_PATH = join(MODEL_DIR, 'package-manifest.json');
const TOKENIZER_PATH = join(MODEL_DIR, 'bundle/tokenizer.json');
const EXPECTED_MANIFEST_SHA256 = 'c4e5cb4dc827f1ca76a47f3c19d88ef7cd47a53268e474b0c98df6c998399424';
const EXPECTED_TOKENIZER_SHA256 = '609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f';
const REQUIRED_ORT_VERSION = '1.30.0';
const DEFAULT_THREADS = 2;
const ALLOWED_THREADS = new Set([1, 2, 4]);
const MINIMUM_CONFIDENCE_MILLI = 800;
const MINIMUM_ACTION_PROBABILITY = 0.5;
const MAX_INPUT_TOKENS = 512;
const MAX_HEAD_TOKENS = 192;
const MAX_STATE_BYTES = 32 * 1024;
const MAX_QUESTION_BYTES = 32 * 1024;
const SAMPLE_DEADLINE_MS = 30_000;
const BACKEND_CREATE_DEADLINE_MS = 600_000;
const TOTAL_DEADLINE_MS = 1_800_000;
const FINALIZATION_RESERVE_MS = 30_000;
const CALIBRATION_BINS = 10;
const ACCURACY_COVERAGE_THRESHOLDS_MILLI = [500, 600, 700, 800, 900, 950] as const;
const OUTPUT_DIR = join(ROOT, 'artifacts');
const require = createRequire(import.meta.url);
const SOURCE_FILES = [
  'scripts/evaluate-laya-quality.mts',
  'scripts/mvp/s1-quality-corpus.mjs',
  'scripts/mvp/s1-quality-corpus.d.mts',
  'scripts/mvp/s1-quality-corpus.test.mjs',
  'docs/architecture/m0-s1-quality-experiment.md',
  'packages/decision-worker/src/laya/layaBackend.ts',
  'packages/decision-worker/src/laya/vendor/laya-ts/runtime/common.js',
  'packages/decision-worker/src/laya/vendor/laya-ts/runtime/common.d.ts',
  'packages/decision-worker/src/laya/vendor/laya-ts/runtime/tokenizer.js',
  'packages/decision-worker/src/laya/vendor/laya-ts/runtime/tokenizer.d.ts',
  'packages/desktop-api/src/generated/envelope.ts',
  'packages/desktop-api/src/generated/schema.ts',
  'package.json',
  'package-lock.json',
] as const;

type SampleStatus = 'not_run' | 'accepted' | 'refused' | 'truncated' | 'error' | string;
type Preflight = {
  readonly status: 'ready' | 'unavailable';
  readonly error_code: string | null;
  readonly backend_budget_valid: boolean | null;
  readonly state_utf8_bytes: number | null;
  readonly question_utf8_bytes: number | null;
  readonly sequence_token_count: number | null;
  readonly state_tokens_before_clamp: number | null;
  readonly state_tokens_used: number | null;
  readonly state_tokens_dropped: number | null;
  readonly sequence_feed_sha256: string | null;
  readonly markers: readonly number[] | null;
  readonly option_marker_count: number | null;
  readonly option_marker_distinct_count: number | null;
  readonly option_tokens_per_option: number | null;
  readonly state_truncated: boolean | null;
  readonly rejection_reasons: readonly string[];
};
type PlannedSample = {
  readonly sample_id: string;
  readonly case_id: string;
  readonly stratum: string;
  readonly permutation_id: string;
  readonly permutation_order: readonly number[];
  readonly option_keys: readonly string[];
  readonly request_sha256: string;
  readonly laya_feature_sha256: string | null;
  readonly request: DecisionRequest;
  readonly preflight: Preflight;
};
type RawTensor = { readonly data: number[] | null; readonly dims: readonly number[] | null; readonly type: string | null };
type HeadCapture = { readonly logits: RawTensor; readonly action_logits: RawTensor };
type SampleTrace = {
  readonly planned: PlannedSample;
  started_at: string;
  wall_ms: number | null;
  backend_inference_ms: number | null;
  backend_queue_ms: number | null;
  backend_failure: string | null;
  backend_proposal: DecisionProposal | null;
  backend_error: { name: string; message: string } | null;
  status: SampleStatus;
  decision_call_made: boolean;
  real_inference_observed: boolean;
  calls: { encoder: number; head: number };
  graph_wall_ms: { encoder: number; head: number };
  graph_errors: Array<{ graph: string; name: string; message: string }>;
  head_capture: HeadCapture | null;
  raw_logits: number[] | null;
  raw_action_logits: number[] | null;
  probabilities: number[] | null;
  action_probabilities: number[] | null;
  predicted_candidate_id: string | null;
  max_probability: number | null;
  answer_confidence_milli: number | null;
  tie_for_maximum: boolean | null;
  rss_bytes: number | null;
};
type HashInventory = {
  readonly source_sha256: Record<string, string>;
  readonly model_manifest_sha256: string;
  readonly model_id: string | null;
  readonly model_revision: string | null;
  readonly model_package_files: Record<string, unknown> | null;
  readonly runtime_package_version: string;
  readonly runtime_package_sha256: string;
  readonly runtime_native_sha256: Record<string, string>;
};
type EvaluationRow = {
  readonly sample: SampleTrace;
  readonly expected_candidate_id: string;
  readonly correct: boolean;
};
type CalibrationMetrics = {
  readonly raw_output_cases: number;
  readonly complete_canonical_output: boolean;
  readonly expected_calibration_error: number | null;
  readonly multiclass_brier_score: number | null;
  readonly top_choice_accuracy_over_raw_output_cases: number | null;
  readonly empty_raw_output_denominator: boolean;
  readonly bins: Array<{
    bin: number;
    lower_inclusive: number;
    upper_exclusive: number;
    includes_upper_bound: boolean;
    count: number;
    mean_confidence: number | null;
    accuracy: number | null;
  }>;
};
type PermutationMetrics = {
  readonly planned_cases: number;
  readonly complete_cases: number;
  readonly flipped_cases: number;
  readonly case_flip_rate: number | null;
  readonly stability: number | null;
  readonly valid_canonical_to_permutation_comparisons: number;
  readonly flipped_comparisons: number;
  readonly pair_flip_rate: number | null;
  readonly zero_denominator: boolean;
  readonly by_case: Array<Record<string, unknown>>;
};

const threads = parseThreads(process.argv.slice(2));
if (threads === null) {
  console.error('Usage: node --import tsx scripts/evaluate-laya-quality.mts [--threads 1|2|4]');
  process.exitCode = 2;
} else {
  await runExperiment(threads);
}

function parseThreads(args: readonly string[]): number | null {
  if (args.length === 0) return DEFAULT_THREADS;
  if (args.length !== 2 || args[0] !== '--threads' || !/^(1|2|4)$/.test(args[1] ?? '')) return null;
  const parsed = Number(args[1]);
  return ALLOWED_THREADS.has(parsed) ? parsed : null;
}

async function runExperiment(numThreads: number): Promise<void> {
  const runId = randomUUID();
  const planPath = join(OUTPUT_DIR, `m0-laya-s1-quality-plan-${runId}.json`);
  const resultPath = join(OUTPUT_DIR, `m0-laya-s1-quality-result-${runId}.json`);
  const runStartedAt = new Date().toISOString();
  const runStarted = performance.now();
  let backend: LayaDecisionBackend | null = null;
  let activeTrace: SampleTrace | null = null;
  let sourceBefore: HashInventory | null = null;
  let sourceBeforeError: string | null = null;
  let sourceAfter: HashInventory | null = null;
  let sourceAfterError: string | null = null;
  let tokenizer: TokenizerLike | null = null;
  let tokenizerError: string | null = null;
  let manifestPinMatches = false;
  let runtimeVersion: string | null = null;
  let backendCreateMs: number | null = null;
  let sessionReleaseMs: number | null = null;
  let backendCreateError: string | null = null;
  let sessionReleaseError: string | null = null;
  let modelPackageVerifiedByBackend = false;
  let plannedSamples: PlannedSample[] = [];
  let samples: SampleTrace[] = [];
  let planSha256: string | null = null;
  let planSaved = false;
  let resultSaved = false;
  let status = 'not_run';

  await mkdir(OUTPUT_DIR, { recursive: true });
  try {
    try { sourceBefore = await hashInventory(); }
    catch (error) { sourceBeforeError = errorMessage(error); }
    manifestPinMatches = sourceBefore !== null
      && sourceBefore.model_manifest_sha256 === EXPECTED_MANIFEST_SHA256
      && sourceBefore.model_revision === REVISION;
    try { tokenizer = await loadPinnedTokenizer(); }
    catch (error) { tokenizerError = errorMessage(error); }

    plannedSamples = buildPlanSamples(tokenizer, tokenizerError);
    samples = plannedSamples.map(makeEmptyTrace);
    const plan = makePlanArtifact({
      runId,
      resultPath,
      numThreads,
      plannedSamples,
      sourceBefore,
      sourceBeforeError,
      manifestPinMatches,
      tokenizerError,
      startedAt: new Date().toISOString(),
    });
    planSha256 = sha256(Buffer.from(stableJson(plan), 'utf8'));
    await writeExclusive(planPath, { ...plan, plan_sha256: planSha256 });
    planSaved = true;

    const prerequisitesReady = sourceBefore !== null && manifestPinMatches && tokenizer !== null
      && sourceBefore.runtime_package_version === REQUIRED_ORT_VERSION;
    if (!prerequisitesReady) {
      status = 'not_run_prerequisite_unavailable';
      for (const sample of samples) sample.backend_error = {
        name: 'PrerequisiteUnavailable',
        message: sourceBeforeError ?? tokenizerError ?? 'Pinned model or ONNX Runtime identity did not match.',
      };
    } else {
      const runtimeModule = await import('onnxruntime-node');
      const actualRuntime = runtimeModule as unknown as OnnxRuntimeModule & { env?: { versions?: { node?: string } } };
      runtimeVersion = actualRuntime.env?.versions?.node ?? null;
      if (runtimeVersion !== REQUIRED_ORT_VERSION) {
        status = 'not_run_runtime_version_mismatch';
        backendCreateError = 'onnxruntime_version_mismatch';
        setNotRunReason(samples, 'RuntimeVersionMismatch', backendCreateError);
      } else {
        const countedRuntime = createCountingRuntime(actualRuntime, () => activeTrace, numThreads);
        const createController = new AbortController();
        const createTimer = setTimeout(() => createController.abort(), BACKEND_CREATE_DEADLINE_MS);
        const createStarted = performance.now();
        console.log(JSON.stringify({ kind: 'laya_s1_quality_backend_create_started', run_id: runId, intra_op_threads: numThreads }));
        try {
          const backendModule = await import('../packages/decision-worker/src/laya/layaBackend.js');
          backend = await backendModule.LayaDecisionBackend.create({
            bundleManifestPath: MANIFEST_PATH,
            numThreads,
            minimumConfidenceMilli: MINIMUM_CONFIDENCE_MILLI,
            runtime: countedRuntime,
            signal: createController.signal,
          });
          backendCreateMs = elapsed(createStarted);
          modelPackageVerifiedByBackend = true;
          status = 'running';
        } catch (error) {
          backendCreateMs = elapsed(createStarted);
          backendCreateError = errorMessage(error);
          status = 'not_run_backend_unavailable';
          setNotRunReason(samples, 'BackendUnavailable', backendCreateError);
        } finally {
          clearTimeout(createTimer);
        }
      }

      if (backend) {
        for (let index = 0; index < samples.length; index += 1) {
          if (elapsed(runStarted) >= TOTAL_DEADLINE_MS - FINALIZATION_RESERVE_MS) {
            for (const notStarted of samples.slice(index)) {
              notStarted.backend_error = { name: 'TotalDeadline', message: 'Sample was retained but not run before the frozen total deadline.' };
            }
            status = 'partial_total_deadline';
            break;
          }
          const trace = samples[index]!;
          activeTrace = trace;
          trace.started_at = new Date().toISOString();
          trace.decision_call_made = true;
          const started = performance.now();
          try {
            const proposal = await backend.decide(trace.planned.request, new AbortController().signal);
            trace.backend_proposal = proposal;
            trace.backend_failure = proposal.failure ?? null;
            trace.backend_inference_ms = proposal.inference_ms;
            trace.backend_queue_ms = proposal.queue_ms;
            trace.status = trace.backend_failure === null ? 'accepted' : trace.backend_failure;
          } catch (error) {
            trace.backend_error = { name: errorName(error), message: errorMessage(error) };
            trace.status = 'error';
          } finally {
            trace.wall_ms = elapsed(started);
            trace.real_inference_observed = trace.calls.encoder === 1 && trace.calls.head === 1
              && trace.head_capture?.logits.data != null && trace.head_capture?.action_logits.data != null;
            deriveHeadMetrics(trace);
            trace.rss_bytes = process.memoryUsage().rss;
            activeTrace = null;
          }
          if ((index + 1) % 10 === 0) {
            console.log(JSON.stringify({ kind: 'laya_s1_quality_progress', completed: index + 1, total: samples.length }));
          }
        }
        if (status === 'running') status = samples.every((sample) => sample.decision_call_made)
          ? 'completed_all_planned_samples' : 'partial';
      }
    }
  } catch (error) {
    status = 'failed';
    backendCreateError ??= errorMessage(error);
    setNotRunReason(samples, 'RunnerError', backendCreateError);
  } finally {
    activeTrace = null;
    if (backend) {
      const releaseStarted = performance.now();
      try { await backend.close(); }
      catch (error) { sessionReleaseError = errorMessage(error); }
      sessionReleaseMs = elapsed(releaseStarted);
    }
    try { sourceAfter = await hashInventory(); }
    catch (error) { sourceAfterError = errorMessage(error); }

    if (!planSaved) {
      const fallbackSamples = plannedSamples.length > 0 ? plannedSamples : buildPlanSamples(null, 'plan_preparation_failed');
      plannedSamples = fallbackSamples;
      samples = fallbackSamples.map(makeEmptyTrace);
      const fallbackPlan = makePlanArtifact({
        runId,
        resultPath,
        numThreads,
        plannedSamples: fallbackSamples,
        sourceBefore,
        sourceBeforeError,
        manifestPinMatches,
        tokenizerError: tokenizerError ?? 'plan_preparation_failed',
        startedAt: new Date().toISOString(),
      });
      planSha256 = sha256(Buffer.from(stableJson(fallbackPlan), 'utf8'));
      try {
        await writeExclusive(planPath, { ...fallbackPlan, plan_sha256: planSha256 });
        planSaved = true;
      } catch (error) {
        status = 'failed_plan_artifact_write';
        backendCreateError ??= errorMessage(error);
      }
    }

    const sourceStable = sourceBefore !== null && sourceAfter !== null
      ? stableJson(sourceBefore) === stableJson(sourceAfter) : null;
    if (sourceStable === false && status === 'completed_all_planned_samples') status = 'source_changed_during_experiment';
    if (sourceAfter === null && status === 'completed_all_planned_samples') status = 'source_hash_after_unavailable';
    const evaluation = evaluateSamples(samples, S1_QUALITY_CORPUS.reference_labels);
    const qualityScreen = sourceStable === true ? evaluation.qualityScreen : {
      ...evaluation.qualityScreen,
      status: 'not_assessable',
      reason: 'source/runtime hashes are missing or changed across the experiment',
    };
    if (sessionReleaseError && status === 'completed_all_planned_samples') status = 'session_release_failed';
    const resultArtifact = {
      schema_version: 1,
      kind: 'm0_laya_s1_offline_quality_result',
      run_id: runId,
      status,
      started_at: runStartedAt,
      finished_at: new Date().toISOString(),
      elapsed_ms: elapsed(runStarted),
      plan_artifact: relative(ROOT, planPath),
      plan_sha256: planSha256,
      result_artifact_is_exclusive: true,
      model: {
        model_id: 'convaiinnovations/laya-multilingual',
        model_revision: REVISION,
        arm: 'pinned local base ONNX Laya; no teacher, fine-tuned, or INT8 substitution',
        precision_label: 'base FP32 candidate; package manifest does not declare precision explicitly',
        package_verified_by_laya_backend_before_inference: modelPackageVerifiedByBackend,
        package_manifest_sha256_before: sourceBefore?.model_manifest_sha256 ?? null,
      },
      runtime: {
        package: 'onnxruntime-node',
        required_version: REQUIRED_ORT_VERSION,
        package_version_before: sourceBefore?.runtime_package_version ?? null,
        loaded_version: runtimeVersion,
        execution_provider: 'cpu',
        intra_op_threads: numThreads,
        inter_op_threads: 1,
      },
      production_policy: {
        minimum_confidence_milli: MINIMUM_CONFIDENCE_MILLI,
        minimum_action_probability: MINIMUM_ACTION_PROBABILITY,
        production_thresholds_unchanged: true,
        product_default_threads_unchanged: 2,
        offline_thread_override_only: true,
      },
      backend_setup: {
        elapsed_ms: backendCreateMs,
        session_release_ms: sessionReleaseMs,
        error: backendCreateError,
        release_error: sessionReleaseError,
      },
      preflight_distribution: summarizePreflight(plannedSamples),
      sample_count: {
        planned: plannedSamples.length,
        decision_calls_made: samples.filter((sample) => sample.decision_call_made).length,
        real_encoder_and_head_observed: samples.filter((sample) => sample.real_inference_observed).length,
        accepted: samples.filter((sample) => sample.status === 'accepted').length,
        refused: samples.filter((sample) => sample.status === 'refused').length,
        truncated: samples.filter((sample) => sample.status === 'truncated').length,
        error: samples.filter((sample) => sample.status === 'error').length,
        backend_failure_other_than_refusal_or_truncation: samples.filter((sample) => sample.backend_failure !== null
          && sample.backend_failure !== 'refused' && sample.backend_failure !== 'truncated').length,
        not_run: samples.filter((sample) => !sample.decision_call_made).length,
      },
      metrics: evaluation.metrics,
      quality_screen: qualityScreen,
      reference_evaluation: evaluation.rows.map(({ sample, expected_candidate_id, correct }) => ({
        sample_id: sample.planned.sample_id,
        case_id: sample.planned.case_id,
        stratum: sample.planned.stratum,
        permutation_id: sample.planned.permutation_id,
        expected_candidate_id,
        predicted_candidate_id: sample.predicted_candidate_id,
        correct,
        proposal_accepted: sample.status === 'accepted',
      })),
      samples: samples.map(serializeSample),
      alternate_arms: {
        cloud_teacher: { status: 'not_run', reason: 'This offline segment makes no provider calls; teacher labels are not treated as ground truth.' },
        fine_tuned_laya: { status: 'not_run', reason: 'No pinned fine-tuned artifact was supplied for this experiment.' },
        int8_laya: { status: 'not_run', reason: 'No pinned INT8 artifact was supplied; no substitute or conversion is performed.' },
        score_first_position_bias: { status: 'not_run', reason: 'The current production backend accepts choice questions only; score questions are not synthesized.' },
      },
      source_runtime_hashes: {
        before: sourceBefore,
        before_error: sourceBeforeError,
        after: sourceAfter,
        after_error: sourceAfterError,
        stable: sourceStable,
      },
      evidence_boundary: {
        offline_only: true,
        provider_calls: false,
        physical_input: false,
        head_capture_uses_same_inference_call: true,
        second_inference_for_probabilities: false,
        synthetic_or_preflight_sample_counted_as_inference: false,
        m5_knowledge_release_created: false,
        task_quality_verified: false,
        product_promotion_applied: false,
        mvp_complete: false,
        limitations: [
          'Twenty manually authored cases are a diagnostic corpus, not a representative product-quality estimate.',
          'A missing accepted denominator is reported as null conditional accuracy, never as 100 percent.',
          'Teacher, fine-tuned, INT8, score-question, and live-task quality evidence remain unavailable.',
          'The manifest pins model files but does not itself declare their numeric precision.',
        ],
      },
    };
    try {
      await writeExclusive(resultPath, resultArtifact);
      resultSaved = true;
    } catch (error) {
      status = 'failed_result_artifact_write';
      console.error(JSON.stringify({ kind: 'laya_s1_quality_result_write_failed', run_id: runId, error: errorMessage(error) }));
    }
  }

  console.log(JSON.stringify({
    kind: 'laya_s1_quality_finished',
    run_id: runId,
    status,
    plan_artifact: relative(ROOT, planPath),
    plan_saved: planSaved,
    result_artifact: resultSaved ? relative(ROOT, resultPath) : null,
  }));
  if (!resultSaved || status !== 'completed_all_planned_samples') {
    process.exitCode = 1;
  }
}

function buildPlanSamples(pinnedTokenizer: TokenizerLike | null, tokenizerLoadError: string | null): PlannedSample[] {
  const samples: PlannedSample[] = [];
  for (const qualityCase of S1_QUALITY_CORPUS.model_cases) {
    const optionCount = qualityCase.request.questions[0]!.options.length;
    const permutations = fixedOptionPermutations(optionCount);
    permutations.forEach((order, index) => {
      const permutationId = index === 0 ? 'canonical' : `fixed-${index}`;
      const request = requestWithOptionOrder(qualityCase.request, order);
      const requestHash = sha256(Buffer.from(stableJson(request), 'utf8'));
      let preflight: Preflight;
      if (!pinnedTokenizer) {
        preflight = unavailablePreflight(tokenizerLoadError ?? 'tokenizer_unavailable');
      } else {
        preflight = preflightRequest(request, pinnedTokenizer);
      }
      samples.push({
        sample_id: `${qualityCase.case_id}:${permutationId}`,
        case_id: qualityCase.case_id,
        stratum: qualityCase.stratum,
        permutation_id: permutationId,
        permutation_order: [...order],
        option_keys: request.questions[0]!.options.map((option) => option.key),
        request_sha256: requestHash,
        laya_feature_sha256: preflight.sequence_feed_sha256,
        request,
        preflight,
      });
    });
  }
  return samples;
}

function unavailablePreflight(errorCode: string): Preflight {
  return {
    status: 'unavailable', error_code: errorCode, backend_budget_valid: null,
    state_utf8_bytes: null, question_utf8_bytes: null, sequence_token_count: null,
    state_tokens_before_clamp: null, state_tokens_used: null, state_tokens_dropped: null,
    sequence_feed_sha256: null,
    markers: null, option_marker_count: null, option_marker_distinct_count: null,
    option_tokens_per_option: null, state_truncated: null, rejection_reasons: [errorCode],
  };
}

function preflightRequest(request: DecisionRequest, tokenizer: TokenizerLike): Preflight {
  const question = request.questions[0]!;
  const input = projectLayaFeatureInput(request);
  const stateText = serializeState(input.state);
  const stateBytes = Buffer.byteLength(stateText, 'utf8');
  const questionBytes = Buffer.byteLength(question.prompt, 'utf8') + question.options.reduce((sum, option) =>
    sum + Buffer.byteLength(option.key, 'utf8') + Buffer.byteLength(option.description, 'utf8'), 0);
  const internalQuestion: InternalQ = {
    t: input.question.t,
    ins: input.question.ins,
    crit: input.question.crit,
  };
  try {
    const sequence = buildSequence(tokenizer, stateText, internalQuestion, MAX_INPUT_TOKENS, MAX_HEAD_TOKENS);
    const reasons: string[] = [];
    if (stateBytes > MAX_STATE_BYTES) reasons.push('state_over_32_kib');
    if (questionBytes > MAX_QUESTION_BYTES) reasons.push('question_over_32_kib');
    if (sequence.stats.truncated) reasons.push('state_tokens_truncated');
    if (sequence.markers.length !== question.options.length || sequence.markers.length < 2 || sequence.markers.length > 256) {
      reasons.push('option_marker_count_out_of_range');
    }
    if (sequence.stats.options.distinct !== question.options.length) reasons.push('option_markers_collapsed');
    if (sequence.stats.options.tokens_per_option !== null) reasons.push('options_share_reduced_token_budget');
    return {
      status: 'ready', error_code: null, backend_budget_valid: reasons.length === 0,
      state_utf8_bytes: stateBytes, question_utf8_bytes: questionBytes,
      sequence_token_count: sequence.ids.length,
      state_tokens_before_clamp: sequence.stats.state_tokens,
      state_tokens_used: sequence.stats.state_tokens_used,
      state_tokens_dropped: sequence.stats.state_tokens_dropped,
      sequence_feed_sha256: sha256(Buffer.from(stableJson({
        input_ids: sequence.ids,
        marker_positions: sequence.markers,
        qtype: 0,
        attention_mask: Array.from(sequence.ids, () => 1),
      }), 'utf8')),
      markers: sequence.markers, option_marker_count: sequence.markers.length,
      option_marker_distinct_count: sequence.stats.options.distinct,
      option_tokens_per_option: sequence.stats.options.tokens_per_option,
      state_truncated: sequence.stats.truncated, rejection_reasons: reasons,
    };
  } catch (error) {
    return unavailablePreflight(errorMessage(error));
  }
}

function makePlanArtifact(input: {
  runId: string;
  resultPath: string;
  numThreads: number;
  plannedSamples: readonly PlannedSample[];
  sourceBefore: HashInventory | null;
  sourceBeforeError: string | null;
  manifestPinMatches: boolean;
  tokenizerError: string | null;
  startedAt: string;
}): Record<string, unknown> {
  const labelsHash = sha256(Buffer.from(stableJson(S1_QUALITY_CORPUS.reference_labels), 'utf8'));
  const modelCasesHash = sha256(Buffer.from(stableJson(S1_QUALITY_CORPUS.model_cases), 'utf8'));
  return {
    schema_version: 1,
    kind: 'm0_laya_s1_offline_quality_plan',
    run_id: input.runId,
    reserved_result_artifact: relative(ROOT, input.resultPath),
    frozen_at: input.startedAt,
    status: 'frozen_before_any_inference',
    rules: {
      corpus_version: S1_QUALITY_CORPUS.version,
      case_count: S1_QUALITY_CASE_COUNT,
      strata: { navigation: 5, disambiguation: 5, recovery: 5, bilingual: 5 },
      model_case_payload_sha256: modelCasesHash,
      reference_label_sidecar_sha256: labelsHash,
      labels_in_decision_request: false,
      case_ids_or_expected_choices_in_laya_features: false,
      per_case_permutation_order: 'canonical, reverse, one-position rotation, half rotation; exact duplicate orders removed in that order',
      sample_policy: 'retain every planned case and permutation; errors, refusal, truncation, and not-run status stay in the report',
      warmups: 0,
      preflight_counts_as_inference: false,
      minimum_confidence_milli: MINIMUM_CONFIDENCE_MILLI,
      minimum_action_probability: MINIMUM_ACTION_PROBABILITY,
      accuracy_coverage_confidence_thresholds_milli: ACCURACY_COVERAGE_THRESHOLDS_MILLI,
      action_probability_floor_for_all_curve_points: MINIMUM_ACTION_PROBABILITY,
      calibration: {
        confidence: 'maximum softmax probability from the captured real head logits',
        correctness: 'top candidate equals the separate manual reference label',
        bins: CALIBRATION_BINS,
        binning: 'equal-width bins on [0, 1], with confidence 1 assigned to the last bin',
        brier: 'mean per-case sum of squared multiclass probability errors',
        include_refused_samples: true,
      },
      permutation: {
        reference_order: 'canonical',
        compare: 'argmax candidate ids from captured raw head outputs, including refused proposals',
        case_denominator: 'cases with real logits for canonical and every unique planned permutation',
        zero_denominator: 'metric null and not assessable',
      },
      diagnostic_screen: {
        purpose: 'predeclared diagnostic only; never a product promotion gate',
        requires_all_20_canonical_real_head_outputs: true,
        minimum_production_policy_coverage: 0.8,
        minimum_accepted_conditional_accuracy: 0.95,
        minimum_each_stratum_production_policy_coverage: 0.6,
        minimum_each_stratum_accepted_conditional_accuracy: 0.8,
        maximum_ece: 0.1,
        minimum_permutation_stability: 0.95,
        requires_all_20_cases_complete_for_permutation_screen: true,
        empty_accepted_denominator: 'conditional accuracy null; screen fails',
      },
      budgets: {
        maximum_state_utf8_bytes: MAX_STATE_BYTES,
        maximum_question_utf8_bytes: MAX_QUESTION_BYTES,
        maximum_input_tokens: MAX_INPUT_TOKENS,
        maximum_head_tokens: MAX_HEAD_TOKENS,
        per_decision_deadline_ms: SAMPLE_DEADLINE_MS,
        backend_create_deadline_ms: BACKEND_CREATE_DEADLINE_MS,
        total_deadline_ms: TOTAL_DEADLINE_MS,
      },
      model_arms: {
        local_base: 'planned: pinned local multilingual base ONNX package',
        cloud_teacher: 'not_run: no provider call in this segment',
        fine_tuned_laya: 'not_run: no pinned fine-tuned artifact supplied',
        int8_laya: 'not_run: no pinned INT8 artifact supplied',
        score_first_position_bias: 'not_run: backend only supports choice questions',
        precision_note: 'The pinned package manifest contains no explicit precision field; this experiment does not independently certify FP32 dtype.',
      },
      inference_threads: { intra_op: input.numThreads, inter_op: 1, product_default_unchanged: 2 },
    },
    prerequisite_snapshot: {
      source_runtime_hashes_before: input.sourceBefore,
      source_runtime_hash_error: input.sourceBeforeError,
      pinned_manifest_matches: input.manifestPinMatches,
      tokenizer_error: input.tokenizerError,
    },
    sample_matrix: input.plannedSamples.map((sample) => ({
      sample_id: sample.sample_id,
      case_id: sample.case_id,
      stratum: sample.stratum,
      permutation_id: sample.permutation_id,
      permutation_order: sample.permutation_order,
      option_keys: sample.option_keys,
      request_sha256: sample.request_sha256,
      laya_feature_sha256: sample.laya_feature_sha256,
      request: sample.request,
      preflight: sample.preflight,
      inference_started: false,
    })),
    environment: {
      platform: process.platform,
      architecture: process.arch,
      node_version: process.version,
      cpu_model: os.cpus()[0]?.model ?? 'unknown',
      logical_cpu_count: os.cpus().length,
    },
  };
}

function makeEmptyTrace(planned: PlannedSample): SampleTrace {
  return {
    planned,
    started_at: '',
    wall_ms: null,
    backend_inference_ms: null,
    backend_queue_ms: null,
    backend_failure: null,
    backend_proposal: null,
    backend_error: null,
    status: 'not_run',
    decision_call_made: false,
    real_inference_observed: false,
    calls: { encoder: 0, head: 0 },
    graph_wall_ms: { encoder: 0, head: 0 },
    graph_errors: [],
    head_capture: null,
    raw_logits: null,
    raw_action_logits: null,
    probabilities: null,
    action_probabilities: null,
    predicted_candidate_id: null,
    max_probability: null,
    answer_confidence_milli: null,
    tie_for_maximum: null,
    rss_bytes: null,
  };
}

function setNotRunReason(samples: SampleTrace[], name: string, message: string): void {
  for (const sample of samples) {
    if (sample.decision_call_made) continue;
    sample.backend_error = { name, message };
  }
}

function createCountingRuntime(
  actualRuntime: OnnxRuntimeModule,
  active: () => SampleTrace | null,
  numThreads: number,
): OnnxRuntimeModule {
  return {
    Tensor: actualRuntime.Tensor,
    InferenceSession: {
      async create(path, options) {
        if (options.executionProviders.length !== 1 || options.executionProviders[0] !== 'cpu'
          || options.intraOpNumThreads !== numThreads || options.interOpNumThreads !== 1) {
          throw new Error('runtime_options_mismatch');
        }
        const graph = basename(path) === 'encoder.onnx' ? 'encoder'
          : basename(path) === 'head.onnx' ? 'head' : 'unknown';
        const session = await actualRuntime.InferenceSession.create(path, options);
        return countedSession(session, graph, active);
      },
    },
  };
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
        const result = await session.run(feeds);
        if (trace && graph === 'head') {
          trace.head_capture = {
            logits: copyFloatTensor(result['logits']),
            action_logits: copyFloatTensor(result['act_logits']),
          };
        }
        return result;
      } catch (error) {
        if (trace) trace.graph_errors.push({ graph, name: errorName(error), message: errorMessage(error) });
        throw error;
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

function copyFloatTensor(tensor: OnnxTensor | undefined): RawTensor {
  if (!tensor) return { data: null, dims: null, type: null };
  return {
    data: tensor.data instanceof Float32Array ? Array.from(tensor.data) : null,
    dims: [...tensor.dims],
    type: tensor.type ?? null,
  };
}

function deriveHeadMetrics(trace: SampleTrace): void {
  const capture = trace.head_capture;
  if (!capture || !capture.logits.data || !capture.action_logits.data) return;
  if (capture.logits.type !== null && capture.logits.type !== 'float32') return;
  if (capture.action_logits.type !== null && capture.action_logits.type !== 'float32') return;
  if (!sameShape(capture.logits.dims, [1, trace.planned.option_keys.length])
    || !sameShape(capture.action_logits.dims, [1, 2])) return;
  const logits = capture.logits.data;
  const actionLogits = capture.action_logits.data;
  if (logits.length !== trace.planned.option_keys.length || actionLogits.length !== 2
    || [...logits, ...actionLogits].some((value) => !Number.isFinite(value))) return;
  const probabilities = softmax(logits);
  const actionProbabilities = softmax(actionLogits);
  if (probabilities.length !== trace.planned.option_keys.length
    || ![...probabilities, ...actionProbabilities].every(Number.isFinite)) return;
  let winner = 0;
  for (let index = 1; index < probabilities.length; index += 1) {
    if (probabilities[index]! > probabilities[winner]!) winner = index;
  }
  const confidence = answerConfidence(probabilities);
  const max = Math.max(...probabilities);
  trace.raw_logits = logits;
  trace.raw_action_logits = actionLogits;
  trace.probabilities = probabilities;
  trace.action_probabilities = actionProbabilities;
  trace.predicted_candidate_id = trace.planned.option_keys[winner] ?? null;
  trace.max_probability = max;
  trace.answer_confidence_milli = Math.round(confidence * 1000);
  trace.tie_for_maximum = probabilities.filter((value) => value === max).length > 1;
}

function evaluateSamples(
  samples: readonly SampleTrace[],
  labels: readonly { case_id: string; expected_candidate_id: string; rationale: string }[],
): { rows: EvaluationRow[]; metrics: Record<string, unknown>; qualityScreen: Record<string, unknown> } {
  const labelsByCase = new Map(labels.map((label) => [label.case_id, label]));
  const caseCount = S1_QUALITY_CASE_COUNT;
  const canonical = samples.filter((sample) => sample.planned.permutation_id === 'canonical');
  const rows = canonical.flatMap((sample) => {
    const label = labelsByCase.get(sample.planned.case_id);
    return label ? [{ sample, expected_candidate_id: label.expected_candidate_id,
      correct: sample.predicted_candidate_id === label.expected_candidate_id }] : [];
  });
  const productionAccepted = rows.filter((row) => row.sample.status === 'accepted');
  const productionCorrect = productionAccepted.filter((row) => row.correct).length;
  const canonicalRaw = canonical.filter(hasProbabilities);
  const rawCorrect = canonicalRaw.filter((sample) => labelsByCase.get(sample.planned.case_id)?.expected_candidate_id
    === sample.predicted_candidate_id).length;
  const accuracyCoverage = ACCURACY_COVERAGE_THRESHOLDS_MILLI.map((threshold) => {
    const accepted = canonical.filter((sample) => sample.answer_confidence_milli !== null
      && sample.answer_confidence_milli >= threshold
      && (sample.action_probabilities?.[0] ?? 0) >= MINIMUM_ACTION_PROBABILITY);
    const correct = accepted.filter((sample) => {
      const label = labelsByCase.get(sample.planned.case_id);
      return label?.expected_candidate_id === sample.predicted_candidate_id;
    }).length;
    return {
      minimum_confidence_milli: threshold,
      action_probability_floor: MINIMUM_ACTION_PROBABILITY,
      denominator_cases: caseCount,
      raw_head_output_cases: canonicalRaw.length,
      accepted_cases: accepted.length,
      coverage: accepted.length / caseCount,
      correct_accepted_cases: correct,
      conditional_accuracy: accepted.length > 0 ? correct / accepted.length : null,
      all_case_accuracy: correct / caseCount,
      counterfactual_only: threshold !== MINIMUM_CONFIDENCE_MILLI,
    };
  });
  const stratumMetrics = ['navigation', 'disambiguation', 'recovery', 'bilingual'].map((stratum) => {
    const stratumRows = rows.filter((row) => row.sample.planned.stratum === stratum);
    const accepted = stratumRows.filter((row) => row.sample.status === 'accepted');
    const correct = accepted.filter((row) => row.correct).length;
    const raw = stratumRows.filter((row) => hasProbabilities(row.sample));
    const rawCorrectInStratum = raw.filter((row) => row.correct).length;
    return {
      stratum,
      denominator_cases: 5,
      canonical_rows: stratumRows.length,
      real_head_output_cases: stratumRows.filter((row) => hasProbabilities(row.sample)).length,
      accepted_cases: accepted.length,
      production_policy_coverage: accepted.length / 5,
      correct_accepted_cases: correct,
      conditional_accuracy: accepted.length > 0 ? correct / accepted.length : null,
      all_case_accuracy: correct / 5,
      raw_argmax_correct_cases: rawCorrectInStratum,
      raw_argmax_accuracy: raw.length > 0 ? rawCorrectInStratum / raw.length : null,
      raw_argmax_accuracy_over_fixed_five_case_denominator: rawCorrectInStratum / 5,
    };
  });
  const calibration = calibrationMetrics(canonicalRaw, labelsByCase);
  const permutation = permutationMetrics(samples);
  const fullCanonical = canonical.length === caseCount && canonicalRaw.length === caseCount;
  const perStratumPass = stratumMetrics.every((item) => item.canonical_rows === 5
    && item.production_policy_coverage >= 0.6
    && item.conditional_accuracy !== null && item.conditional_accuracy >= 0.8);
  const screenChecks = {
    all_20_canonical_real_head_outputs: fullCanonical,
    production_policy_coverage_at_least_0_80: productionAccepted.length / caseCount >= 0.8,
    accepted_conditional_accuracy_at_least_0_95: productionAccepted.length > 0
      && productionCorrect / productionAccepted.length >= 0.95,
    each_stratum_coverage_at_least_0_60_and_accuracy_at_least_0_80: perStratumPass,
    ece_available_and_at_most_0_10: calibration.expected_calibration_error !== null
      && calibration.complete_canonical_output === true && calibration.expected_calibration_error <= 0.1,
    all_20_cases_permutation_stability_at_least_0_95: permutation.complete_cases === caseCount
      && permutation.stability !== null && permutation.stability >= 0.95,
  };
  const assessable = fullCanonical && rows.length === caseCount && permutation.complete_cases === caseCount
    && calibration.complete_canonical_output === true;
  const qualityScreen = {
    status: Object.values(screenChecks).every(Boolean) ? 'diagnostic_pass' : assessable ? 'diagnostic_fail' : 'not_assessable',
    checks: screenChecks,
    is_product_promotion_gate: false,
    scope: 'small hand-authored M0 fixture diagnostic only',
  };
  const metrics = {
    denominator_cases: caseCount,
    canonical_samples_planned: caseCount,
    canonical_samples_recorded: canonical.length,
    canonical_real_head_output_cases: canonicalRaw.length,
    production_policy: {
      confidence_threshold_milli: MINIMUM_CONFIDENCE_MILLI,
      action_probability_floor: MINIMUM_ACTION_PROBABILITY,
      accepted_cases: productionAccepted.length,
      coverage: productionAccepted.length / caseCount,
      correct_accepted_cases: productionCorrect,
      conditional_accuracy: productionAccepted.length > 0 ? productionCorrect / productionAccepted.length : null,
      all_case_accuracy: productionCorrect / caseCount,
      empty_accepted_denominator: productionAccepted.length === 0,
    },
    raw_argmax_accuracy: {
      denominator_cases: caseCount,
      real_head_output_cases: canonicalRaw.length,
      correct_raw_argmax_cases: rawCorrect,
      accuracy_over_fixed_case_denominator: rawCorrect / caseCount,
      accuracy_among_raw_output_cases: canonicalRaw.length > 0 ? rawCorrect / canonicalRaw.length : null,
      includes_backend_refusals_with_valid_logits: true,
      empty_raw_output_denominator: canonicalRaw.length === 0,
    },
    accuracy_coverage_curve: accuracyCoverage,
    by_stratum: stratumMetrics,
    calibration,
    permutation,
    score_first_position_bias: { status: 'not_run', reason: 'current backend accepts choice only' },
  };
  return { rows, metrics, qualityScreen };
}

function calibrationMetrics(
  samples: readonly SampleTrace[],
  labelsByCase: ReadonlyMap<string, { expected_candidate_id: string }>,
): CalibrationMetrics {
  const bins = Array.from({ length: CALIBRATION_BINS }, (_, index) => ({
    bin: index,
    lower_inclusive: index / CALIBRATION_BINS,
    upper_exclusive: (index + 1) / CALIBRATION_BINS,
    includes_upper_bound: index === CALIBRATION_BINS - 1,
    count: 0,
    mean_confidence: null as number | null,
    accuracy: null as number | null,
  }));
  let squaredErrorTotal = 0;
  let correctCount = 0;
  for (const sample of samples) {
    if (!sample.probabilities || sample.max_probability === null || !sample.predicted_candidate_id) continue;
    const label = labelsByCase.get(sample.planned.case_id);
    if (!label) continue;
    const index = Math.min(CALIBRATION_BINS - 1, Math.floor(sample.max_probability * CALIBRATION_BINS));
    const bin = bins[index]!;
    const correct = sample.predicted_candidate_id === label.expected_candidate_id;
    bin.count += 1;
    bin.mean_confidence = (bin.mean_confidence ?? 0) + sample.max_probability;
    bin.accuracy = (bin.accuracy ?? 0) + Number(correct);
    correctCount += Number(correct);
    for (let optionIndex = 0; optionIndex < sample.probabilities.length; optionIndex += 1) {
      const target = sample.planned.option_keys[optionIndex] === label.expected_candidate_id ? 1 : 0;
      const difference = sample.probabilities[optionIndex]! - target;
      squaredErrorTotal += difference * difference;
    }
  }
  const sampleCount = samples.filter((sample) => sample.probabilities !== null).length;
  let ece = 0;
  for (const bin of bins) {
    if (bin.count === 0) continue;
    bin.mean_confidence = bin.mean_confidence! / bin.count;
    bin.accuracy = bin.accuracy! / bin.count;
    ece += bin.count / Math.max(1, sampleCount) * Math.abs(bin.mean_confidence - bin.accuracy);
  }
  return {
    raw_output_cases: sampleCount,
    complete_canonical_output: sampleCount === S1_QUALITY_CASE_COUNT,
    expected_calibration_error: sampleCount > 0 ? ece : null,
    multiclass_brier_score: sampleCount > 0 ? squaredErrorTotal / sampleCount : null,
    top_choice_accuracy_over_raw_output_cases: sampleCount > 0 ? correctCount / sampleCount : null,
    empty_raw_output_denominator: sampleCount === 0,
    bins,
  };
}

function permutationMetrics(samples: readonly SampleTrace[]): PermutationMetrics {
  const caseIds = S1_QUALITY_CORPUS.model_cases.map((item) => item.case_id);
  let completeCases = 0;
  let flippedCases = 0;
  let validPairComparisons = 0;
  let flippedPairComparisons = 0;
  const byCase: Array<Record<string, unknown>> = [];
  for (const caseId of caseIds) {
    const group = samples.filter((sample) => sample.planned.case_id === caseId);
    const canonical = group.find((sample) => sample.planned.permutation_id === 'canonical');
    const alternatives = group.filter((sample) => sample.planned.permutation_id !== 'canonical');
    const eligible = hasRealPrediction(canonical)
      && alternatives.length === group.length - 1
      && alternatives.every(hasRealPrediction);
    const caseFlipped = eligible && alternatives.some((sample) => sample.predicted_candidate_id !== canonical!.predicted_candidate_id);
    for (const alternative of alternatives) {
      if (hasRealPrediction(canonical) && hasRealPrediction(alternative)) {
        validPairComparisons += 1;
        if (canonical.predicted_candidate_id !== alternative.predicted_candidate_id) flippedPairComparisons += 1;
      }
    }
    if (eligible) {
      completeCases += 1;
      if (caseFlipped) flippedCases += 1;
    }
    byCase.push({
      case_id: caseId,
      planned_permutations: group.length,
      complete_raw_output_for_every_permutation: eligible,
      canonical_prediction: canonical?.predicted_candidate_id ?? null,
      permutation_predictions: alternatives.map((sample) => ({
        permutation_id: sample.planned.permutation_id,
        predicted_candidate_id: sample.predicted_candidate_id,
      })),
      flipped: eligible ? caseFlipped : null,
    });
  }
  return {
    planned_cases: caseIds.length,
    complete_cases: completeCases,
    flipped_cases: flippedCases,
    case_flip_rate: completeCases > 0 ? flippedCases / completeCases : null,
    stability: completeCases > 0 ? 1 - flippedCases / completeCases : null,
    valid_canonical_to_permutation_comparisons: validPairComparisons,
    flipped_comparisons: flippedPairComparisons,
    pair_flip_rate: validPairComparisons > 0 ? flippedPairComparisons / validPairComparisons : null,
    zero_denominator: completeCases === 0,
    by_case: byCase,
  };
}

function summarizePreflight(samples: readonly PlannedSample[]): Record<string, unknown> {
  const lengths = samples.flatMap((sample) => sample.preflight.sequence_token_count === null
    ? [] : [sample.preflight.sequence_token_count]).sort((a, b) => a - b);
  const at = (fraction: number): number | null => lengths.length
    ? lengths[Math.max(0, Math.ceil(fraction * lengths.length) - 1)]! : null;
  return {
    planned_sample_count: samples.length,
    candidate_count_distribution: countDistribution(samples.map((sample) => sample.request.state.candidates.length)),
    option_count_distribution: countDistribution(samples.map((sample) => sample.request.questions[0]!.options.length)),
    sequence_token_count: {
      measured_samples: lengths.length,
      min: lengths[0] ?? null,
      p50: at(0.5),
      p95: at(0.95),
      max: lengths.at(-1) ?? null,
    },
    backend_budget_valid_samples: samples.filter((sample) => sample.preflight.backend_budget_valid === true).length,
    preflight_rejected_or_unavailable_samples: samples.filter((sample) => sample.preflight.backend_budget_valid !== true).length,
    full_rows_retained: true,
  };
}

function countDistribution(values: readonly number[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const value of values) counts[String(value)] = (counts[String(value)] ?? 0) + 1;
  return counts;
}

function hasProbabilities(sample: SampleTrace): boolean {
  return sample.real_inference_observed && sample.probabilities !== null && sample.action_probabilities !== null;
}

function hasRealPrediction(sample: SampleTrace | undefined): sample is SampleTrace {
  return sample !== undefined && sample.real_inference_observed && sample.predicted_candidate_id !== null;
}

function sameShape(actual: readonly number[] | null, expected: readonly number[]): boolean {
  return actual !== null && actual.length === expected.length && actual.every((dimension, index) => dimension === expected[index]);
}

function serializeSample(sample: SampleTrace): Record<string, unknown> {
  return {
    sample_id: sample.planned.sample_id,
    case_id: sample.planned.case_id,
    stratum: sample.planned.stratum,
    permutation_id: sample.planned.permutation_id,
    permutation_order: sample.planned.permutation_order,
    option_keys: sample.planned.option_keys,
    request_sha256: sample.planned.request_sha256,
    laya_feature_sha256: sample.planned.laya_feature_sha256,
    preflight: sample.planned.preflight,
    started_at: sample.started_at || null,
    status: sample.status,
    decision_call_made: sample.decision_call_made,
    real_inference_observed: sample.real_inference_observed,
    wall_ms: sample.wall_ms,
    backend_inference_ms: sample.backend_inference_ms,
    backend_queue_ms: sample.backend_queue_ms,
    backend_failure: sample.backend_failure,
    backend_error: sample.backend_error,
    proposal: sample.backend_proposal,
    calls: sample.calls,
    graph_wall_ms: sample.graph_wall_ms,
    graph_errors: sample.graph_errors,
    head_capture: sample.head_capture,
    raw_logits: sample.raw_logits,
    raw_action_logits: sample.raw_action_logits,
    probabilities: sample.probabilities,
    action_probabilities: sample.action_probabilities,
    predicted_candidate_id: sample.predicted_candidate_id,
    max_probability: sample.max_probability,
    answer_confidence_milli: sample.answer_confidence_milli,
    tie_for_maximum: sample.tie_for_maximum,
    rss_bytes: sample.rss_bytes,
  };
}

async function loadPinnedTokenizer(): Promise<TokenizerLike> {
  const bytes = await readFile(TOKENIZER_PATH);
  if (sha256(bytes) !== EXPECTED_TOKENIZER_SHA256) throw new Error('pinned_tokenizer_hash_mismatch');
  let parsed: TokenizerData | null;
  try { parsed = parseTokenizerJson(JSON.parse(bytes.toString('utf8'))); }
  finally { bytes.fill(0); }
  if (parsed === null) throw new Error('pinned_tokenizer_parse_failed');
  const ids = parsed.ids;
  if (ids.cls !== 2 || ids.sep !== 1 || ids.mask !== 4 || ids.pad !== 0 || ids.unk !== 3) {
    throw new Error('pinned_tokenizer_ids_mismatch');
  }
  return {
    clsId: ids.cls,
    sepId: ids.sep,
    maskId: ids.mask,
    padId: ids.pad,
    maskToken: parsed.maskToken,
    encode: (text: string) => encodeWithData(parsed, text),
  };
}

async function hashInventory(): Promise<HashInventory> {
  const sourceHashes: Record<string, string> = {};
  for (const file of SOURCE_FILES) sourceHashes[file] = await hashFile(join(ROOT, file));
  const manifestBytes = await readFile(MANIFEST_PATH);
  const manifest = JSON.parse(manifestBytes.toString('utf8')) as Record<string, unknown>;
  const runtimePackagePath = require.resolve('onnxruntime-node/package.json');
  const runtimeRoot = dirname(runtimePackagePath);
  const runtimePackageBytes = await readFile(runtimePackagePath);
  const runtimePackage = JSON.parse(runtimePackageBytes.toString('utf8')) as { version?: string };
  const nativeDirectory = join(runtimeRoot, 'bin', 'napi-v6', process.platform, process.arch);
  const entries = await readdir(nativeDirectory);
  const nativeNames = entries.filter((name) => name.endsWith('.node')
    || /^libonnxruntime.*\.(dylib|so(?:\.\d+)*|dll)$/.test(name)).sort();
  if (!nativeNames.some((name) => name.endsWith('.node'))) throw new Error('runtime_binding_unavailable');
  const nativeHashes: Record<string, string> = {};
  for (const name of nativeNames) {
    const path = join(nativeDirectory, name);
    const info = await stat(path);
    if (!info.isFile()) throw new Error('runtime_native_file_invalid');
    nativeHashes[relative(ROOT, path)] = await hashFile(path);
  }
  return {
    source_sha256: sourceHashes,
    model_manifest_sha256: sha256(manifestBytes),
    model_id: typeof manifest['model_id'] === 'string' ? manifest['model_id'] : null,
    model_revision: typeof manifest['model_revision'] === 'string' ? manifest['model_revision'] : null,
    model_package_files: typeof manifest['package_files'] === 'object' && manifest['package_files'] !== null
      ? manifest['package_files'] as Record<string, unknown> : null,
    runtime_package_version: runtimePackage.version ?? 'unknown',
    runtime_package_sha256: sha256(runtimePackageBytes),
    runtime_native_sha256: nativeHashes,
  };
}

async function hashFile(path: string): Promise<string> {
  const digest = createHash('sha256');
  const stream = createReadStream(path);
  for await (const chunk of stream) digest.update(chunk as Buffer);
  return digest.digest('hex');
}

async function writeExclusive(path: string, value: Record<string, unknown>): Promise<void> {
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' });
}

function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(',')}]`;
  if (typeof value === 'object' && value !== null) {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${stableJson(record[key])}`).join(',')}}`;
  }
  return JSON.stringify(value) ?? 'null';
}

function sha256(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex');
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : 'UnknownError';
}

function errorMessage(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return message.replace(/[\r\n\t]+/g, ' ').slice(0, 500);
}

function elapsed(start: number): number {
  return Math.max(0, performance.now() - start);
}
