import assert from 'node:assert/strict';
import test from 'node:test';
import { validateOwnedRevisitChallenge } from '../src/main/ownedRevisitTypes.js';
import type { OwnedDocumentSourceBootstrap } from '../src/main/ownedDocumentBootstrap.js';
import type { RootView } from '@computer-use/desktop-api';

const registration = { registration_id: '123e4567-e89b-42d3-a456-426614174000' } as OwnedDocumentSourceBootstrap;
const root: RootView = { root_task_id: 'root-1', task_revision: 1, root_epoch: 1, state: 'waiting_evidence',
  headline: 'task', attempts_used: 2, attempts_max: 4 };
const challenge = { version: 1, transition_id: '123e4567-e89b-42d3-a456-426614174001',
  root_task_id: root.root_task_id, task_revision: 1, root_epoch: 1,
  old_registration_id: registration.registration_id, save_attempt_id: 'attempt-2', save_intent_id: 'intent-2',
  save_span: { from_ms: 900, to_ms: 910 }, expected_sha256: 'a'.repeat(64), expected_byte_length: 52,
  old_device_session_epoch: 1, expected_device_session_epoch: 2, old_source_namespace: 'source-1',
  clock_domain: 'macos/1/wall', deadline_ms: 31_000 };

test('private challenge must match the original frozen Root, registration and next epoch', () => {
  const decoded = validateOwnedRevisitChallenge(challenge, registration, root, 1, 1000);
  assert.deepEqual(decoded, challenge); assert.notEqual(decoded, challenge);
  for (const change of [
    { version: 2 }, { transition_id: null }, { old_registration_id: 'different' }, { root_task_id: 'different' },
    { task_revision: 2 }, { root_epoch: 2 }, { expected_device_session_epoch: 3 }, { old_device_session_epoch: 2 },
    { deadline_ms: 1000 }, { deadline_ms: 31_001 }, { save_span: { from_ms: 911, to_ms: 910 } },
    { save_span: { from_ms: 900, to_ms: 1001 } }, { expected_byte_length: 2049 },
    { expected_byte_length: -1 }, { expected_sha256: 'A'.repeat(64) }, { unexpected: true },
  ]) assert.throws(() => validateOwnedRevisitChallenge({ ...challenge, ...change }, registration, root, 1, 1000), /challenge invalid/);
  assert.throws(() => validateOwnedRevisitChallenge(challenge, registration, { ...root, state: 'completed' }, 1, 1000));
  assert.throws(() => validateOwnedRevisitChallenge(challenge, registration, null, 1, 1000));
});
