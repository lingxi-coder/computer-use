import { randomUUID } from 'node:crypto';
import { lstatSync, mkdirSync, readdirSync, realpathSync } from 'node:fs';
import { basename, resolve } from 'node:path';
import { performance } from 'node:perf_hooks';

const TOKEN = /^[0-9a-f-]{36}$/i;

export function createOwnedTempScope(parentDirectory, kind) {
  if (!['driver', 'evaluator'].includes(kind)) throw new TypeError('owned temp scope kind is invalid');
  if (resolve(parentDirectory) !== parentDirectory || realpathSync(parentDirectory) !== parentDirectory) {
    throw new TypeError('owned temp scope parent must be a canonical absolute path');
  }
  const token = randomUUID();
  const path = resolve(parentDirectory, `.owned-${kind}-tmp-${token}`);
  mkdirSync(path, { mode: 0o700 });
  const stat = lstatSync(path);
  if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o777) !== 0o700) {
    throw new Error('coordinator-owned temp scope could not be created privately');
  }
  if (typeof process.getuid === 'function' && stat.uid !== process.getuid()) throw new Error('coordinator-owned temp scope owner differs');
  if (realpathSync(path) !== path) throw new Error('coordinator-owned temp scope path is not canonical');
  return {
    version: 1,
    kind,
    token,
    path,
    realpath: path,
    device: stat.dev,
    inode: stat.ino,
    uid: stat.uid,
    mode: stat.mode & 0o777,
    created_at_monotonic_ms: performance.now(),
  };
}

export function createProcessGroupWitness({ pid, token }) {
  if (!Number.isSafeInteger(pid) || pid <= 0 || !TOKEN.test(token)) throw new TypeError('process group witness identity is invalid');
  return {
    version: 1,
    kind: 'coordinator_spawned_posix_process_group',
    token,
    pid,
    process_group_id: pid,
    detached_spawn: true,
    spawned_at_monotonic_ms: performance.now(),
  };
}

export async function probeProcessGroup(witness, { timeoutMs = 300, pollIntervalMs = 20 } = {}) {
  if (!validProcessGroupWitness(witness)) return { status: 'unknown', reason: 'process_group_identity_missing_or_invalid' };
  if (process.platform === 'win32') return { status: 'unknown', reason: 'posix_process_group_probe_unsupported' };
  const deadline = performance.now() + timeoutMs;
  while (true) {
    try {
      process.kill(-witness.process_group_id, 0);
    } catch (error) {
      if (error?.code === 'ESRCH') return { status: 'passed', reason: 'process_group_absent', witness };
      if (error?.code !== 'EINTR') return { status: 'unknown', reason: `process_group_probe_${error?.code ?? 'failed'}`, witness };
    }
    if (performance.now() >= deadline) return { status: 'failed', reason: 'process_group_still_present', witness };
    await new Promise(resolveDelay => setTimeout(resolveDelay, pollIntervalMs));
  }
}

export function probeOwnedTempScope(scope) {
  if (!validTempScope(scope)) return { status: 'unknown', reason: 'owned_temp_scope_identity_missing_or_invalid' };
  try {
    const stat = lstatSync(scope.path);
    if (!stat.isDirectory() || stat.isSymbolicLink() || realpathSync(scope.path) !== scope.realpath) {
      return { status: 'unknown', reason: 'owned_temp_scope_path_changed' };
    }
    if (stat.dev !== scope.device || stat.ino !== scope.inode || stat.uid !== scope.uid || (stat.mode & 0o777) !== scope.mode) {
      return { status: 'unknown', reason: 'owned_temp_scope_identity_changed' };
    }
    const entries = readdirSync(scope.path);
    if (entries.length > 0) return { status: 'failed', reason: 'owned_temp_scope_has_remaining_entries', entry_count: entries.length };
    return { status: 'passed', reason: 'owned_temp_scope_identity_matches_and_is_empty', entry_count: 0 };
  } catch (error) {
    return { status: 'unknown', reason: `owned_temp_scope_probe_${error?.code ?? 'failed'}` };
  }
}

export async function probeOwnedResources({ processWitness, tempScope, directChildClosed, timeoutMs = 300, pollIntervalMs = 20 }) {
  const processGroup = processWitness?.token === tempScope?.token
    ? await probeProcessGroup(processWitness, { timeoutMs, pollIntervalMs })
    : { status: 'unknown', reason: 'process_group_and_temp_scope_token_mismatch' };
  const temp = probeOwnedTempScope(tempScope);
  const directChild = directChildClosed === true
    ? { status: 'passed', reason: 'coordinator_child_close_confirmed' }
    : { status: 'unknown', reason: 'coordinator_child_close_not_confirmed' };
  const passed = directChild.status === 'passed' && processGroup.status === 'passed' && temp.status === 'passed';
  return {
    version: 1,
    status: passed ? 'passed' : [directChild, processGroup, temp].some(item => item.status === 'failed') ? 'failed' : 'unknown',
    scope: 'coordinator_process_group_and_private_temp_subtree_only',
    group_cleanup_probe_passed: processGroup.status === 'passed',
    product_owned_resource_coverage_verified: false,
    direct_child: directChild,
    process_group: processGroup,
    temp_scope: temp,
    limitation: 'Does not cover processes that leave the coordinator-created process group or product resources owned outside this scope; product ownership coverage remains unverified.',
  };
}

function validProcessGroupWitness(witness) {
  return witness?.version === 1
    && witness.kind === 'coordinator_spawned_posix_process_group'
    && TOKEN.test(witness.token)
    && Number.isSafeInteger(witness.pid) && witness.pid > 0
    && witness.process_group_id === witness.pid
    && witness.detached_spawn === true
    && Number.isFinite(witness.spawned_at_monotonic_ms);
}

function validTempScope(scope) {
  return scope?.version === 1
    && ['driver', 'evaluator'].includes(scope.kind)
    && TOKEN.test(scope.token)
    && typeof scope.path === 'string' && resolve(scope.path) === scope.path
    && basename(scope.path) === `.owned-${scope.kind}-tmp-${scope.token}`
    && scope.realpath === scope.path
    && Number.isFinite(scope.device) && Number.isFinite(scope.inode)
    && Number.isSafeInteger(scope.uid)
    && scope.mode === 0o700
    && Number.isFinite(scope.created_at_monotonic_ms);
}
