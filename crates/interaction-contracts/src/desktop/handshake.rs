//! Process handshake and lifecycle messages (design 4.6).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::browser::BrowserPageProfile;
use super::device::CapabilityManifest;
use super::require_nonempty;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PeerRole {
    Supervisor,
    Host,
    Device,
    Source,
    Perception,
    Decision,
    Operator,
}

/// A pid alone does not identify a process; the start time disambiguates reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    #[schemars(range(min = 1))]
    pub protocol_min: u16,
    #[schemars(range(min = 1))]
    pub protocol_max: u16,
    pub role: PeerRole,
    pub build_revision: String,
    pub process: ProcessIdentity,
    pub session_epoch: u64,
    pub worker_epoch: u64,
    /// One-time credential delivered over the inherited private pipe.
    #[schemars(length(min = 16))]
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HelloAck {
    pub negotiated_version: u16,
    pub role: PeerRole,
    pub session_epoch: u64,
}

/// Sent once the peer has finished its own self-checks. Only a ready
/// capability may be used to admit dependent requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    pub role: PeerRole,
    pub capabilities: Option<CapabilityManifest>,
    pub notes: Vec<String>,
}

/// What a supervisor tells a process it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Configure {
    pub role: PeerRole,
    pub session_epoch: u64,
    pub settings: BTreeMap<String, String>,
    /// Frozen by the trusted host for the owned browser fixture. It is never
    /// read from arbitrary settings or model-authored goal text.
    pub browser_page_profile: Option<BrowserPageProfile>,
}

