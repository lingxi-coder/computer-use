mod common;

use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, DeviceIdentity, ObservationPurpose,
    SourceFreshness, SurfaceRef,
};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{ObjectRef, RequestId, RootTaskId, SemanticTargetRef};
use interaction_contracts::desktop::primitive::PrimitivePolicy;
use interaction_contracts::desktop::source::{
    AppIdentity, AxActionName, AxEnabledObservation, GlobalFocusObservation, LayerKind,
    ObjectIdentityContinuity, ObservationFact, ObserverCoverage, Rect, SourceIdentityBoundary,
    SourceState, StructureSnapshot, TableViewport, TableViewportCell, TableViewportColumn,
    TableViewportRow, UiObject, UiValue, WindowInfo,
};
use interaction_contracts::desktop::table_scope::{
    TableScopeCandidate, TableScopePathElement, TableScopeSearch, TableScopeSearchState,
};
use interaction_contracts::desktop::task::{
    ConfirmationScope, ExpectedValue, GoalSpec, Predicate, RootBudgetSpec, RowColumnConditionSpec,
    RowContainerRole, RowContainerScope, RowTableScope, RowTargetSpec, TargetSpec, TaskScope,
};
use interaction_engine::environment::binding::{BindingReducer, RowBinding};
use interaction_engine::environment::pack::AppPack;
use interaction_engine::environment::state_core::{EnvSnapshot, StateCore};
use interaction_engine::execution::DeviceView;
use interaction_engine::reaction::candidates::{Basis, Resolution, Scope, build};
use interaction_engine::reaction::compiler::{CompileInput, CompilerLimits, compile};
use interaction_engine::reaction::skills::row::RowSelectionActionPlan;
use interaction_engine::reaction::skills::{
    ActionOutcome, Skill, SkillResult, SkillRun, SkillStep, SkillView,
};

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn reference(value: String) -> ObjectRef {
    ObjectRef::new(value).unwrap()
}

