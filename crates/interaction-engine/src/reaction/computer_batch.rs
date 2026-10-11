//! In-memory lifecycle for an ordered computer-action proposal.
//!
//! This module only emits one typed step proposal at a time. It does not
//! authorize a target, map model coordinates to accessibility objects, create
//! bindings or permits, submit input, or persist progress across process
//! restarts. The host must keep its batch-ID high-water mark durable and route
//! every emitted step through the normal fresh-observation and authorization
//! path. RootGoal ownership and permissions remain outside this state machine.

use std::collections::BTreeSet;
use std::fmt;

use interaction_contracts::desktop::computer::{
    ComputerActionProposal, ComputerBatchProposal, ComputerFence,
};
use sha2::{Digest, Sha256};

/// A host-assigned, non-model-controlled identifier for one installed batch.
///
/// The host supplies monotonically increasing IDs and persists the high-water
/// mark across restarts. The runner rejects zero and any ID it has already
/// accepted. It deliberately does not mint IDs from clocks or randomness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComputerBatchId(u64);

impl ComputerBatchId {
    pub fn new(value: u64) -> Result<Self, ComputerBatchError> {
        if value == 0 {
            return Err(ComputerBatchError::InvalidBatchId);
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A proposal action's kind, used for an all-or-nothing capability check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComputerActionKind {
    Click,
    DoubleClick,
    Drag,
    Move,
    Scroll,
    Keypress,
    Type,
    Wait,
    Screenshot,
}

impl ComputerActionKind {
    pub fn for_action(action: &ComputerActionProposal) -> Self {
        match action {
            ComputerActionProposal::Click { .. } => Self::Click,
            ComputerActionProposal::DoubleClick { .. } => Self::DoubleClick,
            ComputerActionProposal::Drag { .. } => Self::Drag,
            ComputerActionProposal::Move { .. } => Self::Move,
            ComputerActionProposal::Scroll { .. } => Self::Scroll,
            ComputerActionProposal::Keypress { .. } => Self::Keypress,
            ComputerActionProposal::Type { .. } => Self::Type,
            ComputerActionProposal::Wait => Self::Wait,
            ComputerActionProposal::Screenshot => Self::Screenshot,
        }
    }

    pub fn step_kind(self) -> ComputerStepKind {
        match self {
            Self::Wait | Self::Screenshot => ComputerStepKind::Observation,
            Self::Click
            | Self::DoubleClick
            | Self::Drag
            | Self::Move
            | Self::Scroll
            | Self::Keypress
            | Self::Type => ComputerStepKind::Input,
        }
    }
}

/// Explicitly declared adapter/compiler support for proposal representations.
///
/// This is not user authorization or a RootGoal permission. The default is
/// empty so a caller must name every action kind its complete batch can carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComputerBatchCapabilities {
    supported: BTreeSet<ComputerActionKind>,
}

impl ComputerBatchCapabilities {
    pub fn new(supported: impl IntoIterator<Item = ComputerActionKind>) -> Self {
        Self {
            supported: supported.into_iter().collect(),
        }
    }

    pub fn supports(&self, kind: ComputerActionKind) -> bool {
        self.supported.contains(&kind)
    }
}

/// Whether a step asks the host to perform input or obtain an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerStepKind {
    Input,
    Observation,
}

/// An opaque receipt token. Its fields are private so callers can only return
/// a token issued by this runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComputerBatchStepToken {
    batch_id: ComputerBatchId,
    action_index: usize,
    nonce: u64,
    fence_fingerprint: [u8; 32],
}

/// One action for the caller to compile and route through ordinary admission.
///
/// A proposal is not a device command. In particular, `action` retains the
/// model's coordinate-space data and does not infer or choose an AX object.
#[derive(Debug, Clone, PartialEq)]
pub struct ComputerBatchStepProposal {
    batch_id: ComputerBatchId,
    action_index: usize,
    action: ComputerActionProposal,
    fence: ComputerFence,
    kind: ComputerStepKind,
    token: ComputerBatchStepToken,
}

impl ComputerBatchStepProposal {
    pub const fn batch_id(&self) -> ComputerBatchId {
        self.batch_id
    }

    pub const fn action_index(&self) -> usize {
        self.action_index
    }

    pub fn action(&self) -> &ComputerActionProposal {
        &self.action
    }

    pub const fn fence(&self) -> &ComputerFence {
        &self.fence
    }

    pub const fn kind(&self) -> ComputerStepKind {
        self.kind
    }

    pub const fn token(&self) -> ComputerBatchStepToken {
        self.token
    }
}

/// The only receipts that can advance or stop a batch.
#[derive(Debug, Clone, PartialEq)]
pub enum ComputerBatchStepReceipt {
    /// A known input completion. A later step still needs a strictly newer
    /// observation and a different snapshot.
    InputCompleted,
    /// A completed Wait/Screenshot observation, bound to its new fence.
    ObservationCompleted {
        fresh_fence: ComputerFence,
    },
    Unknown,
    Denied,
    Cancelled,
    Stale,
    TimedOut,
}

