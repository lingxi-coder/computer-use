import { EventEmitter } from 'node:events';
import { dirname, resolve } from 'node:path';

import type { NativeAuthorization, NativeWindowTarget, OperatorCommand, DecisionProposal, StopScope, StopDisposition, StopSource, OwnedDocumentStatus, UiProjection,
  RowTableScope, ScopedWindowTarget, BackgroundWindowTarget } from '@computer-use/desktop-api';

import { Supervisor, type SupervisorOptions, type SupervisorEvents } from './supervisor.js';
import { validateNativeTableScope } from './nativeSource.js';
import { authorizeOwnedDocumentSave, authorizeOwnedDocumentValueInput } from './ownedDocumentInput.js';
import { activateOwnedDocumentForObservation, encodeOwnedDocumentSourceBootstrap,
  type OwnedDocumentLaunchRunner, type OwnedDocumentRegistration } from './ownedDocumentLaunch.js';

type Pipeline = Pick<Supervisor, 'projection' | 'clockOriginUnixMs' | 'image' | 'operator' | 'stopAtDevice' | 'sendDecisionProposal' | 'shutdown'> & {
  revisitOwnedRoot?: () => Promise<OwnedDocumentRegistration>;
  on<K extends keyof SupervisorEvents>(event: K, listener: (...args: SupervisorEvents[K]) => void): unknown;
  off<K extends keyof SupervisorEvents>(event: K, listener: (...args: SupervisorEvents[K]) => void): unknown;
};
export interface OwnedDocumentSessionServices {
  createRunner: () => OwnedDocumentLaunchRunner;
  confirmEdit: (signal: AbortSignal) => Promise<boolean>;
  confirmSave: (signal: AbortSignal) => Promise<boolean>;
  canPlan: () => boolean;
}

type OwnedRootAnchor = { root_task_id: string; task_revision: number; root_epoch: number; saved_at_ms: number };
type RootIdentity = Omit<OwnedRootAnchor, 'saved_at_ms'>;
type ScopedConsentAnchor = {
  app: string;
  pid: number;
  windowId: string;
  framePoints: readonly [number, number, number, number];
  sourceWindowFrame: readonly [number, number, number, number];
  windowTitle: string;
  sourceSessionId: string;
  sessionEpoch: number;
  processStartSeconds: number;
  processStartMicros: number;
  tableScope: RowTableScope;
  rootIdentityHash: string;
  sourceSequence: number;
  sourceObservedMs: number;
};
type BackgroundConsentAnchor = {
  app: string;
  pid: number;
  windowId: string;
  framePoints: readonly [number, number, number, number];
  sourceWindowFrame: readonly [number, number, number, number];
  windowTitle: string | null;
  sourceSessionId: string;
  sessionEpoch: number;
  processStartSeconds: number;
  processStartMicros: number;
  targetContextHash: string;
  sourceSequence: number;
  sourceObservedMs: number;
};
type AuthorizationDomain = NonNullable<NativeAuthorization['candidate_domain']>;
type ConfirmNativeWindow = (target: NativeWindowTarget, domain: AuthorizationDomain) => Promise<boolean>;
type NativeCandidate = { target: NativeWindowTarget; scopedAnchor: ScopedConsentAnchor | null;
  backgroundAnchor: BackgroundConsentAnchor | null; domain: AuthorizationDomain };
type Events = SupervisorEvents & {
  nativeAuthorization: [NativeAuthorization];
  ownedDocumentStatus: [OwnedDocumentStatus];
  sessionChanged: [];
  controlStop: [];
};
const TERMINAL = new Set(['completed', 'failed', 'unverified', 'expired', 'cancelled']);
const REGISTERED = new Set(['com.lingxi.computer-user.guard-fixture', 'com.lingxi.computer-user.mvp-fixture', 'com.apple.TextEdit']);
const OWNED_SAVE_NOTE = 'the owned Save input completed; independent saved-file and root revisit evidence is still required';
const OWNED_DOCUMENT_BUNDLE = 'com.apple.TextEdit';

function roundLikeRust(value: number): number {
  return value < 0 ? Math.ceil(value - 0.5) : Math.floor(value + 0.5);
}

function sameTableScope(left: RowTableScope, right: RowTableScope): boolean {
  return left.scroll_path.length === right.scroll_path.length
    && left.scroll_path.every((container, index) => container.role === right.scroll_path[index]?.role
      && container.label === right.scroll_path[index]?.label)
    && left.column_headers.length === right.column_headers.length
    && left.column_headers.every((header, index) => header === right.column_headers[index]);
}

/** Main owns consent and session replacement. A replacement always starts observing with fresh pipes. */
export class NativeSession extends EventEmitter<Events> {
  private pipeline: Pipeline | null = null;
  private detach: (() => void) | null = null;
  private phase: NativeAuthorization['phase'];
  private authorized: NativeWindowTarget | null = null;
  private authorizedDomain: NativeAuthorization['authorized_domain'] = null;
  private reason: string | null = null;
  private generation = 0;
  private closing = false;
  private change: Promise<void> | null = null;
  private ownedLifecycleTask: Promise<void> | null = null;
  private shutdownTask: Promise<void> | null = null;
  private configurationLocks = new Set<symbol>();
  private cleanupUnconfirmed = false;
  private ownedPhase: OwnedDocumentStatus['phase'];
  private ownedReason: string | null = null;
  private ownedRegistration: OwnedDocumentRegistration | null;
  private ownedRunner: OwnedDocumentLaunchRunner | null = null;
  private ownedEditApproved = false;
  private ownedSaveAuthorized = false;
  private ownedSaveAuthorizedAtMs = 0;
  private ownedSaveAnchor: OwnedRootAnchor | null = null;
  private ownedRevisitAnchor: OwnedRootAnchor | null = null;
  private ownedPromptRoot: RootIdentity | null = null;
  private ownedPromptActive = false;
  private ownedObservedRoot: RootIdentity | null = null;
  private ownedSaveNoteHighWaterAtMs = 0;
  private editPromptAbort: AbortController | null = null;
  private savePromptAbort: AbortController | null = null;
  private ownedActivationGeneration = 0;
  private ownedActivationPending = false;

  private constructor(
    private options: SupervisorOptions,
    private readonly confirm: ConfirmNativeWindow,
    private readonly start: (options: SupervisorOptions) => Promise<Pipeline>,
    private readonly now: () => number,
    private readonly ownedServices?: OwnedDocumentSessionServices,
  ) {
    super();
    this.phase = options.nativeSource ? 'observing' : 'not_native';
    this.ownedRegistration = options.ownedDocument ?? null;
    this.ownedPhase = this.ownedRegistration ? 'observing' : 'not_created';
    if (!ownedServices && !this.ownedRegistration) this.ownedReason = 'Temporary TextEdit documents are unavailable in this session.';
  }

  static async start(
    options: SupervisorOptions,
    confirm: ConfirmNativeWindow,
    start: (options: SupervisorOptions) => Promise<Pipeline> = (options) => Supervisor.start(options),
    now: () => number = Date.now,
    ownedServices?: OwnedDocumentSessionServices,
  ): Promise<NativeSession> {
    if (options.nativeSource?.authorizedWindowId !== undefined
      && options.nativeSource.sourceReadWindowId !== undefined
      && options.nativeSource.authorizedWindowId !== options.nativeSource.sourceReadWindowId) {
      throw new Error('Source read and authorized windows must match');
    }
    // Even trusted startup options cannot persist consent across product launches.
    const initialNativeSource = options.nativeSource ? { ...options.nativeSource, authorizedWindowId: undefined,
      authorizedProcessIdentity: undefined } : undefined;
    if (initialNativeSource) delete initialNativeSource.authorizedBackgroundWindowId;
    const initial = { ...options, modelAuthorization: undefined, ownedDocumentValueAuthorization: undefined,
      ownedDocumentSaveAuthorization: undefined,
      ownedDocumentReferencePlanner: undefined, ownedDocumentRootRevisit: undefined, mode: 'observe' as const,
      ...(initialNativeSource ? { nativeSource: initialNativeSource } : {}),
    };
    const session = new NativeSession(initial, confirm, start, now, ownedServices);
    session.connect(await start(initial));
    return session;
  }

  get projection() {
    const value = this.pipeline?.projection ?? null;
    return value && this.cleanupUnconfirmed ? { ...value, cleanup: 'unknown' as const } : value;
  }
  image(frameId: string) { return this.pipeline?.image(frameId) ?? null; }

