//! Provider-neutral computer-use proposals and frame requests.
//!
//! These types describe model output as data. A completed proposal only means
//! generation finished; it grants no permission to submit input to a Device.
//! Host adapters keep provider call IDs and safety-check state in their own
//! ledger and must independently authorize each resulting action.
//!
//! JSON Schema describes the shape and constraints expressible in the wire
//! format. Every consumer must still run the complete runtime validator
//! (`validate()` in Rust or the TypeScript parity validator): they enforce
//! UTF-8 byte limits, JavaScript-safe integer bounds, finite numbers, and
//! complete frame provenance that the shared `FrameMeta` schema cannot encode.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ids::{MAX_ID_LEN, RequestId, RootTaskId};
use super::source::{FrameMeta, FrameProvenance, FrameStatus, SingleCaptureFrameInfo};

pub const MAX_COMPUTER_ACTIONS: usize = 128;
pub const MAX_COMPUTER_DRAG_POINTS: usize = 256;
pub const MAX_COMPUTER_KEYS: usize = 16;
pub const MAX_COMPUTER_KEY_BYTES: usize = 64;
pub const MAX_COMPUTER_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_COMPUTER_COORDINATE_ABS: f64 = 1_000_000.0;
/// Largest integer that a JavaScript `number` can represent exactly.
pub const MAX_COMPUTER_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

const COMPUTER_NONBLANK_SAFE_STRING_PATTERN: &str = r"^[^\u0000-\u001F\u007F-\u009F]*[^\u0009-\u000D\u0020\u0085\u00A0\u1680\u2000-\u200A\u2028\u2029\u202F\u205F\u3000\u0000-\u001F\u007F-\u009F][^\u0000-\u001F\u007F-\u009F]*$";

/// The caller-owned snapshot and task lifetime a computer-use request is tied to.
///
/// A model proposal must never supply or alter this fence. Expiry and current
/// root/task authorization are checked by the host at use time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerFence {
    #[schemars(pattern(COMPUTER_NONBLANK_SAFE_STRING_PATTERN))]
    pub request_id: RequestId,
    #[schemars(range(min = 1, max = MAX_COMPUTER_SAFE_INTEGER))]
    pub generation: u64,
    #[schemars(pattern(COMPUTER_NONBLANK_SAFE_STRING_PATTERN))]
    pub root_task_id: RootTaskId,
    #[schemars(range(min = 1, max = MAX_COMPUTER_SAFE_INTEGER))]
    pub task_revision: u64,
    #[schemars(range(min = 1, max = MAX_COMPUTER_SAFE_INTEGER))]
    pub root_epoch: u64,
    #[schemars(length(min = 1, max = 256))]
    #[schemars(pattern(COMPUTER_NONBLANK_SAFE_STRING_PATTERN))]
    pub snapshot_id: String,
    #[schemars(range(min = 1, max = MAX_COMPUTER_SAFE_INTEGER))]
    pub structure_sequence: u64,
    #[schemars(range(min = 1, max = MAX_COMPUTER_SAFE_INTEGER))]
    pub deadline_ms: u64,
}

impl ComputerFence {
    pub fn validate(&self) -> Result<(), String> {
        validate_identifier("request_id", self.request_id.as_str())?;
        validate_identifier("root_task_id", self.root_task_id.as_str())?;
        validate_identifier("snapshot_id", &self.snapshot_id)?;
        for (field, value) in [
            ("generation", self.generation),
            ("task_revision", self.task_revision),
            ("root_epoch", self.root_epoch),
            ("structure_sequence", self.structure_sequence),
            ("deadline_ms", self.deadline_ms),
        ] {
            if value == 0 {
                return Err(format!("computer fence {field} must be nonzero"));
            }
            validate_js_safe_integer(field, value)?;
        }
        Ok(())
    }
}

/// Bind an expected complete screenshot frame to one caller-owned fence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerFrameRequest {
    pub fence: ComputerFence,
    pub expected_frame: FrameMeta,
}

impl ComputerFrameRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.fence.validate()?;
        validate_complete_frame_identity(&self.expected_frame, &self.fence)
    }
}

