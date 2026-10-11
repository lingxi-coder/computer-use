import assert from 'node:assert/strict';
import type { EventEmitter } from 'node:events';
import { test } from 'node:test';

import { browserDiagnostics } from '../src/main/browserDiagnostics.js';

const ownerEvent = 'computer-use:browser-owner';
const snapshotEvent = 'computer-use:browser-snapshot';
const executionEvent = 'computer-use:browser-execution';
const processEvents = process as unknown as EventEmitter;

test('browser diagnostics route each callback to its matching process event', () => {
  const owner = {
    supervisorDir: '/tmp/supervisor',
    profileParent: '/tmp/profile',
    pids: { main: 101, device: 202 },
  };
  const snapshot = { snapshot_id: 'snapshot-1', context: { page_target_id: 'page-1' } };
  const execution = { submission: { sequence: 3 }, reply: { outcome: 'completed' } };
  const received: Array<{ event: string; value: unknown }> = [];
  const listeners = new Map<string, (value: unknown) => void>();

  for (const event of [ownerEvent, snapshotEvent, executionEvent]) {
    const listener = (value: unknown) => received.push({ event, value });
    listeners.set(event, listener);
    processEvents.on(event, listener);
  }

  try {
    browserDiagnostics.observeBrowserSourceOwner?.(owner as never);
    browserDiagnostics.observeBrowserSnapshot?.(snapshot as never);
    browserDiagnostics.observeBrowserExecution?.(execution as never);

    assert.deepEqual(received.map(({ event }) => event), [ownerEvent, snapshotEvent, executionEvent]);
    assert.deepEqual(received.map(({ value }) => value), [owner, snapshot, execution]);
  } finally {
    for (const [event, listener] of listeners) processEvents.removeListener(event, listener);
  }
});

test('browser diagnostics publish deeply cloned and frozen payloads', () => {
  const owner = {
    supervisorDir: '/tmp/supervisor',
    profileParent: '/tmp/profile',
    pids: { main: 101, device: 202 },
  };
  const snapshot = {
    snapshot_id: 'snapshot-1',
    context: { page_target_id: 'page-1' },
    nodes: [{ node_id: 'node-1', child_ids: ['node-2'] }],
  };
  const execution = {
    submission: { intent: { evidence: { binding: { checked: false as boolean } } } },
    reply: { outcome: 'completed' },
  };
  const observed: Record<string, unknown> = {};
  const listeners = new Map<string, (value: unknown) => void>();

  for (const event of [ownerEvent, snapshotEvent, executionEvent]) {
    const listener = (value: unknown) => { observed[event] = value; };
    listeners.set(event, listener);
    processEvents.on(event, listener);
  }

  try {
    browserDiagnostics.observeBrowserSourceOwner?.(owner as never);
    browserDiagnostics.observeBrowserSnapshot?.(snapshot as never);
    browserDiagnostics.observeBrowserExecution?.(execution as never);

    const ownerCopy = observed[ownerEvent] as typeof owner;
    const snapshotCopy = observed[snapshotEvent] as typeof snapshot;
    const executionCopy = observed[executionEvent] as typeof execution;

    assert.notStrictEqual(ownerCopy, owner);
    assert.notStrictEqual(ownerCopy.pids, owner.pids);
    assert.notStrictEqual(snapshotCopy.nodes, snapshot.nodes);
    assert.notStrictEqual(snapshotCopy.nodes[0], snapshot.nodes[0]);
    assert.notStrictEqual(executionCopy.submission.intent.evidence.binding, execution.submission.intent.evidence.binding);

    assert.equal(Object.isFrozen(ownerCopy), true);
    assert.equal(Object.isFrozen(ownerCopy.pids), true);
    assert.equal(Object.isFrozen(snapshotCopy), true);
    assert.equal(Object.isFrozen(snapshotCopy.context), true);
    assert.equal(Object.isFrozen(snapshotCopy.nodes), true);
    assert.equal(Object.isFrozen(snapshotCopy.nodes[0]!), true);
    assert.equal(Object.isFrozen(snapshotCopy.nodes[0]!.child_ids), true);
    assert.equal(Object.isFrozen(executionCopy.submission.intent.evidence.binding), true);

    owner.pids.main = 999;
    snapshot.nodes[0]!.child_ids[0] = 'changed';
    execution.submission.intent.evidence.binding.checked = true;
    assert.equal(ownerCopy.pids.main, 101);
    assert.equal(snapshotCopy.nodes[0]!.child_ids[0], 'node-2');
    assert.equal(executionCopy.submission.intent.evidence.binding.checked, false);

    assert.equal(Reflect.set(ownerCopy.pids, 'main', 303), false);
    assert.equal(ownerCopy.pids.main, 101);
  } finally {
    for (const [event, listener] of listeners) processEvents.removeListener(event, listener);
  }
});

test('a throwing diagnostic listener does not interrupt later observers or the callback', () => {
  const observed: string[] = [];
  const throwingListener = () => {
    observed.push('throwing');
    throw new Error('diagnostic observer failure');
  };
  const laterListener = (value: unknown) => {
    observed.push(`later:${(value as { snapshot_id: string }).snapshot_id}`);
  };
  processEvents.on(snapshotEvent, throwingListener);
  processEvents.on(snapshotEvent, laterListener);

  try {
    assert.doesNotThrow(() => browserDiagnostics.observeBrowserSnapshot?.({ snapshot_id: 'snapshot-2' } as never));
    assert.deepEqual(observed, ['throwing', 'later:snapshot-2']);
  } finally {
    processEvents.removeListener(snapshotEvent, throwingListener);
    processEvents.removeListener(snapshotEvent, laterListener);
  }
});

test('once listeners keep EventEmitter once semantics', () => {
  let count = 0;
  const listener = () => { count += 1; };
  processEvents.once(executionEvent, listener);

  try {
    browserDiagnostics.observeBrowserExecution?.({ submission: {}, reply: {} } as never);
    browserDiagnostics.observeBrowserExecution?.({ submission: {}, reply: {} } as never);
    assert.equal(count, 1);
    assert.equal(processEvents.listeners(executionEvent).includes(listener), false);
  } finally {
    processEvents.removeListener(executionEvent, listener);
  }
});

test('with no listeners, diagnostics do not clone or freeze the source value', () => {
  assert.equal(processEvents.listenerCount(ownerEvent), 0);
  const source = { nested: { callback: () => 'not cloneable' } };

  assert.doesNotThrow(() => browserDiagnostics.observeBrowserSourceOwner?.(source as never));
  assert.equal(Object.isFrozen(source), false);
  assert.equal(Object.isFrozen(source.nested), false);
});