/// Why a terminal batch stopped. Completed actions always remain a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerBatchStopReason {
    Unknown { action_index: usize },
    Denied { action_index: usize },
    Cancelled { action_index: usize },
    Stale { action_index: usize },
    TimedOut { action_index: usize },
    InvalidReceipt { action_index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerBatchTerminal {
    Completed,
    Stopped(ComputerBatchStopReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerBatchStatus {
    Ready,
    AwaitingFreshObservation,
    StepOutstanding,
    Terminal,
}

/// A stable view of the completed prefix and any terminal reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerBatchProgress {
    pub batch_id: ComputerBatchId,
    pub completed_prefix: usize,
    pub total_actions: usize,
    pub next_action_index: Option<usize>,
    pub status: ComputerBatchStatus,
    pub current_fence: ComputerFence,
    pub terminal: Option<ComputerBatchTerminal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerBatchError {
    InvalidProposal(String),
    InvalidCurrentFence(String),
    UnsupportedAction(ComputerActionKind),
    InvalidBatchId,
    BatchIdNotMonotonic,
    BatchAlreadyActive,
    TerminalMustBeDrained,
    NoActiveBatch,
    WrongBatchId,
    StepOutstanding,
    StaleFence,
    TimedOut,
    InvalidReceipt,
}

impl fmt::Display for ComputerBatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProposal(reason) => write!(formatter, "invalid computer batch: {reason}"),
            Self::InvalidCurrentFence(reason) => {
                write!(formatter, "invalid current computer fence: {reason}")
            }
            Self::UnsupportedAction(kind) => {
                write!(formatter, "computer action kind {kind:?} is unsupported")
            }
            Self::InvalidBatchId => formatter.write_str("computer batch ID must be nonzero"),
            Self::BatchIdNotMonotonic => {
                formatter.write_str("computer batch ID did not exceed the high-water mark")
            }
            Self::BatchAlreadyActive => formatter.write_str("a computer batch is already active"),
            Self::TerminalMustBeDrained => {
                formatter.write_str("terminal computer batch progress must be drained first")
            }
            Self::NoActiveBatch => formatter.write_str("there is no active computer batch"),
            Self::WrongBatchId => formatter.write_str("computer batch ID does not match"),
            Self::StepOutstanding => {
                formatter.write_str("the current computer step has no consumed receipt")
            }
            Self::StaleFence => formatter.write_str("computer batch fence is stale or changed"),
            Self::TimedOut => formatter.write_str("computer batch deadline has elapsed"),
            Self::InvalidReceipt => {
                formatter.write_str("computer step receipt does not match its step kind")
            }
        }
    }
}

impl std::error::Error for ComputerBatchError {}

#[derive(Debug, Default)]
pub struct ComputerBatchRunner {
    highest_batch_id: u64,
    next_token_nonce: u64,
    state: RunnerState,
}

#[derive(Debug, Default)]
enum RunnerState {
    #[default]
    Empty,
    Active(ActiveBatch),
    Terminal(ComputerBatchProgress),
}

#[derive(Debug)]
struct ActiveBatch {
    id: ComputerBatchId,
    actions: Vec<ComputerActionProposal>,
    completed_prefix: usize,
    current_fence: ComputerFence,
    deadline_ms: u64,
    outstanding: Option<ComputerBatchStepToken>,
    awaiting_fresh_observation: bool,
}

impl ComputerBatchRunner {
    /// Restore a host-persisted batch-ID high-water mark after process restart.
    /// Active batches are intentionally not restored by this in-memory runner.
    pub fn with_highest_batch_id(highest_batch_id: u64) -> Self {
        Self {
            highest_batch_id,
            ..Self::default()
        }
    }

    pub const fn highest_batch_id(&self) -> u64 {
        self.highest_batch_id
    }

    /// Validate and install the complete proposal before any prefix can run.
    ///
    /// `current_fence` is supplied by the host. It must belong to the same
    /// request/generation/root-task/task-revision/root-epoch and may only keep
    /// the proposal's observation or advance it. The effective deadline can
    /// only shrink.
    pub fn install(
        &mut self,
        batch_id: ComputerBatchId,
        proposal: ComputerBatchProposal,
        capabilities: &ComputerBatchCapabilities,
        current_fence: ComputerFence,
        now_ms: u64,
    ) -> Result<ComputerBatchProgress, ComputerBatchError> {
        match &self.state {
            RunnerState::Active(_) => return Err(ComputerBatchError::BatchAlreadyActive),
            RunnerState::Terminal(_) => {
                return Err(ComputerBatchError::TerminalMustBeDrained);
            }
            RunnerState::Empty => {}
        }
        if batch_id.get() <= self.highest_batch_id {
            return Err(ComputerBatchError::BatchIdNotMonotonic);
        }
        proposal
            .validate()
            .map_err(ComputerBatchError::InvalidProposal)?;
        current_fence
            .validate()
            .map_err(ComputerBatchError::InvalidCurrentFence)?;
        if !same_logical_fence(&proposal.fence, &current_fence)
            || !valid_observation_successor(&proposal.fence, &current_fence, true)
        {
            return Err(ComputerBatchError::StaleFence);
        }
        let effective_deadline = current_fence.deadline_ms.min(proposal.fence.deadline_ms);
        if now_ms >= effective_deadline {
            return Err(ComputerBatchError::TimedOut);
        }
        for action in &proposal.actions {
            let kind = ComputerActionKind::for_action(action);
            if !capabilities.supports(kind) {
                return Err(ComputerBatchError::UnsupportedAction(kind));
            }
        }

        self.highest_batch_id = batch_id.get();
        let progress = ComputerBatchProgress {
            batch_id,
            completed_prefix: 0,
            total_actions: proposal.actions.len(),
            next_action_index: Some(0),
            status: ComputerBatchStatus::Ready,
            current_fence: current_fence.clone(),
            terminal: None,
        };
        self.state = RunnerState::Active(ActiveBatch {
            id: batch_id,
            actions: proposal.actions,
            completed_prefix: 0,
            current_fence,
            deadline_ms: effective_deadline,
            outstanding: None,
            awaiting_fresh_observation: false,
        });
        Ok(progress)
    }

