use std::collections::{BTreeMap, BTreeSet};

use interaction_contracts::desktop::binding::{BindRequest, BindScope, TargetDescription};
use interaction_contracts::desktop::decision::DecisionState;
use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, DependencyUpdate, DeviceIdentity, PermitRef,
    RootGrant, SurfaceRef,
};
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{ObjectRef, RequestId, SemanticTargetRef};
use interaction_contracts::desktop::operator::RunMode;
use interaction_contracts::desktop::primitive::PrimitivePolicy;
use interaction_contracts::desktop::source::{
    FrameMeta, FrameProvenance, FrameStatus, FrameStreamInfo, ObjectIdentityContinuity, Rect,
    SourceCommand, SourceHealth, SourceKind, SourceStamp, SourceState,
};

use crate::environment::binding::BindingReducer;
use crate::environment::pack::AppPack;
use crate::environment::state_core::StateCore;
use crate::execution::DeviceCommand;
use crate::infrastructure::storage::Storage;
use crate::reaction::decisions::{DecisionBasis, OpenSpec};
use crate::tasks::TaskConfig;
use crate::testkit::{EnvRig, TARGET, domain_capabilities_for_test_profile, goal};
use crate::verification::Verification;
use sim_world::app::{SimApp, SnapshotMeta};

use super::super::{
    Engine, EngineConfig, NativeWindowScope, Output, RootRun, SourceScopeCommandState, Wait,
};

fn fixture_engine(with_window_scope: bool) -> Engine {
    fixture_engine_with_identity_mode(with_window_scope, None)
}

fn fixture_engine_with_identity_mode(
    with_window_scope: bool,
    read_scoped_logical_reference: Option<&str>,
) -> Engine {
    let mut rig = EnvRig::new();
    for reference in [
        "sim-window-w1",
        "group-push",
        "sw-push-daily",
        "sw-push-weekly",
    ] {
        rig.app.set_object_identity_continuity(
            reference,
            ObjectIdentityContinuity::NotificationCovered,
        );
    }
    if let Some(reference) = read_scoped_logical_reference {
        rig.app
            .set_object_identity_continuity(reference, ObjectIdentityContinuity::ReadScoped);
    }
    let observed = rig.observe(10);
    let snapshot = observed.snapshot;
    let window = snapshot.windows[0].clone();
    let window_root = ObjectRef::new("sim-window-w1").unwrap();

    let mut state = StateCore::new();
    state.set_session(snapshot.session_epoch);
    state.admit(snapshot, 10, &rig.pack).unwrap();
    let env = state.latest().unwrap().clone();

    let mut bindings = BindingReducer::new();
    let target = SemanticTargetRef::new(TARGET).unwrap();
    let request = BindRequest {
        request_id: RequestId::new("identity-target-bind").unwrap(),
        semantic_target: target,
        description: TargetDescription {
            label: "Daily picks".into(),
            group: Some("Push".into()),
            role_hint: Some("switch".into()),
        },
        scope: BindScope {
            app: env.snapshot.app.bundle_id.clone(),
            account: None,
        },
        choice: None,
        max_age_ms: u64::MAX,
    };
    bindings.bind(&env, &request, 10);
    bindings
        .bind_window(&env, &window_root)
        .expect("fixture window gets a real activation binding");
    assert_eq!(bindings.all().count(), 2);
    assert!(
        bindings
            .all()
            .any(|binding| binding.rule == "window_activation")
    );
    assert!(
        bindings
            .current(&SemanticTargetRef::new(TARGET).unwrap())
            .is_some()
    );

    let mut config = EngineConfig::new(TaskConfig::new(vec![rig.pack.clone()]));
    config.initial_mode = RunMode::Execute;
    if with_window_scope {
        config.native_window_scope = Some(NativeWindowScope {
            app: env.snapshot.app.bundle_id.clone(),
            pid: env.snapshot.app.pid,
            window: window.window_id.clone(),
        });
    }
    let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());
    engine.now_ms = 10;
    engine.state = state;
    engine.bindings = bindings;
    engine.device.install_manifest(CapabilityManifest {
        identity: DeviceIdentity {
            session_epoch: 7,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "sim/1/monotonic".into(),
            dependencies: BTreeMap::from([
                ("binding".into(), 4),
                ("geometry".into(), 8),
                ("window".into(), 12),
            ]),
        },
        backend: BackendKind::Simulated,
        execution_domain: interaction_contracts::desktop::domain::ExecutionDomain::Simulated,
        policy: PrimitivePolicy::AtomicOnly,
        human_input: CapabilityState::Available,
        hit_test: CapabilityState::Available,
        input: CapabilityState::Available,
        domains: domain_capabilities_for_test_profile(
            interaction_contracts::desktop::domain::ExecutionDomain::Simulated,
        ),
        max_uninterruptible_ms: 100,
    });
    engine
}

