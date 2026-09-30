use std::process::Command;

use interaction_contracts::i0::{WireMessage, decode};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    name: String,
    schema_valid: bool,
    decode_valid: bool,
    message: Value,
}

fn published_schema() -> Value {
    serde_json::from_str(include_str!(
        "../../../contracts/i0-v1/interaction.schema.json"
    ))
    .expect("published schema must be JSON")
}

#[test]
fn published_schema_matches_rust_types_and_export_command() {
    let published = published_schema();
    let derived = serde_json::to_value(schemars::schema_for!(WireMessage)).unwrap();
    assert_eq!(
        published, derived,
        "regenerate contracts/interaction.schema.json"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_export-schema"))
        .output()
        .expect("schema exporter must start");
    assert!(
        output.status.success(),
        "schema exporter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exported: Value = serde_json::from_slice(&output.stdout)
        .expect("exporter stdout must contain only the JSON schema");
    assert_eq!(published, exported);
}

#[test]
fn complete_wire_fixtures_define_structural_and_semantic_boundaries() {
    let schema = published_schema();
    let validator = jsonschema::validator_for(&schema).expect("schema must compile");
    let fixtures: Vec<Fixture> =
        serde_json::from_str(include_str!("../../../contracts/i0-v1/fixtures.json"))
            .expect("wire fixtures must be well-formed");
    assert!(!fixtures.is_empty());

    let mut positive_cases = 0;
    let mut structural_failures = 0;
    let mut semantic_failures = 0;

    for fixture in fixtures {
        let schema_result = validator.validate(&fixture.message);
        assert_eq!(
            schema_result.is_ok(),
            fixture.schema_valid,
            "{}: unexpected schema result: {schema_result:?}",
            fixture.name
        );

        let decoded = decode(&fixture.message.to_string());
        assert_eq!(
            decoded.is_ok(),
            fixture.decode_valid,
            "{}: unexpected decode result: {decoded:?}",
            fixture.name
        );

        match (fixture.schema_valid, fixture.decode_valid) {
            (true, true) => {
                positive_cases += 1;
                assert_eq!(
                    serde_json::to_value(decoded.unwrap()).unwrap(),
                    fixture.message,
                    "{}: valid message lost data on round trip",
                    fixture.name
                );
            }
            (false, false) => structural_failures += 1,
            (true, false) => semantic_failures += 1,
            (false, true) => panic!("{}: schema rejects a decoded fixture", fixture.name),
        }
    }

    assert!(positive_cases > 0, "fixtures must include valid messages");
    assert!(
        structural_failures > 0,
        "fixtures must exercise structural rejection"
    );
    assert!(
        semantic_failures > 0,
        "fixtures must exercise decode-only invariants"
    );
}
