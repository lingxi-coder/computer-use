//! Value-contract tests only. These records do not prove a live TextEdit read,
//! owner registration, AX observer delivery, or a native setter invocation.

use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::ids::{BindingRef, SemanticTargetRef};
use interaction_contracts::desktop::ids::{ObjectRef, RequestId};
use interaction_contracts::desktop::owned_document::{
    AxPropertyObservation, DocumentBindingEvidence, DocumentBindingRead, DocumentReadStatus,
    MarkedRangeRead, MarkedTextRange, OwnedDocumentAncestor, OwnedDocumentProfile,
    OwnedDocumentReceiverEvidence, OwnedDocumentSourceProvenance, OwnedDocumentValueReadback,
    OwnedScopeObserverState,
};
use interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof;
use interaction_contracts::desktop::source::{
    KernelProcessIdentity, ObservationFact, ObserverCoverage, Rect, SourceIdentityBoundary,
    SourceState,
};
use serde_json::json;

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn evidence() -> OwnedDocumentReceiverEvidence {
    let receiver = ObjectRef::new("owned-receiver").unwrap();
    let scroll_area = ObjectRef::new("owned-scroll-area").unwrap();
    let window = ObjectRef::new("owned-window").unwrap();
    OwnedDocumentReceiverEvidence {
        value_readback: None,
        registration_id: "host-owned-registration-1".into(),
        profile: OwnedDocumentProfile::OwnedPlainTextValueSetterV1,
        provenance: OwnedDocumentSourceProvenance {
            identity_boundary: SourceIdentityBoundary {
                namespace: "d85e0f74-b6f7-4bce-8f8c-563e4b12da29".into(),
                generation: 3,
            },
            snapshot_id: RequestId::new("source-read-7").unwrap(),
            source_sequence: 7,
            observed_ms: 1_000,
            valid_until_ms: 1_250,
            clock_domain: "native/1/monotonic".into(),
        },
        process: KernelProcessIdentity {
            pid: 700,
            start_seconds: 1_700_000_000,
            start_micros: 123_456,
        },
        window_id: "window-42".into(),
        window_object_ref: window.clone(),
        window_frame: rect(0.0, 0.0, 600.0, 400.0),
        actual_object_ref: receiver.clone(),
        receiver_frame: rect(20.0, 40.0, 500.0, 300.0),
        application_focused_object_ref: receiver.clone(),
        system_focused_object_ref: receiver.clone(),
        ancestry: vec![
            OwnedDocumentAncestor {
                object_ref: scroll_area,
                role: "AXScrollArea".into(),
            },
            OwnedDocumentAncestor {
                object_ref: window,
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
                observed_ms: 1_005,
                timely: true,
                registered_path_matches: true,
            },
            after: DocumentBindingRead {
                status: DocumentReadStatus::Value,
                api_code: Some(0),
                cf_type_id: Some(7),
                observed_ms: 1_025,
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
            receiver_object_ref: receiver.clone(),
            receiver_same_cf_as_actual_object_ref: true,
            observed_ms: 1_010,
            observation: AxPropertyObservation::Value {
                value: MarkedTextRange {
                    location: 66,
                    length: 0,
                },
            },
        },
        marked_range_after: MarkedRangeRead {
            receiver_object_ref: receiver,
            receiver_same_cf_as_actual_object_ref: true,
            observed_ms: 1_020,
            observation: AxPropertyObservation::Value {
                value: MarkedTextRange {
                    location: 66,
                    length: 0,
                },
            },
        },
    }
}

fn owned_input_proof() -> OwnedDocumentBindingProof {
    OwnedDocumentBindingProof {
        binding: BindingRef::new("owned-binding").unwrap(),
        semantic_target: SemanticTargetRef::new("owned-document-value").unwrap(),
        environment_revision: 1,
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        snapshot_observed_ms: 1_000,
        receiver: Box::new(evidence()),
    }
}

#[test]
fn owned_binding_descriptor_is_fresh_scope_evidence_without_generic_fingerprint_or_contents() {
    let proof = owned_input_proof();
    assert!(proof.validate_at(1_100, 200).is_ok());
    assert!(proof.validate_at(1_200, 200).is_err());
    assert!(proof.validate_at(1_250, 1000).is_err());
    assert!(proof.validate_at(999, 1000).is_err());
    let value = serde_json::to_value(&proof).unwrap();
    assert!(value.get("fingerprint").is_none());
    assert!(value.get("permit").is_none());
    assert!(value.get("document_path").is_none());
    assert_eq!(
        format!("{proof:?}"),
        "OwnedDocumentBindingProof([private scope])"
    );
    for field in ["revision", "session", "surface", "start"] {
        let mut changed = proof.clone();
        match field {
            "revision" => changed.environment_revision = 0,
            "session" => changed.session_epoch = 0,
            "surface" => changed.surface.epoch = 0,
            "start" => changed.snapshot_observed_ms = 999,
            _ => unreachable!(),
        }
        assert!(changed.validate().is_err(), "invalid owned context {field}");
    }
}

#[test]
fn owned_binding_identity_ignores_labels_and_read_values_but_detects_receiver_replacement() {
    let proof = owned_input_proof();
    let mut next = proof.clone();
    next.environment_revision += 1;
    next.receiver.provenance.snapshot_id = RequestId::new("new-snapshot").unwrap();
    next.receiver.provenance.source_sequence += 1;
    next.receiver.title = AxPropertyObservation::Value {
        value: "changed title".into(),
    };
    next.receiver.description = AxPropertyObservation::Unsupported { code: -25205 };
    assert!(proof.same_instance(&next));
    for field in [
        "binding",
        "target",
        "session",
        "surface",
        "process",
        "registration",
        "generation",
        "window",
        "geometry",
        "clock",
    ] {
        let mut changed = next.clone();
        match field {
            "binding" => changed.binding = BindingRef::new("other-binding").unwrap(),
            "target" => changed.semantic_target = SemanticTargetRef::new("other-target").unwrap(),
            "session" => changed.session_epoch += 1,
            "surface" => changed.surface.epoch += 1,
            "process" => changed.receiver.process.start_micros += 1,
            "registration" => changed.receiver.registration_id = "other-registration".into(),
            "generation" => changed.receiver.provenance.identity_boundary.generation += 1,
            "window" => changed.receiver.window_id = "window-43".into(),
            "geometry" => changed.receiver.receiver_frame.x += 1.0,
            "clock" => changed.receiver.provenance.clock_domain = "other/clock".into(),
            _ => unreachable!(),
        }
        assert!(
            !proof.same_instance(&changed),
            "replacement {field} kept identity"
        );
    }
}

#[test]
fn owned_setter_value_policy_preserves_bounded_unicode_without_normalizing_or_truncating() {
    for value in ["", "中文\tCafe\u{301} 👩🏽‍💻\r\n", &"a".repeat(2048)] {
        assert!(OwnedDocumentBindingProof::validate_setter_value(value).is_ok());
    }
    for value in [
        "private\0text",
        "private\u{7}text",
        &"a".repeat(2049),
        &"é".repeat(1025),
    ] {
        assert!(OwnedDocumentBindingProof::validate_setter_value(value).is_err());
    }
}

fn owned_observation() -> interaction_contracts::desktop::device::Observation {
    use interaction_contracts::desktop::device::{
        Observation, ObservationPurpose, SourceFreshness,
    };
    let proof = owned_input_proof();
    Observation {
        observation_id: "owned-evidence".into(),
        session_epoch: 1,
        surface: proof.surface.clone(),
        target_ref: proof.binding.as_str().into(),
        clock_domain: proof.receiver.provenance.clock_domain.clone(),
        source_sequence: 7,
        observed_ms: 1_000,
        valid_until_ms: 1_200,
        freshness: SourceFreshness::Verified,
        execution_domain: ExecutionDomain::ForegroundNative,
        purpose: ObservationPurpose::OwnedDocumentValueSetter,
        binding_proof: None,
        owned_document_proof: Some(Box::new(proof)),
        dependencies: ["authorization", "focus", "window", "owned_scope"]
            .into_iter()
            .map(|name| (name.to_string(), 1))
            .collect(),
    }
}

fn owned_intent() -> interaction_contracts::desktop::device::ActionIntent {
    use interaction_contracts::desktop::device::{
        ActionIntent, CoordinateSpace, PermitRef, ScheduledPrimitive,
    };
    use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
    let observation = owned_observation();
    ActionIntent {
        intent_id: "owned-intent".into(),
        attempt_id: Some("activated-attempt".into()),
        task_id: "frozen-root".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: 1,
        execution_domain: ExecutionDomain::ForegroundNative,
        permit: PermitRef {
            id: "owned-permit".into(),
            generation: 1,
        },
        surface: observation.surface,
        target_ref: observation.target_ref,
        binding_fingerprint: None,
        binding_proof: None,
        owned_document_proof: observation.owned_document_proof,
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: observation.dependencies,
        evidence_id: observation.observation_id,
        latest_start_ms: 1_100,
        duration_ms: 100,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: Primitive::AxAction {
                action: AxActionKind::SetValue {
                    value: "中文 👩🏽‍💻".into(),
                },
            },
        }],
    }
}

