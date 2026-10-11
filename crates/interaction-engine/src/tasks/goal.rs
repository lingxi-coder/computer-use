//! GoalCompiler and PlanInstaller: pure checks that turn a planner's draft
//! into a goal the user can confirm, and refuse plans that reach beyond it
//! (design 12.5 and 13.3).

use interaction_contracts::desktop::binding::normalize_label;
use interaction_contracts::desktop::browser::BrowserPageProfile;
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{ConfirmationId, RootTaskId, SemanticTargetRef};
use interaction_contracts::desktop::operator::{
    ConfirmationKind, ConfirmationRequest, GoalSummary,
};
use interaction_contracts::desktop::planning::{GoalDraft, PlanBundle, ScrollDirection};
use interaction_contracts::desktop::task::{
    AllowedControlScope, ConfirmationScope, ExpectedValue, GoalSpec, MAX_SCOPED_FORM_SAFE_INTEGER,
    Predicate, RootBudgetSpec, ScopedCommitKind, ScopedUiRevisit, TargetSpec, TaskScope,
};

use crate::environment::pack::{AppPack, ScopedControlContextKind, ScopedControlValueKind};

// Native AX SetValue and registered-editor Source reads share this bound.
const MAX_SCOPED_NATIVE_TEXT_BYTES: usize = 2_048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRejection {
    Structure(String),
    UnregisteredSkill(String),
    /// A step would have an effect the frozen goal does not include.
    ExceedsGoal {
        skill: String,
        class: EffectClass,
    },
    /// No trusted source could classify the step's effect.
    UnknownEffect {
        skill: String,
    },
}

impl std::fmt::Display for PlanRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Structure(reason) => write!(f, "plan structure: {reason}"),
            Self::UnregisteredSkill(skill) => write!(f, "skill {skill} is not registered"),
            Self::ExceedsGoal { skill, class } => {
                write!(
                    f,
                    "step {skill} would have a {class:?} effect beyond the goal"
                )
            }
            Self::UnknownEffect { skill } => write!(f, "step {skill} has an unknown effect"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    NoGoalDraft,
    AppNotExecutable(String),
    InvalidGoal(String),
    Plan(PlanRejection),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGoalDraft => f.write_str("the planner did not return a goal"),
            Self::AppNotExecutable(app) => write!(
                f,
                "{app} is not in the execution whitelist; it can only be observed"
            ),
            Self::InvalidGoal(reason) => write!(f, "the goal is invalid: {reason}"),
            Self::Plan(rejection) => write!(f, "{rejection}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Trusted Host metadata used while compiling a planner proposal. The model
/// never constructs this value or supplies its authorization fields.
pub(crate) struct HostGoalCompileContext<'a> {
    execution_domain: ExecutionDomain,
    browser_page_profile: Option<BrowserPageProfile>,
    allowed_scope: Option<&'a AllowedControlScope>,
    allowed_scope_app: Option<&'a str>,
}

impl<'a> HostGoalCompileContext<'a> {
    pub(crate) fn free_text(
        execution_domain: ExecutionDomain,
        browser_page_profile: Option<BrowserPageProfile>,
    ) -> Self {
        Self {
            execution_domain,
            browser_page_profile,
            allowed_scope: None,
            allowed_scope_app: None,
        }
    }

    pub(crate) fn scoped(
        execution_domain: ExecutionDomain,
        browser_page_profile: Option<BrowserPageProfile>,
        allowed_scope: &'a AllowedControlScope,
        allowed_scope_app: &'a str,
    ) -> Self {
        Self {
            execution_domain,
            browser_page_profile,
            allowed_scope: Some(allowed_scope),
            allowed_scope_app: Some(allowed_scope_app),
        }
    }
}

/// The effect class a skill has by itself, before the control it acts on is known.
pub fn skill_class(skill: &str) -> Option<EffectClass> {
    match skill {
        "ui.read_value" => Some(EffectClass::ReadOnly),
        "ui.scroll_to" | "ui.activate_window" | "ui.revisit" => Some(EffectClass::Navigation),
        "ui.ensure_value" | "ui.enter_text" | "ui.save_document" | "ui.select_by_label" => {
            Some(EffectClass::ReversibleEdit)
        }
        "ui.select_row" => Some(EffectClass::Navigation),
        // The class of an activation depends on the control; the pack decides.
        "ui.activate" => Some(EffectClass::ReadOnly),
        _ => None,
    }
}

/// Check a plan against the frozen-to-be goal. A plan only refines the goal:
/// it cannot add skills that are not registered or effects the goal excludes.
pub fn validate_plan(
    goal: &GoalSpec,
    bundle: &PlanBundle,
    pack: &AppPack,
    registered: &[String],
) -> Result<(), PlanRejection> {
    bundle.validate().map_err(PlanRejection::Structure)?;
    let mut validated_radio_choice = false;
    if goal.allowed_scope.is_some() {
        let target = &goal.target.semantic_target;
        let choice_values = scoped_choice_values(goal, pack);
        let number_scroll_container = scoped_number_form_scroll_container(goal, pack);
        let commit_target = goal
            .scoped_commit
            .as_ref()
            .and_then(|commit| commit.action.as_ref())
            .map(|action| &action.semantic_target);
        for subgoal in &bundle.subgoals {
            let primary_read_or_ensure = subgoal.target.as_ref() == Some(target)
                && (subgoal.skill == "ui.read_value"
                    || (choice_values.is_none() && subgoal.skill == "ui.ensure_value"));
            let primary_choice = choice_values.is_some()
                && subgoal.skill == "ui.select_by_label"
                && subgoal.target.as_ref() == Some(target);
            let primary_revisit = goal.scoped_revisit.is_some()
                && subgoal.skill == "ui.revisit"
                && subgoal.target.as_ref() == Some(target);
            let scoped_save = commit_target.is_some_and(|commit_target| {
                subgoal.skill == "ui.activate" && subgoal.target.as_ref() == Some(commit_target)
            });
            let scoped_number_scroll = number_scroll_container.is_some_and(|container| {
                scoped_number_scroll_step_matches(goal, container, subgoal)
            });
            if !(primary_read_or_ensure
                || primary_choice
                || primary_revisit
                || scoped_save
                || scoped_number_scroll)
                || (subgoal.scroll.is_some() && !scoped_number_scroll)
                || (subgoal.skill == "ui.scroll_to" && !scoped_number_scroll)
                || (subgoal.select.is_some() && !primary_choice)
            {
                return Err(PlanRejection::Structure(
                    "scoped plans may only use the frozen control, exact Host-authorized Save, and revisit".into(),
                ));
            }
        }
        if choice_values.is_some()
            || (goal.scoped_revisit.is_some() && number_scroll_container.is_none())
        {
            validate_scoped_revisit_plan(goal, bundle, pack)?;
            // Radio selection is compiled through the exact direct-Press path
            // only after its full frozen parent/choice/revisit plan validates.
            validated_radio_choice = scoped_radio_choice_values(goal, pack).is_some();
        }
        if let Some(container) = number_scroll_container {
            validate_scoped_number_scroll_plan(goal, bundle, container)?;
        }
        if commit_target.is_some() {
            validate_scoped_commit_plan(goal, bundle)?;
        }
    }
    if goal.execution_domain == ExecutionDomain::HeadlessBrowser {
        let target = &goal.target.semantic_target;
        if goal.browser_page_profile.is_none()
            || goal.target.row.is_some()
            || goal.target.role_hint.as_deref() != Some("checkbox")
            || !matches!(&goal.desired, ExpectedValue::Bool { .. })
            || goal.confirmation_scope != ConfirmationScope::CurrentUi
            || bundle.subgoals.len() != 1
            || bundle.subgoals[0].skill != "ui.ensure_value"
            || bundle.subgoals[0].target.as_ref() != Some(target)
            || bundle.subgoals[0].scroll.is_some()
            || bundle.subgoals[0].select.is_some()
            || !bundle.dependencies.is_empty()
        {
            return Err(PlanRejection::Structure(
                "headless browser plans allow one fixed checkbox step: ui.ensure_value".into(),
            ));
        }
    }
    if goal.execution_domain == ExecutionDomain::BackgroundAx {
        let target = &goal.target.semantic_target;
        let allowed_target_role = goal.target.role_hint.as_deref().is_some_and(|role| {
            matches!(
                role,
                "AXButton" | "AXCheckBox" | "AXRadioButton" | "AXSwitch"
            )
        });
        if goal.target.row.is_some()
            || !allowed_target_role
            || bundle.subgoals.len() != 1
            || bundle.subgoals[0].target.as_ref() != Some(target)
            || !matches!(
                bundle.subgoals[0].skill.as_str(),
                "ui.activate" | "ui.ensure_value"
            )
            || bundle.subgoals[0].scroll.is_some()
            || bundle.subgoals[0].select.is_some()
            || !bundle.dependencies.is_empty()
        {
            return Err(PlanRejection::Structure(
                "background AX plans allow one frozen form target step: ui.activate or ui.ensure_value".into(),
            ));
        }
        if pack.effect_class(
            goal.target.role_hint.as_deref().unwrap_or_default(),
            &goal.target.label,
        ) > goal.max_effect_class
        {
            return Err(PlanRejection::UnknownEffect {
                skill: bundle.subgoals[0].skill.clone(),
            });
        }
    }
    if !frozen_row_goal_is_well_formed(goal) {
        return Err(PlanRejection::Structure(
            "row predicate must be the unique selected=true condition for its frozen target".into(),
        ));
    }
    validate_row_plan_order(goal, bundle, pack)?;
    if goal.target.row.as_ref().is_some_and(|row| {
        pack.row_selection_effect_class(
            &goal.target.semantic_target,
            row,
            goal.target.label.as_str(),
        )
        .is_none()
    }) {
        return Err(PlanRejection::UnknownEffect {
            skill: "ui.select_row".into(),
        });
    }
    for subgoal in &bundle.subgoals {
        if !registered.contains(&subgoal.skill) {
            return Err(PlanRejection::UnregisteredSkill(subgoal.skill.clone()));
        }
        if let Some(spec) = &subgoal.scroll {
            spec.validate().map_err(PlanRejection::Structure)?;
            let target = subgoal
                .target
                .as_ref()
                .expect("structure validation requires destination");
            if target != &goal.target.semantic_target && pack.control(target).is_none() {
                return Err(PlanRejection::Structure(
                    "scroll destination is neither frozen goal nor registered control".into(),
                ));
            }
        }
        if let Some(spec) = &subgoal.select {
            spec.validate().map_err(PlanRejection::Structure)?;
            let target = subgoal.target.as_ref().expect("validated menu target");
            let (role, label) = if target == &goal.target.semantic_target {
                if !matches!(&goal.desired, interaction_contracts::desktop::task::ExpectedValue::Text { value }
                    if value == &spec.choice_label)
                {
                    return Err(PlanRejection::Structure(
                        "menu goal selection must retain the frozen desired text".into(),
                    ));
                }
                (
                    goal.target.role_hint.as_deref().unwrap_or(""),
                    goal.target.label.as_str(),
                )
            } else if let Some(control) = pack.control(target) {
                (control.role.as_str(), control.label.as_str())
            } else {
                return Err(PlanRejection::Structure(
                    "menu opener is neither frozen goal nor registered control".into(),
                ));
            };
            let exact_radio_choice =
                validated_radio_choice && target == &goal.target.semantic_target;
            if !exact_radio_choice
                && (!["AXMenuButton", "AXPopUpButton"].contains(&role)
                    || pack.effect_class(role, label) > EffectClass::ReversibleEdit
                    || pack.effect_class("AXMenuItem", &spec.choice_label)
                        > EffectClass::ReversibleEdit)
            {
                return Err(PlanRejection::Structure(
                    "menu selection requires registered reversible opener and choice semantics"
                        .into(),
                ));
            }
        }
        if subgoal.skill == "ui.select_row" {
            let target = subgoal.target.as_ref().expect("validated row target");
            if target != &goal.target.semantic_target
                || subgoal.select.is_some()
                || subgoal.scroll.is_some()
            {
                return Err(PlanRejection::Structure(
                    "row selection must reference only the frozen Goal semantic target".into(),
                ));
            }
            let Some(row) = goal.target.row.as_ref() else {
                return Err(PlanRejection::UnknownEffect {
                    skill: subgoal.skill.clone(),
                });
            };
            if goal.confirmation_scope != ConfirmationScope::UiRevisit
                || !matches!(&goal.desired, ExpectedValue::Bool { value: true })
                || pack
                    .row_selection_effect_class(target, row, goal.target.label.as_str())
                    .is_none()
            {
                return Err(PlanRejection::UnknownEffect {
                    skill: subgoal.skill.clone(),
                });
            }
        } else if goal.target.row.is_some()
            && subgoal.target.as_ref() == Some(&goal.target.semantic_target)
        {
            let safe_read_or_revisit =
                matches!(subgoal.skill.as_str(), "ui.read_value" | "ui.revisit");
            // Row scroll scope was already checked with the frozen target in
            // validate_row_plan_order; this branch only classifies the skill.
            let scoped_scroll = subgoal.skill == "ui.scroll_to";
            if !safe_read_or_revisit && !scoped_scroll {
                return Err(PlanRejection::UnknownEffect {
                    skill: subgoal.skill.clone(),
                });
            }
        }
        let base = skill_class(&subgoal.skill)
            .ok_or_else(|| PlanRejection::UnregisteredSkill(subgoal.skill.clone()))?;
        // An activation is only as harmless as the control it presses.
        let class = if subgoal.skill == "ui.select_row" {
            let target = subgoal.target.as_ref().expect("validated row target");
            let row = goal.target.row.as_ref().expect("checked row goal");
            pack.row_selection_effect_class(target, row, goal.target.label.as_str())
                .unwrap_or(EffectClass::Unknown)
        } else if subgoal.skill == "ui.activate" {
            match subgoal
                .target
                .as_ref()
                .and_then(|target| pack.control(target))
            {
                Some(control) => pack.effect_class(&control.role, &control.label),
                None => {
                    let label = subgoal
                        .target
                        .as_ref()
                        .map(|target| target.as_str().replace(['-', '_', '/'], " "))
                        .unwrap_or_default();
                    pack.effect_class_for_label(&label)
                }
            }
        } else {
            base
        };
        if class == EffectClass::Unknown {
            return Err(PlanRejection::UnknownEffect {
                skill: subgoal.skill.clone(),
            });
        }
        let exact_scoped_commit = goal
            .scoped_commit
            .as_ref()
            .and_then(|commit| commit.action.as_ref())
            .is_some_and(|action| {
                subgoal.skill == "ui.activate"
                    && subgoal.target.as_ref() == Some(&action.semantic_target)
                    && class == EffectClass::Commit
            });
        if class > goal.max_effect_class && !exact_scoped_commit {
            return Err(PlanRejection::ExceedsGoal {
                skill: subgoal.skill.clone(),
                class,
            });
        }
    }
    Ok(())
}

/// A foreground scoped goal needs the model's plan to place verification
/// navigation after its only target mutation/read. The exact page endpoints
/// are already Host-frozen in `GoalSpec`; this accepts only the semantic
/// `ui.revisit` step and never lets the plan name either navigation control.
fn validate_scoped_revisit_plan(
    goal: &GoalSpec,
    bundle: &PlanBundle,
    pack: &AppPack,
) -> Result<(), PlanRejection> {
    if let Some(choices) = scoped_choice_values(goal, pack) {
        return validate_scoped_choice_plan(goal, bundle, pack, choices);
    }
    let target = &goal.target.semantic_target;
    let ensures: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.ensure_value" && step.target.as_ref() == Some(target))
        .collect();
    let revisits: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.revisit")
        .collect();
    let [ensure] = ensures.as_slice() else {
        return Err(PlanRejection::Structure(
            "scoped foreground plans require exactly one target ensure step before revisit".into(),
        ));
    };
    let [revisit] = revisits.as_slice() else {
        return Err(PlanRejection::Structure(
            "scoped foreground plans require exactly one Host-authorized revisit".into(),
        ));
    };
    for step in bundle.subgoals.iter().filter(|step| step.id != revisit.id) {
        if !dependency_path_exists(bundle, &step.id, &revisit.id) {
            return Err(PlanRejection::Structure(
                "all scoped target reads and changes must precede the final revisit".into(),
            ));
        }
    }
    if !dependency_path_exists(bundle, &ensure.id, &revisit.id) {
        return Err(PlanRejection::Structure(
            "the scoped revisit must depend on the target ensure step".into(),
        ));
    }
    Ok(())
}

/// Resolve choices only from the exact Host-approved scoped form registration.
/// The allowed values remain in AppPack and are never copied into GoalSpec.
fn scoped_choice_values(goal: &GoalSpec, pack: &AppPack) -> Option<&'static [&'static str]> {
    let allowed = goal.allowed_scope.as_ref()?;
    let [target] = allowed.targets.as_slice() else {
        return None;
    };
    let scoped = pack.scoped_control(target)?;
    match scoped.value_kind {
        ScopedControlValueKind::Choice(choices) | ScopedControlValueKind::RadioChoice(choices) => {
            Some(choices)
        }
        ScopedControlValueKind::Bool
        | ScopedControlValueKind::Text
        | ScopedControlValueKind::Number => None,
    }
}

