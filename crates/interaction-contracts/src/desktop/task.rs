//! Root tasks: the frozen goal, its states and the attempt admission messages
//! (design 11.2, 13.3 and 13.5).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::decision::DecisionSource;
use super::effect::EffectClass;
use super::error::FailureCode;
use super::ids::{AttemptId, DecisionId, RootTaskId, SemanticTargetRef};
use super::require_nonempty;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExpectedValue {
    Bool { value: bool },
    Text { value: String },
    Number { value: i64 },
}

/// How much the user asked to have confirmed. The two are separate claims:
/// seeing the state again in the UI is not proof that it was committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationScope {
    UiRevisit,
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskScope {
    #[schemars(length(min = 1))]
    pub app: String,
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetSpec {
    pub semantic_target: SemanticTargetRef,
    #[schemars(length(min = 1))]
    pub label: String,
    pub group: Option<String>,
    pub role_hint: Option<String>,
}

/// The root budget. It is spent by attempts, replans and recoveries alike and
/// is never reset by a retry, a restart or a model switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RootBudgetSpec {
    /// Absolute, in the host's monotonic milliseconds.
    pub deadline_ms: u64,
    pub max_attempts: u32,
    pub max_observations: u32,
    pub max_recoveries: u32,
    pub max_replans: u32,
}

/// A restricted, checkable success condition. There is no way to write "the
/// model says it worked".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Predicate {
    ContextMatches {
        app: String,
    },
    TargetBound {
        target: SemanticTargetRef,
    },
    ReadValueEquals {
        target: SemanticTargetRef,
        expected: ExpectedValue,
        /// The reading must come from evidence observed after the last input.
        observed_after_action: bool,
    },
    /// The page was left and entered again and the target re-bound from new evidence.
    Revisited {
        target: SemanticTargetRef,
    },
    /// Evidence that the change was committed, not merely shown.
    CommitConfirmed {
        target: SemanticTargetRef,
    },
    And {
        of: Vec<Predicate>,
    },
    Or {
        of: Vec<Predicate>,
    },
    Not {
        of: Box<Predicate>,
    },
}

const MAX_PREDICATE_DEPTH: usize = 8;
const MAX_PREDICATE_NODES: usize = 64;

impl Predicate {
    pub fn validate(&self) -> Result<(), String> {
        let mut nodes = 0;
        self.check(0, &mut nodes)
    }

    fn check(&self, depth: usize, nodes: &mut usize) -> Result<(), String> {
        if depth > MAX_PREDICATE_DEPTH {
            return Err("predicate is nested too deeply".into());
        }
        *nodes += 1;
        if *nodes > MAX_PREDICATE_NODES {
            return Err("predicate has too many nodes".into());
        }
        match self {
            Self::ContextMatches { app } => require_nonempty("predicate.app", app),
            Self::And { of } | Self::Or { of } => {
                if of.is_empty() {
                    return Err("and/or need at least one operand".into());
                }
                of.iter()
                    .try_for_each(|child| child.check(depth + 1, nodes))
            }
            Self::Not { of } => of.check(depth + 1, nodes),
            _ => Ok(()),
        }
    }

    /// Whether any leaf needs post-action evidence.
    pub fn needs_post_action_evidence(&self) -> bool {
        match self {
            Self::ReadValueEquals {
                observed_after_action,
                ..
            } => *observed_after_action,
            Self::Revisited { .. } | Self::CommitConfirmed { .. } => true,
            Self::And { of } | Self::Or { of } => of.iter().any(Self::needs_post_action_evidence),
            Self::Not { of } => of.needs_post_action_evidence(),
            _ => false,
        }
    }
}

/// The frozen goal. After the user confirms it, none of this changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoalSpec {
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    /// The user's words, kept verbatim.
    #[schemars(length(min = 1))]
    pub user_text: String,
    pub scope: TaskScope,
    pub target: TargetSpec,
    pub desired: ExpectedValue,
    pub confirmation_scope: ConfirmationScope,
    pub max_effect_class: EffectClass,
    pub required: Predicate,
    pub budget: RootBudgetSpec,
}

