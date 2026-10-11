//! Read-only binding for a Host-registered owned document.
//!
//! This evidence lives beside the generic environment state, but it never
//! participates in semantic bindings, active-layer selection or input grants.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::ids::{BindingRef, RequestId, SemanticTargetRef};
use interaction_contracts::desktop::owned_document::OwnedDocumentReceiverEvidence;
use interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof;
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::source::{SourceIdentityBoundary, StructureSnapshot};
use sha2::{Digest, Sha256};

const MAX_TRACKED_NAMESPACES: usize = 32;
const MAX_RECENT_PROOF_IDS: usize = 256;

/// A current, read-only view of one registered owned-document receiver.
///
/// The contained proof can support private read-only inspection. This type is
/// not accepted by the generic binding reducer or any execution pathway.
#[derive(Clone, PartialEq)]
pub struct OwnedDocumentBinding {
    evidence: OwnedDocumentReceiverEvidence,
    session_epoch: u64,
    surface: SurfaceRef,
    snapshot_id: RequestId,
    source_sequence: u64,
    snapshot_observed_ms: u64,
    identity_boundary: SourceIdentityBoundary,
}

impl fmt::Debug for OwnedDocumentBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedDocumentBinding([private read-only evidence])")
    }
}

impl OwnedDocumentBinding {
    fn from_snapshot(snapshot: &StructureSnapshot) -> Option<Self> {
        let evidence = snapshot.owned_document_evidence.as_deref()?.clone();
        let identity_boundary = evidence.provenance.identity_boundary.clone();
        Some(Self {
            evidence,
            session_epoch: snapshot.session_epoch,
            surface: snapshot.surface.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            source_sequence: snapshot.source_sequence,
            snapshot_observed_ms: snapshot.observed_ms,
            identity_boundary,
        })
    }

    /// The typed Source proof, kept process-local and separate from generic
    /// UI bindings. It carries no document text or filesystem path.
    pub fn evidence(&self) -> &OwnedDocumentReceiverEvidence {
        &self.evidence
    }

    pub fn snapshot_id(&self) -> &RequestId {
        &self.snapshot_id
    }

    pub fn source_sequence(&self) -> u64 {
        self.source_sequence
    }

    pub fn snapshot_observed_ms(&self) -> u64 {
        self.snapshot_observed_ms
    }

    pub fn receiver_object_ref(&self) -> &interaction_contracts::desktop::ids::ObjectRef {
        &self.evidence.actual_object_ref
    }

    pub fn window_id(&self) -> &str {
        &self.evidence.window_id
    }

    /// Convert this already-current private receiver into the dedicated
    /// device proof. This does not create a generic UI binding or grant input.
    pub fn binding_proof(
        &self,
        binding: BindingRef,
        semantic_target: SemanticTargetRef,
        environment_revision: u64,
    ) -> Result<OwnedDocumentBindingProof, &'static str> {
        let proof = OwnedDocumentBindingProof {
            binding,
            semantic_target,
            environment_revision,
            session_epoch: self.session_epoch,
            surface: self.surface.clone(),
            snapshot_observed_ms: self.snapshot_observed_ms,
            receiver: Box::new(self.evidence.clone()),
        };
        proof
            .validate()
            .map_err(|_| "invalid owned document receiver proof")?;
        Ok(proof)
    }

    fn current_at(&self, now_ms: u64, max_age_ms: u64) -> bool {
        let proof = &self.evidence;
        now_ms >= self.snapshot_observed_ms
            && now_ms.saturating_sub(self.snapshot_observed_ms) < max_age_ms
            && now_ms >= proof.provenance.observed_ms
            && now_ms.saturating_sub(proof.provenance.observed_ms) < max_age_ms
            && now_ms < proof.provenance.valid_until_ms
            && proof.validate_at(now_ms).is_ok()
    }

    fn matches_snapshot(&self, snapshot: &StructureSnapshot) -> bool {
        self.session_epoch == snapshot.session_epoch
            && self.surface == snapshot.surface
            && self.snapshot_id == snapshot.snapshot_id
            && self.source_sequence == snapshot.source_sequence
            && self.snapshot_observed_ms == snapshot.observed_ms
            && self.evidence.provenance.snapshot_id == snapshot.snapshot_id
            && self.evidence.provenance.source_sequence == snapshot.source_sequence
            && self.evidence.provenance.clock_domain == snapshot.clock_domain
            && self.evidence.provenance.identity_boundary == self.identity_boundary
    }
}

