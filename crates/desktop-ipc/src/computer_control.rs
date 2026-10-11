//! Private Host/Main request and rejection framing for the computer image port.
//!
//! Frame requests use `CUCIREQ1`; safety confirmations use `CUCISAF1`; both
//! carry a bounded big-endian JSON length. Image replies reuse `CUCIMG01`,
//! frame failures use `CUCIREJ1`, and safety replies use `CUCISAR1`. This
//! protocol never uses the desktop control channel.

use std::io::{self, Read, Write};

use interaction_contracts::desktop::computer::{
    ComputerBatchProposal, ComputerFence, ComputerFrameRequest,
};
use serde::{Deserialize, Serialize};

use crate::computer_image::{ComputerImagePacket, MAGIC as IMAGE_MAGIC};

pub const REQUEST_MAGIC: &[u8; 8] = b"CUCIREQ1";
pub const REJECTION_MAGIC: &[u8; 8] = b"CUCIREJ1";
pub const SAFETY_REQUEST_MAGIC: &[u8; 8] = b"CUCISAF1";
pub const SAFETY_REPLY_MAGIC: &[u8; 8] = b"CUCISAR1";
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_REJECTION_BYTES: usize = 4 * 1024;
pub const MAX_SAFETY_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_SAFETY_CHECKS: usize = 32;
pub const MAX_SAFETY_ID_BYTES: usize = 64;
pub const MAX_SAFETY_CODE_BYTES: usize = 64;
pub const MAX_SAFETY_MESSAGE_BYTES: usize = 4096;
const PREFIX_BYTES: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerImageRejectionReason {
    SourceUnavailable,
    StaleFence,
    FrameUnavailable,
    FrameMismatch,
    DeadlineExpired,
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerImageRejection {
    pub fence: ComputerFence,
    pub reason: ComputerImageRejectionReason,
}

/// Display-only projection of an OpenAI safety check. It is private to fd5;
/// the Host retains the original SDK values for exact acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerSafetyCheck {
    pub id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_nonnull_optional_string"
    )]
    pub code: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_nonnull_optional_string"
    )]
    pub message: Option<String>,
}

/// A pending provider safety decision shown by Main before the Host can proceed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerSafetyRequest {
    pub fence: ComputerFence,
    pub pending_safety_checks: Vec<ComputerSafetyCheck>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_nonnull_optional_proposal"
    )]
    pub proposal: Option<ComputerBatchProposal>,
}

