//! Owned-fixture reference planner for exercising the full host pipeline.
//! Explicitly selected by the trusted launcher; it is never a model fallback.
use interaction_contracts::desktop::{
    effect::EffectClass,
    ids::SemanticTargetRef,
    planning::{
        Dependency, GoalDraft, PlanBundle, PlanProposal, PlannerOutput, PlanningReason,
        PlanningRequest, ReviewProposal, Subgoal,
    },
    task::{ConfirmationScope, ExpectedValue, TargetSpec},
};
use interaction_engine::environment::pack::AppPack;

const ROW_FIXTURE_APP: &str = "com.lingxi.computer-user.mvp-fixture";

pub fn plan(request: &PlanningRequest) -> PlannerOutput {
    if request.reason != PlanningReason::GoalCompile {
        return PlannerOutput::Review {
            proposal: ReviewProposal {
                notes: vec!["reference planner has no recovery plan".into()],
                evidence: vec![],
            },
        };
    }
    if request.context.user_text == "Reveal fixture scroll destination" {
        return scroll_plan();
    }
    if request.context.user_text == "Enable background fixture toggle" {
        return background_plan();
    }
    if request.context.user_text == "Select fixture menu second item" {
        return menu_plan();
    }
    if let Some(target_json) = request
        .context
        .user_text
        .strip_prefix("Select fixture row: ")
    {
        return row_plan(target_json);
    }
    let value = request
        .context
        .user_text
        .strip_prefix("Fill document: ")
        .filter(|value| {
            value.len() <= 2048
                && !value
                    .chars()
                    .any(|c| c.is_control() && !['\r', '\n', '\t'].contains(&c))
        });
    let keyboard = request.context.user_text == "Enable fixture keyboard toggle";
    let toggle = keyboard || request.context.user_text == "Enable fixture toggle";
    let target = SemanticTargetRef::new(if keyboard {
        "keyboard-toggle"
    } else if toggle {
        "toggle"
    } else {
        "document"
    })
    .expect("constant id");
    let goal_draft = (toggle || value.is_some()).then(|| GoalDraft {
        app: "com.lingxi.computer-user.guard-fixture".into(),
        account: None,
        target: TargetSpec {
            row: None,
            semantic_target: target.clone(),
            label: if keyboard {
                "Key fixture toggle"
            } else if toggle {
                "MVP toggle"
            } else {
                "MVP document"
            }
            .into(),
            group: Some(
                if keyboard {
                    "Key fixture group"
                } else {
                    "Fixture group"
                }
                .into(),
            ),
            role_hint: Some(if toggle { "AXCheckBox" } else { "AXTextField" }.into()),
        },
        desired: if toggle {
            ExpectedValue::Bool { value: true }
        } else {
            ExpectedValue::Text {
                value: value.expect("validated text").into(),
            }
        },
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::ReversibleEdit,
    });
    let subgoals = if goal_draft.is_some() {
        vec![
            Subgoal {
                select: None,
                id: if toggle { "ensure" } else { "fill" }.into(),
                description: if toggle {
                    "enable the registered toggle"
                } else {
                    "enter the specified text"
                }
                .into(),
                skill: if toggle {
                    "ui.ensure_value"
                } else {
                    "ui.enter_text"
                }
                .into(),
                target: Some(target.clone()),
                expects: vec!["value".into()],
                scroll: None,
            },
            Subgoal {
                select: None,
                id: "revisit".into(),
                description: "leave and return to verify the document".into(),
                skill: "ui.revisit".into(),
                target: Some(target),
                expects: vec!["revisited".into()],
                scroll: None,
            },
        ]
    } else {
        vec![]
    };
    PlannerOutput::Plan {
        proposal: Box::new(PlanProposal {
            goal_draft,
            bundle: PlanBundle {
                dependencies: if subgoals.is_empty() {
                    vec![]
                } else {
                    vec![Dependency {
                        before: if toggle { "ensure" } else { "fill" }.into(),
                        after: "revisit".into(),
                    }]
                },
                subgoals,
                required_observations: vec![],
                expected_effects: vec!["goal".into()],
                recovery_branches: vec![],
                termination: vec!["goal.required".into()],
            },
        }),
    }
}

