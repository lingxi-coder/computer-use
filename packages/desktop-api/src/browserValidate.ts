import Ajv2020 from 'ajv/dist/2020.js';
import type { ValidateFunction } from 'ajv';

import type {
  BrowserActionIntent,
  BrowserExecutionObservation,
} from './generated/envelope.js';
import type {
  BrowserCheckboxBinding,
  BrowserPageSnapshot,
  BrowserReadScope,
  BrowserSourceControl,
  BrowserTargetContext,
} from './generated/browser.js';
import { browserWireSchema } from './generated/browser-schema.js';
import { envelopeSchema } from './generated/schema.js';
import type { Validation } from './validate.js';

const MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER;
const MAX_SNAPSHOT_BYTES = 2 * 1024 * 1024;
const MAX_READ_SCOPE_BYTES = 8_192;
const MAX_BROWSER_OBSERVATION_BYTES = 16 * 1_024;
const MAX_BROWSER_INTENT_BYTES = 32 * 1_024;
const MAX_BROWSER_OBSERVATION_TTL_MS = 2_000;
const MAX_BROWSER_DEPENDENCIES = 16;
const MAX_BROWSER_DEPENDENCY_NAME_BYTES = 64;
const MAX_BROWSER_TEXT_BYTES = 256;
const MAX_NODES = 2_048;
const MAX_CHILDREN_PER_NODE = 2_048;
const MAX_EDGES = 8_192;
const MAX_TREE_DEPTH = 256;

const encoder = new TextEncoder();
const byteLength = (value: string): number => encoder.encode(value).byteLength;
const controls = /[\u0000-\u001f\u007f-\u009f]/;
const whitespaceOnly = /^\p{White_Space}*$/u;

function hasUnpairedSurrogate(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return true;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return true;
    }
  }
  return false;
}

function boundedString(value: string, maxBytes: number, allowEmpty = false): boolean {
  return typeof value === 'string'
    && (allowEmpty || value.length > 0)
    && byteLength(value) <= maxBytes
    && !hasUnpairedSurrogate(value)
    && !controls.test(value);
}

function validIdentifier(value: string, maxBytes: number): boolean {
  return boundedString(value, maxBytes)
    && !whitespaceOnly.test(value);
}

function validText(value: string, maxBytes: number): boolean {
  return validIdentifier(value, maxBytes);
}

