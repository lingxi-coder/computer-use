//! Preserve the I0 scenarios while applying the current Unknown isolation rule.

use std::process::Command;

#[test]
fn i0_demo_preserves_the_prototype_except_for_unknown_input_isolation() {
    let output = Command::new(env!("CARGO_BIN_EXE_i0-demo"))
        .output()
        .expect("i0-demo must start");
    assert!(
        output.status.success(),
        "i0-demo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The historical prototype remains immutable. A lost acknowledgement now
    // isolates further input even when held-input cleanup succeeds.
    let mut expected: serde_json::Value =
        serde_json::from_str(include_str!("../../../contracts/i0-v1/i0-demo.golden.json"))
            .expect("the archived prototype is valid JSON");
    let unknown = expected["scenarios"]
        .as_array_mut()
        .expect("prototype scenarios")
        .iter_mut()
        .find(|scenario| scenario["scenario"] == "lost_ack_preserves_unknown_and_cleans_inputs")
        .expect("the lost acknowledgement scenario exists");
    assert_eq!(unknown["isolated"], false);
    assert_eq!(unknown["receipt"]["status"], "unknown");
    unknown["isolated"] = true.into();
    let golden = serde_json::to_string_pretty(&expected).unwrap() + "\n";
    assert_eq!(
        String::from_utf8(output.stdout).expect("demo output is UTF-8"),
        golden,
        "the executor changed a prototype scenario beyond the Unknown isolation rule"
    );
}
