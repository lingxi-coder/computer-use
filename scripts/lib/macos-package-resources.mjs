import { createHash } from 'node:crypto';
import { constants as fsConstants, createReadStream, createWriteStream } from 'node:fs';
import { chmod, lstat, mkdir, mkdtemp, open, readFile, realpath, readdir, rename, rm, writeFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';

export const MACOS_RESOURCE_MANIFEST = 'resource-manifest.json';
export const LAYA_MODEL_REVISION = 'e4e9ddf21a7b1903b7acffd8814ad4307bf63a67';
export const LAYA_SOURCE_COMMIT = '2e4d9c87e8b1621deb344eac7de5c7258f32f849';
export const LAYA_PACKAGE_MANIFEST_SHA256 = 'c4e5cb4dc827f1ca76a47f3c19d88ef7cd47a53268e474b0c98df6c998399424';
export const MACOS_MINIMUM_VERSION = '14.2';

const MODEL_FILES = Object.freeze({
  'encoder.onnx': { bytes: 2_771_288, sha256: '85080d470855319def232f3927d1d8d8794c23a2fae943b3f4564a1e8d3204d0' },
  'encoder.onnx.data': { bytes: 1_227_816_960, sha256: 'f1e49a027352064d72831863456be6160748854296bff8332df14130dbf79834' },
  'head.onnx': { bytes: 244_928, sha256: '66d3710bedbfa1043411e8de7c60927f2a279105d18e4f1b9db175e06452a02a' },
  'head.onnx.data': { bytes: 59_899_904, sha256: '0292a4eb478c2c3ec0b6555b4a4093bc802e965654d7aeb4ac22907a17f261d3' },
  'rl_agent_config.json': { bytes: 472, sha256: '25061739243b617ad88d1219ba6f8a9c86c5881ca28df024fa2d9b3b2fcc30c6' },
  'tokenizer.json': { bytes: 34_363_188, sha256: '609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f' },
});

const BINARIES = Object.freeze([
  { name: 'cu-auth', sourceName: 'cu-auth', kind: 'rust' },
  { name: 'cu-host', sourceName: 'cu-host', kind: 'rust' },
  { name: 'cu-device', sourceName: 'cu-device', kind: 'rust' },
  { name: 'cu-source', sourceName: 'cu-source', kind: 'rust' },
  { name: 'cu-macos-source', sourceName: 'cu-macos-source', kind: 'swift' },
  { name: 'cu-macos-device-guard', sourceName: 'cu-macos-device-guard', kind: 'swift' },
  { name: 'cu-macos-keychain', sourceName: 'cu-macos-keychain', kind: 'swift' },
  { name: 'cu-macos-owned-document-launcher', sourceName: 'cu-macos-owned-document-launcher', kind: 'swift' },
]);

const FIXED_FILE_PATHS = Object.freeze([
  ...BINARIES.map(({ name }) => `bin/${name}`),
  'fixtures/headless/readonly-page.html',
  `models/laya-multilingual/${LAYA_MODEL_REVISION}/package-manifest.json`,
  ...Object.keys(MODEL_FILES).map((name) => `models/laya-multilingual/${LAYA_MODEL_REVISION}/bundle/${name}`),
  'LICENSES/LingXi-MIT.txt',
  'LICENSES/Laya-Apache-2.0.txt',
  'LICENSES/THIRD-PARTY-NOTICES.txt',
]);
const RESOURCE_TREE_ROOTS = new Set(['bin', 'fixtures', 'models', 'LICENSES', MACOS_RESOURCE_MANIFEST]);
export const MACOS_RUNTIME_RESOURCE_PATHS = Object.freeze([...FIXED_FILE_PATHS, MACOS_RESOURCE_MANIFEST]);

const MACH_O_CPU_TYPES = Object.freeze({
  arm64: 0x0100000c,
  x64: 0x01000007,
});
const CPU_TYPE_NAMES = new Map(Object.entries(MACH_O_CPU_TYPES).map(([name, value]) => [value, name]));

function requireValue(condition, message) {
  if (!condition) throw new Error(message);
}

function posixPath(path) {
  return path.split(sep).join('/');
}

function isInside(parent, candidate) {
  const path = relative(parent, candidate);
  return path === '' || (!isAbsolute(path) && path !== '..' && !path.startsWith(`..${sep}`));
}

function normalizeArchitecture(value) {
  requireValue(Object.hasOwn(MACH_O_CPU_TYPES, value), `unsupported macOS package architecture: ${value}`);
  return value;
}

function validateManifestIdentity(manifest) {
  requireValue(manifest && typeof manifest === 'object' && !Array.isArray(manifest), 'resource manifest must be an object');
  requireValue(manifest.kind === 'macos_runtime_resource_manifest' && manifest.schema_version === 1,
    'unsupported macOS runtime resource manifest');
  requireValue(/^[0-9a-f]{40}$/.test(manifest.source_revision ?? ''), 'manifest source_revision must be a full Git commit');
  requireValue(['debug', 'release'].includes(manifest.build_kind), 'manifest build_kind must be explicit');
  requireValue(manifest.development_build === (manifest.build_kind === 'debug'),
    'manifest development_build does not match build_kind');
  requireValue(manifest.distribution_claim === (manifest.build_kind === 'debug'
    ? 'local-development-package' : 'release-build-unverified'), 'manifest distribution claim mismatch');
  requireValue(manifest.platform === 'darwin', 'manifest platform must be darwin');
  requireValue(manifest.source_revision_is_head_only === true && manifest.source_tree_state === 'not_attested',
    'manifest must state that the Git revision does not attest source-tree cleanliness');
  normalizeArchitecture(manifest.architecture);
  requireValue(manifest.minimum_macos_version === MACOS_MINIMUM_VERSION, 'manifest minimum macOS version mismatch');
  requireValue(manifest.laya?.model_revision === LAYA_MODEL_REVISION
    && manifest.laya?.source_commit === LAYA_SOURCE_COMMIT, 'manifest Laya pin mismatch');
  requireValue(Array.isArray(manifest.files), 'manifest files must be an array');
  const paths = manifest.files.map((record) => record?.path);
  requireValue(paths.every((path) => typeof path === 'string') && paths.length === FIXED_FILE_PATHS.length
    && new Set(paths).size === paths.length && [...paths].sort().join('\n') === [...FIXED_FILE_PATHS].sort().join('\n'),
  'manifest file set differs from the fixed macOS runtime resource inventory');
  for (const record of manifest.files) {
    requireValue(Number.isSafeInteger(record.bytes) && record.bytes >= 0, `invalid byte length: ${record.path}`);
    requireValue(/^[0-9a-f]{64}$/.test(record.sha256 ?? ''), `invalid SHA-256: ${record.path}`);
    requireValue(typeof record.executable === 'boolean', `invalid executable bit record: ${record.path}`);
    requireValue(typeof record.source_path === 'string' || record.source_path === null,
      `invalid source path record: ${record.path}`);
    if (record.source_path !== null) {
      requireValue(!isAbsolute(record.source_path) && !record.source_path.split('/').includes('..'),
        `source path escapes the repository: ${record.path}`);
    }
  }
  const records = new Map(manifest.files.map((record) => [record.path, record]));
  for (const binary of BINARIES) {
    const record = records.get(`bin/${binary.name}`);
    requireValue(record?.executable === true && record.kind === binary.kind
      && record.build_kind === manifest.build_kind && Array.isArray(record.architectures)
      && record.architectures.length === 1 && record.architectures[0] === manifest.architecture,
    `manifest native executable record is invalid: ${binary.name}`);
  }
  for (const path of FIXED_FILE_PATHS.filter((item) => !item.startsWith('bin/'))) {
    requireValue(records.get(path)?.executable === false, `manifest non-executable resource record is invalid: ${path}`);
  }
  for (const [name] of Object.entries(MODEL_FILES)) {
    const record = records.get(`models/laya-multilingual/${LAYA_MODEL_REVISION}/bundle/${name}`);
    requireValue(record?.pinned === true, `manifest Laya asset record is not pinned: ${name}`);
  }
  requireValue(records.get(`models/laya-multilingual/${LAYA_MODEL_REVISION}/package-manifest.json`)?.pinned === true,
    'manifest Laya package manifest record is not pinned');
  const laya = manifest.laya;
  requireValue(laya?.model_id === 'convaiinnovations/laya-multilingual'
    && laya.package_manifest_sha256 === LAYA_PACKAGE_MANIFEST_SHA256
    && laya.cpu_parity_verified === true && laya.product_runtime_integrated_at_export === false
    && laya.task_quality_verified === false && laya.model_license_declared === 'Apache-2.0'
    && typeof laya.model_license_source === 'string', 'manifest Laya status or license metadata is invalid');
  requireValue(manifest.optional_runtime_modes?.native_source?.helper === 'bin/cu-macos-source'
    && manifest.optional_runtime_modes.native_source.enabled_by_default === false
    && manifest.optional_runtime_modes.native_source.selector === 'CU_SOURCE_BACKEND=native'
    && manifest.optional_runtime_modes.device_guard?.helper === 'bin/cu-macos-device-guard'
    && manifest.optional_runtime_modes.device_guard.enabled_by_default === false
    && manifest.optional_runtime_modes.device_guard.loaded_for === 'authorized-native-owned-document-value-input',
  'manifest optional native helper mode policy is invalid');
}

export function validateMacosResourcePathInventory(actualPaths, allowedAdditionalTopLevelPaths = []) {
  requireValue(Array.isArray(actualPaths) && actualPaths.every((path) => typeof path === 'string'),
    'actual resource paths must be a string array');
  requireValue(new Set(actualPaths).size === actualPaths.length, 'actual resource paths contain duplicates');
  requireValue(Array.isArray(allowedAdditionalTopLevelPaths), 'allowedAdditionalTopLevelPaths must be an array');
  const allowedTopLevel = new Set();
  for (const path of allowedAdditionalTopLevelPaths) {
    requireValue(typeof path === 'string' && path.length > 0 && path !== '.' && path !== '..'
      && !path.includes('/') && !path.includes('\\') && !RESOURCE_TREE_ROOTS.has(path),
    `invalid or colliding additional top-level path: ${path}`);
    allowedTopLevel.add(path);
  }
  const expectedPaths = new Set(MACOS_RUNTIME_RESOURCE_PATHS);
  const actual = new Set(actualPaths);
  const missing = [...expectedPaths].filter((path) => !actual.has(path));
  const unlisted = actualPaths.filter((path) => !expectedPaths.has(path)
    && !allowedTopLevel.has(path.split('/')[0]));
  requireValue(missing.length === 0 && unlisted.length === 0,
    `staged resource tree contains missing or unlisted files: ${[...missing.map((path) => `missing ${path}`), ...unlisted].join(', ')}`);
}

async function assertNoSymlinkPath(root, relativePath, expectedKind = 'file') {
  const rootPath = resolve(root);
  const absolute = resolve(rootPath, relativePath);
  requireValue(isInside(rootPath, absolute), `path escapes its root: ${relativePath}`);
  const rootInfo = await lstat(rootPath);
  requireValue(!rootInfo.isSymbolicLink() && rootInfo.isDirectory(), `root is not a canonical directory: ${rootPath}`);
  let cursor = rootPath;
  const components = relative(rootPath, absolute).split(sep).filter(Boolean);
  for (let index = 0; index < components.length; index += 1) {
    cursor = join(cursor, components[index]);
    const info = await lstat(cursor);
    requireValue(!info.isSymbolicLink(), `symlink is not allowed in a package resource path: ${posixPath(relativePath)}`);
    const last = index === components.length - 1;
    if (!last) requireValue(info.isDirectory(), `resource parent is not a directory: ${cursor}`);
    else if (expectedKind === 'file') requireValue(info.isFile(), `resource is not a regular file: ${cursor}`);
    else requireValue(info.isDirectory(), `resource is not a directory: ${cursor}`);
  }
  return absolute;
}

async function sha256File(path) {
  const info = await lstat(path);
  requireValue(info.isFile() && !info.isSymbolicLink(), `refusing non-regular file: ${path}`);
  const hash = createHash('sha256');
  let bytes = 0;
  const stream = createReadStream(path, { flags: fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0) });
  for await (const chunk of stream) {
    bytes += chunk.length;
    hash.update(chunk);
  }
  return { bytes, sha256: hash.digest('hex') };
}

