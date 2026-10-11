//! Real cu-device/Core/InputBackend/native-worker integration for owned apps.
//! The independent driver supplies an actual source proof and injects faults.
use desktop_ipc::{
    SharedClock,
    handshake::{Connection, make_hello, read_ready, send_hello},
    link::{FramedReader, Inbound, Link, spawn_reader},
    socket,
};
use interaction_contracts::desktop::{
    binding::BindingProof,
    device::*,
    handshake::{Configure, PeerRole, Shutdown as ShutdownMessage},
    message::Message,
};
use std::{
    collections::BTreeMap,
    io::Write,
    net::Shutdown,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

struct Startup {
    child: Option<Child>,
    dir: PathBuf,
}
impl Drop for Startup {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
pub struct Session {
    child: Child,
    pub dir: PathBuf,
    pub supervisor: Link<ChildStdin>,
    supervisor_in: Receiver<Inbound>,
    pub host: Link<std::os::unix::net::UnixStream>,
    host_in: Receiver<Inbound>,
    pub clock: SharedClock,
    next_lease: Instant,
    epoch: u64,
    pending_error: Option<String>,
}
impl Session {
    pub fn start(
        device: &str,
        helper: &str,
        proof: &BindingProof,
    ) -> Result<(Self, DeviceIdentity), String> {
        Self::start_epoch(device, helper, proof, 1, false)
    }
    pub fn start_epoch(
        device: &str,
        helper: &str,
        proof: &BindingProof,
        epoch: u64,
        observing: bool,
    ) -> Result<(Self, DeviceIdentity), String> {
        let clock = SharedClock::starting_now();
        let dir = PathBuf::from("/private/tmp").join(format!(
            "cu-native-{}-{}",
            std::process::id(),
            clock.origin_unix_ms()
        ));
        socket::private_dir(&dir).map_err(|e| e.to_string())?;
        let token = socket::new_token().map_err(|e| e.to_string())?;
        let child = Command::new(device)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut starting = Startup {
            child: Some(child),
            dir: dir.clone(),
        };
        let child = starting.child.as_mut().unwrap();
        println!(
            "{}",
            serde_json::json!({"kind":"device_started","pid":child.id()})
        );
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        let mut supervisor = Link::new(child.stdin.take().unwrap(), "fixture-supervisor", clock);
        let mut settings = BTreeMap::new();
        for (k, v) in [
            ("socket_dir", dir.display().to_string()),
            ("host_token", token.clone()),
            ("clock_origin_ms", clock.origin_unix_ms().to_string()),
            ("world_log", dir.join("world.log").display().to_string()),
            ("backend", "macos-ax-press".into()),
            ("native_guard_path", helper.into()),
            ("native_target_pid", proof.fingerprint.app_pid.to_string()),
            ("native_target_bundle", proof.fingerprint.app.clone()),
            (
                "native_target_window_id",
                proof.fingerprint.window_id.clone(),
            ),
        ] {
            settings.insert(k.into(), v);
        }
        if observing {
            settings.insert("backend".into(), "macos-observe".into());
            for key in [
                "native_target_pid",
                "native_target_bundle",
                "native_target_window_id",
            ] {
                settings.remove(key);
            }
        }
        supervisor
            .send(Message::Configure(Configure {
                role: PeerRole::Device,
                session_epoch: epoch,
                settings,
                browser_page_profile: None,
            }))
            .map_err(|e| e.to_string())?;
        let (tx, supervisor_in) = mpsc::channel();
        spawn_reader(
            "owned-device-supervisor",
            FramedReader::new(child.stdout.take().unwrap()),
            tx,
            |v| v,
        );
        match supervisor_in
            .recv_timeout(Duration::from_secs(4))
            .map_err(|e| e.to_string())?
        {
            Inbound::Frame(e) if matches!(e.message, Message::Ready(_)) => {}
            other => return Err(format!("device startup failed: {other:?}")),
        }
        let mut conn = Connection::new(
            socket::connect(&dir.join("device.sock"), Duration::from_secs(2))
                .map_err(|e| e.to_string())?,
            "owned-host",
            clock,
        )
        .map_err(|e| e.to_string())?;
        send_hello(&mut conn, make_hello(PeerRole::Host, &token, epoch, clock))
            .map_err(|e| e.to_string())?;
        let manifest = read_ready(&mut conn)
            .map_err(|e| e.to_string())?
            .capabilities
            .ok_or("missing device manifest")?;
        if observing && manifest.input != CapabilityState::Unavailable {
            return Err("recovery observer unexpectedly permits input".into());
        }
        let identity = manifest.identity;
        let (tx, host_in) = mpsc::channel();
        spawn_reader("owned-device-host", conn.reader, tx, |v| v);
        Ok((
            Self {
                child: starting.child.take().unwrap(),
                dir,
                supervisor,
                supervisor_in,
                host: conn.link,
                host_in,
                clock,
                next_lease: Instant::now(),
                epoch,
                pending_error: None,
            },
            identity,
        ))
    }
    fn renew(&mut self) -> Result<(), String> {
        if Instant::now() >= self.next_lease {
            let lease = Message::LeaseRenew(LeaseRenew {
                lease_epoch: self.epoch,
                valid_for_ms: 750,
            });
            self.supervisor
                .send(lease.clone())
                .map_err(|e| e.to_string())?;
            self.host.send(lease).map_err(|e| e.to_string())?;
            self.next_lease = Instant::now() + Duration::from_millis(100);
        }
        Ok(())
    }
    pub fn pump(&mut self) -> Result<Vec<Message>, String> {
        if let Some(error) = self.pending_error.take() {
            return Err(error);
        }
        let mut error = self.renew().err();
        let mut messages = Vec::new();
        for rx in [&self.supervisor_in, &self.host_in] {
            loop {
                match rx.try_recv() {
                    Ok(Inbound::Frame(e)) => messages.push(e.message),
                    Ok(other) => {
                        error.get_or_insert_with(|| format!("device link closed: {other:?}"));
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        error.get_or_insert_with(|| "device link reader disconnected".into());
                        break;
                    }
                }
            }
        }
        // A disconnect must not discard terminal receipts already read in
        // this batch. Deliver the frames before reporting the closed link.
        if messages.is_empty() {
            if let Some(error) = error {
                return Err(error);
            }
        } else {
            self.pending_error = error;
        }
        Ok(messages)
    }
    #[allow(
        dead_code,
        reason = "Shared example support; only the death observer waits for SIGKILL"
    )]
    pub fn wait_killed(&mut self) -> Result<bool, String> {
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                use std::os::unix::process::ExitStatusExt;
                return Ok(status.signal() == Some(9));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(false)
    }
    pub fn wait(&mut self, test: impl Fn(&Message) -> bool) -> Result<Message, String> {
        let end = Instant::now() + Duration::from_secs(4);
        loop {
            for m in self.pump()? {
                if test(&m) {
                    return Ok(m);
                }
            }
            if Instant::now() >= end {
                return Err("device response timed out".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    pub fn send_ack(&mut self, message: Message) -> Result<(), String> {
        let id = self.host.send(message).map_err(|e| e.to_string())?;
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            self.renew()?;
            if let Ok(Inbound::Frame(e)) = self.host_in.recv_timeout(Duration::from_millis(2))
                && e.in_reply_to.as_ref() == Some(&id)
            {
                return match e.message {
                    Message::CommandAck(ack) if ack.accepted => Ok(()),
                    other => Err(format!("request refused: {other:?}")),
                };
            }
            if Instant::now() >= end {
                return Err("command acknowledgement timed out".into());
            }
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.supervisor.send(Message::Shutdown(ShutdownMessage {
            reason: "owned native test finished".into(),
        }));
        let _ = self.host.get_ref().shutdown(Shutdown::Both);
        let end = Instant::now() + Duration::from_millis(800);
        while Instant::now() < end {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
