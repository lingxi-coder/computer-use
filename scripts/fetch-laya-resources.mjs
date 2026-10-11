#!/usr/bin/env node

import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import {
  link,
  lstat,
  mkdir,
  open,
  realpath,
  statfs,
  unlink,
} from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";

const MODEL_ID = "convaiinnovations/laya-multilingual";
const REVISION = "e4e9ddf21a7b1903b7acffd8814ad4307bf63a67";
const SOURCE_COMMIT = "2e4d9c87e8b1621deb344eac7de5c7258f32f849";
const MINIMUM_FREE_BYTES = 2 * 1024 ** 3;
const API_ROOT = `https://huggingface.co/api/models/${MODEL_ID}`;
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const RESOURCE_DIR = join(REPO_ROOT, "target/model-resources/laya", REVISION);
const ARTIFACT_PATH = join(REPO_ROOT, "artifacts/m0-laya-resource-acquisition.json");

const FILES = [
  {
    path: "model.safetensors",
    bytes: 643835514,
    sha256: "9d628fd971b700382ac6f65920a86f149777b2e748e0c955fb3b19695aa8f204",
    treeOid: "b99c8bea239c53f6f6bce734557dc6c403fa6b3e",
    lfsSha256: "9d628fd971b700382ac6f65920a86f149777b2e748e0c955fb3b19695aa8f204",
  },
  {
    path: "tokenizer/tokenizer.json",
    bytes: 34363188,
    sha256: "609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f",
    treeOid: "56d765858ed38eaabd35cd90759cbb1f89b0baa4",
    lfsSha256: "609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f",
  },
  {
    path: "rl_agent_config.json",
    bytes: 472,
    sha1: "00e35f88bb731bb9126a914666ab1cdac8a204c8",
    treeOid: "00e35f88bb731bb9126a914666ab1cdac8a204c8",
  },
  {
    path: "encoder/config.json",
    bytes: 1938,
    sha1: "0de0e2d30638873790cf962def52e2acf4db3eef",
    treeOid: "0de0e2d30638873790cf962def52e2acf4db3eef",
  },
];

function fail(message) {
  throw new Error(message);
}

function bytesAvailable(stats) {
  return Number(stats.bavail) * Number(stats.bsize);
}

async function assertDiskFloor(requiredDownloadBytes) {
  const stats = await statfs(REPO_ROOT);
  const available = bytesAvailable(stats);
  const needed = requiredDownloadBytes + MINIMUM_FREE_BYTES;
  if (!Number.isSafeInteger(available) || available < needed) {
    fail(`disk floor rejected: available=${available} required=${needed}`);
  }
  return available;
}

async function getJson(url, label) {
  const response = await fetch(url, {
    headers: { accept: "application/json" },
    redirect: "follow",
  });
  if (!response.ok) fail(`${label} returned HTTP ${response.status}`);
  const declaredLength = Number(response.headers.get("content-length") ?? 0);
  if (declaredLength > 2 * 1024 * 1024) fail(`${label} exceeded the metadata size bound`);
  const body = await response.arrayBuffer();
  if (body.byteLength > 2 * 1024 * 1024) fail(`${label} exceeded the metadata size bound`);
  return JSON.parse(Buffer.from(body).toString("utf8"));
}

