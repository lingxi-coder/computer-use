//! The loop across real processes: cu-host, cu-device and cu-source started by
//! a supervisor that plays Electron main, talking over private sockets and
//! pipes. The application and the models are simulated; everything else is what
//! the product does.

use std::os::unix::net::UnixStream;
use std::time::Duration;

use desktop_ipc::handshake::{Connection, make_hello, read_ready, send_hello};
use desktop_ipc::socket;
use desktop_supervisor::{Options, Role, Session};
use interaction_contracts::desktop::device::{StopDisposition, StopScope, StopSource};
use interaction_contracts::desktop::handshake::PeerRole;
use interaction_contracts::desktop::ids::RootTaskId;
use interaction_contracts::desktop::operator::{OperatorCommand, StopStatus};
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
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
    hello.protocol_min = 2;
    hello.protocol_max = 3;
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
    assert_eq!(ack.negotiated_version, 1);
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

    // The right switch, set once, through the accessibility action, then the page left and entered.
    let entries = session.world_log().entries().unwrap();
    assert_eq!(
        inputs(&entries),
        vec![set_off(), press(), press()],
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
        s.pending_confirmation().is_some()
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
    session.wait_for("the task to finish", PATIENCE, |s| {
        s.root_state().is_some_and(RootState::is_terminal)
    });
    assert_eq!(
        session.root_state(),
        Some(RootState::Completed),
        "{}",
        session.report()
    );
    let world = world_of(&session);
    assert_eq!(world.app.save_state(), SaveState::Saved);
    assert_eq!(world.app.modal(), None);
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
