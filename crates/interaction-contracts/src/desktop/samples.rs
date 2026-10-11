//! One realistic sample of every message. They feed the published fixtures,
//! the frame tests and the TypeScript conformance tests.

use std::collections::BTreeMap;

use super::PROTOCOL_VERSION;
use super::browser::{
    BrowserObservedBool, BrowserReadScope, BrowserRefreshRequest,
    browser_contract_golden_snapshots, derive_browser_checkbox_binding,
};
use super::browser_execution::{BrowserAction, BrowserActionIntent, BrowserExecutionObservation};
use super::decision::*;
use super::device::*;
use super::domain::{DomainAction, ExecutionDomain};
use super::effect::EffectClass;
use super::handshake::*;
use super::ids::*;
use super::message::{Envelope, Message};
use super::operator::*;
use super::primitive::Primitive;
use super::primitive::{AxActionKind, PrimitivePolicy};
use super::source::*;
use super::task::{
    AllowedControlScope, AllowedControlTarget, ConfirmationScope, ExpectedValue, RootState,
};

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
        execution_domain: ExecutionDomain::Simulated,
        policy: PrimitivePolicy::AtomicOnly,
        human_input: CapabilityState::Available,
        hit_test: CapabilityState::Available,
        input: CapabilityState::Available,
        domains: vec![
            ExecutionDomainCapability {
                domain: ExecutionDomain::ForegroundNative,
                state: CapabilityState::Unavailable,
                actions: vec![],
            },
            ExecutionDomainCapability {
                domain: ExecutionDomain::BackgroundAx,
                state: CapabilityState::Unavailable,
                actions: vec![],
            },
            ExecutionDomainCapability {
                domain: ExecutionDomain::HeadlessBrowser,
                state: CapabilityState::Unavailable,
                actions: vec![],
            },
            ExecutionDomainCapability {
                domain: ExecutionDomain::AppIntegration,
                state: CapabilityState::Unavailable,
                actions: vec![],
            },
            ExecutionDomainCapability {
                domain: ExecutionDomain::Simulated,
                state: CapabilityState::Available,
                actions: vec![DomainAction::SimulatedAction],
            },
        ],
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

fn proof() -> super::binding::BindingProof {
    super::binding::BindingProof {
        menu_scope: None,
        focus_scope: None,
        continuity: super::binding::BindingContinuityProof {
            kind: ObjectIdentityContinuity::NotificationCovered,
            source_sequence: 1,
            context_digest: "bc1:0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            scope: super::binding::BindingProofScope::WholeStructure,
        },
        device_context: super::binding::DeviceContextWitness::WholeStructureV1 {
            table_column: None,
            app: Box::new(super::binding::DeviceContextApp {
                bundle_id: "test.settings".into(),
                pid: 42,
            }),
            active_layer: Some("window-1".into()),
            window: Box::new(super::binding::DeviceContextWindow {
                window_id: "window-1".into(),
                title: Some("settings".into()),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                kind: LayerKind::Window,
                modal: false,
                owner_bundle_id: "test.settings".into(),
            }),
            target: Box::new(super::binding::DeviceContextTarget {
                object: super::binding::DeviceContextObject {
                    role: "AXSwitch".into(),
                    subrole: None,
                    label: "daily picks".into(),
                    frame: Rect {
                        x: 10.0,
                        y: 10.0,
                        width: 40.0,
                        height: 20.0,
                    },
                },
                group: Some("push".into()),
                scroll: None,
            }),
            ancestry: vec![
                super::binding::DeviceContextObject {
                    role: "AXWindow".into(),
                    subrole: Some("AXStandardWindow".into()),
                    label: "settings".into(),
                    frame: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 100.0,
                        height: 100.0,
                    },
                },
                super::binding::DeviceContextObject {
                    role: "AXGroup".into(),
                    subrole: None,
                    label: "push".into(),
                    frame: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 100.0,
                        height: 100.0,
                    },
                },
            ],
            sibling_parent_ancestry_index: Some(1),
            siblings: vec![super::binding::DeviceContextObject {
                role: "AXSwitch".into(),
                subrole: None,
                label: "daily picks".into(),
                frame: Rect {
                    x: 10.0,
                    y: 10.0,
                    width: 40.0,
                    height: 20.0,
                },
            }],
            target_sibling_index: 0,
            nearest_labelled_group_ancestry_index: Some(1),
            scroll_ancestors: vec![],
            menu: None,
            focus: None,
        },
        binding: BindingRef::new("daily-picks").unwrap(),
        instance: ObjectRef::new("toggle-1").unwrap(),
        snapshot_id: RequestId::new("snap-1").unwrap(),
        environment_revision: 1,
        layer_revision: 1,
        fingerprint: super::binding::InstanceFingerprint {
            app: "test.settings".into(),
            app_pid: 42,
            window_id: "window-1".into(),
            ancestor_roles: vec!["AXWindow".into(), "AXGroup".into()],
            group: Some("push".into()),
            role: "AXSwitch".into(),
            subrole: None,
            label: "daily picks".into(),
            sibling_index: 0,
            frame: [10, 10, 40, 20],
            scroll_container: None,
        },
        display_frame: Rect {
            x: 10.0,
            y: 10.0,
            width: 40.0,
            height: 20.0,
        },
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
        execution_domain: ExecutionDomain::Simulated,
        permit: permit(),
        surface: surface(),
        target_ref: "daily-picks".into(),
        binding_fingerprint: Some(proof().fingerprint.digest()),
        owned_document_proof: None,
        binding_proof: Some(proof()),
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