    /// Return the latest accepted host fence without changing runner state.
    pub fn current_fence(&self) -> Option<&ComputerFence> {
        match &self.state {
            RunnerState::Empty => None,
            RunnerState::Active(active) => Some(&active.current_fence),
            RunnerState::Terminal(progress) => Some(&progress.current_fence),
        }
    }

    pub fn progress(&self) -> Option<ComputerBatchProgress> {
        match &self.state {
            RunnerState::Empty => None,
            RunnerState::Active(active) => Some(active.progress()),
            RunnerState::Terminal(progress) => Some(progress.clone()),
        }
    }

    /// Issue exactly one step proposal. The caller still performs fresh
    /// binding, Observe → Grant → Submit admission outside this component.
    pub fn take_step(
        &mut self,
        fresh_fence: ComputerFence,
        now_ms: u64,
    ) -> Result<ComputerBatchStepProposal, ComputerBatchError> {
        if self.expire(now_ms) {
            return Err(ComputerBatchError::TimedOut);
        }
        let action_index = match &self.state {
            RunnerState::Empty => return Err(ComputerBatchError::NoActiveBatch),
            RunnerState::Terminal(_) => return Err(ComputerBatchError::NoActiveBatch),
            RunnerState::Active(active) => {
                if active.outstanding.is_some() {
                    return Err(ComputerBatchError::StepOutstanding);
                }
                active.completed_prefix
            }
        };
        let (invalid_fence, candidate_timed_out) = match &self.state {
            RunnerState::Active(active) => {
                let fence_is_current = fresh_fence.validate().is_ok()
                    && same_logical_fence(&active.current_fence, &fresh_fence)
                    && fresh_fence.deadline_ms <= active.deadline_ms
                    && valid_observation_successor(
                        &active.current_fence,
                        &fresh_fence,
                        !active.awaiting_fresh_observation,
                    );
                (
                    !fence_is_current,
                    fence_is_current && now_ms >= fresh_fence.deadline_ms,
                )
            }
            _ => unreachable!("active state was checked above"),
        };
        if invalid_fence {
            self.stop_active(ComputerBatchStopReason::Stale { action_index });
            return Err(ComputerBatchError::StaleFence);
        }
        if candidate_timed_out {
            self.stop_active(ComputerBatchStopReason::TimedOut { action_index });
            return Err(ComputerBatchError::TimedOut);
        }

        let Some(nonce) = self.next_token_nonce.checked_add(1) else {
            self.stop_active(ComputerBatchStopReason::Stale { action_index });
            return Err(ComputerBatchError::StaleFence);
        };
        self.next_token_nonce = nonce;

        let action = match &self.state {
            RunnerState::Active(active) => active.actions.get(action_index).cloned(),
            _ => unreachable!("active state was checked above"),
        };
        let Some(action) = action else {
            self.stop_active(ComputerBatchStopReason::Stale { action_index });
            return Err(ComputerBatchError::StaleFence);
        };

        let (batch_id, step_fence) = {
            let RunnerState::Active(active) = &mut self.state else {
                unreachable!("active state was checked above")
            };
            active.current_fence = fresh_fence;
            active.deadline_ms = active.deadline_ms.min(active.current_fence.deadline_ms);
            active.awaiting_fresh_observation = false;
            (active.id, active.current_fence.clone())
        };

        let token = ComputerBatchStepToken {
            batch_id,
            action_index,
            nonce,
            fence_fingerprint: fence_fingerprint(&step_fence),
        };
        let RunnerState::Active(active) = &mut self.state else {
            unreachable!("active state was checked above")
        };
        active.outstanding = Some(token);
        Ok(ComputerBatchStepProposal {
            batch_id,
            action_index,
            kind: ComputerActionKind::for_action(&action).step_kind(),
            action,
            fence: step_fence,
            token,
        })
    }

