import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import type {
  BrowserActionIntent,
  BrowserExecutionObservation,
  BrowserPageSnapshot,
  BrowserReadScope,
  BrowserSourceControl,
  BrowserTargetContext,
} from '../src/index.js';
import {
  browserTargetContextHash,
  validateBrowserActionIntent,
  validateBrowserExecutionObservation,
  validateBrowserPageSnapshot,
  validateBrowserReadScope,
  validateBrowserSourceControl,
} from '../src/index.js';

function context(): BrowserTargetContext {
  return {
    browser_session_id: 'session-1',
    browser_context_id: 'context-1',
    page_target_id: 'target-1',
    cdp_session_id: 'cdp-1',
    frame_id: 'frame-1',
    document_loader_id: 'loader-1',
    fixture_file_url: 'file:///tmp/computer-use/fixtures/headless/readonly-page.html',
    session_epoch: 7,
  };
}

function readScope(): BrowserReadScope {
  return {
    scope_id: '550e8400-e29b-41d4-a716-446655440000',
    generation: 1,
    context: context(),
    clock_domain: 'headless-performance-monotonic-ms',
    issued_ms: 100,
  };
}

async function executionObservation(): Promise<BrowserExecutionObservation> {
  const targetContext = context();
  const scope: BrowserReadScope = {
    ...readScope(),
    context: targetContext,
  };
  const targetContextHash = await browserTargetContextHash(targetContext);
  return {
    observation_id: 'browser-snapshot-2',
    surface: { id: targetContext.page_target_id, epoch: targetContext.session_epoch },
    scope,
    environment_revision: 4,
    binding_ref: 'binding-background-option',
    binding: {
      snapshot_id: 'browser-snapshot-2',
      source_sequence: 2,
      observed_ms: 120,
      read_finished_ms: 125,
      clock_domain: scope.clock_domain,
      context: targetContext,
      target_context_hash: targetContextHash,
      ax_node_id: 'checkbox-ax-17',
      backend_dom_node_id: 17,
      role: 'checkbox',
      name: 'Background option',
      disabled: 'false',
      checked: false,
    },
    valid_until_ms: 200,
    freshness: 'verified',
    dependencies: { browser_document: 3 },
  };
}

async function browserIntent(): Promise<BrowserActionIntent> {
  const evidence = await executionObservation();
  return {
    intent_id: 'browser-intent-1',
    attempt_id: 'attempt-1',
    task_id: 'task-1',
    task_revision: 1,
    root_epoch: 1,
    session_epoch: evidence.scope.context.session_epoch,
    permit: { id: 'permit-1', generation: 1 },
    evidence,
    latest_start_ms: 130,
    duration_ms: 20,
    action: { type: 'set_checkbox', checked: true },
  };
}

async function snapshot(): Promise<BrowserPageSnapshot> {
  const targetContext = context();
  return {
    snapshot_id: 'snapshot-1',
    source_sequence: 1,
    observed_ms: 100,
    read_finished_ms: 110,
    clock_domain: 'browser-target-1',
    context: targetContext,
    target_context_hash: await browserTargetContextHash(targetContext),
    root_ax_node_id: 'ax-root',
    nodes: [
      {
        ax_node_id: 'ax-root',
        backend_dom_node_id: null,
        parent_ax_node_id: null,
        child_ax_node_ids: ['ax-checkbox'],
        ignored: false,
        role: 'RootWebArea',
        name: null,
        disabled: 'unknown',
        checked: 'unknown',
      },
      {
        ax_node_id: 'ax-checkbox',
        backend_dom_node_id: 17,
        parent_ax_node_id: 'ax-root',
        child_ax_node_ids: [],
        ignored: false,
        role: 'checkbox',
        name: '',
        disabled: 'false',
        checked: 'false',
      },
    ],
  };
}

function copy<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function rustGoldens(): BrowserPageSnapshot[] {
  return JSON.parse(readFileSync(new URL('../../../contracts/browser-v1/goldens.json', import.meta.url), 'utf8')) as BrowserPageSnapshot[];
}

test('browser target context hash matches the Rust tc1 golden', async () => {
  for (const golden of rustGoldens()) {
    assert.equal(await browserTargetContextHash(golden.context), golden.target_context_hash);
  }
});

test('browser page snapshot accepts a complete reciprocal tree and preserves unknown state', async () => {
  const value = await snapshot();
  const result = await validateBrowserPageSnapshot(value);
  assert.deepEqual(result, { ok: true, value });
  assert.equal(value.nodes[0].disabled, 'unknown');
  assert.equal(value.nodes[1]!.name, '');
  for (const golden of rustGoldens()) assert.equal((await validateBrowserPageSnapshot(golden)).ok, true);
});

