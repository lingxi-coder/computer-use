use std::collections::BTreeMap;

use crate::environment::pack::AppPack;
use crate::execution::{
    DeviceCommand, DeviceNotice, DeviceReply, DeviceView, OwnedDocumentIntentSpec,
};
use crate::infrastructure::storage::Storage;
use crate::reaction::candidates::{Basis, Resolution, Scope, build};
use crate::reaction::decisions::DecisionBasis;
use crate::reaction::owned_text::{OwnedDocumentActionKind, OwnedDocumentActionPlan};
use crate::tasks::TaskConfig;
use crate::tasks::store::TaskStore;
use crate::verification::{owned_revisit_goal_supported, owned_text_value_diagnostic};
use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, CommandAck, DependencyVersions,
    DeviceIdentity, GrantOutcome, PermitRef, Receipt, ReceiptStatus, ReleaseStatus, ResumeAck,
    StopAck, StopDisposition, StopScope, StopSource, SubmittedAction, SurfaceRef,
};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{
    AttemptId, BindingRef, ObjectRef, RequestId, RootTaskId, SemanticTargetRef,
};
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode};
use interaction_contracts::desktop::owned_document::{
    AxPropertyObservation, DocumentBindingEvidence, DocumentBindingRead, DocumentReadStatus,
    MarkedRangeRead, MarkedTextRange, OwnedDocumentAncestor, OwnedDocumentProfile,
    OwnedDocumentReceiverEvidence, OwnedDocumentSourceProvenance, OwnedDocumentValueReadback,
    OwnedScopeObserverState,
};
use interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof;
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::planning::{GoalDraft, PlanBundle, Subgoal};
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive, PrimitivePolicy};
use interaction_contracts::desktop::source::{
    AppIdentity, KernelProcessIdentity, Rect, SourceCommand, SourceHealth, SourceIdentityBoundary,
    SourceKind, SourceState, StructureSnapshot,
};
use interaction_contracts::desktop::task::{
    ConfirmationScope, ExpectedValue, RowColumnConditionSpec, RowContainerRole, RowContainerScope,
    RowTableScope, RowTargetSpec, TargetSpec,
};
use interaction_contracts::desktop::verification::{EffectStatus, TimeSpan, TriState};
use sha2::{Digest, Sha256};
use sim_world::app::{SimApp, SnapshotMeta};

use super::owned_revisit::{
    OwnedRevisitCompletion, OwnedRevisitFileLineage, OwnedRevisitFileVersion,
};
use super::{Engine, EngineConfig, Input, Output, SourceScopeCommandState};

fn native_action_intent(
    intent: &interaction_contracts::desktop::device::ExecutionIntent,
) -> &interaction_contracts::desktop::device::ActionIntent {
    intent
        .native()
        .expect("this regression covers a native intent")
}

const REGISTRATION_ID: &str = "00000000-0000-4000-8000-000000000001";
const NAMESPACE_A: &str = "d85e0f74-b6f7-4bce-8f8c-563e4b12da29";
const NAMESPACE_B: &str = "e85e0f74-b6f7-4bce-8f8c-563e4b12da29";
const PID: u32 = 700;
const START_SECONDS: u64 = 1_700_000_000;
const START_MICROS: u64 = 123_456;
const CLOCK: &str = "native/1/monotonic";
const MACOS_CLOCK: &str = "macos/1/monotonic";

fn registration(pid: u32, start_seconds: u64) -> OwnedDocumentRegistration {
    OwnedDocumentRegistration {
        version: 1,
        registration_id: REGISTRATION_ID.into(),
        pid,
        start_seconds,
        start_micros: START_MICROS,
        executable_path: "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit".into(),
        bundle_id: "com.apple.TextEdit".into(),
        document_path: "/private/tmp/document.txt".into(),
        document_sha256: "a".repeat(64),
        document_dev: 1,
        document_ino: 2,
        directory_dev: 1,
        directory_ino: 3,
        issued_unix_ms: 1_700_000_000_000,
    }
}

