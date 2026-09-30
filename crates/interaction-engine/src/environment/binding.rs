//! BindingReducer: the only writer of bindings (design 6.7).
//!
//! A binding ties a semantic target to one live instance, proven by a
//! fingerprint. It is rebuilt from fresh evidence, never carried over from an
//! old one: any change of layer, page, instance or geometry drops it.

use std::collections::BTreeMap;

use interaction_contracts::desktop::binding::{
    BindCandidate, BindRequest, BindingOutcome, BindingResult, EvidenceRef, instance_fingerprint,
    normalize_label,
};
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::ids::{BindingRef, ObjectRef, RequestId, SemanticTargetRef};
use interaction_contracts::desktop::source::UiObject;

use super::state_core::EnvSnapshot;

/// The score gap that lets the reducer bind without asking anyone.
const DEFAULT_MARGIN_MILLI: u32 = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub binding_ref: BindingRef,
    pub semantic_target: SemanticTargetRef,
    pub instance: ObjectRef,
    pub fingerprint_digest: String,
    pub layer_revision: u64,
    pub env_revision: u64,
    pub snapshot_id: RequestId,
    pub observed_ms: u64,
    pub page: Option<String>,
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalidated {
    pub binding: BindingRef,
    pub semantic_target: SemanticTargetRef,
    pub reason: String,
}

#[derive(Debug)]
pub struct BindingReducer {
    bindings: BTreeMap<String, Binding>,
    next: u64,
    margin_milli: u32,
}

impl Default for BindingReducer {
    fn default() -> Self {
        Self::new()
    }
}

struct Scored<'a> {
    object: &'a UiObject,
    score_milli: u32,
}

fn label_of(object: &UiObject) -> String {
    normalize_label(
        object
            .title
            .as_deref()
            .or(object.description.as_deref())
            .unwrap_or(""),
    )
}

impl BindingReducer {
    pub fn new() -> Self {
        Self {
            bindings: BTreeMap::new(),
            next: 1,
            margin_milli: DEFAULT_MARGIN_MILLI,
        }
    }

    pub fn current(&self, target: &SemanticTargetRef) -> Option<&Binding> {
        self.bindings.get(target.as_str())
    }

    pub fn all(&self) -> impl Iterator<Item = &Binding> {
        self.bindings.values()
    }

    fn reject(
        request: &BindRequest,
        env: &EnvSnapshot,
        code: FailureCode,
        reason: &str,
    ) -> BindingResult {
        BindingResult {
            request_id: request.request_id.clone(),
            outcome: BindingOutcome::Rejected {
                code,
                reason: reason.into(),
            },
            environment_revision: env.revision,
        }
    }

