//! Between the engine's inputs and outputs and the wire messages.
//!
//! The engine numbers every command that needs an answer; the wire matches
//! answers by `in_reply_to`. The bridge keeps the two together, and is pure so
//! it can be tested without a process.

use std::collections::BTreeMap;

use interaction_contracts::desktop::device::CapabilityManifest;
use interaction_contracts::desktop::handshake::Ready;
use interaction_contracts::desktop::ids::MessageId;
use interaction_contracts::desktop::message::{Envelope, Message};
use interaction_engine::engine::Input;
use interaction_engine::execution::{DeviceCommand, DeviceNotice, DeviceReply};

/// The wire message for a device command.
pub fn command_message(command: DeviceCommand) -> Message {
    match command {
        DeviceCommand::RegisterRoot(grant) => Message::RegisterRoot(grant),
        DeviceCommand::Observe(observation) => Message::Observation(observation),
        DeviceCommand::GrantPermit(request) => Message::GrantPermit(request),
        DeviceCommand::Submit(request) => Message::SubmitIntent(request),
        DeviceCommand::Revoke(request) => Message::RevokeExecution(request),
        DeviceCommand::QueryReceipt(query) => Message::QueryReceipt(query),
        DeviceCommand::UpdateDependency(update) => Message::DependencyUpdate(update),
        DeviceCommand::Stop(stop) => Message::StopCommand(stop),
        DeviceCommand::Resume(resume) => Message::ResumeCommand(resume),
        DeviceCommand::RenewLease(lease) => Message::LeaseRenew(lease),
    }
}

#[derive(Debug, Default)]
pub struct Bridge {
    /// Message id to the engine's correlation number, and whether it asked
    /// the device about a receipt (whose "no record" arrives as a refusal).
    pending: BTreeMap<String, (u64, bool)>,
}

impl Bridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember that the message with this id carries the engine's `corr`.
    pub fn sent(&mut self, id: &MessageId, corr: u64, command: &DeviceCommand) {
        let is_query = matches!(command, DeviceCommand::QueryReceipt(_));
        self.pending
            .insert(id.as_str().to_string(), (corr, is_query));
    }

    /// The device went away: nothing it was asked will be answered.
    pub fn forget_all(&mut self) {
        self.pending.clear();
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// What a message from the device means to the engine.
    pub fn device_input(&mut self, envelope: &Envelope) -> Option<Input> {
        let matched = envelope
            .in_reply_to
            .as_ref()
            .and_then(|id| self.pending.remove(id.as_str()));
        let reply = |corr: u64, reply: DeviceReply| Some(Input::Device { corr, reply });
        match (matched, &envelope.message) {
            (Some((corr, is_query)), Message::CommandAck(ack)) => reply(
                corr,
                if is_query && !ack.accepted {
                    // "No record of that intent" is an answer, not a failure.
                    DeviceReply::Receipt(None)
                } else {
                    DeviceReply::Ack(ack.clone())
                },
            ),
            (Some((corr, _)), Message::GrantOutcome(outcome)) => {
                reply(corr, DeviceReply::Grant(outcome.clone()))
            }
            (Some((corr, _)), Message::Receipt(receipt)) => {
                reply(corr, DeviceReply::Receipt(Some(Box::new(receipt.clone()))))
            }
            (Some((corr, _)), Message::StopAck(ack)) => {
                reply(corr, DeviceReply::Stopped(ack.clone()))
            }
            (Some((corr, _)), Message::ResumeAck(ack)) => {
                reply(corr, DeviceReply::Resumed(ack.clone()))
            }
            (Some((corr, _)), Message::DependencyVersions(versions)) => {
                reply(corr, DeviceReply::Dependencies(versions.clone()))
            }
            (None, Message::Receipt(receipt)) => Some(Input::DeviceNotice(DeviceNotice::Receipt(
                Box::new(receipt.clone()),
            ))),
            (None, Message::DeviceEvent(event)) => {
                Some(Input::DeviceNotice(DeviceNotice::Event(event.clone())))
            }
            (
                None,
                Message::Ready(Ready {
                    capabilities: Some(manifest),
                    ..
                }),
            ) => Some(manifest_input(manifest)),
            (None, Message::CapabilityManifest(manifest)) => Some(manifest_input(manifest)),
            // A late answer to something no longer awaited, or something the
            // host has no use for.
            _ => None,
        }
    }
}

fn manifest_input(manifest: &CapabilityManifest) -> Input {
    Input::DeviceNotice(DeviceNotice::Manifest(Box::new(manifest.clone())))
}

