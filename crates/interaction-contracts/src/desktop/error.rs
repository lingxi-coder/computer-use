//! The shared failure vocabulary (design 13.4).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why an action, decision or task did not go the normal way.
///
/// These are distinct outcomes, not flavours of "low confidence": each one
/// drives a different recovery path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    SourceInvalid,
    EvidenceExpired,
    TargetAmbiguous,
    ContextUnresolved,
    ModelUnavailable,
    ContextInsufficient,
    CandidateMissing,
    InputRejected,
    PartialInput,
    EffectUnknown,
    ReleaseFailed,
    ControlProfileInvalid,
    TargetOccluded,
    HumanInputDetected,
    StopLatched,
    EffectNotAuthorized,
    BudgetExhausted,
    Cancelled,
}

impl FailureCode {
    /// The upper-case name used in the design documents and in logs.
    pub fn design_name(self) -> &'static str {
        match self {
            Self::SourceInvalid => "SOURCE_INVALID",
            Self::EvidenceExpired => "EVIDENCE_EXPIRED",
            Self::TargetAmbiguous => "TARGET_AMBIGUOUS",
            Self::ContextUnresolved => "CONTEXT_UNRESOLVED",
            Self::ModelUnavailable => "MODEL_UNAVAILABLE",
            Self::ContextInsufficient => "CONTEXT_INSUFFICIENT",
            Self::CandidateMissing => "CANDIDATE_MISSING",
            Self::InputRejected => "INPUT_REJECTED",
            Self::PartialInput => "PARTIAL_INPUT",
            Self::EffectUnknown => "EFFECT_UNKNOWN",
            Self::ReleaseFailed => "RELEASE_FAILED",
            Self::ControlProfileInvalid => "CONTROL_PROFILE_INVALID",
            Self::TargetOccluded => "TARGET_OCCLUDED",
            Self::HumanInputDetected => "HUMAN_INPUT_DETECTED",
            Self::StopLatched => "STOP_LATCHED",
            Self::EffectNotAuthorized => "EFFECT_NOT_AUTHORIZED",
            Self::BudgetExhausted => "BUDGET_EXHAUSTED",
            Self::Cancelled => "CANCELLED",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_are_snake_case_and_design_names_are_upper_case() {
        assert_eq!(
            serde_json::to_string(&FailureCode::StopLatched).unwrap(),
            "\"stop_latched\""
        );
        assert_eq!(FailureCode::StopLatched.design_name(), "STOP_LATCHED");
        assert_eq!(FailureCode::TargetOccluded.design_name(), "TARGET_OCCLUDED");
    }
}