fn validate_complete_frame_identity(
    frame: &FrameMeta,
    fence: &ComputerFence,
) -> Result<(), String> {
    // FrameMeta is shared with the desktop envelope and its general schema intentionally
    // keeps the source's full u64 range. This private channel rejects integer
    // metadata that JavaScript cannot preserve exactly instead of narrowing
    // the shared FrameMeta contract or silently rounding values in TS.
    for (field, value) in [
        ("session_epoch", frame.session_epoch),
        ("surface.epoch", frame.surface.epoch),
        ("source_sequence", frame.source_sequence),
        ("geometry_revision", frame.geometry_revision),
        ("source_ms", frame.source_ms),
        ("byte_length", frame.byte_length),
    ] {
        validate_js_safe_integer(field, value)?;
    }
    if frame.status != FrameStatus::Complete
        || frame.session_epoch == 0
        || frame.source_sequence == 0
        || frame.surface.epoch == 0
        || frame.geometry_revision == 0
    {
        return Err("computer frame must be complete with nonzero source identity".into());
    }
    for (field, value) in [
        ("frame_id", frame.frame_id.as_str()),
        ("surface.id", frame.surface.id.as_str()),
        ("clock_domain", frame.clock_domain.as_str()),
    ] {
        validate_identifier(field, value)?;
    }
    frame.validate_provenance()?;
    match frame
        .provenance
        .as_ref()
        .ok_or("computer frame requires typed source provenance")?
    {
        FrameProvenance::Stream { stream } => {
            for (field, value) in [
                ("stream.display_time_ticks", stream.display_time_ticks),
                ("stream.display_time_ns", stream.display_time_ns),
                ("stream.received_uptime_ns", stream.received_uptime_ns),
                ("stream.received_unix_ms", stream.received_unix_ms),
                ("stream.callback_ms", stream.callback_ms),
                ("stream.dropped_frames", stream.dropped_frames),
                ("stream.capture_interval_ms", stream.capture_interval_ms),
            ] {
                validate_js_safe_integer(field, value)?;
            }
            for (field, value) in [
                ("stream.stream_id", stream.stream_id.as_str()),
                ("stream.source_clock", stream.source_clock.as_str()),
                ("stream.window_id", stream.window_id.as_str()),
            ] {
                validate_identifier(field, value)?;
            }
            let image_frame_id = stream
                .image_frame_id
                .as_deref()
                .ok_or("complete computer frame requires an image identity")?;
            validate_identifier("stream.image_frame_id", image_frame_id)?;
            if stream.pid == 0 {
                return Err("complete computer frame requires a nonzero source pid".into());
            }
        }
        FrameProvenance::SingleCapture { capture } => {
            validate_single_capture_frame(frame, capture, fence)?;
        }
    }
    Ok(())
}

fn validate_single_capture_frame(
    frame: &FrameMeta,
    capture: &SingleCaptureFrameInfo,
    fence: &ComputerFence,
) -> Result<(), String> {
    capture.validate()?;
    if capture.request_id != fence.request_id || capture.deadline_ms != fence.deadline_ms {
        return Err(
            "single capture request and original deadline must match the Computer fence".into(),
        );
    }
    if frame.source_ms != capture.capture_started_ms {
        return Err(
            "single capture FrameMeta source_ms must be its conservative start bound".into(),
        );
    }
    let geometry = frame
        .window_geometry()
        .ok_or("single capture frame has no window geometry")?;
    if geometry.pid != capture.pid
        || geometry.window_id != capture.window_id
        || geometry.window_frame != capture.window_frame
        || geometry.content_rect != capture.content_rect
        || geometry.content_scale != capture.content_scale
    {
        return Err("single capture frame geometry differs from its capture provenance".into());
    }
    if capture.content_rect.x != 0.0
        || capture.content_rect.y != 0.0
        || capture.window_frame.width <= 0.0
        || capture.window_frame.height <= 0.0
        || capture.content_rect.width <= 0.0
        || capture.content_rect.height <= 0.0
        || !frame.scale_factor.is_finite()
        || frame.scale_factor <= 0.0
        || !within_pixel_tolerance(
            capture.content_rect.width * frame.scale_factor,
            frame.width_px,
        )
        || !within_pixel_tolerance(
            capture.content_rect.height * frame.scale_factor,
            frame.height_px,
        )
        || !within_pixel_tolerance(
            capture.window_frame.width * frame.scale_factor * capture.content_scale,
            frame.width_px,
        )
        || !within_pixel_tolerance(
            capture.window_frame.height * frame.scale_factor * capture.content_scale,
            frame.height_px,
        )
        || !within_pixel_tolerance(
            (capture.window_frame.width * capture.content_scale - capture.content_rect.width)
                * frame.scale_factor,
            0,
        )
        || !within_pixel_tolerance(
            (capture.window_frame.height * capture.content_scale - capture.content_rect.height)
                * frame.scale_factor,
            0,
        )
    {
        return Err("single capture is not a full-window image transform".into());
    }
    Ok(())
}

fn within_pixel_tolerance(value: f64, pixels: u32) -> bool {
    value.is_finite() && (value - f64::from(pixels)).abs() <= 1.0
}

/// Mouse buttons available to provider-neutral computer-use proposals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComputerMouseButton {
    Left,
    Right,
    Wheel,
    Back,
    Forward,
}

