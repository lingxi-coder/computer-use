//! Canonical builder and Core regressions using test-only Source facts.
//! No native pixels, AX handles, TIS readings or physical input are fabricated.

mod common;

use execution_core::{
    core::{CoreConfig, ExecutionCore},
    sim_backend::SimBackend,
};
use interaction_contracts::desktop::{
    binding::{DeviceContextWitness, LayoutIndependentSpecialKey, instance_fingerprint},
    device::{
        BackendKind, CapabilityManifest, CapabilityState, ObservationPurpose, ReceiptStatus,
        RootGrant, SourceFreshness,
    },
    domain::ExecutionDomain,
    ids::{AttemptId, ObjectRef, RequestId, RootTaskId, SemanticTargetRef},
    primitive::{Modifiers, Primitive, PrimitivePolicy},
    source::{
        AxActionName, CompositionState, FocusContext, FocusRelation, KernelProcessIdentity,
        MenuAnchorContinuity, MenuContext, MenuDrawingWindow, MenuOpenerAnchor,
        ObjectIdentityContinuity, Rect, SourceKeyboardContext, UiObject, UiValue,
    },
};
use interaction_engine::{
    environment::{
        binding::{Binding, BindingReducer},
        pack::AppPack,
        state_core::{EnvSnapshot, StateCore},
    },
    execution::{DeviceView, IntentSpec},
    reaction::skills::{ActionPlan, ExecutionPath},
};
use sim_world::app::{SimApp, SnapshotMeta};

fn fixture() -> (ExecutionCore<SimBackend>, DeviceView, StateCore, Binding) {
    let mut core = ExecutionCore::new(CoreConfig::production("focused-builder"), SimBackend::new());
    core.advance_to(100).unwrap();
    core.renew_lease(60_000);
    let mut view = DeviceView::new();
    view.install_manifest(CapabilityManifest {
        identity: core.identity(),
        backend: BackendKind::Macos,
        execution_domain: ExecutionDomain::ForegroundNative,
        policy: PrimitivePolicy::AtomicOnly,
        human_input: CapabilityState::Available,
        hit_test: CapabilityState::Available,
        input: CapabilityState::Available,
        domains: common::domain_capabilities_for_test_profile(ExecutionDomain::ForegroundNative),
        max_uninterruptible_ms: 100,
    });
    let identity = core.identity();
    let mut snapshot = SimApp::new().snapshot(&SnapshotMeta {
        snapshot_id: "focused-read-1".into(),
        session_epoch: identity.session_epoch,
        surface: identity.surface,
        clock_domain: identity.clock_domain,
        source_sequence: 1,
        observed_ms: 50,
    });
    let window = snapshot.windows[0].clone();
    let root = UiObject {
        instance_ref: ObjectRef::new("root").unwrap(),
        parent: None,
        window_id: window.window_id.clone(),
        role: "AXWindow".into(),
        subrole: Some("AXStandardWindow".into()),
        title: None,
        description: None,
        value: None,
        enabled: true,
        focused: false,
        frame: window.frame,
        actions: vec![],
        ax_action_names: None,
        in_view: true,
        group: None,
        secure: false,
        scroll: None,
        identity_continuity: ObjectIdentityContinuity::NotificationCovered,
    };
    let mut target = root.clone();
    target.instance_ref = ObjectRef::new("checkbox").unwrap();
    target.parent = Some(root.instance_ref.clone());
    target.role = "AXCheckBox".into();
    target.subrole = None;
    target.title = Some("Enabled".into());
    target.value = Some(UiValue::Bool(false));
    target.focused = true;
    target.frame = Rect {
        x: 40.0,
        y: 60.0,
        width: 100.0,
        height: 30.0,
    };
    let mut sibling = target.clone();
    sibling.instance_ref = ObjectRef::new("other-checkbox").unwrap();
    sibling.title = Some("Other".into());
    sibling.focused = false;
    sibling.frame.y += 50.0;
    snapshot.objects = vec![root, target.clone(), sibling];
    sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
    snapshot.window_context = None;
    snapshot.focus_context = Some(FocusContext {
        source_session_id: "focus-watch-1".into(),
        session_epoch: snapshot.session_epoch,
        process: KernelProcessIdentity {
            pid: snapshot.app.pid,
            start_seconds: 10,
            start_micros: 20,
        },
        origin_window_id: window.window_id,
        origin_window_frame: window.frame,
        target_instance: target.instance_ref.clone(),
        target_fingerprint: instance_fingerprint(&snapshot, &target),
        target_frame: target.frame,
        focused_instance: target.instance_ref.clone(),
        relation: FocusRelation::ExactControl,
        composition: CompositionState::NotApplicable,
        keyboard: SourceKeyboardContext {
            selected_input_source: None,
            underlying_keyboard_layout: None,
            keyboard_type: None,
            caps_lock: None,
        },
        verified_source_sequence: snapshot.source_sequence,
        verified_observed_ms: snapshot.observed_ms,
    });
    snapshot.validate().unwrap();
    let mut state = StateCore::new();
    state.set_identity(&core.identity(), false);
    state
        .admit(snapshot, 100, &AppPack::settings_fixture())
        .unwrap();
    let binding = fresh_binding(state.latest().unwrap());
    (core, view, state, binding)
}

