//! The operator surface: commands from the UI and what the UI is shown.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::device::{CapabilityManifest, ReleaseStatus};
use super::effect::EffectClass;
use super::ids::{ConfirmationId, RootTaskId};
use super::task::{ConfirmationScope, ExpectedValue, RootState};

/// observe only collects; shadow also records suggestions; execute submits
/// authorised input. No model can raise the mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Observe,
    Shadow,
    Execute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorCommand {
    CreateTask {
        text: String,
    },
    ConfirmGoal {
        confirmation_id: ConfirmationId,
        approved: bool,
    },
    PauseTask {
        root_task_id: RootTaskId,
    },
    ResumeTask {
        root_task_id: RootTaskId,
    },
    /// Cancels the root. `None` cancels whatever is running.
    StopTask {
        root_task_id: Option<RootTaskId>,
    },
    SetMode {
        mode: RunMode,
    },
}

/// A goal in words the user can check before anything runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoalSummary {
    pub user_text: String,
    pub app: String,
    pub target_label: String,
    pub group: Option<String>,
    pub desired: ExpectedValue,
    pub confirmation_scope: ConfirmationScope,
    pub max_effect_class: EffectClass,
}

/// One action that needs its own confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionSummary {
    pub skill: String,
    pub target_label: String,
    pub description: String,
    pub effect_class: EffectClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfirmationKind {
    Goal { goal: GoalSummary },
    Effect { action: ActionSummary },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationRequest {
    pub confirmation_id: ConfirmationId,
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    pub kind: ConfirmationKind,
}

/// A confirmation covers exactly what it showed and nothing beyond it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationResult {
    pub confirmation_id: ConfirmationId,
    pub approved: bool,
}

/// What the stop control shows. A click alone never turns into "stopped".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopStatus {
    NotStopped,
    Stopping,
    StoppedConfirmed,
    /// The stop was requested but the device has not confirmed the release.
    StopUnconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RootView {
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub state: RootState,
    pub headline: String,
    pub attempts_used: u32,
    pub attempts_max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineEntry {
    pub at_ms: u64,
    pub text: String,
}

/// A bounded snapshot for the renderer, always re-derivable from host state.
/// The renderer never treats it as an execution receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UiProjection {
    pub revision: u64,
    pub mode: RunMode,
    pub root: Option<RootView>,
    pub stop_status: StopStatus,
    /// Input cleanup as the device last reported it.
    pub cleanup: ReleaseStatus,
    pub pending_confirmation: Option<ConfirmationRequest>,
    pub capabilities: Option<CapabilityManifest>,
    pub timeline: Vec<TimelineEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_commands_are_a_closed_tagged_union() {
        let json = r#"{"command":"create_task","text":"turn it off"}"#;
        assert!(serde_json::from_str::<OperatorCommand>(json).is_ok());
        let unknown = r#"{"command":"run_shell","text":"rm"}"#;
        assert!(serde_json::from_str::<OperatorCommand>(unknown).is_err());
        let extra = r#"{"command":"create_task","text":"x","execute":true}"#;
        assert!(serde_json::from_str::<OperatorCommand>(extra).is_err());
    }
}
