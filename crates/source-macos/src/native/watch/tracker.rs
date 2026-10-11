//! Deterministic provenance checks for the native NDJSON stream. Arrival time
//! is used only to detect a stalled channel; it never becomes evidence time.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::ids::RequestId;
use interaction_contracts::desktop::owned_document::OwnedDocumentReceiverEvidence;
use interaction_contracts::desktop::source::{
    FrameMeta, FrameStatus, FrameStreamInfo, KernelProcessIdentity, LayerKind,
    ObjectIdentityContinuity, ObservationFact, Rect, SourceBackgroundWindowContext, SourceHealth,
    SourceKind, SourceStamp, SourceState, StructureSnapshot, TargetContext, UiObject,
};
mod capture;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::WatchReport;
use crate::config::{NativeConfig, TextReadPolicy};
use crate::native::{
    NativeOcr, NativePermissions, NativeStructure, private_json, short, validate_asset,
};

const CLOCK_JUMP_TOLERANCE_MS: i128 = 100;
const MAX_TRACKED_AX_READ_IDENTITIES: usize = 8;

#[derive(Debug, Deserialize)]
pub(super) struct Header {
    pub protocol_version: u16,
    pub kind: String,
    pub stream_id: String,
    pub sequence: u64,
    pub received_uptime_ns: u64,
    pub received_unix_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFrame {
    pub status: FrameStatus,
    pub display_time_ticks: Option<u64>,
    pub display_time_uptime_ns: Option<u64>,
    pub source_unix_ms: Option<u64>,
    pub geometry_revision: u64,
    pub width_px: u32,
    pub height_px: u32,
    pub scale_factor: f64,
    pub content_scale: f64,
    pub content_rect: Rect,
    pub dirty_rects: Vec<Rect>,
    pub frame_id: Option<String>,
    pub window_id: String,
    pub pid: u32,
    pub frame: Rect,
    pub byte_length: u64,
    pub png_file: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Observation {
    permissions: NativePermissions,
    structure: NativeStructure,
    structure_started_uptime_ns: u64,
    structure_finished_uptime_ns: u64,
    #[serde(default)]
    structure_started_unix_ms: Option<u64>,
    #[serde(default)]
    structure_finished_unix_ms: Option<u64>,
    frame_ref: Option<String>,
    tree_revision: u64,
    observer_revision: u64,
    observer_available: bool,
    #[serde(rename = "observer_reason")]
    _observer_reason: Option<String>,
    ocr: NativeOcr,
    errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeReadIdentity {
    stream_id: String,
    started_uptime_ns: u64,
    finished_uptime_ns: u64,
    started_unix_ms: u64,
    finished_unix_ms: u64,
}

#[derive(Debug)]
struct ReadScopedRead {
    identity: NativeReadIdentity,
    tokens: std::collections::HashSet<String>,
}

/// A watch observation can republish the same already-read AX structure when
/// pairing it with a later frame. Only matching uptime and wall-clock bounds
/// prove that two records describe that same AX read; record sequence alone is
/// not an AX-read counter.
fn native_read_identity(
    header: &Header,
    observation: &Observation,
) -> Result<Option<NativeReadIdentity>, String> {
    let (Some(started_unix_ms), Some(finished_unix_ms)) = (
        observation.structure_started_unix_ms,
        observation.structure_finished_unix_ms,
    ) else {
        if observation.structure_started_unix_ms.is_some()
            || observation.structure_finished_unix_ms.is_some()
        {
            return Err("native AX read has incomplete wall-clock bounds".into());
        }
        return Ok(None);
    };
    if started_unix_ms > finished_unix_ms || finished_unix_ms > header.received_unix_ms {
        return Err("native AX read has invalid wall-clock bounds".into());
    }
    let started_age_ms = header
        .received_uptime_ns
        .checked_sub(observation.structure_started_uptime_ns)
        .ok_or("native AX read start uptime is in the future")?
        .div_ceil(1_000_000);
    let finished_age_ms = header
        .received_uptime_ns
        .checked_sub(observation.structure_finished_uptime_ns)
        .ok_or("native AX read finish uptime is in the future")?
        .div_ceil(1_000_000);
    let uptime_started_unix_ms = header
        .received_unix_ms
        .checked_sub(started_age_ms)
        .ok_or("native AX read start predates its clock mapping")?;
    let uptime_finished_unix_ms = header
        .received_unix_ms
        .checked_sub(finished_age_ms)
        .ok_or("native AX read finish predates its clock mapping")?;
    if (i128::from(started_unix_ms) - i128::from(uptime_started_unix_ms)).abs()
        > CLOCK_JUMP_TOLERANCE_MS
        || (i128::from(finished_unix_ms) - i128::from(uptime_finished_unix_ms)).abs()
            > CLOCK_JUMP_TOLERANCE_MS
    {
        return Err("native AX read wall and uptime stamps disagree".into());
    }
    Ok(Some(NativeReadIdentity {
        stream_id: header.stream_id.clone(),
        started_uptime_ns: observation.structure_started_uptime_ns,
        finished_uptime_ns: observation.structure_finished_uptime_ns,
        started_unix_ms,
        finished_unix_ms,
    }))
}

fn map_owned_document_evidence(
    config: &NativeConfig,
    outer_clock_domain: &str,
    origin_unix_ms: u64,
    header: &Header,
    observation: &Observation,
    expected_snapshot_id: &str,
) -> Result<Option<OwnedDocumentReceiverEvidence>, String> {
    let Some(raw) = observation.structure.owned_document_evidence.as_ref() else {
        return Ok(None);
    };
    let bootstrap = config
        .owned_document_bootstrap
        .as_ref()
        .ok_or("native watch returned unregistered owned-document evidence")?;
    let started_unix_ms = observation
        .structure_started_unix_ms
        .ok_or("owned-document evidence has no raw structure start time")?;
    let finished_unix_ms = observation
        .structure_finished_unix_ms
        .ok_or("owned-document evidence has no raw structure finish time")?;
    if started_unix_ms > finished_unix_ms
        || finished_unix_ms > header.received_unix_ms
        || !observation.permissions.accessibility
        || observation.structure.app.bundle_id != bootstrap.bundle_id
        || observation.structure.app.pid != bootstrap.pid
        || config.bundle_id != bootstrap.bundle_id
        || config.pid != Some(bootstrap.pid)
        || raw.registration_id != bootstrap.registration_id
        || raw.process != bootstrap.process_identity()
        || raw.provenance.snapshot_id.as_str() != expected_snapshot_id
        || raw.provenance.source_sequence != header.sequence
        || raw.provenance.clock_domain != "native_unix_ms"
        || raw.provenance.observed_ms > started_unix_ms
        || raw.provenance.valid_until_ms <= finished_unix_ms
        || raw
            .provenance
            .valid_until_ms
            .saturating_sub(raw.provenance.observed_ms)
            > config.timeout_ms.saturating_add(1_000)
        || raw.marked_range_before.observed_ms < started_unix_ms
        || raw.marked_range_after.observed_ms < raw.marked_range_before.observed_ms
        || raw.marked_range_after.observed_ms > finished_unix_ms
        || raw.document_binding.before.observed_ms < started_unix_ms
        || raw.document_binding.after.observed_ms < raw.document_binding.before.observed_ms
        || raw.document_binding.after.observed_ms > finished_unix_ms
        || raw.value_readback.as_ref().is_some_and(|readback| {
            readback.observed_ms < started_unix_ms || readback.observed_ms > finished_unix_ms
        })
    {
        return Err(
            "native watch owned-document evidence does not match its registration or record".into(),
        );
    }
    let start_age_ms = header
        .received_uptime_ns
        .checked_sub(observation.structure_started_uptime_ns)
        .ok_or("owned-document start uptime is in the future")?
        .div_ceil(1_000_000);
    let finish_age_ms = header
        .received_uptime_ns
        .checked_sub(observation.structure_finished_uptime_ns)
        .ok_or("owned-document finish uptime is in the future")?
        .div_ceil(1_000_000);
    let uptime_start_unix_ms = header
        .received_unix_ms
        .checked_sub(start_age_ms)
        .ok_or("owned-document start predates its clock mapping")?;
    let uptime_finish_unix_ms = header
        .received_unix_ms
        .checked_sub(finish_age_ms)
        .ok_or("owned-document finish predates its clock mapping")?;
    if (i128::from(started_unix_ms) - i128::from(uptime_start_unix_ms)).abs()
        > CLOCK_JUMP_TOLERANCE_MS
        || (i128::from(finished_unix_ms) - i128::from(uptime_finish_unix_ms)).abs()
            > CLOCK_JUMP_TOLERANCE_MS
    {
        return Err("owned-document wall and uptime stamps disagree".into());
    }
    raw.validate_at(header.received_unix_ms)
        .map_err(|_| "native watch returned invalid or stale owned-document evidence")?;

    let mut mapped = raw.clone();
    let convert = |raw_ms: &mut u64| -> Result<(), String> {
        *raw_ms = raw_ms
            .checked_sub(origin_unix_ms)
            .ok_or("owned-document timestamp predates the source clock")?;
        Ok(())
    };
    convert(&mut mapped.provenance.observed_ms)?;
    convert(&mut mapped.provenance.valid_until_ms)?;
    convert(&mut mapped.marked_range_before.observed_ms)?;
    convert(&mut mapped.marked_range_after.observed_ms)?;
    convert(&mut mapped.document_binding.before.observed_ms)?;
    convert(&mut mapped.document_binding.after.observed_ms)?;
    if let Some(readback) = mapped.value_readback.as_mut() {
        convert(&mut readback.observed_ms)?;
    }
    mapped.provenance.clock_domain = outer_clock_domain.to_string();
    mapped
        .validate_at(
            header
                .received_unix_ms
                .checked_sub(origin_unix_ms)
                .ok_or("owned-document callback predates the source clock")?,
        )
        .map_err(|_| "owned-document evidence expired during source clock conversion")?;
    Ok(Some(mapped))
}

#[derive(Debug, Clone, PartialEq)]
struct Geometry {
    revision: u64,
    pid: u32,
    window: String,
    window_frame: Rect,
    content_rect: Rect,
    width: u32,
    height: u32,
    scale: f64,
    content_scale: f64,
}

impl From<&NativeFrame> for Geometry {
    fn from(frame: &NativeFrame) -> Self {
        Self {
            revision: frame.geometry_revision,
            pid: frame.pid,
            window: frame.window_id.clone(),
            window_frame: frame.frame,
            content_rect: frame.content_rect,
            width: frame.width_px,
            height: frame.height_px,
            scale: frame.scale_factor,
            content_scale: frame.content_scale,
        }
    }
}

#[derive(Clone)]
struct Image {
    id: String,
    source_ms: u64,
    display_ticks: u64,
    display_ns: u64,
    checksum: String,
    geometry: Geometry,
}

pub struct WatchTracker {
    config: NativeConfig,
    session_epoch: u64,
    clock_domain: String,
    origin_unix_ms: u64,
    capture_interval_ms: u64,
    stream_id: Option<String>,
    expected_watch_id: Option<String>,
    pending_capture: Option<interaction_contracts::desktop::source::SourceCommand>,
    sequence: Option<u64>,
    clock_pair: Option<(u64, u64)>,
    max_callback_unix_ms: u64,
    lane_uptime: std::collections::BTreeMap<String, u64>,
    target_pid: Option<u32>,
    geometry: Option<Geometry>,
    image: Option<Image>,
    last_display_ns: Option<u64>,
    native_drops: u64,
    observer_revision: Option<u64>,
    tree_revision: Option<u64>,
    /// Bounded recent duplicate detector, keyed by actual AX-read stamps rather
    /// than stream record sequence. The producer mints fresh per-read ids; this
    /// table catches reuse while the source read remains in the recent window.
    read_scoped_reads: std::collections::VecDeque<ReadScopedRead>,
    identity_boundary_namespace: Option<String>,
    identity_boundary_generation: Option<u64>,
    owned_identity_boundary_namespace: Option<String>,
    owned_identity_boundary_generation: Option<u64>,
    last_meta: Option<FrameMeta>,
    last_complete: Option<Value>,
    pending_image: Option<Arc<[u8]>>,
    latest_observation: Option<Value>,
    last_pixel_observation: Option<Value>,
    last_gap_reason: Option<String>,
    last_screenshot_failure: Option<String>,
    last_ax_failure: Option<String>,
    menu_source_session_id: Option<String>,
    focus_source_session_id: Option<String>,
    focus_process: Option<interaction_contracts::desktop::source::KernelProcessIdentity>,
    window_source_session_id: Option<String>,
    window_process: Option<interaction_contracts::desktop::source::KernelProcessIdentity>,
    background_process: Option<KernelProcessIdentity>,
    menu_anchor_history: std::collections::BTreeMap<
        String,
        interaction_contracts::desktop::source::MenuOpenerAnchor,
    >,
    menu_anchor_revision: u64,
}

impl WatchTracker {
    pub fn new(
        config: NativeConfig,
        session_epoch: u64,
        clock_domain: String,
        origin_unix_ms: u64,
        capture_interval_ms: u64,
    ) -> Self {
        Self {
            target_pid: config.pid,
            config,
            session_epoch,
            clock_domain,
            origin_unix_ms,
            capture_interval_ms,
            stream_id: None,
            expected_watch_id: None,
            pending_capture: None,
            sequence: None,
            clock_pair: None,
            max_callback_unix_ms: origin_unix_ms,
            geometry: None,
            image: None,
            last_display_ns: None,
            native_drops: 0,
            observer_revision: None,
            tree_revision: None,
            read_scoped_reads: Default::default(),
            identity_boundary_namespace: None,
            identity_boundary_generation: None,
            owned_identity_boundary_namespace: None,
            owned_identity_boundary_generation: None,
            lane_uptime: Default::default(),
            last_meta: None,
            last_complete: None,
            pending_image: None,
            latest_observation: None,
            last_pixel_observation: None,
            last_gap_reason: None,
            last_screenshot_failure: None,
            last_ax_failure: None,
            menu_source_session_id: None,
            focus_source_session_id: None,
            focus_process: None,
            window_source_session_id: None,
            window_process: None,
            background_process: None,
            menu_anchor_history: Default::default(),
            menu_anchor_revision: 0,
        }
    }

    pub fn stream_id(&self) -> Option<&str> {
        self.stream_id.as_deref()
    }
    pub fn sequence(&self) -> u64 {
        self.sequence.unwrap_or(0)
    }

    pub fn ingest(
        &mut self,
        value: Value,
        assets: &Path,
        now_unix_ms: u64,
    ) -> Result<WatchReport, String> {
        let header: Header = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid watch record header: {error}"))?;
        let lane = if header.kind == "health" {
            format!("health:{}", value["source"].as_str().unwrap_or("all"))
        } else {
            header.kind.clone()
        };
        self.check_header(&header, now_unix_ms, &lane)?;
        let mut report = WatchReport::new(header.stream_id.clone(), header.sequence);
        match header.kind.as_str() {
            "capture_result" => {
                report = self.capture_result(&header, value.clone(), assets, now_unix_ms)?;
            }
            "frame" => {
                let frame: NativeFrame = serde_json::from_value(value["frame"].clone())
                    .map_err(|error| format!("invalid native frame: {error}"))?;
                let meta = self.frame(&header, frame, assets)?;
                let valid = matches!(meta.status, FrameStatus::Complete | FrameStatus::Idle)
                    && meta
                        .stream_info()
                        .is_some_and(|stream| stream.image_frame_id.is_some());
                report.health.push(self.health(
                    &header,
                    SourceKind::Screenshot,
                    if valid {
                        SourceState::Available
                    } else {
                        SourceState::Unavailable
                    },
                    (!valid).then(|| "native capture has no current complete image".into()),
                )?);
                report.image = self.pending_image.take();
                report.frame_meta = Some(meta);
                self.last_meta = report.frame_meta.clone();
                if report
                    .frame_meta
                    .as_ref()
                    .is_some_and(|meta| meta.status == FrameStatus::Complete)
                {
                    self.last_gap_reason = None;
                    self.last_complete =
                        Some(json!({"record":value,"frame_meta":report.frame_meta}));
                }
            }
            "observation" => {
                report = self.observation(&header, value.clone())?;
                self.latest_observation = Some(value.clone());
                if value["frame_ref"].as_str().is_some() {
                    self.last_pixel_observation = Some(value.clone());
                }
            }
            "health" => {
                let state: SourceState = serde_json::from_value(value["state"].clone())
                    .map_err(|error| format!("invalid native health state: {error}"))?;
                let reason = value["reason"].as_str().map(short);
                let sources = match value.get("source") {
                    Some(source) => vec![
                        serde_json::from_value::<SourceKind>(source.clone())
                            .map_err(|error| format!("invalid native health source: {error}"))?,
                    ],
                    None => vec![
                        SourceKind::Ax,
                        SourceKind::AxObserver,
                        SourceKind::Screenshot,
                        SourceKind::Ocr,
                    ],
                };
                for source in sources {
                    if source == SourceKind::Screenshot && state == SourceState::Unavailable {
                        self.image = None;
                    }
                    report
                        .health
                        .push(self.health(&header, source, state, reason.clone())?);
                }
            }
            "gap" => {
                let reason = value["reason"]
                    .as_str()
                    .filter(|reason| !reason.trim().is_empty())
                    .map(short);
                let dropped = value["dropped_frames"]
                    .as_u64()
                    .filter(|count| *count > 0 || reason.is_some())
                    .ok_or("native gap has neither a discard count nor a failure reason")?;
                self.native_drops = self
                    .native_drops
                    .checked_add(dropped)
                    .ok_or("native discard count overflow")?;
                self.image = None;
                self.last_gap_reason = reason.clone();
                let reason =
                    reason.unwrap_or_else(|| "native source records were discarded".into());
                report.health = [
                    SourceKind::Ax,
                    SourceKind::AxObserver,
                    SourceKind::Screenshot,
                    SourceKind::Ocr,
                ]
                .map(|source| {
                    self.health(
                        &header,
                        source,
                        SourceState::Unavailable,
                        Some(reason.clone()),
                    )
                })
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?;
            }
            "error" => return Err("the native watch helper reported a protocol error".into()),
            _ => return Err("unknown native watch record kind".into()),
        }
        for health in &report.health {
            let saved = match health.source {
                SourceKind::Screenshot => &mut self.last_screenshot_failure,
                SourceKind::Ax => &mut self.last_ax_failure,
                SourceKind::Ocr | SourceKind::AxObserver => continue,
            };
            if health.state == SourceState::Available {
                *saved = None;
            } else if health.state == SourceState::Unavailable
                && let Some(reason) = &health.reason
                && !reason.trim().is_empty()
                && !matches!(
                    reason.as_str(),
                    "watch stopped"
                        | "AX session closed"
                        | "native capture has no current complete image"
                )
            {
                *saved = Some(reason.clone());
            }
        }
        // The manifest preserves raw callback metadata, including SDK IDLE
        // timestamps; the public projection separately pins pixel provenance.
        let manifest = assets.join("watch-manifest.json");
        let temporary = assets.join("watch-manifest.json.tmp");
        private_json(
            &temporary,
            &json!({ "protocol_version":1, "record":value,
            "stream_id":header.stream_id,"frame_meta":report.frame_meta, "snapshot":report.snapshot,
            "last_complete":self.last_complete,"latest_observation":self.latest_observation,
            "last_pixel_observation":self.last_pixel_observation,
            "current_image_frame_id":self.image.as_ref().map(|image|image.id.clone()) }),
        )?;
        fs::rename(temporary, &manifest)
            .map_err(|error| format!("publishing watch manifest: {error}"))?;
        report.manifest = Some(manifest);
        Ok(report)
    }

    fn check_header(
        &mut self,
        header: &Header,
        now_unix_ms: u64,
        lane: &str,
    ) -> Result<(), String> {
        if header.protocol_version != 1
            || self
                .expected_watch_id
                .as_ref()
                .is_some_and(|expected| expected != &header.stream_id)
            || header.sequence == 0
            || header.stream_id.trim().is_empty()
            || header.stream_id.len() > 256
            || header.stream_id.chars().any(char::is_control)
        {
            return Err("invalid native stream identity or version".into());
        }
        if self
            .stream_id
            .as_ref()
            .is_some_and(|stream| stream != &header.stream_id)
        {
            return Err("native stream identity changed inside one helper session".into());
        }
        let expected = self
            .sequence
            .map_or(Some(1), |sequence| sequence.checked_add(1))
            .ok_or("native record sequence exhausted")?;
        if header.sequence != expected {
            return Err("native watch record sequence was duplicated, reversed or lost".into());
        }
        if self
            .lane_uptime
            .get(lane)
            .is_some_and(|previous| header.received_uptime_ns < *previous)
        {
            return Err("native callback uptime went backwards within its source lane".into());
        }
        let callback_age = now_unix_ms
            .checked_sub(header.received_unix_ms)
            .ok_or("native callback is in the future")?;
        if callback_age > self.capture_interval_ms.saturating_mul(3).max(3_000) {
            return Err("native callback was stale before it reached the source".into());
        }
        if let Some((unix_ms, uptime_ns)) = self.clock_pair {
            let wall_delta = i128::from(header.received_unix_ms) - i128::from(unix_ms);
            let uptime_delta =
                (i128::from(header.received_uptime_ns) - i128::from(uptime_ns)) / 1_000_000;
            if (wall_delta - uptime_delta).abs() > CLOCK_JUMP_TOLERANCE_MS {
                return Err("native wall clock mapping changed; revalidation is required".into());
            }
        }
        header
            .received_unix_ms
            .checked_sub(self.origin_unix_ms)
            .ok_or("native callback predates the declared clock origin")?;
        self.stream_id = Some(header.stream_id.clone());
        self.sequence = Some(header.sequence);
        self.clock_pair = Some((header.received_unix_ms, header.received_uptime_ns));
        self.max_callback_unix_ms = self.max_callback_unix_ms.max(header.received_unix_ms);
        self.lane_uptime
            .insert(lane.into(), header.received_uptime_ns);
        Ok(())
    }

    fn health(
        &self,
        header: &Header,
        source: SourceKind,
        state: SourceState,
        reason: Option<String>,
    ) -> Result<SourceHealth, String> {
        Ok(SourceHealth {
            source,
            state,
            reason,
            at_ms: header
                .received_unix_ms
                .checked_sub(self.origin_unix_ms)
                .ok_or("native health predates the clock origin")?,
            provenance: Some(SourceStamp {
                session_epoch: self.session_epoch,
                stream_id: header.stream_id.clone(),
                source_sequence: header.sequence,
            }),
        })
    }

    fn pin_pid(&mut self, pid: u32) -> Result<(), String> {
        if pid == 0 || self.target_pid.is_some_and(|expected| expected != pid) {
            return Err("native watch changed or lost its target process".into());
        }
        self.target_pid = Some(pid);
        Ok(())
    }

    fn frame(
        &mut self,
        header: &Header,
        frame: NativeFrame,
        assets: &Path,
    ) -> Result<FrameMeta, String> {
        if self.config.background_target.is_some() {
            return Err("background AX-only watch cannot publish screen frames".into());
        }
        self.pin_pid(frame.pid)?;
        if frame.window_id.trim().is_empty()
            || frame.window_id.len() > 256
            || !rect_valid(frame.frame)
            || !rect_valid(frame.content_rect)
            || !frame.scale_factor.is_finite()
            || frame.scale_factor <= 0.0
            || !frame.content_scale.is_finite()
            || frame.content_scale <= 0.0
            || frame.dirty_rects.len() > 256
            || frame.dirty_rects.iter().any(|rect| !rect_valid(*rect))
            || frame.width_px > 4_096
            || frame.height_px > 4_096
            || frame.geometry_revision == 0
        {
            return Err("native frame has invalid or unsupported geometry".into());
        }
        let geometry = Geometry::from(&frame);
        if self.geometry.as_ref().is_some_and(|old| old != &geometry) {
            if self
                .geometry
                .as_ref()
                .is_some_and(|old| frame.geometry_revision < old.revision)
            {
                return Err("native geometry revision went backwards".into());
            }
            self.image = None;
        }
        self.geometry = Some(geometry.clone());
        let callback_ms = header
            .received_unix_ms
            .checked_sub(self.origin_unix_ms)
            .ok_or("native callback predates its origin")?;
        if frame.status == FrameStatus::Complete {
            let id = frame
                .frame_id
                .clone()
                .filter(|id| !id.trim().is_empty() && id.len() <= 256)
                .ok_or("complete native frame has no bounded image identity")?;
            let ns = frame
                .display_time_uptime_ns
                .filter(|time| *time > 0)
                .ok_or("complete native frame has no display timestamp")?;
            let ticks = frame
                .display_time_ticks
                .filter(|time| *time > 0)
                .ok_or("complete native frame has no raw display timestamp")?;
            if self.last_display_ns.is_some_and(|old| ns <= old)
                || self.image.as_ref().is_some_and(|old| old.id == id)
            {
                return Err("native complete image or display timestamp was replayed".into());
            }
            let age_ns = header
                .received_uptime_ns
                .checked_sub(ns)
                .ok_or("native display timestamp is after its callback")?;
            let age_ms = age_ns.div_ceil(1_000_000);
            let source_unix_ms = frame
                .source_unix_ms
                .ok_or("native frame has no original wall time")?;
            let derived = header
                .received_unix_ms
                .checked_sub(age_ms)
                .ok_or("native image wall time underflow")?;
            if source_unix_ms.abs_diff(derived) > 2 {
                return Err("native image wall time does not match its display provenance".into());
            }
            let source_ms = source_unix_ms
                .checked_sub(self.origin_unix_ms)
                .ok_or("native image predates the declared clock origin")?;
            if frame.width_px == 0 || frame.height_px == 0 {
                return Err("complete native image has no pixel dimensions".into());
            }
            validate_asset(
                assets,
                &json!({"png_file":frame.png_file,"byte_length":frame.byte_length}),
            )?;
            let path = assets.join(frame.png_file.as_ref().expect("validated PNG basename"));
            let mut bytes = Vec::new();
            fs::File::open(path)
                .map_err(|error| format!("opening native PNG: {error}"))?
                .take(crate::native::MAX_CAPTURE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("reading native PNG: {error}"))?;
            if bytes.len() as u64 != frame.byte_length {
                return Err("native PNG changed before checksum calculation".into());
            }
            let checksum = format!("{:x}", Sha256::digest(&bytes));
            self.pending_image = Some(bytes.into());
            self.image = Some(Image {
                id,
                source_ms,
                display_ticks: ticks,
                display_ns: ns,
                checksum,
                geometry: geometry.clone(),
            });
            self.last_display_ns = Some(ns);
        } else if frame.png_file.is_some() || frame.byte_length != 0 {
            return Err("noncomplete native status tried to supply new pixels".into());
        }
        if matches!(
            frame.status,
            FrameStatus::Blank
                | FrameStatus::Suspended
                | FrameStatus::Stopped
                | FrameStatus::Started
        ) {
            self.image = None;
        }
        if frame.status == FrameStatus::Idle
            && self.image.as_ref().is_some_and(|image| {
                frame.frame_id.as_ref() != Some(&image.id) || image.geometry != geometry
            })
        {
            self.image = None;
        }
        let image = self.image.as_ref();
        Ok(FrameMeta {
            frame_id: format!("{}-event-{}", header.stream_id, header.sequence),
            session_epoch: self.session_epoch,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: self.session_epoch,
            },
            clock_domain: self.clock_domain.clone(),
            source_sequence: header.sequence,
            source_ms: image.map_or(callback_ms, |image| image.source_ms),
            status: frame.status,
            geometry_revision: frame.geometry_revision,
            width_px: frame.width_px,
            height_px: frame.height_px,
            scale_factor: frame.scale_factor,
            byte_length: frame.byte_length,
            checksum: image.map_or(String::new(), |image| image.checksum.clone()),
            provenance: Some(
                interaction_contracts::desktop::source::FrameProvenance::Stream {
                    stream: FrameStreamInfo {
                        stream_id: header.stream_id.clone(),
                        image_frame_id: image.map(|image| image.id.clone()),
                        display_time_ticks: image.map_or(0, |image| image.display_ticks),
                        display_time_ns: image.map_or(0, |image| image.display_ns),
                        received_uptime_ns: header.received_uptime_ns,
                        received_unix_ms: header.received_unix_ms,
                        callback_ms,
                        source_clock: "mach_absolute".into(),
                        content_rect: frame.content_rect,
                        content_scale: frame.content_scale,
                        dirty_rects: frame.dirty_rects,
                        dropped_frames: self.native_drops,
                        capture_interval_ms: self.capture_interval_ms,
                        window_id: frame.window_id,
                        pid: frame.pid,
                        window_frame: frame.frame,
                    },
                },
            ),
        })
    }

    fn track_read_scoped_tokens(
        &mut self,
        identity: Option<NativeReadIdentity>,
        objects: &[UiObject],
    ) -> Result<(), String> {
        let tokens: std::collections::HashSet<_> = objects
            .iter()
            .filter(|object| object.identity_continuity == ObjectIdentityContinuity::ReadScoped)
            .map(|object| object.instance_ref.as_str().to_string())
            .collect();
        let Some(identity) = identity else {
            if tokens.is_empty() {
                return Ok(());
            }
            return Err("read-scoped watch objects require paired AX read timestamps".into());
        };

        if self.read_scoped_reads.iter().any(|read| {
            read.identity != identity && tokens.iter().any(|token| read.tokens.contains(token))
        }) {
            return Err("native watch reused a read-scoped object token across AX reads".into());
        }

        if let Some(index) = self
            .read_scoped_reads
            .iter()
            .position(|read| read.identity == identity)
        {
            let mut read = self.read_scoped_reads.remove(index).unwrap();
            let new_token_count = tokens.difference(&read.tokens).count();
            if read.tokens.len() + new_token_count > 512 {
                return Err("native watch changed object tokens within one AX read".into());
            }
            read.tokens.extend(tokens);
            self.read_scoped_reads.push_back(read);
        } else {
            self.read_scoped_reads
                .push_back(ReadScopedRead { identity, tokens });
        }
        while self.read_scoped_reads.len() > MAX_TRACKED_AX_READ_IDENTITIES {
            self.read_scoped_reads.pop_front();
        }
        Ok(())
    }

    fn observation(&mut self, header: &Header, value: Value) -> Result<WatchReport, String> {
        let observation: Observation = serde_json::from_value(value)
            .map_err(|error| format!("invalid native watch structure: {error}"))?;
        self.pin_pid(observation.structure.app.pid)?;
        crate::native::validate_observer_coverage(
            &observation.structure.objects,
            &observation.structure.observer_coverage,
        )?;
        let observer_state = observation.structure.observer_coverage.state;
        if observation.observer_available != (observer_state != SourceState::Unavailable) {
            return Err("native observer channel flag contradicts observer coverage state".into());
        }
        if !observation.permissions.accessibility && observer_state != SourceState::Unavailable {
            return Err(
                "native observer coverage is available without Accessibility permission".into(),
            );
        }
        let read_identity = native_read_identity(header, &observation)?;
        let identity_boundary = observation.structure.identity_boundary.as_ref();
        if observation.structure.complete && identity_boundary.is_none() {
            return Err("complete native watch structure has no identity boundary".into());
        }
        if let Some(boundary) = identity_boundary {
            boundary.validate()?;
            if self
                .identity_boundary_namespace
                .as_ref()
                .is_some_and(|namespace| namespace != &boundary.namespace)
            {
                return Err("native AX identity namespace changed inside one watch".into());
            }
            if self
                .identity_boundary_generation
                .is_some_and(|generation| boundary.generation < generation)
            {
                return Err("native AX identity generation moved backwards".into());
            }
        }
        if observation.structure.app.bundle_id != self.config.bundle_id
            || observation.structure.objects.len() > 512
            || observation.structure.windows.len() > 128
            || observation.ocr.blocks.len() > 128
            || observation.tree_revision == 0
            || self
                .tree_revision
                .is_some_and(|old| observation.tree_revision < old)
            || self
                .observer_revision
                .is_some_and(|old| observation.observer_revision < old)
            || observation.structure_started_uptime_ns > observation.structure_finished_uptime_ns
            || observation.structure_finished_uptime_ns > header.received_uptime_ns
            || observation
                .structure
                .objects
                .iter()
                .any(|object| object.secure && object.value.is_some())
        {
            return Err("native watch structure violated its scope or bounds".into());
        }
        self.tree_revision = Some(observation.tree_revision);
        self.observer_revision = Some(observation.observer_revision);
        let age_ms = (header.received_uptime_ns - observation.structure_started_uptime_ns)
            .div_ceil(1_000_000);
        let source_ms = header
            .received_unix_ms
            .checked_sub(age_ms)
            .and_then(|time| time.checked_sub(self.origin_unix_ms))
            .ok_or("native AX observation predates the source clock")?;
        let snapshot_id = format!("{}-tree-{}", header.stream_id, header.sequence);
        let owned_document_evidence = map_owned_document_evidence(
            &self.config,
            &self.clock_domain,
            self.origin_unix_ms,
            header,
            &observation,
            &snapshot_id,
        )?;
        if let Some(boundary) = owned_document_evidence
            .as_ref()
            .map(|evidence| &evidence.provenance.identity_boundary)
        {
            boundary.validate()?;
            if self
                .owned_identity_boundary_namespace
                .as_ref()
                .is_some_and(|namespace| namespace != &boundary.namespace)
                || self
                    .owned_identity_boundary_generation
                    .is_some_and(|generation| boundary.generation < generation)
            {
                return Err("owned-document identity boundary changed or moved backwards".into());
            }
        }
        self.check_window_provenance(header, &observation.structure)?;
        self.check_menu_provenance(header, &observation.structure)?;
        self.check_focus_provenance(header, &observation.structure)?;
        crate::native::validate_requested_table_scope(
            &self.config,
            observation.structure.table_scope_search.as_ref(),
        )?;
        let (target_context, target_process) = map_background_target_context(
            &self.config,
            self.session_epoch,
            header,
            &observation,
            source_ms,
        )?;
        if let Some(process) = target_process {
            if self
                .background_process
                .as_ref()
                .is_some_and(|previous| previous != &process)
            {
                return Err("background AX target process birth changed inside one watch".into());
            }
            self.background_process = Some(process);
        }
        let mut snapshot = StructureSnapshot {
            snapshot_id: RequestId::new(snapshot_id).map_err(|error| error.to_string())?,
            session_epoch: self.session_epoch,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: self.session_epoch,
            },
            clock_domain: self.clock_domain.clone(),
            identity_boundary: observation.structure.identity_boundary.clone(),
            // Preserve the current observation's exact image content ID. A
            // missing value remains missing; older frames are never inferred.
            frame_ref: observation.frame_ref.clone(),
            observer_coverage: observation.structure.observer_coverage.clone(),
            owned_document_evidence: owned_document_evidence.map(Box::new),
            source_sequence: header.sequence,
            observed_ms: source_ms,
            global_focus: observation
                .structure
                .global_focus
                .clone()
                .unwrap_or_else(crate::native::unavailable_global_focus),
            target_context,
            app: observation.structure.app,
            windows: observation.structure.windows,
            complete: observation.permissions.accessibility && observation.structure.complete,
            incomplete_reason: if observation.permissions.accessibility {
                observation.structure.incomplete_reason
            } else {
                Some("Accessibility permission is unavailable".into())
            },
            objects: observation.structure.objects,
            menu_anchors: observation.structure.menu_anchors,
            menu_context: observation.structure.menu_context,
            focus_context: observation.structure.focus_context,
            window_context: observation.structure.window_context,
            table_viewports: observation.structure.table_viewports,
            table_scope_search: observation.structure.table_scope_search,
        };
        crate::native::normalize_snapshot_provenance(&mut snapshot, header.sequence);
        crate::native::validate_native_global_focus_window(&snapshot)?;
        snapshot
            .validate()
            .map_err(|error| format!("invalid native AX snapshot: {error}"))?;
        self.track_read_scoped_tokens(read_identity, &snapshot.objects)?;
        if let Some(boundary) = &snapshot.identity_boundary {
            self.identity_boundary_namespace = Some(boundary.namespace.clone());
            self.identity_boundary_generation = Some(boundary.generation);
        }
        if let Some(boundary) = snapshot
            .owned_document_evidence
            .as_ref()
            .map(|evidence| &evidence.provenance.identity_boundary)
        {
            self.owned_identity_boundary_namespace = Some(boundary.namespace.clone());
            self.owned_identity_boundary_generation = Some(boundary.generation);
        }
        let mut report = WatchReport::new(header.stream_id.clone(), header.sequence);
        report.health.push(self.health(
            header,
            SourceKind::Ax,
            if !observation.permissions.accessibility {
                SourceState::Unavailable
            } else if snapshot.complete {
                SourceState::Available
            } else {
                SourceState::Degraded
            },
            if snapshot.complete {
                None
            } else {
                snapshot.incomplete_reason.clone()
            },
        )?);
        report.health.push(self.health(
            header,
            SourceKind::AxObserver,
            observer_state,
            crate::native::observer_coverage_reason(&snapshot.observer_coverage),
        )?);
        let frame_matches = self
            .image
            .as_ref()
            .is_some_and(|image| observation.frame_ref.as_ref() == Some(&image.id));
        report.health.push(
            self.health(
                header,
                SourceKind::Ocr,
                if !frame_matches || !observation.permissions.screen_capture {
                    SourceState::Unavailable
                } else if observation.ocr.complete {
                    SourceState::Available
                } else {
                    SourceState::Degraded
                },
                observation
                    .ocr
                    .reason
                    .or_else(|| observation.errors.first().map(short)),
            )?,
        );
        if self.config.background_target.is_some() {
            report.health.push(self.health(
                header,
                SourceKind::Screenshot,
                SourceState::Unavailable,
                Some("background AX-only Source does not capture pixels".into()),
            )?);
        }
        report.snapshot = Some(snapshot);
        Ok(report)
    }

    pub(super) fn interval_ms(&self) -> u64 {
        self.capture_interval_ms
    }

    pub(super) fn session_epoch(&self) -> u64 {
        self.session_epoch
    }

    fn check_window_provenance(
        &mut self,
        header: &Header,
        structure: &NativeStructure,
    ) -> Result<(), String> {
        let Some(context) = &structure.window_context else {
            return Ok(());
        };
        let fixed_window = match &self.config.text_policy {
            Some(TextReadPolicy::RegisteredEditor { window_id })
                if self.config.pid == Some(structure.app.pid)
                    && crate::config::REGISTERED_EDITOR_BUNDLES
                        .contains(&self.config.bundle_id.as_str()) =>
            {
                window_id
            }
            _ => return Err("native window facts require trusted fixed window policy".into()),
        };
        context.validate()?;
        // These are producer-gated read-only window facts. Partial capture
        // privacy stays private to Native Source; this census only limits the
        // public window assertion to the exact requested, unique row domain.
        let scoped_window = structure.table_scope_search.as_ref().is_some_and(|scope| {
            scope.unique_candidate().is_some()
                && self.config.table_scope.as_ref() == Some(&scope.requested_scope)
                && scope.process_id == context.process.pid
                && scope.window_id == context.origin_window_id
                && structure.focus_context.is_none()
        });
        if (!structure.complete && !scoped_window)
            || structure.menu_context.is_some()
            || structure.objects.iter().any(|object| object.secure)
            || context.origin_window_id != *fixed_window
            || context.session_epoch != self.session_epoch
            || context.process.pid != structure.app.pid
            || context.verified_source_sequence != header.sequence
            || context.verified_observed_ms != header.received_unix_ms
            || self
                .window_source_session_id
                .as_ref()
                .is_some_and(|id| id != &context.source_session_id)
            || self
                .window_process
                .as_ref()
                .is_some_and(|process| process != &context.process)
            || self
                .focus_source_session_id
                .as_ref()
                .is_some_and(|id| id != &context.source_session_id)
            || self
                .focus_process
                .as_ref()
                .is_some_and(|process| process != &context.process)
            || self
                .menu_source_session_id
                .as_ref()
                .is_some_and(|id| id != &context.source_session_id)
            || structure.menu_anchors.iter().any(|anchor| {
                anchor.source_session_id != context.source_session_id
                    || anchor.process != context.process
            })
            || structure.focus_context.as_ref().is_some_and(|focus| {
                focus.source_session_id != context.source_session_id
                    || focus.process != context.process
            })
        {
            return Err("native window facts changed trusted scope or raw provenance".into());
        }
        self.window_source_session_id = Some(context.source_session_id.clone());
        self.window_process = Some(context.process.clone());
        Ok(())
    }

    fn check_focus_provenance(
        &mut self,
        header: &Header,
        structure: &NativeStructure,
    ) -> Result<(), String> {
        let Some(focus) = &structure.focus_context else {
            return Ok(());
        };
        let fixed_window = match &self.config.text_policy {
            Some(TextReadPolicy::RegisteredEditor { window_id })
                if self.config.pid == Some(structure.app.pid)
                    && crate::config::REGISTERED_EDITOR_BUNDLES
                        .contains(&self.config.bundle_id.as_str()) =>
            {
                window_id
            }
            _ => return Err("native focus facts require trusted fixed window policy".into()),
        };
        focus.validate()?;
        let composition_scope_valid = match &focus.composition {
            interaction_contracts::desktop::source::CompositionState::Unknown
            | interaction_contracts::desktop::source::CompositionState::NotApplicable => true,
            interaction_contracts::desktop::source::CompositionState::Marked { .. }
            | interaction_contracts::desktop::source::CompositionState::Unmarked { .. } => {
                structure.objects.iter().any(|object| {
                    object.instance_ref == focus.focused_instance
                        && matches!(object.role.as_str(), "AXTextField" | "AXTextArea")
                        && object.focused
                        && object.enabled
                        && object.in_view
                        && !object.secure
                })
            }
        };
        let composition_raw_provenance_valid = match &focus.composition {
            interaction_contracts::desktop::source::CompositionState::Marked { proof }
            | interaction_contracts::desktop::source::CompositionState::Unmarked { proof } => {
                proof.focused_instance == focus.focused_instance
                    && proof.verified_source_sequence == header.sequence
                    && proof.verified_observed_ms == header.received_unix_ms
            }
            interaction_contracts::desktop::source::CompositionState::Unknown
            | interaction_contracts::desktop::source::CompositionState::NotApplicable => true,
        };
        if !composition_scope_valid || !composition_raw_provenance_valid {
            return Err(
                "native composition proof requires its exact text receiver and raw Source stamps"
                    .into(),
            );
        }
        if !structure.complete
            || structure.menu_context.is_some()
            || focus.origin_window_id != *fixed_window
            || focus.session_epoch != self.session_epoch
            || focus.process.pid != structure.app.pid
            || focus.verified_source_sequence != header.sequence
            || focus.verified_observed_ms != header.received_unix_ms
            || self
                .focus_source_session_id
                .as_ref()
                .is_some_and(|id| id != &focus.source_session_id)
            || self
                .focus_process
                .as_ref()
                .is_some_and(|process| process != &focus.process)
            || self
                .window_source_session_id
                .as_ref()
                .is_some_and(|id| id != &focus.source_session_id)
            || self
                .window_process
                .as_ref()
                .is_some_and(|process| process != &focus.process)
        {
            return Err("native focus facts changed trusted scope or raw provenance".into());
        }
        self.focus_source_session_id = Some(focus.source_session_id.clone());
        self.focus_process = Some(focus.process.clone());
        Ok(())
    }

    fn check_menu_provenance(
        &mut self,
        header: &Header,
        structure: &NativeStructure,
    ) -> Result<(), String> {
        if structure.menu_anchors.is_empty() && structure.menu_context.is_none() {
            if structure.complete {
                self.menu_anchor_history.clear();
            }
            return Ok(());
        }
        let fixed_window = match &self.config.text_policy {
            Some(TextReadPolicy::RegisteredEditor { window_id })
                if self.config.pid == Some(structure.app.pid)
                    && crate::config::REGISTERED_EDITOR_BUNDLES
                        .contains(&self.config.bundle_id.as_str()) =>
            {
                window_id
            }
            _ => {
                return Err(
                    "native menu authority needs the trusted registered fixed window policy".into(),
                );
            }
        };
        if structure.menu_anchors.len() > 8 || !structure.complete {
            return Err("native menu anchor projection is incomplete or exceeds its bound".into());
        }
        let mut new_revisions = std::collections::BTreeSet::new();
        for anchor in &structure.menu_anchors {
            if anchor.origin_window_id != *fixed_window
                || anchor.process.pid != structure.app.pid
                || anchor.verified_source_sequence != header.sequence
                || anchor.verified_observed_ms != header.received_unix_ms
                || self
                    .window_source_session_id
                    .as_ref()
                    .is_some_and(|id| id != &anchor.source_session_id)
                || self
                    .window_process
                    .as_ref()
                    .is_some_and(|process| process != &anchor.process)
                || self
                    .menu_source_session_id
                    .as_ref()
                    .is_some_and(|session| session != &anchor.source_session_id)
                || self
                    .menu_anchor_history
                    .get(&anchor.anchor_id)
                    .is_some_and(|old| !old.same_identity(anchor))
                || !self.menu_anchor_history.contains_key(&anchor.anchor_id)
                    && (anchor.anchor_revision <= self.menu_anchor_revision
                        || !new_revisions.insert(anchor.anchor_revision))
            {
                return Err(
                    "native menu anchor changed identity, fixed scope or raw provenance".into(),
                );
            }
            anchor.validate()?;
            self.menu_source_session_id = Some(anchor.source_session_id.clone());
        }
        self.menu_anchor_revision = self.menu_anchor_revision.max(
            structure
                .menu_anchors
                .iter()
                .map(|anchor| anchor.anchor_revision)
                .max()
                .unwrap_or(0),
        );
        if let Some(context) = &structure.menu_context {
            context.validate()?;
            if context.opener.verified_source_sequence != header.sequence
                || context.opener.verified_observed_ms != header.received_unix_ms
                || !structure
                    .menu_anchors
                    .iter()
                    .any(|anchor| anchor == &context.opener)
            {
                return Err("native menu context is not this record's anchored observation".into());
            }
        }
        self.menu_anchor_history = structure
            .menu_anchors
            .iter()
            .map(|anchor| (anchor.anchor_id.clone(), anchor.clone()))
            .collect();
        Ok(())
    }

    pub fn failure_health(&self, reason: &str, now_unix_ms: u64) -> Vec<SourceHealth> {
        [
            SourceKind::Ax,
            SourceKind::AxObserver,
            SourceKind::Screenshot,
            SourceKind::Ocr,
        ]
        .map(|source| SourceHealth {
            source,
            state: SourceState::Unavailable,
            reason: Some(short(reason)),
            // A terminal fault must still close the host gate when its
            // cause is a wall-clock rollback. This only dates Unavailable
            // against known callbacks; pixel and heartbeat ages stay real.
            at_ms: now_unix_ms
                .max(self.max_callback_unix_ms)
                .saturating_sub(self.origin_unix_ms),
            provenance: self.stream_id.as_ref().map(|id| SourceStamp {
                session_epoch: self.session_epoch,
                stream_id: id.clone(),
                source_sequence: self.sequence().saturating_add(1),
            }),
        })
        .to_vec()
    }

    /// Closing stdout adds no new diagnosis. Preserve the native cause that
    /// preceded teardown; parsing, clock and watchdog faults keep their own
    /// reasons because callers only use this for a closed channel/clean exit.
    pub fn closed_channel_reason(&self, fallback: &str) -> String {
        self.last_gap_reason
            .as_ref()
            .or(self.last_screenshot_failure.as_ref())
            .or(self.last_ax_failure.as_ref())
            .cloned()
            .unwrap_or_else(|| fallback.into())
    }

    pub fn failure_report(&self, reason: &str, now_unix_ms: u64) -> WatchReport {
        let sequence = self.sequence().saturating_add(1);
        let mut report = WatchReport::new(self.stream_id.clone().unwrap_or_default(), sequence);
        report.health = self.failure_health(reason, now_unix_ms);
        if let Some(mut meta) = self.last_meta.clone() {
            meta.frame_id = format!("{}-source-stop-{sequence}", report.stream_id);
            meta.source_sequence = sequence;
            meta.status = FrameStatus::Stopped;
            meta.byte_length = 0;
            if let Some(stream) = meta.stream_info_mut() {
                stream.image_frame_id = None;
                stream.dirty_rects.clear();
            }
            // This is a channel failure referencing the last real callback.
            // It deliberately invents no native uptime/capture timestamp.
            report.frame_meta = Some(meta);
        }
        report
    }
}

fn map_background_target_context(
    config: &NativeConfig,
    session_epoch: u64,
    header: &Header,
    observation: &Observation,
    observed_ms: u64,
) -> Result<(Option<TargetContext>, Option<KernelProcessIdentity>), String> {
    let Some(expected) = config.background_target.as_ref() else {
        if observation.structure.background_window_context.is_some() {
            return Err("native background target facts were not requested".into());
        }
        return Ok((None, None));
    };
    let Some(raw) = observation.structure.background_window_context.as_ref() else {
        if observation.structure.complete {
            return Err("complete background AX read is missing its fixed target context".into());
        }
        return Ok((None, None));
    };
    let app = &observation.structure.app;
    let window = &raw.window;
    if config.mode != crate::config::NativeCaptureMode::Watch
        || config.table_scope.is_some()
        || config.text_policy.is_some()
        || config.owned_document_bootstrap.is_some()
        || app.bundle_id != config.bundle_id
        || config.pid != Some(app.pid)
        || expected
            .process
            .as_ref()
            .is_some_and(|process| process != &window.process)
        || window.process.pid != app.pid
        || window.session_epoch != session_epoch
        || window.source_session_id != header.stream_id
        || window.origin_window_id != expected.window_id
        || window.verified_source_sequence != header.sequence
        || window.verified_observed_ms != header.received_unix_ms
        || !matches!(raw.hidden, ObservationFact::Known { value: false })
        || !matches!(raw.minimized, ObservationFact::Known { value: false })
        || !observation.permissions.accessibility
        || !observation.structure.complete
        || observation.structure.incomplete_reason.is_some()
        || observation.structure.active_layer.is_some()
        || observation.structure.objects.is_empty()
        || observation.structure.window_context.is_some()
        || observation.structure.focus_context.is_some()
        || !observation.structure.menu_anchors.is_empty()
        || observation.structure.menu_context.is_some()
        || observation.structure.owned_document_evidence.is_some()
        || !observation.structure.table_viewports.is_empty()
        || observation.structure.table_scope_search.is_some()
        || observation.frame_ref.is_some()
        || observation.ocr.complete
        || !observation.ocr.blocks.is_empty()
        || observation.structure.objects.iter().any(|object| {
            object
                .ax_action_names
                .as_ref()
                .is_none_or(|names| !valid_background_ax_action_names(names))
        })
        || observation
            .structure
            .objects
            .iter()
            .any(|object| object.secure)
    {
        return Err(
            "native background target facts changed their fixed scope or read bounds".into(),
        );
    }
    let [actual_window] = observation.structure.windows.as_slice() else {
        return Err("native background target requires one actual visible window".into());
    };
    if actual_window.window_id != expected.window_id
        || actual_window.kind != LayerKind::Window
        || actual_window.modal
        || actual_window.owner_bundle_id != app.bundle_id
        || actual_window.frame != window.origin_window_frame
    {
        return Err("native background target does not match its actual standard window".into());
    }
    let mut source = SourceBackgroundWindowContext {
        window: window.clone(),
        hidden: raw.hidden.clone(),
        minimized: raw.minimized.clone(),
        target_context_hash: String::new(),
    };
    source.window.verified_observed_ms = observed_ms;
    source.target_context_hash = source.computed_target_context_hash(app)?;
    source.validate(app)?;
    Ok((
        Some(TargetContext::NativeWindow {
            app: app.clone(),
            source,
        }),
        Some(window.process.clone()),
    ))
}

fn valid_background_ax_action_names(names: &[String]) -> bool {
    names.len() <= 32
        && names.iter().enumerate().all(|(index, name)| {
            !name.is_empty()
                && name.len() <= 64
                && !name.chars().any(char::is_control)
                && !names[..index].contains(name)
        })
}

#[cfg(test)]
mod background_target_context_tests {
    use super::*;
    use crate::config::{BackgroundNativeTarget, NativeCaptureMode};
    use crate::native::NativeBackgroundWindowContext;
    use interaction_contracts::desktop::ids::ObjectRef;
    use interaction_contracts::desktop::source::{
        AppIdentity, AxActionName, KernelProcessIdentity, LayerKind, ObjectIdentityContinuity,
        ObserverCoverage, Rect, SourceWindowContext, UiObject, WindowInfo,
    };

