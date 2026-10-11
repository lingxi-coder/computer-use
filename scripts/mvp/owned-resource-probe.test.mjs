import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, realpathSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
  createOwnedTempScope, createProcessGroupWitness, probeOwnedResources, probeOwnedTempScope, probeProcessGroup,
} from './owned-resource-probe.mjs';

function temporaryRoot(t) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'cu-owned-resource-probe-')));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}

function waitForClose(child) {
  return new Promise(resolveClose => child.once('close', resolveClose));
}

test('owned temp probe is bound to coordinator directory identity and emptiness', t => {
  const root = temporaryRoot(t);
  const scope = createOwnedTempScope(root, 'driver');
  assert.equal(probeOwnedTempScope(scope).status, 'passed');

  const leftoverPath = join(scope.path, 'leftover.tmp');
  writeFileSync(leftoverPath, 'test-owned resource');
  assert.deepEqual(probeOwnedTempScope(scope), {
    status: 'failed', reason: 'owned_temp_scope_has_remaining_entries', entry_count: 1,
  });
  rmSync(leftoverPath);
  assert.equal(probeOwnedTempScope(scope).status, 'passed');

  const movedPath = join(root, 'original-scope');
  renameSync(scope.path, movedPath);
  mkdirSync(scope.path, { mode: 0o700 });
  assert.equal(probeOwnedTempScope(scope).status, 'unknown');
});

test('process-group probe sees a live coordinator-owned group, then confirms it absent after exact child exit', async t => {
  const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {
    detached: true,
    stdio: 'ignore',
  });
  assert.ok(child.pid);
  const closed = waitForClose(child);
  const witness = createProcessGroupWitness({ pid: child.pid, token: randomUUID() });
  try {
    const live = await probeProcessGroup(witness, { timeoutMs: 40, pollIntervalMs: 5 });
    assert.equal(live.status, 'failed');
    assert.equal(live.reason, 'process_group_still_present');

    child.kill('SIGTERM');
    await closed;
    const gone = await probeProcessGroup(witness, { timeoutMs: 100, pollIntervalMs: 5 });
    assert.equal(gone.status, 'passed');
    assert.equal(gone.reason, 'process_group_absent');
  } finally {
    if (child.exitCode === null && child.signalCode === null) {
      try { child.kill('SIGKILL'); } catch { /* exact test-owned child only */ }
      await closed;
    }
  }
});

test('missing process identity or unsupported ownership scope stays unknown and never verifies product coverage', async () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'cu-owned-resource-probe-')));
  try {
    const tempScope = createOwnedTempScope(root, 'driver');
    const probe = await probeOwnedResources({
      processWitness: null,
      tempScope,
      directChildClosed: true,
    });
    assert.equal(probe.status, 'unknown');
    assert.equal(probe.group_cleanup_probe_passed, false);
    assert.equal(probe.temp_scope.status, 'passed');
    assert.equal(probe.product_owned_resource_coverage_verified, false);
    assert.match(probe.limitation, /product ownership coverage remains unverified/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
