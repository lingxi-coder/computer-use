use desktop_ipc::SharedClock;
use desktop_ipc::link::Link;
use interaction_contracts::desktop::handshake::{Configure, PeerRole};
use interaction_contracts::desktop::message::Message;
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn inherited_provider_keys_cannot_enable_an_unauthorized_host() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SharedClock::starting_now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cu-host"))
        .env("OPENAI_API_KEY", "ambient-test-key")
        .env("ANTHROPIC_API_KEY", "other-test-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut settings = BTreeMap::new();
    for (k, v) in [
        ("device_token", "0123456789abcdef0123456789abcdef"),
        ("source_token", "0123456789abcdef0123456789abcdef"),
        ("device_epoch", "1"),
        ("planning_backend", "model"),
    ] {
        settings.insert(k.into(), v.into());
    }
    settings.insert("clock_origin_ms".into(), clock.origin_unix_ms().to_string());
    settings.insert("socket_dir".into(), dir.path().to_string_lossy().into());
    let db = dir.path().join("must-not-exist.db");
    settings.insert("db_path".into(), db.to_string_lossy().into());
    let mut input = Link::new(child.stdin.take().unwrap(), "bootstrap-test", clock);
    input
        .send(Message::Configure(Configure {
            role: PeerRole::Host,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        }))
        .unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!db.exists());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("matching private authorization"));
    assert!(!stderr.contains("test-key"));
    assert!(output.stdout.is_empty());
}
#[test]
fn a_malformed_private_credential_is_never_echoed_to_logs() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cu-host"))
        .arg("--model-bootstrap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let bytes = b"{\"api_key\":\"never-echo-this-key\",\"route\":{}}";
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    input.write_all(bytes).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid private model authorization"));
    assert!(!stderr.contains("never-echo-this-key"));
    assert!(output.stdout.is_empty());
}
