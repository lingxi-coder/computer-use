use interaction_contracts::desktop::binding::{
    BindingProof, BindingProofScope, FocusedControlScope, MenuBindingScope,
    derive_binding_evidence, derive_focused_binding_evidence, instance_fingerprint,
};
use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::ids::{BindingRef, ObjectRef, RequestId};
use interaction_contracts::desktop::source::{
    AppIdentity, GlobalFocusObservation, LayerKind, ObjectIdentityContinuity, ObservationFact,
    ObserverCoverage, Rect, SourceState, StructureSnapshot, UiObject, WindowInfo,
};

const APP: &str = "test.app";
const WINDOW: &str = "window-1";

pub fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Make an exact synthetic Source tree for proof fixtures. Binding evidence is
/// compiled from this snapshot so the public facts and Source commitment stay
/// in lockstep.
pub fn snapshot_with_target(
    instance: &str,
    source_sequence: u64,
    role: &str,
    subrole: Option<&str>,
    label: &str,
    frame: Rect,
    continuity: ObjectIdentityContinuity,
) -> StructureSnapshot {
    let window_frame = rect(0.0, 0.0, 800.0, 600.0);
    let root = ui_object(
        "window-root",
        None,
        "AXWindow",
        Some("AXStandardWindow"),
        Some("Settings"),
        window_frame,
        ObjectIdentityContinuity::NotificationCovered,
    );
    let mut target = ui_object(
        instance,
        Some("window-root"),
        role,
        subrole,
        Some(label),
        frame,
        continuity,
    );
    target.enabled = true;
    let sibling = ui_object(
        "sibling",
        Some("window-root"),
        "AXStaticText",
        None,
        Some("Another control"),
        rect(150.0, 40.0, 120.0, 24.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    snapshot_from_objects(vec![root, target, sibling], source_sequence)
}

pub fn ui_object(
    instance: &str,
    parent: Option<&str>,
    role: &str,
    subrole: Option<&str>,
    title: Option<&str>,
    frame: Rect,
    continuity: ObjectIdentityContinuity,
) -> UiObject {
    UiObject {
        instance_ref: ObjectRef::new(instance).unwrap(),
        parent: parent.map(|parent| ObjectRef::new(parent).unwrap()),
        window_id: WINDOW.into(),
        role: role.into(),
        subrole: subrole.map(str::to_owned),
        title: title.map(str::to_owned),
        description: None,
        value: None,
        enabled: true,
        focused: false,
        frame,
        actions: vec![],
        ax_action_names: None,
        in_view: true,
        group: None,
        secure: false,
        scroll: None,
        identity_continuity: continuity,
    }
}

pub fn snapshot_from_objects(objects: Vec<UiObject>, source_sequence: u64) -> StructureSnapshot {
    let covered_objects = objects
        .iter()
        .filter(|object| {
            object.identity_continuity == ObjectIdentityContinuity::NotificationCovered
        })
        .count() as u32;
    let read_scoped_objects = objects.len() as u32 - covered_objects;
    StructureSnapshot {
        snapshot_id: RequestId::new(format!("synthetic-{source_sequence}")).unwrap(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "synthetic/1/monotonic".into(),
        identity_boundary: None,
        frame_ref: None,
        source_sequence,
        observed_ms: 0,
        app: AppIdentity {
            bundle_id: APP.into(),
            name: "Settings".into(),
            pid: 42,
        },
        global_focus: GlobalFocusObservation {
            app: ObservationFact::Known {
                value: AppIdentity {
                    bundle_id: APP.into(),
                    name: "Settings".into(),
                    pid: 42,
                },
            },
            window_id: ObservationFact::Known {
                value: WINDOW.into(),
            },
        },
        target_context: None,
        windows: vec![WindowInfo {
            window_id: WINDOW.into(),
            title: Some("Settings".into()),
            frame: objects
                .iter()
                .find(|object| object.role == "AXWindow")
                .expect("synthetic snapshot contains its AXWindow")
                .frame,
            z_index: 0,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: APP.into(),
        }],
        complete: true,
        incomplete_reason: None,
        objects,
        observer_coverage: ObserverCoverage {
            state: if read_scoped_objects == 0 {
                SourceState::Available
            } else {
                SourceState::Degraded
            },
            covered_objects,
            read_scoped_objects,
            failed_registrations: 0,
            failures: vec![],
        },
        menu_anchors: vec![],
        menu_context: None,
        focus_context: None,
        window_context: None,
        table_scope_search: None,
        table_viewports: vec![],
        owned_document_evidence: None,
    }
}

pub fn binding_proof(
    snapshot: &StructureSnapshot,
    instance: &str,
    binding: &str,
    menu_scope: Option<MenuBindingScope>,
    focus_scope: Option<FocusedControlScope>,
) -> BindingProof {
    let target = snapshot
        .objects
        .iter()
        .find(|object| object.instance_ref.as_str() == instance)
        .expect("binding fixture target is in its synthetic Source snapshot");
    let evidence = if focus_scope.is_some() {
        derive_focused_binding_evidence(snapshot, target, BindingProofScope::WholeStructure)
    } else {
        derive_binding_evidence(snapshot, target, BindingProofScope::WholeStructure)
    }
    .expect("complete synthetic Source context derives a whole-structure witness");
    BindingProof {
        binding: BindingRef::new(binding).unwrap(),
        instance: target.instance_ref.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        environment_revision: 1,
        layer_revision: 1,
        fingerprint: instance_fingerprint(snapshot, target),
        display_frame: target.frame,
        continuity: evidence.continuity,
        device_context: evidence.device_context,
        menu_scope,
        focus_scope,
    }
}
