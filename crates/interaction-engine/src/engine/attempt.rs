//! One attempt, from admission to its end (design 11.2, 11.3, 14.3 and 15).
//!
//! Admission is the only way an attempt starts: the effect gate is checked
//! again, then TaskRuntime records quota, attempt and the decision's
//! consumption in one transaction. After that a skill asks for bounded inputs
//! and each one travels the same way: current evidence to the device, a permit
//! on that evidence, then the intent. A receipt says what reached the
//! application; it is never taken as proof of the effect.

use interaction_contracts::desktop::binding::{
    BindingProofScope, LayoutIndependentSpecialKey, MenuOpeningRef,
    derive_background_ax_binding_evidence, instance_fingerprint,
};
use interaction_contracts::desktop::browser::BrowserCheckboxBinding;
use interaction_contracts::desktop::browser_execution::BrowserAction;
use interaction_contracts::desktop::decision::{Candidate, DecisionSource};
use interaction_contracts::desktop::device::{
    CommandAck, ExecutionObservation, GrantOutcome, QueryReceipt, Receipt, ReceiptStatus,
    SourceFreshness,
};
use interaction_contracts::desktop::effect::{Admission, EffectContext, admit};
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::ids::{
    AttemptId, DecisionId, ObjectRef, RequestId, SemanticTargetRef,
};
use interaction_contracts::desktop::operator::ActionSummary;
use interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof;
use interaction_contracts::desktop::source::{SourceWindowContext, TargetContext};
use interaction_contracts::desktop::task::{
    AdmitAttempt, AdmitOutcome, AdmitRejection, AttemptQuota, AttemptState, ConfirmationScope,
    ExpectedValue, GoalSpec,
};
use sha2::{Digest, Sha256};

use super::owned_revisit::{OwnedRevisitStore, OwnedSaveEvidence};
use super::{Engine, EngineError, Step};
use crate::environment::binding::{Binding, RowBinding};
use crate::environment::browser::BrowserLiveBinding;
use crate::environment::read::{focused_window_id, object_for, value_text};
use crate::environment::state_core::EnvSnapshot;
use crate::execution::{
    BrowserIntentSpec, DeviceCommand, Dispatch, DispatchPlan, Expect, IntentSpec,
    OwnedDocumentIntentSpec, Stage,
};
use crate::infrastructure::storage::StoreError;
use crate::ports::DecisionLifecycle;
use crate::reaction::candidates::{
    Resolution, Scope, gate, scoped_choice_values, scoped_radio_choice_values,
    scoped_revisit_plan_is_allowed,
};
use crate::reaction::decisions::Held;
use crate::reaction::owned_text::{OwnedDocumentActionKind, OwnedDocumentActionPlan};
use crate::reaction::skills::{
    ActionOutcome, ActionPlan, BrowserActionPlan, ExecutionPath, Skill, SkillResult, SkillRun,
    SkillStep, SkillView, scoped_commit_action_binding_object, scoped_form_number_control,
    scoped_radio_choice_control, scoped_radio_choice_spec, scoped_radio_value_from_binding,
    scoped_revisit_binding_object, scoped_revisit_primary_binding_object,
};
use crate::reaction::store::ReactionStore;
use crate::tasks::TaskError;
use crate::tasks::budget::BudgetKind;
use crate::tasks::goal::{scoped_number_form_scroll_container, scoped_number_scroll_step_matches};
use crate::tasks::store::TaskStore;

enum CandidateBinding {
    Generic(Box<Binding>),
    Row(Box<RowBinding>),
    OwnedDocument(OwnedDocumentBindingProof),
    Browser(Box<BrowserLiveBinding>),
}

impl CandidateBinding {
    fn binding_ref(&self) -> &interaction_contracts::desktop::ids::BindingRef {
        match self {
            Self::Generic(binding) => &binding.binding_ref,
            Self::Row(binding) => &binding.binding_ref,
            Self::OwnedDocument(proof) => &proof.binding,
            Self::Browser(binding) => &binding.binding_ref,
        }
    }

    fn semantic_target(&self) -> &SemanticTargetRef {
        match self {
            Self::Generic(binding) => &binding.semantic_target,
            Self::Row(binding) => &binding.semantic_target,
            Self::OwnedDocument(proof) => &proof.semantic_target,
            Self::Browser(binding) => &binding.semantic_target,
        }
    }

    fn generic(&self) -> Option<&Binding> {
        match self {
            Self::Generic(binding) => Some(binding.as_ref()),
            Self::Row(_) | Self::OwnedDocument(_) | Self::Browser(_) => None,
        }
    }
}

struct OwnedDocumentAttempt {
    admitted_proof: OwnedDocumentBindingProof,
    input_proof: Option<OwnedDocumentBindingProof>,
    value: String,
    action: OwnedDocumentActionKind,
    input_issued: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextCurrentUiRejection {
    InputFactsIncomplete,
    DeadlineExpired,
    SourceUnusable,
    SnapshotMissing,
    SnapshotOutsideDeadline,
    BindingUnavailable,
    BindingSemanticChanged,
    BindingInstanceChanged,
    BindingFingerprintChanged,
    BindingPageChanged,
    Verification(crate::verification::TextValueRejection),
}

impl TextCurrentUiRejection {
    fn code(self) -> &'static str {
        match self {
            Self::InputFactsIncomplete => "input_facts_incomplete",
            Self::DeadlineExpired => "deadline_expired",
            Self::SourceUnusable => "source_unusable",
            Self::SnapshotMissing => "snapshot_missing",
            Self::SnapshotOutsideDeadline => "snapshot_outside_deadline",
            Self::BindingUnavailable => "binding_unavailable",
            Self::BindingSemanticChanged => "bound_semantic_changed",
            Self::BindingInstanceChanged => "bound_instance_changed",
            Self::BindingFingerprintChanged => "bound_fingerprint_changed",
            Self::BindingPageChanged => "bound_page_changed",
            Self::Verification(reason) => reason.code(),
        }
    }
}

/// The attempt that is running now.
pub(crate) struct AttemptRun {
    pub id: AttemptId,
    pub subgoal: String,
    pub skill: SkillRun,
    pub admitted_effect_class: interaction_contracts::desktop::effect::EffectClass,
    pub(super) computer_step: Option<super::computer::ComputerAttemptStep>,
    /// Only a matching known completed input can advance a computer action.
    pub computer_input_completed: bool,
    /// It acts on the goal's own target.
    pub goal_target: bool,
    pub quota: AttemptQuota,
    pub observations_used: u32,
    pub dispatch: Option<Dispatch>,
    owned_document: Option<OwnedDocumentAttempt>,
    /// The exact AX text setter target and source scope that preceded input.
    pub last_input_path: Option<ExecutionPath>,
    pub last_input_binding: Option<Binding>,
    pub last_input_text: Option<String>,
    pub last_input_window: Option<SourceWindowContext>,
    pub last_input_intent_id: Option<String>,
    /// Only populated from an exact matching receipt with an actual input end.
    pub text_input_end_ms: Option<u64>,
    pub text_input_span: Option<interaction_contracts::desktop::verification::TimeSpan>,
    /// Keep this exact Attempt alive while read-only text effect evidence is
    /// retried within its original deadline and observation quota.
    pub text_current_ui_pending: bool,
    pub text_current_ui_last_snapshot: Option<RequestId>,
    text_current_ui_last_rejection: Option<TextCurrentUiRejection>,
    pub browser_current_ui_pending: bool,
    pub browser_current_ui_no_input: bool,
    pub browser_current_ui_fence_ms: Option<u64>,
    pub browser_current_ui_last_snapshot: Option<String>,
    browser_current_ui_last_rejection: Option<crate::verification::BrowserCurrentUiRejection>,
    pub browser_original_binding: Option<BrowserCheckboxBinding>,
    pub browser_input_span: Option<interaction_contracts::desktop::verification::TimeSpan>,
    pub sequence: u32,
    /// Input may have reached the application, so the quota is spent.
    pub input_reached: bool,
    /// Receipt uncertainty is distinct from a known input whose business
    /// effect has not yet been proved by observations.
    pub mechanical_unknown: bool,
    pub deadline_ms: u64,
    pub menu_opening: Option<MenuOpeningRef>,
}

fn text_set_value_span(
    receipt: &Receipt,
    expected: &str,
) -> Option<(u64, interaction_contracts::desktop::verification::TimeSpan)> {
    exact_single_primitive_span(
        receipt,
        &interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::SetValue {
                value: expected.into(),
            },
        },
    )
}

fn exact_single_primitive_span(
    receipt: &Receipt,
    expected: &interaction_contracts::desktop::primitive::Primitive,
) -> Option<(u64, interaction_contracts::desktop::verification::TimeSpan)> {
    if receipt.status != ReceiptStatus::Completed
        || matches!(
            receipt.release_status,
            interaction_contracts::desktop::device::ReleaseStatus::Failed
                | interaction_contracts::desktop::device::ReleaseStatus::Unknown
        )
    {
        return None;
    }
    let mut submitted = receipt.submitted.iter().filter(|input| !input.cleanup());
    let input = submitted.next()?;
    if submitted.next().is_some() || input.native_primitive() != Some(expected) {
        return None;
    }
    let end_ms = receipt.input_end_ms?;
    if end_ms < input.at_ms() || end_ms > receipt.at_ms {
        return None;
    }
    Some((
        end_ms,
        interaction_contracts::desktop::verification::TimeSpan {
            from_ms: input.at_ms(),
            to_ms: end_ms,
        },
    ))
}

fn exact_single_browser_action_span(
    receipt: &Receipt,
    expected: BrowserAction,
) -> Option<interaction_contracts::desktop::verification::TimeSpan> {
    if receipt.status != ReceiptStatus::Completed
        || matches!(
            receipt.release_status,
            interaction_contracts::desktop::device::ReleaseStatus::Failed
                | interaction_contracts::desktop::device::ReleaseStatus::Unknown
        )
    {
        return None;
    }
    let mut submitted = receipt.submitted.iter().filter(|input| !input.cleanup());
    let input = submitted.next()?;
    if submitted.next().is_some() || input.browser_action() != Some(&expected) {
        return None;
    }
    let end_ms = receipt.input_end_ms?;
    if end_ms < input.at_ms() || end_ms > receipt.at_ms {
        return None;
    }
    Some(interaction_contracts::desktop::verification::TimeSpan {
        from_ms: input.at_ms(),
        to_ms: end_ms,
    })
}

fn has_registered_radio_choice_target(
    goal: &GoalSpec,
    pack: &crate::environment::pack::AppPack,
) -> bool {
    goal.allowed_scope
        .as_ref()
        .and_then(|scope| scope.targets.first())
        .and_then(|target| pack.scoped_control(target))
        .is_some_and(|scoped| {
            matches!(
                scoped.value_kind,
                crate::environment::pack::ScopedControlValueKind::RadioChoice(_)
            )
        })
}

#[derive(Clone)]
struct TextCurrentUiFacts {
    attempt_id: AttemptId,
    intent_id: String,
    target: SemanticTargetRef,
    expected: ExpectedValue,
    input_end_ms: u64,
    input_span: interaction_contracts::desktop::verification::TimeSpan,
    input_binding: Binding,
    input_window: SourceWindowContext,
    deadline_ms: u64,
}

#[derive(Clone)]
struct OwnedTextCurrentUiFacts {
    attempt_id: AttemptId,
    intent_id: String,
    expected: String,
    original: OwnedDocumentBindingProof,
    input_span: interaction_contracts::desktop::verification::TimeSpan,
    deadline_ms: u64,
}

