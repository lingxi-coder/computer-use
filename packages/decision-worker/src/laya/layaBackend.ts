import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { lstat, readFile, realpath } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { performance } from 'node:perf_hooks';

import type {
  Candidate,
  DecisionFailure,
  DecisionProposal,
  DecisionRequest,
  PolicyChoice,
} from '@computer-use/desktop-api';

import type { DecisionBackend } from '../worker.js';
import { answerConfidence, buildSequence, serializeState, softmax, type InternalQ } from './vendor/laya-ts/runtime/common.js';
import { encodeWithData, parseTokenizerJson, type TokenizerData, type TokenizerLike } from './vendor/laya-ts/runtime/tokenizer.js';

const MODEL_ID = 'convaiinnovations/laya-multilingual';
const MODEL_REVISION = 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67';
const PACKAGE_MANIFEST_SHA256 = 'c4e5cb4dc827f1ca76a47f3c19d88ef7cd47a53268e474b0c98df6c998399424';
const BACKEND_NAME = `laya-onnx-cpu/${MODEL_REVISION}`;
const MAX_MANIFEST_BYTES = 64 * 1024;
const MAX_STATE_BYTES = 32 * 1024;
const MAX_QUESTION_BYTES = 32 * 1024;
const MAX_OPTIONS = 128;
const MAX_INPUT_TOKENS = 512;
const MAX_HEAD_TOKENS = 192;
const MAX_MARKERS = 256;
const MAX_QUEUE = 4;
const MAX_INFERENCE_BUDGET_MS = 30_000;
const DEFAULT_THREADS = 2;
const DEFAULT_MINIMUM_CONFIDENCE_MILLI = 800;
const MIN_ACTION_PROBABILITY = 0.5;
const HIDDEN_SIZE = 768;
const SESSION_FACTORY = Symbol('LayaDecisionBackend verified-session factory');

const EXPECTED_FILES = {
  'encoder.onnx': { bytes: 2_771_288, sha256: '85080d470855319def232f3927d1d8d8794c23a2fae943b3f4564a1e8d3204d0' },
  'encoder.onnx.data': { bytes: 1_227_816_960, sha256: 'f1e49a027352064d72831863456be6160748854296bff8332df14130dbf79834' },
  'head.onnx': { bytes: 244_928, sha256: '66d3710bedbfa1043411e8de7c60927f2a279105d18e4f1b9db175e06452a02a' },
  'head.onnx.data': { bytes: 59_899_904, sha256: '0292a4eb478c2c3ec0b6555b4a4093bc802e965654d7aeb4ac22907a17f261d3' },
  'rl_agent_config.json': { bytes: 472, sha256: '25061739243b617ad88d1219ba6f8a9c86c5881ca28df024fa2d9b3b2fcc30c6' },
  'tokenizer.json': { bytes: 34_363_188, sha256: '609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f' },
} as const;

type ModelFile = keyof typeof EXPECTED_FILES;

export interface OnnxTensor {
  readonly data: Float32Array | BigInt64Array | Uint8Array;
  readonly dims: readonly number[];
  readonly type?: string;
}

export interface OnnxSession {
  readonly inputNames: readonly string[];
  readonly outputNames: readonly string[];
  run(feeds: Record<string, OnnxTensor>): Promise<Record<string, OnnxTensor>>;
  release?(): Promise<void> | void;
  dispose?(): Promise<void> | void;
}

export interface OnnxRuntimeModule {
  Tensor: new (
    type: 'int64' | 'float32' | 'bool',
    data: BigInt64Array | Float32Array | Uint8Array,
    dims: number[],
  ) => OnnxTensor;
  InferenceSession: {
    create(path: string, options: {
      executionProviders: ['cpu'];
      intraOpNumThreads: number;
      interOpNumThreads: 1;
    }): Promise<OnnxSession>;
  };
}

