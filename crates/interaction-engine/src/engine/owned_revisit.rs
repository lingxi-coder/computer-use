//! Host-private continuation for proving an owned document was saved and
//! reopened for the same frozen root. This API is intentionally separate from
//! `Input`, `OperatorCommand`, and the desktop message envelope.

use std::fmt;

use interaction_contracts::desktop::device::{CapabilityState, ReleaseStatus};
use interaction_contracts::desktop::ids::{AttemptId, RootTaskId};
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::source::SourceIdentityBoundary;
use interaction_contracts::desktop::task::{
    AttemptState, ConfirmationScope, ExpectedValue, RootState,
};
use interaction_contracts::desktop::verification::{EffectResult, EffectStatus, TimeSpan};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Engine, EngineError, Output, Step};
use crate::infrastructure::storage::{Storage, StoreError};
use crate::tasks::store::{AttemptRow, TaskStore};
use crate::verification::{
    OwnedRevisitConclusion, VerificationStore, conclude_owned_revisit, owned_revisit_goal_supported,
};

/// Opaque, bounded challenge returned to the trusted Host/Main control path.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedRevisitChallenge {
    pub version: u8,
    pub transition_id: String,
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub old_registration_id: String,
    pub save_attempt_id: AttemptId,
    pub save_intent_id: String,
    pub save_span: TimeSpan,
    pub expected_sha256: String,
    pub expected_byte_length: u32,
    pub old_device_session_epoch: u64,
    pub expected_device_session_epoch: u64,
    pub old_source_namespace: String,
    /// Shared Host monotonic clock origin used for the Main reopen boundary.
    pub clock_domain: String,
    pub deadline_ms: u64,
}

impl fmt::Debug for OwnedRevisitChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedRevisitChallenge([private transition])")
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedRevisitFileVersion {
    pub dev: u64,
    pub ino: u64,
    pub size: u32,
    pub sha256: String,
    /// Decimal strings preserve Node BigInt nanosecond timestamps exactly.
    pub mtime_ns: String,
    pub ctime_ns: String,
}

impl fmt::Debug for OwnedRevisitFileVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedRevisitFileVersion([private file metadata])")
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedRevisitFileLineage {
    pub directory_dev: u64,
    pub directory_ino: u64,
    pub before_close: OwnedRevisitFileVersion,
    pub after_close: OwnedRevisitFileVersion,
    pub after_reopen: OwnedRevisitFileVersion,
}

impl fmt::Debug for OwnedRevisitFileLineage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedRevisitFileLineage([private file lineage])")
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedRevisitCompletion {
    pub version: u8,
    pub transition_id: String,
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub new_registration: OwnedDocumentRegistration,
    pub file_lineage: OwnedRevisitFileLineage,
    pub new_device_session_epoch: u64,
    pub reopen_boundary_ms: u64,
    pub clock_domain: String,
}

impl fmt::Debug for OwnedRevisitCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedRevisitCompletion([private transition])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnedRevisitStatus {
    AwaitingSourceProof,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedRevisitError {
    Unavailable,
    Invalid(String),
    Expired,
    Cancelled,
    Storage(String),
}

impl fmt::Display for OwnedRevisitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("owned revisit is unavailable"),
            Self::Invalid(reason) => write!(formatter, "owned revisit is invalid: {reason}"),
            Self::Expired => formatter.write_str("owned revisit challenge expired"),
            Self::Cancelled => formatter.write_str("owned revisit was cancelled"),
            Self::Storage(reason) => write!(formatter, "owned revisit storage error: {reason}"),
        }
    }
}

