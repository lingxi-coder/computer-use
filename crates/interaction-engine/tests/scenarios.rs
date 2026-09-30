//! The closed loop, end to end, in a simulated world: a task goes from the
//! user's words to a confirmed goal, is admitted, acts on the application
//! through the execution core, and is judged only by evidence observed after
//! its input.

mod common;

use common::world::*;
use interaction_contracts::desktop::device::{StopDisposition, StopScope};
use interaction_contracts::desktop::operator::StopStatus;
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
use interaction_contracts::desktop::task::RootState;
use interaction_contracts::desktop::verification::{Causality, EffectStatus};
use interaction_engine::verification::VerificationStore;
use rusqlite::Connection;

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn set_value(on: &str) -> Primitive {
    Primitive::AxAction {
        action: AxActionKind::SetValue { value: on.into() },
    }
}

fn press() -> Primitive {
    Primitive::AxAction {
        action: AxActionKind::Press,
    }
}

#[test]
fn a_notification_task_completes_and_only_the_named_switch_changes() {
    let mut world = World::new();
    world.start(TEXT);
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );

    // The right switch, and only it.
    assert_eq!(world.switch("Push", "Daily picks"), Some(false));
    assert_eq!(world.switch("Email", "Daily picks"), Some(true));
    assert_eq!(world.switch("Push", "Weekly digest"), Some(true));

    // Three inputs: set the value, leave the page, come back. Nothing else.
    let inputs: Vec<Primitive> = world
        .applied()
        .iter()
        .map(|i| i.primitive.clone())
        .collect();
    assert_eq!(
        inputs,
        vec![set_value("off"), press(), press()],
        "{}",
        world.report()
    );

    // A plan with one clear way needs no model: every decision was the plan's own.
    assert!(world.decision_requests.is_empty());
    let conn = world.engine.storage().read();
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM decisions WHERE state = 'consumed' AND source = 'plan'"
        ),
        2
    );
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM decisions WHERE state <> 'consumed'"
        ),
        0
    );
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM attempts WHERE state = 'settled'"
        ),
        2
    );

    // The goal was judged on evidence, and the history says how.
    let history = VerificationStore::history(conn, "root-1").unwrap();
    assert!(history.len() >= 2);
    assert_eq!(
        history[0].status,
        EffectStatus::Unsatisfied,
        "the baseline: not yet true"
    );
    assert!(history[0].input_span.is_none());
    let last = history.last().unwrap();
    assert_eq!(last.status, EffectStatus::Satisfied);
    assert_eq!(last.causality, Causality::Confirmed);
    assert!(
        history[..history.len() - 1]
            .iter()
            .all(|result| result.status != EffectStatus::Satisfied),
        "nothing was concluded satisfied before the page was entered again"
    );
    let span = last.input_span.unwrap();
    assert_eq!(span.from_ms, 0);
    assert!(
        last.observed_ms >= span.to_ms,
        "the conclusion came after the last input ended"
    );
}

#[test]
fn an_ambiguous_target_is_put_to_the_backend_and_its_pick_is_what_changes() {
    let mut world = World::new();
    world.teacher.prefer_word = Some("Push".into());
    world.start(TEXT_AMBIGUOUS);
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert_eq!(world.switch("Push", "Daily picks"), Some(false));
    assert_eq!(world.switch("Email", "Daily picks"), Some(true));

    // One question, about the two same-named switches, with the control paths beside them.
    assert_eq!(world.decision_requests.len(), 1, "{}", world.report());
    let request = &world.decision_requests[0];
    let skills: Vec<&str> = request
        .state
        .candidates
        .iter()
        .map(|c| c.skill.as_str())
        .collect();
    assert_eq!(
        skills.iter().filter(|s| **s == "ui.ensure_value").count(),
        2
    );
    assert!(skills.contains(&"observe_more") && skills.contains(&"request_planning"));

    // The pick was attributed to its maker, and the group it named is what
    // found the control again after the page was left and entered.
    let conn = world.engine.storage().read();
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM decisions WHERE source = 'teacher' AND state = 'consumed'"
        ),
        1
    );
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM attempts WHERE source = 'teacher'"
        ),
        1
    );
    assert_eq!(
        count(conn, "SELECT COUNT(*) FROM attempts WHERE source = 'plan'"),
        1
    );
}

