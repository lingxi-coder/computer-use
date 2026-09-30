//! The engine: the seven modules wired into one loop (design 20.1).
//!
//! It is a state machine. The shell feeds it events with the time they
//! happened and carries its outputs to the device, the source, the decision
//! backend, the planner and the operator UI. It reads no clock, opens no
//! socket and waits for nothing: every call does a bounded amount of work and
//! returns, and anything that needs an answer is an output whose reply comes
//! back as a later input.
//!
//! Each round takes what has arrived, advances stop and cancel handling
//! first, relates receipts to new evidence, asks for the facts that are
//! missing, asks a model only when a branch has real meaning, checks every
//! proposal again at admission, and only then produces an input request.

mod attempt;
mod evidence;
mod operator;
mod planning;
mod running;
mod stops;

use std::collections::{BTreeSet, VecDeque};

use interaction_contracts::desktop::decision::{DecisionCancel, DecisionProposal, DecisionRequest};
use interaction_contracts::desktop::device::ReleaseStatus;
use interaction_contracts::desktop::ids::{AttemptId, ObjectRef, RequestId};
use interaction_contracts::desktop::operator::{
    OperatorCommand, RootView, RunMode, StopStatus, TimelineEntry, UiProjection,
};
use interaction_contracts::desktop::planning::{PlannerOutput, PlanningRequest};
use interaction_contracts::desktop::source::{SourceCommand, SourceHealth, StructureSnapshot};
use interaction_contracts::desktop::task::{GoalSpec, RootState};

use crate::deliberation::PlanningCoordinator;
use crate::environment::binding::BindingReducer;
use crate::environment::pack::AppPack;
use crate::environment::state_core::StateCore;
use crate::execution::{DeviceCommand, DeviceNotice, DeviceReply, DeviceView, Expect};
use crate::infrastructure::storage::{Storage, StoreError};
use crate::reaction::compiler::CompilerLimits;
use crate::reaction::decisions::DecisionBook;
use crate::reaction::skills::Skill;
use crate::tasks::{AdmitFault, TaskConfig, TaskError, TaskRuntime};
use crate::verification::Verification;

use attempt::AttemptRun;

/// A round does at most this much before yielding, so one event cannot hold
/// the loop.
const MAX_ROUNDS: usize = 64;
const TIMELINE_LIMIT: usize = 40;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub tasks: TaskConfig,
    /// How old a snapshot may be and still be acted on.
    pub evidence_max_age_ms: u64,
    /// How long evidence handed to the device stays valid.
    pub evidence_ttl_ms: u64,
    pub decision_timeout_ms: u64,
    pub planning_timeout_ms: u64,
    /// How long a device command may go unanswered.
    pub command_timeout_ms: u64,
    /// How long before the source is asked again for evidence.
    pub refresh_timeout_ms: u64,
    pub action_duration_ms: u64,
    pub start_slack_ms: u64,
    /// The most time one attempt may take.
    pub attempt_window_ms: u64,
    pub lease_ms: u64,
    /// Consecutive backend failures before the planner is asked instead.
    pub max_decision_failures: u32,
    pub decision_backoff_ms: u64,
    pub compiler: CompilerLimits,
    /// Observe until the operator says otherwise; no model can raise this.
    pub initial_mode: RunMode,
}

impl EngineConfig {
    pub fn new(mut tasks: TaskConfig) -> Self {
        // A plan may only name skills this build can actually run.
        tasks.registered_skills = Skill::IMPLEMENTED.map(String::from).to_vec();
        Self {
            tasks,
            evidence_max_age_ms: 2_000,
            evidence_ttl_ms: 3_000,
            decision_timeout_ms: 2_000,
            planning_timeout_ms: 30_000,
            command_timeout_ms: 2_000,
            refresh_timeout_ms: 1_000,
            action_duration_ms: 100,
            start_slack_ms: 500,
            attempt_window_ms: 15_000,
            lease_ms: 5_000,
            max_decision_failures: 3,
            decision_backoff_ms: 1_000,
            compiler: CompilerLimits::default(),
            initial_mode: RunMode::Observe,
        }
    }
}

