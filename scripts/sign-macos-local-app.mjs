#!/usr/bin/env node

import { createHash, randomUUID, X509Certificate } from 'node:crypto';
import { constants as fsConstants, createReadStream } from 'node:fs';
import {
  chmod,
  lstat,
  mkdir,
  readFile,
  readdir,
  readlink,
  realpath,
  rename,
  rm,
  rmdir,
  writeFile,
} from 'node:fs/promises';
import { execFileSync, spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { verifyMacosRuntimeResourceManifest } from './lib/macos-package-resources.mjs';

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(SCRIPT_DIR, '..');
const ARTIFACTS_ROOT = path.join(REPO_ROOT, 'artifacts');
const PACKAGE_ARTIFACT_RELATIVE = 'artifacts/mvp-macos-package-003445c7-2dcc-4ca7-b475-837b3db357bd';
const PACKAGE_ARTIFACT_DIR = path.join(REPO_ROOT, PACKAGE_ARTIFACT_RELATIVE);
const SOURCE_ASSEMBLY_RELATIVE = `${PACKAGE_ARTIFACT_RELATIVE}/assembly-01`;
const SOURCE_ASSEMBLY_DIR = path.join(REPO_ROOT, SOURCE_ASSEMBLY_RELATIVE);
const SOURCE_MANIFEST_RELATIVE = `${SOURCE_ASSEMBLY_RELATIVE}/assembly-manifest.json`;
const SOURCE_MANIFEST_PATH = path.join(REPO_ROOT, SOURCE_MANIFEST_RELATIVE);
const SOURCE_MANIFEST_SHA256 = '0283f98b1f04ef205000e926bc3dabcb346a49887c925ecf8de09a9eff1660c9';
const CHECKPOINT_RELATIVE = `${PACKAGE_ARTIFACT_RELATIVE}/package-checkpoint.json`;
const CHECKPOINT_PATH = path.join(REPO_ROOT, CHECKPOINT_RELATIVE);
const CHECKPOINT_SHA256 = '0f7b7a613471a98045b6951dcb59168b2d7cf0aab920af1a017e95324b246535';
const OWNER_MARKER = '.computer-use-local-signing-owner.json';
const PRODUCT_NAME = 'Computer Use';
const BUNDLE_ID = 'com.lingxi.computer-use';
const TARGET_ARCH = 'arm64';
const RESOURCE_MANIFEST_PATH = 'Contents/Resources/resource-manifest.json';
const RESOURCE_MANIFEST_OWNED_ROOTS = new Set([
  'bin',
  'fixtures',
  'models',
  'LICENSES',
  'resource-manifest.json',
]);

function usage() {
  return [
    'Usage:',
    '  node scripts/sign-macos-local-app.mjs --output /absolute/new/artifact-dir --identity-sha1 <40-hex-fingerprint>',
    '',
    'Copies the pinned unsigned debug assembly into a new artifact directory, verifies the copied',
    'app, and signs that copy with an Apple Development identity for local use.',
    'The output must be a new directory inside a separate, canonical artifact directory under artifacts/.',
    'This does not create a Developer ID release or notarize the app.',
  ].join('\n');
}

export function parseArgs(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--help' || argument === '-h') return { help: true };
    if (argument !== '--output' && argument !== '--identity-sha1') {
      throw new Error(`Unknown argument: ${argument}`);
    }
    const value = argv[index + 1];
    if (!value || value.startsWith('--')) throw new Error(`Expected a value after ${argument}`);
    const key = argument === '--output' ? 'output' : 'identitySha1';
    if (values[key]) throw new Error(`${argument} may only be provided once`);
    values[key] = value;
    index += 1;
  }

  for (const [key, argument] of [['output', '--output'], ['identitySha1', '--identity-sha1']]) {
    if (!values[key]) throw new Error(`Missing required ${argument}`);
  }
  if (!path.isAbsolute(values.output)) throw new Error('--output must be an absolute path');
  values.output = path.resolve(values.output);
  if (!/^[a-f0-9]{40}$/i.test(values.identitySha1)) {
    throw new Error('--identity-sha1 must be a 40-character SHA-1 certificate fingerprint');
  }
  values.identitySha1 = values.identitySha1.toUpperCase();
  return values;
}

function run(command, args, options = {}) {
  try {
    const result = execFileSync(command, args, {
      cwd: options.cwd ?? REPO_ROOT,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      maxBuffer: 32 * 1024 * 1024,
    });
    return result;
  } catch (error) {
    const stderr = error.stderr?.toString().trim();
    const stdout = error.stdout?.toString().trim();
    const details = [stderr, stdout].filter(Boolean).join('\n');
    throw new Error(`${command} ${args.join(' ')} failed${details ? `:\n${details}` : ''}`, {
      cause: error,
    });
  }
}

