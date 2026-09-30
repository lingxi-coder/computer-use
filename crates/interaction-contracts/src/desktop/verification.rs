//! Three-valued logic and effect results (design 13.3 and 14.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ids::{AttemptId, RequestId, RootTaskId, SemanticTargetRef};
use super::task::ConfirmationScope;

/// Unknown is not false: an unverifiable claim never counts as a failure of
/// the claim, and never as success either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriState {
    True,
    False,
    Unknown,
}

impl TriState {
    /// Any false makes it false; otherwise any unknown makes it unknown.
    pub fn and(values: impl IntoIterator<Item = TriState>) -> TriState {
        let mut unknown = false;
        for value in values {
            match value {
                Self::False => return Self::False,
                Self::Unknown => unknown = true,
                Self::True => {}
            }
        }
        if unknown { Self::Unknown } else { Self::True }
    }

    /// Any true makes it true; otherwise any unknown makes it unknown.
    pub fn or(values: impl IntoIterator<Item = TriState>) -> TriState {
        let mut unknown = false;
        for value in values {
            match value {
                Self::True => return Self::True,
                Self::Unknown => unknown = true,
                Self::False => {}
            }
        }
        if unknown { Self::Unknown } else { Self::False }
    }

    pub fn is_true(self) -> bool {
        self == Self::True
    }
}

impl std::ops::Not for TriState {
    type Output = TriState;

    /// Negating unknown stays unknown.
    fn not(self) -> TriState {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectStatus {
    Satisfied,
    Unsatisfied,
    Unknown,
}

impl From<TriState> for EffectStatus {
    fn from(value: TriState) -> Self {
        match value {
            TriState::True => Self::Satisfied,
            TriState::False => Self::Unsatisfied,
            TriState::Unknown => Self::Unknown,
        }
    }
}

/// Whether the action itself is known to have caused the state.
///
/// The goal can be satisfied by someone else's action. That is a satisfied
/// goal, not a confirmed causal label for this action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Causality {
    Confirmed,
    Unknown,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimeSpan {
    pub from_ms: u64,
    pub to_ms: u64,
}

/// An immutable conclusion. New evidence produces a new revision; history is
/// never rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectResult {
    pub result_id: RequestId,
    pub revision: u64,
    pub root_task_id: RootTaskId,
    pub attempt_id: Option<AttemptId>,
    pub intent_id: Option<String>,
    /// Predicate name and version.
    pub predicate: String,
    pub target: Option<SemanticTargetRef>,
    pub status: EffectStatus,
    /// Snapshots the conclusion rests on.
    pub evidence_group: Vec<RequestId>,
    /// The span in which input was actually submitted.
    pub input_span: Option<TimeSpan>,
    pub observed_ms: u64,
    pub confirmation_scope: ConfirmationScope,
    pub causality: Causality,
}

/// What Verification lacks in order to conclude. It has no way to act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceNeed {
    pub need_id: RequestId,
    pub fact: String,
    pub target: Option<SemanticTargetRef>,
    /// The evidence must have been observed after this time.
    pub observed_after_ms: Option<u64>,
    /// Satisfying it needs input (leave and re-enter a page), so it goes
    /// through TaskRuntime and Reaction rather than being a plain observation.
    pub requires_input: bool,
}

#[cfg(test)]
mod tests {
    use super::TriState::{False, True, Unknown};
    use super::*;

    #[test]
    fn negation_keeps_unknown() {
        assert_eq!(!True, False);
        assert_eq!(!False, True);
        assert_eq!(!Unknown, Unknown);
    }

    #[test]
    fn and_follows_the_design_table() {
        assert_eq!(TriState::and([]), True);
        assert_eq!(TriState::and([True, True]), True);
        assert_eq!(TriState::and([True, Unknown]), Unknown);
        assert_eq!(
            TriState::and([Unknown, False]),
            False,
            "a false beats an unknown"
        );
        assert_eq!(TriState::and([False, Unknown, True]), False);
    }

    #[test]
    fn or_is_the_dual() {
        assert_eq!(TriState::or([]), False);
        assert_eq!(TriState::or([False, False]), False);
        assert_eq!(TriState::or([False, Unknown]), Unknown);
        assert_eq!(TriState::or([Unknown, True]), True);
    }

    #[test]
    fn unknown_never_becomes_success() {
        assert_eq!(EffectStatus::from(Unknown), EffectStatus::Unknown);
        assert!(!Unknown.is_true());
    }
}
