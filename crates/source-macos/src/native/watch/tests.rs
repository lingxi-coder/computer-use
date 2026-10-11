use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn private_diagnostics_retain_only_a_bounded_raw_prefix_and_drain_the_tail() {
    let directory = tempfile::tempdir().unwrap();
    socket::private_dir(directory.path()).unwrap();
    let sink = DiagnosticSink::new(directory.path()).unwrap();
    let mut raw = vec![0xff; MAX_DIAGNOSTICS + 7];
    raw.extend_from_slice(&vec![b'x'; MAX_DIAGNOSTICS * 8]);
    let mut input = std::io::Cursor::new(raw);
    let captured = drain_diagnostics(&mut input, Some(sink));
    assert_eq!(
        input.position(),
        input.get_ref().len() as u64,
        "the entire stream is drained after the prefix fills"
    );
    assert_eq!(captured.prefix.len(), MAX_DIAGNOSTICS);
    assert!(captured.prefix.iter().all(|byte| *byte == 0xff));
    let file = directory.path().join(DIAGNOSTICS_FILE);
    let metadata = fs::symlink_metadata(&file).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert!(
        metadata.len() <= (MAX_DIAGNOSTICS * 6 + 1024) as u64,
        "lossy UTF-8 and JSON escaping still have a fixed bound"
    );
    let value: serde_json::Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(
        value["raw_stderr"].as_str().unwrap(),
        "\u{fffd}".repeat(MAX_DIAGNOSTICS)
    );
    assert_eq!(value["retained_bytes"], MAX_DIAGNOSTICS);
    assert_eq!(value["drained_bytes"], input.get_ref().len());
    assert_eq!(
        value["discarded_bytes"],
        input.get_ref().len() - MAX_DIAGNOSTICS
    );
    assert_eq!(value["truncated"], true);
    assert_eq!(value["complete"], true);
    assert_eq!(value["read_failed"], false);
    assert!(!directory.path().join(DIAGNOSTICS_TEMP).exists());
}

#[test]
fn private_diagnostics_refuse_existing_and_replaced_symlinks_without_touching_targets() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("watch");
    socket::private_dir(&directory).unwrap();
    let victim = root.path().join("outside-watch");
    fs::write(&victim, b"retain this file").unwrap();
    let destination = directory.join(DIAGNOSTICS_FILE);
    symlink(&victim, &destination).unwrap();
    assert!(DiagnosticSink::new(&directory).is_err());
    assert_eq!(fs::read(&victim).unwrap(), b"retain this file");
    fs::remove_file(&destination).unwrap();
    let mut sink = DiagnosticSink::new(&directory).unwrap();
    let mut bytes = DiagnosticBytes::default();
    bytes.retain(b"private diagnostics");
    let temporary = directory.join(DIAGNOSTICS_TEMP);
    symlink(&victim, &temporary).unwrap();
    assert!(sink.publish(&bytes, false, false).is_err());
    assert!(
        fs::symlink_metadata(&temporary)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&victim).unwrap(), b"retain this file");
    fs::remove_file(&temporary).unwrap();
    fs::remove_file(&destination).unwrap();
    symlink(&victim, &destination).unwrap();
    assert!(sink.publish(&bytes, false, false).is_err());
    assert_eq!(fs::read(&victim).unwrap(), b"retain this file");
    assert!(
        sink.published.metadata().unwrap().is_file(),
        "the originally owned file handle survives a replaced pathname"
    );
}