fn add_existing_permits(engine: &mut Engine) {
    let target_binding = engine
        .bindings
        .current(&SemanticTargetRef::new(TARGET).unwrap())
        .unwrap()
        .binding_ref
        .clone();
    let window_binding = engine
        .bindings
        .current(&SemanticTargetRef::new("native-window").unwrap())
        .unwrap()
        .binding_ref
        .clone();
    let task_goal = goal(
        interaction_contracts::desktop::task::ConfirmationScope::UiRevisit,
        EffectClass::Navigation,
    );
    let root_id = task_goal.root_task_id.as_str().to_string();
    engine.run = Some(RootRun {
        window_activated: true,
        keyboard_result_context: None,
        verification: Verification::new(0),
        goal: task_goal,
        pack: AppPack::settings_fixture(),
        grant: RootGrant {
            root_id,
            revision: 1,
            root_epoch: 1,
            deadline_ms: 60_000,
        },
        registered: true,
        registering: false,
        done: BTreeSet::new(),
        attempt: None,
        wait: None,
        refined_group: None,
        chosen: None,
        plan_targets: BTreeSet::new(),
        recent: Vec::new(),
        failures: 0,
        decision_failures: 0,
        decision_resume_ms: 0,
        decision_retry_at: None,
        stalls: 0,
        bind_seq: 0,
        resume_authorized: false,
        reconcile: Vec::new(),
        reconciling: None,
        reconciliation_retries: 0,
        reconciled_input: BTreeSet::new(),
        effect_verification_only: false,
        revalidate_floor: None,
        permits: vec![
            PermitRef {
                id: "old-window-permit".into(),
                generation: 3,
            },
            PermitRef {
                id: "old-target-permit".into(),
                generation: 5,
            },
        ],
        stop_wait_since: None,
        paused_session: None,
        last_attempt: None,
    });
    engine.permit_bindings = BTreeMap::from([
        ("old-target-permit".into(), target_binding),
        ("old-window-permit".into(), window_binding),
    ]);
}

#[test]
fn a_row_goal_cannot_reuse_a_same_label_generic_input_binding() {
    use crate::reaction::candidates::Resolution;
    use interaction_contracts::desktop::task::{
        ExpectedValue, RowColumnConditionSpec, RowContainerRole, RowContainerScope, RowTableScope,
        RowTargetSpec,
    };

    let mut engine = fixture_engine(true);
    add_existing_permits(&mut engine);
    let run = engine.run.as_mut().unwrap();
    run.goal.target.role_hint = Some("AXRow".into());
    run.goal.target.row = Some(RowTargetSpec {
        table_scope: RowTableScope {
            scroll_path: vec![RowContainerScope {
                role: RowContainerRole::AxScrollArea,
                label: "Projects".into(),
            }],
            column_headers: vec!["Name".into(), "Owner".into()],
        },
        conditions: vec![
            RowColumnConditionSpec {
                column_name: "Name".into(),
                value: ExpectedValue::Text {
                    value: "Daily picks".into(),
                },
            },
            RowColumnConditionSpec {
                column_name: "Owner".into(),
                value: ExpectedValue::Text {
                    value: "Mina".into(),
                },
            },
        ],
    });
    let target = run.goal.target.semantic_target.clone();
    assert!(engine.bindings.current(&target).is_some());
    let description = engine.goal_description();
    let env = engine.state.latest().unwrap().clone();
    assert!(matches!(
        engine.resolve(&target, description, &env),
        Resolution::Unavailable(_)
    ));
    assert!(engine.bindings.current(&target).is_none());
    assert!(engine.outputs.is_empty());
}