async function copyAndHash(source, destination, executable, expected = undefined) {
  const sourceInfo = await lstat(source);
  requireValue(sourceInfo.isFile() && !sourceInfo.isSymbolicLink(), `refusing non-regular resource input: ${source}`);
  const digest = createHash('sha256');
  let bytes = 0;
  const counter = new Transform({
    transform(chunk, _encoding, callback) {
      bytes += chunk.length;
      digest.update(chunk);
      callback(null, chunk);
    },
  });
  const mode = executable ? 0o755 : 0o644;
  try {
    await pipeline(
      createReadStream(source, { flags: fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0) }),
      counter,
      createWriteStream(destination, { flags: 'wx', mode }),
    );
    await chmod(destination, mode);
  } catch (error) {
    await rm(destination, { force: true }).catch(() => {});
    throw error;
  }
  const sha256 = digest.digest('hex');
  if (expected) {
    requireValue(bytes === expected.bytes, `resource size mismatch: ${source}`);
    requireValue(sha256 === expected.sha256, `resource SHA-256 mismatch: ${source}`);
  }
  return { bytes, sha256, executable };
}

function cpuTypeName(cpuType) {
  return CPU_TYPE_NAMES.get(cpuType >>> 0) ?? `cpu-type-0x${(cpuType >>> 0).toString(16)}`;
}

