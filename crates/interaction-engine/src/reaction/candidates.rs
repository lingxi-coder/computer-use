//! CandidateFactory: the finite set of actions a decision may choose from
//! (design 9.2 to 9.4).
//!
//! Candidates come from the plan's current step, the current bindings and the
//! layer that receives input. Each carries an effect class and a precondition
//! status decided by code, so a backend can only pick among actions that were
//! already allowed to be considered. The observe, plan and stop paths are
//! always present and are never crowded out.

use std::collections::{BTreeMap, BTreeSet};

use interaction_contracts::desktop::binding::{BindCandidate, normalize_label};
use interaction_contracts::desktop::decision::{Candidate, Precondition};
use interaction_contracts::desktop::effect::{
    Admission, DenyReason, EffectClass, EffectContext, admit,
};
use interaction_contracts::desktop::ids::{CandidateId, SemanticTargetRef};
use interaction_contracts::desktop::planning::Subgoal;
use interaction_contracts::desktop::source::{LayerKind, UiObject};
use interaction_contracts::desktop::task::GoalSpec;

use crate::environment::binding::{Binding, BindingReducer};
use crate::environment::pack::AppPack;
use crate::environment::read::{object_for, value_text};
use crate::environment::state_core::EnvSnapshot;
use crate::tasks::goal::skill_class;

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
    Bound(Binding),
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

/// The effect gate of 9.3 for one action. Candidate generation and admission
/// both call this, so they can never disagree.
pub fn gate(
    scope: &Scope<'_>,
    class: EffectClass,
    target: Option<&SemanticTargetRef>,
    confirmed: bool,
) -> Admission {
    let goal_target = &scope.goal.target.semantic_target;
    let named = target.is_some_and(|target| target == goal_target);
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

/// Whether the window that receives input is a dialog or sheet.
pub fn modal_in_front(env: &EnvSnapshot) -> bool {
    let Some(active) = env.snapshot.active_layer.as_deref() else {
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
    let active = env.snapshot.active_layer.as_deref().unwrap_or("");
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
    let Some(active) = env.snapshot.active_layer.as_deref() else {
        return Vec::new();
    };
    let mut controls = Vec::new();
    for object in &env.snapshot.objects {
        if object.window_id != active
            || !object.enabled
            || !object.in_view
            || object.secure
            || object.role != "button"
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
    binding: Option<Binding>,
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
        target: subgoal.target.clone(),
        binding: binding.cloned(),
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
        Basis::Step {
            subgoal,
            resolution,
        } => match resolution {
            Resolution::Bound(binding) => {
                match object_for(scope.env, &binding.fingerprint_digest) {
                    Some(object) => drafts.push(step_draft(scope, subgoal, Some(binding), object)),
                    None => drafts.push(Draft {
                        skill: subgoal.skill.clone(),
                        target: subgoal.target.clone(),
                        binding: Some(binding.clone()),
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
            Resolution::Ambiguous(options) => {
                for option in options {
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
            Resolution::Unavailable(reason) => drafts.push(Draft {
                skill: subgoal.skill.clone(),
                target: subgoal.target.clone(),
                binding: None,
                parameters: BTreeMap::new(),
                class: skill_class(&subgoal.skill).unwrap_or(EffectClass::Unknown),
                needs_observation: Some(reason.clone()),
                description: format!("{} (waiting for evidence: {reason})", subgoal.description),
            }),
        },
        Basis::Layer { title, controls } => {
            for layer in *controls {
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
                    binding: Some(layer.binding.clone()),
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
                binding: draft.binding.map(|binding| binding.binding_ref),
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
    use interaction_contracts::desktop::task::ConfirmationScope;
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
        let resolution = Resolution::Bound(binding.clone());
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
        let resolution = Resolution::Bound(env_binding);
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
        env.snapshot.app.bundle_id = "some.other.app".into();
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
        let resolution = Resolution::Bound(binding);
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
            id: "x".into(),
            description: "press it".into(),
            skill: "ui.activate".into(),
            target: Some(SemanticTargetRef::new("mystery").unwrap()),
            expects: vec![],
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
