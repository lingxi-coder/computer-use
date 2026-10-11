import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { chmodSync, closeSync, lstatSync, mkdirSync, mkdtempSync, openSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test, { after, before } from 'node:test';
import {
  activateOwnedDocumentForObservation, createOwnedDocumentLaunchRunnerForTesting,
  encodeOwnedDocumentRevisitLineage, encodeOwnedDocumentSourceBootstrap,
  OwnedDocumentRegistration, type OwnedDocumentLaunchRunner,
} from '../src/main/ownedDocumentLaunch.js';

const HELPER = 'cu-macos-owned-document-launcher';
const TEXTEDIT_EXEC = '/System/Applications/TextEdit.app/Contents/MacOS/TextEdit';
const TEST_HELPER = join(resolve(dirname(fileURLToPath(import.meta.url)), '../../..'), 'target', 'debug', HELPER);
let createdHelperIdentity: { dev: number; ino: number } | null = null;

before(() => {
  mkdirSync(dirname(TEST_HELPER), { recursive: true });
  let created = false;
  try {
    const fd = openSync(TEST_HELPER, 'wx', 0o700);
    try { writeFileSync(fd, '#!/bin/sh\nexit 0\n'); }
    finally { closeSync(fd); }
    chmodSync(TEST_HELPER, 0o700);
    created = true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
  }
  const stat = lstatSync(TEST_HELPER);
  assert.equal(stat.isFile(), true);
  assert.equal(stat.isSymbolicLink(), false);
  assert.notEqual(stat.mode & 0o111, 0);
  if (created) createdHelperIdentity = { dev: stat.dev, ino: stat.ino };
});

after(() => {
  if (!createdHelperIdentity) return;
  try {
    const stat = lstatSync(TEST_HELPER);
    if (stat.isFile() && !stat.isSymbolicLink() && stat.dev === createdHelperIdentity.dev && stat.ino === createdHelperIdentity.ino) {
      rmSync(TEST_HELPER);
    }
  } catch { /* A concurrent build or cleanup owns the changed path. */ }
});

class FixtureChild extends EventEmitter {
  readonly pid = 310_001;
  readonly stdout = new EventEmitter();
  readonly stderr = new EventEmitter();
  readonly stdin = {
    write: (line: string, callback?: (error?: Error | null) => void): boolean => {
      const command = JSON.parse(line) as Record<string, unknown>;
      this.handle(command);
      const error = command.op === 'launch' && this.failLaunchWriteCallback ? new Error('fixture write callback failure') : null;
      setImmediate(() => callback?.(error));
      return true;
    },
    end: (): void => { setImmediate(() => this.close()); },
  };
  readonly launcherID = '10101010-2020-4020-8020-101010101010';
  readonly launcherIdentity: Record<string, unknown>;
  targetExecutable = TEXTEDIT_EXEC;
  receiptAgeMs = 0;
  firstStopStatus: string | null = null;
  failLaunchWriteCallback = false;
  activationStatus: 'activated' | 'unknown' = 'activated';
  activationPID: number | null = null;
  activationCalls = 0;
  launchCalls = 0;
  closed = false;
  stopCalls = 0;
  quitCalls = 0;
  targetPID = this.pid + 1;
  targetStartSeconds = 0;
  targetStartMicros = 0;
  helperPath = '';
  documentPath = '';
  runID = '';
  documentSHA256 = '';
  onStop: (() => void) | null = null;
  afterLaunchReceipt: ((launchCount: number) => void) | null = null;

  constructor(helperPath: string) {
    super();
    this.helperPath = helperPath;
    this.launcherIdentity = {
      pid: this.pid, parent_pid: process.pid, start_seconds: Math.floor(Date.now() / 1000),
      start_microseconds: 0, executable_path: helperPath, parent_executable_path: process.execPath,
    };
    setImmediate(() => this.emitLine({ kind: 'ready', launcher_id: this.launcherID, launcher_identity: this.launcherIdentity }));
  }

  kill(): boolean { setImmediate(() => this.close()); return true; }

  private handle(command: Record<string, unknown>): void {
    if (command.op === 'launch') {
      this.launchCalls += 1;
      if (this.launchCalls > 1) this.targetPID += 1;
      this.runID = String(command.run_id);
      this.documentPath = String(command.document_path);
      this.documentSHA256 = String(command.document_sha256);
      const now = Date.now();
      const startedAt = Math.max(now - this.receiptAgeMs, this.lastTargetStartedAt + 1);
      this.lastTargetStartedAt = startedAt;
      this.targetStartSeconds = Math.floor(startedAt / 1000);
      this.targetStartMicros = (startedAt % 1000) * 1000;
      const targetIdentity = {
        pid: this.targetPID, parent_pid: 1, start_seconds: this.targetStartSeconds,
        start_microseconds: this.targetStartMicros, executable_path: this.targetExecutable,
        parent_executable_path: '/sbin/launchd',
      };
      const receipt = {
        version: 1, run_id: this.runID, launcher_id: this.launcherID,
        launcher_identity: this.launcherIdentity, target_identity: targetIdentity,
        bundle_identifier: 'com.apple.TextEdit', application_path: '/System/Applications/TextEdit.app',
        executable_path: TEXTEDIT_EXEC, document_path: this.documentPath,
        document_sha256: this.documentSHA256, preexisting_textedit_pids: [],
        preexisting_text_edit_applications: [], launch_start_unix_milliseconds: startedAt - 1,
        launch_finish_unix_milliseconds: startedAt, new_pid_excluded_from_preexisting_set: true,
        target_started_within_launch_interval: true, creates_new_application_instance: true,
        adds_to_recent_items: false, activates_new_instance: false,
        arguments: ['-ApplePersistenceIgnoreState', 'YES'], target_parent_is_launchd: true, no_input_authority: true,
      };
      this.emitLine({ kind: 'launch_receipt', launcher_id: this.launcherID, receipt });
      this.afterLaunchReceipt?.(this.launchCalls);
      return;
    }
    if (command.op === 'stop') {
      this.stopCalls += 1;
      this.onStop?.();
      const status = this.stopCalls === 1 && this.firstStopStatus ? this.firstStopStatus : 'terminated';
      this.emitLine({ kind: 'stop_result', launcher_id: this.launcherID, run_id: this.runID, status, target_pid: this.targetPID });
      return;
    }
    if (command.op === 'activate') {
      this.activationCalls += 1;
      this.emitLine({ kind: 'activation_result', launcher_id: this.launcherID, run_id: this.runID,
        target_pid: this.activationPID ?? this.targetPID, status: this.activationStatus });
      return;
    }
    if (command.op === 'quit') {
      this.quitCalls += 1;
      this.emitLine({ kind: 'quit_result', launcher_id: this.launcherID, status: 'terminated' });
    }
  }

  private lastTargetStartedAt = 0;
  private emitLine(value: unknown): void { this.stdout.emit('data', Buffer.from(`${JSON.stringify(value)}\n`)); }
  private close(): void { if (this.closed) return; this.closed = true; this.emit('close', 0, null); }
}

function runnerFixture(
  t: { after(callback: () => void): void },
  configure?: (child: FixtureChild) => void,
  documentWriter?: (fd: number, text: string) => void,
): {
  runner: OwnedDocumentLaunchRunner; children: FixtureChild[]; tempBase: string;
} {
  const tempBase = mkdtempSync(join(tmpdir(), 'cu-owned-document-runner-test-'));
  t.after(() => rmSync(tempBase, { recursive: true, force: true }));
  const children: FixtureChild[] = [];
  const spawnChild = (helperPath: string): FixtureChild => {
    const child = new FixtureChild(helperPath);
    configure?.(child);
    children.push(child);
    return child;
  };
  return { runner: createOwnedDocumentLaunchRunnerForTesting(spawnChild, tempBase, documentWriter), children, tempBase };
}

function savedVersionExpectation(text: string): { sha256: string; byteLength: number } {
  return {
    sha256: createHash('sha256').update(text, 'utf8').digest('hex'),
    byteLength: Buffer.byteLength(text, 'utf8'),
  };
}

test('creates exact private multiline Unicode document and cleans its same-inode edited file after confirmed stop', async (t) => {
  const f = runnerFixture(t, (child) => { child.firstStopStatus = 'still_running'; });
  const text = 'hello\t世界\r\nsecond line\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const stat = lstatSync(child.documentPath);
  assert.equal(child.documentPath, realpathSync(child.documentPath));
  assert.equal(stat.mode & 0o777, 0o600);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
  assert.deepEqual(readdirSync(dirname(child.documentPath)), ['document.txt']);

  assert.equal(await registration.stop(), 'unknown');
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
  writeFileSync(child.documentPath, 'edited in TextEdit\n', 'utf8');
  assert.equal(await registration.stop(), 'stopped');
  assert.equal(child.closed, true);
  assert.deepEqual(await f.runner.stopAll(), []);
  assert.equal(readdirSync(f.tempBase).length, 0);
});

test('encodes a bounded exact bootstrap only from the current opaque registration', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('bootstrap source\n');
  const child = f.children[0]!;
  assert.throws(() => Object.assign(registration, { pid: 999, document_path: '/forged/document.txt', unknown: 'ignored' }));
  const frame = encodeOwnedDocumentSourceBootstrap(registration);
  const length = frame.readUInt32BE(0);
  assert.equal(length, frame.byteLength - 4);
  assert.ok(length <= 8192);
  const record = JSON.parse(frame.subarray(4).toString('utf8')) as Record<string, unknown>;
  assert.deepEqual(Object.keys(record).sort(), [
    'version', 'registration_id', 'pid', 'start_seconds', 'start_micros', 'executable_path', 'bundle_id',
    'document_path', 'document_sha256', 'document_dev', 'document_ino', 'directory_dev', 'directory_ino',
    'issued_unix_ms',
  ].sort());
  assert.equal(record.version, 1);
  assert.equal(record.registration_id, child.runID);
  assert.equal(record.pid, child.targetPID);
  assert.equal(record.start_seconds, child.targetStartSeconds);
  assert.equal(record.start_micros, child.targetStartMicros);
  assert.equal(record.executable_path, TEXTEDIT_EXEC);
  assert.equal(record.bundle_id, 'com.apple.TextEdit');
  assert.equal(record.document_path, child.documentPath);
  assert.equal(record.document_sha256, child.documentSHA256);
  assert.equal(await registration.stop(), 'stopped');
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /not active|no longer eligible/);
});