    /// Consume a receipt for the one outstanding step.
    ///
    /// Unknown, denied, cancelled, stale and timed-out receipts are terminal.
    /// A known input completion advances the prefix but requires a strictly
    /// newer fence before the next step may be proposed. Wait and Screenshot
    /// require a successful observation receipt carrying that new fence.
    pub fn complete_step(
        &mut self,
        token: ComputerBatchStepToken,
        receipt: ComputerBatchStepReceipt,
        now_ms: u64,
    ) -> Result<ComputerBatchProgress, ComputerBatchError> {
        if self.expire(now_ms) {
            return Err(ComputerBatchError::TimedOut);
        }
        let step_details = match &self.state {
            RunnerState::Empty => return Err(ComputerBatchError::NoActiveBatch),
            RunnerState::Terminal(_) => return Err(ComputerBatchError::NoActiveBatch),
            RunnerState::Active(active) => active.outstanding.map(|outstanding| {
                let action = &active.actions[outstanding.action_index];
                (
                    outstanding.action_index,
                    ComputerActionKind::for_action(action).step_kind(),
                    outstanding,
                )
            }),
        };
        let Some((action_index, expected_kind, expected_token)) = step_details else {
            let action_index = match &self.state {
                RunnerState::Active(active) => active.completed_prefix,
                _ => unreachable!("active state was checked above"),
            };
            self.stop_active(ComputerBatchStopReason::Stale { action_index });
            return Err(ComputerBatchError::StaleFence);
        };
        if token != expected_token {
            self.stop_active(ComputerBatchStopReason::Stale { action_index });
            return Err(ComputerBatchError::StaleFence);
        }

        match receipt {
            ComputerBatchStepReceipt::InputCompleted
                if expected_kind == ComputerStepKind::Input =>
            {
                self.complete_known_step(action_index, None);
                Ok(self
                    .progress()
                    .expect("completed step retains runner state"))
            }
            ComputerBatchStepReceipt::ObservationCompleted { fresh_fence }
                if expected_kind == ComputerStepKind::Observation =>
            {
                let (invalid_fence, candidate_timed_out) = match &self.state {
                    RunnerState::Active(active) => {
                        let fence_is_current = fresh_fence.validate().is_ok()
                            && same_logical_fence(&active.current_fence, &fresh_fence)
                            && fresh_fence.deadline_ms <= active.deadline_ms
                            && valid_observation_successor(
                                &active.current_fence,
                                &fresh_fence,
                                false,
                            );
                        (
                            !fence_is_current,
                            fence_is_current && now_ms >= fresh_fence.deadline_ms,
                        )
                    }
                    _ => unreachable!("outstanding step was checked above"),
                };
                if invalid_fence {
                    self.stop_active(ComputerBatchStopReason::Stale { action_index });
                    return Err(ComputerBatchError::StaleFence);
                }
                if candidate_timed_out {
                    self.stop_active(ComputerBatchStopReason::TimedOut { action_index });
                    return Err(ComputerBatchError::TimedOut);
                }
                self.complete_known_step(action_index, Some(fresh_fence));
                Ok(self
                    .progress()
                    .expect("completed step retains runner state"))
            }
            ComputerBatchStepReceipt::Unknown => {
                self.stop_active(ComputerBatchStopReason::Unknown { action_index });
                Ok(self.progress().expect("stopped batch retains progress"))
            }
            ComputerBatchStepReceipt::Denied => {
                self.stop_active(ComputerBatchStopReason::Denied { action_index });
                Ok(self.progress().expect("stopped batch retains progress"))
            }
            ComputerBatchStepReceipt::Cancelled => {
                self.stop_active(ComputerBatchStopReason::Cancelled { action_index });
                Ok(self.progress().expect("stopped batch retains progress"))
            }
            ComputerBatchStepReceipt::Stale => {
                self.stop_active(ComputerBatchStopReason::Stale { action_index });
                Ok(self.progress().expect("stopped batch retains progress"))
            }
            ComputerBatchStepReceipt::TimedOut => {
                self.stop_active(ComputerBatchStopReason::TimedOut { action_index });
                Ok(self.progress().expect("stopped batch retains progress"))
            }
            ComputerBatchStepReceipt::InputCompleted
            | ComputerBatchStepReceipt::ObservationCompleted { .. } => {
                self.stop_active(ComputerBatchStopReason::InvalidReceipt { action_index });
                Err(ComputerBatchError::InvalidReceipt)
            }
        }
    }

    /// Permanently stop the active batch, preserving its completed prefix.
    pub fn cancel(
        &mut self,
        batch_id: ComputerBatchId,
    ) -> Result<ComputerBatchProgress, ComputerBatchError> {
        let action_index = match &self.state {
            RunnerState::Empty => return Err(ComputerBatchError::NoActiveBatch),
            RunnerState::Terminal(_) => return Err(ComputerBatchError::NoActiveBatch),
            RunnerState::Active(active) => {
                if active.id != batch_id {
                    return Err(ComputerBatchError::WrongBatchId);
                }
                active.completed_prefix
            }
        };
        self.stop_active(ComputerBatchStopReason::Cancelled { action_index });
        Ok(self.progress().expect("stopped batch retains progress"))
    }

    /// Stop an active batch once its fixed, monotonically shrinking deadline
    /// has elapsed. Returns whether this call performed the transition.
    pub fn expire(&mut self, now_ms: u64) -> bool {
        let timed_out = matches!(
            &self.state,
            RunnerState::Active(active) if now_ms >= active.deadline_ms
        );
        if timed_out {
            let action_index = match &self.state {
                RunnerState::Active(active) => active.completed_prefix,
                _ => unreachable!("timed-out state checked above"),
            };
            self.stop_active(ComputerBatchStopReason::TimedOut { action_index });
        }
        timed_out
    }

