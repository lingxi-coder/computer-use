//! A running root: what to do next, and who decides (design 13 and 20.1).
//!
//! Each round the engine evaluates the frozen goal over the evidence, and if
//! it does not hold picks the next plan step. A step with one clear way to run
//! goes straight to admission as the plan's own decision. A step with a real
//! branch — several same-named controls, a dialog nobody planned for — is put
//! to a decision backend as a choice among candidates that were already
//! allowed. Either way it reaches the device only through the one admission.

use interaction_contracts::desktop::binding::{
    BindRequest, BindScope, BindingOutcome, TargetDescription, instance_fingerprint,
    normalize_label,
};
use interaction_contracts::desktop::decision::{
    Candidate, DecisionFailure, DecisionSource, Precondition,
};
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{CandidateId, RequestId, SemanticTargetRef};
use interaction_contracts::desktop::operator::RunMode;
use interaction_contracts::desktop::planning::Subgoal;
use interaction_contracts::desktop::task::{ExpectedValue, RootState, ScopedCommitKind};
use interaction_contracts::desktop::verification::TriState;

use super::{Engine, EngineError, Output, Step};
use crate::environment::read::focused_window_id;
use crate::environment::state_core::EnvSnapshot;
use crate::execution::{DeviceCommand, Expect};
use crate::ports::DecisionLifecycle;
use crate::reaction::candidates::{
    Basis, Resolution, Scope, bind_layer_controls, build, layer_title, modal_in_front,
    scoped_radio_choice_values, scoped_revisit_plan_is_allowed,
};
use crate::reaction::compiler::{CompileFailure, CompileInput, compile};
use crate::reaction::decisions::{Control, DecisionBasis, Dropped, OpenSpec, Verdict};
use crate::reaction::owned_text::OwnedDocumentActionPlan;
use crate::reaction::skills::{scoped_radio_choice_control, scoped_revisit_primary_binding_object};
use crate::verification::{EvidenceContext, ScopedCommitReceiptStatus, Verdict as GoalVerdict};

enum Verified {
    Completed,
    Pending(Box<GoalVerdict>),
}

const SCOPED_AUTO_SAVE_SETTLE_MS: u64 = 800;
const SCOPED_AUTO_SAVE_POLL_INTERVAL_MS: u64 = 400;
const SCOPED_AUTO_SAVE_MAX_WAIT_MS: u64 = 3_000;

impl Engine {
    /// A completed owned-document action may finish its already-authorized
    /// read-only revisit after the device restarts with input unavailable.
    /// This is deliberately not an execution profile: it only permits the
    /// existing owned-revisit state machine to consume its independent Source
    /// proof, and never reaches normal planning or attempt dispatch.
    fn owned_revisit_read_only_continuation(&self) -> bool {
        let Some(progress) = self.owned_revisit.as_ref() else {
            return false;
        };
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let Some(active) = self.tasks.active() else {
            return false;
        };
        let Some(manifest) = self.device.manifest() else {
            return false;
        };

        let challenge = &progress.challenge;
        let phase_is_read_only = match progress.phase {
            super::owned_revisit::OwnedRevisitPhase::HostCompletion => {
                progress.completion.is_none()
            }
            super::owned_revisit::OwnedRevisitPhase::SourceProof => {
                progress.completion.as_ref().is_some_and(|completion| {
                    completion.version == 1
                        && completion.transition_id == challenge.transition_id
                        && completion.root_task_id == challenge.root_task_id
                        && completion.task_revision == challenge.task_revision
                        && completion.root_epoch == challenge.root_epoch
                        && completion.new_device_session_epoch
                            == challenge.expected_device_session_epoch
                        && completion.clock_domain == challenge.clock_domain
                })
            }
            _ => false,
        };

        run.effect_verification_only
            && run.attempt.is_none()
            && phase_is_read_only
            && run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative
            && manifest.execution_domain == run.goal.execution_domain
            && manifest.backend == interaction_contracts::desktop::device::BackendKind::Macos
            && manifest.input
                == interaction_contracts::desktop::device::CapabilityState::Unavailable
            && manifest.max_uninterruptible_ms == 0
            && manifest.identity.session_epoch == challenge.expected_device_session_epoch
            && self.now_ms < challenge.deadline_ms
            && run.goal.root_task_id == challenge.root_task_id
            && run.goal.task_revision == challenge.task_revision
            && run.grant.root_epoch == challenge.root_epoch
            && active.row.root_id == challenge.root_task_id
            && active.row.task_revision == challenge.task_revision
            && active.row.root_epoch == challenge.root_epoch
            && !active.row.cancel_requested
    }

