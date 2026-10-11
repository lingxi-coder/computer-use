import { createHash } from 'node:crypto';
import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readSync,
  realpathSync,
} from 'node:fs';
import { basename, dirname, isAbsolute, resolve } from 'node:path';

import type {
  AllowedControlScope,
  FrozenTaskCatalogEntry,
  FrozenTaskCatalogSnapshot,
  FrozenTaskProfile,
  FrozenTaskSelection,
} from '@computer-use/desktop-api';

const GOALS_FILE_NAME = 'task-goals.json';
const MANIFEST_FILE_NAME = 'goal-catalog-manifest.json';
const MAX_GOALS_BYTES = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES = 8 * 1024;
const MAX_OBJECTIVE_BYTES = 32_768;
const SHA256 = /^[a-f0-9]{64}$/;
const CONTROL_TEXT = /[\u0000-\u001f\u007f-\u009f]/;
const OBJECTIVE_CONTROLS = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/;

const FIXTURE_TASK_IDS = Object.freeze([
  ...Array.from({ length: 6 }, (_, index) => `SET-${String(index + 1).padStart(2, '0')}`),
  ...Array.from({ length: 7 }, (_, index) => `FORM-${String(index + 1).padStart(2, '0')}`),
  ...Array.from({ length: 4 }, (_, index) => `LIST-${String(index + 1).padStart(2, '0')}`),
  ...Array.from({ length: 3 }, (_, index) => `MODAL-${String(index + 1).padStart(2, '0')}`),
]);
const TEXTEDIT_TASK_IDS = Object.freeze(
  Array.from({ length: 20 }, (_, index) => `TEXTEDIT-${String(index + 1).padStart(2, '0')}`),
);
const EXPECTED_IDENTITIES: readonly ParsedIdentity[] = Object.freeze([
  ...FIXTURE_TASK_IDS.flatMap((taskId) => [1, 2, 3, 4, 5].map((seed) => ({
    suite: 'fixture' as const,
    task_id: taskId,
    seed,
  }))),
  ...TEXTEDIT_TASK_IDS.map((taskId) => ({ suite: 'textedit' as const, task_id: taskId, seed: 1 })),
]);

const ENV_NAMES = Object.freeze({
  goalsPath: 'CU_FROZEN_TASK_GOALS_PATH',
  manifestPath: 'CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH',
  goalsSha256: 'CU_FROZEN_TASK_GOALS_SHA256',
  manifestSha256: 'CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256',
});

export type FrozenTaskCatalogUnavailableReason =
  | 'configuration_incomplete'
  | 'configuration_invalid'
  | 'catalog_files_unavailable'
  | 'catalog_files_unsafe'
  | 'catalog_hash_mismatch'
  | 'catalog_manifest_invalid'
  | 'catalog_goals_invalid';

export type FrozenTaskScopeFamily = 'control' | 'form' | 'list' | 'modal' | 'textedit';

export type FrozenTaskUnsupportedReason =
  | 'list_scope_not_supported'
  | 'modal_scope_not_supported'
  | 'textedit_scope_not_supported';

export interface FrozenTaskCanonicalGoal {
  readonly objective: string;
  readonly allowed_scope: AllowedControlScope;
}

export type FrozenTaskResolution =
  | {
    readonly status: 'available';
    readonly goal: FrozenTaskCanonicalGoal;
    readonly profile: FrozenTaskProfile;
    readonly profileMetadata: FrozenTaskCatalogEntry;
  }
  | {
    readonly status: 'unsupported';
    readonly reason: FrozenTaskUnsupportedReason;
    readonly profileMetadata: FrozenTaskCatalogEntry;
  };

export interface FrozenTaskCatalog {
  status(): FrozenTaskCatalogSnapshot['status'];
  list(): readonly FrozenTaskCatalogEntry[];
  snapshot(): FrozenTaskCatalogSnapshot;
  resolve(selection: FrozenTaskSelection): FrozenTaskResolution | null;
}