#[test]
fn private_diagnostic_updates_publish_complete_snapshots_and_stop_io_after_the_cap() {
    let directory = tempfile::tempdir().unwrap();
    socket::private_dir(directory.path()).unwrap();
    let mut sink = DiagnosticSink::new(directory.path()).unwrap();
    let mut bytes = DiagnosticBytes::default();
    for chunk in [b"one\n".as_slice(), b"two\n".as_slice()] {
        assert!(bytes.retain(chunk));
        sink.publish(&bytes, false, false).unwrap();
        let current: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.path().join(DIAGNOSTICS_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            current["raw_stderr"].as_str().unwrap(),
            String::from_utf8_lossy(&bytes.prefix).as_ref()
        );
        assert_eq!(current["complete"], false);
        assert!(
            regular_file_matches(&directory.path().join(DIAGNOSTICS_FILE), &sink.published)
                .unwrap()
        );
    }
    assert!(bytes.retain(&vec![b'x'; MAX_DIAGNOSTICS]));
    sink.publish(&bytes, false, false).unwrap();
    assert!(
        !bytes.retain(b"more bytes"),
        "additional chunks only drain; they do not trigger disk updates"
    );
    assert_eq!(bytes.prefix.len(), MAX_DIAGNOSTICS);
    sink.publish(&bytes, true, false).unwrap();
}

#[test]
fn ndjson_record_limit_is_checked_before_a_delimiter_exists() {
    let mut input = std::io::Cursor::new(vec![b'x'; MAX_HELPER_OUTPUT + 1]);
    assert!(read_record(&mut input).unwrap_err().contains("byte limit"));
    let mut input = std::io::Cursor::new(b"{}".to_vec());
    assert!(
        read_record(&mut input)
            .unwrap_err()
            .contains("inside a record")
    );
}

fn frame_report(sequence: u64, status: FrameStatus) -> WatchReport {
    use interaction_contracts::desktop::source::{FrameStreamInfo, Rect};
    let mut report = WatchReport::new("stream".into(), sequence);
    report.frame_meta = Some(FrameMeta {
        frame_id: format!("event-{sequence}"),
        session_epoch: 1,
        surface: interaction_contracts::desktop::device::SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "macos/1/wall".into(),
        source_sequence: sequence,
        source_ms: 100,
        status,
        geometry_revision: 1,
        width_px: 10,
        height_px: 10,
        scale_factor: 1.0,
        byte_length: 0,
        checksum: "hash".into(),
        provenance: Some(
            interaction_contracts::desktop::source::FrameProvenance::Stream {
                stream: FrameStreamInfo {
                    stream_id: "stream".into(),
                    image_frame_id: Some("image".into()),
                    display_time_ticks: 1,
                    display_time_ns: 1,
                    received_uptime_ns: sequence,
                    received_unix_ms: sequence,
                    callback_ms: sequence,
                    source_clock: "mach_absolute".into(),
                    content_rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 10.0,
                        height: 10.0,
                    },
                    content_scale: 1.0,
                    dirty_rects: vec![],
                    dropped_frames: 0,
                    capture_interval_ms: 1000,
                    window_id: "window-1".into(),
                    pid: 42,
                    window_frame: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 10.0,
                        height: 10.0,
                    },
                },
            },
        ),
    });
    report
}

#[test]
fn mailbox_preserves_first_complete_when_idle_bursts_arrive() {
    let mut mailbox = Mailbox::default();
    mailbox.publish(frame_report(1, FrameStatus::Complete));
    for sequence in 2..=20 {
        mailbox.publish(frame_report(sequence, FrameStatus::Idle));
    }
    let reports = mailbox.take();
    assert_eq!(reports.len(), 2);
    assert_eq!(
        reports[0].frame_meta.as_ref().unwrap().status,
        FrameStatus::Complete
    );
    assert_eq!(reports[1].sequence, 20);
    assert_eq!(
        reports[1]
            .frame_meta
            .as_ref()
            .unwrap()
            .stream_info()
            .unwrap()
            .dropped_frames,
        0
    );
    assert_eq!(
        mailbox.coalesced, 18,
        "idle supersession is explicit coalescing, not a pixel gap"
    );
    mailbox.fail(WatchReport::new("stream".into(), 21));
    mailbox.publish(frame_report(22, FrameStatus::Complete));
    assert_eq!(
        mailbox.take()[0].sequence,
        21,
        "terminal failure wins over stale available data"
    );
}