test('rejects plain and prototype-forged registration objects', (t) => {
  const f = runnerFixture(t);
  const plain = { pid: 310_002, document_path: '/tmp/document.txt' } as unknown as OwnedDocumentRegistration;
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(plain), /invalid owned document registration/);
  const forged = Object.create(OwnedDocumentRegistration.prototype) as OwnedDocumentRegistration;
  Object.assign(forged, { pid: 310_002, registration_id: '10101010-2020-4020-8020-101010101010' });
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(forged), /not active/);
  assert.throws(() => OwnedDocumentRegistration.issue(
    {}, null as never, async () => 'unknown', () => false, async () => 'unknown',
    async () => { throw new Error('unreachable'); },
    () => { throw new Error('unreachable'); },
  ), /invalid owned document registration issuer/);
  assert.deepEqual(f.children, []);
});

test('rejects bootstrap issuance after the helper closes', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('helper-close\n');
  f.children[0]!.kill();
  await new Promise<void>((resolveClose) => setImmediate(resolveClose));
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /no longer eligible/);
});

test('retains one exact document for a one-use revisit ticket and issues a fresh process registration', async (t) => {
  const f = runnerFixture(t);
  const text = 'revisit stays private\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const originalFrame = encodeOwnedDocumentSourceBootstrap(registration);
  const original = JSON.parse(originalFrame.subarray(4).toString('utf8')) as Record<string, unknown>;
  const originalStat = lstatSync(child.documentPath);
  const ticket = await registration.retainForRevisit(text);

  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
  assert.deepEqual(readdirSync(dirname(child.documentPath)), ['document.txt']);
  assert.equal(child.stopCalls, 1);
  assert.equal(child.quitCalls, 0);
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /not active|no longer eligible/);

  await new Promise<void>((resolveDelay) => setTimeout(resolveDelay, 5));
  const revisited = await ticket.reopen();
  const revisitedFrame = encodeOwnedDocumentSourceBootstrap(revisited);
  const revisitedRecord = JSON.parse(revisitedFrame.subarray(4).toString('utf8')) as Record<string, unknown>;
  const revisitedStat = lstatSync(child.documentPath);
  assert.equal(child.launchCalls, 2);
  assert.equal(child.documentPath, original.document_path);
  assert.notEqual(revisitedRecord.registration_id, original.registration_id);
  assert.notEqual(revisitedRecord.pid, original.pid);
  assert.notEqual(
    `${revisitedRecord.start_seconds}:${revisitedRecord.start_micros}`,
    `${original.start_seconds}:${original.start_micros}`,
  );
  assert.equal(revisitedRecord.document_sha256, original.document_sha256);
  assert.equal(revisitedStat.dev, originalStat.dev);
  assert.equal(revisitedStat.ino, originalStat.ino);
  await assert.rejects(ticket.reopen(), /unavailable|consumed|used/i);
  assert.equal(await revisited.stop(), 'stopped');
  assert.equal(child.stopCalls, 2);
  assert.equal(child.quitCalls, 1);
  assert.equal(f.children.length, 1);
});

