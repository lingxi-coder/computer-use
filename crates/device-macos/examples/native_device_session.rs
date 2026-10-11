//! Real native device session against owned apps.
#[path = "support/native_session.rs"]
mod support;
use interaction_contracts::desktop::{
    binding::BindingProof,
    device::*,
    domain::ExecutionDomain,
    message::Message,
    primitive::{AxActionKind, Modifiers, MouseButton, Primitive},
};
use std::{
    io::{BufRead, Write},
    sync::mpsc,
    time::{Duration, Instant},
};
use support::Session;
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err("expected device/helper/proof/scenario".into());
    }
    let proof: BindingProof =
        serde_json::from_slice(&std::fs::read(&args[3]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if args[4] == "click-recovery-observe" {
        let (mut session, identity) = Session::start_epoch(&args[1], &args[2], &proof, 2, true)?;
        session
            .host
            .send(Message::QueryReceipt(QueryReceipt {
                session_epoch: 1,
                intent_id: "production-native-one".into(),
            }))
            .map_err(|error| error.to_string())?;
        let response =
            session.wait(|message| matches!(message, Message::CommandAck(ack) if !ack.accepted))?;
        println!(
            "{}",
            serde_json::json!({"kind":"recovery_observer_result","new_epoch":identity.session_epoch,"query_response":response,"input_capability_unavailable_verified":true,"normal_input_submitted":false})
        );
        return Ok(());
    }
    let (mut session, identity) = Session::start(&args[1], &args[2], &proof)?;
    session.wait(|m|matches!(m,Message::DeviceEvent(DeviceEvent{event:DeviceEventKind::CapabilityChanged{manifest},..}) if manifest.input==CapabilityState::Available && manifest.hit_test==CapabilityState::Available))?;
    let now = session.clock.now_ms();
    session.send_ack(Message::RegisterRoot(RootGrant {
        root_id: "owned-root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: now + 5000,
    }))?;
    let evidence = Observation {
        observation_id: "owned-source".into(),
        session_epoch: 1,
        surface: identity.surface.clone(),
        target_ref: proof.binding.as_str().into(),
        clock_domain: identity.clock_domain.clone(),
        source_sequence: 1,
        observed_ms: now,
        valid_until_ms: now + 1500,
        freshness: SourceFreshness::Verified,
        execution_domain: ExecutionDomain::ForegroundNative,
        purpose: Default::default(),
        owned_document_proof: None,
        binding_proof: Some(proof.clone()),
        dependencies: identity.dependencies.clone(),
    };
    session.send_ack(Message::Observation(ExecutionObservation::Native {
        observation: Box::new(evidence.clone()),
    }))?;
    session
        .host
        .send(Message::GrantPermit(GrantPermit {
            acked_stop_seq: 0,
            task_id: "owned-root".into(),
            target_ref: evidence.target_ref.clone(),
            evidence_id: evidence.observation_id.clone(),
            valid_until_ms: now + 1500,
        }))
        .map_err(|e| e.to_string())?;
    let Message::GrantOutcome(GrantOutcome::Granted { permit }) =
        session.wait(|m| matches!(m, Message::GrantOutcome(_)))?
    else {
        return Err("native permit rejected".into());
    };
    let primitive = if args[4].starts_with("click-") {
        let (x, y) = proof.display_frame.center();
        Primitive::Click {
            button: match args[4].as_str() {
                "click-right" => MouseButton::Right,
                "click-middle" => MouseButton::Middle,
                _ => MouseButton::Left,
            },
            x: x as i32,
            y: y as i32,
            modifiers: if args[4] == "click-control" {
                Modifiers {
                    control: true,
                    ..Modifiers::default()
                }
            } else if args[4] == "click-modified" {
                Modifiers {
                    shift: true,
                    option: true,
                    command: true,
                    ..Modifiers::default()
                }
            } else {
                Modifiers::default()
            },
        }
    } else if args[4].starts_with("position-") {
        let (x, y) = proof.display_frame.center();
        Primitive::PointerPosition {
            x: x.round() as i32,
            y: y.round() as i32,
        }
    } else if args[4].starts_with("scroll-") {
        let (dx, dy) = match args[4].as_str() {
            "scroll-up" => (0, 120),
            "scroll-right" => (-120, 0),
            "scroll-left" => (120, 0),
            "scroll-invalid" => (0, 121),
            _ => (0, -120),
        };
        Primitive::Scroll { dx, dy }
    } else {
        Primitive::AxAction {
            action: if args[4].starts_with("menu-") {
                AxActionKind::ShowMenu
            } else if args[4].starts_with("set-text") {
                AxActionKind::SetValue {
                    value: if args[4] == "set-text-oversize" {
                        "x".repeat(2049)
                    } else if args[4] == "set-text-boundary" {
                        let prefix = "👩‍👩‍👧‍👦\r\n中文";
                        format!("{prefix}{}", "a".repeat(2048 - prefix.len()))
                    } else {
                        "MVP 中文\nHello 👋".into()
                    },
                }
            } else {
                AxActionKind::Press
            },
        }
    };
    let intent = ActionIntent {
        intent_id: "production-native-one".into(),
        attempt_id: Some("owned-attempt".into()),
        task_id: "owned-root".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: 1,
        execution_domain: evidence.execution_domain,
        permit,
        surface: identity.surface,
        target_ref: evidence.target_ref,
        binding_fingerprint: Some(proof.fingerprint.digest()),
        owned_document_proof: None,
        binding_proof: Some(proof),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: identity.dependencies,
        evidence_id: evidence.observation_id,
        latest_start_ms: now + 200,
        duration_ms: 250,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive,
        }],
    };
    session
        .host
        .send(Message::SubmitIntent(SubmitIntent {
            acked_stop_seq: 0,
            intent: ExecutionIntent::Native {
                intent: Box::new(intent.clone()),
            },
        }))
        .map_err(|e| e.to_string())?;
    println!("{}", serde_json::json!({"kind":"device_request_enqueued"}));
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let (control_tx, control_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let _ = control_tx.send(line);
        }
    });
    let mut stop_at = None;
    let mut stop_latency = None;
    let mut pending_queried = false;
    let mut final_receipt = None;
    let mut observed_receipt_statuses = Vec::new();
    let mut channel_isolated = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while final_receipt.is_none()
        || args[4] == "stop-inflight"
            && (stop_latency.is_none()
                || final_receipt
                    .as_ref()
                    .is_none_or(|r: &Receipt| r.submitted.is_empty()))
    {
        if control_rx.try_recv().is_ok() {
            stop_at = Some(Instant::now());
            session
                .supervisor
                .send(Message::StopCommand(StopCommand {
                    scope: StopScope::All,
                    disposition: StopDisposition::Cancel,
                    source: StopSource::OperatorButton,
                    reason: "owned callback fault injection".into(),
                }))
                .map_err(|e| e.to_string())?;
        }
        let messages = match session.pump() {
            Ok(messages) => messages,
            Err(error) if args[4].starts_with("click-death-") => {
                if !session.wait_killed()? {
                    return Err(format!("expected owned cu-device SIGKILL: {error}"));
                }
                println!(
                    "{}",
                    serde_json::json!({"kind":"death_observer_result","device_sigkill_verified":true,"old_connection_closed":true,"input_outcome":"unknown","terminal_device_receipt_available":final_receipt.is_some(),"observed_receipt_statuses":observed_receipt_statuses,"error":error,"intent_id":intent.intent_id,"old_epoch":intent.session_epoch})
                );
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        for m in messages {
            match m {
                Message::Receipt(r) if r.intent_id == intent.intent_id => {
                    observed_receipt_statuses.push(r.status);
                    if r.status == ReceiptStatus::Running {
                        pending_queried = true;
                    }
                    if matches!(
                        r.status,
                        ReceiptStatus::Completed
                            | ReceiptStatus::Unknown
                            | ReceiptStatus::Rejected
                            | ReceiptStatus::Partial
                            | ReceiptStatus::Cancelled
                    ) {
                        final_receipt = Some(r);
                    }
                }
                Message::StopAck(_) => {
                    if let Some(at) = stop_at {
                        stop_latency = Some(at.elapsed().as_millis());
                    }
                }
                Message::DeviceEvent(DeviceEvent {
                    event: DeviceEventKind::ChannelIsolated { .. },
                    ..
                }) => channel_isolated = true,
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            return Err("production native result missing".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let receipt = final_receipt.unwrap();
    let expected = if [
        "applied",
        "set-text",
        "set-text-boundary",
        "click-applied",
        "click-right",
        "click-middle",
        "click-modified",
        "click-control",
        "position-applied",
        "scroll-down",
        "scroll-up",
        "scroll-right",
        "scroll-left",
        "menu-applied",
        "menu-no-effect",
    ]
    .contains(&args[4].as_str())
    {
        ReceiptStatus::Completed
    } else if [
        "occluded",
        "set-text-unfocused",
        "set-text-oversize",
        "set-text-secure",
        "click-occluded",
        "position-occluded",
        "scroll-occluded",
        "scroll-stale",
        "scroll-invalid",
        "menu-occluded",
    ]
    .contains(&args[4].as_str())
    {
        ReceiptStatus::Rejected
    } else {
        ReceiptStatus::Unknown
    };
    if receipt.status != expected {
        return Err(format!(
            "native core result was {:?}: {:?}",
            receipt.status, receipt.reason
        ));
    }
    if expected == ReceiptStatus::Completed
        && (receipt.submitted.len() != 1
            || receipt.submitted[0].native_primitive() != Some(&intent.steps[0].primitive)
            || receipt.submitted[0].cleanup())
    {
        return Err("missing real submission range".into());
    }
    if ["occluded", "click-occluded"].contains(&args[4].as_str())
        && (receipt.reason_code
            != Some(interaction_contracts::desktop::error::FailureCode::TargetOccluded)
            || !receipt.submitted.is_empty())
    {
        return Err("native occlusion reason or submission range lost".into());
    }
    session
        .host
        .send(Message::SubmitIntent(SubmitIntent {
            acked_stop_seq: 0,
            intent: ExecutionIntent::Native {
                intent: Box::new(intent.clone()),
            },
        }))
        .map_err(|e| e.to_string())?;
    let Message::Receipt(duplicate) =
        session.wait(|m| matches!(m,Message::Receipt(r) if r.intent_id==intent.intent_id))?
    else {
        unreachable!()
    };
    if duplicate.status != receipt.status {
        return Err("duplicate receipt changed result".into());
    }
    if expected == ReceiptStatus::Unknown {
        if !channel_isolated {
            session.wait(|message| {
                matches!(
                    message,
                    Message::DeviceEvent(DeviceEvent {
                        event: DeviceEventKind::ChannelIsolated { .. },
                        ..
                    })
                )
            })?;
            channel_isolated = true;
        }
        let mut later = intent.clone();
        later.intent_id = "after-unknown-must-not-dispatch".into();
        later.latest_start_ms = session.clock.now_ms() + 100;
        session
            .host
            .send(Message::SubmitIntent(SubmitIntent {
                acked_stop_seq: 0,
                intent: ExecutionIntent::Native {
                    intent: Box::new(later.clone()),
                },
            }))
            .map_err(|error| error.to_string())?;
        let Message::Receipt(rejected) = session.wait(|message| matches!(message, Message::Receipt(receipt) if receipt.intent_id == later.intent_id))? else { unreachable!() };
        if rejected.status != ReceiptStatus::Rejected || !rejected.submitted.is_empty() {
            return Err("isolated native channel admitted new input".into());
        }
    }
    let no_sim = std::fs::metadata(session.dir.join("world.log")).map_or(true, |m| m.len() == 0);
    if !no_sim {
        return Err("native device routed input to simulated world".into());
    }
    if stop_latency.is_some_and(|v| v > 150) {
        return Err("stop waited for native completion".into());
    }
    println!(
        "{}",
        serde_json::json!({"kind":"production_result","production_core_native_backend_connected":true,"simulated_input_used":false,"receipt_status":receipt.status,"reason_code":receipt.reason_code,"submitted_count":receipt.submitted.len(),"input_end_ms":receipt.input_end_ms,"command_end_ms":receipt.command_end_ms,"pending_running_receipt_seen":pending_queried,"duplicate_not_replayed":true,"new_intent_rejected_after_unknown":expected==ReceiptStatus::Unknown,"channel_isolated":channel_isolated,"stop_ack_ms":stop_latency,"real_native_ax_input_verified":expected==ReceiptStatus::Completed && !args[4].starts_with("click-") && !args[4].starts_with("scroll-") && !args[4].starts_with("position-"),"real_native_pointer_position_input_verified":expected==ReceiptStatus::Completed && args[4].starts_with("position-"),"real_native_scroll_input_verified":expected==ReceiptStatus::Completed && args[4].starts_with("scroll-"),"real_native_pointer_input_verified":expected==ReceiptStatus::Completed && args[4].starts_with("click-"),"business_effect_verified":false})
    );
    Ok(())
}
