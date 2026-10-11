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
use interaction_contracts::desktop::task::{AllowedControlScope, FrozenTaskProfile};

use super::{Engine, EngineError, Output, Step};
use crate::deliberation::accounting::PlanningRequestAttribution;
use crate::deliberation::{Arrival, StaleReason};
use crate::environment::pack::AppPack;
use crate::reaction::skills::Skill;
use crate::tasks::goal::resolve_allowed_scope_app;
use crate::tasks::{CreateOutcome, DraftError, PlanInstallError, runtime::QueuedTask};

fn scoped_native_planning_note(
    scope: &AllowedControlScope,
    app: Option<&str>,
    packs: &[AppPack],
) -> String {
    let registration = app.and_then(|app| {
        let mut matches = packs.iter().filter(|pack| pack.bundle_id == app);
        let pack = matches.next()?;
        if matches.next().is_some() || scope.validate().is_err() {
            return None;
        }
        let control = pack.scoped_control(scope.targets.first()?)?;
        Some((pack, control))
    });
    let choice_values = registration.and_then(|(_, control)| match control.value_kind {
        crate::environment::pack::ScopedControlValueKind::Choice(values) => Some(values),
        crate::environment::pack::ScopedControlValueKind::Bool
        | crate::environment::pack::ScopedControlValueKind::Text
        | crate::environment::pack::ScopedControlValueKind::Number
        | crate::environment::pack::ScopedControlValueKind::RadioChoice(_) => None,
    });
    let radio_choice_values = registration.and_then(|(_, control)| match control.value_kind {
        crate::environment::pack::ScopedControlValueKind::RadioChoice(values) => Some(values),
        crate::environment::pack::ScopedControlValueKind::Bool
        | crate::environment::pack::ScopedControlValueKind::Text
        | crate::environment::pack::ScopedControlValueKind::Number
        | crate::environment::pack::ScopedControlValueKind::Choice(_) => None,
    });
    let validation_feedback =
        registration.and_then(|(pack, _)| pack.scoped_validation_feedback(scope.targets.first()?));
    let number_scroll_container = registration
        .and_then(|(pack, _)| pack.scoped_form_scroll_container(scope.targets.first()?));
    let commit =
        registration.and_then(|(pack, control)| pack.scoped_commit_binding(control.control));
    let base = "Trusted host profile selects ForegroundNative for a scoped task. Use the canonical app ID from the registered app catalog, not the display name in allowed_scope. Preserve the supplied allowed_scope exactly. The host freezes the execution domain independently of this draft.";
    if let Some(values) = radio_choice_values {
        let values = serde_json::to_string(values).expect("trusted radio choice list serializes");
        return format!(
            "{base} This registered AXRadioGroup is a typed radio choice. Set confirmation_scope to ui_revisit, max_effect_class to reversible_edit, and desired to exactly one of these Host-registered literal choices: {values}. Propose exactly one ui.select_by_label step for the frozen parent group, with select.choice_label equal to desired, followed by one dependent ui.revisit step for that same parent group. Do not propose ui.ensure_value or ui.enter_text, target a child radio button, or turn a child into a Bool goal. The Host resolves and presses the exact desired child only from fresh Source evidence, then derives the frozen revisit endpoints. This is a radio-group choice, not the popup ShowMenu transaction. Do not invent choices or name child/page targets."
        );
    }
    if let Some(values) = choice_values {
        let values = serde_json::to_string(values).expect("trusted choice list serializes");
        return format!(
            "{base} This registered AXPopUpButton is a typed Choice control. Set confirmation_scope to ui_revisit, max_effect_class to reversible_edit, and desired to exactly one of these Host-registered literal choices: {values}. Propose exactly one ui.select_by_label step for the frozen popup, with select.choice_label equal to desired, followed by one dependent ui.revisit step for that same control. Do not propose ui.ensure_value or ui.enter_text, and do not treat the popup as a set-value field. The Host uses the existing ShowMenu, fresh Source, and exact AXMenuItem Press transaction, then derives the revisit endpoints. Do not invent choices, add menu input mechanisms, or name page targets."
        );
    }
    if let Some(feedback) = validation_feedback {
        let description = serde_json::to_string(feedback.description)
            .expect("trusted validation description serializes");
        let role = serde_json::to_string(feedback.role).expect("trusted AX role serializes");
        let context = serde_json::to_string(feedback.context_label)
            .expect("trusted validation context serializes");
        let valid =
            serde_json::to_string(feedback.valid_text).expect("trusted validation text serializes");
        let invalid =
            serde_json::to_string(feedback.invalid_text).expect("trusted invalid text serializes");
        return format!(
            "{base} This exact registered text form also has Host-owned validation feedback. Keep the requested Region code as the primary Text goal and propose one ui.ensure_value step for that frozen field, followed by one dependent ui.revisit step for the same field. Completion additionally requires the current complete Source to contain exactly one visible, nonsecure {role} in group {context}, with exact AXDescription {description} and AXValue {valid}; missing, duplicate, offscreen, or invalid feedback {invalid} is unverified. Do not target the feedback as another control or infer validation success from the entered text alone. The Host derives the frozen revisit endpoints."
        );
    }
    if let Some(container) = number_scroll_container {
        let semantic = serde_json::to_string(container.semantic.as_str())
            .expect("trusted scroll semantic serializes");
        let label = serde_json::to_string(container.label.as_str())
            .expect("trusted scroll label serializes");
        let role =
            serde_json::to_string(container.role.as_str()).expect("trusted scroll role serializes");
        return format!(
            "{base} This exact registered offscreen numeric form control keeps the Goal as a typed integer. Set confirmation_scope to ui_revisit, max_effect_class to reversible_edit, and encode GoalDraft.desired as {{\"type\":\"number\",\"value\":integer}} within the exact safe-integer range -9007199254740991 through 9007199254740991; never downgrade it to Text. Propose exactly this ordered dependency chain: first ui.scroll_to targeting the frozen primary field, with scroll.container exactly semantic {semantic}, label {label}, role {role}, direction down, and bounded pixels/max_actions; then ui.ensure_value for that same primary field; then ui.revisit for that same primary field; then a second ui.scroll_to with the same exact registered container and primary destination after revisit so the rebuilt page returns the field to view. You may add one ui.read_value for the primary field only after that final scroll. Do not add other controls, child targets, recovery branches, or scroll containers. The Host accepts only the exact registered container, and runtime reads the canonical integer from the field's actual Source value."
        );
    }
    match commit {
        Some(commit) if commit.action.is_some() => {
            let action =
                serde_json::to_string(commit.action.as_ref().expect("checked Save action"))
                    .expect("trusted TargetSpec serializes");
            format!(
                "{base} This registered control requires explicit saving. Set confirmation_scope to commit and max_effect_class to commit. Propose exactly one ui.ensure_value step for the primary control, then one dependent ui.activate step using only this Host-registered Save target: {action}. Do not add other actions or a revisit. The Host derives and shows this exact Save action in the Goal confirmation; no other Commit action is authorized. Do not supply a receipt binding."
            )
        }
        Some(_) => format!(
            "{base} This registered control saves automatically. Use ui_revisit confirmation and reversible_edit maximum effect, with one ui.ensure_value step followed by a dependent ui.revisit for the same primary control. The Host waits for the application's visible save receipt before revisiting and derives navigation endpoints. Do not propose ui.activate, extra page targets, a Save action, or a receipt binding."
        ),
        None => format!(
            "{base} Propose one ui.ensure_value step for its registered main control, followed only by a dependent ui.revisit step for that same control when confirmation requires it. The Host derives revisit navigation endpoints; do not add page targets or ui.activate steps."
        ),
    }
}

