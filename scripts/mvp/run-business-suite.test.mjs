import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import {
  chmodSync, existsSync, mkdtempSync, mkdirSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { fixtureTaskIds, textEditTaskIds } from './business-results.mjs';
import { assignTaskVariants, canonicalSha256, sha256 } from './task-profiles.mjs';
import { runBusinessSuite, selectSuiteIdentities, validateFrozenSuiteFiles } from './run-business-suite.mjs';

const fixtureApp = { name: 'Interaction Fixture', bundle_id: 'com.lingxi.computer-user.mvp-fixture', version: '1.0.0' };
const textEditApp = { name: 'TextEdit', bundle_id: 'com.apple.TextEdit', version: '2.0.0' };
const nodeExecutablePath = realpathSync(process.execPath);
const nodeExecutableSha256 = sha256(readFileSync(nodeExecutablePath));

function createProfileFiles(root) {
  const textEditOutputDirectory = join(root, 'textedit-output');
  const environment = {
    version: 1,
    os: { name: 'macOS', build: '24A100' },
    display: { width_px: 3024, height_px: 1964, dpi_x: 255, dpi_y: 255 },
    power_mode: 'AC balanced',
    hardware: { model: 'Apple M2 Max', architecture: 'arm64', memory_gib: 64 },
    app: fixtureApp,
    textedit_app: textEditApp,
    model: { name: 'Laya local', version: 'release-1' },
    graph: { release: 'graph-1' },
    preprocess: { release: 'preprocess-1' },
    program: { release: 'program-1' },
    knowledge: { release: 'generic-interaction-1', task_workflows_included: false },
    artifact_hashes: Object.fromEntries(['app', 'textedit_app', 'model', 'graph', 'preprocess', 'program', 'knowledge']
      .map((name, index) => [name, String(index + 1).repeat(64)])),
  };
  const records = [
    ...fixtureTaskIds.flatMap((task_id, taskIndex) => [1, 2, 3, 4, 5].map(seed => ({
      suite: 'fixture', task_id, seed,
      task_request: {
        objective: `Perform ${task_id} seed ${seed}.`,
        allowed_scope: { application: fixtureApp.name, targets: [{ control: task_id }], no_unrelated_controls: true },
        initial_state: { application: fixtureApp, variant: assignTaskVariants(taskIndex, seed) },
      },
    }))),
    ...textEditTaskIds.map(task_id => ({
      suite: 'textedit', task_id, seed: 1,
      task_request: {
        objective: `Save requested TextEdit file ${task_id}.`,
        allowed_scope: {
          application: 'TextEdit', output_directory: textEditOutputDirectory,
          file_name: `cu-mvp-${task_id.toLowerCase()}.txt`, document_format: 'plain_text',
          operations: ['write_requested_contents', 'save', 'close', 'reopen_and_read_back'], no_other_files: true,
        },
        initial_state: {
          application: textEditApp,
          variant: { language: 'en-US', theme: 'light', layout: 'compact' },
          output_directory: textEditOutputDirectory,
          file_name: `cu-mvp-${task_id.toLowerCase()}.txt`,
          document_format: 'plain_text',
          file_state: Number(task_id.slice(-2)) >= 11 ? 'existing_disposable_test_file' : 'target_file_absent',
          document_state: Number(task_id.slice(-2)) >= 11 ? 'existing_file_open' : 'new_untitled_document',
          initial_contents: Number(task_id.slice(-2)) >= 11 ? `Disposable initial fixture content for ${task_id}.\n` : null,
          save_dialog_variant: Number(task_id.slice(-2)) >= 11 ? 'existing_file_overwrite_confirmation' : 'first_save_native_dialog',
        },
      },
    })),
  ];
  const requests = { format_version: 1, task_profile_version: 1, records };
  const truths = {
    format_version: 1,
    task_profile_version: 1,
    audience: 'independent_evaluator_only',
    not_for_online_model: true,
    records: records.map(({ suite, task_id, seed }) => ({ suite, task_id, seed, evaluator_truth: { expected_case: `${suite}/${task_id}/${seed}` } })),
  };
  const requestBytes = Buffer.from(JSON.stringify(requests));
  const truthBytes = Buffer.from(JSON.stringify(truths));
  const freezeBase = {
    format_version: 1,
    task_profile_version: 1,
    plan_only: true,
    execution_started: false,
    business_results: 'not_run',
    environment,
    environment_sha256: canonicalSha256(environment),
    denominators: { fixture_tasks: 20, fixture_seeds_per_task: 5, fixture_runs: 100, textedit_runs: 20, total_records: 120 },
    export_hashes: { online_requests_sha256: sha256(requestBytes), evaluator_truth_sha256: sha256(truthBytes) },
  };
  const freeze = { ...freezeBase, freeze_content_sha256: canonicalSha256(freezeBase) };
  const requestFile = join(root, 'task-requests.json');
  const truthFile = join(root, 'evaluator-truth.json');
  const freezeFile = join(root, 'task-profile-freeze.json');
  writeFileSync(requestFile, requestBytes, { mode: 0o600 });
  writeFileSync(truthFile, truthBytes, { mode: 0o600 });
  writeFileSync(freezeFile, JSON.stringify(freeze), { mode: 0o600 });
  return { requestFile, truthFile, freezeFile, records, truths };
}

function temporaryRoot(t) {
  const root = realpathSync(mkdtempSync(join(realpathSync(tmpdir()), 'cu-business-suite-test-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}

function writeScript(path, source) {
  writeFileSync(path, source, { mode: 0o700 });
  chmodSync(path, 0o700);
  return path;
}

async function stopKnownTestChild(pidFile) {
  if (!existsSync(pidFile)) return;
  const pid = Number(readFileSync(pidFile, 'utf8').trim());
  if (!Number.isSafeInteger(pid) || pid <= 0) return;
  try { process.kill(pid, 'SIGTERM'); } catch { return; }
  const deadline = Date.now() + 1000;
  while (Date.now() < deadline) {
    try { process.kill(pid, 0); } catch (error) {
      if (error?.code === 'ESRCH') return;
    }
    await new Promise(resolveDelay => setTimeout(resolveDelay, 20));
  }
  try { process.kill(pid, 'SIGKILL'); } catch { /* exact test-owned PID may already have exited */ }
  const killDeadline = Date.now() + 1000;
  while (Date.now() < killDeadline) {
    try { process.kill(pid, 0); } catch (error) {
      if (error?.code === 'ESRCH') return;
    }
    await new Promise(resolveDelay => setTimeout(resolveDelay, 20));
  }
  throw new Error(`test-owned child ${pid} did not exit after SIGKILL`);
}

function writeCommandManifest(root, kind, entryPath, launchArgvPrefix = [realpathSync(entryPath)]) {
  const path = join(root, `${kind}-command-manifest.json`);
  const content = {
    version: 1,
    kind,
    frozen_product_decision_program_sha256: kind === 'driver' ? '6'.repeat(64) : null,
    launch_executable_sha256: nodeExecutableSha256,
    launch_argv_prefix: launchArgvPrefix,
    declared_artifacts: [{ role: 'entry', path: realpathSync(entryPath), sha256: sha256(readFileSync(entryPath)) }],
  };
  writeFileSync(path, JSON.stringify(content), { mode: 0o600 });
  return path;
}

const fakeDriverSource = String.raw`
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
const value = key => { const index = process.argv.indexOf(key); if (index < 0) throw new Error('missing ' + key); return process.argv[index + 1]; };
if (process.argv.some(arg => /truth-file|evaluator-truth|oracle-file|task-requests\.json|task-profile-freeze\.json/i.test(arg))) throw new Error('truth or aggregate export leaked to driver');
if (process.env.EVALUATOR_TRUTH_FILE || process.env.OPENAI_API_KEY) throw new Error('sensitive environment leaked to driver');
const request = JSON.parse(readFileSync(value('--request'), 'utf8'));
if (Object.keys(request).sort().join(',') !== 'allowed_scope,objective') throw new Error('model request included reset or evaluator data');
const reset = JSON.parse(readFileSync(value('--launcher-reset-file'), 'utf8'));
if (!reset.initial_state || !reset.task_id) throw new Error('separate reset input missing');
const runDir = value('--output-dir');
mkdirSync(join(runDir, 'evidence'), { mode: 0o700 });
const evidence = Buffer.from(JSON.stringify({ diagnostic: true }));
writeFileSync(join(runDir, 'evidence', 'final.json'), evidence, { flag: 'wx', mode: 0o600 });
const identity = { suite: value('--suite'), task_id: value('--task-id'), seed: Number(value('--seed')) };
const receipt = {
  version: 1, ...identity, root_id: 'diagnostic-root', admission: { accepted: true, rejection_reason: null },
  status: 'unknown', system_reported_completed: false, cold_start: true,
  typed_ledger: { decisions: [{ decision_id: 'decision-1', source: 'unknown', selected_action_id: null, provider_response_id: null, s2_request_ref: null, at_ms: 1 }], attempts: [], effects: [] },
  local_inference_revisions: [],
  evidence_refs: [{ path: 'evidence/final.json', sha256: createHash('sha256').update(evidence).digest('hex'), bytes: evidence.length }],
  model_call_attribution: { admitted_remote_call_rows: 0, actual_network_requests: null, calls: [] },
  stop_cleanup: { stop_status: 'released', cleanup_status: 'clean', owned_processes_remaining: [], owned_directories_remaining: [] },
  final_read: { status: 'unknown', source_sequence: null, observed_at_ms: null, evidence_ref: null },
  auditors: { trace: 'unknown', scope: 'unknown', current_ui: 'unknown', stop_cleanup: 'passed' },
};
writeFileSync(value('--receipt'), JSON.stringify(receipt), { flag: 'wx', mode: 0o600 });
if (process.argv.includes('--mutate-entry')) appendFileSync(process.argv[1], '\n// changed after launch\n');
if (process.argv.includes('--leave-escaped-child') && identity.task_id === 'SET-01') {
  const escaped = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { detached: true, stdio: 'ignore' });
  escaped.unref();
  writeFileSync(join(runDir, 'escaped-child.pid'), String(escaped.pid) + String.fromCharCode(10), { flag: 'wx', mode: 0o600 });
}
if (process.argv.includes('--leave-group-child') && identity.task_id === 'SET-01') {
  const owned = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
  owned.unref();
  writeFileSync(join(runDir, 'group-child.pid'), String(owned.pid) + String.fromCharCode(10), { flag: 'wx', mode: 0o600 });
}
`;

const fakeEvaluatorSource = String.raw`
import { readFileSync, writeFileSync } from 'node:fs';
const value = key => { const index = process.argv.indexOf(key); if (index < 0) throw new Error('missing ' + key); return process.argv[index + 1]; };
const phase = JSON.parse(readFileSync(value('--driver-phase-complete-file'), 'utf8'));
if (phase.all_driver_children_terminated !== true || phase.all_owned_resources_cleanup_self_reported_clean !== true || phase.all_group_cleanup_probes_passed !== true || phase.coordinator_scope_cleanup_probe_complete !== true || phase.product_owned_resource_coverage_verified !== false || phase.driver_declared_command_artifact_set_matches_hashes_after_phase !== true || phase.evaluator_declared_command_artifact_set_matches_hashes_before_truth_load !== true || phase.driver_child_count !== 20) throw new Error('evaluator started before the full driver phase ended');
const truth = JSON.parse(readFileSync(value('--truth-file'), 'utf8'));
const input = JSON.parse(readFileSync(value('--evaluation-input-file'), 'utf8'));
if (truth.task_id !== value('--task-id') || input.task_id !== truth.task_id) throw new Error('evaluator identity mismatch');
const result = {
  version: 1, suite: value('--suite'), task_id: value('--task-id'), seed: Number(value('--seed')),
  verdict: 'unknown', evidence_validity: 'unknown',
  auditors: { independent_truth: 'unknown', trace: 'unknown', current_ui: 'unknown', scope: 'unknown', stop_cleanup: 'unknown' },
  reason_codes: ['synthetic_child_test_only'],
};
writeFileSync(value('--result-file'), JSON.stringify(result), { flag: 'wx', mode: 0o600 });
`;

const cleanupGateDriverSource = String.raw`
import { createHash } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
const value = key => { const index = process.argv.indexOf(key); if (index < 0) throw new Error('missing ' + key); return process.argv[index + 1]; };
const identity = { suite: value('--suite'), task_id: value('--task-id'), seed: Number(value('--seed')) };
const first = identity.task_id === 'SET-01';
const runDir = value('--output-dir');
mkdirSync(join(runDir, 'evidence'), { mode: 0o700 });
const evidence = Buffer.from(JSON.stringify({ task_id: identity.task_id }));
writeFileSync(join(runDir, 'evidence', 'final.json'), evidence, { flag: 'wx', mode: 0o600 });
const receipt = {
  version: 1, ...identity, root_id: 'diagnostic-root', admission: { accepted: true, rejection_reason: null },
  status: first ? 'completed' : 'unknown', system_reported_completed: first, cold_start: true,
  typed_ledger: {
    decisions: [{ decision_id: 'decision-1', source: first ? 'teacher' : 'unknown', selected_action_id: 'action-1', provider_response_id: first ? 'call-1' : null, s2_request_ref: null, at_ms: 1 }],
    attempts: [], effects: [],
  },
  local_inference_revisions: [],
  evidence_refs: [{ path: 'evidence/final.json', sha256: createHash('sha256').update(evidence).digest('hex'), bytes: evidence.length }],
  model_call_attribution: first
    ? { admitted_remote_call_rows: 1, actual_network_requests: 1, calls: [{ scope: 's1_decision', root_id: 'diagnostic-root', request_id: null, decision_id: 'decision-1', generation: 1,
      provider: 'test-provider', model: 'test-model', provider_response_id: 'call-1', started_ms: 0, finished_ms: 1, outcome: 'proposal',
      usage: { input_tokens: null, output_tokens: null, cost_micros: null }, cost_usd: 0 }] }
    : { admitted_remote_call_rows: 0, actual_network_requests: null, calls: [] },
  stop_cleanup: first
    ? { stop_status: 'released', cleanup_status: 'clean', owned_processes_remaining: [], owned_directories_remaining: [] }
    : { stop_status: 'unknown', cleanup_status: 'unknown', owned_processes_remaining: [], owned_directories_remaining: [] },
  final_read: first
    ? { status: 'current_ui', source_sequence: 1, observed_at_ms: 2, evidence_ref: 'evidence/final.json' }
    : { status: 'unknown', source_sequence: null, observed_at_ms: null, evidence_ref: null },
  auditors: { trace: first ? 'passed' : 'unknown', scope: first ? 'passed' : 'unknown', current_ui: first ? 'passed' : 'unknown', stop_cleanup: first ? 'passed' : 'unknown' },
};
writeFileSync(value('--receipt'), JSON.stringify(receipt), { flag: 'wx', mode: 0o600 });
`;

test('frozen inputs require exact full identities and diagnostic selection cannot become a gate', t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const validated = validateFrozenSuiteFiles(files);
  assert.equal(validated.expected.length, 120);
  const subset = selectSuiteIdentities(validated.expected, 'm3-fixture-diagnostic', 1);
  assert.equal(subset.length, 20);
  assert.ok(subset.every(identity => identity.suite === 'fixture' && identity.seed === 1));
  assert.throws(() => selectSuiteIdentities(validated.expected, 'm3-fixture-diagnostic', 6), /diagnostic seed/);
});

test('invalid preflight leaves an existing output untouched', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const driverScript = writeScript(join(root, 'unused-driver.mjs'), 'process.exit(0);\n');
  const evaluatorScript = writeScript(join(root, 'unused-evaluator.mjs'), 'process.exit(0);\n');
  const out = join(root, 'existing-output');
  mkdirSync(out, { mode: 0o700 });
  const sentinel = join(out, 'keep.txt');
  writeFileSync(sentinel, 'preserve', { mode: 0o600 });
  await assert.rejects(runBusinessSuite({
    ...files,
    outDirectory: out,
    driverPath: realpathSync(process.execPath),
    driverCommandManifest: writeCommandManifest(root, 'driver', driverScript),
    driverArgs: [driverScript],
    evaluatorPath: realpathSync(process.execPath),
    evaluatorCommandManifest: writeCommandManifest(root, 'evaluator', evaluatorScript),
    evaluatorArgs: [evaluatorScript],
  }), /output directory must be new/);
  assert.equal(readFileSync(sentinel, 'utf8'), 'preserve');
});

test('missing declared command artifact set is rejected before creating suite output', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const driverScript = writeScript(join(root, 'driver-entry.mjs'), 'process.exit(0);\n');
  const evaluatorScript = writeScript(join(root, 'evaluator-entry.mjs'), 'process.exit(0);\n');
  const out = join(root, 'missing-closure-output');
  await assert.rejects(runBusinessSuite({
    ...files,
    outDirectory: out,
    driverPath: realpathSync(process.execPath),
    driverArgs: [driverScript],
    evaluatorPath: realpathSync(process.execPath),
    evaluatorCommandManifest: writeCommandManifest(root, 'evaluator', evaluatorScript),
    evaluatorArgs: [evaluatorScript],
    scope: 'm3-fixture-diagnostic',
  }), /driver command manifest is required/);
  assert.equal(existsSync(out), false);
});

test('driver script hash stays separate from and bound to the frozen DecisionProgram hash', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const driverScript = writeScript(join(root, 'driver-entry.mjs'), 'process.exit(0);\n');
  const evaluatorScript = writeScript(join(root, 'evaluator-entry.mjs'), 'process.exit(0);\n');
  const driverManifest = JSON.parse(readFileSync(writeCommandManifest(root, 'driver', driverScript), 'utf8'));
  driverManifest.frozen_product_decision_program_sha256 = '9'.repeat(64);
  const invalidDriverManifest = join(root, 'wrong-program-binding.json');
  writeFileSync(invalidDriverManifest, JSON.stringify(driverManifest), { mode: 0o600 });
  const out = join(root, 'wrong-program-output');
  await assert.rejects(runBusinessSuite({
    ...files,
    outDirectory: out,
    driverPath: realpathSync(process.execPath),
    driverCommandManifest: invalidDriverManifest,
    driverArgs: [driverScript],
    evaluatorPath: realpathSync(process.execPath),
    evaluatorCommandManifest: writeCommandManifest(root, 'evaluator', evaluatorScript),
    evaluatorArgs: [evaluatorScript],
  }), /DecisionProgram binding differs from the frozen product hash/);
  assert.equal(existsSync(out), false);
});

test('Node startup code cannot run before the declared command entry', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const driverScript = writeScript(join(root, 'driver-entry.mjs'), 'process.exit(0);\n');
  const requiredScript = writeScript(join(root, 'unlisted-require.mjs'), 'globalThis.unlistedStartupCodeRan = true;\n');
  const evaluatorScript = writeScript(join(root, 'evaluator-entry.mjs'), 'process.exit(0);\n');
  const launchArgvPrefix = ['--require', requiredScript, driverScript];
  const driverManifest = writeCommandManifest(root, 'driver', driverScript, launchArgvPrefix);
  const out = join(root, 'startup-bypass-output');
  await assert.rejects(runBusinessSuite({
    ...files,
    outDirectory: out,
    driverPath: realpathSync(process.execPath),
    driverCommandManifest: driverManifest,
    driverArgs: launchArgvPrefix,
    evaluatorPath: realpathSync(process.execPath),
    evaluatorCommandManifest: writeCommandManifest(root, 'evaluator', evaluatorScript),
    evaluatorArgs: [evaluatorScript],
  }), /interpreter entry must be argv\[0\]/);
  assert.equal(existsSync(out), false);
});

