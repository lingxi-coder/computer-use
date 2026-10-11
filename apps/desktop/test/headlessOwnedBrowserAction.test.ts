import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdirSync, mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { PassThrough } from 'node:stream';
import test, { type TestContext } from 'node:test';

import type { ChildProcess } from 'node:child_process';

import { CdpPipe, HeadlessBrowserError, type HeadlessDiagnosticReason } from '../src/main/headless/cdpPipe.js';
import {
  createOwnedBrowserSessionForTests,
  OwnedBrowserActionOutcomeUnknown,
  chooseOwnedCheckboxTapPoint,
  inspectOwnedBrowserCheckboxSnapshot,
  type OwnedBrowserCheckboxAction,
  type OwnedBrowserSession,
} from '../src/main/headless/ownedBrowser.js';

const fixtureUrl = 'file:///workspace/fixtures/headless/readonly-page.html';

interface SnapshotNode {
  readonly name: string;
  readonly parent: number;
  readonly type?: number;
  readonly attributes?: Readonly<Record<string, string>>;
}

function snapshot(
  nodes: readonly SnapshotNode[],
  options: {
    readonly documentUrl?: string;
    readonly checkedIndexes?: readonly number[] | null;
    readonly duplicateBackendAt?: number;
    readonly danglingParentAt?: number;
    readonly duplicateLayoutAt?: number;
    readonly emptyStyleAt?: number;
  } = {},
): Record<string, unknown> {
  const strings: string[] = [];
  const stringIndex = (value: string): number => {
    let index = strings.indexOf(value);
    if (index < 0) { index = strings.length; strings.push(value); }
    return index;
  };
  const backendNodeId = nodes.map((_, index) => index + 101);
  if (options.duplicateBackendAt !== undefined) {
    backendNodeId[options.duplicateBackendAt] = backendNodeId[0]!;
  }
  const attributes = nodes.map((node) => Object.entries(node.attributes ?? {}).flatMap(([name, value]) => [
    stringIndex(name), stringIndex(value),
  ]));
  const styleIndexes = [stringIndex('block'), stringIndex('visible'), stringIndex('1'), stringIndex('auto')];
  const layoutIndexes = nodes.map((_, index) => index);
  if (options.duplicateLayoutAt !== undefined) layoutIndexes.push(options.duplicateLayoutAt);
  const layout = {
    nodeIndex: layoutIndexes,
    bounds: layoutIndexes.map(() => [0, 0, 100, 100]),
    styles: layoutIndexes.map((index) => (
      nodes[index]?.type === 9 || options.emptyStyleAt === index ? [] : styleIndexes
    )),
  };
  const documentURL = stringIndex(options.documentUrl ?? fixtureUrl);
  const nodeTree: Record<string, unknown> = {
    backendNodeId,
    nodeType: nodes.map((node) => node.type ?? 1),
    nodeName: nodes.map((node) => stringIndex(node.name)),
    parentIndex: nodes.map((node, index) => options.danglingParentAt === index ? nodes.length + 1 : node.parent),
    attributes,
    shadowRootType: { index: [], value: [] },
  };
  if (options.checkedIndexes !== null) nodeTree.inputChecked = { index: [...(options.checkedIndexes ?? [])] };
  return {
    documents: [{
      documentURL,
      nodes: nodeTree,
      layout,
    }],
    strings,
  };
}

function simpleNodes(attributes: Readonly<Record<string, string>> = {}): SnapshotNode[] {
  return [
    { name: '#document', parent: -1, type: 9 },
    { name: 'HTML', parent: 0 },
    { name: 'BODY', parent: 1 },
    { name: 'INPUT', parent: 2, attributes: { type: 'checkbox', ...attributes } },
  ];
}

function isProtocolError(error: unknown): boolean {
  return error instanceof HeadlessBrowserError && error.code === 'protocol_error';
}

