//! Value-contract tests only. These facts are fixture Source readings, not
//! native AX/TIS probes, input delivery evidence or keyboard capability grants.

use std::collections::BTreeMap;

use interaction_contracts::desktop::{
    binding::{
        BindingProof, BindingProofScope, FocusedControlScope, FocusedKeyProfile,
        LayoutIndependentSpecialKey, MenuBindingScope, derive_focused_binding_evidence,
        instance_fingerprint,
    },
    device::{
        ActionIntent, CoordinateSpace, Observation, ObservationPurpose, PermitRef,
        ScheduledPrimitive, SourceFreshness, SurfaceRef,
    },
    domain::ExecutionDomain,
    ids::{BindingRef, ObjectRef, RequestId},
    primitive::{AxActionKind, Modifiers, MouseButton, Primitive},
    source::{
        AppIdentity, CompositionProof, CompositionProofKind, CompositionState, FocusContext,
        FocusRelation, GlobalFocusObservation, KernelProcessIdentity, LayerKind,
        MenuAnchorContinuity, MenuOpenerAnchor, ObjectIdentityContinuity, ObservationFact,
        ObserverCoverage, Rect, SourceInputSource, SourceKeyboardContext, SourceState,
        SourceWindowContext, StructureSnapshot, UiObject, UiValue, WindowInfo,
    },
};

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn snapshot() -> StructureSnapshot {
    let root_frame = rect(0.0, 0.0, 600.0, 400.0);
    let root = UiObject {
        instance_ref: ObjectRef::new("root").unwrap(),
        parent: None,
        window_id: "window-1".into(),
        role: "AXWindow".into(),
        subrole: Some("AXStandardWindow".into()),
        title: None,
        description: None,
        value: None,
        enabled: true,
        focused: false,
        frame: root_frame,
        actions: vec![],
        ax_action_names: None,
        in_view: true,
        group: None,
        secure: false,
        scroll: None,
        identity_continuity: ObjectIdentityContinuity::NotificationCovered,
    };
    let mut target = root.clone();
    target.instance_ref = ObjectRef::new("receiver").unwrap();
    target.parent = Some(root.instance_ref.clone());
    target.role = "AXCheckBox".into();
    target.subrole = None;
    target.title = Some("Enabled".into());
    target.value = Some(UiValue::Bool(false));
    target.focused = true;
    target.frame = rect(40.0, 60.0, 100.0, 30.0);
    let mut snapshot = StructureSnapshot {
        snapshot_id: RequestId::new("read-1").unwrap(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "native/1/monotonic".into(),
        identity_boundary: None,
        frame_ref: None,
        owned_document_evidence: None,
        source_sequence: 2,
        observed_ms: 100,
        app: AppIdentity {
            bundle_id: "owned.fixture".into(),
            name: "Fixture".into(),
            pid: 42,
        },
        windows: vec![WindowInfo {
            window_id: "window-1".into(),
            title: Some("Fixture".into()),
            frame: root_frame,
            z_index: 0,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: "owned.fixture".into(),
        }],
        global_focus: GlobalFocusObservation {
            app: ObservationFact::Known {
                value: AppIdentity {
                    bundle_id: "owned.fixture".into(),
                    name: "Fixture".into(),
                    pid: 42,
                },
            },
            window_id: ObservationFact::Known {
                value: "window-1".into(),
            },
        },
        target_context: None,
        complete: true,
        incomplete_reason: None,
        observer_coverage: ObserverCoverage {
            state: SourceState::Available,
            covered_objects: 2,
            read_scoped_objects: 0,
            failed_registrations: 0,
            failures: vec![],
        },
        objects: vec![root, target],
        table_scope_search: None,
        table_viewports: vec![],
        menu_anchors: vec![],
        menu_context: None,
        focus_context: None,
        window_context: None,
    };
    let target = &snapshot.objects[1];
    snapshot.focus_context = Some(FocusContext {
        source_session_id: "watch-1".into(),
        session_epoch: 1,
        process: KernelProcessIdentity {
            pid: 42,
            start_seconds: 10,
            start_micros: 20,
        },
        origin_window_id: "window-1".into(),
        origin_window_frame: root_frame,
        target_instance: target.instance_ref.clone(),
        target_fingerprint: instance_fingerprint(&snapshot, target),
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
        verified_source_sequence: 2,
        verified_observed_ms: 100,
    });
    snapshot
}

fn scope() -> FocusedControlScope {
    FocusedControlScope {
        context: snapshot().focus_context.unwrap(),
        profile: FocusedKeyProfile::MacosAtomicSpecialKeyV1,
        key: LayoutIndependentSpecialKey::Space,
    }
}

fn proof() -> BindingProof {
    let source = snapshot();
    let scope = scope();
    let target = source
        .objects
        .iter()
        .find(|object| object.instance_ref == scope.context.target_instance)
        .unwrap();
    let evidence =
        derive_focused_binding_evidence(&source, target, BindingProofScope::WholeStructure)
            .expect("the fixture snapshot supplies the exact focused Source closure");
    BindingProof {
        binding: BindingRef::new("binding-1").unwrap(),
        instance: scope.context.target_instance.clone(),
        snapshot_id: source.snapshot_id.clone(),
        environment_revision: 1,
        layer_revision: 1,
        fingerprint: scope.context.target_fingerprint.clone(),
        display_frame: scope.context.target_frame,
        continuity: evidence.continuity,
        device_context: evidence.device_context,
        menu_scope: None,
        focus_scope: Some(scope),
    }
}

fn dependencies() -> BTreeMap<String, u64> {
    ["authorization", "binding", "focus", "geometry", "layer"]
        .into_iter()
        .map(|name| (name.into(), 1))
        .collect()
}

fn observation() -> Observation {
    Observation {
        observation_id: "observation-1".into(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        target_ref: "binding-1".into(),
        clock_domain: "native/1/monotonic".into(),
        source_sequence: 2,
        observed_ms: 100,
        valid_until_ms: 200,
        freshness: SourceFreshness::Verified,
        execution_domain: ExecutionDomain::ForegroundNative,
        purpose: ObservationPurpose::FocusedControl,
        owned_document_proof: None,
        binding_proof: Some(proof()),
        dependencies: dependencies(),
    }
}

fn intent() -> ActionIntent {
    let proof = proof();
    ActionIntent {
        intent_id: "intent-1".into(),
        attempt_id: Some("attempt-1".into()),
        task_id: "root-1".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: 1,
        execution_domain: ExecutionDomain::ForegroundNative,
        permit: PermitRef {
            id: "permit-1".into(),
            generation: 1,
        },
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        target_ref: "binding-1".into(),
        binding_fingerprint: Some(proof.fingerprint.digest()),
        owned_document_proof: None,
        binding_proof: Some(proof),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: dependencies(),
        evidence_id: "observation-1".into(),
        latest_start_ms: 150,
        duration_ms: 25,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: Primitive::KeyPress {
                key: "Space".into(),
                modifiers: Modifiers::default(),
            },
        }],
    }
}