test('a changed driver command artifact stops the suite before truth staging or evaluation', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const driverScript = writeScript(join(root, 'mutating-driver.mjs'), fakeDriverSource);
  const evaluatorScript = writeScript(join(root, 'must-not-evaluate.mjs'), fakeEvaluatorSource);
  const driverCommandManifest = writeCommandManifest(root, 'driver', driverScript, [driverScript, '--mutate-entry']);
  const evaluatorCommandManifest = writeCommandManifest(root, 'evaluator', evaluatorScript);
  const outDirectory = join(root, 'mutating-output');
  const result = await runBusinessSuite({
    ...files,
    outDirectory,
    driverPath: realpathSync(process.execPath),
    driverCommandManifest,
    driverArgs: [driverScript, '--mutate-entry'],
    evaluatorPath: realpathSync(process.execPath),
    evaluatorCommandManifest,
    evaluatorArgs: [evaluatorScript],
    scope: 'm3-fixture-diagnostic',
    diagnosticSeed: 1,
    timeoutMs: 10_000,
  });
  assert.equal(result.executed_driver_count, 1);
  assert.equal(result.runs[0].driver_process.exit_confirmed, true);
  assert.equal(result.runs[0].driver_process.declared_command_artifact_set_matches_hashes_after_child, false);
  assert.equal(result.driver_phase.driver_declared_command_artifact_set_matches_hashes_after_phase, false);
  assert.equal(result.acceptance_boundary.independent_truth_loaded_after_all_driver_children_exited, false);
  assert.equal(result.acceptance_boundary.truth_load_failure_code, 'DRIVER_COMMAND_ARTIFACT_SET_CHANGED');
  assert.deepEqual(readdirSync(join(outDirectory, 'evaluation')), []);
  assert.equal(result.unrun.pending_identities.length, 19);
  assert.equal(result.business_gate_passed, false);
});

