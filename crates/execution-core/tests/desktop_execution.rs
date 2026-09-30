//! The execution contracts of design v0.5 (14.3, 14.8, 26.3 and the EXE clauses),
//! exercised directly on the production state machine.

use std::collections::BTreeMap;

use execution_core::core::{CoreConfig, CoreEvent, ExecutionCore, PermitState};
use execution_core::sim_backend::SimBackend;
use interaction_contracts::desktop::device::*;
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::primitive::{
    AxActionKind, Modifiers, MouseButton, Primitive, PrimitivePolicy,
};

type Core = ExecutionCore<SimBackend>;

const FINGERPRINT: &str = "fp1:expected";

/// Production configuration: atomic primitives only, binding proofs and a lease required.
fn production() -> Core {
    let mut core = ExecutionCore::new(CoreConfig::production("test"), SimBackend::new());
    core.renew_lease(10_000);
    core
}

/// Production configuration with held-class primitives enabled, for cleanup tests.
fn production_with_held() -> Core {
    let mut config = CoreConfig::production("test");
    config.policy = PrimitivePolicy::AllowHeld;
    let mut core = ExecutionCore::new(config, SimBackend::new());
    core.renew_lease(10_000);
    core
}

fn register(core: &mut Core, root: &str, deadline_ms: u64) {
    core.register_root(&RootGrant {
        root_id: root.into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms,
    })
    .unwrap();
}

fn observe(core: &mut Core, id: &str, sequence: u64, target: &str, until: u64) {
    core.observe(Observation {
        observation_id: id.into(),
        session_epoch: core.session_epoch(),
        surface: core.surface(),
        target_ref: target.into(),
        clock_domain: core.clock_domain(),
        source_sequence: sequence,
        observed_ms: core.now_ms(),
        valid_until_ms: until,
        freshness: SourceFreshness::Verified,
        dependencies: core.dependency_versions(),
    })
    .unwrap();
}

fn grant(
    core: &mut Core,
    root: &str,
    target: &str,
    evidence: &str,
    acked: u64,
) -> Result<PermitRef, execution_core::core::Denial> {
    core.grant_permit(&GrantPermit {
        acked_stop_seq: acked,
        task_id: root.into(),
        target_ref: target.into(),
        evidence_id: evidence.into(),
        valid_until_ms: 1_000,
    })
}

/// Root, evidence and a permit for one target, all at time zero.
fn ready(core: &mut Core, root: &str, target: &str) -> PermitRef {
    register(core, root, 5_000);
    let evidence = format!("obs-{root}");
    observe(core, &evidence, 1, target, 2_000);
    grant(core, root, target, &evidence, 0).unwrap()
}

fn ax_press() -> Primitive {
    Primitive::AxAction {
        action: AxActionKind::Press,
    }
}

fn click() -> Primitive {
    Primitive::Click {
        button: MouseButton::Left,
        x: 10,
        y: 10,
        modifiers: Modifiers::default(),
    }
}

fn intent_for(
    core: &Core,
    permit: &PermitRef,
    root: &str,
    target: &str,
    id: &str,
    steps: Vec<ScheduledPrimitive>,
) -> ActionIntent {
    ActionIntent {
        intent_id: id.into(),
        attempt_id: Some(format!("attempt-{id}")),
        task_id: root.into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: core.session_epoch(),
        permit: permit.clone(),
        surface: core.surface(),
        target_ref: target.into(),
        binding_fingerprint: Some(FINGERPRINT.into()),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: core.dependency_versions(),
        evidence_id: format!("obs-{root}"),
        latest_start_ms: 500,
        duration_ms: 100,
        steps,
    }
}

/// An intent on the default target.
fn intent(
    core: &Core,
    permit: &PermitRef,
    root: &str,
    id: &str,
    steps: Vec<ScheduledPrimitive>,
) -> ActionIntent {
    intent_for(core, permit, root, "target", id, steps)
}

fn step(offset_ms: u64, primitive: Primitive) -> ScheduledPrimitive {
    ScheduledPrimitive {
        offset_ms,
        primitive,
    }
}

fn send(core: &mut Core, intent: ActionIntent, acked: u64) -> Receipt {
    core.submit(SubmitIntent {
        acked_stop_seq: acked,
        intent,
    })
}

fn start(core: &mut Core, intent: ActionIntent) -> Receipt {
    assert_eq!(send(core, intent, 0).status, ReceiptStatus::Accepted);
    let running = core.dispatch_next().expect("idle channel can dispatch");
    assert_eq!(running.status, ReceiptStatus::Running);
    running
}

