//! CandidateFactory: the finite set of actions a decision may choose from
//! (design 9.2 to 9.4).
//!
//! Candidates come from the plan's current step, the current bindings and the
//! layer that receives input. Each carries an effect class and a precondition
//! status decided by code, so a backend can only pick among actions that were
//! already allowed to be considered. The observe, plan and stop paths are
//! always present and are never crowded out.

use std::collections::{BTreeMap, BTreeSet};

use interaction_contracts::desktop::binding::{
    BindCandidate, BindingProofScope, derive_background_ax_binding_evidence, instance_fingerprint,
    normalize_label,
};
use interaction_contracts::desktop::decision::{Candidate, Precondition};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::{
    Admission, DenyReason, EffectClass, EffectContext, admit,
};
use interaction_contracts::desktop::ids::{BindingRef, CandidateId, SemanticTargetRef};
use interaction_contracts::desktop::planning::{PlanBundle, Subgoal};
use interaction_contracts::desktop::source::{LayerKind, TargetContext, UiObject};
use interaction_contracts::desktop::task::{
    ConfirmationScope, ExpectedValue, GoalSpec, TargetSpec,
};

use crate::environment::binding::{Binding, BindingReducer, RowBinding};
use crate::environment::pack::{AppPack, ScopedControlValueKind};
use crate::environment::read::{focused_window_id, object_for, value_text};
use crate::environment::state_core::EnvSnapshot;
use crate::tasks::goal::{
    scoped_number_form_scroll_container, scoped_number_scroll_step_matches, skill_class,
    validate_plan,
};

pub const OBSERVE_MORE: &str = "observe_more";
pub const REQUEST_PLANNING: &str = "request_planning";
pub const STOP_OR_YIELD: &str = "stop_or_yield";

/// The three paths that are not actions on the application.
pub fn is_control(skill: &str) -> bool {
    matches!(skill, OBSERVE_MORE | REQUEST_PLANNING | STOP_OR_YIELD)
}

/// How the step's target resolved against the current evidence.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Bound(Box<Binding>),
    /// A public row selector resolved only through the table viewport path.
    /// Its private Source refs stay in the Engine and are never copied into a
    /// generic control candidate.
    Row(Box<RowBinding>),
    /// A registered TextEdit receiver resolved only from the private owned
    /// Source proof. The reference is opaque to generic binding code.
    OwnedDocument(BindingRef),
    Ambiguous(Vec<BindCandidate>),
    NotFound {
        observation_complete: bool,
    },
    /// The evidence cannot be used right now.
    Unavailable(String),
}

/// A control in the front layer that an action could press.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerControl {
    pub binding: Binding,
    pub role: String,
    pub label: String,
}

pub enum Basis<'a> {
    /// The plan's current step.
    Step {
        subgoal: &'a Subgoal,
        resolution: &'a Resolution,
    },
    /// A dialog or sheet is in front; no plan step can be acted on behind it.
    Layer {
        title: &'a str,
        controls: &'a [LayerControl],
    },
}

/// What the gate and the descriptions are checked against.
pub struct Scope<'a> {
    pub goal: &'a GoalSpec,
    /// The semantic targets of the installed plan.
    pub plan_targets: &'a BTreeSet<String>,
    pub pack: &'a AppPack,
    pub env: &'a EnvSnapshot,
}

/// Return the registered popup labels only for a Host-frozen scoped choice
/// control. The labels remain in AppPack rather than user or plan authority.
pub(crate) fn scoped_choice_values(
    goal: &GoalSpec,
    pack: &AppPack,
) -> Option<&'static [&'static str]> {
    if goal.execution_domain != ExecutionDomain::ForegroundNative
        || goal.confirmation_scope != ConfirmationScope::UiRevisit
        || goal.scoped_revisit.is_none()
        || goal.scoped_commit.is_some()
        || goal.allowed_scope.as_ref()?.targets.len() != 1
        || goal.allowed_scope.as_ref()?.targets[0].form.is_none()
    {
        return None;
    }
    match pack
        .scoped_control(&goal.allowed_scope.as_ref()?.targets[0])?
        .value_kind
    {
        ScopedControlValueKind::Choice(values) => Some(values),
        ScopedControlValueKind::Bool
        | ScopedControlValueKind::Text
        | ScopedControlValueKind::Number
        | ScopedControlValueKind::RadioChoice(_) => None,
    }
}

/// Return the registered radio labels only for a Host-frozen radio-group
/// choice. This remains separate from popup/menu admission.
pub(crate) fn scoped_radio_choice_values(
    goal: &GoalSpec,
    pack: &AppPack,
) -> Option<&'static [&'static str]> {
    if goal.validate().is_err()
        || goal.execution_domain != ExecutionDomain::ForegroundNative
        || goal.confirmation_scope != ConfirmationScope::UiRevisit
        || goal.scoped_revisit.is_none()
        || goal.scoped_commit.is_some()
        || goal.scope.app != pack.bundle_id
    {
        return None;
    }
    let allowed = goal.allowed_scope.as_ref()?;
    if allowed.targets.len() != 1
        || allowed.validate().is_err()
        || (allowed.application != pack.name && allowed.application != pack.bundle_id)
    {
        return None;
    }
    let target = &allowed.targets[0];
    let scoped = pack.scoped_control(target)?;
    let choices = match scoped.value_kind {
        ScopedControlValueKind::RadioChoice(values) => values,
        ScopedControlValueKind::Bool
        | ScopedControlValueKind::Text
        | ScopedControlValueKind::Number
        | ScopedControlValueKind::Choice(_) => return None,
    };
    if scoped.context_kind != crate::environment::pack::ScopedControlContextKind::Form
        || scoped.control.role != "AXRadioGroup"
        || goal.target.semantic_target.as_str() != scoped.control.semantic
        || goal.target.label != scoped.control.label
        || goal.target.role_hint.as_deref() != Some("AXRadioGroup")
        || goal.target.group.as_deref() != scoped.context_label
        || target.form.as_deref() != scoped.context_label
    {
        return None;
    }
    matches!(&goal.desired, ExpectedValue::Text { value } if choices.contains(&value.as_str()))
        .then_some(choices)
}

fn has_registered_radio_choice_scope(goal: &GoalSpec, pack: &AppPack) -> bool {
    goal.allowed_scope
        .as_ref()
        .and_then(|scope| scope.targets.first())
        .and_then(|target| pack.scoped_control(target))
        .is_some_and(|scoped| matches!(scoped.value_kind, ScopedControlValueKind::RadioChoice(_)))
}

fn has_registered_number_scope(goal: &GoalSpec, pack: &AppPack) -> bool {
    goal.allowed_scope
        .as_ref()
        .and_then(|scope| scope.targets.first())
        .and_then(|target| pack.scoped_control(target))
        .is_some_and(|scoped| scoped.value_kind == ScopedControlValueKind::Number)
}

