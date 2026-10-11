import assert from 'node:assert/strict';
import { test } from 'node:test';
import { executionPresentationTransition, indicatorBounds } from '../src/main/executionPresentation.js';

test('only authorized foreground execution enters or updates the non-focusing indicator', () => {
  assert.equal(executionPresentationTransition(true, 'foreground_native', 'console'), 'enter_indicator');
  assert.equal(executionPresentationTransition(true, 'foreground_native', 'indicator'), 'update_indicator');
  assert.equal(executionPresentationTransition(true, 'background_ax', 'console'), 'keep_console');
  assert.equal(executionPresentationTransition(true, null, 'console'), 'keep_console');
});

test('BackgroundAx or stopped execution restores an existing indicator without activating the window', () => {
  assert.equal(executionPresentationTransition(true, 'background_ax', 'indicator'), 'restore_console');
  assert.equal(executionPresentationTransition(false, 'foreground_native', 'indicator'), 'restore_console');
  assert.equal(executionPresentationTransition(false, 'background_ax', 'console'), 'keep_console');
});

test('execution indicator uses free display points and handles negative monitor origins', () => {
  const area = { x: -1280, y: 0, width: 1280, height: 800 };
  assert.deepEqual(indicatorBounds([-1280, 0, 600, 600], [area]), { x: -292, y: 12, width: 280, height: 124 });
  assert.deepEqual(indicatorBounds([0, 0, 1000, 1000], [area]), { x: -292, y: 12, width: 280, height: 124 });
});
test('no valid free placement keeps the console hidden instead of covering the target', () => {
  const area = { x: 0, y: 0, width: 1000, height: 700 };
  for (const target of [null, [], [0, 0, 0, 700], [0, 0, 1000, 700], [NaN, 0, 1, 1]]) assert.equal(indicatorBounds(target, [area]), null);
  assert.equal(indicatorBounds([0, 0, 100, 100], [{ x: 0, y: 0, width: 200, height: 100 }]), null);
});
