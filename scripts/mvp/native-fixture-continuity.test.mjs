import assert from 'node:assert/strict';
import test from 'node:test';
import { nativeFixtureBindingEvidence, nativeFixtureAuthorityContext } from './native-fixture-continuity.mjs';

function coveredRead() {
  const frame = { x: 0, y: 0, width: 400, height: 300 };
  const node = (instance, role, parent, bounds, title) => ({
    instance_ref: instance, parent, window_id: 'window-1', role,
    subrole: role === 'AXWindow' ? 'AXStandardWindow' : null, title, description: null,
    value: null, enabled: true, focused: false, frame: bounds, actions: [],
    in_view: true, group: null, secure: false, scroll: null,
    identity_continuity: 'notification_covered',
  });
  return {
    complete: true, incomplete_reason: null,
    app: { bundle_id: 'fixture', name: 'Fixture', pid: 1 },
    windows: [{ window_id: 'window-1', title: 'Fixture', frame, z_index: 0,
      kind: 'window', modal: false, owner_bundle_id: 'fixture' }],
    global_focus: {
      app: { state: 'known', value: { bundle_id: 'fixture', name: 'Fixture', pid: 1 } },
      window_id: { state: 'known', value: 'window-1' },
    },
    target_context: null,
    objects: [node('root-1', 'AXWindow', null, frame, 'Fixture'),
      node('button-1', 'AXButton', 'root-1', { x: 10.25, y: 20, width: 60, height: 24 }, 'Run')],
    observer_coverage: { state: 'available', covered_objects: 2, read_scoped_objects: 0,
      failed_registrations: 0, failures: [] },
    menu_anchors: [], menu_context: null, focus_context: null, window_context: null,
    table_viewports: [],
  };
}

test('diagnostic evidence uses the Rust compiler and remains read-scoped', () => {
  const read = coveredRead();
  const first = nativeFixtureBindingEvidence(read, read.objects[1], 1);
  assert.equal(first.continuity.kind, 'read_scoped');
  assert.match(first.continuity.context_digest, /^bc1:[0-9a-f]{64}$/u);
  assert.equal(first.device_context.scope, 'whole_structure_v1');
  assert.equal(first.device_context.active_layer, 'window-1');
  assert.equal(first.device_context.target.object.frame.x, 10.25);
  assert.equal(first.device_context.target.object.label, 'run');
  assert.equal(first.device_context.siblings.length, 1);
  const next = nativeFixtureBindingEvidence(read, read.objects[1], 2);
  assert.equal(next.continuity.source_sequence, 2);
  assert.equal(next.continuity.context_digest, first.continuity.context_digest);
  const wire = nativeFixtureAuthorityContext({ ...first, binding: 'diagnostic-binding' });
  assert.equal(wire.context_witness.scope, 'whole_structure_v1');
  assert.equal(Object.hasOwn(wire.original_proof, 'device_context'), false);
  assert.equal(Object.hasOwn(wire.current_proof, 'device_context'), false);
});

test('complete sibling context detects changes that preserve the target index', () => {
  const read = coveredRead();
  const sibling = { ...structuredClone(read.objects[1]), instance_ref: 'button-2', title: 'Other' };
  sibling.frame.x = 100;
  read.objects.push(sibling);
  read.observer_coverage.covered_objects = 3;
  const before = nativeFixtureBindingEvidence(read, read.objects[1], 1);
  read.objects[2].title = 'Replacement';
  const after = nativeFixtureBindingEvidence(read, read.objects[1], 2);
  assert.equal(before.device_context.target_sibling_index, after.device_context.target_sibling_index);
  assert.notDeepEqual(before.device_context.siblings, after.device_context.siblings);
});

test('missing, inconsistent, partial and unbounded Source facts cannot seed evidence', () => {
  const missing = coveredRead(); delete missing.objects[0].identity_continuity;
  assert.throws(() => nativeFixtureBindingEvidence(missing, missing.objects[1], 1), /continuity is unknown/u);
  const inconsistent = coveredRead(); inconsistent.observer_coverage.covered_objects = 0;
  assert.throws(() => nativeFixtureBindingEvidence(inconsistent, inconsistent.objects[1], 1), /coverage does not match/u);
  for (const field of ['failed_registrations', 'failures']) {
    const absent = coveredRead(); delete absent.observer_coverage[field];
    assert.throws(() => nativeFixtureBindingEvidence(absent, absent.objects[1], 1), /registration failures/u);
  }
  const unbounded = coveredRead(); unbounded.observer_coverage.failed_registrations = 4097;
  assert.throws(() => nativeFixtureBindingEvidence(unbounded, unbounded.objects[1], 1), /registration failures/u);
  const partial = coveredRead(); partial.complete = false;
  assert.throws(() => nativeFixtureBindingEvidence(partial, partial.objects[1], 1), /complete bounded Source/u);
  const contradictory = coveredRead(); contradictory.observer_coverage.state = 'unavailable';
  assert.throws(() => nativeFixtureBindingEvidence(contradictory, contradictory.objects[1], 1), /contradicts/u);
});

test('fixture inputs require the current focus/context contract and reject legacy active_layer', () => {
  const legacy = coveredRead();
  legacy.active_layer = 'window-1';
  assert.throws(() => nativeFixtureBindingEvidence(legacy, legacy.objects[1], 1), /active_layer/u);

  const incompleteContract = coveredRead();
  delete incompleteContract.global_focus;
  assert.throws(() => nativeFixtureBindingEvidence(incompleteContract, incompleteContract.objects[1], 1), /global_focus/u);

  const missingTargetContext = coveredRead();
  delete missingTargetContext.target_context;
  assert.throws(() => nativeFixtureBindingEvidence(missingTargetContext, missingTargetContext.objects[1], 1), /target_context/u);
});
