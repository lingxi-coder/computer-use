import assert from 'node:assert/strict';
import { PassThrough } from 'node:stream';
import test from 'node:test';

import {
  CdpPipe,
  HeadlessBrowserError,
  type CdpPipeOptions,
} from '../src/main/headless/cdpPipe.js';

const FIXTURE_URL = 'file:///tmp/headless-fixture.html';
const CHECKBOX_SELECTORS = {
  native: 'input[type="checkbox" i]',
  disabled: 'input[type="checkbox" i]:disabled',
  checked: 'input[type="checkbox" i]:checked',
  indeterminate: 'input[type="checkbox" i]:indeterminate',
  blocked: 'input[type="checkbox" i][aria-disabled="true" i], [aria-disabled="true" i] input[type="checkbox" i], input[type="checkbox" i][inert], [inert] input[type="checkbox" i]',
} as const;
const CHECKBOX_SELECTOR_SEQUENCE = [
  ['native', CHECKBOX_SELECTORS.native],
  ['disabled', CHECKBOX_SELECTORS.disabled],
  ['checked', CHECKBOX_SELECTORS.checked],
  ['indeterminate', CHECKBOX_SELECTORS.indeterminate],
  ['blocked', CHECKBOX_SELECTORS.blocked],
  ['nativeAgain', CHECKBOX_SELECTORS.native],
] as const;

interface FixtureNode {
  readonly nodeId: number;
  readonly backendNodeId: number;
  readonly nodeType: number;
  readonly nodeName: string;
  readonly parentId: number | null;
  readonly children: FixtureNode[];
  readonly childNodeCount: number;
  readonly isSVG?: boolean;
}

function fixtureDocument(options: {
  readonly url?: string;
  readonly inputBackendIds?: readonly number[];
  readonly customBackendId?: number;
  readonly duplicateInputNodeId?: boolean;
  readonly incompleteBodyChildCount?: number;
} = {}): Record<string, unknown> {
  const inputBackendIds = options.inputBackendIds ?? [42, 49];
  const inputs: FixtureNode[] = inputBackendIds.map((backendNodeId, index) => ({
    nodeId: options.duplicateInputNodeId && index === 1 ? 10 : 10 + index,
    backendNodeId,
    nodeType: 1,
    nodeName: 'INPUT',
    parentId: 3,
    children: [],
    childNodeCount: 0,
    isSVG: false,
  }));
  const customNodeId = Math.max(19, ...inputs.map((node) => node.nodeId)) + 1;
  const custom: FixtureNode = {
    nodeId: customNodeId, backendNodeId: options.customBackendId ?? 48, nodeType: 1, nodeName: 'DIV',
    parentId: 3, children: [], childNodeCount: 0,
  };
  const bodyChildren = [...inputs, custom];
  const body: FixtureNode = {
    nodeId: 3, backendNodeId: 103, nodeType: 1, nodeName: 'BODY', parentId: 2,
    children: bodyChildren,
    childNodeCount: options.incompleteBodyChildCount ?? bodyChildren.length,
  };
  const html: FixtureNode = {
    nodeId: 2, backendNodeId: 102, nodeType: 1, nodeName: 'HTML', parentId: 1,
    children: [body], childNodeCount: 1,
  };
  const root: FixtureNode & { documentURL: string } = {
    nodeId: 1, backendNodeId: 101, nodeType: 9, nodeName: '#document', parentId: null,
    children: [html], childNodeCount: 1, documentURL: options.url ?? FIXTURE_URL,
  };
  return { root };
}

function defaultSelectorResults(inputNodeIds: readonly number[] = [10, 11]) {
  return {
    native: [...inputNodeIds],
    disabled: inputNodeIds.length > 0 ? [inputNodeIds[0]!] : [],
    checked: inputNodeIds.length > 1 ? [inputNodeIds[1]!] : [],
    indeterminate: inputNodeIds.length > 1 ? [inputNodeIds[1]!] : [],
    blocked: inputNodeIds.length > 0 ? [inputNodeIds[0]!] : [],
    nativeAgain: [...inputNodeIds],
  };
}

