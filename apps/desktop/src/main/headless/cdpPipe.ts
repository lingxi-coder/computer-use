import { TextDecoder } from 'node:util';
import { performance } from 'node:perf_hooks';
import type { Readable, Writable } from 'node:stream';

const DEFAULT_COMMAND_TIMEOUT_MS = 5_000;
const MAX_COMMAND_TIMEOUT_MS = 20_000;
const MAX_PENDING_COMMANDS = 8;
const MAX_EVENT_WAITERS = 8;
const MAX_FRAME_BYTES = 8 * 1024 * 1024;
const MAX_BUFFERED_BYTES = MAX_FRAME_BYTES + 64 * 1024;
const MAX_QUEUED_EVENTS = 128;

type JsonRecord = Record<string, unknown>;

export type HeadlessBrowserErrorCode =
  | 'invalid_input'
  | 'spawn_failed'
  | 'protocol_error'
  | 'command_timeout'
  | 'document_changed'
  | 'child_exit_unconfirmed'
  | 'cleanup_failed'
  | 'closed';

export type HeadlessDiagnosticStage =
  | 'transport'
  | 'cdp_response'
  | 'response_envelope'
  | 'event_envelope'
  | 'event_listener'
  | 'invalid_ack'
  | 'target_discovery'
  | 'startup_target_roster'
  | 'startup_target_close'
  | 'startup_target_not_page'
  | 'startup_target_url_not_blank'
  | 'startup_target_context_present'
  | 'startup_target_info_url_changed'
  | 'startup_target_info_context_present'
  | 'event_target_created_shape'
  | 'event_target_info_changed_shape'
  | 'target_creation_not_page'
  | 'target_creation_event_count'
  | 'target_creation_url_not_blank'
  | 'target_creation_context_mismatch'
  | 'unexpected_target_created'
  | 'unexpected_target_info_changed'
  | 'unexpected_target_destroyed'
  | 'target_destroyed_shape'
  | 'page_frame_event_session_mismatch'
  | 'page_frame_event_shape'
  | 'page_frame_event_url'
  | 'network_event_session_mismatch'
  | 'network_event_shape'
  | 'network_event_url'
  | 'browser_download_event'
  | 'inspector_detached'
  | 'target_roster'
  | 'context_creation'
  | 'target_creation'
  | 'target_attachment'
  | 'session_setup'
  | 'fixture_navigation'
  | 'load_event'
  | 'document_identity'
  | 'checkbox_identity'
  | 'checkbox_document_root'
  | 'checkbox_document_shape'
  | 'checkbox_document_children'
  | 'checkbox_document_changed'
  | 'checkbox_query'
  | 'checkbox_action_snapshot'
  | 'checkbox_action_ax'
  | 'checkbox_action_geometry'
  | 'checkbox_action_hit_test'
  | 'checkbox_action_input'
  | 'observation'
  | 'screenshot'
  | 'cleanup'
  | 'initialization';

export type HeadlessDiagnosticReason =
  | 'top_level'
  | 'document_url_index'
  | 'node_tables'
  | 'node_row'
  | 'attributes'
  | 'root_graph'
  | 'shadow_data'
  | 'checked_data'
  | 'layout_tables'
  | 'layout_row_style';

export type HeadlessCdpCommandMethod =
  | 'Target.createBrowserContext'
  | 'Target.disposeBrowserContext'
  | 'Target.setDiscoverTargets'
  | 'Target.getTargets'
  | 'Target.createTarget'
  | 'Target.attachToTarget'
  | 'Target.closeTarget'
  | 'DOM.getDocument'
  | 'DOM.querySelectorAll'
  | 'DOMSnapshot.captureSnapshot'
  | 'Accessibility.getPartialAXTree'
  | 'DOM.getContentQuads'
  | 'DOM.getNodeForLocation'
  | 'Input.synthesizeTapGesture'
  | 'Page.enable'
  | 'Accessibility.enable'
  | 'Emulation.setScriptExecutionDisabled'
  | 'Network.enable'
  | 'Network.setBlockedURLs'
  | 'Browser.setDownloadBehavior'
  | 'Page.navigate'
  | 'Page.getFrameTree'
  | 'Accessibility.getFullAXTree'
  | 'Page.getLayoutMetrics'
  | 'Page.captureScreenshot';

export interface HeadlessBrowserDiagnostic {
  readonly stage: HeadlessDiagnosticStage;
  readonly reason?: HeadlessDiagnosticReason;
  readonly commandMethod?: HeadlessCdpCommandMethod;
  readonly nativeErrorCode?: number;
}

export type HeadlessInputWriteGuardResult = 'active' | 'deadline_expired' | 'unavailable';

/** Signals that the final write guard refused input before any CDP bytes were sent. */
export class HeadlessBrowserInputNotSubmitted extends Error {
  constructor(readonly reason: Exclude<HeadlessInputWriteGuardResult, 'active'>) {
    super('owned browser input was not submitted');
    this.name = 'HeadlessBrowserInputNotSubmitted';
  }
}

export interface HeadlessInputWriteGuard {
  /** Performance.now() deadline derived from Core's fixed command_end_ms. */
  readonly deadlineMonotonicMs: number;
  /** Last source/clock-state check, called immediately before deadline sampling. */
  readonly isActive: () => boolean;
  /** Records the exact monotonic instant sampled for the final write. */
  readonly onWriteStart: (monotonicMs: number) => void;
}

export class HeadlessBrowserError extends Error {
  constructor(
    readonly code: HeadlessBrowserErrorCode,
    readonly diagnostic?: HeadlessBrowserDiagnostic,
  ) {
    super(code);
    this.name = 'HeadlessBrowserError';
  }