/// Everything that can happen to the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Operator(OperatorCommand),
    Structure(Box<StructureSnapshot>),
    SourceHealth(SourceHealth),
    Device {
        corr: u64,
        reply: DeviceReply,
    },
    DeviceNotice(DeviceNotice),
    Decision(Box<DecisionProposal>),
    Planner {
        request_id: RequestId,
        generation: u64,
        output: Box<PlannerOutput>,
    },
    /// Time passed. Timeouts are the only thing that needs this.
    Tick,
}

/// What the shell must carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Device {
        corr: u64,
        command: Box<DeviceCommand>,
    },
    Source(SourceCommand),
    Decision(Box<DecisionRequest>),
    CancelDecision(DecisionCancel),
    Plan(Box<PlanningRequest>),
    Ui(Box<UiProjection>),
}

#[derive(Debug)]
pub enum EngineError {
    Task(TaskError),
    Store(StoreError),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Task(error) => write!(f, "{error}"),
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<TaskError> for EngineError {
    fn from(error: TaskError) -> Self {
        Self::Task(error)
    }
}

impl From<StoreError> for EngineError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

type Step = Result<bool, EngineError>;

/// Waiting for evidence observed at or after a time.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Wait {
    pub floor_ms: u64,
    pub requested_ms: u64,
}

/// What exists only while a frozen root is alive.
pub(crate) struct RootRun {
    pub goal: GoalSpec,
    pub pack: AppPack,
    pub verification: Verification,
    /// The registration message the device needs, until it has acknowledged it.
    pub grant: interaction_contracts::desktop::device::RootGrant,
    pub registered: bool,
    pub registering: bool,
    /// Sub-goals that finished; a skill is safe to run again because it reads
    /// before it acts, so this is progress, not a script.
    pub done: BTreeSet<String>,
    pub attempt: Option<AttemptRun>,
    pub wait: Option<Wait>,
    /// What a person or model chose among same-named controls. Object
    /// references do not survive between observations, so the group is kept.
    pub refined_group: Option<String>,
    pub chosen: Option<ObjectRef>,
    pub plan_targets: BTreeSet<String>,
    pub recent: Vec<String>,
    pub failures: u32,
    pub decision_failures: u32,
    /// No new decision is asked for before this time.
    pub decision_resume_ms: u64,
    /// When a failed request is asked again, at a higher generation.
    pub decision_retry_at: Option<u64>,
    /// Observations made without learning anything new.
    pub stalls: u32,
    pub bind_seq: u64,
    pub resume_authorized: bool,
    /// Attempts whose outcome is unknown after a restart, and the intent ids
    /// to ask the device about, one at a time.
    pub reconcile: Vec<(AttemptId, String)>,
    pub reconciling: Option<(AttemptId, String)>,
    pub revalidate_floor: Option<u64>,
    pub permits: Vec<interaction_contracts::desktop::device::PermitRef>,
    pub stop_wait_since: Option<u64>,
    /// The latest attempt and intent, for tracing an effect back to its input.
    pub last_attempt: Option<(AttemptId, Option<String>)>,
}

pub struct Engine {
    pub(crate) cfg: EngineConfig,
    pub(crate) storage: Storage,
    pub(crate) tasks: TaskRuntime,
    pub(crate) state: StateCore,
    pub(crate) bindings: BindingReducer,
    pub(crate) planning: PlanningCoordinator,
    pub(crate) decisions: DecisionBook,
    pub(crate) device: DeviceView,
    pub(crate) run: Option<RootRun>,
    pub(crate) mode: RunMode,
    pub(crate) outputs: Vec<Output>,
    pub(crate) timeline: VecDeque<TimelineEntry>,
    pub(crate) ui_revision: u64,
    pub(crate) last_ui: Option<UiProjection>,
    pub(crate) stop_status: StopStatus,
    pub(crate) cleanup: ReleaseStatus,
    pub(crate) now_ms: u64,
    pub(crate) last_lease_ms: Option<u64>,
    pub(crate) faulted: Option<String>,
}

