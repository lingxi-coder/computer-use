//! One attempt, from admission to its end (design 11.2, 11.3, 14.3 and 15).
//!
//! Admission is the only way an attempt starts: the effect gate is checked
//! again, then TaskRuntime records quota, attempt and the decision's
//! consumption in one transaction. After that a skill asks for bounded inputs
//! and each one travels the same way: current evidence to the device, a permit
//! on that evidence, then the intent. A receipt says what reached the
//! application; it is never taken as proof of the effect.

use interaction_contracts::desktop::decision::{Candidate, DecisionSource};
use interaction_contracts::desktop::device::{
    CommandAck, GrantOutcome, QueryReceipt, Receipt, ReceiptStatus, SourceFreshness,
};
use interaction_contracts::desktop::effect::Admission;
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::ids::{AttemptId, DecisionId, ObjectRef};
use interaction_contracts::desktop::operator::ActionSummary;
use interaction_contracts::desktop::task::{
    AdmitAttempt, AdmitOutcome, AdmitRejection, AttemptQuota, AttemptState,
};

use super::{Engine, EngineError, Step};
use crate::environment::binding::Binding;
use crate::environment::state_core::EnvSnapshot;
use crate::execution::{DeviceCommand, Dispatch, Expect, IntentSpec, Stage};
use crate::infrastructure::storage::StoreError;
use crate::ports::DecisionLifecycle;
use crate::reaction::candidates::{Resolution, Scope, gate};
use crate::reaction::decisions::Held;
use crate::reaction::skills::{
    ActionOutcome, ActionPlan, Skill, SkillResult, SkillRun, SkillStep, SkillView,
};
use crate::reaction::store::ReactionStore;
use crate::tasks::TaskError;
use crate::tasks::budget::BudgetKind;
use crate::tasks::store::TaskStore;

/// The attempt that is running now.
pub(crate) struct AttemptRun {
    pub id: AttemptId,
    pub subgoal: String,
    pub skill: SkillRun,
    /// It acts on the goal's own target.
    pub goal_target: bool,
    pub quota: AttemptQuota,
    pub observations_used: u32,
    pub dispatch: Option<Dispatch>,
    pub sequence: u32,
    /// Input may have reached the application, so the quota is spent.
    pub input_reached: bool,
    pub deadline_ms: u64,
}

impl Engine {
    // ---- Admission ----------------------------------------------------------------------

