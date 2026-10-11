//! The device process's loop (design 4.6, 14.4 and 14.8).
//!
//! One thread runs everything that touches the core. Reader threads feed it
//! from the supervisor's pipe and from the host's socket, and the loop wakes
//! for the core's own deadlines. The supervisor's stop commands arrive on their
//! own pipe and are applied the moment they are read; nothing in front of them
//! waits for the host.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use desktop_ipc::SharedClock;
use desktop_ipc::handshake::{Acceptance, Connection, spawn_acceptor};
use desktop_ipc::link::{FramedReader, Inbound, Link, QueuedWriter, spawn_reader};
use desktop_ipc::socket;
use execution_core::core::{CoreConfig, CoreEvent, ExecutionCore};
use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, CommandAck, DependencyVersions, DeviceEvent,
    DeviceEventKind, ExecutionDomainCapability, GrantOutcome, SimOp, StopCommand, StopDisposition,
    StopScope, StopSource,
};
use interaction_contracts::desktop::domain::{DomainAction, ExecutionDomain};
use interaction_contracts::desktop::handshake::{PeerRole, Ready};
use interaction_contracts::desktop::message::{Envelope, Message};
use interaction_contracts::desktop::primitive::PrimitivePolicy;

use crate::backend::DeviceBackend;
use crate::config::{DeviceBackendMode, DeviceConfig};
use crate::guard::{GuardClient, GuardSnapshot, OwnedValueStartOptions};

/// The longest the loop sleeps, so a stop or a deadline is never far away.
const MAX_WAIT_MS: u64 = 20;
const OUTPUT_CAPACITY: usize = 64;
const FINAL_OUTPUT_WAIT: Duration = Duration::from_millis(100);
const OWNED_READINESS_DIAGNOSTIC_OFFSETS_MS: [u64; 6] = [0, 500, 1_500, 5_000, 10_000, 15_000];

enum Event {
    Supervisor(Inbound),
    Host(u64, Inbound),
    /// A host that has passed the handshake.
    NewHost(Box<Connection>),
}

#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Exit,
}

struct HostConn {
    id: u64,
    link: Link<QueuedWriter>,
    /// Closing every clone's socket also interrupts a blocked output worker.
    socket: UnixStream,
}

impl Drop for HostConn {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

struct Shell {
    core: ExecutionCore<DeviceBackend>,
    supervisor: Link<QueuedWriter>,
    host: Option<HostConn>,
    clock: SharedClock,
    event_seq: u64,
    next_conn: u64,
    /// For the reader thread of each host connection.
    tx: Sender<Event>,
    native_guard: Option<GuardClient>,
    guard_human_input: CapabilityState,
    guard_previous: Option<GuardSnapshot>,
    owned_readiness_diagnostics: OwnedReadinessDiagnostics,
    background_ax_readiness_diagnostics: BackgroundAxReadinessDiagnostics,
    browser_ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OwnedReadinessSample {
    sample_index: usize,
    accessibility: bool,
    human_input: bool,
    startup_mouse_buttons_clear: bool,
    authority_clock_ready: bool,
    host_alive: bool,
    supervisor_alive: bool,
    backend_native_ready: bool,
    core_isolated: bool,
    ready: bool,
}

impl OwnedReadinessSample {
    fn json_line(self) -> String {
        format!(
            concat!(
                "{{\"kind\":\"owned_guard_readiness\",\"sample_index\":{},",
                "\"accessibility\":{},\"human_input\":{},",
                "\"startup_mouse_buttons_clear\":{},\"authority_clock_ready\":{},",
                "\"host_alive\":{},\"supervisor_alive\":{},",
                "\"backend_native_ready\":{},\"core_isolated\":{},\"ready\":{}}}"
            ),
            self.sample_index,
            self.accessibility,
            self.human_input,
            self.startup_mouse_buttons_clear,
            self.authority_clock_ready,
            self.host_alive,
            self.supervisor_alive,
            self.backend_native_ready,
            self.core_isolated,
            self.ready,
        )
    }
}

struct OwnedReadinessDiagnostics {
    enabled: bool,
    started_at_ms: u64,
    next_sample_index: usize,
}

impl OwnedReadinessDiagnostics {
    fn new(enabled: bool, started_at_ms: u64) -> Self {
        Self {
            enabled,
            started_at_ms,
            next_sample_index: 0,
        }
    }

    fn sample_if_due(
        &mut self,
        now_ms: u64,
        snapshot: &GuardSnapshot,
        backend_native_ready: bool,
        core_isolated: bool,
    ) -> Option<OwnedReadinessSample> {
        if !self.enabled {
            return None;
        }
        let offset_ms = *OWNED_READINESS_DIAGNOSTIC_OFFSETS_MS.get(self.next_sample_index)?;
        if now_ms.saturating_sub(self.started_at_ms) < offset_ms {
            return None;
        }

        let sample_index = self.next_sample_index;
        self.next_sample_index += 1;
        Some(OwnedReadinessSample {
            sample_index,
            accessibility: snapshot.accessibility,
            human_input: snapshot.human_input,
            startup_mouse_buttons_clear: snapshot.startup_mouse_buttons_clear,
            authority_clock_ready: snapshot.authority_clock_ready,
            host_alive: snapshot.host_alive,
            supervisor_alive: snapshot.supervisor_alive,
            backend_native_ready,
            core_isolated,
            ready: backend_native_ready && !core_isolated,
        })
    }
}

struct BackgroundAxReadinessDiagnostics {
    enabled: bool,
    started_at_ms: u64,
    next_sample_index: usize,
}

impl BackgroundAxReadinessDiagnostics {
    fn new(enabled: bool, started_at_ms: u64) -> Self {
        Self {
            enabled,
            started_at_ms,
            next_sample_index: 0,
        }
    }