fn row_target() -> TargetSpec {
    TargetSpec {
        semantic_target: SemanticTargetRef::new("project-row").unwrap(),
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

fn source_object(
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
        window_id: "window-1".into(),
        role: role.into(),
        subrole: None,
        title: title.map(str::to_owned),
        description: description.map(str::to_owned),
        value,
        enabled: true,
        focused: false,
        frame,
        actions: Vec::<AxActionName>::new(),
        ax_action_names: None,
        in_view: true,
        group: None,
        secure: false,
        scroll: None,
        identity_continuity: ObjectIdentityContinuity::ReadScoped,
    }
}

fn snapshot(
    sequence: u64,
    observed_ms: u64,
    selected: Option<bool>,
    settable: Option<bool>,
) -> StructureSnapshot {
    snapshot_with_ax_enabled(
        sequence,
        observed_ms,
        selected,
        settable,
        AxEnabledObservation::Enabled,
        AxEnabledObservation::Enabled,
    )
}

fn snapshot_with_ax_enabled(
    sequence: u64,
    observed_ms: u64,
    selected: Option<bool>,
    settable: Option<bool>,
    table_ax_enabled: AxEnabledObservation,
    row_ax_enabled: AxEnabledObservation,
) -> StructureSnapshot {
    let window = reference(format!("window-object-{sequence}"));
    let outer_scroll = reference(format!("preferences-scroll-{sequence}"));
    let projects_scroll = reference(format!("projects-scroll-{sequence}"));
    let table = reference(format!("table-{sequence}"));
    let row = reference(format!("row-{sequence}"));
    let window_frame = rect(0.0, 0.0, 400.0, 300.0);
    let outer_scroll_frame = rect(5.0, 5.0, 390.0, 290.0);
    let projects_scroll_frame = rect(10.0, 10.0, 360.0, 180.0);
    let row_frame = rect(10.0, 40.0, 360.0, 30.0);
    let headers = ["Name", "Owner", "Status"];
    let values = ["Monthly report", "Acme", "Open"];
    let mut objects = vec![
        source_object(
            &window,
            None,
            "AXWindow",
            window_frame,
            Some("Records"),
            None,
            None,
        ),
        source_object(
            &outer_scroll,
            Some(&window),
            "AXScrollArea",
            outer_scroll_frame,
            None,
            Some("Preferences content"),
            None,
        ),
        source_object(
            &projects_scroll,
            Some(&outer_scroll),
            "AXScrollArea",
            projects_scroll_frame,
            None,
            Some("Projects"),
            None,
        ),
    ];
    let mut table_object = source_object(
        &table,
        Some(&projects_scroll),
        "AXTable",
        projects_scroll_frame,
        None,
        None,
        None,
    );
    table_object.enabled = table_ax_enabled == AxEnabledObservation::Enabled;
    objects.push(table_object);
    let mut row_object = source_object(&row, Some(&table), "AXRow", row_frame, None, None, None);
    row_object.enabled = row_ax_enabled == AxEnabledObservation::Enabled;
    objects.push(row_object);
    let mut columns = Vec::new();
    let mut cells = Vec::new();
    for (index, (header, value)) in headers.iter().zip(values).enumerate() {
        let column_ref = reference(format!("column-{sequence}-{index}"));
        let header_ref = reference(format!("header-{sequence}-{index}"));
        let cell_ref = reference(format!("cell-{sequence}-{index}"));
        let column_frame = rect(10.0 + index as f64 * 120.0, 10.0, 120.0, 180.0);
        let header_frame = rect(column_frame.x, 10.0, 120.0, 25.0);
        let cell_frame = rect(column_frame.x, row_frame.y, 120.0, row_frame.height);
        objects.push(source_object(
            &column_ref,
            Some(&table),
            "AXColumn",
            column_frame,
            None,
            Some(header),
            None,
        ));
        let mut header_object = source_object(
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
        let cell_value = UiValue::Text(value.into());
        objects.push(source_object(
            &cell_ref,
            Some(&row),
            "AXCell",
            cell_frame,
            None,
            None,
            Some(cell_value.clone()),
        ));
        columns.push(TableViewportColumn {
            instance_ref: Some(column_ref.clone()),
            parent_instance_ref: table.clone(),
            frame: column_frame,
            header_instance_ref: Some(header_ref.clone()),
            header: Some((*header).into()),
            ax_index: Some(index as u32),
        });
        cells.push(TableViewportCell {
            instance_ref: Some(cell_ref),
            parent_instance_ref: Some(row.clone()),
            frame: cell_frame,
            row_ax_index: Some(7),
            column_ax_index: Some(index as u32),
            column_instance_ref: Some(column_ref),
            header_instance_ref: Some(header_ref),
            column_header: Some((*header).into()),
            value: Some(cell_value),
        });
    }
    let read_scoped_objects = objects.len() as u32;
    StructureSnapshot {
        snapshot_id: RequestId::new(format!("row-snapshot-{sequence}")).unwrap(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "test/1/monotonic".into(),
        identity_boundary: Some(SourceIdentityBoundary {
            namespace: "11111111-1111-4111-8111-111111111111".into(),
            generation: sequence,
        }),
        frame_ref: None,
        source_sequence: sequence,
        observed_ms,
        app: AppIdentity {
            bundle_id: "com.lingxi.computer-user.mvp-fixture".into(),
            name: "Row fixture".into(),
            pid: 42,
        },
        global_focus: GlobalFocusObservation {
            app: ObservationFact::Known {
                value: AppIdentity {
                    bundle_id: "com.lingxi.computer-user.mvp-fixture".into(),
                    name: "Row fixture".into(),
                    pid: 42,
                },
            },
            window_id: ObservationFact::Known {
                value: "window-1".into(),
            },
        },
        target_context: None,
        windows: vec![WindowInfo {
            window_id: "window-1".into(),
            title: Some("Records".into()),
            frame: window_frame,
            z_index: 0,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: "com.lingxi.computer-user.mvp-fixture".into(),
        }],
        complete: true,
        incomplete_reason: None,
        objects,
        observer_coverage: ObserverCoverage {
            state: SourceState::Degraded,
            covered_objects: 0,
            read_scoped_objects,
            failed_registrations: 0,
            failures: vec![],
        },
        menu_anchors: vec![],
        menu_context: None,
        focus_context: None,
        window_context: None,
        table_scope_search: None,
        table_viewports: vec![TableViewport {
            table_instance_ref: table.clone(),
            scroll_container_instance_ref: projects_scroll,
            process_id: 42,
            ax_enabled: table_ax_enabled,
            table_frame: projects_scroll_frame,
            viewport_frame: projects_scroll_frame,
            visible_rows_complete: true,
            visible_rows: vec![TableViewportRow {
                instance_ref: Some(row),
                parent_instance_ref: table.clone(),
                frame: row_frame,
                ax_enabled: row_ax_enabled,
                ax_index: Some(7),
                accessibility_identifier: None,
                selected,
                ax_selected_settable: settable,
                cells_complete: true,
                cells,
            }],
            columns_complete: true,
            columns,
        }],
        owned_document_evidence: None,
    }
}

fn state_with_first_read(
    selected: Option<bool>,
    settable: Option<bool>,
) -> (StateCore, AppPack, EnvSnapshot) {
    state_with_snapshot(snapshot(1, 100, selected, settable))
}

/// Synthetic Source input for Engine/DeviceView integration only; this does
/// not simulate the native producer's traversal or pixel/privacy acceptance.
fn scoped_snapshot(sequence: u64, observed_ms: u64, selected: Option<bool>) -> StructureSnapshot {
    let mut read = snapshot(sequence, observed_ms, selected, Some(true));
    read.complete = false;
    read.incomplete_reason = Some("table row forest omitted after scoped census".into());
    read.objects
        .iter_mut()
        .find(|object| object.role == "AXWindow")
        .unwrap()
        .subrole = Some("AXStandardWindow".into());
    let scope = row_target().row.unwrap().table_scope;
    let viewport = &read.table_viewports[0];
    let path = scope
        .scroll_path
        .iter()
        .map(|step| {
            let object = read
                .objects
                .iter()
                .find(|object| {
                    object.role == "AXScrollArea"
                        && object.description.as_deref() == Some(step.label.as_str())
                })
                .unwrap();
            TableScopePathElement {
                instance_ref: object.instance_ref.clone(),
                label: step.label.clone(),
                frame: object.frame,
            }
        })
        .collect();
    read.table_scope_search = Some(TableScopeSearch {
        requested_scope: scope.clone(),
        process_id: read.app.pid,
        window_id: read
            .global_focus
            .known_window_for_app(&read.app)
            .expect("the row fixture reports its focused app window")
            .into(),
        state: TableScopeSearchState::Complete,
        candidates: vec![TableScopeCandidate {
            table_instance_ref: viewport.table_instance_ref.clone(),
            scroll_container_instance_ref: viewport.scroll_container_instance_ref.clone(),
            scroll_path: path,
            column_headers: scope.column_headers,
        }],
    });
    read.validate().unwrap();
    read
}

#[test]
fn scoped_row_pipeline_builds_only_the_dedicated_input_and_verifies_a_new_read() {
    let (mut state, pack, env) = state_with_snapshot(scoped_snapshot(1, 100, Some(false)));
    assert!(!state.usable(100, 2_000));
    let scope = row_target().row.unwrap().table_scope;
    assert!(state.row_scope_usable(&scope, 100, 2_000));
    let (mut bindings, binding) = row_binding(&env, 100);
    assert_eq!(binding.table_scope, scope);
    let device = device_view_for(&env);
    let observation = device
        .row_selection_observation(
            &env,
            &binding,
            SourceFreshness::Verified,
            100,
            1_000,
            None,
            100,
            2_000,
        )
        .unwrap();
    assert_eq!(observation.purpose, ObservationPurpose::RowSelection);
    assert!(observation.binding_proof.unwrap().supports_row_selection());
    let object = env
        .snapshot
        .objects
        .iter()
        .find(|object| object.instance_ref == binding.source.row_instance_ref)
        .unwrap();
    assert!(
        interaction_contracts::desktop::binding::derive_binding_evidence(
            &env.snapshot,
            object,
            interaction_contracts::desktop::binding::BindingProofScope::WholeStructure,
        )
        .is_err()
    );
    let mut run = SkillRun::new(
        Skill::SelectRow,
        binding.semantic_target.clone(),
        None,
        binding.binding_ref.clone(),
    )
    .with_row_target(row_target());
    assert!(matches!(
        current_row_step(&mut run, &state, &mut bindings, &pack, 100),
        SkillStep::SelectRow(_)
    ));
    run.acted(ActionOutcome::Completed { end_ms: 150 });
    state
        .admit(scoped_snapshot(2, 150, Some(true)), 150, &pack)
        .unwrap();
    assert!(matches!(
        current_row_step(&mut run, &state, &mut bindings, &pack, 150),
        SkillStep::AwaitEvidence { .. }
    ));
    state
        .admit(scoped_snapshot(3, 151, Some(true)), 151, &pack)
        .unwrap();
    assert_eq!(
        current_row_step(&mut run, &state, &mut bindings, &pack, 151),
        SkillStep::Done(SkillResult::Succeeded)
    );
    assert_eq!(run.actions(), 1);
    assert!(!state.usable(151, 2_000));
}

#[test]
fn scoped_row_pipeline_refuses_missing_incomplete_or_changed_scope_and_stale_proofs() {
    let (_, _, original) = state_with_snapshot(scoped_snapshot(1, 100, Some(false)));
    let (_, binding) = row_binding(&original, 100);
    let device = device_view_for(&original);
    for mutation in ["missing", "incomplete", "changed_scope", "missing_row"] {
        let mut env = original.clone();
        match mutation {
            "missing" => env.snapshot.table_scope_search = None,
            "incomplete" => {
                let search = env.snapshot.table_scope_search.as_mut().unwrap();
                search.state = TableScopeSearchState::Incomplete {
                    reason: "deadline".into(),
                };
                search.candidates.clear();
            }
            "changed_scope" => binding_scope_mutation(&mut env.snapshot),
            "missing_row" => {
                let row = &mut env.snapshot.table_viewports[0].visible_rows[0];
                row.instance_ref = None;
                for cell in &mut row.cells {
                    cell.instance_ref = None;
                    cell.parent_instance_ref = None;
                }
            }
            _ => unreachable!(),
        }
        env.snapshot.validate().unwrap();
        assert!(
            BindingReducer::new()
                .bind_row(&env, &row_target(), 100, 2_000)
                .is_err(),
            "{mutation}"
        );
        assert!(
            device
                .row_selection_observation(
                    &env,
                    &binding,
                    SourceFreshness::Verified,
                    100,
                    1_000,
                    None,
                    100,
                    2_000
                )
                .is_err(),
            "{mutation}"
        );
    }
    assert!(
        device
            .row_selection_observation(
                &original,
                &binding,
                SourceFreshness::Verified,
                100,
                3_000,
                None,
                2_101,
                2_000
            )
            .is_err()
    );
}

fn binding_scope_mutation(snapshot: &mut StructureSnapshot) {
    let search = snapshot.table_scope_search.as_mut().unwrap();
    search.requested_scope.scroll_path[0].label = "Elsewhere".into();
    search.candidates[0].scroll_path[0].label = "Elsewhere".into();
    let reference = search.candidates[0].scroll_path[0].instance_ref.clone();
    snapshot
        .objects
        .iter_mut()
        .find(|object| object.instance_ref == reference)
        .unwrap()
        .description = Some("Elsewhere".into());
}

fn state_with_snapshot(snapshot: StructureSnapshot) -> (StateCore, AppPack, EnvSnapshot) {
    let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
    let mut state = StateCore::new();
    state.set_session(1);
    let now_ms = snapshot.observed_ms;
    state.admit(snapshot, now_ms, &pack).unwrap();
    let env = state.latest().unwrap().clone();
    (state, pack, env)
}

fn device_view_for(env: &EnvSnapshot) -> DeviceView {
    let mut device = DeviceView::new();
    device.install_manifest(CapabilityManifest {
        identity: DeviceIdentity {
            session_epoch: env.snapshot.session_epoch,
            surface: env.snapshot.surface.clone(),
            clock_domain: env.snapshot.clock_domain.clone(),
            dependencies: Default::default(),
        },
        backend: BackendKind::Simulated,
        execution_domain: ExecutionDomain::Simulated,
        policy: PrimitivePolicy::AtomicOnly,
        human_input: CapabilityState::Available,
        hit_test: CapabilityState::Available,
        input: CapabilityState::Available,
        domains: common::domain_capabilities_for_test_profile(ExecutionDomain::Simulated),
        max_uninterruptible_ms: 100,
    });
    device
}

fn row_binding(env: &EnvSnapshot, now_ms: u64) -> (BindingReducer, RowBinding) {
    let mut bindings = BindingReducer::new();
    let binding = bindings
        .bind_row(env, &row_target(), now_ms, 2_000)
        .unwrap();
    (bindings, binding)
}

fn row_goal() -> GoalSpec {
    let target = row_target();
    GoalSpec {
        root_task_id: RootTaskId::new("row-root").unwrap(),
        task_revision: 1,
        execution_domain: ExecutionDomain::Simulated,
        browser_page_profile: None,
        user_text: "Select the Monthly report row".into(),
        scope: TaskScope {
            app: "com.lingxi.computer-user.mvp-fixture".into(),
            account: None,
        },
        allowed_scope: None,
        scoped_revisit: None,
        scoped_commit: None,
        target: target.clone(),
        desired: ExpectedValue::Bool { value: true },
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::Navigation,
        required: Predicate::And {
            of: vec![
                Predicate::ContextMatches {
                    app: "com.lingxi.computer-user.mvp-fixture".into(),
                },
                Predicate::RowSelected {
                    target: target.semantic_target.clone(),
                    observed_after_action: true,
                },
                Predicate::Revisited {
                    target: target.semantic_target,
                },
            ],
        },
        budget: RootBudgetSpec {
            deadline_ms: 10_000,
            max_attempts: 4,
            max_observations: 8,
            max_recoveries: 1,
            max_replans: 0,
        },
    }
}

fn current_row_step<'a>(
    run: &'a mut SkillRun,
    state: &'a StateCore,
    bindings: &'a mut BindingReducer,
    pack: &'a AppPack,
    now_ms: u64,
) -> SkillStep {
    run.advance(&mut SkillView {
        state,
        bindings,
        pack,
        now_ms,
        max_age_ms: 2_000,
    })
}

#[test]
fn row_candidate_and_decision_projection_hide_all_binding_and_source_refs() {
    let (_, pack, env) = state_with_first_read(Some(false), Some(true));
    let (bindings, binding) = row_binding(&env, 100);
    let goal = row_goal();
    let plan_targets =
        std::collections::BTreeSet::from([goal.target.semantic_target.as_str().to_string()]);
    let resolution = Resolution::Row(Box::new(binding.clone()));
    let candidates = build(
        &Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &pack,
            env: &env,
        },
        &Basis::Step {
            subgoal: &interaction_contracts::desktop::planning::Subgoal {
                id: "select-row".into(),
                skill: "ui.select_row".into(),
                target: Some(goal.target.semantic_target.clone()),
                description: "select frozen row".into(),
                expects: vec![],
                scroll: None,
                select: None,
            },
            resolution: &resolution,
        },
    );
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.skill == "ui.select_row")
        .unwrap();
    assert!(candidate.parameters.is_empty());
    assert_eq!(candidate.binding.as_ref(), Some(&binding.binding_ref));
    assert!(
        !candidate
            .parameters
            .values()
            .any(|value| value.contains("row-1"))
    );
    let decision_state = compile(
        CompileInput {
            goal: &goal,
            subgoal: "select the row",
            env: &env,
            now_ms: 100,
            candidates,
            recent_results: &[],
            missing_fields: vec![],
        },
        &CompilerLimits::default(),
    )
    .unwrap();
    assert!(decision_state.objects.is_empty());
    assert!(
        decision_state
            .candidates
            .iter()
            .all(|candidate| candidate.binding.is_none())
    );
    let row_candidate = decision_state
        .candidates
        .iter()
        .find(|candidate| candidate.skill == "ui.select_row")
        .unwrap();
    assert!(
        row_candidate
            .description
            .contains("AXScrollArea \"Projects\"")
    );
    assert!(
        row_candidate
            .description
            .contains("AXScrollArea \"Preferences content\"")
    );
    assert!(
        row_candidate
            .description
            .contains("Name = \"Monthly report\"")
            && row_candidate.description.contains("Owner = \"Acme\"")
            && row_candidate.description.contains("Status")
    );
    let wire = serde_json::to_string(&decision_state).unwrap();
    assert!(!wire.contains(binding.binding_ref.as_str()));
    assert!(!wire.contains("row-1"));
    assert!(!wire.contains("table-1"));
    assert!(!wire.contains("cell-1"));
    assert!(bindings.contains_binding_ref(&binding.binding_ref));
}