test('M3 diagnostic uses one seed per fixture, runs all drivers before truth evaluation, and stays ineligible', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const driverScript = writeScript(join(root, 'fake-driver.mjs'), fakeDriverSource);
  const evaluatorScript = writeScript(join(root, 'fake-evaluator.mjs'), fakeEvaluatorSource);
  const driverCommandManifest = writeCommandManifest(root, 'driver', driverScript);
  const evaluatorCommandManifest = writeCommandManifest(root, 'evaluator', evaluatorScript);
  const oldTruth = process.env.EVALUATOR_TRUTH_FILE;
  const oldApiKey = process.env.OPENAI_API_KEY;
  process.env.EVALUATOR_TRUTH_FILE = files.truthFile;
  process.env.OPENAI_API_KEY = 'must-not-be-inherited';
  let result;
  try {
    result = await runBusinessSuite({
      ...files,
      outDirectory: join(root, 'diagnostic-output'),
      driverPath: realpathSync(process.execPath),
      driverCommandManifest,
      driverArgs: [driverScript],
      evaluatorPath: realpathSync(process.execPath),
      evaluatorCommandManifest,
      evaluatorArgs: [evaluatorScript],
      scope: 'm3-fixture-diagnostic',
      diagnosticSeed: 1,
      timeoutMs: 10_000,
    });
  } finally {
    if (oldTruth === undefined) delete process.env.EVALUATOR_TRUTH_FILE; else process.env.EVALUATOR_TRUTH_FILE = oldTruth;
    if (oldApiKey === undefined) delete process.env.OPENAI_API_KEY; else process.env.OPENAI_API_KEY = oldApiKey;
  }
  assert.equal(result.selected_identity_count, 20);
  assert.equal(result.executed_driver_count, 20);
  assert.equal(result.driver_phase.all_driver_children_terminated, true);
  assert.equal(result.driver_phase.all_owned_resources_cleanup_self_reported_clean, true);
  assert.equal(result.driver_phase.cleanup_evidence_mode, 'independent_coordinator_process_group_and_private_temp_subtree');
  assert.equal(result.driver_phase.all_group_cleanup_probes_passed, true);
  assert.equal(result.driver_phase.coordinator_scope_cleanup_probe_complete, true);
  assert.equal(result.driver_phase.product_owned_resource_coverage_verified, false);
  assert.equal(result.business_gate_eligible, false);
  assert.equal(result.business_gate_passed, false);
  assert.equal(result.business_results.fixture.aggregator.expected_runs, 100);
  assert.equal(result.business_results.fixture.aggregator.observed_runs, 20);
  assert.equal(result.business_results.fixture.aggregator.missing_runs.length, 80);
  assert.equal(result.business_results.fixture.business_quality_thresholds_met, false);
  assert.equal(result.business_results.textedit.aggregator.expected_runs, 20);
  assert.equal(result.business_results.textedit.aggregator.missing_runs.length, 20);
  assert.equal(result.evidence_validity_gate.passed, false);
  assert.equal(result.acceptance_boundary.direct_args_env_goal_only_excludes_truth, true);
  assert.equal(result.acceptance_boundary.same_uid_isolation_enforced, false);
  assert.equal(result.acceptance_boundary.trusted_driver_required, true);
  assert.equal(result.acceptance_boundary.coordinator_scope_cleanup_probe_complete, true);
  assert.equal(result.acceptance_boundary.product_owned_resource_coverage_verified, false);
  assert.equal(result.acceptance_boundary.product_owned_resource_coverage_status, 'unknown');
  assert.equal(result.acceptance_boundary.runner_direct_truth_routing_to_statecore_s1_harness_knowledge, false);
  assert.equal(result.source_pins.frozen_product_decision_program_sha256, '6'.repeat(64));
  assert.notEqual(result.source_pins.driver_declared_command_artifact_set.declared_artifact_set[0].sha256,
    result.source_pins.frozen_product_decision_program_sha256);
  assert.equal(result.source_pins.driver_declared_command_artifact_set.declared_artifact_set_matches_hashes, true);
  assert.equal(result.source_pins.evaluator_declared_command_artifact_set.declared_artifact_set_matches_hashes, true);
  assert.equal(result.source_pins.driver_declared_command_artifact_set.completeness_independently_verified, false);
  assert.equal(result.source_pins.driver_declared_command_artifact_set.declared_artifact_set[0].role, 'entry');
  assert.equal(result.business_results.fixture.evidence_validity_gate.passed, false);
  assert.equal(result.business_results.fixture.evidence_validity_gate.coordinator_scope_cleanup_probe_complete, true);
  assert.equal(result.business_results.fixture.evidence_validity_gate.product_owned_resource_coverage_verified, false);
  const firstRun = result.runs[0];
  assert.equal(firstRun.status, 'unknown');
  assert.equal(firstRun.evaluator.status, 'completed');
  const driverRequest = JSON.parse(readFileSync(join(root, 'diagnostic-output', 'runs', 'fixture-set-01-1', 'request.json'), 'utf8'));
  assert.deepEqual(Object.keys(driverRequest).sort(), ['allowed_scope', 'objective']);
  const reset = JSON.parse(readFileSync(join(root, 'diagnostic-output', 'runs', 'fixture-set-01-1', 'launcher-reset.json'), 'utf8'));
  assert.ok(reset.initial_state);
  assert.equal('evaluator_truth' in reset, false);
});