fn allowed_skills(engine: &Engine) -> Vec<String> {
    Skill::IMPLEMENTED
        .into_iter()
        .filter(|skill| engine.cfg.allow_owned_document_save || *skill != "ui.save_document")
        .map(String::from)
        .collect()
}

impl Engine {
    /// The shell must durably admit accounting before it starts the provider
    /// request. This grants no execution authority or task changes.
    pub fn model_call_started(
        &mut self,
        at_ms: u64,
        request: &interaction_contracts::desktop::planning::PlanningRequest,
        provider: &str,
        model: &str,
    ) -> Result<(), crate::infrastructure::storage::StoreError> {
        if !self.planning.inflight().is_some_and(|i| {
            i.root == request.root_task_id
                && i.request_id == request.request_id
                && i.generation == request.generation
                && i.task_revision == request.task_revision
                && i.root_epoch == request.root_epoch
                && i.reason == request.reason
                && i.context == request.context
                && i.deadline_ms == request.deadline_ms
        }) {
            return Err(crate::infrastructure::storage::StoreError::Conflict(
                "model accounting scope is not current".into(),
            ));
        }
        crate::deliberation::accounting::started(&mut self.storage, request, at_ms, provider, model)
    }

    pub fn model_call_finished(
        &mut self,
        completion: &crate::deliberation::accounting::ModelCompletion,
    ) -> Result<(), crate::infrastructure::storage::StoreError> {
        crate::deliberation::accounting::finished(&mut self.storage, completion)
    }