#[test]
fn row_revisit_candidate_is_navigation_not_read_only() {
    let (_, pack, env) = state_with_first_read(Some(true), Some(false));
    let (_, binding) = row_binding(&env, 100);
    let goal = row_goal();
    let plan_targets =
        std::collections::BTreeSet::from([goal.target.semantic_target.as_str().to_string()]);
    let resolution = Resolution::Row(Box::new(binding));
    let candidates = build(
        &Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &pack,
            env: &env,
        },
        &Basis::Step {
            subgoal: &interaction_contracts::desktop::planning::Subgoal {
                id: "revisit-row".into(),
                skill: "ui.revisit".into(),
                target: Some(goal.target.semantic_target.clone()),
                description: "leave and return to verify the selected row".into(),
                expects: vec![],
                scroll: None,
                select: None,
            },
            resolution: &resolution,
        },
    );
    let revisit = candidates
        .iter()
        .find(|candidate| candidate.skill == "ui.revisit")
        .unwrap();

    assert_eq!(revisit.effect_class, EffectClass::Navigation);
    assert_eq!(
        revisit.precondition,
        interaction_contracts::desktop::decision::Precondition::Executable
    );
}

#[test]
fn source_read_rotation_gets_a_new_binding_ref_for_the_same_frozen_selector() {
    let (mut state, pack, env1) = state_with_first_read(Some(false), Some(true));
    let (mut bindings, first) = row_binding(&env1, 100);
    assert_eq!(first.source.source_sequence, 1);
    state
        .admit(snapshot(2, 200, Some(false), Some(true)), 200, &pack)
        .unwrap();
    let env2 = state.latest().unwrap().clone();
    let invalidated = bindings.revalidate(&env2);
    assert!(
        invalidated
            .iter()
            .any(|item| item.binding == first.binding_ref)
    );
    let second = bindings.bind_row(&env2, &row_target(), 200, 2_000).unwrap();
    assert_eq!(second.source.source_sequence, 2);
    assert_ne!(second.binding_ref, first.binding_ref);
    assert_eq!(second.selector.conditions, first.selector.conditions);
    assert!(!bindings.contains_binding_ref(&first.binding_ref));
}