#[test]
fn owned_observation_requires_exact_proof_purpose_and_versioned_dependency_scope() {
    use interaction_contracts::desktop::device::{ObservationPurpose, SourceFreshness};
    let original = owned_observation();
    assert!(original.validate().is_ok());
    for field in [
        "purpose",
        "proof",
        "freshness",
        "target",
        "session",
        "surface",
        "sequence",
        "clock",
        "start",
        "expiry",
        "deps",
    ] {
        let mut changed = original.clone();
        match field {
            "purpose" => changed.purpose = ObservationPurpose::TargetInput,
            "proof" => changed.owned_document_proof = None,
            "freshness" => changed.freshness = SourceFreshness::Uncertain,
            "target" => changed.target_ref = "other-binding".into(),
            "session" => changed.session_epoch = 2,
            "surface" => changed.surface.epoch = 2,
            "sequence" => changed.source_sequence = 8,
            "clock" => changed.clock_domain = "other/clock".into(),
            "start" => changed.observed_ms += 1,
            "expiry" => changed.valid_until_ms = 1_251,
            "deps" => {
                changed.dependencies.remove("owned_scope");
            }
            _ => unreachable!(),
        }
        assert!(
            changed.validate().is_err(),
            "invalid owned observation {field}"
        );
    }
}

