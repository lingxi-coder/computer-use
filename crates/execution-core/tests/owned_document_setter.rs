//! Core admission tests for the explicit owned-document AXSetValue profile.

mod common;

use common::{binding_proof, snapshot_with_target};
use execution_core::core::{CoreConfig, ExecutionCore, PermitState};
use execution_core::sim_backend::SimBackend;
use interaction_contracts::desktop::binding::BindingProof;
use interaction_contracts::desktop::device::*;
use interaction_contracts::desktop::ids::{BindingRef, ObjectRef, RequestId, SemanticTargetRef};
use interaction_contracts::desktop::owned_document::{
    AxPropertyObservation, DocumentBindingEvidence, DocumentBindingRead, DocumentReadStatus,
    MarkedRangeRead, MarkedTextRange, OwnedDocumentAncestor, OwnedDocumentProfile,
    OwnedDocumentReceiverEvidence, OwnedDocumentSourceProvenance, OwnedScopeObserverState,
};
use interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof;
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
use interaction_contracts::desktop::source::{
    KernelProcessIdentity, ObjectIdentityContinuity, Rect, SourceIdentityBoundary,
};

type Core = ExecutionCore<SimBackend>;

#[test]
fn private_dependency_bootstrap_preserves_invalid_and_unknown_states() {
    let mut core = core(false);
    assert_eq!(core.dependency_validity()["focus"], Some(true));
    core.update_dependency("focus", None);
    core.update_dependency("window", Some(false));
    assert_eq!(core.dependency_validity()["focus"], None);
    assert_eq!(core.dependency_validity()["window"], Some(false));
}

fn core(with_lease: bool) -> Core {
    core_with_config(with_lease, CoreConfig::owned_document_setter("owned-test"))
}

fn core_with_config(with_lease: bool, config: CoreConfig) -> Core {
    let mut core = ExecutionCore::new(config, SimBackend::new());
    core.advance_to(1_000).unwrap();
    if with_lease {
        core.renew_lease(10_000);
    }
    core
}

fn register(core: &mut Core) {
    core.register_root(&RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 5_000,
    })
    .unwrap();
}

fn evidence(core: &Core, environment_revision: u64, sequence: u64, generation: u64) -> Observation {
    evidence_for_purpose(
        core,
        environment_revision,
        sequence,
        generation,
        ObservationPurpose::OwnedDocumentValueSetter,
    )
}