interface CatalogConfig {
  readonly goalsPath?: string;
  readonly manifestPath?: string;
  readonly goalsSha256?: string;
  readonly manifestSha256?: string;
}

interface ParsedIdentity {
  readonly suite: 'fixture' | 'textedit';
  readonly task_id: string;
  readonly seed: number;
}

interface RawCatalogRecord extends ParsedIdentity {
  readonly selector: string;
  readonly goal: {
    readonly objective: string;
    readonly allowed_scope: unknown;
  };
}

interface CatalogManifest {
  readonly format_version: 1;
  readonly task_profile_version: 1;
  readonly source_freeze_sha256: string;
  readonly source_freeze_content_sha256: string;
  readonly source_request_sha256: string;
  readonly task_goals_sha256: string;
  readonly identities_sha256: string;
  readonly identity_count: 120;
  readonly denominators: {
    readonly fixture_runs: 100;
    readonly textedit_runs: 20;
    readonly total_records: 120;
  };
  readonly selector_scheme: 'sha256/domain-separated-v1';
}

interface StoredRecord {
  readonly metadata: FrozenTaskCatalogEntry;
  readonly goal?: FrozenTaskCanonicalGoal;
  readonly profile?: FrozenTaskProfile;
  readonly unsupportedReason?: FrozenTaskUnsupportedReason;
}

interface DirectoryIdentity {
  readonly dev: bigint;
  readonly ino: bigint;
  readonly uid: bigint;
  readonly mode: bigint;
}

class CatalogFailure extends Error {
  constructor(readonly reason: FrozenTaskCatalogUnavailableReason) {
    super(reason);
  }
}

function fail(reason: FrozenTaskCatalogUnavailableReason): never {
  throw new CatalogFailure(reason);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && Object.getPrototypeOf(value) === Object.prototype;
}

function exactKeys(
  value: unknown,
  expected: readonly string[],
  reason: FrozenTaskCatalogUnavailableReason,
): asserts value is Record<string, unknown> {
  if (!isRecord(value)) fail(reason);
  const keys = Object.keys(value).sort();
  const sorted = [...expected].sort();
  if (keys.length !== sorted.length || keys.some((key, index) => key !== sorted[index])) fail(reason);
}

function nonemptyText(
  value: unknown,
  maxBytes: number,
  reason: FrozenTaskCatalogUnavailableReason,
): asserts value is string {
  if (typeof value !== 'string' || value.trim() === '' || Buffer.byteLength(value, 'utf8') > maxBytes
      || CONTROL_TEXT.test(value)) {
    fail(reason);
  }
}

function objectiveText(value: unknown): asserts value is string {
  if (typeof value !== 'string' || value.trim() === '' || Buffer.byteLength(value, 'utf8') > MAX_OBJECTIVE_BYTES
      || OBJECTIVE_CONTROLS.test(value)) {
    fail('catalog_goals_invalid');
  }
}

function requireSha256(value: unknown, reason: FrozenTaskCatalogUnavailableReason): asserts value is string {
  if (typeof value !== 'string' || !SHA256.test(value)) fail(reason);
}

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (isRecord(value)) {
    return `{${Object.keys(value).sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`;
  }
  const serialized = JSON.stringify(value);
  if (serialized === undefined) fail('catalog_goals_invalid');
  return serialized;
}

function stableJson(value: unknown): string {
  return `${canonicalJson(value)}\n`;
}

function sha256(value: Uint8Array | string): string {
  return createHash('sha256').update(value).digest('hex');
}

function parseCanonicalJson(
  bytes: Buffer,
  reason: FrozenTaskCatalogUnavailableReason,
): unknown {
  let value: unknown;
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    value = JSON.parse(text) as unknown;
  } catch {
    fail(reason);
  }
  if (!Buffer.from(stableJson(value), 'utf8').equals(bytes)) fail(reason);
  return value;
}

function currentUid(): bigint {
  const uid = process.getuid?.();
  if (!Number.isSafeInteger(uid) || uid! < 0) fail('catalog_files_unsafe');
  return BigInt(uid!);
}