#[test]
fn a_task_that_includes_the_commit_presses_apply_and_reads_the_saved_state() {
    let mut world = World::new();
    world.start(TEXT_COMMIT);
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    let inputs: Vec<Primitive> = world
        .applied()
        .iter()
        .map(|i| i.primitive.clone())
        .collect();
    assert_eq!(
        inputs,
        vec![set_value("off"), press()],
        "{}",
        world.report()
    );
    assert_eq!(world.switch("Push", "Daily picks"), Some(false));
    assert_eq!(
        world.app.borrow().save_state(),
        sim_world::app::SaveState::Saved
    );

    // The goal asked for the commit, so nobody was asked to confirm it.
    assert!(world.ui.as_ref().unwrap().pending_confirmation.is_none());
    let history = VerificationStore::history(world.engine.storage().read(), "root-1").unwrap();
    assert_eq!(history.last().unwrap().status, EffectStatus::Satisfied);
    assert_eq!(
        history.last().unwrap().confirmation_scope,
        interaction_contracts::desktop::task::ConfirmationScope::Commit
    );
}

#[test]
fn a_stop_that_reaches_the_device_first_leaves_the_host_unable_to_act_until_it_catches_up() {
    let mut world = World::new();
    // The operator's stop goes straight to the device; the host hears late.
    world.hold_notices = true;
    world.stop_at_device(StopScope::All, StopDisposition::Cancel);
    world.start(TEXT);
    world.run_for(1_500);

    // The host is still running its task and has tried to act. The device
    // refused every request, so nothing reached the application.
    assert!(
        world.applied().is_empty(),
        "no input may follow the stop: {}",
        world.report()
    );
    assert_eq!(world.switch("Push", "Daily picks"), Some(true));
    assert!(
        world
            .engine
            .timeline()
            .any(|entry| entry.text.contains("stop is latched")),
        "{}",
        world.report()
    );
    assert!(
        world.engine.device().acked_stop_seq() < 1,
        "the host has not processed the stop"
    );

    // Now the host hears about it, handles it, and only then acknowledges it.
    world.deliver_delayed();
    world.run_for(200);
    assert_eq!(
        world.outcome(),
        Some(RootState::Cancelled),
        "{}",
        world.report()
    );
    assert_eq!(world.engine.device().acked_stop_seq(), 1);
    assert_eq!(world.engine.stop_status(), StopStatus::StoppedConfirmed);
    assert!(world.applied().is_empty());
}

// ---- Dialogs, confirmation and what a backend may not do ------------------------------------

use interaction_contracts::desktop::decision::{DecisionSource, Precondition};
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::operator::ConfirmationKind;
use sim_world::app::{Modal, SaveState};

/// The moment the switch has been turned off, before anything else happens.
fn after_the_switch_is_off(world: &mut World) {
    world.until(|w| w.switch("Push", "Daily picks") == Some(false), 5_000);
}

