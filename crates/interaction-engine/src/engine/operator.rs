//! The operator's commands (design 27.1). The operator can create, confirm,
//! pause, resume and stop tasks and choose the mode; nothing else in the
//! system can, and no model can raise the mode.

use std::collections::BTreeSet;

use interaction_contracts::desktop::device::{
    ReleaseStatus, RootGrant, StopCommand, StopDisposition, StopScope, StopSource,
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
    /// A release confirmed for a previous stop does not mean the new or
    /// resumed task is stopped. An unresolved release remains visible.
    pub(super) fn clear_confirmed_stop(&mut self) {
        if self.stop_status == StopStatus::StoppedConfirmed
            && matches!(
                self.cleanup,
                ReleaseStatus::Released | ReleaseStatus::NotNeeded
            )
        {
            self.stop_status = StopStatus::NotStopped;
            self.cleanup = ReleaseStatus::NotNeeded;
        }
    }

    pub(super) fn on_operator(&mut self, command: OperatorCommand) -> Result<(), EngineError> {
        match command {
            OperatorCommand::CreateTask { text } => self.start_task(&text),
            OperatorCommand::CreateScopedTask {
                objective,
                allowed_scope,
            } => self.start_scoped_task(&objective, allowed_scope),
            OperatorCommand::CreateFrozenTask {
                objective,
                allowed_scope,
                profile,
            } => self.start_frozen_task(&objective, allowed_scope, profile),
            OperatorCommand::ConfirmGoal {
                confirmation_id,
                approved,
            } => {
                let pending_goal = self
                    .tasks
                    .active()
                    .is_some_and(|active| active.row.draft.is_some() && active.row.goal.is_none());
                if approved && pending_goal {
                    let frozen_domain = self
                        .tasks
                        .active()
                        .and_then(|active| active.row.draft.as_ref())
                        .map(|draft| draft.execution_domain);
                    let frozen_browser_profile = self
                        .tasks
                        .active()
                        .and_then(|active| active.row.draft.as_ref())
                        .and_then(|draft| draft.browser_page_profile);
                    let current_manifest = self.device.manifest();
                    if current_manifest.is_none_or(|manifest| {
                        Some(manifest.execution_domain) != frozen_domain
                            || frozen_browser_profile != self.cfg.browser_page_profile
                            || manifest.validate().is_err()
                            || !self.device.execution_domain_available()
                    }) {
                        self.note(
                            "goal confirmation was refused because the trusted execution profile changed or is unavailable",
                        );
                        return Ok(());
                    }
                }
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
        if goal.execution_domain
            == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        {
            self.state.require_fresh_source_after(frozen_ms);
        }
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
            window_activated: false,
            keyboard_result_context: None,
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
            reconciliation_retries: 0,
            reconciled_input: BTreeSet::new(),
            effect_verification_only: false,
            revalidate_floor: None,
            permits: Vec::new(),
            stop_wait_since: None,
            paused_session: None,
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
        self.clear_source_table_scope(false);
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
        self.cancel_computer_lane("the root's current work was abandoned");
        if let Some(cancel) = self
            .decisions
            .close(&mut self.storage, DecisionLifecycle::Superseded)?
        {
            self.emit(super::Output::CancelDecision(cancel));
        }
        if let Some(request) = self.planning.cancel() {
            self.emit(super::Output::CancelPlan(request));
        }
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
        self.clear_source_table_scope(false);
        self.abandon_work()?;
        let session = self.device.session_epoch();
        if let Some(run) = self.run.as_mut() {
            run.paused_session = session;
        }
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
        if active.row.goal.as_ref().is_some_and(|goal| {
            goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
        }) {
            self.note(
                "the browser page scope is single-use; cancel this root, start a fresh browser session, and create a new task",
            );
            return Ok(());
        }
        match active.row.state {
            RootState::Suspended => {
                self.clear_computer_block();
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
                self.clear_computer_block();
                self.clear_confirmed_stop();
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

#[cfg(test)]
mod browser_resume_tests {
    use super::*;
    use crate::engine::{EngineConfig, Input, Output};
    use crate::environment::pack::AppPack;
    use crate::infrastructure::storage::Storage;
    use crate::tasks::TaskConfig;
    use crate::tasks::store::TaskStore;
    use interaction_contracts::desktop::browser::BrowserPageProfile;
    use interaction_contracts::desktop::device::{
        BackendKind, CapabilityState, ExecutionDomainCapability,
    };
    use interaction_contracts::desktop::domain::{DomainAction, ExecutionDomain};
    use interaction_contracts::desktop::effect::EffectClass;
    use interaction_contracts::desktop::ids::{AttemptId, RootTaskId, SemanticTargetRef};
    use interaction_contracts::desktop::planning::{GoalDraft, PlanBundle, Subgoal};
    use interaction_contracts::desktop::task::{
        ConfirmationScope, ExpectedValue, RootState, TargetSpec,
    };

    const TASK_TEXT: &str = "Enable browser fixture checkbox";

    fn headless_manifest() -> interaction_contracts::desktop::device::CapabilityManifest {
        let mut manifest = interaction_contracts::desktop::samples::manifest();
        manifest.backend = BackendKind::HeadlessBrowser;
        manifest.execution_domain = ExecutionDomain::HeadlessBrowser;
        manifest.human_input = CapabilityState::Unavailable;
        manifest.hit_test = CapabilityState::Unavailable;
        manifest.input = CapabilityState::Available;
        manifest.domains = ExecutionDomainCapability::all_unavailable();
        let browser = manifest
            .domains
            .iter_mut()
            .find(|capability| capability.domain == ExecutionDomain::HeadlessBrowser)
            .unwrap();
        browser.state = CapabilityState::Available;
        browser.actions = vec![DomainAction::HeadlessBrowserAction];
        manifest
    }

    fn browser_root(
        needs_revalidation: bool,
        retain_unknown_reconcile: bool,
    ) -> (Engine, RootTaskId) {
        let profile = BrowserPageProfile::FixtureBackgroundOption;
        let mut tasks = TaskConfig::new(vec![AppPack::browser_fixture()]);
        tasks.default_budget.max_observations = 1;
        let mut config = EngineConfig::new(tasks);
        config.browser_page_profile = Some(profile);
        config.initial_mode = RunMode::Execute;
        config.refresh_timeout_ms = 5;
        let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());
        engine.device.install_manifest(headless_manifest());

        let root = match engine
            .tasks
            .create_root(&mut engine.storage, 0, TASK_TEXT)
            .unwrap()
        {
            crate::tasks::CreateOutcome::Started(root) => root,
            crate::tasks::CreateOutcome::Queued => panic!("no root should already be active"),
        };
        let target = SemanticTargetRef::new(profile.semantic_target()).unwrap();
        let draft = GoalDraft {
            app: profile.app().into(),
            account: None,
            target: TargetSpec {
                row: None,
                semantic_target: target.clone(),
                label: profile.label().into(),
                group: None,
                role_hint: Some(profile.role().into()),
            },
            desired: ExpectedValue::Bool { value: true },
            confirmation_scope: ConfirmationScope::CurrentUi,
            max_effect_class: EffectClass::ReversibleEdit,
        };
        let bundle = PlanBundle {
            subgoals: vec![Subgoal {
                id: "ensure".into(),
                description: "enable the registered checkbox".into(),
                skill: "ui.ensure_value".into(),
                target: Some(target),
                expects: vec!["value".into()],
                scroll: None,
                select: None,
            }],
            dependencies: vec![],
            required_observations: vec![],
            expected_effects: vec!["goal".into()],
            recovery_branches: vec![],
            termination: vec!["goal.required".into()],
        };
        let confirmation = engine
            .tasks
            .accept_draft_with_profile(
                &mut engine.storage,
                1,
                &draft,
                ExecutionDomain::HeadlessBrowser,
                Some(profile),
                &bundle,
            )
            .unwrap();
        let crate::tasks::ConfirmOutcome::GoalFrozen { goal, grant } = engine
            .tasks
            .answer_confirmation(&mut engine.storage, 2, &confirmation.confirmation_id, true)
            .unwrap()
        else {
            panic!("the fixed checkbox goal should freeze")
        };
        engine
            .tasks
            .admission_passed(&mut engine.storage, 3)
            .unwrap();
        engine
            .tasks
            .pause_requested(&mut engine.storage, 4)
            .unwrap();
        engine
            .tasks
            .suspend_unconfirmed(&mut engine.storage, 5)
            .unwrap();
        if needs_revalidation {
            engine
                .tasks
                .resume_requested(&mut engine.storage, 6)
                .unwrap();
        }
        engine.begin_run(*goal, grant).unwrap();
        let run = engine.run.as_mut().unwrap();
        run.registered = true;
        if retain_unknown_reconcile {
            run.reconcile.push((
                AttemptId::new("unknown-browser-attempt").unwrap(),
                "unknown-browser-intent".into(),
            ));
        }
        (engine, root)
    }

    fn no_browser_or_native_refresh(outputs: &[Output]) -> bool {
        outputs.iter().all(|output| {
            !matches!(output, Output::BrowserRefresh(_)) && !matches!(output, Output::Source { .. })
        })
    }

    #[test]
    fn browser_root_resume_is_rejected_without_refresh_or_observation_budget_spend() {
        let (mut engine, root) = browser_root(true, false);
        let before = TaskStore::load_budget(engine.storage().read(), root.as_str()).unwrap();
        assert_eq!(before.observations, 0);
        assert_eq!(engine.root_state(), Some(RootState::NeedsRevalidation));

        let outputs = engine.handle(
            10,
            Input::Operator(OperatorCommand::ResumeTask {
                root_task_id: root.clone(),
            }),
        );
        assert!(no_browser_or_native_refresh(&outputs));
        assert_eq!(engine.root_state(), Some(RootState::NeedsRevalidation));
        assert!(!engine.run.as_ref().unwrap().resume_authorized);
        assert!(engine.run.as_ref().unwrap().wait.is_none());

        for now_ms in [20, 100, 1_000, 5_000] {
            let tick = engine.handle(now_ms, Input::Tick);
            assert!(no_browser_or_native_refresh(&tick));
        }
        let after = TaskStore::load_budget(engine.storage().read(), root.as_str()).unwrap();
        assert_eq!(after.observations, before.observations);
        assert_eq!(engine.root_state(), Some(RootState::NeedsRevalidation));
        assert!(
            engine
                .timeline()
                .all(|entry| !entry.text.contains("observation budget is exhausted"))
        );
        assert!(engine.timeline().any(|entry| {
            entry.text.contains("single-use") && entry.text.contains("fresh browser session")
        }));
    }

    #[test]
    fn rejected_browser_resume_keeps_unknown_receipt_reconciliation_queued() {
        let (mut engine, root) = browser_root(true, true);
        let outputs = engine.handle(
            10,
            Input::Operator(OperatorCommand::ResumeTask { root_task_id: root }),
        );
        assert!(no_browser_or_native_refresh(&outputs));
        let run = engine.run.as_ref().unwrap();
        assert_eq!(run.reconcile.len(), 1);
        assert_eq!(run.reconcile[0].1, "unknown-browser-intent");
        assert!(run.reconciling.is_none());
        assert!(!run.resume_authorized);
    }

    #[test]
    fn suspended_browser_root_cannot_rebind_its_single_use_scope() {
        let (mut engine, root) = browser_root(false, false);
        assert_eq!(engine.root_state(), Some(RootState::Suspended));
        let outputs = engine.handle(
            10,
            Input::Operator(OperatorCommand::ResumeTask { root_task_id: root }),
        );
        assert!(no_browser_or_native_refresh(&outputs));
        assert_eq!(engine.root_state(), Some(RootState::Suspended));
        assert!(!engine.run.as_ref().unwrap().resume_authorized);
    }
}
