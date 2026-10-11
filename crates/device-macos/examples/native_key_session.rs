//! Owned native keyboard acceptance. Source facts come from the production
//! watch and binder; a single real cu-device submits through its native backend.
use interaction_contracts::desktop::{
    binding::LayoutIndependentSpecialKey,
    device::*,
    ids::SemanticTargetRef,
    message::Message,
    primitive::{Modifiers, Primitive},
    source::{FrameMeta, FrameStatus, SourceHealth, StructureSnapshot},
};
use interaction_engine::{
    environment::{binding::BindingReducer, pack::AppPack, state_core::StateCore},
    execution::DeviceView,
};
use serde::{Deserialize, Serialize};
use source_macos::{
    config::{NativeCaptureMode, NativeConfig, TextReadPolicy},
    native::watch::NativeWatch,
};
use std::{
    io::{BufRead, Write},
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FixtureScope {
    pid: u32,
    bundle_id: String,
    window_id: String,
    label: String,
    #[serde(default = "default_epoch")]
    session_epoch: u64,
}
fn default_epoch() -> u64 {
    7
}

mod session {
    use desktop_ipc::{
        SharedClock,
        handshake::{Connection, make_hello, read_ready, send_hello},
        link::{FramedReader, Inbound, Link, spawn_reader},
        socket,
    };
    use interaction_contracts::desktop::{
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
        pub fn device_pid(&self) -> u32 {
            self.child.id()
        }
        pub fn start(
            device: &str,
            helper: &str,
            scope: &super::FixtureScope,
        ) -> Result<(Self, DeviceIdentity), String> {
            let epoch = scope.session_epoch;
            let clock = SharedClock::starting_now();
            let dir = PathBuf::from("/private/tmp").join(format!(
                "cu-native-{}-{}",
                std::process::id(),
                clock.origin_unix_ms()
            ));
            socket::private_dir(&dir).map_err(|e| e.to_string())?;
            let diagnostics = dir.join("key-diagnostics");
            socket::private_dir(&diagnostics).map_err(|e| e.to_string())?;
            let token = socket::new_token().map_err(|e| e.to_string())?;
            let child = Command::new(device)
                .env("CU_OWNED_KEY_DIAGNOSTICS_DIR", &diagnostics)
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
            let mut supervisor =
                Link::new(child.stdin.take().unwrap(), "fixture-supervisor", clock);
            let mut settings = BTreeMap::new();
            for (k, v) in [
                ("socket_dir", dir.display().to_string()),
                ("host_token", token.clone()),
                ("clock_origin_ms", clock.origin_unix_ms().to_string()),
                ("world_log", dir.join("world.log").display().to_string()),
                ("backend", "macos-ax-press".into()),
                ("native_guard_path", helper.into()),
                ("native_target_pid", scope.pid.to_string()),
                ("native_target_bundle", scope.bundle_id.clone()),
                ("native_target_window_id", scope.window_id.clone()),
            ] {
                settings.insert(k.into(), v);
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
        pub fn send_ack(&mut self, message: Message) -> Result<Message, String> {
            let id = self.host.send(message).map_err(|e| e.to_string())?;
            let end = Instant::now() + Duration::from_secs(3);
            loop {
                self.renew()?;
                if let Ok(Inbound::Frame(e)) = self.host_in.recv_timeout(Duration::from_millis(2))
                    && e.in_reply_to.as_ref() == Some(&id)
                {
                    return match e.message {
                        Message::CommandAck(ack) if ack.accepted => Ok(Message::CommandAck(ack)),
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
}

fn emit(value: serde_json::Value) -> Result<(), String> {
    println!("{value}");
    std::io::stdout().flush().map_err(|e| e.to_string())
}

fn wait_control(
    session: &mut session::Session,
    controls: &mpsc::Receiver<String>,
    expected: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        match controls.try_recv() {
            Ok(line) => {
                let parsed = serde_json::from_str::<serde_json::Value>(&line).ok();
                let command = parsed
                    .as_ref()
                    .and_then(|v| v["command"].as_str())
                    .unwrap_or(line.trim());
                if command == expected {
                    return Ok(());
                }
                return Err(format!("expected control {expected}, got {command}"));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("controller closed before explicit stage release".into());
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        for message in session.pump()? {
            emit(serde_json::json!({"kind":"device_event", "message":message}))?;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "control {expected} timed out; no automatic submission"
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn read_source(
    session: &mut session::Session,
    watch: &mut NativeWatch,
    state: &mut StateCore,
    pack: &AppPack,
    scope: &FixtureScope,
) -> Result<(StructureSnapshot, FrameMeta, Vec<SourceHealth>), String> {
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut frame = None;
    let mut health_records = Vec::new();
    loop {
        for message in session.pump()? {
            emit(serde_json::json!({"kind":"device_event", "message":message}))?;
        }
        let now = session.clock.now_ms();
        for report in watch.take_reports() {
            for health in report.health {
                state
                    .note_health(health.clone(), now)
                    .map_err(|e| format!("source health rejected: {e:?}"))?;
                health_records.push(health);
                if health_records.len() > 64 {
                    health_records.remove(0);
                }
            }
            if let Some(snapshot) = report.snapshot {
                state
                    .admit(snapshot, now, pack)
                    .map_err(|e| format!("source snapshot rejected: {e}"))?;
            }
            if let Some(meta) = report.frame_meta {
                state
                    .admit_frame(meta.clone(), now, 1500)
                    .map_err(|e| format!("source frame rejected: {e:?}"))?;
                if meta.status == FrameStatus::Complete {
                    frame = Some(meta);
                }
            }
        }
        if state.usable(now, 1500)
            && state
                .frame_deadline(now, 1500)
                .is_some_and(|end| end >= now.saturating_add(600))
            && let Some(env) = state.latest()
            && env.snapshot.focus_context.is_some()
            && env.snapshot.app.pid == scope.pid
            && env.snapshot.app.bundle_id == scope.bundle_id
            && env
                .snapshot
                .global_focus
                .known_window_for_app(&env.snapshot.app)
                == Some(scope.window_id.as_str())
            && env.snapshot.observed_ms.saturating_add(1500) >= now.saturating_add(600)
        {
            return Ok((
                env.snapshot.clone(),
                frame.ok_or("source frame metadata missing")?,
                health_records,
            ));
        }
        if watch.ended() {
            return Err(
                "source watch ended before fresh positive focus evidence; no restart or input"
                    .into(),
            );
        }
        if Instant::now() >= deadline {
            emit_source_preparation_gate_diagnostic(state, scope, session.clock.now_ms(), &frame);
            return Err("fresh bounded Source focus/frame/health unavailable; no input".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn emit_source_preparation_gate_diagnostic(
    state: &StateCore,
    scope: &FixtureScope,
    now_ms: u64,
    latest_complete_frame: &Option<FrameMeta>,
) {
    let snapshot = state.latest().map(|env| &env.snapshot);
    let frame_view = state
        .observation_view(now_ms)
        .and_then(|observation| observation.frame);
    let frame_deadline_ms = state.frame_deadline(now_ms, 1500);
    let frame_remaining_ms = frame_deadline_ms.map(|deadline| deadline.saturating_sub(now_ms));
    let frame_margin_600ms =
        frame_deadline_ms.map(|deadline| deadline >= now_ms.saturating_add(600));
    let snapshot_scope_exact = snapshot.map(|snapshot| {
        snapshot.app.pid == scope.pid
            && snapshot.app.bundle_id == scope.bundle_id
            && snapshot.global_focus.known_window_for_app(&snapshot.app)
                == Some(scope.window_id.as_str())
    });
    let source_sequence = snapshot.map(|snapshot| snapshot.source_sequence);
    let snapshot_age_ms = snapshot.map(|snapshot| now_ms.saturating_sub(snapshot.observed_ms));
    let snapshot_complete = snapshot.map(|snapshot| snapshot.complete);
    let focus_present = snapshot.map(|snapshot| snapshot.focus_context.is_some());
    let latest_frame_ref_present = frame_view.as_ref().map(|frame| frame.frame_id.is_some());
    let heartbeat_available = frame_view.as_ref().map(|frame| frame.healthy);
    let frame_status = frame_view.as_ref().map(|frame| match frame.status {
        FrameStatus::Complete => "complete",
        FrameStatus::Idle => "idle",
        FrameStatus::Blank => "blank",
        FrameStatus::Suspended => "suspended",
        FrameStatus::Started => "started",
        FrameStatus::Stopped => "stopped",
    });

    let record = serde_json::json!({
        "kind": "source_preparation_gate_diagnostic",
        "now_ms": now_ms,
        "source_seq": source_sequence,
        "snapshot_age_ms": snapshot_age_ms,
        "complete": snapshot_complete,
        "focus_present": focus_present,
        "exact_scope": snapshot_scope_exact,
        "state_usable": state.usable(now_ms, 1500),
        "frame_status": frame_status,
        "actual_frame_source_ms": latest_complete_frame.as_ref().map(|frame| frame.source_ms),
        "actual_frame_source_seq": latest_complete_frame.as_ref().map(|frame| frame.source_sequence),
        "frame_deadline_ms": frame_deadline_ms,
        "frame_remaining_ms": frame_remaining_ms,
        "margin_600ms": frame_margin_600ms,
        "latest_frame_ref_present": latest_frame_ref_present,
        "heartbeat_available": heartbeat_available,
    });
    let Ok(encoded) = serde_json::to_vec(&record) else {
        return;
    };
    const PREFIX: &[u8] = b"CU_SOURCE_PREPARATION_GATE_DIAGNOSTIC ";
    const MAX_RECORD_BYTES: usize = 2048;
    if PREFIX.len() + encoded.len() + 1 > MAX_RECORD_BYTES {
        return;
    }
    let mut line = Vec::with_capacity(PREFIX.len() + encoded.len() + 1);
    line.extend_from_slice(PREFIX);
    line.extend_from_slice(&encoded);
    line.push(b'\n');
    let _ = std::io::stderr().lock().write_all(&line);
}

fn terminal(status: ReceiptStatus) -> bool {
    matches!(
        status,
        ReceiptStatus::Completed
            | ReceiptStatus::Unknown
            | ReceiptStatus::Rejected
            | ReceiptStatus::Partial
            | ReceiptStatus::Cancelled
    )
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 6 {
        return Err("expected DEVICE GUARD SOURCE FIXTURE_SCOPE_JSON SCENARIO".into());
    }
    let scenario = args[5].as_str();
    if ![
        "key-success",
        "key-unknown",
        "key-replay",
        "key-rejected",
        "key-unavailable",
        "key-death",
    ]
    .contains(&scenario)
    {
        return Err("unknown key acceptance scenario".into());
    }
    let scope: FixtureScope =
        serde_json::from_slice(&std::fs::read(&args[4]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if scope.pid == 0
        || scope.pid > i32::MAX as u32
        || scope.bundle_id != "com.lingxi.computer-user.guard-fixture"
        || scope.label.is_empty()
        || scope.label.len() > 512
        || scope.label.chars().any(char::is_control)
        || scope.session_epoch == 0
        || scope.session_epoch > 32
        || scope
            .window_id
            .strip_prefix("window-")
            .and_then(|id| id.parse::<u32>().ok())
            .is_none_or(|id| id == 0)
    {
        return Err("acceptance requires a bounded fixed owned fixture scope".into());
    }
    let (control_tx, controls) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if control_tx.send(line).is_err() {
                break;
            }
        }
    });
    let (mut session, identity) = session::Session::start(&args[1], &args[2], &scope)?;
    let Message::DeviceEvent(DeviceEvent { event: DeviceEventKind::CapabilityChanged { manifest }, .. }) = session.wait(|message| matches!(message, Message::DeviceEvent(DeviceEvent { event: DeviceEventKind::CapabilityChanged { manifest }, .. }) if manifest.input == CapabilityState::Available && manifest.hit_test == CapabilityState::Available))? else { unreachable!() };
    if identity.session_epoch != scope.session_epoch {
        return Err("device rejected the shared trusted session epoch".into());
    }
    let mut view = DeviceView::new();
    view.install_manifest(manifest.clone());
    view.note_lease(true);
    let action_duration_ms = manifest.max_uninterruptible_ms.min(500);
    if action_duration_ms == 0 {
        return Err("native manifest provides no atomic key duration bound".into());
    }
    let source_config = NativeConfig {
        table_scope: None,
        background_target: None,
        owned_document_bootstrap: None,
        helper_path: PathBuf::from(&args[3]),
        bundle_id: scope.bundle_id.clone(),
        pid: Some(scope.pid),
        assets_dir: session.dir.join("source-assets"),
        timeout_ms: 3000,
        mode: NativeCaptureMode::Watch,
        text_policy: Some(TextReadPolicy::RegisteredEditor {
            window_id: scope.window_id.clone(),
        }),
    };
    let mut watch = NativeWatch::new(
        source_config.clone(),
        scope.session_epoch,
        identity.clock_domain.clone(),
        session.clock,
        250,
    )?;
    let mut state = StateCore::new();
    state.set_identity(&identity, true);
    let pack = AppPack::observe_only(&scope.bundle_id);
    let mut isolated = false;
    let mut statuses = Vec::new();
    let mut final_receipt: Option<Receipt> = None;
    emit(
        serde_json::json!({"kind":"ready", "scope":scope, "device_identity":identity, "manifest":manifest,
        "source_assets_dir":source_config.assets_dir, "clock_origin_unix_ms":session.clock.origin_unix_ms(),
        "key_diagnostics_dir":session.dir.join("key-diagnostics"),
        "physical_input_submitted":false}),
    )?;
    let result = (|| -> Result<(), String> {
        let (snapshot, frame, health) =
            read_source(&mut session, &mut watch, &mut state, &pack, &scope)?;
        let env = state.latest().ok_or("current Source environment missing")?;
        let focus = snapshot
            .focus_context
            .as_ref()
            .ok_or("actual Source focus missing")?;
        let candidates: Vec<_> = snapshot
            .objects
            .iter()
            .filter(|object| {
                object.role == "AXCheckBox"
                    && object.title.as_deref() == Some(scope.label.as_str())
                    && object.window_id == scope.window_id
                    && object.enabled
                    && object.in_view
                    && !object.secure
            })
            .collect();
        if candidates.len() != 1
            || candidates[0].instance_ref != focus.focused_instance
            || focus.target_instance != focus.focused_instance
        {
            return Err("label must resolve uniquely to the actual Source focused checkbox".into());
        }
        let mut bindings = BindingReducer::new();
        let binding = bindings
            .bind_instance(
                env,
                SemanticTargetRef::new("owned-focused-checkbox").map_err(|e| e.to_string())?,
                &candidates[0].instance_ref,
            )
            .ok_or("actual focused target binding failed")?;
        let now = session.clock.now_ms();
        let root_deadline = now.saturating_add(5000);
        let frame_deadline = state
            .frame_deadline(now, 1500)
            .ok_or("bounded actual Source frame required")?;
        let evidence = view
            .bound_focused_observation(
                env,
                &binding,
                SourceFreshness::Verified,
                1500,
                root_deadline,
                Some(frame_deadline),
                LayoutIndependentSpecialKey::Space,
            )
            .map_err(str::to_string)?;
        evidence.validate()?;
        emit(
            serde_json::json!({"kind":"source_probe", "snapshot":snapshot, "frame":frame, "health":health,
        "binding": {"binding_ref":binding.binding_ref,"instance":binding.instance,"rule":binding.rule,"fingerprint_digest":binding.fingerprint_digest},
        "evidence":evidence, "separate_native_authority_probe":false, "physical_input_submitted":false}),
        )?;
        let root_ack = session.send_ack(Message::RegisterRoot(RootGrant {
            root_id: "owned-key-root".into(),
            revision: 1,
            root_epoch: 1,
            deadline_ms: root_deadline,
        }))?;
        let observation_ack =
            session.send_ack(Message::Observation(ExecutionObservation::Native {
                observation: Box::new(evidence.clone()),
            }))?;
        emit(
            serde_json::json!({"kind":"admission", "root_ack":root_ack, "observation_ack":observation_ack}),
        )?;
        session
            .host
            .send(Message::GrantPermit(GrantPermit {
                acked_stop_seq: 0,
                task_id: "owned-key-root".into(),
                target_ref: evidence.target_ref.clone(),
                evidence_id: evidence.observation_id.clone(),
                valid_until_ms: evidence.valid_until_ms,
            }))
            .map_err(|e| e.to_string())?;
        let grant = session.wait(|m| matches!(m, Message::GrantOutcome(_)))?;
        emit(serde_json::json!({"kind":"permit", "message":grant}))?;
        let Message::GrantOutcome(GrantOutcome::Granted { permit }) = grant else {
            return Err("native focused permit rejected".into());
        };
        let intent = ActionIntent {
            intent_id: "owned-key-one".into(),
            attempt_id: Some("owned-key-attempt".into()),
            task_id: "owned-key-root".into(),
            task_revision: 1,
            root_epoch: 1,
            session_epoch: identity.session_epoch,
            execution_domain: evidence.execution_domain,
            permit,
            surface: identity.surface.clone(),
            target_ref: evidence.target_ref.clone(),
            binding_fingerprint: evidence
                .binding_proof
                .as_ref()
                .map(|proof| proof.fingerprint.digest()),
            owned_document_proof: None,
            binding_proof: evidence.binding_proof.clone(),
            coordinate_space: CoordinateSpace::DisplayPoints,
            dependencies: evidence.dependencies.clone(),
            evidence_id: evidence.observation_id.clone(),
            latest_start_ms: evidence
                .valid_until_ms
                .checked_sub(action_duration_ms)
                .ok_or("source authority too short")?,
            duration_ms: action_duration_ms,
            steps: vec![ScheduledPrimitive {
                offset_ms: 0,
                primitive: Primitive::KeyPress {
                    key: "Space".into(),
                    modifiers: Modifiers::default(),
                },
            }],
        };
        intent.validate()?;
        emit(
            serde_json::json!({"kind":"key_ready", "intent":ExecutionIntent::from(intent.clone()), "evidence":ExecutionObservation::from(evidence.clone()), "scope":scope, "now_ms":session.clock.now_ms(), "physical_input_submitted":false,
        "control_required":"submit", "source_assets_dir":source_config.assets_dir,
        "key_diagnostics_dir":session.dir.join("key-diagnostics"), "device_pid":session.device_pid()}),
        )?;
        wait_control(&mut session, &controls, "submit", Duration::from_secs(3))?;
        session
            .host
            .send(Message::SubmitIntent(SubmitIntent {
                acked_stop_seq: 0,
                intent: ExecutionIntent::Native {
                    intent: Box::new(intent.clone()),
                },
            }))
            .map_err(|e| e.to_string())?;
        emit(serde_json::json!({"kind":"device_request_enqueued", "intent_id":intent.intent_id}))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let messages = match session.pump() {
                Ok(messages) => messages,
                Err(error) if scenario == "key-death" => {
                    if !session.wait_killed()? {
                        return Err(format!("expected owned cu-device SIGKILL: {error}"));
                    }
                    emit(
                        serde_json::json!({"kind":"death_observer_result", "device_sigkill_verified":true, "old_connection_closed":true,
                    "input_outcome":"unknown", "core_terminal_receipt":final_receipt, "observed_receipt_statuses":statuses,
                    "intent_id":intent.intent_id, "old_epoch":intent.session_epoch, "error":error, "input_replayed":false}),
                    )?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            for message in messages {
                if let Message::Receipt(receipt) = &message
                    && receipt.intent_id == intent.intent_id
                {
                    statuses.push(receipt.status);
                    if terminal(receipt.status) {
                        final_receipt = Some(receipt.clone());
                    }
                }
                if matches!(
                    message,
                    Message::DeviceEvent(DeviceEvent {
                        event: DeviceEventKind::ChannelIsolated { .. },
                        ..
                    })
                ) {
                    isolated = true;
                }
                emit(serde_json::json!({"kind":"core_message", "message":message}))?;
            }
            if scenario != "key-death"
                && final_receipt.is_some()
                && (scenario != "key-unknown" && scenario != "key-replay" || isolated)
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err("production key receipt/isolation missing".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let receipt = final_receipt
            .clone()
            .ok_or("terminal key receipt missing")?;
        receipt.validate()?;
        let expected = match scenario {
            "key-success" => ReceiptStatus::Completed,
            "key-rejected" | "key-unavailable" => ReceiptStatus::Rejected,
            _ => ReceiptStatus::Unknown,
        };
        if receipt.status != expected {
            return Err(format!(
                "unexpected key receipt {:?}: {:?}",
                receipt.status, receipt.reason
            ));
        }
        if receipt.status == ReceiptStatus::Completed
            && (receipt.submitted.len() != 1
                || receipt.submitted[0].native_primitive() != Some(&intent.steps[0].primitive)
                || receipt.submitted[0].cleanup()
                || receipt.input_end_ms.is_none())
        {
            return Err("key completion missing actual returned input evidence".into());
        }
        if receipt.status == ReceiptStatus::Rejected && !receipt.submitted.is_empty() {
            return Err("rejected input claimed submission".into());
        }
        if scenario == "key-replay" {
            session
                .host
                .send(Message::SubmitIntent(SubmitIntent {
                    acked_stop_seq: 0,
                    intent: ExecutionIntent::Native {
                        intent: Box::new(intent.clone()),
                    },
                }))
                .map_err(|e| e.to_string())?;
            let replay = session
                .wait(|m| matches!(m, Message::Receipt(r) if r.intent_id == intent.intent_id))?;
            emit(serde_json::json!({"kind":"replay_response", "message":replay}))?;
            if !matches!(&replay, Message::Receipt(r) if r == &receipt && r.status == ReceiptStatus::Unknown)
            {
                return Err("unknown intent was not preserved on duplicate submit".into());
            }
        }
        session
            .host
            .send(Message::QueryReceipt(QueryReceipt {
                session_epoch: intent.session_epoch,
                intent_id: intent.intent_id.clone(),
            }))
            .map_err(|e| e.to_string())?;
        let queried = session
            .wait(|m| matches!(m, Message::Receipt(r) if r.intent_id == intent.intent_id))?;
        if !matches!(&queried, Message::Receipt(r) if r == &receipt) {
            return Err("receipt query changed settled key outcome".into());
        }
        if std::fs::metadata(session.dir.join("world.log")).is_ok_and(|m| m.len() > 0) {
            return Err("keyboard acceptance routed through simulation".into());
        }
        emit(
            serde_json::json!({"kind":"key_session_result", "receipt":receipt, "query_response":queried,
        "channel_isolated":isolated, "observed_receipt_statuses":statuses, "actual_input_end_ms":receipt.input_end_ms,
        "input_end_ms":receipt.input_end_ms, "isolation":isolated, "no_replay":true,
        "duplicate_submit_tested":scenario == "key-replay", "input_replayed":false,
        "effect_success_claimed":false, "simulation_log_empty":true, "source_assets_dir":source_config.assets_dir,
        "key_diagnostics_dir":session.dir.join("key-diagnostics"),
        "control_required":"finish"}),
        )?;
        wait_control(&mut session, &controls, "finish", Duration::from_secs(5))?;
        Ok(())
    })();
    if let Err(error) = &result {
        // Keep this generation's private Source files alive until the driver
        // has copied them. No authority is renewed on this failure path.
        let _ = emit(
            serde_json::json!({"kind":"key_session_result", "error":error,
            "receipt":final_receipt, "input_end_ms":final_receipt.as_ref().and_then(|receipt| receipt.input_end_ms),
            "isolation":isolated, "channel_isolated":isolated, "observed_receipt_statuses":statuses, "no_replay":true,
            "effect_success_claimed":false, "source_assets_dir":source_config.assets_dir,
            "key_diagnostics_dir":session.dir.join("key-diagnostics"),
            "control_required":"finish"}),
        );
        let _ = controls.recv_timeout(Duration::from_secs(5));
    }
    result
}

fn main() -> Result<(), String> {
    match run() {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = emit(serde_json::json!({"kind":"error", "error":error}));
            Err(error)
        }
    }
}
