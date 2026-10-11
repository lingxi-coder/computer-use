use desktop_ipc::SharedClock;
use desktop_ipc::link::{FramedReader, Link};
use interaction_contracts::desktop::device::CapabilityState;
use interaction_contracts::desktop::handshake::{Configure, PeerRole};
use interaction_contracts::desktop::message::Message;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn private_frame() -> Vec<u8> {
    let payload = serde_json::to_vec(&json!({
        "version": 1,
        "registration_id": "123e4567-e89b-42d3-a456-426614174000",
        "pid": 1234,
        "start_seconds": 1_700_000_000,
        "start_micros": 123456,
        "executable_path": "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
        "bundle_id": "com.apple.TextEdit",
        "document_path": "/private/tmp/owned/document.txt",
        "document_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "document_dev": 1,
        "document_ino": 2,
        "directory_dev": 3,
        "directory_ino": 4,
        "issued_unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64,
    }))
    .unwrap();
    let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
    bytes.extend(payload);
    bytes
}

fn observation_configure(socket_dir: String, clock: SharedClock) -> Configure {
    let settings = BTreeMap::from([
        ("socket_dir".into(), socket_dir),
        (
            "host_token".into(),
            "0123456789abcdef0123456789abcdef".into(),
        ),
        ("clock_origin_ms".into(), clock.origin_unix_ms().to_string()),
        ("world_log".into(), "/tmp/device-owned-test.log".into()),
        ("backend".into(), "macos-observe".into()),
    ]);
    Configure {
        role: PeerRole::Device,
        session_epoch: 1,
        settings,
        browser_page_profile: None,
    }
}

#[test]
fn real_cli_keeps_owned_registration_on_observe_and_reports_no_input_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SharedClock::starting_now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cu-device"))
        .arg("--owned-document-bootstrap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&private_frame()).unwrap();
    let mut link = Link::new(stdin, "owned-registration-test", clock);
    link.send(Message::Configure(observation_configure(
        dir.path().to_string_lossy().into_owned(),
        clock,
    )))
    .unwrap();
    drop(link);

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reader = FramedReader::new(std::io::Cursor::new(output.stdout));
    let ready = reader.read_next().unwrap().unwrap();
    let Message::Ready(ready) = ready.message else {
        panic!("cu-device did not report Ready")
    };
    let manifest = ready.capabilities.unwrap();
    assert_eq!(
        manifest.backend,
        interaction_contracts::desktop::device::BackendKind::Macos
    );
    assert_eq!(manifest.input, CapabilityState::Unavailable);
    assert_eq!(manifest.hit_test, CapabilityState::Unavailable);
    assert_eq!(manifest.human_input, CapabilityState::Unavailable);
}

#[test]
fn real_cli_reports_startup_errors_without_echoing_private_fields() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cu-device"))
        .arg("--model-bootstrap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unsupported startup arguments")
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_cu-device"))
        .arg("--owned-document-bootstrap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let secret = br#"{"document_path":"/private/customer-secret/document.txt","document_sha256":"private-hash"}"#;
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(&(secret.len() as u32).to_be_bytes())
        .unwrap();
    input.write_all(secret).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid private owned document registration"));
    assert!(!stderr.contains("customer-secret"));
    assert!(!stderr.contains("private-hash"));
}
