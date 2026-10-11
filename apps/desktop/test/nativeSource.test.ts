import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import type { RowTableScope } from '@computer-use/desktop-api';

import { nativeSourceFromEnvironment, nativeSourceReadSettings, validateNativeSource } from '../src/main/nativeSource.js';
import { Supervisor } from '../src/main/supervisor.js';

function fixture() {
  const dir = mkdtempSync(join(tmpdir(), 'cu-native-config-'));
  const helperPath = join(dir, 'cu-macos-source');
  writeFileSync(helperPath, '', { mode: 0o700 });
  return {
    dir, helperPath,
    binaries: { host: join(dir, 'cu-host'), device: join(dir, 'cu-device'), source: join(dir, 'cu-source') },
    dispose: () => rmSync(dir, { recursive: true, force: true }),
  };
}

function scopeAtSerializedBytes(size: number): RowTableScope {
  const headers = Array.from({ length: 16 }, (_, index) => `${'h'.repeat(190)}${String(index).padStart(10, '0')}`);
  const scope: RowTableScope = {
    scroll_path: [{ role: 'ax_scroll_area', label: 'Archive' }],
    column_headers: headers as RowTableScope['column_headers'],
  };
  let remaining = size - Buffer.byteLength(JSON.stringify(scope), 'utf8');
  for (let index = 0; index < headers.length && remaining > 0; index += 1) {
    const added = Math.min(remaining, 256 - headers[index]!.length);
    headers[index] += 'x'.repeat(added);
    remaining -= added;
  }
  if (remaining !== 0) throw new Error(`could not construct a ${size}-byte scope fixture`);
  scope.column_headers = headers as RowTableScope['column_headers'];
  if (Buffer.byteLength(JSON.stringify(scope), 'utf8') !== size) throw new Error('scope fixture has the wrong serialized size');
  return scope;
}

test('default source configuration stays simulated, and native resolves the adjacent helper', () => {
  const f = fixture();
  try {
    assert.equal(nativeSourceFromEnvironment({}, f.binaries), undefined);
    assert.equal(nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'fixture' }, f.binaries), undefined);
    assert.deepEqual(nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: ' com.apple.TextEdit ' }, f.binaries), {
      helperPath: f.helperPath, bundleId: 'com.apple.TextEdit',
    });
    assert.deepEqual(nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: 'com.apple.TextEdit', CU_NATIVE_SOURCE_HELPER: f.helperPath, CU_TARGET_PID: '123',
    }, f.binaries), { helperPath: f.helperPath, bundleId: 'com.apple.TextEdit', pid: 123 });
  } finally {
    f.dispose();
  }
});

test('native configuration rejects missing targets, invalid helper paths and invalid PIDs', () => {
  const f = fixture();
  try {
    assert.throws(() => nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'native' }, f.binaries), /requires CU_TARGET_BUNDLE_ID/);
    assert.throws(() => nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'unknown' }, f.binaries), /unsupported source backend/);
    for (const helperPath of ['relative/helper', join(f.dir, 'missing'), f.dir]) {
      assert.throws(() => validateNativeSource({ helperPath, bundleId: 'com.apple.TextEdit' }), /absolute path|not found/);
    }
    for (const bundleId of ['  ', 'x'.repeat(257), 'com.apple.TextEdit\n']) {
      assert.throws(() => validateNativeSource({ helperPath: f.helperPath, bundleId }), /target bundle ID/);
    }
    for (const pid of ['', '0', '-1', '1.5', 'nan', 'Infinity', '4294967296']) {
      assert.throws(() => nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: 'com.apple.TextEdit', CU_TARGET_PID: pid }, f.binaries), /positive integer/);
    }
  } finally {
    f.dispose();
  }
});

test('the supervisor rejects invalid native options before trying to start any binary', async () => {
  await assert.rejects(Supervisor.start({
    binaries: { host: '/missing/cu-host', device: '/missing/cu-device', source: '/missing/cu-source' },
    nativeSource: { helperPath: 'relative/helper', bundleId: 'com.apple.TextEdit' },
  }), /native source helper must have an absolute path/);
});

