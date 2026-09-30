//! A running root: what to do next, and who decides (design 13 and 20.1).
//!
//! Each round the engine evaluates the frozen goal over the evidence, and if
//! it does not hold picks the next plan step. A step with one clear way to run
//! goes straight to admission as the plan's own decision. A step with a real
//! branch — several same-named controls, a dialog nobody planned for — is put
//! to a decision backend as a choice among candidates that were already
//! allowed. Either way it reaches the device only through the one admission.

use interaction_contracts::desktop::binding::{
    BindRequest, BindScope, BindingOutcome, TargetDescription,
};
use interaction_contracts::desktop::decision::{DecisionFailure, DecisionSource, Precondition};
use interaction_contracts::desktop::ids::{RequestId, SemanticTargetRef};
use interaction_contracts::desktop::operator::RunMode;
use interaction_contracts::desktop::planning::Subgoal;
use interaction_contracts::desktop::task::RootState;
use interaction_contracts::desktop::verification::TriState;

use super::{Engine, EngineError, Output, Step};
use crate::environment::state_core::EnvSnapshot;
use crate::execution::{DeviceCommand, Expect};
use crate::ports::DecisionLifecycle;
use crate::reaction::candidates::{
    Basis, Resolution, Scope, bind_layer_controls, build, layer_title, modal_in_front,
};
use crate::reaction::compiler::{CompileFailure, CompileInput, compile};
use crate::reaction::decisions::{Control, DecisionBasis, Dropped, OpenSpec, Verdict};
use crate::verification::{EvidenceContext, Verdict as GoalVerdict};

enum Verified {
    Completed,
    Pending(Box<GoalVerdict>),
}

impl Engine {
    /// The situation a decision is made in, when there is evidence.
    pub(super) fn current_basis(&self) -> Option<DecisionBasis> {
        let env = self.state.latest()?;
        let active = self.tasks.active()?;
        Some(DecisionBasis::capture(
            env,
            &self.bindings,
            active.row.plan_revision,
            active.row.task_revision,
            active.row.root_epoch,
        ))
    }

    // ---- Getting started -------------------------------------------------------------------

    /// READY to RUNNING needs the operator's mode, a device that can take
    /// input, the root registered with it, and current evidence.
    pub(super) fn step_ready(&mut self) -> Step {
        if self.mode != RunMode::Execute || self.run.is_none() {
            return Ok(false);
        }
        if !self.device.input_available() || self.device.behind_on_stops() {
            return Ok(false);
        }
        let (registered, registering) = {
            let run = self.run.as_ref().expect("checked above");
            (run.registered, run.registering)
        };
        if !registered {
            if registering {
                return Ok(false);
            }
            let grant = {
                let run = self.run.as_mut().expect("checked above");
                run.registering = true;
                run.grant.clone()
            };
            self.send(Expect::Register, DeviceCommand::RegisterRoot(grant));
            return Ok(true);
        }
        // Registered: only current evidence is missing.
        if !self.evidence_usable() {
            if self.run.as_ref().is_some_and(|run| run.wait.is_some()) {
                return Ok(false);
            }
            let now = self.now_ms;
            return self.request_refresh(now, "the task needs current evidence to start");
        }
        if let Some(run) = self.run.as_mut() {
            run.wait = None;
        }
        self.tasks
            .admission_passed(&mut self.storage, self.now_ms)?;
        self.note("the task is running");
        Ok(true)
    }

    // ---- One round of a running root -------------------------------------------------------------