function privateDirectoryIdentity(path: string, owner: bigint): DirectoryIdentity {
  try {
    if (realpathSync(path) !== path) fail('catalog_files_unsafe');
    const stat = lstatSync(path, { bigint: true });
    if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== owner
        || (stat.mode & 0o7777n) !== 0o700n) {
      fail('catalog_files_unsafe');
    }
    return { dev: stat.dev, ino: stat.ino, uid: stat.uid, mode: stat.mode & 0o7777n };
  } catch (error) {
    if (error instanceof CatalogFailure) throw error;
    fail('catalog_files_unsafe');
  }
}

function sameDirectory(a: DirectoryIdentity, b: DirectoryIdentity): boolean {
  return a.dev === b.dev && a.ino === b.ino && a.uid === b.uid && a.mode === b.mode;
}

interface BigIntFileIdentityStat {
  readonly dev: bigint;
  readonly ino: bigint;
  readonly uid: bigint;
  readonly nlink: bigint;
  readonly mode: bigint;
  readonly size: bigint;
  readonly mtimeNs: bigint;
  readonly ctimeNs: bigint;
}

function sameFileIdentity(a: BigIntFileIdentityStat, b: BigIntFileIdentityStat): boolean {
  return a.dev === b.dev && a.ino === b.ino && a.uid === b.uid && a.nlink === b.nlink
    && a.mode === b.mode && a.size === b.size && a.mtimeNs === b.mtimeNs && a.ctimeNs === b.ctimeNs;
}

