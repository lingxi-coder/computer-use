import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import {
  validateBrowserPageSnapshot,
  type BrowserPageSnapshot,
} from '@computer-use/desktop-api';

import type { HeadlessCheckboxState, HeadlessObservation } from '../src/main/headless/cdpPipe.js';
import {
  createBrowserPageSnapshot,
  HEADLESS_OBSERVATION_CLOCK_DOMAIN,
  type HeadlessPageClockMapping,
} from '../src/main/headless/pageSource.js';

const REPLAY_PATH = fileURLToPath(new URL(
  './fixtures/headless-readonly-observation.json',
  import.meta.url,
));
const replay = JSON.parse(readFileSync(REPLAY_PATH, 'utf8')) as {
  provenance: {
    browserSessionId: string;
    browserContextId: string;
    targetId: string;
    pageId: string;
    sessionId: string;
    sequence: number;
  };
  document: HeadlessObservation['document'];
  rawAX: HeadlessObservation['rawAX'];
  layoutMetrics: HeadlessObservation['layoutMetrics'];
};

/** Replays real archived AX nodes with explicitly synthetic timing metadata. */
function replayObservation(): HeadlessObservation {
  return {
    provenance: {
      ...replay.provenance,
      clockDomain: HEADLESS_OBSERVATION_CLOCK_DOMAIN,
      readStartedMonotonicMs: 770.01775,
      readFinishedMonotonicMs: 771.4025,
    },
    document: replay.document,
    rawAX: structuredClone(replay.rawAX),
    layoutMetrics: replay.layoutMetrics,
  } as HeadlessObservation;
}

function clockMapping(overrides: Partial<HeadlessPageClockMapping> = {}): HeadlessPageClockMapping {
  return {
    sourceClockDomain: HEADLESS_OBSERVATION_CLOCK_DOMAIN,
    targetClockDomain: 'test-session-logical-ms',
    sessionEpoch: 7,
    mapMonotonicMs: (value) => value,
    ...overrides,
  };
}

function sourceOptions(observation: HeadlessObservation, clock = clockMapping()) {
  return { fixtureFileUrl: observation.document.url, ...clock };
}

function nativeCheckboxState(overrides: Partial<HeadlessCheckboxState> = {}): HeadlessCheckboxState {
  return {
    backendDOMNodeId: 17,
    disabled: false,
    checked: false,
    indeterminate: false,
    documentUrl: replay.document.url,
    connected: true,
    topDocument: true,
    nativeCheckbox: true,
    blockedByAriaOrInert: false,
    ...overrides,
  };
}

function withCheckboxStates(
  observation: HeadlessObservation,
  checkboxStates: readonly HeadlessCheckboxState[],
): HeadlessObservation {
  return { ...observation, checkboxStates };
}

function setAXBoolean(node: HeadlessObservation['rawAX']['nodes'][number], name: 'disabled' | 'checked', value: boolean): void {
  const properties = (node.properties ?? []).filter((property) => property.name !== name);
  (node as { properties?: readonly unknown[] }).properties = [
    ...properties,
    { name, value: { type: 'boolean', value } },
  ];
}

test('typed page snapshot preserves the complete archived AX tree and read interval', async () => {
  const observation = replayObservation();
  const snapshot = await createBrowserPageSnapshot(observation, sourceOptions(observation));

  assert.equal(snapshot.source_sequence, replay.provenance.sequence);
  assert.equal(snapshot.observed_ms, 770);
  assert.equal(snapshot.read_finished_ms, 772);
  assert.equal(snapshot.clock_domain, 'test-session-logical-ms');
  assert.equal(snapshot.context.browser_session_id, replay.provenance.browserSessionId);
  assert.equal(snapshot.context.browser_context_id, replay.provenance.browserContextId);
  assert.equal(snapshot.context.page_target_id, replay.provenance.targetId);
  assert.equal(snapshot.context.cdp_session_id, replay.provenance.sessionId);
  assert.equal(snapshot.context.frame_id, replay.document.frameId);
  assert.equal(snapshot.context.document_loader_id, replay.document.loaderId);
  assert.equal(snapshot.context.fixture_file_url, replay.document.url);
  assert.equal(snapshot.context.session_epoch, 7);
  assert.equal(snapshot.root_ax_node_id, '6');
  assert.equal(snapshot.nodes.length, replay.rawAX.nodes.length);
  assert.deepEqual(snapshot.nodes.map((node) => node.ax_node_id), replay.rawAX.nodes.map((node) => node.nodeId));
  assert.ok(snapshot.nodes.some((node) => node.ignored), 'ignored AX nodes remain in the source tree');
  assert.deepEqual(snapshot.nodes[0]?.child_ax_node_ids, replay.rawAX.nodes[0]?.childIds);

  const unchecked = snapshot.nodes.find((node) => node.ax_node_id === '17');
  const disabled = snapshot.nodes.find((node) => node.ax_node_id === '18');
  assert.equal(unchecked?.checked, 'false');
  assert.equal(disabled?.disabled, 'true');
  assert.equal(disabled?.checked, 'unknown');
  assert.equal(snapshot.nodes.find((node) => node.ax_node_id === '15')?.name, '');
  assert.ok(snapshot.nodes.some((node) => node.backend_dom_node_id === null));
  assert.ok(snapshot.nodes.some((node) => node.parent_ax_node_id === null));

  const second = await createBrowserPageSnapshot(observation, sourceOptions(observation));
  assert.notEqual(second.snapshot_id, snapshot.snapshot_id, 'each conversion is a distinct read snapshot');
});

