//! Frozen evidence joining one host-issued computer frame request to the exact
//! Source structure snapshot that was current when that request was created.
//!
//! This value can only be minted by `StateCore::freeze_computer_observation`.
//! It is an in-memory capability for that original frame/snapshot pair, not a
//! replacement for current source health, binding, effect, or input checks.

use interaction_contracts::desktop::computer::ComputerFrameRequest;
use interaction_contracts::desktop::source::{
    FrameMeta, FrameStatus, LayerKind, StructureSnapshot, UiObject,
};

use super::state_core::EnvSnapshot;
use crate::environment::read::focused_window_id;

#[derive(Debug, Clone, PartialEq)]
pub struct FrozenComputerObservation {
    request: ComputerFrameRequest,
    snapshot: StructureSnapshot,
    environment_revision: u64,
    layer_revision: u64,
    page: Option<String>,
}

impl FrozenComputerObservation {
    pub fn request(&self) -> &ComputerFrameRequest {
        &self.request
    }

    pub fn snapshot(&self) -> &StructureSnapshot {
        &self.snapshot
    }

    pub const fn environment_revision(&self) -> u64 {
        self.environment_revision
    }

    pub const fn layer_revision(&self) -> u64 {
        self.layer_revision
    }

    pub fn page(&self) -> Option<&str> {
        self.page.as_deref()
    }

    /// Convert image pixels to global upper-left screen points. The factory
    /// accepts only a full-window, unpadded, uniformly scaled image.
    pub fn image_pixel_to_screen(&self, x: f64, y: f64) -> Result<(f64, f64), String> {
        let frame = &self.request.expected_frame;
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= f64::from(frame.width_px)
            || y >= f64::from(frame.height_px)
        {
            return Err("computer image point is outside the frozen pixel buffer".into());
        }
        let stream = frame
            .window_geometry()
            .ok_or("frozen computer frame has no stream geometry")?;
        let scale = frame.scale_factor * stream.content_scale;
        if !scale.is_finite() || scale <= 0.0 {
            return Err("frozen computer frame has an invalid pixel scale".into());
        }
        Ok((
            stream.window_frame.x + x / scale,
            stream.window_frame.y + y / scale,
        ))
    }
}

/// Called only by the Environment owner after it has checked current frame
/// health and matched `frame` to the latest complete image it admitted.
pub(super) fn freeze(
    request: ComputerFrameRequest,
    frame: &FrameMeta,
    env: &EnvSnapshot,
) -> Result<FrozenComputerObservation, String> {
    request.validate()?;
    if frame != &request.expected_frame {
        return Err("computer frame request does not match the latest admitted image".into());
    }
    validate_pair(&request, &env.snapshot)?;
    Ok(FrozenComputerObservation {
        request,
        snapshot: env.snapshot.clone(),
        environment_revision: env.revision,
        layer_revision: env.layer_revision,
        page: env.page.clone(),
    })
}

pub(super) fn validate_live_stream(
    frozen: &FrozenComputerObservation,
    current_frame: &FrameMeta,
) -> Result<(), String> {
    let original = &frozen.request.expected_frame;
    if current_frame.status != FrameStatus::Complete
        || current_frame.session_epoch != original.session_epoch
        || current_frame.surface != original.surface
        || current_frame.clock_domain != original.clock_domain
        || current_frame.geometry_revision != original.geometry_revision
        || current_frame.width_px != original.width_px
        || current_frame.height_px != original.height_px
        || current_frame.scale_factor != original.scale_factor
        || original.watch_id().is_none()
        || current_frame.watch_id() != original.watch_id()
        || original.window_geometry().is_none()
        || current_frame.window_geometry() != original.window_geometry()
    {
        return Err("computer batch screenshot watch or geometry identity changed".into());
    }
    Ok(())
}