#[test]
fn a_dialog_that_appears_mid_task_needs_the_users_word_before_its_commit_button_is_pressed() {
    let mut world = World::new();
    world.start(TEXT);
    after_the_switch_is_off(&mut world);
    world.app.borrow_mut().inject_modal(Modal::UnsavedChanges);

    world.until(|w| w.pending_confirmation().is_some(), 10_000);
    assert_eq!(
        world.state(),
        Some(RootState::NeedsInput),
        "{}",
        world.report()
    );

    // The dialog took the layer, so what was bound behind it was dropped and
    // the decision was about the dialog's own buttons.
    let request = world
        .decision_requests
        .last()
        .expect("the backend was asked");
    assert_eq!(request.decision_program, "ui.layer@1");
    let by_label = |label: &str| {
        request
            .state
            .candidates
            .iter()
            .find(|c| c.parameters.get("label").map(String::as_str) == Some(label))
            .unwrap_or_else(|| panic!("no candidate for {label}"))
    };
    assert_eq!(by_label("Apply").effect_class, EffectClass::Commit);
    assert_eq!(by_label("Apply").precondition, Precondition::Executable);
    assert!(
        matches!(
            by_label("Discard").precondition,
            Precondition::Rejected { .. }
        ),
        "a destructive button the goal does not name is not offered as executable"
    );
    assert_eq!(by_label("Cancel").effect_class, EffectClass::Navigation);

    // The prompt names what would be pressed, and nothing has been yet.
    let prompt = world.pending_confirmation().unwrap();
    match &prompt.kind {
        ConfirmationKind::Effect { action } => {
            assert_eq!(action.target_label, "Apply");
            assert_eq!(action.effect_class, EffectClass::Commit);
        }
        other => panic!("expected an effect confirmation: {other:?}"),
    }
    assert_eq!(world.app.borrow().modal(), Some(Modal::UnsavedChanges));
    assert_eq!(world.app.borrow().save_state(), SaveState::Unsaved);
    assert_eq!(
        world.applied().len(),
        1,
        "only the switch was touched so far"
    );

    // The user agrees to this one action, and the task goes on.
    world.answer(true);
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert_eq!(world.app.borrow().save_state(), SaveState::Saved);
    assert_eq!(world.app.borrow().modal(), None);
    assert_eq!(world.switch("Push", "Daily picks"), Some(false));

    // The approval covered the dialog button only; the rest of the task
    // needed none, and the goal's own limit never moved.
    let conn = world.engine.storage().read();
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM attempts WHERE effect_class = 'commit' AND state = 'settled'"
        ),
        1
    );
    let goal_json: String = conn
        .query_row(
            "SELECT goal_json FROM root_tasks WHERE root_id = 'root-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(goal_json.contains("\"max_effect_class\":\"reversible_edit\""));
}

#[test]
fn a_user_who_declines_the_confirmation_stops_the_task_and_the_button_is_never_pressed() {
    let mut world = World::new();
    world.start(TEXT);
    after_the_switch_is_off(&mut world);
    world.app.borrow_mut().inject_modal(Modal::UnsavedChanges);
    world.until(|w| w.pending_confirmation().is_some(), 10_000);

    world.answer(false);
    world.run_for(500);
    assert_eq!(
        world.outcome(),
        Some(RootState::Cancelled),
        "{}",
        world.report()
    );
    assert_eq!(
        world.app.borrow().save_state(),
        SaveState::Unsaved,
        "Apply was never pressed"
    );
    assert_eq!(world.app.borrow().modal(), Some(Modal::UnsavedChanges));
    assert_eq!(world.applied().len(), 1);
    assert_eq!(world.engine.stop_status(), StopStatus::StoppedConfirmed);
}

#[test]
fn a_harmless_dialog_is_dismissed_without_asking_and_the_destructive_button_stays_out_of_reach() {
    let mut world = World::new();
    world.start(TEXT);
    after_the_switch_is_off(&mut world);
    world.app.borrow_mut().inject_modal(Modal::ResetConfirm);
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert!(world.ui.as_ref().unwrap().pending_confirmation.is_none());
    assert_eq!(world.app.borrow().modal(), None);
    // Reset would have put every switch back to its default. It was not pressed.
    assert_eq!(world.switch("Push", "Daily picks"), Some(false));
    assert!(
        world
            .applied()
            .iter()
            .all(|input| !matches!(input.effect, sim_world::app::ApplyEffect::Reset)),
        "{}",
        world.report()
    );
}

#[test]
fn a_backend_that_picks_what_was_not_offered_as_executable_is_ignored() {
    let mut world = World::new();
    world.start(TEXT);
    after_the_switch_is_off(&mut world);
    world.app.borrow_mut().inject_modal(Modal::ResetConfirm);
    world.backend = Backend::HostileOnce;
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );

    // The hostile answer was refused, recorded, and asked again.
    assert!(world.decision_requests.len() >= 2, "{}", world.report());
    assert!(
        world
            .engine
            .timeline()
            .any(|e| e.text.contains("not executable")),
        "{}",
        world.report()
    );
    assert!(
        world
            .applied()
            .iter()
            .all(|input| !matches!(input.effect, sim_world::app::ApplyEffect::Reset)),
        "the destructive button was never pressed"
    );
    assert_eq!(world.switch("Push", "Daily picks"), Some(false));
    let conn = world.engine.storage().read();
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM decisions WHERE state = 'superseded'"
        ),
        1
    );
}