  get ownedDocumentStatus(): OwnedDocumentStatus {
    const root = this.projection?.root;
    const activeRoot = !!root && !TERMINAL.has(root.state);
    const browserProfileActive = this.options.browserPageProfile !== undefined;
    const ownedBusy = ['creating', 'authorizing_edit', 'authorizing_save', 'revisiting'].includes(this.ownedPhase);
    const blocked = this.closing || this.configurationLocks.size > 0 || !this.pipeline || this.cleanupIsUnconfirmed()
      || ownedBusy || browserProfileActive;
    const canCreate = !!this.ownedServices && !blocked && !activeRoot
      && (this.ownedPhase === 'not_created' || this.ownedPhase === 'completed');
    const canAuthorizeEdit = !!this.ownedServices && !!this.ownedRegistration && !blocked && !activeRoot
      && !this.ownedEditApproved && this.ownedPhase === 'observing';
    const canAuthorizeSave = !!this.ownedServices && !!this.ownedRegistration && this.ownedEditApproved
      && !this.ownedSaveAuthorized && !blocked && !activeRoot && this.ownedPhase === 'edit_authorized'
      && this.ownedServices.canPlan();
    const anchor = this.ownedSaveAnchor;
    const canRevisit = !!this.ownedRegistration && this.ownedSaveAuthorized && !blocked
      && !!anchor && !!root && this.sameRoot(root, anchor) && !TERMINAL.has(root.state)
      && typeof this.pipeline?.revisitOwnedRoot === 'function' && this.ownedPhase === 'save_authorized';
    const reason = this.ownedReason
      ?? (this.cleanupIsUnconfirmed() ? 'Input cleanup is unconfirmed; owned-document actions remain closed.'
        : browserProfileActive ? 'The fixture browser profile is active; owned-document actions are closed.'
        : this.ownedPhase === 'unavailable' ? 'The owned-document session is unavailable.'
          : !this.ownedServices && !this.ownedRegistration ? 'Temporary TextEdit documents are unavailable in this session.'
            : this.ownedRegistration && !this.ownedServices ? 'Owned-document lifecycle services are unavailable.'
              : this.ownedServices !== undefined && !this.ownedServices.canPlan() && this.ownedEditApproved
                ? 'Authorize an S2 planning model before saving or starting an owned-document task.'
                : activeRoot && !canRevisit ? 'Finish or stop the current task before changing owned-document permissions.' : null);
    return {
      phase: this.ownedPhase,
      can_create: canCreate,
      can_authorize_edit: canAuthorizeEdit,
      can_authorize_save: canAuthorizeSave,
      can_revisit: canRevisit,
      reason,
    };
  }

  private candidateEvidence(): NativeCandidate | null {
    const pipeline = this.pipeline;
    const observation = pipeline?.projection?.observation;
    const target = observation?.observed_target;
    const native = this.options.nativeSource;
    if (!pipeline || !native) return null;
    // Background TargetContext is an independent logical target, not a claim
    // about system focus. It can only enter through its dedicated projection
    // candidate; never fall back to foreground observed_target metadata.
    if (native.backgroundWindowId !== undefined) {
      return this.backgroundCandidate(pipeline, observation?.background_window_target ?? null);
    }
    if (native.tableScope !== undefined) return this.scopedCandidate(pipeline, observation?.scoped_window_target ?? null);
    if (!target || !REGISTERED.has(target.app) || target.app !== native.bundleId
      || (native.pid !== undefined && target.pid !== native.pid)
      || (native.sourceReadWindowId !== undefined && target.window_id !== native.sourceReadWindowId)
      || target.session_epoch !== pipeline.projection?.capabilities?.identity.session_epoch
      || !/^window-[1-9]\d*$/.test(target.window_id) || Number(target.window_id.slice(7)) > 0xffff_ffff
      || target.pid <= 0 || target.pid > 0x7fff_ffff
      || target.observed_ms > this.now() - pipeline.clockOriginUnixMs
      || target.valid_until_ms <= this.now() - pipeline.clockOriginUnixMs) return null;
    return { target: { snapshot_id: target.snapshot_id, app: target.app, pid: target.pid, window_id: target.window_id,
      window_title: target.window_title ?? null, contains_private_fields: target.contains_private_fields, frame_points: target.frame_points ?? null,
      expires_unix_ms: pipeline.clockOriginUnixMs + target.valid_until_ms }, scopedAnchor: null, backgroundAnchor: null,
      domain: 'foreground_native' };
  }

  private backgroundCandidate(pipeline: Pipeline, background: BackgroundWindowTarget | null): NativeCandidate | null {
    const native = this.options.nativeSource;
    if (!native || !background || typeof background !== 'object') return null;
    try {
      const { source, frame_points: framePoints } = background;
      if (!source || typeof source !== 'object' || !source.window || typeof source.window !== 'object') return null;
      const window = source.window;
      const frame = window.origin_window_frame;
      const process = window.process;
      const title = background.window_title ?? null;
      const now = this.now() - pipeline.clockOriginUnixMs;
      const validFrame = !!framePoints && framePoints.length === 4
        && framePoints.every((value) => Number.isSafeInteger(value))
        && !!frame && Number.isFinite(frame.x) && Number.isFinite(frame.y)
        && Number.isFinite(frame.width) && Number.isFinite(frame.height)
        && [frame.x, frame.y, frame.width, frame.height].every((value) => Math.abs(value) <= 100_000)
        && frame.width > 0 && frame.height > 0
        && framePoints[0] === roundLikeRust(frame.x) && framePoints[1] === roundLikeRust(frame.y)
        && framePoints[2] === roundLikeRust(frame.width) && framePoints[3] === roundLikeRust(frame.height)
        && framePoints[2] > 0 && framePoints[3] > 0;
      if (typeof background.snapshot_id !== 'string' || !background.snapshot_id.trim()
        || Buffer.byteLength(background.snapshot_id, 'utf8') > 256 || /[\u0000-\u001f\u007f-\u009f]/u.test(background.snapshot_id)
        || !REGISTERED.has(background.app) || background.app !== native.bundleId
        || pipeline.projection?.observation?.target_app !== background.app
        || native.pid === undefined || background.pid !== native.pid || process?.pid !== background.pid
        || background.window_id !== native.backgroundWindowId || window.origin_window_id !== background.window_id
        || !/^window-[1-9]\d*$/.test(background.window_id) || Number(background.window_id.slice(7)) > 0xffff_ffff
        || background.pid <= 0 || background.pid > 0x7fff_ffff
        || !Number.isSafeInteger(background.session_epoch) || background.session_epoch <= 0
        || background.session_epoch !== pipeline.projection?.capabilities?.identity.session_epoch
        || window.session_epoch !== background.session_epoch
        || !Number.isSafeInteger(background.structure_sequence) || background.structure_sequence <= 0
        || window.verified_source_sequence !== background.structure_sequence
        || !Number.isSafeInteger(background.observed_ms) || background.observed_ms < 0
        || window.verified_observed_ms !== background.observed_ms || background.observed_ms > now
        || !Number.isSafeInteger(background.valid_until_ms) || background.valid_until_ms <= now
        || background.valid_until_ms <= background.observed_ms
        || background.valid_until_ms - background.observed_ms > 2_000
        || typeof window.source_session_id !== 'string' || !window.source_session_id.trim()
        || Buffer.byteLength(window.source_session_id, 'utf8') > 256
        || /[\u0000-\u001f\u007f-\u009f]/u.test(window.source_session_id)
        || !Number.isSafeInteger(process?.start_seconds) || process.start_seconds <= 0
        || !Number.isSafeInteger(process?.start_micros) || process.start_micros < 0 || process.start_micros > 999_999
        || source.hidden?.state !== 'known' || source.hidden.value !== false
        || source.minimized?.state !== 'known' || source.minimized.value !== false
        || (title !== null && (typeof title !== 'string' || !title.trim() || Buffer.byteLength(title, 'utf8') > 4_096
          || /[\u0000-\u001f\u007f-\u009f]/u.test(title)) )
        || !/^tc1:[0-9a-f]{64}$/.test(source.target_context_hash)
        || !validFrame) return null;

      const anchor: BackgroundConsentAnchor = {
        app: background.app,
        pid: background.pid,
        windowId: background.window_id,
        framePoints: framePoints!,
        sourceWindowFrame: [frame.x, frame.y, frame.width, frame.height],
        windowTitle: title,
        sourceSessionId: window.source_session_id,
        sessionEpoch: window.session_epoch,
        processStartSeconds: process.start_seconds,
        processStartMicros: process.start_micros,
        targetContextHash: source.target_context_hash,
        sourceSequence: window.verified_source_sequence,
        sourceObservedMs: window.verified_observed_ms,
      };
      return { target: {
        snapshot_id: background.snapshot_id,
        app: background.app,
        pid: background.pid,
        window_id: background.window_id,
        window_title: title,
        frame_points: framePoints,
        contains_private_fields: false,
        expires_unix_ms: pipeline.clockOriginUnixMs + background.valid_until_ms,
      }, scopedAnchor: null, backgroundAnchor: anchor, domain: 'background_ax' };
    } catch {
      return null;
    }
  }

