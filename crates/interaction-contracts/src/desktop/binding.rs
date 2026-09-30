//! Binding a semantic target to one live instance (design 6.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::FailureCode;
use super::ids::{BindingRef, ObjectRef, RequestId, SemanticTargetRef};

/// Everything that identifies one instance across observations. It is a
/// continuity check and a pre-submit proof, not a long-lived identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceFingerprint {
    pub app: String,
    pub window_id: String,
    pub ancestor_roles: Vec<String>,
    pub role: String,
    pub subrole: Option<String>,
    /// Normalised title or description.
    pub label: String,
    pub sibling_index: u32,
    /// Frame in display points, rounded to whole points.
    pub frame: [i64; 4],
    pub scroll_container: Option<String>,
}

impl InstanceFingerprint {
    /// A stable digest. Field order is fixed by the struct, so the same
    /// fingerprint always yields the same string on the host and the device.
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_vec(self).expect("a fingerprint always serialises");
        let hash = Sha256::digest(&canonical);
        let mut out = String::from("fp1:");
        for byte in hash {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

/// Lower-case, collapse whitespace: the text form used in labels and matching.
pub fn normalize_label(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// How a task describes the object it wants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetDescription {
    #[schemars(length(min = 1))]
    pub label: String,
    pub group: Option<String>,
    pub role_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindScope {
    pub app: String,
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindRequest {
    pub request_id: RequestId,
    pub semantic_target: SemanticTargetRef,
    pub description: TargetDescription,
    pub scope: BindScope,
    /// The pick Reaction made among candidates of an earlier ambiguous answer.
    pub choice: Option<ObjectRef>,
    /// Oldest evidence the caller accepts.
    pub max_age_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub snapshot_id: RequestId,
    pub environment_revision: u64,
    pub observed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindCandidate {
    pub instance: ObjectRef,
    pub role: String,
    pub label: String,
    pub group: Option<String>,
    pub score_milli: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum BindingOutcome {
    Bound {
        binding: BindingRef,
        instance: ObjectRef,
        fingerprint_digest: String,
        evidence: EvidenceRef,
        active_layer_revision: u64,
        rule: String,
    },
    /// Several instances qualify. Reaction observes further or asks S1.
    Ambiguous {
        candidates: Vec<BindCandidate>,
    },
    /// Nothing qualified. `observation_complete` separates "the scope was
    /// fully observed and it is not there" from "we could not see everything".
    NotFound {
        observation_complete: bool,
    },
    Rejected {
        code: FailureCode,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindingResult {
    pub request_id: RequestId,
    pub outcome: BindingOutcome,
    pub environment_revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint() -> InstanceFingerprint {
        InstanceFingerprint {
            app: "test.app".into(),
            window_id: "w1".into(),
            ancestor_roles: vec!["window".into(), "group".into()],
            role: "switch".into(),
            subrole: None,
            label: "daily picks".into(),
            sibling_index: 2,
            frame: [10, 20, 40, 20],
            scroll_container: None,
        }
    }

    #[test]
    fn digest_is_stable_and_sensitive_to_every_field() {
        let base = fingerprint();
        assert_eq!(base.digest(), base.clone().digest());
        assert!(base.digest().starts_with("fp1:"));
        assert_eq!(base.digest().len(), 4 + 64);

        let mut moved = base.clone();
        moved.frame = [10, 21, 40, 20];
        assert_ne!(
            base.digest(),
            moved.digest(),
            "a moved element is a different proof"
        );
        let mut relabelled = base.clone();
        relabelled.label = "weekly picks".into();
        assert_ne!(base.digest(), relabelled.digest());
        let mut regrouped = base.clone();
        regrouped.ancestor_roles = vec!["window".into()];
        assert_ne!(base.digest(), regrouped.digest());
    }

    #[test]
    fn labels_are_normalised_for_matching() {
        assert_eq!(normalize_label("  Daily   Picks \n"), "daily picks");
        assert_eq!(normalize_label("每日  推荐"), "每日 推荐");
    }
}
