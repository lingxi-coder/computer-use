//! Execution, as the host sees it (design 5.3 and 14).
//!
//! The device decides what may be submitted; the host only keeps what it needs
//! to ask correctly: who the device is, which dependency revisions it holds,
//! which stop the host has processed, and which commands still await a reply.
//! It builds the messages the device expects and never pretends to know more
//! than the device told it.

use std::collections::BTreeMap;

use interaction_contracts::desktop::binding::{
    BindingProof, BindingProofScope, DeviceContextWitness, FocusedControlScope, FocusedKeyProfile,
    LayoutIndependentSpecialKey, MenuBindingScope, derive_background_ax_binding_evidence,
    derive_binding_evidence, derive_focused_binding_evidence,
    derive_row_selection_binding_evidence, derive_window_root_binding_evidence,
    instance_fingerprint,
};
use interaction_contracts::desktop::browser_execution::{
    BrowserActionIntent, BrowserExecutionObservation,
};
use interaction_contracts::desktop::device::{
    ActionIntent, BackendKind, CapabilityManifest, CapabilityState, CommandAck, CoordinateSpace,
    DependencyUpdate, DependencyVersions, DeviceEvent, DeviceIdentity, ExecutionIntent,
    ExecutionObservation, GrantOutcome, GrantPermit, LeaseRenew, Observation, ObservationPurpose,
    PermitRef, QueryReceipt, Receipt, ResumeAck, ResumeCommand, RevokeExecution, RootGrant,
    ScheduledPrimitive, SourceFreshness, StopAck, StopCommand, SubmitIntent,
};
use interaction_contracts::desktop::domain::{DomainAction, ExecutionDomain};
use interaction_contracts::desktop::ids::{AttemptId, RootTaskId};
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
use interaction_contracts::desktop::source::StructureSnapshot;

use crate::environment::{
    binding::{Binding, RowBinding},
    read::focused_window_id,
    row::row_scope_coverage,
    state_core::EnvSnapshot,
};
use crate::reaction::owned_text::OwnedDocumentActionPlan;
use crate::reaction::skills::row::RowSelectionActionPlan;
use crate::reaction::skills::{ActionPlan, BrowserActionPlan, ExecutionPath};

const LATE_GRANT_CAPACITY: usize = 1024;

/// What the host asks of the device.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceCommand {
    RegisterRoot(RootGrant),
    Observe(ExecutionObservation),
    GrantPermit(GrantPermit),
    Submit(SubmitIntent),
    Revoke(RevokeExecution),
    QueryReceipt(QueryReceipt),
    UpdateDependency(DependencyUpdate),
    Stop(StopCommand),
    Resume(ResumeCommand),
    RenewLease(LeaseRenew),
}

/// The answer to one command.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceReply {
    Ack(CommandAck),
    Grant(GrantOutcome),
    /// The receipt of a submitted intent, or of a query that may find none.
    Receipt(Option<Box<Receipt>>),
    Stopped(StopAck),
    Resumed(ResumeAck),
    Dependencies(DependencyVersions),
}

/// What the device says on its own.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceNotice {
    Manifest(Box<CapabilityManifest>),
    Receipt(Box<Receipt>),
    Event(DeviceEvent),
}

/// What a sent command is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    Register,
    Observe {
        attempt: AttemptId,
        observation_id: String,
    },
    Grant {
        attempt: AttemptId,
        observation_id: String,
    },
    Submit {
        intent_id: String,
    },
    Revoke,
    Query {
        intent_id: String,
    },
    Dependency,
    Stop,
    Resume,
    Lease,
}

/// How far one bounded input has got on its way to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The evidence was sent; the device has not accepted it yet.
    Observing,
    /// The permit was asked for.
    Granting,
    /// The intent was sent.
    Submitting,
    /// The device accepted the intent; its end is awaited.
    Running,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DispatchPlan {
    Generic(Box<ActionPlan>),
    RowSelection(Box<RowSelectionActionPlan>),
    OwnedDocument(OwnedDocumentActionPlan),
    Browser(Box<BrowserActionPlan>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dispatch {
    pub attempt: AttemptId,
    pub plan: DispatchPlan,
    pub observation: ExecutionObservation,
    /// The exact outstanding Observe/Grant command for this dispatch.
    pub reply_corr: Option<u64>,
    pub permit: Option<PermitRef>,
    pub intent_id: Option<String>,
    pub stage: Stage,
    pub started_ms: u64,
}

pub struct IntentSpec<'a> {
    pub attempt: &'a AttemptId,
    pub root: &'a RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub permit: &'a PermitRef,
    pub observation: &'a Observation,
    pub plan: &'a ActionPlan,
    pub sequence: u32,
    pub now_ms: u64,
    pub start_slack_ms: u64,
    pub duration_ms: u64,
}

pub struct RowSelectionIntentSpec<'a> {
    pub attempt: &'a AttemptId,
    pub root: &'a RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub permit: &'a PermitRef,
    pub observation: &'a Observation,
    pub plan: &'a RowSelectionActionPlan,
    pub sequence: u32,
    pub now_ms: u64,
    pub start_slack_ms: u64,
    pub duration_ms: u64,
}

/// Inputs to the device's dedicated owned-document SetValue builder.
pub struct OwnedDocumentIntentSpec<'a> {
    attempt: &'a AttemptId,
    root: &'a RootTaskId,
    task_revision: u64,
    root_epoch: u64,
    permit: &'a PermitRef,
    observation: &'a Observation,
    plan: &'a OwnedDocumentActionPlan,
    sequence: u32,
    now_ms: u64,
    max_age_ms: u64,
    start_slack_ms: u64,
    duration_ms: u64,
    registration: &'a OwnedDocumentRegistration,
}

pub struct BrowserIntentSpec<'a> {
    pub attempt: &'a AttemptId,
    pub root: &'a RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub permit: &'a PermitRef,
    pub observation: &'a BrowserExecutionObservation,
    pub plan: &'a BrowserActionPlan,
    pub sequence: u32,
    pub now_ms: u64,
    pub start_slack_ms: u64,
    pub duration_ms: u64,
}

impl<'a> OwnedDocumentIntentSpec<'a> {
    /// Build a typed request context; the proof and value are rechecked when
    /// DeviceView constructs the intent.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        attempt: &'a AttemptId,
        root: &'a RootTaskId,
        task_revision: u64,
        root_epoch: u64,
        permit: &'a PermitRef,
        observation: &'a Observation,
        plan: &'a OwnedDocumentActionPlan,
        sequence: u32,
        now_ms: u64,
        max_age_ms: u64,
        start_slack_ms: u64,
        duration_ms: u64,
        registration: &'a OwnedDocumentRegistration,
    ) -> Self {
        Self {
            attempt,
            root,
            task_revision,
            root_epoch,
            permit,
            observation,
            plan,
            sequence,
            now_ms,
            max_age_ms,
            start_slack_ms,
            duration_ms,
            registration,
        }
    }
}

#[derive(Debug, Default)]
pub struct DeviceView {
    manifest: Option<CapabilityManifest>,
    dependencies: BTreeMap<String, u64>,
    acked_stop_seq: u64,
    seen_stop_seq: u64,
    next_corr: u64,
    outstanding: BTreeMap<u64, (Expect, u64)>,
    /// Timed-out grants can still arrive; keep their identity so the reply is
    /// routed to revocation instead of being mistaken for a newer dispatch.
    late_grants: BTreeMap<u64, Expect>,
    /// The device has accepted the host's control lease. Without one it refuses
    /// everything, so nothing is asked of it until it has.
    lease_established: bool,
}

impl DeviceView {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- Identity and capability -------------------------------------------

    /// The device introduced itself, or its capabilities changed. A new
    /// session invalidates everything that was in flight.
    pub fn install_manifest(&mut self, manifest: CapabilityManifest) {
        let new_session = self
            .manifest
            .as_ref()
            .is_none_or(|old| old.identity.session_epoch != manifest.identity.session_epoch);
        if new_session {
            self.outstanding.clear();
            self.late_grants.clear();
            // Stop numbers belong to one device session. A number from the old
            // one must never count as having handled a stop in the new one.
            self.acked_stop_seq = 0;
            self.seen_stop_seq = 0;
            self.lease_established = false;
        }
        self.dependencies = manifest.identity.dependencies.clone();
        self.manifest = Some(manifest);
    }

    /// The device is gone. Nothing it said still holds, and nothing is sent to
    /// it until a new one introduces itself.
    pub fn forget(&mut self) {
        self.manifest = None;
        self.dependencies.clear();
        self.outstanding.clear();
        self.late_grants.clear();
        self.acked_stop_seq = 0;
        self.seen_stop_seq = 0;
        self.lease_established = false;
    }

    pub fn lease_established(&self) -> bool {
        self.lease_established
    }

    pub fn note_lease(&mut self, established: bool) {
        self.lease_established = established;
    }

    pub fn manifest(&self) -> Option<&CapabilityManifest> {
        self.manifest.as_ref()
    }

