use std::collections::BTreeSet;

use interaction_contracts::desktop::device::RootGrant;
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{
    AttemptId, ObjectRef, RequestId, RootTaskId, SemanticTargetRef,
};
use interaction_contracts::desktop::operator::RunMode;
use interaction_contracts::desktop::source::{
    AppIdentity, AxActionName, AxEnabledObservation, GlobalFocusObservation, LayerKind,
    ObjectIdentityContinuity, ObservationFact, ObserverCoverage, Rect, SourceIdentityBoundary,
    StructureSnapshot, TableViewport, TableViewportCell, TableViewportColumn, TableViewportRow,
    UiObject, UiValue, WindowInfo,
};
use interaction_contracts::desktop::task::{
    ConfirmationScope, ExpectedValue, GoalSpec, Predicate, RootBudgetSpec, RowColumnConditionSpec,
    RowContainerRole, RowContainerScope, RowTableScope, RowTargetSpec, TargetSpec, TaskScope,
};

use crate::environment::pack::AppPack;
use crate::infrastructure::storage::Storage;
use crate::tasks::TaskConfig;
use crate::verification::Verification;

use super::{Engine, EngineConfig, RootRun};

const APP: &str = "com.lingxi.computer-user.mvp-fixture";
const SEMANTIC: &str = "project-row";
const WINDOW_ID: &str = "window-1";
const IDENTITY_NAMESPACE: &str = "11111111-1111-4111-8111-111111111111";

