//! Engine-owned computer proposal lifecycle.
//!
//! Provider output remains inert until Engine validates the whole action batch,
//! recompiles one step against a fresh Environment read, and sends the result
//! through the ordinary DecisionBook and Attempt admission path.

use std::collections::VecDeque;

use interaction_contracts::desktop::computer::{
    ComputerBatchProposal, ComputerFence, ComputerFrameRequest,
};
use interaction_contracts::desktop::decision::{Candidate, DecisionSource};
use interaction_contracts::desktop::ids::RequestId;
use interaction_contracts::desktop::planning::Subgoal;
use interaction_contracts::desktop::source::{FrameMeta, SourceCommand, StructureSnapshot};

use crate::deliberation::computer_accounting::{
    self, ComputerCallCompletion, ComputerCallIdentity, ComputerCallOutcome, ComputerCallStage,
};
use crate::environment::computer_observation::{
    FrozenComputerObservation, validate_exact_frame_pair,
};
use crate::reaction::computer_batch::{
    ComputerActionKind, ComputerBatchCapabilities, ComputerBatchId, ComputerBatchProgress,
    ComputerBatchRunner, ComputerBatchStatus, ComputerBatchStepProposal, ComputerBatchStepReceipt,
    ComputerBatchTerminal,
};
use crate::reaction::computer_compiler::{
    self, CompiledComputerBatch, ComputerCompileContext, ComputerStepSuggestion,
};
use crate::tasks::TaskError;

use super::{Engine, EngineError, Output, Step};

const MAX_OPEN_CALLS: usize = 128;
const MAX_CALLS_PER_ROOT: u32 = 8;

/// A request for the Host's computer-model or image port. It carries no image
/// pixels and grants no Device authority.
#[derive(Debug, Clone, PartialEq)]
pub enum ComputerCommand {
    Begin {
        identity: ComputerCallIdentity,
        goal: String,
        subgoal: String,
    },
    Screenshot {
        identity: ComputerCallIdentity,
        request: Box<ComputerFrameRequest>,
    },
    CallFinished {
        identity: ComputerCallIdentity,
        effective_outcome: ComputerCallOutcome,
    },
    BatchOutcome {
        identity: ComputerCallIdentity,
        proposal: ComputerBatchProposal,
        completed_prefix: usize,
        total: usize,
        terminal: bool,
        reason: Option<String>,
    },
    Cancel {
        identity: ComputerCallIdentity,
        reason: String,
    },
}

/// Typed Host events for one Engine-owned computer conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum ComputerInput {
    ScreenshotNeeded {
        identity: ComputerCallIdentity,
        capture_after_ms: u64,
        provider: String,
        model: String,
    },
    Proposal {
        identity: ComputerCallIdentity,
        proposal: ComputerBatchProposal,
    },
    Finished {
        completion: ComputerCallCompletion,
    },
    Failed {
        completion: ComputerCallCompletion,
        reason: String,
    },
    /// Local observation/safety delivery failed after the SDK call was already
    /// accounted. It closes the private conversation without a second finish.
    ObservationFailed {
        identity: ComputerCallIdentity,
        reason: String,
    },
    /// The person declined or could not receive an SDK safety confirmation.
    /// Pause the Root so ordinary planning cannot retry the same action.
    SafetyUnconfirmed {
        identity: ComputerCallIdentity,
        reason: String,
    },
    /// Rejection/late native failure for an exact Engine-issued capture.
    /// Acceptance is transport bookkeeping, never observation evidence.
    CaptureRejected {
        request_id: RequestId,
        watch_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComputerScopeKey {
    root_id: String,
    task_revision: u64,
    root_epoch: u64,
    plan_revision: u64,
    subgoal_id: String,
}

#[derive(Debug, Clone)]
struct CaptureWait {
    after_ms: u64,
    stage: ComputerCallStage,
}

#[derive(Debug, Clone)]
struct SourceCapture {
    request_id: RequestId,
    generation: u64,
    watch_id: String,
    after_ms: u64,
    deadline_ms: u64,
}

#[derive(Debug, Clone)]
struct ObservationWait {
    step: ComputerBatchStepProposal,
    barrier_ms: u64,
}

#[derive(Debug)]
struct DeferredProposal {
    proposal: ComputerBatchProposal,
    after_ms: u64,
}

#[derive(Debug)]
struct ComputerPending {
    key: ComputerScopeKey,
    identity: ComputerCallIdentity,
    initial_snapshot: StructureSnapshot,
    initial_frame: Option<FrameMeta>,
    frame_request: Option<ComputerFrameRequest>,
    frozen: Option<FrozenComputerObservation>,
    ready_for_payload: bool,
    capture_wait: Option<CaptureWait>,
    source_capture: Option<SourceCapture>,
    provider_route: Option<(String, String)>,
    started_identities: Vec<ComputerCallIdentity>,
    proposal: Option<ComputerBatchProposal>,
    deferred_proposal: Option<DeferredProposal>,
    compiled: Option<CompiledComputerBatch>,
    batch_id: Option<ComputerBatchId>,
    runner: ComputerBatchRunner,
    outstanding: Option<ComputerBatchStepProposal>,
    observation_wait: Option<ObservationWait>,
    admission_candidate: Option<Candidate>,
    admission_step: Option<ComputerBatchStepProposal>,
    awaiting_fresh_after_ms: Option<u64>,
}

#[derive(Debug, Default)]
pub(super) struct ComputerLane {
    pending: Option<ComputerPending>,
    blocked: Option<ComputerScopeKey>,
    open_calls: VecDeque<ComputerCallIdentity>,
}

#[derive(Debug, Clone)]
pub(super) struct ComputerAttemptStep {
    pub step: ComputerBatchStepProposal,
}

impl ComputerLane {
    fn current_pending(&self, identity: &ComputerCallIdentity) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.identity == *identity)
    }

    pub(super) fn is_active(&self) -> bool {
        self.pending.is_some()
    }
}

impl Engine {
    pub(super) fn reconcile_computer_scope(&mut self) {
        let Some(identity) = self
            .computer
            .pending
            .as_ref()
            .map(|pending| pending.identity.clone())
        else {
            return;
        };
        if let Err(reason) = self.validate_computer_scope(&identity, self.now_ms) {
            self.cancel_computer_scope(&identity, &reason);
        }
    }

    pub(super) fn retire_computer_goal_satisfied(&mut self) {
        self.cancel_computer_lane(
            "the frozen RootGoal was satisfied before the ordered batch ended",
        );
    }

    pub(super) fn cancel_computer_for_recovery(&mut self) {
        self.cancel_computer_lane("Engine recovery never resumes computer inputs");
    }

    pub(super) fn cancel_computer_lane(&mut self, reason: &str) {
        if let Some(identity) = self
            .computer
            .pending
            .as_ref()
            .map(|pending| pending.identity.clone())
        {
            self.cancel_computer_scope(&identity, reason);
        }
    }

