import { Worker } from 'node:worker_threads';
import type { FrameMeta, ImageAvailability, ObservationImage, UiProjection } from '@computer-use/desktop-api';
import { frameCapture, frameImageId, frameStream, frameGeometry, frameWatchId } from '@computer-use/desktop-api';

export class ImageClient {
  private worker: Worker | null = null;
  private latest: { meta: FrameMeta; bytes: Uint8Array } | null = null;
  private capture: { meta: FrameMeta; bytes: Uint8Array } | null = null;
  private captureTimer: ReturnType<typeof setTimeout> | null = null;
  constructor(private readonly changed: () => void, private readonly log: (reason: string) => void) {}
  async start(path: string, token: string, epoch: number, pid?: number, clockOriginMs?: number): Promise<void> {
    const extension = import.meta.url.endsWith('.ts') ? 'ts' : 'js';
    const worker = new Worker(new URL(`./imageWorker.${extension}`, import.meta.url), { workerData: { path, token, epoch, pid } });
    this.worker = worker;
    await new Promise<void>((done, fail) => {
      const timeout = setTimeout(() => fail(new Error('image worker did not become ready')), 2500);
      worker.on('message', (message: { kind: string; meta?: FrameMeta; bytes?: ArrayBuffer; reason?: string }) => {
        if (this.worker !== worker) return;
        if (message.kind === 'ready') { clearTimeout(timeout); done(); }
        else if (message.kind === 'image') {
          const packet = { meta: message.meta!, bytes: new Uint8Array(message.bytes!) };
          const capture = frameCapture(packet.meta);
          if (capture) {
            this.clearCapture();
            const remaining = clockOriginMs === undefined ? 2000
              : clockOriginMs + Math.min(capture.deadline_ms, capture.capture_started_ms + 2000) - Date.now();
            if (remaining > 0) {
              this.capture = packet;
              this.captureTimer = setTimeout(() => { this.clearCapture(); this.changed(); }, Math.min(2000, remaining));
              this.captureTimer.unref();
            }
          } else {
            if (this.capture && frameWatchId(this.capture.meta) !== frameWatchId(packet.meta)) this.clearCapture();
            this.latest = packet;
          }
          worker.postMessage({ kind: 'ack' }); this.changed();
        } else if (message.kind === 'closed') {
          this.latest = null; this.clearCapture(); clearTimeout(timeout); this.log(message.reason ?? 'image channel closed'); this.changed(); fail(new Error(message.reason));
        }
      });
      worker.on('error', (error) => {
        if (this.worker !== worker) return;
        this.log(`image worker failed: ${error.message.slice(0, 400)}`);
        this.latest = null; this.clearCapture(); clearTimeout(timeout); this.changed(); fail(new Error('image worker failed'));
      });
      worker.on('exit', () => { if (this.worker !== worker) return; this.latest = null; this.clearCapture(); clearTimeout(timeout); this.changed(); fail(new Error('image worker exited')); });
    });
  }
  get packet(): { meta: FrameMeta; bytes: Uint8Array } | null { return this.latest; }
  packetFor(imageId: string | null | undefined): { meta: FrameMeta; bytes: Uint8Array } | null {
    if (imageId && this.capture && frameImageId(this.capture.meta) === imageId) return this.capture;
    return this.latest;
  }
  private clearCapture(): void {
    if (this.captureTimer) clearTimeout(this.captureTimer);
    this.captureTimer = null; this.capture = null;
  }
  async close(): Promise<void> {
    const worker = this.worker; this.worker = null; this.latest = null; this.clearCapture();
    if (worker) await worker.terminate();
  }
}

/** Exact cross-channel match; idle can attest these bytes but cannot change
 * their original time/checksum/geometry. The UI projection remains authoritative. */
export function imageAvailability(packet: { meta: FrameMeta } | null, control: FrameMeta | null,
  projection: UiProjection | null, origin: number, projectionAt: number, now = Date.now()): ImageAvailability {
  const sameRect = (a: { x: number; y: number; width: number; height: number }, b: typeof a) =>
    a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height;
  const unavailable: ImageAvailability = { frame_id: null, expires_unix_ms: null };
  const view = projection?.observation?.frame;
  if (!packet || !control || !view?.healthy || !['complete', 'idle'].includes(control.status)) return unavailable;
  const a = packet.meta; const b = control; const x = frameGeometry(a); const y = frameGeometry(b);
  if (!x || !y || !frameImageId(a) || view.stream_id !== frameWatchId(a) || view.frame_id !== frameImageId(a)
    || view.geometry_revision !== a.geometry_revision || view.width_px !== a.width_px || view.height_px !== a.height_px
    || a.session_epoch !== b.session_epoch || a.surface.id !== b.surface.id || a.surface.epoch !== b.surface.epoch
    || a.clock_domain !== b.clock_domain || a.source_ms !== b.source_ms || a.checksum !== b.checksum
    || a.geometry_revision !== b.geometry_revision || a.width_px !== b.width_px || a.height_px !== b.height_px || a.scale_factor !== b.scale_factor
    || frameWatchId(a) !== frameWatchId(b) || frameImageId(a) !== frameImageId(b) || x.pid !== y.pid || x.window_id !== y.window_id
    || x.content_scale !== y.content_scale || !sameRect(x.window_frame, y.window_frame)
    || !sameRect(x.content_rect, y.content_rect)
    || now < origin + b.source_ms || now < projectionAt) return unavailable;
  const capture = frameCapture(a); const controlCapture = frameCapture(b);
  let expires: number;
  if (capture || controlCapture) {
    if (!capture || !controlCapture || capture.request_id !== controlCapture.request_id
      || capture.capture_started_uptime_ns !== controlCapture.capture_started_uptime_ns
      || capture.capture_finished_uptime_ns !== controlCapture.capture_finished_uptime_ns
      || capture.capture_started_ms !== controlCapture.capture_started_ms || capture.capture_finished_ms !== controlCapture.capture_finished_ms
      || capture.clock_uncertainty_ms !== controlCapture.clock_uncertainty_ms
      || capture.requested_after_ms !== controlCapture.requested_after_ms || capture.deadline_ms !== controlCapture.deadline_ms
      || now < origin + capture.capture_finished_ms) return unavailable;
    expires = Math.min(origin + capture.capture_started_ms + 2000, origin + capture.deadline_ms, projectionAt + 2000);
  } else {
    const stream = frameStream(a); const controlStream = frameStream(b);
    if (!stream || !controlStream || stream.display_time_ticks !== controlStream.display_time_ticks
      || stream.display_time_ns !== controlStream.display_time_ns || controlStream.callback_ms < stream.callback_ms) return unavailable;
    expires = Math.min(origin + controlStream.callback_ms + 2000, projectionAt + 2000);
  }
  return now < expires ? { frame_id: frameImageId(a), expires_unix_ms: expires } : unavailable;
}

export function observationImage(packet: { meta: FrameMeta; bytes: Uint8Array } | null,
  available: ImageAvailability, requested: string): ObservationImage | null {
  if (!packet || available.frame_id !== requested || available.expires_unix_ms === null) return null;
  return { frame_id: requested, stream_id: frameWatchId(packet.meta)!, width_px: packet.meta.width_px,
    height_px: packet.meta.height_px, expires_unix_ms: available.expires_unix_ms, bytes: packet.bytes.slice() };
}
