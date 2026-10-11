import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test, afterEach } from 'node:test';

import type { DecisionProposal, DecisionRequest } from '@computer-use/desktop-api';
import type { Delivery } from '@computer-use/decision-worker';

import {
  decodeTeacherObservation,
  TEACHER_OBSERVATION_MAX_FRAME_BYTES,
  TeacherObservationDecoder,
  TeacherObservationProtocolError,
  TeacherShadowObserver,
  type TeacherObservation,
  type TeacherObservationScope,
  type TeacherShadowRunner,
} from '../src/main/teacherShadowObserver.js';

const observers = new Set<TeacherShadowObserver>();

afterEach(async () => {
  await Promise.all([...observers].map((observer) => observer.close()));
  observers.clear();
});

function request(change: Partial<DecisionRequest> = {}): DecisionRequest {
  const candidate = {
    candidate_id: 'candidate-1',
    skill: 'ui.ensure_value',
    semantic_target: null,
    binding: null,
    parameters: {},
    effect_class: 'reversible_edit' as const,
    precondition: { state: 'executable' as const },
    description: 'set the value to off',
  };
  return {
    decision_id: 'decision-1',
    lane_id: 'lane-1',
    request_generation: 1,
    root_task_id: 'root-1',
    task_revision: 1,
    plan_revision: 1,
    root_epoch: 1,
    decision_program: 'ui.step@1',
    calibration_revision: 'none',
    state: {
      root_task_id: 'root-1',
      task_revision: 1,
      goal_summary: 'choose a listed action',
      subgoal: 'select a value',
      snapshot_revision: 1,
      objects: [],
      candidates: [candidate],
      recent_results: [],
      missing_fields: [],
      omitted_items: [],
      candidate_set_digest: 'cs1:request-1',
    },
    questions: [{
      id: 'question-1',
      kind: 'choice',
      prompt: 'Which action should run?',
      options: [{ key: candidate.candidate_id, description: candidate.description }],
    }],
    created_ms: 100,
    result_deadline_ms: 10_000,
    ...change,
  };
}

function scope(input: DecisionRequest, overrides: Partial<TeacherObservationScope> = {}): TeacherObservationScope {
  return {
    decision_id: input.decision_id,
    request_generation: input.request_generation,
    root_task_id: input.root_task_id,
    root_epoch: input.root_epoch,
    task_revision: input.task_revision,
    candidate_set_digest: input.state.candidate_set_digest,
    ...overrides,
  };
}

function proposal(
  input: DecisionRequest,
  source: 'teacher' | 's1',
  overrides: Partial<DecisionProposal> = {},
): DecisionProposal {
  return {
    decision_id: input.decision_id,
    request_generation: input.request_generation,
    candidate_set_digest: input.state.candidate_set_digest,
    backend: source === 'teacher' ? 'teacher-fixture' : 'laya-fixture',
    source,
    model_revision: 'fixture-v1',
    answers: input.questions.map((question) => ({
      question_id: question.id,
      selected: question.kind === 'choice' ? question.options[0]?.key ?? null : null,
      confidence_milli: source === 'teacher' ? 720 : 960,
    })),
    choice: { action: 'start_skill', candidate: 'candidate-1' },
    failure: null,
    truncated: false,
    queue_ms: 1,
    inference_ms: 4,
    ...overrides,
  };
}

function started(input: DecisionRequest): TeacherObservation {
  return { version: 1, type: 'started', request: input };
}

function finished(input: DecisionRequest, value: DecisionProposal, overrideScope: Partial<TeacherObservationScope> = {}): TeacherObservation {
  return { version: 1, type: 'finished', scope: scope(input, overrideScope), proposal: value };
}

function frame(value: unknown): Buffer {
  const body = Buffer.from(JSON.stringify(value), 'utf8');
  const output = Buffer.allocUnsafe(4 + body.length);
  output.writeUInt32BE(body.length, 0);
  body.copy(output, 4);
  return output;
}

function concatenate(parts: readonly Uint8Array[]): Buffer {
  return Buffer.concat(parts.map((part) => Buffer.from(part)));
}

