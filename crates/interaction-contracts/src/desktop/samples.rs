//! One realistic sample of every message. They feed the published fixtures,
//! the frame tests and the TypeScript conformance tests.

use std::collections::BTreeMap;

use super::PROTOCOL_VERSION;
use super::decision::*;
use super::device::*;
use super::effect::EffectClass;
use super::handshake::*;
use super::ids::*;
use super::message::{Envelope, Message};
use super::operator::*;
use super::primitive::Primitive;
use super::primitive::{AxActionKind, PrimitivePolicy};
use super::source::*;
use super::task::{ConfirmationScope, ExpectedValue, RootState};

pub fn envelope_for(message: Message) -> Envelope {
    Envelope {
        protocol_version: PROTOCOL_VERSION,
        message_id: MessageId::new(format!("msg-{}", message.name())).unwrap(),
        in_reply_to: None,
        producer: ProducerId::new("cu-sample").unwrap(),
        producer_sequence: 1,
        emitted_ms: 100,
        trace_id: "trace-1".into(),
        message,
    }
}

fn surface() -> SurfaceRef {
    SurfaceRef {
        id: "main".into(),
        epoch: 1,
    }
}

fn permit() -> PermitRef {
    PermitRef {
        id: "permit-1".into(),
        generation: 1,
    }
}

fn dependencies() -> BTreeMap<String, u64> {
    BTreeMap::from([
        ("authorization".into(), 1),
        ("binding".into(), 1),
        ("focus".into(), 1),
        ("layer".into(), 1),
    ])
}

fn root_id() -> RootTaskId {
    RootTaskId::new("root-1").unwrap()
}

pub fn manifest() -> CapabilityManifest {
    CapabilityManifest {
        identity: DeviceIdentity {
            session_epoch: 1,
            surface: surface(),
            clock_domain: "sim/1/monotonic".into(),
            dependencies: dependencies(),
        },
        backend: BackendKind::Simulated,
        policy: PrimitivePolicy::AtomicOnly,
        human_input: CapabilityState::Available,
        hit_test: CapabilityState::Available,
        input: CapabilityState::Available,
        max_uninterruptible_ms: 50,
    }
}

pub fn stop_command() -> Message {
    Message::StopCommand(StopCommand {
        scope: StopScope::All,
        disposition: StopDisposition::Pause,
        source: StopSource::OperatorButton,
        reason: "user pressed stop".into(),
    })
}

pub fn resume_command() -> Message {
    Message::ResumeCommand(ResumeCommand {
        scope: StopScope::Root {
            root_id: "root-1".into(),
        },
    })
}

fn stop_ack() -> StopAck {
    StopAck {
        stop_seq: 1,
        stop_floor: 1,
        scope: StopScope::All,
        disposition: StopDisposition::Pause,
        source: StopSource::OperatorButton,
        revoked_permits: 1,
        cleanup: ReleaseStatus::NotNeeded,
        at_ms: 120,
    }
}

fn intent() -> ActionIntent {
    ActionIntent {
        intent_id: "intent-1".into(),
        attempt_id: Some("attempt-1".into()),
        task_id: "root-1".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: 1,
        permit: permit(),
        surface: surface(),
        target_ref: "daily-picks".into(),
        binding_fingerprint: Some("fp1:0123456789abcdef".into()),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: dependencies(),
        evidence_id: "obs-1".into(),
        latest_start_ms: 200,
        duration_ms: 100,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: Primitive::AxAction {
                action: AxActionKind::SetValue {
                    value: "off".into(),
                },
            },
        }],
    }
}

fn snapshot() -> StructureSnapshot {
    StructureSnapshot {
        snapshot_id: RequestId::new("snap-1").unwrap(),
        session_epoch: 1,
        surface: surface(),
        clock_domain: "sim/1/monotonic".into(),
        source_sequence: 1,
        observed_ms: 90,
        app: AppIdentity {
            bundle_id: "test.settings".into(),
            name: "Settings".into(),
            pid: 4242,
        },
        windows: vec![WindowInfo {
            window_id: "w1".into(),
            title: Some("Notifications".into()),
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 600.0,
                height: 400.0,
            },
            z_index: 0,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: "test.settings".into(),
        }],
        active_layer: Some("w1".into()),
        complete: true,
        incomplete_reason: None,
        objects: vec![UiObject {
            instance_ref: ObjectRef::new("obj-1").unwrap(),
            parent: None,
            window_id: "w1".into(),
            role: "switch".into(),
            subrole: None,
            title: Some("Daily picks".into()),
            description: None,
            value: Some(UiValue::Bool(true)),
            enabled: true,
            focused: false,
            frame: Rect {
                x: 40.0,
                y: 60.0,
                width: 40.0,
                height: 20.0,
            },
            actions: vec![AxActionName::Press, AxActionName::SetValue],
            in_view: true,
            group: Some("Push".into()),
            secure: false,
        }],
    }
}

