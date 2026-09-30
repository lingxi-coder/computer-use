use execution_core::sim::*;
use interaction_contracts::i0::*;

const TASK: &str = "task-1";
const TARGET: &str = "target-1";
const EVIDENCE: &str = "observation-1";

fn observation(sim: &Simulator, id: &str, sequence: u64, until: u64) -> Observation {
    Observation {
        observation_id: id.into(),
        session_epoch: sim.session_epoch(),
        surface: sim.surface(),
        target_ref: TARGET.into(),
        clock_domain: sim.clock_domain(),
        source_sequence: sequence,
        observed_ms: sim.now_ms(),
        valid_until_ms: until,
        freshness: SourceFreshness::Verified,
        dependencies: sim.dependency_versions(),
    }
}

fn setup(task_until: u64, evidence_until: u64, permit_until: u64) -> (Simulator, PermitRef) {
    let mut sim = Simulator::new();
    sim.register_task(TASK, 1, task_until).unwrap();
    sim.observe(observation(&sim, EVIDENCE, 1, evidence_until))
        .unwrap();
    let permit = sim
        .grant_permit(TASK, TARGET, EVIDENCE, permit_until)
        .unwrap();
    (sim, permit)
}

fn ready() -> (Simulator, PermitRef) {
    setup(1_000, 1_000, 900)
}

fn key_down(key: &str) -> Input {
    Input::KeyDown { key: key.into() }
}

fn key_up(key: &str) -> Input {
    Input::KeyUp { key: key.into() }
}

fn step(offset_ms: u64, input: Input) -> ScheduledInput {
    ScheduledInput { offset_ms, input }
}

fn action(sim: &Simulator, permit: &PermitRef, id: &str, key: &str) -> ActionIntent {
    ActionIntent {
        intent_id: id.into(),
        task_id: TASK.into(),
        task_revision: 1,
        session_epoch: sim.session_epoch(),
        permit: permit.clone(),
        surface: sim.surface(),
        target_ref: TARGET.into(),
        coordinate_space: CoordinateSpace::SurfacePx,
        dependencies: sim.dependency_versions(),
        evidence_id: EVIDENCE.into(),
        latest_start_ms: 500,
        duration_ms: 100,
        steps: vec![step(0, key_down(key))],
    }
}

fn receipt(sim: &Simulator, epoch: u64, id: &str) -> Receipt {
    let result = sim.query_receipt(epoch, id).expect("intent has a receipt");
    result
        .validate()
        .expect("runtime receipt is protocol-valid");
    result
}

fn start(sim: &mut Simulator, intent: ActionIntent) -> Receipt {
    assert_eq!(sim.submit(intent).status, ReceiptStatus::Accepted);
    let result = sim.dispatch_next().expect("idle channel can dispatch");
    assert_eq!(result.status, ReceiptStatus::Running);
    result
}

fn normal_events(sim: &Simulator) -> Vec<(u64, Input)> {
    sim.backend_events()
        .iter()
        .filter(|event| !event.cleanup)
        .map(|event| (event.at_ms, event.input.clone()))
        .collect()
}

fn cleanup_events(sim: &Simulator) -> Vec<(u64, Input)> {
    sim.backend_events()
        .iter()
        .filter(|event| event.cleanup)
        .map(|event| (event.at_ms, event.input.clone()))
        .collect()
}

