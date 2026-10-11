//! End-to-end Engine coverage for the Host-derived scoped native revisit.

use std::collections::{BTreeMap, BTreeSet};

use interaction_contracts::desktop::binding::instance_fingerprint;
use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, CommandAck, DependencyVersions,
    DeviceIdentity, GrantOutcome, PermitRef, Receipt, ReceiptStatus, ReleaseStatus,
    SubmittedAction, SurfaceRef,
};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::{Admission, DenyReason, EffectClass};
use interaction_contracts::desktop::ids::{ObjectRef, RequestId, SemanticTargetRef};
use interaction_contracts::desktop::operator::{ConfirmationRequest, OperatorCommand, RunMode};
use interaction_contracts::desktop::planning::{
    Dependency, GoalDraft, PlanBundle, PlanProposal, PlannerOutput, PlanningRequest,
    ScrollDirection, ScrollToSpec, SelectByLabelSpec, Subgoal,
};
use interaction_contracts::desktop::primitive::PrimitivePolicy;
use interaction_contracts::desktop::source::{
    AppIdentity, AxActionName, FrameMeta, FrameProvenance, FrameStatus, FrameStreamInfo,
    GlobalFocusObservation, KernelProcessIdentity, LayerKind, MenuAnchorContinuity, MenuContext,
    MenuDrawingWindow, MenuOpenerAnchor, ObjectIdentityContinuity, ObservationFact,
    ObserverCoverage, Rect, ScrollAxisState, ScrollState, SourceCommand, SourceIdentityBoundary,
    SourceKind, StructureSnapshot, UiObject, UiValue, WindowInfo,
};
use interaction_contracts::desktop::task::{
    AllowedControlScope, AllowedControlTarget, ConfirmationScope, ExpectedValue, Predicate,
    ScopedCommitKind, TargetSpec,
};

use crate::environment::pack::AppPack;
use crate::execution::{DeviceCommand, DeviceNotice, DeviceReply, DispatchPlan, Expect};
use crate::infrastructure::storage::Storage;
use crate::reaction::candidates::{Scope, gate, scoped_revisit_plan_is_allowed};
use crate::reaction::skills::{
    ExecutionPath, scoped_commit_action_binding_object, scoped_radio_choice_control,
    scoped_revisit_controls, scoped_revisit_primary_binding_object,
};
use crate::tasks::TaskConfig;

use super::{Engine, EngineConfig, Input, Output, RootRun};

const APP: &str = "com.lingxi.computer-user.mvp-fixture";
const WINDOW: &str = "window-1";
const TARGET: &str = "setting-weekly-digest-en";
const TARGET_LABEL: &str = "Weekly digest";
const TARGET_GROUP: &str = "Notifications";
const IDENTITY_NAMESPACE: &str = "11111111-1111-4111-8111-111111111111";
const REGION_VALIDATION_DESCRIPTION: &str = "fixture-region-code-validation";
const REGION_VALID_TEXT_EN: &str = "Region code is valid";
const REGION_INVALID_TEXT_EN: &str = "Enter a region code in the form CA-1234";
const REGION_VALID_TEXT_ZH: &str = "地区代码有效";
const REGION_INVALID_TEXT_ZH: &str = "请输入 CA-1234 格式的地区代码";

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[allow(clippy::too_many_arguments)] // Explicit Source fields in the test fixture.
fn object(
    id: &str,
    parent: Option<&str>,
    role: &str,
    frame: Rect,
    title: Option<&str>,
    value: Option<UiValue>,
    group: Option<&str>,
    actions: Vec<AxActionName>,
) -> UiObject {
    UiObject {
        instance_ref: ObjectRef::new(id).unwrap(),
        parent: parent.map(|value| ObjectRef::new(value).unwrap()),
        window_id: WINDOW.into(),
        role: role.into(),
        subrole: (role == "AXWindow").then(|| "AXStandardWindow".into()),
        title: title.map(str::to_owned),
        description: None,
        value,
        enabled: role != "AXWindow" && role != "AXGroup",
        focused: false,
        frame,
        actions,
        ax_action_names: None,
        in_view: true,
        group: group.map(str::to_owned),
        secure: false,
        scroll: None,
        identity_continuity: ObjectIdentityContinuity::NotificationCovered,
    }
}

/// One complete normal-UI Source read. It contains only the current page's
/// actual fixture controls; the goal never receives reset or evaluator data.
fn source(
    sequence: u64,
    observed_ms: u64,
    settings: bool,
    checked: bool,
    group: &str,
) -> StructureSnapshot {
    let window_frame = rect(0.0, 0.0, 800.0, 600.0);
    let mut objects = vec![object(
        "window-root",
        None,
        "AXWindow",
        window_frame,
        Some("Interaction Fixture"),
        None,
        None,
        Vec::new(),
    )];
    for (id, label, selected) in [
        ("page-settings", "Settings", settings),
        ("page-overview", "Overview", !settings),
    ] {
        objects.push(object(
            id,
            Some("window-root"),
            "AXRadioButton",
            rect(
                if label == "Settings" { 10.0 } else { 120.0 },
                10.0,
                100.0,
                28.0,
            ),
            Some(label),
            Some(UiValue::Bool(selected)),
            None,
            vec![AxActionName::Press],
        ));
    }
    if settings {
        objects.push(object(
            "notifications-group",
            Some("window-root"),
            "AXGroup",
            rect(10.0, 60.0, 500.0, 100.0),
            Some(group),
            None,
            None,
            Vec::new(),
        ));
        objects.push(object(
            "weekly-digest",
            Some("notifications-group"),
            "AXCheckBox",
            rect(24.0, 82.0, 180.0, 28.0),
            Some(TARGET_LABEL),
            Some(UiValue::Bool(checked)),
            Some(group),
            vec![AxActionName::Press],
        ));
    }
    let app = AppIdentity {
        bundle_id: APP.into(),
        name: "Interaction Fixture".into(),
        pid: 42,
    };
    let snapshot = StructureSnapshot {
        snapshot_id: RequestId::new(format!("scoped-revisit-read-{sequence}")).unwrap(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "macos/1/wall".into(),
        identity_boundary: Some(SourceIdentityBoundary {
            namespace: IDENTITY_NAMESPACE.into(),
            generation: 1,
        }),
        frame_ref: None,
        source_sequence: sequence,
        observed_ms,
        app: app.clone(),
        windows: vec![WindowInfo {
            window_id: WINDOW.into(),
            title: Some("Interaction Fixture".into()),
            frame: window_frame,
            z_index: 0,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: APP.into(),
        }],
        global_focus: GlobalFocusObservation {
            app: ObservationFact::Known { value: app },
            window_id: ObservationFact::Known {
                value: WINDOW.into(),
            },
        },
        target_context: None,
        complete: true,
        incomplete_reason: None,
        observer_coverage: ObserverCoverage::for_projected_objects(&objects),
        objects,
        menu_anchors: Vec::new(),
        menu_context: None,
        focus_context: None,
        window_context: None,
        table_viewports: Vec::new(),
        table_scope_search: None,
        owned_document_evidence: None,
    };
    snapshot
        .validate()
        .expect("the fixture Source snapshot is a complete same-window tree");
    snapshot
}

#[allow(clippy::too_many_arguments)]
fn form_source(
    sequence: u64,
    observed_ms: u64,
    settings: bool,
    semantic: &str,
    label: &str,
    form: &str,
    role: &str,
    value: UiValue,
) -> StructureSnapshot {
    form_source_with_focus(
        sequence,
        observed_ms,
        settings,
        semantic,
        label,
        form,
        role,
        false,
        value,
    )
}

#[allow(clippy::too_many_arguments)]
fn form_source_with_focus(
    sequence: u64,
    observed_ms: u64,
    settings: bool,
    semantic: &str,
    label: &str,
    form: &str,
    role: &str,
    focused: bool,
    value: UiValue,
) -> StructureSnapshot {
    let mut snapshot = source(sequence, observed_ms, settings, false, form);
    if settings {
        let context = snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref.as_str() == "notifications-group")
            .expect("the synthetic form includes its labelled AXGroup");
        context.title = Some(form.into());

        let target = snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref.as_str() == "weekly-digest")
            .expect("the synthetic form includes its primary control");
        target.instance_ref = ObjectRef::new(semantic).unwrap();
        target.role = role.into();
        target.title = Some(label.into());
        target.value = Some(value);
        target.frame = rect(24.0, 82.0, 260.0, 36.0);
        target.focused = focused;
        target.group = Some(form.into());
        target.actions = if role == "AXCheckBox" {
            vec![AxActionName::Press]
        } else {
            vec![AxActionName::SetValue]
        };
    }
    snapshot
        .validate()
        .expect("the synthetic form Source remains a complete same-window tree");
    snapshot
}

#[allow(clippy::too_many_arguments)]
fn region_validation_source(
    sequence: u64,
    observed_ms: u64,
    settings: bool,
    semantic: &str,
    label: &str,
    form: &str,
    value: &str,
    focused: bool,
    feedback_text: Option<&str>,
) -> StructureSnapshot {
    let mut snapshot = source(sequence, observed_ms, settings, false, form);
    snapshot.objects.retain(|object| {
        !["notifications-group", "weekly-digest"].contains(&object.instance_ref.as_str())
    });
    if semantic.ends_with("-zh") {
        for (instance, title) in [("page-settings", "设置"), ("page-overview", "概览")] {
            if let Some(page) = snapshot
                .objects
                .iter_mut()
                .find(|object| object.instance_ref.as_str() == instance)
            {
                page.title = Some(title.into());
            }
        }
    }
    if settings {
        snapshot.objects.push(object(
            "account-form",
            Some("window-root"),
            "AXGroup",
            rect(10.0, 60.0, 500.0, 140.0),
            Some(form),
            None,
            None,
            Vec::new(),
        ));
        let mut field = object(
            "region-code-field-live",
            Some("account-form"),
            "AXTextField",
            rect(24.0, 82.0, 260.0, 36.0),
            Some(label),
            Some(UiValue::Text(value.into())),
            Some(form),
            vec![AxActionName::SetValue],
        );
        field.focused = focused;
        snapshot.objects.push(field);
        if let Some(feedback_text) = feedback_text {
            let mut feedback = object(
                "region-validation-feedback",
                Some("account-form"),
                "AXStaticText",
                rect(24.0, 124.0, 420.0, 26.0),
                None,
                Some(UiValue::Text(feedback_text.into())),
                Some(form),
                Vec::new(),
            );
            feedback.description = Some(REGION_VALIDATION_DESCRIPTION.into());
            snapshot.objects.push(feedback);
        }
    }
    snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    snapshot
        .validate()
        .expect("the synthetic region-code Source is a complete current form tree");
    snapshot
}

#[derive(Clone, Copy)]
enum RegionFeedbackShape {
    Missing,
    Invalid,
    Duplicate,
    ForeignForm,
    Offscreen,
    WrongRole,
    SameNamedForeignGroup,
}

fn region_feedback_variant_source(
    sequence: u64,
    observed_ms: u64,
    semantic: &str,
    label: &str,
    form: &str,
    value: &str,
    shape: RegionFeedbackShape,
) -> StructureSnapshot {
    let mut snapshot = region_validation_source(
        sequence,
        observed_ms,
        true,
        semantic,
        label,
        form,
        value,
        true,
        Some(region_valid_text(semantic)),
    );
    let feedback_index = snapshot
        .objects
        .iter()
        .position(|object| {
            object.role == "AXStaticText"
                && object.description.as_deref() == Some(REGION_VALIDATION_DESCRIPTION)
        })
        .expect("the base Source has one registered validation feedback object");
    match shape {
        RegionFeedbackShape::Missing => {
            snapshot.objects.remove(feedback_index);
        }
        RegionFeedbackShape::Invalid => {
            let invalid = snapshot.objects.get_mut(feedback_index).unwrap();
            invalid.value = Some(UiValue::Text(region_invalid_text(semantic).into()));
        }
        RegionFeedbackShape::Duplicate => {
            let mut duplicate = snapshot.objects[feedback_index].clone();
            duplicate.instance_ref =
                ObjectRef::new("region-validation-feedback-duplicate").unwrap();
            duplicate.frame = rect(24.0, 152.0, 420.0, 26.0);
            snapshot.objects.push(duplicate);
        }
        RegionFeedbackShape::ForeignForm => {
            let mut feedback = snapshot.objects.remove(feedback_index);
            snapshot.objects.push(object(
                "foreign-profile-form",
                Some("window-root"),
                "AXGroup",
                rect(10.0, 212.0, 500.0, 100.0),
                Some("Profile"),
                None,
                None,
                Vec::new(),
            ));
            feedback.parent = Some(ObjectRef::new("foreign-profile-form").unwrap());
            feedback.group = Some("Profile".into());
            feedback.frame = rect(24.0, 232.0, 420.0, 26.0);
            snapshot.objects.push(feedback);
        }
        RegionFeedbackShape::Offscreen => {
            snapshot.objects.get_mut(feedback_index).unwrap().in_view = false;
        }
        RegionFeedbackShape::WrongRole => {
            snapshot.objects.get_mut(feedback_index).unwrap().role = "AXButton".into();
        }
        RegionFeedbackShape::SameNamedForeignGroup => {
            let mut feedback = snapshot.objects.remove(feedback_index);
            snapshot.objects.push(object(
                "duplicate-account-form",
                Some("window-root"),
                "AXGroup",
                rect(10.0, 212.0, 500.0, 100.0),
                Some(form),
                None,
                None,
                Vec::new(),
            ));
            feedback.parent = Some(ObjectRef::new("duplicate-account-form").unwrap());
            feedback.group = Some(form.into());
            feedback.frame = rect(24.0, 232.0, 420.0, 26.0);
            snapshot.objects.push(feedback);
        }
    }
    snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    snapshot
        .validate()
        .expect("the validation feedback variation remains a complete Source tree");
    snapshot
}

#[allow(clippy::too_many_arguments)]
fn choice_source(
    sequence: u64,
    observed_ms: u64,
    settings: bool,
    semantic: &str,
    label: &str,
    form: &str,
    value: &str,
    open_menu: bool,
    choices: &[&str],
) -> StructureSnapshot {
    let mut snapshot = source(sequence, observed_ms, settings, false, form);
    if semantic.ends_with("-zh") {
        for (instance, label) in [("page-settings", "设置"), ("page-overview", "概览")] {
            if let Some(page) = snapshot
                .objects
                .iter_mut()
                .find(|object| object.instance_ref.as_str() == instance)
            {
                page.title = Some(label.into());
            }
        }
    }
    if settings {
        let context = snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref.as_str() == "notifications-group")
            .expect("the synthetic form includes its labelled AXGroup");
        context.title = Some(form.into());

        let target = snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref.as_str() == "weekly-digest")
            .expect("the synthetic form includes its primary control");
        target.instance_ref = ObjectRef::new(semantic).unwrap();
        target.role = "AXPopUpButton".into();
        target.title = Some(label.into());
        target.value = Some(UiValue::Text(value.into()));
        target.frame = rect(24.0, 82.0, 260.0, 36.0);
        target.focused = false;
        target.group = Some(form.into());
        target.actions = vec![AxActionName::ShowMenu];

        let anchor = MenuOpenerAnchor {
            anchor_id: "cadence-anchor".into(),
            source_session_id: "scoped-popup-watch".into(),
            anchor_revision: 1,
            process: KernelProcessIdentity {
                pid: snapshot.app.pid,
                start_seconds: 1,
                start_micros: 0,
            },
            origin_window_id: WINDOW.into(),
            origin_window_frame: snapshot.windows[0].frame,
            opener_instance: ObjectRef::new(semantic).unwrap(),
            opener_fingerprint: instance_fingerprint(
                &snapshot,
                snapshot
                    .objects
                    .iter()
                    .find(|object| object.instance_ref.as_str() == semantic)
                    .expect("the trusted popup is present"),
            ),
            opener_frame: rect(24.0, 82.0, 260.0, 36.0),
            verified_source_sequence: sequence,
            verified_observed_ms: observed_ms,
            continuity: MenuAnchorContinuity::DirectRead,
        };
        snapshot.menu_anchors = vec![anchor.clone()];

        if open_menu {
            let mut menu = snapshot
                .objects
                .iter()
                .find(|object| object.instance_ref == anchor.opener_instance)
                .expect("the trusted popup remains in the Source tree")
                .clone();
            menu.instance_ref = ObjectRef::new(format!("cadence-menu-{sequence}")).unwrap();
            menu.parent = Some(anchor.opener_instance.clone());
            menu.role = "AXMenu".into();
            menu.subrole = None;
            menu.title = Some("Reminder cadence choices".into());
            menu.description = None;
            menu.value = None;
            menu.group = None;
            menu.frame = rect(250.0, 120.0, 180.0, 28.0 + 25.0 * choices.len() as f64);
            menu.actions.clear();
            menu.identity_continuity = ObjectIdentityContinuity::ReadScoped;
            let menu_frame = menu.frame;
            let menu_instance = menu.instance_ref.clone();
            snapshot.objects.push(menu.clone());
            for (index, choice) in choices.iter().enumerate() {
                let mut item = menu.clone();
                item.instance_ref =
                    ObjectRef::new(format!("cadence-choice-{sequence}-{index}")).unwrap();
                item.parent = Some(menu_instance.clone());
                item.role = "AXMenuItem".into();
                item.title = Some((*choice).into());
                item.frame = rect(
                    menu_frame.x,
                    menu_frame.y + 2.0 + 25.0 * index as f64,
                    menu_frame.width,
                    25.0,
                );
                item.actions = vec![AxActionName::Press];
                item.identity_continuity = ObjectIdentityContinuity::ReadScoped;
                snapshot.objects.push(item);
            }
            snapshot.menu_context = Some(MenuContext {
                context_id: format!("cadence-menu-context-{sequence}"),
                source_session_id: anchor.source_session_id.clone(),
                opener: anchor.clone(),
                menu_instance,
                menu_fingerprint: instance_fingerprint(&snapshot, &menu),
                menu_frame,
                drawing_window: MenuDrawingWindow {
                    window_id: 101,
                    owner_pid: snapshot.app.pid,
                    layer: 101,
                    frame: menu_frame,
                },
            });
        }
    }
    snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    snapshot
        .validate()
        .expect("the synthetic popup Source is a complete native tree");
    snapshot
}

