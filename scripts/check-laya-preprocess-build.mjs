#!/usr/bin/env node
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

// Reproduce the vendor build in a temporary directory without editing source,
// checked-in runtime, manifest, or the application's compiler settings.
const root = fileURLToPath(new URL('..', import.meta.url));
const vendor = join(root, 'packages/decision-worker/src/laya/vendor/laya-ts');
const pins = JSON.parse(readFileSync(join(vendor, 'build-pins.json'), 'utf8'));
const compiler = join(root, 'node_modules/typescript');
const version = JSON.parse(readFileSync(join(compiler, 'package.json'), 'utf8')).version;
const hash = path => createHash('sha256').update(readFileSync(path)).digest('hex');
assert.equal(version, '5.9.3');
assert.equal(pins.typescript_version, version);
assert.equal(pins.source_commit, '2e4d9c87e8b1621deb344eac7de5c7258f32f849');
assert.deepEqual(pins.compiler_options, {
  target: 'ES2022', module: 'NodeNext', strict: true,
  noUncheckedIndexedAccess: false, declaration: true,
});
assert.deepEqual(Object.keys(pins.source_sha256).sort(), ['src/common.ts', 'src/tokenizer.ts']);
assert.deepEqual(Object.keys(pins.runtime_sha256).sort(), [
  'runtime/common.d.ts', 'runtime/common.js', 'runtime/tokenizer.d.ts', 'runtime/tokenizer.js',
]);
for (const [path, expected] of Object.entries(pins.source_sha256)) assert.equal(hash(join(vendor, path)), expected);
const output = mkdtempSync(join(tmpdir(), 'cu-laya-preprocess-build-'));
try {
  execFileSync(process.execPath, [join(compiler, 'bin/tsc'), '--target', 'ES2022',
    '--module', 'NodeNext', '--moduleResolution', 'NodeNext', '--strict',
    '--noUncheckedIndexedAccess', 'false', '--skipLibCheck', '--declaration',
    '--rootDir', join(vendor, 'src'), '--outDir', output,
    join(vendor, 'src/tokenizer.ts'), join(vendor, 'src/common.ts')], { cwd: root, stdio: 'pipe' });
  for (const [path, expected] of Object.entries(pins.runtime_sha256)) {
    assert.equal(hash(join(vendor, path)), expected, path);
    assert.equal(hash(join(output, path.slice('runtime/'.length))), expected, `rebuilt ${path}`);
  }
  console.log(JSON.stringify({ ok: true, typescript_version: version, reproduced_runtime_files: 4 }));
} finally { rmSync(output, { recursive: true, force: true }); }
