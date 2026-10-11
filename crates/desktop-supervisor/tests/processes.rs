//! The loop across real processes: cu-host, cu-device and cu-source started by
//! a supervisor that plays Electron main, talking over private sockets and
//! pipes. The application and the models are simulated; everything else is what
//! the product does.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use desktop_ipc::SharedClock;
use desktop_ipc::handshake::{Connection, make_hello, read_ready, send_hello};
use desktop_ipc::link::{FramedReader, Inbound, Link, spawn_reader};
use desktop_ipc::socket;
use desktop_supervisor::{Options, Role, Session, binaries};
use interaction_contracts::desktop::PROTOCOL_VERSION;
use interaction_contracts::desktop::device::{
    ExecutionIntent, ExecutionObservation, LeaseRenew, StopCommand, StopDisposition, StopScope,
    StopSource,
};
use interaction_contracts::desktop::handshake::{Configure, PeerRole};
use interaction_contracts::desktop::ids::RootTaskId;
use interaction_contracts::desktop::message::Message;
use interaction_contracts::desktop::operator::{OperatorCommand, StopStatus};
use interaction_contracts::desktop::primitive::{AxActionKind, Modifiers, MouseButton, Primitive};
use interaction_contracts::desktop::task::RootState;
use sim_world::app::{Modal, SaveState};
use sim_world::world_log::{ModalKind, WorldEntry, WorldFollower};

const TEXT: &str = r#"Turn off "Daily picks" in "Push""#;
const TEXT_AMBIGUOUS: &str = r#"Turn off "Daily picks""#;
const PATIENCE: Duration = Duration::from_secs(20);

fn root1() -> RootTaskId {
    RootTaskId::new("root-1").unwrap()
}

fn inputs(entries: &[WorldEntry]) -> Vec<Primitive> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            WorldEntry::Input { primitive, .. } => Some(primitive.clone()),
            _ => None,
        })
        .collect()
}

fn set_off() -> Primitive {
    Primitive::AxAction {
        action: AxActionKind::SetValue {
            value: "off".into(),
        },
    }
}

fn press() -> Primitive {
    Primitive::AxAction {
        action: AxActionKind::Press,
    }
}

fn click_at(x: i32, y: i32) -> Primitive {
    Primitive::Click {
        button: MouseButton::Left,
        x,
        y,
        modifiers: Modifiers::default(),
    }
}

/// The application as the log leaves it.
fn world_of(session: &Session) -> WorldFollower {
    let mut follower = WorldFollower::new(session.world_log().path());
    follower.catch_up().unwrap();
    follower
}

#[test]
fn the_device_takes_only_a_host_with_the_right_role_token_epoch_and_version() {
    let session = Session::start_device_and_source();
    let clock = session.clock();
    let connect = || {
        let stream = socket::connect(&session.device_socket(), Duration::from_secs(2)).unwrap();
        Connection::new(stream, "test", clock).unwrap()
    };

    // A stranger with a guessed token is refused, and told why.
    let mut conn = connect();
    let error = send_hello(
        &mut conn,
        make_hello(
            PeerRole::Host,
            "00000000000000000000000000000000",
            session.epoch(),
            clock,
        ),
    )
    .unwrap_err();
    assert!(error.to_string().contains("token"), "{error}");

    // The right token in the wrong role, for the wrong epoch, or speaking no common version.
    let mut conn = connect();
    let error = send_hello(
        &mut conn,
        make_hello(
            PeerRole::Source,
            session.device_token(),
            session.epoch(),
            clock,
        ),
    )
    .unwrap_err();
    assert!(error.to_string().contains("role"), "{error}");
    let mut conn = connect();
    let error = send_hello(
        &mut conn,
        make_hello(
            PeerRole::Host,
            session.device_token(),
            session.epoch() + 5,
            clock,
        ),
    )
    .unwrap_err();
    assert!(error.to_string().contains("epoch"), "{error}");
    let mut conn = connect();
    let mut hello = make_hello(
        PeerRole::Host,
        session.device_token(),
        session.epoch(),
        clock,
    );
    hello.protocol_min = PROTOCOL_VERSION + 1;
    hello.protocol_max = PROTOCOL_VERSION + 1;
    let error = send_hello(&mut conn, hello).unwrap_err();
    assert!(
        error.to_string().contains("no common protocol version"),
        "{error}"
    );

    // The right one is acknowledged, and the device says what it can really do.
    let mut conn = connect();
    let ack = send_hello(
        &mut conn,
        make_hello(
            PeerRole::Host,
            session.device_token(),
            session.epoch(),
            clock,
        ),
    )
    .unwrap();
    assert_eq!(ack.negotiated_version, PROTOCOL_VERSION);
    let ready = read_ready(&mut conn).unwrap();
    let manifest = ready
        .capabilities
        .expect("a device reports its capabilities");
    assert_eq!(
        manifest.backend,
        interaction_contracts::desktop::device::BackendKind::Simulated,
        "a simulated device says so"
    );
    assert_eq!(manifest.identity.session_epoch, session.epoch());
    drop(UnixStream::connect(session.device_socket()));
}

