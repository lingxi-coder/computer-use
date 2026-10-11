import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { activateOwnedDocumentForObservation, createOwnedDocumentLaunchRunner,
  encodeOwnedDocumentSourceBootstrap } from '../apps/desktop/src/main/ownedDocumentLaunch.js';

assert.deepEqual(process.argv.slice(2), ['--probe']);
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const source = join(root, 'native/device/fixtures/OwnedFocusMessagingProbe.swift');
const menuSource = join(root, 'native/device/fixtures/OwnedSaveMenuRead.swift');
const binary = join(root, 'target/debug/cu-owned-focus-probe');
const artifact = join(root, 'artifacts', `m3-owned-focus-messaging-${randomUUID()}.json`);
const hash = (file: string) => createHash('sha256').update(readFileSync(file)).digest('hex');
const evidence: Record<string, unknown> = { kind: 'owned_focus_messaging_diagnostic', schema_version: 1,
  started_at: new Date().toISOString(), ok: false, input_calls: 0, editing_verified: false,
  mvp_complete: false, source_sha256: hash(source), menu_source_sha256: hash(menuSource),
  script_sha256: hash(fileURLToPath(import.meta.url)) };
const runner = createOwnedDocumentLaunchRunner();
let phase = 'compile';
try {
  execFileSync('xcrun', ['swiftc', '-swift-version', '6', '-strict-concurrency=complete', '-warnings-as-errors',
    '-parse-as-library', '-module-cache-path', join(root, 'target/native-macos/module-cache'),
    source, menuSource, '-o', binary], { cwd: root, timeout: 30_000, stdio: ['ignore', 'ignore', 'pipe'] });
  evidence.binary_sha256 = hash(binary);
  phase = 'launch';
  const registration = await runner.launch('Owned AX messaging diagnostic\n零输入焦点读取\n');
  assert.equal(await activateOwnedDocumentForObservation(registration), true);
  const prefix = encodeOwnedDocumentSourceBootstrap(registration);
  const input = Buffer.from(prefix.subarray(4));
  prefix.fill(0);
  phase = 'readonly_probe';
  try {
    const output = execFileSync(binary, ['--probe'], { input, encoding: 'utf8', timeout: 4_000, maxBuffer: 16_384 });
    const reads: unknown = JSON.parse(output);
    if (reads && typeof reads === 'object' && !Array.isArray(reads)
      && 'accessibility_trusted' in reads && reads.accessibility_trusted === false) {
      evidence.accessibility_trusted = false;
      evidence.scope_current = true;
    } else {
      assert.ok(reads && typeof reads === 'object' && 'reads' in reads
        && Array.isArray(reads.reads) && reads.reads.length === 20);
      evidence.diagnostic = reads;
      evidence.ok = true;
    }
  } finally { input.fill(0); }
} catch (error) {
  evidence.failure_phase = phase;
  evidence.failure_type = error instanceof Error ? error.name : 'unknown';
  if (error && typeof error === 'object' && 'status' in error && typeof error.status === 'number') {
    evidence.helper_exit_code = error.status;
  }
} finally {
  evidence.cleanup = await runner.stopAll();
  evidence.source_unchanged = hash(source) === evidence.source_sha256;
  evidence.menu_source_unchanged = hash(menuSource) === evidence.menu_source_sha256;
  evidence.finished_at = new Date().toISOString();
  writeFileSync(artifact, JSON.stringify(evidence, null, 2) + '\n', { mode: 0o600, flag: 'wx' });
}
console.log(JSON.stringify({ ok: evidence.ok, artifact, input_calls: 0, cleanup: evidence.cleanup }));
process.exitCode = evidence.ok ? 0 : 1;