test('a saved atomic replacement with the expected UTF-8 value can be retained and reopened', async (t) => {
  const f = runnerFixture(t);
  const expectedText = 'saved by TextEdit: 你好 👩🏽‍💻\n';
  const registration = await f.runner.launch('unsaved initial text\n');
  const child = f.children[0]!;
  const before = lstatSync(child.documentPath);
  const replacement = join(dirname(child.documentPath), 'saved-document.tmp');
  writeFileSync(replacement, expectedText, { encoding: 'utf8', mode: 0o600 });
  renameSync(replacement, child.documentPath);
  const saved = lstatSync(child.documentPath);
  assert.notEqual(saved.ino, before.ino);
  assert.deepEqual(readdirSync(dirname(child.documentPath)), ['document.txt']);

  const ticket = await registration.retainForRevisit(expectedText);
  assert.equal(readFileSync(child.documentPath, 'utf8'), expectedText);
  const revisited = await ticket.reopen();
  const revisitedFrame = encodeOwnedDocumentSourceBootstrap(revisited);
  const record = JSON.parse(revisitedFrame.subarray(4).toString('utf8')) as Record<string, unknown>;
  assert.equal(record.document_path, child.documentPath);
  assert.equal(record.document_sha256, child.documentSHA256);
  assert.equal(lstatSync(child.documentPath).ino, saved.ino);
  assert.equal(child.launchCalls, 2);
  assert.equal(await revisited.stop(), 'stopped');
});

test('reopen rejects an oversized in-place document mutation after ticket issuance', async (t) => {
  const f = runnerFixture(t);
  const text = 'bounded text\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const ticket = await registration.retainForRevisit(text);
  const retainedInode = lstatSync(child.documentPath).ino;
  writeFileSync(child.documentPath, 'x'.repeat(2049), 'utf8');
  assert.equal(lstatSync(child.documentPath).ino, retainedInode);
  assert.equal(lstatSync(child.documentPath).size, 2049);

  await assert.rejects(ticket.reopen(), /unavailable|reopen/i);
  assert.equal(child.launchCalls, 1);
  assert.equal(readFileSync(child.documentPath, 'utf8'), 'x'.repeat(2049));
  assert.deepEqual(await f.runner.stopAll(), ['stopped']);
  assert.equal(child.closed, true);
  assert.throws(() => lstatSync(child.documentPath), { code: 'ENOENT' });
});

test('reopen rejects an identical-byte atomic inode replacement made after ticket issuance', async (t) => {
  const f = runnerFixture(t);
  const text = 'same bytes do not renew the retained inode\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const ticket = await registration.retainForRevisit(text);
  const retained = lstatSync(child.documentPath);
  const replacement = join(dirname(child.documentPath), 'post-ticket-replacement.tmp');
  writeFileSync(replacement, text, { encoding: 'utf8', mode: 0o600 });
  renameSync(replacement, child.documentPath);
  const changed = lstatSync(child.documentPath);
  assert.notEqual(changed.ino, retained.ino);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);

  await assert.rejects(ticket.reopen(), /unavailable|reopen/i);
  assert.equal(child.launchCalls, 1);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
  assert.deepEqual(await f.runner.stopAll(), ['unknown']);
  assert.equal(child.closed, true);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
});