fn fresh_binding(env: &EnvSnapshot) -> Binding {
    BindingReducer::new()
        .bind_instance(
            env,
            SemanticTargetRef::new("enabled").unwrap(),
            &env.snapshot.objects[1].instance_ref,
        )
        .unwrap()
}

fn build(
    view: &DeviceView,
    env: &EnvSnapshot,
    binding: &Binding,
) -> Result<interaction_contracts::desktop::device::Observation, &'static str> {
    view.bound_focused_observation(
        env,
        binding,
        SourceFreshness::Verified,
        3000,
        1000,
        Some(700),
        LayoutIndependentSpecialKey::Space,
    )
}

#[test]
fn current_source_builds_scoped_evidence_and_clips_root_attempt_frame_and_ttl() {
    let (mut core, view, state, binding) = fixture();
    let env = state.latest().unwrap();
    let observed = build(&view, env, &binding).unwrap();
    assert_eq!(observed.purpose, ObservationPurpose::FocusedControl);
    assert_eq!(observed.source_sequence, env.snapshot.source_sequence);
    assert_eq!(observed.observed_ms, env.snapshot.observed_ms);
    assert_eq!(
        observed.valid_until_ms, 700,
        "normal frame lease caps evidence"
    );
    let proof = observed.binding_proof.as_ref().unwrap();
    assert_eq!(
        proof.focus_scope.as_ref().unwrap().context,
        *env.snapshot.focus_context.as_ref().unwrap()
    );
    assert_eq!(proof.snapshot_id, env.snapshot.snapshot_id);
    assert_eq!(proof.environment_revision, env.revision);
    assert_eq!(proof.layer_revision, env.layer_revision);
    assert_eq!(proof.display_frame, env.snapshot.objects[1].frame);
    assert!(proof.menu_scope.is_none());
    assert!(matches!(
        &proof.device_context,
        DeviceContextWitness::WholeStructureV1 {
            focus: Some(witness),
            ..
        } if witness.origin_window_id
            == env.snapshot.focus_context.as_ref().unwrap().origin_window_id
            && witness.origin_window_frame
                == env.snapshot.focus_context.as_ref().unwrap().origin_window_frame
            && witness.target_frame
                == env.snapshot.focus_context.as_ref().unwrap().target_frame
            && witness.relation == env.snapshot.focus_context.as_ref().unwrap().relation
    ));
    let root = RootTaskId::new("root").unwrap();
    core.register_root(&RootGrant {
        root_id: root.as_str().into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 1000,
    })
    .unwrap();
    core.observe(observed.clone()).unwrap();
    core.grant_permit(&view.grant(&root, &observed, 9000))
        .unwrap();
    assert!(
        core.backend().events().is_empty(),
        "observing focus and granting authority must not submit input"
    );
    for name in ["focus", "layer", "binding", "authorization", "geometry"] {
        assert_eq!(observed.dependencies[name], view.dependencies()[name]);
    }
    let capped = view
        .bound_focused_observation(
            env,
            &binding,
            SourceFreshness::Verified,
            3000,
            250,
            Some(700),
            LayoutIndependentSpecialKey::Space,
        )
        .unwrap();
    assert_eq!(
        capped.valid_until_ms, 250,
        "immutable root/attempt deadline caps evidence"
    );
    let short = view
        .bound_focused_observation(
            env,
            &binding,
            SourceFreshness::Verified,
            100,
            1000,
            Some(700),
            LayoutIndependentSpecialKey::Space,
        )
        .unwrap();
    assert_eq!(
        short.valid_until_ms, 150,
        "age starts at actual Source time"
    );
    let grant = view.grant(&RootTaskId::new("root").unwrap(), &short, 9000);
    assert_eq!(grant.valid_until_ms, 150);
    for deadline in [0, 50] {
        assert!(
            view.bound_focused_observation(
                env,
                &binding,
                SourceFreshness::Verified,
                3000,
                deadline,
                Some(700),
                LayoutIndependentSpecialKey::Space
            )
            .is_err()
        );
    }
    assert!(
        view.bound_focused_observation(
            env,
            &binding,
            SourceFreshness::Verified,
            3000,
            1000,
            Some(50),
            LayoutIndependentSpecialKey::Space
        )
        .is_err()
    );
}

