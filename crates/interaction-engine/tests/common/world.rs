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
    HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure, SubmitContext,
    SubmitOutcome,
};
use execution_core::core::{CoreConfig, CoreEvent, ExecutionCore};
use interaction_contracts::desktop::binding::instance_fingerprint;
use interaction_contracts::desktop::decision::{
    DecisionFailure, DecisionProposal, DecisionRequest, DecisionSource, PolicyChoice, Precondition,
};
use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, CommandAck, DependencyVersions, DeviceEvent,
    DeviceEventKind, GrantOutcome, StopCommand, StopDisposition, StopScope, StopSource, SurfaceRef,
};
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode, UiProjection};
use interaction_contracts::desktop::planning::PlanningRequest;
use interaction_contracts::desktop::primitive::{Primitive, PrimitivePolicy};
use interaction_contracts::desktop::source::SourceCommand;
use interaction_contracts::desktop::task::RootState;
use interaction_engine::engine::{Engine, EngineConfig, Input, Output};
use interaction_engine::environment::pack::AppPack;
use interaction_engine::execution::{DeviceCommand, DeviceNotice, DeviceReply};
use interaction_engine::infrastructure::storage::Storage;
use interaction_engine::tasks::TaskConfig;
use sim_world::app::{Applied, ApplyEffect, SimApp, SnapshotMeta};
use sim_world::policy::{ScriptedPlanner, ScriptedTeacher};

/// One input the simulated operating system received.
#[derive(Debug, Clone)]
pub struct InputRecord {
    pub at_ms: u64,
    pub primitive: Primitive,
    pub effect: ApplyEffect,
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
    app: Rc<RefCell<SimApp>>,
    log: Rc<RefCell<Vec<InputRecord>>>,
    clock: Rc<Cell<u64>>,
}

