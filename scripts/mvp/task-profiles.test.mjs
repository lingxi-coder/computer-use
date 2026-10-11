import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fixtureTaskIds, textEditTaskIds } from './business-results.mjs';
import {
  MAX_ASSET_BYTES, buildTaskProfileBundle, canonicalSha256, hashBoundedAsset,
  projectTaskRequestForModel, projectTaskRequestsForModel, sha256, writeFrozenTaskProfiles,
} from './task-profiles.mjs';

const artifactNames = ['app', 'textedit_app', 'model', 'graph', 'preprocess', 'program', 'knowledge'];

function fixtureEnvironment(t) {
  const root = mkdtempSync(join(tmpdir(), 'mvp-task-profiles-test-'));
  const assetRoot = join(root, 'assets');
  mkdirSync(assetRoot);
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const artifactAssets = Object.fromEntries(artifactNames.map(name => {
    const relativePath = `${name}.artifact`;
    const bytes = Buffer.from(`frozen ${name} artifact fixture`, 'utf8');
    writeFileSync(join(assetRoot, relativePath), bytes, { flag: 'wx', mode: 0o600 });
    return [name, { relative_path: relativePath, sha256: sha256(bytes), max_bytes: 1024 }];
  }));
  return {
    root,
    assetRoot,
    manifest: {
      version: 1,
      os: { name: 'macOS', build: '25A000' },
      display: { width_px: 2560, height_px: 1600, dpi_x: 220, dpi_y: 220 },
      power_mode: 'AC balanced',
      hardware: { model: 'Apple M2 Max', architecture: 'arm64', memory_gib: 64 },
      app: { name: 'GuardFixture', bundle_id: 'com.lingxi.computer-user.guard-fixture', version: '1.0.0' },
      textedit_app: { name: 'TextEdit', bundle_id: 'com.apple.TextEdit', version: '1.0.0' },
      model: { name: 'local-evaluator-model', version: '2026-10' },
      graph: { release: 'graph-r1' },
      preprocess: { release: 'preprocess-r1' },
      program: { release: 'program-r1' },
      knowledge: { release: 'knowledge-r1', task_workflows_included: false },
      asset_root: assetRoot,
      artifact_assets: artifactAssets,
    },
  };
}

function bundleFor(t, options = {}) {
  const setup = fixtureEnvironment(t);
  const taskOutputDirectory = options.taskOutputDirectory ?? join(setup.root, 'task-output');
  return { ...setup, taskOutputDirectory,
    bundle: buildTaskProfileBundle({ environment: setup.manifest, taskOutputDirectory }) };
}

function records(bundle, suite) { return bundle.onlineRequests.records.filter(record => record.suite === suite); }
function truthRecords(bundle, suite) { return bundle.evaluatorTruth.records.filter(record => record.suite === suite); }

function assertNoEvaluatorOrNavigationFields(value) {
  if (Array.isArray(value)) { value.forEach(assertNoEvaluatorOrNavigationFields); return; }
  if (!value || typeof value !== 'object') return;
  for (const [key, child] of Object.entries(value)) {
    assert.doesNotMatch(key, /^(evaluator(?:_truth)?|truth|oracle|expected_(result|final_state|value)|navigation|steps|workflow|selector|coordinates?)$/i);
    assertNoEvaluatorOrNavigationFields(child);
  }
}

test('frozen denominators exactly match 20 fixture tasks x five seeds and 20 TextEdit runs', t => {
  const { bundle } = bundleFor(t);
  const fixture = records(bundle, 'fixture');
  const textedit = records(bundle, 'textedit');
  assert.equal(fixture.length, 100);
  assert.equal(textedit.length, 20);
  assert.equal(bundle.freezeManifest.denominators.fixture_tasks, 20);
  assert.equal(bundle.freezeManifest.denominators.fixture_seeds_per_task, 5);
  assert.equal(bundle.freezeManifest.denominators.fixture_runs, 100);
  assert.equal(bundle.freezeManifest.denominators.textedit_runs, 20);
  assert.equal(bundle.freezeManifest.denominators.total_records, 120);
  assert.deepEqual([...new Set(fixture.map(row => row.task_id))], fixtureTaskIds);
  assert.deepEqual([...new Set(textedit.map(row => row.task_id))], textEditTaskIds);
  for (const id of fixtureTaskIds) {
    assert.deepEqual(fixture.filter(row => row.task_id === id).map(row => row.seed), [1, 2, 3, 4, 5]);
  }
  assert.ok(textedit.every(row => row.seed === 1));
});

