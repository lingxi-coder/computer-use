import { spawn as nodeSpawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { EventEmitter } from 'node:events';
import {
  chmodSync, closeSync, constants, fchmodSync, fstatSync, fsyncSync, lstatSync, mkdtempSync, openSync, readFileSync,
  readdirSync, readSync, realpathSync, rmdirSync, unlinkSync, writeFileSync,
  type BigIntStats, type Stats,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { encodeOwnedDocumentSourceBootstrapRecord, type OwnedDocumentSourceBootstrap } from './ownedDocumentBootstrap.js';

const HELPER_NAME = 'cu-macos-owned-document-launcher';
const TEXTEDIT_BUNDLE = 'com.apple.TextEdit';
const TEXTEDIT_APP = '/System/Applications/TextEdit.app';
const TEXTEDIT_EXEC = '/System/Applications/TextEdit.app/Contents/MacOS/TextEdit';
const DOCUMENT_NAME = 'document.txt';
const MAX_DOCUMENT_BYTES = 4096;
const MAX_LINE_BYTES = 64 * 1024;
const MAX_STDOUT_BYTES = 128 * 1024;
const MAX_QUEUED_MESSAGES = 8;
const MAX_STDERR_BYTES = 8 * 1024;
const RECEIPT_KEYS = [
  'version', 'run_id', 'launcher_id', 'launcher_identity', 'target_identity', 'bundle_identifier',
  'application_path', 'executable_path', 'document_path', 'document_sha256', 'preexisting_textedit_pids',
  'preexisting_text_edit_applications', 'launch_start_unix_milliseconds', 'launch_finish_unix_milliseconds',
  'new_pid_excluded_from_preexisting_set', 'target_started_within_launch_interval',
  'creates_new_application_instance', 'adds_to_recent_items', 'activates_new_instance', 'arguments',
  'target_parent_is_launchd', 'no_input_authority',
].sort();
const IDENTITY_KEYS = ['pid', 'parent_pid', 'start_seconds', 'start_microseconds', 'executable_path', 'parent_executable_path'].sort();
const REGISTRATION_ISSUER = Object.freeze({});

type JsonObject = Record<string, unknown>;
type KernelIdentity = {
  pid: number; parent_pid: number; start_seconds: number; start_microseconds: number;
  executable_path: string; parent_executable_path: string | null;
};
type LaunchReceipt = {
  version: number; run_id: string; launcher_id: string; launcher_identity: KernelIdentity;
  target_identity: KernelIdentity; bundle_identifier: string; application_path: string; executable_path: string;
  document_path: string; document_sha256: string; preexisting_textedit_pids: number[];
  preexisting_text_edit_applications: unknown[]; launch_start_unix_milliseconds: number;
  launch_finish_unix_milliseconds: number; new_pid_excluded_from_preexisting_set: boolean;
  target_started_within_launch_interval: boolean; creates_new_application_instance: boolean;
  adds_to_recent_items: boolean; activates_new_instance: boolean; arguments: string[];
  target_parent_is_launchd: boolean; no_input_authority: boolean;
};

interface WritableLike { write(data: string, callback?: (error?: Error | null) => void): boolean; end(): void }
interface ReadableLike extends EventEmitter {}
interface ChildLike extends EventEmitter {
  pid?: number;
  stdin?: WritableLike;
  stdout?: ReadableLike;
  stderr?: ReadableLike;
  kill(signal?: NodeJS.Signals): boolean;
}
type SpawnLike = (file: string, args: readonly string[], options: {
  cwd: string; env: NodeJS.ProcessEnv; stdio: ['pipe', 'pipe', 'pipe'];
}) => ChildLike;

interface FileIdentity {
  directory: string; documentPath: string; directoryDev: number; directoryIno: number;
  fileDev: number; fileIno: number; size: number; mtimeNs: string; ctimeNs: string; sha256: string;
}
export interface SavedVersionExpectation { sha256: string; byteLength: number }
export interface SavedFileVersionFacts {
  dev: number; ino: number; size: number; sha256: string; mtime_ns: string; ctime_ns: string;
}
export interface OwnedDocumentRevisitFileLineage {
  directory_dev: number;
  directory_ino: number;
  before_close: SavedFileVersionFacts;
  after_close: SavedFileVersionFacts;
  after_reopen: SavedFileVersionFacts;
}
interface PartialDirectoryIdentity { directory: string; directoryDev: number | null; directoryIno: number | null; owner: number; fileDev: number | null; fileIno: number | null }
type DocumentWriter = (fd: number, text: string) => void;
interface ActiveLaunch {
  readonly runId: string;
  readonly child: ChildLike;
  readonly channel: ChildChannel;
  readonly file: FileIdentity;
  readonly helperPath: string;
  launcherId: string;
  readonly receipt: LaunchReceipt | null;
  phase: 'starting' | 'launch_sent' | 'unknown' | 'registered' | 'stopping' | 'retaining' | 'retained';
  stopPromise: Promise<'stopped' | 'unknown'> | null;
  activationPromise: Promise<boolean> | null;
  launcherClosed: boolean;
  diagnosticCleanupDigest: string | null;
  diagnosticCleanupComplete: boolean;
  targetClosed: boolean;
}

interface RegistrationBinding {
  readonly entry: ActiveLaunch;
  readonly stop: () => Promise<'stopped' | 'unknown'>;
  readonly isCurrent: () => boolean;
  readonly diagnosticStop: (expectedText: string) => Promise<'stopped' | 'unknown'>;
  readonly retain: (expectedText: string) => Promise<OwnedDocumentRevisitTicket>;
  readonly captureSavedVersion: (expected: SavedVersionExpectation) => OwnedDocumentSavedVersion;
}

const activeRegistrations = new WeakMap<OwnedDocumentRegistration, RegistrationBinding>();
const closedChildren = new WeakSet<ChildLike>();
interface SavedVersionLineageState {
  readonly beforeClose: FileIdentity;
  afterClose: FileIdentity | null;
  afterReopen: FileIdentity | null;
  reopenedRegistration: OwnedDocumentRegistration | null;
  emitted: boolean;
}
interface RevisitTicketBinding {
  consumed: boolean;
  reopen: () => Promise<OwnedDocumentRegistration>;
  lineage?: SavedVersionLineageState;
}
const revisitTickets = new WeakMap<OwnedDocumentRevisitTicket, RevisitTicketBinding>();
const savedVersionTokens = new WeakMap<OwnedDocumentSavedVersion, {
  consumed: boolean; retain: () => Promise<OwnedDocumentRevisitTicket>;
}>();

export class OwnedDocumentLifecycleError extends Error {
  constructor(readonly code: 'reopen_unavailable' | 'reopen_file_changed' | 'reopen_not_launched'
    | 'reopen_preexisting_identity_unavailable' | 'reopen_invalid_file' | 'reopen_receipt_unavailable'
    | 'reopen_identity_reused' | 'reopen_unknown' | 'saved_version_unavailable'
    | 'saved_version_changed' | 'saved_version_consumed') {
    super('owned document lifecycle failed: ' + code);
    this.name = 'OwnedDocumentLifecycleError';
  }
}

/** Main-only file continuity. It is neither execution nor Root revisit evidence. */
export class OwnedDocumentRevisitTicket {
  private constructor() {}

  static issue(
    issuer: object,
    reopen: () => Promise<OwnedDocumentRegistration>,
    lineage?: SavedVersionLineageState,
  ): OwnedDocumentRevisitTicket {
    if (issuer !== REGISTRATION_ISSUER) throw new Error('invalid owned revisit issuer');
    const ticket = Object.freeze(new OwnedDocumentRevisitTicket());
    revisitTickets.set(ticket, { consumed: false, reopen, lineage });
    return ticket;
  }

  reopen(): Promise<OwnedDocumentRegistration> {
    const binding = revisitTickets.get(this);
    if (!binding || binding.consumed) return Promise.reject(new Error('owned revisit ticket is unavailable'));
    binding.consumed = true;
    return binding.reopen();
  }
}

/** Opaque Main-only capture of one saved file version. It carries no properties. */
export class OwnedDocumentSavedVersion {
  private constructor() {}

  static issue(issuer: object, retain: () => Promise<OwnedDocumentRevisitTicket>): OwnedDocumentSavedVersion {
    if (issuer !== REGISTRATION_ISSUER) throw new Error('invalid saved version issuer');
    const token = Object.freeze(new OwnedDocumentSavedVersion());
    savedVersionTokens.set(token, { consumed: false, retain });
    return token;
  }

  retainForRevisit(): Promise<OwnedDocumentRevisitTicket> {
    const binding = savedVersionTokens.get(this);
    if (!binding || binding.consumed) {
      return Promise.reject(new OwnedDocumentLifecycleError('saved_version_consumed'));
    }
    binding.consumed = true;
    return binding.retain();
  }
}

/** Returns only the private file-version facts after the exact Main-issued ticket completes. */
export function encodeOwnedDocumentRevisitLineage(
  ticket: OwnedDocumentRevisitTicket,
  reopenedRegistration: OwnedDocumentRegistration,
): OwnedDocumentRevisitFileLineage {
  const binding = revisitTickets.get(ticket);
  const lineage = binding?.lineage;
  if (!binding || !lineage || !binding.consumed || lineage.emitted) {
    throw new OwnedDocumentLifecycleError('reopen_unavailable');
  }
  lineage.emitted = true;
  const registrationBinding = activeRegistrations.get(reopenedRegistration);
  const reopenedEntry = registrationBinding?.entry;
  if (lineage.reopenedRegistration !== reopenedRegistration || !lineage.afterClose || !lineage.afterReopen
    || !registrationBinding?.isCurrent() || !reopenedEntry || reopenedEntry.phase !== 'registered' || !reopenedEntry.receipt
    || reopenedEntry.stopPromise !== null || reopenedEntry.activationPromise !== null
    || closedChildren.has(reopenedEntry.child) || !reopenedEntry.child.stdin
    || !sameFileVersion(lineage.beforeClose, lineage.afterClose)
    || !sameFileVersion(lineage.beforeClose, lineage.afterReopen)) {
    throw new OwnedDocumentLifecycleError('reopen_file_changed');
  }
  const finalVersion = inspectRetainedDocument(reopenedEntry.file, lineage.beforeClose.sha256, true);
  if (!finalVersion || !sameFileVersion(lineage.beforeClose, finalVersion)) {
    throw new OwnedDocumentLifecycleError('reopen_file_changed');
  }
  lineage.afterReopen = finalVersion;
  return Object.freeze({
    directory_dev: lineage.beforeClose.directoryDev,
    directory_ino: lineage.beforeClose.directoryIno,
    before_close: savedFileVersionFacts(lineage.beforeClose),
    after_close: savedFileVersionFacts(lineage.afterClose),
    after_reopen: savedFileVersionFacts(lineage.afterReopen),
  });
}

/** Opaque Main-only ownership token. It intentionally has no identity or IPC projection. */
export class OwnedDocumentRegistration {
  private constructor() {}

  static issue(
    issuer: object,
    entry: ActiveLaunch,
    stop: () => Promise<'stopped' | 'unknown'>,
    isCurrent: () => boolean,
    diagnosticStop: (expectedText: string) => Promise<'stopped' | 'unknown'>,
    retain: (expectedText: string) => Promise<OwnedDocumentRevisitTicket>,
    captureSavedVersion: (expected: SavedVersionExpectation) => OwnedDocumentSavedVersion,
  ): OwnedDocumentRegistration {
    if (issuer !== REGISTRATION_ISSUER) throw new Error('invalid owned document registration issuer');
    const value = new OwnedDocumentRegistration();
    Object.freeze(value);
    activeRegistrations.set(value, { entry, stop, isCurrent, diagnosticStop, retain, captureSavedVersion });
    return value;
  }

  stop(): Promise<'stopped' | 'unknown'> {
    const binding = activeRegistrations.get(this);
    if (!binding) return Promise.resolve('unknown');
    return binding.stop();
  }

  /** Explicit disposal of this temporary diagnostic, never registration renewal. */
  stopDiagnostic(expectedText: string): Promise<'stopped' | 'unknown'> {
    const binding = activeRegistrations.get(this);
    return binding ? binding.diagnosticStop(expectedText) : Promise.resolve('unknown');
  }

  /** Stop only this owned process, preserving an exactly matched file for a new registration. */
  retainForRevisit(expectedText: string): Promise<OwnedDocumentRevisitTicket> {
    const binding = activeRegistrations.get(this);
    return binding ? binding.retain(expectedText) : Promise.reject(new Error('owned document registration is unavailable'));
  }

  /** Captures a bounded current file version; this is Main-only continuity data, not a grant. */
  captureSavedVersion(expected: SavedVersionExpectation): OwnedDocumentSavedVersion {
    const binding = activeRegistrations.get(this);
    if (!binding) throw new OwnedDocumentLifecycleError('saved_version_unavailable');
    return binding.captureSavedVersion(expected);
  }

  /** Read current bytes before closing. This neither renews input nor proves Save causality. */
  savedContentsMatch(expectedText: string): boolean {
    const binding = activeRegistrations.get(this);
    if (!binding || !binding.isCurrent() || binding.entry.phase !== 'registered'
      || binding.entry.stopPromise !== null || binding.entry.targetClosed
      || closedChildren.has(binding.entry.child)
      || typeof expectedText !== 'string' || expectedText.includes('\0')
      || Buffer.byteLength(expectedText, 'utf8') > 2048) return false;
    const digest = createHash('sha256').update(expectedText, 'utf8').digest('hex');
    // A TextEdit atomic save may replace the inode, but may never replace our
    // original private directory, expose a file or add unrelated files.
    return inspectRetainedDocument(binding.entry.file, digest, false) !== null;
  }
}

/** Main-only serialization of a currently registered owned document. The token carries no fields. */
export function encodeOwnedDocumentSourceBootstrap(registration: OwnedDocumentRegistration): Buffer {
  if (!registration || typeof registration !== 'object' || !(registration instanceof OwnedDocumentRegistration)) {
    throw new Error('invalid owned document registration');
  }
  const binding = activeRegistrations.get(registration);
  if (!binding) throw new Error('owned document registration is not active');
  const { entry } = binding;
  if (!binding.isCurrent() || entry.phase !== 'registered' || entry.stopPromise !== null
    || entry.activationPromise !== null || closedChildren.has(entry.child) || !entry.child.stdin || !entry.receipt
    || !fileStillMatches(entry.file)) {
    throw new Error('owned document registration is no longer eligible for source bootstrap');
  }
  try {
    const names = readdirSync(entry.file.directory);
    if (names.length !== 1 || names[0] !== DOCUMENT_NAME) {
      throw new Error('owned document directory contents changed');
    }
  } catch {
    throw new Error('owned document directory is no longer eligible for source bootstrap');
  }

  const target = entry.receipt.target_identity;
  const record: OwnedDocumentSourceBootstrap = {
    version: 1,
    registration_id: entry.runId,
    pid: target.pid,
    start_seconds: target.start_seconds,
    start_micros: target.start_microseconds,
    executable_path: target.executable_path,
    bundle_id: entry.receipt.bundle_identifier,
    document_path: entry.file.documentPath,
    document_sha256: entry.file.sha256,
    document_dev: entry.file.fileDev,
    document_ino: entry.file.fileIno,
    directory_dev: entry.file.directoryDev,
    directory_ino: entry.file.directoryIno,
    issued_unix_ms: Date.now(),
  };
  return encodeOwnedDocumentSourceBootstrapRecord(record);
}

/** Ask only the privately registered helper to activate its exact owned instance for observation. */
export function activateOwnedDocumentForObservation(registration: OwnedDocumentRegistration): Promise<boolean> {
  if (!registration || typeof registration !== 'object' || !(registration instanceof OwnedDocumentRegistration)) {
    return Promise.resolve(false);
  }
  const binding = activeRegistrations.get(registration);
  if (!binding) return Promise.resolve(false);
  const { entry } = binding;
  if (entry.activationPromise) return entry.activationPromise;
  if (!binding.isCurrent() || entry.phase !== 'registered' || entry.stopPromise !== null
    || closedChildren.has(entry.child) || !entry.child.stdin || !entry.receipt) return Promise.resolve(false);

  const operation = requestOwnedDocumentActivation(entry);
  entry.activationPromise = operation;
  void operation.finally(() => {
    if (entry.activationPromise === operation) entry.activationPromise = null;
  }).catch(() => undefined);
  return operation;
}

async function requestOwnedDocumentActivation(entry: ActiveLaunch): Promise<boolean> {
  try {
    const names = readdirSync(entry.file.directory);
    if (entry.phase !== 'registered' || entry.stopPromise !== null || closedChildren.has(entry.child)
      || !entry.receipt || names.length !== 1 || names[0] !== DOCUMENT_NAME || !fileStillMatches(entry.file)) {
      entry.phase = 'unknown';
      return false;
    }
    await entry.channel.send({ op: 'activate', run_id: entry.runId });
    const response = await entry.channel.next(3_000);
    if (!exactKeys(response, ['kind', 'launcher_id', 'run_id', 'target_pid', 'status'])
      || response.kind !== 'activation_result' || response.launcher_id !== entry.launcherId
      || response.run_id !== entry.runId || !entry.receipt
      || response.target_pid !== entry.receipt.target_identity.pid || response.status !== 'activated') {
      entry.phase = 'unknown';
      return false;
    }
    return true;
  } catch {
    entry.phase = 'unknown';
    return false;
  }
}

export interface OwnedDocumentLaunchRunner {
  launch(text: string): Promise<OwnedDocumentRegistration>;
  stopAll(): Promise<readonly ('stopped' | 'unknown')[]>;
}

class ChildChannel {
  private buffer = Buffer.alloc(0);
  private readonly messages: JsonObject[] = [];
  private readonly waiters: Array<{ resolve: (value: JsonObject) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }> = [];
  private terminal: Error | null = null;
  private stderrBytes = 0;
  private stdoutBytes = 0;

  constructor(private readonly child: ChildLike) {
    child.stdout?.on('data', (chunk: Buffer | string) => this.onData(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)));
    child.stderr?.on('data', (chunk: Buffer | string) => {
      this.stderrBytes += Buffer.isBuffer(chunk) ? chunk.length : Buffer.byteLength(chunk);
      if (this.stderrBytes > MAX_STDERR_BYTES) this.fail(new Error('owned launcher diagnostic limit exceeded'));
    });
    child.once('error', () => this.fail(new Error('owned launcher process could not be started')));
    child.once('exit', () => { closedChildren.add(child); });
    child.once('close', () => { closedChildren.add(child); this.fail(new Error('owned launcher process closed')); });
  }

  async send(value: JsonObject): Promise<void> {
    if (this.terminal) throw this.terminal;
    const line = `${JSON.stringify(value)}\n`;
    if (Buffer.byteLength(line) > 8192 || !this.child.stdin) throw new Error('owned launcher command exceeds its bound');
    await new Promise<void>((resolveWrite, rejectWrite) => {
      let settled = false;
      const timer = setTimeout(() => {
        if (!settled) { settled = true; rejectWrite(new Error('owned launcher command write timed out')); }
      }, 2_000);
      this.child.stdin!.write(line, (error) => {
        if (settled) return;
        settled = true; clearTimeout(timer);
        if (error) rejectWrite(new Error('owned launcher command could not be written'));
        else resolveWrite();
      });
    });
  }

  next(timeoutMs: number): Promise<JsonObject> {
    const queued = this.messages.shift();
    if (queued) return Promise.resolve(queued);
    if (this.terminal) return Promise.reject(this.terminal);
    return new Promise<JsonObject>((resolveNext, rejectNext) => {
      const waiter = {
        resolve: resolveNext,
        reject: rejectNext,
        timer: setTimeout(() => {
          const index = this.waiters.indexOf(waiter);
          if (index >= 0) this.waiters.splice(index, 1);
          rejectNext(new Error('owned launcher response timed out'));
        }, timeoutMs),
      };
      this.waiters.push(waiter);
    });
  }

  private onData(chunk: Buffer): void {
    if (this.terminal) return;
    this.stdoutBytes += chunk.length;
    if (chunk.length > MAX_LINE_BYTES || this.stdoutBytes > MAX_STDOUT_BYTES) {
      this.fail(new Error('owned launcher output exceeded its bound'));
      return;
    }
    this.buffer = Buffer.concat([this.buffer, chunk]);
    if (this.buffer.length > MAX_LINE_BYTES && this.buffer.indexOf(0x0a) < 0) {
      this.fail(new Error('owned launcher response exceeded its bound'));
      return;
    }
    while (true) {
      const newline = this.buffer.indexOf(0x0a);
      if (newline < 0) break;
      if (newline > MAX_LINE_BYTES) { this.fail(new Error('owned launcher response exceeded its bound')); return; }
      const line = this.buffer.subarray(0, newline);
      this.buffer = this.buffer.subarray(newline + 1);
      let value: unknown;
      try { value = JSON.parse(line.toString('utf8')); } catch { this.fail(new Error('owned launcher returned invalid JSON')); return; }
      if (!isObject(value)) { this.fail(new Error('owned launcher returned an invalid record')); return; }
      const waiter = this.waiters.shift();
      if (waiter) { clearTimeout(waiter.timer); waiter.resolve(value); }
      else {
        if (this.messages.length >= MAX_QUEUED_MESSAGES) { this.fail(new Error('owned launcher response queue exceeded its bound')); return; }
        this.messages.push(value);
      }
    }
  }

  private fail(error: Error): void {
    if (this.terminal) return;
    this.terminal = error;
    for (const waiter of this.waiters.splice(0)) { clearTimeout(waiter.timer); waiter.reject(error); }
  }
}

class ProductOwnedDocumentLaunchRunner implements OwnedDocumentLaunchRunner {
  private readonly active = new Map<string, ActiveLaunch>();
  private readonly unresolvedDirectories = new Map<string, PartialDirectoryIdentity>();
  private lifecycle: Promise<unknown> | null = null;

  constructor(
    private readonly helperPath: string,
    private readonly spawnChild: SpawnLike,
    private readonly tempBase: string,
    private readonly documentWriter: DocumentWriter = (fd, text) => writeFileSync(fd, text, { encoding: 'utf8' }),
  ) {}

  async launch(text: string): Promise<OwnedDocumentRegistration> {
    if (typeof text !== 'string' || text.includes('\u0000') || Buffer.byteLength(text, 'utf8') > MAX_DOCUMENT_BYTES) {
      throw new Error('Owned document text must be NUL-free and at most 4096 UTF-8 bytes.');
    }
    if (this.active.size !== 0 || this.unresolvedDirectories.size !== 0) throw new Error('An owned document launch is already active or unresolved.');
    const helper = resolveTrustedHelper(this.helperPath);
    let file: FileIdentity;
    try {
      file = createPrivateDocument(text, this.tempBase, this.documentWriter, (identity) => {
        this.unresolvedDirectories.set(identity.directory, identity);
      });
    } catch {
      throw new Error('A private owned document could not be created or safely cleaned up.');
    }
    const runId = randomUUID().toLowerCase();
    let child: ChildLike | null = null;
    let entry: ActiveLaunch | null = null;
    let launchCommandSent = false;
    try {
      child = this.spawnChild(helper.path, [], {
        cwd: dirname(helper.path),
        env: helperEnvironment(this.tempBase),
        stdio: ['pipe', 'pipe', 'pipe'],
      });
      const channel = new ChildChannel(child);
      entry = {
        runId, child, channel, file, helperPath: helper.path, launcherId: '', receipt: null,
        phase: 'starting', stopPromise: null, activationPromise: null,
        launcherClosed: false, diagnosticCleanupDigest: null, diagnosticCleanupComplete: false,
        targetClosed: false,
      };
      this.active.set(runId, entry);

      const ready = await channel.next(5_000);
      if (!exactKeys(ready, ['kind', 'launcher_id', 'launcher_identity']) || ready.kind !== 'ready') throw new Error('owned launcher handshake failed');
      const launcherId = uuid(ready.launcher_id);
      const launcher = processIdentity(ready.launcher_identity);
      if (!launcherId || !launcher || launcher.pid !== child.pid || launcher.parent_pid !== process.pid
        || launcher.executable_path !== helper.path || !sameFile(helper.path, launcher.executable_path)) {
        throw new Error('owned launcher identity did not match the spawned helper');
      }
      entry.launcherId = launcherId;
      const sentAt = Date.now();
      entry.phase = 'launch_sent';
      launchCommandSent = true;
      await channel.send({ op: 'launch', run_id: runId, document_path: file.documentPath, document_sha256: file.sha256 });
      const response = await channel.next(20_000);
      if (exactKeys(response, ['kind', 'launcher_id', 'status', 'error'])
        && response.kind === 'launch_error' && response.launcher_id === launcherId
        && response.status === 'not_launched' && typeof response.error === 'string') {
        if (await this.discardKnownUnlaunched(entry)) {
          launchCommandSent = false;
          entry = null;
        }
        throw new Error('TextEdit launch was refused.');
      }
      if (!exactKeys(response, ['kind', 'launcher_id', 'receipt']) || response.kind !== 'launch_receipt'
        || response.launcher_id !== launcherId) throw new Error('owned launch receipt was missing or malformed');
      const receipt = validateReceipt(response.receipt, {
        runId, launcherId, launcher, helperPath: helper.path, file, sentAt, receivedAt: Date.now(),
      });
      entry = { ...entry, receipt, phase: 'registered' };
      this.active.set(runId, entry);
      return this.issueRegistration(entry);
    } catch {
      if (entry && launchCommandSent) {
        entry.phase = 'unknown';
        const result = await this.tryStopUnknown(entry);
        if (result === 'stopped') throw new Error('TextEdit launch could not be verified; its owned process was stopped.');
        // Keep the registration and private file for a later safe stop attempt.
      } else if (entry) {
        await this.discardKnownUnlaunched(entry);
      } else {
        if (child) child.kill('SIGKILL');
        removePrivateDocument(file);
      }
      throw new Error('Owned TextEdit launch is unavailable or unresolved. No process identity was granted to the caller.');
    }
  }

  async stopAll(): Promise<readonly ('stopped' | 'unknown')[]> {
    if (this.lifecycle) { try { await this.lifecycle; } catch { /* Still clean tracked ownership. */ } }
    const results: Array<'stopped' | 'unknown'> = [];
    for (const entry of [...this.active.values()]) results.push(await this.stopEntry(entry));
    for (const [directory, identity] of [...this.unresolvedDirectories]) {
      if (removePartialPrivateDirectory(identity)) { this.unresolvedDirectories.delete(directory); results.push('stopped'); }
      else results.push('unknown');
    }
    return results;
  }

  private stopEntry(entry: ActiveLaunch): Promise<'stopped' | 'unknown'> {
    if (this.lifecycle) return this.lifecycle.then(() => this.stopEntry(entry), () => this.stopEntry(entry));
    if (this.active.get(entry.runId) !== entry) return Promise.resolve('unknown');
    if (entry.stopPromise) return entry.stopPromise;
    if (entry.activationPromise) return entry.activationPromise.then(() => this.stopEntry(entry));
    const operation = this.stopEntryInner(entry);
    entry.stopPromise = operation;
    void operation.finally(() => { if (entry.stopPromise === operation) entry.stopPromise = null; }).catch(() => undefined);
    return operation;
  }

  private stopDiagnosticEntry(entry: ActiveLaunch, expectedText: string): Promise<'stopped' | 'unknown'> {
    if (typeof expectedText !== 'string' || expectedText.includes('\u0000')
      || Buffer.byteLength(expectedText, 'utf8') > 2048) return Promise.resolve('unknown');
    const digest = createHash('sha256').update(expectedText, 'utf8').digest('hex');
    if (entry.diagnosticCleanupDigest !== null && entry.diagnosticCleanupDigest !== digest) {
      return Promise.resolve('unknown');
    }
    if (entry.diagnosticCleanupComplete) return Promise.resolve('stopped');
    if (this.active.get(entry.runId) !== entry) return Promise.resolve('unknown');
    entry.diagnosticCleanupDigest = digest;
    return this.stopEntry(entry);
  }

  private async stopEntryInner(entry: ActiveLaunch): Promise<'stopped' | 'unknown'> {
    entry.phase = 'stopping';
    try {
      if (entry.launcherClosed) return this.finishStoppedCleanup(entry);
      if (!entry.targetClosed && !await this.closeTarget(entry)) return 'unknown';
      await entry.channel.send({ op: 'quit' });
      const quit = await entry.channel.next(2_000);
      if (!exactKeys(quit, ['kind', 'launcher_id', 'status']) || quit.kind !== 'quit_result'
        || quit.launcher_id !== entry.launcherId || !['terminated', 'owned_identity_gone', 'no_owned_instance'].includes(String(quit.status))) {
        entry.phase = 'unknown';
        return 'unknown';
      }
      entry.child.stdin?.end();
      const closed = await waitForClose(entry.child, 2_000);
      if (!closed) { entry.phase = 'unknown'; return 'unknown'; }
      // Only exact native stop/quit receipts and observed helper closure reach
      // this cache; filesystem cleanup retries cannot fabricate process death.
      entry.launcherClosed = true;
      return this.finishStoppedCleanup(entry);
    } catch {
      entry.phase = 'unknown';
      return 'unknown';
    }
  }

  private issueRegistration(entry: ActiveLaunch): OwnedDocumentRegistration {
    return OwnedDocumentRegistration.issue(REGISTRATION_ISSUER, entry,
      () => this.stopEntry(entry), () => this.active.get(entry.runId) === entry,
      expectedText => this.stopDiagnosticEntry(entry, expectedText),
      expectedText => this.retainEntry(entry, expectedText),
      expected => this.captureSavedVersion(entry, expected));
  }

  private captureSavedVersion(entry: ActiveLaunch, expected: SavedVersionExpectation): OwnedDocumentSavedVersion {
    if (!validSavedVersionExpectation(expected) || this.active.get(entry.runId) !== entry
      || entry.phase !== 'registered' || entry.stopPromise || entry.activationPromise || entry.launcherClosed
      || closedChildren.has(entry.child) || !entry.receipt || !entry.child.stdin) {
      throw new OwnedDocumentLifecycleError('saved_version_unavailable');
    }
    const captured = inspectRetainedDocument(entry.file, expected.sha256, false);
    if (!captured || captured.size !== expected.byteLength) {
      throw new OwnedDocumentLifecycleError('saved_version_unavailable');
    }
    return OwnedDocumentSavedVersion.issue(REGISTRATION_ISSUER, () =>
      this.retainSavedVersion(entry, captured));
  }

  private retainSavedVersion(entry: ActiveLaunch, captured: FileIdentity): Promise<OwnedDocumentRevisitTicket> {
    return this.runLifecycle(async () => {
      if (this.active.get(entry.runId) !== entry || entry.phase !== 'registered'
        || entry.stopPromise || entry.activationPromise || entry.launcherClosed || closedChildren.has(entry.child)
        || !entry.receipt || !entry.child.stdin) {
        throw new OwnedDocumentLifecycleError('saved_version_unavailable');
      }
      const beforeClose = inspectRetainedDocument(captured, captured.sha256, true);
      if (!beforeClose || !sameFileVersion(captured, beforeClose)) {
        entry.phase = 'unknown';
        throw new OwnedDocumentLifecycleError('saved_version_changed');
      }
      entry.phase = 'retaining';
      try {
        if (!await this.closeTarget(entry)) throw new OwnedDocumentLifecycleError('saved_version_changed');
        const afterClose = inspectRetainedDocument(captured, captured.sha256, true);
        if (!afterClose || !sameFileVersion(captured, afterClose) || closedChildren.has(entry.child)) {
          throw new OwnedDocumentLifecycleError('saved_version_changed');
        }
        const retained: ActiveLaunch = { ...entry, file: afterClose, phase: 'retained' };
        this.active.set(entry.runId, retained);
        const lineage: SavedVersionLineageState = {
          beforeClose,
          afterClose,
          afterReopen: null,
          reopenedRegistration: null,
          emitted: false,
        };
        return OwnedDocumentRevisitTicket.issue(
          REGISTRATION_ISSUER,
          () => this.reopenEntry(retained, lineage),
          lineage,
        );
      } catch (error) {
        entry.phase = 'unknown';
        if (error instanceof OwnedDocumentLifecycleError) throw error;
        throw new OwnedDocumentLifecycleError('saved_version_changed');
      }
    });
  }

  private async closeTarget(entry: ActiveLaunch): Promise<boolean> {
    await entry.channel.send({ op: 'stop', run_id: entry.runId });
    const result = await entry.channel.next(7_000);
    if (!exactKeys(result, ['kind', 'launcher_id', 'run_id', 'status', 'target_pid'])
      || result.kind !== 'stop_result' || result.launcher_id !== entry.launcherId || result.run_id !== entry.runId
      || !entry.receipt || result.target_pid !== entry.receipt.target_identity.pid
      || !['terminated', 'owned_identity_gone'].includes(String(result.status))) {
      entry.phase = 'unknown'; return false;
    }
    entry.targetClosed = true;
    return true;
  }

  private runLifecycle<T>(operation: () => Promise<T>): Promise<T> {
    if (this.lifecycle) return Promise.reject(new Error('owned document lifecycle is busy'));
    const promise = operation();
    this.lifecycle = promise;
    void promise.finally(() => { if (this.lifecycle === promise) this.lifecycle = null; }).catch(() => undefined);
    return promise;
  }

  private retainEntry(entry: ActiveLaunch, expectedText: string): Promise<OwnedDocumentRevisitTicket> {
    return this.runLifecycle(async () => {
      if (typeof expectedText !== 'string' || expectedText.includes('\u0000')
        || Buffer.byteLength(expectedText, 'utf8') > 2048
        || this.active.get(entry.runId) !== entry || entry.phase !== 'registered'
        || entry.stopPromise || entry.activationPromise || entry.launcherClosed || closedChildren.has(entry.child)) {
        throw new Error('owned document cannot enter retention');
      }
      entry.phase = 'retaining';
      try {
        if (!await this.closeTarget(entry)) throw new Error('owned document close was not confirmed');
        const digest = createHash('sha256').update(expectedText, 'utf8').digest('hex');
        const file = inspectRetainedDocument(entry.file, digest, false);
        if (!file || closedChildren.has(entry.child)) throw new Error('retained document identity or bytes were not verified');
        const retained: ActiveLaunch = { ...entry, file, phase: 'retained' };
        // Retire the original registration while retaining private cleanup ownership.
        this.active.set(entry.runId, retained);
        return OwnedDocumentRevisitTicket.issue(REGISTRATION_ISSUER, () => this.reopenEntry(retained));
      } catch {
        entry.phase = 'unknown';
        throw new Error('owned document retention is unresolved');
      }
    });
  }

  private reopenEntry(
    retained: ActiveLaunch,
    lineage?: SavedVersionLineageState,
  ): Promise<OwnedDocumentRegistration> {
    return this.runLifecycle(async () => {
      if (this.active.get(retained.runId) !== retained || retained.phase !== 'retained'
        || !retained.targetClosed || retained.launcherClosed || !retained.receipt
        || retained.stopPromise || closedChildren.has(retained.child)
        || resolveTrustedHelper(retained.helperPath).path !== retained.helperPath) {
        throw new OwnedDocumentLifecycleError('reopen_unavailable');
      }
      const beforeOpen = inspectRetainedDocument(retained.file, retained.file.sha256, true);
      if (!beforeOpen || !sameFileVersion(retained.file, beforeOpen)) {
        throw new OwnedDocumentLifecycleError('reopen_file_changed');
      }
      const runId = randomUUID().toLowerCase();
      let entry: ActiveLaunch = { ...retained, runId, receipt: null, phase: 'launch_sent', targetClosed: false,
        diagnosticCleanupDigest: null, diagnosticCleanupComplete: false };
      this.active.delete(retained.runId);
      this.active.set(runId, entry);
      try {
        const sentAt = Date.now();
        await entry.channel.send({ op: 'launch', run_id: runId,
          document_path: entry.file.documentPath, document_sha256: entry.file.sha256 });
        const record = await entry.channel.next(15_000);
        if (exactKeys(record, ['kind', 'launcher_id', 'status', 'error'])
          && record.kind === 'launch_error' && record.launcher_id === entry.launcherId && record.status === 'not_launched') {
          this.active.delete(runId);
          this.active.set(retained.runId, retained);
          throw new OwnedDocumentLifecycleError(record.error === 'preexisting TextEdit identity unavailable'
            ? 'reopen_preexisting_identity_unavailable' : record.error === 'private document validation failed'
              ? 'reopen_invalid_file' : 'reopen_not_launched');
        }
        if (!exactKeys(record, ['kind', 'launcher_id', 'receipt']) || record.kind !== 'launch_receipt'
          || record.launcher_id !== entry.launcherId) throw new OwnedDocumentLifecycleError('reopen_receipt_unavailable');
        const receipt = validateReceipt(record.receipt, { runId, launcherId: entry.launcherId,
          launcher: retained.receipt.launcher_identity, helperPath: entry.helperPath,
          file: entry.file, sentAt, receivedAt: Date.now() }, false);
        const old = retained.receipt.target_identity, fresh = receipt.target_identity;
        if (old.pid === fresh.pid && old.start_seconds === fresh.start_seconds
          && old.start_microseconds === fresh.start_microseconds) throw new OwnedDocumentLifecycleError('reopen_identity_reused');
        entry = { ...entry, file: beforeOpen, receipt, phase: 'registered' };
        this.active.set(runId, entry);
        const afterOpen = inspectRetainedDocument(retained.file, retained.file.sha256, true);
        if (!afterOpen || !sameFileVersion(retained.file, afterOpen)) {
          entry = { ...entry, phase: 'unknown' };
          this.active.set(runId, entry);
          throw new OwnedDocumentLifecycleError('reopen_file_changed');
        }
        entry = { ...entry, file: afterOpen, phase: 'registered' };
        this.active.set(runId, entry);
        const registration = this.issueRegistration(entry);
        if (lineage) {
          lineage.afterReopen = afterOpen;
          lineage.reopenedRegistration = registration;
        }
        return registration;
      } catch (error) {
        entry.phase = 'unknown';
        throw error instanceof OwnedDocumentLifecycleError ? error : new OwnedDocumentLifecycleError('reopen_unknown');
      }
    });
  }

  private finishStoppedCleanup(entry: ActiveLaunch): 'stopped' | 'unknown' {
    const removed = removePrivateDocument(entry.file)
      || (entry.diagnosticCleanupDigest !== null && entry.launcherClosed
        && removeDiagnosticSavedDocument(entry.file, entry.diagnosticCleanupDigest));
    if (!removed) { entry.phase = 'unknown'; return 'unknown'; }
    entry.diagnosticCleanupComplete = entry.diagnosticCleanupDigest !== null;
    this.active.delete(entry.runId);
    return 'stopped';
  }

  private async tryStopUnknown(entry: ActiveLaunch): Promise<'stopped' | 'unknown'> {
    return this.stopEntry(entry);
  }

  private async discardKnownUnlaunched(entry: ActiveLaunch): Promise<boolean> {
    try {
      await entry.channel.send({ op: 'quit' });
      const reply = await entry.channel.next(1_000);
      if (exactKeys(reply, ['kind', 'launcher_id', 'status']) && reply.kind === 'quit_result'
        && reply.launcher_id === entry.launcherId && reply.status === 'no_owned_instance') {
        entry.child.stdin?.end();
        if (await waitForClose(entry.child, 1_000) && removePrivateDocument(entry.file)) {
          this.active.delete(entry.runId);
          return true;
        }
      }
    } catch { /* A known-unlaunched document is still safe to unlink below. */ }
    entry.child.kill('SIGKILL');
    if (!await waitForClose(entry.child, 1_000) || !removePrivateDocument(entry.file)) {
      entry.phase = 'unknown';
      return false;
    }
    this.active.delete(entry.runId);
    return true;
  }
}

function resolveHelperCandidates(): string[] {
  const moduleDir = dirname(fileURLToPath(import.meta.url));
  const resourcesPath = (process as NodeJS.Process & { resourcesPath?: string }).resourcesPath;
  return [
    resourcesPath ? resolve(resourcesPath, 'bin', HELPER_NAME) : null,
    resolve(moduleDir, '../../../target/debug', HELPER_NAME),
    resolve(moduleDir, '../../../../target/debug', HELPER_NAME),
    resolve(moduleDir, '../../../target/release', HELPER_NAME),
    resolve(moduleDir, '../../../../target/release', HELPER_NAME),
  ].filter((candidate): candidate is string => candidate !== null);
}

function resolveTrustedHelper(requested?: string): { path: string } {
  const candidates = resolveHelperCandidates();
  const candidate = requested ?? candidates.find((path) => {
    try { const stat = lstatSync(path); return stat.isFile() && !stat.isSymbolicLink() && (stat.mode & 0o111) !== 0; }
    catch { return false; }
  });
  if (!candidate || !isAbsolute(candidate)) throw new Error('Owned TextEdit launcher is unavailable.');
  const canonical = realpathSync(candidate);
  if (!candidates.some((path) => resolve(path) === resolve(candidate) && realpathSync(path) === canonical)) {
    throw new Error('Owned TextEdit launcher path is not a product path.');
  }
  const stat = lstatSync(candidate);
  if (!stat.isFile() || stat.isSymbolicLink() || (stat.mode & 0o111) === 0 || canonical !== candidate) {
    throw new Error('Owned TextEdit launcher is not a trusted regular executable.');
  }
  return { path: canonical };
}

function helperEnvironment(tempBase: string): NodeJS.ProcessEnv {
  const result: NodeJS.ProcessEnv = {};
  for (const key of ['LANG', 'LC_ALL', 'LC_CTYPE', 'TZ']) if (typeof process.env[key] === 'string') result[key] = process.env[key];
  result.TMPDIR = tempBase;
  return result;
}

function createPrivateDocument(
  text: string,
  tempBase: string,
  documentWriter: DocumentWriter,
  retainUnresolved: (identity: PartialDirectoryIdentity) => void,
): FileIdentity {
  const canonicalBase = realpathSync(tempBase);
  const directory = mkdtempSync(join(canonicalBase, 'cu-owned-document-'));
  const owner = process.getuid?.() ?? -1;
  const partial: PartialDirectoryIdentity = {
    directory, directoryDev: null, directoryIno: null, owner, fileDev: null, fileIno: null,
  };
  try {
    const canonicalDirectory = realpathSync(directory);
    const initialDirectoryStat = lstatSync(directory);
    partial.directoryDev = initialDirectoryStat.dev;
    partial.directoryIno = initialDirectoryStat.ino;
    if (canonicalDirectory !== directory || !initialDirectoryStat.isDirectory() || initialDirectoryStat.isSymbolicLink()
      || initialDirectoryStat.uid !== owner || (initialDirectoryStat.mode & 0o777) !== 0o700) {
      throw new Error('New private directory identity is invalid.');
    }
    chmodSync(directory, 0o700);
    const directoryStat = lstatSync(canonicalDirectory);
    if (!sameDirectoryIdentity(partial, directoryStat) || (directoryStat.mode & 0o777) !== 0o700) throw new Error('Private document directory changed.');
    const documentPath = join(canonicalDirectory, DOCUMENT_NAME);
    const fd = openSync(documentPath, 'wx', 0o600);
    try {
      const descriptor = fstatSync(fd);
      partial.fileDev = descriptor.dev;
      partial.fileIno = descriptor.ino;
      const openedPath = lstatSync(documentPath);
      if (!openedPath.isFile() || openedPath.isSymbolicLink() || openedPath.dev !== descriptor.dev || openedPath.ino !== descriptor.ino
        || openedPath.uid !== owner || openedPath.nlink !== 1) throw new Error('Opened document inode changed.');
      fchmodSync(fd, 0o600);
      documentWriter(fd, text);
      fsyncSync(fd);
    } finally { closeSync(fd); }
    const fileStat = lstatSync(documentPath);
    const canonicalDocument = realpathSync(documentPath);
    if (!fileStat.isFile() || fileStat.isSymbolicLink() || fileStat.nlink !== 1 || fileStat.uid !== owner
      || fileStat.dev !== partial.fileDev || fileStat.ino !== partial.fileIno
      || (fileStat.mode & 0o777) !== 0o600 || canonicalDocument !== documentPath || fileStat.size !== Buffer.byteLength(text, 'utf8')) {
      throw new Error('Private document file could not be secured.');
    }
    const preciseFileStat = lstatSync(documentPath, { bigint: true });
    const preciseDev = safeNumber(preciseFileStat.dev), preciseIno = safeNumber(preciseFileStat.ino);
    const preciseSize = safeNumber(preciseFileStat.size);
    if (!preciseFileStat.isFile() || preciseFileStat.isSymbolicLink() || preciseFileStat.nlink !== 1n
      || preciseFileStat.uid !== BigInt(owner) || (preciseFileStat.mode & 0o777n) !== 0o600n
      || preciseDev !== fileStat.dev || preciseIno !== fileStat.ino || preciseSize !== fileStat.size) {
      throw new Error('Private document file could not be secured.');
    }
    return {
      directory: canonicalDirectory,
      documentPath: canonicalDocument,
      directoryDev: directoryStat.dev,
      directoryIno: directoryStat.ino,
      fileDev: preciseDev,
      fileIno: preciseIno,
      size: preciseSize,
      mtimeNs: preciseFileStat.mtimeNs.toString(),
      ctimeNs: preciseFileStat.ctimeNs.toString(),
      sha256: createHash('sha256').update(readFileSync(documentPath)).digest('hex'),
    };
  } catch (error) {
    if (!removePartialPrivateDirectory(partial)) retainUnresolved(partial);
    throw error;
  }
}

function sameDirectoryIdentity(identity: PartialDirectoryIdentity, stat: ReturnType<typeof lstatSync>): boolean {
  return stat !== undefined && identity.directoryDev !== null && identity.directoryIno !== null
    && stat.isDirectory() && !stat.isSymbolicLink() && stat.uid === identity.owner
    && stat.dev === identity.directoryDev && stat.ino === identity.directoryIno
    && realpathSync(identity.directory) === identity.directory;
}

function removePartialPrivateDirectory(identity: PartialDirectoryIdentity): boolean {
  try {
    const directoryStat = lstatSync(identity.directory);
    if (!sameDirectoryIdentity(identity, directoryStat) || (directoryStat.mode & 0o777) !== 0o700) return false;
    const children = readdirSync(identity.directory);
    if (children.length === 0) {
      const finalDirectory = lstatSync(identity.directory);
      if (!sameDirectoryIdentity(identity, finalDirectory) || (finalDirectory.mode & 0o777) !== 0o700
        || readdirSync(identity.directory).length !== 0) return false;
      rmdirSync(identity.directory);
      return true;
    }
    if (identity.fileDev === null || identity.fileIno === null || children.length !== 1 || children[0] !== DOCUMENT_NAME) return false;
    const documentPath = join(identity.directory, DOCUMENT_NAME);
    const document = lstatSync(documentPath);
    if (!document.isFile() || document.isSymbolicLink() || document.nlink !== 1
      || document.uid !== identity.owner || document.dev !== identity.fileDev || document.ino !== identity.fileIno
      || (document.mode & 0o777) !== 0o600 || realpathSync(documentPath) !== documentPath) return false;
    const finalDirectory = lstatSync(identity.directory);
    const finalDocument = lstatSync(documentPath);
    const finalChildren = readdirSync(identity.directory);
    if (!sameDirectoryIdentity(identity, finalDirectory) || (finalDirectory.mode & 0o777) !== 0o700
      || finalChildren.length !== 1 || finalChildren[0] !== DOCUMENT_NAME
      || !finalDocument.isFile() || finalDocument.isSymbolicLink() || finalDocument.nlink !== 1
      || finalDocument.uid !== identity.owner || finalDocument.dev !== identity.fileDev || finalDocument.ino !== identity.fileIno
      || (finalDocument.mode & 0o777) !== 0o600 || realpathSync(documentPath) !== documentPath) return false;
    unlinkSync(documentPath);
    rmdirSync(identity.directory);
    return true;
  } catch { return false; }
}

function privateFileIdentityMatches(file: FileIdentity): boolean {
  try {
    const dir = lstatSync(file.directory);
    const doc = lstatSync(file.documentPath);
    return dir.isDirectory() && !dir.isSymbolicLink() && dir.uid === process.getuid?.()
      && dir.dev === file.directoryDev && dir.ino === file.directoryIno && (dir.mode & 0o777) === 0o700
      && doc.isFile() && !doc.isSymbolicLink() && doc.nlink === 1 && doc.uid === process.getuid?.()
      && doc.dev === file.fileDev && doc.ino === file.fileIno && (doc.mode & 0o777) === 0o600
      && realpathSync(file.directory) === file.directory && realpathSync(file.documentPath) === file.documentPath;
  } catch { return false; }
}

function fileStillMatches(file: FileIdentity): boolean {
  if (!privateFileIdentityMatches(file)) return false;
  let fd: number | null = null;
  const bytes = Buffer.alloc(MAX_DOCUMENT_BYTES + 1);
  try {
    fd = openSync(file.documentPath, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = fstatSync(fd);
    if (!opened.isFile() || opened.dev !== file.fileDev || opened.ino !== file.fileIno
      || opened.uid !== process.getuid?.() || opened.nlink !== 1 || (opened.mode & 0o777) !== 0o600
      || opened.size < 0 || opened.size > MAX_DOCUMENT_BYTES) return false;
    const count = readSync(fd, bytes, 0, bytes.length, 0);
    const after = fstatSync(fd), path = lstatSync(file.documentPath);
    const stable = (value: Stats): boolean => value.dev === opened.dev && value.ino === opened.ino
      && value.size === opened.size && value.mtimeMs === opened.mtimeMs && value.ctimeMs === opened.ctimeMs;
    return count === opened.size && stable(after) && stable(path) && privateFileIdentityMatches(file)
      && createHash('sha256').update(bytes.subarray(0, count)).digest('hex') === file.sha256;
  } catch { return false; }
  finally { bytes.fill(0); if (fd !== null) closeSync(fd); }
}

function removePrivateDocument(file: FileIdentity): boolean {
  if (!privateFileIdentityMatches(file)) return false;
  try {
    const entries = readdirSync(file.directory);
    if (entries.length !== 1 || entries[0] !== DOCUMENT_NAME) return false;
    if (!privateFileIdentityMatches(file)) return false;
    const finalEntries = readdirSync(file.directory);
    if (finalEntries.length !== 1 || finalEntries[0] !== DOCUMENT_NAME) return false;
    unlinkSync(file.documentPath);
    rmdirSync(file.directory);
    return true;
  } catch { return false; }
}

function removeDiagnosticSavedDocument(file: FileIdentity, expectedDigest: string): boolean {
  let fd: number | null = null;
  const bytes = Buffer.alloc(2049);
  try {
    const directoryMatches = (): boolean => {
      const dir = lstatSync(file.directory);
      const names = readdirSync(file.directory);
      return dir.isDirectory() && !dir.isSymbolicLink() && dir.uid === process.getuid?.()
        && dir.dev === file.directoryDev && dir.ino === file.directoryIno && (dir.mode & 0o777) === 0o700
        && realpathSync(file.directory) === file.directory && names.length === 1 && names[0] === DOCUMENT_NAME;
    };
    const eligible = (value: Stats): boolean => value.isFile()
      && !value.isSymbolicLink() && value.uid === process.getuid?.() && value.nlink === 1
      && (value.mode & 0o777) === 0o600 && value.size >= 0 && value.size <= 2048;
    const same = (a: Stats, b: Stats): boolean =>
      a.dev === b.dev && a.ino === b.ino && a.size === b.size
      && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs && eligible(b);
    if (!directoryMatches()) return false;
    const before = lstatSync(file.documentPath);
    if (!eligible(before) || realpathSync(file.documentPath) !== file.documentPath) return false;
    fd = openSync(file.documentPath, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = fstatSync(fd);
    if (!same(before, opened)) return false;
    const length = readSync(fd, bytes, 0, bytes.length, 0);
    if (length !== opened.size || createHash('sha256').update(bytes.subarray(0, length)).digest('hex') !== expectedDigest
      || !same(opened, fstatSync(fd)) || !same(opened, lstatSync(file.documentPath))
      || !directoryMatches()) return false;
    unlinkSync(file.documentPath);
    rmdirSync(file.directory);
    return true;
  } catch { return false; }
  finally { bytes.fill(0); if (fd !== null) closeSync(fd); }
}

function inspectRetainedDocument(original: FileIdentity, expectedDigest: string, requireExactFile: boolean): FileIdentity | null {
  let fd: number | null = null;
  const bytes = Buffer.alloc(2049);
  try {
    const directoryMatches = (): boolean => {
      const dir = lstatSync(original.directory, { bigint: true }), names = readdirSync(original.directory);
      return dir.isDirectory() && !dir.isSymbolicLink() && dir.uid === BigInt(process.getuid?.() ?? -1)
        && dir.dev === BigInt(original.directoryDev) && dir.ino === BigInt(original.directoryIno)
        && (dir.mode & 0o777n) === 0o700n
        && realpathSync(original.directory) === original.directory && names.length === 1 && names[0] === DOCUMENT_NAME;
    };
    const eligible = (value: BigIntStats): boolean => value.isFile() && !value.isSymbolicLink()
      && value.uid === BigInt(process.getuid?.() ?? -1) && value.nlink === 1n
      && (value.mode & 0o777n) === 0o600n && value.dev <= BigInt(Number.MAX_SAFE_INTEGER)
      && value.ino > 0n && value.ino <= BigInt(Number.MAX_SAFE_INTEGER)
      && value.size >= 0n && value.size <= 2_048n;
    const same = (a: BigIntStats, b: BigIntStats): boolean => eligible(b) && a.dev === b.dev && a.ino === b.ino
      && a.size === b.size && a.mtimeNs === b.mtimeNs && a.ctimeNs === b.ctimeNs;
    if (!directoryMatches()) return null;
    const before = lstatSync(original.documentPath, { bigint: true });
    if (!eligible(before) || realpathSync(original.documentPath) !== original.documentPath) return null;
    fd = openSync(original.documentPath, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = fstatSync(fd, { bigint: true });
    if (!same(before, opened)) return null;
    if (requireExactFile && (opened.dev !== BigInt(original.fileDev) || opened.ino !== BigInt(original.fileIno))) return null;
    const count = readSync(fd, bytes, 0, bytes.length, 0);
    if (BigInt(count) !== opened.size || createHash('sha256').update(bytes.subarray(0, count)).digest('hex') !== expectedDigest
      || !same(opened, fstatSync(fd, { bigint: true }))
      || !same(opened, lstatSync(original.documentPath, { bigint: true }))
      || !directoryMatches()) return null;
    const dev = safeNumber(opened.dev), ino = safeNumber(opened.ino), size = safeNumber(opened.size);
    if (dev === null || ino === null || size === null) return null;
    return {
      ...original,
      fileDev: dev,
      fileIno: ino,
      size,
      mtimeNs: opened.mtimeNs.toString(),
      ctimeNs: opened.ctimeNs.toString(),
      sha256: expectedDigest,
    };
  } catch { return null; }
  finally { bytes.fill(0); if (fd !== null) closeSync(fd); }
}

function validateReceipt(value: unknown, expected: {
  runId: string; launcherId: string; launcher: KernelIdentity; helperPath: string; file: FileIdentity;
  sentAt: number; receivedAt: number;
}, verifyFile = true): LaunchReceipt {
  if (!isObject(value) || !exactKeys(value, RECEIPT_KEYS)) throw new Error('owned receipt schema mismatch');
  const receipt = value as unknown as LaunchReceipt;
  if (receipt.version !== 1 || receipt.run_id !== expected.runId || receipt.launcher_id !== expected.launcherId
    || !sameJson(receipt.launcher_identity, expected.launcher)
    || receipt.bundle_identifier !== TEXTEDIT_BUNDLE || receipt.application_path !== TEXTEDIT_APP
    || receipt.executable_path !== TEXTEDIT_EXEC || receipt.document_path !== expected.file.documentPath
    || receipt.document_sha256 !== expected.file.sha256 || !uuid(receipt.run_id)
    || !Number.isSafeInteger(receipt.launch_start_unix_milliseconds) || !Number.isSafeInteger(receipt.launch_finish_unix_milliseconds)
    || receipt.launch_start_unix_milliseconds <= 0 || receipt.launch_finish_unix_milliseconds < receipt.launch_start_unix_milliseconds
    || receipt.launch_finish_unix_milliseconds - receipt.launch_start_unix_milliseconds > 15_000
    || receipt.launch_start_unix_milliseconds < expected.sentAt - 1_000
    || receipt.launch_finish_unix_milliseconds > expected.receivedAt + 1_000
    || receipt.launch_finish_unix_milliseconds < expected.receivedAt - 25_000
    || receipt.new_pid_excluded_from_preexisting_set !== true || receipt.target_started_within_launch_interval !== true
    || receipt.creates_new_application_instance !== true || receipt.adds_to_recent_items !== false
    || receipt.activates_new_instance !== false || receipt.target_parent_is_launchd !== true || receipt.no_input_authority !== true
    || !Array.isArray(receipt.preexisting_textedit_pids) || receipt.preexisting_textedit_pids.length > 4096
    || receipt.preexisting_textedit_pids.some((pid) => !Number.isInteger(pid) || pid <= 0)
    || !Array.isArray(receipt.preexisting_text_edit_applications) || receipt.preexisting_text_edit_applications.length > 4096
    || !Array.isArray(receipt.arguments) || receipt.arguments.length !== 2
    || receipt.arguments[0] !== '-ApplePersistenceIgnoreState' || receipt.arguments[1] !== 'YES') {
    throw new Error('owned receipt facts did not match the launch request');
  }
  const target = processIdentity(receipt.target_identity);
  const began = target.start_seconds * 1000 + Math.floor(target.start_microseconds / 1000);
  const previous = receipt.preexisting_textedit_pids;
  const previousApps = receipt.preexisting_text_edit_applications;
  const previousAppPIDs = new Set<number>();
  let previousFactsValid = true;
  for (const raw of previousApps) {
    if (!isObject(raw) || !exactKeys(raw, ['pid', 'bundle_identifier', 'bundle_path', 'kernel_identity'])) {
      previousFactsValid = false;
      break;
    }
    const item = raw as { pid: unknown; bundle_identifier: unknown; bundle_path: unknown; kernel_identity: unknown };
    let identity: KernelIdentity;
    try { identity = processIdentity(item.kernel_identity); } catch { previousFactsValid = false; break; }
    if (!Number.isInteger(item.pid) || item.pid !== identity.pid || item.pid <= 0
      || item.bundle_identifier !== TEXTEDIT_BUNDLE
      || !(item.bundle_path === null || (typeof item.bundle_path === 'string' && isAbsolute(item.bundle_path)))
      || identity.executable_path !== TEXTEDIT_EXEC || previousAppPIDs.has(item.pid)) {
      previousFactsValid = false;
      break;
    }
    previousAppPIDs.add(item.pid);
  }
  const previousPIDs = new Set(previous);
  const targetFresh = began >= expected.sentAt - 1_000 && began <= expected.receivedAt + 1_000;
  if (target.pid === expected.launcher.pid || target.executable_path !== TEXTEDIT_EXEC || target.parent_pid !== 1
    || previousPIDs.size !== previous.length || previousApps.length !== previousPIDs.size
    || !previousFactsValid || [...previousPIDs].some((pid) => !previousAppPIDs.has(pid))
    || receipt.preexisting_textedit_pids.includes(target.pid)
    || began < receipt.launch_start_unix_milliseconds - 1000 || began > receipt.launch_finish_unix_milliseconds
    || !targetFresh || verifyFile && !fileStillMatches(expected.file)) throw new Error('owned receipt identity, freshness, or document changed');
  return receipt;
}

function processIdentity(value: unknown): KernelIdentity {
  if (!isObject(value) || !exactKeys(value, IDENTITY_KEYS)) throw new Error('invalid process identity');
  const item = value as unknown as KernelIdentity;
  if (!Number.isInteger(item.pid) || item.pid <= 0 || item.pid > 0x7fff_ffff
    || !Number.isInteger(item.parent_pid) || item.parent_pid < 0 || item.parent_pid > 0x7fff_ffff
    || !Number.isSafeInteger(item.start_seconds) || item.start_seconds <= 0
    || !Number.isInteger(item.start_microseconds) || item.start_microseconds < 0 || item.start_microseconds >= 1_000_000
    || typeof item.executable_path !== 'string' || !isAbsolute(item.executable_path)
    || !(item.parent_executable_path === null || typeof item.parent_executable_path === 'string')) {
    throw new Error('invalid process identity');
  }
  return item;
}

function exactKeys(value: JsonObject, keys: readonly string[]): boolean {
  return Object.keys(value).sort().join(',') === [...keys].sort().join(',');
}
function isObject(value: unknown): value is JsonObject { return !!value && typeof value === 'object' && !Array.isArray(value); }
function validSavedVersionExpectation(value: unknown): value is SavedVersionExpectation {
  try {
    return isObject(value) && exactKeys(value, ['sha256', 'byteLength'])
      && typeof value.sha256 === 'string' && /^[0-9a-f]{64}$/.test(value.sha256)
      && Number.isSafeInteger(value.byteLength) && (value.byteLength as number) >= 0
      && (value.byteLength as number) <= 2048;
  } catch { return false; }
}
function safeNumber(value: bigint): number | null {
  return value >= 0n && value <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(value) : null;
}
function sameFileVersion(a: FileIdentity, b: FileIdentity): boolean {
  return a.directory === b.directory && a.documentPath === b.documentPath
    && a.directoryDev === b.directoryDev && a.directoryIno === b.directoryIno
    && a.fileDev === b.fileDev && a.fileIno === b.fileIno && a.size === b.size
    && a.mtimeNs === b.mtimeNs && a.ctimeNs === b.ctimeNs && a.sha256 === b.sha256;
}
function savedFileVersionFacts(file: FileIdentity): SavedFileVersionFacts {
  return Object.freeze({
    dev: file.fileDev,
    ino: file.fileIno,
    size: file.size,
    sha256: file.sha256,
    mtime_ns: file.mtimeNs,
    ctime_ns: file.ctimeNs,
  });
}
function uuid(value: unknown): string | null { return typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value) ? value : null; }
function sameFile(left: string, right: string): boolean { try { return realpathSync(left) === realpathSync(right); } catch { return false; } }
function sameJson(left: unknown, right: unknown): boolean { return JSON.stringify(left) === JSON.stringify(right); }
function waitForClose(child: ChildLike, timeoutMs: number): Promise<boolean> {
  if (closedChildren.has(child)) return Promise.resolve(true);
  return new Promise((resolveClose) => {
    const timer = setTimeout(() => { child.removeListener('close', onClose); resolveClose(false); }, timeoutMs);
    const onClose = (): void => { clearTimeout(timer); resolveClose(true); };
    child.once('close', onClose);
  });
}

export function createOwnedDocumentLaunchRunner(): OwnedDocumentLaunchRunner {
  const helper = resolveTrustedHelper();
  return new ProductOwnedDocumentLaunchRunner(helper.path, nodeSpawn as unknown as SpawnLike, tmpdir());
}

/** @internal Test-only process seam; it does not alter the product helper path or command schema. */
export function createOwnedDocumentLaunchRunnerForTesting(
  spawnChild: SpawnLike,
  tempBase = tmpdir(),
  documentWriter?: DocumentWriter,
): OwnedDocumentLaunchRunner {
  const helper = resolveHelperCandidates().find((path) => {
    try { const stat = lstatSync(path); return stat.isFile() && !stat.isSymbolicLink() && (stat.mode & 0o111) !== 0; }
    catch { return false; }
  }) ?? resolve(process.cwd(), 'target/debug', HELPER_NAME);
  return new ProductOwnedDocumentLaunchRunner(helper, spawnChild, tempBase, documentWriter);
}
