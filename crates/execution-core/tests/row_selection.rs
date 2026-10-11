//! Pure execution-core regressions for the dedicated AXSelected=true row path.
//! The SimBackend only records this synthetic action; it does not verify or
//! claim that any native accessibility target exists.

mod common;

use common::{binding_proof, rect, snapshot_with_target};
use execution_core::core::{CoreConfig, ExecutionCore};
use execution_core::sim_backend::SimBackend;
use interaction_contracts::desktop::binding::{
    BindingContinuityProof, BindingProof, BindingProofScope, DeviceContextApp, DeviceContextObject,
    DeviceContextPoint, DeviceContextRowSelectorColumn, DeviceContextTableColumn,
    DeviceContextTarget, DeviceContextVisibleRow, DeviceContextWindow, DeviceContextWitness,
    InstanceFingerprint,
};
use interaction_contracts::desktop::device::{
    ActionIntent, CoordinateSpace, GrantPermit, Observation, ObservationPurpose, PermitRef,
    RenewPermit, ScheduledPrimitive, SourceFreshness,
};
use interaction_contracts::desktop::ids::{BindingRef, ObjectRef, RequestId};
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
use interaction_contracts::desktop::source::{LayerKind, ObjectIdentityContinuity, UiValue};

type Core = ExecutionCore<SimBackend>;

fn production() -> Core {
    let mut core = ExecutionCore::new(CoreConfig::production("test"), SimBackend::new());
    core.renew_lease(5_000);
    core
}

fn row_proof(source_sequence: u64) -> BindingProof {
    let window_frame = rect(0.0, 0.0, 800.0, 600.0);
    let viewport_frame = rect(0.0, 0.0, 500.0, 180.0);
    let table_frame = viewport_frame;
    let target_frame = rect(0.0, 10.0, 500.0, 30.0);
    let other_frame = rect(0.0, 40.0, 500.0, 30.0);
    let table_ref = ObjectRef::new("table-1").unwrap();
    let scroll_ref = ObjectRef::new("scroll-1").unwrap();
    let row_ref = ObjectRef::new(format!("row-{source_sequence}")).unwrap();
    let window_id = "window-1".to_string();
    let bundle_id = "test.app".to_string();
    let fingerprint = InstanceFingerprint {
        app: bundle_id.clone(),
        app_pid: 42,
        window_id: window_id.clone(),
        ancestor_roles: vec!["AXWindow".into(), "AXScrollArea".into(), "AXTable".into()],
        group: None,
        role: "AXRow".into(),
        subrole: None,
        label: String::new(),
        sibling_index: 0,
        frame: [0, 10, 500, 30],
        scroll_container: None,
    };
    let scope = BindingProofScope::TableViewport {
        table_instance_ref: table_ref,
        scroll_container_instance_ref: scroll_ref,
        viewport_frame,
        visible_rows_complete: true,
        columns_complete: true,
    };
    let witness = DeviceContextWitness::TableViewportV1 {
        table_ax_enabled: interaction_contracts::desktop::source::AxEnabledObservation::Enabled,
        target_ax_enabled: interaction_contracts::desktop::source::AxEnabledObservation::Enabled,
        app: Box::new(DeviceContextApp {
            bundle_id: bundle_id.clone(),
            pid: 42,
        }),
        window: Box::new(DeviceContextWindow {
            window_id,
            title: Some("records".into()),
            frame: window_frame,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: bundle_id,
        }),
        table: DeviceContextObject {
            role: "AXTable".into(),
            subrole: None,
            label: "Records".into(),
            frame: table_frame,
        },
        target: Box::new(DeviceContextTarget {
            object: DeviceContextObject {
                role: "AXRow".into(),
                subrole: None,
                label: String::new(),
                frame: target_frame,
            },
            group: None,
            scroll: None,
        }),
        scroll_path: vec![DeviceContextObject {
            role: "AXScrollArea".into(),
            subrole: None,
            label: "Records content".into(),
            frame: window_frame,
        }],
        viewport_frame,
        ordered_columns: vec![DeviceContextTableColumn {
            ax_index: 0,
            header: "Owner".into(),
            frame: rect(0.0, 0.0, 250.0, 180.0),
        }],
        selector_columns: vec![DeviceContextRowSelectorColumn {
            ax_index: 0,
            header: "Owner".into(),
            expected_value: UiValue::Text("Ada".into()),
        }],
        visible_rows: vec![
            DeviceContextVisibleRow {
                ax_index: 42,
                frame: target_frame,
                selector_values: vec![UiValue::Text("Ada".into())],
            },
            DeviceContextVisibleRow {
                ax_index: 43,
                frame: other_frame,
                selector_values: vec![UiValue::Text("Bea".into())],
            },
        ],
        target_row_ax_index: 42,
        target_selected: false,
        target_ax_selected_settable: true,
        hit_point: DeviceContextPoint { x: 1.0, y: 25.0 },
    };
    let proof = BindingProof {
        binding: BindingRef::new("row-target").unwrap(),
        instance: row_ref,
        snapshot_id: RequestId::new(format!("row-snapshot-{source_sequence}")).unwrap(),
        environment_revision: 1,
        layer_revision: 1,
        fingerprint,
        display_frame: target_frame,
        continuity: BindingContinuityProof {
            kind: ObjectIdentityContinuity::ReadScoped,
            source_sequence,
            context_digest: format!("bc1:{source_sequence:064x}"),
            scope,
        },
        device_context: witness,
        menu_scope: None,
        focus_scope: None,
    };
    proof.validate().unwrap();
    proof
}