// ---- Evidence from before the input -----------------------------------------------------------

#[test]
fn a_frame_from_before_the_input_neither_confirms_nor_contradicts_it() {
    let mut world = World::new();
    // A frame captured before anything happened: the switch is still on.
    let stale = world.app.borrow().snapshot(&sim_world::app::SnapshotMeta {
        snapshot_id: "stale".into(),
        session_epoch: 1,
        surface: interaction_contracts::desktop::device::SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "sim/1/monotonic".into(),
        source_sequence: 0,
        observed_ms: 10,
    });
    world.start(TEXT);
    assert_eq!(
        world.switch("Push", "Daily picks"),
        Some(false),
        "the input has landed"
    );

    // The source goes quiet. The late frame arrives: captured at 10, it
    // carries a later sequence number because it was processed late.
    world.source_up = false;
    world.run_for(50);
    let mut late = stale;
    world.source_sequence += 1;
    late.source_sequence = world.source_sequence;
    world.send(interaction_engine::engine::Input::Structure(Box::new(late)));
    let latest = world.engine.state().latest().unwrap();
    assert_eq!(
        latest.snapshot.snapshot_id.as_str(),
        "stale",
        "the late frame is now the latest evidence"
    );
    world.run_for(800);

    // Shown the old value, the skill neither takes it as a failure to retry
    // nor as a confirmation: it waits for evidence from after the input.
    assert_eq!(
        world.applied().len(),
        1,
        "no second input on the strength of an old frame"
    );
    assert_ne!(world.outcome(), Some(RootState::Completed));
    assert!(!world.finished(), "{}", world.report());
    let history = VerificationStore::history(world.engine.storage().read(), "root-1").unwrap();
    assert!(
        history
            .iter()
            .all(|result| result.status != EffectStatus::Satisfied)
    );

    // The real frame comes, and the task goes on to finish.
    world.source_up = true;
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    let inputs: Vec<Primitive> = world
        .applied()
        .iter()
        .map(|i| i.primitive.clone())
        .collect();
    assert_eq!(inputs, vec![set_value("off"), press(), press()]);
}

// ---- A backend that is not there -----------------------------------------------------------------

#[test]
fn an_unavailable_backend_makes_the_decision_wait_and_nothing_else_is_tried() {
    let mut world = World::new();
    world.teacher.prefer_word = Some("Push".into());
    world.backend = Backend::Unavailable;
    world.start(TEXT_AMBIGUOUS);
    world.run_for(6_000);

    assert_eq!(
        world.state(),
        Some(RootState::Running),
        "{}",
        world.report()
    );
    assert!(world.applied().is_empty());
    // The same decision was asked again, at higher and higher generations.
    let first = &world.decision_requests[0];
    let generations: Vec<u64> = world
        .decision_requests
        .iter()
        .filter(|r| r.decision_id == first.decision_id)
        .map(|r| r.request_generation)
        .collect();
    assert!(generations.len() >= 3, "{generations:?}");
    assert!(
        generations.windows(2).all(|w| w[1] > w[0]),
        "{generations:?}"
    );
    // No cheaper-looking route was taken: no plan was asked for, nothing was spent.
    assert_eq!(
        world.planning_requests.len(),
        1,
        "only the goal compilation"
    );
    let conn = world.engine.storage().read();
    assert_eq!(count(conn, "SELECT COUNT(*) FROM attempts"), 0);

    // When the backend comes back the task finishes.
    world.backend = Backend::Answering;
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert_eq!(world.switch("Push", "Daily picks"), Some(false));
}

