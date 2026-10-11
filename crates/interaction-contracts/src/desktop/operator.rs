//! The operator surface: commands from the UI and what the UI is shown.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::browser::BrowserPageProfile;
use super::device::{CapabilityManifest, ReleaseStatus};
use super::effect::EffectClass;
use super::ids::{BindingRef, ConfirmationId, RequestId, RootTaskId};
use super::source::{
    FrameStatus, SourceBackgroundWindowContext, SourceHealth, SourceWindowContext, validate_menu_id,
};
use super::task::{
    AllowedControlScope, ConfirmationScope, ExpectedValue, FrozenTaskProfile, RootState,
    RowTableScope, RowTargetSpec, ScopedCommitBinding, ScopedCommitKind, ScopedUiRevisit,
};

/// Largest exactly representable integer accepted for Operator structure identity.
pub const MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE: u64 = 9_007_199_254_740_991;

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
    /// A free-form objective admitted only within a Host-validated control scope.
    CreateScopedTask {
        objective: String,
        allowed_scope: AllowedControlScope,
    },
    /// Main supplies the goal selected from its trusted, pinned catalog. The
    /// profile is provenance only; the ordinary scope and Goal checks still run.
    CreateFrozenTask {
        objective: String,
        allowed_scope: AllowedControlScope,
        profile: FrozenTaskProfile,
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
    /// The public table scope and exact column conditions being confirmed.
    pub row: Option<RowTargetSpec>,
    pub desired: ExpectedValue,
    pub confirmation_scope: ConfirmationScope,
    pub max_effect_class: EffectClass,
    /// The immutable control boundary approved with this goal, when present.
    pub allowed_scope: Option<AllowedControlScope>,
    /// Exact verification navigation frozen by Host for a scoped native goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_revisit: Option<ScopedUiRevisit>,
    /// The exact Host-frozen commit action and visible receipt, when required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_commit: Option<ScopedCommitBinding>,
    /// The host-frozen browser fixture profile, when this is a browser goal.
    pub browser_page_profile: Option<BrowserPageProfile>,
}