/// A point in an ordered drag path, in the frame coordinate space supplied to
/// the compiler. Bounds here are numeric only; image/window hit-testing is a
/// separate host-side compilation check.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerPoint {
    #[schemars(range(min = -1000000.0, max = 1000000.0))]
    pub x: f64,
    #[schemars(range(min = -1000000.0, max = 1000000.0))]
    pub y: f64,
}

impl ComputerPoint {
    pub fn validate(&self) -> Result<(), String> {
        validate_coordinate("x", self.x)?;
        validate_coordinate("y", self.y)
    }
}

/// One ordered, bounded action proposed by a model. This is not a device
/// command and includes no provider-owned call or safety-check metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerActionProposal {
    Click {
        button: ComputerMouseButton,
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        x: f64,
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 16))]
        #[schemars(inner(
            length(min = 1, max = 64),
            pattern(COMPUTER_NONBLANK_SAFE_STRING_PATTERN)
        ))]
        keys: Option<Vec<String>>,
    },
    DoubleClick {
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        x: f64,
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 16))]
        #[schemars(inner(
            length(min = 1, max = 64),
            pattern(COMPUTER_NONBLANK_SAFE_STRING_PATTERN)
        ))]
        keys: Option<Vec<String>>,
    },
    Drag {
        #[schemars(length(min = 2, max = 256))]
        path: Vec<ComputerPoint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 16))]
        #[schemars(inner(
            length(min = 1, max = 64),
            pattern(COMPUTER_NONBLANK_SAFE_STRING_PATTERN)
        ))]
        keys: Option<Vec<String>>,
    },
    Move {
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        x: f64,
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 16))]
        #[schemars(inner(
            length(min = 1, max = 64),
            pattern(COMPUTER_NONBLANK_SAFE_STRING_PATTERN)
        ))]
        keys: Option<Vec<String>>,
    },
    Scroll {
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        scroll_x: f64,
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        scroll_y: f64,
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        x: f64,
        #[schemars(range(min = -1000000.0, max = 1000000.0))]
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 16))]
        #[schemars(inner(
            length(min = 1, max = 64),
            pattern(COMPUTER_NONBLANK_SAFE_STRING_PATTERN)
        ))]
        keys: Option<Vec<String>>,
    },
    Keypress {
        #[schemars(length(min = 1, max = 16))]
        #[schemars(inner(
            length(min = 1, max = 64),
            pattern(COMPUTER_NONBLANK_SAFE_STRING_PATTERN)
        ))]
        keys: Vec<String>,
    },
    Type {
        #[schemars(length(max = 65536))]
        #[schemars(pattern(r"^[^\u0000]*$"))]
        text: String,
    },
    Wait,
    Screenshot,
}

impl ComputerActionProposal {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Click { x, y, keys, .. }
            | Self::DoubleClick { x, y, keys }
            | Self::Move { x, y, keys } => {
                validate_coordinate("x", *x)?;
                validate_coordinate("y", *y)?;
                validate_optional_keys(keys.as_deref())
            }
            Self::Drag { path, keys } => {
                if !(2..=MAX_COMPUTER_DRAG_POINTS).contains(&path.len()) {
                    return Err(format!(
                        "computer drag path must contain 2..={MAX_COMPUTER_DRAG_POINTS} points"
                    ));
                }
                for point in path {
                    point.validate()?;
                }
                validate_optional_keys(keys.as_deref())
            }
            Self::Scroll {
                scroll_x,
                scroll_y,
                x,
                y,
                keys,
            } => {
                validate_coordinate("scroll_x", *scroll_x)?;
                validate_coordinate("scroll_y", *scroll_y)?;
                validate_coordinate("x", *x)?;
                validate_coordinate("y", *y)?;
                validate_optional_keys(keys.as_deref())
            }
            Self::Keypress { keys } => validate_keys(keys, false),
            Self::Type { text } => {
                if text.len() > MAX_COMPUTER_TEXT_BYTES || text.contains('\0') {
                    return Err(format!(
                        "computer type text must be at most {MAX_COMPUTER_TEXT_BYTES} bytes and contain no NUL"
                    ));
                }
                Ok(())
            }
            Self::Wait | Self::Screenshot => Ok(()),
        }
    }
}

/// An ordered model-generated action batch bound to the caller's fence.
///
/// This is only a completed proposal. Root permission, Device capability,
/// current frame freshness and input authorization remain separate checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerBatchProposal {
    pub fence: ComputerFence,
    #[schemars(length(min = 1, max = 128))]
    pub actions: Vec<ComputerActionProposal>,
}

impl ComputerBatchProposal {
    pub fn validate(&self) -> Result<(), String> {
        self.fence.validate()?;
        if self.actions.is_empty() || self.actions.len() > MAX_COMPUTER_ACTIONS {
            return Err(format!(
                "computer batch must contain 1..={MAX_COMPUTER_ACTIONS} actions"
            ));
        }
        for (index, action) in self.actions.iter().enumerate() {
            action
                .validate()
                .map_err(|error| format!("computer action {index}: {error}"))?;
        }
        Ok(())
    }
}

