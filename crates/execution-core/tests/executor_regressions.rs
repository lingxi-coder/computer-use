use execution_core::sim::Simulator;
use interaction_contracts::i0::*;

fn setup() -> (Simulator, PermitRef) {
    let mut sim = Simulator::new();
    sim.register_task("task", 1, 1000).unwrap();
    observe(&mut sim, "obs", 1);
    let permit = sim.grant_permit("task", "target", "obs", 500).unwrap();
    (sim, permit)
}

fn observe(sim: &mut Simulator, id: &str, sequence: u64) {
    sim.observe(Observation {
        observation_id: id.into(),
        session_epoch: sim.session_epoch(),
        surface: sim.surface(),
        target_ref: "target".into(),
        clock_domain: sim.clock_domain(),
        source_sequence: sequence,
        observed_ms: sim.now_ms(),
        valid_until_ms: 800,
        freshness: SourceFreshness::Verified,
        dependencies: sim.dependency_versions(),
    })
    .unwrap();
}

fn action(sim: &Simulator, permit: PermitRef, id: &str) -> ActionIntent {
    ActionIntent {
        intent_id: id.into(),
        task_id: "task".into(),
        task_revision: 1,
        session_epoch: sim.session_epoch(),
        permit,
        surface: sim.surface(),
        target_ref: "target".into(),
        coordinate_space: CoordinateSpace::SurfacePx,
        dependencies: sim.dependency_versions(),
        evidence_id: "obs".into(),
        latest_start_ms: 100,
        duration_ms: 50,
        steps: vec![ScheduledInput {
            offset_ms: 0,
            input: Input::KeyDown { key: "W".into() },
        }],
    }
}

#[test]
fn later_ownership_rejection_is_partial_and_preserves_external_input() {
    let (mut sim, permit) = setup();
    sim.hold_external_key("Shift");
    let mut intent = action(&sim, permit, "partial");
    intent.steps.push(ScheduledInput {
        offset_ms: 10,
        input: Input::KeyDown {
            key: "Shift".into(),
        },
    });
    sim.submit(intent);
    sim.dispatch_next();
    sim.advance_to(10).unwrap();
    let receipt = sim.query_receipt(1, "partial").unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Partial);
    assert_eq!(receipt.release_status, ReleaseStatus::Released);
    assert_eq!(sim.held_inputs(), vec!["key:Shift"]);
    assert_eq!(receipt.submitted.len(), 2); // W down and cleanup, never Shift down.
}

#[test]
fn recovering_release_failure_cannot_revive_other_queued_permits() {
    let (mut sim, first) = setup();
    let second = sim.grant_permit("task", "target", "obs", 500).unwrap();
    sim.submit(action(&sim, first.clone(), "first"));
    sim.dispatch_next();
    sim.submit(action(&sim, second.clone(), "queued"));
    sim.set_release_failure(true);
    sim.revoke(RevokeExecution {
        session_epoch: 1,
        permit: first,
        reason: "cancel".into(),
    })
    .unwrap();
    assert!(sim.is_isolated());
    assert_eq!(
        sim.query_receipt(1, "queued").unwrap().status,
        ReceiptStatus::Cancelled
    );
    sim.set_release_failure(false);
    sim.recover_channel().unwrap();
    assert!(sim.dispatch_next().is_none());
    let late = action(&sim, second, "late");
    assert_eq!(sim.submit(late).status, ReceiptStatus::Rejected);
    assert_eq!(sim.backend_events().len(), 2);
}

#[test]
fn failed_cleanup_at_normal_end_permanently_revokes_its_permit() {
    let (mut sim, permit) = setup();
    sim.submit(action(&sim, permit.clone(), "first"));
    sim.dispatch_next();
    sim.set_release_failure(true);
    sim.advance_to(50).unwrap();
    assert_eq!(
        sim.query_receipt(1, "first").unwrap().status,
        ReceiptStatus::Unknown
    );
    sim.set_release_failure(false);
    sim.recover_channel().unwrap();
    assert_eq!(
        sim.submit(action(&sim, permit, "late")).status,
        ReceiptStatus::Rejected
    );
}

#[test]
fn reconnect_preserves_root_deadline_and_requires_explicit_same_budget_authorization() {
    let (mut sim, _) = setup();
    sim.reconnect();
    observe(&mut sim, "obs", 1);
    assert!(sim.grant_permit("task", "target", "obs", 500).is_err());
    assert!(sim.register_task("task", 1, 2000).is_err());
    assert!(sim.register_task("task", 2, 1000).is_err());
    sim.register_task("task", 1, 1000).unwrap();
    assert!(sim.grant_permit("task", "target", "obs", 500).is_ok());
}

#[test]
fn explicit_task_cancellation_survives_reconnection() {
    let (mut sim, _) = setup();
    sim.cancel_task("task").unwrap();
    sim.reconnect();
    observe(&mut sim, "obs", 1);
    assert!(sim.register_task("task", 1, 1000).is_err());
    assert!(sim.grant_permit("task", "target", "obs", 500).is_err());
    sim.register_task("new-explicit-task", 1, 1000).unwrap();
    assert!(
        sim.grant_permit("new-explicit-task", "target", "obs", 500)
            .is_ok()
    );
}

#[test]
fn bulk_revocation_is_idempotent_without_recursing_per_queued_intent() {
    let (mut sim, permit) = setup();
    for index in 0..5000 {
        let intent = action(&sim, permit.clone(), &format!("queued-{index}"));
        assert_eq!(sim.submit(intent).status, ReceiptStatus::Accepted);
    }
    let request = RevokeExecution {
        session_epoch: 1,
        permit,
        reason: "bulk cancellation".into(),
    };
    sim.revoke(request.clone()).unwrap();
    sim.revoke(request).unwrap();
    assert!(sim.dispatch_next().is_none());
    assert!(sim.backend_events().is_empty());
    for index in 0..5000 {
        assert_eq!(
            sim.query_receipt(1, &format!("queued-{index}"))
                .unwrap()
                .status,
            ReceiptStatus::Cancelled
        );
    }
}
