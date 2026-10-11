//! Source IO stays outside the host and the source control loop. Fixture mode
//! reports the world log; native mode runs one bounded helper job at a time.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use desktop_ipc::handshake::{Acceptance, Connection, spawn_acceptor};
use desktop_ipc::link::{FramedReader, Inbound, Link, QueuedWriter, spawn_reader};
use desktop_ipc::socket;
use interaction_contracts::desktop::device::{CommandAck, SurfaceRef};
use interaction_contracts::desktop::handshake::{PeerRole, Ready};
use interaction_contracts::desktop::ids::{MessageId, RequestId};
use interaction_contracts::desktop::message::Message;
use interaction_contracts::desktop::source::{
    SourceCommand, SourceHealth, SourceKind, SourceStamp, SourceState,
};
use interaction_contracts::desktop::task::RowTableScope;
use sim_world::app::SnapshotMeta;
use sim_world::world_log::WorldFollower;

use crate::config::{NativeCaptureMode, SourceBackend, SourceConfig, validate_refresh_table_scope};
use crate::images::ImageServer;
use crate::native::watch::NativeWatch;
use crate::native::{NativeWorker, unavailable};

const OUTPUT_QUEUE: usize = 16;
const CONTROL_POLL_MS: u64 = 20;
const WATCH_REFRESH_COOLDOWN_MS: u64 = 250;

enum Event {
    Supervisor(Inbound),
    Host(u64, Inbound),
    NewHost(Box<Connection>),
}

struct HostConn {
    id: u64,
    link: Link<QueuedWriter>,
    closer: UnixStream,
}

struct PendingCapture {
    owner: u64,
    message_id: MessageId,
    request_id: RequestId,
    watch_id: String,
    deadline_ms: u64,
}

fn reject_capture(host: &mut Option<HostConn>, ticket: PendingCapture, code: &str) {
    if let Some(conn) = host.as_mut()
        && conn.id == ticket.owner
        && conn
            .link
            .reply(
                &ticket.message_id,
                Message::CommandAck(CommandAck::rejected(None, code)),
            )
            .is_err()
    {
        *host = None;
    }
}

impl Drop for HostConn {
    fn drop(&mut self) {
        let _ = self.closer.shutdown(std::net::Shutdown::Both);
    }
}

/// Only this peer's ticket is confirmed. A refresh acknowledgement means the
/// requested read was scheduled; it grants no freshness, census-completeness,
/// uniqueness or execution authority.
fn acknowledge_refresh(host: &mut Option<HostConn>, ticket: &mut Option<(u64, MessageId)>) {
    let Some((owner, request)) = ticket.take() else {
        return;
    };
    if let Some(conn) = host.as_mut()
        && conn.id == owner
        && conn
            .link
            .reply(&request, Message::CommandAck(CommandAck::ok()))
            .is_err()
    {
        *host = None;
    }
}

fn reject_refresh(host: &mut Option<HostConn>, owner: u64, request: &MessageId, code: &str) {
    if let Some(conn) = host.as_mut()
        && conn.id == owner
        && conn
            .link
            .reply(
                request,
                Message::CommandAck(CommandAck::rejected(None, code)),
            )
            .is_err()
    {
        *host = None;
    }
}

fn watch_config_with_scope(
    base: &crate::config::NativeConfig,
    table_scope: &Option<RowTableScope>,
) -> crate::config::NativeConfig {
    let mut config = base.clone();
    config.table_scope = table_scope.clone();
    config
}