#[test]
fn acceptance_queues_work_and_large_time_jumps_preserve_timer_order() {
    let (mut sim, permit) = ready();
    let mut intent = action(&sim, &permit, "timed", "W");
    intent.steps.extend([
        step(
            20,
            Input::PointerDown {
                button: MouseButton::Left,
            },
        ),
        step(40, Input::PointerMove { x: 12, y: 34 }),
    ]);
    let accepted = sim.submit(intent);
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    assert!(accepted.submitted.is_empty());
    assert!(sim.backend_events().is_empty());
    assert!(sim.held_inputs().is_empty());

    assert_eq!(sim.dispatch_next().unwrap().status, ReceiptStatus::Running);
    sim.advance_to(800).unwrap();

    assert_eq!(
        normal_events(&sim),
        vec![
            (0, key_down("W")),
            (
                20,
                Input::PointerDown {
                    button: MouseButton::Left
                }
            ),
            (40, Input::PointerMove { x: 12, y: 34 }),
        ]
    );
    let cleanup = cleanup_events(&sim);
    assert_eq!(cleanup.len(), 2);
    assert!(cleanup.contains(&(100, key_up("W"))));
    assert!(cleanup.contains(&(
        100,
        Input::PointerUp {
            button: MouseButton::Left
        }
    )));
    assert!(sim.held_inputs().is_empty());
    let result = receipt(&sim, sim.session_epoch(), "timed");
    assert_eq!(result.status, ReceiptStatus::Completed);
    assert_eq!(result.command_end_ms, Some(100));
    assert_eq!(result.release_status, ReleaseStatus::Released);
}

#[test]
fn independent_intents_wait_for_the_same_physical_focus_resource() {
    let (mut sim, permit) = ready();
    let first = action(&sim, &permit, "first", "W");
    let second_permit = sim.grant_permit(TASK, TARGET, EVIDENCE, 900).unwrap();
    let second = action(&sim, &second_permit, "second", "A");
    start(&mut sim, first);
    assert_eq!(sim.submit(second).status, ReceiptStatus::Accepted);
    assert!(sim.dispatch_next().is_none());
    assert_eq!(normal_events(&sim), vec![(0, key_down("W"))]);

    sim.advance_to(100).unwrap();
    assert_eq!(sim.dispatch_next().unwrap().status, ReceiptStatus::Running);
    assert_eq!(
        normal_events(&sim),
        vec![(0, key_down("W")), (100, key_down("A"))]
    );
    sim.advance_to(200).unwrap();
    assert!(sim.held_inputs().is_empty());
}

#[test]
fn queued_geometry_invalidation_cancels_without_any_backend_input() {
    let (mut sim, permit) = ready();
    let intent = action(&sim, &permit, "queued", "W");
    assert_eq!(sim.submit(intent).status, ReceiptStatus::Accepted);
    sim.update_dependency("geometry", Some(true));

    assert!(sim.dispatch_next().is_none());
    assert!(sim.backend_events().is_empty());
    assert_eq!(
        receipt(&sim, sim.session_epoch(), "queued").status,
        ReceiptStatus::Cancelled
    );
}

#[test]
fn every_required_dependency_can_cancel_held_input_and_remaining_steps() {
    for dependency in ["focus", "layer", "binding", "authorization", "geometry"] {
        let (mut sim, permit) = ready();
        let mut intent = action(&sim, &permit, "held", "W");
        intent.steps.push(step(50, key_down("A")));
        start(&mut sim, intent);
        sim.advance_to(10).unwrap();
        sim.update_dependency(dependency, None);
        sim.advance_to(200).unwrap();

        assert_eq!(
            normal_events(&sim),
            vec![(0, key_down("W"))],
            "{dependency}"
        );
        assert_eq!(
            cleanup_events(&sim),
            vec![(10, key_up("W"))],
            "{dependency}"
        );
        assert!(sim.held_inputs().is_empty(), "{dependency}");
        let result = receipt(&sim, sim.session_epoch(), "held");
        assert_eq!(result.status, ReceiptStatus::Cancelled, "{dependency}");
        assert_eq!(
            result.release_status,
            ReleaseStatus::Released,
            "{dependency}"
        );
    }
}

#[test]
fn unrelated_state_updates_do_not_interrupt_or_repeat_an_action() {
    let (mut sim, permit) = ready();
    let intent = action(&sim, &permit, "unrelated", "W");
    start(&mut sim, intent);
    sim.advance_to(20).unwrap();
    sim.update_dependency("unrelated-window", Some(true));
    sim.advance_to(99).unwrap();
    assert_eq!(sim.held_inputs(), vec!["key:W"]);
    assert!(cleanup_events(&sim).is_empty());
    sim.advance_to(100).unwrap();
    assert_eq!(normal_events(&sim), vec![(0, key_down("W"))]);
    assert_eq!(cleanup_events(&sim), vec![(100, key_up("W"))]);
    assert_eq!(
        receipt(&sim, sim.session_epoch(), "unrelated").status,
        ReceiptStatus::Completed
    );
}