#[test]
fn a_silent_backend_lets_requests_expire_and_the_task_goes_on_when_it_returns() {
    let mut world = World::new();
    world.teacher.prefer_word = Some("Push".into());
    world.backend = Backend::Silent;
    world.start(TEXT_AMBIGUOUS);
    world.run_for(9_000);
    assert_eq!(
        world.state(),
        Some(RootState::Running),
        "{}",
        world.report()
    );
    assert!(world.applied().is_empty());
    let ids: std::collections::BTreeSet<_> = world
        .decision_requests
        .iter()
        .map(|r| r.decision_id.as_str().to_string())
        .collect();
    assert!(
        ids.len() >= 2,
        "an expired request is replaced by a new one: {ids:?}"
    );
    let conn = world.engine.storage().read();
    assert!(
        count(
            conn,
            "SELECT COUNT(*) FROM decisions WHERE state = 'expired'"
        ) >= 1
    );

    world.backend = Backend::Answering;
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    let _ = DecisionSource::Teacher;
}

// ---- Stopping, pausing, resuming and dying --------------------------------------------------------

use interaction_contracts::desktop::ids::RootTaskId;
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode};

fn root1() -> RootTaskId {
    RootTaskId::new("root-1").unwrap()
}

#[test]
fn a_person_touching_the_keyboard_pauses_the_task_and_only_an_explicit_resume_continues_it() {
    let mut world = World::new();
    world.start(TEXT);
    after_the_switch_is_off(&mut world);
    let ack = world.core.human_input_detected();
    assert_eq!(ack.disposition, StopDisposition::Pause);
    world.run_for(200);

    assert_eq!(
        world.state(),
        Some(RootState::Suspended),
        "{}",
        world.report()
    );
    assert_eq!(world.engine.stop_status(), StopStatus::StoppedConfirmed);
    assert!(
        world
            .engine
            .timeline()
            .any(|e| e.text.contains("a person took over"))
    );

    // Nothing moves on its own, however long it is left.
    let inputs = world.applied().len();
    world.run_for(3_000);
    assert_eq!(world.state(), Some(RootState::Suspended));
    assert_eq!(world.applied().len(), inputs);

    // The operator resumes it. It is revalidated from new evidence before it goes on.
    world.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert!(
        world
            .engine
            .timeline()
            .any(|e| e.text.contains("revalidated"))
    );
    // The switch was not touched a second time.
    let inputs: Vec<Primitive> = world
        .applied()
        .iter()
        .map(|i| i.primitive.clone())
        .collect();
    assert_eq!(
        inputs,
        vec![set_value("off"), press(), press()],
        "{}",
        world.report()
    );
}

#[test]
fn the_operators_stop_releases_the_input_and_nothing_follows_it() {
    let mut world = World::new();
    world.start(TEXT);
    after_the_switch_is_off(&mut world);
    world.operator(OperatorCommand::StopTask { root_task_id: None });
    world.run_for(300);

    assert_eq!(
        world.outcome(),
        Some(RootState::Cancelled),
        "{}",
        world.report()
    );
    assert_eq!(world.engine.stop_status(), StopStatus::StoppedConfirmed);
    let inputs = world.applied().len();
    world.run_for(2_000);
    assert_eq!(world.applied().len(), inputs, "no input after the stop");

    // A cancelled root does not come back; a new task is a new root.
    world.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });
    assert_eq!(world.state(), None);
    world.start(TEXT);
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert_eq!(world.engine.view().unwrap().root_task_id.as_str(), "root-2");
}