test('browser snapshots reject a stale hash and changed session or document identity', async () => {
  const valid = await snapshot();
  const wrongHash = copy(valid);
  wrongHash.target_context_hash = `tc1:${'0'.repeat(64)}`;
  assert.equal((await validateBrowserPageSnapshot(wrongHash)).ok, false);

  for (const change of [
    (value: BrowserPageSnapshot) => { value.context.browser_session_id = 'foreign-session'; },
    (value: BrowserPageSnapshot) => { value.context.document_loader_id = 'foreign-document'; },
    (value: BrowserPageSnapshot) => { value.context.session_epoch += 1; },
  ]) {
    const foreign = copy(valid);
    change(foreign);
    assert.equal((await validateBrowserPageSnapshot(foreign)).ok, false);
  }
});

test('browser snapshot validator rejects unknown fields and UTF-8 byte overflows', async () => {
  const valid = await snapshot();
  assert.equal((await validateBrowserPageSnapshot({ ...valid, untrusted: true })).ok, false);

  const oversizedName = copy(valid);
  oversizedName.nodes[1]!.name = '😀'.repeat(129); // 516 UTF-8 bytes, below schema maxLength.
  assert.equal((await validateBrowserPageSnapshot(oversizedName)).ok, false);

  const oversizedIdentifier = context();
  oversizedIdentifier.browser_session_id = '😀'.repeat(65); // 260 UTF-8 bytes.
  await assert.rejects(browserTargetContextHash(oversizedIdentifier));
});

test('browser snapshot validator rejects malformed and disconnected page graphs', async () => {
  const valid = await snapshot();

  const danglingChild = copy(valid);
  danglingChild.nodes[0].child_ax_node_ids = ['missing'];
  assert.equal((await validateBrowserPageSnapshot(danglingChild)).ok, false);

  const mismatchedParent = copy(valid);
  mismatchedParent.nodes[1]!.parent_ax_node_id = null;
  assert.equal((await validateBrowserPageSnapshot(mismatchedParent)).ok, false);

  const duplicate = copy(valid);
  duplicate.nodes[1]!.ax_node_id = 'ax-root';
  assert.equal((await validateBrowserPageSnapshot(duplicate)).ok, false);

  const cycle = copy(valid);
  cycle.nodes[0].child_ax_node_ids = [];
  cycle.nodes[1]!.parent_ax_node_id = 'ax-loop';
  cycle.nodes[1]!.child_ax_node_ids = ['ax-loop'];
  cycle.nodes.push({
    ax_node_id: 'ax-loop',
    backend_dom_node_id: null,
    parent_ax_node_id: 'ax-checkbox',
    child_ax_node_ids: ['ax-checkbox'],
    ignored: true,
    role: null,
    name: null,
    disabled: 'unknown',
    checked: 'unknown',
  });
  assert.equal((await validateBrowserPageSnapshot(cycle)).ok, false);
});

test('browser snapshot validator enforces same-domain read bounds and safe integers', async () => {
  const valid = await snapshot();
  const overlapping = copy(valid);
  overlapping.read_finished_ms = overlapping.observed_ms - 1;
  assert.equal((await validateBrowserPageSnapshot(overlapping)).ok, false);

  const unsafe = copy(valid);
  unsafe.source_sequence = Number.MAX_SAFE_INTEGER + 1;
  assert.equal((await validateBrowserPageSnapshot(unsafe)).ok, false);
});

test('browser read scopes require canonical UUIDs, a fixed context and lossless clock values', () => {
  const valid = readScope();
  assert.deepEqual(validateBrowserReadScope(valid), { ok: true, value: valid });

  for (const scope_id of [
    '550E8400-e29b-41d4-a716-446655440000',
    '550e8400-e29b-01d4-a716-446655440000',
    '550e8400-e29b-41d4-c716-446655440000',
  ]) {
    assert.equal(validateBrowserReadScope({ ...valid, scope_id }).ok, false);
  }
  assert.equal(validateBrowserReadScope({ ...valid, generation: Number.MAX_SAFE_INTEGER + 1 }).ok, false);
  assert.equal(validateBrowserReadScope({ ...valid, issued_ms: 1.5 }).ok, false);
  assert.equal(validateBrowserReadScope({ ...valid, clock_domain: '雪'.repeat(43) }).ok, false);
  assert.equal(validateBrowserReadScope({ ...valid, clock_domain: ' \t ' }).ok, false);
  assert.equal(validateBrowserReadScope({ ...valid, clock_domain: 'clock\u0000' }).ok, false);
  assert.equal(validateBrowserReadScope({ ...valid, context: { ...valid.context, window_id: 'native' } }).ok, false);
  assert.equal(validateBrowserReadScope({ ...valid, context: { ...valid.context, fixture_file_url: 'https://example.invalid/page' } }).ok, false);
  assert.equal(validateBrowserReadScope({ ...valid, target_context_hash: `tc1:${'0'.repeat(64)}` }).ok, false);
  assert.equal(validateBrowserReadScope({ ...valid, input_authorized: true }).ok, false);
});