  private scopedCandidate(pipeline: Pipeline, scoped: ScopedWindowTarget | null): NativeCandidate | null {
    const native = this.options.nativeSource;
    if (!native || !scoped || !native.pid || !native.sourceReadWindowId || !native.tableScope) return null;
    const { target, source, table_scope: tableScope, root_identity_hash: rootIdentityHash } = scoped;
    if (!target || typeof target !== 'object' || !source || typeof source !== 'object'
      || !source.process || typeof source.process !== 'object'
      || !source.origin_window_frame || typeof source.origin_window_frame !== 'object') return null;
    const now = this.now() - pipeline.clockOriginUnixMs;
    let checkedScope: RowTableScope;
    let configuredScope: RowTableScope;
    try {
      checkedScope = validateNativeTableScope(tableScope);
      configuredScope = validateNativeTableScope(native.tableScope);
    }
    catch { return null; }
    const scopeMatches = sameTableScope(checkedScope, configuredScope);
    const windowFrame = source.origin_window_frame;
    const framePoints = target.frame_points;
    const validFrame = !!framePoints && framePoints.length === 4
      && framePoints.every((value) => Number.isSafeInteger(value))
      && Number.isFinite(windowFrame.x) && Number.isFinite(windowFrame.y)
      && Number.isFinite(windowFrame.width) && Number.isFinite(windowFrame.height)
      && windowFrame.width > 0 && windowFrame.height > 0
      && framePoints[0] === roundLikeRust(windowFrame.x) && framePoints[1] === roundLikeRust(windowFrame.y)
      && framePoints[2] > 0 && framePoints[3] > 0
      && framePoints[2] === roundLikeRust(windowFrame.width) && framePoints[3] === roundLikeRust(windowFrame.height);
    const title = target.window_title;
    if (typeof target.snapshot_id !== 'string' || !target.snapshot_id || target.snapshot_id.length > 256
      || !REGISTERED.has(target.app) || target.app !== native.bundleId || target.pid !== native.pid
      || source.process.pid !== target.pid || source.origin_window_id !== native.sourceReadWindowId
      || target.window_id !== native.sourceReadWindowId
      || !/^window-[1-9]\d*$/.test(target.window_id) || Number(target.window_id.slice(7)) > 0xffff_ffff
      || target.pid <= 0 || target.pid > 0x7fff_ffff
      || !Number.isSafeInteger(target.session_epoch) || target.session_epoch <= 0
      || target.session_epoch !== pipeline.projection?.capabilities?.identity.session_epoch
      || source.session_epoch !== target.session_epoch
      || source.verified_source_sequence !== target.structure_sequence
      || source.verified_observed_ms !== target.observed_ms
      || !Number.isSafeInteger(target.structure_sequence) || target.structure_sequence <= 0
      || !Number.isSafeInteger(target.observed_ms) || target.observed_ms < 0 || target.observed_ms > now
      || !Number.isSafeInteger(target.valid_until_ms) || target.valid_until_ms <= now
      || target.valid_until_ms <= target.observed_ms || target.valid_until_ms - target.observed_ms > 2_000
      || target.valid_until_ms - target.observed_ms <= 0
      || !Number.isSafeInteger(source.verified_source_sequence) || source.verified_source_sequence <= 0
      || !Number.isSafeInteger(source.verified_observed_ms) || source.verified_observed_ms < 0
      || typeof source.source_session_id !== 'string' || !source.source_session_id.trim()
      || Buffer.byteLength(source.source_session_id, 'utf8') > 256
      || /[\u0000-\u001f\u007f-\u009f]/u.test(source.source_session_id)
      || !Number.isSafeInteger(source.process.start_seconds) || source.process.start_seconds <= 0
      || !Number.isSafeInteger(source.process.start_micros) || source.process.start_micros < 0 || source.process.start_micros > 999_999
      || !validFrame || typeof title !== 'string' || !title.trim() || target.contains_private_fields
      || !scopeMatches || typeof rootIdentityHash !== 'string' || !/^[0-9a-f]{64}$/.test(rootIdentityHash)) return null;

    const anchor: ScopedConsentAnchor = {
      app: target.app,
      pid: target.pid,
      windowId: target.window_id,
      framePoints: framePoints!,
      sourceWindowFrame: [windowFrame.x, windowFrame.y, windowFrame.width, windowFrame.height],
      windowTitle: title,
      sourceSessionId: source.source_session_id,
      sessionEpoch: source.session_epoch,
      processStartSeconds: source.process.start_seconds,
      processStartMicros: source.process.start_micros,
      tableScope: checkedScope,
      rootIdentityHash,
      sourceSequence: source.verified_source_sequence,
      sourceObservedMs: source.verified_observed_ms,
    };
    return { target: { snapshot_id: target.snapshot_id, app: target.app, pid: target.pid, window_id: target.window_id,
      window_title: title, contains_private_fields: false, frame_points: framePoints,
      expires_unix_ms: pipeline.clockOriginUnixMs + target.valid_until_ms }, scopedAnchor: anchor, backgroundAnchor: null,
      domain: 'foreground_native' };
  }

  private sameScopedConsentAnchor(anchor: ScopedConsentAnchor, current: NativeCandidate | null): boolean {
    const next = current?.scopedAnchor;
    return !!next && next.app === anchor.app && next.pid === anchor.pid && next.windowId === anchor.windowId
      && next.windowTitle === anchor.windowTitle && next.framePoints.every((value, index) => value === anchor.framePoints[index])
      && next.sourceWindowFrame.every((value, index) => value === anchor.sourceWindowFrame[index])
      && next.sourceSessionId === anchor.sourceSessionId && next.sessionEpoch === anchor.sessionEpoch
      && next.processStartSeconds === anchor.processStartSeconds && next.processStartMicros === anchor.processStartMicros
      && sameTableScope(next.tableScope, anchor.tableScope)
      && next.rootIdentityHash === anchor.rootIdentityHash
      && next.sourceSequence >= anchor.sourceSequence && next.sourceObservedMs >= anchor.sourceObservedMs;
  }

  private sameBackgroundConsentAnchor(anchor: BackgroundConsentAnchor, current: NativeCandidate | null): boolean {
    const next = current?.backgroundAnchor;
    return !!next && next.app === anchor.app && next.pid === anchor.pid && next.windowId === anchor.windowId
      && next.windowTitle === anchor.windowTitle && next.framePoints.every((value, index) => value === anchor.framePoints[index])
      && next.sourceWindowFrame.every((value, index) => value === anchor.sourceWindowFrame[index])
      && next.sourceSessionId === anchor.sourceSessionId && next.sessionEpoch === anchor.sessionEpoch
      && next.processStartSeconds === anchor.processStartSeconds && next.processStartMicros === anchor.processStartMicros
      && next.targetContextHash === anchor.targetContextHash
      && next.sourceSequence >= anchor.sourceSequence && next.sourceObservedMs >= anchor.sourceObservedMs;
  }

  private sameConsentCandidate(anchor: NativeCandidate, current: NativeCandidate | null): boolean {
    if (!current || current.domain !== anchor.domain) return false;
    if (anchor.backgroundAnchor) return this.sameBackgroundConsentAnchor(anchor.backgroundAnchor, current);
    if (anchor.scopedAnchor) return this.sameScopedConsentAnchor(anchor.scopedAnchor, current);
    const expected = anchor.target;
    const next = current?.target;
    return !!next && next.app === expected.app && next.pid === expected.pid && next.window_id === expected.window_id;
  }

  get authorization(): NativeAuthorization {
    const candidateEvidence = this.candidateEvidence();
    const candidate = candidateEvidence?.target ?? null;
    const active = this.projection?.root && !TERMINAL.has(this.projection.root.state);
    const uncertainCleanup = this.cleanupIsUnconfirmed();
    return { phase: this.phase, candidate, authorized: this.authorized,
      candidate_domain: candidateEvidence?.domain ?? null,
      authorized_domain: this.authorized ? this.authorizedDomain : null,
      can_authorize: this.phase === 'observing' && candidate !== null && !active && !uncertainCleanup && !this.closing && this.configurationLocks.size === 0,
      can_refresh_observation: this.canRefreshObservation(),
      reason: this.reason ?? (uncertainCleanup ? 'Input cleanup is unconfirmed. Window permission remains closed.'
        : active && !this.authorized ? 'Finish or stop the current task before authorizing a window.' : null) };
  }