#[allow(clippy::too_many_arguments)]
fn radio_choice_source(
    sequence: u64,
    observed_ms: u64,
    settings: bool,
    semantic: &str,
    label: &str,
    form: &str,
    selected_labels: &[&str],
) -> StructureSnapshot {
    let mut snapshot = source(sequence, observed_ms, settings, false, form);
    snapshot.objects.retain(|object| {
        !["notifications-group", "weekly-digest"].contains(&object.instance_ref.as_str())
    });
    if semantic.ends_with("-zh") {
        for (instance, title) in [("page-settings", "设置"), ("page-overview", "概览")] {
            if let Some(page) = snapshot
                .objects
                .iter_mut()
                .find(|object| object.instance_ref.as_str() == instance)
            {
                page.title = Some(title.into());
            }
        }
    }
    if settings {
        let choices: &[&str] = if label == "摘要分组" {
            &["按天", "按周", "按月"]
        } else {
            &["By day", "By week", "By month"]
        };
        snapshot.objects.push(object(
            "preferences-form",
            Some("window-root"),
            "AXGroup",
            rect(10.0, 60.0, 500.0, 180.0),
            Some(form),
            None,
            None,
            Vec::new(),
        ));
        let mut radio_group = object(
            "summary-grouping",
            Some("preferences-form"),
            "AXRadioGroup",
            rect(24.0, 82.0, 260.0, 108.0),
            Some(label),
            None,
            Some(form),
            Vec::new(),
        );
        radio_group.enabled = false;
        snapshot.objects.push(radio_group);
        let id_prefix = if label == "摘要分组" {
            "summary-choice-zh"
        } else {
            "summary-choice-en"
        };
        for (index, choice) in choices.iter().enumerate() {
            snapshot.objects.push(object(
                &format!("{id_prefix}-{index}"),
                Some("summary-grouping"),
                "AXRadioButton",
                rect(32.0, 90.0 + 28.0 * index as f64, 220.0, 24.0),
                Some(choice),
                Some(UiValue::Bool(selected_labels.contains(choice))),
                Some(label),
                vec![AxActionName::Press],
            ));
        }
    }
    snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    snapshot
        .validate()
        .expect("the synthetic radio-group Source is a complete native tree");
    snapshot
}

#[allow(clippy::too_many_arguments)]
fn persistence_source(
    sequence: u64,
    observed_ms: u64,
    semantic: &str,
    label: &str,
    group: &str,
    checked: bool,
    receipt_text: &str,
    include_save: bool,
) -> StructureSnapshot {
    let mut snapshot = source(sequence, observed_ms, true, checked, group);
    let target = snapshot
        .objects
        .iter_mut()
        .find(|object| object.instance_ref.as_str() == "weekly-digest")
        .expect("the fixture source includes the registered primary checkbox");
    target.instance_ref = ObjectRef::new(semantic).unwrap();
    target.title = Some(label.into());
    target.value = Some(UiValue::Bool(checked));
    target.group = Some(group.into());
    if let Some(container) = snapshot
        .objects
        .iter_mut()
        .find(|object| object.instance_ref.as_str() == "notifications-group")
    {
        container.title = Some(group.into());
    }
    if include_save {
        snapshot.objects.push(object(
            "save-include-archived-en",
            Some("notifications-group"),
            "AXButton",
            rect(24.0, 140.0, 120.0, 32.0),
            Some("Save"),
            None,
            Some(group),
            vec![AxActionName::Press],
        ));
    }
    let mut receipt = object(
        "persistence-receipt",
        Some("notifications-group"),
        "AXStaticText",
        rect(24.0, 180.0, 300.0, 24.0),
        None,
        Some(UiValue::Text(receipt_text.into())),
        Some(group),
        Vec::new(),
    );
    receipt.description = Some("fixture-persistence-receipt".into());
    snapshot.objects.push(receipt);
    snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    snapshot
        .validate()
        .expect("the persistence Source is a complete current fixture tree");
    snapshot
}