test('browser source control uses a closed discriminator and closed invalidation reasons', () => {
  const scope = readScope();
  const install: BrowserSourceControl = { kind: 'install', scope };
  assert.deepEqual(validateBrowserSourceControl(install), { ok: true, value: install });

  for (const reason of ['source_unavailable', 'document_changed', 'session_closed', 'stopped'] as const) {
    const invalidate: BrowserSourceControl = {
      kind: 'invalidate',
      scope_id: scope.scope_id,
      generation: 2,
      reason,
    };
    assert.deepEqual(validateBrowserSourceControl(invalidate), { ok: true, value: invalidate });
  }

  assert.equal(validateBrowserSourceControl({ kind: 'restart', scope }).ok, false);
  assert.equal(validateBrowserSourceControl({ kind: 'invalidate', scope_id: scope.scope_id,
    generation: 2, reason: 'resume_input' }).ok, false);
  assert.equal(validateBrowserSourceControl({ kind: 'invalidate', scope_id: scope.scope_id.toUpperCase(),
    generation: 2, reason: 'stopped' }).ok, false);
  assert.equal(validateBrowserSourceControl({ kind: 'invalidate', scope_id: scope.scope_id,
    generation: 0, reason: 'stopped' }).ok, false);
  assert.equal(validateBrowserSourceControl({ kind: 'install', scope, input_authorized: true }).ok, false);
});

test('scope and source-control wire limits count serde-compatible UTF-8 JSON bytes', () => {
  const suffix = '/fixtures/headless/readonly-page.html';
  const urlPrefix = 'file:///';
  const urlLength = 4_096;
  const fillLength = urlLength - urlPrefix.length - suffix.length;
  const withQuotes = (count: number): BrowserReadScope => ({
    ...readScope(),
    context: {
      ...context(),
      fixture_file_url: `${urlPrefix}${'"'.repeat(count)}${'a'.repeat(fillLength - count)}${suffix}`,
    },
  });
  const jsonBytes = (value: object): number => Buffer.byteLength(JSON.stringify(value)!, 'utf8');
  const base = withQuotes(0);
  const baseScopeBytes = jsonBytes(base);
  const baseControlBytes = jsonBytes({ kind: 'install', scope: base });
  const quoteCount = 8_192 - baseControlBytes + 1;
  assert.ok(quoteCount >= 0 && quoteCount <= fillLength);

  const nearLimit = withQuotes(quoteCount);
  const install: BrowserSourceControl = { kind: 'install', scope: nearLimit };
  assert.ok(jsonBytes(nearLimit) <= 8_192);
  assert.ok(jsonBytes(install) > 8_192);
  assert.equal(validateBrowserReadScope(nearLimit).ok, true);
  assert.equal(validateBrowserSourceControl(install).ok, false);

  const oversizedScope = withQuotes(8_192 - baseScopeBytes + 1);
  assert.ok(jsonBytes(oversizedScope) > 8_192);
  assert.equal(validateBrowserReadScope(oversizedScope).ok, false);
});

