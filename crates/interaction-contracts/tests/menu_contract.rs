use std::collections::BTreeMap;

use interaction_contracts::desktop::{
    binding::{
        BindingProof, BindingProofScope, MenuBindingScope, MenuOpeningRef, derive_binding_evidence,
        instance_fingerprint,
    },
    device::{
        ActionIntent, CoordinateSpace, Observation, ObservationPurpose, PermitRef,
        ScheduledPrimitive, SourceFreshness, SurfaceRef,
    },
    domain::ExecutionDomain,
    ids::{AttemptId, BindingRef, ObjectRef, RequestId, SemanticTargetRef},
    planning::{PlanBundle, ScrollDirection, ScrollToSpec, SelectByLabelSpec, Subgoal},
    primitive::{AxActionKind, Modifiers, MouseButton, Primitive},
    source::{
        AppIdentity, AxActionName, GlobalFocusObservation, KernelProcessIdentity, LayerKind,
        MenuAnchorContinuity, MenuContext, MenuDrawingWindow, MenuOpenerAnchor,
        ObjectIdentityContinuity, ObservationFact, ObserverCoverage, Rect, SourceState,
        StructureSnapshot, UiObject, WindowInfo,
    },
    task::TargetSpec,
};

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn object(id: &str, parent: Option<&str>, role: &str, title: &str, frame: Rect) -> UiObject {
    UiObject {
        instance_ref: ObjectRef::new(id).unwrap(),
        parent: parent.map(|p| ObjectRef::new(p).unwrap()),
        window_id: "window-1".into(),
        role: role.into(),
        subrole: None,
        title: Some(title.into()),
        description: None,
        value: None,
        enabled: true,
        focused: false,
        frame,
        actions: vec![],
        ax_action_names: None,
        in_view: true,
        group: Some("Menu group".into()),
        secure: false,
        scroll: None,
        identity_continuity: ObjectIdentityContinuity::NotificationCovered,
    }
}