fn row_observation(core: &Core, id: &str, source_sequence: u64) -> Observation {
    Observation {
        observation_id: id.into(),
        session_epoch: core.session_epoch(),
        surface: core.surface(),
        target_ref: "row-target".into(),
        clock_domain: core.clock_domain(),
        source_sequence,
        observed_ms: core.now_ms(),
        valid_until_ms: 1_000,
        freshness: SourceFreshness::Verified,
        execution_domain: core.config().execution_domain,
        purpose: ObservationPurpose::RowSelection,
        binding_proof: Some(row_proof(source_sequence)),
        owned_document_proof: None,
        dependencies: core.dependency_versions(),
    }
}

fn row_intent(
    core: &Core,
    permit: &PermitRef,
    evidence: &Observation,
    intent_id: &str,
    primitive: Primitive,
) -> ActionIntent {
    let proof = evidence.binding_proof.clone().unwrap();
    ActionIntent {
        intent_id: intent_id.into(),
        attempt_id: Some("row-attempt".into()),
        task_id: "root".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: core.session_epoch(),
        execution_domain: evidence.execution_domain,
        permit: permit.clone(),
        surface: core.surface(),
        target_ref: evidence.target_ref.clone(),
        binding_fingerprint: Some(proof.fingerprint.digest()),
        owned_document_proof: None,
        binding_proof: Some(proof),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: evidence.dependencies.clone(),
        evidence_id: evidence.observation_id.clone(),
        latest_start_ms: 500,
        duration_ms: 100,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive,
        }],
    }
}

fn selected_true() -> Primitive {
    Primitive::AxAction {
        action: AxActionKind::SetSelectedTrue,
    }
}

fn row_permit(core: &mut Core, evidence: &Observation) -> PermitRef {
    core.grant_permit(&row_permit_request(evidence)).unwrap()
}

fn row_permit_request(evidence: &Observation) -> GrantPermit {
    GrantPermit {
        acked_stop_seq: 0,
        task_id: "root".into(),
        target_ref: evidence.target_ref.clone(),
        evidence_id: evidence.observation_id.clone(),
        valid_until_ms: 900,
    }
}

#[test]
fn row_permit_admits_only_one_exact_ax_selected_true_and_cannot_renew() {
    let mut core = production();
    core.register_root(&interaction_contracts::desktop::device::RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 4_000,
    })
    .unwrap();
    let evidence = row_observation(&core, "row-observation", 1);
    core.observe(evidence.clone()).unwrap();
    let permit = row_permit(&mut core, &evidence);

    let wrong_primitive = row_intent(
        &core,
        &permit,
        &evidence,
        "row-press-is-not-selection",
        Primitive::AxAction {
            action: AxActionKind::Press,
        },
    );
    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: wrong_primitive.into()
        })
        .status,
        interaction_contracts::desktop::device::ReceiptStatus::Rejected
    );

    let first = row_intent(
        &core,
        &permit,
        &evidence,
        "row-select-once",
        selected_true(),
    );
    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: first.clone().into()
        })
        .status,
        interaction_contracts::desktop::device::ReceiptStatus::Accepted
    );

    let second = row_intent(
        &core,
        &permit,
        &evidence,
        "row-select-twice",
        selected_true(),
    );
    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: second.into()
        })
        .status,
        interaction_contracts::desktop::device::ReceiptStatus::Rejected
    );
    assert!(
        core.renew(RenewPermit {
            session_epoch: core.session_epoch(),
            permit: permit.clone(),
            evidence_id: evidence.observation_id.clone(),
            valid_until_ms: 950,
        })
        .is_err()
    );

    let _ = core.dispatch_next();
    assert_eq!(core.backend().events().len(), 1);
    assert_eq!(core.backend().events()[0].primitive, selected_true());
    let before_replay = core
        .query_receipt(core.session_epoch(), "row-select-once")
        .unwrap();
    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: first.into()
        }),
        before_replay
    );
    assert_eq!(core.backend().events().len(), 1);
}