class PipeHarness {
  readonly readable = new PassThrough();
  readonly writable = new PassThrough();
  readonly pipe: CdpPipe;
  readonly retired: Promise<HeadlessBrowserError>;
  private resolveRetired!: (error: HeadlessBrowserError) => void;
  private output = Buffer.alloc(0);
  private commands: Record<string, unknown>[] = [];
  private commandWaiters: Array<(command: Record<string, unknown>) => void> = [];
  private commandCount = 0;

  constructor(options: Omit<CdpPipeOptions, 'onRetire'> = {}) {
    this.retired = new Promise((resolve) => { this.resolveRetired = resolve; });
    this.pipe = new CdpPipe(this.readable, this.writable, FIXTURE_URL, {
      ...options,
      onRetire: (error) => this.resolveRetired(error),
    });
    this.writable.on('data', (chunk: Buffer) => this.collectCommands(chunk));
  }

  async nextCommand(): Promise<Record<string, unknown>> {
    const command = this.commands.shift();
    if (command) return command;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Timed out waiting for a CDP command')), 1_000);
      this.commandWaiters.push((next) => {
        clearTimeout(timer);
        resolve(next);
      });
    });
  }

  send(value: unknown): void {
    this.sendBytes(Buffer.from(`${JSON.stringify(value)}\0`, 'utf8'));
  }

  sendBytes(bytes: Buffer): void { this.readable.write(bytes); }

  close(): void { this.pipe.close(); }

  get writtenCommandCount(): number { return this.commandCount; }

  private collectCommands(chunk: Buffer): void {
    this.output = this.output.length === 0 ? Buffer.from(chunk) : Buffer.concat([this.output, chunk]);
    while (true) {
      const delimiter = this.output.indexOf(0);
      if (delimiter < 0) return;
      const frame = this.output.subarray(0, delimiter);
      this.output = Buffer.from(this.output.subarray(delimiter + 1));
      const command = JSON.parse(frame.toString('utf8')) as Record<string, unknown>;
      this.commandCount += 1;
      const waiter = this.commandWaiters.shift();
      if (waiter) waiter(command);
      else this.commands.push(command);
    }
  }
}

function response(command: Record<string, unknown>, result: Record<string, unknown>): Buffer {
  return Buffer.from(`${JSON.stringify({ id: command.id, result,
    ...(typeof command.sessionId === 'string' ? { sessionId: command.sessionId } : {}) })}\0`, 'utf8');
}

async function beginCheckboxBatch(
  harness: PipeHarness,
  backendDOMNodeIds: readonly number[],
  options: {
    readonly before?: Record<string, unknown>;
    readonly after?: Record<string, unknown>;
    readonly selectorResults?: ReturnType<typeof defaultSelectorResults>;
    readonly stopAfterSelector?: typeof CHECKBOX_SELECTOR_SEQUENCE[number][0];
  } = {},
): Promise<{ states: Promise<unknown>; commands: Record<string, unknown>[] }> {
  const states = harness.pipe.readCheckboxStates('session-1', backendDOMNodeIds);
  const documentBefore = options.before ?? fixtureDocument();
  const documentAfter = options.after ?? documentBefore;
  const selectorResults = options.selectorResults ?? defaultSelectorResults();
  const commands: Record<string, unknown>[] = [];

  const firstDocumentCommand = await harness.nextCommand();
  assert.deepEqual(firstDocumentCommand, {
    id: firstDocumentCommand.id,
    method: 'DOM.getDocument',
    params: { depth: -1, pierce: false },
    sessionId: 'session-1',
  });
  commands.push(firstDocumentCommand);
  harness.sendBytes(response(firstDocumentCommand, documentBefore));

  for (const [resultKey, selector] of CHECKBOX_SELECTOR_SEQUENCE) {
    const command = await harness.nextCommand();
    assert.deepEqual(command, {
      id: command.id,
      method: 'DOM.querySelectorAll',
      params: { nodeId: 1, selector },
      sessionId: 'session-1',
    });
    commands.push(command);
    const nodeIds = selectorResults[resultKey];
    harness.sendBytes(response(command, { nodeIds }));
    if (options.stopAfterSelector === resultKey) return { states, commands };
  }

  const finalDocumentCommand = await harness.nextCommand();
  assert.deepEqual(finalDocumentCommand, {
    id: finalDocumentCommand.id,
    method: 'DOM.getDocument',
    params: { depth: -1, pierce: false },
    sessionId: 'session-1',
  });
  commands.push(finalDocumentCommand);
  harness.sendBytes(response(finalDocumentCommand, documentAfter));
  return { states, commands };
}