function validFixtureFileUrl(value: string): boolean {
  if (!boundedString(value, 4_096)
    || !value.startsWith('file:///')
    || !value.endsWith('/fixtures/headless/readonly-page.html')
    || /[?#\\ ]/.test(value)) return false;

  const path = value.slice('file:///'.length);
  if (path.length === 0) return false;
  for (let index = 0; index < value.length; index += 1) {
    if (value[index] !== '%') continue;
    if (!/^[0-9a-fA-F]{2}$/.test(value.slice(index + 1, index + 3))) return false;
    index += 2;
  }
  return true;
}

function validContext(context: BrowserTargetContext): string | undefined {
  if (!context || typeof context !== 'object') return 'browser target context is missing';
  for (const [field, value] of [
    ['browser_session_id', context.browser_session_id],
    ['browser_context_id', context.browser_context_id],
    ['page_target_id', context.page_target_id],
    ['cdp_session_id', context.cdp_session_id],
    ['frame_id', context.frame_id],
    ['document_loader_id', context.document_loader_id],
  ] as const) {
    if (!validIdentifier(value, 256)) return `browser target context ${field} is invalid`;
  }
  if (!Number.isSafeInteger(context.session_epoch)
    || context.session_epoch <= 0
    || context.session_epoch > MAX_SAFE_INTEGER) {
    return 'browser target context session_epoch is not a positive JS-safe integer';
  }
  if (!validFixtureFileUrl(context.fixture_file_url)) {
    return 'browser target context must carry the exact local static fixture file URL';
  }
  return undefined;
}

function canonicalTargetContext(context: BrowserTargetContext): BrowserTargetContext {
  // This field order is byte-significant and mirrors BrowserTargetContext in Rust.
  return {
    browser_session_id: context.browser_session_id,
    browser_context_id: context.browser_context_id,
    page_target_id: context.page_target_id,
    cdp_session_id: context.cdp_session_id,
    frame_id: context.frame_id,
    document_loader_id: context.document_loader_id,
    fixture_file_url: context.fixture_file_url,
    session_epoch: context.session_epoch,
  };
}

/**
 * Computes the stable `tc1` identity from compact UTF-8 JSON in the Rust
 * contract's fixed field order. This is identity metadata only; it grants no
 * browser access or execution authority.
 */
export async function browserTargetContextHash(context: BrowserTargetContext): Promise<string> {
  const invalid = validContext(context);
  if (invalid) throw new Error(invalid);
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) throw new Error('Web Crypto SHA-256 is unavailable');
  const json = JSON.stringify(canonicalTargetContext(context));
  if (json === undefined) throw new Error('browser target context could not be serialized');
  const digest = await subtle.digest('SHA-256', encoder.encode(json));
  const hex = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
  return `tc1:${hex}`;
}

function buildAjv(): Ajv2020 {
  const ajv = new Ajv2020({ strict: true, allErrors: true });
  const formats = new Set<string>();
  const addFormats = (node: unknown): void => {
    if (Array.isArray(node)) node.forEach(addFormats);
    else if (node && typeof node === 'object') {
      for (const [key, value] of Object.entries(node)) {
        if (key === 'format' && typeof value === 'string') formats.add(value);
        else addFormats(value);
      }
    }
  };
  addFormats(browserWireSchema);
  addFormats(envelopeSchema);
  for (const format of formats) ajv.addFormat(format, true);
  ajv.addSchema(browserWireSchema, 'browser-v1');
  ajv.addSchema(envelopeSchema, 'desktop-v2');
  return ajv;
}

const ajv = buildAjv();
const validateSnapshotSchema = ajv.getSchema('browser-v1#/$defs/BrowserPageSnapshot') as ValidateFunction;
const validateReadScopeSchema = ajv.getSchema('browser-v1#/$defs/BrowserReadScope') as ValidateFunction;
const validateSourceControlSchema = ajv.getSchema('browser-v1#/$defs/BrowserSourceControl') as ValidateFunction;
const validateExecutionObservationSchema = ajv.getSchema('desktop-v2#/$defs/BrowserExecutionObservation') as ValidateFunction;
const validateActionIntentSchema = ajv.getSchema('desktop-v2#/$defs/BrowserActionIntent') as ValidateFunction;

function schemaErrors(validator: ValidateFunction): string[] {
  return (validator.errors ?? []).map((error) => `${error.instancePath || '/'} ${error.message ?? 'is invalid'}`);
}

function validScopeUuid(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value);
}

function canonicalReadScope(scope: BrowserReadScope): BrowserReadScope {
  return {
    scope_id: scope.scope_id,
    generation: scope.generation,
    context: canonicalTargetContext(scope.context),
    clock_domain: scope.clock_domain,
    issued_ms: scope.issued_ms,
  };
}

function canonicalSourceControl(control: BrowserSourceControl): BrowserSourceControl {
  if (control.kind === 'install') {
    return { kind: 'install', scope: canonicalReadScope(control.scope) };
  }
  return {
    kind: 'invalidate',
    scope_id: control.scope_id,
    generation: control.generation,
    reason: control.reason,
  };
}

function serializedBytes(value: object): number {
  const json = JSON.stringify(value);
  return json === undefined ? Number.POSITIVE_INFINITY : byteLength(json);
}

function validateReadScopeSemantics(scope: BrowserReadScope): string | undefined {
  if (!validScopeUuid(scope.scope_id)) return 'browser read scope_id must be a canonical UUID';
  if (!Number.isSafeInteger(scope.generation) || scope.generation <= 0 || scope.generation > MAX_SAFE_INTEGER) {
    return 'browser read scope generation must be a positive JS-safe integer';
  }
  if (!Number.isSafeInteger(scope.issued_ms) || scope.issued_ms < 0 || scope.issued_ms > MAX_SAFE_INTEGER) {
    return 'browser read scope issued_ms is not a JS-safe integer';
  }
  if (!validText(scope.clock_domain, 128)) return 'browser read scope clock_domain is empty or unbounded';
  const contextError = validContext(scope.context);
  if (contextError) return contextError;
  // The context hash is a computed property; the scope wire shape stores no hash.
  if (serializedBytes(canonicalReadScope(scope)) > MAX_READ_SCOPE_BYTES) {
    return 'browser read scope exceeds its serialized byte bound';
  }
  return undefined;
}