fn evidence_for_purpose(
    core: &Core,
    environment_revision: u64,
    sequence: u64,
    generation: u64,
    purpose: ObservationPurpose,
) -> Observation {
    let observed_ms = core.now_ms();
    let clock_domain = core.clock_domain();
    let receiver_ref = ObjectRef::new("owned-receiver").unwrap();
    let window_ref = ObjectRef::new("owned-window").unwrap();
    let scroll_ref = ObjectRef::new("owned-scroll").unwrap();
    let proof = OwnedDocumentBindingProof {
        binding: BindingRef::new("owned-target").unwrap(),
        semantic_target: SemanticTargetRef::new("document-value").unwrap(),
        environment_revision,
        session_epoch: core.session_epoch(),
        surface: core.surface(),
        snapshot_observed_ms: observed_ms,
        receiver: Box::new(OwnedDocumentReceiverEvidence {
            value_readback: None,
            registration_id: "registration-1".into(),
            profile: OwnedDocumentProfile::OwnedPlainTextValueSetterV1,
            provenance: OwnedDocumentSourceProvenance {
                identity_boundary: SourceIdentityBoundary {
                    namespace: "d85e0f74-b6f7-4bce-8f8c-563e4b12da29".into(),
                    generation,
                },
                snapshot_id: RequestId::new(format!("snapshot-{sequence}")).unwrap(),
                source_sequence: sequence,
                observed_ms: observed_ms - 10,
                valid_until_ms: observed_ms + 400,
                clock_domain: clock_domain.clone(),
            },
            process: KernelProcessIdentity {
                pid: 700,
                start_seconds: 1_700_000_000,
                start_micros: 123_456,
            },
            window_id: "window-42".into(),
            window_object_ref: window_ref.clone(),
            window_frame: rect(0.0, 0.0, 600.0, 400.0),
            actual_object_ref: receiver_ref.clone(),
            receiver_frame: rect(20.0, 40.0, 500.0, 300.0),
            application_focused_object_ref: receiver_ref.clone(),
            system_focused_object_ref: receiver_ref.clone(),
            ancestry: vec![
                OwnedDocumentAncestor {
                    object_ref: scroll_ref,
                    role: "AXScrollArea".into(),
                },
                OwnedDocumentAncestor {
                    object_ref: window_ref,
                    role: "AXWindow".into(),
                },
            ],
            scope_complete: true,
            profile_eligible: true,
            marked_range_stable_unmarked: true,
            observer_state: OwnedScopeObserverState::Continuous,
            secure: false,
            document_binding_verified: true,
            document_binding: DocumentBindingEvidence {
                before: DocumentBindingRead {
                    status: DocumentReadStatus::Value,
                    api_code: Some(0),
                    cf_type_id: Some(7),
                    observed_ms: observed_ms - 5,
                    timely: true,
                    registered_path_matches: true,
                },
                after: DocumentBindingRead {
                    status: DocumentReadStatus::Value,
                    api_code: Some(0),
                    cf_type_id: Some(7),
                    observed_ms: observed_ms - 4,
                    timely: true,
                    registered_path_matches: true,
                },
            },
            title: AxPropertyObservation::Value {
                value: "Owned fixture.txt".into(),
            },
            role: AxPropertyObservation::Value {
                value: "AXTextArea".into(),
            },
            subrole: AxPropertyObservation::Unsupported { code: -25205 },
            enabled: AxPropertyObservation::Unsupported { code: -25205 },
            focused: AxPropertyObservation::Value { value: true },
            value_settable: AxPropertyObservation::Value { value: true },
            description: AxPropertyObservation::Failure { code: -25200 },
            marked_range_before: MarkedRangeRead {
                receiver_object_ref: receiver_ref.clone(),
                receiver_same_cf_as_actual_object_ref: true,
                observed_ms: observed_ms - 3,
                observation: AxPropertyObservation::Value {
                    value: MarkedTextRange {
                        location: 0,
                        length: 0,
                    },
                },
            },
            marked_range_after: MarkedRangeRead {
                receiver_object_ref: receiver_ref,
                receiver_same_cf_as_actual_object_ref: true,
                observed_ms: observed_ms - 2,
                observation: AxPropertyObservation::Value {
                    value: MarkedTextRange {
                        location: 0,
                        length: 0,
                    },
                },
            },
        }),
    };
    let dependencies = core
        .dependency_versions()
        .into_iter()
        .filter(|(name, _)| {
            ["authorization", "focus", "window", "owned_scope"].contains(&name.as_str())
        })
        .collect();
    Observation {
        observation_id: format!("owned-observation-{sequence}"),
        session_epoch: core.session_epoch(),
        surface: core.surface(),
        target_ref: proof.binding.as_str().into(),
        clock_domain,
        source_sequence: sequence,
        observed_ms,
        valid_until_ms: observed_ms + 300,
        freshness: SourceFreshness::Verified,
        execution_domain: core.config().execution_domain,
        purpose,
        binding_proof: None,
        owned_document_proof: Some(Box::new(proof)),
        dependencies,
    }
}

fn permit_for(core: &mut Core, observation: &Observation) -> PermitRef {
    core.observe(observation.clone()).unwrap();
    core.grant_permit(&GrantPermit {
        acked_stop_seq: 0,
        task_id: "root".into(),
        target_ref: observation.target_ref.clone(),
        evidence_id: observation.observation_id.clone(),
        valid_until_ms: observation.observed_ms + 200,
    })
    .unwrap()
}

