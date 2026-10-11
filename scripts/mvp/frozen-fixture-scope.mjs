import assert from 'node:assert/strict';

const FIXTURE_APPLICATION_NAME = 'Interaction Fixture';
const FIXTURE_BUNDLE_ID = 'com.lingxi.computer-user.mvp-fixture';

function record(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
}

function exactKeys(value, keys, label) {
  assert.ok(record(value), `${label} must be an object`);
  assert.deepEqual(Object.keys(value).sort(), [...keys].sort(), `${label} has an unexpected shape`);
}

function nonemptyText(value, label, maximumBytes = 256) {
  assert.equal(typeof value, 'string', `${label} must be text`);
  assert.ok(value.trim() && Buffer.byteLength(value, 'utf8') <= maximumBytes, `${label} is empty or too long`);
}

export function fixtureScopeFamilyForTaskId(taskId) {
  if (/^SET-0[1-6]$/.test(taskId)) return 'control';
  if (/^FORM-0[1-7]$/.test(taskId)) return 'form';
  return null;
}

export function validateFixtureAllowedScope(scope, taskId) {
  const family = fixtureScopeFamilyForTaskId(taskId);
  assert.ok(family, `fixture scope is unsupported for ${String(taskId)}`);
  exactKeys(scope, ['application', 'targets', 'no_unrelated_controls'], 'fixture allowed scope');
  nonemptyText(scope.application, 'allowed application');
  assert.equal(scope.application, FIXTURE_APPLICATION_NAME, 'fixture allowed application differs');
  assert.equal(scope.no_unrelated_controls, true, 'fixture scope must prohibit unrelated controls');
  assert.ok(Array.isArray(scope.targets) && scope.targets.length === 1, 'fixture scope must name one primary target');

  const target = scope.targets[0];
  const targetKeys = family === 'form' ? ['control', 'form']
    : taskId === 'SET-06' ? ['control', 'disambiguator', 'group'] : ['control', 'group'];
  exactKeys(target, targetKeys, 'fixture primary target');
  nonemptyText(target.control, 'fixture primary control');
  nonemptyText(family === 'form' ? target.form : target.group, family === 'form' ? 'fixture form' : 'fixture control group');
  if (taskId === 'SET-06') assert.equal(target.disambiguator, target.group, 'fixture disambiguator must equal its group');
  return { family, target };
}

export function validateFixtureGoalPrompt(goal, taskId) {
  assert.ok(record(goal), 'Goal confirmation detail is missing');
  const allowedScope = normalizeFixtureGoalScopeWire(goal.allowed_scope, taskId);
  const target = allowedScope.targets[0];
  assert.equal(goal.app, FIXTURE_BUNDLE_ID, 'Goal confirmation application is not the exact registered fixture bundle');
  assert.equal(goal.target_label, target.control, 'Goal confirmation primary control differs from its scope');
  return {
    family: fixtureScopeFamilyForTaskId(taskId),
    allowed_scope: allowedScope,
    allowed_scope_wire: goal.allowed_scope,
    target,
  };
}

export function fixtureGoalConfirmationMatchesRequest(confirmation, taskId, expectedScope) {
  try {
    const prompt = validateFixtureGoalPrompt({
      app: confirmation?.application,
      target_label: confirmation?.target_label,
      allowed_scope: confirmation?.allowed_scope_wire,
    }, taskId);
    assert.equal(confirmation.scope_family, prompt.family);
    assert.deepEqual(prompt.allowed_scope, expectedScope);
    return true;
  } catch { return false; }
}

function normalizeFixtureGoalScopeWire(scope, taskId) {
  const family = fixtureScopeFamilyForTaskId(taskId);
  assert.ok(family, `fixture Goal scope is unsupported for ${String(taskId)}`);
  exactKeys(scope, ['application', 'targets', 'no_unrelated_controls'], 'Goal wire allowed scope');
  nonemptyText(scope.application, 'Goal wire allowed application');
  assert.equal(scope.application, FIXTURE_APPLICATION_NAME, 'Goal wire allowed application differs');
  assert.equal(scope.no_unrelated_controls, true, 'Goal wire scope must prohibit unrelated controls');
  assert.ok(Array.isArray(scope.targets) && scope.targets.length === 1, 'Goal wire scope must name one primary target');

  const wireTarget = scope.targets[0];
  const wireKeys = family === 'form'
    ? ['control', 'disambiguator', 'form', 'group']
    : ['control', 'disambiguator', 'group'];
  exactKeys(wireTarget, wireKeys, 'Goal wire primary target');
  nonemptyText(wireTarget.control, 'Goal wire primary control');

  let target;
  if (family === 'form') {
    assert.equal(wireTarget.group, null, 'form Goal must not carry a group authority');
    assert.equal(wireTarget.disambiguator, null, 'form Goal must not carry a disambiguator authority');
    nonemptyText(wireTarget.form, 'Goal wire form context');
    target = { control: wireTarget.control, form: wireTarget.form };
  } else {
    assert.equal(wireTarget.form, undefined, 'control Goal must not carry a form authority');
    nonemptyText(wireTarget.group, 'Goal wire control group');
    if (taskId === 'SET-06') {
      assert.equal(wireTarget.disambiguator, wireTarget.group, 'Goal wire disambiguator must equal its group');
      target = { control: wireTarget.control, group: wireTarget.group, disambiguator: wireTarget.disambiguator };
    } else {
      assert.equal(wireTarget.disambiguator, null, 'control Goal must not carry a disambiguator authority');
      target = { control: wireTarget.control, group: wireTarget.group };
    }
  }
  return { application: scope.application, targets: [target], no_unrelated_controls: true };
}