test('page source rejects non-reciprocal parent references, duplicate IDs, and dangling children', async (t) => {
  const cases: Array<[string, (observation: HeadlessObservation) => void]> = [
    ['wrong parent', (observation) => {
      const node = observation.rawAX.nodes.find((item) => item.nodeId === '14')!;
      (node as { parentId?: string }).parentId = '15';
    }],
    ['duplicate node ID', (observation) => {
      const nodes = observation.rawAX.nodes as unknown as Array<{ nodeId: string }>;
      nodes[2]!.nodeId = nodes[1]!.nodeId;
    }],
    ['dangling child', (observation) => {
      const root = observation.rawAX.nodes.find((item) => item.nodeId === '6')!;
      (root as { childIds?: readonly string[] }).childIds = [...(root.childIds ?? []), 'missing-child'];
    }],
    ['second root', (observation) => {
      const root = observation.rawAX.nodes.find((item) => item.nodeId === '6')!;
      const secondRoot = observation.rawAX.nodes.find((item) => item.nodeId === '7')!;
      (root as { childIds?: readonly string[] }).childIds = (root.childIds ?? []).filter((id) => id !== '7');
      (secondRoot as { parentId?: string }).parentId = undefined;
    }],
    ['disconnected cycle', (observation) => {
      const nodes = observation.rawAX.nodes as unknown as Array<Record<string, unknown>>;
      nodes.push(
        { nodeId: 'cycle-a', ignored: true, parentId: 'cycle-b', childIds: ['cycle-b'] },
        { nodeId: 'cycle-b', ignored: true, parentId: 'cycle-a', childIds: ['cycle-a'] },
      );
    }],
  ];

  for (const [name, mutate] of cases) {
    await t.test(name, async () => {
      const observation = replayObservation();
      mutate(observation);
      await assert.rejects(createBrowserPageSnapshot(observation, sourceOptions(observation)));
    });
  }
});

test('malformed disabled data is rejected instead of being normalized to unknown', async () => {
  const observation = replayObservation();
  const node = observation.rawAX.nodes.find((item) => item.nodeId === '18')!;
  (node as { properties?: readonly unknown[] }).properties = [
    { name: 'disabled', value: { type: 'string', value: 'false' } },
  ];

  await assert.rejects(createBrowserPageSnapshot(observation, sourceOptions(observation)));
});

test('missing disabled and tristate mixed remain unknown', async () => {
  const observation = replayObservation();
  const disabledButton = observation.rawAX.nodes.find((item) => item.nodeId === '18')!;
  (disabledButton as { properties?: readonly unknown[] }).properties = [];
  const checkbox = observation.rawAX.nodes.find((item) => item.nodeId === '17')!;
  (checkbox as { properties?: readonly unknown[] }).properties = [
    { name: 'checked', value: { type: 'tristate', value: 'mixed' } },
  ];

  const snapshot = await createBrowserPageSnapshot(observation, sourceOptions(observation));
  assert.equal(snapshot.nodes.find((node) => node.ax_node_id === '18')?.disabled, 'unknown');
  assert.equal(snapshot.nodes.find((node) => node.ax_node_id === '17')?.disabled, 'unknown');
  assert.equal(snapshot.nodes.find((node) => node.ax_node_id === '17')?.checked, 'unknown');
});