impl InputBackend for WorldBackend {
    fn submit(
        &mut self,
        at_ms: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome {
        let effect = self.app.borrow_mut().apply(
            &meta(at_ms),
            &Applied {
                primitive: primitive.clone(),
                binding_fingerprint: context.binding_fingerprint.map(str::to_string),
            },
        );
        self.log.borrow_mut().push(InputRecord {
            at_ms,
            primitive: primitive.clone(),
            effect,
        });
        SubmitOutcome::Applied
    }

    fn release(&mut self, _at_ms: u64, _release: &Primitive) -> Result<(), ReleaseFailure> {
        Ok(())
    }

    fn held(&self, _token: &str) -> HeldState {
        HeldState::NotHeld
    }

    fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome {
        let app = self.app.borrow();
        let snapshot = app.snapshot(&meta(self.clock.get()));
        let Some(active) = snapshot.active_layer.clone() else {
            return HitTestOutcome::Unresolved;
        };
        let digest_of = |object: &interaction_contracts::desktop::source::UiObject| {
            instance_fingerprint(&snapshot, object).digest()
        };
        let in_active =
            |object: &&interaction_contracts::desktop::source::UiObject| object.window_id == active;
        // A modal sheet takes every point and every action: whatever is under
        // the pointer is part of the sheet.
        let modal = snapshot.windows.iter().any(|window| window.modal);
        let expected_here = snapshot
            .objects
            .iter()
            .filter(in_active)
            .any(|object| digest_of(object) == request.expected_fingerprint);
        match request.primitive {
            Primitive::AxAction { .. } => {
                if expected_here {
                    HitTestOutcome::Element(request.expected_fingerprint.to_string())
                } else if modal {
                    snapshot
                        .objects
                        .iter()
                        .filter(in_active)
                        .find(|object| object.role == "sheet")
                        .map_or(HitTestOutcome::Unresolved, |object| {
                            HitTestOutcome::Element(digest_of(object))
                        })
                } else {
                    HitTestOutcome::Unresolved
                }
            }
            Primitive::Click { x, y, .. } => {
                let under = snapshot
                    .objects
                    .iter()
                    .filter(in_active)
                    .filter(|object| object.frame.contains(f64::from(*x), f64::from(*y)))
                    .min_by(|a, b| {
                        (a.frame.width * a.frame.height)
                            .partial_cmp(&(b.frame.width * b.frame.height))
                            .expect("finite areas")
                    });
                match under {
                    Some(object) => HitTestOutcome::Element(digest_of(object)),
                    None => HitTestOutcome::Unresolved,
                }
            }
            _ => HitTestOutcome::Unresolved,
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

pub struct World {
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
    pub source_sequence: u64,
    /// Snapshots stop while this is false.
    pub source_up: bool,
    clock: Rc<Cell<u64>>,
    inbox: VecDeque<Input>,
    /// What the device said but the host has not received yet.
    delayed: Vec<Input>,
    /// While true, the device's own notices are held instead of delivered.
    pub hold_notices: bool,
    /// The device carries out a stop but its answer never arrives.
    pub mute_stop: bool,
    /// Every command the engine sent the device, in order.
    pub device_log: Vec<(u64, &'static str)>,
    path: Option<PathBuf>,
    config: EngineConfig,
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
        let config_for_restart = config.clone();
        let app = Rc::new(RefCell::new(SimApp::new()));
        let inputs = Rc::new(RefCell::new(Vec::new()));
        let clock = Rc::new(Cell::new(0));
        let backend = WorldBackend {
            app: app.clone(),
            log: inputs.clone(),
            clock: clock.clone(),
        };
        let mut core = ExecutionCore::new(CoreConfig::production("sim"), backend);
        core.renew_lease(60_000);
        let identity = core.identity();
        let mut world = Self {
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
            source_sequence: 0,
            source_up: true,
            clock,
            inbox: VecDeque::new(),
            delayed: Vec::new(),
            hold_notices: false,
            mute_stop: false,
            device_log: Vec::new(),
            path: None,
            config: config_for_restart,
            identity,
            steps: 0,
        };
        world.introduce_device();
        world
    }

    /// The device tells the engine who it is.
    pub fn introduce_device(&mut self) {
        let manifest = CapabilityManifest {
            identity: self.core.identity(),
            backend: BackendKind::Simulated,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Available,
            input: CapabilityState::Available,
            max_uninterruptible_ms: 100,
        };
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
            Output::Source(SourceCommand::RefreshNow) => self.publish(),
            Output::Decision(request) => self.backend_answers(*request),
            Output::CancelDecision(_) => {}
            Output::Plan(request) => self.planner_answers(*request),
            Output::Ui(ui) => self.ui = Some(*ui),
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
        let reply = match command {
            DeviceCommand::RegisterRoot(grant) => {
                DeviceReply::Ack(match self.core.register_root(&grant) {
                    Ok(()) => CommandAck::ok(),
                    Err(reason) => CommandAck::rejected(None, reason),
                })
            }
            DeviceCommand::Observe(observation) => {
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
            DeviceCommand::QueryReceipt(query) => DeviceReply::Receipt(
                self.core
                    .query_receipt(query.session_epoch, &query.intent_id)
                    .map(Box::new),
            ),
            DeviceCommand::UpdateDependency(update) => {
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
            DeviceCommand::RenewLease(lease) => {
                self.core.renew_lease(lease.valid_for_ms);
                DeviceReply::Ack(CommandAck::ok())
            }
        };
        self.inbox.push_back(Input::Device { corr, reply });
        self.pump_core();
    }

    /// Forward what the device says on its own.
    fn pump_core(&mut self) {
        for event in self.core.drain_events() {
            let notice = match event {
                CoreEvent::ReceiptChanged(receipt) => DeviceNotice::Receipt(Box::new(receipt)),
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

    /// The source reports the application as it is now.
    pub fn publish(&mut self) {
        if !self.source_up {
            return;
        }
        self.source_sequence += 1;
        let snapshot = self.app.borrow().snapshot(&SnapshotMeta {
            snapshot_id: format!("snap-{}", self.source_sequence),
            session_epoch: self.identity.session_epoch,
            surface: self.identity.surface.clone(),
            clock_domain: self.identity.clock_domain.clone(),
            source_sequence: self.source_sequence,
            observed_ms: self.now,
        });
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
                    proposal.answers.clear();
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
        out
    }
}