#[test]
fn complete_exact_control_and_scoped_atomic_key_contracts_validate() {
    assert!(snapshot().validate().is_ok());
    assert!(scope().validate().is_ok());
    assert!(proof().validate().is_ok());
    assert!(observation().validate().is_ok());
    assert!(intent().validate().is_ok());
    let mut ordinary = snapshot();
    ordinary.focus_context = None;
    assert!(
        ordinary.validate().is_ok(),
        "absence does not fabricate positive focus evidence"
    );
    let mut old_ax = intent();
    old_ax.binding_proof.as_mut().unwrap().focus_scope = None;
    old_ax.steps[0].primitive = Primitive::AxAction {
        action: AxActionKind::Press,
    };
    assert!(old_ax.validate().is_ok());
    let mut old_observation = observation();
    old_observation.purpose = ObservationPurpose::TargetInput;
    old_observation.binding_proof.as_mut().unwrap().focus_scope = None;
    assert!(old_observation.validate().is_ok());
}

#[test]
fn focused_source_recomputes_actual_target_and_requires_complete_standard_window() {
    let changes: Vec<fn(&mut StructureSnapshot)> = vec![
        |s| s.source_sequence += 1,
        |s| s.observed_ms += 1,
        |s| s.session_epoch += 1,
        |s| s.app.pid += 1,
        |s| s.app.bundle_id.push('x'),
        |s| s.global_focus.window_id = ObservationFact::Absent,
        |s| {
            s.complete = false;
            s.incomplete_reason = Some("partial".into());
        },
        |s| s.objects[1].focused = false,
        |s| s.objects[1].enabled = false,
        |s| s.objects[1].in_view = false,
        |s| s.objects[1].secure = true,
        |s| s.objects[1].title = Some("Replacement".into()),
        |s| s.objects[1].frame.x += 0.01,
        |s| s.objects[1].group = Some("Other".into()),
        |s| s.objects[1].parent = None,
        |s| s.objects[1].parent = Some(s.objects[1].instance_ref.clone()),
        |s| s.objects[0].secure = true,
        |s| s.objects[0].subrole = Some("AXDialog".into()),
        |s| s.windows[0].frame.x += 0.01,
        |s| s.windows[0].modal = true,
        |s| s.windows[0].kind = LayerKind::Sheet,
        |s| s.windows[0].owner_bundle_id.push('x'),
        |s| {
            s.focus_context.as_mut().unwrap().focused_instance =
                ObjectRef::new("field-editor").unwrap()
        },
        |s| s.focus_context.as_mut().unwrap().process.start_seconds = 0,
        |s| s.focus_context.as_mut().unwrap().process.start_micros = 1_000_000,
        |s| s.focus_context.as_mut().unwrap().source_session_id = "bad\nwatch".into(),
    ];
    for (index, change) in changes.into_iter().enumerate() {
        let mut bad = snapshot();
        change(&mut bad);
        assert!(
            bad.validate().is_err(),
            "accepted invalid Source mutation {index}"
        );
    }
}