    pub(super) fn computer_blocks_current_subgoal(&self) -> bool {
        let Some(blocked) = &self.computer.blocked else {
            return false;
        };
        let Some(active) = self.tasks.active() else {
            return false;
        };
        let Some(subgoal) = self.next_subgoal() else {
            return false;
        };
        blocked.root_id == active.row.root_id.as_str()
            && blocked.task_revision == active.row.task_revision
            && blocked.root_epoch == active.row.root_epoch
            && blocked.plan_revision == active.row.plan_revision
            && blocked.subgoal_id == subgoal.id
    }

    pub(super) fn clear_computer_block(&mut self) {
        self.computer.blocked = None;
    }

    /// Persist one exact provider call before the Host starts its worker.
    pub fn computer_call_started(
        &mut self,
        at_ms: u64,
        identity: &ComputerCallIdentity,
        provider: &str,
        model: &str,
    ) -> Result<(), crate::infrastructure::storage::StoreError> {
        self.now_ms = self.now_ms.max(at_ms);
        let durable_call_count = match self
            .durable_computer_call_count(&identity.fence.root_task_id)
        {
            Ok(count) => count,
            Err(error) => {
                self.cancel_computer_scope(identity, "computer call limit could not be checked");
                return Err(error);
            }
        };
        let accepted = self.cfg.computer_proposals_enabled
            && self
                .run
                .as_ref()
                .is_none_or(|run| run.goal.allowed_scope.is_none())
            && self.computer.current_pending(identity)
            && self.validate_computer_scope(identity, at_ms).is_ok()
            && self.computer.pending.as_ref().is_some_and(|pending| {
                pending
                    .provider_route
                    .as_ref()
                    .is_none_or(|(route, model_name)| route == provider && model_name == model)
            })
            && !self.computer.open_calls.contains(identity)
            && !self
                .computer
                .pending
                .as_ref()
                .is_some_and(|pending| pending.started_identities.contains(identity))
            && durable_call_count < MAX_CALLS_PER_ROOT
            && self.computer.open_calls.len() < MAX_OPEN_CALLS;
        if !accepted {
            self.cancel_computer_scope(
                identity,
                "provider call did not match the live Engine scope",
            );
            return Err(crate::infrastructure::storage::StoreError::Conflict(
                "computer call did not match a live Engine scope".into(),
            ));
        }
        if let Err(error) =
            computer_accounting::started(&mut self.storage, identity, at_ms, provider, model)
        {
            self.cancel_computer_scope(identity, "computer call accounting could not be opened");
            return Err(error);
        }
        self.computer.open_calls.push_back(identity.clone());
        if let Some(pending) = self.computer.pending.as_mut() {
            pending.started_identities.push(identity.clone());
        }
        Ok(())
    }

    /// Close one previously started call even if its Engine scope was revoked
    /// while the Host worker was running.
    pub fn computer_call_finished(
        &mut self,
        completion: &ComputerCallCompletion,
    ) -> Result<ComputerCallOutcome, crate::infrastructure::storage::StoreError> {
        if !self.computer.open_calls.contains(&completion.identity) {
            return Err(crate::infrastructure::storage::StoreError::Conflict(
                "computer completion has no exact open Engine call".into(),
            ));
        }
        let effective = computer_accounting::finished(&mut self.storage, completion)?;
        if let Some(index) = self
            .computer
            .open_calls
            .iter()
            .position(|identity| identity == &completion.identity)
        {
            self.computer.open_calls.remove(index);
        }
        Ok(effective)
    }