    /// Only the frozen row-selection step may use the independent table census.
    /// Navigation and ordinary controls retain the whole-structure gate.
    pub(super) fn row_root_evidence_usable(&self) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let Some(row) = run.goal.target.row.as_ref() else {
            return false;
        };
        if run.goal.scope.app != run.pack.bundle_id
            || run.attempt.as_ref().is_some_and(|attempt| {
                attempt.skill.skill() != crate::reaction::skills::Skill::SelectRow
            })
            || (!run.effect_verification_only
                && self
                    .next_subgoal()
                    .is_none_or(|step| step.skill != "ui.select_row"))
        {
            return false;
        }
        self.state
            .row_scope_usable(&row.table_scope, self.now_ms, self.cfg.evidence_max_age_ms)
            && self
                .state
                .latest()
                .is_some_and(|env| env.snapshot.app.bundle_id == run.goal.scope.app)
    }

    fn owned_root_evidence_usable(&self) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let interaction_contracts::desktop::task::ExpectedValue::Text { value } = &run.goal.desired
        else {
            return false;
        };
        let next_subgoal = if !run.effect_verification_only {
            let Some(subgoal) = self.next_subgoal() else {
                return false;
            };
            if subgoal.skill != "ui.enter_text"
                && (subgoal.skill != "ui.save_document" || !self.cfg.allow_owned_document_save)
                || subgoal.scroll.is_some()
                || subgoal.select.is_some()
                || subgoal
                    .target
                    .as_ref()
                    .is_some_and(|target| target != &run.goal.target.semantic_target)
            {
                return false;
            }
            Some(subgoal)
        } else {
            None
        };
        if value.len() > 2048 || value.contains('\0') {
            return false;
        }
        if next_subgoal.as_ref().is_some_and(|subgoal| subgoal.skill == "ui.enter_text")
            && interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof::
                validate_setter_value(value)
                .is_err()
        {
            return false;
        }
        let Some(proof) = self.owned_document_proof_for(&run.goal.target.semantic_target) else {
            return false;
        };
        if next_subgoal
            .as_ref()
            .is_some_and(|subgoal| subgoal.skill == "ui.save_document")
        {
            let ExpectedValue::Text { value } = &run.goal.desired else {
                return false;
            };
            return self.cfg.allow_owned_document_save
                && OwnedDocumentActionPlan::save(proof, value.clone()).is_ok();
        }
        true
    }

    /// The situation a decision is made in, when there is evidence.
    pub(super) fn current_basis(&self) -> Option<DecisionBasis> {
        let env = self.state.latest()?;
        let active = self.tasks.active()?;
        let owned_document = self.owned_root_evidence_usable().then(|| {
            let run = self
                .run
                .as_ref()
                .expect("owned evidence requires an active root");
            self.owned_document_proof_for(&run.goal.target.semantic_target)
                .expect("owned evidence was just checked")
        });
        Some(DecisionBasis::capture_with_owned_document(
            env,
            &self.bindings,
            active.row.plan_revision,
            active.row.task_revision,
            active.row.root_epoch,
            owned_document,
        ))
    }

    // ---- Getting started -------------------------------------------------------------------

    /// READY to RUNNING needs the operator's mode, a device that can take
    /// input, the root registered with it, and current evidence.
    pub(super) fn step_ready(&mut self) -> Step {
        if self.mode != RunMode::Execute || self.run.is_none() {
            return Ok(false);
        }
        if !self.execution_profile_matches_goal()
            || self.device.behind_on_stops()
            || !self.device.lease_established()
        {
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
        if !(self.goal_evidence_usable()
            || self.owned_root_evidence_usable()
            || self.row_root_evidence_usable()
            || self.activation_pending() && self.window_evidence_usable())
        {
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
        if self.owned_revisit_read_only_continuation() {
            return self.advance_owned_revisit();
        }
        if !self.execution_profile_matches_goal() {
            if self.tasks.active_state() == Some(RootState::Running) {
                self.pause_root("the selected execution-domain capability changed")?;
                return Ok(true);
            }
            return Ok(false);
        }
        if self.owned_revisit.is_some() {
            return self.advance_owned_revisit();
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
        if self.activation_pending() {
            return self.start_window_attempt();
        }
        if self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
        }) {
            return self.step_headless_browser();
        }
        if !self.goal_evidence_usable()
            && !self.owned_root_evidence_usable()
            && !self.row_root_evidence_usable()
        {
            if self.run.as_ref().is_some_and(|run| run.wait.is_some()) {
                return Ok(false);
            }
            let now = self.now_ms;
            return self.await_source(
                now,
                "there is no current evidence",
                !self.state.requires_frames() && !self.computer.is_active(),
            );
        }
        // The goal can only be read through its binding, so that comes first:
        // a baseline taken before the target is bound would be unknown, and so
        // would be the conclusion after a page was entered again.
        self.maintain_goal_binding();
        let verdict = match self.verify()? {
            Verified::Completed => {
                self.retire_computer_goal_satisfied();
                return Ok(true);
            }
            Verified::Pending(verdict) => *verdict,
        };
        if let Some(result) = self.wait_for_scoped_auto_commit_receipt()? {
            return Ok(result);
        }
        if self.native_context_settling() {
            return Ok(false);
        }
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.effect_verification_only)
        {
            if self.bump_stalls() <= 1 {
                return self.request_refresh(
                    self.now_ms.saturating_add(1),
                    "checking the completed input's goal evidence again",
                );
            }
            self.note(
                "the completed input's business effect could not be proved; no input is repeated",
            );
            self.abandon_work()?;
            self.tasks.input_ended_unverified(
                &mut self.storage,
                self.now_ms,
                "the completed input's business effect could not be proved",
            )?;
            return Ok(true);
        }
        if self.computer.is_active() {
            return self.computer_drive();
        }
        if self.cfg.computer_proposals_enabled
            && self
                .next_subgoal()
                .is_some_and(|subgoal| subgoal.skill == "ui.activate")
            && self.computer_drive()?
        {
            return Ok(true);
        }
        if self.computer_blocks_current_subgoal() {
            self.pause_root(
                "the computer action scope ended; an explicit operator decision is required",
            )?;
            return Ok(true);
        }
        self.choose_next(verdict)
    }

    /// Execute the one Host-frozen browser checkbox skill through the same
    /// Running root, DecisionBook, effect gate, and durable Attempt admission
    /// path used by a direct native plan step.
    fn step_headless_browser(&mut self) -> Step {
        if !self.goal_evidence_usable() {
            if self.run.as_ref().is_some_and(|run| run.wait.is_some()) {
                return Ok(false);
            }
            return self.await_source(
                self.now_ms,
                "the browser goal needs a fresh read from its installed page scope",
                true,
            );
        }
        let Some((profile, target, desired)) = self.run.as_ref().and_then(|run| {
            let profile = run.goal.browser_page_profile?;
            let ExpectedValue::Bool { value } = &run.goal.desired else {
                return None;
            };
            Some((profile, run.goal.target.semantic_target.clone(), *value))
        }) else {
            self.fail_root("the browser goal has no frozen boolean page profile")?;
            return Ok(true);
        };
        let Some(subgoal) = self.next_subgoal() else {
            self.fail_root("the browser plan ended before its checkbox goal was verified")?;
            return Ok(true);
        };
        if subgoal.skill != "ui.ensure_value"
            || subgoal.target.as_ref() != Some(&target)
            || subgoal.scroll.is_some()
            || subgoal.select.is_some()
            || target.as_str() != profile.semantic_target()
        {
            self.fail_root("the browser plan does not match its frozen checkbox target")?;
            return Ok(true);
        }
        let Some(observation) = self.browser_execution_observation(profile) else {
            return self.await_source(
                self.now_ms,
                "the browser checkbox binding is no longer current",
                true,
            );
        };
        let mut parameters = std::collections::BTreeMap::new();
        parameters.insert("desired".into(), desired.to_string());
        let candidate = Candidate {
            candidate_id: CandidateId::new("browser-checkbox").expect("fixed candidate id"),
            skill: "ui.ensure_value".into(),
            semantic_target: Some(target),
            binding: Some(observation.binding_ref),
            parameters,
            effect_class: EffectClass::ReversibleEdit,
            precondition: Precondition::Executable,
            description: format!(
                "Set the fixed browser checkbox \"{}\" to {desired}",
                profile.label()
            ),
        };
        self.run_direct(candidate)
    }

    /// Bind the goal's own target as soon as it can be, and again whenever a
    /// page is entered anew. Whatever the bind answers besides "bound" is
    /// dealt with when a step needs the target.
    pub(super) fn maintain_goal_binding(&mut self) {
        let Some(env) = self.state.latest().cloned() else {
            return;
        };
        let background_ax = self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        });
        if env.snapshot.menu_context.is_some()
            || self.run.is_none()
            || (!background_ax && modal_in_front(&env))
        {
            return;
        }
        if background_ax {
            let Some((goal, pack, app)) = self.run.as_ref().map(|run| {
                (
                    run.goal.target.clone(),
                    run.pack.clone(),
                    run.goal.scope.app.clone(),
                )
            }) else {
                return;
            };
            let Some(control) = pack.control(&goal.semantic_target) else {
                self.bindings.invalidate(
                    &goal.semantic_target,
                    "BackgroundAX target is not a registered control",
                );
                return;
            };
            if goal.row.is_some()
                || env.snapshot.app.bundle_id != app
                || env.snapshot.app.bundle_id != pack.bundle_id
                || normalize_label(&goal.label) != normalize_label(&control.label)
                || goal.role_hint.as_deref() != Some(control.role.as_str())
                || pack.effect_class(&control.role, &control.label)
                    != interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
                || !self.background_ax_evidence_usable(None)
            {
                self.bindings.invalidate(
                    &goal.semantic_target,
                    "BackgroundAX target or fresh Host-scoped Source context is unavailable",
                );
                return;
            }
            let result = self.bindings.bind_background_ax(
                &env,
                &goal,
                self.now_ms,
                self.cfg.evidence_max_age_ms.min(2_000),
            );
            if let Err(reason) = result {
                self.bindings.invalidate(&goal.semantic_target, &reason);
            }
            return;
        }
        let target = self
            .run
            .as_ref()
            .map(|run| run.goal.target.semantic_target.clone())
            .expect("checked above");
        let description = self.goal_description();
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.goal.target.row.is_some())
        {
            self.resolve_row(&target, &env);
            return;
        }
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.pack.keyboard_toggle(&target).is_some())
        {
            if let Some(binding) = self.bind_keyboard_goal(&env)
                && let Some(run) = self.run.as_mut()
            {
                run.verification.visits.set_target_page(env.page.clone());
                run.verification
                    .visits
                    .note_bound(binding.observed_ms, env.page.as_deref());
            } else {
                self.bindings.invalidate(
                    &target,
                    "current keyboard receiver or read-only kernel/root facts are unavailable",
                );
            }
            return;
        }
        if let Resolution::Bound(binding) = self.resolve(&target, description, &env) {
            // A hidden goal may first be bound by a container-scoped skill.
            // Reusing that binding still establishes its page; rebinding after
            // return must use the binding's actual evidence time, never now.
            if let Some(run) = self.run.as_mut() {
                run.verification.visits.set_target_page(env.page.clone());
                run.verification
                    .visits
                    .note_bound(binding.observed_ms, env.page.as_deref());
            }
        }
    }

    /// Input binding comes from current actual focus; a completed keyboard
    /// transaction may retain only its descriptor for independent UI reads.
    pub(super) fn bind_keyboard_goal(
        &mut self,
        env: &EnvSnapshot,
    ) -> Option<crate::environment::binding::Binding> {
        if !self.evidence_usable()
            || env.snapshot.validate().is_err()
            || env.snapshot.menu_context.is_some()
            || modal_in_front(env)
        {
            return None;
        }
        let run = self.run.as_ref()?;
        let target = &run.goal.target;
        let definition = run.pack.keyboard_toggle(&target.semantic_target)?;
        if !matches!(
            run.goal.desired,
            interaction_contracts::desktop::task::ExpectedValue::Bool { .. }
        ) || normalize_label(&target.label) != normalize_label(&definition.label)
            || target
                .role_hint
                .as_ref()
                .is_some_and(|role| role != &definition.role)
        {
            return None;
        }
        let readonly = run.keyboard_result_context.as_ref();
        let context = readonly.or(env.snapshot.focus_context.as_ref())?;
        if readonly.is_some() {
            // A saved descriptor only identifies a result under newly proved
            // current kernel/root facts. It grants no focus or input authority.
            let window = env.snapshot.window_context.as_ref()?;
            if window.session_epoch != env.snapshot.session_epoch
                || window.process != context.process
                || window.origin_window_id != context.origin_window_id
                || window.origin_window_frame != context.origin_window_frame
                || window.verified_source_sequence != env.snapshot.source_sequence
                || window.verified_observed_ms != env.snapshot.observed_ms
            {
                return None;
            }
        }
        if context.session_epoch != env.snapshot.session_epoch
            || context.process.pid != env.snapshot.app.pid
            || context.target_fingerprint.app != env.snapshot.app.bundle_id
            || focused_window_id(&env.snapshot) != Some(context.origin_window_id.as_str())
            || env
                .snapshot
                .windows
                .iter()
                .filter(|window| {
                    window.window_id == context.origin_window_id
                        && window.frame == context.origin_window_frame
                        && !window.modal
                        && window.owner_bundle_id == env.snapshot.app.bundle_id
                })
                .count()
                != 1
            || env
                .snapshot
                .focus_context
                .as_ref()
                .is_some_and(|current| readonly.is_some() && current.process != context.process)
        {
            return None;
        }
        if readonly.is_none()
            && (context.verified_source_sequence != env.snapshot.source_sequence
                || context.verified_observed_ms != env.snapshot.observed_ms)
        {
            return None;
        }
        let description = self.goal_description();
        let mut objects = env.snapshot.objects.iter().filter(|object| {
            object.window_id == context.origin_window_id
                && object.role == definition.role
                && normalize_label(
                    object
                        .title
                        .as_deref()
                        .or(object.description.as_deref())
                        .unwrap_or(""),
                ) == normalize_label(&description.label)
                && target.group.as_ref().is_none_or(|group| {
                    object
                        .group
                        .as_ref()
                        .is_some_and(|actual| normalize_label(group) == normalize_label(actual))
                })
                && object.enabled
                && object.in_view
                && !object.secure
                && keyboard_read_path_safe(&env.snapshot, object, context)
                && object.frame == context.target_frame
                && instance_fingerprint(&env.snapshot, object) == context.target_fingerprint
                && (readonly.is_some() || object.instance_ref == context.target_instance)
        });
        let instance = objects.next()?.instance_ref.clone();
        if objects.next().is_some() {
            return None;
        }
        let semantic = target.semantic_target.clone();
        self.bindings.bind_instance(env, semantic, &instance)
    }

    pub(super) fn maintain_keyboard_result_binding(&mut self) {
        let eligible = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| {
                !attempt.mechanical_unknown
                    && attempt.dispatch.is_none()
                    && attempt.skill.keyboard_context().is_some()
                    && attempt.skill.input_floor().is_some_and(|floor| {
                        self.state.latest_since(floor.saturating_add(1)).is_some()
                    })
            });
        if !eligible {
            return;
        }
        let Some(env) = self.state.latest().cloned() else {
            return;
        };
        match self.bind_keyboard_goal(&env) {
            Some(binding) => {
                if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
                    attempt.skill.update_keyboard_read_binding(&binding);
                }
            }
            None => {
                if let Some(run) = self.run.as_ref() {
                    self.bindings.invalidate(
                        &run.goal.target.semantic_target,
                        "fresh keyboard result lacks its current kernel/root proof",
                    );
                }
            }
        }
    }

    pub(super) fn maintain_menu_result_binding(&mut self) {
        let Some(target) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .filter(|attempt| {
                attempt.menu_opening.is_some()
                    && attempt.dispatch.is_none()
                    && !attempt.mechanical_unknown
                    && attempt.skill.menu_readonly_result_ready()
            })
            .and_then(|attempt| attempt.skill.menu_target())
            .cloned()
        else {
            return;
        };
        let Some(env) = self.state.latest().cloned() else {
            return;
        };
        if env.snapshot.menu_context.is_some() || !self.evidence_usable() {
            return;
        }
        let description = self.describe_target(&target);
        self.resolve(&target, description, &env);
    }

    /// Evaluate the frozen goal. Only here, and only when every required
    /// predicate is true, does the root complete.
    fn verify(&mut self) -> Result<Verified, EngineError> {
        let now = self.now_ms;
        let max_age_ms = self.cfg.evidence_max_age_ms;
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
            now_ms: now,
            max_age_ms,
            state,
            bindings,
            pack: &run.pack,
            goal: &run.goal,
        };
        if run.verification.last_input_end_ms().is_none() {
            let status = run.verification.evaluate(&run.goal.required, &ctx);
            run.verification.note_baseline(status);
            run.verification.capture_scoped_commit_baseline(&ctx);
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

    /// Wait only for the visible autosave receipt before the Host-frozen
    /// revisit step. This is a Source-only wait; the observation budget and
    /// root deadline bound it, and a missing receipt never re-enters planning.
    fn wait_for_scoped_auto_commit_receipt(&mut self) -> Result<Option<bool>, EngineError> {
        let Some(run) = self.run.as_ref() else {
            return Ok(None);
        };
        let Some(commit) = run.goal.scoped_commit.as_ref() else {
            return Ok(None);
        };
        if commit.receipt.kind != ScopedCommitKind::Automatic
            || run.goal.scoped_revisit.is_none()
            || self
                .next_subgoal()
                .is_none_or(|subgoal| subgoal.skill != "ui.revisit")
        {
            return Ok(None);
        }
        let Some(input_end_ms) = run.verification.last_input_end_ms() else {
            return self.end_scoped_commit_unverified(
                "the automatic-save target changed without a recorded input",
            );
        };
        let ctx = EvidenceContext {
            state: &self.state,
            bindings: &self.bindings,
            pack: &run.pack,
            goal: &run.goal,
            now_ms: self.now_ms,
            max_age_ms: self.cfg.evidence_max_age_ms,
        };
        if run.verification.scoped_commit_receipt_status(&ctx)
            == ScopedCommitReceiptStatus::Confirmed
        {
            return Ok(None);
        }
        let earliest_receipt_ms = input_end_ms.saturating_add(SCOPED_AUTO_SAVE_SETTLE_MS);
        let local_deadline = input_end_ms.saturating_add(SCOPED_AUTO_SAVE_MAX_WAIT_MS);
        let root_deadline = run.goal.budget.deadline_ms;
        let poll_deadline = local_deadline.min(root_deadline);
        let latest_observed_ms = self.state.latest().map(|env| env.snapshot.observed_ms);
        if latest_observed_ms.is_some_and(|observed| observed >= poll_deadline)
            || self.now_ms >= poll_deadline
        {
            return self.end_scoped_commit_unverified(
                "the automatic-save receipt did not advance within the bounded Source wait",
            );
        }
        let settle_floor = latest_observed_ms
            .map(|observed| {
                earliest_receipt_ms.max(observed.saturating_add(SCOPED_AUTO_SAVE_POLL_INTERVAL_MS))
            })
            .unwrap_or(earliest_receipt_ms)
            .min(poll_deadline);
        self.await_source(
            settle_floor,
            "waiting for the automatic-save receipt in fresh Source evidence",
            true,
        )
        .map(Some)
    }

    fn end_scoped_commit_unverified(&mut self, reason: &str) -> Result<Option<bool>, EngineError> {
        self.note(reason);
        self.abandon_work()?;
        self.tasks
            .input_ended_unverified(&mut self.storage, self.now_ms, reason)?;
        Ok(Some(true))
    }

    fn choose_next(&mut self, verdict: GoalVerdict) -> Step {
        let Some(env) = self.state.latest().cloned() else {
            return Ok(false);
        };
        if let Some(run) = self.run.as_ref().filter(|run| {
            run.goal.scoped_revisit.is_some()
                || run
                    .goal
                    .scoped_commit
                    .as_ref()
                    .is_some_and(|commit| commit.action.is_some())
        }) {
            let allowed = self
                .tasks
                .active()
                .and_then(|active| active.row.plan.as_ref())
                .is_some_and(|plan| scoped_revisit_plan_is_allowed(&run.goal, &run.pack, plan));
            if !allowed {
                self.fail_root("the scoped revisit plan is outside its frozen target and steps")?;
                return Ok(true);
            }
        }
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
            group: if run.goal.allowed_scope.is_some() {
                run.goal.target.group.clone()
            } else {
                run.refined_group
                    .clone()
                    .or_else(|| run.goal.target.group.clone())
            },
            role_hint: run.goal.target.role_hint.clone(),
        }
    }

    /// How a step's target is found again in the current evidence.
    fn describe_target(&self, target: &SemanticTargetRef) -> TargetDescription {
        let run = self.run.as_ref().expect("a running root has a run");
        if target == &run.goal.target.semantic_target {
            return self.goal_description();
        }
        if let Some(action) = run
            .goal
            .scoped_commit
            .as_ref()
            .and_then(|commit| commit.action.as_ref())
            .filter(|action| &action.semantic_target == target)
        {
            return TargetDescription {
                label: action.label.clone(),
                group: action.group.clone(),
                role_hint: action.role_hint.clone(),
            };
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
        if let Some(run) = self.run.as_ref().filter(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        }) {
            let frozen = run.goal.target.clone();
            let pack = run.pack.clone();
            if target != &frozen.semantic_target
                || frozen.row.is_some()
                || env.snapshot.app.bundle_id != run.goal.scope.app
                || env.snapshot.app.bundle_id != pack.bundle_id
                || pack.control(&frozen.semantic_target).is_none_or(|control| {
                    normalize_label(&frozen.label) != normalize_label(&control.label)
                        || frozen.role_hint.as_deref() != Some(control.role.as_str())
                        || pack.effect_class(&control.role, &control.label)
                            != interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
                })
                || !self.background_ax_evidence_usable(None)
            {
                return Resolution::Unavailable(
                    "BackgroundAX resolution is outside the frozen registered target scope".into(),
                );
            }
            return self
                .bindings
                .bind_background_ax(
                    env,
                    &frozen,
                    self.now_ms,
                    self.cfg.evidence_max_age_ms.min(2_000),
                )
                .map(|binding| Resolution::Bound(Box::new(binding)))
                .unwrap_or_else(Resolution::Unavailable);
        }
        if self.run.as_ref().is_some_and(|run| {
            target == &run.goal.target.semantic_target && run.goal.target.row.is_some()
        }) {
            self.bindings.invalidate(
                target,
                "row target cannot be resolved through generic role/label binding",
            );
            return Resolution::Unavailable(
                "row target requires its dedicated viewport binding and input proof".into(),
            );
        }
        let radio_goal = self.run.as_ref().and_then(|run| {
            (target == &run.goal.target.semantic_target
                && scoped_radio_choice_values(&run.goal, &run.pack).is_some())
            .then(|| (run.goal.clone(), run.pack.clone()))
        });
        if let Some((goal, pack)) = radio_goal {
            if !self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
                || env.snapshot.observed_ms > self.now_ms
            {
                self.bindings.invalidate(
                    target,
                    "radio-group target needs a current complete Source read",
                );
                return Resolution::Unavailable(
                    "radio-group target needs a current complete Source read".into(),
                );
            }
            let Some(radio) = scoped_radio_choice_control(&goal, &pack, env) else {
                self.bindings.invalidate(
                    target,
                    "the exact registered radio group and desired child are not uniquely bound",
                );
                return Resolution::Unavailable(
                    "the exact registered radio group and desired child are not uniquely bound"
                        .into(),
                );
            };
            let Some(binding) =
                self.bindings
                    .bind_instance(env, target.clone(), &radio.desired.instance_ref)
            else {
                self.bindings.invalidate(
                    target,
                    "the registered desired radio child has no Native Device binding proof",
                );
                return Resolution::Unavailable(
                    "the registered desired radio child has no Native Device binding proof".into(),
                );
            };
            if scoped_revisit_primary_binding_object(&goal, &pack, env, &binding).is_none() {
                self.bindings.invalidate(
                    target,
                    "the child binding does not prove the frozen radio-group target",
                );
                return Resolution::Unavailable(
                    "the child binding does not prove the frozen radio-group target".into(),
                );
            }
            if let Some(run) = self.run.as_mut() {
                run.verification.visits.set_target_page(env.page.clone());
                run.verification
                    .visits
                    .note_bound(env.snapshot.observed_ms, env.page.as_deref());
            }
            return Resolution::Bound(Box::new(binding));
        }
        if let Some(binding) = self.bindings.current(target) {
            return Resolution::Bound(Box::new(binding.clone()));
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
            choice: if is_goal_target && run.goal.allowed_scope.is_none() {
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
                Resolution::Bound(Box::new(binding))
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

    /// Resolve a frozen row semantic through the typed, viewport-scoped
    /// resolver. This intentionally never installs a generic Binding.
    fn resolve_row(&mut self, target: &SemanticTargetRef, env: &EnvSnapshot) -> Resolution {
        let Some(run) = self.run.as_ref() else {
            return Resolution::Unavailable("there is no running row task".into());
        };
        if target != &run.goal.target.semantic_target
            || run.goal.target.row.is_none()
            || env.snapshot.app.bundle_id != run.goal.scope.app
            || env.snapshot.app.bundle_id != run.pack.bundle_id
        {
            return Resolution::Unavailable(
                "the row target is outside its frozen task and application scope".into(),
            );
        }
        if !self.state.row_scope_usable(
            &run.goal
                .target
                .row
                .as_ref()
                .expect("row goal was checked")
                .table_scope,
            self.now_ms,
            self.cfg.evidence_max_age_ms,
        ) {
            return Resolution::Unavailable(
                "row selection needs healthy fresh Source evidence".into(),
            );
        }
        let target = run.goal.target.clone();
        match self
            .bindings
            .bind_row(env, &target, self.now_ms, self.cfg.evidence_max_age_ms)
        {
            Ok(binding) => {
                if let Some(run) = self.run.as_mut() {
                    run.verification.visits.set_target_page(env.page.clone());
                    run.verification
                        .visits
                        .note_bound(binding.observed_ms, env.page.as_deref());
                }
                Resolution::Row(Box::new(binding))
            }
            Err(reason) => Resolution::Unavailable(reason),
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
        let (input_target, description) = if let Some(scroll) = &subgoal.scroll {
            (
                scroll.container.semantic_target.clone(),
                TargetDescription {
                    label: scroll.container.label.clone(),
                    group: scroll.container.group.clone(),
                    role_hint: scroll.container.role_hint.clone(),
                },
            )
        } else {
            (target.clone(), self.describe_target(&target))
        };
        let owned_target = self.run.as_ref().and_then(|run| {
            let expected_text = match &run.goal.desired {
                interaction_contracts::desktop::task::ExpectedValue::Text { value } => value,
                _ => return None,
            };
            if !matches!(subgoal.skill.as_str(), "ui.enter_text" | "ui.save_document")
                || (subgoal.skill == "ui.save_document" && !self.cfg.allow_owned_document_save)
                || subgoal.scroll.is_some()
                || subgoal.select.is_some()
                || input_target != run.goal.target.semantic_target
                || subgoal
                    .target
                    .as_ref()
                    .is_some_and(|target| target != &run.goal.target.semantic_target)
                || (subgoal.skill == "ui.enter_text"
                    && interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof::
                        validate_setter_value(expected_text)
                        .is_err())
            {
                return None;
            }
            let proof = self.owned_document_proof_for(&run.goal.target.semantic_target)?;
            if subgoal.skill == "ui.save_document"
                && OwnedDocumentActionPlan::save(proof.clone(), expected_text.clone()).is_err()
            {
                return None;
            }
            Some(Resolution::OwnedDocument(proof.binding))
        });
        let row_goal = self
            .run
            .as_ref()
            .is_some_and(|run| run.goal.target.row.is_some());
        let resolution = if let Some(owned) = owned_target {
            owned
        } else if row_goal {
            if !matches!(subgoal.skill.as_str(), "ui.select_row" | "ui.revisit")
                || input_target != target
                || target
                    != self
                        .run
                        .as_ref()
                        .expect("row goal has a run")
                        .goal
                        .target
                        .semantic_target
                || subgoal.scroll.is_some()
                || subgoal.select.is_some()
            {
                self.fail_root("the row plan step does not match the frozen row workflow")?;
                return Ok(true);
            }
            self.resolve_row(&target, env)
        } else {
            self.resolve(&input_target, description, env)
        };
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.goal.allowed_scope.is_some())
            && matches!(&resolution, Resolution::Ambiguous(_))
        {
            self.fail_root("the allowed control is ambiguous in the current Source read")?;
            return Ok(true);
        }
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
            Resolution::Row(_) => {
                if let Some(step) = candidates
                    .iter()
                    .find(|candidate| candidate.skill == subgoal.skill)
                {
                    match &step.precondition {
                        Precondition::Executable => return self.run_direct(step.clone()),
                        Precondition::NeedsObservation => {
                            let now = self.now_ms;
                            return self.request_refresh(
                                now,
                                "the row selection needs a fresh Source read",
                            );
                        }
                        Precondition::Rejected { reason } => {
                            let reason = format!("the row plan step is not allowed: {reason}");
                            self.fail_root(&reason)?;
                            return Ok(true);
                        }
                    }
                }
            }
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
            Resolution::OwnedDocument(_) => {
                if let Some(step) = candidates
                    .iter()
                    .find(|candidate| candidate.skill == subgoal.skill)
                {
                    match &step.precondition {
                        Precondition::Executable => return self.run_direct(step.clone()),
                        Precondition::NeedsObservation => {
                            let now = self.now_ms;
                            return self.request_refresh(
                                now,
                                "the owned text step needs current evidence",
                            );
                        }
                        Precondition::Rejected { reason } => {
                            let reason = format!("the owned text step is not allowed: {reason}");
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
        self.clear_source_table_scope(false);
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
        self.clear_source_table_scope(false);
        if let Some(transition_id) = self.owned_revisit_transition_id().map(str::to_owned) {
            self.cancel_owned_revisit(&transition_id, self.now_ms, "the root deadline passed")
                .map_err(|error| {
                    EngineError::Store(crate::infrastructure::storage::StoreError::Corrupt(
                        error.to_string(),
                    ))
                })?;
        }
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
        if self.run.is_some() {
            self.clear_source_table_scope(false);
        }
        self.cancel_computer_lane("the root task ended");
        let ended = self.tasks.last_finished().map(|row| row.state);
        let Some(run) = self.run.take() else {
            return;
        };
        self.bindings.invalidate_all("the task ended");
        self.permit_bindings.clear();
        self.permit_owned_proofs.clear();
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
        if self.owned_revisit.is_some()
            && self
                .tasks
                .active_state()
                .is_some_and(|state| self.deadline_passed(state))
        {
            self.expire_root()?;
            return Ok(());
        }
        if self
            .owned_revisit
            .as_ref()
            .is_some_and(|progress| self.now_ms >= progress.challenge.deadline_ms)
            && matches!(
                self.tasks.active_state(),
                Some(RootState::Running | RootState::WaitingEvidence)
            )
        {
            self.advance_owned_revisit()?;
            return Ok(());
        }
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

    pub(super) fn renew_lease(&mut self) {
        if self.owned_revisit.is_some() || self.device.manifest().is_none() {
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

/// A retained result descriptor never exempts the new actual ancestry from
/// the ordinary secure/root boundary, even when focus has left the control.
pub(super) fn keyboard_read_path_safe(
    snapshot: &interaction_contracts::desktop::source::StructureSnapshot,
    target: &interaction_contracts::desktop::source::UiObject,
    context: &interaction_contracts::desktop::source::FocusContext,
) -> bool {
    let mut current = target;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..32 {
        if !seen.insert(current.instance_ref.clone())
            || current.secure
            || current.role == "AXSecureTextField"
            || current.subrole.as_deref() == Some("AXSecureTextField")
            || current.window_id != context.origin_window_id
            || matches!(
                current.role.as_str(),
                "AXMenu" | "AXMenuItem" | "AXSheet" | "AXDialog"
            )
        {
            return false;
        }
        let Some(parent) = &current.parent else {
            return current.role == "AXWindow"
                && current.subrole.as_deref() == Some("AXStandardWindow")
                && current.frame == context.origin_window_frame;
        };
        let mut parents = snapshot
            .objects
            .iter()
            .filter(|object| &object.instance_ref == parent);
        let Some(next) = parents.next() else {
            return false;
        };
        if parents.next().is_some() {
            return false;
        }
        current = next;
    }
    false
}
