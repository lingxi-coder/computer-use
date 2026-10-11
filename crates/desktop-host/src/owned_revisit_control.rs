//! Private Main-to-Host control channel for a planned owned-document revisit.
//!
//! This protocol never enters desktop IPC Envelopes or the operator queue.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub(crate) const VERSION: u16 = 1;
pub(crate) const MAX_MESSAGE_BYTES: usize = 32 * 1024;
const INVALID: &str = "invalid private owned revisit control";

/// Open only the explicitly inherited private control endpoint. `/dev/fd/3`
/// duplicates the already-open descriptor; it does not discover a path or use
/// ambient configuration.
pub(crate) fn open_inherited_fd3() -> Result<UnixStream, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fd/3")
        .map_err(|_| INVALID.to_string())?;
    if !file
        .metadata()
        .map_err(|_| INVALID.to_string())?
        .file_type()
        .is_socket()
    {
        return Err(INVALID.into());
    }
    let descriptor: OwnedFd = file.into();
    Ok(UnixStream::from(descriptor))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RejectionCode {
    InvalidState,
    EngineRejected,
    Timeout,
    PeerUnavailable,
    DeviceNotObserveOnly,
    TransitionFailed,
    ProtocolError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MainMessage<T> {
    Prepare {
        version: u16,
        request_id: String,
    },
    Finish {
        version: u16,
        request_id: String,
        completion: Box<T>,
        new_device_token: String,
        new_source_token: String,
    },
    Abort {
        version: u16,
        request_id: String,
        transition_id: String,
    },
}

impl<T> MainMessage<T> {
    pub(crate) fn request_id(&self) -> &str {
        match self {
            Self::Prepare { request_id, .. }
            | Self::Finish { request_id, .. }
            | Self::Abort { request_id, .. } => request_id,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        let request_id = self.request_id();
        if !canonical_uuid(request_id) {
            return Err(INVALID.into());
        }
        match self {
            Self::Prepare { version, .. } => check_version(*version),
            Self::Finish {
                version,
                new_device_token,
                new_source_token,
                ..
            } => {
                check_version(*version)?;
                if !canonical_token(new_device_token) || !canonical_token(new_source_token) {
                    return Err(INVALID.into());
                }
                Ok(())
            }
            Self::Abort {
                version,
                transition_id,
                ..
            } => {
                check_version(*version)?;
                if !canonical_uuid(transition_id) {
                    return Err(INVALID.into());
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HostMessage<T> {
    Prepared {
        version: u16,
        request_id: String,
        challenge: Box<T>,
    },
    Finished {
        version: u16,
        request_id: String,
        transition_id: String,
    },
    Aborted {
        version: u16,
        request_id: String,
        transition_id: String,
    },
    Rejected {
        version: u16,
        request_id: String,
        code: RejectionCode,
    },
}

impl<T> HostMessage<T> {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let (version, request_id) = match self {
            Self::Prepared {
                version,
                request_id,
                ..
            }
            | Self::Finished {
                version,
                request_id,
                ..
            }
            | Self::Aborted {
                version,
                request_id,
                ..
            }
            | Self::Rejected {
                version,
                request_id,
                ..
            } => (*version, request_id),
        };
        check_version(version)?;
        if !canonical_uuid(request_id) {
            return Err(INVALID.into());
        }
        match self {
            Self::Finished { transition_id, .. } | Self::Aborted { transition_id, .. }
                if !canonical_uuid(transition_id) =>
            {
                Err(INVALID.into())
            }
            _ => Ok(()),
        }
    }
}

pub(crate) fn read_main<T: DeserializeOwned>(
    reader: &mut impl Read,
) -> Result<MainMessage<T>, String> {
    let mut length = [0; 4];
    reader
        .read_exact(&mut length)
        .map_err(|_| INVALID.to_string())?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(INVALID.into());
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    reader
        .read_exact(&mut bytes)
        .map_err(|_| INVALID.to_string())?;
    let message: MainMessage<T> =
        serde_json::from_slice(&bytes).map_err(|_| INVALID.to_string())?;
    message.validate()?;
    Ok(message)
}

pub(crate) fn write_host<T: Serialize>(
    writer: &mut impl Write,
    message: &HostMessage<T>,
) -> Result<(), String> {
    message.validate()?;
    let bytes = Zeroizing::new(serde_json::to_vec(message).map_err(|_| INVALID.to_string())?);
    if bytes.is_empty() || bytes.len() > MAX_MESSAGE_BYTES {
        return Err(INVALID.into());
    }
    let length = u32::try_from(bytes.len()).map_err(|_| INVALID.to_string())?;
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|()| writer.write_all(&bytes))
        .and_then(|()| writer.flush())
        .map_err(|_| INVALID.to_string())?;
    Ok(())
}

fn check_version(version: u16) -> Result<(), String> {
    if version == VERSION {
        Ok(())
    } else {
        Err(INVALID.into())
    }
}

fn canonical_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}

pub(crate) fn is_canonical_transition_id(value: &str) -> bool {
    canonical_uuid(value)
}

fn canonical_token(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    const REQUEST: &str = "123e4567-e89b-42d3-a456-426614174000";
    const TRANSITION: &str = "123e4567-e89b-42d3-a456-426614174001";

    fn frame(value: serde_json::Value) -> Vec<u8> {
        let payload = serde_json::to_vec(&value).unwrap();
        let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
        bytes.extend(payload);
        bytes
    }

    fn prepare() -> serde_json::Value {
        json!({"version":1,"type":"prepare","request_id":REQUEST})
    }

    #[test]
    fn accepts_exact_prepare_and_preserves_following_frame() {
        let mut bytes = frame(prepare());
        bytes.extend(frame(json!({
            "version":1,"type":"abort","request_id":REQUEST,"transition_id":TRANSITION
        })));
        let mut input = Cursor::new(bytes);
        let decoded: MainMessage<serde_json::Value> = read_main(&mut input).unwrap();
        assert!(matches!(decoded, MainMessage::Prepare { .. }));
        let decoded: MainMessage<serde_json::Value> = read_main(&mut input).unwrap();
        assert!(matches!(decoded, MainMessage::Abort { .. }));
    }

    #[test]
    fn rejects_bad_version_unknown_duplicate_and_noncanonical_identity() {
        let mut bad_version = prepare();
        bad_version["version"] = json!(2);
        let mut unknown = prepare();
        unknown["registration"] = json!("must-not-be-a-wire-field");
        let duplicate = format!(
            "{{\"version\":1,\"type\":\"prepare\",\"request_id\":\"{REQUEST}\",\"request_id\":\"{REQUEST}\"}}"
        );
        let mut invalid_id = prepare();
        invalid_id["request_id"] = json!(REQUEST.to_uppercase());
        let invalid_transition = json!({
            "version":1,"type":"abort","request_id":REQUEST,"transition_id":"not-a-uuid"
        });
        for bytes in [frame(bad_version), frame(unknown), frame(invalid_id)] {
            let mut input = Cursor::new(bytes);
            assert!(read_main::<serde_json::Value>(&mut input).is_err());
        }
        let mut duplicate_input = Cursor::new({
            let bytes = duplicate.as_bytes();
            let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
            framed.extend_from_slice(bytes);
            framed
        });
        assert!(read_main::<serde_json::Value>(&mut duplicate_input).is_err());
        assert!(
            read_main::<serde_json::Value>(&mut Cursor::new(frame(invalid_transition))).is_err()
        );
    }

    #[test]
    fn rejects_oversized_or_truncated_frames_before_decoding() {
        let mut oversized = Cursor::new(((MAX_MESSAGE_BYTES + 1) as u32).to_be_bytes());
        assert!(read_main::<serde_json::Value>(&mut oversized).is_err());
        let mut truncated = Cursor::new([0, 0, 0, 12, b'{', b'}']);
        assert!(read_main::<serde_json::Value>(&mut truncated).is_err());
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CompletionStub {
        transition_id: String,
    }

    #[test]
    fn typed_completion_rejects_nested_unknown_fields() {
        let valid_finish = json!({
            "version":1,"type":"finish","request_id":REQUEST,
            "completion":{"transition_id":TRANSITION},
            "new_device_token":"0123456789abcdef0123456789abcdef",
            "new_source_token":"fedcba9876543210fedcba9876543210"
        });
        let decoded = read_main::<CompletionStub>(&mut Cursor::new(frame(valid_finish))).unwrap();
        let MainMessage::Finish { completion, .. } = decoded else {
            panic!("the finish frame has the right variant");
        };
        assert_eq!(completion.transition_id, TRANSITION);

        let finish = json!({
            "version":1,"type":"finish","request_id":REQUEST,
            "completion":{"transition_id":TRANSITION,"unexpected":"private-data"},
            "new_device_token":"0123456789abcdef0123456789abcdef",
            "new_source_token":"fedcba9876543210fedcba9876543210"
        });
        assert!(read_main::<CompletionStub>(&mut Cursor::new(frame(finish))).is_err());
    }

    #[test]
    fn finish_tokens_are_exact_and_host_rejection_codes_are_closed_set() {
        let finish = json!({
            "version":1,"type":"finish","request_id":REQUEST,
            "completion":{"placeholder":true},
            "new_device_token":"0123456789abcdef0123456789abcdef",
            "new_source_token":"fedcba9876543210fedcba9876543210"
        });
        let mut input = Cursor::new(frame(finish));
        let _: MainMessage<serde_json::Value> = read_main(&mut input).unwrap();

        let bad_token = json!({
            "version":1,"type":"finish","request_id":REQUEST,
            "completion":{"placeholder":true},
            "new_device_token":"0123456789ABCDEF0123456789ABCDEF",
            "new_source_token":"fedcba9876543210fedcba9876543210"
        });
        assert!(read_main::<serde_json::Value>(&mut Cursor::new(frame(bad_token))).is_err());

        let unknown_code = json!({
            "version":1,"type":"rejected","request_id":REQUEST,"code":"private-error-text"
        });
        assert!(serde_json::from_value::<HostMessage<serde_json::Value>>(unknown_code).is_err());
    }

    #[test]
    fn writes_bounded_versioned_reply_with_exact_shape() {
        let reply = HostMessage::<serde_json::Value>::Rejected {
            version: VERSION,
            request_id: REQUEST.into(),
            code: RejectionCode::ProtocolError,
        };
        let mut bytes = Vec::new();
        write_host(&mut bytes, &reply).unwrap();
        let length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(length, bytes.len() - 4);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes[4..]).unwrap(),
            json!({"version":1,"type":"rejected","request_id":REQUEST,"code":"protocol_error"})
        );
    }

    #[test]
    fn oversized_host_reply_is_refused_before_writing_a_prefix() {
        let reply = HostMessage::Prepared {
            version: VERSION,
            request_id: REQUEST.into(),
            challenge: Box::new("x".repeat(MAX_MESSAGE_BYTES)),
        };
        let mut bytes = Vec::new();
        assert!(write_host(&mut bytes, &reply).is_err());
        assert!(bytes.is_empty());
    }
}