test('language, theme and layout assignments are preallocated and balanced over the frozen fixture', t => {
  const { bundle } = bundleFor(t);
  const fixture = records(bundle, 'fixture');
  const counts = { language: {}, theme: {}, layout: {} };
  for (let index = 0; index < fixtureTaskIds.length; index += 1) {
    const id = fixtureTaskIds[index];
    for (const seed of [1, 2, 3, 4, 5]) {
      const row = fixture.find(value => value.task_id === id && value.seed === seed);
      const variant = row.task_request.initial_state.variant;
      const seedIndex = seed - 1;
      assert.deepEqual(variant, {
        language: ['en-US', 'zh-CN'][(index + seedIndex) % 2],
        theme: ['light', 'dark'][(index + Math.floor(seedIndex / 2)) % 2],
        layout: ['compact', 'standard', 'wide', 'two_column'][(index + seedIndex) % 4],
      });
      for (const key of Object.keys(counts)) counts[key][variant[key]] = (counts[key][variant[key]] ?? 0) + 1;
    }
  }
  assert.deepEqual(counts.language, { 'en-US': 50, 'zh-CN': 50 });
  assert.deepEqual(counts.theme, { light: 50, dark: 50 });
  assert.deepEqual(counts.layout, { compact: 25, standard: 25, wide: 25, two_column: 25 });
});

test('all 20 fixture tasks retain their semantic condition and scoped initial state', t => {
  const { bundle } = bundleFor(t);
  const fixture = records(bundle, 'fixture');
  const pick = id => fixture.find(row => row.task_id === id && row.seed === 1);
  assert.equal(pick('SET-01').task_request.initial_state.controls[0].value, false);
  assert.equal(truthRecords(bundle, 'fixture').find(row => row.task_id === 'SET-01' && row.seed === 1).evaluator_truth.required_change_count, 0);
  assert.equal(pick('SET-03').task_request.initial_state.persistence, 'manual_save_required');
  assert.equal(pick('SET-04').task_request.initial_state.persistence, 'automatic');
  assert.equal(truthRecords(bundle, 'fixture').find(row => row.task_id === 'SET-05' && row.seed === 1).evaluator_truth.required_fresh_readback, true);
  const sameName = pick('SET-06').task_request.initial_state.controls;
  assert.equal(sameName[0].label, sameName[1].label);
  assert.notEqual(sameName[0].group, sameName[1].group);
  assert.equal(pick('FORM-01').task_request.initial_state.controls[0].input_kind, 'single_line');
  assert.equal(pick('FORM-02').task_request.initial_state.controls[0].input_kind, 'multiline');
  assert.equal(pick('FORM-03').task_request.initial_state.controls[0].input_kind, 'dropdown');
  assert.equal(pick('FORM-04').task_request.initial_state.controls[0].input_kind, 'radio_group');
  assert.equal(pick('FORM-05').task_request.initial_state.controls[0].input_kind, 'checkbox');
  assert.equal(pick('FORM-06').task_request.initial_state.validation_state, 'invalid');
  assert.equal(pick('FORM-07').task_request.initial_state.controls[0].initially_in_viewport, false);
  assert.equal(pick('LIST-02').task_request.initial_state.list.same_named_rows.length, 2);
  assert.equal(pick('LIST-03').task_request.initial_state.list.virtualized, true);
  assert.equal(truthRecords(bundle, 'fixture').find(row => row.task_id === 'LIST-04' && row.seed === 1).evaluator_truth.row_identity_must_survive_reorder, true);
  assert.equal(pick('MODAL-01').task_request.initial_state.active_modal.kind, 'known_informational');
  assert.ok(pick('MODAL-02').task_request.allowed_scope.targets[0].modal);
  assert.equal(truthRecords(bundle, 'fixture').find(row => row.task_id === 'MODAL-03' && row.seed === 1).evaluator_truth.background_controls_unchanged, true);
});