impl Engine {
    pub(super) fn owned_document_attempt_active(&self) -> bool {
        self.run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| attempt.owned_document.is_some())
    }

    pub(super) fn owned_document_attempt_preservable_for(
        &self,
        current: &OwnedDocumentBindingProof,
    ) -> bool {
        self.run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| {
                attempt
                    .owned_document
                    .as_ref()
                    .is_some_and(|owned| owned.admitted_proof.same_instance(current))
                    && !attempt.input_reached
                    && !attempt.mechanical_unknown
                    && attempt.dispatch.as_ref().is_none_or(|dispatch| {
                        matches!(dispatch.stage, Stage::Observing | Stage::Granting)
                    })
            })
    }

    pub(super) fn owned_document_attempt_evidence_usable(&self) -> bool {
        let Some(attempt) = self.run.as_ref().and_then(|run| run.attempt.as_ref()) else {
            return false;
        };
        let Some(owned) = attempt.owned_document.as_ref() else {
            return false;
        };
        let Some(current) = self.owned_document_proof_for(&owned.admitted_proof.semantic_target)
        else {
            return false;
        };
        if !owned.admitted_proof.same_instance(&current) {
            return false;
        }
        let Some(dispatch) = attempt.dispatch.as_ref() else {
            return true;
        };
        matches!(
            &dispatch.plan,
            DispatchPlan::OwnedDocument(plan)
                if dispatch
                    .observation
                    .native()
                    .and_then(|observation| observation.owned_document_proof.as_deref())
                    == Some(plan.proof())
                    && plan.proof().same_instance(&current)
        )
    }

    fn owned_document_attempt_replaced(&self) -> bool {
        let Some(owned) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.owned_document.as_ref())
        else {
            return false;
        };
        self.owned_document_proof_for(&owned.admitted_proof.semantic_target)
            .is_some_and(|current| !owned.admitted_proof.same_instance(&current))
    }

    fn background_ax_candidate_parameters_match(
        &self,
        candidate: &Candidate,
        goal: &GoalSpec,
        env: &EnvSnapshot,
        skill: Skill,
    ) -> bool {
        if !matches!(skill, Skill::Activate | Skill::EnsureValue)
            || !matches!(&goal.desired, ExpectedValue::Bool { .. })
            || candidate.semantic_target.as_ref() != Some(&goal.target.semantic_target)
        {
            return false;
        }
        let Some(binding) = self.bindings.current(&goal.target.semantic_target) else {
            return false;
        };
        if candidate.binding.as_ref() != Some(&binding.binding_ref)
            || binding.semantic_target != goal.target.semantic_target
            || binding.rule != "background_ax"
            || !binding.device_context.supports_background_ax_press()
        {
            return false;
        }
        let BindingProofScope::BackgroundAx {
            target_context_hash,
        } = &binding.continuity.scope
        else {
            return false;
        };
        if !self.background_ax_evidence_usable(Some(target_context_hash)) {
            return false;
        }
        let Some(object) = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.instance)
        else {
            return false;
        };
        let Some(label) = object.title.as_deref().or(object.description.as_deref()) else {
            return false;
        };
        if !object.enabled
            || !object.in_view
            || object.secure
            || object.scroll.is_some()
            || instance_fingerprint(&env.snapshot, object).digest() != binding.fingerprint_digest
        {
            return false;
        }
        let Some(TargetContext::NativeWindow { app, source }) =
            env.snapshot.target_context.as_ref()
        else {
            return false;
        };
        if app != &env.snapshot.app
            || app.bundle_id != goal.scope.app
            || source.target_context_hash.as_str() != target_context_hash.as_str()
            || source.window.origin_window_id != object.window_id
            || source.window.session_epoch != env.snapshot.session_epoch
            || source.window.verified_source_sequence != env.snapshot.source_sequence
            || source.window.verified_observed_ms != env.snapshot.observed_ms
        {
            return false;
        }
        let Ok(current) = derive_background_ax_binding_evidence(&env.snapshot, object) else {
            return false;
        };
        if !current.device_context.supports_background_ax_press()
            || !binding.continuity.same_closure(&current.continuity)
            || binding.continuity.source_sequence > current.continuity.source_sequence
        {
            return false;
        }

        let desired = value_text(&goal.desired);
        let expected_parameter_count = if skill == Skill::EnsureValue { 4 } else { 3 };
        candidate.parameters.len() == expected_parameter_count
            && candidate.parameters.get("instance").map(String::as_str)
                == Some(binding.instance.as_str())
            && candidate.parameters.get("role").map(String::as_str) == Some(object.role.as_str())
            && candidate.parameters.get("label").map(String::as_str) == Some(label)
            && (skill != Skill::EnsureValue
                || candidate.parameters.get("desired").map(String::as_str)
                    == Some(desired.as_str()))
    }

    fn browser_binding_for_candidate(&mut self, candidate: &Candidate) -> Option<CandidateBinding> {
        let run = self.run.as_ref()?;
        let profile = run.goal.browser_page_profile?;
        let next = self.next_subgoal()?;
        let ExpectedValue::Bool { value } = &run.goal.desired else {
            return None;
        };
        if run.goal.execution_domain
            != interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
            || run.goal.scope.app != profile.app()
            || run.goal.target.semantic_target.as_str() != profile.semantic_target()
            || run.goal.target.label != profile.label()
            || run.goal.target.role_hint.as_deref() != Some(profile.role())
            || next.skill != Skill::EnsureValue.name()
            || next.target.as_ref() != Some(&run.goal.target.semantic_target)
            || next.scroll.is_some()
            || next.select.is_some()
            || candidate.skill != Skill::EnsureValue.name()
            || candidate.semantic_target.as_ref() != Some(&run.goal.target.semantic_target)
            || candidate.effect_class
                != interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
            || candidate.parameters.len() != 1
            || candidate.parameters.get("desired").map(String::as_str)
                != Some(if *value { "true" } else { "false" })
        {
            return None;
        }
        let observation = self.browser_execution_observation(profile)?;
        if candidate.binding.as_ref() != Some(&observation.binding_ref) {
            return None;
        }
        self.state
            .current_browser_binding(&observation.binding_ref)
            .cloned()
            .map(Box::new)
            .map(CandidateBinding::Browser)
    }

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
        if self.run.as_ref().is_some_and(|run| {
            if run.goal.allowed_scope.is_none() {
                return false;
            }
            let primary_target =
                candidate.semantic_target.as_ref() == Some(&run.goal.target.semantic_target);
            let manual_action = run
                .goal
                .scoped_commit
                .as_ref()
                .filter(|commit| {
                    commit.receipt.kind
                        == interaction_contracts::desktop::task::ScopedCommitKind::Manual
                })
                .and_then(|commit| commit.action.as_ref());
            let commit_action = manual_action.is_some_and(|action| {
                candidate.skill == Skill::Activate.name()
                    && candidate.semantic_target.as_ref() == Some(&action.semantic_target)
            });
            let scoped_choice = scoped_choice_values(&run.goal, &run.pack).is_some();
            let scoped_radio_choice = scoped_radio_choice_values(&run.goal, &run.pack).is_some();
            let scoped_label_choice = scoped_choice || scoped_radio_choice;
            let registered_radio_target = has_registered_radio_choice_target(&run.goal, &run.pack);
            let scoped_number_scroll = candidate.skill == Skill::ScrollTo.name()
                && scoped_number_form_scroll_container(&run.goal, &run.pack).is_some_and(
                    |container| {
                        candidate
                            .semantic_target
                            .as_ref()
                            .is_some_and(|target| target.as_str() == container.semantic)
                            && self.next_subgoal().is_some_and(|step| {
                                scoped_number_scroll_step_matches(&run.goal, container, &step)
                            })
                    },
                );
            if !primary_target && !commit_action && !scoped_number_scroll {
                return true;
            }
            if run.goal.scoped_revisit.is_some()
                || manual_action.is_some()
                || scoped_choice
                || registered_radio_target
            {
                if !self
                    .tasks
                    .active()
                    .and_then(|active| active.row.plan.as_ref())
                    .is_some_and(|plan| scoped_revisit_plan_is_allowed(&run.goal, &run.pack, plan))
                {
                    return true;
                }
                let next = self.next_subgoal();
                let expected_effect = match candidate.skill.as_str() {
                    "ui.read_value" => {
                        Some(interaction_contracts::desktop::effect::EffectClass::ReadOnly)
                    }
                    "ui.ensure_value" => {
                        Some(interaction_contracts::desktop::effect::EffectClass::ReversibleEdit)
                    }
                    "ui.revisit" => {
                        Some(interaction_contracts::desktop::effect::EffectClass::Navigation)
                    }
                    "ui.scroll_to" if scoped_number_scroll => {
                        Some(interaction_contracts::desktop::effect::EffectClass::Navigation)
                    }
                    "ui.select_by_label" if scoped_label_choice => {
                        Some(interaction_contracts::desktop::effect::EffectClass::ReversibleEdit)
                    }
                    "ui.activate" if commit_action => {
                        Some(interaction_contracts::desktop::effect::EffectClass::Commit)
                    }
                    _ => None,
                };
                let expected_target = if commit_action {
                    manual_action.map(|action| &action.semantic_target)
                } else {
                    Some(&run.goal.target.semantic_target)
                };
                return expected_effect != Some(candidate.effect_class)
                    || next.is_none_or(|subgoal| {
                        subgoal.skill != candidate.skill
                            || subgoal.target.as_ref() != expected_target
                            || (subgoal.scroll.is_some() && !scoped_number_scroll)
                            || (subgoal.select.is_some()
                                && !(scoped_label_choice
                                    && candidate.skill == Skill::SelectByLabel.name()
                                    && subgoal.skill == Skill::SelectByLabel.name()))
                    });
            }
            !matches!(
                candidate.skill.as_str(),
                "ui.ensure_value" | "ui.read_value"
            ) || candidate.effect_class
                > interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
        }) {
            return self.drop_decision(
                &decision,
                "scoped roots accept only the current frozen value or revisit step",
            );
        }
        if source == DecisionSource::Computer
            && self
                .run
                .as_ref()
                .is_some_and(|run| run.goal.allowed_scope.is_some())
        {
            return self.drop_decision(
                &decision,
                "computer proposals have no trusted semantic binding for this scoped root",
            );
        }
        let candidate = if source == DecisionSource::Computer {
            match self.rebind_computer_admission(&candidate) {
                Ok(candidate) => candidate,
                Err(reason) => return self.drop_decision(&decision, &reason),
            }
        } else {
            candidate
        };
        let Some(skill) = Skill::from_name(&candidate.skill) else {
            return self.drop_decision(&decision, "the candidate's skill has no runtime");
        };
        let headless_browser = self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
        });
        let env = if headless_browser {
            None
        } else {
            self.state.latest().cloned()
        };
        if !headless_browser && env.is_none() {
            return self.drop_decision(&decision, "there is no evidence to admit against");
        }
        let background_ax = self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        });
        if background_ax {
            let allowed_next = self.run.as_ref().and_then(|run| {
                let next = self.next_subgoal()?;
                let target = candidate.semantic_target.as_ref()?;
                (target == &run.goal.target.semantic_target
                    && matches!(skill, Skill::Activate | Skill::EnsureValue)
                    && next.skill == skill.name()
                    && next.target.as_ref() == Some(target)
                    && next.scroll.is_none()
                    && next.select.is_none()
                    && self.background_ax_candidate_parameters_match(
                        &candidate,
                        &run.goal,
                        env.as_ref().expect("BackgroundAX uses native evidence"),
                        skill,
                    )
                    && candidate.effect_class
                        == interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
                    && candidate
                        .binding
                        .as_ref()
                        .is_none_or(|binding| binding.as_str().starts_with("bg-bind-")))
                .then_some(())
            });
            if allowed_next.is_none() {
                return self.drop_decision(
                    &decision,
                    "BackgroundAX accepts only one registered AXPress value step for the frozen target",
                );
            }
        }
        let binding = if headless_browser {
            self.browser_binding_for_candidate(&candidate)
        } else {
            self.binding_for(
                &candidate,
                env.as_ref().expect("native evidence was checked"),
            )
        };
        let Some(binding) = binding else {
            return self.drop_decision(&decision, "the candidate's target is no longer bound");
        };
        if let Some(run) = self.run.as_ref()
            && (run.goal.scoped_revisit.is_some()
                || scoped_choice_values(&run.goal, &run.pack).is_some()
                || has_registered_radio_choice_target(&run.goal, &run.pack)
                || run
                    .goal
                    .scoped_commit
                    .as_ref()
                    .is_some_and(|commit| commit.action.is_some()))
            && matches!(
                skill,
                Skill::EnsureValue
                    | Skill::ReadValue
                    | Skill::Revisit
                    | Skill::Activate
                    | Skill::SelectByLabel
            )
        {
            let Some(env) = env.as_ref() else {
                return self.drop_decision(
                    &decision,
                    "scoped native controls require a current foreground Source read",
                );
            };
            let Some(primary_binding) = binding.generic() else {
                return self.drop_decision(
                    &decision,
                    "scoped native controls require their current primary-control binding",
                );
            };
            let is_commit_action = run
                .goal
                .scoped_commit
                .as_ref()
                .filter(|commit| {
                    commit.receipt.kind
                        == interaction_contracts::desktop::task::ScopedCommitKind::Manual
                })
                .and_then(|commit| commit.action.as_ref())
                .is_some_and(|action| {
                    skill == Skill::Activate
                        && candidate.semantic_target.as_ref() == Some(&action.semantic_target)
                });
            let exact_binding = if is_commit_action {
                scoped_commit_action_binding_object(&run.goal, &run.pack, env, primary_binding)
                    .is_some()
            } else {
                scoped_revisit_primary_binding_object(&run.goal, &run.pack, env, primary_binding)
                    .is_some()
            };
            if !self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
                || env.snapshot.observed_ms > self.now_ms
                || !exact_binding
            {
                return self.drop_decision(
                    &decision,
                    "the scoped primary control no longer matches its frozen form, type, or Source context",
                );
            }
        }
        if background_ax
            && binding.generic().is_none_or(|binding| {
                binding.rule != "background_ax"
                    || candidate.semantic_target.as_ref() != Some(&binding.semantic_target)
                    || candidate.binding.as_ref() != Some(&binding.binding_ref)
                    || !binding.device_context.supports_background_ax_press()
            })
        {
            return self.drop_decision(
                &decision,
                "BackgroundAX candidate does not carry the frozen registered target proof",
            );
        }
        let owned_document = matches!(&binding, CandidateBinding::OwnedDocument(_));
        if owned_document
            && (skill != Skill::EnterText
                && (skill != Skill::SaveDocument || !self.cfg.allow_owned_document_save)
                || candidate.semantic_target.as_ref() != Some(binding.semantic_target())
                || candidate
                    .parameters
                    .keys()
                    .any(|key| matches!(key.as_str(), "value" | "text" | "instance" | "choice")))
        {
            return self.drop_decision(
                &decision,
                "the owned document candidate is not an authorized frozen text action",
            );
        }
        let admission = if headless_browser {
            let run = self.run.as_ref().expect("a running root has a run");
            admit(
                candidate.effect_class,
                &EffectContext {
                    in_scope: candidate.semantic_target.as_ref()
                        == Some(&run.goal.target.semantic_target)
                        && run.goal.scope.app
                            == run
                                .goal
                                .browser_page_profile
                                .expect("headless profile validated")
                                .app(),
                    target_covered: candidate.semantic_target.as_ref()
                        == Some(&run.goal.target.semantic_target),
                    target_named: candidate.semantic_target.as_ref()
                        == Some(&run.goal.target.semantic_target),
                    max_effect_class: run.goal.max_effect_class,
                    confirmed,
                },
            )
        } else {
            let run = self.run.as_ref().expect("a running root has a run");
            gate(
                &Scope {
                    goal: &run.goal,
                    plan_targets: &run.plan_targets,
                    pack: &run.pack,
                    env: env.as_ref().expect("native evidence was checked"),
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
                .min(root_deadline)
                .min(if source == DecisionSource::Computer {
                    self.computer_admission_deadline().unwrap_or(self.now_ms)
                } else {
                    root_deadline
                });
            let request = AdmitAttempt {
                attempt_id,
                decision_id: decision.clone(),
                generation,
                root_task_id: active.row.root_id.clone(),
                root_epoch: active.row.root_epoch,
                task_revision: active.row.task_revision,
                plan_revision: active.row.plan_revision,
                subgoal: if skill == Skill::ActivateWindow {
                    "__activate_window".into()
                } else {
                    match self.decisions.pending() {
                        // A step decision names its sub-goal; a dialog decision has none.
                        Some(pending) => pending
                            .subgoal
                            .clone()
                            .unwrap_or_else(|| "layer".to_string()),
                        // A plan's own decision is for the step that is next.
                        None => self
                            .next_subgoal()
                            .map_or_else(|| "direct".to_string(), |subgoal| subgoal.id),
                    }
                },
                skill: skill.name().to_string(),
                quota: if skill == Skill::ScrollTo {
                    self.next_subgoal()
                        .and_then(|subgoal| subgoal.scroll)
                        .map(|spec| {
                            let inputs = spec.max_actions
                                * if self.cfg.native_window_scope.is_some() {
                                    2
                                } else {
                                    1
                                };
                            AttemptQuota {
                                actions: inputs,
                                observations: inputs + 1,
                            }
                        })
                        .unwrap_or_else(|| skill.quota())
                } else if skill == Skill::SelectByLabel
                    && self.run.as_ref().is_some_and(|run| {
                        scoped_radio_choice_values(&run.goal, &run.pack).is_some()
                    })
                {
                    AttemptQuota {
                        actions: 1,
                        observations: 3,
                    }
                } else if headless_browser
                    || owned_document
                    || (skill == Skill::EnsureValue
                        && self.run.as_ref().is_some_and(|run| {
                            run.pack
                                .keyboard_toggle(binding.semantic_target())
                                .is_some()
                        }))
                {
                    AttemptQuota {
                        actions: 1,
                        observations: 2,
                    }
                } else {
                    skill.quota()
                },
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
                "binding": binding.binding_ref(),
                "target": binding.semantic_target(),
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
        binding: &CandidateBinding,
    ) {
        let select_config = self
            .tasks
            .active()
            .and_then(|active| active.row.plan.as_ref())
            .and_then(|plan| {
                plan.subgoals
                    .iter()
                    .find(|subgoal| subgoal.id == request.subgoal)
            })
            .and_then(|subgoal| subgoal.select.clone());
        let scroll_config = self
            .tasks
            .active()
            .and_then(|active| active.row.plan.as_ref())
            .and_then(|plan| {
                plan.subgoals
                    .iter()
                    .find(|subgoal| subgoal.id == request.subgoal)
            })
            .and_then(|subgoal| subgoal.scroll.clone().zip(subgoal.target.clone()))
            .and_then(|(mut spec, target)| {
                let run = self.run.as_ref()?;
                let destination = if target == run.goal.target.semantic_target {
                    run.goal.target.clone()
                } else {
                    let control = run.pack.control(&target)?;
                    interaction_contracts::desktop::task::TargetSpec {
                        row: None,
                        semantic_target: target,
                        label: control.label.clone(),
                        group: None,
                        role_hint: Some(control.role.clone()),
                    }
                };
                if spec.container.group.is_none() {
                    spec.container.group = self
                        .state
                        .latest()
                        .and_then(|env| {
                            binding
                                .generic()
                                .and_then(|generic| object_for(env, &generic.fingerprint_digest))
                        })
                        .and_then(|object| object.group.clone());
                }
                Some((spec, destination))
            });
        let computer_step = if request.source == DecisionSource::Computer {
            self.take_computer_attempt_step()
        } else {
            None
        };
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let goal_target = binding.semantic_target() == &run.goal.target.semantic_target;
        run.last_attempt = Some((request.attempt_id.clone(), None));
        run.decision_failures = 0;
        run.attempt = Some(AttemptRun {
            id: request.attempt_id.clone(),
            subgoal: request.subgoal.clone(),
            computer_step,
            admitted_effect_class: request.effect_class,
            computer_input_completed: false,
            skill: {
                let skill_run = SkillRun::new(
                    skill,
                    binding.semantic_target().clone(),
                    Some(select_config.as_ref().map_or_else(
                        || run.goal.desired.clone(),
                        |spec| interaction_contracts::desktop::task::ExpectedValue::Text {
                            value: spec.choice_label.clone(),
                        },
                    )),
                    binding.binding_ref().clone(),
                )
                .with_execution_domain(run.goal.execution_domain);
                let skill_run = if matches!(skill, Skill::ReadValue | Skill::SelectByLabel) {
                    if let Some(spec) = scoped_radio_choice_spec(&run.goal, &run.pack) {
                        skill_run.with_scoped_radio_choice(spec)
                    } else {
                        skill_run
                    }
                } else {
                    skill_run
                };
                let skill_run = if skill == Skill::EnsureValue
                    && run.goal.allowed_scope.as_ref().is_some_and(|scope| {
                        scope.targets.len() == 1 && scope.targets[0].form.is_some()
                    })
                    && (matches!(&run.goal.desired, ExpectedValue::Text { .. })
                        || scoped_form_number_control(&run.goal, &run.pack).is_some())
                    && matches!(
                        run.goal.target.role_hint.as_deref(),
                        Some("AXTextField" | "AXTextArea")
                    ) {
                    skill_run.with_scoped_form_text()
                } else {
                    skill_run
                };
                let skill_run = if skill == Skill::Revisit {
                    if let Some(pair) = run.goal.scoped_revisit.clone() {
                        skill_run.with_scoped_revisit(pair)
                    } else {
                        skill_run
                    }
                } else {
                    skill_run
                };
                let skill_run = if let Some(profile) = run.goal.browser_page_profile {
                    skill_run.with_browser_page_profile(profile)
                } else {
                    skill_run
                };
                let skill_run = if matches!(binding, CandidateBinding::Row(_)) {
                    skill_run.with_row_target(run.goal.target.clone())
                } else {
                    skill_run
                };
                if let Some(spec) = select_config {
                    skill_run.with_select(spec)
                } else if let Some((spec, destination)) = scroll_config {
                    let configured = skill_run.with_scroll(spec, destination);
                    if self.cfg.native_window_scope.is_some() {
                        configured.with_scroll_pointer_preparation()
                    } else {
                        configured
                    }
                } else if skill == Skill::ActivateWindow {
                    if let Some(scope) = &self.cfg.native_window_scope {
                        skill_run.with_window(scope.app.clone(), scope.pid, scope.window.clone())
                    } else {
                        skill_run
                    }
                } else {
                    skill_run
                }
            },
            goal_target,
            quota: *granted,
            observations_used: 0,
            dispatch: None,
            owned_document: match binding {
                CandidateBinding::OwnedDocument(proof) => {
                    let ExpectedValue::Text { value } = &run.goal.desired else {
                        return;
                    };
                    Some(OwnedDocumentAttempt {
                        admitted_proof: proof.clone(),
                        input_proof: None,
                        value: value.clone(),
                        action: if skill == Skill::SaveDocument {
                            OwnedDocumentActionKind::Save
                        } else {
                            OwnedDocumentActionKind::SetValue
                        },
                        input_issued: false,
                    })
                }
                CandidateBinding::Generic(_) => None,
                CandidateBinding::Row(_) => None,
                CandidateBinding::Browser(_) => None,
            },
            last_input_path: None,
            last_input_binding: None,
            last_input_text: None,
            last_input_window: None,
            last_input_intent_id: None,
            text_input_end_ms: None,
            text_input_span: None,
            text_current_ui_pending: false,
            text_current_ui_last_snapshot: None,
            text_current_ui_last_rejection: None,
            browser_current_ui_pending: false,
            browser_current_ui_no_input: false,
            browser_current_ui_fence_ms: None,
            browser_current_ui_last_snapshot: None,
            browser_current_ui_last_rejection: None,
            browser_original_binding: None,
            browser_input_span: None,
            sequence: 0,
            input_reached: false,
            mechanical_unknown: false,
            deadline_ms: request.deadline_ms,
            menu_opening: None,
        });
    }

    /// The binding the skill starts with: the one the candidate named, or, for
    /// a pick among same-named controls, the one that pick makes.
    fn binding_for(
        &mut self,
        candidate: &Candidate,
        env: &EnvSnapshot,
    ) -> Option<CandidateBinding> {
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.goal.allowed_scope.is_some())
            && candidate.parameters.contains_key("choice")
        {
            return None;
        }
        let row_goal = self
            .run
            .as_ref()
            .is_some_and(|run| run.goal.target.row.is_some());
        if candidate.skill == "ui.select_row" || (row_goal && candidate.skill == "ui.revisit") {
            let run = self.run.as_ref()?;
            let target = candidate.semantic_target.as_ref()?;
            if candidate
                .binding
                .as_ref()
                .is_some_and(|reference| !reference.as_str().starts_with("row-bind-"))
            {
                return None;
            }
            let skill = Skill::from_name(&candidate.skill)?;
            let expected_class = interaction_contracts::desktop::effect::EffectClass::Navigation;
            let next = self.next_subgoal()?;
            if target != &run.goal.target.semantic_target
                || run.goal.target.row.is_none()
                || candidate.effect_class != expected_class
                || !candidate.parameters.is_empty()
                || next.skill != skill.name()
                || next.scroll.is_some()
                || next.select.is_some()
                || next.target.as_ref() != Some(&run.goal.target.semantic_target)
                || self
                    .decisions
                    .pending()
                    .and_then(|pending| pending.subgoal.as_deref())
                    .is_some_and(|subgoal| subgoal != next.id)
            {
                return None;
            }
            // The decision may have outlived its Source read. Re-resolve only
            // the same frozen public selector, retaining the admitted Decision
            // identity while issuing a new read-scoped binding/ref.
            let frozen_target = run.goal.target.clone();
            let binding = self
                .bindings
                .bind_row(
                    env,
                    &frozen_target,
                    self.now_ms,
                    self.cfg.evidence_max_age_ms,
                )
                .ok()?;
            if skill == Skill::SelectRow
                && binding.source.selected != Some(true)
                && (binding.source.selected != Some(false)
                    || binding.source.ax_selected_settable != Some(true)
                    || binding.device_context.is_none()
                    || binding.continuity.is_none()
                    || binding.hit_point.is_none())
            {
                return None;
            }
            return Some(CandidateBinding::Row(Box::new(binding)));
        }
        if let Some(reference) = candidate
            .binding
            .as_ref()
            .filter(|reference| reference.as_str().starts_with("owned-text/"))
        {
            let run = self.run.as_ref()?;
            let target = candidate.semantic_target.as_ref()?;
            let ExpectedValue::Text { value } = &run.goal.desired else {
                return None;
            };
            let expected_step = match candidate.skill.as_str() {
                "ui.enter_text" => Skill::EnterText,
                "ui.save_document" if self.cfg.allow_owned_document_save => Skill::SaveDocument,
                _ => return None,
            };
            if candidate.skill != expected_step.name()
                || candidate.effect_class
                    != interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
                || target != &run.goal.target.semantic_target
                || candidate
                    .parameters
                    .keys()
                    .any(|key| matches!(key.as_str(), "value" | "text" | "instance" | "choice"))
                || self.next_subgoal().is_none_or(|subgoal| {
                    subgoal.skill != expected_step.name()
                        || subgoal.scroll.is_some()
                        || subgoal.select.is_some()
                        || subgoal
                            .target
                            .as_ref()
                            .is_some_and(|target| target != &run.goal.target.semantic_target)
                })
            {
                return None;
            }
            if expected_step == Skill::EnterText
                && OwnedDocumentBindingProof::validate_setter_value(value).is_err()
            {
                return None;
            }
            let proof = self.owned_document_proof_for(target)?;
            if &proof.binding != reference || &proof.semantic_target != target {
                return None;
            }
            if expected_step == Skill::SaveDocument
                && OwnedDocumentActionPlan::save(proof.clone(), value.clone()).is_err()
            {
                return None;
            }
            return Some(CandidateBinding::OwnedDocument(proof));
        }
        if candidate.skill == Skill::SaveDocument.name() {
            return None;
        }
        // The fixed keyboard semantic is never resolved through label choices
        // or a teacher's wrapper selection: only the current exact Source focus
        // matching the original frozen TargetSpec supplies its input binding.
        let keyboard = candidate.skill == "ui.ensure_value"
            && candidate.semantic_target.as_ref().is_some_and(|target| {
                self.run
                    .as_ref()
                    .is_some_and(|run| run.pack.keyboard_toggle(target).is_some())
            });
        if keyboard {
            let run = self.run.as_ref()?;
            if candidate.semantic_target.as_ref() != Some(&run.goal.target.semantic_target)
                || run.keyboard_result_context.is_some()
            {
                return None;
            }
            return self
                .bind_keyboard_goal(env)
                .map(|binding| CandidateBinding::Generic(Box::new(binding)));
        }
        if let Some(choice) = candidate.parameters.get("choice") {
            let instance = ObjectRef::new(choice.clone()).ok()?;
            if candidate.skill == "ui.scroll_to" {
                let spec = self.next_subgoal()?.scroll?;
                if candidate.semantic_target.as_ref() != Some(&spec.container.semantic_target) {
                    return None;
                }
                let run = self.run.as_ref()?;
                let request = interaction_contracts::desktop::binding::BindRequest {
                    request_id: interaction_contracts::desktop::ids::RequestId::new(format!(
                        "scroll-choice-{}",
                        self.now_ms
                    ))
                    .ok()?,
                    semantic_target: spec.container.semantic_target.clone(),
                    description: interaction_contracts::desktop::binding::TargetDescription {
                        label: spec.container.label,
                        group: spec.container.group,
                        role_hint: spec.container.role_hint,
                    },
                    scope: interaction_contracts::desktop::binding::BindScope {
                        app: run.goal.scope.app.clone(),
                        account: run.goal.scope.account.clone(),
                    },
                    choice: Some(instance),
                    max_age_ms: self.cfg.evidence_max_age_ms,
                };
                if !matches!(
                    self.bindings.bind(env, &request, self.now_ms).outcome,
                    interaction_contracts::desktop::binding::BindingOutcome::Bound { .. }
                ) {
                    return None;
                }
                return self
                    .bindings
                    .current(&spec.container.semantic_target)
                    .cloned()
                    .map(|binding| CandidateBinding::Generic(Box::new(binding)));
            }
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
                Resolution::Bound(binding) => Some(CandidateBinding::Generic(binding)),
                _ => None,
            };
        }
        let reference = candidate.binding.as_ref()?;
        self.bindings
            .all()
            .find(|binding| &binding.binding_ref == reference)
            .cloned()
            .map(|binding| CandidateBinding::Generic(Box::new(binding)))
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

    fn text_current_ui_facts(&self, attempt: &AttemptRun) -> Option<TextCurrentUiFacts> {
        if attempt.skill.skill() != Skill::EnterText
            || !attempt.input_reached
            || attempt.mechanical_unknown
            || attempt.last_input_path != Some(ExecutionPath::Ax)
        {
            return None;
        }
        let run = self.run.as_ref()?;
        let ExpectedValue::Text { value } = &run.goal.desired else {
            return None;
        };
        let input_binding = attempt.last_input_binding.as_ref()?.clone();
        let input_window = attempt.last_input_window.as_ref()?.clone();
        let input_end_ms = attempt.text_input_end_ms?;
        let input_span = attempt.text_input_span?;
        let intent_id = attempt.last_input_intent_id.as_ref()?.clone();
        if attempt.last_input_text.as_deref() != Some(value.as_str())
            || input_span.to_ms != input_end_ms
            || input_span.from_ms > input_end_ms
        {
            return None;
        }
        Some(TextCurrentUiFacts {
            attempt_id: attempt.id.clone(),
            intent_id,
            target: input_binding.semantic_target.clone(),
            expected: run.goal.desired.clone(),
            input_end_ms,
            input_span,
            input_binding,
            input_window,
            deadline_ms: attempt.deadline_ms,
        })
    }

    fn owned_text_current_ui_facts(&self, attempt: &AttemptRun) -> Option<OwnedTextCurrentUiFacts> {
        if attempt.skill.skill() != Skill::EnterText
            || !attempt.input_reached
            || attempt.mechanical_unknown
            || attempt.last_input_path != Some(ExecutionPath::Ax)
        {
            return None;
        }
        let run = self.run.as_ref()?;
        let ExpectedValue::Text { value } = &run.goal.desired else {
            return None;
        };
        let owned = attempt.owned_document.as_ref()?;
        let original = owned.input_proof.as_ref()?.clone();
        let intent_id = attempt.last_input_intent_id.as_ref()?.clone();
        let input_span = attempt.text_input_span?;
        if owned.value != *value
            || attempt.last_input_text.as_deref() != Some(value.as_str())
            || attempt.text_input_end_ms != Some(input_span.to_ms)
            || input_span.from_ms > input_span.to_ms
            || original.semantic_target != run.goal.target.semantic_target
        {
            return None;
        }
        Some(OwnedTextCurrentUiFacts {
            attempt_id: attempt.id.clone(),
            intent_id,
            expected: value.clone(),
            original,
            input_span,
            deadline_ms: attempt.deadline_ms,
        })
    }

    fn owned_text_current_ui_proof(
        &self,
        facts: &OwnedTextCurrentUiFacts,
        now_ms: u64,
    ) -> Option<(
        OwnedDocumentBindingProof,
        interaction_contracts::desktop::verification::TriState,
    )> {
        if now_ms >= facts.deadline_ms {
            return None;
        }
        let proof = self.owned_document_proof_for(&facts.original.semantic_target)?;
        let registration = self.cfg.owned_document_registration.as_deref()?;
        let verdict = crate::verification::owned_text_value_diagnostic(
            &facts.original,
            &proof,
            registration,
            &facts.expected,
            facts.input_span,
            now_ms,
            self.cfg.evidence_max_age_ms,
        );
        Some((proof, verdict))
    }

    fn record_owned_text_current_ui(
        &mut self,
        facts: &OwnedTextCurrentUiFacts,
        current: &OwnedDocumentBindingProof,
    ) -> Result<(), EngineError> {
        let readback = current
            .receiver
            .value_readback
            .as_ref()
            .expect("a positive owned text diagnostic requires a digest readback");
        let run = self
            .run
            .as_ref()
            .ok_or_else(|| StoreError::Corrupt("owned text result without root".into()))?;
        crate::verification::conclude_current_ui(
            &mut self.storage,
            interaction_contracts::desktop::verification::EffectResult {
                result_id: RequestId::new(format!("text-owned-effect-{}", facts.attempt_id))
                    .expect("attempt id produces a valid result id"),
                revision: 1,
                root_task_id: run.goal.root_task_id.clone(),
                attempt_id: Some(facts.attempt_id.clone()),
                intent_id: Some(facts.intent_id.clone()),
                predicate: "text.value@1".into(),
                target: Some(current.semantic_target.clone()),
                status: interaction_contracts::desktop::verification::EffectStatus::Satisfied,
                evidence_group: vec![current.receiver.provenance.snapshot_id.clone()],
                input_span: Some(facts.input_span),
                observed_ms: readback.observed_ms,
                confirmation_scope:
                    interaction_contracts::desktop::task::ConfirmationScope::CurrentUi,
                causality: interaction_contracts::desktop::verification::Causality::Unknown,
            },
        )?;
        Ok(())
    }

    fn prove_text_current_ui(
        &mut self,
        facts: &TextCurrentUiFacts,
        now_ms: u64,
    ) -> Result<(RequestId, u64), TextCurrentUiRejection> {
        if now_ms >= facts.deadline_ms {
            return Err(TextCurrentUiRejection::DeadlineExpired);
        }
        if !self.state.usable(now_ms, self.cfg.evidence_max_age_ms) {
            return Err(TextCurrentUiRejection::SourceUnusable);
        }
        let env = self
            .state
            .latest()
            .cloned()
            .ok_or(TextCurrentUiRejection::SnapshotMissing)?;
        if env.snapshot.observed_ms >= facts.deadline_ms {
            return Err(TextCurrentUiRejection::SnapshotOutsideDeadline);
        }
        let current = self
            .bindings
            .bind_instance(&env, facts.target.clone(), &facts.input_binding.instance)
            .ok_or(TextCurrentUiRejection::BindingUnavailable)?;
        if current.semantic_target != facts.input_binding.semantic_target {
            return Err(TextCurrentUiRejection::BindingSemanticChanged);
        }
        if current.instance != facts.input_binding.instance {
            return Err(TextCurrentUiRejection::BindingInstanceChanged);
        }
        if current.fingerprint_digest != facts.input_binding.fingerprint_digest {
            return Err(TextCurrentUiRejection::BindingFingerprintChanged);
        }
        if current.page != facts.input_binding.page {
            return Err(TextCurrentUiRejection::BindingPageChanged);
        }
        crate::verification::text_value_diagnostic(
            &self.state,
            &self.bindings,
            &facts.target,
            &facts.expected,
            facts.input_end_ms,
            &facts.input_binding,
            &facts.input_window,
            now_ms,
            self.cfg.evidence_max_age_ms,
        )
        .map_err(TextCurrentUiRejection::Verification)?;
        Ok((env.snapshot.snapshot_id, env.snapshot.observed_ms))
    }

    fn record_text_current_ui(
        &mut self,
        facts: &TextCurrentUiFacts,
        evidence: (RequestId, u64),
    ) -> Result<(), EngineError> {
        let Some(run) = self.run.as_ref() else {
            return Ok(());
        };
        crate::verification::conclude_current_ui(
            &mut self.storage,
            interaction_contracts::desktop::verification::EffectResult {
                result_id: RequestId::new(format!("text-effect-{}", facts.attempt_id)).unwrap(),
                revision: 1,
                root_task_id: run.goal.root_task_id.clone(),
                attempt_id: Some(facts.attempt_id.clone()),
                intent_id: Some(facts.intent_id.clone()),
                predicate: "text.value@1".into(),
                target: Some(facts.target.clone()),
                status: interaction_contracts::desktop::verification::EffectStatus::Satisfied,
                evidence_group: vec![evidence.0],
                input_span: Some(facts.input_span),
                observed_ms: evidence.1,
                confirmation_scope:
                    interaction_contracts::desktop::task::ConfirmationScope::CurrentUi,
                causality: interaction_contracts::desktop::verification::Causality::Unknown,
            },
        )?;
        Ok(())
    }

    fn pending_text_current_ui(&mut self) -> Step {
        let now = self.now_ms;
        let Some((deadline_ms, last_snapshot, input_end_ms)) = self.run.as_ref().and_then(|run| {
            let attempt = run.attempt.as_ref()?;
            attempt.text_current_ui_pending.then_some((
                attempt.deadline_ms,
                attempt.text_current_ui_last_snapshot.clone(),
                attempt.text_input_end_ms?,
            ))
        }) else {
            return Ok(false);
        };
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.stop_wait_since.is_some())
        {
            return Ok(false);
        }
        if now >= deadline_ms {
            return self.end_pending_text_unverified();
        }
        if self.native_context_settling() {
            return Ok(false);
        }
        let owned_target = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| {
                attempt
                    .owned_document
                    .as_ref()
                    .map(|owned| owned.admitted_proof.semantic_target.clone())
            });
        let fresh = if let Some(target) = owned_target.as_ref() {
            self.owned_document_proof_for(target)
                .filter(|proof| {
                    proof.snapshot_observed_ms > input_end_ms
                        && proof.snapshot_observed_ms <= deadline_ms
                        && Some(&proof.receiver.provenance.snapshot_id) != last_snapshot.as_ref()
                })
                .map(|proof| proof.receiver.provenance.snapshot_id.clone())
        } else {
            self.state
                .latest()
                .filter(|env| {
                    env.snapshot.observed_ms > input_end_ms
                        && env.snapshot.observed_ms <= deadline_ms
                        && Some(&env.snapshot.snapshot_id) != last_snapshot.as_ref()
                })
                .map(|env| env.snapshot.snapshot_id.clone())
        };
        if let Some(snapshot_id) = fresh {
            if owned_target.is_some() {
                let verdict = self
                    .run
                    .as_ref()
                    .and_then(|run| run.attempt.as_ref())
                    .and_then(|attempt| self.owned_text_current_ui_facts(attempt))
                    .and_then(|facts| self.owned_text_current_ui_proof(&facts, now))
                    .map(|(_, verdict)| verdict)
                    .unwrap_or(interaction_contracts::desktop::verification::TriState::Unknown);
                match verdict {
                    interaction_contracts::desktop::verification::TriState::True => {
                        if let Some(attempt) =
                            self.run.as_mut().and_then(|run| run.attempt.as_mut())
                        {
                            attempt.text_current_ui_pending = false;
                        }
                        if let Some(run) = self.run.as_mut() {
                            run.stalls = 0;
                            run.wait = None;
                        }
                        self.note("a later owned readback verified the completed SetValue");
                        self.finish_attempt(SkillResult::Succeeded)?;
                        return Ok(true);
                    }
                    interaction_contracts::desktop::verification::TriState::False => {
                        if let Some(attempt) =
                            self.run.as_mut().and_then(|run| run.attempt.as_mut())
                        {
                            attempt.text_current_ui_pending = false;
                        }
                        self.finish_attempt(SkillResult::Failed {
                            code: FailureCode::EffectUnknown,
                            reason: "a fresh owned-document digest did not match the frozen text"
                                .into(),
                        })?;
                        return Ok(true);
                    }
                    interaction_contracts::desktop::verification::TriState::Unknown => {
                        if let Some(attempt) =
                            self.run.as_mut().and_then(|run| run.attempt.as_mut())
                        {
                            attempt.text_current_ui_last_snapshot = Some(snapshot_id);
                            attempt.text_current_ui_last_rejection =
                                Some(TextCurrentUiRejection::Verification(
                                    crate::verification::TextValueRejection::ValueUnavailable,
                                ));
                        }
                    }
                }
            } else {
                let facts = self
                    .run
                    .as_ref()
                    .and_then(|run| run.attempt.as_ref())
                    .and_then(|attempt| self.text_current_ui_facts(attempt));
                let proof = match facts.as_ref() {
                    Some(facts) => self.prove_text_current_ui(facts, now),
                    None => Err(TextCurrentUiRejection::InputFactsIncomplete),
                };
                match proof {
                    Ok(_) => {
                        if let Some(attempt) =
                            self.run.as_mut().and_then(|run| run.attempt.as_mut())
                        {
                            attempt.text_current_ui_pending = false;
                        }
                        if let Some(run) = self.run.as_mut() {
                            run.effect_verification_only = false;
                            run.stalls = 0;
                            run.wait = None;
                        }
                        self.note(
                            "fresh same-instance Source evidence verified the completed text input",
                        );
                        self.finish_attempt(SkillResult::Succeeded)?;
                        return Ok(true);
                    }
                    Err(rejection) => {
                        if let Some(attempt) =
                            self.run.as_mut().and_then(|run| run.attempt.as_mut())
                        {
                            attempt.text_current_ui_last_snapshot = Some(snapshot_id);
                            attempt.text_current_ui_last_rejection = Some(rejection);
                        }
                    }
                }
            }
        }
        let (observations_used, observation_quota) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .map(|attempt| (attempt.observations_used, attempt.quota.observations))
            .unwrap();
        if observations_used >= observation_quota {
            return self.end_pending_text_unverified();
        }
        let next_after = self
            .state
            .latest()
            .and_then(|env| env.snapshot.observed_ms.checked_add(1))
            .into_iter()
            .chain(input_end_ms.checked_add(1))
            .max();
        let Some(next_after) = next_after else {
            return self.end_pending_text_unverified();
        };
        let native_stream = self
            .device
            .identity()
            .is_some_and(|identity| identity.clock_domain.starts_with("macos/"));
        let sent = self.await_source(
            next_after,
            "verifying the completed text input from fresh Source evidence",
            !native_stream,
        )?;
        if sent && let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.observations_used += 1;
        }
        Ok(sent)
    }

    fn end_pending_text_unverified(&mut self) -> Step {
        let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.take()) else {
            return Ok(false);
        };
        let rejection = attempt
            .text_current_ui_last_rejection
            .unwrap_or(TextCurrentUiRejection::InputFactsIncomplete);
        self.device.forget_attempt_waits();
        self.tasks.attempt_ended(
            &mut self.storage,
            &attempt.id,
            if attempt.mechanical_unknown {
                AttemptState::Unknown
            } else {
                AttemptState::Settled
            },
            !attempt.input_reached,
        )?;
        self.abandon_work()?;
        self.tasks.input_ended_unverified(
            &mut self.storage,
            self.now_ms,
            "the completed text input could not be verified on its original Source identity",
        )?;
        self.note(format!(
            "text input CurrentUi proof ended unverified ({}); no input was repeated",
            rejection.code()
        ));
        Ok(true)
    }

    pub(super) fn drive_attempt(&mut self) -> Step {
        let now = self.now_ms;
        let Some(attempt) = self.run.as_ref().and_then(|run| run.attempt.as_ref()) else {
            return Ok(false);
        };
        if attempt.browser_current_ui_pending {
            return self.pending_browser_current_ui();
        }
        if attempt.text_current_ui_pending {
            return self.pending_text_current_ui();
        }
        if now >= attempt.deadline_ms && attempt.dispatch.is_none() {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::EvidenceExpired,
                reason: "the attempt ran out of time".into(),
            })?;
            return Ok(true);
        }
        if attempt.dispatch.is_some() {
            return Ok(false);
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
        if !self.attempt_evidence_usable() {
            if self.owned_document_attempt_replaced() {
                self.finish_attempt(SkillResult::Failed {
                    code: FailureCode::ContextUnresolved,
                    reason: "the registered document receiver changed before input".into(),
                })?;
                return Ok(true);
            }
            if self.run.as_ref().is_some_and(|run| run.wait.is_some()) {
                return Ok(false);
            }
            return self.await_source(
                now,
                "evidence is too old to act on",
                !self.state.requires_frames()
                    && !self.menu_same_watch_required()
                    && self
                        .run
                        .as_ref()
                        .and_then(|run| run.attempt.as_ref())
                        .is_none_or(|attempt| attempt.computer_step.is_none()),
            );
        }
        if self.native_context_settling() {
            return Ok(false);
        }
        if self.owned_document_attempt_active() {
            let issued = self
                .run
                .as_ref()
                .and_then(|run| run.attempt.as_ref())
                .and_then(|attempt| attempt.owned_document.as_ref())
                .is_some_and(|owned| owned.input_issued);
            if issued {
                self.finish_attempt(SkillResult::Failed {
                    code: FailureCode::EffectUnknown,
                    reason: "the owned setter was already dispatched and cannot be replayed".into(),
                })?;
                return Ok(true);
            }
            return self.dispatch_owned_document_action();
        }
        if self.relocate_known_toggle()? {
            return Ok(true);
        }
        self.maintain_menu_result_binding();
        self.maintain_keyboard_result_binding();
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
                now_ms: self.now_ms,
                max_age_ms: self.cfg.evidence_max_age_ms,
            })
        };
        match step {
            SkillStep::Act(plan) => self.start_dispatch(*plan),
            SkillStep::BrowserAct(plan) => self.start_browser_dispatch(*plan),
            SkillStep::SelectRow(plan) => self.start_row_selection_dispatch(*plan),
            SkillStep::AwaitEvidence { after_ms } => self.attempt_await(after_ms),
            SkillStep::Done(result) => {
                if result == SkillResult::AlreadyDone
                    && self.run.as_ref().is_some_and(|run| {
                        run.goal.execution_domain
                        == interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
                    })
                {
                    let original = self
                        .run
                        .as_ref()
                        .and_then(|run| run.attempt.as_ref())
                        .and_then(|attempt| attempt.skill.browser_original_binding())
                        .cloned();
                    if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
                        attempt.browser_original_binding = original;
                    }
                    return self.begin_browser_current_ui(
                        crate::verification::BrowserCurrentUiFence::NoInputNeeded {
                            fence_ms: self.now_ms,
                        },
                    );
                }
                self.finish_attempt(result)?;
                Ok(true)
            }
        }
    }

    fn relocate_known_toggle(&mut self) -> Step {
        let eligible = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| {
                attempt.goal_target
                    && !attempt.mechanical_unknown
                    && attempt.skill.may_relocate_known_toggle()
                    && attempt
                        .skill
                        .input_floor()
                        .is_some_and(|floor| self.state.latest_since(floor).is_some())
            });
        if !eligible {
            return Ok(false);
        }
        self.maintain_goal_binding();
        let Some(run) = self.run.as_ref() else {
            return Ok(false);
        };
        let Some(binding) = self
            .bindings
            .current(&run.goal.target.semantic_target)
            .cloned()
        else {
            return Ok(false);
        };
        let Some(env) = self.state.latest() else {
            return Ok(false);
        };
        let Some(object) = object_for(env, &binding.fingerprint_digest) else {
            return Ok(false);
        };
        let label = object
            .title
            .as_deref()
            .or(object.description.as_deref())
            .unwrap_or("");
        let class = run
            .pack
            .effect_class(&object.role, label)
            .raise(interaction_contracts::desktop::effect::EffectClass::ReversibleEdit);
        if gate(
            &Scope {
                goal: &run.goal,
                plan_targets: &run.plan_targets,
                pack: &run.pack,
                env,
            },
            class,
            Some(&binding.semantic_target),
            false,
        ) != Admission::Allow
        {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::ContextUnresolved,
                reason: "the relocated target's effect is not authorized".into(),
            })?;
            return Ok(true);
        }
        let changed = self
            .run
            .as_mut()
            .and_then(|run| run.attempt.as_mut())
            .is_some_and(|attempt| attempt.skill.relocate_known_toggle(&binding));
        if changed {
            self.note(
                "fresh target revalidated after AX completion; the next input needs a new permit",
            );
        }
        Ok(changed)
    }

    fn register_browser_current_ui(
        &mut self,
        fence: crate::verification::BrowserCurrentUiFence,
    ) -> Result<(), String> {
        let Some(run) = self.run.as_ref() else {
            return Err("browser CurrentUi verification has no active root".into());
        };
        let Some(profile) = run.goal.browser_page_profile else {
            return Err("browser CurrentUi verification has no frozen page profile".into());
        };
        let scope = self
            .state
            .browser_scope()
            .cloned()
            .ok_or_else(|| "browser CurrentUi verification has no active scope".to_string())?;
        let active = self
            .tasks
            .active()
            .ok_or_else(|| "browser CurrentUi verification has no durable root".to_string())?;
        let (attempt_id, original_binding, deadline_ms, intent_id) = {
            let attempt = run
                .attempt
                .as_ref()
                .ok_or_else(|| "browser CurrentUi verification has no attempt".to_string())?;
            (
                attempt.id.clone(),
                attempt.browser_original_binding.clone().ok_or_else(|| {
                    "browser CurrentUi verification has no original binding".to_string()
                })?,
                attempt.deadline_ms,
                attempt.last_input_intent_id.clone(),
            )
        };
        let session_epoch = self
            .device
            .session_epoch()
            .ok_or_else(|| "browser CurrentUi verification has no Device session".to_string())?;
        let expected_checked = match &run.goal.desired {
            ExpectedValue::Bool { value } => *value,
            _ => return Err("browser CurrentUi verification goal is not boolean".into()),
        };
        match fence {
            crate::verification::BrowserCurrentUiFence::Submitted { .. } if intent_id.is_none() => {
                return Err("a submitted browser receipt has no intent id".into());
            }
            crate::verification::BrowserCurrentUiFence::NoInputNeeded { .. }
                if intent_id.is_some() =>
            {
                return Err("an already-satisfied browser step unexpectedly has an intent".into());
            }
            _ => {}
        }
        let input = crate::verification::BrowserCurrentUiInput {
            root_task_id: run.goal.root_task_id.clone(),
            task_revision: active.row.task_revision,
            root_epoch: active.row.root_epoch,
            attempt_id,
            intent_id,
            session_epoch,
            browser_page_profile: profile,
            goal_app: run.goal.scope.app.clone(),
            scope,
            original_binding,
            expected_checked,
            fence,
            deadline_ms,
        };
        self.run
            .as_mut()
            .ok_or_else(|| {
                "browser CurrentUi verification root ended during registration".to_string()
            })?
            .verification
            .register_browser_current_ui_input(input)
            .map_err(|reason| format!("browser CurrentUi fence was rejected: {}", reason.code()))?;
        let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) else {
            return Err("browser CurrentUi verification attempt ended during registration".into());
        };
        attempt.browser_current_ui_no_input = matches!(
            fence,
            crate::verification::BrowserCurrentUiFence::NoInputNeeded { .. }
        );
        attempt.browser_current_ui_fence_ms = Some(match fence {
            crate::verification::BrowserCurrentUiFence::Submitted { input_span } => {
                input_span.to_ms
            }
            crate::verification::BrowserCurrentUiFence::NoInputNeeded { fence_ms } => fence_ms,
        });
        attempt.browser_current_ui_pending = true;
        attempt.browser_current_ui_last_snapshot = None;
        attempt.browser_current_ui_last_rejection = None;
        Ok(())
    }

    fn begin_browser_current_ui(
        &mut self,
        fence: crate::verification::BrowserCurrentUiFence,
    ) -> Step {
        if let Err(reason) = self.register_browser_current_ui(fence) {
            self.note(&reason);
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                reason,
            })?;
            return Ok(true);
        }
        let after_ms = match fence {
            crate::verification::BrowserCurrentUiFence::Submitted { input_span } => {
                input_span.to_ms.saturating_add(1)
            }
            crate::verification::BrowserCurrentUiFence::NoInputNeeded { fence_ms } => {
                fence_ms.saturating_add(1)
            }
        };
        let sent = self.await_source(
            after_ms,
            "verifying the browser checkbox from a new independent page read",
            true,
        )?;
        if sent && let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.observations_used += 1;
        }
        Ok(sent)
    }

    fn browser_current_ui_proof(
        &self,
    ) -> Option<
        Result<
            crate::verification::BrowserCurrentUiProof,
            crate::verification::BrowserCurrentUiRejection,
        >,
    > {
        let run = self.run.as_ref()?;
        let attempt = run.attempt.as_ref()?;
        let active = self.tasks.active()?;
        let scope = self.state.browser_scope()?;
        let latest = self.state.browser_latest()?;
        let reference = self.browser_read_binding_ref.as_ref()?;
        let binding = self.state.current_browser_binding(reference)?;
        let profile = run.goal.browser_page_profile?;
        Some(run.verification.verify_browser_current_ui(
            crate::verification::BrowserCurrentUiReadback {
                root_task_id: &run.goal.root_task_id,
                task_revision: active.row.task_revision,
                root_epoch: active.row.root_epoch,
                attempt_id: &attempt.id,
                intent_id: attempt.last_input_intent_id.as_deref(),
                session_epoch: self.device.session_epoch()?,
                browser_page_profile: profile,
                goal_app: &run.goal.scope.app,
                scope,
                snapshot: &latest.snapshot,
                binding: &binding.proof,
                now_ms: self.now_ms,
                max_age_ms: self.cfg.evidence_max_age_ms.min(2_000),
            },
        ))
    }

    fn pending_browser_current_ui(&mut self) -> Step {
        let Some((deadline_ms, fence_ms, last_snapshot, no_input, used, quota)) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| {
                attempt.browser_current_ui_pending.then_some((
                    attempt.deadline_ms,
                    attempt.browser_current_ui_fence_ms?,
                    attempt.browser_current_ui_last_snapshot.clone(),
                    attempt.browser_current_ui_no_input,
                    attempt.observations_used,
                    attempt.quota.observations,
                ))
            })
        else {
            return Ok(false);
        };
        if self
            .run
            .as_ref()
            .is_some_and(|run| run.stop_wait_since.is_some())
        {
            return Ok(false);
        }
        if self.now_ms >= deadline_ms {
            return self
                .end_pending_browser_current_ui("the browser checkbox readback deadline expired");
        }
        let current = self.state.browser_latest();
        if current.is_some_and(|current| {
            current.snapshot.observed_ms > fence_ms
                && current.snapshot.observed_ms < deadline_ms
                && current.snapshot.read_finished_ms <= self.now_ms
                && Some(&current.snapshot.snapshot_id) != last_snapshot.as_ref()
        }) {
            match self.browser_current_ui_proof() {
                Some(Ok(proof)) => {
                    let result = proof.into_effect_result();
                    crate::verification::conclude_current_ui(&mut self.storage, result)?;
                    if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
                        attempt.browser_current_ui_pending = false;
                    }
                    if let Some(run) = self.run.as_mut() {
                        run.wait = None;
                        run.stalls = 0;
                    }
                    self.finish_attempt(if no_input {
                        SkillResult::AlreadyDone
                    } else {
                        SkillResult::Succeeded
                    })?;
                    self.tasks
                        .predicates_satisfied(&mut self.storage, self.now_ms)?;
                    self.note("a separate fresh browser read verified the frozen checkbox goal");
                    return Ok(true);
                }
                Some(Err(rejection)) => {
                    let snapshot_id = current.unwrap().snapshot.snapshot_id.clone();
                    if matches!(
                        rejection,
                        crate::verification::BrowserCurrentUiRejection::ValueMismatch
                            | crate::verification::BrowserCurrentUiRejection::BindingMismatch
                            | crate::verification::BrowserCurrentUiRejection::WitnessInvalid
                            | crate::verification::BrowserCurrentUiRejection::ScopeMismatch
                            | crate::verification::BrowserCurrentUiRejection::AttemptMismatch
                    ) {
                        return self.end_pending_browser_current_ui(&format!(
                            "browser checkbox readback was rejected ({})",
                            rejection.code()
                        ));
                    }
                    if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
                        attempt.browser_current_ui_last_snapshot = Some(snapshot_id);
                        attempt.browser_current_ui_last_rejection = Some(rejection);
                    }
                }
                None => {}
            }
        }
        if used >= quota {
            return self.end_pending_browser_current_ui(
                "the browser checkbox did not produce a verifiable fresh read",
            );
        }
        let latest_after = self
            .state
            .browser_latest()
            .map(|current| current.snapshot.observed_ms.saturating_add(1));
        let after_ms = latest_after
            .unwrap_or(fence_ms.saturating_add(1))
            .max(fence_ms.saturating_add(1));
        let sent = self.await_source(
            after_ms,
            "reading the browser checkbox again for independent verification",
            true,
        )?;
        if sent && let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.observations_used += 1;
        }
        Ok(sent)
    }

    fn end_pending_browser_current_ui(&mut self, reason: &str) -> Step {
        let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.take()) else {
            return Ok(false);
        };
        self.device.forget_attempt_waits();
        self.tasks.attempt_ended(
            &mut self.storage,
            &attempt.id,
            if attempt.mechanical_unknown {
                AttemptState::Unknown
            } else {
                AttemptState::Settled
            },
            !attempt.input_reached,
        )?;
        self.abandon_work()?;
        if attempt.input_reached {
            self.tasks
                .input_ended_unverified(&mut self.storage, self.now_ms, reason)?;
        } else {
            self.fail_root(reason)?;
        }
        self.note(format!(
            "browser CurrentUi verification ended without proof: {reason}"
        ));
        Ok(true)
    }

    fn attempt_await(&mut self, after_ms: u64) -> Step {
        let browser_goal = self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
        });
        let arrived = if browser_goal {
            self.state
                .browser_latest()
                .is_some_and(|current| current.snapshot.observed_ms >= after_ms)
        } else {
            self.state.latest_since(after_ms).is_some()
        };
        if arrived {
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
        let native_stream = self
            .device
            .identity()
            .is_some_and(|id| id.clock_domain.starts_with("macos/"));
        let sent = self.await_source(
            after_ms,
            "evidence from after the input",
            !native_stream
                && !self.menu_same_watch_required()
                && self
                    .run
                    .as_ref()
                    .and_then(|run| run.attempt.as_ref())
                    .is_none_or(|attempt| attempt.computer_step.is_none()),
        )?;
        if sent && let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.observations_used += 1;
        }
        Ok(sent)
    }

    fn start_browser_dispatch(&mut self, plan: BrowserActionPlan) -> Step {
        let Some((profile, target, attempt_id, attempt_deadline)) =
            self.run.as_ref().and_then(|run| {
                let profile = run.goal.browser_page_profile?;
                let attempt = run.attempt.as_ref()?;
                Some((
                    profile,
                    run.goal.target.semantic_target.clone(),
                    attempt.id.clone(),
                    attempt.deadline_ms,
                ))
            })
        else {
            return self.input_never_left(
                FailureCode::ContextUnresolved,
                "the browser action lost its frozen Root or Attempt",
            );
        };
        if target.as_str() != profile.semantic_target()
            || !matches!(plan.action, BrowserAction::SetCheckbox { checked } if checked != plan.binding.checked)
        {
            return self.input_never_left(
                FailureCode::EffectNotAuthorized,
                "the browser action does not change the frozen checkbox target",
            );
        }
        let Some(observation) = self.browser_execution_observation(profile) else {
            return self.input_never_left(
                FailureCode::SourceInvalid,
                "the browser checkbox read is no longer current",
            );
        };
        if observation.binding.as_ref() != &plan.binding
            || observation.valid_until_ms <= self.now_ms
            || self.now_ms >= attempt_deadline
        {
            return self.input_never_left(
                FailureCode::SourceInvalid,
                "the browser action proof changed before Device observation",
            );
        }
        let execution_observation = ExecutionObservation::Browser {
            observation: Box::new(observation.clone()),
        };
        let observe_corr = self.send(
            Expect::Observe {
                attempt: attempt_id.clone(),
                observation_id: observation.observation_id.clone(),
            },
            DeviceCommand::Observe(execution_observation.clone()),
        );
        if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.browser_original_binding = Some(plan.binding.clone());
            attempt.browser_input_span = None;
            attempt.last_input_intent_id = None;
            attempt.last_input_path = None;
            attempt.dispatch = Some(Dispatch {
                attempt: attempt_id,
                plan: DispatchPlan::Browser(Box::new(plan)),
                observation: execution_observation,
                reply_corr: Some(observe_corr),
                permit: None,
                intent_id: None,
                stage: Stage::Observing,
                started_ms: self.now_ms,
            });
        }
        Ok(true)
    }

    fn dispatch_owned_document_action(&mut self) -> Step {
        let Some((admitted, value, action)) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.owned_document.as_ref())
            .map(|owned| {
                (
                    owned.admitted_proof.clone(),
                    owned.value.clone(),
                    owned.action,
                )
            })
        else {
            return Ok(false);
        };
        if action == OwnedDocumentActionKind::Save && !self.cfg.allow_owned_document_save {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::EffectNotAuthorized,
                reason: "the Host has not authorized owned-document Save".into(),
            })?;
            return Ok(true);
        }
        let Some(current) = self.owned_document_proof_for(&admitted.semantic_target) else {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::SourceInvalid,
                reason: "the owned receiver proof is unavailable".into(),
            })?;
            return Ok(true);
        };
        if !admitted.same_instance(&current) {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::ContextUnresolved,
                reason: "the registered document receiver changed before input".into(),
            })?;
            return Ok(true);
        }
        let plan_result = match action {
            OwnedDocumentActionKind::SetValue => OwnedDocumentActionPlan::new(current, value),
            OwnedDocumentActionKind::Save => OwnedDocumentActionPlan::save(current, value),
        };
        let plan = match plan_result {
            Ok(plan) => plan,
            Err(reason) => {
                self.finish_attempt(SkillResult::Failed {
                    code: FailureCode::EffectNotAuthorized,
                    reason: reason.into(),
                })?;
                return Ok(true);
            }
        };
        self.start_owned_document_dispatch(plan)
    }

    fn start_owned_document_dispatch(&mut self, plan: OwnedDocumentActionPlan) -> Step {
        let Some(registration) = self.cfg.owned_document_registration.as_deref() else {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::EffectNotAuthorized,
                reason: "the private owned-document registration is unavailable".into(),
            })?;
            return Ok(true);
        };
        let Some((attempt_id, attempt_deadline, admitted)) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| {
                attempt.owned_document.as_ref().map(|owned| {
                    (
                        attempt.id.clone(),
                        attempt.deadline_ms,
                        owned.admitted_proof.clone(),
                    )
                })
            })
        else {
            return Ok(false);
        };
        if !admitted.same_instance(plan.proof()) {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::ContextUnresolved,
                reason: "the owned receiver identity no longer matches this attempt".into(),
            })?;
            return Ok(true);
        }
        let Some(env) = self.state.latest().cloned() else {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::SourceInvalid,
                reason: "the source snapshot for the owned action disappeared".into(),
            })?;
            return Ok(true);
        };
        let root_deadline = self
            .tasks
            .active()
            .and_then(|active| active.row.deadline_ms)
            .unwrap_or(self.now_ms);
        let observation = match self.device.owned_document_observation(
            &plan,
            registration,
            self.now_ms,
            self.cfg.evidence_max_age_ms,
            self.cfg.evidence_ttl_ms,
            root_deadline.min(attempt_deadline),
        ) {
            Ok(observation) => observation,
            Err(reason) => {
                self.finish_attempt(SkillResult::Failed {
                    code: FailureCode::SourceInvalid,
                    reason: reason.into(),
                })?;
                return Ok(true);
            }
        };
        let now = self.now_ms;
        let execution_observation = ExecutionObservation::Native {
            observation: Box::new(observation.clone()),
        };
        let observe_corr = self.send(
            Expect::Observe {
                attempt: attempt_id.clone(),
                observation_id: observation.observation_id.clone(),
            },
            DeviceCommand::Observe(execution_observation.clone()),
        );
        if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            let owned = attempt
                .owned_document
                .as_mut()
                .expect("an owned dispatch belongs to an owned attempt");
            owned.input_issued = true;
            owned.input_proof = Some(plan.proof().clone());
            attempt.last_input_path = Some(ExecutionPath::Ax);
            attempt.last_input_binding = None;
            attempt.last_input_text = Some(plan.expected_value().to_string());
            attempt.last_input_window = env.snapshot.window_context.clone();
            attempt.last_input_intent_id = None;
            attempt.text_input_end_ms = None;
            attempt.text_input_span = None;
            attempt.text_current_ui_pending = false;
            attempt.text_current_ui_last_snapshot = None;
            attempt.dispatch = Some(Dispatch {
                attempt: attempt_id,
                plan: DispatchPlan::OwnedDocument(plan),
                observation: execution_observation,
                reply_corr: Some(observe_corr),
                permit: None,
                intent_id: None,
                stage: Stage::Observing,
                started_ms: now,
            });
        }
        Ok(true)
    }

    // ---- The way to the device ---------------------------------------------------------------------------

    pub(super) fn keyboard_dispatch_matches_current(&self) -> bool {
        let Some(dispatch) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
        else {
            return true;
        };
        let plan = match &dispatch.plan {
            DispatchPlan::Generic(plan) => plan,
            DispatchPlan::RowSelection(_) => return true,
            DispatchPlan::OwnedDocument(_) | DispatchPlan::Browser(_) => return false,
        };
        if !matches!(
            plan.primitive,
            interaction_contracts::desktop::primitive::Primitive::KeyPress { .. }
        ) {
            return true;
        }
        let Some(env) = self.state.latest() else {
            return false;
        };
        let Some(proof) = dispatch
            .observation
            .native()
            .and_then(|observation| observation.binding_proof.as_ref())
        else {
            return false;
        };
        let Some(scope) = proof.focus_scope.as_ref() else {
            return false;
        };
        env.snapshot.menu_context.is_none()
            && env.snapshot.focus_context.as_ref().is_some_and(|current| {
                current.same_identity(&scope.context)
                    && current.verified_source_sequence == env.snapshot.source_sequence
                    && current.verified_observed_ms == env.snapshot.observed_ms
            })
            && env.snapshot.objects.iter().any(|object| {
                object.instance_ref == proof.instance
                    && object.frame == proof.display_frame
                    && instance_fingerprint(&env.snapshot, object) == proof.fingerprint
            })
    }

    fn row_selection_dispatch_matches_current(&self, dispatch: &Dispatch) -> bool {
        let DispatchPlan::RowSelection(plan) = &dispatch.plan else {
            return true;
        };
        let binding = &plan.binding;
        let Some(env) = self.state.latest() else {
            return false;
        };
        let Some(proof) = dispatch
            .observation
            .native()
            .and_then(|observation| observation.binding_proof.as_ref())
        else {
            return false;
        };
        self.state.row_scope_usable(
            &binding.table_scope,
            self.now_ms,
            self.cfg.evidence_max_age_ms,
        ) && proof.binding == binding.binding_ref
            && env.snapshot.snapshot_id == proof.snapshot_id
            && env.revision == binding.env_revision
            && env.layer_revision == binding.layer_revision
            && env.snapshot.source_sequence == binding.source.source_sequence
            && focused_window_id(&env.snapshot) == Some(binding.source.window_id.as_str())
            && self
                .bindings
                .current_row(&binding.semantic_target)
                .is_some_and(|current| {
                    current.binding_ref == binding.binding_ref
                        && current.snapshot_id == env.snapshot.snapshot_id
                        && current.source.source_sequence == env.snapshot.source_sequence
                        && current.source.selected == Some(false)
                        && current.source.ax_selected_settable == Some(true)
                })
    }

    fn keyboard_input_key(
        &self,
        plan: &ActionPlan,
    ) -> Result<Option<LayoutIndependentSpecialKey>, &'static str> {
        let interaction_contracts::desktop::primitive::Primitive::KeyPress { key, modifiers } =
            &plan.primitive
        else {
            return if plan.path == crate::reaction::skills::ExecutionPath::Keyboard {
                Err("keyboard path needs one canonical KeyPress")
            } else {
                Ok(None)
            };
        };
        let Some(run) = self.run.as_ref() else {
            return Err("keyboard input needs a registered running root");
        };
        if plan.path != crate::reaction::skills::ExecutionPath::Keyboard
            || run.keyboard_result_context.is_some()
            || key != "Space"
            || *modifiers != interaction_contracts::desktop::primitive::Modifiers::default()
            || run
                .pack
                .keyboard_toggle(&plan.binding.semantic_target)
                .is_none()
            || run.attempt.as_ref().is_none_or(|attempt| {
                attempt.skill.skill() != Skill::EnsureValue
                    || !attempt.goal_target
                    || attempt.skill.actions() != 1
                    || attempt.skill.keyboard_context().is_none()
            })
            || !self.evidence_usable()
        {
            return Err("only the registered focused toggle's one Space key is authorized");
        }
        Ok(Some(LayoutIndependentSpecialKey::Space))
    }

    fn start_row_selection_dispatch(
        &mut self,
        plan: crate::reaction::skills::row::RowSelectionActionPlan,
    ) -> Step {
        let Some((attempt_id, deadline_ms, current)) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| {
                (attempt.skill.skill() == Skill::SelectRow && attempt.dispatch.is_none()).then(
                    || {
                        (
                            attempt.id.clone(),
                            attempt.deadline_ms,
                            self.bindings
                                .current_row(&plan.binding.semantic_target)
                                .cloned(),
                        )
                    },
                )
            })
            .and_then(|(id, deadline, binding)| binding.map(|binding| (id, deadline, binding)))
        else {
            return self.input_never_left(
                FailureCode::ContextUnresolved,
                "row selection does not belong to the active dedicated row attempt",
            );
        };
        let Some(env) = self.state.latest().cloned() else {
            return self.input_never_left(FailureCode::SourceInvalid, "there is no row evidence");
        };
        if !self.state.row_scope_usable(
            &current.table_scope,
            self.now_ms,
            self.cfg.evidence_max_age_ms,
        ) || current.binding_ref != plan.binding.binding_ref
            || current.snapshot_id != env.snapshot.snapshot_id
            || current.source.source_sequence != env.snapshot.source_sequence
            || current.source.selected != Some(false)
            || current.source.ax_selected_settable != Some(true)
            || plan.binding != current
        {
            return self.input_never_left(
                FailureCode::EvidenceExpired,
                "the row binding changed before its single selection input",
            );
        }
        let root_deadline = self
            .tasks
            .active()
            .and_then(|active| active.row.deadline_ms)
            .unwrap_or(self.now_ms);
        let freshness = if self.state.row_scope_usable(
            &current.table_scope,
            self.now_ms,
            self.cfg.evidence_max_age_ms,
        ) {
            SourceFreshness::Verified
        } else {
            SourceFreshness::Uncertain
        };
        let source_deadline = if self.state.requires_frames() {
            match self.state.row_scope_frame_deadline(
                &current.table_scope,
                self.now_ms,
                self.cfg.evidence_max_age_ms,
            ) {
                Some(deadline) => Some(deadline),
                None => {
                    return self.input_never_left(
                        FailureCode::SourceInvalid,
                        "native frame evidence is unavailable for row selection",
                    );
                }
            }
        } else {
            None
        };
        let observation = match self.device.row_selection_observation(
            &env,
            &current,
            freshness,
            self.cfg.evidence_ttl_ms,
            root_deadline.min(deadline_ms),
            source_deadline,
            self.now_ms,
            self.cfg.evidence_max_age_ms,
        ) {
            Ok(observation) => observation,
            Err(reason) => {
                return self.input_never_left(FailureCode::SourceInvalid, reason);
            }
        };
        let execution_observation = ExecutionObservation::Native {
            observation: Box::new(observation.clone()),
        };
        let observe_corr = self.send(
            Expect::Observe {
                attempt: attempt_id,
                observation_id: observation.observation_id.clone(),
            },
            DeviceCommand::Observe(execution_observation.clone()),
        );
        let now = self.now_ms;
        if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.last_input_path = Some(ExecutionPath::Ax);
            attempt.last_input_binding = None;
            attempt.last_input_text = None;
            attempt.last_input_window = env.snapshot.window_context.clone();
            attempt.last_input_intent_id = None;
            attempt.text_input_end_ms = None;
            attempt.text_input_span = None;
            attempt.text_current_ui_pending = false;
            attempt.text_current_ui_last_snapshot = None;
            attempt.dispatch = Some(Dispatch {
                attempt: attempt.id.clone(),
                plan: DispatchPlan::RowSelection(Box::new(plan)),
                observation: execution_observation,
                reply_corr: Some(observe_corr),
                permit: None,
                intent_id: None,
                stage: Stage::Observing,
                started_ms: now,
            });
        }
        Ok(true)
    }

    /// A scoped Revisit navigation input is authorized only by its exact
    /// frozen pair and the current Revisit phase. Recheck the skill, plan,
    /// effect class, binding registry, Source closure, and pointer primitive at
    /// dispatch start and again after the device's reserve/map/activate path.
    fn scoped_revisit_action_matches_current(
        &self,
        plan: &ActionPlan,
        expected_source_sequence: Option<u64>,
    ) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let manual_action = run
            .goal
            .scoped_commit
            .as_ref()
            .filter(|commit| {
                commit.receipt.kind
                    == interaction_contracts::desktop::task::ScopedCommitKind::Manual
            })
            .and_then(|commit| commit.action.as_ref());
        if run.goal.scoped_revisit.is_none()
            && manual_action.is_none()
            && !has_registered_radio_choice_target(&run.goal, &run.pack)
        {
            return true;
        }
        if run.goal.validate().is_err()
            || run.goal.execution_domain
                != interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative
            || (manual_action.is_some() && run.goal.confirmation_scope != ConfirmationScope::Commit)
            || (manual_action.is_none()
                && run.goal.confirmation_scope != ConfirmationScope::UiRevisit)
        {
            return false;
        }
        let Some(task_plan) = self
            .tasks
            .active()
            .and_then(|active| active.row.plan.as_ref())
        else {
            return false;
        };
        if !scoped_revisit_plan_is_allowed(&run.goal, &run.pack, task_plan) {
            return false;
        }
        let Some(attempt) = run.attempt.as_ref() else {
            return false;
        };
        let Some(next) = self.next_subgoal() else {
            return false;
        };
        if attempt.skill.skill() == Skill::ScrollTo {
            let Some(container) = scoped_number_form_scroll_container(&run.goal, &run.pack) else {
                return false;
            };
            let Some(spec) = next.scroll.as_ref() else {
                return false;
            };
            if attempt.computer_step.is_some()
                || attempt.goal_target
                || attempt.subgoal != next.id
                || !scoped_number_scroll_step_matches(&run.goal, container, &next)
                || spec.validate().is_err()
                || attempt.admitted_effect_class
                    != interaction_contracts::desktop::effect::EffectClass::Navigation
                || plan.binding.semantic_target != spec.container.semantic_target
                || self.bindings.current(&spec.container.semantic_target) != Some(&plan.binding)
                || plan.path != ExecutionPath::Pointer
                || !self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
            {
                return false;
            }
            let Some(env) = self.state.latest() else {
                return false;
            };
            let Some(pair) = run.goal.scoped_revisit.as_ref() else {
                return false;
            };
            let Some(controls) = crate::reaction::skills::scoped_revisit_controls(
                &run.goal.target.semantic_target,
                pair,
                &run.pack,
                env,
            ) else {
                return false;
            };
            if env.snapshot.observed_ms > self.now_ms
                || expected_source_sequence
                    .is_some_and(|sequence| sequence != env.snapshot.source_sequence)
                || env.page.as_deref() != Some(controls.home_page.as_str())
                || plan.binding.continuity.scope
                    != interaction_contracts::desktop::binding::BindingProofScope::WholeStructure
                || !plan.binding.device_context.supports_native_input()
            {
                return false;
            }
            let Some(object) = object_for(env, &plan.binding.fingerprint_digest) else {
                return false;
            };
            if object.instance_ref != plan.binding.instance
                || object.role != container.role
                || object.title.as_deref().or(object.description.as_deref())
                    != Some(container.label.as_str())
                || !object.enabled
                || !object.in_view
                || object.secure
            {
                return false;
            }
            let (x, y) = object.frame.center();
            return match &plan.primitive {
                interaction_contracts::desktop::primitive::Primitive::PointerPosition {
                    x: actual_x,
                    y: actual_y,
                } => {
                    self.cfg.native_window_scope.is_some()
                        && (*actual_x, *actual_y) == (x.round() as i32, y.round() as i32)
                }
                interaction_contracts::desktop::primitive::Primitive::Scroll { dx, dy } => {
                    *dx == 0 && *dy == -(spec.pixels as i32)
                }
                _ => false,
            };
        }
        if attempt.skill.skill() == Skill::SelectByLabel
            && scoped_radio_choice_values(&run.goal, &run.pack).is_some()
        {
            let Some(choices) = scoped_radio_choice_values(&run.goal, &run.pack) else {
                return false;
            };
            let Some(select) = next.select.as_ref() else {
                return false;
            };
            if attempt.computer_step.is_some()
                || !attempt.goal_target
                || attempt.sequence != 0
                || attempt.menu_opening.is_some()
                || attempt.subgoal != next.id
                || next.skill != Skill::SelectByLabel.name()
                || next.target.as_ref() != Some(&run.goal.target.semantic_target)
                || next.scroll.is_some()
                || select.validate().is_err()
                || !choices.contains(&select.choice_label.as_str())
                || !matches!(
                    &run.goal.desired,
                    ExpectedValue::Text { value } if value == &select.choice_label
                )
                || !matches!(
                    attempt.skill.menu_desired(),
                    Some(ExpectedValue::Text { value }) if value == &select.choice_label
                )
                || attempt.admitted_effect_class
                    != interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
                || plan.path != ExecutionPath::Ax
                || !matches!(
                    &plan.primitive,
                    interaction_contracts::desktop::primitive::Primitive::AxAction {
                        action: interaction_contracts::desktop::primitive::AxActionKind::Press,
                    }
                )
                || plan.binding.semantic_target != run.goal.target.semantic_target
                || self.bindings.current(&run.goal.target.semantic_target) != Some(&plan.binding)
                || !self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
            {
                return false;
            }
            let Some(env) = self.state.latest() else {
                return false;
            };
            if env.snapshot.observed_ms > self.now_ms
                || expected_source_sequence
                    .is_some_and(|sequence| sequence != env.snapshot.source_sequence)
            {
                return false;
            }
            let Some(object) =
                scoped_revisit_primary_binding_object(&run.goal, &run.pack, env, &plan.binding)
            else {
                return false;
            };
            return object.role == "AXRadioButton" && object.instance_ref == plan.binding.instance;
        }
        if attempt.skill.skill() == Skill::SelectByLabel
            && scoped_choice_values(&run.goal, &run.pack).is_some()
        {
            let Some(choices) = scoped_choice_values(&run.goal, &run.pack) else {
                return false;
            };
            let Some(select) = next.select.as_ref() else {
                return false;
            };
            if attempt.computer_step.is_some()
                || !attempt.goal_target
                || attempt.subgoal != next.id
                || next.skill != Skill::SelectByLabel.name()
                || next.target.as_ref() != Some(&run.goal.target.semantic_target)
                || next.scroll.is_some()
                || select.validate().is_err()
                || !choices.contains(&select.choice_label.as_str())
                || !matches!(
                    &run.goal.desired,
                    ExpectedValue::Text { value } if value == &select.choice_label
                )
                || !matches!(
                    attempt.skill.menu_desired(),
                    Some(ExpectedValue::Text { value }) if value == &select.choice_label
                )
                || attempt.admitted_effect_class
                    != interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
                || plan.path != ExecutionPath::Ax
            {
                return false;
            }
            let Some(env) = self.state.latest() else {
                return false;
            };
            if env.snapshot.observed_ms > self.now_ms
                || expected_source_sequence
                    .is_some_and(|sequence| sequence != env.snapshot.source_sequence)
            {
                return false;
            }
            return match &plan.primitive {
                interaction_contracts::desktop::primitive::Primitive::AxAction {
                    action: interaction_contracts::desktop::primitive::AxActionKind::ShowMenu,
                } => {
                    attempt.sequence == 0
                        && attempt.menu_opening.is_none()
                        && self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
                        && plan.binding.semantic_target == run.goal.target.semantic_target
                        && self.bindings.current(&run.goal.target.semantic_target)
                            == Some(&plan.binding)
                        && scoped_revisit_primary_binding_object(
                            &run.goal,
                            &run.pack,
                            env,
                            &plan.binding,
                        )
                        .is_some()
                }
                interaction_contracts::desktop::primitive::Primitive::AxAction {
                    action: interaction_contracts::desktop::primitive::AxActionKind::Press,
                } => {
                    attempt.sequence == 1
                        && attempt.menu_opening.is_some()
                        && self
                            .state
                            .menu_usable(self.now_ms, self.cfg.evidence_max_age_ms)
                        && plan.binding.rule == "menu_item"
                        && matches!(self.menu_input_scope(plan), Ok(Some(_)))
                }
                _ => false,
            };
        }
        if let Some(action) = manual_action
            && attempt.skill.skill() == Skill::Activate
        {
            if attempt.computer_step.is_some()
                || attempt.goal_target
                || attempt.subgoal != next.id
                || next.skill != Skill::Activate.name()
                || next.target.as_ref() != Some(&action.semantic_target)
                || next.scroll.is_some()
                || next.select.is_some()
                || attempt.admitted_effect_class
                    != interaction_contracts::desktop::effect::EffectClass::Commit
                || plan.binding.semantic_target != action.semantic_target
                || self.bindings.current(&action.semantic_target) != Some(&plan.binding)
                || plan.path != ExecutionPath::Ax
                || plan.primitive
                    != (interaction_contracts::desktop::primitive::Primitive::AxAction {
                        action: interaction_contracts::desktop::primitive::AxActionKind::Press,
                    })
            {
                return false;
            }
            let Some(env) = self.state.latest() else {
                return false;
            };
            if !self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
                || env.snapshot.observed_ms > self.now_ms
                || expected_source_sequence
                    .is_some_and(|sequence| sequence != env.snapshot.source_sequence)
                || scoped_commit_action_binding_object(&run.goal, &run.pack, env, &plan.binding)
                    .is_none()
            {
                return false;
            }
            return true;
        }
        if attempt.skill.skill() != Skill::Revisit {
            let form_scope =
                run.goal.allowed_scope.as_ref().is_some_and(|scope| {
                    scope.targets.len() == 1 && scope.targets[0].form.is_some()
                });
            if !form_scope {
                let expected_effect = match attempt.skill.skill() {
                    Skill::EnsureValue => {
                        interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
                    }
                    Skill::ReadValue => {
                        interaction_contracts::desktop::effect::EffectClass::ReadOnly
                    }
                    _ => return false,
                };
                if attempt.computer_step.is_some() || !attempt.goal_target {
                    return false;
                }
                if attempt.subgoal != next.id
                    || next.skill != attempt.skill.skill().name()
                    || next.target.as_ref() != Some(&run.goal.target.semantic_target)
                    || next.scroll.is_some()
                    || next.select.is_some()
                {
                    return false;
                }
                if attempt.admitted_effect_class != expected_effect {
                    return false;
                }
                if plan.binding.semantic_target != run.goal.target.semantic_target
                    || self.bindings.current(&run.goal.target.semantic_target)
                        != Some(&plan.binding)
                {
                    return false;
                }
                if !self.state.latest().is_some_and(|env| {
                    self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
                        && env.snapshot.observed_ms <= self.now_ms
                        && expected_source_sequence
                            .is_none_or(|sequence| sequence == env.snapshot.source_sequence)
                }) {
                    return false;
                }
                return true;
            }
            if attempt.skill.skill() != Skill::EnsureValue {
                // ReadValue completes from Source without an input dispatch.
                // No other skill can use this primary-target exception.
                return false;
            }
            let Some(env) = self.state.latest() else {
                return false;
            };
            if !self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
                || env.snapshot.observed_ms > self.now_ms
                || expected_source_sequence
                    .is_some_and(|sequence| sequence != env.snapshot.source_sequence)
            {
                return false;
            }
            let Some(object) =
                scoped_revisit_primary_binding_object(&run.goal, &run.pack, env, &plan.binding)
            else {
                return false;
            };
            if run
                .pack
                .effect_class(&object.role, run.goal.target.label.as_str())
                != interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
            {
                return false;
            }
            let scoped_form_text =
                run.goal.allowed_scope.as_ref().is_some_and(|scope| {
                    scope.targets.len() == 1 && scope.targets[0].form.is_some()
                }) && (matches!(&run.goal.desired, ExpectedValue::Text { .. })
                    || scoped_form_number_control(&run.goal, &run.pack).is_some());
            let (expected_primitive, expected_path) = if scoped_form_text {
                if attempt.skill.scoped_form_text_focus_action_pending() {
                    if object.focused
                        || !matches!(object.role.as_str(), "AXTextField" | "AXTextArea")
                        || !object.actions.contains(
                            &interaction_contracts::desktop::source::AxActionName::SetValue,
                        )
                    {
                        return false;
                    }
                    let (x, y) = object.frame.center();
                    (
                        interaction_contracts::desktop::primitive::Primitive::Click {
                            button: interaction_contracts::desktop::primitive::MouseButton::Left,
                            x: x.round() as i32,
                            y: y.round() as i32,
                            modifiers:
                                interaction_contracts::desktop::primitive::Modifiers::default(),
                        },
                        ExecutionPath::Pointer,
                    )
                } else if attempt.skill.scoped_form_text_set_value_action_pending() {
                    if !object.focused
                        || !object.actions.contains(
                            &interaction_contracts::desktop::source::AxActionName::SetValue,
                        )
                        || attempt
                            .skill
                            .scoped_form_focus_source_sequence()
                            .is_some_and(|sequence| env.snapshot.source_sequence <= sequence)
                    {
                        return false;
                    }
                    (
                        interaction_contracts::desktop::primitive::Primitive::AxAction {
                            action:
                                interaction_contracts::desktop::primitive::AxActionKind::SetValue {
                                    value: value_text(&run.goal.desired),
                                },
                        },
                        ExecutionPath::Ax,
                    )
                } else {
                    return false;
                }
            } else if object
                .actions
                .contains(&interaction_contracts::desktop::source::AxActionName::SetValue)
            {
                (
                    interaction_contracts::desktop::primitive::Primitive::AxAction {
                        action: interaction_contracts::desktop::primitive::AxActionKind::SetValue {
                            value: value_text(&run.goal.desired),
                        },
                    },
                    ExecutionPath::Ax,
                )
            } else if matches!(&run.goal.desired, ExpectedValue::Bool { .. })
                && object
                    .actions
                    .contains(&interaction_contracts::desktop::source::AxActionName::Press)
            {
                (
                    interaction_contracts::desktop::primitive::Primitive::AxAction {
                        action: interaction_contracts::desktop::primitive::AxActionKind::Press,
                    },
                    ExecutionPath::Ax,
                )
            } else {
                return false;
            };
            if attempt.computer_step.is_some() || !attempt.goal_target {
                return false;
            }
            if attempt.subgoal != next.id
                || next.skill != Skill::EnsureValue.name()
                || next.target.as_ref() != Some(&run.goal.target.semantic_target)
                || next.scroll.is_some()
                || next.select.is_some()
            {
                return false;
            }
            if attempt.admitted_effect_class
                != interaction_contracts::desktop::effect::EffectClass::ReversibleEdit
            {
                return false;
            }
            if plan.binding.semantic_target != run.goal.target.semantic_target
                || self.bindings.current(&run.goal.target.semantic_target) != Some(&plan.binding)
            {
                return false;
            }
            if plan.path != expected_path {
                return false;
            }
            if plan.primitive != expected_primitive {
                return false;
            }
            return true;
        }
        let Some(pair) = run.goal.scoped_revisit.as_ref() else {
            return false;
        };
        let Some(target) = attempt.skill.scoped_revisit_action_target() else {
            return false;
        };
        if attempt.computer_step.is_some()
            || !attempt.goal_target
            || attempt.skill.skill() != Skill::Revisit
            || attempt.skill.scoped_revisit_pair() != Some(pair)
            || attempt.admitted_effect_class
                != interaction_contracts::desktop::effect::EffectClass::Navigation
            || attempt.subgoal != next.id
            || next.skill != Skill::Revisit.name()
            || next.target.as_ref() != Some(&run.goal.target.semantic_target)
            || next.scroll.is_some()
            || next.select.is_some()
            || (target != &pair.home && target != &pair.away)
            || plan.binding.semantic_target != target.semantic_target
            || plan.path != ExecutionPath::Pointer
        {
            return false;
        }
        let Some(env) = self.state.latest() else {
            return false;
        };
        if !self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
            || env.snapshot.observed_ms > self.now_ms
            || expected_source_sequence
                .is_some_and(|sequence| sequence != env.snapshot.source_sequence)
            || self.bindings.current(&target.semantic_target) != Some(&plan.binding)
        {
            return false;
        }
        let Some(object) = scoped_revisit_binding_object(
            &run.goal.target.semantic_target,
            pair,
            &run.pack,
            env,
            &plan.binding,
            target,
        ) else {
            return false;
        };
        if run.pack.effect_class(
            &object.role,
            object
                .title
                .as_deref()
                .or(object.description.as_deref())
                .unwrap_or(""),
        ) != interaction_contracts::desktop::effect::EffectClass::Navigation
        {
            return false;
        }
        let (x, y) = object.frame.center();
        matches!(
            &plan.primitive,
            interaction_contracts::desktop::primitive::Primitive::Click {
                button: interaction_contracts::desktop::primitive::MouseButton::Left,
                x: click_x,
                y: click_y,
                modifiers,
            } if *click_x == x.round() as i32
                && *click_y == y.round() as i32
                && *modifiers == interaction_contracts::desktop::primitive::Modifiers::default()
        )
    }

    pub(super) fn scoped_revisit_dispatch_matches_current(&self, dispatch: &Dispatch) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let has_manual_commit_action = run
            .goal
            .scoped_commit
            .as_ref()
            .is_some_and(|commit| commit.action.is_some());
        if run.goal.scoped_revisit.is_none()
            && !has_manual_commit_action
            && !has_registered_radio_choice_target(&run.goal, &run.pack)
        {
            return true;
        }
        let DispatchPlan::Generic(plan) = &dispatch.plan else {
            return false;
        };
        let Some(attempt) = run.attempt.as_ref() else {
            return false;
        };
        let Some(observation) = dispatch.observation.native() else {
            return false;
        };
        let Some(proof) = observation.binding_proof.as_ref() else {
            return false;
        };
        let Some(env) = self.state.latest() else {
            return false;
        };
        if attempt.skill.skill() == Skill::SelectByLabel
            && scoped_choice_values(&run.goal, &run.pack).is_some()
        {
            let expected_purpose = match &plan.primitive {
                interaction_contracts::desktop::primitive::Primitive::AxAction {
                    action: interaction_contracts::desktop::primitive::AxActionKind::ShowMenu,
                } => interaction_contracts::desktop::device::ObservationPurpose::TargetInput,
                interaction_contracts::desktop::primitive::Primitive::AxAction {
                    action: interaction_contracts::desktop::primitive::AxActionKind::Press,
                } => interaction_contracts::desktop::device::ObservationPurpose::MenuSelection,
                _ => return false,
            };
            let Some(object) = env
                .snapshot
                .objects
                .iter()
                .find(|object| object.instance_ref == plan.binding.instance)
            else {
                return false;
            };
            return self.scoped_revisit_action_matches_current(
                plan,
                Some(dispatch.observation.source_sequence()),
            ) && self.menu_dispatch_matches_current()
                && observation.execution_domain
                    == interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative
                && observation.purpose == expected_purpose
                && observation.freshness == SourceFreshness::Verified
                && observation.session_epoch == env.snapshot.session_epoch
                && observation.target_ref == plan.binding.binding_ref.as_str()
                && proof.binding == plan.binding.binding_ref
                && proof.instance == plan.binding.instance
                && proof.snapshot_id == plan.binding.snapshot_id
                && proof.environment_revision == plan.binding.env_revision
                && proof.layer_revision == plan.binding.layer_revision
                && proof.fingerprint.digest() == plan.binding.fingerprint_digest
                && proof.display_frame == object.frame
                && proof.continuity == plan.binding.continuity
                && proof.device_context == plan.binding.device_context
                && proof.menu_scope.is_some()
                && proof.focus_scope.is_none();
        }
        let object = if attempt.skill.skill() == Skill::ScrollTo
            && scoped_number_form_scroll_container(&run.goal, &run.pack).is_some()
        {
            // The action-phase check below proves the exact registered
            // container; the dispatch proof must describe that same object.
            object_for(env, &plan.binding.fingerprint_digest)
        } else if attempt.skill.skill() == Skill::Revisit {
            let Some(pair) = run.goal.scoped_revisit.as_ref() else {
                return false;
            };
            let Some(target) = attempt.skill.scoped_revisit_action_target() else {
                return false;
            };
            scoped_revisit_binding_object(
                &run.goal.target.semantic_target,
                pair,
                &run.pack,
                env,
                &plan.binding,
                target,
            )
        } else if attempt.skill.skill() == Skill::Activate
            && run
                .goal
                .scoped_commit
                .as_ref()
                .and_then(|commit| commit.action.as_ref())
                .is_some_and(|action| action.semantic_target == plan.binding.semantic_target)
        {
            scoped_commit_action_binding_object(&run.goal, &run.pack, env, &plan.binding)
        } else {
            scoped_revisit_primary_binding_object(&run.goal, &run.pack, env, &plan.binding)
        };
        let Some(object) = object else {
            return false;
        };
        self.scoped_revisit_action_matches_current(
            plan,
            Some(dispatch.observation.source_sequence()),
        ) && observation.execution_domain
            == interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative
            && observation.purpose
                == interaction_contracts::desktop::device::ObservationPurpose::TargetInput
            && observation.freshness == SourceFreshness::Verified
            && observation.session_epoch == env.snapshot.session_epoch
            && observation.target_ref == plan.binding.binding_ref.as_str()
            && proof.binding == plan.binding.binding_ref
            && proof.instance == plan.binding.instance
            && proof.snapshot_id == plan.binding.snapshot_id
            && proof.environment_revision == plan.binding.env_revision
            && proof.layer_revision == plan.binding.layer_revision
            && proof.fingerprint.digest() == plan.binding.fingerprint_digest
            && proof.display_frame == object.frame
            && proof.continuity == plan.binding.continuity
            && proof.device_context == plan.binding.device_context
            && proof.menu_scope.is_none()
            && proof.focus_scope.is_none()
    }

    fn start_dispatch(&mut self, plan: ActionPlan) -> Step {
        if !self.scoped_revisit_action_matches_current(&plan, None) {
            return self.input_never_left(
                FailureCode::ContextUnresolved,
                "the scoped revisit input no longer matches its frozen pair and phase",
            );
        }
        if let Some(step) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.computer_step.clone())
        {
            let pointer_click = plan.path == ExecutionPath::Pointer
                && matches!(&plan.primitive, interaction_contracts::desktop::primitive::Primitive::Click {
                    button: interaction_contracts::desktop::primitive::MouseButton::Left,
                    modifiers, ..
                } if *modifiers == interaction_contracts::desktop::primitive::Modifiers::default());
            if !pointer_click || self.validate_computer_attempt_step(&step).is_err() {
                return self.input_never_left(
                    FailureCode::ContextUnresolved,
                    "the computer action no longer has its approved pointer semantics or scope",
                );
            }
        }
        let keyboard_key = match self.keyboard_input_key(&plan) {
            Ok(key) => key,
            Err(reason) => return self.input_never_left(FailureCode::EffectNotAuthorized, reason),
        };
        let direct_radio_choice_press = self.run.as_ref().is_some_and(|run| {
            run.attempt.as_ref().is_some_and(|attempt| {
                attempt.skill.skill() == Skill::SelectByLabel
                    && scoped_radio_choice_values(&run.goal, &run.pack).is_some()
                    && plan.path == ExecutionPath::Ax
                    && matches!(
                        &plan.primitive,
                        interaction_contracts::desktop::primitive::Primitive::AxAction {
                            action: interaction_contracts::desktop::primitive::AxActionKind::Press,
                        }
                    )
            })
        });
        // `scoped_revisit_action_matches_current` above has already required
        // the exact registered radio child Binding and frozen Text choice.
        // This is a TargetInput press, not a popup menu item transaction.
        let menu_scope = if direct_radio_choice_press {
            Ok(None)
        } else {
            self.menu_input_scope(&plan)
        };
        let menu_scope = match menu_scope {
            Ok(scope) => scope,
            Err(reason) => return self.input_never_left(FailureCode::EffectNotAuthorized, reason),
        };
        let Some(env) = self.state.latest().cloned() else {
            return self.input_never_left(FailureCode::SourceInvalid, "there is no evidence");
        };
        let attempt_id = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .map(|attempt| attempt.id.clone())
            .expect("a dispatch belongs to an active attempt");
        let root_deadline = self
            .tasks
            .active()
            .and_then(|active| active.row.deadline_ms)
            .unwrap_or(self.now_ms);
        let attempt_deadline = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .map_or(self.now_ms, |attempt| attempt.deadline_ms);
        let activating = matches!(
            plan.primitive,
            interaction_contracts::desktop::primitive::Primitive::ActivateWindow { .. }
        );
        let background_ax = self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        });
        let background_hash = plan.binding.continuity.scope.clone();
        let background_target_context_hash = match background_hash {
            BindingProofScope::BackgroundAx {
                target_context_hash,
            } => Some(target_context_hash),
            _ => None,
        };
        let freshness = if background_ax
            && self.background_ax_evidence_usable(background_target_context_hash.as_deref())
            || (activating && self.window_evidence_usable())
            || (env.snapshot.complete
                && focused_window_id(&env.snapshot).is_some()
                && !self.state.structure_unavailable())
        {
            SourceFreshness::Verified
        } else {
            SourceFreshness::Uncertain
        };
        let source_deadline = if self.state.requires_frames() && !activating && !background_ax {
            let deadline = if matches!(
                menu_scope,
                Some(interaction_contracts::desktop::binding::MenuBindingScope::Item { .. })
            ) {
                self.state
                    .menu_frame_deadline(self.now_ms, self.cfg.evidence_max_age_ms)
            } else {
                self.state
                    .frame_deadline(self.now_ms, self.cfg.evidence_max_age_ms)
            };
            match deadline {
                Some(deadline) => Some(deadline),
                None => {
                    return self.input_never_left(
                        FailureCode::SourceInvalid,
                        "native frame evidence is unavailable",
                    );
                }
            }
        } else {
            None
        };
        let built_observation = if let Some(key) = keyboard_key {
            if menu_scope.is_some() {
                return self.input_never_left(
                    FailureCode::EffectNotAuthorized,
                    "keyboard and menu input scopes cannot coexist",
                );
            }
            self.device.bound_focused_observation(
                &env,
                &plan.binding,
                freshness,
                self.cfg.evidence_ttl_ms,
                root_deadline.min(attempt_deadline),
                source_deadline,
                key,
            )
        } else if let Some(scope) = menu_scope {
            self.device.bound_scoped_observation(
                &env,
                &plan.binding,
                freshness,
                self.cfg.evidence_ttl_ms,
                root_deadline.min(attempt_deadline),
                source_deadline,
                scope,
            )
        } else {
            self.device.bound_observation(
                &env,
                &plan.binding,
                freshness,
                self.cfg.evidence_ttl_ms,
                root_deadline.min(attempt_deadline),
                source_deadline,
            )
        };
        let observation = match built_observation {
            Ok(observation) => observation,
            Err(why) => {
                self.note(format!("the evidence cannot be given to the device: {why}"));
                return self.input_never_left(FailureCode::SourceInvalid, why);
            }
        };
        let execution_observation = ExecutionObservation::Native {
            observation: Box::new(observation.clone()),
        };
        let observe_corr = self.send(
            Expect::Observe {
                attempt: attempt_id,
                observation_id: observation.observation_id.clone(),
            },
            DeviceCommand::Observe(execution_observation.clone()),
        );
        let now = self.now_ms;
        if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.last_input_path = Some(plan.path);
            attempt.last_input_binding = Some(plan.binding.clone());
            attempt.last_input_text = match &plan.primitive {
                interaction_contracts::desktop::primitive::Primitive::AxAction {
                    action:
                        interaction_contracts::desktop::primitive::AxActionKind::SetValue { value },
                } => Some(value.clone()),
                _ => None,
            };
            attempt.last_input_window = env.snapshot.window_context.clone();
            attempt.last_input_intent_id = None;
            attempt.text_input_end_ms = None;
            attempt.text_input_span = None;
            attempt.text_current_ui_pending = false;
            attempt.text_current_ui_last_snapshot = None;
            attempt.dispatch = Some(Dispatch {
                attempt: attempt.id.clone(),
                plan: DispatchPlan::Generic(Box::new(plan)),
                observation: execution_observation,
                reply_corr: Some(observe_corr),
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
    pub(super) fn observed(
        &mut self,
        expected_attempt: &AttemptId,
        expected_observation: &str,
        expected_corr: u64,
        ack: CommandAck,
    ) -> Result<(), EngineError> {
        let Some((root, dispatch)) = self
            .dispatch_in(Stage::Observing)
            .map(|(root, dispatch)| (root, dispatch.clone()))
        else {
            return Ok(());
        };
        if dispatch.reply_corr != Some(expected_corr)
            || &dispatch.attempt != expected_attempt
            || dispatch.observation.observation_id() != expected_observation
        {
            return Ok(());
        }
        if self.expire_pending_dispatch()? {
            return Ok(());
        }
        if !self.attempt_evidence_usable() {
            self.reject_unusable_dispatch_evidence(
                &dispatch.plan,
                "execution evidence expired before its acknowledgement",
            )?;
            return Ok(());
        }
        if !self.row_selection_dispatch_matches_current(&dispatch) {
            self.abort_dispatch(
                Some(FailureCode::SourceInvalid),
                "the row Source read changed before its observation was acknowledged",
            );
            return Ok(());
        }
        let observation = dispatch.observation.clone();
        if self.now_ms >= observation.valid_until_ms() {
            self.abort_dispatch(
                Some(FailureCode::EvidenceExpired),
                "the acknowledged observation expired",
            );
            return Ok(());
        }
        if !ack.accepted {
            self.abort_dispatch(
                ack.code,
                ack.reason
                    .as_deref()
                    .unwrap_or("the device refused the evidence"),
            );
            return Ok(());
        }
        if !self.scoped_revisit_dispatch_matches_current(&dispatch) {
            self.abort_dispatch(
                Some(FailureCode::SourceInvalid),
                "the scoped revisit binding or Source changed before permit mapping",
            );
            return Ok(());
        }
        let valid_until = observation.valid_until_ms();
        let grant = self
            .device
            .grant_execution(&root, &observation, valid_until);
        let grant_corr = self.send(
            Expect::Grant {
                attempt: expected_attempt.clone(),
                observation_id: expected_observation.into(),
            },
            DeviceCommand::GrantPermit(grant),
        );
        if let Some(dispatch) = self.dispatch_mut() {
            dispatch.stage = Stage::Granting;
            dispatch.reply_corr = Some(grant_corr);
        }
        Ok(())
    }

    /// The device decided on the permit.
    pub(super) fn granted(
        &mut self,
        expected_attempt: &AttemptId,
        expected_observation: &str,
        expected_corr: u64,
        outcome: GrantOutcome,
    ) -> Result<(), EngineError> {
        let Some((root, dispatch)) = self
            .dispatch_in(Stage::Granting)
            .map(|(root, d)| (root, d.clone()))
        else {
            if let GrantOutcome::Granted { permit } = &outcome {
                self.revoke_late_grant(
                    permit,
                    "the bound input was invalidated before its permit arrived",
                );
            }
            return Ok(());
        };
        if dispatch.reply_corr != Some(expected_corr)
            || &dispatch.attempt != expected_attempt
            || dispatch.observation.observation_id() != expected_observation
        {
            if let GrantOutcome::Granted { permit } = &outcome {
                self.revoke_late_grant(
                    permit,
                    "a permit response belonged to a completed or different input dispatch",
                );
            }
            return Ok(());
        }
        if self.expire_pending_dispatch()? {
            if let GrantOutcome::Granted { permit } = &outcome {
                self.revoke_late_grant(
                    permit,
                    "the input attempt expired before its permit arrived",
                );
            }
            return Ok(());
        }
        if let Some(step) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.computer_step.clone())
            && self.validate_computer_attempt_step(&step).is_err()
        {
            if let GrantOutcome::Granted { permit } = &outcome {
                self.revoke_late_grant(permit, "the computer proposal scope ended before input");
            }
            self.abort_dispatch(
                Some(FailureCode::ContextUnresolved),
                "the computer proposal scope ended before input",
            );
            return Ok(());
        }
        if !self.row_selection_dispatch_matches_current(&dispatch) {
            if let GrantOutcome::Granted { permit } = &outcome {
                self.revoke_late_grant(
                    permit,
                    "the row Source read changed before its permit arrived",
                );
            }
            self.abort_dispatch(
                Some(FailureCode::SourceInvalid),
                "the row Source read changed before its permit arrived",
            );
            return Ok(());
        }
        let bound_ref = dispatch
            .observation
            .native()
            .and_then(|observation| observation.binding_proof.as_ref())
            .map(|proof| proof.binding.clone());
        let owned_proof = dispatch
            .observation
            .native()
            .and_then(|observation| observation.owned_document_proof.as_deref())
            .cloned();
        if bound_ref
            .as_ref()
            .is_some_and(|reference| !self.bindings.contains_binding_ref(reference))
        {
            if let GrantOutcome::Granted { permit } = &outcome {
                self.revoke_late_grant(
                    permit,
                    "the binding was invalidated before its permit arrived",
                );
            }
            self.abort_dispatch(
                Some(FailureCode::SourceInvalid),
                "the binding was invalidated before its permit arrived",
            );
            return Ok(());
        }
        let current_evidence = self.attempt_evidence_usable();
        if !current_evidence || self.now_ms >= dispatch.observation.valid_until_ms() {
            if let GrantOutcome::Granted { permit } = &outcome
                && let Some(session_epoch) = self.device.session_epoch()
            {
                self.send(
                    Expect::Revoke,
                    DeviceCommand::Revoke(
                        interaction_contracts::desktop::device::RevokeExecution {
                            session_epoch,
                            permit: permit.clone(),
                            reason: "execution evidence expired before the permit response".into(),
                        },
                    ),
                );
            }
            if current_evidence {
                self.abort_dispatch(
                    Some(FailureCode::EvidenceExpired),
                    "the permit's original observation expired",
                );
            } else {
                self.reject_unusable_dispatch_evidence(
                    &dispatch.plan,
                    "execution evidence expired before the permit response",
                )?;
            }
            return Ok(());
        }
        let permit = match outcome {
            GrantOutcome::Granted { permit } => permit,
            GrantOutcome::Denied { code, reason } => {
                self.abort_dispatch(code, &reason);
                return Ok(());
            }
        };
        if !self.scoped_revisit_dispatch_matches_current(&dispatch) {
            self.revoke_late_grant(
                &permit,
                "the scoped revisit binding or Source changed before activation",
            );
            self.abort_dispatch(
                Some(FailureCode::SourceInvalid),
                "the scoped revisit binding or Source changed before activation",
            );
            return Ok(());
        }
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
        let submit_result = match &dispatch.plan {
            DispatchPlan::Generic(plan) => dispatch
                .observation
                .native()
                .ok_or("native action has non-native observation")
                .and_then(|observation| {
                    self.device.intent(&IntentSpec {
                        attempt: &dispatch.attempt,
                        root: &root,
                        task_revision,
                        root_epoch,
                        permit: &permit,
                        observation,
                        plan: plan.as_ref(),
                        sequence,
                        now_ms: self.now_ms,
                        start_slack_ms: self.cfg.start_slack_ms,
                        duration_ms: self.cfg.action_duration_ms,
                    })
                }),
            DispatchPlan::RowSelection(plan) => dispatch
                .observation
                .native()
                .ok_or("row selection has non-native observation")
                .and_then(|observation| {
                    self.device
                        .row_selection_intent(&crate::execution::RowSelectionIntentSpec {
                            attempt: &dispatch.attempt,
                            root: &root,
                            task_revision,
                            root_epoch,
                            permit: &permit,
                            observation,
                            plan,
                            sequence,
                            now_ms: self.now_ms,
                            start_slack_ms: self.cfg.start_slack_ms,
                            duration_ms: self.cfg.action_duration_ms.min(1_500),
                        })
                }),
            DispatchPlan::OwnedDocument(plan) => self
                .cfg
                .owned_document_registration
                .as_deref()
                .ok_or("private owned-document registration is unavailable")
                .and_then(|registration| {
                    self.device
                        .owned_document_intent(&OwnedDocumentIntentSpec::new(
                            &dispatch.attempt,
                            &root,
                            task_revision,
                            root_epoch,
                            &permit,
                            dispatch
                                .observation
                                .native()
                                .ok_or("owned action has non-native observation")?,
                            plan,
                            sequence,
                            self.now_ms,
                            self.cfg.evidence_max_age_ms,
                            self.cfg.start_slack_ms,
                            self.cfg.action_duration_ms.min(1_500),
                            registration,
                        ))
                }),
            DispatchPlan::Browser(plan) => dispatch
                .observation
                .browser()
                .ok_or("browser action has non-browser observation")
                .and_then(|observation| {
                    self.device.browser_intent(&BrowserIntentSpec {
                        attempt: &dispatch.attempt,
                        root: &root,
                        task_revision,
                        root_epoch,
                        permit: &permit,
                        observation,
                        plan,
                        sequence,
                        now_ms: self.now_ms,
                        start_slack_ms: self.cfg.start_slack_ms,
                        duration_ms: self.cfg.action_duration_ms.min(1_500),
                    })
                }),
        };
        let submit = match submit_result {
            Ok(submit) => submit,
            Err(why) => {
                self.abort_dispatch(Some(FailureCode::SourceInvalid), why);
                return Ok(());
            }
        };
        let intent_id = submit.intent.intent_id().to_owned();
        // Record the last intent before it can leave the engine. Recovery can
        // query exactly the range that may have been sent, including a crash
        // before the acknowledgement, without inventing unused quota slots.
        self.tasks
            .attempt_dispatched(&mut self.storage, &dispatch.attempt, &intent_id)?;
        if let Some(run) = self.run.as_mut() {
            run.permits.push(permit.clone());
            if let Some((_, intent)) = run.last_attempt.as_mut() {
                *intent = Some(intent_id.clone());
            }
            if let Some(attempt) = run.attempt.as_mut()
                && attempt.id == dispatch.attempt
            {
                attempt.last_input_intent_id = Some(intent_id.clone());
            }
        }
        if let Some(binding) = bound_ref {
            self.permit_bindings.insert(permit.id.clone(), binding);
        }
        if let Some(proof) = owned_proof {
            self.permit_owned_proofs.insert(permit.id.clone(), proof);
        }
        if let Some(dispatch) = self.dispatch_mut() {
            dispatch.permit = Some(permit);
            dispatch.intent_id = Some(intent_id.clone());
            dispatch.stage = Stage::Submitting;
        }
        self.send(Expect::Submit { intent_id }, DeviceCommand::Submit(submit));
        Ok(())
    }

    fn revoke_late_grant(
        &mut self,
        permit: &interaction_contracts::desktop::device::PermitRef,
        reason: &str,
    ) {
        if let Some(session_epoch) = self.device.session_epoch() {
            self.send(
                Expect::Revoke,
                DeviceCommand::Revoke(interaction_contracts::desktop::device::RevokeExecution {
                    session_epoch,
                    permit: permit.clone(),
                    reason: reason.into(),
                }),
            );
        }
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

    /// A generic source reset may already have closed the global evidence gate
    /// while a typed owned-document attempt remains independently usable. If
    /// that private proof later expires, the generic reset is idempotent and
    /// cannot retire this dispatch on its own, so end the one-shot attempt.
    fn reject_unusable_dispatch_evidence(
        &mut self,
        plan: &DispatchPlan,
        reason: &str,
    ) -> Result<(), EngineError> {
        if matches!(
            plan,
            DispatchPlan::OwnedDocument(_) | DispatchPlan::Browser(_)
        ) {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::EvidenceExpired,
                reason: reason.into(),
            })
        } else {
            self.invalidate_source_evidence(reason)
        }
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
            .is_some_and(|attempt| {
                attempt.dispatch.as_ref().is_some_and(|dispatch| {
                    dispatch.intent_id.as_deref() == Some(receipt.intent_id.as_str())
                        && (!matches!(
                            &dispatch.plan,
                            DispatchPlan::OwnedDocument(_)
                                | DispatchPlan::RowSelection(_)
                                | DispatchPlan::Browser(_)
                        ) || receipt.attempt_id.as_deref() == Some(attempt.id.as_str()))
                        && (!matches!(
                            &dispatch.plan,
                            DispatchPlan::RowSelection(_) | DispatchPlan::Browser(_)
                        ) || self.device.session_epoch() == Some(receipt.session_epoch))
                        && (attempt.computer_step.is_none()
                            || (receipt.attempt_id.as_deref() == Some(attempt.id.as_str())
                                && self.device.session_epoch() == Some(receipt.session_epoch)))
                })
            });
        if !matches_dispatch {
            return Ok(());
        }
        let delivered_attempt = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .expect("a matching dispatch belongs to an attempt")
            .id
            .clone();
        // Receipt identity proves delivery, independently of mechanical or
        // business success. A terminal skill that produced no input does not.
        self.storage
            .transaction(|tx| ReactionStore::mark_delivered(tx, &delivered_attempt))?;
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
        let moved = receipt
            .submitted
            .iter()
            .any(|submitted| !submitted.cleanup());
        // A Completed acknowledgement is already after the call returned.
        // Its deadline is an authority bound, not a measured return time.
        // This fallback is only an observation floor; exact batch spans still
        // require the explicit native input_end_ms.
        let end_ms = receipt.input_end_ms.unwrap_or_else(|| {
            if receipt.status == ReceiptStatus::Completed {
                receipt.at_ms
            } else {
                receipt.command_end_ms.unwrap_or(receipt.at_ms)
            }
        });
        let valid_opening = self.record_menu_opening(&receipt);
        let outcome = match receipt.status {
            ReceiptStatus::Completed if valid_opening => ActionOutcome::Completed { end_ms },
            ReceiptStatus::Completed => ActionOutcome::Unknown {
                at_ms: receipt.at_ms,
            },
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
        let browser_action = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
            .and_then(|dispatch| match &dispatch.plan {
                DispatchPlan::Browser(plan) => Some(plan.action),
                _ => None,
            });
        let browser_span =
            browser_action.and_then(|action| exact_single_browser_action_span(&receipt, action));
        let browser_dispatch = browser_action.is_some();
        let browser_receipt_valid = browser_dispatch
            && receipt.status == ReceiptStatus::Completed
            && receipt.attempt_id.as_deref() == Some(delivered_attempt.as_str())
            && self.device.session_epoch() == Some(receipt.session_epoch)
            && browser_span.is_some();
        let outcome = if browser_dispatch && receipt.status == ReceiptStatus::Completed {
            match browser_span {
                Some(span) if browser_receipt_valid => {
                    ActionOutcome::Completed { end_ms: span.to_ms }
                }
                _ => ActionOutcome::Unknown {
                    at_ms: receipt.at_ms,
                },
            }
        } else {
            outcome
        };
        let row_selection_span = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
            .and_then(|dispatch| match &dispatch.plan {
                DispatchPlan::RowSelection(plan) => {
                    exact_single_primitive_span(&receipt, &plan.primitive())
                }
                _ => None,
            });
        let row_selection_dispatch = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
            .is_some_and(|dispatch| matches!(&dispatch.plan, DispatchPlan::RowSelection(_)));
        let row_selection_receipt_valid = row_selection_dispatch
            && receipt.status == ReceiptStatus::Completed
            && receipt.attempt_id.as_deref() == Some(delivered_attempt.as_str())
            && self.device.session_epoch() == Some(receipt.session_epoch)
            && row_selection_span.is_some();
        let outcome = if row_selection_dispatch && receipt.status == ReceiptStatus::Completed {
            match row_selection_span {
                Some((end_ms, _)) if row_selection_receipt_valid => {
                    ActionOutcome::Completed { end_ms }
                }
                _ => ActionOutcome::Unknown {
                    at_ms: receipt.at_ms,
                },
            }
        } else {
            outcome
        };
        let (owned_dispatch, owned_primitive, text_expected, owned_save) =
            self.run
                .as_ref()
                .and_then(|run| run.attempt.as_ref())
                .map(|attempt| {
                    let owned_plan = attempt
                        .dispatch
                        .as_ref()
                        .and_then(|dispatch| match &dispatch.plan {
                            DispatchPlan::OwnedDocument(plan) => Some(plan),
                            DispatchPlan::Generic(_)
                            | DispatchPlan::RowSelection(_)
                            | DispatchPlan::Browser(_) => None,
                        });
                    let expected = if owned_plan.is_some() {
                        None
                    } else {
                        attempt.last_input_text.clone()
                    };
                    (
                        attempt.owned_document.is_some() && owned_plan.is_some(),
                        owned_plan.map(OwnedDocumentActionPlan::primitive),
                        expected,
                        owned_plan.is_some_and(|plan| plan.kind() == OwnedDocumentActionKind::Save),
                    )
                })
                .unwrap_or((false, None, None, false));
        let action_input = if owned_dispatch {
            owned_primitive
                .as_ref()
                .and_then(|expected| exact_single_primitive_span(&receipt, expected))
        } else {
            text_expected
                .as_deref()
                .and_then(|expected| text_set_value_span(&receipt, expected))
        };
        let owned_receipt_valid = owned_dispatch
            && receipt.status == ReceiptStatus::Completed
            && receipt.attempt_id.as_deref() == Some(delivered_attempt.as_str())
            && action_input.is_some();
        let outcome =
            if owned_dispatch && receipt.status == ReceiptStatus::Completed && !owned_receipt_valid
            {
                ActionOutcome::Unknown {
                    at_ms: receipt.at_ms,
                }
            } else {
                outcome
            };
        if owned_receipt_valid
            && owned_save
            && let (Some((_, span)), Some(root)) = (
                action_input,
                self.run.as_ref().map(|run| &run.goal.root_task_id),
            )
        {
            let evidence = self
                .run
                .as_ref()
                .and_then(|run| {
                    let attempt = run.attempt.as_ref()?;
                    let owned = attempt.owned_document.as_ref()?;
                    let ExpectedValue::Text { value } = &run.goal.desired else {
                        return None;
                    };
                    let active = self.tasks.active()?;
                    Some(OwnedSaveEvidence {
                        root_task_id: root.clone(),
                        task_revision: active.row.task_revision,
                        root_epoch: active.row.root_epoch,
                        registration_id: owned.admitted_proof.receiver.registration_id.clone(),
                        attempt_id: attempt.id.clone(),
                        intent_id: receipt.intent_id.clone(),
                        span,
                        expected_sha256: format!("{:x}", Sha256::digest(value.as_bytes())),
                        expected_byte_length: value.len().try_into().ok()?,
                        device_session_epoch: receipt.session_epoch,
                        source_identity_boundary: owned
                            .admitted_proof
                            .receiver
                            .provenance
                            .identity_boundary
                            .clone(),
                        source_clock_domain: owned
                            .admitted_proof
                            .receiver
                            .provenance
                            .clock_domain
                            .clone(),
                        release_status: receipt.release_status,
                        completed_ms: self.now_ms,
                    })
                })
                .ok_or_else(|| {
                    EngineError::Store(StoreError::Corrupt(
                        "completed owned Save lacks its frozen proof".into(),
                    ))
                })?;
            OwnedRevisitStore::record_save(&mut self.storage, &evidence)?;
            let detail = serde_json::json!({
                "root_task_id": root.as_str(),
                "attempt_id": delivered_attempt.as_str(),
                "intent_id": receipt.intent_id,
                "input_span": span,
            })
            .to_string();
            self.storage.log(
                receipt.at_ms,
                Some(root.as_str()),
                "owned_document_save_input_completed",
                &detail,
            );
            self.note(
                "the owned Save input completed; independent saved-file and root revisit evidence is still required",
            );
        }
        let computer_receipt_completed = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .filter(|attempt| attempt.computer_step.is_some())
            .and_then(|attempt| attempt.dispatch.as_ref())
            .is_some_and(|dispatch| match &dispatch.plan {
                DispatchPlan::Generic(plan) => {
                    exact_single_primitive_span(&receipt, &plan.primitive).is_some()
                }
                DispatchPlan::RowSelection(plan) => {
                    exact_single_primitive_span(&receipt, &plan.primitive()).is_some()
                }
                DispatchPlan::OwnedDocument(_) => false,
                DispatchPlan::Browser(_) => false,
            });
        let keyboard_context = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
            .and_then(|dispatch| match &dispatch.plan {
                DispatchPlan::Generic(plan)
                    if plan.path == crate::reaction::skills::ExecutionPath::Keyboard =>
                {
                    dispatch
                        .observation
                        .native()
                        .and_then(|observation| observation.binding_proof.as_ref())
                }
                _ => None,
            })
            .and_then(|proof| proof.focus_scope.as_ref())
            .map(|scope| scope.context.clone());
        if let Some(context) = keyboard_context
            && let Some(run) = self.run.as_mut()
        {
            run.keyboard_result_context = if matches!(outcome, ActionOutcome::Completed { .. }) {
                Some(context)
            } else {
                None
            };
        }
        if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) {
            attempt.input_reached |= moved || matches!(outcome, ActionOutcome::Unknown { .. });
            attempt.mechanical_unknown |= matches!(outcome, ActionOutcome::Unknown { .. });
            attempt.computer_input_completed |=
                computer_receipt_completed && matches!(outcome, ActionOutcome::Completed { .. });
            if let Some((end_ms, span)) = action_input {
                attempt.text_input_end_ms = Some(end_ms);
                attempt.text_input_span = Some(span);
            }
            if let Some(span) = browser_span {
                attempt.browser_input_span = Some(span);
            }
            attempt.dispatch = None;
            attempt.skill.acted(outcome);
        }
        if browser_receipt_valid && let Some(input_span) = browser_span {
            self.begin_browser_current_ui(crate::verification::BrowserCurrentUiFence::Submitted {
                input_span,
            })?;
            return Ok(());
        }
        if owned_dispatch {
            if owned_receipt_valid {
                self.finish_attempt(SkillResult::Succeeded)?;
            } else {
                let code = match outcome {
                    ActionOutcome::Rejected(Some(code)) => code,
                    ActionOutcome::Cancelled => FailureCode::Cancelled,
                    _ => FailureCode::EffectUnknown,
                };
                self.finish_attempt(SkillResult::Failed {
                    code,
                    reason:
                        "the owned action receipt did not prove one completed matching primitive"
                            .into(),
                })?;
            }
        }
        Ok(())
    }

    /// Tell Verification when input actually reached the application, so
    /// evidence from before it can never confirm its effect.
    fn record_receipt_input(&mut self, receipt: &Receipt) {
        let background_input = self.run.as_ref().and_then(|run| {
            if run.goal.execution_domain
                != interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
            {
                return None;
            }
            let attempt = run.attempt.as_ref()?;
            if receipt.attempt_id.as_deref() != Some(attempt.id.as_str())
                || self.device.session_epoch() != Some(receipt.session_epoch)
            {
                return None;
            }
            let dispatch = attempt.dispatch.as_ref()?;
            let observation = dispatch.observation.native()?;
            if observation.purpose
                != interaction_contracts::desktop::device::ObservationPurpose::BackgroundAxAction
                || observation.execution_domain
                    != interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
            {
                return None;
            }
            let DispatchPlan::Generic(plan) = &dispatch.plan else {
                return None;
            };
            if plan.path != ExecutionPath::Ax
                || plan.binding.rule != "background_ax"
                || plan.primitive
                    != (interaction_contracts::desktop::primitive::Primitive::AxAction {
                        action: interaction_contracts::desktop::primitive::AxActionKind::Press,
                    })
            {
                return None;
            }
            let binding = attempt.last_input_binding.as_ref()?;
            if binding.binding_ref != plan.binding.binding_ref {
                return None;
            }
            let BindingProofScope::BackgroundAx {
                target_context_hash,
            } = &binding.continuity.scope
            else {
                return None;
            };
            let (_, span) = exact_single_primitive_span(receipt, &plan.primitive)?;
            Some((
                binding.clone(),
                observation.source_sequence,
                target_context_hash.clone(),
                span.to_ms,
            ))
        });
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
        if matches!(
            receipt.status,
            ReceiptStatus::Unknown | ReceiptStatus::Partial
        ) || receipt.status == ReceiptStatus::Cancelled
            && receipt.submitted.iter().any(|s| !s.cleanup())
        {
            run.verification.record_unknown_input(receipt.at_ms);
        }
        let mut times = receipt
            .submitted
            .iter()
            .filter(|submitted| !submitted.cleanup())
            .map(|submitted| submitted.at_ms());
        let Some(first) = times.next() else {
            return;
        };
        let last = times.next_back().unwrap_or(first);
        let end = receipt
            .input_end_ms
            .unwrap_or_else(|| {
                if receipt.status == ReceiptStatus::Completed {
                    receipt.at_ms
                } else {
                    receipt.command_end_ms.unwrap_or(last)
                }
            })
            .max(last);
        run.verification.record_input(first, end);
        if let Some((binding, source_sequence, target_context_hash, end_ms)) = background_input {
            run.verification.record_background_action_input(
                binding,
                source_sequence,
                target_context_hash,
                end_ms,
            );
            run.effect_verification_only = true;
        }
    }

    fn owned_setter_has_save_followup(&self, attempt: &AttemptRun) -> bool {
        if !self.cfg.allow_owned_document_save
            || attempt.skill.skill() != Skill::EnterText
            || !attempt
                .owned_document
                .as_ref()
                .is_some_and(|owned| owned.action == OwnedDocumentActionKind::SetValue)
        {
            return false;
        }
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let Some(plan) = self
            .tasks
            .active()
            .and_then(|active| active.row.plan.as_ref())
        else {
            return false;
        };
        let mut done = run.done.clone();
        done.insert(attempt.subgoal.clone());
        plan.subgoals
            .iter()
            .find(|subgoal| {
                !done.contains(&subgoal.id)
                    && plan
                        .dependencies
                        .iter()
                        .filter(|dependency| dependency.after == subgoal.id)
                        .all(|dependency| done.contains(&dependency.before))
            })
            .is_some_and(|subgoal| {
                subgoal.skill == Skill::SaveDocument.name()
                    && subgoal.scroll.is_none()
                    && subgoal.select.is_none()
                    && subgoal.target.as_ref() == Some(&run.goal.target.semantic_target)
            })
    }

    // ---- The end of an attempt ------------------------------------------------------------------------------

    pub(super) fn finish_attempt(&mut self, mut result: SkillResult) -> Result<(), EngineError> {
        let Some(mut attempt) = self.run.as_mut().and_then(|run| run.attempt.take()) else {
            return Ok(());
        };
        self.device.forget_attempt_waits();
        let now = self.now_ms;
        let name = attempt.skill.skill().name();
        let input = attempt.input_reached;
        let radio_choice_attempt = self.run.as_ref().is_some_and(|run| {
            attempt.skill.skill() == Skill::SelectByLabel
                && scoped_radio_choice_values(&run.goal, &run.pack).is_some()
        });
        let is_computer = attempt.computer_step.is_some();
        let manual_commit_action_attempt = self.run.as_ref().is_some_and(|run| {
            attempt.skill.skill() == Skill::Activate
                && attempt.last_input_binding.as_ref().is_some_and(|binding| {
                    run.goal
                        .scoped_commit
                        .as_ref()
                        .filter(|commit| {
                            commit.receipt.kind
                                == interaction_contracts::desktop::task::ScopedCommitKind::Manual
                        })
                        .and_then(|commit| commit.action.as_ref())
                        .is_some_and(|action| action.semantic_target == binding.semantic_target)
                })
        });
        if manual_commit_action_attempt
            && (input || attempt.mechanical_unknown)
            && let Some(run) = self.run.as_mut()
        {
            // A Save dispatch is one-shot even if its device result is
            // uncertain. Only Source receipt verification may continue.
            run.effect_verification_only = true;
        }
        if attempt
            .owned_document
            .as_ref()
            .is_some_and(|owned| owned.input_issued)
            && let Some(run) = self.run.as_mut()
        {
            // Once an owned one-shot action begins, later input is blocked
            // until its own evidence path proves the safe next step.
            run.effect_verification_only = true;
        }
        if attempt.skill.keyboard_context().is_some()
            && matches!(result, SkillResult::AlreadyDone | SkillResult::Succeeded)
        {
            if !self.keyboard_value_verified(&attempt.skill, attempt.mechanical_unknown, input) {
                result = SkillResult::Failed {
                    code: if attempt.mechanical_unknown {
                        FailureCode::EffectUnknown
                    } else {
                        FailureCode::ContextUnresolved
                    },
                    reason: "keyboard result lacks independent fresh normal-window value evidence"
                        .into(),
                };
            } else {
                let env = self.state.latest().unwrap();
                let run = self.run.as_ref().unwrap();
                crate::verification::conclude_current_ui(
                    &mut self.storage,
                    interaction_contracts::desktop::verification::EffectResult {
                        result_id: interaction_contracts::desktop::ids::RequestId::new(format!(
                            "keyboard-effect-{}",
                            attempt.id
                        ))
                        .unwrap(),
                        revision: 1,
                        root_task_id: run.goal.root_task_id.clone(),
                        attempt_id: Some(attempt.id.clone()),
                        intent_id: if input {
                            run.last_attempt
                                .as_ref()
                                .and_then(|(_, intent)| intent.clone())
                        } else {
                            None
                        },
                        predicate: "keyboard.value@1".into(),
                        target: Some(run.goal.target.semantic_target.clone()),
                        status:
                            interaction_contracts::desktop::verification::EffectStatus::Satisfied,
                        evidence_group: vec![env.snapshot.snapshot_id.clone()],
                        input_span: if input {
                            run.verification.input_span()
                        } else {
                            None
                        },
                        observed_ms: env.snapshot.observed_ms,
                        confirmation_scope:
                            interaction_contracts::desktop::task::ConfirmationScope::CurrentUi,
                        causality: if input {
                            interaction_contracts::desktop::verification::Causality::Unknown
                        } else {
                            interaction_contracts::desktop::verification::Causality::NotApplicable
                        },
                    },
                )?;
                // This descriptor only permits future independent reads. The
                // CurrentUi audit above is separate from Root UiRevisit.
                self.run.as_mut().unwrap().keyboard_result_context =
                    attempt.skill.keyboard_context().cloned();
            }
        }
        if attempt
            .owned_document
            .as_ref()
            .is_some_and(|owned| owned.action == OwnedDocumentActionKind::SetValue)
            && input
        {
            let facts = self.owned_text_current_ui_facts(&attempt);
            let evidence = facts
                .as_ref()
                .and_then(|facts| self.owned_text_current_ui_proof(facts, now));
            match (facts, evidence) {
                (
                    Some(facts),
                    Some((proof, interaction_contracts::desktop::verification::TriState::True)),
                ) => {
                    self.record_owned_text_current_ui(&facts, &proof)?;
                    let can_continue_to_save = self.owned_setter_has_save_followup(&attempt);
                    if let Some(run) = self.run.as_mut() {
                        // CurrentUi is an attempt result only. Root UiRevisit or
                        // Commit predicates still need their own evidence.
                        run.effect_verification_only = !can_continue_to_save;
                    }
                    result = SkillResult::Succeeded;
                }
                (
                    Some(_),
                    Some((_, interaction_contracts::desktop::verification::TriState::False)),
                ) => {
                    result = SkillResult::Failed {
                        code: FailureCode::EffectUnknown,
                        reason: "a fresh owned-document digest read did not match the frozen text"
                            .into(),
                    };
                }
                (facts, _) => {
                    let can_wait = facts.is_some()
                        && !attempt.mechanical_unknown
                        && now < attempt.deadline_ms
                        && attempt.observations_used < attempt.quota.observations;
                    if can_wait {
                        attempt.text_current_ui_pending = true;
                        attempt.text_current_ui_last_snapshot = facts
                            .as_ref()
                            .and_then(|facts| {
                                self.owned_document_proof_for(&facts.original.semantic_target)
                            })
                            .map(|proof| proof.receiver.provenance.snapshot_id.clone())
                            .or_else(|| {
                                self.state
                                    .latest()
                                    .map(|env| env.snapshot.snapshot_id.clone())
                            });
                        attempt.text_current_ui_last_rejection =
                            Some(TextCurrentUiRejection::Verification(
                                crate::verification::TextValueRejection::ValueUnavailable,
                            ));
                        if let Some(run) = self.run.as_mut() {
                            run.effect_verification_only = true;
                            run.attempt = Some(attempt);
                        }
                        self.note(
                            "the completed owned SetValue awaits a fresh same-receiver digest read",
                        );
                        let _ = self.pending_text_current_ui()?;
                        return Ok(());
                    }
                    result = SkillResult::Failed {
                        code: FailureCode::EffectUnknown,
                        reason: "the owned SetValue lacks a current post-input digest read".into(),
                    };
                }
            }
        } else if attempt.skill.skill() == Skill::EnterText && input {
            let facts = self.text_current_ui_facts(&attempt);
            let proof = facts
                .as_ref()
                .map(|facts| self.prove_text_current_ui(facts, now))
                .unwrap_or(Err(TextCurrentUiRejection::InputFactsIncomplete));
            match (facts, proof) {
                (Some(facts), Ok(evidence)) => {
                    self.record_text_current_ui(&facts, evidence)?;
                    result = SkillResult::Succeeded;
                }
                (facts, proof) => {
                    let rejection = proof
                        .err()
                        .unwrap_or(TextCurrentUiRejection::InputFactsIncomplete);
                    let can_wait_for_original_proof = facts.is_some()
                        && !attempt.mechanical_unknown
                        && now < attempt.deadline_ms
                        && attempt.observations_used < attempt.quota.observations;
                    if can_wait_for_original_proof {
                        attempt.text_current_ui_pending = true;
                        attempt.text_current_ui_last_snapshot = self
                            .state
                            .latest()
                            .map(|env| env.snapshot.snapshot_id.clone());
                        attempt.text_current_ui_last_rejection = Some(rejection);
                        if let Some(run) = self.run.as_mut() {
                            run.effect_verification_only = true;
                            run.attempt = Some(attempt);
                        }
                        self.note("the completed text input awaits read-only Source proof on its original binding");
                        let _ = self.pending_text_current_ui()?;
                        return Ok(());
                    }
                    result = SkillResult::Failed {
                        code: FailureCode::EffectUnknown,
                        reason: "text input lacks independent fresh same-instance value evidence"
                            .into(),
                    };
                }
            }
        }
        if radio_choice_attempt
            && matches!(result, SkillResult::AlreadyDone | SkillResult::Succeeded)
        {
            let radio_evidence = (|| {
                let run = self.run.as_ref()?;
                let interaction_contracts::desktop::task::ExpectedValue::Text { value: desired } =
                    &run.goal.desired
                else {
                    return None;
                };
                if attempt.mechanical_unknown
                    || !matches!(
                        attempt.skill.menu_desired(),
                        Some(interaction_contracts::desktop::task::ExpectedValue::Text {
                            value
                        }) if value == desired
                    )
                    || !self.state.usable(now, self.cfg.evidence_max_age_ms)
                {
                    return None;
                }
                let env = self.state.latest()?;
                if env.snapshot.observed_ms > now {
                    return None;
                }
                if input {
                    let floor = attempt.skill.input_floor()?;
                    let input_binding = attempt.last_input_binding.as_ref()?;
                    if env.snapshot.observed_ms <= floor
                        || env.snapshot.source_sequence <= input_binding.continuity.source_sequence
                    {
                        return None;
                    }
                }
                let radio = scoped_radio_choice_control(&run.goal, &run.pack, env)?;
                if radio.selected_label != desired {
                    return None;
                }
                let target = run.goal.target.semantic_target.clone();
                let binding = self.bindings.bind_instance(
                    env,
                    target.clone(),
                    &radio.desired.instance_ref,
                )?;
                let Some(interaction_contracts::desktop::source::UiValue::Text(selected)) =
                    scoped_radio_value_from_binding(&run.goal, &run.pack, env, &binding)
                else {
                    return None;
                };
                (selected.as_str() == desired.as_str()).then(|| {
                    (
                        env.snapshot.snapshot_id.clone(),
                        env.snapshot.observed_ms,
                        target,
                    )
                })
            })();
            if let Some((snapshot_id, observed_ms, target)) = radio_evidence {
                let run = self
                    .run
                    .as_ref()
                    .ok_or_else(|| StoreError::Corrupt("radio result without root".into()))?;
                crate::verification::conclude_current_ui(
                    &mut self.storage,
                    interaction_contracts::desktop::verification::EffectResult {
                        result_id: RequestId::new(format!("radio-effect-{}", attempt.id))
                            .expect("attempt id produces a valid result id"),
                        revision: 1,
                        root_task_id: run.goal.root_task_id.clone(),
                        attempt_id: Some(attempt.id.clone()),
                        intent_id: if input {
                            run.last_attempt
                                .as_ref()
                                .and_then(|(_, intent)| intent.clone())
                        } else {
                            None
                        },
                        predicate: "radio_group.selected_value@1".into(),
                        target: Some(target),
                        status:
                            interaction_contracts::desktop::verification::EffectStatus::Satisfied,
                        evidence_group: vec![snapshot_id],
                        input_span: if input {
                            run.verification.input_span()
                        } else {
                            None
                        },
                        observed_ms,
                        confirmation_scope:
                            interaction_contracts::desktop::task::ConfirmationScope::CurrentUi,
                        causality: if input {
                            interaction_contracts::desktop::verification::Causality::Unknown
                        } else {
                            interaction_contracts::desktop::verification::Causality::NotApplicable
                        },
                    },
                )?;
            } else {
                result = SkillResult::Failed {
                    code: if attempt.mechanical_unknown {
                        FailureCode::EffectUnknown
                    } else {
                        FailureCode::ContextUnresolved
                    },
                    reason: "radio choice lacks fresh exact selected-child Text evidence".into(),
                };
            }
        }
        if attempt.skill.skill() == Skill::SelectByLabel
            && !radio_choice_attempt
            && matches!(result, SkillResult::AlreadyDone | SkillResult::Succeeded)
        {
            let run = self
                .run
                .as_ref()
                .ok_or_else(|| StoreError::Corrupt("menu attempt without root".into()))?;
            let target = attempt
                .skill
                .menu_target()
                .ok_or_else(|| StoreError::Corrupt("menu attempt without opener target".into()))?
                .clone();
            let desired = attempt
                .skill
                .menu_desired()
                .ok_or_else(|| StoreError::Corrupt("menu attempt without literal choice".into()))?
                .clone();
            let floor = attempt.skill.input_floor().unwrap_or(0);
            if attempt.mechanical_unknown
                || input
                    && !self.menu_readonly_result_scope_valid(
                        &attempt.skill,
                        attempt.mechanical_unknown,
                    )
                || !crate::verification::menu_selected_value(
                    &self.state,
                    &self.bindings,
                    &target,
                    &desired,
                    now,
                    self.cfg.evidence_max_age_ms,
                    floor,
                )
                .is_true()
            {
                result = SkillResult::Failed {
                    code: if attempt.mechanical_unknown {
                        FailureCode::EffectUnknown
                    } else {
                        FailureCode::ContextUnresolved
                    },
                    reason: "menu selection lacks independent fresh closed-menu value evidence"
                        .into(),
                };
            } else {
                let env = self.state.latest().unwrap();
                crate::verification::conclude_current_ui(
                    &mut self.storage,
                    interaction_contracts::desktop::verification::EffectResult {
                        result_id: interaction_contracts::desktop::ids::RequestId::new(format!(
                            "menu-effect-{}",
                            attempt.id
                        ))
                        .unwrap(),
                        revision: 1,
                        root_task_id: run.goal.root_task_id.clone(),
                        attempt_id: Some(attempt.id.clone()),
                        intent_id: run
                            .last_attempt
                            .as_ref()
                            .and_then(|(_, intent)| intent.clone()),
                        predicate: "menu.selected_value@1".into(),
                        target: Some(target),
                        status:
                            interaction_contracts::desktop::verification::EffectStatus::Satisfied,
                        evidence_group: vec![env.snapshot.snapshot_id.clone()],
                        input_span: if input {
                            run.verification.input_span()
                        } else {
                            None
                        },
                        observed_ms: env.snapshot.observed_ms,
                        confirmation_scope:
                            interaction_contracts::desktop::task::ConfirmationScope::CurrentUi,
                        causality: if input {
                            interaction_contracts::desktop::verification::Causality::Unknown
                        } else {
                            interaction_contracts::desktop::verification::Causality::NotApplicable
                        },
                    },
                )?;
            }
        }
        if attempt.skill.skill() == Skill::ScrollTo
            && matches!(result, SkillResult::AlreadyDone | SkillResult::Succeeded)
        {
            let (container, destination) = attempt.skill.scroll_targets().ok_or_else(|| {
                StoreError::Corrupt("scroll attempt without semantic targets".into())
            })?;
            let floor = attempt.skill.input_floor().unwrap_or(0);
            if !crate::verification::scroll_target_bound(
                &self.state,
                &self.bindings,
                container,
                destination,
                now,
                self.cfg.evidence_max_age_ms,
                floor,
            )
            .is_true()
            {
                result = SkillResult::Failed {
                    code: FailureCode::ContextUnresolved,
                    reason: "scroll destination lacks fresh container-scoped binding evidence"
                        .into(),
                };
            } else {
                let env = self.state.latest().unwrap();
                let run = self.run.as_ref().unwrap();
                crate::verification::conclude_current_ui(
                    &mut self.storage,
                    interaction_contracts::desktop::verification::EffectResult {
                        result_id: interaction_contracts::desktop::ids::RequestId::new(format!(
                            "scroll-effect-{}",
                            attempt.id
                        ))
                        .unwrap(),
                        revision: 1,
                        root_task_id: run.goal.root_task_id.clone(),
                        attempt_id: Some(attempt.id.clone()),
                        intent_id: run
                            .last_attempt
                            .as_ref()
                            .and_then(|(_, intent)| intent.clone()),
                        predicate: "scroll.target_visible_bound@1".into(),
                        target: Some(destination.clone()),
                        status:
                            interaction_contracts::desktop::verification::EffectStatus::Satisfied,
                        evidence_group: vec![env.snapshot.snapshot_id.clone()],
                        input_span: if input {
                            run.verification.input_span()
                        } else {
                            None
                        },
                        observed_ms: env.snapshot.observed_ms,
                        confirmation_scope:
                            interaction_contracts::desktop::task::ConfirmationScope::CurrentUi,
                        causality: if input {
                            interaction_contracts::desktop::verification::Causality::Unknown
                        } else {
                            interaction_contracts::desktop::verification::Causality::NotApplicable
                        },
                    },
                )?;
            }
        }
        if attempt.skill.skill() == Skill::ActivateWindow
            && matches!(result, SkillResult::AlreadyDone | SkillResult::Succeeded)
        {
            let scope = self
                .cfg
                .native_window_scope
                .as_ref()
                .ok_or_else(|| StoreError::Corrupt("window attempt without scope".into()))?;
            let floor = attempt.skill.input_floor().unwrap_or(0);
            let scoped_binding = attempt.last_input_binding.as_ref().and_then(|binding| {
                match &binding.continuity.scope {
                    interaction_contracts::desktop::binding::BindingProofScope::WindowRoot {
                        table_scope,
                    } => Some((binding, table_scope)),
                    _ => None,
                }
            });
            let env = self.state.latest().cloned();
            let verified = if let Some((binding, table_scope)) = scoped_binding {
                let source_floor = binding.continuity.source_sequence;
                !attempt.mechanical_unknown
                    && env.as_ref().is_some_and(|env| {
                        env.snapshot.observed_ms > floor
                            && env.snapshot.source_sequence > source_floor
                            && self
                                .state
                                .row_window_root(
                                    table_scope,
                                    &scope.app,
                                    scope.pid,
                                    &scope.window,
                                    now,
                                    self.cfg.evidence_max_age_ms.min(2_000),
                                )
                                .is_some_and(|root| root.instance_ref == binding.instance)
                    })
            } else {
                crate::verification::window_active(
                    &self.state,
                    &scope.app,
                    scope.pid,
                    &scope.window,
                    now,
                    floor,
                )
                .is_true()
            };
            if !verified {
                if scoped_binding.is_some() {
                    result = SkillResult::Failed {
                        code: if attempt.mechanical_unknown {
                            FailureCode::EffectUnknown
                        } else {
                            FailureCode::ContextUnresolved
                        },
                        reason: if attempt.mechanical_unknown {
                            "the device outcome is unknown; a new scoped Source read cannot make it a success".into()
                        } else {
                            "WindowRoot activation lacks a fresh same-read post-input focus witness"
                                .into()
                        },
                    };
                } else {
                    return Err(StoreError::Corrupt(
                        "window activation succeeded without fresh focus evidence".into(),
                    )
                    .into());
                }
            } else {
                let env = env.expect("verified evidence has a current snapshot");
                let run = self.run.as_ref().unwrap();
                crate::verification::conclude_current_ui(
                    &mut self.storage,
                    interaction_contracts::desktop::verification::EffectResult {
                        result_id: interaction_contracts::desktop::ids::RequestId::new(format!(
                            "window-effect-{}",
                            attempt.id
                        ))
                        .unwrap(),
                        revision: 1,
                        root_task_id: run.goal.root_task_id.clone(),
                        attempt_id: Some(attempt.id.clone()),
                        intent_id: run
                            .last_attempt
                            .as_ref()
                            .and_then(|(_, intent)| intent.clone()),
                        predicate: "window.active@1".into(),
                        target: Some(
                            interaction_contracts::desktop::ids::SemanticTargetRef::new(
                                "native-window",
                            )
                            .unwrap(),
                        ),
                        status:
                            interaction_contracts::desktop::verification::EffectStatus::Satisfied,
                        evidence_group: vec![env.snapshot.snapshot_id.clone()],
                        input_span: if input {
                            run.verification.input_span()
                        } else {
                            None
                        },
                        observed_ms: env.snapshot.observed_ms,
                        confirmation_scope:
                            interaction_contracts::desktop::task::ConfirmationScope::CurrentUi,
                        causality: if input {
                            interaction_contracts::desktop::verification::Causality::Unknown
                        } else {
                            interaction_contracts::desktop::verification::Causality::NotApplicable
                        },
                    },
                )?;
                self.run.as_mut().unwrap().window_activated = true;
            }
        }
        let validation_gated_ensure = self.run.as_ref().is_some_and(|run| {
            attempt.skill.skill() == Skill::EnsureValue
                && attempt.goal_target
                && run
                    .goal
                    .allowed_scope
                    .as_ref()
                    .filter(|scope| scope.targets.len() == 1)
                    .and_then(|scope| scope.targets.first())
                    .and_then(|target| run.pack.scoped_validation_feedback(target))
                    .is_some()
        });
        if validation_gated_ensure
            && matches!(result, SkillResult::AlreadyDone | SkillResult::Succeeded)
        {
            let proof = (|| {
                let run = self.run.as_ref()?;
                let env = self.state.latest()?;
                if !self.state.usable(now, self.cfg.evidence_max_age_ms)
                    || env.snapshot.observed_ms > now
                {
                    return None;
                }
                let binding = attempt
                    .last_input_binding
                    .as_ref()
                    .or_else(|| self.bindings.current(&run.goal.target.semantic_target))?;
                if input
                    && (attempt.mechanical_unknown
                        || attempt
                            .skill
                            .input_floor()
                            .is_none_or(|floor| env.snapshot.observed_ms <= floor)
                        || env.snapshot.source_sequence <= binding.continuity.source_sequence)
                {
                    return None;
                }
                crate::verification::scoped_validation_feedback_valid(
                    &run.goal, &run.pack, env, binding,
                )
                .then_some(())
            })()
            .is_some();
            if !proof {
                result = SkillResult::Failed {
                    code: FailureCode::EffectUnknown,
                    reason: "the exact Region code value lacks fresh positive validation feedback"
                        .into(),
                };
            }
        }
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
                    if !is_computer {
                        run.done.insert(attempt.subgoal.clone());
                    }
                    run.failures = 0;
                    run.stalls = 0;
                    run.recent.push(format!(
                        "{name}: {}",
                        if already { "already satisfied" } else { "done" }
                    ));
                    // The change itself is done; from here leaving the page counts.
                    if attempt.goal_target
                        && attempt.owned_document.is_none()
                        && matches!(
                            attempt.skill.skill(),
                            Skill::EnsureValue
                                | Skill::EnterText
                                | Skill::SelectByLabel
                                | Skill::SelectRow
                        )
                    {
                        run.verification.visits.arm(now);
                    }
                }
                if let Some(step) = attempt.computer_step.take() {
                    // Mechanical action progress never proves the RootGoal.
                    self.complete_computer_attempt(
                        step,
                        !already && attempt.computer_input_completed && !attempt.mechanical_unknown,
                    );
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
            SkillResult::NotFound {
                observation_complete,
                search_complete,
                reason,
            } => {
                self.tasks.attempt_ended(
                    &mut self.storage,
                    &attempt.id,
                    AttemptState::Settled,
                    !input,
                )?;
                if let Some(step) = attempt.computer_step.take() {
                    self.complete_computer_attempt(step, false);
                }
                self.note(format!("{name}: destination not found; observation_complete={observation_complete}, search_complete={search_complete}; {reason}"));
                self.fail_root(if search_complete {
                    "the scroll search ended with complete absence evidence"
                } else {
                    "the bounded scroll search reached its boundary; remaining content coverage is unproved"
                })?;
                Ok(())
            }
            SkillResult::Failed { code, reason } => {
                let state = match code {
                    FailureCode::EffectUnknown if attempt.mechanical_unknown => {
                        AttemptState::Unknown
                    }
                    FailureCode::Cancelled => AttemptState::Cancelled,
                    _ => AttemptState::Settled,
                };
                self.tasks
                    .attempt_ended(&mut self.storage, &attempt.id, state, !input)?;
                if let Some(run) = self.run.as_mut() {
                    run.recent
                        .push(format!("{name}: {} ({})", code.design_name(), reason));
                    if code == FailureCode::EffectUnknown && attempt.mechanical_unknown {
                        run.reconcile.extend((1..=attempt.sequence).map(|sequence| {
                            (
                                attempt.id.clone(),
                                format!("intent-{}-{sequence}", attempt.id),
                            )
                        }));
                        run.resume_authorized = false;
                    }
                }
                if let Some(step) = attempt.computer_step.take() {
                    self.complete_computer_attempt(step, false);
                }
                self.note(format!("{name} failed: {} — {reason}", code.design_name()));
                if attempt.skill.skill() == Skill::SelectRow
                    && (input || attempt.mechanical_unknown)
                {
                    if let Some(run) = self.run.as_mut() {
                        run.effect_verification_only = true;
                        run.resume_authorized = false;
                    }
                    return self.pause_root(
                        "row selection did not reach a verified selected row; the action will not be repeated",
                    );
                }
                if attempt.skill.skill() == Skill::SelectByLabel
                    || attempt.skill.keyboard_context().is_some()
                {
                    // A new attempt must never replay this transaction's
                    // opening or choice after its bounded evidence failed.
                    return if code == FailureCode::EffectUnknown && attempt.mechanical_unknown {
                        self.route_failure(code)
                    } else {
                        self.fail_root(&format!(
                            "the bounded {} transaction ended: {reason}",
                            if attempt.skill.keyboard_context().is_some() {
                                "keyboard"
                            } else if radio_choice_attempt {
                                "radio choice"
                            } else {
                                "menu"
                            }
                        ))
                    };
                }
                if code == FailureCode::EffectUnknown && !attempt.mechanical_unknown {
                    if let Some(run) = self.run.as_mut() {
                        run.effect_verification_only = true;
                        run.stalls = 0;
                    }
                    self.note("the device completed the input; verifying its business effect before any further input");
                    self.request_refresh(
                        now.saturating_add(1),
                        "verifying the effect of completed input",
                    )?;
                    return Ok(());
                }
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
            FailureCode::EffectUnknown => {
                self.pause_root("input may have reached the application; its outcome is unknown")
            }
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
        self.device.forget_attempt_waits();
        if attempt
            .owned_document
            .as_ref()
            .is_some_and(|owned| owned.input_issued)
            && let Some(run) = self.run.as_mut()
        {
            // An owned setter authorization is one-shot even when a stop or
            // stale observation prevents it from reaching Submit.
            run.effect_verification_only = true;
        }
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

    /// An unanswered evidence/permit request cannot outlive its attempt.
    /// This runs before incoming replies, so an expired reply cannot submit.
    pub(super) fn expire_pending_dispatch(&mut self) -> Result<bool, EngineError> {
        let expired = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| {
                self.now_ms >= attempt.deadline_ms
                    && attempt.dispatch.as_ref().is_some_and(|dispatch| {
                        matches!(dispatch.stage, Stage::Observing | Stage::Granting)
                    })
            });
        if expired {
            self.finish_attempt(SkillResult::Failed {
                code: FailureCode::EvidenceExpired,
                reason: "the attempt ran out of time before input was submitted".into(),
            })?;
        }
        Ok(expired)
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

#[cfg(test)]
mod text_receipt_tests {
    use super::*;
    use interaction_contracts::desktop::device::{ReleaseStatus, SubmittedAction};
    use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};

    fn receipt(status: ReceiptStatus, release_status: ReleaseStatus) -> Receipt {
        Receipt {
            intent_id: "intent".into(),
            attempt_id: Some("attempt".into()),
            session_epoch: 1,
            status,
            reason_code: None,
            reason: None,
            submitted: vec![SubmittedAction::Native {
                at_ms: 20,
                primitive: Primitive::AxAction {
                    action: AxActionKind::SetValue {
                        value: "wanted".into(),
                    },
                },
                cleanup: false,
            }],
            release_status,
            command_end_ms: Some(31),
            input_end_ms: Some(30),
            at_ms: 31,
        }
    }

    #[test]
    fn text_span_requires_completed_matching_ax_setter_and_actual_input_end() {
        assert_eq!(
            text_set_value_span(
                &receipt(ReceiptStatus::Completed, ReleaseStatus::NotNeeded),
                "wanted"
            ),
            Some((
                30,
                interaction_contracts::desktop::verification::TimeSpan {
                    from_ms: 20,
                    to_ms: 30,
                }
            ))
        );
        for status in [
            ReceiptStatus::Unknown,
            ReceiptStatus::Partial,
            ReceiptStatus::Cancelled,
        ] {
            assert!(
                text_set_value_span(&receipt(status, ReleaseStatus::NotNeeded), "wanted").is_none()
            );
        }
        for release in [ReleaseStatus::Unknown, ReleaseStatus::Failed] {
            assert!(
                text_set_value_span(&receipt(ReceiptStatus::Completed, release), "wanted")
                    .is_none()
            );
        }
    }

    #[test]
    fn text_span_rejects_missing_end_extra_input_or_wrong_setter_value() {
        let mut missing_end = receipt(ReceiptStatus::Completed, ReleaseStatus::NotNeeded);
        missing_end.input_end_ms = None;
        assert!(text_set_value_span(&missing_end, "wanted").is_none());

        let mut multiple = receipt(ReceiptStatus::Completed, ReleaseStatus::NotNeeded);
        multiple.submitted.push(multiple.submitted[0].clone());
        assert!(text_set_value_span(&multiple, "wanted").is_none());

        assert!(
            text_set_value_span(
                &receipt(ReceiptStatus::Completed, ReleaseStatus::NotNeeded),
                "different"
            )
            .is_none()
        );
    }
}

#[cfg(test)]
mod background_candidate_admission_tests {
    use super::*;
    use crate::environment::pack::AppPack;
    use crate::execution::DeviceCommand;
    use crate::infrastructure::storage::Storage;
    use crate::tasks::TaskConfig;
    use interaction_contracts::desktop::decision::{Candidate, Precondition};
    use interaction_contracts::desktop::device::{
        BackendKind, CapabilityManifest, CapabilityState, ExecutionDomainCapability,
    };
    use interaction_contracts::desktop::domain::{DomainAction, ExecutionDomain};
    use interaction_contracts::desktop::effect::EffectClass;
    use interaction_contracts::desktop::ids::{CandidateId, SemanticTargetRef};
    use interaction_contracts::desktop::planning::{GoalDraft, PlanBundle, Subgoal};
    use interaction_contracts::desktop::primitive::PrimitivePolicy;
    use interaction_contracts::desktop::source::{SourceCommand, StructureSnapshot, TargetContext};
    use interaction_contracts::desktop::task::{ConfirmationScope, ExpectedValue, TargetSpec};
    use std::collections::BTreeMap;

    const SOURCE_FIXTURE: &str = include_str!(
        "../../../../contracts/desktop-v1/native-binding/background-window-source.json"
    );
    const APP: &str = "com.lingxi.computer-user.guard-fixture";

    // This is an offline replay of captured Source bytes. The post-refresh
    // sequence/time below are synthetic test projections, not a second native
    // read and not evidence that a live Device or Native action occurred.
    fn source_fixture() -> StructureSnapshot {
        serde_json::from_str(SOURCE_FIXTURE)
            .expect("the checked-in actual Source replay fixture is valid")
    }

    fn background_manifest(snapshot: &StructureSnapshot) -> CapabilityManifest {
        let domains = ExecutionDomain::ALL
            .into_iter()
            .map(|domain| {
                let available = domain == ExecutionDomain::BackgroundAx;
                ExecutionDomainCapability {
                    domain,
                    state: if available {
                        CapabilityState::Available
                    } else {
                        CapabilityState::Unavailable
                    },
                    actions: if available {
                        vec![DomainAction::BackgroundAxPress]
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect();
        CapabilityManifest {
            identity: interaction_contracts::desktop::device::DeviceIdentity {
                session_epoch: snapshot.session_epoch,
                surface: snapshot.surface.clone(),
                clock_domain: snapshot.clock_domain.clone(),
                dependencies: BTreeMap::from([
                    ("authorization".into(), 1),
                    ("background_scope".into(), 1),
                    ("binding".into(), 1),
                    ("geometry".into(), 1),
                    ("window".into(), 1),
                ]),
            },
            backend: BackendKind::Macos,
            execution_domain: ExecutionDomain::BackgroundAx,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Unavailable,
            input: CapabilityState::Available,
            domains,
            max_uninterruptible_ms: 100,
        }
    }

    fn goal_draft() -> GoalDraft {
        GoalDraft {
            app: APP.into(),
            account: None,
            target: TargetSpec {
                row: None,
                semantic_target: SemanticTargetRef::new("background-toggle").unwrap(),
                label: "Background toggle".into(),
                group: None,
                role_hint: Some("AXCheckBox".into()),
            },
            desired: ExpectedValue::Bool { value: true },
            confirmation_scope: ConfirmationScope::CurrentUi,
            max_effect_class: EffectClass::ReversibleEdit,
        }
    }

    fn admitted_root() -> (Engine, StructureSnapshot) {
        let mut snapshot = source_fixture();
        let now = snapshot.observed_ms;
        let pack = AppPack::registered_native(APP).expect("guard fixture is registered");
        let mut config = super::super::EngineConfig::new(TaskConfig::new(vec![pack]));
        let TargetContext::NativeWindow { app, source } = snapshot.target_context.as_ref().unwrap()
        else {
            panic!("fixture carries its own logical target context");
        };
        config.native_background_window_scope = Some(super::super::NativeBackgroundWindowScope {
            app: app.bundle_id.clone(),
            pid: app.pid,
            window: source.window.origin_window_id.clone(),
            process_start_seconds: Some(source.window.process.start_seconds),
            process_start_micros: Some(source.window.process.start_micros),
        });
        config.initial_mode = interaction_contracts::desktop::operator::RunMode::Execute;
        let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());
        let manifest = background_manifest(&snapshot);
        engine.state.set_identity(&manifest.identity, false);
        engine.device.install_manifest(manifest);
        engine.device.note_lease(true);
        engine.handle(
            now,
            super::super::Input::Structure(Box::new(snapshot.clone())),
        );

        engine
            .tasks
            .create_root(
                &mut engine.storage,
                now + 1,
                "Enable background fixture toggle",
            )
            .unwrap();
        let target = goal_draft().target.semantic_target.clone();
        let plan = PlanBundle {
            subgoals: vec![Subgoal {
                id: "enable-toggle".into(),
                description: "Enable the registered background toggle".into(),
                skill: "ui.ensure_value".into(),
                target: Some(target),
                expects: Vec::new(),
                scroll: None,
                select: None,
            }],
            dependencies: Vec::new(),
            required_observations: Vec::new(),
            expected_effects: Vec::new(),
            recovery_branches: Vec::new(),
            termination: Vec::new(),
        };
        let confirmation = engine
            .tasks
            .accept_draft(
                &mut engine.storage,
                now + 1,
                &goal_draft(),
                ExecutionDomain::BackgroundAx,
                &plan,
            )
            .unwrap();
        let outputs = engine.handle(
            now + 2,
            super::super::Input::Operator(
                interaction_contracts::desktop::operator::OperatorCommand::ConfirmGoal {
                    confirmation_id: confirmation.confirmation_id,
                    approved: true,
                },
            ),
        );
        assert!(
            !engine.goal_evidence_usable(),
            "the pre-confirmation snapshot cannot start the frozen BackgroundAX root"
        );
        let register = outputs
            .iter()
            .find_map(|output| match output {
                super::super::Output::Device { corr, command }
                    if matches!(command.as_ref(), DeviceCommand::RegisterRoot(_)) =>
                {
                    Some(*corr)
                }
                _ => None,
            })
            .expect("confirmed BackgroundAX root registers through Device");
        let registered = engine.handle(
            now + 3,
            super::super::Input::Device {
                corr: register,
                reply: super::super::DeviceReply::Ack(
                    interaction_contracts::desktop::device::CommandAck::ok(),
                ),
            },
        );
        let refresh_generation = registered
            .iter()
            .find_map(|output| match output {
                super::super::Output::Source {
                    generation,
                    command: SourceCommand::RefreshNow { .. },
                } => Some(*generation),
                _ => None,
            })
            .expect("registered root requests a fresh Source read");
        engine.handle(
            now + 4,
            super::super::Input::SourceRefreshStarted {
                generation: refresh_generation,
                accepted: true,
            },
        );
        assert!(
            !engine.state.refresh_pending(),
            "the matching ACK was consumed"
        );
        assert!(
            !engine.background_ax_evidence_usable(None),
            "the refresh acknowledgement does not make its pre-ack read current"
        );

        // Project the replay's immutable target facts onto the next monotonic
        // Source sequence to exercise the post-ack gate deterministically.
        snapshot.source_sequence += 1;
        snapshot.snapshot_id = interaction_contracts::desktop::ids::RequestId::new(
            "background-parameter-post-refresh",
        )
        .unwrap();
        snapshot.observed_ms += 5;
        if let Some(TargetContext::NativeWindow { app, source }) = snapshot.target_context.as_mut()
        {
            source.window.verified_source_sequence = snapshot.source_sequence;
            source.window.verified_observed_ms = snapshot.observed_ms;
            source.target_context_hash = source.computed_target_context_hash(app).unwrap();
        }
        engine.handle(
            snapshot.observed_ms,
            super::super::Input::Structure(Box::new(snapshot.clone())),
        );
        assert!(engine.run.is_some(), "the confirmed Root remains active");
        (engine, snapshot)
    }

    fn candidate(
        binding: &crate::environment::binding::Binding,
        snapshot: &StructureSnapshot,
    ) -> Candidate {
        let target = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.instance)
            .unwrap();
        Candidate {
            candidate_id: CandidateId::new("ensure-background-toggle").unwrap(),
            skill: "ui.ensure_value".into(),
            semantic_target: Some(binding.semantic_target.clone()),
            binding: Some(binding.binding_ref.clone()),
            parameters: BTreeMap::from([
                ("instance".into(), binding.instance.as_str().into()),
                ("role".into(), target.role.clone()),
                (
                    "label".into(),
                    target
                        .title
                        .as_deref()
                        .or(target.description.as_deref())
                        .unwrap()
                        .into(),
                ),
                ("desired".into(), "on".into()),
            ]),
            effect_class: EffectClass::ReversibleEdit,
            precondition: Precondition::Executable,
            description: "Enable Background toggle".into(),
        }
    }

    #[test]
    fn background_ensure_value_admission_requires_exact_canonical_parameters() {
        let (mut engine, snapshot) = admitted_root();
        let env = engine.state.latest().unwrap().clone();
        let goal = engine.run.as_ref().unwrap().goal.clone();
        let binding = engine
            .bindings
            .bind_background_ax(&env, &goal.target, snapshot.observed_ms, 2_000)
            .expect("the actual Source replay binds the frozen guard-fixture target");

        let canonical = candidate(&binding, &snapshot);
        assert!(engine.background_ax_candidate_parameters_match(
            &canonical,
            &goal,
            &env,
            Skill::EnsureValue,
        ));

        let mut extra = canonical.clone();
        extra.parameters.insert("ignored".into(), "extra".into());
        assert!(!engine.background_ax_candidate_parameters_match(
            &extra,
            &goal,
            &env,
            Skill::EnsureValue,
        ));

        let mut missing = canonical.clone();
        missing.parameters.remove("desired");
        assert!(!engine.background_ax_candidate_parameters_match(
            &missing,
            &goal,
            &env,
            Skill::EnsureValue,
        ));

        for (key, value) in [("desired", "off"), ("label", "Different control")] {
            let mut mismatch = canonical.clone();
            mismatch.parameters.insert(key.into(), value.into());
            assert!(!engine.background_ax_candidate_parameters_match(
                &mismatch,
                &goal,
                &env,
                Skill::EnsureValue,
            ));
        }
    }
}