#[test]
fn owned_intent_requires_one_bounded_setter_and_owning_attempt() {
    use interaction_contracts::desktop::device::CoordinateSpace;
    use interaction_contracts::desktop::primitive::{AxActionKind, Modifiers, Primitive};
    let original = owned_intent();
    assert!(original.validate().is_ok());
    for field in [
        "attempt",
        "fingerprint",
        "coordinate",
        "target",
        "session",
        "surface",
        "expiry",
        "duration",
        "offset",
        "multiple",
        "key",
        "press",
        "value",
        "deps",
    ] {
        let mut changed = original.clone();
        match field {
            "attempt" => changed.attempt_id = None,
            "fingerprint" => changed.binding_fingerprint = Some("fake-generic-fp".into()),
            "coordinate" => changed.coordinate_space = CoordinateSpace::SurfacePx,
            "target" => changed.target_ref = "other-binding".into(),
            "session" => changed.session_epoch += 1,
            "surface" => changed.surface.epoch += 1,
            "expiry" => changed.latest_start_ms = 1_151,
            "duration" => changed.duration_ms = 1_501,
            "offset" => changed.steps[0].offset_ms = 1,
            "multiple" => {
                changed.steps.push(changed.steps[0].clone());
            }
            "key" => {
                changed.steps[0].primitive = Primitive::KeyPress {
                    key: "Return".into(),
                    modifiers: Modifiers::default(),
                }
            }
            "press" => {
                changed.steps[0].primitive = Primitive::AxAction {
                    action: AxActionKind::Press,
                }
            }
            "value" => {
                changed.steps[0].primitive = Primitive::AxAction {
                    action: AxActionKind::SetValue {
                        value: "a".repeat(2049),
                    },
                }
            }
            "deps" => {
                changed.dependencies.insert("layer".into(), 1);
            }
            _ => unreachable!(),
        }
        assert!(changed.validate().is_err(), "invalid owned intent {field}");
    }
}

