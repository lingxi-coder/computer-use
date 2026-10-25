import assert from 'node:assert/strict';
import test from 'node:test';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureBrandIdentity } from '../src/main/brandIdentity.js';

test('renaming the app keeps the established account and task directory', (t) => {
  const appData = mkdtempSync(join(tmpdir(), 'control-brand-existing-'));
  t.after(() => rmSync(appData, { recursive: true, force: true }));
  const established = join(appData, 'Computer Use');
  mkdirSync(established);
  const marker = join(established, 'task-history.json');
  writeFileSync(marker, '{"existing":true}');
  for (const initialName of ['Computer Use', 'LingXi Control']) {
    let name = initialName;
    let userData = join(appData, name);
    configureBrandIdentity({
      getName: () => name,
      getPath: (key) => key === 'appData' ? appData : userData,
      setName: (value) => { name = value; },
      setPath: (_key, value) => { userData = value; },
    });
    assert.equal(name, 'LingXi Control');
    assert.equal(userData, established);
    assert.equal(readFileSync(marker, 'utf8'), '{"existing":true}');
  }
});

test('first launch creates the data directory before passing it to Electron', (t) => {
  const root = mkdtempSync(join(tmpdir(), 'control-brand-fresh-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const appData = join(root, 'application-data');
  const established = join(appData, 'Computer Use');
  assert.equal(existsSync(established), false);
  let selected: string | undefined;
  configureBrandIdentity({
    getName: () => 'LingXi Control',
    getPath: (key) => key === 'appData' ? appData : join(appData, 'LingXi Control'),
    setName: () => undefined,
    setPath: (_key, value) => {
      // Match Electron's precondition rather than accepting a nonexistent path.
      assert.equal(statSync(value).isDirectory(), true);
      selected = value;
    },
  });
  assert.equal(selected, established);
  assert.equal(existsSync(join(appData, 'LingXi Control')), false);
});

test('branding preserves an explicit isolated diagnostic profile without creating a default profile', (t) => {
  const root = mkdtempSync(join(tmpdir(), 'control-brand-diagnostic-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const appData = join(root, 'application-data');
  const profile = join(root, 'isolated-profile');
  mkdirSync(profile);
  const directories: string[] = [];
  configureBrandIdentity({
    getName: () => 'Diagnostic',
    getPath: (key) => key === 'appData' ? appData : profile,
    setName: () => undefined,
    setPath: (_key, value) => { directories.push(value); },
  });
  assert.deepEqual(directories, []);
  assert.equal(existsSync(appData), false);
  assert.equal(statSync(profile).isDirectory(), true);
});
