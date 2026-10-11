import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { test } from 'node:test';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { fixtureBackgroundOptionSupervisorOptions } from '../src/main/browserProfile.js';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');

test('the fixture profile resolves only fixed Chrome and local fixture paths', () => {
  const options = fixtureBackgroundOptionSupervisorOptions({
    isPackaged: false,
    resourcesPath: '/app/resources',
    moduleDirectory: join(repoRoot, 'apps/desktop/out/main'),
    platform: 'darwin',
    homeDirectory: '/Users/operator',
    exists: (path) => path === '/Users/operator/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  });
  assert.deepEqual(options, {
    browserPageProfile: { kind: 'fixture_background_option' },
    browserSource: {
      chromeExecutable: '/Users/operator/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
      fixturePath: join(repoRoot, 'fixtures', 'headless', 'readonly-page.html'),
    },
  });
});

test('packaged fixture resolution uses the app resources directory', () => {
  const resourcesPath = mkdtempSync(join(realpathSync(tmpdir()), 'browser-profile-resources-'));
  try {
    const fixturePath = join(resourcesPath, 'fixtures', 'headless', 'readonly-page.html');
    mkdirSync(dirname(fixturePath), { recursive: true });
    writeFileSync(fixturePath, '<!doctype html><title>fixed fixture</title>');
    const options = fixtureBackgroundOptionSupervisorOptions({
      isPackaged: true,
      resourcesPath,
      moduleDirectory: '/unused',
      platform: 'darwin',
      homeDirectory: '/Users/operator',
      exists: (path) => path === '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    });
    assert.equal(options.browserSource.fixturePath, fixturePath);
  } finally {
    rmSync(resourcesPath, { recursive: true, force: true });
  }
});

test('the fixed product browser profile refuses unsupported platforms or missing fixed Chrome installs', () => {
  const base = {
    isPackaged: false,
    resourcesPath: '/app/resources',
    moduleDirectory: '/checkout/apps/desktop/out/main',
    homeDirectory: '/Users/operator',
    exists: () => false,
  };
  assert.throws(() => fixtureBackgroundOptionSupervisorOptions({ ...base, platform: 'linux' }), /only on macOS/);
  assert.throws(() => fixtureBackgroundOptionSupervisorOptions({ ...base, platform: 'darwin' }), /Google Chrome/);
});

test('packaged profile fails closed when the fixture asset is missing or non-canonical', () => {
  const resourcesPath = mkdtempSync(join(realpathSync(tmpdir()), 'browser-profile-invalid-'));
  try {
    const runtime = {
      isPackaged: true,
      resourcesPath,
      moduleDirectory: '/unused',
      platform: 'darwin' as const,
      homeDirectory: '/Users/operator',
      exists: (path: string) => path === '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    };
    assert.throws(() => fixtureBackgroundOptionSupervisorOptions(runtime), /fixture browser page is missing/);

    const asset = join(resourcesPath, 'fixtures', 'headless', 'readonly-page.html');
    const target = join(resourcesPath, 'source.html');
    mkdirSync(dirname(asset), { recursive: true });
    writeFileSync(target, '<!doctype html><title>fixed fixture</title>');
    symlinkSync(target, asset);
    assert.throws(() => fixtureBackgroundOptionSupervisorOptions(runtime), /regular canonical asset/);
  } finally {
    rmSync(resourcesPath, { recursive: true, force: true });
  }
});