test('TextEdit has 20 unique plain-text tasks with balanced language, save-dialog and overwrite variants', t => {
  const { bundle } = bundleFor(t);
  const textedit = records(bundle, 'textedit');
  const truth = truthRecords(bundle, 'textedit');
  const filenames = textedit.map(row => row.task_request.allowed_scope.file_name);
  assert.equal(new Set(filenames).size, 20);
  assert.deepEqual(textedit.map(row => row.task_id), textEditTaskIds);
  assert.equal(textedit.filter(row => row.task_request.initial_state.variant.language === 'en-US').length, 10);
  assert.equal(textedit.filter(row => row.task_request.initial_state.variant.language === 'zh-CN').length, 10);
  assert.equal(textedit.filter(row => row.task_request.initial_state.file_state === 'target_file_absent').length, 10);
  assert.equal(textedit.filter(row => row.task_request.initial_state.file_state === 'existing_disposable_test_file').length, 10);
  assert.equal(textedit.filter(row => row.task_request.initial_state.save_dialog_variant === 'first_save_native_dialog').length, 10);
  assert.equal(textedit.filter(row => row.task_request.initial_state.save_dialog_variant === 'existing_file_overwrite_confirmation').length, 10);
  assert.ok(textedit.every(row => row.task_request.initial_state.document_format === 'plain_text'));
  assert.ok(textedit.every(row => row.task_request.objective.includes('Lingxi TextEdit')));
  for (let index = 0; index < textedit.length; index += 1) {
    assert.deepEqual(textedit[index].task_request.initial_state.variant, {
      language: index % 2 === 0 ? 'en-US' : 'zh-CN',
      theme: ['light', 'dark'][(index + Math.floor(index / 3)) % 2],
      layout: ['compact', 'standard', 'wide', 'two_column'][(index * 3) % 4],
    });
  }
  const truthById = new Map(truth.map(row => [row.task_id, row.evaluator_truth]));
  for (const row of textedit) {
    const state = row.task_request.initial_state;
    const expected = truthById.get(row.task_id);
    assert.match(expected.expected_content_sha256, /^[a-f0-9]{64}$/);
    assert.ok(expected.expected_content_byte_length > 0);
    assert.equal(expected.expected_ui_close_reopen_readback, true);
    const confirmation = expected.destructive_overwrite_confirmation;
    if (state.file_state === 'existing_disposable_test_file') {
      assert.equal(confirmation.required, true);
      assert.equal(confirmation.driver_answer, 'approve');
      assert.equal(confirmation.actual_confirmation_event_required, true);
      assert.equal(confirmation.confirmation_id, `overwrite-${row.task_id.toLowerCase()}`);
      assert.match(confirmation.initial_file_sha256, /^[a-f0-9]{64}$/);
      assert.equal(confirmation.plan_status, 'not_run_no_confirmation_event_yet');
      assert.deepEqual(confirmation.event_fields, ['confirmation_id', 'target_file_name', 'response', 'observed_at']);
    } else {
      assert.equal(confirmation.required, false);
      assert.equal(confirmation.actual_confirmation_event_required, false);
    }
  }
});

test('online requests contain only user request, allowed scope and initial state; truth has its own hash', t => {
  const { bundle, assetRoot } = bundleFor(t);
  assertNoEvaluatorOrNavigationFields(bundle.onlineRequests);
  for (const record of bundle.onlineRequests.records) {
    assert.deepEqual(Object.keys(record).sort(), ['seed', 'suite', 'task_id', 'task_request']);
    assert.deepEqual(Object.keys(record.task_request).sort(), ['allowed_scope', 'initial_state', 'objective']);
  }
  const serializedRequest = JSON.stringify(bundle.onlineRequests);
  assert.doesNotMatch(serializedRequest, /expected_content_sha256|driver_answer|independent_evaluator_only|freeze_content_sha256/);
  const requestHash = sha256(bundle.files['task-requests.json']);
  const truthHash = sha256(bundle.files['evaluator-truth.json']);
  assert.notEqual(requestHash, truthHash);
  assert.equal(bundle.freezeManifest.export_hashes.online_requests_sha256, requestHash);
  assert.equal(bundle.freezeManifest.export_hashes.evaluator_truth_sha256, truthHash);
  assert.equal(bundle.freezeManifest.freeze_content_sha256, canonicalSha256(Object.fromEntries(
    Object.entries(bundle.freezeManifest).filter(([key]) => key !== 'freeze_content_sha256'))));
  assert.equal(bundle.freezeManifest.plan_only, true);
  assert.equal(bundle.freezeManifest.execution_started, false);
  assert.equal(bundle.freezeManifest.business_results, 'not_run');
  assert.equal(bundle.freezeManifest.environment.knowledge.task_workflows_included, false);
  assert.deepEqual(Object.keys(bundle.freezeManifest.environment.artifact_hashes).sort(), [...artifactNames].sort());
  assert.equal(JSON.stringify(bundle.freezeManifest).includes(assetRoot), false);
});

