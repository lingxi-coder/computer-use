//! Versioned wire messages for the deterministic interaction prototype.
//!
//! Decoding checks message-local invariants. Session identity, current time,
//! evidence lookup, and execution authorization belong to the runtime.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceRef {
    #[schemars(length(min = 1))]
    pub id: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermitRef {
    #[schemars(length(min = 1))]
    pub id: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceFreshness {
    Verified,
    Uncertain,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    #[schemars(length(min = 1))]
    pub observation_id: String,
    pub session_epoch: u64,
    pub surface: SurfaceRef,
    #[schemars(length(min = 1))]
    pub target_ref: String,
    #[schemars(length(min = 1))]
    pub clock_domain: String,
    pub source_sequence: u64,
    pub observed_ms: u64,
    pub valid_until_ms: u64,
    pub freshness: SourceFreshness,
    pub dependencies: BTreeMap<String, u64>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Input {
    KeyDown {
        #[schemars(length(min = 1))]
        key: String,
    },
    KeyUp {
        #[schemars(length(min = 1))]
        key: String,
    },
    PointerDown {
        button: MouseButton,
    },
    PointerUp {
        button: MouseButton,
    },
    PointerMove {
        x: i32,
        y: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduledInput {
    pub offset_ms: u64,
    pub input: Input,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    SurfacePx,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionIntent {
    #[schemars(length(min = 1))]
    pub intent_id: String,
    #[schemars(length(min = 1))]
    pub task_id: String,
    pub task_revision: u64,
    pub session_epoch: u64,
    pub permit: PermitRef,
    pub surface: SurfaceRef,
    #[schemars(length(min = 1))]
    pub target_ref: String,
    pub coordinate_space: CoordinateSpace,
    pub dependencies: BTreeMap<String, u64>,
    #[schemars(length(min = 1))]
    pub evidence_id: String,
    pub latest_start_ms: u64,
    #[schemars(range(min = 1))]
    pub duration_ms: u64,
    #[schemars(length(min = 1))]
    pub steps: Vec<ScheduledInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeExecution {
    pub session_epoch: u64,
    pub permit: PermitRef,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenewPermit {
    pub session_epoch: u64,
    pub permit: PermitRef,
    #[schemars(length(min = 1))]
    pub evidence_id: String,
    pub valid_until_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Accepted,
    Running,
    Completed,
    Cancelled,
    Rejected,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStatus {
    NotNeeded,
    Held,
    Released,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmittedInput {
    pub at_ms: u64,
    pub input: Input,
    pub cleanup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    #[schemars(length(min = 1))]
    pub intent_id: String,
    pub session_epoch: u64,
    pub status: ReceiptStatus,
    pub reason: Option<String>,
    pub submitted: Vec<SubmittedInput>,
    pub release_status: ReleaseStatus,
    pub command_end_ms: Option<u64>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectStatus {
    Satisfied,
    Unsatisfied,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectResult {
    #[schemars(length(min = 1))]
    pub intent_id: String,
    #[schemars(length(min = 1))]
    pub predicate: String,
    pub status: EffectStatus,
    #[schemars(inner(length(min = 1)))]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Message {
    Observation(Observation),
    ActionIntent(ActionIntent),
    RevokeExecution(RevokeExecution),
    RenewPermit(RenewPermit),
    ActionReceipt(Receipt),
    EffectResult(EffectResult),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WireMessage {
    #[schemars(range(min = 1, max = 1))]
    pub protocol_version: u16,
    pub message: Message,
}

impl WireMessage {
    /// Validate invariants that do not require external state or a clock.
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(format!(
                "unsupported protocol_version {}; expected {PROTOCOL_VERSION}",
                self.protocol_version
            ));
        }

        match &self.message {
            Message::Observation(observation) => observation.validate(),
            Message::ActionIntent(intent) => intent.validate(),
            Message::RevokeExecution(revoke) => {
                require_nonempty("permit.id", &revoke.permit.id)?;
                require_nonempty("reason", &revoke.reason)
            }
            Message::RenewPermit(renew) => {
                require_nonempty("permit.id", &renew.permit.id)?;
                require_nonempty("evidence_id", &renew.evidence_id)
            }
            Message::ActionReceipt(receipt) => receipt.validate(),
            Message::EffectResult(result) => {
                require_nonempty("intent_id", &result.intent_id)?;
                require_nonempty("predicate", &result.predicate)?;
                for evidence_ref in &result.evidence_refs {
                    require_nonempty("evidence_refs entry", evidence_ref)?;
                }
                Ok(())
            }
        }
    }
}

impl Observation {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("observation_id", &self.observation_id)?;
        require_nonempty("surface.id", &self.surface.id)?;
        require_nonempty("target_ref", &self.target_ref)?;
        require_nonempty("clock_domain", &self.clock_domain)?;
        validate_dependencies(&self.dependencies)?;
        if self.observed_ms >= self.valid_until_ms {
            return Err("observed_ms must be less than valid_until_ms".into());
        }
        Ok(())
    }
}

impl ActionIntent {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("intent_id", &self.intent_id)?;
        require_nonempty("task_id", &self.task_id)?;
        require_nonempty("permit.id", &self.permit.id)?;
        require_nonempty("surface.id", &self.surface.id)?;
        require_nonempty("target_ref", &self.target_ref)?;
        require_nonempty("evidence_id", &self.evidence_id)?;
        validate_dependencies(&self.dependencies)?;

        if self.duration_ms == 0 {
            return Err("duration_ms must be greater than zero".into());
        }
        if self.latest_start_ms.checked_add(self.duration_ms).is_none() {
            return Err("latest_start_ms + duration_ms exceeds the u64 time range".into());
        }
        if self.steps.is_empty() {
            return Err("steps must not be empty".into());
        }

        let mut previous_offset = None;
        let mut held_keys = BTreeSet::new();
        let mut held_buttons = BTreeSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            if step.offset_ms >= self.duration_ms {
                return Err(format!(
                    "steps[{index}].offset_ms must be less than duration_ms"
                ));
            }
            if previous_offset.is_some_and(|previous| step.offset_ms <= previous) {
                return Err("step offsets must be strictly increasing".into());
            }
            previous_offset = Some(step.offset_ms);
            validate_input(&step.input)?;

            match &step.input {
                Input::KeyDown { key } if !held_keys.insert(key.as_str()) => {
                    return Err(format!(
                        "steps[{index}] presses a key already held by this action"
                    ));
                }
                Input::KeyUp { key } if !held_keys.remove(key.as_str()) => {
                    return Err(format!(
                        "steps[{index}] releases a key not held by this action"
                    ));
                }
                Input::PointerDown { button } if !held_buttons.insert(*button) => {
                    return Err(format!(
                        "steps[{index}] presses a button already held by this action"
                    ));
                }
                Input::PointerUp { button } if !held_buttons.remove(button) => {
                    return Err(format!(
                        "steps[{index}] releases a button not held by this action"
                    ));
                }
                _ => {}
            }
        }
        // Remaining held inputs are valid; the executor owns deadline cleanup.
        Ok(())
    }
}

impl Receipt {
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("intent_id", &self.intent_id)?;
        let mut previous_time = None;
        for submitted in &self.submitted {
            validate_input(&submitted.input)?;
            if submitted.at_ms > self.at_ms {
                return Err("submitted input must not be later than receipt.at_ms".into());
            }
            if previous_time.is_some_and(|previous| submitted.at_ms < previous) {
                return Err("submitted input times must not decrease".into());
            }
            previous_time = Some(submitted.at_ms);
        }
        Ok(())
    }
}

/// Decode strict JSON and reject unsupported versions or invalid local values.
pub fn decode(json: &str) -> Result<WireMessage, String> {
    let message: WireMessage = serde_json::from_str(json).map_err(|error| error.to_string())?;
    message.validate()?;
    Ok(message)
}

fn require_nonempty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must not be empty or whitespace-only"))
    } else {
        Ok(())
    }
}

fn validate_dependencies(dependencies: &BTreeMap<String, u64>) -> Result<(), String> {
    for name in dependencies.keys() {
        require_nonempty("dependency name", name)?;
    }
    Ok(())
}

fn validate_input(input: &Input) -> Result<(), String> {
    match input {
        Input::KeyDown { key } | Input::KeyUp { key } => require_nonempty("input.key", key),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action() -> ActionIntent {
        ActionIntent {
            intent_id: "intent-1".into(),
            task_id: "task-1".into(),
            task_revision: 1,
            session_epoch: 1,
            permit: PermitRef {
                id: "permit-1".into(),
                generation: 1,
            },
            surface: SurfaceRef {
                id: "surface-1".into(),
                epoch: 1,
            },
            target_ref: "target-1".into(),
            coordinate_space: CoordinateSpace::SurfacePx,
            dependencies: BTreeMap::from([("focus".into(), 1)]),
            evidence_id: "observation-1".into(),
            latest_start_ms: 10,
            duration_ms: 20,
            steps: vec![ScheduledInput {
                offset_ms: 0,
                input: Input::KeyDown { key: "w".into() },
            }],
        }
    }

    fn wire() -> WireMessage {
        WireMessage {
            protocol_version: PROTOCOL_VERSION,
            message: Message::ActionIntent(action()),
        }
    }

    #[test]
    fn round_trip_uses_lowercase_tagged_payloads_and_allows_cleanup() {
        let original = wire();
        let json = serde_json::to_value(&original).unwrap();
        assert_eq!(json["message"]["type"], "action_intent");
        assert_eq!(json["message"]["payload"]["coordinate_space"], "surface_px");
        assert_eq!(
            json["message"]["payload"]["steps"][0]["input"]["type"],
            "key_down"
        );
        assert_eq!(decode(&json.to_string()).unwrap(), original);
    }

    #[test]
    fn strict_decode_rejects_unknown_fields_at_each_nested_boundary() {
        let schema = serde_json::to_value(schemars::schema_for!(WireMessage)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&serde_json::to_value(wire()).unwrap()));
        for path in [
            "",
            "/message",
            "/message/payload",
            "/message/payload/surface",
            "/message/payload/steps/0",
            "/message/payload/steps/0/input",
            "/message/payload/steps/0/input/payload",
        ] {
            let mut json = serde_json::to_value(wire()).unwrap();
            json.pointer_mut(path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("unexpected".into(), serde_json::json!(true));
            assert!(
                decode(&json.to_string()).is_err(),
                "accepted unknown field at {path}"
            );
            assert!(
                !validator.is_valid(&json),
                "schema accepted unknown field at {path}"
            );
        }
    }

    #[test]
    fn schema_encodes_version_and_nonempty_action_constraints() {
        let schema = serde_json::to_value(schemars::schema_for!(WireMessage)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for (path, invalid_value) in [
            ("/protocol_version", serde_json::json!(2)),
            ("/message/payload/intent_id", serde_json::json!("")),
            ("/message/payload/duration_ms", serde_json::json!(0)),
            ("/message/payload/steps", serde_json::json!([])),
        ] {
            let mut json = serde_json::to_value(wire()).unwrap();
            *json.pointer_mut(path).unwrap() = invalid_value;
            assert!(!validator.is_valid(&json), "schema accepted invalid {path}");
        }
    }

    #[test]
    fn decode_rejects_unsupported_versions_and_blank_identifiers() {
        let mut message = wire();
        message.protocol_version = 2;
        assert!(decode(&serde_json::to_string(&message).unwrap()).is_err());
        let mut intent = action();
        intent.target_ref = " \t\n".into();
        assert!(intent.validate().is_err());
    }

    #[test]
    fn action_rejects_invalid_time_ranges_and_offsets() {
        let mut intent = action();
        intent.duration_ms = 0;
        assert!(intent.validate().is_err());
        intent = action();
        intent.latest_start_ms = u64::MAX;
        assert!(intent.validate().is_err());
        intent = action();
        intent.steps.clear();
        assert!(intent.validate().is_err());
        for offset in [0, 20, 21] {
            intent = action();
            intent.steps.push(ScheduledInput {
                offset_ms: offset,
                input: Input::KeyUp { key: "w".into() },
            });
            assert!(intent.validate().is_err(), "accepted offset {offset}");
        }
    }

    #[test]
    fn action_tracks_keys_and_buttons_independently() {
        let mut intent = action();
        intent.steps.push(ScheduledInput {
            offset_ms: 1,
            input: Input::KeyDown { key: "w".into() },
        });
        assert!(intent.validate().is_err());
        intent.steps[1].input = Input::KeyUp { key: "a".into() };
        assert!(intent.validate().is_err());
        intent.steps[1].input = Input::PointerUp {
            button: MouseButton::Left,
        };
        assert!(intent.validate().is_err());
        intent.steps[1].input = Input::PointerDown {
            button: MouseButton::Left,
        };
        assert!(intent.validate().is_ok());
        intent.steps.push(ScheduledInput {
            offset_ms: 2,
            input: Input::PointerDown {
                button: MouseButton::Left,
            },
        });
        assert!(intent.validate().is_err());
        intent.steps[2].input = Input::PointerUp {
            button: MouseButton::Left,
        };
        intent.steps.push(ScheduledInput {
            offset_ms: 3,
            input: Input::KeyUp { key: "w".into() },
        });
        assert!(intent.validate().is_ok());
    }

    #[test]
    fn observation_requires_positive_validity_window() {
        let mut observation = Observation {
            observation_id: "obs-1".into(),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "surface-1".into(),
                epoch: 1,
            },
            target_ref: "target-1".into(),
            clock_domain: "device-monotonic".into(),
            source_sequence: 1,
            observed_ms: 10,
            valid_until_ms: 11,
            freshness: SourceFreshness::Verified,
            dependencies: BTreeMap::new(),
        };
        assert!(observation.validate().is_ok());
        observation.valid_until_ms = 10;
        assert!(observation.validate().is_err());
        observation.valid_until_ms = 9;
        assert!(observation.validate().is_err());
    }

    #[test]
    fn receipt_allows_simultaneous_cleanup_but_not_future_or_reordered_inputs() {
        let mut receipt = Receipt {
            intent_id: "intent-1".into(),
            session_epoch: 1,
            status: ReceiptStatus::Running,
            reason: None,
            submitted: vec![SubmittedInput {
                at_ms: 10,
                input: Input::KeyDown { key: "w".into() },
                cleanup: false,
            }],
            release_status: ReleaseStatus::Held,
            command_end_ms: Some(20),
            at_ms: 10,
        };
        assert!(receipt.validate().is_ok());
        receipt.submitted.push(SubmittedInput {
            at_ms: 10,
            input: Input::KeyUp { key: "w".into() },
            cleanup: true,
        });
        assert!(receipt.validate().is_ok());
        receipt.submitted[1].at_ms = 11;
        assert!(receipt.validate().is_err());
        receipt.submitted[1].at_ms = 9;
        assert!(receipt.validate().is_err());
    }
}