/** Validates the host-installed observation scope without granting execution authority. */
export function validateBrowserReadScope(value: unknown): Validation<BrowserReadScope> {
  if (!validateReadScopeSchema(value)) return { ok: false, errors: schemaErrors(validateReadScopeSchema) };
  const scope = value as BrowserReadScope;
  const semanticError = validateReadScopeSemantics(scope);
  return semanticError ? { ok: false, errors: [semanticError] } : { ok: true, value: scope };
}

/** Validates the closed host-to-source lifecycle message for one read scope. */
export function validateBrowserSourceControl(value: unknown): Validation<BrowserSourceControl> {
  if (!validateSourceControlSchema(value)) {
    return { ok: false, errors: schemaErrors(validateSourceControlSchema) };
  }
  const control = value as BrowserSourceControl;
  if (control.kind === 'install') {
    const scopeValidation = validateBrowserReadScope(control.scope);
    if (!scopeValidation.ok) return scopeValidation;
  } else {
    if (!validScopeUuid(control.scope_id)) {
      return { ok: false, errors: ['browser source invalidation scope_id must be a canonical UUID'] };
    }
    if (!Number.isSafeInteger(control.generation)
      || control.generation <= 0
      || control.generation > MAX_SAFE_INTEGER) {
      return { ok: false, errors: ['browser source invalidation generation must be positive and JS-safe'] };
    }
  }
  if (serializedBytes(canonicalSourceControl(control)) > MAX_READ_SCOPE_BYTES) {
    return { ok: false, errors: ['browser source control exceeds its serialized byte bound'] };
  }
  return { ok: true, value: control };
}

function canonicalCheckboxBinding(binding: BrowserCheckboxBinding): BrowserCheckboxBinding {
  return {
    snapshot_id: binding.snapshot_id,
    source_sequence: binding.source_sequence,
    observed_ms: binding.observed_ms,
    read_finished_ms: binding.read_finished_ms,
    clock_domain: binding.clock_domain,
    context: canonicalTargetContext(binding.context),
    target_context_hash: binding.target_context_hash,
    ax_node_id: binding.ax_node_id,
    backend_dom_node_id: binding.backend_dom_node_id,
    role: binding.role,
    name: binding.name,
    disabled: binding.disabled,
    checked: binding.checked,
  };
}

function canonicalReadScopeJson(scope: BrowserReadScope): string {
  return JSON.stringify(canonicalReadScope(scope))!;
}

function compareUtf8(left: string, right: string): number {
  const leftBytes = encoder.encode(left);
  const rightBytes = encoder.encode(right);
  const limit = Math.min(leftBytes.length, rightBytes.length);
  for (let index = 0; index < limit; index += 1) {
    if (leftBytes[index] !== rightBytes[index]) return leftBytes[index]! - rightBytes[index]!;
  }
  return leftBytes.length - rightBytes.length;
}

function canonicalDependenciesJson(dependencies: Readonly<Record<string, number | undefined>>): string {
  // Rust stores this map in BTreeMap order. Build its JSON directly because
  // JSON.stringify reorders integer-like property names numerically.
  const entries = Object.entries(dependencies).sort(([left], [right]) => compareUtf8(left, right));
  return `{${entries.map(([name, revision]) => `${JSON.stringify(name)}:${revision}`).join(',')}}`;
}

function canonicalExecutionObservationJson(observation: BrowserExecutionObservation): string {
  const binding = canonicalCheckboxBinding(observation.binding);
  const surface = { id: observation.surface.id, epoch: observation.surface.epoch };
  const bindingJson = JSON.stringify(binding)!;
  return `{"observation_id":${JSON.stringify(observation.observation_id)}`
    + `,"surface":${JSON.stringify(surface)}`
    + `,"scope":${canonicalReadScopeJson(observation.scope)}`
    + `,"environment_revision":${observation.environment_revision}`
    + `,"binding_ref":${JSON.stringify(observation.binding_ref)}`
    + `,"binding":${bindingJson}`
    + `,"valid_until_ms":${observation.valid_until_ms}`
    + `,"freshness":${JSON.stringify(observation.freshness)}`
    + `,"dependencies":${canonicalDependenciesJson(observation.dependencies)}}`;
}