#[test]
fn focus_cannot_coexist_with_menu_evidence_or_menu_layers() {
    let mut bad = snapshot();
    let mut menu = bad.objects[1].clone();
    menu.instance_ref = ObjectRef::new("menu").unwrap();
    menu.role = "AXMenu".into();
    bad.objects.push(menu);
    assert!(bad.validate().is_err());
    let mut bad = proof();
    let context = &bad.focus_scope.as_ref().unwrap().context;
    bad.menu_scope = Some(MenuBindingScope::Opener {
        anchor: MenuOpenerAnchor {
            anchor_id: "opener".into(),
            source_session_id: context.source_session_id.clone(),
            anchor_revision: 1,
            process: context.process.clone(),
            origin_window_id: context.origin_window_id.clone(),
            origin_window_frame: context.origin_window_frame,
            opener_instance: context.target_instance.clone(),
            opener_fingerprint: context.target_fingerprint.clone(),
            opener_frame: context.target_frame,
            verified_source_sequence: context.verified_source_sequence,
            verified_observed_ms: context.verified_observed_ms,
            continuity: MenuAnchorContinuity::DirectRead,
        },
        attempt_deadline_ms: 1000,
    });
    assert_eq!(
        bad.validate().unwrap_err(),
        "menu and focused-control scopes are mutually exclusive"
    );
}

#[test]
fn focus_purpose_requires_verified_exact_source_scope_and_cannot_mix_domains() {
    for purpose in [
        ObservationPurpose::TargetInput,
        ObservationPurpose::WindowActivation,
        ObservationPurpose::MenuSelection,
    ] {
        let mut bad = observation();
        bad.purpose = purpose;
        assert!(bad.validate().is_err());
    }
    let changes: Vec<fn(&mut Observation)> = vec![
        |o| o.binding_proof = None,
        |o| o.binding_proof.as_mut().unwrap().focus_scope = None,
        |o| o.freshness = SourceFreshness::Uncertain,
        |o| o.freshness = SourceFreshness::Failed,
        |o| o.session_epoch += 1,
        |o| o.source_sequence += 1,
        |o| o.observed_ms += 1,
        |o| o.target_ref = "other-binding".into(),
    ];
    for change in changes {
        let mut bad = observation();
        change(&mut bad);
        assert!(bad.validate().is_err());
    }
}

