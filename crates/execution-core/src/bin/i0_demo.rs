use execution_core::sim::*;
use interaction_contracts::i0::*;
use serde_json::{Value, json};

fn seed() -> Result<(Simulator, PermitRef), String> {
    let mut sim = Simulator::new();
    sim.register_task("demo-task", 1, 1000)?;
    sim.observe(observation(&sim, "frame-1", 1, 500))?;
    let permit = sim.grant_permit("demo-task", "target-1", "frame-1", 400)?;
    Ok((sim, permit))
}

fn observation(sim: &Simulator, id: &str, sequence: u64, until: u64) -> Observation {
    Observation {
        observation_id: id.into(),
        session_epoch: sim.session_epoch(),
        surface: sim.surface(),
        target_ref: "target-1".into(),
        clock_domain: sim.clock_domain(),
        source_sequence: sequence,
        observed_ms: sim.now_ms(),
        valid_until_ms: until,
        freshness: SourceFreshness::Verified,
        dependencies: sim.dependency_versions(),
    }
}

fn action(sim: &Simulator, permit: &PermitRef) -> ActionIntent {
    ActionIntent {
        intent_id: "intent-1".into(),
        task_id: "demo-task".into(),
        task_revision: 1,
        session_epoch: sim.session_epoch(),
        permit: permit.clone(),
        surface: sim.surface(),
        target_ref: "target-1".into(),
        coordinate_space: CoordinateSpace::SurfacePx,
        dependencies: sim.dependency_versions(),
        evidence_id: "frame-1".into(),
        latest_start_ms: 100,
        duration_ms: 100,
        steps: vec![ScheduledInput {
            offset_ms: 0,
            input: Input::KeyDown { key: "W".into() },
        }],
    }
}

fn result(name: &str, sim: &Simulator, expected: ReceiptStatus) -> Result<Value, String> {
    let receipt = sim
        .query_receipt(1, "intent-1")
        .ok_or("missing demo receipt")?;
    let passed = receipt.status == expected;
    let events: Vec<Value> = sim
        .backend_events()
        .iter()
        .map(|event| {
            json!({
                "at_ms": event.at_ms, "input": event.input, "cleanup": event.cleanup,
            })
        })
        .collect();
    Ok(
        json!({ "scenario": name, "passed": passed, "receipt": receipt,
        "held_inputs": sim.held_inputs(), "isolated": sim.is_isolated(), "backend_truth": events }),
    )
}

fn run() -> Result<Value, String> {
    let mut scenarios = Vec::new();

    let (mut sim, permit) = seed()?;
    sim.submit(action(&sim, &permit));
    sim.dispatch_next();
    sim.advance_to(1000)?;
    scenarios.push(result(
        "bounded_end_on_large_clock_jump",
        &sim,
        ReceiptStatus::Completed,
    )?);

    let (mut sim, permit) = seed()?;
    sim.submit(action(&sim, &permit));
    sim.update_dependency("focus", Some(false));
    sim.dispatch_next();
    scenarios.push(result(
        "invalidate_while_queued",
        &sim,
        ReceiptStatus::Cancelled,
    )?);

    let (mut sim, permit) = seed()?;
    sim.submit(action(&sim, &permit));
    sim.dispatch_next();
    sim.advance_to(25)?;
    sim.update_dependency("layer", None);
    scenarios.push(result(
        "release_after_active_layer_becomes_unknown",
        &sim,
        ReceiptStatus::Cancelled,
    )?);

    let (mut sim, permit) = seed()?;
    sim.submit(action(&sim, &permit));
    sim.dispatch_next();
    sim.advance_to(20)?;
    sim.observe(observation(&sim, "frame-2", 2, 700))?;
    sim.renew(RenewPermit {
        session_epoch: 1,
        permit,
        evidence_id: "frame-2".into(),
        valid_until_ms: 600,
    })?;
    sim.advance_to(150)?;
    scenarios.push(result(
        "renewal_does_not_extend_action",
        &sim,
        ReceiptStatus::Completed,
    )?);

    let (mut sim, permit) = seed()?;
    sim.submit(action(&sim, &permit));
    sim.revoke(RevokeExecution {
        session_epoch: 1,
        permit: permit.clone(),
        reason: "operator cancellation".into(),
    })?;
    let rejected_renewal = sim
        .renew(RenewPermit {
            session_epoch: 1,
            permit,
            evidence_id: "frame-1".into(),
            valid_until_ms: 400,
        })
        .is_err();
    let mut report = result(
        "revocation_fences_late_renewal",
        &sim,
        ReceiptStatus::Cancelled,
    )?;
    report["late_renewal_rejected"] = json!(rejected_renewal);
    report["passed"] = json!(report["passed"].as_bool().unwrap_or(false) && rejected_renewal);
    scenarios.push(report);

    let (mut sim, permit) = seed()?;
    let mut intent = action(&sim, &permit);
    intent.steps.push(ScheduledInput {
        offset_ms: 10,
        input: Input::KeyDown { key: "A".into() },
    });
    sim.fail_submission_on(2, SubmissionFault::UnknownApplied);
    sim.submit(intent);
    sim.dispatch_next();
    sim.advance_to(10)?;
    scenarios.push(result(
        "lost_ack_preserves_unknown_and_cleans_inputs",
        &sim,
        ReceiptStatus::Unknown,
    )?);

    let (mut sim, permit) = seed()?;
    sim.submit(action(&sim, &permit));
    sim.dispatch_next();
    sim.set_release_failure(true);
    sim.disconnect();
    scenarios.push(result(
        "release_failure_isolates_channel",
        &sim,
        ReceiptStatus::Unknown,
    )?);

    let passed = scenarios.iter().all(|scenario| scenario["passed"] == true);
    Ok(
        json!({ "mode": "deterministic_simulation", "real_device_input": false,
        "business_effects_verified": false, "passed": passed, "scenarios": scenarios }),
    )
}

fn main() {
    match run() {
        Ok(mut report) => {
            // SDK feature unification may enable serde_json/preserve_order.
            // The frozen I0 report always uses lexicographic object keys.
            report.sort_all_objects();
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serializable report")
            );
            if report["passed"] != true {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("I0 demo failed: {error}");
            std::process::exit(1);
        }
    }
}