#[test]
fn owned_save_requires_its_purpose_proof_and_bounded_expected_value() {
    use interaction_contracts::desktop::device::ObservationPurpose;
    use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};

    let mut observation = owned_observation();
    observation.purpose = ObservationPurpose::OwnedDocumentSave;
    assert!(observation.validate().is_ok());

    let mut missing_proof = observation.clone();
    missing_proof.owned_document_proof = None;
    assert!(missing_proof.validate().is_err());

    let mut save = owned_intent();
    save.steps[0].primitive = Primitive::AxAction {
        action: AxActionKind::SaveOwnedDocument {
            expected_value: "saved text\n".into(),
        },
    };
    assert!(save.validate().is_ok());

    for expected_value in ["contains\0nul".to_owned(), "x".repeat(2049)] {
        let mut invalid = save.clone();
        invalid.steps[0].primitive = Primitive::AxAction {
            action: AxActionKind::SaveOwnedDocument { expected_value },
        };
        assert!(invalid.validate().is_err());
    }

    let mut no_proof = save;
    no_proof.owned_document_proof = None;
    assert!(no_proof.validate().is_err());
}

#[test]
fn owned_proof_cannot_be_mixed_with_a_complete_generic_proof() {
    use interaction_contracts::desktop::{message::Message, samples};
    // The contracts sample carries explicit typed fixture facts; it does not
    // claim to represent a live native AX observation.
    let mut generic = samples::all()
        .into_iter()
        .find_map(|(_, envelope)| match envelope.message {
            Message::Observation(observation) => {
                observation.native().unwrap().binding_proof.clone()
            }
            _ => None,
        })
        .expect("the contracts sample includes a complete synthetic generic binding proof");
    generic.binding = BindingRef::new("owned-binding").unwrap();
    assert!(generic.validate().is_ok());
    let mut observation = owned_observation();
    observation.binding_proof = Some(generic.clone());
    assert!(observation.validate().is_err());
    let mut intent = owned_intent();
    intent.binding_proof = Some(generic.clone());
    assert!(intent.validate().is_err());

    let mut generic_save = owned_intent();
    generic_save.owned_document_proof = None;
    generic_save.binding_proof = Some(generic.clone());
    generic_save.binding_fingerprint = Some(generic.fingerprint.digest());
    generic_save.steps[0].primitive =
        interaction_contracts::desktop::primitive::Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::SaveOwnedDocument {
                expected_value: "ordinary binding cannot Save".into(),
            },
        };
    assert!(generic_save.validate().is_err());
}

