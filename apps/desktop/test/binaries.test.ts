import assert from 'node:assert/strict';
import { chmodSync, mkdtempSync, mkdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { resolveBinaries } from '../src/main/binaries.js';

const names = ['cu-host', 'cu-device', 'cu-source'];

function makeBinaries(directory: string): void {
  mkdirSync(directory, { recursive: true });
  for (const name of names) writeFileSync(join(directory, name), 'test executable', { mode: 0o700 });
}

test('installed apps choose packaged binaries and reject environment or checkout fallback', () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'cu-packaged-binaries-')));
  try {
    const resources = join(root, 'Resources');
    const override = join(root, 'override');
    makeBinaries(override);
    assert.throws(() => resolveBinaries({ CU_BIN_DIR: override }, resources, true), /not found/);
    makeBinaries(join(resources, 'bin'));
    assert.equal(resolveBinaries({ CU_BIN_DIR: override }, resources, true).host, join(resources, 'bin/cu-host'));
    rmSync(join(resources, 'bin/cu-source'));
    assert.throws(() => resolveBinaries({ CU_BIN_DIR: override }, resources, true), /not found/);
    assert.equal(resolveBinaries({ CU_BIN_DIR: override }).host, join(override, 'cu-host'));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('installed binary resources reject nonexecutables and symlink escapes', () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'cu-packaged-binaries-')));
  try {
    const resources = join(root, 'Resources');
    const bin = join(resources, 'bin');
    makeBinaries(bin);
    const source = join(bin, 'cu-source');
    chmodSync(source, 0o600);
    assert.throws(() => resolveBinaries({}, resources, true), /not found/);
    chmodSync(source, 0o700);
    const external = join(root, 'external');
    writeFileSync(external, 'external', { mode: 0o700 });
    rmSync(source);
    symlinkSync(external, source);
    assert.throws(() => resolveBinaries({}, resources, true), /not found/);
    rmSync(bin, { recursive: true });
    const externalDir = join(root, 'external-dir');
    makeBinaries(externalDir);
    symlinkSync(externalDir, bin);
    assert.throws(() => resolveBinaries({}, resources, true), /not found/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