export interface LayaDecisionBackendOptions {
  /** The pinned package-manifest.json; assets are beside it or in its bundle/ child directory. */
  bundleManifestPath: string;
  /** CPU intra-op threads. Values outside 1..4 are refused. */
  numThreads?: number;
  /** Conservative answer-confidence floor in thousandths; values below 800 are refused. */
  minimumConfidenceMilli?: number;
  /** Injection seam for deterministic tests; production dynamically loads onnxruntime-node. */
  runtime?: OnnxRuntimeModule;
  /** Cancels hash verification/session loading when the owning worker is shutting down. */
  signal?: AbortSignal;
}

export class LayaBackendUnavailableError extends Error {
  constructor(message = 'The pinned Laya CPU backend is unavailable.', options?: ErrorOptions) {
    super(message, options);
    this.name = 'LayaBackendUnavailableError';
  }
}

function abortError(): Error {
  const error = new Error('The Laya decision was cancelled.');
  error.name = 'AbortError';
  return error;
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw abortError();
}

function safeFailure(
  request: DecisionRequest,
  failure: DecisionFailure,
  modelRevision = MODEL_REVISION,
  backend = BACKEND_NAME,
): DecisionProposal {
  return {
    decision_id: request.decision_id,
    request_generation: request.request_generation,
    candidate_set_digest: request.state.candidate_set_digest,
    backend,
    source: 's1',
    model_revision: modelRevision,
    answers: [],
    choice: null,
    failure,
    truncated: failure === 'truncated',
    queue_ms: 0,
    inference_ms: 0,
  };
}

/** Use when backend loading failed before a DecisionBackend instance was available. */
export function layaUnavailableProposal(request: DecisionRequest): DecisionProposal {
  return safeFailure(request, 'unavailable', 'unavailable', 'laya-onnx-cpu/unavailable');
}

function sha256(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex');
}

interface PinnedFileEntry { bytes: number; sha256: string }
interface PinnedManifest {
  kind: string;
  model_id: string;
  model_revision: string;
  package_bytes: number;
  package_files: Record<string, PinnedFileEntry>;
  external_data_references?: Record<string, { external_tensor_count: number; locations: string[] }>;
}

const EXPECTED_PACKAGE_BYTES = Object.values(EXPECTED_FILES).reduce((sum, file) => sum + file.bytes, 0);
const EXPECTED_NAMES = Object.keys(EXPECTED_FILES).sort();

function verifyManifestShape(value: unknown): PinnedManifest {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new LayaBackendUnavailableError();
  }
  const manifest = value as Partial<PinnedManifest>;
  if (manifest.kind !== 'laya_onnx_package_manifest' || manifest.model_id !== MODEL_ID
    || manifest.model_revision !== MODEL_REVISION || manifest.package_bytes !== EXPECTED_PACKAGE_BYTES
    || typeof manifest.package_files !== 'object' || manifest.package_files === null || Array.isArray(manifest.package_files)) {
    throw new LayaBackendUnavailableError();
  }
  const names = Object.keys(manifest.package_files).sort();
  if (names.length !== EXPECTED_NAMES.length || names.some((name, index) => name !== EXPECTED_NAMES[index])) {
    throw new LayaBackendUnavailableError();
  }
  for (const name of EXPECTED_NAMES as ModelFile[]) {
    const item = manifest.package_files[name];
    const expected = EXPECTED_FILES[name];
    if (!item || item.bytes !== expected.bytes || item.sha256 !== expected.sha256) {
      throw new LayaBackendUnavailableError();
    }
  }
  const references = manifest.external_data_references;
  const encoderReferences = references?.['encoder.onnx']?.locations;
  const headReferences = references?.['head.onnx']?.locations;
  if (!encoderReferences || encoderReferences.length !== 1 || encoderReferences[0] !== 'encoder.onnx.data'
    || !headReferences || headReferences.length !== 1 || headReferences[0] !== 'head.onnx.data') {
    throw new LayaBackendUnavailableError();
  }
  return manifest as PinnedManifest;
}