    /// Steps 1 to 3 of 6.7: recall, deterministic screening and scoring.
    fn candidates<'a>(env: &'a EnvSnapshot, request: &BindRequest) -> Vec<Scored<'a>> {
        let Some(active) = env.snapshot.active_layer.as_deref() else {
            return Vec::new();
        };
        let want = normalize_label(&request.description.label);
        let want_group = request.description.group.as_deref().map(normalize_label);
        let mut scored = Vec::new();
        for object in &env.snapshot.objects {
            // Only what is in the window that receives input can be acted on.
            if object.window_id != active || !object.enabled || !object.in_view || object.secure {
                continue;
            }
            let label = label_of(object);
            if label.is_empty() {
                continue;
            }
            let mut score = if label == want {
                1000
            } else if label.contains(&want) || want.contains(&label) {
                600
            } else {
                continue;
            };
            if let Some(role) = request.description.role_hint.as_deref() {
                if object.role != role {
                    continue;
                }
                score += 100;
            }
            if let Some(group) = &want_group {
                if object.group.as_deref().map(normalize_label).as_ref() != Some(group) {
                    continue;
                }
                score += 200;
            }
            scored.push(Scored {
                object,
                score_milli: score,
            });
        }
        scored.sort_by(|a, b| {
            b.score_milli
                .cmp(&a.score_milli)
                .then_with(|| a.object.instance_ref.cmp(&b.object.instance_ref))
        });
        scored
    }

    fn summarize(scored: &[Scored<'_>]) -> Vec<BindCandidate> {
        scored
            .iter()
            .map(|entry| BindCandidate {
                instance: entry.object.instance_ref.clone(),
                role: entry.object.role.clone(),
                label: entry.object.title.clone().unwrap_or_default(),
                group: entry.object.group.clone(),
                score_milli: entry.score_milli,
            })
            .collect()
    }

    fn create(
        &mut self,
        env: &EnvSnapshot,
        request: &BindRequest,
        object: &UiObject,
        rule: &str,
    ) -> BindingResult {
        let digest = instance_fingerprint(&env.snapshot, object).digest();
        let binding_ref =
            BindingRef::new(format!("bind-{}", self.next)).expect("a counter is a valid id");
        self.next += 1;
        let binding = Binding {
            binding_ref: binding_ref.clone(),
            semantic_target: request.semantic_target.clone(),
            instance: object.instance_ref.clone(),
            fingerprint_digest: digest.clone(),
            layer_revision: env.layer_revision,
            env_revision: env.revision,
            snapshot_id: env.snapshot.snapshot_id.clone(),
            observed_ms: env.snapshot.observed_ms,
            page: env.page.clone(),
            rule: rule.into(),
        };
        self.bindings
            .insert(request.semantic_target.as_str().to_string(), binding);
        BindingResult {
            request_id: request.request_id.clone(),
            outcome: BindingOutcome::Bound {
                binding: binding_ref,
                instance: object.instance_ref.clone(),
                fingerprint_digest: digest,
                evidence: EvidenceRef {
                    snapshot_id: env.snapshot.snapshot_id.clone(),
                    environment_revision: env.revision,
                    observed_ms: env.snapshot.observed_ms,
                },
                active_layer_revision: env.layer_revision,
                rule: rule.into(),
            },
            environment_revision: env.revision,
        }
    }

    pub fn bind(&mut self, env: &EnvSnapshot, request: &BindRequest, now_ms: u64) -> BindingResult {
        if now_ms.saturating_sub(env.snapshot.observed_ms) > request.max_age_ms {
            return Self::reject(
                request,
                env,
                FailureCode::EvidenceExpired,
                "the evidence is too old to bind from",
            );
        }
        if env.snapshot.active_layer.is_none() {
            return Self::reject(
                request,
                env,
                FailureCode::ContextUnresolved,
                "the active layer is unknown",
            );
        }
        if !env.snapshot.complete {
            // Uniqueness cannot be claimed over a read that was cut short.
            return Self::reject(
                request,
                env,
                FailureCode::ContextUnresolved,
                "the structure read is incomplete",
            );
        }
        let scored = Self::candidates(env, request);

        if let Some(choice) = &request.choice {
            return match scored
                .iter()
                .find(|entry| &entry.object.instance_ref == choice)
            {
                Some(entry) => {
                    let object = entry.object;
                    self.create(env, request, object, "choice")
                }
                None => Self::reject(
                    request,
                    env,
                    FailureCode::TargetAmbiguous,
                    "the chosen instance is no longer a candidate",
                ),
            };
        }
        match scored.as_slice() {
            [] => BindingResult {
                request_id: request.request_id.clone(),
                outcome: BindingOutcome::NotFound {
                    observation_complete: env.snapshot.complete,
                },
                environment_revision: env.revision,
            },
            [only] => {
                let object = only.object;
                self.create(env, request, object, "unique")
            }
            [best, second, ..] if best.score_milli >= second.score_milli + self.margin_milli => {
                let object = best.object;
                self.create(env, request, object, "margin")
            }
            _ => BindingResult {
                request_id: request.request_id.clone(),
                outcome: BindingOutcome::Ambiguous {
                    candidates: Self::summarize(&scored),
                },
                environment_revision: env.revision,
            },
        }
    }

    /// Bind straight to a known instance, for controls that are not the
    /// task's target, such as a button in a dialog.
    pub fn bind_instance(
        &mut self,
        env: &EnvSnapshot,
        target: SemanticTargetRef,
        instance: &ObjectRef,
    ) -> Option<Binding> {
        let object = env
            .snapshot
            .objects
            .iter()
            .find(|object| &object.instance_ref == instance)?;
        let request = BindRequest {
            request_id: RequestId::new(format!("bind-instance-{}", self.next)).ok()?,
            semantic_target: target.clone(),
            description: interaction_contracts::desktop::binding::TargetDescription {
                label: object.title.clone().unwrap_or_else(|| "control".into()),
                group: None,
                role_hint: None,
            },
            scope: interaction_contracts::desktop::binding::BindScope {
                app: env.snapshot.app.bundle_id.clone(),
                account: None,
            },
            choice: None,
            max_age_ms: u64::MAX,
        };
        self.create(env, &request, object, "instance");
        self.bindings.get(target.as_str()).cloned()
    }

    /// Check every binding against new evidence and drop the ones that no
    /// longer hold. The caller tells the device about each.
    pub fn revalidate(&mut self, env: &EnvSnapshot) -> Vec<Invalidated> {
        let mut dropped = Vec::new();
        let targets: Vec<String> = self.bindings.keys().cloned().collect();
        for key in targets {
            let binding = &self.bindings[&key];
            let reason = if env.snapshot.active_layer.is_none() {
                Some("the active layer is unknown")
            } else if binding.layer_revision != env.layer_revision {
                Some("the active layer changed")
            } else if binding.page != env.page {
                Some("the page changed")
            } else if !env.snapshot.complete {
                Some("the structure read is incomplete")
            } else if !self.instance_still_there(env, &binding.fingerprint_digest) {
                Some("the instance moved, changed or disappeared")
            } else {
                None
            };
            if let Some(reason) = reason {
                let binding = self.bindings.remove(&key).expect("listed above");
                dropped.push(Invalidated {
                    binding: binding.binding_ref,
                    semantic_target: binding.semantic_target,
                    reason: reason.into(),
                });
            }
        }
        dropped
    }

    fn instance_still_there(&self, env: &EnvSnapshot, digest: &str) -> bool {
        let Some(active) = env.snapshot.active_layer.as_deref() else {
            return false;
        };
        env.snapshot.objects.iter().any(|object| {
            object.window_id == active
                && instance_fingerprint(&env.snapshot, object).digest() == digest
        })
    }

    pub fn invalidate_all(&mut self, reason: &str) -> Vec<Invalidated> {
        std::mem::take(&mut self.bindings)
            .into_values()
            .map(|binding| Invalidated {
                binding: binding.binding_ref,
                semantic_target: binding.semantic_target,
                reason: reason.into(),
            })
            .collect()
    }

    pub fn invalidate(&mut self, target: &SemanticTargetRef, reason: &str) -> Option<Invalidated> {
        self.bindings
            .remove(target.as_str())
            .map(|binding| Invalidated {
                binding: binding.binding_ref,
                semantic_target: binding.semantic_target,
                reason: reason.into(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::pack::AppPack;
    use crate::environment::state_core::StateCore;
    use interaction_contracts::desktop::binding::{BindScope, TargetDescription};
    use interaction_contracts::desktop::device::SurfaceRef;
    use sim_world::app::{Applied, Modal, SimApp, SnapshotMeta};

    fn meta(sequence: u64) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: format!("snap-{sequence}"),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "sim/1/monotonic".into(),
            source_sequence: sequence,
            observed_ms: sequence * 10,
        }
    }

    fn observe(core: &mut StateCore, app: &SimApp, sequence: u64) -> EnvSnapshot {
        core.admit(
            app.snapshot(&meta(sequence)),
            sequence * 10,
            &AppPack::settings_fixture(),
        )
        .unwrap();
        core.latest().unwrap().clone()
    }

    fn request(label: &str, group: Option<&str>, role: Option<&str>) -> BindRequest {
        BindRequest {
            request_id: RequestId::new("bind-req").unwrap(),
            semantic_target: SemanticTargetRef::new("target").unwrap(),
            description: TargetDescription {
                label: label.into(),
                group: group.map(String::from),
                role_hint: role.map(String::from),
            },
            scope: BindScope {
                app: "test.settings".into(),
                account: None,
            },
            choice: None,
            max_age_ms: 1_000,
        }
    }

    fn setup() -> (StateCore, SimApp, BindingReducer) {
        let mut core = StateCore::new();
        core.set_session(1);
        (core, SimApp::new(), BindingReducer::new())
    }

    #[test]
    fn a_group_in_the_request_disambiguates_the_same_label() {
        let (mut core, app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        let result = reducer.bind(
            &env,
            &request("Daily picks", Some("Push"), Some("switch")),
            10,
        );
        let BindingOutcome::Bound { instance, rule, .. } = result.outcome else {
            panic!("{:?}", result.outcome)
        };
        assert_eq!(instance.as_str(), "sw-push-daily");
        assert_eq!(rule, "unique");

        let result = reducer.bind(
            &env,
            &request("Daily picks", Some("Email"), Some("switch")),
            10,
        );
        let BindingOutcome::Bound { instance, .. } = result.outcome else {
            panic!()
        };
        assert_eq!(instance.as_str(), "sw-email-daily");
    }

    #[test]
    fn the_same_label_without_a_group_is_ambiguous_and_lists_both() {
        let (mut core, app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        let result = reducer.bind(&env, &request("Daily picks", None, Some("switch")), 10);
        let BindingOutcome::Ambiguous { candidates } = result.outcome else {
            panic!("{:?}", result.outcome)
        };
        let groups: Vec<_> = candidates
            .iter()
            .map(|c| c.group.clone().unwrap())
            .collect();
        assert_eq!(
            groups,
            vec!["Email", "Push"],
            "ties break by instance id, deterministically"
        );
        assert!(
            reducer
                .current(&SemanticTargetRef::new("target").unwrap())
                .is_none(),
            "nothing is bound yet"
        );
    }

    #[test]
    fn a_choice_among_ambiguous_candidates_is_bound_only_if_it_still_qualifies() {
        let (mut core, app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        let mut pick = request("Daily picks", None, Some("switch"));
        pick.choice = Some(ObjectRef::new("sw-email-daily").unwrap());
        let BindingOutcome::Bound { instance, rule, .. } = reducer.bind(&env, &pick, 10).outcome
        else {
            panic!()
        };
        assert_eq!(
            (instance.as_str(), rule.as_str()),
            ("sw-email-daily", "choice")
        );

        pick.choice = Some(ObjectRef::new("sw-push-weekly").unwrap());
        assert!(matches!(
            reducer.bind(&env, &pick, 10).outcome,
            BindingOutcome::Rejected {
                code: FailureCode::TargetAmbiguous,
                ..
            }
        ));
    }

    #[test]
    fn a_clearly_better_match_binds_without_asking() {
        let (mut core, app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        // "Promotions" matches exactly once; "digest" is a substring hit on one object only.
        let BindingOutcome::Bound { instance, .. } = reducer
            .bind(&env, &request("Promotions", None, None), 10)
            .outcome
        else {
            panic!()
        };
        assert_eq!(instance.as_str(), "sw-email-promo");
        let BindingOutcome::Bound { instance, .. } = reducer
            .bind(&env, &request("digest", None, None), 10)
            .outcome
        else {
            panic!()
        };
        assert_eq!(instance.as_str(), "sw-push-weekly");
    }

    #[test]
    fn nothing_matching_is_not_found_and_says_whether_the_view_was_complete() {
        let (mut core, app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        assert_eq!(
            reducer
                .bind(&env, &request("Nonexistent", None, None), 10)
                .outcome,
            BindingOutcome::NotFound {
                observation_complete: true
            }
        );
        // Wrong role hint: the label matches but the control is not what was asked for.
        assert!(matches!(
            reducer
                .bind(
                    &env,
                    &request("Daily picks", Some("Push"), Some("button")),
                    10
                )
                .outcome,
            BindingOutcome::NotFound { .. }
        ));
    }

    #[test]
    fn evidence_that_is_old_incomplete_or_without_a_layer_cannot_bind() {
        let (mut core, app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        assert!(matches!(
            reducer
                .bind(&env, &request("Daily picks", Some("Push"), None), 10_000)
                .outcome,
            BindingOutcome::Rejected {
                code: FailureCode::EvidenceExpired,
                ..
            }
        ));
        let mut partial = env.clone();
        partial.snapshot.complete = false;
        partial.snapshot.incomplete_reason = Some("timeout".into());
        assert!(matches!(
            reducer
                .bind(&partial, &request("Daily picks", Some("Push"), None), 10)
                .outcome,
            BindingOutcome::Rejected {
                code: FailureCode::ContextUnresolved,
                ..
            }
        ));
        let mut no_layer = env;
        no_layer.snapshot.active_layer = None;
        assert!(matches!(
            reducer
                .bind(&no_layer, &request("Daily picks", Some("Push"), None), 10)
                .outcome,
            BindingOutcome::Rejected {
                code: FailureCode::ContextUnresolved,
                ..
            }
        ));
    }

    #[test]
    fn disabled_hidden_and_secure_controls_are_never_candidates() {
        let (mut core, app, mut reducer) = setup();
        let mut env = observe(&mut core, &app, 1);
        for object in &mut env.snapshot.objects {
            if object.instance_ref.as_str() == "sw-push-daily" {
                object.enabled = false;
            }
            if object.instance_ref.as_str() == "sw-email-daily" {
                object.in_view = false;
            }
        }
        assert!(matches!(
            reducer
                .bind(&env, &request("Daily picks", None, Some("switch")), 10)
                .outcome,
            BindingOutcome::NotFound { .. }
        ));
    }

    #[test]
    fn a_binding_survives_a_value_change_but_not_a_layer_page_or_geometry_change() {
        let (mut core, mut app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        reducer.bind(
            &env,
            &request("Daily picks", Some("Push"), Some("switch")),
            10,
        );
        let target = SemanticTargetRef::new("target").unwrap();

        // Toggling the switch changes its value, not what it is.
        let proof = reducer.current(&target).unwrap().fingerprint_digest.clone();
        app.apply(
            &meta(2),
            &Applied {
                primitive: interaction_contracts::desktop::primitive::Primitive::AxAction {
                    action: interaction_contracts::desktop::primitive::AxActionKind::SetValue {
                        value: "off".into(),
                    },
                },
                binding_fingerprint: Some(proof),
            },
        );
        let env = observe(&mut core, &app, 3);
        assert!(reducer.revalidate(&env).is_empty());
        assert!(reducer.current(&target).is_some());

        // A sheet in front changes the layer, even though the switch is untouched.
        app.inject_modal(Modal::UnsavedChanges);
        let env = observe(&mut core, &app, 4);
        let dropped = reducer.revalidate(&env);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].reason, "the active layer changed");
        assert!(reducer.current(&target).is_none());
    }

    #[test]
    fn moving_the_element_drops_the_binding() {
        let (mut core, app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        reducer.bind(
            &env,
            &request("Daily picks", Some("Push"), Some("switch")),
            10,
        );
        let mut env = observe(&mut core, &app, 2);
        for object in &mut env.snapshot.objects {
            if object.instance_ref.as_str() == "sw-push-daily" {
                object.frame.y += 5.0;
            }
        }
        let dropped = reducer.revalidate(&env);
        assert_eq!(dropped.len(), 1);
        assert_eq!(
            dropped[0].reason,
            "the instance moved, changed or disappeared"
        );
    }

    #[test]
    fn binding_straight_to_an_instance_records_a_binding_for_it() {
        let (mut core, app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        let target = SemanticTargetRef::new("dialog-apply").unwrap();
        let binding = reducer
            .bind_instance(&env, target.clone(), &ObjectRef::new("btn-apply").unwrap())
            .unwrap();
        assert_eq!(binding.rule, "instance");
        assert_eq!(
            reducer.current(&target).unwrap().instance.as_str(),
            "btn-apply"
        );
        assert!(
            reducer
                .bind_instance(&env, target, &ObjectRef::new("nope").unwrap())
                .is_none()
        );
    }

    #[test]
    fn invalidate_all_clears_everything_with_a_reason() {
        let (mut core, app, mut reducer) = setup();
        let env = observe(&mut core, &app, 1);
        reducer.bind(&env, &request("Daily picks", Some("Push"), None), 10);
        let dropped = reducer.invalidate_all("new session");
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].reason, "new session");
        assert_eq!(reducer.all().count(), 0);
    }
}