  private canRefreshObservation(): boolean {
    const native = this.options.nativeSource;
    if (!native || (this.phase !== 'observing' && this.phase !== 'unavailable')
      || (this.phase === 'observing' && !this.pipeline) || this.closing || this.change !== null
      || this.configurationLocks.size > 0 || this.cleanupIsUnconfirmed() || this.ownedRegistration
      || this.ownedLifecycleTask !== null || this.ownedActivationPending
      || ['creating', 'authorizing_edit', 'authorizing_save', 'revisiting'].includes(this.ownedPhase)) return false;
    const root = this.projection?.root;
    if (root && !TERMINAL.has(root.state)) return false;
    const backgroundRead = native.backgroundWindowId !== undefined;
    const scopedTableRead = native.sourceReadWindowId !== undefined && native.tableScope !== undefined;
    const windowId = backgroundRead ? native.backgroundWindowId : native.sourceReadWindowId;
    if (native.pid === undefined || !Number.isSafeInteger(native.pid) || native.pid <= 0 || native.pid > 0x7fff_ffff
      || !windowId || !/^window-[1-9]\d*$/.test(windowId) || Number(windowId.slice(7)) > 0xffff_ffff
      || (!backgroundRead && !scopedTableRead) || !REGISTERED.has(native.bundleId)) return false;
    if (native.sourceReadWindowId !== undefined && native.tableScope !== undefined) {
      try { validateNativeTableScope(native.tableScope); }
      catch { return false; }
    }
    if (this.phase === 'observing' && this.candidateEvidence() !== null) return false;
    return true;
  }

  private publish(): void {
    this.emit('nativeAuthorization', this.authorization);
    this.emit('ownedDocumentStatus', this.ownedDocumentStatus);
  }

  private publishOwned(): void { this.emit('ownedDocumentStatus', this.ownedDocumentStatus); }

  private trustedOwnedPid(registration: OwnedDocumentRegistration): number {
    const prefix = encodeOwnedDocumentSourceBootstrap(registration);
    try {
      if (prefix.length < 5 || prefix.readUInt32BE(0) !== prefix.length - 4 || prefix.length > 8196) {
        throw new Error('invalid owned document registration');
      }
      const claims = JSON.parse(prefix.subarray(4).toString('utf8')) as Record<string, unknown>;
      if (claims['version'] !== 1 || claims['bundle_id'] !== OWNED_DOCUMENT_BUNDLE
        || !Number.isSafeInteger(claims['pid']) || (claims['pid'] as number) <= 0 || (claims['pid'] as number) > 0x7fff_ffff) {
        throw new Error('invalid owned document registration');
      }
      return claims['pid'] as number;
    } finally { prefix.fill(0); }
  }

  private assertOwnedIdle(): void {
    if (!this.ownedServices || this.closing || this.configurationLocks.size > 0 || !this.pipeline
      || this.cleanupIsUnconfirmed() || (this.projection?.root && !TERMINAL.has(this.projection.root.state))) {
      throw new Error('Owned-document actions are unavailable while the session is busy or cleanup is uncertain.');
    }
  }

  private waitForOwnedLifecycleTask(): Promise<void> {
    const task = this.ownedLifecycleTask;
    return task?.catch(() => undefined) ?? Promise.resolve();
  }

  private async closeForOwnedTransition(generation: number): Promise<void> {
    const old = this.pipeline;
    this.detach?.(); this.detach = null;
    old?.stopAtDevice({ scope: 'all' }, 'cancel', 'operator_button', 'the owned-document input scope is changing');
    await old?.shutdown();
    const cleanup = old?.projection?.cleanup;
    if (cleanup === 'held' || cleanup === 'unknown' || cleanup === 'failed') this.cleanupUnconfirmed = true;
    if (this.cleanupIsUnconfirmed()) throw new Error('Owned-document cleanup is unconfirmed.');
    if (generation !== this.generation || this.closing) throw new Error('Owned-document action was cancelled.');
    this.pipeline = null;
    this.emit('imageAvailability', { frame_id: null, expires_unix_ms: null });
  }

  private safeOptions(options: SupervisorOptions): SupervisorOptions {
    const nativeSource = options.nativeSource ? { ...options.nativeSource, authorizedWindowId: undefined,
      authorizedProcessIdentity: undefined } : undefined;
    if (nativeSource) delete nativeSource.authorizedBackgroundWindowId;
    return { ...options, ownedDocumentValueAuthorization: undefined, ownedDocumentSaveAuthorization: undefined,
      ownedDocumentReferencePlanner: undefined, ownedDocumentRootRevisit: undefined, mode: 'observe',
      ...(nativeSource ? { nativeSource } : {}) };
  }

  /** Replaces the current observing session with Main's single fixed browser profile. */
  async startBrowserProfile(options: Pick<SupervisorOptions, 'browserPageProfile' | 'browserSource'>): Promise<void> {
    if (this.closing) throw new Error('The product is closing.');
    if (options.browserPageProfile?.kind !== 'fixture_background_option'
      || Object.keys(options.browserPageProfile).sort().join(',') !== 'kind' || !options.browserSource
      || Object.keys(options).sort().join(',') !== 'browserPageProfile,browserSource'
      || Object.keys(options.browserSource).sort().join(',') !== 'chromeExecutable,fixturePath') {
      throw new Error('The fixed fixture browser profile configuration is invalid.');
    }
    if (this.options.browserPageProfile?.kind === 'fixture_background_option') return;
    if (this.options.nativeSource || this.ownedRegistration || this.ownedPhase !== 'not_created') {
      throw new Error('The fixture browser profile cannot be mixed with native or owned-document sessions.');
    }
    if (this.configurationLocks.size > 0 || this.ownedLifecycleTask || this.ownedActivationPending
      || this.cleanupIsUnconfirmed()) {
      throw new Error('The fixture browser profile cannot start while session configuration or cleanup is pending.');
    }

    const generation = ++this.generation;
    this.ownedActivationGeneration += 1;
    this.ownedActivationPending = false;
    this.cancelPendingConsent();
    this.cancelPromptControllers();
    const next = this.safeOptions({
      ...this.options,
      mode: 'observe',
      nativeSource: undefined,
      ownedDocument: undefined,
      ownedDocumentValueAuthorization: undefined,
      ownedDocumentSaveAuthorization: undefined,
      ownedDocumentReferencePlanner: undefined,
      ownedDocumentRootRevisit: undefined,
      modelAuthorization: undefined,
      browserPageProfile: { kind: 'fixture_background_option' },
      browserSource: {
        chromeExecutable: options.browserSource.chromeExecutable,
        fixturePath: options.browserSource.fixturePath,
      },
    });
    this.phase = 'switching';
    this.authorized = null;
    this.authorizedDomain = null;
    this.reason = 'Starting the fixed fixture browser profile…';
    this.publish();
    // Stop the existing Root through its normal operator path before shutting
    // down that process tree and installing the new observing session.
    await this.pipeline?.operator({ command: 'stop_task', root_task_id: null });
    await this.change?.catch(() => undefined);
    await this.waitForOwnedLifecycleTask();
    if (this.closing || generation !== this.generation) throw new Error('Browser profile selection was cancelled.');
    await this.replace(generation, next, null);
  }

  createOwnedDocument(): Promise<void> {
    this.assertOwnedIdle();
    if (!this.ownedServices || !['not_created', 'completed'].includes(this.ownedPhase)) {
      return Promise.reject(new Error('A temporary TextEdit document cannot be created now.'));
    }
    const operation = this.createOwnedDocumentTransaction();
    this.ownedLifecycleTask = operation;
    void operation.finally(() => { if (this.ownedLifecycleTask === operation) this.ownedLifecycleTask = null; }).catch(() => undefined);
    return operation;
  }

