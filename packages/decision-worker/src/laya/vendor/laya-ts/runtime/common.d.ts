import type { TokenizerLike } from "./tokenizer.js";
export type QType = "choice" | "score" | "noul";
export interface InternalQ {
    t: QType;
    ins: string;
    crit: unknown;
    labels?: {
        false: string;
        true: string;
    };
}
export declare function serializeState(state: unknown): string;
/** The text of a criterion: the string itself, else the JSON the model was shown.
 *
 * Exported because `agent.ts` needs it for a `score` answer's `legend`, which maps an index to the
 * text of that level. Putting the raw value in made the response's JSON types depend on the
 * caller's input -- a numeric scale came back as `{"0": 1}`, a boolean as `{"0": true}` and a null
 * as `{"0": null}`, which a Jev client refuses to parse (#302).
 */
export declare function renderCriterion(v: unknown): string;
export declare function renderOptions(q: InternalQ): string[];
export interface OptionStats {
    /** Options the question defines, not the markers that survived the sequence clamp. */
    total: number;
    /** How many of them still have a token span of their own. */
    distinct: number;
    /** The per-option cap the head budget forced, or null when none was applied. */
    tokens_per_option: number | null;
}
export interface QuestionPrefix {
    /** [CLS] head [SEP] options [SEP] — the state-independent part of the sequence. */
    ids: number[];
    /** Mask-marker positions (absolute; the prefix sits at the start of the final sequence). */
    markers: number[];
    nOptions: number;
    optionStats: OptionStats;
}
export declare function buildQuestionPrefix(tok: TokenizerLike, q: InternalQ, maxLen?: number, headMaxLen?: number, optionOrder?: number[]): QuestionPrefix;
export interface SequenceStats {
    /** Encoded length of the full state, before the window clamp. */
    state_tokens: number;
    /** State tokens that actually reached the encoder after the final maxLen clamp. */
    state_tokens_used: number;
    /** State tokens dropped by the clamp: state_tokens - state_tokens_used. */
    state_tokens_dropped: number;
    /** True when the clamp dropped any state token. */
    truncated: boolean;
    /** What the head budget did to this question's options (Python `build_sequence`'s other stats). */
    options: OptionStats;
}
/** Append pre-encoded state tokens to a question prefix. Identical output to building the
 * whole sequence in one pass, but the state only needs encoding once per state, not once
 * per (state, question) pair.
 *
 * The state is clamped to whatever room the head leaves; `stats` reports that clamp so callers
 * never have to guess it from the character length of what they sent (issue #174, Python #181). */
export declare function sequenceWithState(prefix: QuestionPrefix, stateIds: number[], sepId: number, maxLen?: number, truncateLeft?: boolean): {
    ids: number[];
    markers: number[];
    stats: SequenceStats;
};
/** The questions whose options no longer have a token span each, keyed by question id.
 *
 * `total` is what the question defines, not the number of markers that reached the sequence, so a
 * report cannot say "43 of 43" about a question whose 28 missing options never entered the input
 * at all. Mirrors Python `laya.common.collapsed_options`; empty when nothing collapsed, which is
 * the overwhelming majority of requests, so the agents add the key only when it says something. */
export declare function collapsedOptions(qids: string[], stats: (SequenceStats | undefined)[]): Record<string, OptionStats>;
export declare function buildSequence(tok: TokenizerLike, state: unknown, q: InternalQ, maxLen?: number, headMaxLen?: number, optionOrder?: number[], truncateLeft?: boolean): {
    ids: number[];
    markers: number[];
    stats: SequenceStats;
};
export declare function softmax(z: number[]): number[];
export declare function confidenceFromProbs(p: number[]): number;
export declare function answerConfidence(p: number[]): number;
export type MinConfidenceMap = Record<string, number>;
export type MinConfidence = number | MinConfidenceMap;
/**
 * Validate a per-bucket abstention-threshold map (#394).
 *
 * Keys are option-count bucket strings like "choice:2", "choice:3-5", "score:6-10", "noul:2",
 * plus an optional "default". Values are numbers in [0.0, 1.0].
 */
export declare function checkMinConfidenceMap(m: unknown): MinConfidenceMap;
/**
 * Validate opt-in abstention threshold `min_confidence` (#361, #394).
 *
 * Either a real number in [0.0, 1.0] or a per-bucket mapping. Booleans are rejected.
 */
export declare function checkMinConfidence(v: unknown): MinConfidence;
export declare function optionBucket(answer: Record<string, unknown>): string | null;
export declare function resolveMinConfidence(answer: Record<string, unknown>, thresholds: MinConfidenceMap, defaultVal?: number): number;
/**
 * Opt-in abstention marker (#361, #394): flag answers whose confidence falls below `min_confidence`.
 *
 * Reads `answer_confidence` (the calibrated max(p) confidence, invariant to label count k),
 * falling back to `confidence` if `answer_confidence` is absent.
 * The raw answer and confidence stay intact; `low_confidence: true` is added.
 *
 * Supports both a scalar number in [0.0, 1.0] and a per-bucket mapping of thresholds.
 */
export declare function flagLowConfidence(results: Array<Record<string, unknown>> | Record<string, unknown>, minConfidence: MinConfidence): void;
export declare const TEMP_MIN = 0.5, TEMP_MAX = 5;
export declare function clampTemperature(t: unknown): number;
export declare function tempBucket(qtype: number, k: number): string;
export interface BinningEntry {
    bins: number;
    values: number[];
}
export type BinningMap = Record<string, BinningEntry>;
/**
 * Validate a histogram-binning recalibration map.
 *
 * Keys are option-count bucket strings like "choice:2", "choice:3-5", "score:6-10", "noul:2".
 * Values are objects with an integer `bins >= 1` and a `values` array of length `bins`
 * where each number is in [0.0, 1.0].
 */
export declare function checkBinningMap(m: unknown): BinningMap;
/**
 * Recalibrate one `answer_confidence` for its option-count `bucket` (tempBucket).
 *
 * Returns the confidence unchanged when the map has no entry for the bucket, so a bucket the map
 * was not fit for passes through rather than being forced to a wrong value.
 */
export declare function applyBinningMap(confidence: number, bucket: string, binningMap?: BinningMap | null): number;
/** Max of a length list without spread (Math.max(...arr) throws RangeError past ~100k args). */
export declare function maxOf(values: ArrayLike<number>, fallback?: number): number;
export interface CollateItem {
    ids: number[];
    markers: number[];
    qtype: number;
    label?: number;
    target?: number[];
    [k: string]: unknown;
}
export interface CollatedBatch {
    inputIds: number[][];
    attentionMask: number[][];
    markerPos: number[][];
    markerMask: boolean[][];
    qtype: number[];
    label: number[];
    meta: Record<string, unknown>[];
    target?: number[][];
}
/** TS parity of py `collate_items(batch, pad_id)`: batch = list of groups. */
export declare function collateItems(batch: CollateItem[][], padId: number): CollatedBatch | null;