#[test]
fn owned_proof_registration_match_uses_private_owner_and_kernel_identity() {
    use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
    let mut proof = owned_input_proof();
    proof.receiver.registration_id = "123e4567-e89b-42d3-a456-426614174000".into();
    let registration: OwnedDocumentRegistration = serde_json::from_value(json!({
        "version":1,"registration_id":proof.receiver.registration_id,
        "pid":700,"start_seconds":1_700_000_000u64,"start_micros":123_456,
        "executable_path":"/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
        "bundle_id":"com.apple.TextEdit","document_path":"/private/tmp/owned/document.txt",
        "document_sha256":"a".repeat(64),"document_dev":1,"document_ino":2,
        "directory_dev":1,"directory_ino":3,"issued_unix_ms":1_700_000_000_000u64
    }))
    .unwrap();
    assert!(proof.validate_registration(&registration).is_ok());
    let mut other = registration.clone();
    other.start_micros += 1;
    assert!(proof.validate_registration(&other).is_err());
    other = registration;
    other.registration_id = "123e4567-e89b-42d3-a456-426614174001".into();
    assert!(proof.validate_registration(&other).is_err());
}

#[test]
fn source_snapshot_rejects_uncorrelated_or_invalid_owned_evidence() {
    use interaction_contracts::desktop::{message::Message, samples};
    let proof = evidence();
    let mut snapshot = samples::all()
        .into_iter()
        .find_map(|(_, envelope)| match envelope.message {
            Message::StructureSnapshot(snapshot) => Some(snapshot),
            _ => None,
        })
        .unwrap();
    snapshot.app.bundle_id = "com.apple.TextEdit".into();
    snapshot.app.pid = proof.process.pid;
    snapshot.snapshot_id = proof.provenance.snapshot_id.clone();
    snapshot.source_sequence = proof.provenance.source_sequence;
    snapshot.clock_domain = proof.provenance.clock_domain.clone();
    snapshot.observed_ms = 1_030;
    snapshot.complete = false;
    snapshot.incomplete_reason = Some("generic scope unobserved".into());
    snapshot.objects.clear();
    snapshot.observer_coverage = ObserverCoverage {
        state: SourceState::Unavailable,
        covered_objects: 0,
        read_scoped_objects: 0,
        failed_registrations: 0,
        failures: vec![],
    };
    snapshot.windows.clear();
    snapshot.global_focus.window_id = ObservationFact::Absent;
    snapshot.owned_document_evidence = Some(Box::new(proof));
    assert!(snapshot.validate().is_ok());
    for field in [
        "app", "pid", "snapshot", "sequence", "clock", "time", "proof",
    ] {
        let mut changed = snapshot.clone();
        match field {
            "app" => changed.app.bundle_id = "foreign.app".into(),
            "pid" => changed.app.pid += 1,
            "snapshot" => changed.snapshot_id = RequestId::new("other-read").unwrap(),
            "sequence" => changed.source_sequence += 1,
            "clock" => changed.clock_domain = "other/clock".into(),
            "time" => changed.observed_ms = 1_250,
            "proof" => {
                changed
                    .owned_document_evidence
                    .as_mut()
                    .unwrap()
                    .document_binding_verified = false
            }
            _ => unreachable!(),
        }
        assert!(
            changed.validate().is_err(),
            "uncorrelated {field} evidence admitted"
        );
    }
}

#[test]
fn owned_profile_uses_explicit_property_states_and_has_an_independent_schema() {
    let proof = evidence();
    assert!(proof.validate().is_ok());
    assert!(proof.validate_at(1_100).is_ok());
    assert!(proof.eligible_at(1_100));

    let value = serde_json::to_value(&proof).unwrap();
    assert_eq!(value["profile"], "owned_plain_text_value_setter_v1");
    assert_eq!(
        value["enabled"],
        json!({ "status": "unsupported", "code": -25205 })
    );
    assert_eq!(
        value["description"],
        json!({ "status": "failure", "code": -25200 })
    );
    assert_eq!(
        value["marked_range_before"]["observation"]["value"]["length"],
        0
    );
    assert_eq!(
        value["marked_range_after"]["observation"]["value"]["location"],
        66
    );
    assert!(
        value.get("value").is_none(),
        "AXValue contents must not be serialized"
    );
    assert!(
        value.get("text").is_none(),
        "AX text contents must not be serialized"
    );
    assert!(
        value.get("title").is_some(),
        "title is retained as typed evidence only"
    );

    let schema =
        serde_json::to_value(schemars::schema_for!(OwnedDocumentReceiverEvidence)).unwrap();
    assert_eq!(schema["type"], "object");
    for field in [
        "registration_id",
        "profile",
        "provenance",
        "process",
        "window_id",
        "window_frame",
        "actual_object_ref",
        "ancestry",
        "scope_complete",
        "profile_eligible",
        "marked_range_stable_unmarked",
        "observer_state",
        "role",
        "enabled",
        "focused",
        "value_settable",
        "description",
        "marked_range_before",
        "marked_range_after",
        "document_binding_verified",
        "document_binding",
    ] {
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == field),
            "schema must require {field}"
        );
    }
    let decoded: OwnedDocumentReceiverEvidence = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded, proof);
}