function assertCode(code: string) {
  return (error: unknown): boolean => error instanceof HeadlessBrowserError && error.code === code;
}

async function waitForRetirement(harness: PipeHarness): Promise<HeadlessBrowserError> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('Pipe did not retire')), 1_000);
    void harness.retired.then((error) => { clearTimeout(timer); resolve(error); });
  });
}

test('CdpPipe accepts a response split inside a UTF-8 code point', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const result = harness.pipe.createBrowserContext();
  const command = await harness.nextCommand();
  const frame = response(command, { browserContextId: 'context-窗' });
  const splitAt = frame.indexOf(Buffer.from('窗', 'utf8')) + 1;
  assert.ok(splitAt > 0);
  harness.sendBytes(frame.subarray(0, splitAt));
  await new Promise<void>((resolve) => setImmediate(resolve));
  harness.sendBytes(frame.subarray(splitAt));

  assert.equal(await result, 'context-窗');
  assert.equal(harness.pipe.isRetired, false);
});

test('CdpPipe accepts coalesced response frames for separate fixed wrappers', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const discovery = harness.pipe.setTargetDiscovery(true);
  const targets = harness.pipe.getTargets();
  const [discoveryCommand, targetsCommand] = await Promise.all([
    harness.nextCommand(), harness.nextCommand(),
  ]);
  harness.sendBytes(Buffer.concat([
    response(discoveryCommand, {}),
    response(targetsCommand, { targetInfos: [] }),
  ]));

  await Promise.all([discovery, targets.then((value) => assert.deepEqual(value, []))]);
  assert.equal(harness.pipe.isRetired, false);
});

test('target discovery and roster requests use only the fixed page filter', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());
  const pageFilter = [{ type: 'page', exclude: false }, { exclude: true }];

  const enable = harness.pipe.setTargetDiscovery(true);
  const enableCommand = await harness.nextCommand();
  assert.deepEqual(enableCommand, {
    id: enableCommand.id,
    method: 'Target.setDiscoverTargets',
    params: { discover: true, filter: pageFilter },
  });
  harness.sendBytes(response(enableCommand, {}));
  await enable;

  const targets = harness.pipe.getTargets();
  const targetsCommand = await harness.nextCommand();
  assert.deepEqual(targetsCommand, {
    id: targetsCommand.id,
    method: 'Target.getTargets',
    params: { filter: pageFilter },
  });
  harness.sendBytes(response(targetsCommand, { targetInfos: [] }));
  assert.deepEqual(await targets, []);

  const disable = harness.pipe.setTargetDiscovery(false);
  const disableCommand = await harness.nextCommand();
  assert.deepEqual(disableCommand, {
    id: disableCommand.id,
    method: 'Target.setDiscoverTargets',
    params: { discover: false },
  });
  harness.sendBytes(response(disableCommand, {}));
  await disable;

  assert.equal(harness.writtenCommandCount, 3);
  assert.equal(harness.pipe.isRetired, false);
});