#[test]
fn revoked_permit_rejects_late_input_and_late_renewal() {
    let (mut sim, permit) = ready();
    let delayed = action(&sim, &permit, "delayed", "W");
    sim.advance_to(1).unwrap();
    sim.observe(observation(&sim, "new-evidence", 2, 1_000))
        .unwrap();
    sim.revoke(RevokeExecution {
        session_epoch: sim.session_epoch(),
        permit: permit.clone(),
        reason: "target no longer valid".into(),
    })
    .unwrap();
    assert!(
        sim.renew(RenewPermit {
            session_epoch: sim.session_epoch(),
            permit,
            evidence_id: "new-evidence".into(),
            valid_until_ms: 950,
        })
        .is_err()
    );
    assert_eq!(sim.submit(delayed).status, ReceiptStatus::Rejected);
    assert!(sim.dispatch_next().is_none());
    assert!(sim.backend_events().is_empty());
}

#[test]
fn renewal_cannot_extend_the_running_commands_fixed_end() {
    let (mut sim, permit) = setup(1_000, 1_000, 200);
    let mut intent = action(&sim, &permit, "fixed-end", "W");
    intent.latest_start_ms = 100;
    start(&mut sim, intent);
    sim.advance_to(40).unwrap();
    assert!(
        sim.renew(RenewPermit {
            session_epoch: sim.session_epoch(),
            permit: permit.clone(),
            evidence_id: EVIDENCE.into(),
            valid_until_ms: 900,
        })
        .is_err()
    );
    sim.observe(observation(&sim, "new-evidence", 2, 1_000))
        .unwrap();
    sim.renew(RenewPermit {
        session_epoch: sim.session_epoch(),
        permit,
        evidence_id: "new-evidence".into(),
        valid_until_ms: 900,
    })
    .unwrap();
    sim.advance_to(300).unwrap();

    assert_eq!(cleanup_events(&sim), vec![(100, key_up("W"))]);
    assert_eq!(
        receipt(&sim, sim.session_epoch(), "fixed-end").command_end_ms,
        Some(100)
    );
    assert!(sim.held_inputs().is_empty());
}

#[test]
fn reconnect_requires_fresh_task_evidence_and_permission() {
    let (mut sim, permit) = ready();
    let old_epoch = sim.session_epoch();
    let old_action = action(&sim, &permit, "old-action", "W");
    start(&mut sim, old_action.clone());
    sim.advance_to(10).unwrap();
    sim.disconnect();
    assert!(sim.held_inputs().is_empty());
    assert_eq!(cleanup_events(&sim), vec![(10, key_up("W"))]);

    let new_epoch = sim.reconnect();
    assert_ne!(new_epoch, old_epoch);
    assert_eq!(
        receipt(&sim, old_epoch, "old-action").status,
        ReceiptStatus::Cancelled
    );
    let mut delayed = old_action;
    delayed.intent_id = "old-delayed".into();
    assert_eq!(sim.submit(delayed).status, ReceiptStatus::Rejected);
    assert!(
        sim.renew(RenewPermit {
            session_epoch: old_epoch,
            permit,
            evidence_id: EVIDENCE.into(),
            valid_until_ms: 950,
        })
        .is_err()
    );
    assert!(sim.grant_permit(TASK, TARGET, EVIDENCE, 900).is_err());

    sim.register_task(TASK, 1, 1_000).unwrap();
    sim.observe(observation(&sim, EVIDENCE, 1, 1_000)).unwrap();
    let new_permit = sim.grant_permit(TASK, TARGET, EVIDENCE, 900).unwrap();
    let new_action = action(&sim, &new_permit, "new-action", "A");
    start(&mut sim, new_action);
    sim.advance_to(110).unwrap();
    assert_eq!(
        normal_events(&sim),
        vec![(0, key_down("W")), (10, key_down("A"))]
    );
    assert!(sim.held_inputs().is_empty());
}

