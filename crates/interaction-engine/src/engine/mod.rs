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
mod browser;
pub mod computer;
mod evidence;
mod keyboard;
mod menu;
mod operator;
#[cfg(test)]
mod owned_document_tests;
pub mod owned_revisit;
mod planning;
#[cfg(test)]
mod row_visits_tests;
mod running;
#[cfg(test)]
mod scoped_revisit_tests;
#[cfg(test)]
mod scoped_window_tests;
mod stops;
mod window;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use interaction_contracts::desktop::browser::BrowserPageProfile;
use interaction_contracts::desktop::browser::BrowserRefreshRequest;
use interaction_contracts::desktop::decision::{DecisionCancel, DecisionProposal, DecisionRequest};
use interaction_contracts::desktop::device::ReleaseStatus;
use interaction_contracts::desktop::ids::{
    AttemptId, BindingRef, ObjectRef, RequestId, SemanticTargetRef,
};
use interaction_contracts::desktop::operator::{
    OperatorCommand, RootView, RunMode, StopStatus, TimelineEntry, UiProjection,
};
use interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof;
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::planning::{PlannerOutput, PlanningRequest};
use interaction_contracts::desktop::source::{
    FrameMeta, SourceCommand, SourceHealth, StructureSnapshot,
};
use interaction_contracts::desktop::task::{GoalSpec, RootState};

use crate::deliberation::PlanningCoordinator;
use crate::environment::binding::BindingReducer;
use crate::environment::owned_document::OwnedDocumentBinding;
use crate::environment::owned_document::binding_ref as owned_binding_ref;
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
use computer::{ComputerCommand, ComputerInput, ComputerLane};

/// A round does at most this much before yielding, so one event cannot hold
/// the loop.
const MAX_ROUNDS: usize = 64;
const TIMELINE_LIMIT: usize = 40;