#[test]
fn selected_true_is_already_done_and_never_emits_an_input() {
    let (state, pack, env) = state_with_snapshot(snapshot_with_ax_enabled(
        1,
        100,
        Some(true),
        Some(false),
        AxEnabledObservation::Unavailable,
        AxEnabledObservation::Disabled,
    ));
    let (mut bindings, binding) = row_binding(&env, 100);
    assert!(binding.hit_point.is_none());
    assert!(binding.continuity.is_none());
    assert!(binding.device_context.is_none());
    let mut run = SkillRun::new(
        Skill::SelectRow,
        binding.semantic_target.clone(),
        None,
        binding.binding_ref.clone(),
    )
    .with_row_target(row_target());
    assert_eq!(
        current_row_step(&mut run, &state, &mut bindings, &pack, 100),
        SkillStep::Done(SkillResult::AlreadyDone)
    );
    assert_eq!(run.actions(), 0);
}

#[test]
fn explicit_axenabled_unsupported_allows_setselected_with_generic_enabled_false() {
    let (state, pack, env) = state_with_snapshot(snapshot_with_ax_enabled(
        1,
        100,
        Some(false),
        Some(true),
        AxEnabledObservation::AttributeUnsupported,
        AxEnabledObservation::AttributeUnsupported,
    ));
    let table = env
        .snapshot
        .objects
        .iter()
        .find(|object| object.role == "AXTable")
        .unwrap();
    let row = env
        .snapshot
        .objects
        .iter()
        .find(|object| object.role == "AXRow")
        .unwrap();
    assert!(!table.enabled);
    assert!(!row.enabled);

    let (mut bindings, binding) = row_binding(&env, 100);
    assert_eq!(
        binding.source.table_ax_enabled,
        AxEnabledObservation::AttributeUnsupported
    );
    assert_eq!(
        binding.source.row_ax_enabled,
        AxEnabledObservation::AttributeUnsupported
    );
    assert!(binding.hit_point.is_some());
    assert!(RowSelectionActionPlan::new(binding.clone()).is_ok());

    let device = device_view_for(&env);
    let observation = device
        .row_selection_observation(
            &env,
            &binding,
            SourceFreshness::Verified,
            1_000,
            2_000,
            None,
            100,
            2_000,
        )
        .expect("DeviceView must accept the exact typed row proof");
    assert_eq!(observation.purpose, ObservationPurpose::RowSelection);
    let proof = observation
        .binding_proof
        .as_ref()
        .expect("row observation carries its exact binding proof");
    assert!(proof.supports_row_selection());
    match &proof.device_context {
        interaction_contracts::desktop::binding::DeviceContextWitness::TableViewportV1 {
            table_ax_enabled,
            target_ax_enabled,
            ..
        } => {
            assert_eq!(
                *table_ax_enabled,
                AxEnabledObservation::AttributeUnsupported
            );
            assert_eq!(
                *target_ax_enabled,
                AxEnabledObservation::AttributeUnsupported
            );
        }
        _ => panic!("DeviceView must retain both typed AXEnabled witnesses"),
    }

    for (table_ax_enabled, row_ax_enabled) in [
        (
            AxEnabledObservation::Disabled,
            AxEnabledObservation::AttributeUnsupported,
        ),
        (
            AxEnabledObservation::Unavailable,
            AxEnabledObservation::AttributeUnsupported,
        ),
        (
            AxEnabledObservation::AttributeUnsupported,
            AxEnabledObservation::Disabled,
        ),
        (
            AxEnabledObservation::AttributeUnsupported,
            AxEnabledObservation::Unavailable,
        ),
    ] {
        let mut stale_env = env.clone();
        stale_env.snapshot.table_viewports[0].ax_enabled = table_ax_enabled;
        stale_env.snapshot.table_viewports[0].visible_rows[0].ax_enabled = row_ax_enabled;
        stale_env
            .snapshot
            .objects
            .iter_mut()
            .find(|object| object.role == "AXTable")
            .unwrap()
            .enabled = table_ax_enabled == AxEnabledObservation::Enabled;
        stale_env
            .snapshot
            .objects
            .iter_mut()
            .find(|object| object.role == "AXRow")
            .unwrap()
            .enabled = row_ax_enabled == AxEnabledObservation::Enabled;
        assert!(stale_env.snapshot.validate().is_ok());
        assert!(
            device
                .row_selection_observation(
                    &stale_env,
                    &binding,
                    SourceFreshness::Verified,
                    1_000,
                    2_000,
                    None,
                    100,
                    2_000,
                )
                .is_err()
        );

        let mut tampered_binding = binding.clone();
        tampered_binding.source.table_ax_enabled = table_ax_enabled;
        tampered_binding.source.row_ax_enabled = row_ax_enabled;
        assert!(
            device
                .row_selection_observation(
                    &stale_env,
                    &tampered_binding,
                    SourceFreshness::Verified,
                    1_000,
                    2_000,
                    None,
                    100,
                    2_000,
                )
                .is_err()
        );
    }

    let mut disabled_after_binding = binding.clone();
    disabled_after_binding.source.row_ax_enabled = AxEnabledObservation::Disabled;
    assert!(RowSelectionActionPlan::new(disabled_after_binding).is_err());

    let mut run = SkillRun::new(
        Skill::SelectRow,
        binding.semantic_target.clone(),
        None,
        binding.binding_ref.clone(),
    )
    .with_row_target(row_target());
    assert!(matches!(
        current_row_step(&mut run, &state, &mut bindings, &pack, 100),
        SkillStep::SelectRow(_)
    ));
    assert_eq!(run.actions(), 1);
}