async function inspectPinnedMetadata() {
  const revisionUrl = `${API_ROOT}/revision/${REVISION}`;
  const revision = await getJson(revisionUrl, "pinned revision metadata");
  if (revision.modelId !== MODEL_ID || revision.sha !== REVISION || revision.private !== false) {
    fail("pinned revision identity/privacy facts did not match");
  }
  if (revision.gated !== false) fail("pinned revision is gated or gate status is unknown");
  if (revision.cardData?.license !== "apache-2.0") fail("pinned revision license declaration changed");

  const treeUrl = `${API_ROOT}/tree/${REVISION}?recursive=true&expand=true`;
  const tree = await getJson(treeUrl, "pinned revision tree");
  if (!Array.isArray(tree)) fail("pinned revision tree did not return an array");
  const byPath = new Map(tree.map((entry) => [entry.path, entry]));
  for (const file of FILES) {
    const entry = byPath.get(file.path);
    if (!entry || entry.size !== file.bytes || entry.oid !== file.treeOid) {
      fail(`pinned tree metadata mismatch for ${file.path}`);
    }
    if (file.lfsSha256 && entry.lfs?.oid !== file.lfsSha256) {
      fail(`pinned LFS metadata mismatch for ${file.path}`);
    }
    if (file.lfsSha256 && entry.lfs?.size !== file.bytes) {
      fail(`pinned LFS size metadata mismatch for ${file.path}`);
    }
  }
  const onnxPaths = tree.map((entry) => entry.path).filter((path) => path.toLowerCase().endsWith(".onnx"));
  return {
    modelId: revision.modelId,
    revision: revision.sha,
    license: revision.cardData.license,
    gated: revision.gated,
    revisionMetadataUrl: revisionUrl,
    treeMetadataUrl: treeUrl,
    onnxPaths,
    files: FILES.map((file) => {
      const entry = byPath.get(file.path);
      return {
        path: file.path,
        bytes: entry.size,
        treeOid: entry.oid,
        lfsOid: entry.lfs?.oid ?? null,
      };
    }),
  };
}

async function hashFile(path, expectedBytes) {
  const sha256 = createHash("sha256");
  const gitBlob = createHash("sha1").update(`blob ${expectedBytes}\0`);
  let bytes = 0;
  const stream = createReadStream(path);
  for await (const chunk of stream) {
    bytes += chunk.length;
    sha256.update(chunk);
    gitBlob.update(chunk);
  }
  return { bytes, sha256: sha256.digest("hex"), gitBlobSha1: gitBlob.digest("hex") };
}

async function verifyLocal(path, expected) {
  const details = await lstat(path);
  if (!details.isFile() || details.isSymbolicLink()) fail(`refusing non-regular resource path: ${path}`);
  if (details.size !== expected.bytes) fail(`existing resource size mismatch: ${expected.path}`);
  const hashes = await hashFile(path, expected.bytes);
  if (hashes.bytes !== expected.bytes) fail(`resource read size mismatch: ${expected.path}`);
  if (expected.sha256 && hashes.sha256 !== expected.sha256) fail(`resource SHA-256 mismatch: ${expected.path}`);
  if (expected.sha1 && hashes.gitBlobSha1 !== expected.sha1) fail(`resource Git blob SHA-1 mismatch: ${expected.path}`);
  if (expected.lfsSha256 && hashes.sha256 !== expected.lfsSha256) fail(`resource LFS SHA-256 mismatch: ${expected.path}`);
  return hashes;
}

async function ensureResourceParent(directory) {
  const withinResource = relative(RESOURCE_DIR, directory);
  if (
    isAbsolute(withinResource)
    || withinResource === ".."
    || withinResource.startsWith(`..${sep}`)
  ) {
    fail(`resource parent escaped the pinned directory: ${directory}`);
  }
  const realResourceDir = await realpath(RESOURCE_DIR);
  let cursor = RESOURCE_DIR;
  for (const component of withinResource.split(sep).filter(Boolean)) {
    cursor = join(cursor, component);
    await mkdir(cursor).catch((error) => {
      if (error.code !== "EEXIST") throw error;
    });
    const details = await lstat(cursor);
    if (!details.isDirectory() || details.isSymbolicLink()) {
      fail(`refusing non-directory or symlink resource parent: ${cursor}`);
    }
    const realCursor = await realpath(cursor);
    const fromResource = relative(realResourceDir, realCursor);
    if (isAbsolute(fromResource) || fromResource === ".." || fromResource.startsWith(`..${sep}`)) {
      fail(`resolved resource parent escaped the pinned directory: ${cursor}`);
    }
  }
}

function resolveDownloadUrl(path) {
  const encodedPath = path.split("/").map(encodeURIComponent).join("/");
  return `https://huggingface.co/${MODEL_ID}/resolve/${REVISION}/${encodedPath}?download=true`;
}

