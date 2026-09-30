//! Deterministic stand-ins for the planner (S2) and the teacher model that
//! occupies the S1 slot. They are not models: they parse a few phrasings and
//! apply fixed preferences, which is enough to drive the closed loop and to
//! make every decision reproducible.

use interaction_contracts::desktop::decision::{
    Answer, DecisionProposal, DecisionRequest, DecisionSource, PolicyChoice, Precondition,
};
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::SemanticTargetRef;
use interaction_contracts::desktop::planning::{
    Dependency, GoalDraft, PlanBundle, PlanProposal, PlannerOutput, PlanningReason,
    PlanningRequest, ReviewProposal, Subgoal,
};
use interaction_contracts::desktop::task::{ConfirmationScope, ExpectedValue, TargetSpec};

use crate::app::BUNDLE_ID;

/// The text between quote marks, in order: `"x"`, `“x”` and `「x」`.
pub fn quoted(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current: Option<(char, String)> = None;
    for ch in text.chars() {
        match (&mut current, ch) {
            (None, '"' | '“' | '「') => {
                let close = match ch {
                    '"' => '"',
                    '“' => '”',
                    _ => '」',
                };
                current = Some((close, String::new()));
            }
            (Some((close, buffer)), c) if c == *close => {
                found.push(std::mem::take(buffer));
                current = None;
            }
            (Some((_, buffer)), c) => buffer.push(c),
            _ => {}
        }
    }
    found
}