test('a descendant left in the coordinator process group blocks the next task and truth phase', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const driverScript = writeScript(join(root, 'group-residual-driver.mjs'), fakeDriverSource);
  const evaluatorScript = writeScript(join(root, 'must-not-evaluate.mjs'), fakeEvaluatorSource);
  const driverArgs = [driverScript, '--leave-group-child'];
  const outDirectory = join(root, 'group-residual-output');
  const pidFile = join(outDirectory, 'runs', 'fixture-set-01-1', 'group-child.pid');
  let result;
  try {
    result = await runBusinessSuite({
      ...files,
      outDirectory,
      driverPath: realpathSync(process.execPath),
      driverCommandManifest: writeCommandManifest(root, 'driver', driverScript, driverArgs),
      driverArgs,
      evaluatorPath: realpathSync(process.execPath),
      evaluatorCommandManifest: writeCommandManifest(root, 'evaluator', evaluatorScript),
      evaluatorArgs: [evaluatorScript],
      scope: 'm3-fixture-diagnostic',
      diagnosticSeed: 1,
      timeoutMs: 10_000,
    });
  } finally {
    await stopKnownTestChild(pidFile);
  }

  assert.equal(result.executed_driver_count, 1);
  assert.equal(result.runs[0].driver_process.exit_confirmed, true);
  assert.equal(result.runs[0].driver_process.owned_resource_probe.process_group.status, 'failed');
  assert.equal(result.runs[0].driver_process.owned_resource_probe.process_group.reason, 'process_group_still_present');
  assert.equal(result.runs[0].driver_process.owned_resource_probe.temp_scope.status, 'passed');
  assert.equal(result.driver_phase.all_group_cleanup_probes_passed, false);
  assert.equal(result.driver_phase.coordinator_scope_cleanup_probe_complete, false);
  assert.equal(result.acceptance_boundary.independent_truth_loaded_after_all_driver_children_exited, false);
  assert.deepEqual(readdirSync(join(outDirectory, 'evaluation')), []);
  assert.equal(result.unrun.pending_identities.length, 19);
  assert.equal(result.business_results.fixture.aggregator.observed_runs, 1);
  assert.equal(result.business_results.fixture.aggregator.missing_runs.length, 99);
  assert.equal(result.business_gate_passed, false);
});

