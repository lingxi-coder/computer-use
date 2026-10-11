#!/usr/bin/env node
// Offline numerical parity; this is not task quality or product acceptance.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';
import * as ort from 'onnxruntime-node';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const revision = 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67';
const exportRoot = join(root, 'target/model-exports/laya-multilingual', revision);
const bundle = join(exportRoot, 'bundle');
const manifestPath = join(exportRoot, 'package-manifest.json');
const goldenPath = join(root, 'artifacts/m0-laya-tokenized-parity.json');
const output = join(root, 'artifacts', `m0-laya-node-cpu-${randomUUID()}.json`);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const expected = {
  'encoder.onnx': [2771288, '85080d470855319def232f3927d1d8d8794c23a2fae943b3f4564a1e8d3204d0'],
  'encoder.onnx.data': [1227816960, 'f1e49a027352064d72831863456be6160748854296bff8332df14130dbf79834'],
  'head.onnx': [244928, '66d3710bedbfa1043411e8de7c60927f2a279105d18e4f1b9db175e06452a02a'],
  'head.onnx.data': [59899904, '0292a4eb478c2c3ec0b6555b4a4093bc802e965654d7aeb4ac22907a17f261d3'],
};
const record = { kind: 'laya_node_cpu_numerical_parity', started_at: new Date().toISOString(),
  model_revision: revision, runtime_version: ort.env.versions.node, node_version: process.version,
  cpu_only: true, num_threads: 4, ok: false, quality_verified: false,
  product_runtime_verified: false, shared_load_latency_verified: false, mvp_complete: false };
let encoder; let head; let phase = 'resource_validation';

async function fileHash(path) {
  const digest = createHash('sha256');
  for await (const chunk of createReadStream(path, { highWaterMark: 256 * 1024 })) digest.update(chunk);
  return digest.digest('hex');
}
function softmax(values) {
  assert.ok(values.length > 0 && values.every(Number.isFinite));
  const maximum = Math.max(...values);
  const exps = values.map(value => Math.exp(value - maximum));
  const total = exps.reduce((a, b) => a + b, 0);
  return exps.map(value => value / total);
}
function int64(values, dims) { return new ort.Tensor('int64', BigInt64Array.from(values, BigInt), dims); }

