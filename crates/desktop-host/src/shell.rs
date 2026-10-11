//! The host process's loop.
//!
//! One thread runs the engine. Reader threads feed it from the supervisor, the
//! device and the source; a short timeout drives the engine's own clock-based
//! work. Everything the engine says goes out as a wire message, and everything
//! that arrives is turned into an input before the engine sees it.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use desktop_ipc::SharedClock;
use desktop_ipc::handshake::{Connection, make_hello, read_ready, send_hello};
use desktop_ipc::link::{FramedReader, Inbound, Link, spawn_reader};
use desktop_ipc::socket;
use interaction_contracts::desktop::decision::{
    DecisionFailure, DecisionProposal, DecisionRequest, DecisionSource,
};
use interaction_contracts::desktop::device::{BackendKind, CapabilityState, CommandAck};
use interaction_contracts::desktop::handshake::{Configure, PeerRole, Ready};
use interaction_contracts::desktop::message::Message;
use interaction_contracts::desktop::source::{
    SourceCommand, SourceHealth, SourceKind, SourceState,
};
use interaction_engine::engine::owned_revisit::{
    OwnedRevisitChallenge, OwnedRevisitCompletion, OwnedRevisitError, OwnedRevisitStatus,
};
use interaction_engine::engine::{BrowserPageAdmissionError, Engine, EngineConfig, Input, Output};
use interaction_engine::environment::pack::AppPack;
use interaction_engine::infrastructure::storage::Storage;
use interaction_engine::tasks::TaskConfig;
use sim_world::policy::ScriptedPlanner;

use crate::bridge::{Bridge, command_message, source_input};
use crate::computer_image::ComputerImagePort;
use crate::computer_session::ComputerSession;
use crate::config::{HostConfig, PlanningBackend};
use crate::model_bootstrap::{HostModelServices, ModelIdentity};
use crate::owned_revisit_control::{
    HostMessage as OwnedRevisitReply, MainMessage as OwnedRevisitRequest, RejectionCode,
    VERSION as OWNED_REVISIT_VERSION, is_canonical_transition_id,
    read_main as read_owned_revisit_request, write_host as write_owned_revisit_reply,
};
use crate::source_capture::SourceCaptureBridge;
use crate::source_refresh::SourceRefreshBridge;
use crate::teacher_observation::{
    TeacherObservation, TeacherObservationReason, TeacherObservationWriter,
};
use harness_adapter::TurnResult;
use harness_adapter::model::ModelPlanner;
use harness_adapter::model::ModelTransport;
use harness_adapter::teacher::{TeacherAdapter, TeacherTurnResult};
use interaction_contracts::desktop::planning::PlanningRequest;
use interaction_engine::deliberation::PlanningFailure;
use interaction_engine::deliberation::accounting::{ModelCompletion, ModelOutcome, ModelUsage};

const TICK: Duration = Duration::from_millis(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OWNED_REVISIT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(test)]
#[path = "shell/teacher_observation_tests.rs"]
mod teacher_observation_tests;

enum Event {
    Supervisor(Inbound),
    Device(u64, Inbound),
    Source(u64, Inbound),
    OwnedRevisit(Result<OwnedRevisitRequest<OwnedRevisitCompletion>, String>),
}

#[cfg(test)]
mod model_tests;

#[cfg(test)]
#[path = "computer_session_tests.rs"]
mod computer_session_tests;

struct Peer {
    id: u64,
    link: Link<UnixStream>,
}

struct Shell<W: Write> {
    engine: Engine,
    bridge: Bridge,
    supervisor: Link<W>,
    device: Option<Peer>,
    source: Option<Peer>,
    source_refresh: SourceRefreshBridge,
    source_capture: SourceCaptureBridge,
    clock: SharedClock,
    config: HostConfig,
    tx: Sender<Event>,
    next_conn: u64,
    planner: ScriptedPlanner,
    model_planner: Option<ModelPlanner>,
    pending_model: Option<Box<PlanningRequest>>,
    inflight_model: Option<(String, u64)>,
    teacher: Option<TeacherAdapter<ModelTransport>>,
    teacher_identity: Option<ModelIdentity>,
    computer: Option<ComputerSession>,
    pending_teacher: Option<DecisionRequest>,
    teacher_observation: Option<TeacherObservationWriter>,
    teacher_observation_terminal: bool,
    pending_supervisor_decisions: std::collections::BTreeSet<(String, u64)>,
    queue: VecDeque<Input>,
    owned_revisit: Option<OwnedRevisitSession>,
    suppress_model_results: std::collections::BTreeSet<(String, u64)>,
    owned_diagnostic_last: Option<(Option<u64>, bool)>,
    owned_diagnostic_count: u8,
    exit: bool,
}

struct OwnedRevisitSession {
    writer: UnixStream,
    pending: Option<PendingOwnedRevisit>,
    active: Option<ActiveOwnedRevisit>,
    seen_request_ids: std::collections::BTreeSet<String>,
    failed_closed: bool,
}

struct PendingOwnedRevisit {
    request_id: String,
    challenge: OwnedRevisitChallenge,
}

struct ActiveOwnedRevisit {
    transition_id: String,
}

type Connected = (Peer, Ready, FramedReader<UnixStream>);

fn teacher_failure(request: DecisionRequest, failure: DecisionFailure) -> DecisionProposal {
    DecisionProposal {
        decision_id: request.decision_id,
        request_generation: request.request_generation,
        candidate_set_digest: request.state.candidate_set_digest,
        backend: "s1_teacher".into(),
        source: DecisionSource::Teacher,
        model_revision: "unavailable".into(),
        answers: vec![],
        choice: None,
        failure: Some(failure),
        truncated: false,
        queue_ms: 0,
        inference_ms: 0,
    }
}

fn connect_peer(
    path: &Path,
    token: &str,
    epoch: u64,
    clock: SharedClock,
    id: u64,
    timeout: Duration,
) -> Result<Connected, String> {
    let stream = socket::connect(path, timeout).map_err(|e| format!("connecting: {e}"))?;
    let mut conn = Connection::new(stream, "host", clock).map_err(|e| e.to_string())?;
    send_hello(&mut conn, make_hello(PeerRole::Host, token, epoch, clock))
        .map_err(|e| e.to_string())?;
    let ready = read_ready(&mut conn).map_err(|e| e.to_string())?;
    Ok((
        Peer {
            id,
            link: conn.link,
        },
        ready,
        conn.reader,
    ))
}

/// Run the host until the supervisor is gone or says to stop.
pub fn run<R, W>(
    config: HostConfig,
    supervisor: Link<W>,
    supervisor_reader: FramedReader<R>,
    owned_revisit_control: Option<UnixStream>,
) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write,
{
    run_with_planner(
        config,
        supervisor,
        supervisor_reader,
        None,
        owned_revisit_control,
    )
}

/// The model worker is explicitly authorized before entering the host loop.
pub fn run_with_planner<R, W>(
    config: HostConfig,
    supervisor: Link<W>,
    supervisor_reader: FramedReader<R>,
    model_planner: Option<ModelPlanner>,
    owned_revisit_control: Option<UnixStream>,
) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write,
{
    run_with_models(
        config,
        supervisor,
        supervisor_reader,
        HostModelServices {
            planner: model_planner,
            ..HostModelServices::default()
        },
        owned_revisit_control,
        None,
        None,
    )
}