fn stop_all(reason: &str) -> StopCommand {
    StopCommand {
        scope: StopScope::All,
        disposition: StopDisposition::Pause,
        source: StopSource::OperatorButton,
        reason: reason.into(),
    }
}

fn receipt(core: &Core, id: &str) -> Receipt {
    let found = core
        .query_receipt(core.session_epoch(), id)
        .expect("intent has a receipt");
    found.validate().expect("runtime receipt is protocol-valid");
    found
}

// ---- Atomic-only policy (EXE-08, design 5.3) -------------------------------

#[test]
fn production_policy_refuses_held_class_primitives_without_touching_the_backend() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    let held = intent(
        &core,
        &permit,
        "root",
        "held",
        vec![step(0, Primitive::KeyDown { key: "W".into() })],
    );
    let result = send(&mut core, held, 0);
    assert_eq!(result.status, ReceiptStatus::Rejected);
    assert_eq!(result.reason_code, Some(FailureCode::InputRejected));
    assert!(core.backend().events().is_empty());
    assert_eq!(core.held_input_count(), 0);
}

#[test]
fn atomic_primitives_run_and_owe_no_release() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    let work = intent(
        &core,
        &permit,
        "root",
        "press",
        vec![step(0, ax_press()), step(20, click())],
    );
    start(&mut core, work);
    core.advance_to(100).unwrap();
    let done = receipt(&core, "press");
    assert_eq!(done.status, ReceiptStatus::Completed);
    assert_eq!(done.release_status, ReleaseStatus::NotNeeded);
    assert_eq!(done.submitted.len(), 2);
    assert!(done.submitted.iter().all(|entry| !entry.cleanup));
    assert_eq!(core.held_input_count(), 0);
    assert_eq!(core.backend().events().len(), 2);
}

// ---- Stop latch (EXE-04, EXE-05, design 14.8) -------------------------------

#[test]
fn a_stop_that_beat_the_host_makes_every_lagging_request_fail_with_stop_latched() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    let work = intent(&core, &permit, "root", "a", vec![step(0, ax_press())]);
    start(&mut core, work);

    let ack = core.stop(&stop_all("user pressed stop"));
    assert_eq!(ack.stop_seq, 1);
    assert_eq!(ack.stop_floor, 1);
    assert_eq!(ack.revoked_permits, 1);
    assert_eq!(receipt(&core, "a").status, ReceiptStatus::Cancelled);
    assert_eq!(
        receipt(&core, "a").reason_code,
        Some(FailureCode::Cancelled)
    );

    // The host has not processed the stop yet: its requests are refused.
    let refused = grant(&mut core, "root", "target", "obs-root", 0).unwrap_err();
    assert_eq!(refused.code, Some(FailureCode::StopLatched));
    let again = intent(&core, &permit, "root", "b", vec![step(0, ax_press())]);
    let late = send(&mut core, again, 0);
    assert_eq!(late.status, ReceiptStatus::Rejected);
    assert_eq!(late.reason_code, Some(FailureCode::StopLatched));
    assert!(core.dispatch_next().is_none());
    assert_eq!(core.backend().events().len(), 1, "no input after the stop");
}

#[test]
fn catching_up_with_the_stop_is_not_enough_to_resume_a_paused_root() {
    let mut core = production();
    ready(&mut core, "root", "target");
    core.stop(&stop_all("pause"));
    let still_paused = grant(&mut core, "root", "target", "obs-root", 1).unwrap_err();
    assert_eq!(still_paused.code, Some(FailureCode::StopLatched));
    assert!(
        still_paused.reason.contains("paused"),
        "{}",
        still_paused.reason
    );

    let ack = core.resume(&ResumeCommand {
        scope: StopScope::Root {
            root_id: "root".into(),
        },
    });
    assert!(ack.resumed);
    // Old permits stay revoked; a new one needs the host to have caught up.
    assert!(grant(&mut core, "root", "target", "obs-root", 0).is_err());
    let fresh = grant(&mut core, "root", "target", "obs-root", 1).unwrap();
    assert_eq!(core.permit_state(&fresh.id), Some(PermitState::Issued));
}