  private async createOwnedDocumentTransaction(): Promise<void> {
    const services = this.ownedServices;
    if (!services) throw new Error('Temporary TextEdit documents are unavailable.');
    const generation = ++this.generation;
    this.cancelPromptControllers();
    this.ownedPhase = 'creating'; this.ownedReason = null; this.publishOwned();
    let runner: OwnedDocumentLaunchRunner | null = null;
    try {
      await this.closeForOwnedTransition(generation);
      if (this.ownedRunner) {
        const stopped = await this.ownedRunner.stopAll();
        if (stopped.some((result) => result !== 'stopped')) {
          this.cleanupUnconfirmed = true;
          throw new Error('Owned-document cleanup is unconfirmed.');
        }
        this.ownedRunner = null; this.ownedRegistration = null;
      }
      runner = services.createRunner();
      this.ownedRunner = runner;
      const registration = await runner.launch('');
      const pid = this.trustedOwnedPid(registration);
      if (generation !== this.generation || this.closing) throw new Error('Owned-document action was cancelled.');
      const currentNative = this.options.nativeSource;
      const nativeSource = {
        helperPath: currentNative?.helperPath ?? resolve(dirname(this.options.binaries.source), 'cu-macos-source'),
        ...(currentNative?.guardHelperPath ? { guardHelperPath: currentNative.guardHelperPath } : {}),
        bundleId: OWNED_DOCUMENT_BUNDLE, pid,
      };
      const next = this.safeOptions({ ...this.options, mode: 'observe', nativeSource,
        ownedDocument: registration, ownedDocumentValueAuthorization: undefined,
        ownedDocumentSaveAuthorization: undefined, ownedDocumentReferencePlanner: undefined,
        ownedDocumentRootRevisit: undefined });
      this.ownedRegistration = registration;
      this.ownedEditApproved = false; this.ownedSaveAuthorized = false; this.ownedSaveAuthorizedAtMs = 0;
      this.ownedSaveAnchor = null; this.ownedRevisitAnchor = null;
      this.ownedObservedRoot = null; this.ownedSaveNoteHighWaterAtMs = 0;
      await this.replace(generation, next, null);
      if (generation !== this.generation || this.closing) throw new Error('Owned-document action was cancelled.');
      this.ownedPhase = 'observing'; this.ownedReason = null; this.publish();
    } catch {
      this.ownedPhase = 'unavailable';
      this.ownedReason = this.cleanupIsUnconfirmed()
        ? 'Owned-document cleanup is unconfirmed; actions remain closed.'
        : 'The temporary TextEdit document could not be started safely.';
      if (runner) {
        const stopped = await runner.stopAll().catch(() => ['unknown'] as const);
        if (stopped.some((result) => result !== 'stopped')) {
          this.cleanupUnconfirmed = true;
          this.ownedRunner = runner;
        } else if (this.ownedRunner === runner) {
          this.ownedRunner = null; this.ownedRegistration = null;
        }
      }
      this.publish();
      throw new Error('The temporary TextEdit document could not be started safely.');
    }
  }

  async authorizeOwnedDocumentEdit(): Promise<void> {
    this.assertOwnedIdle();
    const services = this.ownedServices;
    if (!this.ownedRegistration || !services || this.ownedPhase !== 'observing') {
      throw new Error('Owned-document editing cannot be authorized now.');
    }
    const registration = this.ownedRegistration;
    const generation = ++this.generation;
    const controller = new AbortController(); this.editPromptAbort = controller;
    this.ownedPromptRoot = this.currentPromptRoot(); this.ownedPromptActive = true;
    this.ownedPhase = 'authorizing_edit'; this.ownedReason = null; this.publishOwned();
    try {
      const approved = await services.confirmEdit(controller.signal);
      if (!approved) return;
      this.assertOwnedGeneration(generation, controller);
      const pid = this.trustedOwnedPid(this.ownedRegistration);
      const next = { ...this.options, mode: 'observe' as const, nativeSource: { ...this.options.nativeSource!, pid,
        bundleId: OWNED_DOCUMENT_BUNDLE, authorizedWindowId: undefined, authorizedProcessIdentity: undefined, referencePlanner: undefined },
        ownedDocument: this.ownedRegistration,
        ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(this.ownedRegistration),
        ownedDocumentSaveAuthorization: undefined, ownedDocumentReferencePlanner: undefined, ownedDocumentRootRevisit: undefined };
      await this.replace(generation, next, null);
      this.assertOwnedGeneration(generation, controller);
      if (this.ownedRegistration !== registration) throw new Error('Owned-document authorization changed.');
      this.trustedOwnedPid(registration);
      this.ownedEditApproved = true; this.ownedSaveAuthorized = false; this.ownedSaveAuthorizedAtMs = 0;
      this.ownedSaveAnchor = null; this.ownedPhase = 'edit_authorized'; this.ownedReason = null;
    } catch {
      if (generation === this.generation) {
        this.ownedPhase = this.ownedRegistration ? 'observing' : 'unavailable';
        this.ownedReason = 'Owned-document editing could not be authorized safely.';
      }
      throw new Error('Owned-document editing could not be authorized safely.');
    } finally {
      if (this.editPromptAbort === controller) this.editPromptAbort = null;
      this.ownedPromptRoot = null; this.ownedPromptActive = false;
      if (generation === this.generation && this.ownedPhase === 'authorizing_edit') this.ownedPhase = 'observing';
      this.publish();
    }
  }

  async authorizeOwnedDocumentSave(): Promise<void> {
    this.assertOwnedIdle();
    const services = this.ownedServices;
    if (!this.ownedRegistration || !services || !this.ownedEditApproved
      || this.ownedPhase !== 'edit_authorized' || !services.canPlan()) {
      throw new Error('Owned-document Save requires approved editing and an authorized S2 planning model.');
    }
    const registration = this.ownedRegistration;
    const generation = ++this.generation;
    const controller = new AbortController(); this.savePromptAbort = controller;
    this.ownedPromptRoot = this.currentPromptRoot(); this.ownedPromptActive = true;
    this.ownedPhase = 'authorizing_save'; this.ownedReason = null; this.publishOwned();
    try {
      const approved = await services.confirmSave(controller.signal);
      if (!approved) return;
      this.assertOwnedGeneration(generation, controller);
      if (!services.canPlan() || !this.ownedRegistration || !this.ownedEditApproved) {
        throw new Error('Owned-document Save authorization changed.');
      }
      const pid = this.trustedOwnedPid(this.ownedRegistration);
      const next = { ...this.options, mode: 'observe' as const,
        nativeSource: { ...this.options.nativeSource!, pid, bundleId: OWNED_DOCUMENT_BUNDLE,
          authorizedWindowId: undefined, authorizedProcessIdentity: undefined, referencePlanner: undefined },
        ownedDocument: this.ownedRegistration,
        ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(this.ownedRegistration),
        ownedDocumentSaveAuthorization: authorizeOwnedDocumentSave(this.ownedRegistration),
        ownedDocumentReferencePlanner: undefined, ownedDocumentRootRevisit: true };
      await this.replace(generation, next, null);
      this.assertOwnedGeneration(generation, controller);
      if (!services.canPlan() || this.ownedRegistration !== registration || !this.ownedEditApproved) {
        throw new Error('Owned-document authorization changed.');
      }
      this.trustedOwnedPid(registration);
      this.ownedEditApproved = true; this.ownedSaveAuthorized = true;
      this.ownedSaveAuthorizedAtMs = this.pipeline ? this.now() - this.pipeline.clockOriginUnixMs : 0;
      this.ownedSaveNoteHighWaterAtMs = Math.max(this.ownedSaveNoteHighWaterAtMs,
        this.ownedSaveAuthorizedAtMs, this.noteHighWater(this.pipeline?.projection ?? null));
      this.ownedSaveAnchor = null; this.ownedPhase = 'save_authorized'; this.ownedReason = null;
    } catch {
      if (generation === this.generation) {
        this.ownedPhase = this.ownedEditApproved ? 'edit_authorized' : 'unavailable';
        this.ownedReason = 'Owned-document Save could not be authorized safely.';
      }
      throw new Error('Owned-document Save could not be authorized safely.');
    } finally {
      if (this.savePromptAbort === controller) this.savePromptAbort = null;
      this.ownedPromptRoot = null; this.ownedPromptActive = false;
      if (generation === this.generation && this.ownedPhase === 'authorizing_save') this.ownedPhase = 'edit_authorized';
      this.publish();
    }
  }

  revisitOwnedDocument(): Promise<void> {
    if (this.ownedLifecycleTask) return Promise.reject(new Error('An owned-document operation is already running.'));
    const operation = this.revisitOwnedDocumentTransaction();
    this.ownedLifecycleTask = operation;
    void operation.finally(() => { if (this.ownedLifecycleTask === operation) this.ownedLifecycleTask = null; }).catch(() => undefined);
    return operation;
  }

  private async revisitOwnedDocumentTransaction(): Promise<void> {
    const status = this.ownedDocumentStatus;
    const pipeline = this.pipeline;
    const root = this.projection?.root;
    const anchor = this.ownedSaveAnchor;
    if (!status.can_revisit || !pipeline?.revisitOwnedRoot || !root || !anchor) {
      throw new Error('The owned document is not ready to revisit.');
    }
    const generation = ++this.generation;
    this.ownedPhase = 'revisiting'; this.ownedReason = null;
    this.ownedRevisitAnchor = { ...anchor };
    this.publishOwned();
    try {
      const registration = await pipeline.revisitOwnedRoot();
      if (generation !== this.generation || this.closing) throw new Error('Owned-document revisit was cancelled.');
      const pid = this.trustedOwnedPid(registration);
      this.ownedRegistration = registration;
      this.options = this.safeOptions({ ...this.options, ownedDocument: registration,
        nativeSource: { ...this.options.nativeSource!, pid, bundleId: OWNED_DOCUMENT_BUNDLE,
          authorizedWindowId: undefined, authorizedProcessIdentity: undefined, referencePlanner: undefined },
        ownedDocumentRootRevisit: undefined });
      this.ownedEditApproved = false; this.ownedSaveAuthorized = false; this.ownedSaveAuthorizedAtMs = 0;
      this.ownedSaveAnchor = null;
      if (this.ownedPhase === 'revisiting') this.ownedPhase = 'observing';
      this.ownedReason = null;
      this.publish();
    } catch {
      if (generation === this.generation) {
        this.ownedPhase = 'unavailable'; this.ownedReason = 'The owned document could not be verified after reopening.';
        this.ownedEditApproved = false; this.ownedSaveAuthorized = false; this.ownedSaveAnchor = null;
      }
      this.publish();
      throw new Error('The owned document could not be verified after reopening.');
    }
  }