fn dependency_updates(engine: &Engine) -> Vec<DependencyUpdate> {
    engine
        .outputs
        .iter()
        .filter_map(|output| match output {
            Output::Device { command, .. } => match command.as_ref() {
                DeviceCommand::UpdateDependency(update) => Some(update.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn revoked_permits(engine: &Engine) -> BTreeSet<String> {
    engine
        .outputs
        .iter()
        .filter_map(|output| match output {
            Output::Device { command, .. } => match command.as_ref() {
                DeviceCommand::Revoke(revoke) => Some(revoke.permit.id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn source_snapshot(
    sequence: u64,
    read_scoped_group: bool,
) -> interaction_contracts::desktop::source::StructureSnapshot {
    let mut app = SimApp::new();
    for reference in [
        "sim-window-w1",
        "group-push",
        "sw-push-daily",
        "sw-push-weekly",
    ] {
        app.set_object_identity_continuity(
            reference,
            ObjectIdentityContinuity::NotificationCovered,
        );
    }
    if read_scoped_group {
        app.set_object_identity_continuity("group-push", ObjectIdentityContinuity::ReadScoped);
    }
    app.snapshot(&SnapshotMeta {
        snapshot_id: format!("identity-snapshot-{sequence}"),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "sim/1/monotonic".into(),
        source_sequence: sequence,
        observed_ms: sequence * 10,
    })
}

fn open_pending_decision(engine: &mut Engine) {
    let env = engine.state.latest().unwrap();
    let basis = DecisionBasis::capture(env, &engine.bindings, 1, 1, 1);
    let root = engine.run.as_ref().unwrap().goal.root_task_id.clone();
    let state = DecisionState {
        root_task_id: root.clone(),
        task_revision: 1,
        goal_summary: "test identity decision".into(),
        subgoal: "choose a target".into(),
        snapshot_revision: env.revision,
        objects: Vec::new(),
        candidates: Vec::new(),
        recent_results: Vec::new(),
        missing_fields: Vec::new(),
        omitted_items: Vec::new(),
        candidate_set_digest: "empty".into(),
    };
    engine
        .decisions
        .open(
            &mut engine.storage,
            OpenSpec {
                root: &root,
                task_revision: 1,
                plan_revision: 1,
                root_epoch: 1,
                program: "test identity choice",
                state,
                basis,
                subgoal: Some("target".into()),
                now_ms: engine.now_ms,
                timeout_ms: 1_000,
            },
        )
        .unwrap();
}

fn target_request(
    env: &crate::environment::state_core::EnvSnapshot,
    request_id: &str,
) -> BindRequest {
    BindRequest {
        request_id: RequestId::new(request_id).unwrap(),
        semantic_target: SemanticTargetRef::new(TARGET).unwrap(),
        description: TargetDescription {
            label: "Daily picks".into(),
            group: Some("Push".into()),
            role_hint: Some("switch".into()),
        },
        scope: BindScope {
            app: env.snapshot.app.bundle_id.clone(),
            account: None,
        },
        choice: None,
        max_age_ms: u64::MAX,
    }
}

#[test]
fn identity_reset_bypasses_closed_gate_and_clears_all_bindings() {
    let mut engine = fixture_engine(false);
    engine.source_gate_closed = true;

    engine
        .invalidate_identity_boundary("registry generation advanced")
        .unwrap();

    assert!(engine.source_gate_closed);
    assert_eq!(engine.bindings.all().count(), 0);
    let updates = dependency_updates(&engine);
    assert!(updates.contains(&DependencyUpdate {
        name: "binding".into(),
        valid: Some(false),
    }));
    assert!(updates.contains(&DependencyUpdate {
        name: "geometry".into(),
        valid: Some(false),
    }));
}

#[test]
fn identity_reset_revokes_window_and_target_authority_and_reports_current_window_evidence() {
    let mut engine = fixture_engine(true);
    add_existing_permits(&mut engine);
    assert!(engine.window_evidence_usable());
    engine.source_gate_closed = true;

    engine
        .invalidate_identity_boundary("registry generation advanced")
        .unwrap();

    assert_eq!(engine.bindings.all().count(), 0);
    assert_eq!(
        revoked_permits(&engine),
        BTreeSet::from(["old-target-permit".into(), "old-window-permit".into()])
    );
    assert!(engine.run.as_ref().unwrap().permits.is_empty());
    assert!(dependency_updates(&engine).contains(&DependencyUpdate {
        name: "window".into(),
        valid: Some(true),
    }));
}

#[test]
fn a_read_scoped_ancestor_expires_only_its_binding_and_revokes_dependent_state() {
    let mut engine = fixture_engine_with_identity_mode(false, Some("group-push"));
    let target = SemanticTargetRef::new(TARGET).unwrap();
    let old_target = engine.bindings.current(&target).unwrap().clone();
    assert_eq!(
        old_target.continuity.kind,
        ObjectIdentityContinuity::ReadScoped,
        "a covered leaf inherits the read-scoped status of its ancestor closure"
    );
    let window_target = SemanticTargetRef::new("native-window").unwrap();
    let old_window = engine
        .bindings
        .current(&window_target)
        .unwrap()
        .binding_ref
        .clone();

    add_existing_permits(&mut engine);
    open_pending_decision(&mut engine);
    let old_decision = engine.decisions.pending().unwrap().decision_id().clone();

    engine.now_ms = 20;
    engine.on_structure(source_snapshot(2, true)).unwrap();

    assert!(engine.bindings.current(&target).is_none());
    assert_eq!(
        engine.bindings.current(&window_target).unwrap().binding_ref,
        old_window,
        "the unrelated covered window proof remains live"
    );
    assert_eq!(
        revoked_permits(&engine),
        BTreeSet::from(["old-target-permit".into()])
    );
    assert_eq!(
        engine
            .run
            .as_ref()
            .unwrap()
            .permits
            .iter()
            .map(|permit| permit.id.as_str())
            .collect::<Vec<_>>(),
        vec!["old-window-permit"]
    );
    assert!(engine.decisions.pending().is_none());
    assert!(engine.outputs.iter().any(|output| matches!(
        output,
        Output::CancelDecision(cancel) if cancel.decision_id == old_decision
    )));
    assert!(
        !dependency_updates(&engine)
            .iter()
            .any(|update| { update.name == "binding" && update.valid == Some(false) })
    );

    let env = engine.state.latest().unwrap().clone();
    let rebound = engine.bindings.bind(
        &env,
        &target_request(&env, "fresh-after-read-scoped"),
        engine.now_ms,
    );
    let fresh_ref = match rebound.outcome {
        interaction_contracts::desktop::binding::BindingOutcome::Bound { binding, .. } => binding,
        outcome => panic!("fresh read must create a new binding: {outcome:?}"),
    };
    assert_ne!(fresh_ref, old_target.binding_ref);
}

#[test]
fn a_covered_binding_drops_when_its_context_closure_changes() {
    let mut engine = fixture_engine(false);
    let target = SemanticTargetRef::new(TARGET).unwrap();
    let old_binding = engine.bindings.current(&target).unwrap().clone();
    let old_window = engine
        .bindings
        .current(&SemanticTargetRef::new("native-window").unwrap())
        .unwrap()
        .binding_ref
        .clone();
    assert_eq!(
        old_binding.continuity.kind,
        ObjectIdentityContinuity::NotificationCovered
    );

    let mut snapshot = source_snapshot(2, false);
    {
        let group = snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref.as_str() == "group-push")
            .unwrap();
        group.title = Some("Renamed Push".into());
    }
    for object in &mut snapshot.objects {
        if object
            .parent
            .as_ref()
            .is_some_and(|parent| parent.as_str() == "group-push")
        {
            object.group = Some("Renamed Push".into());
        }
    }
    engine.now_ms = 20;
    engine.on_structure(snapshot).unwrap();

    assert!(engine.bindings.current(&target).is_none());
    assert_eq!(
        engine
            .bindings
            .current(&SemanticTargetRef::new("native-window").unwrap())
            .unwrap()
            .binding_ref,
        old_window,
        "the context update invalidates only closures that depend on the group"
    );
    let env = engine.state.latest().unwrap().clone();
    let mut request = target_request(&env, "context-rebind");
    request.description.group = Some("Renamed Push".into());
    let rebound = engine.bindings.bind(&env, &request, engine.now_ms);
    assert!(matches!(
        rebound.outcome,
        interaction_contracts::desktop::binding::BindingOutcome::Bound { .. }
    ));
    assert_ne!(
        engine.bindings.current(&target).unwrap().binding_ref,
        old_binding.binding_ref,
        "a fresh binding uses a new reference after the context digest changes"
    );
}

#[test]
fn unavailable_root_observer_keeps_the_global_reset_until_a_new_healthy_baseline() {
    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    open_pending_decision(&mut engine);
    let old_target = engine
        .bindings
        .current(&SemanticTargetRef::new(TARGET).unwrap())
        .unwrap()
        .binding_ref
        .clone();

    let mut unavailable = source_snapshot(2, false);
    for object in &mut unavailable.objects {
        object.identity_continuity = ObjectIdentityContinuity::ReadScoped;
    }
    unavailable.observer_coverage.state = SourceState::Unavailable;
    unavailable.observer_coverage.covered_objects = 0;
    unavailable.observer_coverage.read_scoped_objects = unavailable.objects.len() as u32;
    engine.now_ms = 20;
    engine.on_structure(unavailable).unwrap();

    assert!(engine.source_gate_closed);
    assert!(!engine.evidence_usable());
    assert!(!engine.state.usable(engine.now_ms, 2_000));
    assert!(!engine.window_evidence_usable());
    assert_eq!(engine.bindings.all().count(), 0);
    assert_eq!(
        revoked_permits(&engine),
        BTreeSet::from(["old-target-permit".into(), "old-window-permit".into()])
    );
    assert!(engine.decisions.pending().is_none());

    engine.now_ms = 30;
    engine.on_structure(source_snapshot(3, false)).unwrap();
    assert!(!engine.source_gate_closed);
    assert!(engine.evidence_usable());
    let env = engine.state.latest().unwrap().clone();
    let rebound = engine.bindings.bind(
        &env,
        &target_request(&env, "after-healthy-baseline"),
        engine.now_ms,
    );
    let fresh_ref = match rebound.outcome {
        interaction_contracts::desktop::binding::BindingOutcome::Bound { binding, .. } => binding,
        outcome => panic!("healthy baseline permits a fresh binding: {outcome:?}"),
    };
    assert_ne!(fresh_ref, old_target);
}

fn row_table_scope() -> interaction_contracts::desktop::task::RowTableScope {
    use interaction_contracts::desktop::task::{
        RowContainerRole, RowContainerScope, RowTableScope,
    };

    RowTableScope {
        scroll_path: vec![RowContainerScope {
            role: RowContainerRole::AxScrollArea,
            label: "Preferences content".into(),
        }],
        column_headers: vec!["Name".into(), "Owner".into()],
    }
}

fn install_frozen_row_scope(engine: &mut Engine) {
    use interaction_contracts::desktop::task::{
        ExpectedValue, RowColumnConditionSpec, RowTargetSpec,
    };

    engine.run.as_mut().unwrap().goal.target.row = Some(RowTargetSpec {
        table_scope: row_table_scope(),
        conditions: vec![RowColumnConditionSpec {
            column_name: "Name".into(),
            value: ExpectedValue::Text {
                value: "private row value must stay in Engine".into(),
            },
        }],
    });
}

fn last_refresh(
    engine: &Engine,
) -> (
    u64,
    Option<interaction_contracts::desktop::task::RowTableScope>,
) {
    let Some(Output::Source {
        generation,
        command: SourceCommand::RefreshNow { table_scope },
    }) = engine.outputs.last()
    else {
        panic!(
            "expected Source RefreshNow output, got {:?}",
            engine.outputs
        );
    };
    (*generation, table_scope.clone())
}

fn refresh_scopes(
    engine: &Engine,
) -> Vec<Option<interaction_contracts::desktop::task::RowTableScope>> {
    engine
        .outputs
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
fn refresh_uses_only_the_frozen_public_row_scope_and_resume_refresh_reuses_it() {
    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    install_frozen_row_scope(&mut engine);
    let scope = row_table_scope();

    engine.send_refresh().unwrap();
    let (first_generation, first_scope) = last_refresh(&engine);
    assert_eq!(first_scope, Some(scope.clone()));
    let first_command = match &engine.outputs.last().unwrap() {
        Output::Source { command, .. } => command,
        _ => unreachable!(),
    };
    let wire = serde_json::to_string(first_command).unwrap();
    assert!(!wire.contains("conditions"));
    assert!(!wire.contains("private row value must stay in Engine"));
    assert!(engine.state.refresh_started(first_generation));

    engine.clear_source_table_scope(false);
    let (clear_generation, cleared_scope) = last_refresh(&engine);
    assert!(clear_generation > first_generation);
    assert_eq!(cleared_scope, None);
    assert!(!engine.state.refresh_pending());
    engine.outputs.clear();

    // A normal refresh after the lifecycle clear (the same path used after
    // Resume) re-reads only the still-frozen public table scope.
    engine.send_refresh().unwrap();
    assert_eq!(last_refresh(&engine).1, Some(scope));
}

#[test]
fn a_nonrow_normal_refresh_explicitly_clears_the_previous_scope() {
    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    engine.source_scope_command_state = SourceScopeCommandState::Scoped;

    engine.send_refresh().unwrap();

    assert_eq!(last_refresh(&engine).1, None);
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Cleared
    );
}

#[test]
fn lifecycle_clear_uses_a_unique_ack_generation_without_waiting_or_completing_a_read() {
    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    install_frozen_row_scope(&mut engine);
    engine.send_refresh().unwrap();
    let (business_generation, scope) = last_refresh(&engine);
    assert!(scope.is_some());
    engine.run.as_mut().unwrap().wait = Some(Wait {
        floor_ms: 41,
        requested_ms: 42,
    });

    engine.clear_source_table_scope(true);

    let (clear_generation, cleared_scope) = last_refresh(&engine);
    assert!(clear_generation > business_generation);
    assert_eq!(cleared_scope, None);
    assert!(engine.state.refresh_pending());
    assert_eq!(
        engine
            .run
            .as_ref()
            .unwrap()
            .wait
            .map(|wait| (wait.floor_ms, wait.requested_ms)),
        Some((41, 42))
    );
    engine.on_refresh_started(clear_generation, true).unwrap();
    assert!(
        engine.state.refresh_pending(),
        "a cleanup ack is not the business refresh ack"
    );

    // A later business read has its own generation. Neither its predecessor's
    // delayed acknowledgement nor the out-of-band cleanup acknowledgement can
    // settle it.
    engine.send_refresh().unwrap();
    let (later_generation, later_scope) = last_refresh(&engine);
    assert!(later_generation > clear_generation);
    assert_eq!(later_scope, Some(row_table_scope()));
    engine
        .on_refresh_started(business_generation, true)
        .unwrap();
    assert!(
        engine.state.refresh_pending(),
        "an old read ack cannot settle the new read"
    );
    engine.on_refresh_started(clear_generation, true).unwrap();
    assert!(
        engine.state.refresh_pending(),
        "a cleanup ack cannot settle the new read"
    );
    engine.on_refresh_started(later_generation, true).unwrap();
    assert!(!engine.state.refresh_pending());
}

fn bare_engine() -> Engine {
    let pack = AppPack::settings_fixture();
    Engine::new(
        EngineConfig::new(TaskConfig::new(vec![pack])),
        Storage::open_in_memory().unwrap(),
    )
}

fn admit_health_test_frame(
    engine: &mut Engine,
    status: FrameStatus,
    sequence: u64,
    callback_ms: u64,
) {
    engine.state.set_identity(
        &DeviceIdentity {
            session_epoch: 7,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "native/7/monotonic".into(),
            dependencies: BTreeMap::new(),
        },
        false,
    );
    engine
        .state
        .admit_frame(
            FrameMeta {
                frame_id: format!("health-frame-{sequence}"),
                session_epoch: 7,
                surface: SurfaceRef {
                    id: "main".into(),
                    epoch: 1,
                },
                clock_domain: "native/7/monotonic".into(),
                source_sequence: sequence,
                source_ms: callback_ms,
                status,
                geometry_revision: 1,
                width_px: 100,
                height_px: 80,
                scale_factor: 1.0,
                byte_length: if status == FrameStatus::Complete {
                    32_000
                } else {
                    0
                },
                checksum: if status == FrameStatus::Complete {
                    format!("health-checksum-{sequence}")
                } else {
                    String::new()
                },
                provenance: Some(FrameProvenance::Stream {
                    stream: FrameStreamInfo {
                        stream_id: "health-test-watch".into(),
                        image_frame_id: (status == FrameStatus::Complete)
                            .then(|| format!("health-image-{sequence}")),
                        display_time_ticks: sequence,
                        display_time_ns: sequence,
                        received_uptime_ns: callback_ms.max(1),
                        received_unix_ms: callback_ms,
                        callback_ms,
                        source_clock: "mach_absolute".into(),
                        window_id: "window-1".into(),
                        pid: 42,
                        window_frame: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 100.0,
                            height: 80.0,
                        },
                        content_rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 100.0,
                            height: 80.0,
                        },
                        content_scale: 1.0,
                        dirty_rects: Vec::new(),
                        dropped_frames: 0,
                        capture_interval_ms: 16,
                    },
                }),
            },
            callback_ms,
            2_000,
        )
        .unwrap();
}

fn pre_frame_ocr_health(reason: &str, at_ms: u64) -> SourceHealth {
    SourceHealth {
        source: SourceKind::Ocr,
        state: SourceState::Unavailable,
        reason: Some(reason.into()),
        at_ms,
        provenance: Some(SourceStamp {
            session_epoch: 7,
            stream_id: "health-test-ocr".into(),
            source_sequence: 1,
        }),
    }
}

#[test]
fn rejected_old_source_health_does_not_clear_the_current_scope() {
    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    install_frozen_row_scope(&mut engine);
    engine.source_scope_command_state = SourceScopeCommandState::Scoped;
    let current_epoch = engine.state.latest().unwrap().snapshot.session_epoch;
    engine
        .state
        .note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Available,
                reason: None,
                at_ms: 11,
                provenance: Some(SourceStamp {
                    session_epoch: current_epoch,
                    stream_id: "current-watch".into(),
                    source_sequence: 1,
                }),
            },
            11,
        )
        .unwrap();
    engine.outputs.clear();

    engine.now_ms = 20;
    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Unavailable,
            reason: Some("a stale source session".into()),
            at_ms: 20,
            provenance: Some(SourceStamp {
                session_epoch: current_epoch.saturating_sub(1),
                stream_id: "retired-watch".into(),
                source_sequence: 2,
            }),
        })
        .unwrap();
    assert!(refresh_scopes(&engine).is_empty());
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Scoped
    );
}

#[test]
fn first_available_health_does_not_guess_session_replacement_from_unknown_scope_state() {
    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    install_frozen_row_scope(&mut engine);
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Unknown
    );
    let session_epoch = engine.state.latest().unwrap().snapshot.session_epoch;
    engine.outputs.clear();
    engine.now_ms = 20;

    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Available,
            reason: None,
            at_ms: 20,
            provenance: Some(SourceStamp {
                session_epoch,
                stream_id: "first-source-health".into(),
                source_sequence: 1,
            }),
        })
        .unwrap();

    assert!(refresh_scopes(&engine).is_empty());
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Unknown
    );
    assert_eq!(
        engine
            .state
            .health(SourceKind::Ax)
            .map(|health| health.state),
        Some(SourceState::Available)
    );
}