fn browser_execution_observation() -> BrowserExecutionObservation {
    let mut snapshots = browser_contract_golden_snapshots().expect("browser contract goldens");
    let mut snapshot = snapshots.remove(0);
    // This positive protocol fixture is explicitly synthetic. Real CDP goldens
    // keep a missing `disabled` property as unknown; only a separately observed
    // known-false value may qualify as enabled execution evidence.
    let checkbox = snapshot
        .nodes
        .iter_mut()
        .find(|node| node.ax_node_id == "checkbox-ax")
        .expect("golden fixture has its checkbox node");
    checkbox.disabled = BrowserObservedBool::False;
    snapshot
        .validate()
        .expect("synthetic enabled checkbox snapshot is valid");
    let binding = derive_browser_checkbox_binding(&snapshot, "Background option")
        .expect("synthetic fixture has one observed enabled checkbox");
    let context = snapshot.context.clone();
    BrowserExecutionObservation {
        observation_id: snapshot.snapshot_id,
        surface: SurfaceRef {
            id: context.page_target_id.clone(),
            epoch: context.session_epoch,
        },
        scope: Box::new(BrowserReadScope {
            scope_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            generation: 1,
            context,
            clock_domain: snapshot.clock_domain,
            issued_ms: snapshot.observed_ms,
        }),
        environment_revision: 2,
        binding_ref: BindingRef::new("browser-bind-synthetic-1").unwrap(),
        binding: Box::new(binding),
        valid_until_ms: snapshot.observed_ms + 1_000,
        freshness: SourceFreshness::Verified,
        dependencies: BTreeMap::from([
            ("authorization".into(), 1),
            ("binding".into(), 1),
            ("browser_scope".into(), 1),
            ("document".into(), 1),
        ]),
    }
}

fn browser_action_intent(evidence: BrowserExecutionObservation) -> BrowserActionIntent {
    BrowserActionIntent {
        intent_id: "browser-intent-synthetic-1".into(),
        attempt_id: "browser-attempt-synthetic-1".into(),
        task_id: "browser-task-synthetic-1".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: evidence.session_epoch(),
        permit: PermitRef {
            id: "browser-permit-synthetic-1".into(),
            generation: 1,
        },
        latest_start_ms: evidence.binding.read_finished_ms + 1,
        duration_ms: 100,
        evidence: Box::new(evidence),
        action: BrowserAction::SetCheckbox { checked: true },
    }
}