test('concurrent retain calls create at most one ticket and stop the target once', async (t) => {
  const f = runnerFixture(t);
  const text = 'single retained target\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const results = await Promise.allSettled([
    registration.retainForRevisit(text),
    registration.retainForRevisit(text),
  ]);
  const tickets = results.flatMap((result) => result.status === 'fulfilled' ? [result.value] : []);

  assert.ok(tickets.length >= 1);
  assert.equal(new Set(tickets).size, 1);
  assert.equal(child.stopCalls, 1);
  assert.equal(child.launchCalls, 1);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
  assert.deepEqual(await f.runner.stopAll(), ['stopped']);
  assert.equal(child.stopCalls, 1);
  assert.equal(child.quitCalls, 1);
});

test('a revisit ticket is rejected when the expected final text does not match', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('original text\n');
  const child = f.children[0]!;
  await assert.rejects(registration.retainForRevisit('different text\n'));
  assert.equal(readFileSync(child.documentPath, 'utf8'), 'original text\n');
  assert.deepEqual(readdirSync(dirname(child.documentPath)), ['document.txt']);
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /no longer eligible/);
  assert.equal(child.launchCalls, 1);
  assert.equal(await f.runner.stopAll().then((results) => results.length), 1);
});

test('revisit refuses replaced directories, symlink documents and extra directory entries', async (t) => {
  const { symlinkSync } = await import('node:fs');
  for (const fault of ['directory', 'symlink', 'extra'] as const) {
    const f = runnerFixture(t);
    const text = 'expected private document\n';
    const registration = await f.runner.launch(text);
    const child = f.children[0]!;
    const directory = dirname(child.documentPath);
    let externalPath = '';
    if (fault === 'directory') {
      renameSync(directory, `${directory}-original`);
      mkdirSync(directory, { mode: 0o700 });
      writeFileSync(child.documentPath, text, { encoding: 'utf8', mode: 0o600 });
    } else if (fault === 'symlink') {
      externalPath = join(f.tempBase, 'outside.txt');
      writeFileSync(externalPath, text, { encoding: 'utf8', mode: 0o600 });
      const heldDocument = join(directory, 'held-original.txt');
      renameSync(child.documentPath, heldDocument);
      symlinkSync(externalPath, child.documentPath);
    } else {
      writeFileSync(join(directory, 'unexpected.txt'), 'keep me', { encoding: 'utf8', mode: 0o600 });
    }

    await assert.rejects(registration.retainForRevisit(text));
    if (fault === 'symlink') {
      assert.equal(lstatSync(child.documentPath).isSymbolicLink(), true);
      assert.equal(readFileSync(externalPath, 'utf8'), text);
    } else {
      assert.equal(readFileSync(child.documentPath, 'utf8'), text);
    }
    assert.equal(child.launchCalls, 1);
    child.kill();
  }
});

test('revisit refuses an unconfirmed still-running target and preserves the document', async (t) => {
  const f = runnerFixture(t, (child) => { child.firstStopStatus = 'still_running'; });
  const text = 'must remain while target is running\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  await assert.rejects(registration.retainForRevisit(text));
  assert.equal(child.stopCalls, 1);
  assert.equal(child.quitCalls, 0);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
  await assert.rejects(f.runner.launch('second launch forbidden'), /already active or unresolved/);
  assert.equal(child.launchCalls, 1);
  assert.equal(await registration.stop(), 'stopped');
});

test('stopAll cancels a pending revisit ticket and cleans the retained document', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('cancel pending revisit\n');
  const child = f.children[0]!;
  const documentPath = child.documentPath;
  const ticket = await registration.retainForRevisit('cancel pending revisit\n');
  assert.equal(readFileSync(documentPath, 'utf8'), 'cancel pending revisit\n');

  assert.deepEqual(await f.runner.stopAll(), ['stopped']);
  assert.throws(() => lstatSync(documentPath), { code: 'ENOENT' });
  assert.equal(child.closed, true);
  await assert.rejects(ticket.reopen(), /unavailable|cancel|consumed|used/i);
  assert.equal(child.launchCalls, 1);
  assert.equal(child.quitCalls, 1);
});

test('overlapping reopen and stopAll produce at most one fresh launch and consume cancellation', async (t) => {
  const f = runnerFixture(t);
  const text = 'overlap without duplicate launch\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const ticket = await registration.retainForRevisit(text);
  const reopen = ticket.reopen();
  const stopAll = f.runner.stopAll();
  const [reopenResult, stopResults] = await Promise.allSettled([reopen, stopAll]);

  assert.equal(child.launchCalls <= 2, true);
  assert.equal(f.children.length, 1);
  assert.equal(child.quitCalls <= 1, true);
  if (reopenResult.status === 'fulfilled') {
    assert.ok(['stopped', 'unknown'].includes((stopResults as PromiseFulfilledResult<readonly ('stopped' | 'unknown')[]>).value[0] ?? ''));
    assert.throws(() => encodeOwnedDocumentSourceBootstrap(reopenResult.value), /not active|no longer eligible/);
  } else {
    assert.match(String(reopenResult.reason), /unavailable|cancel|consumed|used/i);
  }
  if (stopResults.status === 'fulfilled') assert.equal(stopResults.value.length, 1);
});