/** Reads the CPU architecture table from a thin or universal Mach-O header. */
export function inspectMachOArchitectures(header) {
  requireValue(Buffer.isBuffer(header) && header.length >= 8, 'Mach-O header is truncated');
  const magicLE = header.readUInt32LE(0);
  const magicBE = header.readUInt32BE(0);
  if (magicLE === 0xfeedface || magicLE === 0xfeedfacf) {
    return [cpuTypeName(header.readUInt32LE(4))];
  }
  if (magicBE === 0xfeedface || magicBE === 0xfeedfacf) {
    return [cpuTypeName(header.readUInt32BE(4))];
  }

  const fatMagic = new Map([
    [0xcafebabe, { endian: 'BE', entryBytes: 20 }],
    [0xbebafeca, { endian: 'LE', entryBytes: 20 }],
    [0xcafebabf, { endian: 'BE', entryBytes: 32 }],
    [0xbfbafeca, { endian: 'LE', entryBytes: 32 }],
  ]).get(magicBE);
  if (!fatMagic) throw new Error('binary is not a supported Mach-O executable');
  const count = fatMagic.endian === 'BE' ? header.readUInt32BE(4) : header.readUInt32LE(4);
  requireValue(count > 0 && count <= 32, 'Mach-O universal header has an invalid architecture count');
  requireValue(header.length >= 8 + count * fatMagic.entryBytes, 'Mach-O universal header is truncated');
  const architectures = [];
  for (let index = 0; index < count; index += 1) {
    const offset = 8 + index * fatMagic.entryBytes;
    const cpuType = fatMagic.endian === 'BE' ? header.readUInt32BE(offset) : header.readUInt32LE(offset);
    architectures.push(cpuTypeName(cpuType));
  }
  requireValue(new Set(architectures).size === architectures.length, 'Mach-O universal header contains duplicate architectures');
  return architectures.sort();
}

