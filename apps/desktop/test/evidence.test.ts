import assert from 'node:assert/strict';
import { test } from 'node:test';
import { reconcileNativeFrameEvidence, type NativeFrameEvidence, type RuntimeEvidence } from '../../../scripts/electron-evidence.js';
const good: NativeFrameEvidence = {
  ok: true, requires_native_frame: true, healthy_frame_screenshot_created: true, binary_image: { decoded: true },
  observation: { ownsTarget: true, healthyFrame: true, sources: [{ source: 'screenshot', state: 'available' }] },
  checks: ['healthy owned-fixture screenshot reaches the Operator', 'native frame dimensions are rendered',
    'native image and confirmation ages are rendered', 'native PNG pixels are decoded through the binary channel'],
};
test('a later failed native UI run cannot inherit an earlier success or imply a lock state', () => {
  const proof: RuntimeEvidence = { limitations: { native_frame_ui: { verified: true, screen_locked: false } } };
  reconcileNativeFrameEvidence(proof, 'production_native_frame', good, { ...good, ok: false }, 1);
  const limitation = proof.limitations!.native_frame_ui as { verified: boolean; screen_locked: boolean | null };
  assert.equal(limitation.verified, false); assert.equal(limitation.screen_locked, null);
  assert.equal(proof.run_history!.production_native_frame!.length, 1);
  reconcileNativeFrameEvidence(proof, 'production_native_frame', undefined, good, 0);
  assert.equal((proof.limitations!.native_frame_ui as { verified: boolean }).verified, true);
  reconcileNativeFrameEvidence(proof, 'production_fixture', undefined, { requires_native_frame: false, ok: false }, 1);
  assert.equal((proof.limitations!.native_frame_ui as { verified: boolean }).verified, true);
});