test('a bounded startup target-discovery timeout accepts a slow ACK without extending later commands', async (t) => {
  const harness = new PipeHarness({ commandTimeoutMs: 25 });
  t.after(() => harness.close());

  const discovery = harness.pipe.setTargetDiscovery(true, 500);
  const discoveryCommand = await harness.nextCommand();
  await new Promise<void>((resolve) => setTimeout(resolve, 80));
  harness.sendBytes(response(discoveryCommand, {}));
  await discovery;
  assert.equal(harness.pipe.isRetired, false);

  const roster = harness.pipe.getTargets();
  await harness.nextCommand();
  await assert.rejects(roster, assertCode('command_timeout'));
  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 2);
});

test('a command-specific CDP timeout rejects invalid bounds before writing', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  for (const timeoutMs of [0, -1, Number.NaN, Number.POSITIVE_INFINITY, 1.5, 20_001]) {
    await assert.rejects(harness.pipe.setTargetDiscovery(true, timeoutMs), assertCode('invalid_input'));
  }
  assert.equal(harness.writtenCommandCount, 0);
  assert.equal(harness.pipe.isRetired, false);

  const atHardCap = harness.pipe.setTargetDiscovery(true, 20_000);
  const command = await harness.nextCommand();
  harness.sendBytes(response(command, {}));
  await atHardCap;
  assert.equal(harness.writtenCommandCount, 1);
});

test('checkbox batches use bounded fixed DOM selectors and preserve input order and mixed state', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const empty = await harness.pipe.readCheckboxStates('session-1', []);
  assert.deepEqual(empty, []);
  assert.equal(harness.writtenCommandCount, 0);
  assert.equal('evaluate' in harness.pipe, false);
  assert.equal('callFunctionOn' in harness.pipe, false);
  assert.equal('querySelectorAll' in harness.pipe, false);
  assert.equal('readCheckboxState' in harness.pipe, false);

  const { states, commands } = await beginCheckboxBatch(harness, [49, 48, 42]);
  assert.deepEqual(await states, [
    {
      backendDOMNodeId: 49,
      disabled: false,
      checked: true,
      indeterminate: true,
      documentUrl: FIXTURE_URL,
      connected: true,
      topDocument: true,
      nativeCheckbox: true,
      blockedByAriaOrInert: false,
    },
    {
      backendDOMNodeId: 48,
      disabled: false,
      checked: false,
      indeterminate: false,
      documentUrl: FIXTURE_URL,
      connected: true,
      topDocument: true,
      nativeCheckbox: false,
      blockedByAriaOrInert: false,
    },
    {
      backendDOMNodeId: 42,
      disabled: true,
      checked: false,
      indeterminate: false,
      documentUrl: FIXTURE_URL,
      connected: true,
      topDocument: true,
      nativeCheckbox: true,
      blockedByAriaOrInert: true,
    },
  ]);
  assert.deepEqual(commands.map((command) => command.method), [
    'DOM.getDocument',
    'DOM.querySelectorAll', 'DOM.querySelectorAll', 'DOM.querySelectorAll',
    'DOM.querySelectorAll', 'DOM.querySelectorAll',
    'DOM.querySelectorAll', 'DOM.getDocument',
  ]);
  assert.equal(harness.pipe.isRetired, false);
  assert.equal(harness.writtenCommandCount, 8);
});

test('checkbox batches retain actual backend identity when getDocument renumbers frontend IDs', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());
  const after = structuredClone(fixtureDocument());
  const renumber = (node: FixtureNode): void => {
    const mutable = node as { nodeId: number; parentId: number | null; children: FixtureNode[] };
    mutable.nodeId += 1_000;
    if (mutable.parentId !== null) mutable.parentId += 1_000;
    mutable.children.forEach(renumber);
  };
  renumber(after.root as FixtureNode);
  const { states } = await beginCheckboxBatch(harness, [42], { after });
  const result = await states as Array<{ backendDOMNodeId: number; nativeCheckbox: boolean }>;
  assert.equal(result[0]?.backendDOMNodeId, 42);
  assert.equal(result[0]?.nativeCheckbox, true);
  assert.equal(harness.pipe.isRetired, false);
});

