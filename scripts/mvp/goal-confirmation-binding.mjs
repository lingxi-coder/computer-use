import assert from 'node:assert/strict';

/** Require a projection or exported-ledger Root identity to match the Goal confirmation. */
export function assertGoalConfirmationRootBinding(confirmation, root, stage = 'Root state') {
  assert.ok(confirmation && typeof confirmation === 'object', 'Goal confirmation identity is missing');
  assert.ok(root && typeof root === 'object', `${stage} identity is missing`);
  assert.equal(typeof confirmation.root_task_id, 'string', 'Goal confirmation Root ID is missing');
  assert.ok(Number.isSafeInteger(confirmation.task_revision) && confirmation.task_revision >= 0,
    'Goal confirmation task revision is invalid');
  const rootTaskId = typeof root.root_task_id === 'string' ? root.root_task_id : root.root_id;
  assert.equal(rootTaskId, confirmation.root_task_id, `${stage} Root ID differs from the confirmed Goal`);
  assert.equal(root.task_revision, confirmation.task_revision, `${stage} task revision differs from the confirmed Goal`);
  return true;
}
