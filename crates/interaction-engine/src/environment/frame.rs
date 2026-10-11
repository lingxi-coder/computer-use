//! Native image content, callback liveness and geometry are separate facts.
//! IDLE attests the existing image; it never changes its original source time.

use interaction_contracts::desktop::device::DeviceIdentity;
use interaction_contracts::desktop::operator::FrameView;
use interaction_contracts::desktop::source::{FrameMeta, FrameStatus, Rect};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameChange {
    pub context_changed: bool,
    pub invalidated: bool,
    pub recovered: bool,
}

#[derive(Debug)]
pub struct FrameTracker {
    identity: Option<DeviceIdentity>,
    stream: Option<String>,
    retired_streams: BTreeSet<String>,
    sequence: Option<u64>,
    callback_ms: Option<u64>,
    geometry: Option<u64>,
    dropped: u64,
    content: Option<FrameMeta>,
    stream_content: Option<FrameMeta>,
    latest: Option<FrameMeta>,
    valid: bool,
    heartbeat_timeout_ms: u64,
}

impl FrameTracker {
    pub fn new(heartbeat_timeout_ms: u64) -> Self {
        Self {
            identity: None,
            stream: None,
            retired_streams: BTreeSet::new(),
            sequence: None,
            callback_ms: None,
            geometry: None,
            dropped: 0,
            content: None,
            stream_content: None,
            latest: None,
            valid: false,
            heartbeat_timeout_ms: heartbeat_timeout_ms.max(1),
        }
    }

    pub fn set_identity(&mut self, identity: &DeviceIdentity) {
        let same_session = self.identity.as_ref().is_some_and(|old| {
            old.session_epoch == identity.session_epoch
                && old.surface == identity.surface
                && old.clock_domain == identity.clock_domain
        });
        if !same_session {
            let timeout = self.heartbeat_timeout_ms;
            *self = Self::new(timeout);
            let mut normalized = identity.clone();
            normalized.dependencies.clear();
            self.identity = Some(normalized);
        }
    }

    pub fn reset(&mut self) {
        let identity = self.identity.clone();
        let timeout = self.heartbeat_timeout_ms;
        *self = Self::new(timeout);
        self.identity = identity;
    }

    pub fn invalidate(&mut self) -> bool {
        let changed = self.valid;
        self.valid = false;
        changed
    }

    pub fn stream_id(&self) -> Option<&str> {
        self.stream.as_deref()
    }