/// One explicit owned diagnostic, with no hidden evaluator state in the plan.
fn background_plan() -> PlannerOutput {
    let target = SemanticTargetRef::new("background-toggle").expect("constant id");
    PlannerOutput::Plan {
        proposal: Box::new(PlanProposal {
            goal_draft: Some(GoalDraft {
                app: "com.lingxi.computer-user.guard-fixture".into(),
                account: None,
                target: TargetSpec {
                    row: None,
                    semantic_target: target.clone(),
                    label: "Background toggle".into(),
                    group: None,
                    role_hint: Some("AXCheckBox".into()),
                },
                desired: ExpectedValue::Bool { value: true },
                confirmation_scope: ConfirmationScope::CurrentUi,
                max_effect_class: EffectClass::ReversibleEdit,
            }),
            bundle: PlanBundle {
                subgoals: vec![Subgoal {
                    select: None,
                    id: "ensure".into(),
                    description: "enable the registered background toggle".into(),
                    skill: "ui.ensure_value".into(),
                    target: Some(target),
                    expects: vec!["value".into()],
                    scroll: None,
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

fn row_plan(target_json: &str) -> PlannerOutput {
    let target = serde_json::from_str::<TargetSpec>(target_json)
        .ok()
        .filter(|target| target.validate().is_ok())
        .filter(|target| target.row.is_some())
        .filter(|target| {
            AppPack::registered_native(ROW_FIXTURE_APP).and_then(|pack| {
                pack.row_selection_effect_class(
                    &target.semantic_target,
                    target.row.as_ref()?,
                    &target.label,
                )
            }) == Some(EffectClass::Navigation)
        });

    let goal_draft = target.as_ref().map(|target| GoalDraft {
        app: ROW_FIXTURE_APP.into(),
        account: None,
        target: target.clone(),
        desired: ExpectedValue::Bool { value: true },
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::Navigation,
    });
    let subgoals = target
        .as_ref()
        .map(|target| {
            vec![
                Subgoal {
                    select: None,
                    id: "select".into(),
                    description: "select the uniquely matched fixture row".into(),
                    skill: "ui.select_row".into(),
                    target: Some(target.semantic_target.clone()),
                    expects: vec!["selected".into()],
                    scroll: None,
                },
                Subgoal {
                    select: None,
                    id: "revisit".into(),
                    description: "leave and return to verify the selected row".into(),
                    skill: "ui.revisit".into(),
                    target: Some(target.semantic_target.clone()),
                    expects: vec!["goal".into()],
                    scroll: None,
                },
            ]
        })
        .unwrap_or_default();
    PlannerOutput::Plan {
        proposal: Box::new(PlanProposal {
            goal_draft,
            bundle: PlanBundle {
                dependencies: if subgoals.is_empty() {
                    vec![]
                } else {
                    vec![Dependency {
                        before: "select".into(),
                        after: "revisit".into(),
                    }]
                },
                subgoals,
                required_observations: vec![],
                expected_effects: vec!["goal".into()],
                recovery_branches: vec![],
                termination: vec!["goal.required".into()],
            },
        }),
    }
}

fn menu_plan() -> PlannerOutput {
    let target = SemanticTargetRef::new("menu-control").unwrap();
    let choice = "Fixture menu second item";
    PlannerOutput::Plan {
        proposal: Box::new(PlanProposal {
            goal_draft: Some(GoalDraft {
                app: "com.lingxi.computer-user.guard-fixture".into(),
                account: None,
                target: TargetSpec {
                    row: None,
                    semantic_target: target.clone(),
                    label: "Fixture native menu".into(),
                    group: Some("Menu fixture group".into()),
                    role_hint: Some("AXMenuButton".into()),
                },
                desired: ExpectedValue::Text {
                    value: choice.into(),
                },
                confirmation_scope: ConfirmationScope::UiRevisit,
                max_effect_class: EffectClass::ReversibleEdit,
            }),
            bundle: PlanBundle {
                subgoals: vec![
                    Subgoal {
                        id: "select".into(),
                        description: "select the registered second menu item".into(),
                        skill: "ui.select_by_label".into(),
                        target: Some(target.clone()),
                        expects: vec!["selected_value".into()],
                        scroll: None,
                        select: Some(
                            interaction_contracts::desktop::planning::SelectByLabelSpec {
                                choice_label: choice.into(),
                            },
                        ),
                    },
                    Subgoal {
                        id: "revisit".into(),
                        description: "leave and return to verify the menu value".into(),
                        skill: "ui.revisit".into(),
                        target: Some(target),
                        expects: vec!["goal".into()],
                        scroll: None,
                        select: None,
                    },
                ],
                dependencies: vec![Dependency {
                    before: "select".into(),
                    after: "revisit".into(),
                }],
                required_observations: vec![],
                expected_effects: vec!["goal".into()],
                recovery_branches: vec![],
                termination: vec!["goal.required".into()],
            },
        }),
    }
}

fn scroll_plan() -> PlannerOutput {
    use interaction_contracts::desktop::planning::{ScrollDirection, ScrollToSpec};
    let target = SemanticTargetRef::new("scroll-destination").unwrap();
    let container = SemanticTargetRef::new("scroll-document").unwrap();
    PlannerOutput::Plan {
        proposal: Box::new(PlanProposal {
            goal_draft: Some(GoalDraft {
                app: "com.lingxi.computer-user.guard-fixture".into(),
                account: None,
                target: TargetSpec {
                    row: None,
                    semantic_target: target.clone(),
                    label: "MVP scroll destination".into(),
                    group: Some("Scroll fixture group".into()),
                    role_hint: Some("AXCheckBox".into()),
                },
                desired: ExpectedValue::Bool { value: false },
                confirmation_scope: ConfirmationScope::UiRevisit,
                max_effect_class: EffectClass::ReversibleEdit,
            }),
            bundle: PlanBundle {
                subgoals: vec![
                    Subgoal {
                        select: None,
                        id: "reveal".into(),
                        description: "reveal the registered scroll destination".into(),
                        skill: "ui.scroll_to".into(),
                        target: Some(target.clone()),
                        expects: vec!["visible_bound".into()],
                        scroll: Some(ScrollToSpec {
                            container: TargetSpec {
                                row: None,
                                semantic_target: container,
                                label: "MVP scroll document".into(),
                                group: Some("Scroll fixture group".into()),
                                role_hint: Some("AXScrollArea".into()),
                            },
                            direction: ScrollDirection::Down,
                            pixels: 120,
                            max_actions: 4,
                        }),
                    },
                    Subgoal {
                        select: None,
                        id: "read".into(),
                        description: "confirm the destination remains unchecked".into(),
                        skill: "ui.ensure_value".into(),
                        target: Some(target.clone()),
                        expects: vec!["value".into()],
                        scroll: None,
                    },
                    Subgoal {
                        select: None,
                        id: "revisit".into(),
                        description: "leave and return to verify the revealed destination".into(),
                        skill: "ui.revisit".into(),
                        target: Some(target),
                        expects: vec!["revisited".into()],
                        scroll: None,
                    },
                ],
                dependencies: vec![
                    Dependency {
                        before: "reveal".into(),
                        after: "read".into(),
                    },
                    Dependency {
                        before: "read".into(),
                        after: "revisit".into(),
                    },
                ],
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

    const PROJECT_ROW_TARGET_JSON: &str = r#"{
        "semantic_target":"project-row",
        "label":"Weekly report",
        "group":null,
        "role_hint":"AXRow",
        "row":{
            "table_scope":{
                "scroll_path":[
                    {"role":"ax_scroll_area","label":"Preferences content"},
                    {"role":"ax_scroll_area","label":"Projects"}
                ],
                "column_headers":["Name","Owner","Status"]
            },
            "conditions":[
                {"column_name":"Name","value":{"type":"text","value":"Weekly report"}},
                {"column_name":"Owner","value":{"type":"text","value":"Mina"}},
                {"column_name":"Status","value":{"type":"text","value":"Review"}}
            ]
        }
    }"#;

    fn request(text: &str) -> PlanningRequest {
        let context = interaction_contracts::desktop::planning::PlanningContext {
            user_text: text.into(),
            allowed_scope: None,
            unknown_items: vec![],
            allowed_skills: vec![],
            recent_failures: vec![],
            notes: vec![],
        };
        PlanningRequest {
            request_id: interaction_contracts::desktop::ids::RequestId::new("ref").unwrap(),
            generation: 1,
            root_task_id: interaction_contracts::desktop::ids::RootTaskId::new("root").unwrap(),
            task_revision: 1,
            root_epoch: 1,
            reason: PlanningReason::GoalCompile,
            context,
            deadline_ms: 1000,
        }
    }

    #[test]
    fn reference_scroll_plan_keeps_container_destination_and_revisit_distinct() {
        let PlannerOutput::Plan { proposal } = plan(&request("Reveal fixture scroll destination"))
        else {
            panic!("expected plan")
        };
        let goal = proposal.goal_draft.unwrap();
        assert_eq!(goal.desired, ExpectedValue::Bool { value: false });
        assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
        let first = &proposal.bundle.subgoals[0];
        let spec = first.scroll.as_ref().unwrap();
        assert_eq!(first.target.as_ref(), Some(&goal.target.semantic_target));
        assert_ne!(spec.container.semantic_target, goal.target.semantic_target);
        assert_eq!((spec.pixels, spec.max_actions), (120, 4));
        assert!(proposal.bundle.validate().is_ok());
        let PlannerOutput::Plan { proposal } =
            plan(&request("Reveal fixture scroll destination and submit"))
        else {
            panic!("expected plan")
        };
        assert!(proposal.goal_draft.is_none());
    }

    #[test]
    fn explicit_menu_reference_freezes_choice_and_preserves_revisit() {
        let PlannerOutput::Plan { proposal } = plan(&request("Select fixture menu second item"))
        else {
            panic!("expected menu plan")
        };
        let goal = proposal.goal_draft.as_ref().unwrap();
        assert_eq!(goal.target.semantic_target.as_str(), "menu-control");
        assert_eq!(goal.target.group.as_deref(), Some("Menu fixture group"));
        assert_eq!(
            goal.desired,
            ExpectedValue::Text {
                value: "Fixture menu second item".into()
            }
        );
        assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
        assert_eq!(proposal.bundle.subgoals[0].skill, "ui.select_by_label");
        assert_eq!(
            proposal.bundle.subgoals[0]
                .select
                .as_ref()
                .unwrap()
                .choice_label,
            "Fixture menu second item"
        );
        assert_eq!(proposal.bundle.subgoals[1].skill, "ui.revisit");
        assert!(proposal.bundle.validate().is_ok());
        for text in [
            "Select fixture menu second item and publish",
            "Select fixture menu first item",
        ] {
            let PlannerOutput::Plan { proposal } = plan(&request(text)) else {
                panic!("expected refused draft")
            };
            assert!(proposal.goal_draft.is_none());
        }
    }

    #[test]
    fn explicit_reference_text_compilation_keeps_literal_value_and_rejects_oversize() {
        let request = request("Fill document: 中文\r\n👩‍👩‍👧‍👦");
        let PlannerOutput::Plan { proposal } = plan(&request) else {
            panic!("expected plan")
        };
        assert_eq!(
            proposal.goal_draft.unwrap().desired,
            ExpectedValue::Text {
                value: "中文\r\n👩‍👩‍👧‍👦".into()
            }
        );
        let mut bad = request;
        bad.context.user_text = format!("Fill document: {}", "x".repeat(2049));
        let PlannerOutput::Plan { proposal } = plan(&bad) else {
            panic!("expected plan")
        };
        assert!(proposal.goal_draft.is_none());
    }

    #[test]
    fn keyboard_reference_freezes_registered_target_and_revisit_without_key_steps() {
        let PlannerOutput::Plan { proposal } = plan(&request("Enable fixture keyboard toggle"))
        else {
            panic!("expected keyboard plan")
        };
        let goal = proposal.goal_draft.unwrap();
        assert_eq!(goal.target.semantic_target.as_str(), "keyboard-toggle");
        assert_eq!(goal.target.label, "Key fixture toggle");
        assert_eq!(goal.target.group.as_deref(), Some("Key fixture group"));
        assert_eq!(goal.target.role_hint.as_deref(), Some("AXCheckBox"));
        assert_eq!(goal.desired, ExpectedValue::Bool { value: true });
        assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
        assert_eq!(goal.max_effect_class, EffectClass::ReversibleEdit);
        assert_eq!(proposal.bundle.subgoals[0].skill, "ui.ensure_value");
        assert_eq!(proposal.bundle.subgoals[1].skill, "ui.revisit");
        assert!(proposal.bundle.subgoals.iter().all(|step| {
            step.target.as_ref() == Some(&goal.target.semantic_target)
                && step.select.is_none()
                && step.scroll.is_none()
        }));
        let PlannerOutput::Plan { proposal } =
            plan(&request("Enable fixture keyboard toggle and publish"))
        else {
            panic!("expected unsupported plan")
        };
        assert!(proposal.goal_draft.is_none());
    }

    #[test]
    fn explicit_reference_toggle_uses_a_typed_goal_and_the_registered_skill() {
        let PlannerOutput::Plan { proposal } = plan(&request("Enable fixture toggle")) else {
            panic!("expected plan")
        };
        let goal = proposal.goal_draft.unwrap();
        assert_eq!(goal.target.semantic_target.as_str(), "toggle");
        assert_eq!(goal.target.role_hint.as_deref(), Some("AXCheckBox"));
        assert_eq!(goal.desired, ExpectedValue::Bool { value: true });
        assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
        assert_eq!(proposal.bundle.subgoals[0].skill, "ui.ensure_value");
        assert_eq!(proposal.bundle.subgoals[1].skill, "ui.revisit");
        assert_eq!(proposal.bundle.dependencies[0].before, "ensure");
        let PlannerOutput::Plan { proposal } =
            plan(&request("Enable fixture toggle and publish it"))
        else {
            panic!("expected plan")
        };
        assert!(proposal.goal_draft.is_none());
    }

    #[test]
    fn explicit_row_reference_preserves_public_disambiguating_conditions() {
        let PlannerOutput::Plan { proposal } = plan(&request(&format!(
            "Select fixture row: {PROJECT_ROW_TARGET_JSON}"
        ))) else {
            panic!("expected plan")
        };
        let goal = proposal.goal_draft.as_ref().unwrap();
        let expected_target: TargetSpec = serde_json::from_str(PROJECT_ROW_TARGET_JSON).unwrap();
        assert_eq!(goal.app.as_str(), ROW_FIXTURE_APP);
        assert_eq!(goal.target, expected_target);
        assert_eq!(
            goal.target.row.as_ref().unwrap().conditions[1].column_name,
            "Owner"
        );
        assert_eq!(
            goal.target.row.as_ref().unwrap().conditions[1].value,
            ExpectedValue::Text {
                value: "Mina".into()
            }
        );
        assert_eq!(
            goal.target.row.as_ref().unwrap().conditions[2],
            interaction_contracts::desktop::task::RowColumnConditionSpec {
                column_name: "Status".into(),
                value: ExpectedValue::Text {
                    value: "Review".into(),
                },
            }
        );
        assert_eq!(
            goal.target.row.as_ref().unwrap().table_scope.scroll_path,
            vec![
                interaction_contracts::desktop::task::RowContainerScope {
                    role: interaction_contracts::desktop::task::RowContainerRole::AxScrollArea,
                    label: "Preferences content".into(),
                },
                interaction_contracts::desktop::task::RowContainerScope {
                    role: interaction_contracts::desktop::task::RowContainerRole::AxScrollArea,
                    label: "Projects".into(),
                },
            ]
        );
        assert_eq!(goal.desired, ExpectedValue::Bool { value: true });
        assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
        assert_eq!(goal.max_effect_class, EffectClass::Navigation);
        assert_eq!(
            proposal
                .bundle
                .subgoals
                .iter()
                .map(|subgoal| subgoal.skill.as_str())
                .collect::<Vec<_>>(),
            vec!["ui.select_row", "ui.revisit"]
        );
        assert_eq!(proposal.bundle.dependencies.len(), 1);
        assert_eq!(proposal.bundle.dependencies[0].before, "select");
        assert_eq!(proposal.bundle.dependencies[0].after, "revisit");
        assert_eq!(
            proposal.bundle.termination,
            vec!["goal.required".to_string()]
        );
        assert!(proposal.bundle.validate().is_ok());
    }

    #[test]
    fn invalid_row_references_return_an_empty_plan() {
        let target: serde_json::Value = serde_json::from_str(PROJECT_ROW_TARGET_JSON).unwrap();
        let mut cases = vec![
            (
                "missing row",
                serde_json::json!({
                    "semantic_target":"project-row",
                    "label":"Project row",
                    "group":"Projects",
                    "role_hint":"AXRow"
                })
                .to_string(),
            ),
            ("unknown field", {
                let mut value = target.clone();
                value["untrusted"] = serde_json::json!(true);
                value.to_string()
            }),
            ("private index", {
                let mut value = target.clone();
                value["row"]["ax_index"] = serde_json::json!(42);
                value.to_string()
            }),
            ("unregistered table scope", {
                let mut value = target.clone();
                value["row"]["table_scope"]["scroll_path"][0]["label"] =
                    serde_json::json!("Other list");
                value.to_string()
            }),
            ("unregistered semantic target", {
                let mut value = target.clone();
                value["semantic_target"] = serde_json::json!("unregistered-row");
                value.to_string()
            }),
            ("dangerous target label", {
                let mut value = target.clone();
                value["label"] = serde_json::json!("Delete project row");
                value.to_string()
            }),
        ];
        cases.push(("malformed JSON", "{broken".into()));

        for (description, json) in cases {
            let PlannerOutput::Plan { proposal } =
                plan(&request(&format!("Select fixture row: {json}")))
            else {
                panic!("invalid row reference should return an empty plan: {description}")
            };
            assert!(proposal.goal_draft.is_none(), "{description}");
            assert!(proposal.bundle.subgoals.is_empty(), "{description}");
        }
    }
}
