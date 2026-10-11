//! A semantic binding to an independently verified owned-document scope.
//! This descriptor is not a root grant, permit or native execution capability.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

use super::device::SurfaceRef;
use super::ids::{BindingRef, SemanticTargetRef};
use super::owned_document::OwnedDocumentReceiverEvidence;
use super::owned_registration::OwnedDocumentRegistration;

#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnedDocumentBindingProof {
    pub binding: BindingRef,
    pub semantic_target: SemanticTargetRef,
    #[schemars(range(min = 1))]
    pub environment_revision: u64,
    #[schemars(range(min = 1))]
    pub session_epoch: u64,
    pub surface: SurfaceRef,
    /// The conservative start time of the complete Source scope read.
    #[schemars(range(min = 1))]
    pub snapshot_observed_ms: u64,
    pub receiver: Box<OwnedDocumentReceiverEvidence>,
}

impl fmt::Debug for OwnedDocumentBindingProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedDocumentBindingProof([private scope])")
    }
}

impl OwnedDocumentBindingProof {
    pub fn validate(&self) -> Result<(), String> {
        self.receiver.validate()?;
        if self.environment_revision == 0
            || self.session_epoch == 0
            || self.surface.id.trim().is_empty()
            || self.surface.id.len() > 256
            || self.surface.id.chars().any(char::is_control)
            || self.surface.epoch == 0
            || self.snapshot_observed_ms < self.receiver.provenance.observed_ms
            || self.snapshot_observed_ms >= self.receiver.provenance.valid_until_ms
        {
            return Err("invalid owned document binding context".into());
        }
        Ok(())
    }

    pub fn validate_at(&self, now_ms: u64, max_age_ms: u64) -> Result<(), String> {
        self.validate()?;
        self.receiver.validate_at(now_ms)?;
        if now_ms < self.snapshot_observed_ms
            || now_ms.saturating_sub(self.snapshot_observed_ms) >= max_age_ms
            || now_ms.saturating_sub(self.receiver.provenance.observed_ms) >= max_age_ms
        {
            return Err("owned document binding is not fresh".into());
        }
        Ok(())
    }

    /// The exact value policy for the dedicated atomic setter. This validates
    /// content only; runtime mode, root, permit and native guards still apply.
    pub fn validate_setter_value(value: &str) -> Result<(), String> {
        if value.len() > 2048
            || value
                .chars()
                .any(|c| c.is_control() && !['\n', '\r', '\t'].contains(&c))
        {
            return Err("owned setter value exceeds its UTF-8 or control-character policy".into());
        }
        Ok(())
    }

    /// Resolve provenance against the private startup registration. This
    /// comparison alone does not grant input or relax runtime freshness.
    pub fn validate_registration(
        &self,
        registration: &OwnedDocumentRegistration,
    ) -> Result<(), String> {
        registration.validate_identity()?;
        self.validate()?;
        if self.receiver.registration_id != registration.registration_id
            || self.receiver.process != registration.process_identity()
        {
            return Err("owned document proof has no matching private registration".into());
        }
        Ok(())
    }

    /// Identity survives a new read or value change; it does not survive a
    /// receiver, window, process, namespace, generation or session replacement.
    pub fn same_instance(&self, other: &Self) -> bool {
        let a = &self.receiver;
        let b = &other.receiver;
        self.validate().is_ok()
            && other.validate().is_ok()
            && self.binding == other.binding
            && self.semantic_target == other.semantic_target
            && self.session_epoch == other.session_epoch
            && self.surface == other.surface
            && a.registration_id == b.registration_id
            && a.profile == b.profile
            && a.process == b.process
            && a.provenance.identity_boundary == b.provenance.identity_boundary
            && a.provenance.clock_domain == b.provenance.clock_domain
            && a.window_id == b.window_id
            && a.window_object_ref == b.window_object_ref
            && a.window_frame == b.window_frame
            && a.actual_object_ref == b.actual_object_ref
            && a.receiver_frame == b.receiver_frame
            && a.ancestry == b.ancestry
    }
}