#[test]
fn one_source_read_grants_one_permit_across_alias_evidence_ids() {
    let mut core = production();
    core.register_root(&interaction_contracts::desktop::device::RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 4_000,
    })
    .unwrap();

    let evidence = row_observation(&core, "row-read-one", 1);
    core.observe(evidence.clone()).unwrap();
    let permit = row_permit(&mut core, &evidence);

    let mut alias = evidence.clone();
    alias.observation_id = "row-read-one-alias".into();
    core.observe(alias.clone())
        .expect("an identical row read may arrive under a fresh evidence id");
    assert!(
        core.grant_permit(&row_permit_request(&alias)).is_err(),
        "an evidence-id alias must not mint a second permit for the same snapshot/sequence"
    );
    assert!(
        core.grant_permit(&row_permit_request(&evidence)).is_err(),
        "replaying the original evidence id must not mint another permit either"
    );

    let next_read = row_observation(&core, "row-read-two", 2);
    core.observe(next_read.clone()).unwrap();
    let next_permit = row_permit(&mut core, &next_read);
    assert_ne!(
        permit, next_permit,
        "a genuinely new Source read can grant once"
    );
}

#[test]
fn row_intent_must_use_its_permits_exact_evidence_and_source_proof() {
    let mut core = production();
    core.register_root(&interaction_contracts::desktop::device::RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 4_000,
    })
    .unwrap();
    let evidence = row_observation(&core, "row-permit-evidence", 1);
    core.observe(evidence.clone()).unwrap();
    let permit = row_permit(&mut core, &evidence);

    let mut alias = evidence.clone();
    alias.observation_id = "row-other-evidence-id".into();
    core.observe(alias.clone()).unwrap();
    let alias_intent = row_intent(
        &core,
        &permit,
        &alias,
        "row-uses-other-evidence-id",
        selected_true(),
    );
    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: alias_intent.into()
        })
        .status,
        interaction_contracts::desktop::device::ReceiptStatus::Rejected
    );

    let mut different_read_intent = row_intent(
        &core,
        &permit,
        &evidence,
        "row-uses-different-source-proof",
        selected_true(),
    );
    let different_read_proof = row_proof(2);
    different_read_intent.binding_fingerprint = Some(different_read_proof.fingerprint.digest());
    different_read_intent.binding_proof = Some(different_read_proof);
    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: different_read_intent.into()
        })
        .status,
        interaction_contracts::desktop::device::ReceiptStatus::Rejected
    );
    assert!(core.backend().events().is_empty());
}

#[test]
fn row_permit_cannot_be_reused_with_ordinary_target_evidence() {
    let mut core = production();
    core.register_root(&interaction_contracts::desktop::device::RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 4_000,
    })
    .unwrap();

    let snapshot = snapshot_with_target(
        "button-1",
        1,
        "AXButton",
        None,
        "Ordinary target",
        rect(10.0, 10.0, 100.0, 40.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    let ordinary = Observation {
        observation_id: "ordinary-evidence".into(),
        session_epoch: core.session_epoch(),
        surface: core.surface(),
        target_ref: "row-target".into(),
        clock_domain: core.clock_domain(),
        source_sequence: 1,
        observed_ms: core.now_ms(),
        valid_until_ms: 1_000,
        freshness: SourceFreshness::Verified,
        execution_domain: core.config().execution_domain,
        purpose: ObservationPurpose::TargetInput,
        binding_proof: Some(binding_proof(
            &snapshot,
            "button-1",
            "row-target",
            None,
            None,
        )),
        owned_document_proof: None,
        dependencies: core.dependency_versions(),
    };
    core.observe(ordinary.clone()).unwrap();
    let row_evidence = row_observation(&core, "row-permit-evidence", 2);
    core.observe(row_evidence.clone()).unwrap();
    let row_permit = row_permit(&mut core, &row_evidence);

    let wrong_purpose_intent = row_intent(
        &core,
        &row_permit,
        &ordinary,
        "row-permit-with-ordinary-evidence",
        Primitive::AxAction {
            action: AxActionKind::Press,
        },
    );
    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: wrong_purpose_intent.into()
        })
        .status,
        interaction_contracts::desktop::device::ReceiptStatus::Rejected
    );
    assert!(core.backend().events().is_empty());
}