test('checkbox batches reject page shadow roots before any selector query', async (t) => {
  for (const shadowRootType of ['open', 'closed']) {
    await t.test(shadowRootType, async (subtest) => {
      const harness = new PipeHarness();
      subtest.after(() => harness.close());
      const document = fixtureDocument();
      const root = document.root as FixtureNode;
      const custom = root.children[0]!.children[0]!.children[2]!;
      (custom as unknown as Record<string, unknown>).shadowRoots = [{ shadowRootType }];
      const states = harness.pipe.readCheckboxStates('session-1', [42]);
      const command = await harness.nextCommand();
      harness.sendBytes(response(command, document));
      await assert.rejects(states, (error: unknown) => error instanceof HeadlessBrowserError
        && error.code === 'protocol_error' && error.diagnostic?.stage === 'checkbox_document_shape');
      assert.equal(harness.pipe.isRetired, true);
      assert.equal(harness.writtenCommandCount, 1);
    });
  }
});

test('checkbox batches reject a backend identity replacement between the two DOM reads', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const { states } = await beginCheckboxBatch(harness, [42], {
    after: fixtureDocument({ inputBackendIds: [142, 149] }),
  });
  await assert.rejects(states, (error: unknown) => {
    assert.ok(error instanceof HeadlessBrowserError);
    assert.deepEqual(error.diagnostic, { stage: 'checkbox_document_changed', commandMethod: 'DOM.getDocument' });
    return error.code === 'protocol_error';
  });
  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 8);
});

test('checkbox batches reject non-native nodes returned by a native-state selector', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const selectorResults = { ...defaultSelectorResults(), disabled: [20] };
  const { states } = await beginCheckboxBatch(harness, [42], { selectorResults });
  await assert.rejects(states, assertCode('protocol_error'));
  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 8);
});

test('checkbox selector results must map to nodes in the validated root graph', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const selectorResults = { ...defaultSelectorResults(), checked: [999] };
  const { states } = await beginCheckboxBatch(harness, [42], {
    selectorResults,
    stopAfterSelector: 'checked',
  });
  await assert.rejects(states, assertCode('protocol_error'));
  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 4);
});

test('checkbox batches reject a document URL outside the owned fixture', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const states = harness.pipe.readCheckboxStates('session-1', [42]);
  const command = await harness.nextCommand();
  assert.deepEqual(command, {
    id: command.id,
    method: 'DOM.getDocument',
    params: { depth: -1, pierce: false },
    sessionId: 'session-1',
  });
  harness.sendBytes(response(command, fixtureDocument({ url: 'https://foreign.example/' })));

  await assert.rejects(states, (error: unknown) => {
    assert.ok(error instanceof HeadlessBrowserError);
    assert.deepEqual(error.diagnostic, { stage: 'checkbox_document_root', commandMethod: 'DOM.getDocument' });
    return error.code === 'protocol_error';
  });
  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 1);
});

test('checkbox DOM graph rejects duplicate node and backend identities', async (t) => {
  const duplicateBackend = new PipeHarness();
  t.after(() => duplicateBackend.close());
  const backendStates = duplicateBackend.pipe.readCheckboxStates('session-1', [42]);
  const backendCommand = await duplicateBackend.nextCommand();
  duplicateBackend.sendBytes(response(backendCommand, fixtureDocument({ inputBackendIds: [42, 42] })));
  await assert.rejects(backendStates, assertCode('protocol_error'));
  assert.equal(duplicateBackend.writtenCommandCount, 1);

  const duplicateFrontend = new PipeHarness();
  t.after(() => duplicateFrontend.close());
  const frontendStates = duplicateFrontend.pipe.readCheckboxStates('session-1', [42]);
  const frontendCommand = await duplicateFrontend.nextCommand();
  duplicateFrontend.sendBytes(response(frontendCommand, fixtureDocument({ duplicateInputNodeId: true })));
  await assert.rejects(frontendStates, assertCode('protocol_error'));
  assert.equal(duplicateFrontend.writtenCommandCount, 1);
});