    pub fn identity(&self) -> Option<&DeviceIdentity> {
        self.manifest.as_ref().map(|manifest| &manifest.identity)
    }

    pub fn session_epoch(&self) -> Option<u64> {
        self.identity().map(|identity| identity.session_epoch)
    }

    /// The device can take input right now.
    pub fn input_available(&self) -> bool {
        self.manifest
            .as_ref()
            .is_some_and(|manifest| manifest.input == CapabilityState::Available)
    }

    /// Whether the trusted selected execution domain can run at least one of
    /// its declared action families. BackgroundAX and the owned headless
    /// browser are independent of OS-global keyboard/pointer input capability.
    pub fn execution_domain_available(&self) -> bool {
        let Some(manifest) = self.manifest.as_ref() else {
            return false;
        };
        if manifest.validate().is_err() {
            return false;
        }
        manifest
            .domains
            .iter()
            .find(|capability| capability.domain == manifest.execution_domain)
            .is_some_and(|capability| {
                let domain_action_available = match manifest.execution_domain {
                    ExecutionDomain::ForegroundNative => capability.actions.iter().any(|action| {
                        matches!(
                            action,
                            DomainAction::NativePointer
                                | DomainAction::NativeKeyboard
                                | DomainAction::NativeScroll
                                | DomainAction::WindowActivation
                                | DomainAction::ForegroundAx
                        )
                    }),
                    ExecutionDomain::BackgroundAx => capability
                        .actions
                        .contains(&DomainAction::BackgroundAxPress),
                    ExecutionDomain::HeadlessBrowser => capability
                        .actions
                        .contains(&DomainAction::HeadlessBrowserAction),
                    ExecutionDomain::AppIntegration => false,
                    ExecutionDomain::Simulated => {
                        capability.actions.contains(&DomainAction::SimulatedAction)
                    }
                };
                capability.state == CapabilityState::Available
                    && domain_action_available
                    && (matches!(
                        manifest.execution_domain,
                        ExecutionDomain::BackgroundAx | ExecutionDomain::HeadlessBrowser
                    ) || manifest.input == CapabilityState::Available)
            })
    }

    pub fn dependencies(&self) -> &BTreeMap<String, u64> {
        &self.dependencies
    }

    pub fn note_dependency(&mut self, name: &str, revision: u64) {
        self.dependencies.insert(name.to_string(), revision);
    }

    pub fn replace_dependencies(&mut self, versions: BTreeMap<String, u64>) {
        self.dependencies = versions;
    }

    // ---- Stops ---------------------------------------------------------------

    /// The highest stop the host has *processed*. The device refuses requests
    /// that carry less than its floor.
    pub fn acked_stop_seq(&self) -> u64 {
        self.acked_stop_seq
    }

    /// The device told the host about a stop. It is not acknowledged until the
    /// task runtime has acted on it.
    pub fn stop_seen(&mut self, seq: u64) {
        self.seen_stop_seq = self.seen_stop_seq.max(seq);
    }

    pub fn stop_processed(&mut self, seq: u64) {
        self.seen_stop_seq = self.seen_stop_seq.max(seq);
        self.acked_stop_seq = self.acked_stop_seq.max(seq);
    }

    /// A stop is known that the task runtime has not handled yet.
    pub fn behind_on_stops(&self) -> bool {
        self.seen_stop_seq > self.acked_stop_seq
    }

    // ---- Commands awaiting a reply ---------------------------------------------

    /// Number a command and remember what its reply should be.
    pub fn send(&mut self, now_ms: u64, expect: Expect) -> u64 {
        self.next_corr += 1;
        self.outstanding.insert(self.next_corr, (expect, now_ms));
        self.next_corr
    }

    pub fn reply(&mut self, corr: u64) -> Option<Expect> {
        self.outstanding
            .remove(&corr)
            .map(|(expect, _)| expect)
            .or_else(|| self.late_grants.remove(&corr))
    }

    /// Inspect the expected reply for a correlation without consuming it.
    /// Callers can use this to validate the reply variant before taking it.
    pub fn expected_reply(&self, corr: u64) -> Option<&Expect> {
        self.outstanding
            .get(&corr)
            .map(|(expect, _)| expect)
            .or_else(|| self.late_grants.get(&corr))
    }

    /// Replies to a finished attempt must never advance a later dispatch.
    /// Keep grant replies so a permit issued just before cancellation can be
    /// explicitly revoked when its late result arrives. Query and dependency
    /// replies have lifetimes outside an individual attempt and remain routable.
    pub fn forget_attempt_waits(&mut self) {
        self.outstanding.retain(|_, (expect, _)| {
            !matches!(expect, Expect::Observe { .. } | Expect::Submit { .. })
        });
        self.late_grants
            .retain(|_, expect| matches!(expect, Expect::Grant { .. }));
    }

    pub fn is_waiting(&self, matching: impl Fn(&Expect) -> bool) -> bool {
        self.outstanding
            .values()
            .any(|(expect, _)| matching(expect))
    }

    /// Commands that have waited longer than `limit_ms`. Timed-out grants
    /// retain a bounded reply tombstone so late permits can be revoked. When
    /// the cap is reached, the smallest correlation (the oldest command in
    /// this session) is evicted first; callers must revoke Granted outcomes
    /// whose correlation no longer has an expectation.
    pub fn timed_out(&mut self, now_ms: u64, limit_ms: u64) -> Vec<(u64, Expect)> {
        let late: Vec<u64> = self
            .outstanding
            .iter()
            .filter(|(_, (_, sent))| now_ms.saturating_sub(*sent) > limit_ms)
            .map(|(corr, _)| *corr)
            .collect();
        late.into_iter()
            .filter_map(|corr| {
                self.outstanding.remove(&corr).map(|(expect, _)| {
                    if matches!(expect, Expect::Grant { .. }) {
                        self.late_grants.insert(corr, expect.clone());
                        while self.late_grants.len() > LATE_GRANT_CAPACITY {
                            if let Some(oldest_corr) =
                                self.late_grants.first_key_value().map(|(key, _)| *key)
                            {
                                self.late_grants.remove(&oldest_corr);
                            }
                        }
                    }
                    (corr, expect)
                })
            })
            .collect()
    }

    pub fn forget_outstanding(&mut self) {
        self.outstanding.clear();
        self.late_grants.clear();
    }

    // ---- Builders ------------------------------------------------------------------