test('activates only the registered owned instance and coalesces concurrent requests', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('activate for observation\n');
  const child = f.children[0]!;
  const first = activateOwnedDocumentForObservation(registration);
  const second = activateOwnedDocumentForObservation(registration);
  assert.equal(await first, true);
  assert.equal(await second, true);
  assert.equal(child.activationCalls, 1);
  assert.equal(await registration.stop(), 'stopped');
  assert.equal(await activateOwnedDocumentForObservation(registration), false);
});

test('unknown activation response or mismatched target PID never confirms activation', async (t) => {
  const f = runnerFixture(t, (child) => { child.activationPID = child.targetPID + 1; });
  const registration = await f.runner.launch('reject foreign activation\n');
  const child = f.children[0]!;
  assert.equal(await activateOwnedDocumentForObservation(registration), false);
  assert.equal(child.activationCalls, 1);
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /no longer eligible/);
  assert.equal(await registration.stop(), 'stopped');
  const unknownRegistration = await f.runner.launch('unknown activation\n');
  const unknownChild = f.children[1]!;
  unknownChild.activationPID = unknownChild.targetPID;
  unknownChild.activationStatus = 'unknown';
  assert.equal(await activateOwnedDocumentForObservation(unknownRegistration), false);
  assert.equal(unknownChild.activationCalls, 1);
  assert.equal(await unknownRegistration.stop(), 'stopped');
});

test('rejects bootstrap issuance once stop is pending', async (t) => {
  const f = runnerFixture(t, (child) => { child.firstStopStatus = 'still_running'; });
  const registration = await f.runner.launch('stop-pending\n');
  const pendingStop = registration.stop();
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /no longer eligible/);
  assert.equal(await pendingStop, 'unknown');
});

test('rejects bootstrap issuance after document bytes change even when its inode remains', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('original bytes\n');
  const child = f.children[0]!;
  writeFileSync(child.documentPath, 'changed bytes\n', 'utf8');
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /no longer eligible/);
  assert.equal(await registration.stop(), 'stopped');
});

test('rejects bootstrap issuance after the document inode is replaced', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('original inode\n');
  const child = f.children[0]!;
  const replacement = join(dirname(child.documentPath), 'replacement.txt');
  const original = join(dirname(child.documentPath), 'original.txt');
  writeFileSync(replacement, 'replacement inode\n', { encoding: 'utf8', mode: 0o600 });
  renameSync(child.documentPath, original);
  renameSync(replacement, child.documentPath);
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /no longer eligible/);
  assert.equal(await registration.stop(), 'unknown');
});

test('rejects NUL and oversized UTF-8 text before spawning a helper', async (t) => {
  const f = runnerFixture(t);
  await assert.rejects(f.runner.launch('before\0after'), /NUL-free/);
  await assert.rejects(f.runner.launch('🙂'.repeat(1025)), /4096 UTF-8 bytes/);
  assert.equal(f.children.length, 0);
  assert.equal(readdirSync(f.tempBase).length, 0);
});

test('a write failure after open removes only the exact partial private inode', async (t) => {
  const f = runnerFixture(t, undefined, (fd) => {
    writeFileSync(fd, 'partial document bytes');
    throw new Error('simulated filesystem write failure');
  });
  await assert.rejects(f.runner.launch('requested full document'));
  assert.equal(f.children.length, 0);
  assert.deepEqual(readdirSync(f.tempBase), []);
  assert.deepEqual(await f.runner.stopAll(), []);
});

test('creation rollback preserves a replaced directory and blocks further launches', async (t) => {
  let replacedDirectory = '';
  const f = runnerFixture(t, undefined, (fd) => {
    writeFileSync(fd, 'original partial bytes');
    const [name] = readdirSync(f.tempBase);
    assert.ok(name);
    replacedDirectory = join(f.tempBase, name);
    renameSync(replacedDirectory, join(f.tempBase, 'original-directory'));
    mkdirSync(replacedDirectory, { mode: 0o700 });
    writeFileSync(join(replacedDirectory, 'document.txt'), 'replacement must survive', { mode: 0o600 });
    throw new Error('creation failed after directory replacement');
  });
  await assert.rejects(f.runner.launch('requested document'));
  assert.equal(f.children.length, 0);
  assert.equal(readFileSync(join(replacedDirectory, 'document.txt'), 'utf8'), 'replacement must survive');
  assert.equal(readFileSync(join(f.tempBase, 'original-directory/document.txt'), 'utf8'), 'original partial bytes');
  assert.deepEqual(await f.runner.stopAll(), ['unknown']);
  await assert.rejects(f.runner.launch('another document'), /already active or unresolved/);
  assert.equal(readFileSync(join(replacedDirectory, 'document.txt'), 'utf8'), 'replacement must survive');
});

test('wrong target identity leaves the document and active entry unknown; no second launch is sent', async (t) => {
  const f = runnerFixture(t, (child) => { child.targetExecutable = '/tmp/foreign'; });
  await assert.rejects(f.runner.launch('exact bytes\n'));
  const child = f.children[0]!;
  assert.equal(child.stopCalls, 1);
  assert.equal(readFileSync(child.documentPath, 'utf8'), 'exact bytes\n');
  await assert.rejects(f.runner.launch('must not launch again'), /already active or unresolved/);
  assert.equal(f.children.length, 1);
  assert.equal((await f.runner.stopAll()).length, 1);
  child.kill();
});