function captureCombined(command, args) {
  const result = spawnSync(command, args, {
    cwd: REPO_ROOT,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const details = [result.stderr?.trim(), result.stdout?.trim()].filter(Boolean).join('\n');
    throw new Error(`${command} ${args.join(' ')} failed${details ? `:\n${details}` : ''}`);
  }
  return `${result.stdout ?? ''}${result.stderr ?? ''}`;
}

async function sha256File(filePath) {
  const info = await lstat(filePath);
  if (!info.isFile() || info.isSymbolicLink()) throw new Error(`Refusing non-regular file: ${filePath}`);
  const hash = createHash('sha256');
  let bytes = 0;
  const stream = createReadStream(filePath, {
    flags: fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0),
  });
  for await (const chunk of stream) {
    bytes += chunk.length;
    hash.update(chunk);
  }
  return { bytes, sha256: hash.digest('hex') };
}

function isPathInside(parentPath, childPath) {
  const relativePath = path.relative(parentPath, childPath);
  return relativePath !== '' && relativePath !== '..'
    && !relativePath.startsWith(`..${path.sep}`)
    && !path.isAbsolute(relativePath);
}

function toPosixPath(value) {
  return value.split(path.sep).join('/');
}

function canonicalFileEntry(entry) {
  if (entry.type === 'symlink') {
    return {
      path: entry.path,
      type: 'symlink',
      linkTarget: entry.linkTarget,
      byteLength: 0,
      executable: false,
    };
  }
  return {
    path: entry.path,
    type: 'file',
    byteLength: entry.byteLength,
    sha256: entry.sha256,
    executable: entry.executable,
  };
}

function compareFileEntries(left, right) {
  return left.path.localeCompare(right.path);
}

async function createAppFileManifest(appPath) {
  const entries = [];

  async function visit(currentPath, relativePath) {
    const children = await readdir(currentPath, { withFileTypes: true });
    children.sort((left, right) => left.name.localeCompare(right.name));
    for (const child of children) {
      const childPath = path.join(currentPath, child.name);
      const childRelative = relativePath ? path.join(relativePath, child.name) : child.name;
      const info = await lstat(childPath);
      if (info.isSymbolicLink()) {
        if (!childRelative.startsWith(`Contents${path.sep}Frameworks${path.sep}`)) {
          throw new Error(`Unexpected symbolic link in app bundle: ${toPosixPath(childRelative)}`);
        }
        const target = await readlink(childPath);
        const resolvedTarget = path.resolve(path.dirname(childPath), target);
        if (!isPathInside(appPath, resolvedTarget)) {
          throw new Error(`Framework symlink escapes the app bundle: ${toPosixPath(childRelative)} -> ${target}`);
        }
        entries.push({
          path: toPosixPath(childRelative),
          type: 'symlink',
          linkTarget: target,
          byteLength: 0,
          executable: false,
        });
        continue;
      }
      if (info.isDirectory()) {
        await visit(childPath, childRelative);
        continue;
      }
      if (!info.isFile()) throw new Error(`Unexpected file type in app bundle: ${toPosixPath(childRelative)}`);
      const digest = await sha256File(childPath);
      entries.push({
        path: toPosixPath(childRelative),
        type: 'file',
        byteLength: digest.bytes,
        sha256: digest.sha256,
        executable: (info.mode & 0o111) !== 0,
      });
    }
  }

  const rootInfo = await lstat(appPath);
  if (!rootInfo.isDirectory() || rootInfo.isSymbolicLink()) {
    throw new Error(`App bundle is not a regular directory: ${appPath}`);
  }
  await visit(appPath, '');
  return entries.sort(compareFileEntries);
}

function assertManifestFilesEqual(actual, expected, label) {
  if (!Array.isArray(expected)) throw new Error(`${label} manifest has no files array`);
  const normalizedExpected = expected.map(canonicalFileEntry).sort(compareFileEntries);
  if (actual.length !== normalizedExpected.length) {
    throw new Error(`${label} file inventory count mismatch: expected ${normalizedExpected.length}, found ${actual.length}`);
  }
  for (let index = 0; index < actual.length; index += 1) {
    if (JSON.stringify(actual[index]) !== JSON.stringify(normalizedExpected[index])) {
      throw new Error(`${label} file inventory mismatch at ${actual[index]?.path ?? normalizedExpected[index]?.path}`);
    }
  }
}