impl std::error::Error for OwnedRevisitError {}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnedSaveEvidence {
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub registration_id: String,
    pub attempt_id: AttemptId,
    pub intent_id: String,
    pub span: TimeSpan,
    pub expected_sha256: String,
    pub expected_byte_length: u32,
    pub device_session_epoch: u64,
    pub source_identity_boundary: SourceIdentityBoundary,
    pub source_clock_domain: String,
    pub release_status: ReleaseStatus,
    pub completed_ms: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnedRevisitPhase {
    PeerRestart,
    NewManifest,
    HostCompletion,
    SourceProof,
}

pub(super) struct OwnedRevisitProgress {
    pub challenge: OwnedRevisitChallenge,
    pub old_registration: OwnedDocumentRegistration,
    pub save_evidence: OwnedSaveEvidence,
    pub current_ui_anchor: EffectResult,
    pub phase: OwnedRevisitPhase,
    pub completion: Option<OwnedRevisitCompletion>,
    pub source_refresh_generation: Option<u64>,
    pub source_refresh_accepted: bool,
}

pub(super) struct OwnedRevisitStore;

impl OwnedRevisitStore {
    pub(super) fn record_save(
        storage: &mut Storage,
        evidence: &OwnedSaveEvidence,
    ) -> Result<(), StoreError> {
        let json = serde_json::to_string(evidence)?;
        storage.transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO owned_save_evidence
                 (root_id, attempt_id, task_revision, root_epoch, registration_id, intent_id,
                  span_from_ms, span_to_ms, expected_sha256, expected_byte_length,
                  device_session_epoch, source_namespace, clock_domain, completed_ms, json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    evidence.root_task_id.as_str(),
                    evidence.attempt_id.as_str(),
                    evidence.task_revision as i64,
                    evidence.root_epoch as i64,
                    evidence.registration_id,
                    evidence.intent_id,
                    evidence.span.from_ms as i64,
                    evidence.span.to_ms as i64,
                    evidence.expected_sha256,
                    evidence.expected_byte_length as i64,
                    evidence.device_session_epoch as i64,
                    evidence.source_identity_boundary.namespace,
                    evidence.source_clock_domain,
                    evidence.completed_ms as i64,
                    json,
                ],
            )?;
            Ok(())
        })
    }

    fn latest_save(
        storage: &Storage,
        root: &RootTaskId,
    ) -> Result<Option<OwnedSaveEvidence>, StoreError> {
        let json: Option<String> = storage
            .read()
            .query_row(
                "SELECT json FROM owned_save_evidence WHERE root_id = ?1
                 ORDER BY completed_ms DESC LIMIT 1",
                [root.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()
    }

    fn insert_challenge(
        storage: &mut Storage,
        challenge: &OwnedRevisitChallenge,
        save_attempt_id: &AttemptId,
        now_ms: u64,
    ) -> Result<(), StoreError> {
        let json = serde_json::to_string(challenge)?;
        storage.transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO owned_revisit_transitions
                 (transition_id, root_id, save_attempt_id, task_revision, root_epoch, state,
                  challenge_json, created_ms, deadline_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'awaiting_peer', ?6, ?7, ?8)",
                rusqlite::params![
                    challenge.transition_id,
                    challenge.root_task_id.as_str(),
                    save_attempt_id.as_str(),
                    i64::try_from(challenge.task_revision)
                        .map_err(|_| StoreError::Corrupt("task revision overflow".into()))?,
                    i64::try_from(challenge.root_epoch)
                        .map_err(|_| StoreError::Corrupt("root epoch overflow".into()))?,
                    json,
                    i64::try_from(now_ms)
                        .map_err(|_| StoreError::Corrupt("time overflow".into()))?,
                    i64::try_from(challenge.deadline_ms)
                        .map_err(|_| StoreError::Corrupt("deadline overflow".into()))?,
                ],
            )?;
            Ok(())
        })
    }

    fn set_completion(
        storage: &mut Storage,
        completion: &OwnedRevisitCompletion,
    ) -> Result<(), StoreError> {
        let json = serde_json::to_string(completion)?;
        storage.transaction::<_, StoreError>(|tx| {
            let changed = tx.execute(
                "UPDATE owned_revisit_transitions
                 SET state = 'awaiting_source_proof', completion_json = ?2
                 WHERE transition_id = ?1 AND state = 'awaiting_host_completion'",
                rusqlite::params![completion.transition_id, json],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict("owned revisit phase changed".into()));
            }
            Ok(())
        })
    }

    pub(super) fn set_phase(
        storage: &mut Storage,
        transition_id: &str,
        from: &str,
        to: &str,
    ) -> Result<(), StoreError> {
        storage.transaction::<_, StoreError>(|tx| {
            let changed = tx.execute(
                "UPDATE owned_revisit_transitions SET state = ?3
                 WHERE transition_id = ?1 AND state = ?2",
                rusqlite::params![transition_id, from, to],
            )?;
            if changed != 1 {
                return Err(StoreError::Conflict("owned revisit phase changed".into()));
            }
            Ok(())
        })
    }

    fn set_terminal(
        storage: &mut Storage,
        transition_id: &str,
        state: &str,
        failure: Option<&str>,
    ) -> Result<(), StoreError> {
        storage.transaction::<_, StoreError>(|tx| {
            tx.execute(
                "UPDATE owned_revisit_transitions SET state = ?2, failure = ?3
                 WHERE transition_id = ?1 AND state NOT IN ('completed','cancelled','expired','invalidated_restart')",
                rusqlite::params![transition_id, state, failure],
            )?;
            Ok(())
        })
    }

    pub(super) fn invalidate_after_restart(
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<(), StoreError> {
        storage.transaction::<_, StoreError>(|tx| {
            tx.execute(
                "UPDATE owned_revisit_transitions
                 SET state = 'invalidated_restart', failure = 'engine restart before proof'
                 WHERE state IN ('awaiting_peer','awaiting_new_manifest','awaiting_host_completion','awaiting_source_proof')",
                [],
            )?;
            tx.execute(
                "INSERT INTO event_log(at_ms, root_id, kind, detail)
                 SELECT ?1, root_id, 'owned_revisit_recovery_boundary', 'pending revisit invalidated on Engine restart'
                 FROM owned_revisit_transitions WHERE state = 'invalidated_restart' AND created_ms <= ?1",
                [now_ms as i64],
            )?;
            Ok(())
        })
    }
}