pub fn run<R, W>(
    config: SourceConfig,
    supervisor: Link<W>,
    supervisor_reader: FramedReader<R>,
) -> Result<(), String>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
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
    let images = config
        .image_token
        .as_ref()
        .map(|token| {
            ImageServer::new(
                &config.socket_dir.join("images.sock"),
                token.clone(),
                config.session_epoch,
            )
        })
        .transpose()
        .map_err(|e| format!("image channel: {e}"))?;
    let mut supervisor = supervisor.queued("source-supervisor-output", OUTPUT_QUEUE);
    let mut follower = match &config.backend {
        SourceBackend::Fixture { world_log } => Some(WorldFollower::new(world_log)),
        SourceBackend::Native(_) => None,
    };
    let mut native = match &config.backend {
        SourceBackend::Native(native) if native.mode == NativeCaptureMode::Probe => {
            Some(NativeWorker::new(
                native.as_ref().clone(),
                config.session_epoch,
                config.clock_domain.clone(),
            )?)
        }
        _ => None,
    };
    let watch_config = match &config.backend {
        SourceBackend::Native(native) if native.mode == NativeCaptureMode::Watch => {
            Some(native.as_ref().clone())
        }
        _ => None,
    };
    let mut watcher: Option<NativeWatch> = None;
    let mut active_table_scope = watch_config
        .as_ref()
        .and_then(|native| native.table_scope.clone());
    let mut watcher_owner: Option<u64> = None;
    let mut logged_manifest = None;
    let notes = if watch_config.is_some() {
        vec!["native SCStream watch with AX and Vision; native input remains unavailable".into()]
    } else if native.is_some() {
        vec!["native observation entry: one-shot AX, ScreenCaptureKit capture and Vision; input unavailable".into()]
    } else {
        vec!["fixture source".into()]
    };
    supervisor
        .send(Message::Ready(Ready {
            role: PeerRole::Source,
            capabilities: None,
            notes: notes.clone(),
        }))
        .map_err(|e| format!("the supervisor pipe: {e}"))?;

    let mut host: Option<HostConn> = None;
    let mut next_conn = 0u64;
    let mut sequence = 0u64;
    let mut refresh = false;
    let mut refresh_requested_at: Option<Instant> = None;
    let mut refresh_reply: Option<(u64, MessageId)> = None;
    let mut running_reply: Option<(u64, MessageId)> = None;
    let mut capture_reply: Option<PendingCapture> = None;
    let interval = Duration::from_millis(config.interval_ms);
    let mut last_started = Instant::now();
    let mut latest_health = Vec::new();
    let mut last_watch_record: Option<(String, u64)> = None;
    if let Some(worker) = native.as_mut() {
        worker.request(false, config.clock.now_ms(), 0);
    }

    loop {
        if supervisor.get_ref().failed() {
            return Err("the source supervisor output failed".into());
        }
        if host
            .as_ref()
            .is_some_and(|host| host.link.get_ref().failed())
        {
            host = None;
        }
        if watcher_owner.is_some_and(|owner| host.as_ref().is_none_or(|host| host.id != owner)) {
            if let Some(ticket) = capture_reply.take()
                && let Some(watch) = &watcher
            {
                watch.cancel_capture(&ticket.request_id, &ticket.watch_id);
            }
            drop(watcher.take());
            watcher_owner = None;
            active_table_scope = watch_config
                .as_ref()
                .and_then(|native| native.table_scope.clone());
            last_watch_record = None;
            logged_manifest = None;
            if let Some(images) = &images {
                images.clear();
            }
            refresh = false;
            refresh_requested_at = None;
            refresh_reply = None;
            running_reply = None;
        }
        if capture_reply.as_ref().is_some_and(|ticket| {
            config.clock.now_ms() >= ticket.deadline_ms
                || host.as_ref().is_none_or(|conn| conn.id != ticket.owner)
        }) {
            let ticket = capture_reply.take().unwrap();
            if let Some(watch) = &watcher {
                watch.cancel_capture(&ticket.request_id, &ticket.watch_id);
            }
            reject_capture(&mut host, ticket, "deadline_expired");
        }
        if let Some(watch) = watcher.as_ref() {
            for mut report in watch.take_reports() {
                if let Some(failure) = report.capture_failure.take()
                    && capture_reply.as_ref().is_some_and(|ticket| {
                        ticket.request_id == failure.request_id
                            && ticket.watch_id == failure.watch_id
                    })
                {
                    reject_capture(&mut host, capture_reply.take().unwrap(), &failure.code);
                }
                if let Some(capture) = report
                    .frame_meta
                    .as_ref()
                    .and_then(|frame| frame.capture_info())
                    && !capture_reply.as_ref().is_some_and(|ticket| {
                        ticket.request_id == capture.request_id
                            && ticket.watch_id == capture.watch_id
                            && config.clock.now_ms() < ticket.deadline_ms
                    })
                {
                    // Late, cancelled and foreign results never reach either channel.
                    continue;
                }
                if let Some(images) = &images {
                    if report.health.iter().any(|h| {
                        h.state != SourceState::Available && h.source == SourceKind::Screenshot
                    }) {
                        images.clear();
                    }
                    if let (Some(meta), Some(bytes)) =
                        (report.frame_meta.as_ref(), report.image.take())
                    {
                        images.offer(desktop_ipc::image::ImageFrame {
                            meta: meta.clone(),
                            bytes,
                        });
                    }
                }
                if !report.stream_id.is_empty() {
                    last_watch_record = Some((report.stream_id.clone(), report.sequence));
                }
                for health in &report.health {
                    remember_health(&mut latest_health, health.clone());
                }
                if let Some(manifest) = report.manifest
                    && logged_manifest.as_ref() != Some(&manifest)
                {
                    eprintln!("cu-source: native watch manifest {}", manifest.display());
                    logged_manifest = Some(manifest);
                }
                if let Some(snapshot) = report.snapshot.as_mut() {
                    sequence = sequence.checked_add(1).ok_or("source sequence exhausted")?;
                    // The helper stream stamp remains on health/frames. AX wire
                    // sequence stays monotonic across explicit helper restarts.
                    crate::native::normalize_snapshot_provenance(snapshot, sequence);
                }
                if images.is_some() {
                    if let Some(frame) = report.frame_meta.as_ref() {
                        supervisor
                            .send(Message::FrameMeta(frame.clone()))
                            .map_err(|e| format!("image metadata control: {e}"))?;
                    }
                    for health in &report.health {
                        supervisor
                            .send(Message::SourceHealth(health.clone()))
                            .map_err(|e| format!("image health control: {e}"))?;
                    }
                }
                if let Some(conn) = host.as_mut() {
                    let mut sent = true;
                    if let Some(frame) = report.frame_meta {
                        sent &= conn.link.send(Message::FrameMeta(frame)).is_ok();
                    }
                    for health in report.health {
                        sent &= conn.link.send(Message::SourceHealth(health)).is_ok();
                    }
                    if let Some(snapshot) = report.snapshot {
                        sent &= conn
                            .link
                            .send(Message::StructureSnapshot(Box::new(snapshot)))
                            .is_ok();
                    }
                    if !sent {
                        host = None;
                    }
                }
            }
        }
        if let Some(result) = native.as_mut().and_then(NativeWorker::take_report) {
            let now = config.clock.now_ms();
            match result {
                Ok(mut report) => {
                    for health in &mut report.health {
                        health.at_ms = now;
                    }
                    latest_health = report.health;
                    if let Some(manifest) = report.manifest {
                        eprintln!(
                            "cu-source: native observation manifest {}",
                            manifest.display()
                        );
                    }
                    if let Some(conn) = host.as_mut() {
                        let mut sent = true;
                        for health in &latest_health {
                            sent &= conn
                                .link
                                .send(Message::SourceHealth(health.clone()))
                                .is_ok();
                        }
                        if let Some(snapshot) = report.snapshot {
                            sent &= conn
                                .link
                                .send(Message::StructureSnapshot(Box::new(snapshot)))
                                .is_ok();
                        }
                        if !sent {
                            host = None;
                        }
                    }
                }
                Err(reason) => {
                    latest_health = unavailable(&reason, now);
                    if let Some(conn) = host.as_mut() {
                        let sent = latest_health.iter().all(|health| {
                            conn.link
                                .send(Message::SourceHealth(health.clone()))
                                .is_ok()
                        });
                        if !sent {
                            host = None;
                        }
                    }
                }
            }
            acknowledge_refresh(&mut host, &mut running_reply);
        }
        if host.is_some()
            && refresh
            && let Some(native) = watch_config.as_ref()
            && last_started.elapsed() >= Duration::from_millis(WATCH_REFRESH_COOLDOWN_MS)
            && refresh_requested_at.is_some_and(|requested| {
                requested.elapsed() >= Duration::from_millis(WATCH_REFRESH_COOLDOWN_MS)
            })
        {
            // An explicit refresh must obtain new pixels even for a static
            // screen whose SCStream only emits IDLE. Restart one worker at a
            // time with a new stream identity; bursts share one pending bit.
            let at_ms = latest_health
                .iter()
                .map(|health| health.at_ms)
                .max()
                .unwrap_or(0)
                .max(config.clock.now_ms());
            let provenance = last_watch_record
                .as_ref()
                .map(|(stream_id, sequence)| {
                    Ok::<_, String>(SourceStamp {
                        session_epoch: config.session_epoch,
                        stream_id: stream_id.clone(),
                        source_sequence: sequence
                            .checked_add(1)
                            .ok_or("source sequence exhausted")?,
                    })
                })
                .transpose()?;
            let mut sent = true;
            for source in [
                SourceKind::Screenshot,
                SourceKind::Ax,
                SourceKind::AxObserver,
                SourceKind::Ocr,
            ] {
                let health = SourceHealth {
                    source,
                    state: SourceState::Unavailable,
                    reason: Some(
                        "an explicit refresh invalidated the previous source stream".into(),
                    ),
                    at_ms,
                    provenance: provenance.clone(),
                };
                remember_health(&mut latest_health, health.clone());
                if images.is_some() {
                    supervisor
                        .send(Message::SourceHealth(health.clone()))
                        .map_err(|error| format!("refresh image health control: {error}"))?;
                }
                if let Some(conn) = host.as_mut() {
                    sent &= conn.link.send(Message::SourceHealth(health)).is_ok();
                }
            }
            // Queue the control fault before cancelling. Nothing remaining in
            // the old mailbox can be forwarded after its authority is closed.
            if let Some(ticket) = capture_reply.take() {
                reject_capture(&mut host, ticket, "capture_unavailable");
            }
            if let Some(images) = &images {
                images.clear();
            }
            drop(watcher.take());
            last_watch_record = None;
            logged_manifest = None;
            if sent {
                watcher = Some(NativeWatch::new(
                    watch_config_with_scope(native, &active_table_scope),
                    config.session_epoch,
                    config.clock_domain.clone(),
                    config.clock,
                    config.interval_ms,
                )?);
                watcher_owner = host.as_ref().map(|host| host.id);
            } else {
                host = None;
            }
            refresh = false;
            refresh_requested_at = None;
            last_started = Instant::now();
            acknowledge_refresh(&mut host, &mut refresh_reply);
        }
        if host.is_some()
            && watch_config.is_none()
            && (refresh || last_started.elapsed() >= interval)
        {
            if let Some(worker) = native.as_mut() {
                let next = sequence.checked_add(1).ok_or("source sequence exhausted")?;
                if worker.request(true, config.clock.now_ms(), next) {
                    sequence = next;
                    running_reply = refresh_reply.take();
                    refresh = false;
                    last_started = Instant::now();
                }
            } else if let Some(follower) = follower.as_mut() {
                sequence = sequence.checked_add(1).ok_or("source sequence exhausted")?;
                report_fixture(&config, &mut host, follower, sequence);
                acknowledge_refresh(&mut host, &mut refresh_reply);
                refresh = false;
                last_started = Instant::now();
            }
        }
        match rx.recv_timeout(Duration::from_millis(CONTROL_POLL_MS)) {
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
            Ok(Event::Supervisor(Inbound::Frame(envelope))) => match envelope.message {
                Message::Shutdown(_) => break,
                Message::Configure(_) => {}
                other => eprintln!("source: unexpected {} from the supervisor", other.name()),
            },
            Ok(Event::Supervisor(Inbound::Closed | Inbound::Broken(_))) => break,
            Ok(Event::NewHost(conn)) => {
                refresh_reply = None;
                if let Some(ticket) = capture_reply.take()
                    && let Some(watch) = &watcher
                {
                    watch.cancel_capture(&ticket.request_id, &ticket.watch_id);
                }
                let Connection { reader, link } = *conn;
                let closer = link
                    .get_ref()
                    .try_clone()
                    .map_err(|e| format!("source host socket: {e}"))?;
                let mut link = link.queued("source-host-output", OUTPUT_QUEUE);
                if link
                    .send(Message::Ready(Ready {
                        role: PeerRole::Source,
                        capabilities: None,
                        notes: notes.clone(),
                    }))
                    .is_err()
                {
                    let _ = closer.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                if native.is_none() && watch_config.is_none() {
                    latest_health = vec![SourceHealth {
                        source: SourceKind::Ax,
                        state: SourceState::Available,
                        reason: None,
                        at_ms: config.clock.now_ms(),
                        provenance: None,
                    }];
                }
                if !latest_health
                    .iter()
                    .all(|health| link.send(Message::SourceHealth(health.clone())).is_ok())
                {
                    let _ = closer.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                drop(watcher.take());
                watcher_owner = None;
                active_table_scope = watch_config
                    .as_ref()
                    .and_then(|native| native.table_scope.clone());
                last_watch_record = None;
                logged_manifest = None;
                if let Some(images) = &images {
                    images.clear();
                }
                running_reply = None;
                next_conn += 1;
                let id = next_conn;
                spawn_reader("source-host", reader, tx.clone(), move |inbound| {
                    Event::Host(id, inbound)
                });
                host = Some(HostConn { id, link, closer });
                if watcher.is_none()
                    && let Some(native) = watch_config.as_ref()
                {
                    watcher = Some(NativeWatch::new(
                        watch_config_with_scope(native, &active_table_scope),
                        config.session_epoch,
                        config.clock_domain.clone(),
                        config.clock,
                        config.interval_ms,
                    )?);
                    watcher_owner = Some(id);
                    last_started = Instant::now();
                }
                refresh = watch_config.is_none();
                refresh_requested_at = None;
            }
            Ok(Event::Host(id, Inbound::Frame(envelope))) => {
                if host.as_ref().is_some_and(|host| host.id == id) {
                    match envelope.message {
                        Message::SourceCommand(command @ SourceCommand::RefreshNow { .. }) => {
                            let scope = match &command {
                                SourceCommand::RefreshNow { table_scope } => table_scope.as_ref(),
                                _ => unreachable!(),
                            };
                            let rejection = if command.validate().is_err() {
                                Some("invalid_table_scope")
                            } else {
                                validate_refresh_table_scope(&config, scope).err()
                            };
                            if let Some(code) = rejection {
                                reject_refresh(&mut host, id, &envelope.message_id, code);
                            } else {
                                let SourceCommand::RefreshNow { table_scope } = command else {
                                    unreachable!()
                                };
                                active_table_scope = table_scope;
                                refresh = true;
                                refresh_requested_at = Some(Instant::now());
                                refresh_reply = Some((id, envelope.message_id));
                            }
                        }
                        Message::SourceCommand(command @ SourceCommand::CaptureNow { .. }) => {
                            let SourceCommand::CaptureNow {
                                request_id,
                                watch_id,
                                deadline_ms,
                                ..
                            } = &command
                            else {
                                unreachable!()
                            };
                            let ticket = PendingCapture {
                                owner: id,
                                message_id: envelope.message_id.clone(),
                                request_id: request_id.clone(),
                                watch_id: watch_id.clone(),
                                deadline_ms: *deadline_ms,
                            };
                            let accepted = !refresh
                                && capture_reply.is_none()
                                && config.clock.now_ms() < *deadline_ms
                                && watcher.as_ref().is_some_and(|watch| {
                                    watch.request_capture(command.clone()).is_ok()
                                });
                            if accepted {
                                capture_reply = Some(ticket);
                                if let Some(conn) = host.as_mut()
                                    && conn
                                        .link
                                        .reply(
                                            &envelope.message_id,
                                            Message::CommandAck(CommandAck::ok()),
                                        )
                                        .is_err()
                                {
                                    host = None;
                                }
                            } else {
                                reject_capture(&mut host, ticket, "capture_unavailable");
                            }
                        }
                        Message::SourceCommand(SourceCommand::CancelCapture {
                            request_id,
                            watch_id,
                        }) => {
                            if let Some(watch) = &watcher {
                                watch.cancel_capture(&request_id, &watch_id);
                            }
                            if capture_reply.as_ref().is_some_and(|ticket| {
                                ticket.owner == id
                                    && ticket.request_id == request_id
                                    && ticket.watch_id == watch_id
                            }) {
                                capture_reply = None;
                            }
                        }
                        other => eprintln!("source: unexpected {} from the host", other.name()),
                    }
                }
            }
            Ok(Event::Host(id, Inbound::Closed | Inbound::Broken(_))) => {
                if host.as_ref().is_some_and(|host| host.id == id) {
                    host = None;
                }
            }
        }
    }
    // Drop kills and reaps the helper; it does not wait for a source or OCR deadline.
    drop(native);
    drop(watcher);
    drop(host);
    let _ = supervisor.get_mut().finish(Duration::from_millis(100));
    Ok(())
}

#[cfg(test)]
mod tests;

fn remember_health(latest: &mut Vec<SourceHealth>, health: SourceHealth) {
    if let Some(previous) = latest
        .iter_mut()
        .find(|previous| previous.source == health.source)
    {
        *previous = health;
    } else {
        latest.push(health);
    }
}

fn report_fixture(
    config: &SourceConfig,
    host: &mut Option<HostConn>,
    follower: &mut WorldFollower,
    sequence: u64,
) {
    let Some(conn) = host.as_mut() else {
        return;
    };
    let _ = follower.catch_up();
    let snapshot = follower.app.snapshot(&SnapshotMeta {
        snapshot_id: format!("snap-{sequence}"),
        session_epoch: config.session_epoch,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: config.session_epoch,
        },
        clock_domain: config.clock_domain.clone(),
        source_sequence: sequence,
        observed_ms: config.clock.now_ms(),
    });
    if conn
        .link
        .send(Message::StructureSnapshot(Box::new(snapshot)))
        .is_err()
    {
        *host = None;
    }
}