#[test]
fn focus_scope_authorizes_only_one_canonical_unmodified_atomic_key() {
    let wrong = vec![
        Primitive::Click {
            button: MouseButton::Left,
            x: 50,
            y: 70,
            modifiers: Modifiers::default(),
        },
        Primitive::PointerPosition { x: 50, y: 70 },
        Primitive::Scroll { dx: 0, dy: -40 },
        Primitive::AxAction {
            action: AxActionKind::Press,
        },
        Primitive::ActivateWindow {
            app: "owned.fixture".into(),
        },
        Primitive::TypeText { text: " ".into() },
        Primitive::KeyDown {
            key: "Space".into(),
        },
        Primitive::KeyUp {
            key: "Space".into(),
        },
    ];
    for primitive in wrong {
        let mut bad = intent();
        bad.steps[0].primitive = primitive;
        assert!(bad.validate().is_err());
    }
    for key in [
        "space", " Space", "Space ", " ", "49", "A", "Command", "Tab", "Space\0",
    ] {
        let mut bad = intent();
        bad.steps[0].primitive = Primitive::KeyPress {
            key: key.into(),
            modifiers: Modifiers::default(),
        };
        assert!(bad.validate().is_err(), "accepted {key:?}");
    }
    for modifiers in [
        Modifiers {
            shift: true,
            ..Default::default()
        },
        Modifiers {
            control: true,
            ..Default::default()
        },
        Modifiers {
            option: true,
            ..Default::default()
        },
        Modifiers {
            command: true,
            ..Default::default()
        },
    ] {
        let mut bad = intent();
        bad.steps[0].primitive = Primitive::KeyPress {
            key: "Space".into(),
            modifiers,
        };
        assert!(bad.validate().is_err());
    }
    let mut bad = intent();
    bad.steps[0].offset_ms = 1;
    assert!(bad.validate().is_err());
    bad = intent();
    let mut extra = bad.steps[0].clone();
    extra.offset_ms = 1;
    bad.steps.push(extra);
    assert!(bad.validate().is_err());
    bad = intent();
    bad.attempt_id = None;
    assert!(bad.validate().is_err());
    bad = intent();
    bad.session_epoch += 1;
    assert!(bad.validate().is_err());
    bad = intent();
    bad.latest_start_ms = 99;
    assert!(bad.validate().is_err());
}