test('checkbox DOM graph rejects incomplete child counts', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const states = harness.pipe.readCheckboxStates('session-1', [42]);
  const command = await harness.nextCommand();
  harness.sendBytes(response(command, fixtureDocument({ incompleteBodyChildCount: 1 })));

  await assert.rejects(states, assertCode('protocol_error'));
  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 1);
});

test('checkbox selector results are capped at 32 native nodes', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const backendIds = Array.from({ length: 33 }, (_, index) => 1_000 + index);
  const nodeIds = Array.from({ length: 33 }, (_, index) => 10 + index);
  const selectorResults = defaultSelectorResults(nodeIds);
  const { states } = await beginCheckboxBatch(harness, [backendIds[0]!], {
    before: fixtureDocument({ inputBackendIds: backendIds }),
    selectorResults,
    stopAfterSelector: 'native',
  });

  await assert.rejects(states, assertCode('protocol_error'));
  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 2);
});

test('checkbox batches reject duplicate requested and selector IDs without arbitrary selectors', async (t) => {
  const duplicateRequestHarness = new PipeHarness();
  t.after(() => duplicateRequestHarness.close());
  await assert.rejects(
    duplicateRequestHarness.pipe.readCheckboxStates('session-1', [42, 42]),
    assertCode('invalid_input'),
  );
  assert.equal(duplicateRequestHarness.writtenCommandCount, 0);

  const duplicateSelectorHarness = new PipeHarness();
  t.after(() => duplicateSelectorHarness.close());
  const selectorResults = { ...defaultSelectorResults(), native: [10, 10] };
  const { states } = await beginCheckboxBatch(duplicateSelectorHarness, [42], {
    selectorResults,
    stopAfterSelector: 'native',
  });
  await assert.rejects(states, assertCode('protocol_error'));
  assert.equal(duplicateSelectorHarness.pipe.isRetired, true);
  assert.equal(duplicateSelectorHarness.writtenCommandCount, 2);
});

test('CdpPipe retires on an inbound frame beyond the configured byte limit', async (t) => {
  const harness = new PipeHarness({ maxFrameBytes: 1_024 });
  t.after(() => harness.close());

  harness.sendBytes(Buffer.alloc(1_025, 0x61));
  const error = await waitForRetirement(harness);
  assert.equal(error.code, 'protocol_error');
  assert.equal(harness.pipe.isRetired, true);
});

test('CdpPipe retires when UTF-8 framing is malformed', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  harness.sendBytes(Buffer.from([0xff, 0x00]));
  const error = await waitForRetirement(harness);
  assert.equal(error.code, 'protocol_error');
});

test('an unknown response id retires the pipe and rejects its pending command', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const result = harness.pipe.createBrowserContext();
  await harness.nextCommand();
  harness.send({ id: 99_999, result: { browserContextId: 'unexpected' } });

  await assert.rejects(result, assertCode('protocol_error'));
  assert.equal(harness.pipe.isRetired, true);
});

test('CDP error responses expose bounded diagnostics and retire without leaking payloads', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const result = harness.pipe.createBrowserContext();
  const command = await harness.nextCommand();
  harness.send({
    id: command.id,
    error: { code: -32_000, message: 'sensitive protocol text', data: { token: 'sensitive payload' } },
  });

  await assert.rejects(result, (error: unknown) => {
    assert.ok(error instanceof HeadlessBrowserError);
    assert.equal(error.code, 'protocol_error');
    assert.deepEqual(error.diagnostic, {
      stage: 'cdp_response',
      commandMethod: 'Target.createBrowserContext',
      nativeErrorCode: -32_000,
    });
    assert.equal(error.message, 'protocol_error');
    assert.equal(Object.hasOwn(error, 'data'), false);
    assert.doesNotMatch(JSON.stringify(error.diagnostic), /sensitive|token|payload/);
    assert.doesNotMatch(error.stack ?? '', /sensitive|token|payload/);
    return true;
  });
  assert.equal(harness.pipe.isRetired, true);
});