impl Configure {
    pub fn validate(&self) -> Result<(), String> {
        if self.session_epoch == 0 || self.session_epoch > 9_007_199_254_740_991 {
            return Err("configure session_epoch must be positive and JS-safe".into());
        }
        if self.browser_page_profile.is_some()
            && !matches!(self.role, PeerRole::Host | PeerRole::Device)
        {
            return Err(
                "browser page profile is only valid for trusted Host or Device configure".into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Shutdown {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeError {
    NoCommonVersion {
        theirs: (u16, u16),
        ours: (u16, u16),
    },
    WrongRole {
        expected: PeerRole,
        got: PeerRole,
    },
    BadToken,
    StaleEpoch {
        expected: u64,
        got: u64,
    },
    ProcessMismatch,
    Malformed(String),
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCommonVersion { theirs, ours } => write!(
                f,
                "no common protocol version (peer {}..={}, ours {}..={})",
                theirs.0, theirs.1, ours.0, ours.1
            ),
            Self::WrongRole { expected, got } => {
                write!(f, "expected role {expected:?}, peer claims {got:?}")
            }
            Self::BadToken => f.write_str("handshake token rejected"),
            Self::StaleEpoch { expected, got } => {
                write!(f, "session epoch {got} is not the current {expected}")
            }
            Self::ProcessMismatch => f.write_str("peer process identity does not match"),
            Self::Malformed(reason) => write!(f, "malformed hello: {reason}"),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// What the listening side knows about the peer it expects.
#[derive(Debug, Clone)]
pub struct ExpectedPeer {
    pub role: PeerRole,
    pub token: String,
    pub session_epoch: u64,
    /// The pid the transport reported for the connection, when it can.
    pub transport_pid: Option<u32>,
}

/// The highest version both sides speak.
pub fn negotiate(theirs: (u16, u16), ours: (u16, u16)) -> Result<u16, HandshakeError> {
    let high = theirs.1.min(ours.1);
    let low = theirs.0.max(ours.0);
    if theirs.0 > theirs.1 || ours.0 > ours.1 || high < low {
        Err(HandshakeError::NoCommonVersion { theirs, ours })
    } else {
        Ok(high)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Hello {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("build_revision", &self.build_revision)?;
        if self.protocol_min == 0 || self.protocol_min > self.protocol_max {
            return Err("protocol range must be non-empty and start at 1".into());
        }
        if self.token.len() < 16 {
            return Err("handshake token is too short".into());
        }
        Ok(())
    }
}

/// Check a hello against what this side expects and produce the answer.
pub fn verify_hello(
    hello: &Hello,
    expected: &ExpectedPeer,
    supported: (u16, u16),
) -> Result<HelloAck, HandshakeError> {
    hello.validate().map_err(HandshakeError::Malformed)?;
    if hello.role != expected.role {
        return Err(HandshakeError::WrongRole {
            expected: expected.role,
            got: hello.role,
        });
    }
    if !constant_time_eq(hello.token.as_bytes(), expected.token.as_bytes()) {
        return Err(HandshakeError::BadToken);
    }
    if hello.session_epoch != expected.session_epoch {
        return Err(HandshakeError::StaleEpoch {
            expected: expected.session_epoch,
            got: hello.session_epoch,
        });
    }
    if let Some(pid) = expected.transport_pid
        && pid != hello.process.pid
    {
        return Err(HandshakeError::ProcessMismatch);
    }
    let negotiated_version = negotiate((hello.protocol_min, hello.protocol_max), supported)?;
    Ok(HelloAck {
        negotiated_version,
        role: hello.role,
        session_epoch: hello.session_epoch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::browser::BrowserPageProfile;

    const TOKEN: &str = "0123456789abcdef0123";

    fn hello() -> Hello {
        Hello {
            protocol_min: 1,
            protocol_max: 2,
            role: PeerRole::Device,
            build_revision: "test".into(),
            process: ProcessIdentity {
                pid: 100,
                start_time_ms: 5,
            },
            session_epoch: 3,
            worker_epoch: 1,
            token: TOKEN.into(),
        }
    }

    #[test]
    fn browser_profile_configure_is_limited_to_trusted_host_and_device_roles() {
        let configure = Configure {
            role: PeerRole::Device,
            session_epoch: 3,
            settings: BTreeMap::new(),
            browser_page_profile: Some(BrowserPageProfile::FixtureBackgroundOption),
        };
        assert!(configure.validate().is_ok());
        let mut source = configure;
        source.role = PeerRole::Source;
        assert!(source.validate().is_err());
    }

    fn expected() -> ExpectedPeer {
        ExpectedPeer {
            role: PeerRole::Device,
            token: TOKEN.into(),
            session_epoch: 3,
            transport_pid: Some(100),
        }
    }

    #[test]
    fn versions_negotiate_to_the_highest_common_one() {
        assert_eq!(negotiate((1, 2), (1, 1)), Ok(1));
        assert_eq!(negotiate((1, 3), (2, 5)), Ok(3));
        assert!(negotiate((3, 4), (1, 2)).is_err());
        assert!(
            negotiate((2, 1), (1, 2)).is_err(),
            "an inverted range is malformed"
        );
    }

    #[test]
    fn a_correct_hello_is_acknowledged_with_the_negotiated_version() {
        let ack = verify_hello(&hello(), &expected(), (1, 1)).unwrap();
        assert_eq!(ack.negotiated_version, 1);
        assert_eq!(ack.role, PeerRole::Device);
    }

    #[test]
    fn every_identity_check_can_reject() {
        let mut bad_role = expected();
        bad_role.role = PeerRole::Source;
        assert!(matches!(
            verify_hello(&hello(), &bad_role, (1, 1)),
            Err(HandshakeError::WrongRole { .. })
        ));
        let mut bad_token = expected();
        bad_token.token = "ffffffffffffffffffff".into();
        assert_eq!(
            verify_hello(&hello(), &bad_token, (1, 1)),
            Err(HandshakeError::BadToken)
        );
        let mut stale = expected();
        stale.session_epoch = 4;
        assert!(matches!(
            verify_hello(&hello(), &stale, (1, 1)),
            Err(HandshakeError::StaleEpoch { .. })
        ));
        let mut other_process = expected();
        other_process.transport_pid = Some(101);
        assert_eq!(
            verify_hello(&hello(), &other_process, (1, 1)),
            Err(HandshakeError::ProcessMismatch)
        );
        let mut future_only = hello();
        future_only.protocol_min = 5;
        future_only.protocol_max = 6;
        assert!(matches!(
            verify_hello(&future_only, &expected(), (1, 1)),
            Err(HandshakeError::NoCommonVersion { .. })
        ));
    }

    #[test]
    fn short_tokens_are_malformed() {
        let mut short = hello();
        short.token = "abc".into();
        assert!(matches!(
            verify_hello(&short, &expected(), (1, 1)),
            Err(HandshakeError::Malformed(_))
        ));
    }
}
