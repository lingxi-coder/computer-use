#!/usr/bin/env node

import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdir, readFile, writeFile, link, unlink } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { performance } from "node:perf_hooks";

const ROOT = resolve(dirname(new URL(import.meta.url).pathname), "..");
const REVISION = "e4e9ddf21a7b1903b7acffd8814ad4307bf63a67";
const SOURCE_COMMIT = "2e4d9c87e8b1621deb344eac7de5c7258f32f849";
const SOURCE_DIR = join(ROOT, "target/model-sources/laya-2e4d9c87");
const SOURCE_TS_DIR = join(SOURCE_DIR, "laya-ts/src");
const RUNTIME_DIR = join(ROOT, "target/model-runtime/laya-tokenizer");
const TOKENIZER_PATH = join(ROOT, "target/model-exports/laya-multilingual", REVISION, "bundle/tokenizer.json");
const PACKAGE_MANIFEST_PATH = join(ROOT, "target/model-exports/laya-multilingual", REVISION, "package-manifest.json");
const PY_ARTIFACT_PATH = join(ROOT, "artifacts/m0-laya-tokenized-parity.json");
const ARTIFACT_PATH = join(ROOT, "artifacts/m0-laya-ts-tokenizer-parity.json");
const PYTHON = join(ROOT, "target/model-export-runtime/py312/bin/python");
const EXPECTED_TOKENIZER_SHA256 = "609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f";
const EXPECTED_BLOBS = {
  "laya-ts/src/tokenizer.ts": "3e460d0d4ba42b069b432a86e714b9b688e3cc59",
  "laya-ts/src/common.ts": "38f170ed69a0614315bf57a30e22e2dd6090aea7",
};

function sha256Bytes(data) {
  return createHash("sha256").update(data).digest("hex");
}

