//! Browser page reads enter the ordinary Engine event path as read-only
//! observations. They do not create a native snapshot, Root, or device input.

use interaction_contracts::desktop::browser::{
    BrowserObservedBool, BrowserPageNode, BrowserPageSnapshot, BrowserReadScope,
    BrowserSourceControl, BrowserTargetContext, browser_target_context_hash,
};
use interaction_contracts::desktop::device::{BackendKind, CapabilityState};
use interaction_contracts::desktop::domain::{DomainAction, ExecutionDomain};
use interaction_contracts::desktop::operator::{OperatorCommand, UiProjection};
use interaction_engine::engine::{BrowserPageAdmissionError, Engine, EngineConfig, Input, Output};
use interaction_engine::environment::pack::AppPack;
use interaction_engine::execution::DeviceNotice;
use interaction_engine::infrastructure::storage::Storage;
use interaction_engine::tasks::TaskConfig;

const CLOCK: &str = "headless-fixture-clock";

fn context(epoch: u64) -> BrowserTargetContext {
    BrowserTargetContext {
        browser_session_id: "browser-session-1".into(),
        browser_context_id: "browser-context-1".into(),
        page_target_id: "page-target-1".into(),
        cdp_session_id: "cdp-session-1".into(),
        frame_id: "frame-1".into(),
        document_loader_id: "document-1".into(),
        fixture_file_url: "file:///tmp/fixtures/headless/readonly-page.html".into(),
        session_epoch: epoch,
    }
}

fn scope(generation: u64, issued_ms: u64) -> BrowserReadScope {
    BrowserReadScope {
        scope_id: format!("00000000-0000-4000-8000-{generation:012x}"),
        generation,
        context: context(1),
        clock_domain: CLOCK.into(),
        issued_ms,
    }
}

fn page(
    sequence: u64,
    snapshot_id: &str,
    observed_ms: u64,
    read_finished_ms: u64,
    page_context: BrowserTargetContext,
) -> BrowserPageSnapshot {
    BrowserPageSnapshot {
        snapshot_id: snapshot_id.into(),
        source_sequence: sequence,
        observed_ms,
        read_finished_ms,
        clock_domain: CLOCK.into(),
        target_context_hash: browser_target_context_hash(&page_context).unwrap(),
        context: page_context,
        root_ax_node_id: "root".into(),
        nodes: vec![
            BrowserPageNode {
                ax_node_id: "root".into(),
                backend_dom_node_id: Some(1),
                parent_ax_node_id: None,
                child_ax_node_ids: vec!["checkbox".into()],
                ignored: false,
                role: Some("rootWebArea".into()),
                name: Some("Read-only fixture".into()),
                disabled: BrowserObservedBool::Unknown,
                checked: BrowserObservedBool::Unknown,
            },
            BrowserPageNode {
                ax_node_id: "checkbox".into(),
                backend_dom_node_id: Some(2),
                parent_ax_node_id: Some("root".into()),
                child_ax_node_ids: vec![],
                ignored: false,
                role: Some("checkbox".into()),
                name: Some("Background option".into()),
                disabled: BrowserObservedBool::False,
                checked: BrowserObservedBool::False,
            },
        ],
    }
}

fn engine() -> Engine {
    let config = EngineConfig::new(TaskConfig::new(vec![AppPack::settings_fixture()]));
    let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());
    let manifest = interaction_contracts::desktop::samples::manifest();
    assert!(manifest.validate().is_ok());
    assert!(!manifest.supports(
        ExecutionDomain::HeadlessBrowser,
        DomainAction::HeadlessBrowserAction,
    ));
    engine.handle(
        0,
        Input::DeviceNotice(DeviceNotice::Manifest(Box::new(manifest))),
    );
    engine
}

fn browser_engine() -> Engine {
    let mut config = EngineConfig::new(TaskConfig::new(vec![AppPack::browser_fixture()]));
    config.browser_page_profile =
        Some(interaction_contracts::desktop::browser::BrowserPageProfile::FixtureBackgroundOption);
    Engine::new(config, Storage::open_in_memory().unwrap())
}

