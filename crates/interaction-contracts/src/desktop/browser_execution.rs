//! Bounded execution evidence and action contracts for the owned browser
//! device. The capability manifest must still identify the actual dedicated
//! headless backend before this domain can be selected.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::browser::{BrowserCheckboxBinding, BrowserReadScope};
use super::device::{PermitRef, SourceFreshness, SurfaceRef};
use super::ids::BindingRef;
use super::require_nonempty;

const MAX_JS_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_BROWSER_OBSERVATION_BYTES: usize = 16 * 1024;
const MAX_BROWSER_INTENT_BYTES: usize = 32 * 1024;
const MAX_BROWSER_OBSERVATION_TTL_MS: u64 = 2_000;
const MAX_BROWSER_DEPENDENCIES: usize = 16;
const MAX_BROWSER_DEPENDENCY_NAME_BYTES: usize = 64;
const MAX_BROWSER_TEXT_BYTES: usize = 256;
const BROWSER_CHECKBOX_NAME: &str = "Background option";

/// The only browser action represented by this contract revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserAction {
    SetCheckbox { checked: bool },
}

impl BrowserAction {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::SetCheckbox { .. } => Ok(()),
        }
    }
}

/// Browser execution evidence assembled from one Environment-admitted read
/// and its exact source-derived checkbox binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserExecutionObservation {
    #[schemars(length(min = 1, max = 256))]
    pub observation_id: String,
    pub surface: SurfaceRef,
    pub scope: Box<BrowserReadScope>,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub environment_revision: u64,
    pub binding_ref: BindingRef,
    pub binding: Box<BrowserCheckboxBinding>,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub valid_until_ms: u64,
    pub freshness: SourceFreshness,
    #[schemars(length(max = 16))]
    pub dependencies: BTreeMap<String, u64>,
}

impl BrowserExecutionObservation {
    pub fn observation_id(&self) -> &str {
        &self.observation_id
    }

    pub fn session_epoch(&self) -> u64 {
        self.scope.context.session_epoch
    }

    pub fn surface(&self) -> &SurfaceRef {
        &self.surface
    }

    /// The Environment-issued live binding ref is the browser target ref.
    pub fn target_ref(&self) -> &str {
        self.binding_ref.as_str()
    }

    pub fn clock_domain(&self) -> &str {
        &self.scope.clock_domain
    }

    pub fn source_sequence(&self) -> u64 {
        self.binding.source_sequence
    }

    pub fn observed_ms(&self) -> u64 {
        self.binding.observed_ms
    }

    pub fn valid_until_ms(&self) -> u64 {
        self.valid_until_ms
    }

    pub fn freshness(&self) -> SourceFreshness {
        self.freshness
    }

    pub fn dependencies(&self) -> &BTreeMap<String, u64> {
        &self.dependencies
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_bounded_text(
            "browser observation_id",
            &self.observation_id,
            MAX_BROWSER_TEXT_BYTES,
        )?;
        self.scope.validate()?;
        self.binding.validate()?;
        if self.environment_revision == 0 || self.environment_revision > MAX_JS_SAFE_INTEGER {
            return Err("browser environment_revision must be positive and JS-safe".into());
        }
        if self.surface.id != self.scope.context.page_target_id
            || self.surface.epoch != self.scope.context.session_epoch
        {
            return Err("browser execution surface must identify its exact page session".into());
        }
        if self.observation_id != self.binding.snapshot_id
            || self.binding.context != self.scope.context
            || self.binding.target_context_hash
                != super::browser::browser_target_context_hash(&self.scope.context)?
            || self.binding.clock_domain != self.scope.clock_domain
            || self.binding.observed_ms < self.scope.issued_ms
        {
            return Err(
                "browser execution observation differs from its installed scope or binding".into(),
            );
        }
        if self.binding.role != "checkbox"
            || self.binding.name != BROWSER_CHECKBOX_NAME
            || self.binding.disabled != super::browser::BrowserObservedBool::False
        {
            return Err("browser execution observation requires the fixed enabled checkbox".into());
        }
        let maximum_valid_until = self
            .binding
            .observed_ms
            .checked_add(MAX_BROWSER_OBSERVATION_TTL_MS)
            .ok_or("browser observation expiry overflows its time range")?;
        if self.valid_until_ms > MAX_JS_SAFE_INTEGER
            || self.valid_until_ms < self.binding.read_finished_ms
            || self.valid_until_ms > maximum_valid_until
        {
            return Err(
                "browser observation validity exceeds its read interval or two-second bound".into(),
            );
        }
        if self.freshness != SourceFreshness::Verified {
            return Err("browser execution requires verified source freshness".into());
        }
        validate_browser_dependencies(&self.dependencies)?;
        let bytes = serde_json::to_vec(self)
            .map_err(|_| "browser execution observation could not be serialized".to_string())?;
        if bytes.len() > MAX_BROWSER_OBSERVATION_BYTES {
            return Err("browser execution observation exceeds 16 KiB".into());
        }
        Ok(())
    }
}