fn scoped_radio_choice_values(goal: &GoalSpec, pack: &AppPack) -> Option<&'static [&'static str]> {
    let allowed = goal.allowed_scope.as_ref()?;
    let [target] = allowed.targets.as_slice() else {
        return None;
    };
    match pack.scoped_control(target)?.value_kind {
        ScopedControlValueKind::RadioChoice(choices) => Some(choices),
        ScopedControlValueKind::Bool
        | ScopedControlValueKind::Text
        | ScopedControlValueKind::Number
        | ScopedControlValueKind::Choice(_) => None,
    }
}

pub(crate) fn scoped_number_form_scroll_container<'a>(
    goal: &GoalSpec,
    pack: &'a AppPack,
) -> Option<&'a crate::environment::pack::ControlDef> {
    goal.validate().ok()?;
    let allowed = goal.allowed_scope.as_ref()?;
    let [scope_target] = allowed.targets.as_slice() else {
        return None;
    };
    let scoped = pack.scoped_control(scope_target)?;
    let ExpectedValue::Number { value } = &goal.desired else {
        return None;
    };
    if !(-MAX_SCOPED_FORM_SAFE_INTEGER..=MAX_SCOPED_FORM_SAFE_INTEGER).contains(value)
        || goal.execution_domain != ExecutionDomain::ForegroundNative
        || goal.confirmation_scope != ConfirmationScope::UiRevisit
        || goal.scoped_commit.is_some()
        || goal.scoped_revisit.is_none()
        || goal.scope.app != pack.bundle_id
        || goal.scope.account.is_some()
        || goal.target.row.is_some()
        || scoped.value_kind != ScopedControlValueKind::Number
        || scoped.context_kind != ScopedControlContextKind::Form
        || scoped.control.role != "AXTextField"
        || goal.target.semantic_target.as_str() != scoped.control.semantic
        || goal.target.label != scoped.control.label
        || goal.target.group.as_deref() != scoped.context_label
        || scope_target.context_label() != scoped.context_label
        || goal.target.role_hint.as_deref() != Some("AXTextField")
    {
        return None;
    }
    pack.scoped_form_scroll_container(scope_target)
}

pub(crate) fn scoped_number_scroll_step_matches(
    goal: &GoalSpec,
    container: &crate::environment::pack::ControlDef,
    subgoal: &interaction_contracts::desktop::planning::Subgoal,
) -> bool {
    if subgoal.skill != "ui.scroll_to"
        || subgoal.target.as_ref() != Some(&goal.target.semantic_target)
        || subgoal.select.is_some()
    {
        return false;
    }
    let Some(spec) = subgoal.scroll.as_ref() else {
        return false;
    };
    spec.direction == ScrollDirection::Down
        && spec.container.semantic_target.as_str() == container.semantic
        && spec.container.label == container.label
        && spec.container.group.is_none()
        && spec.container.role_hint.as_deref() == Some("AXScrollArea")
        && spec.container.row.is_none()
}

fn validate_scoped_number_scroll_plan(
    goal: &GoalSpec,
    bundle: &PlanBundle,
    container: &crate::environment::pack::ControlDef,
) -> Result<(), PlanRejection> {
    let reject = |reason: &str| PlanRejection::Structure(reason.into());
    let target = &goal.target.semantic_target;
    let scrolls: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.scroll_to")
        .collect();
    let ensures: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.ensure_value" && step.target.as_ref() == Some(target))
        .collect();
    let revisits: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.revisit")
        .collect();
    let reads: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.read_value")
        .collect();
    let ([ensure], [revisit], [_, _]) =
        (ensures.as_slice(), revisits.as_slice(), scrolls.as_slice())
    else {
        return Err(reject(
            "numeric scoped plans require two registered scrolls around the target ensure and revisit",
        ));
    };

    let scrolls_before_ensure: Vec<_> = scrolls
        .iter()
        .copied()
        .filter(|scroll| dependency_path_exists(bundle, &scroll.id, &ensure.id))
        .collect();
    let scrolls_after_revisit: Vec<_> = scrolls
        .iter()
        .copied()
        .filter(|scroll| dependency_path_exists(bundle, &revisit.id, &scroll.id))
        .collect();
    let ([scroll_before], [scroll_after]) = (
        scrolls_before_ensure.as_slice(),
        scrolls_after_revisit.as_slice(),
    ) else {
        return Err(reject(
            "numeric scoped plan requires one scroll before ensure and one after revisit",
        ));
    };

    if bundle.subgoals.len() != 4 + reads.len()
        || reads.len() > 1
        || !bundle.recovery_branches.is_empty()
        || !scoped_number_scroll_step_matches(goal, container, scroll_before)
        || !scoped_number_scroll_step_matches(goal, container, scroll_after)
        || scroll_before.id == scroll_after.id
        || ensure.scroll.is_some()
        || ensure.select.is_some()
        || revisit.target.as_ref() != Some(target)
        || revisit.scroll.is_some()
        || revisit.select.is_some()
        || !dependency_path_exists(bundle, &ensure.id, &revisit.id)
        || reads.first().is_some_and(|read| {
            read.target.as_ref() != Some(target)
                || read.scroll.is_some()
                || read.select.is_some()
                || !dependency_path_exists(bundle, &scroll_after.id, &read.id)
        })
    {
        return Err(reject(
            "numeric scoped plan must scroll into view, ensure, revisit, and restore view through the exact registered container",
        ));
    }
    Ok(())
}

fn validate_scoped_choice_plan(
    goal: &GoalSpec,
    bundle: &PlanBundle,
    pack: &AppPack,
    choices: &[&str],
) -> Result<(), PlanRejection> {
    let reject = |reason: &str| PlanRejection::Structure(reason.into());
    let Some(allowed) = goal.allowed_scope.as_ref() else {
        return Err(reject("choice requires one frozen allowed control"));
    };
    let [allowed_target] = allowed.targets.as_slice() else {
        return Err(reject("choice requires one frozen allowed control"));
    };
    let Some(scoped) = pack.scoped_control(allowed_target) else {
        return Err(reject("choice is not a registered scoped control"));
    };
    let target = &goal.target.semantic_target;
    let expected_role = match scoped.value_kind {
        ScopedControlValueKind::Choice(registered) if registered == choices => "AXPopUpButton",
        ScopedControlValueKind::RadioChoice(registered) if registered == choices => "AXRadioGroup",
        _ => {
            return Err(reject(
                "choice values do not match the exact Host registration",
            ));
        }
    };
    let Some((home, away)) = pack.scoped_revisit_pages(scoped.control) else {
        return Err(reject("choice has no Host-registered revisit pair"));
    };
    let expected_text = match &goal.desired {
        ExpectedValue::Text { value } if choices.contains(&value.as_str()) => value,
        _ => return Err(reject("desired text is not a Host-registered choice")),
    };
    let Some(revisit_pair) = goal.scoped_revisit.as_ref() else {
        return Err(reject("choice requires frozen revisit endpoints"));
    };
    let endpoint_matches =
        |target_spec: &TargetSpec, control: &crate::environment::pack::ControlDef| {
            target_spec.semantic_target.as_str() == control.semantic
                && target_spec.label == control.label
                && target_spec.group.is_none()
                && target_spec.role_hint.as_deref() == Some(control.role.as_str())
                && target_spec.row.is_none()
        };
    if goal.validate().is_err()
        || goal.execution_domain != ExecutionDomain::ForegroundNative
        || goal.confirmation_scope != ConfirmationScope::UiRevisit
        || goal.scoped_commit.is_some()
        || scoped.context_kind != ScopedControlContextKind::Form
        || scoped.control.role != expected_role
        || goal.target.role_hint.as_deref() != Some(expected_role)
        || goal.target.semantic_target.as_str() != scoped.control.semantic
        || goal.target.label != scoped.control.label
        || goal.target.group.as_deref() != scoped.context_label
        || allowed_target.form.as_deref() != scoped.context_label
        || !endpoint_matches(&revisit_pair.home, home)
        || !endpoint_matches(&revisit_pair.away, away)
    {
        return Err(reject("choice does not match its exact Host registration"));
    }

    let selects: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.select_by_label")
        .collect();
    let revisits: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.revisit")
        .collect();
    let [select] = selects.as_slice() else {
        return Err(reject(
            "scoped choice plan requires exactly one ui.select_by_label step",
        ));
    };
    let [revisit] = revisits.as_slice() else {
        return Err(reject(
            "scoped choice plan requires exactly one frozen ui.revisit step",
        ));
    };
    if select.target.as_ref() != Some(target)
        || select.scroll.is_some()
        || !select.select.as_ref().is_some_and(|spec| {
            spec.choice_label == expected_text.as_str() && spec.validate().is_ok()
        })
        || revisit.target.as_ref() != Some(target)
        || revisit.scroll.is_some()
        || revisit.select.is_some()
        || bundle.subgoals.iter().any(|step| {
            let primary_read =
                step.skill == "ui.read_value" && step.target.as_ref() == Some(target);
            step.id != select.id && step.id != revisit.id && !primary_read
        })
        || !dependency_path_exists(bundle, &select.id, &revisit.id)
        || bundle.subgoals.iter().any(|step| {
            step.id != revisit.id && !dependency_path_exists(bundle, &step.id, &revisit.id)
        })
        || !bundle.recovery_branches.is_empty()
    {
        return Err(reject(
            "scoped choice plan must select the frozen option and depend on its exact revisit",
        ));
    }
    Ok(())
}

/// A manual scoped save is one exact, final Commit activation after the
/// primary Boolean target has been ensured. The user confirms this exact scope
/// once with the Goal, so the Host may include Commit for that one action.
fn validate_scoped_commit_plan(goal: &GoalSpec, bundle: &PlanBundle) -> Result<(), PlanRejection> {
    let Some(action) = goal
        .scoped_commit
        .as_ref()
        .and_then(|commit| commit.action.as_ref())
    else {
        return Ok(());
    };
    if goal.confirmation_scope != ConfirmationScope::Commit || goal.scoped_revisit.is_some() {
        return Err(PlanRejection::Structure(
            "manual scoped commit requires Commit confirmation without an extra revisit".into(),
        ));
    }
    let target = &goal.target.semantic_target;
    let ensures: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| step.skill == "ui.ensure_value" && step.target.as_ref() == Some(target))
        .collect();
    let saves: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|step| {
            step.skill == "ui.activate" && step.target.as_ref() == Some(&action.semantic_target)
        })
        .collect();
    let ([ensure], [save]) = (ensures.as_slice(), saves.as_slice()) else {
        return Err(PlanRejection::Structure(
            "manual scoped commit requires one target ensure and one exact Save action".into(),
        ));
    };
    for step in &bundle.subgoals {
        let allowed = (step.skill == "ui.ensure_value" || step.skill == "ui.read_value")
            && step.target.as_ref() == Some(target)
            || step.id == save.id;
        if !allowed || step.skill == "ui.revisit" {
            return Err(PlanRejection::Structure(
                "manual scoped commit allows only the target read/ensure and exact Save action"
                    .into(),
            ));
        }
        if step.id != save.id && !dependency_path_exists(bundle, &step.id, &save.id) {
            return Err(PlanRejection::Structure(
                "the exact Save action must follow every scoped target read or change".into(),
            ));
        }
    }
    if !dependency_path_exists(bundle, &ensure.id, &save.id) {
        return Err(PlanRejection::Structure(
            "the exact Save action must depend on the target ensure step".into(),
        ));
    }
    Ok(())
}