function snapshotRecords(value: Record<string, unknown>): {
  readonly snapshot: Record<string, unknown>;
  readonly document: Record<string, unknown>;
  readonly nodeTree: Record<string, unknown>;
} {
  const documents = value.documents as Record<string, unknown>[];
  const document = documents[0]!;
  return {
    snapshot: value,
    document,
    nodeTree: document.nodes as Record<string, unknown>,
  };
}

const expectedDocument = { frameId: 'frame-1', loaderId: 'loader-1', url: fixtureUrl } as const;
const AX_CHECKBOX = {
  nodes: [{
    nodeId: 'checkbox-ax',
    ignored: false,
    frameId: expectedDocument.frameId,
    backendDOMNodeId: 104,
    role: { type: 'role', value: 'checkbox' },
    properties: [
      { name: 'checked', value: { type: 'boolean', value: false } },
      { name: 'disabled', value: { type: 'boolean', value: false } },
    ],
  }],
};
const ACTION_METRICS = {
  cssLayoutViewport: { pageX: 0, pageY: 0, clientWidth: 100, clientHeight: 100 },
  cssVisualViewport: { offsetX: 0, offsetY: 0, clientWidth: 100, clientHeight: 100, scale: 1 },
  cssContentSize: { width: 100, height: 100 },
};

class SessionPipeHarness {
  readonly readable = new PassThrough();
  readonly writable = new PassThrough();
  readonly pipe: CdpPipe;
  private bytes = Buffer.alloc(0);
  private commands: Record<string, unknown>[] = [];
  private waiters: Array<(command: Record<string, unknown>) => void> = [];

  constructor() {
    this.pipe = new CdpPipe(this.readable, this.writable, fixtureUrl);
    this.writable.on('data', (chunk: Buffer) => {
      this.bytes = this.bytes.length ? Buffer.concat([this.bytes, chunk]) : Buffer.from(chunk);
      while (true) {
        const delimiter = this.bytes.indexOf(0);
        if (delimiter < 0) return;
        const frame = this.bytes.subarray(0, delimiter);
        this.bytes = Buffer.from(this.bytes.subarray(delimiter + 1));
        const command = JSON.parse(frame.toString('utf8')) as Record<string, unknown>;
        const waiter = this.waiters.shift();
        if (waiter) waiter(command);
        else this.commands.push(command);
      }
    });
  }

  async nextCommand(): Promise<Record<string, unknown>> {
    const command = this.commands.shift();
    if (command) return command;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('timed out waiting for fake CDP command')), 2_000);
      this.waiters.push((next) => { clearTimeout(timer); resolve(next); });
    });
  }

  respond(command: Record<string, unknown>, result: Record<string, unknown>): void {
    this.readable.write(`${JSON.stringify({ id: command.id, result,
      ...(typeof command.sessionId === 'string' ? { sessionId: command.sessionId } : {}) })}\0`);
  }

  disconnect(): void { this.readable.end(); }
}

class FakeOwnedChild extends EventEmitter {
  readonly pid = 88_321;
  exitCode: number | null = null;
  signalCode: NodeJS.Signals | null = null;
  killed = false;
  readonly signals: NodeJS.Signals[] = [];

  kill(signal: NodeJS.Signals): boolean {
    this.killed = true;
    this.signals.push(signal);
    this.signalCode = signal;
    setImmediate(() => this.emit('close', null, signal));
    return true;
  }
}

interface OwnedSessionTestFixture {
  readonly session: OwnedBrowserSession;
  readonly child: FakeOwnedChild;
  readonly pipeHarness: SessionPipeHarness;
}