fn headless_manifest(
    available: bool,
) -> interaction_contracts::desktop::device::CapabilityManifest {
    use interaction_contracts::desktop::device::ExecutionDomainCapability;

    let mut manifest = interaction_contracts::desktop::samples::manifest();
    manifest.backend = BackendKind::HeadlessBrowser;
    manifest.execution_domain = ExecutionDomain::HeadlessBrowser;
    manifest.human_input = CapabilityState::Unavailable;
    manifest.hit_test = CapabilityState::Unavailable;
    manifest.input = if available {
        CapabilityState::Available
    } else {
        CapabilityState::Unavailable
    };
    manifest.domains = ExecutionDomainCapability::all_unavailable();
    if available {
        let browser = manifest
            .domains
            .iter_mut()
            .find(|capability| capability.domain == ExecutionDomain::HeadlessBrowser)
            .unwrap();
        browser.state = CapabilityState::Available;
        browser.actions = vec![DomainAction::HeadlessBrowserAction];
    }
    manifest
}

fn projection(outputs: &[Output]) -> &UiProjection {
    outputs
        .iter()
        .rev()
        .find_map(|output| match output {
            Output::Ui(view) => Some(view.as_ref()),
            _ => None,
        })
        .expect("Engine publishes its current operator projection")
}

fn install(engine: &mut Engine, page_scope: BrowserReadScope) {
    engine
        .control_browser_source(
            page_scope.issued_ms,
            BrowserSourceControl::Install {
                scope: Box::new(page_scope),
            },
        )
        .expect("trusted host installs the browser read scope");
}

#[test]
fn browser_page_input_projects_readonly_checkbox_without_native_state_or_input_authority() {
    let mut engine = engine();
    let page_scope = scope(1, 100);
    install(&mut engine, page_scope.clone());

    let outputs = engine.handle(
        101,
        Input::BrowserPage(Box::new(page(
            1,
            "page-read-1",
            100,
            100,
            page_scope.context.clone(),
        ))),
    );

    let view = projection(&outputs);
    let browser = view
        .browser_observation
        .as_ref()
        .expect("the independently admitted page read is projected");
    assert_eq!(browser.scope, page_scope);
    assert_eq!(
        browser.target_context_hash,
        browser_target_context_hash(&page_scope.context).unwrap()
    );
    assert_eq!(browser.snapshot_id, "page-read-1");
    assert_eq!(browser.node_count, 2);
    assert!(browser.readonly_binding_ref.is_some());
    assert_eq!(browser.readonly_checked, Some(false));

    assert!(
        view.root.is_none(),
        "browser evidence does not start a Root"
    );
    assert!(
        view.observation.is_none(),
        "no native observation was supplied"
    );
    assert!(engine.state().latest().is_none());
    assert!(view.capabilities.as_ref().is_some_and(|manifest| {
        manifest
            .domains
            .iter()
            .find(|domain| domain.domain == ExecutionDomain::HeadlessBrowser)
            .is_some_and(|domain| domain.state == CapabilityState::Unavailable)
    }));
    assert!(
        outputs
            .iter()
            .all(|output| !matches!(output, Output::Device { .. }))
    );
}

#[test]
fn browser_input_cannot_self_grant_and_rejected_current_reads_clear_only_their_evidence() {
    let mut engine = engine();
    let page_scope = scope(1, 100);

    let unscoped = engine.handle(
        90,
        Input::BrowserPage(Box::new(page(
            1,
            "unscoped-read",
            90,
            90,
            page_scope.context.clone(),
        ))),
    );
    assert!(projection(&unscoped).browser_observation.is_none());

    install(&mut engine, page_scope.clone());
    let before_scope = engine.handle(
        100,
        Input::BrowserPage(Box::new(page(
            2,
            "read-before-issuance",
            99,
            99,
            page_scope.context.clone(),
        ))),
    );
    assert!(projection(&before_scope).browser_observation.is_none());

    let first = engine.handle(
        101,
        Input::BrowserPage(Box::new(page(
            3,
            "accepted-read",
            100,
            100,
            page_scope.context.clone(),
        ))),
    );
    let accepted = projection(&first)
        .browser_observation
        .as_ref()
        .expect("a fresh read in the installed scope is visible")
        .clone();

    let mut foreign_context = page_scope.context.clone();
    foreign_context.page_target_id = "foreign-target".into();
    let foreign = engine.handle(
        102,
        Input::BrowserPage(Box::new(page(4, "foreign-read", 102, 102, foreign_context))),
    );
    let retained = projection(&foreign)
        .browser_observation
        .as_ref()
        .expect("foreign page evidence cannot replace the current scoped read");
    assert_eq!(retained.snapshot_id, accepted.snapshot_id);
    assert_eq!(retained.readonly_binding_ref, accepted.readonly_binding_ref);

    let future_end = engine.handle(
        103,
        Input::BrowserPage(Box::new(page(
            4,
            "future-read-end",
            103,
            104,
            page_scope.context.clone(),
        ))),
    );
    assert!(projection(&future_end).browser_observation.is_none());

    let recovered = engine.handle(
        104,
        Input::BrowserPage(Box::new(page(
            5,
            "fresh-after-rejection",
            103,
            103,
            page_scope.context.clone(),
        ))),
    );
    assert!(projection(&recovered).browser_observation.is_some());

    let stale = engine.handle(
        2_103,
        Input::BrowserPage(Box::new(page(
            6,
            "expired-read",
            103,
            103,
            page_scope.context.clone(),
        ))),
    );
    assert!(projection(&stale).browser_observation.is_none());
}

