//! Dedicated text actions for a Host-registered owned document.
//!
//! These plans carry only the private Source proof and the frozen expected
//! value. They do not synthesize generic bindings, objects, coordinates, or
//! keyboard actions.

use std::fmt;

use interaction_contracts::desktop::device::ObservationPurpose;
use interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof;
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnedDocumentActionKind {
    SetValue,
    Save,
}

#[derive(Clone, PartialEq)]
pub struct OwnedDocumentActionPlan {
    proof: OwnedDocumentBindingProof,
    kind: OwnedDocumentActionKind,
    expected_value: String,
}

impl fmt::Debug for OwnedDocumentActionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedDocumentActionPlan([private action])")
    }
}

impl OwnedDocumentActionPlan {
    /// Validate and create the dedicated atomic AX SetValue action.
    pub fn new(proof: OwnedDocumentBindingProof, value: String) -> Result<Self, &'static str> {
        proof
            .validate()
            .map_err(|_| "invalid owned document action proof")?;
        validate_setter_expected_value(&value)?;
        Ok(Self {
            proof,
            kind: OwnedDocumentActionKind::SetValue,
            expected_value: value,
        })
    }

    /// Create a save action only when the current private AXValue digest proves
    /// the frozen expected text is already present in this receiver.
    pub fn save(
        proof: OwnedDocumentBindingProof,
        expected_value: String,
    ) -> Result<Self, &'static str> {
        proof
            .validate()
            .map_err(|_| "invalid owned document action proof")?;
        validate_save_expected_value(&expected_value)?;
        let bytes = expected_value.as_bytes();
        let expected_digest = format!("{:x}", Sha256::digest(bytes));
        let matches = proof
            .receiver
            .value_readback
            .as_ref()
            .is_some_and(|readback| {
                readback.utf8_byte_count as usize == bytes.len()
                    && readback.sha256 == expected_digest
            });
        if !matches {
            return Err("owned document Save requires a matching private AXValue readback");
        }
        Ok(Self {
            proof,
            kind: OwnedDocumentActionKind::Save,
            expected_value,
        })
    }

    /// The exact private scope used to authorize the action observation.
    pub fn proof(&self) -> &OwnedDocumentBindingProof {
        &self.proof
    }

    pub fn kind(&self) -> OwnedDocumentActionKind {
        self.kind
    }

    pub fn observation_purpose(&self) -> ObservationPurpose {
        match self.kind {
            OwnedDocumentActionKind::SetValue => ObservationPurpose::OwnedDocumentValueSetter,
            OwnedDocumentActionKind::Save => ObservationPurpose::OwnedDocumentSave,
        }
    }

    pub fn expected_value(&self) -> &str {
        &self.expected_value
    }

    pub(crate) fn primitive(&self) -> Primitive {
        Primitive::AxAction {
            action: match self.kind {
                OwnedDocumentActionKind::SetValue => AxActionKind::SetValue {
                    value: self.expected_value.clone(),
                },
                OwnedDocumentActionKind::Save => AxActionKind::SaveOwnedDocument {
                    expected_value: self.expected_value.clone(),
                },
            },
        }
    }
}

fn validate_setter_expected_value(value: &str) -> Result<(), &'static str> {
    OwnedDocumentBindingProof::validate_setter_value(value)
        .map_err(|_| "owned document action value is outside its bounded text policy")
}

fn validate_save_expected_value(value: &str) -> Result<(), &'static str> {
    if value.contains('\0') || value.len() > 2048 {
        return Err("owned document Save value must be NUL-free and at most 2048 UTF-8 bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_value_policy_allows_controls_while_set_value_keeps_its_control_filter() {
        let expected = "before\u{0001}after";
        assert!(validate_save_expected_value(expected).is_ok());
        assert!(validate_setter_expected_value(expected).is_err());
        assert!(validate_save_expected_value(&"x".repeat(2_049)).is_err());
        assert!(validate_save_expected_value("bad\0value").is_err());
    }
}