test('native checkbox read fills unknown AX state and agrees with known AX state', async () => {
  const observed = replayObservation();
  const observedNode = observed.rawAX.nodes.find((node) => node.nodeId === '17')!;
  (observedNode as { properties?: readonly unknown[] }).properties =
    observedNode.properties?.filter((property) => property.name !== 'checked');
  const withState = withCheckboxStates(observed, [nativeCheckboxState({ checked: true })]);
  const snapshot = await createBrowserPageSnapshot(withState, sourceOptions(withState));
  assert.equal(snapshot.nodes.find((node) => node.ax_node_id === '17')?.checked, 'true');
  assert.equal(snapshot.nodes.find((node) => node.ax_node_id === '17')?.disabled, 'false');

  const knownFalse = replayObservation();
  const falseRead = withCheckboxStates(knownFalse, [nativeCheckboxState()]);
  const falseSnapshot = await createBrowserPageSnapshot(falseRead, sourceOptions(falseRead));
  assert.equal(falseSnapshot.nodes.find((node) => node.ax_node_id === '17')?.checked, 'false');
  assert.equal(falseSnapshot.nodes.find((node) => node.ax_node_id === '17')?.disabled, 'false');

  const conflict = replayObservation();
  const conflictRead = withCheckboxStates(conflict, [nativeCheckboxState({ checked: true })]);
  await assert.rejects(createBrowserPageSnapshot(conflictRead, sourceOptions(conflictRead)));
});

test('indeterminate native checkbox state remains unknown and conflicts with a known AX boolean', async () => {
  const mixedAx = replayObservation();
  const checkbox = mixedAx.rawAX.nodes.find((node) => node.nodeId === '17')!;
  (checkbox as { properties?: readonly unknown[] }).properties = [
    { name: 'checked', value: { type: 'tristate', value: 'mixed' } },
  ];
  const indeterminateRead = withCheckboxStates(mixedAx, [nativeCheckboxState({
    checked: false,
    indeterminate: true,
  })]);
  const snapshot = await createBrowserPageSnapshot(indeterminateRead, sourceOptions(indeterminateRead));
  assert.equal(snapshot.nodes.find((node) => node.ax_node_id === '17')?.checked, 'unknown');

  const knownAx = replayObservation();
  const conflict = withCheckboxStates(knownAx, [nativeCheckboxState({ indeterminate: true })]);
  await assert.rejects(createBrowserPageSnapshot(conflict, sourceOptions(conflict)));
});

test('native checkbox disabled and ARIA or inert states stay conservative', async () => {
  const unknownAx = replayObservation();
  const fieldsetRead = withCheckboxStates(unknownAx, [nativeCheckboxState({ disabled: true })]);
  const fieldsetSnapshot = await createBrowserPageSnapshot(fieldsetRead, sourceOptions(fieldsetRead));
  assert.equal(fieldsetSnapshot.nodes.find((node) => node.ax_node_id === '17')?.disabled, 'true');

  const ariaBlocked = replayObservation();
  const checkbox = ariaBlocked.rawAX.nodes.find((node) => node.nodeId === '17')!;
  setAXBoolean(checkbox, 'disabled', false);
  const blockedRead = withCheckboxStates(ariaBlocked, [nativeCheckboxState({ blockedByAriaOrInert: true })]);
  const blockedSnapshot = await createBrowserPageSnapshot(blockedRead, sourceOptions(blockedRead));
  assert.equal(blockedSnapshot.nodes.find((node) => node.ax_node_id === '17')?.disabled, 'true');

  const ariaReflected = replayObservation();
  const reflectedCheckbox = ariaReflected.rawAX.nodes.find((node) => node.nodeId === '17')!;
  setAXBoolean(reflectedCheckbox, 'disabled', true);
  const reflectedRead = withCheckboxStates(ariaReflected, [nativeCheckboxState({ blockedByAriaOrInert: true })]);
  const reflectedSnapshot = await createBrowserPageSnapshot(reflectedRead, sourceOptions(reflectedRead));
  assert.equal(reflectedSnapshot.nodes.find((node) => node.ax_node_id === '17')?.disabled, 'true');
});