    fn durable_computer_call_count(
        &self,
        root_id: &interaction_contracts::desktop::ids::RootTaskId,
    ) -> Result<u32, crate::infrastructure::storage::StoreError> {
        let count: i64 = self.storage.read().query_row(
            "SELECT COUNT(*) FROM computer_model_calls WHERE root_id=?1",
            [root_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Revalidate an image request immediately before Host pixel egress.
    pub fn validate_computer_frame_request(
        &self,
        identity: &ComputerCallIdentity,
        request: &ComputerFrameRequest,
        now_ms: u64,
    ) -> Result<(), String> {
        let pending = self
            .computer
            .pending
            .as_ref()
            .ok_or("no computer proposal scope is pending")?;
        if pending.identity != *identity
            || pending.frame_request.as_ref() != Some(request)
            || request.fence != identity.fence
        {
            return Err("computer frame request does not match the private pending scope".into());
        }
        self.validate_computer_scope(identity, now_ms)?;
        let frozen = pending
            .frozen
            .as_ref()
            .ok_or("computer frame request has no frozen Source pairing")?;
        self.state.validate_frozen_computer_observation(
            frozen,
            now_ms,
            self.cfg.evidence_max_age_ms,
        )
    }

    /// Recompile a held computer candidate at every admission boundary. A
    /// confirmation delay cannot preserve an old instance or AXPress meaning.
    pub(super) fn rebind_computer_admission(
        &mut self,
        candidate: &Candidate,
    ) -> Result<Candidate, String> {
        let Some((identity, original, step, compiled)) =
            self.computer.pending.as_ref().and_then(|pending| {
                Some((
                    pending.identity.clone(),
                    pending.admission_candidate.clone()?,
                    pending.admission_step.clone()?,
                    pending.compiled.clone()?,
                ))
            })
        else {
            return Err("computer candidate is not privately pending".into());
        };
        if &original != candidate
            || self
                .computer
                .pending
                .as_ref()
                .and_then(|pending| pending.outstanding.as_ref())
                .map(ComputerBatchStepProposal::token)
                != Some(step.token())
        {
            self.cancel_computer_scope(&identity, "computer candidate or step token changed");
            return Err("computer candidate does not match the exact pending Engine step".into());
        }
        if let Err(reason) = self.validate_computer_scope(&identity, self.now_ms) {
            self.cancel_computer_scope(&identity, &reason);
            return Err(reason);
        }
        let Some(subgoal) = self.next_subgoal() else {
            self.cancel_computer_scope(&identity, "approved sub-goal is no longer current");
            return Err("approved sub-goal is no longer current".into());
        };
        let suggestion = compiled
            .suggest_step(
                step.action_index(),
                &self.state,
                &mut self.bindings,
                &subgoal,
                self.now_ms,
                self.cfg.evidence_max_age_ms,
            )
            .map_err(|error| error.to_string());
        match suggestion {
            Ok(ComputerStepSuggestion::Candidate {
                candidate: rebound, ..
            }) if rebound.semantic_target == original.semantic_target
                && rebound.candidate_id == original.candidate_id
                && rebound.skill == original.skill
                && rebound.effect_class == original.effect_class
                && rebound.parameters == original.parameters
                && rebound.precondition == original.precondition
                && rebound.description == original.description =>
            {
                Ok(*rebound)
            }
            Ok(_) => {
                self.cancel_computer_scope(
                    &identity,
                    "fresh computer binding changed its admitted meaning",
                );
                Err("fresh computer binding changed its admitted meaning".into())
            }
            Err(reason) => {
                self.cancel_computer_scope(&identity, &reason);
                Err(reason)
            }
        }
    }

    pub(super) fn computer_admission_deadline(&self) -> Option<u64> {
        self.computer
            .pending
            .as_ref()?
            .admission_step
            .as_ref()
            .map(|step| step.fence().deadline_ms)
    }

    pub(super) fn validate_computer_attempt_step(
        &self,
        step: &ComputerAttemptStep,
    ) -> Result<(), String> {
        let pending = self
            .computer
            .pending
            .as_ref()
            .ok_or("computer scope was retired before input")?;
        if pending
            .outstanding
            .as_ref()
            .map(ComputerBatchStepProposal::token)
            != Some(step.step.token())
        {
            return Err("computer attempt no longer owns the outstanding batch step".into());
        }
        self.validate_computer_scope(&pending.identity, self.now_ms)?;
        if let Some(frozen) = &pending.frozen {
            self.state.validate_frozen_computer_observation(
                frozen,
                self.now_ms,
                self.cfg.evidence_max_age_ms,
            )?;
        }
        Ok(())
    }

    pub(super) fn on_computer_input(&mut self, input: ComputerInput) -> Result<(), EngineError> {
        match input {
            ComputerInput::Finished { completion } => {
                self.close_computer_call(completion, None);
            }
            ComputerInput::Failed {
                mut completion,
                reason,
            } => {
                if completion.outcome == ComputerCallOutcome::Proposal {
                    completion.outcome = ComputerCallOutcome::Failed;
                }
                self.close_computer_call(completion, Some(reason));
            }
            ComputerInput::ObservationFailed { identity, reason } => {
                if self.computer.current_pending(&identity) {
                    self.cancel_computer_scope(&identity, &reason);
                }
            }
            ComputerInput::SafetyUnconfirmed { identity, reason } => {
                if self.computer.current_pending(&identity) {
                    self.cancel_computer_scope(&identity, &reason);
                    self.pause_root("computer safety checks were not confirmed")?;
                }
            }
            ComputerInput::CaptureRejected {
                request_id,
                watch_id,
                reason,
            } => {
                let identity = self.computer.pending.as_ref().and_then(|pending| {
                    pending
                        .source_capture
                        .as_ref()
                        .filter(|capture| {
                            capture.request_id == request_id && capture.watch_id == watch_id
                        })
                        .map(|_| pending.identity.clone())
                });
                if let Some(identity) = identity {
                    self.cancel_computer_scope(&identity, &reason);
                }
            }
            ComputerInput::ScreenshotNeeded {
                identity,
                capture_after_ms,
                provider,
                model,
            } => self.computer_needs_screenshot(identity, capture_after_ms, provider, model),
            ComputerInput::Proposal { identity, proposal } => {
                self.install_computer_proposal(identity, proposal)?;
            }
        }
        Ok(())
    }

    fn close_computer_call(&mut self, completion: ComputerCallCompletion, failure: Option<String>) {
        let identity = completion.identity.clone();
        let outcome = match self.computer_call_finished(&completion) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.note(format!(
                    "computer model completion was not applied: {error}"
                ));
                return;
            }
        };
        self.emit(Output::Computer(Box::new(ComputerCommand::CallFinished {
            identity: identity.clone(),
            effective_outcome: outcome,
        })));
        let pending_matches = self
            .computer
            .pending
            .as_ref()
            .is_some_and(|pending| pending.identity == identity);
        if !pending_matches {
            return;
        }
        if outcome == ComputerCallOutcome::Proposal && failure.is_none() {
            if let Some(pending) = self.computer.pending.as_mut() {
                pending.ready_for_payload = true;
            }
        } else {
            self.cancel_computer_scope(
                &identity,
                failure
                    .as_deref()
                    .unwrap_or("computer provider call did not produce a proposal"),
            );
        }
    }

    fn validate_computer_scope(
        &self,
        identity: &ComputerCallIdentity,
        now_ms: u64,
    ) -> Result<(), String> {
        identity.validate()?;
        if now_ms >= identity.fence.deadline_ms {
            return Err("computer request deadline has elapsed".into());
        }
        let pending = self
            .computer
            .pending
            .as_ref()
            .ok_or("computer scope is no longer pending")?;
        if pending.identity != *identity {
            return Err("computer request identity was replaced".into());
        }
        let active = self.tasks.active().ok_or("root task is no longer active")?;
        let run = self
            .run
            .as_ref()
            .ok_or("root execution state is unavailable")?;
        if run.goal.allowed_scope.is_some() {
            return Err(
                "computer proposals have no trusted semantic binding for this scoped root".into(),
            );
        }
        let current_subgoal = self
            .next_subgoal()
            .ok_or("there is no current plan sub-goal")?;
        if run.goal.root_task_id != identity.fence.root_task_id
            || run.goal.task_revision != identity.fence.task_revision
            || active.row.root_id != identity.fence.root_task_id
            || active.row.task_revision != identity.fence.task_revision
            || active.row.root_epoch != identity.fence.root_epoch
            || active.row.plan_revision != identity.plan_revision
            || !computer_scope_state_active(active.row.state)
            || current_subgoal.id != identity.subgoal_id
            || current_subgoal.id != pending.key.subgoal_id
            || self.mode != interaction_contracts::desktop::operator::RunMode::Execute
        {
            return Err(
                "computer request no longer matches the active root, plan or sub-goal".into(),
            );
        }
        if let Some(frozen) = &pending.frozen {
            let env = self
                .state
                .latest()
                .ok_or("Source structure is unavailable")?;
            if !same_source_context(&pending.initial_snapshot, &env.snapshot) {
                return Err("computer Source identity or active layer changed".into());
            }
            self.state.validate_frozen_computer_watch(frozen, now_ms)?;
        } else {
            let env = self
                .state
                .latest()
                .ok_or("Source structure is unavailable")?;
            if !same_source_context(&pending.initial_snapshot, &env.snapshot)
                || env.snapshot.source_sequence < identity.fence.structure_sequence
            {
                return Err("computer begin watch identity changed".into());
            }
            if let Some(initial) = &pending.initial_frame {
                let current = self
                    .state
                    .current_watch_frame(now_ms)
                    .ok_or("computer source stream is no longer healthy")?;
                if !same_frame_watch(initial, current) {
                    return Err("computer source stream or window geometry changed".into());
                }
            }
        }
        Ok(())
    }

    fn cancel_computer_scope(&mut self, identity: &ComputerCallIdentity, reason: &str) {
        let Some(pending) = self.computer.pending.take() else {
            return;
        };
        if pending.identity != *identity {
            self.computer.pending = Some(pending);
            return;
        }
        if let Some(capture) = &pending.source_capture {
            self.emit(Output::Source {
                generation: capture.generation,
                command: SourceCommand::CancelCapture {
                    request_id: capture.request_id.clone(),
                    watch_id: capture.watch_id.clone(),
                },
            });
        }
        if let Some(proposal) = pending.proposal {
            let progress = pending.runner.progress();
            if let Some(progress) = progress {
                self.emit(Output::Computer(Box::new(ComputerCommand::BatchOutcome {
                    identity: identity.clone(),
                    proposal,
                    completed_prefix: progress.completed_prefix,
                    total: progress.total_actions,
                    terminal: true,
                    reason: Some(reason.to_string()),
                })));
            }
        }
        self.computer.blocked = Some(pending.key);
        self.emit(Output::Computer(Box::new(ComputerCommand::Cancel {
            identity: identity.clone(),
            reason: reason.to_string(),
        })));
        self.note(format!("computer proposal scope closed: {reason}"));
    }

    fn begin_computer_scope(&mut self, subgoal: Subgoal) -> Result<bool, EngineError> {
        if !self.cfg.computer_proposals_enabled || self.computer.pending.is_some() {
            return Ok(false);
        }
        let Some(active) = self.tasks.active() else {
            return Ok(false);
        };
        let Some(run) = self.run.as_ref() else {
            return Ok(false);
        };
        if run.goal.allowed_scope.is_some() {
            return Ok(false);
        }
        let Some(env) = self.state.latest().cloned() else {
            return Ok(false);
        };
        if subgoal.skill != "ui.activate"
            || (run.goal.target.row.is_some()
                && subgoal
                    .target
                    .as_ref()
                    .is_none_or(|target| target == &run.goal.target.semantic_target))
            || subgoal.scroll.is_some()
            || subgoal.select.is_some()
            || env.snapshot.menu_context.is_some()
            || env.snapshot.source_sequence == 0
        {
            return Ok(false);
        }
        let deadline = active
            .row
            .deadline_ms
            .unwrap_or_else(|| self.now_ms.saturating_add(self.cfg.planning_timeout_ms))
            .min(self.now_ms.saturating_add(self.cfg.planning_timeout_ms));
        if self.now_ms >= deadline {
            return Ok(false);
        }
        let key = ComputerScopeKey {
            root_id: active.row.root_id.as_str().to_string(),
            task_revision: active.row.task_revision,
            root_epoch: active.row.root_epoch,
            plan_revision: active.row.plan_revision,
            subgoal_id: subgoal.id.clone(),
        };
        if self.computer.blocked.as_ref() == Some(&key) {
            return Ok(false);
        }
        if self.durable_computer_call_count(&active.row.root_id)? >= MAX_CALLS_PER_ROOT {
            self.computer.blocked = Some(key);
            self.note("computer proposal call limit reached for this root task");
            return Ok(false);
        }
        let generation = computer_accounting::allocate_generation(&mut self.storage)?;
        let fence = ComputerFence {
            request_id: RequestId::new(format!("computer-{generation}"))
                .expect("a monotonic counter forms a valid request ID"),
            generation,
            root_task_id: active.row.root_id.clone(),
            task_revision: active.row.task_revision,
            root_epoch: active.row.root_epoch,
            snapshot_id: env.snapshot.snapshot_id.as_str().to_string(),
            structure_sequence: env.snapshot.source_sequence,
            deadline_ms: deadline,
        };
        let identity = ComputerCallIdentity {
            fence,
            plan_revision: active.row.plan_revision,
            subgoal_id: subgoal.id.clone(),
            created_ms: self.now_ms,
            stage: ComputerCallStage::Begin,
        };
        let pending = ComputerPending {
            key,
            identity: identity.clone(),
            initial_snapshot: env.snapshot.clone(),
            initial_frame: self
                .state
                .current_complete_frame(self.now_ms, self.cfg.evidence_max_age_ms)
                .cloned(),
            frame_request: None,
            frozen: None,
            ready_for_payload: false,
            capture_wait: None,
            source_capture: None,
            provider_route: None,
            started_identities: Vec::new(),
            proposal: None,
            deferred_proposal: None,
            compiled: None,
            batch_id: None,
            runner: ComputerBatchRunner::default(),
            outstanding: None,
            observation_wait: None,
            admission_candidate: None,
            admission_step: None,
            awaiting_fresh_after_ms: None,
        };
        let goal_text = run.goal.user_text.clone();
        self.computer.pending = Some(pending);
        self.emit(Output::Computer(Box::new(ComputerCommand::Begin {
            identity,
            goal: goal_text,
            subgoal: subgoal.description,
        })));
        Ok(true)
    }

    fn computer_needs_screenshot(
        &mut self,
        identity: ComputerCallIdentity,
        capture_after_ms: u64,
        provider: String,
        model: String,
    ) {
        let route_matches = self
            .computer
            .pending
            .as_ref()
            .filter(|pending| pending.identity == identity)
            .and_then(|pending| pending.provider_route.as_ref())
            .is_none_or(|route| route == &(provider.clone(), model.clone()));
        if !route_matches {
            self.cancel_computer_scope(&identity, "computer continuation changed provider route");
            return;
        }
        let Some(pending) = self.computer.pending.as_mut() else {
            return;
        };
        if pending.identity != identity
            || !pending.ready_for_payload
            || capture_after_ms < identity.created_ms
            || capture_after_ms >= identity.fence.deadline_ms
        {
            return;
        }
        let stage = if identity.stage == ComputerCallStage::Begin {
            ComputerCallStage::InitialScreenshot
        } else {
            ComputerCallStage::FreshScreenshot
        };
        pending.ready_for_payload = false;
        pending.provider_route = Some((provider.clone(), model.clone()));
        pending.capture_wait = Some(CaptureWait {
            after_ms: capture_after_ms,
            stage,
        });
    }

    fn install_computer_proposal(
        &mut self,
        identity: ComputerCallIdentity,
        proposal: ComputerBatchProposal,
    ) -> Result<(), EngineError> {
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.goal.allowed_scope.is_some())
        {
            self.cancel_computer_scope(
                &identity,
                "computer proposals have no trusted semantic binding for this scoped root",
            );
            return Ok(());
        }
        if self
            .validate_computer_scope(&identity, self.now_ms)
            .is_err()
        {
            self.cancel_computer_scope(&identity, "proposal arrived for a stale computer scope");
            return Ok(());
        }
        let Some(pending) = self.computer.pending.as_mut() else {
            return Ok(());
        };
        if pending.identity != identity || !pending.ready_for_payload {
            return Ok(());
        }
        if proposal.fence != identity.fence {
            self.cancel_computer_scope(
                &identity,
                "proposal fence differs from the admitted model call",
            );
            return Ok(());
        }
        let Some(frozen) = pending.frozen.clone() else {
            self.cancel_computer_scope(
                &identity,
                "proposal has no frozen screenshot and Source pair",
            );
            return Ok(());
        };
        if !self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
            || self
                .state
                .current_complete_frame(self.now_ms, self.cfg.evidence_max_age_ms)
                .is_none()
        {
            if let Err(error) = computer_compiler::validate_batch_capabilities(&proposal.actions) {
                self.cancel_computer_scope(&identity, &error.to_string());
                return Ok(());
            }
            let pending = self.computer.pending.as_mut().expect("checked above");
            pending.ready_for_payload = false;
            pending.deferred_proposal = Some(DeferredProposal {
                proposal,
                after_ms: self.now_ms,
            });
            self.request_source_capture(self.now_ms)?;
            return Ok(());
        }
        let Some(subgoal) = self.next_subgoal() else {
            self.cancel_computer_scope(&identity, "approved plan no longer has a current sub-goal");
            return Ok(());
        };
        let Some(run) = self.run.as_ref() else {
            return Ok(());
        };
        let context = ComputerCompileContext::new(
            &run.goal,
            &subgoal,
            &run.plan_targets,
            &run.pack,
            &self.state,
            self.now_ms,
            self.cfg.evidence_max_age_ms,
        );
        let compiled = match computer_compiler::compile_batch(frozen, proposal.clone(), context) {
            Ok(compiled) => compiled,
            Err(error) => {
                self.cancel_computer_scope(&identity, &error.to_string());
                return Ok(());
            }
        };
        let batch_value = computer_accounting::allocate_batch_id(&mut self.storage)?;
        let batch_id = ComputerBatchId::new(batch_value)
            .map_err(|error| TaskError::Invalid(error.to_string()))?;
        let Some(current) = self.current_computer_fence(&identity) else {
            self.cancel_computer_scope(&identity, "current Source fence is unavailable");
            return Ok(());
        };
        let capabilities = ComputerBatchCapabilities::new([
            ComputerActionKind::Click,
            ComputerActionKind::Wait,
            ComputerActionKind::Screenshot,
        ]);
        let Some(pending) = self.computer.pending.as_mut() else {
            return Ok(());
        };
        if pending
            .runner
            .install(
                batch_id,
                proposal.clone(),
                &capabilities,
                current,
                self.now_ms,
            )
            .is_err()
        {
            self.cancel_computer_scope(&identity, "the batch runner rejected the proposal fence");
            return Ok(());
        }
        pending.ready_for_payload = false;
        pending.proposal = Some(proposal);
        pending.compiled = Some(compiled);
        pending.batch_id = Some(batch_id);
        Ok(())
    }