/// What a message from the source means to the engine.
pub fn source_input(message: Message) -> Option<Input> {
    match message {
        Message::StructureSnapshot(snapshot) => Some(Input::Structure(snapshot)),
        Message::SourceHealth(health) => Some(Input::SourceHealth(health)),
        Message::FrameMeta(meta) => Some(Input::Frame(Box::new(meta))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::browser::{
        BrowserReadScope, BrowserSourceControl, BrowserSourceInvalidation,
        browser_contract_golden_snapshots,
    };
    use interaction_contracts::desktop::device::{
        CommandAck, GrantOutcome, PermitRef, QueryReceipt, Receipt, ReceiptStatus, ReleaseStatus,
    };
    use interaction_contracts::desktop::samples;

    fn reply_envelope(to: &MessageId, message: Message) -> Envelope {
        let mut envelope = samples::envelope_for(message);
        envelope.in_reply_to = Some(to.clone());
        envelope
    }

    fn id(text: &str) -> MessageId {
        MessageId::new(text).unwrap()
    }

    #[test]
    fn an_answer_reaches_the_command_that_asked_and_only_once() {
        let mut bridge = Bridge::new();
        let command =
            DeviceCommand::RenewLease(interaction_contracts::desktop::device::LeaseRenew {
                lease_epoch: 1,
                valid_for_ms: 5_000,
            });
        bridge.sent(&id("host-7"), 42, &command);
        assert_eq!(bridge.pending(), 1);
        let answer = reply_envelope(&id("host-7"), Message::CommandAck(CommandAck::ok()));
        match bridge.device_input(&answer) {
            Some(Input::Device {
                corr: 42,
                reply: DeviceReply::Ack(ack),
            }) => assert!(ack.accepted),
            other => panic!("{other:?}"),
        }
        assert_eq!(bridge.pending(), 0);
        assert!(
            bridge.device_input(&answer).is_none(),
            "a second copy finds nothing"
        );
    }

    #[test]
    fn a_refusal_to_a_receipt_query_means_the_device_has_no_record() {
        let mut bridge = Bridge::new();
        let query = DeviceCommand::QueryReceipt(QueryReceipt {
            session_epoch: 1,
            intent_id: "intent-1".into(),
        });
        bridge.sent(&id("host-1"), 7, &query);
        let refusal = reply_envelope(
            &id("host-1"),
            Message::CommandAck(CommandAck::rejected(None, "no record")),
        );
        assert!(matches!(
            bridge.device_input(&refusal),
            Some(Input::Device {
                corr: 7,
                reply: DeviceReply::Receipt(None)
            })
        ));
    }

    #[test]
    fn grants_receipts_and_stops_keep_their_kinds() {
        let mut bridge = Bridge::new();
        let other = DeviceCommand::RenewLease(interaction_contracts::desktop::device::LeaseRenew {
            lease_epoch: 1,
            valid_for_ms: 1,
        });
        bridge.sent(&id("a"), 1, &other);
        let grant = reply_envelope(
            &id("a"),
            Message::GrantOutcome(GrantOutcome::Granted {
                permit: PermitRef {
                    id: "p".into(),
                    generation: 1,
                },
            }),
        );
        assert!(matches!(
            bridge.device_input(&grant),
            Some(Input::Device {
                reply: DeviceReply::Grant(GrantOutcome::Granted { .. }),
                ..
            })
        ));

        // An unsolicited receipt is a notice, not a reply.
        let receipt = Receipt {
            intent_id: "i".into(),
            attempt_id: None,
            session_epoch: 1,
            status: ReceiptStatus::Completed,
            reason_code: None,
            reason: None,
            submitted: vec![],
            release_status: ReleaseStatus::NotNeeded,
            command_end_ms: None,
            input_end_ms: None,
            at_ms: 1,
        };
        let notice = samples::envelope_for(Message::Receipt(receipt));
        assert!(matches!(
            bridge.device_input(&notice),
            Some(Input::DeviceNotice(DeviceNotice::Receipt(_)))
        ));
    }

    #[test]
    fn the_manifest_arrives_with_ready_or_on_its_own() {
        let mut bridge = Bridge::new();
        let manifest = samples::manifest();
        let ready = samples::envelope_for(Message::Ready(Ready {
            role: interaction_contracts::desktop::handshake::PeerRole::Device,
            capabilities: Some(manifest.clone()),
            notes: vec![],
        }));
        assert!(matches!(
            bridge.device_input(&ready),
            Some(Input::DeviceNotice(DeviceNotice::Manifest(_)))
        ));
        let alone = samples::envelope_for(Message::CapabilityManifest(manifest));
        assert!(matches!(
            bridge.device_input(&alone),
            Some(Input::DeviceNotice(DeviceNotice::Manifest(_)))
        ));
        // Something the host has no use for is dropped.
        let noise = samples::envelope_for(Message::Shutdown(
            interaction_contracts::desktop::handshake::Shutdown { reason: "x".into() },
        ));
        assert!(bridge.device_input(&noise).is_none());
    }

    #[test]
    fn every_engine_command_has_a_wire_message() {
        use interaction_contracts::desktop::device::{
            StopCommand, StopDisposition, StopScope, StopSource,
        };
        let stop = DeviceCommand::Stop(StopCommand {
            scope: StopScope::All,
            disposition: StopDisposition::Cancel,
            source: StopSource::Host,
            reason: "x".into(),
        });
        assert_eq!(command_message(stop).name(), "stop_command");
    }

    #[test]
    fn browser_controls_and_page_snapshots_are_not_source_peer_inputs() {
        let mut snapshots = browser_contract_golden_snapshots().unwrap();
        let snapshot = snapshots.remove(0);
        let scope_id = "550e8400-e29b-41d4-a716-446655440000";
        let scope = BrowserReadScope {
            scope_id: scope_id.into(),
            generation: 1,
            context: snapshot.context.clone(),
            clock_domain: snapshot.clock_domain.clone(),
            issued_ms: snapshot.observed_ms,
        };

        let install = Message::BrowserSourceControl(BrowserSourceControl::Install {
            scope: Box::new(scope),
        });
        assert!(install.validate().is_ok());
        assert!(source_input(install).is_none());

        let invalidate = Message::BrowserSourceControl(BrowserSourceControl::Invalidate {
            scope_id: scope_id.into(),
            generation: 1,
            reason: BrowserSourceInvalidation::Stopped,
        });
        assert!(invalidate.validate().is_ok());
        assert!(source_input(invalidate).is_none());

        let page = Message::BrowserPageSnapshot(snapshot);
        assert!(page.validate().is_ok());
        assert!(source_input(page).is_none());
    }
}