#[test]
fn exact_pre_frame_ocr_placeholder_keeps_health_without_clearing_initial_scope() {
    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    install_frozen_row_scope(&mut engine);
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Unknown
    );
    let session_epoch = engine.state.latest().unwrap().snapshot.session_epoch;
    engine.outputs.clear();
    engine.now_ms = 20;

    engine
        .on_health(SourceHealth {
            source: SourceKind::Ocr,
            state: SourceState::Unavailable,
            reason: Some("not captured".into()),
            at_ms: 20,
            provenance: Some(SourceStamp {
                session_epoch,
                stream_id: "startup-ocr".into(),
                source_sequence: 1,
            }),
        })
        .unwrap();

    assert!(refresh_scopes(&engine).is_empty());
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Unknown
    );
    let ocr = engine.state.health(SourceKind::Ocr).unwrap();
    assert_eq!(ocr.state, SourceState::Unavailable);
    assert_eq!(ocr.reason.as_deref(), Some("not captured"));

    // The exemption is only for the auxiliary OCR placeholder. A real
    // observer failure still clears the scope even after that health arrived.
    engine.outputs.clear();
    engine.now_ms = 21;
    engine
        .on_health(SourceHealth {
            source: SourceKind::AxObserver,
            state: SourceState::Unavailable,
            reason: Some("AX notification observer is unavailable".into()),
            at_ms: 21,
            provenance: Some(SourceStamp {
                session_epoch,
                stream_id: "startup-ax-observer".into(),
                source_sequence: 1,
            }),
        })
        .unwrap();
    assert_eq!(refresh_scopes(&engine), vec![None]);
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Cleared
    );
}

