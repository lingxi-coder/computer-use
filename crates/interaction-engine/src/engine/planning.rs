//! Deliberation's side of the loop: compiling the user's words into a goal,
//! and asking the planner again when the plan runs out (design 12).
//!
//! What the planner returns is a proposal. A goal is only ever taken from the
//! answer to a compile request, and a plan is installed only after it has been
//! checked against the frozen goal exactly like the first one.

use interaction_contracts::desktop::ids::RootTaskId;
use interaction_contracts::desktop::planning::{
    PlanBundle, PlannerOutput, PlanningContext, PlanningReason,
};

use super::{Engine, EngineError, Output, Step};
use crate::deliberation::{Arrival, StaleReason};
use crate::reaction::skills::Skill;
use crate::tasks::{CreateOutcome, DraftError, PlanInstallError};

fn allowed_skills() -> Vec<String> {
    Skill::IMPLEMENTED.map(String::from).to_vec()
}

impl Engine {
    pub(super) fn start_task(&mut self, text: &str) -> Result<(), EngineError> {
        match self
            .tasks
            .create_root(&mut self.storage, self.now_ms, text)?
        {
            CreateOutcome::Started(root) => {
                self.note(format!("task {root} is being compiled into a goal"));
                self.request_goal_compile(&root, text);
            }
            CreateOutcome::Queued => self.note("another task is running; this one waits"),
        }
        Ok(())
    }

    fn request_goal_compile(&mut self, root: &RootTaskId, text: &str) {
        let context = PlanningContext {
            user_text: text.to_string(),
            unknown_items: vec![],
            allowed_skills: allowed_skills(),
            recent_failures: vec![],
            notes: vec![],
        };
        let request = self.planning.request(
            root,
            1,
            1,
            PlanningReason::GoalCompile,
            context,
            self.now_ms,
            self.cfg.planning_timeout_ms,
        );
        self.emit(Output::Plan(Box::new(request)));
    }

    /// Compiling waits for the planner; its timeout is handled on a tick.
    pub(super) fn step_compiling(&mut self) -> Step {
        Ok(false)
    }

    pub(super) fn on_planner(
        &mut self,
        request_id: &interaction_contracts::desktop::ids::RequestId,
        generation: u64,
        output: PlannerOutput,
    ) -> Result<(), EngineError> {
        let reason = self.planning.inflight().map(|inflight| inflight.reason);
        match self
            .planning
            .arrived(request_id, generation, output, self.now_ms)
        {
            Arrival::Accepted(output) => match reason {
                Some(PlanningReason::GoalCompile) => self.goal_answer(*output),
                Some(_) => self.plan_answer(*output),
                None => Ok(()),
            },
            // The request is over without a usable answer.
            Arrival::Stale(why @ (StaleReason::WrongPhase | StaleReason::Expired))
                if reason == Some(PlanningReason::GoalCompile) =>
            {
                self.note(format!("the planner's answer was unusable: {why:?}"));
                self.tasks.fail_compile(
                    &mut self.storage,
                    self.now_ms,
                    "the planner did not return a goal",
                )?;
                Ok(())
            }
            Arrival::Stale(why) => {
                self.note(format!("a planner answer was dropped: {why:?}"));
                Ok(())
            }
        }
    }

    fn goal_answer(&mut self, output: PlannerOutput) -> Result<(), EngineError> {
        let PlannerOutput::Plan { proposal } = output else {
            self.tasks.fail_compile(
                &mut self.storage,
                self.now_ms,
                "the planner answered with something other than a goal",
            )?;
            return Ok(());
        };
        let Some(draft) = proposal.goal_draft.as_ref() else {
            self.note("the planner did not find a goal in the request");
            self.tasks.fail_compile(
                &mut self.storage,
                self.now_ms,
                "the planner did not return a goal",
            )?;
            return Ok(());
        };
        match self
            .tasks
            .accept_draft(&mut self.storage, self.now_ms, draft, &proposal.bundle)
        {
            Ok(_) => {
                self.note("the goal is ready and waits for the user to confirm it");
                Ok(())
            }
            Err(DraftError::Compile(error)) => {
                self.note(format!("the goal cannot be used: {error}"));
                self.tasks
                    .fail_compile(&mut self.storage, self.now_ms, &error.to_string())?;
                Ok(())
            }
            Err(DraftError::Task(error)) => Err(error.into()),
        }
    }

