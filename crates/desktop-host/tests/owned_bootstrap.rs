use desktop_ipc::SharedClock;
use desktop_ipc::link::Link;
use interaction_contracts::desktop::handshake::{Configure, PeerRole};
use interaction_contracts::desktop::message::Message;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn private_frame(value: Value) -> Vec<u8> {
    let payload = serde_json::to_vec(&value).unwrap();
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend(payload);
    frame
}

fn owned_registration(pid: u32) -> Value {
    json!({
        "version": 1,
        "registration_id": "123e4567-e89b-42d3-a456-426614174000",
        "pid": pid,
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
    })
}

fn model_authorization() -> Value {
    json!({
        "purposes": ["s1_teacher"],
        "credential": { "kind": "api_key", "api_key": "private-test-key" },
        "route": {
            "provider_id": "open_ai",
            "protocol": "open_ai_chat",
            "base_url": "https://example.invalid/v1",
            "model": "test-model",
            "max_output_tokens": 4096
        }
    })
}

#[test]
fn the_real_cli_reads_owned_then_model_then_configure_and_rejects_identity_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SharedClock::starting_now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cu-host"))
        .args(["--owned-document-bootstrap", "--model-bootstrap"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdin = child.stdin.take().unwrap();
    let mut input = std::io::BufWriter::new(stdin);
    input
        .write_all(&private_frame(owned_registration(1234)))
        .unwrap();
    input
        .write_all(&private_frame(model_authorization()))
        .unwrap();

    let mut settings = BTreeMap::new();
    for (key, value) in [
        ("socket_dir", dir.path().to_string_lossy().into_owned()),
        (
            "db_path",
            dir.path().join("host.db").to_string_lossy().into_owned(),
        ),
        ("device_token", "0123456789abcdef0123456789abcdef".into()),
        ("device_epoch", "1".into()),
        ("source_token", "fedcba9876543210fedcba9876543210".into()),
        ("native_target_bundle", "com.apple.TextEdit".into()),
        ("native_target_pid", "4321".into()),
        ("initial_mode", "observe".into()),
    ] {
        settings.insert(key.into(), value);
    }
    settings.insert("clock_origin_ms".into(), clock.origin_unix_ms().to_string());
    let mut protocol = Link::new(input, "owned-bootstrap-test", clock);
    protocol
        .send(Message::Configure(Configure {
            role: PeerRole::Host,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        }))
        .unwrap();
    drop(protocol);

    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("owned document registration does not match observe configuration"));
    assert!(!stderr.contains("document.txt"));
    assert!(!stderr.contains("0123456789abcdef0123456789abcdef"));
    assert!(!stderr.contains("private-test-key"));
}

#[test]
fn the_real_cli_rejects_reversed_prefix_order_without_reading_stdin() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cu-host"))
        .args(["--model-bootstrap", "--owned-document-bootstrap"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"must-not-be-read")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unsupported startup arguments")
    );
}