    fn current_computer_fence(&self, identity: &ComputerCallIdentity) -> Option<ComputerFence> {
        let env = self.state.latest()?;
        Some(ComputerFence {
            snapshot_id: env.snapshot.snapshot_id.as_str().to_string(),
            structure_sequence: env.snapshot.source_sequence,
            ..identity.fence.clone()
        })
    }

    pub(super) fn computer_drive(&mut self) -> Step {
        if !self.cfg.computer_proposals_enabled {
            return Ok(false);
        }
        if let Some(identity) = self
            .computer
            .pending
            .as_ref()
            .map(|pending| pending.identity.clone())
        {
            if self
                .validate_computer_scope(&identity, self.now_ms)
                .is_err()
            {
                self.cancel_computer_scope(
                    &identity,
                    "root, plan, source identity or deadline changed",
                );
                return Ok(true);
            }
            if self.drive_screenshot_capture()? {
                return Ok(true);
            }
            if self.drive_deferred_computer_proposal()? {
                return Ok(true);
            }
            if self.drive_computer_observation()? {
                return Ok(true);
            }
            if self.drive_computer_step()? {
                return Ok(true);
            }
            return Ok(false);
        }
        if self.run.as_ref().is_some_and(|run| run.attempt.is_some())
            || self.decisions.pending().is_some()
            || self.planning.inflight().is_some()
        {
            return Ok(false);
        }
        if let Some(subgoal) = self.next_subgoal() {
            return self.begin_computer_scope(subgoal);
        }
        Ok(false)
    }