test('browser execution observations require the exact verified enabled checkbox scope', async () => {
  const valid = await executionObservation();
  assert.deepEqual(await validateBrowserExecutionObservation(valid), { ok: true, value: valid });

  const invalid: unknown[] = [];
  const changedSurface = copy(valid);
  changedSurface.surface.id = 'another-page';
  invalid.push(changedSurface);
  const changedObservationId = copy(valid);
  changedObservationId.observation_id = 'another-snapshot';
  invalid.push(changedObservationId);
  const foreignBinding = copy(valid);
  foreignBinding.binding.context.document_loader_id = 'foreign-document';
  invalid.push(foreignBinding);
  const staleHash = copy(valid);
  staleHash.binding.target_context_hash = `tc1:${'0'.repeat(64)}`;
  invalid.push(staleHash);
  const differentClock = copy(valid);
  differentClock.binding.clock_domain = 'other-clock';
  invalid.push(differentClock);
  const unknownDisabled = copy(valid);
  unknownDisabled.binding.disabled = 'unknown';
  invalid.push(unknownDisabled);
  const wrongName = copy(valid);
  wrongName.binding.name = 'Other option';
  invalid.push(wrongName);
  const unverified = copy(valid);
  unverified.freshness = 'uncertain';
  invalid.push(unverified);
  const missingRevision = copy(valid);
  missingRevision.environment_revision = 0;
  invalid.push(missingRevision);
  const beforeScope = copy(valid);
  beforeScope.scope.issued_ms = beforeScope.binding.observed_ms + 1;
  invalid.push(beforeScope);
  const beyondTtl = copy(valid);
  beyondTtl.valid_until_ms = beyondTtl.binding.observed_ms + 2_001;
  invalid.push(beyondTtl);
  const beforeReadFinished = copy(valid);
  beforeReadFinished.valid_until_ms = beforeReadFinished.binding.read_finished_ms - 1;
  invalid.push(beforeReadFinished);
  const tooManyDependencies = copy(valid);
  tooManyDependencies.dependencies = Object.fromEntries(Array.from({ length: 17 }, (_, index) => [`dep-${index}`, index]));
  invalid.push(tooManyDependencies);
  const unsafeDependency = copy(valid);
  unsafeDependency.dependencies.browser_document = Number.MAX_SAFE_INTEGER + 1;
  invalid.push(unsafeDependency);
  const oversizedBindingRef = copy(valid);
  oversizedBindingRef.binding_ref = '😀'.repeat(65);
  invalid.push(oversizedBindingRef);
  const oversizedDependencyName = copy(valid);
  oversizedDependencyName.dependencies = { ['😀'.repeat(17)]: 1 };
  invalid.push(oversizedDependencyName);
  const oversizedObservationId = copy(valid);
  oversizedObservationId.observation_id = '😀'.repeat(65);
  oversizedObservationId.binding.snapshot_id = oversizedObservationId.observation_id;
  invalid.push(oversizedObservationId);
  invalid.push({ ...valid, untrusted: true });

  for (const candidate of invalid) {
    assert.equal((await validateBrowserExecutionObservation(candidate)).ok, false);
  }
});

test('browser action intents fit a safe changed-value interval and exact session', async () => {
  const valid = await browserIntent();
  assert.deepEqual(await validateBrowserActionIntent(valid), { ok: true, value: valid });

  const invalid: unknown[] = [];
  const unchanged = copy(valid);
  unchanged.action.checked = false;
  invalid.push(unchanged);
  const early = copy(valid);
  early.latest_start_ms = early.evidence.binding.read_finished_ms - 1;
  invalid.push(early);
  const late = copy(valid);
  late.duration_ms = late.evidence.valid_until_ms - late.latest_start_ms + 1;
  invalid.push(late);
  const unsafeEnd = copy(valid);
  unsafeEnd.latest_start_ms = Number.MAX_SAFE_INTEGER;
  unsafeEnd.duration_ms = 1;
  invalid.push(unsafeEnd);
  const missingTaskRevision = copy(valid);
  missingTaskRevision.task_revision = 0;
  invalid.push(missingTaskRevision);
  const missingPermitGeneration = copy(valid);
  missingPermitGeneration.permit.generation = 0;
  invalid.push(missingPermitGeneration);
  const foreignSession = copy(valid);
  foreignSession.session_epoch += 1;
  invalid.push(foreignSession);
  const oversizedIntentId = copy(valid);
  oversizedIntentId.intent_id = '😀'.repeat(65);
  invalid.push(oversizedIntentId);
  const wrongEvidenceContext = copy(valid);
  wrongEvidenceContext.evidence.surface.epoch += 1;
  invalid.push(wrongEvidenceContext);
  invalid.push({ ...valid, debug: true });

  for (const candidate of invalid) assert.equal((await validateBrowserActionIntent(candidate)).ok, false);
});

test('browser execution byte bounds count canonical serde-compatible UTF-8 JSON', async () => {
  const valid = await executionObservation();
  const suffix = '/fixtures/headless/readonly-page.html';
  const quotedContext = {
    browser_session_id: '"'.repeat(256),
    browser_context_id: '"'.repeat(256),
    page_target_id: '"'.repeat(256),
    cdp_session_id: '"'.repeat(256),
    frame_id: '"'.repeat(256),
    document_loader_id: '"'.repeat(256),
    fixture_file_url: `file:///${'"'.repeat(2_300)}${suffix}`,
    session_epoch: 7,
  };
  const oversized = copy(valid);
  oversized.surface.id = quotedContext.page_target_id;
  oversized.scope.context = quotedContext;
  oversized.binding.context = quotedContext;
  oversized.binding.target_context_hash = await browserTargetContextHash(quotedContext);
  assert.equal((await validateBrowserExecutionObservation(oversized)).ok, false);
});
