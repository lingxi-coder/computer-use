//! The envelope and the closed set of messages that cross a process boundary.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::PROTOCOL_VERSION;
use super::decision::{DecisionCancel, DecisionProposal, DecisionRequest};
use super::device::{
    CapabilityManifest, CommandAck, DependencyUpdate, DependencyVersions, DeviceEvent,
    GrantOutcome, GrantPermit, LeaseRenew, Observation, QueryReceipt, Receipt, RenewPermit,
    ResumeAck, ResumeCommand, RevokeExecution, RootGrant, SimControl, StopAck, StopCommand,
    SubmitIntent,
};
use super::handshake::{Configure, Hello, HelloAck, Ready, Shutdown};
use super::ids::{MessageId, ProducerId};
use super::operator::{ConfirmationRequest, ConfirmationResult, OperatorCommand, UiProjection};
use super::require_nonempty;
use super::source::{FrameMeta, SourceCommand, SourceHealth, StructureSnapshot};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Message {
    // Lifecycle and handshake.
    Hello(Hello),
    HelloAck(HelloAck),
    Ready(Ready),
    Configure(Configure),
    Shutdown(Shutdown),
    LeaseRenew(LeaseRenew),
    // Operator surface.
    OperatorCommand(OperatorCommand),
    UiProjection(UiProjection),
    ConfirmationRequest(ConfirmationRequest),
    ConfirmationResult(ConfirmationResult),
    // Execution.
    RegisterRoot(RootGrant),
    GrantPermit(GrantPermit),
    GrantOutcome(GrantOutcome),
    Observation(Observation),
    SubmitIntent(SubmitIntent),
    Receipt(Receipt),
    QueryReceipt(QueryReceipt),
    RenewPermit(RenewPermit),
    RevokeExecution(RevokeExecution),
    StopCommand(StopCommand),
    StopAck(StopAck),
    ResumeCommand(ResumeCommand),
    ResumeAck(ResumeAck),
    DependencyUpdate(DependencyUpdate),
    DependencyVersions(DependencyVersions),
    DeviceEvent(DeviceEvent),
    CapabilityManifest(CapabilityManifest),
    CommandAck(CommandAck),
    /// Accepted only by the simulated backend.
    SimControl(SimControl),
    // Sources.
    StructureSnapshot(StructureSnapshot),
    SourceHealth(SourceHealth),
    FrameMeta(FrameMeta),
    SourceCommand(SourceCommand),
    // Decision worker relay.
    DecisionRequest(DecisionRequest),
    DecisionProposal(DecisionProposal),
    DecisionCancel(DecisionCancel),
}

impl Message {
    /// The wire name, useful in logs and assertions.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::HelloAck(_) => "hello_ack",
            Self::Ready(_) => "ready",
            Self::Configure(_) => "configure",
            Self::Shutdown(_) => "shutdown",
            Self::LeaseRenew(_) => "lease_renew",
            Self::OperatorCommand(_) => "operator_command",
            Self::UiProjection(_) => "ui_projection",
            Self::ConfirmationRequest(_) => "confirmation_request",
            Self::ConfirmationResult(_) => "confirmation_result",
            Self::RegisterRoot(_) => "register_root",
            Self::GrantPermit(_) => "grant_permit",
            Self::GrantOutcome(_) => "grant_outcome",
            Self::Observation(_) => "observation",
            Self::SubmitIntent(_) => "submit_intent",
            Self::Receipt(_) => "receipt",
            Self::QueryReceipt(_) => "query_receipt",
            Self::RenewPermit(_) => "renew_permit",
            Self::RevokeExecution(_) => "revoke_execution",
            Self::StopCommand(_) => "stop_command",
            Self::StopAck(_) => "stop_ack",
            Self::ResumeCommand(_) => "resume_command",
            Self::ResumeAck(_) => "resume_ack",
            Self::DependencyUpdate(_) => "dependency_update",
            Self::DependencyVersions(_) => "dependency_versions",
            Self::DeviceEvent(_) => "device_event",
            Self::CapabilityManifest(_) => "capability_manifest",
            Self::CommandAck(_) => "command_ack",
            Self::SimControl(_) => "sim_control",
            Self::StructureSnapshot(_) => "structure_snapshot",
            Self::SourceHealth(_) => "source_health",
            Self::FrameMeta(_) => "frame_meta",
            Self::SourceCommand(_) => "source_command",
            Self::DecisionRequest(_) => "decision_request",
            Self::DecisionProposal(_) => "decision_proposal",
            Self::DecisionCancel(_) => "decision_cancel",
        }
    }

    /// Message-local invariants that need no state and no clock.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Hello(hello) => hello.validate(),
            Self::Observation(observation) => observation.validate(),
            Self::RegisterRoot(root) => root.validate(),
            Self::GrantPermit(request) => request.validate(),
            Self::SubmitIntent(request) => request.intent.validate(),
            Self::Receipt(receipt) => receipt.validate(),
            Self::StopCommand(stop) => stop.validate(),
            Self::StructureSnapshot(snapshot) => snapshot.validate(),
            Self::DecisionRequest(request) => request.validate(),
            Self::LeaseRenew(lease) if lease.valid_for_ms == 0 => {
                Err("valid_for_ms must be positive".into())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    #[schemars(range(min = 1, max = 1))]
    pub protocol_version: u16,
    pub message_id: MessageId,
    /// The request this answers, when it answers one.
    pub in_reply_to: Option<MessageId>,
    pub producer: ProducerId,
    /// Per producer, strictly increasing. A gap means messages were lost.
    pub producer_sequence: u64,
    pub emitted_ms: u64,
    #[schemars(length(min = 1))]
    pub trace_id: String,
    pub message: Message,
}

impl Envelope {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(format!(
                "unsupported protocol_version {}; expected {PROTOCOL_VERSION}",
                self.protocol_version
            ));
        }
        require_nonempty("trace_id", &self.trace_id)?;
        self.message.validate()
    }
}

/// Decode strict JSON and reject unsupported versions or invalid local values.
pub fn decode_envelope(bytes: &[u8]) -> Result<Envelope, String> {
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    envelope.validate()?;
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::samples;

    #[test]
    fn every_sample_is_valid_and_round_trips_through_strict_json() {
        for (name, envelope) in samples::all() {
            envelope
                .validate()
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            let json = serde_json::to_vec(&envelope).unwrap();
            let decoded = decode_envelope(&json).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(decoded, envelope, "{name} changed on a round trip");
        }
    }

    #[test]
    fn the_samples_cover_every_message_type() {
        let mut names: Vec<&str> = samples::all()
            .iter()
            .map(|(_, envelope)| envelope.message.name())
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 36, "one sample per message type: {names:?}");
    }

    #[test]
    fn unknown_message_types_and_versions_are_rejected() {
        let mut value =
            serde_json::to_value(samples::envelope_for(samples::stop_command())).unwrap();
        value["message"]["type"] = serde_json::json!("format_disk");
        assert!(decode_envelope(value.to_string().as_bytes()).is_err());
        let mut value =
            serde_json::to_value(samples::envelope_for(samples::stop_command())).unwrap();
        value["protocol_version"] = serde_json::json!(2);
        assert!(decode_envelope(value.to_string().as_bytes()).is_err());
    }
}