async function regularFile(path: string, parent: string, expectedBytes: number, signal?: AbortSignal): Promise<void> {
  throwIfAborted(signal);
  const stat = await lstat(path).catch(() => null);
  if (!stat || stat.isSymbolicLink() || !stat.isFile() || stat.size !== expectedBytes) {
    throw new LayaBackendUnavailableError();
  }
  const realParent = await realpath(parent).catch(() => null);
  const realFile = await realpath(path).catch(() => null);
  if (!realParent || !realFile || dirname(realFile) !== realParent) throw new LayaBackendUnavailableError();
}

async function hashFile(path: string, bytes: number, parent: string, signal?: AbortSignal): Promise<string> {
  await regularFile(path, parent, bytes, signal);
  const hash = createHash('sha256');
  const stream = createReadStream(path);
  const onAbort = (): void => { stream.destroy(abortError()); };
  signal?.addEventListener('abort', onAbort, { once: true });
  try {
    for await (const chunk of stream) {
      throwIfAborted(signal);
      hash.update(chunk as Buffer);
    }
    throwIfAborted(signal);
    return hash.digest('hex');
  } catch {
    if (signal?.aborted) throw abortError();
    throw new LayaBackendUnavailableError();
  } finally {
    signal?.removeEventListener('abort', onAbort);
    stream.destroy();
  }
}

async function readVerifiedTokenizer(path: string, parent: string, signal?: AbortSignal): Promise<Buffer> {
  const expected = EXPECTED_FILES['tokenizer.json'];
  await regularFile(path, parent, expected.bytes, signal);
  const parts: Buffer[] = [];
  const hash = createHash('sha256');
  let total = 0;
  const stream = createReadStream(path);
  const onAbort = (): void => { stream.destroy(abortError()); };
  signal?.addEventListener('abort', onAbort, { once: true });
  try {
    for await (const chunk of stream) {
      throwIfAborted(signal);
      const part = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      total += part.byteLength;
      if (total > expected.bytes) throw new LayaBackendUnavailableError();
      hash.update(part);
      parts.push(part);
    }
    throwIfAborted(signal);
    if (total !== expected.bytes || hash.digest('hex') !== expected.sha256) throw new LayaBackendUnavailableError();
    return Buffer.concat(parts, total);
  } catch {
    if (signal?.aborted) throw abortError();
    throw new LayaBackendUnavailableError();
  } finally {
    signal?.removeEventListener('abort', onAbort);
    stream.destroy();
  }
}

async function findAssetDirectory(manifestPath: string): Promise<string> {
  const manifestDirectory = dirname(resolve(manifestPath));
  const directEncoder = join(manifestDirectory, 'encoder.onnx');
  const direct = await lstat(directEncoder).catch(() => null);
  if (direct) {
    if (direct.isSymbolicLink() || !direct.isFile()) throw new LayaBackendUnavailableError();
    return manifestDirectory;
  }
  const bundleDirectory = join(manifestDirectory, 'bundle');
  const bundleStat = await lstat(bundleDirectory).catch(() => null);
  if (!bundleStat || bundleStat.isSymbolicLink() || !bundleStat.isDirectory()) throw new LayaBackendUnavailableError();
  return bundleDirectory;
}

interface LoadedBundle {
  readonly assetDirectory: string;
  readonly paths: Record<ModelFile, string>;
  readonly tokenizer: TokenizerLike;
}