export function assertTargetArchitecture(name, architectures, targetArchitecture) {
  const target = normalizeArchitecture(targetArchitecture);
  requireValue(Array.isArray(architectures) && architectures.length === 1 && architectures[0] === target,
    `${name} architecture mismatch: expected only ${target}, found ${architectures?.join(', ') ?? '(unknown)'}`);
  return architectures[0];
}

async function inspectMachOFile(path) {
  const handle = await open(path, fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0));
  try {
    const info = await handle.stat();
    requireValue(info.isFile(), `native executable is not a regular file: ${path}`);
    const length = Math.min(info.size, 4096);
    const buffer = Buffer.alloc(length);
    const { bytesRead } = await handle.read(buffer, 0, length, 0);
    return inspectMachOArchitectures(buffer.subarray(0, bytesRead));
  } finally {
    await handle.close();
  }
}

function makeThirdPartyNotice() {
  return [
    'Computer Use third-party notices',
    '',
    'LingXi Computer Use is distributed under the MIT license in LingXi-MIT.txt.',
    '',
    'Laya multilingual tokenizer and preprocessing code',
    `Source: https://github.com/NandhaKishorM/laya/tree/${LAYA_SOURCE_COMMIT}`,
    'License: Apache-2.0; see Laya-Apache-2.0.txt.',
    '',
    'Laya multilingual ONNX model',
    `Model revision: ${LAYA_MODEL_REVISION}`,
    'License declared by the checked-in model metadata: Apache-2.0.',
    `Metadata: https://huggingface.co/api/models/convaiinnovations/laya-multilingual/revision/${LAYA_MODEL_REVISION}`,
    'This notice records repository metadata and does not claim independent legal review of model redistribution terms.',
    '',
  ].join('\n');
}

