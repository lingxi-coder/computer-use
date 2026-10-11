use std::collections::BTreeMap;

use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, ExecutionDomainCapability, SurfaceRef,
};
use interaction_contracts::desktop::domain::{DomainAction, ExecutionDomain};
use interaction_contracts::desktop::ids::{ObjectRef, RequestId};
use interaction_contracts::desktop::primitive::PrimitivePolicy;
use interaction_contracts::desktop::source::{
    AppIdentity, AxActionName, GlobalFocusObservation, KernelProcessIdentity, LayerKind,
    ObjectIdentityContinuity, ObservationFact, ObserverCoverage, Rect,
    SourceBackgroundWindowContext, SourceHealth, SourceKind, SourceStamp, SourceState,
    SourceWindowContext, StructureSnapshot, TargetContext, UiObject, UiValue, WindowInfo,
};
use interaction_engine::engine::{
    Engine, EngineConfig, Input, NativeBackgroundWindowScope, Output,
};
use interaction_engine::environment::pack::AppPack;
use interaction_engine::execution::DeviceCommand;
use interaction_engine::infrastructure::storage::Storage;
use interaction_engine::tasks::TaskConfig;

const APP: &str = "com.example.background-health";
const PID: u32 = 412;
const WINDOW: &str = "background-window";
const FRAME: Rect = Rect {
    x: 20.0,
    y: 30.0,
    width: 420.0,
    height: 280.0,
};

fn domain_capabilities(selected: ExecutionDomain) -> Vec<ExecutionDomainCapability> {
    ExecutionDomain::ALL
        .into_iter()
        .map(|domain| {
            let actions = if domain != selected {
                Vec::new()
            } else {
                match domain {
                    ExecutionDomain::ForegroundNative => vec![
                        DomainAction::NativePointer,
                        DomainAction::NativeKeyboard,
                        DomainAction::NativeScroll,
                        DomainAction::WindowActivation,
                        DomainAction::ForegroundAx,
                    ],
                    ExecutionDomain::BackgroundAx => vec![DomainAction::BackgroundAxPress],
                    ExecutionDomain::Simulated => vec![DomainAction::SimulatedAction],
                    ExecutionDomain::HeadlessBrowser => {
                        vec![DomainAction::HeadlessBrowserAction]
                    }
                    ExecutionDomain::AppIntegration => vec![DomainAction::AppIntegrationAction],
                }
            };
            let available = !actions.is_empty()
                && matches!(
                    domain,
                    ExecutionDomain::ForegroundNative | ExecutionDomain::BackgroundAx
                );
            ExecutionDomainCapability {
                domain,
                state: if available {
                    CapabilityState::Available
                } else {
                    CapabilityState::Unavailable
                },
                actions: if available { actions } else { Vec::new() },
            }
        })
        .collect()
}

fn manifest(domain: ExecutionDomain) -> CapabilityManifest {
    CapabilityManifest {
        identity: interaction_contracts::desktop::device::DeviceIdentity {
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "macos/1/wall".into(),
            dependencies: BTreeMap::from([
                ("authorization".into(), 1),
                ("background_scope".into(), 1),
                ("binding".into(), 1),
                ("geometry".into(), 1),
                ("window".into(), 1),
            ]),
        },
        backend: BackendKind::Macos,
        execution_domain: domain,
        policy: PrimitivePolicy::AtomicOnly,
        human_input: CapabilityState::Available,
        hit_test: CapabilityState::Available,
        input: CapabilityState::Available,
        domains: domain_capabilities(domain),
        max_uninterruptible_ms: 100,
    }
}

