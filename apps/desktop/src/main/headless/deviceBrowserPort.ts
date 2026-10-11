import { randomBytes } from 'node:crypto';
import type { ChildProcess } from 'node:child_process';
import type { Duplex } from 'node:stream';

import {
  validateBrowserControlHello,
  validateBrowserControlReady,
  validateBrowserControlReply,
  type BrowserControlBootstrap,
  type BrowserControlHello,
  type BrowserControlReply,
  type BrowserControlReady,
  type BrowserControlSubmit,
  type BrowserPageProfile,
  type BrowserReadScope,
} from '@computer-use/desktop-api';

import {
  BrowserControlFrameDecoder,
  encodeBrowserControlBootstrap,
  encodeBrowserControlReady,
  encodeBrowserControlReply,
} from './browserControlWire.js';
import type { BrowserSourceErrorCode } from './browserSource.js';

const CONTROL_PROFILE: BrowserPageProfile = Object.freeze({ kind: 'fixture_background_option' });
const HELLO_TIMEOUT_MS = 5_000;

export interface DeviceBrowserActionResource {
  /** The only operation accepted from this authenticated Device channel. */
  submit(value: BrowserControlSubmit): Promise<BrowserControlReply>;
  /** Must synchronously retire the source and exact Chrome resource before returning its cleanup promise. */
  retireNow(reason: Exclude<BrowserSourceErrorCode, 'invalid_input'>): Promise<void>;
}

export interface DeviceBrowserPortOptions {
  readonly stream: Duplex;
  readonly device: ChildProcess;
  readonly scope: BrowserReadScope;
  readonly sessionEpoch: number;
  readonly channelGeneration: number;
  readonly browserPageProfile: BrowserPageProfile;
  readonly nowMs: () => number;
  readonly resource: DeviceBrowserActionResource;
}

function validOptions(options: DeviceBrowserPortOptions): boolean {
  return options !== null && typeof options === 'object'
    && options.stream !== null && options.stream !== undefined && typeof options.stream.write === 'function'
    && options.device !== null && options.device !== undefined
    && Number.isSafeInteger(options.device.pid) && Number(options.device.pid) > 0
    && Number.isSafeInteger(options.sessionEpoch) && options.sessionEpoch > 0
    && Number.isSafeInteger(options.channelGeneration) && options.channelGeneration > 0
    && options.scope !== null && typeof options.scope === 'object'
    && options.scope.context !== null && typeof options.scope.context === 'object'
    && options.scope.context.session_epoch === options.sessionEpoch
    && options.browserPageProfile !== null && typeof options.browserPageProfile === 'object'
    && Object.keys(options.browserPageProfile).length === 1
    && options.browserPageProfile.kind === CONTROL_PROFILE.kind
    && typeof options.nowMs === 'function'
    && options.resource !== null && options.resource !== undefined
    && typeof options.resource.submit === 'function'
    && typeof options.resource.retireNow === 'function';
}

function writeFrame(stream: Duplex, frame: Buffer): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    if (stream.destroyed || !stream.writable) {
      frame.fill(0);
      reject(new Error('browser Device port is closed'));
      return;
    }
    try {
      stream.write(frame, (error?: Error | null) => {
        frame.fill(0);
        if (error) reject(new Error('browser Device port write failed'));
        else resolve();
      });
    } catch {
      frame.fill(0);
      reject(new Error('browser Device port write failed'));
    }
  });
}

/**
 * Main-owned FD3 peer. It has no public submit entry point, permit table, or
 * generic browser command interface; all operation bytes originate at Device.
 */
export class DeviceBrowserPort {
  private readonly bootstrap: BrowserControlBootstrap;
  private readonly decoder: BrowserControlFrameDecoder;
  private readonly expectedPid: number;
  private startTask: Promise<void> | null = null;
  private startResolve: (() => void) | null = null;
  private startReject: ((error: Error) => void) | null = null;
  private helloTimer: ReturnType<typeof setTimeout> | null = null;
  private replyTimer: ReturnType<typeof setTimeout> | null = null;
  private ready = false;
  private helloReceived = false;
  private retired = false;
  private accepting = false;
  private processingSubmit = false;
  private resourceRetired = false;
  private cleanupTask: Promise<void> | null = null;
  private readonly onStreamData = (chunk: Buffer): void => this.handleData(chunk);
  private readonly onStreamError = (): void => { void this.retire('session_closed'); };
  private readonly onStreamEnd = (): void => { void this.retire('session_closed'); };
  private readonly onStreamClose = (): void => { void this.retire('session_closed'); };
  private readonly onChildClose = (): void => { void this.retire('session_closed'); };
  private readonly onChildError = (): void => { void this.retire('session_closed'); };

