//! Prints the published desktop v2 artifacts.
//!
//! `export-desktop schema` writes the JSON Schema of the envelope and
//! `export-desktop fixtures` writes the golden fixtures. Redirect them into
//! `contracts/desktop-v2/` after changing any wire type.

use std::io::{self, Write};

use interaction_contracts::desktop::{message::Envelope, samples};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = io::stdout().lock();
    match std::env::args().nth(1).as_deref() {
        Some("schema") => {
            let mut schema = schemars::schema_for!(Envelope);
            if let Some(object) = schema.as_object_mut() {
                object.sort_keys();
                object
                    .values_mut()
                    .for_each(serde_json::Value::sort_all_objects);
            }
            serde_json::to_writer_pretty(&mut stdout, &schema)?;
        }
        Some("fixtures") => {
            let mut fixtures = samples::fixtures();
            for fixture in &mut fixtures {
                fixture.message.sort_all_objects();
            }
            serde_json::to_writer_pretty(&mut stdout, &fixtures)?;
        }
        Some("background-contexts") => {
            use interaction_contracts::desktop::source::{
                KernelProcessIdentity, ObservationFact, Rect, SourceBackgroundWindowContext,
                SourceWindowContext,
            };
            let frames = [
                ("integer", [0.0, 120.0, 360.0, 180.0]),
                ("fractional", [-13.5, 5.125, 360.75, 180.25]),
                ("negative_zero", [-0.0, 0.0, 360.0, 180.0]),
                ("small_exponent", [1e-6, -1e-6, 360.0, 180.0]),
                ("small_decimal", [0.00001, -0.00001, 360.0, 180.0]),
                ("small_size", [0.0, 0.0, 1e-10, 1e-10]),
                ("large_fraction", [-65535.5, 65535.25, 1024.5, 768.75]),
            ];
            let mut cases = Vec::new();
            for (name, [x, y, width, height]) in frames {
                let bundle_id = "com.lingxi.computer-user.guard-fixture";
                let pid = 700;
                let mut source = SourceBackgroundWindowContext {
                    window: SourceWindowContext {
                        source_session_id: "source-session".into(),
                        session_epoch: 7,
                        process: KernelProcessIdentity {
                            pid,
                            start_seconds: 100,
                            start_micros: 20,
                        },
                        origin_window_id: "window-42".into(),
                        origin_window_frame: Rect {
                            x,
                            y,
                            width,
                            height,
                        },
                        verified_source_sequence: 3,
                        verified_observed_ms: 1_000,
                    },
                    hidden: ObservationFact::Known { value: false },
                    minimized: ObservationFact::Known { value: false },
                    target_context_hash: String::new(),
                };
                source.target_context_hash =
                    source.computed_target_context_hash_for(bundle_id, pid)?;
                source.validate_identity(bundle_id, pid)?;
                cases.push(serde_json::json!({"name":name,"bundle_id":bundle_id,"pid":pid,"source":source}));
            }
            serde_json::to_writer_pretty(&mut stdout, &cases)?;
        }
        _ => {
            eprintln!("usage: export-desktop <schema|fixtures|background-contexts>");
            std::process::exit(2);
        }
    }
    writeln!(stdout)?;
    Ok(())
}
