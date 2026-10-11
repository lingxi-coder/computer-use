//! Regressions for ended planner requests, bounded receipt recovery and stop
//! status across task lifecycles. All input goes through the execution core.

mod common;

use common::world::{TEXT, World, engine_config};
use interaction_contracts::desktop::device::{ReceiptStatus, ReleaseStatus};
use interaction_contracts::desktop::ids::RootTaskId;
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode, StopStatus};
use interaction_contracts::desktop::planning::{PlannerOutput, PlanningRequest};
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
use interaction_contracts::desktop::source::{SourceHealth, SourceKind, SourceState};
use interaction_contracts::desktop::task::RootState;
use interaction_engine::engine::Input;

fn root1() -> RootTaskId {
    RootTaskId::new("root-1").unwrap()
}

fn replace_read_instances(
    snapshot: &mut interaction_contracts::desktop::source::StructureSnapshot,
) {
    use interaction_contracts::desktop::ids::ObjectRef;
    let suffix = format!("-read-{}", snapshot.source_sequence);
    for object in &mut snapshot.objects {
        object.instance_ref =
            ObjectRef::new(format!("{}{}", object.instance_ref.as_str(), suffix)).unwrap();
        object.parent = object
            .parent
            .as_ref()
            .map(|parent| ObjectRef::new(format!("{}{}", parent.as_str(), suffix)).unwrap());
    }
}

