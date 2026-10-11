import { existsSync, statSync } from 'node:fs';
import { dirname, isAbsolute, resolve } from 'node:path';

import type { KernelProcessIdentity, RowTableScope } from '@computer-use/desktop-api';

import type { Binaries } from './binaries.js';

const REFERENCE_PLANNER_BUNDLES = new Set([
  'com.lingxi.computer-user.guard-fixture',
  'com.lingxi.computer-user.mvp-fixture',
]);
const BACKGROUND_REFERENCE_PLANNER_BUNDLES = new Set([
  'com.lingxi.computer-user.guard-fixture',
]);
const REGISTERED_READ_BUNDLES = new Set([
  'com.lingxi.computer-user.guard-fixture',
  'com.lingxi.computer-user.mvp-fixture',
  'com.apple.TextEdit',
]);
const WINDOW_ID = /^window-([1-9]\d*)$/;
const REGISTERED_PROCESS_MAX = 0x7fff_ffff;
const WINDOW_NUMBER_MAX = 0xffff_ffff;
const MAX_ROW_SCOPE_LABEL_BYTES = 256;
const MAX_ROW_TABLE_ANCESTORS = 8;
const MAX_ROW_TABLE_HEADERS = 64;
const MAX_ROW_TABLE_SCOPE_JSON_BYTES = 4096;
const MAX_ROW_TABLE_SCOPE_JSON_DEPTH = 8;
const ROW_SCOPE_HEADER_WHITESPACE = /[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u;

function isRegisteredWindowId(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  const match = WINDOW_ID.exec(value);
  if (!match) return false;
  const number = Number(match[1]);
  return Number.isSafeInteger(number) && number > 0 && number <= WINDOW_NUMBER_MAX;
}

function exactKeys(value: unknown, expected: readonly string[]): value is Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) return false;
  const actual = Object.keys(value).sort();
  const sortedExpected = [...expected].sort();
  return actual.length === sortedExpected.length && actual.every((key, index) => key === sortedExpected[index]);
}

function validateScopeText(value: unknown): value is string {
  return typeof value === 'string' && value.trim().length > 0
    && Buffer.byteLength(value, 'utf8') <= MAX_ROW_SCOPE_LABEL_BYTES
    && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
}

function rowScopeHeaderKey(value: string): string {
  return value.split(ROW_SCOPE_HEADER_WHITESPACE).filter(Boolean).join(' ').toLowerCase();
}

export function validateNativeTableScope(value: unknown): RowTableScope {
  if (!exactKeys(value, ['scroll_path', 'column_headers'])) {
    throw new Error('native table scope must contain only public scroll path and headers');
  }
  const scrollPath = value['scroll_path'];
  const columnHeaders = value['column_headers'];
  if (!Array.isArray(scrollPath) || scrollPath.length < 1 || scrollPath.length > MAX_ROW_TABLE_ANCESTORS
    || !Array.isArray(columnHeaders) || columnHeaders.length < 1 || columnHeaders.length > MAX_ROW_TABLE_HEADERS) {
    throw new Error('native table scope exceeds its public row bounds');
  }
  const checkedPath = scrollPath.map((container) => {
    if (!exactKeys(container, ['role', 'label'])) {
      throw new Error('native table scope contains an invalid public scroll path');
    }
    const label = container['label'];
    if (container['role'] !== 'ax_scroll_area' || !validateScopeText(label)) {
      throw new Error('native table scope contains an invalid public scroll path');
    }
    return { role: 'ax_scroll_area' as const, label };
  });
  const checkedHeaders = columnHeaders.map((header) => {
    if (!validateScopeText(header)) throw new Error('native table scope contains an invalid public header');
    return header;
  });
  const headerKeys = checkedHeaders.map(rowScopeHeaderKey);
  if (new Set(headerKeys).size !== headerKeys.length) {
    throw new Error('native table scope contains duplicate public headers');
  }
  const scope = {
    scroll_path: checkedPath as RowTableScope['scroll_path'],
    column_headers: checkedHeaders as RowTableScope['column_headers'],
  };
  if (Buffer.byteLength(JSON.stringify(scope), 'utf8') > MAX_ROW_TABLE_SCOPE_JSON_BYTES) {
    throw new Error('native table scope exceeds its 4096-byte serialized budget');
  }
  return scope;
}

