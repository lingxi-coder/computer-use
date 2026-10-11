//! Bounded transport correlation only; Environment owns refresh authority.
use interaction_contracts::desktop::{
    ids::MessageId,
    message::{Envelope, Message},
};
use interaction_engine::engine::Input;

#[derive(Default)]
pub(crate) struct SourceRefreshBridge {
    pending: Option<(MessageId, u64)>,
}

impl SourceRefreshBridge {
    pub fn sent(&mut self, id: MessageId, generation: u64) {
        self.pending = Some((id, generation));
    }
    pub fn clear(&mut self) {
        self.pending = None;
    }
    pub fn arrived(&mut self, envelope: &Envelope) -> Option<Input> {
        let (id, generation) = self.pending.as_ref()?;
        if envelope.in_reply_to.as_ref() != Some(id) {
            return None;
        }
        let Message::CommandAck(ack) = &envelope.message else {
            return None;
        };
        let result = Input::SourceRefreshStarted {
            generation: *generation,
            accepted: ack.accepted,
        };
        self.pending = None;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::{device::CommandAck, samples};
    fn reply(id: &str, message: Message) -> Envelope {
        let mut envelope = samples::envelope_for(message);
        envelope.in_reply_to = Some(MessageId::new(id).unwrap());
        envelope
    }
    #[test]
    fn old_foreign_duplicate_and_uncorrelated_acks_cannot_open_the_latest_barrier() {
        let mut bridge = SourceRefreshBridge::default();
        bridge.sent(MessageId::new("refresh-1").unwrap(), 1);
        bridge.sent(MessageId::new("refresh-2").unwrap(), 2);
        for envelope in [
            reply("refresh-1", Message::CommandAck(CommandAck::ok())),
            reply("foreign", Message::CommandAck(CommandAck::ok())),
            samples::envelope_for(Message::CommandAck(CommandAck::ok())),
        ] {
            assert_eq!(bridge.arrived(&envelope), None);
        }
        let latest = reply("refresh-2", Message::CommandAck(CommandAck::ok()));
        assert_eq!(
            bridge.arrived(&latest),
            Some(Input::SourceRefreshStarted {
                generation: 2,
                accepted: true
            })
        );
        assert_eq!(bridge.arrived(&latest), None);
        bridge.sent(MessageId::new("refresh-3").unwrap(), 3);
        bridge.clear();
        assert_eq!(
            bridge.arrived(&reply("refresh-3", Message::CommandAck(CommandAck::ok()))),
            None
        );
    }
    #[test]
    fn data_correlated_to_a_request_is_still_not_a_transition_ack() {
        let mut bridge = SourceRefreshBridge::default();
        bridge.sent(MessageId::new("refresh-1").unwrap(), 1);
        let data = samples::all()
            .into_iter()
            .map(|(_, envelope)| envelope.message)
            .find(|m| matches!(m, Message::StructureSnapshot(_)))
            .unwrap();
        assert_eq!(bridge.arrived(&reply("refresh-1", data)), None);
        assert_eq!(
            bridge.arrived(&reply("refresh-1", Message::CommandAck(CommandAck::ok()))),
            Some(Input::SourceRefreshStarted {
                generation: 1,
                accepted: true
            })
        );
    }
}
