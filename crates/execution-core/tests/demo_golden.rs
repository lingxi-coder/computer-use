//! The I0 demo must keep printing exactly what the original prototype printed.

use std::process::Command;

#[test]
fn i0_demo_output_matches_the_original_prototype_byte_for_byte() {
    let output = Command::new(env!("CARGO_BIN_EXE_i0-demo"))
        .output()
        .expect("i0-demo must start");
    assert!(
        output.status.success(),
        "i0-demo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let golden = include_str!("../../../contracts/i0-v1/i0-demo.golden.json");
    assert_eq!(
        String::from_utf8(output.stdout).expect("demo output is UTF-8"),
        golden,
        "the executor no longer reproduces contracts/i0-v1/i0-demo.golden.json"
    );
}