/// Stable candidate reference for one semantic target in one frozen root.
/// The fallback keeps the validated id bounded when either input is long.
pub fn binding_ref(root: &str, semantic_target: &str) -> BindingRef {
    let candidate = format!("owned-text/{root}/{semantic_target}");
    if candidate.len() <= 256 {
        return BindingRef::new(candidate).expect("validated ids produce a valid binding ref");
    }
    let digest = Sha256::digest(candidate.as_bytes());
    BindingRef::new(format!("owned-text/{digest:x}")).expect("a digest is a valid binding ref")
}

/// Owns one private registration and the current source-validated proof.
///
/// Sequence and identity state are separate from `BindingReducer` so that a
/// generic incomplete read cannot silently grant execution authority.
#[derive(Debug, Default)]
pub struct OwnedDocumentBindingTracker {
    registration: Option<Box<OwnedDocumentRegistration>>,
    current: Option<OwnedDocumentBinding>,
    session_epoch: Option<u64>,
    surface: Option<SurfaceRef>,
    clock_domain: Option<String>,
    active_namespace: Option<String>,
    generation_high_water: BTreeMap<String, u64>,
    retired_namespaces: BTreeSet<String>,
    recently_seen_snapshot_ids: BTreeMap<RequestId, u64>,
    rebind_after_sequence: Option<u64>,
    identity_tracking_exhausted: bool,
}

impl OwnedDocumentBindingTracker {
    pub fn new(registration: Option<Box<OwnedDocumentRegistration>>) -> Self {
        let mut tracker = Self::default();
        tracker.install_registration(registration);
        tracker
    }

    /// Invalid startup registration is treated as unavailable. The caller is
    /// responsible for wall-clock startup freshness before installing it.
    pub fn install_registration(&mut self, registration: Option<Box<OwnedDocumentRegistration>>) {
        let next = registration.filter(|value| value.validate_identity().is_ok());
        let previous_id = self
            .registration
            .as_deref()
            .map(|value| value.registration_id.clone());
        let next_id = next.as_deref().map(|value| value.registration_id.clone());
        if previous_id != next_id {
            self.recently_seen_snapshot_ids.clear();
        }
        self.clear_identity_state();
        self.registration = next;
    }

    /// Admit a proof only after its containing StructureSnapshot was admitted.
    pub fn observe_admitted(&mut self, snapshot: &StructureSnapshot, now_ms: u64, max_age_ms: u64) {
        self.current = None;
        self.recently_seen_snapshot_ids
            .retain(|_, expires_ms| now_ms < *expires_ms);

        let Some(registration) = self.registration.as_deref() else {
            return;
        };

        let Some(evidence) = snapshot.owned_document_evidence.as_deref() else {
            return;
        };
        let proof = &evidence.provenance;
        if registration.validate_identity().is_err()
            || evidence.registration_id != registration.registration_id
            || evidence.process != registration.process_identity()
            || snapshot.app.pid != registration.pid
            || snapshot.app.bundle_id != registration.bundle_id
            || evidence.validate_at(now_ms).is_err()
            || proof.snapshot_id != snapshot.snapshot_id
            || proof.source_sequence != snapshot.source_sequence
            || proof.clock_domain != snapshot.clock_domain
            || proof.observed_ms > snapshot.observed_ms
            || snapshot.observed_ms >= proof.valid_until_ms
            || snapshot.observed_ms > now_ms
            || now_ms.saturating_sub(snapshot.observed_ms) >= max_age_ms
            || now_ms.saturating_sub(proof.observed_ms) >= max_age_ms
            || self
                .recently_seen_snapshot_ids
                .contains_key(&snapshot.snapshot_id)
            || self.recently_seen_snapshot_ids.len() >= MAX_RECENT_PROOF_IDS
        {
            return;
        }

        let seen_until = snapshot
            .observed_ms
            .saturating_add(max_age_ms)
            .min(proof.observed_ms.saturating_add(max_age_ms));
        self.recently_seen_snapshot_ids
            .insert(snapshot.snapshot_id.clone(), seen_until);

        if !self.note_snapshot_context(snapshot) || self.identity_tracking_exhausted {
            return;
        }
        if self.observe_identity_boundary(&proof.identity_boundary, snapshot.source_sequence) {
            return;
        }
        if self
            .rebind_after_sequence
            .is_some_and(|floor| snapshot.source_sequence <= floor)
        {
            return;
        }
        self.rebind_after_sequence = None;

        self.current = OwnedDocumentBinding::from_snapshot(snapshot);
    }