#[test]
fn value_readback_retains_exact_utf8_digest_without_contents() {
    use sha2::{Digest, Sha256};
    let text = "中文\tCafe\u{301} 👩🏽‍💻\r\n";
    let mut proof = evidence();
    proof.value_readback = Some(OwnedDocumentValueReadback {
        receiver_object_ref: proof.actual_object_ref.clone(),
        receiver_same_cf_as_actual_object_ref: true,
        observed_ms: 1_030,
        sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        utf8_byte_count: text.len() as u32,
    });
    assert!(proof.validate_at(1_100).is_ok());
    let serialized = serde_json::to_string(&proof).unwrap();
    assert!(!serialized.contains(text));
    assert!(!serialized.contains("中文"));
    let roundtrip: OwnedDocumentReceiverEvidence = serde_json::from_str(&serialized).unwrap();
    assert_eq!(roundtrip.value_readback, proof.value_readback);
    // An unreadable value is distinct from an actual empty UI value.
    proof.value_readback = None;
    assert!(proof.validate_at(1_100).is_ok());
    proof.value_readback = Some(OwnedDocumentValueReadback {
        receiver_object_ref: proof.actual_object_ref.clone(),
        receiver_same_cf_as_actual_object_ref: true,
        observed_ms: 1_030,
        sha256: format!("{:x}", Sha256::digest([])),
        utf8_byte_count: 0,
    });
    assert!(proof.validate_at(1_100).is_ok());
}

#[test]
fn value_readback_rejects_wrong_receiver_future_and_invalid_digest_bounds() {
    let mut proof = evidence();
    proof.value_readback = Some(OwnedDocumentValueReadback {
        receiver_object_ref: proof.actual_object_ref.clone(),
        receiver_same_cf_as_actual_object_ref: true,
        observed_ms: 1_030,
        sha256: "a".repeat(64),
        utf8_byte_count: 2048,
    });
    assert!(proof.validate_at(1_100).is_ok());
    assert!(proof.validate_at(1_029).is_err());
    for field in [
        "receiver",
        "same_cf",
        "early",
        "expiry",
        "hash_length",
        "hash_case",
        "hash_char",
        "bytes",
    ] {
        let mut changed = proof.clone();
        let value = changed.value_readback.as_mut().unwrap();
        match field {
            "receiver" => value.receiver_object_ref = ObjectRef::new("foreign").unwrap(),
            "same_cf" => value.receiver_same_cf_as_actual_object_ref = false,
            "early" => value.observed_ms = 999,
            "expiry" => value.observed_ms = 1_250,
            "hash_length" => value.sha256.pop().map(|_| ()).unwrap(),
            "hash_case" => value.sha256 = "A".repeat(64),
            "hash_char" => value.sha256 = "g".repeat(64),
            "bytes" => value.utf8_byte_count = 2049,
            _ => unreachable!(),
        }
        assert!(
            changed.validate().is_err(),
            "invalid readback {field} admitted"
        );
    }
}