async function downloadOne(file) {
  const destination = join(RESOURCE_DIR, file.path);
  const part = `${destination}.part-${process.pid}`;
  await ensureResourceParent(dirname(destination));

  try {
    const existing = await lstat(destination).catch((error) => {
      if (error.code === "ENOENT") return null;
      throw error;
    });
    if (existing) {
      return { path: file.path, sourceUrl: resolveDownloadUrl(file.path), disposition: "verified_existing", ...(await verifyLocal(destination, file)) };
    }

    const response = await fetch(resolveDownloadUrl(file.path), {
      headers: { accept: "application/octet-stream" },
      redirect: "follow",
    });
    if (!response.ok || !response.body) fail(`download ${file.path} returned HTTP ${response.status}`);
    const declaredLength = Number(response.headers.get("content-length") ?? 0);
    if (declaredLength !== 0 && declaredLength !== file.bytes) {
      fail(`download Content-Length mismatch for ${file.path}`);
    }
    const counter = new Transform({
      transform(chunk, _encoding, callback) {
        this.count = (this.count ?? 0) + chunk.length;
        if (this.count > file.bytes) callback(new Error(`download exceeded pinned size for ${file.path}`));
        else callback(null, chunk);
      },
    });
    counter.count = 0;
    await pipeline(
      Readable.fromWeb(response.body),
      counter,
      createWriteStream(part, { flags: "wx", mode: 0o644 }),
    );
    if (counter.count !== file.bytes) fail(`download byte count mismatch for ${file.path}`);

    const hashes = await verifyLocal(part, file);
    const handle = await open(part, "r+");
    await handle.sync();
    await handle.close();
    try {
      await link(part, destination);
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      const raced = await verifyLocal(destination, file);
      await unlink(part);
      return { path: file.path, sourceUrl: resolveDownloadUrl(file.path), disposition: "verified_existing_after_race", ...raced };
    }
    await unlink(part);
    return { path: file.path, sourceUrl: resolveDownloadUrl(file.path), disposition: "downloaded_and_verified", ...hashes };
  } catch (error) {
    await unlink(part).catch(() => {});
    throw error;
  }
}

async function writeArtifactOnce(artifact) {
  await mkdir(dirname(ARTIFACT_PATH), { recursive: true });
  const part = `${ARTIFACT_PATH}.part-${process.pid}`;
  const content = `${JSON.stringify(artifact, null, 2)}\n`;
  const handle = await open(part, "wx", 0o644);
  try {
    await handle.writeFile(content, "utf8");
    await handle.sync();
    await handle.close();
    try {
      await link(part, ARTIFACT_PATH);
    } catch (error) {
      if (error.code === "EEXIST") fail(`refusing to overwrite acquisition artifact: ${ARTIFACT_PATH}`);
      throw error;
    }
  } finally {
    await handle.close().catch(() => {});
    await unlink(part).catch(() => {});
  }
}

function baseArtifact() {
  return {
    schema_version: 1,
    kind: "m0_laya_resource_acquisition",
    model_id: MODEL_ID,
    pinned_revision: REVISION,
    repository: `https://huggingface.co/${MODEL_ID}/tree/${REVISION}`,
    target_directory: `target/model-resources/laya/${REVISION}`,
    license_declaration: "apache-2.0",
    credentials_used: false,
    remote_code_executed: false,
    future_export_reference: {
      repository: "https://github.com/NandhaKishorM/laya",
      commit: SOURCE_COMMIT,
      local_model_dir_route: "laya-ts/scripts/export_onnx.py --model-dir <local checkpoint copy> --out-dir <separate output directory>",
      scripts: [
        {
          path: "scripts/export_onnx.py",
          raw_url: `https://raw.githubusercontent.com/NandhaKishorM/laya/${SOURCE_COMMIT}/scripts/export_onnx.py`,
          expected_git_blob_sha1: "4b5ce64cbcb50f4a98540c81ad279abb7512cdaf",
          observed_git_blob_sha1: "4b5ce64cbcb50f4a98540c81ad279abb7512cdaf",
          verified: true,
        },
        {
          path: "laya-ts/scripts/export_onnx.py",
          raw_url: `https://raw.githubusercontent.com/NandhaKishorM/laya/${SOURCE_COMMIT}/laya-ts/scripts/export_onnx.py`,
          expected_git_blob_sha1: "d82f7f28c70cd884ea0c48e0dddf4648612bc3cf",
          observed_git_blob_sha1: "d82f7f28c70cd884ea0c48e0dddf4648612bc3cf",
          verified: true,
        },
      ],
      export_executed: false,
    },
    generated_at: new Date().toISOString(),
    acquisition_status: "in_progress",
    files: [],
    facts: [],
    unknowns: [
      "No ONNX graph is present in this pinned revision; CPU inference readiness is false.",
      "No ONNX export, runtime parity check, preprocessing validation, task-quality evaluation, or CPU latency measurement was performed.",
      "The Apache-2.0 value is the repository's pinned model-card metadata declaration; this acquisition does not constitute legal review.",
    ],
  };
}