fn set_value_intent(
    core: &Core,
    observation: &Observation,
    permit: &PermitRef,
    id: &str,
) -> ActionIntent {
    ActionIntent {
        intent_id: id.into(),
        attempt_id: Some(format!("attempt-{id}")),
        task_id: "root".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: core.session_epoch(),
        execution_domain: observation.execution_domain,
        permit: permit.clone(),
        surface: core.surface(),
        target_ref: observation.target_ref.clone(),
        binding_fingerprint: None,
        binding_proof: None,
        owned_document_proof: observation.owned_document_proof.clone(),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: observation.dependencies.clone(),
        evidence_id: observation.observation_id.clone(),
        latest_start_ms: core.now_ms(),
        duration_ms: 100,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: Primitive::AxAction {
                action: AxActionKind::SetValue {
                    value: "bounded fixture text".into(),
                },
            },
        }],
    }
}

fn save_intent(
    core: &Core,
    observation: &Observation,
    permit: &PermitRef,
    id: &str,
) -> ActionIntent {
    let mut intent = set_value_intent(core, observation, permit, id);
    intent.steps[0].primitive = Primitive::AxAction {
        action: AxActionKind::SaveOwnedDocument {
            expected_value: "saved fixture text\n".into(),
        },
    };
    intent
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn explicit_profile_admits_one_owned_setter_without_generic_hit_test_and_keeps_generic_gates() {
    assert_eq!(
        CoreConfig::owned_document_setter("owned-test").required_dependencies,
        CoreConfig::production("owned-test").required_dependencies
    );
    assert!(!CoreConfig::production("owned-test").allow_owned_document_value_setter);
    assert!(!CoreConfig::production("owned-test").allow_owned_document_save);
    assert!(!CoreConfig::owned_document_setter("owned-test").allow_owned_document_save);
    assert!(CoreConfig::owned_document_actions("owned-test").allow_owned_document_save);
    assert_eq!(
        CoreConfig::owned_document_actions("owned-test").required_dependencies,
        CoreConfig::production("owned-test").required_dependencies
    );

    let mut core = core(true);
    register(&mut core);
    let observation = evidence(&core, 1, 1, 3);
    let permit = permit_for(&mut core, &observation);
    let intent = set_value_intent(&core, &observation, &permit, "set-once");

    // The scoped native setter path does its own receiver re-probe; the Core
    // must not require a generic pointer hit-test for this exact primitive.
    core.backend_mut().set_hit_test(SimHitTest::Unresolved);
    let accepted = core.submit(SubmitIntent {
        acked_stop_seq: 0,
        intent: intent.clone().into(),
    });
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    core.dispatch_next();
    core.advance_to(1_100).unwrap();
    assert_eq!(
        core.query_receipt(core.session_epoch(), "set-once")
            .unwrap()
            .status,
        ReceiptStatus::Completed
    );
    assert_eq!(core.backend().events().len(), 1);
    assert!(matches!(
        &core.backend().events()[0].primitive,
        Primitive::AxAction {
            action: AxActionKind::SetValue { .. }
        }
    ));

    let replay = core.submit(SubmitIntent {
        acked_stop_seq: 0,
        intent: intent.into(),
    });
    assert_eq!(replay.status, ReceiptStatus::Completed);
    assert_eq!(core.backend().events().len(), 1);
}

#[test]
fn owned_document_save_requires_explicit_profile_and_matching_observation_purpose() {
    let mut setter_only = core(true);
    let setter_only_save =
        evidence_for_purpose(&setter_only, 1, 1, 3, ObservationPurpose::OwnedDocumentSave);
    assert!(
        setter_only
            .observe(setter_only_save)
            .unwrap_err()
            .contains("disabled")
    );

    let mut save_core = core_with_config(true, CoreConfig::owned_document_actions("owned-test"));
    register(&mut save_core);
    let save_evidence =
        evidence_for_purpose(&save_core, 1, 1, 3, ObservationPurpose::OwnedDocumentSave);
    let save_permit = permit_for(&mut save_core, &save_evidence);
    let save = save_intent(&save_core, &save_evidence, &save_permit, "owned-save");
    save.validate().unwrap();

    save_core.backend_mut().set_hit_test(SimHitTest::Unresolved);
    assert_eq!(
        save_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: save.into()
            })
            .status,
        ReceiptStatus::Accepted
    );
    save_core.dispatch_next();
    save_core.advance_to(1_100).unwrap();
    assert_eq!(
        save_core
            .query_receipt(save_core.session_epoch(), "owned-save")
            .unwrap()
            .status,
        ReceiptStatus::Completed
    );
    assert_eq!(save_core.backend().events().len(), 1);
    assert!(matches!(
        &save_core.backend().events()[0].primitive,
        Primitive::AxAction {
            action: AxActionKind::SaveOwnedDocument { .. }
        }
    ));

    let mut setter_core = core_with_config(true, CoreConfig::owned_document_actions("owned-test"));
    register(&mut setter_core);
    let setter_evidence = evidence(&setter_core, 1, 1, 3);
    let setter_permit = permit_for(&mut setter_core, &setter_evidence);
    let wrong_save = save_intent(
        &setter_core,
        &setter_evidence,
        &setter_permit,
        "wrong-save-purpose",
    );
    assert_eq!(
        setter_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: wrong_save.into()
            })
            .status,
        ReceiptStatus::Rejected
    );
    assert!(setter_core.backend().events().is_empty());

    let mut default_core = core_with_config(false, CoreConfig::production("owned-test"));
    // The contract fixture is internally valid so this reaches the explicit
    // profile gate rather than failing because production lacks owned_scope.
    default_core.update_dependency("owned_scope", Some(true));
    let default_save = evidence_for_purpose(
        &default_core,
        1,
        1,
        3,
        ObservationPurpose::OwnedDocumentSave,
    );
    assert!(
        default_core
            .observe(default_save)
            .unwrap_err()
            .contains("disabled")
    );

    let mut wrong_primitive_core =
        core_with_config(true, CoreConfig::owned_document_actions("owned-test"));
    register(&mut wrong_primitive_core);
    let save_purpose = evidence_for_purpose(
        &wrong_primitive_core,
        1,
        1,
        3,
        ObservationPurpose::OwnedDocumentSave,
    );
    let permit = permit_for(&mut wrong_primitive_core, &save_purpose);
    let wrong_setter = set_value_intent(
        &wrong_primitive_core,
        &save_purpose,
        &permit,
        "wrong-setter-purpose",
    );
    assert_eq!(
        wrong_primitive_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: wrong_setter.into()
            })
            .status,
        ReceiptStatus::Rejected
    );
    assert!(wrong_primitive_core.backend().events().is_empty());
}