    /// The Host records an S1 teacher call only for the exact decision request
    /// currently pending in Reaction. This uses its own ledger and generation.
    pub fn decision_model_call_started(
        &mut self,
        at_ms: u64,
        request: &interaction_contracts::desktop::decision::DecisionRequest,
        provider: &str,
        model: &str,
    ) -> Result<(), crate::infrastructure::storage::StoreError> {
        if !self
            .decisions
            .pending()
            .is_some_and(|pending| pending.request == *request)
        {
            return Err(crate::infrastructure::storage::StoreError::Conflict(
                "teacher accounting scope is not the pending decision".into(),
            ));
        }
        crate::deliberation::accounting::decision_call_started(
            &mut self.storage,
            request,
            at_ms,
            provider,
            model,
        )
    }

    /// Complete only the S1 row the Host opened; a cancelled decision need no
    /// longer be pending when its provider cleanup barrier is finalized.
    pub fn decision_model_call_finished(
        &mut self,
        completion: &crate::deliberation::accounting::DecisionCallCompletion,
    ) -> Result<(), crate::infrastructure::storage::StoreError> {
        crate::deliberation::accounting::decision_call_finished(&mut self.storage, completion)
    }

    pub(super) fn start_task(&mut self, text: &str) -> Result<(), EngineError> {
        self.start_queued_task(QueuedTask {
            text: text.to_string(),
            allowed_scope: None,
            allowed_scope_app: None,
            frozen_profile: None,
        })
    }

    pub(super) fn start_scoped_task(
        &mut self,
        objective: &str,
        allowed_scope: AllowedControlScope,
    ) -> Result<(), EngineError> {
        self.start_scoped_task_with_profile(objective, allowed_scope, None)
    }

    pub(super) fn start_frozen_task(
        &mut self,
        objective: &str,
        allowed_scope: AllowedControlScope,
        profile: FrozenTaskProfile,
    ) -> Result<(), EngineError> {
        if let Err(reason) = profile.validate() {
            self.note(format!("frozen task was refused: {reason}"));
            return Ok(());
        }
        self.start_scoped_task_with_profile(objective, allowed_scope, Some(profile))
    }

    fn start_scoped_task_with_profile(
        &mut self,
        objective: &str,
        allowed_scope: AllowedControlScope,
        frozen_profile: Option<FrozenTaskProfile>,
    ) -> Result<(), EngineError> {
        if let Err(reason) = interaction_contracts::desktop::task::validate_user_text(objective) {
            self.note(format!("scoped task was refused: {reason}"));
            return Ok(());
        }
        let allowed_scope_app = match resolve_allowed_scope_app(
            &allowed_scope,
            &self.tasks.config().packs,
            self.cfg.browser_page_profile,
        ) {
            Ok(app) => app,
            Err(error) => {
                self.note(format!("scoped task was refused: {error}"));
                return Ok(());
            }
        };
        self.start_queued_task(QueuedTask {
            text: objective.to_string(),
            allowed_scope: Some(allowed_scope),
            allowed_scope_app: Some(allowed_scope_app),
            frozen_profile,
        })
    }

    pub(super) fn start_queued_task(&mut self, queued: QueuedTask) -> Result<(), EngineError> {
        let text = queued.text.clone();
        let outcome =
            self.tasks
                .create_root_from_request(&mut self.storage, self.now_ms, queued)?;
        match outcome {
            CreateOutcome::Started(root) => {
                self.clear_confirmed_stop();
                self.note(format!("task {root} is being compiled into a goal"));
                self.request_goal_compile(&root, &text);
            }
            CreateOutcome::Queued => self.note("another task is running; this one waits"),
        }
        Ok(())
    }

