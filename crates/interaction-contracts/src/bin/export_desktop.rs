//! Prints the published desktop v1 artifacts.
//!
//! `export-desktop schema` writes the JSON Schema of the envelope and
//! `export-desktop fixtures` writes the golden fixtures. Redirect them into
//! `contracts/desktop-v1/` after changing any wire type.

use std::io::{self, Write};

use interaction_contracts::desktop::{message::Envelope, samples};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = io::stdout().lock();
    match std::env::args().nth(1).as_deref() {
        Some("schema") => {
            serde_json::to_writer_pretty(&mut stdout, &schemars::schema_for!(Envelope))?;
        }
        Some("fixtures") => {
            serde_json::to_writer_pretty(&mut stdout, &samples::fixtures())?;
        }
        _ => {
            eprintln!("usage: export-desktop <schema|fixtures>");
            std::process::exit(2);
        }
    }
    writeln!(stdout)?;
    Ok(())
}