    fn header() -> Header {
        Header {
            protocol_version: 1,
            kind: "observation".into(),
            stream_id: "watch-background".into(),
            sequence: 7,
            received_uptime_ns: 2_000_000_000,
            received_unix_ms: 20_000,
        }
    }

    fn config(process: Option<KernelProcessIdentity>) -> NativeConfig {
        NativeConfig {
            helper_path: "/tmp/unexecuted-background-helper".into(),
            bundle_id: "test.app".into(),
            pid: Some(42),
            assets_dir: "/tmp/unexecuted-background-assets".into(),
            timeout_ms: 1_000,
            mode: NativeCaptureMode::Watch,
            text_policy: None,
            table_scope: None,
            background_target: Some(BackgroundNativeTarget {
                window_id: "window-7".into(),
                process,
            }),
            owned_document_bootstrap: None,
        }
    }

    fn observation(header: &Header) -> Observation {
        let app = AppIdentity {
            bundle_id: "test.app".into(),
            name: "Test".into(),
            pid: 42,
        };
        let process = KernelProcessIdentity {
            pid: 42,
            start_seconds: 1_700_000_000,
            start_micros: 123_456,
        };
        let window_object = UiObject {
            instance_ref: ObjectRef::new("background-window").unwrap(),
            parent: None,
            window_id: "window-7".into(),
            role: "AXWindow".into(),
            subrole: Some("AXStandardWindow".into()),
            title: Some("Test".into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: Rect {
                x: 10.0,
                y: 20.0,
                width: 400.0,
                height: 300.0,
            },
            actions: vec![],
            ax_action_names: Some(vec![]),
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::ReadScoped,
        };
        let observer_coverage =
            ObserverCoverage::for_projected_objects(std::slice::from_ref(&window_object));
        Observation {
            permissions: NativePermissions {
                accessibility: true,
                screen_capture: false,
            },
            structure: NativeStructure {
                app,
                global_focus: Some(crate::native::unavailable_global_focus()),
                background_window_context: Some(NativeBackgroundWindowContext {
                    window: SourceWindowContext {
                        source_session_id: header.stream_id.clone(),
                        session_epoch: 3,
                        process,
                        origin_window_id: "window-7".into(),
                        origin_window_frame: Rect {
                            x: 10.0,
                            y: 20.0,
                            width: 400.0,
                            height: 300.0,
                        },
                        verified_source_sequence: header.sequence,
                        verified_observed_ms: header.received_unix_ms,
                    },
                    hidden: ObservationFact::Known { value: false },
                    minimized: ObservationFact::Known { value: false },
                }),
                identity_boundary: None,
                owned_document_evidence: None,
                observer_coverage,
                windows: vec![WindowInfo {
                    window_id: "window-7".into(),
                    title: Some("Test".into()),
                    frame: Rect {
                        x: 10.0,
                        y: 20.0,
                        width: 400.0,
                        height: 300.0,
                    },
                    z_index: 4,
                    kind: LayerKind::Window,
                    modal: false,
                    owner_bundle_id: "test.app".into(),
                }],
                active_layer: None,
                complete: true,
                incomplete_reason: None,
                objects: vec![window_object],
                menu_anchors: vec![],
                menu_context: None,
                focus_context: None,
                window_context: None,
                table_viewports: vec![],
                table_scope_search: None,
            },
            structure_started_uptime_ns: 1_900_000_000,
            structure_finished_uptime_ns: header.received_uptime_ns,
            structure_started_unix_ms: Some(19_900),
            structure_finished_unix_ms: Some(header.received_unix_ms),
            frame_ref: None,
            tree_revision: 1,
            observer_revision: 1,
            observer_available: true,
            _observer_reason: None,
            ocr: NativeOcr {
                complete: false,
                reason: Some("not captured".into()),
                blocks: vec![],
            },
            errors: vec![],
        }
    }

    #[test]
    fn complete_visible_context_uses_actual_birth_and_source_computed_hash() {
        let header = header();
        let observation = observation(&header);
        let (target, process) =
            map_background_target_context(&config(None), 3, &header, &observation, 19_999).unwrap();
        let Some(TargetContext::NativeWindow { app, source }) = target else {
            panic!("expected native target context");
        };
        assert_eq!(process.unwrap().start_micros, 123_456);
        assert_eq!(source.window.verified_observed_ms, 19_999);
        assert_eq!(
            source.target_context_hash,
            source.computed_target_context_hash(&app).unwrap()
        );
    }

    #[test]
    fn background_context_rejects_wrong_birth_window_visibility_and_partial_tree() {
        let header = header();
        let good = observation(&header);
        let pinned = config(Some(KernelProcessIdentity {
            pid: 42,
            start_seconds: 1_700_000_000,
            start_micros: 123_456,
        }));
        assert!(map_background_target_context(&pinned, 3, &header, &good, 19_999).is_ok());

        let mut wrong_birth = good.clone();
        wrong_birth
            .structure
            .background_window_context
            .as_mut()
            .unwrap()
            .window
            .process
            .start_seconds += 1;
        assert!(map_background_target_context(&pinned, 3, &header, &wrong_birth, 19_999).is_err());
        let mut wrong_window = good.clone();
        wrong_window
            .structure
            .background_window_context
            .as_mut()
            .unwrap()
            .window
            .origin_window_id = "window-8".into();
        assert!(
            map_background_target_context(&config(None), 3, &header, &wrong_window, 19_999)
                .is_err()
        );
        let mut minimized = good.clone();
        minimized
            .structure
            .background_window_context
            .as_mut()
            .unwrap()
            .minimized = ObservationFact::Known { value: true };
        assert!(
            map_background_target_context(&config(None), 3, &header, &minimized, 19_999).is_err()
        );
        let mut incomplete = good;
        incomplete.structure.complete = false;
        assert!(
            map_background_target_context(&config(None), 3, &header, &incomplete, 19_999).is_err()
        );
    }

    #[test]
    fn unrequested_raw_background_facts_are_rejected() {
        let header = header();
        let observation = observation(&header);
        assert!(
            map_background_target_context(
                &NativeConfig {
                    background_target: None,
                    ..config(None)
                },
                3,
                &header,
                &observation,
                19_999
            )
            .is_err()
        );
    }

    #[test]
    fn background_target_requires_complete_raw_action_names_per_projected_object() {
        let header = header();
        let config = config(None);
        let mut observation = observation(&header);
        let mut no_projected_tree = observation.clone();
        no_projected_tree.structure.objects.clear();
        assert!(
            map_background_target_context(&config, 3, &header, &no_projected_tree, 19_999).is_err()
        );
        let button_index = observation.structure.objects.len();
        observation.structure.objects.push(UiObject {
            instance_ref: ObjectRef::new("background-button").unwrap(),
            parent: None,
            window_id: "window-7".into(),
            role: "AXButton".into(),
            subrole: None,
            title: Some("Continue".into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: Rect {
                x: 20.0,
                y: 30.0,
                width: 90.0,
                height: 24.0,
            },
            actions: vec![AxActionName::Press],
            ax_action_names: Some(vec!["AXPress".into()]),
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::ReadScoped,
        });
        assert!(map_background_target_context(&config, 3, &header, &observation, 19_999).is_ok());

        observation.structure.objects[button_index].ax_action_names = None;
        assert!(map_background_target_context(&config, 3, &header, &observation, 19_999).is_err());
        observation.structure.objects[button_index].ax_action_names =
            Some(vec!["AXPress".into(), "AXPress".into()]);
        assert!(map_background_target_context(&config, 3, &header, &observation, 19_999).is_err());
        observation.structure.objects[button_index].ax_action_names =
            Some(vec![String::from_utf8(vec![b'a'; 65]).unwrap()]);
        assert!(map_background_target_context(&config, 3, &header, &observation, 19_999).is_err());
        observation.structure.objects[button_index].ax_action_names =
            Some((0..33).map(|index| format!("AXAction{index}")).collect());
        assert!(map_background_target_context(&config, 3, &header, &observation, 19_999).is_err());
        observation.structure.objects[button_index].ax_action_names =
            Some(vec!["AX\nPress".into()]);
        assert!(map_background_target_context(&config, 3, &header, &observation, 19_999).is_err());
    }
}

fn rect_valid(rect: Rect) -> bool {
    [rect.x, rect.y, rect.width, rect.height]
        .into_iter()
        .all(f64::is_finite)
        && rect.width >= 0.0
        && rect.height >= 0.0
}

#[cfg(test)]
mod window_context_tests {
    use super::*;

    fn tracker(assets: &Path) -> WatchTracker {
        WatchTracker::new(
            NativeConfig {
                helper_path: "/tmp/unexecuted-window-test-helper".into(),
                bundle_id: "com.lingxi.computer-user.guard-fixture".into(),
                pid: Some(42),
                assets_dir: assets.into(),
                timeout_ms: 1000,
                mode: crate::config::NativeCaptureMode::Watch,
                table_scope: None,
                background_target: None,
                text_policy: Some(TextReadPolicy::RegisteredEditor {
                    window_id: "window-1".into(),
                }),
                owned_document_bootstrap: None,
            },
            7,
            "macos/7/window-test".into(),
            1_000_000,
            100,
        )
    }

    fn record(sequence: u64, callback_ms: u64) -> Value {
        let app = "com.lingxi.computer-user.guard-fixture";
        let frame = json!({"x":20.0,"y":30.0,"width":160.0,"height":100.0});
        json!({
            "protocol_version":1,"kind":"observation","stream_id":"window-stream",
            "sequence":sequence,"received_uptime_ns":10_000_000_000_u64+callback_ms*1_000_000,
            "received_unix_ms":1_000_000+callback_ms,
            "permissions":{"accessibility":true,"screen_capture":true},
            "structure":{"app":{"bundle_id":app,"name":"Fixture","pid":42},
                "identity_boundary":{"namespace":"11111111-1111-4111-8111-111111111111","generation":1},
                "windows":[{"window_id":"window-1","title":"Fixture","frame":frame,
                    "z_index":0,"kind":"window","modal":false,"owner_bundle_id":app}],
                "global_focus":{"app":{"state":"known","value":{"bundle_id":app,"name":"Fixture","pid":42}},
                    "window_id":{"state":"known","value":"window-1"}},
                "active_layer":"window-1","complete":true,"incomplete_reason":null,
                "objects":[{"instance_ref":"current-root","identity_continuity":"read_scoped","parent":null,"window_id":"window-1",
                    "role":"AXWindow","subrole":"AXStandardWindow","title":"Fixture",
                    "description":null,"value":null,"enabled":true,"focused":false,"frame":frame,
                    "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null}],
                "observer_coverage":{"state":"unavailable","covered_objects":0,
                    "read_scoped_objects":1,"failed_registrations":0,"failures":[]},
                "menu_anchors":[],"menu_context":null,"focus_context":null,
                "window_context":{"source_session_id":"actual-window-session","session_epoch":7,
                    "process":{"pid":42,"start_seconds":100,"start_micros":200},
                    "origin_window_id":"window-1","origin_window_frame":frame,
                    "verified_source_sequence":sequence,"verified_observed_ms":1_000_000+callback_ms}},
            "structure_started_uptime_ns":10_000_000_000_u64+(callback_ms-25)*1_000_000,
            "structure_finished_uptime_ns":10_000_000_000_u64+callback_ms*1_000_000,
            "structure_started_unix_ms":1_000_000_u64+callback_ms-25,
            "structure_finished_unix_ms":1_000_000_u64+callback_ms,
            "frame_ref":null,"tree_revision":sequence,"observer_revision":sequence,
            "observer_available":false,"observer_reason":"direct readonly observation",
            "ocr":{"complete":false,"reason":"no pixel claim","blocks":[]},"errors":[]
        })
    }

    #[test]
    fn window_context_accepts_focus_none_and_normalizes_only_verified_publication() {
        let assets = tempfile::tempdir().unwrap();
        let mut snapshot = tracker(assets.path())
            .ingest(record(1, 100), assets.path(), 1_000_150)
            .unwrap()
            .snapshot
            .unwrap();
        assert!(snapshot.focus_context.is_none());
        assert_eq!(snapshot.observed_ms, 75);
        assert_eq!(
            snapshot
                .window_context
                .as_ref()
                .unwrap()
                .verified_observed_ms,
            75
        );
        crate::native::normalize_snapshot_provenance(&mut snapshot, 99);
        let context = snapshot.window_context.as_ref().unwrap();
        assert_eq!(context.verified_source_sequence, 99);
        assert_eq!(context.verified_observed_ms, snapshot.observed_ms);
        assert_eq!(context.session_epoch, 7);
        snapshot.validate().unwrap();
    }

    #[test]
    fn window_context_rejects_forged_raw_scope_privacy_and_root_facts() {
        let assets = tempfile::tempdir().unwrap();
        for change in [
            "sequence",
            "time",
            "epoch",
            "pid",
            "window",
            "frame",
            "partial",
            "secure",
            "modal",
            "root",
            "global_focus",
        ] {
            let mut value = record(1, 100);
            match change {
                "sequence" => {
                    value["structure"]["window_context"]["verified_source_sequence"] = json!(0)
                }
                "time" => value["structure"]["window_context"]["verified_observed_ms"] = json!(0),
                "epoch" => value["structure"]["window_context"]["session_epoch"] = json!(8),
                "pid" => value["structure"]["window_context"]["process"]["pid"] = json!(43),
                "window" => {
                    value["structure"]["window_context"]["origin_window_id"] = json!("window-2")
                }
                "frame" => {
                    value["structure"]["window_context"]["origin_window_frame"]["x"] = json!(21.0)
                }
                "partial" => value["structure"]["complete"] = json!(false),
                "secure" => value["structure"]["objects"][0]["secure"] = json!(true),
                "modal" => value["structure"]["windows"][0]["modal"] = json!(true),
                "root" => value["structure"]["objects"][0]["subrole"] = json!("AXDialog"),
                "global_focus" => {
                    value["structure"]["global_focus"]["window_id"] =
                        json!({"state": "unavailable"})
                }
                _ => unreachable!(),
            }
            assert!(
                tracker(assets.path())
                    .ingest(value, assets.path(), 1_000_150)
                    .is_err(),
                "{change}"
            );
        }
    }

    #[test]
    fn window_context_pins_source_session_and_actual_kernel_across_focus_none_reads() {
        let assets = tempfile::tempdir().unwrap();
        for change in ["session", "kernel"] {
            let mut tracker = tracker(assets.path());
            tracker
                .ingest(record(1, 100), assets.path(), 1_000_150)
                .unwrap();
            let mut next = record(2, 200);
            if change == "session" {
                next["structure"]["window_context"]["source_session_id"] =
                    json!("replacement-session");
            } else {
                next["structure"]["window_context"]["process"]["start_seconds"] = json!(101);
            }
            assert!(
                tracker.ingest(next, assets.path(), 1_000_250).is_err(),
                "{change}"
            );
        }
    }

    #[test]
    fn window_context_requires_explicit_trusted_fixed_watch_policy() {
        let assets = tempfile::tempdir().unwrap();
        let mut tracker = tracker(assets.path());
        tracker.config.text_policy = None;
        assert!(
            tracker
                .ingest(record(1, 100), assets.path(), 1_000_150)
                .is_err()
        );
    }

    #[test]
    fn registered_mvp_fixture_window_context_uses_the_configured_policy_list() {
        let assets = tempfile::tempdir().unwrap();
        for bundle in [
            "com.lingxi.computer-user.mvp-fixture",
            "unregistered.example",
        ] {
            let mut tracker = tracker(assets.path());
            tracker.config.bundle_id = bundle.into();
            let value: Value = serde_json::from_str(
                &record(1, 100)
                    .to_string()
                    .replace("com.lingxi.computer-user.guard-fixture", bundle),
            )
            .unwrap();
            let result = tracker.ingest(value, assets.path(), 1_000_150);
            assert_eq!(result.is_ok(), bundle != "unregistered.example", "{bundle}");
            if let Ok(report) = result {
                assert!(report.snapshot.unwrap().window_context.is_some());
            }
        }
    }
}

#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tests;
