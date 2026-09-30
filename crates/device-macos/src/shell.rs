//! The device process's loop (design 4.6, 14.4 and 14.8).
//!
//! One thread runs everything that touches the core. Reader threads feed it
//! from the supervisor's pipe and from the host's socket, and the loop wakes
//! for the core's own deadlines. The supervisor's stop commands arrive on their
//! own pipe and are applied the moment they are read; nothing in front of them
//! waits for the host.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use desktop_ipc::SharedClock;
use desktop_ipc::handshake::{Acceptance, Connection, spawn_acceptor};
use desktop_ipc::link::{FramedReader, Inbound, Link, spawn_reader};
use desktop_ipc::socket;
use execution_core::core::{CoreConfig, CoreEvent, ExecutionCore};
use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, CommandAck, DependencyVersions, DeviceEvent,
    DeviceEventKind, GrantOutcome, SimOp, StopCommand, StopDisposition, StopScope, StopSource,
};
use interaction_contracts::desktop::handshake::{PeerRole, Ready};
use interaction_contracts::desktop::message::{Envelope, Message};
use interaction_contracts::desktop::primitive::PrimitivePolicy;

use crate::backend::SimWorldBackend;
use crate::config::DeviceConfig;

/// The longest the loop sleeps, so a stop or a deadline is never far away.
const MAX_WAIT_MS: u64 = 20;

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
    link: Link<UnixStream>,
}

struct Shell<W: Write> {
    core: ExecutionCore<SimWorldBackend>,
    supervisor: Link<W>,
    host: Option<HostConn>,
    clock: SharedClock,
    event_seq: u64,
    next_conn: u64,
    /// For the reader thread of each host connection.
    tx: Sender<Event>,
}

/// What this device can really do right now.
pub fn manifest(core: &ExecutionCore<SimWorldBackend>) -> CapabilityManifest {
    CapabilityManifest {
        identity: core.identity(),
        backend: BackendKind::Simulated,
        policy: PrimitivePolicy::AtomicOnly,
        human_input: CapabilityState::Available,
        hit_test: CapabilityState::Available,
        input: CapabilityState::Available,
        max_uninterruptible_ms: 100,
    }
}

fn ack(result: Result<(), String>) -> Message {
    Message::CommandAck(match result {
        Ok(()) => CommandAck::ok(),
        Err(reason) => CommandAck::rejected(None, reason),
    })
}

/// Run the device until the supervisor is gone or says to stop.
pub fn run<R, W>(
    config: DeviceConfig,
    supervisor: Link<W>,
    supervisor_reader: FramedReader<R>,
) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write,
{
    socket::private_dir(&config.socket_dir).map_err(|e| format!("socket directory: {e}"))?;
    let listener = socket::listen(&config.socket_path()).map_err(|e| format!("listening: {e}"))?;
    let mut core = ExecutionCore::new(
        CoreConfig::production("sim"),
        SimWorldBackend::new(&config.world_log, config.clock),
    );
    // A restarted device starts a new session: its epoch is never reused.
    for _ in 1..config.session_epoch {
        core.reconnect();
    }

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
        supervisor,
        host: None,
        clock: config.clock,
        event_seq: 0,
        next_conn: 0,
        tx,
    };
    let ready = Message::Ready(Ready {
        role: PeerRole::Device,
        capabilities: Some(manifest(&shell.core)),
        notes: vec!["simulated backend".into()],
    });
    shell
        .supervisor
        .send(ready)
        .map_err(|e| format!("the supervisor pipe: {e}"))?;
    shell.event_loop(&rx);
    Ok(())
}

impl<W: Write> Shell<W> {
    fn event_loop(&mut self, rx: &Receiver<Event>) {
        loop {
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
    }

    fn close_gate(&mut self, reason: &str) {
        self.core.stop(&StopCommand {
            scope: StopScope::All,
            disposition: StopDisposition::Cancel,
            source: StopSource::Supervisor,
            reason: reason.into(),
        });
        self.forward_events();
    }

    fn tick(&mut self) {
        let now = self.clock.now_ms();
        // A clock that stepped back changes nothing: the core never goes back.
        let _ = self.core.advance_to(now);
        self.core.backend_mut().catch_up();
        self.core.dispatch_next();
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
        let Connection { reader, mut link } = conn;
        let ready = Message::Ready(Ready {
            role: PeerRole::Device,
            capabilities: Some(manifest(&self.core)),
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
        self.host = Some(HostConn { id, link });
    }

    /// The host is gone. Whatever it was doing stops until a host says otherwise.
    fn host_lost(&mut self) {
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
                    self.device_event(DeviceEventKind::PermitRevoked { permit, reason }, false);
                }
                CoreEvent::Isolated { reason } => {
                    self.device_event(DeviceEventKind::ChannelIsolated { reason }, true);
                }
                CoreEvent::Recovered => self.device_event(DeviceEventKind::ChannelRecovered, false),
                CoreEvent::Stopped(ack) => {
                    // Main and the host both hear about every stop.
                    self.device_event(DeviceEventKind::Stopped { ack: ack.clone() }, false);
                    self.send_supervisor(Message::StopAck(ack));
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
                self.core.update_dependency(&update.name, update.valid);
                Some(Message::DependencyVersions(DependencyVersions {
                    versions: self.core.dependency_versions(),
                }))
            }
            Message::StopCommand(stop) => Some(Message::StopAck(self.core.stop(&stop))),
            Message::ResumeCommand(resume) => Some(Message::ResumeAck(self.core.resume(&resume))),
            Message::LeaseRenew(lease) => {
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
                self.core.stop(&stop);
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
