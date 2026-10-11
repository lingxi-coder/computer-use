import { randomUUID } from 'node:crypto';

import {
  browserTargetContextHash,
  validateBrowserPageSnapshot,
  type BrowserObservedBool,
  type BrowserPageNode,
  type BrowserPageSnapshot,
  type BrowserTargetContext,
} from '@computer-use/desktop-api';

import {
  HeadlessBrowserError,
  type CdpAXNode,
  type HeadlessCheckboxState,
  type HeadlessObservation,
} from './cdpPipe.js';

const MAX_CHECKBOX_STATES = 32;
const MAX_CHECKBOX_DOCUMENT_URL_BYTES = 4_096;
const CHECKBOX_STATE_KEYS = new Set([
  'backendDOMNodeId',
  'disabled',
  'checked',
  'indeterminate',
  'documentUrl',
  'connected',
  'topDocument',
  'nativeCheckbox',
  'blockedByAriaOrInert',
]);

/** OwnedBrowser records these timestamps with this process's performance.now(). */
export const HEADLESS_OBSERVATION_CLOCK_DOMAIN = 'headless-performance-monotonic-ms' as const;

export interface HeadlessPageSourceOptions {
  /** Canonical file URL of the fixture passed to launchOwnedBrowser. */
  readonly fixtureFileUrl: string;
  /** Must match the clock domain declared by the source observation. */
  readonly sourceClockDomain: string;
  /** Trusted Main clock domain used for freshness comparisons by the host. */
  readonly targetClockDomain: string;
  /** Epoch of the owning session; a new session must supply a new epoch. */
  readonly sessionEpoch: number;
  /** Converts a source monotonic instant into the owning session's logical milliseconds. */
  readonly mapMonotonicMs: (sourceMonotonicMs: number) => number;
}

export type HeadlessPageClockMapping = Omit<HeadlessPageSourceOptions, 'fixtureFileUrl'>;

function invalidInput(): never {
  throw new HeadlessBrowserError('invalid_input');
}

function protocolError(): never {
  throw new HeadlessBrowserError('protocol_error');
}

function logicalInstant(
  clock: HeadlessPageClockMapping,
  sourceMs: number,
  boundary: 'read_start' | 'read_finish',
): number {
  let mapped: number;
  try {
    mapped = clock.mapMonotonicMs(sourceMs);
  } catch {
    return protocolError();
  }
  if (!Number.isFinite(mapped) || mapped < 0) return protocolError();
  const conservative = boundary === 'read_start' ? Math.floor(mapped) : Math.ceil(mapped);
  if (!Number.isSafeInteger(conservative)) return protocolError();
  return conservative;
}

function optionalAXString(value: CdpAXNode['role'] | CdpAXNode['name']): string | null {
  if (value === undefined) return null;
  const actual = value.value;
  if (actual === undefined || actual === null) return null;
  if (typeof actual !== 'string') return protocolError();
  return actual;
}

function observedBool(node: CdpAXNode, propertyName: 'disabled' | 'checked'): BrowserObservedBool {
  const matches = (node.properties ?? []).filter((property) => property.name === propertyName);
  if (matches.length === 0) return 'unknown';
  if (matches.length !== 1) return protocolError();

  const propertyValue = matches[0]!.value;
  const value = propertyValue.value;
  if (propertyName === 'disabled') {
    if (propertyValue.type === 'boolean' || propertyValue.type === 'booleanOrUndefined') {
      if (value === true) return 'true';
      if (value === false) return 'false';
      if (propertyValue.type === 'booleanOrUndefined' && (value === undefined || value === null)) return 'unknown';
    }
    return protocolError();
  }

  if ((propertyValue.type === 'boolean' || propertyValue.type === 'booleanOrUndefined')
    && typeof value === 'boolean') return value ? 'true' : 'false';
  if (propertyValue.type === 'booleanOrUndefined' && (value === undefined || value === null)) return 'unknown';
  if (propertyValue.type === 'tristate') {
    if (value === 'true') return 'true';
    if (value === 'false') return 'false';
    if (value === 'mixed' || value === undefined || value === null) return 'unknown';
  }
  return protocolError();
}