impl ComputerSafetyRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.fence.validate()?;
        if self.pending_safety_checks.is_empty()
            || self.pending_safety_checks.len() > MAX_SAFETY_CHECKS
        {
            return Err(format!(
                "computer safety request must contain 1..={MAX_SAFETY_CHECKS} checks"
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for check in &self.pending_safety_checks {
            if !bounded_nonblank_text(&check.id, MAX_SAFETY_ID_BYTES)
                || !ids.insert(check.id.as_str())
                || check
                    .code
                    .as_deref()
                    .is_some_and(|code| !bounded_nonblank_text(code, MAX_SAFETY_CODE_BYTES))
                || check.message.as_deref().is_some_and(|message| {
                    message.len() > MAX_SAFETY_MESSAGE_BYTES
                        || message.chars().any(char::is_control)
                })
            {
                return Err("computer safety check fields are invalid".into());
            }
        }
        if let Some(proposal) = &self.proposal {
            proposal.validate()?;
            if proposal.fence != self.fence {
                return Err("computer safety proposal fence does not match request".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerSafetyReply {
    pub fence: ComputerFence,
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComputerPortRequest {
    Frame(Box<ComputerFrameRequest>),
    Safety(Box<ComputerSafetyRequest>),
}

#[derive(Debug, Clone)]
pub enum ComputerImageResponse {
    Image(Box<ComputerImagePacket>),
    Rejected(ComputerImageRejection),
}

fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn validate_request(request: &ComputerFrameRequest) -> io::Result<()> {
    request
        .validate()
        .map_err(|_| invalid("computer frame request is invalid"))
}

fn validate_safety_request(request: &ComputerSafetyRequest) -> io::Result<()> {
    request
        .validate()
        .map_err(|_| invalid("computer safety request is invalid"))
}

fn bounded_nonblank_text(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn deserialize_nonnull_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

fn deserialize_nonnull_optional_proposal<'de, D>(
    deserializer: D,
) -> Result<Option<ComputerBatchProposal>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    ComputerBatchProposal::deserialize(deserializer).map(Some)
}

fn write_framed_json<W, C>(
    magic: &[u8; 8],
    body: &[u8],
    max_bytes: usize,
    out: &mut W,
    checkpoint: &mut C,
) -> io::Result<()>
where
    W: Write,
    C: FnMut() -> io::Result<()>,
{
    if !(2..=max_bytes).contains(&body.len()) {
        return Err(invalid("computer request JSON exceeds bounds"));
    }
    let length =
        u32::try_from(body.len()).map_err(|_| invalid("computer request length exceeds bounds"))?;
    checkpoint()?;
    out.write_all(magic)?;
    checkpoint()?;
    out.write_all(&length.to_be_bytes())?;
    checkpoint()?;
    out.write_all(body)
}

fn read_framed_body<R, C>(
    input: &mut R,
    checkpoint: &mut C,
    max_bytes: usize,
) -> io::Result<([u8; 8], Vec<u8>)>
where
    R: Read,
    C: FnMut() -> io::Result<()>,
{
    let mut input = CheckedReader { input, checkpoint };
    let mut prefix = [0u8; PREFIX_BYTES];
    input.read_exact(&mut prefix)?;
    let magic = prefix[..8].try_into().unwrap();
    let length = u32::from_be_bytes(prefix[8..12].try_into().unwrap()) as usize;
    if !(2..=max_bytes).contains(&length) {
        return Err(invalid("computer request length exceeds bounds"));
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    Ok((magic, body))
}

/// Write one validated Host request. Callers must discard the stream on error.
pub fn write_request<W, C>(
    request: &ComputerFrameRequest,
    out: &mut W,
    mut checkpoint: C,
) -> io::Result<()>
where
    W: Write,
    C: FnMut() -> io::Result<()>,
{
    validate_request(request)?;
    let body = serde_json::to_vec(request).map_err(io::Error::other)?;
    write_framed_json(
        REQUEST_MAGIC,
        &body,
        MAX_REQUEST_BYTES,
        out,
        &mut checkpoint,
    )
}

/// Send bounded safety-check data to Main for an explicit native confirmation.
/// Callers must bound blocking IO and discard the stream after a write error.
pub fn write_safety_request<W, C>(
    request: &ComputerSafetyRequest,
    out: &mut W,
    mut checkpoint: C,
) -> io::Result<()>
where
    W: Write,
    C: FnMut() -> io::Result<()>,
{
    validate_safety_request(request)?;
    let body = serde_json::to_vec(request).map_err(io::Error::other)?;
    write_framed_json(
        SAFETY_REQUEST_MAGIC,
        &body,
        MAX_SAFETY_REQUEST_BYTES,
        out,
        &mut checkpoint,
    )
}

/// Read one frame request for callers that require that variant. The caller
/// owns read timeouts and must discard the stream after any framing or cancellation error.
pub fn read_request<R, C>(input: &mut R, checkpoint: C) -> io::Result<ComputerFrameRequest>
where
    R: Read,
    C: FnMut() -> io::Result<()>,
{
    match read_port_request(input, checkpoint)? {
        ComputerPortRequest::Frame(request) => Ok(*request),
        ComputerPortRequest::Safety(_) => Err(invalid("expected computer frame request")),
    }
}

/// Decode either Host-to-Main fd5 request. The existing `read_request` remains
/// frame-only for callers that require that variant.
pub fn read_port_request<R, C>(input: &mut R, mut checkpoint: C) -> io::Result<ComputerPortRequest>
where
    R: Read,
    C: FnMut() -> io::Result<()>,
{
    let mut input = CheckedReader {
        input,
        checkpoint: &mut checkpoint,
    };
    let mut prefix = [0u8; PREFIX_BYTES];
    input.read_exact(&mut prefix)?;
    let magic: [u8; 8] = prefix[..8].try_into().unwrap();
    let max_bytes = if &magic == REQUEST_MAGIC {
        MAX_REQUEST_BYTES
    } else if &magic == SAFETY_REQUEST_MAGIC {
        MAX_SAFETY_REQUEST_BYTES
    } else {
        return Err(invalid("computer request magic is invalid"));
    };
    let length = u32::from_be_bytes(prefix[8..12].try_into().unwrap()) as usize;
    if !(2..=max_bytes).contains(&length) {
        return Err(invalid("computer request length exceeds bounds"));
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    match &magic {
        value if value == REQUEST_MAGIC => {
            let request: ComputerFrameRequest = serde_json::from_slice(&body)
                .map_err(|_| invalid("computer frame request JSON is invalid"))?;
            validate_request(&request)?;
            Ok(ComputerPortRequest::Frame(Box::new(request)))
        }
        value if value == SAFETY_REQUEST_MAGIC => {
            let request: ComputerSafetyRequest = serde_json::from_slice(&body)
                .map_err(|_| invalid("computer safety request JSON is invalid"))?;
            validate_safety_request(&request)?;
            Ok(ComputerPortRequest::Safety(Box::new(request)))
        }
        _ => Err(invalid("computer request magic is invalid")),
    }
}

pub fn write_safety_reply<W, C>(
    reply: &ComputerSafetyReply,
    out: &mut W,
    mut checkpoint: C,
) -> io::Result<()>
where
    W: Write,
    C: FnMut() -> io::Result<()>,
{
    reply
        .fence
        .validate()
        .map_err(|_| invalid("computer safety reply fence is invalid"))?;
    let body = serde_json::to_vec(reply).map_err(io::Error::other)?;
    write_framed_json(
        SAFETY_REPLY_MAGIC,
        &body,
        MAX_REJECTION_BYTES,
        out,
        &mut checkpoint,
    )
}

/// Read only a reply correlated to the exact request, before its fence expires.
/// Callers must bound blocking IO and discard the stream after any error.
pub fn read_safety_reply<R, N, C>(
    input: &mut R,
    request: &ComputerSafetyRequest,
    mut now_ms: N,
    mut checkpoint: C,
) -> io::Result<ComputerSafetyReply>
where
    R: Read,
    N: FnMut() -> u64,
    C: FnMut() -> io::Result<()>,
{
    validate_safety_request(request)?;
    if now_ms() >= request.fence.deadline_ms {
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    }
    let (magic, body) = read_framed_body(input, &mut checkpoint, MAX_REJECTION_BYTES)?;
    if &magic != SAFETY_REPLY_MAGIC {
        return Err(invalid("computer safety reply magic is invalid"));
    }
    let reply: ComputerSafetyReply = serde_json::from_slice(&body)
        .map_err(|_| invalid("computer safety reply JSON is invalid"))?;
    reply
        .fence
        .validate()
        .map_err(|_| invalid("computer safety reply fence is invalid"))?;
    if reply.fence != request.fence {
        return Err(invalid(
            "computer safety reply correlation does not match request",
        ));
    }
    checkpoint()?;
    if now_ms() >= request.fence.deadline_ms {
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    }
    Ok(reply)
}

pub fn write_rejection<W, C>(
    rejection: &ComputerImageRejection,
    out: &mut W,
    mut checkpoint: C,
) -> io::Result<()>
where
    W: Write,
    C: FnMut() -> io::Result<()>,
{
    rejection
        .fence
        .validate()
        .map_err(|_| invalid("computer rejection fence is invalid"))?;
    let body = serde_json::to_vec(rejection).map_err(io::Error::other)?;
    if !(2..=MAX_REJECTION_BYTES).contains(&body.len()) {
        return Err(invalid("computer rejection JSON exceeds bounds"));
    }
    let length = u32::try_from(body.len())
        .map_err(|_| invalid("computer rejection length exceeds bounds"))?;
    checkpoint()?;
    out.write_all(REJECTION_MAGIC)?;
    checkpoint()?;
    out.write_all(&length.to_be_bytes())?;
    checkpoint()?;
    out.write_all(&body)
}

/// Read one correlated image or rejection response for an outstanding request.
/// A malformed or foreign response is a protocol error, never a fallback image.
/// The callback cannot interrupt blocking IO; callers must configure timeouts
/// and discard the stream after any error.
pub fn read_response<R, N, C>(
    input: &mut R,
    request: &ComputerFrameRequest,
    mut now_ms: N,
    mut checkpoint: C,
) -> io::Result<ComputerImageResponse>
where
    R: Read,
    N: FnMut() -> u64,
    C: FnMut() -> io::Result<()>,
{
    validate_request(request)?;
    checkpoint()?;
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic)?;
    checkpoint()?;

    if &magic == IMAGE_MAGIC {
        let mut prefixed = PrefixReader {
            prefix: magic,
            offset: 0,
            input,
        };
        let image = ComputerImagePacket::read_from_with_check(
            &mut prefixed,
            request,
            &mut now_ms,
            &mut checkpoint,
        )?;
        return Ok(ComputerImageResponse::Image(Box::new(image)));
    }
    if &magic != REJECTION_MAGIC {
        return Err(invalid("computer response magic is invalid"));
    }

    checkpoint()?;
    let mut length = [0u8; 4];
    input.read_exact(&mut length)?;
    checkpoint()?;
    let length = u32::from_be_bytes(length) as usize;
    if !(2..=MAX_REJECTION_BYTES).contains(&length) {
        return Err(invalid("computer rejection length exceeds bounds"));
    }
    let mut body = vec![0; length];
    checkpoint()?;
    input.read_exact(&mut body)?;
    checkpoint()?;
    let rejection: ComputerImageRejection =
        serde_json::from_slice(&body).map_err(|_| invalid("computer rejection JSON is invalid"))?;
    rejection
        .fence
        .validate()
        .map_err(|_| invalid("computer rejection fence is invalid"))?;
    if rejection.fence != request.fence {
        return Err(invalid(
            "computer rejection correlation does not match request",
        ));
    }
    Ok(ComputerImageResponse::Rejected(rejection))
}

struct CheckedReader<'a, R, C> {
    input: &'a mut R,
    checkpoint: &'a mut C,
}

impl<R, C> Read for CheckedReader<'_, R, C>
where
    R: Read,
    C: FnMut() -> io::Result<()>,
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        (self.checkpoint)()?;
        let read = self.input.read(buffer)?;
        (self.checkpoint)()?;
        Ok(read)
    }

    fn read_exact(&mut self, mut buffer: &mut [u8]) -> io::Result<()> {
        while !buffer.is_empty() {
            (self.checkpoint)()?;
            match self.input.read(buffer) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(read) => {
                    buffer = &mut buffer[read..];
                    (self.checkpoint)()?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

struct PrefixReader<'a, R> {
    prefix: [u8; 8],
    offset: usize,
    input: &'a mut R,
}

impl<R: Read> Read for PrefixReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.offset < self.prefix.len() {
            let length = (self.prefix.len() - self.offset).min(buffer.len());
            buffer[..length].copy_from_slice(&self.prefix[self.offset..self.offset + length]);
            self.offset += length;
            return Ok(length);
        }
        self.input.read(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
    use std::io::Cursor;

    const IMAGE_PACKET: &[u8] =
        include_bytes!("../../../contracts/desktop-v1/image-channel/packet.bin");

    fn request() -> ComputerFrameRequest {
        let image = crate::image::read_from(&mut Cursor::new(IMAGE_PACKET)).unwrap();
        ComputerFrameRequest {
            fence: ComputerFence {
                request_id: RequestId::new("frame-request-1").unwrap(),
                generation: 1,
                root_task_id: RootTaskId::new("root-1").unwrap(),
                task_revision: 2,
                root_epoch: 3,
                snapshot_id: "snapshot-4".into(),
                structure_sequence: 5,
                deadline_ms: 20_000,
            },
            expected_frame: image.meta,
        }
    }

    fn safety_request() -> ComputerSafetyRequest {
        let frame_request = request();
        ComputerSafetyRequest {
            fence: frame_request.fence.clone(),
            pending_safety_checks: vec![ComputerSafetyCheck {
                id: "safety-1".into(),
                code: Some("confirm".into()),
                message: Some("Review this action".into()),
            }],
            proposal: Some(ComputerBatchProposal {
                fence: frame_request.fence,
                actions: vec![
                    interaction_contracts::desktop::computer::ComputerActionProposal::Wait,
                ],
            }),
        }
    }

    #[test]
    fn request_wire_round_trips_one_bounded_valid_request() {
        let request = request();
        let mut bytes = Vec::new();
        write_request(&request, &mut bytes, || Ok(())).unwrap();
        assert_eq!(&bytes[..8], REQUEST_MAGIC);
        assert_eq!(
            read_request(&mut Cursor::new(bytes), || Ok(())).unwrap(),
            request
        );
    }

    #[test]
    fn safety_request_dispatches_separately_and_round_trips_typed_review_data() {
        let frame_request = request();
        let safety = safety_request();
        let mut bytes = Vec::new();
        write_safety_request(&safety, &mut bytes, || Ok(())).unwrap();
        assert_eq!(&bytes[..8], SAFETY_REQUEST_MAGIC);
        assert_eq!(
            read_port_request(&mut Cursor::new(bytes), || Ok(())).unwrap(),
            ComputerPortRequest::Safety(Box::new(safety))
        );

        let mut frame_bytes = Vec::new();
        write_request(&frame_request, &mut frame_bytes, || Ok(())).unwrap();
        assert!(matches!(
            read_port_request(&mut Cursor::new(frame_bytes), || Ok(())).unwrap(),
            ComputerPortRequest::Frame(_)
        ));
    }

    #[test]
    fn safety_reply_requires_exact_fence_and_live_deadline() {
        let request = safety_request();
        let reply = ComputerSafetyReply {
            fence: request.fence.clone(),
            approved: true,
        };
        let mut bytes = Vec::new();
        write_safety_reply(&reply, &mut bytes, || Ok(())).unwrap();
        assert_eq!(&bytes[..8], SAFETY_REPLY_MAGIC);
        assert_eq!(
            read_safety_reply(
                &mut Cursor::new(bytes.clone()),
                &request,
                || 19_999,
                || Ok(())
            )
            .unwrap(),
            reply
        );

        assert_eq!(
            read_safety_reply(
                &mut Cursor::new(bytes.clone()),
                &request,
                || 20_000,
                || Ok(())
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
        let mut foreign = reply;
        foreign.fence.generation += 1;
        let mut foreign_wire = Vec::new();
        write_safety_reply(&foreign, &mut foreign_wire, || Ok(())).unwrap();
        assert!(
            read_safety_reply(
                &mut Cursor::new(foreign_wire),
                &request,
                || 19_999,
                || Ok(())
            )
            .is_err()
        );
    }

    #[test]
    fn safety_request_rejects_duplicate_unbounded_or_mismatched_review_data() {
        let mut invalid_request = safety_request();
        invalid_request
            .pending_safety_checks
            .push(invalid_request.pending_safety_checks[0].clone());
        assert!(write_safety_request(&invalid_request, &mut Vec::new(), || Ok(())).is_err());

        let mut invalid_request = safety_request();
        invalid_request.pending_safety_checks[0].message =
            Some("x".repeat(MAX_SAFETY_MESSAGE_BYTES + 1));
        assert!(write_safety_request(&invalid_request, &mut Vec::new(), || Ok(())).is_err());

        let mut invalid_request = safety_request();
        invalid_request.proposal.as_mut().unwrap().fence.generation += 1;
        assert!(write_safety_request(&invalid_request, &mut Vec::new(), || Ok(())).is_err());

        let mut oversized_request = safety_request();
        oversized_request.proposal.as_mut().unwrap().actions = (0..17)
            .map(
                |_| interaction_contracts::desktop::computer::ComputerActionProposal::Type {
                    text: "x".repeat(65_536),
                },
            )
            .collect();
        let mut output = Vec::new();
        assert!(write_safety_request(&oversized_request, &mut output, || Ok(())).is_err());
        assert!(output.is_empty());

        let mut unknown_field = serde_json::to_value(safety_request()).unwrap();
        unknown_field["unexpected"] = serde_json::json!(true);
        let mut null_field = serde_json::to_value(safety_request()).unwrap();
        null_field["pending_safety_checks"][0]["code"] = serde_json::Value::Null;
        let null_body = serde_json::to_vec(&null_field).unwrap();
        let mut null_bytes = SAFETY_REQUEST_MAGIC.to_vec();
        null_bytes.extend_from_slice(&(null_body.len() as u32).to_be_bytes());
        null_bytes.extend_from_slice(&null_body);
        assert!(read_port_request(&mut Cursor::new(null_bytes), || Ok(())).is_err());
        let body = serde_json::to_vec(&unknown_field).unwrap();
        let mut bytes = SAFETY_REQUEST_MAGIC.to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&body);
        assert!(read_port_request(&mut Cursor::new(bytes), || Ok(())).is_err());
    }

    #[test]
    fn request_wire_rejects_bad_magic_length_truncation_and_invalid_contracts() {
        let request = request();
        let mut bytes = Vec::new();
        write_request(&request, &mut bytes, || Ok(())).unwrap();

        let mut bad_magic = bytes.clone();
        bad_magic[0] ^= 1;
        assert!(read_request(&mut Cursor::new(bad_magic), || Ok(())).is_err());

        let mut oversized = REQUEST_MAGIC.to_vec();
        oversized.extend_from_slice(&((MAX_REQUEST_BYTES as u32) + 1).to_be_bytes());
        assert!(read_request(&mut Cursor::new(oversized), || Ok(())).is_err());
        assert!(
            read_request(&mut Cursor::new(bytes[..bytes.len() - 1].to_vec()), || Ok(
                ()
            ))
            .is_err()
        );

        let mut invalid_request = request;
        invalid_request.fence.structure_sequence = 0;
        assert!(write_request(&invalid_request, &mut Vec::new(), || Ok(())).is_err());
    }

    #[test]
    fn late_interrupted_checkpoint_is_returned_without_read_exact_retrying_it() {
        let request = request();
        let mut bytes = Vec::new();
        write_request(&request, &mut bytes, || Ok(())).unwrap();
        let calls = std::cell::Cell::new(0usize);
        let error = read_request(&mut Cursor::new(bytes), || {
            let next = calls.get() + 1;
            calls.set(next);
            if next >= 4 {
                Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn response_reader_correlates_image_and_bounded_rejection_variants() {
        let request = request();
        let image = ComputerImagePacket {
            fence: request.fence.clone(),
            image: crate::image::read_from(&mut Cursor::new(IMAGE_PACKET)).unwrap(),
        };
        let mut image_wire = Vec::new();
        image
            .write_to_with_check(&request, &mut image_wire, || 10_000, || Ok(()))
            .unwrap();
        assert!(matches!(
            read_response(&mut Cursor::new(image_wire), &request, || 10_001, || Ok(())).unwrap(),
            ComputerImageResponse::Image(_)
        ));

        let rejection = ComputerImageRejection {
            fence: request.fence.clone(),
            reason: ComputerImageRejectionReason::FrameUnavailable,
        };
        let mut reject_wire = Vec::new();
        write_rejection(&rejection, &mut reject_wire, || Ok(())).unwrap();
        assert!(matches!(
            read_response(&mut Cursor::new(reject_wire.clone()), &request, || 10_001, || Ok(()))
                .unwrap(),
            ComputerImageResponse::Rejected(value) if value == rejection
        ));

        let mut foreign = rejection;
        foreign.fence.generation += 1;
        let mut foreign_wire = Vec::new();
        write_rejection(&foreign, &mut foreign_wire, || Ok(())).unwrap();
        assert!(
            read_response(
                &mut Cursor::new(foreign_wire),
                &request,
                || 10_001,
                || Ok(())
            )
            .is_err()
        );

        reject_wire[0] ^= 1;
        assert!(
            read_response(
                &mut Cursor::new(reject_wire),
                &request,
                || 10_001,
                || Ok(())
            )
            .is_err()
        );
    }
}