#[test]
fn startup_ocr_placeholder_is_not_exempt_after_a_frame_callback_or_for_other_reasons() {
    let mut no_frame_engine = bare_engine();
    no_frame_engine.now_ms = 10;
    no_frame_engine
        .on_health(pre_frame_ocr_health("Vision capture failed", 10))
        .unwrap();
    assert_eq!(refresh_scopes(&no_frame_engine), vec![None]);

    let callback_cases = [
        (FrameStatus::Started, 1, 10, 10),
        (FrameStatus::Blank, 2, 11, 11),
        (FrameStatus::Complete, 1, 10, 3_000),
    ];
    for (status, sequence, callback_ms, health_ms) in callback_cases {
        let mut engine = bare_engine();
        if status == FrameStatus::Blank {
            admit_health_test_frame(&mut engine, FrameStatus::Started, 1, 10);
        }
        admit_health_test_frame(&mut engine, status, sequence, callback_ms);
        let observation = engine.state.observation_view(health_ms).unwrap();
        let frame = observation.frame.expect("the callback remains observable");
        if status == FrameStatus::Complete {
            assert!(
                !frame.healthy,
                "the complete frame is expired at the health time"
            );
        }
        engine.now_ms = health_ms;

        engine
            .on_health(pre_frame_ocr_health("not captured", health_ms))
            .unwrap();

        assert_eq!(refresh_scopes(&engine), vec![None]);
        assert_eq!(
            engine.source_scope_command_state,
            SourceScopeCommandState::Cleared
        );
    }
}

