import assert from 'node:assert/strict';
import {
  chmodSync,
  linkSync,
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import {
  loadFrozenTaskCatalog,
  type FrozenTaskCatalogUnavailableReason,
} from '../src/main/frozenTaskCatalog.js';

const FIXTURE_IDS = [
  ...Array.from({ length: 6 }, (_, index) => `SET-${String(index + 1).padStart(2, '0')}`),
  ...Array.from({ length: 7 }, (_, index) => `FORM-${String(index + 1).padStart(2, '0')}`),
  ...Array.from({ length: 4 }, (_, index) => `LIST-${String(index + 1).padStart(2, '0')}`),
  ...Array.from({ length: 3 }, (_, index) => `MODAL-${String(index + 1).padStart(2, '0')}`),
];
const TEXTEDIT_IDS = Array.from({ length: 20 }, (_, index) => `TEXTEDIT-${String(index + 1).padStart(2, '0')}`);
const SOURCE_FREEZE_SHA256 = 'a'.repeat(64);
const OUTPUT_DIRECTORY = '/private/tmp/frozen-task-catalog-never-open';

interface TestArtifact {
  readonly goals: Buffer;
  readonly manifest: Buffer;
  readonly goalsSha256: string;
  readonly manifestSha256: string;
}

function stableJson(value: unknown): string {
  const canonical = (item: unknown): string => {
    if (Array.isArray(item)) return `[${item.map(canonical).join(',')}]`;
    if (item !== null && typeof item === 'object') {
      const record = item as Record<string, unknown>;
      return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${canonical(record[key])}`).join(',')}}`;
    }
    return JSON.stringify(item);
  };
  return `${canonical(value)}\n`;
}

function sha256(value: string | Buffer): string {
  return createHash('sha256').update(value).digest('hex');
}

function targetFor(taskId: string): Record<string, string> {
  if (taskId.startsWith('SET-')) {
    const group = `Group ${taskId}`;
    return taskId === 'SET-06'
      ? { control: `Control ${taskId}`, group, disambiguator: group }
      : { control: `Control ${taskId}`, group };
  }
  if (taskId.startsWith('FORM-')) return { control: `Control ${taskId}`, form: `Form ${taskId}` };
  if (taskId === 'LIST-02') return { list: `List ${taskId}`, target: `Target ${taskId}`, context: `Context ${taskId}` };
  if (taskId.startsWith('LIST-')) return { list: `List ${taskId}`, target: `Target ${taskId}` };
  if (taskId === 'MODAL-02') return { modal: `Modal ${taskId}`, control: `Control ${taskId}` };
  if (taskId.startsWith('MODAL-')) return { modal: `Modal ${taskId}`, background: `Background ${taskId}` };
  throw new Error(`unexpected fixture id ${taskId}`);
}

function goalFor(suite: 'fixture' | 'textedit', taskId: string, seed: number) {
  if (suite === 'textedit') {
    const fileName = `cu-mvp-${taskId.toLowerCase()}.txt`;
    return {
      objective: `Save “${OUTPUT_DIRECTORY}/${fileName}” as plain text and read it back.`,
      allowed_scope: {
        application: 'TextEdit',
        output_directory: OUTPUT_DIRECTORY,
        file_name: fileName,
        document_format: 'plain_text',
        operations: ['write_requested_contents', 'save', 'close', 'reopen_and_read_back'],
        no_other_files: true,
      },
    };
  }
  return {
    objective: `Frozen objective for ${taskId}, seed ${seed}.`,
    allowed_scope: {
      application: 'Interaction Fixture',
      targets: [targetFor(taskId)],
      no_unrelated_controls: true,
    },
  };
}

function recordsFor(): Record<string, unknown>[] {
  const identities = [
    ...FIXTURE_IDS.flatMap((taskId) => [1, 2, 3, 4, 5].map((seed) => ({ suite: 'fixture', task_id: taskId, seed }))),
    ...TEXTEDIT_IDS.map((taskId) => ({ suite: 'textedit', task_id: taskId, seed: 1 })),
  ];
  return identities.map((identity) => {
    const goal = goalFor(identity.suite as 'fixture' | 'textedit', identity.task_id, identity.seed);
    const goalSha256 = sha256(stableJson(goal));
    const selectorInput = `computer-use-frozen-task-selector-v1\n${SOURCE_FREEZE_SHA256}\n${identity.suite}\n${identity.task_id}\n${identity.seed}\n${goalSha256}`;
    return { selector: sha256(selectorInput), ...identity, goal };
  });
}

function makeArtifact(
  mutateGoals?: (goals: Record<string, unknown>) => void,
  mutateManifest?: (manifest: Record<string, unknown>) => void,
): TestArtifact {
  const goals: Record<string, unknown> = {
    format_version: 1,
    task_profile_version: 1,
    records: recordsFor(),
  };
  mutateGoals?.(goals);
  const records = goals['records'] as Record<string, unknown>[];
  for (const record of records) {
    const goal = record['goal'];
    if (!goal || typeof goal !== 'object' || Array.isArray(goal)) continue;
    const goalHash = sha256(stableJson(goal));
    const selectorInput = `computer-use-frozen-task-selector-v1\n${SOURCE_FREEZE_SHA256}\n${record['suite']}\n${record['task_id']}\n${record['seed']}\n${goalHash}`;
    record['selector'] = sha256(selectorInput);
  }
  const goalBytes = Buffer.from(stableJson(goals), 'utf8');
  const identityTriples = records.map((record) => [record['suite'], record['task_id'], record['seed']]);
  const manifest: Record<string, unknown> = {
    format_version: 1,
    task_profile_version: 1,
    source_freeze_sha256: SOURCE_FREEZE_SHA256,
    source_freeze_content_sha256: 'b'.repeat(64),
    source_request_sha256: 'c'.repeat(64),
    task_goals_sha256: sha256(goalBytes),
    identities_sha256: sha256(stableJson(identityTriples)),
    identity_count: 120,
    denominators: { fixture_runs: 100, textedit_runs: 20, total_records: 120 },
    selector_scheme: 'sha256/domain-separated-v1',
  };
  mutateManifest?.(manifest);
  const manifestBytes = Buffer.from(stableJson(manifest), 'utf8');
  return {
    goals: goalBytes,
    manifest: manifestBytes,
    goalsSha256: sha256(goalBytes),
    manifestSha256: sha256(manifestBytes),
  };
}

function makeDirectory(): string {
  const parent = realpathSync(tmpdir());
  const directory = mkdtempSync(join(parent, 'frozen-task-catalog-'));
  chmodSync(directory, 0o700);
  return directory;
}

function writeArtifact(directory: string, artifact: TestArtifact): { goalsPath: string; manifestPath: string } {
  const goalsPath = join(directory, 'task-goals.json');
  const manifestPath = join(directory, 'goal-catalog-manifest.json');
  writeFileSync(goalsPath, artifact.goals, { mode: 0o600, flag: 'wx' });
  writeFileSync(manifestPath, artifact.manifest, { mode: 0o600, flag: 'wx' });
  chmodSync(goalsPath, 0o600);
  chmodSync(manifestPath, 0o600);
  return { goalsPath, manifestPath };
}

function environment(directory: string, artifact: TestArtifact): NodeJS.ProcessEnv {
  return {
    CU_FROZEN_TASK_GOALS_PATH: join(directory, 'task-goals.json'),
    CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH: join(directory, 'goal-catalog-manifest.json'),
    CU_FROZEN_TASK_GOALS_SHA256: artifact.goalsSha256,
    CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256: artifact.manifestSha256,
  };
}

function expectUnavailable(
  env: NodeJS.ProcessEnv,
  reason: FrozenTaskCatalogUnavailableReason,
): void {
  const catalog = loadFrozenTaskCatalog(env);
  assert.deepEqual(catalog.status(), { state: 'unavailable', reason });
  assert.deepEqual(catalog.list(), []);
}

test('unconfigured catalog is empty; partial or noncanonical environment is explicitly unavailable', () => {
  assert.deepEqual(loadFrozenTaskCatalog({}).status(), { state: 'empty' });
  expectUnavailable({ CU_FROZEN_TASK_GOALS_PATH: '/no/such/task-goals.json' }, 'configuration_incomplete');

  const artifact = makeArtifact();
  const directory = makeDirectory();
  try {
    writeArtifact(directory, artifact);
    const env = environment(directory, artifact);
    env['CU_FROZEN_TASK_GOALS_PATH'] = `${directory}/nested/../task-goals.json`;
    expectUnavailable(env, 'configuration_invalid');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('catalog accepts only the complete pinned 120 identities and returns immutable scoped goals privately', () => {
  const artifact = makeArtifact();
  const directory = makeDirectory();
  try {
    const paths = writeArtifact(directory, artifact);
    // A Main catalog read is intentionally independent of the original frozen request/reset/truth files.
    symlinkSync('/does/not/exist', join(directory, 'task-requests.json'));
    symlinkSync('/does/not/exist', join(directory, 'evaluator-truth.json'));
    symlinkSync('/does/not/exist', join(directory, 'task-profile-freeze.json'));
    const catalog = loadFrozenTaskCatalog(environment(directory, artifact));
    assert.deepEqual(catalog.status(), { state: 'available' });
    assert.equal(catalog.list().length, 120);
    assert.deepEqual(catalog.list().map(({ suite, task_id, seed }) => ({ suite, task_id, seed: Number(seed) })), [
      ...FIXTURE_IDS.flatMap((task_id) => [1, 2, 3, 4, 5].map((seed) => ({ suite: 'fixture', task_id, seed }))),
      ...TEXTEDIT_IDS.map((task_id) => ({ suite: 'textedit', task_id, seed: 1 })),
    ]);
    assert.equal(catalog.list().filter((entry) => entry.scope_compatible).length, 65);
    assert.equal(catalog.list().filter((entry) => entry.scope_family === 'form').length, 35);
    assert.ok(catalog.list().filter((entry) => entry.scope_family === 'form')
      .every((entry) => entry.scope_compatible && entry.unsupported_reason === undefined));
    assert.equal(catalog.list().filter((entry) => entry.scope_family === 'list').length, 20);
    assert.equal(catalog.list().filter((entry) => entry.scope_family === 'modal').length, 15);
    assert.equal(catalog.list().filter((entry) => entry.scope_family === 'textedit').length, 20);
    assert.ok(Object.isFrozen(catalog.list()) && Object.isFrozen(catalog.list()[0]));
    assert.ok(catalog.list().every((entry) => !('objective' in entry) && !('allowed_scope' in entry)));
    assert.ok(catalog.list().every((entry) => !JSON.stringify(entry).includes(OUTPUT_DIRECTORY)));

    const set02 = catalog.list().find((entry) => entry.task_id === 'SET-02' && entry.seed === '3');
    assert.ok(set02);
    const resolved = catalog.resolve({ catalog_id: set02.catalog_id, selector: set02.selector });
    assert.ok(resolved && resolved.status === 'available');
    assert.equal(resolved.goal.objective, 'Frozen objective for SET-02, seed 3.');
    assert.deepEqual(resolved.goal.allowed_scope, {
      application: 'Interaction Fixture',
      targets: [{ control: 'Control SET-02', group: 'Group SET-02' }],
      no_unrelated_controls: true,
    });
    assert.equal(resolved.profile.goals_sha256, artifact.goalsSha256);
    assert.equal(resolved.profile.source_freeze_sha256, SOURCE_FREEZE_SHA256);
    assert.equal(resolved.profile.seed, 3);
    assert.equal(resolved.profile.catalog_id, artifact.manifestSha256);
    assert.ok(Object.isFrozen(resolved) && Object.isFrozen(resolved.goal)
      && Object.isFrozen(resolved.goal.allowed_scope) && Object.isFrozen(resolved.goal.allowed_scope.targets)
      && Object.isFrozen(resolved.goal.allowed_scope.targets[0]) && Object.isFrozen(resolved.profile));

    const form = catalog.list().find((entry) => entry.task_id === 'FORM-01' && entry.seed === '1');
    assert.ok(form?.scope_compatible);
    assert.equal(form?.unsupported_reason, undefined);
    const formResolution = catalog.resolve({ catalog_id: form!.catalog_id, selector: form!.selector });
    assert.ok(formResolution && formResolution.status === 'available');
    assert.deepEqual(formResolution.goal.allowed_scope, {
      application: 'Interaction Fixture',
      targets: [{ control: 'Control FORM-01', form: 'Form FORM-01' }],
      no_unrelated_controls: true,
    });
    assert.equal(formResolution.profile.task_id, 'FORM-01');
    assert.ok(Object.isFrozen(formResolution.goal.allowed_scope.targets[0]));

    const textEdit = catalog.list().find((entry) => entry.task_id === 'TEXTEDIT-01');
    const textEditResolution = catalog.resolve({ catalog_id: textEdit!.catalog_id, selector: textEdit!.selector });
    assert.ok(textEditResolution && textEditResolution.status === 'unsupported');
    assert.equal(textEditResolution.reason, 'textedit_scope_not_supported');
    assert.equal(JSON.stringify(textEditResolution).includes('output_directory'), false);

    for (const [taskId, expectedReason] of [
      ['LIST-01', 'list_scope_not_supported'],
      ['MODAL-01', 'modal_scope_not_supported'],
    ] as const) {
      const entry = catalog.list().find((candidate) => candidate.task_id === taskId && candidate.seed === '1');
      const resolution = catalog.resolve({ catalog_id: entry!.catalog_id, selector: entry!.selector });
      assert.ok(resolution && resolution.status === 'unsupported');
      assert.equal(resolution.reason, expectedReason);
      assert.equal('goal' in resolution, false);
    }

    assert.equal(catalog.resolve({ catalog_id: '0'.repeat(64), selector: set02.selector }), null);
    assert.equal(catalog.resolve({ catalog_id: set02.catalog_id, selector: 'not-an-opaque-selector' }), null);
    assert.deepEqual(catalog.snapshot().tasks, catalog.list());
    assert.ok(Object.isFrozen(catalog.snapshot()) && Object.isFrozen(catalog.snapshot().status));
    assert.deepEqual(paths, {
      goalsPath: join(directory, 'task-goals.json'),
      manifestPath: join(directory, 'goal-catalog-manifest.json'),
    });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('manifest and goals hashes, selector derivation, identity order, and exact schemas are all bound', () => {
  const directory = makeDirectory();
  try {
    const valid = makeArtifact();
    writeArtifact(directory, valid);
    const env = environment(directory, valid);
    const goalsPath = env['CU_FROZEN_TASK_GOALS_PATH']!;
    const manifestPath = env['CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH']!;

    writeFileSync(goalsPath, Buffer.concat([valid.goals, Buffer.from(' ', 'utf8')]));
    expectUnavailable(env, 'catalog_hash_mismatch');

    const wrongManifestBinding = makeArtifact(undefined, (manifest) => { manifest['task_goals_sha256'] = 'd'.repeat(64); });
    writeFileSync(goalsPath, wrongManifestBinding.goals);
    writeFileSync(manifestPath, wrongManifestBinding.manifest);
    env['CU_FROZEN_TASK_GOALS_SHA256'] = wrongManifestBinding.goalsSha256;
    env['CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256'] = wrongManifestBinding.manifestSha256;
    expectUnavailable(env, 'catalog_hash_mismatch');

    const badManifestKeys = makeArtifact(undefined, (manifest) => { manifest['unexpected'] = true; });
    writeFileSync(goalsPath, badManifestKeys.goals);
    writeFileSync(manifestPath, badManifestKeys.manifest);
    env['CU_FROZEN_TASK_GOALS_SHA256'] = badManifestKeys.goalsSha256;
    env['CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256'] = badManifestKeys.manifestSha256;
    expectUnavailable(env, 'catalog_manifest_invalid');

    const canonicalArtifact = makeArtifact();
    const parsedGoals = JSON.parse(canonicalArtifact.goals.toString('utf8')) as Record<string, unknown>;
    const noncanonicalGoals = Buffer.from(`${JSON.stringify(parsedGoals, null, 1)}\n`, 'utf8');
    const noncanonicalManifestObject = JSON.parse(canonicalArtifact.manifest.toString('utf8')) as Record<string, unknown>;
    noncanonicalManifestObject['task_goals_sha256'] = sha256(noncanonicalGoals);
    const noncanonicalManifest = Buffer.from(stableJson(noncanonicalManifestObject), 'utf8');
    writeFileSync(goalsPath, noncanonicalGoals);
    writeFileSync(manifestPath, noncanonicalManifest);
    env['CU_FROZEN_TASK_GOALS_SHA256'] = sha256(noncanonicalGoals);
    env['CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256'] = sha256(noncanonicalManifest);
    expectUnavailable(env, 'catalog_goals_invalid');

    const duplicate = makeArtifact((goals) => {
      const records = goals['records'] as Record<string, unknown>[];
      records[1] = structuredClone(records[0]!);
    });
    writeFileSync(goalsPath, duplicate.goals);
    writeFileSync(manifestPath, duplicate.manifest);
    env['CU_FROZEN_TASK_GOALS_SHA256'] = duplicate.goalsSha256;
    env['CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256'] = duplicate.manifestSha256;
    expectUnavailable(env, 'catalog_goals_invalid');

    const missing = makeArtifact((goals) => { (goals['records'] as unknown[]).pop(); });
    writeFileSync(goalsPath, missing.goals);
    writeFileSync(manifestPath, missing.manifest);
    env['CU_FROZEN_TASK_GOALS_SHA256'] = missing.goalsSha256;
    env['CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256'] = missing.manifestSha256;
    expectUnavailable(env, 'catalog_goals_invalid');

    const goodSelectorArtifact = makeArtifact();
    const malformedGoals = JSON.parse(goodSelectorArtifact.goals.toString('utf8')) as Record<string, unknown>;
    const malformedRecords = malformedGoals['records'] as Record<string, unknown>[];
    malformedRecords[0]!['selector'] = 'f'.repeat(64);
    const malformedGoalBytes = Buffer.from(stableJson(malformedGoals), 'utf8');
    const malformedManifest = JSON.parse(goodSelectorArtifact.manifest.toString('utf8')) as Record<string, unknown>;
    malformedManifest['task_goals_sha256'] = sha256(malformedGoalBytes);
    const malformedManifestBytes = Buffer.from(stableJson(malformedManifest), 'utf8');
    const badSelector: TestArtifact = {
      goals: malformedGoalBytes,
      manifest: malformedManifestBytes,
      goalsSha256: sha256(malformedGoalBytes),
      manifestSha256: sha256(malformedManifestBytes),
    };
    writeFileSync(goalsPath, badSelector.goals);
    writeFileSync(manifestPath, badSelector.manifest);
    env['CU_FROZEN_TASK_GOALS_SHA256'] = badSelector.goalsSha256;
    env['CU_FROZEN_GOAL_CATALOG_MANIFEST_SHA256'] = badSelector.manifestSha256;
    expectUnavailable(env, 'catalog_goals_invalid');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('bounded UTF-8 objective and scope text are rejected when oversized without trimming frozen values', () => {
  const directory = makeDirectory();
  try {
    const objectiveTooLarge = makeArtifact((goals) => {
      const records = goals['records'] as Record<string, unknown>[];
      const firstGoal = records[0]!['goal'] as Record<string, unknown>;
      firstGoal['objective'] = '界'.repeat(10_923); // 32,769 UTF-8 bytes.
    });
    writeArtifact(directory, objectiveTooLarge);
    expectUnavailable(environment(directory, objectiveTooLarge), 'catalog_goals_invalid');

    const scopeTooLarge = makeArtifact((goals) => {
      const records = goals['records'] as Record<string, unknown>[];
      const firstGoal = records[0]!['goal'] as Record<string, unknown>;
      const scope = firstGoal['allowed_scope'] as Record<string, unknown>;
      const targets = scope['targets'] as Record<string, unknown>[];
      targets[0]!['control'] = 'é'.repeat(129); // 258 UTF-8 bytes, over producer's 256-byte bound.
    });
    writeFileSync(join(directory, 'task-goals.json'), scopeTooLarge.goals);
    writeFileSync(join(directory, 'goal-catalog-manifest.json'), scopeTooLarge.manifest);
    chmodSync(join(directory, 'task-goals.json'), 0o600);
    chmodSync(join(directory, 'goal-catalog-manifest.json'), 0o600);
    expectUnavailable(environment(directory, scopeTooLarge), 'catalog_goals_invalid');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('widened control scopes, extra target fields, and mismatched SET-06 disambiguation are rejected', () => {
  const directory = makeDirectory();
  try {
    const goalsPath = join(directory, 'task-goals.json');
    const manifestPath = join(directory, 'goal-catalog-manifest.json');
    for (const mutate of [
      (scope: Record<string, unknown>) => { scope['no_unrelated_controls'] = false; },
      (scope: Record<string, unknown>) => {
        const targets = scope['targets'] as unknown[];
        targets.push(structuredClone(targets[0]!));
      },
      (_scope: Record<string, unknown>, target: Record<string, unknown>) => { target['another_control'] = 'outside scope'; },
    ]) {
      const artifact = makeArtifact((goals) => {
        const first = (goals['records'] as Record<string, unknown>[])[0]!;
        const goal = first['goal'] as Record<string, unknown>;
        const scope = goal['allowed_scope'] as Record<string, unknown>;
        const target = (scope['targets'] as Record<string, unknown>[])[0]!;
        mutate(scope, target);
      });
      writeFileSync(goalsPath, artifact.goals);
      writeFileSync(manifestPath, artifact.manifest);
      chmodSync(goalsPath, 0o600);
      chmodSync(manifestPath, 0o600);
      expectUnavailable(environment(directory, artifact), 'catalog_goals_invalid');
    }

    const mismatchedDisambiguator = makeArtifact((goals) => {
      const target = (((goals['records'] as Record<string, unknown>[])[25]!['goal'] as Record<string, unknown>)
        ['allowed_scope'] as Record<string, unknown>)['targets'] as Record<string, unknown>[];
      target[0]!['disambiguator'] = 'a different group';
    });
    writeFileSync(goalsPath, mismatchedDisambiguator.goals);
    writeFileSync(manifestPath, mismatchedDisambiguator.manifest);
    chmodSync(goalsPath, 0o600);
    chmodSync(manifestPath, 0o600);
    expectUnavailable(environment(directory, mismatchedDisambiguator), 'catalog_goals_invalid');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('rejects symlinked, hard-linked, nonprivate, changed, and cross-directory catalog files', () => {
  const artifact = makeArtifact();
  const directory = makeDirectory();
  try {
    const paths = writeArtifact(directory, artifact);

    const badMode = environment(directory, artifact);
    chmodSync(paths.goalsPath, 0o644);
    expectUnavailable(badMode, 'catalog_files_unsafe');
    chmodSync(paths.goalsPath, 0o600);

    chmodSync(directory, 0o755);
    expectUnavailable(environment(directory, artifact), 'catalog_files_unsafe');
    chmodSync(directory, 0o700);

    const hardlink = join(directory, 'goals-hardlink');
    linkSync(paths.goalsPath, hardlink);
    expectUnavailable(environment(directory, artifact), 'catalog_files_unsafe');
    rmSync(hardlink);

    const otherDirectory = join(directory, 'other');
    mkdirSync(otherDirectory, { mode: 0o700 });
    chmodSync(otherDirectory, 0o700);
    const crossDirectoryEnv = environment(directory, artifact);
    crossDirectoryEnv['CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH'] = join(otherDirectory, 'goal-catalog-manifest.json');
    writeFileSync(crossDirectoryEnv['CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH'], artifact.manifest, { mode: 0o600 });
    expectUnavailable(crossDirectoryEnv, 'configuration_invalid');

    const symlinkedDirectory = join(directory, 'linked');
    symlinkSync(directory, symlinkedDirectory);
    const symlinkEnv = environment(symlinkedDirectory, artifact);
    expectUnavailable(symlinkEnv, 'catalog_files_unsafe');

    const symlinkFileDirectory = join(directory, 'symlink-file-case');
    mkdirSync(symlinkFileDirectory, { mode: 0o700 });
    chmodSync(symlinkFileDirectory, 0o700);
    const symlinkFileEnv = environment(symlinkFileDirectory, artifact);
    writeFileSync(join(symlinkFileDirectory, 'goals-target'), artifact.goals, { mode: 0o600 });
    symlinkSync(join(symlinkFileDirectory, 'goals-target'), symlinkFileEnv['CU_FROZEN_TASK_GOALS_PATH']!);
    writeFileSync(symlinkFileEnv['CU_FROZEN_GOAL_CATALOG_MANIFEST_PATH']!, artifact.manifest, { mode: 0o600 });
    expectUnavailable(symlinkFileEnv, 'catalog_files_unsafe');

    const changed = environment(directory, artifact);
    const modifiedManifest = Buffer.from(artifact.manifest);
    modifiedManifest[5] = modifiedManifest[5]! ^ 1;
    writeFileSync(paths.manifestPath, modifiedManifest);
    expectUnavailable(changed, 'catalog_hash_mismatch');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('a configured missing file never falls back to scanning or guessing a catalog directory', () => {
  const directory = makeDirectory();
  try {
    const artifact = makeArtifact();
    const env = environment(directory, artifact);
    expectUnavailable(env, 'catalog_files_unavailable');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
