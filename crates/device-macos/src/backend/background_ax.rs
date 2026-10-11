//! Dedicated background AXPress backend. Semantic TargetObject evidence never
//! enters the foreground point hit-test or global input path.

use std::collections::BTreeSet;

use execution_core::backend::{
    HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure, SubmitCompletion, SubmitContext,
    SubmitOutcome,
};
use interaction_contracts::desktop::binding::{BindingProof, DeviceContextWitness};
use interaction_contracts::desktop::device::{
    CoordinateSpace, ObservationPurpose, PermitRef, SourceFreshness,
};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
use interaction_contracts::desktop::source::KernelProcessIdentity;

use crate::guard::{GuardClient, GuardSnapshot, NativeBackgroundAxScope};

pub struct BackgroundAxBackend {
    target: (u32, String),
    window: String,
    process: KernelProcessIdentity,
    guard: Option<GuardClient>,
    ready: bool,
    guard_revision: Option<u64>,
    sequence: u64,
    pending: Option<(u64, u64, String)>,
}

impl BackgroundAxBackend {
    pub fn new(target: (u32, String), window: String, process: KernelProcessIdentity) -> Self {
        Self {
            target,
            window,
            process,
            guard: None,
            ready: false,
            guard_revision: None,
            sequence: 0,
            pending: None,
        }
    }

    pub fn attach(&mut self, guard: GuardClient) -> Result<(), String> {
        let expected = NativeBackgroundAxScope {
            pid: self.target.0,
            bundle_id: self.target.1.clone(),
            window_id: self.window.clone(),
            start_seconds: self.process.start_seconds,
            start_micros: self.process.start_micros,
        };
        if self.guard.is_some()
            || !guard.is_background_ax_execution()
            || guard.background_ax_scope().as_ref() != Some(&expected)
            || guard.execution_target() != Some((self.target.0, self.target.1.as_str()))
            || guard.execution_window() != Some(self.window.as_str())
        {
            return Err("BackgroundAx guard scope mismatch or already attached".into());
        }
        self.guard = Some(guard);
        Ok(())
    }

    pub fn guard_mut(&mut self) -> Option<&mut GuardClient> {
        self.guard.as_mut()
    }

    pub fn ready(&self) -> bool {
        self.ready
            && self
                .guard
                .as_ref()
                .is_some_and(GuardClient::is_background_ax_execution)
    }

    pub fn snapshot(&mut self, snapshot: &GuardSnapshot) {
        self.guard_revision = Some(snapshot.revision());
        self.ready = snapshot.accessibility
            && snapshot.human_input
            && snapshot.startup_mouse_buttons_clear
            && snapshot.authority_clock_ready
            && snapshot.host_alive
            && snapshot.supervisor_alive;
    }

    pub fn disable(&mut self) {
        self.ready = false;
        self.guard_revision = None;
        self.pending = None;
        if let Some(mut guard) = self.guard.take() {
            guard.terminate();
        }
    }

    fn supported(&self, primitive: &Primitive, context: &SubmitContext<'_>) -> bool {
        if !self.ready()
            || self.pending.is_some()
            || context.intent.execution_domain != ExecutionDomain::BackgroundAx
            || context.authority.evidence.execution_domain != ExecutionDomain::BackgroundAx
            || context.authority.evidence.purpose != ObservationPurpose::BackgroundAxAction
            || context.intent.coordinate_space != CoordinateSpace::TargetObject
            || context.intent.owned_document_proof.is_some()
            || context.intent.attempt_id.is_none()
            || !matches!(
                primitive,
                Primitive::AxAction {
                    action: AxActionKind::Press
                }
            )
            || context.intent.validate().is_err()
            || context.authority.evidence.validate().is_err()
        {
            return false;
        }
        let Some(original) = context.intent.binding_proof.as_ref() else {
            return false;
        };
        let Some(current) = context.authority.evidence.binding_proof.as_ref() else {
            return false;
        };
        background_proof_matches(
            original,
            &self.target,
            &self.window,
            &self.process,
            context.intent.session_epoch,
        ) && background_proof_matches(
            current,
            &self.target,
            &self.window,
            &self.process,
            context.authority.evidence.session_epoch,
        ) && original.same_instance(current)
            && context.authority.evidence.freshness == SourceFreshness::Verified
            && context.authority.evidence.source_sequence == current.continuity.source_sequence
            && context.authority.evidence.dependencies == context.intent.dependencies
            && context
                .intent
                .dependencies
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == BTreeSet::from([
                    "authorization",
                    "background_scope",
                    "binding",
                    "geometry",
                    "window",
                ])
    }