async function writeGeneratedFile(path, contents) {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, contents, { flag: 'wx', mode: 0o644 });
  await chmod(path, 0o644);
  const { bytes, sha256 } = await sha256File(path);
  return { bytes, sha256, executable: false };
}

async function validatePinnedModelSource(rootPath) {
  const revisionRoot = `target/model-exports/laya-multilingual/${LAYA_MODEL_REVISION}`;
  const manifestPath = await assertNoSymlinkPath(rootPath, `${revisionRoot}/package-manifest.json`);
  const manifestHashes = await sha256File(manifestPath);
  requireValue(manifestHashes.bytes > 0 && manifestHashes.sha256 === LAYA_PACKAGE_MANIFEST_SHA256,
    'pinned Laya package-manifest.json SHA-256 mismatch');
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  requireValue(manifest.kind === 'laya_onnx_package_manifest'
    && manifest.model_id === 'convaiinnovations/laya-multilingual'
    && manifest.model_revision === LAYA_MODEL_REVISION
    && manifest.source_commit === LAYA_SOURCE_COMMIT,
  'pinned Laya package identity mismatch');
  requireValue(manifest.cpu_parity_verified === true, 'pinned Laya package has no CPU parity record');
  requireValue(manifest.product_runtime_integrated === false && manifest.task_quality_verified === false
    && manifest.mvp_complete === false, 'Laya package status does not match the recorded incomplete product state');
  const declared = manifest.package_files;
  requireValue(declared && Object.keys(declared).sort().join('\n') === Object.keys(MODEL_FILES).sort().join('\n'),
    'pinned Laya package file inventory mismatch');
  for (const [name, expected] of Object.entries(MODEL_FILES)) {
    requireValue(declared[name]?.bytes === expected.bytes && declared[name]?.sha256 === expected.sha256,
      `pinned Laya package manifest hash mismatch: ${name}`);
  }
  requireValue(manifest.package_bytes === Object.values(MODEL_FILES).reduce((sum, item) => sum + item.bytes, 0),
    'pinned Laya package byte total mismatch');

  const licenseMetadataPath = await assertNoSymlinkPath(rootPath, 'assets/model-manifests/laya-multilingual-base.json');
  const licenseMetadata = JSON.parse(await readFile(licenseMetadataPath, 'utf8'));
  requireValue(licenseMetadata.model_id === manifest.model_id && licenseMetadata.revision === LAYA_MODEL_REVISION
    && licenseMetadata.declared_license?.spdx_id === 'Apache-2.0'
    && typeof licenseMetadata.declared_license?.metadata_url === 'string',
  'Laya model license metadata does not match the pinned revision');
  for (const [name, expected] of Object.entries(MODEL_FILES)) {
    const assetPath = await assertNoSymlinkPath(rootPath, `${revisionRoot}/bundle/${name}`);
    const info = await lstat(assetPath);
    requireValue(info.size === expected.bytes, `pinned Laya model size mismatch: ${name}`);
  }
  return { revisionRoot, manifest, manifestPath, manifestHashes };
}

/** Fast metadata preflight; the stage operation hashes each model asset while copying it. */
export async function verifyPinnedLayaModelSource(root) {
  requireValue(typeof root === 'string' && isAbsolute(root), 'root must be an absolute repository path');
  const rootPath = await realpath(resolve(root));
  const result = await validatePinnedModelSource(rootPath);
  return {
    model_revision: result.manifest.model_revision,
    source_commit: result.manifest.source_commit,
    package_manifest_sha256: result.manifestHashes.sha256,
    package_files: result.manifest.package_files,
  };
}