#[test]
fn owned_document_save_rejects_generic_proof_stale_or_over_deadline_and_unknown_is_not_replayed() {
    let mut save_core = core_with_config(true, CoreConfig::owned_document_actions("owned-test"));
    register(&mut save_core);
    let save_evidence =
        evidence_for_purpose(&save_core, 1, 1, 3, ObservationPurpose::OwnedDocumentSave);
    let save_permit = permit_for(&mut save_core, &save_evidence);
    let mut generic_save = save_intent(&save_core, &save_evidence, &save_permit, "generic-save");
    generic_save.owned_document_proof = None;
    generic_save.binding_proof = Some(generic_binding_proof(&generic_save.target_ref));
    generic_save.binding_fingerprint = Some(
        generic_save
            .binding_proof
            .as_ref()
            .unwrap()
            .fingerprint
            .digest(),
    );
    assert_eq!(
        save_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: generic_save.into()
            })
            .status,
        ReceiptStatus::Rejected
    );
    assert!(save_core.backend().events().is_empty());

    let mut stale_core = core_with_config(true, CoreConfig::owned_document_actions("owned-test"));
    let stale = evidence_for_purpose(&stale_core, 1, 1, 3, ObservationPurpose::OwnedDocumentSave);
    stale_core.advance_to(1_400).unwrap();
    assert!(stale_core.observe(stale).is_err());

    let mut late_core = core_with_config(true, CoreConfig::owned_document_actions("owned-test"));
    register(&mut late_core);
    let evidence = evidence_for_purpose(&late_core, 1, 1, 3, ObservationPurpose::OwnedDocumentSave);
    let permit = permit_for(&mut late_core, &evidence);
    let mut late = save_intent(&late_core, &evidence, &permit, "late-save");
    late.duration_ms = 301;
    assert!(late.validate().is_ok());
    assert_eq!(
        late_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: late.into()
            })
            .status,
        ReceiptStatus::Rejected
    );
    assert!(late_core.backend().events().is_empty());

    let mut unknown_core = core_with_config(true, CoreConfig::owned_document_actions("owned-test"));
    register(&mut unknown_core);
    let evidence = evidence_for_purpose(
        &unknown_core,
        1,
        1,
        3,
        ObservationPurpose::OwnedDocumentSave,
    );
    let permit = permit_for(&mut unknown_core, &evidence);
    unknown_core
        .backend_mut()
        .fail_submission_on(1, SimFault::UnknownApplied);
    let intent = save_intent(&unknown_core, &evidence, &permit, "unknown-save");
    assert_eq!(
        unknown_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: intent.clone().into()
            })
            .status,
        ReceiptStatus::Accepted
    );
    unknown_core.dispatch_next();
    assert_eq!(
        unknown_core
            .query_receipt(unknown_core.session_epoch(), "unknown-save")
            .unwrap()
            .status,
        ReceiptStatus::Unknown
    );
    assert_eq!(
        unknown_core.permit_state(&permit.id),
        Some(PermitState::Revoked)
    );
    assert_eq!(unknown_core.backend().events().len(), 1);
    assert_eq!(
        unknown_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: intent.clone().into()
            })
            .status,
        ReceiptStatus::Unknown
    );
    let mut retry = intent;
    retry.intent_id = "unknown-save-retry".into();
    retry.attempt_id = Some("attempt-unknown-save-retry".into());
    assert_eq!(
        unknown_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: retry.into()
            })
            .status,
        ReceiptStatus::Rejected
    );
    assert_eq!(unknown_core.backend().events().len(), 1);
}