fn menu_snapshot() -> StructureSnapshot {
    let root_frame = rect(0.0, 0.0, 600.0, 400.0);
    let menu_frame = rect(40.0, 60.0, 180.0, 80.0);
    let mut root = object("root", None, "AXWindow", "Fixture", root_frame);
    root.subrole = Some("AXStandardWindow".into());
    root.group = None;
    let mut opener = object(
        "opener",
        Some("group"),
        "AXMenuButton",
        "Choice",
        rect(40.0, 30.0, 180.0, 30.0),
    );
    opener.actions = vec![AxActionName::ShowMenu];
    let mut item = object(
        "item",
        Some("menu"),
        "AXMenuItem",
        "Second option",
        rect(40.0, 90.0, 180.0, 24.0),
    );
    item.actions = vec![AxActionName::Press];
    let mut snapshot = StructureSnapshot {
        snapshot_id: RequestId::new("snapshot-1").unwrap(),
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
            bundle_id: "owned.app".into(),
            name: "Owned".into(),
            pid: 42,
        },
        windows: vec![WindowInfo {
            window_id: "window-1".into(),
            title: Some("Fixture".into()),
            frame: root_frame,
            z_index: 0,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: "owned.app".into(),
        }],
        global_focus: GlobalFocusObservation {
            app: ObservationFact::Known {
                value: AppIdentity {
                    bundle_id: "owned.app".into(),
                    name: "Owned".into(),
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
            covered_objects: 5,
            read_scoped_objects: 0,
            failed_registrations: 0,
            failures: vec![],
        },
        objects: vec![
            root,
            object("group", Some("root"), "AXGroup", "Menu group", root_frame),
            opener,
            object("menu", Some("opener"), "AXMenu", "", menu_frame),
            item,
        ],
        table_scope_search: None,
        table_viewports: vec![],
        menu_anchors: vec![],
        menu_context: None,
        focus_context: None,
        window_context: None,
    };
    let anchor = MenuOpenerAnchor {
        anchor_id: "anchor-1".into(),
        source_session_id: "watch-1".into(),
        anchor_revision: 1,
        process: KernelProcessIdentity {
            pid: 42,
            start_seconds: 1700000000,
            start_micros: 3,
        },
        origin_window_id: "window-1".into(),
        origin_window_frame: root_frame,
        opener_instance: snapshot.objects[2].instance_ref.clone(),
        opener_fingerprint: instance_fingerprint(&snapshot, &snapshot.objects[2]),
        opener_frame: snapshot.objects[2].frame,
        verified_source_sequence: snapshot.source_sequence,
        verified_observed_ms: snapshot.observed_ms,
        continuity: MenuAnchorContinuity::DirectRead,
    };
    snapshot.menu_context = Some(MenuContext {
        context_id: "context-1".into(),
        source_session_id: anchor.source_session_id.clone(),
        opener: anchor.clone(),
        menu_instance: snapshot.objects[3].instance_ref.clone(),
        menu_fingerprint: instance_fingerprint(&snapshot, &snapshot.objects[3]),
        menu_frame,
        drawing_window: MenuDrawingWindow {
            window_id: 2,
            owner_pid: 42,
            layer: 101,
            frame: menu_frame,
        },
    });
    snapshot.menu_anchors.push(anchor);
    snapshot
}

fn proof(item: bool) -> BindingProof {
    let snapshot = menu_snapshot();
    let object = &snapshot.objects[if item { 4 } else { 2 }];
    let evidence = derive_binding_evidence(&snapshot, object, BindingProofScope::WholeStructure)
        .expect("the fixture tree and Source menu context describe one closure");
    BindingProof {
        binding: BindingRef::new("binding-1").unwrap(),
        instance: object.instance_ref.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        environment_revision: 1,
        layer_revision: 1,
        fingerprint: instance_fingerprint(&snapshot, object),
        display_frame: object.frame,
        continuity: evidence.continuity,
        device_context: evidence.device_context,
        menu_scope: Some(if item {
            MenuBindingScope::Item {
                context: snapshot.menu_context.unwrap(),
                opened_by: MenuOpeningRef {
                    attempt_id: AttemptId::new("attempt-1").unwrap(),
                    intent_id: "opening-intent".into(),
                    attempt_deadline_ms: 1000,
                },
            }
        } else {
            MenuBindingScope::Opener {
                anchor: snapshot.menu_anchors[0].clone(),
                attempt_deadline_ms: 1000,
            }
        }),
        focus_scope: None,
    }
}

fn observation(item: bool) -> Observation {
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
        purpose: if item {
            ObservationPurpose::MenuSelection
        } else {
            ObservationPurpose::TargetInput
        },
        owned_document_proof: None,
        binding_proof: Some(proof(item)),
        dependencies: BTreeMap::new(),
    }
}

fn intent(item: bool) -> ActionIntent {
    let proof = proof(item);
    ActionIntent {
        intent_id: "selection-intent".into(),
        attempt_id: Some("attempt-1".into()),
        task_id: "task-1".into(),
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
        dependencies: BTreeMap::new(),
        evidence_id: "observation-1".into(),
        latest_start_ms: 100,
        duration_ms: 25,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: Primitive::AxAction {
                action: if item {
                    AxActionKind::Press
                } else {
                    AxActionKind::ShowMenu
                },
            },
        }],
    }
}

#[test]
fn complete_level_one_menu_and_normal_empty_menu_fields_validate() {
    let mut snapshot = menu_snapshot();
    assert!(snapshot.validate().is_ok());
    snapshot.objects.truncate(3);
    snapshot.observer_coverage.covered_objects = snapshot.objects.len() as u32;
    snapshot.menu_context = None;
    assert!(
        snapshot.validate().is_ok(),
        "a closed menu may retain its proven source opener anchor"
    );
    snapshot.menu_anchors.clear();
    assert!(
        snapshot.validate().is_ok(),
        "ordinary reads do not need menu proofs"
    );
    assert!(proof(false).validate().is_ok());
    assert!(proof(true).validate().is_ok());
    assert!(observation(false).validate().is_ok());
    assert!(observation(true).validate().is_ok());
    assert!(intent(false).validate().is_ok());
    assert!(intent(true).validate().is_ok());
}