    fn request_source_capture(&mut self, after_ms: u64) -> Result<bool, EngineError> {
        let Some(pending) = self.computer.pending.as_ref() else {
            return Ok(false);
        };
        if pending.source_capture.is_some() {
            return Ok(false);
        }
        let Some(frame) = self.state.current_watch_frame(self.now_ms).cloned() else {
            return Ok(false);
        };
        let Some(watch_id) = frame.watch_id().map(str::to_string) else {
            return Ok(false);
        };
        let deadline_ms = pending.identity.fence.deadline_ms;
        if after_ms >= deadline_ms || self.now_ms >= deadline_ms {
            return Ok(false);
        }
        let generation = computer_accounting::allocate_generation(&mut self.storage)?;
        let request_id = RequestId::new(format!("computer-{generation}"))
            .expect("a monotonic counter forms a valid capture ID");
        let command = SourceCommand::CaptureNow {
            request_id: request_id.clone(),
            watch_id: watch_id.clone(),
            session_epoch: frame.session_epoch,
            surface: frame.surface,
            after_ms,
            deadline_ms,
        };
        command.validate().map_err(TaskError::Invalid)?;
        self.computer
            .pending
            .as_mut()
            .expect("checked above")
            .source_capture = Some(SourceCapture {
            request_id,
            generation,
            watch_id,
            after_ms,
            deadline_ms,
        });
        self.emit(Output::Source {
            generation,
            command,
        });
        Ok(true)
    }

    fn drive_deferred_computer_proposal(&mut self) -> Result<bool, EngineError> {
        let Some(pending) = self.computer.pending.as_ref() else {
            return Ok(false);
        };
        let Some(deferred) = &pending.deferred_proposal else {
            return Ok(false);
        };
        let after_ms = deferred.after_ms;
        let ready = self
            .state
            .latest()
            .zip(
                self.state
                    .current_complete_frame(self.now_ms, self.cfg.evidence_max_age_ms),
            )
            .is_some_and(|(env, frame)| {
                self.matches_requested_capture(frame)
                    && is_fresh_exact_pair_after(
                        &env.snapshot,
                        frame,
                        after_ms,
                        &pending.identity.fence.snapshot_id,
                        pending.identity.fence.structure_sequence,
                    )
            });
        if !ready {
            return self.request_source_capture(after_ms);
        }
        let pending = self.computer.pending.as_mut().expect("checked above");
        let identity = pending.identity.clone();
        let proposal = pending
            .deferred_proposal
            .take()
            .expect("checked above")
            .proposal;
        pending.ready_for_payload = true;
        self.retire_source_capture();
        self.install_computer_proposal(identity, proposal)?;
        Ok(true)
    }

    fn matches_requested_capture(&self, frame: &FrameMeta) -> bool {
        let Some(capture) = frame.capture_info() else {
            return true;
        };
        self.computer
            .pending
            .as_ref()
            .and_then(|pending| pending.source_capture.as_ref())
            .is_some_and(|request| {
                capture.request_id == request.request_id
                    && capture.watch_id == request.watch_id
                    && capture.requested_after_ms == request.after_ms
                    && capture.deadline_ms == request.deadline_ms
                    && capture.capture_started_ms > request.after_ms
                    && capture.capture_finished_ms <= request.deadline_ms
            })
    }

    fn retire_source_capture(&mut self) {
        if let Some(capture) = self
            .computer
            .pending
            .as_mut()
            .and_then(|pending| pending.source_capture.take())
        {
            self.emit(Output::Source {
                generation: capture.generation,
                command: SourceCommand::CancelCapture {
                    request_id: capture.request_id,
                    watch_id: capture.watch_id,
                },
            });
        }
    }