#[test]
fn lowering_mode_preserves_browser_scope_but_requires_a_strictly_newer_read() {
    let mut engine = engine();
    let page_scope = scope(1, 100);
    install(&mut engine, page_scope.clone());
    let before_mode = engine.handle(
        101,
        Input::BrowserPage(Box::new(page(
            1,
            "before-mode",
            100,
            100,
            page_scope.context.clone(),
        ))),
    );
    let original_binding = projection(&before_mode)
        .browser_observation
        .as_ref()
        .and_then(|view| view.readonly_binding_ref.clone())
        .expect("the initial read has a live read-only binding");

    engine.handle(
        102,
        Input::Operator(OperatorCommand::SetMode {
            mode: interaction_contracts::desktop::operator::RunMode::Execute,
        }),
    );
    let observed = engine.handle(
        103,
        Input::Operator(OperatorCommand::SetMode {
            mode: interaction_contracts::desktop::operator::RunMode::Observe,
        }),
    );
    let retained = projection(&observed)
        .browser_observation
        .as_ref()
        .expect("Observe keeps the trusted read-only page scope");
    assert_eq!(retained.scope, page_scope);
    assert_eq!(retained.snapshot_id, "before-mode");
    assert!(retained.readonly_binding_ref.is_none());
    assert!(retained.readonly_checked.is_none());
    assert!(
        engine
            .state()
            .current_browser_binding(&original_binding)
            .is_none()
    );

    // Equality with the mode floor is not a post-mode read.
    let at_floor_rejected = engine.admit_browser_page(
        103,
        page(2, "at-mode-floor", 103, 103, page_scope.context.clone()),
    );
    assert_eq!(
        at_floor_rejected,
        Err(BrowserPageAdmissionError::ReadBeforeFloor)
    );
    let at_floor = engine.handle(103, Input::Tick);
    assert!(
        at_floor
            .iter()
            .all(|output| !matches!(output, Output::Ui(_)))
    );
    assert_eq!(
        engine
            .state()
            .browser_latest()
            .map(|latest| latest.snapshot.snapshot_id.as_str()),
        Some("before-mode")
    );
    assert!(
        engine
            .state()
            .current_browser_binding(&original_binding)
            .is_none()
    );

    let executing = engine.handle(
        104,
        Input::Operator(OperatorCommand::SetMode {
            mode: interaction_contracts::desktop::operator::RunMode::Execute,
        }),
    );
    let retained = projection(&executing).browser_observation.as_ref().unwrap();
    assert_eq!(retained.snapshot_id, "before-mode");
    assert!(retained.readonly_binding_ref.is_none());

    let late_rejected = engine.admit_browser_page(
        104,
        page(
            3,
            "in-flight-before-mode",
            102,
            104,
            page_scope.context.clone(),
        ),
    );
    assert_eq!(
        late_rejected,
        Err(BrowserPageAdmissionError::ReadBeforeFloor)
    );
    let late = engine.handle(104, Input::Tick);
    assert!(late.iter().all(|output| !matches!(output, Output::Ui(_))));
    assert_eq!(
        engine
            .state()
            .browser_latest()
            .map(|latest| latest.snapshot.snapshot_id.as_str()),
        Some("before-mode")
    );
    assert!(
        engine
            .state()
            .current_browser_binding(&original_binding)
            .is_none()
    );
    assert!(
        late.iter()
            .all(|output| !matches!(output, Output::Device { .. }))
    );

    let fresh_admitted = engine.admit_browser_page(
        106,
        page(4, "after-mode", 105, 105, page_scope.context.clone()),
    );
    assert_eq!(fresh_admitted, Ok(()));
    let fresh = engine.handle(106, Input::Tick);
    let restored = projection(&fresh).browser_observation.as_ref().unwrap();
    assert_eq!(restored.scope, page_scope);
    assert_eq!(restored.snapshot_id, "after-mode");
    let restored_binding = restored
        .readonly_binding_ref
        .as_ref()
        .expect("a fresh post-mode read restores its binding");
    assert_ne!(restored_binding, &original_binding);
    assert!(
        engine
            .state()
            .current_browser_binding(restored_binding)
            .is_some()
    );
    assert_eq!(restored.readonly_checked, Some(false));

    let restored_binding = restored_binding.clone();
    let late_old_rejected = engine.admit_browser_page(
        107,
        page(
            5,
            "late-old-after-refresh",
            102,
            107,
            page_scope.context.clone(),
        ),
    );
    assert_eq!(
        late_old_rejected,
        Err(BrowserPageAdmissionError::ReadBeforeFloor)
    );
    let late_old = engine.handle(107, Input::Tick);
    assert!(
        late_old
            .iter()
            .all(|output| !matches!(output, Output::Ui(_)))
    );
    assert_eq!(
        engine
            .state()
            .browser_latest()
            .map(|latest| latest.snapshot.snapshot_id.as_str()),
        Some("after-mode")
    );
    assert!(
        engine
            .state()
            .current_browser_binding(&restored_binding)
            .is_some()
    );
}