test('a stale helper launch receipt is rejected and its unverified document remains unknown', async (t) => {
  const f = runnerFixture(t, (child) => { child.receiptAgeMs = 30_000; });
  await assert.rejects(f.runner.launch('stale receipt bytes\n'));
  const child = f.children[0]!;
  assert.equal(child.stopCalls, 1);
  assert.equal(readFileSync(child.documentPath, 'utf8'), 'stale receipt bytes\n');
  await assert.rejects(f.runner.launch('never resend'), /already active or unresolved/);
  assert.equal(f.children.length, 1);
  child.kill();
});

test('a delivered launch command with a failed write callback stays unknown and is never resent', async (t) => {
  const f = runnerFixture(t, (child) => { child.failLaunchWriteCallback = true; });
  await assert.rejects(f.runner.launch('launch may have started'));
  const child = f.children[0]!;
  assert.equal(child.stopCalls, 1);
  assert.equal(readFileSync(child.documentPath, 'utf8'), 'launch may have started');
  await assert.rejects(f.runner.launch('second launch forbidden'), /already active or unresolved/);
  assert.equal(f.children.length, 1);
  child.kill();
});

test('atomic replacement after target exit is retained as unknown instead of deleting an unregistered inode', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('before save\n');
  const child = f.children[0]!;
  const original = child.documentPath;
  const replacement = join(dirname(original), 'replacement.txt');
  writeFileSync(replacement, 'after atomic save\n', { encoding: 'utf8', mode: 0o600 });
  const replacementDocument = join(dirname(original), 'hold-original.txt');
  const { renameSync } = await import('node:fs');
  renameSync(original, replacementDocument);
  renameSync(replacement, original);
  assert.equal(await registration.stop(), 'unknown');
  assert.equal(readFileSync(original, 'utf8'), 'after atomic save\n');
  assert.equal(readFileSync(replacementDocument, 'utf8'), 'before save\n');
  child.kill();
});

test('explicit diagnostic cleanup removes an exact saved replacement after confirmed child exit', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('before save\n');
  const child = f.children[0]!;
  const original = child.documentPath;
  const replacement = join(dirname(original), 'replacement.txt');
  const finalText = '中文 👩🏽‍💻 Café\n';
  writeFileSync(replacement, finalText, { mode: 0o600 });
  renameSync(replacement, original);
  assert.equal(await registration.stopDiagnostic(finalText), 'stopped');
  assert.equal(child.closed, true);
  assert.throws(() => lstatSync(dirname(original)), { code: 'ENOENT' });
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /no longer eligible/);
  assert.equal(await registration.stopDiagnostic(finalText), 'stopped');
  assert.equal(await registration.stopDiagnostic('different value'), 'unknown');
  assert.equal(child.stopCalls, 1);
});

test('saved byte readback before close is bounded and never renews a replaced registration', async t => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('initial\n');
  const child = f.children[0]!;
  assert.equal(registration.savedContentsMatch('desired\n'), false);
  const replacement = join(dirname(child.documentPath), 'replacement.txt');
  writeFileSync(replacement, 'desired\n', { mode: 0o600 });
  renameSync(replacement, child.documentPath);
  assert.equal(registration.savedContentsMatch('desired\n'), true);
  assert.equal(registration.savedContentsMatch('desired\n\0'), false);
  assert.equal(registration.savedContentsMatch('x'.repeat(2049)), false);
  assert.throws(() => encodeOwnedDocumentSourceBootstrap(registration), /no longer eligible/);
  assert.equal(child.stopCalls, 0);
  await registration.stopDiagnostic('desired\n');
  assert.equal(registration.savedContentsMatch('desired\n'), false);
});

test('captured saved version accepts an atomic Save replacement and exposes only verified file lineage', async t => {
  const f = runnerFixture(t);
  const expectedText = 'saved after atomic replacement: 世界\n';
  const registration = await f.runner.launch('unsaved before Save\n');
  const child = f.children[0]!;
  const replacement = join(dirname(child.documentPath), 'saved-version.tmp');
  writeFileSync(replacement, expectedText, { encoding: 'utf8', mode: 0o600 });
  renameSync(replacement, child.documentPath);
  const savedStat = lstatSync(child.documentPath, { bigint: true });
  const captured = registration.captureSavedVersion(savedVersionExpectation(expectedText));
  assert.deepEqual(Object.keys(captured), []);

  const ticket = await captured.retainForRevisit();
  assert.equal(child.stopCalls, 1);
  assert.deepEqual(readdirSync(dirname(child.documentPath)), ['document.txt']);
  const reopened = await ticket.reopen();
  const lineage = encodeOwnedDocumentRevisitLineage(ticket, reopened);
  assert.deepEqual(Object.keys(lineage).sort(), [
    'after_close', 'after_reopen', 'before_close', 'directory_dev', 'directory_ino',
  ]);
  assert.deepEqual(lineage.before_close, lineage.after_close);
  assert.deepEqual(lineage.before_close, lineage.after_reopen);
  assert.equal(lineage.before_close.dev, Number(savedStat.dev));
  assert.equal(lineage.before_close.ino, Number(savedStat.ino));
  assert.equal(lineage.before_close.size, Buffer.byteLength(expectedText, 'utf8'));
  assert.equal(lineage.before_close.sha256, savedVersionExpectation(expectedText).sha256);
  assert.match(lineage.before_close.mtime_ns, /^\d+$/);
  assert.match(lineage.before_close.ctime_ns, /^\d+$/);
  assert.ok(Object.isFrozen(lineage));
  assert.ok(Object.isFrozen(lineage.before_close));
  const encoded = JSON.stringify(lineage);
  assert.equal(encoded.includes(child.documentPath), false);
  assert.equal(encoded.includes(expectedText), false);
  assert.throws(() => encodeOwnedDocumentRevisitLineage(ticket, reopened), /unavailable|changed/i);
  await assert.rejects(captured.retainForRevisit(), /consumed|unavailable/i);
  await assert.rejects(ticket.reopen(), /unavailable|consumed|used/i);
  assert.equal(await reopened.stop(), 'stopped');
});