    /// Take a chosen candidate through the gate and the admission transaction.
    pub(super) fn admit_candidate(
        &mut self,
        decision: DecisionId,
        generation: u64,
        candidate: Candidate,
        source: DecisionSource,
        confirmed: bool,
    ) -> Step {
        let Some(skill) = Skill::from_name(&candidate.skill) else {
            return self.drop_decision(&decision, "the candidate's skill has no runtime");
        };
        let Some(env) = self.state.latest().cloned() else {
            return self.drop_decision(&decision, "there is no evidence to admit against");
        };
        let Some(binding) = self.binding_for(&candidate, &env) else {
            return self.drop_decision(&decision, "the candidate's target is no longer bound");
        };
        let admission = {
            let run = self.run.as_ref().expect("a running root has a run");
            gate(
                &Scope {
                    goal: &run.goal,
                    plan_targets: &run.plan_targets,
                    pack: &run.pack,
                    env: &env,
                },
                candidate.effect_class,
                candidate.semantic_target.as_ref(),
                confirmed,
            )
        };
        match admission {
            Admission::Allow => {}
            Admission::NeedsConfirmation if source == DecisionSource::Plan => {
                self.fail_root("a plan step needs a confirmation the goal did not give")?;
                return Ok(true);
            }
            Admission::NeedsConfirmation => {
                return self.ask_confirmation(decision, candidate, source);
            }
            Admission::Deny(reason) => {
                return self.drop_decision(
                    &decision,
                    &format!("the effect gate refused it: {reason:?}"),
                );
            }
        }

        let (request, payload) = {
            let active = self.tasks.active().expect("a running root is active");
            let seq = TaskStore::next_attempt_seq(self.storage.read())?;
            let attempt_id = AttemptId::new(format!("att-{seq}")).expect("a counter is a valid id");
            let root_deadline = active.row.deadline_ms.unwrap_or(self.now_ms);
            let deadline = self
                .now_ms
                .saturating_add(self.cfg.attempt_window_ms)
                .min(root_deadline);
            let request = AdmitAttempt {
                attempt_id,
                decision_id: decision.clone(),
                generation,
                root_task_id: active.row.root_id.clone(),
                root_epoch: active.row.root_epoch,
                task_revision: active.row.task_revision,
                plan_revision: active.row.plan_revision,
                subgoal: match self.decisions.pending() {
                    // A step decision names its sub-goal; a dialog decision has none.
                    Some(pending) => pending
                        .subgoal
                        .clone()
                        .unwrap_or_else(|| "layer".to_string()),
                    // A plan's own decision is for the step that is next.
                    None => self
                        .next_subgoal()
                        .map_or_else(|| "direct".to_string(), |subgoal| subgoal.id),
                },
                skill: skill.name().to_string(),
                quota: skill.quota(),
                deadline_ms: deadline,
                effect_class: candidate.effect_class,
                source,
            };
            // What the device has to be told once the attempt is active. It is
            // recorded with the admission, so after a crash the delivery can be
            // reconciled with the device instead of repeated.
            let payload = serde_json::json!({
                "attempt": request.attempt_id,
                "skill": request.skill,
                "binding": binding.binding_ref,
                "target": binding.semantic_target,
            })
            .to_string();
            (request, payload)
        };

        match self.tasks.admit_attempt(
            &mut self.storage,
            &ReactionStore,
            &request,
            self.now_ms,
            &payload,
        ) {
            Ok(AdmitOutcome::Activated { activation }) => {
                self.decisions.consumed();
                self.start_attempt(&request, &activation.granted, skill, &binding);
                self.note(format!(
                    "{} admitted as {} ({source:?})",
                    skill.name(),
                    request.attempt_id
                ));
                Ok(true)
            }
            Ok(AdmitOutcome::Rejected { reason }) => {
                self.note(format!("admission refused: {reason:?}"));
                self.close_decision_row(&decision)?;
                match reason {
                    AdmitRejection::BudgetExhausted => {
                        self.fail_root("the attempt budget is exhausted")?;
                    }
                    AdmitRejection::EffectNotAuthorized => {
                        self.fail_root("the action is not authorised by the goal")?;
                    }
                    // The situation moved on; the next round decides again.
                    _ => {}
                }
                Ok(true)
            }
            // A failed write rolled everything back. Nothing was recorded, so
            // asking again is safe; waiting a little keeps it from spinning.
            Err(TaskError::Store(StoreError::Fault(point))) => {
                self.note(format!("admission failed at {point}; nothing was recorded"));
                self.close_decision_row(&decision)?;
                let resume = self.now_ms + self.cfg.decision_backoff_ms;
                if let Some(run) = self.run.as_mut() {
                    run.decision_resume_ms = resume;
                }
                Ok(true)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn start_attempt(
        &mut self,
        request: &AdmitAttempt,
        granted: &AttemptQuota,
        skill: Skill,
        binding: &Binding,
    ) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let goal_target = binding.semantic_target == run.goal.target.semantic_target;
        run.last_attempt = Some((request.attempt_id.clone(), None));
        run.decision_failures = 0;
        run.attempt = Some(AttemptRun {
            id: request.attempt_id.clone(),
            subgoal: request.subgoal.clone(),
            skill: SkillRun::new(
                skill,
                binding.semantic_target.clone(),
                Some(run.goal.desired.clone()),
                binding.binding_ref.clone(),
            ),
            goal_target,
            quota: *granted,
            observations_used: 0,
            dispatch: None,
            sequence: 0,
            input_reached: false,
            deadline_ms: request.deadline_ms,
        });
    }

    /// The binding the skill starts with: the one the candidate named, or, for
    /// a pick among same-named controls, the one that pick makes.
    fn binding_for(&mut self, candidate: &Candidate, env: &EnvSnapshot) -> Option<Binding> {
        if let Some(choice) = candidate.parameters.get("choice") {
            let instance = ObjectRef::new(choice.clone()).ok()?;
            let group = env
                .snapshot
                .objects
                .iter()
                .find(|object| object.instance_ref == instance)?
                .group
                .clone();
            let run = self.run.as_mut()?;
            let target = run.goal.target.semantic_target.clone();
            // The pick names one instance now. The group it sits in is what
            // identifies it later, because instance references do not last.
            run.chosen = Some(instance);
            run.refined_group = group.or_else(|| run.refined_group.clone());
            self.bindings
                .invalidate(&target, "a choice was made among same-named controls");
            let description = self.goal_description();
            return match self.resolve(&target, description, env) {
                Resolution::Bound(binding) => Some(binding),
                _ => None,
            };
        }
        let reference = candidate.binding.as_ref()?;
        self.bindings
            .all()
            .find(|binding| &binding.binding_ref == reference)
            .cloned()
    }

    /// The action needs the user's word first. The root waits with its input
    /// released, and the answer covers this one action only.
    fn ask_confirmation(
        &mut self,
        decision: DecisionId,
        candidate: Candidate,
        source: DecisionSource,
    ) -> Step {
        let label = candidate
            .parameters
            .get("label")
            .cloned()
            .unwrap_or_else(|| candidate.skill.clone());
        let summary = ActionSummary {
            skill: candidate.skill.clone(),
            target_label: label,
            description: candidate.description.clone(),
            effect_class: candidate.effect_class,
        };
        let class = candidate.effect_class;
        self.decisions.hold(Held {
            candidate,
            source,
            backend: String::new(),
        });
        if self.decisions.held().is_none() {
            return self.drop_decision(&decision, "there is no open decision to hold");
        }
        self.tasks.request_effect_confirmation(
            &mut self.storage,
            self.now_ms,
            decision,
            summary,
        )?;
        self.note(format!(
            "waiting for the user to confirm a {class:?} action"
        ));
        Ok(true)
    }

    /// Drop a decision that cannot be used, and wait a moment before asking again.
    fn drop_decision(&mut self, decision: &DecisionId, reason: &str) -> Step {
        self.note(format!("decision {decision} was not used: {reason}"));
        self.close_decision_row(decision)?;
        let resume = self.now_ms + self.cfg.decision_backoff_ms / 10;
        if let Some(run) = self.run.as_mut() {
            run.decision_resume_ms = resume;
        }
        Ok(true)
    }

    fn close_decision_row(&mut self, decision: &DecisionId) -> Result<(), EngineError> {
        if self
            .decisions
            .pending()
            .is_some_and(|pending| pending.decision_id() == decision)
        {
            if let Some(cancel) = self
                .decisions
                .close(&mut self.storage, DecisionLifecycle::Superseded)?
            {
                self.emit(super::Output::CancelDecision(cancel));
            }
        } else {
            self.storage.transaction::<_, StoreError>(|tx| {
                ReactionStore::close(tx, decision, DecisionLifecycle::Superseded).map(|_| ())
            })?;
        }
        Ok(())
    }

    // ---- Driving a skill ---------------------------------------------------------------------------

    pub(super) fn drive_attempt(&mut self) -> Step {
        let now = self.now_ms;
        let Some(attempt) = self.run.as_ref().and_then(|run| run.attempt.as_ref()) else {
            return Ok(false);
        };
        if attempt.dispatch.is_some() {
            return Ok(false);
        }
        if now > attempt.deadline_ms {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::EvidenceExpired,
                reason: "the attempt ran out of time".into(),
            })?;
            return Ok(true);
        }
        // Permits rest on dependency revisions; wait until the device has told
        // us the new ones before building anything on them.
        if self
            .device
            .is_waiting(|expect| matches!(expect, Expect::Dependency))
        {
            return Ok(false);
        }
        // A device without the host's lease refuses everything; wait for it.
        if !self.device.lease_established() {
            return Ok(false);
        }
        if !self.evidence_usable() {
            if self.run.as_ref().is_some_and(|run| run.wait.is_some()) {
                return Ok(false);
            }
            return self.request_refresh(now, "evidence is too old to act on");
        }
        let step = {
            let Engine {
                state,
                bindings,
                run,
                ..
            } = self;
            let Some(run) = run.as_mut() else {
                return Ok(false);
            };
            let Some(attempt) = run.attempt.as_mut() else {
                return Ok(false);
            };
            attempt.skill.advance(&mut SkillView {
                state,
                bindings,
                pack: &run.pack,
            })
        };
        match step {
            SkillStep::Act(plan) => self.start_dispatch(*plan),
            SkillStep::AwaitEvidence { after_ms } => self.attempt_await(after_ms),
            SkillStep::Done(result) => {
                self.finish_attempt(result)?;
                Ok(true)
            }
        }
    }

