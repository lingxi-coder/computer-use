//! Deterministic planner for the single trusted headless browser fixture.
//!
//! The caller supplies the Host-frozen closed profile after checking the
//! current Device capability. This planner cannot select a page, domain, or
//! permission from user text.

use interaction_contracts::desktop::browser::BrowserPageProfile;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::SemanticTargetRef;
use interaction_contracts::desktop::planning::{
    GoalDraft, PlanBundle, PlanProposal, PlannerOutput, PlanningReason, PlanningRequest, Subgoal,
};
use interaction_contracts::desktop::task::{ConfirmationScope, ExpectedValue, TargetSpec};

const ENABLE_FIXTURE_CHECKBOX: &str = "Enable browser fixture checkbox";

pub(crate) fn plan(request: &PlanningRequest, profile: BrowserPageProfile) -> PlannerOutput {
    if request.reason != PlanningReason::GoalCompile
        || request.context.user_text != ENABLE_FIXTURE_CHECKBOX
    {
        return PlannerOutput::Review {
            proposal: interaction_contracts::desktop::planning::ReviewProposal {
                notes: vec![
                    "browser reference supports only its fixed checkbox fixture task".into(),
                ],
                evidence: vec![],
            },
        };
    }

    let target = SemanticTargetRef::new(profile.semantic_target()).expect("closed profile id");
    PlannerOutput::Plan {
        proposal: Box::new(PlanProposal {
            goal_draft: Some(GoalDraft {
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
            }),
            bundle: PlanBundle {
                subgoals: vec![Subgoal {
                    id: "ensure".into(),
                    description: "enable the registered browser fixture checkbox".into(),
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
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
    use interaction_contracts::desktop::planning::PlanningContext;

    fn request(text: &str, reason: PlanningReason) -> PlanningRequest {
        PlanningRequest {
            request_id: RequestId::new("browser-reference").unwrap(),
            generation: 1,
            root_task_id: RootTaskId::new("root-browser-reference").unwrap(),
            task_revision: 1,
            root_epoch: 1,
            reason,
            context: PlanningContext {
                user_text: text.into(),
                allowed_scope: None,
                unknown_items: vec![],
                allowed_skills: vec![],
                recent_failures: vec![],
                notes: vec![],
            },
            deadline_ms: 1_000,
        }
    }

    #[test]
    fn fixed_fixture_task_compiles_one_boolean_current_ui_step() {
        let PlannerOutput::Plan { proposal } = plan(
            &request(ENABLE_FIXTURE_CHECKBOX, PlanningReason::GoalCompile),
            BrowserPageProfile::FixtureBackgroundOption,
        ) else {
            panic!("the one supported browser fixture task should compile")
        };
        let goal = proposal.goal_draft.as_ref().unwrap();
        assert_eq!(goal.app, "browser.fixture");
        assert_eq!(
            goal.target.semantic_target.as_str(),
            "browser.fixture.background_option"
        );
        assert_eq!(goal.target.label, "Background option");
        assert_eq!(goal.target.role_hint.as_deref(), Some("checkbox"));
        assert_eq!(goal.desired, ExpectedValue::Bool { value: true });
        assert_eq!(goal.confirmation_scope, ConfirmationScope::CurrentUi);
        assert_eq!(proposal.bundle.subgoals.len(), 1);
        assert_eq!(proposal.bundle.subgoals[0].skill, "ui.ensure_value");
        assert!(proposal.bundle.dependencies.is_empty());
        assert!(proposal.bundle.validate().is_ok());
    }

    #[test]
    fn browser_reference_does_not_expand_to_other_user_text_or_planning_phases() {
        for request in [
            request(
                "Enable browser fixture checkbox and publish it",
                PlanningReason::GoalCompile,
            ),
            request(ENABLE_FIXTURE_CHECKBOX, PlanningReason::Escalation),
        ] {
            assert!(matches!(
                plan(&request, BrowserPageProfile::FixtureBackgroundOption),
                PlannerOutput::Review { .. }
            ));
        }
    }
}
