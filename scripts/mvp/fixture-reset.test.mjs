import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fixtureTaskIds } from './business-results.mjs';
import { buildTaskProfileBundle, canonicalSha256, sha256, stableJson, writeFrozenTaskProfiles } from './task-profiles.mjs';
import { FIXTURE_APPLICATION, loadFixtureReset, projectFixtureReset } from './fixture-reset.mjs';

function setup(t) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'mvp-reset-test-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const asset_root = join(root, 'assets'); mkdirSync(asset_root);
  const artifact_assets = Object.fromEntries(['app', 'textedit_app', 'model', 'graph', 'preprocess', 'program', 'knowledge'].map(name => {
    const bytes = Buffer.from(`synthetic reset regression ${name}`);
    writeFileSync(join(asset_root, name), bytes);
    return [name, { relative_path: name, sha256: sha256(bytes), max_bytes: 1024 }];
  }));
  const environment = {
    version: 1, os: { name: 'macOS', build: 'synthetic-build' },
    display: { width_px: 3024, height_px: 1964, dpi_x: 255, dpi_y: 255 }, power_mode: 'synthetic',
    hardware: { model: 'synthetic', architecture: 'arm64', memory_gib: 64 }, app: FIXTURE_APPLICATION,
    textedit_app: { name: 'TextEdit', bundle_id: 'com.apple.TextEdit', version: '1.0.0' },
    model: { name: 'synthetic', version: '1' }, graph: { release: 'synthetic' },
    preprocess: { release: 'synthetic' }, program: { release: 'synthetic' },
    knowledge: { release: 'synthetic', task_workflows_included: false }, asset_root, artifact_assets,
  };
  const bundle = buildTaskProfileBundle({ environment, taskOutputDirectory: join(root, 'text-output') });
  const requestBytes = bundle.files['task-requests.json'];
  const freezeBytes = bundle.files['task-profile-freeze.json'];
  return { root, bundle, requestBytes, freezeBytes, taskId: 'SET-02', seed: 1 };
}

// Refreezing here models a structurally self-consistent but invalid driver artifact.
function alter(input, mutate) {
  const requests = JSON.parse(input.requestBytes);
  const freeze = JSON.parse(input.freezeBytes);
  mutate(requests, freeze);
  const requestBytes = Buffer.from(stableJson(requests));
  freeze.export_hashes.online_requests_sha256 = sha256(requestBytes);
  freeze.environment_sha256 = canonicalSha256(freeze.environment);
  delete freeze.freeze_content_sha256;
  freeze.freeze_content_sha256 = canonicalSha256(freeze);
  return { ...input, requestBytes, freezeBytes: Buffer.from(stableJson(freeze)) };
}

test('all 100 frozen fixture resets omit the goal; every model projection omits reset and truth', t => {
  const input = setup(t);
  for (const taskId of fixtureTaskIds) for (const seed of [1, 2, 3, 4, 5]) {
    const output = projectFixtureReset({ ...input, taskId, seed });
    assert.deepEqual(Object.keys(output.resetProfile).sort(), ['initial_state', 'seed', 'task_id', 'version']);
    assert.deepEqual(Object.keys(output.modelGoal).sort(), ['allowed_scope', 'objective']);
    assert.equal(output.resetProfile.task_id, taskId);
    assert.equal(output.resetProfile.seed, seed);
    assert.doesNotMatch(JSON.stringify(output.modelGoal), /initial_state|evaluator_truth|expected_|driver_answer/);
    assert.equal(output.pins.request_sha256, sha256(input.requestBytes));
  }
});

test('request and freeze tampering are rejected before projection', t => {
  const input = setup(t);
  const changed = JSON.parse(input.requestBytes); changed.records[0].seed = 6;
  assert.throws(() => projectFixtureReset({ ...input, requestBytes: Buffer.from(stableJson(changed)) }), /request export hash mismatch/);
  const freeze = JSON.parse(input.freezeBytes); freeze.business_results = 'completed';
  assert.throws(() => projectFixtureReset({ ...input, freezeBytes: Buffer.from(stableJson(freeze)) }));
});

test('duplicates, removed cases and variant changes cannot reduce or rewrite the denominator', t => {
  const input = setup(t);
  for (const mutate of [
    request => { request.records[1] = request.records[0]; },
    request => { request.records.pop(); },
    request => { request.records[0].task_request.initial_state.variant.language = 'zh-CN'; },
    (_request, freeze) => { freeze.denominators.fixture_runs = 99; },
  ]) assert.throws(() => projectFixtureReset(alter(input, mutate)));
});

test('self-consistent exports still reject oracle, workflow and unregistered applications', t => {
  const input = setup(t);
  for (const mutate of [
    request => { request.records[0].task_request.initial_state.oracle = { value: true }; },
    request => { request.records[0].task_request.allowed_scope.targets[0].steps = ['click']; },
    request => { request.records[0].task_request.initial_state.application.bundle_id = 'unknown.application'; },
    (_request, freeze) => { freeze.environment.app.bundle_id = 'unknown.application'; },
  ]) assert.throws(() => projectFixtureReset(alter(input, mutate)));
});

test('invalid identities, fields, controls and oversized input are rejected', t => {
  const input = setup(t);
  assert.throws(() => projectFixtureReset({ ...input, seed: '1' }));
  assert.throws(() => projectFixtureReset({ ...input, taskId: 'TEXTEDIT-01' }));
  assert.throws(() => projectFixtureReset({ ...input, requestBytes: Buffer.alloc(1024 * 1024 + 1) }));
  assert.throws(() => projectFixtureReset(alter(input, request => { request.records[0].task_request.initial_state.controls[0].label = 'bad\u0000label'; })));
  assert.throws(() => projectFixtureReset(alter(input, request => { request.records[0].task_request.allowed_scope.no_unrelated_controls = false; })));
});

test('file loader reads immutable bounded regular files and rejects symlink inputs', t => {
  const input = setup(t); const out = join(input.root, 'freeze'); mkdirSync(out);
  writeFrozenTaskProfiles(out, input.bundle);
  const requestFile = join(out, 'task-requests.json'); const freezeFile = join(out, 'task-profile-freeze.json');
  const result = loadFixtureReset({ requestFile, freezeFile, taskId: input.taskId, seed: input.seed });
  assert.equal(result.resetProfile.task_id, input.taskId);
  assert.equal(result.pins.freeze_sha256, sha256(readFileSync(freezeFile)));
  const alias = join(input.root, 'alias.json'); symlinkSync(requestFile, alias);
  assert.throws(() => loadFixtureReset({ requestFile: alias, freezeFile, taskId: input.taskId, seed: input.seed }), /bounded regular file/);
});
