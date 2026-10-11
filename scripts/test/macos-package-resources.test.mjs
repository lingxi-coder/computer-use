import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import {
  LAYA_MODEL_REVISION,
  LAYA_SOURCE_COMMIT,
  MACOS_MINIMUM_VERSION,
  MACOS_RESOURCE_MANIFEST,
  MACOS_RUNTIME_RESOURCE_PATHS,
  assertTargetArchitecture,
  inspectMachOArchitectures,
  validateMacosResourcePathInventory,
  verifyMacosRuntimeResourceManifest,
  verifyPinnedLayaModelSource,
} from '../lib/macos-package-resources.mjs';

const ROOT = new URL('../..', import.meta.url).pathname;

function thinMachO(architecture) {
  const cpuType = architecture === 'arm64' ? 0x0100000c : 0x01000007;
  const header = Buffer.alloc(32);
  header.writeUInt32LE(0xfeedfacf, 0);
  header.writeUInt32LE(cpuType, 4);
  header.writeUInt32LE(0, 8);
  header.writeUInt32LE(2, 12);
  return header;
}

function fatMachO(architectures) {
  const header = Buffer.alloc(8 + architectures.length * 20);
  header.writeUInt32BE(0xcafebabe, 0);
  header.writeUInt32BE(architectures.length, 4);
  architectures.forEach((architecture, index) => {
    header.writeUInt32BE(architecture === 'arm64' ? 0x0100000c : 0x01000007, 8 + index * 20);
  });
  return header;
}

test('recognizes thin Mach-O architecture headers', () => {
  assert.deepEqual(inspectMachOArchitectures(thinMachO('arm64')), ['arm64']);
  assert.deepEqual(inspectMachOArchitectures(thinMachO('x64')), ['x64']);
});

test('recognizes universal Mach-O architecture headers so staging can reject mixed targets', () => {
  const arches = inspectMachOArchitectures(fatMachO(['arm64', 'x64']));
  assert.deepEqual(arches, ['arm64', 'x64']);
  assert.throws(() => assertTargetArchitecture('helper', arches, 'arm64'), /expected only arm64/);
});

test('rejects a Mach-O executable for the wrong package architecture', () => {
  assert.throws(() => assertTargetArchitecture('cu-host', inspectMachOArchitectures(thinMachO('x64')), 'arm64'),
    /expected only arm64, found x64/);
  assert.equal(assertTargetArchitecture('cu-host', inspectMachOArchitectures(thinMachO('arm64')), 'arm64'), 'arm64');
});

test('allows only explicitly named app-owned top-level resources after Packager assembly', () => {
  assert.doesNotThrow(() => validateMacosResourcePathInventory([
    ...MACOS_RUNTIME_RESOURCE_PATHS,
    'app.asar',
    'app.asar.unpacked/node_modules/native/addon.node',
  ], ['app.asar', 'app.asar.unpacked']));
  assert.throws(() => validateMacosResourcePathInventory([
    ...MACOS_RUNTIME_RESOURCE_PATHS,
    'bin/unlisted-helper',
  ], ['app.asar', 'bin']), /invalid or colliding additional top-level path/);
  assert.throws(() => validateMacosResourcePathInventory([
    ...MACOS_RUNTIME_RESOURCE_PATHS,
    'models/laya-multilingual/unlisted.json',
  ], ['app.asar']), /unlisted files/);
});

test('rejects truncated and non-Mach-O executable headers', () => {
  assert.throws(() => inspectMachOArchitectures(Buffer.from([0xcf, 0xfa, 0xed])), /truncated/);
  assert.throws(() => inspectMachOArchitectures(Buffer.from('#!/bin/sh')), /Mach-O/);
});

test('validates the pinned Laya package identity, declared license, and six-file inventory without reading model weights', async () => {
  const verified = await verifyPinnedLayaModelSource(ROOT);
  assert.equal(verified.model_revision, LAYA_MODEL_REVISION);
  assert.equal(verified.source_commit, LAYA_SOURCE_COMMIT);
  assert.equal(verified.package_files['encoder.onnx.data'].bytes, 1_227_816_960);
  assert.equal(Object.keys(verified.package_files).length, 6);
});

test('rejects a modified Laya package manifest before staging any resource files', async (context) => {
  const root = await mkdtemp(join(tmpdir(), 'cu-laya-resource-manifest-'));
  context.after(async () => rm(root, { recursive: true, force: true }));
  const modelRoot = join(root, 'target/model-exports/laya-multilingual', LAYA_MODEL_REVISION);
  await mkdir(modelRoot, { recursive: true });
  await writeFile(join(modelRoot, 'package-manifest.json'), '{"kind":"laya_onnx_package_manifest"}\n');
  await assert.rejects(verifyPinnedLayaModelSource(root), /package-manifest\.json SHA-256 mismatch/);
});

test('rejects a staged manifest whose Laya revision is not the pinned package', async (context) => {
  const stage = await mkdtemp(join(tmpdir(), 'cu-macos-resource-stage-'));
  context.after(async () => rm(stage, { recursive: true, force: true }));
  await writeFile(join(stage, MACOS_RESOURCE_MANIFEST), JSON.stringify({
    kind: 'macos_runtime_resource_manifest', schema_version: 1,
    source_revision: 'a'.repeat(40), architecture: 'arm64', build_kind: 'debug',
    source_revision_is_head_only: true, source_tree_state: 'not_attested',
    development_build: true, distribution_claim: 'local-development-package',
    platform: 'darwin', minimum_macos_version: MACOS_MINIMUM_VERSION,
    laya: { model_revision: 'b'.repeat(40), source_commit: LAYA_SOURCE_COMMIT }, files: [],
  }));
  await assert.rejects(verifyMacosRuntimeResourceManifest({ destination: stage }), /Laya pin mismatch/);
});