    fn drive_screenshot_capture(&mut self) -> Result<bool, EngineError> {
        let Some(pending) = self.computer.pending.as_ref() else {
            return Ok(false);
        };
        let Some(wait) = pending.capture_wait.clone() else {
            return Ok(false);
        };
        let Some(env) = self.state.latest().cloned() else {
            return Ok(false);
        };
        let Some(frame) = self
            .state
            .current_complete_frame(self.now_ms, self.cfg.evidence_max_age_ms)
            .cloned()
        else {
            return self.request_source_capture(wait.after_ms);
        };
        let old = &pending.identity.fence;
        if !self.matches_requested_capture(&frame)
            || !is_fresh_exact_pair_after(
                &env.snapshot,
                &frame,
                wait.after_ms,
                &old.snapshot_id,
                old.structure_sequence,
            )
            || !same_source_context(&pending.initial_snapshot, &env.snapshot)
            || pending
                .initial_frame
                .as_ref()
                .is_some_and(|initial| !same_frame_watch(initial, &frame))
        {
            return self.request_source_capture(wait.after_ms);
        }
        let generation = if let Some(capture) = &pending.source_capture {
            capture.generation
        } else {
            computer_accounting::allocate_generation(&mut self.storage)?
        };
        let mut fence = old.clone();
        fence.request_id = RequestId::new(format!("computer-{generation}"))
            .expect("a monotonic counter forms a valid request ID");
        fence.generation = generation;
        fence.snapshot_id = env.snapshot.snapshot_id.as_str().to_string();
        fence.structure_sequence = env.snapshot.source_sequence;
        let identity = ComputerCallIdentity {
            fence: fence.clone(),
            plan_revision: pending.key.plan_revision,
            subgoal_id: pending.key.subgoal_id.clone(),
            created_ms: self.now_ms,
            stage: wait.stage,
        };
        let request = ComputerFrameRequest {
            fence,
            expected_frame: frame,
        };
        let frozen = match self.state.freeze_computer_observation(
            request.clone(),
            self.now_ms,
            self.cfg.evidence_max_age_ms,
        ) {
            Ok(frozen) => frozen,
            Err(_) => return Ok(false),
        };
        let Some(pending) = self.computer.pending.as_mut() else {
            return Ok(false);
        };
        pending.identity = identity.clone();
        pending.frame_request = Some(request.clone());
        pending.frozen = Some(frozen);
        pending.capture_wait = None;
        self.retire_source_capture();
        self.emit(Output::Computer(Box::new(ComputerCommand::Screenshot {
            identity,
            request: Box::new(request),
        })));
        Ok(true)
    }

    fn drive_computer_observation(&mut self) -> Result<bool, EngineError> {
        let Some(wait) = self
            .computer
            .pending
            .as_ref()
            .and_then(|pending| pending.observation_wait.clone())
        else {
            return Ok(false);
        };
        let Some(identity) = self
            .computer
            .pending
            .as_ref()
            .map(|pending| pending.identity.clone())
        else {
            return Ok(false);
        };
        let Some(current) = self.current_computer_fence(&identity) else {
            return Ok(false);
        };
        let Some(frame) = self
            .state
            .current_complete_frame(self.now_ms, self.cfg.evidence_max_age_ms)
        else {
            return self.request_source_capture(wait.barrier_ms);
        };
        let Some(env) = self.state.latest() else {
            return Ok(false);
        };
        if !self.matches_requested_capture(frame)
            || !is_fresh_exact_pair_after(
                &env.snapshot,
                frame,
                wait.barrier_ms,
                &wait.step.fence().snapshot_id,
                wait.step.fence().structure_sequence,
            )
        {
            return self.request_source_capture(wait.barrier_ms);
        }
        let progress = {
            let Some(pending) = self.computer.pending.as_mut() else {
                return Ok(false);
            };
            let Some(step) = pending.outstanding.take() else {
                return Ok(false);
            };
            let progress = pending.runner.complete_step(
                step.token(),
                ComputerBatchStepReceipt::ObservationCompleted {
                    fresh_fence: current,
                },
                self.now_ms,
            );
            pending.observation_wait = None;
            pending.awaiting_fresh_after_ms = None;
            progress
        };
        self.retire_source_capture();
        match progress {
            Ok(progress) => self.apply_batch_progress(progress),
            Err(error) => {
                if let Some(identity) = self
                    .computer
                    .pending
                    .as_ref()
                    .map(|pending| pending.identity.clone())
                {
                    self.cancel_computer_scope(&identity, &error.to_string());
                }
            }
        }
        Ok(true)
    }

    fn drive_computer_step(&mut self) -> Result<bool, EngineError> {
        let Some(pending) = self.computer.pending.as_ref() else {
            return Ok(false);
        };
        if pending.capture_wait.is_some()
            || pending.observation_wait.is_some()
            || pending.proposal.is_none()
            || pending.outstanding.is_some()
        {
            return Ok(false);
        }
        let Some(progress) = pending.runner.progress() else {
            return Ok(false);
        };
        if progress.status == ComputerBatchStatus::AwaitingFreshObservation {
            let Some(barrier_ms) = pending.awaiting_fresh_after_ms else {
                return Ok(false);
            };
            let Some(env) = self.state.latest() else {
                return Ok(false);
            };
            let Some(frame) = self
                .state
                .current_complete_frame(self.now_ms, self.cfg.evidence_max_age_ms)
            else {
                return self.request_source_capture(barrier_ms);
            };
            if !self.matches_requested_capture(frame)
                || !is_fresh_exact_pair_after(
                    &env.snapshot,
                    frame,
                    barrier_ms,
                    &progress.current_fence.snapshot_id,
                    progress.current_fence.structure_sequence,
                )
            {
                return self.request_source_capture(barrier_ms);
            }
        }
        let identity = pending.identity.clone();
        self.retire_source_capture();
        let Some(fence) = self.current_computer_fence(&identity) else {
            return Ok(false);
        };
        let Some(subgoal) = self.next_subgoal() else {
            self.cancel_computer_scope(&identity, "the approved plan has no current sub-goal");
            return Ok(true);
        };
        let (step, suggestion) = {
            let Some(pending) = self.computer.pending.as_mut() else {
                return Ok(false);
            };
            let Some(compiled) = pending.compiled.as_ref() else {
                return Ok(false);
            };
            let step = match pending.runner.take_step(fence, self.now_ms) {
                Ok(step) => step,
                Err(error) => {
                    let reason = error.to_string();
                    self.cancel_computer_scope(&identity, &reason);
                    return Ok(true);
                }
            };
            pending.awaiting_fresh_after_ms = None;
            let suggestion = compiled.suggest_step(
                step.action_index(),
                &self.state,
                &mut self.bindings,
                &subgoal,
                self.now_ms,
                self.cfg.evidence_max_age_ms,
            );
            (step, suggestion)
        };
        match suggestion {
            Ok(ComputerStepSuggestion::Candidate { candidate, .. }) => {
                let decision = {
                    let Some(active) = self.tasks.active() else {
                        self.cancel_computer_scope(&identity, "root task ended before admission");
                        return Ok(true);
                    };
                    self.decisions.open_direct(
                        &mut self.storage,
                        &active.row.root_id,
                        self.now_ms,
                    )?
                };
                if let Some(pending) = self.computer.pending.as_mut() {
                    pending.outstanding = Some(step.clone());
                    pending.admission_candidate = Some((*candidate).clone());
                    pending.admission_step = Some(step.clone());
                }
                let _ =
                    self.admit_candidate(decision, 1, *candidate, DecisionSource::Computer, false)?;
                let attached = self
                    .run
                    .as_ref()
                    .and_then(|run| run.attempt.as_ref())
                    .is_some_and(|attempt| attempt.computer_step.is_some());
                if !attached && self.decisions.pending().is_none() {
                    self.cancel_computer_scope(
                        &identity,
                        "ordinary Attempt admission rejected the computer action",
                    );
                }
                Ok(true)
            }
            Ok(ComputerStepSuggestion::Observation { .. }) => {
                if let Some(pending) = self.computer.pending.as_mut() {
                    pending.outstanding = Some(step.clone());
                    pending.observation_wait = Some(ObservationWait {
                        step,
                        barrier_ms: self.now_ms,
                    });
                }
                Ok(true)
            }
            Err(error) => {
                self.cancel_computer_scope(&identity, &error.to_string());
                Ok(true)
            }
        }
    }