    /// Drain terminal progress before another batch can be installed.
    pub fn take_terminal_progress(
        &mut self,
        batch_id: ComputerBatchId,
    ) -> Result<ComputerBatchProgress, ComputerBatchError> {
        match &self.state {
            RunnerState::Terminal(progress) if progress.batch_id == batch_id => {
                let RunnerState::Terminal(progress) = std::mem::take(&mut self.state) else {
                    unreachable!("terminal state matched above")
                };
                Ok(progress)
            }
            RunnerState::Terminal(_) => Err(ComputerBatchError::WrongBatchId),
            RunnerState::Active(_) => Err(ComputerBatchError::BatchAlreadyActive),
            RunnerState::Empty => Err(ComputerBatchError::NoActiveBatch),
        }
    }

    fn complete_known_step(
        &mut self,
        action_index: usize,
        observation_fence: Option<ComputerFence>,
    ) {
        let is_terminal = match &mut self.state {
            RunnerState::Active(active) => {
                debug_assert_eq!(active.completed_prefix, action_index);
                active.completed_prefix += 1;
                active.outstanding = None;
                if let Some(fence) = observation_fence {
                    active.current_fence = fence;
                    active.deadline_ms = active.deadline_ms.min(active.current_fence.deadline_ms);
                    active.awaiting_fresh_observation = false;
                } else {
                    active.awaiting_fresh_observation = true;
                }
                active.completed_prefix == active.actions.len()
            }
            _ => unreachable!("known completion requires an active batch"),
        };
        if is_terminal {
            self.finish(ComputerBatchTerminal::Completed);
        }
    }

    fn stop_active(&mut self, reason: ComputerBatchStopReason) {
        self.finish(ComputerBatchTerminal::Stopped(reason));
    }

    fn finish(&mut self, terminal: ComputerBatchTerminal) {
        let RunnerState::Active(active) = std::mem::take(&mut self.state) else {
            return;
        };
        self.state = RunnerState::Terminal(ComputerBatchProgress {
            batch_id: active.id,
            completed_prefix: active.completed_prefix,
            total_actions: active.actions.len(),
            next_action_index: None,
            status: ComputerBatchStatus::Terminal,
            current_fence: active.current_fence,
            terminal: Some(terminal),
        });
    }
}

impl ActiveBatch {
    fn progress(&self) -> ComputerBatchProgress {
        let status = if self.outstanding.is_some() {
            ComputerBatchStatus::StepOutstanding
        } else if self.awaiting_fresh_observation {
            ComputerBatchStatus::AwaitingFreshObservation
        } else {
            ComputerBatchStatus::Ready
        };
        ComputerBatchProgress {
            batch_id: self.id,
            completed_prefix: self.completed_prefix,
            total_actions: self.actions.len(),
            next_action_index: Some(self.completed_prefix),
            status,
            current_fence: self.current_fence.clone(),
            terminal: None,
        }
    }
}

fn same_logical_fence(left: &ComputerFence, right: &ComputerFence) -> bool {
    left.request_id == right.request_id
        && left.generation == right.generation
        && left.root_task_id == right.root_task_id
        && left.task_revision == right.task_revision
        && left.root_epoch == right.root_epoch
}

fn fence_fingerprint(fence: &ComputerFence) -> [u8; 32] {
    let canonical = serde_json::to_vec(fence).expect("ComputerFence always serializes");
    Sha256::digest(canonical).into()
}

fn valid_observation_successor(
    previous: &ComputerFence,
    candidate: &ComputerFence,
    allow_same: bool,
) -> bool {
    if candidate.deadline_ms > previous.deadline_ms {
        return false;
    }
    let same_observation = candidate.structure_sequence == previous.structure_sequence
        && candidate.snapshot_id == previous.snapshot_id;
    if same_observation {
        return allow_same;
    }
    candidate.structure_sequence > previous.structure_sequence
        && candidate.snapshot_id != previous.snapshot_id
}

#[cfg(test)]
mod tests {
    use interaction_contracts::desktop::computer::{
        ComputerActionProposal, ComputerBatchProposal, ComputerFence, ComputerMouseButton,
    };
    use interaction_contracts::desktop::ids::{RequestId, RootTaskId};

    use super::*;

    fn fence(sequence: u64) -> ComputerFence {
        ComputerFence {
            request_id: RequestId::new("computer-request-1").unwrap(),
            generation: 7,
            root_task_id: RootTaskId::new("root-task-2").unwrap(),
            task_revision: 3,
            root_epoch: 11,
            snapshot_id: format!("snapshot-{sequence}"),
            structure_sequence: sequence,
            deadline_ms: 10_000,
        }
    }

    fn click() -> ComputerActionProposal {
        ComputerActionProposal::Click {
            button: ComputerMouseButton::Left,
            x: 10.0,
            y: 20.0,
            keys: None,
        }
    }

    fn proposal(actions: Vec<ComputerActionProposal>) -> ComputerBatchProposal {
        ComputerBatchProposal {
            fence: fence(1),
            actions,
        }
    }

    fn capabilities(kinds: &[ComputerActionKind]) -> ComputerBatchCapabilities {
        ComputerBatchCapabilities::new(kinds.iter().copied())
    }

    fn install(
        runner: &mut ComputerBatchRunner,
        batch_id: u64,
        actions: Vec<ComputerActionProposal>,
        supported: &[ComputerActionKind],
    ) -> ComputerBatchProgress {
        runner
            .install(
                ComputerBatchId::new(batch_id).unwrap(),
                proposal(actions),
                &capabilities(supported),
                fence(1),
                1,
            )
            .unwrap()
    }