fn health_report(sequence: u64, source: SourceKind, state: SourceState) -> WatchReport {
    let mut report = WatchReport::new("stream".into(), sequence);
    report.health.push(SourceHealth {
        source,
        state,
        reason: Some(format!("original status {sequence}")),
        at_ms: sequence * 100,
        provenance: Some(interaction_contracts::desktop::source::SourceStamp {
            session_epoch: 1,
            stream_id: "stream".into(),
            source_sequence: sequence,
        }),
    });
    report
}

#[test]
fn mailbox_retains_each_source_loss_before_later_recovery() {
    let mut mailbox = Mailbox::default();
    let ax_loss = health_report(2, SourceKind::Ax, SourceState::Unavailable);
    let original = ax_loss.health[0].clone();
    mailbox.publish(ax_loss);
    mailbox.publish(health_report(3, SourceKind::Ocr, SourceState::Unavailable));
    mailbox.publish(health_report(4, SourceKind::Ocr, SourceState::Available));
    let reports = mailbox.take();
    assert_eq!(
        reports.iter().map(|r| r.sequence).collect::<Vec<_>>(),
        [2, 3, 4]
    );
    assert_eq!(
        reports[0].health,
        [original],
        "loss keeps its real stamp and time"
    );
    assert_eq!(reports[1].health[0].state, SourceState::Unavailable);
    assert_eq!(reports[2].health[0].state, SourceState::Available);
    assert_eq!(mailbox.discarded, 0, "health coalescing is not a pixel gap");
}

#[test]
fn mailbox_keeps_embedded_degraded_ax_when_a_new_snapshot_overwrites_it() {
    let snapshot = |sequence, complete| {
        serde_json::from_value::<StructureSnapshot>(json!({
            "snapshot_id":format!("snapshot-{sequence}"), "session_epoch":1,
            "surface":{"id":"main","epoch":1}, "clock_domain":"macos/1/wall",
            "source_sequence":sequence, "observed_ms":sequence * 100,
            "app":{"bundle_id":"test.app","name":"Test","pid":42},
            "windows":[],
            "global_focus":{"app":{"state":"unavailable"},"window_id":{"state":"unavailable"}},
            "complete":complete,
            "incomplete_reason":null, "objects":[],
            "observer_coverage":{"state":"unavailable","covered_objects":0,
                "read_scoped_objects":0,"failed_registrations":0,"failures":[]},
            "menu_anchors":[], "menu_context":null
        }))
        .unwrap()
    };
    let mut mailbox = Mailbox::default();
    let mut degraded = health_report(2, SourceKind::Ax, SourceState::Degraded);
    degraded.snapshot = Some(snapshot(2, false));
    mailbox.publish(degraded);
    let mut recovered = health_report(3, SourceKind::Ax, SourceState::Available);
    recovered.snapshot = Some(snapshot(3, true));
    mailbox.publish(recovered);
    let reports = mailbox.take();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].sequence, 2);
    assert_eq!(reports[0].health[0].state, SourceState::Degraded);
    assert!(reports[0].snapshot.is_none());
    assert_eq!(reports[1].sequence, 3);
    assert_eq!(reports[1].health.len(), 1, "no duplicated recovery stamp");
    assert!(reports[1].snapshot.as_ref().unwrap().complete);
    assert_eq!(mailbox.discarded, 1);
}

