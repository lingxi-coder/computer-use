import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmodSync, linkSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { readTextEditFileOracle } from './textedit-file-oracle.mjs';

function setup(t, taskId = 'TEXTEDIT-01') {
  const dir = mkdtempSync(join(realpathSync(tmpdir()), 'cu-textedit-oracle-'));
  chmodSync(dir, 0o700); t.after(() => rmSync(dir, { recursive: true, force: true }));
  const fileName = `cu-mvp-${taskId.toLowerCase()}.txt`;
  const contents = Buffer.from(`Unicode 保存验证 ${taskId}.\nExact second line.\n`);
  const existing = Number(taskId.slice(-2)) >= 11;
  return { dir, path: join(dir, fileName), contents,
    allowedScope: { application: 'TextEdit', output_directory: dir, file_name: fileName, document_format: 'plain_text',
      operations: ['write_requested_contents', 'save', 'close', 'reopen_and_read_back'], no_other_files: true },
    profileTruth: { suite: 'textedit', task_id: taskId, seed: 1, evaluator_truth: {
      expected_file_name: fileName, expected_format: 'plain_text',
      expected_content_sha256: createHash('sha256').update(contents).digest('hex'), expected_content_byte_length: contents.length,
      expected_ui_close_reopen_readback: true,
      expected_initial_file_state: existing ? 'existing_disposable_test_file' : 'target_file_absent',
      destructive_overwrite_confirmation: { required: existing, actual_confirmation_event_required: existing },
      evaluator_must_read_saved_file_independently: true,
    } },
  };
}

test('stable exact saved bytes satisfy only the independent file condition and remain unchanged', t => {
  const value = setup(t); writeFileSync(value.path, value.contents);
  const outcome = readTextEditFileOracle(value);
  assert.equal(outcome.verdict, 'satisfied'); assert.equal(outcome.file_evidence.stable_read, true);
  assert.deepEqual(readFileSync(value.path), value.contents);
  assert.equal(outcome.business_completion_verified, false);
  assert.equal(outcome.ui_close_reopen_status, 'not_checked');
  assert.equal(outcome.mvp_complete, false);
});

test('missing files and incorrect contents cannot qualify a saved file', t => {
  const value = setup(t); assert.equal(readTextEditFileOracle(value).verdict, 'not_satisfied');
  writeFileSync(value.path, 'Unrequested contents\n');
  assert.ok(readTextEditFileOracle(value).reason_codes.includes('saved_content_hash_mismatch'));
});

test('exact file bytes cannot prove an overwrite confirmation or a close/reopen event', t => {
  const value = setup(t, 'TEXTEDIT-11'); writeFileSync(value.path, value.contents);
  const outcome = readTextEditFileOracle(value);
  assert.equal(outcome.verdict, 'satisfied'); assert.equal(outcome.overwrite_confirmation_status, 'not_checked');
  assert.equal(outcome.business_evidence_validity, 'unknown');
});

test('symlinks, hardlinks and non-file targets remain unknown without reading unrelated bytes', t => {
  for (const kind of ['symlink', 'hardlink', 'directory']) {
    const value = setup(t); const other = join(value.dir, 'unrelated.txt'); writeFileSync(other, value.contents);
    if (kind === 'symlink') symlinkSync(other, value.path);
    else if (kind === 'hardlink') linkSync(other, value.path);
    else mkdirSync(value.path);
    assert.equal(readTextEditFileOracle(value).verdict, 'unknown');
    assert.deepEqual(readFileSync(other), value.contents);
  }
});

test('a public or symlinked output directory cannot establish an owned file scope', t => {
  const value = setup(t); writeFileSync(value.path, value.contents); chmodSync(value.dir, 0o755);
  assert.equal(readTextEditFileOracle(value).verdict, 'unknown'); chmodSync(value.dir, 0o700);
  const parent = setup(t); const linked = join(parent.dir, 'scope'); symlinkSync(value.dir, linked);
  assert.equal(readTextEditFileOracle({ ...value, allowedScope: { ...value.allowedScope, output_directory: linked } }).verdict, 'unknown');
});

test('rich text, invalid UTF-8, binary and oversized files cannot qualify', t => {
  for (const bytes of [Buffer.from('{\\rtf1\\ansi Rich text}'), Buffer.from([0xc3, 0x28]),
    Buffer.from('hello\u0000world'), Buffer.alloc(1024 * 1024 + 1, 65)]) {
    const value = setup(t); writeFileSync(value.path, bytes);
    assert.notEqual(readTextEditFileOracle(value).verdict, 'satisfied');
  }
});

test('scope and frozen identity cannot redirect the oracle to other filenames or omit independent-read requirements', t => {
  const value = setup(t);
  assert.throws(() => readTextEditFileOracle({ ...value, allowedScope: { ...value.allowedScope, file_name: '../another.txt' } }));
  assert.throws(() => readTextEditFileOracle({ ...value, profileTruth: { ...value.profileTruth, seed: 2 } }));
  const truth = structuredClone(value.profileTruth); truth.evaluator_truth.evaluator_must_read_saved_file_independently = false;
  assert.throws(() => readTextEditFileOracle({ ...value, profileTruth: truth }));
});