function readPrivateStableFile(
  path: string,
  expectedName: string,
  expectedDirectory: DirectoryIdentity | undefined,
  maxBytes: number,
  oversizeReason: FrozenTaskCatalogUnavailableReason,
  owner: bigint,
): { bytes: Buffer; directory: DirectoryIdentity } {
  const parent = dirname(path);
  let fd: number | undefined;
  try {
    if (!isAbsolute(path) || resolve(path) !== path || basename(path) !== expectedName
        || dirname(resolve(path)) !== parent) {
      fail('configuration_invalid');
    }
    const beforeDirectory = privateDirectoryIdentity(parent, owner);
    if (expectedDirectory && !sameDirectory(expectedDirectory, beforeDirectory)) fail('catalog_files_unsafe');
    const before = lstatSync(path, { bigint: true });
    if (!before.isFile() || before.isSymbolicLink() || before.uid !== owner || before.nlink !== 1n
        || (before.mode & 0o7777n) !== 0o600n || before.size <= 0n || before.size > BigInt(maxBytes)) {
      fail(before.size > BigInt(maxBytes) ? oversizeReason : 'catalog_files_unsafe');
    }
    if (realpathSync(path) !== path) fail('catalog_files_unsafe');

    const noFollow = constants.O_NOFOLLOW;
    if (typeof noFollow !== 'number') fail('catalog_files_unsafe');
    fd = openSync(path, constants.O_RDONLY | noFollow);
    const opened = fstatSync(fd, { bigint: true });
    if (!opened.isFile() || !sameFileIdentity(before, opened) || opened.uid !== owner || opened.nlink !== 1n
        || (opened.mode & 0o7777n) !== 0o600n || opened.size <= 0n || opened.size > BigInt(maxBytes)) {
      fail('catalog_files_unsafe');
    }

    const size = Number(opened.size);
    if (!Number.isSafeInteger(size)) fail('catalog_files_unsafe');
    const bytes = Buffer.alloc(size);
    let offset = 0;
    while (offset < size) {
      const count = readSync(fd, bytes, offset, size - offset, offset);
      if (count <= 0) fail('catalog_files_unsafe');
      offset += count;
    }
    const afterFd = fstatSync(fd, { bigint: true });
    const afterPath = lstatSync(path, { bigint: true });
    const afterDirectory = privateDirectoryIdentity(parent, owner);
    if (offset !== size || !sameFileIdentity(opened, afterFd) || !sameFileIdentity(opened, afterPath)
        || !sameDirectory(beforeDirectory, afterDirectory) || realpathSync(path) !== path) {
      fail('catalog_files_unsafe');
    }
    return { bytes, directory: beforeDirectory };
  } catch (error) {
    if (error instanceof CatalogFailure) throw error;
    return fail(isMissing(error) ? 'catalog_files_unavailable' : 'catalog_files_unsafe');
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

function isMissing(error: unknown): boolean {
  return error !== null && typeof error === 'object' && 'code' in error
    && ['ENOENT', 'ENOTDIR'].includes(String((error as { code?: unknown }).code));
}

function exactExpectedConfig(env: NodeJS.ProcessEnv): CatalogConfig | null {
  const configured = Object.values(ENV_NAMES).map((key) => env[key]);
  if (configured.every((value) => value === undefined)) return null;
  if (configured.some((value) => value === undefined)) fail('configuration_incomplete');
  const [goalsPath, manifestPath, goalsSha256, manifestSha256] = configured;
  if ([goalsPath, manifestPath, goalsSha256, manifestSha256].some((value) => typeof value !== 'string' || value.length === 0)) {
    fail('configuration_invalid');
  }
  if (!SHA256.test(goalsSha256!) || !SHA256.test(manifestSha256!)) fail('configuration_invalid');
  if (!isAbsolute(goalsPath!) || !isAbsolute(manifestPath!)
      || resolve(goalsPath!) !== goalsPath || resolve(manifestPath!) !== manifestPath
      || basename(goalsPath!) !== GOALS_FILE_NAME || basename(manifestPath!) !== MANIFEST_FILE_NAME
      || dirname(goalsPath!) !== dirname(manifestPath!)) {
    fail('configuration_invalid');
  }
  return { goalsPath, manifestPath, goalsSha256, manifestSha256 };
}

function parseManifest(value: unknown): CatalogManifest {
  exactKeys(value, [
    'format_version',
    'task_profile_version',
    'source_freeze_sha256',
    'source_freeze_content_sha256',
    'source_request_sha256',
    'task_goals_sha256',
    'identities_sha256',
    'identity_count',
    'denominators',
    'selector_scheme',
  ], 'catalog_manifest_invalid');
  const manifest = value;
  if (manifest['format_version'] !== 1 || manifest['task_profile_version'] !== 1
      || manifest['identity_count'] !== 120 || manifest['selector_scheme'] !== 'sha256/domain-separated-v1') {
    fail('catalog_manifest_invalid');
  }
  for (const key of [
    'source_freeze_sha256',
    'source_freeze_content_sha256',
    'source_request_sha256',
    'task_goals_sha256',
    'identities_sha256',
  ]) requireSha256(manifest[key], 'catalog_manifest_invalid');
  exactKeys(manifest['denominators'], ['fixture_runs', 'textedit_runs', 'total_records'], 'catalog_manifest_invalid');
  const denominators = manifest['denominators'];
  if (denominators['fixture_runs'] !== 100 || denominators['textedit_runs'] !== 20 || denominators['total_records'] !== 120) {
    fail('catalog_manifest_invalid');
  }
  return manifest as unknown as CatalogManifest;
}

function targetKeysFor(taskId: string): readonly string[] {
  if (taskId.startsWith('SET-')) return taskId === 'SET-06'
    ? ['control', 'group', 'disambiguator']
    : ['control', 'group'];
  if (taskId.startsWith('FORM-')) return ['control', 'form'];
  if (taskId === 'LIST-02') return ['list', 'target', 'context'];
  if (taskId.startsWith('LIST-')) return ['list', 'target'];
  if (taskId === 'MODAL-02') return ['modal', 'control'];
  if (taskId.startsWith('MODAL-')) return ['modal', 'background'];
  fail('catalog_goals_invalid');
}

function validateFixtureScope(scope: unknown, taskId: string): FrozenTaskScopeFamily {
  exactKeys(scope, ['application', 'targets', 'no_unrelated_controls'], 'catalog_goals_invalid');
  const app = scope['application'];
  nonemptyText(app, 256, 'catalog_goals_invalid');
  if (scope['no_unrelated_controls'] !== true || !Array.isArray(scope['targets']) || scope['targets'].length !== 1) {
    fail('catalog_goals_invalid');
  }
  const target = scope['targets'][0];
  exactKeys(target, targetKeysFor(taskId), 'catalog_goals_invalid');
  for (const value of Object.values(target)) {
    nonemptyText(value, 256, 'catalog_goals_invalid');
  }
  if (taskId.startsWith('SET-')) {
    if (taskId === 'SET-06' && target['disambiguator'] !== target['group']) fail('catalog_goals_invalid');
    return 'control';
  }
  if (taskId.startsWith('FORM-')) return 'form';
  if (taskId.startsWith('LIST-')) return 'list';
  return 'modal';
}

function validateTextEditScope(scope: unknown, taskId: string, objective: string): void {
  exactKeys(scope, [
    'application',
    'output_directory',
    'file_name',
    'document_format',
    'operations',
    'no_other_files',
  ], 'catalog_goals_invalid');
  nonemptyText(scope['application'], 256, 'catalog_goals_invalid');
  nonemptyText(scope['output_directory'], 4096, 'catalog_goals_invalid');
  nonemptyText(scope['file_name'], 256, 'catalog_goals_invalid');
  if (typeof scope['output_directory'] !== 'string' || !isAbsolute(scope['output_directory'])
      || scope['output_directory'].split(/[\\/]/).includes('..')) {
    fail('catalog_goals_invalid');
  }
  if (scope['file_name'] !== `cu-mvp-${taskId.toLowerCase()}.txt`
      || scope['document_format'] !== 'plain_text'
      || scope['no_other_files'] !== true
      || !Array.isArray(scope['operations'])
      || !scope['operations'].every((operation) => typeof operation === 'string' && !CONTROL_TEXT.test(operation))
      || JSON.stringify(scope['operations']) !== JSON.stringify(['write_requested_contents', 'save', 'close', 'reopen_and_read_back'])
      || !objective.includes(`${scope['output_directory']}/${scope['file_name']}`)) {
    fail('catalog_goals_invalid');
  }
}

function scopeFamilyAndUnsupportedReason(
  scope: unknown,
  suite: 'fixture' | 'textedit',
  taskId: string,
  objective: string,
): { family: FrozenTaskScopeFamily; unsupported?: FrozenTaskUnsupportedReason } {
  if (suite === 'fixture') {
    const family = validateFixtureScope(scope, taskId);
    if (family === 'control') return { family };
    // Typed form context is expressible by the operator contract. The trusted
    // AppPack still decides later which concrete FORM tasks it can execute.
    if (family === 'form') return { family };
    if (family === 'list') return { family, unsupported: 'list_scope_not_supported' };
    return { family, unsupported: 'modal_scope_not_supported' };
  }
  validateTextEditScope(scope, taskId, objective);
  return { family: 'textedit', unsupported: 'textedit_scope_not_supported' };
}

function parseGoals(value: unknown): readonly RawCatalogRecord[] {
  exactKeys(value, ['format_version', 'task_profile_version', 'records'], 'catalog_goals_invalid');
  const goals = value;
  if (goals['format_version'] !== 1 || goals['task_profile_version'] !== 1
      || !Array.isArray(goals['records']) || goals['records'].length !== 120) {
    fail('catalog_goals_invalid');
  }
  const records: RawCatalogRecord[] = [];
  for (let index = 0; index < EXPECTED_IDENTITIES.length; index += 1) {
    const item = goals['records'][index];
    exactKeys(item, ['selector', 'suite', 'task_id', 'seed', 'goal'], 'catalog_goals_invalid');
    const expected = EXPECTED_IDENTITIES[index]!;
    if (item['suite'] !== expected.suite || item['task_id'] !== expected.task_id || item['seed'] !== expected.seed) {
      fail('catalog_goals_invalid');
    }
    requireSha256(item['selector'], 'catalog_goals_invalid');
    exactKeys(item['goal'], ['objective', 'allowed_scope'], 'catalog_goals_invalid');
    const goal = item['goal'];
    objectiveText(goal['objective']);
    const identity: ParsedIdentity = {
      suite: expected.suite,
      task_id: expected.task_id,
      seed: expected.seed,
    };
    records.push({
      ...identity,
      selector: item['selector'],
      goal: { objective: goal['objective'], allowed_scope: goal['allowed_scope'] },
    });
  }
  return records;
}

function selectorFor(
  sourceFreezeSha256: string,
  record: RawCatalogRecord,
): string {
  const goalHash = sha256(Buffer.from(stableJson(record.goal), 'utf8'));
  const input = `computer-use-frozen-task-selector-v1\n${sourceFreezeSha256}\n${record.suite}\n${record.task_id}\n${record.seed}\n${goalHash}`;
  return sha256(Buffer.from(input, 'utf8'));
}

function deepFreeze<T>(value: T): T {
  if (value !== null && typeof value === 'object' && !Object.isFrozen(value)) {
    for (const child of Object.values(value as Record<string, unknown>)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

function loadEntries(config: CatalogConfig): { catalogId: string; entries: readonly StoredRecord[] } {
  const owner = currentUid();
  let goalsBytes: Buffer | undefined;
  let manifestBytes: Buffer | undefined;
  try {
    const goalsFile = readPrivateStableFile(
      config.goalsPath!, GOALS_FILE_NAME, undefined, MAX_GOALS_BYTES, 'catalog_goals_invalid', owner,
    );
    goalsBytes = goalsFile.bytes;
    const manifestFile = readPrivateStableFile(
      config.manifestPath!, MANIFEST_FILE_NAME, goalsFile.directory, MAX_MANIFEST_BYTES, 'catalog_manifest_invalid', owner,
    );
    manifestBytes = manifestFile.bytes;
    const goalsSha = sha256(goalsBytes);
    const manifestSha = sha256(manifestBytes);
    if (goalsSha !== config.goalsSha256 || manifestSha !== config.manifestSha256) fail('catalog_hash_mismatch');

    const manifest = parseManifest(parseCanonicalJson(manifestBytes, 'catalog_manifest_invalid'));
    if (manifest.task_goals_sha256 !== goalsSha) fail('catalog_hash_mismatch');
    const rawGoals = parseCanonicalJson(goalsBytes, 'catalog_goals_invalid');
    const records = parseGoals(rawGoals);
    const identityTriples = records.map((record) => [record.suite, record.task_id, record.seed]);
    if (sha256(Buffer.from(stableJson(identityTriples), 'utf8')) !== manifest.identities_sha256) {
      fail('catalog_manifest_invalid');
    }
    const selectors = new Set<string>();
    const appNames = new Map<string, string>();
    const entries = records.map((record) => {
      if (record.selector !== selectorFor(manifest.source_freeze_sha256, record) || selectors.has(record.selector)) {
        fail('catalog_goals_invalid');
      }
      selectors.add(record.selector);
      const family = scopeFamilyAndUnsupportedReason(
        record.goal.allowed_scope, record.suite, record.task_id, record.goal.objective,
      );
      const scope = record.goal.allowed_scope as Record<string, unknown>;
      const application = scope['application'];
      const priorApplication = appNames.get(record.suite);
      if (priorApplication !== undefined && priorApplication !== application) fail('catalog_goals_invalid');
      appNames.set(record.suite, application as string);

      const metadata = deepFreeze({
        catalog_id: manifestSha,
        selector: record.selector,
      task_id: record.task_id,
      seed: String(record.seed),
      suite: record.suite,
      scope_family: family.family,
      scope_compatible: family.unsupported === undefined,
        ...(family.unsupported === undefined ? {} : { unsupported_reason: family.unsupported }),
      } satisfies FrozenTaskCatalogEntry);
      if (family.unsupported) return { metadata, unsupportedReason: family.unsupported } satisfies StoredRecord;

      const goal = deepFreeze({
        objective: record.goal.objective,
        allowed_scope: record.goal.allowed_scope as AllowedControlScope,
      } satisfies FrozenTaskCanonicalGoal);
      const profile = deepFreeze({
        catalog_id: manifestSha,
        selector: record.selector,
        source_freeze_sha256: manifest.source_freeze_sha256,
        source_request_sha256: manifest.source_request_sha256,
        goals_sha256: manifest.task_goals_sha256,
        suite: record.suite,
        task_id: record.task_id,
        seed: record.seed,
      } satisfies FrozenTaskProfile);
      return { metadata, goal, profile } satisfies StoredRecord;
    });
    return { catalogId: manifestSha, entries: deepFreeze(entries) };
  } finally {
    goalsBytes?.fill(0);
    manifestBytes?.fill(0);
  }
}

class FrozenTaskCatalogImpl implements FrozenTaskCatalog {
  private readonly entriesBySelector: ReadonlyMap<string, StoredRecord>;
  private readonly publicEntries: readonly FrozenTaskCatalogEntry[];

  private constructor(
    private readonly catalogId: string | null,
    private readonly state: FrozenTaskCatalogSnapshot['status'],
    entries: readonly StoredRecord[],
  ) {
    this.entriesBySelector = new Map(entries.map((entry) => [entry.metadata.selector, entry]));
    this.publicEntries = deepFreeze(entries.map((entry) => entry.metadata));
    Object.freeze(this);
  }

  static empty(): FrozenTaskCatalog {
    return new FrozenTaskCatalogImpl(null, deepFreeze({ state: 'empty' }), []);
  }

  static unavailable(reason: FrozenTaskCatalogUnavailableReason): FrozenTaskCatalog {
    return new FrozenTaskCatalogImpl(null, deepFreeze({ state: 'unavailable', reason }), []);
  }

  static available(catalogId: string, entries: readonly StoredRecord[]): FrozenTaskCatalog {
    return new FrozenTaskCatalogImpl(catalogId, deepFreeze({ state: 'available' }), entries);
  }

  status(): FrozenTaskCatalogSnapshot['status'] {
    return this.state;
  }

  list(): readonly FrozenTaskCatalogEntry[] {
    return this.publicEntries;
  }

  snapshot(): FrozenTaskCatalogSnapshot {
    return deepFreeze({ status: this.state, tasks: this.publicEntries });
  }

  resolve(selection: FrozenTaskSelection): FrozenTaskResolution | null {
    if (!isRecord(selection) || Object.keys(selection).length !== 2
        || !Object.hasOwn(selection, 'catalog_id') || !Object.hasOwn(selection, 'selector')
        || typeof selection['catalog_id'] !== 'string' || typeof selection['selector'] !== 'string'
        || selection['catalog_id'] !== this.catalogId || !SHA256.test(selection['selector'])) return null;
    const record = this.entriesBySelector.get(selection['selector']);
    if (!record) return null;
    if (record.unsupportedReason) {
      return deepFreeze({
        status: 'unsupported',
        reason: record.unsupportedReason,
        profileMetadata: record.metadata,
      });
    }
    if (!record.goal || !record.profile) return null;
    return deepFreeze({
      status: 'available',
      goal: record.goal,
      profile: record.profile,
      profileMetadata: record.metadata,
    });
  }
}

export function loadFrozenTaskCatalog(env: NodeJS.ProcessEnv = process.env): FrozenTaskCatalog {
  let config: CatalogConfig | null;
  try {
    config = exactExpectedConfig(env);
    if (config === null) return FrozenTaskCatalogImpl.empty();
    const loaded = loadEntries(config);
    return FrozenTaskCatalogImpl.available(loaded.catalogId, loaded.entries);
  } catch (error) {
    return FrozenTaskCatalogImpl.unavailable(error instanceof CatalogFailure ? error.reason : 'catalog_files_unavailable');
  }
}