impl GoalSpec {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("user_text", &self.user_text)?;
        require_nonempty("scope.app", &self.scope.app)?;
        require_nonempty("target.label", &self.target.label)?;
        if self.task_revision == 0 {
            return Err("task_revision starts at 1".into());
        }
        if self.max_effect_class == EffectClass::Unknown {
            return Err("a goal cannot allow effects of unknown class".into());
        }
        if self.max_effect_class >= EffectClass::External {
            return Err("phase 1 goals cannot include external effects".into());
        }
        if self.budget.max_attempts == 0 {
            return Err("the budget must allow at least one attempt".into());
        }
        self.required.validate()
    }
}

/// The root task states of design 13.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RootState {
    Draft,
    CompilingGoal,
    AwaitingConfirmation,
    NeedsInput,
    Ready,
    Running,
    WaitingEvidence,
    WaitingPlan,
    Pausing,
    Suspended,
    NeedsRevalidation,
    Cancelling,
    Completed,
    Failed,
    Unverified,
    Expired,
    Cancelled,
}

impl RootState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Unverified | Self::Expired | Self::Cancelled
        )
    }

    /// The root is active, so a still-valid sub-goal may produce input.
    pub fn may_produce_input(self) -> bool {
        matches!(
            self,
            Self::Running | Self::WaitingEvidence | Self::WaitingPlan
        )
    }
}

/// Everything that can move a root task from one state to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RootEvent {
    SubmitGoal,
    DraftPassed,
    AmbiguityFound,
    CompileFailed,
    UserConfirmed,
    UserEdited,
    AdmissionPassed,
    AwaitEvidence,
    AwaitPlan,
    EvidenceArrived,
    PlanInstalled,
    NeedUserInfo,
    UserAnswered,
    /// A pause, human input, or device or source failure.
    PauseRequested,
    InputsReleased,
    ResumeRequested,
    ProcessRestarted,
    RevalidationPassed,
    PredicatesSatisfied,
    Unrecoverable,
    InputEndedUnverified,
    DeadlineExpired,
    StopRequested,
    CancelFinished,
}

/// How much of a root's budget one attempt may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptQuota {
    pub actions: u32,
    pub observations: u32,
}

/// Reaction's request to start a skill. It is checked and recorded in one
/// transaction by TaskRuntime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdmitAttempt {
    pub attempt_id: AttemptId,
    pub decision_id: DecisionId,
    pub generation: u64,
    pub root_task_id: RootTaskId,
    pub root_epoch: u64,
    pub task_revision: u64,
    pub plan_revision: u64,
    pub subgoal: String,
    pub skill: String,
    pub quota: AttemptQuota,
    pub deadline_ms: u64,
    pub effect_class: EffectClass,
    pub source: DecisionSource,
}

/// Only an activation allows normal input for the attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub attempt_id: AttemptId,
    pub root_task_id: RootTaskId,
    pub root_epoch: u64,
    pub activation_seq: u64,
    pub granted: AttemptQuota,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdmitRejection {
    RootNotRunning,
    RootEpochStale,
    VersionMismatch,
    DeadlineExpired,
    BudgetExhausted,
    DecisionNotPending,
    DecisionAlreadyConsumed,
    AttemptIdConflict,
    EffectNotAuthorized,
}

