//! Dedicated request and receipt for Main-authorized owned-text SetValue and Save.
//! This packet intentionally has no generic binding fingerprint or hit-test
//! coordinates: the native helper rebinds the owned proof on the AX worker.

use std::collections::{BTreeMap, BTreeSet};

use execution_core::backend::SubmitContext;
use interaction_contracts::desktop::device::{
    ObservationPurpose, PermitRef, SourceFreshness, SurfaceRef,
};
use interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof;
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
use serde::{Deserialize, Serialize};

use super::{OWNED_VALUE_PROOF_LIMIT, OWNED_VALUE_REQUEST_LIMIT};

const OWNED_DEPENDENCIES: [&str; 4] = ["authorization", "focus", "window", "owned_scope"];
const OWNED_MAX_AGE_MS: u64 = 2_000;

#[derive(Clone, Copy)]
pub(super) struct OwnedValueBuildOptions<'a> {
    pub registration: &'a OwnedDocumentRegistration,
    pub allow_save: bool,
}

pub(crate) fn owned_action_value_is_valid(action: &str, value: &str) -> bool {
    match action {
        "ax_set_owned_document_value" => {
            OwnedDocumentBindingProof::validate_setter_value(value).is_ok()
        }
        "ax_save_document" => value.len() <= 2_048 && !value.contains('\0'),
        _ => false,
    }
}