fn resets_owned_document_binding(input: &Input) -> bool {
    use interaction_contracts::desktop::device::DeviceEventKind;

    match input {
        Input::Operator(
            OperatorCommand::StopTask { .. }
            | OperatorCommand::PauseTask { .. }
            | OperatorCommand::ResumeTask { .. },
        ) => true,
        Input::Operator(OperatorCommand::SetMode { mode }) if *mode != RunMode::Execute => true,
        Input::DeviceLost { .. } => true,
        Input::Device { reply, .. } => {
            matches!(reply, &DeviceReply::Stopped(_) | &DeviceReply::Resumed(_))
        }
        Input::DeviceNotice(DeviceNotice::Event(event)) => matches!(
            &event.event,
            &DeviceEventKind::Stopped { .. }
                | &DeviceEventKind::Resumed { .. }
                | &DeviceEventKind::ChannelIsolated { .. }
                | &DeviceEventKind::ChannelRecovered
        ),
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Trusted closed page profile selected by Host configuration. GoalDraft
    /// and planner output cannot supply or change this authority.
    pub browser_page_profile: Option<BrowserPageProfile>,
    pub native_window_scope: Option<NativeWindowScope>,
    /// Host-authorized native window identity for passive BackgroundAX work.
    /// This scope never activates the window or grants foreground input.
    pub native_background_window_scope: Option<NativeBackgroundWindowScope>,
    /// Host-private startup registration for the read-only owned-document path.
    pub owned_document_registration: Option<Box<OwnedDocumentRegistration>>,
    /// Separate Host-authorized opt-in for the bounded owned-document Save action.
    /// SetValue remains available through the ordinary owned-reference route.
    pub allow_owned_document_save: bool,
    /// Host-authorized opt-in for the bounded Computer Use lane.
    pub computer_proposals_enabled: bool,
    pub tasks: TaskConfig,
    /// How old a snapshot may be and still be acted on.
    pub evidence_max_age_ms: u64,
    /// How long evidence handed to the device stays valid.
    pub evidence_ttl_ms: u64,
    pub source_heartbeat_timeout_ms: u64,
    pub decision_timeout_ms: u64,
    pub planning_timeout_ms: u64,
    /// How long a device command may go unanswered.
    pub command_timeout_ms: u64,
    /// How long before the source is asked again for evidence.
    pub refresh_timeout_ms: u64,
    pub action_duration_ms: u64,
    /// Quiet interval after a native AX publication before issuing authority.
    pub native_evidence_quiet_ms: u64,
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

#[derive(Debug, Clone)]
pub struct NativeWindowScope {
    pub app: String,
    pub pid: u32,
    pub window: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeBackgroundWindowScope {
    pub app: String,
    pub pid: u32,
    pub window: String,
    pub process_start_seconds: Option<u64>,
    pub process_start_micros: Option<u64>,
}

impl NativeBackgroundWindowScope {
    pub fn validate(&self) -> Result<(), String> {
        if self.app.trim().is_empty()
            || self.app.len() > 256
            || self.app.chars().any(char::is_control)
            || self.pid == 0
            || self.pid > i32::MAX as u32
            || self.window.trim().is_empty()
            || self.window.len() > 256
            || self.window.chars().any(char::is_control)
            || self.process_start_seconds.is_some() != self.process_start_micros.is_some()
            || self
                .process_start_seconds
                .is_some_and(|seconds| seconds == 0)
            || self
                .process_start_micros
                .is_some_and(|micros| micros >= 1_000_000)
        {
            return Err("invalid native BackgroundAX target window scope".into());
        }
        Ok(())
    }

    pub fn process_birth(&self) -> Option<(u64, u64)> {
        self.process_start_seconds.zip(self.process_start_micros)
    }
}

impl EngineConfig {
    pub fn new(mut tasks: TaskConfig) -> Self {
        // A plan may only name skills this build can actually run.
        tasks.registered_skills = Skill::IMPLEMENTED
            .into_iter()
            .filter(|skill| *skill != "ui.save_document")
            .map(String::from)
            .collect();
        Self {
            browser_page_profile: None,
            native_window_scope: None,
            native_background_window_scope: None,
            owned_document_registration: None,
            allow_owned_document_save: false,
            computer_proposals_enabled: false,
            tasks,
            evidence_max_age_ms: 2_000,
            evidence_ttl_ms: 3_000,
            source_heartbeat_timeout_ms: 2_000,
            decision_timeout_ms: 2_000,
            planning_timeout_ms: 30_000,
            command_timeout_ms: 2_000,
            refresh_timeout_ms: 1_000,
            action_duration_ms: 100,
            native_evidence_quiet_ms: 100,
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
    BrowserPage(Box<interaction_contracts::desktop::browser::BrowserPageSnapshot>),
    SourceHealth(SourceHealth),
    SourceRefreshStarted {
        generation: u64,
        accepted: bool,
    },
    Frame(Box<FrameMeta>),
    Device {
        corr: u64,
        reply: DeviceReply,
    },
    DeviceNotice(DeviceNotice),
    Decision(Box<DecisionProposal>),
    Computer(Box<ComputerInput>),
    Planner {
        request_id: RequestId,
        generation: u64,
        output: Box<PlannerOutput>,
    },
    PlannerFailed {
        request_id: RequestId,
        generation: u64,
        failure: crate::deliberation::PlanningFailure,
    },
    /// The connection to the device ended. Nothing more will come from it, and
    /// nothing it was told to release can be confirmed.
    DeviceLost {
        reason: String,
    },
    /// Time passed. Timeouts are the only thing that needs this.
    Tick,
}

/// Why a trusted browser page snapshot was not admitted by Environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserPageAdmissionError {
    /// Its read started at or before the active freshness floor.
    ReadBeforeFloor,
    /// Any other scope, freshness, shape, or replay rejection.
    Rejected,
}

/// What the shell must carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Device {
        corr: u64,
        command: Box<DeviceCommand>,
    },
    Source {
        generation: u64,
        command: SourceCommand,
    },
    /// Read-only refresh of the installed browser scope, routed through Main
    /// rather than the native SourceCommand protocol.
    BrowserRefresh(Box<BrowserRefreshRequest>),
    Decision(Box<DecisionRequest>),
    CancelDecision(DecisionCancel),
    Plan(Box<PlanningRequest>),
    CancelPlan(crate::deliberation::InFlight),
    Ui(Box<UiProjection>),
    Computer(Box<ComputerCommand>),
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
    pub window_activated: bool,
    /// Known keyboard input may preserve its descriptor for read-only results.
    /// This context never authorizes another input or survives a restart.
    pub keyboard_result_context: Option<interaction_contracts::desktop::source::FocusContext>,
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
    pub reconciliation_retries: u32,
    /// Input seen across all queried intents of an unknown attempt. Refunds
    /// are decided only after every intent in that attempt is settled.
    pub reconciled_input: BTreeSet<AttemptId>,
    pub effect_verification_only: bool,
    pub revalidate_floor: Option<u64>,
    pub permits: Vec<interaction_contracts::desktop::device::PermitRef>,
    pub stop_wait_since: Option<u64>,
    /// The device session that held the pause, so a resume onto another one
    /// (a device that restarted) does not wait for a latch that is not there.
    pub paused_session: Option<u64>,
    /// The latest attempt and intent, for tracing an effect back to its input.
    pub last_attempt: Option<(AttemptId, Option<String>)>,
}

/// Engine's last Source scope command, not an observed Source or certificate
/// fact. It only avoids redundant lifecycle clears within one live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceScopeCommandState {
    Unknown,
    Cleared,
    Scoped,
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
    /// Consumer handle only; Environment remains the sole owner of the proof.
    browser_read_binding_ref: Option<BindingRef>,
    pub(crate) stop_status: StopStatus,
    pub(crate) cleanup: ReleaseStatus,
    pub(crate) now_ms: u64,
    pub(crate) last_lease_ms: Option<u64>,
    pub(crate) last_structure_arrival_ms: Option<u64>,
    pub(crate) faulted: Option<String>,
    pub(crate) source_gate_closed: bool,
    /// Source sequence before the latest host replacement of the trusted
    /// native BackgroundAX window scope; old target metadata cannot rebind.
    pub(crate) background_scope_sequence_floor: Option<u64>,
    pub(crate) source_scope_command_state: SourceScopeCommandState,
    /// Correlates the acknowledgement of an out-of-band scope-clear command;
    /// it never represents Source evidence or a business refresh wait.
    pub(crate) source_scope_clear_generation: Option<u64>,
    pub(crate) window_context: Option<String>,
    /// Active device permits keyed to the exact generic BindingRef they use.
    /// Owned-document permits have a separate typed proof and are not entered.
    pub(crate) permit_bindings: BTreeMap<String, BindingRef>,
    /// Private permits are preserved across generic AX coverage loss only
    /// while their independently admitted receiver proof still matches.
    pub(crate) permit_owned_proofs: BTreeMap<String, OwnedDocumentBindingProof>,
    owned_revisit: Option<owned_revisit::OwnedRevisitProgress>,
    computer: ComputerLane,
}

impl Engine {
    pub fn new(mut cfg: EngineConfig, storage: Storage) -> Self {
        let save_authorized = cfg.allow_owned_document_save
            && cfg
                .owned_document_registration
                .as_deref()
                .is_some_and(|registration| {
                    registration.validate_identity().is_ok()
                        && registration.bundle_id == "com.apple.TextEdit"
                });
        cfg.allow_owned_document_save = save_authorized;
        let mut task_config = cfg.tasks.clone();
        task_config
            .registered_skills
            .retain(|skill| skill != "ui.save_document");
        if save_authorized {
            task_config
                .registered_skills
                .push("ui.save_document".into());
        }
        let mode = cfg.initial_mode;
        let tasks = TaskRuntime::new(task_config);
        let mut state = StateCore::with_frame_timeout(cfg.source_heartbeat_timeout_ms);
        state.install_owned_document_registration(
            cfg.owned_document_registration.clone(),
            cfg.evidence_max_age_ms,
        );
        Self {
            cfg,
            storage,
            tasks,
            state,
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
            browser_read_binding_ref: None,
            stop_status: StopStatus::NotStopped,
            cleanup: ReleaseStatus::NotNeeded,
            now_ms: 0,
            last_lease_ms: None,
            last_structure_arrival_ms: None,
            faulted: None,
            source_gate_closed: false,
            background_scope_sequence_floor: None,
            source_scope_command_state: SourceScopeCommandState::Unknown,
            source_scope_clear_generation: None,
            window_context: None,
            permit_bindings: BTreeMap::new(),
            permit_owned_proofs: BTreeMap::new(),
            owned_revisit: None,
            computer: ComputerLane::default(),
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

    /// The deterministic browser-reference planner is available only when
    /// Host has frozen the closed page profile and the current private Device
    /// manifest advertises its exact browser action capability.
    pub fn browser_reference_available(&self) -> bool {
        use interaction_contracts::desktop::browser::BrowserPageProfile;
        use interaction_contracts::desktop::device::BackendKind;
        use interaction_contracts::desktop::domain::{DomainAction, ExecutionDomain};

        self.cfg.browser_page_profile == Some(BrowserPageProfile::FixtureBackgroundOption)
            && self.device.manifest().is_some_and(|manifest| {
                manifest.validate().is_ok()
                    && manifest.backend == BackendKind::HeadlessBrowser
                    && manifest.execution_domain == ExecutionDomain::HeadlessBrowser
                    && manifest.supports(
                        ExecutionDomain::HeadlessBrowser,
                        DomainAction::HeadlessBrowserAction,
                    )
            })
    }

    pub fn bindings(&self) -> &BindingReducer {
        &self.bindings
    }

    pub fn state(&self) -> &StateCore {
        &self.state
    }

    /// The current Host-registered owned-document receiver, when its private
    /// Source proof is current. This view never authorizes input.
    pub fn owned_document_binding(&self) -> Option<&OwnedDocumentBinding> {
        self.state
            .owned_document_binding(self.now_ms, self.cfg.evidence_max_age_ms)
    }

    /// Rebuild the typed proof only from the current private Source binding
    /// and this process's private registration. It never feeds generic binding
    /// resolution or task completeness.
    pub(super) fn owned_document_proof_for(
        &self,
        target: &SemanticTargetRef,
    ) -> Option<OwnedDocumentBindingProof> {
        let registration = self.cfg.owned_document_registration.as_deref()?;
        let run = self.run.as_ref()?;
        if target != &run.goal.target.semantic_target
            || run.goal.scope.app != registration.bundle_id
            || run.pack.bundle_id != registration.bundle_id
            || self.state.latest()?.snapshot.app.bundle_id != registration.bundle_id
        {
            return None;
        }
        let binding = self.owned_document_binding()?;
        let proof = binding
            .binding_proof(
                owned_binding_ref(run.goal.root_task_id.as_str(), target.as_str()),
                target.clone(),
                binding.source_sequence(),
            )
            .ok()?;
        proof.validate_registration(registration).ok()?;
        proof
            .validate_at(self.now_ms, self.cfg.evidence_max_age_ms)
            .ok()?;
        Some(proof)
    }

    pub fn timeline(&self) -> impl Iterator<Item = &TimelineEntry> {
        self.timeline.iter()
    }

    /// Replace the Host-authorized passive BackgroundAX identity scope.
    /// This only narrows which same-read Source context may be compared; it
    /// never activates a window or grants Device input authority.
    pub fn set_native_background_window_scope(
        &mut self,
        scope: Option<NativeBackgroundWindowScope>,
    ) -> Result<(), EngineError> {
        if let Some(scope) = &scope {
            scope
                .validate()
                .map_err(|error| EngineError::Task(TaskError::Invalid(error)))?;
        }
        if self.cfg.native_background_window_scope == scope {
            return Ok(());
        }

        self.background_scope_sequence_floor =
            self.state.latest().map(|env| env.snapshot.source_sequence);
        self.cfg.native_background_window_scope = scope;
        self.set_evidence_dependency("background_scope", false);

        let background_selected = self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        }) || self.device.manifest().is_some_and(|manifest| {
            manifest.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        });
        if background_selected {
            self.invalidate_source_evidence(
                "the Host replaced the passive BackgroundAX window identity",
            )?;
        }
        self.note("passive BackgroundAX target scope changed; a newer Source read is required");
        Ok(())
    }

    /// Make the next admission fail at a named point, for tests.
    pub fn inject_admit_fault(&mut self, fault: Option<AdmitFault>) {
        self.tasks.inject_admit_fault(fault);
    }

    // ---- The one entry point --------------------------------------------------------

    /// Take one event and return what the shell must do about it.
    pub fn handle(&mut self, now_ms: u64, input: Input) -> Vec<Output> {
        self.now_ms = self.now_ms.max(now_ms);
        if resets_owned_document_binding(&input) {
            self.state.clear_owned_document_binding();
            if matches!(
                &input,
                Input::Operator(OperatorCommand::SetMode { mode }) if *mode != RunMode::Execute
            ) {
                self.require_fresh_browser_read_after(self.now_ms);
            } else {
                self.retire_browser_source_for_stop();
            }
        }
        if let Err(error) = self.expire_source_evidence() {
            self.fail_closed(error);
        }
        if let Err(error) = self.expire_pending_dispatch() {
            self.fail_closed(error);
        }
        if let Err(error) = self.dispatch(input) {
            self.fail_closed(error);
        }
        self.reconcile_computer_scope();
        self.pump();
        self.refresh_ui();
        std::mem::take(&mut self.outputs)
    }

    fn dispatch(&mut self, input: Input) -> Result<(), EngineError> {
        match input {
            Input::Operator(command) => self.on_operator(command),
            Input::Structure(snapshot) => self.on_structure(*snapshot),
            Input::BrowserPage(snapshot) => {
                if self.admit_browser_page(self.now_ms, *snapshot).is_err() {
                    self.note("browser page read rejected");
                }
                Ok(())
            }
            Input::SourceHealth(health) => self.on_health(health),
            Input::SourceRefreshStarted {
                generation,
                accepted,
            } => self.on_refresh_started(generation, accepted),
            Input::Frame(meta) => self.on_frame(*meta),
            Input::Device { corr, reply } => self.on_device_reply(corr, reply),
            Input::DeviceNotice(notice) => self.on_device_notice(notice),
            Input::DeviceLost { reason } => self.on_device_lost(&reason),
            Input::Decision(proposal) => self.on_decision(*proposal),
            Input::Computer(input) => self.on_computer_input(*input),
            Input::Planner {
                request_id,
                generation,
                output,
            } => self.on_planner(&request_id, generation, *output),
            Input::PlannerFailed {
                request_id,
                generation,
                failure,
            } => self.on_planner_failed(&request_id, generation, failure),
            Input::Tick => self.on_tick(),
        }
    }

    /// Load what survived a restart. Nothing resumes by itself: the operator
    /// has to say so, and the root is revalidated first.
    pub fn recover(&mut self, now_ms: u64) -> Vec<Output> {
        self.now_ms = self.now_ms.max(now_ms);
        self.cancel_computer_for_recovery();
        self.state.clear_owned_document_binding();
        self.retire_browser_source_for_stop();
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
        self.clear_source_table_scope(false);
        self.cancel_computer_for_recovery();
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
        if let Some(queued) = self.tasks.next_queued_task() {
            self.start_queued_task(queued)?;
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
            observation: self.state.observation_view(self.now_ms),
            browser_observation: self.browser_observation_view(),
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
