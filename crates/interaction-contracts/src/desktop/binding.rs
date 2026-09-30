//! Binding a semantic target to one live instance (design 6.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::FailureCode;
use super::ids::{BindingRef, ObjectRef, RequestId, SemanticTargetRef};
use super::source::{StructureSnapshot, UiObject};

/// Everything that identifies one instance across observations. It is a
/// continuity check and a pre-submit proof, not a long-lived identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceFingerprint {
    pub app: String,
    pub window_id: String,
    pub ancestor_roles: Vec<String>,
    /// Normalised title of the nearest labelled group, so the same label in two
    /// groups never shares a fingerprint even when layouts are reused.
    pub group: Option<String>,
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

/// The fingerprint of one object as it appears in one snapshot.
///
/// The host computes it when binding and a device backend computes it for
/// whatever is under the pointer, so both must use exactly this function.
pub fn instance_fingerprint(
    snapshot: &StructureSnapshot,
    object: &UiObject,
) -> InstanceFingerprint {
    let mut ancestor_roles = Vec::new();
    let mut cursor = object.parent.as_ref();
    while let Some(parent_ref) = cursor {
        let Some(parent) = snapshot
            .objects
            .iter()
            .find(|candidate| &candidate.instance_ref == parent_ref)
        else {
            break;
        };
        ancestor_roles.push(parent.role.clone());
        cursor = parent.parent.as_ref();
        if ancestor_roles.len() > 32 {
            break;
        }
    }
    ancestor_roles.reverse();
    let sibling_index = snapshot
        .objects
        .iter()
        .filter(|candidate| {
            candidate.parent == object.parent && candidate.window_id == object.window_id
        })
        .position(|candidate| candidate.instance_ref == object.instance_ref)
        .unwrap_or(0) as u32;
    let label = object
        .title
        .as_deref()
        .or(object.description.as_deref())
        .unwrap_or("");
    InstanceFingerprint {
        app: snapshot.app.bundle_id.clone(),
        window_id: object.window_id.clone(),
        ancestor_roles,
        group: object.group.as_deref().map(normalize_label),
        role: object.role.clone(),
        subrole: object.subrole.clone(),
        label: normalize_label(label),
        sibling_index,
        frame: [
            object.frame.x.round() as i64,
            object.frame.y.round() as i64,
            object.frame.width.round() as i64,
            object.frame.height.round() as i64,
        ],
        scroll_container: None,
    }
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
            group: Some("push".into()),
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

    fn object(reference: &str, parent: Option<&str>, role: &str, title: &str) -> UiObject {
        use super::super::source::Rect;
        UiObject {
            instance_ref: ObjectRef::new(reference).unwrap(),
            parent: parent.map(|p| ObjectRef::new(p).unwrap()),
            window_id: "w1".into(),
            role: role.into(),
            subrole: None,
            title: Some(title.into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: Rect {
                x: 10.4,
                y: 20.6,
                width: 40.0,
                height: 20.0,
            },
            actions: vec![],
            in_view: true,
            group: None,
            secure: false,
        }
    }

    fn snapshot(objects: Vec<UiObject>) -> StructureSnapshot {
        use super::super::device::SurfaceRef;
        use super::super::source::{AppIdentity, LayerKind, Rect, WindowInfo};
        StructureSnapshot {
            snapshot_id: RequestId::new("s").unwrap(),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "sim/1/monotonic".into(),
            source_sequence: 1,
            observed_ms: 1,
            app: AppIdentity {
                bundle_id: "test.app".into(),
                name: "T".into(),
                pid: 1,
            },
            windows: vec![WindowInfo {
                window_id: "w1".into(),
                title: None,
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                z_index: 0,
                kind: LayerKind::Window,
                modal: false,
                owner_bundle_id: "test.app".into(),
            }],
            active_layer: Some("w1".into()),
            complete: true,
            incomplete_reason: None,
            objects,
        }
    }

    #[test]
    fn an_objects_fingerprint_follows_its_ancestors_label_and_position() {
        let snap = snapshot(vec![
            object("g", None, "group", "Push"),
            object("a", Some("g"), "switch", "  Daily   Picks "),
            object("b", Some("g"), "switch", "Weekly"),
        ]);
        let fp = instance_fingerprint(&snap, &snap.objects[1]);
        assert_eq!(fp.ancestor_roles, vec!["group".to_string()]);
        assert_eq!(fp.label, "daily picks");
        assert_eq!(fp.sibling_index, 0);
        assert_eq!(
            fp.frame,
            [10, 21, 40, 20],
            "frames are rounded to whole points"
        );
        assert_eq!(
            instance_fingerprint(&snap, &snap.objects[2]).sibling_index,
            1
        );
        assert_ne!(
            instance_fingerprint(&snap, &snap.objects[1]).digest(),
            instance_fingerprint(&snap, &snap.objects[2]).digest()
        );
    }

    #[test]
    fn the_same_label_under_different_groups_has_different_fingerprints() {
        let mut first = object("a", Some("g1"), "switch", "Daily picks");
        first.group = Some("Push".into());
        let mut second = object("b", Some("g2"), "switch", "Daily picks");
        second.group = Some("Email".into());
        let snap = snapshot(vec![
            object("g1", None, "group", "Push"),
            object("g2", None, "group", "Email"),
            first,
            second,
        ]);
        let push = instance_fingerprint(&snap, &snap.objects[2]);
        let email = instance_fingerprint(&snap, &snap.objects[3]);
        assert_eq!(
            push.frame, email.frame,
            "a reused layout puts them at the same place"
        );
        assert_eq!(push.sibling_index, email.sibling_index);
        assert_ne!(
            push.digest(),
            email.digest(),
            "only the group tells them apart"
        );
    }

    #[test]
    fn labels_are_normalised_for_matching() {
        assert_eq!(normalize_label("  Daily   Picks \n"), "daily picks");
        assert_eq!(normalize_label("每日  推荐"), "每日 推荐");
    }
}