pub(super) fn dependency_update_command(
    epoch: u64,
    name: &str,
    revision: u64,
    valid: Option<bool>,
) -> Option<serde_json::Value> {
    if epoch == 0 || revision == 0 || !OWNED_DEPENDENCIES.contains(&name) {
        return None;
    }
    Some(serde_json::json!({
        "protocol_version": 1,
        "session_epoch": epoch,
        "command": "dependency_update",
        "name": name,
        "revision": revision,
        "valid": valid,
    }))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeOwnedValueRequest {
    pub kind: String,
    pub request_id: String,
    pub intent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    pub task_id: String,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub permit: PermitRef,
    pub stop_seq: u64,
    pub guard_revision: u64,
    pub clock_domain: String,
    pub surface: SurfaceRef,
    pub evidence_id: String,
    pub intent_evidence_id: String,
    pub source_sequence: u64,
    pub observed_ms: u64,
    pub valid_until_ms: u64,
    pub command_end_ms: u64,
    pub started_ms: u64,
    pub duration_ms: u64,
    pub latest_start_ms: u64,
    pub original_proof: OwnedDocumentBindingProof,
    pub current_proof: OwnedDocumentBindingProof,
    pub dependencies: BTreeMap<String, u64>,
    pub action: String,
    pub value: String,
}

impl NativeOwnedValueRequest {
    pub(super) fn build(
        request_id: String,
        context: &SubmitContext<'_>,
        primitive: &Primitive,
        guard_revision: u64,
        now_ms: u64,
        epoch: u64,
        options: OwnedValueBuildOptions<'_>,
    ) -> Result<Self, String> {
        let registration = options.registration;
        let allow_save = options.allow_save;
        let intent = context.intent;
        let authority = &context.authority;
        let evidence = &authority.evidence;
        let (value, action, required_purpose) = match primitive {
            Primitive::AxAction {
                action: AxActionKind::SetValue { value },
            } => (
                value,
                "ax_set_owned_document_value",
                ObservationPurpose::OwnedDocumentValueSetter,
            ),
            Primitive::AxAction {
                action: AxActionKind::SaveOwnedDocument { expected_value },
            } if allow_save => (
                expected_value,
                "ax_save_document",
                ObservationPurpose::OwnedDocumentSave,
            ),
            Primitive::AxAction {
                action: AxActionKind::SaveOwnedDocument { .. },
            } => return Err("owned Save requires the separate startup grant".into()),
            _ => return Err("owned value mode accepts only its fixed AX actions".into()),
        };

        registration.validate_identity()?;
        intent.validate()?;
        evidence.validate()?;
        if !owned_action_value_is_valid(action, value) {
            return Err("owned action value is outside its fixed text policy".into());
        }

        let original = intent
            .owned_document_proof
            .as_deref()
            .ok_or("missing original owned document proof")?;
        let current = evidence
            .owned_document_proof
            .as_deref()
            .ok_or("missing current owned document proof")?;
        let expected_clock = format!("macos/{epoch}/wall");
        let started_ms = authority
            .command_end_ms
            .checked_sub(intent.duration_ms)
            .ok_or("owned value command duration underflow")?;
        let exact_dependencies = intent
            .dependencies
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == BTreeSet::from(OWNED_DEPENDENCIES);

        original.validate_registration(registration)?;
        current.validate_registration(registration)?;
        if !original.same_instance(current)
            || current.environment_revision < original.environment_revision
            || intent.binding_proof.is_some()
            || evidence.binding_proof.is_some()
            || evidence.purpose != required_purpose
            || evidence.freshness != SourceFreshness::Verified
            || intent.session_epoch != epoch
            || evidence.session_epoch != epoch
            || intent.surface != evidence.surface
            || intent.target_ref != evidence.target_ref
            || authority.permit != intent.permit
            || intent.dependencies != evidence.dependencies
            || !exact_dependencies
            || intent.dependencies.values().any(|revision| *revision == 0)
            || evidence.clock_domain != expected_clock
            || original.surface != intent.surface
            || current.surface != evidence.surface
            || original.binding.as_str() != intent.target_ref
            || current.binding.as_str() != evidence.target_ref
            || original.session_epoch != epoch
            || current.session_epoch != epoch
            || original.receiver.process != registration.process_identity()
            || current.receiver.process != registration.process_identity()
            || original.receiver.provenance.clock_domain != expected_clock
            || current.receiver.provenance.clock_domain != expected_clock
            || current.snapshot_observed_ms != evidence.observed_ms
            || current.receiver.provenance.source_sequence != evidence.source_sequence
            || current.receiver.provenance.observed_ms > now_ms
            || evidence.observed_ms > now_ms
            || original.validate_at(now_ms, OWNED_MAX_AGE_MS).is_err()
            || current.validate_at(now_ms, OWNED_MAX_AGE_MS).is_err()
            || evidence.valid_until_ms <= now_ms
            || authority.valid_until_ms <= now_ms
            || authority.valid_until_ms > evidence.valid_until_ms
            || authority.valid_until_ms > current.receiver.provenance.valid_until_ms
            || authority.valid_until_ms > authority.command_end_ms
            || authority.valid_until_ms.saturating_sub(now_ms) > 1_500
            || started_ms > now_ms
            || started_ms > intent.latest_start_ms
            || intent.duration_ms == 0
            || intent.duration_ms > 1_500
            || intent.steps.len() != 1
            || intent.steps[0].offset_ms != 0
            || &intent.steps[0].primitive != primitive
            || request_id.is_empty()
            || request_id.len() > 128
        {
            return Err(
                "owned value request context, proof, dependency or deadline mismatch".into(),
            );
        }

        Ok(Self {
            kind: "owned_value_submit".into(),
            request_id,
            intent_id: intent.intent_id.clone(),
            attempt_id: intent.attempt_id.clone(),
            task_id: intent.task_id.clone(),
            task_revision: intent.task_revision,
            root_epoch: intent.root_epoch,
            permit: authority.permit.clone(),
            stop_seq: authority.stop_seq,
            guard_revision,
            clock_domain: evidence.clock_domain.clone(),
            surface: evidence.surface.clone(),
            evidence_id: evidence.observation_id.clone(),
            intent_evidence_id: intent.evidence_id.clone(),
            source_sequence: evidence.source_sequence,
            observed_ms: evidence.observed_ms,
            valid_until_ms: authority.valid_until_ms,
            command_end_ms: authority.command_end_ms,
            started_ms,
            duration_ms: intent.duration_ms,
            latest_start_ms: intent.latest_start_ms,
            original_proof: original.clone(),
            current_proof: current.clone(),
            dependencies: intent.dependencies.clone(),
            action: action.into(),
            value: value.clone(),
        })
    }
}

pub(super) fn request_wire_within_bounds(request: &NativeOwnedValueRequest, epoch: u64) -> bool {
    if !owned_action_value_is_valid(&request.action, &request.value) {
        return false;
    }
    let Some(original_proof_size) = serde_json::to_vec(&request.original_proof)
        .ok()
        .map(|bytes| bytes.len())
    else {
        return false;
    };
    let Some(current_proof_size) = serde_json::to_vec(&request.current_proof)
        .ok()
        .map(|bytes| bytes.len())
    else {
        return false;
    };
    let Some(request_wire_size) = serde_json::to_vec(&serde_json::json!({
        "protocol_version": 1,
        "session_epoch": epoch,
        "command": "owned_value_submit",
        "owned_value": request,
    }))
    .ok()
    .map(|bytes| bytes.len().saturating_add(1)) else {
        return false;
    };
    original_proof_size <= OWNED_VALUE_PROOF_LIMIT
        && current_proof_size <= OWNED_VALUE_PROOF_LIMIT
        && request_wire_size <= OWNED_VALUE_REQUEST_LIMIT as usize
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeOwnedValueResponse {
    pub protocol_version: u32,
    pub kind: String,
    pub session_epoch: u64,
    pub request_id: String,
    pub outcome: String,
    pub reason: String,
    pub submitted: bool,
    pub committed_ms: Option<u64>,
    pub returned_ms: Option<u64>,
}

impl NativeOwnedValueResponse {
    pub fn valid_for(&self, request: &NativeOwnedValueRequest, epoch: u64) -> bool {
        self.valid_for_window(
            &request.request_id,
            request.started_ms,
            request.valid_until_ms,
            epoch,
        )
    }

    fn valid_for_window(
        &self,
        request_id: &str,
        started_ms: u64,
        valid_until_ms: u64,
        epoch: u64,
    ) -> bool {
        self.protocol_version == 1
            && self.kind == "owned_value_submit"
            && self.session_epoch == epoch
            && self.request_id == request_id
            && self.reason.len() <= 256
            && if self.submitted {
                let commit_valid = self
                    .committed_ms
                    .is_some_and(|at| at >= started_ms && at < valid_until_ms);
                let returned_valid = match self.outcome.as_str() {
                    "applied" => self.returned_ms.is_some_and(|end| {
                        self.committed_ms.is_some_and(|at| {
                            end >= at && end <= valid_until_ms.saturating_add(500)
                        })
                    }),
                    "unknown" => self.returned_ms.is_none_or(|end| {
                        self.committed_ms.is_some_and(|at| {
                            end >= at && end <= valid_until_ms.saturating_add(500)
                        })
                    }),
                    _ => false,
                };
                commit_valid && returned_valid
            } else {
                matches!(
                    self.outcome.as_str(),
                    "rejected" | "expired" | "revoked" | "stop_latched" | "context_unresolved"
                ) && self.committed_ms.is_none()
                    && self.returned_ms.is_none()
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(
        outcome: &str,
        submitted: bool,
        committed: Option<u64>,
        returned: Option<u64>,
    ) -> NativeOwnedValueResponse {
        NativeOwnedValueResponse {
            protocol_version: 1,
            kind: "owned_value_submit".into(),
            session_epoch: 9,
            request_id: "owned-submit-1".into(),
            outcome: outcome.into(),
            reason: "bounded receipt".into(),
            submitted,
            committed_ms: committed,
            returned_ms: returned,
        }
    }

    #[test]
    fn owned_receipts_keep_precommit_rejection_and_entered_action_unknown_distinct() {
        let rejected = response("stop_latched", false, None, None);
        assert!(rejected.valid_for_window("owned-submit-1", 100, 200, 9));

        let applied = response("applied", true, Some(150), Some(151));
        assert!(applied.valid_for_window("owned-submit-1", 100, 200, 9));

        let unknown_without_return_time = response("unknown", true, Some(150), None);
        assert!(unknown_without_return_time.valid_for_window("owned-submit-1", 100, 200, 9));

        for invalid in [
            response("rejected", true, Some(150), Some(151)),
            response("applied", true, Some(200), Some(201)),
            response("applied", true, Some(150), None),
            response("applied", true, Some(150), Some(149)),
            response("applied", true, Some(150), Some(701)),
            response("unknown", true, Some(150), Some(149)),
            response("unknown", true, Some(150), Some(701)),
            response("stop_latched", false, Some(150), Some(151)),
        ] {
            assert!(
                !invalid.valid_for_window("owned-submit-1", 100, 200, 9),
                "receipt must not blur whether the owned AX action call was entered"
            );
        }
        assert!(!applied.valid_for_window("another-request", 100, 200, 9));
        assert!(!applied.valid_for_window("owned-submit-1", 100, 200, 10));
    }
}

#[cfg(test)]
mod owned_request_wire_tests {
    use super::*;
    use execution_core::backend::{SubmissionAuthority, SubmitContext};
    use interaction_contracts::desktop::device::{
        ActionIntent, CoordinateSpace, Observation, ObservationPurpose, PermitRef,
        ScheduledPrimitive, SourceFreshness, SurfaceRef,
    };
    use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
    use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    const REGISTRATION_ID: &str = "123e4567-e89b-42d3-a456-426614174000";
    const CLOCK: &str = "macos/7/wall";
    const NOW: u64 = 1_100;

    fn registration() -> OwnedDocumentRegistration {
        OwnedDocumentRegistration {
            version: 1,
            registration_id: REGISTRATION_ID.into(),
            pid: 700,
            start_seconds: 1_700_000_000,
            start_micros: 123_456,
            executable_path: "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit".into(),
            bundle_id: "com.apple.TextEdit".into(),
            document_path: "/private/tmp/device-owned-test/document.txt".into(),
            document_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
            document_dev: 1,
            document_ino: 2,
            directory_dev: 1,
            directory_ino: 3,
            issued_unix_ms: 1_000,
        }
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Value {
        json!({ "x": x, "y": y, "width": width, "height": height })
    }

    fn proof_value(
        revision: u64,
        snapshot_ms: u64,
        sequence: u64,
        observed_ms: u64,
        valid_until_ms: u64,
    ) -> Value {
        let receiver = "owned-receiver";
        let provenance = json!({
            "identity_boundary": {
                "namespace": "d85e0f74-b6f7-4bce-8f8c-563e4b12da29",
                "generation": 3,
            },
            "snapshot_id": format!("source-read-{sequence}"),
            "source_sequence": sequence,
            "observed_ms": observed_ms,
            "valid_until_ms": valid_until_ms,
            "clock_domain": CLOCK,
        });
        let marked = |stamp| {
            json!({
                "receiver_object_ref": receiver,
                "receiver_same_cf_as_actual_object_ref": true,
                "observed_ms": stamp,
                "observation": { "status": "value", "value": { "location": 0, "length": 0 } },
            })
        };
        let read = |stamp| {
            json!({
                "status": "value",
                "api_code": 0,
                "cf_type_id": 7,
                "observed_ms": stamp,
                "timely": true,
                "registered_path_matches": true,
            })
        };
        json!({
            "binding": "owned-target",
            "semantic_target": "document-value",
            "environment_revision": revision,
            "session_epoch": 7,
            "surface": { "id": "main", "epoch": 1 },
            "snapshot_observed_ms": snapshot_ms,
            "receiver": {
                "value_readback": null,
                "registration_id": REGISTRATION_ID,
                "profile": "owned_plain_text_value_setter_v1",
                "provenance": provenance,
                "process": { "pid": 700, "start_seconds": 1_700_000_000_u64, "start_micros": 123_456 },
                "window_id": "window-42",
                "window_object_ref": "owned-window",
                "window_frame": rect(0.0, 0.0, 600.0, 400.0),
                "actual_object_ref": receiver,
                "receiver_frame": rect(20.0, 40.0, 500.0, 300.0),
                "application_focused_object_ref": receiver,
                "system_focused_object_ref": receiver,
                "ancestry": [
                    { "object_ref": "owned-scroll", "role": "AXScrollArea" },
                    { "object_ref": "owned-window", "role": "AXWindow" },
                ],
                "scope_complete": true,
                "profile_eligible": true,
                "marked_range_stable_unmarked": true,
                "observer_state": "continuous",
                "secure": false,
                "document_binding_verified": true,
                "document_binding": {
                    "before": read(observed_ms + 2),
                    "after": read(observed_ms + 3),
                },
                "title": { "status": "value", "value": "Fixture title" },
                "role": { "status": "value", "value": "AXTextArea" },
                "subrole": { "status": "unsupported", "code": -25205 },
                "enabled": { "status": "unsupported", "code": -25205 },
                "focused": { "status": "value", "value": true },
                "value_settable": { "status": "value", "value": true },
                "description": { "status": "failure", "code": -25200 },
                "marked_range_before": marked(observed_ms + 4),
                "marked_range_after": marked(observed_ms + 5),
            },
        })
    }

    fn proof(
        revision: u64,
        snapshot_ms: u64,
        sequence: u64,
        observed_ms: u64,
        valid_until_ms: u64,
    ) -> interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof {
        serde_json::from_value(proof_value(
            revision,
            snapshot_ms,
            sequence,
            observed_ms,
            valid_until_ms,
        ))
        .unwrap()
    }

    #[test]
    fn complete_owned_request_wire_fits_native_bounds_and_oversize_fails_closed() {
        let registration = registration();
        let original = proof(1, 1_000, 7, 990, 1_400);
        let current = proof(2, 1_080, 8, 1_070, 1_500);
        let permit = PermitRef {
            id: "permit-fixture".into(),
            generation: 1,
        };
        let dependencies = BTreeMap::from([
            ("authorization".into(), 1),
            ("focus".into(), 2),
            ("window".into(), 3),
            ("owned_scope".into(), 4),
        ]);
        let surface = SurfaceRef {
            id: "main".into(),
            epoch: 1,
        };
        let value = "x".repeat(2_048);
        let primitive = Primitive::AxAction {
            action: AxActionKind::SetValue {
                value: value.clone(),
            },
        };
        let intent = ActionIntent {
            intent_id: "intent-fixture".into(),
            attempt_id: Some("attempt-fixture".into()),
            task_id: "task-fixture".into(),
            task_revision: 1,
            root_epoch: 1,
            session_epoch: 7,
            execution_domain:
                interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative,
            permit: permit.clone(),
            surface: surface.clone(),
            target_ref: "owned-target".into(),
            binding_fingerprint: None,
            binding_proof: None,
            owned_document_proof: Some(Box::new(original.clone())),
            coordinate_space: CoordinateSpace::DisplayPoints,
            dependencies: dependencies.clone(),
            evidence_id: "evidence-original".into(),
            latest_start_ms: 1_100,
            duration_ms: 200,
            steps: vec![ScheduledPrimitive {
                offset_ms: 0,
                primitive: primitive.clone(),
            }],
        };
        let evidence = Observation {
            observation_id: "evidence-current".into(),
            session_epoch: 7,
            surface: surface.clone(),
            target_ref: "owned-target".into(),
            clock_domain: CLOCK.into(),
            source_sequence: 8,
            observed_ms: 1_080,
            valid_until_ms: 1_400,
            freshness: SourceFreshness::Verified,
            execution_domain:
                interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative,
            purpose: ObservationPurpose::OwnedDocumentValueSetter,
            binding_proof: None,
            owned_document_proof: Some(Box::new(current.clone())),
            dependencies,
        };
        let authority = SubmissionAuthority {
            permit: permit.clone(),
            stop_seq: 0,
            valid_until_ms: 1_200,
            command_end_ms: 1_250,
            evidence,
        };
        let context = SubmitContext {
            intent: &intent,
            authority,
        };
        let request = NativeOwnedValueRequest::build(
            "request-fixture".into(),
            &context,
            &primitive,
            5,
            NOW,
            7,
            OwnedValueBuildOptions {
                registration: &registration,
                allow_save: false,
            },
        )
        .unwrap();

        assert!(request.original_proof.validate_at(NOW, 2_000).is_ok());
        assert!(request.current_proof.validate_at(NOW, 2_000).is_ok());
        assert_eq!(request.stop_seq, 0);
        assert_eq!(request.action, "ax_set_owned_document_value");
        let original_bytes = serde_json::to_vec(&request.original_proof).unwrap();
        let current_bytes = serde_json::to_vec(&request.current_proof).unwrap();
        let line = serde_json::to_vec(&json!({
            "protocol_version": 1,
            "session_epoch": 7,
            "command": "owned_value_submit",
            "owned_value": &request,
        }))
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&line).unwrap()["owned_value"]["stop_seq"],
            0
        );
        assert!(original_bytes.len() <= OWNED_VALUE_PROOF_LIMIT);
        assert!(current_bytes.len() <= OWNED_VALUE_PROOF_LIMIT);
        assert!(line.len() < OWNED_VALUE_REQUEST_LIMIT as usize);
        assert!(request_wire_within_bounds(&request, 7));

        let save_primitive = Primitive::AxAction {
            action: AxActionKind::SaveOwnedDocument {
                expected_value: value.clone(),
            },
        };
        let mut save_intent = intent.clone();
        save_intent.intent_id = "intent-save-fixture".into();
        save_intent.steps[0].primitive = save_primitive.clone();
        let mut save_evidence = context.authority.evidence.clone();
        save_evidence.observation_id = "evidence-save-current".into();
        save_evidence.purpose = ObservationPurpose::OwnedDocumentSave;
        let save_context = SubmitContext {
            intent: &save_intent,
            authority: SubmissionAuthority {
                permit: permit.clone(),
                stop_seq: 0,
                valid_until_ms: 1_200,
                command_end_ms: 1_250,
                evidence: save_evidence,
            },
        };
        let save_request = NativeOwnedValueRequest::build(
            "request-save-fixture".into(),
            &save_context,
            &save_primitive,
            5,
            NOW,
            7,
            OwnedValueBuildOptions {
                registration: &registration,
                allow_save: true,
            },
        )
        .unwrap();
        assert_eq!(save_request.action, "ax_save_document");
        assert_eq!(save_request.value, value);
        assert!(request_wire_within_bounds(&save_request, 7));
        let mut save_control_value = save_request.clone();
        save_control_value.value = "\u{0001}".into();
        assert!(request_wire_within_bounds(&save_control_value, 7));
        assert!(
            NativeOwnedValueRequest::build(
                "request-save-disabled".into(),
                &save_context,
                &save_primitive,
                5,
                NOW,
                7,
                OwnedValueBuildOptions {
                    registration: &registration,
                    allow_save: false,
                },
            )
            .is_err()
        );
        let mut arbitrary_action = save_request;
        arbitrary_action.action = "ax_press_arbitrary_element".into();
        assert!(!request_wire_within_bounds(&arbitrary_action, 7));

        let mut oversized_proof = request.clone();
        oversized_proof.original_proof.receiver.title =
            interaction_contracts::desktop::owned_document::AxPropertyObservation::Value {
                value: "x".repeat(9_000),
            };
        assert!(!request_wire_within_bounds(&oversized_proof, 7));

        let mut oversized_line = request;
        oversized_line.value = "x".repeat(40_000);
        assert!(!request_wire_within_bounds(&oversized_line, 7));
    }

    #[test]
    fn owned_native_dependency_updates_forward_only_the_exact_scope() {
        for name in ["authorization", "focus", "window", "owned_scope"] {
            assert_eq!(
                dependency_update_command(7, name, 4, Some(false)),
                Some(serde_json::json!({
                    "protocol_version": 1,
                    "session_epoch": 7,
                    "command": "dependency_update",
                    "name": name,
                    "revision": 4,
                    "valid": false,
                }))
            );
        }
        for name in ["binding", "geometry", "unrelated", "owned_scope_extra"] {
            assert!(dependency_update_command(7, name, 4, Some(false)).is_none());
        }
        assert!(dependency_update_command(0, "focus", 4, Some(true)).is_none());
        assert!(dependency_update_command(7, "focus", 0, Some(true)).is_none());
    }

    #[test]
    fn owned_save_expected_value_is_nul_free_and_bounded_without_setter_controls_policy() {
        let control = char::from_u32(1).unwrap().to_string();
        assert!(owned_action_value_is_valid("ax_save_document", &control));
        assert!(owned_action_value_is_valid(
            "ax_save_document",
            "line\nwith text"
        ));
        assert!(!owned_action_value_is_valid(
            "ax_save_document",
            "nul\0byte"
        ));
        assert!(!owned_action_value_is_valid(
            "ax_save_document",
            &"x".repeat(2_049)
        ));
        assert!(!owned_action_value_is_valid(
            "ax_set_owned_document_value",
            &control
        ));
        assert!(!owned_action_value_is_valid(
            "ax_press_arbitrary_element",
            "safe"
        ));
    }
}