    fn apply_batch_progress(&mut self, progress: ComputerBatchProgress) {
        let Some(terminal) = progress.terminal.clone() else {
            return;
        };
        let Some((identity, proposal, key)) = self.computer.pending.as_ref().and_then(|pending| {
            Some((
                pending.identity.clone(),
                pending.proposal.clone()?,
                pending.key.clone(),
            ))
        }) else {
            return;
        };
        let (reason, completed) = match terminal {
            ComputerBatchTerminal::Completed => (None, true),
            ComputerBatchTerminal::Stopped(reason) => (Some(format!("{reason:?}")), false),
        };
        self.emit(Output::Computer(Box::new(ComputerCommand::BatchOutcome {
            identity: identity.clone(),
            proposal,
            completed_prefix: progress.completed_prefix,
            total: progress.total_actions,
            terminal: true,
            reason,
        })));
        if completed {
            if let Some(run) = self.run.as_mut() {
                run.done.insert(identity.subgoal_id.clone());
            }
            self.computer.pending = None;
        } else {
            self.computer.pending = None;
            self.computer.blocked = Some(key);
            self.emit(Output::Computer(Box::new(ComputerCommand::Cancel {
                identity,
                reason: "computer batch stopped before its full ordered prefix completed".into(),
            })));
        }
    }

    pub(super) fn take_computer_attempt_step(&mut self) -> Option<ComputerAttemptStep> {
        let pending = self.computer.pending.as_mut()?;
        if pending.identity.subgoal_id.is_empty() {
            return None;
        }
        pending.admission_candidate.take()?;
        pending
            .admission_step
            .take()
            .map(|step| ComputerAttemptStep { step })
    }

    pub(super) fn complete_computer_attempt(&mut self, step: ComputerAttemptStep, success: bool) {
        let identity = self
            .computer
            .pending
            .as_ref()
            .map(|pending| pending.identity.clone());
        let Some(identity) = identity else {
            return;
        };
        let progress = {
            let Some(pending) = self.computer.pending.as_mut() else {
                return;
            };
            if pending
                .outstanding
                .as_ref()
                .map(ComputerBatchStepProposal::token)
                != Some(step.step.token())
            {
                return;
            }
            pending.outstanding = None;
            if success {
                pending.awaiting_fresh_after_ms = Some(self.now_ms);
                pending.runner.complete_step(
                    step.step.token(),
                    ComputerBatchStepReceipt::InputCompleted,
                    self.now_ms,
                )
            } else {
                pending.runner.complete_step(
                    step.step.token(),
                    ComputerBatchStepReceipt::Unknown,
                    self.now_ms,
                )
            }
        };
        match progress {
            Ok(progress) => self.apply_batch_progress(progress),
            Err(error) => self.cancel_computer_scope(&identity, &error.to_string()),
        }
    }
}

fn same_source_context(first: &StructureSnapshot, current: &StructureSnapshot) -> bool {
    first.app == current.app
        && first.session_epoch == current.session_epoch
        && first.surface == current.surface
        && first.clock_domain == current.clock_domain
        && first.global_focus == current.global_focus
        && match (&first.identity_boundary, &current.identity_boundary) {
            (None, None) => true,
            (Some(first), Some(current)) => {
                first.namespace == current.namespace && current.generation >= first.generation
            }
            _ => false,
        }
}

fn same_frame_watch(first: &FrameMeta, current: &FrameMeta) -> bool {
    first.watch_id().is_some()
        && first.watch_id() == current.watch_id()
        && first.window_geometry().is_some()
        && first.window_geometry() == current.window_geometry()
        && first.session_epoch == current.session_epoch
        && first.surface == current.surface
        && first.clock_domain == current.clock_domain
        && first.geometry_revision == current.geometry_revision
        && first.width_px == current.width_px
        && first.height_px == current.height_px
        && first.scale_factor == current.scale_factor
}

fn is_fresh_exact_pair_after(
    snapshot: &StructureSnapshot,
    frame: &FrameMeta,
    barrier_ms: u64,
    previous_snapshot_id: &str,
    previous_structure_sequence: u64,
) -> bool {
    frame.source_ms > barrier_ms
        && snapshot.snapshot_id.as_str() != previous_snapshot_id
        && snapshot.source_sequence > previous_structure_sequence
        && validate_exact_frame_pair(snapshot, frame).is_ok()
}