test('Goal model projection covers all 120 requests without reset state or evaluator-only data', t => {
  const { bundle } = bundleFor(t);
  const driverRecords = bundle.onlineRequests.records;
  const truthRecordsByKey = new Map(bundle.evaluatorTruth.records.map(record => [
    `${record.suite}:${record.task_id}:${record.seed}`,
    record.evaluator_truth,
  ]));
  const projections = projectTaskRequestsForModel(bundle.onlineRequests);
  assert.equal(projections.length, 120);
  assert.ok(driverRecords.some(record => /[\r\n]/.test(record.task_request.objective)), 'projection input includes multiline user content');

  for (let index = 0; index < driverRecords.length; index += 1) {
    const record = driverRecords[index];
    const request = record.task_request;
    const projection = projections[index];
    const projectionJson = JSON.stringify(projection);
    const truth = truthRecordsByKey.get(`${record.suite}:${record.task_id}:${record.seed}`);

    assert.deepEqual(Object.keys(projection).sort(), ['allowed_scope', 'objective']);
    assert.deepEqual(projection, {
      objective: request.objective,
      allowed_scope: request.allowed_scope,
    });
    assert.deepEqual(projectTaskRequestForModel(record), projection);
    assert.doesNotMatch(projectionJson, /initial_state|evaluator_truth|expected_|driver_answer|confirmation_id|initial_file_sha256/);
    assert.equal(projectionJson.includes(JSON.stringify(request.initial_state)), false);
    assert.ok(truth);

    if (record.suite === 'textedit') {
      for (const key of ['initial_contents', 'file_state', 'document_state', 'save_dialog_variant']) {
        const resetValue = request.initial_state[key];
        if (typeof resetValue === 'string') assert.equal(projectionJson.includes(resetValue), false);
      }
      assert.equal(projectionJson.includes(truth.expected_content_sha256), false);
      const confirmation = truth.destructive_overwrite_confirmation;
      if (confirmation.confirmation_id) assert.equal(projectionJson.includes(confirmation.confirmation_id), false);
      if (confirmation.initial_file_sha256) assert.equal(projectionJson.includes(confirmation.initial_file_sha256), false);
    }
  }
});

test('profile and export hashes are stable for identical inputs', t => {
  const setup = fixtureEnvironment(t);
  const first = buildTaskProfileBundle({ environment: setup.manifest, taskOutputDirectory: join(setup.root, 'task-output') });
  const second = buildTaskProfileBundle({ environment: setup.manifest, taskOutputDirectory: join(setup.root, 'task-output') });
  assert.deepEqual(first.hashes, second.hashes);
  assert.equal(first.freezeManifest.freeze_content_sha256, second.freezeManifest.freeze_content_sha256);
  assert.deepEqual(first.files['task-requests.json'], second.files['task-requests.json']);
  assert.deepEqual(first.files['evaluator-truth.json'], second.files['evaluator-truth.json']);
});

test('LIST-01 reset labels match the requested language and exact named entry', t => {
  const { bundle } = bundleFor(t);
  const rows = records(bundle, 'fixture').filter(row => row.task_id === 'LIST-01');
  assert.deepEqual(new Set(rows.map(row => row.task_request.initial_state.variant.language)), new Set(['en-US', 'zh-CN']));
  for (const row of rows) {
    const label = row.task_request.initial_state.list.rows[0].label;
    const prefix = row.task_request.initial_state.variant.language === 'zh-CN' ? '季度回顾' : 'Quarterly review';
    assert.equal(label, `${prefix} ${String(row.seed).padStart(2, '0')}`);
    assert.ok(row.task_request.objective.includes(`“${label}”`));
  }
});

test('freeze rejects incomplete environment, missing hashes, mismatches and unbounded asset paths', t => {
  const setup = fixtureEnvironment(t);
  const missingPower = structuredClone(setup.manifest); delete missingPower.power_mode;
  assert.throws(() => buildTaskProfileBundle({ environment: missingPower, taskOutputDirectory: join(setup.root, 'task-output') }), /environment manifest/);
  const missingHash = structuredClone(setup.manifest); delete missingHash.artifact_assets.app.sha256;
  assert.throws(() => buildTaskProfileBundle({ environment: missingHash, taskOutputDirectory: join(setup.root, 'task-output') }), /declared artifact hash/);
  const wrongHash = structuredClone(setup.manifest); wrongHash.artifact_assets.model.sha256 = '0'.repeat(64);
  assert.throws(() => buildTaskProfileBundle({ environment: wrongHash, taskOutputDirectory: join(setup.root, 'task-output') }), /does not match/);
  const escaped = structuredClone(setup.manifest); escaped.artifact_assets.graph.relative_path = '../outside';
  assert.throws(() => buildTaskProfileBundle({ environment: escaped, taskOutputDirectory: join(setup.root, 'task-output') }), /stay within/);
  const unboundedTotal = structuredClone(setup.manifest);
  for (const asset of Object.values(unboundedTotal.artifact_assets)) asset.max_bytes = MAX_ASSET_BYTES;
  assert.throws(() => buildTaskProfileBundle({ environment: unboundedTotal, taskOutputDirectory: join(setup.root, 'task-output') }), /aggregate artifact read bound/);
  assert.throws(() => buildTaskProfileBundle({ environment: setup.manifest, taskOutputDirectory: `${setup.root}/../task-output` }), /parent traversal/);
});