test('saved-version capture rejects bad expectations and stale registrations without exposing file details', async t => {
  const f = runnerFixture(t);
  const text = 'captured only while active\n';
  const registration = await f.runner.launch(text);
  assert.throws(() => registration.captureSavedVersion({ sha256: 'bad', byteLength: text.length }), /saved_version_unavailable/);
  assert.throws(() => registration.captureSavedVersion({
    sha256: savedVersionExpectation(text).sha256, byteLength: Buffer.byteLength(text, 'utf8') + 1,
  }), /saved_version_unavailable/);
  const captured = registration.captureSavedVersion(savedVersionExpectation(text));
  assert.equal(await registration.stop(), 'stopped');
  assert.throws(() => registration.captureSavedVersion(savedVersionExpectation(text)), /saved_version_unavailable/);
  await assert.rejects(captured.retainForRevisit(), /saved_version_unavailable/);
  assert.equal(f.children[0]!.launchCalls, 1);
});

test('saved-version retention rejects same-content inode replacement before stop', async t => {
  const f = runnerFixture(t);
  const text = 'same bytes, different inode\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const captured = registration.captureSavedVersion(savedVersionExpectation(text));
  const replacement = join(dirname(child.documentPath), 'same-bytes-new-inode.tmp');
  writeFileSync(replacement, text, { encoding: 'utf8', mode: 0o600 });
  renameSync(replacement, child.documentPath);

  await assert.rejects(captured.retainForRevisit(), /saved_version_changed/);
  assert.equal(child.stopCalls, 0);
  assert.equal(child.launchCalls, 1);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
  assert.deepEqual(await f.runner.stopAll(), ['unknown']);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
});

test('saved-version retention detects same-inode same-length rewrite restored to original bytes', async t => {
  const f = runnerFixture(t);
  const text = 'stable content\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const captured = registration.captureSavedVersion(savedVersionExpectation(text));
  const originalInode = lstatSync(child.documentPath).ino;
  const replacement = 'broken content\n';
  assert.equal(Buffer.byteLength(replacement), Buffer.byteLength(text));
  writeFileSync(child.documentPath, replacement, 'utf8');
  await new Promise<void>((resolveDelay) => setTimeout(resolveDelay, 5));
  writeFileSync(child.documentPath, text, 'utf8');
  assert.equal(lstatSync(child.documentPath).ino, originalInode);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);

  await assert.rejects(captured.retainForRevisit(), /saved_version_changed/);
  assert.equal(child.stopCalls, 0);
  assert.deepEqual(await f.runner.stopAll(), ['stopped']);
});

test('saved-version retention rejects an autosave that replaces the file during target close', async t => {
  const f = runnerFixture(t);
  const text = 'save completed bytes\n';
  const registration = await f.runner.launch('before Save\n');
  const child = f.children[0]!;
  const saved = join(dirname(child.documentPath), 'saved-before-close.tmp');
  writeFileSync(saved, text, { encoding: 'utf8', mode: 0o600 });
  renameSync(saved, child.documentPath);
  const captured = registration.captureSavedVersion(savedVersionExpectation(text));
  child.onStop = () => {
    const replacement = join(dirname(child.documentPath), 'autosave-during-close.tmp');
    writeFileSync(replacement, text, { encoding: 'utf8', mode: 0o600 });
    renameSync(replacement, child.documentPath);
  };

  await assert.rejects(captured.retainForRevisit(), /saved_version_changed/);
  assert.equal(child.stopCalls, 1);
  assert.equal(child.launchCalls, 1);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
  assert.deepEqual(await f.runner.stopAll(), ['unknown']);
  assert.equal(child.closed, true);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
});

test('reopen rejects same-inode content restored during launch before issuing a registration', async t => {
  const f = runnerFixture(t);
  const text = 'immutable saved version\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const captured = registration.captureSavedVersion(savedVersionExpectation(text));
  const ticket = await captured.retainForRevisit();
  child.afterLaunchReceipt = (launchCount) => {
    if (launchCount !== 2) return;
    writeFileSync(child.documentPath, 'rewritten saved version\n', 'utf8');
    writeFileSync(child.documentPath, text, 'utf8');
  };

  await assert.rejects(ticket.reopen(), /reopen_file_changed/);
  assert.equal(child.launchCalls, 2);
  assert.equal(readFileSync(child.documentPath, 'utf8'), text);
  assert.deepEqual(await f.runner.stopAll(), ['stopped']);
  assert.equal(child.closed, true);
});

