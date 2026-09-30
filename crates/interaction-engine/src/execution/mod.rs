//! Execution, as the host sees it (design 5.3 and 14).
//!
//! The device decides what may be submitted; the host only keeps what it needs
//! to ask correctly: who the device is, which dependency revisions it holds,
//! which stop the host has processed, and which commands still await a reply.
//! It builds the messages the device expects and never pretends to know more
//! than the device told it.

use std::collections::BTreeMap;

use interaction_contracts::desktop::device::{
    ActionIntent, CapabilityManifest, CapabilityState, CommandAck, CoordinateSpace,
    DependencyUpdate, DependencyVersions, DeviceEvent, DeviceIdentity, GrantOutcome, GrantPermit,
    LeaseRenew, Observation, PermitRef, QueryReceipt, Receipt, ResumeAck, ResumeCommand,
    RevokeExecution, RootGrant, ScheduledPrimitive, SourceFreshness, StopAck, StopCommand,
    SubmitIntent,
};
use interaction_contracts::desktop::ids::{AttemptId, RootTaskId};
use interaction_contracts::desktop::primitive::Primitive;
use interaction_contracts::desktop::source::StructureSnapshot;

use crate::reaction::skills::ActionPlan;

/// What the host asks of the device.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceCommand {
    RegisterRoot(RootGrant),
    Observe(Observation),
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
    Observe,
    Grant,
    Submit { intent_id: String },
    Revoke,
    Query { intent_id: String },
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
pub struct Dispatch {
    pub attempt: AttemptId,
    pub plan: ActionPlan,
    pub observation: Observation,
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

#[derive(Debug, Default)]
pub struct DeviceView {
    manifest: Option<CapabilityManifest>,
    dependencies: BTreeMap<String, u64>,
    acked_stop_seq: u64,
    seen_stop_seq: u64,
    next_corr: u64,
    outstanding: BTreeMap<u64, (Expect, u64)>,
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
            // Stop numbers belong to one device session. A number from the old
            // one must never count as having handled a stop in the new one.
            self.acked_stop_seq = 0;
            self.seen_stop_seq = 0;
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
        self.acked_stop_seq = 0;
        self.seen_stop_seq = 0;
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
        self.outstanding.remove(&corr).map(|(expect, _)| expect)
    }

    pub fn is_waiting(&self, matching: impl Fn(&Expect) -> bool) -> bool {
        self.outstanding
            .values()
            .any(|(expect, _)| matching(expect))
    }

    /// Commands that have waited longer than `limit_ms`, removed from the
    /// table: the caller decides what an unanswered command means.
    pub fn timed_out(&mut self, now_ms: u64, limit_ms: u64) -> Vec<(u64, Expect)> {
        let late: Vec<u64> = self
            .outstanding
            .iter()
            .filter(|(_, (_, sent))| now_ms.saturating_sub(*sent) > limit_ms)
            .map(|(corr, _)| *corr)
            .collect();
        late.into_iter()
            .filter_map(|corr| {
                self.outstanding
                    .remove(&corr)
                    .map(|(expect, _)| (corr, expect))
            })
            .collect()
    }

    pub fn forget_outstanding(&mut self) {
        self.outstanding.clear();
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
        let identity = self
            .identity()
            .ok_or("the device has not introduced itself")?;
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
            valid_until_ms: snapshot.observed_ms.saturating_add(ttl_ms).min(limit_ms),
            freshness,
            dependencies: self.dependencies.clone(),
        })
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

    pub fn intent(&self, spec: &IntentSpec<'_>) -> Result<SubmitIntent, &'static str> {
        let identity = self
            .identity()
            .ok_or("the device has not introduced itself")?;
        let primitive: Primitive = spec.plan.primitive.clone();
        Ok(SubmitIntent {
            acked_stop_seq: self.acked_stop_seq,
            intent: ActionIntent {
                intent_id: format!("intent-{}-{}", spec.attempt, spec.sequence),
                attempt_id: Some(spec.attempt.as_str().to_string()),
                task_id: spec.root.as_str().to_string(),
                task_revision: spec.task_revision,
                root_epoch: spec.root_epoch,
                session_epoch: identity.session_epoch,
                permit: spec.permit.clone(),
                surface: identity.surface.clone(),
                target_ref: spec.observation.target_ref.clone(),
                binding_fingerprint: Some(spec.plan.binding.fingerprint_digest.clone()),
                coordinate_space: CoordinateSpace::DisplayPoints,
                dependencies: spec.observation.dependencies.clone(),
                evidence_id: spec.observation.observation_id.clone(),
                latest_start_ms: spec.now_ms.saturating_add(spec.start_slack_ms),
                duration_ms: spec.duration_ms.max(1),
                steps: vec![ScheduledPrimitive {
                    offset_ms: 0,
                    primitive,
                }],
            },
        })
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

    fn device() -> (ExecutionCore<SimBackend>, DeviceView) {
        let mut core = ExecutionCore::new(CoreConfig::production("sim"), SimBackend::new());
        core.advance_to(100).unwrap();
        core.renew_lease(60_000);
        let mut view = DeviceView::new();
        view.install_manifest(CapabilityManifest {
            identity: core.identity(),
            backend: BackendKind::Simulated,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Available,
            input: CapabilityState::Available,
            max_uninterruptible_ms: 100,
        });
        (core, view)
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
        let snapshot = rig.state.latest().unwrap().snapshot.clone();
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
            .observation(
                &snapshot,
                binding.binding_ref.as_str(),
                SourceFreshness::Verified,
                3_000,
                60_000,
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
        assert_eq!(request.intent.intent_id, "intent-att-1-1");
        assert_eq!(
            request.intent.binding_fingerprint.as_deref(),
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
        let a = view.send(100, Expect::Observe);
        let b = view.send(150, Expect::Grant);
        assert_ne!(a, b);
        assert!(view.is_waiting(|e| *e == Expect::Grant));
        assert_eq!(view.reply(a), Some(Expect::Observe));
        assert_eq!(view.reply(a), None, "a reply counts once");
        assert!(view.timed_out(200, 100).is_empty());
        let late = view.timed_out(400, 100);
        assert_eq!(late, vec![(b, Expect::Grant)]);
        assert_eq!(
            view.reply(b),
            None,
            "a timed-out command's reply is ignored"
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
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Available,
            input: CapabilityState::Available,
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
        view.send(100, Expect::Observe);
        let mut identity = core.identity();
        identity.session_epoch += 1;
        view.install_manifest(CapabilityManifest {
            identity,
            backend: BackendKind::Simulated,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Available,
            input: CapabilityState::Available,
            max_uninterruptible_ms: 100,
        });
        assert!(!view.is_waiting(|_| true));
        assert_eq!(view.session_epoch(), Some(2));
        assert!(view.input_available());
    }
}