/**
 * Copies the exact runtime resource closure consumed by the current packaged Main process.
 * No Cargo build is performed: nativeBinDir must already contain the five selected executables.
 */
export async function stageMacosRuntimeResources({
  root,
  destination,
  architecture,
  buildKind,
  sourceRevision,
  nativeBinDir,
}) {
  requireValue(typeof root === 'string' && isAbsolute(root), 'root must be an absolute repository path');
  requireValue(typeof destination === 'string' && isAbsolute(destination), 'destination must be an absolute path');
  requireValue(typeof nativeBinDir === 'string' && isAbsolute(nativeBinDir), 'nativeBinDir must be an absolute path');
  requireValue(['debug', 'release'].includes(buildKind), 'buildKind must explicitly be debug or release');
  const targetArchitecture = normalizeArchitecture(architecture);
  requireValue(/^[0-9a-f]{40}$/.test(sourceRevision ?? ''), 'sourceRevision must be a full Git commit');

  const rootPath = await realpath(resolve(root));
  const actualRevision = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: rootPath, encoding: 'utf8' }).trim();
  requireValue(actualRevision === sourceRevision, 'sourceRevision does not match the repository HEAD');
  const nativeDirectory = resolve(nativeBinDir);
  requireValue(isInside(rootPath, nativeDirectory) && nativeDirectory !== rootPath,
    'nativeBinDir must be inside the repository root');
  const nativeRel = posixPath(relative(rootPath, nativeDirectory));
  await assertNoSymlinkPath(rootPath, nativeRel, 'directory');
  const model = await validatePinnedModelSource(rootPath);

  const requestedDestination = resolve(destination);
  const requestedParent = dirname(requestedDestination);
  await mkdir(requestedParent, { recursive: true });
  const parentPath = await realpath(requestedParent);
  const finalPath = join(parentPath, requestedDestination.split(sep).at(-1));
  requireValue(isInside(parentPath, finalPath) && finalPath !== parentPath, 'destination must be a child of its parent');
  const finalInfo = await lstat(finalPath).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
  if (finalInfo) requireValue(finalInfo.isDirectory() && !finalInfo.isSymbolicLink(), 'destination must be a real directory');

  const tempPath = await mkdtemp(join(parentPath, `.${finalPath.split(sep).at(-1)}.resources-`));
  const records = [];
  const addCopiedFile = async (source, relativeDestination, executable, expected = undefined) => {
    const destinationPath = join(tempPath, relativeDestination);
    await mkdir(dirname(destinationPath), { recursive: true });
    const outputInfo = await copyAndHash(source, destinationPath, executable, expected);
    records.push({
      path: posixPath(relativeDestination),
      source_path: posixPath(relative(rootPath, source)),
      bytes: outputInfo.bytes,
      sha256: outputInfo.sha256,
      executable: outputInfo.executable,
      ...(expected ? { pinned: true } : {}),
    });
  };

  try {
    for (const binary of BINARIES) {
      const source = await assertNoSymlinkPath(nativeDirectory, binary.sourceName);
      const sourceInfo = await lstat(source);
      requireValue((sourceInfo.mode & 0o111) !== 0, `native executable lacks executable permission: ${binary.sourceName}`);
      const arches = await inspectMachOFile(source);
      assertTargetArchitecture(binary.sourceName, arches, targetArchitecture);
      const relativeDestination = `bin/${binary.name}`;
      await addCopiedFile(source, relativeDestination, true);
      records.at(-1).kind = binary.kind;
      records.at(-1).architectures = arches;
      records.at(-1).build_kind = buildKind;
    }

    const fixture = await assertNoSymlinkPath(rootPath, 'fixtures/headless/readonly-page.html');
    await addCopiedFile(fixture, 'fixtures/headless/readonly-page.html', false);

    const modelDest = `models/laya-multilingual/${LAYA_MODEL_REVISION}`;
    await addCopiedFile(model.manifestPath, `${modelDest}/package-manifest.json`, false,
      { bytes: model.manifestHashes.bytes, sha256: LAYA_PACKAGE_MANIFEST_SHA256 });
    for (const [name, expected] of Object.entries(MODEL_FILES)) {
      const source = await assertNoSymlinkPath(rootPath, `${model.revisionRoot}/bundle/${name}`);
      await addCopiedFile(source, `${modelDest}/bundle/${name}`, false, expected);
    }

    const projectLicense = await assertNoSymlinkPath(rootPath, 'LICENSE');
    await addCopiedFile(projectLicense, 'LICENSES/LingXi-MIT.txt', false);
    const layaLicense = await assertNoSymlinkPath(rootPath, 'packages/decision-worker/src/laya/vendor/laya-ts/LICENSE');
    await addCopiedFile(layaLicense, 'LICENSES/Laya-Apache-2.0.txt', false);
    const notice = makeThirdPartyNotice();
    const noticeResult = await writeGeneratedFile(join(tempPath, 'LICENSES/THIRD-PARTY-NOTICES.txt'), notice);
    records.push({
      path: 'LICENSES/THIRD-PARTY-NOTICES.txt', source_path: null,
      bytes: noticeResult.bytes, sha256: noticeResult.sha256, executable: false, generated: true,
    });

    records.sort((left, right) => left.path.localeCompare(right.path));
    const manifest = {
      kind: 'macos_runtime_resource_manifest',
      schema_version: 1,
      source_revision: sourceRevision,
      source_revision_is_head_only: true,
      source_tree_state: 'not_attested',
      architecture: targetArchitecture,
      build_kind: buildKind,
      development_build: buildKind === 'debug',
      distribution_claim: buildKind === 'debug' ? 'local-development-package' : 'release-build-unverified',
      platform: 'darwin',
      minimum_macos_version: MACOS_MINIMUM_VERSION,
      optional_runtime_modes: {
        native_source: {
          helper: 'bin/cu-macos-source',
          enabled_by_default: false,
          selector: 'CU_SOURCE_BACKEND=native',
        },
        device_guard: {
          helper: 'bin/cu-macos-device-guard',
          enabled_by_default: false,
          loaded_for: 'authorized-native-owned-document-value-input',
          environment_override: 'CU_NATIVE_DEVICE_GUARD',
        },
      },
      laya: {
        model_id: 'convaiinnovations/laya-multilingual',
        model_revision: LAYA_MODEL_REVISION,
        source_commit: LAYA_SOURCE_COMMIT,
        package_manifest_sha256: LAYA_PACKAGE_MANIFEST_SHA256,
        cpu_parity_verified: true,
        product_runtime_integrated_at_export: false,
        task_quality_verified: false,
        model_license_declared: 'Apache-2.0',
        model_license_source: 'assets/model-manifests/laya-multilingual-base.json',
      },
      files: records,
    };
    validateManifestIdentity(manifest);
    await writeFile(join(tempPath, MACOS_RESOURCE_MANIFEST), `${JSON.stringify(manifest, null, 2)}\n`,
      { flag: 'wx', mode: 0o644 });
    await chmod(join(tempPath, MACOS_RESOURCE_MANIFEST), 0o644);
    await verifyMacosRuntimeResourceManifest({ root: rootPath, destination: tempPath });

    if (finalInfo) {
      const entries = await readdir(finalPath);
      if (entries.length > 0) {
        const existing = await verifyMacosRuntimeResourceManifest({ root: rootPath, destination: finalPath });
        const existingText = await readFile(join(finalPath, MACOS_RESOURCE_MANIFEST), 'utf8');
        const stagedText = await readFile(join(tempPath, MACOS_RESOURCE_MANIFEST), 'utf8');
        requireValue(existingText === stagedText, 'destination already contains a different verified resource stage');
        await rm(tempPath, { recursive: true, force: true });
        return { destination: finalPath, manifestPath: join(finalPath, MACOS_RESOURCE_MANIFEST), manifest: existing };
      }
      await rm(finalPath, { recursive: true, force: true });
    }
    await rename(tempPath, finalPath);
    return { destination: finalPath, manifestPath: join(finalPath, MACOS_RESOURCE_MANIFEST), manifest };
  } catch (error) {
    await rm(tempPath, { recursive: true, force: true }).catch(() => {});
    throw error;
  }
}