fn computer_scope_state_active(state: interaction_contracts::desktop::task::RootState) -> bool {
    matches!(
        state,
        interaction_contracts::desktop::task::RootState::Running
            | interaction_contracts::desktop::task::RootState::WaitingEvidence
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use interaction_contracts::desktop::device::SurfaceRef;
    use interaction_contracts::desktop::source::{
        FrameStatus, FrameStreamInfo, Rect, SourceIdentityBoundary,
    };
    use sim_world::app::{SimApp, SnapshotMeta};

    use crate::infrastructure::storage::Storage;
    use crate::tasks::{CreateOutcome, TaskConfig, TaskRuntime};
    use crate::testkit::EnvRig;

    fn frame(stream_id: &str, sequence: u64, source_ms: u64) -> FrameMeta {
        FrameMeta {
            frame_id: format!("frame-{sequence}"),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "native/1/monotonic".into(),
            source_sequence: sequence,
            source_ms,
            status: FrameStatus::Complete,
            geometry_revision: 1,
            width_px: 100,
            height_px: 80,
            scale_factor: 1.0,
            byte_length: 32_000,
            checksum: format!("sum-{sequence}"),
            provenance: Some(
                interaction_contracts::desktop::source::FrameProvenance::Stream {
                    stream: FrameStreamInfo {
                        stream_id: stream_id.into(),
                        image_frame_id: Some(format!("image-{sequence}")),
                        display_time_ticks: sequence,
                        display_time_ns: sequence,
                        received_uptime_ns: source_ms,
                        received_unix_ms: source_ms,
                        callback_ms: source_ms,
                        source_clock: "native-clock".into(),
                        window_id: "window-1".into(),
                        pid: 42,
                        window_frame: Rect {
                            x: 10.0,
                            y: 20.0,
                            width: 100.0,
                            height: 80.0,
                        },
                        content_rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 100.0,
                            height: 80.0,
                        },
                        content_scale: 1.0,
                        dirty_rects: vec![],
                        dropped_frames: 0,
                        capture_interval_ms: 16,
                    },
                },
            ),
        }
    }

    fn frame_paired_snapshot(
        sequence: u64,
        source_ms: u64,
        frame_ref: Option<&str>,
    ) -> StructureSnapshot {
        let frame = frame("watch-1", sequence, source_ms);
        let stream = frame.stream_info().unwrap();
        let mut snapshot = SimApp::new().snapshot(&SnapshotMeta {
            snapshot_id: format!("snapshot-{sequence}"),
            session_epoch: frame.session_epoch,
            surface: frame.surface.clone(),
            clock_domain: frame.clock_domain.clone(),
            source_sequence: sequence,
            observed_ms: source_ms,
        });
        snapshot.frame_ref = frame_ref.map(str::to_string);
        snapshot.app.pid = stream.pid;
        crate::testkit::focus_snapshot_window(&mut snapshot, stream.window_id.clone());
        for window in &mut snapshot.windows {
            window.window_id = stream.window_id.clone();
            window.frame = stream.window_frame;
        }
        for object in &mut snapshot.objects {
            object.window_id = stream.window_id.clone();
            if object.parent.is_none() {
                object.frame = stream.window_frame;
            }
        }
        snapshot
    }

    #[test]
    fn begin_scope_survives_new_reads_only_with_same_source_and_window_watch() {
        let mut rig = EnvRig::new();
        let first = rig.observe(10).snapshot;
        let mut next = rig.app.snapshot(&SnapshotMeta {
            snapshot_id: "fresh-snapshot".into(),
            session_epoch: first.session_epoch,
            surface: first.surface.clone(),
            clock_domain: first.clock_domain.clone(),
            source_sequence: first.source_sequence + 1,
            observed_ms: first.observed_ms + 10,
        });
        assert!(same_source_context(&first, &next));

        next.frame_ref = Some("paired-image-content-id".into());
        assert!(same_source_context(&first, &next));

        let mut changed_boundary = next.clone();
        changed_boundary.identity_boundary = Some(SourceIdentityBoundary {
            namespace: "11111111-1111-4111-8111-111111111111".into(),
            generation: 1,
        });
        assert!(!same_source_context(&first, &changed_boundary));

        let mut stable_boundary_first = first.clone();
        stable_boundary_first.identity_boundary = Some(SourceIdentityBoundary {
            namespace: "22222222-2222-4222-8222-222222222222".into(),
            generation: 4,
        });
        let mut stable_boundary_next = stable_boundary_first.clone();
        stable_boundary_next.source_sequence += 1;
        stable_boundary_next
            .identity_boundary
            .as_mut()
            .unwrap()
            .generation = 5;
        assert!(same_source_context(
            &stable_boundary_first,
            &stable_boundary_next
        ));
        stable_boundary_next
            .identity_boundary
            .as_mut()
            .unwrap()
            .generation = 3;
        assert!(!same_source_context(
            &stable_boundary_first,
            &stable_boundary_next
        ));
        stable_boundary_next
            .identity_boundary
            .as_mut()
            .unwrap()
            .namespace = "33333333-3333-4333-8333-333333333333".into();
        stable_boundary_next
            .identity_boundary
            .as_mut()
            .unwrap()
            .generation = 6;
        assert!(!same_source_context(
            &stable_boundary_first,
            &stable_boundary_next
        ));

        next.session_epoch += 1;
        assert!(!same_source_context(&first, &next));
    }

    #[test]
    fn fresh_frames_must_keep_the_same_native_stream_and_full_window_geometry() {
        let first = frame("watch-1", 1, 10);
        let next = frame("watch-1", 2, 20);
        assert!(same_frame_watch(&first, &next));

        let restarted = frame("watch-2", 2, 20);
        assert!(!same_frame_watch(&first, &restarted));

        let mut resized = frame("watch-1", 2, 20);
        resized.stream_info_mut().unwrap().window_frame.width += 1.0;
        assert!(!same_frame_watch(&first, &resized));
    }

    #[test]
    fn all_fresh_pixel_barriers_require_the_exact_source_image_pair() {
        let paired = frame_paired_snapshot(2, 20, Some("image-2"));
        let new_frame = frame("watch-1", 2, 20);

        assert!(is_fresh_exact_pair_after(
            &paired,
            &new_frame,
            10,
            "snapshot-1",
            1
        ));

        let missing_ref = frame_paired_snapshot(2, 20, None);
        assert!(!is_fresh_exact_pair_after(
            &missing_ref,
            &new_frame,
            10,
            "snapshot-1",
            1
        ));

        // A newer tree that retains the old image's content ID still cannot
        // advance any fresh-pixel barrier.
        let old_image_ref = frame_paired_snapshot(2, 20, Some("image-1"));
        assert!(!is_fresh_exact_pair_after(
            &old_image_ref,
            &new_frame,
            10,
            "snapshot-1",
            1
        ));

        assert!(!is_fresh_exact_pair_after(
            &paired,
            &new_frame,
            20,
            "snapshot-1",
            1
        ));
        assert!(!is_fresh_exact_pair_after(
            &paired,
            &new_frame,
            10,
            "snapshot-2",
            1
        ));
    }

    #[test]
    fn running_scope_survives_only_the_attempts_normal_evidence_wait() {
        use interaction_contracts::desktop::task::RootState;

        assert!(computer_scope_state_active(RootState::Running));
        assert!(computer_scope_state_active(RootState::WaitingEvidence));
        for state in [
            RootState::Ready,
            RootState::WaitingPlan,
            RootState::Pausing,
            RootState::Suspended,
            RootState::NeedsInput,
            RootState::Cancelling,
            RootState::Completed,
            RootState::Failed,
        ] {
            assert!(!computer_scope_state_active(state), "{state:?}");
        }
    }

    #[test]
    fn per_root_model_call_limit_counts_durable_rows() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut tasks = TaskRuntime::new(TaskConfig::new(vec![]));
        let CreateOutcome::Started(root) = tasks
            .create_root(&mut storage, 0, "bounded computer calls")
            .unwrap()
        else {
            panic!("the test root should be created");
        };
        storage
            .transaction(|tx| {
                tx.execute(
                    "UPDATE root_tasks SET root_epoch=1,plan_revision=1,plan_json='{}',state='running' WHERE root_id=?1",
                    [root.as_str()],
                )?;
                for generation in 1..=u64::from(MAX_CALLS_PER_ROOT) {
                    tx.execute(
                        "INSERT INTO computer_model_calls(
                            root_id,request_id,generation,task_revision,root_epoch,snapshot_id,
                            structure_sequence,deadline_ms,plan_revision,subgoal_id,created_ms,
                            stage,provider,model,started_ms,usage_json
                         ) VALUES (?1,?2,?3,1,1,'snapshot-1',1,100,1,'subgoal-1',1,
                                   'begin','openai','test',1,'{}')",
                        rusqlite::params![
                            root.as_str(),
                            format!("call-{generation}"),
                            generation as i64,
                        ],
                    )?;
                }
                Ok::<(), crate::infrastructure::storage::StoreError>(())
            })
            .unwrap();

        let engine = Engine::new(
            super::super::EngineConfig::new(TaskConfig::new(vec![])),
            storage,
        );
        assert_eq!(
            engine.durable_computer_call_count(&root).unwrap(),
            MAX_CALLS_PER_ROOT
        );
    }
}
