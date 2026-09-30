//! Planner (S2) requests and proposals (design 12).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::FailureCode;
use super::ids::{RequestId, RootTaskId, SemanticTargetRef};
use super::task::{ConfirmationScope, ExpectedValue, TargetSpec};
use super::verification::EvidenceNeed;
use crate::desktop::effect::EffectClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanningReason {
    /// Turning the user's words into a goal draft.
    GoalCompile,
    Escalation,
    Review,
    EvidenceFollowUp,
}

/// The bounded context handed to the planner. Everything in it is data, not
/// instructions, including text that came from the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanningContext {
    pub user_text: String,
    pub unknown_items: Vec<String>,
    pub allowed_skills: Vec<String>,
    pub recent_failures: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanningRequest {
    pub request_id: RequestId,
    pub generation: u64,
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub reason: PlanningReason,
    pub context: PlanningContext,
    pub deadline_ms: u64,
}

/// A goal the planner drafted. Only valid while the goal is being compiled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoalDraft {
    pub app: String,
    pub account: Option<String>,
    pub target: TargetSpec,
    pub desired: ExpectedValue,
    pub confirmation_scope: ConfirmationScope,
    pub max_effect_class: EffectClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Subgoal {
    #[schemars(length(min = 1))]
    pub id: String,
    pub description: String,
    /// A registered skill; anything else is refused at installation.
    pub skill: String,
    pub target: Option<SemanticTargetRef>,
    pub expects: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecoveryBranch {
    pub on: FailureCode,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanBundle {
    pub subgoals: Vec<Subgoal>,
    pub dependencies: Vec<Dependency>,
    pub required_observations: Vec<String>,
    pub expected_effects: Vec<String>,
    pub recovery_branches: Vec<RecoveryBranch>,
    pub termination: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanProposal {
    /// Present only in answers to a goal-compile request.
    pub goal_draft: Option<GoalDraft>,
    pub bundle: PlanBundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewProposal {
    pub notes: Vec<String>,
    pub evidence: Vec<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRequest {
    pub needs: Vec<EvidenceNeed>,
}

/// The planner's answer is exactly one of these. A result can never be both a
/// plan and an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlannerOutput {
    Plan { proposal: Box<PlanProposal> },
    Review { proposal: ReviewProposal },
    EvidenceRequest { request: EvidenceRequest },
}

impl PlanBundle {
    /// Structure only: unique ids, known dependency endpoints, no cycles.
    /// Whether the skills are registered is the installer's decision.
    pub fn validate(&self) -> Result<(), String> {
        if self.subgoals.is_empty() {
            return Err("a plan needs at least one sub-goal".into());
        }
        let mut ids = BTreeSet::new();
        for subgoal in &self.subgoals {
            if subgoal.id.trim().is_empty() {
                return Err("sub-goal ids must not be blank".into());
            }
            if !ids.insert(subgoal.id.as_str()) {
                return Err(format!("duplicate sub-goal {}", subgoal.id));
            }
        }
        let mut incoming: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for dependency in &self.dependencies {
            for end in [&dependency.before, &dependency.after] {
                if !ids.contains(end.as_str()) {
                    return Err(format!("dependency names unknown sub-goal {end}"));
                }
            }
            incoming
                .entry(dependency.after.as_str())
                .or_default()
                .push(dependency.before.as_str());
        }
        // Kahn's algorithm: a leftover node means a cycle.
        let mut remaining: BTreeMap<&str, usize> = self
            .subgoals
            .iter()
            .map(|subgoal| {
                (
                    subgoal.id.as_str(),
                    incoming.get(subgoal.id.as_str()).map_or(0, Vec::len),
                )
            })
            .collect();
        let mut ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut done = 0;
        while let Some(id) = ready.pop() {
            done += 1;
            for dependency in self.dependencies.iter().filter(|d| d.before == id) {
                let count = remaining
                    .get_mut(dependency.after.as_str())
                    .expect("endpoints were checked");
                *count -= 1;
                if *count == 0 {
                    ready.push(dependency.after.as_str());
                }
            }
        }
        if done != self.subgoals.len() {
            return Err("sub-goal dependencies contain a cycle".into());
        }
        Ok(())
    }
}

impl PlanningRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.generation == 0 {
            return Err("generation starts at 1".into());
        }
        if self.task_revision == 0 || self.root_epoch == 0 {
            return Err("task revision and root epoch start at 1".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subgoal(id: &str) -> Subgoal {
        Subgoal {
            id: id.into(),
            description: id.into(),
            skill: "ui.ensure_value".into(),
            target: None,
            expects: vec![],
        }
    }

    fn bundle(deps: &[(&str, &str)]) -> PlanBundle {
        PlanBundle {
            subgoals: vec![subgoal("a"), subgoal("b"), subgoal("c")],
            dependencies: deps
                .iter()
                .map(|(b, a)| Dependency {
                    before: (*b).into(),
                    after: (*a).into(),
                })
                .collect(),
            required_observations: vec![],
            expected_effects: vec![],
            recovery_branches: vec![],
            termination: vec![],
        }
    }

    #[test]
    fn dependency_graphs_must_be_acyclic_and_closed() {
        assert!(bundle(&[("a", "b"), ("b", "c")]).validate().is_ok());
        assert!(bundle(&[]).validate().is_ok());
        assert!(
            bundle(&[("a", "b"), ("b", "c"), ("c", "a")])
                .validate()
                .is_err()
        );
        assert!(bundle(&[("a", "a")]).validate().is_err());
        assert!(bundle(&[("a", "zzz")]).validate().is_err());
    }

    #[test]
    fn duplicate_or_missing_subgoals_are_rejected() {
        let mut duplicate = bundle(&[]);
        duplicate.subgoals.push(subgoal("a"));
        assert!(duplicate.validate().is_err());
        let mut empty = bundle(&[]);
        empty.subgoals.clear();
        assert!(empty.validate().is_err());
    }

    #[test]
    fn planner_output_is_a_tagged_union() {
        let review = PlannerOutput::Review {
            proposal: ReviewProposal {
                notes: vec!["ok".into()],
                evidence: vec![],
            },
        };
        let json = serde_json::to_value(&review).unwrap();
        assert_eq!(json["kind"], "review");
        let extra = serde_json::json!({"kind": "review", "proposal": {"notes": [], "evidence": []}, "plan": {}});
        assert!(serde_json::from_value::<PlannerOutput>(extra).is_err());
    }
}
