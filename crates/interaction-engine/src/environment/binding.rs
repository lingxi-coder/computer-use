//! BindingReducer: the only writer of bindings (design 6.7).
//!
//! A binding ties a semantic target to one live instance, proven by a
//! fingerprint. It is rebuilt from fresh evidence, never carried over from an
//! old one: any change of layer, page, instance or geometry drops it.

use std::collections::BTreeMap;

use interaction_contracts::desktop::binding::{
    BindCandidate, BindRequest, BindingContinuityProof, BindingOutcome, BindingProofScope,
    BindingResult, DeviceContextPoint, DeviceContextWitness, EvidenceRef,
    derive_background_ax_binding_evidence, derive_binding_evidence,
    derive_row_selection_binding_evidence, derive_window_root_binding_evidence,
    instance_fingerprint, normalize_label,
};
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::ids::{BindingRef, ObjectRef, RequestId, SemanticTargetRef};
use interaction_contracts::desktop::row::{RowSelectionObservation, RowSelector};
use interaction_contracts::desktop::source::{
    ObjectIdentityContinuity, Rect, SourceIdentityBoundary, SourceState, StructureSnapshot,
    TargetContext, UiObject,
};
use interaction_contracts::desktop::task::{RowTableScope, TargetSpec};

use super::read::focused_window_id;
use super::row::{RowResolutionFailure, resolve_row_target};
use super::state_core::EnvSnapshot;

/// The score gap that lets the reducer bind without asking anyone.
const DEFAULT_MARGIN_MILLI: u32 = 300;

#[derive(Debug, Clone, PartialEq)]
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
    pub continuity: BindingContinuityProof,
    pub device_context: DeviceContextWitness,
    pub identity_boundary: Option<SourceIdentityBoundary>,
}

/// Match a newly derived BackgroundAX binding to the semantic target that
/// received a known completed input. This is read-only evidence: it does not
/// claim that a read-scoped AX element is the same native entity across reads.
pub fn background_readback_matches(
    input: &Binding,
    current: &Binding,
    snapshot: &StructureSnapshot,
    input_source_sequence: u64,
) -> bool {
    if input.rule != "background_ax"
        || current.rule != "background_ax"
        || input.semantic_target != current.semantic_target
        || input.identity_boundary != current.identity_boundary
        || current.identity_boundary != snapshot.identity_boundary
        || input_source_sequence == 0
        || input.continuity.source_sequence != input_source_sequence
        || snapshot.source_sequence <= input_source_sequence
        || current.snapshot_id != snapshot.snapshot_id
        || current.observed_ms != snapshot.observed_ms
        || current.continuity.source_sequence != snapshot.source_sequence
        || input.continuity.kind != current.continuity.kind
        || input.continuity.validate().is_err()
        || current.continuity.validate().is_err()
        || !input.device_context.supports_background_ax_press()
        || !current.device_context.supports_background_ax_press()
        || !snapshot.complete
        || snapshot.menu_context.is_some()
        || snapshot.observer_coverage.state == SourceState::Unavailable
        || snapshot.validate().is_err()
    {
        return false;
    }
    if input.continuity.kind == ObjectIdentityContinuity::NotificationCovered
        && !input.continuity.same_closure(&current.continuity)
    {
        return false;
    }

    let (
        input_app,
        input_window,
        input_root,
        input_target,
        input_enabled,
        input_actions,
        input_ancestry,
        input_parent_index,
        input_siblings,
        input_sibling_index,
        input_group_index,
        input_source,
        input_hash,
    ) = match &input.device_context {
        DeviceContextWitness::BackgroundAxV1 {
            app,
            window,
            root,
            target,
            target_enabled,
            target_action_names,
            ancestry,
            sibling_parent_ancestry_index,
            siblings,
            target_sibling_index,
            nearest_labelled_group_ancestry_index,
            source,
            target_context_hash,
        } => (
            app,
            window,
            root,
            target,
            target_enabled,
            target_action_names,
            ancestry,
            sibling_parent_ancestry_index,
            siblings,
            target_sibling_index,
            nearest_labelled_group_ancestry_index,
            source,
            target_context_hash,
        ),
        _ => return false,
    };
    let (
        current_app,
        current_window,
        current_root,
        current_target,
        current_enabled,
        current_actions,
        current_ancestry,
        current_parent_index,
        current_siblings,
        current_sibling_index,
        current_group_index,
        current_source,
        current_hash,
    ) = match &current.device_context {
        DeviceContextWitness::BackgroundAxV1 {
            app,
            window,
            root,
            target,
            target_enabled,
            target_action_names,
            ancestry,
            sibling_parent_ancestry_index,
            siblings,
            target_sibling_index,
            nearest_labelled_group_ancestry_index,
            source,
            target_context_hash,
        } => (
            app,
            window,
            root,
            target,
            target_enabled,
            target_action_names,
            ancestry,
            sibling_parent_ancestry_index,
            siblings,
            target_sibling_index,
            nearest_labelled_group_ancestry_index,
            source,
            target_context_hash,
        ),
        _ => return false,
    };

    let input_window_context = &input_source.window;
    let current_window_context = &current_source.window;
    if input_app != current_app
        || input_window != current_window
        || input_root != current_root
        || input_target != current_target
        || input_enabled != current_enabled
        || input_actions != current_actions
        || input_ancestry != current_ancestry
        || input_parent_index != current_parent_index
        || input_siblings != current_siblings
        || input_sibling_index != current_sibling_index
        || input_group_index != current_group_index
        || input_hash != current_hash
        || input_hash != &input_source.target_context_hash
        || current_hash != &current_source.target_context_hash
        || input_source.hidden != current_source.hidden
        || input_source.minimized != current_source.minimized
        || input_window_context.source_session_id != current_window_context.source_session_id
        || input_window_context.session_epoch != current_window_context.session_epoch
        || input_window_context.process != current_window_context.process
        || input_window_context.origin_window_id != current_window_context.origin_window_id
        || input_window_context.origin_window_frame != current_window_context.origin_window_frame
        || input_window_context.verified_source_sequence != input_source_sequence
        || input_window_context.verified_observed_ms != input.observed_ms
        || current_window_context.session_epoch != snapshot.session_epoch
        || current_window_context.process.pid != snapshot.app.pid
        || current_window_context.verified_source_sequence != snapshot.source_sequence
        || current_window_context.verified_observed_ms != snapshot.observed_ms
        || current_app.bundle_id != snapshot.app.bundle_id
        || current_app.pid != snapshot.app.pid
    {
        return false;
    }
    let BindingProofScope::BackgroundAx {
        target_context_hash: input_scope_hash,
    } = &input.continuity.scope
    else {
        return false;
    };
    let BindingProofScope::BackgroundAx {
        target_context_hash: current_scope_hash,
    } = &current.continuity.scope
    else {
        return false;
    };
    if input_scope_hash != current_scope_hash || input_scope_hash != input_hash {
        return false;
    }
    let Some(TargetContext::NativeWindow { app, source }) = snapshot.target_context.as_ref() else {
        return false;
    };
    if app.bundle_id != current_app.bundle_id
        || app.pid != current_app.pid
        || source.target_context_hash != *current_hash
        || !source.is_visible_standard_window()
    {
        return false;
    }

    let Some(object) = snapshot
        .objects
        .iter()
        .find(|object| object.instance_ref == current.instance)
    else {
        return false;
    };
    let matches_target = |candidate: &&UiObject| {
        candidate.role == current_target.object.role
            && normalize_label(&label_of(candidate)) == current_target.object.label
            && candidate
                .group
                .as_ref()
                .map(|group| normalize_label(group))
                .as_deref()
                == current_target.group.as_deref()
            && candidate.enabled
            && candidate.in_view
            && !candidate.secure
            && candidate.scroll.is_none()
    };
    let matching_targets: Vec<_> = snapshot.objects.iter().filter(matches_target).collect();
    if matching_targets.len() != 1
        || matching_targets[0].instance_ref != object.instance_ref
        || object.subrole != current_target.object.subrole
        || object.frame != current_target.object.frame
        || !object.enabled
        || !object.in_view
        || object.secure
        || object.scroll.is_some()
        || instance_fingerprint(snapshot, object).digest() != current.fingerprint_digest
    {
        return false;
    }

    let Ok(derived) = derive_background_ax_binding_evidence(snapshot, object) else {
        return false;
    };
    derived.device_context.supports_background_ax_press()
        && current.continuity.same_closure(&derived.continuity)
        && current.device_context == derived.device_context
}

