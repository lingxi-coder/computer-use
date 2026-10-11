import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import test, { type TestContext } from 'node:test';

import { HeadlessBrowserError, type HeadlessObservation } from '../src/main/headless/cdpPipe.js';
import { launchOwnedBrowser, type OwnedBrowserSession } from '../src/main/headless/ownedBrowser.js';

const DOCUMENT_FRAME_ID = 'fixture-frame';
const INITIAL_LOADER_ID = 'fixture-loader';
const SENTINEL_ENV_KEY = 'CU_HEADLESS_TEST_SECRET_SENTINEL';

interface AxNodeFixture {
  nodeId: string;
  ignored: boolean;
  role: { type: string; value: string };
  backendDOMNodeId: number;
}

interface FakePeerScenario {
  axNodes?: AxNodeFixture[];
  stateOverrides?: Record<string, Partial<{
    disabled: boolean;
    checked: boolean;
    indeterminate: boolean;
    nativeCheckbox: boolean;
    blockedByAriaOrInert: boolean;
  }>>;
  documentURLOverride?: string;
  loaderChangeAtFrameTreeCall?: number;
  delayQueryIndex?: number;
  delayMs?: number;
}

interface FakePeerLogEntry {
  kind: 'startup' | 'command';
  method?: string;
  params?: Record<string, unknown>;
  sessionId?: string;
  sentinelPresent?: boolean;
}

interface OwnedPeerHarness {
  root: string;
  profileParent: string;
  logPath: string;
  session: OwnedBrowserSession;
  readLog(): FakePeerLogEntry[];
}

function checkboxNodes(backendIds: readonly number[]): AxNodeFixture[] {
  return backendIds.map((backendDOMNodeId, index) => ({
    nodeId: `checkbox-${index + 1}`,
    ignored: false,
    role: { type: 'role', value: 'checkbox' },
    backendDOMNodeId,
  }));
}