#[derive(Clone, Copy)]
enum RowRead {
    Matching,
    WrongOwner,
    Ambiguous,
    PartialViewport,
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn reference(value: String) -> ObjectRef {
    ObjectRef::new(value).expect("fixture references are valid")
}

fn object(
    instance_ref: &ObjectRef,
    parent: Option<&ObjectRef>,
    role: &str,
    frame: Rect,
    title: Option<&str>,
    description: Option<&str>,
    value: Option<UiValue>,
) -> UiObject {
    UiObject {
        instance_ref: instance_ref.clone(),
        parent: parent.cloned(),
        window_id: WINDOW_ID.into(),
        role: role.into(),
        subrole: None,
        title: title.map(str::to_owned),
        description: description.map(str::to_owned),
        value,
        enabled: true,
        focused: false,
        frame,
        actions: Vec::new(),
        ax_action_names: None,
        in_view: true,
        group: None,
        secure: false,
        scroll: None,
        identity_continuity: ObjectIdentityContinuity::ReadScoped,
    }
}

fn row_target() -> TargetSpec {
    TargetSpec {
        semantic_target: SemanticTargetRef::new(SEMANTIC).unwrap(),
        label: "Monthly report".into(),
        group: None,
        role_hint: Some("AXRow".into()),
        row: Some(RowTargetSpec {
            table_scope: RowTableScope {
                scroll_path: vec![
                    RowContainerScope {
                        role: RowContainerRole::AxScrollArea,
                        label: "Preferences content".into(),
                    },
                    RowContainerScope {
                        role: RowContainerRole::AxScrollArea,
                        label: "Projects".into(),
                    },
                ],
                column_headers: vec!["Name".into(), "Owner".into(), "Status".into()],
            },
            conditions: vec![
                RowColumnConditionSpec {
                    column_name: "Name".into(),
                    value: ExpectedValue::Text {
                        value: "Monthly report".into(),
                    },
                },
                RowColumnConditionSpec {
                    column_name: "Owner".into(),
                    value: ExpectedValue::Text {
                        value: "Acme".into(),
                    },
                },
            ],
        }),
    }
}

fn row_goal() -> GoalSpec {
    let target = row_target();
    let semantic = target.semantic_target.clone();
    GoalSpec {
        root_task_id: RootTaskId::new("row-visits-root").unwrap(),
        task_revision: 1,
        execution_domain: ExecutionDomain::Simulated,
        browser_page_profile: None,
        user_text: "Select the Monthly report row".into(),
        scope: TaskScope {
            app: APP.into(),
            account: None,
        },
        allowed_scope: None,
        scoped_revisit: None,
        scoped_commit: None,
        target,
        desired: ExpectedValue::Bool { value: true },
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::Navigation,
        required: Predicate::And {
            of: vec![
                Predicate::ContextMatches { app: APP.into() },
                Predicate::RowSelected {
                    target: semantic.clone(),
                    observed_after_action: true,
                },
                Predicate::Revisited { target: semantic },
            ],
        },
        budget: RootBudgetSpec {
            deadline_ms: 60_000,
            max_attempts: 4,
            max_observations: 12,
            max_recoveries: 1,
            max_replans: 1,
        },
    }
}

fn engine_with_row_goal() -> Engine {
    let pack = AppPack::registered_native(APP).expect("MVP fixture pack is registered");
    let mut config = EngineConfig::new(TaskConfig::new(vec![pack.clone()]));
    config.initial_mode = RunMode::Execute;
    let mut engine = Engine::new(config, Storage::open_in_memory().unwrap());
    engine.state.set_session(1);
    engine.run = Some(RootRun {
        window_activated: true,
        keyboard_result_context: None,
        goal: row_goal(),
        pack,
        verification: Verification::new(0),
        grant: RootGrant {
            root_id: "row-visits-root".into(),
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
        reconcile: Vec::<(AttemptId, String)>::new(),
        reconciling: None,
        reconciliation_retries: 0,
        reconciled_input: BTreeSet::new(),
        effect_verification_only: false,
        revalidate_floor: None,
        permits: Vec::new(),
        stop_wait_since: None,
        paused_session: None,
        last_attempt: None,
    });
    engine
}

fn structure(
    sequence: u64,
    observed_ms: u64,
    on_settings: bool,
    rows: Option<RowRead>,
) -> StructureSnapshot {
    let window_ref = reference(format!("window-{sequence}"));
    let window_frame = rect(0.0, 0.0, 800.0, 600.0);
    let mut objects = vec![object(
        &window_ref,
        None,
        "AXWindow",
        window_frame,
        Some("Interaction Fixture"),
        None,
        None,
    )];

    for (index, (page, selected)) in [("Settings", on_settings), ("Overview", !on_settings)]
        .into_iter()
        .enumerate()
    {
        let nav_ref = reference(format!("navigation-{sequence}-{index}"));
        let mut nav = object(
            &nav_ref,
            Some(&window_ref),
            "AXRadioButton",
            rect(20.0 + index as f64 * 120.0, 20.0, 110.0, 28.0),
            Some(page),
            None,
            Some(UiValue::Bool(selected)),
        );
        nav.actions = vec![AxActionName::Press];
        objects.push(nav);
    }

    let mut table_viewports = Vec::new();
    if let Some(row_read) = rows {
        let outer_scroll_ref = reference(format!("outer-scroll-{sequence}"));
        let projects_scroll_ref = reference(format!("projects-scroll-{sequence}"));
        let table_ref = reference(format!("table-{sequence}"));
        let outer_scroll_frame = rect(10.0, 40.0, 380.0, 250.0);
        let projects_scroll_frame = rect(20.0, 60.0, 360.0, 190.0);
        let table_frame = rect(20.0, 80.0, 360.0, 160.0);
        objects.push(object(
            &outer_scroll_ref,
            Some(&window_ref),
            "AXScrollArea",
            outer_scroll_frame,
            None,
            Some("Preferences content"),
            None,
        ));
        objects.push(object(
            &projects_scroll_ref,
            Some(&outer_scroll_ref),
            "AXScrollArea",
            projects_scroll_frame,
            None,
            Some("Projects"),
            None,
        ));
        objects.push(object(
            &table_ref,
            Some(&projects_scroll_ref),
            "AXTable",
            table_frame,
            None,
            None,
            None,
        ));

        let headers = ["Name", "Owner", "Status"];
        let columns: Vec<_> = headers
            .iter()
            .enumerate()
            .map(|(index, header)| {
                let column_ref = reference(format!("column-{sequence}-{index}"));
                let header_ref = reference(format!("header-{sequence}-{index}"));
                let column_frame = rect(20.0 + index as f64 * 120.0, 80.0, 120.0, 160.0);
                let header_frame = rect(column_frame.x, 80.0, 120.0, 20.0);
                objects.push(object(
                    &column_ref,
                    Some(&table_ref),
                    "AXColumn",
                    column_frame,
                    None,
                    Some(header),
                    None,
                ));
                let mut header_object = object(
                    &header_ref,
                    Some(&column_ref),
                    "AXButton",
                    header_frame,
                    None,
                    Some(header),
                    None,
                );
                header_object.subrole = Some("AXSortButton".into());
                header_object.actions = vec![AxActionName::Press];
                objects.push(header_object);
                TableViewportColumn {
                    instance_ref: Some(column_ref),
                    parent_instance_ref: table_ref.clone(),
                    frame: column_frame,
                    header_instance_ref: Some(header_ref),
                    header: Some((*header).into()),
                    ax_index: Some(index as u32),
                }
            })
            .collect();

        let row_facts: Vec<(&str, &str, &str)> = match row_read {
            RowRead::Matching | RowRead::PartialViewport => {
                vec![("Monthly report", "Acme", "Open")]
            }
            RowRead::WrongOwner => vec![("Monthly report", "Other", "Open")],
            RowRead::Ambiguous => vec![
                ("Monthly report", "Acme", "Open"),
                ("Monthly report", "Acme", "Open"),
            ],
        };
        let mut visible_rows = Vec::new();
        for (row_ordinal, values) in row_facts.iter().enumerate() {
            let row_ref = reference(format!("row-{sequence}-{row_ordinal}"));
            let row_frame = rect(20.0, 105.0 + row_ordinal as f64 * 30.0, 360.0, 24.0);
            objects.push(object(
                &row_ref,
                Some(&table_ref),
                "AXRow",
                row_frame,
                None,
                None,
                None,
            ));
            let mut cells = Vec::new();
            let row_values = [values.0, values.1, values.2];
            for (column_index, (header, value)) in headers.iter().zip(row_values.iter()).enumerate()
            {
                let cell_ref = reference(format!("cell-{sequence}-{row_ordinal}-{column_index}"));
                let column = &columns[column_index];
                let column_ref = column.instance_ref.as_ref().unwrap();
                let header_ref = column.header_instance_ref.as_ref().unwrap();
                let cell_frame = rect(
                    row_frame.x + column_index as f64 * 120.0,
                    row_frame.y,
                    120.0,
                    row_frame.height,
                );
                let cell_value = UiValue::Text((*value).into());
                objects.push(object(
                    &cell_ref,
                    Some(&row_ref),
                    "AXCell",
                    cell_frame,
                    None,
                    None,
                    Some(cell_value.clone()),
                ));
                cells.push(TableViewportCell {
                    instance_ref: Some(cell_ref),
                    parent_instance_ref: Some(row_ref.clone()),
                    frame: cell_frame,
                    row_ax_index: Some(42 + row_ordinal as u32),
                    column_ax_index: Some(column_index as u32),
                    column_instance_ref: Some(column_ref.clone()),
                    header_instance_ref: Some(header_ref.clone()),
                    column_header: Some((*header).into()),
                    value: Some(cell_value),
                });
            }
            visible_rows.push(TableViewportRow {
                instance_ref: Some(row_ref),
                parent_instance_ref: table_ref.clone(),
                frame: row_frame,
                ax_enabled: AxEnabledObservation::Enabled,
                accessibility_identifier: None,
                ax_index: Some(42 + row_ordinal as u32),
                selected: Some(true),
                ax_selected_settable: Some(true),
                cells_complete: true,
                cells,
            });
        }
        table_viewports.push(TableViewport {
            table_instance_ref: table_ref.clone(),
            scroll_container_instance_ref: projects_scroll_ref,
            process_id: 42,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame,
            viewport_frame: table_frame,
            visible_rows_complete: !matches!(row_read, RowRead::PartialViewport),
            visible_rows,
            columns_complete: true,
            columns,
        });
    }

    let observer_coverage = ObserverCoverage::for_projected_objects(&objects);
    StructureSnapshot {
        snapshot_id: RequestId::new(format!("row-visits-read-{sequence}")).unwrap(),
        session_epoch: 1,
        surface: interaction_contracts::desktop::device::SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "test/1/monotonic".into(),
        identity_boundary: Some(SourceIdentityBoundary {
            namespace: IDENTITY_NAMESPACE.into(),
            generation: 1,
        }),
        frame_ref: None,
        source_sequence: sequence,
        observed_ms,
        app: AppIdentity {
            bundle_id: APP.into(),
            name: "Interaction Fixture".into(),
            pid: 42,
        },
        global_focus: GlobalFocusObservation {
            app: ObservationFact::Known {
                value: AppIdentity {
                    bundle_id: APP.into(),
                    name: "Interaction Fixture".into(),
                    pid: 42,
                },
            },
            window_id: ObservationFact::Known {
                value: WINDOW_ID.into(),
            },
        },
        target_context: None,
        windows: vec![WindowInfo {
            window_id: WINDOW_ID.into(),
            title: Some("Interaction Fixture".into()),
            frame: window_frame,
            z_index: 0,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: APP.into(),
        }],
        complete: true,
        incomplete_reason: None,
        objects,
        observer_coverage,
        menu_anchors: Vec::new(),
        menu_context: None,
        focus_context: None,
        window_context: None,
        table_scope_search: None,
        table_viewports,
        owned_document_evidence: None,
    }
}

fn admit(engine: &mut Engine, snapshot: StructureSnapshot, now_ms: u64) {
    engine.now_ms = now_ms;
    engine
        .on_structure(snapshot)
        .expect("fixture Source read is admitted");
}

fn admit_and_bind(engine: &mut Engine, snapshot: StructureSnapshot, now_ms: u64) {
    admit(engine, snapshot, now_ms);
    engine.maintain_goal_binding();
}

#[test]
fn row_revisit_requires_a_fresh_complete_rebind_after_return() {
    let mut engine = engine_with_row_goal();
    admit_and_bind(
        &mut engine,
        structure(1, 100, true, Some(RowRead::Matching)),
        100,
    );

    let target = SemanticTargetRef::new(SEMANTIC).unwrap();
    let first = engine
        .bindings
        .current_row(&target)
        .expect("the initial known-page row resolves from the frozen public selector");
    assert_eq!(first.source.selected, Some(true));
    assert!(
        first.hit_point.is_none(),
        "selected rows do not fabricate input proof"
    );
    assert!(
        first.continuity.is_none(),
        "read-scoped row facts make no cross-read identity claim"
    );
    let initial_row_ref = first.source.row_instance_ref.clone();
    {
        let visits = &engine.run.as_ref().unwrap().verification.visits;
        assert_eq!(visits.target_page(), Some("Settings"));
        assert!(!visits.left());
        assert!(!visits.returned());
        assert!(!visits.satisfied());
    }

    {
        engine.run.as_mut().unwrap().verification.visits.arm(110);
    }
    admit_and_bind(&mut engine, structure(2, 120, false, None), 120);
    {
        let visits = &engine.run.as_ref().unwrap().verification.visits;
        assert!(visits.left());
        assert!(!visits.returned());
        assert!(!visits.satisfied());
        assert_eq!(visits.target_page(), Some("Settings"));
    }
    assert!(engine.bindings.current_row(&target).is_none());

    admit(
        &mut engine,
        structure(3, 130, true, Some(RowRead::Matching)),
        130,
    );
    {
        let visits = &engine.run.as_ref().unwrap().verification.visits;
        assert!(visits.left());
        assert!(visits.returned());
        assert!(
            !visits.satisfied(),
            "returning alone is insufficient before row rebinding"
        );
    }
    engine.maintain_goal_binding();
    let rebound = engine
        .bindings
        .current_row(&target)
        .expect("a complete unique row may bind in the fresh returned-page read");
    assert_eq!(rebound.snapshot_id.as_str(), "row-visits-read-3");
    assert_ne!(rebound.source.row_instance_ref, initial_row_ref);
    assert_eq!(rebound.source.selected, Some(true));
    assert!(rebound.continuity.is_none());
    assert!(engine.run.as_ref().unwrap().verification.visits.satisfied());
}

#[test]
fn wrong_ambiguous_or_partial_return_rows_do_not_rebind_page_visits() {
    for (case, returned_rows) in [
        ("wrong values", RowRead::WrongOwner),
        ("ambiguous selector", RowRead::Ambiguous),
        ("partial viewport", RowRead::PartialViewport),
    ] {
        let mut engine = engine_with_row_goal();
        admit_and_bind(
            &mut engine,
            structure(1, 100, true, Some(RowRead::Matching)),
            100,
        );
        engine.run.as_mut().unwrap().verification.visits.arm(110);
        admit_and_bind(&mut engine, structure(2, 120, false, None), 120);
        admit(
            &mut engine,
            structure(3, 130, true, Some(returned_rows)),
            130,
        );

        let visits_before_bind = &engine.run.as_ref().unwrap().verification.visits;
        assert!(visits_before_bind.left(), "{case}");
        assert!(visits_before_bind.returned(), "{case}");
        assert!(!visits_before_bind.satisfied(), "{case}");

        engine.maintain_goal_binding();
        assert!(
            engine
                .bindings
                .current_row(&SemanticTargetRef::new(SEMANTIC).unwrap())
                .is_none(),
            "{case} must not leave an actionable current row binding"
        );
        assert!(
            !engine.run.as_ref().unwrap().verification.visits.satisfied(),
            "{case} must not count as a fresh semantic rebind"
        );
    }
}
