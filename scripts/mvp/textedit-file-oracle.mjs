import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { closeSync, constants, fstatSync, lstatSync, openSync, readSync, realpathSync } from 'node:fs';
import { isAbsolute, join, resolve } from 'node:path';
import { textEditTaskIds } from './business-results.mjs';

const MAX_FILE_BYTES = 1024 * 1024;
const HASH = /^[a-f0-9]{64}$/;
const OPERATIONS = ['write_requested_contents', 'save', 'close', 'reopen_and_read_back'];

function exact(value, keys, label) {
  assert.ok(value && typeof value === 'object' && !Array.isArray(value), `${label} must be an object`);
  assert.deepEqual(Object.keys(value).sort(), [...keys].sort(), `${label} has unexpected fields`);
}

function sameFile(a, b) {
  return a.dev === b.dev && a.ino === b.ino && a.size === b.size && a.mode === b.mode
    && a.uid === b.uid && a.nlink === b.nlink && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs;
}

function identity(profileTruth, allowedScope) {
  exact(profileTruth, ['suite', 'task_id', 'seed', 'evaluator_truth'], 'TextEdit truth');
  assert.equal(profileTruth.suite, 'textedit');
  assert.ok(textEditTaskIds.includes(profileTruth.task_id), 'unknown TextEdit task identity');
  assert.equal(profileTruth.seed, 1);
  exact(allowedScope, ['application', 'output_directory', 'file_name', 'document_format', 'operations', 'no_other_files'], 'TextEdit scope');
  assert.equal(allowedScope.application, 'TextEdit');
  assert.equal(allowedScope.document_format, 'plain_text');
  assert.equal(allowedScope.no_other_files, true);
  assert.deepEqual(allowedScope.operations, OPERATIONS);
  const fileName = `cu-mvp-${profileTruth.task_id.toLowerCase()}.txt`;
  assert.equal(allowedScope.file_name, fileName);
  assert.ok(typeof allowedScope.output_directory === 'string' && isAbsolute(allowedScope.output_directory)
    && resolve(allowedScope.output_directory) === allowedScope.output_directory,
  'TextEdit output directory must be canonical and absolute');
  const truth = profileTruth.evaluator_truth;
  exact(truth, ['expected_file_name', 'expected_format', 'expected_content_sha256', 'expected_content_byte_length',
    'expected_ui_close_reopen_readback', 'expected_initial_file_state', 'destructive_overwrite_confirmation',
    'evaluator_must_read_saved_file_independently'], 'TextEdit evaluator conditions');
  assert.equal(truth.expected_file_name, fileName);
  assert.equal(truth.expected_format, 'plain_text');
  assert.ok(typeof truth.expected_content_sha256 === 'string' && HASH.test(truth.expected_content_sha256));
  assert.ok(Number.isSafeInteger(truth.expected_content_byte_length)
    && truth.expected_content_byte_length > 0 && truth.expected_content_byte_length <= MAX_FILE_BYTES);
  assert.equal(truth.expected_ui_close_reopen_readback, true);
  assert.equal(truth.evaluator_must_read_saved_file_independently, true);
  const existing = Number(profileTruth.task_id.slice(-2)) >= 11;
  assert.equal(truth.expected_initial_file_state, existing ? 'existing_disposable_test_file' : 'target_file_absent');
  assert.ok(truth.destructive_overwrite_confirmation && typeof truth.destructive_overwrite_confirmation === 'object');
  assert.equal(truth.destructive_overwrite_confirmation.required, existing);
  assert.equal(truth.destructive_overwrite_confirmation.actual_confirmation_event_required, existing);
  return { suite: 'textedit', task_id: profileTruth.task_id, seed: 1 };
}

/**
 * Independent post-run file condition only (design 14.7). Never supplies data
 * to Source, models, Goal, or Memory, and never proves UI reopen or consent.
 * The expected hash/length retain the frozen profile's exact UTF-8 byte rule.
 */
export function readTextEditFileOracle({ profileTruth, allowedScope }) {
  const task = identity(profileTruth, allowedScope);
  const filePath = join(allowedScope.output_directory, allowedScope.file_name);
  const result = (verdict, reasonCodes, fileEvidence = null) => ({
    version: 1, ...task, channel: 'independent_saved_file', verdict, reason_codes: reasonCodes,
    file_evidence: fileEvidence, ui_close_reopen_status: 'not_checked', overwrite_confirmation_status: 'not_checked',
    business_evidence_validity: 'unknown', business_completion_verified: false, mvp_complete: false,
  });
  let fd;
  try {
    const directory = lstatSync(allowedScope.output_directory);
    if (!directory.isDirectory() || directory.isSymbolicLink() || (directory.mode & 0o777) !== 0o700
      || (typeof process.getuid === 'function' && directory.uid !== process.getuid())
      || realpathSync(allowedScope.output_directory) !== allowedScope.output_directory) {
      return result('unknown', ['output_scope_not_private_canonical_directory']);
    }
    let before;
    try { before = lstatSync(filePath); }
    catch (error) {
      if (error.code === 'ENOENT') return result('not_satisfied', ['saved_file_missing']);
      return result('unknown', ['saved_file_unreadable']);
    }
    if (!before.isFile() || before.isSymbolicLink() || before.nlink !== 1
      || (typeof process.getuid === 'function' && before.uid !== process.getuid())
      || realpathSync(filePath) !== filePath) return result('unknown', ['saved_file_not_owned_regular_file']);
    if (before.size > MAX_FILE_BYTES) return result('unknown', ['saved_file_exceeds_read_bound']);
    fd = openSync(filePath, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = fstatSync(fd);
    if (!sameFile(before, opened)) return result('unknown', ['saved_file_changed_before_read']);
    const bytes = Buffer.alloc(opened.size);
    let offset = 0;
    while (offset < bytes.length) {
      const count = readSync(fd, bytes, offset, bytes.length - offset, offset);
      if (count === 0) return result('unknown', ['saved_file_changed_during_read']);
      offset += count;
    }
    const afterDirectory = lstatSync(allowedScope.output_directory);
    if (!sameFile(opened, fstatSync(fd)) || !sameFile(opened, lstatSync(filePath))
      || afterDirectory.dev !== directory.dev || afterDirectory.ino !== directory.ino
      || afterDirectory.mode !== directory.mode || afterDirectory.uid !== directory.uid
      || realpathSync(allowedScope.output_directory) !== allowedScope.output_directory) {
      return result('unknown', ['saved_file_or_scope_changed_during_read']);
    }
    const sha256 = createHash('sha256').update(bytes).digest('hex');
    let validUtf8 = true;
    try { new TextDecoder('utf-8', { fatal: true }).decode(bytes); } catch { validUtf8 = false; }
    const plainText = validUtf8 && !bytes.includes(0) && !/^\s*\{\\rtf\d/iu.test(bytes.toString('utf8', 0, 128));
    const evidence = { path: filePath, sha256, bytes: bytes.length, regular_file: true, stable_read: true,
      encoding: validUtf8 ? 'utf8' : 'unsupported', plain_text: plainText };
    const reasons = [];
    if (!plainText) reasons.push('saved_file_not_frozen_plain_utf8_text');
    if (sha256 !== profileTruth.evaluator_truth.expected_content_sha256) reasons.push('saved_content_hash_mismatch');
    if (bytes.length !== profileTruth.evaluator_truth.expected_content_byte_length) reasons.push('saved_content_length_mismatch');
    return result(reasons.length ? 'not_satisfied' : 'satisfied', reasons, evidence);
  } catch {
    return result('unknown', ['independent_file_read_unavailable']);
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}