impl Engine {
    /// The one active trusted revisit challenge, for Host correlation only.
    pub fn owned_revisit_transition_id(&self) -> Option<&str> {
        self.owned_revisit
            .as_ref()
            .map(|progress| progress.challenge.transition_id.as_str())
    }

    /// Begin the private Main-managed restart after the Engine independently
    /// verifies its persisted Save receipt and earlier CurrentUi effect.
    pub fn begin_owned_revisit(
        &mut self,
        now_ms: u64,
    ) -> Result<OwnedRevisitChallenge, OwnedRevisitError> {
        self.now_ms = self.now_ms.max(now_ms);
        if self.owned_revisit.is_some() || !self.outputs.is_empty() {
            return Err(OwnedRevisitError::Unavailable);
        }
        let Some(active) = self.tasks.active() else {
            return Err(OwnedRevisitError::Unavailable);
        };
        let root_id = active.row.root_id.clone();
        let task_revision = active.row.task_revision;
        let root_epoch = active.row.root_epoch;
        let root_deadline = active.row.deadline_ms.unwrap_or(0);
        let active_state = active.row.state;
        let cancel_requested = active.row.cancel_requested;
        let plan = active.row.plan.clone();
        if cancel_requested
            || !matches!(
                active_state,
                RootState::Running | RootState::WaitingEvidence
            )
            || root_deadline <= self.now_ms
        {
            return Err(OwnedRevisitError::Unavailable);
        }
        let run = self.run.as_ref().ok_or(OwnedRevisitError::Unavailable)?;
        if run.goal.root_task_id != root_id
            || run.goal.task_revision != task_revision
            || run.grant.root_epoch != root_epoch
            || !owned_revisit_goal_supported(&run.goal)
            || run.attempt.is_some()
            || !run.reconcile.is_empty()
            || run.reconciling.is_some()
            || run.stop_wait_since.is_some()
            || run.registering
            || self.decisions.pending().is_some()
            || self.planning.inflight().is_some()
            || self.device.is_waiting(|_| true)
            || self.device.behind_on_stops()
            || !self.outputs.is_empty()
            || !matches!(
                self.stop_status,
                interaction_contracts::desktop::operator::StopStatus::NotStopped
                    | interaction_contracts::desktop::operator::StopStatus::StoppedConfirmed
            )
        {
            return Err(OwnedRevisitError::Unavailable);
        }

        let Some(registration) = self.cfg.owned_document_registration.as_deref().cloned() else {
            return Err(OwnedRevisitError::Unavailable);
        };
        let Some(identity) = self.device.identity() else {
            return Err(OwnedRevisitError::Unavailable);
        };
        let old_session = identity.session_epoch;
        let expected_session = old_session
            .checked_add(1)
            .ok_or_else(|| OwnedRevisitError::Invalid("device session overflow".into()))?;
        let save = OwnedRevisitStore::latest_save(&self.storage, &root_id)
            .map_err(store_error)?
            .ok_or(OwnedRevisitError::Unavailable)?;
        let ExpectedValue::Text { value } = &run.goal.desired else {
            return Err(OwnedRevisitError::Unavailable);
        };
        let expected_sha256 = format!("{:x}", Sha256::digest(value.as_bytes()));
        if save.task_revision != task_revision
            || save.root_epoch != root_epoch
            || save.registration_id != registration.registration_id
            || save.expected_sha256 != expected_sha256
            || save.expected_byte_length as usize != value.len()
            || save.device_session_epoch != old_session
            // A post-Save refresh intentionally retires the current binding.
            // This read-only lifecycle uses the proven Save's source identity;
            // fresh authority is required again only for the new UI read.
            || save.source_identity_boundary.validate().is_err()
            || save.source_clock_domain != identity.clock_domain
            || !matches!(
                save.release_status,
                ReleaseStatus::NotNeeded | ReleaseStatus::Released
            )
            || value.len() > 2048
            || value.contains('\0')
        {
            return Err(OwnedRevisitError::Unavailable);
        }
        let attempts =
            TaskStore::load_attempts(self.storage.read(), root_id.as_str()).map_err(store_error)?;
        let save_attempt = attempts
            .iter()
            .find(|attempt| attempt.attempt_id == save.attempt_id)
            .ok_or(OwnedRevisitError::Unavailable)?;
        if save_attempt.state != AttemptState::Settled
            || save_attempt.skill != "ui.save_document"
            || save_attempt.intent_id.as_deref() != Some(save.intent_id.as_str())
            || save_attempt.root_epoch != root_epoch
            || save_attempt.task_revision != task_revision
            || !run.done.contains(&save_attempt.subgoal)
            || !plan.as_ref().is_some_and(|plan| {
                plan.subgoals
                    .iter()
                    .any(|step| step.id == save_attempt.subgoal && step.skill == "ui.save_document")
            })
            || attempts.iter().any(|attempt| {
                matches!(
                    attempt.state,
                    AttemptState::Active | AttemptState::Dispatched | AttemptState::Unknown
                )
            })
        {
            return Err(OwnedRevisitError::Unavailable);
        }
        let current_ui_anchor = settled_current_ui_anchor(
            self.storage.read(),
            &root_id,
            &run.goal.target.semantic_target,
            &attempts,
            task_revision,
            root_epoch,
            save.span,
        )
        .map_err(store_error)?
        .ok_or(OwnedRevisitError::Unavailable)?;

        let deadline_ms = self.now_ms.saturating_add(30_000).min(root_deadline);
        if deadline_ms <= self.now_ms {
            return Err(OwnedRevisitError::Expired);
        }
        let transition_id = revisit_uuid(
            &root_id,
            task_revision,
            root_epoch,
            &save.attempt_id,
            &save.intent_id,
            save.span,
            self.now_ms,
        );
        let challenge = OwnedRevisitChallenge {
            version: 1,
            transition_id,
            root_task_id: root_id.clone(),
            task_revision,
            root_epoch,
            old_registration_id: registration.registration_id.clone(),
            save_attempt_id: save.attempt_id.clone(),
            save_intent_id: save.intent_id.clone(),
            save_span: save.span,
            expected_sha256,
            expected_byte_length: value.len() as u32,
            old_device_session_epoch: old_session,
            expected_device_session_epoch: expected_session,
            old_source_namespace: save.source_identity_boundary.namespace.clone(),
            clock_domain: identity.clock_domain.clone(),
            deadline_ms,
        };
        // Supersede a harmless periodic Source wait so its late callback/read
        // cannot become the reopened-document witness.
        if self.state.refresh_pending() {
            let generation = self
                .state
                .begin_refresh()
                .ok_or_else(|| OwnedRevisitError::Invalid("refresh generation exhausted".into()))?;
            if !self.state.refresh_started(generation) {
                return Err(OwnedRevisitError::Unavailable);
            }
        }
        OwnedRevisitStore::insert_challenge(
            &mut self.storage,
            &challenge,
            &save.attempt_id,
            self.now_ms,
        )
        .map_err(store_error)?;

        if let Some(cancel) = self
            .decisions
            .close(
                &mut self.storage,
                crate::ports::DecisionLifecycle::Superseded,
            )
            .map_err(|error| OwnedRevisitError::Storage(error.to_string()))?
        {
            self.emit(Output::CancelDecision(cancel));
        }
        self.planning.cancel();
        self.bindings
            .invalidate_all("owned document revisit begins");
        self.state.clear_owned_document_binding();
        self.source_gate_closed = true;
        let Some(run) = self.run.as_mut() else {
            return Err(OwnedRevisitError::Unavailable);
        };
        run.effect_verification_only = true;
        run.wait = None;
        self.owned_revisit = Some(OwnedRevisitProgress {
            challenge: challenge.clone(),
            old_registration: registration.clone(),
            save_evidence: save,
            current_ui_anchor,
            phase: OwnedRevisitPhase::PeerRestart,
            completion: None,
            source_refresh_generation: None,
            source_refresh_accepted: false,
        });
        Ok(challenge)
    }

