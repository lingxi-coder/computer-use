//! A simulated world for the engine: the settings application, a real
//! execution core in front of it, and scripted stand-ins for the planner and
//! the decision backend.
//!
//! Nothing here reads a clock. Time is a number the test moves, and every
//! message between the engine and the rest is carried by this loop, so the
//! order things happen in is exactly what the test scripted.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use execution_core::backend::{
    HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure, SubmitCompletion,
    SubmitContext, SubmitOutcome,
};
use execution_core::core::{CoreConfig, CoreEvent, ExecutionCore};
use interaction_contracts::desktop::decision::{
    DecisionFailure, DecisionProposal, DecisionRequest, DecisionSource, PolicyChoice, Precondition,
};
use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, CommandAck, DependencyVersions, DeviceEvent,
    DeviceEventKind, ExecutionDomainCapability, GrantOutcome, Observation, Receipt, ReceiptStatus,
    ReleaseStatus, StopCommand, StopDisposition, StopScope, StopSource, SurfaceRef,
};
use interaction_contracts::desktop::domain::{DomainAction, ExecutionDomain};
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode, UiProjection};
use interaction_contracts::desktop::planning::PlanningRequest;
use interaction_contracts::desktop::primitive::{Primitive, PrimitivePolicy};
use interaction_contracts::desktop::source::{SourceCommand, StructureSnapshot};
use interaction_contracts::desktop::task::RootState;
use interaction_engine::engine::computer::ComputerCommand;
use interaction_engine::engine::{Engine, EngineConfig, Input, Output};
use interaction_engine::environment::pack::AppPack;
use interaction_engine::execution::{DeviceCommand, DeviceNotice, DeviceReply};
use interaction_engine::infrastructure::storage::Storage;
use interaction_engine::tasks::TaskConfig;
use sim_world::app::{Applied, ApplyEffect, SimApp, SnapshotMeta};
use sim_world::policy::{ScriptedPlanner, ScriptedTeacher};