fn decision_request() -> DecisionRequest {
    DecisionRequest {
        decision_id: DecisionId::new("d-1").unwrap(),
        lane_id: "lane-1".into(),
        request_generation: 1,
        root_task_id: root_id(),
        task_revision: 1,
        plan_revision: 1,
        root_epoch: 1,
        decision_program: "ui.ensure_value@1".into(),
        calibration_revision: "none".into(),
        state: DecisionState {
            root_task_id: root_id(),
            task_revision: 1,
            goal_summary: "turn off daily picks".into(),
            subgoal: "ensure the switch is off".into(),
            snapshot_revision: 2,
            objects: vec![StateObject {
                instance: ObjectRef::new("obj-1").unwrap(),
                role: "switch".into(),
                label: "Daily picks".into(),
                value: Some("on".into()),
                group: Some("Push".into()),
                enabled: true,
                in_view: true,
                age_ms: 10,
            }],
            candidates: vec![Candidate {
                candidate_id: CandidateId::new("c-1").unwrap(),
                skill: "ui.ensure_value".into(),
                semantic_target: Some(SemanticTargetRef::new("daily-picks").unwrap()),
                binding: Some(BindingRef::new("bind-1").unwrap()),
                parameters: BTreeMap::from([("value".into(), "off".into())]),
                effect_class: EffectClass::ReversibleEdit,
                precondition: Precondition::Executable,
                description: "set the switch off".into(),
            }],
            recent_results: vec![],
            missing_fields: vec![],
            omitted_items: vec![],
            candidate_set_digest: "sha:abc".into(),
        },
        questions: vec![Question {
            id: "q1".into(),
            kind: QuestionKind::Choice,
            prompt: "Which candidate advances the goal?".into(),
            options: vec![QuestionOption {
                key: "c-1".into(),
                description: "set the switch off".into(),
            }],
        }],
        created_ms: 95,
        result_deadline_ms: 600,
    }
}