function fromBoolean(value: boolean): BrowserObservedBool {
  return value ? 'true' : 'false';
}

function mapNode(node: CdpAXNode, checkboxStates: ReadonlyMap<number, HeadlessCheckboxState>): BrowserPageNode {
  const backendId = node.backendDOMNodeId;
  if (backendId !== undefined
    && (!Number.isInteger(backendId) || backendId < -2_147_483_648 || backendId > 2_147_483_647)) {
    return protocolError();
  }
  const checkboxState = backendId === undefined ? undefined : checkboxStates.get(backendId);
  const axDisabled = observedBool(node, 'disabled');
  const axChecked = observedBool(node, 'checked');
  return {
    ax_node_id: node.nodeId,
    backend_dom_node_id: backendId ?? null,
    parent_ax_node_id: node.parentId ?? null,
    child_ax_node_ids: [...(node.childIds ?? [])],
    ignored: node.ignored,
    role: optionalAXString(node.role),
    name: optionalAXString(node.name),
    disabled: checkboxState
      ? fromBoolean(checkboxState.disabled || checkboxState.blockedByAriaOrInert)
      : axDisabled,
    checked: checkboxState
      ? (checkboxState.indeterminate ? 'unknown' : fromBoolean(checkboxState.checked))
      : axChecked,
  };
}