    fn request_goal_compile(&mut self, root: &RootTaskId, text: &str) {
        let profile_note = self.device.manifest().map(|manifest| {
            match manifest.execution_domain {
                interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx => {
                    "Trusted host profile selects BackgroundAX. For simple boolean controls, the explicit supported confirmation scope is current_ui with a fresh same-target-context readback; do not request UI revisit, scrolling, rows, keyboard, pointer or window activation. The host freezes the execution domain independently of this draft.".to_string()
                }
                interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative => {
                    let active = self.tasks.active();
                    if let Some(scope) = active.and_then(|active| active.row.allowed_scope.as_ref()) {
                        scoped_native_planning_note(
                            scope,
                            active.and_then(|active| active.row.allowed_scope_app.as_deref()),
                            &self.tasks.config().packs,
                        )
                    } else {
                        "Trusted host profile selects ForegroundNative. Preserve the ordinary UI revisit or commit confirmation scope; the host freezes the execution domain independently of this draft.".to_string()
                    }
                }
                interaction_contracts::desktop::domain::ExecutionDomain::Simulated => {
                    "Trusted host profile selects Simulated. Preserve the ordinary UI revisit or commit confirmation scope; the host freezes the execution domain independently of this draft.".to_string()
                }
                interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
                    if self.cfg.browser_page_profile.is_some() => {
                    "Trusted host profile selects the fixed local browser checkbox fixture. Draft exactly one ui.ensure_value step for the registered checkbox with a boolean desired value and current_ui confirmation. The page profile and execution domain are frozen by the Host, never by this draft.".to_string()
                }
                interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
                | interaction_contracts::desktop::domain::ExecutionDomain::AppIntegration => {
                    "The selected trusted execution profile is not implemented; do not invent a supported confirmation scope.".to_string()
                }
            }
        });
        let context = PlanningContext {
            user_text: text.to_string(),
            allowed_scope: self
                .tasks
                .active()
                .and_then(|active| active.row.allowed_scope.clone()),
            unknown_items: vec![],
            allowed_skills: allowed_skills(self),
            recent_failures: vec![],
            notes: std::iter::once(crate::environment::pack::planning_catalog(
                &self.tasks.config().packs,
            ))
            .chain(profile_note)
            .collect(),
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
        let accepted_request = self
            .planning
            .inflight()
            .filter(|inflight| {
                inflight.request_id.as_str() == request_id.as_str()
                    && inflight.generation == generation
            })
            .cloned();
        let reason = accepted_request.as_ref().map(|inflight| inflight.reason);
        let attribution = accepted_request.map(|inflight| PlanningRequestAttribution {
            root: inflight.root,
            request: inflight.request_id,
            generation: inflight.generation,
            reason: inflight.reason,
        });
        match self
            .planning
            .arrived(request_id, generation, output, self.now_ms)
        {
            Arrival::Accepted(output) => match reason {
                Some(PlanningReason::GoalCompile) => {
                    self.goal_answer(*output, attribution.as_ref())
                }
                Some(_) => self.plan_answer(*output, attribution.as_ref()),
                None => Ok(()),
            },
            // The request is over without a usable answer.
            Arrival::Stale(why @ (StaleReason::WrongPhase | StaleReason::Expired)) => {
                self.note(format!("the planner's answer was unusable: {why:?}"));
                match reason {
                    Some(PlanningReason::GoalCompile) => {
                        self.tasks.fail_compile(
                            &mut self.storage,
                            self.now_ms,
                            "the planner did not return a goal",
                        )?;
                        Ok(())
                    }
                    // The coordinator sealed the request. Just like a timeout,
                    // keep the existing plan and the replan already spent.
                    Some(_) => self.resume_current_plan(),
                    None => Ok(()),
                }
            }
            Arrival::Stale(why) => {
                self.note(format!("a planner answer was dropped: {why:?}"));
                Ok(())
            }
        }
    }

    fn goal_answer(
        &mut self,
        output: PlannerOutput,
        attribution: Option<&PlanningRequestAttribution>,
    ) -> Result<(), EngineError> {
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
        if !super::menu::valid_menu_plan(
            &proposal.bundle,
            &draft.target.semantic_target,
            &draft.desired,
        ) {
            self.tasks.fail_compile(
                &mut self.storage,
                self.now_ms,
                "menu selection must keep the root opener and its exact frozen text choice",
            )?;
            return Ok(());
        }
        let Some(manifest) = self.device.manifest() else {
            self.tasks.fail_compile(
                &mut self.storage,
                self.now_ms,
                "the device has not selected a trusted execution domain",
            )?;
            return Ok(());
        };
        if let Err(reason) = manifest.validate() {
            self.tasks.fail_compile(
                &mut self.storage,
                self.now_ms,
                &format!("the device capability manifest is invalid: {reason}"),
            )?;
            return Ok(());
        }
        let execution_domain = manifest.execution_domain;
        match self.tasks.accept_draft_with_profile_attribution(
            &mut self.storage,
            self.now_ms,
            draft,
            execution_domain,
            self.cfg.browser_page_profile,
            &proposal.bundle,
            attribution,
        ) {
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
                allowed_scope: self
                    .run
                    .as_ref()
                    .and_then(|run| run.goal.allowed_scope.clone()),
                unknown_items: vec![reason.to_string()],
                allowed_skills: allowed_skills(self),
                recent_failures: recent,
                notes: vec![crate::environment::pack::planning_catalog(
                    &self.tasks.config().packs,
                )],
            },
            self.now_ms,
            self.cfg.planning_timeout_ms,
        );
        self.emit(Output::Plan(Box::new(request)));
        self.note(format!("asked the planner for a new plan: {reason}"));
        Ok(true)
    }