#[test]
fn menu_snapshot_rejects_stale_provenance_missing_anchor_and_unsafe_roots() {
    let original = menu_snapshot();
    let changes: Vec<fn(&mut StructureSnapshot)> = vec![
        |s| s.source_sequence += 1,
        |s| s.observed_ms += 1,
        |s| s.app.pid += 1,
        |s| s.app.bundle_id = "foreign.app".into(),
        |s| s.global_focus.window_id = ObservationFact::Absent,
        |s| s.menu_anchors.clear(),
        |s| s.menu_context = None,
        |s| s.menu_context.as_mut().unwrap().opener.anchor_revision += 1,
        |s| s.menu_context.as_mut().unwrap().source_session_id = "new-watch".into(),
        |s| s.menu_anchors[0].verified_source_sequence += 1,
        |s| s.menu_anchors[0].verified_observed_ms += 1,
        |s| s.menu_anchors[0].source_session_id = "new-watch".into(),
        |s| s.windows[0].modal = true,
        |s| s.windows[0].kind = LayerKind::Sheet,
        |s| s.windows[0].frame.x += 0.01,
        |s| s.objects[0].subrole = Some("AXDialog".into()),
        |s| {
            s.complete = false;
            s.incomplete_reason = Some("partial".into());
        },
    ];
    for (index, change) in changes.into_iter().enumerate() {
        let mut bad = original.clone();
        change(&mut bad);
        assert!(bad.validate().is_err(), "accepted unsafe mutation {index}");
    }
}

#[test]
fn menu_snapshot_rejects_claimed_ancestors_unrelated_items_and_duplicate_menus() {
    let original = menu_snapshot();
    let changes: Vec<fn(&mut StructureSnapshot)> = vec![
        |s| s.objects[2].parent = Some(ObjectRef::new("absent").unwrap()),
        |s| s.objects[1].parent = Some(ObjectRef::new("opener").unwrap()),
        |s| s.objects[3].parent = Some(ObjectRef::new("group").unwrap()),
        |s| s.objects[4].parent = Some(ObjectRef::new("opener").unwrap()),
        |s| s.objects[4].window_id = "unknown-window".into(),
        |s| s.objects[4].secure = true,
        |s| s.objects[4].frame.x += 0.01,
        |s| s.objects[3].frame.x += 0.01,
        |s| s.objects[2].frame.x += 0.01,
        |s| s.objects[2].title = Some("Replacement".into()),
        |s| s.objects[2].actions.clear(),
        |s| s.objects[2].enabled = false,
        |s| s.objects[3].enabled = false,
        |s| s.objects[3].in_view = false,
        |s| {
            let mut menu = s.objects[3].clone();
            menu.instance_ref = ObjectRef::new("submenu").unwrap();
            menu.parent = Some(s.objects[4].instance_ref.clone());
            s.objects.push(menu);
        },
        |s| {
            let mut bar = s.objects[1].clone();
            bar.instance_ref = ObjectRef::new("bar").unwrap();
            bar.role = "AXMenuBar".into();
            s.objects.push(bar);
        },
    ];
    for (index, change) in changes.into_iter().enumerate() {
        let mut bad = original.clone();
        change(&mut bad);
        assert!(
            bad.validate().is_err(),
            "accepted unsafe tree mutation {index}"
        );
    }
}

#[test]
fn menu_anchor_identity_ignores_only_fresh_provenance_and_registry_ids() {
    let base = menu_snapshot().menu_anchors.remove(0);
    let mut fresh = base.clone();
    fresh.verified_source_sequence += 1;
    fresh.verified_observed_ms += 1;
    fresh.opener_instance = ObjectRef::new("new-registry-opener").unwrap();
    assert!(base.same_identity(&fresh));
    let changes: Vec<fn(&mut MenuOpenerAnchor)> = vec![
        |a| a.anchor_id.push('x'),
        |a| a.source_session_id.push('x'),
        |a| a.anchor_revision += 1,
        |a| a.process.pid += 1,
        |a| a.process.start_seconds += 1,
        |a| a.process.start_micros += 1,
        |a| a.origin_window_id.push('x'),
        |a| a.origin_window_frame.x += 0.01,
        |a| a.opener_frame.x += 0.01,
        |a| a.opener_fingerprint.label.push('x'),
        |a| a.continuity = MenuAnchorContinuity::ObservedLifetime,
    ];
    for change in changes {
        let mut changed = base.clone();
        change(&mut changed);
        assert!(!base.same_identity(&changed));
    }
}