function assertPinnedPackageManifest(manifest, manifestDigest, checkpoint, checkpointDigest) {
  if (manifestDigest.sha256 !== SOURCE_MANIFEST_SHA256) {
    throw new Error(`Pinned source assembly manifest SHA-256 mismatch: ${manifestDigest.sha256}`);
  }
  if (checkpointDigest.sha256 !== CHECKPOINT_SHA256) {
    throw new Error(`Pinned package checkpoint SHA-256 mismatch: ${checkpointDigest.sha256}`);
  }
  const sourcePin = checkpoint.artifact_pins?.[SOURCE_MANIFEST_RELATIVE];
  if (!sourcePin || sourcePin.sha256 !== SOURCE_MANIFEST_SHA256 || sourcePin.bytes !== manifestDigest.bytes) {
    throw new Error('Package checkpoint does not pin the expected source assembly manifest.');
  }
  if (manifest.schemaVersion !== 1
    || manifest.product !== PRODUCT_NAME
    || manifest.bundleIdentifier !== BUNDLE_ID
    || manifest.signed !== false
    || manifest.buildKind !== 'debug-development'
    || manifest.target?.platform !== 'darwin'
    || manifest.target?.architecture !== TARGET_ARCH
    || typeof manifest.appPath !== 'string') {
    throw new Error('Pinned source assembly manifest does not describe the expected unsigned arm64 development app.');
  }
  if (path.isAbsolute(manifest.appPath) || manifest.appPath.split(/[\\/]/).includes('..')) {
    throw new Error(`Unsafe appPath in source assembly manifest: ${manifest.appPath}`);
  }
  if (!manifest.appPath.endsWith(`${PRODUCT_NAME}.app`)) {
    throw new Error(`Unexpected app bundle path in source assembly manifest: ${manifest.appPath}`);
  }
  return {
    sourcePin,
    checkpointSha256: checkpointDigest.sha256,
  };
}

export function parseAppleDevelopmentIdentityOutput(output, requestedSha1) {
  const requested = requestedSha1.toUpperCase();
  const identities = output.split(/\r?\n/)
    .map((line) => line.match(/^\s*\d+\)\s+([A-F0-9]{40})\s+"([^"]+)"/i))
    .filter(Boolean)
    .map((match) => ({ sha1: match[1].toUpperCase(), name: match[2] }));
  const matching = identities.filter((identity) => identity.sha1 === requested);
  if (matching.length !== 1) {
    throw new Error(`Expected exactly one valid code-signing identity with SHA-1 ${requested}; found ${matching.length}.`);
  }
  const identity = matching[0];
  if (!identity.name.startsWith('Apple Development: ')) {
    throw new Error(`Local signing requires an Apple Development certificate; selected identity is ${identity.name}.`);
  }
  const certificateIdentifier = identity.name.match(/\(([^()]+)\)$/)?.[1] ?? null;
  return {
    sha1: identity.sha1,
    name: identity.name,
    certificateKind: 'Apple Development',
    certificateIdentifier,
  };
}

export function parseAppleDevelopmentCertificateMetadata(identity, certificateFingerprintSha1, certificateSubject) {
  const normalizedFingerprint = certificateFingerprintSha1.replaceAll(':', '').toUpperCase();
  if (normalizedFingerprint !== identity.sha1) {
    throw new Error(`Certificate fingerprint does not match the selected identity ${identity.sha1}.`);
  }
  const subjectFields = certificateSubject.split(/[\r\n,]/).map((field) => field.trim());
  const teamIdentifier = subjectFields
    .find((field) => field.startsWith('OU='))
    ?.slice('OU='.length)
    .trim();
  if (!teamIdentifier || !/^[A-Z0-9]{10}$/i.test(teamIdentifier)) {
    throw new Error(`Could not read a valid team identifier from the certificate subject OU: ${certificateSubject}`);
  }
  return { ...identity, teamIdentifier };
}

export function parseAppleDevelopmentCertificateOutput(identity, pemOutput) {
  const certificates = pemOutput.match(/-----BEGIN CERTIFICATE-----[\s\S]+?-----END CERTIFICATE-----/g) ?? [];
  const matching = new Map();
  for (const pem of certificates) {
    const certificate = new X509Certificate(pem);
    const fingerprintSha1 = createHash('sha1').update(certificate.raw).digest('hex').toUpperCase();
    if (fingerprintSha1 === identity.sha1) matching.set(fingerprintSha1, certificate);
  }
  if (matching.size !== 1) {
    throw new Error(`Expected one public certificate matching identity SHA-1 ${identity.sha1}; found ${matching.size}.`);
  }
  const certificate = matching.get(identity.sha1);
  return parseAppleDevelopmentCertificateMetadata(identity, identity.sha1, certificate.subject);
}