impl GoalSummary {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(revisit) = &self.scoped_revisit {
            revisit.validate()?;
            if self.allowed_scope.is_none()
                || (self.confirmation_scope != ConfirmationScope::UiRevisit
                    && !(self.confirmation_scope == ConfirmationScope::Commit
                        && self
                            .scoped_commit
                            .as_ref()
                            .is_some_and(|commit| commit.receipt.kind == ScopedCommitKind::Manual)))
                || self.browser_page_profile.is_some()
            {
                return Err("scoped revisit summary requires a scoped native goal".into());
            }
        }
        if let Some(commit) = &self.scoped_commit {
            commit.validate()?;
            let allowed = self
                .allowed_scope
                .as_ref()
                .ok_or("scoped commit summary requires a frozen allowed control")?;
            let target = &allowed.targets[0];
            if self.browser_page_profile.is_some()
                || self.target_label != target.control
                || self.group.as_deref() != target.context_label()
                || self.row.is_some()
                || !matches!(&self.desired, ExpectedValue::Bool { value: true })
                || commit.receipt.group != target.context_label().unwrap_or_default()
                || commit
                    .action
                    .as_ref()
                    .is_some_and(|action| action.semantic_target.as_str() == target.control)
            {
                return Err("goal summary does not match its scoped commit binding".into());
            }
            let valid_confirmation = matches!(
                (
                    commit.receipt.kind,
                    self.confirmation_scope,
                    self.scoped_revisit.is_some()
                ),
                (ScopedCommitKind::Manual, ConfirmationScope::Commit, false)
                    | (
                        ScopedCommitKind::Automatic,
                        ConfirmationScope::UiRevisit,
                        true
                    )
            );
            let valid_max_effect = match commit.receipt.kind {
                ScopedCommitKind::Manual => self.max_effect_class == EffectClass::Commit,
                ScopedCommitKind::Automatic => self.max_effect_class <= EffectClass::ReversibleEdit,
            };
            if !valid_confirmation || !valid_max_effect {
                return Err("goal summary has an unsupported scoped commit confirmation".into());
            }
        }
        if let Some(allowed_scope) = &self.allowed_scope {
            allowed_scope.validate()?;
            let target = &allowed_scope.targets[0];
            let form_scope = target.form.is_some();
            let supported_value = if form_scope {
                super::task::scoped_form_value_supported(&self.desired)
            } else {
                matches!(&self.desired, ExpectedValue::Bool { .. })
            };
            let supported_confirmation = if form_scope {
                self.confirmation_scope == ConfirmationScope::UiRevisit
                    && self.scoped_revisit.is_some()
                    && self.scoped_commit.is_none()
            } else {
                matches!(
                    (
                        self.confirmation_scope,
                        self.scoped_revisit.is_some(),
                        self.scoped_commit.is_some()
                    ),
                    (ConfirmationScope::CurrentUi, false, false)
                        | (ConfirmationScope::UiRevisit, true, false)
                        | (ConfirmationScope::UiRevisit, true, true)
                        | (ConfirmationScope::Commit, false, true)
                )
            };
            let exact_manual_commit = self.confirmation_scope == ConfirmationScope::Commit
                && self.scoped_commit.as_ref().is_some_and(|commit| {
                    commit.receipt.kind == ScopedCommitKind::Manual && commit.action.is_some()
                });
            let supported_max_effect = self.max_effect_class <= EffectClass::ReversibleEdit
                || (exact_manual_commit && self.max_effect_class == EffectClass::Commit);
            if self.target_label != target.control
                || self.group.as_deref() != target.context_label()
                || self.row.is_some()
                || !supported_confirmation
                || !supported_value
                || !supported_max_effect
            {
                return Err("goal summary does not match its allowed control scope".into());
            }
        }
        if let Some(profile) = self.browser_page_profile
            && (self.app != profile.app()
                || self.target_label != profile.label()
                || self.group.is_some()
                || self.row.is_some()
                || !matches!(&self.desired, ExpectedValue::Bool { .. })
                || self.confirmation_scope != ConfirmationScope::CurrentUi
                || self.max_effect_class != EffectClass::ReversibleEdit)
        {
            return Err("browser goal summary does not match its fixed checkbox profile".into());
        }
        Ok(())
    }
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
    Goal { goal: Box<GoalSummary> },
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

impl ConfirmationRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.task_revision == 0 {
            return Err("confirmation task_revision starts at 1".into());
        }
        if let ConfirmationKind::Goal { goal } = &self.kind {
            goal.validate()?;
        }
        Ok(())
    }
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    pub observation: Option<ObservationView>,
    /// Read-only browser facts; this never indicates a browser input grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_observation: Option<BrowserObservationView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserObservationView {
    pub scope: super::browser::BrowserReadScope,
    pub snapshot_id: String,
    pub revision: u64,
    pub source_sequence: u64,
    pub observed_ms: u64,
    pub read_finished_ms: u64,
    pub target_context_hash: String,
    pub node_count: u64,
    pub readonly_binding_ref: Option<BindingRef>,
    pub readonly_checked: Option<bool>,
}

impl BrowserObservationView {
    pub fn validate(&self) -> Result<(), String> {
        use super::browser::{MAX_BROWSER_SAFE_INTEGER, browser_target_context_hash};
        self.scope.validate()?;
        if self.snapshot_id.is_empty()
            || self.snapshot_id.len() > 256
            || self.snapshot_id.chars().any(char::is_control)
            || self.revision == 0
            || self.revision > MAX_BROWSER_SAFE_INTEGER
            || self.source_sequence == 0
            || self.source_sequence > MAX_BROWSER_SAFE_INTEGER
            || self.read_finished_ms > MAX_BROWSER_SAFE_INTEGER
            || self.read_finished_ms < self.observed_ms
            || self.observed_ms < self.scope.issued_ms
            || self.node_count == 0
            || self.node_count > 2_048
            || self.target_context_hash != browser_target_context_hash(&self.scope.context)?
            || self.readonly_binding_ref.is_some() != self.readonly_checked.is_some()
        {
            return Err("invalid browser read-only projection".into());
        }
        Ok(())
    }
}