    fn sample_if_due(&mut self, now_ms: u64) -> Option<usize> {
        if !self.enabled {
            return None;
        }
        let offset_ms = *OWNED_READINESS_DIAGNOSTIC_OFFSETS_MS.get(self.next_sample_index)?;
        if now_ms.saturating_sub(self.started_at_ms) < offset_ms {
            return None;
        }
        let sample_index = self.next_sample_index;
        self.next_sample_index += 1;
        Some(sample_index)
    }
}

fn owned_readiness_diagnostics_enabled(mode: DeviceBackendMode, setting: Option<&str>) -> bool {
    mode == DeviceBackendMode::MacosOwnedValue && setting == Some("1")
}

fn write_owned_readiness_sample(sample: OwnedReadinessSample) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{}", sample.json_line());
}

/// What this device can really do right now.
fn domain_capabilities(
    selected_domain: ExecutionDomain,
    state: CapabilityState,
    actions: &[DomainAction],
) -> Vec<ExecutionDomainCapability> {
    ExecutionDomain::ALL
        .into_iter()
        .map(|domain| {
            if domain == selected_domain && state == CapabilityState::Available {
                ExecutionDomainCapability {
                    domain,
                    state: CapabilityState::Available,
                    actions: actions.to_vec(),
                }
            } else {
                ExecutionDomainCapability::unavailable(domain)
            }
        })
        .collect()
}

pub fn manifest(core: &ExecutionCore<DeviceBackend>) -> CapabilityManifest {
    let mode = core.backend().mode();
    let (backend, execution_domain, state, hit_test, max_uninterruptible_ms, actions) = match mode {
        DeviceBackendMode::Simulated => (
            BackendKind::Simulated,
            ExecutionDomain::Simulated,
            CapabilityState::Available,
            CapabilityState::Available,
            100,
            vec![DomainAction::SimulatedAction],
        ),
        DeviceBackendMode::MacosObserve => (
            BackendKind::Macos,
            ExecutionDomain::ForegroundNative,
            CapabilityState::Unavailable,
            CapabilityState::Unavailable,
            0,
            vec![],
        ),
        DeviceBackendMode::MacosAxPress => (
            BackendKind::Macos,
            ExecutionDomain::ForegroundNative,
            if core.backend().native_ready() && !core.is_isolated() {
                CapabilityState::Available
            } else {
                CapabilityState::Unavailable
            },
            if core.backend().native_ready() && !core.is_isolated() {
                CapabilityState::Available
            } else {
                CapabilityState::Unavailable
            },
            500,
            vec![
                DomainAction::NativePointer,
                DomainAction::NativeKeyboard,
                DomainAction::NativeScroll,
                DomainAction::WindowActivation,
                DomainAction::ForegroundAx,
            ],
        ),
        DeviceBackendMode::MacosBackgroundAx => (
            BackendKind::Macos,
            ExecutionDomain::BackgroundAx,
            if core.backend().native_ready() && !core.is_isolated() {
                CapabilityState::Available
            } else {
                CapabilityState::Unavailable
            },
            CapabilityState::Unavailable,
            500,
            vec![DomainAction::BackgroundAxPress],
        ),
        DeviceBackendMode::MacosOwnedValue => (
            BackendKind::Macos,
            ExecutionDomain::ForegroundNative,
            if core.backend().native_ready() && !core.is_isolated() {
                CapabilityState::Available
            } else {
                CapabilityState::Unavailable
            },
            CapabilityState::Unavailable,
            500,
            vec![DomainAction::ForegroundAx],
        ),
        DeviceBackendMode::HeadlessBrowser => (
            BackendKind::HeadlessBrowser,
            ExecutionDomain::HeadlessBrowser,
            if core.backend().browser_ready() && !core.is_isolated() {
                CapabilityState::Available
            } else {
                CapabilityState::Unavailable
            },
            CapabilityState::Unavailable,
            500,
            vec![DomainAction::HeadlessBrowserAction],
        ),
    };
    let domains = domain_capabilities(execution_domain, state, &actions);
    CapabilityManifest {
        identity: core.identity(),
        backend,
        execution_domain,
        policy: PrimitivePolicy::AtomicOnly,
        human_input: if matches!(
            mode,
            DeviceBackendMode::MacosBackgroundAx | DeviceBackendMode::HeadlessBrowser
        ) {
            CapabilityState::Unavailable
        } else {
            state
        },
        hit_test,
        input: state,
        domains,
        max_uninterruptible_ms,
    }
}

fn core_config(mode: DeviceBackendMode, owned_save_enabled: bool) -> CoreConfig {
    match mode {
        DeviceBackendMode::Simulated => {
            let mut config = CoreConfig::production("sim");
            config.execution_domain = ExecutionDomain::Simulated;
            config
        }
        DeviceBackendMode::MacosObserve | DeviceBackendMode::MacosAxPress => {
            let mut config = CoreConfig::production("macos");
            config.clock_domain_kind = "wall".into();
            config
        }
        DeviceBackendMode::MacosBackgroundAx => {
            let mut config = CoreConfig::background_ax("macos");
            config.clock_domain_kind = "wall".into();
            config
        }
        DeviceBackendMode::MacosOwnedValue => {
            let mut config = if owned_save_enabled {
                CoreConfig::owned_document_actions("macos")
            } else {
                CoreConfig::owned_document_setter("macos")
            };
            config.clock_domain_kind = "wall".into();
            config.dependencies = ["authorization", "focus", "window", "owned_scope"]
                .map(String::from)
                .to_vec();
            config.required_dependencies = config.dependencies.clone();
            config.geometry_dependency = "owned_scope".into();
            config.require_hit_test = false;
            config
        }
        DeviceBackendMode::HeadlessBrowser => {
            unreachable!("headless browser Core config requires its trusted scope")
        }
    }
}

fn core_config_for_device(config: &DeviceConfig) -> Result<CoreConfig, String> {
    if config.backend == DeviceBackendMode::HeadlessBrowser {
        let scope = config
            .browser_scope
            .as_deref()
            .ok_or("headless browser Core config requires the private bootstrap scope")?;
        return CoreConfig::headless_browser(scope.clone());
    }
    Ok(core_config(config.backend, config.owned_save_enabled))
}

fn ack(result: Result<(), String>) -> Message {
    Message::CommandAck(match result {
        Ok(()) => CommandAck::ok(),
        Err(reason) => CommandAck::rejected(None, reason),
    })
}

/// Run the device until the supervisor is gone or says to stop.
pub fn run<R, W>(
    mut config: DeviceConfig,
    supervisor: Link<W>,
    supervisor_reader: FramedReader<R>,
) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let shell_started_at_ms = config.clock.now_ms();
    let readiness_diagnostic_setting = if config.backend == DeviceBackendMode::MacosOwnedValue {
        std::env::var("CU_OWNED_READINESS_DIAGNOSTICS").ok()
    } else {
        None
    };
    let owned_readiness_diagnostics_enabled = owned_readiness_diagnostics_enabled(
        config.backend,
        readiness_diagnostic_setting.as_deref(),
    );
    socket::private_dir(&config.socket_dir).map_err(|e| format!("socket directory: {e}"))?;
    let listener = socket::listen(&config.socket_path()).map_err(|e| format!("listening: {e}"))?;
    let core_configuration = core_config_for_device(&config)?;
    let backend = DeviceBackend::new(&mut config)?;
    let mut core = ExecutionCore::new(core_configuration, backend);
    if config.backend == DeviceBackendMode::HeadlessBrowser {
        if core.session_epoch() != config.session_epoch {
            return Err("browser Core epoch differs from its private bootstrap".into());
        }
    } else {
        // Establish the actual identity before bootstrapping the native clock.
        for _ in 1..config.session_epoch {
            core.reconnect();
        }
    }
    let native_guard = if !matches!(
        config.backend,
        DeviceBackendMode::Simulated | DeviceBackendMode::HeadlessBrowser
    ) {
        config.native_guard_path.as_ref().and_then(|path| {
            let marker = i64::from_str_radix(config.host_token.get(..15).unwrap_or("1"), 16)
                .unwrap_or(1)
                .max(1);
            let result = match &config.native_target {
                Some((pid, bundle)) if config.backend == DeviceBackendMode::MacosOwnedValue => {
                    let registration = config.owned_document_registration.as_deref()?;
                    if *pid != registration.pid || bundle != "com.apple.TextEdit" {
                        return None;
                    }
                    GuardClient::start_owned_execution(
                        path,
                        &config.socket_dir,
                        &core.identity(),
                        config.clock,
                        marker,
                        registration,
                        OwnedValueStartOptions {
                            dependency_validity: core.dependency_validity(),
                            allow_save: config.owned_save_enabled,
                        },
                    )
                }
                Some((pid, bundle))
                    if config.backend == DeviceBackendMode::MacosBackgroundAx
                        && config.native_target_process.is_some() =>
                {
                    GuardClient::start_background_ax_execution(
                        path,
                        &core.identity(),
                        config.clock,
                        marker,
                        *pid,
                        bundle,
                        config
                            .native_target_window
                            .as_deref()
                            .expect("validated BG window"),
                        config
                            .native_target_process
                            .as_ref()
                            .expect("active BG process birth"),
                    )
                }
                Some(_) if config.backend == DeviceBackendMode::MacosBackgroundAx => {
                    // A background observation without a consented birth pair
                    // remains a passive guard session with no target CLI args.
                    GuardClient::start(path, &core.identity(), config.clock, marker)
                }
                Some((pid, bundle)) => GuardClient::start_execution(
                    path,
                    &core.identity(),
                    config.clock,
                    marker,
                    *pid,
                    bundle,
                    config.native_target_window.as_deref(),
                    config.native_target_process.as_ref(),
                ),
                None => GuardClient::start(path, &core.identity(), config.clock, marker),
            };
            match result {
                Ok(guard) => Some(guard),
                Err(reason) => {
                    eprintln!("device: {reason}");
                    None
                }
            }
        })
    } else {
        None
    };
    let active_background_ax = config.backend == DeviceBackendMode::MacosBackgroundAx
        && config.native_target_process.is_some();
    let native_guard = if matches!(
        config.backend,
        DeviceBackendMode::MacosAxPress | DeviceBackendMode::MacosOwnedValue
    ) || active_background_ax
    {
        let guard = native_guard.ok_or("native execution guard could not start")?;
        if active_background_ax {
            core.backend_mut()
                .background_ax_mut()
                .expect("active BG backend")
                .attach(guard)?;
        } else {
            core.backend_mut().native_mut().unwrap().attach(guard)?;
        }
        None
    } else {
        native_guard
    };
    let background_ax_readiness_enabled = core
        .backend_mut()
        .background_ax_mut()
        .and_then(|backend| backend.guard_mut())
        .is_some_and(|guard| guard.background_ax_readiness_diagnostics_enabled());
    let browser_ready = core.backend().browser_ready();