    fn plan_answer(
        &mut self,
        output: PlannerOutput,
        attribution: Option<&PlanningRequestAttribution>,
    ) -> Result<(), EngineError> {
        match output {
            PlannerOutput::Plan { proposal } => {
                if self.run.as_ref().is_some_and(|run| {
                    !super::menu::valid_menu_plan(
                        &proposal.bundle,
                        &run.goal.target.semantic_target,
                        &run.goal.desired,
                    )
                }) {
                    self.fail_root(
                        "the new menu plan changes the frozen opener or literal text result",
                    )?;
                    return Ok(());
                }
                let installation = match attribution {
                    Some(attribution) => self.tasks.install_plan_from_request(
                        &mut self.storage,
                        self.now_ms,
                        &proposal.bundle,
                        attribution,
                    ),
                    None => self.tasks.install_plan(
                        &mut self.storage,
                        self.now_ms,
                        Some(&proposal.bundle),
                    ),
                };
                match installation {
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
            for spec in bundle
                .subgoals
                .iter()
                .filter_map(|subgoal| subgoal.scroll.as_ref())
            {
                run.plan_targets
                    .insert(spec.container.semantic_target.as_str().to_string());
            }
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
        self.emit(Output::CancelPlan(expired.clone()));
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

    pub(super) fn on_planner_failed(
        &mut self,
        request_id: &interaction_contracts::desktop::ids::RequestId,
        generation: u64,
        failure: crate::deliberation::PlanningFailure,
    ) -> Result<(), EngineError> {
        let Some(request) = self.planning.failed(request_id, generation) else {
            return Ok(());
        };
        self.note(format!(
            "the model planning turn ended without a proposal: {failure:?}"
        ));
        if request.reason == PlanningReason::GoalCompile {
            self.tasks.fail_compile(
                &mut self.storage,
                self.now_ms,
                "the model did not return a valid goal",
            )?;
            Ok(())
        } else {
            self.resume_current_plan()
        }
    }
}

#[cfg(test)]
mod model_call_accounting_tests {
    use super::*;
    use crate::engine::{EngineConfig, Input, Output};
    use crate::infrastructure::storage::Storage;
    use crate::tasks::{TaskConfig, store::TaskStore};
    use interaction_contracts::desktop::browser::BrowserPageProfile;
    use interaction_contracts::desktop::operator::OperatorCommand;
    use interaction_contracts::desktop::task::{AllowedControlScope, AllowedControlTarget};

    #[test]
    fn scoped_save_planning_uses_only_the_exact_trusted_persistence_registration() {
        let app = "com.lingxi.computer-user.mvp-fixture";
        let pack = AppPack::registered_native(app).unwrap();
        let scope = |control: &str, group: &str| AllowedControlScope {
            application: "Interaction Fixture".into(),
            targets: vec![AllowedControlTarget {
                control: control.into(),
                group: Some(group.into()),
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        };
        let manual = scope("Include archived items", "Search");
        let note = scoped_native_planning_note(&manual, Some(app), std::slice::from_ref(&pack));
        let commit = pack
            .scoped_commit_binding(pack.scoped_control(&manual.targets[0]).unwrap().control)
            .unwrap();
        assert!(note.contains(commit.action.unwrap().semantic_target.as_str()));
        assert!(note.contains("confirmation_scope to commit and max_effect_class to commit"));
        assert!(note.contains("no other Commit action is authorized"));
        assert!(!note.contains("do not add page targets or ui.activate steps"));

        let auto = scoped_native_planning_note(
            &scope("免打扰时段", "通知"),
            Some(app),
            std::slice::from_ref(&pack),
        );
        assert!(auto.contains("saves automatically"));
        assert!(auto.contains("Do not propose ui.activate"));
        assert!(!auto.contains("Host-registered Save target:"));

        for (scope, app) in [
            (scope("Weekly digest", "Notifications"), Some(app)),
            (scope("Include archived items", "Notifications"), Some(app)),
            (manual, Some("unregistered.app")),
        ] {
            let note = scoped_native_planning_note(&scope, app, std::slice::from_ref(&pack));
            assert!(!note.contains("Host-registered Save target:"));
            assert!(note.contains("do not add page targets or ui.activate steps"));
        }
    }

    #[test]
    fn scoped_popup_planning_uses_the_exact_localized_choices_and_menu_workflow() {
        let app = "com.lingxi.computer-user.mvp-fixture";
        let pack = AppPack::registered_native(app).unwrap();
        let scope = |control: &str, form: &str| AllowedControlScope {
            application: "Interaction Fixture".into(),
            targets: vec![AllowedControlTarget {
                control: control.into(),
                group: None,
                form: Some(form.into()),
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        };
        for (label, form, values) in [
            (
                "Reminder cadence",
                "Preferences",
                "[\"Daily\",\"Weekly\",\"Monthly\"]",
            ),
            ("提醒频率", "偏好设置", "[\"每天\",\"每周\",\"每月\"]"),
        ] {
            let note = scoped_native_planning_note(
                &scope(label, form),
                Some(app),
                std::slice::from_ref(&pack),
            );
            assert!(note.contains(values));
            assert!(note.contains("ui.select_by_label"));
            assert!(note.contains("select.choice_label equal to desired"));
            assert!(note.contains("dependent ui.revisit"));
            assert!(note.contains("Do not propose ui.ensure_value or ui.enter_text"));
            assert!(note.contains("ShowMenu, fresh Source, and exact AXMenuItem Press"));
        }

        let wrong_context = scoped_native_planning_note(
            &scope("Reminder cadence", "Profile"),
            Some(app),
            std::slice::from_ref(&pack),
        );
        assert!(!wrong_context.contains("Host-registered literal choices"));
    }

    #[test]
    fn scoped_radio_group_planning_preserves_parent_text_goal_and_registered_choices() {
        let app = "com.lingxi.computer-user.mvp-fixture";
        let pack = AppPack::registered_native(app).unwrap();
        let scope = |control: &str, form: &str| AllowedControlScope {
            application: "Interaction Fixture".into(),
            targets: vec![AllowedControlTarget {
                control: control.into(),
                group: None,
                form: Some(form.into()),
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        };
        for (label, form, values) in [
            (
                "Summary grouping",
                "Preferences",
                "[\"By day\",\"By week\",\"By month\"]",
            ),
            ("摘要分组", "偏好设置", "[\"按天\",\"按周\",\"按月\"]"),
        ] {
            let note = scoped_native_planning_note(
                &scope(label, form),
                Some(app),
                std::slice::from_ref(&pack),
            );
            assert!(note.contains("registered AXRadioGroup"));
            assert!(note.contains(values));
            assert!(note.contains("desired to exactly one"));
            assert!(note.contains("ui.select_by_label"));
            assert!(note.contains("select.choice_label equal to desired"));
            assert!(note.contains("one dependent ui.revisit"));
            assert!(note.contains("target a child radio button"));
            assert!(note.contains("turn a child into a Bool goal"));
            assert!(
                note.contains("This is a radio-group choice, not the popup ShowMenu transaction.")
            );
            assert!(note.contains("Do not propose ui.ensure_value or ui.enter_text"));
        }

        let wrong_context = scoped_native_planning_note(
            &scope("Summary grouping", "Profile"),
            Some(app),
            std::slice::from_ref(&pack),
        );
        assert!(!wrong_context.contains("Host-registered literal choices"));
        assert!(!wrong_context.contains("registered AXRadioGroup"));
    }

    #[test]
    fn region_code_planning_uses_only_pack_registered_validation_feedback() {
        let app = "com.lingxi.computer-user.mvp-fixture";
        let pack = AppPack::registered_native(app).unwrap();
        let scope = |control: &str, form: &str| AllowedControlScope {
            application: "Interaction Fixture".into(),
            targets: vec![AllowedControlTarget {
                control: control.into(),
                group: None,
                form: Some(form.into()),
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        };

        for (control, form, valid, invalid) in [
            (
                "Region code",
                "Account",
                "Region code is valid",
                "Enter a region code in the form CA-1234",
            ),
            (
                "地区代码",
                "账户",
                "地区代码有效",
                "请输入 CA-1234 格式的地区代码",
            ),
        ] {
            let note = scoped_native_planning_note(
                &scope(control, form),
                Some(app),
                std::slice::from_ref(&pack),
            );
            assert!(note.contains("Host-owned validation feedback"));
            assert!(note.contains("one ui.ensure_value step"));
            assert!(note.contains("one dependent ui.revisit step"));
            assert!(note.contains("current complete Source"));
            assert!(note.contains("AXStaticText"));
            assert!(note.contains(form));
            assert!(note.contains("fixture-region-code-validation"));
            assert!(note.contains(valid));
            assert!(note.contains(invalid));
            assert!(note.contains("Do not target the feedback as another control"));
        }

        for (control, form) in [
            ("Region code", "Profile"),
            ("Region code", "Preferences"),
            ("Display name", "Profile"),
        ] {
            let note = scoped_native_planning_note(
                &scope(control, form),
                Some(app),
                std::slice::from_ref(&pack),
            );
            assert!(!note.contains("Host-owned validation feedback"));
            assert!(!note.contains("fixture-region-code-validation"));
        }
    }

    #[test]
    fn numeric_form_planning_preserves_integer_goal_and_registered_scroll_round_trip() {
        let app = "com.lingxi.computer-user.mvp-fixture";
        let pack = AppPack::registered_native(app).unwrap();
        let scope = |control: &str, form: &str| AllowedControlScope {
            application: "Interaction Fixture".into(),
            targets: vec![AllowedControlTarget {
                control: control.into(),
                group: None,
                form: Some(form.into()),
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        };

        for (control, form, semantic, label) in [
            (
                "Alert threshold",
                "Advanced preferences",
                "scroll-preferences",
                "Preferences content",
            ),
            (
                "提醒阈值",
                "高级偏好设置",
                "scroll-preferences-zh",
                "偏好设置内容",
            ),
        ] {
            let note = scoped_native_planning_note(
                &scope(control, form),
                Some(app),
                std::slice::from_ref(&pack),
            );
            assert!(note.contains("GoalDraft.desired as {\"type\":\"number\",\"value\":integer}"));
            assert!(note.contains("never downgrade it to Text"));
            assert!(note.contains("-9007199254740991 through 9007199254740991"));
            assert!(note.contains(semantic));
            assert!(note.contains(label));
            assert!(note.contains("role \"AXScrollArea\""));
            assert!(note.contains("first ui.scroll_to"));
            assert!(note.contains("then ui.ensure_value"));
            assert!(note.contains("then ui.revisit"));
            assert!(note.contains("a second ui.scroll_to"));
            assert!(note.contains("only after that final scroll"));
            assert!(note.contains("canonical integer from the field's actual Source value"));
        }

        for (control, form) in [
            ("Alert threshold", "Profile"),
            ("Alert threshold", "Account"),
            ("提醒阈值", "偏好设置"),
            ("Display name", "Profile"),
        ] {
            let note = scoped_native_planning_note(
                &scope(control, form),
                Some(app),
                std::slice::from_ref(&pack),
            );
            assert!(!note.contains("typed integer"));
            assert!(!note.contains("scroll-preferences"));
        }
    }

    #[test]
    fn model_call_admission_requires_the_exact_pending_request() {
        let mut engine = Engine::new(
            EngineConfig::new(TaskConfig::new(vec![])),
            Storage::open_in_memory().unwrap(),
        );
        let request = engine
            .handle(
                100,
                Input::Operator(OperatorCommand::CreateTask {
                    text: "inspect".into(),
                }),
            )
            .into_iter()
            .find_map(|output| match output {
                Output::Plan(request) => Some(*request),
                _ => None,
            })
            .expect("task creation opens its goal-compile request");

        let mut extended_deadline = request.clone();
        extended_deadline.deadline_ms += 1;
        assert!(
            engine
                .model_call_started(101, &extended_deadline, "open_ai", "fixture-model")
                .is_err()
        );
        let mut changed_context = request.clone();
        changed_context.context.notes.push("forged context".into());
        assert!(
            engine
                .model_call_started(101, &changed_context, "open_ai", "fixture-model")
                .is_err()
        );
        assert!(
            engine
                .model_call_started(101, &request, "open_ai", "fixture-model")
                .is_ok()
        );

        let calls: u64 = engine
            .storage()
            .read()
            .query_row("SELECT COUNT(*) FROM model_calls", [], |row| row.get(0))
            .unwrap();
        assert_eq!(calls, 1);
        let reason: Option<String> = engine
            .storage()
            .read()
            .query_row("SELECT reason FROM model_calls", [], |row| row.get(0))
            .unwrap();
        assert_eq!(reason.as_deref(), Some("goal_compile"));
    }

    #[test]
    fn scoped_create_persists_trusted_app_binding_and_sends_scope_to_planner() {
        let profile = BrowserPageProfile::FixtureBackgroundOption;
        let mut config = EngineConfig::new(TaskConfig::new(vec![
            crate::environment::pack::AppPack::browser_fixture(),
        ]));
        config.browser_page_profile = Some(profile);
        let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());
        let allowed_scope = AllowedControlScope {
            application: "Browser fixture".into(),
            targets: vec![AllowedControlTarget {
                control: "Background option".into(),
                group: None,
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        };
        let mut unknown_application = allowed_scope.clone();
        unknown_application.application = "Unknown application".into();
        let rejected = engine.handle(
            99,
            Input::Operator(OperatorCommand::CreateScopedTask {
                objective: "turn off the background option".into(),
                allowed_scope: unknown_application,
            }),
        );
        assert!(
            rejected
                .iter()
                .all(|output| !matches!(output, Output::Plan(_)))
        );
        assert!(engine.faulted().is_none());
        assert!(engine.tasks.active().is_none());

        let request = engine
            .handle(
                100,
                Input::Operator(OperatorCommand::CreateScopedTask {
                    objective: "turn off the background option".into(),
                    allowed_scope: allowed_scope.clone(),
                }),
            )
            .into_iter()
            .find_map(|output| match output {
                Output::Plan(request) => Some(*request),
                _ => None,
            })
            .expect("scoped task opens its goal-compile request");

        assert_eq!(request.context.user_text, "turn off the background option");
        assert_eq!(request.context.allowed_scope, Some(allowed_scope.clone()));
        let root = TaskStore::load_root(engine.storage().read(), request.root_task_id.as_str())
            .unwrap()
            .expect("scoped root is persisted before planning");
        assert_eq!(root.allowed_scope, Some(allowed_scope));
        assert_eq!(root.allowed_scope_app.as_deref(), Some(profile.app()));
    }

    #[test]
    fn an_invalid_scoped_request_does_not_stop_an_existing_root() {
        let mut engine = Engine::new(
            EngineConfig::new(TaskConfig::new(vec![])),
            Storage::open_in_memory().unwrap(),
        );
        let output = engine.handle(
            100,
            Input::Operator(OperatorCommand::CreateTask {
                text: "inspect the current settings".into(),
            }),
        );
        assert!(
            output
                .iter()
                .any(|output| matches!(output, Output::Plan(_)))
        );
        let active_root = engine
            .tasks
            .active()
            .expect("ordinary task remains active")
            .row
            .root_id
            .clone();

        let rejected = engine.handle(
            101,
            Input::Operator(OperatorCommand::CreateScopedTask {
                objective: "do something".into(),
                allowed_scope: AllowedControlScope {
                    application: "Unregistered app".into(),
                    targets: vec![AllowedControlTarget {
                        control: "Unknown control".into(),
                        group: None,
                        form: None,
                        disambiguator: None,
                    }],
                    no_unrelated_controls: true,
                },
            }),
        );
        assert!(
            rejected
                .iter()
                .all(|output| !matches!(output, Output::Device { .. }))
        );
        assert!(engine.faulted().is_none());
        assert_eq!(
            engine
                .tasks
                .active()
                .map(|active| active.row.root_id.clone()),
            Some(active_root)
        );
        assert_eq!(engine.tasks.queue_len(), 0);
    }

    #[test]
    fn frozen_admission_retains_metadata_without_sending_it_to_the_planner() {
        let profile = BrowserPageProfile::FixtureBackgroundOption;
        let mut config = EngineConfig::new(TaskConfig::new(vec![
            crate::environment::pack::AppPack::browser_fixture(),
        ]));
        config.browser_page_profile = Some(profile);
        let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());
        let frozen = FrozenTaskProfile {
            catalog_id: "a".repeat(64),
            selector: "b".repeat(64),
            source_freeze_sha256: "c".repeat(64),
            source_request_sha256: "d".repeat(64),
            goals_sha256: "e".repeat(64),
            suite: "fixture".into(),
            task_id: "SET-02".into(),
            seed: 1,
        };
        let scope = AllowedControlScope {
            application: "Browser fixture".into(),
            targets: vec![AllowedControlTarget {
                control: "Background option".into(),
                group: None,
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        };
        let mut invalid = frozen.clone();
        invalid.catalog_id = "A".repeat(64);
        assert!(
            engine
                .handle(
                    1,
                    Input::Operator(OperatorCommand::CreateFrozenTask {
                        objective: "Turn off the background option".into(),
                        allowed_scope: scope.clone(),
                        profile: invalid
                    })
                )
                .iter()
                .all(|output| !matches!(output, Output::Plan(_)))
        );
        assert!(engine.faulted().is_none());
        assert!(engine.tasks.active().is_none());
        let request = engine
            .handle(
                2,
                Input::Operator(OperatorCommand::CreateFrozenTask {
                    objective: "Turn off the background option".into(),
                    allowed_scope: scope.clone(),
                    profile: frozen.clone(),
                }),
            )
            .into_iter()
            .find_map(|output| {
                if let Output::Plan(request) = output {
                    Some(request)
                } else {
                    None
                }
            })
            .unwrap();
        let json = serde_json::to_value(&request.context).unwrap();
        for key in [
            "frozen_profile",
            "selector",
            "catalog_id",
            "initial_state",
            "evaluator_truth",
        ] {
            assert!(json.get(key).is_none());
        }
        assert_eq!(request.context.allowed_scope, Some(scope));
        let row = TaskStore::load_root(engine.storage().read(), request.root_task_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(row.frozen_profile, Some(frozen));
    }
}