#[test]
fn generic_production_profile_and_generic_proof_exclusivity_are_not_weakened() {
    let mut production_core =
        ExecutionCore::new(CoreConfig::production("owned-test"), SimBackend::new());
    production_core.advance_to(1_000).unwrap();
    let mut observation = evidence(&production_core, 1, 1, 3);
    observation.dependencies.insert("owned_scope".into(), 1);
    assert!(
        production_core
            .observe(observation.clone())
            .unwrap_err()
            .contains("disabled")
    );

    let mut scoped = core(true);
    let mut mixed = evidence(&scoped, 1, 1, 3);
    mixed.binding_proof = Some(generic_binding_proof(&mixed.target_ref));
    assert!(mixed.validate().unwrap_err().contains("mutually exclusive"));
    assert!(scoped.observe(mixed).is_err());

    observation.owned_document_proof = None;
    assert!(observation.validate().is_err());

    let mut generic_core = core(true);
    register(&mut generic_core);
    let mut generic = evidence(&generic_core, 1, 1, 3);
    generic.purpose = ObservationPurpose::TargetInput;
    generic.owned_document_proof = None;
    generic.binding_proof = Some(generic_binding_proof(&generic.target_ref));
    generic_core.observe(generic.clone()).unwrap();
    assert!(
        generic_core
            .grant_permit(&GrantPermit {
                acked_stop_seq: 0,
                task_id: "root".into(),
                target_ref: generic.target_ref,
                evidence_id: generic.observation_id,
                valid_until_ms: 1_200,
            })
            .unwrap_err()
            .reason
            .contains("required dependency")
    );
}