    fn attempt_await(&mut self, after_ms: u64) -> Step {
        if self.state.latest_since(after_ms).is_some() {
            // It arrived between the skill's look and now.
            return Ok(true);
        }
        let exhausted = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| attempt.observations_used >= attempt.quota.observations);
        let already_waiting = self.run.as_ref().is_some_and(|run| run.wait.is_some());
        if exhausted && !already_waiting {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::BudgetExhausted,
                reason: "the attempt used all the observations it was given".into(),
            })?;
            return Ok(true);
        }
        let sent = self.request_refresh(after_ms, "evidence from after the input")?;
        if sent && let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.observations_used += 1;
        }
        Ok(sent)
    }

    // ---- The way to the device ---------------------------------------------------------------------------

    fn start_dispatch(&mut self, plan: ActionPlan) -> Step {
        let Some(env) = self.state.latest().cloned() else {
            return self.input_never_left(FailureCode::SourceInvalid, "there is no evidence");
        };
        let root_deadline = self
            .tasks
            .active()
            .and_then(|active| active.row.deadline_ms)
            .unwrap_or(self.now_ms);
        let freshness = if env.snapshot.complete
            && env.snapshot.active_layer.is_some()
            && !self.state.structure_unavailable()
        {
            SourceFreshness::Verified
        } else {
            SourceFreshness::Uncertain
        };
        let observation = match self.device.observation(
            &env.snapshot,
            plan.binding.binding_ref.as_str(),
            freshness,
            self.cfg.evidence_ttl_ms,
            root_deadline,
        ) {
            Ok(observation) => observation,
            Err(why) => {
                self.note(format!("the evidence cannot be given to the device: {why}"));
                return self.input_never_left(FailureCode::SourceInvalid, why);
            }
        };
        self.send(Expect::Observe, DeviceCommand::Observe(observation.clone()));
        let now = self.now_ms;
        if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.dispatch = Some(Dispatch {
                attempt: attempt.id.clone(),
                plan,
                observation,
                permit: None,
                intent_id: None,
                stage: Stage::Observing,
                started_ms: now,
            });
        }
        Ok(true)
    }

    /// The skill asked for an input that could not even be sent.
    fn input_never_left(&mut self, code: FailureCode, reason: &str) -> Step {
        self.note(format!("no input was sent: {reason}"));
        if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.skill.acted(ActionOutcome::Rejected(Some(code)));
        }
        Ok(true)
    }

    /// The device answered the evidence.
    pub(super) fn observed(&mut self, ack: CommandAck) -> Result<(), EngineError> {
        let Some((root, observation)) = self
            .dispatch_in(Stage::Observing)
            .map(|(root, d)| (root, d.observation.clone()))
        else {
            return Ok(());
        };
        if !ack.accepted {
            self.abort_dispatch(
                ack.code,
                ack.reason
                    .as_deref()
                    .unwrap_or("the device refused the evidence"),
            );
            return Ok(());
        }
        let valid_until = observation.valid_until_ms;
        let grant = self.device.grant(&root, &observation, valid_until);
        if let Some(dispatch) = self.dispatch_mut() {
            dispatch.stage = Stage::Granting;
        }
        self.send(Expect::Grant, DeviceCommand::GrantPermit(grant));
        Ok(())
    }

    /// The device decided on the permit.
    pub(super) fn granted(&mut self, outcome: GrantOutcome) -> Result<(), EngineError> {
        let Some((root, dispatch)) = self
            .dispatch_in(Stage::Granting)
            .map(|(root, d)| (root, d.clone()))
        else {
            return Ok(());
        };
        let permit = match outcome {
            GrantOutcome::Granted { permit } => permit,
            GrantOutcome::Denied { code, reason } => {
                self.abort_dispatch(code, &reason);
                return Ok(());
            }
        };
        let (task_revision, root_epoch) = {
            let active = self.tasks.active().expect("a running root is active");
            (active.row.task_revision, active.row.root_epoch)
        };
        let sequence = {
            let attempt = self
                .run
                .as_mut()
                .and_then(|run| run.attempt.as_mut())
                .expect("a dispatch belongs to an attempt");
            attempt.sequence += 1;
            attempt.sequence
        };
        let submit = match self.device.intent(&IntentSpec {
            attempt: &dispatch.attempt,
            root: &root,
            task_revision,
            root_epoch,
            permit: &permit,
            observation: &dispatch.observation,
            plan: &dispatch.plan,
            sequence,
            now_ms: self.now_ms,
            start_slack_ms: self.cfg.start_slack_ms,
            duration_ms: self.cfg.action_duration_ms,
        }) {
            Ok(submit) => submit,
            Err(why) => {
                self.abort_dispatch(Some(FailureCode::SourceInvalid), why);
                return Ok(());
            }
        };
        let intent_id = submit.intent.intent_id.clone();
        if let Some(run) = self.run.as_mut() {
            run.permits.push(permit.clone());
            if let Some((_, intent)) = run.last_attempt.as_mut() {
                *intent = Some(intent_id.clone());
            }
        }
        if let Some(dispatch) = self.dispatch_mut() {
            dispatch.permit = Some(permit);
            dispatch.intent_id = Some(intent_id.clone());
            dispatch.stage = Stage::Submitting;
        }
        self.send(Expect::Submit { intent_id }, DeviceCommand::Submit(submit));
        Ok(())
    }

    /// The dispatch of the current attempt, if it is at `stage`.
    fn dispatch_in(
        &self,
        stage: Stage,
    ) -> Option<(interaction_contracts::desktop::ids::RootTaskId, &Dispatch)> {
        let run = self.run.as_ref()?;
        let dispatch = run.attempt.as_ref()?.dispatch.as_ref()?;
        (dispatch.stage == stage).then(|| (run.goal.root_task_id.clone(), dispatch))
    }

    fn dispatch_mut(&mut self) -> Option<&mut Dispatch> {
        self.run.as_mut()?.attempt.as_mut()?.dispatch.as_mut()
    }

    /// The device refused before any input was submitted.
    pub(super) fn abort_dispatch(&mut self, code: Option<FailureCode>, reason: &str) {
        self.note(format!("the device refused the input: {reason}"));
        let now = self.now_ms;
        if let Some(run) = self.run.as_mut() {
            // A stop the host has not processed yet: the answer is to wait for
            // it, not to count a failure.
            if code == Some(FailureCode::StopLatched) {
                run.stop_wait_since.get_or_insert(now);
            }
            if let Some(attempt) = run.attempt.as_mut() {
                attempt.dispatch = None;
                attempt.skill.acted(ActionOutcome::Rejected(code));
            }
        }
    }

    /// A receipt, whether it answers a submission or arrives on its own.
    pub(super) fn on_receipt(&mut self, receipt: Receipt) -> Result<(), EngineError> {
        // Input that reached the application counts as input whenever its
        // receipt arrives, even after the attempt stopped waiting for it.
        self.record_receipt_input(&receipt);
        let matches_dispatch = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
            .is_some_and(|dispatch| {
                dispatch.intent_id.as_deref() == Some(receipt.intent_id.as_str())
            });
        if !matches_dispatch {
            return Ok(());
        }
        let (attempt_id, running) = {
            let attempt = self
                .run
                .as_ref()
                .and_then(|run| run.attempt.as_ref())
                .expect("checked above");
            (
                attempt.id.clone(),
                attempt
                    .dispatch
                    .as_ref()
                    .is_some_and(|dispatch| dispatch.stage == Stage::Running),
            )
        };
        let terminal = !matches!(
            receipt.status,
            ReceiptStatus::Accepted | ReceiptStatus::Running
        );
        if !running {
            // The device has the intent: the attempt is dispatched, durably.
            self.tasks
                .attempt_dispatched(&mut self.storage, &attempt_id, &receipt.intent_id)?;
            if let Some(dispatch) = self.dispatch_mut() {
                dispatch.stage = Stage::Running;
            }
        }
        if !terminal {
            return Ok(());
        }
        let moved = receipt.submitted.iter().any(|submitted| !submitted.cleanup);
        let end_ms = receipt.command_end_ms.unwrap_or(receipt.at_ms);
        let outcome = match receipt.status {
            ReceiptStatus::Completed => ActionOutcome::Completed { end_ms },
            ReceiptStatus::Partial | ReceiptStatus::Unknown => ActionOutcome::Unknown {
                at_ms: receipt.at_ms,
            },
            ReceiptStatus::Cancelled if moved => ActionOutcome::Unknown {
                at_ms: receipt.at_ms,
            },
            ReceiptStatus::Cancelled => ActionOutcome::Cancelled,
            ReceiptStatus::Rejected => {
                if receipt.reason_code == Some(FailureCode::StopLatched) {
                    let now = self.now_ms;
                    if let Some(run) = self.run.as_mut() {
                        run.stop_wait_since.get_or_insert(now);
                    }
                }
                ActionOutcome::Rejected(receipt.reason_code)
            }
            ReceiptStatus::Accepted | ReceiptStatus::Running => return Ok(()),
        };
        if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.input_reached |= moved || matches!(outcome, ActionOutcome::Unknown { .. });
            attempt.dispatch = None;
            attempt.skill.acted(outcome);
        }
        Ok(())
    }

    /// Tell Verification when input actually reached the application, so
    /// evidence from before it can never confirm its effect.
    fn record_receipt_input(&mut self, receipt: &Receipt) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let ours = run
            .last_attempt
            .as_ref()
            .is_some_and(|(attempt, _)| receipt.attempt_id.as_deref() == Some(attempt.as_str()));
        if !ours {
            return;
        }
        let mut times = receipt
            .submitted
            .iter()
            .filter(|submitted| !submitted.cleanup)
            .map(|submitted| submitted.at_ms);
        let Some(first) = times.next() else {
            return;
        };
        let last = times.next_back().unwrap_or(first);
        let end = receipt.command_end_ms.unwrap_or(last).max(last);
        run.verification.record_input(first, end);
    }

    // ---- The end of an attempt ------------------------------------------------------------------------------

    pub(super) fn finish_attempt(&mut self, result: SkillResult) -> Result<(), EngineError> {
        let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.take()) else {
            return Ok(());
        };
        let now = self.now_ms;
        let name = attempt.skill.skill().name();
        let input = attempt.input_reached;
        match result {
            SkillResult::AlreadyDone | SkillResult::Succeeded => {
                let already = matches!(result, SkillResult::AlreadyDone);
                self.tasks.attempt_ended(
                    &mut self.storage,
                    &attempt.id,
                    AttemptState::Settled,
                    !input,
                )?;
                if let Some(run) = self.run.as_mut() {
                    run.done.insert(attempt.subgoal.clone());
                    run.failures = 0;
                    run.stalls = 0;
                    run.recent.push(format!(
                        "{name}: {}",
                        if already { "already satisfied" } else { "done" }
                    ));
                    // The change itself is done; from here leaving the page counts.
                    if attempt.goal_target && attempt.skill.skill() == Skill::EnsureValue {
                        run.verification.visits.arm(now);
                    }
                }
                self.note(format!(
                    "{name} {}",
                    if already {
                        "found the value already right"
                    } else {
                        "succeeded"
                    }
                ));
                Ok(())
            }
            SkillResult::Failed { code, reason } => {
                let state = match code {
                    FailureCode::EffectUnknown => AttemptState::Unknown,
                    FailureCode::Cancelled => AttemptState::Cancelled,
                    _ => AttemptState::Settled,
                };
                self.tasks
                    .attempt_ended(&mut self.storage, &attempt.id, state, !input)?;
                if let Some(run) = self.run.as_mut() {
                    run.recent
                        .push(format!("{name}: {} ({})", code.design_name(), reason));
                }
                self.note(format!("{name} failed: {} — {reason}", code.design_name()));
                self.route_failure(code)
            }
        }
    }

    /// What a failure means for the root. The codes are not one thing: each
    /// drives its own path, and none of them is "low confidence".
    fn route_failure(&mut self, code: FailureCode) -> Result<(), EngineError> {
        match code {
            // The device holds a stop the host has not processed yet. Waiting for
            // it is the answer; nothing was tried and nothing is spent.
            FailureCode::StopLatched => {
                self.note("a stop is latched at the device; waiting for the host to process it");
                Ok(())
            }
            // The stop and pause paths own these.
            FailureCode::Cancelled | FailureCode::HumanInputDetected => Ok(()),
            FailureCode::BudgetExhausted | FailureCode::EffectNotAuthorized => {
                self.fail_root(code.design_name())
            }
            // Everything else is recoverable once, within the budget, from
            // fresh evidence.
            _ => {
                if !self
                    .tasks
                    .spend(&mut self.storage, BudgetKind::Recovery, 1)?
                {
                    return self.fail_root("the recovery budget is exhausted");
                }
                if let Some(run) = self.run.as_mut() {
                    run.failures += 1;
                }
                let now = self.now_ms;
                self.request_refresh(now, "recovering from a failed attempt")?;
                Ok(())
            }
        }
    }

    /// The attempt is abandoned because the root is stopping or ending.
    pub(super) fn end_attempt_cancelled(&mut self) -> Result<(), EngineError> {
        let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.take()) else {
            return Ok(());
        };
        // An input that may already be on its way is unknown, not cancelled.
        let sent = attempt
            .dispatch
            .as_ref()
            .is_some_and(|dispatch| matches!(dispatch.stage, Stage::Submitting | Stage::Running));
        let state = if sent || attempt.input_reached {
            AttemptState::Unknown
        } else {
            AttemptState::Cancelled
        };
        self.tasks.attempt_ended(
            &mut self.storage,
            &attempt.id,
            state,
            !(sent || attempt.input_reached),
        )?;
        Ok(())
    }

    pub(super) fn query_receipt(&mut self, intent_id: &str) {
        if let Some(session) = self.device.session_epoch() {
            self.send(
                Expect::Query {
                    intent_id: intent_id.to_string(),
                },
                DeviceCommand::QueryReceipt(QueryReceipt {
                    session_epoch: session,
                    intent_id: intent_id.to_string(),
                }),
            );
        }
    }
}