/// A row goal selects only through `ui.select_row`; bounded, scoped
/// `ui.scroll_to` steps may prepare the frozen row, but must precede selection.
/// Its frozen Root predicate also requires a revisit after selection.
fn validate_row_plan_order(
    goal: &GoalSpec,
    bundle: &PlanBundle,
    pack: &AppPack,
) -> Result<(), PlanRejection> {
    if goal.target.row.is_none() {
        return Ok(());
    }

    let target = &goal.target.semantic_target;
    let row_selects: Vec<_> = bundle
        .subgoals
        .iter()
        .filter(|subgoal| subgoal.skill == "ui.select_row")
        .collect();
    let [select] = row_selects.as_slice() else {
        return Err(PlanRejection::Structure(
            "row plans require exactly one ui.select_row for the frozen Goal target".into(),
        ));
    };
    if select.target.as_ref() != Some(target) {
        return Err(PlanRejection::Structure(
            "row selection must reference only the frozen Goal semantic target".into(),
        ));
    }

    for scroll in bundle
        .subgoals
        .iter()
        .filter(|subgoal| subgoal.skill == "ui.scroll_to")
    {
        if scroll.target.as_ref() != Some(target) {
            return Err(PlanRejection::Structure(
                "row-plan scrolling must explicitly target the frozen Goal semantic target".into(),
            ));
        }
        let Some(spec) = scroll.scroll.as_ref() else {
            return Err(PlanRejection::Structure(
                "row-plan scrolling requires its frozen table scope".into(),
            ));
        };
        if !registered_row_scroll_destination(goal, spec, pack) {
            return Err(PlanRejection::Structure(
                "row scrolling must use the frozen table's registered nearest scroll container"
                    .into(),
            ));
        }
        if !dependency_path_exists(bundle, &scroll.id, &select.id) {
            return Err(PlanRejection::Structure(
                "every row-plan scroll must precede the frozen row selection in the dependency graph"
                    .into(),
            ));
        }
    }

    let mut target_revisits = 0usize;
    for revisit in bundle
        .subgoals
        .iter()
        .filter(|subgoal| subgoal.skill == "ui.revisit")
    {
        if revisit.target.is_none() {
            return Err(PlanRejection::Structure(
                "row revisit must explicitly reference the frozen Goal semantic target".into(),
            ));
        }
        if revisit.target.as_ref() != Some(target) {
            continue;
        }
        target_revisits += 1;
        if !dependency_path_exists(bundle, &select.id, &revisit.id) {
            return Err(PlanRejection::Structure(
                "every row revisit must depend on the frozen row selection".into(),
            ));
        }
    }
    if target_revisits == 0 {
        return Err(PlanRejection::Structure(
            "row plans require ui.revisit for the frozen Goal target".into(),
        ));
    }
    Ok(())
}