/// A synthetic, same-window stream satisfies the ordinary ForegroundNative
/// frame gate. Its first callback is installed before a Root exists; later
/// callbacks stay on the same stream and do not reset dependencies.
fn frame(sequence: u64, source_ms: u64, callback_ms: u64) -> FrameMeta {
    let window_frame = rect(0.0, 0.0, 800.0, 600.0);
    FrameMeta {
        frame_id: format!("scoped-revisit-frame-{sequence}"),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "macos/1/wall".into(),
        source_sequence: sequence,
        source_ms,
        status: FrameStatus::Complete,
        geometry_revision: 1,
        width_px: 800,
        height_px: 600,
        scale_factor: 1.0,
        byte_length: 800 * 600 * 4,
        checksum: format!("sha256:scoped-revisit-frame-{sequence}"),
        provenance: Some(FrameProvenance::Stream {
            stream: FrameStreamInfo {
                stream_id: "scoped-revisit-stream".into(),
                image_frame_id: Some(format!("scoped-revisit-image-{sequence}")),
                display_time_ticks: source_ms * 1_000_000,
                display_time_ns: source_ms * 1_000_000,
                received_uptime_ns: callback_ms * 1_000_000,
                received_unix_ms: 100_000 + callback_ms,
                callback_ms,
                source_clock: "mach_absolute".into(),
                window_id: WINDOW.into(),
                pid: 42,
                window_frame,
                content_rect: window_frame,
                content_scale: 1.0,
                dirty_rects: Vec::new(),
                dropped_frames: 0,
                capture_interval_ms: 100,
            },
        }),
    }
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
                ("focus".into(), 1),
                ("window".into(), 1),
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

fn scope(group: &str) -> AllowedControlScope {
    AllowedControlScope {
        application: "Interaction Fixture".into(),
        targets: vec![AllowedControlTarget {
            control: TARGET_LABEL.into(),
            group: Some(group.into()),
            form: None,
            disambiguator: None,
        }],
        no_unrelated_controls: true,
    }
}

fn form_scope(label: &str, form: &str) -> AllowedControlScope {
    AllowedControlScope {
        application: "Interaction Fixture".into(),
        targets: vec![AllowedControlTarget {
            control: label.into(),
            group: None,
            form: Some(form.into()),
            disambiguator: None,
        }],
        no_unrelated_controls: true,
    }
}

fn manual_commit_scope() -> AllowedControlScope {
    AllowedControlScope {
        application: "Interaction Fixture".into(),
        targets: vec![AllowedControlTarget {
            control: "Include archived items".into(),
            group: Some("Search".into()),
            form: None,
            disambiguator: None,
        }],
        no_unrelated_controls: true,
    }
}

fn automatic_commit_scope() -> AllowedControlScope {
    AllowedControlScope {
        application: "Interaction Fixture".into(),
        targets: vec![AllowedControlTarget {
            control: "Quiet hours".into(),
            group: Some("Notifications".into()),
            form: None,
            disambiguator: None,
        }],
        no_unrelated_controls: true,
    }
}

fn commit_draft(semantic: &str, label: &str, group: &str, scope: ConfirmationScope) -> GoalDraft {
    GoalDraft {
        app: APP.into(),
        account: None,
        target: TargetSpec {
            semantic_target: SemanticTargetRef::new(semantic).unwrap(),
            label: label.into(),
            group: Some(group.into()),
            role_hint: Some("AXCheckBox".into()),
            row: None,
        },
        desired: ExpectedValue::Bool { value: true },
        confirmation_scope: scope,
        max_effect_class: if scope == ConfirmationScope::Commit {
            EffectClass::Commit
        } else {
            EffectClass::ReversibleEdit
        },
    }
}

fn manual_commit_plan() -> PlanBundle {
    PlanBundle {
        subgoals: vec![
            Subgoal {
                id: "ensure-include-archived".into(),
                description: "Enable Include archived items".into(),
                skill: "ui.ensure_value".into(),
                target: Some(SemanticTargetRef::new("setting-include-archived-en").unwrap()),
                expects: vec!["value".into()],
                scroll: None,
                select: None,
            },
            Subgoal {
                id: "save-include-archived".into(),
                description: "Save the Search setting".into(),
                skill: "ui.activate".into(),
                target: Some(SemanticTargetRef::new("save-include-archived-en").unwrap()),
                expects: vec!["commit".into()],
                scroll: None,
                select: None,
            },
        ],
        dependencies: vec![Dependency {
            before: "ensure-include-archived".into(),
            after: "save-include-archived".into(),
        }],
        required_observations: Vec::new(),
        expected_effects: vec!["goal.required".into()],
        recovery_branches: Vec::new(),
        termination: vec!["goal.required".into()],
    }
}

fn form_draft(
    semantic: &str,
    label: &str,
    form: &str,
    role: &str,
    desired: ExpectedValue,
) -> GoalDraft {
    GoalDraft {
        app: APP.into(),
        account: None,
        target: TargetSpec {
            semantic_target: SemanticTargetRef::new(semantic).unwrap(),
            label: label.into(),
            group: Some(form.into()),
            role_hint: Some(role.into()),
            row: None,
        },
        desired,
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::ReversibleEdit,
    }
}

fn draft() -> GoalDraft {
    GoalDraft {
        app: APP.into(),
        account: None,
        target: TargetSpec {
            semantic_target: SemanticTargetRef::new(TARGET).unwrap(),
            label: TARGET_LABEL.into(),
            group: Some(TARGET_GROUP.into()),
            role_hint: Some("AXCheckBox".into()),
            row: None,
        },
        desired: ExpectedValue::Bool { value: false },
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::ReversibleEdit,
    }
}

fn plan() -> PlanBundle {
    scoped_plan(TARGET)
}

fn scoped_plan(target: &str) -> PlanBundle {
    PlanBundle {
        subgoals: vec![
            Subgoal {
                id: "ensure-weekly-digest".into(),
                description: "Set the requested checkbox value".into(),
                skill: "ui.ensure_value".into(),
                target: Some(SemanticTargetRef::new(target).unwrap()),
                expects: vec!["value".into()],
                scroll: None,
                select: None,
            },
            Subgoal {
                id: "revisit-weekly-digest".into(),
                description: "Revisit the target page to verify the value".into(),
                skill: "ui.revisit".into(),
                target: Some(SemanticTargetRef::new(target).unwrap()),
                expects: vec!["goal.required".into()],
                scroll: None,
                select: None,
            },
        ],
        dependencies: vec![Dependency {
            before: "ensure-weekly-digest".into(),
            after: "revisit-weekly-digest".into(),
        }],
        required_observations: Vec::new(),
        expected_effects: vec!["goal.required".into()],
        recovery_branches: Vec::new(),
        termination: vec!["goal.required".into()],
    }
}

fn choice_plan(target: &str, choice: &str) -> PlanBundle {
    PlanBundle {
        subgoals: vec![
            Subgoal {
                id: "read-current-reminder-cadence".into(),
                description: "Read the current reminder cadence".into(),
                skill: "ui.read_value".into(),
                target: Some(SemanticTargetRef::new(target).unwrap()),
                expects: vec!["value".into()],
                scroll: None,
                select: None,
            },
            Subgoal {
                id: "select-reminder-cadence".into(),
                description: "Select the requested reminder cadence".into(),
                skill: "ui.select_by_label".into(),
                target: Some(SemanticTargetRef::new(target).unwrap()),
                expects: vec!["value".into()],
                scroll: None,
                select: Some(SelectByLabelSpec {
                    choice_label: choice.into(),
                }),
            },
            Subgoal {
                id: "revisit-reminder-cadence".into(),
                description: "Revisit the form to verify the selected cadence".into(),
                skill: "ui.revisit".into(),
                target: Some(SemanticTargetRef::new(target).unwrap()),
                expects: vec!["goal.required".into()],
                scroll: None,
                select: None,
            },
        ],
        dependencies: vec![
            Dependency {
                before: "read-current-reminder-cadence".into(),
                after: "select-reminder-cadence".into(),
            },
            Dependency {
                before: "select-reminder-cadence".into(),
                after: "revisit-reminder-cadence".into(),
            },
        ],
        required_observations: Vec::new(),
        expected_effects: vec!["goal.required".into()],
        recovery_branches: Vec::new(),
        termination: vec!["goal.required".into()],
    }
}

fn radio_choice_plan(target: &str, choice: &str) -> PlanBundle {
    PlanBundle {
        subgoals: vec![
            Subgoal {
                id: "select-summary-grouping".into(),
                description: "Select the requested summary grouping".into(),
                skill: "ui.select_by_label".into(),
                target: Some(SemanticTargetRef::new(target).unwrap()),
                expects: vec!["value".into()],
                scroll: None,
                select: Some(SelectByLabelSpec {
                    choice_label: choice.into(),
                }),
            },
            Subgoal {
                id: "revisit-summary-grouping".into(),
                description: "Revisit the form to verify the summary grouping".into(),
                skill: "ui.revisit".into(),
                target: Some(SemanticTargetRef::new(target).unwrap()),
                expects: vec!["goal.required".into()],
                scroll: None,
                select: None,
            },
        ],
        dependencies: vec![Dependency {
            before: "select-summary-grouping".into(),
            after: "revisit-summary-grouping".into(),
        }],
        required_observations: Vec::new(),
        expected_effects: vec!["goal.required".into()],
        recovery_branches: Vec::new(),
        termination: vec!["goal.required".into()],
    }
}

fn start_scoped_task(
    objective: &str,
    allowed_scope: AllowedControlScope,
) -> (Engine, PlanningRequest, Vec<Output>) {
    let pack = AppPack::registered_native(APP).expect("the native fixture has a trusted pack");
    let mut config = EngineConfig::new(TaskConfig::new(vec![pack]));
    config.initial_mode = RunMode::Execute;
    let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());

    let mut outputs = engine.handle(
        100,
        Input::DeviceNotice(DeviceNotice::Manifest(Box::new(manifest()))),
    );
    let lease = find_device_command(&outputs, 0, |command| {
        matches!(command, DeviceCommand::RenewLease(_))
    })
    .expect("the native device establishes its lease before goal confirmation");
    outputs.extend(engine.handle(
        101,
        Input::Device {
            corr: lease.0,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));

    // ForegroundNative requires a same-window frame stream. Warm its first
    // stream before creating the Root, then settle dependency changes before
    // normal task admission begins.
    let warm_frame_start = outputs.len();
    outputs.extend(engine.handle(106, Input::Frame(Box::new(frame(1, 105, 106)))));
    acknowledge_dependency_updates(&mut engine, &mut outputs, warm_frame_start, 107);

    let create_start = outputs.len();
    outputs.extend(engine.handle(
        110,
        Input::Operator(OperatorCommand::CreateScopedTask {
            objective: objective.into(),
            allowed_scope,
        }),
    ));
    let request = outputs[create_start..]
        .iter()
        .find_map(|output| match output {
            Output::Plan(request) => Some(request.as_ref().clone()),
            _ => None,
        })
        .expect("the Engine sends the user request to GoalCompiler planning");
    (engine, request, outputs)
}

fn submit_proposal(
    engine: &mut Engine,
    request: &PlanningRequest,
    outputs: &mut Vec<Output>,
    goal_draft: GoalDraft,
    plan: PlanBundle,
) {
    outputs.extend(engine.handle(
        111,
        Input::Planner {
            request_id: request.request_id.clone(),
            generation: request.generation,
            output: Box::new(PlannerOutput::Plan {
                proposal: Box::new(PlanProposal {
                    goal_draft: Some(goal_draft),
                    bundle: plan,
                }),
            }),
        },
    ));
}

fn create_and_confirm() -> (Engine, ConfirmationRequest, Vec<Output>) {
    let (engine, confirmation, outputs) = create_and_confirm_scoped(
        "Turn off Weekly digest.",
        scope(TARGET_GROUP),
        draft(),
        plan(),
    );
    let interaction_contracts::desktop::operator::ConfirmationKind::Goal { goal } =
        &confirmation.kind
    else {
        panic!("the confirmation describes a goal");
    };
    let revisit = goal
        .scoped_revisit
        .as_ref()
        .expect("Host derives the revisit pair");
    assert_eq!(revisit.home.semantic_target.as_str(), "page-settings");
    assert_eq!(revisit.away.semantic_target.as_str(), "page-overview");
    (engine, confirmation, outputs)
}

fn create_and_confirm_scoped(
    objective: &str,
    allowed_scope: AllowedControlScope,
    goal_draft: GoalDraft,
    plan: PlanBundle,
) -> (Engine, ConfirmationRequest, Vec<Output>) {
    let (mut engine, request, mut outputs) = start_scoped_task(objective, allowed_scope.clone());
    submit_proposal(&mut engine, &request, &mut outputs, goal_draft, plan);
    let confirmation = outputs
        .iter()
        .rev()
        .find_map(|output| match output {
            Output::Ui(projection) => projection.pending_confirmation.clone(),
            _ => None,
        })
        .expect("GoalCompiler returns a user confirmation");
    assert!(confirmation.validate().is_ok());
    let interaction_contracts::desktop::operator::ConfirmationKind::Goal { goal } =
        &confirmation.kind
    else {
        panic!("the confirmation describes a goal");
    };
    assert_eq!(goal.allowed_scope, Some(allowed_scope));
    assert!(
        goal.scoped_revisit.is_some(),
        "Host derives the revisit pair"
    );
    (engine, confirmation, outputs)
}

fn create_and_confirm_scoped_commit(
    objective: &str,
    allowed_scope: AllowedControlScope,
    goal_draft: GoalDraft,
    plan: PlanBundle,
) -> (Engine, ConfirmationRequest, Vec<Output>) {
    let (mut engine, request, mut outputs) = start_scoped_task(objective, allowed_scope.clone());
    submit_proposal(&mut engine, &request, &mut outputs, goal_draft, plan);
    let confirmation = outputs
        .iter()
        .rev()
        .find_map(|output| match output {
            Output::Ui(projection) => projection.pending_confirmation.clone(),
            _ => None,
        })
        .expect("GoalCompiler returns a user confirmation");
    assert!(confirmation.validate().is_ok());
    assert!(matches!(
        &confirmation.kind,
        interaction_contracts::desktop::operator::ConfirmationKind::Goal { goal }
            if goal.scoped_commit.is_some()
    ));
    (engine, confirmation, outputs)
}

fn confirm_root(
    engine: &mut Engine,
    confirmation: &ConfirmationRequest,
    outputs: &mut Vec<Output>,
    now_ms: u64,
) {
    let start = outputs.len();
    outputs.extend(engine.handle(
        now_ms,
        Input::Operator(OperatorCommand::ConfirmGoal {
            confirmation_id: confirmation.confirmation_id.clone(),
            approved: true,
        }),
    ));
    let register = find_device_command(outputs, start, |command| {
        matches!(command, DeviceCommand::RegisterRoot(_))
    })
    .expect("confirmation registers the Root with Device");
    outputs.extend(engine.handle(
        now_ms + 1,
        Input::Device {
            corr: register.0,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
}

fn refresh_generation(outputs: &[Output]) -> Option<u64> {
    outputs.iter().rev().find_map(|output| match output {
        Output::Source {
            generation,
            command: SourceCommand::RefreshNow { .. },
        } => Some(*generation),
        _ => None,
    })
}

fn feed_fresh_source(
    engine: &mut Engine,
    outputs: &mut Vec<Output>,
    snapshot: StructureSnapshot,
    now_ms: u64,
) -> usize {
    let generation = refresh_generation(outputs).expect("the action requests a fresh Source read");
    let action_start = outputs.len();
    outputs.extend(engine.handle(
        now_ms,
        Input::SourceRefreshStarted {
            generation,
            accepted: true,
        },
    ));
    outputs.extend(engine.handle(
        now_ms,
        Input::Frame(Box::new(frame(
            snapshot.source_sequence + 1,
            snapshot.observed_ms.saturating_sub(2),
            snapshot.observed_ms.saturating_sub(1),
        ))),
    ));
    let arrival = now_ms + 1;
    outputs.extend(engine.handle(arrival, Input::Structure(Box::new(snapshot))));
    let settled_at = arrival + engine.cfg.native_evidence_quiet_ms + 1;
    outputs.extend(engine.handle(settled_at, Input::Tick));
    acknowledge_dependency_updates(engine, outputs, action_start, settled_at + 1);
    action_start
}

fn find_device_command(
    outputs: &[Output],
    start: usize,
    predicate: impl Fn(&DeviceCommand) -> bool,
) -> Option<(u64, DeviceCommand)> {
    outputs
        .get(start..)?
        .iter()
        .rev()
        .find_map(|output| match output {
            Output::Device { corr, command } if predicate(command.as_ref()) => {
                Some((*corr, command.as_ref().clone()))
            }
            _ => None,
        })
}

fn acknowledge_dependency_updates(
    engine: &mut Engine,
    outputs: &mut Vec<Output>,
    start: usize,
    mut now_ms: u64,
) {
    let mut acknowledged = BTreeSet::new();
    loop {
        let update = outputs.get(start..).and_then(|new_outputs| {
            new_outputs.iter().find_map(|output| match output {
                Output::Device { corr, command }
                    if !acknowledged.contains(corr)
                        && matches!(command.as_ref(), DeviceCommand::UpdateDependency(_)) =>
                {
                    let DeviceCommand::UpdateDependency(update) = command.as_ref() else {
                        unreachable!()
                    };
                    Some((*corr, update.name.clone()))
                }
                _ => None,
            })
        });
        let Some((corr, name)) = update else {
            break;
        };
        let mut versions = engine.device.dependencies().clone();
        versions.insert(
            name.clone(),
            versions.get(&name).copied().unwrap_or_default() + 1,
        );
        outputs.extend(engine.handle(
            now_ms,
            Input::Device {
                corr,
                reply: DeviceReply::Dependencies(DependencyVersions { versions }),
            },
        ));
        acknowledged.insert(corr);
        now_ms += 1;
    }
}

fn missing_observe_diagnostics(engine: &Engine, outputs: &[Output], start: usize) -> String {
    let now_ms = engine.now_ms;
    let max_age_ms = engine.cfg.evidence_max_age_ms;
    let latest = engine.state.latest().map(|env| {
        let snapshot = &env.snapshot;
        let matching_controls: Vec<_> = snapshot
            .objects
            .iter()
            .filter(|object| {
                object.title.as_deref() == Some(TARGET_LABEL)
                    || matches!(object.title.as_deref(), Some("Settings" | "Overview"))
            })
            .map(|object| {
                (
                    object.instance_ref.as_str(),
                    object.role.as_str(),
                    object.title.as_deref(),
                    object.group.as_deref(),
                    object.value.as_ref(),
                    object.actions.as_slice(),
                )
            })
            .collect();
        format!(
            "revision={} sequence={} observed_ms={} age_ms={} complete={} menu={} app={}/{} focus_app={:?} focus_window={:?} focused_window_known={} page={:?} matching_controls={:?} snapshot_validation={:?}",
            env.revision,
            snapshot.source_sequence,
            snapshot.observed_ms,
            now_ms.saturating_sub(snapshot.observed_ms),
            snapshot.complete,
            snapshot.menu_context.is_some(),
            snapshot.app.bundle_id,
            snapshot.app.pid,
            snapshot.global_focus.app,
            snapshot.global_focus.window_id,
            snapshot
                .global_focus
                .known_window_for_app(&snapshot.app)
                .is_some(),
            env.page,
            matching_controls,
            snapshot.validate(),
        )
    });
    let frame = engine.state.current_watch_frame(now_ms).map(|frame| {
        format!(
            "id={} sequence={} source_ms={} status={:?} stream={:?} geometry={:?}",
            frame.frame_id,
            frame.source_sequence,
            frame.source_ms,
            frame.status,
            frame.stream_info().map(|stream| {
                (
                    stream.stream_id.as_str(),
                    stream.callback_ms,
                    stream.window_id.as_str(),
                    stream.pid,
                    stream.window_frame,
                )
            }),
            frame.window_geometry(),
        )
    });
    let mut blockers = Vec::new();
    if engine.state.refresh_pending() {
        blockers.push("refresh_pending".to_owned());
    }
    if engine.state.structure_unavailable() {
        blockers.push("structure_unavailable".to_owned());
    }
    if let Some(env) = engine.state.latest() {
        let snapshot = &env.snapshot;
        if !snapshot.complete {
            blockers.push("snapshot_incomplete".to_owned());
        }
        if snapshot.menu_context.is_some() {
            blockers.push("menu_context_present".to_owned());
        }
        if snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .is_none()
        {
            blockers.push("focused_window_unknown".to_owned());
        }
        if snapshot.observed_ms > now_ms {
            blockers.push("snapshot_from_future".to_owned());
        }
        if now_ms.saturating_sub(snapshot.observed_ms) > max_age_ms {
            blockers.push("snapshot_too_old".to_owned());
        }
    } else {
        blockers.push("no_admitted_structure_snapshot".to_owned());
    }
    if engine.state.requires_frames() && engine.state.frame_deadline(now_ms, max_age_ms).is_none() {
        blockers.push("required_frame_deadline_missing_or_stale".to_owned());
    }
    if !engine.state.usable(now_ms, max_age_ms) && blockers.is_empty() {
        blockers
            .push("StateCore usable=false due to private floor or frame/window alignment".into());
    }
    let active = engine.tasks.active().map(|active| {
        format!(
            "state={:?} root={} goal={:?} plan={:?} pending={:?} failure={:?}",
            active.row.state,
            active.row.root_id,
            active.row.goal,
            active.row.plan,
            active.pending,
            active.row.failure,
        )
    });
    let run = engine.run.as_ref().map(|run| {
        format!(
            "registered={} registering={} done={:?} attempt_present={} wait_present={} source_gate_closed={} goal={:?}",
            run.registered,
            run.registering,
            run.done,
            run.attempt.is_some(),
            run.wait.is_some(),
            engine.source_gate_closed,
            run.goal,
        )
    });
    let manifest = engine.device.manifest().map(|manifest| {
        format!(
            "backend={:?} domain={:?} validation={:?} domain_available={} input_available={} declared_dependencies={:?}",
            manifest.backend,
            manifest.execution_domain,
            manifest.validate(),
            engine.device.execution_domain_available(),
            engine.device.input_available(),
            manifest.identity.dependencies,
        )
    });
    let waits = [
        (
            "lease",
            engine
                .device
                .is_waiting(|expect| matches!(expect, Expect::Lease)),
        ),
        (
            "register",
            engine
                .device
                .is_waiting(|expect| matches!(expect, Expect::Register)),
        ),
        (
            "dependency",
            engine
                .device
                .is_waiting(|expect| matches!(expect, Expect::Dependency)),
        ),
        (
            "observe",
            engine
                .device
                .is_waiting(|expect| matches!(expect, Expect::Observe { .. })),
        ),
        (
            "grant",
            engine
                .device
                .is_waiting(|expect| matches!(expect, Expect::Grant { .. })),
        ),
        (
            "submit",
            engine
                .device
                .is_waiting(|expect| matches!(expect, Expect::Submit { .. })),
        ),
    ];
    format!(
        "\nmissing Observe diagnostics:\nnow_ms={now_ms} run_mode={:?} root_state={:?} active={active:#?} run={run:#?}\nstate.usable={} blockers={blockers:?} refresh_pending={} structure_unavailable={} requires_frames={} frame_deadline={:?} latest={latest:#?} current_watch_frame={frame:#?} ax_health={:?} screenshot_health={:?}\nsource_gate_closed={} lease_established={} waits={waits:?} device_dependencies={:?} manifest={manifest:#?}\ntimeline={:#?}\nnew_outputs={:#?}",
        engine.mode,
        engine.root_state(),
        engine.state.usable(now_ms, max_age_ms),
        engine.state.refresh_pending(),
        engine.state.structure_unavailable(),
        engine.state.requires_frames(),
        engine.state.frame_deadline(now_ms, max_age_ms),
        engine.state.health(SourceKind::Ax),
        engine.state.health(SourceKind::Screenshot),
        engine.source_gate_closed,
        engine.device.lease_established(),
        engine.device.dependencies(),
        engine.timeline.iter().collect::<Vec<_>>(),
        outputs.get(start..).unwrap_or_default(),
    )
}

fn dispatch_next_action(
    engine: &mut Engine,
    outputs: &mut Vec<Output>,
    start: usize,
    now_ms: u64,
    permit_number: u64,
) -> (u64, interaction_contracts::desktop::device::SubmitIntent) {
    let Some((observe, command)) = find_device_command(outputs, start, |command| {
        matches!(command, DeviceCommand::Observe(_))
    }) else {
        panic!(
            "the planned skill did not observe its target before input{}",
            missing_observe_diagnostics(engine, outputs, start)
        );
    };
    assert!(matches!(command, DeviceCommand::Observe(_)));
    let after_observe = outputs.len();
    outputs.extend(engine.handle(
        now_ms,
        Input::Device {
            corr: observe,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
    let grant = find_device_command(outputs, after_observe, |command| {
        matches!(command, DeviceCommand::GrantPermit(_))
    })
    .unwrap_or_else(|| {
        panic!(
            "fresh Source evidence reaches the normal permit gate; timeline={:#?} new_outputs={:#?}",
            engine.timeline.iter().collect::<Vec<_>>(),
            outputs.get(after_observe..).unwrap_or_default()
        )
    });
    let after_grant = outputs.len();
    outputs.extend(engine.handle(
        now_ms + 1,
        Input::Device {
            corr: grant.0,
            reply: DeviceReply::Grant(GrantOutcome::Granted {
                permit: PermitRef {
                    id: format!("scoped-revisit-permit-{permit_number}"),
                    generation: permit_number,
                },
            }),
        },
    ));
    let (submit_corr, submit_command) = find_device_command(outputs, after_grant, |command| {
        matches!(command, DeviceCommand::Submit(_))
    })
    .expect("the admitted native action reaches one Submit");
    let DeviceCommand::Submit(submit) = submit_command else {
        unreachable!()
    };
    (submit_corr, submit)
}

fn receipt(
    submit: &interaction_contracts::desktop::device::SubmitIntent,
    status: ReceiptStatus,
) -> Receipt {
    let native = submit
        .intent
        .native()
        .expect("this regression exercises the native foreground path");
    let start_ms = submit.intent.latest_start_ms();
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
                at_ms: start_ms,
                primitive: native.steps[0].primitive.clone(),
                cleanup: false,
            }]
        } else {
            Vec::new()
        },
        release_status: ReleaseStatus::NotNeeded,
        command_end_ms: completed.then_some(start_ms + 2),
        input_end_ms: completed.then_some(start_ms + 1),
        at_ms: start_ms + 3,
    }
}

fn finish_submission(
    engine: &mut Engine,
    outputs: &mut Vec<Output>,
    corr: u64,
    submit: &interaction_contracts::desktop::device::SubmitIntent,
    status: ReceiptStatus,
) -> u64 {
    let completed = receipt(submit, status);
    let now_ms = completed.at_ms;
    outputs.extend(engine.handle(
        now_ms,
        Input::Device {
            corr,
            reply: DeviceReply::Receipt(Some(Box::new(completed))),
        },
    ));
    now_ms
}

fn manual_save_ready(initial_receipt: &str) -> (Engine, Vec<Output>, usize, u64) {
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped_commit(
        "Enable Include archived items and save it.",
        manual_commit_scope(),
        commit_draft(
            "setting-include-archived-en",
            "Include archived items",
            "Search",
            ConfirmationScope::Commit,
        ),
        manual_commit_plan(),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            1,
            120,
            "setting-include-archived-en",
            "Include archived items",
            "Search",
            false,
            initial_receipt,
            true,
        ),
        119,
    );
    let (ensure_corr, ensure_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, 230, 1);
    assert_eq!(
        &ensure_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::Press,
        }
    );
    let after_ensure = finish_submission(
        &mut engine,
        &mut outputs,
        ensure_corr,
        &ensure_submit,
        ReceiptStatus::Completed,
    );
    let save_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            2,
            after_ensure + 2,
            "setting-include-archived-en",
            "Include archived items",
            "Search",
            true,
            "No saved change yet",
            true,
        ),
        after_ensure + 1,
    );
    (engine, outputs, save_start, after_ensure)
}

fn submit_count(outputs: &[Output]) -> usize {
    outputs
        .iter()
        .filter(|output| {
            matches!(output, Output::Device { command, .. } if matches!(command.as_ref(), DeviceCommand::Submit(_)))
        })
        .count()
}

fn region_valid_text(semantic: &str) -> &'static str {
    if semantic.ends_with("-zh") {
        REGION_VALID_TEXT_ZH
    } else {
        REGION_VALID_TEXT_EN
    }
}

fn region_invalid_text(semantic: &str) -> &'static str {
    if semantic.ends_with("-zh") {
        REGION_INVALID_TEXT_ZH
    } else {
        REGION_INVALID_TEXT_EN
    }
}

fn region_code_set_value_pending(
    semantic: &str,
    label: &str,
    form: &str,
    initial: &str,
    desired: &str,
    objective: &str,
) -> (
    Engine,
    Vec<Output>,
    u64,
    interaction_contracts::desktop::device::SubmitIntent,
) {
    let expected = ExpectedValue::Text {
        value: desired.into(),
    };
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
        objective,
        form_scope(label, form),
        form_draft(semantic, label, form, "AXTextField", expected),
        scoped_plan(semantic),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    assert!(
        engine.run.is_some(),
        "the confirmed Region code Goal is active"
    );

    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        region_validation_source(
            1,
            120,
            true,
            semantic,
            label,
            form,
            initial,
            false,
            Some(region_invalid_text(semantic)),
        ),
        119,
    );
    let (focus_corr, focus_submit) = dispatch_next_action(&mut engine, &mut outputs, start, 230, 1);
    assert_eq!(
        &focus_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::Click {
            button: interaction_contracts::desktop::primitive::MouseButton::Left,
            x: 154,
            y: 100,
            modifiers: interaction_contracts::desktop::primitive::Modifiers::default(),
        },
        "an unfocused Region code field receives one click on its exact bound frame"
    );
    let focused_at = finish_submission(
        &mut engine,
        &mut outputs,
        focus_corr,
        &focus_submit,
        ReceiptStatus::Completed,
    );

    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        region_validation_source(
            2,
            focused_at + 2,
            true,
            semantic,
            label,
            form,
            initial,
            true,
            Some(region_invalid_text(semantic)),
        ),
        focused_at + 1,
    );
    let (set_corr, set_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, focused_at + 110, 2);
    assert_eq!(
        &set_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::SetValue {
                value: desired.into(),
            },
        },
        "the existing ensure_value skill uses exact AX SetValue after a fresh focus read"
    );
    (engine, outputs, set_corr, set_submit)
}