impl Engine {
    pub fn new(cfg: EngineConfig, storage: Storage) -> Self {
        let mode = cfg.initial_mode;
        let tasks = TaskRuntime::new(cfg.tasks.clone());
        Self {
            cfg,
            storage,
            tasks,
            state: StateCore::new(),
            bindings: BindingReducer::new(),
            planning: PlanningCoordinator::new(),
            decisions: DecisionBook::new(),
            device: DeviceView::new(),
            run: None,
            mode,
            outputs: Vec::new(),
            timeline: VecDeque::new(),
            ui_revision: 0,
            last_ui: None,
            stop_status: StopStatus::NotStopped,
            cleanup: ReleaseStatus::NotNeeded,
            now_ms: 0,
            last_lease_ms: None,
            faulted: None,
        }
    }

    // ---- What tests and the shell may look at --------------------------------

    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    pub fn root_state(&self) -> Option<RootState> {
        self.tasks.active_state()
    }

    /// The current root, or the last one that finished.
    pub fn view(&self) -> Option<RootView> {
        self.tasks.view()
    }

    pub fn last_finished(&self) -> Option<RootState> {
        self.tasks.last_finished().map(|row| row.state)
    }

    pub fn mode(&self) -> RunMode {
        self.mode
    }

    pub fn stop_status(&self) -> StopStatus {
        self.stop_status
    }

    pub fn faulted(&self) -> Option<&str> {
        self.faulted.as_deref()
    }

    pub fn device(&self) -> &DeviceView {
        &self.device
    }

    pub fn bindings(&self) -> &BindingReducer {
        &self.bindings
    }

    pub fn state(&self) -> &StateCore {
        &self.state
    }

    pub fn timeline(&self) -> impl Iterator<Item = &TimelineEntry> {
        self.timeline.iter()
    }

    /// Make the next admission fail at a named point, for tests.
    pub fn inject_admit_fault(&mut self, fault: Option<AdmitFault>) {
        self.tasks.inject_admit_fault(fault);
    }

    // ---- The one entry point --------------------------------------------------------

    /// Take one event and return what the shell must do about it.
    pub fn handle(&mut self, now_ms: u64, input: Input) -> Vec<Output> {
        self.now_ms = self.now_ms.max(now_ms);
        if let Err(error) = self.dispatch(input) {
            self.fail_closed(error);
        }
        self.pump();
        self.refresh_ui();
        std::mem::take(&mut self.outputs)
    }

    fn dispatch(&mut self, input: Input) -> Result<(), EngineError> {
        match input {
            Input::Operator(command) => self.on_operator(command),
            Input::Structure(snapshot) => self.on_structure(*snapshot),
            Input::SourceHealth(health) => self.on_health(health),
            Input::Device { corr, reply } => self.on_device_reply(corr, reply),
            Input::DeviceNotice(notice) => self.on_device_notice(notice),
            Input::Decision(proposal) => self.on_decision(*proposal),
            Input::Planner {
                request_id,
                generation,
                output,
            } => self.on_planner(&request_id, generation, *output),
            Input::Tick => self.on_tick(),
        }
    }

    /// Load what survived a restart. Nothing resumes by itself: the operator
    /// has to say so, and the root is revalidated first.
    pub fn recover(&mut self, now_ms: u64) -> Vec<Output> {
        self.now_ms = self.now_ms.max(now_ms);
        if let Err(error) = self.recover_inner() {
            self.fail_closed(error);
        }
        self.pump();
        self.refresh_ui();
        std::mem::take(&mut self.outputs)
    }

    // ---- Shared helpers ----------------------------------------------------------------

    pub(crate) fn note(&mut self, text: impl Into<String>) {
        self.timeline.push_back(TimelineEntry {
            at_ms: self.now_ms,
            text: text.into(),
        });
        while self.timeline.len() > TIMELINE_LIMIT {
            self.timeline.pop_front();
        }
    }

    pub(crate) fn send(&mut self, expect: Expect, command: DeviceCommand) -> u64 {
        let corr = self.device.send(self.now_ms, expect);
        self.outputs.push(Output::Device {
            corr,
            command: Box::new(command),
        });
        corr
    }

    pub(crate) fn emit(&mut self, output: Output) {
        self.outputs.push(output);
    }