/// Dependency edges point from prerequisite (`before`) to dependent
/// (`after`). Walk backwards from the dependent so transitive prerequisites
/// are accepted without trusting subgoal vector order.
fn dependency_path_exists(bundle: &PlanBundle, prerequisite: &str, dependent: &str) -> bool {
    let mut pending = vec![dependent.to_owned()];
    let mut visited = std::collections::BTreeSet::new();
    while let Some(current) = pending.pop() {
        if current == prerequisite {
            return true;
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        pending.extend(
            bundle
                .dependencies
                .iter()
                .filter(|edge| edge.after == current)
                .map(|edge| edge.before.clone()),
        );
    }
    false
}

fn registered_row_scroll_destination(
    goal: &GoalSpec,
    spec: &interaction_contracts::desktop::planning::ScrollToSpec,
    pack: &AppPack,
) -> bool {
    let Some(row) = goal.target.row.as_ref() else {
        return false;
    };
    let Some(nearest) = row.table_scope.scroll_path.last() else {
        return false;
    };
    let container = &spec.container;
    container.role_hint.as_deref() == Some(nearest.role.ax_role())
        && container.label == nearest.label
        && pack
            .control(&container.semantic_target)
            .is_some_and(|control| {
                control.role == nearest.role.ax_role() && control.label == nearest.label
            })
}

fn frozen_row_goal_is_well_formed(goal: &GoalSpec) -> bool {
    fn inspect(
        predicate: &Predicate,
        target: &SemanticTargetRef,
        row_selected_count: &mut usize,
        invalid: &mut bool,
    ) {
        match predicate {
            Predicate::RowSelected {
                target: predicate_target,
                observed_after_action,
            } => {
                *row_selected_count += 1;
                *invalid |= predicate_target != target || !observed_after_action;
            }
            Predicate::And { of } | Predicate::Or { of } => {
                for child in of {
                    inspect(child, target, row_selected_count, invalid);
                }
            }
            Predicate::Not { of } => inspect(of, target, row_selected_count, invalid),
            _ => {}
        }
    }

    let mut count = 0;
    let mut invalid = false;
    inspect(
        &goal.required,
        &goal.target.semantic_target,
        &mut count,
        &mut invalid,
    );
    if let Some(row) = &goal.target.row {
        let target = &goal.target.semantic_target;
        goal.target.validate().is_ok()
            && row.validate().is_ok()
            && matches!(&goal.desired, ExpectedValue::Bool { value: true })
            && goal.confirmation_scope == ConfirmationScope::UiRevisit
            && goal.required
                == (Predicate::And {
                    of: vec![
                        Predicate::ContextMatches {
                            app: goal.scope.app.clone(),
                        },
                        Predicate::RowSelected {
                            target: target.clone(),
                            observed_after_action: true,
                        },
                        Predicate::Revisited {
                            target: target.clone(),
                        },
                    ],
                })
    } else {
        count == 0 && !invalid
    }
}

/// Build the goal from a planner's draft. `budget.deadline_ms` of the result
/// is a duration; it becomes an absolute time when the user confirms.
pub fn compile_goal(
    root: &RootTaskId,
    user_text: &str,
    draft: &GoalDraft,
    execution_domain: ExecutionDomain,
    default_budget: RootBudgetSpec,
    packs: &[AppPack],
) -> Result<(GoalSpec, AppPack), CompileError> {
    compile_goal_with_profile(
        root,
        user_text,
        draft,
        execution_domain,
        None,
        default_budget,
        packs,
    )
}

pub fn compile_goal_with_profile(
    root: &RootTaskId,
    user_text: &str,
    draft: &GoalDraft,
    execution_domain: ExecutionDomain,
    browser_page_profile: Option<BrowserPageProfile>,
    default_budget: RootBudgetSpec,
    packs: &[AppPack],
) -> Result<(GoalSpec, AppPack), CompileError> {
    compile_goal_with_context(
        root,
        user_text,
        draft,
        HostGoalCompileContext::free_text(execution_domain, browser_page_profile),
        default_budget,
        packs,
    )
}

/// Resolve the user-facing app name (or exact bundle id) to one trusted pack
/// and verify that the requested control is statically registered.
pub fn resolve_allowed_scope_app(
    allowed_scope: &AllowedControlScope,
    packs: &[AppPack],
    browser_page_profile: Option<BrowserPageProfile>,
) -> Result<String, CompileError> {
    allowed_scope
        .validate()
        .map_err(CompileError::InvalidGoal)?;
    let application = allowed_scope.application.as_str();
    let browser_pack = AppPack::browser_fixture();
    let mut matching_packs: Vec<AppPack> = packs
        .iter()
        .filter(|pack| {
            pack.bundle_id != browser_pack.bundle_id
                && (pack.name == application || pack.bundle_id == application)
        })
        .cloned()
        .collect();
    if browser_page_profile.is_some()
        && (browser_pack.name == application || browser_pack.bundle_id == application)
    {
        matching_packs.push(browser_pack);
    }
    let [pack] = matching_packs.as_slice() else {
        return Err(CompileError::InvalidGoal(
            "allowed application must resolve to exactly one trusted app pack".into(),
        ));
    };
    let target = &allowed_scope.targets[0];
    if pack.scoped_control(target).is_none() {
        return Err(CompileError::InvalidGoal(
            "allowed control and context must resolve to one registered scoped control".into(),
        ));
    }
    Ok(pack.bundle_id.clone())
}

fn scoped_draft_app_matches_host_binding(
    draft_app: &str,
    allowed_scope: &AllowedControlScope,
    canonical_app: &str,
    selected_pack: &AppPack,
    packs: &[AppPack],
    browser_page_profile: Option<BrowserPageProfile>,
) -> bool {
    if draft_app == canonical_app || draft_app == allowed_scope.application {
        return true;
    }
    if draft_app != selected_pack.name {
        return false;
    }

    let mut matches: Vec<&str> = packs
        .iter()
        .filter(|pack| pack.name == draft_app || pack.bundle_id == draft_app)
        .map(|pack| pack.bundle_id.as_str())
        .collect();
    let browser_pack = AppPack::browser_fixture();
    if browser_page_profile.is_some()
        && (browser_pack.name == draft_app || browser_pack.bundle_id == draft_app)
    {
        matches.push(browser_pack.bundle_id.as_str());
    }
    matches.len() == 1 && matches[0] == canonical_app
}

fn simple_control_role(role: &str) -> bool {
    let role = role.strip_prefix("AX").unwrap_or(role);
    matches!(
        normalize_label(role).as_str(),
        "button" | "checkbox" | "radio button" | "radiobutton" | "switch"
    )
}

fn scoped_native_text_value_supported(value: &str, role: &str) -> bool {
    value.len() <= MAX_SCOPED_NATIVE_TEXT_BYTES
        && !value
            .chars()
            .any(|character| character.is_control() && !['\n', '\r', '\t'].contains(&character))
        && (role != "AXTextField"
            || !value
                .chars()
                .any(|character| matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')))
}

/// Compile a model draft against the Host-persisted typed scope, when present.
/// The model can propose a goal, but cannot supply or widen its authorization.
pub(crate) fn compile_goal_with_context(
    root: &RootTaskId,
    user_text: &str,
    draft: &GoalDraft,
    context: HostGoalCompileContext<'_>,
    default_budget: RootBudgetSpec,
    packs: &[AppPack],
) -> Result<(GoalSpec, AppPack), CompileError> {
    let HostGoalCompileContext {
        execution_domain,
        browser_page_profile,
        allowed_scope,
        allowed_scope_app,
    } = context;
    interaction_contracts::desktop::task::validate_user_text(user_text)
        .map_err(CompileError::InvalidGoal)?;
    if allowed_scope.is_some() != allowed_scope_app.is_some() {
        return Err(CompileError::InvalidGoal(
            "allowed control scope is missing its trusted application binding".into(),
        ));
    }
    if let (Some(allowed_scope), Some(allowed_scope_app)) = (allowed_scope, allowed_scope_app) {
        let resolved = resolve_allowed_scope_app(allowed_scope, packs, browser_page_profile)?;
        if resolved != allowed_scope_app {
            return Err(CompileError::InvalidGoal(
                "trusted application binding no longer matches the allowed scope".into(),
            ));
        }
    }
    let pack = if execution_domain == ExecutionDomain::HeadlessBrowser {
        let Some(profile) = browser_page_profile else {
            return Err(CompileError::InvalidGoal(
                "headless browser requires a Host-selected page profile".into(),
            ));
        };
        let profile_pack = AppPack::browser_fixture();
        let app_is_authorized = match (allowed_scope, allowed_scope_app) {
            (Some(scope), Some(canonical_app)) => {
                canonical_app == profile.app()
                    && scoped_draft_app_matches_host_binding(
                        &draft.app,
                        scope,
                        canonical_app,
                        &profile_pack,
                        packs,
                        browser_page_profile,
                    )
            }
            (None, None) => draft.app == profile.app(),
            _ => false,
        };
        if !app_is_authorized
            || draft.target.semantic_target.as_str() != profile.semantic_target()
            || draft.target.label != profile.label()
            || draft.target.group.is_some()
            || draft.target.role_hint.as_deref() != Some(profile.role())
            || draft.target.row.is_some()
            || !matches!(&draft.desired, ExpectedValue::Bool { .. })
            || draft.confirmation_scope != ConfirmationScope::CurrentUi
            || draft.max_effect_class != EffectClass::ReversibleEdit
        {
            return Err(CompileError::InvalidGoal(
                "headless browser goals must match the Host-selected fixed checkbox profile".into(),
            ));
        }
        profile_pack
    } else {
        if browser_page_profile.is_some() {
            return Err(CompileError::InvalidGoal(
                "browser page profile requires the headless browser execution domain".into(),
            ));
        }
        if let (Some(scope), Some(canonical_app)) = (allowed_scope, allowed_scope_app) {
            let mut matching_packs = packs.iter().filter(|pack| pack.bundle_id == canonical_app);
            let Some(pack) = matching_packs.next() else {
                return Err(CompileError::InvalidGoal(
                    "trusted application binding has no registered app pack".into(),
                ));
            };
            if matching_packs.next().is_some()
                || !scoped_draft_app_matches_host_binding(
                    &draft.app,
                    scope,
                    canonical_app,
                    pack,
                    packs,
                    browser_page_profile,
                )
            {
                return Err(CompileError::InvalidGoal(
                    "planner-selected app is outside the exact trusted scope application".into(),
                ));
            }
            pack.clone()
        } else {
            packs
                .iter()
                .find(|pack| pack.bundle_id == draft.app)
                .ok_or_else(|| CompileError::AppNotExecutable(draft.app.clone()))?
                .clone()
        }
    };
    if let Some(allowed_scope) = allowed_scope {
        validate_scoped_draft(
            draft,
            allowed_scope,
            allowed_scope_app.unwrap(),
            execution_domain,
            &pack,
        )?;
    }
    let scoped_revisit = match (allowed_scope, draft.confirmation_scope, execution_domain) {
        (Some(scope), ConfirmationScope::UiRevisit, ExecutionDomain::ForegroundNative) => {
            let scoped = pack.scoped_control(&scope.targets[0]).ok_or_else(|| {
                CompileError::InvalidGoal(
                    "foreground revisit target is not an exact registered fixture control".into(),
                )
            })?;
            let (home, away) = pack.scoped_revisit_pages(scoped.control).ok_or_else(|| {
                CompileError::InvalidGoal(
                    "the trusted app pack has no scoped revisit pair for this control".into(),
                )
            })?;
            Some(ScopedUiRevisit {
                home: target_spec(home)?,
                away: target_spec(away)?,
            })
        }
        (Some(_), _, _) => None,
        (None, _, _) => None,
    };
    let scoped_commit = if let Some(scope) = allowed_scope {
        let scoped = pack.scoped_control(&scope.targets[0]).ok_or_else(|| {
            CompileError::InvalidGoal(
                "scoped commit target is not registered by the trusted app pack".into(),
            )
        })?;
        pack.scoped_commit_binding(scoped.control)
    } else {
        None
    };
    if execution_domain == ExecutionDomain::BackgroundAx {
        let supported_role = draft.target.role_hint.as_deref().is_some_and(|role| {
            matches!(
                role,
                "AXButton" | "AXCheckBox" | "AXRadioButton" | "AXSwitch"
            )
        });
        let registered_control =
            pack.control(&draft.target.semantic_target)
                .is_some_and(|control| {
                    Some(control.role.as_str()) == draft.target.role_hint.as_deref()
                        && normalize_label(&control.label) == normalize_label(&draft.target.label)
                });
        if !supported_role
            || !registered_control
            || draft.target.row.is_some()
            || !matches!(&draft.desired, ExpectedValue::Bool { .. })
            || draft.confirmation_scope != ConfirmationScope::CurrentUi
            || pack.effect_class(
                draft.target.role_hint.as_deref().unwrap_or_default(),
                &draft.target.label,
            ) > draft.max_effect_class
        {
            return Err(CompileError::InvalidGoal(
                "background AX goals require a registered simple boolean control and current-UI confirmation with fresh readback".into(),
            ));
        }
    }
    if let Some(row) = &draft.target.row {
        row.validate().map_err(CompileError::InvalidGoal)?;
        if !matches!(&draft.desired, ExpectedValue::Bool { value: true }) {
            return Err(CompileError::InvalidGoal(
                "row targets can only request selected=true".into(),
            ));
        }
        if draft.confirmation_scope != ConfirmationScope::UiRevisit {
            return Err(CompileError::InvalidGoal(
                "row selection is a navigation operation and requires UI revisit confirmation"
                    .into(),
            ));
        }
        let selection_effect = pack
            .row_selection_effect_class(
                &draft.target.semantic_target,
                row,
                draft.target.label.as_str(),
            )
            .ok_or_else(|| {
                CompileError::InvalidGoal(
                    "row target does not match a registered application table profile".into(),
                )
            })?;
        if selection_effect > draft.max_effect_class {
            return Err(CompileError::InvalidGoal(
                "row selection effect exceeds the frozen goal ceiling".into(),
            ));
        }
    }
    let goal_app = allowed_scope_app.unwrap_or(&draft.app).to_string();
    let target = draft.target.semantic_target.clone();
    let mut checks = vec![Predicate::ContextMatches {
        app: goal_app.clone(),
    }];
    if draft.target.row.is_some() {
        checks.push(Predicate::RowSelected {
            target: target.clone(),
            observed_after_action: true,
        });
    } else {
        checks.push(Predicate::ReadValueEquals {
            target: target.clone(),
            expected: draft.desired.clone(),
            observed_after_action: true,
        });
    }
    match draft.confirmation_scope {
        ConfirmationScope::CurrentUi
            if matches!(
                execution_domain,
                ExecutionDomain::BackgroundAx | ExecutionDomain::HeadlessBrowser
            ) => {}
        ConfirmationScope::CurrentUi => {
            return Err(CompileError::InvalidGoal(
                "root goals require UI revisit or commit confirmation".into(),
            ));
        }
        ConfirmationScope::UiRevisit => checks.push(Predicate::Revisited { target }),
        ConfirmationScope::Commit => checks.push(Predicate::CommitConfirmed { target }),
    }
    if scoped_commit
        .as_ref()
        .is_some_and(|commit| commit.receipt.kind == ScopedCommitKind::Automatic)
    {
        checks.push(Predicate::CommitConfirmed {
            target: draft.target.semantic_target.clone(),
        });
    }
    let goal = GoalSpec {
        root_task_id: root.clone(),
        task_revision: 1,
        execution_domain,
        browser_page_profile,
        user_text: user_text.to_string(),
        scope: TaskScope {
            app: goal_app,
            account: if allowed_scope.is_some() {
                None
            } else {
                draft.account.clone()
            },
        },
        allowed_scope: allowed_scope.cloned(),
        scoped_revisit,
        scoped_commit,
        target: draft.target.clone(),
        desired: draft.desired.clone(),
        confirmation_scope: draft.confirmation_scope,
        max_effect_class: draft.max_effect_class,
        required: Predicate::And { of: checks },
        budget: default_budget,
    };
    goal.validate().map_err(CompileError::InvalidGoal)?;
    Ok((goal, pack))
}

fn validate_scoped_draft(
    draft: &GoalDraft,
    allowed_scope: &AllowedControlScope,
    allowed_app: &str,
    execution_domain: ExecutionDomain,
    pack: &AppPack,
) -> Result<(), CompileError> {
    let scope_target = &allowed_scope.targets[0];
    let Some(scoped) = pack.scoped_control(scope_target) else {
        return Err(CompileError::InvalidGoal(
            "allowed control and context are not registered by the trusted app pack".into(),
        ));
    };
    let control = scoped.control;
    let value_kind_matches = matches!(
        (scoped.value_kind, &draft.desired),
        (ScopedControlValueKind::Bool, ExpectedValue::Bool { .. })
            | (ScopedControlValueKind::Text, ExpectedValue::Text { .. })
    ) || matches!(
        (scoped.value_kind, &draft.desired),
        (ScopedControlValueKind::Number, ExpectedValue::Number { value })
            if (-MAX_SCOPED_FORM_SAFE_INTEGER..=MAX_SCOPED_FORM_SAFE_INTEGER).contains(value)
    ) || matches!(
        (scoped.value_kind, &draft.desired),
        (ScopedControlValueKind::Choice(choices), ExpectedValue::Text { value })
            if choices.contains(&value.as_str())
    ) || matches!(
        (scoped.value_kind, &draft.desired),
        (ScopedControlValueKind::RadioChoice(choices), ExpectedValue::Text { value })
            if choices.contains(&value.as_str())
    );
    let native_text_value_supported = match (scoped.value_kind, &draft.desired) {
        (ScopedControlValueKind::Text, ExpectedValue::Text { value }) => {
            scoped_native_text_value_supported(value, &control.role)
        }
        (ScopedControlValueKind::Choice(choices), ExpectedValue::Text { value }) => {
            choices.contains(&value.as_str())
        }
        (ScopedControlValueKind::RadioChoice(choices), ExpectedValue::Text { value }) => {
            choices.contains(&value.as_str())
        }
        (_, ExpectedValue::Bool { .. }) => true,
        (_, ExpectedValue::Number { value }) => {
            (-MAX_SCOPED_FORM_SAFE_INTEGER..=MAX_SCOPED_FORM_SAFE_INTEGER).contains(value)
        }
        _ => false,
    };
    let role_matches_value_kind = match scoped.value_kind {
        ScopedControlValueKind::Bool => simple_control_role(&control.role),
        ScopedControlValueKind::Text => {
            matches!(control.role.as_str(), "AXTextField" | "AXTextArea")
        }
        ScopedControlValueKind::Number => control.role == "AXTextField",
        ScopedControlValueKind::Choice(_) => control.role == "AXPopUpButton",
        ScopedControlValueKind::RadioChoice(_) => control.role == "AXRadioGroup",
    };
    let scoped_commit = pack.scoped_commit_binding(control);
    let exact_manual_commit = execution_domain == ExecutionDomain::ForegroundNative
        && draft.confirmation_scope == ConfirmationScope::Commit
        && scoped_commit.as_ref().is_some_and(|commit| {
            commit.receipt.kind == ScopedCommitKind::Manual && commit.action.is_some()
        });
    let confirmation_matches_context = if scoped.context_kind == ScopedControlContextKind::Form {
        execution_domain == ExecutionDomain::ForegroundNative
            && draft.confirmation_scope == ConfirmationScope::UiRevisit
    } else {
        matches!(
            (execution_domain, draft.confirmation_scope),
            (
                ExecutionDomain::BackgroundAx | ExecutionDomain::HeadlessBrowser,
                ConfirmationScope::CurrentUi
            ) | (
                ExecutionDomain::ForegroundNative,
                ConfirmationScope::UiRevisit
            )
        ) || (execution_domain == ExecutionDomain::ForegroundNative
            && draft.confirmation_scope == ConfirmationScope::Commit
            && scoped_commit
                .as_ref()
                .is_some_and(|commit| commit.receipt.kind == ScopedCommitKind::Manual))
    };
    if pack.bundle_id != allowed_app
        || draft.account.is_some()
        || draft.target.row.is_some()
        || draft.target.semantic_target.as_str() != control.semantic
        || draft.target.label != scope_target.control
        || draft.target.label != control.label
        || draft.target.group.as_deref() != scope_target.context_label()
        || draft.target.group.as_deref() != scoped.context_label
        || draft.target.role_hint.as_deref() != Some(control.role.as_str())
        || !role_matches_value_kind
        || !value_kind_matches
        || !native_text_value_supported
        || !confirmation_matches_context
        || (draft.max_effect_class > EffectClass::ReversibleEdit
            && !(exact_manual_commit && draft.max_effect_class == EffectClass::Commit))
        || pack.effect_class(&control.role, &control.label) > draft.max_effect_class
    {
        return Err(CompileError::InvalidGoal(
            "planner goal exceeds the single registered typed control scope".into(),
        ));
    }
    Ok(())
}

fn target_spec(control: &crate::environment::pack::ControlDef) -> Result<TargetSpec, CompileError> {
    Ok(TargetSpec {
        semantic_target: SemanticTargetRef::new(control.semantic.clone())
            .map_err(|error| CompileError::InvalidGoal(error.to_string()))?,
        label: control.label.clone(),
        group: None,
        role_hint: Some(control.role.clone()),
        row: None,
    })
}

/// The goal in words the user can check before anything runs.
pub fn confirmation_for(goal: &GoalSpec) -> ConfirmationRequest {
    ConfirmationRequest {
        confirmation_id: ConfirmationId::new(format!(
            "conf-{}-r{}",
            goal.root_task_id, goal.task_revision
        ))
        .expect("derived from valid ids"),
        root_task_id: goal.root_task_id.clone(),
        task_revision: goal.task_revision,
        kind: ConfirmationKind::Goal {
            goal: Box::new(GoalSummary {
                user_text: goal.user_text.clone(),
                app: goal.scope.app.clone(),
                target_label: goal.target.label.clone(),
                group: goal.target.group.clone(),
                row: goal.target.row.clone(),
                desired: goal.desired.clone(),
                confirmation_scope: goal.confirmation_scope,
                max_effect_class: goal.max_effect_class,
                allowed_scope: goal.allowed_scope.clone(),
                scoped_revisit: goal.scoped_revisit.clone(),
                scoped_commit: goal.scoped_commit.clone(),
                browser_page_profile: goal.browser_page_profile,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::browser::BrowserPageProfile;
    use interaction_contracts::desktop::ids::SemanticTargetRef;
    use interaction_contracts::desktop::planning::{Dependency, Subgoal};
    use interaction_contracts::desktop::task::{
        AllowedControlScope, AllowedControlTarget, ExpectedValue, TargetSpec,
    };

    fn registered() -> Vec<String> {
        [
            "ui.read_value",
            "ui.ensure_value",
            "ui.select_by_label",
            "ui.revisit",
            "ui.activate",
            "ui.select_row",
            "ui.scroll_to",
        ]
        .map(String::from)
        .to_vec()
    }

    fn budget() -> RootBudgetSpec {
        RootBudgetSpec {
            deadline_ms: 120_000,
            max_attempts: 12,
            max_observations: 40,
            max_recoveries: 4,
            max_replans: 2,
        }
    }

    fn draft(scope: ConfirmationScope, max: EffectClass) -> GoalDraft {
        GoalDraft {
            app: "test.settings".into(),
            account: None,
            target: TargetSpec {
                semantic_target: SemanticTargetRef::new("push/daily-picks").unwrap(),
                label: "Daily picks".into(),
                group: Some("Push".into()),
                role_hint: Some("switch".into()),
                row: None,
            },
            desired: ExpectedValue::Bool { value: false },
            confirmation_scope: scope,
            max_effect_class: max,
        }
    }

    fn saved_view_row_draft(
        desired: ExpectedValue,
        scope: ConfirmationScope,
        max_effect_class: EffectClass,
    ) -> GoalDraft {
        GoalDraft {
            app: "com.lingxi.computer-user.mvp-fixture".into(),
            account: None,
            target: TargetSpec {
                semantic_target: SemanticTargetRef::new("saved-view-row").unwrap(),
                label: "Quarterly review".into(),
                group: None,
                role_hint: Some("AXRow".into()),
                row: Some(interaction_contracts::desktop::task::RowTargetSpec {
                    table_scope: interaction_contracts::desktop::task::RowTableScope {
                        scroll_path: vec![interaction_contracts::desktop::task::RowContainerScope {
                            role: interaction_contracts::desktop::task::RowContainerRole::AxScrollArea,
                            label: "Saved views".into(),
                        }],
                        column_headers: vec!["Entry".into()],
                    },
                    conditions: vec![
                        interaction_contracts::desktop::task::RowColumnConditionSpec {
                            column_name: "Entry".into(),
                            value: ExpectedValue::Text {
                                value: "Quarterly review".into(),
                            },
                        },
                    ],
                }),
            },
            desired,
            confirmation_scope: scope,
            max_effect_class,
        }
    }

    fn compile(draft: &GoalDraft) -> Result<GoalSpec, CompileError> {
        compile_goal(
            &RootTaskId::new("root-1").unwrap(),
            "turn it off",
            draft,
            ExecutionDomain::Simulated,
            budget(),
            &[AppPack::settings_fixture()],
        )
        .map(|(goal, _)| goal)
    }

    fn browser_scope() -> AllowedControlScope {
        AllowedControlScope {
            application: "Browser fixture".into(),
            targets: vec![AllowedControlTarget {
                control: "Background option".into(),
                group: None,
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        }
    }

    fn browser_draft() -> GoalDraft {
        let profile = BrowserPageProfile::FixtureBackgroundOption;
        GoalDraft {
            app: profile.app().into(),
            account: None,
            target: TargetSpec {
                semantic_target: SemanticTargetRef::new(profile.semantic_target()).unwrap(),
                label: profile.label().into(),
                group: None,
                role_hint: Some(profile.role().into()),
                row: None,
            },
            desired: ExpectedValue::Bool { value: false },
            confirmation_scope: ConfirmationScope::CurrentUi,
            max_effect_class: EffectClass::ReversibleEdit,
        }
    }

    fn mvp_scope(control: &str, group: &str, disambiguator: Option<&str>) -> AllowedControlScope {
        AllowedControlScope {
            application: "Interaction Fixture".into(),
            targets: vec![AllowedControlTarget {
                control: control.into(),
                group: Some(group.into()),
                form: None,
                disambiguator: disambiguator.map(str::to_owned),
            }],
            no_unrelated_controls: true,
        }
    }

    fn mvp_scoped_draft() -> GoalDraft {
        GoalDraft {
            app: "com.lingxi.computer-user.mvp-fixture".into(),
            account: None,
            target: TargetSpec {
                semantic_target: SemanticTargetRef::new("setting-weekly-digest-en").unwrap(),
                label: "Weekly digest".into(),
                group: Some("Notifications".into()),
                role_hint: Some("AXCheckBox".into()),
                row: None,
            },
            desired: ExpectedValue::Bool { value: false },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::ReversibleEdit,
        }
    }

    fn mvp_form_scope(control: &str, form: &str) -> AllowedControlScope {
        AllowedControlScope {
            application: "Interaction Fixture".into(),
            targets: vec![AllowedControlTarget {
                control: control.into(),
                group: None,
                form: Some(form.into()),
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        }
    }

    fn mvp_form_draft(
        semantic: &str,
        control: &str,
        form: &str,
        role: &str,
        desired: ExpectedValue,
    ) -> GoalDraft {
        GoalDraft {
            app: "com.lingxi.computer-user.mvp-fixture".into(),
            account: None,
            target: TargetSpec {
                semantic_target: SemanticTargetRef::new(semantic).unwrap(),
                label: control.into(),
                group: Some(form.into()),
                role_hint: Some(role.into()),
                row: None,
            },
            desired,
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::ReversibleEdit,
        }
    }

    fn mvp_popup_draft(label: &str, desired: &str) -> GoalDraft {
        mvp_form_draft(
            "form-reminder-cadence-en",
            label,
            "Preferences",
            "AXPopUpButton",
            ExpectedValue::Text {
                value: desired.into(),
            },
        )
    }

    fn mvp_radio_group_draft(label: &str, form: &str, desired: &str) -> GoalDraft {
        let (semantic, role) = match label {
            "Summary grouping" => ("form-summary-grouping-en", "AXRadioGroup"),
            "摘要分组" => ("form-summary-grouping-zh", "AXRadioGroup"),
            _ => panic!("unknown test radio group"),
        };
        mvp_form_draft(
            semantic,
            label,
            form,
            role,
            ExpectedValue::Text {
                value: desired.into(),
            },
        )
    }

    fn popup_plan(target: &str, choice: &str) -> PlanBundle {
        let mut select = step("select", "ui.select_by_label", target);
        select.select = Some(
            interaction_contracts::desktop::planning::SelectByLabelSpec {
                choice_label: choice.into(),
            },
        );
        bundle(vec![select, step("revisit", "ui.revisit", target)])
    }

    fn compile_mvp_scope(
        draft: &GoalDraft,
        scope: &AllowedControlScope,
    ) -> Result<(GoalSpec, AppPack), CompileError> {
        compile_goal_with_context(
            &RootTaskId::new("scoped-mvp-root").unwrap(),
            "Turn off Weekly digest.",
            draft,
            HostGoalCompileContext::scoped(
                ExecutionDomain::ForegroundNative,
                None,
                scope,
                "com.lingxi.computer-user.mvp-fixture",
            ),
            budget(),
            &[AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap()],
        )
    }

    fn compile_browser_scope(
        draft: &GoalDraft,
        allowed_scope: &AllowedControlScope,
        allowed_scope_app: &str,
    ) -> Result<(GoalSpec, AppPack), CompileError> {
        compile_goal_with_context(
            &RootTaskId::new("scoped-root").unwrap(),
            "turn off the background option",
            draft,
            HostGoalCompileContext::scoped(
                ExecutionDomain::HeadlessBrowser,
                Some(BrowserPageProfile::FixtureBackgroundOption),
                allowed_scope,
                allowed_scope_app,
            ),
            budget(),
            &[AppPack::browser_fixture()],
        )
    }

    fn step(id: &str, skill: &str, target: &str) -> Subgoal {
        Subgoal {
            select: None,
            id: id.into(),
            description: id.into(),
            skill: skill.into(),
            target: Some(SemanticTargetRef::new(target).unwrap()),
            expects: vec![],
            scroll: None,
        }
    }

    fn bundle(steps: Vec<Subgoal>) -> PlanBundle {
        let dependencies = steps
            .windows(2)
            .map(|pair| Dependency {
                before: pair[0].id.clone(),
                after: pair[1].id.clone(),
            })
            .collect();
        PlanBundle {
            subgoals: steps,
            dependencies,
            required_observations: vec![],
            expected_effects: vec![],
            recovery_branches: vec![],
            termination: vec![],
        }
    }

    fn row_plan() -> PlanBundle {
        bundle(vec![
            step("select", "ui.select_row", "saved-view-row"),
            step("revisit", "ui.revisit", "saved-view-row"),
        ])
    }

    #[test]
    fn scoped_goal_binds_one_trusted_app_control_and_confirmation_scope() {
        let scope = browser_scope();
        let draft = browser_draft();
        let (goal, pack) = compile_browser_scope(&draft, &scope, "browser.fixture").unwrap();
        assert_eq!(goal.allowed_scope, Some(scope.clone()));
        assert_eq!(goal.scope.app, "browser.fixture");
        assert_eq!(pack.bundle_id, "browser.fixture");
        assert!(confirmation_for(&goal).validate().is_ok());

        let mut wrong_target = draft.clone();
        wrong_target.target.semantic_target = SemanticTargetRef::new("other-control").unwrap();
        assert!(compile_browser_scope(&wrong_target, &scope, "browser.fixture").is_err());

        let mut wrong_group = scope.clone();
        wrong_group.targets[0].group = Some("Other group".into());
        assert!(compile_browser_scope(&draft, &wrong_group, "browser.fixture").is_err());

        let mut wrong_app = scope.clone();
        wrong_app.application = "Untrusted application".into();
        assert!(compile_browser_scope(&draft, &wrong_app, "browser.fixture").is_err());
        assert!(compile_browser_scope(&draft, &scope, "other.bundle").is_err());

        let mut revisit = draft.clone();
        revisit.confirmation_scope = ConfirmationScope::UiRevisit;
        assert!(compile_browser_scope(&revisit, &scope, "browser.fixture").is_err());
        let mut text_desired = draft;
        text_desired.desired = ExpectedValue::Text { value: "on".into() };
        assert!(compile_browser_scope(&text_desired, &scope, "browser.fixture").is_err());
    }

    #[test]
    fn frozen_native_boolean_goal_gets_only_the_pack_derived_revisit_pair() {
        let scope = mvp_scope("Weekly digest", "Notifications", None);
        let (goal, pack) = compile_mvp_scope(&mvp_scoped_draft(), &scope).unwrap();
        let revisit = goal.scoped_revisit.as_ref().unwrap();
        assert_eq!(
            goal.target.semantic_target.as_str(),
            "setting-weekly-digest-en"
        );
        assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
        assert_eq!(revisit.home.semantic_target.as_str(), "page-settings");
        assert_eq!(revisit.home.label, "Settings");
        assert_eq!(revisit.away.semantic_target.as_str(), "page-overview");
        assert_eq!(revisit.away.label, "Overview");
        let confirmation = confirmation_for(&goal);
        let ConfirmationKind::Goal { goal: summary } = &confirmation.kind else {
            panic!("goal compilation should create a goal confirmation");
        };
        assert_eq!(summary.scoped_revisit.as_ref(), Some(revisit));
        assert!(confirmation.validate().is_ok());

        let target = goal.target.semantic_target.as_str();
        let planned = bundle(vec![
            step("ensure", "ui.ensure_value", target),
            step("revisit", "ui.revisit", target),
        ]);
        validate_plan(&goal, &planned, &pack, &registered()).unwrap();

        let reversed = bundle(vec![
            step("revisit", "ui.revisit", target),
            step("ensure", "ui.ensure_value", target),
        ]);
        assert!(validate_plan(&goal, &reversed, &pack, &registered()).is_err());

        let unsequenced = PlanBundle {
            dependencies: vec![],
            ..planned
        };
        assert!(validate_plan(&goal, &unsequenced, &pack, &registered()).is_err());
    }

    #[test]
    fn scoped_app_aliases_resolve_only_to_the_host_bound_pack_and_freeze_its_bundle_id() {
        let fixture = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let canonical_app = fixture.bundle_id.clone();
        let mut scope = mvp_scope("Weekly digest", "Notifications", None);
        let mut draft = mvp_scoped_draft();
        draft.app = fixture.name.clone();

        let compile = |draft: &GoalDraft, scope: &AllowedControlScope, packs: &[AppPack]| {
            compile_goal_with_context(
                &RootTaskId::new("scoped-app-alias").unwrap(),
                "Turn off Weekly digest.",
                draft,
                HostGoalCompileContext::scoped(
                    ExecutionDomain::ForegroundNative,
                    None,
                    scope,
                    &canonical_app,
                ),
                budget(),
                packs,
            )
        };

        let (goal, _) = compile(&draft, &scope, std::slice::from_ref(&fixture))
            .expect("the exact authorized scope display name resolves to the trusted pack");
        assert_eq!(goal.scope.app, canonical_app);
        assert!(matches!(
            &goal.required,
            Predicate::And { of }
                if matches!(of.first(), Some(Predicate::ContextMatches { app }) if app == &canonical_app)
        ));

        scope.application = canonical_app.clone();
        let (goal, _) = compile(&draft, &scope, std::slice::from_ref(&fixture))
            .expect("the unique registered pack display name is an allowed scoped alias");
        assert_eq!(goal.scope.app, canonical_app);

        let mut cross_scope_app = draft.clone();
        cross_scope_app.app = "com.lingxi.computer-user.guard-fixture".into();
        assert!(compile(&cross_scope_app, &scope, std::slice::from_ref(&fixture)).is_err());

        let mut wrong_case = draft.clone();
        wrong_case.app = fixture.name.to_lowercase();
        assert!(compile(&wrong_case, &scope, std::slice::from_ref(&fixture)).is_err());

        let mut duplicate_name =
            AppPack::registered_native("com.lingxi.computer-user.guard-fixture").unwrap();
        duplicate_name.name = fixture.name.clone();
        let packs = [fixture.clone(), duplicate_name];
        assert!(compile(&draft, &scope, &packs).is_err());

        let mut ambiguous_scope_name = scope;
        ambiguous_scope_name.application = fixture.name.clone();
        assert!(resolve_allowed_scope_app(&ambiguous_scope_name, &packs, None).is_err());

        let mut free_text_display_name = mvp_scoped_draft();
        free_text_display_name.app = "Interaction Fixture".into();
        assert!(matches!(
            compile_goal(
                &RootTaskId::new("free-text-app-name").unwrap(),
                "Turn off Weekly digest.",
                &free_text_display_name,
                ExecutionDomain::ForegroundNative,
                budget(),
                std::slice::from_ref(&fixture),
            ),
            Err(CompileError::AppNotExecutable(_))
        ));
    }

    #[test]
    fn frozen_form_goals_compile_only_registered_typed_controls_with_revisit() {
        let cases = [
            (
                "form-display-name-en",
                "Display name",
                "Profile",
                "AXTextField",
                ExpectedValue::Text {
                    value: "Synthetic name".into(),
                },
                "page-settings",
                "Settings",
                "page-overview",
                "Overview",
            ),
            (
                "form-notes-zh",
                "备注",
                "个人资料",
                "AXTextArea",
                ExpectedValue::Text {
                    value: "第一行\n第二行".into(),
                },
                "page-settings-zh",
                "设置",
                "page-overview-zh",
                "概览",
            ),
            (
                "form-notify-on-change-en",
                "Notify on change",
                "Preferences",
                "AXCheckBox",
                ExpectedValue::Bool { value: true },
                "page-settings",
                "Settings",
                "page-overview",
                "Overview",
            ),
        ];
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        for (
            semantic,
            label,
            form,
            role,
            desired,
            home_semantic,
            home_label,
            away_semantic,
            away_label,
        ) in cases
        {
            let scope = mvp_form_scope(label, form);
            let draft = mvp_form_draft(semantic, label, form, role, desired);
            let (goal, compiled_pack) = compile_goal_with_context(
                &RootTaskId::new("scoped-form-root").unwrap(),
                "Set the requested fixture form control.",
                &draft,
                HostGoalCompileContext::scoped(
                    ExecutionDomain::ForegroundNative,
                    None,
                    &scope,
                    "com.lingxi.computer-user.mvp-fixture",
                ),
                budget(),
                std::slice::from_ref(&pack),
            )
            .expect("the frozen form control is registered with its exact type");
            assert_eq!(compiled_pack.bundle_id, pack.bundle_id);
            assert_eq!(goal.target.semantic_target.as_str(), semantic);
            assert_eq!(goal.target.group.as_deref(), Some(form));
            assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
            let revisit = goal.scoped_revisit.as_ref().unwrap();
            assert_eq!(revisit.home.semantic_target.as_str(), home_semantic);
            assert_eq!(revisit.home.label, home_label);
            assert_eq!(revisit.away.semantic_target.as_str(), away_semantic);
            assert_eq!(revisit.away.label, away_label);
            let confirmation = confirmation_for(&goal);
            assert!(confirmation.validate().is_ok());

            let target = goal.target.semantic_target.as_str();
            let plan = bundle(vec![
                step("ensure", "ui.ensure_value", target),
                step("revisit", "ui.revisit", target),
            ]);
            validate_plan(&goal, &plan, &pack, &registered()).unwrap();
        }
    }

    #[test]
    fn region_code_goals_keep_text_input_and_exact_account_revisit() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        for (locale, semantic, label, form, home, home_label, away, away_label, desired) in [
            (
                "en",
                "form-region-code-en",
                "Region code",
                "Account",
                "page-settings",
                "Settings",
                "page-overview",
                "Overview",
                "CA-2345",
            ),
            (
                "zh",
                "form-region-code-zh",
                "地区代码",
                "账户",
                "page-settings-zh",
                "设置",
                "page-overview-zh",
                "概览",
                "CA-2345",
            ),
        ] {
            let scope = mvp_form_scope(label, form);
            let draft = mvp_form_draft(
                semantic,
                label,
                form,
                "AXTextField",
                ExpectedValue::Text {
                    value: desired.into(),
                },
            );
            let (goal, _) = compile_goal_with_context(
                &RootTaskId::new(format!("region-code-{locale}-root")).unwrap(),
                "Correct the region code.",
                &draft,
                HostGoalCompileContext::scoped(
                    ExecutionDomain::ForegroundNative,
                    None,
                    &scope,
                    "com.lingxi.computer-user.mvp-fixture",
                ),
                budget(),
                std::slice::from_ref(&pack),
            )
            .expect("the exact Region code Text field is registered in Account");

            assert_eq!(goal.target.semantic_target.as_str(), semantic);
            assert_eq!(goal.target.label, label);
            assert_eq!(goal.target.role_hint.as_deref(), Some("AXTextField"));
            assert_eq!(goal.target.group.as_deref(), Some(form));
            assert_eq!(
                goal.desired,
                ExpectedValue::Text {
                    value: desired.into()
                }
            );
            assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
            assert_eq!(goal.max_effect_class, EffectClass::ReversibleEdit);
            assert!(goal.scoped_commit.is_none());
            let target = goal.target.semantic_target.clone();
            assert_eq!(
                goal.required,
                Predicate::And {
                    of: vec![
                        Predicate::ContextMatches {
                            app: pack.bundle_id.clone(),
                        },
                        Predicate::ReadValueEquals {
                            target: target.clone(),
                            expected: goal.desired.clone(),
                            observed_after_action: true,
                        },
                        Predicate::Revisited {
                            target: target.clone(),
                        },
                    ]
                }
            );
            let revisit = goal.scoped_revisit.as_ref().unwrap();
            assert_eq!(revisit.home.semantic_target.as_str(), home);
            assert_eq!(revisit.home.label, home_label);
            assert_eq!(revisit.away.semantic_target.as_str(), away);
            assert_eq!(revisit.away.label, away_label);
            assert!(confirmation_for(&goal).validate().is_ok());
            let serialized_summary = serde_json::to_string(&confirmation_for(&goal)).unwrap();
            assert!(!serialized_summary.contains("validation_feedback"));
        }

        let valid_scope = mvp_form_scope("Region code", "Account");
        let valid_draft = mvp_form_draft(
            "form-region-code-en",
            "Region code",
            "Account",
            "AXTextField",
            ExpectedValue::Text {
                value: "CA-2345".into(),
            },
        );
        let compile = |draft: &GoalDraft, scope: &AllowedControlScope| {
            compile_goal_with_context(
                &RootTaskId::new("region-code-invalid-root").unwrap(),
                "Correct the region code.",
                draft,
                HostGoalCompileContext::scoped(
                    ExecutionDomain::ForegroundNative,
                    None,
                    scope,
                    "com.lingxi.computer-user.mvp-fixture",
                ),
                budget(),
                std::slice::from_ref(&pack),
            )
        };
        assert!(compile(&valid_draft, &mvp_form_scope("Region code", "Profile")).is_err());
        let mut wrong_role = valid_draft;
        wrong_role.target.role_hint = Some("AXRadioGroup".into());
        assert!(compile(&wrong_role, &valid_scope).is_err());
    }

    #[test]
    fn frozen_popup_goals_compile_only_pack_choices_and_require_select_then_revisit() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let cases = [
            (
                "form-reminder-cadence-en",
                "Reminder cadence",
                "Preferences",
                "Daily",
                "Monthly",
            ),
            (
                "form-reminder-cadence-zh",
                "提醒频率",
                "偏好设置",
                "每天",
                "每月",
            ),
        ];

        for (semantic, label, form, desired, other_choice) in cases {
            let scope = mvp_form_scope(label, form);
            let draft = mvp_form_draft(
                semantic,
                label,
                form,
                "AXPopUpButton",
                ExpectedValue::Text {
                    value: desired.into(),
                },
            );
            let (goal, compiled_pack) = compile_goal_with_context(
                &RootTaskId::new("scoped-popup-root").unwrap(),
                "Set the requested fixture reminder cadence.",
                &draft,
                HostGoalCompileContext::scoped(
                    ExecutionDomain::ForegroundNative,
                    None,
                    &scope,
                    "com.lingxi.computer-user.mvp-fixture",
                ),
                budget(),
                std::slice::from_ref(&pack),
            )
            .expect("the Host pack authorizes the exact localized popup choice");
            assert_eq!(compiled_pack.bundle_id, pack.bundle_id);
            assert_eq!(goal.target.semantic_target.as_str(), semantic);
            assert_eq!(goal.target.role_hint.as_deref(), Some("AXPopUpButton"));
            assert_eq!(goal.target.group.as_deref(), Some(form));
            assert_eq!(
                goal.desired,
                ExpectedValue::Text {
                    value: desired.into()
                }
            );
            assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
            assert_eq!(goal.max_effect_class, EffectClass::ReversibleEdit);
            assert!(goal.scoped_commit.is_none());
            assert!(goal.scoped_revisit.is_some());
            let serialized_goal = serde_json::to_string(&goal).unwrap();
            assert!(!serialized_goal.contains("choice_values"));
            assert!(!serialized_goal.contains(other_choice));
            assert!(confirmation_for(&goal).validate().is_ok());

            let target = goal.target.semantic_target.as_str();
            let plan = popup_plan(target, desired);
            validate_plan(&goal, &plan, &pack, &registered()).unwrap();

            let mut missing_frozen_revisit = goal.clone();
            missing_frozen_revisit.scoped_revisit = None;
            assert!(validate_plan(&missing_frozen_revisit, &plan, &pack, &registered()).is_err());

            let mut read_select_revisit = step("read", "ui.read_value", target);
            let mut select = step("select", "ui.select_by_label", target);
            select.select = Some(
                interaction_contracts::desktop::planning::SelectByLabelSpec {
                    choice_label: desired.into(),
                },
            );
            let read_plan = bundle(vec![
                read_select_revisit,
                select,
                step("revisit", "ui.revisit", target),
            ]);
            validate_plan(&goal, &read_plan, &pack, &registered()).unwrap();
            read_select_revisit = step("read", "ui.read_value", "form-notes-en");
            let mut foreign_read_plan = read_plan.clone();
            foreign_read_plan.subgoals[0] = read_select_revisit;
            assert!(validate_plan(&goal, &foreign_read_plan, &pack, &registered()).is_err());

            let mut wrong_goal_target = goal.clone();
            wrong_goal_target.target.semantic_target =
                SemanticTargetRef::new("form-notes-en").unwrap();
            let wrong_goal_plan = popup_plan("form-notes-en", desired);
            assert!(
                validate_plan(&wrong_goal_target, &wrong_goal_plan, &pack, &registered()).is_err()
            );

            let mut wrong_choice = popup_plan(target, desired);
            wrong_choice.subgoals[0]
                .select
                .as_mut()
                .unwrap()
                .choice_label = other_choice.into();
            assert!(validate_plan(&goal, &wrong_choice, &pack, &registered()).is_err());

            let mut wrong_target = popup_plan(target, desired);
            wrong_target.subgoals[0].target =
                Some(SemanticTargetRef::new("form-notes-en").unwrap());
            assert!(validate_plan(&goal, &wrong_target, &pack, &registered()).is_err());

            let ensure_plan = bundle(vec![
                step("ensure", "ui.ensure_value", target),
                step("revisit", "ui.revisit", target),
            ]);
            assert!(validate_plan(&goal, &ensure_plan, &pack, &registered()).is_err());

            let mut missing_dependency = popup_plan(target, desired);
            missing_dependency.dependencies.clear();
            assert!(validate_plan(&goal, &missing_dependency, &pack, &registered()).is_err());

            let mut duplicate_select = popup_plan(target, desired);
            let mut second_select = step("select-again", "ui.select_by_label", target);
            second_select.select = Some(
                interaction_contracts::desktop::planning::SelectByLabelSpec {
                    choice_label: desired.into(),
                },
            );
            duplicate_select.subgoals.insert(1, second_select);
            duplicate_select.dependencies = vec![
                Dependency {
                    before: "select".into(),
                    after: "select-again".into(),
                },
                Dependency {
                    before: "select-again".into(),
                    after: "revisit".into(),
                },
            ];
            assert!(validate_plan(&goal, &duplicate_select, &pack, &registered()).is_err());
        }
    }

    #[test]
    fn frozen_popup_goals_reject_unregistered_choice_context_and_role() {
        let valid_scope = mvp_form_scope("Reminder cadence", "Preferences");
        let valid_draft = mvp_popup_draft("Reminder cadence", "Weekly");
        let compile =
            |draft: &GoalDraft, scope: &AllowedControlScope| compile_mvp_scope(draft, scope);

        let mut invalid_choice = valid_draft.clone();
        invalid_choice.desired = ExpectedValue::Text {
            value: "Yearly".into(),
        };
        assert!(compile(&invalid_choice, &valid_scope).is_err());

        let wrong_context = mvp_form_scope("Reminder cadence", "Profile");
        assert!(compile(&valid_draft, &wrong_context).is_err());

        let mut wrong_role = valid_draft;
        wrong_role.target.role_hint = Some("AXMenuButton".into());
        assert!(compile(&wrong_role, &valid_scope).is_err());
    }

    #[test]
    fn frozen_radio_group_goals_preserve_parent_text_scope_and_exact_selection_plan() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let cases = [
            (
                "Summary grouping",
                "Preferences",
                "By week",
                "By day",
                "form-summary-grouping-en",
            ),
            (
                "摘要分组",
                "偏好设置",
                "按周",
                "按天",
                "form-summary-grouping-zh",
            ),
        ];

        for (label, form, desired, other_choice, semantic) in cases {
            let scope = mvp_form_scope(label, form);
            let draft = mvp_radio_group_draft(label, form, desired);
            let (goal, compiled_pack) = compile_mvp_scope(&draft, &scope)
                .expect("the Host pack authorizes the exact localized parent choice");
            assert_eq!(compiled_pack.bundle_id, pack.bundle_id);
            assert_eq!(goal.target.semantic_target.as_str(), semantic);
            assert_eq!(goal.target.label, label);
            assert_eq!(goal.target.role_hint.as_deref(), Some("AXRadioGroup"));
            assert_eq!(goal.target.group.as_deref(), Some(form));
            assert_eq!(
                goal.desired,
                ExpectedValue::Text {
                    value: desired.into()
                }
            );
            assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
            assert_eq!(goal.max_effect_class, EffectClass::ReversibleEdit);
            assert!(goal.scoped_commit.is_none());
            assert!(goal.scoped_revisit.is_some());

            let ConfirmationKind::Goal { goal: summary } = confirmation_for(&goal).kind else {
                panic!("radio group confirmation remains a Goal summary")
            };
            assert_eq!(summary.target_label, label);
            assert_eq!(summary.group.as_deref(), Some(form));
            assert_eq!(
                summary.desired,
                ExpectedValue::Text {
                    value: desired.into()
                }
            );
            let serialized_summary = serde_json::to_string(&summary).unwrap();
            assert!(!serialized_summary.contains("choice_values"));
            assert!(!serialized_summary.contains(other_choice));

            let target = goal.target.semantic_target.as_str();
            let plan = popup_plan(target, desired);
            validate_plan(&goal, &plan, &pack, &registered()).unwrap();

            let mut wrong_choice = popup_plan(target, desired);
            wrong_choice.subgoals[0]
                .select
                .as_mut()
                .unwrap()
                .choice_label = other_choice.into();
            assert!(validate_plan(&goal, &wrong_choice, &pack, &registered()).is_err());

            let mut child_target = popup_plan(target, desired);
            child_target.subgoals[0].target =
                Some(SemanticTargetRef::new("summary-radio-by-week").unwrap());
            assert!(validate_plan(&goal, &child_target, &pack, &registered()).is_err());

            let ensure_plan = bundle(vec![
                step("ensure", "ui.ensure_value", target),
                step("revisit", "ui.revisit", target),
            ]);
            assert!(validate_plan(&goal, &ensure_plan, &pack, &registered()).is_err());

            let mut missing_dependency = popup_plan(target, desired);
            missing_dependency.dependencies.clear();
            assert!(validate_plan(&goal, &missing_dependency, &pack, &registered()).is_err());

            let mut read = step("read", "ui.read_value", target);
            let mut select = step("select", "ui.select_by_label", target);
            select.select = Some(
                interaction_contracts::desktop::planning::SelectByLabelSpec {
                    choice_label: desired.into(),
                },
            );
            let read_select_revisit = bundle(vec![
                read.clone(),
                select,
                step("revisit", "ui.revisit", target),
            ]);
            validate_plan(&goal, &read_select_revisit, &pack, &registered()).unwrap();
            read.target = Some(SemanticTargetRef::new("summary-radio-by-week").unwrap());
            let mut foreign_read = read_select_revisit.clone();
            foreign_read.subgoals[0] = read;
            assert!(validate_plan(&goal, &foreign_read, &pack, &registered()).is_err());
        }
    }

    #[test]
    fn frozen_radio_group_goals_reject_wrong_scope_role_value_kind_and_choice() {
        let valid_scope = mvp_form_scope("Summary grouping", "Preferences");
        let valid_draft = mvp_radio_group_draft("Summary grouping", "Preferences", "By month");
        let compile =
            |draft: &GoalDraft, scope: &AllowedControlScope| compile_mvp_scope(draft, scope);

        for wrong_scope in [
            mvp_form_scope("Summary grouping", "Profile"),
            mvp_form_scope("Summary grouping", "Notifications"),
            mvp_scope("Summary grouping", "Preferences", None),
        ] {
            assert!(compile(&valid_draft, &wrong_scope).is_err());
        }

        let mut wrong_role = valid_draft.clone();
        wrong_role.target.role_hint = Some("AXRadioButton".into());
        assert!(compile(&wrong_role, &valid_scope).is_err());

        let mut child_boolean = valid_draft.clone();
        child_boolean.desired = ExpectedValue::Bool { value: true };
        assert!(compile(&child_boolean, &valid_scope).is_err());

        let mut unsupported_choice = valid_draft;
        unsupported_choice.desired = ExpectedValue::Text {
            value: "By title".into(),
        };
        assert!(compile(&unsupported_choice, &valid_scope).is_err());
    }

    #[test]
    fn scoped_form_goals_reject_unregistered_context_type_and_confirmation() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let valid_scope = mvp_form_scope("Display name", "Profile");
        let valid_draft = mvp_form_draft(
            "form-display-name-en",
            "Display name",
            "Profile",
            "AXTextField",
            ExpectedValue::Text {
                value: "Synthetic name".into(),
            },
        );
        let compile = |draft: &GoalDraft, scope: &AllowedControlScope| {
            compile_goal_with_context(
                &RootTaskId::new("scoped-form-invalid").unwrap(),
                "Set the requested fixture form control.",
                draft,
                HostGoalCompileContext::scoped(
                    ExecutionDomain::ForegroundNative,
                    None,
                    scope,
                    "com.lingxi.computer-user.mvp-fixture",
                ),
                budget(),
                std::slice::from_ref(&pack),
            )
        };

        for unsupported in [
            mvp_form_scope("Reminder cadence", "Preferences"),
            mvp_form_scope("Summary grouping", "Profile"),
            mvp_form_scope("Email", "Account"),
            mvp_form_scope("Alert threshold", "Advanced preferences"),
            mvp_form_scope("Display name", "Preferences"),
        ] {
            assert!(compile(&valid_draft, &unsupported).is_err());
        }

        let mut wrong_role = valid_draft.clone();
        wrong_role.target.role_hint = Some("AXTextArea".into());
        assert!(compile(&wrong_role, &valid_scope).is_err());

        let mut wrong_value_kind = valid_draft.clone();
        wrong_value_kind.desired = ExpectedValue::Bool { value: true };
        assert!(compile(&wrong_value_kind, &valid_scope).is_err());

        let mut at_native_byte_limit = valid_draft.clone();
        at_native_byte_limit.desired = ExpectedValue::Text {
            value: "🙂".repeat(MAX_SCOPED_NATIVE_TEXT_BYTES / 4),
        };
        assert!(compile(&at_native_byte_limit, &valid_scope).is_ok());

        let mut empty_text = valid_draft.clone();
        empty_text.desired = ExpectedValue::Text {
            value: String::new(),
        };
        assert!(compile(&empty_text, &valid_scope).is_ok());

        for value in [
            "x".repeat(MAX_SCOPED_NATIVE_TEXT_BYTES + 1),
            "🙂".repeat(MAX_SCOPED_NATIVE_TEXT_BYTES / 4 + 1),
            "a\0b".into(),
            "a\u{1}b".into(),
            "a\u{85}b".into(),
            "first\nsecond".into(),
            "first\rsecond".into(),
            "first\u{2028}second".into(),
        ] {
            let mut invalid_text = valid_draft.clone();
            invalid_text.desired = ExpectedValue::Text { value };
            assert!(compile(&invalid_text, &valid_scope).is_err());
        }

        let notes_scope = mvp_form_scope("Notes", "Profile");
        let notes = mvp_form_draft(
            "form-notes-en",
            "Notes",
            "Profile",
            "AXTextArea",
            ExpectedValue::Text {
                value: "first\r\nsecond\tindented".into(),
            },
        );
        assert!(compile(&notes, &notes_scope).is_ok());

        let mut current_ui = valid_draft.clone();
        current_ui.confirmation_scope = ConfirmationScope::CurrentUi;
        assert!(compile(&current_ui, &valid_scope).is_err());
        let mut commit = valid_draft;
        commit.confirmation_scope = ConfirmationScope::Commit;
        assert!(compile(&commit, &valid_scope).is_err());
    }

    #[test]
    fn numeric_form_goal_keeps_integer_value_and_requires_registered_scroll_round_trip() {
        for (label, form, semantic, container_semantic, container_label) in [
            (
                "Alert threshold",
                "Advanced preferences",
                "form-alert-threshold-en",
                "scroll-preferences",
                "Preferences content",
            ),
            (
                "提醒阈值",
                "高级偏好设置",
                "form-alert-threshold-zh",
                "scroll-preferences-zh",
                "偏好设置内容",
            ),
        ] {
            let scope = mvp_form_scope(label, form);
            let draft = mvp_form_draft(
                semantic,
                label,
                form,
                "AXTextField",
                ExpectedValue::Number { value: 42 },
            );
            let (goal, pack) = compile_mvp_scope(&draft, &scope).unwrap();
            assert_eq!(goal.desired, ExpectedValue::Number { value: 42 });
            let container = scoped_number_form_scroll_container(&goal, &pack).unwrap();
            assert_eq!(container.semantic, container_semantic);
            assert_eq!(container.label, container_label);

            let target = goal.target.semantic_target.as_str();
            let make_scroll = |id: &str| {
                let mut scroll = step(id, "ui.scroll_to", target);
                scroll.scroll = Some(interaction_contracts::desktop::planning::ScrollToSpec {
                    container: TargetSpec {
                        semantic_target: SemanticTargetRef::new(container.semantic.as_str())
                            .unwrap(),
                        label: container.label.clone(),
                        group: None,
                        role_hint: Some(container.role.clone()),
                        row: None,
                    },
                    direction: ScrollDirection::Down,
                    pixels: 48,
                    max_actions: 4,
                });
                scroll
            };
            let plan = bundle(vec![
                make_scroll("scroll-before"),
                step("ensure", "ui.ensure_value", target),
                step("revisit", "ui.revisit", target),
                make_scroll("scroll-after"),
            ]);
            validate_plan(&goal, &plan, &pack, &registered()).unwrap();

            let mut missing_restore = plan.clone();
            missing_restore.subgoals.pop();
            missing_restore.dependencies = missing_restore
                .subgoals
                .windows(2)
                .map(|pair| Dependency {
                    before: pair[0].id.clone(),
                    after: pair[1].id.clone(),
                })
                .collect();
            assert!(validate_plan(&goal, &missing_restore, &pack, &registered()).is_err());

            let mut wrong_container = plan.clone();
            wrong_container.subgoals[3]
                .scroll
                .as_mut()
                .unwrap()
                .container
                .semantic_target = SemanticTargetRef::new("scroll-projects").unwrap();
            assert!(validate_plan(&goal, &wrong_container, &pack, &registered()).is_err());
        }

        let valid_scope = mvp_form_scope("Alert threshold", "Advanced preferences");
        let valid_draft = mvp_form_draft(
            "form-alert-threshold-en",
            "Alert threshold",
            "Advanced preferences",
            "AXTextField",
            ExpectedValue::Number { value: 42 },
        );
        let compile =
            |draft: &GoalDraft, scope: &AllowedControlScope| compile_mvp_scope(draft, scope);
        for wrong_scope in [
            mvp_form_scope("Alert threshold", "Profile"),
            mvp_form_scope("Alert threshold", "Account"),
            mvp_form_scope("Reminder cadence", "Preferences"),
        ] {
            assert!(compile(&valid_draft, &wrong_scope).is_err());
        }
        let mut wrong_role = valid_draft.clone();
        wrong_role.target.role_hint = Some("AXTextArea".into());
        assert!(compile(&wrong_role, &valid_scope).is_err());
        let mut text_downgrade = valid_draft.clone();
        text_downgrade.desired = ExpectedValue::Text { value: "42".into() };
        assert!(compile(&text_downgrade, &valid_scope).is_err());
        for value in [
            MAX_SCOPED_FORM_SAFE_INTEGER + 1,
            -MAX_SCOPED_FORM_SAFE_INTEGER - 1,
        ] {
            let mut out_of_range = valid_draft.clone();
            out_of_range.desired = ExpectedValue::Number { value };
            assert!(compile(&out_of_range, &valid_scope).is_err());
        }
    }

    #[test]
    fn scoped_foreground_does_not_downgrade_unregistered_set_tasks_to_commit() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let scope = mvp_scope("Weekly digest", "Notifications", None);
        assert!(resolve_allowed_scope_app(&scope, std::slice::from_ref(&pack), None).is_ok());
        let mut current_ui = mvp_scoped_draft();
        current_ui.confirmation_scope = ConfirmationScope::CurrentUi;
        assert!(compile_mvp_scope(&current_ui, &scope).is_err());
        let mut commit = mvp_scoped_draft();
        commit.confirmation_scope = ConfirmationScope::Commit;
        commit.max_effect_class = EffectClass::Commit;
        assert!(compile_mvp_scope(&commit, &scope).is_err());

        let unknown = mvp_scope("Invented control", "Search", None);
        assert!(resolve_allowed_scope_app(&unknown, std::slice::from_ref(&pack), None).is_err());
    }

    #[test]
    fn scoped_plan_rejects_revisit_target_drift_and_side_effect_shapes() {
        let scope = browser_scope();
        let (goal, pack) =
            compile_browser_scope(&browser_draft(), &scope, "browser.fixture").unwrap();
        let target = goal.target.semantic_target.as_str();
        let ensure = step("ensure", "ui.ensure_value", target);
        validate_plan(&goal, &bundle(vec![ensure.clone()]), &pack, &registered()).unwrap();

        for mut invalid in [
            step("revisit", "ui.revisit", target),
            step("activate", "ui.activate", target),
            step("foreign", "ui.ensure_value", "another-control"),
            step("targetless", "ui.ensure_value", target),
        ] {
            if invalid.id == "targetless" {
                invalid.target = None;
            }
            assert!(
                validate_plan(&goal, &bundle(vec![invalid]), &pack, &registered()).is_err(),
                "scoped plan accepted an unsafe shape"
            );
        }

        let mut scroll = ensure.clone();
        scroll.scroll = Some(interaction_contracts::desktop::planning::ScrollToSpec {
            container: TargetSpec {
                semantic_target: goal.target.semantic_target.clone(),
                label: goal.target.label.clone(),
                group: None,
                role_hint: Some("AXScrollArea".into()),
                row: None,
            },
            direction: interaction_contracts::desktop::planning::ScrollDirection::Down,
            pixels: 20,
            max_actions: 1,
        });
        assert!(validate_plan(&goal, &bundle(vec![scroll]), &pack, &registered()).is_err());

        let mut select = ensure;
        select.select = Some(
            interaction_contracts::desktop::planning::SelectByLabelSpec {
                choice_label: "Other option".into(),
            },
        );
        assert!(validate_plan(&goal, &bundle(vec![select]), &pack, &registered()).is_err());
    }

    #[test]
    fn a_revisit_goal_requires_the_value_and_the_revisit() {
        let goal = compile(&draft(
            ConfirmationScope::UiRevisit,
            EffectClass::ReversibleEdit,
        ))
        .unwrap();
        let Predicate::And { of } = &goal.required else {
            panic!("expected an and")
        };
        assert_eq!(of.len(), 3);
        assert!(matches!(of[2], Predicate::Revisited { .. }));
        assert!(goal.required.needs_post_action_evidence());
        assert_eq!(
            goal.budget.deadline_ms, 120_000,
            "still a duration until confirmed"
        );
    }

    #[test]
    fn row_goal_freezes_only_selected_true_and_public_registered_scope() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let draft = saved_view_row_draft(
            ExpectedValue::Bool { value: true },
            ConfirmationScope::UiRevisit,
            EffectClass::Navigation,
        );
        let (goal, _) = compile_goal(
            &RootTaskId::new("row-root").unwrap(),
            "select the quarterly review row",
            &draft,
            ExecutionDomain::Simulated,
            budget(),
            &[pack],
        )
        .unwrap();
        let Predicate::And { of } = &goal.required else {
            panic!("a row root goal retains the normal context and confirmation clauses")
        };
        assert_eq!(of.len(), 3);
        assert!(matches!(
            &of[1],
            Predicate::RowSelected {
                target,
                observed_after_action: true
            } if target == &goal.target.semantic_target
        ));
        assert_eq!(goal.target.row, draft.target.row);
        let confirmation = confirmation_for(&goal);
        let ConfirmationKind::Goal { goal: summary } = confirmation.kind else {
            panic!("goal confirmation")
        };
        assert_eq!(summary.row, draft.target.row);
    }

    #[test]
    fn row_goal_rejects_false_desired_or_an_unregistered_static_scope() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let false_goal = saved_view_row_draft(
            ExpectedValue::Bool { value: false },
            ConfirmationScope::UiRevisit,
            EffectClass::Navigation,
        );
        assert!(matches!(
            compile_goal(
                &RootTaskId::new("row-root").unwrap(),
                "select a row",
                &false_goal,
                ExecutionDomain::Simulated,
                budget(),
                std::slice::from_ref(&pack),
            ),
            Err(CompileError::InvalidGoal(_))
        ));

        let mut label_raises_effect = saved_view_row_draft(
            ExpectedValue::Bool { value: true },
            ConfirmationScope::UiRevisit,
            EffectClass::Navigation,
        );
        label_raises_effect.target.label = "Delete this saved view".into();
        assert!(matches!(
            compile_goal(
                &RootTaskId::new("row-root").unwrap(),
                "select a row",
                &label_raises_effect,
                ExecutionDomain::Simulated,
                budget(),
                std::slice::from_ref(&pack),
            ),
            Err(CompileError::InvalidGoal(_))
        ));

        let commit_goal = saved_view_row_draft(
            ExpectedValue::Bool { value: true },
            ConfirmationScope::Commit,
            EffectClass::Navigation,
        );
        assert!(matches!(
            compile_goal(
                &RootTaskId::new("row-root").unwrap(),
                "select a row",
                &commit_goal,
                ExecutionDomain::Simulated,
                budget(),
                std::slice::from_ref(&pack),
            ),
            Err(CompileError::InvalidGoal(_))
        ));

        let mut wrong_scope = saved_view_row_draft(
            ExpectedValue::Bool { value: true },
            ConfirmationScope::UiRevisit,
            EffectClass::Navigation,
        );
        wrong_scope
            .target
            .row
            .as_mut()
            .unwrap()
            .table_scope
            .scroll_path[0]
            .label = "Projects".into();
        assert!(matches!(
            compile_goal(
                &RootTaskId::new("row-root").unwrap(),
                "select a row",
                &wrong_scope,
                ExecutionDomain::Simulated,
                budget(),
                std::slice::from_ref(&pack),
            ),
            Err(CompileError::InvalidGoal(_))
        ));
    }

    #[test]
    fn row_plan_uses_only_the_registered_selection_effect_and_frozen_semantic_target() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let draft = saved_view_row_draft(
            ExpectedValue::Bool { value: true },
            ConfirmationScope::UiRevisit,
            EffectClass::Navigation,
        );
        let (goal, _) = compile_goal(
            &RootTaskId::new("row-root").unwrap(),
            "select a row",
            &draft,
            ExecutionDomain::Simulated,
            budget(),
            std::slice::from_ref(&pack),
        )
        .unwrap();
        let valid = row_plan();
        assert_eq!(validate_plan(&goal, &valid, &pack, &registered()), Ok(()));

        let mut dependency_order_is_authoritative = row_plan();
        dependency_order_is_authoritative.subgoals.reverse();
        assert_eq!(
            validate_plan(
                &goal,
                &dependency_order_is_authoritative,
                &pack,
                &registered()
            ),
            Ok(())
        );

        let select_only = bundle(vec![step("select", "ui.select_row", "saved-view-row")]);
        assert!(matches!(
            validate_plan(&goal, &select_only, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let revisit_only = bundle(vec![step("revisit", "ui.revisit", "saved-view-row")]);
        assert!(matches!(
            validate_plan(&goal, &revisit_only, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let revisit_before_select = bundle(vec![
            step("revisit", "ui.revisit", "saved-view-row"),
            step("select", "ui.select_row", "saved-view-row"),
        ]);
        assert!(matches!(
            validate_plan(&goal, &revisit_before_select, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let mut no_dependency = row_plan();
        no_dependency.dependencies.clear();
        assert!(matches!(
            validate_plan(&goal, &no_dependency, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let duplicate_selection = bundle(vec![
            step("select-one", "ui.select_row", "saved-view-row"),
            step("select-two", "ui.select_row", "saved-view-row"),
            step("revisit", "ui.revisit", "saved-view-row"),
        ]);
        assert!(matches!(
            validate_plan(&goal, &duplicate_selection, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let transitive_revisits = bundle(vec![
            step("select", "ui.select_row", "saved-view-row"),
            step("inspect", "ui.read_value", "saved-view-row"),
            step("revisit-one", "ui.revisit", "saved-view-row"),
            step("revisit-two", "ui.revisit", "saved-view-row"),
        ]);
        assert_eq!(
            validate_plan(&goal, &transitive_revisits, &pack, &registered()),
            Ok(())
        );

        let mut one_unordered_revisit = bundle(vec![
            step("select", "ui.select_row", "saved-view-row"),
            step("revisit-one", "ui.revisit", "saved-view-row"),
            step("revisit-two", "ui.revisit", "saved-view-row"),
        ]);
        one_unordered_revisit.dependencies = vec![Dependency {
            before: "select".into(),
            after: "revisit-one".into(),
        }];
        assert!(matches!(
            validate_plan(&goal, &one_unordered_revisit, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let mut missing_target = valid.clone();
        missing_target.subgoals[0].target = None;
        assert!(matches!(
            validate_plan(&goal, &missing_target, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let wrong_target = bundle(vec![
            step("select", "ui.select_row", "archive-row"),
            step("revisit", "ui.revisit", "saved-view-row"),
        ]);
        assert!(matches!(
            validate_plan(&goal, &wrong_target, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let mut tampered_goal = goal.clone();
        tampered_goal
            .target
            .row
            .as_mut()
            .unwrap()
            .table_scope
            .scroll_path[0]
            .label = "Projects".into();
        assert!(matches!(
            validate_plan(&tampered_goal, &valid, &pack, &registered()),
            Err(PlanRejection::UnknownEffect { .. })
        ));

        let alternate_skill = bundle(vec![
            step("select", "ui.select_row", "saved-view-row"),
            step("activate", "ui.activate", "saved-view-row"),
            step("revisit", "ui.revisit", "saved-view-row"),
        ]);
        assert!(matches!(
            validate_plan(&goal, &alternate_skill, &pack, &registered()),
            Err(PlanRejection::UnknownEffect { .. })
        ));

        let unrelated_profile = AppPack::settings_fixture();
        assert_eq!(
            validate_plan(&goal, &valid, &unrelated_profile, &registered()),
            Err(PlanRejection::UnknownEffect {
                skill: "ui.select_row".into()
            })
        );

        let mut read_only = goal.clone();
        read_only.max_effect_class = EffectClass::ReadOnly;
        assert!(matches!(
            validate_plan(&read_only, &valid, &pack, &registered()),
            Err(PlanRejection::ExceedsGoal {
                skill,
                class: EffectClass::Navigation
            }) if skill == "ui.select_row"
        ));

        let mut scoped_scroll = step("scroll", "ui.scroll_to", "saved-view-row");
        scoped_scroll.scroll = Some(interaction_contracts::desktop::planning::ScrollToSpec {
            container: TargetSpec {
                semantic_target: SemanticTargetRef::new("scroll-saved-views").unwrap(),
                label: "Saved views".into(),
                group: None,
                role_hint: Some("AXScrollArea".into()),
                row: None,
            },
            direction: interaction_contracts::desktop::planning::ScrollDirection::Down,
            pixels: 20,
            max_actions: 2,
        });

        let select = step("select", "ui.select_row", "saved-view-row");
        let revisit = step("revisit", "ui.revisit", "saved-view-row");
        let mut scroll_is_a_dag_prerequisite =
            bundle(vec![select.clone(), scoped_scroll.clone(), revisit.clone()]);
        scroll_is_a_dag_prerequisite.dependencies = vec![
            Dependency {
                before: "scroll".into(),
                after: "select".into(),
            },
            Dependency {
                before: "select".into(),
                after: "revisit".into(),
            },
        ];
        assert_eq!(
            validate_plan(&goal, &scroll_is_a_dag_prerequisite, &pack, &registered()),
            Ok(())
        );

        let mut array_order_is_not_a_dependency =
            bundle(vec![scoped_scroll.clone(), select.clone(), revisit.clone()]);
        array_order_is_not_a_dependency.dependencies = vec![
            Dependency {
                before: "select".into(),
                after: "scroll".into(),
            },
            Dependency {
                before: "select".into(),
                after: "revisit".into(),
            },
        ];
        assert!(matches!(
            validate_plan(
                &goal,
                &array_order_is_not_a_dependency,
                &pack,
                &registered()
            ),
            Err(PlanRejection::Structure(_))
        ));

        let mut scroll_without_dependency =
            bundle(vec![scoped_scroll.clone(), select.clone(), revisit.clone()]);
        scroll_without_dependency.dependencies = vec![Dependency {
            before: "select".into(),
            after: "revisit".into(),
        }];
        assert!(matches!(
            validate_plan(&goal, &scroll_without_dependency, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let mut first_scroll = scoped_scroll.clone();
        first_scroll.id = "scroll-one".into();
        let mut second_scroll = scoped_scroll.clone();
        second_scroll.id = "scroll-two".into();
        let mut multiple_scoped_scrolls = bundle(vec![
            select.clone(),
            second_scroll,
            first_scroll,
            revisit.clone(),
        ]);
        multiple_scoped_scrolls.dependencies = vec![
            Dependency {
                before: "scroll-one".into(),
                after: "scroll-two".into(),
            },
            Dependency {
                before: "scroll-two".into(),
                after: "select".into(),
            },
            Dependency {
                before: "select".into(),
                after: "revisit".into(),
            },
        ];
        assert_eq!(
            validate_plan(&goal, &multiple_scoped_scrolls, &pack, &registered()),
            Ok(())
        );
        multiple_scoped_scrolls
            .dependencies
            .retain(|dependency| dependency.before != "scroll-one");
        assert!(matches!(
            validate_plan(&goal, &multiple_scoped_scrolls, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        let mut other_scroll_target = scoped_scroll.clone();
        other_scroll_target.target = Some(SemanticTargetRef::new("scroll-projects").unwrap());
        assert!(
            pack.control(other_scroll_target.target.as_ref().unwrap())
                .is_some(),
            "the counterexample uses a registered non-goal semantic target"
        );
        let mut other_target_row_plan =
            bundle(vec![other_scroll_target, select.clone(), revisit.clone()]);
        other_target_row_plan.dependencies = vec![
            Dependency {
                before: "scroll".into(),
                after: "select".into(),
            },
            Dependency {
                before: "select".into(),
                after: "revisit".into(),
            },
        ];
        assert!(matches!(
            validate_plan(&goal, &other_target_row_plan, &pack, &registered()),
            Err(PlanRejection::Structure(reason))
                if reason.contains("frozen Goal semantic target")
        ));

        let mut missing_scroll_target =
            bundle(vec![scoped_scroll.clone(), select.clone(), revisit.clone()]);
        missing_scroll_target.subgoals[0].target = None;
        assert!(matches!(
            validate_plan(&goal, &missing_scroll_target, &pack, &registered()),
            Err(PlanRejection::Structure(_))
        ));

        scoped_scroll.scroll.as_mut().unwrap().container.label = "Projects".into();
        let mut wrong_scope_row_plan = bundle(vec![scoped_scroll, select.clone(), revisit.clone()]);
        wrong_scope_row_plan.dependencies = vec![
            Dependency {
                before: "scroll".into(),
                after: "select".into(),
            },
            Dependency {
                before: "select".into(),
                after: "revisit".into(),
            },
        ];
        assert!(matches!(
            validate_plan(
                &goal,
                &wrong_scope_row_plan,
                &pack,
                &registered()
            ),
            Err(PlanRejection::Structure(reason))
                if reason.contains("registered nearest scroll container")
        ));
    }

    #[test]
    fn a_navigation_observation_cannot_weaken_root_goal_confirmation() {
        assert!(matches!(
            compile(&draft(
                ConfirmationScope::CurrentUi,
                EffectClass::ReversibleEdit
            )),
            Err(CompileError::InvalidGoal(_))
        ));
    }

    #[test]
    fn a_commit_goal_requires_commit_evidence_instead() {
        let goal = compile(&draft(ConfirmationScope::Commit, EffectClass::Commit)).unwrap();
        let Predicate::And { of } = &goal.required else {
            panic!("expected an and")
        };
        assert!(matches!(of[2], Predicate::CommitConfirmed { .. }));
    }

    #[test]
    fn an_app_without_a_pack_cannot_be_executed() {
        let mut elsewhere = draft(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        elsewhere.app = "com.other.app".into();
        assert_eq!(
            compile(&elsewhere),
            Err(CompileError::AppNotExecutable("com.other.app".into()))
        );
    }

    #[test]
    fn menu_plan_cannot_change_frozen_choice_or_invent_unregistered_menu_effects() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.guard-fixture").unwrap();
        let menu_draft = GoalDraft {
            app: pack.bundle_id.clone(),
            account: None,
            target: TargetSpec {
                semantic_target: SemanticTargetRef::new("menu-control").unwrap(),
                label: "Fixture native menu".into(),
                group: Some("Menu fixture group".into()),
                role_hint: Some("AXMenuButton".into()),
                row: None,
            },
            desired: ExpectedValue::Text {
                value: "Fixture menu second item".into(),
            },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::ReversibleEdit,
        };
        let (goal, _) = compile_goal(
            &RootTaskId::new("menu-plan").unwrap(),
            "select menu",
            &menu_draft,
            ExecutionDomain::ForegroundNative,
            budget(),
            std::slice::from_ref(&pack),
        )
        .unwrap();
        let mut selection = step("select", "ui.select_by_label", "menu-control");
        selection.select = Some(
            interaction_contracts::desktop::planning::SelectByLabelSpec {
                choice_label: "Fixture menu second item".into(),
            },
        );
        let plan = bundle(vec![selection.clone()]);
        let registered = vec!["ui.select_by_label".into()];
        assert!(validate_plan(&goal, &plan, &pack, &registered).is_ok());
        selection.select.as_mut().unwrap().choice_label = "Fixture menu first item".into();
        assert!(validate_plan(&goal, &bundle(vec![selection]), &pack, &registered).is_err());
        let mut bad_goal = goal.clone();
        bad_goal.target.role_hint = Some("AXButton".into());
        assert!(validate_plan(&bad_goal, &plan, &pack, &registered).is_err());
        let mut bad_pack = pack;
        bad_pack
            .rules
            .retain(|rule| rule.role.as_deref() != Some("AXMenuItem"));
        assert!(validate_plan(&goal, &plan, &bad_pack, &registered).is_err());
        assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
    }

    #[test]
    fn goals_beyond_phase_one_are_refused() {
        assert!(matches!(
            compile(&draft(ConfirmationScope::UiRevisit, EffectClass::External)),
            Err(CompileError::InvalidGoal(_))
        ));
        assert!(matches!(
            compile(&draft(ConfirmationScope::UiRevisit, EffectClass::Unknown)),
            Err(CompileError::InvalidGoal(_))
        ));
    }

    #[test]
    fn the_confirmation_shows_what_the_user_is_agreeing_to() {
        let goal = compile(&draft(
            ConfirmationScope::UiRevisit,
            EffectClass::ReversibleEdit,
        ))
        .unwrap();
        let request = confirmation_for(&goal);
        let ConfirmationKind::Goal { goal: summary } = request.kind else {
            panic!("a goal confirmation")
        };
        assert_eq!(summary.target_label, "Daily picks");
        assert_eq!(summary.group.as_deref(), Some("Push"));
        assert_eq!(summary.max_effect_class, EffectClass::ReversibleEdit);
        assert_eq!(request.confirmation_id.as_str(), "conf-root-1-r1");
    }

    #[test]
    fn a_plan_inside_the_goal_is_accepted() {
        let goal = compile(&draft(
            ConfirmationScope::UiRevisit,
            EffectClass::ReversibleEdit,
        ))
        .unwrap();
        let plan = bundle(vec![
            step("a", "ui.ensure_value", "push/daily-picks"),
            step("b", "ui.revisit", "push/daily-picks"),
        ]);
        assert_eq!(
            validate_plan(&goal, &plan, &AppPack::settings_fixture(), &registered()),
            Ok(())
        );
    }

    #[test]
    fn a_plan_cannot_widen_the_effect_class_of_the_goal() {
        let goal = compile(&draft(
            ConfirmationScope::UiRevisit,
            EffectClass::ReversibleEdit,
        ))
        .unwrap();
        let pack = AppPack::settings_fixture();
        let apply = bundle(vec![step("a", "ui.activate", "apply-button")]);
        assert_eq!(
            validate_plan(&goal, &apply, &pack, &registered()),
            Err(PlanRejection::ExceedsGoal {
                skill: "ui.activate".into(),
                class: EffectClass::Commit
            })
        );
        let reset = bundle(vec![step("a", "ui.activate", "reset-button")]);
        assert!(matches!(
            validate_plan(&goal, &reset, &pack, &registered()),
            Err(PlanRejection::ExceedsGoal {
                class: EffectClass::Destructive,
                ..
            })
        ));
        // The same activation is fine once the goal includes commits.
        let commit_goal = compile(&draft(ConfirmationScope::Commit, EffectClass::Commit)).unwrap();
        assert_eq!(
            validate_plan(&commit_goal, &apply, &pack, &registered()),
            Ok(())
        );
    }

    #[test]
    fn unregistered_skills_and_unclassifiable_steps_are_refused() {
        let goal = compile(&draft(
            ConfirmationScope::UiRevisit,
            EffectClass::ReversibleEdit,
        ))
        .unwrap();
        let pack = AppPack::settings_fixture();
        let unknown_skill = bundle(vec![step("a", "shell.run", "x")]);
        assert_eq!(
            validate_plan(&goal, &unknown_skill, &pack, &registered()),
            Err(PlanRejection::UnregisteredSkill("shell.run".into()))
        );
        let mystery = bundle(vec![step("a", "ui.activate", "frobnicator")]);
        assert_eq!(
            validate_plan(&goal, &mystery, &pack, &registered()),
            Err(PlanRejection::UnknownEffect {
                skill: "ui.activate".into()
            })
        );
    }

    #[test]
    fn a_structurally_broken_plan_is_refused() {
        let goal = compile(&draft(
            ConfirmationScope::UiRevisit,
            EffectClass::ReversibleEdit,
        ))
        .unwrap();
        let empty = bundle(vec![]);
        assert!(matches!(
            validate_plan(&goal, &empty, &AppPack::settings_fixture(), &registered()),
            Err(PlanRejection::Structure(_))
        ));
    }
}