async function verifyBundle(bundleManifestPath: string, signal?: AbortSignal): Promise<LoadedBundle> {
  throwIfAborted(signal);
  const manifestStat = await lstat(bundleManifestPath).catch(() => null);
  if (!manifestStat || manifestStat.isSymbolicLink() || !manifestStat.isFile()
    || manifestStat.size <= 0 || manifestStat.size > MAX_MANIFEST_BYTES) {
    throw new LayaBackendUnavailableError();
  }
  const manifestParent = dirname(resolve(bundleManifestPath));
  const resolvedParent = await realpath(manifestParent).catch(() => null);
  const resolvedManifest = await realpath(bundleManifestPath).catch(() => null);
  if (!resolvedParent || !resolvedManifest || dirname(resolvedManifest) !== resolvedParent) throw new LayaBackendUnavailableError();
  const manifestBytes = await readFile(bundleManifestPath);
  throwIfAborted(signal);
  if (sha256(manifestBytes) !== PACKAGE_MANIFEST_SHA256) throw new LayaBackendUnavailableError();
  let decoded: unknown;
  try { decoded = JSON.parse(manifestBytes.toString('utf8')); }
  catch { throw new LayaBackendUnavailableError(); }
  verifyManifestShape(decoded);

  const assetDirectory = await findAssetDirectory(bundleManifestPath);
  const paths = {} as Record<ModelFile, string>;
  let tokenizerBytes: Buffer | null = null;
  for (const name of EXPECTED_NAMES as ModelFile[]) {
    throwIfAborted(signal);
    const path = join(assetDirectory, name);
    const expected = EXPECTED_FILES[name];
    if (name === 'tokenizer.json') {
      tokenizerBytes = await readVerifiedTokenizer(path, assetDirectory, signal);
      paths[name] = path;
      continue;
    }
    const actual = await hashFile(path, expected.bytes, assetDirectory, signal);
    if (actual !== expected.sha256) throw new LayaBackendUnavailableError();
    paths[name] = path;
  }
  if (!tokenizerBytes) throw new LayaBackendUnavailableError();
  let tokenizerData: TokenizerData | null;
  try { tokenizerData = parseTokenizerJson(JSON.parse(tokenizerBytes.toString('utf8'))); }
  catch { tokenizerData = null; }
  tokenizerBytes.fill(0);
  if (!tokenizerData || JSON.stringify(tokenizerData.ids) !== JSON.stringify({ cls: 2, sep: 1, mask: 4, pad: 0, unk: 3 })) {
    throw new LayaBackendUnavailableError();
  }
  const data = tokenizerData;
  const tokenizer: TokenizerLike = {
    clsId: data.ids.cls,
    sepId: data.ids.sep,
    maskId: data.ids.mask,
    padId: data.ids.pad,
    maskToken: data.maskToken,
    encode: (text) => encodeWithData(data, text),
  };
  return { assetDirectory, paths, tokenizer };
}

function exactNames(actual: readonly string[], expected: readonly string[]): boolean {
  const left = [...actual].sort();
  const right = [...expected].sort();
  return left.length === right.length && left.every((name, index) => name === right[index]);
}

const ENCODER_INPUTS = ['input_ids', 'attention_mask'] as const;
const ENCODER_OUTPUTS = ['last_hidden_state'] as const;
const HEAD_INPUTS = ['hidden_states', 'marker_pos', 'marker_mask', 'qtype', 'attention_mask'] as const;
const HEAD_OUTPUTS = ['logits', 'act_logits'] as const;

function assertSession(session: OnnxSession, inputs: readonly string[], outputs: readonly string[]): void {
  if (!exactNames(session.inputNames, inputs) || !exactNames(session.outputNames, outputs)
    || (typeof session.release !== 'function' && typeof session.dispose !== 'function')) {
    throw new LayaBackendUnavailableError();
  }
}

async function releaseSession(session: OnnxSession | null): Promise<void> {
  if (!session) return;
  if (session.release) await session.release();
  else if (session.dispose) await session.dispose();
}

function validateOptions(options: LayaDecisionBackendOptions): { threads: number; minConfidence: number } {
  if (!options.bundleManifestPath || typeof options.bundleManifestPath !== 'string') {
    throw new LayaBackendUnavailableError();
  }
  const threads = options.numThreads ?? DEFAULT_THREADS;
  const minConfidence = options.minimumConfidenceMilli ?? DEFAULT_MINIMUM_CONFIDENCE_MILLI;
  if (!Number.isInteger(threads) || threads < 1 || threads > 4
    || !Number.isInteger(minConfidence) || minConfidence < DEFAULT_MINIMUM_CONFIDENCE_MILLI || minConfidence > 1000) {
    throw new LayaBackendUnavailableError();
  }
  return { threads, minConfidence };
}

