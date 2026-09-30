//! The source process's loop.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use desktop_ipc::handshake::{Acceptance, Connection, spawn_acceptor};
use desktop_ipc::link::{FramedReader, Inbound, Link, spawn_reader};
use desktop_ipc::socket;
use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::handshake::{PeerRole, Ready};
use interaction_contracts::desktop::message::Message;
use interaction_contracts::desktop::source::{
    SourceCommand, SourceHealth, SourceKind, SourceState,
};
use sim_world::app::SnapshotMeta;
use sim_world::world_log::WorldFollower;

use crate::config::SourceConfig;

enum Event {
    Supervisor(Inbound),
    Host(u64, Inbound),
    NewHost(Box<Connection>),
}

struct HostConn {
    id: u64,
    link: Link<UnixStream>,
}

/// Report the application until the supervisor is gone.
pub fn run<R, W>(
    config: SourceConfig,
    mut supervisor: Link<W>,
    supervisor_reader: FramedReader<R>,
) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write,
{
    socket::private_dir(&config.socket_dir).map_err(|e| format!("socket directory: {e}"))?;
    let listener = socket::listen(&config.socket_path()).map_err(|e| format!("listening: {e}"))?;
    let (tx, rx) = mpsc::channel::<Event>();
    spawn_reader(
        "source-supervisor",
        supervisor_reader,
        tx.clone(),
        Event::Supervisor,
    );
    spawn_acceptor(
        "source-accept",
        listener,
        tx.clone(),
        config.clock,
        Acceptance {
            producer: "source".into(),
            role: PeerRole::Host,
            token: config.host_token.clone(),
            session_epoch: config.session_epoch,
        },
        Event::NewHost,
    );
    supervisor
        .send(Message::Ready(Ready {
            role: PeerRole::Source,
            capabilities: None,
            notes: vec!["fixture source".into()],
        }))
        .map_err(|e| format!("the supervisor pipe: {e}"))?;

    let mut follower = WorldFollower::new(&config.world_log);
    let mut host: Option<HostConn> = None;
    let mut next_conn = 0u64;
    let mut sequence = 0u64;
    let interval = Duration::from_millis(config.interval_ms.max(5));

    let report = |host: &mut Option<HostConn>, follower: &mut WorldFollower, sequence: &mut u64| {
        let Some(conn) = host.as_mut() else {
            return;
        };
        let _ = follower.catch_up();
        *sequence += 1;
        let snapshot = follower.app.snapshot(&SnapshotMeta {
            snapshot_id: format!("snap-{sequence}"),
            session_epoch: config.session_epoch,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: config.session_epoch,
            },
            clock_domain: format!("sim/{}/monotonic", config.session_epoch),
            source_sequence: *sequence,
            observed_ms: config.clock.now_ms(),
        });
        if conn
            .link
            .send(Message::StructureSnapshot(snapshot))
            .is_err()
        {
            *host = None;
        }
    };

    loop {
        match rx.recv_timeout(interval) {
            Err(RecvTimeoutError::Timeout) => report(&mut host, &mut follower, &mut sequence),
            Err(RecvTimeoutError::Disconnected) => break,
            Ok(Event::Supervisor(Inbound::Frame(envelope))) => match envelope.message {
                Message::Shutdown(_) => break,
                Message::Configure(_) => {}
                other => eprintln!("source: unexpected {} from the supervisor", other.name()),
            },
            Ok(Event::Supervisor(Inbound::Closed | Inbound::Broken(_))) => break,
            Ok(Event::NewHost(conn)) => {
                let Connection { reader, mut link } = *conn;
                let ready = Message::Ready(Ready {
                    role: PeerRole::Source,
                    capabilities: None,
                    notes: vec![],
                });
                let health = Message::SourceHealth(SourceHealth {
                    source: SourceKind::Ax,
                    state: SourceState::Available,
                    reason: None,
                    at_ms: config.clock.now_ms(),
                });
                if link.send(ready).is_err() || link.send(health).is_err() {
                    continue;
                }
                next_conn += 1;
                let id = next_conn;
                spawn_reader("source-host", reader, tx.clone(), move |inbound| {
                    Event::Host(id, inbound)
                });
                host = Some(HostConn { id, link });
                report(&mut host, &mut follower, &mut sequence);
            }
            Ok(Event::Host(id, Inbound::Frame(envelope))) => {
                if host.as_ref().is_some_and(|h| h.id == id) {
                    match envelope.message {
                        Message::SourceCommand(SourceCommand::RefreshNow) => {
                            report(&mut host, &mut follower, &mut sequence);
                        }
                        other => eprintln!("source: unexpected {} from the host", other.name()),
                    }
                }
            }
            Ok(Event::Host(id, Inbound::Closed | Inbound::Broken(_))) => {
                if host.as_ref().is_some_and(|h| h.id == id) {
                    host = None;
                }
            }
        }
    }
    Ok(())
}
