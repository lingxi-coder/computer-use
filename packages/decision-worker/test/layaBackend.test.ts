import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import type { Candidate, DecisionRequest } from '@computer-use/desktop-api';

import {
  createLayaCpuBackendFromSessions,
  LayaDecisionBackend,
  LayaBackendUnavailableError,
  layaUnavailableProposal,
  type OnnxRuntimeModule,
  type OnnxSession,
  type OnnxTensor,
} from '../src/laya/layaBackend.js';

function candidate(id: string, skill = 'ui.ensure_value'): Candidate {
  return {
    candidate_id: id,
    skill,
    semantic_target: null,
    binding: null,
    parameters: {},
    effect_class: 'reversible_edit',
    precondition: { state: 'executable' },
    description: `Select ${id}`,
  };
}

function request(change: Partial<DecisionRequest> = {}): DecisionRequest {
  const candidates = [candidate('c1'), candidate('c2')];
  return {
    decision_id: 'dec-laya',
    lane_id: 'lane-laya',
    request_generation: 3,
    root_task_id: 'root-laya',
    task_revision: 4,
    plan_revision: 2,
    root_epoch: 7,
    decision_program: 'ui.step@1',
    calibration_revision: 'none',
    state: {
      root_task_id: 'root-laya',
      task_revision: 4,
      goal_summary: 'choose one listed action',
      subgoal: 'select an action',
      snapshot_revision: 9,
      objects: [],
      candidates,
      recent_results: [],
      missing_fields: [],
      omitted_items: [],
      candidate_set_digest: 'cs1:laya-test',
    },
    questions: [{
      id: 'q1', kind: 'choice', prompt: 'Which listed action should run?',
      options: candidates.map(({ candidate_id, description }) => ({ key: candidate_id, description })),
    }],
    created_ms: 100,
    result_deadline_ms: 30_100,
    ...change,
  };
}

class Tensor implements OnnxTensor {
  constructor(
    readonly type: 'int64' | 'float32' | 'bool',
    readonly data: Float32Array | BigInt64Array | Uint8Array,
    readonly dims: readonly number[],
  ) {}
}

type Run = (feeds: Record<string, OnnxTensor>) => Promise<Record<string, OnnxTensor>>;

class Session implements OnnxSession {
  readonly seen: Record<string, OnnxTensor>[] = [];
  released = 0;

  constructor(
    readonly inputNames: readonly string[],
    readonly outputNames: readonly string[],
    private readonly handler: Run,
  ) {}

  async run(feeds: Record<string, OnnxTensor>): Promise<Record<string, OnnxTensor>> {
    this.seen.push(feeds);
    return this.handler(feeds);
  }

  release(): void { this.released += 1; }
}

function sessions(options: {
  logits?: number[];
  actionLogits?: number[];
  hiddenDims?: number[];
  logitsDims?: number[];
  onEncoder?: () => void;
  holdEncoder?: Promise<void>;
} = {}): { runtime: OnnxRuntimeModule; encoder: Session; head: Session } {
  let runtime!: OnnxRuntimeModule;
  class RuntimeTensor extends Tensor {}
  runtime = {
    Tensor: RuntimeTensor,
    InferenceSession: { create: async () => { throw new Error('not used by injected-session tests'); } },
  };
  const encoder = new Session(['input_ids', 'attention_mask'], ['last_hidden_state'], async (feeds) => {
    options.onEncoder?.();
    if (options.holdEncoder) await options.holdEncoder;
    const ids = feeds['input_ids']!;
    const seq = ids.dims[1]!;
    const hiddenDims = options.hiddenDims ?? [1, seq, 768];
    const hiddenLength = hiddenDims.reduce((a, b) => a * b, 1);
    return { last_hidden_state: new RuntimeTensor('float32', new Float32Array(hiddenLength).fill(0.25), hiddenDims) };
  });
  const head = new Session(
    ['hidden_states', 'marker_pos', 'marker_mask', 'qtype', 'attention_mask'],
    ['logits', 'act_logits'],
    async (feeds) => {
      const markers = feeds['marker_pos']!;
      const k = markers.dims[1]!;
      const logits = options.logits ?? [6, 0];
      return {
        logits: new RuntimeTensor('float32', Float32Array.from(logits), options.logitsDims ?? [1, k]),
        act_logits: new RuntimeTensor('float32', Float32Array.from(options.actionLogits ?? [3, 0]), [1, 2]),
      };
    },
  );
  return { runtime, encoder, head };
}