#[test]
fn a_cancelled_root_never_gets_another_permit_and_cannot_resume_or_re_register() {
    let mut core = production();
    ready(&mut core, "root", "target");
    core.stop(&StopCommand {
        scope: StopScope::Root {
            root_id: "root".into(),
        },
        disposition: StopDisposition::Cancel,
        source: StopSource::GlobalHotkey,
        reason: "hotkey".into(),
    });
    let refused = grant(&mut core, "root", "target", "obs-root", 1).unwrap_err();
    assert_eq!(refused.code, Some(FailureCode::Cancelled));
    let resume = core.resume(&ResumeCommand {
        scope: StopScope::Root {
            root_id: "root".into(),
        },
    });
    assert!(!resume.resumed);
    assert!(resume.reason.unwrap().contains("cancelled"));
    assert!(
        core.register_root(&RootGrant {
            root_id: "root".into(),
            revision: 1,
            root_epoch: 1,
            deadline_ms: 5_000
        })
        .is_err()
    );
}

#[test]
fn a_stop_scoped_to_one_root_leaves_the_other_one_running() {
    let mut core = production();
    let first = ready(&mut core, "r1", "t1");
    register(&mut core, "r2", 5_000);
    observe(&mut core, "obs-r2", 2, "t2", 2_000);
    let second = grant(&mut core, "r2", "t2", "obs-r2", 0).unwrap();
    let work = intent_for(
        &core,
        &second,
        "r2",
        "t2",
        "second",
        vec![step(0, ax_press())],
    );
    start(&mut core, work);

    let ack = core.stop(&StopCommand {
        scope: StopScope::Root {
            root_id: "r1".into(),
        },
        disposition: StopDisposition::Pause,
        source: StopSource::OperatorButton,
        reason: "pause r1".into(),
    });
    assert_eq!(ack.revoked_permits, 1);
    assert_eq!(core.permit_state(&first.id), Some(PermitState::Revoked));
    assert_eq!(core.permit_state(&second.id), Some(PermitState::InUse));
    core.advance_to(100).unwrap();
    assert_eq!(receipt(&core, "second").status, ReceiptStatus::Completed);
}

#[test]
fn a_stop_before_dispatch_cancels_queued_work_without_any_backend_input() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    let queued = intent(&core, &permit, "root", "queued", vec![step(0, ax_press())]);
    assert_eq!(send(&mut core, queued, 0).status, ReceiptStatus::Accepted);
    core.stop(&stop_all("stop"));
    assert!(core.dispatch_next().is_none());
    assert_eq!(receipt(&core, "queued").status, ReceiptStatus::Cancelled);
    assert!(core.backend().events().is_empty());
}

#[test]
fn a_stop_releases_held_input_without_waiting_for_anything() {
    let mut core = production_with_held();
    let permit = ready(&mut core, "root", "target");
    let mut holding = intent(
        &core,
        &permit,
        "root",
        "hold",
        vec![step(0, Primitive::KeyDown { key: "W".into() })],
    );
    holding.binding_fingerprint = None;
    start(&mut core, holding);
    assert_eq!(core.held_input_count(), 1);

    let ack = core.stop(&stop_all("stop"));
    assert_eq!(ack.cleanup, ReleaseStatus::Released);
    assert_eq!(core.held_input_count(), 0);
    assert!(core.backend().held_tokens().is_empty());
    let cancelled = receipt(&core, "hold");
    assert_eq!(cancelled.status, ReceiptStatus::Cancelled);
    assert!(cancelled.submitted.iter().any(|entry| entry.cleanup));
}

#[test]
fn an_unconfirmed_release_during_a_stop_is_reported_and_isolates_the_channel() {
    let mut core = production_with_held();
    let permit = ready(&mut core, "root", "target");
    let mut holding = intent(
        &core,
        &permit,
        "root",
        "hold",
        vec![step(0, Primitive::KeyDown { key: "W".into() })],
    );
    holding.binding_fingerprint = None;
    start(&mut core, holding);
    core.backend_mut().set_release_failure(true);
    core.drain_events();

    let ack = core.stop(&stop_all("stop"));
    assert_eq!(
        ack.cleanup,
        ReleaseStatus::Failed,
        "the stop must not claim it released"
    );
    assert!(core.is_isolated());
    assert_eq!(receipt(&core, "hold").status, ReceiptStatus::Unknown);
    let events = core.drain_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, CoreEvent::Isolated { .. }))
    );

    core.backend_mut().set_release_failure(false);
    core.recover_channel().unwrap();
    assert!(!core.is_isolated());
    assert!(core.drain_events().contains(&CoreEvent::Recovered));
}

