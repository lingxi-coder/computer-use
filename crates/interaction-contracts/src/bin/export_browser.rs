//! Schema and synthetic goldens for the isolated browser contract and private control channel.
use interaction_contracts::desktop::browser::{
    BrowserCheckboxBinding, BrowserCheckboxWitness, BrowserPageProfile, BrowserPageSnapshot,
    BrowserReadScope, BrowserRefreshRequest, BrowserSourceControl,
    browser_contract_golden_snapshots, derive_browser_checkbox_binding,
};
use interaction_contracts::desktop::browser_control::{
    BrowserControlAuthority, BrowserControlBootstrap, BrowserControlGoldenFrames,
    BrowserControlHello, BrowserControlOutcome, BrowserControlReady, BrowserControlReply,
    BrowserControlSubmit, BrowserControlToken, browser_control_contract_golden_frames,
};
use interaction_contracts::desktop::browser_execution::{
    BrowserAction, BrowserActionIntent, BrowserExecutionObservation,
};
use interaction_contracts::desktop::device::{
    ExecutionIntent, ExecutionObservation, SubmittedAction,
};
use interaction_contracts::desktop::message::Envelope;
use schemars::JsonSchema;
use std::io::{self, Read, Write};

#[derive(JsonSchema)]
#[allow(dead_code)]
struct BrowserWireContract {
    page_profile: BrowserPageProfile,
    snapshot: BrowserPageSnapshot,
    binding: BrowserCheckboxBinding,
    witness: BrowserCheckboxWitness,
    scope: BrowserReadScope,
    source_control: BrowserSourceControl,
    refresh_request: BrowserRefreshRequest,
    control_token: BrowserControlToken,
    control_bootstrap: BrowserControlBootstrap,
    control_hello: BrowserControlHello,
    control_ready: BrowserControlReady,
    control_authority: BrowserControlAuthority,
    control_submit: BrowserControlSubmit,
    control_outcome: BrowserControlOutcome,
    control_reply: BrowserControlReply,
    control_goldens: BrowserControlGoldenFrames,
    browser_action: BrowserAction,
    browser_execution_observation: BrowserExecutionObservation,
    browser_action_intent: BrowserActionIntent,
    execution_observation: ExecutionObservation,
    execution_intent: ExecutionIntent,
    submitted_action: SubmittedAction,
    public_envelope: Envelope,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("validate-snapshot") {
        let path = std::env::args().nth(2).ok_or("snapshot path is required")?;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("snapshot exceeds its byte bound".into());
        }
        let snapshot: BrowserPageSnapshot = serde_json::from_slice(&bytes)?;
        snapshot.validate().map_err(io::Error::other)?;
        let binding_derived =
            derive_browser_checkbox_binding(&snapshot, "Background option").is_ok();
        let result = serde_json::json!({
            "snapshot_validated": true,
            "node_count": snapshot.nodes.len(),
            "target_context_hash": snapshot.target_context_hash,
            "binding_derived": binding_derived,
            "execution_authorized": false,
        });
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &result)?;
        writeln!(stdout)?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("goldens") {
        let snapshots = browser_contract_golden_snapshots().map_err(io::Error::other)?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &snapshots)?;
        writeln!(stdout)?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("control-goldens") {
        let frames = browser_control_contract_golden_frames().map_err(io::Error::other)?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &frames)?;
        writeln!(stdout)?;
        return Ok(());
    }
    let mut schema = schemars::schema_for!(BrowserWireContract);
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