#[test]
fn same_focused_instance_preserves_receiver_kernel_watch_geometry_and_key() {
    let base = proof();
    let mut fresh = base.clone();
    fresh.snapshot_id = RequestId::new("read-2").unwrap();
    fresh.environment_revision += 1;
    let context = &mut fresh.focus_scope.as_mut().unwrap().context;
    context.verified_source_sequence += 1;
    context.verified_observed_ms += 1;
    assert!(fresh.validate().is_ok());
    assert!(base.same_instance(&fresh));
    let changes: Vec<fn(&mut BindingProof)> = vec![
        |p| p.focus_scope = None,
        |p| p.layer_revision += 1,
        |p| p.environment_revision = 0,
        |p| p.instance = ObjectRef::new("replacement").unwrap(),
        |p| p.display_frame.x += 0.01,
        |p| p.focus_scope.as_mut().unwrap().key = LayoutIndependentSpecialKey::Tab,
        |p| {
            p.focus_scope
                .as_mut()
                .unwrap()
                .context
                .source_session_id
                .push('x')
        },
        |p| p.focus_scope.as_mut().unwrap().context.session_epoch += 1,
        |p| {
            p.focus_scope
                .as_mut()
                .unwrap()
                .context
                .process
                .start_seconds += 1
        },
        |p| p.focus_scope.as_mut().unwrap().context.process.start_micros += 1,
        |p| p.focus_scope.as_mut().unwrap().context.process.pid += 1,
        |p| {
            p.focus_scope
                .as_mut()
                .unwrap()
                .context
                .origin_window_id
                .push('x')
        },
        |p| {
            p.focus_scope
                .as_mut()
                .unwrap()
                .context
                .origin_window_frame
                .x += 0.01
        },
        |p| {
            p.focus_scope.as_mut().unwrap().context.target_instance =
                ObjectRef::new("same-label-replacement").unwrap()
        },
        |p| {
            p.focus_scope.as_mut().unwrap().context.focused_instance =
                ObjectRef::new("field-editor").unwrap()
        },
        |p| p.focus_scope.as_mut().unwrap().context.target_frame.x += 0.01,
        |p| {
            p.focus_scope
                .as_mut()
                .unwrap()
                .context
                .target_fingerprint
                .label
                .push('x')
        },
        |p| {
            p.focus_scope
                .as_mut()
                .unwrap()
                .context
                .verified_source_sequence -= 1
        },
        |p| p.focus_scope.as_mut().unwrap().context.verified_observed_ms -= 1,
        |p| p.focus_scope.as_mut().unwrap().context.composition = CompositionState::Unknown,
        |p| p.focus_scope.as_mut().unwrap().context.keyboard.caps_lock = Some(false),
    ];
    for (index, change) in changes.into_iter().enumerate() {
        let mut changed = base.clone();
        change(&mut changed);
        assert!(
            !base.same_instance(&changed),
            "accepted identity change {index}"
        );
    }
}

fn input_source(id: &str, ascii: bool, layout_hash: Option<String>) -> SourceInputSource {
    SourceInputSource {
        id: Some(id.into()),
        source_type: Some("input-source-type".into()),
        category: None,
        mode_id: None,
        ascii_capable: Some(ascii),
        layout_data_sha256: layout_hash,
    }
}

#[test]
fn selected_ime_and_underlying_ascii_layout_never_prove_composition_free() {
    let mut snapshot = snapshot();
    snapshot.objects[1].role = "AXTextField".into();
    snapshot.objects[1].value = Some(UiValue::Text("text".into()));
    let fp = instance_fingerprint(&snapshot, &snapshot.objects[1]);
    let context = snapshot.focus_context.as_mut().unwrap();
    context.target_fingerprint = fp;
    context.composition = CompositionState::Unknown;
    context.keyboard.selected_input_source = Some(input_source("actual.ime", false, None));
    context.keyboard.underlying_keyboard_layout =
        Some(input_source("actual.layout", true, Some("a".repeat(64))));
    let current_context = context.clone();
    assert!(snapshot.validate().is_ok());
    let mut scope = scope();
    scope.context = current_context.clone();
    assert!(scope.validate().is_err());
    scope.context.composition = CompositionState::NotApplicable;
    assert!(
        scope.validate().is_err(),
        "text is not a known nontext receiver"
    );
    let proof = CompositionProof {
        kind: CompositionProofKind::AxTextInputMarkedRange,
        focused_instance: current_context.focused_instance.clone(),
        verified_source_sequence: 2,
        verified_observed_ms: 100,
    };
    for state in [
        CompositionState::Marked {
            proof: proof.clone(),
        },
        CompositionState::Unmarked {
            proof: proof.clone(),
        },
    ] {
        scope.context.composition = state;
        assert!(
            scope.context.validate().is_ok(),
            "actual positive facts can be represented without opening a key profile"
        );
        assert!(
            scope.validate().is_err(),
            "initial nontext profile is not a generic text/IME profile"
        );
    }
    let marked = serde_json::to_value(CompositionState::Marked {
        proof: proof.clone(),
    })
    .unwrap();
    assert_eq!(marked["kind"], "marked");
    assert_eq!(marked["proof"]["kind"], "ax_text_input_marked_range");
    assert_eq!(
        serde_json::from_value::<CompositionState>(marked).unwrap(),
        CompositionState::Marked {
            proof: proof.clone()
        }
    );
    assert!(
        serde_json::from_value::<CompositionProofKind>(serde_json::json!(
            "ax_marked_text_marker_range"
        ))
        .is_err()
    );
    let mut wrong = proof;
    wrong.focused_instance = ObjectRef::new("unrelated-editor").unwrap();
    scope.context.composition = CompositionState::Unmarked { proof: wrong };
    assert!(scope.context.validate().is_err());
    let mut wrong_sequence = CompositionProof {
        kind: CompositionProofKind::AxTextInputMarkedRange,
        focused_instance: current_context.focused_instance.clone(),
        verified_source_sequence: current_context.verified_source_sequence + 1,
        verified_observed_ms: current_context.verified_observed_ms,
    };
    scope.context.composition = CompositionState::Unmarked {
        proof: wrong_sequence.clone(),
    };
    assert!(scope.context.validate().is_err());
    wrong_sequence.verified_source_sequence = current_context.verified_source_sequence;
    wrong_sequence.verified_observed_ms = current_context.verified_observed_ms + 1;
    scope.context.composition = CompositionState::Marked {
        proof: wrong_sequence,
    };
    assert!(scope.context.validate().is_err());
}