#[test]
fn a_restart_between_the_input_and_its_receipt_does_not_replay_the_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = World::on_disk(&dir.path().join("host.db"));
    world.start(TEXT);
    after_the_switch_is_off(&mut world);
    assert_eq!(world.applied().len(), 1);

    // The host dies with the input on its way and no receipt yet.
    world.restart_engine();
    assert_eq!(
        world.state(),
        Some(RootState::NeedsRevalidation),
        "{}",
        world.report()
    );
    let conn = world.engine.storage().read();
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM attempts WHERE state = 'unknown'"
        ),
        1
    );
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM decisions WHERE state = 'consumed'"
        ),
        1
    );
    assert_eq!(count(conn, "SELECT COUNT(*) FROM attempt_map"), 1);
    assert_eq!(count(conn, "SELECT COUNT(*) FROM admission_outbox"), 1);

    // It does nothing by itself: no request reaches the device, however long it waits.
    let mark = world.log_len();
    world.run_for(2_000);
    assert_eq!(world.state(), Some(RootState::NeedsRevalidation));
    for name in ["observe", "grant_permit", "submit", "register_root"] {
        assert_eq!(
            world.sent_since(mark, name),
            0,
            "{name} must wait for the operator"
        );
    }

    // The operator resumes it. The device is asked what happened to the input
    // that was in flight, the root is registered again under its old deadline,
    // and the application is looked at before anything is decided.
    world.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert!(world.sent_since(mark, "query_receipt") >= 1);
    let inputs: Vec<Primitive> = world
        .applied()
        .iter()
        .map(|i| i.primitive.clone())
        .collect();
    assert_eq!(
        inputs,
        vec![set_value("off"), press(), press()],
        "the switch was set once, before the restart, and never again: {}",
        world.report()
    );
    // The quota of the attempt that was in flight stayed spent.
    let conn = world.engine.storage().read();
    let used: i64 = conn
        .query_row(
            "SELECT attempts_used FROM budget_ledger WHERE root_id = 'root-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        used, 2,
        "the first attempt (settled by the device's record) and the revisit"
    );
}

#[test]
fn the_attempt_budget_is_a_hard_limit_and_a_failed_root_stops_where_it_is() {
    let mut config = engine_config();
    config.tasks.default_budget.max_attempts = 1;
    let mut world = World::with_config(config);
    world.start(TEXT);
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Failed),
        "{}",
        world.report()
    );
    // The first attempt did its work; the second was refused.
    assert_eq!(world.applied().len(), 1);
    assert!(
        world
            .engine
            .timeline()
            .any(|e| e.text.contains("attempt budget"))
    );
    assert_eq!(world.switch("Push", "Daily picks"), Some(false));
}

#[test]
fn a_root_that_cannot_get_evidence_expires_and_the_device_is_told_to_let_go() {
    let mut config = engine_config();
    config.tasks.default_budget.deadline_ms = 600;
    let mut world = World::with_config(config);
    world.source_up = false;
    world.start(TEXT);
    world.until(|w| w.finished(), 10_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Expired),
        "{}",
        world.report()
    );
    assert!(world.applied().is_empty());
    assert!(
        world.sent("stop") >= 1,
        "the device must release whatever it holds"
    );
    world.run_for(200);
    assert_eq!(world.engine.device().acked_stop_seq(), 1);
}

#[test]
fn nothing_is_submitted_outside_execute_mode_and_lowering_the_mode_pauses_the_task() {
    let mut config = engine_config();
    config.initial_mode = RunMode::Observe;
    let mut world = World::with_config(config);
    world.start(TEXT);
    world.run_for(1_500);
    assert_eq!(world.state(), Some(RootState::Ready), "{}", world.report());
    assert_eq!(world.sent("register_root"), 0);
    assert!(world.applied().is_empty());

    world.operator(OperatorCommand::SetMode {
        mode: RunMode::Execute,
    });
    after_the_switch_is_off(&mut world);
    world.operator(OperatorCommand::SetMode {
        mode: RunMode::Observe,
    });
    world.run_for(300);
    assert_eq!(
        world.state(),
        Some(RootState::Suspended),
        "{}",
        world.report()
    );
    let inputs = world.applied().len();
    world.run_for(2_000);
    assert_eq!(world.applied().len(), inputs);
}

// ---- What cannot become a goal -----------------------------------------------------------------------

#[test]
fn words_with_no_target_fail_to_compile_and_nothing_reaches_the_device() {
    let mut world = World::new();
    world.create_task("Turn it off");
    world.until(|w| w.finished(), 5_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Failed),
        "{}",
        world.report()
    );
    assert_eq!(world.sent("register_root"), 0);
    assert!(world.applied().is_empty());
}

