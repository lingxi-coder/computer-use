//! One-shot native AX submissions. An absent/invalid reply after enqueue is
//! unknown, never a definite rejection and never permission to resend.
use serde::{Deserialize, Serialize};

use super::authority::NativeProbeRequest;

const MAX_ROW_AX_CALL_SPAN_NS: u64 = 1_000_000_000;
const MAX_AX_PRESS_CALL_SPAN_NS: u64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeRowTracePrimitive {
    AxSetSelectedTrue,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeRowTraceClockDomain {
    DispatchTimeUptime,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeAxTracePrimitive {
    AxPress,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeAxTraceClockDomain {
    DispatchTimeUptime,
}

/// Fixed, non-authorizing progress marker for BackgroundAx diagnostics. This
/// is deliberately separate from the AXPress call trace and is never used to
/// validate or authorize a submit.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeBackgroundAxTraceStage {
    Preflight,
    ProcessIdentity,
    ApplicationBoundary,
    WindowBoundary,
    TargetTraversal,
    TargetMatch,
    TargetNodeIdentity,
    TargetChildrenCount,
    TargetChildrenRead,
    TargetChildrenConfirm,
    TargetDeadline,
    AuxiliaryRelation,
    AuxiliaryNonempty,
    ChildrenType,
    AuxiliaryDecorationChildren,
    AuxiliaryGroupIdentity,
    AuxiliaryGroupRole,
    AuxiliaryGroupParent,
    AuxiliaryGroupWindow,
    AuxiliaryGroupChildren,
    AuxiliaryGroupBounds,
    AuxiliaryWindowUnsupported,
    AuxiliaryWindowNoValue,
    AuxiliaryWindowTimeout,
    AuxiliaryWindowReadFailed,
    AuxiliaryWindowMalformed,
    AuxiliaryWindowForeign,
    AuxiliaryWindowMismatch,
    AuxiliaryWindowParentUnproven,
    InitialStability,
    FinalRevalidation,
    CommitReady,
    Submitted,
}

/// Boolean-only native BackgroundAx diagnostic metadata. Invalid or unknown
/// diagnostic shapes are discarded without changing the submit response.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeBackgroundAxTrace {
    pub stage: NativeBackgroundAxTraceStage,
    pub process_identity_matches: bool,
    pub application_boundary_matches: bool,
    pub window_boundary_matches: bool,
    pub target_traversal_complete: bool,
    pub target_matches: bool,
    pub initial_stability_passed: bool,
    pub final_revalidation_passed: bool,
    pub commit_ready: bool,
    pub submitted: bool,
}

fn deserialize_background_ax_trace<'de, D>(
    deserializer: D,
) -> Result<Option<NativeBackgroundAxTrace>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// Private timing/result diagnostics for the single row AX setter. This does
/// not authorize input or establish a business effect.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeRowTrace {
    pub primitive: NativeRowTracePrimitive,
    pub ax_call_count: u8,
    pub ax_error_code: Option<i32>,
    pub clock_domain: NativeRowTraceClockDomain,
    pub call_started_uptime_ns: Option<u64>,
    pub call_finished_uptime_ns: Option<u64>,
}

impl NativeRowTrace {
    fn valid(&self) -> bool {
        if self.primitive != NativeRowTracePrimitive::AxSetSelectedTrue
            || self.clock_domain != NativeRowTraceClockDomain::DispatchTimeUptime
        {
            return false;
        }

        match (
            self.ax_call_count,
            self.ax_error_code,
            self.call_started_uptime_ns,
            self.call_finished_uptime_ns,
        ) {
            (0, None, None, None) => true,
            (1, Some(_), Some(started), Some(finished)) => {
                started > 0
                    && finished >= started
                    && finished.saturating_sub(started) <= MAX_ROW_AX_CALL_SPAN_NS
            }
            _ => false,
        }
    }
}

/// Private timing/result diagnostics for one AXPress call. It never
/// authorizes input or establishes a business effect.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeAxTrace {
    pub primitive: NativeAxTracePrimitive,
    pub ax_call_count: u8,
    pub ax_error_code: Option<i32>,
    pub clock_domain: NativeAxTraceClockDomain,
    pub call_started_uptime_ns: Option<u64>,
    pub call_finished_uptime_ns: Option<u64>,
}