/** Rejects changed, missing, symlinked, wrongly typed, or unlisted staged resources. */
export async function verifyMacosRuntimeResourceManifest({ root, destination, allowedAdditionalTopLevelPaths = [] }) {
  requireValue(typeof destination === 'string' && isAbsolute(destination), 'destination must be an absolute path');
  requireValue(Array.isArray(allowedAdditionalTopLevelPaths), 'allowedAdditionalTopLevelPaths must be an array');
  const stageRoot = resolve(destination);
  const stageInfo = await lstat(stageRoot);
  requireValue(stageInfo.isDirectory() && !stageInfo.isSymbolicLink(), 'staged resources root must be a real directory');
  const manifestPath = await assertNoSymlinkPath(stageRoot, MACOS_RESOURCE_MANIFEST);
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  validateManifestIdentity(manifest);
  if (root) {
    const rootPath = resolve(root);
    await realpath(rootPath);
  }

  const actualPaths = await collectFiles(stageRoot, allowedAdditionalTopLevelPaths);
  validateMacosResourcePathInventory(actualPaths, allowedAdditionalTopLevelPaths);

  const byPath = new Map(manifest.files.map((record) => [record.path, record]));
  for (const record of manifest.files) {
    const absolute = await assertNoSymlinkPath(stageRoot, record.path);
    const info = await lstat(absolute);
    const executable = (info.mode & 0o111) !== 0;
    requireValue(executable === record.executable, `staged executable bit mismatch: ${record.path}`);
    const actual = await sha256File(absolute);
    requireValue(actual.bytes === record.bytes && actual.sha256 === record.sha256,
      `staged file hash or size mismatch: ${record.path}`);
  }
  const manifestRecord = byPath.get(`models/laya-multilingual/${LAYA_MODEL_REVISION}/package-manifest.json`);
  requireValue(manifestRecord?.sha256 === LAYA_PACKAGE_MANIFEST_SHA256,
    'staged Laya package manifest is not the pinned manifest');
  const modelManifestPath = join(stageRoot, `models/laya-multilingual/${LAYA_MODEL_REVISION}/package-manifest.json`);
  const modelManifest = JSON.parse(await readFile(modelManifestPath, 'utf8'));
  requireValue(modelManifest.model_revision === LAYA_MODEL_REVISION && modelManifest.source_commit === LAYA_SOURCE_COMMIT
    && modelManifest.cpu_parity_verified === true && modelManifest.product_runtime_integrated === false
    && modelManifest.task_quality_verified === false && modelManifest.mvp_complete === false,
  'staged Laya package identity or completion boundary mismatch');
  for (const [name, expected] of Object.entries(MODEL_FILES)) {
    const record = byPath.get(`models/laya-multilingual/${LAYA_MODEL_REVISION}/bundle/${name}`);
    requireValue(record?.bytes === expected.bytes && record?.sha256 === expected.sha256,
      `staged Laya asset pin mismatch: ${name}`);
  }
  for (const record of manifest.files.filter((item) => item.kind === 'rust' || item.kind === 'swift')) {
    const archs = await inspectMachOFile(join(stageRoot, record.path));
    assertTargetArchitecture(record.path, archs, manifest.architecture);
    assertTargetArchitecture(`${record.path} manifest`, record.architectures, manifest.architecture);
  }
  requireValue(manifest.laya.package_manifest_sha256 === LAYA_PACKAGE_MANIFEST_SHA256,
    'manifest Laya package-manifest pin mismatch');
  return manifest;
}

async function collectFiles(root, allowedAdditionalTopLevelPaths = []) {
  const files = [];
  const allowedAppRoots = new Set(allowedAdditionalTopLevelPaths);
  async function visit(directory, prefix = '') {
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries) {
      const relativePath = prefix ? `${prefix}/${entry.name}` : entry.name;
      const absolute = join(directory, entry.name);
      const info = await lstat(absolute);
      if (info.isSymbolicLink()) {
        requireValue(allowedAppRoots.has(relativePath.split('/')[0]),
          `symlink is not allowed in staged resources: ${relativePath}`);
        continue;
      }
      if (info.isDirectory()) await visit(absolute, relativePath);
      else {
        requireValue(info.isFile(), `non-regular staged resource: ${relativePath}`);
        files.push(relativePath);
      }
    }
  }
  await visit(root);
  return files.sort();
}