    pub fn submit(&mut self, primitive: &Primitive, context: &SubmitContext<'_>) -> SubmitOutcome {
        if !self.supported(primitive, context) {
            return SubmitOutcome::Rejected;
        }
        let Some(sequence) = self.sequence.checked_add(1) else {
            return SubmitOutcome::Rejected;
        };
        let Some(revision) = self.guard_revision else {
            return SubmitOutcome::Rejected;
        };
        let Some(guard) = self.guard.as_mut() else {
            return SubmitOutcome::Rejected;
        };
        if let Err(error) = guard.submit_background_ax_at_revision(context, primitive, revision) {
            eprintln!("BackgroundAx submission could not enqueue: {error}");
            return SubmitOutcome::Rejected;
        }
        self.sequence = sequence;
        self.pending = Some((
            sequence,
            context.intent.session_epoch,
            context.intent.intent_id.clone(),
        ));
        SubmitOutcome::Pending {
            sequence,
            reply_deadline_ms: context.authority.valid_until_ms.saturating_add(500),
        }
    }

    pub fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        let (sequence, session_epoch, intent_id) = self.pending.as_ref()?.clone();
        let reply = match self.guard.as_mut().map(GuardClient::poll_submission) {
            Some(Ok(Some(reply))) => reply,
            Some(Ok(None)) => return None,
            _ => {
                self.ready = false;
                self.pending = None;
                return Some(SubmitCompletion {
                    sequence,
                    session_epoch,
                    intent_id,
                    outcome: SubmitOutcome::Unknown,
                    submitted_at_ms: None,
                    input_end_ms: None,
                    failure_code: Some(FailureCode::EffectUnknown),
                });
            }
        };
        let (outcome, failure_code) = match reply.outcome.as_str() {
            "applied" => (SubmitOutcome::Applied, None),
            "unknown" => (SubmitOutcome::Unknown, Some(FailureCode::EffectUnknown)),
            "target_occluded" => (SubmitOutcome::Rejected, Some(FailureCode::TargetOccluded)),
            "expired" => (SubmitOutcome::Rejected, Some(FailureCode::EvidenceExpired)),
            "stop_latched" => (SubmitOutcome::Rejected, Some(FailureCode::StopLatched)),
            "revoked" => (SubmitOutcome::Rejected, Some(FailureCode::Cancelled)),
            _ => (SubmitOutcome::Rejected, Some(FailureCode::InputRejected)),
        };
        self.pending = None;
        Some(SubmitCompletion {
            sequence,
            session_epoch,
            intent_id,
            outcome,
            submitted_at_ms: reply.committed_ms,
            input_end_ms: reply.returned_ms,
            failure_code,
        })
    }

    pub fn revoke(&mut self, permit: &PermitRef) {
        if let Some(guard) = &mut self.guard {
            guard.revoke(permit);
        }
    }

    pub fn abandon_pending(&mut self) {
        self.ready = false;
        self.pending = None;
        if let Some(guard) = &mut self.guard {
            guard.terminate();
        }
    }
}

fn background_proof_matches(
    proof: &BindingProof,
    target: &(u32, String),
    window_id: &str,
    process: &KernelProcessIdentity,
    session_epoch: u64,
) -> bool {
    if !proof.supports_background_ax_press() {
        return false;
    }
    let DeviceContextWitness::BackgroundAxV1 {
        app,
        window,
        source,
        ..
    } = &proof.device_context
    else {
        return false;
    };
    app.bundle_id == target.1
        && app.pid == target.0
        && window.window_id == window_id
        && source.window.process == *process
        && source.window.session_epoch == session_epoch
        && source.window.origin_window_id == window_id
        && proof.fingerprint.app == target.1
        && proof.fingerprint.app_pid == target.0
        && proof.fingerprint.window_id == window_id
}

impl InputBackend for BackgroundAxBackend {
    fn submit(
        &mut self,
        _at_ms: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome {
        self.submit(primitive, context)
    }

    fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        self.poll_completion()
    }

    fn revoke(&mut self, permit: &PermitRef) {
        self.revoke(permit);
    }

    fn abandon_pending(&mut self) {
        self.abandon_pending();
    }

    fn release(&mut self, _at_ms: u64, _release: &Primitive) -> Result<(), ReleaseFailure> {
        Err(ReleaseFailure)
    }

    fn held(&self, _token: &str) -> execution_core::backend::HeldState {
        execution_core::backend::HeldState::Unknown
    }

    fn hit_test(&mut self, _request: &HitTestRequest<'_>) -> HitTestOutcome {
        HitTestOutcome::Unresolved
    }
}
