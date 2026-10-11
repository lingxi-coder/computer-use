import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ModelKeyStoreFailure, NativeKeychain, ProductModelKeyStore, connectionId, parseKeychainReply, type PrivateKeychain } from '../src/main/modelKeyStore.js';
import type { ModelRoute } from '@computer-use/desktop-api';

const KEY = 'owned-store-test-key';
const route: ModelRoute = { provider_id: 'open_ai', protocol: 'open_ai_responses', base_url: 'https://example.invalid/v1', model: 'fixture', max_output_tokens: 4096 };
class Keychain implements PrivateKeychain {
  available = true; values = new Map<string, string>(); reads: string[] = [];
  async save(id: string, value: string) { this.values.set(id, value); }
  async load(id: string) { this.reads.push(id); return this.values.get(id) ?? null; }
  async forget(id: string) { this.values.delete(id); }
}
test('metadata has no plaintext key, startup does not read Keychain and forget stays scoped', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cu-model-store-'));
  try {
    const keychain = new Keychain(), store = new ProductModelKeyStore(dir, keychain);
    assert.deepEqual(await store.list(), []); const saved = await store.save(route, KEY);
    assert.equal(saved.id, connectionId(route)); assert.deepEqual(keychain.reads, []);
    const file = join(dir, 'connections.json'); assert(!(await readFile(file, 'utf8')).includes(KEY));
    assert(!(await readFile(file, 'utf8')).includes('s1_teacher') && !(await readFile(file, 'utf8')).includes('s2_planning'));
    assert.equal((await stat(file)).mode & 0o777, 0o600);
    assert.deepEqual(await store.list(), [saved]); assert.deepEqual(keychain.reads, []);
    assert.equal(await store.load('f'.repeat(64)), null); assert.deepEqual(keychain.reads, []);
    assert.equal((await store.load(saved.id))?.api_key, KEY); assert.deepEqual(keychain.reads, [saved.id]);
    await store.forget(saved.id); assert.equal(await store.load(saved.id), null); assert.equal(keychain.values.size, 0);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
test('forged metadata, extra fields and insecure routes cannot trigger a key lookup', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cu-model-store-'));
  try {
    const keychain = new Keychain(), store = new ProductModelKeyStore(dir, keychain);
    for (const value of [[{ id: 'a'.repeat(64), route }], [{ id: connectionId(route), route, key: KEY }], [{ id: connectionId(route), route: { ...route, base_url: 'http://example.invalid' } }]]) {
      await writeFile(join(dir, 'connections.json'), JSON.stringify(value), { mode: 0o600 }); await assert.rejects(store.list());
    }
    assert.deepEqual(keychain.reads, []);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
test('malformed public metadata uses a fixed error without echoing its contents', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cu-model-store-'));
  try {
    const store = new ProductModelKeyStore(dir, new Keychain());
    await writeFile(join(dir, 'connections.json'), '{' + KEY, { mode: 0o600 });
    await assert.rejects(store.list(), { message: 'Saved connection metadata is invalid.' });
  } finally { await rm(dir, { recursive: true, force: true }); }
});
test('Keychain helper failures accept only finite safe codes, including nonzero replies', () => {
  for (const code of ['auth_required', 'locked', 'invalid', 'unavailable'] as const) {
    assert.throws(() => parseKeychainReply(Buffer.from(JSON.stringify({ ok: false, code })), 'load', 1), (error) => {
      assert(error instanceof ModelKeyStoreFailure);
      assert.equal(error.code, code);
      assert(!error.message.includes(KEY));
      return true;
    });
  }
  assert.equal(parseKeychainReply(Buffer.from('{"ok":true,"code":"not_found"}'), 'load', 0), null);
  assert.throws(() => parseKeychainReply(Buffer.from('{"ok":true,"code":"loaded"}'), 'load', 1), (error) => {
    assert(error instanceof ModelKeyStoreFailure); assert.equal(error.code, 'unavailable'); return true;
  });
  const secret = `status-${KEY}-\nraw-helper-details`;
  assert.throws(() => parseKeychainReply(Buffer.from(JSON.stringify({ ok: false, code: secret, status: secret, key: KEY })), 'load', 1), (error) => {
    assert(error instanceof ModelKeyStoreFailure); assert.equal(error.code, 'unavailable');
    assert(!error.message.includes(KEY) && !error.message.includes(secret)); return true;
  });
  assert.throws(() => parseKeychainReply(Buffer.from(`{${KEY}`), 'load', 1), (error) => {
    assert(error instanceof ModelKeyStoreFailure); assert.equal(error.code, 'invalid');
    assert(!error.message.includes(KEY)); return true;
  });
  for (const exitCode of [1, null] as const) {
    assert.throws(() => parseKeychainReply(Buffer.alloc(0), 'load', exitCode), (error) => {
      assert(error instanceof ModelKeyStoreFailure); assert.equal(error.code, 'unavailable'); return true;
    });
  }
  assert.throws(() => parseKeychainReply(Buffer.from('{malformed'), 'load', 0), (error) => {
    assert(error instanceof ModelKeyStoreFailure); assert.equal(error.code, 'invalid'); return true;
  });
});
test('a timed-out helper is reported as timeout without parsing or exposing its output', { skip: process.platform !== 'darwin' }, async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cu-keychain-timeout-'));
  try {
    const helper = join(dir, 'stalled-keychain');
    await writeFile(helper, '#!/usr/bin/env node\nsetInterval(() => {}, 1000);\n', { mode: 0o700 });
    const store = new NativeKeychain(helper, 25);
    await assert.rejects(store.load('a'.repeat(64)), (error) => {
      assert(error instanceof ModelKeyStoreFailure); assert.equal(error.code, 'timeout');
      assert(!error.message.includes(KEY)); return true;
    });
  } finally { await rm(dir, { recursive: true, force: true }); }
});
test('concurrent saves and removals preserve all requested metadata updates', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cu-model-store-'));
  try {
    const keychain = new Keychain(), store = new ProductModelKeyStore(dir, keychain);
    const records = await Promise.all([store.save(route, KEY), store.save({ ...route, model: 'second' }, KEY)]);
    assert.deepEqual(await store.list(), records); assert.equal(keychain.values.size, 2);
    await Promise.all(records.map(record => store.forget(record.id)));
    assert.deepEqual(await store.list(), []); assert.equal(keychain.values.size, 0);
  } finally { await rm(dir, { recursive: true, force: true }); }
});


test('failed metadata commit restores an existing Keychain session', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cu-model-store-rollback-'));
  try {
    const keychain = new Keychain(), store = new ProductModelKeyStore(dir, keychain);
    const saved = await store.save(route, KEY);
    const metadata = await readFile(join(dir, 'connections.json'), 'utf8');
    const originalWrite = (store as unknown as { write: (rows: unknown[]) => Promise<void> }).write;
    (store as unknown as { write: (rows: unknown[]) => Promise<void> }).write = async () => { throw new Error('simulated metadata rename failure'); };
    await assert.rejects(store.save(route, 'replacement-oauth-session'), /could not be recorded/);
    assert.equal(keychain.values.get(saved.id), KEY);
    assert.equal(await readFile(join(dir, 'connections.json'), 'utf8'), metadata);
    (store as unknown as { write: typeof originalWrite }).write = originalWrite;
    assert.equal((await store.load(saved.id))?.api_key, KEY);
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('a helper failure after secret mutation restores the old item', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cu-model-store-rollback-'));
  try {
    const keychain = new Keychain(), store = new ProductModelKeyStore(dir, keychain);
    const saved = await store.save(route, KEY);
    const originalSave = keychain.save.bind(keychain);
    keychain.save = async (id, value) => {
      await originalSave(id, value);
      if (value === 'replacement') throw new Error('lost helper acknowledgement');
    };
    await assert.rejects(store.save(route, 'replacement'), /could not be recorded/);
    assert.equal(keychain.values.get(saved.id), KEY);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