  private assertOwnedGeneration(generation: number, controller: AbortController): void {
    if (this.closing || controller.signal.aborted || generation !== this.generation
      || this.configurationLocks.size > 0 || this.cleanupIsUnconfirmed()
      || (this.projection?.root && !TERMINAL.has(this.projection.root.state))) {
      throw new Error('Owned-document authorization was cancelled.');
    }
  }

  private currentPromptRoot(): Omit<OwnedRootAnchor, 'saved_at_ms'> | null {
    const root = this.projection?.root;
    return root ? { root_task_id: root.root_task_id, task_revision: root.task_revision, root_epoch: root.root_epoch } : null;
  }

  private cancelPromptControllers(): void {
    this.editPromptAbort?.abort(); this.editPromptAbort = null;
    this.savePromptAbort?.abort(); this.savePromptAbort = null;
  }

  private sameRoot(root: NonNullable<UiProjection['root']>, anchor: OwnedRootAnchor): boolean {
    return root.root_task_id === anchor.root_task_id && root.task_revision === anchor.task_revision
      && root.root_epoch === anchor.root_epoch;
  }

  private observeOwnedProjection(projection: UiProjection): void {
    const root = projection.root ?? null;
    const rootIdentity: RootIdentity | null = root
      ? { root_task_id: root.root_task_id, task_revision: root.task_revision, root_epoch: root.root_epoch } : null;
    if (!this.sameRootIdentity(rootIdentity, this.ownedObservedRoot)) {
      this.ownedSaveAnchor = null;
      this.ownedObservedRoot = rootIdentity;
      this.ownedSaveNoteHighWaterAtMs = Math.max(this.ownedSaveNoteHighWaterAtMs, this.noteHighWater(projection));
    }
    if (this.ownedPromptActive) {
      const samePromptRoot = this.sameRootIdentity(rootIdentity, this.ownedPromptRoot);
      if (!samePromptRoot) this.cancelPendingConsent();
    }
    if (projection.stop_status !== 'not_stopped' || (root && TERMINAL.has(root.state))) {
      this.ownedSaveAnchor = null;
      this.ownedSaveNoteHighWaterAtMs = Math.max(this.ownedSaveNoteHighWaterAtMs, this.noteHighWater(projection));
    }
    if (this.ownedSaveAnchor && (!root || !this.sameRoot(root, this.ownedSaveAnchor))) this.ownedSaveAnchor = null;

    if (root && !TERMINAL.has(root.state) && projection.stop_status === 'not_stopped' && this.ownedSaveAuthorized) {
      const savedAt = this.noteHighWater(projection);
      if (savedAt > Math.max(this.ownedSaveNoteHighWaterAtMs, this.ownedSaveAuthorizedAtMs)) {
        this.ownedSaveNoteHighWaterAtMs = savedAt;
        this.ownedSaveAnchor = {
          root_task_id: root.root_task_id,
          task_revision: root.task_revision,
          root_epoch: root.root_epoch,
          saved_at_ms: savedAt,
        };
      }
    }

    if (this.ownedRevisitAnchor && root && this.sameRoot(root, this.ownedRevisitAnchor)) {
      if (root.state === 'completed') {
        this.ownedPhase = 'completed';
        this.ownedReason = null;
        this.ownedEditApproved = false;
        this.ownedSaveAuthorized = false;
        this.ownedSaveAnchor = null;
        this.ownedRevisitAnchor = null;
      } else if (TERMINAL.has(root.state)) {
        this.ownedPhase = 'unavailable';
        this.ownedReason = 'The owned document could not be verified for this task.';
        this.ownedEditApproved = false;
        this.ownedSaveAuthorized = false;
        this.ownedSaveAnchor = null;
        this.ownedRevisitAnchor = null;
      }
    } else if (this.ownedRevisitAnchor && (!root || !this.sameRoot(root, this.ownedRevisitAnchor))) {
      this.ownedRevisitAnchor = null;
      this.ownedSaveAnchor = null;
      this.ownedPhase = 'unavailable';
      this.ownedReason = 'The task changed during owned-document verification.';
      this.ownedEditApproved = false;
      this.ownedSaveAuthorized = false;
    }
  }

  private sameRootIdentity(left: RootIdentity | null, right: RootIdentity | null): boolean {
    return left === null ? right === null : right !== null && left.root_task_id === right.root_task_id
      && left.task_revision === right.task_revision && left.root_epoch === right.root_epoch;
  }

  private noteHighWater(projection: UiProjection | null | undefined): number {
    return (projection?.timeline ?? []).filter((entry) => entry.text === OWNED_SAVE_NOTE)
      .reduce((latest, entry) => Math.max(latest, entry.at_ms), 0);
  }

  private connect(pipeline: Pipeline): void {
    this.pipeline = pipeline;
    const image: (...args: SupervisorEvents['imageAvailability']) => void = (...args) => this.emit('imageAvailability', ...args);
    const device: (...args: SupervisorEvents['deviceEvent']) => void = (...args) => this.emit('deviceEvent', ...args);
    const request: (...args: SupervisorEvents['decisionRequest']) => void = (...args) => this.emit('decisionRequest', ...args);
    const cancel: (...args: SupervisorEvents['decisionCancel']) => void = (...args) => this.emit('decisionCancel', ...args);
    const log: (...args: SupervisorEvents['log']) => void = (...args) => this.emit('log', ...args);
    const exit: (...args: SupervisorEvents['exit']) => void = (...args) => {
      this.generation += 1;
      this.emit('sessionChanged');
      this.authorized = null; this.authorizedDomain = null;
      this.phase = 'unavailable'; this.reason = 'A native process exited. Restart observation before granting input again.';
      this.ownedSaveAnchor = null;
      this.ownedRevisitAnchor = null;
      this.ownedEditApproved = false;
      this.ownedSaveAuthorized = false;
      this.ownedPhase = this.ownedRegistration ? 'unavailable' : 'not_created';
      this.ownedReason = this.ownedRegistration
        ? 'A native process exited. Restart the owned-document session before continuing.' : null;
      this.pipeline?.stopAtDevice({ scope: 'all' }, 'cancel', 'operator_button', 'a session process exited');
      this.publish();
      this.publishOwned();
      this.emit('controlStop');
      this.emit('exit', ...args);
    };
    const ownedProjection = (value: SupervisorEvents['projection'][0]): void => {
      this.observeOwnedProjection(value);
      this.emit('projection', this.projection ?? value); this.publish();
    };
    const ownedStop: (...args: SupervisorEvents['stopAck']) => void = (...args) => {
      this.ownedSaveAnchor = null;
      this.emit('stopAck', ...args); this.publishOwned();
    };
    pipeline.on('projection', ownedProjection); pipeline.on('imageAvailability', image); pipeline.on('stopAck', ownedStop);
    pipeline.on('deviceEvent', device); pipeline.on('decisionRequest', request); pipeline.on('decisionCancel', cancel);
    pipeline.on('log', log); pipeline.on('exit', exit);
    this.detach = () => {
      pipeline.off('projection', ownedProjection); pipeline.off('imageAvailability', image); pipeline.off('stopAck', ownedStop);
      pipeline.off('deviceEvent', device); pipeline.off('decisionRequest', request); pipeline.off('decisionCancel', cancel);
      pipeline.off('log', log); pipeline.off('exit', exit);
    };
    if (pipeline.projection) ownedProjection(pipeline.projection);
    else this.publishOwned();
  }