#[test]
fn disabled_error_or_nonsettable_row_never_produces_selection_input() {
    for (table_enabled, row_enabled, settable) in [
        (
            AxEnabledObservation::Disabled,
            AxEnabledObservation::Enabled,
            Some(true),
        ),
        (
            AxEnabledObservation::Enabled,
            AxEnabledObservation::Disabled,
            Some(true),
        ),
        (
            AxEnabledObservation::Unavailable,
            AxEnabledObservation::Enabled,
            Some(true),
        ),
        (
            AxEnabledObservation::Enabled,
            AxEnabledObservation::Unavailable,
            Some(true),
        ),
        (
            AxEnabledObservation::Enabled,
            AxEnabledObservation::Enabled,
            Some(false),
        ),
    ] {
        let (state, pack, env) = state_with_snapshot(snapshot_with_ax_enabled(
            1,
            100,
            Some(false),
            settable,
            table_enabled,
            row_enabled,
        ));
        let (mut bindings, binding) = row_binding(&env, 100);
        assert!(binding.hit_point.is_none());
        assert!(binding.continuity.is_none());
        assert!(binding.device_context.is_none());
        assert!(RowSelectionActionPlan::new(binding.clone()).is_err());
        let mut run = SkillRun::new(
            Skill::SelectRow,
            binding.semantic_target.clone(),
            None,
            binding.binding_ref.clone(),
        )
        .with_row_target(row_target());
        assert!(matches!(
            current_row_step(&mut run, &state, &mut bindings, &pack, 100),
            SkillStep::Done(SkillResult::Failed { .. })
        ));
        assert_eq!(run.actions(), 0);
    }
}

