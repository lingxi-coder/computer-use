//! Explicit owned-document plumbing diagnostic. It exercises ordinary task
//! admission; it does not stand in for teacher/model or revisit acceptance.
use interaction_contracts::desktop::{
    effect::EffectClass,
    ids::SemanticTargetRef,
    owned_input::OwnedDocumentBindingProof,
    planning::{
        GoalDraft, PlanBundle, PlanProposal, PlannerOutput, PlanningReason, PlanningRequest,
        ReviewProposal, Subgoal,
    },
    task::{ConfirmationScope, ExpectedValue, TargetSpec},
};

pub fn plan(request: &PlanningRequest, allow_owned_document_save: bool) -> PlannerOutput {
    let command = request.context.user_text.as_str();
    let (value, fill, save) = if let Some(value) = command.strip_prefix("Fill owned document: ") {
        (value, true, false)
    } else if let Some(value) = command.strip_prefix("Fill and save owned document: ") {
        (value, true, true)
    } else if let Some(value) = command.strip_prefix("Save owned document: ") {
        (value, false, true)
    } else {
        return review();
    };
    let value_valid = if save && !fill {
        valid_save_value(value)
    } else {
        OwnedDocumentBindingProof::validate_setter_value(value).is_ok()
    };
    if request.reason != PlanningReason::GoalCompile
        || (save && !allow_owned_document_save)
        || !value_valid
    {
        return review();
    }
    let target = SemanticTargetRef::new("owned-document").expect("constant id");
    let mut subgoals = Vec::new();
    if fill {
        subgoals.push(Subgoal {
            id: "fill-owned".into(),
            description: "enter the frozen text in the owned document".into(),
            skill: "ui.enter_text".into(),
            target: Some(target.clone()),
            expects: vec!["value".into()],
            scroll: None,
            select: None,
        });
    }
    if save {
        subgoals.push(Subgoal {
            id: "save-owned".into(),
            description: "save the owned document after checking its text".into(),
            skill: "ui.save_document".into(),
            target: Some(target.clone()),
            // A Save receipt is input history, not evidence of a persisted file.
            expects: vec![],
            scroll: None,
            select: None,
        });
    }
    PlannerOutput::Plan {
        proposal: Box::new(PlanProposal {
            goal_draft: Some(GoalDraft {
                app: "com.apple.TextEdit".into(),
                account: None,
                target: TargetSpec {
                    row: None,
                    semantic_target: target,
                    label: "Owned document".into(),
                    group: None,
                    role_hint: Some("AXTextArea".into()),
                },
                desired: ExpectedValue::Text {
                    value: value.into(),
                },
                // SetValue and Save receipts cannot replace independent revisit evidence.
                confirmation_scope: ConfirmationScope::UiRevisit,
                max_effect_class: EffectClass::ReversibleEdit,
            }),
            bundle: PlanBundle {
                subgoals,
                dependencies: vec![],
                required_observations: vec![],
                expected_effects: vec!["goal".into()],
                recovery_branches: vec![],
                termination: vec!["goal.required".into()],
            },
        }),
    }
}

fn review() -> PlannerOutput {
    PlannerOutput::Review {
        proposal: ReviewProposal {
            notes: vec![
                "owned reference supports only explicit bounded text-entry diagnostics".into(),
            ],
            evidence: vec![],
        },
    }
}