test('lineage serialization rechecks the file after reopening and remains one-use', async t => {
  const f = runnerFixture(t);
  const text = 'recheck before private completion\n';
  const registration = await f.runner.launch(text);
  const child = f.children[0]!;
  const captured = registration.captureSavedVersion(savedVersionExpectation(text));
  const ticket = await captured.retainForRevisit();
  const reopened = await ticket.reopen();
  writeFileSync(child.documentPath, 'temporary rewritten content\n', 'utf8');
  await new Promise<void>((resolveDelay) => setTimeout(resolveDelay, 5));
  writeFileSync(child.documentPath, text, 'utf8');

  assert.throws(() => encodeOwnedDocumentRevisitLineage(ticket, reopened), /reopen_file_changed/);
  assert.throws(() => encodeOwnedDocumentRevisitLineage(ticket, reopened), /unavailable|changed/i);
  assert.deepEqual(await f.runner.stopAll(), ['stopped']);
});

test('saved byte readback rejects symlinks, exposed modes, extra files and replaced directories', async t => {
  const { symlinkSync, unlinkSync } = await import('node:fs');
  for (const fault of ['symlink', 'mode', 'extra', 'directory', 'oversized'] as const) {
    const f = runnerFixture(t);
    const registration = await f.runner.launch('desired\n');
    const child = f.children[0]!;
    const directory = dirname(child.documentPath);
    if (fault === 'symlink') {
      const target = join(f.tempBase, 'external.txt');
      writeFileSync(target, 'desired\n', { mode: 0o600 });
      unlinkSync(child.documentPath); symlinkSync(target, child.documentPath);
    } else if (fault === 'mode') chmodSync(child.documentPath, 0o644);
    else if (fault === 'extra') writeFileSync(join(directory, 'extra.txt'), 'preserve', { mode: 0o600 });
    else if (fault === 'oversized') writeFileSync(child.documentPath, 'x'.repeat(2049), { mode: 0o600 });
    else {
      renameSync(directory, directory + '-original');
      mkdirSync(directory, { mode: 0o700 });
      writeFileSync(child.documentPath, 'desired\n', { mode: 0o600 });
    }
    assert.equal(registration.savedContentsMatch('desired\n'), false);
    assert.equal(child.stopCalls, 0);
    child.kill();
  }
});

test('diagnostic cleanup may retry a default cleanup failure with proven child closure', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('initial\n');
  const child = f.children[0]!;
  const replacement = join(dirname(child.documentPath), 'replacement.txt');
  writeFileSync(replacement, 'saved\n', { mode: 0o600 });
  renameSync(replacement, child.documentPath);
  assert.equal(await registration.stop(), 'unknown');
  assert.equal(child.closed, true);
  assert.equal(await registration.stopDiagnostic('saved\n'), 'stopped');
  assert.equal(child.stopCalls, 1);
});

test('diagnostic cleanup rejects mismatched replacement bytes and cannot switch its expected value', async (t) => {
  const f = runnerFixture(t);
  const registration = await f.runner.launch('initial\n');
  const child = f.children[0]!;
  const replacement = join(dirname(child.documentPath), 'replacement.txt');
  writeFileSync(replacement, 'unexpected\n', { mode: 0o600 });
  renameSync(replacement, child.documentPath);
  assert.equal(await registration.stopDiagnostic('expected\n'), 'unknown');
  assert.equal(readFileSync(child.documentPath, 'utf8'), 'unexpected\n');
  assert.equal(await registration.stopDiagnostic('unexpected\n'), 'unknown');
  assert.equal(readFileSync(child.documentPath, 'utf8'), 'unexpected\n');
});

test('diagnostic cleanup retains a replaced directory and unrelated extra files', async (t) => {
  for (const fault of ['directory', 'extra'] as const) {
    const f = runnerFixture(t);
    const registration = await f.runner.launch('initial\n');
    const child = f.children[0]!;
    const directory = dirname(child.documentPath);
    if (fault === 'directory') {
      renameSync(directory, directory + '-original');
      mkdirSync(directory, { mode: 0o700 });
      writeFileSync(child.documentPath, 'expected\n', { mode: 0o600 });
    } else {
      const replacement = join(directory, 'replacement.txt');
      writeFileSync(replacement, 'expected\n', { mode: 0o600 });
      renameSync(replacement, child.documentPath);
      writeFileSync(join(directory, 'unrelated.txt'), 'preserve', { mode: 0o600 });
    }
    assert.equal(await registration.stopDiagnostic('expected\n'), 'unknown');
    assert.equal(readFileSync(child.documentPath, 'utf8'), 'expected\n');
  }
});

test('diagnostic cleanup rejects symlinks, exposed modes and unconfirmed target exit', async (t) => {
  const { symlinkSync, unlinkSync } = await import('node:fs');
  for (const fault of ['symlink', 'mode', 'still_running'] as const) {
    const f = runnerFixture(t, child => { if (fault === 'still_running') child.firstStopStatus = 'still_running'; });
    const registration = await f.runner.launch('initial\n');
    const child = f.children[0]!;
    const directory = dirname(child.documentPath);
    const replacement = join(directory, 'replacement.txt');
    writeFileSync(replacement, 'expected\n', { mode: 0o600 });
    renameSync(replacement, child.documentPath);
    if (fault === 'symlink') {
      const target = join(f.tempBase, 'external.txt');
      writeFileSync(target, 'expected\n', { mode: 0o600 });
      unlinkSync(child.documentPath); symlinkSync(target, child.documentPath);
    }
    if (fault === 'mode') chmodSync(child.documentPath, 0o644);
    assert.equal(await registration.stopDiagnostic('expected\n'), 'unknown');
    assert.equal(readFileSync(child.documentPath, 'utf8'), 'expected\n');
    child.kill();
  }
});