fn background_snapshot(sequence: u64, observed_ms: u64) -> StructureSnapshot {
    let app = AppIdentity {
        bundle_id: APP.into(),
        name: "Background Test App".into(),
        pid: PID,
    };
    let other_app = AppIdentity {
        bundle_id: "com.example.other".into(),
        name: "Other App".into(),
        pid: 913,
    };
    let root_ref = ObjectRef::new("root").unwrap();
    let target_ref = ObjectRef::new("toggle").unwrap();
    let root = UiObject {
        instance_ref: root_ref.clone(),
        parent: None,
        window_id: WINDOW.into(),
        role: "AXWindow".into(),
        subrole: Some("AXStandardWindow".into()),
        title: Some("Background Test App".into()),
        description: None,
        value: None,
        enabled: false,
        focused: false,
        frame: FRAME,
        actions: Vec::new(),
        ax_action_names: None,
        in_view: true,
        group: None,
        secure: false,
        scroll: None,
        identity_continuity: ObjectIdentityContinuity::NotificationCovered,
    };
    let target = UiObject {
        instance_ref: target_ref,
        parent: Some(root_ref),
        window_id: WINDOW.into(),
        role: "AXCheckBox".into(),
        subrole: None,
        title: Some("Background toggle".into()),
        description: None,
        value: Some(UiValue::Bool(false)),
        enabled: true,
        focused: false,
        frame: Rect {
            x: 40.0,
            y: 60.0,
            width: 120.0,
            height: 24.0,
        },
        actions: vec![AxActionName::Press],
        ax_action_names: Some(vec!["AXPress".into()]),
        in_view: true,
        group: None,
        secure: false,
        scroll: None,
        identity_continuity: ObjectIdentityContinuity::NotificationCovered,
    };
    let mut source = SourceBackgroundWindowContext {
        window: SourceWindowContext {
            source_session_id: "source-session-1".into(),
            session_epoch: 1,
            process: KernelProcessIdentity {
                pid: PID,
                start_seconds: 1_700_000_000,
                start_micros: 123_456,
            },
            origin_window_id: WINDOW.into(),
            origin_window_frame: FRAME,
            verified_source_sequence: sequence,
            verified_observed_ms: observed_ms,
        },
        hidden: ObservationFact::Known { value: false },
        minimized: ObservationFact::Known { value: false },
        target_context_hash: String::new(),
    };
    source.target_context_hash = source.computed_target_context_hash(&app).unwrap();
    StructureSnapshot {
        snapshot_id: RequestId::new(format!("background-{sequence}")).unwrap(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "macos/1/wall".into(),
        identity_boundary: None,
        frame_ref: None,
        source_sequence: sequence,
        observed_ms,
        app: app.clone(),
        windows: vec![WindowInfo {
            window_id: WINDOW.into(),
            title: Some("Background Test App".into()),
            frame: FRAME,
            z_index: 0,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: APP.into(),
        }],
        global_focus: GlobalFocusObservation {
            app: ObservationFact::Known { value: other_app },
            window_id: ObservationFact::Unavailable,
        },
        target_context: Some(TargetContext::NativeWindow { app, source }),
        complete: true,
        incomplete_reason: None,
        objects: vec![root, target],
        observer_coverage: ObserverCoverage {
            state: SourceState::Available,
            covered_objects: 2,
            read_scoped_objects: 0,
            failed_registrations: 0,
            failures: Vec::new(),
        },
        menu_anchors: Vec::new(),
        menu_context: None,
        focus_context: None,
        window_context: None,
        table_viewports: Vec::new(),
        table_scope_search: None,
        owned_document_evidence: None,
    }
}

fn engine() -> Engine {
    let tasks = TaskConfig::new(vec![AppPack::observe_only(APP)]);
    let mut config = EngineConfig::new(tasks);
    config.native_background_window_scope = Some(NativeBackgroundWindowScope {
        app: APP.into(),
        pid: PID,
        window: WINDOW.into(),
        process_start_seconds: Some(1_700_000_000),
        process_start_micros: Some(123_456),
    });
    Engine::new(config, Storage::open_in_memory().unwrap())
}

fn background_is_usable(engine: &Engine, snapshot: &StructureSnapshot, now_ms: u64) -> bool {
    let TargetContext::NativeWindow { app, source } = snapshot.target_context.as_ref().unwrap()
    else {
        unreachable!()
    };
    engine.state().background_ax_usable(
        (&app.bundle_id, app.pid),
        &source.window.origin_window_id,
        Some((
            source.window.process.start_seconds,
            source.window.process.start_micros,
        )),
        Some(source.target_context_hash.as_str()),
        now_ms,
        2_000,
    )
}

#[test]
fn background_ax_watch_does_not_require_pixels_but_keeps_ax_and_domain_floors() {
    let mut engine = engine();
    let bg_manifest = manifest(ExecutionDomain::BackgroundAx);
    assert!(bg_manifest.validate().is_ok());
    engine.handle(
        900,
        Input::DeviceNotice(interaction_engine::execution::DeviceNotice::Manifest(
            Box::new(bg_manifest),
        )),
    );
    assert!(!engine.state().requires_frames());

    let snapshot = background_snapshot(10, 950);
    assert!(snapshot.validate().is_ok());
    engine.handle(1_000, Input::Structure(Box::new(snapshot.clone())));
    let view = engine.state().observation_view(1_000).unwrap();
    assert!(view.frame.is_none());
    assert!(view.background_window_target.is_some());
    assert!(background_is_usable(&engine, &snapshot, 1_000));

    engine.handle(
        1_100,
        Input::SourceHealth(SourceHealth {
            source: SourceKind::Screenshot,
            state: SourceState::Unavailable,
            reason: Some("background AX-only Source does not capture pixels".into()),
            at_ms: 1_100,
            provenance: Some(SourceStamp {
                session_epoch: 1,
                stream_id: "screen-watch-1".into(),
                source_sequence: 1,
            }),
        }),
    );
    let view = engine.state().observation_view(1_100).unwrap();
    assert!(view.frame.is_none());
    assert!(view.background_window_target.is_some());
    assert!(background_is_usable(&engine, &snapshot, 1_100));
    assert_eq!(
        engine.state().health(SourceKind::Screenshot).unwrap().state,
        SourceState::Unavailable,
        "the unavailable pixel channel remains visible as a health fact"
    );

    let foreground_manifest = manifest(ExecutionDomain::ForegroundNative);
    engine.handle(
        1_200,
        Input::DeviceNotice(interaction_engine::execution::DeviceNotice::Manifest(
            Box::new(foreground_manifest),
        )),
    );
    assert!(engine.state().requires_frames());
    assert!(engine.state().latest().is_none());
    assert!(!background_is_usable(&engine, &snapshot, 1_200));

    engine.handle(
        1_300,
        Input::DeviceNotice(interaction_engine::execution::DeviceNotice::Manifest(
            Box::new(manifest(ExecutionDomain::BackgroundAx)),
        )),
    );
    assert!(!engine.state().requires_frames());
    assert!(engine.state().latest().is_none());
    assert!(!background_is_usable(&engine, &snapshot, 1_300));

    // The old read is still unusable even if Source publishes it again after
    // the domain switch; the new read must cross the domain-change floor.
    engine.handle(1_400, Input::Structure(Box::new(snapshot.clone())));
    assert!(!background_is_usable(&engine, &snapshot, 1_400));
    assert!(
        engine
            .state()
            .observation_view(1_400)
            .unwrap()
            .background_window_target
            .is_none()
    );

    let fresh = background_snapshot(11, 1_301);
    let outputs = engine.handle(1_401, Input::Structure(Box::new(fresh.clone())));
    assert!(background_is_usable(&engine, &fresh, 1_401));
    assert!(
        engine
            .state()
            .observation_view(1_401)
            .unwrap()
            .background_window_target
            .is_some()
    );
    assert!(
        outputs.iter().any(|output| matches!(
            output,
            Output::Device { command, .. }
                if matches!(
                    command.as_ref(),
                    DeviceCommand::UpdateDependency(update)
                        if update.name == "binding" && update.valid == Some(true)
                )
        )),
        "fresh BG target context reopens evidence after the domain floor"
    );

    engine.handle(
        1_410,
        Input::SourceHealth(SourceHealth {
            source: SourceKind::Ax,
            state: SourceState::Degraded,
            reason: Some("AX target metadata read failed".into()),
            at_ms: 1_410,
            provenance: Some(SourceStamp {
                session_epoch: 1,
                stream_id: "ax-watch-1".into(),
                source_sequence: 12,
            }),
        }),
    );
    assert!(!background_is_usable(&engine, &fresh, 1_410));
    assert!(
        engine
            .state()
            .observation_view(1_410)
            .unwrap()
            .background_window_target
            .is_none()
    );
}