    /// The device's evidence for one target, built from a snapshot. It must
    /// come from the same device session, surface and clock as the device.
    pub fn observation(
        &self,
        snapshot: &StructureSnapshot,
        target_ref: &str,
        freshness: SourceFreshness,
        ttl_ms: u64,
        limit_ms: u64,
    ) -> Result<Observation, &'static str> {
        self.observation_with_deadline(snapshot, target_ref, freshness, ttl_ms, limit_ms, None)
    }

    pub fn observation_with_deadline(
        &self,
        snapshot: &StructureSnapshot,
        target_ref: &str,
        freshness: SourceFreshness,
        ttl_ms: u64,
        limit_ms: u64,
        source_deadline: Option<u64>,
    ) -> Result<Observation, &'static str> {
        let identity = self
            .identity()
            .ok_or("the device has not introduced itself")?;
        let execution_domain = self
            .manifest
            .as_ref()
            .map(|manifest| manifest.execution_domain)
            .ok_or("the device has no trusted execution-domain profile")?;
        if snapshot.session_epoch != identity.session_epoch
            || snapshot.surface != identity.surface
            || snapshot.clock_domain != identity.clock_domain
        {
            return Err("the snapshot is from another device session, surface or clock");
        }
        let revisions: Vec<String> = self.dependencies.values().map(u64::to_string).collect();
        Ok(Observation {
            observation_id: format!(
                "obs-{}-{}-d{}",
                snapshot.snapshot_id,
                target_ref,
                revisions.join(".")
            ),
            session_epoch: identity.session_epoch,
            surface: identity.surface.clone(),
            target_ref: target_ref.to_string(),
            clock_domain: identity.clock_domain.clone(),
            source_sequence: snapshot.source_sequence,
            observed_ms: snapshot.observed_ms,
            valid_until_ms: snapshot
                .observed_ms
                .saturating_add(ttl_ms)
                .min(limit_ms)
                .min(source_deadline.unwrap_or(u64::MAX)),
            freshness,
            execution_domain,
            purpose: Default::default(),
            owned_document_proof: None,
            binding_proof: None,
            dependencies: self
                .dependencies
                .iter()
                .filter(|(name, _)| name.as_str() != "window")
                .map(|(name, value)| (name.clone(), *value))
                .collect(),
        })
    }

    /// Build dedicated evidence for one current private owned-document action.
    /// This intentionally does not synthesize a generic binding proof.
    pub fn owned_document_observation(
        &self,
        plan: &OwnedDocumentActionPlan,
        registration: &OwnedDocumentRegistration,
        now_ms: u64,
        max_age_ms: u64,
        ttl_ms: u64,
        limit_ms: u64,
    ) -> Result<Observation, &'static str> {
        let proof = plan.proof();
        proof
            .validate_registration(registration)
            .map_err(|_| "owned document proof does not match its private registration")?;
        proof
            .validate_at(now_ms, max_age_ms)
            .map_err(|_| "owned document proof is stale or outside Source time")?;
        let identity = self
            .identity()
            .ok_or("the device has not introduced itself")?;
        let execution_domain = self
            .manifest
            .as_ref()
            .map(|manifest| manifest.execution_domain)
            .ok_or("the device has no trusted execution-domain profile")?;
        if proof.session_epoch != identity.session_epoch
            || proof.surface != identity.surface
            || proof.receiver.provenance.clock_domain != identity.clock_domain
        {
            return Err("owned document proof belongs to another device session or surface");
        }

        let dependencies = self.current_owned_document_dependencies()?;
        let revisions: Vec<_> = dependencies.values().map(u64::to_string).collect();
        let valid_until_ms = proof
            .snapshot_observed_ms
            .saturating_add(ttl_ms)
            .min(proof.receiver.provenance.valid_until_ms)
            .min(limit_ms);
        if valid_until_ms <= now_ms {
            return Err("owned document evidence expires before the action can start");
        }
        let observation = Observation {
            observation_id: format!(
                "obs-owned-{}-{}-d{}",
                proof.binding,
                proof.receiver.provenance.source_sequence,
                revisions.join(".")
            ),
            session_epoch: identity.session_epoch,
            surface: identity.surface.clone(),
            target_ref: proof.binding.as_str().to_string(),
            clock_domain: identity.clock_domain.clone(),
            source_sequence: proof.receiver.provenance.source_sequence,
            observed_ms: proof.snapshot_observed_ms,
            valid_until_ms,
            freshness: SourceFreshness::Verified,
            execution_domain,
            purpose: plan.observation_purpose(),
            binding_proof: None,
            owned_document_proof: Some(Box::new(proof.clone())),
            dependencies,
        };
        observation
            .validate()
            .map_err(|_| "invalid owned document execution evidence")?;
        Ok(observation)
    }

    /// Build an intent for exactly one atomic owned-document AX SetValue.
    pub fn owned_document_intent(
        &self,
        spec: &OwnedDocumentIntentSpec<'_>,
    ) -> Result<SubmitIntent, &'static str> {
        let identity = self
            .identity()
            .ok_or("the device has not introduced itself")?;
        spec.plan
            .proof()
            .validate_registration(spec.registration)
            .map_err(|_| "owned document plan does not match its private registration")?;
        spec.plan
            .proof()
            .validate_at(spec.now_ms, spec.max_age_ms)
            .map_err(|_| "owned document plan proof is stale or outside Source time")?;
        spec.observation
            .validate()
            .map_err(|_| "invalid owned document observation")?;
        if spec.observation.purpose != spec.plan.observation_purpose()
            || spec.observation.freshness != SourceFreshness::Verified
            || spec.observation.session_epoch != identity.session_epoch
            || spec.observation.surface != identity.surface
            || spec.observation.clock_domain != identity.clock_domain
            || spec.observation.observed_ms > spec.now_ms
            || spec.observation.valid_until_ms <= spec.now_ms
            || spec.observation.owned_document_proof.as_deref() != Some(spec.plan.proof())
            || spec.observation.dependencies != self.current_owned_document_dependencies()?
        {
            return Err("owned document intent differs from the current observation");
        }
        let latest_start_ms = spec
            .now_ms
            .checked_add(spec.start_slack_ms)
            .ok_or("owned document intent start time overflow")?;
        let end_ms = latest_start_ms
            .checked_add(spec.duration_ms)
            .ok_or("owned document intent deadline overflow")?;
        if spec.duration_ms == 0
            || spec.duration_ms > 1_500
            || end_ms > spec.observation.valid_until_ms
            || end_ms > spec.plan.proof().receiver.provenance.valid_until_ms
        {
            return Err("owned document intent exceeds its evidence or duration bound");
        }

        let request = SubmitIntent {
            acked_stop_seq: self.acked_stop_seq,
            intent: ActionIntent {
                intent_id: format!("intent-{}-{}", spec.attempt, spec.sequence),
                attempt_id: Some(spec.attempt.as_str().to_string()),
                task_id: spec.root.as_str().to_string(),
                task_revision: spec.task_revision,
                root_epoch: spec.root_epoch,
                session_epoch: identity.session_epoch,
                execution_domain: spec.observation.execution_domain,
                permit: spec.permit.clone(),
                surface: identity.surface.clone(),
                target_ref: spec.plan.proof().binding.as_str().to_string(),
                binding_fingerprint: None,
                binding_proof: None,
                owned_document_proof: Some(Box::new(spec.plan.proof().clone())),
                coordinate_space: CoordinateSpace::DisplayPoints,
                dependencies: spec.observation.dependencies.clone(),
                evidence_id: spec.observation.observation_id.clone(),
                latest_start_ms,
                duration_ms: spec.duration_ms,
                steps: vec![ScheduledPrimitive {
                    offset_ms: 0,
                    primitive: spec.plan.primitive(),
                }],
            }
            .into(),
        };
        request
            .intent
            .validate()
            .map_err(|_| "invalid owned document value setter intent")?;
        Ok(request)
    }

    fn current_owned_document_dependencies(&self) -> Result<BTreeMap<String, u64>, &'static str> {
        ["authorization", "focus", "window", "owned_scope"]
            .into_iter()
            .map(|name| {
                self.dependencies
                    .get(name)
                    .copied()
                    .filter(|revision| *revision > 0)
                    .map(|revision| (name.to_string(), revision))
                    .ok_or("owned document execution dependency is unavailable")
            })
            .collect()
    }

    /// Refresh provenance from the actual current snapshot, while preserving
    /// the instance and descriptor admitted by the binding reducer.
    pub fn bound_observation(
        &self,
        env: &EnvSnapshot,
        binding: &Binding,
        freshness: SourceFreshness,
        ttl_ms: u64,
        limit_ms: u64,
        source_deadline: Option<u64>,
    ) -> Result<Observation, &'static str> {
        self.build_bound_observation(
            env,
            binding,
            freshness,
            ttl_ms,
            limit_ms,
            source_deadline,
            None,
            None,
        )
    }

    /// Only an activated menu attempt may supply this Runtime-owned scope.
    /// All descriptor and provenance fields still come from current Source.
    #[allow(clippy::too_many_arguments)]
    pub fn bound_scoped_observation(
        &self,
        env: &EnvSnapshot,
        binding: &Binding,
        freshness: SourceFreshness,
        ttl_ms: u64,
        limit_ms: u64,
        source_deadline: Option<u64>,
        menu_scope: MenuBindingScope,
    ) -> Result<Observation, &'static str> {
        self.build_bound_observation(
            env,
            binding,
            freshness,
            ttl_ms,
            limit_ms,
            source_deadline,
            Some(menu_scope),
            None,
        )
    }

    /// An activated Runtime operation supplies the resolved key, never focus
    /// facts. The caller must first establish normal StateCore usability and
    /// pass its normal frame deadline; menu-only AX evidence cannot substitute.
    #[allow(clippy::too_many_arguments)]
    pub fn bound_focused_observation(
        &self,
        env: &EnvSnapshot,
        binding: &Binding,
        freshness: SourceFreshness,
        ttl_ms: u64,
        limit_ms: u64,
        source_deadline: Option<u64>,
        key: LayoutIndependentSpecialKey,
    ) -> Result<Observation, &'static str> {
        self.build_bound_observation(
            env,
            binding,
            freshness,
            ttl_ms,
            limit_ms,
            source_deadline,
            None,
            Some(key),
        )
    }

    fn background_ax_observation(
        &self,
        env: &EnvSnapshot,
        binding: &Binding,
        freshness: SourceFreshness,
        ttl_ms: u64,
        limit_ms: u64,
        source_deadline: Option<u64>,
    ) -> Result<Observation, &'static str> {
        let snapshot = &env.snapshot;
        if binding.rule != "background_ax"
            || binding.identity_boundary != snapshot.identity_boundary
            || env.revision < binding.env_revision
            || snapshot.observer_coverage.state
                == interaction_contracts::desktop::source::SourceState::Unavailable
            || !snapshot.complete
            || snapshot.menu_context.is_some()
            || freshness != SourceFreshness::Verified
        {
            return Err("background AX needs a fresh complete bound target context");
        }
        let BindingProofScope::BackgroundAx {
            target_context_hash,
        } = &binding.continuity.scope
        else {
            return Err("background AX binding has the wrong proof scope");
        };
        let object = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.instance)
            .ok_or("the background AX target is missing from current Source")?;
        if !object.enabled
            || !object.in_view
            || object.secure
            || object.scroll.is_some()
            || instance_fingerprint(snapshot, object).digest() != binding.fingerprint_digest
        {
            return Err("the background AX target changed or is unavailable");
        }
        let evidence = derive_background_ax_binding_evidence(snapshot, object)
            .map_err(|_| "the current Source has no valid background AX witness")?;
        let DeviceContextWitness::BackgroundAxV1 { source, .. } = &evidence.device_context else {
            return Err("the current Source did not derive a background AX witness");
        };
        if !evidence.device_context.supports_background_ax_press()
            || source.target_context_hash != *target_context_hash
            || !binding.continuity.same_closure(&evidence.continuity)
            || binding.continuity.source_sequence > evidence.continuity.source_sequence
            || snapshot.source_sequence != source.window.verified_source_sequence
            || snapshot.observed_ms != source.window.verified_observed_ms
        {
            return Err("the background AX target context or continuity changed");
        }
        let device_dependencies = [
            "authorization",
            "background_scope",
            "binding",
            "geometry",
            "window",
        ];
        let dependencies = device_dependencies
            .into_iter()
            .map(|name| {
                self.dependencies
                    .get(name)
                    .copied()
                    .filter(|revision| *revision > 0)
                    .map(|revision| (name.to_string(), revision))
                    .ok_or("background AX execution dependency is unavailable")
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let proof = BindingProof {
            binding: binding.binding_ref.clone(),
            instance: object.instance_ref.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            environment_revision: env.revision,
            layer_revision: env.layer_revision,
            fingerprint: instance_fingerprint(snapshot, object),
            display_frame: object.frame,
            continuity: evidence.continuity,
            device_context: evidence.device_context,
            menu_scope: None,
            focus_scope: None,
        };
        proof
            .validate()
            .map_err(|_| "the current background AX proof is invalid")?;
        let mut observation = self.observation_with_deadline(
            snapshot,
            binding.binding_ref.as_str(),
            freshness,
            ttl_ms.min(2_000),
            limit_ms,
            source_deadline,
        )?;
        observation.execution_domain = ExecutionDomain::BackgroundAx;
        observation.purpose = ObservationPurpose::BackgroundAxAction;
        observation.binding_proof = Some(proof);
        observation.dependencies = dependencies;
        observation
            .validate()
            .map_err(|_| "invalid background AX action evidence")?;
        Ok(observation)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_bound_observation(
        &self,
        env: &EnvSnapshot,
        binding: &Binding,
        freshness: SourceFreshness,
        ttl_ms: u64,
        limit_ms: u64,
        source_deadline: Option<u64>,
        menu_scope: Option<MenuBindingScope>,
        focused_key: Option<LayoutIndependentSpecialKey>,
    ) -> Result<Observation, &'static str> {
        let snapshot = &env.snapshot;
        snapshot
            .validate()
            .map_err(|_| "invalid current source structure")?;
        if self
            .manifest
            .as_ref()
            .is_some_and(|manifest| manifest.execution_domain == ExecutionDomain::BackgroundAx)
        {
            if menu_scope.is_some() || focused_key.is_some() {
                return Err("BackgroundAX does not support menu or focus scopes");
            }
            return self.background_ax_observation(
                env,
                binding,
                freshness,
                ttl_ms,
                limit_ms,
                source_deadline,
            );
        }
        let focus_scope = if let Some(key) = focused_key {
            if menu_scope.is_some()
                || snapshot.menu_context.is_some()
                || matches!(binding.rule.as_str(), "menu_item" | "window_activation")
                || binding.env_revision != env.revision
                || binding.snapshot_id != snapshot.snapshot_id
                || binding.observed_ms != snapshot.observed_ms
                || binding.page != env.page
            {
                return Err("focused evidence requires a current normal target binding");
            }
            if ["focus", "layer", "binding", "authorization", "geometry"]
                .into_iter()
                .any(|name| {
                    self.dependencies
                        .get(name)
                        .is_none_or(|revision| *revision == 0)
                })
            {
                return Err("focused execution dependencies are unavailable");
            }
            if self
                .manifest
                .as_ref()
                .is_some_and(|manifest| manifest.backend == BackendKind::Macos)
                && source_deadline.is_none()
            {
                return Err("native focused evidence requires the normal frame deadline");
            }
            let context = snapshot
                .focus_context
                .as_ref()
                .ok_or("current Source has no actual focus context")?;
            if context.target_instance != binding.instance {
                return Err("the bound instance is not the actual current focused receiver");
            }
            Some(FocusedControlScope {
                context: context.clone(),
                profile: FocusedKeyProfile::MacosAtomicSpecialKeyV1,
                key,
            })
        } else {
            None
        };
        let selecting = match (&snapshot.menu_context, &menu_scope) {
            (Some(current), Some(MenuBindingScope::Item { context, .. }))
                if current == context && binding.rule == "menu_item" =>
            {
                true
            }
            (None, Some(MenuBindingScope::Opener { anchor, .. }))
                if snapshot.menu_anchors.contains(anchor)
                    && binding.rule != "menu_item"
                    && binding.rule != "window_activation" =>
            {
                false
            }
            (None, None) if binding.rule != "menu_item" => false,
            _ => return Err("binding proof does not belong to the current menu input domain"),
        };
        let object = snapshot
            .objects
            .iter()
            .find(|o| o.instance_ref == binding.instance)
            .ok_or("the bound source instance is missing")?;
        let fingerprint = instance_fingerprint(snapshot, object);
        let activating = binding.rule == "window_activation";
        let window_root_scope = match &binding.continuity.scope {
            BindingProofScope::WindowRoot { table_scope } => Some(table_scope),
            _ => None,
        };
        if window_root_scope.is_some()
            && (!activating
                || menu_scope.is_some()
                || focus_scope.is_some()
                || freshness != SourceFreshness::Verified)
        {
            return Err("window-root evidence is restricted to verified window activation");
        }
        if !(snapshot.complete || activating && window_root_scope.is_some())
            || snapshot.observer_coverage.state
                == interaction_contracts::desktop::source::SourceState::Unavailable
            || (!activating && !object.enabled)
            || !object.in_view
            || object.secure
            || (!activating && focused_window_id(snapshot) != Some(object.window_id.as_str()))
            || env.layer_revision != binding.layer_revision
            || env.revision < binding.env_revision
            || fingerprint.digest() != binding.fingerprint_digest
            || binding.identity_boundary != snapshot.identity_boundary
        {
            return Err("the binding no longer matches current complete source evidence");
        }
        let evidence = if let Some(table_scope) = window_root_scope {
            derive_window_root_binding_evidence(snapshot, object, table_scope)
        } else {
            derive_binding_evidence(snapshot, object, BindingProofScope::WholeStructure)
        }
        .map_err(|_| "the current binding context has no valid Device witness")?;
        let purpose_supported = if activating {
            evidence.device_context.supports_window_activation()
        } else {
            evidence.device_context.supports_native_input()
        };
        if !purpose_supported {
            return Err("the current binding scope has no Native Device input witness");
        }
        if !binding.continuity.same_closure(&evidence.continuity) {
            return Err("the current binding context changed");
        }
        let context_matches =
            if window_root_scope.is_some() {
                match (&binding.device_context, &evidence.device_context) {
                (
                    interaction_contracts::desktop::binding::DeviceContextWitness::WindowRootV1 {
                        app: old_app, window: old_window, root: old_root, source: old_source,
                    },
                    interaction_contracts::desktop::binding::DeviceContextWitness::WindowRootV1 {
                        app, window, root, source,
                    },
                ) => old_app == app && old_window == window && old_root == root
                    && old_source.source_session_id == source.source_session_id
                    && old_source.session_epoch == source.session_epoch
                    && old_source.process == source.process
                    && old_source.origin_window_id == source.origin_window_id
                    && old_source.origin_window_frame == source.origin_window_frame
                    && old_source.verified_source_sequence <= source.verified_source_sequence
                    && old_source.verified_observed_ms <= source.verified_observed_ms,
                _ => false,
            }
            } else {
                binding.device_context == evidence.device_context
            };
        if !context_matches {
            return Err("the current Device context witness differs from the admitted binding");
        }
        let device_context = if focus_scope.is_some() {
            derive_focused_binding_evidence(snapshot, object, BindingProofScope::WholeStructure)
                .map_err(|_| "the focused binding context has no valid Device witness")?
                .device_context
        } else {
            evidence.device_context
        };
        let proof = BindingProof {
            binding: binding.binding_ref.clone(),
            instance: object.instance_ref.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            environment_revision: env.revision,
            layer_revision: env.layer_revision,
            fingerprint,
            display_frame: object.frame,
            continuity: evidence.continuity,
            device_context,
            menu_scope,
            focus_scope,
        };
        proof
            .validate()
            .map_err(|_| "the current binding descriptor is invalid or unbounded")?;
        let mut observation = self.observation_with_deadline(
            snapshot,
            binding.binding_ref.as_str(),
            freshness,
            if activating { ttl_ms.min(2000) } else { ttl_ms },
            limit_ms,
            source_deadline,
        )?;
        observation.binding_proof = Some(proof);
        if focused_key.is_some() {
            observation.purpose = ObservationPurpose::FocusedControl;
        }
        if selecting {
            if !observation.dependencies.contains_key("menu") {
                return Err("menu execution dependency is unavailable");
            }
            observation.purpose = ObservationPurpose::MenuSelection;
        }
        if activating {
            observation.purpose = ObservationPurpose::WindowActivation;
            observation.dependencies = ["window", "authorization"]
                .into_iter()
                .map(|name| {
                    self.dependencies
                        .get(name)
                        .copied()
                        .map(|v| (name.into(), v))
                        .ok_or("window activation dependency unavailable")
                })
                .collect::<Result<_, _>>()?;
            observation
                .validate()
                .map_err(|_| "invalid activation evidence")?;
        }
        observation
            .validate()
            .map_err(|_| "invalid current scoped execution evidence")?;
        Ok(observation)
    }

    /// Build the only Observation accepted for a table-row selection. Unlike
    /// generic bindings this re-derives the local viewport proof from the
    /// exact Source read that will be sent to the device.
    #[allow(clippy::too_many_arguments)]
    pub fn row_selection_observation(
        &self,
        env: &EnvSnapshot,
        binding: &RowBinding,
        freshness: SourceFreshness,
        ttl_ms: u64,
        limit_ms: u64,
        source_deadline: Option<u64>,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<Observation, &'static str> {
        let snapshot = &env.snapshot;
        snapshot
            .validate()
            .map_err(|_| "invalid current Source structure")?;
        if binding.source.table_instance_ref != binding.selector.table_instance_ref {
            return Err("row selection source and selector name different tables");
        }
        if !snapshot.complete {
            row_scope_coverage(snapshot, &binding.table_scope).map_err(
                |_| "partial Source has no complete unique census for the frozen row scope",
            )?;
            let candidate = snapshot
                .table_scope_search
                .as_ref()
                .filter(|search| search.requested_scope == binding.table_scope)
                .and_then(|search| search.unique_candidate())
                .ok_or("partial Source has no unique table for the frozen row scope")?;
            if candidate.table_instance_ref != binding.selector.table_instance_ref
                || candidate.table_instance_ref != binding.source.table_instance_ref
            {
                return Err("partial Source table census differs from the row binding");
            }
        }
        let Some(hit_point) = binding.hit_point else {
            return Err("row selection has no proof-bound candidate point");
        };
        if snapshot.observer_coverage.state
            == interaction_contracts::desktop::source::SourceState::Unavailable
            || snapshot.menu_context.is_some()
            || focused_window_id(snapshot) != Some(binding.source.window_id.as_str())
            || binding.snapshot_id != snapshot.snapshot_id
            || binding.source.snapshot_id != snapshot.snapshot_id
            || binding.source.source_sequence != snapshot.source_sequence
            || binding.source.observed_ms != snapshot.observed_ms
            || binding.env_revision != env.revision
            || binding.layer_revision != env.layer_revision
            || binding.identity_boundary != snapshot.identity_boundary
            || binding.source.selected != Some(false)
            || binding.source.ax_selected_settable != Some(true)
        {
            return Err("row selection binding is stale, unavailable or not settable");
        }
        binding
            .source
            .validate_against_snapshot(&binding.selector, snapshot, now_ms, max_age_ms)
            .map_err(|_| "row selection observation no longer matches its frozen selector")?;
        let object = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.source.row_instance_ref)
            .ok_or("the bound AXRow is missing from the current structure")?;
        if object.role != "AXRow"
            || object.window_id != binding.source.window_id
            || object.frame != binding.source.row_frame
            || object.secure
            || !object.in_view
            || instance_fingerprint(snapshot, object).digest() != binding.fingerprint_digest
        {
            return Err("the actual AXRow no longer matches its read-scoped binding");
        }
        let evidence = derive_row_selection_binding_evidence(
            snapshot,
            &binding.selector,
            &binding.source,
            hit_point,
            now_ms,
            max_age_ms,
        )
        .map_err(|_| "the current row has no valid local Device witness")?;
        if !evidence.device_context.supports_row_selection()
            || binding
                .continuity
                .as_ref()
                .is_none_or(|proof| !proof.same_closure(&evidence.continuity))
            || binding.device_context.as_ref() != Some(&evidence.device_context)
        {
            return Err("the current row closure differs from its admitted proof");
        }
        let proof = BindingProof {
            binding: binding.binding_ref.clone(),
            instance: object.instance_ref.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            environment_revision: env.revision,
            layer_revision: env.layer_revision,
            fingerprint: instance_fingerprint(snapshot, object),
            display_frame: object.frame,
            continuity: evidence.continuity,
            device_context: evidence.device_context,
            menu_scope: None,
            focus_scope: None,
        };
        proof
            .validate()
            .map_err(|_| "the current row binding descriptor is invalid")?;
        if !proof.supports_row_selection() {
            return Err("the row proof does not authorize AXSelected=true");
        }
        let mut observation = self.observation_with_deadline(
            snapshot,
            binding.binding_ref.as_str(),
            freshness,
            ttl_ms,
            limit_ms,
            source_deadline,
        )?;
        observation.binding_proof = Some(proof);
        observation.purpose = ObservationPurpose::RowSelection;
        observation.valid_until_ms = observation
            .valid_until_ms
            .min(binding.source.valid_until_ms);
        observation
            .validate()
            .map_err(|_| "invalid current row-selection evidence")?;
        Ok(observation)
    }

    pub fn grant(
        &self,
        root: &RootTaskId,
        observation: &Observation,
        valid_until_ms: u64,
    ) -> GrantPermit {
        GrantPermit {
            acked_stop_seq: self.acked_stop_seq,
            task_id: root.as_str().to_string(),
            target_ref: observation.target_ref.clone(),
            evidence_id: observation.observation_id.clone(),
            valid_until_ms: valid_until_ms.min(observation.valid_until_ms),
        }
    }

    /// Build a grant over the native-or-browser observation without
    /// translating one evidence type into the other.
    pub fn grant_execution(
        &self,
        root: &RootTaskId,
        observation: &ExecutionObservation,
        valid_until_ms: u64,
    ) -> GrantPermit {
        GrantPermit {
            acked_stop_seq: self.acked_stop_seq,
            task_id: root.as_str().to_string(),
            target_ref: observation.target_ref().to_string(),
            evidence_id: observation.observation_id().to_string(),
            valid_until_ms: valid_until_ms.min(observation.valid_until_ms()),
        }
    }

    pub fn intent(&self, spec: &IntentSpec<'_>) -> Result<SubmitIntent, &'static str> {
        let identity = self
            .identity()
            .ok_or("the device has not introduced itself")?;
        spec.observation
            .validate()
            .map_err(|_| "invalid action observation")?;
        if spec.observation.session_epoch != identity.session_epoch
            || spec.observation.surface != identity.surface
            || spec.observation.clock_domain != identity.clock_domain
            || spec.observation.valid_until_ms <= spec.now_ms
            || spec.observation.freshness != SourceFreshness::Verified
        {
            return Err("action observation is stale or belongs to another device context");
        }
        let proof = spec
            .observation
            .binding_proof
            .as_ref()
            .ok_or("the action observation has no complete binding proof")?;
        let background_ax = spec.observation.execution_domain == ExecutionDomain::BackgroundAx;
        if proof.binding != spec.plan.binding.binding_ref
            || proof.instance != spec.plan.binding.instance
            || proof.fingerprint.digest() != spec.plan.binding.fingerprint_digest
            || (!background_ax && proof.layer_revision != spec.plan.binding.layer_revision)
        {
            return Err("the action plan differs from admitted binding evidence");
        }
        let primitive: Primitive = spec.plan.primitive.clone();
        if background_ax
            && (spec.observation.purpose != ObservationPurpose::BackgroundAxAction
                || spec.plan.binding.rule != "background_ax"
                || spec.plan.path != ExecutionPath::Ax
                || primitive
                    != (Primitive::AxAction {
                        action: AxActionKind::Press,
                    })
                || !proof.supports_background_ax_press())
        {
            return Err("BackgroundAX permits only one proof-bound AXPress action");
        }
        let request = SubmitIntent {
            acked_stop_seq: self.acked_stop_seq,
            intent: ActionIntent {
                intent_id: format!("intent-{}-{}", spec.attempt, spec.sequence),
                attempt_id: Some(spec.attempt.as_str().to_string()),
                task_id: spec.root.as_str().to_string(),
                task_revision: spec.task_revision,
                root_epoch: spec.root_epoch,
                session_epoch: identity.session_epoch,
                execution_domain: spec.observation.execution_domain,
                permit: spec.permit.clone(),
                surface: identity.surface.clone(),
                target_ref: spec.observation.target_ref.clone(),
                binding_fingerprint: Some(spec.plan.binding.fingerprint_digest.clone()),
                owned_document_proof: None,
                binding_proof: Some(proof.clone()),
                coordinate_space: if spec.observation.execution_domain
                    == ExecutionDomain::BackgroundAx
                {
                    CoordinateSpace::TargetObject
                } else {
                    CoordinateSpace::DisplayPoints
                },
                dependencies: spec.observation.dependencies.clone(),
                evidence_id: spec.observation.observation_id.clone(),
                latest_start_ms: spec.now_ms.saturating_add(spec.start_slack_ms),
                duration_ms: spec.duration_ms.max(1),
                steps: vec![ScheduledPrimitive {
                    offset_ms: 0,
                    primitive,
                }],
            }
            .into(),
        };
        request
            .intent
            .validate()
            .map_err(|_| "invalid action binding, geometry or deadline")?;
        Ok(request)
    }

    /// Build the fixed checkbox action over one exact, Environment-admitted
    /// browser read. This does not pass through native Primitive builders.
    pub fn browser_intent(
        &self,
        spec: &BrowserIntentSpec<'_>,
    ) -> Result<SubmitIntent, &'static str> {
        let identity = self
            .identity()
            .ok_or("the device has not introduced itself")?;
        spec.observation
            .validate()
            .map_err(|_| "invalid browser action observation")?;
        if identity.session_epoch != spec.observation.session_epoch()
            || identity.surface != spec.observation.surface
            || identity.clock_domain != spec.observation.scope.clock_domain
            || spec.observation.freshness != SourceFreshness::Verified
            || spec.observation.valid_until_ms <= spec.now_ms
            || spec.observation.dependencies != *self.dependencies()
        {
            return Err("browser action evidence is stale or belongs to another device context");
        }
        let latest_start_ms = spec
            .now_ms
            .checked_add(spec.start_slack_ms)
            .ok_or("browser action start time overflow")?;
        let intent = BrowserActionIntent {
            intent_id: format!("intent-{}-{}", spec.attempt, spec.sequence),
            attempt_id: spec.attempt.as_str().to_string(),
            task_id: spec.root.as_str().to_string(),
            task_revision: spec.task_revision,
            root_epoch: spec.root_epoch,
            session_epoch: identity.session_epoch,
            permit: spec.permit.clone(),
            evidence: Box::new(spec.observation.clone()),
            latest_start_ms,
            duration_ms: spec.duration_ms,
            action: spec.plan.action,
        };
        intent
            .validate_against_observation(spec.observation)
            .map_err(|_| "invalid browser action intent")?;
        let submit = SubmitIntent {
            acked_stop_seq: self.acked_stop_seq,
            intent: ExecutionIntent::Browser {
                intent: Box::new(intent),
            },
        };
        submit
            .intent
            .validate()
            .map_err(|_| "invalid browser action request")?;
        Ok(submit)
    }

    /// Construct exactly one viewport-scoped AXSelected=true intent. The
    /// Device still independently hit-tests the proof-bound point and checks
    /// that the native target is the corresponding AXRow.
    pub fn row_selection_intent(
        &self,
        spec: &RowSelectionIntentSpec<'_>,
    ) -> Result<SubmitIntent, &'static str> {
        let identity = self
            .identity()
            .ok_or("the device has not introduced itself")?;
        spec.observation
            .validate()
            .map_err(|_| "invalid row-selection observation")?;
        if spec.observation.purpose != ObservationPurpose::RowSelection
            || spec.observation.freshness != SourceFreshness::Verified
            || spec.observation.session_epoch != identity.session_epoch
            || spec.observation.surface != identity.surface
            || spec.observation.clock_domain != identity.clock_domain
            || spec.observation.observed_ms > spec.now_ms
            || spec.observation.valid_until_ms <= spec.now_ms
        {
            return Err("row-selection observation is stale or belongs to another device context");
        }
        let proof = spec
            .observation
            .binding_proof
            .as_ref()
            .ok_or("row-selection observation has no binding proof")?;
        let binding = &spec.plan.binding;
        let hit_point = binding
            .hit_point
            .ok_or("row-selection action has no proof-bound point")?;
        if !proof.supports_row_selection()
            || proof.binding != binding.binding_ref
            || proof.instance != binding.source.row_instance_ref
            || proof.snapshot_id != binding.snapshot_id
            || proof.fingerprint.digest() != binding.fingerprint_digest
            || proof.display_frame != binding.source.row_frame
            || proof.continuity.source_sequence != binding.source.source_sequence
            || proof.continuity.source_sequence != spec.observation.source_sequence
            || proof.row_selection_point() != Some((hit_point.x, hit_point.y))
            || spec.observation.target_ref != binding.binding_ref.as_str()
            || binding.source.selected != Some(false)
            || binding.source.ax_selected_settable != Some(true)
            || binding.device_context.as_ref() != Some(&proof.device_context)
            || binding
                .continuity
                .as_ref()
                .is_none_or(|continuity| !continuity.same_closure(&proof.continuity))
        {
            return Err("row-selection action differs from its exact read-scoped binding");
        }
        let latest_start_ms = spec
            .now_ms
            .checked_add(spec.start_slack_ms)
            .ok_or("row-selection start time overflow")?;
        let end_ms = latest_start_ms
            .checked_add(spec.duration_ms)
            .ok_or("row-selection deadline overflow")?;
        if spec.duration_ms == 0
            || spec.duration_ms > 1_500
            || end_ms > spec.observation.valid_until_ms
            || spec.plan.primitive()
                != (Primitive::AxAction {
                    action: AxActionKind::SetSelectedTrue,
                })
        {
            return Err("row-selection intent exceeds its evidence or action bound");
        }
        let request = SubmitIntent {
            acked_stop_seq: self.acked_stop_seq,
            intent: ActionIntent {
                intent_id: format!("intent-{}-{}", spec.attempt, spec.sequence),
                attempt_id: Some(spec.attempt.as_str().to_string()),
                task_id: spec.root.as_str().to_string(),
                task_revision: spec.task_revision,
                root_epoch: spec.root_epoch,
                session_epoch: identity.session_epoch,
                execution_domain: spec.observation.execution_domain,
                permit: spec.permit.clone(),
                surface: identity.surface.clone(),
                target_ref: spec.observation.target_ref.clone(),
                binding_fingerprint: Some(binding.fingerprint_digest.clone()),
                owned_document_proof: None,
                binding_proof: Some(proof.clone()),
                coordinate_space: CoordinateSpace::DisplayPoints,
                dependencies: spec.observation.dependencies.clone(),
                evidence_id: spec.observation.observation_id.clone(),
                latest_start_ms,
                duration_ms: spec.duration_ms,
                steps: vec![ScheduledPrimitive {
                    offset_ms: 0,
                    primitive: spec.plan.primitive(),
                }],
            }
            .into(),
        };
        request
            .intent
            .validate()
            .map_err(|_| "invalid row-selection intent")?;
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reaction::skills::ExecutionPath;
    use crate::testkit::*;
    use execution_core::core::{CoreConfig, ExecutionCore};
    use execution_core::sim_backend::SimBackend;
    use interaction_contracts::desktop::device::{BackendKind, ReceiptStatus};
    use interaction_contracts::desktop::primitive::{AxActionKind, PrimitivePolicy};
    use interaction_contracts::desktop::source::ObjectIdentityContinuity;

    fn observe_expect(attempt: &str, observation_id: &str) -> Expect {
        Expect::Observe {
            attempt: AttemptId::new(attempt).unwrap(),
            observation_id: observation_id.into(),
        }
    }

    fn grant_expect(attempt: &str, observation_id: &str) -> Expect {
        Expect::Grant {
            attempt: AttemptId::new(attempt).unwrap(),
            observation_id: observation_id.into(),
        }
    }

    fn device() -> (ExecutionCore<SimBackend>, DeviceView) {
        let mut core = ExecutionCore::new(CoreConfig::production("sim"), SimBackend::new());
        core.advance_to(100).unwrap();
        core.renew_lease(60_000);
        let mut view = DeviceView::new();
        view.install_manifest(CapabilityManifest {
            identity: core.identity(),
            backend: BackendKind::Macos,
            execution_domain: ExecutionDomain::ForegroundNative,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Available,
            input: CapabilityState::Available,
            domains: crate::testkit::domain_capabilities_for_test_profile(
                ExecutionDomain::ForegroundNative,
            ),
            max_uninterruptible_ms: 100,
        });
        (core, view)
    }

    #[test]
    fn finishing_an_attempt_keeps_a_grant_reply_for_revocation() {
        let mut view = DeviceView::new();
        let old_observe = view.send(10, observe_expect("attempt-old", "observation-old"));
        let old_grant = view.send(10, grant_expect("attempt-old", "observation-old"));
        let old_submit = view.send(
            10,
            Expect::Submit {
                intent_id: "intent-old".into(),
            },
        );
        let query = view.send(
            10,
            Expect::Query {
                intent_id: "intent-old".into(),
            },
        );
        let dependency = view.send(10, Expect::Dependency);
        view.forget_attempt_waits();
        let new = view.send(20, grant_expect("attempt-new", "observation-new"));
        assert!(view.reply(old_observe).is_none());
        assert_eq!(
            view.reply(old_grant),
            Some(grant_expect("attempt-old", "observation-old"))
        );
        assert!(view.reply(old_submit).is_none());
        assert_eq!(
            view.reply(new),
            Some(grant_expect("attempt-new", "observation-new"))
        );
        assert_eq!(
            view.reply(query),
            Some(Expect::Query {
                intent_id: "intent-old".into()
            })
        );
        assert_eq!(view.reply(dependency), Some(Expect::Dependency));
    }

    #[test]
    fn what_the_view_builds_is_what_the_device_accepts() {
        let (mut core, view) = device();
        let root = RootTaskId::new("root-1").unwrap();
        core.register_root(&RootGrant {
            root_id: root.as_str().into(),
            revision: 1,
            root_epoch: 1,
            deadline_ms: 60_000,
        })
        .unwrap();

        let mut rig = EnvRig::new();
        rig.observe(50);
        let binding = rig.bind_goal_target();
        let plan = ActionPlan {
            binding: binding.clone(),
            primitive: Primitive::AxAction {
                action: AxActionKind::SetValue {
                    value: "off".into(),
                },
            },
            path: ExecutionPath::Ax,
            note: "set it".into(),
        };

        let observation = view
            .bound_observation(
                rig.state.latest().unwrap(),
                &binding,
                SourceFreshness::Verified,
                3_000,
                60_000,
                None,
            )
            .unwrap();
        assert_eq!(observation.valid_until_ms, 3_050);
        core.observe(observation.clone()).unwrap();
        let grant = view.grant(&root, &observation, 3_050);
        let permit = core.grant_permit(&grant).unwrap();
        let attempt = AttemptId::new("att-1").unwrap();
        let request = view
            .intent(&IntentSpec {
                attempt: &attempt,
                root: &root,
                task_revision: 1,
                root_epoch: 1,
                permit: &permit,
                observation: &observation,
                plan: &plan,
                sequence: 1,
                now_ms: 100,
                start_slack_ms: 500,
                duration_ms: 100,
            })
            .unwrap();
        assert_eq!(request.intent.intent_id(), "intent-att-1-1");
        let native_intent = request
            .intent
            .native()
            .expect("this regression covers a native intent");
        assert_eq!(
            native_intent.binding_fingerprint.as_deref(),
            Some(binding.fingerprint_digest.as_str())
        );

        let receipt = core.submit(request);
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        core.dispatch_next().unwrap();
        core.advance_to(300).unwrap();
        let done = core.query_receipt(1, "intent-att-1-1").unwrap();
        assert_eq!(done.status, ReceiptStatus::Completed);
        let applied = core.backend().events();
        assert_eq!(applied.len(), 1);
        assert_eq!(
            applied[0].binding_fingerprint.as_deref(),
            Some(binding.fingerprint_digest.as_str())
        );
    }

    #[test]
    fn a_bound_observation_preserves_descriptor_but_refreshes_source_provenance() {
        let (_, view) = device();
        let mut rig = EnvRig::new();
        for reference in [
            "sim-window-w1",
            "group-push",
            "sw-push-daily",
            "sw-push-weekly",
        ] {
            rig.app.set_object_identity_continuity(
                reference,
                ObjectIdentityContinuity::NotificationCovered,
            );
        }
        rig.observe(50);
        let binding = rig.bind_goal_target();
        let old = view
            .bound_observation(
                rig.state.latest().unwrap(),
                &binding,
                SourceFreshness::Verified,
                3_000,
                60_000,
                None,
            )
            .unwrap();
        rig.observe(80);
        let fresh = view
            .bound_observation(
                rig.state.latest().unwrap(),
                &binding,
                SourceFreshness::Verified,
                3_000,
                60_000,
                None,
            )
            .unwrap();
        let a = old.binding_proof.as_ref().unwrap();
        let b = fresh.binding_proof.as_ref().unwrap();
        assert!(a.same_instance(b));
        assert_eq!(a.device_context, binding.device_context);
        assert_eq!(b.device_context, binding.device_context);
        assert_ne!(a.snapshot_id, b.snapshot_id);
        assert_eq!(
            b.snapshot_id,
            rig.state.latest().unwrap().snapshot.snapshot_id
        );
        assert!(b.environment_revision > a.environment_revision);
        assert!(fresh.source_sequence > old.source_sequence);
    }

    #[test]
    fn generic_binding_observation_still_rejects_a_partial_structure() {
        let (_, view) = device();
        let mut rig = EnvRig::new();
        rig.observe(50);
        let binding = rig.bind_goal_target();
        let mut env = rig.state.latest().unwrap().clone();
        env.snapshot.complete = false;
        env.snapshot.incomplete_reason = Some("AX child list truncated".into());

        assert!(
            view.bound_observation(
                &env,
                &binding,
                SourceFreshness::Verified,
                3_000,
                60_000,
                None,
            )
            .is_err(),
            "a generic binding cannot inherit the row-only scoped-census exception"
        );
    }

    fn actual_scoped_activation_binding() -> (DeviceView, EnvSnapshot, Binding) {
        let snapshot: StructureSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/desktop-v1/native-binding/scoped-window-synthetic.json"
        )))
        .unwrap();
        snapshot.validate().unwrap();
        let root_ref = snapshot
            .objects
            .iter()
            .find(|object| {
                object.parent.is_none()
                    && Some(object.window_id.as_str())
                        == snapshot.global_focus.known_window_for_app(&snapshot.app)
            })
            .unwrap()
            .instance_ref
            .clone();
        let scope = snapshot
            .table_scope_search
            .as_ref()
            .unwrap()
            .requested_scope
            .clone();
        let (_, mut view) = device();
        let identity = &mut view.manifest.as_mut().unwrap().identity;
        identity.session_epoch = snapshot.session_epoch;
        identity.surface = snapshot.surface.clone();
        identity.clock_domain = snapshot.clock_domain.clone();
        view.dependencies.insert("authorization".into(), 1);
        view.dependencies.insert("window".into(), 1);
        let env = EnvSnapshot {
            revision: 1,
            layer_revision: 1,
            snapshot,
            page: None,
        };
        let mut binder = crate::environment::binding::BindingReducer::new();
        let binding = binder.bind_window_root(&env, &root_ref, &scope).unwrap();
        (view, env, binding)
    }

    #[test]
    fn actual_partial_window_observation_is_activation_only_and_bounded_to_two_seconds() {
        let (view, env, binding) = actual_scoped_activation_binding();
        let observation = view
            .bound_observation(
                &env,
                &binding,
                SourceFreshness::Verified,
                10_000,
                u64::MAX,
                None,
            )
            .unwrap();
        assert_eq!(observation.purpose, ObservationPurpose::WindowActivation);
        assert_eq!(observation.valid_until_ms, env.snapshot.observed_ms + 2000);
        assert_eq!(
            observation
                .dependencies
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["authorization", "window"]
        );
        let proof = observation.binding_proof.as_ref().unwrap();
        assert!(proof.supports_window_activation());
        assert!(!proof.supports_native_input());
        assert!(!proof.supports_row_selection());
        let clipped = view
            .bound_observation(
                &env,
                &binding,
                SourceFreshness::Verified,
                10_000,
                u64::MAX,
                Some(env.snapshot.observed_ms + 99),
            )
            .unwrap();
        assert_eq!(clipped.valid_until_ms, env.snapshot.observed_ms + 99);
    }

    #[test]
    fn window_root_cannot_be_reused_for_generic_input_changed_scope_or_missing_authorization() {
        let (mut view, env, binding) = actual_scoped_activation_binding();
        let mut generic = binding.clone();
        generic.rule = "instance".into();
        assert!(
            view.bound_observation(
                &env,
                &generic,
                SourceFreshness::Verified,
                500,
                u64::MAX,
                None
            )
            .is_err()
        );
        assert!(
            view.bound_observation(
                &env,
                &binding,
                SourceFreshness::Uncertain,
                500,
                u64::MAX,
                None
            )
            .is_err()
        );
        let mut wrong_scope = binding.clone();
        if let BindingProofScope::WindowRoot { table_scope } = &mut wrong_scope.continuity.scope {
            table_scope.column_headers[0] = "different frozen header".into();
        }
        assert!(
            view.bound_observation(
                &env,
                &wrong_scope,
                SourceFreshness::Verified,
                500,
                u64::MAX,
                None
            )
            .is_err()
        );
        view.dependencies.remove("authorization");
        assert!(
            view.bound_observation(
                &env,
                &binding,
                SourceFreshness::Verified,
                500,
                u64::MAX,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn covered_window_root_refresh_uses_real_new_provenance_without_rebinding_identity() {
        let (view, mut env, binding) = actual_scoped_activation_binding();
        let old = view
            .bound_observation(
                &env,
                &binding,
                SourceFreshness::Verified,
                500,
                u64::MAX,
                None,
            )
            .unwrap();
        assert_eq!(
            binding.continuity.kind,
            ObjectIdentityContinuity::NotificationCovered
        );
        env.revision += 1;
        env.snapshot.snapshot_id =
            interaction_contracts::desktop::ids::RequestId::new("fresh-root-read").unwrap();
        env.snapshot.source_sequence += 1;
        env.snapshot.observed_ms += 1;
        let context = env.snapshot.window_context.as_mut().unwrap();
        context.verified_source_sequence = env.snapshot.source_sequence;
        context.verified_observed_ms = env.snapshot.observed_ms;
        let fresh = view
            .bound_observation(
                &env,
                &binding,
                SourceFreshness::Verified,
                500,
                u64::MAX,
                None,
            )
            .unwrap();
        assert_eq!(fresh.source_sequence, old.source_sequence + 1);
        assert_eq!(fresh.observed_ms, old.observed_ms + 1);
        let old_proof = old.binding_proof.as_ref().unwrap();
        let fresh_proof = fresh.binding_proof.as_ref().unwrap();
        assert!(old_proof.same_instance(fresh_proof));
        assert!(old_proof.continuity.same_closure(&fresh_proof.continuity));
        assert_ne!(old_proof.device_context, fresh_proof.device_context);
    }

    #[test]
    fn an_identical_replacement_object_cannot_reuse_a_source_binding() {
        let (_, view) = device();
        let mut rig = EnvRig::new();
        rig.observe(50);
        let binding = rig.bind_goal_target();
        let mut env = rig.state.latest().unwrap().clone();
        let object = env
            .snapshot
            .objects
            .iter_mut()
            .find(|o| o.instance_ref == binding.instance)
            .unwrap();
        object.instance_ref =
            interaction_contracts::desktop::ids::ObjectRef::new("identical-replacement").unwrap();
        assert!(
            view.bound_observation(
                &env,
                &binding,
                SourceFreshness::Verified,
                3_000,
                60_000,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn evidence_from_another_session_surface_or_clock_is_not_built() {
        let (_, view) = device();
        let mut rig = EnvRig::new();
        rig.observe(50);
        let good = rig.state.latest().unwrap().snapshot.clone();
        assert!(
            view.observation(&good, "b", SourceFreshness::Verified, 1_000, 10_000)
                .is_ok()
        );
        let mut other_clock = good.clone();
        other_clock.clock_domain = "other/1/monotonic".into();
        let mut other_session = good.clone();
        other_session.session_epoch = 9;
        let mut other_surface = good.clone();
        other_surface.surface.epoch = 9;
        for bad in [other_clock, other_session, other_surface] {
            assert!(
                view.observation(&bad, "b", SourceFreshness::Verified, 1_000, 10_000)
                    .is_err()
            );
        }
        assert!(
            DeviceView::new()
                .observation(&good, "b", SourceFreshness::Verified, 1_000, 10_000)
                .is_err(),
            "nothing is built before the device introduces itself"
        );
    }

    #[test]
    fn evidence_never_outlives_the_root_and_the_permit_never_outlives_the_evidence() {
        let (_, view) = device();
        let mut rig = EnvRig::new();
        rig.observe(50);
        let snapshot = rig.state.latest().unwrap().snapshot.clone();
        let observation = view
            .observation(&snapshot, "b", SourceFreshness::Verified, 5_000, 2_000)
            .unwrap();
        assert_eq!(
            observation.valid_until_ms, 2_000,
            "clipped to the root's deadline"
        );
        let grant = view.grant(&RootTaskId::new("root-1").unwrap(), &observation, 9_000);
        assert_eq!(grant.valid_until_ms, 2_000);
        let capped = view
            .observation_with_deadline(
                &snapshot,
                "b",
                SourceFreshness::Verified,
                5_000,
                2_000,
                Some(700),
            )
            .unwrap();
        assert_eq!(
            capped.valid_until_ms, 700,
            "native content and heartbeat leases cap evidence"
        );
        assert_eq!(
            view.grant(&RootTaskId::new("root-1").unwrap(), &capped, 9_000)
                .valid_until_ms,
            700
        );
    }

    #[test]
    fn a_stop_is_acknowledged_only_once_the_task_runtime_has_processed_it() {
        let mut view = DeviceView::new();
        assert!(!view.behind_on_stops());
        view.stop_seen(3);
        assert!(view.behind_on_stops());
        assert_eq!(view.acked_stop_seq(), 0, "seeing a stop is not handling it");
        view.stop_processed(2);
        assert!(view.behind_on_stops());
        view.stop_processed(3);
        assert!(!view.behind_on_stops());
        assert_eq!(view.acked_stop_seq(), 3);
        view.stop_processed(1);
        assert_eq!(view.acked_stop_seq(), 3, "it never goes back");
    }

    #[test]
    fn commands_are_matched_to_their_replies_and_time_out() {
        let mut view = DeviceView::new();
        let observe = observe_expect("attempt", "observation");
        let grant = grant_expect("attempt", "observation");
        let a = view.send(100, observe.clone());
        let b = view.send(150, grant.clone());
        assert_ne!(a, b);
        assert!(view.is_waiting(|e| matches!(e, Expect::Grant { .. })));
        assert_eq!(view.reply(a), Some(observe));
        assert_eq!(view.reply(a), None, "a reply counts once");
        assert!(view.timed_out(200, 100).is_empty());
        let late = view.timed_out(400, 100);
        assert_eq!(late, vec![(b, grant)]);
        view.forget_attempt_waits();
        assert_eq!(
            view.reply(b),
            Some(grant_expect("attempt", "observation")),
            "a late grant stays correlated so Engine can revoke it"
        );
        assert_eq!(view.reply(b), None, "a late reply counts once");
    }

    #[test]
    fn expected_reply_peeks_without_consuming_outstanding_or_late_grants() {
        let mut view = DeviceView::new();
        let observe = observe_expect("attempt-live", "observation-live");
        let observe_corr = view.send(10, observe.clone());
        assert_eq!(view.expected_reply(observe_corr), Some(&observe));
        assert_eq!(view.expected_reply(observe_corr), Some(&observe));
        assert_eq!(view.reply(observe_corr), Some(observe));
        assert_eq!(view.expected_reply(observe_corr), None);

        let grant = grant_expect("attempt-late", "observation-late");
        let grant_corr = view.send(20, grant.clone());
        assert_eq!(view.timed_out(31, 10), vec![(grant_corr, grant.clone())]);
        assert_eq!(view.expected_reply(grant_corr), Some(&grant));
        assert_eq!(view.expected_reply(grant_corr), Some(&grant));
        assert_eq!(view.reply(grant_corr), Some(grant));
        assert_eq!(view.expected_reply(grant_corr), None);
        assert_eq!(view.expected_reply(u64::MAX), None);
    }

    #[test]
    fn timed_out_grant_tombstones_are_bounded_and_evict_oldest_correlation() {
        let mut view = DeviceView::new();
        let grants = (0..=LATE_GRANT_CAPACITY)
            .map(|index| {
                let expect =
                    grant_expect(&format!("attempt-{index}"), &format!("observation-{index}"));
                (view.send(0, expect.clone()), expect)
            })
            .collect::<Vec<_>>();

        let timed_out = view.timed_out(1, 0);
        assert_eq!(timed_out.len(), LATE_GRANT_CAPACITY + 1);
        assert_eq!(view.outstanding.len(), 0);
        assert_eq!(view.late_grants.len(), LATE_GRANT_CAPACITY);

        let (oldest_corr, _) = &grants[0];
        assert_eq!(view.expected_reply(*oldest_corr), None);
        let (oldest_retained_corr, oldest_retained_expect) = &grants[1];
        assert_eq!(
            view.expected_reply(*oldest_retained_corr),
            Some(oldest_retained_expect),
            "the oldest retained correlation keeps its original grant identity"
        );
        let (newest_corr, newest_expect) = grants.last().unwrap();
        assert_eq!(
            view.expected_reply(*newest_corr),
            Some(newest_expect),
            "the newest late grant remains routable"
        );
    }

    #[test]
    fn stop_numbers_belong_to_one_device_session() {
        let (core, mut view) = device();
        view.stop_processed(5);
        assert_eq!(view.acked_stop_seq(), 5);
        let mut identity = core.identity();
        identity.session_epoch += 1;
        view.install_manifest(CapabilityManifest {
            identity,
            backend: BackendKind::Simulated,
            execution_domain: ExecutionDomain::Simulated,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Available,
            input: CapabilityState::Available,
            domains: domain_capabilities_for_test_profile(ExecutionDomain::Simulated),
            max_uninterruptible_ms: 100,
        });
        assert_eq!(
            view.acked_stop_seq(),
            0,
            "a stop the new device latches must not look already handled"
        );
        view.stop_seen(1);
        assert!(view.behind_on_stops());
        view.forget();
        assert!(view.manifest().is_none() && !view.input_available() && !view.behind_on_stops());
    }

    #[test]
    fn a_new_device_session_forgets_what_was_in_flight() {
        let (core, mut view) = device();
        view.send(100, observe_expect("attempt", "observation"));
        let mut identity = core.identity();
        identity.session_epoch += 1;
        view.install_manifest(CapabilityManifest {
            identity,
            backend: BackendKind::Simulated,
            execution_domain: ExecutionDomain::Simulated,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Available,
            input: CapabilityState::Available,
            domains: domain_capabilities_for_test_profile(ExecutionDomain::Simulated),
            max_uninterruptible_ms: 100,
        });
        assert!(!view.is_waiting(|_| true));
        assert_eq!(view.session_epoch(), Some(2));
        assert!(view.input_available());
    }
}