function backendFrom(options: Parameters<typeof sessions>[0] = {}) {
  const resources = sessions(options);
  const backend = createLayaCpuBackendFromSessions({ ...resources, tokenizer: {
    clsId: 2,
    sepId: 1,
    maskId: 4,
    padId: 0,
    maskToken: '[MASK]',
    encode: (text) => {
      const compact = text.trim();
      if (!compact) return [];
      let hash = 2_166_136_261;
      for (const character of compact) {
        hash ^= character.codePointAt(0)!;
        hash = Math.imul(hash, 16_777_619);
      }
      return [5 + ((hash >>> 0) % 200_000)];
    },
  } });
  return { ...resources, backend };
}

test('vendored preprocessing is byte-pinned and carries the upstream license', async () => {
  const testDir = dirname(fileURLToPath(import.meta.url));
  const vendorDir = join(testDir, '../src/laya/vendor/laya-ts');
  const gitBlobSha = async (path: string): Promise<string> => {
    const bytes = await readFile(path);
    return createHash('sha1').update(Buffer.concat([Buffer.from(`blob ${bytes.byteLength}\0`), bytes])).digest('hex');
  };
  assert.equal(await gitBlobSha(join(vendorDir, 'src/tokenizer.ts')), '3e460d0d4ba42b069b432a86e714b9b688e3cc59');
  assert.equal(await gitBlobSha(join(vendorDir, 'src/common.ts')), '38f170ed69a0614315bf57a30e22e2dd6090aea7');
  assert.match(await readFile(join(vendorDir, 'LICENSE'), 'utf8'), /Apache License\s+Version 2\.0/);

  const expectedRuntimeHashes = {
    'runtime/common.d.ts': '1c1cc9f1d8a5258f688a07feaf51a1761c53815bf9f42a147111a362ee3dd5ea',
    'runtime/common.js': 'b9f5f9d97e8da92a4bd9462a59f03296ef9b9e0680c3fc1e5ac7bf0f1c21f0fc',
    'runtime/tokenizer.d.ts': '8a5095f56c70f35716a54382584e4602ea366af54435613fad46e4ec35ecb94d',
    'runtime/tokenizer.js': '38244b897ee4d9e0411a6934ce7a2f474ea86794c28b4ba67ee72bb15f630615',
  };
  const pins = JSON.parse(await readFile(join(vendorDir, 'build-pins.json'), 'utf8')) as {
    runtime_sha256: Record<string, string>;
  };
  assert.deepEqual(pins.runtime_sha256, expectedRuntimeHashes);
  for (const [path, expected] of Object.entries(expectedRuntimeHashes)) {
    assert.equal(createHash('sha256').update(await readFile(join(vendorDir, path))).digest('hex'), expected, path);
  }
});

