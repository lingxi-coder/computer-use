import type { FrameMeta } from './generated/envelope.js';

export function frameStream(frame: FrameMeta) {
  return frame.provenance?.kind === 'stream' ? frame.provenance.stream : null;
}

export function frameCapture(frame: FrameMeta) {
  return frame.provenance?.kind === 'single_capture' ? frame.provenance.capture : null;
}

export function frameGeometry(frame: FrameMeta) {
  return frameStream(frame) ?? frameCapture(frame);
}

export function frameWatchId(frame: FrameMeta): string | null {
  return frameStream(frame)?.stream_id ?? frameCapture(frame)?.watch_id ?? null;
}

export function frameImageId(frame: FrameMeta): string | null {
  return frameGeometry(frame)?.image_frame_id ?? null;
}

/** Validate producer-declared operation bounds; these never attest SCStream liveness. */
export function validSingleCapture(frame: FrameMeta): boolean {
  const capture = frameCapture(frame);
  if (!capture) return false;
  const bounded = (value: string) => !!value.trim() && new TextEncoder().encode(value).length <= 256
    && !/\p{Cc}/u.test(value);
  const safe = (value: number) => Number.isSafeInteger(value) && value >= 0;
  const rect = (value: typeof capture.window_frame) =>
    [value.x, value.y, value.width, value.height].every((n) => Number.isFinite(n) && Math.abs(n) <= 100_000)
    && value.width > 0 && value.height > 0;
  if (![capture.watch_id, capture.request_id, capture.image_frame_id, capture.window_id].every(bounded)
    || capture.source_clock !== 'mach_absolute' || capture.pid <= 0 || capture.pid > 2_147_483_647
    || !rect(capture.window_frame) || !rect(capture.content_rect)
    || !Number.isFinite(capture.content_scale) || capture.content_scale <= 0
    || ![capture.capture_started_uptime_ns, capture.capture_finished_uptime_ns, capture.capture_started_ms,
      capture.capture_finished_ms, capture.clock_uncertainty_ms, capture.requested_after_ms, capture.deadline_ms].every(safe)
    || capture.capture_started_uptime_ns === 0 || capture.clock_uncertainty_ms > 10
    || capture.capture_started_uptime_ns > capture.capture_finished_uptime_ns
    || capture.capture_started_ms <= capture.requested_after_ms
    || capture.capture_started_ms > capture.capture_finished_ms || capture.capture_finished_ms > capture.deadline_ms
    || capture.deadline_ms <= capture.requested_after_ms || frame.source_ms !== capture.capture_started_ms
    || frame.status !== 'complete') return false;
  const nativeSpan = capture.capture_finished_uptime_ns - capture.capture_started_uptime_ns;
  const mappedSpan = capture.capture_finished_ms - capture.capture_started_ms;
  return mappedSpan >= Math.floor(nativeSpan / 1_000_000)
    && mappedSpan <= Math.ceil(nativeSpan / 1_000_000) + 2 * capture.clock_uncertainty_ms + 2;
}