function ownedSessionFixture(t: TestContext): OwnedSessionTestFixture {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'cu-owned-browser-action-')));
  const profileParent = join(root, 'profiles');
  const profileDirectory = join(profileParent, 'cu-headless-owned-action-test');
  mkdirSync(profileParent);
  mkdirSync(profileDirectory);
  const child = new FakeOwnedChild();
  const pipeHarness = new SessionPipeHarness();
  const session = createOwnedBrowserSessionForTests({
    child: child as unknown as ChildProcess,
    pipe: pipeHarness.pipe,
    profileDirectory,
    profileParent,
    fixtureURL: fixtureUrl,
    expectedDocument,
    browserContextId: 'browser-context-1',
    targetId: 'page-target-1',
    sessionId: 'cdp-session-1',
    sequence: 1,
  });
  t.after(async () => {
    session.retireNow();
    await session.close().catch(() => undefined);
    rmSync(root, { recursive: true, force: true });
  });
  return { session, child, pipeHarness };
}

function actionFor(deadlineMonotonicMs: number, sequence = 1): OwnedBrowserCheckboxAction {
  return {
    expectedDocument,
    backendDOMNodeId: 104,
    expectedChecked: false,
    desiredChecked: true,
    expectedSourceSequence: sequence,
    inputDeadlineMonotonicMs: deadlineMonotonicMs,
    isSourceActive: () => true,
  };
}

const testPngBase64 = Buffer.from([
  137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0, 73, 69, 78, 68, 0, 0, 0, 0,
]).toString('base64');

async function answerActionCommand(
  harness: SessionPipeHarness,
  command: Record<string, unknown>,
): Promise<void> {
  let result: Record<string, unknown>;
  switch (command.method) {
    case 'Page.getFrameTree':
      result = { frameTree: { frame: { id: expectedDocument.frameId, loaderId: expectedDocument.loaderId,
        url: expectedDocument.url } } };
      break;
    case 'DOMSnapshot.captureSnapshot':
      result = snapshot(simpleNodes(), { checkedIndexes: [] }) as Record<string, unknown>;
      break;
    case 'Accessibility.getPartialAXTree':
      result = AX_CHECKBOX;
      break;
    case 'Page.getLayoutMetrics':
      result = ACTION_METRICS;
      break;
    case 'DOM.getContentQuads':
      result = { quads: [[10, 10, 30, 10, 30, 30, 10, 30]] };
      break;
    case 'DOM.getNodeForLocation':
      result = { backendNodeId: 104, frameId: expectedDocument.frameId, nodeId: 204 };
      break;
    case 'Page.captureScreenshot':
      result = { data: testPngBase64 };
      break;
    default:
      assert.fail(`unexpected fake CDP command: ${String(command.method)}`);
  }
  harness.respond(command, result);
}

test('DOMSnapshot sparse inputChecked marks native false/true without inventing missing state', () => {
  const falseState = inspectOwnedBrowserCheckboxSnapshot(snapshot(simpleNodes(), { checkedIndexes: [] }), fixtureUrl, 104);
  assert.equal(falseState.nativeCheckbox, true);
  assert.equal(falseState.checked, false);

  const trueState = inspectOwnedBrowserCheckboxSnapshot(snapshot(simpleNodes(), { checkedIndexes: [3] }), fixtureUrl, 104);
  assert.equal(trueState.checked, true);

  const missingState = inspectOwnedBrowserCheckboxSnapshot(snapshot(simpleNodes(), { checkedIndexes: null }), fixtureUrl, 104);
  assert.equal(missingState.checked, null);
});

test('disabled fieldset honors its first legend exception and blocks other descendants', () => {
  const inFirstLegend: SnapshotNode[] = [
    { name: '#document', parent: -1, type: 9 },
    { name: 'HTML', parent: 0 },
    { name: 'BODY', parent: 1 },
    { name: 'FIELDSET', parent: 2, attributes: { disabled: '' } },
    { name: 'LEGEND', parent: 3 },
    { name: 'INPUT', parent: 4, attributes: { type: 'checkbox' } },
  ];
  assert.equal(inspectOwnedBrowserCheckboxSnapshot(
    snapshot(inFirstLegend, { checkedIndexes: [] }), fixtureUrl, 106,
  ).disabled, false);

  const outsideFirstLegend = [...inFirstLegend.slice(0, 5),
    { name: 'INPUT', parent: 3, attributes: { type: 'checkbox' } }];
  assert.equal(inspectOwnedBrowserCheckboxSnapshot(
    snapshot(outsideFirstLegend, { checkedIndexes: [] }), fixtureUrl, 106,
  ).disabled, true);
});