#[test]
fn source_session_replacement_and_disconnect_force_an_out_of_band_scope_clear() {
    let mut engine = bare_engine();
    engine.now_ms = 10;
    engine.source_scope_command_state = SourceScopeCommandState::Cleared;
    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Available,
            reason: None,
            at_ms: 10,
            provenance: Some(SourceStamp {
                session_epoch: 1,
                stream_id: "ax-session-1".into(),
                source_sequence: 1,
            }),
        })
        .unwrap();
    engine.outputs.clear();

    engine.now_ms = 20;
    engine.source_scope_command_state = SourceScopeCommandState::Cleared;
    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Available,
            reason: None,
            at_ms: 20,
            provenance: Some(SourceStamp {
                session_epoch: 2,
                stream_id: "ax-session-2".into(),
                source_sequence: 1,
            }),
        })
        .unwrap();
    assert_eq!(last_refresh(&engine).1, None);
    assert!(!engine.state.refresh_pending());
    engine.outputs.clear();

    engine.now_ms = 30;
    engine.source_scope_command_state = SourceScopeCommandState::Cleared;
    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Unavailable,
            reason: Some("Source disconnected".into()),
            at_ms: 30,
            provenance: None,
        })
        .unwrap();
    assert_eq!(last_refresh(&engine).1, None);
    assert!(!engine.state.refresh_pending());
    engine.outputs.clear();

    // Unavailable without provenance does not erase the last accepted session
    // epoch; reconnecting at a new epoch still forces a clear.
    engine.now_ms = 40;
    engine.source_scope_command_state = SourceScopeCommandState::Cleared;
    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Available,
            reason: None,
            at_ms: 40,
            provenance: Some(SourceStamp {
                session_epoch: 3,
                stream_id: "ax-session-3".into(),
                source_sequence: 1,
            }),
        })
        .unwrap();
    assert_eq!(last_refresh(&engine).1, None);
    assert!(!engine.state.refresh_pending());
}