/// A scoped native plan can touch its frozen value, one exact registered
/// popup choice or manual Save action, and its Host-authorized revisit.
pub(crate) fn scoped_revisit_step_is_allowed(
    goal: &GoalSpec,
    pack: &AppPack,
    subgoal: &Subgoal,
) -> bool {
    if goal.scoped_revisit.is_none() && goal.scoped_commit.is_none() {
        return !has_registered_radio_choice_scope(goal, pack)
            && !has_registered_number_scope(goal, pack);
    }
    if goal.validate().is_err()
        || goal.allowed_scope.is_none()
        || goal.execution_domain != ExecutionDomain::ForegroundNative
    {
        return false;
    }
    if subgoal.scroll.is_some() || subgoal.skill == "ui.scroll_to" {
        return scoped_number_form_scroll_container(goal, pack).is_some_and(|container| {
            scoped_number_scroll_step_matches(goal, container, subgoal)
                && subgoal
                    .scroll
                    .as_ref()
                    .is_some_and(|spec| spec.validate().is_ok())
        });
    }
    if has_registered_number_scope(goal, pack)
        && scoped_number_form_scroll_container(goal, pack).is_none()
    {
        return false;
    }
    if let Some(choices) =
        scoped_choice_values(goal, pack).or_else(|| scoped_radio_choice_values(goal, pack))
    {
        let read = subgoal.skill == "ui.read_value"
            && subgoal.target.as_ref() == Some(&goal.target.semantic_target)
            && subgoal.select.is_none();
        let revisit = goal.confirmation_scope == ConfirmationScope::UiRevisit
            && subgoal.skill == "ui.revisit"
            && subgoal.target.as_ref() == Some(&goal.target.semantic_target)
            && subgoal.select.is_none();
        let select = subgoal.skill == "ui.select_by_label"
            && subgoal.target.as_ref() == Some(&goal.target.semantic_target)
            && subgoal.select.as_ref().is_some_and(|select| {
                select.validate().is_ok()
                    && choices.contains(&select.choice_label.as_str())
                    && matches!(
                        &goal.desired,
                        ExpectedValue::Text { value } if value == &select.choice_label
                    )
            });
        return read || revisit || select;
    }
    if subgoal.select.is_some() {
        return false;
    }
    let primary = subgoal.target.as_ref() == Some(&goal.target.semantic_target)
        && matches!(subgoal.skill.as_str(), "ui.ensure_value" | "ui.read_value");
    let revisit = goal.scoped_revisit.is_some()
        && goal.confirmation_scope == ConfirmationScope::UiRevisit
        && subgoal.skill == "ui.revisit"
        && subgoal.target.as_ref() == Some(&goal.target.semantic_target);
    let commit_action = goal
        .scoped_commit
        .as_ref()
        .filter(|commit| {
            commit.receipt.kind == interaction_contracts::desktop::task::ScopedCommitKind::Manual
        })
        .and_then(|commit| commit.action.as_ref())
        .is_some_and(|action| {
            goal.confirmation_scope == ConfirmationScope::Commit
                && subgoal.skill == "ui.activate"
                && subgoal.target.as_ref() == Some(&action.semantic_target)
        });
    primary || revisit || commit_action
}

pub(crate) fn scoped_revisit_plan_is_allowed(
    goal: &GoalSpec,
    pack: &AppPack,
    plan: &PlanBundle,
) -> bool {
    if has_registered_number_scope(goal, pack) {
        if goal.validate().is_err() || scoped_number_form_scroll_container(goal, pack).is_none() {
            return false;
        }
        // Reuse the Host's exact numeric plan validator at admission and
        // dispatch; the model cannot supply a different container or order.
        let registered = super::skills::Skill::IMPLEMENTED
            .iter()
            .map(|skill| (*skill).to_owned())
            .collect::<Vec<_>>();
        return validate_plan(goal, plan, pack, &registered).is_ok();
    }
    let manual_action = goal
        .scoped_commit
        .as_ref()
        .filter(|commit| {
            commit.receipt.kind == interaction_contracts::desktop::task::ScopedCommitKind::Manual
        })
        .and_then(|commit| commit.action.as_ref());
    let choice_values =
        scoped_choice_values(goal, pack).or_else(|| scoped_radio_choice_values(goal, pack));
    if goal.scoped_revisit.is_none()
        && manual_action.is_none()
        && choice_values.is_none()
        && !has_registered_radio_choice_scope(goal, pack)
    {
        return true;
    }
    if goal.validate().is_err()
        || plan
            .subgoals
            .iter()
            .any(|step| !scoped_revisit_step_is_allowed(goal, pack, step))
    {
        return false;
    }
    if let Some(choices) = choice_values {
        let selects: Vec<_> = plan
            .subgoals
            .iter()
            .filter(|step| step.skill == "ui.select_by_label")
            .collect();
        let revisits: Vec<_> = plan
            .subgoals
            .iter()
            .filter(|step| step.skill == "ui.revisit")
            .collect();
        let ([select], [revisit]) = (selects.as_slice(), revisits.as_slice()) else {
            return false;
        };
        if plan.subgoals.len() < 2
            || select.target.as_ref() != Some(&goal.target.semantic_target)
            || revisit.target.as_ref() != Some(&goal.target.semantic_target)
            || revisit.scroll.is_some()
            || revisit.select.is_some()
            || !select.select.as_ref().is_some_and(|spec| {
                spec.validate().is_ok()
                    && choices.contains(&spec.choice_label.as_str())
                    && matches!(
                        &goal.desired,
                        ExpectedValue::Text { value } if value == &spec.choice_label
                    )
            })
            || !plan.recovery_branches.is_empty()
            || plan.subgoals.iter().any(|step| {
                step.id != revisit.id && !dependency_path_exists(plan, &step.id, &revisit.id)
            })
        {
            return false;
        }
        return true;
    }
    if goal.scoped_revisit.is_none() {
        let Some(action) = manual_action else {
            return false;
        };
        let ensures: Vec<_> = plan
            .subgoals
            .iter()
            .filter(|step| {
                step.skill == "ui.ensure_value"
                    && step.target.as_ref() == Some(&goal.target.semantic_target)
            })
            .collect();
        let saves: Vec<_> = plan
            .subgoals
            .iter()
            .filter(|step| {
                step.skill == "ui.activate" && step.target.as_ref() == Some(&action.semantic_target)
            })
            .collect();
        if ensures.len() != 1 || saves.len() != 1 {
            return false;
        }
        let save = saves[0];
        return plan.subgoals.iter().all(|step| {
            let allowed_primary =
                matches!(step.skill.as_str(), "ui.ensure_value" | "ui.read_value")
                    && step.target.as_ref() == Some(&goal.target.semantic_target);
            step.id == save.id
                || allowed_primary && dependency_path_exists(plan, &step.id, &save.id)
        }) && dependency_path_exists(plan, &ensures[0].id, &save.id);
    }
    let ensures: Vec<_> = plan
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.ensure_value")
        .collect();
    let revisits: Vec<_> = plan
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.revisit")
        .collect();
    if ensures.len() != 1 || revisits.len() != 1 {
        return false;
    }
    plan.subgoals
        .iter()
        .filter(|step| matches!(step.skill.as_str(), "ui.ensure_value" | "ui.read_value"))
        .all(|step| dependency_path_exists(plan, &step.id, &revisits[0].id))
}

fn dependency_path_exists(plan: &PlanBundle, before: &str, after: &str) -> bool {
    let mut frontier = vec![after];
    let mut seen = BTreeSet::new();
    while let Some(current) = frontier.pop() {
        if !seen.insert(current) {
            continue;
        }
        for dependency in plan
            .dependencies
            .iter()
            .filter(|dependency| dependency.after == current)
        {
            if dependency.before == before {
                return true;
            }
            frontier.push(&dependency.before);
        }
    }
    false
}

/// The effect gate of 9.3 for one action. Candidate generation and admission
/// both call this, so they can never disagree.
pub fn gate(
    scope: &Scope<'_>,
    class: EffectClass,
    target: Option<&SemanticTargetRef>,
    confirmed: bool,
) -> Admission {
    let goal_target = &scope.goal.target.semantic_target;
    let primary_target = target.is_some_and(|target| target == goal_target);
    let scoped_commit_target = scope
        .goal
        .scoped_commit
        .as_ref()
        .filter(|commit| {
            commit.receipt.kind == interaction_contracts::desktop::task::ScopedCommitKind::Manual
                && scope.goal.confirmation_scope == ConfirmationScope::Commit
                && scope.goal.max_effect_class == EffectClass::Commit
        })
        .and_then(|commit| commit.action.as_ref())
        .is_some_and(|action| {
            class == EffectClass::Commit
                && target.is_some_and(|target| target == &action.semantic_target)
        });
    let scoped_number_container = class == EffectClass::Navigation
        && scoped_number_form_scroll_container(scope.goal, scope.pack).is_some_and(|container| {
            target.is_some_and(|target| target.as_str() == container.semantic)
        });
    let named = if scope.goal.allowed_scope.is_some() && class == EffectClass::Commit {
        scoped_commit_target
    } else {
        primary_target || scoped_commit_target || scoped_number_container
    };
    if scope.goal.allowed_scope.is_some() && !named {
        // The ordinary effect table admits all navigation regardless of target.
        // Typed scopes are stricter: even read/navigation candidates must name
        // the frozen control or its exact trusted numeric scroll container.
        return Admission::Deny(DenyReason::TargetNotCovered);
    }
    let covered = named || target.is_some_and(|t| scope.plan_targets.contains(t.as_str()));
    admit(
        class,
        &EffectContext {
            in_scope: scope.env.snapshot.app.bundle_id == scope.goal.scope.app,
            target_covered: covered,
            target_named: named,
            max_effect_class: scope.goal.max_effect_class,
            confirmed,
        },
    )
}