  withDiagnostic(diagnostic: HeadlessBrowserDiagnostic): HeadlessBrowserError {
    const existing = this.diagnostic;
    const merged: { stage: HeadlessDiagnosticStage; reason?: HeadlessDiagnosticReason;
      commandMethod?: HeadlessCdpCommandMethod; nativeErrorCode?: number } = {
      stage: existing?.stage ?? diagnostic.stage,
    };
    const reason = existing?.reason ?? diagnostic.reason;
    if (reason !== undefined) merged.reason = reason;
    const commandMethod = existing?.commandMethod ?? diagnostic.commandMethod;
    if (commandMethod !== undefined) merged.commandMethod = commandMethod;
    const nativeErrorCode = existing?.nativeErrorCode ?? diagnostic.nativeErrorCode;
    if (nativeErrorCode !== undefined) merged.nativeErrorCode = nativeErrorCode;
    return new HeadlessBrowserError(this.code, merged);
  }
}

export interface HeadlessDocumentIdentity {
  readonly frameId: string;
  readonly loaderId: string;
  readonly url: string;
}

export interface HeadlessCheckboxState {
  readonly backendDOMNodeId: number;
  readonly disabled: boolean;
  readonly checked: boolean;
  readonly indeterminate: boolean;
  readonly documentUrl: string;
  readonly connected: boolean;
  readonly topDocument: boolean;
  readonly nativeCheckbox: boolean;
  readonly blockedByAriaOrInert: boolean;
}

export interface HeadlessBrowserProvenance {
  readonly browserSessionId: string;
  readonly browserContextId: string;
  readonly targetId: string;
  readonly pageId: string;
  readonly sessionId: string;
  readonly sequence: number;
  readonly clockDomain: 'headless-performance-monotonic-ms';
  readonly readStartedMonotonicMs: number;
  readonly readFinishedMonotonicMs: number;
}

export interface CdpAXRelatedNode {
  readonly backendDOMNodeId?: number;
  readonly id?: string;
  readonly text?: string;
  readonly [field: string]: unknown;
}

export interface CdpAXValueSource {
  readonly type: string;
  readonly value?: CdpAXValue;
  readonly attribute?: string;
  readonly superseded?: boolean;
  readonly nativeSource?: string;
  readonly nativeSourceValue?: CdpAXValue;
  readonly invalid?: boolean;
  readonly invalidReason?: string;
  readonly [field: string]: unknown;
}

export interface CdpAXValue {
  readonly type: string;
  readonly value?: string | number | boolean | null;
  readonly relatedNodes?: readonly CdpAXRelatedNode[];
  readonly sources?: readonly CdpAXValueSource[];
  readonly [field: string]: unknown;
}

export interface CdpAXProperty {
  readonly name: string;
  readonly value: CdpAXValue;
  readonly [field: string]: unknown;
}

export interface CdpAXNode {
  readonly nodeId: string;
  readonly ignored: boolean;
  readonly ignoredReasons?: readonly CdpAXProperty[];
  readonly role?: CdpAXValue;
  readonly chromeRole?: CdpAXValue;
  readonly name?: CdpAXValue;
  readonly description?: CdpAXValue;
  readonly value?: CdpAXValue;
  readonly properties?: readonly CdpAXProperty[];
  readonly parentId?: string;
  readonly childIds?: readonly string[];
  readonly backendDOMNodeId?: number;
  readonly frameId?: string;
  readonly [field: string]: unknown;
}

export interface CdpAXTree {
  readonly nodes: readonly CdpAXNode[];
  readonly [field: string]: unknown;
}

export interface HeadlessObservation {
  readonly provenance: HeadlessBrowserProvenance;
  readonly document: HeadlessDocumentIdentity;
  /** The raw, runtime-checked CDP Accessibility.getFullAXTree response. */
  readonly rawAX: CdpAXTree;
  /** Native checkbox properties resolved from AX backend DOM node IDs when available. */
  readonly checkboxStates?: readonly HeadlessCheckboxState[];
  /** The unmodified, runtime-checked Page.getLayoutMetrics result. */
  readonly layoutMetrics: Readonly<JsonRecord>;
}

export interface HeadlessPngCapture {
  readonly provenance: HeadlessBrowserProvenance;
  readonly document: HeadlessDocumentIdentity;
  readonly png: Buffer;
}

export interface CdpEvent {
  readonly method: string;
  readonly params: Readonly<JsonRecord>;
  readonly sessionId?: string;
}

export interface CdpPipeOptions {
  readonly commandTimeoutMs?: number;
  readonly maxFrameBytes?: number;
  readonly onRetire?: (error: HeadlessBrowserError) => void;
}

function isRecord(value: unknown): value is JsonRecord {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function exactKeys(value: JsonRecord, allowed: readonly string[]): boolean {
  return Object.keys(value).every((key) => allowed.includes(key));
}

function requiredString(value: unknown, maximumBytes = 256): value is string {
  return typeof value === 'string' && value.length > 0
    && Buffer.byteLength(value, 'utf8') <= maximumBytes;
}

function requireIdentifier(value: unknown): string {
  if (!requiredString(value)) throw new HeadlessBrowserError('protocol_error');
  return value;
}

function freezeJson<T>(value: T, depth = 0, budget = { remaining: 100_000 }): T {
  if (depth > 64 || --budget.remaining < 0) throw new HeadlessBrowserError('protocol_error');
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    for (const child of Object.values(value as Record<string, unknown>)) freezeJson(child, depth + 1, budget);
    Object.freeze(value);
  }
  return value;
}