    let (tx, rx) = mpsc::channel();
    spawn_reader(
        "device-supervisor",
        supervisor_reader,
        tx.clone(),
        Event::Supervisor,
    );
    spawn_acceptor(
        "device-accept",
        listener,
        tx.clone(),
        config.clock,
        Acceptance {
            producer: "device".into(),
            role: PeerRole::Host,
            token: config.host_token.clone(),
            session_epoch: core.session_epoch(),
        },
        Event::NewHost,
    );

    let mut shell = Shell {
        core,
        supervisor: supervisor.queued("device-supervisor-output", OUTPUT_CAPACITY),
        host: None,
        clock: config.clock,
        event_seq: 0,
        next_conn: 0,
        tx,
        native_guard,
        guard_human_input: CapabilityState::Unavailable,
        guard_previous: None,
        owned_readiness_diagnostics: OwnedReadinessDiagnostics::new(
            owned_readiness_diagnostics_enabled,
            shell_started_at_ms,
        ),
        background_ax_readiness_diagnostics: BackgroundAxReadinessDiagnostics::new(
            background_ax_readiness_enabled,
            shell_started_at_ms,
        ),
        browser_ready,
    };
    let ready = Message::Ready(Ready {
        role: PeerRole::Device,
        capabilities: Some(shell.manifest()),
        notes: vec![match config.backend {
            DeviceBackendMode::Simulated => "simulated backend".into(),
            DeviceBackendMode::MacosObserve => {
                "macOS observation session; input unavailable".into()
            }
            DeviceBackendMode::MacosAxPress => {
                "macOS fixed-target AXPress backend; other primitives unavailable".into()
            }
            DeviceBackendMode::MacosBackgroundAx if config.native_target_process.is_some() => {
                "Main-authorized fixed-window BackgroundAxPress backend".into()
            }
            DeviceBackendMode::MacosBackgroundAx => {
                "passive BackgroundAx observation; input unavailable without a birth-bound grant"
                    .into()
            }
            DeviceBackendMode::MacosOwnedValue => {
                "Main-authorized owned plain-text value setter for the registered TextEdit document"
                    .into()
            }
            DeviceBackendMode::HeadlessBrowser => {
                "Main-authenticated fixed browser fixture checkbox backend".into()
            }
        }],
    });
    shell
        .supervisor
        .send(ready)
        .map_err(|e| format!("the supervisor pipe: {e}"))?;
    shell.event_loop(&rx);
    Ok(())
}

