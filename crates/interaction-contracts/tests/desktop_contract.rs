//! The published desktop v2 artifacts must match the Rust types exactly, and
//! every fixture must get the verdicts it declares from both the JSON Schema
//! and the decoder.

use std::process::Command;

use interaction_contracts::desktop::message::{Envelope, decode_envelope};
use interaction_contracts::desktop::samples::{self, Fixture};
use serde_json::Value;

const REGENERATE: &str = "regenerate with `cargo run -p interaction-contracts --bin export-desktop -- <schema|fixtures> > contracts/desktop-v2/<schema|fixtures>.json`";

fn published_schema() -> Value {
    serde_json::from_str(include_str!("../../../contracts/desktop-v2/schema.json"))
        .expect("published schema must be JSON")
}

fn published_fixtures() -> Vec<Fixture> {
    serde_json::from_str(include_str!("../../../contracts/desktop-v2/fixtures.json"))
        .expect("published fixtures must be well-formed")
}

#[test]
fn published_schema_matches_rust_types_and_export_command() {
    let published = published_schema();
    let derived = serde_json::to_value(schemars::schema_for!(Envelope)).unwrap();
    assert_eq!(published, derived, "{REGENERATE}");

    let output = Command::new(env!("CARGO_BIN_EXE_export-desktop"))
        .arg("schema")
        .output()
        .expect("schema exporter must start");
    assert!(output.status.success());
    assert_eq!(
        output.stdout.as_slice(),
        include_bytes!("../../../contracts/desktop-v2/schema.json"),
        "schema exporter must be independent of serde_json feature unification"
    );
    let exported: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(published, exported);
}

#[test]
fn published_fixtures_are_the_current_ones() {
    assert_eq!(published_fixtures(), samples::fixtures(), "{REGENERATE}");
    let output = Command::new(env!("CARGO_BIN_EXE_export-desktop"))
        .arg("fixtures")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout.as_slice(),
        include_bytes!("../../../contracts/desktop-v2/fixtures.json"),
        "fixture exporter must keep its published bytes"
    );
}

#[test]
fn fixtures_get_the_declared_verdicts_from_schema_and_decoder() {
    let validator = jsonschema::validator_for(&published_schema()).expect("schema must compile");
    let mut valid = 0;
    let mut structural = 0;
    let mut semantic = 0;

    for fixture in published_fixtures() {
        let schema_result = validator.validate(&fixture.message);
        assert_eq!(
            schema_result.is_ok(),
            fixture.schema_valid,
            "{}: unexpected schema verdict: {schema_result:?}",
            fixture.name
        );
        let decoded = decode_envelope(fixture.message.to_string().as_bytes());
        assert_eq!(
            decoded.is_ok(),
            fixture.decode_valid,
            "{}: unexpected decode verdict: {decoded:?}",
            fixture.name
        );
        match (fixture.schema_valid, fixture.decode_valid) {
            (true, true) => {
                valid += 1;
                let round_trip = serde_json::to_value(decoded.unwrap()).unwrap();
                assert_eq!(
                    round_trip, fixture.message,
                    "{}: a valid message lost data on a round trip",
                    fixture.name
                );
            }
            (false, false) => structural += 1,
            (true, false) => semantic += 1,
            (false, true) => panic!(
                "{}: the schema rejects a message the decoder accepts",
                fixture.name
            ),
        }
    }
    assert_eq!(
        valid, 44,
        "36 canonical message samples, one typed scoped task, one frozen task, one activation variant, one scoped refresh, one browser refresh request, and three synthetic browser execution samples"
    );
    assert!(structural >= 10, "structural rejections are exercised");
    assert!(semantic >= 10, "decode-only invariants are exercised");
}

#[test]
fn every_message_type_is_described_by_the_schema() {
    let schema = published_schema();
    let text = schema.to_string();
    for (name, _) in samples::all() {
        assert!(
            text.contains(&format!("\"{name}\"")),
            "schema never mentions {name}"
        );
    }
}
