//! One same-watch result, independently correlated from SCStream callbacks.
use super::*;
use crate::native::watch::CaptureFailure;
use interaction_contracts::desktop::source::{
    FrameProvenance, SingleCaptureFrameInfo, SourceCommand,
};

#[derive(Deserialize)]
struct CaptureResult {
    watch_id: String,
    request_id: RequestId,
    session_epoch: u64,
    surface: SurfaceRef,
    status: String,
    requested_after_ms: u64,
    deadline_ms: u64,
    capture_started_uptime_ns: Option<u64>,
    capture_finished_uptime_ns: Option<u64>,
    capture_started_unix_ms: Option<u64>,
    capture_finished_unix_ms: Option<u64>,
    image_frame_id: Option<String>,
    frame: Option<CaptureFrame>,
    observation: Option<Value>,
    code: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFrame {
    geometry_revision: u64,
    width_px: u32,
    height_px: u32,
    scale_factor: f64,
    content_scale: f64,
    content_rect: Rect,
    window_id: String,
    pid: u32,
    frame: Rect,
    byte_length: u64,
    png_file: String,
    frame_id: String,
}

impl WatchTracker {
    pub(in crate::native::watch) fn bind_watch(&mut self, id: String) {
        self.expected_watch_id = Some(id);
    }
    pub(in crate::native::watch) fn bound_watch_id(&self) -> Option<&str> {
        self.expected_watch_id.as_deref()
    }

    pub(in crate::native::watch) fn control_record(
        &mut self,
        command: &SourceCommand,
        now: u64,
    ) -> Result<Option<Value>, CaptureFailure> {
        match command {
            SourceCommand::CaptureNow {
                request_id,
                watch_id,
                session_epoch,
                surface,
                after_ms,
                deadline_ms,
            } => {
                let failure = |code: &str| CaptureFailure {
                    request_id: request_id.clone(),
                    watch_id: watch_id.clone(),
                    code: code.into(),
                };
                if command.validate().is_err()
                    || self.stream_id.as_deref() != Some(watch_id)
                    || self.expected_watch_id.as_deref() != Some(watch_id)
                    || *session_epoch != self.session_epoch
                    || surface.id != "main"
                    || surface.epoch != self.session_epoch
                    || self.image.is_none()
                    || self.last_screenshot_failure.is_some()
                {
                    return Err(failure("capture_unavailable"));
                }
                if now >= *deadline_ms {
                    return Err(failure("deadline_expired"));
                }
                if self.pending_capture.is_some() {
                    return Err(failure("capture_unavailable"));
                }
                self.pending_capture = Some(command.clone());
                Ok(Some(
                    json!({"protocol_version":1,"command":"capture_now","request_id":request_id,
                    "watch_id":watch_id,"session_epoch":session_epoch,"surface":surface,"after_ms":after_ms,"deadline_ms":deadline_ms}),
                ))
            }
            SourceCommand::CancelCapture {
                request_id,
                watch_id,
            } => {
                if self.pending_capture.as_ref().is_some_and(|pending|
                    matches!(pending, SourceCommand::CaptureNow { request_id: id, watch_id: watch, .. } if id == request_id && watch == watch_id)) {
                    self.pending_capture = None;
                }
                Ok(Some(
                    json!({"protocol_version":1,"command":"cancel_capture","request_id":request_id,
                    "watch_id":watch_id,"session_epoch":self.session_epoch,"surface":{"id":"main","epoch":self.session_epoch}}),
                ))
            }
            SourceCommand::RefreshNow { .. } => Ok(None),
        }
    }