    #[test]
    fn rejects_entire_batch_before_any_prefix_if_one_action_is_unsupported() {
        let mut runner = ComputerBatchRunner::default();
        let error = runner
            .install(
                ComputerBatchId::new(1).unwrap(),
                proposal(vec![
                    click(),
                    ComputerActionProposal::Drag {
                        path: vec![
                            interaction_contracts::desktop::computer::ComputerPoint {
                                x: 1.0,
                                y: 2.0,
                            },
                            interaction_contracts::desktop::computer::ComputerPoint {
                                x: 3.0,
                                y: 4.0,
                            },
                        ],
                        keys: None,
                    },
                ]),
                &capabilities(&[ComputerActionKind::Click]),
                fence(1),
                1,
            )
            .unwrap_err();
        assert_eq!(
            error,
            ComputerBatchError::UnsupportedAction(ComputerActionKind::Drag)
        );
        assert!(runner.progress().is_none());
        assert_eq!(runner.highest_batch_id, 0);
    }

    #[test]
    fn validates_the_whole_shape_before_checking_capabilities() {
        let mut runner = ComputerBatchRunner::default();
        let error = runner
            .install(
                ComputerBatchId::new(1).unwrap(),
                proposal(vec![ComputerActionProposal::Move {
                    x: f64::NAN,
                    y: 0.0,
                    keys: None,
                }]),
                &ComputerBatchCapabilities::default(),
                fence(1),
                1,
            )
            .unwrap_err();
        assert!(matches!(error, ComputerBatchError::InvalidProposal(_)));
        assert!(runner.progress().is_none());
        assert_eq!(runner.highest_batch_id(), 0);
    }

    #[test]
    fn batch_ids_are_host_assigned_monotonic_and_terminal_must_be_drained() {
        let mut runner = ComputerBatchRunner::default();
        let initial = install(&mut runner, 10, vec![click()], &[ComputerActionKind::Click]);
        assert_eq!(initial.batch_id.get(), 10);
        let first = runner.take_step(fence(1), 2).unwrap();
        let terminal = runner
            .complete_step(first.token(), ComputerBatchStepReceipt::Unknown, 3)
            .unwrap();
        assert!(matches!(
            terminal.terminal,
            Some(ComputerBatchTerminal::Stopped(
                ComputerBatchStopReason::Unknown { action_index: 0 }
            ))
        ));
        assert_eq!(
            runner.install(
                ComputerBatchId::new(11).unwrap(),
                proposal(vec![click()]),
                &capabilities(&[ComputerActionKind::Click]),
                fence(1),
                4,
            ),
            Err(ComputerBatchError::TerminalMustBeDrained)
        );
        runner.take_terminal_progress(initial.batch_id).unwrap();
        assert_eq!(
            runner.install(
                ComputerBatchId::new(10).unwrap(),
                proposal(vec![click()]),
                &capabilities(&[ComputerActionKind::Click]),
                fence(1),
                4,
            ),
            Err(ComputerBatchError::BatchIdNotMonotonic)
        );
        let mut resumed = ComputerBatchRunner::with_highest_batch_id(10);
        assert_eq!(resumed.highest_batch_id(), 10);
        assert_eq!(
            resumed.install(
                ComputerBatchId::new(10).unwrap(),
                proposal(vec![click()]),
                &capabilities(&[ComputerActionKind::Click]),
                fence(1),
                4,
            ),
            Err(ComputerBatchError::BatchIdNotMonotonic)
        );
    }

    #[test]
    fn first_step_accepts_original_or_newer_snapshot_but_rejects_fence_lineage_change() {
        let mut runner = ComputerBatchRunner::default();
        let newer = ComputerFence {
            deadline_ms: 9_000,
            ..fence(2)
        };
        runner
            .install(
                ComputerBatchId::new(1).unwrap(),
                proposal(vec![click()]),
                &capabilities(&[ComputerActionKind::Click]),
                newer.clone(),
                1,
            )
            .unwrap();
        assert_eq!(runner.take_step(newer.clone(), 2).unwrap().fence(), &newer);

        let mut other = ComputerBatchRunner::default();
        let mut changed_request = fence(1);
        changed_request.request_id = RequestId::new("other-request").unwrap();
        assert_eq!(
            other.install(
                ComputerBatchId::new(1).unwrap(),
                proposal(vec![click()]),
                &capabilities(&[ComputerActionKind::Click]),
                changed_request,
                1,
            ),
            Err(ComputerBatchError::StaleFence)
        );
        assert!(other.progress().is_none());
    }

    #[test]
    fn one_outstanding_step_and_duplicate_receipt_cannot_advance_twice() {
        let mut runner = ComputerBatchRunner::default();
        install(
            &mut runner,
            1,
            vec![click(), click()],
            &[ComputerActionKind::Click],
        );
        let step = runner.take_step(fence(1), 2).unwrap();
        assert_eq!(
            runner.take_step(fence(1), 2),
            Err(ComputerBatchError::StepOutstanding)
        );
        let progress = runner
            .complete_step(step.token(), ComputerBatchStepReceipt::InputCompleted, 3)
            .unwrap();
        assert_eq!(progress.completed_prefix, 1);
        assert_eq!(
            progress.status,
            ComputerBatchStatus::AwaitingFreshObservation
        );
        assert_eq!(
            runner.complete_step(step.token(), ComputerBatchStepReceipt::InputCompleted, 4),
            Err(ComputerBatchError::StaleFence)
        );
        let progress = runner.progress().unwrap();
        assert_eq!(progress.completed_prefix, 1);
        assert!(matches!(
            progress.terminal,
            Some(ComputerBatchTerminal::Stopped(
                ComputerBatchStopReason::Stale { action_index: 1 }
            ))
        ));
    }