fn region_code_after_set_value(
    semantic: &str,
    label: &str,
    form: &str,
    initial: &str,
    desired: &str,
    objective: &str,
) -> (Engine, Vec<Output>, u64) {
    let (mut engine, mut outputs, set_corr, set_submit) =
        region_code_set_value_pending(semantic, label, form, initial, desired, objective);
    let at = finish_submission(
        &mut engine,
        &mut outputs,
        set_corr,
        &set_submit,
        ReceiptStatus::Completed,
    );
    (engine, outputs, at)
}

fn exercise_region_code_goal(semantic: &str, label: &str, form: &str, objective: &str) {
    let desired = "CA-2001";
    let (mut engine, mut outputs, set_at) =
        region_code_after_set_value(semantic, label, form, "??", desired, objective);
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        region_validation_source(
            3,
            set_at + 2,
            true,
            semantic,
            label,
            form,
            desired,
            true,
            Some(region_valid_text(semantic)),
        ),
        set_at + 1,
    );
    let run = engine
        .run
        .as_ref()
        .expect("valid feedback leaves the Goal active for revisit");
    let env = engine
        .state
        .latest()
        .expect("post-ensure Source is current");
    assert!(
        env.snapshot.objects.iter().any(|object| {
            object.role == "AXStaticText"
                && object.description.as_deref() == Some(REGION_VALIDATION_DESCRIPTION)
                && object.group.as_deref() == Some(form)
                && object.value.as_ref() == Some(&UiValue::Text(region_valid_text(semantic).into()))
        }),
        "the new Source carries the actual localized validation text"
    );
    assert_eq!(
        run.goal.target.semantic_target.as_str(),
        semantic,
        "feedback remains attached to the frozen Region code Text target"
    );

    let (away_corr, away_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, set_at + 110, 3);
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        region_validation_source(
            4,
            away_at + 2,
            false,
            semantic,
            label,
            form,
            desired,
            false,
            None,
        ),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, away_at + 110, 4);
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        region_validation_source(
            5,
            home_at + 2,
            true,
            semantic,
            label,
            form,
            desired,
            false,
            Some(region_valid_text(semantic)),
        ),
        home_at + 1,
    );

    assert_eq!(
        submit_count(&outputs),
        4,
        "one focus click, one SetValue, and the frozen away/home pair are the only inputs"
    );
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "the Region code Goal completes only after fresh valid feedback on return"
    );
}

fn exercise_form_goal(
    semantic: &str,
    label: &str,
    form: &str,
    role: &str,
    initial: UiValue,
    desired: ExpectedValue,
) {
    let objective = match &desired {
        ExpectedValue::Bool { value } => format!("Set {label} to {value}."),
        ExpectedValue::Text { value } => format!("Set {label} to {value:?}."),
        ExpectedValue::Number { value } => format!("Set {label} to {value}."),
    };
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
        &objective,
        form_scope(label, form),
        form_draft(semantic, label, form, role, desired.clone()),
        scoped_plan(semantic),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);

    let initial_after_focus = initial.clone();
    let action_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        form_source(1, 120, true, semantic, label, form, role, initial),
        119,
    );
    let mut next_sequence = 2;
    let after_ensure = match &desired {
        ExpectedValue::Text { value } => {
            let (focus_corr, focus_submit) =
                dispatch_next_action(&mut engine, &mut outputs, action_start, 230, 1);
            let focus_primitive = &focus_submit.intent.native().unwrap().steps[0].primitive;
            assert_eq!(
                focus_primitive,
                &interaction_contracts::desktop::primitive::Primitive::Click {
                    button: interaction_contracts::desktop::primitive::MouseButton::Left,
                    x: 154,
                    y: 100,
                    modifiers: interaction_contracts::desktop::primitive::Modifiers::default(),
                },
                "an unfocused form field receives one click on its exact bound frame"
            );
            let focused_at = finish_submission(
                &mut engine,
                &mut outputs,
                focus_corr,
                &focus_submit,
                ReceiptStatus::Completed,
            );
            let action_start = feed_fresh_source(
                &mut engine,
                &mut outputs,
                form_source_with_focus(
                    2,
                    focused_at + 2,
                    true,
                    semantic,
                    label,
                    form,
                    role,
                    true,
                    initial_after_focus.clone(),
                ),
                focused_at + 1,
            );
            let (set_corr, set_submit) =
                dispatch_next_action(&mut engine, &mut outputs, action_start, focused_at + 110, 2);
            assert_eq!(
                &set_submit.intent.native().unwrap().steps[0].primitive,
                &interaction_contracts::desktop::primitive::Primitive::AxAction {
                    action: interaction_contracts::desktop::primitive::AxActionKind::SetValue {
                        value: value.clone(),
                    },
                },
                "AX SetValue follows a newer Source read that confirms focus"
            );
            next_sequence = 3;
            finish_submission(
                &mut engine,
                &mut outputs,
                set_corr,
                &set_submit,
                ReceiptStatus::Completed,
            )
        }
        ExpectedValue::Bool { .. } => {
            let (ensure_corr, ensure_submit) =
                dispatch_next_action(&mut engine, &mut outputs, action_start, 230, 1);
            assert_eq!(
                &ensure_submit.intent.native().unwrap().steps[0].primitive,
                &interaction_contracts::desktop::primitive::Primitive::AxAction {
                    action: interaction_contracts::desktop::primitive::AxActionKind::Press,
                },
                "the registered checkbox uses the existing one-press ensure path"
            );
            finish_submission(
                &mut engine,
                &mut outputs,
                ensure_corr,
                &ensure_submit,
                ReceiptStatus::Completed,
            )
        }
        ExpectedValue::Number { .. } => panic!("the current form registry has no numeric field"),
    };

    let desired_source_value = match &desired {
        ExpectedValue::Bool { value } => UiValue::Bool(*value),
        ExpectedValue::Text { value } => UiValue::Text(value.clone()),
        ExpectedValue::Number { .. } => unreachable!("numeric form not registered"),
    };
    let action_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        form_source(
            next_sequence,
            after_ensure + 2,
            true,
            semantic,
            label,
            form,
            role,
            desired_source_value.clone(),
        ),
        after_ensure + 1,
    );
    let (away_corr, away_submit) = dispatch_next_action(
        &mut engine,
        &mut outputs,
        action_start,
        after_ensure + 110,
        2,
    );
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );
    next_sequence += 1;

    let action_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        form_source(
            next_sequence,
            away_at + 2,
            false,
            semantic,
            label,
            form,
            role,
            desired_source_value.clone(),
        ),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, action_start, away_at + 110, 3);
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    next_sequence += 1;
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        form_source(
            next_sequence,
            home_at + 2,
            true,
            semantic,
            label,
            form,
            role,
            desired_source_value,
        ),
        home_at + 1,
    );
    let expected_submissions = if matches!(&desired, ExpectedValue::Text { .. }) {
        4 // field focus, AX SetValue, away, then home
    } else {
        3 // ensure, away, then home
    };
    assert_eq!(submit_count(&outputs), expected_submissions);
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "the typed form goal completes only after a fresh return-page read"
    );
}

fn choice_press_ready(
    semantic: &str,
    label: &str,
    form: &str,
    initial: &str,
    desired: &str,
    choices: &[&str],
    objective: &str,
) -> (
    Engine,
    Vec<Output>,
    u64,
    interaction_contracts::desktop::device::SubmitIntent,
) {
    let expected = ExpectedValue::Text {
        value: desired.into(),
    };
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
        objective,
        form_scope(label, form),
        form_draft(semantic, label, form, "AXPopUpButton", expected),
        choice_plan(semantic, desired),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);

    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        choice_source(1, 120, true, semantic, label, form, initial, false, choices),
        119,
    );
    let run = engine.run.as_ref().expect("the confirmed root is active");
    let goal = &run.goal;
    let pack = &run.pack;
    let env = engine.state.latest().expect("the first Source is current");
    assert!(
        goal.scoped_revisit.as_ref().is_some_and(|pair| {
            scoped_revisit_controls(&goal.target.semantic_target, pair, pack, env).is_some()
        }),
        "the frozen revisit pair resolves to the current page controls; pair={:#?} page_controls={:#?}",
        goal.scoped_revisit,
        env.snapshot
            .objects
            .iter()
            .filter(|object| object.role == "AXRadioButton")
            .collect::<Vec<_>>()
    );
    let binding = engine
        .bindings
        .current(&goal.target.semantic_target)
        .expect("the current Source bound the trusted popup");
    assert!(
        scoped_revisit_primary_binding_object(goal, pack, env, binding).is_some(),
        "a registered popup with an allowed current value and AXShowMenu is a valid primary binding"
    );
    let (show_corr, show_submit) = dispatch_next_action(&mut engine, &mut outputs, start, 230, 1);
    assert_eq!(
        &show_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::ShowMenu,
        },
        "the exact popup uses its native ShowMenu opener"
    );
    let shown_at = finish_submission(
        &mut engine,
        &mut outputs,
        show_corr,
        &show_submit,
        ReceiptStatus::Completed,
    );

    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        choice_source(
            2,
            shown_at + 2,
            true,
            semantic,
            label,
            form,
            initial,
            true,
            choices,
        ),
        shown_at + 1,
    );
    assert!(
        engine.menu_dispatch_matches_current(),
        "the active menu input must retain its exact opener/context proof: {:#?}",
        engine
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
    );
    let dispatch = engine
        .run
        .as_ref()
        .and_then(|run| run.attempt.as_ref())
        .and_then(|attempt| attempt.dispatch.as_ref())
        .expect("the exact menu-item action is waiting for its Source observation");
    let DispatchPlan::Generic(plan) = &dispatch.plan else {
        panic!("menu selection uses a generic AX plan: {dispatch:#?}");
    };
    assert!(
        matches!(engine.menu_input_scope(plan), Ok(Some(_))),
        "the original menu opening authorizes one exact item: {:#?}",
        dispatch
    );
    let observation = dispatch
        .observation
        .native()
        .expect("native menu observation");
    let proof = observation
        .binding_proof
        .as_ref()
        .expect("menu observation carries a binding proof");
    assert_eq!(
        observation.execution_domain,
        ExecutionDomain::ForegroundNative
    );
    assert_eq!(
        observation.purpose,
        interaction_contracts::desktop::device::ObservationPurpose::MenuSelection
    );
    assert_eq!(
        observation.freshness,
        interaction_contracts::desktop::device::SourceFreshness::Verified
    );
    assert_eq!(observation.session_epoch, 1);
    assert_eq!(observation.target_ref, plan.binding.binding_ref.as_str());
    assert_eq!(proof.binding, plan.binding.binding_ref);
    assert_eq!(proof.instance, plan.binding.instance);
    assert_eq!(proof.snapshot_id, plan.binding.snapshot_id);
    assert_eq!(proof.environment_revision, plan.binding.env_revision);
    assert_eq!(proof.layer_revision, plan.binding.layer_revision);
    assert_eq!(proof.fingerprint.digest(), plan.binding.fingerprint_digest);
    assert_eq!(proof.continuity, plan.binding.continuity);
    assert_eq!(proof.device_context, plan.binding.device_context);
    assert!(proof.menu_scope.is_some());
    assert!(proof.focus_scope.is_none());
    let (choice_corr, choice_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, shown_at + 110, 2);
    assert_eq!(
        &choice_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::Press,
        },
        "the menu transaction presses one exact AXMenuItem"
    );
    (engine, outputs, choice_corr, choice_submit)
}

fn radio_choice_press_ready(
    semantic: &str,
    label: &str,
    form: &str,
    initial: &str,
    desired: &str,
    objective: &str,
) -> (
    Engine,
    Vec<Output>,
    u64,
    interaction_contracts::desktop::device::SubmitIntent,
) {
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
        objective,
        form_scope(label, form),
        form_draft(
            semantic,
            label,
            form,
            "AXRadioGroup",
            ExpectedValue::Text {
                value: desired.into(),
            },
        ),
        radio_choice_plan(semantic, desired),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);

    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        radio_choice_source(1, 120, true, semantic, label, form, &[initial]),
        119,
    );
    let run = active_radio_run(
        &engine,
        &outputs,
        "the initial selection Source after Goal confirmation",
    );
    let env = engine
        .state
        .latest()
        .expect("the initial radio Source is current");
    let radio = scoped_radio_choice_control(&run.goal, &run.pack, env)
        .expect("the frozen form resolves one exact radio group and its children");
    assert_eq!(radio.group.role, "AXRadioGroup");
    assert_eq!(radio.group.title.as_deref(), Some(label));
    assert_eq!(radio.group.group.as_deref(), Some(form));
    assert_eq!(radio.selected_label, initial);
    assert_eq!(radio.desired.title.as_deref(), Some(desired));
    assert_eq!(radio.desired.parent, Some(radio.group.instance_ref.clone()));
    let desired_instance = radio.desired.instance_ref.clone();
    let target = run.goal.target.semantic_target.clone();
    let binding = engine
        .bindings
        .current(&target)
        .expect("Engine resolves the public group target to its desired child")
        .clone();
    assert_eq!(binding.semantic_target, target);
    assert_eq!(binding.instance, desired_instance);
    let primary = scoped_revisit_primary_binding_object(&run.goal, &run.pack, env, &binding)
        .expect("the exact child binding retains the frozen group semantic target");
    assert_eq!(primary.instance_ref, desired_instance);

    let (choice_corr, choice_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, 230, 1);
    assert_eq!(
        &choice_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::Press,
        },
        "radio selection presses exactly one child with AXPress"
    );
    let dispatch = engine
        .run
        .as_ref()
        .and_then(|run| run.attempt.as_ref())
        .and_then(|attempt| attempt.dispatch.as_ref())
        .expect("the exact radio child action is pending");
    let DispatchPlan::Generic(plan) = &dispatch.plan else {
        panic!("radio selection uses one generic AX action plan: {dispatch:#?}");
    };
    assert_eq!(plan.binding.semantic_target, target);
    assert_eq!(plan.binding.instance, desired_instance);
    assert_eq!(plan.path, ExecutionPath::Ax);
    let observation = dispatch
        .observation
        .native()
        .expect("radio input uses the ordinary native target observation");
    assert_eq!(
        observation.purpose,
        interaction_contracts::desktop::device::ObservationPurpose::TargetInput
    );
    let proof = observation
        .binding_proof
        .as_ref()
        .expect("radio child input carries the ordinary binding proof");
    assert_eq!(proof.instance, desired_instance);
    assert!(proof.menu_scope.is_none());
    (engine, outputs, choice_corr, choice_submit)
}

fn active_radio_run<'a>(engine: &'a Engine, outputs: &[Output], stage: &str) -> &'a RootRun {
    engine.run.as_ref().unwrap_or_else(|| {
        let projections: Vec<_> = outputs
            .iter()
            .filter_map(|output| match output {
                Output::Ui(projection) => Some(projection),
                _ => None,
            })
            .collect();
        panic!(
            "radio Engine run ended at {stage}: active_state={:?}, last_finished={:?}; projections={:#?}; outputs={:#?}; timeline={:#?}",
            engine.root_state(),
            engine.last_finished(),
            projections,
            outputs,
            engine.timeline.iter().collect::<Vec<_>>(),
        )
    })
}

fn exercise_choice_goal(
    semantic: &str,
    label: &str,
    form: &str,
    initial: &str,
    desired: &str,
    choices: &[&str],
    objective: &str,
) {
    let (mut engine, mut outputs, choice_corr, choice_submit) =
        choice_press_ready(semantic, label, form, initial, desired, choices, objective);
    let selected_at = finish_submission(
        &mut engine,
        &mut outputs,
        choice_corr,
        &choice_submit,
        ReceiptStatus::Completed,
    );

    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        choice_source(
            3,
            selected_at + 2,
            true,
            semantic,
            label,
            form,
            desired,
            false,
            choices,
        ),
        selected_at + 1,
    );
    let (away_corr, away_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, selected_at + 110, 3);
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        choice_source(
            4,
            away_at + 2,
            false,
            semantic,
            label,
            form,
            desired,
            false,
            choices,
        ),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, away_at + 110, 4);
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        choice_source(
            5,
            home_at + 2,
            true,
            semantic,
            label,
            form,
            desired,
            false,
            choices,
        ),
        home_at + 1,
    );

    assert_eq!(
        submit_count(&outputs),
        4,
        "ShowMenu, one choice Press, and the exact away/home revisit are the only inputs"
    );
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "a scoped popup goal completes only after fresh returned-page value verification"
    );
}

fn exercise_radio_choice_goal(
    semantic: &str,
    label: &str,
    form: &str,
    initial: &str,
    desired: &str,
    objective: &str,
) {
    let (mut engine, mut outputs, choice_corr, choice_submit) =
        radio_choice_press_ready(semantic, label, form, initial, desired, objective);
    let selected_at = finish_submission(
        &mut engine,
        &mut outputs,
        choice_corr,
        &choice_submit,
        ReceiptStatus::Completed,
    );

    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        radio_choice_source(2, selected_at + 2, true, semantic, label, form, &[desired]),
        selected_at + 1,
    );
    let run = engine
        .run
        .as_ref()
        .expect("the scoped radio goal remains active");
    let env = engine
        .state
        .latest()
        .expect("fresh post-Press Source has been read");
    let radio = scoped_radio_choice_control(&run.goal, &run.pack, env)
        .expect("post-Press Source proves the selected radio child");
    assert_eq!(radio.selected_label, desired);
    assert_eq!(radio.desired.title.as_deref(), Some(desired));

    let (away_corr, away_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, selected_at + 110, 2);
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        radio_choice_source(3, away_at + 2, false, semantic, label, form, &[]),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, away_at + 110, 3);
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        radio_choice_source(4, home_at + 2, true, semantic, label, form, &[desired]),
        home_at + 1,
    );

    assert_eq!(
        submit_count(&outputs),
        3,
        "one exact radio Press and the frozen away/home pair are the only inputs"
    );
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "the Text-valued radio group completes only after fresh home-page verification"
    );
}