async function loadOrt(): Promise<OnnxRuntimeModule> {
  try {
    const packageName: string = 'onnxruntime-node';
    const runtime = await import(packageName) as unknown as OnnxRuntimeModule & {
      env?: { versions?: { node?: string } };
    };
    if (runtime.env?.versions?.node !== '1.30.0'
      || typeof runtime.Tensor !== 'function' || typeof runtime.InferenceSession?.create !== 'function') {
      throw new Error('invalid ONNX Runtime module');
    }
    return runtime;
  } catch (cause) {
    throw new LayaBackendUnavailableError('onnxruntime-node is unavailable; Laya CPU decisions are disabled.', { cause });
  }
}

interface QueueEntry {
  signal: AbortSignal;
  resolve: (release: () => void) => void;
  reject: (reason: Error) => void;
  onAbort: () => void;
}

class SerialInferenceGate {
  private busy = false;
  private closed = false;
  private readonly pending: QueueEntry[] = [];

  acquire(signal: AbortSignal): Promise<() => void> {
    if (this.closed) return Promise.reject(new LayaBackendUnavailableError());
    if (signal.aborted) return Promise.reject(abortError());
    if (!this.busy) {
      this.busy = true;
      return Promise.resolve(this.releaseOnce());
    }
    if (this.pending.length >= MAX_QUEUE) return Promise.reject(new Error('Laya CPU inference queue is full.'));
    return new Promise((resolveQueue, rejectQueue) => {
      const entry: QueueEntry = {
        signal,
        resolve: resolveQueue,
        reject: rejectQueue,
        onAbort: () => {
          const index = this.pending.indexOf(entry);
          if (index >= 0) this.pending.splice(index, 1);
          signal.removeEventListener('abort', entry.onAbort);
          rejectQueue(abortError());
        },
      };
      this.pending.push(entry);
      signal.addEventListener('abort', entry.onAbort, { once: true });
    });
  }

  close(): void {
    this.closed = true;
    for (const entry of this.pending.splice(0)) {
      entry.signal.removeEventListener('abort', entry.onAbort);
      entry.reject(new LayaBackendUnavailableError());
    }
  }

  private releaseOnce(): () => void {
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.releaseNext();
    };
  }

  private releaseNext(): void {
    if (this.closed) { this.busy = false; return; }
    while (this.pending.length > 0) {
      const entry = this.pending.shift()!;
      entry.signal.removeEventListener('abort', entry.onAbort);
      if (entry.signal.aborted) { entry.reject(abortError()); continue; }
      entry.resolve(this.releaseOnce());
      return;
    }
    this.busy = false;
  }
}

export interface LayaSessionSet {
  runtime: OnnxRuntimeModule;
  encoder: OnnxSession;
  head: OnnxSession;
  tokenizer: TokenizerLike;
  minimumConfidenceMilli?: number;
  modelRevision?: string;
}

/** Internal constructor seam used by the CPU backend's injected-session tests. */
export function createLayaCpuBackendFromSessions(sessions: LayaSessionSet): LayaDecisionBackend {
  assertSession(sessions.encoder, ENCODER_INPUTS, ENCODER_OUTPUTS);
  assertSession(sessions.head, HEAD_INPUTS, HEAD_OUTPUTS);
  const minimumConfidenceMilli = sessions.minimumConfidenceMilli ?? DEFAULT_MINIMUM_CONFIDENCE_MILLI;
  if (!Number.isInteger(minimumConfidenceMilli) || minimumConfidenceMilli < DEFAULT_MINIMUM_CONFIDENCE_MILLI || minimumConfidenceMilli > 1000) {
    throw new LayaBackendUnavailableError();
  }
  return new LayaDecisionBackend(SESSION_FACTORY, sessions.runtime, sessions.encoder, sessions.head, sessions.tokenizer,
    minimumConfidenceMilli, sessions.modelRevision ?? MODEL_REVISION);
}

export class LayaDecisionBackend implements DecisionBackend {
  readonly name = BACKEND_NAME;
  readonly source = 's1' as const;
  readonly modelRevision: string;
  private readonly gate = new SerialInferenceGate();
  private readonly controllers = new Set<AbortController>();
  private readonly operations = new Set<Promise<unknown>>();
  private closed = false;
  private closeTask: Promise<void> | null = null;