#[test]
fn owned_observation_must_use_the_core_clock_and_unexpired_source_proof() {
    let mut wrong_clock_core = core(true);
    let mut wrong_clock = evidence(&wrong_clock_core, 1, 1, 3);
    wrong_clock.clock_domain = "foreign/1/monotonic".into();
    wrong_clock
        .owned_document_proof
        .as_mut()
        .unwrap()
        .receiver
        .provenance
        .clock_domain = "foreign/1/monotonic".into();
    assert!(
        wrong_clock_core
            .observe(wrong_clock)
            .unwrap_err()
            .contains("clock")
    );

    let mut expired_core = core(true);
    let expired = evidence(&expired_core, 1, 1, 3);
    expired_core.advance_to(1_400).unwrap();
    assert!(expired_core.observe(expired).unwrap_err().contains("stale"));
}

#[test]
fn missing_root_lease_and_invalid_owned_dependency_cannot_issue_a_permit() {
    let mut no_root = core(true);
    let observation = evidence(&no_root, 1, 1, 3);
    no_root.observe(observation.clone()).unwrap();
    assert!(
        no_root
            .grant_permit(&GrantPermit {
                acked_stop_seq: 0,
                task_id: "missing".into(),
                target_ref: observation.target_ref.clone(),
                evidence_id: observation.observation_id.clone(),
                valid_until_ms: 1_200,
            })
            .is_err()
    );

    let mut no_lease = core(false);
    register(&mut no_lease);
    let observation = evidence(&no_lease, 1, 1, 3);
    no_lease.observe(observation.clone()).unwrap();
    assert!(
        no_lease
            .grant_permit(&GrantPermit {
                acked_stop_seq: 0,
                task_id: "root".into(),
                target_ref: observation.target_ref.clone(),
                evidence_id: observation.observation_id.clone(),
                valid_until_ms: 1_200,
            })
            .unwrap_err()
            .reason
            .contains("lease")
    );

    let mut invalid_scope = core(true);
    register(&mut invalid_scope);
    invalid_scope.update_dependency("owned_scope", Some(false));
    let observation = evidence(&invalid_scope, 1, 1, 3);
    invalid_scope.observe(observation.clone()).unwrap();
    assert!(
        invalid_scope
            .grant_permit(&GrantPermit {
                acked_stop_seq: 0,
                task_id: "root".into(),
                target_ref: observation.target_ref.clone(),
                evidence_id: observation.observation_id.clone(),
                valid_until_ms: 1_200,
            })
            .unwrap_err()
            .reason
            .contains("owned_scope")
    );
}

#[test]
fn only_the_issued_owned_proof_and_atomic_setvalue_shape_can_submit() {
    let mut core = core(true);
    register(&mut core);
    let observation = evidence(&core, 1, 1, 3);
    let permit = permit_for(&mut core, &observation);

    let mut keyboard = set_value_intent(&core, &observation, &permit, "keyboard");
    keyboard.steps[0].primitive = Primitive::KeyPress {
        key: "A".into(),
        modifiers: Default::default(),
    };
    assert_eq!(
        core.submit(SubmitIntent {
            acked_stop_seq: 0,
            intent: keyboard.into()
        })
        .status,
        ReceiptStatus::Rejected
    );

    let mut changed_proof = set_value_intent(&core, &observation, &permit, "changed-proof");
    changed_proof
        .owned_document_proof
        .as_mut()
        .unwrap()
        .environment_revision += 1;
    assert_eq!(
        core.submit(SubmitIntent {
            acked_stop_seq: 0,
            intent: changed_proof.into()
        })
        .status,
        ReceiptStatus::Rejected
    );
    assert!(core.backend().events().is_empty());
}

#[test]
fn source_identity_replacement_revokes_old_permit_and_forbids_renewal() {
    let mut core = core(true);
    register(&mut core);
    let first = evidence(&core, 5, 1, 3);
    let permit = permit_for(&mut core, &first);
    core.advance_to(1_001).unwrap();
    let replaced = evidence(&core, 6, 2, 4);
    core.observe(replaced.clone()).unwrap();
    assert_eq!(core.permit_state(&permit.id), Some(PermitState::Revoked));
    assert!(
        core.renew(RenewPermit {
            session_epoch: core.session_epoch(),
            permit,
            evidence_id: replaced.observation_id,
            valid_until_ms: 1_200,
        })
        .is_err()
    );
}

