//! GoalCompiler and PlanInstaller: pure checks that turn a planner's draft
//! into a goal the user can confirm, and refuse plans that reach beyond it
//! (design 12.5 and 13.3).

use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{ConfirmationId, RootTaskId};
use interaction_contracts::desktop::operator::{
    ConfirmationKind, ConfirmationRequest, GoalSummary,
};
use interaction_contracts::desktop::planning::{GoalDraft, PlanBundle};
use interaction_contracts::desktop::task::{
    ConfirmationScope, GoalSpec, Predicate, RootBudgetSpec, TaskScope,
};

use crate::environment::pack::AppPack;

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

/// The effect class a skill has by itself, before the control it acts on is known.
pub fn skill_class(skill: &str) -> Option<EffectClass> {
    match skill {
        "ui.read_value" => Some(EffectClass::ReadOnly),
        "ui.scroll_to" | "ui.activate_window" | "ui.revisit" => Some(EffectClass::Navigation),
        "ui.ensure_value" | "ui.enter_text" => Some(EffectClass::ReversibleEdit),
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
    for subgoal in &bundle.subgoals {
        if !registered.contains(&subgoal.skill) {
            return Err(PlanRejection::UnregisteredSkill(subgoal.skill.clone()));
        }
        let base = skill_class(&subgoal.skill)
            .ok_or_else(|| PlanRejection::UnregisteredSkill(subgoal.skill.clone()))?;
        // An activation is only as harmless as the control it presses.
        let class = if subgoal.skill == "ui.activate" {
            let label = subgoal
                .target
                .as_ref()
                .map(|target| target.as_str().replace(['-', '_', '/'], " "))
                .unwrap_or_default();
            pack.effect_class_for_label(&label)
        } else {
            base
        };
        if class == EffectClass::Unknown {
            return Err(PlanRejection::UnknownEffect {
                skill: subgoal.skill.clone(),
            });
        }
        if class > goal.max_effect_class {
            return Err(PlanRejection::ExceedsGoal {
                skill: subgoal.skill.clone(),
                class,
            });
        }
    }
    Ok(())
}

/// Build the goal from a planner's draft. `budget.deadline_ms` of the result
/// is a duration; it becomes an absolute time when the user confirms.
pub fn compile_goal(
    root: &RootTaskId,
    user_text: &str,
    draft: &GoalDraft,
    default_budget: RootBudgetSpec,
    packs: &[AppPack],
) -> Result<(GoalSpec, AppPack), CompileError> {
    let pack = packs
        .iter()
        .find(|pack| pack.bundle_id == draft.app)
        .ok_or_else(|| CompileError::AppNotExecutable(draft.app.clone()))?
        .clone();
    let target = draft.target.semantic_target.clone();
    let mut checks = vec![
        Predicate::ContextMatches {
            app: draft.app.clone(),
        },
        Predicate::ReadValueEquals {
            target: target.clone(),
            expected: draft.desired.clone(),
            observed_after_action: true,
        },
    ];
    checks.push(match draft.confirmation_scope {
        ConfirmationScope::UiRevisit => Predicate::Revisited { target },
        ConfirmationScope::Commit => Predicate::CommitConfirmed { target },
    });
    let goal = GoalSpec {
        root_task_id: root.clone(),
        task_revision: 1,
        user_text: user_text.to_string(),
        scope: TaskScope {
            app: draft.app.clone(),
            account: draft.account.clone(),
        },
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
            goal: GoalSummary {
                user_text: goal.user_text.clone(),
                app: goal.scope.app.clone(),
                target_label: goal.target.label.clone(),
                group: goal.target.group.clone(),
                desired: goal.desired.clone(),
                confirmation_scope: goal.confirmation_scope,
                max_effect_class: goal.max_effect_class,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::ids::SemanticTargetRef;
    use interaction_contracts::desktop::planning::{Dependency, Subgoal};
    use interaction_contracts::desktop::task::{ExpectedValue, TargetSpec};

    fn registered() -> Vec<String> {
        [
            "ui.read_value",
            "ui.ensure_value",
            "ui.revisit",
            "ui.activate",
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
            },
            desired: ExpectedValue::Bool { value: false },
            confirmation_scope: scope,
            max_effect_class: max,
        }
    }

    fn compile(draft: &GoalDraft) -> Result<GoalSpec, CompileError> {
        compile_goal(
            &RootTaskId::new("root-1").unwrap(),
            "turn it off",
            draft,
            budget(),
            &[AppPack::settings_fixture()],
        )
        .map(|(goal, _)| goal)
    }

    fn step(id: &str, skill: &str, target: &str) -> Subgoal {
        Subgoal {
            id: id.into(),
            description: id.into(),
            skill: skill.into(),
            target: Some(SemanticTargetRef::new(target).unwrap()),
            expects: vec![],
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