  operator(command: OperatorCommand): void | Promise<void> {
    if (command.command === 'stop_task') {
      this.ownedActivationGeneration += 1;
      this.ownedSaveAnchor = null;
      this.emit('controlStop');
      this.cancelPendingConsent();
      if (this.ownedPhase === 'creating' || this.ownedPhase === 'revisiting') {
        this.generation += 1; this.ownedPhase = 'unavailable';
        this.ownedReason = 'The owned-document operation was stopped before verification completed.';
      }
    } else if (this.closing || this.configurationLocks.size > 0 || this.phase === 'authorizing' || this.phase === 'switching' || this.phase === 'unavailable' || !this.pipeline
      || ['creating', 'authorizing_edit', 'authorizing_save', 'revisiting'].includes(this.ownedPhase)) {
      throw new Error('The session is changing or unavailable. Wait for observation before starting a task.');
    } else if (command.command === 'set_mode' && command.mode === 'execute' && this.cleanupIsUnconfirmed()) {
      throw new Error('Input cleanup is unconfirmed. Execution remains closed.');
    } else if (this.ownedRegistration && command.command === 'create_task'
      && (!this.ownedEditApproved || !this.ownedSaveAuthorized || !this.ownedServices?.canPlan()
        || this.ownedPhase !== 'save_authorized')) {
      throw new Error('Owned-document tasks require approved editing, Save, and an authorized S2 planning model.');
    } else if (this.ownedRegistration && command.command === 'set_mode' && command.mode === 'execute') {
      if (!this.ownedEditApproved || !this.ownedSaveAuthorized || !this.ownedServices?.canPlan()) {
        throw new Error('Owned-document execution requires approved editing, Save, and an authorized S2 planning model.');
      }
      return this.activateOwnedBeforeExecute(command);
    } else if (command.command === 'set_mode' && command.mode === 'execute' && this.options.nativeSource && !this.authorized) {
      throw new Error('Authorize the observed window before enabling native execution.');
    }
    return this.pipeline?.operator(command);
  }

  stopAtDevice(scope: StopScope, disposition: StopDisposition, source: StopSource, reason?: string): void {
    this.ownedActivationGeneration += 1;
    this.ownedActivationPending = false;
    this.emit('controlStop');
    this.cancelPendingConsent();
    this.ownedSaveAnchor = null;
    if (this.ownedPhase === 'creating' || this.ownedPhase === 'revisiting') {
      this.generation += 1; this.ownedPhase = 'unavailable';
      this.ownedReason = 'The owned-document operation was stopped before verification completed.';
    }
    this.pipeline?.stopAtDevice(scope, disposition, source, reason);
    if (this.phase === 'switching' && !this.closing && this.configurationLocks.size === 0) void this.revokeNativeWindow().catch((error: unknown) => this.emit('log', String(error)));
  }

  private cancelPendingConsent(): void {
    const pendingWindow = this.phase === 'authorizing';
    const pendingEdit = this.ownedPhase === 'authorizing_edit';
    const pendingSave = this.ownedPhase === 'authorizing_save';
    if (!pendingWindow && !pendingEdit && !pendingSave) return;
    this.generation += 1; this.cancelPromptControllers(); this.ownedPromptActive = false; this.ownedPromptRoot = null;
    if (pendingWindow) { this.phase = 'observing'; this.reason = 'Window authorization cancelled.'; }
    if (pendingEdit) { this.ownedPhase = 'observing'; this.ownedReason = 'Owned-document editing was cancelled.'; }
    if (pendingSave) { this.ownedPhase = 'edit_authorized'; this.ownedReason = 'Owned-document Save authorization was cancelled.'; }
    this.publish();
  }

  private async activateOwnedBeforeExecute(command: OperatorCommand): Promise<void> {
    if (this.ownedActivationPending || !this.ownedRegistration || !this.pipeline) {
      throw new Error('Owned-document activation is already changing or unavailable.');
    }
    const pipeline = this.pipeline;
    const registration = this.ownedRegistration;
    const root = pipeline.projection?.root ?? null;
    const generation = ++this.ownedActivationGeneration;
    this.ownedActivationPending = true;
    try {
      if (!await activateOwnedDocumentForObservation(registration)) throw new Error('activation refused');
      const currentRoot = pipeline.projection?.root ?? null;
      const rootUnchanged = root === null ? currentRoot === null : !!currentRoot
        && currentRoot.root_task_id === root.root_task_id && currentRoot.task_revision === root.task_revision
        && currentRoot.root_epoch === root.root_epoch && currentRoot.state === root.state;
      if (this.closing || generation !== this.ownedActivationGeneration || this.pipeline !== pipeline
        || this.ownedRegistration !== registration || !rootUnchanged || !this.ownedEditApproved
        || !this.ownedSaveAuthorized || !this.ownedServices?.canPlan() || this.cleanupIsUnconfirmed()) {
        throw new Error('authorization changed');
      }
      pipeline.operator(command);
    } catch {
      throw new Error('TextEdit could not be safely activated; execution was not enabled.');
    } finally {
      if (generation === this.ownedActivationGeneration) this.ownedActivationPending = false;
    }
  }

  sendDecisionProposal(proposal: DecisionProposal): void {
    if (!this.closing && (this.phase === 'authorized' || this.phase === 'observing' || this.phase === 'not_native')) this.pipeline?.sendDecisionProposal(proposal);
  }

  async authorizeNativeWindow(snapshotId: string): Promise<void> {
    if (this.ownedRegistration) throw new Error('Owned TextEdit uses its separate private-document permission.');
    const status = this.authorization;
    const initialCandidate = this.candidateEvidence();
    const target = initialCandidate?.target;
    if (!status.can_authorize || !initialCandidate || !target || target.snapshot_id !== snapshotId) throw new Error('The observed window is stale, unavailable, or busy. Select the current window again.');
    const generation = ++this.generation;
    this.phase = 'authorizing'; this.reason = null; this.publish();
    try {
      const approved = await this.confirm(target, initialCandidate.domain);
      if (this.closing || generation !== this.generation) throw new Error('Window authorization cancelled.');
      if (!approved) return;
      const currentCandidate = this.candidateEvidence();
      const current = currentCandidate?.target;
      const unchanged = this.sameConsentCandidate(initialCandidate, currentCandidate);
      if (!unchanged || !current) {
        throw new Error('The observed window changed during confirmation. Authorize the current window again.');
      }
      if (this.closing || generation !== this.generation) throw new Error('Window authorization cancelled.');
      if (this.cleanupIsUnconfirmed()) throw new Error('Input cleanup is unconfirmed.');
      if (this.configurationLocks.size > 0 || (this.projection?.root && !TERMINAL.has(this.projection.root.state))) {
        throw new Error('The session became busy during confirmation. Window permission remains closed.');
      }
      const sourceReadWindowId = this.options.nativeSource?.sourceReadWindowId;
      if (sourceReadWindowId !== undefined && current.window_id !== sourceReadWindowId) {
        throw new Error('The authorized window does not match the fixed Source read window.');
      }
      const nativeSource = { ...this.options.nativeSource!, pid: target.pid };
      delete nativeSource.authorizedProcessIdentity;
      delete nativeSource.authorizedBackgroundWindowId;
      if (initialCandidate.domain === 'background_ax') {
        const backgroundAnchor = initialCandidate.backgroundAnchor;
        if (!backgroundAnchor || nativeSource.backgroundWindowId !== target.window_id) {
          throw new Error('The BackgroundAx target no longer matches its fixed Source policy.');
        }
        nativeSource.authorizedWindowId = undefined;
        nativeSource.authorizedBackgroundWindowId = target.window_id;
        nativeSource.authorizedProcessIdentity = {
          pid: backgroundAnchor.pid,
          start_seconds: backgroundAnchor.processStartSeconds,
          start_micros: backgroundAnchor.processStartMicros,
        };
      } else {
        nativeSource.authorizedWindowId = this.options.ownedDocument ? undefined : target.window_id;
        const scopedAnchor = initialCandidate.scopedAnchor;
        if (nativeSource.authorizedWindowId !== undefined && scopedAnchor) {
          nativeSource.authorizedProcessIdentity = {
            pid: scopedAnchor.pid,
            start_seconds: scopedAnchor.processStartSeconds,
            start_micros: scopedAnchor.processStartMicros,
          };
        }
      }
      await this.replace(generation, { ...this.options, mode: 'observe', nativeSource, ...(this.options.ownedDocument ? {
        ownedDocumentValueAuthorization: authorizeOwnedDocumentValueInput(this.options.ownedDocument),
      } : {}) }, target, initialCandidate);
    } finally {
      if (generation === this.generation && this.phase === 'authorizing') { this.phase = 'observing'; this.publish(); }
    }
  }

  /** Restart only a fixed scoped or background read-only Source lease. This path never
   * carries a prior Device window or kernel birth-stamp grant into the new lease. */
  async refreshNativeObservation(): Promise<void> {
    if (!this.canRefreshObservation()) {
      throw new Error('Native observation cannot be refreshed while the session is busy, active, or cleanup is uncertain.');
    }
    const generation = ++this.generation;
    const options = this.safeOptions({ ...this.options, mode: 'observe' });
    this.phase = 'switching'; this.authorized = null; this.authorizedDomain = null;
    this.reason = 'Refreshing read-only observation…'; this.publish();
    await this.replace(generation, options, null, null, true);
  }