    /// Install a Main-verified reopen lineage. Completion of the root is
    /// deferred until a fresh owned Source snapshot arrives after the refresh
    /// handshake; this method never grants input.
    pub fn finish_owned_revisit(
        &mut self,
        completion: OwnedRevisitCompletion,
        now_ms: u64,
    ) -> Result<OwnedRevisitStatus, OwnedRevisitError> {
        self.now_ms = self.now_ms.max(now_ms);
        let Some(progress) = self.owned_revisit.as_ref() else {
            return Err(OwnedRevisitError::Unavailable);
        };
        let challenge = progress.challenge.clone();
        let old_registration = progress.old_registration.clone();
        let save_completed_ms = progress.save_evidence.completed_ms;
        let phase = progress.phase;
        if phase != OwnedRevisitPhase::HostCompletion || self.now_ms >= challenge.deadline_ms {
            return Err(OwnedRevisitError::Expired);
        }
        if completion.version != 1
            || completion.transition_id != challenge.transition_id
            || completion.root_task_id != challenge.root_task_id
            || completion.task_revision != challenge.task_revision
            || completion.root_epoch != challenge.root_epoch
            || completion.new_device_session_epoch != challenge.expected_device_session_epoch
            || completion.clock_domain != challenge.clock_domain
            || completion.reopen_boundary_ms <= save_completed_ms
            || completion.reopen_boundary_ms > self.now_ms
            || self.device.session_epoch() != Some(challenge.expected_device_session_epoch)
            || !valid_file_lineage(&old_registration, &completion, &challenge)
        {
            self.cancel_owned_revisit(
                &challenge.transition_id,
                self.now_ms,
                "Main completion failed identity, file, or clock validation",
            )?;
            return Err(OwnedRevisitError::Invalid(
                "completion proof did not match".into(),
            ));
        }
        let Some(manifest) = self.device.manifest().cloned() else {
            return Err(OwnedRevisitError::Unavailable);
        };
        if manifest.identity.session_epoch != challenge.expected_device_session_epoch
            || manifest.input == CapabilityState::Available
            || completion.new_registration.registration_id == challenge.old_registration_id
            || completion.new_registration.process_identity() == old_registration.process_identity()
            || completion.new_registration.validate_identity().is_err()
        {
            self.cancel_owned_revisit(
                &challenge.transition_id,
                self.now_ms,
                "new registration or peer is not read-only and distinct",
            )?;
            return Err(OwnedRevisitError::Invalid(
                "new registration is unavailable".into(),
            ));
        }
        let Some(run) = self.run.as_ref() else {
            return Err(OwnedRevisitError::Unavailable);
        };
        let goal_app = run.goal.scope.app.clone();
        if completion.new_registration.bundle_id != goal_app {
            self.cancel_owned_revisit(
                &challenge.transition_id,
                self.now_ms,
                "new registration targets another application",
            )?;
            return Err(OwnedRevisitError::Invalid(
                "registration app mismatch".into(),
            ));
        }
        OwnedRevisitStore::set_completion(&mut self.storage, &completion).map_err(store_error)?;
        self.cfg.owned_document_registration = Some(Box::new(completion.new_registration.clone()));
        // The authorization was bound to the old registration. A new read-only
        // peer may prove the retained root, but it cannot inherit Save authority.
        self.cfg.allow_owned_document_save = false;
        self.state.install_owned_document_registration(
            Some(Box::new(completion.new_registration.clone())),
            self.cfg.evidence_max_age_ms,
        );
        self.state.clear_owned_document_binding();
        self.source_gate_closed = true;
        let progress = self.owned_revisit.as_mut().expect("checked above");
        progress.phase = OwnedRevisitPhase::SourceProof;
        progress.completion = Some(completion);
        progress.source_refresh_accepted = false;
        if let Some(run) = self.run.as_mut() {
            run.wait = None;
            run.effect_verification_only = true;
        }
        let _ = self.request_refresh(self.now_ms, "verifying the owned document after reopen");
        let generation = self.outputs.iter().rev().find_map(|output| match output {
            Output::Source { generation, .. } => Some(*generation),
            _ => None,
        });
        if let Some(progress) = self.owned_revisit.as_mut() {
            progress.source_refresh_generation = generation;
        }
        Ok(OwnedRevisitStatus::AwaitingSourceProof)
    }