  constructor(
    factory: typeof SESSION_FACTORY,
    private readonly runtime: OnnxRuntimeModule,
    private readonly encoder: OnnxSession,
    private readonly head: OnnxSession,
    private readonly tokenizer: TokenizerLike,
    private readonly minimumConfidenceMilli: number,
    modelRevision: string,
  ) {
    if (factory !== SESSION_FACTORY) throw new LayaBackendUnavailableError();
    this.modelRevision = modelRevision;
  }

  static async create(options: LayaDecisionBackendOptions): Promise<LayaDecisionBackend> {
    const { threads, minConfidence } = validateOptions(options);
    throwIfAborted(options.signal);
    const bundle = await verifyBundle(options.bundleManifestPath, options.signal);
    throwIfAborted(options.signal);
    const runtime = options.runtime ?? await loadOrt();
    throwIfAborted(options.signal);
    let encoder: OnnxSession | null = null;
    let head: OnnxSession | null = null;
    try {
      const sessionOptions = {
        executionProviders: ['cpu'] as ['cpu'],
        intraOpNumThreads: threads,
        interOpNumThreads: 1 as const,
      };
      encoder = await runtime.InferenceSession.create(bundle.paths['encoder.onnx'], sessionOptions);
      assertSession(encoder, ENCODER_INPUTS, ENCODER_OUTPUTS);
      throwIfAborted(options.signal);
      head = await runtime.InferenceSession.create(bundle.paths['head.onnx'], sessionOptions);
      assertSession(head, HEAD_INPUTS, HEAD_OUTPUTS);
      throwIfAborted(options.signal);
      return new LayaDecisionBackend(SESSION_FACTORY, runtime, encoder, head, bundle.tokenizer,
        minConfidence, MODEL_REVISION);
    } catch (cause) {
      await Promise.allSettled([releaseSession(encoder), releaseSession(head)]);
      if (options.signal?.aborted) throw abortError();
      if (cause instanceof LayaBackendUnavailableError) throw cause;
      throw new LayaBackendUnavailableError('The pinned Laya ONNX sessions could not be opened for CPU inference.', { cause });
    }
  }

  async decide(request: DecisionRequest, signal: AbortSignal): Promise<DecisionProposal> {
    if (this.closed) throw new LayaBackendUnavailableError();
    const budget = request.result_deadline_ms - request.created_ms;
    if (!Number.isSafeInteger(request.created_ms) || !Number.isSafeInteger(request.result_deadline_ms) || budget <= 0) {
      return safeFailure(request, 'timeout', this.modelRevision, this.name);
    }
    const controller = new AbortController();
    const externalAbort = (): void => controller.abort();
    signal.addEventListener('abort', externalAbort, { once: true });
    if (signal.aborted) controller.abort();
    this.controllers.add(controller);
    let timedOut = false;
    const timer = setTimeout(() => { timedOut = true; controller.abort(); }, Math.min(budget, MAX_INFERENCE_BUDGET_MS));
    const enqueuedAt = performance.now();
    const operation = this.gate.acquire(controller.signal).then(async (release) => {
      try {
        throwIfAborted(controller.signal);
        if (this.closed) throw new LayaBackendUnavailableError();
        return await this.infer(request, controller.signal, enqueuedAt);
      } finally { release(); }
    });
    this.operations.add(operation);
    void operation.then(
      () => { this.operations.delete(operation); this.controllers.delete(controller); },
      () => { this.operations.delete(operation); this.controllers.delete(controller); },
    );
    let rejectOnAbort!: (error: Error) => void;
    const aborted = new Promise<never>((_resolve, reject) => { rejectOnAbort = reject; });
    const onAbort = (): void => rejectOnAbort(abortError());
    controller.signal.addEventListener('abort', onAbort, { once: true });
    try {
      return await Promise.race([operation, aborted]);
    } catch (error) {
      if (timedOut) return safeFailure(request, 'timeout', this.modelRevision, this.name);
      throw error;
    } finally {
      clearTimeout(timer);
      signal.removeEventListener('abort', externalAbort);
      controller.signal.removeEventListener('abort', onAbort);
    }
  }