test('native checkbox state rejects conflicting disabled evidence', async (t) => {
  const cases: Array<[string, boolean, boolean]> = [
    ['AX enabled versus natively disabled', false, true],
    ['AX disabled versus unblocked native enabled', true, false],
  ];
  for (const [name, axDisabled, runtimeDisabled] of cases) {
    await t.test(name, async () => {
      const observation = replayObservation();
      setAXBoolean(observation.rawAX.nodes.find((node) => node.nodeId === '17')!, 'disabled', axDisabled);
      const state = nativeCheckboxState({ disabled: runtimeDisabled });
      const withState = withCheckboxStates(observation, [state]);
      await assert.rejects(createBrowserPageSnapshot(withState, sourceOptions(withState)));
    });
  }
});

test('native checkbox state rejects document and AX-node mismatches', async (t) => {
  const cases: Array<[string, Partial<HeadlessCheckboxState>]> = [
    ['foreign document', { documentUrl: 'file:///other/fixtures/headless/readonly-page.html' }],
    ['unmatched backend node', { backendDOMNodeId: 99 }],
    ['disconnected node', { connected: false }],
    ['nested frame node', { topDocument: false }],
    ['custom non-native checkbox', { nativeCheckbox: false }],
  ];
  for (const [name, change] of cases) {
    await t.test(name, async () => {
      const observation = replayObservation();
      const withState = withCheckboxStates(observation, [nativeCheckboxState(change)]);
      await assert.rejects(createBrowserPageSnapshot(withState, sourceOptions(withState)));
    });
  }

  await t.test('duplicate AX backend DOM ID', async () => {
    const observation = replayObservation();
    const duplicate = observation.rawAX.nodes.find((node) => node.nodeId === '14')!;
    (duplicate as { backendDOMNodeId?: number }).backendDOMNodeId = 17;
    const withState = withCheckboxStates(observation, [nativeCheckboxState()]);
    await assert.rejects(createBrowserPageSnapshot(withState, sourceOptions(withState)));
  });
});

test('native checkbox state rejects duplicate, malformed, and oversized state records', async (t) => {
  const duplicate = replayObservation();
  const duplicateRead = withCheckboxStates(duplicate, [nativeCheckboxState(), nativeCheckboxState()]);
  await t.test('duplicate backend DOM ID', async () => {
    await assert.rejects(createBrowserPageSnapshot(duplicateRead, sourceOptions(duplicateRead)));
  });

  const malformed = replayObservation();
  const malformedState = { ...nativeCheckboxState(), unexpected: true } as HeadlessCheckboxState;
  const malformedRead = withCheckboxStates(malformed, [malformedState]);
  await t.test('unexpected key', async () => {
    await assert.rejects(createBrowserPageSnapshot(malformedRead, sourceOptions(malformedRead)));
  });

  const wrongValue = replayObservation();
  const wrongValueState = { ...nativeCheckboxState(), checked: 'false' } as unknown as HeadlessCheckboxState;
  const wrongValueRead = withCheckboxStates(wrongValue, [wrongValueState]);
  await t.test('wrong value type', async () => {
    await assert.rejects(createBrowserPageSnapshot(wrongValueRead, sourceOptions(wrongValueRead)));
  });

  const oversized = replayObservation();
  const tooMany = Array.from({ length: 33 }, (_, index) => nativeCheckboxState({ backendDOMNodeId: index + 100 }));
  const oversizedRead = withCheckboxStates(oversized, tooMany);
  await t.test('more than 32 states', async () => {
    await assert.rejects(createBrowserPageSnapshot(oversizedRead, sourceOptions(oversizedRead)));
  });
});

test('page source rejects a changed source time domain and a reversed mapped interval', async () => {
  const mismatched = replayObservation();
  (mismatched.provenance as { clockDomain: string }).clockDomain = 'another-monotonic-domain';
  await assert.rejects(createBrowserPageSnapshot(mismatched, sourceOptions(mismatched)));

  const observation = replayObservation();
  const reversed = clockMapping({ mapMonotonicMs: (value) => value < 771 ? 800 : 799 });
  await assert.rejects(createBrowserPageSnapshot(observation, sourceOptions(observation, reversed)));
});

test('shared browser validator detects a context hash changed after the snapshot was built', async () => {
  const observation = replayObservation();
  const snapshot = await createBrowserPageSnapshot(observation, sourceOptions(observation));
  const changed: BrowserPageSnapshot = structuredClone(snapshot);
  changed.context.browser_context_id = `${changed.context.browser_context_id}-changed`;

  const validation = await validateBrowserPageSnapshot(changed);
  assert.equal(validation.ok, false);
});