    /// Clear current evidence after a rejected record, source loss or stop.
    /// Identity high-water state remains so old namespace proofs cannot return.
    pub fn clear_current(&mut self) {
        self.current = None;
    }

    /// Reset all observed identity state when the Source/device session changes.
    pub fn reset_identity(&mut self) {
        self.clear_identity_state();
    }

    /// Drop evidence that is no longer current according to engine time.
    pub fn expire(&mut self, now_ms: u64, max_age_ms: u64) {
        if self
            .current
            .as_ref()
            .is_some_and(|binding| !binding.current_at(now_ms, max_age_ms))
        {
            self.current = None;
        }
    }

    pub fn get(
        &self,
        snapshot: Option<&StructureSnapshot>,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Option<&OwnedDocumentBinding> {
        let binding = self.current.as_ref()?;
        let snapshot = snapshot?;
        let registration = self.registration.as_deref()?;
        let evidence = &binding.evidence;
        (registration.validate_identity().is_ok()
            && evidence.registration_id == registration.registration_id
            && evidence.process == registration.process_identity()
            && snapshot.app.bundle_id == registration.bundle_id
            && snapshot.app.pid == registration.pid
            && binding.current_at(now_ms, max_age_ms)
            && binding.matches_snapshot(snapshot))
        .then_some(binding)
    }

    fn note_snapshot_context(&mut self, snapshot: &StructureSnapshot) -> bool {
        if let (Some(session), Some(surface), Some(clock)) = (
            self.session_epoch,
            self.surface.as_ref(),
            self.clock_domain.as_ref(),
        ) {
            if session != snapshot.session_epoch
                || surface != &snapshot.surface
                || clock != &snapshot.clock_domain
            {
                self.clear_namespace_state();
                self.session_epoch = Some(snapshot.session_epoch);
                self.surface = Some(snapshot.surface.clone());
                self.clock_domain = Some(snapshot.clock_domain.clone());
                self.rebind_after_sequence = Some(snapshot.source_sequence);
                return false;
            }
        } else {
            self.session_epoch = Some(snapshot.session_epoch);
            self.surface = Some(snapshot.surface.clone());
            self.clock_domain = Some(snapshot.clock_domain.clone());
        }
        true
    }

    /// Namespace history and generation high-water are private to this tracker.
    /// A namespace switch retires the previous namespace until trusted reset.
    fn observe_identity_boundary(
        &mut self,
        incoming: &SourceIdentityBoundary,
        source_sequence: u64,
    ) -> bool {
        if self.retired_namespaces.contains(&incoming.namespace) {
            return true;
        }

        if let Some(active) = self.active_namespace.as_deref()
            && active != incoming.namespace
        {
            if self.generation_high_water.len() >= MAX_TRACKED_NAMESPACES
                && !self.generation_high_water.contains_key(&incoming.namespace)
                || self.retired_namespaces.len() >= MAX_TRACKED_NAMESPACES
            {
                self.identity_tracking_exhausted = true;
                return true;
            }
            self.retired_namespaces.insert(active.to_owned());
            self.active_namespace = Some(incoming.namespace.clone());
            self.generation_high_water
                .entry(incoming.namespace.clone())
                .and_modify(|high| *high = (*high).max(incoming.generation))
                .or_insert(incoming.generation);
            self.rebind_after_sequence = Some(source_sequence);
            return true;
        }

        if let Some(high_water) = self.generation_high_water.get_mut(&incoming.namespace) {
            if incoming.generation < *high_water {
                self.rebind_after_sequence = Some(source_sequence);
                return true;
            }
            *high_water = incoming.generation;
            self.active_namespace = Some(incoming.namespace.clone());
            return false;
        }

        if self.generation_high_water.len() >= MAX_TRACKED_NAMESPACES {
            self.identity_tracking_exhausted = true;
            return true;
        }
        self.active_namespace = Some(incoming.namespace.clone());
        self.generation_high_water
            .insert(incoming.namespace.clone(), incoming.generation);
        false
    }

    fn clear_namespace_state(&mut self) {
        self.active_namespace = None;
        self.generation_high_water.clear();
        self.retired_namespaces.clear();
        self.identity_tracking_exhausted = false;
    }

    fn clear_identity_state(&mut self) {
        self.current = None;
        self.session_epoch = None;
        self.surface = None;
        self.clock_domain = None;
        self.clear_namespace_state();
        self.rebind_after_sequence = None;
    }
}