/// One sample per message type, keyed by its wire name.
pub fn all() -> Vec<(String, Envelope)> {
    let messages = vec![
        Message::Hello(Hello {
            protocol_min: 1,
            protocol_max: 1,
            role: PeerRole::Device,
            build_revision: "m1-sample".into(),
            process: ProcessIdentity {
                pid: 4242,
                start_time_ms: 1000,
            },
            session_epoch: 1,
            worker_epoch: 1,
            token: "0123456789abcdef0123456789abcdef".into(),
        }),
        Message::HelloAck(HelloAck {
            negotiated_version: 1,
            role: PeerRole::Device,
            session_epoch: 1,
        }),
        Message::Ready(Ready {
            role: PeerRole::Device,
            capabilities: Some(manifest()),
            notes: vec!["simulated backend".into()],
        }),
        Message::Configure(Configure {
            role: PeerRole::Device,
            session_epoch: 1,
            settings: BTreeMap::from([("backend".into(), "sim".into())]),
        }),
        Message::Shutdown(Shutdown {
            reason: "product closing".into(),
        }),
        Message::LeaseRenew(LeaseRenew {
            lease_epoch: 1,
            valid_for_ms: 2000,
        }),
        Message::OperatorCommand(OperatorCommand::CreateTask {
            text: "turn off the daily picks notification".into(),
        }),
        Message::UiProjection(UiProjection {
            revision: 3,
            mode: RunMode::Execute,
            root: Some(RootView {
                root_task_id: root_id(),
                task_revision: 1,
                root_epoch: 1,
                state: RootState::Running,
                headline: "Turning off daily picks".into(),
                attempts_used: 1,
                attempts_max: 8,
            }),
            stop_status: StopStatus::NotStopped,
            cleanup: ReleaseStatus::NotNeeded,
            pending_confirmation: None,
            capabilities: Some(manifest()),
            timeline: vec![TimelineEntry {
                at_ms: 90,
                text: "goal confirmed".into(),
            }],
        }),
        Message::ConfirmationRequest(ConfirmationRequest {
            confirmation_id: ConfirmationId::new("conf-1").unwrap(),
            root_task_id: root_id(),
            task_revision: 1,
            kind: ConfirmationKind::Goal {
                goal: GoalSummary {
                    user_text: "turn off the daily picks notification".into(),
                    app: "test.settings".into(),
                    target_label: "Daily picks".into(),
                    group: Some("Push".into()),
                    desired: ExpectedValue::Bool { value: false },
                    confirmation_scope: ConfirmationScope::UiRevisit,
                    max_effect_class: EffectClass::ReversibleEdit,
                },
            },
        }),
        Message::ConfirmationResult(ConfirmationResult {
            confirmation_id: ConfirmationId::new("conf-1").unwrap(),
            approved: true,
        }),
        Message::RegisterRoot(RootGrant {
            root_id: "root-1".into(),
            revision: 1,
            root_epoch: 1,
            deadline_ms: 60_000,
        }),
        Message::GrantPermit(GrantPermit {
            acked_stop_seq: 0,
            task_id: "root-1".into(),
            target_ref: "daily-picks".into(),
            evidence_id: "obs-1".into(),
            valid_until_ms: 5_000,
        }),
        Message::GrantOutcome(GrantOutcome::Granted { permit: permit() }),
        Message::Observation(Observation {
            observation_id: "obs-1".into(),
            session_epoch: 1,
            surface: surface(),
            target_ref: "daily-picks".into(),
            clock_domain: "sim/1/monotonic".into(),
            source_sequence: 1,
            observed_ms: 90,
            valid_until_ms: 5_000,
            freshness: SourceFreshness::Verified,
            dependencies: dependencies(),
        }),
        Message::SubmitIntent(SubmitIntent {
            acked_stop_seq: 0,
            intent: intent(),
        }),
        Message::Receipt(Receipt {
            intent_id: "intent-1".into(),
            attempt_id: Some("attempt-1".into()),
            session_epoch: 1,
            status: ReceiptStatus::Completed,
            reason_code: None,
            reason: Some("bounded input ended; effect not verified".into()),
            submitted: vec![SubmittedPrimitive {
                at_ms: 210,
                primitive: Primitive::AxAction {
                    action: AxActionKind::SetValue {
                        value: "off".into(),
                    },
                },
                cleanup: false,
            }],
            release_status: ReleaseStatus::NotNeeded,
            command_end_ms: Some(300),
            at_ms: 300,
        }),
        Message::QueryReceipt(QueryReceipt {
            session_epoch: 1,
            intent_id: "intent-1".into(),
        }),
        Message::RenewPermit(RenewPermit {
            session_epoch: 1,
            permit: permit(),
            evidence_id: "obs-2".into(),
            valid_until_ms: 6_000,
        }),
        Message::RevokeExecution(RevokeExecution {
            session_epoch: 1,
            permit: permit(),
            reason: "layer changed".into(),
        }),
        stop_command(),
        Message::StopAck(stop_ack()),
        resume_command(),
        Message::ResumeAck(ResumeAck {
            resumed: true,
            reason: None,
        }),
        Message::DependencyUpdate(DependencyUpdate {
            name: "layer".into(),
            valid: Some(false),
        }),
        Message::DependencyVersions(DependencyVersions {
            versions: dependencies(),
        }),
        Message::DeviceEvent(DeviceEvent {
            seq: 1,
            at_ms: 120,
            event: DeviceEventKind::Stopped { ack: stop_ack() },
        }),
        Message::CapabilityManifest(manifest()),
        Message::CommandAck(CommandAck::ok()),
        Message::SimControl(SimControl {
            op: SimOp::SetHitTest {
                result: SimHitTest::Match,
            },
        }),
        Message::StructureSnapshot(snapshot()),
        Message::SourceHealth(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Available,
            reason: None,
            at_ms: 90,
        }),
        Message::FrameMeta(FrameMeta {
            frame_id: "frame-1".into(),
            session_epoch: 1,
            surface: surface(),
            clock_domain: "sim/1/monotonic".into(),
            source_sequence: 1,
            source_ms: 90,
            status: FrameStatus::Complete,
            geometry_revision: 1,
            width_px: 1440,
            height_px: 900,
            scale_factor: 2.0,
            byte_length: 5_184_000,
            checksum: "sha256:00".into(),
        }),
        Message::SourceCommand(SourceCommand::RefreshNow),
        Message::DecisionRequest(decision_request()),
        Message::DecisionProposal(DecisionProposal {
            decision_id: DecisionId::new("d-1").unwrap(),
            request_generation: 1,
            candidate_set_digest: "sha:abc".into(),
            backend: "teacher-scripted".into(),
            source: DecisionSource::Teacher,
            model_revision: "scripted-1".into(),
            answers: vec![Answer {
                question_id: "q1".into(),
                selected: Some("c-1".into()),
                confidence_milli: Some(900),
            }],
            choice: Some(PolicyChoice::StartSkill {
                candidate: CandidateId::new("c-1").unwrap(),
            }),
            failure: None,
            truncated: false,
            queue_ms: 1,
            inference_ms: 20,
        }),
        Message::DecisionCancel(DecisionCancel {
            decision_id: DecisionId::new("d-1").unwrap(),
            request_generation: 1,
        }),
    ];
    messages
        .into_iter()
        .map(|message| (message.name().to_string(), envelope_for(message)))
        .collect()
}