function canonicalActionIntentJson(intent: BrowserActionIntent): string {
  const permit = { id: intent.permit.id, generation: intent.permit.generation };
  const action = { type: intent.action.type, checked: intent.action.checked };
  return `{"intent_id":${JSON.stringify(intent.intent_id)}`
    + `,"attempt_id":${JSON.stringify(intent.attempt_id)}`
    + `,"task_id":${JSON.stringify(intent.task_id)}`
    + `,"task_revision":${intent.task_revision}`
    + `,"root_epoch":${intent.root_epoch}`
    + `,"session_epoch":${intent.session_epoch}`
    + `,"permit":${JSON.stringify(permit)}`
    + `,"evidence":${canonicalExecutionObservationJson(intent.evidence)}`
    + `,"latest_start_ms":${intent.latest_start_ms}`
    + `,"duration_ms":${intent.duration_ms}`
    + `,"action":${JSON.stringify(action)}}`;
}

function validCheckboxBindingScalars(binding: BrowserCheckboxBinding): string | undefined {
  if (!validIdentifier(binding.snapshot_id, 256)
    || !Number.isSafeInteger(binding.source_sequence)
    || binding.source_sequence <= 0
    || binding.source_sequence > MAX_SAFE_INTEGER
    || !Number.isSafeInteger(binding.observed_ms)
    || !Number.isSafeInteger(binding.read_finished_ms)
    || binding.observed_ms < 0
    || binding.read_finished_ms < binding.observed_ms
    || binding.read_finished_ms > MAX_SAFE_INTEGER
    || !validText(binding.clock_domain, 128)
    || !/^tc1:[0-9a-f]{64}$/.test(binding.target_context_hash)
    || !validIdentifier(binding.ax_node_id, 256)
    || !Number.isInteger(binding.backend_dom_node_id)
    || binding.backend_dom_node_id <= 0
    || binding.backend_dom_node_id > 2_147_483_647
    || binding.role !== 'checkbox'
    || !validText(binding.name, 512)
    || binding.disabled !== 'false'
    || typeof binding.checked !== 'boolean') {
    return 'browser checkbox binding is invalid or lacks observed enabled state';
  }
  const contextError = validContext(binding.context);
  if (contextError) return contextError;
  return undefined;
}

function contextsEqual(left: BrowserTargetContext, right: BrowserTargetContext): boolean {
  return left.browser_session_id === right.browser_session_id
    && left.browser_context_id === right.browser_context_id
    && left.page_target_id === right.page_target_id
    && left.cdp_session_id === right.cdp_session_id
    && left.frame_id === right.frame_id
    && left.document_loader_id === right.document_loader_id
    && left.fixture_file_url === right.fixture_file_url
    && left.session_epoch === right.session_epoch;
}

/**
 * Validates Environment-issued browser execution evidence. A valid result is
 * still evidence only; it does not issue a permit or enable browser input.
 */