    pub fn cancel_owned_revisit(
        &mut self,
        transition_id: &str,
        now_ms: u64,
        reason: &str,
    ) -> Result<(), OwnedRevisitError> {
        let Some(progress) = self.owned_revisit.as_ref() else {
            return Err(OwnedRevisitError::Unavailable);
        };
        if progress.challenge.transition_id != transition_id {
            return Err(OwnedRevisitError::Invalid("transition id mismatch".into()));
        }
        let state = if now_ms >= progress.challenge.deadline_ms {
            "expired"
        } else {
            "cancelled"
        };
        OwnedRevisitStore::set_terminal(&mut self.storage, transition_id, state, Some(reason))
            .map_err(store_error)?;
        self.state.clear_owned_document_binding();
        self.owned_revisit = None;
        if let Some(run) = self.run.as_mut() {
            run.effect_verification_only = true;
            run.wait = None;
        }
        Ok(())
    }
}

fn store_error(error: StoreError) -> OwnedRevisitError {
    OwnedRevisitError::Storage(error.to_string())
}

fn settled_current_ui_anchor(
    conn: &rusqlite::Connection,
    root: &RootTaskId,
    target: &interaction_contracts::desktop::ids::SemanticTargetRef,
    attempts: &[AttemptRow],
    task_revision: u64,
    root_epoch: u64,
    save_span: TimeSpan,
) -> Result<Option<EffectResult>, StoreError> {
    let history = VerificationStore::history(conn, root.as_str())?;
    Ok(history.into_iter().rev().find(|effect| {
        if effect.root_task_id != *root
            || effect.predicate != "text.value@1"
            || effect.status != EffectStatus::Satisfied
            || effect.confirmation_scope != ConfirmationScope::CurrentUi
            || effect.target.as_ref() != Some(target)
            || effect.evidence_group.is_empty()
        {
            return false;
        }
        let Some(attempt_id) = effect.attempt_id.as_ref() else {
            return false;
        };
        let Some(intent_id) = effect.intent_id.as_deref() else {
            return false;
        };
        attempts.iter().any(|attempt| {
            attempt.attempt_id == *attempt_id
                && attempt.intent_id.as_deref() == Some(intent_id)
                && attempt.skill == "ui.enter_text"
                && attempt.state == AttemptState::Settled
                && attempt.root_epoch == root_epoch
                && attempt.task_revision == task_revision
        }) && effect.input_span.is_some_and(|span| {
            span.from_ms <= span.to_ms
                && span.to_ms <= effect.observed_ms
                && effect.observed_ms < save_span.from_ms
                && span.to_ms < save_span.from_ms
        })
    }))
}