const CHECKBOX_SELECTORS = {
  native: 'input[type="checkbox" i]',
  disabled: 'input[type="checkbox" i]:disabled',
  checked: 'input[type="checkbox" i]:checked',
  indeterminate: 'input[type="checkbox" i]:indeterminate',
  blocked: 'input[type="checkbox" i][aria-disabled="true" i], [aria-disabled="true" i] input[type="checkbox" i], input[type="checkbox" i][inert], [inert] input[type="checkbox" i]',
} as const;

interface CheckboxDOMDocument {
  readonly rootId: number;
  readonly documentUrl: string;
  readonly nodes: ReadonlyMap<number, { backendId: number; nodeType: number; nodeName: string; isSVG: boolean }>;
  readonly backendIds: ReadonlyMap<number, number>;
  readonly identity: string;
}

function positiveDOMId(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value > 0 && value <= 2_147_483_647;
}

/** Only the complete top-document light DOM is eligible; shadow/frame trees are excluded. */
function checkboxDOMDocument(value: JsonRecord, expectedUrl: string): CheckboxDOMDocument {
  if (!exactKeys(value, ['root']) || !isRecord(value.root) || value.root.nodeType !== 9
    || value.root.documentURL !== expectedUrl) throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_document_root', commandMethod: 'DOM.getDocument' });
  const nodes = new Map<number, { backendId: number; nodeType: number; nodeName: string; isSVG: boolean }>();
  const backendIds = new Map<number, number>();
  const identity: unknown[] = [];
  const queue: Array<{ node: unknown; parent: number | null; depth: number }> = [{ node: value.root, parent: null, depth: 1 }];
  while (queue.length) {
    const { node, parent, depth } = queue.pop()!;
    if (nodes.size >= 2_048 || depth > 24 || !isRecord(node)
      || !positiveDOMId(node.nodeId) || !positiveDOMId(node.backendNodeId)
      || nodes.has(node.nodeId) || backendIds.has(node.backendNodeId)
      || !Number.isInteger(node.nodeType) || Number(node.nodeType) < 1 || Number(node.nodeType) > 12
      || typeof node.nodeName !== 'string' || Buffer.byteLength(node.nodeName, 'utf8') > 256
      || (Object.hasOwn(node, 'parentId') && node.parentId !== parent)
      || (Object.hasOwn(node, 'isSVG') && typeof node.isSVG !== 'boolean')
      || Object.hasOwn(node, 'contentDocument')) throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_document_shape', commandMethod: 'DOM.getDocument' });
    const children = node.children ?? [];
    if (!Array.isArray(children) || children.length > 2_048
      || (Object.hasOwn(node, 'childNodeCount') && node.childNodeCount !== children.length)) {
      throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_document_children', commandMethod: 'DOM.getDocument' });
    }
    // An unresolved shadow subtree does not become connected top-document evidence.
    if (Object.hasOwn(node, 'shadowRoots') && (!Array.isArray(node.shadowRoots)
      || node.shadowRoots.some(root => !isRecord(root) || root.shadowRootType !== 'user-agent'))) {
      throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_document_shape', commandMethod: 'DOM.getDocument' });
    }
    nodes.set(node.nodeId, { backendId: node.backendNodeId, nodeType: Number(node.nodeType), nodeName: node.nodeName, isSVG: node.isSVG === true });
    backendIds.set(node.backendNodeId, node.nodeId);
    // getDocument may reassign frontend IDs. Backend identity and actual ancestry
    // survive that read-local renumbering; frontend IDs are used only for queries.
    identity.push([node.backendNodeId, parent === null ? null : nodes.get(parent)!.backendId,
      node.nodeType, node.nodeName, node.isSVG === true,
      children.map(child => isRecord(child) ? child.backendNodeId : null)]);
    for (let index = children.length - 1; index >= 0; index -= 1) queue.push({ node: children[index], parent: node.nodeId, depth: depth + 1 });
  }
  return { rootId: value.root.nodeId as number, documentUrl: expectedUrl, nodes, backendIds, identity: JSON.stringify(identity) };
}

/** Closed, DOM-native reads only. No arbitrary selector, JavaScript or input API. */
export class CdpPipe {
  private readonly commandTimeoutMs: number;
  private readonly maxFrameBytes: number;
  private readonly onRetire?: (error: HeadlessBrowserError) => void;
  private readonly pending = new Map<number, {
    commandMethod: HeadlessCdpCommandMethod;
    sessionId: string | null;
    resolve: (value: JsonRecord) => void;
    reject: (error: HeadlessBrowserError) => void;
    timer: ReturnType<typeof setTimeout>;
  }>();
  private readonly listeners = new Set<(event: CdpEvent) => void>();
  private readonly eventQueues = new Map<string, CdpEvent[]>();
  private queuedEventCount = 0;
  private eventWaiterCount = 0;
  private readonly eventWaiters = new Map<string, Array<{
    resolve: (event: CdpEvent) => void;
    reject: (error: HeadlessBrowserError) => void;
    timer: ReturnType<typeof setTimeout>;
  }>>();
  private buffer = Buffer.alloc(0);
  private nextId = 0;
  private retired: HeadlessBrowserError | null = null;
  private closed = false;
  private intentionalClose = false;
  private readonly fixtureUrl: string;