  constructor(private readonly options: DeviceBrowserPortOptions) {
    if (!validOptions(options)) throw new Error('browser Device port options are invalid');
    this.expectedPid = Number(options.device.pid);
    this.bootstrap = Object.freeze({
      version: 1,
      token: randomBytes(32).toString('hex'),
      channel_generation: options.channelGeneration,
      browser_page_profile: CONTROL_PROFILE,
      scope: options.scope,
    });
    this.decoder = new BrowserControlFrameDecoder(this.bootstrap);
  }

  isReady(): boolean {
    return this.ready && this.accepting && !this.retired && this.childIsCurrent();
  }

  isRetired(): boolean { return this.retired || this.resourceRetired; }

  /**
   * Starts the bootstrap write synchronously before returning. Supervisor must
   * call this before writing the Device's public Configure frame.
   */
  start(): Promise<void> {
    if (this.startTask) return this.startTask;
    if (this.retired || !this.childIsCurrent()) return Promise.reject(new Error('browser Device port is retired'));
    this.startTask = new Promise<void>((resolve, reject) => {
      this.startResolve = resolve;
      this.startReject = reject;
    });
    this.options.stream.on('data', this.onStreamData);
    this.options.stream.once('error', this.onStreamError);
    this.options.stream.once('end', this.onStreamEnd);
    this.options.stream.once('close', this.onStreamClose);
    this.options.device.once('close', this.onChildClose);
    this.options.device.once('error', this.onChildError);
    this.helloTimer = setTimeout(() => {
      this.failStart('browser Device control hello timed out');
    }, HELLO_TIMEOUT_MS);
    this.helloTimer.unref?.();
    try {
      // The first frame is sent before start() returns, preserving FD3-before-Configure ordering.
      const frame = encodeBrowserControlBootstrap(this.bootstrap);
      void writeFrame(this.options.stream, frame).catch(() => this.failStart('browser Device bootstrap failed'));
    } catch {
      this.failStart('browser Device bootstrap failed');
    }
    return this.startTask;
  }

  /** Synchronously closes FD3, permanently retires the scope, then returns asynchronous cleanup. */
  retire(reason: Exclude<BrowserSourceErrorCode, 'invalid_input'> = 'stopped'): Promise<void> {
    if (this.retired) return this.cleanupTask ?? Promise.resolve();
    this.retired = true;
    this.ready = false;
    this.accepting = false;
    this.rejectStartIfPending('browser Device control channel retired before readiness');
    this.clearTimers();
    this.decoder.dispose();
    this.removeListeners();
    if (!this.options.stream.destroyed) this.options.stream.destroy();
    this.retireResourceNow(reason);
    return this.cleanupTask ?? Promise.resolve();
  }

  close(reason: Exclude<BrowserSourceErrorCode, 'invalid_input'> = 'stopped'): Promise<void> { return this.retire(reason); }

  private handleData(chunk: Buffer): void {
    if (this.retired || !this.ready && (!this.startTask || this.helloReceived) || this.processingSubmit) {
      void this.retire('source_unavailable');
      return;
    }
    void this.decoder.push(chunk, (message) => this.deliver(message)).catch(() => {
      void this.retire('source_unavailable');
    });
  }