function fakePeerSource(scenario: FakePeerScenario, fixtureURL: string, logPath: string): string {
  return `#!${process.execPath}
import { appendFileSync, createReadStream, writeSync } from 'node:fs';

const scenario = ${JSON.stringify(scenario)};
const fixtureURL = ${JSON.stringify(fixtureURL)};
const logPath = ${JSON.stringify(logPath)};
const targetId = 'fixture-target';
const contextId = 'fixture-context';
const sessionId = 'fixture-session';
const frameId = ${JSON.stringify(DOCUMENT_FRAME_ID)};
let targetCreated = false;
let frameTreeCalls = 0;
let selectorQueryCalls = 0;
let incoming = Buffer.alloc(0);
let work = Promise.resolve();

function log(entry) {
  appendFileSync(logPath, JSON.stringify(entry) + '\\n');
}

function send(message) {
  writeSync(4, Buffer.from(JSON.stringify(message) + '\\0'));
}

function event(method, params, commandSessionId) {
  send({ method, params, ...(commandSessionId ? { sessionId: commandSessionId } : {}) });
}

function reply(command, result) {
  send({ id: command.id, result, ...(command.sessionId ? { sessionId: command.sessionId } : {}) });
}

function targetInfo() {
  return { targetId, type: 'page', url: 'about:blank', browserContextId: contextId };
}

function stateFor(backendDOMNodeId) {
  const isNative = backendDOMNodeId === 51;
  return {
    disabled: isNative,
    checked: isNative,
    indeterminate: false,
    nativeCheckbox: isNative,
    blockedByAriaOrInert: false,
    ...(scenario.stateOverrides?.[String(backendDOMNodeId)] ?? {}),
  };
}

function domDocument() {
  const documentNodeId = 100000;
  const htmlNodeId = 100001;
  const headNodeId = 100002;
  const bodyNodeId = 100003;
  const inputNodeId = 100051;
  const customNodeId = 100052;
  return { root: {
    nodeId: documentNodeId,
    backendNodeId: 200000,
    nodeType: 9,
    nodeName: '#document',
    documentURL: scenario.documentURLOverride ?? fixtureURL,
    childNodeCount: 1,
    children: [{
      nodeId: htmlNodeId,
      backendNodeId: 200001,
      parentId: documentNodeId,
      nodeType: 1,
      nodeName: 'HTML',
      childNodeCount: 2,
      children: [
        { nodeId: headNodeId, backendNodeId: 200002, parentId: htmlNodeId, nodeType: 1, nodeName: 'HEAD', childNodeCount: 0, children: [] },
        { nodeId: bodyNodeId, backendNodeId: 200003, parentId: htmlNodeId, nodeType: 1, nodeName: 'BODY', childNodeCount: 2, children: [
          { nodeId: inputNodeId, backendNodeId: 51, parentId: bodyNodeId, nodeType: 1, nodeName: 'INPUT', childNodeCount: 0, children: [] },
          { nodeId: customNodeId, backendNodeId: 52, parentId: bodyNodeId, nodeType: 1, nodeName: 'DIV', childNodeCount: 0, children: [] },
        ] },
      ],
    }],
  } };
}

function backendIdsForSelector(selector) {
  const kind = selector.toLowerCase();
  return [51, 52].filter((backendDOMNodeId) => {
    const state = stateFor(backendDOMNodeId);
    if (kind.includes(':disabled')) return state.nativeCheckbox && state.disabled;
    if (kind.includes(':checked')) return state.nativeCheckbox && state.checked;
    if (kind.includes(':indeterminate')) return state.nativeCheckbox && state.indeterminate;
    if (kind.includes('aria-disabled') || kind.includes('inert')) {
      return state.nativeCheckbox && state.blockedByAriaOrInert;
    }
    return state.nativeCheckbox;
  });
}

async function handle(command) {
  log({ kind: 'command', method: command.method, params: command.params, ...(command.sessionId ? { sessionId: command.sessionId } : {}) });
  switch (command.method) {
    case 'Target.setDiscoverTargets':
    case 'Target.disposeBrowserContext':
    case 'Target.closeTarget':
      reply(command, command.method === 'Target.closeTarget' ? { success: true } : {});
      return;
    case 'Target.getTargets':
      reply(command, { targetInfos: targetCreated ? [targetInfo()] : [] });
      return;
    case 'Target.createBrowserContext':
      reply(command, { browserContextId: contextId });
      return;
    case 'Target.createTarget':
      targetCreated = true;
      event('Target.targetCreated', { targetInfo: targetInfo() });
      reply(command, { targetId });
      return;
    case 'Target.attachToTarget':
      reply(command, { sessionId });
      return;
    case 'Page.enable':
    case 'Accessibility.enable':
    case 'Emulation.setScriptExecutionDisabled':
    case 'Network.enable':
    case 'Network.setBlockedURLs':
    case 'Browser.setDownloadBehavior':
      reply(command, {});
      return;
    case 'Page.navigate':
      event('Page.frameNavigated', { frame: { id: frameId, loaderId: ${JSON.stringify(INITIAL_LOADER_ID)}, url: fixtureURL } }, sessionId);
      event('Page.loadEventFired', { timestamp: 1 }, sessionId);
      reply(command, { frameId, loaderId: ${JSON.stringify(INITIAL_LOADER_ID)} });
      return;
    case 'Page.getFrameTree': {
      frameTreeCalls += 1;
      const changed = scenario.loaderChangeAtFrameTreeCall === frameTreeCalls;
      reply(command, { frameTree: { frame: {
        id: frameId,
        loaderId: changed ? 'changed-loader' : ${JSON.stringify(INITIAL_LOADER_ID)},
        url: fixtureURL,
      } } });
      return;
    }
    case 'Accessibility.getFullAXTree':
      reply(command, { nodes: scenario.axNodes ?? ${JSON.stringify(checkboxNodes([51, 52]))} });
      return;
    case 'DOM.getDocument':
      reply(command, domDocument());
      return;
    case 'DOM.querySelectorAll': {
      selectorQueryCalls += 1;
      if (scenario.delayQueryIndex === selectorQueryCalls && (scenario.delayMs ?? 0) > 0) {
        await new Promise((resolve) => setTimeout(resolve, scenario.delayMs));
      }
      const nodeIdByBackendId = { 51: 100051, 52: 100052 };
      const nodeIds = backendIdsForSelector(command.params.selector).map((backendDOMNodeId) => nodeIdByBackendId[backendDOMNodeId]);
      reply(command, { nodeIds });
      return;
    }
    case 'Page.getLayoutMetrics':
      reply(command, {
        cssLayoutViewport: { clientWidth: 800, clientHeight: 600 },
        cssVisualViewport: { clientWidth: 800, clientHeight: 600, scale: 1, offsetX: 0, offsetY: 0 },
        cssContentSize: { width: 800, height: 1200 },
      });
      return;
    default:
      throw new Error('Unexpected fake CDP command: ' + command.method);
  }
}

log({ kind: 'startup', sentinelPresent: Object.hasOwn(process.env, ${JSON.stringify(SENTINEL_ENV_KEY)}) });
const input = createReadStream(null, { fd: 3, autoClose: false });
input.on('data', (chunk) => {
  incoming = Buffer.concat([incoming, chunk]);
  while (true) {
    const delimiter = incoming.indexOf(0);
    if (delimiter < 0) break;
    const frame = incoming.subarray(0, delimiter);
    incoming = Buffer.from(incoming.subarray(delimiter + 1));
    const command = JSON.parse(frame.toString('utf8'));
    work = work.then(() => handle(command));
  }
});
input.on('end', () => { void work.finally(() => process.exit(0)); });
`;
}