impl UiProjection {
    /// Validate scoped metadata carried by the operator projection.
    ///
    /// The older `observed_target` remains a whole-structure display hint. A
    /// scoped target is separately validated and never upgrades that hint or
    /// grants input authority.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(observation) = &self.observation {
            observation.validate()?;
        }
        if let Some(observation) = &self.browser_observation {
            observation.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationView {
    pub target_app: Option<String>,
    pub target_name: Option<String>,
    pub active_window: Option<String>,
    /// Current native window metadata. This is not input authority or consent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_target: Option<ObservedTarget>,
    /// A same-read, table-scoped window-consent candidate. This does not
    /// replace `observed_target` or authorize ordinary input binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_window_target: Option<ScopedWindowTarget>,
    /// A complete same-read logical native window context for passive
    /// BackgroundAX consent. This is independent from global focus and grants
    /// no input authority by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_window_target: Option<BackgroundWindowTarget>,
    pub sources: Vec<SourceHealth>,
    pub frame: Option<FrameView>,
}

impl ObservationView {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(scoped) = &self.scoped_window_target {
            scoped.validate()?;
            if self.target_app.as_deref() != Some(scoped.target.app.as_str()) {
                return Err("scoped window target app does not match the observation app".into());
            }
            if self
                .observed_target
                .as_ref()
                .is_some_and(|target| target != &scoped.target)
            {
                return Err("scoped and whole-structure window targets disagree".into());
            }
        }
        if let Some(background) = &self.background_window_target {
            background.validate()?;
            if self.target_app.as_deref() != Some(background.app.as_str()) {
                return Err(
                    "background window target app does not match the observation app".into(),
                );
            }
        }
        Ok(())
    }
}

/// Read-only projection of an independently identified native background
/// window. The exact Source frame remains in `source.window` and is the
/// geometry anchor; `frame_points` is only a rounded UI projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackgroundWindowTarget {
    pub snapshot_id: RequestId,
    pub app: String,
    pub pid: u32,
    pub window_id: String,
    pub window_title: Option<String>,
    pub frame_points: [i64; 4],
    pub structure_sequence: u64,
    pub session_epoch: u64,
    pub observed_ms: u64,
    pub valid_until_ms: u64,
    pub source: SourceBackgroundWindowContext,
}

impl BackgroundWindowTarget {
    pub fn validate(&self) -> Result<(), String> {
        validate_menu_id("background target app", &self.app)?;
        validate_menu_id("background target window id", &self.window_id)?;
        self.source.validate_identity(&self.app, self.pid)?;

        if self.pid == 0
            || self.pid > i32::MAX as u32
            || self.structure_sequence == 0
            || self.structure_sequence > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || self.session_epoch == 0
            || self.session_epoch > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || self.observed_ms > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || self.valid_until_ms > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || self.valid_until_ms <= self.observed_ms
            || self.valid_until_ms - self.observed_ms > 2_000
            || self.session_epoch != self.source.window.session_epoch
            || self.structure_sequence != self.source.window.verified_source_sequence
            || self.observed_ms != self.source.window.verified_observed_ms
            || self.window_id != self.source.window.origin_window_id
            || !self.source.is_visible_standard_window()
        {
            return Err("background target does not match a fresh visible Source window".into());
        }
        if let Some(title) = &self.window_title
            && (title.trim().is_empty()
                || title.len() > 4_096
                || title.chars().any(char::is_control))
        {
            return Err("background window title is invalid or unbounded".into());
        }

        let frame = self.source.window.origin_window_frame;
        let expected_frame =
            [frame.x, frame.y, frame.width, frame.height].map(|value| value.round() as i64);
        if self.frame_points != expected_frame {
            return Err("background target frame projection differs from its Source frame".into());
        }
        Ok(())
    }
}

/// Public metadata for a table-scoped window-consent candidate. This contains
/// no AX handle and is not an ordinary input binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopedWindowTarget {
    pub target: ObservedTarget,
    pub table_scope: RowTableScope,
    pub source: SourceWindowContext,
    /// The lowercase SHA-256 payload from a validated `bc1:` WindowRoot
    /// continuity digest; the prefix is omitted on this projection surface.
    #[schemars(length(min = 64, max = 64))]
    #[schemars(regex(pattern = "^[0-9a-f]{64}$"))]
    pub root_identity_hash: String,
}

