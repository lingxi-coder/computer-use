import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import { HeadlessBrowserError } from '../src/main/headless/cdpPipe.js';
import { launchOwnedBrowser } from '../src/main/headless/ownedBrowser.js';

function isInvalidInput(error: unknown): boolean {
  return error instanceof HeadlessBrowserError && error.code === 'invalid_input';
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

test('launchOwnedBrowser rejects symlink and extra paths before invoking the executable', async (t) => {
  const root = mkdtempSync(join(tmpdir(), 'cu-headless-path-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));

  const profileParent = join(root, 'profiles');
  const fixture = join(root, 'fixture.html');
  const chrome = join(root, 'fake-chrome');
  const marker = join(root, 'chrome-was-started');
  mkdirSync(profileParent);
  writeFileSync(fixture, '<!doctype html><title>fixture</title>');
  writeFileSync(chrome, `#!/bin/sh\nprintf launched > ${shellQuote(marker)}\nexit 0\n`);
  chmodSync(chrome, 0o700);

  const fixtureLink = join(root, 'fixture-link.html');
  const chromeLink = join(root, 'chrome-link');
  const profileLink = join(root, 'profiles-link');
  symlinkSync(fixture, fixtureLink);
  symlinkSync(chrome, chromeLink);
  symlinkSync(profileParent, profileLink);

  const base = { chromeExecutable: chrome, fixturePath: fixture, profileParent };
  const cases = [
    { name: 'fixture symlink', options: { ...base, fixturePath: fixtureLink } },
    { name: 'executable symlink', options: { ...base, chromeExecutable: chromeLink } },
    { name: 'profile parent symlink', options: { ...base, profileParent: profileLink } },
    { name: 'extra user profile field', options: { ...base, userDataDir: root } },
  ];

  for (const item of cases) {
    await assert.rejects(launchOwnedBrowser(item.options), isInvalidInput, item.name);
    assert.equal(existsSync(marker), false, `${item.name} must be rejected before Chrome starts`);
    assert.deepEqual(readdirSync(profileParent), [], `${item.name} must not leave an owned profile behind`);
  }
});
