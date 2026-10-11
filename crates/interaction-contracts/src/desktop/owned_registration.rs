//! Main-issued registration carried only on private process startup pipes.
//! It is not an Envelope payload, public setting, input grant or model value.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::source::KernelProcessIdentity;

pub const MAX_OWNED_REGISTRATION_BYTES: usize = 8_192;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedDocumentRegistration {
    pub version: u16,
    pub registration_id: String,
    pub pid: u32,
    pub start_seconds: u64,
    pub start_micros: u64,
    pub executable_path: String,
    pub bundle_id: String,
    pub document_path: String,
    pub document_sha256: String,
    pub document_dev: u64,
    pub document_ino: u64,
    pub directory_dev: u64,
    pub directory_ino: u64,
    pub issued_unix_ms: u64,
}

impl fmt::Debug for OwnedDocumentRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedDocumentRegistration([redacted])")
    }
}

impl OwnedDocumentRegistration {
    pub fn process_identity(&self) -> KernelProcessIdentity {
        KernelProcessIdentity {
            pid: self.pid,
            start_seconds: self.start_seconds,
            start_micros: self.start_micros,
        }
    }

    /// Structural identity remains valid after admission; source proofs have
    /// their own short expiry. Startup freshness is checked separately below.
    pub fn validate_identity(&self) -> Result<(), String> {
        let canonical_uuid = self.registration_id.len() == 36
            && self
                .registration_id
                .bytes()
                .enumerate()
                .all(|(index, byte)| {
                    if [8, 13, 18, 23].contains(&index) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                    }
                });
        let digest = self.document_sha256.len() == 64
            && self
                .document_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        let path = &self.document_path;
        let normalized_path = path.starts_with('/')
            && path.len() <= 4_096
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .skip(1)
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && path.rsplit('/').next() == Some("document.txt");
        if self.version != 1
            || !canonical_uuid
            || self.pid == 0
            || self.pid > i32::MAX as u32
            || self.start_seconds == 0
            || self.start_micros >= 1_000_000
            || self.executable_path != "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
            || self.bundle_id != "com.apple.TextEdit"
            || !normalized_path
            || !digest
            || self.document_ino == 0
            || self.directory_ino == 0
            || self.issued_unix_ms == 0
            || [
                self.document_dev,
                self.document_ino,
                self.directory_dev,
                self.directory_ino,
            ]
            .iter()
            .any(|value| *value > 9_007_199_254_740_991)
        {
            return Err("invalid private owned document registration".into());
        }
        Ok(())
    }

    pub fn validate_at(&self, now_unix_ms: u64) -> Result<(), String> {
        self.validate_identity()?;
        let fresh = self.issued_unix_ms > 0
            && if now_unix_ms < self.issued_unix_ms {
                self.issued_unix_ms - now_unix_ms <= 1_000
            } else {
                now_unix_ms - self.issued_unix_ms <= 5_000
            };
        if !fresh {
            return Err("invalid private owned document registration".into());
        }
        Ok(())
    }
}