#[test]
fn duplicate_ids_never_replay_and_conflicting_payloads_do_not_replace_receipts() {
    let (mut sim, permit) = ready();
    let intent = action(&sim, &permit, "deduplicated", "W");
    let accepted = sim.submit(intent.clone());
    assert_eq!(sim.submit(intent.clone()), accepted);
    sim.dispatch_next().unwrap();
    assert_eq!(
        sim.submit(intent.clone()),
        receipt(&sim, sim.session_epoch(), "deduplicated")
    );
    sim.advance_to(100).unwrap();
    let completed = receipt(&sim, sim.session_epoch(), "deduplicated");
    assert_eq!(sim.submit(intent.clone()), completed);

    let mut conflicting = intent;
    conflicting.steps[0].input = key_down("A");
    assert_eq!(sim.submit(conflicting).status, ReceiptStatus::Rejected);
    assert_eq!(
        receipt(&sim, sim.session_epoch(), "deduplicated"),
        completed
    );
    assert_eq!(normal_events(&sim), vec![(0, key_down("W"))]);
}

#[test]
fn waiting_for_resources_does_not_preserve_an_expired_start_window() {
    let (mut sim, permit) = ready();
    let first = action(&sim, &permit, "blocker", "W");
    start(&mut sim, first);
    let waiting_permit = sim.grant_permit(TASK, TARGET, EVIDENCE, 900).unwrap();
    let mut waiting = action(&sim, &waiting_permit, "waiting", "A");
    waiting.latest_start_ms = 50;
    assert_eq!(sim.submit(waiting).status, ReceiptStatus::Accepted);
    assert!(sim.dispatch_next().is_none());
    sim.advance_to(100).unwrap();
    let _ = sim.dispatch_next();

    assert_eq!(normal_events(&sim), vec![(0, key_down("W"))]);
    assert!(matches!(
        receipt(&sim, sim.session_epoch(), "waiting").status,
        ReceiptStatus::Rejected | ReceiptStatus::Cancelled
    ));
}

#[test]
fn dequeue_rechecks_whether_the_complete_action_fits_its_permit() {
    let (mut sim, permit) = ready();
    let first = action(&sim, &permit, "blocker", "W");
    start(&mut sim, first);
    let short_permit = sim.grant_permit(TASK, TARGET, EVIDENCE, 150).unwrap();
    let mut waiting = action(&sim, &short_permit, "too-late-to-finish", "A");
    waiting.duration_ms = 60;
    waiting.latest_start_ms = 120;
    assert_eq!(sim.submit(waiting).status, ReceiptStatus::Accepted);
    sim.advance_to(100).unwrap();
    let _ = sim.dispatch_next();

    assert_eq!(normal_events(&sim), vec![(0, key_down("W"))]);
    assert!(matches!(
        receipt(&sim, sim.session_epoch(), "too-late-to-finish").status,
        ReceiptStatus::Rejected | ReceiptStatus::Cancelled
    ));
}

#[test]
fn a_command_can_end_at_the_deadline_but_cannot_cross_it() {
    let (mut sim, permit) = setup(100, 100, 100);
    let mut too_long = action(&sim, &permit, "too-long", "A");
    too_long.latest_start_ms = 0;
    too_long.duration_ms = 101;
    assert_eq!(sim.submit(too_long).status, ReceiptStatus::Rejected);
    let mut exact = action(&sim, &permit, "exact", "W");
    exact.latest_start_ms = 0;
    start(&mut sim, exact);
    sim.advance_to(100).unwrap();

    assert_eq!(normal_events(&sim), vec![(0, key_down("W"))]);
    assert_eq!(cleanup_events(&sim), vec![(100, key_up("W"))]);
    assert!(sim.held_inputs().is_empty());
    assert_eq!(
        receipt(&sim, sim.session_epoch(), "exact").status,
        ReceiptStatus::Completed
    );
    assert!(sim.advance_to(99).is_err());
    assert_eq!(sim.now_ms(), 100);
}