test('the backend builds exact pinned Laya encoder/head feeds and returns an S1 candidate proposal', async () => {
  const { backend, encoder, head } = backendFrom();
  try {
    const input = request();
    const proposal = await backend.decide(input, new AbortController().signal);
    assert.equal(proposal.source, 's1');
    assert.equal(proposal.decision_id, input.decision_id);
    assert.equal(proposal.request_generation, input.request_generation);
    assert.equal(proposal.candidate_set_digest, input.state.candidate_set_digest);
    assert.deepEqual(proposal.choice, { action: 'start_skill', candidate: 'c1' });
    assert.equal(proposal.answers[0]?.selected, 'c1');
    assert.ok((proposal.answers[0]?.confidence_milli ?? 0) >= 800);

    const encoderFeeds = encoder.seen[0]!;
    const ids = encoderFeeds['input_ids']!;
    const mask = encoderFeeds['attention_mask']!;
    assert.deepEqual(ids.dims, [1, ids.data.length]);
    assert.ok(ids.data instanceof BigInt64Array);
    assert.ok(mask.data instanceof BigInt64Array);
    assert.ok(Array.from(mask.data).every((value) => value === 1n));

    const headFeeds = head.seen[0]!;
    const markers = headFeeds['marker_pos']!;
    const markerMask = headFeeds['marker_mask']!;
    assert.deepEqual(headFeeds['hidden_states']!.dims, [1, ids.dims[1], 768]);
    assert.deepEqual(markers.dims, [1, 2]);
    assert.deepEqual(headFeeds['qtype']!.dims, [1, 1]);
    assert.deepEqual(headFeeds['attention_mask']!.dims, [1, ids.dims[1]]);
    assert.ok(markerMask.data instanceof Uint8Array);
    assert.deepEqual(Array.from(markerMask.data), [1, 1]);
    const flatIds = ids.data as BigInt64Array;
    for (const marker of markers.data as BigInt64Array) assert.equal(flatIds[Number(marker)], 4n);
  } finally { await backend.close(); }
});

test('unsupported questions, empty candidate lists and explicit low confidence never invent a choice', async () => {
  const { backend } = backendFrom({ logits: [0, 0] });
  try {
    const low = await backend.decide(request(), new AbortController().signal);
    assert.equal(low.failure, 'refused');
    assert.equal(low.choice, null);
    assert.deepEqual(low.answers, []);

    const unsupported = await backend.decide(request({ questions: [] }), new AbortController().signal);
    assert.equal(unsupported.failure, 'unsupported_question');
    assert.equal(unsupported.choice, null);

    const noCandidate = request();
    noCandidate.state.candidates = [];
    const empty = await backend.decide(noCandidate, new AbortController().signal);
    assert.equal(empty.failure, 'unsupported_question');
    assert.equal(empty.choice, null);
  } finally { await backend.close(); }
});

test('malformed graph output shapes and non-finite logits are refused', async () => {
  const wrongHidden = backendFrom({ hiddenDims: [1, 1, 768] });
  try {
    const malformed = await wrongHidden.backend.decide(request(), new AbortController().signal);
    assert.equal(malformed.failure, 'parse_error');
    assert.equal(malformed.choice, null);
    assert.equal(wrongHidden.head.seen.length, 0);
  } finally { await wrongHidden.backend.close(); }

  const nonFinite = backendFrom({ logits: [Number.NaN, 0] });
  try {
    const malformed = await nonFinite.backend.decide(request(), new AbortController().signal);
    assert.equal(malformed.failure, 'parse_error');
    assert.equal(malformed.choice, null);
  } finally { await nonFinite.backend.close(); }
});

test('external cancellation stops before the head and closed sessions cannot yield late results', async () => {
  const controller = new AbortController();
  const cancelled = backendFrom({ onEncoder: () => controller.abort() });
  const pending = cancelled.backend.decide(request(), controller.signal);
  await assert.rejects(pending, { name: 'AbortError' });
  assert.equal(cancelled.head.seen.length, 0);
  await cancelled.backend.close();

  let finishEncoder!: () => void;
  let enteredEncoder!: () => void;
  const entered = new Promise<void>((resolve) => { enteredEncoder = resolve; });
  const held = new Promise<void>((resolve) => { finishEncoder = resolve; });
  const closing = backendFrom({ onEncoder: enteredEncoder, holdEncoder: held });
  const late = closing.backend.decide(request(), new AbortController().signal);
  await entered;
  const closePromise = closing.backend.close();
  await assert.rejects(late, { name: 'AbortError' });
  assert.equal(closing.encoder.released, 0, 'the active native run must drain before its session is released');
  assert.equal(closing.head.released, 0, 'sessions remain owned until the active run drains');
  finishEncoder();
  await closePromise;
  assert.equal(closing.encoder.released, 1);
  assert.equal(closing.head.released, 1);
  await assert.rejects(closing.backend.decide(request(), new AbortController().signal), LayaBackendUnavailableError);
});