#[test]
fn a_task_completes_across_three_processes() {
    let mut session = Session::start(&Options::default());
    session.start_task(TEXT);
    session.wait_for("the task to finish", PATIENCE, |s| {
        s.root_state().is_some_and(RootState::is_terminal)
    });
    assert_eq!(
        session.root_state(),
        Some(RootState::Completed),
        "{}",
        session.report()
    );

    // The right switch is set once; navigation uses the two exact tab clicks.
    let entries = session.world_log().entries().unwrap();
    assert_eq!(
        inputs(&entries),
        vec![set_off(), click_at(70, 52), click_at(70, 84)],
        "{}",
        session.report()
    );
    let world = world_of(&session);
    assert_eq!(world.app.switch_on("Push", "Daily picks"), Some(false));
    assert_eq!(world.app.switch_on("Email", "Daily picks"), Some(true));

    // The operator's view says what the device can do and how the task went.
    let ui = session.ui.clone().unwrap();
    assert_eq!(ui.root.as_ref().unwrap().attempts_used, 2);
    assert!(ui.capabilities.is_some());
    assert!(
        session.decisions.is_empty(),
        "a plan with one clear way needs no model"
    );
}

#[test]
fn a_stale_lease_epoch_cannot_extend_the_current_device_session() {
    let mut session = Session::start_device_and_source();
    let stream = socket::connect(&session.device_socket(), Duration::from_secs(2)).unwrap();
    let mut host = Connection::new(stream, "host", session.clock()).unwrap();
    send_hello(
        &mut host,
        make_hello(
            PeerRole::Host,
            session.device_token(),
            session.epoch(),
            session.clock(),
        ),
    )
    .unwrap();
    read_ready(&mut host).unwrap();
    host.link
        .send(Message::LeaseRenew(LeaseRenew {
            lease_epoch: session.epoch(),
            valid_for_ms: 100,
        }))
        .unwrap();
    let reply = host.reader.read_next().unwrap().unwrap();
    assert!(matches!(reply.message, Message::CommandAck(ack) if ack.accepted));
    host.link
        .send(Message::LeaseRenew(LeaseRenew {
            lease_epoch: session.epoch() + 1,
            valid_for_ms: 60_000,
        }))
        .unwrap();
    let reply = host.reader.read_next().unwrap().unwrap();
    assert!(matches!(reply.message, Message::CommandAck(ack) if !ack.accepted));
    session.wait_for(
        "the original lease still expires",
        Duration::from_secs(2),
        |s| {
            s.device_events.iter().any(|event| {
                matches!(
                    event.event,
                    interaction_contracts::desktop::device::DeviceEventKind::LeaseLost
                )
            })
        },
    );
}