impl Shell {
    fn manifest(&self) -> CapabilityManifest {
        let mut value = manifest(&self.core);
        if !matches!(
            self.core.backend().mode(),
            DeviceBackendMode::Simulated
                | DeviceBackendMode::MacosBackgroundAx
                | DeviceBackendMode::HeadlessBrowser
        ) {
            value.human_input = self.guard_human_input;
        }
        value
    }

    fn guard_stop(&mut self) {
        if let Some(guard) = self.guard_mut() {
            guard.stop();
        }
    }

    fn guard_mut(&mut self) -> Option<&mut GuardClient> {
        if matches!(
            self.core.backend().mode(),
            DeviceBackendMode::MacosAxPress | DeviceBackendMode::MacosOwnedValue
        ) {
            self.core.backend_mut().native_mut()?.guard_mut()
        } else if self.core.backend().mode() == DeviceBackendMode::MacosBackgroundAx {
            if let Some(backend) = self.core.backend_mut().background_ax_mut() {
                backend.guard_mut()
            } else {
                self.native_guard.as_mut()
            }
        } else {
            self.native_guard.as_mut()
        }
    }

    fn poll_guard(&mut self) {
        let Some(result) = self.guard_mut().map(|guard| guard.poll()) else {
            return;
        };
        match result {
            Ok(Some(record)) => {
                let was_ready = self.core.backend().native_ready();
                if let Some(backend) = self.core.backend_mut().native_mut() {
                    backend.snapshot(&record);
                }
                if let Some(backend) = self.core.backend_mut().background_ax_mut() {
                    backend.snapshot(&record);
                }
                let state = if record.human_input {
                    CapabilityState::Available
                } else {
                    CapabilityState::Unavailable
                };
                if state != self.guard_human_input
                    || was_ready != self.core.backend().native_ready()
                {
                    self.guard_human_input = state;
                    self.device_event(
                        DeviceEventKind::CapabilityChanged {
                            manifest: self.manifest(),
                        },
                        true,
                    );
                }
                let previous_record = self.guard_previous.clone();
                let previous = previous_record.as_ref();
                if record.hotkey_sequence > previous.map_or(0, |r| r.hotkey_sequence) {
                    self.core.stop(&StopCommand {
                        scope: StopScope::All,
                        disposition: StopDisposition::Cancel,
                        source: StopSource::GlobalHotkey,
                        reason: "native event tap stop hotkey".into(),
                    });
                }
                if record.human_sequence > previous.map_or(0, |r| r.human_sequence)
                    && matches!(
                        self.core.backend().mode(),
                        DeviceBackendMode::MacosAxPress
                            | DeviceBackendMode::MacosBackgroundAx
                            | DeviceBackendMode::MacosOwnedValue
                    )
                {
                    eprintln!("native human input: {}", record.reason());
                    self.core.human_input_detected();
                    self.device_event(DeviceEventKind::HumanInput, false);
                }
                if record.lease_loss_sequence > previous.map_or(0, |r| r.lease_loss_sequence)
                    || record.fault_sequence > previous.map_or(0, |r| r.fault_sequence)
                {
                    self.core.stop(&StopCommand {
                        scope: StopScope::All,
                        disposition: StopDisposition::Pause,
                        source: StopSource::DeviceLocal,
                        reason: "native guard lease or permission lost".into(),
                    });
                }
                let sample = self.owned_readiness_diagnostics.sample_if_due(
                    self.clock.now_ms(),
                    &record,
                    self.core.backend().native_ready(),
                    self.core.is_isolated(),
                );
                if let Some(sample) = sample {
                    write_owned_readiness_sample(sample);
                }
                let backend_native_ready = self.core.backend().native_ready();
                let core_isolated = self.core.is_isolated();
                if let Some(sample_index) = self
                    .background_ax_readiness_diagnostics
                    .sample_if_due(self.clock.now_ms())
                    && let Some(guard) = self.guard_mut()
                {
                    guard.record_background_ax_readiness(
                        sample_index,
                        &record,
                        backend_native_ready,
                        core_isolated,
                    );
                }
                self.guard_previous = Some(record);
            }
            Ok(None) => {}
            Err(reason) => {
                self.native_guard = None;
                if let Some(backend) = self.core.backend_mut().native_mut() {
                    backend.disable();
                }
                if let Some(backend) = self.core.backend_mut().background_ax_mut() {
                    backend.disable();
                }
                self.guard_human_input = CapabilityState::Unavailable;
                self.device_event(
                    DeviceEventKind::CapabilityChanged {
                        manifest: self.manifest(),
                    },
                    true,
                );
                self.core.stop(&StopCommand {
                    scope: StopScope::All,
                    disposition: StopDisposition::Pause,
                    source: StopSource::DeviceLocal,
                    reason,
                });
            }
        }
    }

