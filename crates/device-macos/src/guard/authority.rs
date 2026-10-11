//! Private, read-only native authority probes. An eligible reply is never an
//! input token. Physical submission needs an ordered native commit operation.
use std::collections::BTreeMap;

use execution_core::backend::SubmitContext;
use interaction_contracts::desktop::binding::{
    BindingContinuityProof, BindingProof, BindingProofScope, DeviceContextWitness,
    FocusedControlScope, FocusedKeyProfile, LayoutIndependentSpecialKey,
    MAX_DEVICE_CONTEXT_WITNESS_BYTES, MenuBindingScope,
};
use interaction_contracts::desktop::device::{
    CoordinateSpace, ObservationPurpose, PermitRef, SourceFreshness, SurfaceRef,
};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::primitive::{AxActionKind, Modifiers, MouseButton, Primitive};
use interaction_contracts::desktop::source::ObjectIdentityContinuity;
use serde::{Deserialize, Serialize, Serializer};

const MAX_NATIVE_PROBE_REQUEST_BYTES: usize = 8192;

#[derive(Debug, Clone, Serialize)]
pub struct NativeMouse {
    pub button: MouseButton,
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeScroll {
    pub dx: i32,
    pub dy: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeKey {
    pub name: String,
    pub modifiers: Modifiers,
}

/// The native helper gets only the immutable proof fields it needs. The
/// current, ref-free Source context witness travels once at request level.
#[derive(Serialize)]
struct NativeSourceProofWire<'a> {
    binding: &'a interaction_contracts::desktop::ids::BindingRef,
    instance: &'a interaction_contracts::desktop::ids::ObjectRef,
    snapshot_id: &'a interaction_contracts::desktop::ids::RequestId,
    environment_revision: u64,
    layer_revision: u64,
    fingerprint: &'a interaction_contracts::desktop::binding::InstanceFingerprint,
    display_frame: interaction_contracts::desktop::source::Rect,
    continuity: &'a BindingContinuityProof,
    menu_scope: &'a Option<MenuBindingScope>,
    focus_scope: &'a Option<FocusedControlScope>,
}

fn serialize_native_source_proof<S>(proof: &BindingProof, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    NativeSourceProofWire {
        binding: &proof.binding,
        instance: &proof.instance,
        snapshot_id: &proof.snapshot_id,
        environment_revision: proof.environment_revision,
        layer_revision: proof.layer_revision,
        fingerprint: &proof.fingerprint,
        display_frame: proof.display_frame,
        continuity: &proof.continuity,
        menu_scope: &proof.menu_scope,
        focus_scope: &proof.focus_scope,
    }
    .serialize(serializer)
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeProbeRequest {
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
    #[serde(serialize_with = "serialize_native_source_proof")]
    pub original_proof: BindingProof,
    #[serde(serialize_with = "serialize_native_source_proof")]
    pub current_proof: BindingProof,
    pub context_witness: DeviceContextWitness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinate_space: Option<CoordinateSpace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_domain: Option<ExecutionDomain>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<ObservationPurpose>,
    pub binding_digest: String,
    pub dependencies: BTreeMap<String, u64>,
    pub action: &'static str,
    pub x: f64,
    pub y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse: Option<NativeMouse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll: Option<NativeScroll>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<NativeKey>,
}

impl NativeProbeRequest {
    pub fn build(
        request_id: String,
        context: &SubmitContext<'_>,
        primitive: &Primitive,
        guard_revision: u64,
        now_ms: u64,
        epoch: u64,
    ) -> Result<Self, String> {
        let intent = context.intent;
        let authority = &context.authority;
        let evidence = &authority.evidence;
        let started_ms = authority
            .command_end_ms
            .checked_sub(intent.duration_ms)
            .ok_or("native command duration underflow")?;
        intent.validate()?;
        evidence.validate()?;
        let original = intent
            .binding_proof
            .as_ref()
            .ok_or("missing original native proof")?;
        let current = evidence
            .binding_proof
            .as_ref()
            .ok_or("missing current native proof")?;
        let background_ax = evidence.purpose == ObservationPurpose::BackgroundAxAction;
        if background_ax {
            validate_background_ax_continuity(original, current, evidence.source_sequence)?;
            if intent.execution_domain != ExecutionDomain::BackgroundAx
                || evidence.execution_domain != ExecutionDomain::BackgroundAx
                || intent.coordinate_space != CoordinateSpace::TargetObject
                || !matches!(
                    primitive,
                    Primitive::AxAction {
                        action: AxActionKind::Press
                    }
                )
            {
                return Err(
                    "background AX native submission needs its exact domain, purpose, coordinate space, and Press primitive".into(),
                );
            }
        } else if evidence.purpose == ObservationPurpose::RowSelection {
            validate_row_selection_continuity(original, current, evidence.source_sequence)?;
            validate_row_selection_purpose(context, primitive)?;
        } else if evidence.purpose == ObservationPurpose::WindowActivation {
            validate_window_activation_continuity(original, current, evidence.source_sequence)?;
            if !matches!(primitive, Primitive::ActivateWindow { app } if app == &original.fingerprint.app)
                || original.menu_scope.is_some()
                || current.menu_scope.is_some()
                || original.focus_scope.is_some()
                || current.focus_scope.is_some()
                || evidence.valid_until_ms > evidence.observed_ms.saturating_add(2000)
            {
                return Err(
                    "window activation requires its dedicated primitive and bounded root evidence"
                        .into(),
                );
            }
            if let DeviceContextWitness::WindowRootV1 { source, .. } = &current.device_context {
                let DeviceContextWitness::WindowRootV1 {
                    source: original_source,
                    ..
                } = &original.device_context
                else {
                    return Err("window activation cannot mix witness scopes".into());
                };
                if source.session_epoch != evidence.session_epoch
                    || source.verified_observed_ms != evidence.observed_ms
                    || original_source.session_epoch != intent.session_epoch
                {
                    return Err(
                        "window activation witness does not match its actual observation".into(),
                    );
                }
            }
        } else {
            validate_binding_continuity(original, current, evidence.source_sequence)?;
            validate_focus_purpose(context, primitive)?;
            if evidence.purpose != ObservationPurpose::FocusedControl {
                validate_menu_purpose(context, primitive)?;
            }
        }
        if intent.session_epoch != epoch
            || evidence.session_epoch != epoch
            || (background_ax && intent.coordinate_space != CoordinateSpace::TargetObject)
            || (!background_ax && intent.coordinate_space != CoordinateSpace::DisplayPoints)
            || evidence.clock_domain != format!("macos/{epoch}/wall")
            || intent.surface != evidence.surface
            || intent.target_ref != evidence.target_ref
            || intent.dependencies != evidence.dependencies
            || authority.permit != intent.permit
            || evidence.freshness != SourceFreshness::Verified
            || evidence.observed_ms > now_ms
            || !original.same_instance(current)
            || current.environment_revision < original.environment_revision
            || authority.valid_until_ms <= now_ms
            || authority.valid_until_ms > evidence.valid_until_ms
            || authority.valid_until_ms > authority.command_end_ms
            || authority.valid_until_ms.saturating_sub(now_ms) > 1500
            || started_ms > now_ms
            || started_ms > intent.latest_start_ms
            || intent.duration_ms > 1500
            || intent.steps.len() != 1
            || intent.steps[0].offset_ms != 0
            || &intent.steps[0].primitive != primitive
        {
            return Err("native probe context, source, step or authority mismatch".into());
        }
        let (action, x, y, value) = if background_ax {
            // TargetObject names a semantic AX element. These sentinel values
            // are inert and must never be interpreted as screen coordinates.
            ("ax_press", 0.0, 0.0, None)
        } else {
            match primitive {
                Primitive::KeyPress { .. }
                    if evidence.purpose == ObservationPurpose::FocusedControl =>
                {
                    let (x, y) = original.display_frame.center();
                    ("key_press", x, y, None)
                }
                Primitive::ActivateWindow { app }
                    if evidence.purpose == ObservationPurpose::WindowActivation
                        && app == &original.fingerprint.app =>
                {
                    let (x, y) = original.display_frame.center();
                    ("activate_window", x, y, None)
                }
                Primitive::AxAction {
                    action: AxActionKind::Press,
                } => {
                    let (x, y) = original.display_frame.center();
                    (
                        if evidence.purpose == ObservationPurpose::MenuSelection {
                            "ax_menu_press"
                        } else {
                            "ax_press"
                        },
                        x,
                        y,
                        None,
                    )
                }
                Primitive::AxAction {
                    action: AxActionKind::SetValue { value },
                } => {
                    let role = original.fingerprint.role.as_str();
                    let allowed = match role {
                        "AXCheckBox" | "AXRadioButton" => ["on", "off"].contains(&value.as_str()),
                        "AXTextField" | "AXTextArea" => {
                            [
                                "com.lingxi.computer-user.guard-fixture",
                                "com.lingxi.computer-user.mvp-fixture",
                                "com.apple.TextEdit",
                            ]
                            .contains(&original.fingerprint.app.as_str())
                                && original
                                    .fingerprint
                                    .subrole
                                    .as_deref()
                                    .is_none_or(|s| s.is_empty())
                        }
                        _ => false,
                    };
                    if !allowed
                        || value.len() > 2048
                        || value
                            .chars()
                            .any(|c| c.is_control() && !['\n', '\r', '\t'].contains(&c))
                    {
                        return Err("native AX value outside registered role or byte policy".into());
                    }
                    let (x, y) = original.display_frame.center();
                    ("ax_set_value", x, y, Some(value.clone()))
                }
                Primitive::AxAction {
                    action: AxActionKind::ShowMenu,
                } => {
                    if !["AXMenuButton", "AXPopUpButton"]
                        .contains(&original.fingerprint.role.as_str())
                    {
                        return Err(
                            "native show menu requires a registered menu control role".into()
                        );
                    }
                    let (x, y) = original.display_frame.center();
                    ("ax_show_menu", x, y, None)
                }
                Primitive::AxAction {
                    action: AxActionKind::SetSelectedTrue,
                } if evidence.purpose == ObservationPurpose::RowSelection => {
                    let (x, y) = original
                        .row_selection_point()
                        .ok_or("native row selection requires its proof-bound hit point")?;
                    ("ax_set_selected_true", x, y, None)
                }
                Primitive::PointerPosition { x, y } => {
                    if ![
                        "AXScrollArea",
                        "AXButton",
                        "AXCheckBox",
                        "AXRadioButton",
                        "AXMenuItem",
                    ]
                    .contains(&original.fingerprint.role.as_str())
                    {
                        return Err(
                            "native pointer move requires a registered pointer target role".into(),
                        );
                    }
                    ("pointer_move", f64::from(*x), f64::from(*y), None)
                }
                Primitive::Scroll { dx, dy } => {
                    if original.fingerprint.role != "AXScrollArea"
                        || !(-120..=120).contains(dx)
                        || !(-120..=120).contains(dy)
                        || (*dx == 0 && *dy == 0)
                    {
                        return Err(
                            "native scroll requires a scroll area and bounded nonzero pixel deltas"
                                .into(),
                        );
                    }
                    let (x, y) = original.display_frame.center();
                    ("scroll", x.round(), y.round(), None)
                }
                Primitive::Click { x, y, .. } => ("click", f64::from(*x), f64::from(*y), None),
                _ => {
                    return Err(
                    "native authority probes support only bounded atomic pointer and registered AX actions"
                        .into(),
                );
                }
            }
        };
        if (action == "activate_window")
            != (evidence.purpose == ObservationPurpose::WindowActivation)
        {
            return Err("native primitive and observation purpose differ".into());
        }
        if (!background_ax && !original.display_frame.contains(x, y))
            || intent.dependencies.len() > 16
            || intent
                .dependencies
                .iter()
                .any(|(k, v)| k.len() > 64 || *v == 0)
        {
            return Err("native probe geometry or dependency budget invalid".into());
        }
        let window = original
            .fingerprint
            .window_id
            .strip_prefix("window-")
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0);
        if window.is_none() || original.fingerprint.app_pid > i32::MAX as u32 {
            return Err("native process/window identity invalid".into());
        }
        let request = Self {
            request_id,
            intent_id: intent.intent_id.clone(),
            attempt_id: intent.attempt_id.clone(),
            task_id: intent.task_id.clone(),
            task_revision: intent.task_revision,
            root_epoch: intent.root_epoch,
            permit: intent.permit.clone(),
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
            context_witness: current.device_context.clone(),
            coordinate_space: background_ax.then_some(CoordinateSpace::TargetObject),
            execution_domain: background_ax.then_some(ExecutionDomain::BackgroundAx),
            purpose: background_ax.then_some(ObservationPurpose::BackgroundAxAction),
            binding_digest: original.fingerprint.digest(),
            dependencies: intent.dependencies.clone(),
            action,
            x,
            y,
            value,
            mouse: match primitive {
                Primitive::Click {
                    button, modifiers, ..
                } => Some(NativeMouse {
                    button: *button,
                    modifiers: *modifiers,
                }),
                _ => None,
            },
            scroll: match primitive {
                Primitive::Scroll { dx, dy } => Some(NativeScroll { dx: *dx, dy: *dy }),
                _ => None,
            },
            key: match primitive {
                Primitive::KeyPress { key, modifiers } => Some(NativeKey {
                    name: key.clone(),
                    modifiers: *modifiers,
                }),
                _ => None,
            },
        };
        request.validate_wire_budget()?;
        Ok(request)
    }

    fn validate_wire_budget(&self) -> Result<(), String> {
        let witness_size = serde_json::to_vec(&self.context_witness)
            .map_err(|_| "native context witness cannot be serialized")?
            .len();
        if witness_size > MAX_DEVICE_CONTEXT_WITNESS_BYTES {
            return Err("native context witness exceeds the 4096-byte bound".into());
        }
        let request_size = serde_json::to_vec(self)
            .map_err(|_| "native probe request cannot be serialized")?
            .len();
        if request_size > MAX_NATIVE_PROBE_REQUEST_BYTES {
            return Err("native probe request exceeds the 8192-byte bound".into());
        }
        Ok(())
    }
}

fn validate_background_ax_continuity(
    original: &BindingProof,
    current: &BindingProof,
    current_source_sequence: u64,
) -> Result<(), String> {
    original.continuity.validate()?;
    current.continuity.validate()?;
    current.device_context.validate()?;
    if !original.supports_background_ax_press()
        || !current.supports_background_ax_press()
        || !original.continuity.same_closure(&current.continuity)
        || current.continuity.source_sequence != current_source_sequence
        || !current
            .device_context
            .supports_execution_domain(ExecutionDomain::BackgroundAx)
    {
        return Err("background AX proof scope, target context or source sequence changed".into());
    }
    let DeviceContextWitness::BackgroundAxV1 {
        app: original_app,
        window: original_window,
        source: original_source,
        target_context_hash: original_hash,
        ..
    } = &original.device_context
    else {
        return Err("background AX original proof has the wrong witness scope".into());
    };
    let DeviceContextWitness::BackgroundAxV1 {
        app,
        window,
        source,
        target_context_hash,
        ..
    } = &current.device_context
    else {
        return Err("background AX current proof has the wrong witness scope".into());
    };
    let original_scope_hash = match &original.continuity.scope {
        BindingProofScope::BackgroundAx {
            target_context_hash,
        } => target_context_hash,
        _ => return Err("background AX original binding scope is missing".into()),
    };
    let current_scope_hash = match &current.continuity.scope {
        BindingProofScope::BackgroundAx {
            target_context_hash,
        } => target_context_hash,
        _ => return Err("background AX current binding scope is missing".into()),
    };
    if original_app != app
        || original_window != window
        || original_hash != target_context_hash
        || original_scope_hash != current_scope_hash
        || original_scope_hash != original_hash
        || current_scope_hash != target_context_hash
        || original_source.window.process != source.window.process
        || original_source.window.source_session_id != source.window.source_session_id
        || original_source.window.session_epoch != source.window.session_epoch
        || original_source.window.origin_window_id != source.window.origin_window_id
        || original_source.window.origin_window_frame != source.window.origin_window_frame
        || original_source.window.verified_source_sequence != original.continuity.source_sequence
        || source.window.verified_source_sequence != current_source_sequence
    {
        return Err("background AX proof does not retain one exact native target context".into());
    }
    Ok(())
}

/// Enforce the whole-structure binding closure on both Source proofs before
/// native submission. Row selection uses its separate viewport comparator below.
/// The `bc1` value remains an opaque Source commitment here: this code pins it
/// across the admitted/current proofs, while Swift independently rebuilds the
/// actual AX ancestry/fingerprint during the atomic hit test. It does not claim
/// to recompute that digest from the live AX tree.
fn validate_binding_continuity(
    original: &BindingProof,
    current: &BindingProof,
    current_source_sequence: u64,
) -> Result<(), String> {
    original.continuity.validate()?;
    current.continuity.validate()?;
    current.device_context.validate()?;
    if !original.supports_native_input() || !current.supports_native_input() {
        return Err("native device has no input comparator for this context witness".into());
    }
    if !matches!(
        &original.continuity.scope,
        BindingProofScope::WholeStructure
    ) || !matches!(&current.continuity.scope, BindingProofScope::WholeStructure)
    {
        return Err("whole-structure input cannot use a table viewport binding scope".into());
    }
    if !original.continuity.same_closure(&current.continuity)
        || current.continuity.source_sequence != current_source_sequence
    {
        return Err("native binding context closure or current Source sequence changed".into());
    }
    match original.continuity.kind {
        ObjectIdentityContinuity::NotificationCovered => {
            if current.continuity.kind != ObjectIdentityContinuity::NotificationCovered
                || current.continuity.source_sequence < original.continuity.source_sequence
            {
                return Err("native notification-covered continuity regressed".into());
            }
        }
        ObjectIdentityContinuity::ReadScoped => {
            if original.continuity.source_sequence != current_source_sequence
                || current.continuity != original.continuity
                || current != original
            {
                return Err(
                    "read-scoped native binding is valid only for its exact Source read".into(),
                );
            }
        }
    }
    Ok(())
}

/// Activation can consume a dedicated root proof while ordinary input retains
/// the whole-structure comparator. Both proofs must describe the same frozen
/// scope and process/window identity; a new read supplies real new provenance.
fn validate_window_activation_continuity(
    original: &BindingProof,
    current: &BindingProof,
    current_source_sequence: u64,
) -> Result<(), String> {
    if matches!(original.continuity.scope, BindingProofScope::WholeStructure)
        && matches!(current.continuity.scope, BindingProofScope::WholeStructure)
    {
        return validate_binding_continuity(original, current, current_source_sequence);
    }
    original.validate()?;
    current.validate()?;
    if !original.supports_window_activation()
        || !current.supports_window_activation()
        || !matches!(
            original.continuity.scope,
            BindingProofScope::WindowRoot { .. }
        )
        || !matches!(
            current.continuity.scope,
            BindingProofScope::WindowRoot { .. }
        )
        || !original.continuity.same_closure(&current.continuity)
        || current.continuity.source_sequence != current_source_sequence
    {
        return Err(
            "window activation root scope, closure or current Source sequence changed".into(),
        );
    }
    let (
        DeviceContextWitness::WindowRootV1 {
            app: old_app,
            window: old_window,
            root: old_root,
            source: old_source,
        },
        DeviceContextWitness::WindowRootV1 {
            app,
            window,
            root,
            source,
        },
    ) = (&original.device_context, &current.device_context)
    else {
        return Err("window activation requires matching dedicated root witnesses".into());
    };
    if old_app != app
        || old_window != window
        || old_root != root
        || old_source.source_session_id != source.source_session_id
        || old_source.session_epoch != source.session_epoch
        || old_source.process != source.process
        || old_source.origin_window_id != source.origin_window_id
        || old_source.origin_window_frame != source.origin_window_frame
        || old_source.verified_source_sequence != original.continuity.source_sequence
        || source.verified_source_sequence != current_source_sequence
        || source.verified_source_sequence < old_source.verified_source_sequence
        || source.verified_observed_ms < old_source.verified_observed_ms
    {
        return Err("window activation process, window or Source provenance changed".into());
    }
    match original.continuity.kind {
        ObjectIdentityContinuity::NotificationCovered => {
            if current.continuity.kind != ObjectIdentityContinuity::NotificationCovered {
                return Err("window activation continuity lost notification coverage".into());
            }
        }
        ObjectIdentityContinuity::ReadScoped => {
            if original != current || original.continuity.source_sequence != current_source_sequence
            {
                return Err(
                    "read-scoped window activation is valid only for its exact Source read".into(),
                );
            }
        }
    }
    Ok(())
}

/// Row input has its own comparator and remains scoped to one complete local
/// table viewport. It must never use the whole-structure native-input gate.
fn validate_row_selection_continuity(
    original: &BindingProof,
    current: &BindingProof,
    current_source_sequence: u64,
) -> Result<(), String> {
    original.continuity.validate()?;
    current.continuity.validate()?;
    current.device_context.validate()?;
    if !original.supports_row_selection() || !current.supports_row_selection() {
        return Err("native row selection requires its dedicated table viewport witness".into());
    }
    if !matches!(
        &original.continuity.scope,
        BindingProofScope::TableViewport { .. }
    ) || !matches!(
        &current.continuity.scope,
        BindingProofScope::TableViewport { .. }
    ) {
        return Err("row selection cannot use whole-structure binding evidence".into());
    }
    if !original.continuity.same_closure(&current.continuity)
        || original.continuity.source_sequence != current_source_sequence
        || current.continuity.source_sequence != current_source_sequence
        || original.continuity != current.continuity
        || original.snapshot_id != current.snapshot_id
        || !original.same_instance(current)
        || current.environment_revision < original.environment_revision
    {
        return Err("native row input requires the exact original/current Source read".into());
    }
    Ok(())
}

fn validate_row_selection_purpose(
    context: &SubmitContext<'_>,
    primitive: &Primitive,
) -> Result<(), String> {
    let intent = context.intent;
    let evidence = &context.authority.evidence;
    let original = intent
        .binding_proof
        .as_ref()
        .ok_or("row input needs an original table viewport proof")?;
    let current = evidence
        .binding_proof
        .as_ref()
        .ok_or("row input needs current table viewport evidence")?;
    if evidence.purpose != ObservationPurpose::RowSelection
        || !matches!(
            primitive,
            Primitive::AxAction {
                action: AxActionKind::SetSelectedTrue
            }
        )
        || !original.supports_row_selection()
        || !current.supports_row_selection()
        || original.menu_scope.is_some()
        || current.menu_scope.is_some()
        || original.focus_scope.is_some()
        || current.focus_scope.is_some()
        || intent.owned_document_proof.is_some()
        || evidence.owned_document_proof.is_some()
        || !["focus", "layer", "binding", "authorization", "geometry"]
            .iter()
            .all(|name| evidence.dependencies.contains_key(*name))
    {
        return Err(
            "native AXSelected=true requires exact row-selection purpose and dependencies".into(),
        );
    }
    Ok(())
}

fn validate_focus_purpose(
    context: &SubmitContext<'_>,
    primitive: &Primitive,
) -> Result<(), String> {
    let original = context
        .intent
        .binding_proof
        .as_ref()
        .ok_or("missing focus input proof")?;
    let current = context
        .authority
        .evidence
        .binding_proof
        .as_ref()
        .ok_or("missing current focus proof")?;
    let focused = context.authority.evidence.purpose == ObservationPurpose::FocusedControl;
    if !focused {
        if original.focus_scope.is_some()
            || current.focus_scope.is_some()
            || matches!(primitive, Primitive::KeyPress { .. })
        {
            return Err("native key input requires focused-control evidence".into());
        }
        return Ok(());
    }
    let scope = original
        .focus_scope
        .as_ref()
        .ok_or("native key input requires actual focus scope")?;
    let fresh = current
        .focus_scope
        .as_ref()
        .ok_or("native key input requires current focus scope")?;
    if original.menu_scope.is_some()
        || current.menu_scope.is_some()
        || !original.same_instance(current)
        || scope.profile != FocusedKeyProfile::MacosAtomicSpecialKeyV1
        || scope.key != LayoutIndependentSpecialKey::Space
        || original.fingerprint.role != "AXCheckBox"
        || !matches!(primitive, Primitive::KeyPress { key, modifiers }
            if key == scope.key.canonical_name() && *modifiers == Modifiers::default())
        || fresh.context.verified_source_sequence < scope.context.verified_source_sequence
        || fresh.context.verified_observed_ms < scope.context.verified_observed_ms
        || ["focus", "layer", "binding", "authorization", "geometry"]
            .iter()
            .any(|name| !context.authority.evidence.dependencies.contains_key(*name))
    {
        return Err("native focused key profile, receiver or source provenance mismatch".into());
    }
    Ok(())
}

/// Value-level admission only. The native worker independently proves actual
/// AX relationships and the settled opener audit before selecting an item.
fn validate_menu_purpose(context: &SubmitContext<'_>, primitive: &Primitive) -> Result<(), String> {
    let intent = context.intent;
    let evidence = &context.authority.evidence;
    let original = intent
        .binding_proof
        .as_ref()
        .ok_or("native input needs a binding proof")?;
    let current = evidence
        .binding_proof
        .as_ref()
        .ok_or("native input needs current binding proof")?;
    match (evidence.purpose, &original.menu_scope, primitive) {
        (
            ObservationPurpose::MenuSelection,
            Some(MenuBindingScope::Item { opened_by, .. }),
            Primitive::AxAction {
                action: AxActionKind::Press,
            },
        ) => {
            if intent.attempt_id.as_deref() != Some(opened_by.attempt_id.as_str())
                || opened_by.intent_id == intent.intent_id
                || [
                    "focus",
                    "layer",
                    "binding",
                    "authorization",
                    "geometry",
                    "menu",
                ]
                .iter()
                .any(|name| !evidence.dependencies.contains_key(*name))
            {
                return Err("menu selection attempt or source provenance mismatch".into());
            }
        }
        (
            ObservationPurpose::TargetInput,
            Some(MenuBindingScope::Opener { .. }),
            Primitive::AxAction {
                action: AxActionKind::ShowMenu,
            },
        ) => {
            if intent.attempt_id.is_none() {
                return Err("menu opener attempt or source provenance mismatch".into());
            }
        }
        (ObservationPurpose::TargetInput | ObservationPurpose::WindowActivation, None, _) => {}
        _ => return Err("native observation purpose, menu scope and primitive differ".into()),
    }
    if !original.same_instance(current) {
        return Err("native menu proof identity changed".into());
    }
    // Current Source provenance may refresh while protected scope stays exact.
    let anchor = match &current.menu_scope {
        Some(MenuBindingScope::Opener { anchor, .. }) => Some(anchor),
        Some(MenuBindingScope::Item { context, .. }) => Some(&context.opener),
        None => None,
    };
    if anchor.is_some_and(|anchor| {
        anchor.verified_source_sequence != evidence.source_sequence
            || anchor.verified_observed_ms != evidence.observed_ms
    }) {
        return Err("current native menu source provenance mismatch".into());
    }
    let original_anchor = match &original.menu_scope {
        Some(MenuBindingScope::Opener { anchor, .. }) => Some(anchor),
        Some(MenuBindingScope::Item { context, .. }) => Some(&context.opener),
        None => None,
    };
    if original_anchor.zip(anchor).is_some_and(|(old, new)| {
        new.verified_source_sequence < old.verified_source_sequence
            || new.verified_observed_ms < old.verified_observed_ms
    }) {
        return Err("native menu source provenance regressed".into());
    }
    let ceiling = match &original.menu_scope {
        Some(MenuBindingScope::Opener {
            attempt_deadline_ms,
            ..
        }) => Some(*attempt_deadline_ms),
        Some(MenuBindingScope::Item { opened_by, .. }) => Some(opened_by.attempt_deadline_ms),
        None => None,
    };
    if ceiling.is_some_and(|ceiling| context.authority.command_end_ms > ceiling) {
        return Err("native menu command exceeds its owning attempt deadline".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProbeResponse {
    pub protocol_version: u32,
    pub kind: String,
    pub session_epoch: u64,
    pub request_id: String,
    pub outcome: String,
    pub reason: String,
    pub binding_digest: Option<String>,
    pub input_authorized: bool,
}

impl NativeProbeResponse {
    pub fn valid_for(&self, request: &NativeProbeRequest, epoch: u64) -> bool {
        self.protocol_version == 1
            && self.kind == "authority_probe"
            && self.session_epoch == epoch
            && self.request_id == request.request_id
            && !self.input_authorized
            && self.reason.len() <= 256
            && matches!(
                self.outcome.as_str(),
                "eligible"
                    | "rejected"
                    | "expired"
                    | "revoked"
                    | "stop_latched"
                    | "context_unresolved"
                    | "target_occluded"
            )
            && if self.outcome == "eligible" {
                self.binding_digest.as_deref() == Some(request.binding_digest.as_str())
            } else {
                self.binding_digest.is_none()
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use execution_core::backend::SubmissionAuthority;
    use interaction_contracts::desktop::{
        device::{ActionIntent, ExecutionIntent, ExecutionObservation},
        ids::ObjectRef,
        message::Message,
        samples,
    };

    fn device_fingerprint(
        fingerprint: &interaction_contracts::desktop::binding::InstanceFingerprint,
    ) -> interaction_contracts::desktop::binding::DeviceContextFingerprint {
        interaction_contracts::desktop::binding::DeviceContextFingerprint {
            app: fingerprint.app.clone(),
            app_pid: fingerprint.app_pid,
            window_id: fingerprint.window_id.clone(),
            ancestor_roles: fingerprint.ancestor_roles.clone(),
            group: fingerprint.group.clone(),
            role: fingerprint.role.clone(),
            subrole: fingerprint.subrole.clone(),
            label: fingerprint.label.clone(),
            sibling_index: fingerprint.sibling_index,
            frame: fingerprint.frame,
        }
    }

    /// Keep synthetic Device proofs structurally aligned with their semantic
    /// fingerprint. These fixtures model value-only AX context; they do not
    /// derive or grant production authority.
    fn sync_device_context(proof: &mut BindingProof) {
        use interaction_contracts::desktop::binding::{
            DeviceContextApp, DeviceContextComposition, DeviceContextDrawingWindow,
            DeviceContextFocusRelation, DeviceContextMenuRelation, DeviceContextObject,
            DeviceContextScrollAncestor, DeviceContextWitness,
        };
        use interaction_contracts::desktop::source::LayerKind;

        let DeviceContextWitness::WholeStructureV1 {
            active_layer,
            window: old_window,
            target: old_target,
            ancestry: old_ancestry,
            siblings: old_siblings,
            scroll_ancestors: old_scroll_ancestors,
            ..
        } = &proof.device_context
        else {
            return;
        };
        let mut window = (**old_window).clone();
        let mut target = (**old_target).clone();
        let active_layer = active_layer.clone();
        let old_ancestry = old_ancestry.as_slice().to_vec();
        let old_siblings = old_siblings.as_slice().to_vec();
        let old_scroll_ancestors = old_scroll_ancestors.as_slice().to_vec();
        let fingerprint = &proof.fingerprint;

        let menu = match proof.menu_scope.as_ref() {
            Some(MenuBindingScope::Opener { anchor, .. }) => {
                Some(DeviceContextMenuRelation::OpenerAnchor {
                    origin_window_id: anchor.origin_window_id.clone(),
                    origin_window_frame: anchor.origin_window_frame,
                    opener_fingerprint: Box::new(device_fingerprint(&anchor.opener_fingerprint)),
                    continuity: anchor.continuity,
                })
            }
            Some(MenuBindingScope::Item { context, .. }) => {
                Some(DeviceContextMenuRelation::AnchoredMenu {
                    origin_window_id: context.opener.origin_window_id.clone(),
                    origin_window_frame: context.opener.origin_window_frame,
                    opener_fingerprint: Box::new(device_fingerprint(
                        &context.opener.opener_fingerprint,
                    )),
                    continuity: context.opener.continuity,
                    menu_fingerprint: Box::new(device_fingerprint(&context.menu_fingerprint)),
                    menu_frame: context.menu_frame,
                    drawing_window: DeviceContextDrawingWindow {
                        window_id: context.drawing_window.window_id,
                        owner_pid: context.drawing_window.owner_pid,
                        layer: context.drawing_window.layer,
                        frame: context.drawing_window.frame,
                    },
                })
            }
            None => None,
        };
        let focus = proof.focus_scope.as_ref().map(|scope| {
            let context = &scope.context;
            DeviceContextFocusRelation {
                origin_window_id: context.origin_window_id.clone(),
                origin_window_frame: context.origin_window_frame,
                target_fingerprint: device_fingerprint(&context.target_fingerprint),
                target_frame: context.target_frame,
                relation: context.relation,
                composition: match &context.composition {
                    interaction_contracts::desktop::source::CompositionState::Unknown => {
                        DeviceContextComposition::Unknown
                    }
                    interaction_contracts::desktop::source::CompositionState::NotApplicable => {
                        DeviceContextComposition::NotApplicable
                    }
                    interaction_contracts::desktop::source::CompositionState::Marked { .. } => {
                        DeviceContextComposition::Marked
                    }
                    interaction_contracts::desktop::source::CompositionState::Unmarked {
                        ..
                    } => DeviceContextComposition::Unmarked,
                },
            }
        });

        let context_window_frame = menu
            .as_ref()
            .map(|relation| match relation {
                DeviceContextMenuRelation::OpenerAnchor {
                    origin_window_frame,
                    ..
                }
                | DeviceContextMenuRelation::AnchoredMenu {
                    origin_window_frame,
                    ..
                } => *origin_window_frame,
            })
            .or_else(|| focus.as_ref().map(|relation| relation.origin_window_frame));
        window.window_id = fingerprint.window_id.clone();
        window.owner_bundle_id = fingerprint.app.clone();
        if let Some(frame) = context_window_frame {
            window.frame = frame;
        }

        let mut ancestry = Vec::with_capacity(fingerprint.ancestor_roles.len());
        for (index, role) in fingerprint.ancestor_roles.iter().enumerate() {
            let descriptor = if index == 0 && role == "AXWindow" {
                DeviceContextObject {
                    role: role.clone(),
                    subrole: Some("AXStandardWindow".into()),
                    label: window.title.clone().unwrap_or_default(),
                    frame: window.frame,
                }
            } else if let Some(MenuBindingScope::Item { context, .. }) = proof.menu_scope.as_ref() {
                match role.as_str() {
                    "AXMenuButton" | "AXPopUpButton" => DeviceContextObject {
                        role: role.clone(),
                        subrole: context.opener.opener_fingerprint.subrole.clone(),
                        label: context.opener.opener_fingerprint.label.clone(),
                        frame: context.opener.opener_frame,
                    },
                    "AXMenu" => DeviceContextObject {
                        role: role.clone(),
                        subrole: context.menu_fingerprint.subrole.clone(),
                        label: context.menu_fingerprint.label.clone(),
                        frame: context.menu_frame,
                    },
                    _ => old_ancestry
                        .iter()
                        .find(|object| object.role == *role)
                        .cloned()
                        .unwrap_or_else(|| synthetic_ancestor(role, fingerprint, window.frame)),
                }
            } else {
                old_ancestry
                    .iter()
                    .find(|object| object.role == *role)
                    .cloned()
                    .unwrap_or_else(|| synthetic_ancestor(role, fingerprint, window.frame))
            };
            ancestry.push(descriptor);
        }
        if ancestry.is_empty() && window.kind == LayerKind::Window {
            window.frame = proof.display_frame;
        }

        target.object = DeviceContextObject {
            role: fingerprint.role.clone(),
            subrole: fingerprint.subrole.clone(),
            label: fingerprint.label.clone(),
            frame: proof.display_frame,
        };
        target.group = fingerprint.group.clone();
        let target_sibling_index = fingerprint.sibling_index as usize;
        let mut siblings = old_siblings;
        assert!(target_sibling_index < siblings.len());
        siblings[target_sibling_index] = target.object.clone();
        let nearest_labelled_group_ancestry_index = ancestry
            .iter()
            .enumerate()
            .rev()
            .find(|(_, object)| object.role == "AXGroup" && !object.label.is_empty())
            .map(|(index, _)| index as u8);
        let scroll_ancestors = ancestry
            .iter()
            .enumerate()
            .filter(|(_, object)| object.role == "AXScrollArea")
            .map(|(index, _)| DeviceContextScrollAncestor {
                ancestry_index: index as u8,
                state: old_scroll_ancestors
                    .iter()
                    .find(|old| old.ancestry_index as usize == index)
                    .and_then(|old| old.state.clone()),
            })
            .collect();
        proof.device_context = DeviceContextWitness::WholeStructureV1 {
            table_column: None,
            app: Box::new(DeviceContextApp {
                bundle_id: fingerprint.app.clone(),
                pid: fingerprint.app_pid,
            }),
            active_layer,
            window: Box::new(window),
            target: Box::new(target),
            sibling_parent_ancestry_index: ancestry.len().checked_sub(1).map(|n| n as u8),
            ancestry,
            siblings,
            target_sibling_index: target_sibling_index as u16,
            nearest_labelled_group_ancestry_index,
            scroll_ancestors,
            menu: menu.map(Box::new),
            focus: focus.map(Box::new),
        };
    }

    fn synthetic_ancestor(
        role: &str,
        fingerprint: &interaction_contracts::desktop::binding::InstanceFingerprint,
        frame: interaction_contracts::desktop::source::Rect,
    ) -> interaction_contracts::desktop::binding::DeviceContextObject {
        interaction_contracts::desktop::binding::DeviceContextObject {
            role: role.into(),
            subrole: None,
            label: if role == "AXGroup" {
                fingerprint.group.clone().unwrap_or_default()
            } else {
                String::new()
            },
            frame,
        }
    }

    fn key_fixture() -> (ActionIntent, SubmissionAuthority, Primitive) {
        use interaction_contracts::desktop::binding::FocusedControlScope;
        use interaction_contracts::desktop::source::{
            CompositionState, FocusContext, FocusRelation, KernelProcessIdentity,
            SourceKeyboardContext,
        };
        let (mut intent, mut authority, _) = fixture();
        let proof = intent.binding_proof.as_mut().unwrap();
        proof.fingerprint.role = "AXCheckBox".into();
        let origin_window_frame = match &proof.device_context {
            DeviceContextWitness::WholeStructureV1 { window, .. } => window.frame,
            DeviceContextWitness::TableViewportV1 { window, .. } => window.frame,
            DeviceContextWitness::WindowRootV1 { window, .. } => window.frame,
            DeviceContextWitness::BackgroundAxV1 { source, .. } => {
                source.window.origin_window_frame
            }
            DeviceContextWitness::Unsupported { .. } => {
                panic!("focused key fixture needs an actual Source window context")
            }
        };
        proof.focus_scope = Some(FocusedControlScope {
            profile: FocusedKeyProfile::MacosAtomicSpecialKeyV1,
            key: LayoutIndependentSpecialKey::Space,
            context: FocusContext {
                source_session_id: "actual-watch".into(),
                session_epoch: 7,
                process: KernelProcessIdentity {
                    pid: proof.fingerprint.app_pid,
                    start_seconds: 1,
                    start_micros: 0,
                },
                origin_window_id: proof.fingerprint.window_id.clone(),
                origin_window_frame,
                target_instance: proof.instance.clone(),
                target_fingerprint: proof.fingerprint.clone(),
                target_frame: proof.display_frame,
                focused_instance: proof.instance.clone(),
                relation: FocusRelation::ExactControl,
                composition: CompositionState::NotApplicable,
                keyboard: SourceKeyboardContext {
                    selected_input_source: None,
                    underlying_keyboard_layout: None,
                    keyboard_type: None,
                    caps_lock: Some(false),
                },
                verified_source_sequence: authority.evidence.source_sequence,
                verified_observed_ms: authority.evidence.observed_ms,
            },
        });
        sync_device_context(proof);
        intent.attempt_id = Some("focused-attempt".into());
        intent.dependencies.insert("geometry".into(), 1);
        intent.binding_fingerprint = Some(proof.fingerprint.digest());
        let primitive = Primitive::KeyPress {
            key: "Space".into(),
            modifiers: Modifiers::default(),
        };
        intent.steps[0].primitive = primitive.clone();
        authority.evidence.purpose = ObservationPurpose::FocusedControl;
        authority.evidence.binding_proof = intent.binding_proof.clone();
        authority.evidence.dependencies = intent.dependencies.clone();
        (intent, authority, primitive)
    }

    #[test]
    fn focused_key_wire_preserves_actual_focus_identity_and_has_only_key_payload() {
        let (intent, authority, primitive) = key_fixture();
        let request = NativeProbeRequest::build(
            "key".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        let expected_witness = request.context_witness.clone();
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["action"], "key_press");
        assert_eq!(wire["key"]["name"], "Space");
        assert_eq!(
            wire["key"]["modifiers"],
            serde_json::to_value(Modifiers::default()).unwrap()
        );
        assert_eq!(
            wire["original_proof"]["focus_scope"],
            wire["current_proof"]["focus_scope"]
        );
        assert_eq!(
            wire["original_proof"]["focus_scope"]["profile"],
            "macos_atomic_special_key_v1"
        );
        assert!(
            wire.get("mouse").is_none()
                && wire.get("scroll").is_none()
                && wire.get("value").is_none()
        );
        assert_eq!(
            wire["context_witness"],
            serde_json::to_value(expected_witness).unwrap()
        );
        assert!(wire["original_proof"].get("device_context").is_none());
        assert!(wire["current_proof"].get("device_context").is_none());
    }

    #[test]
    fn focused_key_accepts_forward_source_reads_only_for_the_same_actual_receiver() {
        let (intent, mut authority, primitive) = key_fixture();
        assert_eq!(
            intent.binding_proof.as_ref().unwrap().continuity.kind,
            ObjectIdentityContinuity::NotificationCovered
        );
        let fresh_sequence = authority.evidence.source_sequence + 1;
        let current = authority.evidence.binding_proof.as_mut().unwrap();
        current.environment_revision += 1;
        current.snapshot_id =
            interaction_contracts::desktop::ids::RequestId::new("snap-key-forward").unwrap();
        current.continuity.source_sequence = fresh_sequence;
        let focus = &mut current.focus_scope.as_mut().unwrap().context;
        focus.verified_source_sequence += 1;
        focus.verified_observed_ms += 1;
        authority.evidence.source_sequence = focus.verified_source_sequence;
        authority.evidence.observed_ms = focus.verified_observed_ms;
        assert!(
            NativeProbeRequest::build(
                "key".into(),
                &SubmitContext {
                    intent: &intent,
                    authority: authority.clone()
                },
                &primitive,
                0,
                100,
                7
            )
            .is_ok()
        );
        authority
            .evidence
            .binding_proof
            .as_mut()
            .unwrap()
            .focus_scope
            .as_mut()
            .unwrap()
            .context
            .process
            .start_micros += 1;
        assert!(
            NativeProbeRequest::build(
                "key".into(),
                &SubmitContext {
                    intent: &intent,
                    authority
                },
                &primitive,
                0,
                100,
                7
            )
            .is_err()
        );
    }

    #[test]
    fn focused_key_transport_receipt_distinguishes_unknown_return_clock() {
        use crate::guard::submission::NativeSubmitResponse;
        let (intent, authority, primitive) = key_fixture();
        let request = NativeProbeRequest::build(
            "key".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        let mut receipt = serde_json::json!({
            "protocol_version": 1, "kind": "atomic_submit", "session_epoch": 7,
            "request_id": "key", "outcome": "applied", "reason": "actual key pair returned",
            "submitted": true, "committed_ms": 110, "returned_ms": 112,
            "binding_digest": request.binding_digest,
        });
        let known: NativeSubmitResponse = serde_json::from_value(receipt.clone()).unwrap();
        assert!(known.valid_for(&request, 7));

        let mut applied_missing = receipt.clone();
        applied_missing
            .as_object_mut()
            .unwrap()
            .remove("returned_ms");
        let applied_missing: NativeSubmitResponse =
            serde_json::from_value(applied_missing).unwrap();
        assert!(!applied_missing.valid_for(&request, 7));
        let mut applied_null = receipt.clone();
        applied_null["returned_ms"] = serde_json::Value::Null;
        let applied_null: NativeSubmitResponse = serde_json::from_value(applied_null).unwrap();
        assert!(!applied_null.valid_for(&request, 7));

        receipt["outcome"] = serde_json::json!("unknown");
        let mut unknown_missing = receipt.clone();
        unknown_missing
            .as_object_mut()
            .unwrap()
            .remove("returned_ms");
        let unknown_missing: NativeSubmitResponse =
            serde_json::from_value(unknown_missing).unwrap();
        assert!(unknown_missing.valid_for(&request, 7));
        receipt["returned_ms"] = serde_json::Value::Null;
        let unknown_null: NativeSubmitResponse = serde_json::from_value(receipt.clone()).unwrap();
        assert!(unknown_null.valid_for(&request, 7));

        receipt["returned_ms"] = serde_json::json!(109);
        let reversed: NativeSubmitResponse = serde_json::from_value(receipt.clone()).unwrap();
        assert!(!reversed.valid_for(&request, 7));

        receipt["returned_ms"] = serde_json::json!(u64::MAX);
        let overflow: NativeSubmitResponse = serde_json::from_value(receipt.clone()).unwrap();
        assert!(!overflow.valid_for(&request, 7));

        receipt["returned_ms"] = serde_json::Value::Null;
        receipt["binding_digest"] = serde_json::json!("fp1:foreign-context");
        let foreign_context: NativeSubmitResponse = serde_json::from_value(receipt).unwrap();
        assert!(!foreign_context.valid_for(&request, 7));
    }

    #[test]
    fn focused_key_rejects_wrong_purpose_receiver_provenance_profile_and_payload() {
        for change in 0..14 {
            let (mut intent, mut authority, mut primitive) = key_fixture();
            match change {
                0 => authority.evidence.purpose = ObservationPurpose::TargetInput,
                1 => authority.evidence.purpose = ObservationPurpose::WindowActivation,
                2 => authority.evidence.purpose = ObservationPurpose::MenuSelection,
                3 => intent.attempt_id = None,
                4 => {
                    primitive = Primitive::TypeText { text: " ".into() };
                    intent.steps[0].primitive = primitive.clone();
                }
                5 => {
                    primitive = Primitive::KeyPress {
                        key: "Return".into(),
                        modifiers: Default::default(),
                    };
                    intent.steps[0].primitive = primitive.clone();
                    intent
                        .binding_proof
                        .as_mut()
                        .unwrap()
                        .focus_scope
                        .as_mut()
                        .unwrap()
                        .key = LayoutIndependentSpecialKey::Return;
                    authority.evidence.binding_proof = intent.binding_proof.clone();
                }
                6 => {
                    if let Primitive::KeyPress { modifiers, .. } = &mut primitive {
                        modifiers.command = true;
                    }
                    intent.steps[0].primitive = primitive.clone();
                }
                7 => {
                    authority
                        .evidence
                        .binding_proof
                        .as_mut()
                        .unwrap()
                        .focus_scope
                        .as_mut()
                        .unwrap()
                        .context
                        .process
                        .start_seconds += 1
                }
                8 => {
                    authority
                        .evidence
                        .binding_proof
                        .as_mut()
                        .unwrap()
                        .focus_scope
                        .as_mut()
                        .unwrap()
                        .context
                        .keyboard
                        .caps_lock = Some(true)
                }
                9 => {
                    authority
                        .evidence
                        .binding_proof
                        .as_mut()
                        .unwrap()
                        .focus_scope
                        .as_mut()
                        .unwrap()
                        .context
                        .source_session_id = "another-watch".into()
                }
                10 => {
                    authority
                        .evidence
                        .binding_proof
                        .as_mut()
                        .unwrap()
                        .focus_scope
                        .as_mut()
                        .unwrap()
                        .context
                        .verified_source_sequence += 1
                }
                11 => intent.steps[0].offset_ms = 1,
                12 => {
                    intent.binding_proof.as_mut().unwrap().focus_scope = None;
                    authority.evidence.binding_proof = intent.binding_proof.clone();
                }
                _ => {
                    authority.evidence.dependencies.remove("geometry");
                    intent.dependencies.remove("geometry");
                }
            }
            assert!(
                NativeProbeRequest::build(
                    "key".into(),
                    &SubmitContext {
                        intent: &intent,
                        authority
                    },
                    &primitive,
                    0,
                    100,
                    7
                )
                .is_err(),
                "mutation {change} cannot authorize native key input"
            );
        }
    }

    fn menu_fixture() -> (ActionIntent, SubmissionAuthority, Primitive) {
        use interaction_contracts::desktop::binding::MenuOpeningRef;
        use interaction_contracts::desktop::source::{
            KernelProcessIdentity, MenuAnchorContinuity, MenuContext, MenuDrawingWindow,
            MenuOpenerAnchor, Rect,
        };
        let (mut intent, mut authority, primitive) = fixture();
        let proof = intent.binding_proof.as_mut().unwrap();
        let mut opener = proof.fingerprint.clone();
        opener.role = "AXMenuButton".into();
        opener.group = None;
        opener.label = "opener".into();
        opener.subrole = None;
        opener.scroll_container = None;
        let anchor = MenuOpenerAnchor {
            anchor_id: "anchor".into(),
            source_session_id: "source-watch".into(),
            anchor_revision: 1,
            process: KernelProcessIdentity {
                pid: opener.app_pid,
                start_seconds: 1,
                start_micros: 0,
            },
            origin_window_id: opener.window_id.clone(),
            origin_window_frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            opener_instance: ObjectRef::new("opener").unwrap(),
            opener_fingerprint: opener.clone(),
            opener_frame: proof.display_frame,
            verified_source_sequence: authority.evidence.source_sequence,
            verified_observed_ms: authority.evidence.observed_ms,
            continuity: MenuAnchorContinuity::DirectRead,
        };
        let mut menu = opener;
        menu.role = "AXMenu".into();
        menu.label = "".into();
        menu.ancestor_roles.push("AXMenuButton".into());
        let context = MenuContext {
            context_id: "menu-context".into(),
            source_session_id: "source-watch".into(),
            opener: anchor,
            menu_instance: ObjectRef::new("menu").unwrap(),
            menu_fingerprint: menu.clone(),
            menu_frame: proof.display_frame,
            drawing_window: MenuDrawingWindow {
                window_id: 9999,
                owner_pid: menu.app_pid,
                layer: 101,
                frame: proof.display_frame,
            },
        };
        proof.fingerprint = menu;
        proof.fingerprint.role = "AXMenuItem".into();
        proof.fingerprint.label = "choice".into();
        proof.fingerprint.ancestor_roles.push("AXMenu".into());
        proof.menu_scope = Some(MenuBindingScope::Item {
            context,
            opened_by: MenuOpeningRef {
                attempt_id: interaction_contracts::desktop::ids::AttemptId::new("menu-attempt")
                    .unwrap(),
                intent_id: "opening-input".into(),
                attempt_deadline_ms: 5000,
            },
        });
        sync_device_context(proof);
        intent.attempt_id = Some("menu-attempt".into());
        intent.dependencies.insert("menu".into(), 1);
        intent.dependencies.insert("geometry".into(), 1);
        authority.evidence.dependencies = intent.dependencies.clone();
        intent.binding_fingerprint = Some(proof.fingerprint.digest());
        authority.evidence.binding_proof = intent.binding_proof.clone();
        authority.evidence.purpose = ObservationPurpose::MenuSelection;
        (intent, authority, primitive)
    }

    #[test]
    fn menu_selection_wire_preserves_attempt_and_fixed_root_proof() {
        let (intent, authority, primitive) = menu_fixture();
        let request = NativeProbeRequest::build(
            "menu".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        let wire = serde_json::to_value(request).unwrap();
        assert_eq!(wire["action"], "ax_menu_press");
        assert_eq!(wire["attempt_id"], "menu-attempt");
        assert_eq!(wire["original_proof"]["menu_scope"]["kind"], "item");
        assert_eq!(
            wire["original_proof"]["fingerprint"]["window_id"],
            wire["original_proof"]["menu_scope"]["context"]["opener"]["origin_window_id"]
        );
        assert!(
            wire.get("mouse").is_none()
                && wire.get("scroll").is_none()
                && wire.get("value").is_none()
        );
    }

    #[test]
    fn menu_selection_rejects_purpose_primitive_attempt_session_and_context_mixing() {
        for change in 0..11 {
            let (mut intent, mut authority, mut primitive) = menu_fixture();
            match change {
                0 => authority.evidence.purpose = ObservationPurpose::TargetInput,
                1 => authority.evidence.purpose = ObservationPurpose::WindowActivation,
                2 => intent.attempt_id = Some("another-attempt".into()),
                3 => {
                    primitive = Primitive::Scroll { dx: 0, dy: -40 };
                    intent.steps[0].primitive = primitive.clone();
                }
                4 => {
                    intent.binding_proof.as_mut().unwrap().menu_scope = None;
                    authority.evidence.binding_proof = intent.binding_proof.clone();
                }
                5 => authority.evidence.session_epoch += 1,
                change => {
                    let Some(MenuBindingScope::Item { context, .. }) = authority
                        .evidence
                        .binding_proof
                        .as_mut()
                        .unwrap()
                        .menu_scope
                        .as_mut()
                    else {
                        unreachable!()
                    };
                    match change {
                        6 => context.context_id = "replacement-menu".into(),
                        7 => context.opener.anchor_id = "replacement-anchor".into(),
                        8 => {
                            context.source_session_id = "replacement-source".into();
                            context.opener.source_session_id = "replacement-source".into();
                        }
                        9 => {
                            context.menu_frame.width += 1.0;
                            context.menu_fingerprint.frame[2] += 1;
                            context.drawing_window.frame = context.menu_frame;
                        }
                        _ => context.drawing_window.window_id += 1,
                    }
                }
            }
            assert!(
                NativeProbeRequest::build(
                    "menu".into(),
                    &SubmitContext {
                        intent: &intent,
                        authority
                    },
                    &primitive,
                    0,
                    100,
                    7
                )
                .is_err(),
                "accepted mutation {change}"
            );
        }
    }

    #[test]
    fn menu_selection_accepts_only_monotonic_current_source_provenance() {
        let (intent, mut authority, primitive) = menu_fixture();
        authority.evidence.source_sequence += 1;
        authority.evidence.observed_ms += 1;
        authority.evidence.observation_id = "fresh-menu".into();
        assert_eq!(
            intent.binding_proof.as_ref().unwrap().continuity.kind,
            ObjectIdentityContinuity::NotificationCovered
        );
        let fresh_sequence = authority.evidence.source_sequence;
        let current_proof = authority.evidence.binding_proof.as_mut().unwrap();
        current_proof.environment_revision += 1;
        current_proof.snapshot_id =
            interaction_contracts::desktop::ids::RequestId::new("snap-menu-forward").unwrap();
        current_proof.continuity.source_sequence = fresh_sequence;
        let Some(MenuBindingScope::Item { context, .. }) = authority
            .evidence
            .binding_proof
            .as_mut()
            .unwrap()
            .menu_scope
            .as_mut()
        else {
            unreachable!()
        };
        context.opener.verified_source_sequence = authority.evidence.source_sequence;
        context.opener.verified_observed_ms = authority.evidence.observed_ms;
        assert!(
            NativeProbeRequest::build(
                "menu".into(),
                &SubmitContext {
                    intent: &intent,
                    authority: authority.clone()
                },
                &primitive,
                0,
                100,
                7
            )
            .is_ok()
        );
        let Some(MenuBindingScope::Item { context, .. }) = authority
            .evidence
            .binding_proof
            .as_mut()
            .unwrap()
            .menu_scope
            .as_mut()
        else {
            unreachable!()
        };
        context.opener.verified_source_sequence += 1;
        assert!(
            NativeProbeRequest::build(
                "menu".into(),
                &SubmitContext {
                    intent: &intent,
                    authority
                },
                &primitive,
                0,
                100,
                7
            )
            .is_err()
        );
    }

    #[test]
    fn scoped_opener_authorizes_only_show_menu_with_its_owning_attempt() {
        for action in [AxActionKind::ShowMenu, AxActionKind::Press] {
            let (mut intent, mut authority, _) = menu_fixture();
            let proof = intent.binding_proof.as_mut().unwrap();
            let Some(MenuBindingScope::Item { context, opened_by }) = proof.menu_scope.take()
            else {
                unreachable!()
            };
            let anchor = context.opener;
            proof.instance = anchor.opener_instance.clone();
            proof.fingerprint = anchor.opener_fingerprint.clone();
            proof.display_frame = anchor.opener_frame;
            proof.menu_scope = Some(MenuBindingScope::Opener {
                anchor,
                attempt_deadline_ms: opened_by.attempt_deadline_ms,
            });
            sync_device_context(proof);
            intent.binding_fingerprint = Some(proof.fingerprint.digest());
            authority.evidence.binding_proof = intent.binding_proof.clone();
            authority.evidence.purpose = ObservationPurpose::TargetInput;
            let primitive = Primitive::AxAction {
                action: action.clone(),
            };
            intent.steps[0].primitive = primitive.clone();
            let result = NativeProbeRequest::build(
                "opener".into(),
                &SubmitContext {
                    intent: &intent,
                    authority,
                },
                &primitive,
                0,
                100,
                7,
            );
            if action == AxActionKind::ShowMenu {
                assert_eq!(result.unwrap().action, "ax_show_menu");
            } else {
                assert!(result.is_err());
            }
        }
    }
    #[test]
    fn clicks_preserve_every_button_and_modifier_combination_with_fixed_point_proof() {
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            for mask in 0..16 {
                let (mut intent, authority, _) = fixture();
                let (x, y) = intent
                    .binding_proof
                    .as_ref()
                    .unwrap()
                    .display_frame
                    .center();
                let modifiers = Modifiers {
                    shift: mask & 1 != 0,
                    control: mask & 2 != 0,
                    option: mask & 4 != 0,
                    command: mask & 8 != 0,
                };
                let primitive = Primitive::Click {
                    button,
                    x: x as i32,
                    y: y as i32,
                    modifiers,
                };
                intent.steps[0].primitive = primitive.clone();
                let request = NativeProbeRequest::build(
                    "click".into(),
                    &SubmitContext {
                        intent: &intent,
                        authority: authority.clone(),
                    },
                    &primitive,
                    0,
                    100,
                    7,
                )
                .unwrap();
                assert_eq!(request.action, "click");
                let mouse = request.mouse.unwrap();
                assert_eq!(mouse.button, button);
                assert_eq!(mouse.modifiers, modifiers);
                assert_eq!(
                    (request.x, request.y),
                    (f64::from(x as i32), f64::from(y as i32))
                );
            }
        }
        let (mut intent, authority, _) = fixture();
        let primitive = Primitive::Click {
            button: MouseButton::Left,
            x: 100_000,
            y: 100_000,
            modifiers: Modifiers::default(),
        };
        intent.steps[0].primitive = primitive.clone();
        assert!(
            NativeProbeRequest::build(
                "outside".into(),
                &SubmitContext {
                    intent: &intent,
                    authority
                },
                &primitive,
                0,
                100,
                7
            )
            .is_err()
        );
    }
    #[test]
    fn scroll_wire_is_bounded_and_requires_the_scroll_area_proof() {
        for (role, dx, dy, accepted) in [
            ("AXScrollArea", -120, 120, true),
            ("AXScrollArea", 0, -1, true),
            ("AXScrollArea", 0, 0, false),
            ("AXScrollArea", 121, 1, false),
            ("AXScrollArea", 1, -121, false),
            ("AXButton", 0, 10, false),
        ] {
            let (mut intent, mut authority, _) = fixture();
            let proof = intent.binding_proof.as_mut().unwrap();
            proof.fingerprint.role = role.into();
            sync_device_context(proof);
            intent.binding_fingerprint =
                Some(intent.binding_proof.as_ref().unwrap().fingerprint.digest());
            authority.evidence.binding_proof = intent.binding_proof.clone();
            let primitive = Primitive::Scroll { dx, dy };
            intent.steps[0].primitive = primitive.clone();
            let result = NativeProbeRequest::build(
                "scroll".into(),
                &SubmitContext {
                    intent: &intent,
                    authority,
                },
                &primitive,
                0,
                100,
                7,
            );
            assert_eq!(result.is_ok(), accepted, "{role}: {dx}/{dy}");
            if let Ok(request) = result {
                assert_eq!(request.action, "scroll");
                assert_eq!(
                    (request.x, request.y),
                    request.original_proof.display_frame.center()
                );
                let wire = serde_json::to_value(request).unwrap();
                assert_eq!(wire["scroll"], serde_json::json!({"dx": dx, "dy": dy}));
                assert!(wire.get("mouse").is_none() && wire.get("value").is_none());
            }
        }
    }

    #[test]
    fn scroll_point_uses_the_same_integer_center_as_atomic_position() {
        let (mut intent, mut authority, _) = fixture();
        let proof = intent.binding_proof.as_mut().unwrap();
        proof.fingerprint.role = "AXScrollArea".into();
        proof.display_frame.x += 0.25;
        proof.display_frame.width += 0.5;
        proof.fingerprint.frame = [
            proof.display_frame.x,
            proof.display_frame.y,
            proof.display_frame.width,
            proof.display_frame.height,
        ]
        .map(|v| v.round() as i64);
        sync_device_context(proof);
        intent.binding_fingerprint = Some(proof.fingerprint.digest());
        authority.evidence.binding_proof = intent.binding_proof.clone();
        let (x, y) = intent
            .binding_proof
            .as_ref()
            .unwrap()
            .display_frame
            .center();
        let primitive = Primitive::Scroll { dx: 0, dy: -120 };
        intent.steps[0].primitive = primitive.clone();
        let request = NativeProbeRequest::build(
            "rounded".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        assert_eq!((request.x, request.y), (x.round(), y.round()));
    }

    #[test]
    fn pointer_move_wire_has_no_button_payload_and_rejects_outside_or_editor_points() {
        for (role, outside, valid) in [
            ("AXScrollArea", false, true),
            ("AXButton", false, true),
            ("AXTextField", false, false),
            ("AXScrollArea", true, false),
        ] {
            let (mut intent, mut authority, _) = fixture();
            let proof = intent.binding_proof.as_mut().unwrap();
            proof.fingerprint.role = role.into();
            sync_device_context(proof);
            let (x, y) = proof.display_frame.center();
            intent.binding_fingerprint = Some(proof.fingerprint.digest());
            authority.evidence.binding_proof = intent.binding_proof.clone();
            let primitive = Primitive::PointerPosition {
                x: if outside { 100000 } else { x.round() as i32 },
                y: y.round() as i32,
            };
            intent.steps[0].primitive = primitive.clone();
            let result = NativeProbeRequest::build(
                "move".into(),
                &SubmitContext {
                    intent: &intent,
                    authority,
                },
                &primitive,
                0,
                100,
                7,
            );
            assert_eq!(result.is_ok(), valid);
            if let Ok(request) = result {
                let wire = serde_json::to_value(request).unwrap();
                assert_eq!(wire["action"], "pointer_move");
                assert!(
                    wire.get("mouse").is_none()
                        && wire.get("scroll").is_none()
                        && wire.get("value").is_none()
                );
            }
        }
    }

    #[test]
    fn show_menu_requires_a_menu_control_and_carries_no_pointer_payload() {
        for role in ["AXMenuButton", "AXPopUpButton", "AXButton", "AXMenuItem"] {
            let (mut intent, mut authority, _) = fixture();
            let proof = intent.binding_proof.as_mut().unwrap();
            proof.fingerprint.role = role.into();
            sync_device_context(proof);
            intent.binding_fingerprint =
                Some(intent.binding_proof.as_ref().unwrap().fingerprint.digest());
            authority.evidence.binding_proof = intent.binding_proof.clone();
            let primitive = Primitive::AxAction {
                action: AxActionKind::ShowMenu,
            };
            intent.steps[0].primitive = primitive.clone();
            let result = NativeProbeRequest::build(
                "menu".into(),
                &SubmitContext {
                    intent: &intent,
                    authority,
                },
                &primitive,
                0,
                100,
                7,
            );
            assert_eq!(
                result.is_ok(),
                ["AXMenuButton", "AXPopUpButton"].contains(&role)
            );
            if let Ok(request) = result {
                assert_eq!(request.action, "ax_show_menu");
                let wire = serde_json::to_value(request).unwrap();
                assert!(
                    wire.get("scroll").is_none()
                        && wire.get("mouse").is_none()
                        && wire.get("value").is_none()
                );
            }
        }
    }

    fn fixture() -> (ActionIntent, SubmissionAuthority, Primitive) {
        let messages = samples::all();
        let mut intent = messages
            .iter()
            .find_map(|(_, e)| match &e.message {
                Message::SubmitIntent(r) => match &r.intent {
                    ExecutionIntent::Native { intent } => Some(intent.as_ref().clone()),
                    ExecutionIntent::Browser { .. } => None,
                },
                _ => None,
            })
            .unwrap();
        let mut evidence = messages
            .iter()
            .find_map(|(_, e)| match &e.message {
                Message::Observation(ExecutionObservation::Native { observation }) => {
                    Some(observation.as_ref().clone())
                }
                _ => None,
            })
            .unwrap();
        let primitive = Primitive::AxAction {
            action: AxActionKind::Press,
        };
        intent.steps[0].primitive = primitive.clone();
        intent.session_epoch = 7;
        intent.surface.epoch = 7;
        let proof = intent.binding_proof.as_mut().unwrap();
        proof.fingerprint.role = "AXButton".into();
        proof.fingerprint.group = None;
        proof.fingerprint.ancestor_roles = vec!["AXWindow".into()];
        sync_device_context(proof);
        intent.binding_fingerprint = Some(proof.fingerprint.digest());
        evidence.binding_proof = intent.binding_proof.clone();
        evidence.session_epoch = 7;
        evidence.surface = intent.surface.clone();
        evidence.clock_domain = "macos/7/wall".into();
        let authority = SubmissionAuthority {
            permit: intent.permit.clone(),
            stop_seq: 5,
            valid_until_ms: 200,
            command_end_ms: 200,
            evidence,
        };
        (intent, authority, primitive)
    }

    fn window_root_fixture() -> (ActionIntent, SubmissionAuthority, Primitive) {
        use interaction_contracts::desktop::binding::{
            derive_window_root_binding_evidence, instance_fingerprint,
        };
        use interaction_contracts::desktop::source::StructureSnapshot;
        // Unit-only projection: this synthetic fixture adds GlobalFocus facts
        // for WindowRoot tests; the preserved raw Source capture has none.
        let snapshot: StructureSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/desktop-v1/native-binding/scoped-window-synthetic.json"
        )))
        .unwrap();
        snapshot.validate().unwrap();
        let scope = &snapshot
            .table_scope_search
            .as_ref()
            .unwrap()
            .requested_scope;
        let window_id = &snapshot.window_context.as_ref().unwrap().origin_window_id;
        let root = snapshot
            .objects
            .iter()
            .find(|object| object.parent.is_none() && &object.window_id == window_id)
            .unwrap();
        let derived = derive_window_root_binding_evidence(&snapshot, root, scope).unwrap();
        let (mut intent, mut authority, _) = fixture();
        let proof = intent.binding_proof.as_mut().unwrap();
        proof.instance = root.instance_ref.clone();
        proof.snapshot_id = snapshot.snapshot_id.clone();
        proof.fingerprint = instance_fingerprint(&snapshot, root);
        proof.display_frame = root.frame;
        proof.continuity = derived.continuity;
        proof.device_context = derived.device_context;
        proof.menu_scope = None;
        proof.focus_scope = None;
        proof.validate().unwrap();
        intent.binding_fingerprint = Some(proof.fingerprint.digest());
        intent.session_epoch = snapshot.session_epoch;
        intent.surface = snapshot.surface.clone();
        intent.dependencies = BTreeMap::from([("authorization".into(), 1), ("window".into(), 1)]);
        intent.latest_start_ms = snapshot.observed_ms + 100;
        let primitive = Primitive::ActivateWindow {
            app: snapshot.app.bundle_id.clone(),
        };
        intent.steps[0].primitive = primitive.clone();
        let evidence = &mut authority.evidence;
        evidence.binding_proof = intent.binding_proof.clone();
        evidence.target_ref = intent.target_ref.clone();
        evidence.session_epoch = snapshot.session_epoch;
        evidence.surface = snapshot.surface.clone();
        evidence.clock_domain = snapshot.clock_domain;
        evidence.source_sequence = snapshot.source_sequence;
        evidence.observed_ms = snapshot.observed_ms;
        evidence.valid_until_ms = snapshot.observed_ms + 500;
        evidence.freshness = SourceFreshness::Verified;
        evidence.purpose = ObservationPurpose::WindowActivation;
        evidence.dependencies = intent.dependencies.clone();
        authority.valid_until_ms = snapshot.observed_ms + 100;
        authority.command_end_ms = snapshot.observed_ms + intent.duration_ms;
        intent.validate().unwrap();
        authority.evidence.validate().unwrap();
        (intent, authority, primitive)
    }

    #[test]
    fn actual_partial_source_replay_builds_only_a_window_activation_probe() {
        let (intent, authority, primitive) = window_root_fixture();
        let request = NativeProbeRequest::build(
            "window-root-replay".into(),
            &SubmitContext {
                intent: &intent,
                authority: authority.clone(),
            },
            &primitive,
            0,
            authority.evidence.observed_ms,
            intent.session_epoch,
        )
        .unwrap();
        assert_eq!(request.action, "activate_window");
        assert!(!request.current_proof.supports_native_input());
        assert!(!request.current_proof.supports_row_selection());
        assert!(request.current_proof.supports_window_activation());
        let wire = serde_json::to_value(request).unwrap();
        assert_eq!(wire["context_witness"]["scope"], "window_root_v1");
        assert!(wire["context_witness"].get("siblings").is_none());
        assert!(wire["context_witness"].get("instance_ref").is_none());
        for purpose in [
            ObservationPurpose::TargetInput,
            ObservationPurpose::RowSelection,
            ObservationPurpose::FocusedControl,
            ObservationPurpose::MenuSelection,
        ] {
            let mut changed = authority.clone();
            changed.evidence.purpose = purpose;
            assert!(
                NativeProbeRequest::build(
                    "wrong-purpose".into(),
                    &SubmitContext {
                        intent: &intent,
                        authority: changed.clone()
                    },
                    &primitive,
                    0,
                    authority.evidence.observed_ms,
                    intent.session_epoch
                )
                .is_err()
            );
        }
    }

    #[test]
    fn window_root_probe_rejects_changed_scope_process_provenance_and_root() {
        let (intent, authority, primitive) = window_root_fixture();
        for mutation in 0..7 {
            let mut changed = authority.clone();
            let proof = changed.evidence.binding_proof.as_mut().unwrap();
            match mutation {
                0 => {
                    if let BindingProofScope::WindowRoot { table_scope } =
                        &mut proof.continuity.scope
                    {
                        table_scope.column_headers[0] = "another header".into();
                    }
                }
                1 => {
                    if let DeviceContextWitness::WindowRootV1 { source, .. } =
                        &mut proof.device_context
                    {
                        source.process.start_seconds += 1;
                    }
                }
                2 => {
                    if let DeviceContextWitness::WindowRootV1 { source, .. } =
                        &mut proof.device_context
                    {
                        source.session_epoch += 1;
                    }
                }
                3 => {
                    if let DeviceContextWitness::WindowRootV1 { source, .. } =
                        &mut proof.device_context
                    {
                        source.verified_source_sequence += 1;
                    }
                }
                4 => {
                    if let DeviceContextWitness::WindowRootV1 { root, .. } =
                        &mut proof.device_context
                    {
                        root.frame.x += 1.0;
                    }
                }
                5 => changed.evidence.valid_until_ms = changed.evidence.observed_ms + 2001,
                6 => {
                    proof.continuity.kind = match proof.continuity.kind {
                        ObjectIdentityContinuity::NotificationCovered => {
                            ObjectIdentityContinuity::ReadScoped
                        }
                        ObjectIdentityContinuity::ReadScoped => {
                            ObjectIdentityContinuity::NotificationCovered
                        }
                    }
                }
                _ => unreachable!(),
            }
            assert!(
                NativeProbeRequest::build(
                    format!("changed-root-{mutation}"),
                    &SubmitContext {
                        intent: &intent,
                        authority: changed.clone()
                    },
                    &primitive,
                    0,
                    authority.evidence.observed_ms,
                    intent.session_epoch
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn covered_window_activation_keeps_identity_and_requires_real_current_read_provenance() {
        let (intent, mut authority, primitive) = window_root_fixture();
        let proof = authority.evidence.binding_proof.as_mut().unwrap();
        assert_eq!(
            proof.continuity.kind,
            ObjectIdentityContinuity::NotificationCovered
        );
        proof.environment_revision += 1;
        proof.snapshot_id =
            interaction_contracts::desktop::ids::RequestId::new("new-window-read").unwrap();
        proof.continuity.source_sequence += 1;
        let DeviceContextWitness::WindowRootV1 { source, .. } = &mut proof.device_context else {
            unreachable!()
        };
        source.verified_source_sequence += 1;
        source.verified_observed_ms += 1;
        authority.evidence.source_sequence += 1;
        authority.evidence.observed_ms += 1;
        let result = NativeProbeRequest::build(
            "current-window-read".into(),
            &SubmitContext {
                intent: &intent,
                authority: authority.clone(),
            },
            &primitive,
            0,
            authority.evidence.observed_ms,
            intent.session_epoch,
        );
        assert!(result.is_ok(), "{result:?}");
        authority.evidence.observed_ms += 1;
        assert!(
            NativeProbeRequest::build(
                "mismatched-window-read".into(),
                &SubmitContext {
                    intent: &intent,
                    authority: authority.clone()
                },
                &primitive,
                0,
                authority.evidence.observed_ms,
                intent.session_epoch
            )
            .is_err()
        );
    }

    fn row_selection_fixture() -> (ActionIntent, SubmissionAuthority, Primitive) {
        use interaction_contracts::desktop::binding::{
            BindingProofScope, DeviceContextApp, DeviceContextObject, DeviceContextPoint,
            DeviceContextRowSelectorColumn, DeviceContextTableColumn, DeviceContextTarget,
            DeviceContextVisibleRow, DeviceContextWindow, DeviceContextWitness,
        };
        use interaction_contracts::desktop::device::ObservationPurpose;
        use interaction_contracts::desktop::primitive::AxActionKind;
        use interaction_contracts::desktop::source::{LayerKind, Rect, UiValue};

        let (mut intent, mut authority, _) = fixture();
        let source_sequence = authority.evidence.source_sequence;
        let proof = {
            let proof = intent.binding_proof.as_mut().unwrap();
            let bundle_id = proof.fingerprint.app.clone();
            let pid = proof.fingerprint.app_pid;
            let window_id = proof.fingerprint.window_id.clone();
            let window_frame = Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            };
            let viewport_frame = Rect {
                x: 10.0,
                y: 10.0,
                width: 500.0,
                height: 180.0,
            };
            let table_frame = viewport_frame;
            let target_frame = Rect {
                x: 20.0,
                y: 30.0,
                width: 480.0,
                height: 30.0,
            };
            let other_frame = Rect {
                x: 20.0,
                y: 60.0,
                width: 480.0,
                height: 30.0,
            };

            proof.fingerprint.ancestor_roles =
                vec!["AXWindow".into(), "AXScrollArea".into(), "AXTable".into()];
            proof.fingerprint.group = None;
            proof.fingerprint.role = "AXRow".into();
            proof.fingerprint.subrole = None;
            proof.fingerprint.label.clear();
            proof.fingerprint.sibling_index = 0;
            proof.fingerprint.frame = [20, 30, 480, 30];
            proof.fingerprint.scroll_container = None;
            proof.instance = ObjectRef::new("row-42").unwrap();
            proof.snapshot_id =
                interaction_contracts::desktop::ids::RequestId::new("row-snapshot").unwrap();
            proof.display_frame = target_frame;
            proof.continuity.kind = ObjectIdentityContinuity::ReadScoped;
            proof.continuity.source_sequence = source_sequence;
            proof.continuity.context_digest = format!("bc1:{source_sequence:064x}");
            proof.continuity.scope = BindingProofScope::TableViewport {
                table_instance_ref: ObjectRef::new("table-1").unwrap(),
                scroll_container_instance_ref: ObjectRef::new("scroll-1").unwrap(),
                viewport_frame,
                visible_rows_complete: true,
                columns_complete: true,
            };
            proof.menu_scope = None;
            proof.focus_scope = None;
            proof.device_context = DeviceContextWitness::TableViewportV1 {
                table_ax_enabled:
                    interaction_contracts::desktop::source::AxEnabledObservation::Enabled,
                target_ax_enabled:
                    interaction_contracts::desktop::source::AxEnabledObservation::Enabled,
                app: Box::new(DeviceContextApp {
                    bundle_id: bundle_id.clone(),
                    pid,
                }),
                window: Box::new(DeviceContextWindow {
                    window_id,
                    title: Some("records".into()),
                    frame: window_frame,
                    kind: LayerKind::Window,
                    modal: false,
                    owner_bundle_id: bundle_id,
                }),
                table: DeviceContextObject {
                    role: "AXTable".into(),
                    subrole: None,
                    label: "Records".into(),
                    frame: table_frame,
                },
                target: Box::new(DeviceContextTarget {
                    object: DeviceContextObject {
                        role: "AXRow".into(),
                        subrole: None,
                        label: String::new(),
                        frame: target_frame,
                    },
                    group: None,
                    scroll: None,
                }),
                scroll_path: vec![DeviceContextObject {
                    role: "AXScrollArea".into(),
                    subrole: None,
                    label: "Records content".into(),
                    frame: window_frame,
                }],
                viewport_frame,
                ordered_columns: vec![DeviceContextTableColumn {
                    ax_index: 0,
                    header: "Owner".into(),
                    frame: Rect {
                        x: 10.0,
                        y: 10.0,
                        width: 250.0,
                        height: 180.0,
                    },
                }],
                selector_columns: vec![DeviceContextRowSelectorColumn {
                    ax_index: 0,
                    header: "Owner".into(),
                    expected_value: UiValue::Text("Ada".into()),
                }],
                visible_rows: vec![
                    DeviceContextVisibleRow {
                        ax_index: 42,
                        frame: target_frame,
                        selector_values: vec![UiValue::Text("Ada".into())],
                    },
                    DeviceContextVisibleRow {
                        ax_index: 43,
                        frame: other_frame,
                        selector_values: vec![UiValue::Text("Bea".into())],
                    },
                ],
                target_row_ax_index: 42,
                target_selected: false,
                target_ax_selected_settable: true,
                hit_point: DeviceContextPoint { x: 21.0, y: 45.0 },
            };
            proof.clone()
        };

        let primitive = Primitive::AxAction {
            action: AxActionKind::SetSelectedTrue,
        };
        intent.steps[0].primitive = primitive.clone();
        intent.binding_fingerprint = Some(proof.fingerprint.digest());
        intent.binding_proof = Some(proof);
        let dependencies: BTreeMap<_, _> =
            ["focus", "layer", "binding", "authorization", "geometry"]
                .into_iter()
                .map(|name| (name.to_string(), 1))
                .collect();
        intent.dependencies = dependencies.clone();
        authority.evidence.purpose = ObservationPurpose::RowSelection;
        authority.evidence.source_sequence = source_sequence;
        authority.evidence.dependencies = dependencies;
        authority.evidence.binding_proof = intent.binding_proof.clone();
        (intent, authority, primitive)
    }

    #[test]
    fn row_selection_emits_only_its_proof_bound_point_and_ax_set_selected_true() {
        let (intent, authority, primitive) = row_selection_fixture();
        let request = NativeProbeRequest::build(
            "row-select".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            3,
            100,
            7,
        )
        .unwrap();
        assert_eq!(request.action, "ax_set_selected_true");
        assert_eq!((request.x, request.y), (21.0, 45.0));
        assert_ne!(
            request.x,
            intent
                .binding_proof
                .as_ref()
                .unwrap()
                .display_frame
                .center()
                .0
        );
        assert!(request.value.is_none());
        assert!(request.mouse.is_none());
        assert!(request.scroll.is_none());
        assert!(request.key.is_none());
        assert!(matches!(
            &request.context_witness,
            DeviceContextWitness::TableViewportV1 {
                target_row_ax_index,
                ..
            } if *target_row_ax_index == 42
        ));
        assert_eq!(
            serde_json::to_value(&request).unwrap()["action"],
            "ax_set_selected_true"
        );
    }

    #[test]
    fn row_trace_requires_the_real_row_submit_and_single_recorded_ax_call() {
        use crate::guard::submission::{
            NativeRowTrace, NativeRowTraceClockDomain, NativeRowTracePrimitive,
            NativeSubmitResponse,
        };

        let (intent, authority, primitive) = row_selection_fixture();
        let request = NativeProbeRequest::build(
            "row-trace".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            3,
            100,
            7,
        )
        .unwrap();
        let trace = NativeRowTrace {
            primitive: NativeRowTracePrimitive::AxSetSelectedTrue,
            ax_call_count: 1,
            ax_error_code: Some(0),
            clock_domain: NativeRowTraceClockDomain::DispatchTimeUptime,
            call_started_uptime_ns: Some(1_000),
            call_finished_uptime_ns: Some(1_025),
        };
        let mut receipt = NativeSubmitResponse {
            protocol_version: 1,
            kind: "atomic_submit".into(),
            session_epoch: 7,
            request_id: request.request_id.clone(),
            outcome: "applied".into(),
            reason: "atomic input call returned; effect unverified".into(),
            submitted: true,
            committed_ms: Some(110),
            returned_ms: Some(210),
            binding_digest: Some(request.binding_digest.clone()),
            row_trace: Some(trace.clone()),
            ax_trace: None,
            background_ax_trace: None,
        };
        assert!(receipt.valid_for(&request, 7));

        // AX success can still be unknown to the Core clock; the native trace
        // remains independent of that wall-clock outcome classification.
        receipt.outcome = "unknown".into();
        assert!(receipt.valid_for(&request, 7));
        receipt.outcome = "applied".into();
        receipt.row_trace.as_mut().unwrap().ax_error_code = Some(-25200);
        assert!(!receipt.valid_for(&request, 7));

        receipt.outcome = "unknown".into();
        assert!(receipt.valid_for(&request, 7));
        receipt.row_trace = None;
        assert!(!receipt.valid_for(&request, 7));

        receipt.outcome = "rejected".into();
        receipt.submitted = false;
        receipt.committed_ms = None;
        receipt.returned_ms = None;
        receipt.binding_digest = None;
        assert!(receipt.valid_for(&request, 7));
        receipt.row_trace = Some(NativeRowTrace {
            ax_call_count: 0,
            ax_error_code: None,
            call_started_uptime_ns: None,
            call_finished_uptime_ns: None,
            ..trace
        });
        assert!(receipt.valid_for(&request, 7));

        let (intent, authority, primitive) = fixture();
        let non_row_request = NativeProbeRequest::build(
            "non-row-trace".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            3,
            100,
            7,
        )
        .unwrap();
        receipt.request_id = non_row_request.request_id.clone();
        receipt.submitted = true;
        receipt.outcome = "applied".into();
        receipt.committed_ms = Some(110);
        receipt.returned_ms = Some(210);
        receipt.binding_digest = Some(non_row_request.binding_digest.clone());
        assert!(!receipt.valid_for(&non_row_request, 7));
        receipt.row_trace = None;
        assert!(receipt.valid_for(&non_row_request, 7));
    }

    #[test]
    fn ax_press_trace_is_optional_but_strict_when_reported() {
        use crate::guard::submission::{
            NativeAxTrace, NativeAxTraceClockDomain, NativeAxTracePrimitive,
            NativeBackgroundAxTrace, NativeBackgroundAxTraceStage, NativeSubmitResponse,
        };

        let (intent, authority, primitive) = fixture();
        let request = NativeProbeRequest::build(
            "ax-press-trace".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            3,
            100,
            7,
        )
        .unwrap();
        assert_eq!(request.action, "ax_press");
        let called = NativeAxTrace {
            primitive: NativeAxTracePrimitive::AxPress,
            ax_call_count: 1,
            ax_error_code: Some(0),
            clock_domain: NativeAxTraceClockDomain::DispatchTimeUptime,
            call_started_uptime_ns: Some(1_000),
            call_finished_uptime_ns: Some(1_025),
        };
        let mut receipt = NativeSubmitResponse {
            protocol_version: 1,
            kind: "atomic_submit".into(),
            session_epoch: 7,
            request_id: request.request_id.clone(),
            outcome: "applied".into(),
            reason: "effect remains unverified".into(),
            submitted: true,
            committed_ms: Some(110),
            returned_ms: Some(210),
            binding_digest: Some(request.binding_digest.clone()),
            row_trace: None,
            ax_trace: Some(called.clone()),
            // A diagnostic marker cannot change the authority of a valid
            // foreground receipt.
            background_ax_trace: Some(NativeBackgroundAxTrace {
                stage: NativeBackgroundAxTraceStage::Preflight,
                process_identity_matches: false,
                application_boundary_matches: false,
                window_boundary_matches: false,
                target_traversal_complete: false,
                target_matches: false,
                initial_stability_passed: false,
                final_revalidation_passed: false,
                commit_ready: false,
                submitted: false,
            }),
        };
        assert!(receipt.valid_for(&request, 7));

        receipt.outcome = "unknown".into();
        receipt.ax_trace.as_mut().unwrap().ax_error_code = Some(-25200);
        assert!(receipt.valid_for(&request, 7));
        receipt.outcome = "applied".into();
        assert!(!receipt.valid_for(&request, 7));

        // Optional absence means no trace was observable. It is never
        // synthesized into a call result and does not change receipt authority.
        receipt.ax_trace = None;
        assert!(receipt.valid_for(&request, 7));

        receipt.submitted = false;
        receipt.outcome = "stop_latched".into();
        receipt.committed_ms = None;
        receipt.returned_ms = None;
        receipt.binding_digest = None;
        receipt.ax_trace = Some(NativeAxTrace {
            primitive: NativeAxTracePrimitive::AxPress,
            ax_call_count: 0,
            ax_error_code: None,
            clock_domain: NativeAxTraceClockDomain::DispatchTimeUptime,
            call_started_uptime_ns: None,
            call_finished_uptime_ns: None,
        });
        assert!(receipt.valid_for(&request, 7));
        receipt.ax_trace.as_mut().unwrap().ax_call_count = 1;
        receipt.ax_trace.as_mut().unwrap().ax_error_code = Some(-25200);
        receipt.ax_trace.as_mut().unwrap().call_started_uptime_ns = Some(1_000);
        receipt.ax_trace.as_mut().unwrap().call_finished_uptime_ns = Some(1_025);
        assert!(!receipt.valid_for(&request, 7));

        let (intent, authority, primitive) = row_selection_fixture();
        let row_request = NativeProbeRequest::build(
            "ax-trace-wrong-action".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        receipt.request_id = row_request.request_id.clone();
        receipt.ax_trace = Some(called);
        assert!(!receipt.valid_for(&row_request, 7));
    }

    #[test]
    fn row_selection_rejects_other_purposes_actions_and_mixed_current_witnesses() {
        use interaction_contracts::desktop::device::ObservationPurpose;

        for mutation in 0..4 {
            let (mut intent, mut authority, mut primitive) = row_selection_fixture();
            match mutation {
                0 => authority.evidence.purpose = ObservationPurpose::TargetInput,
                1 => {
                    primitive = Primitive::AxAction {
                        action: AxActionKind::Press,
                    };
                    intent.steps[0].primitive = primitive.clone();
                }
                2 => {
                    let proof = authority.evidence.binding_proof.as_mut().unwrap();
                    let DeviceContextWitness::TableViewportV1 { hit_point, .. } =
                        &mut proof.device_context
                    else {
                        unreachable!();
                    };
                    hit_point.x += 1.0;
                }
                _ => {
                    let proof = intent.binding_proof.as_mut().unwrap();
                    let DeviceContextWitness::TableViewportV1 {
                        target_selected, ..
                    } = &mut proof.device_context
                    else {
                        unreachable!();
                    };
                    *target_selected = true;
                    authority.evidence.binding_proof = intent.binding_proof.clone();
                }
            }
            assert!(
                NativeProbeRequest::build(
                    "bad-row-selection".into(),
                    &SubmitContext {
                        intent: &intent,
                        authority,
                    },
                    &primitive,
                    0,
                    100,
                    7,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn row_selection_rejects_a_later_notification_covered_source_read() {
        use interaction_contracts::desktop::ids::RequestId;

        let (mut intent, mut authority, primitive) = row_selection_fixture();
        let original = intent.binding_proof.as_mut().unwrap();
        original.continuity.kind = ObjectIdentityContinuity::NotificationCovered;
        original.continuity.context_digest = format!("bc1:{}", "d".repeat(64));
        authority.evidence.binding_proof = intent.binding_proof.clone();

        let next_source_sequence = authority.evidence.source_sequence + 1;
        authority.evidence.source_sequence = next_source_sequence;
        let current = authority.evidence.binding_proof.as_mut().unwrap();
        current.continuity.source_sequence = next_source_sequence;
        current.snapshot_id = RequestId::new("row-snapshot-next").unwrap();

        assert!(
            NativeProbeRequest::build(
                "later-covered-row-read".into(),
                &SubmitContext {
                    intent: &intent,
                    authority,
                },
                &primitive,
                0,
                100,
                7,
            )
            .is_err()
        );
    }

    #[test]
    fn native_requests_preserve_absolute_core_deadline_and_raw_proof() {
        let (intent, authority, primitive) = fixture();
        let expected_witness = authority
            .evidence
            .binding_proof
            .as_ref()
            .unwrap()
            .device_context
            .clone();
        let request = NativeProbeRequest::build(
            "p1".into(),
            &SubmitContext {
                intent: &intent,
                authority: authority.clone(),
            },
            &primitive,
            3,
            100,
            7,
        )
        .unwrap();
        assert_eq!(request.valid_until_ms, 200);
        assert_eq!(request.started_ms, 100);
        assert_eq!(request.guard_revision, 3);
        assert_eq!(request.stop_seq, 5);
        assert_eq!(request.original_proof, intent.binding_proof.unwrap());
        assert_eq!(
            request.current_proof,
            authority.evidence.binding_proof.unwrap()
        );
        assert_eq!((request.x, request.y), (30.0, 20.0));
        assert_eq!(request.context_witness, expected_witness);
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(
            wire["context_witness"],
            serde_json::to_value(expected_witness).unwrap()
        );
        assert!(wire["original_proof"].get("device_context").is_none());
        assert!(wire["current_proof"].get("device_context").is_none());
    }

    #[test]
    fn unsupported_device_context_and_table_viewport_never_reach_native() {
        for mutation in 0..3 {
            let (mut intent, mut authority, primitive) = fixture();
            if mutation == 0 {
                intent
                    .binding_proof
                    .as_mut()
                    .unwrap()
                    .device_context = DeviceContextWitness::Unsupported {
                    reason: interaction_contracts::desktop::binding::DeviceContextUnsupportedReason::TableViewport,
                };
            } else if mutation == 1 {
                authority
                    .evidence
                    .binding_proof
                    .as_mut()
                    .unwrap()
                    .device_context = DeviceContextWitness::Unsupported {
                    reason: interaction_contracts::desktop::binding::DeviceContextUnsupportedReason::TableViewport,
                };
            } else {
                let proof = intent.binding_proof.as_mut().unwrap();
                proof.continuity.scope = BindingProofScope::TableViewport {
                    table_instance_ref: ObjectRef::new("table").unwrap(),
                    scroll_container_instance_ref: ObjectRef::new("scroll").unwrap(),
                    viewport_frame: proof.display_frame,
                    visible_rows_complete: true,
                    columns_complete: true,
                };
                proof.device_context = DeviceContextWitness::Unsupported {
                    reason: interaction_contracts::desktop::binding::DeviceContextUnsupportedReason::TableViewport,
                };
                authority.evidence.binding_proof = intent.binding_proof.clone();
            }
            assert!(
                NativeProbeRequest::build(
                    "unsupported-context".into(),
                    &SubmitContext {
                        intent: &intent,
                        authority,
                    },
                    &primitive,
                    0,
                    100,
                    7,
                )
                .is_err(),
                "mutation {mutation} must not acquire a native input comparator"
            );
        }
    }

    #[test]
    fn native_request_and_context_wire_budgets_fail_closed() {
        let (intent, authority, primitive) = fixture();
        let mut request = NativeProbeRequest::build(
            "bounded".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        let DeviceContextWitness::WholeStructureV1 { target, .. } = &mut request.context_witness
        else {
            unreachable!();
        };
        target.object.label = "x".repeat(MAX_DEVICE_CONTEXT_WITNESS_BYTES + 1);
        assert!(request.validate_wire_budget().is_err());

        let (intent, authority, primitive) = fixture();
        let mut request = NativeProbeRequest::build(
            "bounded".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        request.task_id = "x".repeat(MAX_NATIVE_PROBE_REQUEST_BYTES);
        assert!(request.validate_wire_budget().is_err());
    }
    #[test]
    fn changed_source_identity_clock_or_permission_generation_never_reaches_native() {
        for kind in 0..3 {
            let (intent, mut authority, primitive) = fixture();
            match kind {
                0 => {
                    authority.evidence.binding_proof.as_mut().unwrap().instance =
                        ObjectRef::new("replacement").unwrap()
                }
                1 => authority.evidence.clock_domain = "macos/7/monotonic".into(),
                _ => authority.permit.generation += 1,
            }
            assert!(
                NativeProbeRequest::build(
                    "p1".into(),
                    &SubmitContext {
                        intent: &intent,
                        authority
                    },
                    &primitive,
                    0,
                    100,
                    7
                )
                .is_err()
            );
        }
    }

    #[test]
    fn read_scoped_binding_is_admitted_only_for_its_exact_source_read() {
        let (mut intent, mut authority, primitive) = fixture();
        let proof = intent.binding_proof.as_mut().unwrap();
        proof.continuity.kind = ObjectIdentityContinuity::ReadScoped;
        proof.continuity.source_sequence = 1;
        proof.continuity.context_digest = format!("bc1:{}", "b".repeat(64));
        authority.evidence.binding_proof = intent.binding_proof.clone();
        let request = NativeProbeRequest::build(
            "read-scoped".into(),
            &SubmitContext {
                intent: &intent,
                authority: authority.clone(),
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        assert_eq!(request.source_sequence, 1);

        let mut refreshed = authority.clone();
        refreshed.evidence.source_sequence = 2;
        let current = refreshed.evidence.binding_proof.as_mut().unwrap();
        current.continuity.source_sequence = 2;
        current.continuity.context_digest = format!("bc1:{}", "c".repeat(64));
        current.snapshot_id =
            interaction_contracts::desktop::ids::RequestId::new("snap-2").unwrap();
        assert!(
            NativeProbeRequest::build(
                "read-scoped-refresh".into(),
                &SubmitContext {
                    intent: &intent,
                    authority: refreshed
                },
                &primitive,
                0,
                100,
                7,
            )
            .is_err(),
            "a later Source read requires a new binding, decision, and permit"
        );

        let mut mismatched = authority;
        mismatched.evidence.source_sequence = 2;
        assert!(
            NativeProbeRequest::build(
                "read-scoped-sequence-mismatch".into(),
                &SubmitContext {
                    intent: &intent,
                    authority: mismatched
                },
                &primitive,
                0,
                100,
                7,
            )
            .is_err(),
            "the proof sequence must equal the current evidence sequence"
        );
    }

    #[test]
    fn ordinary_ax_press_cannot_use_a_table_viewport_proof() {
        use interaction_contracts::desktop::source::Rect;

        let (mut intent, mut authority, primitive) = fixture();
        let proof = intent.binding_proof.as_mut().unwrap();
        proof.continuity.scope = BindingProofScope::TableViewport {
            table_instance_ref: ObjectRef::new("table-1").unwrap(),
            scroll_container_instance_ref: ObjectRef::new("scroll-1").unwrap(),
            viewport_frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            visible_rows_complete: true,
            columns_complete: true,
        };
        authority.evidence.binding_proof = intent.binding_proof.clone();
        assert!(
            NativeProbeRequest::build(
                "viewport-scope".into(),
                &SubmitContext {
                    intent: &intent,
                    authority
                },
                &primitive,
                0,
                100,
                7,
            )
            .is_err(),
            "only the dedicated AXSelected=true row route accepts a table viewport proof"
        );
    }

    #[test]
    fn expired_or_fabricated_command_windows_are_rejected_before_writing() {
        let (intent, mut authority, primitive) = fixture();
        authority.valid_until_ms = 100;
        assert!(
            NativeProbeRequest::build(
                "p1".into(),
                &SubmitContext {
                    intent: &intent,
                    authority: authority.clone()
                },
                &primitive,
                0,
                100,
                7
            )
            .is_err()
        );
        authority.valid_until_ms = 200;
        authority.command_end_ms = 1000;
        assert!(
            NativeProbeRequest::build(
                "p1".into(),
                &SubmitContext {
                    intent: &intent,
                    authority
                },
                &primitive,
                0,
                100,
                7
            )
            .is_err()
        );
    }
    #[test]
    fn eligible_native_replies_cannot_become_input_tokens_or_cross_request_epochs() {
        let (intent, authority, primitive) = fixture();
        let request = NativeProbeRequest::build(
            "p1".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        let mut response = NativeProbeResponse {
            protocol_version: 1,
            kind: "authority_probe".into(),
            session_epoch: 7,
            request_id: "p1".into(),
            outcome: "eligible".into(),
            reason: "read only".into(),
            binding_digest: Some(request.binding_digest.clone()),
            input_authorized: false,
        };
        assert!(response.valid_for(&request, 7));
        assert!(!response.valid_for(&request, 8));
        response.input_authorized = true;
        assert!(!response.valid_for(&request, 7));
        response.input_authorized = false;
        response.outcome = "revoked".into();
        assert!(!response.valid_for(&request, 7));
        response.binding_digest = None;
        assert!(response.valid_for(&request, 7));
    }

    #[test]
    fn atomic_receipts_preserve_uncertainty_after_submission() {
        use crate::guard::submission::NativeSubmitResponse;
        let (intent, authority, primitive) = fixture();
        let request = NativeProbeRequest::build(
            "p1".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        let mut receipt = NativeSubmitResponse {
            protocol_version: 1,
            kind: "atomic_submit".into(),
            session_epoch: 7,
            request_id: "p1".into(),
            outcome: "applied".into(),
            reason: "effect unverified".into(),
            submitted: true,
            committed_ms: Some(110),
            returned_ms: Some(210),
            binding_digest: Some(request.binding_digest.clone()),
            row_trace: None,
            ax_trace: None,
            background_ax_trace: None,
        };
        // A receipt may arrive after the fixed authority expired.
        assert!(receipt.valid_for(&request, 7));
        receipt.outcome = "unknown".into();
        assert!(receipt.valid_for(&request, 7));
        receipt.outcome = "rejected".into();
        assert!(!receipt.valid_for(&request, 7));
        receipt.submitted = false;
        assert!(!receipt.valid_for(&request, 7));
        receipt.committed_ms = None;
        receipt.returned_ms = None;
        receipt.binding_digest = None;
        assert!(receipt.valid_for(&request, 7));
        assert!(!receipt.valid_for(&request, 8));
        receipt.request_id = "other".into();
        assert!(!receipt.valid_for(&request, 7));
    }

    #[test]
    fn atomic_receipts_cannot_claim_a_commit_outside_the_core_window() {
        use crate::guard::submission::NativeSubmitResponse;
        let (intent, authority, primitive) = fixture();
        let request = NativeProbeRequest::build(
            "p1".into(),
            &SubmitContext {
                intent: &intent,
                authority,
            },
            &primitive,
            0,
            100,
            7,
        )
        .unwrap();
        let mut receipt = NativeSubmitResponse {
            protocol_version: 1,
            kind: "atomic_submit".into(),
            session_epoch: 7,
            request_id: "p1".into(),
            outcome: "applied".into(),
            reason: "ok".into(),
            submitted: true,
            committed_ms: Some(200),
            returned_ms: Some(201),
            binding_digest: Some(request.binding_digest.clone()),
            row_trace: None,
            ax_trace: None,
            background_ax_trace: None,
        };
        assert!(!receipt.valid_for(&request, 7));
        receipt.committed_ms = Some(99);
        assert!(!receipt.valid_for(&request, 7));
        receipt.committed_ms = Some(110);
        receipt.returned_ms = Some(109);
        assert!(!receipt.valid_for(&request, 7));
        receipt.returned_ms = Some(111);
        receipt.binding_digest = Some("wrong".into());
        assert!(!receipt.valid_for(&request, 7));
    }
    #[test]
    fn set_value_enforces_registered_text_boolean_and_utf8_byte_policies() {
        for (role, app, subrole, value, allowed) in [
            (
                "AXTextField",
                "com.apple.TextEdit",
                None,
                "中文\nHello 👋".to_string(),
                true,
            ),
            (
                "AXTextField",
                "com.lingxi.computer-user.mvp-fixture",
                None,
                "Alert threshold".to_string(),
                true,
            ),
            (
                "AXTextArea",
                "com.lingxi.computer-user.mvp-fixture",
                None,
                "Notes".to_string(),
                true,
            ),
            (
                "AXTextArea",
                "com.apple.TextEdit",
                None,
                "a".repeat(2048),
                true,
            ),
            (
                "AXTextArea",
                "com.apple.TextEdit",
                None,
                "中".repeat(683),
                false,
            ),
            (
                "AXTextField",
                "unregistered.app",
                None,
                "text".into(),
                false,
            ),
            (
                "AXTextField",
                "com.apple.TextEdit",
                Some("AXSecureTextField"),
                "secret".into(),
                false,
            ),
            (
                "AXTextField",
                "com.apple.TextEdit",
                Some("AXUnknown"),
                "text".into(),
                false,
            ),
            (
                "AXTextField",
                "com.apple.TextEdit",
                None,
                "bad\0".into(),
                false,
            ),
            ("AXCheckBox", "test.app", None, "on".into(), true),
            ("AXCheckBox", "test.app", None, "true".into(), false),
        ] {
            let (mut intent, mut authority, _) = fixture();
            let primitive = Primitive::AxAction {
                action: AxActionKind::SetValue { value },
            };
            intent.steps[0].primitive = primitive.clone();
            let proof = intent.binding_proof.as_mut().unwrap();
            proof.fingerprint.role = role.into();
            proof.fingerprint.app = app.into();
            proof.fingerprint.subrole = subrole.map(str::to_string);
            sync_device_context(proof);
            intent.binding_fingerprint = Some(proof.fingerprint.digest());
            authority.evidence.binding_proof = intent.binding_proof.clone();
            let result = NativeProbeRequest::build(
                "value".into(),
                &SubmitContext {
                    intent: &intent,
                    authority,
                },
                &primitive,
                0,
                100,
                7,
            );
            assert_eq!(result.is_ok(), allowed, "{role}/{app}/{subrole:?}");
            if let Ok(request) = result {
                assert_eq!(request.action, "ax_set_value");
            }
        }
    }
}