  constructor(
    private readonly readable: Readable,
    private readonly writable: Writable,
    fixtureUrl: string,
    options: CdpPipeOptions = {},
  ) {
    let parsedURL: URL;
    try { parsedURL = new URL(fixtureUrl); }
    catch { throw new HeadlessBrowserError('invalid_input'); }
    if (parsedURL.protocol !== 'file:' || parsedURL.hostname !== '' || parsedURL.search || parsedURL.hash
      || parsedURL.href !== fixtureUrl) throw new HeadlessBrowserError('invalid_input');
    this.fixtureUrl = parsedURL.href;
    this.commandTimeoutMs = options.commandTimeoutMs ?? DEFAULT_COMMAND_TIMEOUT_MS;
    this.maxFrameBytes = options.maxFrameBytes ?? MAX_FRAME_BYTES;
    this.onRetire = options.onRetire;
    if (!Number.isSafeInteger(this.commandTimeoutMs) || this.commandTimeoutMs <= 0
      || this.commandTimeoutMs > MAX_COMMAND_TIMEOUT_MS
      || !Number.isSafeInteger(this.maxFrameBytes) || this.maxFrameBytes < 1_024
      || this.maxFrameBytes > MAX_FRAME_BYTES) throw new HeadlessBrowserError('invalid_input');

    readable.on('data', this.onData);
    readable.once('error', this.onStreamError);
    readable.once('end', this.onStreamEnd);
    readable.once('close', this.onStreamClose);
    writable.once('error', this.onStreamError);
    writable.once('close', this.onWritableClose);
  }

  get isRetired(): boolean { return this.retired !== null || this.closed; }

