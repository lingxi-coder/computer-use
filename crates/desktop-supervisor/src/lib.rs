//! A supervisor for tests (design 3.1 and 4.6).
//!
//! It plays the part Electron main plays: it starts the processes with
//! private pipes, hands each its configuration and one-time credential over
//! the pipe, relays decision requests to a decision worker, and sends stop
//! commands straight to the device. Everything is a real process and a real
//! socket; only the application and the models are simulated.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use desktop_ipc::link::{FramedReader, Inbound, Link, spawn_reader};
use desktop_ipc::{SharedClock, socket};
use interaction_contracts::desktop::decision::DecisionRequest;
use interaction_contracts::desktop::device::{
    DeviceEvent, SimControl, SimOp, StopAck, StopCommand, StopDisposition, StopScope, StopSource,
};
use interaction_contracts::desktop::handshake::{Configure, PeerRole};
use interaction_contracts::desktop::message::Message;
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode, UiProjection};
use interaction_contracts::desktop::task::RootState;
use sim_world::policy::ScriptedTeacher;
use sim_world::world_log::{WorldEntry, WorldLog};
use tempfile::TempDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Device,
    Source,
    Host,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Source => "source",
            Self::Host => "host",
        }
    }
}

pub struct Binaries {
    pub host: PathBuf,
    pub device: PathBuf,
    pub source: PathBuf,
}

/// Build the three binaries once and find them. A test run of one package does
/// not build the binaries of the others, so this asks cargo to, in the same
/// target directory, which costs nothing when they are already current.
pub fn binaries() -> &'static Binaries {
    static BINARIES: OnceLock<Binaries> = OnceLock::new();
    BINARIES.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "-p",
                "desktop-host",
                "-p",
                "device-macos",
                "-p",
                "source-macos",
                "--bins",
            ])
            .status()
            .expect("cargo can be run");
        assert!(status.success(), "the process binaries did not build");
        let exe = std::env::current_exe().expect("the test binary has a path");
        let dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("test binaries live in target/<profile>/deps")
            .to_path_buf();
        Binaries {
            host: dir.join("cu-host"),
            device: dir.join("cu-device"),
            source: dir.join("cu-source"),
        }
    })
}

struct Tagged {
    role: Role,
    generation: u64,
    inbound: Inbound,
}

struct Proc {
    child: Child,
    link: Option<Link<ChildStdin>>,
    pid: u32,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub mode: Option<RunMode>,
}

/// Everything main knows, and everything it can do.
pub struct Session {
    dir: TempDir,
    clock: SharedClock,
    epoch: u64,
    device_token: String,
    source_token: String,
    device: Option<Proc>,
    source: Option<Proc>,
    host: Option<Proc>,
    rx: Receiver<Tagged>,
    tx: Sender<Tagged>,
    next_generation: u64,
    stderr: Arc<Mutex<Vec<String>>>,
    // What has been seen.
    pub ui: Option<UiProjection>,
    pub decisions: Vec<DecisionRequest>,
    pub stop_acks: Vec<StopAck>,
    pub device_events: Vec<DeviceEvent>,
    pub teacher: ScriptedTeacher,
    pub teacher_answers: bool,
    ready: BTreeSet<(Role, u64)>,
    closed: BTreeMap<(Role, u64), String>,
    current: BTreeMap<Role, u64>,
}

fn private_temp() -> TempDir {
    tempfile::Builder::new()
        .prefix("cu")
        .tempdir()
        .expect("a temporary directory")
}

