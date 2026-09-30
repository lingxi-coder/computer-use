//! The operator's commands (design 27.1). The operator can create, confirm,
//! pause, resume and stop tasks and choose the mode; nothing else in the
//! system can, and no model can raise the mode.

use std::collections::BTreeSet;

use interaction_contracts::desktop::device::{
    RootGrant, StopCommand, StopDisposition, StopScope, StopSource,
};
use interaction_contracts::desktop::ids::{DecisionId, RootTaskId};
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode, StopStatus};
use interaction_contracts::desktop::task::{GoalSpec, RootState};

use super::{Engine, EngineError, RootRun};
use crate::execution::{DeviceCommand, Expect};
use crate::memory::Memory;
use crate::ports::DecisionLifecycle;
use crate::tasks::{ConfirmOutcome, StopDirective, TaskError};
use crate::verification::Verification;

impl Engine {
    pub(super) fn on_operator(&mut self, command: OperatorCommand) -> Result<(), EngineError> {
        match command {
            OperatorCommand::CreateTask { text } => self.start_task(&text),
            OperatorCommand::ConfirmGoal {
                confirmation_id,
                approved,
            } => {
                let outcome = match self.tasks.answer_confirmation(
                    &mut self.storage,
                    self.now_ms,
                    &confirmation_id,
                    approved,
                ) {
                    Ok(outcome) => outcome,
                    // A click on a prompt that is no longer there changes nothing.
                    Err(TaskError::UnknownConfirmation) => {
                        self.note("that confirmation is no longer pending");
                        return Ok(());
                    }
                    Err(error) => return Err(error.into()),
                };
                match outcome {
                    ConfirmOutcome::GoalFrozen { goal, grant } => self.begin_run(*goal, grant),
                    ConfirmOutcome::GoalDeclined => {
                        self.note("the goal was declined; nothing was run");
                        Ok(())
                    }
                    ConfirmOutcome::EffectApproved { decision } => self.effect_approved(&decision),
                    ConfirmOutcome::EffectDenied => {
                        self.note("the action was not confirmed; the task is being stopped");
                        self.after_stop_requested(StopDirective::AwaitRelease)
                    }
                }
            }
            OperatorCommand::StopTask { root_task_id } => self.stop_task(root_task_id),
            OperatorCommand::PauseTask { root_task_id } => self.pause_task(&root_task_id),
            OperatorCommand::ResumeTask { root_task_id } => self.resume_task(&root_task_id),
            OperatorCommand::SetMode { mode } => {
                let lowered = self.mode == RunMode::Execute && mode != RunMode::Execute;
                self.mode = mode;
                self.note(format!("mode is now {}", mode_name(mode)));
                if lowered && self.run.is_some() {
                    // Nothing may be submitted outside execute mode.
                    self.pause_root("the mode was lowered")?;
                }
                Ok(())
            }
        }
    }

    /// The goal is frozen: everything a live root needs is created.
    fn begin_run(&mut self, goal: GoalSpec, grant: RootGrant) -> Result<(), EngineError> {
        let Some(active) = self.tasks.active() else {
            return Ok(());
        };
        let pack = active.pack.clone().ok_or_else(|| {
            EngineError::Task(TaskError::Invalid(
                "the goal has no environment pack".into(),
            ))
        })?;
        let frozen_ms = active.row.frozen_ms.unwrap_or(self.now_ms);
        let mut plan_targets: BTreeSet<String> = active
            .row
            .plan
            .as_ref()
            .map(|plan| {
                plan.subgoals
                    .iter()
                    .filter_map(|subgoal| subgoal.target.as_ref())
                    .map(|target| target.as_str().to_string())
                    .collect()
            })
            .unwrap_or_default();
        plan_targets.insert(goal.target.semantic_target.as_str().to_string());
        let root = goal.root_task_id.clone();
        self.run = Some(RootRun {
            verification: Verification::new(frozen_ms),
            goal,
            pack,
            grant,
            registered: false,
            registering: false,
            done: BTreeSet::new(),
            attempt: None,
            wait: None,
            refined_group: None,
            chosen: None,
            plan_targets,
            recent: Vec::new(),
            failures: 0,
            decision_failures: 0,
            decision_resume_ms: 0,
            decision_retry_at: None,
            stalls: 0,
            bind_seq: 0,
            resume_authorized: false,
            reconcile: Vec::new(),
            reconciling: None,
            revalidate_floor: None,
            permits: Vec::new(),
            stop_wait_since: None,
            last_attempt: None,
        });
        // The knowledge the root may use is fixed now; a later release changes
        // what new roots pin, never this one.
        match Memory::pin(&mut self.storage, &root) {
            Ok(Some(release)) => self.note(format!("task {root} uses knowledge release {release}")),
            Ok(None) => {}
            Err(error) => return Err(error.into()),
        }
        self.note("the goal is confirmed and frozen");
        Ok(())
    }

    fn effect_approved(&mut self, decision: &DecisionId) -> Result<(), EngineError> {
        let Some(pending) = self.decisions.pending() else {
            self.note("the approved action is no longer waiting");
            return Ok(());
        };
        if pending.decision_id() != decision {
            self.note("the approval is for a decision that is no longer open");
            return Ok(());
        }
        let generation = pending.generation();
        let basis = pending.basis.clone();
        let Some(held) = self.decisions.release_hold() else {
            return Ok(());
        };
        // The user confirmed one action in the situation they were shown.
        // If that situation is gone the approval lapses with it.
        let now = self.current_basis();
        if let Some(reason) = now.as_ref().and_then(|now| basis.broken_by(now)) {
            self.note(format!("the approval lapsed: {reason}"));
            if let Some(cancel) = self
                .decisions
                .close(&mut self.storage, DecisionLifecycle::Superseded)?
            {
                self.emit(super::Output::CancelDecision(cancel));
            }
            return Ok(());
        }
        self.admit_candidate(
            decision.clone(),
            generation,
            held.candidate,
            held.source,
            true,
        )
        .map(|_| ())
    }