fn deny_text(reason: DenyReason) -> &'static str {
    match reason {
        DenyReason::OutOfScope => "outside the task's application and account",
        DenyReason::TargetNotCovered => "the goal does not cover this control",
        DenyReason::DestructiveNotNamed => "destructive, and the goal does not name it",
        DenyReason::ExternalNotSupported => "external effects are not executed in this phase",
        DenyReason::UnknownEffect => "what it does is unknown, so it is not executed",
    }
}

pub fn slug(text: &str) -> String {
    normalize_label(text).replace(' ', "-")
}

fn label_of(object: &UiObject) -> &str {
    object
        .title
        .as_deref()
        .or(object.description.as_deref())
        .unwrap_or("")
}

/// Locate a bound control in its own window for BackgroundAX. Foreground
/// `object_for` deliberately requires the globally focused window; BackgroundAX
/// instead uses the frozen target-context witness and the same-read graph.
fn bound_target_object<'a>(
    env: &'a EnvSnapshot,
    goal: &GoalSpec,
    pack: &AppPack,
    binding: &Binding,
) -> Option<&'a UiObject> {
    if goal.execution_domain != ExecutionDomain::BackgroundAx {
        return object_for(env, &binding.fingerprint_digest);
    }

    let snapshot = &env.snapshot;
    if binding.rule != "background_ax"
        || binding.semantic_target != goal.target.semantic_target
        || goal.target.row.is_some()
        || goal.scope.app != pack.bundle_id
        || snapshot.app.bundle_id != goal.scope.app
        || !snapshot.complete
        || snapshot.menu_context.is_some()
        || snapshot.observer_coverage.state
            == interaction_contracts::desktop::source::SourceState::Unavailable
        || snapshot.validate().is_err()
    {
        return None;
    }
    let BindingProofScope::BackgroundAx {
        target_context_hash,
    } = &binding.continuity.scope
    else {
        return None;
    };
    let TargetContext::NativeWindow { app, source } = snapshot.target_context.as_ref()? else {
        return None;
    };
    if app != &snapshot.app
        || app.bundle_id != goal.scope.app
        || source.target_context_hash.as_str() != target_context_hash.as_str()
        || !source.is_visible_standard_window()
        || source.window.session_epoch != snapshot.session_epoch
        || source.window.verified_source_sequence != snapshot.source_sequence
        || source.window.verified_observed_ms != snapshot.observed_ms
    {
        return None;
    }
    let mut matching = snapshot
        .objects
        .iter()
        .filter(|object| object.instance_ref == binding.instance);
    let object = matching.next()?;
    if matching.next().is_some()
        || object.window_id != source.window.origin_window_id
        || !object.enabled
        || !object.in_view
        || object.secure
        || object.scroll.is_some()
        || instance_fingerprint(snapshot, object).digest() != binding.fingerprint_digest
    {
        return None;
    }
    let evidence = derive_background_ax_binding_evidence(snapshot, object).ok()?;
    if !binding.device_context.supports_background_ax_press()
        || !evidence.device_context.supports_background_ax_press()
        || !binding.continuity.same_closure(&evidence.continuity)
        || binding.continuity.source_sequence > evidence.continuity.source_sequence
    {
        return None;
    }
    Some(object)
}

fn is_layer_button_role(role: &str) -> bool {
    matches!(role, "button" | "AXButton")
}

/// Whether the window that receives input is a dialog or sheet.
pub fn modal_in_front(env: &EnvSnapshot) -> bool {
    let Some(active) = focused_window_id(&env.snapshot) else {
        return false;
    };
    env.snapshot
        .windows
        .iter()
        .find(|window| window.window_id == active)
        .is_some_and(|window| {
            window.modal
                || matches!(
                    window.kind,
                    LayerKind::Sheet | LayerKind::Dialog | LayerKind::Popover | LayerKind::Menu
                )
        })
}

pub fn layer_title(env: &EnvSnapshot) -> String {
    let active = focused_window_id(&env.snapshot).unwrap_or("");
    env.snapshot
        .windows
        .iter()
        .find(|window| window.window_id == active)
        .and_then(|window| window.title.clone())
        .unwrap_or_else(|| "a dialog".into())
}

/// Bind every pressable control in the front layer, so a decision can choose
/// among them. The bindings end with the layer.
pub fn bind_layer_controls(env: &EnvSnapshot, bindings: &mut BindingReducer) -> Vec<LayerControl> {
    let Some(active) = focused_window_id(&env.snapshot) else {
        return Vec::new();
    };
    let mut controls = Vec::new();
    for object in &env.snapshot.objects {
        if object.window_id != active
            || !object.enabled
            || !object.in_view
            || object.secure
            || !is_layer_button_role(&object.role)
            || label_of(object).is_empty()
        {
            continue;
        }
        let Ok(target) = SemanticTargetRef::new(format!("layer/{}", slug(label_of(object)))) else {
            continue;
        };
        if let Some(binding) = bindings.bind_instance(env, target, &object.instance_ref) {
            controls.push(LayerControl {
                binding,
                role: object.role.clone(),
                label: label_of(object).to_string(),
            });
        }
    }
    controls
}

struct Draft {
    skill: String,
    target: Option<SemanticTargetRef>,
    binding: Option<BindingRef>,
    parameters: BTreeMap<String, String>,
    class: EffectClass,
    /// Set when the evidence does not allow acting yet.
    needs_observation: Option<String>,
    description: String,
}

fn step_class(scope: &Scope<'_>, skill: &str, object: Option<&UiObject>) -> EffectClass {
    let base = skill_class(skill).unwrap_or(EffectClass::Unknown);
    match (skill, object) {
        // Reading and moving around change nothing in the application's data,
        // whatever the control they start from would do if pressed.
        ("ui.read_value" | "ui.revisit" | "ui.scroll_to" | "ui.activate_window", _) => base,
        (_, Some(object)) => base.raise(scope.pack.effect_class(&object.role, label_of(object))),
        (_, None) => EffectClass::Unknown,
    }
}

fn where_is(object: &UiObject) -> String {
    match &object.group {
        Some(group) => format!("{} in {group}", object.role),
        None => object.role.clone(),
    }
}

fn describe(scope: &Scope<'_>, skill: &str, object: &UiObject, current: Option<&str>) -> String {
    let label = label_of(object);
    let place = where_is(object);
    let now = current
        .map(|value| format!(", currently {value}"))
        .unwrap_or_default();
    match skill {
        "ui.ensure_value" => format!(
            "Set \"{label}\" ({place}{now}) to {}",
            value_text(&scope.goal.desired)
        ),
        "ui.read_value" => format!("Read the value of \"{label}\" ({place})"),
        "ui.activate" => format!("Press \"{label}\" ({place})"),
        "ui.revisit" => format!("Leave the page and come back to re-read \"{label}\""),
        other => format!("Run {other} on \"{label}\" ({place})"),
    }
}

fn shown_value(object: &UiObject) -> Option<String> {
    object.value.as_ref().map(|value| match value {
        interaction_contracts::desktop::source::UiValue::Bool(true) => "on".to_string(),
        interaction_contracts::desktop::source::UiValue::Bool(false) => "off".to_string(),
        other => crate::environment::read::display(other),
    })
}