#[test]
fn canonical_focus_evidence_builds_one_atomic_key_intent_accepted_by_real_core() {
    let (mut core, view, state, binding) = fixture();
    let root = RootTaskId::new("root").unwrap();
    core.register_root(&RootGrant {
        root_id: root.as_str().into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 1000,
    })
    .unwrap();
    let observation = build(&view, state.latest().unwrap(), &binding).unwrap();
    core.observe(observation.clone()).unwrap();
    let permit = core
        .grant_permit(&view.grant(&root, &observation, 1000))
        .unwrap();
    let attempt = AttemptId::new("activated-key-attempt").unwrap();
    let mut plan = ActionPlan {
        binding,
        primitive: Primitive::KeyPress {
            key: "Space".into(),
            modifiers: Modifiers::default(),
        },
        path: ExecutionPath::Keyboard,
        note: "test-only registered nontext operation".into(),
    };
    let build_intent = |plan: &ActionPlan| {
        view.intent(&IntentSpec {
            attempt: &attempt,
            root: &root,
            task_revision: 1,
            root_epoch: 1,
            permit: &permit,
            observation: &observation,
            plan,
            sequence: 1,
            now_ms: 100,
            start_slack_ms: 100,
            duration_ms: 25,
        })
    };
    let request = build_intent(&plan).unwrap();
    let native_intent = request
        .intent
        .native()
        .expect("this regression covers a native intent");
    assert_eq!(native_intent.steps.len(), 1);
    assert_eq!(native_intent.steps[0].offset_ms, 0);
    assert_eq!(native_intent.attempt_id.as_deref(), Some(attempt.as_str()));
    assert_eq!(native_intent.binding_proof, observation.binding_proof);
    plan.primitive = Primitive::KeyPress {
        key: "Return".into(),
        modifiers: Modifiers::default(),
    };
    assert!(
        build_intent(&plan).is_err(),
        "another key cannot inherit the granted Space scope"
    );
    let id = request.intent.intent_id().to_owned();
    assert_eq!(core.submit(request.clone()).status, ReceiptStatus::Accepted);
    core.dispatch_next().unwrap();
    core.advance_to(300).unwrap();
    assert_eq!(
        core.query_receipt(1, &id).unwrap().status,
        ReceiptStatus::Completed
    );
    assert_eq!(core.backend().events().len(), 1);
    assert!(
        matches!(&core.backend().events()[0].primitive, Primitive::KeyPress { key, .. } if key == "Space")
    );
    core.submit(request);
    core.dispatch_next();
    assert_eq!(
        core.backend().events().len(),
        1,
        "the canonical intent is idempotent at Core"
    );
}

#[test]
fn current_focus_is_not_inferred_from_focused_bool_or_an_ordinary_binding() {
    let (_, view, state, binding) = fixture();
    let mut env = state.latest().unwrap().clone();
    env.snapshot.focus_context = None;
    assert!(env.snapshot.objects[1].focused);
    assert!(build(&view, &env, &binding).is_err());
    let ordinary = view
        .bound_observation(&env, &binding, SourceFreshness::Verified, 3000, 1000, None)
        .unwrap();
    assert_eq!(ordinary.purpose, ObservationPurpose::TargetInput);
    let ordinary_proof = ordinary.binding_proof.unwrap();
    assert!(ordinary_proof.focus_scope.is_none());
    assert!(matches!(
        &ordinary_proof.device_context,
        DeviceContextWitness::WholeStructureV1 { focus: None, .. }
    ));
    let env = state.latest().unwrap();
    let other = BindingReducer::new()
        .bind_instance(
            env,
            SemanticTargetRef::new("other").unwrap(),
            &env.snapshot.objects[2].instance_ref,
        )
        .unwrap();
    assert!(build(&view, env, &other).is_err());
    let ordinary = view
        .bound_observation(env, &binding, SourceFreshness::Verified, 3000, 1000, None)
        .unwrap();
    let ordinary_proof = ordinary.binding_proof.unwrap();
    assert!(
        ordinary_proof.focus_scope.is_none(),
        "ordinary input does not inherit Source focus authority"
    );
    assert!(matches!(
        ordinary_proof.device_context,
        DeviceContextWitness::WholeStructureV1 { focus: None, .. }
    ));
}

