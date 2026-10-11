//! Owned-fixture smoke only. The core's backend is a memory simulator; the
//! native helper performs read-only probes and cannot post any input.
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
    if args.len() != 3 {
        return Err("expected owned helper and fixture proof paths".into());
    }
    let proof: BindingProof =
        serde_json::from_slice(&std::fs::read(&args[2]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let clock = SharedClock::starting_now();
    let mut config = CoreConfig::production("macos");
    config.clock_domain_kind = "wall".into();
    let mut core = ExecutionCore::new(config, SimBackend::new());
    let mut guard = GuardClient::start(Path::new(&args[1]), &core.identity(), clock, 1234567)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        guard.renew("host", 750);
        guard.renew("supervisor", 750);
        if guard.poll()?.is_some_and(|s| {
            s.accessibility
                && s.human_input
                && s.host_alive
                && s.supervisor_alive
                && s.authority_clock_ready
        }) {
            break;
        }
        if Instant::now() >= deadline {
            return Err("native guard prerequisites unavailable".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
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
    ) -> Result<device_macos::guard::authority::NativeProbeResponse, String> {
        loop {
            guard.renew("host", 750);
            guard.renew("supervisor", 750);
            if let Some(reply) = guard.poll_probe()? {
                return Ok(reply);
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    guard.probe(
        &SubmitContext {
            intent: &intent,
            authority: authority.clone(),
        },
        &primitive,
    )?;
    let eligible = receive(&mut guard)?;
    if eligible.outcome != "eligible" {
        return Err(format!(
            "native probe rejected: {} {}",
            eligible.outcome, eligible.reason
        ));
    }
    // New guard states must remain visible to the shell after a probe poll.
    thread::sleep(Duration::from_millis(120));
    let notice_preserved = guard.poll()?.is_some();
    guard.revoke(&permit);
    intent.intent_id = "revoked-probe-intent".into();
    guard.probe(
        &SubmitContext {
            intent: &intent,
            authority,
        },
        &primitive,
    )?;
    let revoked = receive(&mut guard)?;
    if revoked.outcome != "revoked" {
        return Err(format!("revocation lost: {}", revoked.outcome));
    }
    let result = serde_json::json!({"core_context_reached_native_worker":true,"revoked_before_query":true,
        "probe_did_not_consume_guard_notice":notice_preserved,"physical_input_sent":false,"native_input_authorized":false});
    println!("{result}");
    Ok(())
}
