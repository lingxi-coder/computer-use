import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fixtureTaskIds, textEditTaskIds } from './business-results.mjs';
import { buildTaskProfileBundle, canonicalSha256, projectTaskRequestForModel, sha256, stableJson } from './task-profiles.mjs';
import { buildFrozenGoalCatalog, frozenGoalSha256, writeFrozenGoalCatalog } from './frozen-goals.mjs';

const artifactNames = ['app', 'textedit_app', 'model', 'graph', 'preprocess', 'program', 'knowledge'];

function fixtureBundle(t) {
  const root = mkdtempSync(join(tmpdir(), 'mvp-frozen-goals-test-'));
  const assetRoot = join(root, 'assets');
  mkdirSync(assetRoot);
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const artifactAssets = Object.fromEntries(artifactNames.map(name => {
    const relativePath = `${name}.artifact`;
    const bytes = Buffer.from(`goal catalog fixture: ${name}`, 'utf8');
    writeFileSync(join(assetRoot, relativePath), bytes, { flag: 'wx', mode: 0o600 });
    return [name, { relative_path: relativePath, sha256: sha256(bytes), max_bytes: 1024 }];
  }));
  const environment = {
    version: 1,
    os: { name: 'macOS', build: '25A000' },
    display: { width_px: 2560, height_px: 1600, dpi_x: 220, dpi_y: 220 },
    power_mode: 'AC balanced',
    hardware: { model: 'Test Mac', architecture: 'arm64', memory_gib: 64 },
    app: { name: 'GuardFixture', bundle_id: 'com.lingxi.computer-user.guard-fixture', version: '1.0.0' },
    textedit_app: { name: 'TextEdit', bundle_id: 'com.apple.TextEdit', version: '1.0.0' },
    model: { name: 'test-model', version: '1' },
    graph: { release: 'graph-r1' },
    preprocess: { release: 'preprocess-r1' },
    program: { release: 'program-r1' },
    knowledge: { release: 'knowledge-r1', task_workflows_included: false },
    asset_root: assetRoot,
    artifact_assets: artifactAssets,
  };
  const bundle = buildTaskProfileBundle({ environment, taskOutputDirectory: join(root, 'task-output') });
  return { root, bundle };
}

function rehashSource(bundle, mutate) {
  const requestExport = JSON.parse(bundle.files['task-requests.json'].toString('utf8'));
  const freeze = JSON.parse(bundle.files['task-profile-freeze.json'].toString('utf8'));
  mutate(requestExport, freeze);
  const requestBytes = Buffer.from(stableJson(requestExport), 'utf8');
  freeze.export_hashes.online_requests_sha256 = sha256(requestBytes);
  const { freeze_content_sha256: _oldHash, ...base } = freeze;
  freeze.freeze_content_sha256 = canonicalSha256(base);
  return { requestBytes, freezeBytes: Buffer.from(stableJson(freeze), 'utf8') };
}

function buildFrom(bundle, input = {}) {
  return buildFrozenGoalCatalog({
    requestBytes: input.requestBytes ?? bundle.files['task-requests.json'],
    freezeBytes: input.freezeBytes ?? bundle.files['task-profile-freeze.json'],
    projectGoal: projectTaskRequestForModel,
  });
}

test('goal-only catalog preserves all 120 identities and exposes only projected request fields', t => {
  const { bundle } = fixtureBundle(t);
  const catalog = buildFrom(bundle);
  assert.equal(catalog.taskGoals.records.length, 120);
  assert.equal(catalog.manifest.identity_count, 120);
  assert.deepEqual(catalog.manifest.denominators, { fixture_runs: 100, textedit_runs: 20, total_records: 120 });
  assert.deepEqual(catalog.taskGoals.records.slice(0, 100).map(row => [row.suite, row.task_id, row.seed]),
    fixtureTaskIds.flatMap(taskId => [1, 2, 3, 4, 5].map(seed => ['fixture', taskId, seed])));
  assert.deepEqual(catalog.taskGoals.records.slice(100).map(row => [row.suite, row.task_id, row.seed]),
    textEditTaskIds.map(taskId => ['textedit', taskId, 1]));
  assert.deepEqual(Object.keys(catalog.files).sort(), ['goal-catalog-manifest.json', 'task-goals.json']);
  for (const row of catalog.taskGoals.records) {
    assert.deepEqual(Object.keys(row).sort(), ['goal', 'seed', 'selector', 'suite', 'task_id']);
    assert.deepEqual(Object.keys(row.goal).sort(), ['allowed_scope', 'objective']);
    assert.match(row.selector, /^[a-f0-9]{64}$/);
    assert.doesNotMatch(JSON.stringify(row.goal), /initial_state|evaluator_truth|expected_|driver_answer|confirmation_id/);
  }
  for (const row of catalog.taskGoals.records.filter(item => item.suite === 'fixture' && item.task_id.startsWith('SET-'))) {
    assert.deepEqual(Object.keys(row.goal.allowed_scope.targets[0]).sort(), row.task_id === 'SET-06'
      ? ['control', 'disambiguator', 'group'] : ['control', 'group']);
  }
  const textEdit = catalog.taskGoals.records[100].goal.allowed_scope;
  assert.deepEqual(Object.keys(textEdit).sort(), ['application', 'document_format', 'file_name', 'no_other_files', 'operations', 'output_directory']);
  assert.equal(catalog.catalogId, sha256(catalog.files['goal-catalog-manifest.json']));
  assert.equal(catalog.manifest.source_freeze_sha256, sha256(bundle.files['task-profile-freeze.json']));
  assert.equal(catalog.manifest.source_request_sha256, sha256(bundle.files['task-requests.json']));
  assert.equal(catalog.manifest.task_goals_sha256, sha256(catalog.files['task-goals.json']));
});