#[test]
fn mailbox_keeps_embedded_frame_loss_and_merges_each_record_once() {
    let mut mailbox = Mailbox::default();
    mailbox.publish(frame_report(1, FrameStatus::Complete));
    let mut blank = frame_report(2, FrameStatus::Blank);
    blank.health = health_report(2, SourceKind::Screenshot, SourceState::Unavailable).health;
    blank
        .health
        .extend(health_report(2, SourceKind::Ocr, SourceState::Degraded).health);
    mailbox.publish(blank);
    let mut idle = frame_report(3, FrameStatus::Idle);
    idle.health = health_report(3, SourceKind::Screenshot, SourceState::Available).health;
    mailbox.publish(idle);
    let reports = mailbox.take();
    assert_eq!(
        reports.iter().map(|r| r.sequence).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        reports[1].health.len(),
        2,
        "both embedded losses survive once"
    );
    assert!(
        reports[1]
            .health
            .iter()
            .all(|health| health.state != SourceState::Available)
    );
    assert_eq!(reports[2].health.len(), 1);
    assert_eq!(
        reports[2].frame_meta.as_ref().unwrap().status,
        FrameStatus::Idle
    );
    assert_eq!(
        reports[2]
            .frame_meta
            .as_ref()
            .unwrap()
            .stream_info()
            .unwrap()
            .dropped_frames,
        0
    );
}

#[test]
fn healthy_idle_and_available_health_coalescing_never_create_a_pixel_gap() {
    let mut mailbox = Mailbox::default();
    for sequence in 1..=20 {
        let mut report = frame_report(
            sequence,
            if sequence == 1 {
                FrameStatus::Complete
            } else {
                FrameStatus::Idle
            },
        );
        report.health =
            health_report(sequence, SourceKind::Screenshot, SourceState::Available).health;
        mailbox.publish(report);
    }
    let reports = mailbox.take();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].sequence, 1);
    assert_eq!(reports[1].sequence, 20);
    assert_eq!(reports[1].health.len(), 1);
    assert_eq!(reports[1].health[0].state, SourceState::Available);
    for report in reports {
        assert_eq!(
            report
                .frame_meta
                .unwrap()
                .stream_info()
                .unwrap()
                .clone()
                .dropped_frames,
            0
        );
    }
    assert_eq!(mailbox.discarded, 0);
}

#[test]
fn mailbox_retains_the_latest_loss_floor_and_final_state_with_bounded_slots() {
    let mut mailbox = Mailbox::default();
    for sequence in 1..=1_000 {
        mailbox.publish(health_report(
            sequence,
            SourceKind::Ax,
            if sequence % 2 == 0 {
                SourceState::Available
            } else {
                SourceState::Unavailable
            },
        ));
    }
    let reports = mailbox.take();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].sequence, 999);
    assert_eq!(reports[0].health[0].state, SourceState::Unavailable);
    assert_eq!(reports[1].sequence, 1_000);
    assert_eq!(reports[1].health[0].state, SourceState::Available);
    assert_eq!(mailbox.discarded, 0);
    mailbox.publish(health_report(1_001, SourceKind::Ax, SourceState::Available));
    mailbox.publish(health_report(
        1_002,
        SourceKind::Ax,
        SourceState::Unavailable,
    ));
    let reports = mailbox.take();
    assert_eq!(
        reports.len(),
        1,
        "latest failed state is emitted once, never as recovery"
    );
    assert_eq!(reports[0].sequence, 1_002);
}