function rejectDuplicateScopeKeys(json: string): void {
  let cursor = 0;
  const whitespace = (): void => { while (/\s/u.test(json[cursor] ?? '')) cursor += 1; };
  const stringToken = (): string => {
    const start = cursor++;
    while (cursor < json.length) {
      const character = json[cursor++]!;
      if (character === '\\') cursor += 1;
      else if (character === '"') break;
    }
    return JSON.parse(json.slice(start, cursor)) as string;
  };
  const value = (depth: number): void => {
    whitespace();
    if (json[cursor] === '{') {
      if (depth >= MAX_ROW_TABLE_SCOPE_JSON_DEPTH) throw new Error('CU_SOURCE_TABLE_SCOPE is nested too deeply');
      cursor += 1;
      const keys = new Set<string>();
      whitespace();
      if (json[cursor] === '}') { cursor += 1; return; }
      while (cursor < json.length) {
        whitespace();
        const key = stringToken();
        if (keys.has(key)) throw new Error('CU_SOURCE_TABLE_SCOPE contains duplicate JSON fields');
        keys.add(key);
        whitespace(); cursor += 1; // The full JSON.parse already checked the colon.
        value(depth + 1);
        whitespace();
        if (json[cursor] === '}') { cursor += 1; return; }
        cursor += 1; // The full JSON.parse already checked the comma.
      }
    } else if (json[cursor] === '[') {
      if (depth >= MAX_ROW_TABLE_SCOPE_JSON_DEPTH) throw new Error('CU_SOURCE_TABLE_SCOPE is nested too deeply');
      cursor += 1;
      whitespace();
      if (json[cursor] === ']') { cursor += 1; return; }
      while (cursor < json.length) {
        value(depth + 1);
        whitespace();
        if (json[cursor] === ']') { cursor += 1; return; }
        cursor += 1; // The full JSON.parse already checked the comma.
      }
    } else if (json[cursor] === '"') {
      stringToken();
    } else {
      while (cursor < json.length && !/[\s,\]}]/u.test(json[cursor]!)) cursor += 1;
    }
  };
  value(0);
}

function parseTableScopeEnvironment(value: string): RowTableScope {
  if (Buffer.byteLength(value, 'utf8') > MAX_ROW_TABLE_SCOPE_JSON_BYTES) {
    throw new Error('CU_SOURCE_TABLE_SCOPE exceeds its 4096-byte request budget');
  }
  let parsed: unknown;
  try { parsed = JSON.parse(value) as unknown; }
  catch { throw new Error('CU_SOURCE_TABLE_SCOPE must be valid JSON'); }
  rejectDuplicateScopeKeys(value);
  return validateNativeTableScope(parsed);
}

export interface NativeSourceOptions {
  helperPath: string;
  bundleId: string;
  pid?: number;
  /** Optional passive guard executable; this does not enable native input. */
  guardHelperPath?: string;
  /** Set only by a trusted, explicit authorization for this process/window. */
  authorizedWindowId?: string;
  /** Trusted Main-only kernel birth stamp for a scoped window authorization. */
  authorizedProcessIdentity?: KernelProcessIdentity;
  /** Fixed Source-only read policy; it grants no Device or Host window authority. */
  sourceReadWindowId?: string;
  /** Initial bounded read scope for a registered Native Source watch. */
  tableScope?: RowTableScope;
  /** Fixed Source-only background AX target. It does not imply global focus or input authority. */
  backgroundWindowId?: string;
  /** Main-only, separately consented BackgroundAx window grant. */
  authorizedBackgroundWindowId?: string;
  /** Deterministic planner for the explicitly owned Guard/MVP fixtures; never a live model fallback. */
  referencePlanner?: boolean;
}