    #[test]
    fn receipt_from_another_batch_cannot_skip_or_complete_this_step() {
        let mut first_runner = ComputerBatchRunner::default();
        install(
            &mut first_runner,
            1,
            vec![click()],
            &[ComputerActionKind::Click],
        );
        let foreign_step = first_runner.take_step(fence(1), 2).unwrap();

        let mut second_runner = ComputerBatchRunner::default();
        install(
            &mut second_runner,
            2,
            vec![click()],
            &[ComputerActionKind::Click],
        );
        second_runner.take_step(fence(1), 2).unwrap();
        assert_eq!(
            second_runner.complete_step(
                foreign_step.token(),
                ComputerBatchStepReceipt::InputCompleted,
                3
            ),
            Err(ComputerBatchError::StaleFence)
        );
        assert!(matches!(
            second_runner.progress().unwrap().terminal,
            Some(ComputerBatchTerminal::Stopped(
                ComputerBatchStopReason::Stale { action_index: 0 }
            ))
        ));
    }

    #[test]
    fn same_batch_id_and_index_do_not_make_tokens_cross_root_fences_equal() {
        let mut first_runner = ComputerBatchRunner::default();
        install(
            &mut first_runner,
            9,
            vec![click()],
            &[ComputerActionKind::Click],
        );
        let first_step = first_runner.take_step(fence(1), 2).unwrap();

        let mut other_fence = fence(1);
        other_fence.request_id = RequestId::new("other-request").unwrap();
        other_fence.root_task_id = RootTaskId::new("other-root").unwrap();
        other_fence.root_epoch += 1;
        let mut other_proposal = proposal(vec![click()]);
        other_proposal.fence = other_fence.clone();

        // Both IDs, indexes and first local nonces matched; the token's
        // private serialized-fence digest must still distinguish the roots.
        let mut second_runner = ComputerBatchRunner::default();
        second_runner
            .install(
                ComputerBatchId::new(9).unwrap(),
                other_proposal,
                &capabilities(&[ComputerActionKind::Click]),
                other_fence.clone(),
                1,
            )
            .unwrap();
        let second_step = second_runner.take_step(other_fence, 2).unwrap();
        assert_ne!(first_step.token(), second_step.token());
        assert_eq!(
            second_runner.complete_step(
                first_step.token(),
                ComputerBatchStepReceipt::InputCompleted,
                3
            ),
            Err(ComputerBatchError::StaleFence)
        );
        assert_eq!(second_runner.progress().unwrap().completed_prefix, 0);
    }

    #[test]
    fn completed_input_cannot_continue_on_same_snapshot_but_new_read_can() {
        let mut stale = ComputerBatchRunner::default();
        install(
            &mut stale,
            1,
            vec![click(), click()],
            &[ComputerActionKind::Click],
        );
        let step = stale.take_step(fence(1), 2).unwrap();
        stale
            .complete_step(step.token(), ComputerBatchStepReceipt::InputCompleted, 3)
            .unwrap();
        assert_eq!(
            stale.take_step(fence(1), 4),
            Err(ComputerBatchError::StaleFence)
        );
        assert!(matches!(
            stale.progress().unwrap().terminal,
            Some(ComputerBatchTerminal::Stopped(
                ComputerBatchStopReason::Stale { action_index: 1 }
            ))
        ));

        let mut fresh = ComputerBatchRunner::default();
        install(
            &mut fresh,
            1,
            vec![click(), click()],
            &[ComputerActionKind::Click],
        );
        let step = fresh.take_step(fence(1), 2).unwrap();
        fresh
            .complete_step(step.token(), ComputerBatchStepReceipt::InputCompleted, 3)
            .unwrap();
        let second = fresh.take_step(fence(2), 4).unwrap();
        assert_eq!(second.action_index(), 1);
        assert_eq!(second.fence().structure_sequence, 2);
        let done = fresh
            .complete_step(second.token(), ComputerBatchStepReceipt::InputCompleted, 5)
            .unwrap();
        assert_eq!(done.completed_prefix, 2);
        assert_eq!(done.terminal, Some(ComputerBatchTerminal::Completed));
    }