test('inert and aria-disabled ancestors block; aria-checked custom semantics are refused', () => {
  assert.equal(inspectOwnedBrowserCheckboxSnapshot(
    snapshot(simpleNodes(), { checkedIndexes: [], documentUrl: fixtureUrl }), fixtureUrl, 104,
  ).blocked, false);

  const inert = simpleNodes();
  inert[2] = { ...inert[2]!, attributes: { inert: '' } };
  assert.equal(inspectOwnedBrowserCheckboxSnapshot(snapshot(inert, { checkedIndexes: [] }), fixtureUrl, 104).blocked, true);

  const aria = simpleNodes();
  aria[2] = { ...aria[2]!, attributes: { 'aria-disabled': 'true' } };
  assert.equal(inspectOwnedBrowserCheckboxSnapshot(snapshot(aria, { checkedIndexes: [] }), fixtureUrl, 104).blocked, true);

  assert.equal(inspectOwnedBrowserCheckboxSnapshot(
    snapshot(simpleNodes({ 'aria-checked': 'false' }), { checkedIndexes: [] }), fixtureUrl, 104,
  ).ariaCheckedPresent, true);
});

test('DOMSnapshot rejects duplicate backend identities, dangling parents, duplicate layout rows, and foreign documents', () => {
  assert.throws(() => inspectOwnedBrowserCheckboxSnapshot(
    snapshot(simpleNodes(), { checkedIndexes: [], duplicateBackendAt: 3 }), fixtureUrl, 104,
  ), isProtocolError);
  assert.throws(() => inspectOwnedBrowserCheckboxSnapshot(
    snapshot(simpleNodes(), { checkedIndexes: [], danglingParentAt: 2 }), fixtureUrl, 104,
  ), isProtocolError);
  assert.throws(() => inspectOwnedBrowserCheckboxSnapshot(
    snapshot(simpleNodes(), { checkedIndexes: [], duplicateLayoutAt: 3 }), fixtureUrl, 104,
  ), isProtocolError);
  assert.throws(() => inspectOwnedBrowserCheckboxSnapshot(
    snapshot(simpleNodes(), { checkedIndexes: [], documentUrl: 'file:///foreign.html' }), fixtureUrl, 104,
  ), (error: unknown) => error instanceof HeadlessBrowserError && error.code === 'document_changed');
});

test('DOMSnapshot accepts Chromium document layout rows without CSS while requiring element styles', () => {
  const result = inspectOwnedBrowserCheckboxSnapshot(snapshot(simpleNodes(), { checkedIndexes: [] }), fixtureUrl, 104);
  assert.equal(result.nativeCheckbox, true);
  assert.equal(result.checked, false);
  assert.equal(result.blocked, false);

  for (const emptyStyleAt of [1, 2, 3]) {
    assert.throws(() => inspectOwnedBrowserCheckboxSnapshot(
      snapshot(simpleNodes(), { checkedIndexes: [], emptyStyleAt }), fixtureUrl, 104,
    ), isProtocolError);
  }
});