impl AdmitRejection {
    pub fn failure_code(self) -> FailureCode {
        match self {
            Self::BudgetExhausted => FailureCode::BudgetExhausted,
            Self::RootNotRunning | Self::RootEpochStale => FailureCode::Cancelled,
            Self::DeadlineExpired => FailureCode::EvidenceExpired,
            Self::EffectNotAuthorized => FailureCode::EffectNotAuthorized,
            Self::VersionMismatch
            | Self::DecisionNotPending
            | Self::DecisionAlreadyConsumed
            | Self::AttemptIdConflict => FailureCode::ContextUnresolved,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdmitOutcome {
    /// Also the answer to a repeated request: the earlier activation again.
    Activated {
        activation: Activation,
    },
    Rejected {
        reason: AdmitRejection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Active,
    Dispatched,
    Settled,
    Unknown,
    Cancelled,
}

impl AttemptState {
    pub fn is_final(self) -> bool {
        matches!(self, Self::Settled | Self::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SemanticTargetRef {
        SemanticTargetRef::new("daily-picks").unwrap()
    }

    fn goal() -> GoalSpec {
        GoalSpec {
            root_task_id: RootTaskId::new("root-1").unwrap(),
            task_revision: 1,
            user_text: "turn off daily picks notifications".into(),
            scope: TaskScope {
                app: "test.app".into(),
                account: None,
            },
            target: TargetSpec {
                semantic_target: target(),
                label: "Daily picks".into(),
                group: Some("Push".into()),
                role_hint: Some("switch".into()),
            },
            desired: ExpectedValue::Bool { value: false },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::ReversibleEdit,
            required: Predicate::And {
                of: vec![
                    Predicate::ContextMatches {
                        app: "test.app".into(),
                    },
                    Predicate::ReadValueEquals {
                        target: target(),
                        expected: ExpectedValue::Bool { value: false },
                        observed_after_action: true,
                    },
                ],
            },
            budget: RootBudgetSpec {
                deadline_ms: 60_000,
                max_attempts: 8,
                max_observations: 16,
                max_recoveries: 3,
                max_replans: 2,
            },
        }
    }

    #[test]
    fn a_reasonable_goal_validates() {
        assert!(goal().validate().is_ok());
    }

    #[test]
    fn goals_cannot_reach_beyond_phase_one() {
        let mut bad = goal();
        bad.max_effect_class = EffectClass::External;
        assert!(bad.validate().is_err());
        bad.max_effect_class = EffectClass::Unknown;
        assert!(bad.validate().is_err());
        bad = goal();
        bad.budget.max_attempts = 0;
        assert!(bad.validate().is_err());
        bad = goal();
        bad.task_revision = 0;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn predicates_are_bounded() {
        let mut nested = Predicate::ContextMatches { app: "a".into() };
        for _ in 0..(MAX_PREDICATE_DEPTH + 2) {
            nested = Predicate::Not {
                of: Box::new(nested),
            };
        }
        assert!(nested.validate().is_err());
        assert!(Predicate::And { of: vec![] }.validate().is_err());
        let wide = Predicate::And {
            of: (0..=MAX_PREDICATE_NODES)
                .map(|_| Predicate::ContextMatches { app: "a".into() })
                .collect(),
        };
        assert!(wide.validate().is_err());
    }

    #[test]
    fn post_action_evidence_is_detected_through_nesting() {
        assert!(goal().required.needs_post_action_evidence());
        assert!(!Predicate::ContextMatches { app: "a".into() }.needs_post_action_evidence());
        assert!(
            Predicate::Not {
                of: Box::new(Predicate::Revisited { target: target() })
            }
            .needs_post_action_evidence()
        );
    }

    #[test]
    fn terminal_and_input_states_are_classified() {
        for state in [
            RootState::Completed,
            RootState::Failed,
            RootState::Unverified,
            RootState::Expired,
            RootState::Cancelled,
        ] {
            assert!(state.is_terminal());
            assert!(!state.may_produce_input());
        }
        assert!(RootState::Running.may_produce_input());
        assert!(RootState::WaitingPlan.may_produce_input());
        for state in [
            RootState::CompilingGoal,
            RootState::AwaitingConfirmation,
            RootState::NeedsInput,
            RootState::Suspended,
            RootState::NeedsRevalidation,
            RootState::Cancelling,
        ] {
            assert!(!state.may_produce_input());
        }
    }

    #[test]
    fn rejections_map_to_the_shared_failure_vocabulary() {
        assert_eq!(
            AdmitRejection::BudgetExhausted.failure_code(),
            FailureCode::BudgetExhausted
        );
        assert_eq!(
            AdmitRejection::RootEpochStale.failure_code(),
            FailureCode::Cancelled
        );
    }
}