    #[test]
    fn wait_and_screenshot_are_observation_steps_and_require_new_fence_receipts() {
        for action in [
            ComputerActionProposal::Wait,
            ComputerActionProposal::Screenshot,
        ] {
            let kind = ComputerActionKind::for_action(&action);
            let mut runner = ComputerBatchRunner::default();
            install(&mut runner, 1, vec![action], &[kind]);
            let step = runner.take_step(fence(1), 2).unwrap();
            assert_eq!(step.kind(), ComputerStepKind::Observation);
            assert_eq!(
                runner.complete_step(step.token(), ComputerBatchStepReceipt::InputCompleted, 3),
                Err(ComputerBatchError::InvalidReceipt)
            );
            assert!(matches!(
                runner.progress().unwrap().terminal,
                Some(ComputerBatchTerminal::Stopped(
                    ComputerBatchStopReason::InvalidReceipt { action_index: 0 }
                ))
            ));
        }

        let mut runner = ComputerBatchRunner::default();
        install(
            &mut runner,
            1,
            vec![ComputerActionProposal::Screenshot, click()],
            &[ComputerActionKind::Screenshot, ComputerActionKind::Click],
        );
        let screenshot = runner.take_step(fence(1), 2).unwrap();
        let observed = ComputerFence {
            deadline_ms: 9_000,
            ..fence(2)
        };
        let progress = runner
            .complete_step(
                screenshot.token(),
                ComputerBatchStepReceipt::ObservationCompleted {
                    fresh_fence: observed.clone(),
                },
                3,
            )
            .unwrap();
        assert_eq!(progress.completed_prefix, 1);
        assert_eq!(progress.current_fence, observed);
        let click = runner.take_step(observed.clone(), 4).unwrap();
        assert_eq!(click.kind(), ComputerStepKind::Input);
        assert_eq!(click.fence(), &observed);
    }

    #[test]
    fn changed_generation_epoch_snapshot_or_extended_deadline_stops_as_stale() {
        let mutators: [fn(&mut ComputerFence); 4] = [
            |fence: &mut ComputerFence| fence.generation += 1,
            |fence: &mut ComputerFence| fence.root_epoch += 1,
            |fence: &mut ComputerFence| fence.snapshot_id = "forked-same-sequence".into(),
            |fence: &mut ComputerFence| fence.deadline_ms += 1,
        ];
        for mutate in mutators {
            let mut runner = ComputerBatchRunner::default();
            install(&mut runner, 1, vec![click()], &[ComputerActionKind::Click]);
            let mut changed = fence(1);
            mutate(&mut changed);
            assert_eq!(
                runner.take_step(changed, 2),
                Err(ComputerBatchError::StaleFence)
            );
            assert!(matches!(
                runner.progress().unwrap().terminal,
                Some(ComputerBatchTerminal::Stopped(
                    ComputerBatchStopReason::Stale { action_index: 0 }
                ))
            ));
        }
    }

    #[test]
    fn unknown_denied_cancelled_stale_and_timeout_lock_the_batch_and_keep_prefix() {
        for receipt in [
            ComputerBatchStepReceipt::Unknown,
            ComputerBatchStepReceipt::Denied,
            ComputerBatchStepReceipt::Cancelled,
            ComputerBatchStepReceipt::Stale,
            ComputerBatchStepReceipt::TimedOut,
        ] {
            let mut runner = ComputerBatchRunner::default();
            install(
                &mut runner,
                1,
                vec![click(), click()],
                &[ComputerActionKind::Click],
            );
            let first = runner.take_step(fence(1), 2).unwrap();
            runner
                .complete_step(first.token(), ComputerBatchStepReceipt::InputCompleted, 3)
                .unwrap();
            let second = runner.take_step(fence(2), 4).unwrap();
            let progress = runner
                .complete_step(second.token(), receipt.clone(), 5)
                .unwrap();
            assert_eq!(progress.completed_prefix, 1);
            assert_eq!(progress.status, ComputerBatchStatus::Terminal);
            assert_eq!(
                runner.take_step(fence(3), 6),
                Err(ComputerBatchError::NoActiveBatch)
            );
            let drained = runner.take_terminal_progress(progress.batch_id).unwrap();
            assert_eq!(drained.completed_prefix, 1);
            assert!(drained.terminal.is_some());
        }

        let mut runner = ComputerBatchRunner::default();
        install(&mut runner, 1, vec![click()], &[ComputerActionKind::Click]);
        assert!(runner.expire(10_000));
        assert!(matches!(
            runner.progress().unwrap().terminal,
            Some(ComputerBatchTerminal::Stopped(
                ComputerBatchStopReason::TimedOut { action_index: 0 }
            ))
        ));

        let mut shortened_deadline = ComputerBatchRunner::default();
        install(
            &mut shortened_deadline,
            1,
            vec![click()],
            &[ComputerActionKind::Click],
        );
        let expired_fence = ComputerFence {
            deadline_ms: 4,
            ..fence(1)
        };
        assert_eq!(
            shortened_deadline.take_step(expired_fence, 4),
            Err(ComputerBatchError::TimedOut)
        );
        assert!(matches!(
            shortened_deadline.progress().unwrap().terminal,
            Some(ComputerBatchTerminal::Stopped(
                ComputerBatchStopReason::TimedOut { action_index: 0 }
            ))
        ));
    }

    #[test]
    fn cancellation_is_fenced_to_the_active_host_batch_id() {
        let mut runner = ComputerBatchRunner::default();
        let initial = install(&mut runner, 4, vec![click()], &[ComputerActionKind::Click]);
        assert_eq!(
            runner.cancel(ComputerBatchId::new(5).unwrap()),
            Err(ComputerBatchError::WrongBatchId)
        );
        let cancelled = runner.cancel(initial.batch_id).unwrap();
        assert_eq!(cancelled.completed_prefix, 0);
        assert!(matches!(
            cancelled.terminal,
            Some(ComputerBatchTerminal::Stopped(
                ComputerBatchStopReason::Cancelled { action_index: 0 }
            ))
        ));
    }
}
