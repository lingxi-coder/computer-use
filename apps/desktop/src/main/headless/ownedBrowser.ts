import { spawn, type ChildProcess } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { constants } from 'node:fs';
import { accessSync, chmodSync, lstatSync, mkdtempSync, readdirSync, realpathSync, rmSync } from 'node:fs';
import { basename, dirname, extname, isAbsolute, join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { Readable, Writable } from 'node:stream';
import { pathToFileURL } from 'node:url';

import {
  CdpPipe,
  HeadlessBrowserInputNotSubmitted,
  HeadlessBrowserError,
  type HeadlessBrowserDiagnostic,
  type HeadlessBrowserErrorCode,
  type HeadlessCdpCommandMethod,
  type HeadlessDiagnosticReason,
  type HeadlessDiagnosticStage,
  type CdpAXTree,
  type HeadlessCheckboxState,
  type HeadlessBrowserProvenance,
  type HeadlessDocumentIdentity,
  type HeadlessObservation,
  type HeadlessPngCapture,
} from './cdpPipe.js';

const MAX_AX_NODES = 2_048;
const MAX_AX_PROPERTIES = 256;
const MAX_AX_CHILD_IDS = 2_048;
const MAX_CHECKBOX_STATE_READS = 32;
const MAX_ACTION_DOM_NODES = 8_192;
const ACTION_STYLE_NAMES = ['display', 'visibility', 'opacity', 'pointer-events'] as const;
const MAX_CAPTURE_BYTES = 5 * 1024 * 1024;
const CHILD_EXIT_WAIT_MS = 2_000;
const TERM_WAIT_MS = 1_000;
const KILL_WAIT_MS = 1_000;
// Cold-start allowance for the first CDP control-plane command only; other
// commands retain CdpPipe's normal timeout and this grants no input authority.
const STARTUP_TARGET_DISCOVERY_TIMEOUT_MS = 15_000;
const MAX_PROFILE_ENTRIES = 20_000;
const MAX_PROFILE_DEPTH = 32;
const CHROME_PROFILE_PREFIX = 'cu-headless-owned-';
const PNG_SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

type JsonRecord = Record<string, unknown>;
type CanonicalPaths = { chrome: string; fixture: string; profileParent: string; fixtureURL: string };

function isRecord(value: unknown): value is JsonRecord {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function requiredString(value: unknown, maxBytes = 4_096): value is string {
  return typeof value === 'string' && value.length > 0 && Buffer.byteLength(value, 'utf8') <= maxBytes;
}

function requireRecord(value: unknown): JsonRecord {
  if (!isRecord(value)) throw new HeadlessBrowserError('protocol_error');
  return value;
}

function validateRegularPath(value: unknown, kind: 'chrome' | 'fixture' | 'profileParent'): string {
  if (!requiredString(value, 4_096) || !isAbsolute(value)) throw new HeadlessBrowserError('invalid_input');
  let stat;
  let canonical: string;
  try {
    stat = lstatSync(value);
    canonical = realpathSync(value);
  } catch {
    throw new HeadlessBrowserError('invalid_input');
  }
  if (stat.isSymbolicLink() || canonical !== value) throw new HeadlessBrowserError('invalid_input');
  if (kind === 'profileParent') {
    if (!stat.isDirectory()) throw new HeadlessBrowserError('invalid_input');
  } else {
    if (!stat.isFile()) throw new HeadlessBrowserError('invalid_input');
    if (kind === 'chrome') {
      try { accessSync(value, constants.X_OK); }
      catch { throw new HeadlessBrowserError('invalid_input'); }
    } else if (!['.html', '.htm'].includes(extname(value).toLowerCase())) {
      throw new HeadlessBrowserError('invalid_input');
    }
  }
  return canonical;
}

function canonicalPaths(options: OwnedBrowserLaunchOptions): CanonicalPaths {
  if (!isRecord(options)
    || Object.keys(options).sort().join(',') !== 'chromeExecutable,fixturePath,profileParent') {
    throw new HeadlessBrowserError('invalid_input');
  }
  const chrome = validateRegularPath(options.chromeExecutable, 'chrome');
  const fixture = validateRegularPath(options.fixturePath, 'fixture');
  const profileParent = validateRegularPath(options.profileParent, 'profileParent');
  return { chrome, fixture, profileParent, fixtureURL: pathToFileURL(fixture).href };
}

function safeRemoveProfile(profileDirectory: string, profileParent: string): void {
  try {
    const stat = lstatSync(profileDirectory);
    const canonical = realpathSync(profileDirectory);
    if (!stat.isDirectory() || stat.isSymbolicLink() || canonical !== profileDirectory
      || dirname(canonical) !== profileParent || !basename(canonical).startsWith(CHROME_PROFILE_PREFIX)) {
      throw new HeadlessBrowserError('cleanup_failed');
    }
    const pending: Array<{ path: string; depth: number }> = [{ path: canonical, depth: 0 }];
    let entryCount = 0;
    while (pending.length > 0) {
      const current = pending.pop()!;
      if (current.depth > MAX_PROFILE_DEPTH) throw new HeadlessBrowserError('cleanup_failed');
      const entries = readdirSync(current.path);
      entryCount += entries.length;
      if (entryCount > MAX_PROFILE_ENTRIES) throw new HeadlessBrowserError('cleanup_failed');
      for (const name of entries) {
        const childPath = join(current.path, name);
        const childStat = lstatSync(childPath);
        if (childStat.isDirectory() && !childStat.isSymbolicLink()) {
          pending.push({ path: childPath, depth: current.depth + 1 });
        }
      }
    }
    rmSync(canonical, { recursive: true, force: false, maxRetries: 2, retryDelay: 50 });
  } catch (error) {
    if (error instanceof HeadlessBrowserError) throw error;
    throw new HeadlessBrowserError('cleanup_failed');
  }
}

function validateAXValue(value: unknown): void {
  const record = requireRecord(value);
  if (!requiredString(record.type, 128)) throw new HeadlessBrowserError('protocol_error');
  if (Object.hasOwn(record, 'value') && record.value !== null
    && typeof record.value !== 'string' && typeof record.value !== 'number'
    && typeof record.value !== 'boolean') throw new HeadlessBrowserError('protocol_error');
  if (typeof record.value === 'string' && Buffer.byteLength(record.value, 'utf8') > 65_536) {
    throw new HeadlessBrowserError('protocol_error');
  }
  if (Object.hasOwn(record, 'relatedNodes')) {
    if (!Array.isArray(record.relatedNodes) || record.relatedNodes.length > MAX_AX_PROPERTIES) {
      throw new HeadlessBrowserError('protocol_error');
    }
    for (const related of record.relatedNodes) {
      const node = requireRecord(related);
      if (Object.hasOwn(node, 'backendDOMNodeId') && !Number.isSafeInteger(node.backendDOMNodeId)) {
        throw new HeadlessBrowserError('protocol_error');
      }
      for (const field of ['id', 'text']) {
        if (Object.hasOwn(node, field) && !requiredString(node[field], 65_536)) {
          throw new HeadlessBrowserError('protocol_error');
        }
      }
    }
  }
  if (Object.hasOwn(record, 'sources')) {
    if (!Array.isArray(record.sources) || record.sources.length > MAX_AX_PROPERTIES) {
      throw new HeadlessBrowserError('protocol_error');
    }
    for (const source of record.sources) {
      const item = requireRecord(source);
      if (!requiredString(item.type, 128)) throw new HeadlessBrowserError('protocol_error');
      for (const field of ['attribute', 'nativeSource', 'invalidReason']) {
        if (Object.hasOwn(item, field) && !requiredString(item[field], 65_536)) {
          throw new HeadlessBrowserError('protocol_error');
        }
      }
      for (const field of ['superseded', 'invalid']) {
        if (Object.hasOwn(item, field) && typeof item[field] !== 'boolean') {
          throw new HeadlessBrowserError('protocol_error');
        }
      }
      if (Object.hasOwn(item, 'value')) validateAXValue(item.value);
      if (Object.hasOwn(item, 'nativeSourceValue')) validateAXValue(item.nativeSourceValue);
    }
  }
}

function validateAXProperties(value: unknown): void {
  if (!Array.isArray(value) || value.length > MAX_AX_PROPERTIES) {
    throw new HeadlessBrowserError('protocol_error');
  }
  for (const property of value) {
    const record = requireRecord(property);
    if (!requiredString(record.name, 128) || !Object.hasOwn(record, 'value')) {
      throw new HeadlessBrowserError('protocol_error');
    }
    validateAXValue(record.value);
  }
}

function validateAXTree(value: unknown): CdpAXTree {
  const tree = requireRecord(value);
  if (!Array.isArray(tree.nodes) || tree.nodes.length === 0 || tree.nodes.length > MAX_AX_NODES) {
    throw new HeadlessBrowserError('protocol_error');
  }
  const ids = new Set<string>();
  let childCount = 0;
  for (const valueNode of tree.nodes) {
    const node = requireRecord(valueNode);
    if (!requiredString(node.nodeId, 256) || ids.has(node.nodeId) || typeof node.ignored !== 'boolean') {
      throw new HeadlessBrowserError('protocol_error');
    }
    ids.add(node.nodeId);
    for (const field of ['parentId', 'frameId']) {
      if (Object.hasOwn(node, field) && !requiredString(node[field], 256)) {
        throw new HeadlessBrowserError('protocol_error');
      }
    }
    if (Object.hasOwn(node, 'backendDOMNodeId') && !Number.isSafeInteger(node.backendDOMNodeId)) {
      throw new HeadlessBrowserError('protocol_error');
    }
    for (const field of ['role', 'chromeRole', 'name', 'description', 'value']) {
      if (Object.hasOwn(node, field)) validateAXValue(node[field]);
    }
    for (const field of ['ignoredReasons', 'properties']) {
      if (Object.hasOwn(node, field)) validateAXProperties(node[field]);
    }
    if (Object.hasOwn(node, 'childIds')) {
      if (!Array.isArray(node.childIds) || node.childIds.length > MAX_AX_CHILD_IDS
        || !node.childIds.every((id) => requiredString(id, 256))) {
        throw new HeadlessBrowserError('protocol_error');
      }
      childCount += node.childIds.length;
      if (childCount > MAX_AX_NODES * 4) throw new HeadlessBrowserError('protocol_error');
    }
  }
  return tree as unknown as CdpAXTree;
}

function validateMetrics(value: unknown): Readonly<JsonRecord> {
  const metrics = requireRecord(value);
  const layoutKeys = ['cssLayoutViewport', 'layoutViewport'];
  const visualKeys = ['cssVisualViewport', 'visualViewport'];
  const contentKeys = ['cssContentSize', 'contentSize'];
  const hasObject = (keys: string[]): boolean => keys.some((key) => isRecord(metrics[key]));
  if (!hasObject(layoutKeys) || !hasObject(visualKeys) || !hasObject(contentKeys)) {
    throw new HeadlessBrowserError('protocol_error');
  }
  for (const key of [...layoutKeys, ...visualKeys, ...contentKeys]) {
    if (!Object.hasOwn(metrics, key)) continue;
    const rect = requireRecord(metrics[key]);
    for (const [name, number] of Object.entries(rect)) {
      if (typeof number !== 'number' || !Number.isFinite(number)) throw new HeadlessBrowserError('protocol_error');
      if (['width', 'height', 'clientWidth', 'clientHeight', 'scale'].includes(name) && Number(number) < 0) {
        throw new HeadlessBrowserError('protocol_error');
      }
    }
  }
  const positiveDimensions: Array<[string[], string[]]> = [
    [layoutKeys, ['clientWidth', 'clientHeight']],
    [visualKeys, ['clientWidth', 'clientHeight', 'scale']],
    [contentKeys, ['width', 'height']],
  ];
  for (const [alternates, fields] of positiveDimensions) {
    const selected = alternates.map((key) => metrics[key]).find(isRecord);
    if (!selected || fields.some((field) => typeof selected[field] !== 'number'
      || !Number.isFinite(selected[field]) || Number(selected[field]) <= 0)) {
      throw new HeadlessBrowserError('protocol_error');
    }
  }
  return metrics;
}

function validateFrameInfo(value: unknown): { targetId: string; type: string; url: string; browserContextId?: string } {
  const info = requireRecord(value);
  if (!requiredString(info.targetId) || !requiredString(info.type, 128)
    || typeof info.url !== 'string' || Buffer.byteLength(info.url, 'utf8') > 4_096) {
    throw new HeadlessBrowserError('protocol_error');
  }
  const contextId = info.browserContextId;
  if (contextId !== undefined && !requiredString(contextId)) throw new HeadlessBrowserError('protocol_error');
  return { targetId: info.targetId, type: info.type, url: info.url,
    ...(typeof contextId === 'string' ? { browserContextId: contextId } : {}) };
}

function sameDocument(left: HeadlessDocumentIdentity, right: HeadlessDocumentIdentity): boolean {
  return left.frameId === right.frameId && left.loaderId === right.loaderId && left.url === right.url;
}

export type OwnedBrowserActionFailure = 'source_invalid' | 'evidence_expired' | 'input_rejected' | 'target_occluded';

export type OwnedBrowserCheckboxResult =
  | { readonly outcome: 'rejected'; readonly failureCode: OwnedBrowserActionFailure;
      readonly diagnostic?: HeadlessBrowserDiagnostic }
  | { readonly outcome: 'completed'; readonly submittedMonotonicMs: number; readonly inputEndMonotonicMs: number };

export class OwnedBrowserActionOutcomeUnknown extends Error {
  constructor(readonly submittedMonotonicMs: number, readonly inputEndMonotonicMs: number | null = null) {
    super('owned browser input outcome is unknown');
    this.name = 'OwnedBrowserActionOutcomeUnknown';
  }
}

function actionViewport(metrics: Readonly<JsonRecord>): OwnedBrowserTapViewport | null {
  const layout = isRecord(metrics.cssLayoutViewport) ? metrics.cssLayoutViewport : metrics.layoutViewport;
  const visual = isRecord(metrics.cssVisualViewport) ? metrics.cssVisualViewport : metrics.visualViewport;
  if (!isRecord(layout) || !isRecord(visual)) return null;
  const values = [layout.pageX, layout.pageY, visual.offsetX, visual.offsetY,
    visual.clientWidth, visual.clientHeight];
  if (!values.every((value) => typeof value === 'number' && Number.isFinite(value))) return null;
  return {
    pageX: Number(layout.pageX),
    pageY: Number(layout.pageY),
    offsetX: Number(visual.offsetX),
    offsetY: Number(visual.offsetY),
    clientWidth: Number(visual.clientWidth),
    clientHeight: Number(visual.clientHeight),
  };
}

export interface OwnedBrowserCheckboxAction {
  readonly expectedDocument: HeadlessDocumentIdentity;
  readonly backendDOMNodeId: number;
  readonly expectedChecked: boolean;
  readonly desiredChecked: boolean;
  readonly expectedSourceSequence: number;
  /** Fixed monotonic translation of Core's command_end_ms. */
  readonly inputDeadlineMonotonicMs: number;
  /** Revalidated synchronously immediately before the single CDP Input write. */
  readonly isSourceActive: () => boolean;
}

export interface OwnedBrowserCheckboxSnapshotState {
  readonly backendDOMNodeId: number;
  readonly nodeName: string;
  readonly nativeCheckbox: boolean;
  readonly checked: boolean | null;
  readonly disabled: boolean;
  readonly blocked: boolean;
  readonly ariaCheckedPresent: boolean;
  readonly frameUrl: string;
}

interface ActionSnapshotNode {
  readonly index: number;
  readonly parent: number;
  readonly nodeType: number;
  readonly nodeName: string;
  readonly backendDOMNodeId: number;
  readonly attributes: ReadonlyMap<string, string>;
}

interface ActionSnapshotDocument {
  readonly nodes: readonly ActionSnapshotNode[];
  readonly children: readonly (readonly number[])[];
  readonly checked: ReadonlyMap<number, boolean>;
  readonly layout: ReadonlyMap<number, { readonly bounds: readonly number[]; readonly styles: readonly string[] }>;
  readonly frameUrl: string;
}

function exactRecordKeys(value: JsonRecord, expected: readonly string[]): boolean {
  return Object.keys(value).length === expected.length && expected.every((key) => Object.hasOwn(value, key));
}

function indexedString(value: unknown, strings: readonly string[]): string {
  if (!Number.isInteger(value) || Number(value) < 0 || Number(value) >= strings.length) {
    throw new HeadlessBrowserError('protocol_error');
  }
  return strings[Number(value)]!;
}

function indexedAttributeValue(value: unknown, strings: readonly string[]): string {
  // Chromium's AddString uses -1 for the empty string. Attribute names remain
  // required non-empty indexes; only value slots accept this producer sentinel.
  return value === -1 ? '' : indexedString(value, strings);
}

function diagnosedActionSnapshotPart<T>(
  reason: HeadlessDiagnosticReason,
  operation: () => T,
): T {
  try { return operation(); }
  catch (error) {
    throw toHeadlessError(error).withDiagnostic({ stage: 'checkbox_action_snapshot', reason });
  }
}

function actionSnapshotDocument(value: unknown, expectedUrl: string): ActionSnapshotDocument {
  const { strings, documents } = diagnosedActionSnapshotPart('top_level', () => {
    const snapshot = requireRecord(value);
    if (!exactRecordKeys(snapshot, ['documents', 'strings']) || !Array.isArray(snapshot.strings)
      || snapshot.strings.length > 65_536 || !snapshot.strings.every((item) => typeof item === 'string')
      || !Array.isArray(snapshot.documents) || snapshot.documents.length !== 1) {
      throw new HeadlessBrowserError('protocol_error');
    }
    const strings = snapshot.strings as string[];
    let totalStringBytes = 0;
    for (const item of strings) {
      totalStringBytes += Buffer.byteLength(item, 'utf8');
      if (totalStringBytes > 1_048_576) throw new HeadlessBrowserError('protocol_error');
    }
    return { strings, documents: snapshot.documents as unknown[] };
  });
  const document = diagnosedActionSnapshotPart('top_level', () => requireRecord(documents[0]));
  diagnosedActionSnapshotPart('document_url_index', () => {
    const documentUrl = indexedString(document.documentURL, strings);
    if (documentUrl !== expectedUrl) throw new HeadlessBrowserError('document_changed');
  });
  const nodeTree = diagnosedActionSnapshotPart('node_tables', () => requireRecord(document.nodes));
  const { backendIds, nodeTypes, nodeNames, parentIndices, attributes } =
    diagnosedActionSnapshotPart('node_tables', () => {
      const backendIds = nodeTree.backendNodeId;
      const nodeTypes = nodeTree.nodeType;
      const nodeNames = nodeTree.nodeName;
      const parentIndices = nodeTree.parentIndex;
      const attributes = nodeTree.attributes;
      if (!Array.isArray(backendIds) || backendIds.length === 0 || backendIds.length > MAX_ACTION_DOM_NODES
        || !Array.isArray(nodeTypes) || nodeTypes.length !== backendIds.length
        || !Array.isArray(nodeNames) || nodeNames.length !== backendIds.length
        || !Array.isArray(parentIndices) || parentIndices.length !== backendIds.length
        || !Array.isArray(attributes) || attributes.length !== backendIds.length) {
        throw new HeadlessBrowserError('protocol_error');
      }
      return { backendIds, nodeTypes, nodeNames, parentIndices, attributes };
    });

  diagnosedActionSnapshotPart('shadow_data', () => {
    if (!Object.hasOwn(nodeTree, 'shadowRootType')) return;
    const roots = requireRecord(nodeTree.shadowRootType);
    if (!exactRecordKeys(roots, ['index', 'value']) || !Array.isArray(roots.index) || !Array.isArray(roots.value)
      || roots.index.length !== roots.value.length || roots.index.length !== 0) {
      throw new HeadlessBrowserError('protocol_error');
    }
  });

  const nodes: ActionSnapshotNode[] = [];
  const backendToIndex = new Map<number, number>();
  const children: number[][] = Array.from({ length: backendIds.length }, () => []);
  let roots = 0;
  diagnosedActionSnapshotPart('node_row', () => {
    for (let index = 0; index < backendIds.length; index += 1) {
      const backendDOMNodeId = backendIds[index];
      const nodeType = nodeTypes[index];
      const parent = parentIndices[index];
      const nodeName = indexedString(nodeNames[index], strings);
      if (!Number.isInteger(backendDOMNodeId) || Number(backendDOMNodeId) <= 0
        || Number(backendDOMNodeId) > 2_147_483_647 || backendToIndex.has(Number(backendDOMNodeId))
        || !Number.isInteger(nodeType) || Number(nodeType) < 1 || Number(nodeType) > 12
        || !Number.isInteger(parent) || Number(parent) < -1 || Number(parent) >= backendIds.length || parent === index
        || nodeName.length === 0 || Buffer.byteLength(nodeName, 'utf8') > 256) {
        throw new HeadlessBrowserError('protocol_error');
      }
      if (parent === -1) roots += 1;
      else children[Number(parent)]!.push(index);
      const parsedAttributes = diagnosedActionSnapshotPart('attributes', () => {
        const rawAttributes = attributes[index];
        if (!Array.isArray(rawAttributes) || rawAttributes.length % 2 !== 0 || rawAttributes.length > 512) {
          throw new HeadlessBrowserError('protocol_error');
        }
        const parsedAttributes = new Map<string, string>();
        for (let offset = 0; offset < rawAttributes.length; offset += 2) {
          const name = indexedString(rawAttributes[offset], strings).toLowerCase();
          const text = indexedAttributeValue(rawAttributes[offset + 1], strings);
          if (!name || name.length > 256 || parsedAttributes.has(name) || Buffer.byteLength(text, 'utf8') > 4_096) {
            throw new HeadlessBrowserError('protocol_error');
          }
          parsedAttributes.set(name, text);
        }
        return parsedAttributes;
      });
      const node: ActionSnapshotNode = {
        index,
        parent: Number(parent),
        nodeType: Number(nodeType),
        nodeName: nodeName.toUpperCase(),
        backendDOMNodeId: Number(backendDOMNodeId),
        attributes: parsedAttributes,
      };
      nodes.push(node);
      backendToIndex.set(node.backendDOMNodeId, index);
    }
  });
  diagnosedActionSnapshotPart('root_graph', () => {
    if (roots !== 1 || nodes[0]?.nodeType !== 9) throw new HeadlessBrowserError('protocol_error');
    for (let index = 0; index < nodes.length; index += 1) {
      let current = index;
      let depth = 0;
      while (current !== 0) {
        if (++depth > 256 || nodes[current]!.parent < 0) throw new HeadlessBrowserError('protocol_error');
        current = nodes[current]!.parent;
      }
    }
  });

  const checked = new Map<number, boolean>();
  diagnosedActionSnapshotPart('checked_data', () => {
    if (!Object.hasOwn(nodeTree, 'inputChecked')) return;
    const rare = requireRecord(nodeTree.inputChecked);
    // DOMSnapshot.RareBooleanData represents true values by index membership;
    // false is the omitted default. It has no parallel `value` array.
    if (!exactRecordKeys(rare, ['index']) || !Array.isArray(rare.index)
      || rare.index.length > backendIds.length) {
      throw new HeadlessBrowserError('protocol_error');
    }
    for (const index of rare.index) {
      if (!Number.isInteger(index) || Number(index) < 0 || Number(index) >= nodes.length
        || checked.has(Number(index))) throw new HeadlessBrowserError('protocol_error');
      const node = nodes[Number(index)]!;
      const inputType = node.attributes.get('type')?.toLowerCase();
      if (node.nodeType !== 1 || node.nodeName !== 'INPUT'
        || (inputType !== 'checkbox' && inputType !== 'radio')) {
        throw new HeadlessBrowserError('protocol_error');
      }
      checked.set(Number(index), true);
    }
    // The sparse boolean data's default is false. Treat it as a known checkbox
    // state only when the queried target is actually a native checkbox.
    for (const node of nodes) {
      if (node.nodeType === 1 && node.nodeName === 'INPUT'
        && node.attributes.get('type')?.toLowerCase() === 'checkbox'
        && !checked.has(node.index)) checked.set(node.index, false);
    }
  });

  const layout = new Map<number, { bounds: readonly number[]; styles: readonly string[] }>();
  const layoutTables = diagnosedActionSnapshotPart('layout_tables', () => {
    if (document.layout === undefined) return null;
    const layoutTree = requireRecord(document.layout);
    if (!Array.isArray(layoutTree.nodeIndex) || !Array.isArray(layoutTree.bounds)
      || layoutTree.nodeIndex.length !== layoutTree.bounds.length || layoutTree.nodeIndex.length > nodes.length
      || !Array.isArray(layoutTree.styles) || layoutTree.styles.length !== layoutTree.nodeIndex.length) {
      throw new HeadlessBrowserError('protocol_error');
    }
    const nodeIndexes: unknown[] = layoutTree.nodeIndex;
    const boundsRows: unknown[] = layoutTree.bounds;
    const styleRows: unknown[] = layoutTree.styles;
    return {
      nodeIndexes,
      boundsRows,
      styleRows,
    };
  });
  if (layoutTables !== null) {
    diagnosedActionSnapshotPart('layout_row_style', () => {
      for (let offset = 0; offset < layoutTables.nodeIndexes.length; offset += 1) {
        const index = layoutTables.nodeIndexes[offset];
        const bounds = layoutTables.boundsRows[offset];
        const styleIndices = layoutTables.styleRows[offset];
        const node = Number.isInteger(index) && Number(index) >= 0 && Number(index) < nodes.length
          ? nodes[Number(index)]
          : undefined;
        const isDocumentRoot = node?.index === 0 && node.nodeType === 9 && node.nodeName === '#DOCUMENT';
        if (!Number.isInteger(index) || Number(index) < 0 || Number(index) >= nodes.length
          || !Array.isArray(bounds) || bounds.length !== 4
          || !bounds.every((item) => typeof item === 'number' && Number.isFinite(item))
          || !Array.isArray(styleIndices)
          || styleIndices.length !== (isDocumentRoot ? 0 : ACTION_STYLE_NAMES.length)
          || layout.has(Number(index))) {
          throw new HeadlessBrowserError('protocol_error');
        }
        const styles = styleIndices.map((item) => indexedString(item, strings));
        layout.set(Number(index), { bounds: bounds as number[], styles });
      }
    });
  }
  return { nodes, children, checked, layout, frameUrl: expectedUrl };
}

/** Pure, bounded DOMSnapshot interpretation used to gate one fixed native checkbox. */
export function inspectOwnedBrowserCheckboxSnapshot(
  value: unknown,
  expectedUrl: string,
  backendDOMNodeId: number,
): OwnedBrowserCheckboxSnapshotState {
  if (!requiredString(expectedUrl, 4_096) || !Number.isInteger(backendDOMNodeId)
    || backendDOMNodeId <= 0 || backendDOMNodeId > 2_147_483_647) throw new HeadlessBrowserError('invalid_input');
  const document = actionSnapshotDocument(value, expectedUrl);
  const targetIndex = document.nodes.findIndex((node) => node.backendDOMNodeId === backendDOMNodeId);
  if (targetIndex < 0) throw new HeadlessBrowserError('document_changed');
  const target = document.nodes[targetIndex]!;
  const nativeCheckbox = target.nodeType === 1 && target.nodeName === 'INPUT'
    && target.attributes.get('type')?.toLowerCase() === 'checkbox';

  const ancestors = new Set<number>();
  let current = targetIndex;
  while (current >= 0) {
    ancestors.add(current);
    if (current === 0) break;
    current = document.nodes[current]!.parent;
  }
  let disabled = target.attributes.has('disabled');
  let blocked = false;
  let ariaCheckedPresent = target.attributes.has('aria-checked');
  for (const index of ancestors) {
    const node = document.nodes[index]!;
    blocked ||= node.attributes.has('inert') || node.attributes.get('aria-disabled')?.trim().toLowerCase() === 'true';
    if (index === targetIndex) continue;
    if (node.nodeName !== 'FIELDSET' || !node.attributes.has('disabled')) continue;
    const firstLegend = document.children[index]!.find((child) => document.nodes[child]!.nodeType === 1
      && document.nodes[child]!.nodeName === 'LEGEND') ?? -1;
    if (firstLegend < 0 || !ancestors.has(firstLegend)) disabled = true;
  }
  const checked = document.checked.get(targetIndex) ?? null;
  if (!document.layout.has(targetIndex)) blocked = true;
  for (const index of ancestors) {
    const layout = document.layout.get(index);
    if (!layout) continue;
    const [x, y, width, height] = layout.bounds;
    if (index === targetIndex && (width! <= 0 || height! <= 0)) blocked = true;
    // Chromium emits the Document's LayoutView row, but it has no Element
    // computed style. Its empty style vector is valid and cannot establish or
    // weaken CSS visibility for the checkbox or its element ancestors.
    if (index === 0 && document.nodes[index]!.nodeType === 9) {
      if (![x, y, width, height].every((item) => typeof item === 'number' && Number.isFinite(item))) {
        throw new HeadlessBrowserError('protocol_error');
      }
      continue;
    }
    const [display, visibility, opacity, pointerEvents] = layout.styles.map((item) => item.toLowerCase());
    const opacityValue = Number.parseFloat(opacity!);
    if (display === 'none' || visibility === 'hidden' || visibility === 'collapse'
      || pointerEvents === 'none' || !Number.isFinite(opacityValue) || opacityValue <= 0) blocked = true;
    if (![x, y, width, height].every((item) => typeof item === 'number' && Number.isFinite(item))) {
      throw new HeadlessBrowserError('protocol_error');
    }
  }
  if (!nativeCheckbox) disabled = true;
  return Object.freeze({
    backendDOMNodeId,
    nodeName: target.nodeName,
    nativeCheckbox,
    checked,
    disabled,
    blocked,
    ariaCheckedPresent,
    frameUrl: document.frameUrl,
  });
}

function validateDocumentIdentity(value: unknown): HeadlessDocumentIdentity {
  const record = requireRecord(value);
  if (Object.keys(record).sort().join(',') !== 'frameId,loaderId,url'
    || !requiredString(record.frameId, 256) || !requiredString(record.loaderId, 256)
    || !requiredString(record.url, 4_096)) throw new HeadlessBrowserError('invalid_input');
  return { frameId: record.frameId, loaderId: record.loaderId, url: record.url };
}

function pngBytes(value: unknown): Buffer {
  if (!requiredString(value, 8 * 1024 * 1024) || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) {
    throw new HeadlessBrowserError('protocol_error');
  }
  const bytes = Buffer.from(value, 'base64');
  if (bytes.length < 20 || bytes.length > MAX_CAPTURE_BYTES || bytes.toString('base64') !== value
    || !bytes.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)
    || !bytes.subarray(bytes.length - 8, bytes.length - 4).equals(Buffer.from('IEND'))) {
    bytes.fill(0);
    throw new HeadlessBrowserError('protocol_error');
  }
  return bytes;
}

function toHeadlessError(error: unknown, fallback: HeadlessBrowserErrorCode = 'protocol_error'): HeadlessBrowserError {
  return error instanceof HeadlessBrowserError ? error : new HeadlessBrowserError(fallback);
}

function withHeadlessDiagnostic(
  error: unknown,
  stage: HeadlessDiagnosticStage,
  commandMethod: HeadlessCdpCommandMethod,
): HeadlessBrowserError {
  return toHeadlessError(error).withDiagnostic({ stage, commandMethod });
}

async function diagnosedCommand<T>(
  operation: Promise<T>,
  stage: HeadlessDiagnosticStage,
  commandMethod: HeadlessCdpCommandMethod,
): Promise<T> {
  try { return await operation; }
  catch (error) { throw withHeadlessDiagnostic(error, stage, commandMethod); }
}

function diagnosedParse<T>(
  operation: () => T,
  stage: HeadlessDiagnosticStage,
  commandMethod: HeadlessCdpCommandMethod,
): T {
  try { return operation(); }
  catch (error) { throw withHeadlessDiagnostic(error, stage, commandMethod); }
}

function streamIsReadable(value: unknown): value is Readable {
  return value instanceof Readable;
}

function streamIsWritable(value: unknown): value is Writable {
  return value instanceof Writable;
}

function waitForChildClose(child: ChildProcess, timeoutMs: number, isClosed: () => boolean): Promise<boolean> {
  if (isClosed()) return Promise.resolve(true);
  return new Promise<boolean>((resolve) => {
    const timer = setTimeout(() => finish(false), timeoutMs);
    const finish = (closed: boolean): void => {
      clearTimeout(timer);
      child.off('close', onClose);
      resolve(closed);
    };
    const onClose = (): void => finish(true);
    child.once('close', onClose);
  });
}

export interface OwnedBrowserLaunchOptions {
  readonly chromeExecutable: string;
  readonly fixturePath: string;
  readonly profileParent: string;
}

export interface OwnedBrowserSession {
  readonly pid: number;
  readonly browserSessionId: string;
  readonly sourceSequence: number;
  observe(): Promise<HeadlessObservation>;
  capturePng(expectedDocument: HeadlessDocumentIdentity): Promise<HeadlessPngCapture>;
  setCheckbox(action: OwnedBrowserCheckboxAction): Promise<OwnedBrowserCheckboxResult>;
  /** Synchronously retires the CDP transport and signals only this owned Chrome child to exit. */
  retireNow(): void;
  close(): Promise<void>;
}

/** @internal Pure-test seam for exercising the production session queue and final CDP write guard. */
export interface OwnedBrowserSessionTestHarnessOptions {
  readonly child: ChildProcess;
  readonly pipe: CdpPipe;
  readonly profileDirectory: string;
  readonly profileParent: string;
  readonly fixtureURL: string;
  readonly expectedDocument: HeadlessDocumentIdentity;
  readonly browserContextId: string;
  readonly targetId: string;
  readonly sessionId: string;
  readonly sequence: number;
}

export interface OwnedBrowserTapViewport {
  readonly clientWidth: number;
  readonly clientHeight: number;
  readonly pageX: number;
  readonly pageY: number;
  readonly offsetX: number;
  readonly offsetY: number;
}

/** Pure geometry gate: choose an integer CSS point inside both a target quad and an unscrolled viewport. */
export function chooseOwnedCheckboxTapPoint(
  quads: readonly (readonly number[])[],
  viewport: OwnedBrowserTapViewport,
): { readonly x: number; readonly y: number } | null {
  if (!Number.isFinite(viewport.clientWidth) || viewport.clientWidth <= 0
    || !Number.isFinite(viewport.clientHeight) || viewport.clientHeight <= 0
    || viewport.pageX !== 0 || viewport.pageY !== 0 || viewport.offsetX !== 0 || viewport.offsetY !== 0) return null;
  for (const quad of quads) {
    if (quad.length !== 8 || !quad.every(Number.isFinite)) continue;
    const xs = [quad[0]!, quad[2]!, quad[4]!, quad[6]!];
    const ys = [quad[1]!, quad[3]!, quad[5]!, quad[7]!];
    const minX = Math.min(...xs); const maxX = Math.max(...xs);
    const minY = Math.min(...ys); const maxY = Math.max(...ys);
    if (!(maxX > minX && maxY > minY)) continue;
    const x = Math.floor(xs.reduce((sum, value) => sum + value, 0) / xs.length);
    const y = Math.floor(ys.reduce((sum, value) => sum + value, 0) / ys.length);
    if (x < 0 || y < 0 || x >= viewport.clientWidth || y >= viewport.clientHeight) continue;
    let sign = 0;
    let inside = true;
    for (let index = 0; index < 4; index += 1) {
      const next = (index + 1) % 4;
      const cross = (xs[next]! - xs[index]!) * (y - ys[index]!)
        - (ys[next]! - ys[index]!) * (x - xs[index]!);
      if (Math.abs(cross) < 1e-9) continue;
      const currentSign = Math.sign(cross);
      if (sign !== 0 && currentSign !== sign) { inside = false; break; }
      sign = currentSign;
    }
    if (inside) return Object.freeze({ x, y });
  }
  return null;
}

function checkboxAXState(value: unknown, backendDOMNodeId: number, frameId: string): {
  readonly checked: boolean | null;
  readonly disabled: boolean | null;
} {
  const tree = validateAXTree(value);
  const matches = tree.nodes.filter((node) => node.backendDOMNodeId === backendDOMNodeId);
  if (matches.length !== 1) throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_action_ax' });
  const node = matches[0]!;
  const role = node.role?.value;
  if (node.ignored || (node.frameId !== undefined && node.frameId !== frameId) || role !== 'checkbox') {
    throw new HeadlessBrowserError('invalid_input');
  }
  const checkedProperties = node.properties?.filter((property) => property.name === 'checked') ?? [];
  const disabledProperties = node.properties?.filter((property) => property.name === 'disabled') ?? [];
  if (checkedProperties.length !== 1 || disabledProperties.length > 1) {
    throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_action_ax' });
  }
  const checkedValue = checkedProperties[0]!.value;
  let checked: boolean | null = null;
  if (checkedValue.type === 'tristate') {
    if (checkedValue.value === 'true') checked = true;
    else if (checkedValue.value === 'false') checked = false;
    else if (checkedValue.value === 'mixed' || checkedValue.value === undefined || checkedValue.value === null) checked = null;
    else throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_action_ax' });
  } else if ((checkedValue.type === 'boolean' || checkedValue.type === 'booleanOrUndefined')
    && typeof checkedValue.value === 'boolean') {
    checked = checkedValue.value;
  } else if (checkedValue.type === 'booleanOrUndefined'
    && (checkedValue.value === undefined || checkedValue.value === null)) {
    checked = null;
  } else {
    throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_action_ax' });
  }
  let disabled: boolean | null = null;
  if (disabledProperties.length === 1) {
    const disabledValue = disabledProperties[0]!.value;
    if ((disabledValue.type === 'boolean' || disabledValue.type === 'booleanOrUndefined')
      && typeof disabledValue.value === 'boolean') {
      disabled = disabledValue.value;
    } else if (disabledValue.type === 'booleanOrUndefined'
      && (disabledValue.value === undefined || disabledValue.value === null)) {
      disabled = null;
    } else throw new HeadlessBrowserError('protocol_error', { stage: 'checkbox_action_ax' });
  }
  return { checked, disabled };
}

class OwnedBrowserSessionImpl implements OwnedBrowserSession {
  readonly pid: number;
  readonly browserSessionId = randomUUID();
  private browserContextId: string | null = null;
  private targetId: string | null = null;
  private sessionId: string | null = null;
  private expectedDocument: HeadlessDocumentIdentity | null = null;
  private sequence = 0;
  private fault: HeadlessBrowserError | null = null;
  private closing = false;
  private closed = false;
  private closeTask: Promise<void> | null = null;
  private operationTail: Promise<void> = Promise.resolve();
  private childClosed = false;
  private diagnosticStage: HeadlessDiagnosticStage = 'initialization';
  private targetCreationInProgress = false;
  private createdTargetIds: string[] = [];
  private readonly startupTargetIds = new Set<string>();
  private readonly closingStartupTargetIds = new Set<string>();
  private readonly removeEventListener: () => void;

  private constructor(
    private readonly child: ChildProcess,
    private readonly pipe: CdpPipe,
    private readonly profileDirectory: string,
    private readonly profileParent: string,
    private readonly fixtureURL: string,
    hasConfirmedChildClose: () => boolean,
  ) {
    if (!child.pid) throw new HeadlessBrowserError('spawn_failed');
    this.pid = child.pid;
    this.childClosed = hasConfirmedChildClose();
    this.removeEventListener = pipe.onEvent((event) => this.onProtocolEvent(event));
    child.once('close', () => {
      this.childClosed = true;
      if (!this.closing) this.invalidate('protocol_error');
    });
    child.once('error', () => { if (!this.closing) this.invalidate('spawn_failed'); });
  }

  get sourceSequence(): number { return this.sequence; }

  static create(
    child: ChildProcess,
    pipe: CdpPipe,
    profileDirectory: string,
    profileParent: string,
    fixtureURL: string,
    hasConfirmedChildClose: () => boolean,
  ): OwnedBrowserSessionImpl {
    return new OwnedBrowserSessionImpl(child, pipe, profileDirectory, profileParent, fixtureURL,
                                       hasConfirmedChildClose);
  }

  static forTests(options: OwnedBrowserSessionTestHarnessOptions): OwnedBrowserSessionImpl {
    const expectedDocument = validateDocumentIdentity(options.expectedDocument);
    if (!requiredString(options.profileDirectory) || !requiredString(options.profileParent)
      || !requiredString(options.fixtureURL) || !requiredString(options.browserContextId)
      || !requiredString(options.targetId) || !requiredString(options.sessionId)
      || !Number.isSafeInteger(options.sequence) || options.sequence <= 0) {
      throw new HeadlessBrowserError('invalid_input');
    }
    const session = OwnedBrowserSessionImpl.create(options.child, options.pipe, options.profileDirectory,
      options.profileParent, options.fixtureURL,
      () => options.child.exitCode !== null || options.child.signalCode !== null);
    session.browserContextId = options.browserContextId;
    session.targetId = options.targetId;
    session.sessionId = options.sessionId;
    session.expectedDocument = expectedDocument;
    session.sequence = options.sequence;
    return session;
  }

  async initialize(): Promise<void> {
    try {
      await this.initializeTarget();
    } catch (error) {
      const diagnosed = toHeadlessError(error).withDiagnostic({ stage: this.diagnosticStage });
      this.pipe.invalidate(diagnosed);
      throw diagnosed;
    }
  }

  observe(): Promise<HeadlessObservation> {
    return this.enqueue(async () => {
      const readStartedMonotonicMs = performance.now();
      const before = await this.readDocumentIdentity();
      const rawAXValue = await this.pipe.fullAXTree(this.requireSessionId());
      const rawAX = validateAXTree(rawAXValue);
      const checkboxNodes = rawAX.nodes.filter(node => !node.ignored && node.role?.value === 'checkbox'
        && Number.isInteger(node.backendDOMNodeId) && node.backendDOMNodeId! > 0
        && node.backendDOMNodeId! <= 2_147_483_647);
      if (checkboxNodes.length > MAX_CHECKBOX_STATE_READS) throw new HeadlessBrowserError('protocol_error');
      const checkboxStates: HeadlessCheckboxState[] = [];
      const backendIds = new Set<number>();
      for (const node of checkboxNodes) {
        const backendId = node.backendDOMNodeId!;
        if (backendIds.has(backendId)) throw new HeadlessBrowserError('protocol_error');
        backendIds.add(backendId);
      }
      const states = checkboxNodes.length === 0 ? [] : await this.pipe.readCheckboxStates(
        this.requireSessionId(), checkboxNodes.map(node => node.backendDOMNodeId!),
      );
      if (states.length !== checkboxNodes.length) throw new HeadlessBrowserError('protocol_error');
      for (let index = 0; index < states.length; index += 1) {
        const backendId = checkboxNodes[index]!.backendDOMNodeId!;
        const state = states[index]!;
        if (state.backendDOMNodeId !== backendId || state.documentUrl !== before.url
          || !state.connected || !state.topDocument) throw new HeadlessBrowserError('document_changed');
        // Custom ARIA checkboxes remain AX-only: this probe establishes native HTML state only.
        if (state.nativeCheckbox) checkboxStates.push(state);
      }
      const rawMetrics = await this.pipe.layoutMetrics(this.requireSessionId());
      const after = await this.readDocumentIdentity();
      this.requireStableDocument(before, after);
      const layoutMetrics = validateMetrics(rawMetrics);
      return {
        provenance: this.nextProvenance(readStartedMonotonicMs, performance.now()),
        document: before,
        rawAX,
        checkboxStates,
        layoutMetrics,
      };
    });
  }

  capturePng(expectedDocument: HeadlessDocumentIdentity): Promise<HeadlessPngCapture> {
    let expected: HeadlessDocumentIdentity;
    try { expected = validateDocumentIdentity(expectedDocument); }
    catch (error) { return Promise.reject(error); }
    return this.enqueue(async () => {
      const readStartedMonotonicMs = performance.now();
      const before = await this.readDocumentIdentity();
      this.requireStableDocument(expected, before);
      const result = await this.pipe.captureScreenshot(this.requireSessionId());
      const after = await this.readDocumentIdentity();
      this.requireStableDocument(expected, after);
      const png = pngBytes(result.data);
      return {
        provenance: this.nextProvenance(readStartedMonotonicMs, performance.now()),
        document: expected,
        png,
      };
    });
  }

  setCheckbox(action: OwnedBrowserCheckboxAction): Promise<OwnedBrowserCheckboxResult> {
    let expected: HeadlessDocumentIdentity;
    try {
      expected = validateDocumentIdentity(action.expectedDocument);
      if (!Number.isInteger(action.backendDOMNodeId) || action.backendDOMNodeId <= 0
        || action.backendDOMNodeId > 2_147_483_647 || typeof action.expectedChecked !== 'boolean'
        || typeof action.desiredChecked !== 'boolean' || !Number.isSafeInteger(action.expectedSourceSequence)
        || action.expectedSourceSequence <= 0 || !Number.isFinite(action.inputDeadlineMonotonicMs)
        || action.inputDeadlineMonotonicMs < 0 || typeof action.isSourceActive !== 'function') {
        throw new HeadlessBrowserError('invalid_input');
      }
    } catch (error) {
      return Promise.reject(error);
    }
    return this.enqueue(async () => {
      if (this.sequence !== action.expectedSourceSequence) {
        return { outcome: 'rejected', failureCode: 'evidence_expired' } as const;
      }
      if (action.expectedChecked === action.desiredChecked) {
        return { outcome: 'rejected', failureCode: 'input_rejected' } as const;
      }
      const sessionId = this.requireSessionId();
      const before = await this.readDocumentIdentity();
      this.requireStableDocument(expected, before);
      const readCheckbox = async (): Promise<OwnedBrowserCheckboxSnapshotState & {
        readonly diagnostic?: HeadlessBrowserDiagnostic;
      }> => {
        const snapshot = await diagnosedCommand(
          this.pipe.captureActionSnapshot(sessionId),
          'checkbox_action_snapshot',
          'DOMSnapshot.captureSnapshot',
        );
        const state = diagnosedParse(
          () => inspectOwnedBrowserCheckboxSnapshot(snapshot, expected.url, action.backendDOMNodeId),
          'checkbox_action_snapshot',
          'DOMSnapshot.captureSnapshot',
        );
        if (!state.nativeCheckbox || state.ariaCheckedPresent) {
          return {
            ...state,
            disabled: true,
            ...(!state.nativeCheckbox ? { diagnostic: {
              stage: 'checkbox_action_snapshot',
              commandMethod: 'DOMSnapshot.captureSnapshot',
            } as const } : {}),
          };
        }
        const ax = await diagnosedCommand(
          this.pipe.partialAXTreeForBackendNode(sessionId, action.backendDOMNodeId),
          'checkbox_action_ax',
          'Accessibility.getPartialAXTree',
        );
        let axState: ReturnType<typeof checkboxAXState>;
        try {
          axState = diagnosedParse(
            () => checkboxAXState(ax, action.backendDOMNodeId, expected.frameId),
            'checkbox_action_ax',
            'Accessibility.getPartialAXTree',
          );
        }
        catch (error) {
          if (error instanceof HeadlessBrowserError && error.code === 'invalid_input') {
            return {
              ...state,
              nativeCheckbox: false,
              disabled: true,
              ...(error.diagnostic ? { diagnostic: error.diagnostic } : {}),
            };
          }
          throw error;
        }
        if (state.checked === null || axState.checked === null || axState.checked !== state.checked) {
          return { ...state, checked: null, disabled: true };
        }
        if (axState.disabled === true) return { ...state, disabled: true };
        return state;
      };
      const state = await readCheckbox();
      if (!state.nativeCheckbox) {
        return {
          outcome: 'rejected',
          failureCode: 'source_invalid',
          ...(state.diagnostic ? { diagnostic: state.diagnostic } : {}),
        } as const;
      }
      if (state.checked === null || state.checked !== action.expectedChecked) {
        return { outcome: 'rejected', failureCode: 'evidence_expired' } as const;
      }
      if (state.disabled) return { outcome: 'rejected', failureCode: 'input_rejected' } as const;
      if (state.blocked) return { outcome: 'rejected', failureCode: 'target_occluded' } as const;
      const metricsValue = await diagnosedCommand(
        this.pipe.layoutMetrics(sessionId, 'checkbox_action_geometry'),
        'checkbox_action_geometry',
        'Page.getLayoutMetrics',
      );
      const metrics = diagnosedParse(
        () => validateMetrics(metricsValue), 'checkbox_action_geometry', 'Page.getLayoutMetrics',
      );
      const viewport = actionViewport(metrics);
      const quads = await diagnosedCommand(
        this.pipe.contentQuadsForBackendNode(sessionId, action.backendDOMNodeId),
        'checkbox_action_geometry',
        'DOM.getContentQuads',
      );
      const point = viewport ? chooseOwnedCheckboxTapPoint(quads, viewport) : null;
      if (!point) return { outcome: 'rejected', failureCode: 'target_occluded' } as const;
      const hit = await diagnosedCommand(
        this.pipe.nodeForLocation(sessionId, point.x, point.y),
        'checkbox_action_hit_test',
        'DOM.getNodeForLocation',
      );
      if (hit.backendNodeId !== action.backendDOMNodeId || hit.frameId !== expected.frameId) {
        return { outcome: 'rejected', failureCode: 'target_occluded' } as const;
      }

      // Re-read native state and identity after hit-testing; there is no script,
      // selector, focus, scroll, or DOM write between this check and Input.
      const finalState = await readCheckbox();
      if (!finalState.nativeCheckbox || finalState.checked === null
        || finalState.checked !== action.expectedChecked || finalState.disabled) {
        return { outcome: 'rejected', failureCode: 'evidence_expired' } as const;
      }
      if (finalState.blocked) return { outcome: 'rejected', failureCode: 'target_occluded' } as const;
      const finalMetricsValue = await diagnosedCommand(
        this.pipe.layoutMetrics(sessionId, 'checkbox_action_geometry'),
        'checkbox_action_geometry',
        'Page.getLayoutMetrics',
      );
      const finalMetrics = diagnosedParse(
        () => validateMetrics(finalMetricsValue), 'checkbox_action_geometry', 'Page.getLayoutMetrics',
      );
      const finalViewport = actionViewport(finalMetrics);
      const finalQuads = await diagnosedCommand(
        this.pipe.contentQuadsForBackendNode(sessionId, action.backendDOMNodeId),
        'checkbox_action_geometry',
        'DOM.getContentQuads',
      );
      const finalPoint = finalViewport ? chooseOwnedCheckboxTapPoint(finalQuads, finalViewport) : null;
      if (!finalPoint) return { outcome: 'rejected', failureCode: 'target_occluded' } as const;
      const finalHit = await diagnosedCommand(
        this.pipe.nodeForLocation(sessionId, finalPoint.x, finalPoint.y),
        'checkbox_action_hit_test',
        'DOM.getNodeForLocation',
      );
      if (finalHit.backendNodeId !== action.backendDOMNodeId || finalHit.frameId !== expected.frameId) {
        return { outcome: 'rejected', failureCode: 'target_occluded' } as const;
      }
      const afterChecks = await this.readDocumentIdentity();
      this.requireStableDocument(expected, afterChecks);

      let submittedMonotonicMs: number | null = null;
      try {
        await this.pipe.synthesizeCheckboxMouseTap(sessionId, finalPoint.x, finalPoint.y, {
          deadlineMonotonicMs: action.inputDeadlineMonotonicMs,
          isActive: action.isSourceActive,
          onWriteStart: (value) => { submittedMonotonicMs = value; },
        });
      } catch (error) {
        if (error instanceof HeadlessBrowserInputNotSubmitted) {
          return {
            outcome: 'rejected',
            failureCode: error.reason === 'deadline_expired' ? 'evidence_expired' : 'source_invalid',
          } as const;
        }
        throw new OwnedBrowserActionOutcomeUnknown(submittedMonotonicMs ?? performance.now());
      }
      const actualSubmittedAt = submittedMonotonicMs;
      if (actualSubmittedAt === null) throw new OwnedBrowserActionOutcomeUnknown(performance.now());
      const inputEndMonotonicMs = performance.now();
      if (this.closing || this.closed || this.childClosed || this.pipe.isRetired) {
        throw new OwnedBrowserActionOutcomeUnknown(actualSubmittedAt, inputEndMonotonicMs);
      }
      return { outcome: 'completed', submittedMonotonicMs: actualSubmittedAt, inputEndMonotonicMs } as const;
    });
  }

  /** Retire and kill synchronously; profile cleanup remains awaitable through close(). */
  retireNow(): void {
    if (this.closed || this.childClosed) return;
    this.closing = true;
    const error = this.fault ?? new HeadlessBrowserError('closed', { stage: 'cleanup' });
    this.fault = error;
    this.pipe.invalidate(error);
    try { this.child.kill('SIGKILL'); } catch { /* Exact owned Chrome child only. */ }
    if (!this.closeTask) this.closeTask = this.closeOwnedBrowser();
  }

  close(): Promise<void> {
    if (this.closeTask) return this.closeTask;
    this.closing = true;
    this.closeTask = this.closeOwnedBrowser();
    return this.closeTask;
  }

  private async initializeTarget(): Promise<void> {
    this.diagnosticStage = 'target_discovery';
    await this.pipe.setTargetDiscovery(true, STARTUP_TARGET_DISCOVERY_TIMEOUT_MS);
    this.diagnosticStage = 'startup_target_roster';
    const initial = await this.pipe.getTargets();
    for (const rawInfo of initial) {
      const info = validateFrameInfo(rawInfo);
      if (info.type !== 'page' || !['about:blank', 'chrome://newtab/'].includes(info.url)
        || info.browserContextId !== undefined) throw new HeadlessBrowserError('protocol_error');
      this.startupTargetIds.add(info.targetId);
      this.closingStartupTargetIds.add(info.targetId);
    }
    this.diagnosticStage = 'startup_target_close';
    for (const targetId of this.startupTargetIds) await this.pipe.closeTarget(targetId);
    this.diagnosticStage = 'startup_target_roster';
    if ((await this.pipe.getTargets()).length !== 0) throw new HeadlessBrowserError('protocol_error');

    this.diagnosticStage = 'context_creation';
    this.browserContextId = await this.pipe.createBrowserContext();
    this.diagnosticStage = 'target_creation';
    this.targetCreationInProgress = true;
    this.createdTargetIds = [];
    const targetId = await this.pipe.createAboutBlankTarget(this.browserContextId);
    this.targetCreationInProgress = false;
    this.targetId = targetId;
    if (this.createdTargetIds.some((created) => created !== targetId)) {
      throw new HeadlessBrowserError('protocol_error');
    }
    this.diagnosticStage = 'target_roster';
    const targetInfos = await this.pipe.getTargets();
    if (targetInfos.length !== 1) throw new HeadlessBrowserError('protocol_error');
    const target = validateFrameInfo(targetInfos[0]);
    if (target.targetId !== targetId || target.type !== 'page' || target.url !== 'about:blank'
      || target.browserContextId !== this.browserContextId) throw new HeadlessBrowserError('protocol_error');

    this.diagnosticStage = 'target_attachment';
    this.sessionId = await this.pipe.attachFlattenedTarget(targetId);
    this.diagnosticStage = 'session_setup';
    await this.pipe.enablePage(this.sessionId);
    await this.pipe.enableAccessibility(this.sessionId);
    await this.pipe.disableScriptExecution(this.sessionId);
    await this.pipe.enableNetworkBlocklist(this.sessionId);
    await this.pipe.denyDownloads(this.browserContextId);
    this.diagnosticStage = 'fixture_navigation';
    const navigation = await this.pipe.navigateFixture(this.sessionId);
    const frameId = navigation.frameId;
    const loaderId = navigation.loaderId;
    if (!requiredString(frameId, 256) || !requiredString(loaderId, 256)
      || (Object.hasOwn(navigation, 'errorText') && navigation.errorText !== undefined)) {
      throw new HeadlessBrowserError('document_changed');
    }
    this.diagnosticStage = 'load_event';
    await this.pipe.waitForEvent('Page.loadEventFired', this.sessionId);
    this.diagnosticStage = 'document_identity';
    const identity = await this.readDocumentIdentity();
    if (identity.frameId !== frameId || identity.loaderId !== loaderId) {
      throw new HeadlessBrowserError('document_changed');
    }
    this.expectedDocument = identity;
  }

  private onProtocolEvent(event: { readonly method: string; readonly params: Readonly<JsonRecord>; readonly sessionId?: string }): void {
    if (this.closing || this.closed) return;
    function reject(stage: HeadlessDiagnosticStage, code: HeadlessBrowserErrorCode = 'protocol_error'): never {
      throw new HeadlessBrowserError(code, { stage });
    }
    const readTargetInfo = (stage: HeadlessDiagnosticStage): ReturnType<typeof validateFrameInfo> => {
      try { return validateFrameInfo(event.params.targetInfo); }
      catch { return reject(stage); }
    };
    try {
      if (event.method === 'Target.targetCreated') {
        const info = readTargetInfo('event_target_created_shape');
        if (this.startupTargetIds.has(info.targetId)) {
          if (!['about:blank', 'chrome://newtab/'].includes(info.url)) reject('startup_target_info_url_changed');
          if (info.browserContextId !== undefined) reject('startup_target_info_context_present');
          this.closingStartupTargetIds.add(info.targetId);
          return;
        }
        if (this.targetCreationInProgress) {
          if (info.type !== 'page') reject('target_creation_not_page');
          if (info.url !== 'about:blank') reject('target_creation_url_not_blank');
          if (info.browserContextId !== this.browserContextId) reject('target_creation_context_mismatch');
          this.createdTargetIds.push(info.targetId);
          if (this.createdTargetIds.length > 8) reject('target_creation_event_count');
          return;
        }
        if (this.targetId !== null) {
          if (info.targetId !== this.targetId) reject('unexpected_target_created');
          if (info.type !== 'page') reject('target_creation_not_page');
          if (!['about:blank', this.fixtureURL].includes(info.url)) reject('target_creation_url_not_blank');
          if (info.browserContextId !== this.browserContextId) reject('target_creation_context_mismatch');
          return;
        }
        if (info.type !== 'page') reject('startup_target_not_page');
        if (!['about:blank', 'chrome://newtab/'].includes(info.url)) reject('startup_target_url_not_blank');
        if (info.browserContextId !== undefined) reject('startup_target_context_present');
        this.startupTargetIds.add(info.targetId);
        this.closingStartupTargetIds.add(info.targetId);
        return;
      }
      if (event.method === 'Target.targetInfoChanged') {
        const info = readTargetInfo('event_target_info_changed_shape');
        if (this.closingStartupTargetIds.has(info.targetId)) {
          if (!['about:blank', 'chrome://newtab/'].includes(info.url)) reject('startup_target_info_url_changed');
          return;
        }
        if (this.startupTargetIds.has(info.targetId)) {
          if (!['about:blank', 'chrome://newtab/'].includes(info.url)) reject('startup_target_info_url_changed');
          if (info.browserContextId !== undefined) reject('startup_target_info_context_present');
          this.closingStartupTargetIds.add(info.targetId);
          return;
        }
        if (this.targetCreationInProgress) {
          if (info.type !== 'page') reject('target_creation_not_page');
          if (info.url !== 'about:blank') reject('target_creation_url_not_blank');
          if (info.browserContextId !== this.browserContextId) reject('target_creation_context_mismatch');
          return;
        }
        if (this.targetId === null || info.targetId !== this.targetId) reject('unexpected_target_info_changed');
        if (info.type !== 'page') reject('target_creation_not_page');
        if (!['about:blank', this.fixtureURL].includes(info.url)) reject('target_creation_url_not_blank');
        if (info.browserContextId !== this.browserContextId) reject('target_creation_context_mismatch');
        return;
      }
      if (event.method === 'Target.targetDestroyed') {
        const targetId = event.params.targetId;
        if (typeof targetId !== 'string') reject('target_destroyed_shape');
        if (this.closingStartupTargetIds.has(targetId)) return;
        if (this.closing && targetId === this.targetId) return;
        reject('unexpected_target_destroyed');
      }
      if (event.method === 'Page.frameNavigated') {
        if (event.sessionId !== this.sessionId) reject('page_frame_event_session_mismatch');
        let frame: JsonRecord;
        try { frame = requireRecord(event.params.frame); }
        catch { reject('page_frame_event_shape'); }
        if (Object.hasOwn(frame, 'parentId') || !requiredString(frame.id, 256)) reject('page_frame_event_shape');
        if (!['about:blank', this.fixtureURL].includes(String(frame.url ?? ''))) {
          reject('page_frame_event_url', 'document_changed');
        }
        return;
      }
      if (event.method === 'Network.requestWillBeSent') {
        if (event.sessionId !== this.sessionId) reject('network_event_session_mismatch');
        let request: JsonRecord;
        try { request = requireRecord(event.params.request); }
        catch { reject('network_event_shape'); }
        if (request.url !== this.fixtureURL) reject('network_event_url');
        return;
      }
      if (event.method === 'Browser.downloadWillBegin') reject('browser_download_event');
      if (event.method === 'Inspector.detached') reject('inspector_detached');
    } catch (error) {
      this.invalidate(toHeadlessError(error));
    }
  }

  private async readDocumentIdentity(): Promise<HeadlessDocumentIdentity> {
    try {
      const result = await diagnosedCommand(
        this.pipe.frameTree(this.requireSessionId(), 'document_identity'), 'document_identity', 'Page.getFrameTree',
      );
      const frameTree = requireRecord(result.frameTree);
      const frame = requireRecord(frameTree.frame);
      const frameId = frame.id;
      const loaderId = frame.loaderId;
      const url = frame.url;
      if (!requiredString(frameId, 256) || !requiredString(loaderId, 256)
        || !requiredString(url, 4_096) || url !== this.fixtureURL
        || (Object.hasOwn(frameTree, 'childFrames')
          && (!Array.isArray(frameTree.childFrames) || frameTree.childFrames.length !== 0))) {
        throw new HeadlessBrowserError('document_changed');
      }
      return { frameId, loaderId, url };
    } catch (error) {
      const diagnosed = withHeadlessDiagnostic(error, 'document_identity', 'Page.getFrameTree');
      if (diagnosed.code === 'document_changed') this.invalidate(diagnosed);
      throw diagnosed;
    }
  }

  private requireStableDocument(expected: HeadlessDocumentIdentity, actual: HeadlessDocumentIdentity): void {
    if (!sameDocument(expected, actual) || !this.expectedDocument
      || !sameDocument(this.expectedDocument, actual)) {
      const error = new HeadlessBrowserError('document_changed', {
        stage: 'document_identity', commandMethod: 'Page.getFrameTree',
      });
      this.invalidate(error);
      throw error;
    }
  }

  private nextProvenance(readStartedMonotonicMs: number, readFinishedMonotonicMs: number): HeadlessBrowserProvenance {
    if (!this.browserContextId || !this.targetId || !this.sessionId
      || !Number.isFinite(readStartedMonotonicMs) || readStartedMonotonicMs < 0
      || !Number.isFinite(readFinishedMonotonicMs) || readFinishedMonotonicMs < readStartedMonotonicMs
      || !Number.isSafeInteger(this.sequence + 1)) throw new HeadlessBrowserError('protocol_error');
    this.sequence += 1;
    return Object.freeze({
      browserSessionId: this.browserSessionId,
      browserContextId: this.browserContextId,
      targetId: this.targetId,
      pageId: this.targetId,
      sessionId: this.sessionId,
      sequence: this.sequence,
      clockDomain: 'headless-performance-monotonic-ms',
      readStartedMonotonicMs,
      readFinishedMonotonicMs,
    });
  }

  private requireSessionId(): string {
    if (!this.sessionId) throw new HeadlessBrowserError('closed');
    return this.sessionId;
  }

  private enqueue<T>(operation: () => Promise<T>): Promise<T> {
    const current = this.operationTail.then(async () => {
      if (this.fault) throw this.fault;
      if (this.closing || this.closed || !this.expectedDocument) throw new HeadlessBrowserError('closed');
      try { return await operation(); }
      catch (error) {
        if (error instanceof OwnedBrowserActionOutcomeUnknown) {
          // Preserve the uncertain action type for Main while retiring this exact
          // Chrome child; normalizing it to protocol_error would invite replay.
          this.retireNow();
          throw error;
        }
        const normalized = toHeadlessError(error);
        if (normalized.code !== 'invalid_input' && normalized.code !== 'closed') this.invalidate(normalized);
        throw normalized;
      }
    });
    this.operationTail = current.then(() => undefined, () => undefined);
    return current;
  }

  private invalidate(error: HeadlessBrowserError | HeadlessBrowserErrorCode): void {
    if (this.fault || this.closing || this.closed) return;
    this.fault = typeof error === 'string' ? new HeadlessBrowserError(error) : error;
    this.pipe.invalidate(this.fault);
    void this.close().catch(() => undefined);
  }

  private async closeOwnedBrowser(): Promise<void> {
    await this.operationTail;
    this.removeEventListener();
    if (!this.pipe.isRetired) {
      if (this.targetId) {
        try { await this.pipe.closeTarget(this.targetId); } catch { /* Closing is best effort before owned-process teardown. */ }
      }
      if (this.browserContextId && !this.pipe.isRetired) {
        try { await this.pipe.disposeBrowserContext(this.browserContextId); } catch { /* The fresh profile is removed after process exit. */ }
      }
      if (!this.pipe.isRetired) {
        try { await this.pipe.setTargetDiscovery(false); } catch { /* The owned process is still terminated below. */ }
      }
    }
    this.pipe.close();

    let exited = await waitForChildClose(this.child, CHILD_EXIT_WAIT_MS, () => this.childClosed);
    if (!exited && !this.childClosed) {
      try { this.child.kill('SIGTERM'); } catch { /* Exact owned child only. */ }
      exited = await waitForChildClose(this.child, TERM_WAIT_MS, () => this.childClosed);
    }
    if (!exited && !this.childClosed) {
      try { this.child.kill('SIGKILL'); } catch { /* Exact owned child only. */ }
      exited = await waitForChildClose(this.child, KILL_WAIT_MS, () => this.childClosed);
    }
    if (!exited || !this.childClosed) {
      throw new HeadlessBrowserError('child_exit_unconfirmed', { stage: 'cleanup' });
    }
    try { safeRemoveProfile(this.profileDirectory, this.profileParent); }
    catch { throw new HeadlessBrowserError('cleanup_failed', { stage: 'cleanup' }); }
    this.closed = true;
  }
}

/** @internal Test-only constructor; production sessions still come from launchOwnedBrowser. */
export function createOwnedBrowserSessionForTests(
  options: OwnedBrowserSessionTestHarnessOptions,
): OwnedBrowserSession {
  return OwnedBrowserSessionImpl.forTests(options);
}

export async function launchOwnedBrowser(options: OwnedBrowserLaunchOptions): Promise<OwnedBrowserSession> {
  const paths = canonicalPaths(options);
  let profileDirectory: string | null = null;
  try {
    profileDirectory = mkdtempSync(join(paths.profileParent, CHROME_PROFILE_PREFIX));
    chmodSync(profileDirectory, 0o700);
    if (realpathSync(profileDirectory) !== profileDirectory || dirname(profileDirectory) !== paths.profileParent) {
      throw new HeadlessBrowserError('invalid_input');
    }
  } catch (error) {
    if (profileDirectory !== null) {
      try { safeRemoveProfile(profileDirectory, paths.profileParent); }
      catch { throw new HeadlessBrowserError('cleanup_failed'); }
    }
    if (error instanceof HeadlessBrowserError) throw error;
    throw new HeadlessBrowserError('spawn_failed');
  }
  if (profileDirectory === null) throw new HeadlessBrowserError('spawn_failed');

  const ownedProfileDirectory = profileDirectory;

  const args = [
    '--headless',
    '--remote-debugging-pipe',
    `--user-data-dir=${ownedProfileDirectory}`,
    `--crash-dumps-dir=${join(ownedProfileDirectory, 'crashes')}`,
    '--no-startup-window',
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-background-networking',
    '--disable-sync',
    '--disable-extensions',
    '--disable-component-update',
    '--disable-default-apps',
    '--host-resolver-rules=MAP * ~NOTFOUND',
  ];
  let child: ChildProcess;
  try {
    child = spawn(paths.chrome, args, {
      cwd: ownedProfileDirectory,
      env: Object.fromEntries(['PATH', 'TMPDIR', 'LANG', 'LC_ALL'].flatMap(key => {
        const value = process.env[key];
        return typeof value === 'string' && value.length <= 16_384 ? [[key, value]] : [];
      })),
      stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'],
      windowsHide: true,
      detached: false,
    });
  } catch {
    safeRemoveProfile(ownedProfileDirectory, paths.profileParent);
    throw new HeadlessBrowserError('spawn_failed');
  }

  let childClosed = false;
  child.on('error', () => undefined);
  child.once('close', () => { childClosed = true; });
  if (!child.pid) {
    const exited = await waitForChildClose(child, CHILD_EXIT_WAIT_MS, () => childClosed);
    if (!exited || !childClosed) throw new HeadlessBrowserError('child_exit_unconfirmed');
    safeRemoveProfile(ownedProfileDirectory, paths.profileParent);
    throw new HeadlessBrowserError('spawn_failed');
  }
  const fd3 = child.stdio[3];
  const fd4 = child.stdio[4];
  if (!streamIsWritable(fd3) || !streamIsReadable(fd4)) {
    try { child.kill('SIGTERM'); } catch { /* Exact owned child only. */ }
    const exited = await waitForChildClose(child, TERM_WAIT_MS, () => childClosed);
    if (!exited || !childClosed) throw new HeadlessBrowserError('child_exit_unconfirmed');
    safeRemoveProfile(ownedProfileDirectory, paths.profileParent);
    throw new HeadlessBrowserError('spawn_failed');
  }

  let session: OwnedBrowserSessionImpl | null = null;
  let pipe: CdpPipe;
  try {
    pipe = new CdpPipe(fd4, fd3, paths.fixtureURL, {
      onRetire: () => { void session?.close().catch(() => undefined); },
    });
    session = OwnedBrowserSessionImpl.create(child, pipe, ownedProfileDirectory, paths.profileParent,
                                            paths.fixtureURL, () => childClosed);
  } catch {
    try { child.kill('SIGTERM'); } catch { /* Exact owned child only. */ }
    const exited = await waitForChildClose(child, TERM_WAIT_MS, () => childClosed);
    if (!exited || !childClosed) throw new HeadlessBrowserError('child_exit_unconfirmed');
    safeRemoveProfile(ownedProfileDirectory, paths.profileParent);
    throw new HeadlessBrowserError('spawn_failed');
  }

  try {
    await session.initialize();
    return session;
  } catch (error) {
    await session.close();
    throw toHeadlessError(error, 'spawn_failed');
  }
}
