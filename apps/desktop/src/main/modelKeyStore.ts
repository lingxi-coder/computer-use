import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { existsSync } from 'node:fs';
import { lstat, mkdir, open, readFile, rename, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import type { ModelRoute, SavedModelConnection } from '@computer-use/desktop-api';
import { hostEnvironment, validateModelKey } from './modelBootstrap.js';
import { modelRoute, type ModelKeyStore } from './modelAccess.js';

export type ModelKeyStoreFailureCode = 'auth_required' | 'locked' | 'not_found' | 'invalid' | 'unavailable' | 'timeout';
type KeychainReplyFailureCode = Exclude<ModelKeyStoreFailureCode, 'timeout'>;
const KEYCHAIN_FAILURE_CODES = new Set<KeychainReplyFailureCode>(['auth_required', 'locked', 'not_found', 'invalid', 'unavailable']);
const SAFE_FAILURE_CODES = new Set<ModelKeyStoreFailureCode>([...KEYCHAIN_FAILURE_CODES, 'timeout']);

/** A bounded Keychain failure category. It deliberately never retains native error text. */
export class ModelKeyStoreFailure extends Error {
  readonly code: ModelKeyStoreFailureCode;
  constructor(code: unknown) {
    super('The saved model key could not be accessed.');
    this.name = 'ModelKeyStoreFailure';
    this.code = typeof code === 'string' && SAFE_FAILURE_CODES.has(code as ModelKeyStoreFailureCode)
      ? code as ModelKeyStoreFailureCode : 'unavailable';
  }
}

type KeychainOperation = 'save' | 'load' | 'forget';

/** Decode only the helper's bounded, allowlisted result shape. Never echo reply text. */
export function parseKeychainReply(bytes: Buffer, operation: KeychainOperation, exitCode: number | null): string | null {
  if (exitCode !== 0 && bytes.length === 0) throw new ModelKeyStoreFailure('unavailable');
  let value: unknown;
  try { value = JSON.parse(bytes.toString('utf8')); }
  catch { throw new ModelKeyStoreFailure('invalid'); }
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new ModelKeyStoreFailure('invalid');
  const reply = value as Record<string, unknown>;
  const code = typeof reply.code === 'string' && KEYCHAIN_FAILURE_CODES.has(reply.code as KeychainReplyFailureCode)
    ? reply.code as KeychainReplyFailureCode : null;
  if (reply.ok === false) {
    throw new ModelKeyStoreFailure(code && code !== 'not_found' ? code : 'unavailable');
  }
  if (reply.ok !== true) throw new ModelKeyStoreFailure('invalid');

  // A missing saved item is a normal load result, but never accept any other
  // success response from a process that exited unsuccessfully.
  if (operation === 'load' && code === 'not_found' && exitCode === 0) return null;
  if (exitCode !== 0) throw new ModelKeyStoreFailure('unavailable');
  const expected = operation === 'load' ? 'loaded' : operation === 'save' ? 'saved' : 'forgotten';
  if (reply.code !== expected) throw new ModelKeyStoreFailure('invalid');
  if (operation !== 'load') return null;
  const key = reply.key;
  if (typeof key !== 'string' || Buffer.byteLength(key, 'utf8') === 0 || Buffer.byteLength(key, 'utf8') > 16 * 1024) {
    throw new ModelKeyStoreFailure('invalid');
  }
  return key;
}

export interface PrivateKeychain {
  readonly available: boolean;
  save(account: string, key: string): Promise<void>;
  load(account: string): Promise<string | null>;
  forget(account: string): Promise<void>;
}
export class NativeKeychain implements PrivateKeychain {
  readonly available: boolean;
  constructor(private readonly helper: string, private readonly timeoutMs = 10_000) {
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 10_000) {
      throw new Error('The Keychain helper timeout is outside its fixed bound.');
    }
    this.available = process.platform === 'darwin' && existsSync(helper);
  }
  private async request(operation: KeychainOperation, account: string, key?: string): Promise<string | null> {
    if (!this.available || !/^[a-f0-9]{64}$/.test(account)) throw new Error('The app Keychain is unavailable.');
    const input = Buffer.from(JSON.stringify({ operation, account, ...(key === undefined ? {} : { key }) }), 'utf8');
    if (input.length > 32 * 1024) { input.fill(0); throw new Error('The API key exceeds its limit.'); }
    const chunks: Buffer[] = [];
    const child = spawn(this.helper, [], { stdio: ['pipe', 'pipe', 'ignore'], env: hostEnvironment(process.env) });
    let size = 0, oversized = false, timedOut = false;
    child.stdout.on('data', (chunk: Buffer) => {
      size += chunk.length;
      if (size > 32 * 1024) { oversized = true; chunk.fill(0); child.kill('SIGKILL'); } else chunks.push(chunk);
    });
    child.stdin.on('error', () => undefined);
    child.stdin.end(input, () => input.fill(0));
    const timeout = setTimeout(() => { timedOut = true; child.kill('SIGKILL'); }, this.timeoutMs);
    try {
      const exitCode = await new Promise<number | null>((done, fail) => {
        child.once('error', () => fail(new ModelKeyStoreFailure('unavailable'))); child.once('close', done);
      });
      if (timedOut) throw new ModelKeyStoreFailure('timeout');
      if (oversized) throw new ModelKeyStoreFailure('invalid');
      const bytes = Buffer.concat(chunks);
      try { return parseKeychainReply(bytes, operation, exitCode); }
      finally { bytes.fill(0); }
    } finally { clearTimeout(timeout); input.fill(0); chunks.forEach((chunk) => chunk.fill(0)); }
  }
  async save(account: string, key: string): Promise<void> { await this.request('save', account, key); }
  async load(account: string): Promise<string | null> { return this.request('load', account); }
  async forget(account: string): Promise<void> { await this.request('forget', account); }
}