export function updateSignedNativeResourceRecords(manifest, actualRecords) {
  if (manifest.kind !== 'macos_runtime_resource_manifest' || !Array.isArray(manifest.files)) {
    throw new Error('Unexpected runtime resource manifest format.');
  }
  const nativeRecords = manifest.files.filter((record) => record.kind === 'rust' || record.kind === 'swift');
  if (nativeRecords.length !== 7) {
    throw new Error(`Expected seven packaged Rust/Swift resource records; found ${nativeRecords.length}.`);
  }
  const actualByPath = new Map(actualRecords.map((record) => [record.path, record]));
  if (actualByPath.size !== actualRecords.length || actualByPath.size !== nativeRecords.length) {
    throw new Error('Signed native resource digest inventory does not match the seven manifest records.');
  }

  const updates = [];
  for (const record of nativeRecords) {
    if (!record.path.startsWith('bin/') || record.executable !== true
      || !Array.isArray(record.architectures) || record.architectures.length !== 1
      || record.architectures[0] !== TARGET_ARCH) {
      throw new Error(`Unexpected native resource record: ${record.path}`);
    }
    const actual = actualByPath.get(record.path);
    if (!actual || !Number.isSafeInteger(actual.bytes) || actual.bytes <= 0
      || !/^[a-f0-9]{64}$/.test(actual.sha256)) {
      throw new Error(`Missing valid post-sign resource digest for ${record.path}`);
    }
    updates.push({
      path: record.path,
      preSignBytes: record.bytes,
      preSignSha256: record.sha256,
      postSignBytes: actual.bytes,
      postSignSha256: actual.sha256,
    });
    record.bytes = actual.bytes;
    record.sha256 = actual.sha256;
  }
  return updates.sort(compareFileEntries);
}

export function deriveAllowedAdditionalResourceRoots(assemblyManifest) {
  if (!Array.isArray(assemblyManifest.files)) {
    throw new Error('Pinned assembly manifest has no file inventory.');
  }
  const roots = new Set();
  for (const entry of assemblyManifest.files) {
    if (typeof entry.path !== 'string' || !entry.path.startsWith('Contents/Resources/')) continue;
    const relativePath = entry.path.slice('Contents/Resources/'.length);
    const topLevel = relativePath.split('/')[0];
    if (topLevel && !RESOURCE_MANIFEST_OWNED_ROOTS.has(topLevel)) roots.add(topLevel);
  }
  if (!roots.has('app')) throw new Error('Pinned Electron resource inventory does not include Contents/Resources/app.');
  return [...roots].sort((left, right) => left.localeCompare(right));
}

export function assertPermittedOutputPath(outputPath, artifactsRoot, sourcePackageRoot, sourceAssemblyRoot) {
  const resolvedOutput = path.resolve(outputPath);
  const resolvedArtifactsRoot = path.resolve(artifactsRoot);
  const resolvedPackageRoot = path.resolve(sourcePackageRoot);
  const resolvedAssemblyRoot = path.resolve(sourceAssemblyRoot);
  const resolvedOutputParent = path.dirname(resolvedOutput);

  if (resolvedOutput === resolvedPackageRoot || isPathInside(resolvedPackageRoot, resolvedOutput)) {
    throw new Error('The immutable source package artifact and all of its descendants are forbidden output paths.');
  }
  if (resolvedOutput === resolvedAssemblyRoot || isPathInside(resolvedAssemblyRoot, resolvedOutput)) {
    throw new Error('The immutable source assembly directory and all of its descendants are forbidden output paths.');
  }
  if (path.dirname(resolvedOutputParent) !== resolvedArtifactsRoot) {
    throw new Error(`--output must be inside a canonical artifact directory directly below ${resolvedArtifactsRoot}`);
  }
}