#[test]
fn menu_context_and_binding_identity_preserve_transaction_and_item_identity() {
    let base = proof(true);
    let mut fresh = base.clone();
    fresh.snapshot_id = RequestId::new("fresh-snapshot").unwrap();
    fresh.environment_revision += 1;
    let Some(MenuBindingScope::Item { context, .. }) = &mut fresh.menu_scope else {
        unreachable!()
    };
    context.opener.verified_source_sequence += 1;
    context.opener.verified_observed_ms += 1;
    context.opener.opener_instance = ObjectRef::new("new-registry-opener").unwrap();
    context.menu_instance = ObjectRef::new("new-registry-menu").unwrap();
    assert!(fresh.validate().is_ok());
    assert!(base.same_instance(&fresh));
    let changes: Vec<fn(&mut BindingProof)> = vec![
        |p| p.instance = ObjectRef::new("replacement-item").unwrap(),
        |p| p.display_frame.x += 0.01,
        |p| p.layer_revision += 1,
        |p| {
            if let Some(MenuBindingScope::Item { context, .. }) = &mut p.menu_scope {
                context.context_id.push('x');
            }
        },
        |p| {
            if let Some(MenuBindingScope::Item { context, .. }) = &mut p.menu_scope {
                context.drawing_window.window_id += 1;
            }
        },
        |p| {
            if let Some(MenuBindingScope::Item { opened_by, .. }) = &mut p.menu_scope {
                opened_by.intent_id.push('x');
            }
        },
        |p| p.menu_scope = None,
    ];
    for change in changes {
        let mut changed = base.clone();
        change(&mut changed);
        assert!(!base.same_instance(&changed));
    }
}

#[test]
fn menu_descriptors_reject_forged_roles_frames_processes_and_drawing_profiles() {
    let original = proof(true);
    let changes: Vec<fn(&mut BindingProof)> = vec![
        |p| p.fingerprint.role = "AXButton".into(),
        |p| p.fingerprint.ancestor_roles.push("AXMenuItem".into()),
        |p| p.menu_scope = None,
        |p| {
            if let Some(MenuBindingScope::Item { context, .. }) = &mut p.menu_scope {
                context.drawing_window.owner_pid += 1;
            }
        },
        |p| {
            if let Some(MenuBindingScope::Item { context, .. }) = &mut p.menu_scope {
                context.drawing_window.layer = 102;
            }
        },
        |p| {
            if let Some(MenuBindingScope::Item { context, .. }) = &mut p.menu_scope {
                context.opener.process.start_micros = 1_000_000;
            }
        },
        |p| {
            if let Some(MenuBindingScope::Item { context, .. }) = &mut p.menu_scope {
                context.opener.process.start_seconds = 0;
            }
        },
        |p| {
            if let Some(MenuBindingScope::Item { context, .. }) = &mut p.menu_scope {
                context.menu_frame.x = f64::NAN;
            }
        },
        |p| {
            if let Some(MenuBindingScope::Item { context, .. }) = &mut p.menu_scope {
                context.source_session_id = "different".into();
            }
        },
        |p| {
            if let Some(MenuBindingScope::Item { opened_by, .. }) = &mut p.menu_scope {
                opened_by.intent_id = "bad\nintent".into();
            }
        },
    ];
    for (index, change) in changes.into_iter().enumerate() {
        let mut bad = original.clone();
        change(&mut bad);
        assert!(
            bad.validate().is_err(),
            "accepted descriptor mutation {index}"
        );
    }
    let mut opener = proof(false);
    opener.fingerprint.role = "AXButton".into();
    assert!(opener.validate().is_err());
}