    pub(super) fn capture_result(
        &mut self,
        header: &Header,
        value: Value,
        assets: &Path,
        now: u64,
    ) -> Result<WatchReport, String> {
        let allowed = [
            "protocol_version",
            "kind",
            "stream_id",
            "sequence",
            "received_uptime_ns",
            "received_unix_ms",
            "watch_id",
            "request_id",
            "session_epoch",
            "surface",
            "status",
            "requested_after_ms",
            "deadline_ms",
            "capture_started_uptime_ns",
            "capture_finished_uptime_ns",
            "capture_started_unix_ms",
            "capture_finished_unix_ms",
            "image_frame_id",
            "frame",
            "observation",
            "code",
        ];
        if value
            .as_object()
            .is_none_or(|object| object.keys().any(|key| !allowed.contains(&key.as_str())))
        {
            return Err("native capture contains unknown fields".into());
        }
        let raw: CaptureResult = serde_json::from_value(value)
            .map_err(|error| format!("invalid native capture result: {error}"))?;
        let mut report = WatchReport::new(header.stream_id.clone(), header.sequence);
        let Some(SourceCommand::CaptureNow {
            request_id,
            watch_id,
            session_epoch,
            surface,
            after_ms,
            deadline_ms,
        }) = self.pending_capture.as_ref()
        else {
            return Ok(report);
        };
        if &raw.request_id != request_id || &raw.watch_id != watch_id {
            return Ok(report);
        }
        if raw.session_epoch != *session_epoch
            || raw.surface != *surface
            || raw.requested_after_ms != *after_ms
            || raw.deadline_ms != *deadline_ms
            || raw.watch_id != header.stream_id
        {
            return Err("native capture changed its authorized scope or deadline".into());
        }
        self.pending_capture = None;
        if now.saturating_sub(self.origin_unix_ms) >= raw.deadline_ms {
            return Ok(report);
        }
        if raw.status == "failed" {
            let code = raw
                .code
                .filter(|code| {
                    matches!(
                        code.as_str(),
                        "deadline_expired"
                            | "cancelled"
                            | "scope_mismatch"
                            | "permission_denied"
                            | "capture_unavailable"
                            | "pair_incomplete"
                            | "internal_error"
                    )
                })
                .ok_or("native capture failure has no stable result code")?;
            if raw.frame.is_some()
                || raw.observation.is_some()
                || raw.image_frame_id.is_some()
                || raw.capture_started_uptime_ns.is_some()
                || raw.capture_finished_uptime_ns.is_some()
                || raw.capture_started_unix_ms.is_some()
                || raw.capture_finished_unix_ms.is_some()
            {
                return Err("failed native capture supplied evidence".into());
            }
            report.capture_failure = Some(CaptureFailure {
                request_id: raw.request_id,
                watch_id: raw.watch_id,
                code,
            });
            return Ok(report);
        }
        if raw.status != "complete" || raw.code.is_some() {
            return Err("invalid native capture outcome".into());
        }
        let frame = raw.frame.ok_or("native capture has no frame")?;
        let start_ns = raw
            .capture_started_uptime_ns
            .ok_or("capture start uptime is missing")?;
        let finish_ns = raw
            .capture_finished_uptime_ns
            .ok_or("capture finish uptime is missing")?;
        let start_wall = raw
            .capture_started_unix_ms
            .ok_or("capture start wall time is missing")?;
        let finish_wall = raw
            .capture_finished_unix_ms
            .ok_or("capture finish wall time is missing")?;
        if start_ns == 0
            || finish_ns < start_ns
            || finish_ns > header.received_uptime_ns
            || finish_wall < start_wall
        {
            return Err("capture native interval is inconsistent".into());
        }
        for (ns, wall) in [(start_ns, start_wall), (finish_ns, finish_wall)] {
            let mapped = header
                .received_unix_ms
                .checked_sub((header.received_uptime_ns - ns).div_ceil(1_000_000))
                .ok_or("capture clock conversion underflow")?;
            if mapped.abs_diff(wall) > 2 {
                return Err("capture wall and uptime clocks disagree".into());
            }
        }
        let started = start_wall
            .checked_sub(self.origin_unix_ms)
            .and_then(|value| value.checked_sub(2))
            .ok_or("capture start predates origin")?;
        let finished = finish_wall
            .checked_sub(self.origin_unix_ms)
            .and_then(|value| value.checked_add(2))
            .ok_or("capture finish clock overflow")?;
        let capture = SingleCaptureFrameInfo {
            watch_id: raw.watch_id,
            request_id: raw.request_id,
            image_frame_id: raw.image_frame_id.ok_or("capture image identity missing")?,
            pid: frame.pid,
            window_id: frame.window_id,
            window_frame: frame.frame,
            content_rect: frame.content_rect,
            content_scale: frame.content_scale,
            source_clock: "mach_absolute".into(),
            capture_started_uptime_ns: start_ns,
            capture_finished_uptime_ns: finish_ns,
            capture_started_ms: started,
            capture_finished_ms: finished,
            clock_uncertainty_ms: 2,
            requested_after_ms: raw.requested_after_ms,
            deadline_ms: raw.deadline_ms,
        };
        capture.validate()?;
        let geometry = self
            .geometry
            .as_ref()
            .ok_or("capture has no established geometry")?;
        if geometry.revision != frame.geometry_revision
            || geometry.pid != capture.pid
            || geometry.window != capture.window_id
            || geometry.window_frame != capture.window_frame
            || geometry.content_rect != capture.content_rect
            || geometry.content_scale != capture.content_scale
            || geometry.width != frame.width_px
            || geometry.height != frame.height_px
            || geometry.scale != frame.scale_factor
        {
            return Err("capture differs from the retained watch geometry".into());
        }
        validate_asset(
            assets,
            &json!({"png_file":frame.png_file,"byte_length":frame.byte_length}),
        )?;
        let mut bytes = Vec::new();
        fs::File::open(assets.join(frame.png_file))
            .map_err(|error| error.to_string())?
            .take(crate::native::MAX_CAPTURE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 != frame.byte_length {
            return Err("capture PNG changed during publication".into());
        }
        let meta = FrameMeta {
            frame_id: frame.frame_id,
            session_epoch: raw.session_epoch,
            surface: raw.surface,
            clock_domain: self.clock_domain.clone(),
            source_sequence: header.sequence,
            source_ms: started,
            status: FrameStatus::Complete,
            geometry_revision: frame.geometry_revision,
            width_px: frame.width_px,
            height_px: frame.height_px,
            scale_factor: frame.scale_factor,
            byte_length: frame.byte_length,
            checksum: format!("{:x}", Sha256::digest(&bytes)),
            provenance: Some(FrameProvenance::SingleCapture { capture }),
        };
        meta.validate_provenance()?;
        desktop_ipc::image::validate_meta(&meta).map_err(|error| error.to_string())?;
        desktop_ipc::image::validate_png(&meta, &bytes).map_err(|error| error.to_string())?;
        let observation = raw
            .observation
            .ok_or("capture has no paired AX observation")?;
        let inner: Header =
            serde_json::from_value(observation.clone()).map_err(|error| error.to_string())?;
        let ax_started_ns = observation
            .get("structure_started_uptime_ns")
            .and_then(Value::as_u64)
            .ok_or("capture AX start uptime is missing")?;
        let ax_started_wall = observation
            .get("structure_started_unix_ms")
            .and_then(Value::as_u64)
            .ok_or("capture AX start wall time is missing")?;
        if inner.protocol_version != 1
            || inner.kind != "observation"
            || inner.stream_id != header.stream_id
            || inner.sequence != header.sequence
            || inner.received_uptime_ns > header.received_uptime_ns
            || inner.received_unix_ms > header.received_unix_ms
            || ax_started_ns < finish_ns
            || ax_started_wall < finish_wall
        {
            return Err("capture AX observation has foreign publication scope".into());
        }
        let inner_wall = header
            .received_unix_ms
            .checked_sub((header.received_uptime_ns - inner.received_uptime_ns).div_ceil(1_000_000))
            .ok_or("capture AX clock conversion underflow")?;
        if inner_wall.abs_diff(inner.received_unix_ms) > 2 {
            return Err("capture AX header wall and uptime clocks disagree".into());
        }
        // AX context proofs are stamped at that read's completion. PNG saving
        // finishes later, so validate against the paired observation's own
        // header while keeping its sequence/watch correlated with the result.
        report = self.observation(&inner, observation)?;
        let snapshot = report
            .snapshot
            .as_ref()
            .ok_or("capture AX projection is missing")?;
        if !snapshot.complete
            || snapshot.frame_ref.as_deref() != meta.image_frame_id()
            || snapshot.app.pid != meta.window_geometry().unwrap().pid
        {
            return Err("capture pair is incomplete or foreign".into());
        }
        report.frame_meta = Some(meta);
        report.image = Some(bytes.into());
        Ok(report)
    }
}