fn validate_optional_keys(keys: Option<&[String]>) -> Result<(), String> {
    if let Some(keys) = keys {
        validate_keys(keys, true)?;
    }
    Ok(())
}

fn validate_keys(keys: &[String], allow_empty: bool) -> Result<(), String> {
    if (!allow_empty && keys.is_empty()) || keys.len() > MAX_COMPUTER_KEYS {
        return Err(format!(
            "computer key list must contain {}..={MAX_COMPUTER_KEYS} entries",
            if allow_empty { 0 } else { 1 }
        ));
    }
    for key in keys {
        if key.trim().is_empty()
            || key.len() > MAX_COMPUTER_KEY_BYTES
            || key.chars().any(char::is_control)
        {
            return Err(format!(
                "computer key names must be nonblank, control-free, and at most {MAX_COMPUTER_KEY_BYTES} bytes"
            ));
        }
    }
    Ok(())
}

fn validate_coordinate(name: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() || value.abs() > MAX_COMPUTER_COORDINATE_ABS {
        return Err(format!(
            "computer {name} must be finite and within +/-{MAX_COMPUTER_COORDINATE_ABS}"
        ));
    }
    Ok(())
}

fn validate_js_safe_integer(name: &str, value: u64) -> Result<(), String> {
    if value > MAX_COMPUTER_SAFE_INTEGER {
        return Err(format!(
            "computer {name} exceeds JavaScript's exact integer range"
        ));
    }
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAX_ID_LEN || value.chars().any(char::is_control) {
        return Err(format!(
            "computer {field} must be nonblank, control-free, and at most {MAX_ID_LEN} bytes"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::device::SurfaceRef;
    use crate::desktop::source::{FrameProvenance, FrameStreamInfo, Rect};

    fn fence() -> ComputerFence {
        ComputerFence {
            request_id: RequestId::new("computer-request-1").unwrap(),
            generation: 1,
            root_task_id: RootTaskId::new("root-task-1").unwrap(),
            task_revision: 2,
            root_epoch: 3,
            snapshot_id: "snapshot-4".into(),
            structure_sequence: 5,
            deadline_ms: 10_000,
        }
    }

    fn frame(status: FrameStatus) -> FrameMeta {
        FrameMeta {
            frame_id: "stream-a-event-8".into(),
            session_epoch: 7,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 7,
            },
            clock_domain: "macos/7/wall".into(),
            source_sequence: 8,
            source_ms: 100,
            status,
            geometry_revision: 1,
            width_px: 1200,
            height_px: 800,
            scale_factor: 2.0,
            byte_length: 0,
            checksum: String::new(),
            provenance: Some(FrameProvenance::Stream {
                stream: FrameStreamInfo {
                    stream_id: "stream-a".into(),
                    image_frame_id: Some("native-image-6".into()),
                    display_time_ticks: 1,
                    display_time_ns: 1,
                    received_uptime_ns: 2,
                    received_unix_ms: 100,
                    callback_ms: 101,
                    source_clock: "mach_absolute".into(),
                    window_id: "window-2".into(),
                    pid: 42,
                    window_frame: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 600.0,
                        height: 400.0,
                    },
                    content_rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 1200.0,
                        height: 800.0,
                    },
                    content_scale: 2.0,
                    dirty_rects: vec![],
                    dropped_frames: 0,
                    capture_interval_ms: 100,
                },
            }),
        }
    }

    fn stream_info_mut(frame: &mut FrameMeta) -> &mut FrameStreamInfo {
        match frame.provenance.as_mut() {
            Some(FrameProvenance::Stream { stream }) => stream,
            _ => panic!("test frame must carry stream provenance"),
        }
    }

    fn capture_info_mut(frame: &mut FrameMeta) -> &mut SingleCaptureFrameInfo {
        match frame.provenance.as_mut() {
            Some(FrameProvenance::SingleCapture { capture }) => capture,
            _ => panic!("test frame must carry single-capture provenance"),
        }
    }

    fn single_capture_frame(status: FrameStatus) -> FrameMeta {
        let mut frame = frame(status);
        frame.frame_id = "capture-event-9".into();
        frame.source_ms = 200;
        frame.width_px = 1160;
        frame.height_px = 768;
        frame.scale_factor = 2.0;
        frame.provenance = Some(FrameProvenance::SingleCapture {
            capture: SingleCaptureFrameInfo {
                watch_id: "stream-a".into(),
                request_id: fence().request_id,
                image_frame_id: "png-content-9".into(),
                pid: 42,
                window_id: "window-2".into(),
                window_frame: Rect {
                    x: 20.0,
                    y: 30.0,
                    width: 1160.0,
                    height: 768.0,
                },
                content_rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 580.0,
                    height: 384.0,
                },
                content_scale: 0.5,
                source_clock: "mach_absolute".into(),
                capture_started_uptime_ns: 1_000,
                capture_finished_uptime_ns: 1_250,
                capture_started_ms: 200,
                capture_finished_ms: 210,
                clock_uncertainty_ms: 10,
                requested_after_ms: 190,
                deadline_ms: 10_000,
            },
        });
        frame
    }

    fn pointer_and_keypress_actions(key: &str) -> [ComputerActionProposal; 6] {
        let keys = || Some(vec![key.to_owned()]);
        [
            ComputerActionProposal::Click {
                button: ComputerMouseButton::Left,
                x: 1.0,
                y: 2.0,
                keys: keys(),
            },
            ComputerActionProposal::DoubleClick {
                x: 1.0,
                y: 2.0,
                keys: keys(),
            },
            ComputerActionProposal::Drag {
                path: vec![
                    ComputerPoint { x: 1.0, y: 2.0 },
                    ComputerPoint { x: 3.0, y: 4.0 },
                ],
                keys: keys(),
            },
            ComputerActionProposal::Move {
                x: 1.0,
                y: 2.0,
                keys: keys(),
            },
            ComputerActionProposal::Scroll {
                scroll_x: 0.0,
                scroll_y: 1.0,
                x: 1.0,
                y: 2.0,
                keys: keys(),
            },
            ComputerActionProposal::Keypress {
                keys: vec![key.to_owned()],
            },
        ]
    }

    #[test]
    fn complete_frame_request_accepts_distinct_event_and_image_ids() {
        let request = ComputerFrameRequest {
            fence: fence(),
            expected_frame: frame(FrameStatus::Complete),
        };
        assert_ne!(
            request.expected_frame.frame_id.as_str(),
            request.expected_frame.image_frame_id().unwrap()
        );
        assert!(request.validate().is_ok());
        let geometry = request.expected_frame.window_geometry().unwrap();
        assert_eq!(geometry.window_id, "window-2");
        assert_eq!(geometry.pid, 42);
        assert_eq!(geometry.window_frame.width, 600.0);
        assert_eq!(geometry.content_rect.width, 1200.0);
        assert_eq!(geometry.content_scale, 2.0);
    }

    #[test]
    fn frame_provenance_uses_only_the_new_tagged_wire_union() {
        let stream = serde_json::to_value(frame(FrameStatus::Complete)).unwrap();
        assert_eq!(stream["provenance"]["kind"], "stream");
        assert!(stream.get("stream").is_none());

        let capture = serde_json::to_value(single_capture_frame(FrameStatus::Complete)).unwrap();
        assert_eq!(capture["provenance"]["kind"], "single_capture");
        let decoded: FrameMeta = serde_json::from_value(capture.clone()).unwrap();
        let geometry = decoded.window_geometry().unwrap();
        assert_eq!(geometry.window_id, "window-2");
        assert_eq!(geometry.window_frame.x, 20.0);
        assert_eq!(geometry.window_frame.width, 1160.0);
        assert_eq!(geometry.content_rect.width, 580.0);
        assert!(decoded.stream_info().is_none());
        assert!(decoded.capture_info().is_some());

        let mut legacy = stream;
        let provenance = legacy
            .as_object_mut()
            .unwrap()
            .remove("provenance")
            .unwrap();
        legacy["stream"] = provenance["stream"].clone();
        assert!(serde_json::from_value::<FrameMeta>(legacy).is_err());
    }

    #[test]
    fn frame_request_requires_complete_stream_identity() {
        let mut request = ComputerFrameRequest {
            fence: fence(),
            expected_frame: frame(FrameStatus::Idle),
        };
        assert!(request.validate().is_err());
        request.expected_frame = frame(FrameStatus::Complete);
        let Some(FrameProvenance::Stream { stream }) = request.expected_frame.provenance.as_mut()
        else {
            unreachable!()
        };
        stream.image_frame_id = None;
        assert!(request.validate().is_err());
        request.expected_frame = frame(FrameStatus::Complete);
        request.expected_frame.provenance = None;
        assert!(request.validate().is_err());
    }

    #[test]
    fn single_capture_frame_request_requires_exact_fence_and_full_window_evidence() {
        let request = ComputerFrameRequest {
            fence: fence(),
            expected_frame: single_capture_frame(FrameStatus::Complete),
        };
        assert!(request.validate().is_ok());
        assert_eq!(
            request.expected_frame.image_frame_id(),
            Some("png-content-9")
        );
        assert_eq!(request.expected_frame.watch_id(), Some("stream-a"));
        assert_eq!(
            request
                .expected_frame
                .capture_info()
                .unwrap()
                .requested_after_ms,
            190
        );

        let mut invalid = request.clone();
        let Some(FrameProvenance::SingleCapture { capture }) =
            invalid.expected_frame.provenance.as_mut()
        else {
            unreachable!()
        };
        capture.request_id = RequestId::new("foreign-request").unwrap();
        assert!(invalid.validate().is_err());

        invalid = request.clone();
        let Some(FrameProvenance::SingleCapture { capture }) =
            invalid.expected_frame.provenance.as_mut()
        else {
            unreachable!()
        };
        capture.deadline_ms = request.fence.deadline_ms + 1;
        assert!(invalid.validate().is_err());

        let mutators: [fn(&mut SingleCaptureFrameInfo); 3] = [
            |capture: &mut SingleCaptureFrameInfo| capture.capture_started_ms = 190,
            |capture: &mut SingleCaptureFrameInfo| {
                capture.capture_finished_ms = capture.deadline_ms + 1
            },
            |capture: &mut SingleCaptureFrameInfo| capture.source_clock = "display_ticks".into(),
        ];
        for mutate in mutators {
            invalid = request.clone();
            let Some(FrameProvenance::SingleCapture { capture }) =
                invalid.expected_frame.provenance.as_mut()
            else {
                unreachable!()
            };
            mutate(capture);
            assert!(invalid.validate().is_err());
        }

        invalid = request.clone();
        invalid.expected_frame.source_ms += 1;
        assert!(invalid.validate().is_err());
        invalid = request.clone();
        invalid.expected_frame.status = FrameStatus::Idle;
        assert!(invalid.validate().is_err());
        invalid = request;
        let Some(FrameProvenance::SingleCapture { capture }) =
            invalid.expected_frame.provenance.as_mut()
        else {
            unreachable!()
        };
        capture.window_frame.width += 2.0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn single_capture_accepts_real_retina_downsample_and_rejects_bad_transforms() {
        let valid = ComputerFrameRequest {
            fence: fence(),
            expected_frame: single_capture_frame(FrameStatus::Complete),
        };
        assert_eq!(valid.expected_frame.width_px, 1160);
        assert_eq!(valid.expected_frame.height_px, 768);
        assert_eq!(valid.expected_frame.scale_factor, 2.0);
        assert_eq!(
            valid.expected_frame.capture_info().unwrap().content_scale,
            0.5
        );
        assert!(valid.validate().is_ok());

        let mutators: [fn(&mut FrameMeta); 8] = [
            |frame| capture_info_mut(frame).content_rect.x = 1.0,
            |frame| capture_info_mut(frame).content_rect.width -= 2.0,
            |frame| capture_info_mut(frame).content_scale = 0.75,
            |frame| frame.scale_factor = 1.0,
            |frame| capture_info_mut(frame).window_frame.width = 0.0,
            |frame| capture_info_mut(frame).content_rect.height = 0.0,
            |frame| capture_info_mut(frame).content_scale = f64::MAX,
            |frame| frame.scale_factor = f64::MAX,
        ];
        for mutate in mutators {
            let mut invalid = valid.clone();
            mutate(&mut invalid.expected_frame);
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn single_capture_provenance_rejects_unrepresentable_clock_values() {
        let request = ComputerFrameRequest {
            fence: fence(),
            expected_frame: single_capture_frame(FrameStatus::Complete),
        };
        let mutators: [fn(&mut SingleCaptureFrameInfo); 7] = [
            |capture| capture.capture_started_uptime_ns = MAX_COMPUTER_SAFE_INTEGER + 1,
            |capture| capture.capture_finished_uptime_ns = MAX_COMPUTER_SAFE_INTEGER + 1,
            |capture| capture.capture_started_ms = MAX_COMPUTER_SAFE_INTEGER + 1,
            |capture| capture.capture_finished_ms = MAX_COMPUTER_SAFE_INTEGER + 1,
            |capture| capture.clock_uncertainty_ms = MAX_COMPUTER_SAFE_INTEGER + 1,
            |capture| capture.requested_after_ms = MAX_COMPUTER_SAFE_INTEGER + 1,
            |capture| capture.deadline_ms = MAX_COMPUTER_SAFE_INTEGER + 1,
        ];
        for mutate in mutators {
            let mut invalid = request.clone();
            let Some(FrameProvenance::SingleCapture { capture }) =
                invalid.expected_frame.provenance.as_mut()
            else {
                unreachable!()
            };
            mutate(capture);
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn fence_requires_every_generation_sequence_and_deadline() {
        let mut value = fence();
        assert!(value.validate().is_ok());
        let zeroers: [fn(&mut ComputerFence); 5] = [
            |fence: &mut ComputerFence| fence.generation = 0,
            |fence: &mut ComputerFence| fence.task_revision = 0,
            |fence: &mut ComputerFence| fence.root_epoch = 0,
            |fence: &mut ComputerFence| fence.structure_sequence = 0,
            |fence: &mut ComputerFence| fence.deadline_ms = 0,
        ];
        for update in zeroers {
            let mut invalid = value.clone();
            update(&mut invalid);
            assert!(invalid.validate().is_err());
        }
        value.snapshot_id = " ".into();
        assert!(value.validate().is_err());
    }

    #[test]
    fn schema_and_runtime_enforce_javascript_safe_fence_integers() {
        let schema = serde_json::to_value(schemars::schema_for!(ComputerFence)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for field in [
            "generation",
            "task_revision",
            "root_epoch",
            "structure_sequence",
            "deadline_ms",
        ] {
            let mut at_limit = serde_json::to_value(fence()).unwrap();
            at_limit[field] = serde_json::json!(MAX_COMPUTER_SAFE_INTEGER);
            assert!(validator.is_valid(&at_limit), "rejected safe {field}");

            let mut over_limit = at_limit;
            over_limit[field] = serde_json::json!(MAX_COMPUTER_SAFE_INTEGER + 1);
            assert!(!validator.is_valid(&over_limit), "accepted unsafe {field}");

            let mut runtime_over_limit = fence();
            match field {
                "generation" => runtime_over_limit.generation = MAX_COMPUTER_SAFE_INTEGER + 1,
                "task_revision" => {
                    runtime_over_limit.task_revision = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                "root_epoch" => runtime_over_limit.root_epoch = MAX_COMPUTER_SAFE_INTEGER + 1,
                "structure_sequence" => {
                    runtime_over_limit.structure_sequence = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                "deadline_ms" => runtime_over_limit.deadline_ms = MAX_COMPUTER_SAFE_INTEGER + 1,
                _ => unreachable!(),
            }
            assert!(runtime_over_limit.validate().is_err());
        }
    }

    #[test]
    fn computer_schema_rejects_nul_blank_control_and_oversized_key_text() {
        let schema = serde_json::to_value(schemars::schema_for!(ComputerBatchProposal)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let value_for = |action| {
            serde_json::to_value(ComputerBatchProposal {
                fence: fence(),
                actions: vec![action],
            })
            .unwrap()
        };

        let valid = value_for(ComputerActionProposal::Click {
            button: ComputerMouseButton::Left,
            x: 1.0,
            y: 2.0,
            keys: Some(vec![]),
        });
        assert!(validator.is_valid(&valid));

        let mut nul_text = value_for(ComputerActionProposal::Type { text: "ok".into() });
        nul_text["actions"][0]["text"] = serde_json::json!("before\0after");
        assert!(!validator.is_valid(&nul_text));
        assert!(
            serde_json::from_value::<ComputerBatchProposal>(nul_text)
                .unwrap()
                .validate()
                .is_err()
        );

        for bad_snapshot in [" \t", "line\nbreak", "nul\0id"] {
            let mut bad = valid.clone();
            bad["fence"]["snapshot_id"] = serde_json::json!(bad_snapshot);
            assert!(
                !validator.is_valid(&bad),
                "accepted snapshot {bad_snapshot:?}"
            );
            assert!(
                serde_json::from_value::<ComputerBatchProposal>(bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }

        let too_long_key = "k".repeat(MAX_COMPUTER_KEY_BYTES + 1);
        for bad_key in [" \t", "line\nbreak", "nul\0key", too_long_key.as_str()] {
            for action in pointer_and_keypress_actions(bad_key) {
                let bad = value_for(action);
                assert!(!validator.is_valid(&bad), "accepted key {bad_key:?}");
                assert!(
                    serde_json::from_value::<ComputerBatchProposal>(bad)
                        .unwrap()
                        .validate()
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn utf8_byte_caps_and_frame_safe_integers_remain_runtime_checks() {
        let schema = serde_json::to_value(schemars::schema_for!(ComputerBatchProposal)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();

        let snapshot_over_bytes = "é".repeat(129);
        let mut long_snapshot = fence();
        long_snapshot.snapshot_id = snapshot_over_bytes;
        let snapshot_proposal = ComputerBatchProposal {
            fence: long_snapshot,
            actions: vec![ComputerActionProposal::Wait],
        };
        assert!(validator.is_valid(&serde_json::to_value(&snapshot_proposal).unwrap()));
        assert!(snapshot_proposal.validate().is_err());

        let long_key = "é".repeat(33);
        let key_proposal = ComputerBatchProposal {
            fence: fence(),
            actions: vec![ComputerActionProposal::Keypress {
                keys: vec![long_key],
            }],
        };
        assert!(validator.is_valid(&serde_json::to_value(&key_proposal).unwrap()));
        assert!(key_proposal.validate().is_err());

        let long_text = "é".repeat((MAX_COMPUTER_TEXT_BYTES / 2) + 1);
        let text_proposal = ComputerBatchProposal {
            fence: fence(),
            actions: vec![ComputerActionProposal::Type { text: long_text }],
        };
        assert!(validator.is_valid(&serde_json::to_value(&text_proposal).unwrap()));
        assert!(text_proposal.validate().is_err());

        let mut frame_request = ComputerFrameRequest {
            fence: fence(),
            expected_frame: frame(FrameStatus::Complete),
        };
        let integer_fields = [
            "session_epoch",
            "surface.epoch",
            "source_sequence",
            "geometry_revision",
            "source_ms",
            "byte_length",
            "stream.display_time_ticks",
            "stream.display_time_ns",
            "stream.received_uptime_ns",
            "stream.received_unix_ms",
            "stream.callback_ms",
            "stream.dropped_frames",
            "stream.capture_interval_ms",
        ];
        for field in integer_fields {
            frame_request.expected_frame = frame(FrameStatus::Complete);
            let frame = &mut frame_request.expected_frame;
            match field {
                "session_epoch" => frame.session_epoch = MAX_COMPUTER_SAFE_INTEGER + 1,
                "surface.epoch" => frame.surface.epoch = MAX_COMPUTER_SAFE_INTEGER + 1,
                "source_sequence" => frame.source_sequence = MAX_COMPUTER_SAFE_INTEGER + 1,
                "geometry_revision" => {
                    frame.geometry_revision = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                "source_ms" => frame.source_ms = MAX_COMPUTER_SAFE_INTEGER + 1,
                "byte_length" => frame.byte_length = MAX_COMPUTER_SAFE_INTEGER + 1,
                "stream.display_time_ticks" => {
                    stream_info_mut(frame).display_time_ticks = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                "stream.display_time_ns" => {
                    stream_info_mut(frame).display_time_ns = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                "stream.received_uptime_ns" => {
                    stream_info_mut(frame).received_uptime_ns = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                "stream.received_unix_ms" => {
                    stream_info_mut(frame).received_unix_ms = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                "stream.callback_ms" => {
                    stream_info_mut(frame).callback_ms = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                "stream.dropped_frames" => {
                    stream_info_mut(frame).dropped_frames = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                "stream.capture_interval_ms" => {
                    stream_info_mut(frame).capture_interval_ms = MAX_COMPUTER_SAFE_INTEGER + 1;
                }
                _ => unreachable!(),
            }
            assert!(frame_request.validate().is_err(), "accepted unsafe {field}");
        }
    }

    #[test]
    fn batch_bounds_all_actions_and_keeps_provider_metadata_out() {
        let batch = ComputerBatchProposal {
            fence: fence(),
            actions: vec![ComputerActionProposal::Click {
                button: ComputerMouseButton::Left,
                x: 12.0,
                y: 20.0,
                keys: Some(vec![]),
            }],
        };
        assert!(batch.validate().is_ok());
        let encoded = serde_json::to_value(&batch).unwrap();
        assert_eq!(encoded["actions"][0]["action"], "click");
        assert!(encoded.get("call_id").is_none());
        assert!(encoded.get("safety_checks").is_none());

        let empty = ComputerBatchProposal {
            fence: fence(),
            actions: vec![],
        };
        assert!(empty.validate().is_err());
        let too_many = ComputerBatchProposal {
            fence: fence(),
            actions: vec![ComputerActionProposal::Wait; MAX_COMPUTER_ACTIONS + 1],
        };
        assert!(too_many.validate().is_err());
    }

    #[test]
    fn each_action_has_bounded_coordinates_paths_keys_and_text() {
        for action in [
            ComputerActionProposal::Move {
                x: f64::NAN,
                y: 0.0,
                keys: None,
            },
            ComputerActionProposal::Scroll {
                scroll_x: 0.0,
                scroll_y: f64::INFINITY,
                x: 0.0,
                y: 0.0,
                keys: None,
            },
            ComputerActionProposal::Keypress { keys: vec![] },
            ComputerActionProposal::Drag {
                path: vec![ComputerPoint { x: 0.0, y: 0.0 }],
                keys: None,
            },
            ComputerActionProposal::Type {
                text: "x".repeat(MAX_COMPUTER_TEXT_BYTES + 1),
            },
        ] {
            assert!(action.validate().is_err());
        }

        assert!(ComputerActionProposal::Screenshot.validate().is_ok());
        assert!(ComputerActionProposal::Wait.validate().is_ok());
        assert!(
            ComputerActionProposal::Keypress {
                keys: vec!["CTRL".into(), "A".into()],
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn unknown_provider_fields_are_rejected_during_decode() {
        let mut encoded = serde_json::to_value(ComputerBatchProposal {
            fence: fence(),
            actions: vec![ComputerActionProposal::Screenshot],
        })
        .unwrap();
        encoded["call_id"] = serde_json::json!("provider-call-1");
        assert!(serde_json::from_value::<ComputerBatchProposal>(encoded).is_err());
    }
}