#[test]
fn menu_purpose_is_explicit_and_requires_current_item_provenance() {
    let mut input = observation(true);
    for purpose in [
        ObservationPurpose::TargetInput,
        ObservationPurpose::WindowActivation,
    ] {
        input.purpose = purpose;
        assert!(input.validate().is_err());
    }
    input = observation(true);
    input.source_sequence += 1;
    assert!(input.validate().is_err());
    input = observation(true);
    input.observed_ms += 1;
    assert!(input.validate().is_err());
    input = observation(true);
    input.binding_proof = None;
    assert!(input.validate().is_err());
    input = observation(false);
    input.purpose = ObservationPurpose::MenuSelection;
    assert!(input.validate().is_err());
}

#[test]
fn menu_item_scope_rejects_every_primitive_except_one_zero_offset_press() {
    let wrong = vec![
        Primitive::Click {
            button: MouseButton::Left,
            x: 80,
            y: 100,
            modifiers: Modifiers::default(),
        },
        Primitive::PointerPosition { x: 80, y: 100 },
        Primitive::Scroll { dx: 0, dy: -40 },
        Primitive::AxAction {
            action: AxActionKind::ShowMenu,
        },
        Primitive::AxAction {
            action: AxActionKind::SetValue {
                value: "Second option".into(),
            },
        },
        Primitive::AxAction {
            action: AxActionKind::ScrollToVisible,
        },
        Primitive::ActivateWindow {
            app: "owned.app".into(),
        },
        Primitive::KeyPress {
            key: "Return".into(),
            modifiers: Modifiers::default(),
        },
        Primitive::TypeText {
            text: "Second option".into(),
        },
    ];
    for primitive in wrong {
        let mut bad = intent(true);
        bad.steps[0].primitive = primitive;
        assert!(bad.validate().is_err());
    }
    let mut bad = intent(true);
    bad.steps[0].offset_ms = 1;
    assert!(bad.validate().is_err());
    bad = intent(true);
    let mut second = bad.steps[0].clone();
    second.offset_ms = 1;
    bad.steps.push(second);
    assert!(bad.validate().is_err());
    bad = intent(true);
    bad.attempt_id = None;
    assert!(bad.validate().is_err());
    bad = intent(true);
    bad.attempt_id = Some("different-attempt".into());
    assert!(bad.validate().is_err());
    bad = intent(true);
    bad.intent_id = "opening-intent".into();
    assert!(bad.validate().is_err());
    bad = intent(false);
    bad.steps[0].primitive = Primitive::AxAction {
        action: AxActionKind::Press,
    };
    assert!(bad.validate().is_err());
}

#[test]
fn menu_attempt_deadline_is_immutable_and_bounds_evidence_and_each_input() {
    for item in [false, true] {
        let base = proof(item);
        let mut expired = base.clone();
        match expired.menu_scope.as_mut().unwrap() {
            MenuBindingScope::Opener {
                attempt_deadline_ms,
                ..
            } => *attempt_deadline_ms = 100,
            MenuBindingScope::Item { opened_by, .. } => opened_by.attempt_deadline_ms = 100,
        }
        assert!(expired.validate().is_err());
        let mut extended = base.clone();
        match extended.menu_scope.as_mut().unwrap() {
            MenuBindingScope::Opener {
                attempt_deadline_ms,
                ..
            } => *attempt_deadline_ms += 1,
            MenuBindingScope::Item { opened_by, .. } => opened_by.attempt_deadline_ms += 1,
        }
        assert!(extended.validate().is_ok());
        assert!(
            !base.same_instance(&extended),
            "new evidence cannot extend the activated attempt"
        );
        let mut older = base.clone();
        match older.menu_scope.as_mut().unwrap() {
            MenuBindingScope::Opener { anchor, .. } => anchor.verified_source_sequence -= 1,
            MenuBindingScope::Item { context, .. } => context.opener.verified_source_sequence -= 1,
        }
        assert!(
            !base.same_instance(&older),
            "menu provenance cannot run backwards"
        );
        let mut bad_observation = observation(item);
        bad_observation.valid_until_ms = 1001;
        assert!(bad_observation.validate().is_err());
        let mut bad_input = intent(item);
        bad_input.latest_start_ms = 990;
        bad_input.duration_ms = 11;
        assert!(bad_input.validate().is_err());
        bad_input.duration_ms = 10;
        assert!(bad_input.validate().is_ok());
    }
}

