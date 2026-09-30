use std::io::{self, Write};

use interaction_contracts::i0::WireMessage;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = schemars::schema_for!(WireMessage);
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &schema)?;
    writeln!(stdout)?;
    Ok(())
}