test('DOMSnapshot accepts only Chromium empty attribute-value sentinel', () => {
  const emptyValue = snapshot(simpleNodes({ disabled: '' }), { checkedIndexes: [] });
  const valueAttributes = snapshotRecords(emptyValue).nodeTree.attributes as number[][];
  valueAttributes[3]![valueAttributes[3]!.length - 1] = -1;
  const state = inspectOwnedBrowserCheckboxSnapshot(emptyValue, fixtureUrl, 104);
  assert.equal(state.disabled, true);

  const emptyName = snapshot(simpleNodes(), { checkedIndexes: [] });
  (snapshotRecords(emptyName).nodeTree.attributes as number[][])[3]![0] = -1;
  assert.throws(() => inspectOwnedBrowserCheckboxSnapshot(emptyName, fixtureUrl, 104), (error: unknown) => (
    error instanceof HeadlessBrowserError && error.code === 'protocol_error'
      && error.diagnostic?.reason === 'attributes'
  ));

  const otherNegativeValue = snapshot(simpleNodes({ title: '' }), { checkedIndexes: [] });
  const attributes = snapshotRecords(otherNegativeValue).nodeTree.attributes as number[][];
  attributes[3]![attributes[3]!.length - 1] = -2;
  assert.throws(() => inspectOwnedBrowserCheckboxSnapshot(otherNegativeValue, fixtureUrl, 104), (error: unknown) => (
    error instanceof HeadlessBrowserError && error.code === 'protocol_error'
      && error.diagnostic?.reason === 'attributes'
  ));
});

test('DOMSnapshot parser failures expose only their closed validation partition', () => {
  const cases: readonly {
    readonly reason: HeadlessDiagnosticReason;
    readonly mutate: (parts: ReturnType<typeof snapshotRecords>) => void;
  }[] = [
    { reason: 'top_level', mutate: ({ snapshot: value }) => { value.strings = 'malformed'; } },
    { reason: 'document_url_index', mutate: ({ document }) => { document.documentURL = 99_999; } },
    { reason: 'node_tables', mutate: ({ nodeTree }) => { nodeTree.backendNodeId = [101]; } },
    {
      reason: 'node_row',
      mutate: ({ nodeTree }) => { (nodeTree.nodeName as unknown[])[1] = 99_999; },
    },
    {
      reason: 'attributes',
      mutate: ({ nodeTree }) => { (nodeTree.attributes as unknown[])[3] = [99_999, 0]; },
    },
    {
      reason: 'root_graph',
      mutate: ({ nodeTree }) => { (nodeTree.parentIndex as unknown[])[1] = -1; },
    },
    {
      reason: 'shadow_data',
      mutate: ({ nodeTree }) => { nodeTree.shadowRootType = { index: [0], value: [] }; },
    },
    {
      reason: 'checked_data',
      mutate: ({ nodeTree }) => { nodeTree.inputChecked = { index: [99_999] }; },
    },
    {
      reason: 'layout_tables',
      mutate: ({ document }) => { ((document.layout as Record<string, unknown>).styles as unknown[]).pop(); },
    },
    {
      reason: 'layout_row_style',
      mutate: ({ document }) => { ((document.layout as Record<string, unknown>).styles as unknown[])[1] = []; },
    },
  ];

  for (const { reason, mutate } of cases) {
    const value = snapshot(simpleNodes(), { checkedIndexes: [] });
    mutate(snapshotRecords(value));
    assert.throws(() => inspectOwnedBrowserCheckboxSnapshot(value, fixtureUrl, 104), (error: unknown) => {
      assert.ok(error instanceof HeadlessBrowserError);
      assert.equal(error.code, 'protocol_error');
      assert.equal(error.diagnostic?.stage, 'checkbox_action_snapshot');
      assert.equal(error.diagnostic?.reason, reason);
      assert.deepEqual(Object.keys(error.diagnostic ?? {}).sort(), ['reason', 'stage']);
      assert.doesNotMatch(JSON.stringify(error.diagnostic), /fixture|sensitive|payload/i);
      return true;
    }, `expected ${reason} validation failure`);
  }
});