fn process(pid: u32, start_seconds: u64) -> KernelProcessIdentity {
    KernelProcessIdentity {
        pid,
        start_seconds,
        start_micros: START_MICROS,
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn evidence(
    snapshot_id: &str,
    source_sequence: u64,
    observed_ms: u64,
    clock_domain: &str,
    namespace: &str,
    generation: u64,
    window: (&str, Rect),
) -> OwnedDocumentReceiverEvidence {
    let (window_id, window_frame) = window;
    let actual = ObjectRef::new(format!("owned-receiver-{source_sequence}")).unwrap();
    let scroll = ObjectRef::new(format!("owned-scroll-{source_sequence}")).unwrap();
    let window_object = ObjectRef::new(format!("owned-window-{source_sequence}")).unwrap();
    OwnedDocumentReceiverEvidence {
        registration_id: REGISTRATION_ID.into(),
        profile: OwnedDocumentProfile::OwnedPlainTextValueSetterV1,
        provenance: OwnedDocumentSourceProvenance {
            identity_boundary: SourceIdentityBoundary {
                namespace: namespace.into(),
                generation,
            },
            snapshot_id: RequestId::new(snapshot_id).unwrap(),
            source_sequence,
            observed_ms,
            valid_until_ms: observed_ms + 5_000,
            clock_domain: clock_domain.into(),
        },
        process: process(PID, START_SECONDS),
        window_id: window_id.into(),
        window_object_ref: window_object.clone(),
        window_frame,
        actual_object_ref: actual.clone(),
        receiver_frame: rect(
            window_frame.x + 10.0,
            window_frame.y + 20.0,
            window_frame.width - 20.0,
            window_frame.height - 40.0,
        ),
        application_focused_object_ref: actual.clone(),
        system_focused_object_ref: actual.clone(),
        ancestry: vec![
            OwnedDocumentAncestor {
                object_ref: scroll,
                role: "AXScrollArea".into(),
            },
            OwnedDocumentAncestor {
                object_ref: window_object,
                role: "AXWindow".into(),
            },
        ],
        scope_complete: true,
        profile_eligible: true,
        marked_range_stable_unmarked: true,
        observer_state: OwnedScopeObserverState::Continuous,
        secure: false,
        document_binding_verified: true,
        document_binding: DocumentBindingEvidence {
            before: DocumentBindingRead {
                status: DocumentReadStatus::Value,
                api_code: Some(0),
                cf_type_id: Some(7),
                observed_ms: observed_ms + 1,
                timely: true,
                registered_path_matches: true,
            },
            after: DocumentBindingRead {
                status: DocumentReadStatus::Value,
                api_code: Some(0),
                cf_type_id: Some(7),
                observed_ms: observed_ms + 2,
                timely: true,
                registered_path_matches: true,
            },
        },
        title: AxPropertyObservation::Value {
            value: "Owned fixture.txt".into(),
        },
        role: AxPropertyObservation::Value {
            value: "AXTextArea".into(),
        },
        subrole: AxPropertyObservation::Unsupported { code: -25205 },
        enabled: AxPropertyObservation::Unsupported { code: -25205 },
        focused: AxPropertyObservation::Value { value: true },
        value_settable: AxPropertyObservation::Value { value: true },
        description: AxPropertyObservation::Failure { code: -25200 },
        marked_range_before: MarkedRangeRead {
            receiver_object_ref: actual.clone(),
            receiver_same_cf_as_actual_object_ref: true,
            observed_ms: observed_ms + 3,
            observation: AxPropertyObservation::Value {
                value: MarkedTextRange {
                    location: 0,
                    length: 0,
                },
            },
        },
        marked_range_after: MarkedRangeRead {
            receiver_object_ref: actual,
            receiver_same_cf_as_actual_object_ref: true,
            observed_ms: observed_ms + 4,
            observation: AxPropertyObservation::Value {
                value: MarkedTextRange {
                    location: 0,
                    length: 0,
                },
            },
        },
        value_readback: None,
    }
}

fn snapshot(
    sequence: u64,
    observed_ms: u64,
    namespace: &str,
    generation: u64,
) -> StructureSnapshot {
    let snapshot_id = format!("owned-read-{sequence}");
    let meta = SnapshotMeta {
        snapshot_id: snapshot_id.clone(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: CLOCK.into(),
        source_sequence: sequence,
        observed_ms,
    };
    let mut snapshot = SimApp::new().snapshot(&meta);
    snapshot.app = AppIdentity {
        bundle_id: "com.apple.TextEdit".into(),
        name: "TextEdit".into(),
        pid: PID,
    };
    for window in &mut snapshot.windows {
        window.owner_bundle_id = "com.apple.TextEdit".into();
    }
    snapshot.complete = false;
    snapshot.incomplete_reason = Some("bounded generic AX traversal".into());
    crate::testkit::focus_window_absent(&mut snapshot);
    snapshot.objects.clear();
    sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
    // This fixture observes TextEdit only through its independent private
    // receiver channel. It carries no generic AX observer claim or bindings.
    snapshot.observer_coverage.state = SourceState::Unavailable;
    snapshot.observer_coverage.covered_objects = 0;
    snapshot.observer_coverage.read_scoped_objects = 0;
    snapshot.observer_coverage.failed_registrations = 0;
    snapshot.observer_coverage.failures.clear();
    // Private identity is carried in the typed sidecar. The generic source
    // identity boundary remains absent in this TextEdit observation mode.
    snapshot.identity_boundary = None;
    let source_window = snapshot.windows[0].clone();
    snapshot.owned_document_evidence = Some(Box::new(evidence(
        &snapshot_id,
        sequence,
        observed_ms,
        CLOCK,
        namespace,
        generation,
        (&source_window.window_id, source_window.frame),
    )));
    snapshot.windows.clear();
    snapshot
}

fn macos_snapshot(mut snapshot: StructureSnapshot) -> StructureSnapshot {
    snapshot.clock_domain = MACOS_CLOCK.into();
    if let Some(evidence) = snapshot.owned_document_evidence.as_deref_mut() {
        evidence.provenance.clock_domain = MACOS_CLOCK.into();
    }
    snapshot
}

fn with_value_readback(mut snapshot: StructureSnapshot, observed_ms: u64) -> StructureSnapshot {
    let evidence = snapshot
        .owned_document_evidence
        .as_deref_mut()
        .expect("owned proof fixture");
    evidence.value_readback = Some(OwnedDocumentValueReadback {
        receiver_object_ref: evidence.actual_object_ref.clone(),
        receiver_same_cf_as_actual_object_ref: true,
        observed_ms,
        sha256: "b".repeat(64),
        utf8_byte_count: 123,
    });
    snapshot
}

fn assert_rejected_value_readback(snapshot: StructureSnapshot) {
    assert!(snapshot.validate().is_err());
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    admit(&mut engine, snapshot, 1_050);
    assert!(engine.owned_document_binding().is_none());
}

fn engine(registration: Option<OwnedDocumentRegistration>) -> Engine {
    let pack = AppPack::registered_native("com.apple.TextEdit").unwrap();
    let mut config = EngineConfig::new(TaskConfig::new(vec![pack]));
    config.owned_document_registration = registration.map(Box::new);
    Engine::new(config, Storage::open_in_memory().unwrap())
}

fn binding_proof(engine: &Engine, binding: &str) -> OwnedDocumentBindingProof {
    let env = engine.state().latest().expect("admitted snapshot");
    engine
        .owned_document_binding()
        .expect("current private binding")
        .binding_proof(
            BindingRef::new(binding).unwrap(),
            SemanticTargetRef::new("document/text").unwrap(),
            env.revision,
        )
        .unwrap()
}

fn device_view(session_epoch: u64) -> DeviceView {
    let mut view = DeviceView::new();
    view.install_manifest(CapabilityManifest {
        identity: DeviceIdentity {
            session_epoch,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: CLOCK.into(),
            dependencies: BTreeMap::from([
                ("authorization".into(), 1),
                ("focus".into(), 2),
                ("window".into(), 3),
                ("owned_scope".into(), 4),
            ]),
        },
        backend: BackendKind::Macos,
        execution_domain: ExecutionDomain::ForegroundNative,
        policy: PrimitivePolicy::AtomicOnly,
        human_input: CapabilityState::Available,
        hit_test: CapabilityState::Available,
        input: CapabilityState::Available,
        domains: crate::testkit::domain_capabilities_for_test_profile(
            ExecutionDomain::ForegroundNative,
        ),
        max_uninterruptible_ms: 100,
    });
    view
}

fn owned_text_root(value: &str) -> (Engine, SemanticTargetRef) {
    owned_document_root(
        value,
        false,
        vec![Subgoal {
            id: "fill-owned-document".into(),
            description: "Set the open document text".into(),
            skill: "ui.enter_text".into(),
            target: Some(SemanticTargetRef::new("document/text").unwrap()),
            expects: vec!["text.value@1".into()],
            scroll: None,
            select: None,
        }],
    )
}

fn owned_save_root(value: &str, allow_save: bool) -> (Engine, SemanticTargetRef) {
    owned_document_root(
        value,
        allow_save,
        vec![Subgoal {
            id: "save-owned-document".into(),
            description: "Save the open document after checking its text".into(),
            skill: "ui.save_document".into(),
            target: Some(SemanticTargetRef::new("document/text").unwrap()),
            expects: vec![],
            scroll: None,
            select: None,
        }],
    )
}

fn owned_fill_save_root(value: &str, allow_save: bool) -> (Engine, SemanticTargetRef) {
    let target = SemanticTargetRef::new("document/text").unwrap();
    owned_document_root(
        value,
        allow_save,
        vec![
            Subgoal {
                id: "fill-owned-document".into(),
                description: "Set the open document text".into(),
                skill: "ui.enter_text".into(),
                target: Some(target.clone()),
                expects: vec!["text.value@1".into()],
                scroll: None,
                select: None,
            },
            Subgoal {
                id: "save-owned-document".into(),
                description: "Save the open document after checking its text".into(),
                skill: "ui.save_document".into(),
                target: Some(target.clone()),
                expects: vec![],
                scroll: None,
                select: None,
            },
        ],
    )
}

fn owned_document_root(
    value: &str,
    allow_save: bool,
    subgoals: Vec<Subgoal>,
) -> (Engine, SemanticTargetRef) {
    let target = SemanticTargetRef::new("document/text").unwrap();
    let pack = AppPack::registered_native("com.apple.TextEdit").unwrap();
    let mut config = EngineConfig::new(TaskConfig::new(vec![pack]));
    config.initial_mode = RunMode::Execute;
    config.owned_document_registration = Some(Box::new(registration(PID, START_SECONDS)));
    config.allow_owned_document_save = allow_save;
    let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());

    engine
        .tasks
        .create_root(
            &mut engine.storage,
            100,
            "Fill the open TextEdit document with the requested text",
        )
        .unwrap();
    let draft = GoalDraft {
        app: "com.apple.TextEdit".into(),
        account: None,
        target: TargetSpec {
            row: None,
            semantic_target: target.clone(),
            label: "Text document".into(),
            group: None,
            role_hint: Some("AXTextArea".into()),
        },
        desired: ExpectedValue::Text {
            value: value.into(),
        },
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::ReversibleEdit,
    };
    let plan = PlanBundle {
        subgoals,
        dependencies: Vec::new(),
        required_observations: Vec::new(),
        expected_effects: vec!["text.value@1".into()],
        recovery_branches: Vec::new(),
        termination: Vec::new(),
    };
    let confirmation = engine
        .tasks
        .accept_draft(
            &mut engine.storage,
            100,
            &draft,
            ExecutionDomain::ForegroundNative,
            &plan,
        )
        .unwrap();
    // The confirmed root is a native foreground task, so its execution profile
    // must already be available when Engine processes the confirmation. This
    // fixture supplies the mock device before confirmation, just as the host
    // receives the device manifest before allowing execution.
    engine.device = device_view(1);
    engine.device.note_lease(true);
    let confirmation_outputs = engine.handle(
        100,
        Input::Operator(OperatorCommand::ConfirmGoal {
            confirmation_id: confirmation.confirmation_id,
            approved: true,
        }),
    );
    let (register_corr, register_command) = find_device_command(&confirmation_outputs, |command| {
        matches!(command, DeviceCommand::RegisterRoot(_))
    })
    .expect("confirmation registers the frozen root with the Device");
    assert!(matches!(register_command, DeviceCommand::RegisterRoot(_)));
    let refresh_outputs = engine.handle(
        100,
        Input::Device {
            corr: register_corr,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    );
    let refresh_generation = refresh_outputs
        .iter()
        .rev()
        .find_map(|output| match output {
            Output::Source {
                generation,
                command: SourceCommand::RefreshNow { .. },
            } => Some(*generation),
            _ => None,
        })
        .expect("a registered root requests its initial Source read");
    engine.handle(
        100,
        Input::SourceRefreshStarted {
            generation: refresh_generation,
            accepted: true,
        },
    );
    (engine, target)
}

fn require_native_frames(engine: &mut Engine) {
    let mut manifest = engine
        .device
        .manifest()
        .expect("owned test device manifest")
        .clone();
    manifest.backend = BackendKind::Macos;
    manifest.identity.clock_domain = MACOS_CLOCK.into();
    assert!(engine.state.set_identity(&manifest.identity, true));
    engine.device.install_manifest(manifest);
}

fn acknowledge_dependency_updates(
    engine: &mut Engine,
    outputs: &[Output],
    now_ms: u64,
) -> Vec<Output> {
    let updates = outputs
        .iter()
        .filter_map(|output| match output {
            Output::Device { corr, command } => match command.as_ref() {
                DeviceCommand::UpdateDependency(update) => Some((*corr, update.name.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut replies = Vec::new();
    for (index, (corr, name)) in updates.into_iter().enumerate() {
        let mut versions = engine.device.dependencies().clone();
        let next_revision = versions
            .get(&name)
            .copied()
            .unwrap_or_default()
            .saturating_add(1);
        versions.insert(name, next_revision);
        replies.extend(engine.handle(
            now_ms.saturating_add(index as u64),
            Input::Device {
                corr,
                reply: DeviceReply::Dependencies(DependencyVersions { versions }),
            },
        ));
    }
    replies
}

fn find_device_command(
    outputs: &[Output],
    matches: impl Fn(&DeviceCommand) -> bool,
) -> Option<(u64, DeviceCommand)> {
    outputs.iter().find_map(|output| match output {
        Output::Device { corr, command } if matches(command) => {
            Some((*corr, command.as_ref().clone()))
        }
        _ => None,
    })
}

fn submit_owned_text(
    engine: &mut Engine,
    source: StructureSnapshot,
    now_ms: u64,
) -> (
    interaction_contracts::desktop::device::SubmitIntent,
    u64,
    Vec<Output>,
) {
    submit_owned_action(engine, source, now_ms, OwnedDocumentActionKind::SetValue)
}

fn submit_owned_action(
    engine: &mut Engine,
    source: StructureSnapshot,
    now_ms: u64,
    action: OwnedDocumentActionKind,
) -> (
    interaction_contracts::desktop::device::SubmitIntent,
    u64,
    Vec<Output>,
) {
    let mut outputs = admit(engine, source, now_ms);
    let dependency_replies = acknowledge_dependency_updates(engine, &outputs, now_ms + 1);
    outputs.extend(dependency_replies);
    if engine
        .device
        .identity()
        .is_some_and(|identity| identity.clock_domain.starts_with("macos/"))
    {
        outputs.extend(engine.handle(now_ms + 101, Input::Tick));
    }
    let (observe_corr, observation) = find_device_command(&outputs, |command| {
        matches!(command, DeviceCommand::Observe(_))
    })
    .expect("owned candidate reaches the typed observation request");
    let DeviceCommand::Observe(observation) = observation else {
        unreachable!()
    };
    let observation = observation
        .native()
        .expect("owned document observe is native");
    assert_eq!(
        observation.purpose,
        match action {
            OwnedDocumentActionKind::SetValue => {
                interaction_contracts::desktop::device::ObservationPurpose::OwnedDocumentValueSetter
            }
            OwnedDocumentActionKind::Save => {
                interaction_contracts::desktop::device::ObservationPurpose::OwnedDocumentSave
            }
        }
    );
    assert!(observation.binding_proof.is_none());
    assert!(observation.owned_document_proof.is_some());
    outputs.extend(engine.handle(
        now_ms + 1,
        Input::Device {
            corr: observe_corr,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
    let (grant_corr, grant) = find_device_command(&outputs, |command| {
        matches!(command, DeviceCommand::GrantPermit(_))
    })
    .expect("the observation reaches the existing permit stage");
    assert!(matches!(grant, DeviceCommand::GrantPermit(_)));
    let permit = PermitRef {
        id: "owned-permit-1".into(),
        generation: 1,
    };
    outputs.extend(engine.handle(
        now_ms + 2,
        Input::Device {
            corr: grant_corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
        },
    ));
    let (submit_corr, submit) = find_device_command(&outputs, |command| {
        matches!(command, DeviceCommand::Submit(_))
    })
    .expect("an admitted one-shot owned attempt reaches Submit");
    let DeviceCommand::Submit(submit) = submit else {
        unreachable!()
    };
    let intent = native_action_intent(&submit.intent);
    assert_eq!(intent.steps.len(), 1);
    match action {
        OwnedDocumentActionKind::SetValue => assert!(matches!(
            &intent.steps[0].primitive,
            Primitive::AxAction {
                action: AxActionKind::SetValue { .. }
            }
        )),
        OwnedDocumentActionKind::Save => assert!(matches!(
            &intent.steps[0].primitive,
            Primitive::AxAction {
                action: AxActionKind::SaveOwnedDocument { .. }
            }
        )),
    }
    assert_eq!(intent.steps[0].offset_ms, 0);
    assert!(intent.binding_proof.is_none());
    assert!(intent.owned_document_proof.is_some());
    (submit, submit_corr, outputs)
}

fn current_owned_candidates(
    engine: &Engine,
) -> Vec<interaction_contracts::desktop::decision::Candidate> {
    let env = engine.state.latest().expect("current Source read");
    let run = engine.run.as_ref().expect("active owned root");
    let subgoal = engine.next_subgoal().expect("active owned step");
    let resolution = engine
        .current_basis()
        .and_then(|basis| basis.owned_document)
        .map(|proof| Resolution::OwnedDocument(proof.binding))
        .unwrap_or(Resolution::NotFound {
            observation_complete: false,
        });
    build(
        &Scope {
            goal: &run.goal,
            plan_targets: &run.plan_targets,
            pack: &run.pack,
            env,
        },
        &Basis::Step {
            subgoal: &subgoal,
            resolution: &resolution,
        },
    )
}

fn setter_receipt(
    submit: &interaction_contracts::desktop::device::SubmitIntent,
    value: &str,
    status: ReceiptStatus,
) -> Receipt {
    let start_ms = submit.intent.latest_start_ms();
    let end_ms = start_ms + 5;
    Receipt {
        intent_id: submit.intent.intent_id().to_owned(),
        attempt_id: submit.intent.attempt_id().map(str::to_owned),
        session_epoch: submit.intent.session_epoch(),
        status,
        reason_code: None,
        reason: None,
        submitted: vec![SubmittedAction::Native {
            at_ms: start_ms,
            primitive: Primitive::AxAction {
                action: AxActionKind::SetValue {
                    value: value.into(),
                },
            },
            cleanup: false,
        }],
        release_status: ReleaseStatus::NotNeeded,
        command_end_ms: Some(end_ms + 1),
        input_end_ms: Some(end_ms),
        at_ms: end_ms + 1,
    }
}

fn save_receipt(
    submit: &interaction_contracts::desktop::device::SubmitIntent,
    expected_value: &str,
    status: ReceiptStatus,
) -> Receipt {
    let start_ms = submit.intent.latest_start_ms();
    let end_ms = start_ms + 5;
    Receipt {
        intent_id: submit.intent.intent_id().to_owned(),
        attempt_id: submit.intent.attempt_id().map(str::to_owned),
        session_epoch: submit.intent.session_epoch(),
        status,
        reason_code: None,
        reason: None,
        submitted: vec![SubmittedAction::Native {
            at_ms: start_ms,
            primitive: Primitive::AxAction {
                action: AxActionKind::SaveOwnedDocument {
                    expected_value: expected_value.into(),
                },
            },
            cleanup: false,
        }],
        release_status: ReleaseStatus::NotNeeded,
        command_end_ms: Some(end_ms + 1),
        input_end_ms: Some(end_ms),
        at_ms: end_ms + 1,
    }
}

fn readback_after_input(
    original: &OwnedDocumentReceiverEvidence,
    sequence: u64,
    observed_ms: u64,
    namespace: &str,
    generation: u64,
    digest_observed_ms: Option<u64>,
    value: &str,
) -> StructureSnapshot {
    let mut current = snapshot(sequence, observed_ms, namespace, generation);
    reuse_receiver_identity(&mut current, original);
    match digest_observed_ms {
        Some(read_ms) => with_value_digest(current, read_ms, value),
        None => current,
    }
}

fn reuse_receiver_identity(
    snapshot: &mut StructureSnapshot,
    original: &OwnedDocumentReceiverEvidence,
) {
    let evidence = snapshot
        .owned_document_evidence
        .as_deref_mut()
        .expect("owned proof fixture");
    let actual = original.actual_object_ref.clone();
    evidence.window_id = original.window_id.clone();
    evidence.window_object_ref = original.window_object_ref.clone();
    evidence.window_frame = original.window_frame;
    evidence.actual_object_ref = actual.clone();
    evidence.receiver_frame = original.receiver_frame;
    evidence.application_focused_object_ref = actual.clone();
    evidence.system_focused_object_ref = actual.clone();
    evidence.ancestry = original.ancestry.clone();
    evidence.marked_range_before.receiver_object_ref = actual.clone();
    evidence.marked_range_after.receiver_object_ref = actual.clone();
    if let Some(readback) = evidence.value_readback.as_mut() {
        readback.receiver_object_ref = actual;
    }
}

fn with_value_digest(
    snapshot: StructureSnapshot,
    observed_ms: u64,
    value: &str,
) -> StructureSnapshot {
    let mut snapshot = with_value_readback(snapshot, observed_ms);
    let readback = snapshot
        .owned_document_evidence
        .as_deref_mut()
        .unwrap()
        .value_readback
        .as_mut()
        .unwrap();
    readback.sha256 = format!("{:x}", Sha256::digest(value.as_bytes()));
    readback.utf8_byte_count = value.len() as u32;
    snapshot
}

fn admit(engine: &mut Engine, snapshot: StructureSnapshot, now_ms: u64) -> Vec<Output> {
    engine.handle(now_ms, Input::Structure(Box::new(snapshot)))
}

fn accept_pending_source_refresh(
    engine: &mut Engine,
    outputs: &[Output],
    now_ms: u64,
) -> Vec<Output> {
    let generation = outputs.iter().rev().find_map(|output| match output {
        Output::Source {
            generation,
            command: interaction_contracts::desktop::source::SourceCommand::RefreshNow { .. },
        } => Some(*generation),
        _ => None,
    });
    generation.map_or_else(Vec::new, |generation| {
        engine.handle(
            now_ms,
            Input::SourceRefreshStarted {
                generation,
                accepted: true,
            },
        )
    })
}

fn original_receiver(
    submit: &interaction_contracts::desktop::device::SubmitIntent,
) -> OwnedDocumentReceiverEvidence {
    native_action_intent(&submit.intent)
        .owned_document_proof
        .as_deref()
        .expect("owned submit carries its private proof")
        .receiver
        .as_ref()
        .clone()
}

#[test]
fn owned_proof_binds_on_a_generic_incomplete_read_without_authorizing_input() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    let outputs = admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);

    let binding = engine
        .owned_document_binding()
        .expect("private proof binds");
    assert_eq!(binding.snapshot_id().as_str(), "owned-read-1");
    assert_eq!(binding.source_sequence(), 1);
    assert_eq!(binding.window_id(), "w1");
    assert!(engine.state().latest().unwrap().snapshot.objects.is_empty());
    assert!(!engine.state().latest().unwrap().snapshot.complete);
    assert_eq!(
        engine
            .state()
            .latest()
            .unwrap()
            .snapshot
            .observer_coverage
            .state,
        SourceState::Unavailable
    );
    assert!(
        engine
            .state()
            .latest()
            .unwrap()
            .snapshot
            .global_focus
            .known_window_for_app(&engine.state().latest().unwrap().snapshot.app)
            .is_none()
    );
    assert!(
        engine
            .state()
            .latest()
            .unwrap()
            .snapshot
            .identity_boundary
            .is_none()
    );
    assert!(!engine.state().usable(1_050, 2_000));
    assert_eq!(engine.bindings().all().count(), 0);
    assert!(
        outputs
            .iter()
            .all(|output| !matches!(output, Output::Device { .. }))
    );
}

#[test]
fn valid_value_readback_stays_private_and_read_only_on_an_incomplete_generic_read() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    let read = with_value_readback(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005);
    let outputs = admit(&mut engine, read, 1_050);

    let binding = engine
        .owned_document_binding()
        .expect("private receiver proof binds");
    let readback = binding
        .evidence()
        .value_readback
        .as_ref()
        .expect("bounded value digest remains available");
    assert_eq!(readback.receiver_object_ref, *binding.receiver_object_ref());
    assert!(readback.receiver_same_cf_as_actual_object_ref);
    assert_eq!(readback.observed_ms, 1_005);
    assert_eq!(readback.sha256, "b".repeat(64));
    assert_eq!(readback.utf8_byte_count, 123);

    let admitted = &engine.state().latest().unwrap().snapshot;
    assert!(!admitted.complete);
    assert!(admitted.objects.is_empty());
    assert!(
        admitted
            .global_focus
            .known_window_for_app(&admitted.app)
            .is_none()
    );
    assert!(admitted.identity_boundary.is_none());
    assert!(!engine.state().usable(1_050, 2_000));
    assert_eq!(engine.bindings().all().count(), 0);
    assert!(engine.root_state().is_none());
    assert!(
        outputs
            .iter()
            .all(|output| !matches!(output, Output::Device { .. }))
    );
}

#[test]
fn invalid_value_readbacks_are_rejected_and_never_become_current() {
    let mut wrong_receiver = with_value_readback(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005);
    wrong_receiver
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .value_readback
        .as_mut()
        .unwrap()
        .receiver_object_ref = ObjectRef::new("other-receiver").unwrap();
    assert_rejected_value_readback(wrong_receiver);

    let mut wrong_cf = with_value_readback(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005);
    wrong_cf
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .value_readback
        .as_mut()
        .unwrap()
        .receiver_same_cf_as_actual_object_ref = false;
    assert_rejected_value_readback(wrong_cf);

    let mut before_provenance = with_value_readback(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005);
    before_provenance
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .value_readback
        .as_mut()
        .unwrap()
        .observed_ms = 999;
    assert_rejected_value_readback(before_provenance);

    let mut at_expiry = with_value_readback(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005);
    at_expiry
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .value_readback
        .as_mut()
        .unwrap()
        .observed_ms = 6_000;
    assert_rejected_value_readback(at_expiry);

    let mut malformed_hash = with_value_readback(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005);
    malformed_hash
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .value_readback
        .as_mut()
        .unwrap()
        .sha256 = "G".repeat(64);
    assert_rejected_value_readback(malformed_hash);

    let mut too_large = with_value_readback(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005);
    too_large
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .value_readback
        .as_mut()
        .unwrap()
        .utf8_byte_count = 2_049;
    assert_rejected_value_readback(too_large);
}

#[test]
fn future_value_readback_is_not_current_even_when_the_generic_snapshot_is_admitted() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    let future_read = with_value_readback(snapshot(1, 1_000, NAMESPACE_A, 3), 1_051);
    assert!(future_read.validate().is_ok());
    admit(&mut engine, future_read, 1_050);

    assert!(engine.state().latest().is_some());
    assert!(engine.owned_document_binding().is_none());
}

#[test]
fn owned_action_builders_keep_the_typed_scope_and_one_atomic_setter() {
    let registration = registration(PID, START_SECONDS);
    let mut engine = engine(Some(registration.clone()));
    admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    let proof = binding_proof(&engine, "owned-text-binding");
    assert_eq!(proof.receiver.registration_id.as_str(), REGISTRATION_ID);
    assert_eq!(proof.receiver.process, process(PID, START_SECONDS));
    assert_eq!(proof.session_epoch, 1);
    assert_eq!(proof.surface.id, "main");

    let value = "private owned text π";
    let plan = OwnedDocumentActionPlan::new(proof.clone(), value.into()).unwrap();
    assert_eq!(plan.proof(), &proof);
    assert_eq!(plan.expected_value(), value);
    assert!(!format!("{plan:?}").contains(value));
    assert!(OwnedDocumentActionPlan::new(proof.clone(), "x".repeat(2_049)).is_err());

    let view = device_view(1);
    let observation = view
        .owned_document_observation(&plan, &registration, 1_050, 2_000, 3_000, 5_000)
        .unwrap();
    assert_eq!(
        observation.purpose,
        interaction_contracts::desktop::device::ObservationPurpose::OwnedDocumentValueSetter
    );
    assert_eq!(
        observation.freshness,
        interaction_contracts::desktop::device::SourceFreshness::Verified
    );
    assert!(observation.binding_proof.is_none());
    assert_eq!(observation.owned_document_proof.as_deref(), Some(&proof));
    assert_eq!(observation.dependencies.len(), 4);
    assert_eq!(observation.valid_until_ms, 4_000);

    let permit = PermitRef {
        id: "permit-1".into(),
        generation: 1,
    };
    let attempt = AttemptId::new("att-1").unwrap();
    let root = RootTaskId::new("root-1").unwrap();
    let intent = view
        .owned_document_intent(&OwnedDocumentIntentSpec::new(
            &attempt,
            &root,
            1,
            1,
            &permit,
            &observation,
            &plan,
            1,
            1_050,
            2_000,
            100,
            100,
            &registration,
        ))
        .unwrap();
    assert_eq!(intent.intent.target_ref(), proof.binding.as_str());
    assert_eq!(intent.intent.evidence_id(), observation.observation_id);
    assert_eq!(intent.intent.attempt_id(), Some("att-1"));
    let native = native_action_intent(&intent.intent);
    assert!(native.binding_fingerprint.is_none());
    assert!(native.binding_proof.is_none());
    assert_eq!(native.owned_document_proof.as_deref(), Some(&proof));
    assert_eq!(native.steps.len(), 1);
    assert!(matches!(
        &native.steps[0].primitive,
        Primitive::AxAction {
            action: AxActionKind::SetValue { value: submitted }
        } if submitted == value
    ));

    assert!(OwnedDocumentActionPlan::save(proof.clone(), value.into()).is_err());

    let save_registration = registration.clone();
    let mut save_engine = self::engine(Some(save_registration.clone()));
    admit(
        &mut save_engine,
        with_value_digest(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005, value),
        1_050,
    );
    let save_proof = binding_proof(&save_engine, "owned-text-binding");
    let save_plan = OwnedDocumentActionPlan::save(save_proof.clone(), value.into()).unwrap();
    let save_observation = view
        .owned_document_observation(&save_plan, &save_registration, 1_050, 2_000, 3_000, 5_000)
        .unwrap();
    assert_eq!(
        save_observation.purpose,
        interaction_contracts::desktop::device::ObservationPurpose::OwnedDocumentSave
    );
    assert!(save_observation.binding_proof.is_none());
    assert_eq!(
        save_observation.owned_document_proof.as_deref(),
        Some(&save_proof)
    );
    assert_eq!(save_observation.dependencies.len(), 4);
    let save_intent = view
        .owned_document_intent(&OwnedDocumentIntentSpec::new(
            &attempt,
            &root,
            1,
            1,
            &permit,
            &save_observation,
            &save_plan,
            2,
            1_050,
            2_000,
            100,
            100,
            &save_registration,
        ))
        .unwrap();
    let native = native_action_intent(&save_intent.intent);
    assert_eq!(native.steps.len(), 1);
    assert_eq!(native.steps[0].offset_ms, 0);
    assert!(matches!(
        &native.steps[0].primitive,
        Primitive::AxAction {
            action: AxActionKind::SaveOwnedDocument { expected_value }
        } if expected_value == value
    ));
    let mut mismatch = save_proof.clone();
    mismatch.receiver.value_readback.as_mut().unwrap().sha256 = "f".repeat(64);
    assert!(OwnedDocumentActionPlan::save(mismatch, value.into()).is_err());

    let wrong_session = device_view(2);
    assert!(
        wrong_session
            .owned_document_observation(&plan, &registration, 1_050, 2_000, 3_000, 5_000)
            .is_err()
    );
    assert!(
        view.owned_document_observation(&plan, &registration, 1_050, 50, 3_000, 5_000)
            .is_err()
    );
    assert!(
        view.owned_document_observation(&plan, &registration, 1_050, 2_000, 3_000, 1_050)
            .is_err()
    );

    let mut missing_dependency = device_view(1);
    missing_dependency.replace_dependencies(BTreeMap::from([
        ("authorization".into(), 1),
        ("focus".into(), 2),
        ("window".into(), 3),
    ]));
    assert!(
        missing_dependency
            .owned_document_observation(&plan, &registration, 1_050, 2_000, 3_000, 5_000)
            .is_err()
    );
}

#[test]
fn save_candidate_is_private_opt_in_and_needs_the_frozen_value_readback() {
    let expected = "save this exact text";
    let read = || with_value_digest(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005, expected);

    let pack = AppPack::registered_native("com.apple.TextEdit").unwrap();
    let mut disabled_config = EngineConfig::new(TaskConfig::new(vec![pack]));
    disabled_config.owned_document_registration = Some(Box::new(registration(PID, START_SECONDS)));
    let mut disabled = Engine::new(disabled_config, Storage::open_in_memory().unwrap());
    disabled
        .tasks
        .create_root(
            &mut disabled.storage,
            100,
            "Fill the open TextEdit document with the requested text",
        )
        .unwrap();
    let target = SemanticTargetRef::new("document/text").unwrap();
    let draft = GoalDraft {
        app: "com.apple.TextEdit".into(),
        account: None,
        target: TargetSpec {
            row: None,
            semantic_target: target.clone(),
            label: "Text document".into(),
            group: None,
            role_hint: Some("AXTextArea".into()),
        },
        desired: ExpectedValue::Text {
            value: expected.into(),
        },
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::ReversibleEdit,
    };
    let save_plan = PlanBundle {
        subgoals: vec![Subgoal {
            id: "save-owned-document".into(),
            description: "Save the open document after checking its text".into(),
            skill: "ui.save_document".into(),
            target: Some(target),
            expects: vec![],
            scroll: None,
            select: None,
        }],
        dependencies: Vec::new(),
        required_observations: Vec::new(),
        expected_effects: vec!["text.value@1".into()],
        recovery_branches: Vec::new(),
        termination: Vec::new(),
    };
    assert!(matches!(
        disabled.tasks.accept_draft(
            &mut disabled.storage,
            100,
            &draft,
            ExecutionDomain::ForegroundNative,
            &save_plan,
        ),
        Err(crate::tasks::DraftError::Compile(
            crate::tasks::goal::CompileError::Plan(
                crate::tasks::goal::PlanRejection::UnregisteredSkill(skill)
            )
        )) if skill == "ui.save_document"
    ));

    let (mut enabled, _) = owned_save_root(expected, true);
    enabled.mode = RunMode::Observe;
    admit(&mut enabled, read(), 1_050);
    assert!(enabled.current_basis().unwrap().owned_document.is_some());
    let candidates = current_owned_candidates(&enabled);
    let save = candidates
        .iter()
        .find(|candidate| candidate.skill == "ui.save_document")
        .expect("S1 receives the private Save candidate");
    assert_eq!(save.effect_class, EffectClass::ReversibleEdit);
    assert_eq!(
        save.precondition,
        interaction_contracts::desktop::decision::Precondition::Executable
    );
    assert!(
        save.binding
            .as_ref()
            .is_some_and(|binding| binding.as_str().starts_with("owned-text/"))
    );
    assert!(save.parameters.is_empty());

    let (mut unreadable, _) = owned_save_root(expected, true);
    unreadable.mode = RunMode::Observe;
    admit(&mut unreadable, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    assert!(unreadable.current_basis().unwrap().owned_document.is_none());
    assert!(
        current_owned_candidates(&unreadable)
            .iter()
            .all(|candidate| candidate.skill != "ui.save_document")
    );

    let pack = AppPack::registered_native("com.apple.TextEdit").unwrap();
    let mut unregistered_config = EngineConfig::new(TaskConfig::new(vec![pack]));
    unregistered_config.allow_owned_document_save = true;
    let unregistered = Engine::new(unregistered_config, Storage::open_in_memory().unwrap());
    assert!(!unregistered.cfg.allow_owned_document_save);
    assert!(
        !unregistered
            .cfg
            .tasks
            .registered_skills
            .iter()
            .any(|skill| skill == "ui.save_document")
    );
}

#[test]
fn owned_save_receipt_records_only_the_exact_input_span_and_waits_for_file_evidence() {
    let expected = "private saved value with unicode π\u{0001}";
    let (mut engine, _) = owned_save_root(expected, true);
    let source = with_value_digest(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005, expected);
    let (submit, corr, _) =
        submit_owned_action(&mut engine, source, 1_050, OwnedDocumentActionKind::Save);
    let attempt_id = submit.intent.attempt_id().unwrap().to_owned();
    let intent_id = submit.intent.intent_id().to_owned();
    let root_id = engine
        .run
        .as_ref()
        .unwrap()
        .goal
        .root_task_id
        .as_str()
        .to_string();
    let start_ms = submit.intent.latest_start_ms();
    let receipt = save_receipt(&submit, expected, ReceiptStatus::Completed);
    let end_ms = receipt.input_end_ms.unwrap();
    let receipt_at = receipt.at_ms;
    let outputs = engine.handle(
        receipt_at,
        Input::Device {
            corr,
            reply: DeviceReply::Receipt(Some(Box::new(receipt))),
        },
    );

    assert_eq!(submit_count(&outputs), 0);
    assert_ne!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
    assert!(engine.run.as_ref().unwrap().effect_verification_only);
    assert!(saved_effects(&engine).iter().all(|result| {
        result.predicate != "document.saved@1"
            && !(result.confirmation_scope == ConfirmationScope::CurrentUi
                && result.status == EffectStatus::Satisfied)
    }));

    let attempt_state: String = engine
        .storage
        .read()
        .query_row(
            "SELECT state FROM attempts WHERE attempt_id = ?1",
            [&attempt_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(attempt_state, "settled");
    let detail: String = engine
        .storage
        .read()
        .query_row(
            "SELECT detail FROM event_log WHERE kind = 'owned_document_save_input_completed' ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(detail["root_task_id"], root_id);
    assert_eq!(detail["attempt_id"], attempt_id);
    assert_eq!(detail["intent_id"], intent_id);
    assert_eq!(detail["input_span"]["from_ms"], start_ms);
    assert_eq!(detail["input_span"]["to_ms"], end_ms);
    assert!(!detail.to_string().contains(expected));
    assert!(!detail.to_string().contains("document.txt"));
}

#[test]
fn verified_owned_set_value_can_advance_once_to_the_planned_save_attempt() {
    run_owned_save_revisit(None);
}

#[test]
fn owned_revisit_expiry_waiting_for_source_cannot_complete_or_submit_input() {
    run_owned_save_revisit(Some(false));
}

#[test]
fn owned_revisit_root_deadline_keeps_expired_state_and_stop_semantics() {
    run_owned_save_revisit(Some(true));
}

fn run_owned_save_revisit(expiry: Option<bool>) {
    let expected = "fill then save π";
    let (mut engine, _) = owned_fill_save_root(expected, true);
    let (setter, setter_corr, mut outputs) = submit_owned_text(
        &mut engine,
        with_value_digest(snapshot(1, 1_000, NAMESPACE_A, 3), 1_005, expected),
        1_050,
    );
    let setter_receipt = setter_receipt(&setter, expected, ReceiptStatus::Completed);
    let setter_end = setter_receipt.input_end_ms.unwrap();
    let setter_receipt_at = setter_receipt.at_ms;
    outputs.extend(engine.handle(
        setter_receipt_at,
        Input::Device {
            corr: setter_corr,
            reply: DeviceReply::Receipt(Some(Box::new(setter_receipt))),
        },
    ));
    let original = original_receiver(&setter);

    let post_input_start = setter_receipt_at + 20;
    let post_input = readback_after_input(
        &original,
        2,
        post_input_start,
        NAMESPACE_A,
        3,
        Some(post_input_start + 5),
        expected,
    );
    assert!(engine.state.refresh_pending());
    let refresh_started = accept_pending_source_refresh(&mut engine, &outputs, post_input_start);
    assert!(
        !engine.state.refresh_pending(),
        "the Source refresh generation was accepted"
    );
    outputs.extend(refresh_started);
    let mut save_outputs = admit(&mut engine, post_input, post_input_start + 10);
    let dependency_replies =
        acknowledge_dependency_updates(&mut engine, &save_outputs, post_input_start + 11);
    save_outputs.extend(dependency_replies);
    save_outputs.extend(engine.handle(post_input_start + 111, Input::Tick));
    outputs.extend(save_outputs.iter().cloned());

    assert_eq!(engine.next_subgoal().unwrap().skill, "ui.save_document");
    assert!(!engine.run.as_ref().unwrap().effect_verification_only);
    assert!(saved_effects(&engine).iter().any(|result| {
        result.predicate == "text.value@1"
            && result.confirmation_scope == ConfirmationScope::CurrentUi
            && result.status == EffectStatus::Satisfied
            && result
                .input_span
                .is_some_and(|span| span.to_ms == setter_end)
    }));

    let (observe_corr, observation) = find_device_command(&save_outputs, |command| {
        matches!(command, DeviceCommand::Observe(_))
    })
    .expect("the verified setter advances to one Save observation");
    let DeviceCommand::Observe(observation) = observation else {
        unreachable!()
    };
    let observation = observation.native().expect("owned Save observe is native");
    assert_eq!(
        observation.purpose,
        interaction_contracts::desktop::device::ObservationPurpose::OwnedDocumentSave
    );
    let observe_reply = engine.handle(
        post_input_start + 112,
        Input::Device {
            corr: observe_corr,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    );
    save_outputs.extend(observe_reply.iter().cloned());
    outputs.extend(observe_reply);
    let (grant_corr, _) = find_device_command(&save_outputs, |command| {
        matches!(command, DeviceCommand::GrantPermit(_))
    })
    .expect("the Save observation uses the normal permit stage");
    let grant_reply = engine.handle(
        post_input_start + 113,
        Input::Device {
            corr: grant_corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted {
                permit: PermitRef {
                    id: "owned-save-permit".into(),
                    generation: 1,
                },
            }),
        },
    );
    save_outputs.extend(grant_reply.iter().cloned());
    outputs.extend(grant_reply);
    let (save_corr, command) = find_device_command(&save_outputs, |command| {
        matches!(command, DeviceCommand::Submit(_))
            && matches!(
                command,
                DeviceCommand::Submit(submit)
                    if matches!(
                        submit.intent.native().and_then(|intent| intent.steps.first()).map(|step| &step.primitive),
                        Some(Primitive::AxAction {
                            action: AxActionKind::SaveOwnedDocument { .. }
                        })
                    )
            )
    })
    .expect("the second one-shot action is exactly SaveOwnedDocument");
    let DeviceCommand::Submit(save_submit) = command else {
        unreachable!()
    };
    let native = native_action_intent(&save_submit.intent);
    assert_eq!(native.steps.len(), 1);
    assert_eq!(native.steps[0].offset_ms, 0);
    assert_ne!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
    assert_eq!(submit_count(&outputs), 2);

    // A successful setter and Save receipt are only the first half of the
    // retained-root flow. The new process must be read-only, Main must attest
    // the stable saved file lineage, and a new private Source proof must
    // independently match the frozen value before UiRevisit completes.
    let save_attempt = interaction_contracts::desktop::ids::AttemptId::new(
        save_submit.intent.attempt_id().unwrap().to_owned(),
    )
    .unwrap();
    let save_intent = save_submit.intent.intent_id().to_owned();
    let save_receipt = save_receipt(&save_submit, expected, ReceiptStatus::Completed);
    let save_completed_at = save_receipt.at_ms;
    outputs.extend(engine.handle(
        save_completed_at,
        Input::Device {
            corr: save_corr,
            reply: DeviceReply::Receipt(Some(Box::new(save_receipt))),
        },
    ));
    assert_ne!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
    assert_eq!(
        engine
            .storage()
            .read()
            .query_row(
                "SELECT COUNT(*) FROM owned_save_evidence WHERE attempt_id = ?1",
                [&save_attempt.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );

    let begin_at = save_completed_at + 2;
    // The real Device acknowledges lease/revocation control traffic before
    // Main prepares the lifecycle. Do not manufacture an empty pending book.
    let control_acks: Vec<_> = outputs
        .iter()
        .filter_map(|output| match output {
            Output::Device { corr, command }
                if matches!(
                    command.as_ref(),
                    DeviceCommand::Revoke(_) | DeviceCommand::RenewLease(_)
                ) =>
            {
                Some(*corr)
            }
            _ => None,
        })
        .collect();
    for corr in control_acks {
        outputs.extend(engine.handle(
            begin_at,
            Input::Device {
                corr,
                reply: DeviceReply::Ack(CommandAck::ok()),
            },
        ));
    }
    assert!(engine.state.refresh_pending());
    assert!(engine.owned_document_binding().is_none());
    let challenge = engine.begin_owned_revisit(begin_at).expect(
        "settled Save begins the read-only lifecycle while its old Source refresh is pending",
    );
    assert_eq!(challenge.save_attempt_id, save_attempt);
    assert_eq!(challenge.save_intent_id, save_intent);
    assert_eq!(
        challenge.old_device_session_epoch + 1,
        challenge.expected_device_session_epoch
    );
    assert!(engine.run.as_ref().unwrap().effect_verification_only);

    engine.handle(
        begin_at + 1,
        Input::DeviceLost {
            reason: "planned owned-document peer restart".into(),
        },
    );

    let new_session = challenge.expected_device_session_epoch;
    let new_clock = format!("macos/{new_session}/monotonic");
    let mut manifest = device_view(challenge.old_device_session_epoch)
        .manifest()
        .unwrap()
        .clone();
    manifest.identity.session_epoch = new_session;
    manifest.identity.surface.epoch = 2;
    manifest.identity.clock_domain = new_clock.clone();
    manifest.backend = BackendKind::Macos;
    manifest.input = CapabilityState::Unavailable;
    manifest.max_uninterruptible_ms = 0;
    engine.handle(
        begin_at + 2,
        Input::DeviceNotice(DeviceNotice::Manifest(Box::new(manifest))),
    );

    let mut reopened_registration = registration(PID + 1, START_SECONDS + 1);
    reopened_registration.registration_id = "00000000-0000-4000-8000-000000000002".into();
    reopened_registration.document_sha256 = format!("{:x}", Sha256::digest(expected.as_bytes()));
    reopened_registration.document_ino = 22;
    reopened_registration.issued_unix_ms = 1_700_000_000_001;
    let reopen_boundary = begin_at + 3;
    let file_version = OwnedRevisitFileVersion {
        dev: reopened_registration.document_dev,
        ino: reopened_registration.document_ino,
        size: expected.len() as u32,
        sha256: reopened_registration.document_sha256.clone(),
        mtime_ns: "1700000000000000000".into(),
        ctime_ns: "1700000000000000001".into(),
    };
    let completion = OwnedRevisitCompletion {
        version: 1,
        transition_id: challenge.transition_id.clone(),
        root_task_id: challenge.root_task_id.clone(),
        task_revision: challenge.task_revision,
        root_epoch: challenge.root_epoch,
        new_registration: reopened_registration.clone(),
        file_lineage: OwnedRevisitFileLineage {
            directory_dev: reopened_registration.directory_dev,
            directory_ino: reopened_registration.directory_ino,
            before_close: file_version.clone(),
            after_close: file_version.clone(),
            after_reopen: file_version,
        },
        new_device_session_epoch: new_session,
        reopen_boundary_ms: reopen_boundary,
        clock_domain: challenge.clock_domain.clone(),
    };
    assert_eq!(
        engine.finish_owned_revisit(completion, begin_at + 4),
        Ok(super::owned_revisit::OwnedRevisitStatus::AwaitingSourceProof)
    );

    let refresh_outputs = engine.handle(begin_at + 5, Input::Tick);
    let generation = refresh_outputs
        .iter()
        .find_map(|output| match output {
            Output::Source {
                generation,
                command: interaction_contracts::desktop::source::SourceCommand::RefreshNow { .. },
            } => Some(*generation),
            _ => None,
        })
        .expect("Finish requests a fresh read from the new Source session");
    engine.handle(
        begin_at + 6,
        Input::SourceRefreshStarted {
            generation,
            accepted: true,
        },
    );
    let delayed_read_at = begin_at + engine.cfg.refresh_timeout_ms + 10;
    let waiting_outputs = engine.handle(delayed_read_at, Input::Tick);
    assert!(
        waiting_outputs
            .iter()
            .all(|output| !matches!(output, Output::Source { .. }))
    );
    assert!(
        engine
            .owned_revisit
            .as_ref()
            .unwrap()
            .source_refresh_accepted
    );
    assert_eq!(
        engine
            .owned_revisit
            .as_ref()
            .unwrap()
            .source_refresh_generation,
        Some(generation)
    );

    if let Some(root_deadline) = expiry {
        let (at, state) = if root_deadline {
            (
                engine.tasks.active().unwrap().row.deadline_ms.unwrap(),
                interaction_contracts::desktop::task::RootState::Expired,
            )
        } else {
            (
                challenge.deadline_ms + 1,
                interaction_contracts::desktop::task::RootState::Unverified,
            )
        };
        let expired_outputs = engine.handle(at, Input::Tick);
        assert_eq!(engine.view().map(|view| view.state), Some(state));
        if root_deadline {
            assert!(expired_outputs.iter().any(|output| matches!(output,
                Output::Device { command, .. } if matches!(command.as_ref(), DeviceCommand::Stop(_))
            )));
            assert!(engine.view().unwrap().root_epoch > challenge.root_epoch);
        }
        assert_eq!(submit_count(&expired_outputs), 0);
        assert!(engine.owned_revisit_transition_id().is_none());
        assert!(!saved_effects(&engine).iter().any(|result| {
            result.confirmation_scope == ConfirmationScope::UiRevisit
                && result.status == EffectStatus::Satisfied
        }));
        return;
    }

    let read_start = delayed_read_at + 10;
    let mut reopened = with_value_digest(
        snapshot(1, read_start, NAMESPACE_B, 1),
        read_start + 5,
        expected,
    );
    reopened.snapshot_id = RequestId::new("owned-reopened-read-1").unwrap();
    reopened.session_epoch = new_session;
    reopened.surface = SurfaceRef {
        id: "main".into(),
        epoch: 2,
    };
    reopened.clock_domain = new_clock.clone();
    reopened.app.pid = reopened_registration.pid;
    let reopened_evidence = reopened.owned_document_evidence.as_deref_mut().unwrap();
    reopened_evidence.registration_id = reopened_registration.registration_id.clone();
    reopened_evidence.process = reopened_registration.process_identity();
    reopened_evidence.provenance.snapshot_id = reopened.snapshot_id.clone();
    reopened_evidence.provenance.clock_domain = new_clock;
    let final_outputs = admit(&mut engine, reopened, read_start + 10);

    assert_eq!(
        engine.view().map(|view| view.state),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "only the admitted post-reopen typed proof advances the retained root"
    );
    assert!(engine.state().latest().unwrap().snapshot.objects.is_empty());
    assert!(!engine.state().latest().unwrap().snapshot.complete);
    assert!(
        engine
            .state()
            .latest()
            .unwrap()
            .snapshot
            .global_focus
            .known_window_for_app(&engine.state().latest().unwrap().snapshot.app)
            .is_none()
    );
    assert_eq!(submit_count(&final_outputs), 0);
    assert!(final_outputs.iter().all(|output| !matches!(
        output,
        Output::Device {
            command,
            ..
        } if matches!(command.as_ref(), DeviceCommand::Submit(_) | DeviceCommand::GrantPermit(_))
    )));
    let results = saved_effects(&engine);
    assert!(results.iter().any(|result| {
        result.predicate == "text.value@1"
            && result.confirmation_scope == ConfirmationScope::CurrentUi
            && result.status == EffectStatus::Satisfied
    }));
    assert!(results.iter().any(|result| {
        result.predicate == "goal.required@1"
            && result.confirmation_scope == ConfirmationScope::UiRevisit
            && result.status == EffectStatus::Satisfied
            && result.attempt_id.as_ref() == Some(&save_attempt)
            && result.intent_id.as_deref() == Some(save_intent.as_str())
    }));
}

#[test]
fn owned_revisit_rejects_a_goal_with_any_extra_or_different_predicate() {
    let (mut engine, _) = owned_fill_save_root("frozen text", true);
    let run = engine.run.as_mut().expect("frozen owned root");
    assert!(owned_revisit_goal_supported(&run.goal));
    let extra = interaction_contracts::desktop::task::Predicate::TargetBound {
        target: run.goal.target.semantic_target.clone(),
    };
    run.goal.required = interaction_contracts::desktop::task::Predicate::And {
        of: vec![run.goal.required.clone(), extra],
    };
    assert!(!owned_revisit_goal_supported(&run.goal));
}

#[test]
fn owned_current_ui_diagnostic_needs_a_new_same_scope_value_digest() {
    let registration = registration(PID, START_SECONDS);
    let mut engine = engine(Some(registration.clone()));
    admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    let original = binding_proof(&engine, "owned-text-binding");
    let input_span = TimeSpan {
        from_ms: 1_055,
        to_ms: 1_060,
    };
    let expected = "owned text π";

    let mut current_snapshot =
        with_value_digest(snapshot(2, 7_000, NAMESPACE_A, 3), 7_010, expected);
    reuse_receiver_identity(&mut current_snapshot, &original.receiver);
    admit(&mut engine, current_snapshot, 7_050);
    let current = binding_proof(&engine, "owned-text-binding");

    // The frozen pre-input proof may now be older than its TTL; its validity
    // at the input span matters, while the post-input proof must be current.
    assert_eq!(
        owned_text_value_diagnostic(
            &original,
            &current,
            &registration,
            expected,
            input_span,
            7_050,
            2_000,
        ),
        TriState::True
    );

    let mut mismatch = current.clone();
    mismatch.receiver.value_readback.as_mut().unwrap().sha256 = "f".repeat(64);
    assert_eq!(
        owned_text_value_diagnostic(
            &original,
            &mismatch,
            &registration,
            expected,
            input_span,
            7_050,
            2_000,
        ),
        TriState::False
    );

    let mut unreadable = current.clone();
    unreadable.receiver.value_readback = None;
    assert_eq!(
        owned_text_value_diagnostic(
            &original,
            &unreadable,
            &registration,
            expected,
            input_span,
            7_050,
            2_000,
        ),
        TriState::Unknown
    );

    let mut replaced_scope = current.clone();
    replaced_scope
        .receiver
        .provenance
        .identity_boundary
        .namespace = NAMESPACE_B.into();
    assert_eq!(
        owned_text_value_diagnostic(
            &original,
            &replaced_scope,
            &registration,
            expected,
            input_span,
            7_050,
            2_000,
        ),
        TriState::Unknown
    );
}

#[test]
fn absent_or_forged_registration_and_process_identity_never_bind() {
    let mut no_registration = engine(None);
    admit(
        &mut no_registration,
        snapshot(1, 1_000, NAMESPACE_A, 3),
        1_050,
    );
    assert!(no_registration.owned_document_binding().is_none());

    let mut invalid_registration = registration(PID, START_SECONDS);
    invalid_registration.registration_id = "not-a-canonical-registration-id".into();
    let mut untrusted = engine(Some(invalid_registration));
    admit(&mut untrusted, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    assert!(untrusted.owned_document_binding().is_none());

    let mut forged_registration = engine(Some(registration(PID, START_SECONDS)));
    let mut forged = snapshot(1, 1_000, NAMESPACE_A, 3);
    forged
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .registration_id = "00000000-0000-4000-8000-000000000099".into();
    admit(&mut forged_registration, forged, 1_050);
    assert!(forged_registration.owned_document_binding().is_none());

    let mut pid_mismatch = engine(Some(registration(PID, START_SECONDS)));
    let mut pid_snapshot = snapshot(1, 1_000, NAMESPACE_A, 3);
    // This private receiver fixture deliberately reports no global focus and
    // contains no generic windows. Changing its registered process identity
    // must still fail against the private registration.
    pid_snapshot.app.pid = PID + 1;
    pid_snapshot
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .process
        .pid = PID + 1;
    admit(&mut pid_mismatch, pid_snapshot, 1_050);
    assert!(pid_mismatch.owned_document_binding().is_none());

    let mut start_mismatch = engine(Some(registration(PID, START_SECONDS)));
    let mut start_snapshot = snapshot(1, 1_000, NAMESPACE_A, 3);
    start_snapshot
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .process
        .start_seconds += 1;
    admit(&mut start_mismatch, start_snapshot, 1_050);
    assert!(start_mismatch.owned_document_binding().is_none());
}

#[test]
fn expired_future_and_replayed_proofs_are_cleared() {
    let mut expired = engine(Some(registration(PID, START_SECONDS)));
    let mut stale = snapshot(1, 1_000, NAMESPACE_A, 3);
    stale
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .provenance
        .valid_until_ms = 1_050;
    admit(&mut expired, stale, 1_100);
    assert!(expired.owned_document_binding().is_none());

    let mut future = engine(Some(registration(PID, START_SECONDS)));
    let mut future_proof = snapshot(1, 1_000, NAMESPACE_A, 3);
    future_proof
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .marked_range_after
        .observed_ms = 1_200;
    admit(&mut future, future_proof, 1_100);
    assert!(future.owned_document_binding().is_none());

    let mut replayed = engine(Some(registration(PID, START_SECONDS)));
    admit(&mut replayed, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    assert!(replayed.owned_document_binding().is_some());
    let mut next = snapshot(2, 1_060, NAMESPACE_A, 3);
    next.owned_document_evidence = replayed
        .state()
        .latest()
        .unwrap()
        .snapshot
        .owned_document_evidence
        .clone();
    admit(&mut replayed, next, 1_100);
    assert!(replayed.owned_document_binding().is_none());
}

#[test]
fn rejected_outer_snapshot_and_document_binding_records_clear_the_old_proof() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    assert!(engine.owned_document_binding().is_some());

    let mut wrong_clock = snapshot(2, 1_060, NAMESPACE_A, 3);
    wrong_clock.clock_domain = "native/other-clock".into();
    admit(&mut engine, wrong_clock, 1_100);
    assert!(engine.owned_document_binding().is_none());

    admit(&mut engine, snapshot(3, 1_110, NAMESPACE_A, 3), 1_120);
    assert!(engine.owned_document_binding().is_some());
    let mut invalid_document_binding = snapshot(4, 1_130, NAMESPACE_A, 3);
    invalid_document_binding
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .document_binding
        .after
        .registered_path_matches = false;
    admit(&mut engine, invalid_document_binding, 1_140);
    assert!(engine.owned_document_binding().is_none());

    let mut without_sidecar = snapshot(5, 1_150, NAMESPACE_A, 3);
    without_sidecar.owned_document_evidence = None;
    admit(&mut engine, without_sidecar, 1_160);
    assert!(engine.owned_document_binding().is_none());
}

#[test]
fn device_session_surface_and_clock_mismatches_reject_the_private_read() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    let identity = DeviceIdentity {
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: CLOCK.into(),
        dependencies: BTreeMap::new(),
    };
    engine.state.set_identity(&identity, false);

    let mut wrong_session = snapshot(1, 1_000, NAMESPACE_A, 3);
    wrong_session.session_epoch = 2;
    admit(&mut engine, wrong_session, 1_050);
    assert!(engine.owned_document_binding().is_none());

    let mut wrong_surface = snapshot(2, 1_060, NAMESPACE_A, 3);
    wrong_surface.surface.epoch = 2;
    admit(&mut engine, wrong_surface, 1_100);
    assert!(engine.owned_document_binding().is_none());

    let mut wrong_clock = snapshot(3, 1_110, NAMESPACE_A, 3);
    wrong_clock.clock_domain = "native/other-clock".into();
    wrong_clock
        .owned_document_evidence
        .as_mut()
        .unwrap()
        .provenance
        .clock_domain = wrong_clock.clock_domain.clone();
    admit(&mut engine, wrong_clock, 1_120);
    assert!(engine.owned_document_binding().is_none());
}

#[test]
fn a_trusted_session_reset_does_not_make_the_previous_proof_replayable() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    assert!(engine.owned_document_binding().is_some());

    engine.state.set_session(2);
    let mut replayed = snapshot(1, 1_000, NAMESPACE_A, 3);
    replayed.session_epoch = 2;
    admit(&mut engine, replayed, 1_100);
    assert!(engine.owned_document_binding().is_none());

    let mut new_read = snapshot(2, 1_110, NAMESPACE_A, 3);
    new_read.session_epoch = 2;
    admit(&mut engine, new_read, 1_120);
    assert!(engine.owned_document_binding().is_some());
}

#[test]
fn private_namespace_or_generation_rollback_requires_a_later_read() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    assert!(engine.owned_document_binding().is_some());

    admit(&mut engine, snapshot(2, 1_060, NAMESPACE_A, 2), 1_100);
    assert!(engine.owned_document_binding().is_none());
    admit(&mut engine, snapshot(3, 1_070, NAMESPACE_A, 2), 1_120);
    assert!(engine.owned_document_binding().is_none());
    admit(&mut engine, snapshot(4, 1_080, NAMESPACE_A, 3), 1_130);
    assert!(engine.owned_document_binding().is_some());

    admit(&mut engine, snapshot(5, 1_090, NAMESPACE_B, 1), 1_140);
    assert!(engine.owned_document_binding().is_none());
    admit(&mut engine, snapshot(6, 1_100, NAMESPACE_B, 1), 1_150);
    assert!(engine.owned_document_binding().is_some());

    admit(&mut engine, snapshot(7, 1_110, NAMESPACE_A, 4), 1_160);
    assert!(engine.owned_document_binding().is_none());
    admit(&mut engine, snapshot(8, 1_120, NAMESPACE_B, 1), 1_170);
    assert!(engine.owned_document_binding().is_some());
}

#[test]
fn incomplete_generic_health_keeps_private_proof_but_unavailable_refresh_and_stop_clear_it() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    engine.handle(
        1_060,
        Input::SourceHealth(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Degraded,
            reason: Some("generic traversal incomplete".into()),
            at_ms: 1_060,
            provenance: None,
        }),
    );
    assert!(engine.owned_document_binding().is_some());

    engine.handle(
        1_065,
        Input::SourceHealth(SourceHealth {
            source: SourceKind::AxObserver,
            state: SourceState::Unavailable,
            reason: Some("generic observer coverage is absent".into()),
            at_ms: 1_065,
            provenance: None,
        }),
    );
    assert!(engine.owned_document_binding().is_some());

    engine.handle(
        1_070,
        Input::SourceHealth(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Unavailable,
            reason: Some("Source disconnected".into()),
            at_ms: 1_070,
            provenance: None,
        }),
    );
    assert!(engine.owned_document_binding().is_none());

    engine.handle(
        1_080,
        Input::SourceHealth(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Available,
            reason: None,
            at_ms: 1_080,
            provenance: None,
        }),
    );
    admit(&mut engine, snapshot(2, 1_090, NAMESPACE_A, 3), 1_100);
    assert!(engine.owned_document_binding().is_some());
    let refresh_generation = engine.state.begin_refresh().unwrap();
    assert!(engine.owned_document_binding().is_none());

    let after_refresh = snapshot(3, 1_120, NAMESPACE_A, 3);
    engine.state.refresh_started(refresh_generation);
    admit(&mut engine, after_refresh, 1_130);
    assert!(engine.owned_document_binding().is_some());

    engine.handle(
        1_140,
        Input::Operator(OperatorCommand::StopTask { root_task_id: None }),
    );
    assert!(engine.owned_document_binding().is_none());
}

#[test]
fn stale_age_and_device_identity_change_hide_the_private_binding() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    assert!(engine.owned_document_binding().is_some());

    let stale_snapshot = snapshot(2, 1_100, NAMESPACE_A, 3);
    admit(&mut engine, stale_snapshot, 4_000);
    assert!(engine.owned_document_binding().is_none());

    admit(&mut engine, snapshot(3, 4_010, NAMESPACE_A, 3), 4_020);
    assert!(engine.owned_document_binding().is_some());
    let identity = DeviceIdentity {
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 2,
        },
        clock_domain: CLOCK.into(),
        dependencies: BTreeMap::new(),
    };
    assert!(engine.state.set_identity(&identity, false));
    assert!(engine.owned_document_binding().is_none());
}

fn submit_count(outputs: &[Output]) -> usize {
    outputs
        .iter()
        .filter(|output| {
            matches!(
                output,
                Output::Device {
                    command,
                    ..
                } if matches!(command.as_ref(), DeviceCommand::Submit(_))
            )
        })
        .count()
}

fn saved_effects(
    engine: &Engine,
) -> Vec<interaction_contracts::desktop::verification::EffectResult> {
    let root = engine
        .view()
        .expect("root remains available for audit")
        .root_task_id;
    crate::verification::VerificationStore::history(engine.storage.read(), root.as_str()).unwrap()
}

fn owned_engine_trace(engine: &Engine, outputs: &[Output]) -> String {
    let latest = engine.state.latest().map(|env| {
        (
            env.snapshot.snapshot_id.as_str(),
            env.snapshot.source_sequence,
            env.snapshot.observed_ms,
            env.snapshot.complete,
            env.snapshot
                .global_focus
                .known_window_for_app(&env.snapshot.app)
                .map(str::to_owned),
            env.snapshot.identity_boundary.as_ref(),
            env.snapshot.owned_document_evidence.is_some(),
        )
    });
    let attempt = engine.run.as_ref().and_then(|run| {
        run.attempt.as_ref().map(|attempt| {
            (
                attempt.input_reached,
                attempt.mechanical_unknown,
                attempt.text_current_ui_pending,
                attempt.dispatch.as_ref().map(|dispatch| dispatch.stage),
            )
        })
    });
    let owned_proof = engine
        .run
        .as_ref()
        .and_then(|run| engine.owned_document_proof_for(&run.goal.target.semantic_target));
    format!(
        "root={:?} gate_closed={} private_binding={:?} owned_attempt=(active:{},usable:{}) owned_current={owned_proof:?} latest={latest:?} attempt={attempt:?} run={:?} pending_decision={:?} device_backend={:?} dependencies={:?} waits={:?} timeline={:?} outputs={outputs:#?}",
        engine.root_state(),
        engine.source_gate_closed,
        engine.owned_document_binding().map(|binding| (
            binding.snapshot_id().as_str(),
            binding.source_sequence(),
            binding.window_id()
        )),
        engine.owned_document_attempt_active(),
        engine.owned_document_attempt_evidence_usable(),
        engine
            .run
            .as_ref()
            .map(|run| (run.wait, run.effect_verification_only, run.stalls)),
        engine.decisions.pending().map(|pending| (
            pending.candidates.len(),
            pending.basis.owned_document.is_some()
        )),
        engine.device.manifest().map(|manifest| manifest.backend),
        engine.device.dependencies(),
        (
            engine
                .device
                .is_waiting(|expect| matches!(expect, crate::execution::Expect::Observe { .. })),
            engine
                .device
                .is_waiting(|expect| matches!(expect, crate::execution::Expect::Grant { .. })),
            engine
                .device
                .is_waiting(|expect| matches!(expect, crate::execution::Expect::Submit { .. })),
        ),
        engine
            .timeline()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
    )
}

#[test]
fn native_generic_missing_boundary_keeps_same_private_scope_observation_alive() {
    let (mut engine, _) = owned_text_root("native owned text");
    require_native_frames(&mut engine);
    assert!(engine.state.requires_frames());

    let first = macos_snapshot(snapshot(1, 1_000, NAMESPACE_A, 3));
    let original = first
        .owned_document_evidence
        .as_deref()
        .expect("private scope fixture")
        .clone();
    let mut outputs = admit(&mut engine, first, 1_050);
    let dependency_replies = acknowledge_dependency_updates(&mut engine, &outputs, 1_051);
    outputs.extend(dependency_replies);

    // Native Source publishes after a quiet interval. A second same-receiver
    // read still carries no generic identity boundary and must keep the owned
    // attempt alive while the engine waits for that interval.
    let mut same_scope = macos_snapshot(snapshot(2, 1_060, NAMESPACE_A, 3));
    reuse_receiver_identity(&mut same_scope, &original);
    let next_outputs = admit(&mut engine, same_scope, 1_160);
    let dependency_replies = acknowledge_dependency_updates(&mut engine, &next_outputs, 1_161);
    outputs.extend(next_outputs);
    outputs.extend(dependency_replies);
    outputs.extend(engine.handle(1_261, Input::Tick));

    let (observe_corr, observe) = find_device_command(&outputs, |command| {
        matches!(command, DeviceCommand::Observe(_))
    })
    .unwrap_or_else(|| {
        panic!(
            "native owned root starts its private observation: {}",
            owned_engine_trace(&engine, &outputs)
        )
    });
    assert!(matches!(observe, DeviceCommand::Observe(_)));

    let mut same_scope = macos_snapshot(snapshot(3, 1_270, NAMESPACE_A, 3));
    reuse_receiver_identity(&mut same_scope, &original);
    let next_outputs = admit(&mut engine, same_scope, 1_280);
    let dependency_replies = acknowledge_dependency_updates(&mut engine, &next_outputs, 1_281);
    assert!(
        next_outputs
            .iter()
            .all(|output| !matches!(output, Output::CancelDecision(_)))
    );
    assert!(
        engine
            .device
            .is_waiting(|expect| matches!(expect, crate::execution::Expect::Observe { .. }))
    );
    assert!(engine.owned_document_binding().is_some());
    assert!(engine.source_gate_closed);
    assert!(!engine.state.usable(1_280, 2_000));
    let current = &engine.state.latest().unwrap().snapshot;
    assert!(current.identity_boundary.is_none());
    assert!(current.objects.is_empty());
    assert_eq!(current.observer_coverage.state, SourceState::Unavailable);
    assert!(
        current
            .global_focus
            .known_window_for_app(&current.app)
            .is_none()
    );
    outputs.extend(next_outputs);
    outputs.extend(dependency_replies);

    outputs.extend(engine.handle(
        1_382,
        Input::Device {
            corr: observe_corr,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
    let (grant_corr, grant) = find_device_command(&outputs, |command| {
        matches!(command, DeviceCommand::GrantPermit(_))
    })
    .expect("the same private scope permits the original observation to continue");
    assert!(matches!(grant, DeviceCommand::GrantPermit(_)));
    outputs.extend(engine.handle(
        1_383,
        Input::Device {
            corr: grant_corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted {
                permit: PermitRef {
                    id: "native-owned-permit".into(),
                    generation: 1,
                },
            }),
        },
    ));

    assert_eq!(submit_count(&outputs), 1);
    let (_, submitted) = find_device_command(&outputs, |command| {
        matches!(command, DeviceCommand::Submit(_))
    })
    .expect("one typed owned setter reaches the existing Submit boundary");
    let DeviceCommand::Submit(submitted) = submitted else {
        unreachable!()
    };
    let native = native_action_intent(&submitted.intent);
    assert_eq!(native.steps.len(), 1);
    assert!(native.binding_proof.is_none());
    assert!(native.owned_document_proof.is_some());
    assert!(matches!(
        &native.steps[0].primitive,
        Primitive::AxAction {
            action: AxActionKind::SetValue { .. }
        }
    ));
    assert_ne!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn native_generic_missing_boundary_does_not_preserve_a_replaced_receiver() {
    let (mut engine, _) = owned_text_root("replacement stays guarded");
    require_native_frames(&mut engine);
    let mut outputs = admit(
        &mut engine,
        macos_snapshot(snapshot(1, 1_000, NAMESPACE_A, 3)),
        1_050,
    );
    let dependency_replies = acknowledge_dependency_updates(&mut engine, &outputs, 1_051);
    outputs.extend(dependency_replies);
    outputs.extend(engine.handle(1_151, Input::Tick));
    let (observe_corr, _) = find_device_command(&outputs, |command| {
        matches!(command, DeviceCommand::Observe(_))
    })
    .unwrap_or_else(|| {
        panic!(
            "owned observation is pending: {}",
            owned_engine_trace(&engine, &outputs)
        )
    });

    // A different receiver under the same semantic target invalidates private
    // continuity even though generic snapshots remain incomplete and boundary-free.
    let replacement = macos_snapshot(snapshot(2, 1_160, NAMESPACE_A, 3));
    let replacement_outputs = admit(&mut engine, replacement, 1_161);
    let dependency_replies =
        acknowledge_dependency_updates(&mut engine, &replacement_outputs, 1_162);
    outputs.extend(replacement_outputs);
    outputs.extend(dependency_replies);
    assert!(
        !engine
            .device
            .is_waiting(|expect| matches!(expect, crate::execution::Expect::Observe { .. }))
    );
    outputs.extend(engine.handle(
        1_262,
        Input::Device {
            corr: observe_corr,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
    assert_eq!(submit_count(&outputs), 0);
    assert_ne!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn owned_candidate_runs_through_admission_receipt_and_later_digest_but_not_root_revisit() {
    let expected = "requested π text";
    let (mut engine, target) = owned_text_root(expected);
    require_native_frames(&mut engine);
    let (submit, submit_corr, mut outputs) = submit_owned_text(
        &mut engine,
        macos_snapshot(snapshot(1, 1_000, NAMESPACE_A, 3)),
        1_050,
    );
    assert_eq!(target.as_str(), "document/text");

    let receipt = setter_receipt(&submit, expected, ReceiptStatus::Completed);
    let receipt_ms = receipt.at_ms;
    outputs.extend(engine.handle(
        receipt_ms,
        Input::Device {
            corr: submit_corr,
            reply: DeviceReply::Receipt(Some(Box::new(receipt.clone()))),
        },
    ));
    let original = original_receiver(&submit);
    assert!(outputs.iter().all(|output| !matches!(
        output,
        Output::Source {
            command: interaction_contracts::desktop::source::SourceCommand::RefreshNow { .. },
            ..
        }
    )));
    assert!(saved_effects(&engine).iter().all(|result| {
        result.confirmation_scope != ConfirmationScope::CurrentUi
            || result.status != EffectStatus::Satisfied
    }));

    // A post-input receiver read with no bounded digest is Unknown. The same
    // scope may be read again; its later start and digest can still verify.
    let first_start = receipt_ms + 20;
    let no_digest = macos_snapshot(readback_after_input(
        &original,
        2,
        first_start,
        NAMESPACE_A,
        3,
        None,
        expected,
    ));
    let first_outputs = admit(&mut engine, no_digest, first_start + 10);
    let dependency_replies =
        acknowledge_dependency_updates(&mut engine, &first_outputs, first_start + 11);
    outputs.extend(first_outputs);
    outputs.extend(dependency_replies);
    outputs.extend(engine.handle(first_start + 111, Input::Tick));
    assert_eq!(submit_count(&outputs), 1);
    assert!(outputs.iter().all(|output| !matches!(
        output,
        Output::Source {
            command: interaction_contracts::desktop::source::SourceCommand::RefreshNow { .. },
            ..
        }
    )));
    assert!(saved_effects(&engine).iter().all(|result| {
        result.confirmation_scope != ConfirmationScope::CurrentUi
            || result.status != EffectStatus::Satisfied
    }));

    let second_start = first_start + 200;
    let digest = macos_snapshot(readback_after_input(
        &original,
        3,
        second_start,
        NAMESPACE_A,
        3,
        Some(second_start + 5),
        expected,
    ));
    let second_outputs = admit(&mut engine, digest, second_start + 10);
    let dependency_replies =
        acknowledge_dependency_updates(&mut engine, &second_outputs, second_start + 11);
    outputs.extend(second_outputs);
    outputs.extend(dependency_replies);
    outputs.extend(engine.handle(second_start + 111, Input::Tick));

    let current_ui = saved_effects(&engine)
        .into_iter()
        .find(|result| {
            result.predicate == "text.value@1"
                && result.confirmation_scope == ConfirmationScope::CurrentUi
                && result.status == EffectStatus::Satisfied
        })
        .unwrap_or_else(|| {
            panic!(
                "the later readback records only the current UI attempt effect: {}",
                owned_engine_trace(&engine, &outputs)
            )
        });
    assert_eq!(current_ui.target.as_ref(), Some(&target));
    assert_eq!(
        current_ui.intent_id.as_deref(),
        Some(submit.intent.intent_id())
    );
    assert_eq!(
        current_ui.input_span,
        receipt.input_end_ms.map(|end| TimeSpan {
            from_ms: submit.intent.latest_start_ms(),
            to_ms: end,
        })
    );
    assert!(submit_count(&outputs) == 1);
    assert_ne!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
    assert_eq!(
        engine
            .run
            .as_ref()
            .expect("UiRevisit root remains active")
            .goal
            .confirmation_scope,
        ConfirmationScope::UiRevisit
    );
}

#[test]
fn owned_digest_mismatch_is_false_and_never_replays_the_completed_setter() {
    let expected = "frozen text";
    let (mut engine, _) = owned_text_root(expected);
    let (submit, submit_corr, mut outputs) =
        submit_owned_text(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    let receipt = setter_receipt(&submit, expected, ReceiptStatus::Completed);
    let receipt_ms = receipt.at_ms;
    outputs.extend(engine.handle(
        receipt_ms,
        Input::Device {
            corr: submit_corr,
            reply: DeviceReply::Receipt(Some(Box::new(receipt))),
        },
    ));
    let original = original_receiver(&submit);
    let start = receipt_ms + 20;
    let refresh_output = accept_pending_source_refresh(&mut engine, &outputs, start);
    outputs.extend(refresh_output);
    let mismatch = readback_after_input(
        &original,
        2,
        start,
        NAMESPACE_A,
        3,
        Some(start + 5),
        "different text",
    );
    outputs.extend(admit(&mut engine, mismatch, start + 10));

    assert!(saved_effects(&engine).iter().all(|result| {
        result.confirmation_scope != ConfirmationScope::CurrentUi
            || result.status != EffectStatus::Satisfied
    }));
    assert_eq!(submit_count(&outputs), 1);
    assert_ne!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn unknown_receipt_and_later_matching_digest_do_not_replay_or_complete_the_root() {
    let expected = "uncertain input";
    let (mut engine, _) = owned_text_root(expected);
    let (submit, submit_corr, mut outputs) =
        submit_owned_text(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    let receipt = setter_receipt(&submit, expected, ReceiptStatus::Unknown);
    let receipt_ms = receipt.at_ms;
    outputs.extend(engine.handle(
        receipt_ms,
        Input::Device {
            corr: submit_corr,
            reply: DeviceReply::Receipt(Some(Box::new(receipt))),
        },
    ));
    let original = original_receiver(&submit);
    let start = receipt_ms + 20;
    let refresh_output = accept_pending_source_refresh(&mut engine, &outputs, start);
    outputs.extend(refresh_output);
    let read = readback_after_input(
        &original,
        2,
        start,
        NAMESPACE_A,
        3,
        Some(start + 5),
        expected,
    );
    outputs.extend(admit(&mut engine, read, start + 10));

    assert!(saved_effects(&engine).iter().all(|result| {
        result.confirmation_scope != ConfirmationScope::CurrentUi
            || result.status != EffectStatus::Satisfied
    }));
    assert_eq!(submit_count(&outputs), 1);
    assert_ne!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn replacement_receiver_stales_pending_owned_evidence_and_stop_never_resubmits() {
    let (mut replaced, _) = owned_text_root("replace guarded");
    let mut outputs = admit(&mut replaced, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    let dependency_replies = acknowledge_dependency_updates(&mut replaced, &outputs, 1_051);
    outputs.extend(dependency_replies);
    let (observe_corr, _) = find_device_command(&outputs, |command| {
        matches!(command, DeviceCommand::Observe(_))
    })
    .expect("owned observation is sent first");
    // A new receiver under the same semantic target retires the pending proof.
    outputs.extend(admit(
        &mut replaced,
        snapshot(2, 1_100, NAMESPACE_A, 3),
        1_110,
    ));
    outputs.extend(replaced.handle(
        1_120,
        Input::Device {
            corr: observe_corr,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
    assert_eq!(submit_count(&outputs), 0);
    assert!(replaced.run.as_ref().unwrap().effect_verification_only);

    let (mut stale, _) = owned_text_root("stale guarded");
    let mut stale_outputs = admit(&mut stale, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    let dependency_replies = acknowledge_dependency_updates(&mut stale, &stale_outputs, 1_051);
    stale_outputs.extend(dependency_replies);
    let (stale_observe_corr, _) = find_device_command(&stale_outputs, |command| {
        matches!(command, DeviceCommand::Observe(_))
    })
    .expect("stale test starts with an owned observation");
    // The private proof's age gate is strict at max_age and stops the old
    // observation before it can become a permit or a setter.
    stale_outputs.extend(stale.handle(
        3_001,
        Input::Device {
            corr: stale_observe_corr,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
    assert_eq!(submit_count(&stale_outputs), 0);
    assert!(stale.run.as_ref().unwrap().effect_verification_only);

    let (mut stopped, _) = owned_text_root("stop guarded");
    let (submit, _, mut stop_outputs) =
        submit_owned_text(&mut stopped, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    assert_eq!(native_action_intent(&submit.intent).steps.len(), 1);
    let root = stopped.run.as_ref().unwrap().goal.root_task_id.clone();
    stop_outputs.extend(stopped.handle(
        1_060,
        Input::Operator(OperatorCommand::StopTask {
            root_task_id: Some(root),
        }),
    ));
    let later = snapshot(2, 1_100, NAMESPACE_A, 3);
    stop_outputs.extend(admit(&mut stopped, later, 1_110));
    assert_eq!(submit_count(&stop_outputs), 1);
    assert_ne!(
        stopped.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn decision_basis_allows_same_scope_source_reads_but_rejects_receiver_replacement() {
    let mut engine = engine(Some(registration(PID, START_SECONDS)));
    admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    let original = binding_proof(&engine, "owned-text-binding");
    let basis = DecisionBasis::capture_with_owned_document(
        engine.state.latest().unwrap(),
        &engine.bindings,
        1,
        1,
        1,
        Some(original.clone()),
    );

    let mut same_scope = snapshot(2, 1_100, NAMESPACE_A, 3);
    reuse_receiver_identity(
        &mut same_scope,
        engine.owned_document_binding().unwrap().evidence(),
    );
    admit(&mut engine, same_scope, 1_110);
    let refreshed = binding_proof(&engine, "owned-text-binding");
    assert!(original.same_instance(&refreshed));
    let current_basis = DecisionBasis::capture_with_owned_document(
        engine.state.latest().unwrap(),
        &engine.bindings,
        1,
        1,
        1,
        Some(refreshed),
    );
    assert_eq!(basis.broken_by(&current_basis), None);

    admit(&mut engine, snapshot(3, 1_200, NAMESPACE_A, 3), 1_210);
    let replacement = binding_proof(&engine, "owned-text-binding");
    let replacement_basis = DecisionBasis::capture_with_owned_document(
        engine.state.latest().unwrap(),
        &engine.bindings,
        1,
        1,
        1,
        Some(replacement),
    );
    assert_eq!(
        basis.broken_by(&replacement_basis),
        Some("the owned document receiver changed")
    );
}

#[test]
fn stop_while_owned_observation_is_pending_cannot_replay_the_setter() {
    let (mut engine, _) = owned_text_root("stop before submit");
    let mut outputs = admit(&mut engine, snapshot(1, 1_000, NAMESPACE_A, 3), 1_050);
    let dependency_replies = acknowledge_dependency_updates(&mut engine, &outputs, 1_051);
    outputs.extend(dependency_replies);
    assert!(
        find_device_command(&outputs, |command| {
            matches!(command, DeviceCommand::Observe(_))
        })
        .is_some()
    );
    let root = engine.run.as_ref().unwrap().goal.root_task_id.clone();
    outputs.extend(engine.handle(
        1_060,
        Input::Operator(OperatorCommand::StopTask {
            root_task_id: Some(root),
        }),
    ));
    outputs.extend(admit(
        &mut engine,
        snapshot(2, 1_100, NAMESPACE_A, 3),
        1_110,
    ));
    assert_eq!(submit_count(&outputs), 0);
    assert_ne!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

fn lifecycle_row_scope() -> RowTableScope {
    RowTableScope {
        scroll_path: vec![RowContainerScope {
            role: RowContainerRole::AxScrollArea,
            label: "Projects".into(),
        }],
        column_headers: vec!["Name".into(), "Owner".into()],
    }
}

fn request_lifecycle_row_scope(engine: &mut Engine) -> RowTableScope {
    let scope = lifecycle_row_scope();
    engine.run.as_mut().unwrap().goal.target.row = Some(RowTargetSpec {
        table_scope: scope.clone(),
        conditions: vec![RowColumnConditionSpec {
            column_name: "Name".into(),
            value: ExpectedValue::Text {
                value: "quarterly report".into(),
            },
        }],
    });
    // This fixture starts a new explicit scoped refresh before the root's
    // bootstrap Source callback arrives. Retire the prior wait marker so the
    // new business request is emitted; StateCore assigns it a fresh generation
    // and supersedes the earlier pending refresh.
    engine.run.as_mut().unwrap().wait = None;
    engine.now_ms = 101;
    engine
        .request_refresh(101, "observing a frozen row target")
        .unwrap();
    scope
}

fn refresh_scopes(outputs: &[Output]) -> Vec<Option<RowTableScope>> {
    outputs
        .iter()
        .filter_map(|output| match output {
            Output::Source {
                command: SourceCommand::RefreshNow { table_scope },
                ..
            } => Some(table_scope.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn pause_clears_the_source_scope_and_resume_refreshes_from_the_frozen_goal() {
    let (mut engine, _) = owned_text_root("select the quarterly report");
    let scope = request_lifecycle_row_scope(&mut engine);
    let scope_generation = engine
        .outputs
        .iter()
        .rev()
        .find_map(|output| match output {
            Output::Source {
                generation,
                command:
                    SourceCommand::RefreshNow {
                        table_scope: Some(found),
                    },
            } if found == &scope => Some(*generation),
            _ => None,
        })
        .expect("the frozen row target requests a scoped Source refresh");
    let refresh_outputs = engine.handle(
        101,
        Input::SourceRefreshStarted {
            generation: scope_generation,
            accepted: true,
        },
    );
    assert_eq!(refresh_scopes(&refresh_outputs), vec![Some(scope.clone())]);

    // This unit exercises scoped-source cleanup, not row business admission:
    // its text-only receiver fixture cannot prove a table/row after the frozen
    // Goal was amended with a row scope. Establish the pause precondition via
    // TaskRuntime's state API and keep business-admission checks in their own
    // tests.
    engine
        .tasks
        .admission_passed(&mut engine.storage, 101)
        .unwrap();
    assert_eq!(
        engine.tasks.active_state(),
        Some(interaction_contracts::desktop::task::RootState::Running)
    );
    let root = engine.run.as_ref().unwrap().goal.root_task_id.clone();
    let budget_before_pause = engine
        .tasks
        .active()
        .unwrap()
        .budget
        .as_ref()
        .unwrap()
        .observations_used;
    let pending_before_pause = engine.state.refresh_pending();

    let pause_outputs = engine.handle(
        102,
        Input::Operator(OperatorCommand::PauseTask {
            root_task_id: root.clone(),
        }),
    );
    assert_eq!(refresh_scopes(&pause_outputs), vec![None]);
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Cleared
    );
    assert_eq!(engine.state.refresh_pending(), pending_before_pause);
    assert_eq!(
        engine
            .tasks
            .active()
            .unwrap()
            .budget
            .as_ref()
            .unwrap()
            .observations_used,
        budget_before_pause,
        "scope cleanup is not an observation-budgeted refresh"
    );
    assert!(engine.run.as_ref().unwrap().wait.is_none());

    let stop_corr = pause_outputs
        .iter()
        .find_map(|output| match output {
            Output::Device { corr, command }
                if matches!(command.as_ref(), DeviceCommand::Stop(_)) =>
            {
                Some(*corr)
            }
            _ => None,
        })
        .expect("pause asks the device to release input");
    engine.handle(
        103,
        Input::Device {
            corr: stop_corr,
            reply: DeviceReply::Stopped(StopAck {
                stop_seq: 1,
                stop_floor: 0,
                scope: StopScope::Root {
                    root_id: root.as_str().to_string(),
                },
                disposition: StopDisposition::Pause,
                source: StopSource::Host,
                revoked_permits: 0,
                cleanup: ReleaseStatus::Released,
                at_ms: 103,
            }),
        },
    );
    assert_eq!(
        engine.tasks.active_state(),
        Some(interaction_contracts::desktop::task::RootState::Suspended)
    );

    engine.outputs.clear();
    let resume_outputs = engine.handle(
        104,
        Input::Operator(OperatorCommand::ResumeTask {
            root_task_id: root.clone(),
        }),
    );
    let resume_corr = resume_outputs
        .iter()
        .find_map(|output| match output {
            Output::Device { corr, command }
                if matches!(command.as_ref(), DeviceCommand::Resume(_)) =>
            {
                Some(*corr)
            }
            _ => None,
        })
        .expect("resume requires an explicit device acknowledgement");
    let resumed_outputs = engine.handle(
        105,
        Input::Device {
            corr: resume_corr,
            reply: DeviceReply::Resumed(ResumeAck {
                resumed: true,
                reason: None,
            }),
        },
    );
    assert_eq!(refresh_scopes(&resumed_outputs), vec![Some(scope)]);
}

#[test]
fn stop_failure_and_finish_clear_a_previously_scoped_source_without_spending_budget() {
    let (mut stopped, _) = owned_text_root("stop row task");
    request_lifecycle_row_scope(&mut stopped);
    stopped.outputs.clear();
    let root = stopped.run.as_ref().unwrap().goal.root_task_id.clone();
    let observations = stopped
        .tasks
        .active()
        .unwrap()
        .budget
        .as_ref()
        .unwrap()
        .observations_used;
    let outputs = stopped.handle(
        102,
        Input::Operator(OperatorCommand::StopTask {
            root_task_id: Some(root),
        }),
    );
    assert_eq!(refresh_scopes(&outputs), vec![None]);
    assert_eq!(
        stopped
            .tasks
            .active()
            .unwrap()
            .budget
            .as_ref()
            .unwrap()
            .observations_used,
        observations
    );

    let (mut failed, _) = owned_text_root("fail row task");
    request_lifecycle_row_scope(&mut failed);
    failed.outputs.clear();
    let failed_root = failed.tasks.active().unwrap().row.root_id.clone();
    let observations = TaskStore::load_budget(failed.storage().read(), failed_root.as_str())
        .unwrap()
        .observations;
    failed.fail_root("test failure").unwrap();
    assert_eq!(refresh_scopes(&failed.outputs), vec![None]);
    assert_eq!(
        TaskStore::load_budget(failed.storage().read(), failed_root.as_str())
            .unwrap()
            .observations,
        observations,
        "terminal failure must not turn scope cleanup into an observation"
    );

    let (mut finished, _) = owned_text_root("finish row task");
    request_lifecycle_row_scope(&mut finished);
    finished.outputs.clear();
    let finished_root = finished.tasks.active().unwrap().row.root_id.clone();
    let observations = TaskStore::load_budget(finished.storage().read(), finished_root.as_str())
        .unwrap()
        .observations;
    finished.finish_root();
    assert_eq!(refresh_scopes(&finished.outputs), vec![None]);
    assert!(finished.run.is_none());
    assert_eq!(
        TaskStore::load_budget(finished.storage().read(), finished_root.as_str())
            .unwrap()
            .observations,
        observations,
        "terminal finish must not turn scope cleanup into an observation"
    );
}