test('asset hashing reads only a regular file within its declared byte limit', t => {
  const setup = fixtureEnvironment(t);
  const descriptor = setup.manifest.artifact_assets.app;
  assert.equal(hashBoundedAsset(setup.assetRoot, descriptor), descriptor.sha256);
  assert.throws(() => hashBoundedAsset(setup.assetRoot, { ...descriptor, max_bytes: 1 }), /exceeds its declared read bound/);
});

test('frozen files are created with wx, carry stable hashes and cannot overwrite an existing freeze', t => {
  const { bundle, root } = bundleFor(t);
  const output = join(root, 'freeze');
  mkdirSync(output);
  const first = writeFrozenTaskProfiles(output, bundle);
  assert.deepEqual(first.files, ['evaluator-truth.json', 'task-profile-freeze.json', 'task-requests.json']);
  assert.deepEqual(first.goalCatalogFiles, ['goal-catalog-manifest.json', 'task-goals.json']);
  for (const name of first.files) {
    const bytes = readFileSync(join(output, name));
    assert.equal(sha256(bytes), first.hashes[name]);
    assert.equal(first.hashes[name], bundle.hashes[name]);
  }
  for (const name of first.goalCatalogFiles) {
    const bytes = readFileSync(join(output, name));
    assert.equal(sha256(bytes), first.goalCatalogHashes[name]);
    assert.equal(first.goalCatalogHashes[name], bundle.goalCatalog.hashes[name]);
  }
  assert.equal(first.catalogId, bundle.goalCatalog.catalogId);
  const before = readFileSync(join(output, 'task-requests.json'));
  assert.throws(() => writeFrozenTaskProfiles(output, bundle), /already frozen/);
  assert.deepEqual(readFileSync(join(output, 'task-requests.json')), before);
});

test('an occupied truth filename blocks the write without touching that file or creating siblings', t => {
  const { bundle, root } = bundleFor(t);
  const output = join(root, 'occupied-freeze');
  mkdirSync(output);
  const sentinel = Buffer.from('previous frozen truth stays byte-identical');
  writeFileSync(join(output, 'evaluator-truth.json'), sentinel, { flag: 'wx', mode: 0o600 });
  assert.throws(() => writeFrozenTaskProfiles(output, bundle), /already frozen/);
  assert.deepEqual(readFileSync(join(output, 'evaluator-truth.json')), sentinel);
  assert.throws(() => readFileSync(join(output, 'task-requests.json')), { code: 'ENOENT' });
  assert.throws(() => readFileSync(join(output, 'task-profile-freeze.json')), { code: 'ENOENT' });
  assert.throws(() => readFileSync(join(output, 'task-goals.json')), { code: 'ENOENT' });
  assert.throws(() => readFileSync(join(output, 'goal-catalog-manifest.json')), { code: 'ENOENT' });
});

test('an occupied goal-only filename blocks the entire five-file write without touching it', t => {
  const { bundle, root } = bundleFor(t);
  const output = join(root, 'occupied-goal-catalog');
  mkdirSync(output);
  const sentinel = Buffer.from('previous goal catalog stays byte-identical');
  writeFileSync(join(output, 'task-goals.json'), sentinel, { flag: 'wx', mode: 0o600 });
  assert.throws(() => writeFrozenTaskProfiles(output, bundle), /already frozen/);
  assert.deepEqual(readFileSync(join(output, 'task-goals.json')), sentinel);
  for (const name of ['evaluator-truth.json', 'task-profile-freeze.json', 'task-requests.json', 'goal-catalog-manifest.json']) {
    assert.throws(() => readFileSync(join(output, name)), { code: 'ENOENT' });
  }
});

test('mutated Buffer artifacts are rejected before the freeze writer creates any files', t => {
  const { bundle, root } = bundleFor(t);
  const output = join(root, 'mutated-freeze');
  mkdirSync(output);
  bundle.goalCatalog.files['task-goals.json'][0] ^= 1;
  assert.throws(() => writeFrozenTaskProfiles(output, bundle), /changed after validation/);
  for (const name of ['evaluator-truth.json', 'task-profile-freeze.json', 'task-requests.json', 'goal-catalog-manifest.json', 'task-goals.json']) {
    assert.throws(() => readFileSync(join(output, name)), { code: 'ENOENT' });
  }
});
