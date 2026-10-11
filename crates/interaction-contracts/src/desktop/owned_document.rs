//! Typed evidence for a host-registered, owned plain-text document receiver.
//!
//! This contract is not a public request and does not mutate the generic
//! structure snapshot. Raw AX objects stay process-local; this evidence uses
//! snapshot-scoped references and explicit, value-only AX observations.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ids::{ObjectRef, RequestId};
use super::require_nonempty;
use super::source::{KernelProcessIdentity, Rect, SourceIdentityBoundary};

const AX_ERROR_SUCCESS: i32 = 0;
const AX_ERROR_ATTRIBUTE_UNSUPPORTED: i32 = -25205;
const AX_ERROR_NO_VALUE: i32 = -25212;
const MAX_CLOCK_DOMAIN_BYTES: usize = 128;
const MAX_REGISTRATION_ID_BYTES: usize = 256;
const MAX_ANCESTORS: usize = 8;

/// The only setter profile admitted by this first owned-document contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OwnedDocumentProfile {
    OwnedPlainTextValueSetterV1,
}

/// An AX property's observed state. The error code is retained exactly; a
/// missing read is not represented as an AX error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AxPropertyObservation<T> {
    Value {
        value: T,
    },
    Unsupported {
        code: i32,
    },
    NoValue {
        code: i32,
    },
    Failure {
        code: i32,
    },
    DecodeFailure {
        api_code: i32,
        actual_cf_type_id: Option<u64>,
    },
    NotAttempted {
        reason: String,
    },
}

impl<T> AxPropertyObservation<T> {
    pub fn validate(&self) -> Result<(), String> {
        self.validate_status("AX property")
    }

    fn validate_status(&self, field: &str) -> Result<(), String> {
        match self {
            Self::Value { .. } => Ok(()),
            Self::Unsupported { code } if *code == AX_ERROR_ATTRIBUTE_UNSUPPORTED => Ok(()),
            Self::Unsupported { .. } => Err(format!(
                "{field} unsupported status must retain AX error {AX_ERROR_ATTRIBUTE_UNSUPPORTED}"
            )),
            Self::NoValue { code } if *code == AX_ERROR_NO_VALUE => Ok(()),
            Self::NoValue { .. } => Err(format!(
                "{field} no-value status must retain AX error {AX_ERROR_NO_VALUE}"
            )),
            Self::Failure { code }
                if *code != AX_ERROR_SUCCESS
                    && *code != AX_ERROR_ATTRIBUTE_UNSUPPORTED
                    && *code != AX_ERROR_NO_VALUE =>
            {
                Ok(())
            }
            Self::Failure { .. } => Err(format!(
                "{field} failure must retain a non-success AX error"
            )),
            Self::DecodeFailure { api_code: 0, .. } => Ok(()),
            Self::DecodeFailure { .. } => Err(format!(
                "{field} decode failure must retain API success code {AX_ERROR_SUCCESS}"
            )),
            Self::NotAttempted { reason } => {
                require_nonempty(&format!("{field} not-attempted reason"), reason)
            }
        }
    }
}

/// Public CFRange facts; raw AXValue/CF objects are never serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkedTextRange {
    pub location: i64,
    pub length: i64,
}

impl MarkedTextRange {
    fn validate_unmarked(self) -> Result<(), String> {
        if self.location < 0 || self.length != 0 {
            return Err(
                "owned plain-text receiver must have a nonnegative empty marked range".into(),
            );
        }
        Ok(())
    }
}

/// One actual marked-range read. The receiver object reference and same-CF
/// result are value facts; the CF object itself never crosses the process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkedRangeRead {
    pub receiver_object_ref: ObjectRef,
    pub receiver_same_cf_as_actual_object_ref: bool,
    #[schemars(range(min = 1))]
    pub observed_ms: u64,
    pub observation: AxPropertyObservation<MarkedTextRange>,
}

