//! Native frame safety with deterministic pixels and the simulated device.
mod common;
use common::world::{Backend, TEXT, TEXT_AMBIGUOUS, World, engine_config};
use interaction_contracts::desktop::device::{DeviceIdentity, GrantOutcome, ReceiptStatus};
use interaction_contracts::desktop::operator::OperatorCommand;
use interaction_contracts::desktop::source::{
    AppIdentity, FrameMeta, FrameStatus, FrameStreamInfo, GlobalFocusObservation,
    KernelProcessIdentity, ObservationFact, Rect, SourceBackgroundWindowContext, SourceHealth,
    SourceKind, SourceStamp, SourceState, SourceWindowContext, StructureSnapshot, TargetContext,
};
use interaction_contracts::desktop::task::RootState;
use interaction_engine::engine::Input;
use interaction_engine::environment::{pack::AppPack, state_core::StateCore};
use interaction_engine::execution::DeviceReply;
use sim_world::app::{SimApp, SnapshotMeta};

fn image(identity: &DeviceIdentity, seq: u64, at: u64) -> FrameMeta {
    let current = snapshot(at, seq);
    FrameMeta {
        frame_id: format!("record-{seq}"),
        session_epoch: identity.session_epoch,
        surface: identity.surface.clone(),
        clock_domain: identity.clock_domain.clone(),
        source_sequence: seq,
        source_ms: at,
        status: FrameStatus::Complete,
        geometry_revision: 1,
        width_px: 100,
        height_px: 100,
        scale_factor: 1.0,
        byte_length: 100,
        checksum: format!("sha256:image-{seq}"),
        provenance: Some(
            interaction_contracts::desktop::source::FrameProvenance::Stream {
                stream: FrameStreamInfo {
                    stream_id: "stream-a".into(),
                    image_frame_id: Some(format!("image-{seq}")),
                    display_time_ticks: at * 1_000_000,
                    display_time_ns: at * 1_000_000,
                    received_uptime_ns: at * 1_000_000,
                    received_unix_ms: 100_000 + at,
                    callback_ms: at,
                    source_clock: "mach_absolute".into(),
                    window_id: current
                        .global_focus
                        .known_window_for_app(&current.app)
                        .expect("the simulated app reports its focused window")
                        .to_owned(),
                    pid: current.app.pid,
                    window_frame: current.windows[0].frame,
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
fn focus_elsewhere(snapshot: &mut StructureSnapshot) {
    snapshot.global_focus = GlobalFocusObservation {
        app: ObservationFact::Known {
            value: AppIdentity {
                bundle_id: "test.other".into(),
                name: "Other application".into(),
                pid: 9000,
            },
        },
        window_id: ObservationFact::Known {
            value: "other-window".into(),
        },
    };
}

fn install_native_target_context(snapshot: &mut StructureSnapshot) {
    let mut source = SourceBackgroundWindowContext {
        window: SourceWindowContext {
            source_session_id: "native-source-test".into(),
            session_epoch: snapshot.session_epoch,
            process: KernelProcessIdentity {
                pid: snapshot.app.pid,
                start_seconds: 100,
                start_micros: 20,
            },
            origin_window_id: snapshot.windows[0].window_id.clone(),
            origin_window_frame: snapshot.windows[0].frame,
            verified_source_sequence: snapshot.source_sequence,
            verified_observed_ms: snapshot.observed_ms,
        },
        hidden: ObservationFact::Known { value: false },
        minimized: ObservationFact::Known { value: false },
        target_context_hash: String::new(),
    };
    source.target_context_hash = source
        .computed_target_context_hash(&snapshot.app)
        .expect("synthetic target context has a bounded stable identity");
    snapshot.target_context = Some(TargetContext::NativeWindow {
        app: snapshot.app.clone(),
        source,
    });
}
fn idle(original: &FrameMeta, seq: u64, at: u64) -> FrameMeta {
    let mut meta = original.clone();
    meta.frame_id = format!("record-{seq}");
    meta.source_sequence = seq;
    meta.status = FrameStatus::Idle;
    meta.byte_length = 0;
    let stream = meta.stream_info_mut().unwrap();
    stream.callback_ms = at;
    stream.received_uptime_ns = at * 1_000_000;
    stream.received_unix_ms = 100_000 + at;
    meta
}
fn identity() -> DeviceIdentity {
    let world = World::new();
    world.engine.device().identity().unwrap().clone()
}
fn snapshot(at: u64, seq: u64) -> interaction_contracts::desktop::source::StructureSnapshot {
    let id = identity();
    SimApp::new().snapshot(&SnapshotMeta {
        snapshot_id: format!("snap-{seq}"),
        session_epoch: id.session_epoch,
        surface: id.surface,
        clock_domain: id.clock_domain,
        source_sequence: seq,
        observed_ms: at,
    })
}
fn health(seq: u64, at: u64, state: SourceState) -> SourceHealth {
    SourceHealth {
        source: SourceKind::Ax,
        state,
        reason: Some("AX status".into()),
        at_ms: at,
        provenance: Some(SourceStamp {
            session_epoch: 1,
            stream_id: "ax-a".into(),
            source_sequence: seq,
        }),
    }
}
fn screenshot_health(seq: u64, at: u64, state: SourceState) -> SourceHealth {
    let mut health = health(seq, at, state);
    health.source = SourceKind::Screenshot;
    health.provenance.as_mut().unwrap().stream_id = "stream-a".into();
    health
}
#[test]
fn idle_keeps_original_pixel_age_and_expiry_does_not_call_the_live_stream_frozen() {
    let mut core = StateCore::with_frame_timeout(1_000);
    core.set_identity(&identity(), true);
    let first = image(&identity(), 1, 100);
    core.admit_frame(first.clone(), 100, 500).unwrap();
    core.admit(snapshot(100, 1), 100, &AppPack::settings_fixture())
        .unwrap();
    core.admit_frame(idle(&first, 2, 550), 550, 500).unwrap();
    assert_eq!(core.frame_deadline(550, 500), Some(600));
    assert!(
        core.expire_sources(600, 500)
            .unwrap()
            .contains("pixels expired")
    );
    assert!(core.expire_sources(601, 500).is_none());
    let view = core.observation_view(600).unwrap();
    assert!(
        view.frame.as_ref().unwrap().healthy,
        "live idle is not frozen"
    );
    assert_eq!(view.frame.as_ref().unwrap().pixel_age_ms, Some(500));
    assert_eq!(
        view.sources
            .iter()
            .find(|h| h.source == SourceKind::Screenshot)
            .unwrap()
            .state,
        SourceState::Degraded
    );
    assert!(!core.usable(600, 500));
    core.note_health(health(1, 610, SourceState::Available), 610)
        .unwrap();
    core.admit(snapshot(620, 2), 620, &AppPack::settings_fixture())
        .unwrap();
    assert!(
        !core.usable(620, 500),
        "health and AX cannot restore expired pixels"
    );
    core.admit_frame(image(&identity(), 3, 630), 630, 500)
        .unwrap();
    assert!(core.usable(630, 500));
}
#[test]
fn health_rejects_replay_wrong_epochs_old_streams_and_future_or_unbounded_stamps() {
    let mut core = StateCore::new();
    core.set_identity(&identity(), true);
    core.note_health(health(1, 100, SourceState::Available), 100)
        .unwrap();
    core.note_health(health(2, 200, SourceState::Unavailable), 200)
        .unwrap();
    let mut cases = vec![
        health(1, 210, SourceState::Available),
        health(3, 190, SourceState::Available),
        health(3, 300, SourceState::Available),
    ];
    let mut bad_epoch = health(3, 210, SourceState::Available);
    bad_epoch.provenance.as_mut().unwrap().session_epoch = 2;
    cases.push(bad_epoch);
    let mut huge = health(3, 210, SourceState::Available);
    huge.provenance.as_mut().unwrap().stream_id = "x".repeat(257);
    cases.push(huge);
    let mut zero = health(0, 210, SourceState::Available);
    zero.at_ms = 210;
    cases.push(zero);
    for bad in cases {
        assert!(core.note_health(bad, 210).is_err());
    }
    assert!(core.structure_unavailable());
    core.note_health(
        SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Unavailable,
            reason: Some("trusted peer EOF".into()),
            at_ms: 215,
            provenance: None,
        },
        215,
    )
    .unwrap();
    let mut new = health(1, 220, SourceState::Available);
    new.provenance.as_mut().unwrap().stream_id = "ax-b".into();
    core.note_health(new, 220).unwrap();
    assert!(
        core.note_health(health(4, 230, SourceState::Available), 230)
            .is_err()
    );
    assert_eq!(
        core.health(SourceKind::Ax)
            .unwrap()
            .provenance
            .as_ref()
            .unwrap()
            .stream_id,
        "ax-b"
    );
    let mut changed = identity();
    changed.dependencies.insert("binding".into(), 50);
    assert!(
        !core.set_identity(&changed, true),
        "ordinary revisions retain evidence"
    );
    changed.session_epoch += 1;
    changed.surface.epoch += 1;
    assert!(core.set_identity(&changed, true));
    assert!(core.observation_view(240).is_none());
}
/// Pair the foreground-native trust profile with deterministic fake input and
/// an explicitly injected frame; no physical device is involved.
fn native_frame_world() -> World {
    let mut world = World::foreground_native_fixture();
    world.run_for(100);
    world.publish();
    let frame = image(world.engine.device().identity().unwrap(), 1, world.now);
    world.send(Input::Frame(Box::new(frame)));
    world
}
#[test]
fn a_frozen_stream_closes_pending_decisions_before_a_late_model_response() {
    let mut world = native_frame_world();
    world.backend = Backend::Silent;
    world.start(TEXT_AMBIGUOUS);
    let request = world.decision_requests.last().unwrap().clone();
    let mut stopped = image(world.engine.device().identity().unwrap(), 2, world.now);
    stopped.status = FrameStatus::Stopped;
    stopped.byte_length = 0;
    world.send(Input::Frame(Box::new(stopped)));
    let stale = world.teacher.decide(&request);
    world.send(Input::Decision(Box::new(stale)));
    assert!(
        world.applied().is_empty(),
        "a late response cannot revive a stopped frame"
    );
    let decisions: i64 = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT COUNT(*) FROM decisions WHERE state='pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(decisions, 0);
    assert!(
        !world
            .ui
            .as_ref()
            .unwrap()
            .observation
            .as_ref()
            .unwrap()
            .frame
            .as_ref()
            .unwrap()
            .healthy
    );
}
#[test]
fn submitted_input_stays_unknown_and_paused_when_fresh_source_evidence_returns() {
    let mut world = native_frame_world();
    world.pending_applied_submission.set(true);
    world.start(TEXT);
    assert_eq!(world.applied().len(), 1);
    let original_submits = world.sent("submit");
    let lost = SourceHealth {
        source: SourceKind::Screenshot,
        state: SourceState::Unavailable,
        reason: Some("injected stream loss".into()),
        at_ms: world.now,
        provenance: None,
    };
    world.send(Input::SourceHealth(lost));
    assert_eq!(
        world.state(),
        Some(RootState::Suspended),
        "{}",
        world.report()
    );
    let unknown: i64 = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT COUNT(*) FROM attempts WHERE state='unknown'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unknown, 1);
    world.run_for(100);
    world.publish();
    let frame = image(world.engine.device().identity().unwrap(), 2, world.now);
    world.send(Input::Frame(Box::new(frame)));
    world.send(Input::SourceHealth(screenshot_health(
        3,
        world.now,
        SourceState::Available,
    )));
    assert_eq!(world.state(), Some(RootState::Suspended));
    assert_eq!(world.sent("submit"), original_submits);
    world.drop_query_replies = usize::MAX;
    let mark = world.log_len();
    world.operator(OperatorCommand::ResumeTask {
        root_task_id: interaction_contracts::desktop::ids::RootTaskId::new("root-1").unwrap(),
    });
    assert_eq!(
        world.sent_since(mark, "query_receipt"),
        1,
        "resume queries the original intent"
    );
    assert_eq!(
        world.sent("submit"),
        original_submits,
        "reconciliation blocks new input"
    );
    assert_eq!(world.state(), Some(RootState::NeedsRevalidation));
}
#[test]
fn callbacks_expire_before_a_model_event_even_without_a_tick() {
    let mut world = native_frame_world();
    world.backend = Backend::Silent;
    world.start(TEXT_AMBIGUOUS);
    let request = world.decision_requests.last().unwrap().clone();
    world.now += 2_001;
    let response = world.teacher.decide(&request);
    world.send(Input::Decision(Box::new(response)));
    assert!(world.applied().is_empty());
    let view = world.ui.as_ref().unwrap().observation.as_ref().unwrap();
    assert!(!view.frame.as_ref().unwrap().healthy);
    assert_eq!(
        view.sources
            .iter()
            .find(|h| h.source == SourceKind::Screenshot)
            .unwrap()
            .state,
        SourceState::Unavailable
    );
    assert!(
        world
            .engine
            .timeline()
            .any(|item| item.text.contains("stopped reporting callbacks"))
    );
}

#[test]
fn a_late_grant_after_source_loss_cannot_submit_the_cancelled_attempt() {
    let mut world = native_frame_world();
    world.hold_grants = true;
    world.start(TEXT);
    assert_eq!(world.sent("grant_permit"), 1);
    assert_eq!(world.sent("submit"), 0);
    world.send(Input::SourceHealth(SourceHealth {
        source: SourceKind::Screenshot,
        state: SourceState::Unavailable,
        reason: Some("injected source loss before submit".into()),
        at_ms: world.now,
        provenance: None,
    }));
    world.deliver_delayed();
    assert_eq!(world.sent("submit"), 0);
    assert!(world.applied().is_empty());
    assert_eq!(
        world.engine.view().unwrap().attempts_used,
        0,
        "proven unsent quota refunded"
    );
    assert!(world.engine.bindings().all().next().is_none());
}

#[test]
fn an_ambiguous_recovery_record_keeps_unknown_input_and_requires_explicit_retry() {
    for status in [
        None,
        Some(ReceiptStatus::Accepted),
        Some(ReceiptStatus::Running),
        Some(ReceiptStatus::Unknown),
        Some(ReceiptStatus::Partial),
    ] {
        let mut world = native_frame_world();
        world.pending_applied_submission.set(true);
        world.start(TEXT);
        assert_eq!(world.applied().len(), 1);
        let submits = world.sent("submit");
        world.send(Input::SourceHealth(SourceHealth {
            source: SourceKind::Screenshot,
            state: SourceState::Unavailable,
            reason: Some("source lost after submit".into()),
            at_ms: world.now,
            provenance: None,
        }));
        world.run_for(50);
        world.publish();
        world.send(Input::Frame(Box::new(image(
            world.engine.device().identity().unwrap(),
            2,
            world.now,
        ))));
        world.send(Input::SourceHealth(screenshot_health(
            3,
            world.now,
            SourceState::Available,
        )));
        world.query_statuses.push_back(status);
        let queries = world.sent("query_receipt");
        world.operator(OperatorCommand::ResumeTask {
            root_task_id: interaction_contracts::desktop::ids::RootTaskId::new("root-1").unwrap(),
        });
        world.run_for(250);
        assert_eq!(
            world.state(),
            Some(RootState::NeedsRevalidation),
            "{status:?}: {}",
            world.report()
        );
        assert_eq!(world.sent("query_receipt"), queries + 1);
        assert_eq!(world.sent("submit"), submits);
        assert_eq!(world.applied().len(), 1);
        assert_eq!(world.engine.view().unwrap().attempts_used, 1);
        let unknown: i64 = world
            .engine
            .storage()
            .read()
            .query_row(
                "SELECT COUNT(*) FROM attempts WHERE state='unknown'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unknown, 1);
    }
}

#[test]
fn an_unknown_os_submission_is_not_replayed_by_a_new_attempt() {
    let mut world = native_frame_world();
    world.unknown_submission.set(true);
    world.start(TEXT);
    world.run_for(250);
    assert_eq!(world.applied().len(), 1, "{}", world.report());
    assert_eq!(world.sent("submit"), 1);
    assert_eq!(world.state(), Some(RootState::Suspended));
    assert_eq!(world.engine.view().unwrap().attempts_used, 1);
    assert_eq!(world.switch("Push", "Daily picks"), Some(true));
}

#[test]
fn old_grants_cannot_change_a_new_dispatch_after_source_recovery() {
    for denied in [false, true] {
        let mut world = native_frame_world();
        world.hold_grants = true;
        world.start(TEXT);
        let mut old = world.take_delayed();
        assert_eq!(old.len(), 1);
        if denied && let Input::Device { reply, .. } = &mut old[0] {
            *reply = DeviceReply::Grant(GrantOutcome::Denied {
                code: None,
                reason: "old denial".into(),
            });
        }
        world.send(Input::SourceHealth(SourceHealth {
            source: SourceKind::Screenshot,
            state: SourceState::Unavailable,
            reason: Some("loss before submit".into()),
            at_ms: world.now,
            provenance: None,
        }));
        world.run_for(50);
        world.publish();
        world.send(Input::Frame(Box::new(image(
            world.engine.device().identity().unwrap(),
            2,
            world.now,
        ))));
        world.send(Input::SourceHealth(screenshot_health(
            3,
            world.now,
            SourceState::Available,
        )));
        let new = world.take_delayed();
        assert_eq!(new.len(), 1, "{}", world.report());
        let attempts: i64 = world
            .engine
            .storage()
            .read()
            .query_row("SELECT COUNT(*) FROM attempts", [], |row| row.get(0))
            .unwrap();
        world.send(old.remove(0));
        assert_eq!(world.sent("submit"), 0);
        let after: i64 = world
            .engine
            .storage()
            .read()
            .query_row("SELECT COUNT(*) FROM attempts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, attempts, "an old denial cannot fail the new attempt");
        world.hold_grants = false;
        for reply in new {
            world.send(reply);
        }
        assert_eq!(world.sent("submit"), 1);
        assert_eq!(world.applied().len(), 1);
    }
}

#[test]
fn observe_and_grant_replies_at_or_after_the_attempt_deadline_cannot_submit() {
    for observing in [false, true] {
        for delay in [100, 150] {
            let mut config = engine_config();
            config.attempt_window_ms = 100;
            let mut world = World::with_config_foreground_native(config);
            world.run_for(100);
            world.publish();
            world.send(Input::Frame(Box::new(image(
                world.engine.device().identity().unwrap(),
                1,
                world.now,
            ))));
            world.hold_observes = observing;
            world.hold_grants = !observing;
            world.start(TEXT);
            let delayed = world.take_delayed();
            assert_eq!(delayed.len(), 1);
            let deadline: u64 = world
                .engine
                .storage()
                .read()
                .query_row(
                    "SELECT deadline_ms FROM attempts ORDER BY rowid LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(deadline, world.now + 100);
            world.now += delay;
            for reply in delayed {
                world.send(reply);
            }
            assert_eq!(
                world.sent("submit"),
                0,
                "observing={observing} delay={delay}: {}",
                world.report()
            );
            assert!(world.applied().is_empty());
        }
    }
}

#[test]
fn a_delayed_complete_frame_before_the_failed_health_cannot_restore_authority() {
    let mut core = StateCore::new();
    core.set_identity(&identity(), true);
    core.admit_frame(image(&identity(), 1, 100), 100, 2_000)
        .unwrap();
    core.admit(snapshot(100, 1), 100, &AppPack::settings_fixture())
        .unwrap();
    core.note_health(screenshot_health(3, 200, SourceState::Unavailable), 200)
        .unwrap();
    core.admit(snapshot(250, 2), 250, &AppPack::settings_fixture())
        .unwrap();
    assert!(
        core.admit_frame(image(&identity(), 2, 150), 250, 2_000)
            .is_err()
    );
    core.note_health(screenshot_health(4, 250, SourceState::Available), 250)
        .unwrap();
    assert!(
        !core.usable(250, 2_000),
        "health alone cannot revive old pixels"
    );
    core.admit_frame(image(&identity(), 5, 260), 260, 2_000)
        .unwrap();
    assert!(core.usable(260, 2_000));
}

#[test]
fn newer_scoped_faults_and_local_eof_close_authority_despite_a_backwards_wall_clock() {
    for unstamped in [false, true] {
        let mut core = StateCore::new();
        core.set_identity(&identity(), true);
        core.admit_frame(image(&identity(), 1, 100), 100, 2_000)
            .unwrap();
        core.admit(snapshot(100, 1), 100, &AppPack::settings_fixture())
            .unwrap();
        core.note_health(screenshot_health(2, 100, SourceState::Available), 100)
            .unwrap();
        assert!(core.usable(100, 2_000));
        let mut fault = screenshot_health(3, 50, SourceState::Unavailable);
        if unstamped {
            fault.provenance = None;
        }
        assert!(core.note_health(fault, 100).unwrap());
        assert!(!core.usable(100, 2_000));
        assert_eq!(core.health(SourceKind::Screenshot).unwrap().at_ms, 100);
        assert!(
            core.note_health(screenshot_health(4, 50, SourceState::Available), 100)
                .is_err()
        );
        assert!(
            core.admit_frame(image(&identity(), 4, 75), 100, 2_000)
                .is_err()
        );
    }
}

#[test]
fn native_frames_cannot_authorize_a_different_ax_pid_window_or_geometry() {
    for change in ["pid", "window", "geometry"] {
        let mut core = StateCore::new();
        core.set_identity(&identity(), true);
        core.admit(snapshot(100, 1), 100, &AppPack::settings_fixture())
            .unwrap();
        let mut frame = image(&identity(), 1, 100);
        let stream = frame.stream_info_mut().unwrap();
        match change {
            "pid" => stream.pid += 1,
            "window" => stream.window_id = "other-window".into(),
            "geometry" => stream.window_frame.x += 100.0,
            _ => unreachable!(),
        }
        let result = core.admit_frame(frame, 100, 500);
        if change != "geometry" {
            assert!(result.is_err(), "{change}");
        } else {
            result.unwrap();
        }
        assert!(!core.usable(100, 500), "{change}");
    }
}

#[test]
fn geometry_complete_before_its_ax_read_keeps_capture_live_and_can_be_attested() {
    let mut core = StateCore::new();
    core.set_identity(&identity(), true);
    core.admit(snapshot(100, 1), 100, &AppPack::settings_fixture())
        .unwrap();
    core.admit_frame(image(&identity(), 1, 100), 100, 2_000)
        .unwrap();
    let mut moved = image(&identity(), 2, 150);
    moved.stream_info_mut().unwrap().window_frame.x += 100.0;
    core.admit_frame(moved.clone(), 150, 2_000).unwrap();
    assert!(core.observation_view(150).unwrap().frame.unwrap().healthy);
    assert!(!core.usable(150, 2_000));
    let mut after = snapshot(160, 2);
    after.windows[0].frame = moved.stream_info().unwrap().window_frame;
    core.admit(after, 160, &AppPack::settings_fixture())
        .unwrap();
    core.admit_frame(idle(&moved, 3, 170), 170, 2_000).unwrap();
    assert!(core.usable(170, 2_000));
    assert_eq!(core.frame_deadline(170, 2_000), Some(2_150));
}

#[test]
fn a_background_window_keeps_a_healthy_readonly_frame_without_an_input_lease() {
    let mut core = StateCore::new();
    core.set_identity(&identity(), true);
    let mut background = snapshot(100, 1);
    focus_elsewhere(&mut background);
    core.admit(background, 100, &AppPack::settings_fixture())
        .unwrap();
    let frame = image(&identity(), 1, 100);
    core.admit_frame(frame.clone(), 100, 500).unwrap();
    core.admit_frame(idle(&frame, 2, 300), 300, 500).unwrap();
    let view = core.observation_view(300).unwrap();
    assert!(view.frame.unwrap().healthy);
    assert!(view.active_window.is_none());
    assert!(!core.usable(300, 500));
    assert!(core.frame_deadline(300, 500).is_none());
}

#[test]
fn refreshed_pixels_cannot_extend_the_ax_execution_deadline() {
    let mut core = StateCore::new();
    core.set_identity(&identity(), true);
    core.admit(snapshot(100, 1), 100, &AppPack::settings_fixture())
        .unwrap();
    core.admit_frame(image(&identity(), 1, 100), 100, 500)
        .unwrap();
    core.admit_frame(image(&identity(), 2, 400), 400, 500)
        .unwrap();
    assert_eq!(core.frame_deadline(400, 500), Some(600));
    assert!(core.frame_deadline(600, 500).is_none());
    assert!(core.frame_deadline(650, 500).is_none());
    assert!(!core.usable(650, 500));
    assert!(core.observation_view(650).unwrap().frame.unwrap().healthy);
}

#[test]
fn a_local_refresh_fault_closes_old_authority_without_refreshing_its_pixels_or_callbacks() {
    let mut core = StateCore::new();
    core.set_identity(&identity(), true);
    core.admit(snapshot(100, 1), 100, &AppPack::settings_fixture())
        .unwrap();
    core.admit_frame(image(&identity(), 1, 100), 100, 500)
        .unwrap();
    core.note_health(screenshot_health(2, 100, SourceState::Available), 100)
        .unwrap();
    let before = core.observation_view(150).unwrap().frame.unwrap();
    assert!(core.usable(150, 500));
    core.note_health(screenshot_health(3, 150, SourceState::Unavailable), 150)
        .unwrap();
    assert!(!core.usable(150, 500));
    let after = core.observation_view(150).unwrap().frame.unwrap();
    assert!(!after.healthy);
    assert_eq!(before.pixel_age_ms, after.pixel_age_ms);
    assert_eq!(before.confirmation_age_ms, after.confirmation_age_ms);
    let mut fresh = image(&identity(), 1, 200);
    fresh.stream_info_mut().unwrap().stream_id = "stream-b".into();
    core.admit_frame(fresh, 200, 500).unwrap();
    assert!(!core.usable(200, 500));
    core.admit(snapshot(201, 2), 201, &AppPack::settings_fixture())
        .unwrap();
    assert!(
        !core.usable(201, 500),
        "fresh records still need the new source's healthy status"
    );
    let mut available = screenshot_health(3, 200, SourceState::Available);
    available.provenance.as_mut().unwrap().stream_id = "stream-b".into();
    core.note_health(available, 201).unwrap();
    assert!(core.usable(201, 500));
    assert!(
        core.note_health(screenshot_health(4, 210, SourceState::Available), 210)
            .is_err()
    );
}

#[test]
fn a_grant_with_a_live_permit_cannot_submit_after_the_ax_read_expires() {
    let mut config = engine_config();
    config.evidence_max_age_ms = 500;
    config.evidence_ttl_ms = 1_000;
    let mut world = World::with_config(config);
    world.run_for(100);
    world.hold_grants = true;
    world.start(TEXT);
    let delayed = world.take_delayed();
    assert_eq!(delayed.len(), 1);
    world.source_up = false;
    world.run_for(550);
    for reply in delayed {
        world.send(reply);
    }
    assert_eq!(world.sent("submit"), 0, "{}", world.report());
    assert!(world.applied().is_empty());
    assert_eq!(world.engine.view().unwrap().attempts_used, 0);
    assert!(
        world.sent("revoke") >= 1,
        "the granted permit is explicitly revoked"
    );
}

#[test]
fn a_different_active_window_cannot_borrow_the_captured_windows_pixels() {
    let mut core = StateCore::new();
    core.set_identity(&identity(), true);
    let mut two_windows = snapshot(100, 1);
    let mut other = two_windows.windows[0].clone();
    other.window_id = "w2".into();
    other.frame.x += 200.0;
    two_windows.windows.push(other.clone());
    core.admit(two_windows.clone(), 100, &AppPack::settings_fixture())
        .unwrap();
    core.admit_frame(image(&identity(), 1, 100), 100, 2_000)
        .unwrap();
    assert!(core.usable(100, 2_000));
    two_windows.source_sequence = 2;
    two_windows.observed_ms = 150;
    two_windows.global_focus.window_id = ObservationFact::Known { value: "w2".into() };
    core.admit(two_windows.clone(), 150, &AppPack::settings_fixture())
        .unwrap();
    core.admit_frame(image(&identity(), 2, 160), 160, 2_000)
        .unwrap();
    assert!(core.observation_view(160).unwrap().frame.unwrap().healthy);
    assert!(core.frame_deadline(160, 2_000).is_none());
    assert!(!core.usable(160, 2_000));
    let mut matching = image(&identity(), 3, 170);
    let stream = matching.stream_info_mut().unwrap();
    stream.window_id = other.window_id;
    stream.window_frame = other.frame;
    core.admit_frame(matching, 170, 2_000).unwrap();
    assert!(
        !core.usable(170, 2_000),
        "a matching frame also needs a later AX read"
    );
    two_windows.source_sequence = 3;
    two_windows.observed_ms = 180;
    core.admit(two_windows, 180, &AppPack::settings_fixture())
        .unwrap();
    assert!(core.usable(180, 2_000));
    assert!(core.frame_deadline(180, 2_000).is_some());
}

fn publish_focus(world: &mut World, active: bool) {
    world.core.advance_to(world.now).unwrap();
    world.source_sequence += 1;
    let mut snapshot = world.app.borrow().snapshot(&SnapshotMeta {
        snapshot_id: format!("focus-{}", world.source_sequence),
        session_epoch: world.engine.device().identity().unwrap().session_epoch,
        surface: world.engine.device().identity().unwrap().surface.clone(),
        clock_domain: world
            .engine
            .device()
            .identity()
            .unwrap()
            .clock_domain
            .clone(),
        source_sequence: world.source_sequence,
        observed_ms: world.now,
    });
    if !active {
        focus_elsewhere(&mut snapshot);
    }
    world.send(Input::Structure(Box::new(snapshot)));
}

#[test]
fn losing_foreground_cancels_a_pending_decision_without_calling_the_stream_lost() {
    let mut world = native_frame_world();
    world.backend = Backend::Silent;
    world.start(TEXT_AMBIGUOUS);
    let request = world.decision_requests.last().unwrap().clone();
    world.source_up = false;
    world.now += 50;
    publish_focus(&mut world, false);
    assert!(
        world
            .ui
            .as_ref()
            .unwrap()
            .observation
            .as_ref()
            .unwrap()
            .frame
            .as_ref()
            .unwrap()
            .healthy
    );
    assert!(
        world
            .ui
            .as_ref()
            .unwrap()
            .observation
            .as_ref()
            .unwrap()
            .active_window
            .is_none()
    );
    let pending: i64 = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT COUNT(*) FROM decisions WHERE state='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    let response = world.teacher.decide(&request);
    world.send(Input::Decision(Box::new(response)));
    assert_eq!(world.sent("submit"), 0);
    assert!(world.applied().is_empty());
}

#[test]
fn focus_restoration_requires_a_fresh_complete_and_ax_before_a_new_permit() {
    let mut world = native_frame_world();
    world.hold_grants = true;
    world.start(TEXT);
    let old = world.take_delayed();
    assert_eq!(old.len(), 1);
    let old_binding = world
        .engine
        .bindings()
        .all()
        .next()
        .unwrap()
        .binding_ref
        .clone();
    world.source_up = false;
    world.now += 50;
    publish_focus(&mut world, false);
    assert!(world.engine.bindings().all().next().is_none());
    assert_eq!(world.engine.view().unwrap().attempts_used, 0);
    assert!(
        world
            .ui
            .as_ref()
            .unwrap()
            .observation
            .as_ref()
            .unwrap()
            .frame
            .as_ref()
            .unwrap()
            .healthy
    );
    world.now += 50;
    publish_focus(&mut world, true);
    assert!(
        world
            .engine
            .state()
            .frame_deadline(world.now, 2_000)
            .is_none(),
        "foreground alone cannot reuse old pixels"
    );
    assert_eq!(world.sent("grant_permit"), 1);
    world.now += 1;
    world.send(Input::Frame(Box::new(image(
        world.engine.device().identity().unwrap(),
        2,
        world.now,
    ))));
    assert_eq!(
        world.sent("grant_permit"),
        1,
        "Complete still needs post-transition AX"
    );
    world.now += 1;
    publish_focus(&mut world, true);
    assert_eq!(world.sent("grant_permit"), 2, "{}", world.report());
    let new = world.take_delayed();
    assert_eq!(new.len(), 1);
    let new_binding = world
        .engine
        .bindings()
        .all()
        .next()
        .unwrap()
        .binding_ref
        .clone();
    assert_ne!(new_binding, old_binding);
    for reply in old {
        world.send(reply);
    }
    assert_eq!(world.sent("submit"), 0);
    world.hold_grants = false;
    for reply in new {
        world.send(reply);
    }
    assert_eq!(world.sent("submit"), 1);
    assert_eq!(world.applied().len(), 1);
}

#[test]
fn foreground_loss_after_submission_pauses_unknown_input_but_keeps_readonly_capture() {
    let mut world = native_frame_world();
    world.pending_applied_submission.set(true);
    world.start(TEXT);
    assert_eq!(world.applied().len(), 1);
    world.source_up = false;
    world.now += 50;
    publish_focus(&mut world, false);
    assert_eq!(world.state(), Some(RootState::Suspended));
    let unknown: i64 = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT COUNT(*) FROM attempts WHERE state='unknown'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unknown, 1);
    assert!(world.engine.bindings().all().next().is_none());
    assert!(
        world
            .ui
            .as_ref()
            .unwrap()
            .observation
            .as_ref()
            .unwrap()
            .frame
            .as_ref()
            .unwrap()
            .healthy
    );
    world.now += 50;
    publish_focus(&mut world, true);
    world.now += 1;
    world.send(Input::Frame(Box::new(image(
        world.engine.device().identity().unwrap(),
        2,
        world.now,
    ))));
    world.now += 1;
    publish_focus(&mut world, true);
    assert_eq!(world.state(), Some(RootState::Suspended));
    assert_eq!(
        world.sent("submit"),
        1,
        "fresh focus does not replay an unknown input"
    );
    assert_eq!(world.engine.view().unwrap().attempts_used, 1);
}

#[test]
fn observer_coverage_loss_does_not_invalidate_a_complete_direct_read() {
    let mut state = StateCore::new();
    state.set_session(1);
    let pack = AppPack::settings_fixture();
    state.admit(snapshot(10, 1), 10, &pack).unwrap();
    let mut loss = health(2, 20, SourceState::Degraded);
    loss.source = SourceKind::AxObserver;
    state.note_health(loss, 20).unwrap();
    assert!(state.usable(20, 100));
    assert_eq!(state.latest().unwrap().snapshot.observed_ms, 10);
}

#[test]
fn inactive_window_order_and_sharing_indicator_do_not_change_the_input_layer() {
    let mut state = StateCore::new();
    state.set_session(1);
    let pack = AppPack::settings_fixture();
    state.admit(snapshot(10, 1), 10, &pack).unwrap();
    let layer = state.latest().unwrap().layer_revision;
    let mut next = snapshot(20, 2);
    let mut aux = next.windows[0].clone();
    aux.window_id = "sharing-indicator".into();
    aux.title = Some("Window".into());
    next.windows.insert(0, aux);
    assert!(!state.admit(next.clone(), 20, &pack).unwrap().layer_changed);
    next.source_sequence = 3;
    next.observed_ms = 30;
    next.windows.reverse();
    assert!(!state.admit(next.clone(), 30, &pack).unwrap().layer_changed);
    assert_eq!(state.latest().unwrap().layer_revision, layer);
    next.source_sequence = 4;
    next.observed_ms = 40;
    let focused_window = next
        .global_focus
        .known_window_for_app(&next.app)
        .map(str::to_owned);
    next.windows
        .iter_mut()
        .find(|w| Some(w.window_id.as_str()) == focused_window.as_deref())
        .unwrap()
        .modal = true;
    assert!(state.admit(next, 40, &pack).unwrap().layer_changed);
}

#[test]
fn native_target_metadata_is_fresh_without_pixels_and_requires_one_resolved_window() {
    let mut id = identity();
    id.clock_domain = format!("macos/{}/wall", id.session_epoch);
    let mut state = StateCore::new();
    state.set_identity(&id, true);
    let pack = AppPack::settings_fixture();
    let mut read = snapshot(10, 1);
    read.clock_domain = id.clock_domain.clone();
    for o in &mut read.objects {
        if o.parent.is_none() {
            o.role = "AXWindow".into();
            o.subrole = Some("AXStandardWindow".into());
            o.frame = read
                .windows
                .iter()
                .find(|w| w.window_id == o.window_id)
                .unwrap()
                .frame;
        }
    }
    read.objects[1].secure = true;
    let focused_window = read
        .global_focus
        .known_window_for_app(&read.app)
        .expect("the native target starts with a same-app focused window")
        .to_owned();
    state.admit(read.clone(), 10, &pack).unwrap();
    assert!(
        state
            .window_root(&read.app.bundle_id, read.app.pid, &focused_window, 10, 2000)
            .is_some()
    );
    let generation = state.begin_refresh().unwrap();
    assert!(
        state
            .window_root(&read.app.bundle_id, read.app.pid, &focused_window, 10, 2000)
            .is_none(),
        "window activation cannot bypass a pending refresh"
    );
    assert!(state.refresh_started(generation));
    let target = state.observation_view(10).unwrap().observed_target.unwrap();
    assert_eq!(target.window_id, focused_window);
    assert_eq!(target.structure_sequence, read.source_sequence);
    assert!(target.contains_private_fields);
    assert!(
        !state.usable(10, 2000),
        "metadata does not grant an input or pixel lease"
    );
    assert!(
        state
            .observation_view(2010)
            .unwrap()
            .observed_target
            .is_none()
    );
    read.source_sequence = 2;
    read.observed_ms = 20;
    focus_elsewhere(&mut read);
    install_native_target_context(&mut read);
    state.admit(read.clone(), 20, &pack).unwrap();
    let target = state
        .observation_view(20)
        .unwrap()
        .observed_target
        .expect("a unique background window is readable");
    assert_eq!(target.structure_sequence, read.source_sequence);
    let mut second = read.windows[0].clone();
    second.window_id = "other-window".into();
    let mut object = read
        .objects
        .iter()
        .find(|o| o.parent.is_none())
        .unwrap()
        .clone();
    object.instance_ref =
        interaction_contracts::desktop::ids::ObjectRef::new("other-root").unwrap();
    object.window_id = second.window_id.clone();
    read.windows.push(second);
    read.objects.push(object);
    read.target_context = None;
    sim_world::app::refresh_simulated_observer_coverage(&mut read);
    read.source_sequence = 3;
    read.observed_ms = 30;
    state.admit(read, 30, &pack).unwrap();
    assert!(
        state
            .observation_view(30)
            .unwrap()
            .observed_target
            .is_none(),
        "ambiguous background windows cannot be authorized from a guessed selection"
    );
}

#[test]
fn source_loss_after_known_terminal_input_keeps_readback_phase_without_pausing_or_replaying() {
    let mut world = native_frame_world();
    world.start(TEXT);
    world.source_up = false;
    world.run_for(150);
    assert_eq!(world.applied().len(), 1);
    let submits = world.sent("submit");
    world.send(Input::SourceHealth(SourceHealth {
        source: SourceKind::Screenshot,
        state: SourceState::Unavailable,
        reason: Some("new pixels required after a known input".into()),
        at_ms: world.now,
        provenance: None,
    }));
    assert_ne!(
        world.state(),
        Some(RootState::Suspended),
        "{}",
        world.report()
    );
    assert_eq!(world.sent("submit"), submits);
    assert_eq!(world.engine.view().unwrap().attempts_used, 1);
    assert!(
        world.engine.bindings().all().next().is_none(),
        "old binding authority remains revoked"
    );
}