  onEvent(listener: (event: CdpEvent) => void): () => void {
    if (this.isRetired) throw this.failure();
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  waitForEvent(method: 'Page.loadEventFired', sessionId: string,
               timeoutMs = this.commandTimeoutMs): Promise<CdpEvent> {
    if (this.isRetired) return Promise.reject(this.failure());
    if (!requiredString(method) || !requiredString(sessionId)
      || !Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > MAX_COMMAND_TIMEOUT_MS) {
      return Promise.reject(new HeadlessBrowserError('invalid_input'));
    }
    const key = this.eventKey(method, sessionId);
    const queue = this.eventQueues.get(key);
    if (queue?.length) {
      const event = queue.shift()!;
      this.queuedEventCount -= 1;
      if (queue.length === 0) this.eventQueues.delete(key);
      return Promise.resolve(event);
    }
    if (this.eventWaiterCount >= MAX_EVENT_WAITERS) {
      const error = new HeadlessBrowserError('protocol_error');
      this.retireWith(error);
      return Promise.reject(error);
    }
    return new Promise<CdpEvent>((resolve, reject) => {
      const timer = setTimeout(() => this.retireWith(new HeadlessBrowserError('command_timeout', {
        stage: 'load_event',
      })), timeoutMs);
      const waiters = this.eventWaiters.get(key) ?? [];
      waiters.push({ resolve, reject, timer });
      this.eventWaiterCount += 1;
      this.eventWaiters.set(key, waiters);
    });
  }

  createBrowserContext(): Promise<string> {
    return this.checkedResult(
      this.command('Target.createBrowserContext', { disposeOnDetach: true }),
      'Target.createBrowserContext',
      (value) => requireIdentifier(value.browserContextId),
    );
  }

  disposeBrowserContext(browserContextId: string): Promise<void> {
    return this.emptyAcknowledgement(this.command('Target.disposeBrowserContext', { browserContextId }),
                                    'Target.disposeBrowserContext');
  }

  setTargetDiscovery(discover: boolean, timeoutMs = this.commandTimeoutMs): Promise<void> {
    const params = discover
      ? { discover, filter: [{ type: 'page', exclude: false }, { exclude: true }] }
      : { discover };
    return this.emptyAcknowledgement(this.command('Target.setDiscoverTargets', params, null, undefined, timeoutMs),
                                    'Target.setDiscoverTargets');
  }

  getTargets(): Promise<readonly unknown[]> {
    return this.checkedResult(this.command('Target.getTargets', {
      filter: [{ type: 'page', exclude: false }, { exclude: true }],
    }), 'Target.getTargets', (value) => {
      if (!Array.isArray(value.targetInfos) || value.targetInfos.length > 32
        || value.targetInfos.some((target) => !isRecord(target))) {
        throw new HeadlessBrowserError('protocol_error');
      }
      return value.targetInfos;
    });
  }

  createAboutBlankTarget(browserContextId: string): Promise<string> {
    return this.checkedResult(this.command('Target.createTarget', {
      url: 'about:blank', browserContextId, background: true,
    }), 'Target.createTarget', (value) => requireIdentifier(value.targetId));
  }

  attachFlattenedTarget(targetId: string): Promise<string> {
    return this.checkedResult(
      this.command('Target.attachToTarget', { targetId, flatten: true }),
      'Target.attachToTarget',
      (value) => requireIdentifier(value.sessionId),
    );
  }

  closeTarget(targetId: string): Promise<void> {
    return this.checkedResult(this.command('Target.closeTarget', { targetId }), 'Target.closeTarget', (value) => {
      if (value.success !== true) throw new HeadlessBrowserError('protocol_error');
    });
  }

  enablePage(sessionId: string): Promise<void> {
    return this.emptyAcknowledgement(this.command('Page.enable', {}, sessionId), 'Page.enable');
  }

  enableAccessibility(sessionId: string): Promise<void> {
    return this.emptyAcknowledgement(this.command('Accessibility.enable', {}, sessionId), 'Accessibility.enable');
  }

  disableScriptExecution(sessionId: string): Promise<void> {
    return this.emptyAcknowledgement(
      this.command('Emulation.setScriptExecutionDisabled', { value: true }, sessionId),
      'Emulation.setScriptExecutionDisabled',
    );
  }

  enableNetworkBlocklist(sessionId: string): Promise<void> {
    return this.emptyAcknowledgement(this.command('Network.enable', { maxTotalBufferSize: 1_048_576,
      maxResourceBufferSize: 262_144 }, sessionId), 'Network.enable').then(async () => {
      await this.emptyAcknowledgement(this.command('Network.setBlockedURLs', { urls: [
        'http://*', 'https://*', 'ws://*', 'wss://*', 'ftp://*',
      ] }, sessionId), 'Network.setBlockedURLs');
    });
  }

  denyDownloads(browserContextId: string): Promise<void> {
    return this.emptyAcknowledgement(this.command('Browser.setDownloadBehavior', {
      behavior: 'deny', browserContextId,
    }), 'Browser.setDownloadBehavior');
  }

  navigateFixture(sessionId: string): Promise<JsonRecord> {
    return this.checkedResult(this.command('Page.navigate', { url: this.fixtureUrl }, sessionId),
      'Page.navigate', (value) => {
      if (!requiredString(value.frameId, 256) || !requiredString(value.loaderId, 256)
        || (Object.hasOwn(value, 'errorText') && value.errorText !== undefined
          && typeof value.errorText !== 'string')) throw new HeadlessBrowserError('protocol_error');
      return value;
    });
  }

  frameTree(sessionId: string, diagnosticStage: HeadlessDiagnosticStage = 'invalid_ack'): Promise<JsonRecord> {
    return this.checkedResult(this.command('Page.getFrameTree', {}, sessionId), 'Page.getFrameTree', (value) => {
      const tree = value.frameTree;
      if (!isRecord(tree) || !isRecord(tree.frame)
        || !requiredString(tree.frame.id, 256) || !requiredString(tree.frame.loaderId, 256)
        || !requiredString(tree.frame.url, 4_096)
        || (Object.hasOwn(tree, 'childFrames') && !Array.isArray(tree.childFrames))) {
        throw new HeadlessBrowserError('protocol_error');
      }
      return value;
    }, diagnosticStage);
  }

  fullAXTree(sessionId: string): Promise<JsonRecord> {
    return this.checkedResult(this.command('Accessibility.getFullAXTree', {}, sessionId),
      'Accessibility.getFullAXTree', (value) => {
      if (!Array.isArray(value.nodes) || value.nodes.length > 2_048) {
        throw new HeadlessBrowserError('protocol_error');
      }
      return value;
    });
  }

  layoutMetrics(sessionId: string, diagnosticStage: HeadlessDiagnosticStage = 'invalid_ack'): Promise<JsonRecord> {
    return this.checkedResult(this.command('Page.getLayoutMetrics', {}, sessionId), 'Page.getLayoutMetrics', (value) => {
      const hasObject = (keys: readonly string[]): boolean => keys.some((key) => isRecord(value[key]));
      if (!hasObject(['cssLayoutViewport', 'layoutViewport'])
        || !hasObject(['cssVisualViewport', 'visualViewport'])
        || !hasObject(['cssContentSize', 'contentSize'])) throw new HeadlessBrowserError('protocol_error');
      return value;
    }, diagnosticStage);
  }

  captureScreenshot(sessionId: string): Promise<JsonRecord> {
    return this.checkedResult(this.command('Page.captureScreenshot', {
      format: 'png', fromSurface: true, captureBeyondViewport: false,
    }, sessionId), 'Page.captureScreenshot', (value) => {
      if (!requiredString(value.data, 8 * 1024 * 1024)) throw new HeadlessBrowserError('protocol_error');
      return value;
    });
  }

  /** Fixed no-script DOM state read for one already-bound backend node. */
  captureActionSnapshot(sessionId: string): Promise<JsonRecord> {
    return this.checkedResult(this.command('DOMSnapshot.captureSnapshot', {
      computedStyles: ['display', 'visibility', 'opacity', 'pointer-events'],
      includeDOMRects: true,
      includePaintOrder: true,
    }, sessionId), 'DOMSnapshot.captureSnapshot', (value) => {
      if (!exactKeys(value, ['documents', 'strings']) || !Array.isArray(value.documents)
        || value.documents.length === 0 || value.documents.length > 4 || !Array.isArray(value.strings)
        || value.strings.length > 65_536) throw new HeadlessBrowserError('protocol_error');
      const bytes = Buffer.byteLength(JSON.stringify(value), 'utf8');
      if (!Number.isSafeInteger(bytes) || bytes <= 0 || bytes > 2 * 1024 * 1024) {
        throw new HeadlessBrowserError('protocol_error');
      }
      return value;
    }, 'checkbox_action_snapshot');
  }

  partialAXTreeForBackendNode(sessionId: string, backendDOMNodeId: number): Promise<JsonRecord> {
    if (!requiredString(sessionId) || !positiveDOMId(backendDOMNodeId)) {
      return Promise.reject(new HeadlessBrowserError('invalid_input'));
    }
    return this.checkedResult(this.command('Accessibility.getPartialAXTree', {
      backendNodeId: backendDOMNodeId,
      fetchRelatives: false,
    }, sessionId), 'Accessibility.getPartialAXTree', (value) => {
      if (!exactKeys(value, ['nodes']) || !Array.isArray(value.nodes) || value.nodes.length === 0
        || value.nodes.length > 16 || value.nodes.some((node) => !isRecord(node))) {
        throw new HeadlessBrowserError('protocol_error');
      }
      return value;
    }, 'checkbox_action_ax');
  }

  contentQuadsForBackendNode(sessionId: string, backendDOMNodeId: number): Promise<readonly number[][]> {
    if (!requiredString(sessionId) || !positiveDOMId(backendDOMNodeId)) {
      return Promise.reject(new HeadlessBrowserError('invalid_input'));
    }
    return this.checkedResult(this.command('DOM.getContentQuads', {
      backendNodeId: backendDOMNodeId,
    }, sessionId), 'DOM.getContentQuads', (value) => {
      if (!exactKeys(value, ['quads']) || !Array.isArray(value.quads) || value.quads.length === 0
        || value.quads.length > 8 || value.quads.some((quad) => !Array.isArray(quad) || quad.length !== 8
          || !quad.every((point) => typeof point === 'number' && Number.isFinite(point)))) {
        throw new HeadlessBrowserError('protocol_error');
      }
      return value.quads as number[][];
    }, 'checkbox_action_geometry');
  }

  nodeForLocation(sessionId: string, x: number, y: number): Promise<JsonRecord> {
    if (!requiredString(sessionId) || !Number.isSafeInteger(x) || x < 0 || x > 1_000_000
      || !Number.isSafeInteger(y) || y < 0 || y > 1_000_000) {
      return Promise.reject(new HeadlessBrowserError('invalid_input'));
    }
    return this.checkedResult(this.command('DOM.getNodeForLocation', {
      x,
      y,
      includeUserAgentShadowDOM: false,
      ignorePointerEventsNone: false,
    }, sessionId), 'DOM.getNodeForLocation', (value) => {
      const expectedKeys = Object.hasOwn(value, 'nodeId')
        ? ['backendNodeId', 'frameId', 'nodeId']
        : ['backendNodeId', 'frameId'];
      if (!exactKeys(value, expectedKeys)
        || !positiveDOMId(value.backendNodeId) || !requiredString(value.frameId, 256)
        || (Object.hasOwn(value, 'nodeId') && !positiveDOMId(value.nodeId))) {
        throw new HeadlessBrowserError('protocol_error');
      }
      return value;
    }, 'checkbox_action_hit_test');
  }

  /** One closed native mouse tap; no caller-controlled command or gesture flags. */
  synthesizeCheckboxMouseTap(
    sessionId: string,
    x: number,
    y: number,
    inputGuard: HeadlessInputWriteGuard,
  ): Promise<void> {
    if (!requiredString(sessionId) || !Number.isFinite(x) || x < 0 || x > 1_000_000
      || !Number.isFinite(y) || y < 0 || y > 1_000_000
      || !Number.isFinite(inputGuard.deadlineMonotonicMs) || inputGuard.deadlineMonotonicMs < 0
      || typeof inputGuard.isActive !== 'function' || typeof inputGuard.onWriteStart !== 'function') {
      return Promise.reject(new HeadlessBrowserError('invalid_input'));
    }
    return this.emptyAcknowledgement(this.command('Input.synthesizeTapGesture', {
      x,
      y,
      duration: 0,
      tapCount: 1,
      gestureSourceType: 'mouse',
    }, sessionId, inputGuard), 'Input.synthesizeTapGesture', 'checkbox_action_input');
  }

  async readCheckboxStates(sessionId: string, backendDOMNodeIds: readonly number[]): Promise<readonly HeadlessCheckboxState[]> {
    if (!requiredString(sessionId) || !Array.isArray(backendDOMNodeIds) || backendDOMNodeIds.length > 32
      || !backendDOMNodeIds.every(positiveDOMId) || new Set(backendDOMNodeIds).size !== backendDOMNodeIds.length) {
      throw new HeadlessBrowserError('invalid_input');
    }
    const requestedIds = [...backendDOMNodeIds];
    if (!requestedIds.length) return [];
    const readDocument = () => this.checkedResult(this.command('DOM.getDocument', { depth: -1, pierce: false }, sessionId),
      'DOM.getDocument', value => checkboxDOMDocument(value, this.fixtureUrl), 'checkbox_identity');
    const before = await readDocument();
    const query = (selector: typeof CHECKBOX_SELECTORS[keyof typeof CHECKBOX_SELECTORS]) => this.checkedResult(
      this.command('DOM.querySelectorAll', { nodeId: before.rootId, selector }, sessionId), 'DOM.querySelectorAll', value => {
        if (!exactKeys(value, ['nodeIds']) || !Array.isArray(value.nodeIds) || value.nodeIds.length > 32
          || !value.nodeIds.every(positiveDOMId) || new Set(value.nodeIds).size !== value.nodeIds.length
          || value.nodeIds.some(id => !before.nodes.has(id))) throw new HeadlessBrowserError('protocol_error');
        return value.nodeIds as number[];
      }, 'checkbox_query');
    const native = await query(CHECKBOX_SELECTORS.native);
    const nativeSet = new Set(native);
    const disabled = await query(CHECKBOX_SELECTORS.disabled);
    const checked = await query(CHECKBOX_SELECTORS.checked);
    const indeterminate = await query(CHECKBOX_SELECTORS.indeterminate);
    const blocked = await query(CHECKBOX_SELECTORS.blocked);
    const nativeAgain = await query(CHECKBOX_SELECTORS.native);
    const after = await readDocument();
    return this.checkedResult(Promise.resolve({}), 'DOM.getDocument', () => {
      if (before.identity !== after.identity) throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_document_changed', commandMethod: 'DOM.getDocument' });
      if (native.length !== nativeAgain.length || native.some((id, index) => nativeAgain[index] !== id)
        || [disabled, checked, indeterminate, blocked].some(ids => ids.some(id => !nativeSet.has(id)))
        || native.some(id => { const node = before.nodes.get(id)!; return node.nodeType !== 1 || node.nodeName !== 'INPUT' || node.isSVG; })) {
        throw new HeadlessBrowserError('protocol_error');
      }
      return requestedIds.map(backendDOMNodeId => {
        const id = before.backendIds.get(backendDOMNodeId);
        if (id === undefined || before.nodes.get(id)?.nodeType !== 1) throw new HeadlessBrowserError('protocol_error');
        const nativeCheckbox = nativeSet.has(id);
        return Object.freeze({ backendDOMNodeId, disabled: disabled.includes(id), checked: checked.includes(id),
          indeterminate: indeterminate.includes(id), documentUrl: before.documentUrl,
          connected: true, topDocument: true, nativeCheckbox, blockedByAriaOrInert: blocked.includes(id) });
      });
    }, 'checkbox_identity');
  }

  /** Close the parent ends of the two debugging-pipe descriptors. */
  close(): void {
    if (this.closed) return;
    this.intentionalClose = true;
    this.closed = true;
    this.failPending(new HeadlessBrowserError('closed'));
    this.failEventWaiters(new HeadlessBrowserError('closed'));
    this.eventQueues.clear();
    this.queuedEventCount = 0;
    this.buffer.fill(0);
    this.buffer = Buffer.alloc(0);
    this.readable.off('data', this.onData);
    this.readable.off('error', this.onStreamError);
    this.readable.off('end', this.onStreamEnd);
    this.readable.off('close', this.onStreamClose);
    this.writable.off('error', this.onStreamError);
    this.writable.off('close', this.onWritableClose);
    this.readable.destroy();
    this.writable.end();
  }

  /** Retire a session after an identity or policy violation. It cannot be reused. */
  invalidate(error: HeadlessBrowserError): void { this.retireWith(error); }

  private command(method: HeadlessCdpCommandMethod, params: JsonRecord,
                  sessionId: string | null = null,
                  inputGuard?: HeadlessInputWriteGuard,
                  timeoutMs = this.commandTimeoutMs): Promise<JsonRecord> {
    if (this.isRetired) return Promise.reject(this.failure());
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > MAX_COMMAND_TIMEOUT_MS) {
      return Promise.reject(new HeadlessBrowserError('invalid_input'));
    }
    if (this.pending.size >= MAX_PENDING_COMMANDS) {
      const error = new HeadlessBrowserError('protocol_error');
      this.retireWith(error);
      return Promise.reject(error);
    }
    const id = ++this.nextId;
    if (!Number.isSafeInteger(id)) {
      const error = new HeadlessBrowserError('protocol_error');
      this.retireWith(error);
      return Promise.reject(error);
    }
    const message = JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) });
    const body = Buffer.from(message, 'utf8');
    if (body.length === 0 || body.length > this.maxFrameBytes) {
      body.fill(0);
      const error = new HeadlessBrowserError('protocol_error');
      this.retireWith(error);
      return Promise.reject(error);
    }
    const frame = Buffer.allocUnsafe(body.length + 1);
    body.copy(frame, 0); frame.fill(0, body.length); body.fill(0);
    return new Promise<JsonRecord>((resolve, reject) => {
      const timer = setTimeout(() => this.retireWith(new HeadlessBrowserError('command_timeout', {
        stage: 'cdp_response', commandMethod: method,
      })), timeoutMs);
      this.pending.set(id, { commandMethod: method, sessionId, resolve, reject, timer });
      if (inputGuard) {
        let active = false;
        try { active = inputGuard.isActive(); }
        catch { active = false; }
        if (!active) {
          clearTimeout(timer);
          this.pending.delete(id);
          frame.fill(0);
          reject(new HeadlessBrowserInputNotSubmitted('unavailable'));
          return;
        }
        const writeStartedAt = performance.now();
        if (!Number.isFinite(writeStartedAt) || writeStartedAt > inputGuard.deadlineMonotonicMs) {
          clearTimeout(timer);
          this.pending.delete(id);
          frame.fill(0);
          reject(new HeadlessBrowserInputNotSubmitted('deadline_expired'));
          return;
        }
        try { inputGuard.onWriteStart(writeStartedAt); }
        catch {
          clearTimeout(timer);
          this.pending.delete(id);
          frame.fill(0);
          reject(new HeadlessBrowserInputNotSubmitted('unavailable'));
          return;
        }
      }
      try {
        this.writable.write(frame, (error?: Error | null) => {
          frame.fill(0);
          if (error) this.retire('protocol_error');
        });
      } catch {
        frame.fill(0);
        this.retire('protocol_error');
      }
    });
  }

  private checkedResult<T>(operation: Promise<JsonRecord>, commandMethod: HeadlessCdpCommandMethod,
                          decode: (value: JsonRecord) => T,
                          diagnosticStage: HeadlessDiagnosticStage = 'invalid_ack'): Promise<T> {
    return operation.then((value) => {
      try { return decode(value); }
      catch (caught) {
        const error = caught instanceof HeadlessBrowserError && caught.code === 'protocol_error'
          ? caught.withDiagnostic({ stage: diagnosticStage, commandMethod })
          : new HeadlessBrowserError('protocol_error', { stage: diagnosticStage, commandMethod });
        this.retireWith(error);
        throw error;
      }
    });
  }

  private emptyAcknowledgement(operation: Promise<JsonRecord>,
                               commandMethod: HeadlessCdpCommandMethod,
                               diagnosticStage: HeadlessDiagnosticStage = 'invalid_ack'): Promise<void> {
    return this.checkedResult(operation, commandMethod, (value) => {
      if (Object.keys(value).length !== 0) throw new HeadlessBrowserError('protocol_error');
    }, diagnosticStage);
  }

  private readonly onData = (chunk: Buffer | string): void => {
    if (this.isRetired) return;
    if (!Buffer.isBuffer(chunk)) { this.retire('protocol_error'); return; }
    if (chunk.length === 0 || this.buffer.length + chunk.length > MAX_BUFFERED_BYTES) {
      this.retire('protocol_error'); return;
    }
    this.buffer = this.buffer.length === 0 ? Buffer.from(chunk) : Buffer.concat([this.buffer, chunk]);
    while (!this.isRetired) {
      const delimiter = this.buffer.indexOf(0);
      if (delimiter < 0) {
        if (this.buffer.length > this.maxFrameBytes) this.retire('protocol_error');
        return;
      }
      if (delimiter === 0 || delimiter > this.maxFrameBytes) { this.retire('protocol_error'); return; }
      const buffered = this.buffer;
      const frame = buffered.subarray(0, delimiter);
      this.buffer = Buffer.from(buffered.subarray(delimiter + 1));
      let value: unknown;
      try {
        const text = new TextDecoder('utf-8', { fatal: true }).decode(frame);
        value = JSON.parse(text) as unknown;
      } catch {
        buffered.fill(0, 0, delimiter + 1);
        this.retire('protocol_error'); return;
      }
      buffered.fill(0, 0, delimiter + 1);
      if (!isRecord(value)) { this.retire('protocol_error'); return; }
      try {
        freezeJson(value);
        if (Object.hasOwn(value, 'id')) this.acceptResponse(value);
        else this.acceptEvent(value);
      } catch {
        this.retire('protocol_error'); return;
      }
    }
  };

  private acceptResponse(value: JsonRecord): void {
    const id = value.id;
    if (!Number.isSafeInteger(id) || (id as number) <= 0) {
      this.retireWith(new HeadlessBrowserError('protocol_error', { stage: 'response_envelope' })); return;
    }
    const pending = this.pending.get(id as number);
    if (!pending) {
      this.retireWith(new HeadlessBrowserError('protocol_error', { stage: 'response_envelope' })); return;
    }
    const hasResult = Object.hasOwn(value, 'result');
    const hasError = Object.hasOwn(value, 'error');
    const expectedKeys = pending.sessionId === null ? ['id', 'result'] : ['id', 'result', 'sessionId'];
    if (hasError) {
      const rawCode = isRecord(value.error) ? value.error.code : undefined;
      const nativeErrorCode = typeof rawCode === 'number' && Number.isSafeInteger(rawCode)
        && rawCode >= -1_000_000 && rawCode <= 1_000_000 ? rawCode : undefined;
      this.retireWith(new HeadlessBrowserError('protocol_error', {
        stage: 'cdp_response', commandMethod: pending.commandMethod,
        ...(nativeErrorCode !== undefined ? { nativeErrorCode } : {}),
      }));
      return;
    }
    if (!hasResult || !exactKeys(value, expectedKeys)
      || (pending.sessionId !== null && value.sessionId !== pending.sessionId)
      || !isRecord(value.result)) {
      this.retireWith(new HeadlessBrowserError('protocol_error', {
        stage: 'response_envelope', commandMethod: pending.commandMethod,
      })); return;
    }
    clearTimeout(pending.timer);
    this.pending.delete(id as number);
    pending.resolve(value.result);
  }

  private acceptEvent(value: JsonRecord): void {
    if (!exactKeys(value, ['method', 'params', 'sessionId'])
      || !requiredString(value.method, 256) || !isRecord(value.params)
      || (Object.hasOwn(value, 'sessionId') && !requiredString(value.sessionId))) {
      this.retireWith(new HeadlessBrowserError('protocol_error', { stage: 'event_envelope' })); return;
    }
    const event: CdpEvent = Object.freeze({
      method: value.method,
      params: value.params,
      ...(typeof value.sessionId === 'string' ? { sessionId: value.sessionId } : {}),
    });
    const key = this.eventKey(event.method, event.sessionId ?? null);
    const waiters = this.eventWaiters.get(key);
    const waiter = waiters?.shift();
    if (waiters && waiters.length === 0) this.eventWaiters.delete(key);
    if (waiter) {
      clearTimeout(waiter.timer);
      this.eventWaiterCount -= 1;
      waiter.resolve(event);
    } else {
      const queue = this.eventQueues.get(key) ?? [];
      if (this.queuedEventCount >= MAX_QUEUED_EVENTS) {
        this.retire('protocol_error'); return;
      }
      queue.push(event);
      this.queuedEventCount += 1;
      this.eventQueues.set(key, queue);
    }
    for (const listener of this.listeners) {
      try { listener(event); }
      catch {
        this.retireWith(new HeadlessBrowserError('protocol_error', { stage: 'event_listener' })); return;
      }
    }
  }

  private eventKey(method: string, sessionId: string | null): string {
    return `${sessionId ?? ''}\u0000${method}`;
  }

  private readonly onStreamError = (): void => { if (!this.intentionalClose) this.retire('protocol_error'); };
  private readonly onStreamEnd = (): void => { if (!this.intentionalClose) this.retire('protocol_error'); };
  private readonly onStreamClose = (): void => { if (!this.intentionalClose) this.retire('protocol_error'); };
  private readonly onWritableClose = (): void => { if (!this.intentionalClose) this.retire('protocol_error'); };

  private retire(code: 'protocol_error' | 'command_timeout'): void {
    this.retireWith(new HeadlessBrowserError(code));
  }

  private retireWith(error: HeadlessBrowserError): void {
    if (this.retired || this.closed) return;
    this.retired = error;
    this.buffer.fill(0);
    this.buffer = Buffer.alloc(0);
    this.failPending(error);
    this.failEventWaiters(error);
    this.eventQueues.clear();
    this.queuedEventCount = 0;
    this.readable.destroy();
    this.writable.destroy();
    this.onRetire?.(error);
  }

  private failPending(error: HeadlessBrowserError): void {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  private failEventWaiters(error: HeadlessBrowserError): void {
    for (const waiters of this.eventWaiters.values()) {
      for (const waiter of waiters) { clearTimeout(waiter.timer); waiter.reject(error); }
    }
    this.eventWaiters.clear();
    this.eventWaiterCount = 0;
  }

  private failure(): HeadlessBrowserError {
    return this.retired ?? new HeadlessBrowserError('closed');
  }
}