impl MarkedRangeRead {
    fn validate_for_receiver(
        &self,
        actual_object_ref: &ObjectRef,
    ) -> Result<MarkedTextRange, String> {
        if self.receiver_object_ref != *actual_object_ref
            || !self.receiver_same_cf_as_actual_object_ref
            || self.observed_ms == 0
        {
            return Err("marked-range read is not tied to the exact receiver object".into());
        }
        self.observation.validate_status("AXTextInputMarkedRange")?;
        match &self.observation {
            AxPropertyObservation::Value { value } => {
                value.validate_unmarked()?;
                Ok(*value)
            }
            _ => Err("marked-range read must be an actual Value observation".into()),
        }
    }
}

/// Label-free parent facts from the focused receiver up to its AX window.
/// Ordering is immediate parent first and the window last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnedDocumentAncestor {
    pub object_ref: ObjectRef,
    #[schemars(length(min = 1, max = 128))]
    pub role: String,
}

impl OwnedDocumentAncestor {
    fn validate(&self) -> Result<(), String> {
        require_nonempty("owned document ancestor role", &self.role)?;
        if self.role.len() > 128 || self.role == "AXSecureTextField" {
            return Err("owned document ancestry contains an invalid or secure role".into());
        }
        Ok(())
    }
}

/// Host-private provenance for the one current Source observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnedDocumentSourceProvenance {
    pub identity_boundary: SourceIdentityBoundary,
    pub snapshot_id: RequestId,
    #[schemars(range(min = 1))]
    pub source_sequence: u64,
    #[schemars(range(min = 1))]
    pub observed_ms: u64,
    #[schemars(range(min = 1))]
    pub valid_until_ms: u64,
    #[schemars(length(min = 1, max = 128))]
    pub clock_domain: String,
}

impl OwnedDocumentSourceProvenance {
    fn validate(&self) -> Result<(), String> {
        self.identity_boundary.validate()?;
        require_nonempty("owned document clock domain", &self.clock_domain)?;
        if self.source_sequence == 0
            || self.observed_ms == 0
            || self.valid_until_ms <= self.observed_ms
            || self.clock_domain.len() > MAX_CLOCK_DOMAIN_BYTES
        {
            return Err("owned document source provenance is invalid".into());
        }
        Ok(())
    }

    fn validate_at(&self, now_ms: u64) -> Result<(), String> {
        self.validate()?;
        if now_ms < self.observed_ms || now_ms >= self.valid_until_ms {
            return Err("owned document source provenance is stale or from the future".into());
        }
        Ok(())
    }
}

/// Whether the registered Source-owned observer set is still suitable for a
/// current receiver read. Registration alone is not notification delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OwnedScopeObserverState {
    Continuous,
    CoverageIncomplete,
    ChangedDuringRead,
    NotAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentReadStatus {
    Value,
    Unsupported,
    NoValue,
    Failure,
    DecodeFailure,
    NotAttempted,
}

/// Diagnostics for an actual public AXDocument read, without its private URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentBindingRead {
    pub status: DocumentReadStatus,
    pub api_code: Option<i32>,
    pub cf_type_id: Option<u64>,
    pub observed_ms: u64,
    pub timely: bool,
    pub registered_path_matches: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentBindingEvidence {
    pub before: DocumentBindingRead,
    pub after: DocumentBindingRead,
}

impl DocumentBindingEvidence {
    fn validate(&self, provenance: &OwnedDocumentSourceProvenance) -> Result<(), String> {
        for read in [&self.before, &self.after] {
            if read.status != DocumentReadStatus::Value
                || read.api_code != Some(AX_ERROR_SUCCESS)
                || read.cf_type_id.is_none_or(|id| id == 0)
                || !read.timely
                || !read.registered_path_matches
                || read.observed_ms < provenance.observed_ms
                || read.observed_ms >= provenance.valid_until_ms
            {
                return Err("owned document public AXDocument binding is unavailable".into());
            }
        }
        if self.after.observed_ms < self.before.observed_ms {
            return Err("owned document AXDocument reads are out of order".into());
        }
        Ok(())
    }
}

/// An exact, bounded UTF-8 AXValue read from the already verified receiver.
/// This carries no document contents and grants no execution authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnedDocumentValueReadback {
    pub receiver_object_ref: ObjectRef,
    pub receiver_same_cf_as_actual_object_ref: bool,
    #[schemars(range(min = 1))]
    pub observed_ms: u64,
    #[schemars(length(min = 64, max = 64))]
    pub sha256: String,
    #[schemars(range(max = 2048))]
    pub utf8_byte_count: u32,
}