/// Additional explicitly synthetic examples cover the v2 browser execution
/// variants without making them available through the device manifest.
pub fn representative_messages() -> Vec<(String, Message)> {
    let evidence = browser_execution_observation();
    let action_intent = browser_action_intent(evidence.clone());
    vec![
        (
            "synthetic_browser_refresh_request".into(),
            Message::BrowserRefreshRequest(BrowserRefreshRequest {
                request_id: "550e8400-e29b-41d4-a716-446655440002".into(),
                scope_id: evidence.scope.scope_id.clone(),
                generation: evidence.scope.generation,
                context: evidence.scope.context.clone(),
                target_context_hash: evidence.binding.target_context_hash.clone(),
                clock_domain: evidence.scope.clock_domain.clone(),
                after_source_sequence: evidence.binding.source_sequence,
                after_ms: evidence.binding.read_finished_ms,
                deadline_ms: evidence.binding.read_finished_ms + 1_000,
            }),
        ),
        (
            "synthetic_browser_observation".into(),
            Message::Observation(ExecutionObservation::Browser {
                observation: Box::new(evidence),
            }),
        ),
        (
            "synthetic_browser_submit_intent".into(),
            Message::SubmitIntent(SubmitIntent {
                acked_stop_seq: 0,
                intent: ExecutionIntent::Browser {
                    intent: Box::new(action_intent),
                },
            }),
        ),
        (
            "synthetic_browser_receipt".into(),
            Message::Receipt(Receipt {
                intent_id: "browser-intent-synthetic-1".into(),
                attempt_id: Some("browser-attempt-synthetic-1".into()),
                session_epoch: 7,
                status: ReceiptStatus::Completed,
                reason_code: None,
                reason: Some("synthetic fixture only; no browser action was executed".into()),
                submitted: vec![SubmittedAction::Browser {
                    at_ms: 180,
                    action: BrowserAction::SetCheckbox { checked: true },
                }],
                release_status: ReleaseStatus::NotNeeded,
                command_end_ms: Some(185),
                input_end_ms: Some(180),
                at_ms: 190,
            }),
        ),
    ]
}