async function launchFakePeer(t: TestContext, scenario: FakePeerScenario = {}): Promise<OwnedPeerHarness> {
  const root = mkdtempSync(join(realpathSync(tmpdir()), 'cu-headless-observe-test-'));
  const profileParent = join(root, 'profiles');
  const fixturePath = join(root, 'fixture.html');
  const executable = join(root, 'fake-cdp-peer.mjs');
  const logPath = join(root, 'cdp-log.jsonl');
  mkdirSync(profileParent);
  writeFileSync(fixturePath, '<!doctype html><title>fake peer fixture</title>');
  const fixtureURL = pathToFileURL(fixturePath).href;
  writeFileSync(executable, fakePeerSource(scenario, fixtureURL, logPath), { mode: 0o700 });
  chmodSync(executable, 0o700);

  let session: OwnedBrowserSession;
  const oldSentinel = process.env[SENTINEL_ENV_KEY];
  let launch: Promise<OwnedBrowserSession>;
  process.env[SENTINEL_ENV_KEY] = 'must-not-reach-the-owned-child';
  try {
    // spawn happens before launchOwnedBrowser reaches its first await, so the peer
    // observes this sentinel only if the owned-browser child environment leaks it.
    launch = launchOwnedBrowser({ chromeExecutable: executable, fixturePath, profileParent });
  } finally {
    if (oldSentinel === undefined) delete process.env[SENTINEL_ENV_KEY];
    else process.env[SENTINEL_ENV_KEY] = oldSentinel;
  }
  try {
    session = await launch;
  } catch (error) {
    rmSync(root, { recursive: true, force: true });
    throw error;
  }
  t.after(async () => {
    try { await session.close(); }
    finally { rmSync(root, { recursive: true, force: true }); }
  });
  return {
    root,
    profileParent,
    logPath,
    session,
    readLog: () => existsSync(logPath)
      ? readFileSync(logPath, 'utf8').trim().split('\n').filter(Boolean).map((line) => JSON.parse(line) as FakePeerLogEntry)
      : [],
  };
}