function gitOutput(args) {
  return execFileSync("git", args, { cwd: SOURCE_DIR, encoding: "utf8" }).trim();
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function firstDifference(a, b) {
  if (a.length !== b.length) return { index: Math.min(a.length, b.length), expectedLength: a.length, actualLength: b.length };
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return { index: i, expected: a[i], actual: b[i] };
  return null;
}

function int64Buffer(rows) {
  const values = rows.flat();
  const buffer = Buffer.alloc(values.length * 8);
  values.forEach((value, index) => buffer.writeBigInt64LE(BigInt(value), index * 8));
  return buffer;
}

function padRows(items, padValue) {
  const width = Math.max(...items.map((item) => item.length));
  return items.map((item) => [...item, ...Array(width - item.length).fill(padValue)]);
}

async function writeArtifactOnce(artifact) {
  await mkdir(dirname(ARTIFACT_PATH), { recursive: true });
  const temp = `${ARTIFACT_PATH}.part-${process.pid}`;
  await writeFile(temp, `${JSON.stringify(artifact, null, 2)}\n`, { flag: "wx" });
  try {
    await link(temp, ARTIFACT_PATH);
  } catch (error) {
    if (error.code === "EEXIST") throw new Error(`refusing to overwrite previous artifact: ${ARTIFACT_PATH}`);
    throw error;
  } finally {
    await unlink(temp).catch(() => {});
  }
}

async function pythonExamples() {
  const code = [
    "import importlib.util,json,pathlib,sys",
    "p=pathlib.Path(sys.argv[1])",
    "spec=importlib.util.spec_from_file_location('laya_parity_fixture',p)",
    "module=importlib.util.module_from_spec(spec)",
    "spec.loader.exec_module(module)",
    "print(json.dumps(module.build_examples(),ensure_ascii=False,separators=(',',':')))"
  ].join(";");
  const result = spawnSync(PYTHON, ["-c", code, join(ROOT, "scripts/check-laya-onnx-parity.py")], {
    cwd: ROOT,
    encoding: "utf8",
    env: {
      PATH: process.env.PATH ?? "/usr/bin:/bin:/usr/sbin:/sbin",
      PYTHONNOUSERSITE: "1",
      PYTHONDONTWRITEBYTECODE: "1",
      HF_HUB_OFFLINE: "1",
      TRANSFORMERS_OFFLINE: "1",
    },
    maxBuffer: 1024 * 1024,
  });
  if (result.status !== 0) throw new Error(`fixture export failed: ${result.stderr || result.error || result.status}`);
  return JSON.parse(result.stdout);
}

async function run() {
  assert(!(await import("node:fs/promises").then(({ access }) => access(ARTIFACT_PATH).then(() => true).catch(() => false))), "artifact already exists; preserving it");
  const started = performance.now();
  const generatedAt = new Date().toISOString();
  const sourceHead = gitOutput(["rev-parse", "HEAD"]);
  assert(sourceHead === SOURCE_COMMIT, "source checkout is not the pinned commit");
  assert(gitOutput(["status", "--porcelain", "--untracked-files=all"]) === "", "source checkout is dirty");
  const sourceBlobHashes = {};
  for (const [relativePath, expected] of Object.entries(EXPECTED_BLOBS)) {
    const actual = gitOutput(["rev-parse", `${SOURCE_COMMIT}:${relativePath}`]);
    assert(actual === expected, `pinned TypeScript source blob changed: ${relativePath}`);
    sourceBlobHashes[relativePath] = actual;
  }

  const tokenizerBytes = await readFile(TOKENIZER_PATH);
  const tokenizerSha256 = sha256Bytes(tokenizerBytes);
  assert(tokenizerSha256 === EXPECTED_TOKENIZER_SHA256, "tokenizer source asset hash mismatch");
  const packageManifest = JSON.parse(await readFile(PACKAGE_MANIFEST_PATH, "utf8"));
  assert(packageManifest.model_revision === REVISION, "ONNX package manifest revision mismatch");
  assert(packageManifest.package_files?.["tokenizer.json"]?.sha256 === tokenizerSha256, "package manifest tokenizer hash mismatch");

  const pythonGolden = JSON.parse(await readFile(PY_ARTIFACT_PATH, "utf8"));
  assert(pythonGolden.model_revision === REVISION, "Python parity artifact revision mismatch");
  assert(pythonGolden.status === "tokenized_cpu_parity_passed", "Python tokenization golden is not a passing run");
  const expectedByName = new Map(pythonGolden.cases.map((item) => [item.name, item]));
  const examples = await pythonExamples();
  assert(examples.length === 6 && expectedByName.size === 6, "expected six Python parity cases");

  const tsc = join(ROOT, "node_modules/.bin/tsc");
  const compileCommand = [
    tsc,
    "--target", "ES2022",
    "--module", "NodeNext",
    "--moduleResolution", "NodeNext",
    "--strict",
    "--skipLibCheck",
    "--rootDir", SOURCE_TS_DIR,
    "--outDir", RUNTIME_DIR,
    join(SOURCE_TS_DIR, "tokenizer.ts"),
    join(SOURCE_TS_DIR, "common.ts"),
  ];
  const compileStarted = performance.now();
  const compile = spawnSync(compileCommand[0], compileCommand.slice(1), {
    cwd: ROOT,
    encoding: "utf8",
    env: { PATH: process.env.PATH ?? "/usr/bin:/bin:/usr/sbin:/sbin", NODE_OPTIONS: "--disable-warning=ExperimentalWarning" },
    maxBuffer: 2 * 1024 * 1024,
  });
  const compileElapsedMs = performance.now() - compileStarted;
  if (compile.status !== 0) throw new Error(`pinned TS compile failed: ${compile.stderr || compile.stdout || compile.error}`);

  const tokenizerModule = await import(pathToFileURL(join(RUNTIME_DIR, "tokenizer.js")).href);
  const commonModule = await import(pathToFileURL(join(RUNTIME_DIR, "common.js")).href);
  const rawTokenizer = JSON.parse(tokenizerBytes.toString("utf8"));
  const tokenizerData = tokenizerModule.parseTokenizerJson(rawTokenizer);
  assert(tokenizerData !== null, "TypeScript parseTokenizerJson rejected the pinned tokenizer");
  const tok = {
    clsId: tokenizerData.ids.cls,
    sepId: tokenizerData.ids.sep,
    maskId: tokenizerData.ids.mask,
    padId: tokenizerData.ids.pad,
    maskToken: tokenizerData.maskToken,
    encode: (text) => tokenizerModule.encodeWithData(tokenizerData, text),
  };
  assert(JSON.stringify(tokenizerData.ids) === JSON.stringify({ cls: 2, sep: 1, mask: 4, pad: 0, unk: 3 }), "TypeScript special-token IDs mismatch");

  const actualByName = new Map();
  const rows = [];
  for (const example of examples) {
    const golden = expectedByName.get(example.name);
    assert(golden !== undefined, `Python golden missing case ${example.name}`);
    const built = commonModule.buildSequence(
      tok,
      example.state,
      example.question,
      512,
      192,
    );
    const inputIds = built.ids;
    const markerPositions = built.markers;
    const truncation = built.stats;
    const options = commonModule.renderOptions(example.question);
    const qtypeId = { choice: 0, score: 1, noul: 2 }[example.question.t];
    const checks = {
      input_ids: firstDifference(golden.input_ids, inputIds),
      marker_positions: firstDifference(golden.marker_positions, markerPositions),
      rendered_options: JSON.stringify(golden.rendered_options) === JSON.stringify(options) ? null : { expected: golden.rendered_options, actual: options },
      qtype: golden.qtype_id === qtypeId ? null : { expected: golden.qtype_id, actual: qtypeId },
      sequence_length: golden.sequence_length === inputIds.length ? null : { expected: golden.sequence_length, actual: inputIds.length },
      state_token_count: golden.state_token_count_before_sequence_clamp === truncation.state_tokens ? null : { expected: golden.state_token_count_before_sequence_clamp, actual: truncation.state_tokens },
      state_tokens_used: golden.state_tokens_used === truncation.state_tokens_used ? null : { expected: golden.state_tokens_used, actual: truncation.state_tokens_used },
      truncation: golden.truncated_to_max_len === truncation.truncated ? null : { expected: golden.truncated_to_max_len, actual: truncation.truncated },
      option_count: options.length === truncation.options.total ? null : { expected: truncation.options.total, actual: options.length },
      mask_token_ids: markerPositions.every((position) => inputIds[position] === tok.maskId) ? null : { expected_mask_id: tok.maskId },
    };
    const passed = Object.values(checks).every((value) => value === null);
    actualByName.set(example.name, { example, inputIds, markerPositions, options, qtypeId, truncation });
    rows.push({
      name: example.name,
      batch: example.batch,
      qtype: example.question.t,
      qtype_id: qtypeId,
      token_ids_byte_for_byte_equal: checks.input_ids === null,
      marker_positions_equal: checks.marker_positions === null,
      rendered_options_equal: checks.rendered_options === null,
      sequence_length_equal: checks.sequence_length === null,
      truncation_facts_equal: checks.state_token_count === null && checks.state_tokens_used === null && checks.truncation === null,
      option_count_equal: checks.option_count === null,
      python_sequence_length: golden.sequence_length,
      typescript_sequence_length: inputIds.length,
      marker_positions: markerPositions,
      marker_token_ids: markerPositions.map((position) => inputIds[position]),
      rendered_options: options,
      python_truncation: {
        state_tokens: golden.state_token_count_before_sequence_clamp,
        state_tokens_used: golden.state_tokens_used,
        truncated: golden.truncated_to_max_len,
      },
      typescript_truncation: truncation,
      first_differences: Object.fromEntries(Object.entries(checks).filter(([, value]) => value !== null)),
      passed,
    });
  }

  const batchResults = [];
  for (const goldenBatch of pythonGolden.batches) {
    const batchExamples = examples.filter((example) => example.batch === goldenBatch.name);
    const actualRows = batchExamples.map((example) => actualByName.get(example.name));
    const lengths = actualRows.map((row) => row.inputIds.length);
    const padRowsActual = padRows(actualRows.map((row) => row.inputIds), tok.padId);
    const attentionRows = actualRows.map((row) => Array(row.inputIds.length).fill(1));
    const padAttention = padRows(attentionRows, 0);
    const inputHash = sha256Bytes(int64Buffer(padRowsActual));
    const maskHash = sha256Bytes(int64Buffer(padAttention));
    batchResults.push({
      name: goldenBatch.name,
      row_count_equal: actualRows.length === goldenBatch.rows,
      sequence_lengths_equal: JSON.stringify(lengths) === JSON.stringify(goldenBatch.sequence_lengths),
      marker_counts_equal: JSON.stringify(actualRows.map((row) => row.markerPositions.length)) === JSON.stringify(goldenBatch.marker_counts),
      padded_length_equal: padRowsActual[0].length === goldenBatch.padded_sequence_length,
      padded_input_ids_hash_equal: inputHash === goldenBatch.input_ids_sha256,
      padded_attention_mask_hash_equal: maskHash === goldenBatch.attention_mask_sha256,
      actual_padded_input_ids_sha256: inputHash,
      expected_padded_input_ids_sha256: goldenBatch.input_ids_sha256,
      actual_padded_attention_mask_sha256: maskHash,
      expected_padded_attention_mask_sha256: goldenBatch.attention_mask_sha256,
      passed: actualRows.length === goldenBatch.rows
        && JSON.stringify(lengths) === JSON.stringify(goldenBatch.sequence_lengths)
        && JSON.stringify(actualRows.map((row) => row.markerPositions.length)) === JSON.stringify(goldenBatch.marker_counts)
        && padRowsActual[0].length === goldenBatch.padded_sequence_length
        && inputHash === goldenBatch.input_ids_sha256
        && maskHash === goldenBatch.attention_mask_sha256,
    });
  }

  const allPassed = rows.every((row) => row.passed) && batchResults.every((batch) => batch.passed);
  const artifact = {
    schema_version: 1,
    kind: "m0_laya_typescript_tokenizer_parity",
    status: allPassed ? "tokenizer_parity_passed" : "tokenizer_parity_failed",
    generated_at: generatedAt,
    finished_at: new Date().toISOString(),
    model_revision: REVISION,
    source_commit: SOURCE_COMMIT,
    source_file_git_blobs: sourceBlobHashes,
    tokenizer_sha256: tokenizerSha256,
    package_manifest_sha256: sha256Bytes(await readFile(PACKAGE_MANIFEST_PATH)),
    python_golden_artifact_sha256: sha256Bytes(await readFile(PY_ARTIFACT_PATH)),
    checker_sha256: sha256Bytes(await readFile(fileURLToPath(import.meta.url))),
    runtime: {
      node_version: process.version,
      typescript_compiler_version: execFileSync(join(ROOT, "node_modules/.bin/tsc"), ["--version"], { cwd: ROOT, encoding: "utf8" }).trim(),
      python_version: execFileSync(PYTHON, ["--version"], { cwd: ROOT, encoding: "utf8" }).trim(),
      compile_command: compileCommand,
      compile_elapsed_ms: compileElapsedMs,
      total_elapsed_ms: performance.now() - started,
      network_used: false,
      onnxruntime_run: false,
    },
    special_token_ids: tokenizerData.ids,
    rows,
    batches: batchResults,
    claims: {
      token_ids_markers_options_lengths_truncation_match_python_golden: allPassed,
      node_onnx_inference_verified: false,
      task_quality_verified: false,
      latency_benchmark_verified: false,
    },
  };
  await writeArtifactOnce(artifact);
  return artifact;
}

try {
  const artifact = await run();
  process.stdout.write(`${JSON.stringify({ status: artifact.status, artifact: ARTIFACT_PATH, rows: artifact.rows.map((row) => ({ name: row.name, passed: row.passed })), batches: artifact.batches.map((batch) => ({ name: batch.name, passed: batch.passed })), timing_ms: artifact.runtime.total_elapsed_ms }, null, 2)}\n`);
  if (artifact.status !== "tokenizer_parity_passed") process.exitCode = 1;
} catch (error) {
  process.stderr.write(`check-laya-ts-tokenizer: ${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