/// One fixed-page checkbox action bound to a live Environment observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserActionIntent {
    #[schemars(length(min = 1, max = 256))]
    pub intent_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub attempt_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub task_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub task_revision: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub root_epoch: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub session_epoch: u64,
    pub permit: PermitRef,
    pub evidence: Box<BrowserExecutionObservation>,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub latest_start_ms: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub duration_ms: u64,
    pub action: BrowserAction,
}

impl BrowserActionIntent {
    pub fn validate(&self) -> Result<(), String> {
        validate_bounded_text("browser intent_id", &self.intent_id, MAX_BROWSER_TEXT_BYTES)?;
        validate_bounded_text(
            "browser attempt_id",
            &self.attempt_id,
            MAX_BROWSER_TEXT_BYTES,
        )?;
        validate_bounded_text("browser task_id", &self.task_id, MAX_BROWSER_TEXT_BYTES)?;
        validate_bounded_text("browser permit.id", &self.permit.id, MAX_BROWSER_TEXT_BYTES)?;
        self.evidence.validate()?;
        if self.task_revision == 0
            || self.task_revision > MAX_JS_SAFE_INTEGER
            || self.root_epoch == 0
            || self.root_epoch > MAX_JS_SAFE_INTEGER
            || self.session_epoch == 0
            || self.session_epoch > MAX_JS_SAFE_INTEGER
            || self.session_epoch != self.evidence.session_epoch()
            || self.permit.generation == 0
            || self.permit.generation > MAX_JS_SAFE_INTEGER
            || self.latest_start_ms > MAX_JS_SAFE_INTEGER
            || self.duration_ms == 0
            || self.duration_ms > MAX_JS_SAFE_INTEGER
        {
            return Err(
                "browser intent metadata must be positive, JS-safe, and match its session".into(),
            );
        }
        let action_end = self
            .latest_start_ms
            .checked_add(self.duration_ms)
            .filter(|end| *end <= MAX_JS_SAFE_INTEGER)
            .ok_or("browser intent end exceeds the JS-safe time range")?;
        if self.latest_start_ms < self.evidence.binding.read_finished_ms
            || action_end > self.evidence.valid_until_ms
        {
            return Err(
                "browser action window must fit after its evidence read and before expiry".into(),
            );
        }
        self.action.validate()?;
        if matches!(
            self.action,
            BrowserAction::SetCheckbox { checked } if checked == self.evidence.binding.checked
        ) {
            return Err("browser checkbox action must change the observed value".into());
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|_| "browser action intent could not be serialized".to_string())?;
        if bytes.len() > MAX_BROWSER_INTENT_BYTES {
            return Err("browser action intent exceeds 32 KiB".into());
        }
        Ok(())
    }

    pub fn validate_against_observation(
        &self,
        observation: &BrowserExecutionObservation,
    ) -> Result<(), String> {
        self.validate()?;
        observation.validate()?;
        if self.evidence.as_ref() != observation {
            return Err("browser intent does not consume its exact execution observation".into());
        }
        Ok(())
    }
}

fn validate_browser_dependencies(dependencies: &BTreeMap<String, u64>) -> Result<(), String> {
    if dependencies.len() > MAX_BROWSER_DEPENDENCIES {
        return Err("browser execution dependencies exceed their count bound".into());
    }
    for (name, revision) in dependencies {
        validate_bounded_text(
            "browser dependency name",
            name,
            MAX_BROWSER_DEPENDENCY_NAME_BYTES,
        )?;
        if *revision > MAX_JS_SAFE_INTEGER {
            return Err("browser dependency revision is not a JS-safe integer".into());
        }
    }
    Ok(())
}