#[test]
fn untrusted_or_expired_observations_cannot_authorize_execution() {
    for case in [
        "foreign-clock",
        "future",
        "uncertain",
        "old-session",
        "wrong-surface",
        "missing-focus",
    ] {
        let mut sim = Simulator::new();
        sim.register_task(TASK, 1, 1_000).unwrap();
        let mut obs = observation(&sim, EVIDENCE, 1, 500);
        match case {
            "foreign-clock" => obs.clock_domain = "unmapped-remote-clock".into(),
            "future" => obs.observed_ms = 1,
            "uncertain" => obs.freshness = SourceFreshness::Uncertain,
            "old-session" => obs.session_epoch += 1,
            "wrong-surface" => obs.surface.epoch += 1,
            "missing-focus" => {
                obs.dependencies.remove("focus");
            }
            _ => unreachable!(),
        }
        // An implementation may retain invalid observations for diagnosis, but not as authorization.
        let _ = sim.observe(obs);
        assert!(
            sim.grant_permit(TASK, TARGET, EVIDENCE, 100).is_err(),
            "{case}"
        );
        assert!(sim.backend_events().is_empty(), "{case}");
    }
    let (mut sim, _) = setup(1_000, 100, 100);
    sim.advance_to(100).unwrap();
    assert!(sim.grant_permit(TASK, TARGET, EVIDENCE, 200).is_err());
}

#[test]
fn callers_cannot_weaken_required_dependencies_or_swap_the_authorized_target() {
    for case in [
        "missing-focus",
        "wrong-target",
        "wrong-task-revision",
        "wrong-surface",
        "wrong-generation",
    ] {
        let (mut sim, permit) = ready();
        let mut intent = action(&sim, &permit, case, "W");
        match case {
            "missing-focus" => {
                intent.dependencies.remove("focus");
            }
            "wrong-target" => intent.target_ref = "other-target".into(),
            "wrong-task-revision" => intent.task_revision += 1,
            "wrong-surface" => intent.surface.epoch += 1,
            "wrong-generation" => intent.permit.generation += 1,
            _ => unreachable!(),
        }
        assert_eq!(sim.submit(intent).status, ReceiptStatus::Rejected, "{case}");
        assert!(sim.backend_events().is_empty(), "{case}");
    }
}

#[test]
fn cancelling_a_task_releases_input_and_prevents_new_attempts() {
    let (mut sim, permit) = ready();
    let first = action(&sim, &permit, "cancelled-task", "W");
    start(&mut sim, first);
    sim.advance_to(15).unwrap();
    sim.cancel_task(TASK).unwrap();
    assert_eq!(cleanup_events(&sim), vec![(15, key_up("W"))]);
    assert!(sim.held_inputs().is_empty());
    let retry = action(&sim, &permit, "new-attempt-same-task", "A");
    assert_eq!(sim.submit(retry).status, ReceiptStatus::Rejected);
    assert!(sim.grant_permit(TASK, TARGET, EVIDENCE, 900).is_err());
}

#[test]
fn a_rejected_later_submission_reports_partial_and_cleans_the_committed_prefix() {
    let (mut sim, permit) = ready();
    let mut intent = action(&sim, &permit, "partial", "W");
    intent.steps.extend([
        step(20, key_down("A")),
        step(40, Input::PointerMove { x: 20, y: 20 }),
    ]);
    sim.fail_submission_on(2, SubmissionFault::Rejected);
    start(&mut sim, intent);
    sim.advance_to(100).unwrap();

    assert_eq!(normal_events(&sim), vec![(0, key_down("W"))]);
    assert_eq!(cleanup_events(&sim), vec![(20, key_up("W"))]);
    let result = receipt(&sim, sim.session_epoch(), "partial");
    assert_eq!(result.status, ReceiptStatus::Partial);
    assert_eq!(result.release_status, ReleaseStatus::Released);
    assert!(sim.held_inputs().is_empty());
}