#[test]
fn scoped_goal_compiles_confirms_executes_exact_revisit_and_completes_from_fresh_source() {
    let (mut engine, confirmation, mut outputs) = create_and_confirm();
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);

    let mut action_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        source(1, 120, true, true, TARGET_GROUP),
        119,
    );
    let (ensure_corr, ensure_submit) =
        dispatch_next_action(&mut engine, &mut outputs, action_start, 230, 1);
    assert!(matches!(
        &ensure_submit.intent.native().unwrap().steps[0].primitive,
        interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::Press
        }
    ));
    let after_ensure = finish_submission(
        &mut engine,
        &mut outputs,
        ensure_corr,
        &ensure_submit,
        ReceiptStatus::Completed,
    );

    action_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        source(2, after_ensure + 2, true, false, TARGET_GROUP),
        after_ensure + 1,
    );
    let (away_corr, away_submit) = dispatch_next_action(
        &mut engine,
        &mut outputs,
        action_start,
        after_ensure + 110,
        2,
    );
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );

    action_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        source(3, away_at + 2, false, false, TARGET_GROUP),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, action_start, away_at + 110, 3);
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        source(4, home_at + 2, true, false, TARGET_GROUP),
        home_at + 1,
    );

    assert_eq!(
        submit_count(&outputs),
        3,
        "one checkbox action and the exact away/home pair"
    );
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "Root completes only after the fresh returned-page read proves the requested value"
    );
}

#[test]
fn manual_save_uses_the_single_goal_confirmation_and_requires_a_new_visible_receipt_revision() {
    let (mut engine, mut outputs, save_start, after_ensure) =
        manual_save_ready("Saved manually · revision 4");
    assert_eq!(
        submit_count(&outputs),
        1,
        "the Save action waits for approval"
    );
    let summary = engine
        .run
        .as_ref()
        .unwrap()
        .goal
        .scoped_commit
        .as_ref()
        .unwrap();
    assert_eq!(summary.receipt.kind, ScopedCommitKind::Manual);
    assert_eq!(
        engine.run.as_ref().unwrap().goal.max_effect_class,
        EffectClass::Commit,
        "the one user-confirmed Goal explicitly includes its exact Host-bound Save action"
    );
    assert_eq!(
        summary.action.as_ref().unwrap().semantic_target.as_str(),
        "save-include-archived-en"
    );
    assert!(
        !outputs[save_start..]
            .iter()
            .filter_map(|output| match output {
                Output::Ui(projection) => projection.pending_confirmation.as_ref(),
                _ => None,
            })
            .any(|confirmation| matches!(
                &confirmation.kind,
                interaction_contracts::desktop::operator::ConfirmationKind::Effect { .. }
            )),
        "the exact Save does not ask for a second per-action confirmation"
    );
    let (save_corr, save_submit) =
        dispatch_next_action(&mut engine, &mut outputs, save_start, after_ensure + 100, 2);
    assert_eq!(
        &save_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::Press,
        },
        "the confirmed action presses the exact grouped Save button"
    );
    let saved_at = finish_submission(
        &mut engine,
        &mut outputs,
        save_corr,
        &save_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            3,
            saved_at + 2,
            "setting-include-archived-en",
            "Include archived items",
            "Search",
            true,
            "Saved manually · revision 5",
            true,
        ),
        saved_at + 1,
    );
    assert_eq!(submit_count(&outputs), 2);
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "only a newer exact visible receipt proves the explicit Save"
    );
}

#[test]
fn scoped_commit_ceiling_covers_only_the_exact_host_bound_save_target() {
    let (engine, outputs, _, _) = manual_save_ready("No saved change yet");
    assert_eq!(submit_count(&outputs), 1, "only the primary ensure has run");
    let run = engine.run.as_ref().unwrap();
    let env = engine
        .state
        .latest()
        .expect("a current scoped Source is present");
    let scope = Scope {
        goal: &run.goal,
        plan_targets: &run.plan_targets,
        pack: &run.pack,
        env,
    };
    let primary = &run.goal.target.semantic_target;
    let save = &run
        .goal
        .scoped_commit
        .as_ref()
        .unwrap()
        .action
        .as_ref()
        .unwrap()
        .semantic_target;
    assert_eq!(
        gate(&scope, EffectClass::Commit, Some(primary), false),
        Admission::Deny(DenyReason::TargetNotCovered),
        "the Goal's Commit ceiling does not upgrade its reversible primary control"
    );
    assert_eq!(
        gate(&scope, EffectClass::Commit, Some(save), false),
        Admission::Allow,
        "only the exact Host-bound Save target receives Commit authority"
    );
}

#[test]
fn manual_save_binding_is_refused_if_the_primary_value_changed_in_the_same_source() {
    let (engine, outputs, _, _) = manual_save_ready("No saved change yet");
    assert_eq!(submit_count(&outputs), 1, "Save has not been submitted");
    let run = engine.run.as_ref().unwrap();
    let commit = run.goal.scoped_commit.as_ref().unwrap();
    let action = commit.action.as_ref().unwrap();
    let binding = engine
        .bindings
        .current(&action.semantic_target)
        .expect("the exact Save control is bound")
        .clone();
    let env = engine
        .state
        .latest()
        .expect("the current complete persistence Source is available")
        .clone();
    assert!(scoped_commit_action_binding_object(&run.goal, &run.pack, &env, &binding).is_some());

    let mut changed = env;
    let primary = changed
        .snapshot
        .objects
        .iter_mut()
        .find(|object| object.instance_ref.as_str() == run.goal.target.semantic_target.as_str())
        .expect("the primary checkbox is in the same Source snapshot");
    primary.value = Some(UiValue::Bool(false));
    assert!(
        scoped_commit_action_binding_object(&run.goal, &run.pack, &changed, &binding).is_none(),
        "Save authority requires the desired primary value in the exact same fresh Source"
    );
}

#[test]
fn stale_manual_receipt_and_unknown_save_never_replay_the_save_action() {
    let (mut engine, mut outputs, save_start, after_ensure) =
        manual_save_ready("Saved manually · revision 4");
    let (save_corr, save_submit) =
        dispatch_next_action(&mut engine, &mut outputs, save_start, after_ensure + 100, 2);
    let saved_at = finish_submission(
        &mut engine,
        &mut outputs,
        save_corr,
        &save_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            3,
            saved_at + 2,
            "setting-include-archived-en",
            "Include archived items",
            "Search",
            true,
            "Saved manually · revision 4",
            true,
        ),
        saved_at + 1,
    );
    assert_eq!(submit_count(&outputs), 2, "the old receipt is not accepted");
    let refresh_start = outputs.len();
    let latest_ms = engine.state.latest().unwrap().snapshot.observed_ms;
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            4,
            latest_ms + 2,
            "setting-include-archived-en",
            "Include archived items",
            "Search",
            true,
            "Saved manually · revision 4",
            true,
        ),
        latest_ms + 1,
    );
    assert!(outputs.len() > refresh_start);
    assert_eq!(submit_count(&outputs), 2, "fresh reads cannot replay Save");
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::WaitingEvidence),
        "stale persistence evidence keeps verification pending without retrying Save"
    );
    let deadline = engine.tasks.active().unwrap().row.deadline_ms.unwrap();
    outputs.extend(engine.handle(deadline, Input::Tick));
    assert_eq!(submit_count(&outputs), 2, "root expiry never replays Save");
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Expired),
        "pending verification reaches the original frozen root deadline"
    );

    let (mut unknown, mut unknown_outputs, save_start, after_ensure) =
        manual_save_ready("No saved change yet");
    let (save_corr, save_submit) = dispatch_next_action(
        &mut unknown,
        &mut unknown_outputs,
        save_start,
        after_ensure + 100,
        2,
    );
    finish_submission(
        &mut unknown,
        &mut unknown_outputs,
        save_corr,
        &save_submit,
        ReceiptStatus::Unknown,
    );
    unknown_outputs.extend(unknown.handle(save_submit.intent.latest_start_ms() + 120, Input::Tick));
    assert_eq!(
        submit_count(&unknown_outputs),
        2,
        "Unknown Commit is never retried"
    );
}

#[test]
fn autosave_waits_for_a_fresh_advanced_receipt_before_the_frozen_revisit() {
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped_commit(
        "Turn on Quiet hours.",
        automatic_commit_scope(),
        commit_draft(
            "setting-quiet-hours-en",
            "Quiet hours",
            "Notifications",
            ConfirmationScope::UiRevisit,
        ),
        scoped_plan("setting-quiet-hours-en"),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            1,
            120,
            "setting-quiet-hours-en",
            "Quiet hours",
            "Notifications",
            false,
            "Saved automatically · revision 7",
            false,
        ),
        119,
    );
    let (ensure_corr, ensure_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, 230, 1);
    let after_ensure = finish_submission(
        &mut engine,
        &mut outputs,
        ensure_corr,
        &ensure_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            2,
            after_ensure + 2,
            "setting-quiet-hours-en",
            "Quiet hours",
            "Notifications",
            true,
            "Saving changes…",
            false,
        ),
        after_ensure + 1,
    );
    let first_poll_floor = engine.run.as_ref().unwrap().wait.unwrap().floor_ms;
    let input_end = engine
        .run
        .as_ref()
        .unwrap()
        .verification
        .last_input_end_ms()
        .expect("the ensure input has a Source-confirmation floor");
    assert!(first_poll_floor >= input_end + 800);
    assert_eq!(submit_count(&outputs), 1, "autosave polling emits no input");

    feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            3,
            first_poll_floor + 1,
            "setting-quiet-hours-en",
            "Quiet hours",
            "Notifications",
            true,
            "Saving changes…",
            false,
        ),
        first_poll_floor,
    );
    let second_poll_floor = engine.run.as_ref().unwrap().wait.unwrap().floor_ms;
    assert!(
        second_poll_floor > first_poll_floor,
        "pending status gets one more bounded Source poll"
    );
    assert_eq!(submit_count(&outputs), 1);

    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            4,
            second_poll_floor + 1,
            "setting-quiet-hours-en",
            "Quiet hours",
            "Notifications",
            true,
            "Saved automatically · revision 8",
            false,
        ),
        second_poll_floor,
    );
    let (away_corr, away_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, second_poll_floor + 110, 2);
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        source(5, away_at + 2, false, false, "Notifications"),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, away_at + 110, 3);
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        persistence_source(
            6,
            home_at + 2,
            "setting-quiet-hours-en",
            "Quiet hours",
            "Notifications",
            true,
            "Saved automatically · revision 8",
            false,
        ),
        home_at + 1,
    );
    assert_eq!(
        submit_count(&outputs),
        3,
        "checkbox plus exact away/home revisit"
    );
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn a_group_mismatch_and_unknown_away_receipt_never_emit_a_followup_input() {
    let (mut mismatched, confirmation, mut mismatch_outputs) = create_and_confirm();
    confirm_root(&mut mismatched, &confirmation, &mut mismatch_outputs, 110);
    feed_fresh_source(
        &mut mismatched,
        &mut mismatch_outputs,
        source(1, 120, true, true, "Workspace notifications"),
        119,
    );
    assert_eq!(submit_count(&mismatch_outputs), 0);
    assert!(
        mismatched
            .bindings
            .current(&SemanticTargetRef::new(TARGET).unwrap())
            .is_none()
    );

    let (mut engine, confirmation, mut outputs) = create_and_confirm();
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        source(1, 120, true, true, TARGET_GROUP),
        119,
    );
    let (ensure_corr, ensure_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, 230, 1);
    let after_ensure = finish_submission(
        &mut engine,
        &mut outputs,
        ensure_corr,
        &ensure_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        source(2, after_ensure + 2, true, false, TARGET_GROUP),
        after_ensure + 1,
    );
    let (away_corr, away_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, after_ensure + 110, 2);
    finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Unknown,
    );
    outputs.extend(engine.handle(away_submit.intent.latest_start_ms() + 120, Input::Tick));
    assert_eq!(
        submit_count(&outputs),
        2,
        "an unknown away result cannot trigger a return click or replay"
    );
    assert_ne!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn scoped_forms_set_single_line_multiline_and_boolean_values_then_revisit_exactly() {
    exercise_form_goal(
        "form-display-name-en",
        "Display name",
        "Profile",
        "AXTextField",
        UiValue::Text("Existing name".into()),
        ExpectedValue::Text {
            value: "Avery Chen".into(),
        },
    );
    exercise_form_goal(
        "form-notes-en",
        "Notes",
        "Profile",
        "AXTextArea",
        UiValue::Text("old notes".into()),
        ExpectedValue::Text {
            value: "First line\nSecond line".into(),
        },
    );
    exercise_form_goal(
        "form-notify-on-change-en",
        "Notify on change",
        "Preferences",
        "AXCheckBox",
        UiValue::Bool(false),
        ExpectedValue::Bool { value: true },
    );
}

#[test]
fn scoped_region_code_requires_fresh_valid_feedback_in_english_and_chinese() {
    exercise_region_code_goal(
        "form-region-code-en",
        "Region code",
        "Account",
        "Correct Region code to CA-2001 so the form passes validation.",
    );
    exercise_region_code_goal(
        "form-region-code-zh",
        "地区代码",
        "账户",
        "将“地区代码”更正为 CA-2001，使表单通过验证。",
    );
}

#[test]
fn scoped_region_code_rejects_missing_invalid_duplicate_and_foreign_feedback() {
    let desired = "CA-2001";
    for (shape, name) in [
        (RegionFeedbackShape::Missing, "missing"),
        (RegionFeedbackShape::Invalid, "invalid"),
        (RegionFeedbackShape::Duplicate, "duplicate"),
        (RegionFeedbackShape::ForeignForm, "foreign-form"),
        (RegionFeedbackShape::Offscreen, "offscreen"),
        (RegionFeedbackShape::WrongRole, "wrong-role"),
        (
            RegionFeedbackShape::SameNamedForeignGroup,
            "same-named-foreign-group",
        ),
    ] {
        let semantic = "form-region-code-en";
        let (mut engine, mut outputs, set_at) = region_code_after_set_value(
            semantic,
            "Region code",
            "Account",
            "??",
            desired,
            "Correct Region code to CA-2001 so the form passes validation.",
        );
        assert!(
            engine.run.is_some(),
            "the completed SetValue leaves the confirmed Goal active for {name} feedback"
        );
        let invalid_feedback = region_feedback_variant_source(
            3,
            set_at + 2,
            semantic,
            "Region code",
            "Account",
            desired,
            shape,
        );
        feed_fresh_source(&mut engine, &mut outputs, invalid_feedback, set_at + 1);
        assert_eq!(
            submit_count(&outputs),
            2,
            "{name} feedback cannot authorize an away-page revisit"
        );
        assert_ne!(
            engine.root_state().or_else(|| engine.last_finished()),
            Some(interaction_contracts::desktop::task::RootState::Completed),
            "field text alone cannot satisfy validation for {name} feedback"
        );
    }
}

#[test]
fn scoped_region_code_requires_valid_feedback_again_after_returning_home() {
    let semantic = "form-region-code-en";
    let label = "Region code";
    let form = "Account";
    let desired = "CA-2001";
    let (mut engine, mut outputs, set_at) = region_code_after_set_value(
        semantic,
        label,
        form,
        "??",
        desired,
        "Correct Region code to CA-2001 so the form passes validation.",
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        region_validation_source(
            3,
            set_at + 2,
            true,
            semantic,
            label,
            form,
            desired,
            true,
            Some(REGION_VALID_TEXT_EN),
        ),
        set_at + 1,
    );
    let (away_corr, away_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, set_at + 110, 3);
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        region_validation_source(
            4,
            away_at + 2,
            false,
            semantic,
            label,
            form,
            desired,
            false,
            None,
        ),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, away_at + 110, 4);
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        region_validation_source(
            5,
            home_at + 2,
            true,
            semantic,
            label,
            form,
            desired,
            false,
            Some(REGION_INVALID_TEXT_EN),
        ),
        home_at + 1,
    );

    assert_eq!(submit_count(&outputs), 4);
    assert_ne!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "a stale or invalidated form result on return cannot complete the Goal"
    );
}