fn mock(script: &str) -> (tempfile::TempDir, NativeConfig) {
    let directory = tempfile::tempdir().unwrap();
    let helper = directory.path().join("helper");
    fs::write(
        &helper,
        format!("#!/bin/sh\nIFS= read -r request || :\n{script}\n"),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let config = NativeConfig {
        helper_path: helper,
        bundle_id: "test.app".into(),
        pid: Some(42),
        assets_dir: directory.path().join("assets"),
        timeout_ms: 100,
        mode: crate::config::NativeCaptureMode::Watch,
        table_scope: None,
        background_target: None,
        text_policy: None,
        owned_document_bootstrap: None,
    };
    (directory, config)
}

fn await_end(watch: &NativeWatch) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while !watch.ended() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn private_stderr_flood_does_not_block_the_helper_or_become_public_evidence() {
    let _lane = crate::native::HELPER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let marker = "private-only AX anchor retirement cause";
    let script = format!(
        "printf '%s\\n' '{marker}' >&2\nn=0\nwhile [ \"$n\" -lt 128 ]; do printf '%2048s' '' >&2; n=$((n+1)); done\nprintf '%s\\n' 'bad json'"
    );
    let (_directory, config) = mock(&script);
    let watch = NativeWatch::new(
        config,
        1,
        "macos/1/wall".into(),
        SharedClock::starting_now(),
        250,
    )
    .unwrap();
    await_end(&watch);
    let reports = watch.take_reports();
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0].snapshot.is_none()
            && reports[0].frame_meta.is_none()
            && reports[0].image.is_none()
    );
    for health in &reports[0].health {
        assert_eq!(health.state, SourceState::Unavailable);
        assert!(
            health
                .reason
                .as_ref()
                .unwrap()
                .contains("invalid native NDJSON record"),
            "the stdout record arrives after a stderr flood, without waiting for the watchdog"
        );
        assert!(!health.reason.as_ref().unwrap().contains(marker));
    }
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(watch.run_dir.join(DIAGNOSTICS_FILE)).unwrap()).unwrap();
    assert!(record["raw_stderr"].as_str().unwrap().starts_with(marker));
    assert_eq!(record["retained_bytes"], MAX_DIAGNOSTICS);
    assert_eq!(record["drained_bytes"], marker.len() + 1 + 128 * 2048);
    assert_eq!(record["complete"], true);
    assert_eq!(record["truncated"], true);
    let run_dir = watch.run_dir.clone();
    drop(watch);
    assert!(
        !run_dir.exists(),
        "this generation's diagnostics are removed with its other owned assets"
    );
}

#[test]
fn private_diagnostic_io_failure_does_not_change_source_health_or_accept_stderr_protocol() {
    let _lane = crate::native::HELPER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let forged = json!({"kind":"health", "source":"ax", "state":"available", "reason":"private-forged-authority"}).to_string();
    for block_diagnostics in [false, true] {
        let blocker = if block_diagnostics {
            // This diagnostic-only fixture reads its owned output_dir from
            // the trusted request and plants a refused temp-file symlink.
            "assets=${request#*\"\\\"output_dir\\\":\\\"\"}; assets=${assets%%\"\\\"\"*}; ln -s /dev/null \"$assets/native-diagnostics.json.tmp\"\n"
        } else {
            ""
        };
        let script = format!("{blocker}printf '%s\\n' '{forged}' >&2\nprintf '%s\\n' 'bad json'");
        let (_directory, config) = mock(&script);
        let watch = NativeWatch::new(
            config,
            1,
            "macos/1/wall".into(),
            SharedClock::starting_now(),
            250,
        )
        .unwrap();
        await_end(&watch);
        let reports = watch.take_reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].health.len(), 4);
        assert!(
            reports[0].snapshot.is_none()
                && reports[0].frame_meta.is_none()
                && reports[0].image.is_none()
        );
        for health in &reports[0].health {
            assert_eq!(health.state, SourceState::Unavailable);
            assert!(
                health
                    .reason
                    .as_ref()
                    .unwrap()
                    .contains("invalid native NDJSON record")
            );
            assert!(
                !health
                    .reason
                    .as_ref()
                    .unwrap()
                    .contains("private-forged-authority")
            );
            assert!(!health.reason.as_ref().unwrap().contains("diagnostic"));
        }
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(watch.run_dir.join(DIAGNOSTICS_FILE)).unwrap())
                .unwrap();
        if block_diagnostics {
            assert!(
                fs::symlink_metadata(watch.run_dir.join(DIAGNOSTICS_TEMP))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                record["raw_stderr"], "",
                "failed publication preserves the previous complete JSON snapshot"
            );
        } else {
            assert!(
                record["raw_stderr"]
                    .as_str()
                    .unwrap()
                    .contains("private-forged-authority")
            );
        }
    }
}