test('action DOMSnapshot parser failures retain a closed stage and command without payload data', async (t) => {
  const fixture = ownedSessionFixture(t);
  const action = fixture.session.setCheckbox(actionFor(performance.now() + 5_000));

  const identity = await fixture.pipeHarness.nextCommand();
  assert.equal(identity.method, 'Page.getFrameTree');
  await answerActionCommand(fixture.pipeHarness, identity);
  const capture = await fixture.pipeHarness.nextCommand();
  assert.equal(capture.method, 'DOMSnapshot.captureSnapshot');
  fixture.pipeHarness.respond(capture, {
    documents: [{ documentURL: 0, nodes: {} }],
    strings: ['file:///private/sensitive.html'],
  });

  await assert.rejects(action, (error: unknown) => {
    assert.ok(error instanceof HeadlessBrowserError);
    assert.equal(error.code, 'document_changed');
    assert.deepEqual(error.diagnostic, {
      stage: 'checkbox_action_snapshot',
      reason: 'document_url_index',
      commandMethod: 'DOMSnapshot.captureSnapshot',
    });
    assert.equal(error.message, 'document_changed');
    assert.doesNotMatch(JSON.stringify(error.diagnostic), /private|sensitive/);
    return true;
  });
});

test('partial AX parser failures retain the Accessibility command boundary', async (t) => {
  const fixture = ownedSessionFixture(t);
  const action = fixture.session.setCheckbox(actionFor(performance.now() + 5_000));

  const identity = await fixture.pipeHarness.nextCommand();
  await answerActionCommand(fixture.pipeHarness, identity);
  const capture = await fixture.pipeHarness.nextCommand();
  await answerActionCommand(fixture.pipeHarness, capture);
  const ax = await fixture.pipeHarness.nextCommand();
  assert.equal(ax.method, 'Accessibility.getPartialAXTree');
  fixture.pipeHarness.respond(ax, { nodes: [{}] });

  await assert.rejects(action, (error: unknown) => {
    assert.ok(error instanceof HeadlessBrowserError);
    assert.equal(error.code, 'protocol_error');
    assert.deepEqual(error.diagnostic, {
      stage: 'checkbox_action_ax',
      commandMethod: 'Accessibility.getPartialAXTree',
    });
    return true;
  });
});

test('action accepts explicit false booleanOrUndefined AX values without treating unknown as false', async (t) => {
  const fixture = ownedSessionFixture(t);
  const action = fixture.session.setCheckbox(actionFor(performance.now() + 5_000));
  let inputWritten = false;

  for (let count = 0; count < 32 && !inputWritten; count += 1) {
    const command = await fixture.pipeHarness.nextCommand();
    if (command.method === 'Accessibility.getPartialAXTree') {
      fixture.pipeHarness.respond(command, { nodes: [{
        nodeId: 'checkbox-ax',
        ignored: false,
        frameId: expectedDocument.frameId,
        backendDOMNodeId: 104,
        role: { type: 'role', value: 'checkbox' },
        properties: [
          { name: 'checked', value: { type: 'booleanOrUndefined', value: false } },
          { name: 'disabled', value: { type: 'booleanOrUndefined', value: false } },
        ],
      }] });
    } else if (command.method === 'Input.synthesizeTapGesture') {
      inputWritten = true;
      fixture.pipeHarness.respond(command, {});
    } else {
      await answerActionCommand(fixture.pipeHarness, command);
    }
  }

  const result = await action;
  assert.equal(inputWritten, true);
  assert.equal(result.outcome, 'completed');
});

test('action accepts omitted optional AX frameId while retaining the exact document and hit-frame checks', async (t) => {
  const fixture = ownedSessionFixture(t);
  const action = fixture.session.setCheckbox(actionFor(performance.now() + 5_000));
  let inputWritten = false;

  for (let count = 0; count < 32 && !inputWritten; count += 1) {
    const command = await fixture.pipeHarness.nextCommand();
    if (command.method === 'Accessibility.getPartialAXTree') {
      const node: Record<string, unknown> = {
        nodeId: 'checkbox-ax',
        ignored: false,
        backendDOMNodeId: 104,
        role: { type: 'role', value: 'checkbox' },
        properties: [
          { name: 'checked', value: { type: 'boolean', value: false } },
          { name: 'disabled', value: { type: 'boolean', value: false } },
        ],
      };
      fixture.pipeHarness.respond(command, { nodes: [node] });
    } else if (command.method === 'Input.synthesizeTapGesture') {
      inputWritten = true;
      fixture.pipeHarness.respond(command, {});
    } else {
      await answerActionCommand(fixture.pipeHarness, command);
    }
  }

  const result = await action;
  assert.equal(inputWritten, true);
  assert.equal(result.outcome, 'completed');
});