export function connectionId(route: ModelRoute): string {
  const { provider_id, protocol, base_url, model, preset_id } = route;
  return createHash('sha256').update(JSON.stringify({ provider_id, protocol, base_url, model, ...(preset_id !== undefined ? { preset_id } : {}) })).digest('hex');
}
/** The metadata file contains public routing facts only. Keys stay in the
 * product's exact Keychain service/account, and saved routes require consent. */
export class ProductModelKeyStore implements ModelKeyStore {
  private readonly file: string;
  private mutations: Promise<unknown> = Promise.resolve();
  constructor(private readonly directory: string, private readonly keychain: PrivateKeychain) { this.file = join(directory, 'connections.json'); }
  get available(): boolean { return this.keychain.available; }
  async list(): Promise<SavedModelConnection[]> {
    let bytes: Buffer;
    try {
      const stat = await lstat(this.file);
      if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 64 * 1024 || (stat.mode & 0o077) !== 0) throw new Error('Saved connection metadata is unavailable.');
      bytes = await readFile(this.file);
    } catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return []; throw new Error('Saved connection metadata is unavailable.'); }
    let value: unknown;
    try {
      if (bytes.length > 64 * 1024) throw new Error();
      value = JSON.parse(bytes.toString('utf8'));
    } catch { throw new Error('Saved connection metadata is invalid.'); }
    if (!Array.isArray(value) || value.length > 16) throw new Error('Saved connection metadata is invalid.');
    const ids = new Set<string>();
    return value.map((row: unknown) => {
      if (!row || typeof row !== 'object' || Object.keys(row).sort().join(',') !== 'id,route') throw new Error('Saved connection metadata is invalid.');
      const v = row as SavedModelConnection;
      const route = modelRoute(v.route);
      if (v.id !== connectionId(route) || ids.has(v.id)) throw new Error('Saved connection metadata is invalid.');
      ids.add(v.id); return { id: v.id, route };
    });
  }
  private async write(rows: SavedModelConnection[]): Promise<void> {
    await mkdir(this.directory, { recursive: true, mode: 0o700 });
    const temp = join(this.directory, `.connections-${randomBytes(8).toString('hex')}.tmp`);
    const file = await open(temp, 'wx', 0o600);
    try {
      try { await file.writeFile(JSON.stringify(rows) + '\n'); await file.sync(); }
      finally { await file.close(); }
      await rename(temp, this.file);
    } finally { await unlink(temp).catch(() => undefined); }
  }
  async save(route: ModelRoute, key: string): Promise<SavedModelConnection> {
    return this.mutate(() => this.saveInner(route, key));
  }
  private async saveInner(route: ModelRoute, key: string): Promise<SavedModelConnection> {
    const checkedRoute = modelRoute(route), checkedKey = validateModelKey(key);
    const rows = await this.list();
    const id = connectionId(checkedRoute);
    if (rows.length >= 16 && !rows.some((r) => r.id === id)) throw new Error('Remove a saved connection before saving another.');
    let previous = rows.some((r) => r.id === id) ? await this.keychain.load(id) : null;
    const record = { id, route: checkedRoute };
    try {
      await this.keychain.save(id, checkedKey);
      await this.write([...rows.filter((r) => r.id !== id), record]);
      return record;
    } catch {
      // A helper can fail after committing the item. Restore the prior secret
      // for both helper failures and failed metadata commits, under this lock.
      try {
        if (previous === null) await this.keychain.forget(id);
        else await this.keychain.save(id, previous);
      } catch { throw new Error('The saved connection could not be restored. Reconnect this provider.'); }
      throw new Error('The saved connection could not be recorded.');
    } finally { previous = null; }
  }
  async load(id: string): Promise<{ route: ModelRoute; api_key: string } | null> {
    const row = (await this.list()).find((r) => r.id === id);
    if (!row) return null;
    const key = await this.keychain.load(id);
    return key === null ? null : { route: row.route, api_key: key };
  }
  async forget(id: string): Promise<void> {
    await this.mutate(async () => {
      const rows = await this.list(); if (!rows.some((r) => r.id === id)) return;
      await this.keychain.forget(id); await this.write(rows.filter((r) => r.id !== id));
    });
  }
  private mutate<T>(operation: () => Promise<T>): Promise<T> {
    const next = this.mutations.then(operation);
    this.mutations = next.catch(() => undefined); return next;
  }
}