    pub(super) fn step_running(&mut self) -> Step {
        if self.run.is_none() {
            return Ok(false);
        }
        if self.mode != RunMode::Execute {
            return Ok(false);
        }
        if self.run.as_ref().is_some_and(|run| run.attempt.is_some()) {
            return self.drive_attempt();
        }
        // The device holds a stop the host has not processed yet. Every request
        // would be refused, so none is made until the stop has been handled.
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.stop_wait_since.is_some())
        {
            return Ok(false);
        }
        if self.decisions.pending().is_some() || self.planning.inflight().is_some() {
            return Ok(false);
        }
        if self
            .run
            .as_ref()
            .is_some_and(|run| self.now_ms < run.decision_resume_ms)
        {
            return Ok(false);
        }
        if !self.evidence_usable() {
            if self.run.as_ref().is_some_and(|run| run.wait.is_some()) {
                return Ok(false);
            }
            let now = self.now_ms;
            return self.request_refresh(now, "there is no current evidence");
        }
        // The goal can only be read through its binding, so that comes first:
        // a baseline taken before the target is bound would be unknown, and so
        // would be the conclusion after a page was entered again.
        self.maintain_goal_binding();
        let verdict = match self.verify()? {
            Verified::Completed => return Ok(true),
            Verified::Pending(verdict) => *verdict,
        };
        self.choose_next(verdict)
    }

    /// Bind the goal's own target as soon as it can be, and again whenever a
    /// page is entered anew. Whatever the bind answers besides "bound" is
    /// dealt with when a step needs the target.
    fn maintain_goal_binding(&mut self) {
        let Some(env) = self.state.latest().cloned() else {
            return;
        };
        if modal_in_front(&env) || self.run.is_none() {
            return;
        }
        let target = self
            .run
            .as_ref()
            .map(|run| run.goal.target.semantic_target.clone())
            .expect("checked above");
        let description = self.goal_description();
        self.resolve(&target, description, &env);
    }

    /// Evaluate the frozen goal. Only here, and only when every required
    /// predicate is true, does the root complete.
    fn verify(&mut self) -> Result<Verified, EngineError> {
        let now = self.now_ms;
        let Engine {
            state,
            bindings,
            run,
            storage,
            tasks,
            ..
        } = self;
        let run = run.as_mut().expect("a running root has a run");
        let ctx = EvidenceContext {
            state,
            bindings,
            pack: &run.pack,
            goal: &run.goal,
        };
        if run.verification.last_input_end_ms().is_none() {
            let status = run.verification.evaluate(&run.goal.required, &ctx);
            run.verification.note_baseline(status);
        }
        let verdict = run.verification.verdict(&ctx);
        let root = run.goal.root_task_id.clone();
        let last = run.last_attempt.clone();
        run.verification.conclude(
            storage,
            &root,
            &ctx,
            &verdict,
            last.as_ref()
                .map(|(attempt, intent)| (attempt, intent.as_deref())),
            now,
        )?;
        if verdict.status == TriState::True {
            tasks.predicates_satisfied(storage, now)?;
            self.note("every required predicate holds: the goal is satisfied");
            return Ok(Verified::Completed);
        }
        Ok(Verified::Pending(Box::new(verdict)))
    }

    fn choose_next(&mut self, verdict: GoalVerdict) -> Step {
        let Some(env) = self.state.latest().cloned() else {
            return Ok(false);
        };
        if modal_in_front(&env) {
            return self.decide_layer(&env);
        }
        let Some(subgoal) = self.next_subgoal() else {
            return self.plan_exhausted(&verdict);
        };
        self.decide_step(&subgoal, &env)
    }

    /// The first sub-goal whose dependencies are done and which is not.
    pub(super) fn next_subgoal(&self) -> Option<Subgoal> {
        let run = self.run.as_ref()?;
        let plan = self.tasks.active()?.row.plan.clone()?;
        plan.subgoals
            .iter()
            .find(|subgoal| {
                !run.done.contains(&subgoal.id)
                    && plan
                        .dependencies
                        .iter()
                        .filter(|dependency| dependency.after == subgoal.id)
                        .all(|dependency| run.done.contains(&dependency.before))
            })
            .cloned()
    }

    pub(super) fn goal_description(&self) -> TargetDescription {
        let run = self.run.as_ref().expect("a running root has a run");
        TargetDescription {
            label: run.goal.target.label.clone(),
            group: run
                .refined_group
                .clone()
                .or_else(|| run.goal.target.group.clone()),
            role_hint: run.goal.target.role_hint.clone(),
        }
    }

    /// How a step's target is found again in the current evidence.
    fn describe_target(&self, target: &SemanticTargetRef) -> TargetDescription {
        let run = self.run.as_ref().expect("a running root has a run");
        if target == &run.goal.target.semantic_target {
            return self.goal_description();
        }
        match run.pack.control(target) {
            Some(control) => TargetDescription {
                label: control.label.clone(),
                group: None,
                role_hint: Some(control.role.clone()),
            },
            None => TargetDescription {
                label: target.as_str().replace(['-', '_', '/'], " "),
                group: None,
                role_hint: None,
            },
        }
    }

    /// Bind a target to a live instance, reusing the binding that still holds.
    pub(super) fn resolve(
        &mut self,
        target: &SemanticTargetRef,
        description: TargetDescription,
        env: &EnvSnapshot,
    ) -> Resolution {
        if let Some(binding) = self.bindings.current(target) {
            return Resolution::Bound(binding.clone());
        }
        let now = self.now_ms;
        let max_age = self.cfg.evidence_max_age_ms;
        let Some(run) = self.run.as_mut() else {
            return Resolution::Unavailable("there is no running task".into());
        };
        run.bind_seq += 1;
        let is_goal_target = target == &run.goal.target.semantic_target;
        let request = BindRequest {
            request_id: RequestId::new(format!("bind-req-{}", run.bind_seq))
                .expect("a counter is a valid id"),
            semantic_target: target.clone(),
            description,
            scope: BindScope {
                app: run.goal.scope.app.clone(),
                account: run.goal.scope.account.clone(),
            },
            choice: if is_goal_target {
                run.chosen.take()
            } else {
                None
            },
            max_age_ms: max_age,
        };
        let result = self.bindings.bind(env, &request, now);
        match result.outcome {
            BindingOutcome::Bound { .. } => {
                let binding = self
                    .bindings
                    .current(target)
                    .cloned()
                    .expect("a bound target has a binding");
                if is_goal_target && let Some(run) = self.run.as_mut() {
                    run.verification.visits.set_target_page(env.page.clone());
                    run.verification
                        .visits
                        .note_bound(env.snapshot.observed_ms, env.page.as_deref());
                }
                Resolution::Bound(binding)
            }
            BindingOutcome::Ambiguous { candidates } => Resolution::Ambiguous(candidates),
            BindingOutcome::NotFound {
                observation_complete,
            } => Resolution::NotFound {
                observation_complete,
            },
            BindingOutcome::Rejected { reason, .. } => Resolution::Unavailable(reason),
        }
    }

    // ---- Choosing an action -----------------------------------------------------------------------

    fn decide_step(&mut self, subgoal: &Subgoal, env: &EnvSnapshot) -> Step {
        let target = subgoal.target.clone().unwrap_or_else(|| {
            self.run
                .as_ref()
                .expect("a running root has a run")
                .goal
                .target
                .semantic_target
                .clone()
        });
        let description = self.describe_target(&target);
        let resolution = self.resolve(&target, description, env);
        let candidates = {
            let run = self.run.as_ref().expect("a running root has a run");
            build(
                &Scope {
                    goal: &run.goal,
                    plan_targets: &run.plan_targets,
                    pack: &run.pack,
                    env,
                },
                &Basis::Step {
                    subgoal,
                    resolution: &resolution,
                },
            )
        };
        match &resolution {
            Resolution::Bound(_) => {
                if let Some(step) = candidates.iter().find(|c| c.skill == subgoal.skill) {
                    match &step.precondition {
                        Precondition::Executable => {
                            let step = step.clone();
                            return self.run_direct(step);
                        }
                        Precondition::NeedsObservation => {
                            let now = self.now_ms;
                            return self.request_refresh(now, "the step needs current evidence");
                        }
                        Precondition::Rejected { reason } => {
                            let reason = format!("the plan step is not allowed: {reason}");
                            self.fail_root(&reason)?;
                            return Ok(true);
                        }
                    }
                }
            }
            Resolution::NotFound {
                observation_complete,
            } => {
                let stalls = self.bump_stalls();
                let now = self.now_ms;
                return if *observation_complete && stalls >= 2 {
                    self.escalate("the target was not found on a fully observed page")
                } else {
                    self.request_refresh(now, "the target is not visible yet")
                };
            }
            Resolution::Unavailable(reason) => {
                let reason = reason.clone();
                let now = self.now_ms;
                return self.request_refresh(now, &reason);
            }
            Resolution::Ambiguous(_) => {}
        }
        self.ask_backend(
            candidates,
            &subgoal.description,
            "ui.step@1",
            Some(subgoal.id.clone()),
        )
    }

    fn bump_stalls(&mut self) -> u32 {
        match self.run.as_mut() {
            Some(run) => {
                run.stalls += 1;
                run.stalls
            }
            None => 0,
        }
    }

    /// A dialog is in front of the application: nothing in the plan applies
    /// behind it, so the choice is among the dialog's own controls.
    fn decide_layer(&mut self, env: &EnvSnapshot) -> Step {
        let controls = bind_layer_controls(env, &mut self.bindings);
        let title = layer_title(env);
        let candidates = {
            let run = self.run.as_ref().expect("a running root has a run");
            build(
                &Scope {
                    goal: &run.goal,
                    plan_targets: &run.plan_targets,
                    pack: &run.pack,
                    env,
                },
                &Basis::Layer {
                    title: &title,
                    controls: &controls,
                },
            )
        };
        self.ask_backend(
            candidates,
            &format!("a dialog \"{title}\" is in front of the application"),
            "ui.layer@1",
            None,
        )
    }

    /// Put a choice among the candidates to the decision backend.
    fn ask_backend(
        &mut self,
        candidates: Vec<interaction_contracts::desktop::decision::Candidate>,
        subgoal_text: &str,
        program: &str,
        subgoal_id: Option<String>,
    ) -> Step {
        let Some(env) = self.state.latest().cloned() else {
            return Ok(false);
        };
        let compiled = {
            let run = self.run.as_ref().expect("a running root has a run");
            compile(
                CompileInput {
                    goal: &run.goal,
                    subgoal: subgoal_text,
                    env: &env,
                    now_ms: self.now_ms,
                    candidates,
                    recent_results: &run.recent,
                    missing_fields: vec![],
                },
                &self.cfg.compiler,
            )
        };
        let state = match compiled {
            Ok(state) => state,
            // Cutting what a candidate points at would be guessing; the plan
            // has to be made smaller instead.
            Err(CompileFailure::TooLarge(what)) => return self.escalate(&what),
        };
        let Some(basis) = self.current_basis() else {
            return Ok(false);
        };
        let (root, task_revision, plan_revision, root_epoch) = {
            let active = self.tasks.active().expect("a running root is active");
            (
                active.row.root_id.clone(),
                active.row.task_revision,
                active.row.plan_revision,
                active.row.root_epoch,
            )
        };
        let asked = state.candidates.len();
        let request = self.decisions.open(
            &mut self.storage,
            OpenSpec {
                root: &root,
                task_revision,
                plan_revision,
                root_epoch,
                program,
                state,
                basis,
                subgoal: subgoal_id,
                now_ms: self.now_ms,
                timeout_ms: self.cfg.decision_timeout_ms,
            },
        )?;
        self.note(format!(
            "asked the decision backend to choose among {asked} candidates"
        ));
        self.emit(Output::Decision(Box::new(request)));
        Ok(true)
    }

    fn run_direct(
        &mut self,
        candidate: interaction_contracts::desktop::decision::Candidate,
    ) -> Step {
        let root = self
            .tasks
            .active()
            .expect("a running root is active")
            .row
            .root_id
            .clone();
        let decision = self
            .decisions
            .open_direct(&mut self.storage, &root, self.now_ms)?;
        self.admit_candidate(decision, 1, candidate, DecisionSource::Plan, false)
    }

    // ---- What a backend answered ---------------------------------------------------------------------

    pub(super) fn on_decision(
        &mut self,
        proposal: interaction_contracts::desktop::decision::DecisionProposal,
    ) -> Result<(), EngineError> {
        let Some(basis_now) = self.current_basis() else {
            self.note("a decision answer arrived with no evidence to check it against");
            return Ok(());
        };
        let verdict =
            self.decisions
                .evaluate(&mut self.storage, &proposal, self.now_ms, &basis_now)?;
        match verdict {
            Verdict::Start {
                decision,
                generation,
                candidate,
                source,
                ..
            } => {
                self.admit_candidate(decision, generation, *candidate, source, false)?;
                Ok(())
            }
            Verdict::Control {
                control, reason, ..
            } => self.apply_control(control, &reason),
            Verdict::Dropped(dropped) => self.dropped_answer(&dropped),
        }
    }

    fn apply_control(&mut self, control: Control, reason: &str) -> Result<(), EngineError> {
        if let Some(cancel) = self
            .decisions
            .close(&mut self.storage, DecisionLifecycle::Superseded)?
        {
            self.emit(Output::CancelDecision(cancel));
        }
        match control {
            Control::ObserveMore => {
                self.bump_stalls();
                let now = self.now_ms;
                self.request_refresh(
                    now,
                    if reason.is_empty() {
                        "the backend asked for it"
                    } else {
                        reason
                    },
                )?;
                Ok(())
            }
            Control::RequestPlanning => {
                self.escalate(reason)?;
                Ok(())
            }
            Control::StopOrYield => {
                self.note(format!("the backend handed the task back: {reason}"));
                self.yield_to_user()
            }
        }
    }

    fn dropped_answer(&mut self, dropped: &Dropped) -> Result<(), EngineError> {
        self.note(format!("a decision answer was not used: {dropped}"));
        let max = self.cfg.max_decision_failures;
        let backoff = self.cfg.decision_backoff_ms;
        let now = self.now_ms;
        let mut escalate = false;
        match dropped {
            Dropped::BackendFailed(
                DecisionFailure::Unavailable
                | DecisionFailure::Timeout
                | DecisionFailure::DependencyInvalid,
            ) => {
                // The backend is not there: the decision waits and asks again
                // later at a higher generation. Nothing else is tried in its place.
                if let Some(run) = self.run.as_mut() {
                    run.decision_failures += 1;
                    run.decision_retry_at =
                        Some(now + backoff * u64::from(run.decision_failures.min(5)));
                }
            }
            Dropped::BackendFailed(_) | Dropped::Invalid(_) | Dropped::DigestMismatch => {
                if let Some(cancel) = self
                    .decisions
                    .close(&mut self.storage, DecisionLifecycle::Superseded)?
                {
                    self.emit(Output::CancelDecision(cancel));
                }
                if let Some(run) = self.run.as_mut() {
                    run.decision_failures += 1;
                    escalate = run.decision_failures >= max;
                }
            }
            Dropped::Unknown | Dropped::OldGeneration | Dropped::Expired | Dropped::Stale(_) => {}
        }
        if escalate {
            if let Some(run) = self.run.as_mut() {
                run.decision_failures = 0;
            }
            self.escalate("the decision backend could not give a usable answer")?;
        }
        Ok(())
    }

    // ---- When the plan has run out ------------------------------------------------------------------------

    /// Every step ran and the goal still does not hold.
    fn plan_exhausted(&mut self, verdict: &GoalVerdict) -> Step {
        let floor = self
            .run
            .as_ref()
            .map_or(0, |run| run.verification.evidence_floor_ms());
        let waiting_on_input = verdict.needs.iter().any(|need| need.requires_input);
        // Evidence from after the last input may simply not have arrived yet.
        if self.state.latest_since(floor).is_none() {
            let now = self.now_ms;
            return self
                .request_refresh(now.max(floor), "no evidence from after the last input yet");
        }
        let stalls = self.bump_stalls();
        if waiting_on_input {
            // The steps ran but the goal is not met: they are not enough.
            return self.escalate("every step ran but the goal is not met");
        }
        if stalls <= 1 {
            let now = self.now_ms;
            return self.request_refresh(now, "the evidence for the goal is still missing");
        }
        // Nothing more can be done and the goal cannot be confirmed.
        self.note("the input ended but the goal could not be confirmed");
        self.abandon_work()?;
        self.tasks.input_ended_unverified(
            &mut self.storage,
            self.now_ms,
            "the required evidence never appeared",
        )?;
        Ok(true)
    }

    // ---- Ending ---------------------------------------------------------------------------------------------

    /// A failure nothing can recover from.
    pub(super) fn fail_root(&mut self, reason: &str) -> Result<(), EngineError> {
        self.note(format!("the task failed: {reason}"));
        self.abandon_work()?;
        match self.tasks.active_state() {
            Some(RootState::Running | RootState::WaitingEvidence | RootState::WaitingPlan) => {
                self.tasks
                    .unrecoverable(&mut self.storage, self.now_ms, reason)?;
            }
            Some(_) => {
                let directive = self.tasks.stop_requested(&mut self.storage, self.now_ms)?;
                self.after_stop_requested(directive)?;
            }
            None => {}
        }
        Ok(())
    }

    pub(super) fn expire_root(&mut self) -> Step {
        self.note("the task ran out of time");
        self.abandon_work()?;
        let root = self.tasks.active().map(|active| active.row.root_id.clone());
        self.tasks
            .deadline_expired(&mut self.storage, self.now_ms)?;
        if let Some(root) = root {
            // Whatever the device holds for it has to be released.
            self.send_host_stop(root.as_str(), "the root deadline passed");
        }
        Ok(true)
    }

    /// The root ended in a final state: what was kept for it goes away.
    pub(super) fn finish_root(&mut self) {
        let ended = self.tasks.last_finished().map(|row| row.state);
        let Some(run) = self.run.take() else {
            return;
        };
        self.bindings.invalidate_all("the task ended");
        if let Some(session) = self.device.session_epoch() {
            for permit in run.permits {
                self.send(
                    Expect::Revoke,
                    DeviceCommand::Revoke(
                        interaction_contracts::desktop::device::RevokeExecution {
                            session_epoch: session,
                            permit,
                            reason: "the task ended".into(),
                        },
                    ),
                );
            }
        }
        self.note(format!("the task ended: {ended:?}"));
    }

    // ---- Time -----------------------------------------------------------------------------------------------

    pub(super) fn on_tick(&mut self) -> Result<(), EngineError> {
        self.planner_timed_out()?;
        self.retry_stale_wait()?;
        self.retry_or_expire_decision()?;
        self.check_device_timeouts()?;
        self.renew_lease();
        self.check_stop_wait();
        Ok(())
    }

    fn retry_or_expire_decision(&mut self) -> Result<(), EngineError> {
        let now = self.now_ms;
        let retry_due = self
            .run
            .as_ref()
            .and_then(|run| run.decision_retry_at)
            .is_some_and(|at| now >= at);
        if retry_due {
            if let Some(run) = self.run.as_mut() {
                run.decision_retry_at = None;
            }
            if let Some(request) =
                self.decisions
                    .retry(&mut self.storage, now, self.cfg.decision_timeout_ms)?
            {
                self.note("asking the decision backend again");
                self.emit(Output::Decision(Box::new(request)));
            }
            return Ok(());
        }
        let waiting_for_retry = self
            .run
            .as_ref()
            .is_some_and(|run| run.decision_retry_at.is_some());
        if waiting_for_retry {
            return Ok(());
        }
        if let Some(cancel) = self.decisions.expire(&mut self.storage, now)? {
            self.note("the decision backend did not answer in time");
            self.emit(Output::CancelDecision(cancel));
            let backoff = self.cfg.decision_backoff_ms;
            if let Some(run) = self.run.as_mut() {
                run.decision_failures += 1;
                run.decision_resume_ms = now + backoff * u64::from(run.decision_failures.min(5));
            }
        }
        Ok(())
    }

    fn renew_lease(&mut self) {
        if self.device.manifest().is_none() {
            return;
        }
        let every = (self.cfg.lease_ms / 2).max(1);
        let due = self
            .last_lease_ms
            .is_none_or(|last| self.now_ms.saturating_sub(last) >= every);
        if !due {
            return;
        }
        self.last_lease_ms = Some(self.now_ms);
        let epoch = self.device.session_epoch().unwrap_or(0);
        self.send(
            Expect::Lease,
            DeviceCommand::RenewLease(interaction_contracts::desktop::device::LeaseRenew {
                lease_epoch: epoch,
                valid_for_ms: self.cfg.lease_ms,
            }),
        );
    }
}