#[test]
fn current_window_context_remains_valid_without_a_focused_element_and_grants_no_focus() {
    let mut snapshot = snapshot();
    let focus = snapshot.focus_context.take().unwrap();
    snapshot.objects[1].focused = false;
    let window = SourceWindowContext {
        source_session_id: focus.source_session_id,
        session_epoch: focus.session_epoch,
        process: focus.process,
        origin_window_id: focus.origin_window_id,
        origin_window_frame: focus.origin_window_frame,
        verified_source_sequence: focus.verified_source_sequence,
        verified_observed_ms: focus.verified_observed_ms,
    };
    snapshot.window_context = Some(window.clone());
    assert!(
        snapshot.validate().is_ok(),
        "fresh actual window/kernel facts do not require a focused element"
    );
    let value = serde_json::to_value(&window).unwrap();
    assert_eq!(
        serde_json::from_value::<SourceWindowContext>(value.clone()).unwrap(),
        window
    );
    let mut extra = value;
    extra["focus_inferred"] = true.into();
    assert!(serde_json::from_value::<SourceWindowContext>(extra).is_err());
    let mut observation = observation();
    observation.binding_proof.as_mut().unwrap().focus_scope = None;
    assert!(
        observation.validate().is_err(),
        "a root witness alone is never focused-control authority"
    );
}

fn with_window_context() -> StructureSnapshot {
    let mut snapshot = snapshot();
    let focus = snapshot.focus_context.as_ref().unwrap();
    snapshot.window_context = Some(SourceWindowContext {
        source_session_id: focus.source_session_id.clone(),
        session_epoch: focus.session_epoch,
        process: focus.process.clone(),
        origin_window_id: focus.origin_window_id.clone(),
        origin_window_frame: focus.origin_window_frame,
        verified_source_sequence: focus.verified_source_sequence,
        verified_observed_ms: focus.verified_observed_ms,
    });
    snapshot
}