#[test]
fn a_plan_that_reaches_beyond_the_goal_is_refused_before_the_user_is_asked() {
    let mut world = World::new();
    world.planner.overreach = true;
    world.create_task(TEXT);
    world.until(|w| w.finished(), 5_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Failed),
        "{}",
        world.report()
    );
    assert!(world.pending_confirmation().is_none());
    assert_eq!(world.sent("register_root"), 0);
}

#[test]
fn a_planner_that_never_answers_fails_the_compilation_and_a_late_answer_is_ignored() {
    let mut world = World::new();
    world.planner_silent = true;
    world.create_task(TEXT);
    world.run_for(31_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Failed),
        "{}",
        world.report()
    );

    // An answer that turns up afterwards finds nothing waiting for it.
    let request = world.planning_requests[0].clone();
    let late = world.planner.plan(&request);
    world.send(interaction_engine::engine::Input::Planner {
        request_id: request.request_id,
        generation: request.generation,
        output: Box::new(late),
    });
    assert_eq!(world.outcome(), Some(RootState::Failed));
    assert!(world.pending_confirmation().is_none());
}

#[test]
fn a_second_task_waits_for_the_first_and_then_runs_on_its_own() {
    let mut world = World::new();
    world.start(TEXT);
    world.create_task(r#"Turn off "Weekly digest" in "Push""#);
    assert_eq!(
        world.planning_requests.len(),
        1,
        "the second is not compiled yet"
    );
    world.until(
        |w| {
            w.engine
                .view()
                .is_some_and(|v| v.root_task_id.as_str() == "root-2")
        },
        60_000,
    );
    // The second task is compiled and waits for its own confirmation.
    world.until(|w| w.pending_confirmation().is_some(), 5_000);
    world.answer(true);
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert_eq!(world.switch("Push", "Daily picks"), Some(false));
    assert_eq!(world.switch("Push", "Weekly digest"), Some(false));
    assert_eq!(world.switch("Email", "Daily picks"), Some(true));
}

// ---- Admission failures, knowledge pins and the operator's view --------------------------------------

use interaction_engine::memory::{Memory, ReleaseManifest};
use interaction_engine::tasks::AdmitFault;

#[test]
fn a_failed_admission_leaves_nothing_behind_and_the_step_is_tried_again() {
    let mut world = World::new();
    world
        .engine
        .inject_admit_fault(Some(AdmitFault::BeforeCommit));
    world.start(TEXT);
    world.run_for(3_000);

    // The write failed at its last moment. Nothing of it exists: no attempt, no
    // quota taken, no decision consumed, and no input went anywhere.
    assert_eq!(
        world.state(),
        Some(RootState::Running),
        "{}",
        world.report()
    );
    assert!(world.applied().is_empty());
    {
        let conn = world.engine.storage().read();
        assert_eq!(count(conn, "SELECT COUNT(*) FROM attempts"), 0);
        assert_eq!(count(conn, "SELECT COUNT(*) FROM attempt_map"), 0);
        assert_eq!(
            count(
                conn,
                "SELECT COUNT(*) FROM decisions WHERE state = 'consumed'"
            ),
            0
        );
        assert_eq!(count(conn, "SELECT attempts_used FROM budget_ledger"), 0);
    }
    assert!(
        world
            .engine
            .timeline()
            .any(|e| e.text.contains("nothing was recorded"))
    );

    // Once the store works again the same step is admitted, for the price of one attempt.
    world.engine.inject_admit_fault(None);
    world.until(|w| w.finished(), 60_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    let conn = world.engine.storage().read();
    let used: i64 = conn
        .query_row("SELECT attempts_used FROM budget_ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(used, 2, "the failed admissions cost nothing");
}

#[test]
fn a_root_uses_the_knowledge_release_that_was_current_when_it_was_confirmed() {
    let mut world = World::new();
    // The engine's database is the host's; publish into it before the task.
    let release = |id: &str| ReleaseManifest {
        release_id: interaction_contracts::desktop::ids::ReleaseId::new(id).unwrap(),
        revisions: vec![format!("{id}/rev")],
    };
    // Use a second handle to the same kind of store to publish, then check pins.
    let mut storage =
        interaction_engine::infrastructure::storage::Storage::open_in_memory().unwrap();
    Memory::publish(&mut storage, &release("r1")).unwrap();
    let _ = &mut world;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host.db");
    {
        let mut seed = interaction_engine::infrastructure::storage::Storage::open(&path).unwrap();
        Memory::publish(&mut seed, &release("r1")).unwrap();
    }
    let mut world = World::on_disk(&path);
    world.start(TEXT);
    assert!(
        world
            .engine
            .timeline()
            .any(|e| e.text.contains("knowledge release r1")),
        "{}",
        world.report()
    );
    world.until(|w| w.finished(), 60_000);

    // A later release changes what new roots pin, not this one; a quarantine
    // reaches even a pinned root at once.
    let conn = world.engine.storage().read();
    let pinned: String = conn
        .query_row(
            "SELECT release_id FROM root_release_pins WHERE root_id = 'root-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pinned, "r1");
    drop(world);
    let mut after = interaction_engine::infrastructure::storage::Storage::open(&path).unwrap();
    Memory::publish(&mut after, &release("r2")).unwrap();
    assert_eq!(
        Memory::pinned(&after, &root1()).unwrap().unwrap().as_str(),
        "r1"
    );
    Memory::quarantine(&mut after, "r1/rev", "found to be wrong").unwrap();
    let refs = Memory::refs_for(&after, &root1()).unwrap();
    assert!(refs.revisions.is_empty() && refs.quarantined == vec!["r1/rev".to_string()]);
}

#[test]
fn a_stop_the_device_has_not_confirmed_is_shown_as_unconfirmed_until_it_is() {
    let mut world = World::new();
    world.start(TEXT);
    after_the_switch_is_off(&mut world);
    world.mute_stop = true;
    world.operator(OperatorCommand::StopTask { root_task_id: None });

    // The click is not the stop: the task is cancelling and the view says so.
    assert_eq!(world.state(), Some(RootState::Cancelling));
    assert_eq!(world.ui.as_ref().unwrap().stop_status, StopStatus::Stopping);
    world.run_for(2_500);
    assert_eq!(
        world.state(),
        Some(RootState::Cancelling),
        "{}",
        world.report()
    );
    assert_eq!(
        world.ui.as_ref().unwrap().stop_status,
        StopStatus::StopUnconfirmed
    );

    // The device's confirmation finally arrives.
    world.deliver_delayed();
    world.run_for(100);
    assert_eq!(
        world.outcome(),
        Some(RootState::Cancelled),
        "{}",
        world.report()
    );
    assert_eq!(
        world.ui.as_ref().unwrap().stop_status,
        StopStatus::StoppedConfirmed
    );
    assert_eq!(
        world.ui.as_ref().unwrap().root.as_ref().unwrap().state,
        RootState::Cancelled
    );
}

#[test]
fn the_operator_view_follows_the_task_from_compilation_to_the_end() {
    let mut world = World::new();
    world.create_task(TEXT);
    world.until(|w| w.pending_confirmation().is_some(), 5_000);
    let ui = world.ui.clone().unwrap();
    assert_eq!(
        ui.root.as_ref().unwrap().state,
        RootState::AwaitingConfirmation
    );
    assert!(matches!(
        ui.pending_confirmation.unwrap().kind,
        ConfirmationKind::Goal { .. }
    ));
    assert_eq!(
        ui.capabilities.as_ref().unwrap().backend,
        interaction_contracts::desktop::device::BackendKind::Simulated
    );

    world.answer(true);
    world.until(|w| w.finished(), 60_000);
    let done = world.ui.clone().unwrap();
    assert_eq!(done.root.as_ref().unwrap().state, RootState::Completed);
    assert_eq!(done.root.as_ref().unwrap().attempts_used, 2);
    assert!(done.pending_confirmation.is_none());
    assert!(
        done.revision > ui.revision,
        "the view is versioned so the renderer can tell what is new"
    );
    assert!(!done.timeline.is_empty());
}