#[test]
fn scoped_region_code_unknown_set_value_is_never_replayed() {
    let semantic = "form-region-code-en";
    let (mut engine, mut outputs, set_corr, set_submit) = region_code_set_value_pending(
        semantic,
        "Region code",
        "Account",
        "??",
        "CA-2001",
        "Correct Region code to CA-2001 so the form passes validation.",
    );
    let unknown_at = finish_submission(
        &mut engine,
        &mut outputs,
        set_corr,
        &set_submit,
        ReceiptStatus::Unknown,
    );
    let refresh_start = outputs.len();
    outputs.extend(engine.handle(set_submit.intent.latest_start_ms() + 120, Input::Tick));
    let refresh = outputs[refresh_start..]
        .iter()
        .rev()
        .find_map(|output| match output {
            Output::Source {
                generation,
                command: SourceCommand::RefreshNow { .. },
            } => Some(*generation),
            _ => None,
        });
    if refresh.is_some() {
        feed_fresh_source(
            &mut engine,
            &mut outputs,
            region_validation_source(
                3,
                unknown_at + 2,
                true,
                semantic,
                "Region code",
                "Account",
                "CA-2001",
                true,
                Some(REGION_VALID_TEXT_EN),
            ),
            unknown_at + 1,
        );
    }
    assert_eq!(
        submit_count(&outputs),
        2,
        "Unknown does not replay SetValue or advance to away/home navigation"
    );
    assert_ne!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn scoped_popup_read_select_and_revisit_completes_from_fresh_source() {
    exercise_choice_goal(
        "form-reminder-cadence-en",
        "Reminder cadence",
        "Preferences",
        "Daily",
        "Weekly",
        &["Daily", "Weekly", "Monthly"],
        "Set Reminder cadence to Weekly.",
    );
    exercise_choice_goal(
        "form-reminder-cadence-zh",
        "提醒频率",
        "偏好设置",
        "每天",
        "每周",
        &["每天", "每周", "每月"],
        "将提醒频率设置为每周。",
    );
}

#[test]
fn scoped_radio_choice_selects_one_child_and_revisits_in_english_and_chinese() {
    exercise_radio_choice_goal(
        "form-summary-grouping-en",
        "Summary grouping",
        "Preferences",
        "By day",
        "By week",
        "Set Summary grouping to By week.",
    );
    exercise_radio_choice_goal(
        "form-summary-grouping-zh",
        "摘要分组",
        "偏好设置",
        "按天",
        "按周",
        "将摘要分组设为按周。",
    );
}

#[test]
fn scoped_radio_choice_already_selected_skips_press_and_revisits() {
    let semantic = "form-summary-grouping-en";
    let label = "Summary grouping";
    let form = "Preferences";
    let desired = "By week";
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
        "Keep Summary grouping at By week.",
        form_scope(label, form),
        form_draft(
            semantic,
            label,
            form,
            "AXRadioGroup",
            ExpectedValue::Text {
                value: desired.into(),
            },
        ),
        radio_choice_plan(semantic, desired),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        radio_choice_source(1, 120, true, semantic, label, form, &[desired]),
        119,
    );
    let run = active_radio_run(
        &engine,
        &outputs,
        "the already-selected radio Source after Goal confirmation",
    );
    let env = engine
        .state
        .latest()
        .expect("the current radio Source is available");
    let radio = scoped_radio_choice_control(&run.goal, &run.pack, env)
        .expect("the exact registered radio group is present");
    assert_eq!(radio.selected_label, desired);
    assert_eq!(
        submit_count(&outputs),
        0,
        "an already-selected option is not pressed"
    );

    let (away_corr, away_submit) = dispatch_next_action(&mut engine, &mut outputs, start, 230, 1);
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        radio_choice_source(2, away_at + 2, false, semantic, label, form, &[]),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, away_at + 110, 2);
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        radio_choice_source(3, home_at + 2, true, semantic, label, form, &[desired]),
        home_at + 1,
    );

    assert_eq!(
        submit_count(&outputs),
        2,
        "only the frozen away/home pair is submitted"
    );
    assert_eq!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn scoped_radio_choice_never_falls_back_without_child_press() {
    let semantic = "form-summary-grouping-en";
    let label = "Summary grouping";
    let form = "Preferences";
    let desired = "By week";
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
        "Set Summary grouping to By week.",
        form_scope(label, form),
        form_draft(
            semantic,
            label,
            form,
            "AXRadioGroup",
            ExpectedValue::Text {
                value: desired.into(),
            },
        ),
        radio_choice_plan(semantic, desired),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    assert!(engine.run.is_some(), "the confirmed Goal is active");

    let mut snapshot = radio_choice_source(1, 120, true, semantic, label, form, &["By day"]);
    let desired_child = snapshot
        .objects
        .iter_mut()
        .find(|object| object.title.as_deref() == Some(desired))
        .expect("the registered desired option exists in Source");
    desired_child.actions = vec![AxActionName::SetValue, AxActionName::ShowMenu];
    snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    snapshot
        .validate()
        .expect("the fallback-action radio Source remains complete");
    feed_fresh_source(&mut engine, &mut outputs, snapshot, 119);

    assert_eq!(
        submit_count(&outputs),
        0,
        "a radio choice requires AXPress and never falls back to SetValue or ShowMenu"
    );
    assert_ne!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn scoped_radio_source_rejects_wrong_parent_and_multiple_selected_children() {
    for invalid_shape in ["wrong_parent", "multiple_selected"] {
        let selected = if invalid_shape == "multiple_selected" {
            &["By day", "By week"][..]
        } else {
            &["By day"][..]
        };
        let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
            "Set Summary grouping to By week.",
            form_scope("Summary grouping", "Preferences"),
            form_draft(
                "form-summary-grouping-en",
                "Summary grouping",
                "Preferences",
                "AXRadioGroup",
                ExpectedValue::Text {
                    value: "By week".into(),
                },
            ),
            radio_choice_plan("form-summary-grouping-en", "By week"),
        );
        confirm_root(&mut engine, &confirmation, &mut outputs, 110);
        assert!(
            engine.run.is_some(),
            "the confirmed Goal is active before malformed Source"
        );
        let mut snapshot = radio_choice_source(
            1,
            120,
            true,
            "form-summary-grouping-en",
            "Summary grouping",
            "Preferences",
            selected,
        );
        if invalid_shape == "wrong_parent" {
            let group = snapshot
                .objects
                .iter_mut()
                .find(|object| object.instance_ref.as_str() == "summary-grouping")
                .expect("the radio group is in the current Source tree");
            group.parent = Some(ObjectRef::new("window-root").unwrap());
        }
        snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
        snapshot
            .validate()
            .expect("the malformed radio context remains a complete Source tree");
        feed_fresh_source(&mut engine, &mut outputs, snapshot, 119);

        assert_eq!(
            submit_count(&outputs),
            0,
            "radio input is denied for the {invalid_shape} Source shape"
        );
        assert_ne!(
            engine.root_state().or_else(|| engine.last_finished()),
            Some(interaction_contracts::desktop::task::RootState::Completed)
        );
    }
}

#[test]
fn scoped_radio_choice_requires_fresh_and_returned_values_to_match() {
    let (mut wrong_after_press, mut wrong_after_press_outputs, choice_corr, choice_submit) =
        radio_choice_press_ready(
            "form-summary-grouping-en",
            "Summary grouping",
            "Preferences",
            "By day",
            "By week",
            "Set Summary grouping to By week.",
        );
    let selected_at = finish_submission(
        &mut wrong_after_press,
        &mut wrong_after_press_outputs,
        choice_corr,
        &choice_submit,
        ReceiptStatus::Completed,
    );
    assert!(
        wrong_after_press.run.is_some(),
        "the completed Press leaves the confirmed Goal active for fresh verification"
    );
    feed_fresh_source(
        &mut wrong_after_press,
        &mut wrong_after_press_outputs,
        radio_choice_source(
            2,
            selected_at + 2,
            true,
            "form-summary-grouping-en",
            "Summary grouping",
            "Preferences",
            &["By day"],
        ),
        selected_at + 1,
    );
    assert_eq!(submit_count(&wrong_after_press_outputs), 1);
    assert_ne!(
        wrong_after_press
            .root_state()
            .or_else(|| wrong_after_press.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "a fresh Source still showing the old option cannot start the revisit"
    );

    let (mut reverted_home, mut reverted_home_outputs, choice_corr, choice_submit) =
        radio_choice_press_ready(
            "form-summary-grouping-en",
            "Summary grouping",
            "Preferences",
            "By day",
            "By week",
            "Set Summary grouping to By week.",
        );
    let selected_at = finish_submission(
        &mut reverted_home,
        &mut reverted_home_outputs,
        choice_corr,
        &choice_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut reverted_home,
        &mut reverted_home_outputs,
        radio_choice_source(
            2,
            selected_at + 2,
            true,
            "form-summary-grouping-en",
            "Summary grouping",
            "Preferences",
            &["By week"],
        ),
        selected_at + 1,
    );
    assert!(
        reverted_home.run.is_some(),
        "the selected value is freshly verified"
    );
    let (away_corr, away_submit) = dispatch_next_action(
        &mut reverted_home,
        &mut reverted_home_outputs,
        start,
        selected_at + 110,
        2,
    );
    let away_at = finish_submission(
        &mut reverted_home,
        &mut reverted_home_outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut reverted_home,
        &mut reverted_home_outputs,
        radio_choice_source(
            3,
            away_at + 2,
            false,
            "form-summary-grouping-en",
            "Summary grouping",
            "Preferences",
            &[],
        ),
        away_at + 1,
    );
    let (home_corr, home_submit) = dispatch_next_action(
        &mut reverted_home,
        &mut reverted_home_outputs,
        start,
        away_at + 110,
        3,
    );
    let home_at = finish_submission(
        &mut reverted_home,
        &mut reverted_home_outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut reverted_home,
        &mut reverted_home_outputs,
        radio_choice_source(
            4,
            home_at + 2,
            true,
            "form-summary-grouping-en",
            "Summary grouping",
            "Preferences",
            &["By day"],
        ),
        home_at + 1,
    );
    assert_eq!(submit_count(&reverted_home_outputs), 3);
    assert_ne!(
        reverted_home
            .root_state()
            .or_else(|| reverted_home.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "the home-page value must still equal the frozen desired Text"
    );
}

#[test]
fn scoped_radio_reparented_child_is_rejected_before_permit_activation() {
    let semantic = "form-summary-grouping-en";
    let label = "Summary grouping";
    let form = "Preferences";
    let desired = "By week";
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
        "Set Summary grouping to By week.",
        form_scope(label, form),
        form_draft(
            semantic,
            label,
            form,
            "AXRadioGroup",
            ExpectedValue::Text {
                value: desired.into(),
            },
        ),
        radio_choice_plan(semantic, desired),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        radio_choice_source(1, 120, true, semantic, label, form, &["By day"]),
        119,
    );
    let run = active_radio_run(&engine, &outputs, "the valid pre-reservation radio Goal");
    let valid_plan = radio_choice_plan(semantic, desired);
    assert!(scoped_revisit_plan_is_allowed(
        &run.goal,
        &run.pack,
        &valid_plan
    ));
    let mut missing_pair = run.goal.clone();
    missing_pair.scoped_revisit = None;
    assert!(
        !scoped_revisit_plan_is_allowed(&missing_pair, &run.pack, &valid_plan),
        "RadioChoice cannot fall through to generic selection without its frozen revisit pair"
    );

    let (observe_corr, _) = find_device_command(&outputs, start, |command| {
        matches!(command, DeviceCommand::Observe(_))
    })
    .expect("the radio action first requires a fresh target observation");
    let grant_start = outputs.len();
    outputs.extend(engine.handle(
        230,
        Input::Device {
            corr: observe_corr,
            reply: DeviceReply::Ack(CommandAck::ok()),
        },
    ));
    let (grant_corr, _) = find_device_command(&outputs, grant_start, |command| {
        matches!(command, DeviceCommand::GrantPermit(_))
    })
    .expect("the exact child binding reaches the ordinary permit reservation");

    let mut reparented = radio_choice_source(2, 240, true, semantic, label, form, &["By day"]);
    let desired_child = reparented
        .objects
        .iter_mut()
        .find(|object| object.title.as_deref() == Some(desired))
        .expect("the desired child remains present in the new Source tree");
    desired_child.parent = Some(ObjectRef::new("preferences-form").unwrap());
    reparented.observer_coverage = ObserverCoverage::for_projected_objects(&reparented.objects);
    reparented
        .validate()
        .expect("the reparented Source remains a valid complete tree");
    let arrival = reparented.observed_ms + 1;
    outputs.extend(engine.handle(arrival, Input::Structure(Box::new(reparented))));
    let settled_at = arrival + engine.cfg.native_evidence_quiet_ms + 1;
    outputs.extend(engine.handle(settled_at, Input::Tick));
    outputs.extend(engine.handle(
        settled_at + 1,
        Input::Device {
            corr: grant_corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted {
                permit: PermitRef {
                    id: "scoped-revisit-permit-1".into(),
                    generation: 1,
                },
            }),
        },
    ));

    assert_eq!(
        submit_count(&outputs),
        0,
        "a child reparented after reservation cannot be activated"
    );
    assert_ne!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn scoped_radio_unknown_press_never_replays_or_completes() {
    let (mut engine, mut outputs, choice_corr, choice_submit) = radio_choice_press_ready(
        "form-summary-grouping-en",
        "Summary grouping",
        "Preferences",
        "By day",
        "By week",
        "Set Summary grouping to By week.",
    );
    let unknown_at = finish_submission(
        &mut engine,
        &mut outputs,
        choice_corr,
        &choice_submit,
        ReceiptStatus::Unknown,
    );
    let refresh_start = outputs.len();
    outputs.extend(engine.handle(choice_submit.intent.latest_start_ms() + 120, Input::Tick));
    let refresh = outputs[refresh_start..]
        .iter()
        .rev()
        .find_map(|output| match output {
            Output::Source {
                generation,
                command: SourceCommand::RefreshNow { .. },
            } => Some(*generation),
            _ => None,
        });
    if refresh.is_some() {
        feed_fresh_source(
            &mut engine,
            &mut outputs,
            radio_choice_source(
                2,
                unknown_at + 2,
                true,
                "form-summary-grouping-en",
                "Summary grouping",
                "Preferences",
                &["By week"],
            ),
            unknown_at + 1,
        );
    }
    assert_eq!(
        submit_count(&outputs),
        1,
        "Unknown never retries the radio Press"
    );
    assert_ne!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn scoped_popup_plan_gate_rejects_wrong_choice_target_and_foreign_read() {
    let semantic = "form-reminder-cadence-en";
    let label = "Reminder cadence";
    let form = "Preferences";
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
        "Set Reminder cadence to Weekly.",
        form_scope(label, form),
        form_draft(
            semantic,
            label,
            form,
            "AXPopUpButton",
            ExpectedValue::Text {
                value: "Weekly".into(),
            },
        ),
        choice_plan(semantic, "Weekly"),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    let run = engine
        .run
        .as_ref()
        .expect("the scoped candidate goal is active");
    let valid = choice_plan(semantic, "Weekly");
    assert!(scoped_revisit_plan_is_allowed(&run.goal, &run.pack, &valid));

    let wrong_choice = choice_plan(semantic, "Quarterly");
    assert!(!scoped_revisit_plan_is_allowed(
        &run.goal,
        &run.pack,
        &wrong_choice
    ));

    let wrong_target = choice_plan("form-notify-on-change-en", "Weekly");
    assert!(!scoped_revisit_plan_is_allowed(
        &run.goal,
        &run.pack,
        &wrong_target
    ));

    let mut foreign_read = choice_plan(semantic, "Weekly");
    foreign_read.subgoals[0].target =
        Some(SemanticTargetRef::new("form-notify-on-change-en").unwrap());
    assert!(!scoped_revisit_plan_is_allowed(
        &run.goal,
        &run.pack,
        &foreign_read
    ));
}

#[test]
fn runtime_popup_source_rejects_unregistered_value_and_wrong_target() {
    for (current_value, current_label, has_show_menu) in [
        ("Quarterly", "Reminder cadence", true),
        ("Daily", "A different popup", true),
        ("Daily", "Reminder cadence", false),
    ] {
        let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
            "Set Reminder cadence to Weekly.",
            form_scope("Reminder cadence", "Preferences"),
            form_draft(
                "form-reminder-cadence-en",
                "Reminder cadence",
                "Preferences",
                "AXPopUpButton",
                ExpectedValue::Text {
                    value: "Weekly".into(),
                },
            ),
            choice_plan("form-reminder-cadence-en", "Weekly"),
        );
        confirm_root(&mut engine, &confirmation, &mut outputs, 110);
        let mut snapshot = choice_source(
            1,
            120,
            true,
            "form-reminder-cadence-en",
            current_label,
            "Preferences",
            current_value,
            false,
            &["Daily", "Weekly", "Monthly"],
        );
        if !has_show_menu {
            snapshot
                .objects
                .iter_mut()
                .find(|object| object.instance_ref.as_str() == "form-reminder-cadence-en")
                .unwrap()
                .actions
                .clear();
        }
        feed_fresh_source(&mut engine, &mut outputs, snapshot, 119);
        assert_eq!(
            submit_count(&outputs),
            0,
            "an unregistered current value or changed popup label cannot reach ShowMenu"
        );
    }
}

#[test]
fn popup_unknown_choice_and_wrong_closed_value_never_replay_or_revisit() {
    let (mut unknown, mut unknown_outputs, choice_corr, choice_submit) = choice_press_ready(
        "form-reminder-cadence-en",
        "Reminder cadence",
        "Preferences",
        "Daily",
        "Weekly",
        &["Daily", "Weekly", "Monthly"],
        "Set Reminder cadence to Weekly.",
    );
    let unknown_at = finish_submission(
        &mut unknown,
        &mut unknown_outputs,
        choice_corr,
        &choice_submit,
        ReceiptStatus::Unknown,
    );
    unknown_outputs
        .extend(unknown.handle(choice_submit.intent.latest_start_ms() + 120, Input::Tick));
    if refresh_generation(&unknown_outputs).is_some() {
        feed_fresh_source(
            &mut unknown,
            &mut unknown_outputs,
            choice_source(
                3,
                unknown_at + 2,
                true,
                "form-reminder-cadence-en",
                "Reminder cadence",
                "Preferences",
                "Weekly",
                false,
                &["Daily", "Weekly", "Monthly"],
            ),
            unknown_at + 1,
        );
    }
    assert_eq!(
        submit_count(&unknown_outputs),
        2,
        "Unknown never replays the menu choice"
    );
    assert_ne!(
        unknown.root_state().or_else(|| unknown.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );

    let (mut wrong_value, confirmation, mut wrong_outputs) = create_and_confirm_scoped(
        "Set Reminder cadence to Weekly.",
        form_scope("Reminder cadence", "Preferences"),
        form_draft(
            "form-reminder-cadence-en",
            "Reminder cadence",
            "Preferences",
            "AXPopUpButton",
            ExpectedValue::Text {
                value: "Weekly".into(),
            },
        ),
        choice_plan("form-reminder-cadence-en", "Weekly"),
    );
    confirm_root(&mut wrong_value, &confirmation, &mut wrong_outputs, 110);
    let start = feed_fresh_source(
        &mut wrong_value,
        &mut wrong_outputs,
        choice_source(
            1,
            120,
            true,
            "form-reminder-cadence-en",
            "Reminder cadence",
            "Preferences",
            "Daily",
            false,
            &["Daily", "Weekly", "Monthly"],
        ),
        119,
    );
    let (show_corr, show_submit) =
        dispatch_next_action(&mut wrong_value, &mut wrong_outputs, start, 230, 1);
    let shown_at = finish_submission(
        &mut wrong_value,
        &mut wrong_outputs,
        show_corr,
        &show_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut wrong_value,
        &mut wrong_outputs,
        choice_source(
            2,
            shown_at + 2,
            true,
            "form-reminder-cadence-en",
            "Reminder cadence",
            "Preferences",
            "Daily",
            true,
            &["Daily", "Weekly", "Monthly"],
        ),
        shown_at + 1,
    );
    let (choice_corr, choice_submit) = dispatch_next_action(
        &mut wrong_value,
        &mut wrong_outputs,
        start,
        shown_at + 110,
        2,
    );
    let selected_at = finish_submission(
        &mut wrong_value,
        &mut wrong_outputs,
        choice_corr,
        &choice_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut wrong_value,
        &mut wrong_outputs,
        choice_source(
            3,
            selected_at + 2,
            true,
            "form-reminder-cadence-en",
            "Reminder cadence",
            "Preferences",
            "Daily",
            false,
            &["Daily", "Weekly", "Monthly"],
        ),
        selected_at + 1,
    );
    assert_eq!(
        submit_count(&wrong_outputs),
        2,
        "a closed popup that still shows the old value cannot start revisit"
    );
    assert_ne!(
        wrong_value
            .root_state()
            .or_else(|| wrong_value.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed)
    );
}

#[test]
fn popup_revisit_requires_the_requested_value_after_returning_home() {
    let (mut engine, mut outputs, choice_corr, choice_submit) = choice_press_ready(
        "form-reminder-cadence-en",
        "Reminder cadence",
        "Preferences",
        "Daily",
        "Weekly",
        &["Daily", "Weekly", "Monthly"],
        "Set Reminder cadence to Weekly.",
    );
    let selected_at = finish_submission(
        &mut engine,
        &mut outputs,
        choice_corr,
        &choice_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        choice_source(
            3,
            selected_at + 2,
            true,
            "form-reminder-cadence-en",
            "Reminder cadence",
            "Preferences",
            "Weekly",
            false,
            &["Daily", "Weekly", "Monthly"],
        ),
        selected_at + 1,
    );
    let (away_corr, away_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, selected_at + 110, 3);
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        choice_source(
            4,
            away_at + 2,
            false,
            "form-reminder-cadence-en",
            "Reminder cadence",
            "Preferences",
            "Weekly",
            false,
            &["Daily", "Weekly", "Monthly"],
        ),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, away_at + 110, 4);
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        choice_source(
            5,
            home_at + 2,
            true,
            "form-reminder-cadence-en",
            "Reminder cadence",
            "Preferences",
            "Daily",
            false,
            &["Daily", "Weekly", "Monthly"],
        ),
        home_at + 1,
    );
    assert_eq!(
        submit_count(&outputs),
        4,
        "the selected value is never replayed"
    );
    assert_ne!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "revisit cannot accept a stale or reverted popup value"
    );
}

