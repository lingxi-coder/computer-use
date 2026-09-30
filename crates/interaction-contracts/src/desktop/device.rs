//! Execution-facing messages between cu-host and cu-device.
//!
//! These mirror the I0 shapes, with plain `String` identifiers (see the
//! module docs of [`super`]), and add the desktop concerns: root epochs, stop
//! sequences and the binding proof used for the point-of-hit check.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::FailureCode;
use super::primitive::{Primitive, PrimitivePolicy};
use super::require_nonempty;
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

/// Evidence as the device sees it: a scoped, time-limited claim about one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    pub dependencies: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    SurfacePx,
    /// Global display points, origin at the top-left of the main display.
    DisplayPoints,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduledPrimitive {
    pub offset_ms: u64,
    pub primitive: Primitive,
}

/// A bounded sequence of primitives with a fixed end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    pub permit: PermitRef,
    pub surface: SurfaceRef,
    #[schemars(length(min = 1))]
    pub target_ref: String,
    /// Digest of the bound instance's fingerprint, checked against what is
    /// under the pointer immediately before a pointer or AX primitive.
    #[serde(default)]
    pub binding_fingerprint: Option<String>,
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

/// An intent plus the highest stop the sending host has processed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitIntent {
    pub acked_stop_seq: u64,
    pub intent: ActionIntent,
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
#[serde(deny_unknown_fields)]
pub struct SubmittedPrimitive {
    pub at_ms: u64,
    pub primitive: Primitive,
    pub cleanup: bool,
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
    pub submitted: Vec<SubmittedPrimitive>,
    pub release_status: ReleaseStatus,
    pub command_end_ms: Option<u64>,
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
    pub policy: PrimitivePolicy,
    pub human_input: CapabilityState,
    pub hit_test: CapabilityState,
    pub input: CapabilityState,
    /// Longest stretch that cannot be interrupted once submitted.
    pub max_uninterruptible_ms: u64,
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
        if self.observed_ms >= self.valid_until_ms {
            return Err("observed_ms must be less than valid_until_ms".into());
        }
        Ok(())
    }
}

impl ActionIntent {
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
            submitted.primitive.validate()?;
            if submitted.at_ms > self.at_ms {
                return Err("submitted input must not be later than receipt.at_ms".into());
            }
            if previous_time.is_some_and(|previous| submitted.at_ms < previous) {
                return Err("submitted input times must not decrease".into());
            }
            previous_time = Some(submitted.at_ms);
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
                permit: value.permit.into(),
                surface: value.surface.into(),
                target_ref: value.target_ref,
                binding_fingerprint: None,
                coordinate_space: CoordinateSpace::SurfacePx,
                dependencies: value.dependencies,
                evidence_id: value.evidence_id,
                latest_start_ms: value.latest_start_ms,
                duration_ms: value.duration_ms,
                steps,
            },
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
                Ok(i0::SubmittedInput {
                    at_ms: entry.at_ms,
                    input: entry.primitive.try_into().map_err(|_| NotAnI0Receipt)?,
                    cleanup: entry.cleanup,
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
    use crate::desktop::primitive::{AxActionKind, Modifiers, MouseButton};

    pub(crate) fn intent() -> ActionIntent {
        ActionIntent {
            intent_id: "intent-1".into(),
            attempt_id: Some("attempt-1".into()),
            task_id: "task-1".into(),
            task_revision: 1,
            root_epoch: 1,
            session_epoch: 1,
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
            submitted: vec![SubmittedPrimitive {
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
            at_ms: 10,
        };
        assert!(receipt.validate().is_ok());
        receipt.submitted[0].at_ms = 11;
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
        assert_eq!(converted.intent.root_epoch, 1);
        assert_eq!(converted.intent.task_revision, 3);
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
