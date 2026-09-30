import { spawn, type ChildProcess } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
  encodeFrame,
  EnvelopeFactory,
  FrameDecoder,
  FrameError,
  type DecisionCancel,
  type DecisionProposal,
  type DecisionRequest,
  type DeviceEvent,
  type Message,
  type OperatorCommand,
  type RunMode,
  type StopAck,
  type StopDisposition,
  type StopScope,
  type StopSource,
  type UiProjection,
} from '@computer-user/desktop-api';

import type { Binaries } from './binaries.js';

export type Role = 'device' | 'source' | 'host';

export interface SupervisorOptions {
  binaries: Binaries;
  /** The mode the host starts in. It is the operator's to raise, never a model's. */
  mode?: RunMode;
  readyTimeoutMs?: number;
}

export interface SupervisorEvents {
  projection: [UiProjection];
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
}

const token = (): string => randomBytes(16).toString('hex');

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
  private ready = new Map<Role, () => void>();
  private shuttingDown = false;

  private constructor(
    private readonly options: SupervisorOptions,
    private readonly tokens: { device: string; source: string },
  ) {
    super();
    // Short, because a Unix socket path has a small limit.
    this.dir = mkdtempSync(join(tmpdir(), 'cu'));
  }

  static async start(options: SupervisorOptions): Promise<Supervisor> {
    const supervisor = new Supervisor(options, { device: token(), source: token() });
    try {
      await supervisor.launch();
    } catch (error) {
      await supervisor.shutdown();
      throw error;
    }
    return supervisor;
  }

  get projection(): UiProjection | null {
    return this.latest;
  }

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
    await this.spawn('device', this.options.binaries.device, 1, this.settings({ host_token: this.tokens.device }), timeout);
    await this.spawn('source', this.options.binaries.source, 1, this.settings({ host_token: this.tokens.source }), timeout);
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
      }),
      timeout,
    );
  }

  private async spawn(
    role: Role,
    program: string,
    sessionEpoch: number,
    settings: Record<string, string>,
    timeoutMs: number,
  ): Promise<void> {
    const process = spawn(program, [], { stdio: ['pipe', 'pipe', 'pipe'] });
    const child: Child = {
      role,
      process,
      factory: new EnvelopeFactory('supervisor', () => Date.now() - this.clockOriginMs),
      decoder: new FrameDecoder(),
      open: true,
      exited: new Promise((done) => {
        process.once('close', (code, signal) => {
          child.open = false;
          if (!this.shuttingDown) this.emit('exit', role, code, signal);
          done();
        });
      }),
    };
    this.children.set(role, child);
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
    this.write(child, { type: 'configure', payload: { role, session_epoch: sessionEpoch, settings } });
    await ready;
  }

  private write(child: Child, message: Message): void {
    if (!child.open || !child.process.stdin?.writable) return;
    child.process.stdin.write(encodeFrame(child.factory.make(message)));
  }

  private onData(child: Child, chunk: Buffer): void {
    child.decoder.push(chunk);
    try {
      for (let envelope = child.decoder.next(); envelope; envelope = child.decoder.next()) {
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
        this.latest = message.payload;
        this.emit('projection', message.payload);
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
      default:
        break;
    }
  }

  // ---- What main can do ------------------------------------------------------------------

  private toHost(message: Message): void {
    const host = this.children.get('host');
    if (host) this.write(host, message);
  }

  operator(command: OperatorCommand): void {
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
    const device = this.children.get('device');
    if (device) this.write(device, { type: 'stop_command', payload: { scope, disposition, source, reason } });
  }

  injectHumanInput(): void {
    const device = this.children.get('device');
    if (device) this.write(device, { type: 'sim_control', payload: { op: { op: 'inject_human_input' } } });
  }

  /** Main goes away, or is told to: pipes close and every process leaves. */
  async shutdown(): Promise<void> {
    if (this.shuttingDown) return;
    this.shuttingDown = true;
    const all = [...this.children.values()];
    for (const child of all) {
      this.write(child, { type: 'shutdown', payload: { reason: 'the supervisor is shutting down' } });
      child.process.stdin?.end();
    }
    const grace = new Promise<void>((done) => setTimeout(done, 3_000).unref());
    await Promise.race([Promise.all(all.map((child) => child.exited)), grace]);
    for (const child of all) if (child.open) child.process.kill('SIGKILL');
    await Promise.all(all.map((child) => child.exited));
    rmSync(this.dir, { recursive: true, force: true });
  }
}