    fn event_loop(&mut self, rx: &Receiver<Event>) {
        loop {
            if self.supervisor.get_ref().failed() {
                break;
            }
            if self
                .host
                .as_ref()
                .is_some_and(|host| host.link.get_ref().failed())
            {
                self.host_lost();
            }
            self.tick();
            let now = self.clock.now_ms();
            let wait = self.core.next_wakeup().map_or(MAX_WAIT_MS, |at| {
                at.saturating_sub(now).clamp(1, MAX_WAIT_MS)
            });
            match rx.recv_timeout(Duration::from_millis(wait)) {
                Ok(event) => {
                    if self.handle(event) == Flow::Exit {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        // Whatever ends the loop, the gate is closed and inputs are released.
        self.close_gate("the device is shutting down");
        // Output is independent of the core even while shutting down. Give a
        // responsive supervisor a bounded chance to receive the final stop.
        self.supervisor.get_mut().finish(FINAL_OUTPUT_WAIT);
    }

    fn close_gate(&mut self, reason: &str) {
        self.guard_stop();
        self.core.stop(&StopCommand {
            scope: StopScope::All,
            disposition: StopDisposition::Cancel,
            source: StopSource::Supervisor,
            reason: reason.into(),
        });
        self.forward_events();
    }

    fn tick(&mut self) {
        self.poll_guard();
        let now = self.clock.now_ms();
        // A clock that stepped back changes nothing: the core never goes back.
        let _ = self.core.advance_to(now);
        self.core.backend_mut().catch_up();
        self.core.dispatch_next();
        let browser_ready = self.core.backend().browser_ready();
        if browser_ready != self.browser_ready {
            self.browser_ready = browser_ready;
            self.device_event(
                DeviceEventKind::CapabilityChanged {
                    manifest: self.manifest(),
                },
                true,
            );
        }
        self.forward_events();
    }

    fn handle(&mut self, event: Event) -> Flow {
        match event {
            Event::Supervisor(Inbound::Frame(envelope)) => self.supervisor_message(*envelope),
            Event::Supervisor(Inbound::Closed | Inbound::Broken(_)) => {
                // The supervisor's lease is gone: the gate closes on its own.
                self.close_gate("the supervisor channel closed");
                Flow::Exit
            }
            Event::NewHost(conn) => {
                self.accept_host(*conn);
                Flow::Continue
            }
            Event::Host(id, Inbound::Frame(envelope)) => {
                if self.is_host(id) {
                    self.host_message(*envelope);
                }
                Flow::Continue
            }
            Event::Host(id, Inbound::Closed | Inbound::Broken(_)) => {
                if self.is_host(id) {
                    self.host_lost();
                }
                Flow::Continue
            }
        }
    }

    fn is_host(&self, id: u64) -> bool {
        self.host.as_ref().is_some_and(|host| host.id == id)
    }

    /// A host passed the handshake. It replaces any earlier one.
    fn accept_host(&mut self, conn: Connection) {
        let Connection { reader, link } = conn;
        let Ok(socket) = link.get_ref().try_clone() else {
            return;
        };
        let mut link = link.queued("device-host-output", OUTPUT_CAPACITY);
        let ready = Message::Ready(Ready {
            role: PeerRole::Device,
            capabilities: Some(self.manifest()),
            notes: vec![],
        });
        if link.send(ready).is_err() {
            return;
        }
        self.next_conn += 1;
        let id = self.next_conn;
        spawn_reader("device-host", reader, self.tx.clone(), move |inbound| {
            Event::Host(id, inbound)
        });
        self.host = Some(HostConn { id, link, socket });
    }

    /// The host is gone. Whatever it was doing stops until a host says otherwise.
    fn host_lost(&mut self) {
        self.guard_stop();
        self.host = None;
        self.core.stop(&StopCommand {
            scope: StopScope::All,
            disposition: StopDisposition::Pause,
            source: StopSource::HostLeaseLost,
            reason: "the host connection closed".into(),
        });
        self.forward_events();
    }

    fn send_host(&mut self, message: Message) {
        let Some(host) = self.host.as_mut() else {
            return;
        };
        if host.link.send(message).is_err() {
            self.host_lost();
        }
    }

    fn reply_host(&mut self, to: &Envelope, message: Message) {
        let Some(host) = self.host.as_mut() else {
            return;
        };
        if host.link.reply(&to.message_id, message).is_err() {
            self.host_lost();
        }
    }

    fn send_supervisor(&mut self, message: Message) {
        // A supervisor that cannot be written to is gone; the reader notices.
        let _ = self.supervisor.send(message);
    }

    fn device_event(&mut self, event: DeviceEventKind, to_supervisor: bool) {
        self.event_seq += 1;
        let message = Message::DeviceEvent(DeviceEvent {
            seq: self.event_seq,
            at_ms: self.clock.now_ms(),
            event,
        });
        if to_supervisor {
            self.send_supervisor(message.clone());
        }
        self.send_host(message);
    }

    /// Tell the host, and the supervisor where it matters, what the core reported.
    fn forward_events(&mut self) {
        for event in self.core.drain_events() {
            match event {
                CoreEvent::ReceiptChanged(receipt) => self.send_host(Message::Receipt(receipt)),
                CoreEvent::PermitRevoked { permit, reason } => {
                    if let Some(guard) = &mut self.native_guard {
                        guard.revoke(&permit);
                    }
                    self.device_event(DeviceEventKind::PermitRevoked { permit, reason }, false);
                }
                CoreEvent::Isolated { reason } => {
                    self.device_event(DeviceEventKind::ChannelIsolated { reason }, true);
                    self.device_event(
                        DeviceEventKind::CapabilityChanged {
                            manifest: self.manifest(),
                        },
                        true,
                    );
                }
                CoreEvent::Recovered => self.device_event(DeviceEventKind::ChannelRecovered, false),
                CoreEvent::Stopped(ack) => {
                    // Main and the host both hear about every stop.
                    self.send_supervisor(Message::StopAck(ack.clone()));
                    self.device_event(DeviceEventKind::Stopped { ack: ack.clone() }, false);
                }
                CoreEvent::Resumed { scope } => {
                    self.device_event(DeviceEventKind::Resumed { scope }, false);
                }
                CoreEvent::LeaseLost => self.device_event(DeviceEventKind::LeaseLost, true),
            }
        }
    }

    // ---- What the host asks -------------------------------------------------------------

    fn host_message(&mut self, envelope: Envelope) {
        // The core's time is now, not the time of the last wake-up: evidence
        // observed a moment ago must not look like it is from the future.
        self.tick();
        let reply = match envelope.message.clone() {
            Message::RegisterRoot(grant) => Some(ack(self.core.register_root(&grant))),
            Message::Observation(observation) => Some(ack(self.core.observe(observation))),
            Message::GrantPermit(request) => Some(Message::GrantOutcome(
                match self.core.grant_permit(&request) {
                    Ok(permit) => GrantOutcome::Granted { permit },
                    Err(denial) => GrantOutcome::Denied {
                        code: denial.code,
                        reason: denial.reason,
                    },
                },
            )),
            Message::SubmitIntent(request) => {
                let receipt = self.core.submit(request);
                self.core.dispatch_next();
                Some(Message::Receipt(receipt))
            }
            Message::QueryReceipt(query) => Some(
                match self
                    .core
                    .query_receipt(query.session_epoch, &query.intent_id)
                {
                    Some(receipt) => Message::Receipt(receipt),
                    None => Message::CommandAck(CommandAck::rejected(
                        None,
                        "the device has no record of that intent",
                    )),
                },
            ),
            Message::RenewPermit(request) => Some(ack(self.core.renew(request))),
            Message::RevokeExecution(request) => Some(ack(self.core.revoke(request))),
            Message::DependencyUpdate(update) => {
                let revision = self.core.update_dependency(&update.name, update.valid);
                if self.core.backend().mode() == DeviceBackendMode::MacosOwnedValue
                    && let Some(guard) = self.guard_mut()
                {
                    guard.update_dependency(&update.name, revision, update.valid);
                }
                Some(Message::DependencyVersions(DependencyVersions {
                    versions: self.core.dependency_versions(),
                }))
            }
            Message::StopCommand(stop) => {
                self.guard_stop();
                Some(Message::StopAck(self.core.stop(&stop)))
            }
            Message::ResumeCommand(resume) => Some(Message::ResumeAck(self.core.resume(&resume))),
            Message::LeaseRenew(lease) => {
                if lease.lease_epoch != self.core.session_epoch() {
                    self.reply_host(
                        &envelope,
                        ack(Err("lease epoch does not match the device session".into())),
                    );
                    return;
                }
                if let Some(guard) = self.guard_mut() {
                    guard.renew("host", lease.valid_for_ms);
                }
                self.core.renew_lease(lease.valid_for_ms);
                Some(Message::CommandAck(CommandAck::ok()))
            }
            other => {
                eprintln!("device: unexpected {} from the host", other.name());
                None
            }
        };
        if let Some(reply) = reply {
            self.reply_host(&envelope, reply);
        }
        self.core.dispatch_next();
        self.forward_events();
    }

    // ---- What the supervisor says ----------------------------------------------------------

    fn supervisor_message(&mut self, envelope: Envelope) -> Flow {
        self.tick();
        match envelope.message.clone() {
            // The operator's stop reaches the device directly. The acknowledgement
            // goes to main and to the host with the core's own report.
            Message::StopCommand(stop) => {
                self.guard_stop();
                self.core.stop(&stop);
            }
            Message::LeaseRenew(lease) => {
                if lease.lease_epoch == self.core.session_epoch()
                    && let Some(guard) = self.guard_mut()
                {
                    guard.renew("supervisor", lease.valid_for_ms);
                }
            }
            Message::ResumeCommand(resume) => {
                let ack = self.core.resume(&resume);
                let _ = self
                    .supervisor
                    .reply(&envelope.message_id, Message::ResumeAck(ack));
            }
            Message::SimControl(control) => match control.op {
                SimOp::InjectHumanInput => {
                    self.core.human_input_detected();
                }
                other => eprintln!("device: control {other:?} is not modelled by this backend"),
            },
            Message::Shutdown(shutdown) => {
                eprintln!("device: shutting down: {}", shutdown.reason);
                return Flow::Exit;
            }
            Message::Configure(_) => {}
            other => eprintln!("device: unexpected {} from the supervisor", other.name()),
        }
        self.forward_events();
        Flow::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_ipc::browser_control::{
        BrowserControlSession, PendingBrowserControl, read_hello, write_bootstrap, write_ready,
    };
    use interaction_contracts::desktop::browser::BrowserPageProfile;
    use interaction_contracts::desktop::browser_control::{
        BrowserControlBootstrap, BrowserControlReady, BrowserControlToken,
    };
    use interaction_contracts::desktop::device::ExecutionObservation;
    use interaction_contracts::desktop::message::Message;
    use interaction_contracts::desktop::samples;
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn guard_snapshot() -> GuardSnapshot {
        serde_json::from_value(serde_json::json!({
            "protocol_version": 1,
            "kind": "guard",
            "session_epoch": 7,
            "sequence": 1,
            "uptime_ms": 100,
            "accessibility": true,
            "human_input": false,
            "host_alive": true,
            "supervisor_alive": false,
            "stop_revision": 0,
            "human_sequence": 0,
            "hotkey_sequence": 0,
            "lease_loss_sequence": 0,
            "fault_sequence": 0,
            "reason": "redacted fixture",
            "input_available": false,
            "authority_clock_ready": true,
            "startup_mouse_buttons_clear": true,
        }))
        .unwrap()
    }

    fn connected_browser_session() -> (BrowserControlSession, UnixStream) {
        let evidence = samples::representative_messages()
            .into_iter()
            .find_map(|(_, message)| match message {
                Message::Observation(ExecutionObservation::Browser { observation }) => {
                    Some(*observation)
                }
                _ => None,
            })
            .expect("synthetic browser scope");
        let mut scope = evidence.scope;
        // The Core's trusted wall-clock profile is tied to this scope epoch;
        // synthetic CDP golden fixtures otherwise use a separate probe clock.
        scope.clock_domain = format!("headless/{}/wall", scope.context.session_epoch);
        scope
            .validate()
            .expect("synthetic browser scope uses Core clock");
        let bootstrap = BrowserControlBootstrap {
            version: interaction_contracts::desktop::browser_control::BROWSER_CONTROL_VERSION,
            token: BrowserControlToken::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
            channel_generation: 9,
            browser_page_profile: BrowserPageProfile::FixtureBackgroundOption,
            scope,
        };
        let (device, mut main) = UnixStream::pair().unwrap();
        let main_bootstrap = bootstrap.clone();
        let main = thread::spawn(move || {
            write_bootstrap(&mut main, &main_bootstrap).unwrap();
            let hello = read_hello(&mut main, &main_bootstrap).unwrap();
            write_ready(
                &mut main,
                &BrowserControlReady {
                    version:
                        interaction_contracts::desktop::browser_control::BROWSER_CONTROL_VERSION,
                    channel_generation: hello.channel_generation,
                    session_epoch: hello.session_epoch,
                    scope_id: hello.scope_id.clone(),
                    scope_generation: hello.scope_generation,
                    next_sequence: 1,
                },
                &hello,
            )
            .unwrap();
            main
        });
        let pending = PendingBrowserControl::read_from_stream(device).unwrap();
        let session = pending.finish().unwrap();
        (session, main.join().unwrap())
    }

    #[test]
    fn owned_readiness_diagnostics_require_the_explicit_owned_mode_setting() {
        assert!(owned_readiness_diagnostics_enabled(
            DeviceBackendMode::MacosOwnedValue,
            Some("1")
        ));
        assert!(!owned_readiness_diagnostics_enabled(
            DeviceBackendMode::MacosOwnedValue,
            Some("true")
        ));
        assert!(!owned_readiness_diagnostics_enabled(
            DeviceBackendMode::MacosOwnedValue,
            None
        ));
        assert!(!owned_readiness_diagnostics_enabled(
            DeviceBackendMode::MacosAxPress,
            Some("1")
        ));
        assert!(
            OwnedReadinessDiagnostics::new(false, 0)
                .sample_if_due(0, &guard_snapshot(), true, false)
                .is_none()
        );
    }

    #[test]
    fn owned_readiness_samples_respect_each_relative_time_boundary() {
        let mut diagnostics = OwnedReadinessDiagnostics::new(true, 1_000);
        let snapshot = guard_snapshot();
        assert_eq!(
            diagnostics
                .sample_if_due(1_000, &snapshot, false, false)
                .unwrap()
                .sample_index,
            0
        );
        assert!(
            diagnostics
                .sample_if_due(1_499, &snapshot, false, false)
                .is_none()
        );
        assert_eq!(
            diagnostics
                .sample_if_due(1_500, &snapshot, false, false)
                .unwrap()
                .sample_index,
            1
        );
        for (sample_index, offset_ms) in [1_500, 5_000, 10_000, 15_000]
            .into_iter()
            .enumerate()
            .map(|(index, offset)| (index + 2, offset))
        {
            assert_eq!(
                diagnostics
                    .sample_if_due(1_000 + offset_ms, &snapshot, false, false)
                    .unwrap()
                    .sample_index,
                sample_index
            );
        }
        assert!(
            diagnostics
                .sample_if_due(20_000, &snapshot, false, false)
                .is_none()
        );
    }

    #[test]
    fn owned_readiness_samples_are_bounded_and_do_not_repeat_after_clock_rollback() {
        let mut diagnostics = OwnedReadinessDiagnostics::new(true, 2_000);
        let snapshot = guard_snapshot();
        assert_eq!(
            diagnostics
                .sample_if_due(2_000, &snapshot, true, false)
                .unwrap()
                .sample_index,
            0
        );
        assert_eq!(
            diagnostics
                .sample_if_due(2_500, &snapshot, true, false)
                .unwrap()
                .sample_index,
            1
        );
        assert!(
            diagnostics
                .sample_if_due(2_200, &snapshot, true, false)
                .is_none()
        );
        assert_eq!(
            diagnostics
                .sample_if_due(3_500, &snapshot, true, false)
                .unwrap()
                .sample_index,
            2
        );
        for offset_ms in [5_000, 10_000, 15_000] {
            assert!(
                diagnostics
                    .sample_if_due(2_000 + offset_ms, &snapshot, true, false)
                    .is_some()
            );
        }
        assert!(
            diagnostics
                .sample_if_due(20_000, &snapshot, true, false)
                .is_none()
        );
    }

    #[test]
    fn background_ax_readiness_samples_are_six_bounded_indexed_snapshots() {
        let mut diagnostics = BackgroundAxReadinessDiagnostics::new(true, 3_000);
        assert_eq!(diagnostics.sample_if_due(3_000), Some(0));
        assert_eq!(diagnostics.sample_if_due(3_499), None);
        assert_eq!(diagnostics.sample_if_due(3_500), Some(1));
        for (index, offset_ms) in [1_500, 5_000, 10_000, 15_000].into_iter().enumerate() {
            assert_eq!(
                diagnostics.sample_if_due(3_000 + offset_ms),
                Some(index + 2)
            );
        }
        assert_eq!(diagnostics.sample_if_due(20_000), None);

        let mut disabled = BackgroundAxReadinessDiagnostics::new(false, 0);
        assert_eq!(disabled.sample_if_due(0), None);
    }

    #[test]
    fn owned_readiness_json_has_only_fixed_booleans_and_sample_index() {
        let sample = OwnedReadinessDiagnostics::new(true, 0)
            .sample_if_due(0, &guard_snapshot(), false, true)
            .unwrap();
        assert_eq!(
            sample.json_line(),
            "{\"kind\":\"owned_guard_readiness\",\"sample_index\":0,\"accessibility\":true,\"human_input\":false,\"startup_mouse_buttons_clear\":true,\"authority_clock_ready\":true,\"host_alive\":true,\"supervisor_alive\":false,\"backend_native_ready\":false,\"core_isolated\":true,\"ready\":false}"
        );
    }

    #[test]
    fn owned_save_core_profile_is_selected_only_for_an_explicit_startup_grant() {
        let setter = core_config(DeviceBackendMode::MacosOwnedValue, false);
        assert!(setter.allow_owned_document_value_setter);
        assert!(!setter.allow_owned_document_save);

        let actions = core_config(DeviceBackendMode::MacosOwnedValue, true);
        assert!(actions.allow_owned_document_value_setter);
        assert!(actions.allow_owned_document_save);
    }

    #[test]
    fn the_native_observation_facade_never_claims_fixture_input_capabilities() {
        let core = ExecutionCore::new(
            core_config(DeviceBackendMode::MacosObserve, false),
            DeviceBackend::MacosObserve,
        );
        let manifest = manifest(&core);
        assert_eq!(manifest.backend, BackendKind::Macos);
        assert_eq!(manifest.input, CapabilityState::Unavailable);
        assert_eq!(manifest.hit_test, CapabilityState::Unavailable);
        assert_eq!(manifest.human_input, CapabilityState::Unavailable);
        assert_eq!(manifest.identity.clock_domain, "macos/1/wall");
    }

    #[test]
    fn browser_manifest_requires_a_live_authenticated_scope_and_advertises_no_native_input() {
        let (session, main_peer) = connected_browser_session();
        let scope = session.scope.clone();
        let fixture_clock_ms = scope.issued_ms.saturating_add(5);
        let unix_now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_millis() as u64;
        let mut device_config = DeviceConfig {
            backend: DeviceBackendMode::HeadlessBrowser,
            socket_dir: std::env::temp_dir(),
            host_token: "0123456789abcdef0123456789abcdef".into(),
            session_epoch: scope.context.session_epoch,
            clock: SharedClock::from_origin(unix_now_ms.saturating_sub(fixture_clock_ms)),
            world_log: std::env::temp_dir().join("browser-fixture-world.log"),
            native_guard_path: None,
            native_target: None,
            native_target_window: None,
            native_target_process: None,
            owned_document_registration: None,
            owned_save_enabled: false,
            browser_scope: Some(scope),
            browser_control_session: Some(session),
        };
        let config = core_config_for_device(&device_config).unwrap();
        let backend = DeviceBackend::new(&mut device_config).unwrap();
        let core = ExecutionCore::new(config, backend);
        let available = manifest(&core);
        assert_eq!(available.backend, BackendKind::HeadlessBrowser);
        assert_eq!(available.execution_domain, ExecutionDomain::HeadlessBrowser);
        assert_eq!(available.input, CapabilityState::Available);
        assert_eq!(available.hit_test, CapabilityState::Unavailable);
        assert_eq!(available.human_input, CapabilityState::Unavailable);
        assert!(available.supports(
            ExecutionDomain::HeadlessBrowser,
            DomainAction::HeadlessBrowserAction
        ));
        assert!(!available.supports(
            ExecutionDomain::ForegroundNative,
            DomainAction::NativePointer
        ));
        assert!(available.validate().is_ok());

        drop(main_peer);
        for _ in 0..2_000 {
            if !core.backend().browser_ready() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let retired = manifest(&core);
        assert_eq!(retired.input, CapabilityState::Unavailable);
        assert!(retired.domains.iter().all(|domain| {
            domain.state == CapabilityState::Unavailable && domain.actions.is_empty()
        }));
    }

    #[test]
    fn simulated_profile_keeps_production_gates_while_selecting_simulated_domain() {
        let config = core_config(DeviceBackendMode::Simulated, false);
        assert_eq!(config.execution_domain, ExecutionDomain::Simulated);
        assert_eq!(config.policy, PrimitivePolicy::AtomicOnly);
        assert!(config.require_lease);
        assert!(config.require_hit_test);
    }

    #[test]
    fn passive_background_profile_is_selected_without_advertising_input() {
        let core = ExecutionCore::new(
            core_config(DeviceBackendMode::MacosBackgroundAx, false),
            DeviceBackend::MacosBackgroundAxPassive,
        );
        let value = manifest(&core);
        assert_eq!(value.execution_domain, ExecutionDomain::BackgroundAx);
        assert_eq!(value.backend, BackendKind::Macos);
        assert_eq!(value.input, CapabilityState::Unavailable);
        assert_eq!(value.hit_test, CapabilityState::Unavailable);
        assert_eq!(value.human_input, CapabilityState::Unavailable);
        assert_eq!(value.domains.len(), ExecutionDomain::ALL.len());
        assert!(value.domains.iter().all(
            |domain| domain.state == CapabilityState::Unavailable && domain.actions.is_empty()
        ));
        assert!(value.validate().is_ok());
    }

    #[test]
    fn background_manifest_can_advertise_only_its_axpress_domain_action() {
        let domains = domain_capabilities(
            ExecutionDomain::BackgroundAx,
            CapabilityState::Available,
            &[DomainAction::BackgroundAxPress],
        );
        let available = domains
            .iter()
            .filter(|domain| domain.state == CapabilityState::Available)
            .collect::<Vec<_>>();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].domain, ExecutionDomain::BackgroundAx);
        assert_eq!(available[0].actions, vec![DomainAction::BackgroundAxPress]);
        assert!(
            domains
                .iter()
                .filter(|domain| domain.domain != ExecutionDomain::BackgroundAx)
                .all(|domain| domain.state == CapabilityState::Unavailable
                    && domain.actions.is_empty())
        );
    }

    #[test]
    fn an_unattached_native_executor_cannot_advertise_input() {
        let core = ExecutionCore::new(
            core_config(DeviceBackendMode::MacosAxPress, false),
            DeviceBackend::MacosAxPress(Box::new(crate::backend::native::NativeAxBackend::new(
                (42, "com.lingxi.computer-user.guard-fixture".into()),
                None,
            ))),
        );
        let value = manifest(&core);
        assert_eq!(value.input, CapabilityState::Unavailable);
        assert_eq!(value.hit_test, CapabilityState::Unavailable);
    }
}