class FakeRunner implements TeacherShadowRunner {
  readonly submitted: { request: DecisionRequest; resolve: (delivery: Delivery) => void }[] = [];
  readonly cancelled: { decision_id: string; request_generation: number }[] = [];
  invalidations = 0;
  closes = 0;
  holdClose = false;

  submit(input: DecisionRequest): Promise<Delivery> {
    return new Promise((resolve) => this.submitted.push({ request: input, resolve }));
  }

  cancel(value: { decision_id: string; request_generation: number }): void { this.cancelled.push(value); }
  invalidate(): number { this.invalidations += 1; return this.invalidations; }
  close(): Promise<void> {
    this.closes += 1;
    return this.holdClose ? new Promise<void>(() => undefined) : Promise.resolve();
  }

  resolve(index: number, value: DecisionProposal): void {
    const pending = this.submitted[index];
    assert.ok(pending, `submission ${index} exists`);
    pending.resolve({ kind: 'proposal', proposal: value });
  }
}

function observeWith(runner: FakeRunner, nowMs: () => number = () => 500): TeacherShadowObserver {
  const observer = new TeacherShadowObserver(runner, { nowMs, deadlinePollMs: 5, closeTimeoutMs: 5 });
  observers.add(observer);
  return observer;
}

async function turn(): Promise<void> { await new Promise<void>((resolve) => setTimeout(resolve, 0)); }

test('Rust golden messages decode and incremental split/concatenated frames preserve their exact types', async () => {
  const fixturePath = new URL('../../../contracts/teacher-observation-v1/fixtures.json', import.meta.url);
  const fixture = JSON.parse(await readFile(fixturePath, 'utf8')) as { version: number; messages: unknown[] };
  assert.equal(fixture.version, 1);
  assert.equal(fixture.messages.length, 3);
  const decoded = fixture.messages.map(decodeTeacherObservation);
  assert.deepEqual(decoded.map((message) => message.type), ['started', 'finished', 'cancelled']);

  const bytes = concatenate(decoded.map((message) => frame(message)));
  const decoder = new TeacherObservationDecoder();
  const received: TeacherObservation[] = [];
  for (let offset = 0; offset < bytes.length;) {
    const count = Math.min(7, bytes.length - offset);
    received.push(...decoder.push(bytes.subarray(offset, offset + count)));
    offset += count;
  }
  assert.deepEqual(received, decoded);
  decoder.dispose();
});

test('decoder permits a total frame exactly at 128 KiB and poisons on one byte over', () => {
  const message = started(request());
  const encoded = Buffer.from(JSON.stringify(message), 'utf8');
  const maxBody = TEACHER_OBSERVATION_MAX_FRAME_BYTES - 4;
  assert.ok(encoded.byteLength < maxBody);
  const body = Buffer.concat([encoded, Buffer.alloc(maxBody - encoded.byteLength, 0x20)]);
  const atLimit = Buffer.allocUnsafe(4 + body.byteLength);
  atLimit.writeUInt32BE(body.byteLength, 0); body.copy(atLimit, 4);
  const decoder = new TeacherObservationDecoder();
  assert.equal(decoder.push(atLimit).length, 1);
  decoder.dispose();

  const over = Buffer.alloc(4);
  over.writeUInt32BE(maxBody + 1, 0);
  const rejected = new TeacherObservationDecoder();
  assert.throws(() => rejected.push(over), TeacherObservationProtocolError);
  assert.throws(() => rejected.push(Buffer.alloc(1)), TeacherObservationProtocolError);
  rejected.dispose();
});

test('decoder retires its stream on malformed JSON, invalid UTF-8, empty frames, or an overfull batch', () => {
  for (const body of [Buffer.alloc(0), Buffer.from('{'), Buffer.from([0xff])]) {
    const bytes = Buffer.allocUnsafe(4 + body.byteLength);
    bytes.writeUInt32BE(body.byteLength, 0); body.copy(bytes, 4);
    const decoder = new TeacherObservationDecoder();
    assert.throws(() => decoder.push(bytes), TeacherObservationProtocolError);
    decoder.dispose();
  }

  const one = frame(started(request()));
  const batch = Buffer.concat(Array.from({ length: 17 }, () => one));
  const decoder = new TeacherObservationDecoder();
  assert.throws(() => decoder.push(batch), TeacherObservationProtocolError);
  decoder.dispose();
});