try {
  assert.equal(ort.env.versions.node, '1.30.0');
  const manifestBytes = readFileSync(manifestPath);
  const manifest = JSON.parse(manifestBytes);
  const goldenBytes = readFileSync(goldenPath);
  const golden = JSON.parse(goldenBytes);
  assert.equal(manifest.model_revision, revision);
  assert.equal(golden.model_revision, revision);
  assert.equal(golden.status, 'tokenized_cpu_parity_passed');
  assert.equal(golden.package_manifest_sha256, hash(manifestBytes));
  assert.equal(golden.cases.length, 6);
  assert.equal(golden.batches.length, 3);
  for (const [name, [bytes, sha256]] of Object.entries(expected)) {
    const path = join(bundle, name);
    assert.equal(statSync(path).size, bytes);
    assert.equal(await fileHash(path), sha256);
    assert.equal(manifest.package_files[name].sha256, sha256);
  }
  record.package_manifest_sha256 = hash(manifestBytes);
  record.python_golden_sha256 = hash(goldenBytes);
  record.script_sha256 = hash(readFileSync(fileURLToPath(import.meta.url)));

  phase = 'cpu_session_load';
  const loadStarted = performance.now();
  const options = { executionProviders: ['cpu'], intraOpNumThreads: 4, interOpNumThreads: 1 };
  encoder = await ort.InferenceSession.create(join(bundle, 'encoder.onnx'), options);
  head = await ort.InferenceSession.create(join(bundle, 'head.onnx'), options);
  record.session_load_ms = performance.now() - loadStarted;
  assert.equal(encoder.outputNames.length, 1);
  assert.equal(head.outputNames.length, 2);
  record.graph_signatures = { encoder_inputs: encoder.inputNames, encoder_outputs: encoder.outputNames,
    head_inputs: head.inputNames, head_outputs: head.outputNames };

  phase = 'cpu_numerical_parity';
  const byName = new Map(golden.cases.map(item => [item.name, item]));
  const results = [];
  for (const batch of golden.batches) {
    const cases = batch.predictions_and_expectations.map(row => byName.get(row.name));
    assert.ok(cases.length > 0 && cases.every(Boolean));
    assert.equal(cases.length, batch.rows);
    const width = batch.padded_sequence_length;
    const markers = Math.max(...cases.map(item => item.marker_positions.length));
    assert.ok(Number.isInteger(width) && width > 0 && width <= 512);
    assert.ok(Number.isInteger(markers) && markers > 0 && markers <= 16);
    assert.ok(cases.every(item => item.input_ids.length > 0 && item.input_ids.length <= width
      && item.attention_mask.length === item.input_ids.length
      && item.input_ids.every(id => Number.isSafeInteger(id) && id >= 0 && id <= 0x7fff_ffff)
      && item.marker_positions.every(pos => Number.isInteger(pos) && pos >= 0 && pos < item.input_ids.length)));
    const ids = cases.flatMap(item => [...item.input_ids, ...Array(width - item.input_ids.length).fill(0)]);
    const attention = cases.flatMap(item => [...item.attention_mask, ...Array(width - item.attention_mask.length).fill(0)]);
    const positions = cases.flatMap(item => [...item.marker_positions, ...Array(markers - item.marker_positions.length).fill(0)]);
    const markerMask = cases.flatMap(item => Array.from({ length: markers }, (_, i) => i < item.marker_positions.length ? 1 : 0));
    assert.equal(hash(Buffer.from(BigInt64Array.from(ids, BigInt).buffer)), batch.input_ids_sha256);
    assert.equal(hash(Buffer.from(BigInt64Array.from(attention, BigInt).buffer)), batch.attention_mask_sha256);
    const dims = [cases.length, width];
    const inputIds = int64(ids, dims); const mask = int64(attention, dims);
    const start = performance.now();
    const hidden = (await encoder.run({ input_ids: inputIds, attention_mask: mask }))[encoder.outputNames[0]];
    assert.equal(hidden.type, 'float32');
    assert.deepEqual(hidden.dims.slice(0, 2), dims);
    const outputs = await head.run({ hidden_states: hidden, attention_mask: mask,
      marker_pos: int64(positions, [cases.length, markers]),
      marker_mask: new ort.Tensor('bool', Uint8Array.from(markerMask), [cases.length, markers]),
      qtype: int64(cases.map(item => item.qtype_id), [cases.length, 1]) });
    const inferenceMs = performance.now() - start;
    const logits = outputs[head.outputNames[0]];
    assert.equal(logits.type, 'float32');
    assert.deepEqual(logits.dims, [cases.length, markers]);
    let maximumError = 0;
    const predictions = cases.map((item, index) => {
      const probabilities = softmax(Array.from(logits.data.slice(index * markers, index * markers + item.marker_positions.length)));
      const reference = batch.predictions_and_expectations.find(row => row.name === item.name);
      assert.equal(reference.probabilities.length, probabilities.length);
      const error = Math.max(...probabilities.map((p, i) => Math.abs(p - reference.probabilities[i])));
      maximumError = Math.max(maximumError, error);
      assert.ok(error <= 1e-4);
      const selected = probabilities.indexOf(Math.max(...probabilities));
      const prediction = item.qtype === 'noul' ? probabilities[1] >= 0.5
        : item.rendered_options[selected].split(':')[0];
      assert.equal(prediction, reference.prediction);
      return { name: item.name, probabilities, max_abs_probability_error: error,
        node_prediction: prediction, reference_prediction: reference.prediction, expected_action: reference.expected_action,
        reference_matches_expectation: reference.prediction === reference.expected_action,
        node_selected_index: selected, quality_gate: false };
    });
    results.push({ name: batch.name, rows: cases.length, inference_ms: inferenceMs,
      max_abs_probability_error: maximumError, predictions });
  }
  record.batches = results;
  record.max_abs_probability_error = Math.max(...results.map(item => item.max_abs_probability_error));
  record.quality_expectation_mismatches_preserved = results.flatMap(item => item.predictions)
    .filter(item => !item.reference_matches_expectation).length;
  record.ok = true;
} catch {
  record.failure_phase = phase;
  process.exitCode = 1;
} finally {
  await head?.release(); await encoder?.release();
  record.finished_at = new Date().toISOString();
  record.process_peak_rss_bytes = process.resourceUsage().maxRSS * 1024;
  writeFileSync(output, JSON.stringify(record, null, 2) + '\n', { flag: 'wx' });
  console.log(JSON.stringify({ ok: record.ok, artifact: output, phase, quality_verified: false, mvp_complete: false }));
}