/// Whether a complete Source read attests the exact PNG represented by this
/// frame. Event IDs and sequence numbers are not substitutes for this content
/// identity.
pub(crate) fn validate_exact_frame_pair(
    snapshot: &StructureSnapshot,
    frame: &FrameMeta,
) -> Result<(), String> {
    let stream = frame
        .window_geometry()
        .ok_or("computer frame has no native stream identity")?;
    let image_frame_id = frame
        .image_frame_id()
        .filter(|value| !value.is_empty())
        .ok_or("computer frame has no image content identity")?;
    if frame.status != FrameStatus::Complete
        || !snapshot.complete
        || snapshot.frame_ref.as_deref() != Some(image_frame_id)
        || snapshot.session_epoch != frame.session_epoch
        || snapshot.surface != frame.surface
        || snapshot.clock_domain != frame.clock_domain
        || snapshot.app.pid != stream.pid
        || focused_window_id(snapshot) != Some(stream.window_id)
        || snapshot.menu_context.is_some()
        || !snapshot.windows.iter().any(|window| {
            window.window_id == stream.window_id
                && window.owner_bundle_id == snapshot.app.bundle_id
                && window.frame == stream.window_frame
        })
    {
        return Err("complete Source snapshot is not paired with this exact image frame".into());
    }
    Ok(())
}

fn validate_pair(
    request: &ComputerFrameRequest,
    snapshot: &StructureSnapshot,
) -> Result<(), String> {
    let frame = &request.expected_frame;
    let stream = frame
        .window_geometry()
        .ok_or("computer frame request requires stream provenance")?;
    validate_exact_frame_pair(snapshot, frame)?;
    if !snapshot.complete
        || snapshot.menu_context.is_some()
        || focused_window_id(snapshot) != Some(stream.window_id)
        || request.fence.snapshot_id != snapshot.snapshot_id.as_str()
        || request.fence.structure_sequence != snapshot.source_sequence
        || snapshot.source_sequence == 0
        || snapshot.session_epoch != frame.session_epoch
        || snapshot.surface != frame.surface
        || snapshot.clock_domain != frame.clock_domain
        || snapshot.app.pid != stream.pid
    {
        return Err(
            "computer frame and complete active Source snapshot do not share identity".into(),
        );
    }

    let mut windows = snapshot.windows.iter().filter(|window| {
        window.window_id == stream.window_id && window.owner_bundle_id == snapshot.app.bundle_id
    });
    let window = windows
        .next()
        .ok_or("computer frame is not paired with one actual active window or sheet")?;
    if windows.next().is_some() || window.frame != stream.window_frame {
        return Err("computer frame window geometry is ambiguous or changed".into());
    }

    let roots: Vec<&UiObject> = snapshot
        .objects
        .iter()
        .filter(|object| {
            object.window_id == stream.window_id
                && object.parent.is_none()
                && match (window.kind, window.modal, object.role.as_str()) {
                    (LayerKind::Window, false, "AXWindow") => {
                        object.subrole.as_deref() == Some("AXStandardWindow")
                    }
                    (LayerKind::Sheet, true, "AXSheet") => true,
                    _ => false,
                }
        })
        .collect();
    if roots.len() != 1
        || roots[0].frame != stream.window_frame
        || !roots[0].in_view
        || roots[0].secure
    {
        return Err(
            "actual AXWindow or AXSheet root does not prove the complete captured rectangle".into(),
        );
    }

    validate_full_window_geometry(frame)?;
    Ok(())
}