test('an escaped child is outside the probe scope and cannot make product ownership coverage verified', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const driverScript = writeScript(join(root, 'escaped-child-driver.mjs'), fakeDriverSource);
  const evaluatorScript = writeScript(join(root, 'fake-evaluator.mjs'), fakeEvaluatorSource);
  const driverArgs = [driverScript, '--leave-escaped-child'];
  const outDirectory = join(root, 'escaped-child-output');
  const pidFile = join(outDirectory, 'runs', 'fixture-set-01-1', 'escaped-child.pid');
  let result;
  try {
    result = await runBusinessSuite({
      ...files,
      outDirectory,
      driverPath: realpathSync(process.execPath),
      driverCommandManifest: writeCommandManifest(root, 'driver', driverScript, driverArgs),
      driverArgs,
      evaluatorPath: realpathSync(process.execPath),
      evaluatorCommandManifest: writeCommandManifest(root, 'evaluator', evaluatorScript),
      evaluatorArgs: [evaluatorScript],
      scope: 'm3-fixture-diagnostic',
      diagnosticSeed: 1,
      timeoutMs: 10_000,
    });
  } finally {
    await stopKnownTestChild(pidFile);
  }

  assert.equal(result.executed_driver_count, 20);
  assert.equal(result.runs[0].driver_process.owned_resource_probe.process_group.status, 'passed');
  assert.equal(result.runs[0].driver_process.owned_resource_probe.product_owned_resource_coverage_verified, false);
  assert.match(result.runs[0].driver_process.owned_resource_probe.limitation, /leave the coordinator-created process group/);
  assert.equal(result.driver_phase.coordinator_scope_cleanup_probe_complete, true);
  assert.equal(result.driver_phase.product_owned_resource_coverage_verified, false);
  assert.equal(result.driver_phase.product_owned_resource_coverage_status, 'unknown');
  assert.equal(result.evidence_validity_gate.coordinator_scope_cleanup_probe_complete, true);
  assert.equal(result.evidence_validity_gate.product_owned_resource_coverage_verified, false);
  assert.equal(result.evidence_validity_gate.passed, false);
  assert.equal(result.business_gate_passed, false);
});