    // ---- Stopping, pausing, resuming -------------------------------------------------

    fn stop_task(&mut self, root: Option<RootTaskId>) -> Result<(), EngineError> {
        let Some(active) = self.tasks.active() else {
            self.note("there is no task to stop");
            return Ok(());
        };
        if root
            .as_ref()
            .is_some_and(|named| named != &active.row.root_id)
        {
            self.note("the stop names another task; it was ignored");
            return Ok(());
        }
        let directive = self.tasks.stop_requested(&mut self.storage, self.now_ms)?;
        self.note("the operator stopped the task");
        self.after_stop_requested(directive)
    }

    /// The root is cancelling. Drop whatever was in flight for it and make sure
    /// the device releases its input, even if the direct stop path already did.
    pub(super) fn after_stop_requested(
        &mut self,
        directive: StopDirective,
    ) -> Result<(), EngineError> {
        self.abandon_work()?;
        match directive {
            StopDirective::Finished => {
                self.stop_status = StopStatus::StoppedConfirmed;
            }
            StopDirective::AwaitRelease => {
                self.stop_status = StopStatus::Stopping;
                if let Some(root) = self.tasks.active().map(|a| a.row.root_id.clone()) {
                    self.send_host_stop(root.as_str(), "the operator stopped the task");
                }
            }
        }
        Ok(())
    }

    /// Forget every request that belonged to the root that is ending.
    pub(super) fn abandon_work(&mut self) -> Result<(), EngineError> {
        if let Some(cancel) = self
            .decisions
            .close(&mut self.storage, DecisionLifecycle::Superseded)?
        {
            self.emit(super::Output::CancelDecision(cancel));
        }
        self.planning.cancel();
        if let Some(run) = self.run.as_mut() {
            run.wait = None;
        }
        self.end_attempt_cancelled()
    }

    pub(super) fn send_host_stop(&mut self, root: &str, reason: &str) {
        self.send(
            Expect::Stop,
            DeviceCommand::Stop(StopCommand {
                scope: StopScope::Root {
                    root_id: root.to_string(),
                },
                disposition: StopDisposition::Cancel,
                source: StopSource::Host,
                reason: reason.to_string(),
            }),
        );
    }

    fn pause_task(&mut self, root: &RootTaskId) -> Result<(), EngineError> {
        let Some(active) = self.tasks.active() else {
            self.note("there is no task to pause");
            return Ok(());
        };
        if &active.row.root_id != root {
            self.note("the pause names another task; it was ignored");
            return Ok(());
        }
        self.pause_root("the operator paused the task")
    }

    /// Release the input and keep the progress. Only an explicit resume goes on.
    pub(super) fn pause_root(&mut self, reason: &str) -> Result<(), EngineError> {
        let Some(active) = self.tasks.active() else {
            return Ok(());
        };
        if !matches!(
            active.row.state,
            RootState::Running
                | RootState::WaitingEvidence
                | RootState::WaitingPlan
                | RootState::NeedsInput
        ) {
            self.note("the task cannot be paused in its current state");
            return Ok(());
        }
        let root_id = active.row.root_id.as_str().to_string();
        self.tasks.pause_requested(&mut self.storage, self.now_ms)?;
        self.abandon_work()?;
        self.stop_status = StopStatus::Stopping;
        self.send(
            Expect::Stop,
            DeviceCommand::Stop(StopCommand {
                scope: StopScope::Root { root_id },
                disposition: StopDisposition::Pause,
                source: StopSource::Host,
                reason: reason.to_string(),
            }),
        );
        self.note(format!("the task is being paused: {reason}"));
        Ok(())
    }

    /// A backend handed the task back: the same as a pause, so the person decides.
    pub(super) fn yield_to_user(&mut self) -> Result<(), EngineError> {
        self.pause_root("the decision backend handed the task back")
    }

    fn resume_task(&mut self, root: &RootTaskId) -> Result<(), EngineError> {
        let Some(active) = self.tasks.active() else {
            self.note("there is no task to resume");
            return Ok(());
        };
        if &active.row.root_id != root {
            self.note("the resume names another task; it was ignored");
            return Ok(());
        }
        match active.row.state {
            RootState::Suspended => {
                if let Some(run) = self.run.as_mut() {
                    run.resume_authorized = true;
                }
                self.send(
                    Expect::Resume,
                    DeviceCommand::Resume(interaction_contracts::desktop::device::ResumeCommand {
                        scope: StopScope::Root {
                            root_id: root.as_str().to_string(),
                        },
                    }),
                );
                self.note("the operator asked to resume the task");
            }
            RootState::NeedsRevalidation => {
                if let Some(run) = self.run.as_mut() {
                    run.resume_authorized = true;
                }
                self.note("the operator resumed the task; it is being revalidated");
            }
            _ => self.note("the task is not paused"),
        }
        Ok(())
    }
}

fn mode_name(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Observe => "observe",
        RunMode::Shadow => "shadow",
        RunMode::Execute => "execute",
    }
}