fn validate_bounded_text(field: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    require_nonempty(field, value)?;
    if value.len() > max_bytes {
        return Err(format!("{field} exceeds its UTF-8 byte bound"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::browser::{
        BrowserObservedBool, browser_contract_golden_snapshots, derive_browser_checkbox_binding,
    };
    use crate::desktop::device::{ExecutionIntent, ExecutionObservation};

    fn observation() -> BrowserExecutionObservation {
        let mut snapshots = browser_contract_golden_snapshots().unwrap();
        let mut snapshot = snapshots.remove(0);
        // Keep the readonly CDP golden unchanged; this execution unit test
        // supplies an explicit synthetic enabled-state observation.
        let checkbox = snapshot
            .nodes
            .iter_mut()
            .find(|node| node.ax_node_id == "checkbox-ax")
            .unwrap();
        checkbox.disabled = BrowserObservedBool::False;
        snapshot.validate().unwrap();
        let binding = derive_browser_checkbox_binding(&snapshot, BROWSER_CHECKBOX_NAME).unwrap();
        let scope = BrowserReadScope {
            scope_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            generation: 1,
            context: snapshot.context.clone(),
            clock_domain: snapshot.clock_domain,
            issued_ms: snapshot.observed_ms,
        };
        BrowserExecutionObservation {
            observation_id: snapshot.snapshot_id,
            surface: SurfaceRef {
                id: snapshot.context.page_target_id.clone(),
                epoch: snapshot.context.session_epoch,
            },
            scope: Box::new(scope),
            environment_revision: 1,
            binding_ref: BindingRef::new("browser-binding-1").unwrap(),
            binding: Box::new(binding),
            valid_until_ms: snapshot.observed_ms + MAX_BROWSER_OBSERVATION_TTL_MS,
            freshness: SourceFreshness::Verified,
            dependencies: BTreeMap::from([("browser_scope".into(), 1)]),
        }
    }

    fn intent(evidence: BrowserExecutionObservation) -> BrowserActionIntent {
        BrowserActionIntent {
            intent_id: "browser-intent-1".into(),
            attempt_id: "attempt-1".into(),
            task_id: "task-1".into(),
            task_revision: 1,
            root_epoch: 1,
            session_epoch: evidence.session_epoch(),
            permit: PermitRef {
                id: "permit-1".into(),
                generation: 1,
            },
            latest_start_ms: evidence.binding.read_finished_ms,
            duration_ms: 10,
            evidence: Box::new(evidence),
            action: BrowserAction::SetCheckbox { checked: true },
        }
    }

    #[test]
    fn browser_execution_observation_and_intent_remain_exactly_bound() {
        let evidence = observation();
        evidence.validate().unwrap();
        assert_eq!(evidence.target_ref(), "browser-binding-1");
        assert_eq!(evidence.surface().id, evidence.scope.context.page_target_id);

        let action_intent = intent(evidence.clone());
        action_intent.validate().unwrap();
        action_intent
            .validate_against_observation(&evidence)
            .unwrap();
        assert_eq!(
            ExecutionIntent::from(action_intent.clone())
                .validate_against_observation(&ExecutionObservation::from(evidence.clone())),
            Ok(())
        );

        let mut stale = action_intent;
        stale.latest_start_ms -= 1;
        assert!(stale.validate().is_err());
        let mut unchanged = intent(evidence);
        unchanged.action = BrowserAction::SetCheckbox { checked: false };
        assert!(unchanged.validate().is_err());
    }

    #[test]
    fn browser_action_wire_shape_is_closed_and_tagged() {
        let action = BrowserAction::SetCheckbox { checked: true };
        assert_eq!(
            serde_json::to_value(action).unwrap(),
            serde_json::json!({"type": "set_checkbox", "checked": true})
        );
        assert!(
            serde_json::from_value::<BrowserAction>(serde_json::json!({
                "type": "set_checkbox",
                "checked": true,
                "window_id": "forbidden-native-field"
            }))
            .is_err()
        );
    }
}