#[test]
fn focused_builder_rejects_stale_binding_even_if_normal_descriptor_can_refresh() {
    let (_, view, state, binding) = fixture();
    let mut env = state.latest().unwrap().clone();
    env.revision += 1;
    env.snapshot.snapshot_id = RequestId::new("focused-read-2").unwrap();
    env.snapshot.source_sequence += 1;
    env.snapshot.observed_ms += 10;
    let context = env.snapshot.focus_context.as_mut().unwrap();
    context.verified_source_sequence = env.snapshot.source_sequence;
    context.verified_observed_ms = env.snapshot.observed_ms;
    assert!(env.snapshot.validate().is_ok());
    assert!(build(&view, &env, &binding).is_err());
    assert!(
        view.bound_observation(&env, &binding, SourceFreshness::Verified, 3000, 1000, None)
            .is_ok()
    );
    let fresh = fresh_binding(&env);
    let current = build(&view, &env, &fresh).unwrap();
    assert_eq!(
        current.binding_proof.unwrap().environment_revision,
        env.revision
    );
}

#[test]
fn focused_builder_rejects_changed_source_scope_partial_secure_and_wrong_epoch() {
    let (_, view, state, binding) = fixture();
    let changes: Vec<fn(&mut EnvSnapshot)> = vec![
        |e| {
            e.snapshot.complete = false;
            e.snapshot.incomplete_reason = Some("partial".into());
        },
        |e| e.snapshot.objects[1].secure = true,
        |e| e.snapshot.objects[1].focused = false,
        |e| e.snapshot.objects[1].frame.x += 0.01,
        |e| e.snapshot.objects[1].title = Some("Changed".into()),
        |e| {
            e.snapshot.focus_context.as_mut().unwrap().focused_instance =
                ObjectRef::new("field-editor").unwrap()
        },
        |e| {
            e.snapshot
                .focus_context
                .as_mut()
                .unwrap()
                .verified_source_sequence += 1
        },
        |e| {
            e.snapshot
                .focus_context
                .as_mut()
                .unwrap()
                .verified_observed_ms += 1
        },
        |e| e.snapshot.focus_context.as_mut().unwrap().session_epoch += 1,
        |e| {
            e.snapshot
                .focus_context
                .as_mut()
                .unwrap()
                .origin_window_frame
                .x += 0.01
        },
        |e| e.snapshot.focus_context.as_mut().unwrap().composition = CompositionState::Unknown,
        |e| {
            e.snapshot.session_epoch += 1;
            e.snapshot.focus_context.as_mut().unwrap().session_epoch += 1;
        },
        |e| e.snapshot.surface.epoch += 1,
        |e| e.snapshot.clock_domain = "other/1/monotonic".into(),
        |e| e.layer_revision += 1,
        |e| {
            e.snapshot.objects[1].instance_ref = ObjectRef::new("same-label-replacement").unwrap();
            let context = e.snapshot.focus_context.as_mut().unwrap();
            context.target_instance = e.snapshot.objects[1].instance_ref.clone();
            context.focused_instance = context.target_instance.clone();
        },
    ];
    for (index, change) in changes.into_iter().enumerate() {
        let mut bad = state.latest().unwrap().clone();
        change(&mut bad);
        assert!(
            build(&view, &bad, &binding).is_err(),
            "accepted mutation {index}"
        );
    }
    for freshness in [SourceFreshness::Uncertain, SourceFreshness::Failed] {
        assert!(
            view.bound_focused_observation(
                state.latest().unwrap(),
                &binding,
                freshness,
                3000,
                1000,
                Some(700),
                LayoutIndependentSpecialKey::Space
            )
            .is_err()
        );
    }
}

#[test]
fn focused_builder_requires_normal_dependencies_and_preserves_extra_revocations() {
    let (_, mut view, state, binding) = fixture();
    let original = view.dependencies().clone();
    for name in ["focus", "layer", "binding", "authorization", "geometry"] {
        let mut missing = original.clone();
        missing.remove(name);
        view.replace_dependencies(missing);
        assert!(
            build(&view, state.latest().unwrap(), &binding).is_err(),
            "accepted missing {name}"
        );
        let mut unknown = original.clone();
        unknown.insert(name.into(), 0);
        view.replace_dependencies(unknown);
        assert!(
            build(&view, state.latest().unwrap(), &binding).is_err(),
            "accepted unknown {name}"
        );
    }
    view.replace_dependencies(original);
    view.note_dependency("custom-cancellation", 42);
    view.note_dependency("window", 8);
    let evidence = build(&view, state.latest().unwrap(), &binding).unwrap();
    assert_eq!(evidence.dependencies["custom-cancellation"], 42);
    assert!(
        !evidence.dependencies.contains_key("window"),
        "uses the normal target dependency domain"
    );
}