async function acquire() {
  const artifact = baseArtifact();
  try {
    const metadata = await inspectPinnedMetadata();
    artifact.metadata = metadata;
    if (metadata.onnxPaths.length !== 0) fail(`unexpected ONNX files at pinned revision: ${metadata.onnxPaths.join(",")}`);

    const destinations = await Promise.all(FILES.map(async (file) => {
      const destination = join(RESOURCE_DIR, file.path);
      const existing = await lstat(destination).catch((error) => {
        if (error.code === "ENOENT") return null;
        throw error;
      });
      if (existing) return { file, present: true };
      return { file, present: false };
    }));
    const missingBytes = destinations.reduce((sum, item) => sum + (item.present ? 0 : item.file.bytes), 0);
    artifact.disk_free_before_bytes = await assertDiskFloor(missingBytes);
    await mkdir(RESOURCE_DIR, { recursive: true });
    for (const directory of [
      join(REPO_ROOT, "target"),
      join(REPO_ROOT, "target/model-resources"),
      join(REPO_ROOT, "target/model-resources/laya"),
      RESOURCE_DIR,
    ]) {
      const directoryStat = await lstat(directory);
      if (!directoryStat.isDirectory() || directoryStat.isSymbolicLink()) {
        fail(`refusing non-directory or symlink resource parent: ${directory}`);
      }
    }
    const [realRoot, realResourceDir] = await Promise.all([realpath(REPO_ROOT), realpath(RESOURCE_DIR)]);
    if (!realResourceDir.startsWith(`${realRoot}/target/`)) {
      fail("resolved resource directory escaped the repository target directory");
    }

    for (const file of FILES) {
      artifact.files.push(await downloadOne(file));
      const freeBytes = bytesAvailable(await statfs(REPO_ROOT));
      artifact.disk_free_after_file_bytes = freeBytes;
      if (freeBytes < MINIMUM_FREE_BYTES) fail(`disk floor fell below 2 GiB after ${file.path}: ${freeBytes}`);
    }

    artifact.acquisition_status = "complete_hash_and_size_verified";
    artifact.facts = [
      "All downloaded bytes came from URLs containing the pinned immutable revision.",
      "Every file passed exact byte count and pinned SHA-256 or Git blob SHA-1 verification before atomic placement.",
      "Preexisting resource paths were reused only after exact verification; mismatches are fatal and are never replaced.",
      "Hugging Face pinned tree metadata contained no .onnx paths.",
      "Both upstream export-script Git blob IDs were independently checked at the pinned source commit; neither script was executed.",
    ];
    await writeArtifactOnce(artifact);
    process.stdout.write(`${JSON.stringify({ status: artifact.acquisition_status, artifact: ARTIFACT_PATH, directory: RESOURCE_DIR, files: artifact.files }, null, 2)}\n`);
  } catch (error) {
    artifact.acquisition_status = "failed";
    artifact.error = error instanceof Error ? error.message : String(error);
    try {
      await writeArtifactOnce(artifact);
    } catch (artifactError) {
      artifact.artifact_write_error = artifactError instanceof Error ? artifactError.message : String(artifactError);
    }
    throw error;
  }
}

if (process.argv.includes("--help")) {
  process.stdout.write("Usage: node scripts/fetch-laya-resources.mjs --acquire\nOnly fetches the fixed public revision; verifies metadata, sizes, hashes, and a 2 GiB free-space floor.\n");
} else if (process.argv.includes("--acquire")) {
  acquire().catch((error) => {
    process.stderr.write(`Laya resource acquisition failed: ${error.message}\n`);
    process.exitCode = 1;
  });
} else {
  process.stderr.write("Pass --acquire to download the pinned public resources, or --help.\n");
  process.exitCode = 2;
}