    pub fn admit(&mut self, meta: FrameMeta, now_ms: u64) -> Result<FrameChange, String> {
        let identity = self.identity.as_ref().ok_or("device identity is unknown")?;
        if meta.session_epoch != identity.session_epoch
            || meta.surface != identity.surface
            || meta.clock_domain != identity.clock_domain
        {
            return Err("frame is from another device session, surface or clock".into());
        }
        if meta.capture_info().is_some() {
            return self.admit_capture(meta, now_ms);
        }
        let provenance = meta
            .stream_info()
            .ok_or("native stream provenance is missing")?;
        if provenance.stream_id.trim().is_empty()
            || provenance.stream_id.len() > 256
            || provenance.stream_id.chars().any(char::is_control)
            || provenance.source_clock != "mach_absolute"
            || provenance.capture_interval_ms == 0
            || meta.source_sequence == 0
            || meta.frame_id.trim().is_empty()
            || meta.frame_id.len() > 256
            || meta.frame_id.chars().any(char::is_control)
        {
            return Err("invalid native stream identity or clock".into());
        }
        if provenance.callback_ms > now_ms
            || meta.source_ms > provenance.callback_ms
            || provenance.display_time_ns > provenance.received_uptime_ns
        {
            return Err("frame time is in the future or internally inconsistent".into());
        }
        if provenance.pid == 0
            || provenance.window_id.trim().is_empty()
            || provenance.window_id.len() > 256
            || provenance.window_id.chars().any(char::is_control)
            || provenance.image_frame_id.as_ref().is_some_and(|id| {
                id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control)
            })
            || provenance.dirty_rects.len() > 256
            || !valid_rect(&provenance.content_rect)
            || !valid_rect(&provenance.window_frame)
            || provenance.dirty_rects.iter().any(|rect| !valid_rect(rect))
            || !provenance.content_scale.is_finite()
            || provenance.content_scale <= 0.0
        {
            return Err("invalid stream geometry".into());
        }
        let changed_stream = self
            .stream
            .as_ref()
            .is_some_and(|id| id != &provenance.stream_id);
        if self.stream.is_none()
            && !matches!(meta.status, FrameStatus::Started | FrameStatus::Complete)
        {
            return Err("a stream must first provide STARTED or COMPLETE".into());
        }
        if self.retired_streams.contains(&provenance.stream_id) {
            return Err("a retired stream cannot regain authority".into());
        }
        if changed_stream
            && self
                .callback_ms
                .is_some_and(|last| provenance.callback_ms < last)
        {
            return Err("a new stream cannot precede the current callback authority".into());
        }
        if changed_stream && self.retired_streams.len() >= 128 {
            self.invalidate();
            return Err("stream restart budget exhausted; a new device session is required".into());
        }
        if !changed_stream {
            if self
                .sequence
                .is_some_and(|last| meta.source_sequence <= last)
                || self
                    .callback_ms
                    .is_some_and(|last| provenance.callback_ms < last)
                || provenance.dropped_frames < self.dropped
            {
                return Err("frame callback is out of order or replayed".into());
            }
        } else if meta.status != FrameStatus::Started && meta.status != FrameStatus::Complete {
            return Err("a new stream must start with STARTED or COMPLETE".into());
        }
        if meta.status == FrameStatus::Complete {
            if meta.width_px == 0
                || meta.height_px == 0
                || meta.width_px > 8192
                || meta.height_px > 8192
                || !meta.scale_factor.is_finite()
                || meta.scale_factor <= 0.0
                || meta.byte_length == 0
                || meta.byte_length > 12 * 1024 * 1024
                || meta.checksum.trim().is_empty()
                || provenance.display_time_ns == 0
                || provenance.display_time_ticks == 0
                || provenance.content_rect.width == 0.0
                || provenance.content_rect.height == 0.0
                || provenance.window_frame.width == 0.0
                || provenance.window_frame.height == 0.0
                || provenance
                    .image_frame_id
                    .as_deref()
                    .is_none_or(str::is_empty)
            {
                return Err("COMPLETE requires bounded pixels and their identity/checksum".into());
            }
            if !changed_stream
                && self.stream_content.as_ref().is_some_and(|old| {
                    old.stream_info().is_some_and(|previous| {
                        previous.image_frame_id == provenance.image_frame_id
                            || provenance.display_time_ns <= previous.display_time_ns
                            || meta.source_ms < old.source_ms
                    })
                })
            {
                return Err(
                    "COMPLETE cannot replay an existing image identity or capture time".into(),
                );
            }
        }
        let was_valid = self.healthy(now_ms);
        let old_geometry = self.geometry;
        let changed_geometry = old_geometry.is_some_and(|old| old != meta.geometry_revision)
            || !changed_stream
                && self.stream_content.as_ref().is_some_and(|old| {
                    old.stream_info().is_some_and(|previous| {
                        previous.pid != provenance.pid
                            || previous.window_id != provenance.window_id
                            || previous.window_frame != provenance.window_frame
                            || previous.content_rect != provenance.content_rect
                            || previous.content_scale != provenance.content_scale
                            || old.scale_factor != meta.scale_factor
                            || old.width_px != meta.width_px
                            || old.height_px != meta.height_px
                    })
                });
        let dropped_gap = !changed_stream && provenance.dropped_frames > self.dropped;
        let mut change = FrameChange {
            context_changed: changed_stream || changed_geometry || dropped_gap,
            invalidated: changed_stream
                || changed_geometry
                || dropped_gap
                || self.valid && !was_valid,
            recovered: false,
        };
        if changed_stream {
            if let Some(previous) = self.stream.as_ref() {
                self.retired_streams.insert(previous.clone());
            }
            self.content = None;
            self.stream_content = None;
            self.valid = false;
            self.sequence = None;
            self.callback_ms = None;
            self.dropped = 0;
        }
        let callback_fresh =
            now_ms.saturating_sub(provenance.callback_ms) < self.heartbeat_timeout_ms;
        match meta.status {
            FrameStatus::Complete => {
                self.stream_content = Some(meta.clone());
                if self
                    .content
                    .as_ref()
                    .is_none_or(|old| meta.source_ms >= old.source_ms)
                {
                    self.content = Some(meta.clone());
                }
                self.valid = callback_fresh;
                change.recovered = !was_valid && self.valid;
                change.invalidated |= !callback_fresh;
            }
            FrameStatus::Idle => {
                let matches_content = self.stream_content.as_ref().is_some_and(|old| {
                    old.geometry_revision == meta.geometry_revision
                        && old.width_px == meta.width_px
                        && old.height_px == meta.height_px
                        && old.scale_factor == meta.scale_factor
                        && old.source_ms == meta.source_ms
                        && old.checksum == meta.checksum
                        && old.stream_info().is_some_and(|previous| {
                            previous.image_frame_id == provenance.image_frame_id
                                && previous.display_time_ns == provenance.display_time_ns
                                && previous.content_rect == provenance.content_rect
                                && previous.content_scale == provenance.content_scale
                        })
                });
                if meta.byte_length != 0
                    || changed_geometry
                    || dropped_gap
                    || !matches_content
                    || !was_valid
                    || !callback_fresh
                {
                    change.invalidated |= self.invalidate();
                    self.valid = false;
                }
            }
            FrameStatus::Started | FrameStatus::Blank | FrameStatus::Suspended => {
                change.invalidated |= self.invalidate();
                self.valid = false;
            }
            FrameStatus::Stopped => {
                change.invalidated |= self.invalidate();
                self.retired_streams.insert(provenance.stream_id.clone());
            }
        }
        self.stream = Some(provenance.stream_id.clone());
        self.sequence = Some(meta.source_sequence);
        self.callback_ms = Some(provenance.callback_ms);
        self.geometry = Some(meta.geometry_revision);
        self.dropped = provenance.dropped_frames;
        self.latest = Some(meta);
        Ok(change)
    }

    fn admit_capture(&mut self, meta: FrameMeta, now_ms: u64) -> Result<FrameChange, String> {
        let capture = meta.capture_info().ok_or("capture provenance is missing")?;
        capture.validate()?;
        let original = self
            .stream_content
            .as_ref()
            .ok_or("capture requires an established stream")?;
        if !self.healthy(now_ms)
            || self.stream.as_deref() != Some(capture.watch_id.as_str())
            || self.retired_streams.contains(&capture.watch_id)
            || meta.status != FrameStatus::Complete
            || meta.source_ms != capture.capture_started_ms
            || capture.capture_finished_ms > now_ms
            || self
                .sequence
                .is_some_and(|sequence| meta.source_sequence <= sequence)
            || meta.geometry_revision != original.geometry_revision
            || meta.window_geometry() != original.window_geometry()
            || meta.width_px != original.width_px
            || meta.height_px != original.height_px
            || meta.scale_factor != original.scale_factor
            || meta.byte_length == 0
            || meta.byte_length > 12 * 1024 * 1024
            || meta.checksum.trim().is_empty()
            || self.content.as_ref().is_some_and(|current| {
                meta.source_ms <= current.source_ms
                    || meta.image_frame_id() == current.image_frame_id()
            })
        {
            return Err("capture is stale, replayed or outside its current healthy watch".into());
        }
        self.sequence = Some(meta.source_sequence);
        self.content = Some(meta.clone());
        self.latest = Some(meta);
        // A one-shot result cannot advance or restore stream callback liveness.
        Ok(FrameChange::default())
    }

    pub fn expire(&mut self, now_ms: u64) -> bool {
        if self.valid
            && self
                .heartbeat_deadline()
                .is_none_or(|deadline| now_ms >= deadline)
        {
            return self.invalidate();
        }
        false
    }

    pub fn healthy(&self, now_ms: u64) -> bool {
        self.valid
            && self
                .heartbeat_deadline()
                .is_some_and(|deadline| now_ms < deadline)
    }

    pub fn content_deadline(&self, max_age_ms: u64) -> Option<u64> {
        self.valid
            .then(|| {
                self.content
                    .as_ref()
                    .map(|meta| meta.source_ms.saturating_add(max_age_ms))
            })
            .flatten()
    }

    pub fn heartbeat_deadline(&self) -> Option<u64> {
        self.callback_ms
            .map(|time| time.saturating_add(self.heartbeat_timeout_ms))
    }

    pub fn latest_content(&self) -> Option<&FrameMeta> {
        self.content.as_ref()
    }

    pub fn view(&self, now_ms: u64) -> Option<FrameView> {
        let latest = self.latest.as_ref()?;
        Some(FrameView {
            stream_id: self.stream.clone()?,
            frame_id: self
                .content
                .as_ref()
                .and_then(|meta| meta.image_frame_id().map(str::to_string)),
            status: latest.status,
            geometry_revision: latest.geometry_revision,
            width_px: latest.width_px,
            height_px: latest.height_px,
            pixel_age_ms: self
                .content
                .as_ref()
                .map(|meta| now_ms.saturating_sub(meta.source_ms)),
            confirmation_age_ms: self.callback_ms.map(|time| now_ms.saturating_sub(time)),
            dropped_frames: self.dropped,
            healthy: self.healthy(now_ms),
        })
    }
}