#[test]
fn one_completed_row_action_requires_a_new_semantic_read_and_is_never_replayed() {
    let (mut state, pack, env1) = state_with_first_read(Some(false), Some(true));
    let (mut bindings, first) = row_binding(&env1, 100);
    let mut run = SkillRun::new(
        Skill::SelectRow,
        first.semantic_target.clone(),
        None,
        first.binding_ref.clone(),
    )
    .with_row_target(row_target());
    let SkillStep::SelectRow(plan) = current_row_step(&mut run, &state, &mut bindings, &pack, 100)
    else {
        panic!("an unselected settable row must use the dedicated typed route");
    };
    assert_eq!(
        plan.primitive(),
        interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::SetSelectedTrue,
        }
    );
    run.acted(ActionOutcome::Completed { end_ms: 150 });
    assert_eq!(
        current_row_step(&mut run, &state, &mut bindings, &pack, 150),
        SkillStep::AwaitEvidence { after_ms: 151 }
    );
    state
        .admit(snapshot(2, 151, Some(false), Some(true)), 151, &pack)
        .unwrap();
    let env2 = state.latest().unwrap().clone();
    bindings.revalidate(&env2);
    let SkillStep::Done(SkillResult::Failed { .. }) =
        current_row_step(&mut run, &state, &mut bindings, &pack, 151)
    else {
        panic!("sticky false selection must stop, not issue another AX action");
    };
    assert_eq!(run.actions(), 1);
}

#[test]
fn unknown_row_action_outcome_is_terminal_for_the_attempt() {
    let (state, pack, env) = state_with_first_read(Some(false), Some(true));
    let (mut bindings, binding) = row_binding(&env, 100);
    let mut run = SkillRun::new(
        Skill::SelectRow,
        binding.semantic_target.clone(),
        None,
        binding.binding_ref.clone(),
    )
    .with_row_target(row_target());
    assert!(matches!(
        current_row_step(&mut run, &state, &mut bindings, &pack, 100),
        SkillStep::SelectRow(_)
    ));
    run.acted(ActionOutcome::Unknown { at_ms: 120 });
    assert!(matches!(
        current_row_step(&mut run, &state, &mut bindings, &pack, 120),
        SkillStep::Done(SkillResult::Failed { .. })
    ));
    assert_eq!(run.actions(), 1);
}