impl NativeAxTrace {
    fn valid(&self) -> bool {
        if self.primitive != NativeAxTracePrimitive::AxPress
            || self.clock_domain != NativeAxTraceClockDomain::DispatchTimeUptime
        {
            return false;
        }

        match (
            self.ax_call_count,
            self.ax_error_code,
            self.call_started_uptime_ns,
            self.call_finished_uptime_ns,
        ) {
            (0, None, None, None) => true,
            (1, Some(_), Some(started), Some(finished)) => {
                started > 0
                    && finished >= started
                    && finished.saturating_sub(started) <= MAX_AX_PRESS_CALL_SPAN_NS
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSubmitResponse {
    pub protocol_version: u32,
    pub kind: String,
    pub session_epoch: u64,
    pub request_id: String,
    pub outcome: String,
    pub reason: String,
    pub submitted: bool,
    pub committed_ms: Option<u64>,
    pub returned_ms: Option<u64>,
    pub binding_digest: Option<String>,
    /// Present only when the native worker reports the dedicated row setter.
    /// It is diagnostic evidence, not an authorization or effect receipt.
    #[serde(default)]
    pub row_trace: Option<NativeRowTrace>,
    /// Present only when the native worker observed its AXPress commit gate.
    /// It is optional measurement, never input authorization or effect proof.
    #[serde(default)]
    pub ax_trace: Option<NativeAxTrace>,
    /// Optional fixed-stage BackgroundAx diagnostics. Invalid diagnostics are
    /// ignored and never affect response validity or authorization.
    #[serde(default, deserialize_with = "deserialize_background_ax_trace")]
    pub background_ax_trace: Option<NativeBackgroundAxTrace>,
}

impl NativeSubmitResponse {
    pub fn valid_for(&self, request: &NativeProbeRequest, epoch: u64) -> bool {
        let background_ax_request = request.execution_domain
            == Some(interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx);
        let request_lane_valid = if background_ax_request {
            request.purpose
                == Some(
                    interaction_contracts::desktop::device::ObservationPurpose::BackgroundAxAction,
                )
                && request.action == "ax_press"
                && request.x == 0.0
                && request.y == 0.0
                && request.coordinate_space
                    == Some(interaction_contracts::desktop::device::CoordinateSpace::TargetObject)
                && request.value.is_none()
                && request.mouse.is_none()
                && request.scroll.is_none()
                && request.key.is_none()
                && request.original_proof.supports_background_ax_press()
                && request.current_proof.supports_background_ax_press()
                && request.original_proof.same_instance(&request.current_proof)
                && request.context_witness == request.current_proof.device_context
                && request.context_witness.supports_execution_domain(
                    interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx,
                )
        } else {
            request.execution_domain.is_none()
                && request.purpose.is_none()
                && request.coordinate_space.is_none()
        };
        let row_action = request.action == "ax_set_selected_true";
        let row_trace_valid = match (&self.row_trace, row_action, self.submitted) {
            (None, false, _) => true,
            (Some(_), false, _) => false,
            (None, true, false) => true,
            (None, true, true) => false,
            (Some(trace), true, submitted) => {
                trace.valid()
                    && if submitted {
                        trace.ax_call_count == 1
                            && (self.outcome != "applied" || trace.ax_error_code == Some(0))
                    } else {
                        trace.ax_call_count == 0
                    }
            }
        };
        let ax_press = request.action == "ax_press";
        let ax_trace_valid = match (&self.ax_trace, self.submitted, background_ax_request) {
            (None, true, true) => false,
            (None, _, _) => true,
            (Some(_), _, false) if !ax_press => false,
            (Some(trace), true, true) => {
                trace.valid()
                    && match self.outcome.as_str() {
                        "applied" => trace.ax_call_count == 1 && trace.ax_error_code == Some(0),
                        "unknown" => matches!(trace.ax_call_count, 0 | 1),
                        _ => false,
                    }
            }
            (Some(trace), submitted, _) => {
                ax_press
                    && trace.valid()
                    && if submitted {
                        trace.ax_call_count == 1
                            && (self.outcome != "applied" || trace.ax_error_code == Some(0))
                    } else {
                        trace.ax_call_count == 0
                    }
            }
        };
        self.protocol_version == 1
            && self.kind == "atomic_submit"
            && self.session_epoch == epoch
            && self.request_id == request.request_id
            && self.reason.len() <= 256
            && request_lane_valid
            && row_trace_valid
            && ax_trace_valid
            && if self.submitted {
                matches!(self.outcome.as_str(), "applied" | "unknown")
                    && self.binding_digest.as_deref() == Some(request.binding_digest.as_str())
                    && self.committed_ms.is_some_and(|at| {
                        at >= request.started_ms
                            && at < request.valid_until_ms
                            && match self.outcome.as_str() {
                                "applied" => self.returned_ms.is_some_and(|end| {
                                    end >= at && end <= request.valid_until_ms.saturating_add(500)
                                }),
                                "unknown" => self.returned_ms.is_none_or(|end| {
                                    end >= at && end <= request.valid_until_ms.saturating_add(500)
                                }),
                                _ => false,
                            }
                    })
            } else {
                matches!(
                    self.outcome.as_str(),
                    "rejected"
                        | "expired"
                        | "revoked"
                        | "stop_latched"
                        | "context_unresolved"
                        | "target_occluded"
                ) && self.committed_ms.is_none()
                    && self.returned_ms.is_none()
                    && self.binding_digest.is_none()
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn submission_receipts_reject_extra_fields_and_probe_tokens() {
        assert!(
            serde_json::from_value::<NativeSubmitResponse>(json!({
                "protocol_version":1,"kind":"atomic_submit","session_epoch":7,
                "request_id":"one","outcome":"applied","reason":"ok","submitted":true,
                "committed_ms":1,"returned_ms":2,"binding_digest":"fp1:abc","input_authorized":true
            }))
            .is_err()
        );
    }

    #[test]
    fn ax_press_trace_can_be_absent_when_the_call_was_not_observed() {
        let base = json!({
            "protocol_version":1,"kind":"atomic_submit","session_epoch":7,
            "request_id":"press","outcome":"context_unresolved","reason":"preflight",
            "submitted":false,"committed_ms":null,"returned_ms":null,
            "binding_digest":null
        });
        let missing: NativeSubmitResponse = serde_json::from_value(base.clone()).unwrap();
        assert!(missing.ax_trace.is_none());

        let mut explicit_null = base;
        explicit_null["ax_trace"] = serde_json::Value::Null;
        let null: NativeSubmitResponse = serde_json::from_value(explicit_null).unwrap();
        assert!(null.ax_trace.is_none());
    }

    #[test]
    fn background_ax_stage_diagnostics_are_optional_closed_and_non_authorizing() {
        let mut response = json!({
            "protocol_version":1,"kind":"atomic_submit","session_epoch":7,
            "request_id":"press","outcome":"context_unresolved","reason":"preflight",
            "submitted":false,"committed_ms":null,"returned_ms":null,
            "binding_digest":null
        });
        response["background_ax_trace"] = json!({
            "stage":"window_boundary",
            "process_identity_matches":true,
            "application_boundary_matches":true,
            "window_boundary_matches":false,
            "target_traversal_complete":false,
            "target_matches":false,
            "initial_stability_passed":false,
            "final_revalidation_passed":false,
            "commit_ready":false,
            "submitted":false
        });
        let decoded: NativeSubmitResponse = serde_json::from_value(response.clone()).unwrap();
        let trace = decoded.background_ax_trace.unwrap();
        assert_eq!(trace.stage, NativeBackgroundAxTraceStage::WindowBoundary);
        assert!(!trace.window_boundary_matches);
        assert!(!trace.submitted);

        response["background_ax_trace"]["unreviewed_value"] = json!("must not escape");
        let decoded: NativeSubmitResponse = serde_json::from_value(response).unwrap();
        assert!(decoded.background_ax_trace.is_none());
        assert_eq!(decoded.outcome, "context_unresolved");
        assert!(!decoded.submitted);
    }

    #[test]
    fn background_ax_target_traversal_stages_use_the_closed_native_wire_values() {
        for (wire, stage) in [
            (
                "target_node_identity",
                NativeBackgroundAxTraceStage::TargetNodeIdentity,
            ),
            (
                "target_children_count",
                NativeBackgroundAxTraceStage::TargetChildrenCount,
            ),
            (
                "target_children_read",
                NativeBackgroundAxTraceStage::TargetChildrenRead,
            ),
            (
                "target_children_confirm",
                NativeBackgroundAxTraceStage::TargetChildrenConfirm,
            ),
            (
                "target_deadline",
                NativeBackgroundAxTraceStage::TargetDeadline,
            ),
            (
                "auxiliary_relation",
                NativeBackgroundAxTraceStage::AuxiliaryRelation,
            ),
            (
                "auxiliary_nonempty",
                NativeBackgroundAxTraceStage::AuxiliaryNonempty,
            ),
            ("children_type", NativeBackgroundAxTraceStage::ChildrenType),
            (
                "auxiliary_decoration_children",
                NativeBackgroundAxTraceStage::AuxiliaryDecorationChildren,
            ),
            (
                "auxiliary_group_identity",
                NativeBackgroundAxTraceStage::AuxiliaryGroupIdentity,
            ),
            (
                "auxiliary_group_role",
                NativeBackgroundAxTraceStage::AuxiliaryGroupRole,
            ),
            (
                "auxiliary_group_parent",
                NativeBackgroundAxTraceStage::AuxiliaryGroupParent,
            ),
            (
                "auxiliary_group_window",
                NativeBackgroundAxTraceStage::AuxiliaryGroupWindow,
            ),
            (
                "auxiliary_group_children",
                NativeBackgroundAxTraceStage::AuxiliaryGroupChildren,
            ),
            (
                "auxiliary_group_bounds",
                NativeBackgroundAxTraceStage::AuxiliaryGroupBounds,
            ),
            (
                "auxiliary_window_unsupported",
                NativeBackgroundAxTraceStage::AuxiliaryWindowUnsupported,
            ),
            (
                "auxiliary_window_no_value",
                NativeBackgroundAxTraceStage::AuxiliaryWindowNoValue,
            ),
            (
                "auxiliary_window_timeout",
                NativeBackgroundAxTraceStage::AuxiliaryWindowTimeout,
            ),
            (
                "auxiliary_window_read_failed",
                NativeBackgroundAxTraceStage::AuxiliaryWindowReadFailed,
            ),
            (
                "auxiliary_window_malformed",
                NativeBackgroundAxTraceStage::AuxiliaryWindowMalformed,
            ),
            (
                "auxiliary_window_foreign",
                NativeBackgroundAxTraceStage::AuxiliaryWindowForeign,
            ),
            (
                "auxiliary_window_mismatch",
                NativeBackgroundAxTraceStage::AuxiliaryWindowMismatch,
            ),
            (
                "auxiliary_window_parent_unproven",
                NativeBackgroundAxTraceStage::AuxiliaryWindowParentUnproven,
            ),
        ] {
            assert_eq!(serde_json::to_value(stage).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<NativeBackgroundAxTraceStage>(json!(wire)).unwrap(),
                stage
            );
        }

        let mut response = json!({
            "protocol_version":1,"kind":"atomic_submit","session_epoch":7,
            "request_id":"press","outcome":"context_unresolved","reason":"target deadline",
            "submitted":false,"committed_ms":null,"returned_ms":null,
            "binding_digest":null,
            "background_ax_trace":{
                "stage":"target_deadline",
                "process_identity_matches":true,
                "application_boundary_matches":true,
                "window_boundary_matches":true,
                "target_traversal_complete":false,
                "target_matches":false,
                "initial_stability_passed":false,
                "final_revalidation_passed":false,
                "commit_ready":false,
                "submitted":false
            }
        });
        let decoded: NativeSubmitResponse = serde_json::from_value(response.clone()).unwrap();
        assert_eq!(
            decoded.background_ax_trace.unwrap().stage,
            NativeBackgroundAxTraceStage::TargetDeadline
        );
        assert_eq!(decoded.outcome, "context_unresolved");
        assert!(!decoded.submitted);

        response["background_ax_trace"]["stage"] = json!("target_parent_chain");
        let decoded: NativeSubmitResponse = serde_json::from_value(response).unwrap();
        assert!(decoded.background_ax_trace.is_none());
        assert_eq!(decoded.outcome, "context_unresolved");
        assert!(!decoded.submitted);
    }

    #[test]
    fn row_trace_has_a_bounded_strict_native_uptime_shape() {
        let called = json!({
            "primitive":"ax_set_selected_true",
            "ax_call_count":1,
            "ax_error_code":0,
            "clock_domain":"dispatch_time_uptime",
            "call_started_uptime_ns":100,
            "call_finished_uptime_ns":125
        });
        let trace: NativeRowTrace = serde_json::from_value(called.clone()).unwrap();
        assert!(trace.valid());

        let zero = json!({
            "primitive":"ax_set_selected_true",
            "ax_call_count":0,
            "ax_error_code":null,
            "clock_domain":"dispatch_time_uptime",
            "call_started_uptime_ns":null,
            "call_finished_uptime_ns":null
        });
        assert!(
            serde_json::from_value::<NativeRowTrace>(zero)
                .unwrap()
                .valid()
        );

        for invalid in [
            json!({"primitive":"ax_press","ax_call_count":1,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":100,
                "call_finished_uptime_ns":125}),
            json!({"primitive":"ax_set_selected_true","ax_call_count":2,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":100,
                "call_finished_uptime_ns":125}),
            json!({"primitive":"ax_set_selected_true","ax_call_count":0,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":100,
                "call_finished_uptime_ns":125}),
            json!({"primitive":"ax_set_selected_true","ax_call_count":1,"ax_error_code":0,
                "clock_domain":"core_wall","call_started_uptime_ns":100,
                "call_finished_uptime_ns":125}),
            json!({"primitive":"ax_set_selected_true","ax_call_count":1,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":200,
                "call_finished_uptime_ns":100}),
            json!({"primitive":"ax_set_selected_true","ax_call_count":1,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":100,
                "call_finished_uptime_ns":1_000_000_101}),
        ] {
            if let Ok(trace) = serde_json::from_value::<NativeRowTrace>(invalid) {
                assert!(!trace.valid());
            }
        }

        let mut extra = called;
        extra["source_object_ref"] = json!("opaque");
        assert!(serde_json::from_value::<NativeRowTrace>(extra).is_err());
    }

    #[test]
    fn ax_press_trace_has_an_independent_bounded_native_uptime_shape() {
        let called = json!({
            "primitive":"ax_press",
            "ax_call_count":1,
            "ax_error_code":-25200,
            "clock_domain":"dispatch_time_uptime",
            "call_started_uptime_ns":100,
            "call_finished_uptime_ns":125
        });
        let trace: NativeAxTrace = serde_json::from_value(called.clone()).unwrap();
        assert!(trace.valid());
        assert_eq!(serde_json::to_value(&trace).unwrap(), called);

        let zero = json!({
            "primitive":"ax_press",
            "ax_call_count":0,
            "ax_error_code":null,
            "clock_domain":"dispatch_time_uptime",
            "call_started_uptime_ns":null,
            "call_finished_uptime_ns":null
        });
        assert!(
            serde_json::from_value::<NativeAxTrace>(zero)
                .unwrap()
                .valid()
        );

        for invalid in [
            json!({"primitive":"ax_set_selected_true","ax_call_count":1,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":100,
                "call_finished_uptime_ns":125}),
            json!({"primitive":"ax_press","ax_call_count":2,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":100,
                "call_finished_uptime_ns":125}),
            json!({"primitive":"ax_press","ax_call_count":0,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":100,
                "call_finished_uptime_ns":125}),
            json!({"primitive":"ax_press","ax_call_count":1,"ax_error_code":0,
                "clock_domain":"core_wall","call_started_uptime_ns":100,
                "call_finished_uptime_ns":125}),
            json!({"primitive":"ax_press","ax_call_count":1,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":200,
                "call_finished_uptime_ns":100}),
            json!({"primitive":"ax_press","ax_call_count":1,"ax_error_code":0,
                "clock_domain":"dispatch_time_uptime","call_started_uptime_ns":100,
                "call_finished_uptime_ns":1_000_000_101}),
        ] {
            if let Ok(trace) = serde_json::from_value::<NativeAxTrace>(invalid) {
                assert!(!trace.valid());
            }
        }

        let mut extra = called;
        extra["row_trace"] = json!({"primitive":"ax_set_selected_true"});
        assert!(serde_json::from_value::<NativeAxTrace>(extra).is_err());
    }
}