impl Session {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let dir = private_temp();
        socket::private_dir(dir.path()).expect("a private directory");
        Self {
            dir,
            clock: SharedClock::starting_now(),
            epoch: 1,
            device_token: socket::new_token().expect("a token"),
            source_token: socket::new_token().expect("a token"),
            device: None,
            source: None,
            host: None,
            rx,
            tx,
            next_generation: 0,
            stderr: Arc::new(Mutex::new(Vec::new())),
            ui: None,
            decisions: Vec::new(),
            stop_acks: Vec::new(),
            device_events: Vec::new(),
            teacher: ScriptedTeacher::default(),
            teacher_answers: true,
            ready: BTreeSet::new(),
            closed: BTreeMap::new(),
            current: BTreeMap::new(),
        }
    }

    /// Start the device, the source and the host, and wait until each is ready.
    pub fn start(options: &Options) -> Self {
        let mut session = Self::start_device_and_source();
        session.spawn_host(options.mode.unwrap_or(RunMode::Execute));
        session.wait_ready(Role::Host, Duration::from_secs(15));
        session
    }

    /// Only the device and the source, for tests that talk to them directly.
    pub fn start_device_and_source() -> Self {
        let mut session = Self::new();
        session.spawn_device();
        session.spawn_source();
        session.wait_ready(Role::Device, Duration::from_secs(10));
        session.wait_ready(Role::Source, Duration::from_secs(10));
        session
    }

    // ---- Paths and identities ---------------------------------------------------------------

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    pub fn device_socket(&self) -> PathBuf {
        self.dir.path().join("device.sock")
    }

    pub fn clock(&self) -> SharedClock {
        self.clock
    }

    pub fn device_token(&self) -> &str {
        &self.device_token
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn world_log(&self) -> WorldLog {
        WorldLog::new(self.dir.path().join("world.log"))
    }

    pub fn db_path(&self) -> PathBuf {
        self.dir.path().join("host.db")
    }

    // ---- Starting processes ----------------------------------------------------------------------

    fn base_settings(&self) -> BTreeMap<String, String> {
        let mut settings = BTreeMap::new();
        settings.insert("socket_dir".into(), self.dir.path().display().to_string());
        settings.insert(
            "clock_origin_ms".into(),
            self.clock.origin_unix_ms().to_string(),
        );
        settings.insert(
            "world_log".into(),
            self.dir.path().join("world.log").display().to_string(),
        );
        settings
    }

    fn spawn(&mut self, role: Role, program: &Path, configure: Configure) -> Proc {
        self.next_generation += 1;
        let generation = self.next_generation;
        let mut child = Command::new(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("cannot start {}: {error}", program.display()));
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let stderr = child.stderr.take().expect("piped stderr");
        let log = self.stderr.clone();
        let label = role.name();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                log.lock().unwrap().push(format!("{label}: {line}"));
            }
        });
        spawn_reader(
            &format!("supervisor-{}", role.name()),
            FramedReader::new(stdout),
            self.tx.clone(),
            move |inbound| Tagged {
                role,
                generation,
                inbound,
            },
        );
        let mut link = Link::new(stdin, "supervisor", self.clock);
        link.send(Message::Configure(configure))
            .expect("the process takes its configuration");
        self.current.insert(role, generation);
        Proc {
            pid: child.id(),
            child,
            link: Some(link),
        }
    }

    fn spawn_device(&mut self) {
        let mut settings = self.base_settings();
        settings.insert("host_token".into(), self.device_token.clone());
        let configure = Configure {
            role: PeerRole::Device,
            session_epoch: self.epoch,
            settings,
            browser_page_profile: None,
        };
        let proc = self.spawn(Role::Device, &binaries().device, configure);
        self.device = Some(proc);
    }

    fn spawn_source(&mut self) {
        let mut settings = self.base_settings();
        settings.insert("host_token".into(), self.source_token.clone());
        let configure = Configure {
            role: PeerRole::Source,
            session_epoch: self.epoch,
            settings,
            browser_page_profile: None,
        };
        let proc = self.spawn(Role::Source, &binaries().source, configure);
        self.source = Some(proc);
    }

    fn spawn_host(&mut self, mode: RunMode) {
        let mut settings = self.base_settings();
        settings.insert("db_path".into(), self.db_path().display().to_string());
        settings.insert("device_token".into(), self.device_token.clone());
        settings.insert("device_epoch".into(), self.epoch.to_string());
        settings.insert("source_token".into(), self.source_token.clone());
        settings.insert(
            "initial_mode".into(),
            match mode {
                RunMode::Observe => "observe",
                RunMode::Shadow => "shadow",
                RunMode::Execute => "execute",
            }
            .into(),
        );
        let configure = Configure {
            role: PeerRole::Host,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        };
        let proc = self.spawn(Role::Host, &binaries().host, configure);
        self.host = Some(proc);
    }

    // ---- What arrives --------------------------------------------------------------------------------

    fn absorb(&mut self, tagged: Tagged) {
        let Tagged {
            role,
            generation,
            inbound,
        } = tagged;
        match inbound {
            Inbound::Frame(envelope) => match (role, envelope.message) {
                (_, Message::Ready(_)) => {
                    self.ready.insert((role, generation));
                }
                (Role::Host, Message::UiProjection(ui)) => self.ui = Some(*ui),
                (Role::Host, Message::DecisionRequest(request)) => {
                    self.decisions.push(request.clone());
                    if self.teacher_answers {
                        let proposal = self.teacher.decide(&request);
                        self.send_to_host(Message::DecisionProposal(proposal));
                    }
                }
                (Role::Device, Message::StopAck(ack)) => self.stop_acks.push(ack),
                (Role::Device, Message::DeviceEvent(event)) => self.device_events.push(event),
                _ => {}
            },
            Inbound::Closed => {
                self.closed.insert((role, generation), "closed".into());
            }
            Inbound::Broken(reason) => {
                self.closed.insert((role, generation), reason);
            }
        }
    }

    /// Take in whatever arrives for up to `duration`.
    pub fn pump(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match self.rx.recv_timeout(left.min(Duration::from_millis(10))) {
                Ok(tagged) => self.absorb(tagged),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn wait_ready(&mut self, role: Role, timeout: Duration) {
        let generation = self.current[&role];
        let deadline = Instant::now() + timeout;
        while !self.ready.contains(&(role, generation)) {
            assert!(
                Instant::now() < deadline,
                "{} did not become ready\n{}",
                role.name(),
                self.report()
            );
            self.pump(Duration::from_millis(20));
        }
    }

    /// Wait until `done` holds, or fail with what happened.
    pub fn wait_for(&mut self, what: &str, timeout: Duration, done: impl Fn(&Session) -> bool) {
        let deadline = Instant::now() + timeout;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}\n{}",
                self.report()
            );
            self.pump(Duration::from_millis(5));
        }
    }

    // ---- What main can do ------------------------------------------------------------------------------

    fn send_to_host(&mut self, message: Message) {
        if let Some(link) = self.host.as_mut().and_then(|p| p.link.as_mut()) {
            let _ = link.send(message);
        }
    }

    fn send_to_device(&mut self, message: Message) {
        if let Some(link) = self.device.as_mut().and_then(|p| p.link.as_mut()) {
            let _ = link.send(message);
        }
    }

    pub fn operator(&mut self, command: OperatorCommand) {
        self.send_to_host(Message::OperatorCommand(command));
    }

    pub fn create_task(&mut self, text: &str) {
        self.operator(OperatorCommand::CreateTask { text: text.into() });
    }

    pub fn root_state(&self) -> Option<RootState> {
        self.ui
            .as_ref()
            .and_then(|ui| ui.root.as_ref())
            .map(|root| root.state)
    }

    pub fn pending_confirmation(
        &self,
    ) -> Option<interaction_contracts::desktop::operator::ConfirmationRequest> {
        self.ui
            .as_ref()
            .and_then(|ui| ui.pending_confirmation.clone())
    }

    pub fn answer(&mut self, approved: bool) {
        let id = self
            .pending_confirmation()
            .expect("a confirmation is pending")
            .confirmation_id;
        self.operator(OperatorCommand::ConfirmGoal {
            confirmation_id: id,
            approved,
        });
    }

    /// Create a task, wait for its goal, and confirm it.
    pub fn start_task(&mut self, text: &str) {
        self.create_task(text);
        self.wait_for("the goal to be ready", Duration::from_secs(10), |s| {
            s.pending_confirmation().is_some()
        });
        self.answer(true);
    }

    /// A stop as the operator's button sends it: main to the device, directly.
    pub fn stop_at_device(&mut self, scope: StopScope, disposition: StopDisposition) {
        self.send_to_device(Message::StopCommand(StopCommand {
            scope,
            disposition,
            source: StopSource::OperatorButton,
            reason: "the operator pressed stop".into(),
        }));
    }

    pub fn inject_human_input(&mut self) {
        self.send_to_device(Message::SimControl(SimControl {
            op: SimOp::InjectHumanInput,
        }));
    }

    pub fn append_world(&self, entry: WorldEntry) {
        self.world_log()
            .append(&entry)
            .expect("the world log is writable");
    }

    /// The inputs that have reached the application so far.
    pub fn inputs(&self) -> Vec<sim_world::world_log::WorldEntry> {
        self.world_log()
            .entries()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| matches!(entry, WorldEntry::Input { .. }))
            .collect()
    }

    // ---- Things that go wrong --------------------------------------------------------------------------------

    fn signal(pid: u32, name: &str) {
        let _ = Command::new("kill").args([name, &pid.to_string()]).status();
    }

    /// The host stops running, as if its machine stalled. Its sockets stay open.
    pub fn freeze_host(&self) {
        if let Some(host) = &self.host {
            Self::signal(host.pid, "-STOP");
        }
    }

    pub fn thaw_host(&self) {
        if let Some(host) = &self.host {
            Self::signal(host.pid, "-CONT");
        }
    }

    /// The device is killed without a chance to clean up.
    pub fn kill_device(&mut self) {
        if let Some(mut device) = self.device.take() {
            let _ = device.child.kill();
            let _ = device.child.wait();
        }
    }

    /// A new session: a new device at a new epoch and, since the source reports
    /// for one session, a new source. The host is told how to reach both.
    pub fn restart_device(&mut self) {
        self.epoch += 1;
        self.device_token = socket::new_token().expect("a token");
        self.source_token = socket::new_token().expect("a token");
        self.source = None;
        self.spawn_device();
        self.spawn_source();
        self.wait_ready(Role::Device, Duration::from_secs(10));
        self.wait_ready(Role::Source, Duration::from_secs(10));
        let mut settings = BTreeMap::new();
        settings.insert("device_token".into(), self.device_token.clone());
        settings.insert("device_epoch".into(), self.epoch.to_string());
        settings.insert("source_token".into(), self.source_token.clone());
        self.send_to_host(Message::Configure(Configure {
            role: PeerRole::Host,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        }));
    }

    /// Main goes away: every process loses its pipe.
    pub fn close_channels(&mut self) {
        for proc in [&mut self.host, &mut self.source, &mut self.device]
            .into_iter()
            .flatten()
        {
            proc.link = None;
        }
    }

    pub fn has_closed(&self, role: Role) -> bool {
        self.current
            .get(&role)
            .is_some_and(|generation| self.closed.contains_key(&(role, *generation)))
    }

    /// The exit status of a process that has ended, waiting up to `timeout`.
    pub fn wait_exit(&mut self, role: Role, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            let slot = match role {
                Role::Device => self.device.as_mut(),
                Role::Source => self.source.as_mut(),
                Role::Host => self.host.as_mut(),
            }?;
            if let Ok(Some(status)) = slot.child.try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            self.pump(Duration::from_millis(10));
        }
    }

    /// Everything seen, for a failing test's message.
    pub fn report(&self) -> String {
        let mut out = String::new();
        if let Some(ui) = &self.ui {
            out.push_str(&format!(
                "root {:?}, stop {:?}, cleanup {:?}\n",
                ui.root.as_ref().map(|r| r.state),
                ui.stop_status,
                ui.cleanup
            ));
            for entry in &ui.timeline {
                out.push_str(&format!("  [{:>6}] {}\n", entry.at_ms, entry.text));
            }
        } else {
            out.push_str("no projection yet\n");
        }
        out.push_str(&format!(
            "stop acks: {:?}\nworld: {:?}\n",
            self.stop_acks,
            self.world_log().entries().unwrap_or_default()
        ));
        for line in self.stderr.lock().unwrap().iter() {
            out.push_str(&format!("{line}\n"));
        }
        out
    }

    pub fn stderr_lines(&self) -> Vec<String> {
        self.stderr.lock().unwrap().clone()
    }
}