#[test]
fn engine_rejects_form_context_and_value_type_mismatches_before_confirmation() {
    let scope = form_scope("Display name", "Profile");
    let wrong_form = form_draft(
        "form-display-name-en",
        "Display name",
        "Preferences",
        "AXTextField",
        ExpectedValue::Text {
            value: "Avery Chen".into(),
        },
    );
    let (mut engine, request, mut outputs) = start_scoped_task("Set Display name.", scope.clone());
    submit_proposal(
        &mut engine,
        &request,
        &mut outputs,
        wrong_form,
        scoped_plan("form-display-name-en"),
    );
    assert!(outputs.iter().all(|output| {
        !matches!(output, Output::Ui(projection) if projection.pending_confirmation.is_some())
            && !matches!(output, Output::Device { command, .. } if matches!(command.as_ref(), DeviceCommand::RegisterRoot(_)))
    }));

    let wrong_type = form_draft(
        "form-display-name-en",
        "Display name",
        "Profile",
        "AXTextField",
        ExpectedValue::Bool { value: true },
    );
    let (mut engine, request, mut outputs) = start_scoped_task("Set Display name.", scope);
    submit_proposal(
        &mut engine,
        &request,
        &mut outputs,
        wrong_type,
        scoped_plan("form-display-name-en"),
    );
    assert!(outputs.iter().all(|output| {
        !matches!(output, Output::Ui(projection) if projection.pending_confirmation.is_some())
            && !matches!(output, Output::Device { command, .. } if matches!(command.as_ref(), DeviceCommand::RegisterRoot(_)))
    }));
}

#[test]
fn runtime_form_source_rejects_a_wrong_context_or_accessibility_role() {
    let display_scope = form_scope("Display name", "Profile");
    let display_draft = || {
        form_draft(
            "form-display-name-en",
            "Display name",
            "Profile",
            "AXTextField",
            ExpectedValue::Text {
                value: "Avery Chen".into(),
            },
        )
    };

    let (mut wrong_context, confirmation, mut context_outputs) = create_and_confirm_scoped(
        "Set Display name.",
        display_scope.clone(),
        display_draft(),
        scoped_plan("form-display-name-en"),
    );
    confirm_root(&mut wrong_context, &confirmation, &mut context_outputs, 110);
    feed_fresh_source(
        &mut wrong_context,
        &mut context_outputs,
        form_source(
            1,
            120,
            true,
            "form-display-name-en",
            "Display name",
            "Preferences",
            "AXTextField",
            UiValue::Text("Existing name".into()),
        ),
        119,
    );
    assert_eq!(submit_count(&context_outputs), 0);

    let (mut wrong_role, confirmation, mut role_outputs) = create_and_confirm_scoped(
        "Set Display name.",
        display_scope,
        display_draft(),
        scoped_plan("form-display-name-en"),
    );
    confirm_root(&mut wrong_role, &confirmation, &mut role_outputs, 110);
    feed_fresh_source(
        &mut wrong_role,
        &mut role_outputs,
        form_source(
            1,
            120,
            true,
            "form-display-name-en",
            "Display name",
            "Profile",
            "AXTextArea",
            UiValue::Text("Existing name".into()),
        ),
        119,
    );
    assert_eq!(submit_count(&role_outputs), 0);

    let (mut ambiguous, confirmation, mut ambiguous_outputs) = create_and_confirm_scoped(
        "Set Display name.",
        form_scope("Display name", "Profile"),
        display_draft(),
        scoped_plan("form-display-name-en"),
    );
    confirm_root(&mut ambiguous, &confirmation, &mut ambiguous_outputs, 110);
    let mut ambiguous_source = form_source(
        1,
        120,
        true,
        "form-display-name-en",
        "Display name",
        "Profile",
        "AXTextField",
        UiValue::Text("Existing name".into()),
    );
    let mut duplicate = ambiguous_source
        .objects
        .iter()
        .find(|object| object.instance_ref.as_str() == "form-display-name-en")
        .unwrap()
        .clone();
    duplicate.instance_ref = ObjectRef::new("duplicate-display-name-field").unwrap();
    duplicate.frame = rect(320.0, 82.0, 260.0, 36.0);
    ambiguous_source.objects.push(duplicate);
    ambiguous_source.observer_coverage =
        ObserverCoverage::for_projected_objects(&ambiguous_source.objects);
    ambiguous_source
        .validate()
        .expect("the ambiguous duplicate is still a complete valid Source read");
    feed_fresh_source(
        &mut ambiguous,
        &mut ambiguous_outputs,
        ambiguous_source,
        119,
    );
    assert_eq!(submit_count(&ambiguous_outputs), 0);
}

