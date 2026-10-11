//! Execution-facing messages between cu-host and cu-device.
//!
//! These mirror the I0 shapes, with plain `String` identifiers (see the
//! module docs of [`super`]), and add the desktop concerns: root epochs, stop
//! sequences and the binding proof used for the point-of-hit check.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::binding::{BindingProof, BindingProofScope, DeviceContextWitness, MenuBindingScope};
use super::browser_execution::{BrowserAction, BrowserActionIntent, BrowserExecutionObservation};
use super::domain::{DomainAction, ExecutionDomain};
use super::error::FailureCode;
use super::owned_input::OwnedDocumentBindingProof;
use super::primitive::{AxActionKind, Primitive, PrimitivePolicy};
use super::require_nonempty;
use super::source::validate_menu_id;
use crate::i0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceRef {
    #[schemars(length(min = 1))]
    pub id: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermitRef {
    #[schemars(length(min = 1))]
    pub id: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceFreshness {
    Verified,
    Uncertain,
    Failed,
}

/// Window metadata may authorize only a registered focus transition. It never
/// supplies the pixel/focus evidence required for ordinary target input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservationPurpose {
    #[default]
    TargetInput,
    WindowActivation,
    /// A current anchored level-one menu item; admits only one AXPress.
    MenuSelection,
    /// An actual focused nontext receiver; admits only one scoped KeyPress.
    FocusedControl,
    /// An independently verified owned plain-text receiver; only atomic SetValue.
    OwnedDocumentValueSetter,
    /// An independently verified owned plain-text receiver; only one exact Save.
    OwnedDocumentSave,
    /// One AXSelected=true action authorized by a complete local table witness.
    RowSelection,
    /// One purpose-bound AXPress in the background native AX domain.
    BackgroundAxAction,
    /// One fixed-profile action in the isolated headless browser domain.
    HeadlessBrowserAction,
    /// Reserved for a future real app-integration adapter.
    AppIntegrationAction,
}
impl ObservationPurpose {
    pub fn is_target_input(&self) -> bool {
        *self == Self::TargetInput
    }

    pub fn supports_execution_domain(&self, domain: ExecutionDomain) -> bool {
        match self {
            Self::BackgroundAxAction => domain == ExecutionDomain::BackgroundAx,
            Self::HeadlessBrowserAction => domain == ExecutionDomain::HeadlessBrowser,
            Self::AppIntegrationAction => domain == ExecutionDomain::AppIntegration,
            Self::TargetInput
            | Self::WindowActivation
            | Self::MenuSelection
            | Self::FocusedControl
            | Self::OwnedDocumentValueSetter
            | Self::OwnedDocumentSave
            | Self::RowSelection => {
                matches!(
                    domain,
                    ExecutionDomain::ForegroundNative | ExecutionDomain::Simulated
                )
            }
        }
    }
}

/// Evidence as the device sees it: a scoped, time-limited claim about one target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    #[schemars(length(min = 1))]
    pub observation_id: String,
    pub session_epoch: u64,
    pub surface: SurfaceRef,
    #[schemars(length(min = 1))]
    pub target_ref: String,
    #[schemars(length(min = 1))]
    pub clock_domain: String,
    pub source_sequence: u64,
    pub observed_ms: u64,
    pub valid_until_ms: u64,
    pub freshness: SourceFreshness,
    pub execution_domain: ExecutionDomain,
    #[serde(default, skip_serializing_if = "ObservationPurpose::is_target_input")]
    pub purpose: ObservationPurpose,
    /// Required by production pointer/AX admission, absent for I0 conversion.
    pub binding_proof: Option<BindingProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_document_proof: Option<Box<OwnedDocumentBindingProof>>,
    pub dependencies: BTreeMap<String, u64>,
}

/// Closed native/browser observation sum type for execution consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionObservation {
    Native {
        observation: Box<Observation>,
    },
    Browser {
        observation: Box<BrowserExecutionObservation>,
    },
}

impl ExecutionObservation {
    pub fn observation_id(&self) -> &str {
        match self {
            Self::Native { observation } => &observation.observation_id,
            Self::Browser { observation } => observation.observation_id(),
        }
    }

    pub fn session_epoch(&self) -> u64 {
        match self {
            Self::Native { observation } => observation.session_epoch,
            Self::Browser { observation } => observation.session_epoch(),
        }
    }

    pub fn surface(&self) -> &SurfaceRef {
        match self {
            Self::Native { observation } => &observation.surface,
            Self::Browser { observation } => observation.surface(),
        }
    }

    pub fn target_ref(&self) -> &str {
        match self {
            Self::Native { observation } => &observation.target_ref,
            Self::Browser { observation } => observation.target_ref(),
        }
    }

    pub fn clock_domain(&self) -> &str {
        match self {
            Self::Native { observation } => &observation.clock_domain,
            Self::Browser { observation } => observation.clock_domain(),
        }
    }

    pub fn source_sequence(&self) -> u64 {
        match self {
            Self::Native { observation } => observation.source_sequence,
            Self::Browser { observation } => observation.source_sequence(),
        }
    }

    pub fn observed_ms(&self) -> u64 {
        match self {
            Self::Native { observation } => observation.observed_ms,
            Self::Browser { observation } => observation.observed_ms(),
        }
    }

    pub fn valid_until_ms(&self) -> u64 {
        match self {
            Self::Native { observation } => observation.valid_until_ms,
            Self::Browser { observation } => observation.valid_until_ms(),
        }
    }

    pub fn freshness(&self) -> SourceFreshness {
        match self {
            Self::Native { observation } => observation.freshness,
            Self::Browser { observation } => observation.freshness(),
        }
    }

    pub fn dependencies(&self) -> &BTreeMap<String, u64> {
        match self {
            Self::Native { observation } => &observation.dependencies,
            Self::Browser { observation } => observation.dependencies(),
        }
    }

    pub fn native(&self) -> Option<&Observation> {
        match self {
            Self::Native { observation } => Some(observation),
            Self::Browser { .. } => None,
        }
    }

    pub fn browser(&self) -> Option<&BrowserExecutionObservation> {
        match self {
            Self::Native { .. } => None,
            Self::Browser { observation } => Some(observation),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Native { observation } => observation.validate(),
            Self::Browser { observation } => observation.validate(),
        }
    }
}

impl From<Observation> for ExecutionObservation {
    fn from(observation: Observation) -> Self {
        Self::Native {
            observation: Box::new(observation),
        }
    }
}

impl From<BrowserExecutionObservation> for ExecutionObservation {
    fn from(observation: BrowserExecutionObservation) -> Self {
        Self::Browser {
            observation: Box::new(observation),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    SurfacePx,
    /// Global display points, origin at the top-left of the main display.
    DisplayPoints,
    /// A stable accessibility object identity; this is not a pointer location.
    TargetObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduledPrimitive {
    pub offset_ms: u64,
    pub primitive: Primitive,
}

/// A bounded sequence of primitives with a fixed end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionIntent {
    #[schemars(length(min = 1))]
    pub intent_id: String,
    /// Correlates the intent with a host attempt; the device treats it as opaque.
    #[serde(default)]
    pub attempt_id: Option<String>,
    #[schemars(length(min = 1))]
    pub task_id: String,
    pub task_revision: u64,
    /// The root cancellation epoch this action was admitted under.
    #[schemars(range(min = 1))]
    pub root_epoch: u64,
    pub session_epoch: u64,
    pub execution_domain: ExecutionDomain,
    pub permit: PermitRef,
    pub surface: SurfaceRef,
    #[schemars(length(min = 1))]
    pub target_ref: String,
    /// Digest of the bound instance's fingerprint, checked against what is
    /// under the pointer immediately before a pointer or AX primitive.
    #[serde(default)]
    pub binding_fingerprint: Option<String>,
    pub binding_proof: Option<BindingProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_document_proof: Option<Box<OwnedDocumentBindingProof>>,
    pub coordinate_space: CoordinateSpace,
    pub dependencies: BTreeMap<String, u64>,
    #[schemars(length(min = 1))]
    pub evidence_id: String,
    pub latest_start_ms: u64,
    #[schemars(range(min = 1))]
    pub duration_ms: u64,
    #[schemars(length(min = 1))]
    pub steps: Vec<ScheduledPrimitive>,
}

/// Closed native/browser intent sum type. Its variants retain their own wire
/// representation rather than treating browser actions as native primitives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionIntent {
    Native { intent: Box<ActionIntent> },
    Browser { intent: Box<BrowserActionIntent> },
}

impl ExecutionIntent {
    pub fn intent_id(&self) -> &str {
        match self {
            Self::Native { intent } => &intent.intent_id,
            Self::Browser { intent } => &intent.intent_id,
        }
    }