export async function validateBrowserExecutionObservation(
  value: unknown,
): Promise<Validation<BrowserExecutionObservation>> {
  if (!validateExecutionObservationSchema(value)) {
    return { ok: false, errors: schemaErrors(validateExecutionObservationSchema) };
  }
  const observation = value as BrowserExecutionObservation;
  const scopeValidation = validateBrowserReadScope(observation.scope);
  if (!scopeValidation.ok) return { ok: false, errors: scopeValidation.errors };
  const bindingError = validCheckboxBindingScalars(observation.binding);
  if (bindingError) return { ok: false, errors: [bindingError] };
  if (!validIdentifier(observation.observation_id, MAX_BROWSER_TEXT_BYTES)
    || !Number.isSafeInteger(observation.environment_revision)
    || observation.environment_revision <= 0
    || observation.environment_revision > MAX_SAFE_INTEGER
    || !validIdentifier(observation.binding_ref, 256)
    || observation.surface.id !== observation.scope.context.page_target_id
    || observation.surface.epoch !== observation.scope.context.session_epoch
    || observation.observation_id !== observation.binding.snapshot_id
    || !contextsEqual(observation.binding.context, observation.scope.context)
    || observation.binding.clock_domain !== observation.scope.clock_domain
    || observation.binding.observed_ms < observation.scope.issued_ms) {
    return { ok: false, errors: ['browser execution observation differs from its installed scope or binding'] };
  }
  if (observation.binding.role !== 'checkbox'
    || observation.binding.name !== 'Background option'
    || observation.binding.disabled !== 'false') {
    return { ok: false, errors: ['browser execution observation requires the fixed enabled checkbox'] };
  }
  const maximumValidUntil = observation.binding.observed_ms + MAX_BROWSER_OBSERVATION_TTL_MS;
  if (!Number.isSafeInteger(observation.valid_until_ms)
    || observation.valid_until_ms < observation.binding.read_finished_ms
    || observation.valid_until_ms > maximumValidUntil) {
    return { ok: false, errors: ['browser observation validity exceeds its read interval or two-second bound'] };
  }
  if (observation.freshness !== 'verified') {
    return { ok: false, errors: ['browser execution requires verified source freshness'] };
  }
  const dependencyEntries = Object.entries(observation.dependencies);
  if (dependencyEntries.length > MAX_BROWSER_DEPENDENCIES
    || dependencyEntries.some(([name, revision]) => !validText(name, MAX_BROWSER_DEPENDENCY_NAME_BYTES)
      || typeof revision !== 'number'
      || !Number.isSafeInteger(revision)
      || revision < 0
      || revision > MAX_SAFE_INTEGER)) {
    return { ok: false, errors: ['browser execution dependencies exceed their bounds'] };
  }

  let expectedHash: string;
  try {
    expectedHash = await browserTargetContextHash(observation.scope.context);
  } catch (error) {
    return { ok: false, errors: [error instanceof Error ? error.message : 'browser target context hash could not be computed'] };
  }
  if (observation.binding.target_context_hash !== expectedHash) {
    return { ok: false, errors: ['browser execution binding context hash does not match its installed scope'] };
  }
  if (byteLength(canonicalExecutionObservationJson(observation)) > MAX_BROWSER_OBSERVATION_BYTES) {
    return { ok: false, errors: ['browser execution observation exceeds 16 KiB'] };
  }
  return { ok: true, value: observation };
}

/** Validates a bounded checkbox action against its exact live browser evidence. */
export async function validateBrowserActionIntent(value: unknown): Promise<Validation<BrowserActionIntent>> {
  if (!validateActionIntentSchema(value)) {
    return { ok: false, errors: schemaErrors(validateActionIntentSchema) };
  }
  const intent = value as BrowserActionIntent;
  const evidenceValidation = await validateBrowserExecutionObservation(intent.evidence);
  if (!evidenceValidation.ok) return { ok: false, errors: evidenceValidation.errors };
  if (![intent.intent_id, intent.attempt_id, intent.task_id, intent.permit.id].every((id) =>
    validIdentifier(id, MAX_BROWSER_TEXT_BYTES))) {
    return { ok: false, errors: ['browser intent identifiers are empty or unbounded'] };
  }
  if (![intent.task_revision, intent.root_epoch, intent.session_epoch, intent.permit.generation]
    .every((revision) => Number.isSafeInteger(revision) && revision > 0 && revision <= MAX_SAFE_INTEGER)
    || intent.session_epoch !== intent.evidence.scope.context.session_epoch
    || !Number.isSafeInteger(intent.latest_start_ms)
    || intent.latest_start_ms < 0
    || intent.latest_start_ms > MAX_SAFE_INTEGER
    || !Number.isSafeInteger(intent.duration_ms)
    || intent.duration_ms <= 0
    || intent.duration_ms > MAX_SAFE_INTEGER) {
    return { ok: false, errors: ['browser intent metadata must be positive, JS-safe, and match its session'] };
  }
  if (intent.latest_start_ms > MAX_SAFE_INTEGER - intent.duration_ms) {
    return { ok: false, errors: ['browser intent end exceeds the JS-safe time range'] };
  }
  const actionEnd = intent.latest_start_ms + intent.duration_ms;
  if (intent.latest_start_ms < intent.evidence.binding.read_finished_ms
    || actionEnd > intent.evidence.valid_until_ms) {
    return { ok: false, errors: ['browser action window must fit after its evidence read and before expiry'] };
  }
  if (intent.action.checked === intent.evidence.binding.checked) {
    return { ok: false, errors: ['browser checkbox action must change the observed value'] };
  }
  if (byteLength(canonicalActionIntentJson(intent)) > MAX_BROWSER_INTENT_BYTES) {
    return { ok: false, errors: ['browser action intent exceeds 32 KiB'] };
  }
  return { ok: true, value: intent };
}