fn snapshot() -> StructureSnapshot {
    StructureSnapshot {
        menu_anchors: vec![],
        menu_context: None,
        focus_context: None,
        window_context: None,
        snapshot_id: RequestId::new("snap-1").unwrap(),
        session_epoch: 1,
        surface: surface(),
        clock_domain: "sim/1/monotonic".into(),
        identity_boundary: None,
        frame_ref: None,
        owned_document_evidence: None,
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
        global_focus: GlobalFocusObservation {
            app: ObservationFact::Known {
                value: AppIdentity {
                    bundle_id: "test.settings".into(),
                    name: "Settings".into(),
                    pid: 4242,
                },
            },
            window_id: ObservationFact::Known { value: "w1".into() },
        },
        target_context: None,
        complete: true,
        incomplete_reason: None,
        observer_coverage: ObserverCoverage {
            state: SourceState::Available,
            covered_objects: 1,
            read_scoped_objects: 0,
            failed_registrations: 0,
            failures: vec![],
        },
        table_scope_search: None,
        table_viewports: vec![],
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
            ax_action_names: None,
            in_view: true,
            group: Some("Push".into()),
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
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
            protocol_min: PROTOCOL_VERSION,
            protocol_max: PROTOCOL_VERSION,
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
            negotiated_version: PROTOCOL_VERSION,
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
            browser_page_profile: None,
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
        Message::OperatorCommand(OperatorCommand::CreateScopedTask {
            objective: "turn off the daily picks notification".into(),
            allowed_scope: AllowedControlScope {
                application: "Settings".into(),
                targets: vec![AllowedControlTarget {
                    control: "Daily picks".into(),
                    group: Some("Push".into()),
                    form: None,
                    disambiguator: Some("Push".into()),
                }],
                no_unrelated_controls: true,
            },
        }),
        Message::OperatorCommand(OperatorCommand::CreateFrozenTask {
            objective: "Turn off Weekly digest.".into(),
            allowed_scope: AllowedControlScope {
                application: "Interaction Fixture".into(),
                targets: vec![AllowedControlTarget {
                    control: "Weekly digest".into(),
                    group: Some("Notifications".into()),
                    form: None,
                    disambiguator: None,
                }],
                no_unrelated_controls: true,
            },
            profile: super::task::FrozenTaskProfile {
                catalog_id: "a".repeat(64),
                selector: "b".repeat(64),
                source_freeze_sha256: "c".repeat(64),
                source_request_sha256: "d".repeat(64),
                goals_sha256: "e".repeat(64),
                suite: "fixture".into(),
                task_id: "SET-02".into(),
                seed: 1,
            },
        }),
        Message::UiProjection(Box::new(UiProjection {
            browser_observation: None,
            revision: 3,
            observation: None,
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
        })),
        Message::ConfirmationRequest(ConfirmationRequest {
            confirmation_id: ConfirmationId::new("conf-1").unwrap(),
            root_task_id: root_id(),
            task_revision: 1,
            kind: ConfirmationKind::Goal {
                goal: Box::new(GoalSummary {
                    user_text: "turn off the daily picks notification".into(),
                    app: "test.settings".into(),
                    target_label: "Daily picks".into(),
                    group: Some("Push".into()),
                    row: None,
                    desired: ExpectedValue::Bool { value: false },
                    confirmation_scope: ConfirmationScope::UiRevisit,
                    max_effect_class: EffectClass::ReversibleEdit,
                    allowed_scope: None,
                    scoped_revisit: None,
                    scoped_commit: None,
                    browser_page_profile: None,
                }),
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
        Message::Observation(
            Observation {
                observation_id: "obs-1".into(),
                session_epoch: 1,
                surface: surface(),
                target_ref: "daily-picks".into(),
                clock_domain: "sim/1/monotonic".into(),
                source_sequence: 1,
                observed_ms: 90,
                valid_until_ms: 5_000,
                freshness: SourceFreshness::Verified,
                execution_domain: ExecutionDomain::Simulated,
                purpose: Default::default(),
                owned_document_proof: None,
                binding_proof: Some(proof()),
                dependencies: dependencies(),
            }
            .into(),
        ),
        Message::SubmitIntent(SubmitIntent {
            acked_stop_seq: 0,
            intent: intent().into(),
        }),
        Message::Receipt(Receipt {
            intent_id: "intent-1".into(),
            attempt_id: Some("attempt-1".into()),
            session_epoch: 1,
            status: ReceiptStatus::Completed,
            reason_code: None,
            reason: Some("bounded input ended; effect not verified".into()),
            submitted: vec![SubmittedAction::Native {
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
            input_end_ms: None,
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
        Message::StructureSnapshot(Box::new(snapshot())),
        Message::SourceHealth(SourceHealth {
            provenance: Some(SourceStamp {
                session_epoch: 1,
                stream_id: "native-stream-1".into(),
                source_sequence: 1,
            }),
            source: SourceKind::Ax,
            state: SourceState::Available,
            reason: None,
            at_ms: 90,
        }),
        Message::FrameMeta(FrameMeta {
            provenance: Some(crate::desktop::source::FrameProvenance::Stream {
                stream: FrameStreamInfo {
                    stream_id: "native-stream-1".into(),
                    image_frame_id: Some("frame-1".into()),
                    display_time_ticks: 90_000_000,
                    display_time_ns: 90_000_000,
                    received_uptime_ns: 95_000_000,
                    received_unix_ms: 1_700_000_000_095,
                    callback_ms: 95,
                    source_clock: "mach_absolute".into(),
                    window_id: "w1".into(),
                    pid: 42,
                    window_frame: Rect {
                        x: -720.0,
                        y: 0.0,
                        width: 720.0,
                        height: 450.0,
                    },
                    content_rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 1440.0,
                        height: 900.0,
                    },
                    content_scale: 1.0,
                    dirty_rects: vec![],
                    dropped_frames: 0,
                    capture_interval_ms: 200,
                },
            }),
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
        Message::SourceCommand(SourceCommand::RefreshNow { table_scope: None }),
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
    let envelope = all()
        .into_iter()
        .find(|(sample_name, _)| sample_name == name)
        .map(|(_, envelope)| envelope)
        .or_else(|| {
            representative_messages()
                .into_iter()
                .find(|(sample_name, _)| sample_name == name)
                .map(|(_, message)| envelope_for(message))
        })
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

    for (name, message) in representative_messages() {
        out.push(Fixture {
            name,
            schema_valid: true,
            decode_valid: true,
            message: serde_json::to_value(envelope_for(message)).expect("samples serialise"),
        });
    }

    out.push(mutated(
        "legacy_untagged_native_observation_rejected_by_v2",
        "observation",
        false,
        false,
        |message| {
            let legacy_observation = message
                .pointer("/message/payload/observation")
                .cloned()
                .expect("native observation has its tagged payload");
            message["message"]["payload"] = legacy_observation;
        },
    ));
    out.push(mutated(
        "synthetic_browser_intent_rejects_cross_session_evidence",
        "synthetic_browser_submit_intent",
        true,
        false,
        |message| {
            set(
                message,
                "/message/payload/intent/intent/session_epoch",
                serde_json::json!(1),
            )
        },
    ));

    for (name, scope, valid) in [
        (
            "valid_scoped_source_refresh",
            json!({"scroll_path":[{"role":"ax_scroll_area","label":"Projects"}],
                "column_headers":["Name","Owner"]}),
            true,
        ),
        (
            "source_refresh_rejects_row_conditions",
            json!({"scroll_path":[{"role":"ax_scroll_area","label":"Projects"}],
                "column_headers":["Name"],"conditions":[{"value":"Secret"}]}),
            false,
        ),
    ] {
        out.push(mutated(name, "source_command", valid, valid, |message| {
            set(
                message,
                "/message/payload",
                json!({"command":"refresh_now","table_scope":scope}),
            )
        }));
    }

    out.push(mutated(
        "window_activation_observation",
        "observation",
        true,
        true,
        |m| {
            let payload = m.pointer_mut("/message/payload/observation").unwrap();
            payload["purpose"] = json!("window_activation");
            payload["dependencies"] = json!({"authorization":1,"window":1});
            let proof = &mut payload["binding_proof"];
            let fp = &mut proof["fingerprint"];
            fp["role"] = json!("AXWindow");
            fp["subrole"] = json!("AXStandardWindow");
            fp["ancestor_roles"] = json!([]);
            fp["group"] = json!(null);
            fp["sibling_index"] = json!(0);
            fp["label"] = json!("settings");
            fp["frame"] = json!([0, 0, 100, 100]);
            proof["display_frame"] = json!({"x":0.0,"y":0.0,"width":100.0,"height":100.0});

            let context = &mut proof["device_context"];
            let frame = json!({"x":0.0,"y":0.0,"width":100.0,"height":100.0});
            let window_object = json!({
                "role":"AXWindow",
                "subrole":"AXStandardWindow",
                "label":"settings",
                "frame":frame
            });
            context["target"]["object"] = window_object.clone();
            context["target"]["group"] = json!(null);
            context["target"]["scroll"] = json!(null);
            context["ancestry"] = json!([]);
            context["sibling_parent_ancestry_index"] = json!(null);
            context["siblings"] = json!([window_object]);
            context["target_sibling_index"] = json!(0);
            context["nearest_labelled_group_ancestry_index"] = json!(null);
            context["scroll_ancestors"] = json!([]);
            context["menu"] = json!(null);
            context["focus"] = json!(null);
        },
    ));
    out.push(mutated(
        "window_activation_rejects_control_proof",
        "observation",
        true,
        false,
        |m| {
            m.pointer_mut("/message/payload/observation").unwrap()["purpose"] =
                json!("window_activation");
        },
    ));

    out.push(mutated(
        "unknown_binding_proof_field",
        "submit_intent",
        false,
        false,
        |m| insert_unexpected(m, "/message/payload/intent/intent/binding_proof"),
    ));
    for (name, pointer, value) in [
        (
            "binding_digest_mismatch",
            "/message/payload/intent/intent/binding_fingerprint",
            json!("fp1:wrong"),
        ),
        (
            "binding_target_mismatch",
            "/message/payload/intent/intent/binding_proof/binding",
            json!("other-binding"),
        ),
        (
            "binding_pid_zero",
            "/message/payload/intent/intent/binding_proof/fingerprint/app_pid",
            json!(0),
        ),
        (
            "binding_label_not_normalized",
            "/message/payload/intent/intent/binding_proof/fingerprint/label",
            json!("Wrong CASE"),
        ),
        (
            "binding_display_frame_empty",
            "/message/payload/intent/intent/binding_proof/display_frame/width",
            json!(0),
        ),
        (
            "binding_display_frame_differs",
            "/message/payload/intent/intent/binding_proof/display_frame/x",
            json!(11),
        ),
    ] {
        out.push(mutated(name, "submit_intent", true, false, |m| {
            set(m, pointer, value)
        }));
    }
    out.push(mutated(
        "observation_binding_target_mismatch",
        "observation",
        true,
        false,
        |m| {
            set(
                m,
                "/message/payload/observation/binding_proof/binding",
                json!("another-binding"),
            )
        },
    ));

    out.push(mutated(
        "binding_coordinate_space_mismatch",
        "submit_intent",
        true,
        false,
        |m| {
            set(
                m,
                "/message/payload/intent/intent/coordinate_space",
                json!("surface_px"),
            )
        },
    ));
    out.push(mutated(
        "binding_click_outside_frame",
        "submit_intent",
        true,
        false,
        |m| {
            set(
                m,
                "/message/payload/intent/intent/steps/0/primitive",
                serde_json::to_value(Primitive::Click {
                    button: super::primitive::MouseButton::Left,
                    x: 1_000,
                    y: 1_000,
                    modifiers: super::primitive::Modifiers::default(),
                })
                .unwrap(),
            )
        },
    ));

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
        |m| insert_unexpected(m, "/message/payload/intent/intent"),
    ));
    out.push(mutated(
        "unknown_field_in_nested_primitive",
        "submit_intent",
        false,
        false,
        |m| {
            insert_unexpected(
                m,
                "/message/payload/intent/intent/steps/0/primitive/payload/action",
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
        |m| set(m, "/protocol_version", json!(1)),
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
                "/message/payload/intent/intent/steps/0/primitive/type",
                json!("teleport"),
            )
        },
    ));
    out.push(mutated(
        "zero_duration",
        "submit_intent",
        false,
        false,
        |m| set(m, "/message/payload/intent/intent/duration_ms", json!(0)),
    ));
    out.push(mutated("no_steps", "submit_intent", false, false, |m| {
        set(m, "/message/payload/intent/intent/steps", json!([]))
    }));
    out.push(mutated(
        "root_epoch_zero",
        "submit_intent",
        false,
        false,
        |m| set(m, "/message/payload/intent/intent/root_epoch", json!(0)),
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
        |m| set(m, "/message/payload/observation/valid_until_ms", json!(10)),
    ));
    out.push(mutated(
        "offsets_not_strictly_increasing",
        "submit_intent",
        true,
        false,
        |m| {
            let step = m
                .pointer("/message/payload/intent/intent/steps/0")
                .cloned()
                .unwrap();
            m.pointer_mut("/message/payload/intent/intent/steps")
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
        |m| {
            set(
                m,
                "/message/payload/intent/intent/steps/0/offset_ms",
                json!(100),
            )
        },
    ));
    out.push(mutated(
        "key_released_but_never_held",
        "submit_intent",
        true,
        false,
        |m| {
            set(
                m,
                "/message/payload/intent/intent/steps/0/primitive",
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
                "/message/payload/intent/intent/steps/0/primitive",
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
        "snapshot_global_focus_window_unknown",
        "structure_snapshot",
        true,
        false,
        |m| {
            set(
                m,
                "/message/payload/global_focus/window_id",
                json!({"state": "known", "value": "ghost"}),
            )
        },
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
        |m| set(m, "/message/payload/protocol_min", json!(3)),
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