    pub fn attempt_id(&self) -> Option<&str> {
        match self {
            Self::Native { intent } => intent.attempt_id.as_deref(),
            Self::Browser { intent } => Some(&intent.attempt_id),
        }
    }

    pub fn task_id(&self) -> &str {
        match self {
            Self::Native { intent } => &intent.task_id,
            Self::Browser { intent } => &intent.task_id,
        }
    }

    pub fn task_revision(&self) -> u64 {
        match self {
            Self::Native { intent } => intent.task_revision,
            Self::Browser { intent } => intent.task_revision,
        }
    }

    pub fn root_epoch(&self) -> u64 {
        match self {
            Self::Native { intent } => intent.root_epoch,
            Self::Browser { intent } => intent.root_epoch,
        }
    }

    pub fn session_epoch(&self) -> u64 {
        match self {
            Self::Native { intent } => intent.session_epoch,
            Self::Browser { intent } => intent.session_epoch,
        }
    }

    pub fn execution_domain(&self) -> ExecutionDomain {
        match self {
            Self::Native { intent } => intent.execution_domain,
            Self::Browser { .. } => ExecutionDomain::HeadlessBrowser,
        }
    }

    pub fn permit(&self) -> &PermitRef {
        match self {
            Self::Native { intent } => &intent.permit,
            Self::Browser { intent } => &intent.permit,
        }
    }

    pub fn surface(&self) -> &SurfaceRef {
        match self {
            Self::Native { intent } => &intent.surface,
            Self::Browser { intent } => &intent.evidence.surface,
        }
    }

    pub fn target_ref(&self) -> &str {
        match self {
            Self::Native { intent } => &intent.target_ref,
            Self::Browser { intent } => intent.evidence.target_ref(),
        }
    }

    pub fn evidence_id(&self) -> &str {
        match self {
            Self::Native { intent } => &intent.evidence_id,
            Self::Browser { intent } => intent.evidence.observation_id(),
        }
    }

    pub fn dependencies(&self) -> &BTreeMap<String, u64> {
        match self {
            Self::Native { intent } => &intent.dependencies,
            Self::Browser { intent } => intent.evidence.dependencies(),
        }
    }

    pub fn latest_start_ms(&self) -> u64 {
        match self {
            Self::Native { intent } => intent.latest_start_ms,
            Self::Browser { intent } => intent.latest_start_ms,
        }
    }

    pub fn duration_ms(&self) -> u64 {
        match self {
            Self::Native { intent } => intent.duration_ms,
            Self::Browser { intent } => intent.duration_ms,
        }
    }

    pub fn native(&self) -> Option<&ActionIntent> {
        match self {
            Self::Native { intent } => Some(intent),
            Self::Browser { .. } => None,
        }
    }

    pub fn browser(&self) -> Option<&BrowserActionIntent> {
        match self {
            Self::Native { .. } => None,
            Self::Browser { intent } => Some(intent),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Native { intent } => intent.validate(),
            Self::Browser { intent } => intent.validate(),
        }
    }