  async revokeNativeWindow(): Promise<void> {
    if (!this.options.nativeSource || this.closing) return;
    this.emit('controlStop');
    this.ownedActivationGeneration += 1;
    this.ownedActivationPending = false;
    this.cancelPendingConsent();
    if (this.ownedPhase === 'creating' || this.ownedPhase === 'revisiting') {
      this.generation += 1;
      this.ownedPhase = 'unavailable'; this.ownedReason = 'The owned-document operation was cancelled.';
    }
    const generation = ++this.generation;
    this.phase = 'switching'; this.reason = 'Closing the input session…'; this.publish();
    this.pipeline?.stopAtDevice({ scope: 'all' }, 'cancel', 'operator_button', 'window authorization revoked');
    this.pipeline?.operator({ command: 'stop_task', root_task_id: null });
    await this.change?.catch(() => undefined);
    await this.ownedLifecycleTask?.catch(() => undefined);
    if (this.closing || generation !== this.generation) return;
    if (this.ownedRegistration) {
      this.ownedEditApproved = false; this.ownedSaveAuthorized = false; this.ownedSaveAuthorizedAtMs = 0;
      this.ownedSaveAnchor = null; this.ownedRevisitAnchor = null;
      this.ownedPhase = 'observing'; this.ownedReason = null;
      await this.replace(generation, this.safeOptions({ ...this.options, ownedDocumentValueAuthorization: undefined,
        ownedDocumentSaveAuthorization: undefined, ownedDocumentReferencePlanner: undefined,
        ownedDocumentRootRevisit: undefined, mode: 'observe' }), null);
      this.publishOwned();
    } else await this.replace(generation, this.safeOptions(this.options), null);
  }

  private async replace(generation: number, options: SupervisorOptions, target: NativeWindowTarget | null,
    consentCandidate: NativeCandidate | null = null, observationRefresh = false): Promise<void> {
    this.phase = 'switching'; this.authorized = null; this.authorizedDomain = null; this.publish();
    this.emit('sessionChanged');
    const old = this.pipeline;
    this.detach?.(); this.detach = null;
    const change = (async () => {
      if (!observationRefresh) old?.stopAtDevice({ scope: 'all' }, 'cancel', 'operator_button', 'replacing the input session');
      await old?.shutdown();
      // New pipes do not prove that an old held input was released. Preserve
      // uncertainty across model/window revocation instead of laundering it
      // through the replacement's fresh "not_needed" projection.
      const cleanup = old?.projection?.cleanup;
      if (cleanup === 'held' || cleanup === 'unknown' || cleanup === 'failed') this.cleanupUnconfirmed = true;
      if (this.closing || generation !== this.generation) return;
      if (observationRefresh && !this.canRefreshObservationDuringReplacement()) {
        throw new Error('Native observation became busy or cleanup became uncertain before refresh.');
      }
      if (target && this.cleanupIsUnconfirmed()) {
        throw new Error('Input cleanup is unconfirmed. The authorized window session was not started.');
      }
      if (target && this.projection?.root && !TERMINAL.has(this.projection.root.state)) {
        throw new Error('The task became busy before the authorized window session could start.');
      }
      if (target && consentCandidate && !this.sameConsentCandidate(consentCandidate, this.candidateEvidence())) {
        throw new Error('The observed window changed or expired before the authorized session could start.');
      }
      this.pipeline = null;
      this.emit('imageAvailability', { frame_id: null, expires_unix_ms: null });
      if (this.closing || generation !== this.generation) return;
      const next = await this.start(options);
      if (this.closing || generation !== this.generation) { await next.shutdown(); return; }
      if (observationRefresh && (next.projection?.root && !TERMINAL.has(next.projection.root.state)
        || next.projection?.cleanup === 'held' || next.projection?.cleanup === 'unknown' || next.projection?.cleanup === 'failed')) {
        await next.shutdown();
        if (next.projection?.cleanup === 'held' || next.projection?.cleanup === 'unknown' || next.projection?.cleanup === 'failed') {
          this.cleanupUnconfirmed = true;
        }
        throw new Error('The refreshed observation started with an active task or uncertain cleanup.');
      }
      this.authorized = target;
      this.authorizedDomain = target ? (options.nativeSource?.authorizedBackgroundWindowId ? 'background_ax' : 'foreground_native') : null;
      this.phase = target ? 'authorized' : options.nativeSource ? 'observing' : 'not_native'; this.reason = null;
      this.options = this.safeOptions(options);
      this.connect(next); this.publish();
    })();
    this.change = change;
    try { await change; }
    catch (error) {
      if (generation === this.generation) { this.phase = 'unavailable'; this.reason = 'The session could not restart. Native input remains closed.'; this.publish(); }
      throw error;
    } finally { if (this.change === change) this.change = null; }
  }

  /** The refresh's own change promise is active here, so check every other
   * admission gate without treating that self-owned transition as a conflict. */
  private canRefreshObservationDuringReplacement(): boolean {
    const root = this.projection?.root;
    return !!this.options.nativeSource && !this.closing && this.configurationLocks.size === 0
      && !this.cleanupIsUnconfirmed() && !this.ownedRegistration && !this.ownedLifecycleTask
      && !this.ownedActivationPending
      && !['creating', 'authorizing_edit', 'authorizing_save', 'revisiting'].includes(this.ownedPhase)
      && !(root && !TERMINAL.has(root.state));
  }

  shutdown(): Promise<void> {
    this.shutdownTask ??= (async () => {
      this.closing = true; this.generation += 1; this.emit('sessionChanged');
      this.ownedActivationGeneration += 1; this.cancelPromptControllers();
      this.ownedPromptActive = false; this.ownedPromptRoot = null;
      this.ownedSaveAnchor = null; this.ownedRevisitAnchor = null;
      this.emit('controlStop');
      this.detach?.(); this.detach = null;
      this.pipeline?.stopAtDevice({ scope: 'all' }, 'cancel', 'operator_button', 'the product is quitting');
      this.pipeline?.operator({ command: 'stop_task', root_task_id: null });
      await this.change?.catch(() => undefined);
      await this.ownedLifecycleTask?.catch(() => undefined);
      await this.pipeline?.shutdown(); this.pipeline = null;
      if (this.ownedRunner) {
        const stopped = await this.ownedRunner.stopAll().catch(() => ['unknown'] as const);
        if (stopped.some((result) => result !== 'stopped')) {
          this.cleanupUnconfirmed = true; this.ownedPhase = 'unavailable';
          this.ownedReason = 'Owned-document cleanup is unconfirmed after shutdown.';
        } else {
          this.ownedRunner = null; this.ownedRegistration = null;
          this.ownedEditApproved = false; this.ownedSaveAuthorized = false;
        }
      }
      this.publishOwned();
    })();
    return this.shutdownTask;
  }

  get canConfigureModel(): boolean {
    return !this.closing && this.configurationLocks.size === 0 && this.pipeline !== null
      && this.options.browserPageProfile === undefined
      && !this.ownedActivationPending
      && this.phase !== 'authorizing' && this.phase !== 'switching' && this.phase !== 'unavailable'
      && !['creating', 'authorizing_edit', 'authorizing_save', 'revisiting'].includes(this.ownedPhase)
      && !(this.projection?.root && !TERMINAL.has(this.projection.root.state))
      && !this.cleanupIsUnconfirmed();
  }

  private cleanupIsUnconfirmed(): boolean {
    const cleanup = this.projection?.cleanup;
    return this.cleanupUnconfirmed || cleanup === 'held' || cleanup === 'unknown' || cleanup === 'failed';
  }

  lockConfiguration(force = false): () => void {
    if (this.closing || (!force && !this.canConfigureModel)) throw new Error('Finish or stop the current task before changing the model connection.');
    const token = Symbol('model configuration'); this.configurationLocks.add(token); this.publish();
    return () => { this.configurationLocks.delete(token); this.publish(); };
  }

  /** Model changes close the old provider and input pipes; window permission
   * must be granted again against the new observation session. */
  async restartForModelChange(): Promise<void> {
    if (this.closing) throw new Error('The product is closing.');
    const generation = ++this.generation;
    this.ownedActivationGeneration += 1; this.cancelPendingConsent(); this.cancelPromptControllers();
    this.ownedActivationPending = false;
    this.ownedEditApproved = false; this.ownedSaveAuthorized = false; this.ownedSaveAuthorizedAtMs = 0;
    this.ownedSaveAnchor = null; this.ownedRevisitAnchor = null;
    if (this.ownedRegistration) { this.ownedPhase = 'observing'; this.ownedReason = null; }
    this.options = this.safeOptions(this.options);
    this.phase = 'switching'; this.reason = 'Changing the model connection…'; this.publish();
    this.pipeline?.stopAtDevice({ scope: 'all' }, 'cancel', 'operator_button', 'model connection changed');
    this.pipeline?.operator({ command: 'stop_task', root_task_id: null });
    await this.change?.catch(() => undefined);
    await this.ownedLifecycleTask?.catch(() => undefined);
    if (this.closing || generation !== this.generation) throw new Error('Model connection change cancelled.');
    await this.replace(generation, this.safeOptions(this.options), null);
  }
}
