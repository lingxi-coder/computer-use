//! Evidence: snapshots in, bindings kept honest, fresh observations asked for
//! (design 6 and 20.1).

use std::collections::BTreeSet;

use interaction_contracts::desktop::browser::{BrowserRefreshRequest, browser_target_context_hash};
use interaction_contracts::desktop::device::DependencyUpdate;
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::ids::BindingRef;
use interaction_contracts::desktop::source::{
    FrameMeta, SourceCommand, SourceHealth, SourceKind, SourceState, StructureSnapshot,
};
use interaction_contracts::desktop::task::{RootState, RowTableScope};

use super::{Engine, EngineError, Output, SourceScopeCommandState, Step, Wait};
use crate::environment::binding::Invalidated;
use crate::execution::{DeviceCommand, Expect, Stage};
use crate::infrastructure::storage::StoreError;
use crate::tasks::budget::BudgetKind;
use sha2::{Digest, Sha256};

const EXPLICIT_REFRESH_INVALIDATION_REASON: &str =
    "an explicit refresh invalidated the previous source stream";
const PRE_FRAME_OCR_NOT_CAPTURED_REASON: &str = "not captured";

fn is_pre_frame_ocr_placeholder(health: &SourceHealth, frame_callback_seen: bool) -> bool {
    health.source == SourceKind::Ocr
        && health.state == SourceState::Unavailable
        && health.reason.as_deref() == Some(PRE_FRAME_OCR_NOT_CAPTURED_REASON)
        && !frame_callback_seen
}