test('passive guard configuration is explicit and validated before process startup', () => {
  const f = fixture();
  try {
    for (const guardHelperPath of ['relative/guard', join(f.dir, 'missing'), f.dir]) {
      assert.throws(() => validateNativeSource({ helperPath: f.helperPath,
        bundleId: 'com.lingxi.fixture', guardHelperPath }), /native device guard/);
    }
    const guardHelperPath = join(f.dir, 'cu-macos-device-guard');
    writeFileSync(guardHelperPath, '', { mode: 0o700 });
    assert.deepEqual(nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: 'com.lingxi.fixture',
      CU_NATIVE_DEVICE_GUARD: guardHelperPath }, f.binaries), {
      helperPath: f.helperPath, bundleId: 'com.lingxi.fixture', guardHelperPath,
    });
  } finally { f.dispose(); }
});


test('native editing requires a fixed registered window and never comes from ordinary observe settings', () => {
  const f = fixture();
  try {
    const observed = { helperPath: f.helperPath, bundleId: 'com.apple.TextEdit', pid: 42 };
    assert.equal(validateNativeSource(observed).authorizedWindowId, undefined);
    assert.equal(validateNativeSource({ ...observed, authorizedWindowId: 'window-1' }).authorizedWindowId, 'window-1');
    const mvp = { ...observed, bundleId: 'com.lingxi.computer-user.mvp-fixture' };
    assert.equal(validateNativeSource({ ...mvp, authorizedWindowId: 'window-1' }).authorizedWindowId, 'window-1');
    assert.throws(() => validateNativeSource({ ...mvp, pid: undefined, authorizedWindowId: 'window-1' }), /fixed process and window/);
    assert.equal(validateNativeSource({ ...mvp, referencePlanner: true }).referencePlanner, true);
    for (const authorizedWindowId of ['', 'window-0', 'window-01', 'window-4294967296', 'other']) {
      assert.throws(() => validateNativeSource({ ...observed, authorizedWindowId }), /fixed process and window/);
    }
    assert.throws(() => validateNativeSource({ ...observed, pid: undefined, authorizedWindowId: 'window-1' }), /fixed process and window/);
    assert.throws(() => validateNativeSource({ ...observed, bundleId: 'unregistered.app', authorizedWindowId: 'window-1' }), /fixed process and window/);
    assert.throws(() => validateNativeSource({ ...observed, referencePlanner: true }), /registered owned fixtures/);
    assert.equal(nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: observed.bundleId, CU_TARGET_PID: '42', CU_NATIVE_EDIT_WINDOW: 'window-1' }, f.binaries)?.authorizedWindowId, undefined);
  } finally { f.dispose(); }
});

test('authorized kernel process identity is fixed, safe, and never sent to Source or read from the environment', () => {
  const f = fixture();
  try {
    const fixed = {
      helperPath: f.helperPath,
      bundleId: 'com.lingxi.computer-user.mvp-fixture',
      pid: 42,
      authorizedWindowId: 'window-9',
    };
    const identity = { pid: 42, start_seconds: 1_790_000_000, start_micros: 0 };
    assert.deepEqual(validateNativeSource({ ...fixed, authorizedProcessIdentity: identity }).authorizedProcessIdentity, identity);
    assert.throws(() => validateNativeSource({ ...fixed, authorizedWindowId: undefined, authorizedProcessIdentity: identity }), /fixed process\/window/);
    assert.throws(() => validateNativeSource({ ...fixed, pid: 43, authorizedProcessIdentity: identity }), /fixed process\/window/);
    for (const start_seconds of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, Number.POSITIVE_INFINITY]) {
      assert.throws(() => validateNativeSource({ ...fixed, authorizedProcessIdentity: { ...identity, start_seconds } }), /valid kernel start stamp/);
    }
    for (const start_micros of [-1, 1_000_000, 1.5, Number.POSITIVE_INFINITY]) {
      assert.throws(() => validateNativeSource({ ...fixed, authorizedProcessIdentity: { ...identity, start_micros } }), /valid kernel start stamp/);
    }
    assert.deepEqual(nativeSourceReadSettings({ ...fixed, authorizedProcessIdentity: identity }), {
      native_text_policy: 'registered_editor', native_text_window_id: 'window-9',
    });
    const fromEnvironment = nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: fixed.bundleId, CU_TARGET_PID: '42',
      CU_TARGET_PROCESS_START_SECONDS: '1790000000', CU_TARGET_PROCESS_START_MICROS: '0',
    }, f.binaries);
    assert.equal(fromEnvironment?.authorizedProcessIdentity, undefined);
  } finally { f.dispose(); }
});