fn slug(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// Parses phrasings such as `Turn off "Daily picks" in "Push" and apply it`.
#[derive(Debug, Clone, Default)]
pub struct ScriptedPlanner {
    /// Also plans a step outside the goal, to show that installation refuses it.
    pub overreach: bool,
}

impl ScriptedPlanner {
    pub fn plan(&self, request: &PlanningRequest) -> PlannerOutput {
        match request.reason {
            PlanningReason::GoalCompile => self.plan_text(&request.context.user_text),
            _ => PlannerOutput::Review {
                proposal: ReviewProposal {
                    notes: vec!["nothing to add".into()],
                    evidence: vec![],
                },
            },
        }
    }

    /// The plan for a user's words, without building a request around them.
    pub fn plan_text(&self, text: &str) -> PlannerOutput {
        let quotes = quoted(text);
        let Some(label) = quotes.first().cloned() else {
            // Nothing to aim at: the planner returns a plan without a goal.
            return PlannerOutput::Plan {
                proposal: Box::new(PlanProposal {
                    goal_draft: None,
                    bundle: bundle(&[]),
                }),
            };
        };
        let group = quotes.get(1).cloned();
        let lower = text.to_lowercase();
        let desired = !(lower.contains("off") || lower.contains("关闭") || lower.contains("关掉"));
        let wants_apply = ["apply", "save", "应用", "保存"]
            .iter()
            .any(|w| lower.contains(w));
        let semantic = match &group {
            Some(group) => format!("{}/{}", slug(group), slug(&label)),
            None => slug(&label),
        };
        let target = SemanticTargetRef::new(semantic.clone()).expect("a slug is a valid ref");
        let (scope, max_effect) = if wants_apply {
            (ConfirmationScope::Commit, EffectClass::Commit)
        } else {
            (ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit)
        };
        let draft = GoalDraft {
            app: BUNDLE_ID.into(),
            account: None,
            target: TargetSpec {
                semantic_target: target.clone(),
                label,
                group,
                role_hint: Some("switch".into()),
            },
            desired: ExpectedValue::Bool { value: desired },
            confirmation_scope: scope,
            max_effect_class: max_effect,
        };
        let mut steps = vec![(
            "ensure",
            "ui.ensure_value",
            Some(target.clone()),
            "the switch has the wanted value",
        )];
        if wants_apply {
            steps.push((
                "apply",
                "ui.activate",
                Some(SemanticTargetRef::new("apply-button").expect("valid")),
                "the change is saved",
            ));
        } else {
            steps.push((
                "revisit",
                "ui.revisit",
                Some(target),
                "the value survives leaving and re-entering the page",
            ));
        }
        if self.overreach {
            steps.push((
                "reset",
                "ui.activate",
                Some(SemanticTargetRef::new("reset-button").expect("valid")),
                "start from a clean slate",
            ));
        }
        PlannerOutput::Plan {
            proposal: Box::new(PlanProposal {
                goal_draft: Some(draft),
                bundle: bundle(&steps),
            }),
        }
    }
}

fn bundle(steps: &[(&str, &str, Option<SemanticTargetRef>, &str)]) -> PlanBundle {
    let subgoals: Vec<Subgoal> = steps
        .iter()
        .map(|(id, skill, target, expects)| Subgoal {
            id: (*id).into(),
            description: (*id).into(),
            skill: (*skill).into(),
            target: target.clone(),
            expects: vec![(*expects).into()],
        })
        .collect();
    let dependencies = subgoals
        .windows(2)
        .map(|pair| Dependency {
            before: pair[0].id.clone(),
            after: pair[1].id.clone(),
        })
        .collect();
    PlanBundle {
        subgoals,
        dependencies,
        required_observations: vec![],
        expected_effects: vec![],
        recovery_branches: vec![],
        termination: vec!["the goal predicates hold".into()],
    }
}

/// Picks among executable candidates with fixed preferences.
#[derive(Debug, Clone)]
pub struct ScriptedTeacher {
    pub backend: String,
    /// Prefer this candidate skill's description words when several tie.
    pub prefer_word: Option<String>,
}

impl Default for ScriptedTeacher {
    fn default() -> Self {
        Self {
            backend: "teacher-scripted".into(),
            prefer_word: None,
        }
    }
}

fn overlap(description: &str, goal: &str) -> usize {
    let goal: Vec<String> = goal
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    description
        .to_lowercase()
        .split_whitespace()
        .filter(|word| {
            goal.iter().any(|g| {
                g.trim_matches(|c: char| !c.is_alphanumeric())
                    == word.trim_matches(|c: char| !c.is_alphanumeric())
            })
        })
        .count()
}

impl ScriptedTeacher {
    pub fn decide(&self, request: &DecisionRequest) -> DecisionProposal {
        let executable: Vec<_> = request
            .state
            .candidates
            .iter()
            .filter(|c| c.precondition == Precondition::Executable)
            .collect();
        let goal = &request.state.goal_summary;
        let skip_control =
            |skill: &str| !matches!(skill, "observe_more" | "request_planning" | "stop_or_yield");
        let picked = self
            .prefer_word
            .as_deref()
            .and_then(|word| {
                executable
                    .iter()
                    .find(|c| c.description.to_lowercase().contains(&word.to_lowercase()))
            })
            .or_else(|| {
                executable
                    .iter()
                    .filter(|c| skip_control(&c.skill))
                    .max_by_key(|c| {
                        (
                            overlap(&c.description, goal),
                            std::cmp::Reverse(c.candidate_id.as_str().to_string()),
                        )
                    })
            })
            .or_else(|| executable.first());
        let (choice, selected) = match picked {
            Some(candidate) => (
                PolicyChoice::StartSkill {
                    candidate: candidate.candidate_id.clone(),
                },
                Some(candidate.candidate_id.as_str().to_string()),
            ),
            None => (
                PolicyChoice::ObserveMore {
                    facts: vec!["a fresh observation".into()],
                },
                None,
            ),
        };
        DecisionProposal {
            decision_id: request.decision_id.clone(),
            request_generation: request.request_generation,
            candidate_set_digest: request.state.candidate_set_digest.clone(),
            backend: self.backend.clone(),
            source: DecisionSource::Teacher,
            model_revision: "scripted-1".into(),
            answers: request
                .questions
                .iter()
                .map(|question| Answer {
                    question_id: question.id.clone(),
                    selected: selected.clone(),
                    confidence_milli: Some(900),
                })
                .collect(),
            choice: Some(choice),
            failure: None,
            truncated: false,
            queue_ms: 0,
            inference_ms: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::ids::RootTaskId;
    use interaction_contracts::desktop::planning::PlanningContext;

    fn request(text: &str) -> PlanningRequest {
        PlanningRequest {
            request_id: interaction_contracts::desktop::ids::RequestId::new("plan-1").unwrap(),
            generation: 1,
            root_task_id: RootTaskId::new("root-1").unwrap(),
            task_revision: 1,
            root_epoch: 1,
            reason: PlanningReason::GoalCompile,
            context: PlanningContext {
                user_text: text.into(),
                unknown_items: vec![],
                allowed_skills: vec![],
                recent_failures: vec![],
                notes: vec![],
            },
            deadline_ms: 10_000,
        }
    }

    fn draft(output: PlannerOutput) -> (GoalDraft, PlanBundle) {
        match output {
            PlannerOutput::Plan { proposal } => {
                (proposal.goal_draft.expect("a goal draft"), proposal.bundle)
            }
            other => panic!("expected a plan, got {other:?}"),
        }
    }

    #[test]
    fn quotes_are_extracted_in_every_supported_style() {
        assert_eq!(
            quoted(r#"off "Daily picks" in "Push""#),
            vec!["Daily picks", "Push"]
        );
        assert_eq!(quoted("关闭“每日推荐”在「推送」"), vec!["每日推荐", "推送"]);
        assert!(quoted("no quotes here").is_empty());
        assert!(quoted(r#"an "unterminated quote"#).is_empty());
    }

    #[test]
    fn the_planner_turns_a_phrase_into_a_goal_and_a_revisit_plan() {
        let (goal, bundle) =
            draft(ScriptedPlanner::default().plan(&request(r#"Turn off "Daily picks" in "Push""#)));
        assert_eq!(goal.target.label, "Daily picks");
        assert_eq!(goal.target.group.as_deref(), Some("Push"));
        assert_eq!(goal.desired, ExpectedValue::Bool { value: false });
        assert_eq!(goal.confirmation_scope, ConfirmationScope::UiRevisit);
        assert_eq!(goal.max_effect_class, EffectClass::ReversibleEdit);
        assert_eq!(bundle.subgoals.len(), 2);
        assert_eq!(bundle.subgoals[1].skill, "ui.revisit");
        bundle.validate().unwrap();
    }

    #[test]
    fn asking_to_apply_widens_the_goal_to_a_commit() {
        let (goal, bundle) = draft(
            ScriptedPlanner::default()
                .plan(&request(r#"Turn on "Promotions" in "Email" and apply it"#)),
        );
        assert_eq!(goal.desired, ExpectedValue::Bool { value: true });
        assert_eq!(goal.confirmation_scope, ConfirmationScope::Commit);
        assert_eq!(goal.max_effect_class, EffectClass::Commit);
        assert_eq!(bundle.subgoals.last().unwrap().skill, "ui.activate");
    }

    #[test]
    fn text_without_a_target_yields_no_goal_draft() {
        match ScriptedPlanner::default().plan(&request("please make it better")) {
            PlannerOutput::Plan { proposal } => assert!(proposal.goal_draft.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_overreaching_planner_adds_a_step_outside_the_goal() {
        let planner = ScriptedPlanner { overreach: true };
        let (_, bundle) = draft(planner.plan(&request(r#"Turn off "Daily picks" in "Push""#)));
        assert!(bundle.subgoals.iter().any(|s| s.id == "reset"));
    }
}