test('zero-deadline and missing-runtime reports use the S1 unavailable proposal shape', async () => {
  const input = request({ result_deadline_ms: 100 });
  const expired = backendFrom();
  try {
    const proposal = await expired.backend.decide(input, new AbortController().signal);
    assert.equal(proposal.source, 's1');
    assert.equal(proposal.failure, 'timeout');
    assert.equal(proposal.choice, null);
  } finally { await expired.backend.close(); }

  const unavailable = layaUnavailableProposal(request());
  assert.equal(unavailable.source, 's1');
  assert.equal(unavailable.failure, 'unavailable');
  assert.equal(unavailable.choice, null);
  assert.deepEqual(unavailable.answers, []);
});

test('sessions with an unexpected graph signature are rejected before use', () => {
  const { runtime, head } = sessions();
  const invalid = new Session(['input_ids'], ['last_hidden_state'], async () => ({}));
  assert.throws(() => createLayaCpuBackendFromSessions({
    runtime, encoder: invalid, head, tokenizer: {
      clsId: 2, sepId: 1, maskId: 4, padId: 0, maskToken: '[MASK]', encode: () => [],
    },
  }), LayaBackendUnavailableError);
});

const integrationManifestPath = process.env['COMPUTER_USE_LAYA_BUNDLE_MANIFEST'];
test('opt-in real pinned Node CPU backend loads, decides once, and closes both sessions', {
  skip: !integrationManifestPath && 'set COMPUTER_USE_LAYA_BUNDLE_MANIFEST to run the local ONNX integration check',
}, async () => {
  const actualRuntime = await import('onnxruntime-node');
  const runtime = actualRuntime as unknown as OnnxRuntimeModule;
  const inferenceCalls: string[] = [];
  const releases: string[] = [];
  const observedRuntime: OnnxRuntimeModule = {
    Tensor: runtime.Tensor,
    InferenceSession: {
      async create(path, options) {
        const session = await runtime.InferenceSession.create(path, options);
        const graph = path.endsWith('encoder.onnx') ? 'encoder' : 'head';
        return {
          inputNames: session.inputNames, outputNames: session.outputNames,
          async run(feeds) { inferenceCalls.push(graph); return session.run(feeds); },
          async release() { releases.push(graph); await session.release?.(); },
        };
      },
    },
  };
  const backend = await LayaDecisionBackend.create({ bundleManifestPath: integrationManifestPath!, numThreads: 2,
    runtime: observedRuntime });
  try {
    const input = request({ created_ms: 0, result_deadline_ms: 60_000 });
    const proposal = await backend.decide(input, new AbortController().signal);
    assert.equal(backend.modelRevision, 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67');
    assert.equal(proposal.source, 's1');
    assert.equal(proposal.decision_id, input.decision_id);
    assert.equal(proposal.request_generation, input.request_generation);
    assert.equal(proposal.candidate_set_digest, input.state.candidate_set_digest);
    assert.deepEqual(inferenceCalls, ['encoder', 'head'], 'both real CPU graphs must execute');
    assert.ok(proposal.failure === null || proposal.failure === 'refused');
    if (proposal.failure === null) {
      if (!proposal.choice || !('candidate' in proposal.choice)) assert.fail('a successful Laya decision must choose a candidate');
      assert.ok(['c1', 'c2'].includes(proposal.choice.candidate));
      assert.equal(proposal.answers[0]?.selected, proposal.choice.candidate);
    } else {
      assert.equal(proposal.choice, null);
      assert.deepEqual(proposal.answers, []);
    }
  } finally { await backend.close(); }
  assert.deepEqual(releases.sort(), ['encoder', 'head']);
});
