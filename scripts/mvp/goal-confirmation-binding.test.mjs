import assert from 'node:assert/strict';
import test from 'node:test';
import { assertGoalConfirmationRootBinding } from './goal-confirmation-binding.mjs';

test('accepts only the exact Root ID and task revision shown in Goal confirmation', () => {
  const confirmation = { root_task_id: 'root-a', task_revision: 7 };
  assert.equal(assertGoalConfirmationRootBinding(confirmation, { ...confirmation, state: 'ready' }), true);
  assert.throws(() => assertGoalConfirmationRootBinding(confirmation, { root_task_id: 'root-b', task_revision: 7 }), /Root ID differs/);
  assert.throws(() => assertGoalConfirmationRootBinding(confirmation, { root_task_id: 'root-a', task_revision: 8 }), /task revision differs/);
  assert.throws(() => assertGoalConfirmationRootBinding({ root_task_id: 'root-a', task_revision: null }, confirmation), /revision is invalid/);
});

test('binds the Goal confirmation to the exported Host ledger root_id field', () => {
  const confirmation = { root_task_id: 'root-a', task_revision: 7 };
  assert.equal(assertGoalConfirmationRootBinding(confirmation, { root_id: 'root-a', task_revision: 7 }, 'Host ledger Root'), true);
  assert.throws(() => assertGoalConfirmationRootBinding(confirmation, { root_id: 'root-b', task_revision: 7 }, 'Host ledger Root'), /Root ID differs/);
  assert.throws(() => assertGoalConfirmationRootBinding(confirmation, { root_id: 'root-a', task_revision: 8 }, 'Host ledger Root'), /task revision differs/);
});