#[test]
fn clear_only_refresh_health_and_late_ack_do_not_create_a_business_wait() {
    const EXPLICIT_REFRESH_REASON: &str =
        "an explicit refresh invalidated the previous source stream";

    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    engine.source_scope_command_state = SourceScopeCommandState::Scoped;
    for source in [
        SourceKind::Screenshot,
        SourceKind::Ax,
        SourceKind::AxObserver,
        SourceKind::Ocr,
    ] {
        engine
            .state
            .note_health(
                SourceHealth {
                    source,
                    state: SourceState::Available,
                    reason: None,
                    at_ms: 11,
                    provenance: Some(SourceStamp {
                        session_epoch: 1,
                        stream_id: "watch-before-clear".into(),
                        source_sequence: 1,
                    }),
                },
                11,
            )
            .unwrap();
    }

    engine.now_ms = 12;
    engine.clear_source_table_scope(true);
    let (first_clear_generation, _) = last_refresh(&engine);
    engine.clear_source_table_scope(true);
    let (latest_clear_generation, _) = last_refresh(&engine);
    assert!(latest_clear_generation > first_clear_generation);
    assert_eq!(
        engine.source_scope_clear_generation,
        Some(latest_clear_generation)
    );
    assert!(!engine.state.refresh_pending());
    engine.outputs.clear();

    for (sequence, source) in [
        SourceKind::Screenshot,
        SourceKind::Ax,
        SourceKind::AxObserver,
        SourceKind::Ocr,
    ]
    .into_iter()
    .enumerate()
    {
        engine.now_ms = 20 + sequence as u64;
        let at_ms = engine.now_ms;
        engine
            .on_health(SourceHealth {
                source,
                state: SourceState::Unavailable,
                reason: Some(EXPLICIT_REFRESH_REASON.into()),
                at_ms,
                provenance: Some(SourceStamp {
                    session_epoch: 1,
                    stream_id: "watch-before-clear".into(),
                    source_sequence: 2,
                }),
            })
            .unwrap();
    }
    assert!(refresh_scopes(&engine).is_empty());
    assert!(!engine.state.refresh_pending());
    assert_eq!(
        engine.source_scope_clear_generation,
        Some(latest_clear_generation)
    );

    engine
        .on_refresh_started(first_clear_generation, true)
        .unwrap();
    assert_eq!(
        engine.source_scope_clear_generation,
        Some(latest_clear_generation)
    );
    assert!(!engine.state.refresh_pending());
    engine
        .on_refresh_started(latest_clear_generation, true)
        .unwrap();
    assert_eq!(engine.source_scope_clear_generation, None);
    assert!(!engine.state.refresh_pending());

    // A subsequent normal read supersedes any outstanding clear-only marker
    // and remains the sole operation allowed to establish refresh_pending.
    install_frozen_row_scope(&mut engine);
    engine.clear_source_table_scope(true);
    engine.send_refresh().unwrap();
    assert_eq!(engine.source_scope_clear_generation, None);
    assert!(engine.state.refresh_pending());
    assert!(last_refresh(&engine).1.is_some());
}