// ---- Human input (EXE-06) ---------------------------------------------------

#[test]
fn human_input_pauses_everything_with_its_own_failure_code() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    let work = intent(&core, &permit, "root", "a", vec![step(0, ax_press())]);
    start(&mut core, work);

    let ack = core.human_input_detected();
    assert_eq!(ack.disposition, StopDisposition::Pause);
    let cancelled = receipt(&core, "a");
    assert_eq!(cancelled.status, ReceiptStatus::Cancelled);
    assert_eq!(cancelled.reason_code, Some(FailureCode::HumanInputDetected));
    let refused = grant(&mut core, "root", "target", "obs-root", ack.stop_seq).unwrap_err();
    assert_eq!(
        refused.code,
        Some(FailureCode::StopLatched),
        "paused until the user resumes"
    );
}

// ---- Point-of-hit check (EXE-07, design 26.3 occlusion row) ------------------

fn dispatch_with_hit(result: SimHitTest, primitive: Primitive) -> (Core, Receipt, PermitRef) {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    core.backend_mut().set_hit_test(result);
    let work = intent(&core, &permit, "root", "hit", vec![step(0, primitive)]);
    assert_eq!(send(&mut core, work, 0).status, ReceiptStatus::Accepted);
    core.dispatch_next();
    core.advance_to(100).unwrap();
    let outcome = receipt(&core, "hit");
    (core, outcome, permit)
}

#[test]
fn a_covered_target_is_refused_before_anything_is_submitted() {
    for primitive in [click(), ax_press()] {
        let (core, outcome, permit) = dispatch_with_hit(
            SimHitTest::Element {
                fingerprint: "fp1:something-else".into(),
            },
            primitive,
        );
        assert_eq!(outcome.status, ReceiptStatus::Rejected);
        assert_eq!(outcome.reason_code, Some(FailureCode::TargetOccluded));
        assert!(
            core.backend().events().is_empty(),
            "nothing reached the system"
        );
        assert_eq!(core.permit_state(&permit.id), Some(PermitState::Revoked));
    }
}

#[test]
fn an_unresolved_hit_test_refuses_with_context_unresolved() {
    let (core, outcome, _) = dispatch_with_hit(SimHitTest::Unresolved, click());
    assert_eq!(outcome.status, ReceiptStatus::Rejected);
    assert_eq!(outcome.reason_code, Some(FailureCode::ContextUnresolved));
    assert!(core.backend().events().is_empty());
}

#[test]
fn a_matching_hit_test_lets_the_action_through() {
    let (core, outcome, _) = dispatch_with_hit(SimHitTest::Match, click());
    assert_eq!(outcome.status, ReceiptStatus::Completed);
    assert_eq!(core.backend().events().len(), 1);
}

#[test]
fn production_refuses_pointer_and_ax_actions_that_carry_no_binding_proof() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    let mut bare = intent(&core, &permit, "root", "bare", vec![step(0, ax_press())]);
    bare.binding_fingerprint = None;
    assert_eq!(send(&mut core, bare, 0).status, ReceiptStatus::Accepted);
    core.dispatch_next();
    let outcome = receipt(&core, "bare");
    assert_eq!(outcome.status, ReceiptStatus::Rejected);
    assert_eq!(outcome.reason_code, Some(FailureCode::ContextUnresolved));
    assert!(core.backend().events().is_empty());
}

#[test]
fn a_rejected_later_step_reports_partial_and_keeps_the_earlier_input() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    let work = intent(
        &core,
        &permit,
        "root",
        "two",
        vec![
            step(0, Primitive::TypeText { text: "hi".into() }),
            step(10, click()),
        ],
    );
    start(&mut core, work);
    core.backend_mut().set_hit_test(SimHitTest::Element {
        fingerprint: "fp1:other".into(),
    });
    core.advance_to(10).unwrap();
    let outcome = receipt(&core, "two");
    assert_eq!(outcome.status, ReceiptStatus::Partial);
    assert_eq!(outcome.reason_code, Some(FailureCode::TargetOccluded));
    assert_eq!(
        outcome.submitted.len(),
        1,
        "the text went out, the click did not"
    );
}

// ---- Control lease (OPS-02) ---------------------------------------------------