    /// The pack for whatever application the evidence shows.
    pub(crate) fn pack_for(&self, bundle_id: &str) -> AppPack {
        if let Some(run) = &self.run
            && run.pack.bundle_id == bundle_id
        {
            return run.pack.clone();
        }
        self.cfg
            .tasks
            .packs
            .iter()
            .find(|pack| pack.bundle_id == bundle_id)
            .cloned()
            .unwrap_or_else(|| AppPack::observe_only(bundle_id))
    }

    /// Something happened that the engine cannot safely work around, such as
    /// a ledger that cannot be written. It stops acting and stops the device.
    fn fail_closed(&mut self, error: EngineError) {
        // An injected admission fault is a test of rollback, not of this.
        if let EngineError::Task(TaskError::Store(StoreError::Fault(point))) = &error {
            self.note(format!("admission failed at {point}; nothing was recorded"));
            return;
        }
        let reason = error.to_string();
        self.note(format!("internal error, acting is disabled: {reason}"));
        if self.faulted.is_none() {
            self.faulted = Some(reason.clone());
            if let Some(active) = self.tasks.active() {
                let root = active.row.root_id.as_str().to_string();
                self.send_host_stop(&root, "the engine failed closed");
            }
        }
    }

    fn pump(&mut self) {
        for _ in 0..MAX_ROUNDS {
            match self.step() {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    self.fail_closed(error);
                    break;
                }
            }
        }
    }

    fn step(&mut self) -> Step {
        if self.faulted.is_some() {
            return Ok(false);
        }
        let Some(state) = self.tasks.active_state() else {
            return self.step_idle();
        };
        if self.deadline_passed(state) {
            return self.expire_root();
        }
        match state {
            RootState::CompilingGoal => self.step_compiling(),
            RootState::Ready => self.step_ready(),
            RootState::Running => self.step_running(),
            RootState::WaitingEvidence => self.step_waiting_evidence(),
            RootState::NeedsRevalidation => self.step_revalidating(),
            RootState::Draft
            | RootState::AwaitingConfirmation
            | RootState::NeedsInput
            | RootState::WaitingPlan
            | RootState::Pausing
            | RootState::Suspended
            | RootState::Cancelling
            | RootState::Completed
            | RootState::Failed
            | RootState::Unverified
            | RootState::Expired
            | RootState::Cancelled => Ok(false),
        }
    }

    /// No root is active: clean up after the one that ended, then start the
    /// next queued task.
    fn step_idle(&mut self) -> Step {
        if self.run.is_some() {
            self.finish_root();
            return Ok(true);
        }
        if let Some(text) = self.tasks.next_queued() {
            self.start_task(&text)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn deadline_passed(&self, state: RootState) -> bool {
        let Some(active) = self.tasks.active() else {
            return false;
        };
        // Only a frozen root has a deadline, and a cancellation that is already
        // in progress is not turned into an expiry.
        active
            .row
            .deadline_ms
            .is_some_and(|deadline| self.now_ms >= deadline)
            && state != RootState::Cancelling
            && !state.is_terminal()
    }

    // ---- The operator's view ----------------------------------------------------------------

    fn projection(&self) -> UiProjection {
        UiProjection {
            revision: self.ui_revision,
            mode: self.mode,
            root: self.tasks.view(),
            stop_status: self.stop_status,
            cleanup: self.cleanup,
            pending_confirmation: self
                .tasks
                .active()
                .and_then(|active| active.pending.as_ref())
                .map(|pending| pending.request().clone()),
            capabilities: self.device.manifest().cloned(),
            timeline: self.timeline.iter().cloned().collect(),
        }
    }

    fn refresh_ui(&mut self) {
        let mut next = self.projection();
        let unchanged = self.last_ui.as_ref().is_some_and(|last| {
            let mut same = last.clone();
            same.revision = next.revision;
            same == next
        });
        if unchanged {
            return;
        }
        self.ui_revision += 1;
        next.revision = self.ui_revision;
        self.last_ui = Some(next.clone());
        self.outputs.push(Output::Ui(Box::new(next)));
    }
}