#[test]
fn an_ambiguous_target_is_relayed_to_the_decision_worker_through_main() {
    let mut session = Session::start(&Options::default());
    session.teacher.prefer_word = Some("Push".into());
    session.start_task(TEXT_AMBIGUOUS);
    session.wait_for("the task to finish", PATIENCE, |s| {
        s.root_state().is_some_and(RootState::is_terminal)
    });
    assert_eq!(
        session.root_state(),
        Some(RootState::Completed),
        "{}",
        session.report()
    );
    assert_eq!(session.decisions.len(), 1, "one question crossed the relay");
    let world = world_of(&session);
    assert_eq!(world.app.switch_on("Push", "Daily picks"), Some(false));
    assert_eq!(world.app.switch_on("Email", "Daily picks"), Some(true));
}

#[test]
fn a_dialog_in_front_of_the_window_needs_the_users_word_across_the_processes_too() {
    let mut session = Session::start(&Options::default());
    session.start_task(TEXT);
    session.wait_for("the switch to be set", PATIENCE, |s| !s.inputs().is_empty());
    session.append_world(WorldEntry::Modal {
        modal: Some(ModalKind::UnsavedChanges),
    });
    session.wait_for("a confirmation", PATIENCE, |s| {
        s.root_state() == Some(RootState::NeedsInput)
            && s.pending_confirmation().is_some_and(|confirmation| {
                matches!(
                    confirmation.kind,
                    interaction_contracts::desktop::operator::ConfirmationKind::Effect { .. }
                )
            })
    });
    assert_eq!(
        session.root_state(),
        Some(RootState::NeedsInput),
        "{}",
        session.report()
    );
    let world = world_of(&session);
    assert_eq!(world.app.modal(), Some(Modal::UnsavedChanges));
    assert_eq!(
        world.app.save_state(),
        SaveState::Unsaved,
        "Apply has not been pressed"
    );

    session.answer(true);
    session.wait_for(
        "the confirmed action to finish or await reconciliation",
        PATIENCE,
        |s| {
            s.root_state()
                .is_some_and(|state| state.is_terminal() || state == RootState::Suspended)
        },
    );
    let applied = session
        .inputs()
        .into_iter()
        .filter_map(|entry| match entry {
            WorldEntry::Input {
                primitive,
                binding_fingerprint,
                ..
            } if primitive == press() => Some(binding_fingerprint),
            _ => None,
        })
        .collect::<Vec<_>>();
    let apply_fingerprint = applied
        .first()
        .expect("Apply reached the application")
        .clone();
    assert!(
        apply_fingerprint.is_some(),
        "Apply has a target binding proof"
    );
    let saved = world_of(&session);
    assert_eq!(saved.app.save_state(), SaveState::Saved);
    assert_eq!(saved.app.modal(), None);
    let mut reconciled_attempt = None;
    if session.root_state() == Some(RootState::Suspended) {
        assert_eq!(applied.len(), 1, "only Apply was pressed before suspension");
        // Removing the dialog can revoke its layer permit before the bounded
        // command window ends. The submitted Press is then mechanically
        // uncertain; only an explicit resume queries that same intent.
        let db = rusqlite::Connection::open(session.db_path()).unwrap();
        let (attempt, intent, refunded): (String, String, bool) = db.query_row(
            "SELECT attempt_id, intent_id, refunded FROM attempts WHERE state='unknown' AND skill='ui.activate'", [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert!(!refunded, "a submitted unknown keeps its quota");
        assert_eq!(
            session
                .ui
                .as_ref()
                .unwrap()
                .root
                .as_ref()
                .unwrap()
                .attempts_used,
            2
        );
        session.pump(Duration::from_millis(150));
        assert_eq!(
            session.inputs().len(),
            2,
            "no input follows an uncertain Apply"
        );
        session.operator(OperatorCommand::ResumeTask {
            root_task_id: root1(),
        });
        session.wait_for(
            "receipt reconciliation and fresh goal verification",
            PATIENCE,
            |s| s.root_state().is_some_and(RootState::is_terminal),
        );
        assert!(
            session.ui.as_ref().unwrap().timeline.iter().any(|entry| {
                entry
                    .text
                    .contains(&format!("the device confirms {intent} ended as"))
            }),
            "the original intent is queried: {}",
            session.report()
        );
        reconciled_attempt = Some(attempt);
    }
    assert_eq!(
        session.root_state(),
        Some(RootState::Completed),
        "{}",
        session.report()
    );
    let world = world_of(&session);
    assert_eq!(world.app.save_state(), SaveState::Saved);
    assert_eq!(world.app.modal(), None);
    let repeated_apply = session
        .inputs()
        .iter()
        .filter(|entry| {
            matches!(entry,
                WorldEntry::Input { primitive, binding_fingerprint, .. }
                    if *primitive == press() && *binding_fingerprint == apply_fingerprint
            )
        })
        .count();
    assert_eq!(repeated_apply, 1, "revalidation never repeats Apply");
    if let Some(attempt) = reconciled_attempt {
        let db = rusqlite::Connection::open(session.db_path()).unwrap();
        let (state, refunded): (String, bool) = db
            .query_row(
                "SELECT state, refunded FROM attempts WHERE attempt_id=?1",
                [attempt],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "settled");
        assert!(!refunded, "the reconciled Apply keeps its spent quota");
    }
}

#[test]
fn a_stop_sent_to_the_device_before_the_host_looks_beats_the_host() {
    let mut session = Session::start(&Options::default());
    session.create_task(TEXT);
    session.wait_for("the goal", PATIENCE, |s| s.pending_confirmation().is_some());

    // The host stalls. The operator confirms the goal and then hits stop: the
    // confirmation waits in the host's pipe, the stop goes straight to the device.
    session.freeze_host();
    session.answer(true);
    session.stop_at_device(StopScope::All, StopDisposition::Cancel);
    session.wait_for("the device's acknowledgement", PATIENCE, |s| {
        !s.stop_acks.is_empty()
    });
    let ack = session.stop_acks[0].clone();
    assert_eq!(ack.stop_seq, 1);
    assert_eq!(ack.source, StopSource::OperatorButton);
    session.pump(Duration::from_millis(300));

    // The host wakes up with a stale view: a confirmed goal, evidence, and
    // everything it needs to act. The device refuses every request it makes,
    // and the host learns of the stop from the device.
    session.thaw_host();
    session.wait_for("the task to end", PATIENCE, |s| {
        s.root_state().is_some_and(RootState::is_terminal)
    });
    assert_eq!(
        session.root_state(),
        Some(RootState::Cancelled),
        "{}",
        session.report()
    );
    assert!(
        inputs(&session.world_log().entries().unwrap()).is_empty(),
        "no input reached the application: {}",
        session.report()
    );
    assert_eq!(
        session.ui.as_ref().unwrap().stop_status,
        StopStatus::StoppedConfirmed
    );
}

#[test]
fn a_person_at_the_keyboard_pauses_the_task_across_the_processes_and_resume_continues_it() {
    let mut session = Session::start(&Options::default());
    session.start_task(TEXT);
    session.wait_for("the switch to be set", PATIENCE, |s| !s.inputs().is_empty());
    session.inject_human_input();
    session.wait_for("the task to be suspended", PATIENCE, |s| {
        s.root_state() == Some(RootState::Suspended)
    });
    assert_eq!(
        session.ui.as_ref().unwrap().stop_status,
        StopStatus::StoppedConfirmed
    );
    let so_far = session.inputs().len();
    session.pump(Duration::from_millis(800));
    assert_eq!(
        session.inputs().len(),
        so_far,
        "nothing moves while suspended"
    );

    session.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });
    session.wait_for("the task to finish", PATIENCE, |s| {
        s.root_state().is_some_and(RootState::is_terminal)
    });
    assert_eq!(
        session.root_state(),
        Some(RootState::Completed),
        "{}",
        session.report()
    );
    let sets = inputs(&session.world_log().entries().unwrap())
        .into_iter()
        .filter(|p| *p == set_off())
        .count();
    assert_eq!(sets, 1, "the switch was set once: {}", session.report());
}