#[test]
fn current_window_context_requires_actual_current_complete_unique_standard_root() {
    assert!(with_window_context().validate().is_ok());
    let changes: Vec<fn(&mut StructureSnapshot)> = vec![
        |s| s.window_context.as_mut().unwrap().source_session_id = "bad\nwatch".into(),
        |s| s.window_context.as_mut().unwrap().source_session_id = "different-watch".into(),
        |s| s.window_context.as_mut().unwrap().session_epoch += 1,
        |s| s.window_context.as_mut().unwrap().process.pid += 1,
        |s| s.window_context.as_mut().unwrap().process.start_seconds += 1,
        |s| s.window_context.as_mut().unwrap().process.start_micros += 1,
        |s| s.window_context.as_mut().unwrap().origin_window_id = "other-window".into(),
        |s| s.window_context.as_mut().unwrap().origin_window_frame.x += 0.01,
        |s| s.window_context.as_mut().unwrap().verified_source_sequence += 1,
        |s| s.window_context.as_mut().unwrap().verified_observed_ms += 1,
        |s| {
            s.complete = false;
            s.incomplete_reason = Some("partial".into());
        },
        |s| s.windows[0].modal = true,
        |s| s.windows[0].kind = LayerKind::Sheet,
        |s| s.windows.push(s.windows[0].clone()),
        |s| s.objects[0].subrole = Some("AXDialog".into()),
        |s| s.objects[0].secure = true,
        |s| s.objects[0].in_view = false,
        |s| {
            let mut root = s.objects[0].clone();
            root.instance_ref = ObjectRef::new("duplicate-root").unwrap();
            s.objects.push(root);
        },
        |s| {
            let mut menu = s.objects[1].clone();
            menu.instance_ref = ObjectRef::new("menu").unwrap();
            menu.role = "AXMenu".into();
            s.objects.push(menu);
        },
    ];
    for (index, change) in changes.into_iter().enumerate() {
        let mut bad = with_window_context();
        change(&mut bad);
        assert!(
            bad.validate().is_err(),
            "accepted invalid window witness {index}"
        );
    }
    let mut no_focus = with_window_context();
    no_focus.focus_context = None;
    no_focus.objects[1].focused = false;
    assert!(no_focus.validate().is_ok());
    no_focus
        .window_context
        .as_mut()
        .unwrap()
        .process
        .start_seconds = 0;
    assert!(no_focus.validate().is_err());
}

#[test]
fn keyboard_fact_wire_bounds_and_closed_focus_tags_are_enforced() {
    for fact in [
        input_source("bad\nsource", true, None),
        input_source(&"x".repeat(257), true, None),
        input_source("layout", true, Some("A".repeat(64))),
        input_source("layout", true, Some("a".repeat(65))),
        input_source("layout", true, Some("g".repeat(64))),
    ] {
        assert!(fact.validate().is_err());
    }
    let empty = SourceInputSource {
        id: None,
        source_type: None,
        category: None,
        mode_id: None,
        ascii_capable: None,
        layout_data_sha256: None,
    };
    assert!(empty.validate().is_err());
    let value = serde_json::to_value(proof()).unwrap();
    assert_eq!(
        value["focus_scope"]["profile"],
        "macos_atomic_special_key_v1"
    );
    assert_eq!(value["focus_scope"]["key"], "Space");
    assert_eq!(value["focus_scope"]["context"]["relation"], "exact_control");
    assert_eq!(
        value["focus_scope"]["context"]["composition"]["kind"],
        "not_applicable"
    );
    assert_eq!(
        serde_json::from_value::<BindingProof>(value.clone()).unwrap(),
        proof()
    );
    let mut bad = value.clone();
    bad["focus_scope"]["context"]["relation"] = "mouse_point_hit".into();
    assert!(serde_json::from_value::<BindingProof>(bad).is_err());
    let mut bad = value.clone();
    bad["focus_scope"]["profile"] = "arbitrary_hotkey".into();
    assert!(serde_json::from_value::<BindingProof>(bad).is_err());
    let mut bad = value.clone();
    bad["focus_scope"]["key"] = "space".into();
    assert!(serde_json::from_value::<BindingProof>(bad).is_err());
    let mut bad = value.clone();
    bad["focus_scope"]["context"]["composition"] = serde_json::json!({"kind":"unmarked"});
    assert!(serde_json::from_value::<BindingProof>(bad).is_err());
    let mut bad = value;
    bad["focus_scope"]["context"]["keyboard"]["assumed_us"] = true.into();
    assert!(serde_json::from_value::<BindingProof>(bad).is_err());
    assert_eq!(
        proof().fingerprint.digest(),
        scope().context.target_fingerprint.digest(),
        "focus does not change fp1"
    );
}