#[test]
fn a_rejected_first_submission_has_no_committed_prefix_or_cleanup() {
    let (mut sim, permit) = ready();
    let mut intent = action(&sim, &permit, "rejected-first", "W");
    intent.steps.push(step(20, key_down("A")));
    sim.fail_submission_on(1, SubmissionFault::Rejected);
    assert_eq!(sim.submit(intent).status, ReceiptStatus::Accepted);
    assert_eq!(sim.dispatch_next().unwrap().status, ReceiptStatus::Rejected);
    sim.advance_to(100).unwrap();

    let result = receipt(&sim, sim.session_epoch(), "rejected-first");
    assert_eq!(result.status, ReceiptStatus::Rejected);
    assert_eq!(result.release_status, ReleaseStatus::NotNeeded);
    assert!(result.submitted.is_empty());
    assert!(sim.backend_events().is_empty());
    assert!(sim.held_inputs().is_empty());
}

#[test]
fn uncertain_submission_never_replays_or_executes_the_remaining_steps() {
    for fault in [
        SubmissionFault::UnknownApplied,
        SubmissionFault::UnknownNotApplied,
    ] {
        let (mut sim, permit) = ready();
        let mut intent = action(&sim, &permit, "unknown", "W");
        intent.steps.push(step(20, key_down("A")));
        let expected_actual_inputs = if matches!(&fault, SubmissionFault::UnknownApplied) {
            vec![(0, key_down("W"))]
        } else {
            vec![]
        };
        sim.fail_submission_on(1, fault);
        assert_eq!(sim.submit(intent.clone()).status, ReceiptStatus::Accepted);
        sim.dispatch_next().unwrap();
        sim.advance_to(100).unwrap();
        let result = receipt(&sim, sim.session_epoch(), "unknown");
        assert_eq!(result.status, ReceiptStatus::Unknown);
        assert_eq!(sim.submit(intent), result);
        assert_eq!(normal_events(&sim), expected_actual_inputs);
        assert!(sim.held_inputs().is_empty());
        assert!(sim.dispatch_next().is_none());
    }
}

#[test]
fn release_failure_isolates_the_channel_until_cleanup_really_succeeds() {
    let (mut sim, permit) = ready();
    let first = action(&sim, &permit, "release-failed", "W");
    start(&mut sim, first);
    sim.advance_to(10).unwrap();
    sim.set_release_failure(true);
    sim.revoke(RevokeExecution {
        session_epoch: sim.session_epoch(),
        permit,
        reason: "operator stop".into(),
    })
    .unwrap();

    assert!(sim.is_isolated());
    assert!(sim.held_inputs().contains(&"key:W".into()));
    assert_eq!(
        receipt(&sim, sim.session_epoch(), "release-failed").release_status,
        ReleaseStatus::Failed
    );
    assert!(sim.recover_channel().is_err());
    assert!(sim.is_isolated());
    let fresh_permit = sim.grant_permit(TASK, TARGET, EVIDENCE, 900);
    if let Ok(permit) = fresh_permit {
        let blocked = action(&sim, &permit, "blocked-by-isolation", "A");
        assert_eq!(sim.submit(blocked).status, ReceiptStatus::Rejected);
    }
    assert_eq!(normal_events(&sim), vec![(0, key_down("W"))]);

    sim.set_release_failure(false);
    sim.recover_channel().unwrap();
    assert!(!sim.is_isolated());
    assert!(sim.held_inputs().is_empty());
    assert_eq!(cleanup_events(&sim), vec![(10, key_up("W"))]);
}

#[test]
fn cleanup_never_releases_a_key_held_by_an_external_actor() {
    let (mut sim, permit) = ready();
    sim.hold_external_key("SHIFT");
    let intent = action(&sim, &permit, "owned-only", "W");
    start(&mut sim, intent);
    sim.advance_to(10).unwrap();
    sim.update_dependency("focus", Some(false));

    assert_eq!(normal_events(&sim), vec![(0, key_down("W"))]);
    assert_eq!(cleanup_events(&sim), vec![(10, key_up("W"))]);
    assert!(
        !sim.backend_events()
            .iter()
            .any(|event| event.input == key_up("SHIFT"))
    );
}
