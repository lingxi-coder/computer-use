import assert from 'node:assert/strict';
import test from 'node:test';

import {
  assertPermittedOutputPath,
  deriveAllowedAdditionalResourceRoots,
  parseAppleDevelopmentCertificateMetadata,
  parseAppleDevelopmentIdentityOutput,
  parseArgs,
  parseSignatureDisplay,
  updateSignedNativeResourceRecords,
} from '../sign-macos-local-app.mjs';

const DEVELOPMENT_SHA1 = '0123456789ABCDEF0123456789ABCDEF01234567';

test('requires a new absolute output path and an exact SHA-1 identity fingerprint', () => {
  assert.deepEqual(parseArgs([
    '--output', '/tmp/computer-use-signed-development',
    '--identity-sha1', DEVELOPMENT_SHA1.toLowerCase(),
  ]), {
    output: '/tmp/computer-use-signed-development',
    identitySha1: DEVELOPMENT_SHA1,
  });
  assert.throws(() => parseArgs(['--output', 'relative/output', '--identity-sha1', DEVELOPMENT_SHA1]),
    /must be an absolute path/);
  assert.throws(() => parseArgs(['--output', '/tmp/output', '--identity-sha1', 'bad']),
    /40-character SHA-1/);
});

test('resolves Apple Development team identity from certificate OU, not the CN suffix', () => {
  const certificateIdentifier = 'KQ7KX8LCYL';
  const output = [
    `  1) ${DEVELOPMENT_SHA1} "Apple Development: Developer Example (${certificateIdentifier})"`,
    '     1 valid identities found',
  ].join('\n');
  const identity = parseAppleDevelopmentIdentityOutput(output, DEVELOPMENT_SHA1);
  assert.deepEqual(identity, {
    sha1: DEVELOPMENT_SHA1,
    name: `Apple Development: Developer Example (${certificateIdentifier})`,
    certificateKind: 'Apple Development',
    certificateIdentifier,
  });
  assert.deepEqual(parseAppleDevelopmentCertificateMetadata(
    identity,
    DEVELOPMENT_SHA1,
    `CN=Apple Development: Developer Example (${certificateIdentifier}),OU=AZ4AX7J833,O=Apple Inc.,C=US`,
  ), {
    ...identity,
    teamIdentifier: 'AZ4AX7J833',
  });
});

test('rejects an unavailable identity, duplicate fingerprint, or Developer ID signing identity', () => {
  assert.throws(() => parseAppleDevelopmentIdentityOutput('', DEVELOPMENT_SHA1), /found 0/);
  const duplicate = [
    `  1) ${DEVELOPMENT_SHA1} "Apple Development: Developer Example (ABCDE12345)"`,
    `  2) ${DEVELOPMENT_SHA1} "Apple Development: Developer Example (ABCDE12345)"`,
  ].join('\n');
  assert.throws(() => parseAppleDevelopmentIdentityOutput(duplicate, DEVELOPMENT_SHA1), /found 2/);
  const distribution = `  1) ${DEVELOPMENT_SHA1} "Developer ID Application: Developer Example (ABCDE12345)"`;
  assert.throws(() => parseAppleDevelopmentIdentityOutput(distribution, DEVELOPMENT_SHA1), /requires an Apple Development/);
});

test('allows output in a separate canonical artifact and rejects writes below the frozen source package', () => {
  const artifacts = '/workspace/computer-use/artifacts';
  const sourcePackage = `${artifacts}/mvp-macos-package-003445c7-2dcc-4ca7-b475-837b3db357bd`;
  const sourceAssembly = `${sourcePackage}/assembly-01`;
  assert.doesNotThrow(() => assertPermittedOutputPath(
    `${artifacts}/mvp-model-business-aca79bad-f1c7-4af1-99e6-9dc12ce060e1/signed-development-01`,
    artifacts,
    sourcePackage,
    sourceAssembly,
  ));
  assert.throws(() => assertPermittedOutputPath(
    `${sourcePackage}/signed-development-01`, artifacts, sourcePackage, sourceAssembly,
  ), /immutable source package artifact/);
  assert.throws(() => assertPermittedOutputPath(
    `${sourceAssembly}/signed-development-01`, artifacts, sourcePackage, sourceAssembly,
  ), /immutable source package artifact/);
  assert.throws(() => assertPermittedOutputPath(
    `${artifacts}/signed-development-01`, artifacts, sourcePackage, sourceAssembly,
  ), /canonical artifact directory/);
});

test('refreshes only the seven signed native records in the resource manifest', () => {
  const manifest = {
    kind: 'macos_runtime_resource_manifest',
    architecture: 'arm64',
    laya: { package_manifest_sha256: 'pinned-model-manifest' },
    files: [
      ...Array.from({ length: 7 }, (_, index) => ({
        path: `bin/native-${index}`,
        kind: index < 4 ? 'rust' : 'swift',
        bytes: index + 10,
        sha256: 'a'.repeat(64),
        executable: true,
        architectures: ['arm64'],
        build_kind: 'debug',
      })),
      {
        path: 'models/laya/package-manifest.json',
        kind: 'model-manifest',
        bytes: 123,
        sha256: 'b'.repeat(64),
        executable: false,
      },
    ],
  };
  const before = structuredClone(manifest);
  const actual = manifest.files.filter((entry) => entry.kind === 'rust' || entry.kind === 'swift')
    .map((entry) => ({ path: entry.path, bytes: entry.bytes + 5, sha256: 'c'.repeat(64) }));

  const updates = updateSignedNativeResourceRecords(manifest, actual);
  assert.equal(updates.length, 7);
  for (let index = 0; index < 7; index += 1) {
    assert.equal(manifest.files[index].bytes, before.files[index].bytes + 5);
    assert.equal(manifest.files[index].sha256, 'c'.repeat(64));
    assert.equal(manifest.files[index].architectures[0], 'arm64');
    assert.equal(updates[index].preSignSha256, before.files[index].sha256);
  }
  assert.deepEqual(manifest.files[7], before.files[7]);
  assert.deepEqual(manifest.laya, before.laya);
});

test('derives only pinned Electron resource roots outside the runtime manifest inventory', () => {
  const roots = deriveAllowedAdditionalResourceRoots({ files: [
    { path: 'Contents/Resources/app/out/main/index.js' },
    { path: 'Contents/Resources/electron.icns' },
    { path: 'Contents/Resources/bin/cu-host' },
    { path: 'Contents/Resources/fixtures/headless/readonly-page.html' },
    { path: 'Contents/Resources/models/laya/encoder.onnx' },
    { path: 'Contents/Resources/LICENSES/LingXi-MIT.txt' },
    { path: 'Contents/Resources/resource-manifest.json' },
    { path: 'Contents/Frameworks/Electron Framework.framework/Electron Framework' },
  ] });
  assert.deepEqual(roots, ['app', 'electron.icns']);
  assert.throws(() => deriveAllowedAdditionalResourceRoots({ files: [
    { path: 'Contents/Resources/electron.icns' },
  ] }), /does not include Contents\/Resources\/app/);
});

test('reads hardened runtime flags embedded in the CodeDirectory display line', () => {
  const details = parseSignatureDisplay([
    'Identifier=com.lingxi.computer-use',
    'CodeDirectory v=20500 size=1234 flags=0x10000(runtime) hashes=31+7 location=embedded',
    'TeamIdentifier=AZ4AX7J833',
  ].join('\n'));
  assert.equal(details.flags, '0x10000(runtime)');
  assert.equal(details.Identifier, 'com.lingxi.computer-use');
});