impl ScopedWindowTarget {
    pub fn validate(&self) -> Result<(), String> {
        self.table_scope.validate()?;
        self.source.validate()?;

        validate_menu_id("scoped target app", &self.target.app)?;
        validate_menu_id("scoped target window id", &self.target.window_id)?;
        let title = self
            .target
            .window_title
            .as_deref()
            .ok_or("scoped window target requires a known title")?;
        if title.trim().is_empty() || title.len() > 4096 || title.chars().any(char::is_control) {
            return Err("scoped window target title is empty or unbounded".into());
        }

        let target = &self.target;
        let source = &self.source;
        if target.pid == 0
            || target.pid > i32::MAX as u32
            || target.session_epoch == 0
            || target.session_epoch > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || target.structure_sequence == 0
            || target.structure_sequence > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || target.observed_ms > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || target.valid_until_ms > MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
            || target.valid_until_ms <= target.observed_ms
            || target.valid_until_ms - target.observed_ms > 2_000
            || target.session_epoch != source.session_epoch
            || target.pid != source.process.pid
            || target.window_id != source.origin_window_id
            || target.structure_sequence != source.verified_source_sequence
            || target.observed_ms != source.verified_observed_ms
        {
            return Err("scoped window target does not match its current source identity".into());
        }

        let expected_frame = [
            source.origin_window_frame.x,
            source.origin_window_frame.y,
            source.origin_window_frame.width,
            source.origin_window_frame.height,
        ]
        .map(|value| value.round() as i64);
        if target.frame_points != Some(expected_frame) {
            return Err("scoped window target frame does not match its source window".into());
        }

        let hash = self.root_identity_hash.as_bytes();
        if hash.len() != 64
            || !hash
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(
                "scoped window root identity must be 64 lowercase hexadecimal characters".into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservedTarget {
    pub snapshot_id: RequestId,
    /// Source structure observation sequence, independent of pixel/frame order.
    #[schemars(range(min = 1, max = MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE))]
    pub structure_sequence: u64,
    pub session_epoch: u64,
    pub app: String,
    pub pid: u32,
    pub window_id: String,
    pub window_title: Option<String>,
    /// Rounded global display points for Operator placement, never input authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_points: Option<[i64; 4]>,
    pub observed_ms: u64,
    pub valid_until_ms: u64,
    pub contains_private_fields: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameView {
    pub stream_id: String,
    pub frame_id: Option<String>,
    pub status: FrameStatus,
    pub geometry_revision: u64,
    pub width_px: u32,
    pub height_px: u32,
    pub pixel_age_ms: Option<u64>,
    pub confirmation_age_ms: Option<u64>,
    pub dropped_frames: u64,
    pub healthy: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::browser::BrowserPageProfile;
    use crate::desktop::message::Message;
    use crate::desktop::source::{
        KernelProcessIdentity, ObservationFact, Rect, SourceBackgroundWindowContext,
    };
    use crate::desktop::task::{
        AllowedControlScope, AllowedControlTarget, RowContainerRole, RowContainerScope,
        ScopedUiRevisit, TargetSpec,
    };

    #[test]
    fn operator_commands_are_a_closed_tagged_union() {
        let json = r#"{"command":"create_task","text":"turn it off"}"#;
        assert!(serde_json::from_str::<OperatorCommand>(json).is_ok());
        let scoped = r#"{"command":"create_scoped_task","objective":"turn it off","allowed_scope":{"application":"Browser fixture","targets":[{"control":"Background option","group":null,"disambiguator":null}],"no_unrelated_controls":true}}"#;
        assert!(serde_json::from_str::<OperatorCommand>(scoped).is_ok());
        let unknown = r#"{"command":"run_shell","text":"rm"}"#;
        assert!(serde_json::from_str::<OperatorCommand>(unknown).is_err());
        let extra = r#"{"command":"create_task","text":"x","execute":true}"#;
        assert!(serde_json::from_str::<OperatorCommand>(extra).is_err());
        let nested_extra = r#"{"command":"create_scoped_task","objective":"turn it off","allowed_scope":{"application":"Browser fixture","targets":[{"control":"Background option","group":null,"disambiguator":null,"list":[]}],"no_unrelated_controls":true}}"#;
        assert!(serde_json::from_str::<OperatorCommand>(nested_extra).is_err());
    }

    #[test]
    fn browser_goal_confirmation_summary_is_fixed_to_the_fixture_profile() {
        let profile = BrowserPageProfile::FixtureBackgroundOption;
        let summary = GoalSummary {
            user_text: "enable the background option".into(),
            app: profile.app().into(),
            target_label: profile.label().into(),
            group: None,
            row: None,
            desired: ExpectedValue::Bool { value: true },
            confirmation_scope: ConfirmationScope::CurrentUi,
            max_effect_class: EffectClass::ReversibleEdit,
            allowed_scope: None,
            scoped_revisit: None,
            scoped_commit: None,
            browser_page_profile: Some(profile),
        };
        assert!(summary.validate().is_ok());

        let mut arbitrary_app = summary;
        arbitrary_app.app = "com.example.other".into();
        assert!(arbitrary_app.validate().is_err());
    }

    #[test]
    fn form_goal_summary_uses_context_label_and_requires_revisit() {
        let endpoint = |semantic_target: &str, label: &str| TargetSpec {
            semantic_target: crate::desktop::ids::SemanticTargetRef::new(semantic_target).unwrap(),
            label: label.into(),
            group: None,
            role_hint: Some("AXRadioButton".into()),
            row: None,
        };
        let summary = GoalSummary {
            user_text: "Fill the profile contact field".into(),
            app: "com.example.settings".into(),
            target_label: "Email".into(),
            group: Some("Profile".into()),
            row: None,
            desired: ExpectedValue::Text {
                value: "first line\nsecond line".into(),
            },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::ReversibleEdit,
            allowed_scope: Some(AllowedControlScope {
                application: "com.example.settings".into(),
                targets: vec![AllowedControlTarget {
                    control: "Email".into(),
                    group: None,
                    form: Some("Profile".into()),
                    disambiguator: None,
                }],
                no_unrelated_controls: true,
            }),
            scoped_revisit: Some(ScopedUiRevisit {
                home: endpoint("settings-home", "Settings"),
                away: endpoint("settings-account", "Account"),
            }),
            scoped_commit: None,
            browser_page_profile: None,
        };
        assert!(summary.validate().is_ok());

        let mut empty_text = summary.clone();
        empty_text.desired = ExpectedValue::Text {
            value: String::new(),
        };
        assert!(empty_text.validate().is_ok());

        let mut context_mismatch = summary.clone();
        context_mismatch.group = Some("Notifications".into());
        assert!(context_mismatch.validate().is_err());

        let mut number_value = summary.clone();
        number_value.desired = ExpectedValue::Number { value: 3 };
        assert!(number_value.validate().is_ok());
        number_value.desired = ExpectedValue::Number {
            value: super::super::task::MAX_SCOPED_FORM_SAFE_INTEGER + 1,
        };
        assert!(number_value.validate().is_err());

        let mut current_ui = summary;
        current_ui.confirmation_scope = ConfirmationScope::CurrentUi;
        current_ui.scoped_revisit = None;
        assert!(current_ui.validate().is_err());
    }

    #[test]
    fn observed_target_structure_sequence_is_positive_and_javascript_safe() {
        let schema = serde_json::to_value(schemars::schema_for!(ObservedTarget)).unwrap();
        assert_eq!(schema["properties"]["structure_sequence"]["minimum"], 1);
        assert_eq!(
            schema["properties"]["structure_sequence"]["maximum"],
            MAX_OBSERVED_TARGET_STRUCTURE_SEQUENCE
        );
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "structure_sequence")
        );
    }

    fn scoped_window_target() -> ScopedWindowTarget {
        ScopedWindowTarget {
            target: ObservedTarget {
                snapshot_id: RequestId::new("scoped-window-snapshot").unwrap(),
                structure_sequence: 7,
                session_epoch: 3,
                app: "com.example.workspace".into(),
                pid: 42,
                window_id: "window-main".into(),
                window_title: Some("Workspace".into()),
                frame_points: Some([10, 20, 500, 401]),
                observed_ms: 1_000,
                valid_until_ms: 3_000,
                contains_private_fields: false,
            },
            table_scope: RowTableScope {
                scroll_path: vec![RowContainerScope {
                    role: RowContainerRole::AxScrollArea,
                    label: "Projects".into(),
                }],
                column_headers: vec!["Name".into()],
            },
            source: SourceWindowContext {
                source_session_id: "source-session-3".into(),
                session_epoch: 3,
                process: KernelProcessIdentity {
                    pid: 42,
                    start_seconds: 1_700_000_000,
                    start_micros: 123_456,
                },
                origin_window_id: "window-main".into(),
                origin_window_frame: Rect {
                    x: 10.4,
                    y: 20.2,
                    width: 500.3,
                    height: 400.6,
                },
                verified_source_sequence: 7,
                verified_observed_ms: 1_000,
            },
            root_identity_hash: "a".repeat(64),
        }
    }

    fn observation(scoped: Option<ScopedWindowTarget>) -> ObservationView {
        ObservationView {
            target_app: Some("com.example.workspace".into()),
            target_name: Some("Workspace App".into()),
            active_window: Some("window-main".into()),
            observed_target: None,
            scoped_window_target: scoped,
            background_window_target: None,
            sources: vec![],
            frame: None,
        }
    }

    fn background_window_target() -> BackgroundWindowTarget {
        let app = "com.example.workspace";
        let frame = Rect {
            x: 10.4,
            y: 20.2,
            width: 500.3,
            height: 400.6,
        };
        let mut source = SourceBackgroundWindowContext {
            window: SourceWindowContext {
                source_session_id: "source-session-3".into(),
                session_epoch: 3,
                process: KernelProcessIdentity {
                    pid: 42,
                    start_seconds: 1_700_000_000,
                    start_micros: 123_456,
                },
                origin_window_id: "window-main".into(),
                origin_window_frame: frame,
                verified_source_sequence: 7,
                verified_observed_ms: 1_000,
            },
            hidden: ObservationFact::Known { value: false },
            minimized: ObservationFact::Known { value: false },
            target_context_hash: String::new(),
        };
        source.target_context_hash = source.computed_target_context_hash_for(app, 42).unwrap();
        BackgroundWindowTarget {
            snapshot_id: RequestId::new("background-window-snapshot").unwrap(),
            app: app.into(),
            pid: 42,
            window_id: "window-main".into(),
            window_title: Some("Workspace".into()),
            frame_points: [frame.x, frame.y, frame.width, frame.height]
                .map(|value| value.round() as i64),
            structure_sequence: 7,
            session_epoch: 3,
            observed_ms: 1_000,
            valid_until_ms: 3_000,
            source,
        }
    }

    #[test]
    fn background_target_is_independent_of_focus_and_uses_source_geometry() {
        let target = background_window_target();
        target.validate().unwrap();
        let mut projection = observation(None);
        projection.active_window = Some("window-other-app".into());
        projection.background_window_target = Some(target.clone());
        projection.validate().unwrap();

        let mut wrong_focus_app = projection.clone();
        wrong_focus_app.target_app = Some("com.example.other".into());
        assert!(wrong_focus_app.validate().is_err());

        let mut wrong_frame_projection = target.clone();
        wrong_frame_projection.frame_points[0] += 1;
        assert!(wrong_frame_projection.validate().is_err());

        let mut hidden = target.clone();
        hidden.source.hidden = ObservationFact::Known { value: true };
        hidden.source.target_context_hash = hidden
            .source
            .computed_target_context_hash_for(&hidden.app, hidden.pid)
            .unwrap();
        assert!(hidden.validate().is_err());

        let mut long_lived = target;
        long_lived.valid_until_ms = long_lived.observed_ms + 2_001;
        assert!(long_lived.validate().is_err());
    }

    #[test]
    fn scoped_window_target_requires_same_source_epoch_pid_window_time_scope_and_frame() {
        let valid = scoped_window_target();
        valid.validate().unwrap();

        let mut wrong_epoch = valid.clone();
        wrong_epoch.target.session_epoch += 1;
        assert!(wrong_epoch.validate().is_err());

        let mut wrong_pid = valid.clone();
        wrong_pid.target.pid += 1;
        assert!(wrong_pid.validate().is_err());

        let mut wrong_window = valid.clone();
        wrong_window.target.window_id = "window-other".into();
        assert!(wrong_window.validate().is_err());

        let mut wrong_sequence = valid.clone();
        wrong_sequence.target.structure_sequence += 1;
        assert!(wrong_sequence.validate().is_err());

        let mut wrong_time = valid.clone();
        wrong_time.target.observed_ms += 1;
        assert!(wrong_time.validate().is_err());

        let mut wrong_frame = valid.clone();
        wrong_frame.target.frame_points = Some([11, 20, 500, 401]);
        assert!(wrong_frame.validate().is_err());
    }

    #[test]
    fn scoped_window_target_requires_public_scope_known_title_short_ttl_and_digest_shape() {
        let valid = scoped_window_target();
        valid.validate().unwrap();

        let mut unknown_title = valid.clone();
        unknown_title.target.window_title = None;
        assert!(unknown_title.validate().is_err());

        let mut long_lived = valid.clone();
        long_lived.target.valid_until_ms = long_lived.target.observed_ms + 2_001;
        assert!(long_lived.validate().is_err());

        let mut expired = valid.clone();
        expired.target.valid_until_ms = expired.target.observed_ms;
        assert!(expired.validate().is_err());

        let mut invalid_scope = valid.clone();
        invalid_scope.table_scope.column_headers.clear();
        assert!(invalid_scope.validate().is_err());

        let mut prefixed_digest = valid.clone();
        prefixed_digest.root_identity_hash = format!("bc1:{}", "a".repeat(64));
        assert!(prefixed_digest.validate().is_err());

        let mut uppercase_digest = valid;
        uppercase_digest.root_identity_hash = "A".repeat(64);
        assert!(uppercase_digest.validate().is_err());
    }

    #[test]
    fn scoped_projection_is_optional_and_cannot_conflict_with_the_whole_target() {
        let mut legacy = observation(None);
        legacy.observed_target = Some(scoped_window_target().target);
        legacy.validate().unwrap();
        let value = serde_json::to_value(&legacy).unwrap();
        assert!(value.get("scoped_window_target").is_none());
        let decoded: ObservationView = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.scoped_window_target, None);
        assert!(decoded.observed_target.is_some());

        let schema = serde_json::to_value(schemars::schema_for!(ObservationView)).unwrap();
        assert!(schema["properties"]["scoped_window_target"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .is_none_or(|required| !required
                    .iter()
                    .any(|field| field == "scoped_window_target"))
        );

        let mut projection = observation(Some(scoped_window_target()));
        projection.target_app = Some("com.example.other".into());
        assert!(projection.validate().is_err());

        let mut conflicting_whole_target = observation(Some(scoped_window_target()));
        let mut old_target = conflicting_whole_target
            .scoped_window_target
            .as_ref()
            .unwrap()
            .target
            .clone();
        old_target.structure_sequence += 1;
        conflicting_whole_target.observed_target = Some(old_target);
        assert!(conflicting_whole_target.validate().is_err());

        let valid_projection = UiProjection {
            browser_observation: None,
            revision: 1,
            mode: RunMode::Observe,
            root: None,
            stop_status: StopStatus::NotStopped,
            cleanup: super::ReleaseStatus::NotNeeded,
            pending_confirmation: None,
            capabilities: None,
            timeline: vec![],
            observation: Some({
                let mut view = observation(Some(scoped_window_target()));
                view.scoped_window_target
                    .as_mut()
                    .unwrap()
                    .root_identity_hash = "bad".into();
                view
            }),
        };
        assert!(
            Message::UiProjection(Box::new(valid_projection))
                .validate()
                .is_err()
        );
    }
}