#[test]
fn same_receiver_refresh_can_advance_environment_revision_before_the_setter() {
    let mut core = core(true);
    register(&mut core);
    let issue = evidence(&core, 5, 1, 3);
    let permit = permit_for(&mut core, &issue);
    core.advance_to(1_001).unwrap();
    let current = evidence(&core, 6, 2, 3);
    core.observe(current.clone()).unwrap();
    assert_eq!(core.permit_state(&permit.id), Some(PermitState::Issued));
    core.renew(RenewPermit {
        session_epoch: core.session_epoch(),
        permit: permit.clone(),
        evidence_id: current.observation_id.clone(),
        valid_until_ms: 1_200,
    })
    .unwrap();
    let intent = set_value_intent(&core, &issue, &permit, "set-after-refresh");
    assert_eq!(
        core.submit(SubmitIntent {
            acked_stop_seq: 0,
            intent: intent.into(),
        })
        .status,
        ReceiptStatus::Accepted
    );
    core.dispatch_next();
    assert_eq!(core.backend().events().len(), 1);
}

#[test]
fn environment_revision_rollback_revokes_and_unknown_result_cannot_replay() {
    {
        let mut core = core(true);
        register(&mut core);
        let first = evidence(&core, 5, 1, 3);
        let permit = permit_for(&mut core, &first);
        core.advance_to(1_001).unwrap();
        let rollback = evidence(&core, 4, 2, 3);
        assert!(core.observe(rollback).unwrap_err().contains("rolled back"));
        assert_eq!(core.permit_state(&permit.id), Some(PermitState::Revoked));
    }

    {
        let mut core = core(true);
        register(&mut core);
        let first = evidence(&core, 5, 1, 3);
        let permit = permit_for(&mut core, &first);
        core.advance_to(1_001).unwrap();
        let rollback = evidence(&core, 6, 2, 2);
        assert!(core.observe(rollback).unwrap_err().contains("rolled back"));
        assert_eq!(core.permit_state(&permit.id), Some(PermitState::Revoked));
    }

    let mut unknown_core = core(true);
    register(&mut unknown_core);
    let observation = evidence(&unknown_core, 1, 1, 3);
    let permit = permit_for(&mut unknown_core, &observation);
    unknown_core
        .backend_mut()
        .fail_submission_on(1, SimFault::UnknownApplied);
    let intent = set_value_intent(&unknown_core, &observation, &permit, "unknown-set");
    assert_eq!(
        unknown_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: intent.clone().into()
            })
            .status,
        ReceiptStatus::Accepted
    );
    unknown_core.dispatch_next();
    let receipt = unknown_core
        .query_receipt(unknown_core.session_epoch(), "unknown-set")
        .unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Unknown);
    assert_eq!(
        unknown_core.permit_state(&permit.id),
        Some(PermitState::Revoked)
    );
    assert_eq!(unknown_core.backend().events().len(), 1);
    let replay = unknown_core.submit(SubmitIntent {
        acked_stop_seq: 0,
        intent: intent.clone().into(),
    });
    assert_eq!(replay.status, ReceiptStatus::Unknown);
    let mut second_intent = intent;
    second_intent.intent_id = "unknown-set-retry".into();
    second_intent.attempt_id = Some("attempt-unknown-set-retry".into());
    assert_eq!(
        unknown_core
            .submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: second_intent.into()
            })
            .status,
        ReceiptStatus::Rejected
    );
    assert_eq!(unknown_core.backend().events().len(), 1);
}

fn generic_binding_proof(target: &str) -> BindingProof {
    let snapshot = snapshot_with_target(
        "generic-object",
        1,
        "AXTextArea",
        None,
        "owned target",
        common::rect(20.0, 40.0, 500.0, 300.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    binding_proof(&snapshot, "generic-object", target, None, None)
}