fn public_row_value(value: &ExpectedValue) -> String {
    match value {
        ExpectedValue::Bool { value } => value.to_string(),
        ExpectedValue::Text { value } => format!("\"{value}\""),
        ExpectedValue::Number { value } => value.to_string(),
    }
}

pub(crate) fn public_row_scope(target: &TargetSpec) -> String {
    let Some(row) = target.row.as_ref() else {
        return target.label.clone();
    };
    let ancestry = row
        .table_scope
        .scroll_path
        .iter()
        .map(|container| format!("{} \"{}\"", container.role.ax_role(), container.label))
        .collect::<Vec<_>>()
        .join(" / ");
    let headers = row.table_scope.column_headers.join(", ");
    let conditions = row
        .conditions
        .iter()
        .map(|condition| {
            format!(
                "{} = {}",
                condition.column_name,
                public_row_value(&condition.value)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("table beneath {ancestry}; columns [{headers}]; row conditions [{conditions}]")
}

fn step_draft(
    scope: &Scope<'_>,
    subgoal: &Subgoal,
    binding: Option<&Binding>,
    object: &UiObject,
) -> Draft {
    let skill = subgoal.skill.as_str();
    let mut parameters = BTreeMap::new();
    parameters.insert("instance".into(), object.instance_ref.as_str().to_string());
    parameters.insert("role".into(), object.role.clone());
    parameters.insert("label".into(), label_of(object).to_string());
    if skill == "ui.ensure_value" {
        parameters.insert("desired".into(), value_text(&scope.goal.desired));
    }
    let current = shown_value(object);
    Draft {
        skill: skill.to_string(),
        target: subgoal
            .scroll
            .as_ref()
            .map(|s| s.container.semantic_target.clone())
            .or_else(|| subgoal.target.clone())
            .or_else(|| {
                scope
                    .goal
                    .scoped_revisit
                    .as_ref()
                    .map(|_| scope.goal.target.semantic_target.clone())
            }),
        binding: binding.map(|binding| binding.binding_ref.clone()),
        parameters,
        class: step_class(scope, skill, Some(object)),
        needs_observation: None,
        description: describe(scope, skill, object, current.as_deref()),
    }
}

fn control(skill: &str, description: &str) -> Draft {
    Draft {
        skill: skill.into(),
        target: None,
        binding: None,
        parameters: BTreeMap::new(),
        class: EffectClass::ReadOnly,
        needs_observation: None,
        description: description.into(),
    }
}

/// The candidates for one decision, with ids `c1`, `c2`, … in this order.
pub fn build(scope: &Scope<'_>, basis: &Basis<'_>) -> Vec<Candidate> {
    let mut drafts: Vec<Draft> = Vec::new();
    match basis {
        Basis::Step { subgoal, .. }
            if (scope.goal.scoped_revisit.is_some() || scope.goal.scoped_commit.is_some())
                && !scoped_revisit_step_is_allowed(scope.goal, scope.pack, subgoal) =>
        {
            drafts.push(Draft {
                skill: subgoal.skill.clone(),
                target: subgoal.target.clone(),
                binding: None,
                parameters: BTreeMap::new(),
                class: EffectClass::Unknown,
                needs_observation: None,
                description:
                    "the scoped plan step is outside its frozen target, Save, and revisit bindings"
                        .into(),
            });
        }
        Basis::Step {
            subgoal,
            resolution,
        } => match resolution {
            Resolution::Row(binding)
                if matches!(subgoal.skill.as_str(), "ui.select_row" | "ui.revisit")
                    && subgoal.target.as_ref() == Some(&scope.goal.target.semantic_target)
                    && subgoal.scroll.is_none()
                    && subgoal.select.is_none() =>
            {
                let selecting = subgoal.skill == "ui.select_row";
                let ready = !selecting
                    || binding.source.selected == Some(true)
                    || (binding.source.selected == Some(false)
                        && binding.source.ax_selected_settable == Some(true)
                        && binding.device_context.is_some()
                        && binding.continuity.is_some()
                        && binding.hit_point.is_some());
                let waiting_for = if ready {
                    None
                } else if binding.source.selected.is_none() {
                    Some("the actual AXSelected state is unknown".to_string())
                } else {
                    Some("the actual AXSelected attribute is not proven settable".to_string())
                };
                drafts.push(Draft {
                    skill: subgoal.skill.clone(),
                    target: Some(scope.goal.target.semantic_target.clone()),
                    binding: Some(binding.binding_ref.clone()),
                    parameters: BTreeMap::new(),
                    class: skill_class(&subgoal.skill).unwrap_or(EffectClass::Unknown),
                    needs_observation: waiting_for,
                    description: if selecting {
                        format!(
                            "Select the uniquely resolved row using its absolute AXSelected state: {}",
                            public_row_scope(&scope.goal.target)
                        )
                    } else {
                        format!(
                            "Leave and return to verify the same frozen row target: {}",
                            public_row_scope(&scope.goal.target)
                        )
                    },
                });
            }
            Resolution::Row(_) => {}
            Resolution::Bound(_) if subgoal.skill == "ui.save_document" => {}
            Resolution::Bound(binding) => {
                match bound_target_object(scope.env, scope.goal, scope.pack, binding) {
                    Some(object) => {
                        drafts.push(step_draft(scope, subgoal, Some(binding.as_ref()), object))
                    }
                    None => drafts.push(Draft {
                        skill: subgoal.skill.clone(),
                        target: subgoal
                            .scroll
                            .as_ref()
                            .map(|s| s.container.semantic_target.clone())
                            .or_else(|| subgoal.target.clone()),
                        binding: Some(binding.binding_ref.clone()),
                        parameters: BTreeMap::new(),
                        class: skill_class(&subgoal.skill).unwrap_or(EffectClass::Unknown),
                        needs_observation: Some("the bound control is not visible".into()),
                        description: format!(
                            "{} (the control is not visible)",
                            subgoal.description
                        ),
                    }),
                }
            }
            Resolution::OwnedDocument(binding) => drafts.push(Draft {
                skill: subgoal.skill.clone(),
                target: subgoal
                    .target
                    .clone()
                    .or_else(|| Some(scope.goal.target.semantic_target.clone())),
                binding: Some(binding.clone()),
                parameters: BTreeMap::new(),
                class: EffectClass::ReversibleEdit,
                needs_observation: None,
                description: match subgoal.skill.as_str() {
                    "ui.save_document" => "Save the registered document after checking its text",
                    _ => "Set the registered document's text value",
                }
                .into(),
            }),
            Resolution::Ambiguous(options) => {
                for option in options
                    .iter()
                    .filter(|_| scope.goal.allowed_scope.is_none())
                {
                    let Some(object) = scope
                        .env
                        .snapshot
                        .objects
                        .iter()
                        .find(|object| object.instance_ref == option.instance)
                    else {
                        continue;
                    };
                    let mut draft = step_draft(scope, subgoal, None, object);
                    draft
                        .parameters
                        .insert("choice".into(), option.instance.as_str().to_string());
                    drafts.push(draft);
                }
            }
            Resolution::NotFound { .. } => {}
            Resolution::Unavailable(_) if subgoal.skill == "ui.save_document" => {}
            Resolution::Unavailable(reason) => drafts.push(Draft {
                skill: subgoal.skill.clone(),
                target: subgoal
                    .scroll
                    .as_ref()
                    .map(|s| s.container.semantic_target.clone())
                    .or_else(|| subgoal.target.clone()),
                binding: None,
                parameters: BTreeMap::new(),
                class: skill_class(&subgoal.skill).unwrap_or(EffectClass::Unknown),
                needs_observation: Some(reason.clone()),
                description: format!("{} (waiting for evidence: {reason})", subgoal.description),
            }),
        },
        Basis::Layer { title, controls } => {
            // A typed one-control scope cannot authorize a different modal
            // button, even when pressing it would ordinarily count as navigation.
            for layer in controls
                .iter()
                .filter(|_| scope.goal.allowed_scope.is_none())
            {
                let Some(object) = object_for(scope.env, &layer.binding.fingerprint_digest) else {
                    continue;
                };
                let mut parameters = BTreeMap::new();
                parameters.insert("instance".into(), object.instance_ref.as_str().to_string());
                parameters.insert("role".into(), layer.role.clone());
                parameters.insert("label".into(), layer.label.clone());
                parameters.insert("layer".into(), (*title).to_string());
                drafts.push(Draft {
                    skill: "ui.activate".into(),
                    target: Some(layer.binding.semantic_target.clone()),
                    binding: Some(layer.binding.binding_ref.clone()),
                    parameters,
                    class: step_class(scope, "ui.activate", Some(object)),
                    needs_observation: None,
                    description: format!("Press \"{}\" in the \"{title}\" dialog", layer.label),
                });
            }
        }
    }
    drafts.push(control(
        OBSERVE_MORE,
        "Take a fresh observation before deciding",
    ));
    drafts.push(control(REQUEST_PLANNING, "Ask the planner for a new plan"));
    drafts.push(control(
        STOP_OR_YIELD,
        "Stop and hand the task back to the user",
    ));

    drafts
        .into_iter()
        .enumerate()
        .map(|(index, draft)| {
            let (precondition, note) = match (&draft.needs_observation, is_control(&draft.skill)) {
                (_, true) => (Precondition::Executable, ""),
                (Some(_), false) => (Precondition::NeedsObservation, ""),
                (None, false) => match gate(scope, draft.class, draft.target.as_ref(), false) {
                    Admission::Allow => (Precondition::Executable, ""),
                    Admission::NeedsConfirmation => (
                        Precondition::Executable,
                        " (needs the user's confirmation first)",
                    ),
                    Admission::Deny(reason) => (
                        Precondition::Rejected {
                            reason: deny_text(reason).into(),
                        },
                        "",
                    ),
                },
            };
            Candidate {
                candidate_id: CandidateId::new(format!("c{}", index + 1))
                    .expect("a counter is a valid id"),
                skill: draft.skill,
                semantic_target: draft.target,
                binding: draft.binding,
                parameters: draft.parameters,
                effect_class: draft.class,
                precondition,
                description: format!("{}{note}", draft.description),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;
    use interaction_contracts::desktop::binding::BindRequest;
    use interaction_contracts::desktop::ids::RequestId;
    use interaction_contracts::desktop::task::{
        AllowedControlScope, AllowedControlTarget, ConfirmationScope, ScopedUiRevisit,
    };
    use sim_world::app::Modal;

    const TEXT: &str = r#"Turn off "Daily picks" in "Push""#;

    fn targets(bundle: &interaction_contracts::desktop::planning::PlanBundle) -> BTreeSet<String> {
        bundle
            .subgoals
            .iter()
            .filter_map(|subgoal| subgoal.target.as_ref())
            .map(|target| target.as_str().to_string())
            .collect()
    }

    fn by_skill<'a>(candidates: &'a [Candidate], skill: &str) -> Vec<&'a Candidate> {
        candidates.iter().filter(|c| c.skill == skill).collect()
    }

    fn live_background_lookup_inputs() -> (GoalSpec, AppPack, EnvSnapshot, Binding) {
        const APP: &str = "com.lingxi.computer-user.guard-fixture";
        let snapshot: interaction_contracts::desktop::source::StructureSnapshot =
            serde_json::from_str(include_str!(
                "../../../../contracts/desktop-v1/native-binding/background-window-source.json"
            ))
            .expect("the captured Source fixture remains a valid public snapshot");
        let env = EnvSnapshot {
            revision: 1,
            layer_revision: 1,
            snapshot,
            page: None,
        };
        let mut goal =
            crate::testkit::goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        goal.execution_domain = ExecutionDomain::BackgroundAx;
        goal.scope.app = APP.into();
        goal.target.semantic_target = SemanticTargetRef::new("background-toggle").unwrap();
        goal.target.label = "Background toggle".into();
        goal.target.group = None;
        goal.target.role_hint = Some("AXCheckBox".into());
        goal.desired = ExpectedValue::Bool { value: true };
        let pack = AppPack::registered_native(APP).expect("the guard fixture is registered");
        let object = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.title.as_deref() == Some("Background toggle"))
            .expect("the captured tree contains the registered toggle");
        let evidence = derive_background_ax_binding_evidence(&env.snapshot, object)
            .expect("the captured target has its raw AXPress proof");
        let binding = Binding {
            binding_ref: BindingRef::new("bg-bind-live-source-candidate").unwrap(),
            semantic_target: goal.target.semantic_target.clone(),
            instance: object.instance_ref.clone(),
            fingerprint_digest: instance_fingerprint(&env.snapshot, object).digest(),
            layer_revision: env.layer_revision,
            env_revision: env.revision,
            snapshot_id: env.snapshot.snapshot_id.clone(),
            observed_ms: env.snapshot.observed_ms,
            page: None,
            rule: "background_ax".into(),
            continuity: evidence.continuity,
            device_context: evidence.device_context,
            identity_boundary: env.snapshot.identity_boundary.clone(),
        };
        (goal, pack, env, binding)
    }

    #[test]
    fn background_bound_target_uses_raw_ax_press_and_not_global_focus_or_typed_actions() {
        let (goal, pack, env, binding) = live_background_lookup_inputs();
        let target = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.instance)
            .unwrap();

        assert!(focused_window_id(&env.snapshot).is_none());
        assert!(target.actions.is_empty());
        assert_eq!(
            target
                .ax_action_names
                .as_ref()
                .unwrap()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["AXPress"]
        );
        assert!(object_for(&env, &binding.fingerprint_digest).is_none());
        assert_eq!(
            bound_target_object(&env, &goal, &pack, &binding)
                .map(|object| object.instance_ref.clone()),
            Some(binding.instance.clone())
        );

        let mut foreground_goal = goal.clone();
        foreground_goal.execution_domain = ExecutionDomain::ForegroundNative;
        assert!(bound_target_object(&env, &foreground_goal, &pack, &binding).is_none());
    }

    #[test]
    fn background_bound_target_rejects_scope_window_identity_and_fingerprint_changes() {
        let (goal, pack, env, binding) = live_background_lookup_inputs();

        let mut changed_hash = binding.clone();
        let BindingProofScope::BackgroundAx {
            target_context_hash,
        } = &mut changed_hash.continuity.scope
        else {
            panic!("the captured binding must use the background scope");
        };
        *target_context_hash = format!("tc1:{}", "0".repeat(64));
        assert!(bound_target_object(&env, &goal, &pack, &changed_hash).is_none());

        let mut foreign_window = env.clone();
        let object = foreign_window
            .snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref == binding.instance)
            .unwrap();
        object.window_id = "window-from-another-context".into();
        assert!(bound_target_object(&foreign_window, &goal, &pack, &binding).is_none());

        let mut changed_identity = env.clone();
        let TargetContext::NativeWindow { app, source } =
            changed_identity.snapshot.target_context.as_mut().unwrap()
        else {
            panic!("the captured snapshot must carry a native target context");
        };
        source.window.process.start_micros += 1;
        source.target_context_hash = source.computed_target_context_hash(app).unwrap();
        assert!(changed_identity.snapshot.validate().is_ok());
        assert!(bound_target_object(&changed_identity, &goal, &pack, &binding).is_none());

        let mut changed_fingerprint = binding.clone();
        changed_fingerprint.fingerprint_digest = "a different instance".into();
        assert!(bound_target_object(&env, &goal, &pack, &changed_fingerprint).is_none());
    }

    #[test]
    fn a_bound_step_becomes_one_executable_candidate_beside_the_control_paths() {
        let mut rig = EnvRig::new();
        let env = rig.observe(10);
        let binding = rig.bind_goal_target();
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let bundle = bundle(TEXT);
        let plan_targets = targets(&bundle);
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let resolution = Resolution::Bound(Box::new(binding.clone()));
        let candidates = build(
            &scope,
            &Basis::Step {
                subgoal: &bundle.subgoals[0],
                resolution: &resolution,
            },
        );

        let ids: Vec<&str> = candidates.iter().map(|c| c.candidate_id.as_str()).collect();
        assert_eq!(ids, ["c1", "c2", "c3", "c4"]);
        let step = &candidates[0];
        assert_eq!(step.skill, "ui.ensure_value");
        assert_eq!(step.effect_class, EffectClass::ReversibleEdit);
        assert_eq!(step.precondition, Precondition::Executable);
        assert_eq!(step.binding.as_ref(), Some(&binding.binding_ref));
        assert_eq!(step.parameters["desired"], "off");
        assert_eq!(step.parameters["instance"], "sw-push-daily");
        assert!(step.description.contains("Daily picks") && step.description.contains("Push"));
        let controls: Vec<&str> = candidates[1..].iter().map(|c| c.skill.as_str()).collect();
        assert_eq!(controls, [OBSERVE_MORE, REQUEST_PLANNING, STOP_OR_YIELD]);
        assert!(
            candidates[1..]
                .iter()
                .all(|c| c.precondition == Precondition::Executable)
        );
    }

    #[test]
    fn generic_revisit_candidate_keeps_its_navigation_effect_class() {
        let mut rig = EnvRig::new();
        let env = rig.observe(10);
        let binding = rig.bind_goal_target();
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::Navigation);
        let bundle = bundle(TEXT);
        let plan_targets = targets(&bundle);
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let subgoal = Subgoal {
            id: "revisit".into(),
            description: "leave and return to verify the target".into(),
            skill: "ui.revisit".into(),
            target: Some(SemanticTargetRef::new(TARGET).unwrap()),
            expects: vec![],
            scroll: None,
            select: None,
        };
        let resolution = Resolution::Bound(Box::new(binding));
        let candidates = build(
            &scope,
            &Basis::Step {
                subgoal: &subgoal,
                resolution: &resolution,
            },
        );
        let revisit = candidates
            .iter()
            .find(|candidate| candidate.skill == "ui.revisit")
            .unwrap();

        assert_eq!(revisit.effect_class, EffectClass::Navigation);
        assert_eq!(revisit.precondition, Precondition::Executable);
    }

    #[test]
    fn scoped_revisit_candidate_keeps_the_primary_target_and_rejects_other_skills() {
        let mut rig = EnvRig::new();
        let env = rig.observe(10);
        let binding = rig.bind_goal_target();
        let mut goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        goal.execution_domain = ExecutionDomain::ForegroundNative;
        goal.allowed_scope = Some(AllowedControlScope {
            application: goal.scope.app.clone(),
            targets: vec![AllowedControlTarget {
                control: goal.target.label.clone(),
                group: goal.target.group.clone(),
                form: None,
                disambiguator: goal.target.group.clone(),
            }],
            no_unrelated_controls: true,
        });
        goal.scoped_revisit = Some(ScopedUiRevisit {
            home: TargetSpec {
                semantic_target: SemanticTargetRef::new("page-settings").unwrap(),
                label: "Settings".into(),
                group: None,
                role_hint: Some("AXRadioButton".into()),
                row: None,
            },
            away: TargetSpec {
                semantic_target: SemanticTargetRef::new("page-overview").unwrap(),
                label: "Overview".into(),
                group: None,
                role_hint: Some("AXRadioButton".into()),
                row: None,
            },
        });
        let revisit = Subgoal {
            id: "revisit".into(),
            description: "leave and return to verify the target".into(),
            skill: "ui.revisit".into(),
            target: Some(goal.target.semantic_target.clone()),
            expects: vec![],
            scroll: None,
            select: None,
        };
        assert!(scoped_revisit_step_is_allowed(&goal, &rig.pack, &revisit));

        let mut other_target = revisit.clone();
        other_target.target = Some(SemanticTargetRef::new("page-overview").unwrap());
        assert!(!scoped_revisit_step_is_allowed(
            &goal,
            &rig.pack,
            &other_target
        ));
        let mut other_skill = revisit.clone();
        other_skill.skill = "ui.activate".into();
        assert!(!scoped_revisit_step_is_allowed(
            &goal,
            &rig.pack,
            &other_skill
        ));

        let plan_targets = BTreeSet::new();
        let candidates = build(
            &Scope {
                goal: &goal,
                plan_targets: &plan_targets,
                pack: &rig.pack,
                env: &env,
            },
            &Basis::Step {
                subgoal: &revisit,
                resolution: &Resolution::Bound(Box::new(binding)),
            },
        );
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.skill == "ui.revisit")
            .unwrap();
        assert_eq!(
            candidate.semantic_target.as_ref(),
            Some(&goal.target.semantic_target),
            "the Revisit candidate stays on the primary checkbox semantic"
        );
        assert_eq!(candidate.effect_class, EffectClass::Navigation);
        assert_eq!(candidate.precondition, Precondition::Executable);
    }

    #[test]
    fn scoped_radio_choice_admission_keeps_parent_semantic_and_exact_text_plan() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let allowed_scope = AllowedControlScope {
            application: "Interaction Fixture".into(),
            targets: vec![AllowedControlTarget {
                control: "Summary grouping".into(),
                group: None,
                form: Some("Preferences".into()),
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        };
        let scoped = pack.scoped_control(&allowed_scope.targets[0]).unwrap();
        let (home, away) = pack.scoped_revisit_pages(scoped.control).unwrap();
        let mut goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        goal.execution_domain = ExecutionDomain::ForegroundNative;
        goal.scope.app = pack.bundle_id.clone();
        goal.allowed_scope = Some(allowed_scope);
        goal.target.semantic_target = SemanticTargetRef::new("form-summary-grouping-en").unwrap();
        goal.target.label = "Summary grouping".into();
        goal.target.group = Some("Preferences".into());
        goal.target.role_hint = Some("AXRadioGroup".into());
        goal.desired = ExpectedValue::Text {
            value: "By week".into(),
        };
        goal.scoped_revisit = Some(ScopedUiRevisit {
            home: TargetSpec {
                semantic_target: SemanticTargetRef::new(home.semantic.clone()).unwrap(),
                label: home.label.clone(),
                group: None,
                role_hint: Some(home.role.clone()),
                row: None,
            },
            away: TargetSpec {
                semantic_target: SemanticTargetRef::new(away.semantic.clone()).unwrap(),
                label: away.label.clone(),
                group: None,
                role_hint: Some(away.role.clone()),
                row: None,
            },
        });
        goal.required = interaction_contracts::desktop::task::Predicate::And {
            of: vec![
                interaction_contracts::desktop::task::Predicate::ContextMatches {
                    app: goal.scope.app.clone(),
                },
                interaction_contracts::desktop::task::Predicate::ReadValueEquals {
                    target: goal.target.semantic_target.clone(),
                    expected: goal.desired.clone(),
                    observed_after_action: true,
                },
                interaction_contracts::desktop::task::Predicate::Revisited {
                    target: goal.target.semantic_target.clone(),
                },
            ],
        };
        assert!(goal.validate().is_ok());
        assert_eq!(
            scoped_radio_choice_values(&goal, &pack),
            Some(&["By day", "By week", "By month"][..])
        );
        assert!(scoped_choice_values(&goal, &pack).is_none());
        let mut wrong_parent = goal.clone();
        wrong_parent.target.semantic_target = SemanticTargetRef::new("form-notes-en").unwrap();
        assert!(scoped_radio_choice_values(&wrong_parent, &pack).is_none());

        let target = goal.target.semantic_target.clone();
        let mut select = Subgoal {
            id: "select".into(),
            description: "select the requested summary group".into(),
            skill: "ui.select_by_label".into(),
            target: Some(target.clone()),
            expects: vec![],
            scroll: None,
            select: Some(
                interaction_contracts::desktop::planning::SelectByLabelSpec {
                    choice_label: "By week".into(),
                },
            ),
        };
        let revisit = Subgoal {
            id: "revisit".into(),
            description: "revisit to verify the selected radio".into(),
            skill: "ui.revisit".into(),
            target: Some(target.clone()),
            expects: vec![],
            scroll: None,
            select: None,
        };
        let plan =
            |select: Subgoal,
             revisit: Subgoal,
             dependencies: Vec<interaction_contracts::desktop::planning::Dependency>| {
                PlanBundle {
                    subgoals: vec![select, revisit],
                    dependencies,
                    required_observations: vec![],
                    expected_effects: vec![],
                    recovery_branches: vec![],
                    termination: vec![],
                }
            };
        let dependency = interaction_contracts::desktop::planning::Dependency {
            before: "select".into(),
            after: "revisit".into(),
        };
        assert!(scoped_revisit_step_is_allowed(&goal, &pack, &select));
        assert!(scoped_revisit_step_is_allowed(&goal, &pack, &revisit));
        assert!(scoped_revisit_plan_is_allowed(
            &goal,
            &pack,
            &plan(select.clone(), revisit.clone(), vec![dependency.clone()])
        ));
        let mut missing_frozen_revisit = goal.clone();
        missing_frozen_revisit.scoped_revisit = None;
        assert!(!scoped_revisit_step_is_allowed(
            &missing_frozen_revisit,
            &pack,
            &select
        ));
        assert!(!scoped_revisit_plan_is_allowed(
            &missing_frozen_revisit,
            &pack,
            &plan(select.clone(), revisit.clone(), vec![dependency.clone()])
        ));

        let read = Subgoal {
            id: "read".into(),
            description: "read the frozen parent value".into(),
            skill: "ui.read_value".into(),
            target: Some(target.clone()),
            expects: vec![],
            scroll: None,
            select: None,
        };
        let mut read_select = select.clone();
        read_select.select.as_mut().unwrap().choice_label = "By week".into();
        let read_plan = PlanBundle {
            subgoals: vec![read.clone(), read_select, revisit.clone()],
            dependencies: vec![
                interaction_contracts::desktop::planning::Dependency {
                    before: "read".into(),
                    after: "select".into(),
                },
                dependency.clone(),
            ],
            required_observations: vec![],
            expected_effects: vec![],
            recovery_branches: vec![],
            termination: vec![],
        };
        assert!(scoped_revisit_plan_is_allowed(&goal, &pack, &read_plan));
        let mut foreign_read = read_plan.clone();
        foreign_read.subgoals[0].target = Some(SemanticTargetRef::new("form-notes-en").unwrap());
        assert!(!scoped_revisit_plan_is_allowed(&goal, &pack, &foreign_read));

        select.select.as_mut().unwrap().choice_label = "By day".into();
        assert!(!scoped_revisit_step_is_allowed(&goal, &pack, &select));
        assert!(!scoped_revisit_plan_is_allowed(
            &goal,
            &pack,
            &plan(select.clone(), revisit.clone(), vec![dependency.clone()])
        ));

        let mut child_target = select.clone();
        child_target.target = Some(SemanticTargetRef::new("summary-radio-by-week").unwrap());
        assert!(!scoped_revisit_step_is_allowed(&goal, &pack, &child_target));

        let mut ensure = select.clone();
        ensure.skill = "ui.ensure_value".into();
        ensure.select = None;
        assert!(!scoped_revisit_step_is_allowed(&goal, &pack, &ensure));

        assert!(!scoped_revisit_plan_is_allowed(
            &goal,
            &pack,
            &plan(
                Subgoal {
                    select: Some(
                        interaction_contracts::desktop::planning::SelectByLabelSpec {
                            choice_label: "By week".into(),
                        },
                    ),
                    ..child_target.clone()
                },
                revisit.clone(),
                vec![dependency.clone()]
            )
        ));
        assert!(!scoped_revisit_plan_is_allowed(
            &goal,
            &pack,
            &plan(
                Subgoal {
                    select: Some(
                        interaction_contracts::desktop::planning::SelectByLabelSpec {
                            choice_label: "By week".into(),
                        },
                    ),
                    ..select.clone()
                },
                revisit,
                vec![]
            )
        ));
    }

    #[test]
    fn an_ambiguous_target_offers_each_instance_with_what_tells_them_apart() {
        let mut rig = EnvRig::new();
        let env = rig.observe(10);
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let bundle = bundle(r#"Turn off "Daily picks""#);
        let plan_targets = targets(&bundle);
        // No group given: both switches qualify equally.
        let request = BindRequest {
            request_id: RequestId::new("b").unwrap(),
            semantic_target: SemanticTargetRef::new("daily-picks").unwrap(),
            description: interaction_contracts::desktop::binding::TargetDescription {
                label: "Daily picks".into(),
                group: None,
                role_hint: Some("switch".into()),
            },
            scope: interaction_contracts::desktop::binding::BindScope {
                app: "test.settings".into(),
                account: None,
            },
            choice: None,
            max_age_ms: u64::MAX,
        };
        let outcome = rig.bindings.bind(&env, &request, 10).outcome;
        let interaction_contracts::desktop::binding::BindingOutcome::Ambiguous { candidates } =
            outcome
        else {
            panic!("two same-named switches are ambiguous: {outcome:?}");
        };
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let resolution = Resolution::Ambiguous(candidates);
        let built = build(
            &scope,
            &Basis::Step {
                subgoal: &bundle.subgoals[0],
                resolution: &resolution,
            },
        );
        let options = by_skill(&built, "ui.ensure_value");
        assert_eq!(options.len(), 2);
        let choices: BTreeSet<&str> = options
            .iter()
            .map(|c| c.parameters["choice"].as_str())
            .collect();
        assert_eq!(choices, BTreeSet::from(["sw-push-daily", "sw-email-daily"]));
        assert!(
            options.iter().all(|c| c.binding.is_none()),
            "nothing is bound yet"
        );
        assert!(options.iter().any(|c| c.description.contains("in Push")));
        assert!(options.iter().any(|c| c.description.contains("in Email")));
        assert!(
            options
                .iter()
                .all(|c| c.description.contains("currently on"))
        );

        let mut scoped_goal = goal.clone();
        scoped_goal.target.group = None;
        scoped_goal.allowed_scope = Some(AllowedControlScope {
            application: "Settings".into(),
            targets: vec![AllowedControlTarget {
                control: "Daily picks".into(),
                group: None,
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        });
        let scoped_scope = Scope {
            goal: &scoped_goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let scoped_built = build(
            &scoped_scope,
            &Basis::Step {
                subgoal: &bundle.subgoals[0],
                resolution: &resolution,
            },
        );
        assert!(by_skill(&scoped_built, "ui.ensure_value").is_empty());
        assert!(
            scoped_built
                .iter()
                .all(|candidate| !candidate.parameters.contains_key("choice"))
        );
    }

    #[test]
    fn a_target_that_is_not_there_leaves_only_the_control_paths() {
        let mut rig = EnvRig::new();
        let env = rig.observe(10);
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let bundle = bundle(TEXT);
        let plan_targets = targets(&bundle);
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let resolution = Resolution::NotFound {
            observation_complete: true,
        };
        let built = build(
            &scope,
            &Basis::Step {
                subgoal: &bundle.subgoals[0],
                resolution: &resolution,
            },
        );
        assert!(built.iter().all(|c| is_control(&c.skill)));
        assert_eq!(built.len(), 3);
    }

    #[test]
    fn unusable_evidence_asks_for_an_observation_instead_of_offering_an_action() {
        let mut rig = EnvRig::new();
        let env = rig.observe(10);
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let bundle = bundle(TEXT);
        let plan_targets = targets(&bundle);
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let resolution = Resolution::Unavailable("the evidence is too old".into());
        let built = build(
            &scope,
            &Basis::Step {
                subgoal: &bundle.subgoals[0],
                resolution: &resolution,
            },
        );
        assert_eq!(built[0].skill, "ui.ensure_value");
        assert_eq!(built[0].precondition, Precondition::NeedsObservation);
        assert!(built[0].description.contains("too old"));
    }

    #[test]
    fn a_dialog_offers_its_buttons_and_the_gate_decides_which_may_be_chosen() {
        let mut rig = EnvRig::new();
        rig.observe(10);
        rig.app.inject_modal(Modal::UnsavedChanges);
        let env = rig.observe(10);
        assert!(modal_in_front(&env));
        assert_eq!(layer_title(&env), "Unsaved changes");
        let controls = bind_layer_controls(&env, &mut rig.bindings);
        let labels: Vec<&str> = controls.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Apply", "Discard", "Cancel"]);

        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let bundle = bundle(TEXT);
        let plan_targets = targets(&bundle);
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let built = build(
            &scope,
            &Basis::Layer {
                title: "Unsaved changes",
                controls: &controls,
            },
        );
        let press = by_skill(&built, "ui.activate");
        assert_eq!(press.len(), 3);

        let apply = press
            .iter()
            .find(|c| c.parameters["label"] == "Apply")
            .unwrap();
        assert_eq!(apply.effect_class, EffectClass::Commit);
        assert_eq!(apply.precondition, Precondition::Executable);
        assert!(apply.description.contains("needs the user's confirmation"));

        let discard = press
            .iter()
            .find(|c| c.parameters["label"] == "Discard")
            .unwrap();
        assert_eq!(discard.effect_class, EffectClass::Destructive);
        assert!(
            matches!(&discard.precondition, Precondition::Rejected { reason } if reason.contains("does not name it")),
            "{:?}",
            discard.precondition
        );

        let cancel = press
            .iter()
            .find(|c| c.parameters["label"] == "Cancel")
            .unwrap();
        assert_eq!(cancel.effect_class, EffectClass::Navigation);
        assert_eq!(cancel.precondition, Precondition::Executable);
        assert!(!cancel.description.contains("confirmation"));

        let mut scoped_goal = goal.clone();
        scoped_goal.allowed_scope = Some(AllowedControlScope {
            application: "Settings".into(),
            targets: vec![AllowedControlTarget {
                control: "Daily picks".into(),
                group: Some("Push".into()),
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        });
        let scoped_scope = Scope {
            goal: &scoped_goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let scoped_built = build(
            &scoped_scope,
            &Basis::Layer {
                title: "Unsaved changes",
                controls: &controls,
            },
        );
        assert!(by_skill(&scoped_built, "ui.activate").is_empty());
        assert_eq!(scoped_built.len(), 3, "only Stop/Observe/Plan remain");
        assert_eq!(
            gate(
                &scoped_scope,
                EffectClass::Navigation,
                Some(&SemanticTargetRef::new("cancel-button").unwrap()),
                false,
            ),
            Admission::Deny(DenyReason::TargetNotCovered),
            "a modal Navigation action is outside the one-control scope"
        );
    }

    #[test]
    fn a_focused_ax_sheet_offers_native_axbutton_controls() {
        let mut rig = EnvRig::new();
        rig.observe(10);
        rig.app.inject_modal(Modal::UnsavedChanges);
        let mut env = rig.observe(10);
        assert!(modal_in_front(&env));
        let active = env
            .snapshot
            .global_focus
            .known_window_for_app(&env.snapshot.app)
            .unwrap()
            .to_owned();
        for object in &mut env.snapshot.objects {
            if object.window_id == active && object.role == "button" {
                object.role = "AXButton".into();
            }
        }

        let controls = bind_layer_controls(&env, &mut rig.bindings);
        let labels: Vec<&str> = controls
            .iter()
            .map(|control| control.label.as_str())
            .collect();
        assert_eq!(labels, ["Apply", "Discard", "Cancel"]);
        assert!(controls.iter().all(|control| control.role == "AXButton"));
    }

    #[test]
    fn a_commit_the_goal_includes_needs_no_confirmation() {
        let mut rig = EnvRig::new();
        let env = rig.observe(10);
        let committing = bundle(r#"Turn off "Daily picks" in "Push" and apply it"#);
        let plan_targets = targets(&committing);
        assert!(plan_targets.contains("apply-button"));
        let goal = goal(ConfirmationScope::Commit, EffectClass::Commit);
        let apply_object = env
            .snapshot
            .objects
            .iter()
            .find(|o| o.instance_ref.as_str() == "btn-apply")
            .unwrap();
        let target = SemanticTargetRef::new("apply-button").unwrap();
        let env_binding = rig
            .bindings
            .bind_instance(&env, target.clone(), &apply_object.instance_ref)
            .unwrap();
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let resolution = Resolution::Bound(Box::new(env_binding));
        let step = &committing.subgoals[1];
        assert_eq!(step.skill, "ui.activate");
        let built = build(
            &scope,
            &Basis::Step {
                subgoal: step,
                resolution: &resolution,
            },
        );
        assert_eq!(built[0].effect_class, EffectClass::Commit);
        assert_eq!(built[0].precondition, Precondition::Executable);
        assert!(
            !built[0].description.contains("confirmation"),
            "{}",
            built[0].description
        );
    }

    #[test]
    fn nothing_outside_the_task_scope_is_executable() {
        let mut rig = EnvRig::new();
        let mut env = rig.observe(10);
        // The same window, but the evidence says it belongs to another app.
        let active = env
            .snapshot
            .global_focus
            .known_window_for_app(&env.snapshot.app)
            .unwrap()
            .to_owned();
        env.snapshot.app.bundle_id = "some.other.app".into();
        crate::testkit::focus_snapshot_window(&mut env.snapshot, active);
        for window in &mut env.snapshot.windows {
            window.owner_bundle_id = "some.other.app".into();
        }
        let instance = env
            .snapshot
            .objects
            .iter()
            .find(|o| o.instance_ref.as_str() == "sw-push-daily")
            .unwrap()
            .instance_ref
            .clone();
        let binding = rig
            .bindings
            .bind_instance(&env, SemanticTargetRef::new(TARGET).unwrap(), &instance)
            .unwrap();
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let bundle = bundle(TEXT);
        let plan_targets = targets(&bundle);
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let resolution = Resolution::Bound(Box::new(binding));
        let built = build(
            &scope,
            &Basis::Step {
                subgoal: &bundle.subgoals[0],
                resolution: &resolution,
            },
        );
        assert!(
            matches!(&built[0].precondition, Precondition::Rejected { reason } if reason.contains("outside")),
            "{:?}",
            built[0].precondition
        );
    }

    #[test]
    fn a_control_whose_effect_is_unknown_is_never_offered_as_executable() {
        let mut rig = EnvRig::new();
        let env = rig.observe(10);
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let bundle = bundle(TEXT);
        let plan_targets = targets(&bundle);
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        // An `ui.activate` step aimed at a role the pack has no class for.
        let mystery = UiObject {
            role: "mystery".into(),
            ..env
                .snapshot
                .objects
                .iter()
                .find(|o| o.role == "button")
                .unwrap()
                .clone()
        };
        let subgoal = Subgoal {
            select: None,
            id: "x".into(),
            description: "press it".into(),
            skill: "ui.activate".into(),
            target: Some(SemanticTargetRef::new("mystery").unwrap()),
            expects: vec![],
            scroll: None,
        };
        assert_eq!(
            step_class(&scope, "ui.activate", Some(&mystery)),
            EffectClass::Unknown
        );
        let draft = step_draft(&scope, &subgoal, None, &mystery);
        assert_eq!(draft.class, EffectClass::Unknown);
        assert!(matches!(
            gate(&scope, draft.class, draft.target.as_ref(), false),
            Admission::Deny(DenyReason::UnknownEffect)
        ));
    }
}