function mapNodes(
  nodes: readonly CdpAXNode[],
  checkboxStates: ReadonlyMap<number, HeadlessCheckboxState>,
): [BrowserPageNode, ...BrowserPageNode[]] {
  if (nodes.length === 0) return protocolError();
  return [mapNode(nodes[0]!, checkboxStates), ...nodes.slice(1).map((node) => mapNode(node, checkboxStates))];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function checkboxStateMap(
  nodes: readonly CdpAXNode[],
  value: HeadlessObservation['checkboxStates'],
  fixtureFileUrl: string,
): ReadonlyMap<number, HeadlessCheckboxState> {
  if (value === undefined) return new Map();
  if (!Array.isArray(value) || value.length > MAX_CHECKBOX_STATES) return protocolError();

  const states = new Map<number, HeadlessCheckboxState>();
  for (const candidate of value as readonly unknown[]) {
    if (!isRecord(candidate)) return protocolError();
    const keys = Reflect.ownKeys(candidate);
    if (keys.length !== CHECKBOX_STATE_KEYS.size
      || keys.some((key) => typeof key !== 'string' || !CHECKBOX_STATE_KEYS.has(key))) {
      return protocolError();
    }

    const state = candidate as unknown as HeadlessCheckboxState;
    const backendId = state.backendDOMNodeId;
    if (!Number.isSafeInteger(backendId) || backendId <= 0 || backendId > 2_147_483_647
      || states.has(backendId)
      || typeof state.documentUrl !== 'string'
      || Buffer.byteLength(state.documentUrl, 'utf8') > MAX_CHECKBOX_DOCUMENT_URL_BYTES
      || /[\u0000-\u001f\u007f-\u009f]/.test(state.documentUrl)
      || state.documentUrl !== fixtureFileUrl
      || typeof state.disabled !== 'boolean' || typeof state.checked !== 'boolean'
      || typeof state.indeterminate !== 'boolean'
      || typeof state.connected !== 'boolean' || typeof state.topDocument !== 'boolean'
      || typeof state.nativeCheckbox !== 'boolean' || typeof state.blockedByAriaOrInert !== 'boolean'
      || !state.connected || !state.topDocument || !state.nativeCheckbox) {
      return protocolError();
    }

    const matches = nodes.filter((node) => node.backendDOMNodeId === backendId);
    if (matches.length !== 1) return protocolError();
    const node = matches[0]!;
    if (node.ignored || optionalAXString(node.role) !== 'checkbox') return protocolError();

    const axDisabled = observedBool(node, 'disabled');
    const axChecked = observedBool(node, 'checked');
    if ((axDisabled === 'false' && state.disabled)
      || (axDisabled === 'true' && !state.disabled && !state.blockedByAriaOrInert)
      || (axChecked !== 'unknown' && (state.indeterminate || (axChecked === 'true') !== state.checked))) {
      return protocolError();
    }
    states.set(backendId, state);
  }
  return states;
}

/**
 * Converts one completed, real owned-browser AX read into the shared typed
 * source contract. The runtime contract validator audits the complete graph,
 * including ignored nodes, before this snapshot leaves Main.
 */
export async function createBrowserPageSnapshot(
  observation: HeadlessObservation,
  options: HeadlessPageSourceOptions,
): Promise<BrowserPageSnapshot> {
  if (!observation || typeof observation !== 'object' || !options || typeof options !== 'object') return invalidInput();
  const { provenance, document, rawAX } = observation;
  const clock = options;
  if (typeof options.fixtureFileUrl !== 'string' || options.fixtureFileUrl.length === 0
    || clock.sourceClockDomain !== HEADLESS_OBSERVATION_CLOCK_DOMAIN
    || typeof clock.targetClockDomain !== 'string' || clock.targetClockDomain.length === 0
    || typeof clock.mapMonotonicMs !== 'function'
    || !Number.isSafeInteger(clock.sessionEpoch) || clock.sessionEpoch <= 0) {
    return invalidInput();
  }
  if (!provenance || !document || !rawAX || !Array.isArray(rawAX.nodes)
    || provenance.clockDomain !== HEADLESS_OBSERVATION_CLOCK_DOMAIN || document.url !== options.fixtureFileUrl
    || provenance.pageId !== provenance.targetId
    || !Number.isSafeInteger(provenance.sequence) || provenance.sequence <= 0
    || !Number.isFinite(provenance.readStartedMonotonicMs) || provenance.readStartedMonotonicMs < 0
    || !Number.isFinite(provenance.readFinishedMonotonicMs)
    || provenance.readFinishedMonotonicMs < provenance.readStartedMonotonicMs) {
    return protocolError();
  }

  const observedMs = logicalInstant(clock, provenance.readStartedMonotonicMs, 'read_start');
  const readFinishedMs = logicalInstant(clock, provenance.readFinishedMonotonicMs, 'read_finish');
  if (readFinishedMs < observedMs) return protocolError();

  const context: BrowserTargetContext = {
    browser_session_id: provenance.browserSessionId,
    browser_context_id: provenance.browserContextId,
    page_target_id: provenance.targetId,
    cdp_session_id: provenance.sessionId,
    frame_id: document.frameId,
    document_loader_id: document.loaderId,
    fixture_file_url: options.fixtureFileUrl,
    session_epoch: clock.sessionEpoch,
  };

  let nodes: [BrowserPageNode, ...BrowserPageNode[]];
  let roots: readonly CdpAXNode[];
  try {
    const runtimeCheckboxStates = checkboxStateMap(rawAX.nodes, observation.checkboxStates, options.fixtureFileUrl);
    nodes = mapNodes(rawAX.nodes, runtimeCheckboxStates);
    roots = rawAX.nodes.filter((node) => node.parentId === undefined);
  } catch {
    return protocolError();
  }
  if (roots.length !== 1) return protocolError();
  let contextHash: string;
  try {
    contextHash = await browserTargetContextHash(context);
  } catch {
    return protocolError();
  }
  const snapshot: BrowserPageSnapshot = {
    snapshot_id: randomUUID(),
    source_sequence: provenance.sequence,
    observed_ms: observedMs,
    read_finished_ms: readFinishedMs,
    clock_domain: clock.targetClockDomain,
    context,
    target_context_hash: contextHash,
    root_ax_node_id: roots[0]!.nodeId,
    nodes,
  };

  let validation: Awaited<ReturnType<typeof validateBrowserPageSnapshot>>;
  try {
    validation = await validateBrowserPageSnapshot(snapshot);
  } catch {
    return protocolError();
  }
  if (!validation.ok) return protocolError();
  return validation.value;
}