test('strict decoding rejects unknown fields, mismatched proposal scope, and invalid cancellation reasons', () => {
  const input = request();
  assert.throws(() => decodeTeacherObservation({ ...started(input), private_data: 'not allowed' }), TeacherObservationProtocolError);
  assert.throws(() => decodeTeacherObservation(finished(input, proposal(input, 's1'))), TeacherObservationProtocolError);
  assert.throws(() => decodeTeacherObservation(finished(input, proposal(input, 'teacher'), {
    candidate_set_digest: 'cs1:foreign',
  })), TeacherObservationProtocolError);
  assert.throws(() => decodeTeacherObservation({
    version: 1, type: 'cancelled', scope: scope(input), reason: 'retry',
  }), TeacherObservationProtocolError);
});

test('teacher and shadow results verify in either arrival order, ignoring only confidence', async () => {
  const input = request();
  const firstRunner = new FakeRunner();
  const first = observeWith(firstRunner);
  first.observe(started(input));
  first.observe(finished(input, proposal(input, 'teacher')));
  firstRunner.resolve(0, proposal(input, 's1'));
  await turn();
  assert.equal(first.statistics.verified, 1);
  assert.equal(first.statistics.disagreements, 0);

  const secondRunner = new FakeRunner();
  const second = observeWith(secondRunner);
  second.observe(started(input));
  secondRunner.resolve(0, proposal(input, 's1', { answers: [{ question_id: 'question-1', selected: 'candidate-1', confidence_milli: 1000 }] }));
  await turn();
  assert.equal(second.statistics.verified, 0);
  second.observe(finished(input, proposal(input, 'teacher', { answers: [{ question_id: 'question-1', selected: 'candidate-1', confidence_milli: 100 }] })));
  assert.equal(second.statistics.verified, 1);
  assert.equal(second.statistics.disagreements, 0);

  const differentRunner = new FakeRunner();
  const different = observeWith(differentRunner);
  different.observe(started(input));
  different.observe(finished(input, proposal(input, 'teacher')));
  differentRunner.resolve(0, proposal(input, 's1', { choice: { action: 'observe_more', facts: ['take a fresh observation'] } }));
  await turn();
  assert.equal(different.statistics.verified, 1);
  assert.equal(different.statistics.disagreements, 1);
});

test('cancel and root or epoch transitions fence old work and never let late completions verify', async () => {
  const runner = new FakeRunner();
  const observer = observeWith(runner);
  const old = request();
  observer.observe(started(old));
  observer.observe({ version: 1, type: 'cancelled', scope: scope(old), reason: 'cancelled' });
  runner.resolve(0, proposal(old, 's1'));
  await turn();
  observer.observe(finished(old, proposal(old, 'teacher')));
  assert.equal(observer.statistics.verified, 0);
  assert.equal(runner.cancelled.length, 1);

  const nextRoot = request({
    decision_id: 'decision-2', root_task_id: 'root-2', root_epoch: 1,
    state: { ...old.state, root_task_id: 'root-2' },
  });
  observer.observe(started(nextRoot));
  assert.equal(runner.invalidations, 1);
  observer.observe(finished(old, proposal(old, 'teacher')));
  observer.observe(finished(nextRoot, proposal(nextRoot, 'teacher')));
  runner.resolve(1, proposal(nextRoot, 's1'));
  await turn();
  assert.equal(observer.statistics.verified, 1);

  const nextEpoch = request({
    decision_id: 'decision-3', root_task_id: 'root-2', root_epoch: 2,
    state: { ...nextRoot.state },
  });
  observer.observe(started(nextEpoch));
  assert.equal(runner.invalidations, 2);
  observer.observe(finished(nextRoot, proposal(nextRoot, 'teacher')));
  runner.resolve(2, proposal(nextEpoch, 's1'));
  observer.observe(finished(nextEpoch, proposal(nextEpoch, 'teacher')));
  await turn();
  assert.equal(observer.statistics.verified, 2);

  const nextRevision = request({
    decision_id: 'decision-4', root_task_id: 'root-2', root_epoch: 2, task_revision: 2,
    state: { ...nextEpoch.state, task_revision: 2 },
  });
  observer.observe(started(nextRevision));
  assert.equal(runner.invalidations, 3);
  observer.observe(finished(nextRevision, proposal(nextRevision, 'teacher')));
  runner.resolve(3, proposal(nextRevision, 's1'));
  await turn();
  assert.equal(observer.statistics.verified, 3);
});