async function verifyOutputPath(outputPath) {
  const artifactsRoot = await realpath(ARTIFACTS_ROOT);
  const packageRoot = await realpath(PACKAGE_ARTIFACT_DIR);
  const assemblyRoot = await realpath(SOURCE_ASSEMBLY_DIR);
  const outputParent = await realpath(path.dirname(outputPath));
  if (outputParent === packageRoot || isPathInside(packageRoot, outputParent)) {
    throw new Error('The immutable source package artifact cannot be used as an output parent.');
  }
  if (outputParent === assemblyRoot || isPathInside(assemblyRoot, outputParent)) {
    throw new Error('The immutable source assembly directory cannot be used as an output parent.');
  }
  if (path.dirname(outputParent) !== artifactsRoot) {
    throw new Error(`--output must be inside a canonical artifact directory directly below ${artifactsRoot}`);
  }
  const canonicalOutputPath = path.join(outputParent, path.basename(outputPath));
  assertPermittedOutputPath(canonicalOutputPath, artifactsRoot, packageRoot, assemblyRoot);
  try {
    await lstat(outputPath);
    throw new Error(`Refusing to overwrite an existing output path: ${outputPath}`);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  return packageRoot;
}

function assertMacHost() {
  if (process.platform !== 'darwin') throw new Error('Local macOS signing must run on macOS.');
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (major < 22 || (major === 22 && minor < 12)) {
    throw new Error(`@electron/osx-sign requires Node.js 22.12 or newer; found ${process.versions.node}.`);
  }
}

export async function verifySourceInputs() {
  const sourceManifestInfo = await lstat(SOURCE_MANIFEST_PATH);
  const checkpointInfo = await lstat(CHECKPOINT_PATH);
  if (!sourceManifestInfo.isFile() || sourceManifestInfo.isSymbolicLink()) {
    throw new Error(`Source assembly manifest is not a regular file: ${SOURCE_MANIFEST_PATH}`);
  }
  if (!checkpointInfo.isFile() || checkpointInfo.isSymbolicLink()) {
    throw new Error(`Package checkpoint is not a regular file: ${CHECKPOINT_PATH}`);
  }

  const [manifestDigest, checkpointDigest] = await Promise.all([
    sha256File(SOURCE_MANIFEST_PATH),
    sha256File(CHECKPOINT_PATH),
  ]);
  const manifest = JSON.parse(await readFile(SOURCE_MANIFEST_PATH, 'utf8'));
  const checkpoint = JSON.parse(await readFile(CHECKPOINT_PATH, 'utf8'));
  const pinned = assertPinnedPackageManifest(manifest, manifestDigest, checkpoint, checkpointDigest);
  const appPath = path.resolve(SOURCE_ASSEMBLY_DIR, manifest.appPath);
  const sourceRoot = await realpath(SOURCE_ASSEMBLY_DIR);
  if (!isPathInside(sourceRoot, appPath)) throw new Error('Pinned app path escapes the source assembly directory.');
  const canonicalAppPath = await realpath(appPath);
  if (canonicalAppPath !== appPath) throw new Error('Source app path contains a symlinked path component.');

  const sourceFiles = await createAppFileManifest(appPath);
  assertManifestFilesEqual(sourceFiles, manifest.files, 'Source app');
  const resourceManifest = sourceFiles.find((entry) => entry.path === RESOURCE_MANIFEST_PATH);
  if (!resourceManifest || resourceManifest.sha256 !== manifest.resourceManifestSha256) {
    throw new Error('Source runtime resource manifest does not match the assembly manifest pin.');
  }
  const resourceManifestRecordCount = manifest.runtimeResources?.files?.length;
  if (!Number.isSafeInteger(resourceManifestRecordCount) || resourceManifestRecordCount !== 18) {
    throw new Error(`Unexpected pinned runtime resource record count: ${resourceManifestRecordCount}`);
  }
  const sourceArchitectures = await inspectNativeArchitectures(appPath, sourceFiles);
  if (sourceArchitectures.length < 20) {
    throw new Error(`Source app contained too few native Mach-O files: ${sourceArchitectures.length}`);
  }

  return {
    manifest,
    manifestDigest,
    checkpointDigest,
    sourcePin: pinned.sourcePin,
    appPath,
    files: sourceFiles,
    architectures: sourceArchitectures,
    resourceManifestPin: {
      path: RESOURCE_MANIFEST_PATH,
      bytes: resourceManifest.byteLength,
      sha256: resourceManifest.sha256,
      fileRecordCount: resourceManifestRecordCount,
    },
  };
}

async function inspectNativeArchitectures(appPath, entries) {
  const candidates = entries.filter((entry) => entry.type === 'file'
    && (entry.executable || /\.(?:dylib|node|so)$/i.test(entry.path)));
  const architectures = [];
  for (const entry of candidates) {
    const filePath = path.join(appPath, ...entry.path.split('/'));
    const kind = run('file', ['-b', filePath]).trim();
    if (!kind.includes('Mach-O')) continue;
    const found = run('lipo', ['-archs', filePath]).trim().split(/\s+/).filter(Boolean).sort();
    if (found.length !== 1 || found[0] !== TARGET_ARCH) {
      throw new Error(`Native architecture mismatch for ${entry.path}: expected only ${TARGET_ARCH}, found ${found.join(', ')}`);
    }
    architectures.push({ path: entry.path, architectures: found });
  }
  return architectures.sort(compareFileEntries);
}

async function refreshSignedRuntimeResourceManifest(appPath, sourcePin, token) {
  const resourcesPath = path.join(appPath, 'Contents', 'Resources');
  const manifestPath = path.join(resourcesPath, 'resource-manifest.json');
  const manifestInfo = await lstat(manifestPath);
  if (!manifestInfo.isFile() || manifestInfo.isSymbolicLink()) {
    throw new Error(`Runtime resource manifest is not a regular file: ${manifestPath}`);
  }
  const beforeDigest = await sha256File(manifestPath);
  if (beforeDigest.sha256 !== sourcePin.sha256 || beforeDigest.bytes !== sourcePin.bytes) {
    throw new Error('The source runtime resource-manifest pin changed before its native code records could be refreshed.');
  }
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  const nativeRecords = manifest.files?.filter((record) => record.kind === 'rust' || record.kind === 'swift') ?? [];
  if (nativeRecords.length !== 7) {
    throw new Error(`Expected seven native resource binaries after signing; found ${nativeRecords.length}.`);
  }

  const actualRecords = [];
  for (const record of nativeRecords) {
    const binaryPath = path.resolve(resourcesPath, ...record.path.split('/'));
    if (!isPathInside(resourcesPath, binaryPath)) throw new Error(`Native resource path escapes Resources: ${record.path}`);
    const info = await lstat(binaryPath);
    if (!info.isFile() || info.isSymbolicLink() || (info.mode & 0o111) === 0) {
      throw new Error(`Signed native resource is not a regular executable: ${record.path}`);
    }
    const digest = await sha256File(binaryPath);
    actualRecords.push({ path: record.path, ...digest });
  }
  const updates = updateSignedNativeResourceRecords(manifest, actualRecords);
  const changed = updates.some((entry) => entry.preSignBytes !== entry.postSignBytes
    || entry.preSignSha256 !== entry.postSignSha256);
  if (changed) {
    const temporaryPath = `${manifestPath}.local-signing-${token}.tmp`;
    try {
      await writeFile(temporaryPath, `${JSON.stringify(manifest, null, 2)}\n`, {
        flag: 'wx',
        mode: manifestInfo.mode & 0o777,
      });
      await chmod(temporaryPath, manifestInfo.mode & 0o777);
      await rename(temporaryPath, manifestPath);
    } finally {
      await rm(temporaryPath, { force: true }).catch(() => {});
    }
  }
  const afterDigest = await sha256File(manifestPath);
  return {
    path: RESOURCE_MANIFEST_PATH,
    preSignBytes: beforeDigest.bytes,
    preSignSha256: beforeDigest.sha256,
    postSignBytes: afterDigest.bytes,
    postSignSha256: afterDigest.sha256,
    updatedNativeBinaryRecords: updates,
  };
}

function captureOuterSignatureMetadata(appPath) {
  return {
    details: parseSignatureDisplay(captureCombined('codesign', ['--display', '--verbose=4', appPath])),
    entitlements: captureCombined('codesign', ['--display', '--entitlements', '-', '--xml', appPath]),
    requirements: captureCombined('codesign', ['-d', '-r-', appPath]),
  };
}

function preservedOuterMetadataView(metadata) {
  const { authorities, TeamIdentifier, RuntimeVersion, flags, Identifier } = metadata.details;
  return {
    authorities,
    teamIdentifier: TeamIdentifier,
    runtimeVersion: RuntimeVersion,
    flags,
    identifier: Identifier,
    entitlements: metadata.entitlements,
    requirements: metadata.requirements,
  };
}

function resealOuterApp(appPath, identity) {
  const before = captureOuterSignatureMetadata(appPath);
  run('codesign', [
    '--force',
    '--sign', identity.sha1,
    '--preserve-metadata=identifier,entitlements,flags,runtime,requirements',
    '--timestamp=none',
    appPath,
  ]);
  const after = captureOuterSignatureMetadata(appPath);
  if (JSON.stringify(preservedOuterMetadataView(before)) !== JSON.stringify(preservedOuterMetadataView(after))) {
    throw new Error('Outer app re-seal changed preserved signing metadata (identifier, entitlements, flags, runtime, or requirements).');
  }
}

async function getPackageVersion(packageName) {
  const packageRequire = createRequire(path.join(REPO_ROOT, 'package.json'));
  const entryPath = packageRequire.resolve(packageName);
  let cursor = path.dirname(entryPath);
  while (true) {
    const packagePath = path.join(cursor, 'package.json');
    try {
      const manifest = JSON.parse(await readFile(packagePath, 'utf8'));
      if (manifest.name === packageName) return manifest.version;
    } catch (error) {
      if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) throw error;
    }
    const parent = path.dirname(cursor);
    if (parent === cursor) break;
    cursor = parent;
  }
  throw new Error(`Could not find package metadata for ${packageName} at ${entryPath}`);
}

