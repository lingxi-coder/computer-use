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
use interaction_contracts::desktop::handshake::{Configure, PeerRole, Ready};
use interaction_contracts::desktop::message::Message;
use interaction_contracts::desktop::source::{
    SourceCommand, SourceHealth, SourceKind, SourceState,
};
use interaction_engine::engine::{Engine, EngineConfig, Input, Output};
use interaction_engine::environment::pack::AppPack;
use interaction_engine::infrastructure::storage::Storage;
use interaction_engine::tasks::TaskConfig;
use sim_world::policy::ScriptedPlanner;

use crate::bridge::{Bridge, command_message, source_input};
use crate::config::HostConfig;

const TICK: Duration = Duration::from_millis(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

enum Event {
    Supervisor(Inbound),
    Device(u64, Inbound),
    Source(u64, Inbound),
}

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
    clock: SharedClock,
    config: HostConfig,
    tx: Sender<Event>,
    next_conn: u64,
    planner: ScriptedPlanner,
    queue: VecDeque<Input>,
    exit: bool,
}

type Connected = (Peer, Ready, FramedReader<UnixStream>);

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
) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write,
{
    let storage = Storage::open(&config.db_path).map_err(|e| format!("the ledger: {e}"))?;
    let mut engine_config = EngineConfig::new(TaskConfig::new(vec![AppPack::settings_fixture()]));
    engine_config.initial_mode = config.initial_mode;
    let engine = Engine::new(engine_config, storage);

    let (tx, rx) = mpsc::channel::<Event>();
    spawn_reader(
        "host-supervisor",
        supervisor_reader,
        tx.clone(),
        Event::Supervisor,
    );
    let mut shell = Shell {
        engine,
        bridge: Bridge::new(),
        supervisor,
        device: None,
        source: None,
        clock: config.clock,
        config,
        tx,
        next_conn: 0,
        planner: ScriptedPlanner::default(),
        queue: VecDeque::new(),
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
        shell.drain();
    }
    Ok(())
}

impl<W: Write> Shell<W> {
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
                Message::OperatorCommand(command) => {
                    self.queue.push_back(Input::Operator(command));
                }
                Message::DecisionProposal(proposal) => {
                    self.queue.push_back(Input::Decision(Box::new(proposal)));
                }
                Message::Configure(configure) => self.reconfigure(&configure),
                Message::Shutdown(_) => self.exit = true,
                other => eprintln!("host: unexpected {} from the supervisor", other.name()),
            },
            Event::Supervisor(Inbound::Closed | Inbound::Broken(_)) => self.exit = true,
            Event::Device(id, Inbound::Frame(envelope)) => {
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
                if self.source.as_ref().is_some_and(|peer| peer.id == id)
                    && let Some(input) = source_input(envelope.message)
                {
                    self.queue.push_back(input);
                }
            }
            Event::Source(id, Inbound::Closed | Inbound::Broken(_)) => {
                if self.source.as_ref().is_some_and(|peer| peer.id == id) {
                    self.source = None;
                    self.queue.push_back(Input::SourceHealth(SourceHealth {
                        source: SourceKind::Ax,
                        state: SourceState::Unavailable,
                        reason: Some("the source process is gone".into()),
                        at_ms: self.clock.now_ms(),
                    }));
                }
            }
        }
    }

    fn device_lost(&mut self, reason: &str) {
        self.device = None;
        self.bridge.forget_all();
        self.queue.push_back(Input::DeviceLost {
            reason: reason.to_string(),
        });
    }

    /// The supervisor started a new device and says how to reach it.
    fn reconfigure(&mut self, configure: &Configure) {
        if configure.role != PeerRole::Host {
            return;
        }
        let settings = &configure.settings;
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
            let now = self.clock.now_ms();
            let outputs = self.engine.handle(now, input);
            self.carry_all(outputs);
        }
    }

    fn carry_all(&mut self, outputs: Vec<Output>) {
        for output in outputs {
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
            Output::Source(SourceCommand::RefreshNow) => {
                if let Some(peer) = self.source.as_mut()
                    && peer
                        .link
                        .send(Message::SourceCommand(SourceCommand::RefreshNow))
                        .is_err()
                {
                    self.source = None;
                }
            }
            Output::Decision(request) => self.send_supervisor(Message::DecisionRequest(*request)),
            Output::CancelDecision(cancel) => {
                self.send_supervisor(Message::DecisionCancel(cancel));
            }
            Output::Plan(request) => {
                // The planner is scripted in this build, and answers at once.
                let output = self.planner.plan(&request);
                self.queue.push_back(Input::Planner {
                    request_id: request.request_id.clone(),
                    generation: request.generation,
                    output: Box::new(output),
                });
            }
            Output::Ui(ui) => self.send_supervisor(Message::UiProjection(*ui)),
        }
    }
}
