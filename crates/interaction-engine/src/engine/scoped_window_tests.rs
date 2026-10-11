use std::collections::BTreeMap;

use crate::environment::pack::AppPack;
use crate::execution::{DeviceCommand, DeviceNotice, DeviceReply};
use crate::infrastructure::storage::Storage;
use crate::tasks::TaskConfig;
use interaction_contracts::desktop::binding::{BindingProofScope, DeviceContextWitness};
use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, CommandAck, DependencyVersions,
    DeviceIdentity, ExecutionIntent, GrantOutcome, PermitRef, Receipt, ReceiptStatus,
    ReleaseStatus, SourceFreshness, SubmittedAction, SurfaceRef,
};

use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{
    ConfirmationId, ObjectRef, RequestId, SemanticTargetRef,
};
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode};
use interaction_contracts::desktop::planning::{Dependency, GoalDraft, PlanBundle, Subgoal};
use interaction_contracts::desktop::primitive::{Primitive, PrimitivePolicy};
use interaction_contracts::desktop::source::{
    AppIdentity, GlobalFocusObservation, LayerKind, ObjectIdentityContinuity, ObservationFact,
    ObserverCoverage, Rect, SourceHealth, SourceIdentityBoundary, SourceKind, SourceStamp,
    SourceState, StructureSnapshot, UiObject, WindowInfo,
};
use interaction_contracts::desktop::task::{
    ConfirmationScope, ExpectedValue, RowColumnConditionSpec, RowTableScope, RowTargetSpec,
    TargetSpec,
};

use super::{Engine, EngineConfig, Input, NativeWindowScope, Output};

fn native_action_intent(
    intent: &ExecutionIntent,
) -> &interaction_contracts::desktop::device::ActionIntent {
    intent
        .native()
        .expect("this regression covers a native intent")
}

const APP: &str = "com.lingxi.computer-user.mvp-fixture";
const PID: u32 = 81166;
const WINDOW: &str = "window-23403";
const SYNTHETIC_FIXTURE: &str =
    include_str!("../../../../contracts/desktop-v1/native-binding/scoped-window-synthetic.json");

fn scoped_source() -> StructureSnapshot {
    serde_json::from_str(SYNTHETIC_FIXTURE)
        .expect("the test-only synthetic snapshot fixture is valid")
}