#[test]
fn a_killed_device_suspends_the_task_and_its_replacement_goes_on_only_when_the_operator_says_so() {
    let mut session = Session::start(&Options::default());
    session.start_task(TEXT);
    session.wait_for("the switch to be set", PATIENCE, |s| !s.inputs().is_empty());
    session.kill_device();

    // The host sees the connection drop. Nothing can confirm a release, so the
    // task is suspended with its cleanup unknown.
    session.wait_for("the task to be suspended", PATIENCE, |s| {
        s.root_state() == Some(RootState::Suspended)
    });
    let ui = session.ui.clone().unwrap();
    assert_eq!(
        ui.stop_status,
        StopStatus::StopUnconfirmed,
        "{}",
        session.report()
    );
    assert_eq!(
        ui.cleanup,
        interaction_contracts::desktop::device::ReleaseStatus::Unknown
    );
    let so_far = session.inputs().len();

    // A new device, at a new epoch, is not trusted with the old task by itself.
    session.restart_device();
    session.wait_for("the new device's capabilities", PATIENCE, |s| {
        s.ui.as_ref()
            .and_then(|ui| ui.capabilities.as_ref())
            .is_some_and(|c| c.identity.session_epoch == 2)
    });
    session.pump(Duration::from_millis(800));
    assert_eq!(session.root_state(), Some(RootState::Suspended));
    assert_eq!(
        session.inputs().len(),
        so_far,
        "the new device took no input on its own"
    );

    session.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });
    session.wait_for("the task to finish", PATIENCE, |s| {
        s.root_state().is_some_and(RootState::is_terminal)
    });
    assert_eq!(
        session.root_state(),
        Some(RootState::Completed),
        "{}",
        session.report()
    );
    let sets = inputs(&session.world_log().entries().unwrap())
        .into_iter()
        .filter(|p| *p == set_off())
        .count();
    assert_eq!(sets, 1, "the switch was set once: {}", session.report());
}

