import { spawn, type ChildProcess } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { existsSync, lstatSync, mkdirSync, mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import type { Duplex } from 'node:stream';

import {
  encodeFrame,
  EnvelopeFactory,
  FrameDecoder,
  FrameError,
  frameCapture,
  frameImageId,
  frameWatchId,
  browserTargetContextHash,
  type DecisionCancel,
  type DecisionProposal,
  type DecisionRequest,
  type DeviceEvent,
  type BrowserSourceControl,
  type BrowserPageSnapshot,
  type BrowserPageProfile,
  type BrowserReadScope,
  type BrowserRefreshRequest,
  type BrowserControlSubmit,
  type BrowserControlReply,
  type FrameMeta,
  type ImageAvailability,
  type ObservationImage,
  type Message,
  type OperatorCommand,
  type RunMode,
  type SourceHealth,
  type StopAck,
  type StopDisposition,
  type StopScope,
  type StopSource,
  type UiProjection,
} from '@computer-use/desktop-api';

import { ImageClient, imageAvailability, observationImage } from './imageClient.js';
import { ComputerImagePort, type ComputerImagePortSnapshot, type ConfirmComputerSafety } from './computerImagePort.js';
import { OwnedRevisitControl } from './ownedRevisitControl.js';
import { TeacherObservationDecoder, type TeacherObservation } from './teacherShadowObserver.js';
import { validateOwnedRevisitChallenge } from './ownedRevisitTypes.js';
import { BrowserReadBeforeFloorError, BrowserSourceController } from './headless/browserSource.js';
import { DeviceBrowserPort } from './headless/deviceBrowserPort.js';

import type { Binaries } from './binaries.js';
import { nativeSourceReadSettings, validateNativeSource, type NativeSourceOptions } from './nativeSource.js';
import { hostEnvironment, privateModelPrefix, type ModelAuthorization } from './modelBootstrap.js';
import { activateOwnedDocumentForObservation, encodeOwnedDocumentRevisitLineage,
  encodeOwnedDocumentSourceBootstrap, type OwnedDocumentRegistration } from './ownedDocumentLaunch.js';
import type { OwnedDocumentSourceBootstrap } from './ownedDocumentBootstrap.js';
import { consumeOwnedDocumentValueAuthorization, consumeOwnedDocumentSaveAuthorization,
  type OwnedDocumentValueAuthorization, type OwnedDocumentSaveAuthorization } from './ownedDocumentInput.js';

const OWNED_DOCUMENT_BUNDLE_ID = 'com.apple.TextEdit';
const OWNED_DOCUMENT_EXECUTABLE = '/System/Applications/TextEdit.app/Contents/MacOS/TextEdit';
const OWNED_DOCUMENT_BOOTSTRAP_KEYS = [
  'version', 'registration_id', 'pid', 'start_seconds', 'start_micros', 'executable_path', 'bundle_id',
  'document_path', 'document_sha256', 'document_dev', 'document_ino', 'directory_dev', 'directory_ino',
  'issued_unix_ms',
].sort();
const MAX_OWNED_DOCUMENT_BOOTSTRAP_BYTES = 8192;

export type Role = 'device' | 'source' | 'host';

export function browserControlAckError(messageType: Message['type'], code: unknown, reason: unknown): Error {
  if (messageType === 'browser_page_snapshot' && code === null && reason === 'browser_read_before_floor') {
    return new BrowserReadBeforeFloorError();
  }
  return new Error('Browser observation scope was rejected.');
}

export interface SupervisorOptions {
  binaries: Binaries;
  /** The mode the host starts in. It is the operator's to raise, never a model's. */
  mode?: RunMode;
  readyTimeoutMs?: number;
  /** Native observation; input requires an explicit fixed-window authorization. */
  nativeSource?: NativeSourceOptions;
  /** Main-only fixed owned page; the optional profile selects its Device resource. */
  browserSource?: { chromeExecutable: string; fixturePath: string };
  /** Selected only by trusted Main; renderer/model task proposals cannot set it. */
  browserPageProfile?: BrowserPageProfile;
  /** Main-only startup ownership evidence, before the browser can fail to launch. */
  observeBrowserSourceOwner?: (value: Readonly<{ supervisorDir: string; profileParent: string;
    pids: Readonly<Partial<Record<Role, number>>> }>) => void;
  /** Main-only identity for the owned temporary TextEdit document; never serialized into settings. */
  ownedDocument?: OwnedDocumentRegistration;
  /** Main-only explicit input grant, consumed by one observing startup. */
  ownedDocumentValueAuthorization?: OwnedDocumentValueAuthorization;
  /** Separate Main-only save consent; requires value-input consent for the same startup. */
  ownedDocumentSaveAuthorization?: OwnedDocumentSaveAuthorization;
  /** Explicit local pipeline diagnostic; never selected by renderer or as model fallback. */
  ownedDocumentReferencePlanner?: boolean;
  /** Main-only retained-Root diagnostic. Requires separate owned Save consent. */
  ownedDocumentRootRevisit?: boolean;
  /** Explicit cloud authorization supplied in memory by main. No env fallback. */
  modelAuthorization?: ModelAuthorization;
  /** Main-only native dialog for provider safety checks; never crosses to Host. */
  confirmComputerSafety?: ConfirmComputerSafety;
  /** Main-only observer, constructed only for a separately authorized S1 teacher. */
  teacherObservationFactory?: (nowMs: () => number) => TeacherObservationHandler;
  /** Main-only observation from startup onward; cannot alter incoming health. */
  observeSourceHealth?: (health: Readonly<SourceHealth>) => void;
  /** Main-only projection observation, including messages before start resolves. */
  observeProjection?: (projection: Readonly<UiProjection>) => void;
  /** Main-only read observation after Host admission ACK. */
  observeBrowserSnapshot?: (snapshot: Readonly<BrowserPageSnapshot>) => void;
  /** Main-only diagnostic copy of the private resource result; grants no authority. */
  observeBrowserExecution?: (value: Readonly<{ submission: BrowserControlSubmit;
    reply: BrowserControlReply }>) => void;
}

export interface TeacherObservationHandler {
  observe(message: TeacherObservation): void;
  close(): void | Promise<void>;
}

/** A teacher-only credential must not select the S2 planning backend. */
export function modelPlanningBackend(authorization: ModelAuthorization | undefined): 'model' | undefined {
  return authorization?.purposes.includes('s2_planning') ? 'model' : undefined;
}

/** Keep the deterministic fixture planner isolated from the production S2 backend. */
export function validateNativeReferencePlanningOptions(options: Pick<SupervisorOptions,
  'nativeSource' | 'modelAuthorization'>): void {
  if (options.nativeSource?.referencePlanner === true
    && modelPlanningBackend(options.modelAuthorization) === 'model') {
    throw new Error('native reference planning cannot be combined with S2 model planning');
  }
}

export function computerImagePortRequested(options: Pick<SupervisorOptions, 'modelAuthorization' | 'nativeSource'>): boolean {
  const authorization = options.modelAuthorization;
  return options.nativeSource?.authorizedWindowId !== undefined
    && authorization !== undefined
    && authorization.purposes.includes('computer_proposals')
    && typeof authorization.account_scope === 'string'
    && authorization.account_scope.trim().length > 0;
}

export function hostPrivateStdioLayout(privateControl: boolean, privateObservation: boolean,
  computerImage: boolean): ('pipe' | 'ignore')[] {
  const stdio: ('pipe' | 'ignore')[] = ['pipe', 'pipe', 'pipe'];
  if (privateControl || privateObservation || computerImage) {
    stdio[3] = privateControl ? 'pipe' : 'ignore';
    if (privateObservation || computerImage) stdio[4] = privateObservation ? 'pipe' : 'ignore';
    if (computerImage) stdio[5] = 'pipe';
  }
  return stdio;
}

/** Keep owned Save production planning bound to an explicitly approved S2 slot. */
export function validateOwnedDocumentPlanningOptions(options: Pick<SupervisorOptions,
  'ownedDocument' | 'ownedDocumentValueAuthorization' | 'ownedDocumentSaveAuthorization'
  | 'ownedDocumentReferencePlanner' | 'ownedDocumentRootRevisit' | 'modelAuthorization'>): void {
  const hasS2Planning = modelPlanningBackend(options.modelAuthorization) === 'model';
  const diagnosticReference = options.ownedDocumentReferencePlanner === true;

  if (options.ownedDocumentRootRevisit !== undefined
    && (options.ownedDocumentRootRevisit !== true || options.ownedDocument === undefined
      || options.ownedDocumentValueAuthorization === undefined
      || options.ownedDocumentSaveAuthorization === undefined
      || (diagnosticReference && options.modelAuthorization !== undefined)
      || (!diagnosticReference && !hasS2Planning))) {
    throw new Error('owned Root revisit requires owned Save grants and one explicit planner authorization');
  }
  if (options.ownedDocumentReferencePlanner !== undefined
    && (!diagnosticReference || options.ownedDocument === undefined
      || options.ownedDocumentValueAuthorization === undefined || options.modelAuthorization !== undefined)) {
    throw new Error('owned reference planning requires a separate owned input diagnostic authorization');
  }
  if (options.ownedDocumentSaveAuthorization !== undefined && options.ownedDocument === undefined) {
    throw new Error('owned Save requires a private document registration');
  }
  if (options.ownedDocumentSaveAuthorization !== undefined
    && options.ownedDocumentValueAuthorization === undefined) {
    throw new Error('owned document save requires separate value-input authorization');
  }
  if (options.ownedDocumentSaveAuthorization !== undefined && !diagnosticReference && !hasS2Planning) {
    throw new Error('owned Save requires explicit S2 planning or the isolated reference diagnostic');
  }
}

export interface SupervisorEvents {
  projection: [UiProjection];
  imageAvailability: [ImageAvailability];
  stopAck: [StopAck];
  deviceEvent: [DeviceEvent];
  decisionRequest: [DecisionRequest];
  decisionCancel: [DecisionCancel];
  exit: [role: Role, code: number | null, signal: NodeJS.Signals | null];
  log: [line: string];
}

interface Child {
  role: Role;
  process: ChildProcess;
  factory: EnvelopeFactory;
  decoder: FrameDecoder;
  exited: Promise<void>;
  open: boolean;
  expectedExit: boolean;
  teacherObservation?: { decoder: TeacherObservationDecoder; handler: TeacherObservationHandler; stream: Duplex };
  computerImagePort?: ComputerImagePort;
}

const token = (): string => randomBytes(16).toString('hex');

function decodeOwnedDocumentSourcePrefix(prefix: Buffer): OwnedDocumentSourceBootstrap {
  if (prefix.byteLength < 5 || prefix.byteLength > MAX_OWNED_DOCUMENT_BOOTSTRAP_BYTES + 4
    || prefix.readUInt32BE(0) !== prefix.byteLength - 4
    || prefix.readUInt32BE(0) > MAX_OWNED_DOCUMENT_BOOTSTRAP_BYTES) {
    throw new Error('owned document source bootstrap frame is invalid');
  }
  let value: unknown;
  try { value = JSON.parse(prefix.subarray(4).toString('utf8')); }
  catch { throw new Error('owned document source bootstrap JSON is invalid'); }
  if (!value || typeof value !== 'object' || Array.isArray(value)
    || Object.keys(value).sort().join(',') !== OWNED_DOCUMENT_BOOTSTRAP_KEYS.join(',')) {
    throw new Error('owned document source bootstrap fields are invalid');
  }
  const claims = value as OwnedDocumentSourceBootstrap;
  if (claims.version !== 1 || typeof claims.registration_id !== 'string'
    || !Number.isSafeInteger(claims.pid) || claims.pid <= 0
    || !Number.isSafeInteger(claims.start_seconds) || claims.start_seconds <= 0
    || !Number.isInteger(claims.start_micros) || claims.start_micros < 0 || claims.start_micros >= 1_000_000
    || typeof claims.executable_path !== 'string' || typeof claims.bundle_id !== 'string'
    || typeof claims.document_path !== 'string' || typeof claims.document_sha256 !== 'string'
    || !Number.isSafeInteger(claims.document_dev) || !Number.isSafeInteger(claims.document_ino)
    || !Number.isSafeInteger(claims.directory_dev) || !Number.isSafeInteger(claims.directory_ino)
    || !Number.isSafeInteger(claims.issued_unix_ms) || claims.issued_unix_ms <= 0) {
    throw new Error('owned document source bootstrap values are invalid');
  }
  return claims;
}

function prepareOwnedDocumentSourcePrefix(
  registration: OwnedDocumentRegistration,
  options: Omit<SupervisorOptions, 'ownedDocument'>,
): Buffer {
  const native = options.nativeSource;
  if ((options.mode !== undefined && options.mode !== 'observe') || !native
    || native.bundleId !== OWNED_DOCUMENT_BUNDLE_ID || native.pid === undefined
    || native.authorizedWindowId !== undefined) {
    throw new Error('owned document source bootstrap requires TextEdit observation mode');
  }
  const expectedHelper = resolve(dirname(options.binaries.source), 'cu-macos-source');
  if (native.helperPath !== expectedHelper) throw new Error('owned document source requires the fixed product source helper');
  let helperStat;
  try { helperStat = lstatSync(expectedHelper); }
  catch { throw new Error('owned document source helper is unavailable'); }
  if (!helperStat.isFile() || helperStat.isSymbolicLink() || (helperStat.mode & 0o111) === 0
    || realpathSync(expectedHelper) !== expectedHelper) {
    throw new Error('owned document source helper must be a canonical regular executable');
  }

  const prefix = encodeOwnedDocumentSourceBootstrap(registration);
  try {
    const claims = decodeOwnedDocumentSourcePrefix(prefix);
    if (native.pid !== claims.pid || native.bundleId !== claims.bundle_id
      || claims.executable_path !== OWNED_DOCUMENT_EXECUTABLE) {
      throw new Error('native source target does not match the owned document registration');
    }
    return prefix;
  } catch (error) {
    prefix.fill(0);
    throw error;
  }
}

/**
 * Electron main's part in the native processes (design 3.1 and 4.6): it starts
 * them with private pipes, hands each its configuration and one-time credential
 * over its pipe, relays decisions, and sends stop commands to the device
 * directly. Losing main closes every pipe, and every process then leaves on its
 * own.
 *
 * It has no Electron imports, so it runs, and is tested, as plain Node against
 * the real native binaries.
 */
export class Supervisor extends EventEmitter<SupervisorEvents> {
  readonly dir: string;
  private readonly clockOriginMs = Date.now();
  private readonly children = new Map<Role, Child>();
  private latest: UiProjection | null = null;
  private projectionAt = 0;
  private imageClient: ImageClient | null = null;
  private imageControl: FrameMeta | null = null;
  private captureControl: FrameMeta | null = null;
  private imageTimer: ReturnType<typeof setInterval> | null = null;
  private deviceLeaseTimer: ReturnType<typeof setInterval> | null = null;
  private lastImageAvailability: ImageAvailability = { frame_id: null, expires_unix_ms: null };
  private ready = new Map<Role, () => void>();
  private shuttingDown = false;
  private shutdownTask: Promise<void> | null = null;
  private modelPrefix: Buffer | null = null;
  private ownedDocumentRegistration: OwnedDocumentRegistration | null;
  private ownedRevisitControl: OwnedRevisitControl | null = null;
  private sessionEpoch = 1;
  private readonly initialOwnedIdentity: OwnedDocumentSourceBootstrap | null;
  private browserSource: BrowserSourceController | null = null;
  private preparedBrowserScope: BrowserReadScope | null = null;
  private preparedBrowserContextHash: string | null = null;
  private deviceBrowserPort: DeviceBrowserPort | null = null;
  private browserRefreshTask: Promise<void> | null = null;
  private browserTimer: ReturnType<typeof setInterval> | null = null;
  private browserStopTask: Promise<void> | null = null;
  private browserControlWaiters = new Map<string, { host: Child; messageType: Message['type']; resolve: () => void;
    reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  private ownedRevisitTask: Promise<OwnedDocumentRegistration> | null = null;
  private revisitInProgress = false;
  private revisitCancelled = false;

  private constructor(
    private readonly options: SupervisorOptions,
    private readonly tokens: { device: string; source: string; image: string },
    ownedDocumentRegistration: OwnedDocumentRegistration | null,
    private readonly ownedDocumentInputAuthorized: boolean,
    private readonly ownedDocumentSaveAuthorized: boolean,
  ) {
    super();
    this.ownedDocumentRegistration = ownedDocumentRegistration;
    if (ownedDocumentRegistration) {
      const prefix = prepareOwnedDocumentSourcePrefix(ownedDocumentRegistration, options);
      try { this.initialOwnedIdentity = decodeOwnedDocumentSourcePrefix(prefix); }
      finally { prefix.fill(0); }
    } else this.initialOwnedIdentity = null;
    try {
      if (options.modelAuthorization) this.modelPrefix = privateModelPrefix(options.modelAuthorization);
    } catch (error) {
      this.ownedDocumentRegistration = null;
      throw error;
    }
    // Short, because a Unix socket path has a small limit.
    this.dir = mkdtempSync(join(tmpdir(), 'cu'));
    if (options.nativeSource) mkdirSync(join(this.dir, 'native-assets'), { mode: 0o700 });
  }

  static async start(options: SupervisorOptions): Promise<Supervisor> {
    if (options.browserPageProfile !== undefined
      && (options.browserPageProfile.kind !== 'fixture_background_option' || !options.browserSource)) {
      throw new Error('Browser execution requires the fixed owned page profile.');
    }
    if (options.browserSource && (options.nativeSource || options.ownedDocument || options.modelAuthorization
      || options.ownedDocumentValueAuthorization || options.ownedDocumentSaveAuthorization
      || options.ownedDocumentReferencePlanner || options.ownedDocumentRootRevisit
      || (!options.browserPageProfile && options.mode !== undefined && options.mode !== 'observe'))) {
      throw new Error('Owned browser observation requires its own observe-only session.');
    }
    const { ownedDocument, ownedDocumentValueAuthorization, ownedDocumentSaveAuthorization, ...baseOptions } = options;
    const validated = { ...baseOptions, ...(options.nativeSource ? { nativeSource: validateNativeSource(options.nativeSource) } : {}) };
    validateNativeReferencePlanningOptions(options);
    validateOwnedDocumentPlanningOptions(options);
    if (validated.nativeSource?.backgroundWindowId !== undefined
      && (ownedDocument !== undefined || ownedDocumentValueAuthorization !== undefined
        || ownedDocumentSaveAuthorization !== undefined || options.ownedDocumentReferencePlanner === true
        || options.ownedDocumentRootRevisit === true)) {
      throw new Error('Background AX observation cannot be mixed with owned-document authorization.');
    }
    if (options.modelAuthorization?.purposes.includes('computer_proposals') === true
      && (!validated.nativeSource || !validated.nativeSource.authorizedWindowId
        || !validated.nativeSource.pid || !options.modelAuthorization.account_scope
        || typeof options.confirmComputerSafety !== 'function')) {
      throw new Error('Computer proposals require selecting a native window and authorizing it for this session first.');
    }
    if (ownedDocument !== undefined) {
      const checked = prepareOwnedDocumentSourcePrefix(ownedDocument, validated);
      checked.fill(0);
    }
    if (ownedDocumentValueAuthorization !== undefined) {
      consumeOwnedDocumentValueAuthorization(ownedDocumentValueAuthorization, ownedDocument);
      const guard = resolve(dirname(options.binaries.device), 'cu-macos-device-guard');
      const selected = validated.nativeSource?.guardHelperPath ?? guard;
      const stat = lstatSync(selected);
      if (selected !== guard || !stat.isFile() || stat.isSymbolicLink() || (stat.mode & 0o111) === 0
        || realpathSync(selected) !== selected) throw new Error('owned input requires the fixed product device guard');
    }
    if (ownedDocumentSaveAuthorization !== undefined) {
      consumeOwnedDocumentSaveAuthorization(ownedDocumentSaveAuthorization, ownedDocument);
      if (ownedDocumentValueAuthorization === undefined) {
        throw new Error('owned document save requires separate value-input authorization');
      }
    }
    const supervisor = new Supervisor(validated, { device: token(), source: token(), image: token() }, ownedDocument ?? null,
      ownedDocumentValueAuthorization !== undefined, ownedDocumentSaveAuthorization !== undefined);
    try {
      if (options.browserPageProfile) await supervisor.prepareBrowserSource();
      await supervisor.launch();
      if (options.browserSource) await supervisor.startBrowserSource();
    } catch (error) {
      await supervisor.shutdown();
      throw error;
    }
    return supervisor;
  }

  get projection(): UiProjection | null {
    return this.latest;
  }

  get clockOriginUnixMs(): number { return this.clockOriginMs; }

  get hostPid(): number | undefined {
    return this.children.get('host')?.process.pid;
  }

  get devicePid(): number | undefined {
    return this.children.get('device')?.process.pid;
  }

  /** Every process that was started, for tests that check nothing is left behind. */
  get pids(): Record<Role, number | undefined> {
    return {
      host: this.children.get('host')?.process.pid,
      device: this.children.get('device')?.process.pid,
      source: this.children.get('source')?.process.pid,
    };
  }

  get worldLogPath(): string {
    return join(this.dir, 'world.log');
  }

  private settings(extra: Record<string, string>): Record<string, string> {
    return {
      socket_dir: this.dir,
      clock_origin_ms: String(this.clockOriginMs),
      world_log: this.worldLogPath,
      ...extra,
    };
  }

  private async launch(): Promise<void> {
    const timeout = this.options.readyTimeoutMs ?? 15_000;
    const native = this.options.nativeSource;
    const guardPath = native?.guardHelperPath ?? join(dirname(this.options.binaries.device), 'cu-macos-device-guard');
    if ((native?.authorizedWindowId || native?.authorizedBackgroundWindowId) && !existsSync(guardPath)) {
      throw new Error('authorized native editing requires the device guard');
    }
    await this.spawn('device', this.options.binaries.device, 1, this.settings({
      host_token: this.tokens.device,
      ...(this.ownedDocumentSaveAuthorized ? { owned_save: '1' } : {}),
      ...(native ? { backend: this.ownedDocumentInputAuthorized ? 'macos-owned-value'
        : native.backgroundWindowId ? 'macos-background-ax' : native.authorizedWindowId ? 'macos-ax-press' : 'macos-observe',
        ...(existsSync(guardPath) ? { native_guard_path: guardPath } : {}),
        ...(native.backgroundWindowId ? {
          native_target_pid: String(native.pid), native_target_bundle: native.bundleId,
          native_target_window_id: native.backgroundWindowId,
          ...(native.authorizedBackgroundWindowId && native.authorizedProcessIdentity ? {
            native_target_process_start_seconds: String(native.authorizedProcessIdentity.start_seconds),
            native_target_process_start_micros: String(native.authorizedProcessIdentity.start_micros),
          } : {}),
        } : native.authorizedWindowId ? {
          native_target_pid: String(native.pid), native_target_bundle: native.bundleId,
          native_target_window_id: native.authorizedWindowId,
          ...(native.authorizedProcessIdentity ? {
            native_target_process_start_seconds: String(native.authorizedProcessIdentity.start_seconds),
            native_target_process_start_micros: String(native.authorizedProcessIdentity.start_micros),
          } : {}),
        } : {}),
      } : {}),
    }), timeout);
    if (native || this.options.browserPageProfile) {
      const renew = (): void => {
        const device = this.children.get('device');
        if (device) this.write(device, { type: 'lease_renew', payload: { lease_epoch: this.sessionEpoch, valid_for_ms: 750 } });
      };
      renew();
      this.deviceLeaseTimer = setInterval(renew, 250);
      this.deviceLeaseTimer.unref();
    }
    await this.spawn('source', this.options.binaries.source, 1, this.settings({
      host_token: this.tokens.source,
      ...(native ? {
        source_backend: 'native',
        image_token: this.tokens.image,
        native_helper_path: native.helperPath,
        target_bundle_id: native.bundleId,
        native_assets_dir: join(this.dir, 'native-assets'),
        ...(native.pid === undefined ? {} : { target_pid: String(native.pid) }),
        ...nativeSourceReadSettings(native),
      } : {}),
    }), timeout);
    if (native) {
      await this.startNativeImages(native, 1);
    }
    await this.spawn(
      'host',
      this.options.binaries.host,
      1,
      this.settings({
        db_path: join(this.dir, 'host.db'),
        device_token: this.tokens.device,
        device_epoch: '1',
        source_token: this.tokens.source,
        initial_mode: this.options.mode ?? 'observe',
        ...(this.options.browserPageProfile ? { planning_backend: 'browser_reference' } : {}),
        ...(this.ownedDocumentSaveAuthorized ? { owned_save: '1' } : {}),
        ...(native ? { native_target_bundle: native.bundleId, planning_backend: native.referencePlanner ? 'native_reference' : 'unconfigured',
          ...(native.pid !== undefined ? { native_target_pid: String(native.pid) } : {}),
          ...(native.authorizedWindowId ? { native_target_window_id: native.authorizedWindowId } : {}),
          ...(native.backgroundWindowId ? { native_background_window_id: native.backgroundWindowId,
            ...(native.authorizedBackgroundWindowId && native.authorizedProcessIdentity ? {
              native_target_process_start_seconds: String(native.authorizedProcessIdentity.start_seconds),
              native_target_process_start_micros: String(native.authorizedProcessIdentity.start_micros),
            } : {}),
          } : {}),
        } : {}),
        ...(modelPlanningBackend(this.options.modelAuthorization) ? { planning_backend: 'model' } : {}),
        ...(this.options.ownedDocumentReferencePlanner ? { planning_backend: 'owned_reference' } : {}),
      }),
      timeout,
    );
  }

  private async startNativeImages(native: NativeSourceOptions, epoch: number): Promise<void> {
    this.imageClient = new ImageClient(() => this.notifyImageAvailability(), reason => this.emit('log', `image: ${reason}`));
    try { await this.imageClient.start(join(this.dir, 'images.sock'), this.tokens.image, epoch, native.pid, this.clockOriginMs); }
    catch {
      this.emit('log', 'image preview unavailable');
      await this.imageClient.close(); this.imageClient = null;
    }
    this.imageTimer = setInterval(() => this.notifyImageAvailability(), 100);
    this.imageTimer.unref();
  }

  private async spawn(
    role: Role,
    program: string,
    sessionEpoch: number,
    settings: Record<string, string>,
    timeoutMs: number,
    includeOwnedBootstrap = true,
  ): Promise<void> {
    if (this.options.browserPageProfile && this.preparedBrowserScope) {
      // Leave the existing five-second Host control bound inside the separate
      // thirty-second scope-install window. Read evidence still expires in 2s.
      const remaining = this.preparedBrowserScope.issued_ms + 25_000
        - (Date.now() - this.clockOriginMs);
      if (remaining <= 0) throw new Error('Browser startup scope expired.');
      timeoutMs = Math.min(timeoutMs, remaining);
    }
    const modelPrefix = role === 'host' ? this.modelPrefix : null;
    const privateControl = role === 'host' && this.options.ownedDocumentRootRevisit === true;
    const privateObservation = role === 'host' && this.options.teacherObservationFactory !== undefined
      && this.options.modelAuthorization?.purposes.includes('s1_teacher') === true;
    const privateComputerImage = role === 'host' && computerImagePortRequested(this.options);
    const privateBrowser = role === 'device' && this.options.browserPageProfile !== undefined;
    const computerAuthorization = this.options.modelAuthorization;
    const computerSource = this.options.nativeSource;
    const confirmComputerSafety = this.options.confirmComputerSafety;
    if (privateComputerImage && (!computerAuthorization?.account_scope || !computerSource?.pid
      || !computerSource.authorizedWindowId || !confirmComputerSafety)) {
      throw new Error('Computer safety confirmation requires the current authorized model and native window scope.');
    }
    // Each private peer gets a newly issued registration, independently of
    // startup waits or buffers already consumed by another peer.
    const ownedPrefix = includeOwnedBootstrap && this.ownedDocumentRegistration
      ? prepareOwnedDocumentSourcePrefix(this.ownedDocumentRegistration, this.options) : null;
    const args = [
      ...(ownedPrefix ? ['--owned-document-bootstrap'] : []),
      ...(modelPrefix ? ['--model-bootstrap'] : []),
      ...(privateControl ? ['--owned-revisit-control'] : []),
      ...(privateObservation ? ['--teacher-observation'] : []),
      ...(privateComputerImage ? ['--computer-image'] : []),
      ...(privateBrowser ? ['--browser-control'] : []),
    ];
    const process = spawn(program, args, {
      stdio: privateBrowser ? ['pipe', 'pipe', 'pipe', 'pipe']
        : hostPrivateStdioLayout(privateControl, privateObservation, privateComputerImage),
      ...(role === 'host' ? { env: hostEnvironment(globalThis.process.env) } : {}),
    });
    const child: Child = {
      role,
      process,
      factory: new EnvelopeFactory('supervisor', () => Date.now() - this.clockOriginMs),
      decoder: new FrameDecoder(),
      open: true,
      expectedExit: false,
      exited: new Promise((done) => {
        process.once('close', (code, signal) => {
          child.open = false;
          this.closeTeacherObservation(child);
          child.computerImagePort?.close();
          if (role === 'source') this.closeComputerImagePort();
          if (role === 'device') this.retireDeviceBrowserPort('source_unavailable');
          if (role === 'host') {
            this.rejectBrowserRequests(child);
            if (this.children.get(role) === child) void this.stopBrowserSource().catch(() => {
              this.emit('log', 'browser page observation cleanup failed');
            });
          }
          if (!this.shuttingDown && !child.expectedExit && this.children.get(role) === child) this.emit('exit', role, code, signal);
          done();
        });
      }),
    };
    this.children.set(role, child);
    let privateBrowserReady: Promise<void> | null = null;
    if (privateBrowser) {
      const stream = process.stdio[3] as Duplex | null;
      const scope = this.preparedBrowserScope;
      const resource = this.browserSource;
      if (!stream || !scope || !resource || process.pid === undefined) {
        throw new Error('Private browser Device channel is unavailable.');
      }
      const port = new DeviceBrowserPort({ stream, device: process, scope, sessionEpoch,
        channelGeneration: 1, browserPageProfile: this.options.browserPageProfile!,
        nowMs: () => Date.now() - this.clockOriginMs,
        resource: {
          submit: async submission => {
            const reply = await resource.executeDeviceSubmission(submission);
            this.observe(this.options.observeBrowserExecution, { submission, reply });
            return reply;
          },
          retireNow: reason => resource.retireForActionOutcome(reason),
        },
      });
      this.deviceBrowserPort = port;
      // Bootstrap writes start now. Configure follows without waiting for the
      // Hello: Device may need that public profile before authenticating fd3.
      privateBrowserReady = port.start();
      void privateBrowserReady.catch(() => undefined);
    }
    if (privateControl) {
      const pipe = process.stdio[3] as Duplex | null;
      if (!pipe || typeof pipe.write !== 'function') throw new Error('owned Root revisit control unavailable');
      this.ownedRevisitControl = new OwnedRevisitControl(pipe);
    }
    if (privateObservation) this.attachTeacherObservation(child);
    if (privateComputerImage) {
      const pipe = (process.stdio as unknown as Array<Duplex | null>)[5] ?? null;
      if (!pipe || typeof pipe.write !== 'function') throw new Error('private computer image port unavailable');
      child.computerImagePort = new ComputerImagePort(pipe, {
        clockOriginMs: this.clockOriginMs,
        targetBundleId: this.options.nativeSource!.bundleId,
        targetPid: computerSource!.pid!,
        targetWindowId: computerSource!.authorizedWindowId!,
        isComputerAuthorizationCurrent: () => {
          const currentAuthorization = this.options.modelAuthorization;
          const currentSource = this.options.nativeSource;
          return currentAuthorization !== undefined
            && currentAuthorization.purposes.includes('computer_proposals')
            && currentAuthorization.account_scope === computerAuthorization!.account_scope
            && currentSource?.bundleId === computerSource!.bundleId
            && currentSource.pid === computerSource!.pid
            && currentSource.authorizedWindowId === computerSource!.authorizedWindowId;
        },
        confirmComputerSafety: confirmComputerSafety!,
        current: () => this.currentComputerImageSnapshot(),
      });
    }
    process.stdin?.on('error', () => undefined); // a child that already left: nothing to tell it
    process.stderr?.setEncoding('utf8');
    process.stderr?.on('data', (text: string) => {
      for (const line of text.split('\n')) if (line.trim()) this.emit('log', `${role}: ${line}`);
    });
    process.stdout?.on('data', (chunk: Buffer) => this.onData(child, chunk));
    const ready = new Promise<void>((done, fail) => {
      const timer = setTimeout(() => fail(new Error(`${role} did not become ready`)), timeoutMs);
      this.ready.set(role, () => {
        clearTimeout(timer);
        done();
      });
      process.once('error', (error) => {
        clearTimeout(timer);
        fail(error);
      });
    });
    if (ownedPrefix) {
      try {
        process.stdin!.write(ownedPrefix, (error) => {
          ownedPrefix.fill(0);
          if (error) process.kill('SIGKILL');
        });
      } catch (error) {
        ownedPrefix.fill(0);
        throw error;
      }
    }
    if (modelPrefix) {
      // Node owns the queued buffer until the write callback. Wipe it only
      // there; wiping immediately can send an all-zero credential to host.
      process.stdin!.write(modelPrefix, () => modelPrefix.fill(0));
      this.modelPrefix = null;
      // Do not retain the original plaintext key in the session options.
      delete this.options.modelAuthorization;
    }
    this.write(child, { type: 'configure', payload: { role, session_epoch: sessionEpoch, settings,
      browser_page_profile: role === 'source' ? null : this.options.browserPageProfile ?? null } });
    await Promise.all([ready, privateBrowserReady]);
  }

  private write(child: Child, message: Message): void {
    if (!child.open || !child.process.stdin?.writable) return;
    child.process.stdin.write(encodeFrame(child.factory.make(message)));
  }

  private attachTeacherObservation(child: Child): void {
    const stream = child.process.stdio[4] as Duplex | null;
    if (!stream || !this.options.teacherObservationFactory) return;
    try {
      const handler = this.options.teacherObservationFactory(() => Date.now() - this.clockOriginMs);
      const decoder = new TeacherObservationDecoder();
      child.teacherObservation = { handler, decoder, stream };
      stream.on('data', (chunk: Buffer) => {
        if (this.shuttingDown || !child.open || child.expectedExit || this.children.get('host') !== child) {
          this.closeTeacherObservation(child); return;
        }
        try {
          for (const message of decoder.push(chunk)) {
            if (!child.teacherObservation || this.shuttingDown || !child.open || child.expectedExit
              || this.children.get('host') !== child) break;
            handler.observe(message);
          }
        } catch { this.closeTeacherObservation(child); }
      });
      stream.once('error', () => this.closeTeacherObservation(child));
      stream.once('end', () => this.closeTeacherObservation(child));
    } catch {
      stream.on('error', () => undefined);
      stream.destroy();
    }
  }

  private closeTeacherObservation(child: Child): void {
    const observation = child.teacherObservation;
    if (!observation) return;
    delete child.teacherObservation;
    observation.decoder.dispose();
    observation.stream.removeAllListeners('data');
    observation.stream.destroy();
    try { void Promise.resolve(observation.handler.close()).catch(() => undefined); }
    catch { /* Optional observation never changes control or teacher admission. */ }
  }

  private onData(child: Child, chunk: Buffer): void {
    // A replaced peer cannot publish a Ready, frame, decision or receipt into
    // the current generation, including bytes already buffered by Node.
    if (this.shuttingDown || !child.open || child.expectedExit || this.children.get(child.role) !== child) return;
    child.decoder.push(chunk);
    try {
      for (let envelope = child.decoder.next(); envelope; envelope = child.decoder.next()) {
        if (this.shuttingDown || !child.open || child.expectedExit || this.children.get(child.role) !== child) return;
        if (child.role === 'host' && envelope.message.type === 'command_ack' && envelope.in_reply_to) {
          const pending = this.browserControlWaiters.get(envelope.in_reply_to);
          if (pending?.host === child) {
            clearTimeout(pending.timer); this.browserControlWaiters.delete(envelope.in_reply_to);
            if (envelope.message.payload.accepted) pending.resolve();
            else pending.reject(browserControlAckError(pending.messageType,
              envelope.message.payload.code, envelope.message.payload.reason));
            continue;
          }
        }
        this.onMessage(child.role, envelope.message);
      }
    } catch (error) {
      if (error instanceof FrameError) {
        // There is no telling where the next frame starts: the connection is dropped.
        this.emit('log', `${child.role}: dropped after a bad frame: ${error.message}`);
        child.process.kill('SIGKILL');
        return;
      }
      throw error;
    }
  }

  private onMessage(role: Role, message: Message): void {
    switch (message.type) {
      case 'ready':
        this.ready.get(role)?.();
        break;
      case 'ui_projection':
        if (role !== 'host') break;
        this.latest = message.payload;
        this.projectionAt = Date.now();
        this.notifyImageAvailability();
        this.observe(this.options.observeProjection, message.payload);
        this.emit('projection', message.payload);
        break;
      case 'frame_meta':
        if (role === 'source') {
          if (frameCapture(message.payload)) this.captureControl = message.payload;
          else {
            if (this.captureControl && frameWatchId(this.captureControl) !== frameWatchId(message.payload)) this.captureControl = null;
            this.imageControl = message.payload;
          }
          this.notifyImageAvailability();
        }
        break;
      case 'source_health':
        if (role === 'source' && message.payload.state !== 'available'
          && message.payload.source === 'screenshot') {
          this.imageControl = null; this.captureControl = null; this.notifyImageAvailability();
          this.invalidateComputerImagePort('source_unavailable');
        }
        if (role === 'source') this.observe(this.options.observeSourceHealth, message.payload);
        break;
      case 'decision_request':
        this.emit('decisionRequest', message.payload);
        break;
      case 'decision_cancel':
        this.emit('decisionCancel', message.payload);
        break;
      case 'stop_ack':
        this.emit('stopAck', message.payload);
        break;
      case 'device_event':
        this.emit('deviceEvent', message.payload);
        break;
      case 'browser_refresh_request':
        if (role === 'host') this.refreshBrowserForHost(message.payload);
        break;
      default:
        break;
    }
  }

  private observe<T>(observer: ((value: Readonly<T>) => void) | undefined, value: T): void {
    if (!observer) return;
    try { observer(structuredClone(value)); }
    catch { this.emit('log', 'startup observation callback failed'); }
  }

  private currentImageAvailability(): ImageAvailability {
    const control = this.selectedImageControl();
    return imageAvailability(this.imageClient?.packetFor(control ? frameImageId(control) : null) ?? null, control, this.latest, this.clockOriginMs, this.projectionAt);
  }
  private selectedImageControl(): FrameMeta | null {
    const imageId = this.latest?.observation?.frame?.frame_id;
    return this.captureControl && imageId === frameImageId(this.captureControl) ? this.captureControl : this.imageControl;
  }
  private notifyImageAvailability(): void {
    if (this.shuttingDown) return;
    const available = this.currentImageAvailability();
    if (available.frame_id !== this.lastImageAvailability.frame_id || available.expires_unix_ms !== this.lastImageAvailability.expires_unix_ms) {
      this.lastImageAvailability = available; this.emit('imageAvailability', available);
    }
  }
  image(frameId: string): ObservationImage | null {
    return observationImage(this.imageClient?.packetFor(frameId) ?? null, this.currentImageAvailability(), frameId);
  }

  private currentComputerImageSnapshot(): ComputerImagePortSnapshot {
    return {
      projection: this.latest,
      projectionAt: this.projectionAt,
      imageControl: this.selectedImageControl(),
      imagePacket: this.imageClient?.packetFor(this.latest?.observation?.frame?.frame_id) ?? null,
    };
  }

  private invalidateComputerImagePort(reason: 'source_unavailable' | 'stale_fence'): void {
    this.children.get('host')?.computerImagePort?.invalidate(reason);
  }

  private closeComputerImagePort(): void {
    const host = this.children.get('host');
    host?.computerImagePort?.close();
    if (host) delete host.computerImagePort;
  }

  // ---- What main can do ------------------------------------------------------------------

  private toHost(message: Message): void {
    const host = this.children.get('host');
    if (host) this.write(host, message);
  }

  operator(command: OperatorCommand): void {
    if (this.options.browserSource && (command.command === 'create_task' || command.command === 'confirm_goal'
      || command.command === 'resume_task' || (command.command === 'set_mode' && command.mode !== 'observe'))
      && (!this.options.browserPageProfile || this.deviceBrowserPort?.isReady() !== true
        || this.browserSource?.getStatus().phase !== 'ready')) {
      throw new Error('Browser input is unavailable in this observation session.');
    }
    if (this.revisitInProgress && !this.revisitCancelled) this.cancelOwnedRevisit();
    this.toHost({ type: 'operator_command', payload: command });
  }

  sendDecisionProposal(proposal: DecisionProposal): void {
    this.toHost({ type: 'decision_proposal', payload: proposal });
  }

  /**
   * A stop, straight to the device. It does not pass through the host, which may
   * be slow or stuck, nor through the renderer, which may be frozen.
   */
  stopAtDevice(scope: StopScope, disposition: StopDisposition, source: StopSource, reason = 'the operator pressed stop'): void {
    // Revocation starts synchronously; browser cleanup never delays native Stop.
    void this.stopBrowserSource().catch(() => this.emit('log', 'browser observation cleanup failed'));
    if (this.revisitInProgress && !this.revisitCancelled) this.cancelOwnedRevisit();
    this.closeComputerImagePort();
    const device = this.children.get('device');
    if (device) this.write(device, { type: 'stop_command', payload: { scope, disposition, source, reason } });
    const host = this.children.get('host');
    if (host) this.closeTeacherObservation(host);
  }

  injectHumanInput(): void {
    const device = this.children.get('device');
    if (device) this.write(device, { type: 'sim_control', payload: { op: { op: 'inject_human_input' } } });
  }

  /** Main-only lifecycle: the Host, frozen Root and database remain alive. */
  revisitOwnedRoot(): Promise<OwnedDocumentRegistration> {
    if (this.ownedRevisitTask || !this.ownedRevisitControl || !this.ownedDocumentRegistration
      || !this.initialOwnedIdentity || !this.options.ownedDocumentRootRevisit || this.shuttingDown) {
      return Promise.reject(new Error('owned Root revisit unavailable'));
    }
    this.revisitInProgress = true;
    this.ownedRevisitTask = this.runOwnedRootRevisit().finally(() => { this.revisitInProgress = false; });
    return this.ownedRevisitTask;
  }

  private cancelOwnedRevisit(): void {
    this.revisitCancelled = true;
    this.ownedRevisitControl?.close();
  }

  private async runOwnedRootRevisit(): Promise<OwnedDocumentRegistration> {
    const control = this.ownedRevisitControl!;
    const original = this.ownedDocumentRegistration!;
    const host = this.children.get('host');
    let transition: string | null = null;
    try {
      const response = await control.prepare();
      const challenge = validateOwnedRevisitChallenge(response.challenge, this.initialOwnedIdentity!,
        this.latest?.root ?? null, this.sessionEpoch, Date.now() - this.clockOriginMs);
      transition = challenge.transition_id;
      const checkpoint = (): void => {
        if (this.shuttingDown || this.revisitCancelled || this.children.get('host') !== host || !host?.open
          || Date.now() - this.clockOriginMs >= challenge.deadline_ms) throw new Error('owned Root revisit expired');
      };
      checkpoint();
      const saved = original.captureSavedVersion({ sha256: challenge.expected_sha256, byteLength: challenge.expected_byte_length });
      await this.stopOwnedPeers(); checkpoint();
      const ticket = await saved.retainForRevisit(); checkpoint();
      const registration = await ticket.reopen();
      // Keep the opaque token for shutdown even if activation or any later
      // stage fails. This does not create a fresh input authorization.
      this.ownedDocumentRegistration = registration;
      checkpoint();
      const reopenBoundary = Date.now() - this.clockOriginMs;
      if (!await activateOwnedDocumentForObservation(registration)) throw new Error('owned revisit activation unavailable');
      checkpoint();
      const prefix = encodeOwnedDocumentSourceBootstrap(registration);
      let claims: OwnedDocumentSourceBootstrap;
      try { claims = decodeOwnedDocumentSourcePrefix(prefix); } finally { prefix.fill(0); }
      if (claims.registration_id === this.initialOwnedIdentity!.registration_id
        || (claims.pid === this.initialOwnedIdentity!.pid && claims.start_seconds === this.initialOwnedIdentity!.start_seconds
          && claims.start_micros === this.initialOwnedIdentity!.start_micros)
        || claims.document_sha256 !== challenge.expected_sha256) throw new Error('owned revisit identity invalid');
      const fileLineage = encodeOwnedDocumentRevisitLineage(ticket, registration);
      this.sessionEpoch = challenge.expected_device_session_epoch;
      this.tokens.device = token(); this.tokens.source = token(); this.tokens.image = token();
      this.options.nativeSource = validateNativeSource({ ...this.options.nativeSource!, pid: claims.pid });
      const native = this.options.nativeSource;
      const timeout = Math.min(this.options.readyTimeoutMs ?? 12_000,
        challenge.deadline_ms - (Date.now() - this.clockOriginMs));
      await this.spawn('device', this.options.binaries.device, this.sessionEpoch, this.settings({
        host_token: this.tokens.device, backend: 'macos-observe',
      }), timeout, false);
      checkpoint();
      await this.spawn('source', this.options.binaries.source, this.sessionEpoch, this.settings({
        host_token: this.tokens.source, source_backend: 'native', image_token: this.tokens.image,
        native_helper_path: native.helperPath, target_bundle_id: native.bundleId,
        native_assets_dir: join(this.dir, 'native-assets'), target_pid: String(claims.pid),
      }), Math.min(timeout, challenge.deadline_ms - (Date.now() - this.clockOriginMs)));
      checkpoint();
      await control.finish({
        version: 1, transition_id: transition, root_task_id: challenge.root_task_id,
        task_revision: challenge.task_revision, root_epoch: challenge.root_epoch,
        new_registration: claims, file_lineage: fileLineage,
        new_device_session_epoch: this.sessionEpoch, reopen_boundary_ms: reopenBoundary,
        clock_domain: challenge.clock_domain,
      }, this.tokens.device, this.tokens.source,
      Math.min(10_000, challenge.deadline_ms - (Date.now() - this.clockOriginMs)));
      // The matching acknowledgement has already committed the lineage at
      // Host. Its delivery may cross the deadline; do not abort that commit.
      if (this.shuttingDown || this.revisitCancelled || this.children.get('host') !== host || !host?.open) {
        throw new Error('owned Root revisit interrupted');
      }
      await this.startNativeImages(native, this.sessionEpoch);
      if (this.shuttingDown || this.revisitCancelled || this.children.get('host') !== host || !host?.open) {
        throw new Error('owned Root revisit interrupted');
      }
      return registration;
    } catch {
      if (transition && !this.revisitCancelled) {
        try { await control.abort(transition); } catch { /* Closing the private pipe invalidates the transition. */ }
      }
      this.cancelOwnedRevisit();
      await this.shutdown();
      throw new Error('owned Root revisit failed');
    }
  }

  private async stopOwnedPeers(): Promise<void> {
    if (this.deviceLeaseTimer) { clearInterval(this.deviceLeaseTimer); this.deviceLeaseTimer = null; }
    if (this.imageTimer) { clearInterval(this.imageTimer); this.imageTimer = null; }
    this.closeComputerImagePort();
    this.imageControl = null; this.captureControl = null;
    await this.imageClient?.close(); this.imageClient = null;
    this.notifyImageAvailability();
    const peers = (['device', 'source'] as const).map(role => this.children.get(role)).filter((child): child is Child => !!child);
    for (const child of peers) {
      child.expectedExit = true;
      this.write(child, { type: 'shutdown', payload: { reason: 'the owned document is reopening for verification' } });
      child.process.stdin?.end();
    }
    const wait = async (durationMs: number): Promise<void> => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        await Promise.race([Promise.all(peers.map(child => child.exited)),
          new Promise<void>(done => { timer = setTimeout(done, durationMs); })]);
      } finally { clearTimeout(timer); }
    };
    await wait(3_000);
    for (const child of peers) if (child.open) child.process.kill('SIGKILL');
    await wait(3_000);
    if (peers.some(child => child.open)) throw new Error('owned observer shutdown unconfirmed');
    for (const child of peers) if (this.children.get(child.role) === child) this.children.delete(child.role);
  }

  /** Main goes away, or is told to: pipes close and every process leaves. */
  shutdown(): Promise<void> {
    for (const child of this.children.values()) this.closeTeacherObservation(child);
    this.ownedRevisitControl?.close(); this.ownedRevisitControl = null;
    this.modelPrefix?.fill(0);
    this.modelPrefix = null;
    this.ownedDocumentRegistration = null;
    delete this.options.modelAuthorization;
    this.shutdownTask ??= this.close();
    return this.shutdownTask;
  }

  private async close(): Promise<void> {
    let browserError: unknown;
    try { await this.stopBrowserSource(); } catch (error) { browserError = error; }
    this.shuttingDown = true;
    for (const pending of this.browserControlWaiters.values()) {
      clearTimeout(pending.timer); pending.reject(new Error('Host closed the browser observation channel.'));
    }
    this.browserControlWaiters.clear();
    this.closeComputerImagePort();
    if (this.imageTimer) clearInterval(this.imageTimer);
    if (this.deviceLeaseTimer) clearInterval(this.deviceLeaseTimer);
    await this.imageClient?.close();
    this.imageClient = null; this.imageControl = null; this.captureControl = null; this.captureControl = null;
    const all = [...this.children.values()];
    for (const child of all) {
      this.write(child, { type: 'shutdown', payload: { reason: 'the supervisor is shutting down' } });
      child.process.stdin?.end();
    }
    const grace = new Promise<void>((done) => setTimeout(done, 3_000).unref());
    await Promise.race([Promise.all(all.map((child) => child.exited)), grace]);
    for (const child of all) if (child.open) child.process.kill('SIGKILL');
    await Promise.all(all.map((child) => child.exited));
    // A failed browser teardown must retain its profile for cleanup/recovery.
    if (browserError) throw browserError;
    rmSync(this.dir, { recursive: true, force: true });
  }

  private createBrowserSource(): BrowserSourceController {
    if (this.browserSource) return this.browserSource;
    const options = this.options.browserSource!;
    const profileParent = join(realpathSync(this.dir), 'headless-profiles');
    mkdirSync(profileParent, { mode: 0o700 });
    this.observe(this.options.observeBrowserSourceOwner, { supervisorDir: this.dir, profileParent, pids: this.pids });
    const controller = BrowserSourceController.create({ ...options, profileParent,
      sessionEpoch: this.sessionEpoch, generation: 1, clockOriginUnixMs: this.clockOriginMs }, {
      sendControl: control => this.browserControl(control),
      sendSnapshot: async snapshot => {
        const host = this.children.get('host');
        if (!host?.open || this.shuttingDown || this.browserSource !== controller) {
          throw new Error('Browser observation channel is unavailable.');
        }
        await this.browserRequest({ type: 'browser_page_snapshot', payload: snapshot });
        if (!host.open || this.shuttingDown || this.children.get('host') !== host || this.browserSource !== controller) {
          throw new Error('Browser observation channel is unavailable.');
        }
        this.observe(this.options.observeBrowserSnapshot, snapshot);
      },
    });
    this.browserSource = controller;
    return controller;
  }

  private async prepareBrowserSource(): Promise<void> {
    const controller = this.createBrowserSource();
    this.preparedBrowserScope = await controller.prepare();
    this.preparedBrowserContextHash = await browserTargetContextHash(this.preparedBrowserScope.context);
  }

  private async startBrowserSource(): Promise<void> {
    const controller = this.createBrowserSource();
    this.observe(this.options.observeBrowserSourceOwner, { supervisorDir: this.dir,
      profileParent: join(realpathSync(this.dir), 'headless-profiles'), pids: this.pids });
    await controller.start();
    // This is Source sampling only; tasks, decisions and input stay in the engine.
    this.browserTimer = setInterval(() => {
      if (this.browserSource === controller) void controller.refresh().catch(() => {
        if (this.browserSource === controller) this.emit('log', 'browser page observation unavailable');
      });
    }, 1_000);
    this.browserTimer.unref();
  }

  refreshBrowserObservation(): Promise<void> {
    if (!this.browserSource) return Promise.reject(new Error('Browser observation is unavailable.'));
    return this.browserSource.refresh().then(() => undefined);
  }

  browserObservationStatus(): ReturnType<BrowserSourceController['getStatus']> | null {
    return this.browserSource?.getStatus() ?? null;
  }

  private browserControl(control: BrowserSourceControl): Promise<void> {
    return this.browserRequest({ type: 'browser_source_control', payload: control });
  }

  private rejectBrowserRequests(host: Child): void {
    for (const [id, pending] of this.browserControlWaiters) {
      if (pending.host !== host) continue;
      clearTimeout(pending.timer);
      this.browserControlWaiters.delete(id);
      pending.reject(new Error('Browser observation Host closed.'));
    }
  }

  private browserRequest(message: Message): Promise<void> {
    const host = this.children.get('host');
    if (!host?.open || !host.process.stdin?.writable || this.shuttingDown || this.browserControlWaiters.size >= 8) {
      return Promise.reject(new Error('Browser observation control is unavailable.'));
    }
    // The existing control envelope has a 1 MiB ceiling. Reject before spending
    // a producer sequence, so invalidation can still use the intact channel.
    if (Buffer.byteLength(JSON.stringify(message), 'utf8') > (1 << 20) - 4_096) {
      return Promise.reject(new Error('Browser observation exceeds the control frame limit.'));
    }
    const envelope = host.factory.make(message);
    return new Promise<void>((resolve, reject) => {
      const fail = (error: Error): void => {
        const pending = this.browserControlWaiters.get(envelope.message_id);
        if (!pending) return;
        clearTimeout(pending.timer); this.browserControlWaiters.delete(envelope.message_id); reject(error);
      };
      const timer = setTimeout(() => fail(new Error('Browser observation control timed out.')), 5_000);
      this.browserControlWaiters.set(envelope.message_id, { host, messageType: message.type, resolve, reject, timer });
      try { host.process.stdin!.write(encodeFrame(envelope), error => { if (error) fail(new Error('Browser observation control failed.')); }); }
      catch { fail(new Error('Browser observation control failed.')); }
    });
  }

  private stopBrowserSource(): Promise<void> {
    this.retireDeviceBrowserPort('stopped');
    this.preparedBrowserScope = null;
    this.preparedBrowserContextHash = null;
    if (this.browserTimer) clearInterval(this.browserTimer);
    this.browserTimer = null;
    const controller = this.browserSource;
    this.browserSource = null;
    if (controller) this.browserStopTask ??= controller.stop();
    return this.browserStopTask ?? Promise.resolve();
  }

  private retireDeviceBrowserPort(reason: 'stopped' | 'source_unavailable' | 'document_changed' | 'session_closed'): void {
    const port = this.deviceBrowserPort;
    this.deviceBrowserPort = null;
    if (port) void port.retire(reason).catch(() => this.emit('log', 'browser resource cleanup failed'));
  }

  private refreshBrowserForHost(request: BrowserRefreshRequest): void {
    const source = this.browserSource;
    const scope = this.preparedBrowserScope;
    const nowMs = Date.now() - this.clockOriginMs;
    if (!source || !scope || this.shuttingDown || !this.options.browserPageProfile
      || this.browserRefreshTask || request.scope_id !== scope.scope_id
      || request.generation !== scope.generation || request.clock_domain !== scope.clock_domain
      || request.target_context_hash !== this.preparedBrowserContextHash
      || request.after_source_sequence > (source.getStatus().lastSequence ?? 0)
      || request.after_ms > nowMs + 250 || request.deadline_ms <= nowMs
      || request.deadline_ms <= request.after_ms
      || request.deadline_ms - request.after_ms > 30_000
      || !Object.keys(scope.context).every(key => request.context[key as keyof typeof scope.context]
        === scope.context[key as keyof typeof scope.context])) return;
    const task = source.refreshAfter(request.after_ms).then(() => undefined);
    this.browserRefreshTask = task;
    void task.catch(() => this.emit('log', 'browser verification observation unavailable'))
      .finally(() => { if (this.browserRefreshTask === task) this.browserRefreshTask = null; });
  }
}