impl OwnedDocumentValueReadback {
    fn validate_for_receiver(
        &self,
        receiver: &ObjectRef,
        provenance: &OwnedDocumentSourceProvenance,
    ) -> Result<(), String> {
        if self.receiver_object_ref != *receiver
            || !self.receiver_same_cf_as_actual_object_ref
            || self.observed_ms < provenance.observed_ms
            || self.observed_ms >= provenance.valid_until_ms
            || self.utf8_byte_count > 2048
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("owned AXValue readback is invalid or uncorrelated".into());
        }
        Ok(())
    }
}

/// One Source-produced receiver record for a Host-private owned-document
/// registration. This is a proof input, not a grant that a model can request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnedDocumentReceiverEvidence {
    /// Opaque key resolved only through Host's private owner registry.
    #[schemars(length(min = 1, max = 256))]
    pub registration_id: String,
    pub profile: OwnedDocumentProfile,
    pub provenance: OwnedDocumentSourceProvenance,
    pub process: KernelProcessIdentity,
    #[schemars(length(min = 1, max = 256))]
    pub window_id: String,
    pub window_object_ref: ObjectRef,
    pub window_frame: Rect,
    pub actual_object_ref: ObjectRef,
    pub receiver_frame: Rect,
    /// Both independent focus reads name the same current receiver object.
    pub application_focused_object_ref: ObjectRef,
    pub system_focused_object_ref: ObjectRef,
    #[schemars(length(min = 1, max = 8))]
    pub ancestry: Vec<OwnedDocumentAncestor>,
    /// This profile's local completeness fact is independent of generic
    /// StructureSnapshot completeness and health.
    pub scope_complete: bool,
    /// Profile-specific predicate is carried as a typed fact and is
    /// independently rechecked against the observations below.
    pub profile_eligible: bool,
    pub marked_range_stable_unmarked: bool,
    pub observer_state: OwnedScopeObserverState,
    pub secure: bool,
    pub document_binding_verified: bool,
    pub document_binding: DocumentBindingEvidence,
    /// Preserved for display-free diagnostics, never used to bind identity.
    pub title: AxPropertyObservation<String>,
    pub role: AxPropertyObservation<String>,
    pub subrole: AxPropertyObservation<String>,
    pub enabled: AxPropertyObservation<bool>,
    pub focused: AxPropertyObservation<bool>,
    pub value_settable: AxPropertyObservation<bool>,
    /// Preserved as raw typed evidence, never used as a label or identity.
    pub description: AxPropertyObservation<String>,
    pub marked_range_before: MarkedRangeRead,
    pub marked_range_after: MarkedRangeRead,
    /// None means no complete timely value read; it never denotes an empty value.
    pub value_readback: Option<OwnedDocumentValueReadback>,
}

impl OwnedDocumentReceiverEvidence {
    /// Validate the profile's evidence facts without deciding freshness.
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("owned document registration id", &self.registration_id)?;
        if self.registration_id.len() > MAX_REGISTRATION_ID_BYTES {
            return Err("owned document registration id exceeds its bound".into());
        }
        self.provenance.validate()?;
        if !self.document_binding_verified {
            return Err("owned document receiver is not bound to its registered document".into());
        }
        self.document_binding.validate(&self.provenance)?;
        if let Some(readback) = &self.value_readback {
            readback.validate_for_receiver(&self.actual_object_ref, &self.provenance)?;
        }
        self.process.validate()?;
        require_nonempty("owned document window id", &self.window_id)?;
        if self.window_id.len() > 256
            || !self.window_frame.valid_bounded()
            || !self.receiver_frame.valid_bounded()
            || !self.window_frame.contains_rect(&self.receiver_frame)
        {
            return Err("owned document window or receiver geometry is invalid".into());
        }
        if !self.scope_complete || self.observer_state != OwnedScopeObserverState::Continuous {
            return Err(
                "owned document receiver scope is incomplete or observer is unhealthy".into(),
            );
        }
        if self.secure {
            return Err(
                "secure text receivers are not eligible for owned plain-text setting".into(),
            );
        }
        if self.actual_object_ref == self.window_object_ref
            || self.application_focused_object_ref != self.actual_object_ref
            || self.system_focused_object_ref != self.actual_object_ref
        {
            return Err(
                "owned document receiver does not match both current focus references".into(),
            );
        }
        if self.ancestry.is_empty() || self.ancestry.len() > MAX_ANCESTORS {
            return Err("owned document receiver ancestry is incomplete or over budget".into());
        }
        let mut seen = BTreeSet::new();
        seen.insert(self.actual_object_ref.clone());
        for ancestor in &self.ancestry {
            ancestor.validate()?;
            if !seen.insert(ancestor.object_ref.clone()) {
                return Err("owned document ancestry contains a repeated object reference".into());
            }
        }
        let window_ancestor = self.ancestry.last().expect("nonempty ancestry was checked");
        if window_ancestor.object_ref != self.window_object_ref
            || window_ancestor.role != "AXWindow"
        {
            return Err("owned document ancestry must terminate at its actual AX window".into());
        }