fn valid_save_value(value: &str) -> bool {
    !value.contains('\0') && value.len() <= 2048
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::{
        ids::{RequestId, RootTaskId},
        planning::PlanningContext,
    };
    fn request(text: &str) -> PlanningRequest {
        PlanningRequest {
            request_id: RequestId::new("owned-reference").unwrap(),
            generation: 1,
            root_task_id: RootTaskId::new("root").unwrap(),
            task_revision: 1,
            root_epoch: 1,
            reason: PlanningReason::GoalCompile,
            deadline_ms: 1000,
            context: PlanningContext {
                user_text: text.into(),
                allowed_scope: None,
                unknown_items: vec![],
                allowed_skills: vec![],
                recent_failures: vec![],
                notes: vec![],
            },
        }
    }
    #[test]
    fn freezes_literal_text_and_retains_independent_root_revisit_requirement() {
        for value in ["", "中文\r\n👩‍👩‍👧‍👦\te\u{301}", &"x".repeat(2048)] {
            let PlannerOutput::Plan { proposal } =
                plan(&request(&format!("Fill owned document: {value}")), false)
            else {
                panic!("expected bounded plan")
            };
            let goal = proposal.goal_draft.unwrap();
            assert_eq!(goal.app, "com.apple.TextEdit");
            assert_eq!(
                goal.desired,
                ExpectedValue::Text {
                    value: value.into()
                }
            );
            assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
            assert_eq!(goal.max_effect_class, EffectClass::ReversibleEdit);
            assert!(proposal.bundle.validate().is_ok());
            assert_eq!(proposal.bundle.subgoals.len(), 1);
            assert_eq!(proposal.bundle.subgoals[0].skill, "ui.enter_text");
        }
    }
    #[test]
    fn rejects_other_commands_oversize_control_characters_and_recovery() {
        for text in [
            "Fill document: text",
            "Save owned document",
            "Fill owned document: bad\0",
            &format!("Fill owned document: {}", "x".repeat(2049)),
            &format!("Fill owned document: {}", "中".repeat(683)),
        ] {
            assert!(matches!(
                plan(&request(text), true),
                PlannerOutput::Review { .. }
            ));
        }
        let mut recovery = request("Fill owned document: allowed");
        recovery.reason = PlanningReason::Escalation;
        assert!(matches!(
            plan(&recovery, true),
            PlannerOutput::Review { .. }
        ));
    }

    #[test]
    fn save_commands_need_private_opt_in_and_keep_revisit_as_the_root_scope() {
        let value = "frozen saved text\n";
        for command in [
            format!("Fill and save owned document: {value}"),
            format!("Save owned document: {value}"),
        ] {
            assert!(matches!(
                plan(&request(&command), false),
                PlannerOutput::Review { .. }
            ));
            let PlannerOutput::Plan { proposal } = plan(&request(&command), true) else {
                panic!("private Save opt-in should produce a diagnostic plan")
            };
            let goal = proposal.goal_draft.unwrap();
            assert_eq!(
                goal.desired,
                ExpectedValue::Text {
                    value: value.into()
                }
            );
            assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
            assert_eq!(
                proposal.bundle.subgoals.last().unwrap().skill,
                "ui.save_document"
            );
            assert!(proposal.bundle.validate().is_ok());
        }

        let PlannerOutput::Plan { proposal } = plan(
            &request(&format!("Fill and save owned document: {value}")),
            true,
        ) else {
            panic!("expected a fill-then-save plan")
        };
        assert_eq!(proposal.bundle.subgoals.len(), 2);
        assert_eq!(proposal.bundle.subgoals[0].skill, "ui.enter_text");
        assert_eq!(proposal.bundle.subgoals[1].skill, "ui.save_document");
        assert!(proposal.bundle.subgoals[1].expects.is_empty());
    }

    #[test]
    fn save_only_accepts_non_nul_controls_but_fill_and_save_keeps_setter_policy() {
        let value = "before\u{0001}after";
        let PlannerOutput::Plan { proposal } =
            plan(&request(&format!("Save owned document: {value}")), true)
        else {
            panic!("Save-only accepts bounded NUL-free controls")
        };
        assert_eq!(proposal.bundle.subgoals.len(), 1);
        assert_eq!(proposal.bundle.subgoals[0].skill, "ui.save_document");

        assert!(matches!(
            plan(
                &request(&format!("Fill and save owned document: {value}")),
                true
            ),
            PlannerOutput::Review { .. }
        ));
        assert!(matches!(
            plan(&request("Save owned document: bad\0value"), true),
            PlannerOutput::Review { .. }
        ));
        assert!(matches!(
            plan(
                &request(&format!("Save owned document: {}", "x".repeat(2049))),
                true
            ),
            PlannerOutput::Review { .. }
        ));
    }
}