fn validate_full_window_geometry(frame: &FrameMeta) -> Result<(), String> {
    let stream = frame
        .window_geometry()
        .ok_or("computer frame request requires stream provenance")?;
    let content = stream.content_rect;
    let window = stream.window_frame;
    let scale = frame.scale_factor * stream.content_scale;
    let within_pixel =
        |actual: f64, expected: f64| actual.is_finite() && (actual - expected).abs() <= 1.0;
    if !content.valid_bounded()
        || !window.valid_bounded()
        || content.width <= 0.0
        || content.height <= 0.0
        || window.width <= 0.0
        || window.height <= 0.0
        || content.x != 0.0
        || content.y != 0.0
        || !frame.scale_factor.is_finite()
        || frame.scale_factor <= 0.0
        || !stream.content_scale.is_finite()
        || stream.content_scale <= 0.0
        || !scale.is_finite()
        || scale <= 0.0
        || !within_pixel(
            content.width * frame.scale_factor,
            f64::from(frame.width_px),
        )
        || !within_pixel(
            content.height * frame.scale_factor,
            f64::from(frame.height_px),
        )
        || !within_pixel(window.width * scale, f64::from(frame.width_px))
        || !within_pixel(window.height * scale, f64::from(frame.height_px))
        || !within_pixel(
            (window.width * stream.content_scale - content.width) * frame.scale_factor,
            0.0,
        )
        || !within_pixel(
            (window.height * stream.content_scale - content.height) * frame.scale_factor,
            0.0,
        )
    {
        return Err("computer frame geometry is not a proven full-window transform".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::environment::pack::AppPack;
    use crate::environment::state_core::{EnvSnapshot, StateCore};
    use interaction_contracts::desktop::device::{DeviceIdentity, SurfaceRef};
    use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
    use interaction_contracts::desktop::source::{FrameStatus, FrameStreamInfo, StructureSnapshot};
    use sim_world::app::{SimApp, SnapshotMeta};
    use std::collections::BTreeMap;

    fn frame(sequence: u64, source_ms: u64) -> FrameMeta {
        FrameMeta {
            frame_id: format!("event-{sequence}"),
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
                        window_frame: interaction_contracts::desktop::source::Rect {
                            x: -100.0,
                            y: 0.0,
                            width: 100.0,
                            height: 100.0,
                        },
                        content_rect: interaction_contracts::desktop::source::Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 100.0,
                            height: 100.0,
                        },
                        content_scale: 1.0,
                        dirty_rects: Vec::new(),
                        dropped_frames: 0,
                        capture_interval_ms: 100,
                    },
                },
            ),
        }
    }

    fn request(expected_frame: FrameMeta, deadline_ms: u64) -> ComputerFrameRequest {
        ComputerFrameRequest {
            fence: interaction_contracts::desktop::computer::ComputerFence {
                request_id: RequestId::new("request-1").unwrap(),
                generation: 1,
                root_task_id: RootTaskId::new("root-1").unwrap(),
                task_revision: 1,
                root_epoch: 1,
                snapshot_id: "snapshot-1".into(),
                structure_sequence: 1,
                deadline_ms,
            },
            expected_frame,
        }
    }

    fn request_for(
        expected_frame: FrameMeta,
        snapshot: &StructureSnapshot,
        deadline_ms: u64,
    ) -> ComputerFrameRequest {
        ComputerFrameRequest {
            fence: interaction_contracts::desktop::computer::ComputerFence {
                request_id: RequestId::new(format!("request-{}", snapshot.source_sequence))
                    .unwrap(),
                generation: snapshot.source_sequence,
                root_task_id: RootTaskId::new("root-1").unwrap(),
                task_revision: 1,
                root_epoch: 1,
                snapshot_id: snapshot.snapshot_id.as_str().to_string(),
                structure_sequence: snapshot.source_sequence,
                deadline_ms,
            },
            expected_frame,
        }
    }

    fn paired_snapshot(
        sequence: u64,
        source_ms: u64,
        frame_ref: Option<&str>,
    ) -> StructureSnapshot {
        let frame = frame(sequence, source_ms);
        let stream = frame.window_geometry().unwrap();
        let mut snapshot = SimApp::new().snapshot(&SnapshotMeta {
            snapshot_id: format!("snapshot-{sequence}"),
            session_epoch: frame.session_epoch,
            surface: frame.surface.clone(),
            clock_domain: frame.clock_domain.clone(),
            source_sequence: sequence,
            observed_ms: source_ms,
        });
        snapshot.frame_ref = frame_ref.map(str::to_string);
        snapshot.app.pid = stream.pid;
        crate::testkit::focus_snapshot_window(&mut snapshot, stream.window_id.to_string());
        for window in &mut snapshot.windows {
            window.window_id = stream.window_id.to_string();
            window.frame = stream.window_frame;
        }
        for object in &mut snapshot.objects {
            object.window_id = stream.window_id.to_string();
            if object.parent.is_none() {
                object.frame = stream.window_frame;
            }
        }
        snapshot
    }

    fn device_identity(frame: &FrameMeta) -> DeviceIdentity {
        DeviceIdentity {
            session_epoch: frame.session_epoch,
            surface: frame.surface.clone(),
            clock_domain: frame.clock_domain.clone(),
            dependencies: BTreeMap::new(),
        }
    }

    #[test]
    fn freeze_requires_the_exact_png_content_id_not_the_event_id() {
        let current_frame = frame(1, 100);
        let exact = paired_snapshot(1, 100, Some("image-1"));
        validate_exact_frame_pair(&exact, &current_frame).unwrap();

        let missing = paired_snapshot(1, 100, None);
        assert!(validate_exact_frame_pair(&missing, &current_frame).is_err());

        let event_id = paired_snapshot(1, 100, Some("event-1"));
        assert!(validate_exact_frame_pair(&event_id, &current_frame).is_err());

        let env = EnvSnapshot {
            revision: 1,
            layer_revision: 1,
            snapshot: exact.clone(),
            page: None,
        };
        let frozen = freeze(
            request_for(current_frame.clone(), &exact, 1_000),
            &current_frame,
            &env,
        )
        .unwrap();
        assert_eq!(frozen.snapshot().frame_ref.as_deref(), Some("image-1"));
        assert!(
            freeze(
                request_for(current_frame.clone(), &missing, 1_000),
                &current_frame,
                &EnvSnapshot {
                    snapshot: missing,
                    ..env
                },
            )
            .is_err()
        );
        let event_env = EnvSnapshot {
            revision: 1,
            layer_revision: 1,
            snapshot: event_id.clone(),
            page: None,
        };
        assert!(
            freeze(
                request_for(current_frame.clone(), &event_id, 1_000),
                &current_frame,
                &event_env,
            )
            .is_err()
        );
    }

    #[test]
    fn a_new_image_cannot_freeze_against_an_old_tree_but_ax_reads_can_rebind() {
        let frame_one = frame(1, 100);
        let snapshot_one = paired_snapshot(1, 100, Some("image-1"));
        let mut state = StateCore::new();
        state.set_identity(&device_identity(&frame_one), true);
        let pack = AppPack::settings_fixture();
        state.admit(snapshot_one.clone(), 100, &pack).unwrap();
        state.admit_frame(frame_one.clone(), 100, 1_000).unwrap();
        let frozen_one = state
            .freeze_computer_observation(
                request_for(frame_one.clone(), &snapshot_one, 1_000),
                100,
                1_000,
            )
            .unwrap();

        let frame_two = frame(2, 140);
        state.admit_frame(frame_two.clone(), 140, 1_000).unwrap();
        assert!(
            state
                .freeze_computer_observation(
                    request_for(frame_two.clone(), &snapshot_one, 1_000),
                    140,
                    1_000,
                )
                .is_err()
        );

        let snapshot_two = paired_snapshot(2, 140, Some("image-2"));
        state.admit(snapshot_two.clone(), 140, &pack).unwrap();
        let frozen_two = state
            .freeze_computer_observation(request_for(frame_two, &snapshot_two, 1_000), 140, 1_000)
            .unwrap();

        let ordinary_ax_read = paired_snapshot(3, 150, None);
        state.admit(ordinary_ax_read, 150, &pack).unwrap();
        assert!(
            state
                .validate_frozen_computer_observation(&frozen_one, 150, 1_000)
                .is_ok()
        );
        assert!(
            state
                .validate_frozen_computer_observation(&frozen_two, 150, 1_000)
                .is_ok()
        );
    }

    #[test]
    fn a_new_complete_image_cannot_extend_the_frozen_request_deadline() {
        let original_frame = frame(1, 100);
        let frozen = FrozenComputerObservation {
            request: request(original_frame, 150),
            snapshot: sim_world::app::SimApp::new().snapshot(&crate::testkit::meta(1, 100)),
            environment_revision: 1,
            layer_revision: 1,
            page: None,
        };
        let identity = DeviceIdentity {
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "macos/1/wall".into(),
            dependencies: BTreeMap::new(),
        };
        let mut state = StateCore::new();
        state.set_identity(&identity, false);
        state.admit_frame(frame(1, 100), 100, 1_000).unwrap();
        state.admit_frame(frame(2, 140), 140, 1_000).unwrap();
        assert!(
            state
                .validate_frozen_computer_observation(&frozen, 140, 1_000)
                .is_ok()
        );
        state.admit_frame(frame(3, 180), 180, 1_000).unwrap();
        assert_eq!(
            state
                .validate_frozen_computer_observation(&frozen, 180, 1_000)
                .unwrap_err(),
            "frozen computer request deadline has elapsed"
        );
    }

    #[test]
    fn frame_geometry_requires_the_exact_full_window_transform() {
        let complete = frame(1, 100);
        validate_full_window_geometry(&complete).unwrap();

        let mut cropped = complete.clone();
        cropped.stream_info_mut().unwrap().content_rect.x = 1.0;
        assert!(validate_full_window_geometry(&cropped).is_err());

        let mut rescaled = complete.clone();
        rescaled.stream_info_mut().unwrap().content_scale = 2.0;
        assert!(validate_full_window_geometry(&rescaled).is_err());
    }

    #[test]
    fn retina_stream_and_capture_freeze_and_map_the_same_window_points() {
        use interaction_contracts::desktop::source::{
            FrameProvenance, Rect, SingleCaptureFrameInfo,
        };

        for is_capture in [false, true] {
            let mut current = frame(1, 100);
            current.width_px = 1160;
            current.height_px = 768;
            current.scale_factor = 2.0;
            let stream = current.stream_info_mut().unwrap();
            stream.window_frame = Rect {
                x: 176.0,
                y: 82.0,
                width: 1160.0,
                height: 768.0,
            };
            stream.content_rect = Rect {
                x: 0.0,
                y: 0.0,
                width: 580.0,
                height: 384.0,
            };
            stream.content_scale = 0.5;
            if is_capture {
                let stream = current.stream_info().unwrap().clone();
                current.provenance = Some(FrameProvenance::SingleCapture {
                    capture: SingleCaptureFrameInfo {
                        watch_id: stream.stream_id,
                        request_id: RequestId::new("request-1").unwrap(),
                        image_frame_id: stream.image_frame_id.unwrap(),
                        pid: stream.pid,
                        window_id: stream.window_id,
                        window_frame: stream.window_frame,
                        content_rect: stream.content_rect,
                        content_scale: stream.content_scale,
                        source_clock: "mach_absolute".into(),
                        capture_started_uptime_ns: 100_000_000,
                        capture_finished_uptime_ns: 110_000_000,
                        capture_started_ms: 100,
                        capture_finished_ms: 114,
                        clock_uncertainty_ms: 2,
                        requested_after_ms: 90,
                        deadline_ms: 1000,
                    },
                });
            }
            let mut snapshot = paired_snapshot(1, 100, Some("image-1"));
            let window = current.window_geometry().unwrap().window_frame;
            for root in snapshot
                .objects
                .iter_mut()
                .filter(|object| object.parent.is_none())
            {
                root.frame = window;
            }
            for layer in &mut snapshot.windows {
                layer.frame = window;
            }
            let env = EnvSnapshot {
                revision: 1,
                layer_revision: 1,
                snapshot: snapshot.clone(),
                page: None,
            };
            let frozen = freeze(
                request_for(current.clone(), &snapshot, 1000),
                &current,
                &env,
            )
            .unwrap();
            assert_eq!(
                frozen.image_pixel_to_screen(580.0, 384.0).unwrap(),
                (756.0, 466.0)
            );
            assert!(frozen.image_pixel_to_screen(1160.0, 384.0).is_err());
        }
    }

    #[test]
    fn full_window_geometry_rejects_zero_area_and_overflowing_scales() {
        for mutation in ["zero-window", "zero-content", "overflow", "nan"] {
            let mut invalid = frame(1, 100);
            let stream = invalid.stream_info_mut().unwrap();
            match mutation {
                "zero-window" => stream.window_frame.width = 0.0,
                "zero-content" => stream.content_rect.height = 0.0,
                "overflow" => stream.content_scale = f64::MAX,
                "nan" => stream.content_scale = f64::NAN,
                _ => unreachable!(),
            }
            assert!(
                validate_full_window_geometry(&invalid).is_err(),
                "{mutation}"
            );
        }
    }
}