#[test]
fn a_new_read_scoped_source_read_invalidates_an_unspent_row_permit() {
    let mut core = production();
    core.register_root(&interaction_contracts::desktop::device::RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 4_000,
    })
    .unwrap();
    let original = row_observation(&core, "row-observation-one", 1);
    core.observe(original.clone()).unwrap();
    let permit = row_permit(&mut core, &original);
    let intent = row_intent(&core, &permit, &original, "stale-row-read", selected_true());

    let next_read = row_observation(&core, "row-observation-two", 2);
    core.observe(next_read).unwrap();
    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: intent.into(),
        })
        .status,
        interaction_contracts::desktop::device::ReceiptStatus::Rejected
    );
    assert!(core.backend().events().is_empty());
}

#[test]
fn notification_covered_row_permit_also_expires_on_a_new_source_read() {
    let mut core = production();
    core.register_root(&interaction_contracts::desktop::device::RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 4_000,
    })
    .unwrap();
    let mut original = row_observation(&core, "covered-row-one", 1);
    let mut original_proof = original.binding_proof.take().unwrap();
    original_proof.instance = ObjectRef::new("covered-row").unwrap();
    original_proof.continuity.kind = ObjectIdentityContinuity::NotificationCovered;
    original_proof.continuity.context_digest = format!("bc1:{}", "a".repeat(64));
    original.binding_proof = Some(original_proof);
    core.observe(original.clone()).unwrap();
    let permit = row_permit(&mut core, &original);
    let intent = row_intent(
        &core,
        &permit,
        &original,
        "covered-row-selection",
        selected_true(),
    );

    let mut next = row_observation(&core, "covered-row-two", 2);
    let mut next_proof = original.binding_proof.clone().unwrap();
    next_proof.snapshot_id = RequestId::new("covered-snapshot-two").unwrap();
    next_proof.continuity.source_sequence = 2;
    next.source_sequence = 2;
    next.binding_proof = Some(next_proof);
    core.observe(next).unwrap();

    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: intent.into(),
        })
        .status,
        interaction_contracts::desktop::device::ReceiptStatus::Rejected
    );
    assert!(core.backend().events().is_empty());
}

#[test]
fn table_viewport_and_set_selected_true_do_not_enter_the_generic_target_route() {
    let mut core = production();
    core.register_root(&interaction_contracts::desktop::device::RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 4_000,
    })
    .unwrap();
    let snapshot = snapshot_with_target(
        "button-1",
        1,
        "AXButton",
        None,
        "Save",
        rect(10.0, 10.0, 100.0, 40.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    let proof = binding_proof(&snapshot, "button-1", "generic-target", None, None);
    let evidence = Observation {
        observation_id: "generic-observation".into(),
        session_epoch: core.session_epoch(),
        surface: core.surface(),
        target_ref: "generic-target".into(),
        clock_domain: core.clock_domain(),
        source_sequence: 1,
        observed_ms: core.now_ms(),
        valid_until_ms: 1_000,
        freshness: SourceFreshness::Verified,
        execution_domain: core.config().execution_domain,
        purpose: ObservationPurpose::TargetInput,
        binding_proof: Some(proof),
        owned_document_proof: None,
        dependencies: core.dependency_versions(),
    };
    core.observe(evidence.clone()).unwrap();
    let permit = core
        .grant_permit(&GrantPermit {
            acked_stop_seq: 0,
            task_id: "root".into(),
            target_ref: evidence.target_ref.clone(),
            evidence_id: evidence.observation_id.clone(),
            valid_until_ms: 900,
        })
        .unwrap();
    let generic_action = ActionIntent {
        intent_id: "generic-set-selected".into(),
        attempt_id: Some("generic-attempt".into()),
        task_id: "root".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: core.session_epoch(),
        execution_domain: evidence.execution_domain,
        permit,
        surface: core.surface(),
        target_ref: evidence.target_ref.clone(),
        binding_fingerprint: evidence
            .binding_proof
            .as_ref()
            .map(|proof| proof.fingerprint.digest()),
        owned_document_proof: None,
        binding_proof: evidence.binding_proof.clone(),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: evidence.dependencies.clone(),
        evidence_id: evidence.observation_id,
        latest_start_ms: 500,
        duration_ms: 100,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: selected_true(),
        }],
    };
    assert!(generic_action.validate().is_err());
    assert_eq!(
        core.submit(interaction_contracts::desktop::device::SubmitIntent {
            acked_stop_seq: 0,
            intent: generic_action.into()
        })
        .status,
        interaction_contracts::desktop::device::ReceiptStatus::Rejected
    );
    assert!(core.backend().events().is_empty());
}