test('truth stays quarantined when an exited driver has unknown owned-resource cleanup', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const driverScript = writeScript(join(root, 'cleanup-gate-driver.mjs'), cleanupGateDriverSource);
  const evaluatorScript = writeScript(join(root, 'must-not-evaluate.mjs'), fakeEvaluatorSource);
  const driverCommandManifest = writeCommandManifest(root, 'driver', driverScript);
  const evaluatorCommandManifest = writeCommandManifest(root, 'evaluator', evaluatorScript);
  const outDirectory = join(root, 'cleanup-gate-output');
  const result = await runBusinessSuite({
    ...files,
    outDirectory,
    driverPath: realpathSync(process.execPath),
    driverCommandManifest,
    driverArgs: [driverScript],
    evaluatorPath: realpathSync(process.execPath),
    evaluatorCommandManifest,
    evaluatorArgs: [evaluatorScript],
    scope: 'm3-fixture-diagnostic',
    diagnosticSeed: 1,
    timeoutMs: 10_000,
  });

  assert.equal(result.executed_driver_count, 2);
  assert.equal(result.driver_phase.all_driver_children_terminated, true);
  assert.equal(result.driver_phase.all_owned_resources_cleanup_self_reported_clean, false);
  assert.equal(result.runs[1].driver_process.exit_confirmed, true);
  assert.equal(result.runs[1].driver_process.timed_out, false);
  assert.equal(result.runs[0].evaluator.status, 'not_run');
  assert.equal(result.runs[1].evaluator.status, 'not_run');
  assert.equal(result.acceptance_boundary.independent_truth_loaded_after_all_driver_children_exited, false);
  assert.equal(result.acceptance_boundary.truth_load_failure_code, 'DRIVER_OWNED_RESOURCE_CLEANUP_NOT_SELF_REPORTED_CLEAN');
  assert.deepEqual(readdirSync(join(outDirectory, 'evaluation')), []);
  assert.equal(result.unrun.pending_identities.length, 18);
  assert.equal(result.business_results.fixture.aggregator.expected_runs, 100);
  assert.equal(result.business_results.fixture.aggregator.observed_runs, 2);
  assert.equal(result.business_results.fixture.aggregator.missing_runs.length, 98);
  assert.equal(result.business_gate_passed, false);
});