test('action rejects an explicit partial AX frame conflict before Input', async (t) => {
  const fixture = ownedSessionFixture(t);
  const action = fixture.session.setCheckbox(actionFor(performance.now() + 5_000));

  let partialAXRead = false;
  for (let count = 0; count < 16 && !partialAXRead; count += 1) {
    const command = await fixture.pipeHarness.nextCommand();
    if (command.method === 'Accessibility.getPartialAXTree') {
      partialAXRead = true;
      fixture.pipeHarness.respond(command, { nodes: [{
        nodeId: 'checkbox-ax',
        ignored: false,
        frameId: 'foreign-frame',
        backendDOMNodeId: 104,
        role: { type: 'role', value: 'checkbox' },
        properties: [
          { name: 'checked', value: { type: 'boolean', value: false } },
          { name: 'disabled', value: { type: 'boolean', value: false } },
        ],
      }] });
    } else {
      await answerActionCommand(fixture.pipeHarness, command);
    }
  }

  const result = await action;
  assert.equal(partialAXRead, true);
  assert.equal(result.outcome, 'rejected');
  if (result.outcome === 'rejected') {
    assert.equal(result.failureCode, 'source_invalid');
    assert.deepEqual(result.diagnostic, {
      stage: 'checkbox_action_ax',
      commandMethod: 'Accessibility.getPartialAXTree',
    });
  }
  assert.equal(fixture.pipeHarness.pipe.isRetired, false);
});

test('hit testing accepts the protocol-optional frontend nodeId while checking backend and frame', async (t) => {
  const fixture = ownedSessionFixture(t);
  const action = fixture.session.setCheckbox(actionFor(performance.now() + 5_000));
  let hitTests = 0;
  let inputWritten = false;

  for (let count = 0; count < 32 && !inputWritten; count += 1) {
    const command = await fixture.pipeHarness.nextCommand();
    if (command.method === 'DOM.getNodeForLocation') {
      hitTests += 1;
      fixture.pipeHarness.respond(command, {
        backendNodeId: 104,
        frameId: expectedDocument.frameId,
      });
    } else if (command.method === 'Input.synthesizeTapGesture') {
      inputWritten = true;
      fixture.pipeHarness.respond(command, {});
    } else {
      await answerActionCommand(fixture.pipeHarness, command);
    }
  }

  const result = await action;
  assert.equal(hitTests, 2);
  assert.equal(inputWritten, true);
  assert.equal(result.outcome, 'completed');
});

test('hit-test response parser failures retain the fixed node-location command boundary', async (t) => {
  const fixture = ownedSessionFixture(t);
  const action = fixture.session.setCheckbox(actionFor(performance.now() + 5_000));

  let hitTestCount = 0;
  for (let count = 0; count < 16 && hitTestCount === 0; count += 1) {
    const command = await fixture.pipeHarness.nextCommand();
    if (command.method === 'DOM.getNodeForLocation') {
      hitTestCount += 1;
      fixture.pipeHarness.respond(command, { backendNodeId: 104, frameId: expectedDocument.frameId, nodeId: 'invalid' });
    } else {
      await answerActionCommand(fixture.pipeHarness, command);
    }
  }

  await assert.rejects(action, (error: unknown) => {
    assert.ok(error instanceof HeadlessBrowserError);
    assert.equal(error.code, 'protocol_error');
    assert.deepEqual(error.diagnostic, {
      stage: 'checkbox_action_hit_test',
      commandMethod: 'DOM.getNodeForLocation',
    });
    return true;
  });
  assert.equal(hitTestCount, 1);
});

