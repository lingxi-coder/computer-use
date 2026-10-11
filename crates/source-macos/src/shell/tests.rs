use super::*;
use desktop_ipc::handshake::{make_hello, read_ready, send_hello};
use interaction_contracts::desktop::handshake::Shutdown;
use interaction_contracts::desktop::source::{FrameMeta, FrameStatus};
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

fn complete(
    conn: &mut Connection,
) -> (
    FrameMeta,
    Vec<SourceHealth>,
    Vec<interaction_contracts::desktop::ids::MessageId>,
) {
    conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut health = Vec::new();
    let mut acknowledgements = Vec::new();
    loop {
        let envelope = conn
            .reader
            .read_next()
            .unwrap_or_else(|error| panic!("no COMPLETE: {error}; source health: {health:?}"))
            .expect("source remains connected");
        match envelope.message {
            Message::FrameMeta(meta) if meta.status == FrameStatus::Complete => {
                return (meta, health, acknowledgements);
            }
            Message::SourceHealth(status) => health.push(status),
            Message::CommandAck(ack) if ack.accepted => {
                acknowledgements.push(envelope.in_reply_to.expect("refresh reply is correlated"))
            }
            _ => {}
        }
    }
}

#[test]
fn explicit_refresh_replaces_an_active_watch_and_coalesces_a_burst() {
    let _lane = crate::native::HELPER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let directory = tempfile::Builder::new()
        .prefix("cu-refresh-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let clock = desktop_ipc::SharedClock::starting_now();
    let helper = directory.path().join("helper");
    let request_log = directory.path().join("watch-requests.jsonl");
    let request_log_path = request_log.display().to_string();
    let frame = json!({
        "protocol_version":1, "kind":"frame", "stream_id":"WATCH_ID", "sequence":1,
        "received_uptime_ns":1_000_000, "received_unix_ms":clock.origin_unix_ms(),
        "frame":{"status":"complete", "frame_id":"image-HELPER_PID", "window_id":"window-1", "pid":42,
            "display_time_ticks":1_000_000, "display_time_uptime_ns":1_000_000,
            "source_unix_ms":clock.origin_unix_ms(), "geometry_revision":1,
            "width_px":1, "height_px":1, "scale_factor":1.0, "content_scale":1.0,
            "content_rect":{"x":0.0,"y":0.0,"width":1.0,"height":1.0}, "dirty_rects":[],
            "frame":{"x":0.0,"y":0.0,"width":1.0,"height":1.0},
            "png_file":"capture.png", "byte_length":3}
    });
    let frame = serde_json::to_string(&frame)
        .unwrap()
        .replace('"', "\\\"")
        .replace("HELPER_PID", "$$")
        .replace("WATCH_ID", "$watch");
    // No interpreter startup or descendants: the shell writes one complete
    // record, then execs sleep so cancellation must kill and reap that PID.
    fs::write(
        &helper,
        format!(
            r#"#!/bin/sh
IFS= read -r request || :
printf '%s\n' "$request" >> '{request_log_path}'
watch_rest=${{request#*\"watch_id\":\"}}
watch=${{watch_rest%%\"*}}
rest=${{request#*\"output_dir\":\"}}
assets=${{rest%%\"*}}
printf '%s' png > "$assets/capture.png"
printf '%s\n' "{frame}"
exec /bin/sleep 10
"#
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let token = socket::new_token().unwrap();
    let assets = directory.path().join("assets");
    let config = SourceConfig {
        socket_dir: directory.path().into(),
        host_token: token.clone(),
        image_token: None,
        session_epoch: 1,
        clock,
        clock_domain: "macos/1/wall".into(),
        backend: SourceBackend::Native(Box::new(crate::config::NativeConfig {
            helper_path: helper,
            bundle_id: "com.lingxi.computer-user.mvp-fixture".into(),
            pid: Some(42),
            assets_dir: assets.clone(),
            timeout_ms: 1_500,
            mode: NativeCaptureMode::Watch,
            table_scope: None,
            background_target: None,
            text_policy: Some(crate::config::TextReadPolicy::RegisteredEditor {
                window_id: "window-1".into(),
            }),
            owned_document_bootstrap: None,
        })),
        // The fixture's capture timestamp is deliberately fixed at startup;
        // long-cadence source validation permits concurrent test scheduling.
        interval_ms: 5_000,
    };
    let source_socket = config.socket_path();
    let (supervisor_socket, source_supervisor) = UnixStream::pair().unwrap();
    let source = Connection::new(source_supervisor, "source", clock).unwrap();
    let worker = std::thread::spawn(move || run(config, source.link, source.reader));
    let mut supervisor = Connection::new(supervisor_socket, "supervisor", clock).unwrap();
    supervisor
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    read_ready(&mut supervisor).unwrap();
    let socket = socket::connect(&source_socket, Duration::from_secs(5)).unwrap();
    let mut host = Connection::new(socket, "host", clock).unwrap();
    send_hello(&mut host, make_hello(PeerRole::Host, &token, 1, clock)).unwrap();
    read_ready(&mut host).unwrap();
    let (first, _, initial_acks) = complete(&mut host);
    assert!(initial_acks.is_empty());
    let first_stream = first.stream_info().unwrap().clone();
    let old_pid = first_stream
        .image_frame_id
        .as_ref()
        .unwrap()
        .strip_prefix("image-")
        .unwrap();
    let mut latest = None;
    for _ in 0..32 {
        latest = Some(
            host.link
                .send(Message::SourceCommand(SourceCommand::RefreshNow {
                    table_scope: None,
                }))
                .unwrap(),
        );
    }
    let (second, transitions, acknowledgements) = complete(&mut host);
    assert_eq!(
        acknowledgements,
        vec![latest.unwrap()],
        "coalesced transition acknowledges only the latest request"
    );
    let fault = transitions
        .iter()
        .filter(|health| health.state == SourceState::Unavailable)
        .collect::<Vec<_>>();
    assert_eq!(
        fault.len(),
        4,
        "all source authority is closed before new pixels"
    );
    for health in fault {
        let stamp = health.provenance.as_ref().unwrap();
        assert_eq!(stamp.stream_id, first_stream.stream_id);
        assert!(stamp.source_sequence > first.source_sequence);
        assert!(health.reason.as_ref().unwrap().contains("explicit refresh"));
    }
    let second_stream = second.stream_info().unwrap().clone();
    assert_ne!(first_stream.stream_id, second_stream.stream_id);
    assert_ne!(first_stream.image_frame_id, second_stream.image_frame_id);
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", old_pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "old helper was reaped before new complete"
    );
    std::thread::sleep(Duration::from_millis(WATCH_REFRESH_COOLDOWN_MS + 100));
    host.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    loop {
        match host.reader.read_next() {
            Ok(Some(envelope)) => assert!(
                !matches!(envelope.message, Message::FrameMeta(_)),
                "the burst shares one refresh rather than launching repeated helpers"
            ),
            Err(desktop_ipc::link::ReadError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            other => panic!("unexpected source result: {other:?}"),
        }
    }
    assert_eq!(
        fs::read_dir(&assets)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .count(),
        1
    );

    let scope = interaction_contracts::desktop::task::RowTableScope {
        scroll_path: vec![interaction_contracts::desktop::task::RowContainerScope {
            role: interaction_contracts::desktop::task::RowContainerRole::AxScrollArea,
            label: "Archive".into(),
        }],
        column_headers: vec!["Name".into()],
    };
    let scoped_request = host
        .link
        .send(Message::SourceCommand(SourceCommand::RefreshNow {
            table_scope: Some(scope.clone()),
        }))
        .unwrap();
    let (scoped, _, scoped_acks) = complete(&mut host);
    assert_eq!(scoped_acks, vec![scoped_request]);
    let scoped_stream = scoped.stream_info().unwrap().clone();
    assert_ne!(second_stream.stream_id, scoped_stream.stream_id);
    assert_ne!(second_stream.image_frame_id, scoped_stream.image_frame_id);
    let scoped_pid = scoped_stream
        .image_frame_id
        .as_ref()
        .unwrap()
        .strip_prefix("image-")
        .unwrap();
    let requests: Vec<serde_json::Value> = fs::read_to_string(&request_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2]["table_scope"],
        serde_json::to_value(&scope).unwrap()
    );

    drop(host);
    let socket = socket::connect(&source_socket, Duration::from_secs(5)).unwrap();
    let mut host = Connection::new(socket, "host-reconnected", clock).unwrap();
    send_hello(&mut host, make_hello(PeerRole::Host, &token, 1, clock)).unwrap();
    read_ready(&mut host).unwrap();
    let (reconnected, _, reconnect_acks) = complete(&mut host);
    assert!(reconnect_acks.is_empty());
    let reconnected_stream = reconnected.stream_info().unwrap().clone();
    assert_ne!(scoped_stream.stream_id, reconnected_stream.stream_id);
    assert_ne!(
        scoped_stream.image_frame_id,
        reconnected_stream.image_frame_id
    );
    let requests: Vec<serde_json::Value> = fs::read_to_string(&request_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[3]["table_scope"].is_null(),
        "a reconnect starts from the configured watch scope, not the old task scope"
    );
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", scoped_pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "disconnect reaps the task-scoped helper before reconnect observations"
    );

    let scoped_again_request = host
        .link
        .send(Message::SourceCommand(SourceCommand::RefreshNow {
            table_scope: Some(scope.clone()),
        }))
        .unwrap();
    let (scoped_again, _, scoped_again_acks) = complete(&mut host);
    assert_eq!(scoped_again_acks, vec![scoped_again_request]);
    let scoped_again_stream = scoped_again.stream_info().unwrap().clone();
    assert_ne!(reconnected_stream.stream_id, scoped_again_stream.stream_id);
    let scoped_again_pid = scoped_again_stream
        .image_frame_id
        .as_ref()
        .unwrap()
        .strip_prefix("image-")
        .unwrap();
    let requests: Vec<serde_json::Value> = fs::read_to_string(&request_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests[4]["table_scope"],
        serde_json::to_value(&scope).unwrap()
    );

    let clear_request = host
        .link
        .send(Message::SourceCommand(SourceCommand::RefreshNow {
            table_scope: None,
        }))
        .unwrap();
    let (cleared, _, clear_acks) = complete(&mut host);
    assert_eq!(clear_acks, vec![clear_request]);
    let cleared_stream = cleared.stream_info().unwrap().clone();
    assert_ne!(scoped_again_stream.stream_id, cleared_stream.stream_id);
    assert_ne!(
        scoped_again_stream.image_frame_id,
        cleared_stream.image_frame_id
    );
    let requests: Vec<serde_json::Value> = fs::read_to_string(&request_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(requests.len(), 6);
    assert!(
        requests[5]["table_scope"].is_null(),
        "None clears the prior scope"
    );
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", scoped_again_pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "the scoped helper is reaped before the unscoped observation is forwarded"
    );
    std::thread::sleep(Duration::from_millis(WATCH_REFRESH_COOLDOWN_MS + 100));
    host.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    loop {
        match host.reader.read_next() {
            Ok(Some(envelope)) => assert!(
                !matches!(envelope.message, Message::FrameMeta(_)),
                "an observation from a retired scoped watch cannot arrive after its replacement"
            ),
            Err(desktop_ipc::link::ReadError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            other => panic!("unexpected source result: {other:?}"),
        }
    }
    supervisor
        .link
        .send(Message::Shutdown(Shutdown {
            reason: "test finished".into(),
        }))
        .unwrap();
    assert!(worker.join().unwrap().is_ok());
    assert_eq!(
        fs::read_dir(&assets)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .count(),
        0
    );
}