#[test]
fn nothing_is_admitted_until_the_host_establishes_a_lease() {
    let mut core = ExecutionCore::new(CoreConfig::production("test"), SimBackend::new());
    register(&mut core, "root", 5_000);
    observe(&mut core, "obs-root", 1, "target", 2_000);
    let refused = grant(&mut core, "root", "target", "obs-root", 0).unwrap_err();
    assert!(refused.reason.contains("lease"), "{}", refused.reason);
}

#[test]
fn a_lapsed_lease_revokes_permits_and_blocks_new_requests_until_renewed() {
    let mut core = ExecutionCore::new(CoreConfig::production("test"), SimBackend::new());
    // The lease ends while the 100 ms action is still running.
    core.renew_lease(60);
    let permit = ready(&mut core, "root", "target");
    let work = intent(
        &core,
        &permit,
        "root",
        "a",
        vec![step(0, ax_press()), step(50, ax_press())],
    );
    start(&mut core, work);
    assert_eq!(
        core.next_wakeup(),
        Some(50),
        "a step is due before the lease ends"
    );
    core.advance_to(60).unwrap();

    assert_eq!(core.permit_state(&permit.id), Some(PermitState::Revoked));
    let cancelled = receipt(&core, "a");
    assert_eq!(cancelled.status, ReceiptStatus::Cancelled);
    assert_eq!(
        cancelled.submitted.len(),
        2,
        "both steps were due before the lease lapsed"
    );
    assert!(core.drain_events().contains(&CoreEvent::LeaseLost));
    assert!(grant(&mut core, "root", "target", "obs-root", 0).is_err());

    core.renew_lease(1_000);
    assert!(grant(&mut core, "root", "target", "obs-root", 0).is_ok());
}

// ---- Permit lifecycle (design 14.3) ---------------------------------------------

#[test]
fn a_permit_moves_through_issued_in_use_expired_and_revoked() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    assert_eq!(core.permit_state(&permit.id), Some(PermitState::Issued));
    let work = intent(&core, &permit, "root", "a", vec![step(0, ax_press())]);
    start(&mut core, work);
    assert_eq!(core.permit_state(&permit.id), Some(PermitState::InUse));
    core.advance_to(100).unwrap();
    assert_eq!(
        core.permit_state(&permit.id),
        Some(PermitState::Issued),
        "a completed action does not spend the permit"
    );
    core.advance_to(1_000).unwrap();
    assert_eq!(
        core.permit_state(&permit.id),
        Some(PermitState::Revoked),
        "expiry is enforced, not just noticed"
    );
    assert_eq!(core.permit_state("nope"), None);
}

#[test]
fn an_intent_admitted_under_another_root_epoch_is_refused() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    let mut stale = intent(&core, &permit, "root", "stale", vec![step(0, ax_press())]);
    stale.root_epoch = 2;
    let outcome = send(&mut core, stale, 0);
    assert_eq!(outcome.status, ReceiptStatus::Rejected);
    assert!(outcome.reason.unwrap().contains("root epoch"));
}

// ---- Events for the shell ---------------------------------------------------------

#[test]
fn receipt_changes_are_reported_in_order() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    core.drain_events();
    let work = intent(&core, &permit, "root", "a", vec![step(0, ax_press())]);
    start(&mut core, work);
    core.advance_to(100).unwrap();
    let statuses: Vec<ReceiptStatus> = core
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            CoreEvent::ReceiptChanged(receipt) => Some(receipt.status),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        vec![
            ReceiptStatus::Accepted,
            ReceiptStatus::Running,
            ReceiptStatus::Completed
        ]
    );
    assert!(core.drain_events().is_empty(), "draining empties the queue");
}

#[test]
fn successive_stops_get_increasing_sequence_numbers() {
    let mut core = production();
    let first = core.stop(&stop_all("one"));
    let second = core.stop(&stop_all("two"));
    assert_eq!((first.stop_seq, second.stop_seq), (1, 2));
    assert_eq!(second.cleanup, ReleaseStatus::NotNeeded);
}

#[test]
fn dependencies_are_tracked_like_any_other_guard() {
    let mut core = production();
    let permit = ready(&mut core, "root", "target");
    let work = intent(&core, &permit, "root", "a", vec![step(0, ax_press())]);
    start(&mut core, work);
    let revisions: BTreeMap<String, u64> = core.dependency_versions();
    core.update_dependency("layer", Some(false));
    assert_eq!(receipt(&core, "a").status, ReceiptStatus::Cancelled);
    assert!(core.dependency_versions()["layer"] > revisions["layer"]);
}
