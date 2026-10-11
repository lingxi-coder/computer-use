//! Private, Main-issued owned-document context on cu-device's stdin pipe.
//! This startup prefix is not a desktop protocol message or input grant.

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use interaction_contracts::desktop::owned_registration::{
    MAX_OWNED_REGISTRATION_BYTES, OwnedDocumentRegistration,
};
use zeroize::Zeroizing;

const INVALID: &str = "invalid private owned document registration";

pub struct OwnedDocumentBootstrap(OwnedDocumentRegistration);

impl OwnedDocumentBootstrap {
    /// Read one bounded 4-byte big-endian length-prefixed registration.
    pub fn read(reader: &mut impl Read) -> Result<Self, String> {
        let registration = Self::decode(reader)?;
        // Reading a pipe can block; validate freshness after the payload arrives.
        let now_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| INVALID.to_string())?
            .as_millis()
            .try_into()
            .map_err(|_| INVALID.to_string())?;
        registration
            .validate_at(now_unix_ms)
            .map_err(|_| INVALID.to_string())?;
        Ok(Self(registration))
    }

    #[cfg(test)]
    fn read_at(reader: &mut impl Read, now_unix_ms: u64) -> Result<Self, String> {
        let registration = Self::decode(reader)?;
        registration
            .validate_at(now_unix_ms)
            .map_err(|_| INVALID.to_string())?;
        Ok(Self(registration))
    }

    fn decode(reader: &mut impl Read) -> Result<OwnedDocumentRegistration, String> {
        let mut length = [0; 4];
        reader
            .read_exact(&mut length)
            .map_err(|_| INVALID.to_string())?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > MAX_OWNED_REGISTRATION_BYTES {
            return Err(INVALID.into());
        }
        let mut bytes = Zeroizing::new(vec![0; length]);
        reader
            .read_exact(&mut bytes)
            .map_err(|_| INVALID.to_string())?;
        let registration: OwnedDocumentRegistration =
            serde_json::from_slice(&bytes).map_err(|_| INVALID.to_string())?;
        Ok(registration)
    }

    pub fn into_registration(self) -> Box<OwnedDocumentRegistration> {
        Box::new(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_800_000_000_000;

    fn registration() -> serde_json::Value {
        json!({
            "version": 1,
            "registration_id": "123e4567-e89b-42d3-a456-426614174000",
            "pid": 1234,
            "start_seconds": 1_700_000_000,
            "start_micros": 123456,
            "executable_path": "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
            "bundle_id": "com.apple.TextEdit",
            "document_path": "/private/tmp/owned/document.txt",
            "document_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "document_dev": 1,
            "document_ino": 2,
            "directory_dev": 3,
            "directory_ino": 4,
            "issued_unix_ms": NOW,
        })
    }

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(payload);
        bytes
    }

    fn read_payload(payload: &[u8]) -> Result<OwnedDocumentBootstrap, String> {
        OwnedDocumentBootstrap::read_at(&mut &frame(payload)[..], NOW)
    }

    #[test]
    fn strict_registration_is_read_without_consuming_configure() {
        let payload = serde_json::to_vec(&registration()).unwrap();
        let mut bytes = frame(&payload);
        bytes.extend_from_slice(b"CONFIGURE");
        let mut input = &bytes[..];
        let bootstrap = OwnedDocumentBootstrap::read_at(&mut input, NOW).unwrap();
        assert_eq!(bootstrap.0.pid, 1234);
        assert_eq!(input, b"CONFIGURE");
    }

    #[test]
    fn duplicate_unknown_missing_stale_and_oversized_payloads_are_rejected() {
        let payload = serde_json::to_vec(&registration()).unwrap();
        let duplicate = format!(
            "{} , \"pid\":1234}}",
            String::from_utf8(payload[..payload.len() - 1].to_vec()).unwrap()
        );
        assert!(read_payload(duplicate.as_bytes()).is_err());

        let mut unknown = registration();
        unknown["extra"] = json!(true);
        assert!(read_payload(serde_json::to_string(&unknown).unwrap().as_bytes()).is_err());

        let mut missing = registration();
        missing.as_object_mut().unwrap().remove("document_path");
        assert!(read_payload(serde_json::to_string(&missing).unwrap().as_bytes()).is_err());

        for issued in [NOW - 5_001, NOW + 1_001] {
            let mut stale = registration();
            stale["issued_unix_ms"] = json!(issued);
            assert!(read_payload(serde_json::to_string(&stale).unwrap().as_bytes()).is_err());
        }

        assert!(OwnedDocumentBootstrap::read_at(&mut &[0, 0, 0, 0][..], NOW).is_err());
        assert!(OwnedDocumentBootstrap::read_at(&mut &[0, 0, 0, 2, b'{'][..], NOW).is_err());
        let oversized = ((MAX_OWNED_REGISTRATION_BYTES + 1) as u32).to_be_bytes();
        assert!(OwnedDocumentBootstrap::read_at(&mut &oversized[..], NOW).is_err());
    }

    #[test]
    fn registration_debug_and_reader_errors_redact_document_identity() {
        let mut value = registration();
        value["document_path"] = json!("/private/customer-secret/document.txt");
        value["document_sha256"] = json!("f".repeat(64));
        let payload = serde_json::to_vec(&value).unwrap();
        let bootstrap = OwnedDocumentBootstrap::read_at(&mut &frame(&payload)[..], NOW).unwrap();
        let debug = format!("{:?}", bootstrap.0);
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("customer-secret"));
        assert!(!debug.contains(&"f".repeat(64)));

        let malformed = br#"{"document_path":"/private/customer-secret/document.txt","document_sha256":"private-hash"}"#;
        let error = match OwnedDocumentBootstrap::read_at(&mut &frame(malformed)[..], NOW) {
            Ok(_) => panic!("malformed registration was accepted"),
            Err(error) => error,
        };
        assert!(!error.contains("customer-secret"));
        assert!(!error.contains("private-hash"));
    }
}