#[test]
fn when_main_goes_away_the_device_closes_its_gate_and_every_process_leaves() {
    let mut session = Session::start(&Options::default());
    session.pump(Duration::from_millis(300));
    session.close_channels();

    // The device latched a stop on its own before it left, and said so.
    session.wait_for("the device's own stop", PATIENCE, |s| {
        s.stop_acks.iter().any(|ack| {
            ack.source == StopSource::Supervisor && ack.disposition == StopDisposition::Cancel
        })
    });
    for role in [Role::Device, Role::Source, Role::Host] {
        let status = session
            .wait_exit(role, Duration::from_secs(10))
            .unwrap_or_else(|| panic!("{role:?} did not leave\n{}", session.report()));
        assert!(
            status.success(),
            "{role:?}: {status:?}\n{}",
            session.report()
        );
    }
}

#[test]
fn an_idle_source_connection_survives_the_handshake_deadline() {
    let mut session = Session::start(&Options::default());
    session.pump(Duration::from_millis(5_500));
    session.start_task(TEXT);
    session.wait_for("the task after idle to finish", PATIENCE, |s| {
        s.root_state().is_some_and(RootState::is_terminal)
    });
    assert_eq!(
        session.root_state(),
        Some(RootState::Completed),
        "{}",
        session.report()
    );
}