fn valid_rect(rect: &Rect) -> bool {
    [rect.x, rect.y, rect.width, rect.height]
        .iter()
        .all(|value| value.is_finite())
        && rect.width >= 0.0
        && rect.height >= 0.0
        && (rect.x + rect.width).is_finite()
        && (rect.y + rect.height).is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::device::SurfaceRef;
    use interaction_contracts::desktop::source::FrameStreamInfo;

    fn complete(sequence: u64, source_ms: u64) -> FrameMeta {
        FrameMeta {
            frame_id: format!("record-{sequence}"),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "macos/1/wall".into(),
            source_sequence: sequence,
            source_ms,
            status: FrameStatus::Complete,
            geometry_revision: 1,
            width_px: 100,
            height_px: 100,
            scale_factor: 1.0,
            byte_length: 100,
            checksum: format!("sha256:image-{sequence}"),
            provenance: Some(
                interaction_contracts::desktop::source::FrameProvenance::Stream {
                    stream: FrameStreamInfo {
                        stream_id: "stream-a".into(),
                        image_frame_id: Some(format!("image-{sequence}")),
                        display_time_ticks: source_ms * 1_000_000,
                        display_time_ns: source_ms * 1_000_000,
                        received_uptime_ns: source_ms * 1_000_000,
                        received_unix_ms: 100_000 + source_ms,
                        callback_ms: source_ms,
                        source_clock: "mach_absolute".into(),
                        window_id: "window-1".into(),
                        pid: 42,
                        window_frame: Rect {
                            x: -100.0,
                            y: 0.0,
                            width: 100.0,
                            height: 100.0,
                        },
                        content_rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 100.0,
                            height: 100.0,
                        },
                        content_scale: 1.0,
                        dirty_rects: vec![],
                        dropped_frames: 0,
                        capture_interval_ms: 100,
                    },
                },
            ),
        }
    }

    fn idle(image: &FrameMeta, sequence: u64, callback_ms: u64) -> FrameMeta {
        let mut meta = image.clone();
        meta.frame_id = format!("record-{sequence}");
        meta.source_sequence = sequence;
        meta.status = FrameStatus::Idle;
        meta.byte_length = 0;
        let stream = meta.stream_info_mut().unwrap();
        stream.callback_ms = callback_ms;
        stream.received_unix_ms = 100_000 + callback_ms;
        stream.received_uptime_ns = callback_ms * 1_000_000;
        meta
    }

    fn tracker(timeout_ms: u64) -> FrameTracker {
        let mut tracker = FrameTracker::new(timeout_ms);
        tracker.set_identity(&DeviceIdentity {
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "macos/1/wall".into(),
            dependencies: Default::default(),
        });
        tracker
    }

    fn capture(sequence: u64, start: u64) -> FrameMeta {
        use interaction_contracts::desktop::{
            ids::RequestId,
            source::{FrameProvenance, SingleCaptureFrameInfo},
        };
        let mut frame = complete(sequence, start);
        let stream = frame.stream_info().unwrap().clone();
        frame.provenance = Some(FrameProvenance::SingleCapture {
            capture: SingleCaptureFrameInfo {
                watch_id: stream.stream_id,
                request_id: RequestId::new("capture-1").unwrap(),
                image_frame_id: format!("capture-{sequence}"),
                pid: stream.pid,
                window_id: stream.window_id,
                window_frame: stream.window_frame,
                content_rect: stream.content_rect,
                content_scale: stream.content_scale,
                source_clock: "mach_absolute".into(),
                capture_started_uptime_ns: start * 1_000_000,
                capture_finished_uptime_ns: (start + 10) * 1_000_000,
                capture_started_ms: start,
                capture_finished_ms: start + 10,
                clock_uncertainty_ms: 2,
                requested_after_ms: start - 1,
                deadline_ms: start + 200,
            },
        });
        frame
    }

    #[test]
    fn capture_replaces_content_without_renewing_or_restoring_stream_heartbeat() {
        let original = complete(1, 100);
        let mut tracker = tracker(1000);
        tracker.admit(original.clone(), 100).unwrap();
        tracker.admit(capture(2, 200), 210).unwrap();
        assert_eq!(
            tracker.latest_content().unwrap().image_frame_id(),
            Some("capture-2")
        );
        assert_eq!(tracker.heartbeat_deadline(), Some(1100));
        tracker.admit(idle(&original, 3, 300), 300).unwrap();
        assert_eq!(tracker.latest_content().unwrap().source_ms, 200);
        assert_eq!(tracker.heartbeat_deadline(), Some(1300));
        assert!(tracker.admit(capture(4, 1300), 1310).is_err());
        tracker.expire(1300);
        assert!(!tracker.healthy(1300));
        assert!(tracker.admit(capture(5, 1400), 1410).is_err());
    }

    #[test]
    fn capture_rejects_foreign_watch_geometry_and_replay() {
        let original = complete(1, 100);
        let mut tracker = tracker(1000);
        tracker.admit(original, 100).unwrap();
        let mut foreign = capture(2, 200);
        let Some(interaction_contracts::desktop::source::FrameProvenance::SingleCapture {
            capture: info,
        }) = foreign.provenance.as_mut()
        else {
            unreachable!()
        };
        info.watch_id = "other-watch".into();
        assert!(tracker.admit(foreign, 210).is_err());
        let mut changed = capture(2, 200);
        changed.geometry_revision += 1;
        assert!(tracker.admit(changed, 210).is_err());
        let accepted = capture(2, 200);
        tracker.admit(accepted.clone(), 210).unwrap();
        assert!(tracker.admit(accepted, 211).is_err());
    }

    #[test]
    fn idle_confirms_liveness_without_refreshing_capture_time_or_content_deadline() {
        let image = complete(1, 100);
        let mut tracker = tracker(1000);
        assert!(tracker.admit(image.clone(), 100).unwrap().recovered);
        assert_eq!(
            tracker.admit(idle(&image, 2, 900), 900).unwrap(),
            FrameChange::default()
        );
        assert!(tracker.healthy(900));
        assert_eq!(tracker.latest_content().unwrap().source_ms, 100);
        assert_eq!(tracker.content_deadline(500), Some(600));
        let view = tracker.view(900).unwrap();
        assert_eq!(view.pixel_age_ms, Some(800));
        assert_eq!(view.confirmation_age_ms, Some(0));
    }

    #[test]
    fn dependency_updates_keep_current_stream_but_idle_cannot_establish_a_new_one() {
        let image = complete(1, 100);
        let mut tracker = tracker(1000);
        assert!(tracker.admit(idle(&image, 2, 200), 200).is_err());
        tracker.admit(image.clone(), 100).unwrap();
        let mut identity = tracker.identity.clone().unwrap();
        identity.dependencies.insert("focus".into(), 99);
        tracker.set_identity(&identity);
        assert_eq!(tracker.latest_content(), Some(&image));
        assert!(tracker.healthy(200));
    }

    #[test]
    fn missing_callbacks_expire_once_and_idle_cannot_revive_the_old_image() {
        let image = complete(1, 100);
        let mut tracker = tracker(1000);
        tracker.admit(image.clone(), 100).unwrap();
        assert!(tracker.expire(1100));
        assert!(!tracker.expire(1101));
        tracker.admit(idle(&image, 2, 1200), 1200).unwrap();
        assert!(!tracker.healthy(1200));
        assert!(tracker.admit(complete(3, 1300), 1300).unwrap().recovered);
    }

    #[test]
    fn delayed_callback_does_not_gain_a_new_lease_when_it_arrives() {
        let mut tracker = tracker(1000);
        tracker.admit(complete(1, 100), 1200).unwrap();
        assert!(!tracker.healthy(1200));
        assert_eq!(tracker.heartbeat_deadline(), Some(1100));
    }

    #[test]
    fn geometry_or_missing_content_on_idle_invalidates_the_confirmation() {
        for change in ["revision", "bounds", "window", "pid", "image", "drop"] {
            let image = complete(1, 100);
            let mut tracker = tracker(1000);
            tracker.admit(image.clone(), 100).unwrap();
            let mut meta = idle(&image, 2, 200);
            let stream = meta.stream_info_mut().unwrap();
            match change {
                "revision" => meta.geometry_revision = 2,
                "bounds" => stream.window_frame.x += 5.0,
                "window" => stream.window_id = "another-window".into(),
                "pid" => stream.pid = 43,
                "image" => stream.image_frame_id = Some("another-image".into()),
                "drop" => stream.dropped_frames = 1,
                _ => unreachable!(),
            }
            assert!(tracker.admit(meta, 200).unwrap().invalidated, "{change}");
            assert!(!tracker.healthy(200), "{change}");
        }
    }

    #[test]
    fn out_of_order_replay_bad_geometry_and_wrong_session_never_replace_good_content() {
        let image = complete(1, 100);
        for change in [
            "sequence", "source", "future", "nan", "session", "stream", "payload",
        ] {
            let mut tracker = tracker(1000);
            tracker.admit(image.clone(), 100).unwrap();
            let mut bad = complete(2, 200);
            match change {
                "sequence" => bad.source_sequence = 1,
                "source" => bad.stream_info_mut().unwrap().display_time_ns = 100_000_000,
                "future" => bad.source_ms = 300,
                "nan" => bad.stream_info_mut().unwrap().content_rect.width = f64::NAN,
                "session" => bad.session_epoch = 2,
                "stream" => {
                    bad.stream_info_mut().unwrap().stream_id = "b".into();
                    bad.byte_length = 0;
                }
                "payload" => bad.byte_length = 0,
                _ => unreachable!(),
            }
            assert!(tracker.admit(bad, 200).is_err(), "{change}");
            assert_eq!(tracker.latest_content(), Some(&image), "{change}");
            assert!(tracker.healthy(200), "{change}");
        }
    }

    #[test]
    fn a_fresh_complete_on_a_new_stream_closes_the_old_context() {
        let mut tracker = tracker(1000);
        tracker.admit(complete(1, 100), 100).unwrap();
        let mut image = complete(1, 200);
        image.stream_info_mut().unwrap().stream_id = "b".into();
        let change = tracker.admit(image, 200).unwrap();
        assert!(change.context_changed && change.invalidated);
        assert!(tracker.healthy(200));
        let mut retired = complete(10, 300);
        retired.stream_info_mut().unwrap().stream_id = "stream-a".into();
        assert!(tracker.admit(retired, 300).is_err());
        assert_eq!(tracker.stream_id(), Some("b"));
    }

    #[test]
    fn an_unknown_older_stream_cannot_replace_current_callback_authority() {
        let image = complete(1, 300);
        let mut tracker = tracker(1000);
        tracker.admit(image.clone(), 300).unwrap();
        let mut older = complete(1, 250);
        older.stream_info_mut().unwrap().stream_id = "older-stream".into();
        assert!(tracker.admit(older, 350).is_err());
        assert_eq!(tracker.latest_content(), Some(&image));
        assert!(tracker.healthy(350));
    }

    #[test]
    fn stopped_retires_its_stream_and_new_device_discards_every_old_record() {
        let image = complete(1, 100);
        let mut tracker = tracker(1000);
        tracker.admit(image.clone(), 100).unwrap();
        let mut stopped = idle(&image, 2, 200);
        stopped.status = FrameStatus::Stopped;
        assert!(tracker.admit(stopped, 200).unwrap().invalidated);
        assert!(tracker.admit(idle(&image, 3, 300), 300).is_err());
        assert!(tracker.admit(complete(3, 300), 300).is_err());
        assert!(!tracker.healthy(300));
        let mut replacement = complete(1, 400);
        replacement.stream_info_mut().unwrap().stream_id = "replacement".into();
        assert!(tracker.admit(replacement, 400).unwrap().recovered);
        tracker.set_identity(&DeviceIdentity {
            session_epoch: 2,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 2,
            },
            clock_domain: "macos/2/wall".into(),
            dependencies: Default::default(),
        });
        assert!(tracker.view(300).is_none());
        assert!(tracker.admit(image, 300).is_err());
    }
}