#[test]
fn known_no_effect_ax_input_relocates_with_a_new_permit_without_resetting_the_attempt() {
    let mut world = World::new();
    world.ignored_ax_setter.set(true);
    world.snapshot_transform = Some(Box::new(replace_read_instances));
    world.start(TEXT);
    world.until(|w| w.finished(), 5000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    let inputs = world.applied();
    assert_eq!(
        inputs.len(),
        4,
        "one AX setter, one pointer and two navigation inputs"
    );
    assert!(matches!(
        inputs[0].primitive,
        Primitive::AxAction {
            action: AxActionKind::SetValue { .. }
        }
    ));
    assert!(matches!(inputs[1].primitive, Primitive::Click { .. }));
    let (count, intent, quota): (i64, String, u32) = world.engine.storage().read().query_row("SELECT COUNT(*), MAX(intent_id), MAX(quota_actions) FROM attempts WHERE skill='ui.ensure_value'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(count, 1);
    assert_eq!(intent, "intent-att-1-2");
    assert_eq!(quota, 2);
    assert_eq!(
        world.sent("grant_permit"),
        4,
        "every new input acquires a fresh permit"
    );
    assert_ne!(inputs[0].permit, inputs[1].permit);
    assert!(inputs[0].instance.is_some());
    assert_ne!(
        inputs[0].instance, inputs[1].instance,
        "the new binding is not an alias of the old instance"
    );
}

#[test]
fn a_relocated_control_with_a_raised_effect_cannot_receive_the_pointer_fallback() {
    let mut world = World::new();
    world.ignored_ax_setter.set(true);
    let inputs = world.inputs.clone();
    world.snapshot_transform = Some(Box::new(move |snapshot| {
        replace_read_instances(snapshot);
        if !inputs.borrow().is_empty() {
            for object in &mut snapshot.objects {
                if object.group.as_deref() == Some("Push")
                    && object.title.as_deref() == Some("Daily picks")
                {
                    object.title = Some("Daily picks delete".into());
                }
            }
        }
    }));
    world.start(TEXT);
    world.run_for(2000);
    assert_eq!(world.applied().len(), 1, "the AX setter is the only input");
    assert_ne!(world.outcome(), Some(RootState::Completed));
    assert!(
        world.engine.timeline().any(|note| note
            .text
            .contains("relocated target's effect is not authorized")),
        "{}",
        world.report()
    );
}

#[test]
fn fresh_data_and_old_acks_cannot_submit_while_a_newer_refresh_is_queued() {
    let mut config = engine_config();
    config.evidence_max_age_ms = 100;
    config.refresh_timeout_ms = 200;
    let mut world = World::with_config(config);
    world.source_up = false;
    world.run_for(200);
    world.hold_refreshes = true;
    world.start(TEXT);
    world.until(|w| !w.refreshes.is_empty(), 2000);
    let first = world.refreshes[0];
    world.source_up = true;
    world.publish();
    world.run_for(50);
    assert!(
        world.applied().is_empty(),
        "fresh unsolicited data cannot finish a queued refresh"
    );
    world.until(|w| w.refreshes.len() >= 2, 2000);
    let second = *world.refreshes.last().unwrap();
    world.publish();
    world.send(Input::SourceRefreshStarted {
        generation: first,
        accepted: true,
    });
    world.run_for(50);
    assert!(
        world.applied().is_empty(),
        "old transition acknowledgement cannot open the newer barrier"
    );
    world.send(Input::SourceRefreshStarted {
        generation: second,
        accepted: false,
    });
    world.publish();
    world.run_for(50);
    assert!(
        world.applied().is_empty(),
        "refused transitions cannot open authority"
    );
    world.hold_refreshes = false;
    // Complete exactly the still-current generation with new facts, then let
    // ordinary refreshes proceed. No budget or goal is replaced.
    world.publish();
    world.send(Input::SourceRefreshStarted {
        generation: second,
        accepted: true,
    });
    world.until(|w| w.finished(), 10000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
}

#[test]
fn compile_and_replan_receive_the_same_registered_catalog() {
    let world = waiting_for_replan();
    let first = &world.planning_requests[0].context.notes;
    let replan = &world.planning_requests[1].context.notes;
    assert_eq!(first.first(), replan.first());
    let catalog: serde_json::Value = serde_json::from_str(&first[0]).unwrap();
    assert_eq!(catalog["kind"], "registered_app_catalog");
    assert_eq!(catalog["apps"][0]["app"], "test.settings");
    assert_eq!(
        catalog["apps"][0]["controls"][0]["semantic_target"],
        "apply-button"
    );
}

#[test]
fn planner_timeout_emits_cancellation_for_the_exact_remote_scope() {
    let mut engine = interaction_engine::engine::Engine::new(
        engine_config(),
        interaction_engine::infrastructure::storage::Storage::open_in_memory().unwrap(),
    );
    let outputs = engine.handle(
        0,
        Input::Operator(OperatorCommand::CreateTask { text: TEXT.into() }),
    );
    let request = outputs
        .into_iter()
        .find_map(|o| {
            if let interaction_engine::engine::Output::Plan(r) = o {
                Some(r)
            } else {
                None
            }
        })
        .unwrap();
    let outputs = engine.handle(request.deadline_ms + 1, Input::Tick);
    assert!(outputs.iter().any(|o|matches!(o,interaction_engine::engine::Output::CancelPlan(r) if r.request_id == request.request_id && r.generation == request.generation && r.root == request.root_task_id)));
    assert_eq!(
        engine.last_finished(),
        Some(interaction_contracts::desktop::task::RootState::Failed)
    );
}

#[test]
fn matching_device_receipts_acknowledge_outbox_delivery_without_claiming_effect_success() {
    let mut world = World::new();
    world.start(TEXT);
    world.until(|w| !w.applied().is_empty(), 5_000);
    world.run_for(100);
    let acknowledged: i64 = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT COUNT(*) FROM admission_outbox WHERE delivered = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        acknowledged > 0,
        "device receipts must close the admission delivery gap"
    );
    world.until(|w| w.finished(), 10_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
}

#[test]
fn a_read_only_attempt_ends_without_fabricating_device_delivery() {
    let world = waiting_for_replan();
    let (delivered, refunded, state): (i64, i64, String) = world.engine.storage().read().query_row(
        "SELECT o.delivered, a.refunded, a.state FROM admission_outbox o JOIN attempts a USING(attempt_id)",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!((delivered, refunded, state.as_str()), (0, 1, "settled"));
    assert!(world.applied().is_empty());
}

/// A valid but insufficient plan runs one read, then asks for a replan.
fn waiting_for_replan() -> World {
    let mut world = World::new();
    world.planner_silent = true;
    world.create_task(TEXT);
    let request = world.planning_requests[0].clone();
    let mut output = world.planner.plan(&request);
    let PlannerOutput::Plan { proposal } = &mut output else {
        panic!("the fixture returns a goal plan");
    };
    proposal.bundle.subgoals.truncate(1);
    proposal.bundle.subgoals[0].skill = "ui.read_value".into();
    proposal.bundle.dependencies.clear();
    answer_planner(&mut world, &request, output);
    world.answer(true);
    world.until(|w| w.state() == Some(RootState::WaitingPlan), 5_000);
    assert_eq!(world.planning_requests.len(), 2);
    world
}

fn answer_planner(world: &mut World, request: &PlanningRequest, output: PlannerOutput) {
    world.send(Input::Planner {
        request_id: request.request_id.clone(),
        generation: request.generation,
        output: Box::new(output),
    });
}

fn complete_replacement_plan(world: &mut World) {
    let replacement = world.planning_requests.last().unwrap().clone();
    let PlannerOutput::Plan { mut proposal } = world.planner.plan_text(TEXT) else {
        panic!("the fixture returns a plan");
    };
    proposal.goal_draft = None;
    answer_planner(world, &replacement, PlannerOutput::Plan { proposal });
    world.until(|w| w.finished(), 10_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert!(world.engine.faulted().is_none());
}

#[test]
fn an_unusable_replan_response_releases_the_wait_and_keeps_the_replan_spent() {
    let mut world = waiting_for_replan();
    let request = world.planning_requests[1].clone();
    // A goal draft cannot replace the goal the user already confirmed.
    let wrong_phase = world.planner.plan_text(TEXT);
    answer_planner(&mut world, &request, wrong_phase);

    assert_eq!(world.planning_requests.len(), 3, "{}", world.report());
    assert_ne!(world.planning_requests[2].request_id, request.request_id);
    let replans: i64 = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT replans_used FROM budget_ledger WHERE root_id = 'root-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(replans, 2, "the refused replan is not refunded");
    complete_replacement_plan(&mut world);
}

#[test]
fn a_replan_response_arriving_after_its_deadline_before_a_tick_does_not_stick() {
    let mut world = waiting_for_replan();
    let request = world.planning_requests[1].clone();
    world.now = request.deadline_ms + 1;
    let output = world.planner.plan(&request);
    answer_planner(&mut world, &request, output);

    assert_eq!(world.planning_requests.len(), 3, "{}", world.report());
    assert!(world.planning_requests[2].deadline_ms > request.deadline_ms);
    // Advance the simulated device clock before answering the new request.
    world.step();
    complete_replacement_plan(&mut world);
}

#[test]
fn an_old_planner_response_cannot_release_a_newer_replan_wait() {
    let mut world = waiting_for_replan();
    let compilation = world.planning_requests[0].clone();
    let stale = world.planner.plan(&compilation);
    answer_planner(&mut world, &compilation, stale);
    assert_eq!(world.state(), Some(RootState::WaitingPlan));
    assert_eq!(world.planning_requests.len(), 2);
    complete_replacement_plan(&mut world);
}

fn restarting_after_input() -> (tempfile::TempDir, World) {
    let dir = tempfile::tempdir().unwrap();
    let mut world = World::on_disk(&dir.path().join("host.db"));
    world.start(TEXT);
    world.until(|w| w.switch("Push", "Daily picks") == Some(false), 5_000);
    assert_eq!(world.applied().len(), 1);
    world.restart_engine();
    assert_eq!(world.state(), Some(RootState::NeedsRevalidation));
    (dir, world)
}

fn sets_off(world: &World) -> usize {
    world
        .applied()
        .iter()
        .filter(|input| {
            input.primitive
                == Primitive::AxAction {
                    action: AxActionKind::SetValue {
                        value: "off".into(),
                    },
                }
        })
        .count()
}

#[test]
fn one_lost_recovery_query_is_retried_without_replaying_input() {
    let (_dir, mut world) = restarting_after_input();
    world.drop_query_replies = 1;
    let mark = world.log_len();
    world.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });
    world.until(|w| w.finished(), 10_000);

    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert_eq!(world.sent_since(mark, "query_receipt"), 2);
    assert_eq!(sets_off(&world), 1, "the original input was not replayed");
    assert!(world.engine.faulted().is_none());
}

#[test]
fn recovery_refunds_only_after_all_intents_prove_the_attempt_produced_no_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = World::on_disk(&dir.path().join("host.db"));
    world.start(TEXT);
    assert_eq!(world.applied().len(), 1);
    let attempt: String = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT attempt_id FROM attempts WHERE state='dispatched'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // Model a two-intent attempt: its last intent was explicitly rejected
    // before input, while the earlier one already reached the application.
    world
        .engine
        .storage()
        .read()
        .execute(
            "UPDATE attempts SET intent_id=?1, quota_actions=2 WHERE attempt_id=?2",
            [&format!("intent-{attempt}-2"), &attempt],
        )
        .unwrap();
    world.restart_engine();
    world
        .query_statuses
        .push_back(Some(ReceiptStatus::Rejected));
    world
        .query_statuses
        .push_back(Some(ReceiptStatus::Completed));
    world.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });
    world.until(|world| world.finished(), 10_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    let (state, refunded): (String, bool) = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT state, refunded FROM attempts WHERE attempt_id=?1",
            [&attempt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "settled");
    assert!(!refunded, "the earlier input keeps the attempt quota spent");
    assert_eq!(sets_off(&world), 1);
}

#[test]
fn a_paused_unknown_attempt_survives_a_crash_and_still_requires_its_original_receipt() {
    for source_loss in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut world = World::on_disk(&dir.path().join("host.db"));
        if !source_loss {
            world.unknown_submission.set(true);
        } else {
            world.pending_applied_submission.set(true);
        }
        world.start(TEXT);
        if source_loss {
            world.send(Input::SourceHealth(SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Unavailable,
                reason: Some("AX loss after submission".into()),
                at_ms: world.now,
                provenance: None,
            }));
        }
        let input_at = world
            .applied()
            .first()
            .expect("the original input was submitted")
            .at_ms;
        if !source_loss {
            let source = world
                .engine
                .state()
                .latest()
                .expect("the pre-input Source is admitted");
            assert_eq!(source.snapshot.observed_ms, input_at);
            assert_eq!(
                world.state(),
                Some(RootState::Suspended),
                "a terminal unknown receipt suspends immediately; later Source cannot resolve mechanical ambiguity"
            );
        }
        world.until(|world| world.state() == Some(RootState::Suspended), 500);
        assert_eq!(
            world.state(),
            Some(RootState::Suspended),
            "{}",
            world.report()
        );
        assert_eq!(world.sent("submit"), 1);
        world.restart_engine();
        world.query_statuses.push_back(Some(ReceiptStatus::Unknown));
        let queries = world.sent("query_receipt");
        world.operator(OperatorCommand::ResumeTask {
            root_task_id: root1(),
        });
        world.run_for(250);
        assert_eq!(world.sent("query_receipt"), queries + 1);
        assert_eq!(
            world.sent("submit"),
            1,
            "source_loss={source_loss}: {}",
            world.report()
        );
        assert_eq!(world.state(), Some(RootState::NeedsRevalidation));
        assert_eq!(world.engine.view().unwrap().attempts_used, 1);
        let unknown: i64 = world
            .engine
            .storage()
            .read()
            .query_row(
                "SELECT COUNT(*) FROM attempts WHERE state='unknown'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unknown, 1);
    }
}