#[test]
fn business_refresh_stream_restart_health_does_not_clear_its_frozen_scope() {
    const EXPLICIT_REFRESH_REASON: &str =
        "an explicit refresh invalidated the previous source stream";

    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    install_frozen_row_scope(&mut engine);
    for source in [
        SourceKind::Screenshot,
        SourceKind::Ax,
        SourceKind::AxObserver,
        SourceKind::Ocr,
    ] {
        engine
            .state
            .note_health(
                SourceHealth {
                    source,
                    state: SourceState::Available,
                    reason: None,
                    at_ms: 11,
                    provenance: Some(SourceStamp {
                        session_epoch: 1,
                        stream_id: "watch-before-refresh".into(),
                        source_sequence: 1,
                    }),
                },
                11,
            )
            .unwrap();
    }

    engine.now_ms = 12;
    engine.send_refresh().unwrap();
    assert!(last_refresh(&engine).1.is_some());
    assert!(engine.state.refresh_pending());
    engine.outputs.clear();

    for (sequence, source) in [
        SourceKind::Screenshot,
        SourceKind::Ax,
        SourceKind::AxObserver,
        SourceKind::Ocr,
    ]
    .into_iter()
    .enumerate()
    {
        engine.now_ms = 20 + sequence as u64;
        let at_ms = engine.now_ms;
        engine
            .on_health(SourceHealth {
                source,
                state: SourceState::Unavailable,
                reason: Some(EXPLICIT_REFRESH_REASON.into()),
                at_ms,
                provenance: Some(SourceStamp {
                    session_epoch: 1,
                    stream_id: "watch-before-refresh".into(),
                    source_sequence: 2,
                }),
            })
            .unwrap();
    }
    assert!(refresh_scopes(&engine).is_empty());
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Scoped
    );
    assert!(engine.state.refresh_pending());

    // RefreshNow rotates stream identity inside the same Source session. That
    // is handled by evidence invalidation, not by erasing the requested scope.
    engine.outputs.clear();
    engine.now_ms = 30;
    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Available,
            reason: None,
            at_ms: 30,
            provenance: Some(SourceStamp {
                session_epoch: 1,
                stream_id: "watch-after-refresh".into(),
                source_sequence: 1,
            }),
        })
        .unwrap();
    assert!(refresh_scopes(&engine).is_empty());
    assert_eq!(
        engine.source_scope_command_state,
        SourceScopeCommandState::Scoped
    );
}

#[test]
fn real_unavailable_clears_once_while_pending_and_repeated_health_does_not_resend() {
    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    install_frozen_row_scope(&mut engine);
    engine
        .state
        .note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Available,
                reason: None,
                at_ms: 11,
                provenance: Some(SourceStamp {
                    session_epoch: 1,
                    stream_id: "watch".into(),
                    source_sequence: 1,
                }),
            },
            11,
        )
        .unwrap();
    engine.now_ms = 12;
    engine.send_refresh().unwrap();
    engine.outputs.clear();

    engine.now_ms = 20;
    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Unavailable,
            reason: Some("the accessibility source failed".into()),
            at_ms: 20,
            provenance: Some(SourceStamp {
                session_epoch: 1,
                stream_id: "watch".into(),
                source_sequence: 2,
            }),
        })
        .unwrap();
    assert_eq!(refresh_scopes(&engine), vec![None]);
    assert!(engine.state.refresh_pending());
    engine.outputs.clear();

    engine.now_ms = 21;
    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Unavailable,
            reason: Some("the accessibility source failed".into()),
            at_ms: 21,
            provenance: Some(SourceStamp {
                session_epoch: 1,
                stream_id: "watch".into(),
                source_sequence: 3,
            }),
        })
        .unwrap();
    assert!(refresh_scopes(&engine).is_empty());
}

#[test]
fn explicit_refresh_unavailable_without_a_pending_ack_is_treated_as_a_real_failure() {
    let mut engine = fixture_engine(false);
    add_existing_permits(&mut engine);
    install_frozen_row_scope(&mut engine);
    engine
        .state
        .note_health(
            SourceHealth {
                source: SourceKind::Ax,
                state: SourceState::Available,
                reason: None,
                at_ms: 11,
                provenance: Some(SourceStamp {
                    session_epoch: 1,
                    stream_id: "watch".into(),
                    source_sequence: 1,
                }),
            },
            11,
        )
        .unwrap();
    engine.now_ms = 12;
    engine.send_refresh().unwrap();
    let (generation, _) = last_refresh(&engine);
    engine.on_refresh_started(generation, true).unwrap();
    assert!(!engine.state.refresh_pending());
    engine.outputs.clear();

    const EXPLICIT_REFRESH_REASON: &str =
        "an explicit refresh invalidated the previous source stream";
    engine.now_ms = 20;
    engine
        .on_health(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Unavailable,
            reason: Some(EXPLICIT_REFRESH_REASON.into()),
            at_ms: 20,
            provenance: Some(SourceStamp {
                session_epoch: 1,
                stream_id: "watch".into(),
                source_sequence: 2,
            }),
        })
        .unwrap();
    assert_eq!(refresh_scopes(&engine), vec![None]);
}