fn complete_source_with_other_window_active() -> StructureSnapshot {
    let target_frame = Rect {
        x: 20.0,
        y: 30.0,
        width: 600.0,
        height: 400.0,
    };
    let other_frame = Rect {
        x: 40.0,
        y: 50.0,
        width: 500.0,
        height: 350.0,
    };
    let windows = vec![
        WindowInfo {
            window_id: WINDOW.into(),
            title: Some("Interaction Fixture".into()),
            frame: target_frame,
            z_index: 1,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: APP.into(),
        },
        WindowInfo {
            window_id: "window-other".into(),
            title: Some("Other Window".into()),
            frame: other_frame,
            z_index: 0,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: APP.into(),
        },
    ];
    let objects = vec![
        UiObject {
            instance_ref: ObjectRef::new("complete-target-window-root").unwrap(),
            parent: None,
            window_id: WINDOW.into(),
            role: "AXWindow".into(),
            subrole: Some("AXStandardWindow".into()),
            title: Some("Interaction Fixture".into()),
            description: None,
            value: None,
            enabled: false,
            focused: false,
            frame: target_frame,
            actions: vec![],
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        },
        UiObject {
            instance_ref: ObjectRef::new("complete-active-window-root").unwrap(),
            parent: None,
            window_id: "window-other".into(),
            role: "AXWindow".into(),
            subrole: Some("AXStandardWindow".into()),
            title: Some("Other Window".into()),
            description: None,
            value: None,
            enabled: false,
            focused: false,
            frame: other_frame,
            actions: vec![],
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        },
    ];
    let snapshot = StructureSnapshot {
        snapshot_id: RequestId::new("complete-inactive-window-read-1").unwrap(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "macos/1/wall".into(),
        identity_boundary: Some(SourceIdentityBoundary {
            namespace: "11111111-1111-4111-8111-111111111111".into(),
            generation: 1,
        }),
        frame_ref: None,
        source_sequence: 1,
        observed_ms: 3_325,
        app: AppIdentity {
            bundle_id: APP.into(),
            name: "Interaction Fixture".into(),
            pid: PID,
        },
        windows,
        global_focus: GlobalFocusObservation {
            app: ObservationFact::Known {
                value: AppIdentity {
                    bundle_id: APP.into(),
                    name: "Interaction Fixture".into(),
                    pid: PID,
                },
            },
            window_id: ObservationFact::Known {
                value: "window-other".into(),
            },
        },
        target_context: None,
        complete: true,
        incomplete_reason: None,
        observer_coverage: ObserverCoverage::for_projected_objects(&objects),
        objects,
        menu_anchors: vec![],
        menu_context: None,
        focus_context: None,
        window_context: None,
        table_viewports: vec![],
        table_scope_search: None,
        owned_document_evidence: None,
    };
    snapshot
        .validate()
        .expect("the complete fixture contains its entire two-window tree");
    snapshot
}

fn source_read_after(snapshot: &StructureSnapshot, observed_ms: u64) -> StructureSnapshot {
    let mut next = snapshot.clone();
    next.snapshot_id = RequestId::new(format!(
        "scoped-window-source-read-{}",
        snapshot.source_sequence + 1
    ))
    .unwrap();
    next.source_sequence += 1;
    next.observed_ms = observed_ms;
    if let Some(context) = next.window_context.as_mut() {
        context.verified_source_sequence = next.source_sequence;
        context.verified_observed_ms = observed_ms;
    }
    next.validate()
        .expect("the follow-up Source DTO has matching current provenance");
    next
}

fn fixture_scope(snapshot: &StructureSnapshot) -> RowTableScope {
    snapshot
        .table_scope_search
        .as_ref()
        .expect("fixture provides a same-read table census")
        .requested_scope
        .clone()
}

fn app_pack() -> AppPack {
    AppPack::registered_native(APP).expect("the interaction fixture pack is registered")
}

fn goal_and_plan(row_scope: Option<RowTableScope>) -> (GoalDraft, PlanBundle) {
    let target = SemanticTargetRef::new("archive-row").unwrap();
    let row_goal = row_scope.is_some();
    let draft = GoalDraft {
        app: APP.into(),
        account: None,
        target: TargetSpec {
            row: row_scope.map(|table_scope| RowTargetSpec {
                conditions: vec![RowColumnConditionSpec {
                    column_name: table_scope.column_headers[0].clone(),
                    value: ExpectedValue::Text {
                        value: "已归档项目 1".into(),
                    },
                }],
                table_scope,
            }),
            semantic_target: target.clone(),
            label: "已归档项目 1".into(),
            group: None,
            role_hint: Some("AXRow".into()),
        },
        desired: if row_goal {
            ExpectedValue::Bool { value: true }
        } else {
            ExpectedValue::Text {
                value: "a value to inspect".into(),
            }
        },
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::Navigation,
    };
    let first_skill = if row_goal {
        "ui.select_row"
    } else {
        "ui.read_value"
    };
    let bundle = PlanBundle {
        subgoals: vec![
            Subgoal {
                id: "inspect-target".into(),
                description: "Inspect the requested target".into(),
                skill: first_skill.into(),
                target: Some(target.clone()),
                expects: vec![],
                scroll: None,
                select: None,
            },
            Subgoal {
                id: "revisit-target".into(),
                description: "Revisit the requested target".into(),
                skill: "ui.revisit".into(),
                target: Some(target),
                expects: vec![],
                scroll: None,
                select: None,
            },
        ],
        dependencies: vec![Dependency {
            before: "inspect-target".into(),
            after: "revisit-target".into(),
        }],
        required_observations: vec![],
        expected_effects: vec![],
        recovery_branches: vec![],
        termination: vec![],
    };
    (draft, bundle)
}

fn new_root(row_scope: Option<RowTableScope>) -> (Engine, ConfirmationId) {
    let pack = app_pack();
    let mut config = EngineConfig::new(TaskConfig::new(vec![pack]));
    config.initial_mode = RunMode::Execute;
    config.native_window_scope = Some(NativeWindowScope {
        app: APP.into(),
        pid: PID,
        window: WINDOW.into(),
    });
    let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());
    engine
        .tasks
        .create_root(&mut engine.storage, 100, "Select the archived item")
        .unwrap();
    let (draft, bundle) = goal_and_plan(row_scope);
    let confirmation = engine
        .tasks
        .accept_draft(
            &mut engine.storage,
            100,
            &draft,
            ExecutionDomain::ForegroundNative,
            &bundle,
        )
        .unwrap();
    // Install the foreground-native device and establish its lease before
    // confirming. A running root is not allowed to start under an unknown
    // execution-domain profile.
    let ready = engine.handle(
        100,
        Input::DeviceNotice(DeviceNotice::Manifest(Box::new(manifest()))),
    );
    let lease = command_corr(&ready, |command| {
        matches!(command, DeviceCommand::RenewLease(_))
    })
    .expect("the device manifest requests its lease before root confirmation");
    engine.handle(
        100,
        Input::Device {
            corr: lease,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    );
    (engine, confirmation.confirmation_id)
}

fn manifest() -> CapabilityManifest {
    CapabilityManifest {
        identity: DeviceIdentity {
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "macos/1/wall".into(),
            dependencies: BTreeMap::from([
                ("authorization".into(), 1),
                ("focus".into(), 2),
                ("window".into(), 3),
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
    }
}

fn command_corr(outputs: &[Output], predicate: impl Fn(&DeviceCommand) -> bool) -> Option<u64> {
    outputs.iter().rev().find_map(|output| match output {
        Output::Device { corr, command } if predicate(command) => Some(*corr),
        _ => None,
    })
}

fn source_refresh(outputs: &[Output]) -> Option<(u64, Option<RowTableScope>)> {
    outputs.iter().rev().find_map(|output| match output {
        Output::Source {
            generation,
            command:
                interaction_contracts::desktop::source::SourceCommand::RefreshNow { table_scope },
        } => Some((*generation, table_scope.clone())),
        _ => None,
    })
}

fn activate_from_source(
    snapshot: StructureSnapshot,
    frozen_scope: Option<RowTableScope>,
) -> (Engine, Vec<Output>) {
    let (mut engine, confirmation_id) = new_root(frozen_scope.clone());
    let mut outputs = engine.handle(
        100,
        Input::Operator(OperatorCommand::ConfirmGoal {
            confirmation_id,
            approved: true,
        }),
    );
    let registration = command_corr(&outputs, |command| {
        matches!(command, DeviceCommand::RegisterRoot(_))
    })
    .expect("the confirmed root is registered through Engine");
    outputs.extend(engine.handle(
        101,
        Input::Device {
            corr: registration,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
    let (refresh, requested_scope) = source_refresh(&outputs).expect("the root requests Source");
    assert_eq!(requested_scope, frozen_scope);
    outputs.extend(engine.handle(
        102,
        Input::SourceRefreshStarted {
            generation: refresh,
            accepted: true,
        },
    ));
    let arrival = snapshot.observed_ms + 1;
    outputs.extend(engine.handle(arrival, Input::Structure(Box::new(snapshot))));

    // The window dependency is updated from the admitted Source witness. The
    // device's acknowledgement then lets the ordinary attempt pump continue.
    for _ in 0..4 {
        let Some(corr) = command_corr(&outputs, |command| {
            matches!(command, DeviceCommand::UpdateDependency(_))
        }) else {
            break;
        };
        let mut versions = engine.device.dependencies().clone();
        if let Some(DeviceCommand::UpdateDependency(update)) =
            outputs.iter().rev().find_map(|output| match output {
                Output::Device {
                    corr: found,
                    command,
                } if *found == corr => Some(command.as_ref()),
                _ => None,
            })
        {
            let next = versions.get(&update.name).copied().unwrap_or_default() + 1;
            versions.insert(update.name.clone(), next);
        }
        outputs.extend(engine.handle(
            arrival + 1,
            Input::Device {
                corr,
                reply: DeviceReply::Dependencies(DependencyVersions { versions }),
            },
        ));
        // A correlation is consumed by the reply. Removing its output prevents
        // this small deterministic driver from acknowledging it twice.
        if let Some(position) = outputs.iter().position(|output| {
            matches!(output, Output::Device { corr: found, command } if *found == corr && matches!(command.as_ref(), DeviceCommand::UpdateDependency(_)))
        }) {
            outputs.remove(position);
        }
    }
    // Native AX evidence has a configured quiet interval before Engine grants
    // activation authority. Let the ordinary Tick pump cross that boundary.
    let settled_at = arrival + engine.cfg.native_evidence_quiet_ms + 1;
    outputs.extend(engine.handle(settled_at, Input::Tick));
    (engine, outputs)
}

fn observe_command(
    outputs: &[Output],
) -> Option<(u64, interaction_contracts::desktop::device::Observation)> {
    outputs.iter().rev().find_map(|output| match output {
        Output::Device { corr, command } => match command.as_ref() {
            DeviceCommand::Observe(observation) => Some((
                *corr,
                observation
                    .native()
                    .expect("native window scenario uses native evidence")
                    .clone(),
            )),
            _ => None,
        },
        _ => None,
    })
}

fn submit_window_activation(
    engine: &mut Engine,
    outputs: &mut Vec<Output>,
    now_ms: u64,
) -> (u64, interaction_contracts::desktop::device::SubmitIntent) {
    let (observe_corr, _) = observe_command(outputs).expect("activation is observed first");
    outputs.extend(engine.handle(
        now_ms,
        Input::Device {
            corr: observe_corr,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
    let grant_corr = command_corr(outputs, |command| {
        matches!(command, DeviceCommand::GrantPermit(_))
    })
    .expect("accepted activation evidence asks for a permit");
    outputs.extend(engine.handle(
        now_ms + 1,
        Input::Device {
            corr: grant_corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted {
                permit: PermitRef {
                    id: "window-activation-permit".into(),
                    generation: 1,
                },
            }),
        },
    ));
    let submit_corr = command_corr(outputs, |command| {
        matches!(command, DeviceCommand::Submit(_))
    })
    .expect("the authorized activation reaches one bounded Submit");
    let submit = outputs
        .iter()
        .find_map(|output| match output {
            Output::Device { corr, command } if *corr == submit_corr => match command.as_ref() {
                DeviceCommand::Submit(submit) => Some(submit.clone()),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    (submit_corr, submit)
}

fn activation_receipt(
    submit: &interaction_contracts::desktop::device::SubmitIntent,
    status: ReceiptStatus,
) -> Receipt {
    let native = native_action_intent(&submit.intent);
    let at_ms = submit.intent.latest_start_ms().saturating_add(5);
    let completed = status == ReceiptStatus::Completed;
    Receipt {
        intent_id: submit.intent.intent_id().to_owned(),
        attempt_id: submit.intent.attempt_id().map(str::to_owned),
        session_epoch: submit.intent.session_epoch(),
        status,
        reason_code: None,
        reason: None,
        submitted: if completed {
            vec![SubmittedAction::Native {
                at_ms: submit.intent.latest_start_ms(),
                primitive: native.steps[0].primitive.clone(),
                cleanup: false,
            }]
        } else {
            vec![]
        },
        release_status: ReleaseStatus::NotNeeded,
        command_end_ms: completed.then_some(at_ms + 1),
        input_end_ms: completed.then_some(at_ms),
        at_ms: at_ms + 1,
    }
}

#[test]
fn partial_frozen_row_admits_one_registered_window_activation_observation() {
    let snapshot = scoped_source();
    let scope = fixture_scope(&snapshot);
    let (engine, outputs) = activate_from_source(snapshot, Some(scope.clone()));
    let (corr, observation) = observe_command(&outputs)
        .expect("the unique registered activation candidate advances to Observe");

    assert_eq!(
        engine.root_state(),
        Some(interaction_contracts::desktop::task::RootState::Running)
    );
    assert!(engine.window_attempt());
    assert_eq!(
        engine
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .map(|attempt| attempt.skill.skill()),
        Some(crate::reaction::skills::Skill::ActivateWindow)
    );
    assert_eq!(
        outputs
            .iter()
            .filter(|output| matches!(output, Output::Device { command, .. } if matches!(command.as_ref(), DeviceCommand::Observe(_))))
            .count(),
        1,
        "the exact frozen table scope produces one activation candidate"
    );
    assert_eq!(
        observation.purpose,
        interaction_contracts::desktop::device::ObservationPurpose::WindowActivation
    );
    assert_eq!(observation.freshness, SourceFreshness::Verified);
    assert_eq!(
        observation.dependencies,
        BTreeMap::from([("authorization".into(), 1), ("window".into(), 4)])
    );
    let proof = observation
        .binding_proof
        .expect("activation is bound to the Source root");
    assert_eq!(
        proof.continuity.scope,
        BindingProofScope::WindowRoot { table_scope: scope }
    );
    assert!(matches!(
        proof.device_context,
        DeviceContextWitness::WindowRootV1 { .. }
    ));
    assert!(outputs.iter().any(|output| matches!(output,
        Output::Device { corr: observed, command }
            if *observed == corr && matches!(command.as_ref(), DeviceCommand::Observe(_))
    )));
}

#[test]
fn a_partial_source_cannot_activate_for_a_different_scope_or_a_goal_without_a_row() {
    let snapshot = scoped_source();
    let mut wrong_scope = fixture_scope(&snapshot);
    wrong_scope.scroll_path[0].label = "Preferences content".into();
    wrong_scope.scroll_path[1].label = "Archive".into();
    wrong_scope.column_headers[0] = "Archive item".into();
    let (engine, outputs) = activate_from_source(snapshot.clone(), Some(wrong_scope));
    assert!(!engine.window_evidence_usable());
    assert!(observe_command(&outputs).is_none());

    let (engine, outputs) = activate_from_source(snapshot, None);
    assert!(!engine.window_evidence_usable());
    assert!(observe_command(&outputs).is_none());
}

#[test]
fn a_complete_source_uses_the_whole_structure_window_proof() {
    let snapshot = complete_source_with_other_window_active();
    let (engine, outputs) = activate_from_source(snapshot, Some(fixture_scope(&scoped_source())));
    assert!(engine.window_attempt());
    let (_, observation) = observe_command(&outputs).expect("complete source still activates");
    let proof = observation
        .binding_proof
        .expect("complete root binding proof");
    assert_eq!(proof.continuity.scope, BindingProofScope::WholeStructure);
    assert!(matches!(
        proof.device_context,
        DeviceContextWitness::WholeStructureV1 { .. }
    ));
}

#[test]
fn ax_metadata_floor_revokes_a_pre_floor_partial_window_root() {
    let snapshot = scoped_source();
    let scope = fixture_scope(&snapshot);
    let (mut engine, outputs) = activate_from_source(snapshot.clone(), Some(scope));
    assert!(observe_command(&outputs).is_some());
    let floor = snapshot.observed_ms + 2;
    engine.handle(
        floor,
        Input::SourceHealth(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Degraded,
            reason: Some("AX metadata read degraded".into()),
            at_ms: floor,
            provenance: Some(SourceStamp {
                session_epoch: snapshot.session_epoch,
                stream_id: "scoped-window-test-ax".into(),
                source_sequence: snapshot.source_sequence + 1,
            }),
        }),
    );
    assert!(!engine.window_evidence_usable());
}

#[test]
fn completed_activation_waits_for_a_new_source_and_unknown_is_never_replayed() {
    let snapshot = scoped_source();
    let scope = fixture_scope(&snapshot);
    let (mut engine, mut outputs) = activate_from_source(snapshot.clone(), Some(scope));
    let (submit_corr, submit) = submit_window_activation(&mut engine, &mut outputs, 3_400);
    assert!(matches!(
        &native_action_intent(&submit.intent).steps[0].primitive,
        Primitive::ActivateWindow { .. }
    ));
    let receipt = activation_receipt(&submit, ReceiptStatus::Completed);
    let receipt_at = receipt.at_ms;
    outputs.extend(engine.handle(
        receipt_at,
        Input::Device {
            corr: submit_corr,
            reply: DeviceReply::Receipt(Some(Box::new(receipt))),
        },
    ));
    assert!(engine.window_attempt());
    assert!(!engine.run.as_ref().unwrap().window_activated);
    let (refresh, _) = source_refresh(&outputs).expect("completion requests fresh Source evidence");
    outputs.extend(engine.handle(
        receipt_at + 1,
        Input::SourceRefreshStarted {
            generation: refresh,
            accepted: true,
        },
    ));
    outputs.extend(engine.handle(receipt_at + 2, Input::Structure(Box::new(snapshot.clone()))));
    assert!(engine.window_attempt());
    assert!(!engine.run.as_ref().unwrap().window_activated);
    assert_eq!(
        engine.state.latest().unwrap().snapshot.source_sequence,
        snapshot.source_sequence,
        "the replayed pre-input Source does not replace the current read"
    );
    assert_eq!(
        outputs
            .iter()
            .filter(|output| matches!(output, Output::Device { command, .. } if matches!(command.as_ref(), DeviceCommand::Submit(_))))
            .count(),
        1,
        "the old partial Source cannot repeat a completed activation"
    );

    let fresh = source_read_after(&snapshot, receipt_at + 3);
    let arrival = fresh.observed_ms + 1;
    outputs.extend(engine.handle(arrival, Input::Structure(Box::new(fresh))));
    outputs.extend(engine.handle(
        arrival + engine.cfg.native_evidence_quiet_ms + 1,
        Input::Tick,
    ));
    assert!(engine.run.as_ref().unwrap().window_activated);
    assert!(!engine.window_attempt());
    assert_eq!(
        outputs
            .iter()
            .filter(|output| matches!(output, Output::Device { command, .. } if matches!(command.as_ref(), DeviceCommand::Submit(_))))
            .count(),
        1,
        "fresh same-root evidence verifies the completed activation without replay"
    );

    let (mut unknown_engine, mut unknown_outputs) =
        activate_from_source(scoped_source(), Some(fixture_scope(&scoped_source())));
    let (unknown_corr, unknown_submit) =
        submit_window_activation(&mut unknown_engine, &mut unknown_outputs, 3_400);
    let unknown = activation_receipt(&unknown_submit, ReceiptStatus::Unknown);
    let unknown_at = unknown.at_ms;
    unknown_outputs.extend(unknown_engine.handle(
        unknown_at,
        Input::Device {
            corr: unknown_corr,
            reply: DeviceReply::Receipt(Some(Box::new(unknown))),
        },
    ));
    let fresh = source_read_after(&scoped_source(), unknown_at + 3);
    let arrival = fresh.observed_ms + 1;
    unknown_outputs.extend(unknown_engine.handle(arrival, Input::Structure(Box::new(fresh))));
    unknown_outputs.extend(unknown_engine.handle(
        arrival + unknown_engine.cfg.native_evidence_quiet_ms + 1,
        Input::Tick,
    ));
    assert_eq!(
        unknown_outputs
            .iter()
            .filter(|output| matches!(output, Output::Device { command, .. } if matches!(command.as_ref(), DeviceCommand::Submit(_))))
            .count(),
        1,
        "an unknown activation outcome is terminal and is not replayed"
    );
    assert!(!unknown_engine.run.as_ref().unwrap().window_activated);
    assert!(unknown_engine.root_state().is_some());
}