/** Validate before the supervisor starts any child process or allocates a session. */
export function validateNativeSource(options: NativeSourceOptions): NativeSourceOptions {
  if (typeof options.helperPath !== 'string' || !isAbsolute(options.helperPath)) {
    throw new Error('native source helper must have an absolute path');
  }
  if (!existsSync(options.helperPath) || !statSync(options.helperPath).isFile()) {
    throw new Error(`native source helper was not found at ${options.helperPath}`);
  }
  if (options.guardHelperPath !== undefined && (!isAbsolute(options.guardHelperPath)
    || !existsSync(options.guardHelperPath) || !statSync(options.guardHelperPath).isFile())) {
    throw new Error('native device guard requires an absolute executable file path');
  }
  if (typeof options.bundleId !== 'string' || !options.bundleId.trim()
    || Buffer.byteLength(options.bundleId, 'utf8') > 256 || /[\u0000-\u001f\u007f-\u009f]/.test(options.bundleId)) {
    throw new Error('native source requires a target bundle ID');
  }
  if (options.pid !== undefined && (!Number.isSafeInteger(options.pid) || options.pid <= 0 || options.pid > 0xffff_ffff)) {
    throw new Error('native source target PID must be a positive integer in the u32 range');
  }
  const bundleId = options.bundleId.trim();
  const registeredReadTarget = options.pid !== undefined && Number.isSafeInteger(options.pid)
    && options.pid > 0 && options.pid <= REGISTERED_PROCESS_MAX
    && REGISTERED_READ_BUNDLES.has(bundleId);
  if (options.authorizedWindowId !== undefined
    && (!isRegisteredWindowId(options.authorizedWindowId) || !registeredReadTarget)) {
    throw new Error('native editing requires a registered fixed process and window');
  }
  if (options.sourceReadWindowId !== undefined
    && (!isRegisteredWindowId(options.sourceReadWindowId) || !registeredReadTarget)) {
    throw new Error('Source read policy requires a registered fixed process and window');
  }
  if (options.sourceReadWindowId !== undefined && options.authorizedWindowId !== undefined
    && options.sourceReadWindowId !== options.authorizedWindowId) {
    throw new Error('Source read and authorized windows must match');
  }
  if (options.backgroundWindowId !== undefined
    && (!isRegisteredWindowId(options.backgroundWindowId) || !registeredReadTarget)) {
    throw new Error('Background AX read policy requires a registered fixed process and window');
  }
  if (options.backgroundWindowId !== undefined
    && (options.sourceReadWindowId !== undefined || options.tableScope !== undefined
      || options.authorizedWindowId !== undefined)) {
    throw new Error('Background AX read policy cannot be mixed with foreground text, table, or fixture policy');
  }
  if (options.authorizedBackgroundWindowId !== undefined
    && (!isRegisteredWindowId(options.authorizedBackgroundWindowId) || !registeredReadTarget
      || options.backgroundWindowId !== options.authorizedBackgroundWindowId
      || options.authorizedWindowId !== undefined)) {
    throw new Error('Background AX authorization requires its exact fixed Source process and window');
  }
  if (options.authorizedProcessIdentity !== undefined) {
    const identity = options.authorizedProcessIdentity;
    const authorizedWindowId = options.authorizedWindowId ?? options.authorizedBackgroundWindowId;
    if (authorizedWindowId === undefined || options.pid === undefined || !registeredReadTarget
      || !exactKeys(identity, ['pid', 'start_seconds', 'start_micros'])
      || !Number.isSafeInteger(identity['pid']) || identity['pid'] !== options.pid
      || !Number.isSafeInteger(identity['start_seconds']) || identity['start_seconds'] <= 0
      || !Number.isSafeInteger(identity['start_micros']) || identity['start_micros'] < 0 || identity['start_micros'] > 999_999) {
      throw new Error('authorized native process identity requires its fixed process/window and a valid kernel start stamp');
    }
  }
  if (options.authorizedBackgroundWindowId !== undefined && options.authorizedProcessIdentity === undefined) {
    throw new Error('Background AX authorization requires the exact kernel process start stamp');
  }
  const tableScope = options.tableScope === undefined ? undefined : validateNativeTableScope(options.tableScope);
  if (tableScope !== undefined && options.sourceReadWindowId === undefined) {
    throw new Error('native table scope requires a Source-only fixed window read policy');
  }
  if (options.referencePlanner !== undefined && typeof options.referencePlanner !== 'boolean') throw new Error('native reference planner must be a boolean');
  if (options.referencePlanner && !REFERENCE_PLANNER_BUNDLES.has(bundleId)) {
    throw new Error('the native reference planner is restricted to registered owned fixtures');
  }
  if (options.backgroundWindowId !== undefined && options.referencePlanner === true
    && !BACKGROUND_REFERENCE_PLANNER_BUNDLES.has(bundleId)) {
    throw new Error('Background AX reference planning is restricted to the registered Guard fixture');
  }
  return { ...options, bundleId, ...(tableScope === undefined ? {} : { tableScope }) };
}