#[test]
fn runtime_primary_binding_rejects_unicode_line_separators_in_text_fields() {
    let (mut engine, confirmation, mut outputs) = create_and_confirm_scoped(
        "Set Display name to a safe value.",
        form_scope("Display name", "Profile"),
        form_draft(
            "form-display-name-en",
            "Display name",
            "Profile",
            "AXTextField",
            ExpectedValue::Text {
                value: "Avery Chen".into(),
            },
        ),
        scoped_plan("form-display-name-en"),
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    feed_fresh_source(
        &mut engine,
        &mut outputs,
        form_source(
            1,
            120,
            true,
            "form-display-name-en",
            "Display name",
            "Profile",
            "AXTextField",
            UiValue::Text("Existing name".into()),
        ),
        119,
    );

    let goal = engine.run.as_ref().unwrap().goal.clone();
    let env = engine.state.latest().unwrap().clone();
    let target = env
        .snapshot
        .objects
        .iter()
        .find(|object| object.instance_ref.as_str() == "form-display-name-en")
        .expect("the otherwise-valid Source contains the exact registered text field");
    let binding = engine
        .bindings
        .bind_instance(
            &env,
            goal.target.semantic_target.clone(),
            &target.instance_ref,
        )
        .expect("the exact primary field has a current binding");
    let pack = engine.run.as_ref().unwrap().pack.clone();

    assert!(goal.validate().is_ok());
    assert!(scoped_revisit_primary_binding_object(&goal, &pack, &env, &binding).is_some());

    for separator in ['\u{2028}', '\u{2029}'] {
        let mut candidate = goal.clone();
        candidate.desired = ExpectedValue::Text {
            value: format!("before{separator}after"),
        };
        let target = candidate.target.semantic_target.clone();
        candidate.required = Predicate::And {
            of: vec![
                Predicate::ContextMatches {
                    app: candidate.scope.app.clone(),
                },
                Predicate::ReadValueEquals {
                    target: target.clone(),
                    expected: candidate.desired.clone(),
                    observed_after_action: true,
                },
                Predicate::Revisited { target },
            ],
        };

        assert!(
            candidate.validate().is_ok(),
            "U+{:04X} is contract-valid",
            separator as u32
        );
        assert!(
            scoped_revisit_primary_binding_object(&candidate, &pack, &env, &binding).is_none(),
            "runtime must reject U+{:04X} on AXTextField",
            separator as u32
        );
    }
}

#[test]
fn scoped_form_text_requires_a_new_focused_source_and_never_replays_unknown_focus() {
    let goal_draft = form_draft(
        "form-display-name-en",
        "Display name",
        "Profile",
        "AXTextField",
        ExpectedValue::Text {
            value: "Avery Chen".into(),
        },
    );
    let scoped_plan = scoped_plan("form-display-name-en");

    let (mut unfocused, confirmation, mut unfocused_outputs) = create_and_confirm_scoped(
        "Set Display name.",
        form_scope("Display name", "Profile"),
        goal_draft.clone(),
        scoped_plan.clone(),
    );
    confirm_root(&mut unfocused, &confirmation, &mut unfocused_outputs, 110);
    let action_start = feed_fresh_source(
        &mut unfocused,
        &mut unfocused_outputs,
        form_source(
            1,
            120,
            true,
            "form-display-name-en",
            "Display name",
            "Profile",
            "AXTextField",
            UiValue::Text("Existing name".into()),
        ),
        119,
    );
    let (focus_corr, focus_submit) =
        dispatch_next_action(&mut unfocused, &mut unfocused_outputs, action_start, 230, 1);
    let focus_at = finish_submission(
        &mut unfocused,
        &mut unfocused_outputs,
        focus_corr,
        &focus_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        &mut unfocused,
        &mut unfocused_outputs,
        form_source_with_focus(
            2,
            focus_at + 2,
            true,
            "form-display-name-en",
            "Display name",
            "Profile",
            "AXTextField",
            false,
            UiValue::Text("Existing name".into()),
        ),
        focus_at + 1,
    );
    assert_eq!(submit_count(&unfocused_outputs), 1);

    let (mut unknown, confirmation, mut unknown_outputs) = create_and_confirm_scoped(
        "Set Display name.",
        form_scope("Display name", "Profile"),
        goal_draft,
        scoped_plan,
    );
    confirm_root(&mut unknown, &confirmation, &mut unknown_outputs, 110);
    let action_start = feed_fresh_source(
        &mut unknown,
        &mut unknown_outputs,
        form_source(
            1,
            120,
            true,
            "form-display-name-en",
            "Display name",
            "Profile",
            "AXTextField",
            UiValue::Text("Existing name".into()),
        ),
        119,
    );
    let (focus_corr, focus_submit) =
        dispatch_next_action(&mut unknown, &mut unknown_outputs, action_start, 230, 1);
    let at_ms = finish_submission(
        &mut unknown,
        &mut unknown_outputs,
        focus_corr,
        &focus_submit,
        ReceiptStatus::Unknown,
    );
    unknown_outputs.extend(unknown.handle(at_ms + 20, Input::Tick));
    assert_eq!(
        submit_count(&unknown_outputs),
        1,
        "an unknown form-field focus click ends EnsureValue without a click or SetValue replay"
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlertThresholdPlacement {
    InsidePreferencesScroll,
    OutsidePreferencesScroll,
}

#[allow(clippy::too_many_arguments)] // Explicit Source and lifecycle fields in this test fixture.
fn alert_threshold_source(
    sequence: u64,
    observed_ms: u64,
    semantic: &str,
    label: &str,
    form: &str,
    raw_value: &str,
    in_view: bool,
    focused: bool,
    scroll_offset: f64,
    placement: AlertThresholdPlacement,
) -> StructureSnapshot {
    let mut snapshot = source(sequence, observed_ms, true, false, form);
    snapshot.objects.retain(|object| {
        !["notifications-group", "weekly-digest"].contains(&object.instance_ref.as_str())
    });
    localize_alert_threshold_tabs(&mut snapshot, semantic);

    let container_label = if semantic.ends_with("-zh") {
        "偏好设置内容"
    } else {
        "Preferences content"
    };
    let container_instance = "live-preferences-scroll".to_owned();
    let page_generation = if sequence >= 6 { 2 } else { 1 };
    let form_instance = format!("live-advanced-preferences-{page_generation}");
    let field_instance = format!("live-alert-threshold-{page_generation}");

    let mut scroll = object(
        &container_instance,
        Some("window-root"),
        "AXScrollArea",
        rect(0.0, 54.0, 800.0, 546.0),
        Some(container_label),
        None,
        None,
        Vec::new(),
    );
    scroll.scroll = Some(ScrollState {
        horizontal: None,
        vertical: Some(ScrollAxisState {
            value: scroll_offset,
            min: Some(0.0),
            max: Some(1000.0),
        }),
    });
    snapshot.objects.push(scroll);

    let form_parent = match placement {
        AlertThresholdPlacement::InsidePreferencesScroll => container_instance.as_str(),
        AlertThresholdPlacement::OutsidePreferencesScroll => "window-root",
    };
    snapshot.objects.push(object(
        &form_instance,
        Some(form_parent),
        "AXGroup",
        rect(10.0, 80.0, 540.0, 470.0),
        Some(form),
        None,
        None,
        Vec::new(),
    ));
    let mut field = object(
        &field_instance,
        Some(&form_instance),
        "AXTextField",
        rect(24.0, 510.0, 260.0, 36.0),
        Some(label),
        Some(UiValue::Text(raw_value.into())),
        Some(form),
        vec![AxActionName::SetValue],
    );
    field.in_view = in_view;
    field.focused = focused;
    snapshot.objects.push(field);
    snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    snapshot
        .validate()
        .expect("the numeric Source has a complete same-window AX tree");
    snapshot
}

fn localize_alert_threshold_tabs(snapshot: &mut StructureSnapshot, semantic: &str) {
    for (source_ref, live_ref, english, chinese) in [
        ("page-settings", "live-settings-tab", "Settings", "设置"),
        ("page-overview", "live-overview-tab", "Overview", "概览"),
    ] {
        if let Some(page) = snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref.as_str() == source_ref)
        {
            page.instance_ref = ObjectRef::new(live_ref).unwrap();
            page.title = Some(
                if semantic.ends_with("-zh") {
                    chinese
                } else {
                    english
                }
                .into(),
            );
        }
    }
    snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
    snapshot
        .validate()
        .expect("localized page tabs retain unique live identities");
}

fn alert_threshold_away_source(
    sequence: u64,
    observed_ms: u64,
    semantic: &str,
    form: &str,
) -> StructureSnapshot {
    let mut snapshot = source(sequence, observed_ms, false, false, form);
    localize_alert_threshold_tabs(&mut snapshot, semantic);
    snapshot
}

#[allow(clippy::too_many_arguments)] // Explicit Source and lifecycle fields in this test fixture.
fn assert_alert_threshold_projection(
    engine: &Engine,
    semantic: &str,
    label: &str,
    form: &str,
    scroll_target: &str,
    scroll_label: &str,
    raw_value: &str,
    in_view: bool,
    scroll_offset: f64,
    inside_scroll: bool,
) {
    let snapshot = &engine
        .state
        .latest()
        .expect("the latest Source is current")
        .snapshot;
    let field = snapshot
        .objects
        .iter()
        .find(|object| object.role == "AXTextField" && object.title.as_deref() == Some(label))
        .expect("the current Source exposes the exact numeric field");
    let form_group = snapshot
        .objects
        .iter()
        .find(|object| Some(&object.instance_ref) == field.parent.as_ref())
        .expect("the numeric field has a live AXGroup parent");
    let scroll = snapshot
        .objects
        .iter()
        .find(|object| {
            object.role == "AXScrollArea" && object.title.as_deref() == Some(scroll_label)
        })
        .expect("the current Source exposes the registered scroll area");
    let generation = if snapshot.source_sequence >= 6 { 2 } else { 1 };
    assert_eq!(
        field.instance_ref.as_str(),
        format!("live-alert-threshold-{generation}")
    );
    assert_eq!(
        form_group.instance_ref.as_str(),
        format!("live-advanced-preferences-{generation}")
    );
    assert_eq!(scroll.instance_ref.as_str(), "live-preferences-scroll");
    assert_ne!(field.instance_ref.as_str(), semantic);
    assert_ne!(scroll.instance_ref.as_str(), scroll_target);
    assert_eq!(form_group.role, "AXGroup");
    assert_eq!(form_group.title.as_deref(), Some(form));
    assert_eq!(field.value, Some(UiValue::Text(raw_value.into())));
    assert_eq!(field.in_view, in_view);
    let actual_offset = scroll
        .scroll
        .as_ref()
        .and_then(|state| state.vertical.as_ref())
        .expect("the scroll area exposes its vertical offset")
        .value;
    assert_eq!(actual_offset, scroll_offset);
    if inside_scroll {
        assert_eq!(form_group.parent.as_ref(), Some(&scroll.instance_ref));
    } else {
        assert_ne!(form_group.parent.as_ref(), Some(&scroll.instance_ref));
    }
}

fn alert_threshold_plan(target: &str, scroll_target: &str, scroll_label: &str) -> PlanBundle {
    let make_scroll = |id: &str| Subgoal {
        id: id.into(),
        description: "Bring the frozen numeric field into view".into(),
        skill: "ui.scroll_to".into(),
        target: Some(SemanticTargetRef::new(target).unwrap()),
        expects: vec!["visible".into()],
        scroll: Some(ScrollToSpec {
            container: TargetSpec {
                row: None,
                semantic_target: SemanticTargetRef::new(scroll_target).unwrap(),
                label: scroll_label.into(),
                group: None,
                role_hint: Some("AXScrollArea".into()),
            },
            direction: ScrollDirection::Down,
            pixels: 80,
            max_actions: 1,
        }),
        select: None,
    };
    PlanBundle {
        subgoals: vec![
            make_scroll("scroll-alert-threshold-before"),
            Subgoal {
                id: "ensure-alert-threshold".into(),
                description: "Set the alert threshold to the requested number".into(),
                skill: "ui.ensure_value".into(),
                target: Some(SemanticTargetRef::new(target).unwrap()),
                expects: vec!["value".into()],
                scroll: None,
                select: None,
            },
            Subgoal {
                id: "revisit-alert-threshold".into(),
                description: "Revisit the form and verify the numeric value".into(),
                skill: "ui.revisit".into(),
                target: Some(SemanticTargetRef::new(target).unwrap()),
                expects: vec!["goal.required".into()],
                scroll: None,
                select: None,
            },
            make_scroll("scroll-alert-threshold-after-return"),
        ],
        dependencies: vec![
            Dependency {
                before: "scroll-alert-threshold-before".into(),
                after: "ensure-alert-threshold".into(),
            },
            Dependency {
                before: "ensure-alert-threshold".into(),
                after: "revisit-alert-threshold".into(),
            },
            Dependency {
                before: "revisit-alert-threshold".into(),
                after: "scroll-alert-threshold-after-return".into(),
            },
        ],
        required_observations: Vec::new(),
        expected_effects: vec!["goal.required".into()],
        recovery_branches: Vec::new(),
        termination: vec!["goal.required".into()],
    }
}

fn create_alert_threshold_goal(
    semantic: &str,
    label: &str,
    form: &str,
    scroll_target: &str,
    scroll_label: &str,
    desired: i64,
) -> (Engine, ConfirmationRequest, Vec<Output>) {
    create_and_confirm_scoped(
        &format!("Set {label} to {desired}."),
        form_scope(label, form),
        form_draft(
            semantic,
            label,
            form,
            "AXTextField",
            ExpectedValue::Number { value: desired },
        ),
        alert_threshold_plan(semantic, scroll_target, scroll_label),
    )
}

fn prepare_alert_threshold_home(
    semantic: &str,
    label: &str,
    form: &str,
    scroll_target: &str,
    scroll_label: &str,
    desired: i64,
) -> (Engine, Vec<Output>, usize, u64) {
    let (mut engine, confirmation, mut outputs) =
        create_alert_threshold_goal(semantic, label, form, scroll_target, scroll_label, desired);
    if let interaction_contracts::desktop::operator::ConfirmationKind::Goal { goal } =
        &confirmation.kind
    {
        assert_eq!(goal.desired, ExpectedValue::Number { value: desired });
    } else {
        panic!("the numeric fixture receives one Goal confirmation");
    }
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);

    let initial = feed_fresh_source(
        &mut engine,
        &mut outputs,
        alert_threshold_source(
            1,
            120,
            semantic,
            label,
            form,
            "10",
            false,
            false,
            0.0,
            AlertThresholdPlacement::InsidePreferencesScroll,
        ),
        119,
    );
    assert_alert_threshold_projection(
        &engine,
        semantic,
        label,
        form,
        scroll_target,
        scroll_label,
        "10",
        false,
        0.0,
        true,
    );
    let (scroll_corr, scroll_submit) =
        dispatch_next_action(&mut engine, &mut outputs, initial, 230, 1);
    assert_eq!(
        &scroll_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::Scroll { dx: 0, dy: -80 },
        "the first bounded scroll uses the Pack-registered Preferences content area"
    );
    let scrolled_at = finish_submission(
        &mut engine,
        &mut outputs,
        scroll_corr,
        &scroll_submit,
        ReceiptStatus::Completed,
    );

    let focus_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        alert_threshold_source(
            2,
            scrolled_at + 2,
            semantic,
            label,
            form,
            "10",
            true,
            false,
            80.0,
            AlertThresholdPlacement::InsidePreferencesScroll,
        ),
        scrolled_at + 1,
    );
    assert_alert_threshold_projection(
        &engine,
        semantic,
        label,
        form,
        scroll_target,
        scroll_label,
        "10",
        true,
        80.0,
        true,
    );
    let (focus_corr, focus_submit) =
        dispatch_next_action(&mut engine, &mut outputs, focus_start, scrolled_at + 110, 2);
    assert!(matches!(
        &focus_submit.intent.native().unwrap().steps[0].primitive,
        interaction_contracts::desktop::primitive::Primitive::Click { .. }
    ));
    let focused_at = finish_submission(
        &mut engine,
        &mut outputs,
        focus_corr,
        &focus_submit,
        ReceiptStatus::Completed,
    );

    let set_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        alert_threshold_source(
            3,
            focused_at + 2,
            semantic,
            label,
            form,
            "10",
            true,
            true,
            80.0,
            AlertThresholdPlacement::InsidePreferencesScroll,
        ),
        focused_at + 1,
    );
    assert_alert_threshold_projection(
        &engine,
        semantic,
        label,
        form,
        scroll_target,
        scroll_label,
        "10",
        true,
        80.0,
        true,
    );
    let (set_corr, set_submit) =
        dispatch_next_action(&mut engine, &mut outputs, set_start, focused_at + 110, 3);
    assert_eq!(
        &set_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::SetValue {
                value: desired.to_string(),
            },
        },
        "the registered numeric value is written as the exact decimal number"
    );
    let set_at = finish_submission(
        &mut engine,
        &mut outputs,
        set_corr,
        &set_submit,
        ReceiptStatus::Completed,
    );

    let ensure_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        alert_threshold_source(
            4,
            set_at + 2,
            semantic,
            label,
            form,
            &desired.to_string(),
            true,
            true,
            80.0,
            AlertThresholdPlacement::InsidePreferencesScroll,
        ),
        set_at + 1,
    );
    assert_alert_threshold_projection(
        &engine,
        semantic,
        label,
        form,
        scroll_target,
        scroll_label,
        &desired.to_string(),
        true,
        80.0,
        true,
    );
    let (away_corr, away_submit) =
        dispatch_next_action(&mut engine, &mut outputs, ensure_start, set_at + 110, 4);
    assert!(matches!(
        &away_submit.intent.native().unwrap().steps[0].primitive,
        interaction_contracts::desktop::primitive::Primitive::Click { .. }
    ));
    let away_at = finish_submission(
        &mut engine,
        &mut outputs,
        away_corr,
        &away_submit,
        ReceiptStatus::Completed,
    );

    let away_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        alert_threshold_away_source(5, away_at + 2, semantic, form),
        away_at + 1,
    );
    let (home_corr, home_submit) =
        dispatch_next_action(&mut engine, &mut outputs, away_start, away_at + 110, 5);
    assert!(matches!(
        &home_submit.intent.native().unwrap().steps[0].primitive,
        interaction_contracts::desktop::primitive::Primitive::Click { .. }
    ));
    let home_at = finish_submission(
        &mut engine,
        &mut outputs,
        home_corr,
        &home_submit,
        ReceiptStatus::Completed,
    );

    let home_start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        alert_threshold_source(
            6,
            home_at + 2,
            semantic,
            label,
            form,
            &desired.to_string(),
            false,
            false,
            0.0,
            AlertThresholdPlacement::InsidePreferencesScroll,
        ),
        home_at + 1,
    );
    assert_alert_threshold_projection(
        &engine,
        semantic,
        label,
        form,
        scroll_target,
        scroll_label,
        &desired.to_string(),
        false,
        0.0,
        true,
    );
    (engine, outputs, home_start, home_at)
}

#[allow(clippy::too_many_arguments)] // Explicit Source and lifecycle fields in this test fixture.
fn finish_alert_threshold_return_scroll(
    engine: &mut Engine,
    outputs: &mut Vec<Output>,
    home_start: usize,
    home_at: u64,
    semantic: &str,
    label: &str,
    form: &str,
    returned_raw_value: &str,
) {
    let (scroll_corr, scroll_submit) =
        dispatch_next_action(engine, outputs, home_start, home_at + 110, 6);
    assert_eq!(
        &scroll_submit.intent.native().unwrap().steps[0].primitive,
        &interaction_contracts::desktop::primitive::Primitive::Scroll { dx: 0, dy: -80 },
        "Settings return resets the scroll offset and requires the second bounded scroll"
    );
    let scroll_at = finish_submission(
        engine,
        outputs,
        scroll_corr,
        &scroll_submit,
        ReceiptStatus::Completed,
    );
    feed_fresh_source(
        engine,
        outputs,
        alert_threshold_source(
            7,
            scroll_at + 2,
            semantic,
            label,
            form,
            returned_raw_value,
            true,
            false,
            80.0,
            AlertThresholdPlacement::InsidePreferencesScroll,
        ),
        scroll_at + 1,
    );
    assert_alert_threshold_projection(
        engine,
        semantic,
        label,
        form,
        if semantic.ends_with("-zh") {
            "scroll-preferences-zh"
        } else {
            "scroll-preferences"
        },
        if semantic.ends_with("-zh") {
            "偏好设置内容"
        } else {
            "Preferences content"
        },
        returned_raw_value,
        true,
        80.0,
        true,
    );
}

#[test]
fn scoped_offscreen_numeric_field_scrolls_writes_and_revisits_in_english_and_chinese() {
    for (semantic, label, form, scroll_target, scroll_label) in [
        (
            "form-alert-threshold-en",
            "Alert threshold",
            "Advanced preferences",
            "scroll-preferences",
            "Preferences content",
        ),
        (
            "form-alert-threshold-zh",
            "提醒阈值",
            "高级偏好设置",
            "scroll-preferences-zh",
            "偏好设置内容",
        ),
    ] {
        let (mut engine, mut outputs, home_start, home_at) =
            prepare_alert_threshold_home(semantic, label, form, scroll_target, scroll_label, 21);
        finish_alert_threshold_return_scroll(
            &mut engine,
            &mut outputs,
            home_start,
            home_at,
            semantic,
            label,
            form,
            "21",
        );
        assert_eq!(submit_count(&outputs), 6);
        assert_eq!(
            engine.root_state().or_else(|| engine.last_finished()),
            Some(interaction_contracts::desktop::task::RootState::Completed),
            "the numeric Goal completes only from fresh canonical value evidence after the second scroll"
        );
    }
}

#[test]
fn scoped_numeric_scroll_requires_progress_and_keeps_the_target_inside_the_registered_container() {
    for (placement, offset, visible) in [
        (AlertThresholdPlacement::InsidePreferencesScroll, 0.0, false),
        (
            AlertThresholdPlacement::OutsidePreferencesScroll,
            80.0,
            true,
        ),
    ] {
        let (mut engine, confirmation, mut outputs) = create_alert_threshold_goal(
            "form-alert-threshold-en",
            "Alert threshold",
            "Advanced preferences",
            "scroll-preferences",
            "Preferences content",
            21,
        );
        confirm_root(&mut engine, &confirmation, &mut outputs, 110);
        let initial = feed_fresh_source(
            &mut engine,
            &mut outputs,
            alert_threshold_source(
                1,
                120,
                "form-alert-threshold-en",
                "Alert threshold",
                "Advanced preferences",
                "10",
                false,
                false,
                0.0,
                placement,
            ),
            119,
        );
        let (scroll_corr, scroll_submit) =
            dispatch_next_action(&mut engine, &mut outputs, initial, 230, 1);
        let scrolled_at = finish_submission(
            &mut engine,
            &mut outputs,
            scroll_corr,
            &scroll_submit,
            ReceiptStatus::Completed,
        );
        let result_start = feed_fresh_source(
            &mut engine,
            &mut outputs,
            alert_threshold_source(
                2,
                scrolled_at + 2,
                "form-alert-threshold-en",
                "Alert threshold",
                "Advanced preferences",
                "10",
                visible,
                false,
                offset,
                placement,
            ),
            scrolled_at + 1,
        );
        assert_eq!(
            submit_count(&outputs),
            1,
            "no-progress or a matching field outside the exact scroll container cannot authorize field input"
        );
        assert!(
            engine.root_state().or_else(|| engine.last_finished())
                != Some(interaction_contracts::desktop::task::RootState::Completed)
        );
        assert!(!outputs[result_start..].iter().any(|output| matches!(
            output,
            Output::Device { command, .. }
                if matches!(command.as_ref(), DeviceCommand::Submit(submit)
                    if matches!(submit.intent.native().map(|native| &native.steps[0].primitive),
                        Some(interaction_contracts::desktop::primitive::Primitive::AxAction {
                            action: interaction_contracts::desktop::primitive::AxActionKind::SetValue { .. }
                        })))
        )));
    }
}

#[test]
fn scoped_numeric_scroll_unknown_input_is_never_replayed() {
    let (mut engine, confirmation, mut outputs) = create_alert_threshold_goal(
        "form-alert-threshold-en",
        "Alert threshold",
        "Advanced preferences",
        "scroll-preferences",
        "Preferences content",
        21,
    );
    confirm_root(&mut engine, &confirmation, &mut outputs, 110);
    let start = feed_fresh_source(
        &mut engine,
        &mut outputs,
        alert_threshold_source(
            1,
            120,
            "form-alert-threshold-en",
            "Alert threshold",
            "Advanced preferences",
            "10",
            false,
            false,
            0.0,
            AlertThresholdPlacement::InsidePreferencesScroll,
        ),
        119,
    );
    let (scroll_corr, scroll_submit) =
        dispatch_next_action(&mut engine, &mut outputs, start, 230, 1);
    let at_ms = finish_submission(
        &mut engine,
        &mut outputs,
        scroll_corr,
        &scroll_submit,
        ReceiptStatus::Unknown,
    );
    outputs.extend(engine.handle(at_ms + 20, Input::Tick));
    assert_eq!(submit_count(&outputs), 1);
    assert_ne!(
        engine.root_state().or_else(|| engine.last_finished()),
        Some(interaction_contracts::desktop::task::RootState::Completed),
        "unknown scroll outcome cannot be treated as successful visibility proof"
    );
}

#[test]
fn scoped_numeric_revisit_rejects_wrong_or_noncanonical_returned_numbers() {
    for returned in ["20", "021", "21.0", "9007199254740992"] {
        let (mut engine, mut outputs, home_start, home_at) = prepare_alert_threshold_home(
            "form-alert-threshold-en",
            "Alert threshold",
            "Advanced preferences",
            "scroll-preferences",
            "Preferences content",
            21,
        );
        finish_alert_threshold_return_scroll(
            &mut engine,
            &mut outputs,
            home_start,
            home_at,
            "form-alert-threshold-en",
            "Alert threshold",
            "Advanced preferences",
            returned,
        );
        assert_eq!(submit_count(&outputs), 6);
        assert_ne!(
            engine.root_state().or_else(|| engine.last_finished()),
            Some(interaction_contracts::desktop::task::RootState::Completed),
            "wrong, noncanonical, or unsafe AX text cannot satisfy the numeric Root predicate"
        );
    }
}