#[test]
fn an_unread_host_cannot_delay_direct_stop_or_keep_its_gate_open() {
    let mut session = Session::start_device_and_source();
    let stream = socket::connect(&session.device_socket(), Duration::from_secs(2)).unwrap();
    let mut host = Connection::new(stream, "host", session.clock()).unwrap();
    send_hello(
        &mut host,
        make_hello(
            PeerRole::Host,
            session.device_token(),
            session.epoch(),
            session.clock(),
        ),
    )
    .unwrap();
    read_ready(&mut host).unwrap();
    // Valid requests fill the reply stream. No host reader consumes replies.
    for _ in 0..500 {
        if host
            .link
            .send(Message::LeaseRenew(LeaseRenew {
                lease_epoch: 1,
                valid_for_ms: 5_000,
            }))
            .is_err()
        {
            break;
        }
    }
    session.stop_at_device(StopScope::All, StopDisposition::Cancel);
    session.wait_for(
        "a direct stop despite the unread host",
        Duration::from_secs(2),
        |s| {
            s.stop_acks
                .iter()
                .any(|ack| ack.source == StopSource::OperatorButton)
        },
    );
    assert!(
        session
            .stop_acks
            .iter()
            .any(|ack| ack.source == StopSource::HostLeaseLost),
        "{}",
        session.report()
    );
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn an_unread_supervisor_output_cannot_block_device_stops_or_shutdown() {
    let dir = tempfile::Builder::new()
        .prefix("cu-output-")
        .tempdir()
        .unwrap();
    let clock = SharedClock::starting_now();
    let token = socket::new_token().unwrap();
    let mut child = ChildGuard(
        Command::new(&binaries().device)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut supervisor = Link::new(child.0.stdin.take().unwrap(), "supervisor", clock);
    let mut stdout = FramedReader::new(child.0.stdout.take().unwrap());
    supervisor
        .send(Message::Configure(Configure {
            role: PeerRole::Device,
            session_epoch: 1,
            settings: BTreeMap::from([
                ("socket_dir".into(), dir.path().display().to_string()),
                ("host_token".into(), token.clone()),
                ("clock_origin_ms".into(), clock.origin_unix_ms().to_string()),
                (
                    "world_log".into(),
                    dir.path().join("world.log").display().to_string(),
                ),
            ]),
            browser_page_profile: None,
        }))
        .unwrap();
    assert!(matches!(
        stdout.read_next().unwrap().unwrap().message,
        Message::Ready(_)
    ));
    // Keep stdout open without consuming it, while the host continues reading.
    let stream = socket::connect(&dir.path().join("device.sock"), Duration::from_secs(2)).unwrap();
    let mut host = Connection::new(stream, "host", clock).unwrap();
    send_hello(&mut host, make_hello(PeerRole::Host, &token, 1, clock)).unwrap();
    read_ready(&mut host).unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = spawn_reader("stop-output-test", host.reader, tx, |inbound| inbound);
    let requests = std::thread::spawn(move || {
        for _ in 0..1_000 {
            if supervisor
                .send(Message::StopCommand(StopCommand {
                    scope: StopScope::All,
                    disposition: StopDisposition::Cancel,
                    source: StopSource::OperatorButton,
                    reason: "unread supervisor output".into(),
                }))
                .is_err()
            {
                break;
            }
            // Give the responsive host's worker time to drain; only the
            // supervisor's deliberately unread output should fill up here.
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "device blocked on its unread supervisor output"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "{status:?}");
    requests.join().unwrap();
    reader.join().unwrap();
    assert!(rx.try_iter().any(|inbound| matches!(inbound, Inbound::Frame(envelope)
        if matches!(envelope.message, Message::DeviceEvent(ref event)
            if matches!(event.event, interaction_contracts::desktop::device::DeviceEventKind::Stopped { .. }))
    )), "the host should see stops while supervisor output is stalled");
}

#[test]
fn a_real_device_rejects_identical_digest_with_a_different_source_instance() {
    use interaction_contracts::desktop::{
        device::{GrantOutcome, GrantPermit, ReceiptStatus, RootGrant, SubmitIntent},
        ids::ObjectRef,
        samples,
    };
    let session = Session::start_device_and_source();
    let stream = socket::connect(&session.device_socket(), Duration::from_secs(2)).unwrap();
    let mut host = Connection::new(stream, "host", session.clock()).unwrap();
    send_hello(
        &mut host,
        make_hello(
            PeerRole::Host,
            session.device_token(),
            session.epoch(),
            session.clock(),
        ),
    )
    .unwrap();
    let identity = read_ready(&mut host)
        .unwrap()
        .capabilities
        .unwrap()
        .identity;
    host.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    fn exchange(host: &mut Connection, message: Message) -> Message {
        let id = host.link.send(message).unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        for _ in 0..32 {
            assert!(Instant::now() < deadline, "bounded device reply");
            let envelope = host.reader.read_next().unwrap().unwrap();
            if envelope.in_reply_to.as_ref() == Some(&id) {
                return envelope.message;
            }
        }
        panic!("no correlated device reply");
    }
    assert!(
        matches!(exchange(&mut host, Message::LeaseRenew(LeaseRenew {
        lease_epoch: session.epoch(), valid_for_ms: 10_000 })), Message::CommandAck(ack) if ack.accepted)
    );
    let now = session.clock().now_ms();
    assert!(
        matches!(exchange(&mut host, Message::RegisterRoot(RootGrant {
        root_id: "root-1".into(), revision: 1, root_epoch: 1, deadline_ms: now + 10_000
    })), Message::CommandAck(ack) if ack.accepted)
    );
    let messages = samples::all();
    let mut observation = messages
        .iter()
        .find_map(|(_, envelope)| match &envelope.message {
            Message::Observation(ExecutionObservation::Native { observation }) => {
                Some(observation.as_ref().clone())
            }
            _ => None,
        })
        .unwrap();
    observation.session_epoch = identity.session_epoch;
    observation.surface = identity.surface.clone();
    observation.clock_domain = identity.clock_domain;
    observation.dependencies = identity.dependencies;
    observation.observed_ms = now;
    observation.valid_until_ms = now + 8_000;
    assert!(
        matches!(exchange(&mut host, Message::Observation(ExecutionObservation::Native {
            observation: Box::new(observation.clone()),
        })), Message::CommandAck(ack) if ack.accepted)
    );
    let grant = exchange(
        &mut host,
        Message::GrantPermit(GrantPermit {
            acked_stop_seq: 0,
            task_id: "root-1".into(),
            target_ref: observation.target_ref.clone(),
            evidence_id: observation.observation_id.clone(),
            valid_until_ms: now + 5_000,
        }),
    );
    let Message::GrantOutcome(GrantOutcome::Granted { permit }) = grant else {
        panic!("{grant:?}");
    };
    let mut intent = messages
        .iter()
        .find_map(|(_, envelope)| match &envelope.message {
            Message::SubmitIntent(request) => match &request.intent {
                ExecutionIntent::Native { intent } => Some(intent.as_ref().clone()),
                ExecutionIntent::Browser { .. } => None,
            },
            _ => None,
        })
        .unwrap();
    intent.permit = permit;
    intent.session_epoch = observation.session_epoch;
    intent.surface = observation.surface.clone();
    intent.dependencies = observation.dependencies;
    intent.latest_start_ms = now + 1_000;
    let digest = intent.binding_fingerprint.clone();
    intent.binding_proof.as_mut().unwrap().instance =
        ObjectRef::new("identical-replacement").unwrap();
    intent.validate().unwrap(); // Digest and descriptor match; the source identity does not.
    assert_eq!(intent.binding_fingerprint, digest);
    let rejected = exchange(
        &mut host,
        Message::SubmitIntent(SubmitIntent {
            acked_stop_seq: 0,
            intent: ExecutionIntent::Native {
                intent: Box::new(intent),
            },
        }),
    );
    let Message::Receipt(receipt) = rejected else {
        panic!("{rejected:?}");
    };
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert!(receipt.submitted.is_empty());
    assert!(receipt.reason.unwrap().contains("immutable issue evidence"));
    assert!(inputs(&session.world_log().entries().unwrap()).is_empty());
}