        self.title.validate_status("AXTitle")?;
        self.role.validate_status("AXRole")?;
        self.subrole.validate_status("AXSubrole")?;
        self.enabled.validate_status("AXEnabled")?;
        self.focused.validate_status("AXFocused")?;
        self.value_settable.validate_status("AXValue settable")?;
        self.description.validate_status("AXDescription")?;

        match &self.role {
            AxPropertyObservation::Value { value } if value == "AXTextArea" => {}
            _ => {
                return Err(
                    "owned plain-text profile requires an explicitly observed AXTextArea".into(),
                );
            }
        }
        match &self.subrole {
            AxPropertyObservation::Value { value } if value.is_empty() => {}
            AxPropertyObservation::Unsupported { code }
                if *code == AX_ERROR_ATTRIBUTE_UNSUPPORTED => {}
            _ => {
                return Err(
                    "owned plain-text profile requires an empty subrole or AXUnsupported".into(),
                );
            }
        }
        match &self.enabled {
            AxPropertyObservation::Value { value: true } => {}
            AxPropertyObservation::Unsupported { code }
                if *code == AX_ERROR_ATTRIBUTE_UNSUPPORTED => {}
            _ => {
                return Err(
                    "owned plain-text profile requires enabled=true or explicit AXUnsupported"
                        .into(),
                );
            }
        }
        if !matches!(&self.focused, AxPropertyObservation::Value { value: true }) {
            return Err("owned plain-text profile requires focused=true".into());
        }
        if !matches!(
            &self.value_settable,
            AxPropertyObservation::Value { value: true }
        ) {
            return Err("owned plain-text profile requires AXValue settable=true".into());
        }
        let before = self
            .marked_range_before
            .validate_for_receiver(&self.actual_object_ref)?;
        let after = self
            .marked_range_after
            .validate_for_receiver(&self.actual_object_ref)?;
        if !self.profile_eligible
            || !self.marked_range_stable_unmarked
            || before != after
            || self.marked_range_before.observed_ms < self.provenance.observed_ms
            || self.marked_range_after.observed_ms < self.marked_range_before.observed_ms
            || self.marked_range_after.observed_ms >= self.provenance.valid_until_ms
        {
            return Err(
                "owned document profile or marked-range proof is ineligible or outside source time bounds"
                    .into(),
            );
        }
        Ok(())
    }

    /// Validate both profile eligibility and current observation freshness.
    pub fn validate_at(&self, now_ms: u64) -> Result<(), String> {
        self.validate()?;
        self.provenance.validate_at(now_ms)?;
        if now_ms < self.marked_range_after.observed_ms
            || now_ms < self.document_binding.after.observed_ms
            || self
                .value_readback
                .as_ref()
                .is_some_and(|readback| now_ms < readback.observed_ms)
        {
            return Err("owned receiver evidence is from the future".into());
        }
        Ok(())
    }

    pub fn eligible_at(&self, now_ms: u64) -> bool {
        self.validate_at(now_ms).is_ok()
    }
}