export function parseSignatureDisplay(output) {
  const fields = {};
  for (const line of output.split(/\r?\n/)) {
    if (line.startsWith('CodeDirectory ')) {
      const codeDirectoryFlags = line.match(/\bflags=([^\s]+)/)?.[1];
      if (codeDirectoryFlags) fields.flags = codeDirectoryFlags;
    }
    const match = line.match(/^(Authority|TeamIdentifier|CDHash|Signature size|Timestamp|Runtime Version|flags|Identifier)=(.*)$/);
    if (!match) continue;
    const [, key, value] = match;
    if (key === 'Authority') (fields.authorities ??= []).push(value);
    else if (key === 'flags') fields.flags = value;
    else fields[key.replaceAll(' ', '')] = value;
  }
  return fields;
}

async function discoverSignatureObjects(appPath, fileEntries, architectures, identity) {
  const targets = new Map();
  targets.set(appPath, 'app');
  const architectureByPath = new Map(architectures.map((entry) => [entry.path, entry.architectures]));
  for (const entry of fileEntries) {
    if (entry.type !== 'file') continue;
    if (architectureByPath.has(entry.path)) targets.set(path.join(appPath, ...entry.path.split('/')), 'mach-o');
  }

  async function visit(currentPath, relativePath) {
    const children = await readdir(currentPath, { withFileTypes: true });
    for (const child of children) {
      const childPath = path.join(currentPath, child.name);
      const childRelative = relativePath ? `${relativePath}/${child.name}` : child.name;
      const info = await lstat(childPath);
      if (!info.isDirectory() || info.isSymbolicLink()) continue;
      if (childRelative.endsWith('.app')) targets.set(childPath, 'app-helper');
      else if (childRelative.endsWith('.framework')) targets.set(childPath, 'framework');
      await visit(childPath, childRelative);
    }
  }
  await visit(appPath, '');

  const objects = [];
  for (const [targetPath, kind] of [...targets.entries()].sort(([left], [right]) => left.localeCompare(right))) {
    const relativePath = path.relative(appPath, targetPath);
    run('codesign', ['--verify', '--strict', '--verbose=2', targetPath]);
    const display = parseSignatureDisplay(captureCombined('codesign', ['-d', '--verbose=4', targetPath]));
    const authority = display.authorities?.[0];
    if (authority !== identity.name || display.TeamIdentifier !== identity.teamIdentifier || !display.CDHash) {
      throw new Error(`Unexpected or missing code signature on ${relativePath || PRODUCT_NAME}.app`);
    }
    objects.push({
      path: relativePath ? toPosixPath(relativePath) : '.',
      kind,
      architectures: architectureByPath.get(toPosixPath(relativePath)) ?? undefined,
      authority,
      teamIdentifier: display.TeamIdentifier,
      cdHash: display.CDHash,
      signatureSize: display.Signaturesize ?? null,
      timestamp: display.Timestamp ?? null,
      runtimeVersion: display.RuntimeVersion ?? null,
      flags: display.flags ?? null,
      identifier: display.Identifier ?? null,
    });
  }
  return objects;
}

