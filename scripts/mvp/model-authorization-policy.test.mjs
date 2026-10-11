import assert from 'node:assert/strict';
import test from 'node:test';
import {
  nativeConsentDriverArgs,
  productModelAuthorizationReady,
  productPurposeConsentMatches,
  summarizeProductModelAuthorization,
} from './model-authorization-policy.mjs';

test('requires the real saved-route status to authorize exactly S2 and S1', () => {
  const status = { phase: 'authorized', planning_available: true, authorized_purposes: ['s2_planning', 's1_teacher'] };
  assert.equal(productModelAuthorizationReady(status), true);
  assert.equal(productModelAuthorizationReady({ ...status, authorized_purposes: ['s2_planning'] }), false);
  assert.equal(productModelAuthorizationReady({ ...status, authorized_purposes: ['s2_planning', 's1_teacher', 'computer_proposals'] }), false);
  assert.equal(productModelAuthorizationReady({ ...status, phase: 'not_configured' }), false);
  assert.equal(productModelAuthorizationReady({ ...status, planning_available: false }), false);
});

test('accepts only native model consent listing S2 and S1 and excluding screenshot proposals', () => {
  const detail = 'Selected purposes:\n• S2 planning: task and target information.\n• S1 teacher: semantic decisions.\n\nTask text may be sent.';
  assert.equal(productPurposeConsentMatches(detail), true);
  assert.equal(productPurposeConsentMatches(detail.replace('• S1 teacher:', '')), false);
  assert.equal(productPurposeConsentMatches(`${detail}\n• Computer actions: screenshots.`), false);
  assert.equal(productPurposeConsentMatches(detail.replace('• S1 teacher:', '• S2 planning:')), false);
});

test('maps audit consent kinds to the exact native helper dialog kinds', () => {
  assert.deepEqual(nativeConsentDriverArgs('allow', 'model'), ['allow', 'model']);
  assert.deepEqual(nativeConsentDriverArgs('allow', 'fixture_window'), ['allow', 'window']);
  assert.throws(() => nativeConsentDriverArgs('allow', 's2_planning_and_s1_teacher'), /dialog kind/);
  assert.throws(() => nativeConsentDriverArgs('execute', 'model'), /action/);
});

test('summarizes final authorization without copying credentials or route secrets', () => {
  const summary = summarizeProductModelAuthorization({
    phase: 'authorized', planning_available: true, authorized_purposes: ['s2_planning', 's1_teacher'],
    saved: [{}], storage: 'keychain', api_key: 'must-not-copy', route: { model: 'also-not-copied' },
    reason: 'safe public status\nfor diagnosis',
  });
  assert.deepEqual(summary, {
    phase: 'authorized', planning_available: true, authorized_purposes: ['s2_planning', 's1_teacher'],
    saved_connection_count: 1, storage: 'keychain', reason: 'safe public status for diagnosis',
  });
});