    /// Ask the planner for a new plan. Costs a replan; when none is left the
    /// root fails instead.
    pub(super) fn escalate(&mut self, reason: &str) -> Step {
        let Some(run) = self.run.as_ref() else {
            return Ok(false);
        };
        let root = run.goal.root_task_id.clone();
        let user_text = run.goal.user_text.clone();
        let task_revision = run.goal.task_revision;
        let recent: Vec<String> = run.recent.iter().rev().take(5).rev().cloned().collect();
        let root_epoch = self
            .tasks
            .active()
            .map_or(1, |active| active.row.root_epoch);
        if !self.tasks.await_plan(&mut self.storage, self.now_ms)? {
            self.fail_root("the replan budget is exhausted")?;
            return Ok(true);
        }
        if let Some(cancel) = self.decisions.close(
            &mut self.storage,
            crate::ports::DecisionLifecycle::Superseded,
        )? {
            self.emit(Output::CancelDecision(cancel));
        }
        let request = self.planning.request(
            &root,
            task_revision,
            root_epoch,
            PlanningReason::Escalation,
            PlanningContext {
                user_text,
                unknown_items: vec![reason.to_string()],
                allowed_skills: allowed_skills(),
                recent_failures: recent,
                notes: vec![],
            },
            self.now_ms,
            self.cfg.planning_timeout_ms,
        );
        self.emit(Output::Plan(Box::new(request)));
        self.note(format!("asked the planner for a new plan: {reason}"));
        Ok(true)
    }

    fn plan_answer(&mut self, output: PlannerOutput) -> Result<(), EngineError> {
        match output {
            PlannerOutput::Plan { proposal } => {
                match self.tasks.install_plan(
                    &mut self.storage,
                    self.now_ms,
                    Some(&proposal.bundle),
                ) {
                    Ok(_) => {
                        self.plan_installed(&proposal.bundle);
                        Ok(())
                    }
                    Err(PlanInstallError::Rejected(rejection)) => {
                        self.note(format!("the new plan was refused: {rejection}"));
                        self.fail_root(&format!("the planner's plan was refused: {rejection}"))?;
                        Ok(())
                    }
                    Err(PlanInstallError::Task(error)) => Err(error.into()),
                }
            }
            PlannerOutput::Review { proposal } => {
                self.note(format!(
                    "the planner had nothing to change: {}",
                    proposal.notes.join("; ")
                ));
                self.resume_current_plan()
            }
            PlannerOutput::EvidenceRequest { request } => {
                self.note(format!(
                    "the planner asked for {} more facts",
                    request.needs.len()
                ));
                self.resume_current_plan()
            }
        }
    }

    fn resume_current_plan(&mut self) -> Result<(), EngineError> {
        match self
            .tasks
            .install_plan(&mut self.storage, self.now_ms, None)
        {
            Ok(_) => Ok(()),
            Err(PlanInstallError::Task(error)) => Err(error.into()),
            Err(PlanInstallError::Rejected(_)) => Ok(()),
        }
    }

    fn plan_installed(&mut self, bundle: &PlanBundle) {
        if let Some(run) = self.run.as_mut() {
            run.done.clear();
            run.failures = 0;
            run.plan_targets = bundle
                .subgoals
                .iter()
                .filter_map(|subgoal| subgoal.target.as_ref())
                .map(|target| target.as_str().to_string())
                .collect();
            run.plan_targets
                .insert(run.goal.target.semantic_target.as_str().to_string());
        }
        self.note("a new plan was installed");
    }

    /// A planner request ran out of time.
    pub(super) fn planner_timed_out(&mut self) -> Result<(), EngineError> {
        let Some(expired) = self.planning.expired(self.now_ms) else {
            return Ok(());
        };
        self.note(format!(
            "the planner did not answer in time ({:?})",
            expired.reason
        ));
        match expired.reason {
            PlanningReason::GoalCompile => {
                self.tasks.fail_compile(
                    &mut self.storage,
                    self.now_ms,
                    "the planner did not answer in time",
                )?;
            }
            // The fast path carries on with the plan it has; the replan that
            // was spent stays spent.
            _ => {
                self.resume_current_plan()?;
            }
        }
        Ok(())
    }
}