/// A table-row binding is deliberately separate from `Binding`: its proof is
/// viewport-scoped, read-scoped, and authorizes only one absolute
/// AXSelected=true action. It never enters generic AX or pointer execution.
#[derive(Debug, Clone, PartialEq)]
pub struct RowBinding {
    pub binding_ref: BindingRef,
    pub semantic_target: SemanticTargetRef,
    /// Frozen public scope used to resolve this row. DeviceView must recheck
    /// this exact scope against the current scoped Source certificate.
    pub table_scope: interaction_contracts::desktop::task::RowTableScope,
    pub selector: RowSelector,
    pub source: RowSelectionObservation,
    pub fingerprint_digest: String,
    pub snapshot_id: RequestId,
    pub observed_ms: u64,
    pub env_revision: u64,
    pub layer_revision: u64,
    pub identity_boundary: Option<SourceIdentityBoundary>,
    pub hit_point: Option<DeviceContextPoint>,
    pub continuity: Option<BindingContinuityProof>,
    pub device_context: Option<DeviceContextWitness>,
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
    row_bindings: BTreeMap<String, RowBinding>,
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
            row_bindings: BTreeMap::new(),
            next: 1,
            margin_milli: DEFAULT_MARGIN_MILLI,
        }
    }

    pub fn current(&self, target: &SemanticTargetRef) -> Option<&Binding> {
        self.bindings.get(target.as_str())
    }

    pub fn current_row(&self, target: &SemanticTargetRef) -> Option<&RowBinding> {
        self.row_bindings.get(target.as_str())
    }

    pub fn contains_binding_ref(&self, reference: &BindingRef) -> bool {
        self.bindings
            .values()
            .any(|binding| &binding.binding_ref == reference)
            || self
                .row_bindings
                .values()
                .any(|binding| &binding.binding_ref == reference)
    }

    pub fn all(&self) -> impl Iterator<Item = &Binding> {
        self.bindings.values()
    }

    pub fn all_rows(&self) -> impl Iterator<Item = &RowBinding> {
        self.row_bindings.values()
    }

    /// Resolve and bind one public row target against exactly this Source read.
    /// Partial reads require the frozen scope's complete local census; a later
    /// snapshot always receives a new ref, even if the same visible row matches.
    pub fn bind_row(
        &mut self,
        env: &EnvSnapshot,
        target: &TargetSpec,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<RowBinding, String> {
        let key = target.semantic_target.as_str().to_string();
        let result = self.bind_row_current(env, target, now_ms, max_age_ms);
        if result.is_err() {
            self.bindings.remove(&key);
            self.row_bindings.remove(&key);
        }
        result
    }

    fn bind_row_current(
        &mut self,
        env: &EnvSnapshot,
        target: &TargetSpec,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<RowBinding, String> {
        if target.row.is_none()
            || env.snapshot.observer_coverage.state == SourceState::Unavailable
            || env.snapshot.menu_context.is_some()
            || env.snapshot.app.bundle_id.is_empty()
            || focused_window_id(&env.snapshot).is_none()
        {
            self.bindings.remove(target.semantic_target.as_str());
            self.row_bindings.remove(target.semantic_target.as_str());
            return Err("row selection requires an active non-menu Source scope".into());
        }
        let table_scope = target
            .row
            .as_ref()
            .map(|row| row.table_scope.clone())
            .ok_or_else(|| "row selection requires a frozen table scope".to_string())?;
        let valid_until_ms = env
            .snapshot
            .observed_ms
            .checked_add(max_age_ms)
            .ok_or_else(|| "row evidence deadline overflowed".to_string())?;
        let resolved =
            resolve_row_target(&env.snapshot, target, valid_until_ms, now_ms, max_age_ms).map_err(
                |failure| match failure {
                    RowResolutionFailure::IncompleteSearch => {
                        "the Source read cannot prove a complete row search".to_string()
                    }
                    RowResolutionFailure::NotFound => {
                        "no row matches the frozen table scope and values".to_string()
                    }
                    RowResolutionFailure::Ambiguous => {
                        "multiple rows match the frozen table scope and values".to_string()
                    }
                    RowResolutionFailure::InvalidScope => {
                        "the frozen row table scope is invalid or stale".to_string()
                    }
                },
            )?;
        let row = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == resolved.source.row_instance_ref)
            .ok_or_else(|| "the resolved row has no current AX object".to_string())?;
        if row.role != "AXRow"
            || row.window_id != resolved.source.window_id
            || focused_window_id(&env.snapshot) != Some(row.window_id.as_str())
            || !row.in_view
            || row.secure
            || row.frame != resolved.source.row_frame
        {
            self.row_bindings.remove(target.semantic_target.as_str());
            return Err("the resolved row is not an available active AXRow".into());
        }

        let row_input_enabled = resolved.source.table_ax_enabled.permits_row_selection()
            && resolved.source.row_ax_enabled.permits_row_selection();
        let hit_point = match (
            resolved.source.selected,
            resolved.source.ax_selected_settable,
            row_input_enabled,
        ) {
            (Some(false), Some(true), true) => Some(row_selection_hit_point(
                &env.snapshot,
                &resolved.selector,
                resolved.source.row_frame,
            )?),
            _ => None,
        };
        let evidence = match hit_point {
            Some(point) => Some(
                derive_row_selection_binding_evidence(
                    &env.snapshot,
                    &resolved.selector,
                    &resolved.source,
                    point,
                    now_ms,
                    max_age_ms,
                )
                .map_err(|reason| {
                    format!("the row has no valid native selection proof: {reason}")
                })?,
            ),
            None => None,
        };
        let fingerprint_digest = instance_fingerprint(&env.snapshot, row).digest();
        if let Some(existing) = self.row_bindings.get(target.semantic_target.as_str())
            && existing.snapshot_id == env.snapshot.snapshot_id
            && existing.observed_ms == env.snapshot.observed_ms
            && existing.env_revision == env.revision
            && existing.layer_revision == env.layer_revision
            && existing.table_scope == table_scope
            && existing.selector == resolved.selector
            && existing.source == resolved.source
            && existing.fingerprint_digest == fingerprint_digest
            && existing.hit_point == hit_point
            && match (&existing.continuity, evidence.as_ref()) {
                (Some(old), Some(new)) => {
                    old.same_closure(&new.continuity)
                        && existing.device_context.as_ref() == Some(&new.device_context)
                }
                (None, None) => true,
                _ => false,
            }
        {
            return Ok(existing.clone());
        }
        let binding_ref = BindingRef::new(format!("row-bind-{}", self.next))
            .expect("a counter is a valid binding id");
        self.next += 1;
        let binding = RowBinding {
            binding_ref,
            semantic_target: target.semantic_target.clone(),
            table_scope,
            selector: resolved.selector,
            source: resolved.source,
            fingerprint_digest,
            snapshot_id: env.snapshot.snapshot_id.clone(),
            observed_ms: env.snapshot.observed_ms,
            env_revision: env.revision,
            layer_revision: env.layer_revision,
            identity_boundary: env.snapshot.identity_boundary.clone(),
            hit_point,
            continuity: evidence.as_ref().map(|value| value.continuity.clone()),
            device_context: evidence.map(|value| value.device_context),
        };
        self.bindings.remove(target.semantic_target.as_str());
        self.row_bindings
            .insert(target.semantic_target.as_str().to_string(), binding.clone());
        Ok(binding)
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
        if env.snapshot.menu_context.is_some() {
            return Vec::new();
        }
        let Some(active) = focused_window_id(&env.snapshot) else {
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
        let evidence = match derive_binding_evidence(
            &env.snapshot,
            object,
            BindingProofScope::WholeStructure,
        ) {
            Ok(evidence) => evidence,
            Err(reason) => {
                return Self::reject(
                    request,
                    env,
                    FailureCode::ContextUnresolved,
                    &format!(
                        "the binding context has no valid continuity or Device witness: {reason}"
                    ),
                );
            }
        };
        if !evidence.device_context.supports_native_input() {
            return Self::reject(
                request,
                env,
                FailureCode::ContextUnresolved,
                "the binding context is outside the supported Native Device witness scope",
            );
        }
        self.install(env, request, object, rule, evidence)
    }

    fn install(
        &mut self,
        env: &EnvSnapshot,
        request: &BindRequest,
        object: &UiObject,
        rule: &str,
        evidence: interaction_contracts::desktop::binding::BindingDerivationEvidence,
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
            continuity: evidence.continuity,
            device_context: evidence.device_context,
            identity_boundary: env.snapshot.identity_boundary.clone(),
        };
        self.row_bindings.remove(request.semantic_target.as_str());
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
        self.bind_filtered(env, request, now_ms, None)
    }

    /// Bind one explicitly registered BackgroundAX target from a complete
    /// same-read graph. The caller must separately apply StateCore's health,
    /// refresh and trusted host-window gates before invoking this method.
    pub fn bind_background_ax(
        &mut self,
        env: &EnvSnapshot,
        target: &TargetSpec,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<Binding, String> {
        if target.row.is_some() || max_age_ms == 0 || max_age_ms > 2_000 {
            self.bindings.remove(target.semantic_target.as_str());
            return Err(
                "background AX binding requires a non-row target and a bounded 2s age".into(),
            );
        }
        let snapshot = &env.snapshot;
        if !snapshot.complete
            || snapshot.menu_context.is_some()
            || snapshot.observer_coverage.state == SourceState::Unavailable
            || snapshot.observed_ms > now_ms
            || now_ms.saturating_sub(snapshot.observed_ms) >= max_age_ms
            || snapshot.validate().is_err()
        {
            self.bindings.remove(target.semantic_target.as_str());
            return Err(
                "background AX binding requires a complete fresh healthy Source read".into(),
            );
        }
        let supported_role = target.role_hint.as_deref().is_some_and(|role| {
            matches!(
                role,
                "AXButton" | "AXCheckBox" | "AXRadioButton" | "AXSwitch"
            )
        });
        if !supported_role {
            self.bindings.remove(target.semantic_target.as_str());
            return Err("background AX binding requires a registered simple-control role".into());
        }
        let label = normalize_label(&target.label);
        let group = target.group.as_deref().map(normalize_label);
        let matching: Vec<_> = snapshot
            .objects
            .iter()
            .filter(|object| {
                object.role == target.role_hint.as_deref().unwrap_or_default()
                    && normalize_label(&label_of(object)) == label
                    && group.as_ref().map_or_else(
                        || object.group.is_none(),
                        |group| {
                            object.group.as_deref().map(normalize_label).as_ref() == Some(group)
                        },
                    )
                    && object.enabled
                    && object.in_view
                    && !object.secure
                    && object.scroll.is_none()
                    && matches!(
                        object.role.as_str(),
                        "AXButton" | "AXCheckBox" | "AXRadioButton" | "AXSwitch"
                    )
            })
            .collect();
        let [object] = matching.as_slice() else {
            self.bindings.remove(target.semantic_target.as_str());
            return Err(
                "background AX binding target is absent or ambiguous in this Source read".into(),
            );
        };
        let evidence = match derive_background_ax_binding_evidence(snapshot, object) {
            Ok(evidence) if evidence.device_context.supports_background_ax_press() => evidence,
            Ok(_) => {
                self.bindings.remove(target.semantic_target.as_str());
                return Err("background AX target has no enabled, visible AXPress witness".into());
            }
            Err(reason) => {
                self.bindings.remove(target.semantic_target.as_str());
                return Err(format!(
                    "background AX target graph is not bindable: {reason}"
                ));
            }
        };

        if let Some(existing) = self.bindings.get(target.semantic_target.as_str())
            && existing.rule == "background_ax"
            && existing.instance == object.instance_ref
            && existing.fingerprint_digest == instance_fingerprint(snapshot, object).digest()
            && existing.continuity.same_closure(&evidence.continuity)
        {
            return Ok(existing.clone());
        }
        self.bindings.remove(target.semantic_target.as_str());
        self.row_bindings.remove(target.semantic_target.as_str());
        let binding_ref = BindingRef::new(format!("bg-bind-{}", self.next))
            .expect("a counter is a valid background binding id");
        self.next += 1;
        let binding = Binding {
            binding_ref,
            semantic_target: target.semantic_target.clone(),
            instance: object.instance_ref.clone(),
            fingerprint_digest: instance_fingerprint(snapshot, object).digest(),
            layer_revision: env.layer_revision,
            env_revision: env.revision,
            snapshot_id: snapshot.snapshot_id.clone(),
            observed_ms: snapshot.observed_ms,
            page: None,
            rule: "background_ax".into(),
            continuity: evidence.continuity,
            device_context: evidence.device_context,
            identity_boundary: snapshot.identity_boundary.clone(),
        };
        self.bindings
            .insert(target.semantic_target.as_str().to_string(), binding.clone());
        Ok(binding)
    }

    /// Re-identify a semantic destination only among the selected container's
    /// current descendants. No outside duplicate or old virtual row can bind.
    pub fn bind_in_container(
        &mut self,
        env: &EnvSnapshot,
        target: &TargetSpec,
        container: &Binding,
        now_ms: u64,
        max_age_ms: u64,
    ) -> BindingResult {
        let request = BindRequest {
            request_id: RequestId::new(format!("bind-scroll-{}", self.next)).expect("counter id"),
            semantic_target: target.semantic_target.clone(),
            description: interaction_contracts::desktop::binding::TargetDescription {
                label: target.label.clone(),
                group: target.group.clone(),
                role_hint: target.role_hint.clone(),
            },
            scope: interaction_contracts::desktop::binding::BindScope {
                app: env.snapshot.app.bundle_id.clone(),
                account: None,
            },
            choice: None,
            max_age_ms,
        };
        if target.row.is_some() {
            self.bindings.remove(target.semantic_target.as_str());
            return Self::reject(
                &request,
                env,
                FailureCode::ContextUnresolved,
                "row destinations require their public table/column resolver and dedicated viewport input proof",
            );
        }
        if target.semantic_target == container.semantic_target
            || self
                .current(&container.semantic_target)
                .is_none_or(|current| current.binding_ref != container.binding_ref)
            || !env.snapshot.objects.iter().any(|object| {
                object.instance_ref == container.instance
                    && object.role == "AXScrollArea"
                    && object.in_view
                    && object.enabled
                    && !object.secure
                    && focused_window_id(&env.snapshot) == Some(object.window_id.as_str())
                    && instance_fingerprint(&env.snapshot, object).digest()
                        == container.fingerprint_digest
            })
        {
            return Self::reject(
                &request,
                env,
                FailureCode::ContextUnresolved,
                "scroll container binding is no longer current",
            );
        }
        self.bindings.remove(target.semantic_target.as_str());
        self.bind_filtered(env, &request, now_ms, Some(&container.instance))
    }

    pub fn in_container(env: &EnvSnapshot, instance: &ObjectRef, container: &ObjectRef) -> bool {
        if instance == container {
            return false;
        }
        let Some(area) = env
            .snapshot
            .objects
            .iter()
            .find(|o| &o.instance_ref == container)
        else {
            return false;
        };
        let window = &area.window_id;
        let mut cursor = instance;
        let mut visited = std::collections::BTreeSet::new();
        for _ in 0..32 {
            if !visited.insert(cursor.clone()) {
                return false;
            }
            let Some(object) = env
                .snapshot
                .objects
                .iter()
                .find(|o| &o.instance_ref == cursor)
            else {
                return false;
            };
            if &object.window_id != window {
                return false;
            }
            let Some(parent) = object.parent.as_ref() else {
                return false;
            };
            if parent == container {
                return true;
            }
            cursor = parent;
        }
        false
    }

    fn bind_filtered(
        &mut self,
        env: &EnvSnapshot,
        request: &BindRequest,
        now_ms: u64,
        container: Option<&ObjectRef>,
    ) -> BindingResult {
        if env.snapshot.observer_coverage.state == SourceState::Unavailable {
            return Self::reject(
                request,
                env,
                FailureCode::ContextUnresolved,
                "the Source root observer is unavailable",
            );
        }
        if env.snapshot.menu_context.is_some() {
            return Self::reject(
                request,
                env,
                FailureCode::ContextUnresolved,
                "normal binding is unavailable while the input domain is a menu",
            );
        }
        if env.snapshot.app.bundle_id != request.scope.app {
            return Self::reject(
                request,
                env,
                FailureCode::ContextUnresolved,
                "the application is outside the binding scope",
            );
        }
        if now_ms.saturating_sub(env.snapshot.observed_ms) > request.max_age_ms {
            return Self::reject(
                request,
                env,
                FailureCode::EvidenceExpired,
                "the evidence is too old to bind from",
            );
        }
        if focused_window_id(&env.snapshot).is_none() {
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
        let scored: Vec<_> = Self::candidates(env, request)
            .into_iter()
            .filter(|entry| {
                container.is_none_or(|container| {
                    Self::in_container(env, &entry.object.instance_ref, container)
                })
            })
            .collect();

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
        if !env.snapshot.complete
            || env.snapshot.observer_coverage.state == SourceState::Unavailable
            || env.snapshot.menu_context.is_some()
        {
            return None;
        }
        let active = focused_window_id(&env.snapshot)?;
        let object = env.snapshot.objects.iter().find(|object| {
            &object.instance_ref == instance
                && object.window_id == active
                && object.enabled
                && object.in_view
                && !object.secure
        })?;
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

    /// Window activation bindings are installed only by the trusted window
    /// scope path, never by ordinary background target matching.
    pub fn bind_window(&mut self, env: &EnvSnapshot, instance: &ObjectRef) -> Option<Binding> {
        let target = SemanticTargetRef::new("native-window").ok()?;
        self.bindings.remove(target.as_str());
        if env.snapshot.menu_context.is_some()
            || env.snapshot.observer_coverage.state == SourceState::Unavailable
        {
            return None;
        }
        let object = env.snapshot.objects.iter().find(|o| {
            &o.instance_ref == instance
                && o.parent.is_none()
                && o.role == "AXWindow"
                && o.subrole.as_deref() == Some("AXStandardWindow")
                && o.in_view
                && !o.secure
        })?;
        let request = BindRequest {
            request_id: RequestId::new(format!("bind-window-{}", self.next)).ok()?,
            semantic_target: target.clone(),
            description: interaction_contracts::desktop::binding::TargetDescription {
                label: object.title.clone().unwrap_or_default(),
                group: None,
                role_hint: Some("AXWindow".into()),
            },
            scope: interaction_contracts::desktop::binding::BindScope {
                app: env.snapshot.app.bundle_id.clone(),
                account: None,
            },
            choice: None,
            max_age_ms: 2000,
        };
        self.create(env, &request, object, "window_activation");
        self.current(&target).cloned()
    }

    /// Bind the fixed window root for the dedicated activation purpose using
    /// the exact frozen row scope. This carries no generic native-input
    /// authority; ordinary `create` continues to require WholeStructure.
    pub fn bind_window_root(
        &mut self,
        env: &EnvSnapshot,
        instance: &ObjectRef,
        table_scope: &RowTableScope,
    ) -> Option<Binding> {
        let target = SemanticTargetRef::new("native-window").ok()?;
        // A failed scoped rebind cannot leave a previous run's complete-tree
        // activation binding available under the same semantic target.
        self.bindings.remove(target.as_str());
        if env.snapshot.complete
            || env.snapshot.menu_context.is_some()
            || env.snapshot.observer_coverage.state == SourceState::Unavailable
        {
            return None;
        }
        let object = env.snapshot.objects.iter().find(|object| {
            &object.instance_ref == instance
                && object.parent.is_none()
                && object.role == "AXWindow"
                && object.subrole.as_deref() == Some("AXStandardWindow")
                && object.in_view
                && !object.secure
        })?;
        let evidence =
            derive_window_root_binding_evidence(&env.snapshot, object, table_scope).ok()?;
        if !evidence.device_context.supports_window_activation()
            || evidence.continuity.scope
                != (BindingProofScope::WindowRoot {
                    table_scope: table_scope.clone(),
                })
            || !matches!(
                &evidence.device_context,
                DeviceContextWitness::WindowRootV1 { .. }
            )
        {
            return None;
        }
        let request = BindRequest {
            request_id: RequestId::new(format!("bind-window-root-{}", self.next)).ok()?,
            semantic_target: target.clone(),
            description: interaction_contracts::desktop::binding::TargetDescription {
                label: object.title.clone().unwrap_or_default(),
                group: None,
                role_hint: Some("AXWindow".into()),
            },
            scope: interaction_contracts::desktop::binding::BindScope {
                app: env.snapshot.app.bundle_id.clone(),
                account: None,
            },
            choice: None,
            max_age_ms: 2_000,
        };
        let result = self.install(env, &request, object, "window_activation", evidence);
        if !matches!(result.outcome, BindingOutcome::Bound { .. }) {
            return None;
        }
        self.current(&target).cloned()
    }

    pub fn invalidate_target_inputs(&mut self) {
        self.bindings
            .retain(|_, binding| binding.rule == "window_activation");
        self.row_bindings.clear();
    }

    /// Check every binding against new evidence and drop the ones that no
    /// longer hold. The caller tells the device about each.
    pub fn revalidate(&mut self, env: &EnvSnapshot) -> Vec<Invalidated> {
        let mut dropped = Vec::new();
        let targets: Vec<String> = self.bindings.keys().cloned().collect();
        for key in targets {
            let binding = self.bindings[&key].clone();
            let mut reason = if env.snapshot.observer_coverage.state == SourceState::Unavailable {
                Some("the Source root observer is unavailable")
            } else if binding.identity_boundary != env.snapshot.identity_boundary {
                Some("the global Source identity boundary changed")
            } else if binding.rule == "background_ax" {
                if !env.snapshot.complete
                    || env.snapshot.menu_context.is_some()
                    || !env.snapshot.objects.iter().any(|object| {
                        object.instance_ref == binding.instance
                            && object.enabled
                            && object.in_view
                            && !object.secure
                            && object.scroll.is_none()
                            && instance_fingerprint(&env.snapshot, object).digest()
                                == binding.fingerprint_digest
                    })
                {
                    Some("the BackgroundAX target instance changed")
                } else {
                    None
                }
            } else if binding.rule == "window_activation" {
                let is_window_root_scope = matches!(
                    &binding.continuity.scope,
                    BindingProofScope::WindowRoot { .. }
                );
                if (!is_window_root_scope && !env.snapshot.complete)
                    || env.snapshot.menu_context.is_some()
                    || !env.snapshot.objects.iter().any(|o| {
                        o.instance_ref == binding.instance
                            && o.parent.is_none()
                            && o.role == "AXWindow"
                            && o.subrole.as_deref() == Some("AXStandardWindow")
                            && o.in_view
                            && !o.secure
                            && instance_fingerprint(&env.snapshot, o).digest()
                                == binding.fingerprint_digest
                    })
                {
                    Some("the activation window instance changed")
                } else {
                    None
                }
            } else if binding.rule != "window_activation"
                && binding.rule != "background_ax"
                && focused_window_id(&env.snapshot).is_none()
            {
                Some("the active layer is unknown")
            } else if binding.rule != "background_ax"
                && binding.layer_revision != env.layer_revision
            {
                Some("the active layer changed")
            } else if binding.rule != "background_ax" && binding.page != env.page {
                Some("the page changed")
            } else if !env.snapshot.complete {
                Some("the structure read is incomplete")
            } else if !self.instance_still_there(env, &binding) {
                Some("the instance moved, changed or disappeared")
            } else {
                None
            };
            let current_evidence = if reason.is_none() {
                env.snapshot
                    .objects
                    .iter()
                    .find(|object| object.instance_ref == binding.instance)
                    .and_then(
                        |object| match (&binding.rule[..], &binding.continuity.scope) {
                            (
                                "window_activation",
                                BindingProofScope::WindowRoot { table_scope },
                            ) => derive_window_root_binding_evidence(
                                &env.snapshot,
                                object,
                                table_scope,
                            )
                            .ok(),
                            ("background_ax", BindingProofScope::BackgroundAx { .. }) => {
                                derive_background_ax_binding_evidence(&env.snapshot, object).ok()
                            }
                            (_, BindingProofScope::WholeStructure) => derive_binding_evidence(
                                &env.snapshot,
                                object,
                                BindingProofScope::WholeStructure,
                            )
                            .ok(),
                            _ => None,
                        },
                    )
            } else {
                None
            };
            if reason.is_none() {
                match current_evidence.as_ref() {
                    Some(current)
                        if current.continuity.kind == ObjectIdentityContinuity::ReadScoped
                            && current.continuity.source_sequence
                                != binding.continuity.source_sequence =>
                    {
                        reason = Some("the read-scoped binding expired with its source read");
                    }
                    Some(current) if binding.rule == "window_activation" => {
                        let expected_scope_matches =
                            match (&binding.continuity.scope, &current.device_context) {
                                (
                                    BindingProofScope::WholeStructure,
                                    DeviceContextWitness::WholeStructureV1 { .. },
                                ) => true,
                                (
                                    BindingProofScope::WindowRoot { .. },
                                    DeviceContextWitness::WindowRootV1 { .. },
                                ) => !env.snapshot.complete,
                                _ => false,
                            };
                        let window_root_closure_matches =
                            !matches!(
                                &binding.continuity.scope,
                                BindingProofScope::WindowRoot { .. }
                            ) || binding.continuity.same_closure(&current.continuity);
                        if !expected_scope_matches
                            || !window_root_closure_matches
                            || !current.device_context.supports_window_activation()
                        {
                            reason = Some(
                                "the activation binding's current proof scope or closure changed",
                            );
                        }
                    }
                    Some(current) if binding.rule == "background_ax" => {
                        let expected_scope_matches =
                            match (&binding.continuity.scope, &current.device_context) {
                                (
                                    BindingProofScope::BackgroundAx {
                                        target_context_hash,
                                    },
                                    DeviceContextWitness::BackgroundAxV1 { source, .. },
                                ) => {
                                    source.target_context_hash.as_str()
                                        == target_context_hash.as_str()
                                }
                                _ => false,
                            };
                        let monotonic_source =
                            match (&binding.device_context, &current.device_context) {
                                (
                                    DeviceContextWitness::BackgroundAxV1 {
                                        source: previous, ..
                                    },
                                    DeviceContextWitness::BackgroundAxV1 {
                                        source: current, ..
                                    },
                                ) => {
                                    previous.window.source_session_id
                                        == current.window.source_session_id
                                        && previous.window.session_epoch
                                            == current.window.session_epoch
                                        && previous.window.process == current.window.process
                                        && previous.window.origin_window_id
                                            == current.window.origin_window_id
                                        && previous.window.origin_window_frame
                                            == current.window.origin_window_frame
                                        && previous.window.verified_source_sequence
                                            <= current.window.verified_source_sequence
                                        && previous.window.verified_observed_ms
                                            <= current.window.verified_observed_ms
                                }
                                _ => false,
                            };
                        if !expected_scope_matches
                            || !monotonic_source
                            || !binding.continuity.same_closure(&current.continuity)
                            || !current.device_context.supports_background_ax_press()
                        {
                            reason =
                                Some("the BackgroundAX binding source identity or closure changed");
                        }
                    }
                    Some(current) if !current.device_context.supports_native_input() => {
                        reason = Some("the binding's current Device witness scope is unsupported");
                    }
                    Some(current) if !binding.continuity.same_closure(&current.continuity) => {
                        reason = Some("the object identity context changed");
                    }
                    Some(_) => {}
                    None => {
                        reason =
                            Some("the object context has no valid continuity or Device witness")
                    }
                }
            }
            if let Some(reason) = reason {
                let binding = self.bindings.remove(&key).expect("listed above");
                dropped.push(Invalidated {
                    binding: binding.binding_ref,
                    semantic_target: binding.semantic_target,
                    reason: reason.into(),
                });
            } else if let Some(evidence) = current_evidence {
                // A covered proof may refresh its sequence after the complete
                // closure and device witness have been rederived unchanged.
                // Read-scoped bindings reach this path only for their originating
                // read.
                let current = self.bindings.get_mut(&key).expect("listed above");
                current.continuity = evidence.continuity;
                current.device_context = evidence.device_context;
                current.env_revision = env.revision;
                current.snapshot_id = env.snapshot.snapshot_id.clone();
                current.observed_ms = env.snapshot.observed_ms;
            }
        }
        let row_targets: Vec<String> = self.row_bindings.keys().cloned().collect();
        for key in row_targets {
            let binding = self.row_bindings[&key].clone();
            let reason = if env.snapshot.observer_coverage.state == SourceState::Unavailable {
                Some("the Source root observer is unavailable")
            } else if binding.identity_boundary != env.snapshot.identity_boundary {
                Some("the global Source identity boundary changed")
            } else if env.snapshot.menu_context.is_some() || !env.snapshot.complete {
                Some("the row's current viewport is unavailable")
            } else if binding.snapshot_id != env.snapshot.snapshot_id
                || binding.source.source_sequence != env.snapshot.source_sequence
                || binding.env_revision != env.revision
            {
                Some("the read-scoped row binding expired with its Source read")
            } else if binding.layer_revision != env.layer_revision
                || focused_window_id(&env.snapshot) != Some(binding.source.window_id.as_str())
            {
                Some("the row's active window changed")
            } else {
                None
            };
            if let Some(reason) = reason {
                let binding = self.row_bindings.remove(&key).expect("listed above");
                dropped.push(Invalidated {
                    binding: binding.binding_ref,
                    semantic_target: binding.semantic_target,
                    reason: reason.into(),
                });
            }
        }
        dropped
    }

    fn instance_still_there(&self, env: &EnvSnapshot, binding: &Binding) -> bool {
        let Some(active) = focused_window_id(&env.snapshot) else {
            return false;
        };
        env.snapshot.objects.iter().any(|object| {
            object.instance_ref == binding.instance
                && object.window_id == active
                && object.enabled
                && object.in_view
                && !object.secure
                && match &env.snapshot.menu_context {
                    Some(context) => {
                        binding.rule == "menu_item"
                            && object.role == "AXMenuItem"
                            && object.parent.as_ref() == Some(&context.menu_instance)
                    }
                    None => binding.rule != "menu_item",
                }
                && instance_fingerprint(&env.snapshot, object).digest()
                    == binding.fingerprint_digest
        })
    }

    /// Only this path can bind a menu item. Exact labels are scoped to the
    /// current one-level menu, including disabled duplicates in ambiguity.
    pub fn bind_in_menu(
        &mut self,
        env: &EnvSnapshot,
        request: &BindRequest,
        context: &interaction_contracts::desktop::source::MenuContext,
    ) -> BindingResult {
        if !env.snapshot.complete
            || env.snapshot.observer_coverage.state == SourceState::Unavailable
            || env.snapshot.validate().is_err()
            || env.snapshot.menu_context.as_ref() != Some(context)
            || request.scope.app != env.snapshot.app.bundle_id
            || request.scope.account.is_some()
            || request.description.role_hint.as_deref() != Some("AXMenuItem")
            || request.choice.is_some()
        {
            return Self::reject(
                request,
                env,
                FailureCode::ContextUnresolved,
                "a menu binding requires the current complete menu context and exact item scope",
            );
        }
        let want = normalize_label(&request.description.label);
        let candidates: Vec<_> = env
            .snapshot
            .objects
            .iter()
            .filter(|object| {
                object.role == "AXMenuItem"
                    && object.parent.as_ref() == Some(&context.menu_instance)
                    && object.window_id == context.opener.origin_window_id
                    && normalize_label(&label_of(object)) == want
            })
            .collect();
        if candidates.len() > 1 {
            return BindingResult {
                request_id: request.request_id.clone(),
                environment_revision: env.revision,
                outcome: BindingOutcome::Ambiguous {
                    candidates: candidates
                        .into_iter()
                        .map(|object| BindCandidate {
                            instance: object.instance_ref.clone(),
                            role: object.role.clone(),
                            label: label_of(object).to_owned(),
                            group: object.group.clone(),
                            score_milli: 1000,
                        })
                        .collect(),
                },
            };
        }
        let Some(object) = candidates.first() else {
            return BindingResult {
                request_id: request.request_id.clone(),
                environment_revision: env.revision,
                outcome: BindingOutcome::NotFound {
                    observation_complete: true,
                },
            };
        };
        if want.is_empty()
            || object.secure
            || !object.enabled
            || !object.in_view
            || !object
                .actions
                .contains(&interaction_contracts::desktop::source::AxActionName::Press)
        {
            return Self::reject(
                request,
                env,
                FailureCode::ContextUnresolved,
                "the exact menu item is unavailable, private or cannot be pressed",
            );
        }
        self.create(env, request, object, "menu_item")
    }

    pub fn invalidate_all(&mut self, reason: &str) -> Vec<Invalidated> {
        let generic = std::mem::take(&mut self.bindings)
            .into_values()
            .map(|binding| Invalidated {
                binding: binding.binding_ref,
                semantic_target: binding.semantic_target,
                reason: reason.into(),
            })
            .collect::<Vec<_>>();
        let rows = std::mem::take(&mut self.row_bindings)
            .into_values()
            .map(|binding| Invalidated {
                binding: binding.binding_ref,
                semantic_target: binding.semantic_target,
                reason: reason.into(),
            });
        generic.into_iter().chain(rows).collect()
    }

    pub fn invalidate(&mut self, target: &SemanticTargetRef, reason: &str) -> Option<Invalidated> {
        self.row_bindings
            .remove(target.as_str())
            .map(|binding| Invalidated {
                binding: binding.binding_ref,
                semantic_target: binding.semantic_target,
                reason: reason.into(),
            })
            .or_else(|| {
                self.bindings
                    .remove(target.as_str())
                    .map(|binding| Invalidated {
                        binding: binding.binding_ref,
                        semantic_target: binding.semantic_target,
                        reason: reason.into(),
                    })
            })
    }
}

fn row_selection_hit_point(
    snapshot: &interaction_contracts::desktop::source::StructureSnapshot,
    selector: &RowSelector,
    row_frame: Rect,
) -> Result<DeviceContextPoint, String> {
    let viewport = snapshot
        .table_viewports
        .iter()
        .find(|viewport| viewport.table_instance_ref == selector.table_instance_ref)
        .ok_or_else(|| "the row's actual table viewport is missing".to_string())?;
    let mut windows = snapshot
        .windows
        .iter()
        .filter(|window| window.window_id == selector.window_id);
    let window = windows
        .next()
        .ok_or_else(|| "the row's actual window geometry is missing".to_string())?;
    if windows.next().is_some()
        || window.owner_bundle_id != snapshot.app.bundle_id
        || !row_frame.valid_bounded()
        || !viewport.viewport_frame.valid_bounded()
        || !window.frame.valid_bounded()
    {
        return Err("the row selection geometry is invalid or ambiguous".into());
    }
    let left = row_frame
        .x
        .max(viewport.viewport_frame.x)
        .max(window.frame.x);
    let top = row_frame
        .y
        .max(viewport.viewport_frame.y)
        .max(window.frame.y);
    let right = (row_frame.x + row_frame.width)
        .min(viewport.viewport_frame.x + viewport.viewport_frame.width)
        .min(window.frame.x + window.frame.width);
    let bottom = (row_frame.y + row_frame.height)
        .min(viewport.viewport_frame.y + viewport.viewport_frame.height)
        .min(window.frame.y + window.frame.height);
    let width = right - left;
    let height = bottom - top;
    if ![left, top, right, bottom, width, height]
        .into_iter()
        .all(f64::is_finite)
        || width <= 0.0
        || height <= 0.0
    {
        return Err("the AXRow has no visible intersection with its table viewport".into());
    }
    let point = DeviceContextPoint {
        x: left + 0.5_f64.min(width / 2.0),
        y: top + height / 2.0,
    };
    let inside = |frame: Rect| {
        point.x >= frame.x
            && point.y >= frame.y
            && point.x < frame.x + frame.width
            && point.y < frame.y + frame.height
    };
    if !inside(row_frame) || !inside(viewport.viewport_frame) || !inside(window.frame) {
        return Err("the row selection point is outside its actual visible geometry".into());
    }
    Ok(point)
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

    fn scoped_window_fixture() -> (EnvSnapshot, RowTableScope, ObjectRef) {
        const SYNTHETIC: &str = include_str!(
            "../../../../contracts/desktop-v1/native-binding/scoped-window-synthetic.json"
        );
        let snapshot: interaction_contracts::desktop::source::StructureSnapshot =
            serde_json::from_str(SYNTHETIC)
                .expect("the test-only synthetic snapshot fixture is valid JSON");
        let scope = snapshot
            .table_scope_search
            .as_ref()
            .expect("the partial Source fixture carries a table census")
            .requested_scope
            .clone();
        let window = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .expect("the scoped Source fixture has an active target window");
        let root = snapshot
            .objects
            .iter()
            .find(|object| {
                object.window_id == window
                    && object.parent.is_none()
                    && object.role == "AXWindow"
                    && object.subrole.as_deref() == Some("AXStandardWindow")
            })
            .expect("the scoped Source fixture has one standard root")
            .instance_ref
            .clone();
        let mut core = StateCore::new();
        core.set_session(snapshot.session_epoch);
        core.admit(
            snapshot.clone(),
            snapshot.observed_ms,
            &AppPack::settings_fixture(),
        )
        .expect("the test-only synthetic snapshot is admissible");
        (core.latest().unwrap().clone(), scope, root)
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
        crate::testkit::focus_elsewhere(&mut no_layer.snapshot);
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
    fn scoped_window_activation_binds_only_the_frozen_window_root_proof() {
        let (env, scope, root) = scoped_window_fixture();
        let mut reducer = BindingReducer::new();
        let binding = reducer
            .bind_window_root(&env, &root, &scope)
            .expect("an exact, same-read scoped census may prepare activation");

        assert_eq!(binding.rule, "window_activation");
        assert_eq!(
            binding.continuity.scope,
            BindingProofScope::WindowRoot {
                table_scope: scope.clone()
            }
        );
        assert!(matches!(
            &binding.device_context,
            DeviceContextWitness::WindowRootV1 { .. }
        ));
        assert!(binding.device_context.supports_window_activation());
        assert!(
            !binding.device_context.supports_native_input(),
            "activation metadata must not become ordinary native-input authority"
        );
    }

    #[test]
    fn scoped_window_activation_rejects_a_different_frozen_scope_and_clears_old_binding() {
        let (env, scope, root) = scoped_window_fixture();
        let target = SemanticTargetRef::new("native-window").unwrap();
        let mut reducer = BindingReducer::new();
        assert!(reducer.bind_window_root(&env, &root, &scope).is_some());

        let mut wrong_scope = scope;
        wrong_scope.column_headers.push("另一个列".into());
        assert!(
            reducer
                .bind_window_root(&env, &root, &wrong_scope)
                .is_none()
        );
        assert!(
            reducer.current(&target).is_none(),
            "failed scoped rebinding cannot leave prior activation authority installed"
        );
    }

    #[test]
    fn ordinary_window_binding_cannot_consume_a_partial_scoped_census() {
        let (env, scope, root) = scoped_window_fixture();
        let target = SemanticTargetRef::new("native-window").unwrap();
        let mut reducer = BindingReducer::new();
        assert!(reducer.bind_window_root(&env, &root, &scope).is_some());

        assert!(reducer.bind_window(&env, &root).is_none());
        assert!(
            reducer.current(&target).is_none(),
            "the complete-tree binder must not retain or borrow the scoped activation proof"
        );
    }

    #[test]
    fn scoped_activation_revalidation_drops_a_changed_kernel_process_closure() {
        let (env, scope, root) = scoped_window_fixture();
        let target = SemanticTargetRef::new("native-window").unwrap();
        let mut reducer = BindingReducer::new();
        assert!(reducer.bind_window_root(&env, &root, &scope).is_some());

        let mut changed = env;
        changed
            .snapshot
            .window_context
            .as_mut()
            .expect("the scoped fixture has Source window provenance")
            .process
            .start_seconds += 1;
        let invalidated = reducer.revalidate(&changed);
        assert_eq!(invalidated.len(), 1);
        assert!(invalidated[0].reason.contains("scope or closure changed"));
        assert!(reducer.current(&target).is_none());
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
        let original_device_context = reducer.current(&target).unwrap().device_context.clone();

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
        assert_eq!(
            reducer.current(&target).unwrap().device_context,
            original_device_context,
            "a value-only change keeps the source-compiled Device witness"
        );

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
    fn an_existing_binding_is_revoked_when_the_instance_becomes_ineligible() {
        for change in ["disabled", "hidden", "secure", "replacement", "process"] {
            let (mut core, app, mut reducer) = setup();
            let mut env = observe(&mut core, &app, 1);
            let request = request("Daily picks", Some("Push"), Some("switch"));
            reducer.bind(&env, &request, 10);
            let object = env
                .snapshot
                .objects
                .iter_mut()
                .find(|object| object.instance_ref.as_str() == "sw-push-daily")
                .unwrap();
            match change {
                "disabled" => object.enabled = false,
                "hidden" => object.in_view = false,
                "secure" => object.secure = true,
                "replacement" => object.instance_ref = ObjectRef::new("replacement").unwrap(),
                "process" => env.snapshot.app.pid += 1,
                _ => unreachable!(),
            }
            assert_eq!(reducer.revalidate(&env).len(), 1, "{change}");
            assert!(
                reducer.current(&request.semantic_target).is_none(),
                "{change}"
            );
        }
    }

    #[test]
    fn direct_instance_binding_requires_a_complete_active_and_operable_target() {
        for change in [
            "disabled",
            "hidden",
            "secure",
            "inactive",
            "incomplete",
            "observer_unavailable",
        ] {
            let (mut core, app, mut reducer) = setup();
            let mut env = observe(&mut core, &app, 1);
            let instance = ObjectRef::new("sw-push-daily").unwrap();
            if change == "observer_unavailable" {
                for object in &mut env.snapshot.objects {
                    object.identity_continuity = ObjectIdentityContinuity::ReadScoped;
                }
                env.snapshot.observer_coverage.state = SourceState::Unavailable;
                env.snapshot.observer_coverage.covered_objects = 0;
                env.snapshot.observer_coverage.read_scoped_objects =
                    env.snapshot.objects.len() as u32;
                assert!(env.snapshot.validate().is_ok());
            } else {
                let object = env
                    .snapshot
                    .objects
                    .iter_mut()
                    .find(|object| object.instance_ref == instance)
                    .unwrap();
                match change {
                    "disabled" => object.enabled = false,
                    "hidden" => object.in_view = false,
                    "secure" => object.secure = true,
                    "inactive" => crate::testkit::focus_elsewhere(&mut env.snapshot),
                    "incomplete" => env.snapshot.complete = false,
                    _ => unreachable!(),
                }
            }
            assert!(
                reducer
                    .bind_instance(&env, SemanticTargetRef::new("target").unwrap(), &instance)
                    .is_none(),
                "{change}"
            );
        }
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
    fn a_target_in_a_table_viewport_is_not_bound_as_a_whole_structure_input() {
        let mut core = StateCore::new();
        core.set_session(1);
        let pack = AppPack::settings_fixture();
        let mut snapshot = SimApp::new().snapshot(&meta(1));
        let root = snapshot
            .objects
            .iter()
            .find(|object| object.parent.is_none() && object.role == "AXWindow")
            .expect("the fixture has its actual window root")
            .clone();
        let frame = |x: f64, y: f64, width: f64, height: f64| {
            interaction_contracts::desktop::source::Rect {
                x,
                y,
                width,
                height,
            }
        };
        let scroll_frame = frame(
            root.frame.x + 10.0,
            root.frame.y + 10.0,
            root.frame.width - 20.0,
            root.frame.height / 2.0,
        );
        let table_frame = scroll_frame;
        let row_frame = frame(
            table_frame.x + 10.0,
            table_frame.y + 10.0,
            table_frame.width - 20.0,
            30.0,
        );
        let scroll_ref = ObjectRef::new("m2-test-scroll").unwrap();
        let table_ref = ObjectRef::new("m2-test-table").unwrap();
        let row_ref = ObjectRef::new("m2-test-row").unwrap();
        let make_object =
            |instance_ref: &ObjectRef,
             parent: &ObjectRef,
             role: &str,
             frame: interaction_contracts::desktop::source::Rect| UiObject {
                instance_ref: instance_ref.clone(),
                parent: Some(parent.clone()),
                window_id: root.window_id.clone(),
                role: role.into(),
                subrole: None,
                title: None,
                description: None,
                value: None,
                enabled: true,
                focused: false,
                frame,
                actions: Vec::new(),
                ax_action_names: None,
                in_view: true,
                group: None,
                secure: false,
                scroll: None,
                identity_continuity: ObjectIdentityContinuity::NotificationCovered,
            };
        snapshot.objects.push(make_object(
            &scroll_ref,
            &root.instance_ref,
            "AXScrollArea",
            scroll_frame,
        ));
        snapshot
            .objects
            .push(make_object(&table_ref, &scroll_ref, "AXTable", table_frame));
        snapshot
            .objects
            .push(make_object(&row_ref, &table_ref, "AXRow", row_frame));
        snapshot.table_viewports = vec![interaction_contracts::desktop::source::TableViewport {
            table_instance_ref: table_ref.clone(),
            scroll_container_instance_ref: scroll_ref.clone(),
            process_id: snapshot.app.pid,
            ax_enabled: interaction_contracts::desktop::source::AxEnabledObservation::Enabled,
            table_frame,
            viewport_frame: scroll_frame,
            visible_rows_complete: false,
            visible_rows: vec![interaction_contracts::desktop::source::TableViewportRow {
                instance_ref: Some(row_ref.clone()),
                parent_instance_ref: table_ref.clone(),
                frame: row_frame,
                ax_enabled: interaction_contracts::desktop::source::AxEnabledObservation::Enabled,
                ax_index: None,
                accessibility_identifier: None,
                selected: None,
                ax_selected_settable: None,
                cells_complete: false,
                cells: Vec::new(),
            }],
            columns_complete: false,
            columns: Vec::new(),
        }];
        sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
        assert!(
            snapshot.validate().is_ok(),
            "the table scope is actual Source data"
        );
        core.admit(snapshot, 10, &pack).unwrap();
        let env = core.latest().unwrap();
        let mut reducer = BindingReducer::new();

        assert!(
            reducer
                .bind_instance(env, SemanticTargetRef::new("table/row").unwrap(), &row_ref,)
                .is_none(),
            "a whole-structure Engine bind must not promote TableViewport evidence"
        );
        assert_eq!(reducer.all().count(), 0);
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