/// Build a closed five-slot capability vector for the selected fixture trust
/// profile. This file is also included directly by desktop-host tests, so the
/// helper lives beside the shared world rather than in its caller's module.
pub fn domain_capabilities_for_test_profile(
    active_domain: ExecutionDomain,
) -> Vec<ExecutionDomainCapability> {
    ExecutionDomain::ALL
        .into_iter()
        .map(|domain| {
            let actions = if domain == active_domain {
                match domain {
                    ExecutionDomain::ForegroundNative => vec![
                        DomainAction::NativePointer,
                        DomainAction::NativeKeyboard,
                        DomainAction::NativeScroll,
                        DomainAction::WindowActivation,
                        DomainAction::ForegroundAx,
                    ],
                    ExecutionDomain::BackgroundAx => vec![DomainAction::BackgroundAxPress],
                    ExecutionDomain::HeadlessBrowser => {
                        vec![DomainAction::HeadlessBrowserAction]
                    }
                    ExecutionDomain::AppIntegration => {
                        vec![DomainAction::AppIntegrationAction]
                    }
                    ExecutionDomain::Simulated => vec![DomainAction::SimulatedAction],
                }
            } else {
                Vec::new()
            };
            let supported = matches!(
                domain,
                ExecutionDomain::ForegroundNative
                    | ExecutionDomain::BackgroundAx
                    | ExecutionDomain::Simulated
            );
            ExecutionDomainCapability {
                domain,
                state: if domain == active_domain && supported {
                    CapabilityState::Available
                } else {
                    CapabilityState::Unavailable
                },
                actions: if domain == active_domain && supported {
                    actions
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

/// One input the simulated operating system received.
#[derive(Debug, Clone)]
pub struct InputRecord {
    pub at_ms: u64,
    pub primitive: Primitive,
    pub effect: ApplyEffect,
    pub permit: interaction_contracts::desktop::device::PermitRef,
    pub instance: Option<interaction_contracts::desktop::ids::ObjectRef>,
}

fn meta(at_ms: u64) -> SnapshotMeta {
    SnapshotMeta {
        snapshot_id: "device-view".into(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "sim/1/monotonic".into(),
        source_sequence: 0,
        observed_ms: at_ms,
    }
}

/// The input backend of the simulated device: it applies inputs to the
/// application and answers point-of-hit queries from what is really on screen.
pub struct WorldBackend {
    unknown_submission: Rc<Cell<bool>>,
    ignored_submission: Rc<Cell<bool>>,
    ignored_ax_setter: Rc<Cell<bool>>,
    async_applied_clicks: Rc<Cell<bool>>,
    pending_applied_submission: Rc<Cell<bool>>,
    next_async_completion_sequence: u64,
    pending_completion: Option<SubmitCompletion>,
    app: Rc<RefCell<SimApp>>,
    log: Rc<RefCell<Vec<InputRecord>>>,
    clock: Rc<Cell<u64>>,
}

impl WorldBackend {
    fn pending_applied_outcome(
        &mut self,
        at_ms: u64,
        context: &SubmitContext<'_>,
    ) -> Option<SubmitOutcome> {
        if !self.pending_applied_submission.replace(false) {
            return None;
        }

        let sequence = self.next_async_completion_sequence;
        self.next_async_completion_sequence = sequence
            .checked_add(1)
            .expect("simulated completion sequence does not wrap");
        Some(SubmitOutcome::Pending {
            sequence,
            reply_deadline_ms: at_ms
                .saturating_add(100)
                .min(context.authority.valid_until_ms.saturating_add(500)),
        })
    }

    fn applied_outcome(
        &mut self,
        at_ms: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome {
        if !self.async_applied_clicks.get() || !matches!(primitive, Primitive::Click { .. }) {
            return SubmitOutcome::Applied;
        }

        let sequence = self.next_async_completion_sequence;
        self.next_async_completion_sequence = sequence
            .checked_add(1)
            .expect("simulated completion sequence does not wrap");
        let input_end_ms = at_ms.saturating_add(1);
        if input_end_ms > context.authority.valid_until_ms {
            return SubmitOutcome::Applied;
        }
        let reply_deadline_ms = at_ms
            .saturating_add(100)
            .min(context.authority.valid_until_ms.saturating_add(500));
        self.pending_completion = Some(SubmitCompletion {
            sequence,
            session_epoch: context.intent.session_epoch,
            intent_id: context.intent.intent_id.clone(),
            outcome: SubmitOutcome::Applied,
            submitted_at_ms: Some(at_ms),
            input_end_ms: Some(input_end_ms),
            failure_code: None,
        });
        SubmitOutcome::Pending {
            sequence,
            reply_deadline_ms,
        }
    }
}

impl InputBackend for WorldBackend {
    fn submit(
        &mut self,
        at_ms: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome {
        if self.unknown_submission.get() {
            self.log.borrow_mut().push(InputRecord {
                at_ms,
                primitive: primitive.clone(),
                effect: ApplyEffect::Ignored("input outcome unknown"),
                permit: context.intent.permit.clone(),
                instance: context
                    .intent
                    .binding_proof
                    .as_ref()
                    .map(|proof| proof.instance.clone()),
            });
            return SubmitOutcome::Unknown;
        }
        if self.ignored_submission.get()
            || self.ignored_ax_setter.get()
                && matches!(
                    primitive,
                    Primitive::AxAction {
                        action: interaction_contracts::desktop::primitive::AxActionKind::SetValue { .. }
                    }
                )
        {
            self.log.borrow_mut().push(InputRecord {
                at_ms,
                primitive: primitive.clone(),
                effect: ApplyEffect::Ignored(
                    "OS acknowledged input but no business effect occurred",
                ),
                permit: context.intent.permit.clone(),
                instance: context
                    .intent
                    .binding_proof
                    .as_ref()
                    .map(|proof| proof.instance.clone()),
            });
            return self.applied_outcome(at_ms, primitive, context);
        }
        let effect = self.app.borrow_mut().apply(
            &meta(at_ms),
            &Applied {
                primitive: primitive.clone(),
                binding_fingerprint: context.intent.binding_fingerprint.clone(),
            },
        );
        self.log.borrow_mut().push(InputRecord {
            at_ms,
            primitive: primitive.clone(),
            effect,
            permit: context.intent.permit.clone(),
            instance: context
                .intent
                .binding_proof
                .as_ref()
                .map(|proof| proof.instance.clone()),
        });
        if let Some(outcome) = self.pending_applied_outcome(at_ms, context) {
            return outcome;
        }
        self.applied_outcome(at_ms, primitive, context)
    }

    fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        self.pending_completion.take()
    }

    fn abandon_pending(&mut self) {
        self.pending_completion = None;
    }

    fn release(&mut self, _at_ms: u64, _release: &Primitive) -> Result<(), ReleaseFailure> {
        Ok(())
    }

    fn held(&self, _token: &str) -> HeldState {
        HeldState::NotHeld
    }

    fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome {
        match self.app.borrow().hit_test(
            &meta(self.clock.get()),
            request.primitive,
            request
                .context
                .intent
                .binding_fingerprint
                .as_deref()
                .unwrap_or(""),
        ) {
            Some(digest) => HitTestOutcome::Element(digest),
            None => HitTestOutcome::Unresolved,
        }
    }
}

/// How the scripted decision backend behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Answering,
    /// It answers that it is unavailable.
    Unavailable,
    /// It never answers.
    Silent,
    /// Once, it picks something that was not offered as executable, then
    /// goes back to answering.
    HostileOnce,
}

pub type SnapshotTransform = Box<dyn Fn(&mut StructureSnapshot)>;

pub struct World {
    pub unknown_submission: Rc<Cell<bool>>,
    pub ignored_submission: Rc<Cell<bool>>,
    pub ignored_ax_setter: Rc<Cell<bool>>,
    /// Opt-in deterministic native-return simulation for exact receipt tests.
    pub async_applied_clicks: Rc<Cell<bool>>,
    /// Apply one input, then withhold its completion receipt to model a true
    /// in-flight submission. Disabled by default and consumed by one submit.
    pub pending_applied_submission: Rc<Cell<bool>>,
    /// Test-only receipt metadata fault: omit input_end_ms from the next
    /// completed click receipt forwarded by this world.
    missing_input_end_fault_armed: bool,
    missing_input_end_faulted_intent: Option<String>,
    pub engine: Engine,
    pub core: ExecutionCore<WorldBackend>,
    pub app: Rc<RefCell<SimApp>>,
    pub inputs: Rc<RefCell<Vec<InputRecord>>>,
    pub planner: ScriptedPlanner,
    pub teacher: ScriptedTeacher,
    pub backend: Backend,
    pub planner_silent: bool,
    pub now: u64,
    pub step_ms: u64,
    pub ui: Option<UiProjection>,
    pub decision_requests: Vec<DecisionRequest>,
    pub planning_requests: Vec<PlanningRequest>,
    /// Computer commands emitted by Engine, available to provider/Host tests.
    pub computer_commands: Vec<ComputerCommand>,
    pub computer_log: Vec<ComputerCommand>,
    pub source_capture_commands: Vec<SourceCommand>,
    pub source_sequence: u64,
    /// Snapshots stop while this is false.
    pub source_up: bool,
    pub snapshot_transform: Option<SnapshotTransform>,
    pub hold_refreshes: bool,
    pub refreshes: Vec<u64>,
    clock: Rc<Cell<u64>>,
    inbox: VecDeque<Input>,
    /// What the device said but the host has not received yet.
    delayed: Vec<Input>,
    /// While true, the device's own notices are held instead of delivered.
    pub hold_notices: bool,
    /// The device carries out a stop but its answer never arrives.
    pub mute_stop: bool,
    /// The device does not answer lease renewals, so the host never holds one.
    pub mute_lease: bool,
    /// How many receipt queries receive no reply, while the device stays alive.
    pub drop_query_replies: usize,
    /// Hold an accepted permit response while source invalidation races it.
    pub hold_grants: bool,
    pub hold_observes: bool,
    pub query_statuses: VecDeque<Option<ReceiptStatus>>,
    /// Every command the engine sent the device, in order.
    pub device_log: Vec<(u64, &'static str)>,
    /// Dependency validity changes, kept separate from the command-name log.
    pub dependency_updates: Vec<(String, Option<bool>)>,
    /// Source observations passed to the simulated core.
    pub observations: Vec<Observation>,
    path: Option<PathBuf>,
    config: EngineConfig,
    execution_domain: ExecutionDomain,
    identity: interaction_contracts::desktop::device::DeviceIdentity,
    steps: u64,
}

pub const TEXT: &str = r#"Turn off "Daily picks" in "Push""#;
pub const TEXT_COMMIT: &str = r#"Turn off "Daily picks" in "Push" and apply it"#;
pub const TEXT_AMBIGUOUS: &str = r#"Turn off "Daily picks""#;

pub fn engine_config() -> EngineConfig {
    let mut config = EngineConfig::new(TaskConfig::new(vec![AppPack::settings_fixture()]));
    config.initial_mode = RunMode::Execute;
    config
}

impl World {
    pub fn new() -> Self {
        Self::build(engine_config(), Storage::open_in_memory().unwrap())
    }

    pub fn with_config(config: EngineConfig) -> Self {
        Self::build(config, Storage::open_in_memory().unwrap())
    }

    /// Use a matched foreground-native profile for tests that explicitly
    /// inject pixels. The device and application remain deterministic fakes.
    pub fn foreground_native_fixture() -> Self {
        Self::with_config_foreground_native(engine_config())
    }

    pub fn with_config_foreground_native(config: EngineConfig) -> Self {
        Self::build_with_domain(
            config,
            Storage::open_in_memory().unwrap(),
            false,
            ExecutionDomain::ForegroundNative,
        )
    }

    /// A device that never grants the host's control lease until told to.
    pub fn with_lease_withheld() -> Self {
        let mut world = Self::build_with(engine_config(), Storage::open_in_memory().unwrap(), true);
        world.mute_lease = true;
        world.introduce_device();
        world
    }

    pub fn on_disk(path: &Path) -> Self {
        let mut world = Self::build(engine_config(), Storage::open(path).unwrap());
        world.path = Some(path.to_path_buf());
        world
    }

    /// The engine process dies and starts again on the same database. The
    /// device and the application carry on as they were.
    pub fn restart_engine(&mut self) {
        let path = self
            .path
            .clone()
            .expect("only an on-disk world can restart");
        // Whatever the old engine had not yet been told is lost with it.
        self.inbox.clear();
        self.delayed.clear();
        self.engine = Engine::new(self.config.clone(), Storage::open(&path).unwrap());
        let outputs = self.engine.recover(self.now);
        for output in outputs {
            self.carry(output);
        }
        self.drain();
        self.introduce_device();
    }

    fn build(config: EngineConfig, storage: Storage) -> Self {
        Self::build_with_domain(config, storage, false, ExecutionDomain::Simulated)
    }

    /// `hold_back_introduction` builds the world without the device having said
    /// hello yet, for tests that change how it behaves first.
    fn build_with(config: EngineConfig, storage: Storage, hold_back_introduction: bool) -> Self {
        Self::build_with_domain(
            config,
            storage,
            hold_back_introduction,
            ExecutionDomain::Simulated,
        )
    }

    fn build_with_domain(
        config: EngineConfig,
        storage: Storage,
        hold_back_introduction: bool,
        execution_domain: ExecutionDomain,
    ) -> Self {
        let config_for_restart = config.clone();
        let app = Rc::new(RefCell::new(SimApp::new()));
        let inputs = Rc::new(RefCell::new(Vec::new()));
        let clock = Rc::new(Cell::new(0));
        let async_applied_clicks = Rc::new(Cell::new(false));
        let pending_applied_submission = Rc::new(Cell::new(false));
        let backend = WorldBackend {
            unknown_submission: Rc::new(Cell::new(false)),
            ignored_submission: Rc::new(Cell::new(false)),
            ignored_ax_setter: Rc::new(Cell::new(false)),
            async_applied_clicks: async_applied_clicks.clone(),
            pending_applied_submission: pending_applied_submission.clone(),
            next_async_completion_sequence: 1,
            pending_completion: None,
            app: app.clone(),
            log: inputs.clone(),
            clock: clock.clone(),
        };
        let unknown_submission = backend.unknown_submission.clone();
        let ignored_submission = backend.ignored_submission.clone();
        let ignored_ax_setter = backend.ignored_ax_setter.clone();
        let mut core_config = CoreConfig::production("sim");
        core_config.execution_domain = execution_domain;
        let mut core = ExecutionCore::new(core_config, backend);
        core.renew_lease(60_000);
        let identity = core.identity();
        let mut world = Self {
            unknown_submission,
            ignored_submission,
            ignored_ax_setter,
            async_applied_clicks,
            pending_applied_submission,
            missing_input_end_fault_armed: false,
            missing_input_end_faulted_intent: None,
            engine: Engine::new(config, storage),
            core,
            app,
            inputs,
            planner: ScriptedPlanner::default(),
            teacher: ScriptedTeacher::default(),
            backend: Backend::Answering,
            planner_silent: false,
            now: 0,
            step_ms: 50,
            ui: None,
            decision_requests: Vec::new(),
            planning_requests: Vec::new(),
            computer_commands: Vec::new(),
            computer_log: Vec::new(),
            source_capture_commands: Vec::new(),
            source_sequence: 0,
            source_up: true,
            snapshot_transform: None,
            hold_refreshes: false,
            refreshes: Vec::new(),
            clock,
            inbox: VecDeque::new(),
            delayed: Vec::new(),
            hold_notices: false,
            mute_stop: false,
            mute_lease: false,
            drop_query_replies: 0,
            hold_grants: false,
            hold_observes: false,
            query_statuses: VecDeque::new(),
            device_log: Vec::new(),
            dependency_updates: Vec::new(),
            observations: Vec::new(),
            path: None,
            config: config_for_restart,
            execution_domain,
            identity,
            steps: 0,
        };
        if !hold_back_introduction {
            world.introduce_device();
        }
        world
    }

    /// The device tells the engine who it is.
    pub fn introduce_device(&mut self) {
        let manifest = CapabilityManifest {
            identity: self.core.identity(),
            backend: match self.execution_domain {
                ExecutionDomain::ForegroundNative | ExecutionDomain::BackgroundAx => {
                    BackendKind::Macos
                }
                ExecutionDomain::Simulated => BackendKind::Simulated,
                ExecutionDomain::HeadlessBrowser | ExecutionDomain::AppIntegration => {
                    unreachable!("the simulator only provides native or simulated fixtures")
                }
            },
            execution_domain: self.execution_domain,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Available,
            input: CapabilityState::Available,
            domains: domain_capabilities_for_test_profile(self.execution_domain),
            max_uninterruptible_ms: 100,
        };
        manifest
            .validate()
            .expect("the fixture manifest matches its closed execution profile");
        self.identity = manifest.identity.clone();
        self.send(Input::DeviceNotice(DeviceNotice::Manifest(Box::new(
            manifest,
        ))));
    }

    // ---- Driving the engine -----------------------------------------------------------

    /// Deliver one input and everything it sets in motion, at the current time.
    pub fn send(&mut self, input: Input) {
        self.inbox.push_back(input);
        self.drain();
    }

    fn drain(&mut self) {
        let mut guard = 0;
        while let Some(input) = self.inbox.pop_front() {
            guard += 1;
            assert!(guard < 10_000, "the world is not settling");
            let outputs = self.engine.handle(self.now, input);
            for output in outputs {
                self.carry(output);
            }
        }
    }

    pub fn operator(&mut self, command: OperatorCommand) {
        self.send(Input::Operator(command));
    }

    pub fn take_computer_commands(&mut self) -> Vec<ComputerCommand> {
        std::mem::take(&mut self.computer_commands)
    }

    /// Explicitly inject a missing-input-end metadata fault on the next
    /// completed click receipt. The device core's stored receipt stays intact.
    pub fn inject_missing_input_end_fault_for_next_completed_click(&mut self) {
        self.missing_input_end_fault_armed = true;
        self.missing_input_end_faulted_intent = None;
    }

    fn apply_receipt_metadata_fault(&mut self, receipt: &mut Receipt) {
        if self.missing_input_end_faulted_intent.as_deref() == Some(receipt.intent_id.as_str()) {
            receipt.input_end_ms = None;
            return;
        }

        let is_completed_click = receipt.status == ReceiptStatus::Completed
            && receipt.input_end_ms.is_some()
            && receipt.submitted.iter().any(|entry| {
                !entry.cleanup()
                    && entry
                        .native_primitive()
                        .is_some_and(|primitive| matches!(primitive, Primitive::Click { .. }))
            });
        if self.missing_input_end_fault_armed && is_completed_click {
            self.missing_input_end_fault_armed = false;
            self.missing_input_end_faulted_intent = Some(receipt.intent_id.clone());
            receipt.input_end_ms = None;
        }
    }

    pub fn create_task(&mut self, text: &str) {
        self.operator(OperatorCommand::CreateTask { text: text.into() });
    }

    /// Answer the confirmation the operator is being asked.
    pub fn answer(&mut self, approved: bool) {
        let id = self
            .ui
            .as_ref()
            .and_then(|ui| ui.pending_confirmation.as_ref())
            .map(|pending| pending.confirmation_id.clone())
            .expect("a confirmation is pending");
        self.operator(OperatorCommand::ConfirmGoal {
            confirmation_id: id,
            approved,
        });
    }

    /// Create a task, let it compile, and confirm the goal.
    pub fn start(&mut self, text: &str) {
        self.create_task(text);
        self.until(
            |w| w.engine.root_state() == Some(RootState::AwaitingConfirmation),
            5_000,
        );
        self.answer(true);
    }

    fn carry(&mut self, output: Output) {
        match output {
            Output::Device { corr, command } => self.device(corr, *command),
            Output::Source {
                generation,
                command: SourceCommand::RefreshNow { .. },
            } => {
                self.refreshes.push(generation);
                if !self.hold_refreshes {
                    self.inbox.push_back(Input::SourceRefreshStarted {
                        generation,
                        accepted: true,
                    });
                    // Match the real Source transport: retirement ACK precedes
                    // reports from the replacement watcher.
                    self.publish();
                }
            }
            Output::BrowserRefresh(_) => {}
            Output::Source {
                command:
                    command @ (SourceCommand::CaptureNow { .. } | SourceCommand::CancelCapture { .. }),
                ..
            } => {
                // A simulator cannot assert that native pixels were captured.
                // Computer tests explicitly inject a complete paired frame.
                self.source_capture_commands.push(command);
            }
            Output::Decision(request) => self.backend_answers(*request),
            Output::CancelDecision(_) => {}
            Output::CancelPlan(_) => {}
            Output::Plan(request) => self.planner_answers(*request),
            Output::Ui(ui) => self.ui = Some(*ui),
            Output::Computer(command) => {
                self.computer_log.push((*command).clone());
                self.computer_commands.push(*command);
            }
        }
    }

    fn device(&mut self, corr: u64, command: DeviceCommand) {
        let name = match &command {
            DeviceCommand::RegisterRoot(_) => "register_root",
            DeviceCommand::Observe(_) => "observe",
            DeviceCommand::GrantPermit(_) => "grant_permit",
            DeviceCommand::Submit(_) => "submit",
            DeviceCommand::Revoke(_) => "revoke",
            DeviceCommand::QueryReceipt(_) => "query_receipt",
            DeviceCommand::UpdateDependency(_) => "update_dependency",
            DeviceCommand::Stop(_) => "stop",
            DeviceCommand::Resume(_) => "resume",
            DeviceCommand::RenewLease(_) => "renew_lease",
        };
        self.device_log.push((self.now, name));
        let observing = matches!(&command, DeviceCommand::Observe(_));
        let reply = match command {
            DeviceCommand::RegisterRoot(grant) => {
                DeviceReply::Ack(match self.core.register_root(&grant) {
                    Ok(()) => CommandAck::ok(),
                    Err(reason) => CommandAck::rejected(None, reason),
                })
            }
            DeviceCommand::Observe(observation) => {
                if let Some(native) = observation.native() {
                    self.observations.push(native.clone());
                }
                DeviceReply::Ack(match self.core.observe(observation) {
                    Ok(()) => CommandAck::ok(),
                    Err(reason) => CommandAck::rejected(None, reason),
                })
            }
            DeviceCommand::GrantPermit(request) => {
                DeviceReply::Grant(match self.core.grant_permit(&request) {
                    Ok(permit) => GrantOutcome::Granted { permit },
                    Err(denial) => GrantOutcome::Denied {
                        code: denial.code,
                        reason: denial.reason,
                    },
                })
            }
            DeviceCommand::Submit(request) => {
                let receipt = self.core.submit(request);
                self.core.dispatch_next();
                DeviceReply::Receipt(Some(Box::new(receipt)))
            }
            DeviceCommand::Revoke(request) => DeviceReply::Ack(match self.core.revoke(request) {
                Ok(()) => CommandAck::ok(),
                Err(reason) => CommandAck::rejected(None, reason),
            }),
            DeviceCommand::QueryReceipt(_) if self.drop_query_replies > 0 => {
                self.drop_query_replies -= 1;
                return;
            }
            DeviceCommand::QueryReceipt(query) => {
                let mut receipt = self
                    .core
                    .query_receipt(query.session_epoch, &query.intent_id);
                if let Some(status) = self.query_statuses.pop_front() {
                    receipt = status.map(|status| {
                        let mut receipt = receipt.unwrap_or_else(|| Receipt {
                            intent_id: query.intent_id.clone(),
                            attempt_id: query.intent_id.strip_prefix("intent-").and_then(|id| {
                                id.rsplit_once('-').map(|(attempt, _)| attempt.into())
                            }),
                            session_epoch: query.session_epoch,
                            status,
                            reason_code: None,
                            reason: None,
                            submitted: vec![],
                            release_status: ReleaseStatus::NotNeeded,
                            command_end_ms: None,
                            input_end_ms: None,
                            at_ms: self.now,
                        });
                        receipt.status = status;
                        if status != ReceiptStatus::Completed {
                            receipt.submitted.clear();
                            receipt.command_end_ms = None;
                        }
                        receipt
                    });
                }
                if let Some(receipt) = receipt.as_mut() {
                    self.apply_receipt_metadata_fault(receipt);
                }
                DeviceReply::Receipt(receipt.map(Box::new))
            }
            DeviceCommand::UpdateDependency(update) => {
                self.dependency_updates
                    .push((update.name.clone(), update.valid));
                self.core.update_dependency(&update.name, update.valid);
                DeviceReply::Dependencies(DependencyVersions {
                    versions: self.core.dependency_versions(),
                })
            }
            DeviceCommand::Stop(stop) if self.mute_stop => {
                // The stop is carried out; nobody hears about it.
                self.hold_notices = true;
                self.core.stop(&stop);
                self.pump_core();
                return;
            }
            DeviceCommand::Stop(stop) => DeviceReply::Stopped(self.core.stop(&stop)),
            DeviceCommand::Resume(resume) => DeviceReply::Resumed(self.core.resume(&resume)),
            DeviceCommand::RenewLease(_) if self.mute_lease => return,
            DeviceCommand::RenewLease(lease) => {
                self.core.renew_lease(lease.valid_for_ms);
                DeviceReply::Ack(CommandAck::ok())
            }
        };
        if self.hold_grants && matches!(reply, DeviceReply::Grant(_))
            || self.hold_observes && observing
        {
            self.delayed.push(Input::Device { corr, reply });
        } else {
            self.inbox.push_back(Input::Device { corr, reply });
        }
        self.pump_core();
    }

    /// Forward what the device says on its own.
    fn pump_core(&mut self) {
        for event in self.core.drain_events() {
            let notice = match event {
                CoreEvent::ReceiptChanged(mut receipt) => {
                    self.apply_receipt_metadata_fault(&mut receipt);
                    DeviceNotice::Receipt(Box::new(receipt))
                }
                CoreEvent::PermitRevoked { permit, reason } => DeviceNotice::Event(DeviceEvent {
                    seq: 0,
                    at_ms: self.now,
                    event: DeviceEventKind::PermitRevoked { permit, reason },
                }),
                CoreEvent::Isolated { reason } => DeviceNotice::Event(DeviceEvent {
                    seq: 0,
                    at_ms: self.now,
                    event: DeviceEventKind::ChannelIsolated { reason },
                }),
                CoreEvent::Recovered => DeviceNotice::Event(DeviceEvent {
                    seq: 0,
                    at_ms: self.now,
                    event: DeviceEventKind::ChannelRecovered,
                }),
                CoreEvent::Stopped(ack) => DeviceNotice::Event(DeviceEvent {
                    seq: 0,
                    at_ms: self.now,
                    event: DeviceEventKind::Stopped { ack },
                }),
                CoreEvent::Resumed { scope } => DeviceNotice::Event(DeviceEvent {
                    seq: 0,
                    at_ms: self.now,
                    event: DeviceEventKind::Resumed { scope },
                }),
                CoreEvent::LeaseLost => DeviceNotice::Event(DeviceEvent {
                    seq: 0,
                    at_ms: self.now,
                    event: DeviceEventKind::LeaseLost,
                }),
            };
            let input = Input::DeviceNotice(notice);
            if self.hold_notices {
                self.delayed.push(input);
            } else {
                self.inbox.push_back(input);
            }
        }
    }

    /// Hand the host what the device said while it was not listening.
    pub fn deliver_delayed(&mut self) {
        self.hold_notices = false;
        for input in std::mem::take(&mut self.delayed) {
            self.inbox.push_back(input);
        }
        self.drain();
    }

    pub fn take_delayed(&mut self) -> Vec<Input> {
        std::mem::take(&mut self.delayed)
    }

    /// The source reports the application as it is now.
    pub fn publish(&mut self) {
        if !self.source_up {
            return;
        }
        self.source_sequence += 1;
        let mut snapshot = self.app.borrow().snapshot(&SnapshotMeta {
            snapshot_id: format!("snap-{}", self.source_sequence),
            session_epoch: self.identity.session_epoch,
            surface: self.identity.surface.clone(),
            clock_domain: self.identity.clock_domain.clone(),
            source_sequence: self.source_sequence,
            observed_ms: self.now,
        });
        if let Some(transform) = &self.snapshot_transform {
            transform(&mut snapshot);
        }
        self.inbox.push_back(Input::Structure(Box::new(snapshot)));
    }

    fn backend_answers(&mut self, request: DecisionRequest) {
        self.decision_requests.push(request.clone());
        match self.backend {
            Backend::Silent => {}
            Backend::Unavailable => {
                let mut proposal: DecisionProposal = self.teacher.decide(&request);
                proposal.choice = None;
                proposal.answers.clear();
                proposal.failure = Some(DecisionFailure::Unavailable);
                proposal.source = DecisionSource::Teacher;
                self.inbox.push_back(Input::Decision(Box::new(proposal)));
            }
            Backend::Answering => {
                let proposal = self.teacher.decide(&request);
                self.inbox.push_back(Input::Decision(Box::new(proposal)));
            }
            Backend::HostileOnce => {
                self.backend = Backend::Answering;
                let mut proposal = self.teacher.decide(&request);
                if let Some(refused) = request
                    .state
                    .candidates
                    .iter()
                    .find(|c| !matches!(c.precondition, Precondition::Executable))
                {
                    proposal.choice = Some(PolicyChoice::StartSkill {
                        candidate: refused.candidate_id.clone(),
                    });
                    // Keep the question identity intact so this hostile case
                    // exercises selection outside the offered action options.
                    for answer in &mut proposal.answers {
                        answer.selected = Some(refused.candidate_id.as_str().into());
                    }
                }
                self.inbox.push_back(Input::Decision(Box::new(proposal)));
            }
        }
    }

    fn planner_answers(&mut self, request: PlanningRequest) {
        self.planning_requests.push(request.clone());
        if self.planner_silent {
            return;
        }
        let output = self.planner.plan(&request);
        self.inbox.push_back(Input::Planner {
            request_id: request.request_id,
            generation: request.generation,
            output: Box::new(output),
        });
    }

    // ---- Time ---------------------------------------------------------------------------------

    /// One step: time moves, the device and the source run, the engine ticks.
    pub fn step(&mut self) {
        self.steps += 1;
        self.now += self.step_ms;
        self.clock.set(self.now);
        self.core.advance_to(self.now).unwrap();
        self.core.dispatch_next();
        self.pump_core();
        // The source reports every other step.
        if self.steps.is_multiple_of(2) {
            self.publish();
        }
        self.inbox.push_back(Input::Tick);
        self.drain();
    }

    pub fn run_for(&mut self, ms: u64) {
        let until = self.now + ms;
        while self.now < until {
            self.step();
        }
    }

    /// Step until `done`, or fail with what happened.
    pub fn until(&mut self, done: impl Fn(&World) -> bool, max_ms: u64) {
        let limit = self.now + max_ms;
        while !done(self) {
            assert!(
                self.now < limit,
                "gave up after {max_ms} ms of simulated time\n{}",
                self.report()
            );
            self.step();
        }
    }

    // ---- What the test can ask -----------------------------------------------------------------

    pub fn state(&self) -> Option<RootState> {
        self.engine.root_state()
    }

    /// The root's state now, or the state it ended in.
    pub fn outcome(&self) -> Option<RootState> {
        self.engine
            .root_state()
            .or_else(|| self.engine.last_finished())
    }

    pub fn finished(&self) -> bool {
        self.engine.root_state().is_none() && self.engine.last_finished().is_some()
    }

    pub fn switch(&self, group: &str, label: &str) -> Option<bool> {
        self.app.borrow().switch_on(group, label)
    }

    pub fn applied(&self) -> Vec<InputRecord> {
        self.inputs.borrow().clone()
    }

    /// A stop as the operator's button sends it: straight to the device.
    pub fn stop_at_device(&mut self, scope: StopScope, disposition: StopDisposition) {
        let ack = self.core.stop(&StopCommand {
            scope,
            disposition,
            source: StopSource::OperatorButton,
            reason: "the operator pressed stop".into(),
        });
        let _ = ack;
        self.pump_core();
        self.drain();
    }

    /// How many times the engine sent the device this command.
    pub fn sent(&self, name: &str) -> usize {
        self.device_log.iter().filter(|(_, n)| *n == name).count()
    }

    pub fn log_len(&self) -> usize {
        self.device_log.len()
    }

    /// Commands sent after position `since` in the log.
    pub fn sent_since(&self, since: usize, name: &str) -> usize {
        self.device_log[since..]
            .iter()
            .filter(|(_, n)| *n == name)
            .count()
    }

    pub fn pending_confirmation(
        &self,
    ) -> Option<interaction_contracts::desktop::operator::ConfirmationRequest> {
        self.ui
            .as_ref()
            .and_then(|ui| ui.pending_confirmation.clone())
    }

    /// What happened, for a failing test's message.
    pub fn report(&self) -> String {
        let mut out = format!(
            "state {:?}, finished {:?}, stop status {:?}\n",
            self.engine.root_state(),
            self.engine.last_finished(),
            self.engine.stop_status()
        );
        if let Some(reason) = self.engine.faulted() {
            out.push_str(&format!("FAULTED: {reason}\n"));
        }
        for entry in self.engine.timeline() {
            out.push_str(&format!("  [{:>6}] {}\n", entry.at_ms, entry.text));
        }
        out.push_str(&format!("inputs: {:?}\n", self.inputs.borrow()));
        if !self.computer_log.is_empty() {
            out.push_str(&format!("computer commands: {:?}\n", self.computer_log));
        }
        out
    }
}