test('a timed-out driver is killed as the exact child and stops the remaining diagnostic runs', async t => {
  const root = temporaryRoot(t);
  const files = createProfileFiles(root);
  const stuckScript = writeScript(join(root, 'stuck-driver.mjs'), "process.on('SIGTERM', () => {}); while (true) {}\n");
  const evaluatorScript = writeScript(join(root, 'unused-evaluator.mjs'), "process.exit(0);\n");
  const driverCommandManifest = writeCommandManifest(root, 'driver', stuckScript);
  const evaluatorCommandManifest = writeCommandManifest(root, 'evaluator', evaluatorScript);
  const result = await runBusinessSuite({
    ...files,
    outDirectory: join(root, 'timeout-output'),
    driverPath: realpathSync(process.execPath),
    driverCommandManifest,
    driverArgs: [stuckScript],
    evaluatorPath: realpathSync(process.execPath),
    evaluatorCommandManifest,
    evaluatorArgs: [evaluatorScript],
    scope: 'm3-fixture-diagnostic',
    diagnosticSeed: 1,
    timeoutMs: 100,
    graceMs: 50,
    killWaitMs: 1500,
  });
  assert.equal(result.executed_driver_count, 1);
  assert.equal(result.runs[0].driver_process.timed_out, true);
  assert.equal(result.runs[0].driver_process.exit_confirmed, true);
  assert.equal(result.driver_phase.all_owned_resources_cleanup_self_reported_clean, false);
  assert.equal(result.runs[0].status, 'DRIVER_TIMEOUT');
  assert.equal(result.unrun.pending_identities.length, 19);
  assert.equal(result.business_results.fixture.aggregator.expected_runs, 100);
  assert.equal(result.business_results.fixture.aggregator.observed_runs, 1);
  assert.equal(result.business_results.fixture.business_quality_thresholds_met, false);
  assert.equal(result.acceptance_boundary.direct_args_env_goal_only_excludes_truth, true);
});