/** Only Source receives the read-only window policy and initial table scope. */
export function nativeSourceReadSettings(options: NativeSourceOptions): Record<string, string> {
  if (options.backgroundWindowId !== undefined) {
    const identity = options.authorizedBackgroundWindowId !== undefined ? options.authorizedProcessIdentity : undefined;
    return {
      native_background_window_id: options.backgroundWindowId,
      ...(identity ? {
        native_target_process_start_seconds: String(identity.start_seconds),
        native_target_process_start_micros: String(identity.start_micros),
      } : {}),
    };
  }
  const readWindowId = options.sourceReadWindowId ?? options.authorizedWindowId;
  const tableScope = options.tableScope === undefined ? undefined : validateNativeTableScope(options.tableScope);
  return {
    ...(readWindowId ? { native_text_policy: 'registered_editor', native_text_window_id: readWindowId } : {}),
    ...(tableScope ? { native_table_scope: JSON.stringify(tableScope) } : {}),
  };
}

/** Native observation is an explicit configuration; the default remains the fixture. */
export function nativeSourceFromEnvironment(env: NodeJS.ProcessEnv, binaries: Binaries): NativeSourceOptions | undefined {
  const backend = env['CU_SOURCE_BACKEND'];
  if (!backend || backend === 'fixture') return undefined;
  if (backend !== 'native') throw new Error(`unsupported source backend: ${backend}`);
  const bundleId = env['CU_TARGET_BUNDLE_ID'];
  if (!bundleId?.trim()) throw new Error('CU_SOURCE_BACKEND=native requires CU_TARGET_BUNDLE_ID');
  const pid = env['CU_TARGET_PID'];
  const sourceReadWindowId = env['CU_SOURCE_READ_WINDOW_ID'];
  const backgroundWindowId = env['CU_SOURCE_BACKGROUND_WINDOW_ID'];
  const encodedTableScope = env['CU_SOURCE_TABLE_SCOPE'];
  return validateNativeSource({
    helperPath: env['CU_NATIVE_SOURCE_HELPER'] ?? resolve(dirname(binaries.source), 'cu-macos-source'),
    bundleId,
    ...(env['CU_NATIVE_REFERENCE_PLANNER'] === '1' ? { referencePlanner: true } : {}),
    ...(env['CU_NATIVE_DEVICE_GUARD'] ? { guardHelperPath: env['CU_NATIVE_DEVICE_GUARD'] } : {}),
    ...(pid === undefined ? {} : { pid: pid.trim() ? Number(pid) : Number.NaN }),
    ...(sourceReadWindowId === undefined ? {} : { sourceReadWindowId }),
    ...(backgroundWindowId === undefined ? {} : { backgroundWindowId }),
    ...(encodedTableScope === undefined ? {} : { tableScope: parseTableScopeEnvironment(encodedTableScope) }),
  });
}