/// One published fixture: a message plus what the schema and the decoder must say about it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub name: String,
    pub schema_valid: bool,
    pub decode_valid: bool,
    pub message: serde_json::Value,
}

fn sample(name: &str) -> serde_json::Value {
    let (_, envelope) = all()
        .into_iter()
        .find(|(sample_name, _)| sample_name == name)
        .unwrap_or_else(|| panic!("no sample named {name}"));
    serde_json::to_value(envelope).expect("samples serialise")
}

fn mutated(
    name: &str,
    base: &str,
    schema_valid: bool,
    decode_valid: bool,
    mutate: impl FnOnce(&mut serde_json::Value),
) -> Fixture {
    let mut message = sample(base);
    mutate(&mut message);
    Fixture {
        name: name.into(),
        schema_valid,
        decode_valid,
        message,
    }
}

fn set(root: &mut serde_json::Value, pointer: &str, value: serde_json::Value) {
    *root
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("no value at {pointer}")) = value;
}

fn insert_unexpected(root: &mut serde_json::Value, pointer: &str) {
    root.pointer_mut(pointer)
        .and_then(serde_json::Value::as_object_mut)
        .unwrap_or_else(|| panic!("no object at {pointer}"))
        .insert("unexpected".into(), serde_json::json!(true));
}