#[test]
fn stop_and_recovery_retire_scope_so_late_pages_cannot_restore_browser_ui() {
    let mut engine = engine();
    let old_scope = scope(1, 100);
    install(&mut engine, old_scope.clone());
    let admitted = engine.handle(
        101,
        Input::BrowserPage(Box::new(page(
            1,
            "before-stop",
            100,
            100,
            old_scope.context.clone(),
        ))),
    );
    assert!(projection(&admitted).browser_observation.is_some());

    let stopped = engine.handle(
        102,
        Input::Operator(OperatorCommand::StopTask { root_task_id: None }),
    );
    assert!(projection(&stopped).browser_observation.is_none());

    let late = engine.handle(
        103,
        Input::BrowserPage(Box::new(page(
            2,
            "late-after-stop",
            103,
            103,
            old_scope.context.clone(),
        ))),
    );
    assert!(projection(&late).browser_observation.is_none());

    engine.recover(104);
    assert!(
        engine
            .control_browser_source(
                105,
                BrowserSourceControl::Install {
                    scope: Box::new(old_scope.clone()),
                },
            )
            .is_err(),
        "recovery cannot reinstall a retired scope generation"
    );

    let new_scope = scope(2, 106);
    install(&mut engine, new_scope.clone());
    let fresh = engine.handle(
        107,
        Input::BrowserPage(Box::new(page(
            1,
            "new-scope-read",
            106,
            106,
            new_scope.context.clone(),
        ))),
    );
    assert_eq!(
        projection(&fresh)
            .browser_observation
            .as_ref()
            .map(|view| view.snapshot_id.as_str()),
        Some("new-scope-read"),
        "the browser projection resumes only under a fresh trusted scope"
    );
}

#[test]
fn browser_reference_availability_requires_host_profile_and_live_private_device_capability() {
    let mut no_profile = engine();
    no_profile.handle(
        1,
        Input::DeviceNotice(DeviceNotice::Manifest(Box::new(headless_manifest(true)))),
    );
    assert!(!no_profile.browser_reference_available());

    let mut unavailable = browser_engine();
    let unavailable_manifest = headless_manifest(false);
    assert!(unavailable_manifest.validate().is_ok());
    unavailable.handle(
        1,
        Input::DeviceNotice(DeviceNotice::Manifest(Box::new(unavailable_manifest))),
    );
    assert!(!unavailable.browser_reference_available());

    let mut available = browser_engine();
    let available_manifest = headless_manifest(true);
    assert!(available_manifest.validate().is_ok());
    available.handle(
        1,
        Input::DeviceNotice(DeviceNotice::Manifest(Box::new(available_manifest))),
    );
    assert!(available.browser_reference_available());

    available.handle(
        2,
        Input::DeviceLost {
            reason: "headless browser device disconnected".into(),
        },
    );
    assert!(
        !available.browser_reference_available(),
        "a disconnected private Device cannot keep the reference planner enabled"
    );
}
