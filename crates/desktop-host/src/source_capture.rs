//! Capture transport correlation is independent of Source refresh authority.
//! An acceptance ACK never proves pixels; the Engine consumes the actual pair.

use interaction_contracts::desktop::{
    ids::{MessageId, RequestId},
    message::{Envelope, Message},
};
use interaction_engine::engine::{Input, computer::ComputerInput};

#[derive(Default)]
pub(crate) struct SourceCaptureBridge {
    pending: Option<Correlation>,
}

struct Correlation {
    message_id: MessageId,
    request_id: RequestId,
    watch_id: String,
    deadline_ms: u64,
}

impl SourceCaptureBridge {
    pub fn available(&mut self, now_ms: u64) -> bool {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| now_ms >= pending.deadline_ms)
        {
            self.pending = None;
        }
        self.pending.is_none()
    }

    pub fn sent(
        &mut self,
        message_id: MessageId,
        request_id: RequestId,
        watch_id: String,
        deadline_ms: u64,
    ) {
        self.pending = Some(Correlation {
            message_id,
            request_id,
            watch_id,
            deadline_ms,
        });
    }

    pub fn cancel(&mut self, request_id: &RequestId, watch_id: &str) {
        if self.pending.as_ref().is_some_and(|pending| {
            pending.request_id == *request_id && pending.watch_id == watch_id
        }) {
            self.pending = None;
        }
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }

    pub fn arrived(&mut self, envelope: &Envelope, now_ms: u64) -> Option<Input> {
        let pending = self.pending.as_ref()?;
        if now_ms >= pending.deadline_ms {
            self.pending = None;
            return None;
        }
        if envelope.in_reply_to.as_ref() != Some(&pending.message_id) {
            return None;
        }
        let Message::CommandAck(ack) = &envelope.message else {
            return None;
        };
        if ack.accepted {
            // Keep the correlation so a later native failure can close this
            // exact request. The complete FrameMeta/AX pair is authoritative.
            return None;
        }
        let pending = self.pending.take().expect("checked above");
        Some(Input::Computer(Box::new(ComputerInput::CaptureRejected {
            request_id: pending.request_id,
            watch_id: pending.watch_id,
            reason: ack
                .reason
                .clone()
                .unwrap_or_else(|| "Source capture was rejected".into()),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::{device::CommandAck, samples};

    fn reply(id: &str, accepted: bool) -> Envelope {
        let mut envelope = samples::envelope_for(Message::CommandAck(if accepted {
            CommandAck::ok()
        } else {
            CommandAck::rejected(None, "capture_timeout")
        }));
        envelope.in_reply_to = Some(MessageId::new(id).unwrap());
        envelope
    }

    #[test]
    fn acceptance_keeps_the_original_failure_correlation_and_never_refreshes_source() {
        let mut bridge = SourceCaptureBridge::default();
        bridge.sent(
            MessageId::new("msg-1").unwrap(),
            RequestId::new("capture-1").unwrap(),
            "watch-1".into(),
            100,
        );
        assert_eq!(bridge.arrived(&reply("msg-1", true), 1), None);
        assert!(!bridge.available(2));
        assert_eq!(bridge.arrived(&reply("other", false), 3), None);
        assert!(
            matches!(bridge.arrived(&reply("msg-1", false), 4), Some(Input::Computer(input))
            if matches!(input.as_ref(), ComputerInput::CaptureRejected {request_id, watch_id, ..}
                if request_id.as_str() == "capture-1" && watch_id == "watch-1"))
        );
        assert_eq!(bridge.arrived(&reply("msg-1", false), 5), None);
    }

    #[test]
    fn cancelled_expired_and_old_replies_do_not_reject_a_replacement_request() {
        let mut bridge = SourceCaptureBridge::default();
        let request = RequestId::new("capture-1").unwrap();
        bridge.sent(
            MessageId::new("msg-1").unwrap(),
            request.clone(),
            "watch-1".into(),
            10,
        );
        bridge.cancel(&request, "foreign-watch");
        assert!(!bridge.available(1));
        bridge.cancel(&request, "watch-1");
        bridge.sent(
            MessageId::new("msg-2").unwrap(),
            RequestId::new("capture-2").unwrap(),
            "watch-1".into(),
            20,
        );
        assert_eq!(bridge.arrived(&reply("msg-1", false), 5), None);
        assert_eq!(bridge.arrived(&reply("msg-2", false), 20), None);
        assert!(bridge.available(20));
    }
}