#[test]
fn document_binding_requires_actual_timely_matching_reads() {
    let mut proof = evidence();
    proof.document_binding_verified = false;
    assert!(proof.validate().is_err());
    for mutate in [
        |proof: &mut OwnedDocumentReceiverEvidence| {
            proof.document_binding.after.registered_path_matches = false
        },
        |proof: &mut OwnedDocumentReceiverEvidence| proof.document_binding.before.timely = false,
        |proof: &mut OwnedDocumentReceiverEvidence| {
            proof.document_binding.before.api_code = Some(-25205)
        },
        |proof: &mut OwnedDocumentReceiverEvidence| proof.document_binding.before.cf_type_id = None,
        |proof: &mut OwnedDocumentReceiverEvidence| proof.document_binding.after.observed_ms = 999,
        |proof: &mut OwnedDocumentReceiverEvidence| {
            proof.document_binding.after.observed_ms = 1_250
        },
    ] {
        let mut proof = evidence();
        mutate(&mut proof);
        assert!(proof.validate().is_err());
    }
    let mut proof = evidence();
    proof.document_binding.after.observed_ms = 1_200;
    assert!(proof.validate().is_ok());
    assert!(proof.validate_at(1_100).is_err());
}

#[test]
fn ax_observation_statuses_keep_raw_codes_and_reject_relabeling() {
    assert_eq!(
        serde_json::to_value(AxPropertyObservation::<bool>::Unsupported { code: -25205 }).unwrap(),
        json!({ "status": "unsupported", "code": -25205 })
    );
    assert!(
        serde_json::from_value::<AxPropertyObservation<bool>>(json!({"status":"unknown"})).is_err()
    );
    assert!(
        AxPropertyObservation::<bool>::Unsupported { code: -25205 }
            .validate()
            .is_ok()
    );
    assert!(
        AxPropertyObservation::<bool>::Unsupported { code: -25212 }
            .validate()
            .is_err()
    );
    assert!(
        AxPropertyObservation::<bool>::NoValue { code: -25212 }
            .validate()
            .is_ok()
    );
    assert!(
        AxPropertyObservation::<bool>::NoValue { code: -25205 }
            .validate()
            .is_err()
    );
    assert!(
        AxPropertyObservation::<bool>::Failure { code: -25200 }
            .validate()
            .is_ok()
    );
    assert!(
        AxPropertyObservation::<bool>::Failure { code: 0 }
            .validate()
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(AxPropertyObservation::<bool>::DecodeFailure {
            api_code: 0,
            actual_cf_type_id: Some(42),
        })
        .unwrap(),
        json!({
            "status": "decode_failure",
            "api_code": 0,
            "actual_cf_type_id": 42
        })
    );
    assert!(
        serde_json::from_value::<AxPropertyObservation<bool>>(json!({
            "status": "decode_failure",
            "api_code": 0,
            "actual_cf_type_id": null
        }))
        .unwrap()
        .validate()
        .is_ok()
    );
    assert!(
        AxPropertyObservation::<bool>::DecodeFailure {
            api_code: -25200,
            actual_cf_type_id: Some(42),
        }
        .validate()
        .is_err()
    );
    assert!(
        AxPropertyObservation::<bool>::NotAttempted {
            reason: String::new()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn missing_unknown_secure_marked_stale_or_unproven_facts_fail_closed() {
    let baseline = evidence();
    let encoded = serde_json::to_value(&baseline).unwrap();
    for required in [
        "registration_id",
        "profile",
        "provenance",
        "process",
        "window_id",
        "window_object_ref",
        "window_frame",
        "actual_object_ref",
        "receiver_frame",
        "application_focused_object_ref",
        "system_focused_object_ref",
        "ancestry",
        "scope_complete",
        "profile_eligible",
        "marked_range_stable_unmarked",
        "observer_state",
        "secure",
        "title",
        "role",
        "subrole",
        "enabled",
        "focused",
        "value_settable",
        "description",
        "marked_range_before",
        "marked_range_after",
    ] {
        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove(required);
        assert!(
            serde_json::from_value::<OwnedDocumentReceiverEvidence>(missing).is_err(),
            "missing {required} must not deserialize"
        );
    }

    let mut proof = baseline.clone();
    proof.secure = true;
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.scope_complete = false;
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.profile_eligible = false;
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.marked_range_stable_unmarked = false;
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.observer_state = OwnedScopeObserverState::CoverageIncomplete;
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.system_focused_object_ref = ObjectRef::new("other-receiver").unwrap();
    assert!(proof.validate().is_err());

    let mut proof = baseline.clone();
    proof.role = AxPropertyObservation::Value {
        value: "AXSecureTextField".into(),
    };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.role = AxPropertyObservation::NoValue { code: -25212 };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.role = AxPropertyObservation::DecodeFailure {
        api_code: 0,
        actual_cf_type_id: Some(1),
    };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.subrole = AxPropertyObservation::NoValue { code: -25212 };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.subrole = AxPropertyObservation::Failure { code: -25200 };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.subrole = AxPropertyObservation::Value {
        value: "AXTextAreaSubrole".into(),
    };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.enabled = AxPropertyObservation::Value { value: false };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.enabled = AxPropertyObservation::NoValue { code: -25212 };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.enabled = AxPropertyObservation::DecodeFailure {
        api_code: 0,
        actual_cf_type_id: None,
    };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.focused = AxPropertyObservation::Value { value: false };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.value_settable = AxPropertyObservation::Value { value: false };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.marked_range_after.observation = AxPropertyObservation::Value {
        value: MarkedTextRange {
            location: 66,
            length: 1,
        },
    };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.marked_range_after.observation = AxPropertyObservation::NotAttempted {
        reason: "unavailable".into(),
    };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof
        .marked_range_after
        .receiver_same_cf_as_actual_object_ref = false;
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.marked_range_after.receiver_object_ref = ObjectRef::new("other-receiver").unwrap();
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.marked_range_after.observation = AxPropertyObservation::Value {
        value: MarkedTextRange {
            location: 67,
            length: 0,
        },
    };
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.marked_range_after.observed_ms = 1_009;
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.marked_range_before.observed_ms = proof.provenance.observed_ms - 1;
    assert!(proof.validate().is_err());
    let mut proof = baseline.clone();
    proof.marked_range_after.observed_ms = proof.provenance.valid_until_ms;
    assert!(proof.validate().is_err());
    assert!(baseline.validate_at(1_019).is_err());
    assert!(baseline.validate_at(1_024).is_err());
    assert!(baseline.validate_at(1_025).is_ok());

    assert!(
        baseline.validate_at(999).is_err(),
        "future provenance is rejected"
    );
    assert!(
        baseline.validate_at(1_250).is_err(),
        "expired provenance is rejected"
    );
    let mut proof = baseline;
    proof.provenance.identity_boundary.namespace = "D85E0F74-B6F7-4BCE-8F8C-563E4B12DA29".into();
    assert!(
        proof.validate().is_err(),
        "noncanonical Source namespace is rejected"
    );
}

#[test]
fn title_and_description_values_do_not_participate_in_receiver_eligibility() {
    let mut proof = evidence();
    proof.title = AxPropertyObservation::Value {
        value: "Ambiguous visible title".into(),
    };
    proof.subrole = AxPropertyObservation::Value {
        value: String::new(),
    };
    proof.description = AxPropertyObservation::Failure { code: -25200 };
    assert!(proof.validate_at(1_100).is_ok());
    proof.description = AxPropertyObservation::DecodeFailure {
        api_code: 0,
        actual_cf_type_id: Some(86),
    };
    assert!(proof.validate_at(1_100).is_ok());
    proof.description = AxPropertyObservation::Value {
        value: "not an identity key".into(),
    };
    assert!(proof.validate_at(1_100).is_ok());
}
