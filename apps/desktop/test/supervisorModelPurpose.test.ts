import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { modelPlanningBackend, Supervisor, validateNativeReferencePlanningOptions } from '../src/main/supervisor.js';

const authorization = (purposes: ('s2_planning' | 's1_teacher')[]) => ({
  route: {
    provider_id: 'open_ai' as const,
    protocol: 'open_ai_responses' as const,
    base_url: 'https://example.invalid/v1',
    model: 'fixture-model',
    max_output_tokens: 4096,
  },
  credential: { kind: 'api_key' as const, api_key: 'purpose-boundary-test-key' },
  purposes,
});

test('only an explicit S2 purpose selects the model planning backend', () => {
  assert.equal(modelPlanningBackend(undefined), undefined);
  assert.equal(modelPlanningBackend(authorization(['s1_teacher'])), undefined);
  assert.equal(modelPlanningBackend(authorization(['s2_planning'])), 'model');
  assert.equal(modelPlanningBackend(authorization(['s1_teacher', 's2_planning'])), 'model');
});

test('native reference planning excludes S2 but permits an S1-only purpose', () => {
  const nativeSource = {
    helperPath: '/private/tmp/cu-native-reference-helper',
    bundleId: 'com.lingxi.computer-user.mvp-fixture',
    referencePlanner: true,
  };
  assert.doesNotThrow(() => validateNativeReferencePlanningOptions({
    nativeSource,
    modelAuthorization: authorization(['s1_teacher']),
  }));
  assert.equal(modelPlanningBackend(authorization(['s1_teacher'])), undefined);
  assert.throws(() => validateNativeReferencePlanningOptions({
    nativeSource,
    modelAuthorization: authorization(['s2_planning']),
  }), /cannot be combined with S2 model planning/);
  assert.throws(() => validateNativeReferencePlanningOptions({
    nativeSource,
    modelAuthorization: authorization(['s1_teacher', 's2_planning']),
  }), /cannot be combined with S2 model planning/);
  assert.throws(() => validateNativeReferencePlanningOptions({
    nativeSource: { ...nativeSource, bundleId: 'com.lingxi.computer-user.guard-fixture', pid: 42, backgroundWindowId: 'window-9' },
    modelAuthorization: authorization(['s2_planning']),
  }), /cannot be combined with S2 model planning/);
  assert.doesNotThrow(() => validateNativeReferencePlanningOptions({
    nativeSource: { ...nativeSource, referencePlanner: false },
    modelAuthorization: authorization(['s2_planning']),
  }));
});

test('Supervisor rejects the conflicting launch before any peer starts or receives the model key', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'cu-reference-s2-gate-'));
  const marker = join(dir, 'peer-started');
  const executable = (name: string): string => {
    const path = join(dir, name);
    writeFileSync(path,
      `#!/usr/bin/env node\nrequire('node:fs').appendFileSync(${JSON.stringify(marker)}, ${JSON.stringify(`${name} started\n`)});\n`,
      { mode: 0o700 });
    return path;
  };
  const key = 'reference-s2-isolation-secret-must-not-reach-a-peer';
  const source = executable('cu-source');
  try {
    await assert.rejects(Supervisor.start({
      binaries: { host: executable('cu-host'), device: executable('cu-device'), source },
      nativeSource: {
        helperPath: source,
        bundleId: 'com.lingxi.computer-user.guard-fixture',
        pid: 42,
        backgroundWindowId: 'window-9',
        referencePlanner: true,
      },
      modelAuthorization: { ...authorization(['s2_planning']), credential: { kind: 'api_key', api_key: key } },
    }), error => {
      assert.match(String(error), /cannot be combined with S2 model planning/);
      assert.equal(String(error).includes(key), false);
      return true;
    });
    assert.equal(existsSync(marker), false, 'the gate runs before device, source, or Host peer startup');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