test('selectors and identity hash bind ordered identity plus canonical goal bytes', t => {
  const { bundle } = fixtureBundle(t);
  const catalog = buildFrom(bundle);
  const first = catalog.taskGoals.records[0];
  const goalHash = frozenGoalSha256(Buffer.from(stableJson(first.goal), 'utf8'));
  const expectedSelector = sha256(Buffer.from([
    'computer-use-frozen-task-selector-v1',
    catalog.manifest.source_freeze_sha256,
    first.suite,
    first.task_id,
    String(first.seed),
    goalHash,
  ].join('\n'), 'utf8'));
  assert.equal(first.selector, expectedSelector);
  const identityTriples = catalog.taskGoals.records.map(({ suite, task_id, seed }) => [suite, task_id, seed]);
  assert.equal(catalog.manifest.identities_sha256, canonicalSha256(identityTriples));
});

test('goal export rejects profile-shaped extras and missing fixture group scope even when source hashes are internally consistent', t => {
  const { bundle } = fixtureBundle(t);
  const extraMaterial = rehashSource(bundle, requests => {
    requests.records[0].task_request.allowed_scope.workflow = 'injected';
  });
  assert.throws(() => buildFrom(bundle, extraMaterial), /unexpected shape|evaluator or navigation material|forbidden/i);

  const missingGroup = rehashSource(bundle, requests => {
    delete requests.records[1].task_request.allowed_scope.targets[0].group;
  });
  assert.throws(() => buildFrom(bundle, missingGroup), /unexpected shape/);

  const arbitraryPath = rehashSource(bundle, requests => {
    requests.records[100].task_request.allowed_scope.task_directory = '/tmp/other';
  });
  assert.throws(() => buildFrom(bundle, arbitraryPath), /path-like field|unexpected shape/);
});

test('goal export rejects noncanonical bytes and freeze/request mismatches', t => {
  const { bundle } = fixtureBundle(t);
  const requestBytes = bundle.files['task-requests.json'];
  const freezeBytes = bundle.files['task-profile-freeze.json'];
  assert.throws(() => buildFrom(bundle, { requestBytes: Buffer.concat([requestBytes, Buffer.from(' ')]), freezeBytes }), /canonical JSON bytes/);
  const changedFreeze = Buffer.from(freezeBytes);
  changedFreeze[changedFreeze.length - 2] ^= 1;
  assert.throws(() => buildFrom(bundle, { requestBytes, freezeBytes: changedFreeze }));
});

test('goal catalog writer creates only the two mode-0600 artifacts and refuses overwrite', t => {
  const { bundle, root } = fixtureBundle(t);
  const catalog = buildFrom(bundle);
  const output = join(root, 'catalog');
  mkdirSync(output);
  const result = writeFrozenGoalCatalog(output, catalog);
  assert.deepEqual(result.files, ['goal-catalog-manifest.json', 'task-goals.json']);
  assert.equal(result.catalog_id, catalog.catalogId);
  for (const name of result.files) {
    assert.equal(readFileSync(join(output, name)).toString('utf8'), catalog.files[name].toString('utf8'));
    assert.equal(statSync(join(output, name)).mode & 0o777, 0o600);
  }
  assert.throws(() => writeFrozenGoalCatalog(output, catalog), /already exists/);
  assert.equal(readFileSync(join(output, 'task-goals.json')).equals(catalog.files['task-goals.json']), true);
});
