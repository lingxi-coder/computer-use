//! Length-prefixed framing for control messages (design 4.6). No IO here.
//!
//! A frame is a 4-byte big-endian length followed by that many bytes of
//! UTF-8 JSON holding one [`Envelope`]. The length is checked before any
//! payload is buffered, so a hostile length cannot force an allocation.

use super::message::Envelope;

pub const LENGTH_PREFIX_BYTES: usize = 4;
/// Control messages only. Images travel on a separate bounded binary channel.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    TooLarge { len: usize, max: usize },
    Empty,
    Malformed(String),
    Invalid(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { len, max } => {
                write!(f, "frame of {len} bytes exceeds the {max} limit")
            }
            Self::Empty => f.write_str("empty frame"),
            Self::Malformed(reason) => write!(f, "malformed frame: {reason}"),
            Self::Invalid(reason) => write!(f, "invalid message: {reason}"),
        }
    }
}

impl std::error::Error for FrameError {}

pub fn encode_frame(envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
    let payload =
        serde_json::to_vec(envelope).map_err(|error| FrameError::Malformed(error.to_string()))?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            len: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    let mut out = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Accumulates bytes and yields complete envelopes. After any error the
/// decoder stays failed: the connection must be dropped, because there is no
/// way to know where the next frame starts.
#[derive(Debug)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    max_frame: usize,
    failed: Option<FrameError>,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new(MAX_FRAME_BYTES)
    }
}

impl FrameDecoder {
    pub fn new(max_frame: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_frame,
            failed: None,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.failed.is_none() {
            self.buffer.extend_from_slice(bytes);
        }
    }

    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// The next complete envelope, or `Ok(None)` when more bytes are needed.
    pub fn next_envelope(&mut self) -> Result<Option<Envelope>, FrameError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if self.buffer.len() < LENGTH_PREFIX_BYTES {
            return Ok(None);
        }
        let len = u32::from_be_bytes([
            self.buffer[0],
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
        ]) as usize;
        if len == 0 {
            return self.fail(FrameError::Empty);
        }
        if len > self.max_frame {
            return self.fail(FrameError::TooLarge {
                len,
                max: self.max_frame,
            });
        }
        if self.buffer.len() < LENGTH_PREFIX_BYTES + len {
            return Ok(None);
        }
        let payload: Vec<u8> = self
            .buffer
            .drain(..LENGTH_PREFIX_BYTES + len)
            .skip(LENGTH_PREFIX_BYTES)
            .collect();
        match super::message::decode_envelope(&payload) {
            Ok(envelope) => Ok(Some(envelope)),
            Err(error) => self.fail(FrameError::Invalid(error)),
        }
    }

    fn fail<T>(&mut self, error: FrameError) -> Result<T, FrameError> {
        self.buffer.clear();
        self.failed = Some(error.clone());
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::samples;

    fn sample() -> Envelope {
        samples::envelope_for(samples::stop_command())
    }

    #[test]
    fn a_frame_round_trips() {
        let bytes = encode_frame(&sample()).unwrap();
        let mut decoder = FrameDecoder::default();
        decoder.push(&bytes);
        assert_eq!(decoder.next_envelope().unwrap().unwrap(), sample());
        assert_eq!(decoder.next_envelope().unwrap(), None);
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn frames_split_at_any_byte_boundary_still_decode() {
        let bytes = encode_frame(&sample()).unwrap();
        for split in 1..bytes.len() {
            let mut decoder = FrameDecoder::default();
            decoder.push(&bytes[..split]);
            assert_eq!(decoder.next_envelope().unwrap(), None, "split at {split}");
            decoder.push(&bytes[split..]);
            assert_eq!(decoder.next_envelope().unwrap().unwrap(), sample());
        }
    }

    #[test]
    fn several_frames_in_one_read_come_out_in_order() {
        let first = encode_frame(&sample()).unwrap();
        let second = encode_frame(&samples::envelope_for(samples::resume_command())).unwrap();
        let mut decoder = FrameDecoder::default();
        decoder.push(&[first.as_slice(), second.as_slice()].concat());
        assert_eq!(
            decoder.next_envelope().unwrap().unwrap().message.name(),
            "stop_command"
        );
        assert_eq!(
            decoder.next_envelope().unwrap().unwrap().message.name(),
            "resume_command"
        );
        assert_eq!(decoder.next_envelope().unwrap(), None);
    }

    #[test]
    fn an_oversized_length_is_refused_before_any_payload_is_buffered() {
        let mut decoder = FrameDecoder::new(1024);
        decoder.push(&(1_000_000u32).to_be_bytes());
        assert!(matches!(
            decoder.next_envelope(),
            Err(FrameError::TooLarge { .. })
        ));
        assert_eq!(
            decoder.buffered(),
            0,
            "nothing is retained after the refusal"
        );
    }

    #[test]
    fn a_failed_decoder_stays_failed() {
        let mut decoder = FrameDecoder::default();
        decoder.push(&0u32.to_be_bytes());
        assert_eq!(decoder.next_envelope(), Err(FrameError::Empty));
        decoder.push(&encode_frame(&sample()).unwrap());
        assert_eq!(decoder.next_envelope(), Err(FrameError::Empty));
    }

    #[test]
    fn garbage_and_invalid_messages_are_errors_not_panics() {
        let mut decoder = FrameDecoder::default();
        let junk = b"not json at all";
        decoder.push(&(junk.len() as u32).to_be_bytes());
        decoder.push(junk);
        assert!(matches!(
            decoder.next_envelope(),
            Err(FrameError::Invalid(_))
        ));

        let mut envelope = serde_json::to_value(sample()).unwrap();
        envelope["protocol_version"] = serde_json::json!(9);
        let payload = serde_json::to_vec(&envelope).unwrap();
        let mut decoder = FrameDecoder::default();
        decoder.push(&(payload.len() as u32).to_be_bytes());
        decoder.push(&payload);
        assert!(matches!(
            decoder.next_envelope(),
            Err(FrameError::Invalid(_))
        ));
    }
}