function canonicalSnapshotJson(value: BrowserPageSnapshot): string {
  // Rebuild every object in serde struct order. Optional Rust fields serialize
  // as null when absent, so normalize the generated TS optionals accordingly.
  const context = canonicalTargetContext(value.context);
  const nodes = value.nodes.map((node) => ({
    ax_node_id: node.ax_node_id,
    backend_dom_node_id: node.backend_dom_node_id ?? null,
    parent_ax_node_id: node.parent_ax_node_id ?? null,
    child_ax_node_ids: node.child_ax_node_ids,
    ignored: node.ignored,
    role: node.role ?? null,
    name: node.name ?? null,
    disabled: node.disabled,
    checked: node.checked,
  }));
  return JSON.stringify({
    snapshot_id: value.snapshot_id,
    source_sequence: value.source_sequence,
    observed_ms: value.observed_ms,
    read_finished_ms: value.read_finished_ms,
    clock_domain: value.clock_domain,
    context,
    target_context_hash: value.target_context_hash,
    root_ax_node_id: value.root_ax_node_id,
    nodes,
  })!;
}

function validateSnapshotSemantics(snapshot: BrowserPageSnapshot): string | undefined {
  if (!validIdentifier(snapshot.snapshot_id, 256)) return 'browser page snapshot_id is empty or unbounded';
  if (!Number.isSafeInteger(snapshot.source_sequence)
    || snapshot.source_sequence <= 0
    || snapshot.source_sequence > MAX_SAFE_INTEGER) {
    return 'browser page source_sequence is not a positive JS-safe integer';
  }
  if (!Number.isSafeInteger(snapshot.observed_ms)
    || !Number.isSafeInteger(snapshot.read_finished_ms)
    || snapshot.observed_ms < 0
    || snapshot.read_finished_ms < snapshot.observed_ms
    || snapshot.read_finished_ms > MAX_SAFE_INTEGER) {
    return 'browser page read times are invalid JS-safe same-domain bounds';
  }
  if (!validText(snapshot.clock_domain, 128)) return 'browser page clock domain is empty or unbounded';

  const contextError = validContext(snapshot.context);
  if (contextError) return contextError;
  if (!/^tc1:[0-9a-f]{64}$/.test(snapshot.target_context_hash)) {
    return 'browser page target-context hash must be lowercase tc1 SHA-256';
  }

  if (!validIdentifier(snapshot.root_ax_node_id, 256)) return 'browser page root AX node id is empty or unbounded';
  if (snapshot.nodes.length === 0 || snapshot.nodes.length > MAX_NODES) {
    return 'browser page node count is empty or exceeds its bound';
  }

  const indices = new Map<string, number>();
  let edgeCount = 0;
  for (let index = 0; index < snapshot.nodes.length; index += 1) {
    const node = snapshot.nodes[index]!;
    if (!validIdentifier(node.ax_node_id, 256)) return 'browser page AX node id is empty or unbounded';
    if (indices.has(node.ax_node_id)) return 'browser page contains duplicate AX node ids';
    indices.set(node.ax_node_id, index);
    if (node.backend_dom_node_id != null
      && (!Number.isInteger(node.backend_dom_node_id) || node.backend_dom_node_id <= 0 || node.backend_dom_node_id > 2_147_483_647)) {
      return 'browser page backend DOM node ids must be positive i32 values';
    }
    if (node.parent_ax_node_id != null && !validIdentifier(node.parent_ax_node_id, 256)) {
      return 'browser page parent AX node id is empty or unbounded';
    }
    if (node.child_ax_node_ids.length > MAX_CHILDREN_PER_NODE) return 'browser page child count exceeds its per-node bound';
    edgeCount += node.child_ax_node_ids.length;
    if (edgeCount > MAX_EDGES) return 'browser page aggregate edge count exceeds its bound';
    if (node.role != null && !boundedString(node.role, 128, true)) return 'browser page role is unbounded or contains control characters';
    if (node.name != null && !boundedString(node.name, 512, true)) return 'browser page name is unbounded or contains control characters';

    const childIds = new Set<string>();
    for (const childId of node.child_ax_node_ids) {
      if (!validIdentifier(childId, 256)) return 'browser page child AX node id is empty or unbounded';
      if (childIds.has(childId)) return 'browser page parent contains a duplicate ordered child id';
      childIds.add(childId);
    }
  }

  const rootIndex = indices.get(snapshot.root_ax_node_id);
  if (rootIndex === undefined) return 'browser page root AX node is missing';
  const rootCount = snapshot.nodes.reduce((count, node) => count + (node.parent_ax_node_id == null ? 1 : 0), 0);
  if (rootCount !== 1
    || snapshot.nodes[rootIndex]!.parent_ax_node_id != null
    || snapshot.nodes.some((node, index) => index !== rootIndex && node.parent_ax_node_id == null)) {
    return 'browser page tree must have exactly the declared root';
  }

  for (const node of snapshot.nodes) {
    for (const childId of node.child_ax_node_ids) {
      const childIndex = indices.get(childId);
      if (childIndex === undefined) return 'browser page contains a dangling child AX node id';
      if (snapshot.nodes[childIndex]!.parent_ax_node_id !== node.ax_node_id) {
        return 'browser page child and actual parent references disagree';
      }
    }
    if (node.parent_ax_node_id != null) {
      const parentIndex = indices.get(node.parent_ax_node_id);
      if (parentIndex === undefined) return 'browser page contains a dangling parent AX node id';
      const references = snapshot.nodes[parentIndex]!.child_ax_node_ids.filter((childId) => childId === node.ax_node_id).length;
      if (references !== 1) return 'browser page actual parent omits or duplicates its child';
    }
  }

  const colors = new Uint8Array(snapshot.nodes.length);
  for (let start = 0; start < snapshot.nodes.length; start += 1) {
    if (colors[start] !== 0) continue;
    colors[start] = 1;
    const stack: Array<{ index: number; nextChild: number; depth: number }> = [{ index: start, nextChild: 0, depth: 1 }];
    while (stack.length > 0) {
      const current = stack[stack.length - 1]!;
      const node = snapshot.nodes[current.index]!;
      if (current.nextChild >= node.child_ax_node_ids.length) {
        colors[current.index] = 2;
        stack.pop();
        continue;
      }
      const childId = node.child_ax_node_ids[current.nextChild]!;
      current.nextChild += 1;
      const childIndex = indices.get(childId);
      if (childIndex === undefined) return 'browser page contains a dangling child AX node id';
      if (colors[childIndex] === 1) return 'browser page tree contains a cycle';
      if (colors[childIndex] === 2) continue;
      const childDepth = current.depth + 1;
      if (childDepth > MAX_TREE_DEPTH) return 'browser page tree exceeds its depth bound';
      colors[childIndex] = 1;
      stack.push({ index: childIndex, nextChild: 0, depth: childDepth });
    }
  }

  const visited = new Uint8Array(snapshot.nodes.length);
  const pending = [rootIndex];
  visited[rootIndex] = 1;
  while (pending.length > 0) {
    const index = pending.pop()!;
    for (const childId of snapshot.nodes[index]!.child_ax_node_ids) {
      const childIndex = indices.get(childId);
      if (childIndex === undefined) return 'browser page contains a dangling child AX node id';
      if (visited[childIndex] === 0) {
        visited[childIndex] = 1;
        pending.push(childIndex);
      }
    }
  }
  if (visited.some((seen) => seen === 0)) return 'browser page contains nodes outside the declared root tree';

  if (byteLength(canonicalSnapshotJson(snapshot)) > MAX_SNAPSHOT_BYTES) {
    return 'browser page snapshot exceeds its serialized byte bound';
  }
  return undefined;
}

/**
 * Validates a complete browser page read against the generated Rust schema and
 * the Rust contract's byte, identity, and tree rules. Success is read-only
 * evidence and never grants a permit or execution authority.
 */
export async function validateBrowserPageSnapshot(value: unknown): Promise<Validation<BrowserPageSnapshot>> {
  if (!validateSnapshotSchema(value)) {
    return { ok: false, errors: schemaErrors(validateSnapshotSchema) };
  }
  const snapshot = value as BrowserPageSnapshot;
  const semanticError = validateSnapshotSemantics(snapshot);
  if (semanticError) return { ok: false, errors: [semanticError] };

  let expectedHash: string;
  try {
    expectedHash = await browserTargetContextHash(snapshot.context);
  } catch (error) {
    return { ok: false, errors: [error instanceof Error ? error.message : 'browser target context hash could not be computed'] };
  }
  if (snapshot.target_context_hash !== expectedHash) {
    return { ok: false, errors: ['browser page target-context hash does not match its fixed context'] };
  }
  return { ok: true, value: snapshot };
}