test('initial row scope uses an independent read-only Source window and forwards only public scope', () => {
  const f = fixture();
  try {
    const tableScope: RowTableScope = {
      scroll_path: [
        { role: 'ax_scroll_area', label: 'Preferences content' },
        { role: 'ax_scroll_area', label: 'Archive' },
      ],
      column_headers: ['Archive item'],
    };
    const readOnly = validateNativeSource({
      helperPath: f.helperPath,
      bundleId: 'com.lingxi.computer-user.mvp-fixture',
      pid: 42,
      sourceReadWindowId: 'window-9',
      tableScope,
    });

    assert.equal(readOnly.authorizedWindowId, undefined, 'read-only scope must not become Device authorization');
    assert.equal(readOnly.sourceReadWindowId, 'window-9');
    assert.deepEqual(readOnly.tableScope, tableScope);
    assert.deepEqual(nativeSourceReadSettings(readOnly), {
      native_text_policy: 'registered_editor',
      native_text_window_id: 'window-9',
      native_table_scope: JSON.stringify(tableScope),
    });
    assert.deepEqual(nativeSourceReadSettings({
      helperPath: f.helperPath,
      bundleId: 'com.lingxi.computer-user.mvp-fixture',
      pid: 42,
    }), {}, 'ordinary native Source settings remain unchanged');
    assert.deepEqual(nativeSourceReadSettings({
      helperPath: f.helperPath,
      bundleId: 'com.lingxi.computer-user.mvp-fixture',
      pid: 42,
      authorizedWindowId: 'window-9',
    }), {
      native_text_policy: 'registered_editor',
      native_text_window_id: 'window-9',
    }, 'existing explicit editing authorization retains its Source read policy');
  } finally { f.dispose(); }
});

test('initial row scope rejects invalid scope and mismatched or unregistered Source read windows', () => {
  const f = fixture();
  try {
    const base = {
      helperPath: f.helperPath,
      bundleId: 'com.lingxi.computer-user.mvp-fixture',
      pid: 42,
      sourceReadWindowId: 'window-9',
    };
    const scope: RowTableScope = {
      scroll_path: [{ role: 'ax_scroll_area', label: 'Archive' }],
      column_headers: ['Archive item'],
    };
    assert.throws(() => validateNativeSource({
      helperPath: f.helperPath, bundleId: base.bundleId, pid: base.pid, tableScope: scope,
    }), /requires a Source-only/);
    assert.throws(() => validateNativeSource({ ...base, tableScope: { ...scope, conditions: [] } as never }), /only public scroll path and headers/);
    assert.throws(() => validateNativeSource({ ...base, tableScope: { ...scope, scroll_path: [] } as never }), /public row bounds/);
    assert.throws(() => validateNativeSource({ ...base, tableScope: {
      ...scope,
      scroll_path: [{ role: 'ax_table', label: 'Archive' }],
    } as never }), /invalid public scroll path/);
    assert.throws(() => validateNativeSource({ ...base, tableScope: {
      ...scope,
      column_headers: ['Archive item', ' archive ITEM '],
    } }), /duplicate public headers/);
    assert.throws(() => validateNativeSource({ ...base, tableScope: {
      ...scope,
      column_headers: ['North\u2003South', ' north south '],
    } }), /duplicate public headers/);
    assert.throws(() => validateNativeSource({ ...base, sourceReadWindowId: 'window-4294967296' }), /Source read policy/);
    assert.throws(() => validateNativeSource({ ...base, sourceReadWindowId: 'window-01' }), /Source read policy/);
    assert.equal(validateNativeSource({ ...base, sourceReadWindowId: 'window-4294967295' }).sourceReadWindowId,
      'window-4294967295', 'CGWindowID uses the native u32 range independently of the PID bound');
    assert.throws(() => validateNativeSource({ ...base, authorizedWindowId: 'window-10' }), /must match/);
    assert.throws(() => validateNativeSource({
      helperPath: f.helperPath, bundleId: base.bundleId, sourceReadWindowId: base.sourceReadWindowId,
    }), /Source read policy/);
    assert.throws(() => validateNativeSource({ ...base, pid: 0x8000_0000 }), /Source read policy/);
    assert.throws(() => validateNativeSource({ ...base, bundleId: 'unregistered.app' }), /Source read policy/);
    assert.equal(validateNativeSource(base).sourceReadWindowId, 'window-9',
      'a registered read-only window is valid without an initial table scope');
  } finally { f.dispose(); }
});