#[test]
fn completed_input_with_an_unproved_effect_is_verified_without_repeating_navigation() {
    let mut world = World::new();
    world.start(TEXT);
    assert_eq!(world.applied().len(), 1);
    // The value was set. The OS acknowledges the following navigation click,
    // but it has no application effect and its receipt finishes Completed.
    world.ignored_submission.set(true);
    world.until(|world| world.finished(), 5_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Unverified),
        "{}",
        world.report()
    );
    assert_eq!(
        world.applied().len(),
        2,
        "one SetValue and one navigation Click"
    );
    assert_eq!(world.sent("submit"), 2);
    assert_eq!(
        world.sent("query_receipt"),
        0,
        "Completed is not a mechanical Unknown"
    );
    let (unknown, spent): (i64, i64) = world.engine.storage().read().query_row(
        "SELECT COUNT(*) FILTER(WHERE state='unknown'), COUNT(*) FILTER(WHERE refunded=0) FROM attempts", [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(unknown, 0);
    assert_eq!(spent, 2);
    assert!(
        world
            .engine
            .timeline()
            .any(|entry| entry.text.contains("verifying its business effect"))
    );
    assert!(
        !world
            .engine
            .timeline()
            .any(|entry| entry.text.contains("being paused"))
    );
}

#[test]
fn repeated_recovery_query_loss_waits_for_explicit_retry_and_keeps_unknown_quota() {
    let (_dir, mut world) = restarting_after_input();
    world.drop_query_replies = usize::MAX;
    let mark = world.log_len();
    world.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });
    world.run_for(5_000);

    assert_eq!(world.state(), Some(RootState::NeedsRevalidation));
    assert_eq!(
        world.sent_since(mark, "query_receipt"),
        2,
        "one bounded retry"
    );
    assert_eq!(
        world.applied().len(),
        1,
        "unresolved recovery cannot submit input"
    );
    assert_eq!(world.engine.view().unwrap().attempts_used, 1);
    let unknown: i64 = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT COUNT(*) FROM attempts WHERE state = 'unknown'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unknown, 1);
    assert!(
        world
            .engine
            .timeline()
            .any(|entry| entry.text.contains("resume the task to retry recovery"))
    );
    world.run_for(1_000);
    assert_eq!(world.sent_since(mark, "query_receipt"), 2);

    world.drop_query_replies = 0;
    world.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });
    world.until(|w| w.finished(), 10_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
    assert_eq!(sets_off(&world), 1);
    assert!(world.engine.faulted().is_none());
}

