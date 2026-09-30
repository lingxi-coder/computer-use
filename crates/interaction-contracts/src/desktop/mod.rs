//! desktop v1: the wire protocol between the desktop processes.
//!
//! It lives in its own namespace and version. The I0 protocol in [`crate::i0`]
//! stays frozen; the primitives here are a superset of I0's inputs so the
//! execution core can run both.
//!
//! Two styles coexist on purpose. The execution-facing types in [`device`]
//! keep plain `String` identifiers like I0, because the device must be able to
//! answer a malformed request with a receipt instead of failing to decode it.
//! Everything above the device uses the validated newtype identifiers from
//! [`ids`].

pub const PROTOCOL_VERSION: u16 = 1;
pub const NAMESPACE: &str = "desktop";

pub mod binding;
pub mod decision;
pub mod device;
pub mod effect;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod ids;
pub mod message;
pub mod operator;
pub mod planning;
pub mod primitive;
pub mod samples;
pub mod source;
pub mod task;
pub mod verification;

/// Reject empty, whitespace-only and control-character text, like I0 does.
pub(crate) fn require_nonempty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must not be empty or whitespace-only"))
    } else if value.chars().any(char::is_control) {
        Err(format!("{field} must not contain control characters"))
    } else {
        Ok(())
    }
}