  close(): Promise<void> {
    this.closeTask ??= (async () => {
      this.closed = true;
      this.gate.close();
      for (const controller of this.controllers) controller.abort();
      await Promise.allSettled([...this.operations]);
      await Promise.allSettled([releaseSession(this.encoder), releaseSession(this.head)]);
    })();
    return this.closeTask;
  }

  private async infer(request: DecisionRequest, signal: AbortSignal, enqueuedAt: number): Promise<DecisionProposal> {
    const question = request.questions.length === 1 ? request.questions[0] : undefined;
    if (!question || question.kind !== 'choice') return safeFailure(request, 'unsupported_question', this.modelRevision, this.name);
    if (!Array.isArray(request.state.candidates) || request.state.candidates.length === 0
      || request.state.candidates.length > MAX_OPTIONS || !Array.isArray(question.options)
      || question.options.length < 2 || question.options.length > MAX_OPTIONS) {
      return safeFailure(request, 'unsupported_question', this.modelRevision, this.name);
    }
    const optionIds = new Set<string>();
    const candidateById = new Map<string, Candidate>();
    for (const candidate of request.state.candidates) {
      if (candidateById.has(candidate.candidate_id)) return safeFailure(request, 'unsupported_question', this.modelRevision, this.name);
      candidateById.set(candidate.candidate_id, candidate);
    }
    for (const option of question.options) {
      if (!option.key || optionIds.has(option.key) || !candidateById.has(option.key)) {
        return safeFailure(request, 'unsupported_question', this.modelRevision, this.name);
      }
      const candidate = candidateById.get(option.key)!;
      if (candidate.precondition.state !== 'executable' || candidate.description !== option.description) {
        return safeFailure(request, 'refused', this.modelRevision, this.name);
      }
      optionIds.add(option.key);
    }
    if (typeof question.prompt !== 'string' || !question.prompt.trim()) {
      return safeFailure(request, 'unsupported_question', this.modelRevision, this.name);
    }
    const qBytes = Buffer.byteLength(question.prompt)
      + question.options.reduce((sum, option) => sum + Buffer.byteLength(option.key) + Buffer.byteLength(option.description), 0);
    if (qBytes > MAX_QUESTION_BYTES) return safeFailure(request, 'truncated', this.modelRevision, this.name);

    let stateText: string;
    try { stateText = serializeState(request.state); }
    catch { return safeFailure(request, 'parse_error', this.modelRevision, this.name); }
    if (Buffer.byteLength(stateText) > MAX_STATE_BYTES) return safeFailure(request, 'truncated', this.modelRevision, this.name);
    const criteria = Object.create(null) as Record<string, string>;
    for (const option of question.options) criteria[option.key] = option.description;
    const internalQuestion: InternalQ = { t: 'choice', ins: question.prompt, crit: criteria };
    let sequence: ReturnType<typeof buildSequence>;
    try { sequence = buildSequence(this.tokenizer, stateText, internalQuestion, MAX_INPUT_TOKENS, MAX_HEAD_TOKENS); }
    catch { return safeFailure(request, 'parse_error', this.modelRevision, this.name); }
    if (sequence.stats.truncated || sequence.markers.length !== question.options.length
      || sequence.markers.length < 2 || sequence.markers.length > MAX_MARKERS
      || sequence.stats.options.distinct !== question.options.length || sequence.stats.options.tokens_per_option !== null) {
      return safeFailure(request, 'truncated', this.modelRevision, this.name);
    }
    throwIfAborted(signal);

    const queueMs = Math.max(0, Math.round(performance.now() - enqueuedAt));
    const inferenceStarted = performance.now();
    const ids = BigInt64Array.from(sequence.ids, (value) => BigInt(value));
    const attention = BigInt64Array.from(sequence.ids, () => 1n);
    const encoded = await this.encoder.run({
      input_ids: new this.runtime.Tensor('int64', ids, [1, sequence.ids.length]),
      attention_mask: new this.runtime.Tensor('int64', attention, [1, sequence.ids.length]),
    });
    throwIfAborted(signal);
    if (this.closed) throw abortError();
    const hidden = encoded['last_hidden_state'];
    if (!hidden || !(hidden.data instanceof Float32Array)
      || !sameDims(hidden.dims, [1, sequence.ids.length, HIDDEN_SIZE]) || !allFinite(hidden.data)) {
      return safeFailure(request, 'parse_error', this.modelRevision, this.name);
    }
    const markerPos = BigInt64Array.from(sequence.markers, (value) => BigInt(value));
    const markerMask = new Uint8Array(sequence.markers.length).fill(1);
    const qtype = BigInt64Array.of(0n);
    const headOutput = await this.head.run({
      hidden_states: hidden,
      marker_pos: new this.runtime.Tensor('int64', markerPos, [1, markerPos.length]),
      marker_mask: new this.runtime.Tensor('bool', markerMask, [1, markerMask.length]),
      qtype: new this.runtime.Tensor('int64', qtype, [1, 1]),
      attention_mask: new this.runtime.Tensor('int64', attention, [1, sequence.ids.length]),
    });
    throwIfAborted(signal);
    if (this.closed) throw abortError();
    const logitsTensor = headOutput['logits'];
    const actionTensor = headOutput['act_logits'];
    if (!logitsTensor || !(logitsTensor.data instanceof Float32Array)
      || !sameDims(logitsTensor.dims, [1, sequence.markers.length]) || !allFinite(logitsTensor.data)
      || !actionTensor || !(actionTensor.data instanceof Float32Array)
      || !sameDims(actionTensor.dims, [1, 2]) || !allFinite(actionTensor.data)) {
      return safeFailure(request, 'parse_error', this.modelRevision, this.name);
    }
    const probabilities = softmax(Array.from(logitsTensor.data));
    const actionProbabilities = softmax(Array.from(actionTensor.data));
    if (!allFinite(probabilities) || !allFinite(actionProbabilities)) return safeFailure(request, 'parse_error', this.modelRevision, this.name);
    const confidence = answerConfidence(probabilities);
    if (Math.round(confidence * 1000) < this.minimumConfidenceMilli
      || actionProbabilities[0]! < MIN_ACTION_PROBABILITY) {
      return safeFailure(request, 'refused', this.modelRevision, this.name);
    }
    let selectedIndex = 0;
    for (let index = 1; index < probabilities.length; index++) {
      if (probabilities[index]! > probabilities[selectedIndex]!) selectedIndex = index;
    }
    const selected = question.options[selectedIndex];
    const candidate = selected ? candidateById.get(selected.key) : undefined;
    if (!selected || !candidate || candidate.precondition.state !== 'executable') {
      return safeFailure(request, 'refused', this.modelRevision, this.name);
    }
    const choice = policyChoice(candidate);
    return {
      decision_id: request.decision_id,
      request_generation: request.request_generation,
      candidate_set_digest: request.state.candidate_set_digest,
      backend: this.name,
      source: 's1',
      model_revision: this.modelRevision,
      answers: [{ question_id: question.id, selected: candidate.candidate_id, confidence_milli: Math.round(confidence * 1000) }],
      choice,
      failure: null,
      truncated: false,
      queue_ms: queueMs,
      inference_ms: Math.max(0, Math.round(performance.now() - inferenceStarted)),
    };
  }
}

function policyChoice(candidate: Candidate): PolicyChoice {
  switch (candidate.skill) {
    case 'observe_more': return { action: 'observe_more', facts: [candidate.description] };
    case 'request_planning': return { action: 'request_planning', reason: candidate.description };
    case 'stop_or_yield': return { action: 'stop_or_yield', reason: candidate.description };
    default: return { action: 'start_skill', candidate: candidate.candidate_id };
  }
}

function sameDims(actual: readonly number[], expected: readonly number[]): boolean {
  return actual.length === expected.length && actual.every((value, index) => value === expected[index]);
}

function allFinite(data: ArrayLike<number | bigint>): boolean {
  for (let index = 0; index < data.length; index++) {
    const value = Number(data[index]);
    if (!Number.isFinite(value)) return false;
  }
  return true;
}