#[test]
fn malformed_and_closed_helper_channels_publish_unavailable() {
    let _lane = crate::native::HELPER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for script in ["printf '%s\\n' 'bad json'", "exit 0"] {
        let (_directory, config) = mock(script);
        let watch = NativeWatch::new(
            config,
            1,
            "macos/1/wall".into(),
            SharedClock::starting_now(),
            250,
        )
        .unwrap();
        await_end(&watch);
        let reports = watch.take_reports();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].health.iter().all(|health| health.state
            == interaction_contracts::desktop::source::SourceState::Unavailable));
    }
}

#[test]
fn helper_eof_preserves_specific_native_failure_and_protocol_errors_take_precedence() {
    let _lane = crate::native::HELPER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for mode in ["health", "gap", "protocol"] {
        let clock = SharedClock::starting_now();
        let mut records = Vec::new();
        if mode == "gap" {
            records.push(json!({"kind":"gap", "dropped_frames":0,
                "reason":"AX privacy invalidated the captured surface"}));
        }
        records.push(
            json!({"kind":"health", "source":"screenshot", "state":"unavailable",
            "reason":"AX privacy unsafe"}),
        );
        records.push(
            json!({"kind":"health", "source":"screenshot", "state":"unavailable",
            "reason":"watch stopped"}),
        );
        let mut lines = Vec::new();
        for (index, mut record) in records.into_iter().enumerate() {
            record["protocol_version"] = json!(1);
            record["stream_id"] = json!("WATCH_ID");
            record["sequence"] = json!(index + 1);
            record["received_uptime_ns"] = json!(1_000_000);
            record["received_unix_ms"] = json!(clock.origin_unix_ms());
            lines.push(serde_json::to_string(&record).unwrap());
        }
        if mode == "protocol" {
            lines.push("malformed NDJSON".into());
        }
        // Shell builtins avoid a cold interpreter startup consuming the
        // watchdog budget; timestamps were fixed before launching this fixture.
        let arguments = lines
            .iter()
            .map(|line| {
                format!(
                    "\"{}\"",
                    line.replace('\\', "\\\\")
                        .replace('\"', "\\\"")
                        .replace("WATCH_ID", "$watch")
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let script = format!(
            r#"rest=${{request#*\"watch_id\":\"}}
watch=${{rest%%\"*}}
printf '%s\n' {arguments}
"#
        );
        let (_directory, config) = mock(&script);
        let watch = NativeWatch::new(config, 1, "macos/1/wall".into(), clock, 250).unwrap();
        await_end(&watch);
        let reports = watch.take_reports();
        assert_eq!(reports.len(), 1, "terminal state replaces pending statuses");
        assert_eq!(reports[0].health.len(), 4);
        let expected = match mode {
            "gap" => "AX privacy invalidated the captured surface",
            "protocol" => "invalid native NDJSON record",
            _ => "AX privacy unsafe",
        };
        for health in &reports[0].health {
            assert_eq!(health.state, SourceState::Unavailable);
            assert!(
                health.reason.as_ref().unwrap().contains(expected),
                "{health:?}"
            );
            assert!(
                health.provenance.is_some(),
                "native failure record was admitted"
            );
        }
    }
}

#[test]
fn a_frozen_helper_is_killed_and_drop_cancels_without_waiting_for_watchdog() {
    let _lane = crate::native::HELPER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let (_directory, config) = mock("exec /bin/sleep 10");
    let watch = NativeWatch::new(
        config.clone(),
        1,
        "macos/1/wall".into(),
        SharedClock::starting_now(),
        250,
    )
    .unwrap();
    await_end(&watch);
    assert!(watch.take_reports()[0].health.iter().any(|health| {
        health
            .reason
            .as_ref()
            .is_some_and(|reason| reason.contains("deadline"))
    }));
    let watch = NativeWatch::new(
        config,
        1,
        "macos/1/wall".into(),
        SharedClock::starting_now(),
        250,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let start = Instant::now();
    drop(watch);
    assert!(start.elapsed() < Duration::from_secs(1));
}