test('deadline and clock failures retire shadow tasks without throwing into the host', async () => {
  let now = 500;
  const runner = new FakeRunner();
  const observer = observeWith(runner, () => now);
  const input = request({ result_deadline_ms: 510 });
  observer.observe(started(input));
  now = 510;
  await new Promise((resolve) => setTimeout(resolve, 15));
  runner.resolve(0, proposal(input, 's1'));
  observer.observe(finished(input, proposal(input, 'teacher')));
  await turn();
  assert.equal(observer.statistics.expired, 1);
  assert.equal(observer.statistics.verified, 0);

  const clockFailureRunner = new FakeRunner();
  const clockFailure = observeWith(clockFailureRunner, () => { throw new Error('clock unavailable'); });
  assert.doesNotThrow(() => clockFailure.observe(started(request())));
  assert.equal(clockFailureRunner.submitted.length, 0);
  assert.equal(clockFailure.statistics.unavailable, 1);
});

test('foreign candidates, answers and invalid confidence cannot be counted as verified', async () => {
  const input = request();
  const runner = new FakeRunner();
  const observer = observeWith(runner);
  observer.observe(started(input));
  observer.observe(finished(input, proposal(input, 'teacher', {
    choice: { action: 'start_skill', candidate: 'foreign-candidate' },
  })));
  runner.resolve(0, proposal(input, 's1'));
  await turn();
  assert.equal(observer.statistics.verified, 0);
  assert.equal(observer.statistics.dropped, 1);

  const confidenceRunner = new FakeRunner();
  const confidenceObserver = observeWith(confidenceRunner);
  confidenceObserver.observe(started(input));
  confidenceObserver.observe(finished(input, proposal(input, 'teacher', {
    answers: [{ question_id: 'question-1', selected: 'candidate-1', confidence_milli: 1001 }],
  })));
  confidenceRunner.resolve(0, proposal(input, 's1'));
  await turn();
  assert.equal(confidenceObserver.statistics.verified, 0);
  assert.equal(confidenceObserver.statistics.dropped, 1);
});

test('four in-flight requests is the hard task bound and statistics contain only fixed counters', () => {
  const runner = new FakeRunner();
  const observer = observeWith(runner);
  for (let index = 1; index <= 5; index += 1) {
    const input = request({ decision_id: `decision-${index}` });
    observer.observe(started(input));
  }
  assert.equal(runner.submitted.length, 4);
  assert.equal(observer.statistics.in_flight, 4);
  assert.equal(observer.statistics.refused, 1);
  assert.deepEqual(Object.keys(observer.statistics).sort(), [
    'disagreements', 'dropped', 'expired', 'in_flight', 'refused', 'started', 'unavailable', 'verified',
  ]);
  const rendered = JSON.stringify(observer.statistics);
  assert.equal(rendered.includes('root-1'), false);
  assert.equal(rendered.includes('candidate-1'), false);
  assert.equal(rendered.includes('cs1:request-1'), false);
});

test('close is idempotent and bounded even when the shadow runner close never settles', async () => {
  const runner = new FakeRunner();
  runner.holdClose = true;
  const observer = observeWith(runner);
  observer.observe(started(request()));
  const first = observer.close();
  assert.equal(observer.close(), first);
  await first;
  runner.resolve(0, proposal(request(), 's1'));
  await turn();
  assert.equal(observer.statistics.verified, 0);
  assert.equal(runner.closes, 1);
});
