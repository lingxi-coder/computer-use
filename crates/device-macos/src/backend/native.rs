//! Nonblocking production backend: the same native worker validates the target
//! and submits on that AX handle. No simulated authority or input log is used.
use crate::guard::owned_value::owned_action_value_is_valid;
use crate::guard::submission::NativeSubmitResponse;
use crate::guard::{GuardClient, GuardSnapshot};
use execution_core::backend::{
    HitTestOutcome, HitTestRequest, SubmitCompletion, SubmitContext, SubmitOutcome,
};
use interaction_contracts::desktop::binding::{
    FocusedKeyProfile, LayoutIndependentSpecialKey, MenuBindingScope,
};
use interaction_contracts::desktop::device::{ObservationPurpose, PermitRef};
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::primitive::{AxActionKind, Modifiers, Primitive};

pub struct NativeAxBackend {
    target: (u32, String),
    window: Option<String>,
    guard: Option<GuardClient>,
    ready: bool,
    keyboard_ready: bool,
    guard_revision: Option<u64>,
    sequence: u64,
    pending: Option<(u64, u64, String)>,
    preflight_rejection: Option<NativeSubmitResponse>,
    owned_registration: Option<Box<OwnedDocumentRegistration>>,
    owned_save_enabled: bool,
}

impl NativeAxBackend {
    pub fn new(target: (u32, String), window: Option<String>) -> Self {
        Self {
            target,
            window,
            guard: None,
            ready: false,
            keyboard_ready: false,
            guard_revision: None,
            sequence: 0,
            pending: None,
            preflight_rejection: None,
            owned_registration: None,
            owned_save_enabled: false,
        }
    }
    pub fn new_owned_value(
        target: (u32, String),
        registration: Box<OwnedDocumentRegistration>,
        owned_save_enabled: bool,
    ) -> Self {
        let mut backend = Self::new(target, None);
        backend.owned_registration = Some(registration);
        backend.owned_save_enabled = owned_save_enabled;
        backend
    }
    pub fn attach(&mut self, guard: GuardClient) -> Result<(), String> {
        if self.guard.is_some()
            || guard.execution_target() != Some((self.target.0, self.target.1.as_str()))
            || guard.execution_window() != self.window.as_deref()
            || guard.is_owned_value_execution() != self.owned_registration.is_some()
            || guard.is_owned_save_execution() != self.owned_save_enabled
        {
            return Err("native guard scope mismatch or already attached".into());
        }
        self.guard = Some(guard);
        Ok(())
    }
    pub fn guard_mut(&mut self) -> Option<&mut GuardClient> {
        self.guard.as_mut()
    }
    pub fn ready(&self) -> bool {
        self.ready && self.guard.is_some()
    }
    pub fn snapshot(&mut self, snapshot: &GuardSnapshot) {
        self.guard_revision = Some(snapshot.revision());
        self.ready = snapshot.accessibility
            && snapshot.human_input
            && snapshot.startup_mouse_buttons_clear
            && snapshot.authority_clock_ready
            && snapshot.host_alive
            && snapshot.supervisor_alive;
        self.keyboard_ready = snapshot.keyboard_input_available && snapshot.startup_keyboard_clear;
    }
    pub fn disable(&mut self) {
        if let Some(reply) = self.guard.as_mut().and_then(|guard| {
            guard.reject_key_preflight("native guard lost before key atomic submission")
        }) {
            self.preflight_rejection = Some(reply);
        }
        self.ready = false;
        self.keyboard_ready = false;
        self.guard = None;
    }
    fn supported(&self, primitive: &Primitive, context: &SubmitContext<'_>) -> bool {
        if self.owned_registration.is_some() {
            return false;
        }
        let row_selection = context.authority.evidence.purpose == ObservationPurpose::RowSelection
            && matches!(
                primitive,
                Primitive::AxAction {
                    action: AxActionKind::SetSelectedTrue
                }
            );
        let window_activation = context.authority.evidence.purpose
            == ObservationPurpose::WindowActivation
            && matches!(primitive, Primitive::ActivateWindow { .. });
        let proof_supported =
            |proof: Option<&interaction_contracts::desktop::binding::BindingProof>| {
                proof.is_some_and(|proof| {
                    if row_selection {
                        proof.supports_row_selection()
                    } else if window_activation {
                        proof.supports_window_activation()
                    } else {
                        proof.supports_native_input()
                    }
                })
            };
        if !proof_supported(context.intent.binding_proof.as_ref())
            || !proof_supported(context.authority.evidence.binding_proof.as_ref())
        {
            return false;
        }
        self.ready()
            && self.pending.is_none()
            && match primitive {
                Primitive::KeyPress { key, modifiers } => {
                    self.keyboard_ready
                        && self.window.is_some()
                        && context.authority.evidence.purpose == ObservationPurpose::FocusedControl
                        && *modifiers == Modifiers::default()
                        && context.intent.binding_proof.as_ref().is_some_and(|p| {
                            p.menu_scope.is_none()
                                && p.fingerprint.role == "AXCheckBox"
                                && p.focus_scope.as_ref().is_some_and(|focus| {
                                    focus.profile == FocusedKeyProfile::MacosAtomicSpecialKeyV1
                                        && focus.key == LayoutIndependentSpecialKey::Space
                                        && key == focus.key.canonical_name()
                                })
                        })
                }
                Primitive::AxAction {
                    action: AxActionKind::Press,
                } if context.authority.evidence.purpose == ObservationPurpose::MenuSelection => {
                    self.window.is_some()
                        && context.intent.binding_proof.as_ref().is_some_and(|p| {
                            matches!(p.menu_scope, Some(MenuBindingScope::Item { .. }))
                                && p.focus_scope.is_none()
                        })
                }
                Primitive::AxAction {
                    action: AxActionKind::SetSelectedTrue,
                } => {
                    row_selection
                        && context.intent.binding_proof.as_ref().is_some_and(|proof| {
                            proof.menu_scope.is_none() && proof.focus_scope.is_none()
                        })
                }
                Primitive::AxAction {
                    action:
                        AxActionKind::Press | AxActionKind::SetValue { .. } | AxActionKind::ShowMenu,
                }
                | Primitive::Click { .. }
                | Primitive::Scroll { .. }
                | Primitive::PointerPosition { .. } => {
                    context.authority.evidence.purpose == ObservationPurpose::TargetInput
                        && context.intent.binding_proof.as_ref().is_some_and(|p| {
                            p.focus_scope.is_none()
                                && match &p.menu_scope {
                                    None => true,
                                    Some(MenuBindingScope::Opener { .. }) => matches!(
                                        primitive,
                                        Primitive::AxAction {
                                            action: AxActionKind::ShowMenu
                                        }
                                    ),
                                    Some(MenuBindingScope::Item { .. }) => false,
                                }
                        })
                }
                Primitive::ActivateWindow { app } => {
                    app == &self.target.1
                        && self.window.is_some()
                        && context.authority.evidence.purpose
                            == ObservationPurpose::WindowActivation
                        && context
                            .intent
                            .binding_proof
                            .as_ref()
                            .is_some_and(|p| p.menu_scope.is_none() && p.focus_scope.is_none())
                }
                _ => false,
            }
            && context.intent.steps.len() == 1
            && context.intent.steps[0].offset_ms == 0
            && context.intent.binding_proof.as_ref().is_some_and(|p| {
                p.fingerprint.app_pid == self.target.0
                    && p.fingerprint.app == self.target.1
                    && self
                        .window
                        .as_ref()
                        .is_none_or(|w| &p.fingerprint.window_id == w)
            })
    }
    fn supported_owned_value(&self, primitive: &Primitive, context: &SubmitContext<'_>) -> bool {
        let Some(registration) = self.owned_registration.as_deref() else {
            return false;
        };
        let Some(original) = context.intent.owned_document_proof.as_deref() else {
            return false;
        };
        let Some(current) = context.authority.evidence.owned_document_proof.as_deref() else {
            return false;
        };
        let action_is_authorized = match primitive {
            Primitive::AxAction {
                action: AxActionKind::SetValue { value },
            } => {
                context.authority.evidence.purpose == ObservationPurpose::OwnedDocumentValueSetter
                    && owned_action_value_is_valid("ax_set_owned_document_value", value)
            }
            Primitive::AxAction {
                action: AxActionKind::SaveOwnedDocument { expected_value },
            } => {
                self.owned_save_enabled
                    && context.authority.evidence.purpose == ObservationPurpose::OwnedDocumentSave
                    && owned_action_value_is_valid("ax_save_document", expected_value)
            }
            _ => false,
        };
        self.ready()
            && self.pending.is_none()
            && action_is_authorized
            && context.intent.binding_proof.is_none()
            && context.intent.owned_document_proof.is_some()
            && context.authority.evidence.binding_proof.is_none()
            && context.authority.evidence.owned_document_proof.is_some()
            && original.validate_registration(registration).is_ok()
            && current.validate_registration(registration).is_ok()
            && original.same_instance(current)
            && context.intent.session_epoch == original.session_epoch
            && context.authority.evidence.session_epoch == current.session_epoch
            && context.intent.target_ref == original.binding.as_str()
            && context.authority.evidence.target_ref == current.binding.as_str()
            && original.receiver.process == registration.process_identity()
    }
    pub fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome {
        if self.owned_registration.is_some() {
            HitTestOutcome::Unresolved
        } else if self.supported(request.primitive, request.context) {
            HitTestOutcome::CheckedDuringSubmit
        } else {
            HitTestOutcome::Unresolved
        }
    }
    pub fn submit(&mut self, primitive: &Primitive, context: &SubmitContext<'_>) -> SubmitOutcome {
        if self.owned_registration.is_some() {
            if !self.supported_owned_value(primitive, context) {
                return SubmitOutcome::Rejected;
            }
            let Some(sequence) = self.sequence.checked_add(1) else {
                return SubmitOutcome::Rejected;
            };
            self.preflight_rejection = None;
            let Some(guard) = self.guard.as_mut() else {
                return SubmitOutcome::Rejected;
            };
            if let Err(error) = guard.submit_owned_value_at_revision(
                context,
                primitive,
                self.guard_revision.unwrap(),
                self.owned_registration.as_deref().unwrap(),
            ) {
                eprintln!("owned value submission could not enqueue: {error}");
                return SubmitOutcome::Rejected;
            }
            self.sequence = sequence;
            self.pending = Some((
                sequence,
                context.intent.session_epoch,
                context.intent.intent_id.clone(),
            ));
            return SubmitOutcome::Pending {
                sequence,
                reply_deadline_ms: context.authority.valid_until_ms.saturating_add(500),
            };
        }
        if !self.supported(primitive, context) {
            return SubmitOutcome::Rejected;
        }
        let Some(sequence) = self.sequence.checked_add(1) else {
            return SubmitOutcome::Rejected;
        };
        self.preflight_rejection = None;
        let guard = self.guard.as_mut().unwrap();
        if let Err(error) =
            guard.submit_press_at_revision(context, primitive, self.guard_revision.unwrap())
        {
            eprintln!("native submission could not enqueue: {error}");
            return SubmitOutcome::Rejected;
        }
        self.sequence = sequence;
        self.pending = Some((
            sequence,
            context.intent.session_epoch,
            context.intent.intent_id.clone(),
        ));
        SubmitOutcome::Pending {
            sequence,
            reply_deadline_ms: context.authority.valid_until_ms.saturating_add(500),
        }
    }
    pub fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        let (sequence, session_epoch, intent_id) = self.pending.as_ref()?.clone();
        let polled = self
            .preflight_rejection
            .take()
            .map(|reply| Ok(Some(reply)))
            .or_else(|| self.guard.as_mut().map(|guard| guard.poll_submission()));
        let response = match polled {
            Some(Ok(None)) => return None,
            Some(Ok(Some(reply))) => {
                if !reply.submitted {
                    eprintln!(
                        "native submission refused: {}: {}",
                        reply.outcome, reply.reason
                    );
                }
                (
                    match reply.outcome.as_str() {
                        "applied" => SubmitOutcome::Applied,
                        "unknown" => SubmitOutcome::Unknown,
                        _ => SubmitOutcome::Rejected,
                    },
                    reply.committed_ms,
                    reply.returned_ms,
                    match reply.outcome.as_str() {
                        "applied" => None,
                        "target_occluded" => Some(FailureCode::TargetOccluded),
                        "context_unresolved" => Some(FailureCode::ContextUnresolved),
                        "expired" => Some(FailureCode::EvidenceExpired),
                        "stop_latched" => Some(FailureCode::StopLatched),
                        "revoked" => Some(FailureCode::Cancelled),
                        "unknown" => Some(FailureCode::EffectUnknown),
                        _ => Some(FailureCode::InputRejected),
                    },
                )
            }
            _ => {
                self.ready = false;
                (
                    SubmitOutcome::Unknown,
                    None,
                    None,
                    Some(FailureCode::EffectUnknown),
                )
            }
        };
        self.pending = None;
        Some(SubmitCompletion {
            sequence,
            session_epoch,
            intent_id,
            outcome: response.0,
            submitted_at_ms: response.1,
            input_end_ms: response.2,
            failure_code: response.3,
        })
    }
    pub fn revoke(&mut self, permit: &PermitRef) {
        if let Some(guard) = &mut self.guard {
            guard.revoke(permit);
        }
    }
    pub fn abandon_pending(&mut self) {
        self.ready = false;
        self.pending = None;
        self.preflight_rejection = None;
        if let Some(guard) = &mut self.guard {
            guard.terminate();
        }
    }
}