    pub fn validate_against_observation(
        &self,
        observation: &ExecutionObservation,
    ) -> Result<(), String> {
        match (self, observation) {
            (Self::Native { intent }, ExecutionObservation::Native { observation }) => {
                intent.validate_against_observation(observation)
            }
            (Self::Browser { intent }, ExecutionObservation::Browser { observation }) => {
                intent.validate_against_observation(observation)
            }
            _ => Err("execution intent and observation domains differ".into()),
        }
    }
}

impl From<ActionIntent> for ExecutionIntent {
    fn from(intent: ActionIntent) -> Self {
        Self::Native {
            intent: Box::new(intent),
        }
    }
}

impl From<BrowserActionIntent> for ExecutionIntent {
    fn from(intent: BrowserActionIntent) -> Self {
        Self::Browser {
            intent: Box::new(intent),
        }
    }
}

/// An intent plus the highest stop the sending host has processed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitIntent {
    pub acked_stop_seq: u64,
    pub intent: ExecutionIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Accepted,
    Running,
    Completed,
    Cancelled,
    Rejected,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStatus {
    NotNeeded,
    Held,
    Released,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubmittedAction {
    Native {
        at_ms: u64,
        primitive: Primitive,
        cleanup: bool,
    },
    Browser {
        at_ms: u64,
        action: BrowserAction,
    },
}

impl SubmittedAction {
    pub fn at_ms(&self) -> u64 {
        match self {
            Self::Native { at_ms, .. } | Self::Browser { at_ms, .. } => *at_ms,
        }
    }

    pub fn cleanup(&self) -> bool {
        match self {
            Self::Native { cleanup, .. } => *cleanup,
            Self::Browser { .. } => false,
        }
    }

    pub fn native_primitive(&self) -> Option<&Primitive> {
        match self {
            Self::Native { primitive, .. } => Some(primitive),
            Self::Browser { .. } => None,
        }
    }

    pub fn browser_action(&self) -> Option<&BrowserAction> {
        match self {
            Self::Native { .. } => None,
            Self::Browser { action, .. } => Some(action),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    #[schemars(length(min = 1))]
    pub intent_id: String,
    #[serde(default)]
    pub attempt_id: Option<String>,
    pub session_epoch: u64,
    pub status: ReceiptStatus,
    #[serde(default)]
    pub reason_code: Option<FailureCode>,
    pub reason: Option<String>,
    pub submitted: Vec<SubmittedAction>,
    pub release_status: ReleaseStatus,
    pub command_end_ms: Option<u64>,
    /// Actual end of input calls, independent of the fixed authority window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_end_ms: Option<u64>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeExecution {
    pub session_epoch: u64,
    pub permit: PermitRef,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenewPermit {
    pub session_epoch: u64,
    pub permit: PermitRef,
    #[schemars(length(min = 1))]
    pub evidence_id: String,
    pub valid_until_ms: u64,
}

/// Registers a frozen root task with the device. The deadline and epoch come
/// from the host and are never reset by re-registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RootGrant {
    #[schemars(length(min = 1))]
    pub root_id: String,
    #[schemars(range(min = 1))]
    pub revision: u64,
    #[schemars(range(min = 1))]
    pub root_epoch: u64,
    pub deadline_ms: u64,
}

/// Asks for the current receipt of an intent, for example after a timeout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryReceipt {
    pub session_epoch: u64,
    #[schemars(length(min = 1))]
    pub intent_id: String,
}

/// Asks the device to issue a permit for one target on fresh evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrantPermit {
    pub acked_stop_seq: u64,
    #[schemars(length(min = 1))]
    pub task_id: String,
    #[schemars(length(min = 1))]
    pub target_ref: String,
    #[schemars(length(min = 1))]
    pub evidence_id: String,
    pub valid_until_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum GrantOutcome {
    Granted {
        permit: PermitRef,
    },
    Denied {
        code: Option<FailureCode>,
        reason: String,
    },
}

/// Generic reply for commands that only need an accept or reject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandAck {
    pub accepted: bool,
    #[serde(default)]
    pub code: Option<FailureCode>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl CommandAck {
    pub fn ok() -> Self {
        Self {
            accepted: true,
            code: None,
            reason: None,
        }
    }

    pub fn rejected(code: Option<FailureCode>, reason: impl Into<String>) -> Self {
        Self {
            accepted: false,
            code,
            reason: Some(reason.into()),
        }
    }
}

/// The host tells the device a dependency changed. `valid: None` means unknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DependencyUpdate {
    #[schemars(length(min = 1))]
    pub name: String,
    pub valid: Option<bool>,
}

/// Current dependency revisions, returned after an update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DependencyVersions {
    pub versions: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum StopScope {
    All,
    Root { root_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopDisposition {
    /// Release the input but keep the root's semantic progress.
    Pause,
    /// Cancel the root. The device keeps a tombstone for it.
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopSource {
    OperatorButton,
    Tray,
    GlobalHotkey,
    DeviceLocal,
    HumanInput,
    Supervisor,
    HostLeaseLost,
    /// The host stopped on its own: a deadline, the budget or a failure.
    Host,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopCommand {
    pub scope: StopScope,
    pub disposition: StopDisposition,
    pub source: StopSource,
    #[schemars(length(min = 1))]
    pub reason: String,
}

/// The device's answer to a stop. It goes to main and to the host, and its
/// `stop_seq` is what the host must echo as `acked_stop_seq` afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopAck {
    pub stop_seq: u64,
    pub stop_floor: u64,
    pub scope: StopScope,
    pub disposition: StopDisposition,
    pub source: StopSource,
    pub revoked_permits: u32,
    pub cleanup: ReleaseStatus,
    pub at_ms: u64,
}

/// An explicit user resume. Only an explicit resume lifts a pause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResumeCommand {
    pub scope: StopScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResumeAck {
    pub resumed: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Available,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Scripted input backend; no real input is produced.
    Simulated,
    Macos,
    /// Real owned Chromium page adapter with its separate browser-control pipe.
    HeadlessBrowser,
}

/// Who the device is in this session. The host builds device evidence from it,
/// so it must match what the device itself checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceIdentity {
    pub session_epoch: u64,
    pub surface: SurfaceRef,
    pub clock_domain: String,
    /// Current dependency revisions. Later changes arrive as device events.
    pub dependencies: BTreeMap<String, u64>,
}

/// What this device can really do right now. Never a statement about what the
/// platform could do in principle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilityManifest {
    pub identity: DeviceIdentity,
    pub backend: BackendKind,
    /// The trusted device-profile domain selected for this session. This is
    /// independent from the per-domain capability advertisement below.
    pub execution_domain: ExecutionDomain,
    pub policy: PrimitivePolicy,
    pub human_input: CapabilityState,
    pub hit_test: CapabilityState,
    pub input: CapabilityState,
    /// One entry for every closed execution domain, including unavailable ones.
    #[schemars(length(min = 5, max = 5))]
    pub domains: Vec<ExecutionDomainCapability>,
    /// Longest stretch that cannot be interrupted once submitted.
    pub max_uninterruptible_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionDomainCapability {
    pub domain: ExecutionDomain,
    pub state: CapabilityState,
    #[schemars(length(max = 9))]
    pub actions: Vec<DomainAction>,
}

impl ExecutionDomainCapability {
    pub fn unavailable(domain: ExecutionDomain) -> Self {
        Self {
            domain,
            state: CapabilityState::Unavailable,
            actions: Vec::new(),
        }
    }

    pub fn all_unavailable() -> Vec<Self> {
        ExecutionDomain::ALL
            .into_iter()
            .map(Self::unavailable)
            .collect()
    }
}

impl CapabilityManifest {
    pub fn validate(&self) -> Result<(), String> {
        let selected_domain_matches_backend = matches!(
            (self.backend, self.execution_domain),
            (
                BackendKind::Macos,
                ExecutionDomain::ForegroundNative | ExecutionDomain::BackgroundAx
            ) | (BackendKind::Simulated, ExecutionDomain::Simulated)
                | (
                    BackendKind::HeadlessBrowser,
                    ExecutionDomain::HeadlessBrowser
                )
        );
        if !selected_domain_matches_backend {
            return Err("selected execution domain does not match the physical backend".into());
        }
        let mut seen = BTreeSet::new();
        for capability in &self.domains {
            if !seen.insert(capability.domain) {
                return Err("capability manifest repeats an execution domain".into());
            }
            let mut actions = BTreeSet::new();
            if capability
                .actions
                .iter()
                .any(|action| !actions.insert(*action))
            {
                return Err("execution-domain capability repeats an action".into());
            }
            match capability.state {
                CapabilityState::Available if capability.actions.is_empty() => {
                    return Err("available execution domain must name its actions".into());
                }
                CapabilityState::Unavailable | CapabilityState::Unknown
                    if !capability.actions.is_empty() =>
                {
                    return Err(
                        "unavailable or unknown execution domain cannot advertise actions".into(),
                    );
                }
                _ => {}
            }
            let allowed = match capability.domain {
                ExecutionDomain::ForegroundNative => [
                    Some(DomainAction::NativePointer),
                    Some(DomainAction::NativeKeyboard),
                    Some(DomainAction::NativeScroll),
                    Some(DomainAction::WindowActivation),
                    Some(DomainAction::ForegroundAx),
                    None,
                    None,
                    None,
                    None,
                ],
                ExecutionDomain::BackgroundAx => [
                    Some(DomainAction::BackgroundAxPress),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ],
                ExecutionDomain::HeadlessBrowser => [
                    Some(DomainAction::HeadlessBrowserAction),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ],
                ExecutionDomain::AppIntegration => [
                    Some(DomainAction::AppIntegrationAction),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ],
                ExecutionDomain::Simulated => [
                    Some(DomainAction::SimulatedAction),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ],
            };
            if capability
                .actions
                .iter()
                .any(|action| !allowed.contains(&Some(*action)))
            {
                return Err("execution-domain capability advertises a cross-domain action".into());
            }
            if capability.state == CapabilityState::Available {
                let backend_matches = matches!(
                    (self.backend, capability.domain),
                    (
                        BackendKind::Macos,
                        ExecutionDomain::ForegroundNative | ExecutionDomain::BackgroundAx
                    ) | (BackendKind::Simulated, ExecutionDomain::Simulated)
                        | (
                            BackendKind::HeadlessBrowser,
                            ExecutionDomain::HeadlessBrowser
                        )
                );
                if !backend_matches {
                    return Err(
                        "available execution domain does not match the physical backend".into(),
                    );
                }
            }
            if capability.domain == ExecutionDomain::AppIntegration
                && capability.state != CapabilityState::Unavailable
            {
                return Err(
                    "app-integration domain is unavailable in this protocol version".into(),
                );
            }
            if capability.domain == ExecutionDomain::HeadlessBrowser
                && capability.state != CapabilityState::Unavailable
                && self.backend != BackendKind::HeadlessBrowser
            {
                return Err(
                    "only the real headless browser backend may report browser-domain capability"
                        .into(),
                );
            }
        }
        let domains: BTreeSet<_> = self.domains.iter().map(|entry| entry.domain).collect();
        if domains.len() != ExecutionDomain::ALL.len()
            || ExecutionDomain::ALL
                .iter()
                .any(|domain| !domains.contains(domain))
        {
            return Err(
                "capability manifest must describe every execution domain exactly once".into(),
            );
        }
        if self
            .domains
            .iter()
            .any(|entry| entry.state == CapabilityState::Available)
            && self.input != CapabilityState::Available
        {
            return Err("an available execution domain requires available device input".into());
        }
        if self.backend == BackendKind::HeadlessBrowser
            && (self.execution_domain != ExecutionDomain::HeadlessBrowser
                || self.policy != PrimitivePolicy::AtomicOnly
                || self.human_input != CapabilityState::Unavailable
                || self.hit_test != CapabilityState::Unavailable)
        {
            return Err(
                "headless browser backend requires its own domain and unavailable native hit-test and human-input capabilities".into(),
            );
        }
        Ok(())
    }

    pub fn supports(&self, domain: ExecutionDomain, action: DomainAction) -> bool {
        self.domains.iter().any(|entry| {
            entry.domain == domain
                && entry.state == CapabilityState::Available
                && entry.actions.contains(&action)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceEventKind {
    Stopped {
        ack: StopAck,
    },
    Resumed {
        scope: StopScope,
    },
    HumanInput,
    DependencyChanged {
        name: String,
        revision: u64,
        valid: Option<bool>,
    },
    PermitRevoked {
        permit: PermitRef,
        reason: String,
    },
    ChannelIsolated {
        reason: String,
    },
    ChannelRecovered,
    CapabilityChanged {
        manifest: CapabilityManifest,
    },
    LeaseLost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceEvent {
    pub seq: u64,
    pub at_ms: u64,
    pub event: DeviceEventKind,
}

/// The host keeps its control lease alive. When the lease lapses the device
/// closes its submit gate on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaseRenew {
    pub lease_epoch: u64,
    #[schemars(range(min = 1))]
    pub valid_for_ms: u64,
}

/// A scripted fault for the simulated backend. Real backends reject these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SimFault {
    Rejected,
    UnknownApplied,
    UnknownNotApplied,
}

/// What is under the pointer, as the simulated backend should report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum SimHitTest {
    /// The bound element is under the pointer.
    Match,
    /// Something else is; the value is its fingerprint digest.
    Element {
        fingerprint: String,
    },
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SimOp {
    HoldExternalKey {
        key: String,
    },
    SetReleaseFailure {
        fail: bool,
    },
    FailSubmissionOn {
        call_index: u64,
        fault: SimFault,
    },
    SetHitTest {
        result: SimHitTest,
    },
    /// Pretend a person touched the keyboard or pointer.
    InjectHumanInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SimControl {
    pub op: SimOp,
}

fn validate_dependencies(dependencies: &BTreeMap<String, u64>) -> Result<(), String> {
    for name in dependencies.keys() {
        require_nonempty("dependency name", name)?;
    }
    Ok(())
}

impl Observation {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("observation_id", &self.observation_id)?;
        require_nonempty("surface.id", &self.surface.id)?;
        require_nonempty("target_ref", &self.target_ref)?;
        require_nonempty("clock_domain", &self.clock_domain)?;
        validate_dependencies(&self.dependencies)?;
        if !self
            .purpose
            .supports_execution_domain(self.execution_domain)
        {
            return Err("observation purpose differs from its frozen execution domain".into());
        }
        if matches!(
            self.execution_domain,
            ExecutionDomain::HeadlessBrowser | ExecutionDomain::AppIntegration
        ) {
            return Err("this execution domain has no available observation witness".into());
        }
        if self.binding_proof.is_some() && self.owned_document_proof.is_some() {
            return Err("generic and owned document proofs are mutually exclusive".into());
        }
        if let Some(proof) = &self.owned_document_proof {
            proof.validate()?;
            if !matches!(
                self.purpose,
                ObservationPurpose::OwnedDocumentValueSetter
                    | ObservationPurpose::OwnedDocumentSave
            ) || self.freshness != SourceFreshness::Verified
                || proof.binding.as_str() != self.target_ref
                || proof.session_epoch != self.session_epoch
                || proof.surface != self.surface
                || proof.snapshot_observed_ms != self.observed_ms
                || proof.receiver.provenance.source_sequence != self.source_sequence
                || proof.receiver.provenance.clock_domain != self.clock_domain
                || self.valid_until_ms > proof.receiver.provenance.valid_until_ms
                || self
                    .dependencies
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>()
                    != BTreeSet::from(["authorization", "focus", "window", "owned_scope"])
            {
                return Err(
                    "owned document observation has inconsistent scope or dependencies".into(),
                );
            }
        } else if matches!(
            self.purpose,
            ObservationPurpose::OwnedDocumentValueSetter | ObservationPurpose::OwnedDocumentSave
        ) {
            return Err("owned document observation requires its independent proof".into());
        }
        if let Some(proof) = &self.binding_proof {
            proof.validate()?;
            if proof.binding.as_str() != self.target_ref {
                return Err("observation binding target mismatch".into());
            }
            if !proof.supports_execution_domain(self.execution_domain) {
                return Err("observation proof differs from its frozen execution domain".into());
            }
            let table_viewport_scope = matches!(
                &proof.continuity.scope,
                BindingProofScope::TableViewport { .. }
            );
            let window_root_scope = matches!(
                &proof.continuity.scope,
                BindingProofScope::WindowRoot { .. }
            );
            if window_root_scope && self.purpose != ObservationPurpose::WindowActivation {
                return Err("window-root evidence is restricted to window activation".into());
            }
            if self.purpose == ObservationPurpose::RowSelection {
                if !proof.supports_row_selection()
                    || proof.continuity.source_sequence != self.source_sequence
                {
                    return Err(
                        "row-selection observation requires its dedicated table-row witness".into(),
                    );
                }
            } else if table_viewport_scope {
                return Err(
                    "table viewport evidence is restricted to the row-selection purpose".into(),
                );
            }
            if let Some(scope) = &proof.focus_scope {
                let context = &scope.context;
                if self.purpose != ObservationPurpose::FocusedControl
                    || self.freshness != SourceFreshness::Verified
                    || context.session_epoch != self.session_epoch
                    || context.verified_source_sequence != self.source_sequence
                    || context.verified_observed_ms != self.observed_ms
                {
                    return Err("focused-control evidence requires exact verified current Source provenance".into());
                }
            }
            let anchor = match &proof.menu_scope {
                Some(MenuBindingScope::Opener {
                    anchor,
                    attempt_deadline_ms,
                }) => {
                    if self.purpose != ObservationPurpose::TargetInput {
                        return Err("a menu opener proof is only ordinary target evidence".into());
                    }
                    if self.valid_until_ms > *attempt_deadline_ms {
                        return Err(
                            "menu opener evidence cannot outlive its activated attempt".into()
                        );
                    }
                    Some(anchor)
                }
                Some(MenuBindingScope::Item { context, opened_by }) => {
                    if self.purpose != ObservationPurpose::MenuSelection {
                        return Err("menu item evidence requires the menu selection purpose".into());
                    }
                    if self.valid_until_ms > opened_by.attempt_deadline_ms {
                        return Err("menu item evidence cannot outlive its opening attempt".into());
                    }
                    Some(&context.opener)
                }
                None => None,
            };
            if anchor.is_some_and(|a| {
                a.verified_source_sequence != self.source_sequence
                    || a.verified_observed_ms != self.observed_ms
            }) {
                return Err("menu evidence provenance must match its current source read".into());
            }
        }
        if self.purpose == ObservationPurpose::FocusedControl
            && self
                .binding_proof
                .as_ref()
                .is_none_or(|proof| proof.focus_scope.is_none())
        {
            return Err("focused-control evidence requires a complete actual focus proof".into());
        }
        if self.purpose == ObservationPurpose::RowSelection
            && self
                .binding_proof
                .as_ref()
                .is_none_or(|proof| !proof.supports_row_selection())
        {
            return Err(
                "row-selection evidence requires a complete dedicated table-row proof".into(),
            );
        }
        if self.purpose == ObservationPurpose::MenuSelection
            && !self
                .binding_proof
                .as_ref()
                .is_some_and(|p| matches!(p.menu_scope, Some(MenuBindingScope::Item { .. })))
        {
            return Err("menu selection requires a complete menu item proof".into());
        }
        if self.purpose == ObservationPurpose::WindowActivation {
            if self.execution_domain != ExecutionDomain::ForegroundNative
                && self.execution_domain != ExecutionDomain::Simulated
            {
                return Err(
                    "window activation is restricted to foreground native or simulated domains"
                        .into(),
                );
            }
            let proof = self
                .binding_proof
                .as_ref()
                .ok_or("window activation needs a complete or scoped window-root proof")?;
            if !proof.supports_window_activation() {
                return Err(
                    "window activation needs a supported standard nonmodal window root".into(),
                );
            }
            if let DeviceContextWitness::WindowRootV1 { source, .. } = &proof.device_context {
                let max_valid_until = self
                    .observed_ms
                    .checked_add(2_000)
                    .ok_or("window activation observation expiry overflows its time range")?;
                if self.freshness != SourceFreshness::Verified
                    || source.session_epoch != self.session_epoch
                    || source.verified_source_sequence != self.source_sequence
                    || source.verified_observed_ms != self.observed_ms
                    || self.valid_until_ms > max_valid_until
                {
                    return Err("window-root activation requires exact fresh Source provenance and a two-second TTL".into());
                }
            }
            if self
                .dependencies
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != BTreeSet::from(["authorization", "window"])
            {
                return Err(
                    "window activation has dedicated authorization/window dependencies".into(),
                );
            }
        }
        if self.purpose == ObservationPurpose::BackgroundAxAction {
            let proof = self
                .binding_proof
                .as_ref()
                .ok_or("background AX action needs its dedicated target witness")?;
            let DeviceContextWitness::BackgroundAxV1 { source, .. } = &proof.device_context else {
                return Err("background AX action requires a BackgroundAxV1 witness".into());
            };
            let max_valid_until = self
                .observed_ms
                .checked_add(2_000)
                .ok_or("background AX observation expiry overflows its time range")?;
            if self.execution_domain != ExecutionDomain::BackgroundAx
                || !proof.supports_background_ax_press()
                || self.freshness != SourceFreshness::Verified
                || source.window.session_epoch != self.session_epoch
                || source.window.verified_source_sequence != self.source_sequence
                || source.window.verified_observed_ms != self.observed_ms
                || proof.continuity.source_sequence != self.source_sequence
                || self.valid_until_ms > max_valid_until
                || self
                    .dependencies
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>()
                    != BTreeSet::from([
                        "authorization",
                        "background_scope",
                        "binding",
                        "geometry",
                        "window",
                    ])
            {
                return Err("background AX action requires exact fresh target-context provenance and a two-second TTL".into());
            }
        }
        if self.observed_ms >= self.valid_until_ms {
            return Err("observed_ms must be less than valid_until_ms".into());
        }
        Ok(())
    }
}

impl From<i0::Observation> for ExecutionObservation {
    fn from(observation: i0::Observation) -> Self {
        Observation::from(observation).into()
    }
}

impl ActionIntent {
    /// Checks that this action consumes the exact observation that authorized
    /// its target and execution domain. A permit alone cannot change domains.
    pub fn validate_against_observation(&self, observation: &Observation) -> Result<(), String> {
        self.validate()?;
        observation.validate()?;
        if self.execution_domain != observation.execution_domain {
            return Err("intent and observation execution domains differ".into());
        }
        if self.session_epoch != observation.session_epoch
            || self.surface != observation.surface
            || self.target_ref != observation.target_ref
            || self.evidence_id != observation.observation_id
        {
            return Err("intent does not consume the exact observation target and identity".into());
        }
        if self.execution_domain == ExecutionDomain::BackgroundAx
            && (self.binding_proof != observation.binding_proof
                || self.dependencies != observation.dependencies)
        {
            return Err(
                "background AX intent must retain the observation's exact target proof and dependencies".into(),
            );
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("intent_id", &self.intent_id)?;
        require_nonempty("task_id", &self.task_id)?;
        require_nonempty("permit.id", &self.permit.id)?;
        require_nonempty("surface.id", &self.surface.id)?;
        require_nonempty("target_ref", &self.target_ref)?;
        require_nonempty("evidence_id", &self.evidence_id)?;
        if let Some(attempt) = &self.attempt_id {
            require_nonempty("attempt_id", attempt)?;
        }
        if let Some(fingerprint) = &self.binding_fingerprint {
            require_nonempty("binding_fingerprint", fingerprint)?;
        }
        validate_dependencies(&self.dependencies)?;
        if matches!(
            self.execution_domain,
            ExecutionDomain::HeadlessBrowser | ExecutionDomain::AppIntegration
        ) {
            return Err("this execution domain has no available action witness".into());
        }
        if self.coordinate_space == CoordinateSpace::TargetObject
            && self.execution_domain != ExecutionDomain::BackgroundAx
        {
            return Err("target-object coordinate space is restricted to background AX".into());
        }
        if self.execution_domain == ExecutionDomain::BackgroundAx && self.binding_proof.is_none() {
            return Err("background AX action requires its dedicated binding proof".into());
        }
        if self.coordinate_space == CoordinateSpace::TargetObject && self.binding_proof.is_none() {
            return Err("target-object action requires its dedicated binding proof".into());
        }
        if self.binding_proof.is_some() && self.owned_document_proof.is_some() {
            return Err("generic and owned document proofs are mutually exclusive".into());
        }
        if let Some(proof) = &self.owned_document_proof {
            proof.validate()?;
            if self.binding_fingerprint.is_some()
                || proof.binding.as_str() != self.target_ref
                || proof.session_epoch != self.session_epoch
                || proof.surface != self.surface
                || self.coordinate_space != CoordinateSpace::DisplayPoints
                || self.execution_domain != ExecutionDomain::ForegroundNative
                || self.attempt_id.is_none()
                || self.task_revision == 0
                || self.root_epoch == 0
                || self.duration_ms == 0
                || self.duration_ms > 1500
                || self
                    .latest_start_ms
                    .checked_add(self.duration_ms)
                    .is_none_or(|end| end > proof.receiver.provenance.valid_until_ms)
                || self.steps.len() != 1
                || self.steps[0].offset_ms != 0
                || self
                    .dependencies
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>()
                    != BTreeSet::from(["authorization", "focus", "window", "owned_scope"])
            {
                return Err(
                    "owned document intent has inconsistent scope, step or deadline".into(),
                );
            }
            if let Primitive::AxAction {
                action: AxActionKind::SetValue { value },
            } = &self.steps[0].primitive
            {
                OwnedDocumentBindingProof::validate_setter_value(value)?;
            } else if let Primitive::AxAction {
                action: AxActionKind::SaveOwnedDocument { expected_value },
            } = &self.steps[0].primitive
            {
                if expected_value.len() > 2048 || expected_value.contains('\0') {
                    return Err(
                        "owned document save value must be NUL-free and at most 2048 bytes".into(),
                    );
                }
            } else {
                return Err(
                    "owned scope accepts only one atomic AX setter or document save".into(),
                );
            }
        } else if self.steps.iter().any(|step| {
            matches!(
                step.primitive,
                Primitive::AxAction {
                    action: AxActionKind::SaveOwnedDocument { .. }
                }
            )
        }) {
            return Err("owned document save requires its independent proof".into());
        }
        if let Some(proof) = &self.binding_proof {
            proof.validate()?;
            if !proof.supports_execution_domain(self.execution_domain) {
                return Err("intent proof differs from its frozen execution domain".into());
            }
            if proof.binding.as_str() != self.target_ref
                || self.binding_fingerprint.as_deref() != Some(proof.fingerprint.digest().as_str())
                || (self.execution_domain == ExecutionDomain::BackgroundAx
                    && self.coordinate_space != CoordinateSpace::TargetObject)
                || (self.execution_domain != ExecutionDomain::BackgroundAx
                    && self.coordinate_space != CoordinateSpace::DisplayPoints)
            {
                return Err(
                    "intent binding descriptor, digest or coordinate space mismatch".into(),
                );
            }
            if matches!(
                &proof.continuity.scope,
                BindingProofScope::WindowRoot { .. }
            ) {
                if self.execution_domain != ExecutionDomain::ForegroundNative
                    && self.execution_domain != ExecutionDomain::Simulated
                {
                    return Err(
                        "window-root proof is restricted to foreground native activation".into(),
                    );
                }
                let exact_window_activation = proof.supports_window_activation()
                    && matches!(
                        &proof.device_context,
                        DeviceContextWitness::WindowRootV1 { source, .. }
                            if source.session_epoch == self.session_epoch
                    )
                    && self.steps.len() == 1
                    && self.steps[0].offset_ms == 0
                    && matches!(
                        &self.steps[0].primitive,
                        Primitive::ActivateWindow { app } if app == &proof.fingerprint.app
                    )
                    && self
                        .dependencies
                        .keys()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>()
                        == BTreeSet::from(["authorization", "window"]);
                if !exact_window_activation {
                    return Err(
                        "window-root proof authorizes only one matching ActivateWindow primitive"
                            .into(),
                    );
                }
            }
            if matches!(
                &proof.continuity.scope,
                BindingProofScope::BackgroundAx { .. }
            ) {
                let source = match &proof.device_context {
                    DeviceContextWitness::BackgroundAxV1 { source, .. } => source,
                    _ => return Err("background AX scope requires its dedicated witness".into()),
                };
                let dependency_names: BTreeSet<_> =
                    self.dependencies.keys().map(String::as_str).collect();
                let expires_at = source
                    .window
                    .verified_observed_ms
                    .checked_add(2_000)
                    .ok_or("background AX input deadline overflows its time range")?;
                if self.execution_domain != ExecutionDomain::BackgroundAx
                    || !proof.supports_background_ax_press()
                    || self.coordinate_space != CoordinateSpace::TargetObject
                    || self.attempt_id.is_none()
                    || self.session_epoch != source.window.session_epoch
                    || self.latest_start_ms < source.window.verified_observed_ms
                    || self
                        .latest_start_ms
                        .checked_add(self.duration_ms)
                        .is_none_or(|end| end > expires_at)
                    || self.steps.len() != 1
                    || self.steps[0].offset_ms != 0
                    || self.steps[0].primitive
                        != (Primitive::AxAction {
                            action: AxActionKind::Press,
                        })
                    || dependency_names
                        != BTreeSet::from([
                            "authorization",
                            "background_scope",
                            "binding",
                            "geometry",
                            "window",
                        ])
                {
                    return Err(
                        "background AX scope authorizes only one fresh target-object AXPress"
                            .into(),
                    );
                }
            }
            if let Some(scope) = &proof.menu_scope {
                validate_menu_id("menu intent id", &self.intent_id)?;
                let attempt = self
                    .attempt_id
                    .as_deref()
                    .ok_or("menu input must correlate to its owning attempt")?;
                validate_menu_id("menu attempt id", attempt)?;
                let (expected, deadline) = match scope {
                    MenuBindingScope::Opener {
                        attempt_deadline_ms,
                        ..
                    } => (AxActionKind::ShowMenu, *attempt_deadline_ms),
                    MenuBindingScope::Item { opened_by, .. } => {
                        if opened_by.attempt_id.as_str() != attempt
                            || opened_by.intent_id == self.intent_id
                        {
                            return Err("menu selection must follow a distinct opening intent in the same attempt".into());
                        }
                        (AxActionKind::Press, opened_by.attempt_deadline_ms)
                    }
                };
                if self
                    .latest_start_ms
                    .checked_add(self.duration_ms)
                    .is_none_or(|end| end > deadline)
                {
                    return Err(
                        "menu input authority cannot extend past the original attempt deadline"
                            .into(),
                    );
                }
                if self.steps.len() != 1
                    || self.steps[0].offset_ms != 0
                    || self.steps[0].primitive != (Primitive::AxAction { action: expected })
                {
                    return Err(
                        "menu scopes authorize only one offset-zero matching AX action".into(),
                    );
                }
            }
            if let Some(scope) = &proof.focus_scope {
                let attempt = self
                    .attempt_id
                    .as_deref()
                    .ok_or("focused key input must correlate to its owning attempt")?;
                validate_menu_id("focused key attempt id", attempt)?;
                validate_menu_id("focused key intent id", &self.intent_id)?;
                if self.session_epoch != scope.context.session_epoch
                    || self.latest_start_ms < scope.context.verified_observed_ms
                    || self.steps.len() != 1
                    || self.steps[0].offset_ms != 0
                    || !matches!(&self.steps[0].primitive, Primitive::KeyPress { key, modifiers }
                        if key == scope.key.canonical_name() && *modifiers == super::primitive::Modifiers::default())
                {
                    return Err("focused-control scope authorizes only one offset-zero canonical unmodified KeyPress".into());
                }
            }
            for step in &self.steps {
                if let Primitive::Click { x, y, .. } = step.primitive
                    && !proof.display_frame.contains(f64::from(x), f64::from(y))
                {
                    return Err("click is outside the bound display frame".into());
                }
            }
        }
        let row_scope = self.binding_proof.as_ref().is_some_and(|proof| {
            matches!(
                &proof.continuity.scope,
                super::binding::BindingProofScope::TableViewport { .. }
            )
        });
        let set_selected_true = self.steps.iter().any(|step| {
            step.primitive
                == (Primitive::AxAction {
                    action: AxActionKind::SetSelectedTrue,
                })
        });
        let exact_row_selection_action = self.steps.len() == 1
            && self.steps[0].offset_ms == 0
            && self.steps[0].primitive
                == (Primitive::AxAction {
                    action: AxActionKind::SetSelectedTrue,
                });
        if (row_scope || set_selected_true)
            && (!row_scope
                || !set_selected_true
                || !exact_row_selection_action
                || self
                    .binding_proof
                    .as_ref()
                    .is_none_or(|proof| !proof.supports_row_selection()))
        {
            return Err("AXSelected=true requires exactly one offset-zero action and a dedicated table-row proof".into());
        }
        if self.root_epoch == 0 {
            return Err("root_epoch must be at least 1".into());
        }
        if self.duration_ms == 0 {
            return Err("duration_ms must be greater than zero".into());
        }
        if self.latest_start_ms.checked_add(self.duration_ms).is_none() {
            return Err("latest_start_ms + duration_ms exceeds the u64 time range".into());
        }
        if self.steps.is_empty() {
            return Err("steps must not be empty".into());
        }

        let mut previous_offset = None;
        let mut held_keys = BTreeSet::new();
        let mut held_buttons = BTreeSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            if step.offset_ms >= self.duration_ms {
                return Err(format!(
                    "steps[{index}].offset_ms must be less than duration_ms"
                ));
            }
            if previous_offset.is_some_and(|previous| step.offset_ms <= previous) {
                return Err("step offsets must be strictly increasing".into());
            }
            previous_offset = Some(step.offset_ms);
            step.primitive.validate()?;

            match &step.primitive {
                Primitive::KeyDown { key } if !held_keys.insert(key.as_str()) => {
                    return Err(format!(
                        "steps[{index}] presses a key already held by this action"
                    ));
                }
                Primitive::KeyUp { key } if !held_keys.remove(key.as_str()) => {
                    return Err(format!(
                        "steps[{index}] releases a key not held by this action"
                    ));
                }
                Primitive::PointerDown { button } if !held_buttons.insert(*button) => {
                    return Err(format!(
                        "steps[{index}] presses a button already held by this action"
                    ));
                }
                Primitive::PointerUp { button } if !held_buttons.remove(button) => {
                    return Err(format!(
                        "steps[{index}] releases a button not held by this action"
                    ));
                }
                _ => {}
            }
        }
        // Remaining held inputs are valid; the executor owns deadline cleanup.
        Ok(())
    }
}

impl Receipt {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("intent_id", &self.intent_id)?;
        let mut previous_time = None;
        for submitted in &self.submitted {
            if let Some(primitive) = submitted.native_primitive() {
                primitive.validate()?;
            }
            if let Some(action) = submitted.browser_action() {
                action.validate()?;
            }
            if submitted.at_ms() > self.at_ms {
                return Err("submitted input must not be later than receipt.at_ms".into());
            }
            if previous_time.is_some_and(|previous| submitted.at_ms() < previous) {
                return Err("submitted input times must not decrease".into());
            }
            previous_time = Some(submitted.at_ms());
        }
        if let Some(end) = self.input_end_ms {
            let last = self
                .submitted
                .iter()
                .filter(|submitted| !submitted.cleanup())
                .map(SubmittedAction::at_ms)
                .max();
            if last.is_none_or(|last| end < last) || end > self.at_ms {
                return Err(
                    "actual input end must cover normal input and precede receipt time".into(),
                );
            }
        }
        Ok(())
    }
}

impl RootGrant {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("root_id", &self.root_id)?;
        if self.revision == 0 || self.root_epoch == 0 || self.deadline_ms == 0 {
            return Err("root revision, epoch and deadline must be positive".into());
        }
        Ok(())
    }
}

impl GrantPermit {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("task_id", &self.task_id)?;
        require_nonempty("target_ref", &self.target_ref)?;
        require_nonempty("evidence_id", &self.evidence_id)
    }
}

impl StopCommand {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("reason", &self.reason)?;
        if let StopScope::Root { root_id } = &self.scope {
            require_nonempty("scope.root_id", root_id)?;
        }
        Ok(())
    }
}

// ---- I0 compatibility -----------------------------------------------------

impl From<i0::SurfaceRef> for SurfaceRef {
    fn from(value: i0::SurfaceRef) -> Self {
        Self {
            id: value.id,
            epoch: value.epoch,
        }
    }
}

impl From<SurfaceRef> for i0::SurfaceRef {
    fn from(value: SurfaceRef) -> Self {
        Self {
            id: value.id,
            epoch: value.epoch,
        }
    }
}

impl From<i0::PermitRef> for PermitRef {
    fn from(value: i0::PermitRef) -> Self {
        Self {
            id: value.id,
            generation: value.generation,
        }
    }
}

impl From<PermitRef> for i0::PermitRef {
    fn from(value: PermitRef) -> Self {
        Self {
            id: value.id,
            generation: value.generation,
        }
    }
}

impl From<i0::SourceFreshness> for SourceFreshness {
    fn from(value: i0::SourceFreshness) -> Self {
        match value {
            i0::SourceFreshness::Verified => Self::Verified,
            i0::SourceFreshness::Uncertain => Self::Uncertain,
            i0::SourceFreshness::Failed => Self::Failed,
        }
    }
}

impl From<i0::Observation> for Observation {
    fn from(value: i0::Observation) -> Self {
        Self {
            observation_id: value.observation_id,
            session_epoch: value.session_epoch,
            surface: value.surface.into(),
            target_ref: value.target_ref,
            clock_domain: value.clock_domain,
            source_sequence: value.source_sequence,
            observed_ms: value.observed_ms,
            valid_until_ms: value.valid_until_ms,
            freshness: value.freshness.into(),
            execution_domain: ExecutionDomain::Simulated,
            purpose: Default::default(),
            owned_document_proof: None,
            binding_proof: None,
            dependencies: value.dependencies,
        }
    }
}

impl From<i0::RevokeExecution> for RevokeExecution {
    fn from(value: i0::RevokeExecution) -> Self {
        Self {
            session_epoch: value.session_epoch,
            permit: value.permit.into(),
            reason: value.reason,
        }
    }
}

impl From<i0::RenewPermit> for RenewPermit {
    fn from(value: i0::RenewPermit) -> Self {
        Self {
            session_epoch: value.session_epoch,
            permit: value.permit.into(),
            evidence_id: value.evidence_id,
            valid_until_ms: value.valid_until_ms,
        }
    }
}

impl From<i0::ActionIntent> for SubmitIntent {
    /// An I0 intent runs under root epoch 1 and has seen no stop.
    fn from(value: i0::ActionIntent) -> Self {
        let steps = value
            .steps
            .into_iter()
            .map(|step| ScheduledPrimitive {
                offset_ms: step.offset_ms,
                primitive: step.input.into(),
            })
            .collect();
        Self {
            acked_stop_seq: 0,
            intent: ActionIntent {
                intent_id: value.intent_id,
                attempt_id: None,
                task_id: value.task_id,
                task_revision: value.task_revision,
                root_epoch: 1,
                session_epoch: value.session_epoch,
                execution_domain: ExecutionDomain::Simulated,
                permit: value.permit.into(),
                surface: value.surface.into(),
                target_ref: value.target_ref,
                binding_fingerprint: None,
                owned_document_proof: None,
                binding_proof: None,
                coordinate_space: CoordinateSpace::SurfacePx,
                dependencies: value.dependencies,
                evidence_id: value.evidence_id,
                latest_start_ms: value.latest_start_ms,
                duration_ms: value.duration_ms,
                steps,
            }
            .into(),
        }
    }
}

impl From<i0::ReceiptStatus> for ReceiptStatus {
    fn from(value: i0::ReceiptStatus) -> Self {
        match value {
            i0::ReceiptStatus::Accepted => Self::Accepted,
            i0::ReceiptStatus::Running => Self::Running,
            i0::ReceiptStatus::Completed => Self::Completed,
            i0::ReceiptStatus::Cancelled => Self::Cancelled,
            i0::ReceiptStatus::Rejected => Self::Rejected,
            i0::ReceiptStatus::Partial => Self::Partial,
            i0::ReceiptStatus::Unknown => Self::Unknown,
        }
    }
}

impl From<ReceiptStatus> for i0::ReceiptStatus {
    fn from(value: ReceiptStatus) -> Self {
        match value {
            ReceiptStatus::Accepted => Self::Accepted,
            ReceiptStatus::Running => Self::Running,
            ReceiptStatus::Completed => Self::Completed,
            ReceiptStatus::Cancelled => Self::Cancelled,
            ReceiptStatus::Rejected => Self::Rejected,
            ReceiptStatus::Partial => Self::Partial,
            ReceiptStatus::Unknown => Self::Unknown,
        }
    }
}

impl From<ReleaseStatus> for i0::ReleaseStatus {
    fn from(value: ReleaseStatus) -> Self {
        match value {
            ReleaseStatus::NotNeeded => Self::NotNeeded,
            ReleaseStatus::Held => Self::Held,
            ReleaseStatus::Released => Self::Released,
            ReleaseStatus::Failed => Self::Failed,
            ReleaseStatus::Unknown => Self::Unknown,
        }
    }
}

/// The receipt contains something I0 cannot express.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnI0Receipt;

impl TryFrom<Receipt> for i0::Receipt {
    type Error = NotAnI0Receipt;

    fn try_from(value: Receipt) -> Result<Self, NotAnI0Receipt> {
        let submitted = value
            .submitted
            .into_iter()
            .map(|entry| {
                let SubmittedAction::Native {
                    at_ms,
                    primitive,
                    cleanup,
                } = entry
                else {
                    return Err(NotAnI0Receipt);
                };
                Ok(i0::SubmittedInput {
                    at_ms,
                    input: primitive.try_into().map_err(|_| NotAnI0Receipt)?,
                    cleanup,
                })
            })
            .collect::<Result<Vec<_>, NotAnI0Receipt>>()?;
        Ok(Self {
            intent_id: value.intent_id,
            session_epoch: value.session_epoch,
            status: value.status.into(),
            reason: value.reason,
            submitted,
            release_status: value.release_status.into(),
            command_end_ms: value.command_end_ms,
            at_ms: value.at_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::domain::{DomainAction, ExecutionDomain};
    use crate::desktop::primitive::{AxActionKind, Modifiers, MouseButton};

    pub(crate) fn intent() -> ActionIntent {
        ActionIntent {
            intent_id: "intent-1".into(),
            attempt_id: Some("attempt-1".into()),
            task_id: "task-1".into(),
            task_revision: 1,
            root_epoch: 1,
            session_epoch: 1,
            execution_domain: ExecutionDomain::Simulated,
            permit: PermitRef {
                id: "permit-1".into(),
                generation: 1,
            },
            surface: SurfaceRef {
                id: "surface-1".into(),
                epoch: 1,
            },
            target_ref: "target-1".into(),
            binding_fingerprint: Some("fp1:abc".into()),
            owned_document_proof: None,
            binding_proof: None,
            coordinate_space: CoordinateSpace::DisplayPoints,
            dependencies: BTreeMap::from([("focus".into(), 1)]),
            evidence_id: "obs-1".into(),
            latest_start_ms: 10,
            duration_ms: 20,
            steps: vec![ScheduledPrimitive {
                offset_ms: 0,
                primitive: Primitive::AxAction {
                    action: AxActionKind::SetValue {
                        value: "off".into(),
                    },
                },
            }],
        }
    }

    #[test]
    fn a_well_formed_atomic_intent_validates() {
        assert!(intent().validate().is_ok());
    }

    #[test]
    fn capability_manifest_closes_domains_and_keeps_unimplemented_domains_unavailable() {
        let manifest = crate::desktop::samples::manifest();
        assert!(manifest.validate().is_ok());
        assert!(manifest.supports(ExecutionDomain::Simulated, DomainAction::SimulatedAction));
        assert!(!manifest.supports(
            ExecutionDomain::ForegroundNative,
            DomainAction::NativePointer
        ));

        let mut no_capabilities = manifest.clone();
        no_capabilities.input = CapabilityState::Unavailable;
        no_capabilities.domains = ExecutionDomainCapability::all_unavailable();
        assert!(no_capabilities.validate().is_ok());

        let mut macos_profile = no_capabilities.clone();
        macos_profile.backend = BackendKind::Macos;
        macos_profile.execution_domain = ExecutionDomain::BackgroundAx;
        assert!(macos_profile.validate().is_ok());
        macos_profile.execution_domain = ExecutionDomain::HeadlessBrowser;
        assert!(macos_profile.validate().is_err());

        let mut duplicate = manifest.clone();
        duplicate.domains[0].domain = ExecutionDomain::Simulated;
        assert!(duplicate.validate().is_err());

        let mut fake_headless = manifest.clone();
        let headless = fake_headless
            .domains
            .iter_mut()
            .find(|entry| entry.domain == ExecutionDomain::HeadlessBrowser)
            .unwrap();
        headless.state = CapabilityState::Unknown;
        assert!(fake_headless.validate().is_err());

        let mut browser_profile = manifest.clone();
        browser_profile.backend = BackendKind::HeadlessBrowser;
        browser_profile.execution_domain = ExecutionDomain::HeadlessBrowser;
        browser_profile.human_input = CapabilityState::Unavailable;
        browser_profile.hit_test = CapabilityState::Unavailable;
        browser_profile.domains.iter_mut().for_each(|entry| {
            if entry.domain == ExecutionDomain::Simulated {
                entry.state = CapabilityState::Unavailable;
                entry.actions.clear();
            }
            if entry.domain == ExecutionDomain::HeadlessBrowser {
                entry.state = CapabilityState::Available;
                entry.actions = vec![DomainAction::HeadlessBrowserAction];
            }
        });
        assert!(browser_profile.validate().is_ok());
        assert!(browser_profile.supports(
            ExecutionDomain::HeadlessBrowser,
            DomainAction::HeadlessBrowserAction
        ));

        let mut browser_with_native_hit_test = browser_profile.clone();
        browser_with_native_hit_test.hit_test = CapabilityState::Available;
        assert!(browser_with_native_hit_test.validate().is_err());

        let mut simulated_advertises_browser = manifest.clone();
        simulated_advertises_browser
            .domains
            .iter_mut()
            .find(|entry| entry.domain == ExecutionDomain::HeadlessBrowser)
            .unwrap()
            .state = CapabilityState::Unknown;
        assert!(simulated_advertises_browser.validate().is_err());

        let mut wrong_backend = manifest;
        wrong_backend.backend = BackendKind::Macos;
        assert!(wrong_backend.validate().is_err());
    }

    #[test]
    fn action_intent_consumes_only_its_observation_domain_and_identity() {
        let observation = Observation {
            observation_id: "obs-1".into(),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "surface-1".into(),
                epoch: 1,
            },
            target_ref: "target-1".into(),
            clock_domain: "sim/1/monotonic".into(),
            source_sequence: 1,
            observed_ms: 1,
            valid_until_ms: 100,
            freshness: SourceFreshness::Verified,
            execution_domain: ExecutionDomain::Simulated,
            purpose: ObservationPurpose::TargetInput,
            binding_proof: None,
            owned_document_proof: None,
            dependencies: BTreeMap::from([("focus".into(), 1)]),
        };
        let intent = intent();
        assert!(intent.validate_against_observation(&observation).is_ok());

        let mut cross_domain = intent.clone();
        cross_domain.execution_domain = ExecutionDomain::ForegroundNative;
        assert!(
            cross_domain
                .validate_against_observation(&observation)
                .is_err()
        );

        let mut wrong_target = intent;
        wrong_target.target_ref = "other-target".into();
        assert!(
            wrong_target
                .validate_against_observation(&observation)
                .is_err()
        );

        let mut unproved_background = Observation {
            execution_domain: ExecutionDomain::BackgroundAx,
            purpose: ObservationPurpose::BackgroundAxAction,
            ..observation
        };
        assert!(unproved_background.validate().is_err());
        unproved_background.execution_domain = ExecutionDomain::Simulated;
        unproved_background.purpose = ObservationPurpose::TargetInput;
        assert!(unproved_background.validate().is_ok());
    }

    #[test]
    fn intent_rejects_bad_epochs_times_and_offsets() {
        let mut bad = intent();
        bad.root_epoch = 0;
        assert!(bad.validate().is_err());
        bad = intent();
        bad.duration_ms = 0;
        assert!(bad.validate().is_err());
        bad = intent();
        bad.latest_start_ms = u64::MAX;
        assert!(bad.validate().is_err());
        bad = intent();
        bad.steps.clear();
        assert!(bad.validate().is_err());
        bad = intent();
        bad.steps.push(ScheduledPrimitive {
            offset_ms: 20,
            primitive: Primitive::Scroll { dx: 0, dy: 1 },
        });
        assert!(bad.validate().is_err(), "offset must be below the duration");
        bad = intent();
        bad.steps.push(ScheduledPrimitive {
            offset_ms: 0,
            primitive: Primitive::Scroll { dx: 0, dy: 1 },
        });
        assert!(bad.validate().is_err(), "offsets must strictly increase");
    }

    #[test]
    fn held_primitives_are_tracked_within_one_intent() {
        let mut held = intent();
        held.steps = vec![
            ScheduledPrimitive {
                offset_ms: 0,
                primitive: Primitive::KeyDown { key: "W".into() },
            },
            ScheduledPrimitive {
                offset_ms: 1,
                primitive: Primitive::KeyDown { key: "W".into() },
            },
        ];
        assert!(held.validate().is_err());
        held.steps[1].primitive = Primitive::KeyUp { key: "A".into() };
        assert!(held.validate().is_err());
        held.steps[1].primitive = Primitive::KeyUp { key: "W".into() };
        assert!(held.validate().is_ok());
        held.steps.push(ScheduledPrimitive {
            offset_ms: 2,
            primitive: Primitive::PointerDown {
                button: MouseButton::Middle,
            },
        });
        assert!(
            held.validate().is_ok(),
            "held inputs left at the end are the executor's to clean up"
        );
    }

    #[test]
    fn receipt_times_must_not_run_backwards() {
        let mut receipt = Receipt {
            intent_id: "intent-1".into(),
            attempt_id: None,
            session_epoch: 1,
            status: ReceiptStatus::Running,
            reason_code: None,
            reason: None,
            submitted: vec![SubmittedAction::Native {
                at_ms: 10,
                primitive: Primitive::Click {
                    button: MouseButton::Left,
                    x: 1,
                    y: 1,
                    modifiers: Modifiers::default(),
                },
                cleanup: false,
            }],
            release_status: ReleaseStatus::NotNeeded,
            command_end_ms: Some(20),
            input_end_ms: None,
            at_ms: 10,
        };
        assert!(receipt.validate().is_ok());
        receipt.input_end_ms = Some(10);
        assert!(receipt.validate().is_ok());
        receipt.input_end_ms = Some(9);
        assert!(receipt.validate().is_err());
        receipt.input_end_ms = Some(11);
        assert!(receipt.validate().is_err());
        receipt.input_end_ms = None;
        if let SubmittedAction::Native { at_ms, .. } = &mut receipt.submitted[0] {
            *at_ms = 11;
        }
        assert!(receipt.validate().is_err());
    }

    #[test]
    fn i0_intents_convert_with_root_epoch_one_and_no_stop_seen() {
        let original = i0::ActionIntent {
            intent_id: "intent-1".into(),
            task_id: "task-1".into(),
            task_revision: 3,
            session_epoch: 2,
            permit: i0::PermitRef {
                id: "permit-1".into(),
                generation: 1,
            },
            surface: i0::SurfaceRef {
                id: "main".into(),
                epoch: 2,
            },
            target_ref: "target-1".into(),
            coordinate_space: i0::CoordinateSpace::SurfacePx,
            dependencies: BTreeMap::from([("focus".into(), 1)]),
            evidence_id: "obs".into(),
            latest_start_ms: 5,
            duration_ms: 10,
            steps: vec![i0::ScheduledInput {
                offset_ms: 0,
                input: i0::Input::KeyDown { key: "W".into() },
            }],
        };
        let converted = SubmitIntent::from(original);
        assert_eq!(converted.acked_stop_seq, 0);
        let native = converted.intent.native().unwrap();
        assert_eq!(native.root_epoch, 1);
        assert_eq!(native.task_revision, 3);
        assert!(converted.intent.validate().is_ok());
    }

    #[test]
    fn stop_and_grant_messages_reject_blank_fields() {
        let stop = StopCommand {
            scope: StopScope::Root {
                root_id: " ".into(),
            },
            disposition: StopDisposition::Pause,
            source: StopSource::OperatorButton,
            reason: "user".into(),
        };
        assert!(stop.validate().is_err());
        let all = StopCommand {
            scope: StopScope::All,
            ..stop
        };
        assert!(all.validate().is_ok());
    }
}