/// Run with separately authorized S2 planning and S1 teacher model slots.
pub fn run_with_models<R, W>(
    config: HostConfig,
    supervisor: Link<W>,
    supervisor_reader: FramedReader<R>,
    services: HostModelServices,
    owned_revisit_control: Option<UnixStream>,
    teacher_observation: Option<TeacherObservationWriter>,
    computer_image: Option<UnixStream>,
) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write,
{
    let HostModelServices {
        planner: model_planner,
        teacher,
        teacher_identity,
        computer,
        ..
    } = services;
    if (config.planning_backend == PlanningBackend::Model) != model_planner.is_some() {
        return Err("model backend requires matching private authorization".into());
    }
    if teacher.is_some() != teacher_identity.is_some() {
        return Err("teacher backend requires matching private authorization".into());
    }
    if teacher_observation.is_some() && teacher.is_none() {
        return Err("teacher observation requires matching private S1 authorization".into());
    }
    if computer.is_some() != computer_image.is_some()
        || (computer.is_some()
            && (config.native_window_scope.is_none()
                || config.native_background_window_scope.is_some()
                || config.native_target_bundle.is_none()))
    {
        return Err(
            "computer images require matching purpose and native window authorization".into(),
        );
    }
    let computer_images = computer_image
        .map(|stream| {
            let clock = config.clock;
            ComputerImagePort::new(stream, std::sync::Arc::new(move || clock.now_ms()))
        })
        .transpose()?;
    let computer = computer
        .zip(computer_images)
        .map(|(worker, pixels)| ComputerSession::new(worker, pixels));
    if owned_revisit_control.is_some()
        && (!config.allow_owned_document_save || config.owned_document_registration.is_none())
    {
        return Err("private owned revisit needs owned Save authorization".into());
    }
    let storage = Storage::open(&config.db_path).map_err(|e| format!("the ledger: {e}"))?;
    let packs = if config.browser_page_profile.is_some() {
        vec![AppPack::browser_fixture()]
    } else if let Some(bundle) = &config.native_target_bundle {
        vec![AppPack::registered_native(bundle).unwrap_or_else(|| AppPack::observe_only(bundle))]
    } else {
        vec![AppPack::settings_fixture()]
    };
    let mut engine_config = EngineConfig::new(TaskConfig::new(packs));
    engine_config.browser_page_profile = config.browser_page_profile;
    if config.native_target_bundle.is_some() {
        // The native guard's lease is capped at 1500 ms. Renew the actual
        // shorter lease on the same cadence instead of a simulated 5s lease.
        engine_config.lease_ms = 750;
        engine_config.action_duration_ms = 250;
        engine_config.refresh_timeout_ms = 2000;
    }
    engine_config.computer_proposals_enabled = computer.is_some();
    engine_config.initial_mode = config.initial_mode;
    engine_config.native_window_scope = config.native_window_scope.clone();
    engine_config.native_background_window_scope = config.native_background_window_scope.clone();
    engine_config.owned_document_registration = config.owned_document_registration.clone();
    engine_config.allow_owned_document_save = config.allow_owned_document_save;
    let engine = Engine::new(engine_config, storage);

    let (tx, rx) = mpsc::channel::<Event>();
    spawn_reader(
        "host-supervisor",
        supervisor_reader,
        tx.clone(),
        Event::Supervisor,
    );
    let owned_revisit = if let Some(stream) = owned_revisit_control {
        let reader = stream
            .try_clone()
            .map_err(|error| format!("private owned revisit channel: {error}"))?;
        spawn_owned_revisit_reader(reader, tx.clone())?;
        Some(OwnedRevisitSession {
            writer: stream,
            pending: None,
            active: None,
            seen_request_ids: std::collections::BTreeSet::new(),
            failed_closed: false,
        })
    } else {
        None
    };
    let mut shell = Shell {
        engine,
        bridge: Bridge::new(),
        supervisor,
        device: None,
        source: None,
        source_refresh: SourceRefreshBridge::default(),
        source_capture: SourceCaptureBridge::default(),
        clock: config.clock,
        config,
        tx,
        next_conn: 0,
        planner: ScriptedPlanner::default(),
        model_planner,
        pending_model: None,
        inflight_model: None,
        teacher,
        teacher_identity,
        computer,
        pending_teacher: None,
        teacher_observation,
        teacher_observation_terminal: false,
        pending_supervisor_decisions: std::collections::BTreeSet::new(),
        queue: VecDeque::new(),
        owned_revisit,
        suppress_model_results: std::collections::BTreeSet::new(),
        owned_diagnostic_last: None,
        owned_diagnostic_count: 0,
        exit: false,
    };

    let now = shell.clock.now_ms();
    let outputs = shell.engine.recover(now);
    shell.carry_all(outputs);
    shell.connect_device(CONNECT_TIMEOUT)?;
    shell.connect_source()?;
    shell
        .supervisor
        .send(Message::Ready(Ready {
            role: PeerRole::Host,
            capabilities: None,
            notes: vec![],
        }))
        .map_err(|e| format!("the supervisor pipe: {e}"))?;
    shell.drain();

    let mut last_tick = Instant::now();
    while !shell.exit {
        match rx.recv_timeout(TICK) {
            Ok(event) => shell.on_event(event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            shell.queue.push_back(Input::Tick);
        }
        shell.check_owned_revisit_timeout();
        shell.drain();
        shell.refresh_owned_revisit_phase();
        shell.poll_model();
        shell.poll_teacher();
        shell.poll_computer();
    }
    shell.close_model();
    if let Some(observer) = &mut shell.teacher_observation {
        observer.close();
    }
    shell.fail_owned_revisit(RejectionCode::TransitionFailed, true);
    Ok(())
}

fn spawn_owned_revisit_reader(reader: UnixStream, tx: Sender<Event>) -> Result<(), String> {
    std::thread::Builder::new()
        .name("host-owned-revisit-control".into())
        .spawn(move || {
            let mut reader = reader;
            loop {
                let message = read_owned_revisit_request::<OwnedRevisitCompletion>(&mut reader);
                let failed = message.is_err();
                if tx.send(Event::OwnedRevisit(message)).is_err() || failed {
                    break;
                }
            }
        })
        .map_err(|_| "private owned revisit reader could not start".to_string())?;
    Ok(())
}

impl<W: Write> Shell<W> {
    fn observe_teacher(&self, message: TeacherObservation) {
        if let Some(observer) = &self.teacher_observation {
            let _ = observer.try_observe(message);
        }
    }

    fn observe_teacher_terminal(
        &mut self,
        request: &DecisionRequest,
        reason: TeacherObservationReason,
    ) {
        if !self.teacher_observation_terminal {
            self.observe_teacher(TeacherObservation::cancelled(request, reason));
            self.teacher_observation_terminal = true;
        }
    }

    fn close_model(&mut self) {
        self.exit = true;
        self.close_computer();
        if let Some(request) = self.pending_teacher.clone() {
            self.observe_teacher_terminal(&request, TeacherObservationReason::Shutdown);
        }
        self.pending_model = None;
        if let Some(planner) = &mut self.model_planner {
            planner.revoke(self.clock.now_ms());
        }
        if let Some(teacher) = &mut self.teacher {
            teacher.revoke(self.clock.now_ms());
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            self.poll_model();
            self.poll_teacher();
            self.poll_computer();
            if (self
                .model_planner
                .as_mut()
                .is_none_or(ModelPlanner::is_idle)
                && self.pending_teacher.is_none()
                && self.computer.as_ref().is_none_or(ComputerSession::is_idle))
                || Instant::now() >= deadline
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // Preserve usage in completions queued before shutdown, without
        // installing their payloads or pumping another device action.
        while let Some(input) = self.queue.pop_front() {
            self.account_closing_computer_input(input);
        }
    }

    fn close_computer(&mut self) {
        if let Some(session) = &mut self.computer {
            session.revoke();
        }
    }

    fn poll_computer(&mut self) {
        let Some(mut session) = self.computer.take() else {
            return;
        };
        let inputs = session.poll(&mut self.engine, self.clock.now_ms());
        self.computer = Some(session);
        if self.exit {
            // Finish accounting after cancellation without admitting another action.
            for input in inputs {
                self.account_closing_computer_input(input);
            }
        } else {
            self.queue.extend(inputs);
        }
    }

    fn account_closing_computer_input(&mut self, input: Input) {
        use interaction_engine::deliberation::computer_accounting::ComputerCallOutcome;
        use interaction_engine::engine::computer::ComputerInput;
        if let Input::Computer(event) = input {
            match *event {
                ComputerInput::Finished { mut completion }
                | ComputerInput::Failed { mut completion, .. } => {
                    if completion.outcome == ComputerCallOutcome::Proposal {
                        completion.outcome = ComputerCallOutcome::Revoked;
                    }
                    let _ = self.engine.computer_call_finished(&completion);
                }
                _ => {}
            }
        }
    }

    fn send_owned_revisit_reply(
        &mut self,
        reply: &OwnedRevisitReply<OwnedRevisitChallenge>,
    ) -> Result<(), String> {
        let Some(session) = self.owned_revisit.as_mut() else {
            return Err("private owned revisit is unavailable".into());
        };
        if session.failed_closed {
            return Err("private owned revisit is closed".into());
        }
        write_owned_revisit_reply(&mut session.writer, reply)
    }

    fn check_owned_revisit_timeout(&mut self) {
        let expired = self
            .owned_revisit
            .as_ref()
            .and_then(|session| session.pending.as_ref())
            .is_some_and(|pending| self.clock.now_ms() >= pending.challenge.deadline_ms);
        if expired {
            self.fail_owned_revisit(RejectionCode::Timeout, true);
        }
    }

    fn fence_old_owned_session(&mut self) {
        self.queue.retain(|input| {
            !matches!(
                input,
                Input::Structure(_)
                    | Input::SourceHealth(_)
                    | Input::SourceRefreshStarted { .. }
                    | Input::Frame(_)
                    | Input::Device { .. }
                    | Input::DeviceNotice(_)
                    | Input::DeviceLost { .. }
                    | Input::Decision(_)
                    | Input::Planner { .. }
                    | Input::PlannerFailed { .. }
            )
        });
        self.source_refresh.clear();
        self.source_capture.clear();
        self.bridge.forget_all();
        if let Some(device) = self.device.take() {
            let _ = device.link.get_ref().shutdown(std::net::Shutdown::Both);
        }
        if let Some(source) = self.source.take() {
            let _ = source.link.get_ref().shutdown(std::net::Shutdown::Both);
        }
        self.pending_model = None;
        if let Some((request_id, generation)) = self.inflight_model.take() {
            self.suppress_model_results
                .insert((request_id.clone(), generation));
            if let Some(planner) = &mut self.model_planner {
                if let Ok(request_id) =
                    interaction_contracts::desktop::ids::RequestId::new(request_id)
                {
                    planner.cancel(&request_id, generation, self.clock.now_ms());
                } else {
                    planner.revoke(self.clock.now_ms());
                }
            }
        }
        if let Some(request) = self.pending_teacher.take()
            && let Some(teacher) = &mut self.teacher
        {
            teacher.cancel(
                request.decision_id.as_str(),
                request.request_generation,
                self.clock.now_ms(),
            );
        }
        let outputs = self.engine.handle(self.clock.now_ms(), Input::Tick);
        self.carry_all(outputs);
        self.pending_supervisor_decisions.clear();
    }

    fn cancel_pending_owned_revisit(&mut self, reason: &'static str) {
        let engine_transition = self
            .engine
            .owned_revisit_transition_id()
            .map(str::to_string);
        let transition_id = self.owned_revisit.as_mut().and_then(|session| {
            if let Some(pending) = session.pending.take() {
                session.active = None;
                return Some(pending.challenge.transition_id);
            }
            let active = session.active.as_ref()?;
            if engine_transition.as_deref() != Some(active.transition_id.as_str()) {
                return None;
            }
            let transition_id = active.transition_id.clone();
            session.active = None;
            Some(transition_id)
        });
        if let Some(transition_id) = transition_id {
            let _ = self
                .engine
                .cancel_owned_revisit(&transition_id, self.clock.now_ms(), reason);
            self.fence_old_owned_session();
        }
    }

    fn refresh_owned_revisit_phase(&mut self) {
        let Some(active) = self
            .owned_revisit
            .as_ref()
            .and_then(|session| session.active.as_ref())
        else {
            return;
        };
        if self.engine.owned_revisit_transition_id() != Some(active.transition_id.as_str())
            && let Some(session) = self.owned_revisit.as_mut()
        {
            session.active = None;
        }
    }

    fn fail_owned_revisit(&mut self, code: RejectionCode, close_channel: bool) {
        let request_id = self
            .owned_revisit
            .as_ref()
            .and_then(|session| session.pending.as_ref())
            .map(|pending| pending.request_id.clone());
        self.cancel_pending_owned_revisit("private revisit transition failed closed");
        if let Some(request_id) = request_id {
            let reply = OwnedRevisitReply::Rejected {
                version: OWNED_REVISIT_VERSION,
                request_id,
                code,
            };
            let _ = self.send_owned_revisit_reply(&reply);
        }
        if close_channel {
            self.close_owned_revisit_channel();
        }
    }

    fn close_owned_revisit_channel(&mut self) {
        if let Some(session) = self.owned_revisit.as_mut() {
            session.failed_closed = true;
            let _ = session.writer.shutdown(std::net::Shutdown::Both);
        }
    }

    fn handle_owned_revisit_event(
        &mut self,
        message: Result<OwnedRevisitRequest<OwnedRevisitCompletion>, String>,
    ) {
        let Ok(message) = message else {
            self.fail_owned_revisit(RejectionCode::ProtocolError, true);
            return;
        };
        if self
            .owned_revisit
            .as_ref()
            .is_none_or(|session| session.failed_closed)
        {
            return;
        }
        match message {
            OwnedRevisitRequest::Prepare {
                version: _,
                request_id,
            } => self.prepare_owned_revisit(request_id),
            OwnedRevisitRequest::Finish {
                version: _,
                request_id,
                completion,
                new_device_token,
                new_source_token,
            } => self.finish_owned_revisit(
                request_id,
                *completion,
                new_device_token,
                new_source_token,
            ),
            OwnedRevisitRequest::Abort {
                version: _,
                request_id,
                transition_id,
            } => self.abort_owned_revisit(request_id, transition_id),
        }
    }

    fn prepare_owned_revisit(&mut self, request_id: String) {
        let Some(session) = self.owned_revisit.as_mut() else {
            return;
        };
        if session.pending.is_some()
            || session.active.as_ref().is_some_and(|active| {
                self.engine.owned_revisit_transition_id() == Some(active.transition_id.as_str())
            })
            || !record_request_id(&mut session.seen_request_ids, &request_id)
        {
            let reply = OwnedRevisitReply::Rejected {
                version: OWNED_REVISIT_VERSION,
                request_id,
                code: RejectionCode::ProtocolError,
            };
            let _ = self.send_owned_revisit_reply(&reply);
            self.cancel_pending_owned_revisit("out-of-order private revisit request");
            self.close_owned_revisit_channel();
            return;
        }
        let challenge = match self.engine.begin_owned_revisit(self.clock.now_ms()) {
            Ok(challenge) => challenge,
            Err(error) => {
                let reply = OwnedRevisitReply::Rejected {
                    version: OWNED_REVISIT_VERSION,
                    request_id,
                    code: revisit_error_code(&error),
                };
                let _ = self.send_owned_revisit_reply(&reply);
                return;
            }
        };
        let expected_epoch = self.config.device_epoch.checked_add(1);
        let old_registration_matches = self
            .config
            .owned_document_registration
            .as_ref()
            .is_some_and(|registration| {
                registration.registration_id == challenge.old_registration_id
            });
        if challenge.version != 1
            || !is_canonical_transition_id(&challenge.transition_id)
            || challenge.old_device_session_epoch != self.config.device_epoch
            || expected_epoch != Some(challenge.expected_device_session_epoch)
            || !old_registration_matches
            || challenge.deadline_ms <= self.clock.now_ms()
        {
            let _ = self.engine.cancel_owned_revisit(
                &challenge.transition_id,
                self.clock.now_ms(),
                "host challenge did not match the current private session",
            );
            self.fence_old_owned_session();
            let reply = OwnedRevisitReply::Rejected {
                version: OWNED_REVISIT_VERSION,
                request_id,
                code: RejectionCode::InvalidState,
            };
            let _ = self.send_owned_revisit_reply(&reply);
            self.fail_owned_revisit(RejectionCode::InvalidState, true);
            return;
        }

        self.fence_old_owned_session();
        if let Some(session) = self.owned_revisit.as_mut() {
            session.active = Some(ActiveOwnedRevisit {
                transition_id: challenge.transition_id.clone(),
            });
            session.pending = Some(PendingOwnedRevisit {
                request_id: request_id.clone(),
                challenge: challenge.clone(),
            });
        }
        let reply = OwnedRevisitReply::Prepared {
            version: OWNED_REVISIT_VERSION,
            request_id,
            challenge: Box::new(challenge),
        };
        if self.send_owned_revisit_reply(&reply).is_err() {
            self.fail_owned_revisit(RejectionCode::TransitionFailed, true);
        }
    }

    fn abort_owned_revisit(&mut self, request_id: String, transition_id: String) {
        let request_is_new = self
            .owned_revisit
            .as_mut()
            .is_some_and(|session| record_request_id(&mut session.seen_request_ids, &request_id));
        if !request_is_new {
            let reply = OwnedRevisitReply::Rejected {
                version: OWNED_REVISIT_VERSION,
                request_id,
                code: RejectionCode::ProtocolError,
            };
            let _ = self.send_owned_revisit_reply(&reply);
            self.cancel_pending_owned_revisit("replayed private revisit request");
            self.close_owned_revisit_channel();
            return;
        }
        let pending_match = self
            .owned_revisit
            .as_ref()
            .and_then(|session| session.pending.as_ref())
            .is_some_and(|pending| pending.challenge.transition_id == transition_id);
        let active_match = self
            .owned_revisit
            .as_ref()
            .and_then(|session| session.active.as_ref())
            .is_some_and(|active| {
                active.transition_id == transition_id
                    && self.engine.owned_revisit_transition_id()
                        == Some(active.transition_id.as_str())
            });
        if !pending_match && !active_match {
            let reply = OwnedRevisitReply::Rejected {
                version: OWNED_REVISIT_VERSION,
                request_id,
                code: RejectionCode::ProtocolError,
            };
            let _ = self.send_owned_revisit_reply(&reply);
            self.cancel_pending_owned_revisit("mismatched private revisit abort");
            self.close_owned_revisit_channel();
            return;
        }
        self.cancel_pending_owned_revisit("Main aborted the private revisit transition");
        let reply = OwnedRevisitReply::Aborted {
            version: OWNED_REVISIT_VERSION,
            request_id,
            transition_id,
        };
        if self.send_owned_revisit_reply(&reply).is_err() {
            self.fail_owned_revisit(RejectionCode::TransitionFailed, true);
        }
    }

    fn finish_owned_revisit(
        &mut self,
        request_id: String,
        completion: OwnedRevisitCompletion,
        new_device_token: String,
        new_source_token: String,
    ) {
        let request_is_new = self
            .owned_revisit
            .as_mut()
            .is_some_and(|session| record_request_id(&mut session.seen_request_ids, &request_id));
        if !request_is_new {
            let reply = OwnedRevisitReply::Rejected {
                version: OWNED_REVISIT_VERSION,
                request_id,
                code: RejectionCode::ProtocolError,
            };
            let _ = self.send_owned_revisit_reply(&reply);
            self.cancel_pending_owned_revisit("replayed private revisit request");
            self.close_owned_revisit_channel();
            return;
        }
        let Some(pending) = self
            .owned_revisit
            .as_ref()
            .and_then(|session| session.pending.as_ref())
        else {
            let reply = OwnedRevisitReply::Rejected {
                version: OWNED_REVISIT_VERSION,
                request_id,
                code: RejectionCode::ProtocolError,
            };
            let _ = self.send_owned_revisit_reply(&reply);
            self.close_owned_revisit_channel();
            return;
        };
        let challenge = pending.challenge.clone();
        if completion.transition_id != challenge.transition_id
            || completion.new_device_session_epoch != challenge.expected_device_session_epoch
            || completion.clock_domain != challenge.clock_domain
            || completion.version != 1
            || completion.new_registration.validate_identity().is_err()
            || self
                .config
                .owned_document_registration
                .as_ref()
                .is_none_or(|old| {
                    completion.new_registration.registration_id == old.registration_id
                        || completion.new_registration.process_identity() == old.process_identity()
                })
            || new_device_token == self.config.device_token
            || new_device_token == self.config.source_token
            || new_source_token == self.config.source_token
            || new_source_token == self.config.device_token
            || new_device_token == new_source_token
        {
            self.fail_owned_revisit(RejectionCode::ProtocolError, true);
            return;
        }
        if self.clock.now_ms() >= challenge.deadline_ms {
            self.fail_owned_revisit(RejectionCode::Timeout, true);
            return;
        }

        let epoch = challenge.expected_device_session_epoch;
        let Some(device_id) = self.next_conn.checked_add(1) else {
            self.fail_owned_revisit(RejectionCode::TransitionFailed, true);
            return;
        };
        let staged_device = connect_peer(
            &self.config.device_socket(),
            &new_device_token,
            epoch,
            self.clock,
            device_id,
            OWNED_REVISIT_CONNECT_TIMEOUT,
        );
        let (device_peer, device_ready, device_reader) = match staged_device {
            Ok(connected) => connected,
            Err(_) => {
                self.fail_owned_revisit(RejectionCode::PeerUnavailable, true);
                return;
            }
        };
        let device_manifest = device_ready.capabilities.as_ref();
        if device_ready.role != PeerRole::Device
            || device_manifest.is_none_or(|manifest| {
                manifest.backend != BackendKind::Macos
                    || manifest.input != CapabilityState::Unavailable
                    || manifest.human_input != CapabilityState::Unavailable
                    || manifest.hit_test != CapabilityState::Unavailable
                    || manifest.identity.session_epoch != epoch
                    || manifest.max_uninterruptible_ms != 0
            })
        {
            let _ = device_peer
                .link
                .get_ref()
                .shutdown(std::net::Shutdown::Both);
            self.fail_owned_revisit(RejectionCode::DeviceNotObserveOnly, true);
            return;
        }
        let Some(manifest) = device_ready.capabilities.clone() else {
            self.fail_owned_revisit(RejectionCode::DeviceNotObserveOnly, true);
            return;
        };

        let Some(source_id) = device_id.checked_add(1) else {
            let _ = device_peer
                .link
                .get_ref()
                .shutdown(std::net::Shutdown::Both);
            self.fail_owned_revisit(RejectionCode::TransitionFailed, true);
            return;
        };
        let staged_source = connect_peer(
            &self.config.source_socket(),
            &new_source_token,
            epoch,
            self.clock,
            source_id,
            OWNED_REVISIT_CONNECT_TIMEOUT,
        );
        let (source_peer, _source_ready, source_reader) = match staged_source {
            Ok(connected) if connected.1.role == PeerRole::Source => connected,
            _ => {
                let _ = device_peer
                    .link
                    .get_ref()
                    .shutdown(std::net::Shutdown::Both);
                self.fail_owned_revisit(RejectionCode::PeerUnavailable, true);
                return;
            }
        };

        let device_reader_id = device_peer.id;
        self.next_conn = device_id;
        self.bridge.forget_all();
        self.device = Some(device_peer);
        let outputs = self.engine.handle(
            self.clock.now_ms(),
            Input::DeviceNotice(interaction_engine::execution::DeviceNotice::Manifest(
                Box::new(manifest.clone()),
            )),
        );
        self.carry_all(outputs);
        if self
            .device
            .as_ref()
            .is_none_or(|peer| peer.id != device_reader_id)
        {
            let _ = source_peer
                .link
                .get_ref()
                .shutdown(std::net::Shutdown::Both);
            self.fail_owned_revisit(RejectionCode::PeerUnavailable, true);
            return;
        }

        let registration = completion.new_registration.clone();
        let status = match self
            .engine
            .finish_owned_revisit(completion, self.clock.now_ms())
        {
            Ok(status) => status,
            Err(error) => {
                let _ = source_peer
                    .link
                    .get_ref()
                    .shutdown(std::net::Shutdown::Both);
                self.fail_owned_revisit(revisit_error_code(&error), true);
                return;
            }
        };
        if status != OwnedRevisitStatus::AwaitingSourceProof {
            let _ = source_peer
                .link
                .get_ref()
                .shutdown(std::net::Shutdown::Both);
            self.fail_owned_revisit(RejectionCode::TransitionFailed, true);
            return;
        }

        self.next_conn = source_id;
        self.config.device_epoch = epoch;
        self.config.device_token = new_device_token;
        self.config.source_token = new_source_token;
        self.config.owned_document_registration = Some(Box::new(registration));
        self.config.allow_owned_document_save = false;
        let source_reader_id = source_peer.id;
        self.source = Some(source_peer);
        self.source_refresh.clear();
        self.source_capture.clear();
        spawn_reader(
            "host-device",
            device_reader,
            self.tx.clone(),
            move |event| Event::Device(device_reader_id, event),
        );
        spawn_reader(
            "host-source",
            source_reader,
            self.tx.clone(),
            move |event| Event::Source(source_reader_id, event),
        );
        self.queue.push_back(Input::Tick);

        let reply = OwnedRevisitReply::Finished {
            version: OWNED_REVISIT_VERSION,
            request_id,
            transition_id: challenge.transition_id,
        };
        if self.send_owned_revisit_reply(&reply).is_err() {
            self.fail_owned_revisit(RejectionCode::TransitionFailed, true);
        } else if let Some(session) = self.owned_revisit.as_mut() {
            session.pending = None;
        }
    }
    fn connect_device(&mut self, timeout: Duration) -> Result<(), String> {
        self.next_conn += 1;
        let id = self.next_conn;
        let (peer, ready, reader) = connect_peer(
            &self.config.device_socket(),
            &self.config.device_token,
            self.config.device_epoch,
            self.clock,
            id,
            timeout,
        )?;
        if let Some(manifest) = ready.capabilities.as_ref() {
            manifest.validate()?;
            let selected_background = manifest.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx;
            if selected_background != self.config.native_background_window_scope.is_some() {
                let _ = peer.link.get_ref().shutdown(std::net::Shutdown::Both);
                return Err(
                    "Device execution domain does not match the configured background evidence scope"
                        .into(),
                );
            }
        } else if self.config.native_background_window_scope.is_some() {
            let _ = peer.link.get_ref().shutdown(std::net::Shutdown::Both);
            return Err("background scope requires the Device capability manifest".into());
        }
        spawn_reader("host-device", reader, self.tx.clone(), move |i| {
            Event::Device(id, i)
        });
        self.device = Some(peer);
        self.bridge.forget_all();
        if let Some(manifest) = ready.capabilities {
            self.queue.push_back(Input::DeviceNotice(
                interaction_engine::execution::DeviceNotice::Manifest(Box::new(manifest)),
            ));
        }
        Ok(())
    }

    fn connect_source(&mut self) -> Result<(), String> {
        self.source_refresh.clear();
        self.source_capture.clear();
        self.next_conn += 1;
        let id = self.next_conn;
        let (peer, _ready, reader) = connect_peer(
            &self.config.source_socket(),
            &self.config.source_token,
            self.config.device_epoch,
            self.clock,
            id,
            CONNECT_TIMEOUT,
        )?;
        spawn_reader("host-source", reader, self.tx.clone(), move |i| {
            Event::Source(id, i)
        });
        self.source = Some(peer);
        Ok(())
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Supervisor(Inbound::Frame(envelope)) => match envelope.message {
                Message::BrowserSourceControl(control) => {
                    let accepted = self
                        .engine
                        .control_browser_source(self.clock.now_ms(), control)
                        .is_ok();
                    let ack = interaction_contracts::desktop::device::CommandAck {
                        accepted,
                        code: None,
                        reason: if accepted {
                            None
                        } else {
                            Some("browser source scope rejected".into())
                        },
                    };
                    if self
                        .supervisor
                        .reply(&envelope.message_id, Message::CommandAck(ack))
                        .is_err()
                    {
                        self.exit = true;
                    }
                    self.queue.push_back(Input::Tick);
                }
                Message::BrowserPageSnapshot(snapshot) => {
                    let ack = browser_page_snapshot_ack(
                        self.engine
                            .admit_browser_page(self.clock.now_ms(), snapshot),
                    );
                    if self
                        .supervisor
                        .reply(&envelope.message_id, Message::CommandAck(ack))
                        .is_err()
                    {
                        self.exit = true;
                    }
                    self.queue.push_back(Input::Tick);
                }
                Message::OperatorCommand(command) => {
                    if matches!(command, interaction_contracts::desktop::operator::OperatorCommand::StopTask { .. }
                        | interaction_contracts::desktop::operator::OperatorCommand::PauseTask { .. }) {
                        self.close_computer();
                        self.queue.push_front(Input::Operator(command));
                    } else {
                        if matches!(command, interaction_contracts::desktop::operator::OperatorCommand::SetMode { mode } if mode != interaction_contracts::desktop::operator::RunMode::Execute) {
                            self.close_computer();
                        }
                        self.queue.push_back(Input::Operator(command));
                    }
                }
                Message::DecisionProposal(proposal) => {
                    if self.teacher.is_none()
                        && self.pending_supervisor_decisions.remove(&(
                            proposal.decision_id.as_str().to_string(),
                            proposal.request_generation,
                        ))
                    {
                        self.queue.push_back(Input::Decision(Box::new(proposal)));
                    }
                }
                Message::Configure(configure) => self.reconfigure(&configure),
                Message::Shutdown(_) => self.exit = true,
                other => eprintln!("host: unexpected {} from the supervisor", other.name()),
            },
            Event::Supervisor(Inbound::Closed | Inbound::Broken(_)) => self.exit = true,
            Event::Device(id, Inbound::Frame(envelope)) => {
                if let Message::Receipt(receipt) = &envelope.message
                    && !matches!(
                        receipt.status,
                        interaction_contracts::desktop::device::ReceiptStatus::Accepted
                            | interaction_contracts::desktop::device::ReceiptStatus::Running
                    )
                {
                    eprintln!(
                        "host: input {} {:?} {:?}, calls={}, end={:?}",
                        receipt.intent_id,
                        receipt.status,
                        receipt.reason_code,
                        receipt.submitted.len(),
                        receipt.input_end_ms
                    );
                }
                if self.device.as_ref().is_some_and(|peer| peer.id == id)
                    && let Some(input) = self.bridge.device_input(&envelope)
                {
                    self.queue.push_back(input);
                }
            }
            Event::Device(id, Inbound::Closed | Inbound::Broken(_)) => {
                if self.device.as_ref().is_some_and(|peer| peer.id == id) {
                    self.device_lost("the connection closed");
                }
            }
            Event::Source(id, Inbound::Frame(envelope)) => {
                if self.source.as_ref().is_some_and(|peer| peer.id == id) {
                    if let Some(input) = self.source_refresh.arrived(&envelope) {
                        self.queue.push_back(input);
                    } else if let Some(input) =
                        self.source_capture.arrived(&envelope, self.clock.now_ms())
                    {
                        self.queue.push_back(input);
                    } else if let Some(input) = source_input(envelope.message) {
                        self.queue.push_back(input);
                    }
                }
            }
            Event::Source(id, Inbound::Closed | Inbound::Broken(_)) => {
                if self.source.as_ref().is_some_and(|peer| peer.id == id) {
                    self.close_computer();
                    self.source = None;
                    self.source_refresh.clear();
                    self.source_capture.clear();
                    for source in [
                        SourceKind::Ax,
                        SourceKind::AxObserver,
                        SourceKind::Screenshot,
                        SourceKind::Ocr,
                    ] {
                        self.queue.push_back(Input::SourceHealth(SourceHealth {
                            source,
                            state: SourceState::Unavailable,
                            reason: Some("the source process is gone".into()),
                            at_ms: self.clock.now_ms(),
                            provenance: None,
                        }));
                    }
                }
            }
            Event::OwnedRevisit(message) => self.handle_owned_revisit_event(message),
        }
    }

    fn device_lost(&mut self, reason: &str) {
        self.close_computer();
        self.device = None;
        self.bridge.forget_all();
        self.queue.push_back(Input::DeviceLost {
            reason: reason.to_string(),
        });
    }

    /// The supervisor started a new device and says how to reach it.
    fn reconfigure(&mut self, configure: &Configure) {
        if self.owned_revisit.is_some() {
            // The private planned transition is the sole authority for peer
            // replacement when this process has an owned revisit channel.
            return;
        }
        if configure.role != PeerRole::Host {
            return;
        }
        self.close_computer();
        let settings = &configure.settings;
        let background_scope_update = settings.contains_key("native_background_window_id")
            || settings.contains_key("native_target_window_id")
            || settings.contains_key("native_target_process_start_seconds")
            || settings.contains_key("native_target_process_start_micros");
        if background_scope_update {
            let scope = match crate::config::background_window_scope_from_configure(configure) {
                Ok(scope) => scope,
                Err(error) => {
                    self.exit = true;
                    eprintln!("host: rejected background scope replacement: {error}");
                    return;
                }
            };
            if scope.as_ref().is_some_and(|scope| {
                self.config.native_target_bundle.as_deref() != Some(scope.app.as_str())
                    || self.config.native_window_scope.is_some()
            }) {
                self.exit = true;
                eprintln!(
                    "host: background scope replacement requires a matching app pack and no foreground window scope"
                );
                return;
            }
            if let Err(error) = self
                .engine
                .set_native_background_window_scope(scope.clone())
            {
                self.exit = true;
                eprintln!("host: could not replace the background evidence scope: {error}");
                return;
            }
            self.config.native_background_window_scope = scope;
        }
        if let (Some(token), Some(epoch)) = (
            settings.get("device_token"),
            settings
                .get("device_epoch")
                .and_then(|e| e.parse::<u64>().ok()),
        ) {
            self.config.device_token = token.clone();
            self.config.device_epoch = epoch;
            if let Some(source_token) = settings.get("source_token") {
                self.config.source_token = source_token.clone();
            }
            if let Err(error) = self.connect_device(Duration::from_secs(3)) {
                eprintln!("host: could not reach the new device: {error}");
            }
            // A new session has a new source: the old one reports the old epoch.
            if settings.contains_key("source_token")
                && let Err(error) = self.connect_source()
            {
                eprintln!("host: could not reach the new source: {error}");
            }
        }
    }

    /// Give the engine everything queued, and send out everything it says.
    fn drain(&mut self) {
        while let Some(input) = self.queue.pop_front() {
            if self.exit {
                self.account_closing_computer_input(input);
                continue;
            }
            let now = self.clock.now_ms();
            let outputs = self.engine.handle(now, input);
            self.carry_all(outputs);
        }
        if self.config.owned_document_registration.is_some() && self.owned_diagnostic_count < 8 {
            let binding = self.engine.owned_document_binding();
            let state = (
                binding.map(|binding| binding.source_sequence()),
                binding.is_some(),
            );
            if self.owned_diagnostic_last != Some(state) {
                self.owned_diagnostic_last = Some(state);
                self.owned_diagnostic_count += 1;
                // Fixed, value-only private diagnostics. No registration,
                // paths, object references, contents or credentials escape.
                let readback =
                    binding.and_then(|binding| binding.evidence().value_readback.as_ref());
                let initial_match = readback
                    .zip(self.config.owned_document_registration.as_ref())
                    .map(|(readback, registration)| {
                        readback.sha256 == registration.document_sha256
                    });
                let diagnostic = serde_json::json!({
                    "kind":"owned_document_readonly_scope", "source_sequence":state.0,
                    "binding_current":state.1, "input_grant":false,
                    "value_readback_present":readback.is_some(),
                    "value_readback_utf8_byte_count":readback.map(|value| value.utf8_byte_count),
                    "value_readback_matches_initial_file":initial_match,
                });
                eprintln!("{diagnostic}");
            }
        }
    }

    fn poll_model(&mut self) {
        self.start_pending_model();
        let result = self
            .model_planner
            .as_mut()
            .and_then(|p| p.poll(self.clock.now_ms()));
        let Some(result) = result else {
            return;
        };
        let scope = result.scope().clone();
        let result_key = (scope.request_id.as_str().to_string(), scope.generation);
        if self.inflight_model.as_ref() == Some(&result_key) {
            self.inflight_model = None;
        }
        let suppress_result = self.suppress_model_results.remove(&result_key);
        let outcome = match &result {
            TurnResult::Proposal { .. } => ModelOutcome::Proposal,
            TurnResult::Missing { .. } => ModelOutcome::Missing,
            TurnResult::Conflict { .. } => ModelOutcome::Conflict,
            TurnResult::Invalid { .. } => ModelOutcome::Invalid,
            TurnResult::Quarantined { .. } => ModelOutcome::Quarantined,
            TurnResult::Cancelled { .. } => ModelOutcome::Cancelled,
            TurnResult::Expired { .. } => ModelOutcome::Expired,
        };
        let usage = result.usage();
        let record = ModelCompletion {
            root: scope.root.clone(),
            request: scope.request_id.clone(),
            generation: scope.generation,
            at_ms: self.clock.now_ms(),
            outcome,
            usage: ModelUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cost_micros: usage.cost_micros,
            },
        };
        if self.engine.model_call_finished(&record).is_err() {
            eprintln!("host: model accounting could not be finalized");
            if !suppress_result {
                self.queue.push_back(Input::PlannerFailed {
                    request_id: scope.request_id,
                    generation: scope.generation,
                    failure: PlanningFailure::Unavailable,
                });
            }
            return;
        }
        if suppress_result {
            return;
        }
        // Usage remains available even after cancellation. Only sanitized
        // counts are logged; no arguments, credential or provider error text.
        eprintln!("host: model turn {} {:?}", scope.request_id, result.usage());
        match result {
            TurnResult::Proposal { output, .. } => self.queue.push_back(Input::Planner {
                request_id: scope.request_id,
                generation: scope.generation,
                output,
            }),
            other => {
                let failure = match other {
                    TurnResult::Missing { .. } => PlanningFailure::Missing,
                    TurnResult::Conflict { .. } => PlanningFailure::Conflict,
                    TurnResult::Invalid { .. } => PlanningFailure::Invalid,
                    TurnResult::Quarantined { .. } => PlanningFailure::Quarantined,
                    TurnResult::Cancelled { .. } => PlanningFailure::Cancelled,
                    TurnResult::Expired { .. } => PlanningFailure::Expired,
                    TurnResult::Proposal { .. } => unreachable!(),
                };
                self.queue.push_back(Input::PlannerFailed {
                    request_id: scope.request_id,
                    generation: scope.generation,
                    failure,
                });
            }
        }
    }

    fn poll_teacher(&mut self) {
        let now = self.clock.now_ms();
        let result = self.teacher.as_mut().and_then(|teacher| teacher.poll(now));
        let Some(result) = result else {
            return;
        };
        let scope = match &result {
            TeacherTurnResult::Proposal { scope, .. }
            | TeacherTurnResult::Cancelled { scope, .. }
            | TeacherTurnResult::Expired { scope, .. }
            | TeacherTurnResult::Quarantined { scope, .. } => scope.clone(),
        };
        let Some(request) = self.pending_teacher.take() else {
            return;
        };
        let observe_terminal = !std::mem::take(&mut self.teacher_observation_terminal);
        if request.decision_id.as_str() != scope.request_id.as_str()
            || request.request_generation != scope.generation
            || request.root_task_id != scope.root
        {
            if observe_terminal {
                self.observe_teacher(TeacherObservation::cancelled(
                    &request,
                    TeacherObservationReason::ScopeMismatch,
                ));
            }
            return;
        }
        let (outcome, usage) = match &result {
            TeacherTurnResult::Proposal {
                proposal, usage, ..
            } => (
                if proposal.failure.is_some() {
                    interaction_engine::deliberation::accounting::DecisionCallOutcome::Failed
                } else {
                    interaction_engine::deliberation::accounting::DecisionCallOutcome::Proposal
                },
                *usage,
            ),
            TeacherTurnResult::Cancelled { usage, .. } => (
                interaction_engine::deliberation::accounting::DecisionCallOutcome::Cancelled,
                *usage,
            ),
            TeacherTurnResult::Expired { usage, .. } => (
                interaction_engine::deliberation::accounting::DecisionCallOutcome::Expired,
                *usage,
            ),
            TeacherTurnResult::Quarantined { usage, .. } => (
                interaction_engine::deliberation::accounting::DecisionCallOutcome::Quarantined,
                *usage,
            ),
        };
        if self
            .engine
            .decision_model_call_finished(
                &interaction_engine::deliberation::accounting::DecisionCallCompletion {
                    root: scope.root.clone(),
                    decision: request.decision_id.clone(),
                    generation: scope.generation,
                    at_ms: now,
                    outcome,
                    usage: interaction_engine::deliberation::accounting::ModelUsage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cost_micros: usage.cost_micros,
                    },
                },
            )
            .is_err()
        {
            eprintln!("host: teacher decision accounting could not be finalized");
            if observe_terminal {
                self.observe_teacher(TeacherObservation::cancelled(
                    &request,
                    TeacherObservationReason::AccountingFailed,
                ));
            }
            if matches!(
                &result,
                TeacherTurnResult::Proposal { .. } | TeacherTurnResult::Quarantined { .. }
            ) {
                self.queue
                    .push_back(Input::Decision(Box::new(teacher_failure(
                        request,
                        DecisionFailure::Unavailable,
                    ))));
            }
            return;
        }
        match result {
            TeacherTurnResult::Proposal {
                proposal, usage, ..
            } => {
                eprintln!("host: teacher decision {:?}", usage);
                if observe_terminal && self.teacher_observation.is_some() {
                    self.observe_teacher(TeacherObservation::finished(&request, proposal.clone()));
                }
                self.queue.push_back(Input::Decision(Box::new(proposal)));
            }
            TeacherTurnResult::Quarantined { usage, .. } => {
                eprintln!("host: teacher decision unavailable {:?}", usage);
                if observe_terminal {
                    self.observe_teacher(TeacherObservation::cancelled(
                        &request,
                        TeacherObservationReason::Quarantined,
                    ));
                }
                self.queue
                    .push_back(Input::Decision(Box::new(teacher_failure(
                        request,
                        DecisionFailure::Unavailable,
                    ))));
            }
            TeacherTurnResult::Expired { usage, .. } => {
                eprintln!("host: teacher decision expired {:?}", usage);
                if observe_terminal {
                    self.observe_teacher(TeacherObservation::cancelled(
                        &request,
                        TeacherObservationReason::Expired,
                    ));
                }
            }
            TeacherTurnResult::Cancelled { usage, .. } => {
                eprintln!("host: teacher decision cancelled {:?}", usage);
                if observe_terminal {
                    self.observe_teacher(TeacherObservation::cancelled(
                        &request,
                        TeacherObservationReason::Cancelled,
                    ));
                }
            }
        }
    }

    fn start_teacher_decision(&mut self, request: DecisionRequest) {
        let Some(teacher) = &mut self.teacher else {
            self.pending_supervisor_decisions.insert((
                request.decision_id.as_str().to_string(),
                request.request_generation,
            ));
            self.send_supervisor(Message::DecisionRequest(request));
            return;
        };
        let Some(identity) = self.teacher_identity.clone() else {
            self.queue
                .push_back(Input::Decision(Box::new(teacher_failure(
                    request,
                    DecisionFailure::Unavailable,
                ))));
            return;
        };
        let now = self.clock.now_ms();
        if self
            .engine
            .decision_model_call_started(now, &request, &identity.provider, &identity.model)
            .is_err()
        {
            self.queue
                .push_back(Input::Decision(Box::new(teacher_failure(
                    request,
                    DecisionFailure::Unavailable,
                ))));
            return;
        }
        match teacher.begin(&request, now) {
            Ok(_) => {
                self.teacher_observation_terminal = false;
                if self.teacher_observation.is_some() {
                    self.observe_teacher(TeacherObservation::started(request.clone()));
                }
                self.pending_teacher = Some(request);
            }
            Err(_) => {
                let _ = self.engine.decision_model_call_finished(
                    &interaction_engine::deliberation::accounting::DecisionCallCompletion {
                        root: request.root_task_id.clone(),
                        decision: request.decision_id.clone(),
                        generation: request.request_generation,
                        at_ms: now,
                        outcome: interaction_engine::deliberation::accounting::DecisionCallOutcome::Unavailable,
                        usage: interaction_engine::deliberation::accounting::ModelUsage::default(),
                    },
                );
                self.queue
                    .push_back(Input::Decision(Box::new(teacher_failure(
                        request,
                        DecisionFailure::Unavailable,
                    ))));
            }
        }
    }

    fn start_pending_model(&mut self) {
        if self.engine.owned_revisit_transition_id().is_some() {
            return;
        }
        let Some(planner) = &mut self.model_planner else {
            return;
        };
        if !planner.can_start() {
            return;
        }
        let Some(request) = self.pending_model.take() else {
            return;
        };
        let now = self.clock.now_ms();
        let (provider, model) = planner.identity();
        let recorded = self
            .engine
            .model_call_started(now, &request, &provider, &model)
            .is_ok();
        if recorded && planner.begin(&request, now).is_ok() {
            self.inflight_model =
                Some((request.request_id.as_str().to_string(), request.generation));
            return;
        }
        if recorded {
            let _ = self.engine.model_call_finished(&ModelCompletion {
                root: request.root_task_id.clone(),
                request: request.request_id.clone(),
                generation: request.generation,
                at_ms: now,
                outcome: ModelOutcome::Unavailable,
                usage: ModelUsage::default(),
            });
        }
        self.queue.push_back(Input::PlannerFailed {
            request_id: request.request_id,
            generation: request.generation,
            failure: PlanningFailure::Unavailable,
        });
    }

    fn carry_all(&mut self, outputs: Vec<Output>) {
        // Publish the current scope before a separate FD5 worker can request
        // its pixels. Main still independently checks the exact frame pair.
        let (projections, commands): (Vec<_>, Vec<_>) = outputs
            .into_iter()
            .partition(|output| matches!(output, Output::Ui(_)));
        for output in projections.into_iter().chain(commands) {
            if self.exit {
                break;
            }
            self.carry(output);
        }
    }

    fn send_supervisor(&mut self, message: Message) {
        if self.supervisor.send(message).is_err() {
            // Nobody is listening any more: the host goes with its supervisor.
            self.exit = true;
        }
    }

    fn carry(&mut self, output: Output) {
        match output {
            Output::Computer(command) => {
                if let Some(mut session) = self.computer.take() {
                    let inputs = session.command(&mut self.engine, *command, self.clock.now_ms());
                    self.computer = Some(session);
                    self.queue.extend(inputs);
                }
            }
            Output::Device { corr, command } => {
                let Some(peer) = self.device.as_mut() else {
                    // No device: the engine's own command timeout notices.
                    return;
                };
                let message = command_message((*command).clone());
                match peer.link.send(message) {
                    Ok(id) => self.bridge.sent(&id, corr, &command),
                    Err(_) => self.device_lost("a write failed"),
                }
            }
            Output::BrowserRefresh(request) => {
                self.send_supervisor(Message::BrowserRefreshRequest(*request));
            }
            Output::Source {
                generation,
                command: SourceCommand::RefreshNow { table_scope },
            } => {
                if let Some(peer) = self.source.as_mut() {
                    match peer
                        .link
                        .send(Message::SourceCommand(SourceCommand::RefreshNow {
                            table_scope,
                        })) {
                        Ok(id) => self.source_refresh.sent(id, generation),
                        Err(_) => {
                            self.source = None;
                            self.source_refresh.clear();
                            self.source_capture.clear();
                        }
                    }
                }
            }
            Output::Source {
                command: command @ SourceCommand::CaptureNow { .. },
                ..
            } => {
                let SourceCommand::CaptureNow {
                    request_id,
                    watch_id,
                    deadline_ms,
                    ..
                } = &command
                else {
                    unreachable!()
                };
                let failure = |reason: &str| {
                    Input::Computer(Box::new(
                        interaction_engine::engine::computer::ComputerInput::CaptureRejected {
                            request_id: request_id.clone(),
                            watch_id: watch_id.clone(),
                            reason: reason.into(),
                        },
                    ))
                };
                if !self.source_capture.available(self.clock.now_ms()) {
                    self.queue
                        .push_back(failure("Source capture channel is busy"));
                } else if let Some(peer) = self.source.as_mut() {
                    match peer.link.send(Message::SourceCommand(command.clone())) {
                        Ok(id) => self.source_capture.sent(
                            id,
                            request_id.clone(),
                            watch_id.clone(),
                            *deadline_ms,
                        ),
                        Err(_) => self
                            .queue
                            .push_back(failure("Source capture transport is unavailable")),
                    }
                } else {
                    self.queue
                        .push_back(failure("Source capture peer is unavailable"));
                }
            }
            Output::Source {
                command: command @ SourceCommand::CancelCapture { .. },
                ..
            } => {
                let SourceCommand::CancelCapture {
                    request_id,
                    watch_id,
                } = &command
                else {
                    unreachable!()
                };
                self.source_capture.cancel(request_id, watch_id);
                if let Some(peer) = self.source.as_mut() {
                    let _ = peer.link.send(Message::SourceCommand(command));
                }
            }
            Output::Decision(request) => self.start_teacher_decision(*request),
            Output::CancelDecision(cancel) => {
                if self.teacher.is_some() {
                    let observation = self
                        .pending_teacher
                        .as_ref()
                        .filter(|request| {
                            request.decision_id == cancel.decision_id
                                && request.request_generation == cancel.request_generation
                        })
                        .cloned();
                    if let Some(request) = self.pending_teacher.as_ref()
                        && request.decision_id == cancel.decision_id
                        && request.request_generation == cancel.request_generation
                        && let Some(teacher) = &mut self.teacher
                    {
                        teacher.cancel(
                            cancel.decision_id.as_str(),
                            cancel.request_generation,
                            self.clock.now_ms(),
                        );
                    }
                    if let Some(request) = observation {
                        self.observe_teacher_terminal(
                            &request,
                            TeacherObservationReason::Cancelled,
                        );
                    }
                } else {
                    self.pending_supervisor_decisions.remove(&(
                        cancel.decision_id.as_str().to_string(),
                        cancel.request_generation,
                    ));
                    self.send_supervisor(Message::DecisionCancel(cancel));
                }
            }
            Output::Plan(request) => {
                if self.config.planning_backend == PlanningBackend::Model {
                    if self.pending_model.is_some() {
                        self.queue.push_back(Input::PlannerFailed {
                            request_id: request.request_id.clone(),
                            generation: request.generation,
                            failure: PlanningFailure::Unavailable,
                        });
                    } else {
                        self.pending_model = Some(request);
                    }
                    return;
                }
                let output = match self.config.planning_backend {
                    PlanningBackend::Fixture => self.planner.plan(&request),
                    PlanningBackend::NativeReference => crate::native_reference::plan(&request),
                    PlanningBackend::BrowserReference => {
                        match (
                            self.engine.browser_reference_available(),
                            self.config.browser_page_profile,
                        ) {
                            (true, Some(profile)) => {
                                crate::browser_reference::plan(&request, profile)
                            }
                            _ => {
                                interaction_contracts::desktop::planning::PlannerOutput::Review {
                                    proposal: interaction_contracts::desktop::planning::ReviewProposal {
                                        notes: vec!["browser reference requires the fixed Host profile and current HeadlessBrowser Device capability".into()],
                                        evidence: vec![],
                                    },
                                }
                            }
                        }
                    }
                    PlanningBackend::OwnedReference => crate::owned_reference::plan(
                        &request,
                        self.config.allow_owned_document_save,
                    ),
                    PlanningBackend::Unconfigured => {
                        interaction_contracts::desktop::planning::PlannerOutput::Review {
                            proposal: interaction_contracts::desktop::planning::ReviewProposal {
                                notes: vec!["native planning provider is not configured".into()],
                                evidence: vec![],
                            },
                        }
                    }
                    PlanningBackend::Model => unreachable!(),
                };
                self.queue.push_back(Input::Planner {
                    request_id: request.request_id.clone(),
                    generation: request.generation,
                    output: Box::new(output),
                });
            }
            Output::CancelPlan(request) => {
                if self.pending_model.as_ref().is_some_and(|p| {
                    p.request_id == request.request_id && p.generation == request.generation
                }) {
                    self.pending_model = None;
                }
                if let Some(planner) = &mut self.model_planner {
                    planner.cancel(&request.request_id, request.generation, self.clock.now_ms());
                }
            }
            Output::Ui(ui) => self.send_supervisor(Message::UiProjection(ui)),
        }
    }
}