#[test]
fn menu_anchor_table_is_bounded_and_rejects_duplicate_current_openers() {
    let mut snapshot = menu_snapshot();
    snapshot.menu_anchors = vec![snapshot.menu_anchors[0].clone(); 9];
    assert!(snapshot.validate().is_err());
    snapshot = menu_snapshot();
    snapshot.menu_anchors.push(snapshot.menu_anchors[0].clone());
    assert!(snapshot.validate().is_err());
    snapshot = menu_snapshot();
    let mut duplicate = snapshot.menu_anchors[0].clone();
    duplicate.anchor_id.push('x');
    snapshot.menu_anchors.push(duplicate);
    assert!(snapshot.validate().is_err());
}

fn plan() -> PlanBundle {
    PlanBundle {
        subgoals: vec![Subgoal {
            id: "select".into(),
            description: "Select a registered choice".into(),
            skill: "ui.select_by_label".into(),
            target: Some(SemanticTargetRef::new("choice").unwrap()),
            expects: vec!["value".into()],
            scroll: None,
            select: Some(SelectByLabelSpec {
                choice_label: "Second option".into(),
            }),
        }],
        dependencies: vec![],
        required_observations: vec![],
        expected_effects: vec![],
        recovery_branches: vec![],
        termination: vec![],
    }
}

#[test]
fn menu_choice_is_typed_bounded_and_cannot_be_attached_to_another_skill() {
    assert!(plan().validate().is_ok());
    for label in [
        "".into(),
        "   ".into(),
        "x".repeat(513),
        "选".repeat(171),
        "bad\nchoice".into(),
        "bad\tchoice".into(),
        "bad\0choice".into(),
    ] {
        let mut bad = plan();
        bad.subgoals[0].select.as_mut().unwrap().choice_label = label;
        assert!(bad.validate().is_err());
    }
    assert!(
        SelectByLabelSpec {
            choice_label: "x".repeat(512)
        }
        .validate()
        .is_ok()
    );
    let mut bad = plan();
    bad.subgoals[0].skill = "ui.ensure_value".into();
    assert!(bad.validate().is_err());
    bad = plan();
    bad.subgoals[0].select = None;
    assert!(bad.validate().is_err());
    bad = plan();
    bad.subgoals[0].target = None;
    assert!(bad.validate().is_err());
    bad = plan();
    bad.subgoals[0].scroll = Some(ScrollToSpec {
        container: TargetSpec {
            row: None,
            semantic_target: SemanticTargetRef::new("container").unwrap(),
            label: "List".into(),
            group: None,
            role_hint: Some("AXScrollArea".into()),
        },
        direction: ScrollDirection::Down,
        pixels: 40,
        max_actions: 2,
    });
    assert!(
        bad.validate().is_err(),
        "menu selection cannot also acquire a scroll profile"
    );
    let mut unknown = serde_json::to_value(plan()).unwrap();
    unknown["subgoals"][0]["select"]["anchor_id"] = "model-chosen".into();
    assert!(
        serde_json::from_value::<PlanBundle>(unknown).is_err(),
        "planner cannot attach source authority"
    );
}

#[test]
fn menu_wire_tags_are_closed_and_do_not_change_fp1() {
    let base = proof(true);
    let value = serde_json::to_value(&base).unwrap();
    assert_eq!(value["menu_scope"]["kind"], "item");
    assert_eq!(
        value["menu_scope"]["context"]["opener"]["continuity"],
        "direct_read"
    );
    assert_eq!(value["menu_scope"]["opened_by"]["attempt_id"], "attempt-1");
    let roundtrip: BindingProof = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(base, roundtrip);
    let fp = base.fingerprint.clone();
    assert_eq!(base.fingerprint.digest(), fp.digest());
    let mut extra = value.clone();
    extra["menu_scope"]["context"]["extra_authority"] = true.into();
    assert!(serde_json::from_value::<BindingProof>(extra).is_err());
    let mut bad = value;
    bad["menu_scope"]["kind"] = "submenu".into();
    assert!(serde_json::from_value::<BindingProof>(bad).is_err());
}