#[test]
fn actual_focus_does_not_supply_missing_native_frame_evidence() {
    let (core, mut view, state, binding) = fixture();
    let mut native = view.manifest().unwrap().clone();
    native.backend = BackendKind::Macos;
    view.install_manifest(native);
    // This is only the missing-evidence case: no synthetic native image or
    // positive StateCore frame verdict is supplied to the builder.
    let mut native_state = StateCore::new();
    native_state.set_identity(&core.identity(), true);
    native_state
        .admit(
            state.latest().unwrap().snapshot.clone(),
            100,
            &AppPack::settings_fixture(),
        )
        .unwrap();
    assert!(!native_state.usable(100, 3000));
    assert!(native_state.frame_deadline(100, 3000).is_none());
    assert!(
        view.bound_focused_observation(
            native_state.latest().unwrap(),
            &binding,
            SourceFreshness::Verified,
            3000,
            1000,
            native_state.frame_deadline(100, 3000),
            LayoutIndependentSpecialKey::Space
        )
        .is_err()
    );
}

#[test]
fn actual_menu_source_cannot_supply_focused_control_evidence() {
    let (_, view, state, binding) = fixture();
    let mut env = state.latest().unwrap().clone();
    // Publish a valid test menu read, with no normal focused-control record.
    let focus = env.snapshot.focus_context.take().unwrap();
    env.snapshot.objects[1].role = "AXMenuButton".into();
    env.snapshot.objects[1].actions = vec![AxActionName::ShowMenu];
    let opener = &env.snapshot.objects[1];
    let anchor = MenuOpenerAnchor {
        anchor_id: "menu-anchor".into(),
        source_session_id: focus.source_session_id,
        anchor_revision: 1,
        process: focus.process,
        origin_window_id: focus.origin_window_id,
        origin_window_frame: focus.origin_window_frame,
        opener_instance: opener.instance_ref.clone(),
        opener_fingerprint: instance_fingerprint(&env.snapshot, opener),
        opener_frame: opener.frame,
        verified_source_sequence: env.snapshot.source_sequence,
        verified_observed_ms: env.snapshot.observed_ms,
        continuity: MenuAnchorContinuity::DirectRead,
    };
    let mut menu = env.snapshot.objects[2].clone();
    menu.instance_ref = ObjectRef::new("menu").unwrap();
    menu.role = "AXMenu".into();
    menu.parent = Some(anchor.opener_instance.clone());
    menu.title = None;
    menu.value = None;
    menu.frame = Rect {
        x: 40.0,
        y: 95.0,
        width: 120.0,
        height: 60.0,
    };
    let mut item = menu.clone();
    item.instance_ref = ObjectRef::new("menu-item").unwrap();
    item.parent = Some(menu.instance_ref.clone());
    item.role = "AXMenuItem".into();
    item.title = Some("Choice".into());
    item.actions = vec![AxActionName::Press];
    item.frame.height = 30.0;
    env.snapshot.objects.push(menu.clone());
    env.snapshot.objects.push(item);
    sim_world::app::refresh_simulated_observer_coverage(&mut env.snapshot);
    env.snapshot.menu_context = Some(MenuContext {
        context_id: "menu-context".into(),
        source_session_id: anchor.source_session_id.clone(),
        opener: anchor.clone(),
        menu_instance: menu.instance_ref.clone(),
        menu_fingerprint: instance_fingerprint(&env.snapshot, &menu),
        menu_frame: menu.frame,
        drawing_window: MenuDrawingWindow {
            window_id: 99,
            owner_pid: env.snapshot.app.pid,
            layer: 101,
            frame: menu.frame,
        },
    });
    env.snapshot.menu_anchors.push(anchor);
    assert!(
        env.snapshot.validate().is_ok(),
        "the actual menu-domain fixture is otherwise valid"
    );
    assert!(build(&view, &env, &binding).is_err());
    let mut window_binding = binding.clone();
    window_binding.rule = "window_activation".into();
    assert!(build(&view, state.latest().unwrap(), &window_binding).is_err());
    let mut menu_binding = binding;
    menu_binding.rule = "menu_item".into();
    assert!(build(&view, state.latest().unwrap(), &menu_binding).is_err());
}