fn revisit_uuid(
    root: &RootTaskId,
    task_revision: u64,
    root_epoch: u64,
    attempt: &AttemptId,
    intent: &str,
    span: TimeSpan,
    now_ms: u64,
) -> String {
    let digest = Sha256::digest(
        format!(
            "{root}\n{task_revision}\n{root_epoch}\n{attempt}\n{intent}\n{}\n{}\n{now_ms}",
            span.from_ms, span.to_ms
        )
        .as_bytes(),
    );
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("sha256 prefix length");
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

pub(super) fn valid_file_lineage(
    old: &OwnedDocumentRegistration,
    completion: &OwnedRevisitCompletion,
    challenge: &OwnedRevisitChallenge,
) -> bool {
    let lineage = &completion.file_lineage;
    let first = &lineage.before_close;
    let closed = &lineage.after_close;
    let reopened = &lineage.after_reopen;
    if lineage.directory_dev != old.directory_dev
        || lineage.directory_ino != old.directory_ino
        || lineage.directory_dev != completion.new_registration.directory_dev
        || lineage.directory_ino != completion.new_registration.directory_ino
        || old.document_path != completion.new_registration.document_path
        || completion.new_registration.document_sha256 != challenge.expected_sha256
        || first.dev != closed.dev
        || first.ino != closed.ino
        || first.size != closed.size
        || first.sha256 != closed.sha256
        || first.mtime_ns != closed.mtime_ns
        || first.ctime_ns != closed.ctime_ns
        || first.dev != reopened.dev
        || first.ino != reopened.ino
        || first.size != reopened.size
        || first.sha256 != reopened.sha256
        || first.mtime_ns != reopened.mtime_ns
        || first.ctime_ns != reopened.ctime_ns
        || reopened.dev != completion.new_registration.document_dev
        || reopened.ino != completion.new_registration.document_ino
        || reopened.size != challenge.expected_byte_length
        || reopened.sha256 != challenge.expected_sha256
        || reopened.size > 2048
    {
        return false;
    }
    [
        &first.mtime_ns,
        &first.ctime_ns,
        &closed.mtime_ns,
        &closed.ctime_ns,
        &reopened.mtime_ns,
        &reopened.ctime_ns,
    ]
    .into_iter()
    .all(|value| {
        !value.is_empty()
            && value.len() <= 39
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.parse::<u128>().is_ok()
    })
}

impl Engine {
    /// Advance the verification-only retained root. This never invokes a
    /// decision, candidate, permit, or input path.
    pub(super) fn advance_owned_revisit(&mut self) -> Step {
        let Some(progress) = self.owned_revisit.as_ref() else {
            return Ok(false);
        };
        if self.now_ms >= progress.challenge.deadline_ms {
            let transition_id = progress.challenge.transition_id.clone();
            self.cancel_owned_revisit(&transition_id, self.now_ms, "challenge expired")
                .map_err(|error| EngineError::Store(StoreError::Corrupt(error.to_string())))?;
            self.abandon_work()?;
            if self.tasks.active_state() == Some(RootState::WaitingEvidence) {
                self.tasks
                    .evidence_arrived(&mut self.storage, self.now_ms)?;
            }
            self.tasks.input_ended_unverified(
                &mut self.storage,
                self.now_ms,
                "the independent owned revisit evidence did not arrive before its deadline",
            )?;
            return Ok(true);
        }
        if progress.phase != OwnedRevisitPhase::SourceProof
            || !progress.source_refresh_accepted
            || self.state.refresh_pending()
        {
            return Ok(false);
        }
        let challenge = progress.challenge.clone();
        let Some(completion) = progress.completion.clone() else {
            return Ok(false);
        };
        let Some(run) = self.run.as_ref() else {
            self.cancel_owned_revisit(
                &challenge.transition_id,
                self.now_ms,
                "frozen root disappeared",
            )
            .map_err(|error| EngineError::Store(StoreError::Corrupt(error.to_string())))?;
            return Ok(true);
        };
        if run.goal.root_task_id != challenge.root_task_id
            || run.goal.task_revision != challenge.task_revision
            || run.grant.root_epoch != challenge.root_epoch
        {
            self.cancel_owned_revisit(&challenge.transition_id, self.now_ms, "frozen root changed")
                .map_err(|error| EngineError::Store(StoreError::Corrupt(error.to_string())))?;
            return Ok(true);
        }
        let Some(proof) = self.owned_document_proof_for(&run.goal.target.semantic_target) else {
            return self.request_owned_revisit_refresh();
        };
        let Some(readback) = proof.receiver.value_readback.as_ref() else {
            return self.request_owned_revisit_refresh();
        };
        if proof.session_epoch != completion.new_device_session_epoch
            || proof.receiver.registration_id != completion.new_registration.registration_id
            || proof.receiver.process != completion.new_registration.process_identity()
        {
            self.cancel_owned_revisit(
                &challenge.transition_id,
                self.now_ms,
                "new AX proof does not match the retained registration",
            )
            .map_err(|error| EngineError::Store(StoreError::Corrupt(error.to_string())))?;
            return Ok(true);
        }
        if proof.receiver.provenance.identity_boundary.namespace == challenge.old_source_namespace {
            self.cancel_owned_revisit(
                &challenge.transition_id,
                self.now_ms,
                "Source identity namespace did not change on reopen",
            )
            .map_err(|error| EngineError::Store(StoreError::Corrupt(error.to_string())))?;
            return Ok(true);
        }
        if readback.observed_ms < proof.receiver.provenance.observed_ms {
            self.cancel_owned_revisit(
                &challenge.transition_id,
                self.now_ms,
                "AX readback predates its admitted Source read",
            )
            .map_err(|error| EngineError::Store(StoreError::Corrupt(error.to_string())))?;
            return Ok(true);
        }
        let bytes = challenge.expected_byte_length as usize;
        if readback.utf8_byte_count as usize != bytes
            || readback.sha256 != challenge.expected_sha256
        {
            self.cancel_owned_revisit(
                &challenge.transition_id,
                self.now_ms,
                "reopened AX value does not match saved file lineage",
            )
            .map_err(|error| EngineError::Store(StoreError::Corrupt(error.to_string())))?;
            return Ok(true);
        }
        let current_ui_anchor = progress.current_ui_anchor.clone();
        let _verified_effect = conclude_owned_revisit(
            &mut self.storage,
            OwnedRevisitConclusion {
                goal: &run.goal,
                registration: &completion.new_registration,
                proof: &proof,
                current_ui_anchor: &current_ui_anchor,
                transition_id: &challenge.transition_id,
                save_attempt_id: &challenge.save_attempt_id,
                save_intent_id: &challenge.save_intent_id,
                save_span: challenge.save_span,
                expected_sha256: &challenge.expected_sha256,
                expected_byte_length: challenge.expected_byte_length,
                old_source_namespace: &challenge.old_source_namespace,
                main_file_sha256: &completion.file_lineage.after_reopen.sha256,
                main_file_byte_length: completion.file_lineage.after_reopen.size,
                now_ms: self.now_ms,
                max_age_ms: self.cfg.evidence_max_age_ms,
            },
        )?;
        OwnedRevisitStore::set_terminal(
            &mut self.storage,
            &challenge.transition_id,
            "completed",
            None,
        )?;
        self.tasks
            .predicates_satisfied(&mut self.storage, self.now_ms)?;
        self.owned_revisit = None;
        self.note("the retained owned document was independently saved and revisited");
        Ok(true)
    }

    fn request_owned_revisit_refresh(&mut self) -> Step {
        if self.state.refresh_pending() || self.run.as_ref().is_some_and(|run| run.wait.is_some()) {
            return Ok(false);
        }
        if let Some(progress) = self.owned_revisit.as_mut() {
            progress.source_refresh_accepted = false;
            progress.source_refresh_generation = None;
        }
        let _ = self.request_refresh(
            self.now_ms,
            "waiting for a fresh owned AX value after reopen",
        )?;
        let generation = self.outputs.iter().rev().find_map(|output| match output {
            Output::Source { generation, .. } => Some(*generation),
            _ => None,
        });
        let Some(generation) = generation else {
            return Ok(false);
        };
        if let Some(progress) = self.owned_revisit.as_mut() {
            progress.source_refresh_generation = Some(generation);
        }
        Ok(true)
    }
}