#[test]
fn a_new_root_clears_a_previous_confirmed_stop_before_it_can_run() {
    let mut config = engine_config();
    config.initial_mode = RunMode::Observe;
    let mut world = World::with_config(config);
    world.start(TEXT);
    world.operator(OperatorCommand::StopTask { root_task_id: None });
    assert_eq!(world.outcome(), Some(RootState::Cancelled));
    assert_eq!(world.engine.stop_status(), StopStatus::StoppedConfirmed);

    world.start(TEXT);
    let ui = world.ui.as_ref().unwrap();
    assert_eq!(ui.root.as_ref().unwrap().root_task_id.as_str(), "root-2");
    assert_eq!(ui.stop_status, StopStatus::NotStopped);
    assert_eq!(ui.cleanup, ReleaseStatus::NotNeeded);
    world.operator(OperatorCommand::SetMode {
        mode: RunMode::Execute,
    });
    world.until(|w| !w.applied().is_empty(), 5_000);
    assert_eq!(world.engine.stop_status(), StopStatus::NotStopped);
}

#[test]
fn resuming_a_paused_root_clears_its_confirmed_stop() {
    let mut world = World::new();
    world.start(TEXT);
    world.operator(OperatorCommand::PauseTask {
        root_task_id: root1(),
    });
    assert_eq!(world.state(), Some(RootState::Suspended));
    assert_eq!(world.engine.stop_status(), StopStatus::StoppedConfirmed);
    world.operator(OperatorCommand::ResumeTask {
        root_task_id: root1(),
    });

    assert_eq!(world.engine.stop_status(), StopStatus::NotStopped);
    assert_eq!(world.ui.as_ref().unwrap().cleanup, ReleaseStatus::NotNeeded);
    world.until(|w| w.finished(), 10_000);
    assert_eq!(
        world.outcome(),
        Some(RootState::Completed),
        "{}",
        world.report()
    );
}

#[test]
fn a_new_root_does_not_erase_an_unconfirmed_release() {
    let mut config = engine_config();
    config.tasks.default_budget.deadline_ms = 1_000;
    let mut world = World::with_config(config);
    world.start(TEXT);
    world.source_up = false;
    world.mute_stop = true;
    world.run_for(3_500);
    assert_eq!(world.outcome(), Some(RootState::Expired));
    assert_eq!(world.engine.stop_status(), StopStatus::StopUnconfirmed);

    world.create_task(TEXT);
    assert_eq!(world.engine.view().unwrap().root_task_id.as_str(), "root-2");
    assert_eq!(world.engine.stop_status(), StopStatus::StopUnconfirmed);
}