test('background AX Source configuration is fixed, read-only, and separate from foreground text/table policy', () => {
  const f = fixture();
  try {
    const background = nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native',
      CU_TARGET_BUNDLE_ID: 'com.lingxi.computer-user.mvp-fixture',
      CU_TARGET_PID: '42',
      CU_SOURCE_BACKGROUND_WINDOW_ID: 'window-9',
      CU_NATIVE_EDIT_WINDOW: 'window-9',
      CU_TARGET_PROCESS_START_SECONDS: '1790000000',
      CU_TARGET_PROCESS_START_MICROS: '123456',
    }, f.binaries);
    assert.deepEqual(background, {
      helperPath: f.helperPath, bundleId: 'com.lingxi.computer-user.mvp-fixture',
      pid: 42, backgroundWindowId: 'window-9',
    }, 'environment identity cannot become a Main-only background or process grant');
    assert.deepEqual(nativeSourceReadSettings(background!), { native_background_window_id: 'window-9' });

    const identity = { pid: 42, start_seconds: 1_790_000_000, start_micros: 123_456 };
    const consented = validateNativeSource({ ...background!, authorizedBackgroundWindowId: 'window-9',
      authorizedProcessIdentity: identity });
    assert.deepEqual(nativeSourceReadSettings(consented), {
      native_background_window_id: 'window-9',
      native_target_process_start_seconds: '1790000000',
      native_target_process_start_micros: '123456',
    }, 'the exact consented birth identity may pin the restarted Source watch');
    assert.equal(consented.authorizedWindowId, undefined, 'BG consent cannot select the foreground AXPress profile');

    for (const backgroundWindowId of ['', 'window-0', 'window-01', 'window-4294967296', 'window-9 ']) {
      assert.throws(() => nativeSourceFromEnvironment({
        CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: background!.bundleId, CU_TARGET_PID: '42',
        CU_SOURCE_BACKGROUND_WINDOW_ID: backgroundWindowId,
      }, f.binaries), /Background AX read policy/);
    }
    assert.throws(() => nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: background!.bundleId,
      CU_SOURCE_BACKGROUND_WINDOW_ID: 'window-9',
    }, f.binaries), /Background AX read policy/);
    assert.throws(() => nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: 'unregistered.app', CU_TARGET_PID: '42',
      CU_SOURCE_BACKGROUND_WINDOW_ID: 'window-9',
    }, f.binaries), /Background AX read policy/);
    assert.throws(() => validateNativeSource({ ...background!, sourceReadWindowId: 'window-9' }), /cannot be mixed/);
    assert.throws(() => validateNativeSource({ ...background!, sourceReadWindowId: 'window-9', tableScope: {
      scroll_path: [{ role: 'ax_scroll_area', label: 'Archive' }], column_headers: ['Archive item'],
    } }), /cannot be mixed/);
    assert.throws(() => validateNativeSource({ ...background!, authorizedWindowId: 'window-9' }), /cannot be mixed/);
    assert.throws(() => validateNativeSource({ ...background!, referencePlanner: true }), /Background AX reference planning/);
    const guardBackground = validateNativeSource({ ...background!, bundleId: 'com.lingxi.computer-user.guard-fixture', referencePlanner: true });
    assert.equal(guardBackground.referencePlanner, true);
    assert.deepEqual(nativeSourceReadSettings(guardBackground), { native_background_window_id: 'window-9' },
      'the deterministic planner does not change Source-only background read settings');
    const guardBackgroundFromEnvironment = nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native',
      CU_TARGET_BUNDLE_ID: 'com.lingxi.computer-user.guard-fixture',
      CU_TARGET_PID: '42',
      CU_SOURCE_BACKGROUND_WINDOW_ID: 'window-9',
      CU_NATIVE_REFERENCE_PLANNER: '1',
    }, f.binaries);
    assert.equal(guardBackgroundFromEnvironment?.referencePlanner, true);
    assert.deepEqual(nativeSourceReadSettings(guardBackgroundFromEnvironment!), { native_background_window_id: 'window-9' });
    assert.throws(() => validateNativeSource({ ...guardBackground, sourceReadWindowId: 'window-9' }), /cannot be mixed/);
    assert.throws(() => validateNativeSource({ ...guardBackground, tableScope: {
      scroll_path: [{ role: 'ax_scroll_area', label: 'Archive' }], column_headers: ['Archive item'],
    } }), /cannot be mixed/);
    assert.throws(() => validateNativeSource({ ...guardBackground, authorizedWindowId: 'window-9' }), /cannot be mixed/);
    assert.throws(() => validateNativeSource({ ...background!, bundleId: 'unregistered.app', referencePlanner: true }), /Background AX read policy/);
    assert.throws(() => validateNativeSource({ ...background!, authorizedBackgroundWindowId: 'window-10',
      authorizedProcessIdentity: identity }), /exact fixed Source process and window/);
    assert.throws(() => validateNativeSource({ ...background!, authorizedBackgroundWindowId: 'window-9' }), /requires the exact kernel/);
    assert.throws(() => validateNativeSource({ ...background!, authorizedBackgroundWindowId: 'window-9',
      authorizedProcessIdentity: { ...identity, pid: 43 } }), /valid kernel start stamp/);
  } finally { f.dispose(); }
});