/// Every valid sample, then deliberate corruptions of them. The corruptions
/// that only the decoder catches (`schema_valid` true, `decode_valid` false)
/// are the cross-field rules a schema cannot express.
pub fn fixtures() -> Vec<Fixture> {
    use serde_json::json;
    let mut out: Vec<Fixture> = all()
        .into_iter()
        .map(|(name, envelope)| Fixture {
            name: format!("valid_{name}"),
            schema_valid: true,
            decode_valid: true,
            message: serde_json::to_value(envelope).expect("samples serialise"),
        })
        .collect();

    // Structural: the schema rejects these and so does the decoder.
    out.push(mutated(
        "unknown_field_at_envelope",
        "stop_command",
        false,
        false,
        |m| insert_unexpected(m, ""),
    ));
    out.push(mutated(
        "unknown_field_in_payload",
        "submit_intent",
        false,
        false,
        |m| insert_unexpected(m, "/message/payload/intent"),
    ));
    out.push(mutated(
        "unknown_field_in_nested_primitive",
        "submit_intent",
        false,
        false,
        |m| {
            insert_unexpected(
                m,
                "/message/payload/intent/steps/0/primitive/payload/action",
            )
        },
    ));
    out.push(mutated(
        "unknown_message_type",
        "stop_command",
        false,
        false,
        |m| set(m, "/message/type", json!("format_disk")),
    ));
    out.push(mutated(
        "unsupported_protocol_version",
        "stop_command",
        false,
        false,
        |m| set(m, "/protocol_version", json!(2)),
    ));
    out.push(mutated(
        "empty_message_id",
        "stop_command",
        false,
        false,
        |m| set(m, "/message_id", json!("")),
    ));
    out.push(mutated(
        "oversized_message_id",
        "stop_command",
        false,
        false,
        |m| set(m, "/message_id", json!("x".repeat(300))),
    ));
    out.push(mutated(
        "unknown_primitive_type",
        "submit_intent",
        false,
        false,
        |m| {
            set(
                m,
                "/message/payload/intent/steps/0/primitive/type",
                json!("teleport"),
            )
        },
    ));
    out.push(mutated(
        "zero_duration",
        "submit_intent",
        false,
        false,
        |m| set(m, "/message/payload/intent/duration_ms", json!(0)),
    ));
    out.push(mutated("no_steps", "submit_intent", false, false, |m| {
        set(m, "/message/payload/intent/steps", json!([]))
    }));
    out.push(mutated(
        "root_epoch_zero",
        "submit_intent",
        false,
        false,
        |m| set(m, "/message/payload/intent/root_epoch", json!(0)),
    ));
    out.push(mutated(
        "short_handshake_token",
        "hello",
        false,
        false,
        |m| set(m, "/message/payload/token", json!("abc")),
    ));
    out.push(mutated(
        "unknown_operator_command",
        "operator_command",
        false,
        false,
        |m| set(m, "/message/payload/command", json!("run_shell")),
    ));
    out.push(mutated(
        "negative_sequence",
        "stop_command",
        false,
        false,
        |m| set(m, "/producer_sequence", json!(-1)),
    ));

    // Semantic: valid shapes whose fields contradict each other.
    out.push(mutated(
        "observation_window_inverted",
        "observation",
        true,
        false,
        |m| set(m, "/message/payload/valid_until_ms", json!(10)),
    ));
    out.push(mutated(
        "offsets_not_strictly_increasing",
        "submit_intent",
        true,
        false,
        |m| {
            let step = m
                .pointer("/message/payload/intent/steps/0")
                .cloned()
                .unwrap();
            m.pointer_mut("/message/payload/intent/steps")
                .and_then(serde_json::Value::as_array_mut)
                .unwrap()
                .push(step);
        },
    ));
    out.push(mutated(
        "offset_at_or_beyond_duration",
        "submit_intent",
        true,
        false,
        |m| set(m, "/message/payload/intent/steps/0/offset_ms", json!(100)),
    ));
    out.push(mutated(
        "key_released_but_never_held",
        "submit_intent",
        true,
        false,
        |m| {
            set(
                m,
                "/message/payload/intent/steps/0/primitive",
                json!({"type": "key_up", "payload": {"key": "W"}}),
            )
        },
    ));
    out.push(mutated(
        "blank_typed_text",
        "submit_intent",
        true,
        false,
        |m| {
            set(
                m,
                "/message/payload/intent/steps/0/primitive",
                json!({"type": "type_text", "payload": {"text": ""}}),
            )
        },
    ));
    out.push(mutated(
        "receipt_submitted_after_receipt_time",
        "receipt",
        true,
        false,
        |m| set(m, "/message/payload/submitted/0/at_ms", json!(999)),
    ));
    out.push(mutated(
        "snapshot_active_layer_unknown",
        "structure_snapshot",
        true,
        false,
        |m| set(m, "/message/payload/active_layer", json!("ghost")),
    ));
    out.push(mutated(
        "snapshot_incomplete_without_reason",
        "structure_snapshot",
        true,
        false,
        |m| set(m, "/message/payload/complete", json!(false)),
    ));
    out.push(mutated(
        "snapshot_object_in_unknown_window",
        "structure_snapshot",
        true,
        false,
        |m| set(m, "/message/payload/objects/0/window_id", json!("ghost")),
    ));
    out.push(mutated(
        "decision_deadline_before_creation",
        "decision_request",
        true,
        false,
        |m| set(m, "/message/payload/result_deadline_ms", json!(1)),
    ));
    out.push(mutated(
        "hello_protocol_range_inverted",
        "hello",
        true,
        false,
        |m| set(m, "/message/payload/protocol_min", json!(2)),
    ));
    out.push(mutated(
        "stop_scope_root_blank",
        "stop_command",
        true,
        false,
        |m| {
            set(
                m,
                "/message/payload/scope",
                json!({"scope": "root", "root_id": " "}),
            )
        },
    ));
    out
}