function commandEntries(harness: OwnedPeerHarness): FakePeerLogEntry[] {
  return harness.readLog().filter((entry) => entry.kind === 'command');
}

function assertErrorCode(error: unknown, code: HeadlessBrowserError['code']): boolean {
  return error instanceof HeadlessBrowserError && error.code === code;
}

test('owned observation brackets DOM-native checkbox reads and returns provenance', async (t) => {
  const harness = await launchFakePeer(t, {
    delayQueryIndex: 3,
    delayMs: 45,
    stateOverrides: { '51': { indeterminate: true, blockedByAriaOrInert: true } },
  });
  const observation: HeadlessObservation = await harness.session.observe();

  assert.deepEqual(observation.document, {
    frameId: DOCUMENT_FRAME_ID,
    loaderId: INITIAL_LOADER_ID,
    url: pathToFileURL(join(harness.root, 'fixture.html')).href,
  });
  assert.equal(observation.rawAX.nodes.length, 2);
  assert.deepEqual(observation.checkboxStates?.map((state) => ({
    backendDOMNodeId: state.backendDOMNodeId,
    disabled: state.disabled,
    checked: state.checked,
    indeterminate: state.indeterminate,
    nativeCheckbox: state.nativeCheckbox,
    blockedByAriaOrInert: state.blockedByAriaOrInert,
  })), [{
    backendDOMNodeId: 51,
    disabled: true,
    checked: true,
    indeterminate: true,
    nativeCheckbox: true,
    blockedByAriaOrInert: true,
  }],
  'custom ARIA checkboxes stay in the AX tree without being reported as native input state');
  assert.ok(observation.provenance.readStartedMonotonicMs <= observation.provenance.readFinishedMonotonicMs);
  assert.ok(observation.provenance.readFinishedMonotonicMs - observation.provenance.readStartedMonotonicMs >= 30,
    'the observation interval includes the delayed checkbox read');
  assert.equal(observation.provenance.sequence, 1);
  assert.equal(observation.provenance.clockDomain, 'headless-performance-monotonic-ms');

  const commands = commandEntries(harness);
  const axIndex = commands.findIndex((entry) => entry.method === 'Accessibility.getFullAXTree');
  assert.ok(axIndex > 0);
  assert.equal(commands[axIndex - 1]?.method, 'Page.getFrameTree', 'the observation begins with document identity');
  const observationReads = commands.slice(axIndex - 1).filter((entry) => [
    'Page.getFrameTree', 'Accessibility.getFullAXTree', 'DOM.getDocument',
    'DOM.querySelectorAll', 'Page.getLayoutMetrics',
  ].includes(entry.method ?? ''));
  assert.deepEqual(observationReads.map((entry) => entry.method), [
    'Page.getFrameTree',
    'Accessibility.getFullAXTree',
    'DOM.getDocument',
    'DOM.querySelectorAll', 'DOM.querySelectorAll', 'DOM.querySelectorAll',
    'DOM.querySelectorAll', 'DOM.querySelectorAll', 'DOM.querySelectorAll',
    'DOM.getDocument',
    'Page.getLayoutMetrics', 'Page.getFrameTree',
  ]);
  const domDocuments = commands.filter((entry) => entry.method === 'DOM.getDocument');
  assert.equal(domDocuments.length, 2, 'the complete DOM tree is re-read to bracket selector queries');
  assert.ok(domDocuments.every((entry) => entry.params?.depth === -1 && entry.params?.pierce === false));
  const queries = commands.filter((entry) => entry.method === 'DOM.querySelectorAll');
  assert.equal(queries.length, 6, 'the bounded state read uses six fixed selector passes');
  const queryRoot = queries[0]?.params?.nodeId;
  assert.ok(queries.every((entry) => entry.params?.nodeId === queryRoot), 'each query is scoped to the same document root');
  assert.equal(queries[0]?.params?.selector, queries[5]?.params?.selector,
    'the native-checkbox set is checked again after collecting state');
  const startup = harness.readLog().find((entry) => entry.kind === 'startup');
  assert.equal(startup?.sentinelPresent, false, 'ambient environment values are not passed to the owned child');

  await harness.session.close();
  assert.deepEqual(readdirSync(harness.profileParent), [], 'closing removes the owned temporary profile');
});

