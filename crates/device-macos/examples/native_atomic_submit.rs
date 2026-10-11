//! Owned-fixture AX submission smoke. The memory core supplies the exact
//! authority; the explicitly scoped native worker performs the real AX call.
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use desktop_ipc::SharedClock;
use device_macos::guard::GuardClient;
use execution_core::{
    backend::SubmitContext,
    core::{CoreConfig, ExecutionCore},
    sim_backend::SimBackend,
};
use interaction_contracts::desktop::{
    binding::BindingProof,
    device::*,
    domain::ExecutionDomain,
    primitive::{AxActionKind, Primitive},
};

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("expected owned helper, fixture proof and expected outcome".into());
    }
    let proof: BindingProof =
        serde_json::from_slice(&std::fs::read(&args[2]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let clock = SharedClock::starting_now();
    let mut config = CoreConfig::production("macos");
    config.clock_domain_kind = "wall".into();
    let mut core = ExecutionCore::new(config, SimBackend::new());
    let mut guard = GuardClient::start_execution(
        Path::new(&args[1]),
        &core.identity(),
        clock,
        1234567,
        proof.fingerprint.app_pid,
        &proof.fingerprint.app,
        None,
        None,
    )?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let initial_revision = loop {
        guard.renew("host", 750);
        guard.renew("supervisor", 750);
        if let Some(s) = guard.poll()?
            && s.accessibility
            && s.human_input
            && s.host_alive
            && s.supervisor_alive
            && s.authority_clock_ready
        {
            break s.revision();
        }
        if Instant::now() >= deadline {
            return Err("native guard prerequisites unavailable".into());
        }
        thread::sleep(Duration::from_millis(20));
    };
    let now = clock.now_ms();
    core.advance_to(now)?;
    core.renew_lease(1500);
    core.register_root(&RootGrant {
        root_id: "probe-root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: now + 5000,
    })?;
    let evidence = Observation {
        observation_id: "owned-source".into(),
        session_epoch: 1,
        surface: core.surface(),
        target_ref: proof.binding.as_str().into(),
        clock_domain: core.clock_domain(),
        source_sequence: 1,
        observed_ms: now,
        valid_until_ms: now + 1500,
        freshness: SourceFreshness::Verified,
        execution_domain: ExecutionDomain::ForegroundNative,
        purpose: Default::default(),
        owned_document_proof: None,
        binding_proof: Some(proof.clone()),
        dependencies: core.dependency_versions(),
    };
    core.observe(evidence.clone())?;
    let permit = core
        .grant_permit(&GrantPermit {
            acked_stop_seq: 0,
            task_id: "probe-root".into(),
            target_ref: evidence.target_ref.clone(),
            evidence_id: evidence.observation_id.clone(),
            valid_until_ms: now + 1500,
        })
        .map_err(|e| e.reason)?;
    let primitive = Primitive::AxAction {
        action: AxActionKind::Press,
    };
    let mut intent = ActionIntent {
        intent_id: "core-probe-intent".into(),
        attempt_id: Some("owned-probe".into()),
        task_id: "probe-root".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: 1,
        execution_domain: evidence.execution_domain,
        permit: permit.clone(),
        surface: core.surface(),
        target_ref: evidence.target_ref,
        binding_fingerprint: Some(proof.fingerprint.digest()),
        owned_document_proof: None,
        binding_proof: Some(proof),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: core.dependency_versions(),
        evidence_id: evidence.observation_id,
        latest_start_ms: now + 100,
        duration_ms: 1000,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: primitive.clone(),
        }],
    };
    let accepted = core.submit(SubmitIntent {
        acked_stop_seq: 0,
        intent: ExecutionIntent::Native {
            intent: Box::new(intent.clone()),
        },
    });
    if accepted.status != ReceiptStatus::Accepted {
        return Err(format!("core rejected fixture: {:?}", accepted.reason));
    }
    core.dispatch_next().ok_or("missing simulated dispatch")?;
    let authority = core
        .backend()
        .events()
        .first()
        .and_then(|v| v.authority.clone())
        .ok_or("missing core authority")?;
    fn receive(
        guard: &mut GuardClient,
    ) -> Result<device_macos::guard::submission::NativeSubmitResponse, String> {
        loop {
            guard.renew("host", 750);
            guard.renew("supervisor", 750);
            if let Some(reply) = guard.poll_submission()? {
                return Ok(reply);
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    if args[3] == "revision-mismatch" {
        guard.stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            guard.renew("host", 750);
            guard.renew("supervisor", 750);
            if guard
                .poll()?
                .is_some_and(|s| s.revision() > initial_revision)
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err("stop revision did not advance".into());
            }
            thread::sleep(Duration::from_millis(2));
        }
        let rejected = guard
            .submit_press_at_revision(
                &SubmitContext {
                    intent: &intent,
                    authority,
                },
                &primitive,
                initial_revision,
            )
            .is_err_and(|reason| reason.contains("core reconciliation"));
        if !rejected {
            return Err("stale native revision refreshed old authority".into());
        }
        println!(
            "{}",
            serde_json::json!({"native_revision_mismatch_rejected":true,"native_ax_action_called":false})
        );
        return Ok(());
    }
    guard.submit_press(
        &SubmitContext {
            intent: &intent,
            authority: authority.clone(),
        },
        &primitive,
    )?;
    if args[3] == "transport-unknown" {
        // The independent driver kills this exact child after the owned AX
        // callback starts. This process never resends an uncertain action.
        println!(
            "{}",
            serde_json::json!({"kind":"native_guard_started","pid":guard.pid()})
        );
        match receive(&mut guard) {
            Err(reason) if reason.contains("unknown") => {
                println!(
                    "{}",
                    serde_json::json!({"kind":"transport_result","native_transport_unknown":true,"unknown_not_replayed":true})
                );
                return Ok(());
            }
            other => return Err(format!("expected lost native receipt, got {other:?}")),
        }
    }
    let reply = receive(&mut guard)?;
    if reply.outcome != args[3] || !reply.submitted {
        return Err(format!(
            "native submission: {} {}",
            reply.outcome, reply.reason
        ));
    }
    // Duplicate requests are rejected by native admission, including after an
    // unknown receipt. Do not infer permission to resend from current UI state.
    guard.submit_press(
        &SubmitContext {
            intent: &intent,
            authority: authority.clone(),
        },
        &primitive,
    )?;
    let duplicate = receive(&mut guard)?;
    if duplicate.outcome != "rejected" || duplicate.submitted {
        return Err("native duplicate submission was not rejected".into());
    }
    guard.revoke(&permit);
    intent.intent_id = "revoked-submit-intent".into();
    guard.submit_press(
        &SubmitContext {
            intent: &intent,
            authority,
        },
        &primitive,
    )?;
    let revoked = receive(&mut guard)?;
    if revoked.outcome != "revoked" || revoked.submitted {
        return Err(format!("native revocation lost: {}", revoked.outcome));
    }
    guard.stop();
    println!(
        "{}",
        serde_json::json!({"core_authority_reached_native_submit":true,
        "outcome":reply.outcome,"committed_ms":reply.committed_ms,"returned_ms":reply.returned_ms,
        "duplicate_rejected":true,"revoked_rejected":true,"native_ax_action_called":true,
        "business_effect_verified":false})
    );
    Ok(())
}