async function reserveOutput(outputPath) {
  await mkdir(outputPath);
  const token = randomUUID();
  const markerPath = path.join(outputPath, OWNER_MARKER);
  try {
    await writeFile(markerPath, `${JSON.stringify({ token, createdAt: new Date().toISOString() })}\n`, {
      flag: 'wx',
      mode: 0o600,
    });
  } catch (error) {
    await rmdir(outputPath).catch(() => {});
    throw error;
  }
  return { token, markerPath };
}

async function removeOwnedOutput(outputPath, token, markerPath) {
  let owner;
  try {
    owner = JSON.parse(await readFile(markerPath, 'utf8'));
  } catch {
    owner = null;
  }
  if (owner?.token !== token) {
    throw new Error(`Output cleanup refused because its ownership marker changed: ${outputPath}`);
  }
  await rm(outputPath, { recursive: true, force: true });
}

async function main(argv) {
  const args = parseArgs(argv);
  if (args.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }
  assertMacHost();
  await verifyOutputPath(args.output);

  const source = await verifySourceInputs();
  const identityOutput = run('security', ['find-identity', '-v', '-p', 'codesigning']);
  const identityRow = parseAppleDevelopmentIdentityOutput(identityOutput, args.identitySha1);
  const certificateOutput = run('security', ['find-certificate', '-a', '-c', identityRow.name, '-p']);
  const identity = parseAppleDevelopmentCertificateOutput(identityRow, certificateOutput);

  const [{ sign }, osxSignVersion] = await Promise.all([
    import('@electron/osx-sign'),
    getPackageVersion('@electron/osx-sign'),
  ]);
  if (typeof sign !== 'function') throw new Error('Installed @electron/osx-sign does not expose the documented sign() API.');

  const { token, markerPath } = await reserveOutput(args.output);
  let completed = false;
  const workPath = path.join(args.output, `.work-${token}`);
  const workAppPath = path.join(workPath, source.manifest.appPath);
  const finalAppPath = path.join(args.output, source.manifest.appPath);

  try {
    await mkdir(path.dirname(workAppPath), { recursive: true });
    run('ditto', ['--rsrc', '--extattr', source.appPath, workAppPath]);

    const copiedFiles = await createAppFileManifest(workAppPath);
    assertManifestFilesEqual(copiedFiles, source.manifest.files, 'Copied app before signing');
    const copiedArchitectures = await inspectNativeArchitectures(workAppPath, copiedFiles);
    if (JSON.stringify(copiedArchitectures) !== JSON.stringify(source.architectures)) {
      throw new Error('Copied app native architecture inventory differs from the verified source assembly.');
    }

    process.stdout.write(`Signing a verified copy of ${source.manifest.appPath} with @electron/osx-sign ${osxSignVersion}.\n`);
    await sign({
      app: workAppPath,
      identity: identity.sha1,
      identityValidation: true,
      platform: 'darwin',
      type: 'development',
      preAutoEntitlements: false,
      preEmbedProvisioningProfile: false,
      optionsForFile: () => ({ timestamp: 'none' }),
    });
    const resourceManifestReceipt = await refreshSignedRuntimeResourceManifest(
      workAppPath,
      source.resourceManifestPin,
      token,
    );
    resealOuterApp(workAppPath, identity);
    run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', workAppPath]);
    const verifiedResourceManifest = await verifyMacosRuntimeResourceManifest({
      root: REPO_ROOT,
      destination: path.join(workAppPath, 'Contents', 'Resources'),
      allowedAdditionalTopLevelPaths: deriveAllowedAdditionalResourceRoots(source.manifest),
    });
    const verifiedResourceManifestDigest = await sha256File(
      path.join(workAppPath, 'Contents', 'Resources', 'resource-manifest.json'),
    );
    if (verifiedResourceManifestDigest.sha256 !== resourceManifestReceipt.postSignSha256
      || verifiedResourceManifest.files.length !== source.resourceManifestPin.fileRecordCount) {
      throw new Error('Final resource-manifest verification differs from the signed resource receipt.');
    }

    const signedFiles = await createAppFileManifest(workAppPath);
    const signedArchitectures = await inspectNativeArchitectures(workAppPath, signedFiles);
    if (JSON.stringify(signedArchitectures) !== JSON.stringify(source.architectures)) {
      throw new Error('Signed app native architecture inventory differs from the verified source assembly.');
    }
    const signatureObjects = await discoverSignatureObjects(workAppPath, signedFiles, signedArchitectures, identity);

    await mkdir(path.dirname(finalAppPath), { recursive: true });
    await rename(workAppPath, finalAppPath);
    await rm(workPath, { recursive: true, force: true });
    run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', finalAppPath]);

    const finalFiles = await createAppFileManifest(finalAppPath);
    if (JSON.stringify(finalFiles) !== JSON.stringify(signedFiles)) {
      throw new Error('Final app tree changed while moving the signed copy into its output location.');
    }
    const artifactTreeSha256 = createHash('sha256').update(JSON.stringify(finalFiles)).digest('hex');
    const signingManifest = {
      schemaVersion: 1,
      kind: 'computer-use-local-development-signed-app',
      product: PRODUCT_NAME,
      bundleIdentifier: BUNDLE_ID,
      version: source.manifest.version,
      signed: true,
      developmentBuild: true,
      releaseEligible: false,
      source: {
        checkpointPath: CHECKPOINT_RELATIVE,
        checkpointSha256: source.checkpointDigest.sha256,
        assemblyManifestPath: SOURCE_MANIFEST_RELATIVE,
        assemblyManifestSha256: source.manifestDigest.sha256,
        assemblyManifestBytes: source.manifestDigest.bytes,
        checkpointAssemblyPin: source.sourcePin,
        sourceRevision: source.manifest.sourceRevision,
        sourceRevisionScope: source.manifest.sourceRevisionScope,
        workingTreeModified: source.manifest.workingTreeModified,
        assemblyBuildKind: source.manifest.buildKind,
        resourceManifestPin: source.resourceManifestPin,
        sourceAppPath: toPosixPath(path.relative(REPO_ROOT, source.appPath)),
        appPath: source.manifest.appPath,
        preSignAppFileCount: copiedFiles.length,
        preSignAppTreeSha256: createHash('sha256').update(JSON.stringify(copiedFiles)).digest('hex'),
      },
      signing: {
        platform: 'darwin',
        type: 'development',
        certificateKind: identity.certificateKind,
        certificateIdentifier: identity.certificateIdentifier,
        identitySha1: identity.sha1,
        identityName: identity.name,
        teamIdentifier: identity.teamIdentifier,
        signerPackage: '@electron/osx-sign',
        signerVersion: osxSignVersion,
        entitlementsPolicy: 'Built-in @electron/osx-sign Electron macOS defaults; no custom entitlement overrides.',
        provisioningProfileUsed: false,
        timestamp: 'disabled for local development signing',
        notarizationPerformed: false,
        productionDeveloperIdSignature: false,
        outerBundleResealed: true,
        outerResealPreservedMetadata: ['identifier', 'entitlements', 'flags', 'runtime', 'requirements'],
      },
      artifact: {
        outputAppPath: source.manifest.appPath,
        appTreeSha256: artifactTreeSha256,
        resourceManifest: resourceManifestReceipt,
        files: finalFiles,
        nativeArchitectures: signedArchitectures,
        codeSignatures: signatureObjects,
      },
    };
    const outputManifestPath = path.join(args.output, 'local-signing-manifest.json');
    await writeFile(outputManifestPath, `${JSON.stringify(signingManifest, null, 2)}\n`, { flag: 'wx', mode: 0o644 });
    await rm(markerPath);
    completed = true;
    process.stdout.write(`${JSON.stringify({
      output: args.output,
      app: finalAppPath,
      manifest: outputManifestPath,
      identity: `${identity.name} [${identity.sha1}]`,
      certificateKind: identity.certificateKind,
      certificateIdentifier: identity.certificateIdentifier,
      teamIdentifier: identity.teamIdentifier,
      signedCodeObjectCount: signatureObjects.length,
      nativeMachOCount: signedArchitectures.length,
      releaseEligible: false,
      notarized: false,
    }, null, 2)}\n`);
  } finally {
    if (!completed) await removeOwnedOutput(args.output, token, markerPath);
  }
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : null;
if (invokedPath === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