fn browser_refresh_uuid(
    scope_id: &str,
    generation: u64,
    source_sequence: u64,
    after_ms: u64,
    deadline_ms: u64,
) -> String {
    let digest = Sha256::digest(
        format!("{scope_id}\n{generation}\n{source_sequence}\n{after_ms}\n{deadline_ms}")
            .as_bytes(),
    );
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("SHA-256 prefix length");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

impl Engine {
    pub(super) fn on_refresh_started(
        &mut self,
        generation: u64,
        accepted: bool,
    ) -> Result<(), EngineError> {
        if self.source_scope_clear_generation == Some(generation) {
            self.source_scope_clear_generation = None;
        }
        let started = accepted && self.state.refresh_started(generation);
        let revisit_matches = self.owned_revisit.as_ref().is_some_and(|progress| {
            progress.phase == super::owned_revisit::OwnedRevisitPhase::SourceProof
                && progress.source_refresh_generation == Some(generation)
        });
        if started && revisit_matches {
            if let Some(progress) = self.owned_revisit.as_mut() {
                progress.source_refresh_accepted = true;
            }
            self.note("the owned revisit Source refresh was acknowledged");
        } else if self.owned_revisit.is_some() && accepted {
            self.note("the owned revisit ignored a nonmatching Source refresh acknowledgement");
        } else if !accepted
            && self
                .owned_revisit
                .as_ref()
                .is_some_and(|progress| progress.source_refresh_generation == Some(generation))
        {
            let transition_id = self
                .owned_revisit
                .as_ref()
                .map(|progress| progress.challenge.transition_id.clone());
            if let Some(transition_id) = transition_id {
                self.cancel_owned_revisit(&transition_id, self.now_ms, "Source refresh rejected")
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            }
        }
        if started {
            self.restore_source_evidence();
            self.sync_window_evidence()?;
        }
        Ok(())
    }

    fn frozen_source_table_scope(&self) -> Option<RowTableScope> {
        self.run
            .as_ref()
            .and_then(|run| run.goal.target.row.as_ref())
            .map(|row| row.table_scope.clone())
    }

    fn send_refresh(&mut self) -> Result<(), EngineError> {
        self.send_refresh_with_scope(self.frozen_source_table_scope())
    }

    fn send_refresh_with_scope(
        &mut self,
        table_scope: Option<RowTableScope>,
    ) -> Result<(), EngineError> {
        let Some(generation) = self.state.begin_refresh() else {
            self.fail_root("the source refresh generation is exhausted")?;
            return Ok(());
        };
        self.emit(Output::Source {
            generation,
            command: SourceCommand::RefreshNow {
                table_scope: table_scope.clone(),
            },
        });
        // A new business refresh supersedes any outstanding clear-only
        // acknowledgement correlation. Its own generation remains pending in
        // StateCore as usual.
        self.source_scope_clear_generation = None;
        self.source_scope_command_state = if table_scope.is_some() {
            SourceScopeCommandState::Scoped
        } else {
            SourceScopeCommandState::Cleared
        };
        Ok(())
    }

    fn send_browser_refresh(&mut self, floor_ms: u64) {
        let (scope, latest) = match (self.state.browser_scope(), self.state.browser_latest()) {
            (Some(scope), Some(latest)) => (scope.clone(), latest.clone()),
            // The first page publication follows private scope installation.
            // A request without a prior sequence is not a valid refresh.
            _ => return,
        };
        if latest.scope_id != scope.scope_id || latest.generation != scope.generation {
            return;
        }
        let after_ms = floor_ms.max(latest.snapshot.observed_ms);
        let timeout_ms = self.cfg.refresh_timeout_ms.clamp(1, 30_000);
        let deadline_ms = self
            .now_ms
            .saturating_add(timeout_ms)
            .min(after_ms.saturating_add(30_000));
        if deadline_ms <= after_ms || deadline_ms <= self.now_ms {
            return;
        }
        let Ok(target_context_hash) = browser_target_context_hash(&scope.context) else {
            return;
        };
        let request = BrowserRefreshRequest {
            request_id: browser_refresh_uuid(
                &scope.scope_id,
                scope.generation,
                latest.snapshot.source_sequence,
                after_ms,
                deadline_ms,
            ),
            scope_id: scope.scope_id,
            generation: scope.generation,
            context: scope.context,
            target_context_hash,
            clock_domain: scope.clock_domain,
            after_source_sequence: latest.snapshot.source_sequence,
            after_ms,
            deadline_ms,
        };
        if request.validate().is_ok() {
            self.emit(Output::BrowserRefresh(Box::new(request)));
        }
    }

    /// Clear Source's retained row scope without making Engine wait for a new
    /// business observation. A force clear is used when a Source session may
    /// have retained state unknown to this Engine instance.
    pub(super) fn clear_source_table_scope(&mut self, force: bool) {
        if !force && self.source_scope_command_state == SourceScopeCommandState::Cleared {
            return;
        }
        let Some(generation) = self.state.next_refresh_generation() else {
            self.note("the Source scope clear could not allocate an acknowledgement generation");
            return;
        };
        self.emit(Output::Source {
            generation,
            command: SourceCommand::RefreshNow { table_scope: None },
        });
        self.source_scope_clear_generation = Some(generation);
        self.source_scope_command_state = SourceScopeCommandState::Cleared;
    }
    pub(super) fn on_structure(&mut self, snapshot: StructureSnapshot) -> Result<(), EngineError> {
        if let Some(progress) = self.owned_revisit.as_ref()
            && (progress.phase != super::owned_revisit::OwnedRevisitPhase::SourceProof
                || !progress.source_refresh_accepted
                || self.state.refresh_pending())
        {
            if !self.timeline.back().is_some_and(|item| {
                item.text == "the owned revisit waits for its Source refresh acknowledgement"
            }) {
                self.note("the owned revisit waits for its Source refresh acknowledgement");
            }
            return Ok(());
        }
        let pack = self.pack_for(&snapshot.app.bundle_id);
        let had_previous = self.state.latest().is_some();
        let owned_before = self
            .run
            .as_ref()
            .map(|run| run.goal.target.semantic_target.clone())
            .and_then(|target| self.owned_document_proof_for(&target));
        let identity_boundary_changed = self
            .state
            .identity_boundary_changed(snapshot.identity_boundary.as_ref());
        let observer_root_unavailable =
            snapshot.observer_coverage.state == SourceState::Unavailable;
        let previous_menu = self
            .state
            .latest()
            .and_then(|env| env.snapshot.menu_context.clone());
        let now = self.now_ms;
        let admitted = match self.state.admit(snapshot, now, &pack) {
            Ok(admitted) => admitted,
            Err(reject) => {
                self.note(format!("a snapshot was refused: {reject}"));
                return Ok(());
            }
        };
        let Some(env) = self.state.latest().cloned() else {
            return Ok(());
        };
        // Native-owned snapshots intentionally omit the generic identity
        // boundary. Keep the private lane alive only when both the previous
        // and newly admitted typed proofs independently validate to the same
        // receiver scope. On the very first snapshot there is no prior scope
        // to compare, so the admitted private registration/proof establishes
        // the baseline; after any reset, an existing prior snapshot requires
        // a successful same-instance comparison.
        let owned_scope_continuous = self
            .run
            .as_ref()
            .map(|run| run.goal.target.semantic_target.clone())
            .and_then(|target| self.owned_document_proof_for(&target))
            .is_some_and(|current| {
                !had_previous
                    || owned_before
                        .as_ref()
                        .is_some_and(|previous| previous.same_instance(&current))
            });
        self.last_structure_arrival_ms = Some(now);
        if identity_boundary_changed || observer_root_unavailable {
            let reason = if observer_root_unavailable {
                "the native Source root observer is unavailable"
            } else {
                "the native Source identity boundary changed"
            };
            if owned_scope_continuous {
                self.invalidate_evidence_with_authority_reset(reason, true, true)?;
            } else {
                self.invalidate_identity_boundary(reason)?;
            }
        }
        self.sync_window_evidence()?;
        if self.state.requires_frames() && !self.current_execution_evidence() {
            // A healthy background capture is useful for the operator, but
            // cannot keep a decision, dispatch or input permit alive.
            let reason =
                "the target is not the active captured window with current execution evidence";
            if owned_scope_continuous {
                self.invalidate_evidence_with_authority_reset(reason, false, true)?;
            } else {
                self.invalidate_source_evidence(reason)?;
            }
        }
        // A binding only holds for the evidence it was made from. Anything that
        // no longer fits is dropped, and the device is told so its permits for
        // that binding stop being valid.
        let dropped = self.bindings.revalidate(&env);
        for binding in &dropped {
            self.note(format!(
                "binding for {} dropped: {}",
                binding.semantic_target, binding.reason
            ));
        }
        if admitted.layer_changed && had_previous {
            self.bump_dependency("layer");
        }
        let menu_changed = match (&previous_menu, &env.snapshot.menu_context) {
            (None, None) => false,
            (Some(before), Some(after)) => !before.same_identity(after),
            _ => true,
        };
        if menu_changed {
            self.bump_dependency("menu");
        }
        if !dropped.is_empty() {
            self.invalidate_binding_authority(&dropped)?;
        }
        if let Some(run) = self.run.as_mut() {
            run.verification
                .visits
                .observe(env.page.as_deref(), env.snapshot.observed_ms);
        }
        if !observer_root_unavailable {
            self.restore_source_evidence();
        }
        Ok(())
    }

    /// Retire authority attached to exactly the bindings that failed current
    /// Source revalidation. The shared device `binding` dependency is not
    /// advanced here because unrelated covered bindings may still be valid.
    fn invalidate_binding_authority(
        &mut self,
        invalidated: &[Invalidated],
    ) -> Result<(), EngineError> {
        let references: BTreeSet<BindingRef> = invalidated
            .iter()
            .map(|entry| entry.binding.clone())
            .collect();
        let reason = invalidated
            .first()
            .map(|entry| entry.reason.as_str())
            .unwrap_or("the binding context changed");

        let pending_depends_on_invalidated = self.decisions.pending().is_some_and(|pending| {
            pending
                .basis
                .bindings
                .iter()
                .any(|binding| references.contains(binding))
        });
        if pending_depends_on_invalidated
            && let Some(cancel) = self.decisions.close(
                &mut self.storage,
                crate::ports::DecisionLifecycle::Superseded,
            )?
        {
            self.emit(Output::CancelDecision(cancel));
        }

        let stale_preinput_dispatch = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
            .filter(|dispatch| matches!(dispatch.stage, Stage::Observing | Stage::Granting))
            .and_then(|dispatch| dispatch.observation.native())
            .and_then(|observation| observation.binding_proof.as_ref())
            .is_some_and(|proof| references.contains(&proof.binding));
        if stale_preinput_dispatch {
            self.abort_dispatch(
                Some(interaction_contracts::desktop::error::FailureCode::SourceInvalid),
                reason,
            );
        }

        let affected_ids: BTreeSet<String> = self
            .permit_bindings
            .iter()
            .filter(|(_, binding)| references.contains(binding))
            .map(|(permit_id, _)| permit_id.clone())
            .collect();
        let permits = if affected_ids.is_empty() {
            Vec::new()
        } else {
            self.run
                .as_mut()
                .map(|run| {
                    let mut affected = Vec::new();
                    run.permits.retain(|permit| {
                        if affected_ids.contains(&permit.id) {
                            affected.push(permit.clone());
                            false
                        } else {
                            true
                        }
                    });
                    affected
                })
                .unwrap_or_default()
        };
        let session_epoch = self.device.session_epoch();
        for permit in permits {
            self.permit_bindings.remove(&permit.id);
            if let Some(session_epoch) = session_epoch {
                self.send(
                    Expect::Revoke,
                    DeviceCommand::Revoke(
                        interaction_contracts::desktop::device::RevokeExecution {
                            session_epoch,
                            permit,
                            reason: reason.into(),
                        },
                    ),
                );
            }
        }
        self.permit_bindings
            .retain(|permit_id, _| !affected_ids.contains(permit_id));
        Ok(())
    }

    pub(super) fn on_health(&mut self, mut health: SourceHealth) -> Result<(), EngineError> {
        health.reason = health
            .reason
            .map(|reason| reason.chars().take(1_024).collect());
        let known_session_epoch = [
            SourceKind::Screenshot,
            SourceKind::Ax,
            SourceKind::AxObserver,
            SourceKind::Ocr,
        ]
        .into_iter()
        .filter_map(|source| self.state.health_stamp(source))
        .map(|stamp| stamp.session_epoch)
        .next();
        let previous_stamp = self.state.health_stamp(health.source);
        let session_replaced = match (previous_stamp, health.provenance.as_ref()) {
            (Some(previous), Some(incoming)) => previous.session_epoch != incoming.session_epoch,
            // Unknown means Engine has not sent a scope command in this
            // lifetime. Without a prior Source epoch it is not evidence that
            // this health item replaced a known session.
            (None, Some(incoming)) => {
                known_session_epoch.is_some_and(|epoch| epoch != incoming.session_epoch)
            }
            _ => false,
        };
        // Before the first screenshot callback, Native reports an auxiliary
        // OCR placeholder. Keep that health unavailable, but do not let this
        // one exact pre-frame state erase NativeConfig's initial table scope.
        // FrameTracker::view retains every callback (including started,
        // blank, stopped and expired callbacks) until the identity resets.
        let frame_callback_seen = self
            .state
            .observation_view(self.now_ms)
            .is_some_and(|view| view.frame.is_some());
        let pre_frame_ocr_placeholder = is_pre_frame_ocr_placeholder(&health, frame_callback_seen);
        let any_source_unavailable = [
            SourceKind::Screenshot,
            SourceKind::Ax,
            SourceKind::AxObserver,
            SourceKind::Ocr,
        ]
        .into_iter()
        .any(|source| {
            self.state
                .health(source)
                .is_some_and(|previous| previous.state == SourceState::Unavailable)
        });
        // Native watch faults can publish Unavailable for all four channels.
        // Treat the first channel as the transition and let the emitted clear
        // cover the rest of that fault batch.
        let first_unavailable = health.state == SourceState::Unavailable && !any_source_unavailable;
        // RefreshNow itself reports that it retired the old stream. While its
        // acknowledgement is pending this is an expected transition, not a
        // disconnect; otherwise a row-scoped refresh would clear its own scope.
        let expected_refresh_transition = health.reason.as_deref()
            == Some(EXPLICIT_REFRESH_INVALIDATION_REASON)
            && (self.state.refresh_pending() || self.source_scope_clear_generation.is_some());
        let clear_for_unavailable = health.state == SourceState::Unavailable
            && !expected_refresh_transition
            && !pre_frame_ocr_placeholder
            && (first_unavailable
                || self.source_scope_command_state != SourceScopeCommandState::Cleared);
        let clear_scope = session_replaced || clear_for_unavailable;
        let reason = health
            .reason
            .clone()
            .unwrap_or_else(|| format!("{:?} source is {:?}", health.source, health.state));
        let changed = self
            .state
            .health(health.source)
            .is_none_or(|old| old.state != health.state || old.reason != health.reason);
        let row_domain_degraded =
            health.source == SourceKind::Ax && health.state == SourceState::Degraded;
        match self.state.note_health(health, self.now_ms) {
            Ok(invalidated) => {
                if clear_scope {
                    // Emit after validating health provenance, but before
                    // evidence retirement can fail on storage.
                    self.clear_source_table_scope(true);
                }
                if invalidated && !(row_domain_degraded && self.row_root_evidence_usable()) {
                    self.invalidate_source_evidence(&reason)?;
                } else if changed {
                    self.note(format!("source status: {reason}"));
                }
                self.restore_source_evidence();
                self.sync_window_evidence()?;
            }
            Err(error) => self.note(format!("source health was refused: {error}")),
        }
        Ok(())
    }

    pub(super) fn on_frame(&mut self, meta: FrameMeta) -> Result<(), EngineError> {
        match self
            .state
            .admit_frame(meta, self.now_ms, self.cfg.evidence_max_age_ms)
        {
            Ok(change) => {
                if self.state.requires_frames() && (change.invalidated || change.context_changed) {
                    self.invalidate_source_evidence("the screen stream or its geometry changed")?;
                }
                self.restore_source_evidence();
            }
            Err(error) => self.note(format!("a screen frame was refused: {error}")),
        }
        Ok(())
    }

    pub(super) fn expire_source_evidence(&mut self) -> Result<(), EngineError> {
        self.state
            .expire_owned_document_binding(self.now_ms, self.cfg.evidence_max_age_ms);
        self.sync_window_evidence()?;
        if let Some(reason) = self
            .state
            .expire_sources(self.now_ms, self.cfg.evidence_max_age_ms)
        {
            self.note(reason);
            if self.state.requires_frames() {
                self.invalidate_source_evidence(reason)?;
            }
        }
        Ok(())
    }

    pub(super) fn invalidate_source_evidence(&mut self, reason: &str) -> Result<(), EngineError> {
        self.invalidate_evidence_with_authority_reset(reason, false, false)
    }

    pub(super) fn invalidate_source_domain(&mut self, reason: &str) -> Result<(), EngineError> {
        self.invalidate_evidence_with_authority_reset(reason, true, false)
    }

    /// An AX identity boundary invalidates target authority even when the
    /// current snapshot and captured pixels remain usable.
    fn invalidate_identity_boundary(&mut self, reason: &str) -> Result<(), EngineError> {
        self.invalidate_evidence_with_authority_reset(reason, true, false)
    }

    fn invalidate_evidence_with_authority_reset(
        &mut self,
        reason: &str,
        force_target_reset: bool,
        preserve_owned_scope: bool,
    ) -> Result<(), EngineError> {
        if self.source_gate_closed && !force_target_reset {
            return Ok(());
        }
        let newly_closed = !self.source_gate_closed;
        self.source_gate_closed = true;
        if newly_closed {
            self.note(format!("execution evidence is unavailable: {reason}"));
        }
        let keep_window =
            !force_target_reset && self.window_attempt() && self.window_evidence_usable();
        if keep_window {
            self.bindings.invalidate_target_inputs();
        } else {
            self.bindings.invalidate_all(reason);
        }
        let current_binding_refs: BTreeSet<BindingRef> = self
            .bindings
            .all()
            .map(|binding| binding.binding_ref.clone())
            .collect();
        self.permit_bindings
            .retain(|_, binding| current_binding_refs.contains(binding));
        let current_owned = if preserve_owned_scope {
            self.run
                .as_ref()
                .map(|run| &run.goal.target.semantic_target)
                .and_then(|target| self.owned_document_proof_for(target))
        } else {
            None
        };
        let preserve_owned_decision = current_owned.as_ref().is_some_and(|current| {
            self.decisions.pending().is_some_and(|pending| {
                pending
                    .basis
                    .owned_document
                    .as_ref()
                    .is_some_and(|basis| basis.same_instance(current))
                    && pending
                        .basis
                        .bindings
                        .iter()
                        .all(|binding| self.bindings.contains_binding_ref(binding))
            })
        });
        let preserve_owned_attempt = current_owned
            .as_ref()
            .is_some_and(|current| self.owned_document_attempt_preservable_for(current));
        if !preserve_owned_decision
            && let Some(cancel) = self.decisions.close(
                &mut self.storage,
                crate::ports::DecisionLifecycle::Superseded,
            )?
        {
            self.emit(Output::CancelDecision(cancel));
        }
        if let Some(run) = self.run.as_mut() {
            run.decision_retry_at = None;
        }
        let unresolved = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| {
                let sent = attempt.mechanical_unknown
                    || attempt.dispatch.as_ref().is_some_and(|dispatch| {
                        matches!(
                            dispatch.stage,
                            crate::execution::Stage::Submitting | crate::execution::Stage::Running
                        )
                    });
                sent.then(|| {
                    (1..=attempt.sequence)
                        .map(|sequence| {
                            (
                                attempt.id.clone(),
                                format!("intent-{}-{sequence}", attempt.id),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            });
        let known_menu = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| {
                attempt.skill.skill() == crate::reaction::skills::Skill::SelectByLabel
                    && attempt.menu_opening.is_some()
                    && !attempt.mechanical_unknown
                    && !attempt.dispatch.as_ref().is_some_and(|dispatch| {
                        matches!(
                            dispatch.stage,
                            crate::execution::Stage::Submitting | crate::execution::Stage::Running
                        )
                    })
            });
        if known_menu
            && self
                .run
                .as_ref()
                .and_then(|run| run.attempt.as_ref())
                .is_some_and(|attempt| attempt.dispatch.is_some())
        {
            // No choice input has left. Fail that pending step inside the
            // original transaction; do not create an attempt that reopens it.
            self.abort_dispatch(
                Some(interaction_contracts::desktop::error::FailureCode::SourceInvalid),
                reason,
            );
        }
        // A known terminal input can keep its read/revisit phase. A pending
        // owned pre-input attempt is retained only above when its private
        // registration and receiver still match; other pending or unknown
        // calls are still reconciled before any late reply can advance them.
        let awaiting_known_effect = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| {
                attempt.input_reached && !attempt.mechanical_unknown && attempt.dispatch.is_none()
            });
        if !awaiting_known_effect && !keep_window && !known_menu && !preserve_owned_attempt {
            self.end_attempt_cancelled()?;
        }
        if let Some(intents) = &unresolved
            && let Some(run) = self.run.as_mut()
        {
            run.reconcile.extend(intents.iter().cloned());
            run.resume_authorized = false;
        }
        self.set_evidence_dependency("binding", false);
        self.set_evidence_dependency("geometry", false);
        if force_target_reset && self.cfg.native_window_scope.is_some() {
            // WindowActivation permits do not depend on binding or geometry.
            // Advance their dependency epoch while keeping the current window
            // evidence usable for a fresh permit.
            self.set_evidence_dependency("window", self.window_evidence_usable());
        }
        // Pending owned Observe/Grant has no returned permit yet. Revoke any
        // permit already held by the root so generic authority stays closed.
        let preserved_owned_permits: BTreeSet<String> = current_owned
            .as_ref()
            .map(|current| {
                self.permit_owned_proofs
                    .iter()
                    .filter(|(_, proof)| proof.same_instance(current))
                    .map(|(permit, _)| permit.clone())
                    .collect()
            })
            .unwrap_or_default();
        let permits = if keep_window {
            Vec::new()
        } else {
            self.run
                .as_mut()
                .map(|run| {
                    let mut revoke = Vec::new();
                    run.permits.retain(|permit| {
                        if preserved_owned_permits.contains(&permit.id) {
                            true
                        } else {
                            revoke.push(permit.clone());
                            false
                        }
                    });
                    revoke
                })
                .unwrap_or_default()
        };
        self.permit_owned_proofs
            .retain(|permit, _| preserved_owned_permits.contains(permit));
        if let Some(session_epoch) = self.device.session_epoch() {
            for permit in permits {
                self.send(
                    Expect::Revoke,
                    DeviceCommand::Revoke(
                        interaction_contracts::desktop::device::RevokeExecution {
                            session_epoch,
                            permit,
                            reason: reason.into(),
                        },
                    ),
                );
            }
        }
        if unresolved.is_some() && !keep_window {
            // Fresh pixels do not authorize another submission of an input
            // whose result is unknown. Resume must reconcile the original ids.
            self.pause_root(
                "execution evidence was invalidated while an input outcome was unresolved",
            )?;
        }
        Ok(())
    }

    pub(super) fn set_evidence_dependency(&mut self, name: &str, valid: bool) {
        if self.device.manifest().is_some() {
            self.send(
                Expect::Dependency,
                DeviceCommand::UpdateDependency(DependencyUpdate {
                    name: name.into(),
                    valid: Some(valid),
                }),
            );
        }
    }

    fn restore_source_evidence(&mut self) {
        let background_scope_usable = self.device.manifest().is_some_and(|manifest| {
            manifest.execution_domain == ExecutionDomain::BackgroundAx
                && self.background_ax_evidence_usable(None)
        });
        if background_scope_usable {
            self.background_scope_sequence_floor = None;
            self.set_evidence_dependency("background_scope", true);
        }
        if self.source_gate_closed && (background_scope_usable || self.current_execution_evidence())
        {
            self.source_gate_closed = false;
            self.set_evidence_dependency("binding", true);
            self.set_evidence_dependency("geometry", true);
            self.note("fresh source evidence is available; new bindings and permits are required");
        }
    }

    /// Tell the device a dependency its permits rest on changed.
    pub(super) fn bump_dependency(&mut self, name: &str) {
        if self.device.manifest().is_none() {
            return;
        }
        self.send(
            Expect::Dependency,
            DeviceCommand::UpdateDependency(DependencyUpdate {
                name: name.to_string(),
                valid: Some(!(self.source_gate_closed && matches!(name, "binding" | "geometry"))),
            }),
        );
    }

    /// Evidence new enough and complete enough to act on.
    pub(super) fn evidence_usable(&self) -> bool {
        self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
            && self
                .state
                .latest()
                .is_some_and(|env| env.snapshot.observer_coverage.state != SourceState::Unavailable)
    }

    pub(super) fn goal_evidence_usable(&self) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        match run.goal.execution_domain {
            ExecutionDomain::ForegroundNative | ExecutionDomain::Simulated => {
                self.evidence_usable()
            }
            ExecutionDomain::BackgroundAx => {
                !self.source_gate_closed && self.background_ax_evidence_usable(None)
            }
            ExecutionDomain::HeadlessBrowser => self.browser_goal_evidence_usable(),
            ExecutionDomain::AppIntegration => false,
        }
    }

    pub(super) fn execution_profile_matches_goal(&self) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        self.device
            .manifest()
            .is_some_and(|manifest| manifest.execution_domain == run.goal.execution_domain)
            && (run.goal.execution_domain != ExecutionDomain::HeadlessBrowser
                || run.goal.browser_page_profile == self.cfg.browser_page_profile)
            && self.device.execution_domain_available()
    }

    pub(super) fn background_ax_evidence_usable(
        &self,
        expected_target_context_hash: Option<&str>,
    ) -> bool {
        let Some(scope) = self.cfg.native_background_window_scope.as_ref() else {
            return false;
        };
        if scope.validate().is_err()
            || self.run.as_ref().is_some_and(|run| {
                run.goal.execution_domain != ExecutionDomain::BackgroundAx
                    || run.goal.scope.app != scope.app
            })
        {
            return false;
        }
        self.background_scope_sequence_floor.is_none_or(|floor| {
            self.state
                .latest()
                .is_some_and(|env| env.snapshot.source_sequence > floor)
        }) && self.state.background_ax_usable(
            (&scope.app, scope.pid),
            &scope.window,
            scope.process_birth(),
            expected_target_context_hash,
            self.now_ms,
            self.cfg.evidence_max_age_ms.min(2_000),
        )
    }

    pub(super) fn native_context_settling(&self) -> bool {
        self.device
            .identity()
            .is_some_and(|id| id.clock_domain.starts_with("macos/"))
            && self.last_structure_arrival_ms.is_some_and(|at| {
                self.now_ms.saturating_sub(at) < self.cfg.native_evidence_quiet_ms
            })
    }

    /// Ask the source for a new observation, observed at or after `floor_ms`.
    /// Observations are budgeted like any other work; asking again while one is
    /// awaited only raises what it must satisfy.
    pub(super) fn request_refresh(&mut self, floor_ms: u64, why: &str) -> Step {
        self.await_source(floor_ms, why, true)
    }

    pub(super) fn await_source(&mut self, floor_ms: u64, why: &str, refresh: bool) -> Step {
        let refresh = refresh && !self.menu_same_watch_required();
        let Some(run) = self.run.as_mut() else {
            return Ok(false);
        };
        if let Some(wait) = run.wait.as_mut() {
            wait.floor_ms = wait.floor_ms.max(floor_ms);
            return Ok(false);
        }
        if !self
            .tasks
            .spend(&mut self.storage, BudgetKind::Observation, 1)?
        {
            self.fail_root("the observation budget is exhausted")?;
            return Ok(true);
        }
        let now = self.now_ms;
        if let Some(run) = self.run.as_mut() {
            run.wait = Some(Wait {
                floor_ms,
                requested_ms: now,
            });
        }
        if refresh {
            let browser_goal = self
                .run
                .as_ref()
                .is_some_and(|run| run.goal.execution_domain == ExecutionDomain::HeadlessBrowser);
            if browser_goal {
                self.send_browser_refresh(floor_ms);
            } else {
                self.send_refresh()?;
            }
        }
        self.note(format!("observing again: {why}"));
        if self.tasks.active_state() == Some(RootState::Running) {
            self.tasks.await_evidence(&mut self.storage, now)?;
        }
        Ok(true)
    }

    /// Back to work once the evidence that was waited for has arrived.
    pub(super) fn step_waiting_evidence(&mut self) -> Step {
        if self.owned_revisit.is_some() {
            if self
                .owned_revisit
                .as_ref()
                .is_some_and(|progress| self.now_ms >= progress.challenge.deadline_ms)
            {
                return self.advance_owned_revisit();
            }
            let floor = self
                .run
                .as_ref()
                .and_then(|run| run.wait)
                .map(|wait| wait.floor_ms);
            let proof_arrived = self.owned_revisit.as_ref().is_some_and(|progress| {
                progress.phase == super::owned_revisit::OwnedRevisitPhase::SourceProof
                    && progress.source_refresh_accepted
                    && !self.state.refresh_pending()
            }) && floor
                .is_some_and(|floor| self.state.latest_since(floor).is_some());
            if !proof_arrived {
                return Ok(false);
            }
            if let Some(run) = self.run.as_mut() {
                run.wait = None;
            }
            self.tasks
                .evidence_arrived(&mut self.storage, self.now_ms)?;
            return Ok(true);
        }
        let floor = self
            .run
            .as_ref()
            .and_then(|run| run.wait)
            .map(|w| w.floor_ms);
        let browser_goal = self
            .run
            .as_ref()
            .is_some_and(|run| run.goal.execution_domain == ExecutionDomain::HeadlessBrowser);
        let arrived = match floor {
            Some(floor) if browser_goal => self
                .state
                .browser_latest()
                .is_some_and(|current| current.snapshot.observed_ms >= floor),
            Some(floor) => self.state.latest_since(floor).is_some(),
            // Nothing is waited for, so there is nothing to wait for any longer.
            None => true,
        };
        if !arrived || !self.attempt_evidence_usable() {
            return Ok(false);
        }
        if let Some(run) = self.run.as_mut() {
            run.wait = None;
        }
        self.tasks
            .evidence_arrived(&mut self.storage, self.now_ms)?;
        Ok(true)
    }

    /// Ask again when an observation did not arrive in time.
    pub(super) fn retry_stale_wait(&mut self) -> Result<(), EngineError> {
        if self.owned_revisit.is_some() {
            // The private lifecycle has its own bounded deadline and refresh
            // correlation. Generic retries would restart the new native watch
            // before its first read and supersede the required acknowledgement.
            return Ok(());
        }
        let now = self.now_ms;
        if self.menu_attempt()
            && self
                .run
                .as_ref()
                .and_then(|run| run.attempt.as_ref())
                .is_some_and(|attempt| now >= attempt.deadline_ms && attempt.dispatch.is_none())
        {
            self.finish_attempt(crate::reaction::skills::SkillResult::Failed {
                code: interaction_contracts::desktop::error::FailureCode::EvidenceExpired,
                reason: "the original menu attempt expired while waiting for its source watch"
                    .into(),
            })?;
            return Ok(());
        }
        if self.menu_same_watch_required() {
            // This is still the same budgeted observation. Restarting the
            // helper would destroy the anchor and cannot satisfy this wait.
            return Ok(());
        }
        let limit = self.cfg.refresh_timeout_ms;
        let stale = self
            .run
            .as_ref()
            .and_then(|run| run.wait)
            .is_some_and(|wait| now.saturating_sub(wait.requested_ms) > limit);
        if !stale {
            return Ok(());
        }
        if self.computer.is_active()
            || self
                .run
                .as_ref()
                .and_then(|run| run.attempt.as_ref())
                .is_some_and(|attempt| attempt.computer_step.is_some())
        {
            // RefreshNow replaces the watch and cannot provide the same-watch
            // observation required by an outstanding computer action.
            self.pause_root("fresh evidence from the original computer watch did not arrive")?;
            return Ok(());
        }
        if !self
            .tasks
            .spend(&mut self.storage, BudgetKind::Observation, 1)?
        {
            self.fail_root("the observation budget is exhausted")?;
            return Ok(());
        }
        if let Some(wait) = self.run.as_mut().and_then(|run| run.wait.as_mut()) {
            wait.requested_ms = now;
        }
        let browser_goal = self
            .run
            .as_ref()
            .is_some_and(|run| run.goal.execution_domain == ExecutionDomain::HeadlessBrowser);
        if browser_goal {
            let floor = self
                .run
                .as_ref()
                .and_then(|run| run.wait)
                .map_or(now, |wait| wait.floor_ms);
            self.send_browser_refresh(floor);
        } else {
            self.send_refresh()?;
        }
        self.note("the source has not answered; asking again");
        Ok(())
    }
}

#[cfg(test)]
#[path = "evidence_identity_tests.rs"]
mod identity_tests;