fn revisit_error_code(error: &OwnedRevisitError) -> RejectionCode {
    match error {
        OwnedRevisitError::Unavailable | OwnedRevisitError::Cancelled => {
            RejectionCode::InvalidState
        }
        OwnedRevisitError::Expired => RejectionCode::Timeout,
        OwnedRevisitError::Invalid(_) => RejectionCode::EngineRejected,
        OwnedRevisitError::Storage(_) => RejectionCode::TransitionFailed,
    }
}

fn record_request_id(seen: &mut std::collections::BTreeSet<String>, request_id: &str) -> bool {
    if seen.len() >= 256 || seen.contains(request_id) {
        return false;
    }
    seen.insert(request_id.to_string())
}

fn browser_page_snapshot_ack(admission: Result<(), BrowserPageAdmissionError>) -> CommandAck {
    match admission {
        Ok(()) => CommandAck {
            accepted: true,
            code: None,
            reason: None,
        },
        Err(BrowserPageAdmissionError::ReadBeforeFloor) => CommandAck {
            accepted: false,
            code: None,
            reason: Some("browser_read_before_floor".into()),
        },
        Err(BrowserPageAdmissionError::Rejected) => CommandAck {
            accepted: false,
            code: None,
            reason: Some("browser page read rejected".into()),
        },
    }
}

#[cfg(test)]
mod browser_page_snapshot_ack_tests {
    use super::*;

    #[test]
    fn pre_floor_page_is_a_typed_negative_ack() {
        let ack = browser_page_snapshot_ack(Err(BrowserPageAdmissionError::ReadBeforeFloor));
        assert!(
            !ack.accepted,
            "the ignored read must not be acknowledged as admitted"
        );
        assert_eq!(ack.code, None);
        assert_eq!(ack.reason.as_deref(), Some("browser_read_before_floor"));
    }

    #[test]
    fn other_page_rejections_keep_the_generic_negative_ack() {
        let ack = browser_page_snapshot_ack(Err(BrowserPageAdmissionError::Rejected));
        assert!(!ack.accepted);
        assert_eq!(ack.reason.as_deref(), Some("browser page read rejected"));
    }

    #[test]
    fn admitted_page_keeps_the_success_ack() {
        let ack = browser_page_snapshot_ack(Ok(()));
        assert!(ack.accepted);
        assert_eq!(ack.reason, None);
    }
}