test('Headless diagnostics merge a missing CDP command without replacing a parser stage', () => {
  const error = new HeadlessBrowserError('protocol_error', { stage: 'checkbox_action_ax' })
    .withDiagnostic({ stage: 'invalid_ack', commandMethod: 'Accessibility.getPartialAXTree', nativeErrorCode: -32_000 });

  assert.deepEqual(error.diagnostic, {
    stage: 'checkbox_action_ax',
    commandMethod: 'Accessibility.getPartialAXTree',
    nativeErrorCode: -32_000,
  });
});

test('a malformed fixed-wrapper result retires the transport', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const result = harness.pipe.createBrowserContext();
  const command = await harness.nextCommand();
  harness.sendBytes(response(command, { browserContextId: '' }));

  await assert.rejects(result, assertCode('protocol_error'));
  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 1);
});

test('getTargets retires the transport when its target list has the wrong shape', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const result = harness.pipe.getTargets();
  const command = await harness.nextCommand();
  harness.sendBytes(response(command, { targetInfos: 'not-an-array' }));

  await assert.rejects(result, assertCode('protocol_error'));
  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 1);
});

test('a response from a different CDP session retires the transport', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const result = harness.pipe.enablePage('expected-session');
  const command = await harness.nextCommand();
  harness.send({ id: command.id, result: {}, sessionId: 'other-session' });

  await assert.rejects(result, assertCode('protocol_error'));
  assert.equal(harness.pipe.isRetired, true);
});

test('excessively deep JSON retires the transport', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  let nested: unknown = 'leaf';
  for (let depth = 0; depth < 66; depth += 1) nested = { child: nested };
  harness.send({ method: 'Page.deepPayload', params: { nested } });

  const error = await waitForRetirement(harness);
  assert.equal(error.code, 'protocol_error');
});

test('a command timeout permanently retires the pipe without retrying later calls', async (t) => {
  const harness = new PipeHarness({ commandTimeoutMs: 25 });
  t.after(() => harness.close());

  const result = harness.pipe.createBrowserContext();
  await harness.nextCommand();
  await assert.rejects(result, assertCode('command_timeout'));
  await assert.rejects(harness.pipe.getTargets(), assertCode('command_timeout'));

  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 1);
});

test('a startup discovery timeout permanently retires the pipe without retrying discovery', async (t) => {
  const harness = new PipeHarness({ commandTimeoutMs: 500 });
  t.after(() => harness.close());

  const discovery = harness.pipe.setTargetDiscovery(true, 25);
  await harness.nextCommand();
  await assert.rejects(discovery, assertCode('command_timeout'));
  await assert.rejects(harness.pipe.getTargets(), assertCode('command_timeout'));

  assert.equal(harness.pipe.isRetired, true);
  assert.equal(harness.writtenCommandCount, 1);
});

test('the total queued-event budget spans distinct method and session keys', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const frames: Buffer[] = [];
  for (let index = 0; index < 129; index += 1) {
    frames.push(Buffer.from(`${JSON.stringify({
      method: `Page.event${index}`,
      params: {},
      sessionId: `session-${index}`,
    })}\0`, 'utf8'));
  }
  harness.sendBytes(Buffer.concat(frames));

  const error = await waitForRetirement(harness);
  assert.equal(error.code, 'protocol_error');
});

test('close rejects an outstanding event waiter promptly', async (t) => {
  const harness = new PipeHarness();
  t.after(() => harness.close());

  const waiting = harness.pipe.waitForEvent('Page.loadEventFired', 'session-1', 5_000);
  harness.close();
  await assert.rejects(waiting, assertCode('closed'));
  assert.equal(harness.pipe.isRetired, true);
});

test('CdpPipe constructor rejects non-file and decorated fixture URLs', () => {
  for (const url of ['https://example.test/fixture.html', 'file:///tmp/fixture.html?query=1', 'file:///tmp/fixture.html#fragment']) {
    assert.throws(() => new CdpPipe(new PassThrough(), new PassThrough(), url), assertCode('invalid_input'));
  }
});