test('tap point stays inside the target quad and unscrolled viewport', () => {
  assert.deepEqual(chooseOwnedCheckboxTapPoint([[10, 10, 30, 10, 30, 30, 10, 30]], {
    pageX: 0, pageY: 0, offsetX: 0, offsetY: 0, clientWidth: 100, clientHeight: 100,
  }), { x: 20, y: 20 });
  assert.equal(chooseOwnedCheckboxTapPoint([[10, 10, 30, 10, 30, 30, 10, 30]], {
    pageX: 1, pageY: 0, offsetX: 0, offsetY: 0, clientWidth: 100, clientHeight: 100,
  }), null);
  assert.equal(chooseOwnedCheckboxTapPoint([[90, 10, 110, 10, 110, 30, 90, 30]], {
    pageX: 0, pageY: 0, offsetX: 0, offsetY: 0, clientWidth: 100, clientHeight: 100,
  }), null);
});

test('queued page reads that finish past command_end are rejected before any Input write', async (t) => {
  const fixture = ownedSessionFixture(t);
  const deadline = performance.now() + 500;
  const replyGraceDeadline = deadline + 500;
  const capture = fixture.session.capturePng(expectedDocument);
  const action = fixture.session.setCheckbox(actionFor(deadline, 2));
  let frameTreeReads = 0;
  let finalIdentityReadFinishedAt = 0;

  for (let count = 0; count < 32; count += 1) {
    const command = await fixture.pipeHarness.nextCommand();
    assert.notEqual(command.method, 'Input.synthesizeTapGesture', 'no gesture may cross the fixed Core deadline');
    if (command.method === 'Page.getFrameTree') {
      frameTreeReads += 1;
      if (frameTreeReads === 1) await new Promise((resolve) => setTimeout(resolve, 150));
      if (frameTreeReads === 4) await new Promise((resolve) => setTimeout(resolve, 400));
    }
    await answerActionCommand(fixture.pipeHarness, command);
    if (frameTreeReads === 4) {
      finalIdentityReadFinishedAt = performance.now();
      break;
    }
  }

  await capture;
  const result = await action;
  assert.equal(frameTreeReads, 4, 'the last identity read followed the queued capture and checkbox rechecks');
  assert.ok(finalIdentityReadFinishedAt > deadline, 'the final frame read crossed command_end_ms');
  assert.ok(finalIdentityReadFinishedAt < replyGraceDeadline, 'the read still finished inside reply grace');
  assert.deepEqual(result, { outcome: 'rejected', failureCode: 'evidence_expired' });
  assert.equal(fixture.child.killed, false, 'a pre-input deadline rejection does not retire the page');
  assert.equal(fixture.pipeHarness.pipe.isRetired, false);
});

test('lost tap acknowledgement remains typed Unknown and kills the exact owned Chrome child', async (t) => {
  const fixture = ownedSessionFixture(t);
  const action = fixture.session.setCheckbox(actionFor(performance.now() + 5_000));
  let inputWasWritten = false;

  for (let count = 0; count < 32; count += 1) {
    const command = await fixture.pipeHarness.nextCommand();
    if (command.method === 'Input.synthesizeTapGesture') {
      inputWasWritten = true;
      fixture.pipeHarness.disconnect();
      break;
    }
    await answerActionCommand(fixture.pipeHarness, command);
  }

  assert.equal(inputWasWritten, true);
  await assert.rejects(action, (error: unknown) => error instanceof OwnedBrowserActionOutcomeUnknown);
  assert.equal(fixture.child.killed, true);
  assert.ok(fixture.child.signals.includes('SIGKILL'));
});