  private async deliver(message: { kind: 'hello'; value: BrowserControlHello } |
    { kind: 'submit'; value: BrowserControlSubmit }): Promise<void> {
    if (this.retired || !this.childIsCurrent()) throw new Error('browser Device peer changed');
    if (message.kind === 'hello') {
      if (this.ready || this.decoder.buffered !== 0) throw new Error('browser Device hello was duplicated or pipelined');
      const hello: BrowserControlHello = message.value;
      const ready: BrowserControlReady = {
        version: 1,
        channel_generation: this.bootstrap.channel_generation,
        session_epoch: this.options.sessionEpoch,
        scope_id: this.bootstrap.scope.scope_id,
        scope_generation: this.bootstrap.scope.generation,
        next_sequence: 1,
      };
      if (!validateBrowserControlHello(hello, this.bootstrap).ok
        || !validateBrowserControlReady(ready, hello).ok) throw new Error('browser Device hello is invalid');
      this.helloReceived = true;
      await writeFrame(this.options.stream, encodeBrowserControlReady(ready));
      if (this.retired || !this.childIsCurrent()) throw new Error('browser Device peer changed');
      this.ready = true;
      this.accepting = true;
      this.clearHelloTimer();
      this.startResolve?.();
      this.startResolve = null;
      this.startReject = null;
      return;
    }

    if (!this.isReady() || this.processingSubmit || this.decoder.buffered !== 0) {
      throw new Error('browser Device submit arrived while the port was busy');
    }
    this.processingSubmit = true;
    const submit = message.value;
    const deadline = submit.authority.reply_deadline_ms;
    let nowMs: number;
    try { nowMs = this.options.nowMs(); }
    catch { throw new Error('browser Device clock is unavailable'); }
    if (!Number.isSafeInteger(nowMs) || nowMs < 0 || nowMs >= deadline) {
      const reply: BrowserControlReply = {
        version: 1,
        request_id: submit.request_id,
        channel_generation: submit.channel_generation,
        session_epoch: submit.session_epoch,
        scope_id: submit.scope_id,
        scope_generation: submit.scope_generation,
        sequence: submit.sequence,
        intent_id: submit.intent.intent_id,
        outcome: 'rejected',
        submitted_at_ms: null,
        input_end_ms: null,
        failure_code: 'evidence_expired',
      };
      await this.sendReply(reply, submit);
      this.processingSubmit = false;
      return;
    }

    this.replyTimer = setTimeout(() => {
      void this.retire('source_unavailable');
    }, Math.max(1, deadline - nowMs));
    this.replyTimer.unref?.();
    try {
      const reply = await this.options.resource.submit(submit);
      if (this.retired || !this.childIsCurrent()) return;
      let afterMs: number;
      try { afterMs = this.options.nowMs(); }
      catch { throw new Error('browser Device clock is unavailable'); }
      if (!Number.isSafeInteger(afterMs) || afterMs >= deadline) {
        // Core may already have fenced this submission. Never deliver a late Completed reply.
        void this.retire('source_unavailable');
        return;
      }
      const validation = await validateBrowserControlReply(reply, submit);
      if (!validation.ok) throw new Error('browser source reply is invalid');
      if (reply.outcome === 'unknown') {
        this.accepting = false;
        this.ready = false;
        this.retireResourceNow('source_unavailable');
        await this.sendReply(reply, submit);
        this.processingSubmit = false;
        await this.retireTransportOnly();
        return;
      }
      if (reply.outcome === 'rejected' && reply.failure_code === 'source_invalid') {
        this.accepting = false;
        this.ready = false;
        this.retireResourceNow('source_unavailable');
        await this.sendReply(reply, submit);
        this.processingSubmit = false;
        await this.retireTransportOnly();
        return;
      }
      await this.sendReply(reply, submit);
      this.clearReplyTimer();
      this.processingSubmit = false;
    } catch {
      void this.retire('source_unavailable');
    }
  }

  private async sendReply(reply: BrowserControlReply, submit: BrowserControlSubmit): Promise<void> {
    const frame = await encodeBrowserControlReply(reply, submit);
    if (this.retired || !this.childIsCurrent()) {
      frame.fill(0);
      throw new Error('browser Device peer changed');
    }
    await writeFrame(this.options.stream, frame);
  }

  private childIsCurrent(): boolean {
    return Number(this.options.device.pid) === this.expectedPid
      && this.options.device.exitCode === null
      && this.options.device.signalCode === null
      && !this.options.device.killed;
  }

  private failStart(message: string): void {
    this.rejectStartIfPending(message);
    void this.retire('source_unavailable');
  }

  private rejectStartIfPending(message: string): void {
    if (!this.startReject) return;
    const reject = this.startReject;
    this.startReject = null;
    this.startResolve = null;
    reject(new Error(message));
  }

  private retireResourceNow(reason: Exclude<BrowserSourceErrorCode, 'invalid_input'>): void {
    if (this.resourceRetired) return;
    this.resourceRetired = true;
    try {
      this.cleanupTask = Promise.resolve(this.options.resource.retireNow(reason)).catch(() => undefined);
    } catch {
      this.cleanupTask = Promise.resolve();
    }
  }

  private async retireTransportOnly(): Promise<void> {
    if (this.retired) return;
    this.retired = true;
    this.ready = false;
    this.accepting = false;
    this.clearTimers();
    this.decoder.dispose();
    this.removeListeners();
    if (!this.options.stream.destroyed) this.options.stream.destroy();
  }

  private removeListeners(): void {
    this.options.stream.off('data', this.onStreamData);
    this.options.stream.off('error', this.onStreamError);
    this.options.stream.off('end', this.onStreamEnd);
    this.options.stream.off('close', this.onStreamClose);
    this.options.device.off('close', this.onChildClose);
    this.options.device.off('error', this.onChildError);
  }

  private clearHelloTimer(): void {
    if (this.helloTimer) clearTimeout(this.helloTimer);
    this.helloTimer = null;
  }

  private clearReplyTimer(): void {
    if (this.replyTimer) clearTimeout(this.replyTimer);
    this.replyTimer = null;
  }

  private clearTimers(): void {
    this.clearHelloTimer();
    this.clearReplyTimer();
  }
}