test('owned observation rejects a mismatched complete DOM document before selector queries', async (t) => {
  const harness = await launchFakePeer(t, { documentURLOverride: 'file:///different-document.html' });

  await assert.rejects(harness.session.observe(), (error: unknown) => assertErrorCode(error, 'protocol_error'));
  await harness.session.close();

  const commands = commandEntries(harness);
  assert.equal(commands.filter((entry) => entry.method === 'DOM.getDocument').length, 1);
  assert.equal(commands.filter((entry) => entry.method === 'DOM.querySelectorAll').length, 0,
    'a document URL mismatch stops selector queries');
  assert.equal(commands.filter((entry) => entry.method === 'Page.getLayoutMetrics').length, 0,
    'metrics are not read after a DOM identity mismatch');
  assert.equal(commands.filter((entry) => entry.method === 'Page.getFrameTree').length, 2,
    'no final observation identity read occurs after the mismatch');
  assert.deepEqual(readdirSync(harness.profileParent), [], 'retirement still removes the owned profile');
});

test('owned observation retires when the document loader changes during checkbox reads', async (t) => {
  const harness = await launchFakePeer(t, { axNodes: checkboxNodes([51]), loaderChangeAtFrameTreeCall: 3 });

  await assert.rejects(harness.session.observe(), (error: unknown) => assertErrorCode(error, 'document_changed'));
  await harness.session.close();

  const commands = commandEntries(harness);
  assert.equal(commands.filter((entry) => entry.method === 'Page.getFrameTree').length, 3,
    'the third identity read detects the changed loader and no later read is started');
  assert.equal(commands.filter((entry) => entry.method === 'DOM.getDocument').length, 2);
  assert.equal(commands.filter((entry) => entry.method === 'DOM.querySelectorAll').length, 6);
  assert.equal(commands.filter((entry) => entry.method === 'Page.getLayoutMetrics').length, 1,
    'the final identity check follows the completed AX, state, and metrics reads');
  assert.deepEqual(readdirSync(harness.profileParent), [], 'retirement still removes the owned profile');
});

test('owned observation rejects excessive and duplicate checkbox backend IDs before reading the DOM', async (t) => {
  const cases: Array<[string, FakePeerScenario]> = [
    ['more than the fixed query cap', { axNodes: checkboxNodes(Array.from({ length: 33 }, (_, index) => 100 + index)) }],
    ['duplicate backend node IDs', { axNodes: checkboxNodes([51, 51]) }],
  ];

  for (const [name, scenario] of cases) {
    await t.test(name, async (subtest) => {
      const harness = await launchFakePeer(subtest, scenario);
      await assert.rejects(harness.session.observe(), (error: unknown) => assertErrorCode(error, 'protocol_error'));
      await harness.session.close();

      const commands = commandEntries(harness);
      assert.equal(commands.filter((entry) => entry.method === 'Accessibility.getFullAXTree').length, 1);
      assert.equal(commands.filter((entry) => entry.method === 'DOM.getDocument').length, 0,
        'invalid checkbox backend ID sets are rejected before reading the DOM');
      assert.equal(commands.filter((entry) => entry.method === 'DOM.querySelectorAll').length, 0);
      assert.equal(commands.filter((entry) => entry.method === 'Page.getLayoutMetrics').length, 0);
      assert.deepEqual(readdirSync(harness.profileParent), [], 'rejection still removes the owned profile');
    });
  }
});
