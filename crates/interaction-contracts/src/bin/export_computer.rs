//! Prints the schema for the private computer-use wire contract.
//!
//! The wrapper collects the two top-level DTOs without adding them to the
//! desktop v1 message envelope.

use std::io::{self, Write};

use interaction_contracts::desktop::computer::{ComputerBatchProposal, ComputerFrameRequest};
use schemars::JsonSchema;

#[derive(JsonSchema)]
#[allow(dead_code)]
struct ComputerWireContract {
    frame_request: ComputerFrameRequest,
    batch_proposal: ComputerBatchProposal,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut schema = schemars::schema_for!(ComputerWireContract);
    if let Some(object) = schema.as_object_mut() {
        object.sort_keys();
        object
            .values_mut()
            .for_each(serde_json::Value::sort_all_objects);
    }
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &schema)?;
    writeln!(stdout)?;
    Ok(())
}
