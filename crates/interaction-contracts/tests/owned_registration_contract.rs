use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use serde_json::{Value, json};

fn payload() -> Value {
    json!({
        "version":1,"registration_id":"00000000-0000-4000-8000-000000000001",
        "pid":42,"start_seconds":1_700_000_000,"start_micros":100,
        "executable_path":"/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
        "bundle_id":"com.apple.TextEdit","document_path":"/private/var/folders/owned/document.txt",
        "document_sha256":"a".repeat(64),"document_dev":1,"document_ino":2,
        "directory_dev":1,"directory_ino":3,"issued_unix_ms":10_000
    })
}

#[test]
fn private_registration_has_exact_fields_and_redacted_debug() {
    let value = payload();
    let registration: OwnedDocumentRegistration = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&registration).unwrap(), value);
    assert!(registration.validate_at(10_100).is_ok());
    let debug = format!("{registration:?}");
    assert!(!debug.contains("/private/var"));
    assert!(!debug.contains(&"a".repeat(64)));
    assert!(!debug.contains(&registration.registration_id));
    let mut extra = value.clone();
    extra["input_grant"] = json!(true);
    assert!(serde_json::from_value::<OwnedDocumentRegistration>(extra).is_err());
    let mut missing = value.clone();
    missing.as_object_mut().unwrap().remove("directory_ino");
    assert!(serde_json::from_value::<OwnedDocumentRegistration>(missing).is_err());
    let text = serde_json::to_string(&value).unwrap();
    let duplicate = text.replacen("\"pid\":42", "\"pid\":42,\"pid\":43", 1);
    assert!(serde_json::from_str::<OwnedDocumentRegistration>(&duplicate).is_err());
}

#[test]
fn startup_freshness_does_not_extend_the_source_proof_lifetime() {
    let mut registration: OwnedDocumentRegistration = serde_json::from_value(payload()).unwrap();
    assert!(registration.validate_at(9_000).is_ok());
    assert!(registration.validate_at(8_999).is_err());
    assert!(registration.validate_at(15_000).is_ok());
    assert!(registration.validate_at(15_001).is_err());
    // Runtime identity is retained; each Source evidence must independently
    // validate its own clock and expiry at every admission/access.
    assert!(registration.validate_identity().is_ok());
    registration.issued_unix_ms = 0;
    assert!(registration.validate_identity().is_err());
}

#[test]
fn identity_rejects_non_normalized_paths_and_foreign_process_shapes() {
    for path in [
        "relative/document.txt",
        "/private/var/../document.txt",
        "/private//var/document.txt",
        "/private/var/./document.txt",
        "/private/var/document.txt/",
        "/private/var/\n/document.txt",
    ] {
        let mut registration: OwnedDocumentRegistration =
            serde_json::from_value(payload()).unwrap();
        registration.document_path = path.into();
        assert!(
            registration.validate_identity().is_err(),
            "accepted non-normalized path"
        );
    }
    for (field, value) in [
        ("pid", json!(0)),
        ("start_micros", json!(1_000_000)),
        ("document_sha256", json!("invalid")),
        ("bundle_id", json!("foreign.app")),
        ("document_ino", json!(0)),
        ("directory_ino", json!(9_007_199_254_740_992_u64)),
    ] {
        let mut changed = payload();
        changed[field] = value;
        let registration: OwnedDocumentRegistration = serde_json::from_value(changed).unwrap();
        assert!(registration.validate_identity().is_err());
    }
}

#[test]
fn registration_does_not_become_a_public_envelope_payload() {
    let schema = serde_json::to_string(&schemars::schema_for!(
        interaction_contracts::desktop::message::Envelope
    ))
    .unwrap();
    assert!(!schema.contains("OwnedDocumentRegistration"));
    assert!(!schema.contains("issued_unix_ms"));
}