test('bounded scoped Source environment options stay read-only and fail closed', () => {
  const f = fixture();
  try {
    const tableScope: RowTableScope = {
      scroll_path: [{ role: 'ax_scroll_area', label: 'Archive' }],
      column_headers: ['Name', 'Owner'],
    };
    const source = nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native',
      CU_TARGET_BUNDLE_ID: 'com.lingxi.computer-user.mvp-fixture',
      CU_TARGET_PID: '42',
      CU_SOURCE_READ_WINDOW_ID: 'window-9',
      CU_SOURCE_TABLE_SCOPE: JSON.stringify(tableScope),
    }, f.binaries);
    assert.equal(source?.sourceReadWindowId, 'window-9');
    assert.deepEqual(source?.tableScope, tableScope);
    assert.equal(source?.authorizedWindowId, undefined);
    assert.deepEqual(nativeSourceReadSettings(source!), {
      native_text_policy: 'registered_editor',
      native_text_window_id: 'window-9',
      native_table_scope: JSON.stringify(tableScope),
    });

    const atLimitScope = scopeAtSerializedBytes(4096);
    const atLimitJson = JSON.stringify(atLimitScope);
    assert.equal(Buffer.byteLength(atLimitJson, 'utf8'), 4096);
    const atLimit = nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: 'com.lingxi.computer-user.mvp-fixture',
      CU_TARGET_PID: '42', CU_SOURCE_READ_WINDOW_ID: 'window-9', CU_SOURCE_TABLE_SCOPE: atLimitJson,
    }, f.binaries);
    assert.deepEqual(atLimit?.tableScope, atLimitScope);
    assert.equal(Buffer.byteLength(nativeSourceReadSettings(atLimit!).native_table_scope!, 'utf8'), 4096);

    const overLimitHeaders = [...atLimitScope.column_headers];
    overLimitHeaders[15] += 'x';
    const overLimitScope: RowTableScope = { ...atLimitScope, column_headers: overLimitHeaders as RowTableScope['column_headers'] };
    const overLimitJson = JSON.stringify(overLimitScope);
    assert.equal(Buffer.byteLength(overLimitJson, 'utf8'), 4097);
    assert.throws(() => nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: 'com.lingxi.computer-user.mvp-fixture',
      CU_TARGET_PID: '42', CU_SOURCE_READ_WINDOW_ID: 'window-9', CU_SOURCE_TABLE_SCOPE: overLimitJson,
    }, f.binaries), /4096-byte/);
    assert.throws(() => validateNativeSource({
      helperPath: f.helperPath, bundleId: 'com.lingxi.computer-user.mvp-fixture', pid: 42,
      sourceReadWindowId: 'window-9', tableScope: overLimitScope,
    }), /4096-byte serialized/);
    assert.throws(() => nativeSourceReadSettings({
      helperPath: f.helperPath, bundleId: 'com.lingxi.computer-user.mvp-fixture', pid: 42,
      sourceReadWindowId: 'window-9', tableScope: overLimitScope,
    }), /4096-byte serialized/);

    for (const env of [
      { CU_SOURCE_TABLE_SCOPE: '{' },
      { CU_SOURCE_TABLE_SCOPE: '{"scroll_path":[],"scroll_path":[{"role":"ax_scroll_area","label":"Archive"}],"column_headers":["Archive item"]}' },
      { CU_SOURCE_TABLE_SCOPE: JSON.stringify({ ...tableScope, ignored: true }) },
      { CU_SOURCE_TABLE_SCOPE: JSON.stringify({ ...tableScope, column_headers: ['Name', ' name '] }) },
      { CU_SOURCE_TABLE_SCOPE: ' '.repeat(4097) },
      { CU_SOURCE_READ_WINDOW_ID: 'window-01' },
      { CU_SOURCE_READ_WINDOW_ID: 'window-9 ' },
    ]) {
      assert.throws(() => nativeSourceFromEnvironment({
        CU_SOURCE_BACKEND: 'native',
        CU_TARGET_BUNDLE_ID: 'com.lingxi.computer-user.mvp-fixture',
        CU_TARGET_PID: '42',
        CU_SOURCE_READ_WINDOW_ID: 'window-9',
        CU_SOURCE_TABLE_SCOPE: JSON.stringify(tableScope),
        ...env,
      }, f.binaries));
    }
    assert.throws(() => nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: 'unregistered.app', CU_TARGET_PID: '42',
      CU_SOURCE_READ_WINDOW_ID: 'window-9', CU_SOURCE_TABLE_SCOPE: JSON.stringify(tableScope),
    }, f.binaries), /Source read policy/);
    assert.throws(() => nativeSourceFromEnvironment({
      CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: 'com.lingxi.computer-user.mvp-fixture',
      CU_SOURCE_TABLE_SCOPE: JSON.stringify(tableScope),
    }, f.binaries), /requires a Source-only/);
  } finally { f.dispose(); }
});

test('reference planning is explicit, registered-fixture-only, and never authorizes a window', () => {
  const f = fixture();
  try {
    for (const bundleId of ['com.lingxi.computer-user.guard-fixture', 'com.lingxi.computer-user.mvp-fixture']) {
      const native = nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: bundleId,
        CU_NATIVE_REFERENCE_PLANNER: '1' }, f.binaries);
      assert.equal(native?.referencePlanner, true);
      assert.equal(native?.authorizedWindowId, undefined);
    }
    const mvpWithoutExplicitPlanner = nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'native',
      CU_TARGET_BUNDLE_ID: 'com.lingxi.computer-user.mvp-fixture' }, f.binaries);
    assert.equal(mvpWithoutExplicitPlanner?.referencePlanner, undefined);
    for (const bundleId of ['com.apple.TextEdit', 'com.lingxi.fixture', 'unregistered.app']) {
      assert.throws(() => nativeSourceFromEnvironment({ CU_SOURCE_BACKEND: 'native', CU_TARGET_BUNDLE_ID: bundleId,
        CU_NATIVE_REFERENCE_PLANNER: '1' }, f.binaries), /registered owned fixtures/);
    }
  } finally { f.dispose(); }
});
