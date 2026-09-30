//! Typed decisions: what Reaction asks a backend and what may come back
//! (design 9, 10 and 11).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::effect::EffectClass;
use super::error::FailureCode;
use super::ids::{
    AttemptId, BindingRef, CandidateId, DecisionId, ObjectRef, RootTaskId, SemanticTargetRef,
};

/// Who actually made a semantic choice. Every consumed choice has exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    /// A step the installed plan fixed, with no branch at run time.
    Plan,
    /// A deterministic workflow or rule.
    Rule,
    /// The local fast model.
    S1,
    /// The cloud model standing in for the S1 slot.
    Teacher,
    /// A choice obtained by escalating to the planner.
    Llm,
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    Choice,
    Score,
    Noul,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    #[schemars(length(min = 1))]
    pub key: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Question {
    #[schemars(length(min = 1))]
    pub id: String,
    pub kind: QuestionKind,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Precondition {
    Executable,
    /// Needs a fresh observation before it can be considered.
    NeedsObservation,
    Rejected {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub candidate_id: CandidateId,
    pub skill: String,
    pub semantic_target: Option<SemanticTargetRef>,
    pub binding: Option<BindingRef>,
    pub parameters: BTreeMap<String, String>,
    pub effect_class: EffectClass,
    pub precondition: Precondition,
    pub description: String,
}

/// One object as the model sees it: facts only, no hidden ids to interpret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StateObject {
    pub instance: ObjectRef,
    pub role: String,
    pub label: String,
    pub value: Option<String>,
    pub group: Option<String>,
    pub enabled: bool,
    pub in_view: bool,
    pub age_ms: u64,
}

/// The bounded projection sent to a decision backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionState {
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    pub goal_summary: String,
    pub subgoal: String,
    pub snapshot_revision: u64,
    pub objects: Vec<StateObject>,
    pub candidates: Vec<Candidate>,
    pub recent_results: Vec<String>,
    pub missing_fields: Vec<String>,
    /// What was dropped to fit the budget. Empty when nothing was.
    pub omitted_items: Vec<String>,
    pub candidate_set_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    pub decision_id: DecisionId,
    pub lane_id: String,
    pub request_generation: u64,
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    pub plan_revision: u64,
    pub root_epoch: u64,
    pub decision_program: String,
    pub calibration_revision: String,
    pub state: DecisionState,
    pub questions: Vec<Question>,
    pub created_ms: u64,
    pub result_deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub question_id: String,
    pub selected: Option<String>,
    /// The backend's own figure, not a calibrated probability.
    pub confidence_milli: Option<u32>,
}

/// What a backend proposes. It is only ever a proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PolicyChoice {
    StartSkill { candidate: CandidateId },
    ContinueSkill { attempt: AttemptId },
    ObserveMore { facts: Vec<String> },
    RequestPlanning { reason: String },
    StopOrYield { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionFailure {
    Refused,
    Timeout,
    ParseError,
    DependencyInvalid,
    UnsupportedQuestion,
    /// Required state was cut to fit; the answer is not usable.
    Truncated,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionProposal {
    pub decision_id: DecisionId,
    pub request_generation: u64,
    pub candidate_set_digest: String,
    pub backend: String,
    pub source: DecisionSource,
    pub model_revision: String,
    pub answers: Vec<Answer>,
    pub choice: Option<PolicyChoice>,
    pub failure: Option<DecisionFailure>,
    pub truncated: bool,
    pub queue_ms: u64,
    pub inference_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionCancel {
    pub decision_id: DecisionId,
    pub request_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionOutcome {
    Accepted,
    ObserveMore,
    RequestPlanning,
    StopOrYield,
    Expired,
    Rejected,
}

/// The host's verdict on a proposal. `Accepted` exists only after the attempt
/// has been activated; nothing before that can produce input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionAdmission {
    pub decision_id: DecisionId,
    pub outcome: AdmissionOutcome,
    pub selected: Option<CandidateId>,
    pub attempt: Option<AttemptId>,
    pub source: DecisionSource,
    pub reason: String,
    pub failure_code: Option<FailureCode>,
}

impl DecisionRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.request_generation == 0 {
            return Err("request_generation starts at 1".into());
        }
        if self.result_deadline_ms <= self.created_ms {
            return Err("result_deadline_ms must be after created_ms".into());
        }
        if self.questions.is_empty() {
            return Err("a decision needs at least one question".into());
        }
        let mut keys = std::collections::BTreeSet::new();
        for candidate in &self.state.candidates {
            if !keys.insert(candidate.candidate_id.as_str()) {
                return Err(format!("duplicate candidate {}", candidate.candidate_id));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> DecisionRequest {
        DecisionRequest {
            decision_id: DecisionId::new("d-1").unwrap(),
            lane_id: "lane-1".into(),
            request_generation: 1,
            root_task_id: RootTaskId::new("root-1").unwrap(),
            task_revision: 1,
            plan_revision: 1,
            root_epoch: 1,
            decision_program: "ui.ensure_value@1".into(),
            calibration_revision: "none".into(),
            state: DecisionState {
                root_task_id: RootTaskId::new("root-1").unwrap(),
                task_revision: 1,
                goal_summary: "turn off daily picks".into(),
                subgoal: "ensure value".into(),
                snapshot_revision: 3,
                objects: vec![],
                candidates: vec![Candidate {
                    candidate_id: CandidateId::new("c-1").unwrap(),
                    skill: "ui.ensure_value".into(),
                    semantic_target: None,
                    binding: None,
                    parameters: BTreeMap::new(),
                    effect_class: EffectClass::ReversibleEdit,
                    precondition: Precondition::Executable,
                    description: "set the switch off".into(),
                }],
                recent_results: vec![],
                missing_fields: vec![],
                omitted_items: vec![],
                candidate_set_digest: "sha:1".into(),
            },
            questions: vec![Question {
                id: "q1".into(),
                kind: QuestionKind::Choice,
                prompt: "which action".into(),
                options: vec![],
            }],
            created_ms: 10,
            result_deadline_ms: 500,
        }
    }

    #[test]
    fn a_request_needs_a_deadline_a_question_and_unique_candidates() {
        assert!(request().validate().is_ok());
        let mut bad = request();
        bad.request_generation = 0;
        assert!(bad.validate().is_err());
        bad = request();
        bad.result_deadline_ms = 10;
        assert!(bad.validate().is_err());
        bad = request();
        bad.questions.clear();
        assert!(bad.validate().is_err());
        bad = request();
        let duplicate = bad.state.candidates[0].clone();
        bad.state.candidates.push(duplicate);
        assert!(bad.validate().is_err());
    }

    #[test]
    fn sources_serialise_as_the_attribution_names() {
        for (source, name) in [
            (DecisionSource::Plan, "plan"),
            (DecisionSource::Rule, "rule"),
            (DecisionSource::S1, "s1"),
            (DecisionSource::Teacher, "teacher"),
            (DecisionSource::Llm, "llm"),
            (DecisionSource::Human, "human"),
        ] {
            assert_eq!(
                serde_json::to_string(&source).unwrap(),
                format!("\"{name}\"")
            );
        }
    }
}
