//! The device's side of the conversation: replies, unsolicited notices, stops,
//! resuming and the recovery after a restart (design 14.8, 16.6 and 20.2).
//!
//! A stop reaches the device directly from the operator, so the host may learn
//! of it from the device's acknowledgement before it has heard from anyone
//! else. The host acts on the stop first and only then acknowledges it, so the
//! device never sees an acknowledgement for a stop the task runtime has not
//! handled.

use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, DeviceEventKind, Receipt, ReceiptStatus,
    ReleaseStatus, RootGrant, StopAck, StopDisposition, StopScope, StopSource,
};
use interaction_contracts::desktop::ids::AttemptId;
use interaction_contracts::desktop::operator::{RunMode, StopStatus};
use interaction_contracts::desktop::task::{AttemptState, RootState};

use super::owned_revisit::{OwnedRevisitPhase, OwnedRevisitStore};
use super::{Engine, EngineError, Step};
use crate::execution::{DeviceCommand, DeviceNotice, DeviceReply, Expect, Stage};
use crate::reaction::store::ReactionStore;
use crate::tasks::StopDirective;
use crate::verification::{Verification, VerificationStore};

/// How long a stop that the host has not caught up with may hold the loop
/// before it says so.
const STOP_WAIT_NOTE_MS: u64 = 5_000;
const RECONCILIATION_RETRIES: u32 = 1;

fn settled(cleanup: ReleaseStatus) -> bool {
    matches!(cleanup, ReleaseStatus::Released | ReleaseStatus::NotNeeded)
}

fn reply_matches(expect: &Expect, reply: &DeviceReply) -> bool {
    match expect {
        Expect::Register | Expect::Lease | Expect::Revoke => {
            matches!(reply, DeviceReply::Ack(_))
        }
        Expect::Observe { .. } => matches!(reply, DeviceReply::Ack(_)),
        Expect::Grant { .. } => matches!(reply, DeviceReply::Grant(_)),
        Expect::Submit { .. } | Expect::Query { .. } => {
            matches!(reply, DeviceReply::Receipt(_))
        }
        Expect::Dependency => matches!(reply, DeviceReply::Dependencies(_)),
        Expect::Stop => matches!(reply, DeviceReply::Stopped(_)),
        Expect::Resume => matches!(reply, DeviceReply::Resumed(_)),
    }
}

impl Engine {
    // ---- Replies -----------------------------------------------------------------------------

    pub(super) fn on_device_reply(
        &mut self,
        corr: u64,
        reply: DeviceReply,
    ) -> Result<(), EngineError> {
        let Some(expect) = self.device.expected_reply(corr) else {
            if let DeviceReply::Grant(
                interaction_contracts::desktop::device::GrantOutcome::Granted { permit },
            ) = &reply
            {
                self.revoke_unmatched_grant(permit, "permit arrived for an unknown correlation");
            }
            self.note("a reply to an unknown or timed-out command was ignored");
            return Ok(());
        };
        if !reply_matches(expect, &reply) {
            let expected = format!("{expect:?}");
            let received = format!("{reply:?}");
            if let DeviceReply::Grant(
                interaction_contracts::desktop::device::GrantOutcome::Granted { permit },
            ) = &reply
            {
                self.revoke_unmatched_grant(
                    permit,
                    "permit arrived with a reply type that did not match its correlation",
                );
            }
            self.note(format!("unexpected reply {received} to {expected}"));
            return Ok(());
        }
        // The expectation remains in DeviceView until the reply variant has
        // been checked. A malformed response must not consume a late-Grant
        // tombstone that is needed to revoke the real permit response.
        let expect = self
            .device
            .reply(corr)
            .expect("a validated reply retains its correlation expectation");
        match (expect, reply) {
            (Expect::Register, DeviceReply::Ack(ack)) => {
                if let Some(run) = self.run.as_mut() {
                    run.registering = false;
                    run.registered = ack.accepted;
                }
                if ack.accepted {
                    self.note("the device registered the task");
                    Ok(())
                } else {
                    let reason = ack.reason.unwrap_or_else(|| "no reason given".into());
                    self.fail_root(&format!("the device refused the task: {reason}"))
                }
            }
            (
                Expect::Observe {
                    attempt,
                    observation_id,
                },
                DeviceReply::Ack(ack),
            ) => self.observed(&attempt, &observation_id, corr, ack),
            (
                Expect::Grant {
                    attempt,
                    observation_id,
                },
                DeviceReply::Grant(outcome),
            ) => self.granted(&attempt, &observation_id, corr, outcome),
            (Expect::Submit { .. }, DeviceReply::Receipt(Some(receipt))) => {
                self.on_receipt(*receipt)
            }
            (Expect::Submit { intent_id }, DeviceReply::Receipt(None)) => {
                self.note(format!("the device has no receipt for {intent_id}"));
                Ok(())
            }
            (Expect::Query { intent_id }, DeviceReply::Receipt(receipt)) => {
                self.reconciled(&intent_id, receipt.map(|boxed| *boxed))
            }
            (Expect::Dependency, DeviceReply::Dependencies(versions)) => {
                self.device.replace_dependencies(versions.versions);
                Ok(())
            }
            (Expect::Stop, DeviceReply::Stopped(ack)) => self.on_stop_ack(ack),
            (Expect::Resume, DeviceReply::Resumed(ack)) => self.on_resume_ack(ack),
            (Expect::Lease, DeviceReply::Ack(ack)) => {
                self.device.note_lease(ack.accepted);
                if !ack.accepted {
                    self.note(format!(
                        "the device refused the control lease: {}",
                        ack.reason.unwrap_or_default()
                    ));
                }
                Ok(())
            }
            (Expect::Revoke, DeviceReply::Ack(_)) => Ok(()),
            _ => unreachable!("reply type was validated before consuming its correlation"),
        }
    }

    fn revoke_unmatched_grant(
        &mut self,
        permit: &interaction_contracts::desktop::device::PermitRef,
        reason: &str,
    ) {
        let Some(session_epoch) = self.device.session_epoch() else {
            self.note("an unmatched permit arrived after the device session was lost");
            return;
        };
        self.send(
            Expect::Revoke,
            DeviceCommand::Revoke(interaction_contracts::desktop::device::RevokeExecution {
                session_epoch,
                permit: permit.clone(),
                reason: reason.into(),
            }),
        );
    }

    // ---- Notices ------------------------------------------------------------------------------------

    pub(super) fn on_device_notice(&mut self, notice: DeviceNotice) -> Result<(), EngineError> {
        match notice {
            DeviceNotice::Manifest(manifest) => self.on_manifest(*manifest),
            DeviceNotice::Receipt(receipt) => self.on_receipt(*receipt),
            DeviceNotice::Event(event) => self.on_device_event(event.event),
        }
    }

    fn on_manifest(&mut self, manifest: CapabilityManifest) -> Result<(), EngineError> {
        let epoch = manifest.identity.session_epoch;
        if let Some(progress) = self.owned_revisit.as_ref() {
            let expected = epoch == progress.challenge.expected_device_session_epoch
                && self.now_ms < progress.challenge.deadline_ms
                && manifest.backend == BackendKind::Macos
                && manifest.input == CapabilityState::Unavailable
                && manifest.max_uninterruptible_ms == 0
                && self.tasks.active().is_some_and(|active| {
                    active.row.root_id == progress.challenge.root_task_id
                        && active.row.task_revision == progress.challenge.task_revision
                        && active.row.root_epoch == progress.challenge.root_epoch
                        && !active.row.cancel_requested
                });
            if expected {
                let first_manifest = matches!(
                    progress.phase,
                    OwnedRevisitPhase::PeerRestart | OwnedRevisitPhase::NewManifest
                );
                if first_manifest {
                    OwnedRevisitStore::set_phase(
                        &mut self.storage,
                        &progress.challenge.transition_id,
                        if progress.phase == OwnedRevisitPhase::PeerRestart {
                            "awaiting_peer"
                        } else {
                            "awaiting_new_manifest"
                        },
                        "awaiting_host_completion",
                    )?;
                }
                self.state.set_identity(&manifest.identity, true);
                self.device.install_manifest(manifest);
                if first_manifest {
                    self.window_context = None;
                    self.bindings
                        .invalidate_all("the owned revisit installed an observing device");
                    self.state.clear_owned_document_binding();
                    self.source_gate_closed = true;
                    self.last_lease_ms = None;
                    if let Some(run) = self.run.as_mut() {
                        run.registered = false;
                        run.registering = false;
                        run.wait = None;
                        run.effect_verification_only = true;
                    }
                    if let Some(progress) = self.owned_revisit.as_mut() {
                        progress.phase = OwnedRevisitPhase::HostCompletion;
                    }
                    self.note("the owned revisit installed a new observing device session");
                }
                return Ok(());
            }
            let transition = progress.challenge.transition_id.clone();
            self.cancel_owned_revisit(
                &transition,
                self.now_ms,
                "unexpected device manifest during revisit",
            )
            .map_err(|error| {
                EngineError::Store(crate::infrastructure::storage::StoreError::Conflict(
                    error.to_string(),
                ))
            })?;
        }
        let previous = self.device.session_epoch();
        let previous_domain = self
            .device
            .manifest()
            .map(|previous| previous.execution_domain);
        let domain_changed =
            previous_domain.is_some_and(|domain| domain != manifest.execution_domain);
        let require_frames = manifest.backend == BackendKind::Macos
            && manifest.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative;
        let identity_changed = self.state.set_identity(&manifest.identity, require_frames);
        if domain_changed {
            self.state.require_fresh_source_after(self.now_ms);
        }
        self.device.install_manifest(manifest);
        if identity_changed || domain_changed {
            self.window_context = None;
        }
        if domain_changed && previous.is_some() {
            self.invalidate_source_domain("the trusted execution domain changed")?;
        } else if identity_changed && previous.is_some() {
            self.invalidate_source_evidence("the observation device context changed")?;
        }
        if previous == Some(epoch) {
            return Ok(());
        }
        // A device refuses everything until it holds the host's control lease,
        // so it is asked for at once rather than on the next tick.
        self.last_lease_ms = None;
        self.renew_lease();
        self.note(format!("the device introduced itself (session {epoch})"));
        if let Some(run) = self.run.as_mut() {
            // Nothing registered with the old session exists in the new one.
            run.registered = false;
            run.registering = false;
        }
        if previous.is_some() && self.run.is_some() {
            // A device that came back does not know what it was doing. Whatever
            // was in flight is unknown, and the person decides about going on.
            self.pause_root("the device restarted")?;
        }
        if self.tasks.active_state() == Some(RootState::Cancelling)
            && let Some(root) = self.tasks.active().map(|a| a.row.root_id.clone())
        {
            // The cancellation could not be confirmed by the device that died.
            self.send_host_stop(root.as_str(), "finishing a cancellation on the new device");
        }
        Ok(())
    }

    /// The device connection ended. The root cannot wait for a release that
    /// will never be confirmed: it is suspended with its cleanup unknown, and
    /// going on needs the operator, a new device session and a fresh look.
    pub(super) fn on_device_lost(&mut self, reason: &str) -> Result<(), EngineError> {
        if let Some(progress) = self.owned_revisit.as_ref() {
            let planned = progress.phase == OwnedRevisitPhase::PeerRestart
                && self.now_ms < progress.challenge.deadline_ms
                && self.device.session_epoch() == Some(progress.challenge.old_device_session_epoch)
                && self.tasks.active().is_some_and(|active| {
                    active.row.root_id == progress.challenge.root_task_id
                        && active.row.task_revision == progress.challenge.task_revision
                        && active.row.root_epoch == progress.challenge.root_epoch
                        && !active.row.cancel_requested
                });
            if planned {
                OwnedRevisitStore::set_phase(
                    &mut self.storage,
                    &progress.challenge.transition_id,
                    "awaiting_peer",
                    "awaiting_new_manifest",
                )?;
                self.device.forget();
                self.state.clear_owned_document_binding();
                self.source_gate_closed = true;
                self.last_lease_ms = None;
                if let Some(run) = self.run.as_mut() {
                    run.registered = false;
                    run.registering = false;
                    run.wait = None;
                    run.effect_verification_only = true;
                }
                if let Some(progress) = self.owned_revisit.as_mut() {
                    progress.phase = OwnedRevisitPhase::NewManifest;
                }
                self.note("the owned revisit retired the settled input device");
                return Ok(());
            }
            let transition = progress.challenge.transition_id.clone();
            self.cancel_owned_revisit(
                &transition,
                self.now_ms,
                "unexpected device loss during revisit",
            )
            .map_err(|error| {
                EngineError::Store(crate::infrastructure::storage::StoreError::Conflict(
                    error.to_string(),
                ))
            })?;
        }
        self.note(format!("the device is gone: {reason}"));
        let session = self.device.session_epoch();
        self.device.forget();
        self.cleanup = ReleaseStatus::Unknown;
        if let Some(run) = self.run.as_mut() {
            run.registered = false;
            run.registering = false;
            run.stop_wait_since = None;
            run.paused_session = session;
        }
        let Some(state) = self.tasks.active_state() else {
            return Ok(());
        };
        match state {
            RootState::Running
            | RootState::WaitingEvidence
            | RootState::WaitingPlan
            | RootState::NeedsInput => {
                self.tasks.pause_requested(&mut self.storage, self.now_ms)?;
                self.abandon_work()?;
                self.tasks
                    .suspend_unconfirmed(&mut self.storage, self.now_ms)?;
                self.stop_status = StopStatus::StopUnconfirmed;
            }
            RootState::Pausing => {
                self.tasks
                    .suspend_unconfirmed(&mut self.storage, self.now_ms)?;
                self.stop_status = StopStatus::StopUnconfirmed;
            }
            // A cancellation in progress stays that way until a device can
            // confirm it let go.
            RootState::Cancelling => self.stop_status = StopStatus::StopUnconfirmed,
            _ => {}
        }
        Ok(())
    }

    fn on_device_event(&mut self, event: DeviceEventKind) -> Result<(), EngineError> {
        match event {
            DeviceEventKind::Stopped { ack } => self.on_stop_ack(ack),
            DeviceEventKind::Resumed { .. } => Ok(()),
            DeviceEventKind::HumanInput => {
                self.note("a person touched the keyboard or pointer");
                Ok(())
            }
            DeviceEventKind::DependencyChanged { name, revision, .. } => {
                self.device.note_dependency(&name, revision);
                Ok(())
            }
            DeviceEventKind::PermitRevoked { permit, reason } => {
                self.note(format!("permit {} was revoked: {reason}", permit.id));
                Ok(())
            }
            DeviceEventKind::ChannelIsolated { reason } => {
                self.note(format!("the input channel is isolated: {reason}"));
                self.stop_status = StopStatus::StopUnconfirmed;
                self.pause_root("the input channel is isolated")
            }
            DeviceEventKind::ChannelRecovered => {
                self.note("the input channel recovered");
                Ok(())
            }
            DeviceEventKind::CapabilityChanged { manifest } => self.on_manifest(manifest),
            DeviceEventKind::LeaseLost => {
                self.note("the control lease lapsed; renewing it");
                self.device.note_lease(false);
                self.last_lease_ms = None;
                self.renew_lease();
                Ok(())
            }
        }
    }

    // ---- Stops ------------------------------------------------------------------------------------------

    /// The device stopped something. The task runtime follows, and only then
    /// does the host acknowledge the stop.
    pub(super) fn on_stop_ack(&mut self, ack: StopAck) -> Result<(), EngineError> {
        self.device.stop_seen(ack.stop_seq);
        let active = self
            .tasks
            .active()
            .map(|active| (active.row.root_id.clone(), active.row.state));
        if let Some((root, state)) = active {
            let ours = match &ack.scope {
                StopScope::All => true,
                StopScope::Root { root_id } => root_id == root.as_str(),
            };
            if ours {
                match ack.disposition {
                    StopDisposition::Cancel => self.follow_cancel(&ack, state)?,
                    StopDisposition::Pause => self.follow_pause(&ack, state)?,
                }
            }
        }
        if ack.source == StopSource::HumanInput {
            self.note("a person took over; the task is paused until they say otherwise");
        }
        // The stop is handled, so the host may say it has caught up.
        self.device.stop_processed(ack.stop_seq);
        if let Some(run) = self.run.as_mut() {
            run.stop_wait_since = None;
        }
        Ok(())
    }

    fn follow_cancel(&mut self, ack: &StopAck, state: RootState) -> Result<(), EngineError> {
        if state != RootState::Cancelling {
            let directive = self.tasks.stop_requested(&mut self.storage, self.now_ms)?;
            self.abandon_work()?;
            self.note("the device cancelled the task");
            if directive == StopDirective::Finished {
                self.stop_status = StopStatus::StoppedConfirmed;
                return Ok(());
            }
        }
        if self.tasks.active_state().is_some() {
            self.tasks
                .inputs_released(&mut self.storage, self.now_ms, ack.cleanup)?;
        }
        self.cleanup = ack.cleanup;
        self.stop_status = if settled(ack.cleanup) {
            StopStatus::StoppedConfirmed
        } else {
            // The task is cancelled but the device could not confirm it let go.
            StopStatus::StopUnconfirmed
        };
        Ok(())
    }

    fn follow_pause(&mut self, ack: &StopAck, state: RootState) -> Result<(), EngineError> {
        if matches!(
            state,
            RootState::Running
                | RootState::WaitingEvidence
                | RootState::WaitingPlan
                | RootState::NeedsInput
        ) {
            self.tasks.pause_requested(&mut self.storage, self.now_ms)?;
            self.abandon_work()?;
        }
        let session = self.device.session_epoch();
        if let Some(run) = self.run.as_mut() {
            run.paused_session = session;
        }
        if self.tasks.active_state() == Some(RootState::Pausing) {
            self.tasks
                .inputs_released(&mut self.storage, self.now_ms, ack.cleanup)?;
        }
        self.cleanup = ack.cleanup;
        self.stop_status = if settled(ack.cleanup) {
            StopStatus::StoppedConfirmed
        } else {
            StopStatus::StopUnconfirmed
        };
        Ok(())
    }

    pub(super) fn on_resume_ack(
        &mut self,
        ack: interaction_contracts::desktop::device::ResumeAck,
    ) -> Result<(), EngineError> {
        // A device that restarted holds no pause, so there is nothing for it to
        // lift. The operator's resume is the authorisation, and the root is
        // revalidated against the new session before it goes on.
        let new_device = self
            .run
            .as_ref()
            .is_some_and(|run| run.paused_session != self.device.session_epoch());
        if !ack.resumed && !new_device {
            self.note(format!(
                "the device did not resume the task: {}",
                ack.reason.unwrap_or_default()
            ));
            return Ok(());
        }
        if self.tasks.active_state() == Some(RootState::Suspended) {
            self.tasks
                .resume_requested(&mut self.storage, self.now_ms)?;
            self.clear_confirmed_stop();
            if let Some(run) = self.run.as_mut() {
                run.revalidate_floor = None;
            }
            self.note("the task will be revalidated before it goes on");
        }
        Ok(())
    }

    // ---- Timeouts -----------------------------------------------------------------------------------------

    pub(super) fn check_device_timeouts(&mut self) -> Result<(), EngineError> {
        let late = self
            .device
            .timed_out(self.now_ms, self.cfg.command_timeout_ms);
        for (corr, expect) in late {
            match expect {
                Expect::Submit { intent_id } => {
                    // Whether the input happened is not known. It is checked,
                    // never repeated on a guess.
                    self.note(format!("no receipt for {intent_id}; asking the device"));
                    if let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut())
                        && attempt
                            .dispatch
                            .as_ref()
                            .and_then(|d| d.intent_id.as_deref())
                            == Some(intent_id.as_str())
                    {
                        attempt.input_reached = true;
                        attempt.dispatch = None;
                        attempt
                            .skill
                            .acted(crate::reaction::skills::ActionOutcome::Unknown {
                                at_ms: self.now_ms,
                            });
                    }
                    self.query_receipt(&intent_id);
                }
                Expect::Observe {
                    attempt,
                    observation_id,
                } => {
                    let is_current = self
                        .run
                        .as_ref()
                        .and_then(|run| run.attempt.as_ref())
                        .and_then(|attempt_run| attempt_run.dispatch.as_ref())
                        .is_some_and(|dispatch| {
                            dispatch.reply_corr == Some(corr)
                                && dispatch.stage == Stage::Observing
                                && dispatch.attempt == attempt
                                && dispatch.observation.observation_id() == observation_id
                        });
                    if is_current {
                        self.abort_dispatch(None, "the device did not answer the observation");
                    }
                }
                Expect::Grant {
                    attempt,
                    observation_id,
                } => {
                    let is_current = self
                        .run
                        .as_ref()
                        .and_then(|run| run.attempt.as_ref())
                        .and_then(|attempt_run| attempt_run.dispatch.as_ref())
                        .is_some_and(|dispatch| {
                            dispatch.reply_corr == Some(corr)
                                && dispatch.stage == Stage::Granting
                                && dispatch.attempt == attempt
                                && dispatch.observation.observation_id() == observation_id
                        });
                    if is_current {
                        self.abort_dispatch(None, "the device did not answer the permit request");
                    } else {
                        self.note(
                            "an obsolete permit request timed out; a late grant remains reserved for revocation",
                        );
                    }
                }
                Expect::Register => {
                    if let Some(run) = self.run.as_mut() {
                        run.registering = false;
                    }
                    self.note("the device did not answer the registration; it will be asked again");
                }
                Expect::Stop => {
                    self.stop_status = StopStatus::StopUnconfirmed;
                    self.note("the device has not confirmed the stop");
                }
                Expect::Query { intent_id } => self.recovery_query_timed_out(&intent_id),
                other => self.note(format!("no answer to {other:?}")),
            }
        }
        Ok(())
    }

    /// A lost recovery reply gets one retry. If that also fails, retain the
    /// same unknown attempt and wait for explicit authorisation to try again.
    /// Neither a missing reply nor a retry grants permission to replay input.
    fn recovery_query_timed_out(&mut self, intent_id: &str) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let Some((attempt, intent)) = run
            .reconciling
            .clone()
            .filter(|(_, intent)| intent == intent_id)
        else {
            self.note(format!("no answer to the receipt query for {intent_id}"));
            return;
        };
        if run.reconciliation_retries < RECONCILIATION_RETRIES {
            run.reconciliation_retries += 1;
            self.note(format!(
                "retrying the recovery receipt query for {intent_id}"
            ));
            self.query_receipt(intent_id);
        } else {
            run.reconciling = None;
            run.reconcile.push((attempt, intent));
            run.reconciliation_retries = 0;
            run.resume_authorized = false;
            self.note(format!(
                "the receipt for {intent_id} is still unknown; resume the task to retry recovery"
            ));
        }
    }

    /// Say so when a latched stop has held the loop a long time.
    pub(super) fn check_stop_wait(&mut self) {
        let now = self.now_ms;
        let long = self
            .run
            .as_ref()
            .and_then(|run| run.stop_wait_since)
            .is_some_and(|since| now.saturating_sub(since) > STOP_WAIT_NOTE_MS);
        if long {
            self.note("a stop is still latched at the device and the host has not received it");
            if let Some(run) = self.run.as_mut() {
                run.stop_wait_since = Some(now);
            }
        }
    }

    // ---- Revalidating a root -------------------------------------------------------------------------------------

    /// After a pause or a restart: register again, settle what is unknown, look
    /// again, and only then go on. The deadline and budget are the original ones.
    pub(super) fn step_revalidating(&mut self) -> Step {
        let Some(run) = self.run.as_ref() else {
            return Ok(false);
        };
        // A browser read scope is one-time and cannot be rebound to an
        // existing Root after a pause or restart. Unknown receipts remain
        // available for audit/reconciliation, but this Root may not spend its
        // observation budget waiting on native Source or a replacement page.
        if run.goal.execution_domain
            == interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
        {
            return Ok(false);
        }
        if !run.resume_authorized
            || self.mode != RunMode::Execute
            || !self.execution_profile_matches_goal()
        {
            return Ok(false);
        }
        if !run.registered {
            if run.registering {
                return Ok(false);
            }
            let grant = run.grant.clone();
            if let Some(run) = self.run.as_mut() {
                run.registering = true;
            }
            self.send(Expect::Register, DeviceCommand::RegisterRoot(grant));
            return Ok(true);
        }
        if run.reconciling.is_some() {
            return Ok(false);
        }
        if let Some((attempt, intent)) = run.reconcile.last().cloned() {
            if let Some(run) = self.run.as_mut() {
                run.reconcile.pop();
                run.reconciling = Some((attempt, intent.clone()));
                run.reconciliation_retries = 0;
            }
            self.query_receipt(&intent);
            return Ok(true);
        }
        let now = self.now_ms;
        let floor = {
            let run = self.run.as_mut().expect("checked above");
            *run.revalidate_floor.get_or_insert(now)
        };
        if self.state.latest_since(floor).is_none() {
            if self.run.as_ref().is_some_and(|run| run.wait.is_some()) {
                return Ok(false);
            }
            return self.request_refresh(floor, "revalidating the task");
        }
        if let Some(run) = self.run.as_mut() {
            run.wait = None;
            run.revalidate_floor = None;
            run.stalls = 0;
        }
        // Bindings made before the pause rest on evidence that is gone.
        let dropped = self.bindings.invalidate_all("the task was revalidated");
        if !dropped.is_empty() {
            self.bump_dependency("binding");
        }
        self.tasks
            .revalidation_passed(&mut self.storage, self.now_ms)?;
        self.note("the task was revalidated and goes on");
        Ok(true)
    }

    /// The device's record of an input whose outcome was unknown.
    pub(super) fn reconciled(
        &mut self,
        intent_id: &str,
        receipt: Option<Receipt>,
    ) -> Result<(), EngineError> {
        let Some((attempt, intent)) = self.run.as_ref().and_then(|run| run.reconciling.clone())
        else {
            // A query made for a timed-out submission, not for recovery.
            if let Some(receipt) = receipt {
                self.record_late_receipt(&receipt);
            }
            return Ok(());
        };
        if intent != intent_id {
            self.note("a receipt query for another intent cannot settle recovery");
            return Ok(());
        }
        let scoped = receipt.as_ref().is_none_or(|receipt| {
            receipt.intent_id == intent
                && receipt.attempt_id.as_deref() == Some(attempt.as_str())
                && Some(receipt.session_epoch) == self.device.session_epoch()
        });
        let definite = receipt.as_ref().is_some_and(|receipt| {
            matches!(
                receipt.status,
                ReceiptStatus::Completed | ReceiptStatus::Rejected | ReceiptStatus::Cancelled
            )
        });
        if let Some(run) = self.run.as_mut() {
            run.reconciling = None;
            run.reconciliation_retries = 0;
        }
        if scoped && let Some(receipt) = receipt.as_ref() {
            self.record_late_receipt(receipt);
            if receipt
                .submitted
                .iter()
                .any(|submitted| !submitted.cleanup())
                && let Some(run) = self.run.as_mut()
            {
                run.reconciled_input.insert(attempt.clone());
            }
        }
        if !scoped || !definite {
            if let Some(run) = self.run.as_mut() {
                run.reconcile.push((attempt.clone(), intent.clone()));
                run.resume_authorized = false;
            }
            self.note(format!(
                "the device has no definite record of {intent}; {attempt} stays unknown; resume the task to retry recovery"
            ));
            return Ok(());
        }
        let receipt = receipt.expect("a definite result has a receipt");
        let (pending, moved) = if let Some(run) = self.run.as_mut() {
            run.last_attempt = Some((attempt.clone(), Some(intent.clone())));
            (
                run.reconcile.iter().any(|(id, _)| id == &attempt),
                run.reconciled_input.contains(&attempt),
            )
        } else {
            return Ok(());
        };
        // An empty rejection for one intent proves nothing about the other
        // inputs of the same attempt. Settle and refund only once all are known.
        if !pending {
            self.tasks
                .attempt_ended(&mut self.storage, &attempt, AttemptState::Settled, !moved)?;
            if let Some(run) = self.run.as_mut() {
                run.reconciled_input.remove(&attempt);
            }
        }
        self.note(format!(
            "{attempt}: the device confirms {intent} ended as {:?}",
            receipt.status
        ));
        Ok(())
    }

    fn record_late_receipt(&mut self, receipt: &Receipt) {
        // Reuse the receipt path's bookkeeping for the input span only.
        let Some(run) = self.run.as_mut() else {
            return;
        };
        if matches!(
            receipt.status,
            ReceiptStatus::Unknown | ReceiptStatus::Partial
        ) || receipt.status == ReceiptStatus::Cancelled
            && receipt.submitted.iter().any(|s| !s.cleanup())
        {
            run.verification.record_unknown_input(receipt.at_ms);
        }
        let mut times = receipt
            .submitted
            .iter()
            .filter(|submitted| !submitted.cleanup())
            .map(|submitted| submitted.at_ms());
        let Some(first) = times.next() else {
            return;
        };
        let last = times.next_back().unwrap_or(first);
        let end = receipt
            .input_end_ms
            .or(receipt.command_end_ms)
            .unwrap_or(last)
            .max(last);
        run.verification.record_input(first, end);
    }

    // ---- After a restart --------------------------------------------------------------------------------------------

    pub(super) fn recover_inner(&mut self) -> Result<(), EngineError> {
        // A restarted Engine cannot reconstruct the trusted Main lifecycle.
        // Preserve its audit history while invalidating every unfinished bridge.
        OwnedRevisitStore::invalidate_after_restart(&mut self.storage, self.now_ms)?;
        self.owned_revisit = None;
        let generation = crate::deliberation::accounting::highest_generation(&self.storage)?;
        self.planning.advance_past(generation);
        self.storage
            .transaction::<_, crate::infrastructure::storage::StoreError>(|tx| {
                ReactionStore::expire_pending(tx).map(|_| ())
            })?;
        let report = self.tasks.recover(&mut self.storage, self.now_ms)?;
        for root in &report.cancelled {
            self.note(format!("{root} never reached the device and was cancelled"));
        }
        for root in &report.revalidating {
            self.note(format!(
                "{root} waits for the operator to resume and revalidate it"
            ));
        }
        for attempt in &report.unknown_attempts {
            self.note(format!(
                "attempt {} has an unknown outcome; its quota stays spent",
                attempt.attempt_id
            ));
        }
        let Some(active) = self.tasks.active() else {
            return Ok(());
        };
        let Some(goal) = active.row.goal.clone() else {
            return Ok(());
        };
        let Some(pack) = active.pack.clone() else {
            return Ok(());
        };
        let root = active.row.root_id.clone();
        let grant = RootGrant {
            root_id: root.as_str().to_string(),
            revision: goal.task_revision,
            root_epoch: active.row.root_epoch,
            deadline_ms: active.row.deadline_ms.unwrap_or(0),
        };
        let frozen = active.row.frozen_ms.unwrap_or(self.now_ms);
        let cancelling = active.row.state == RootState::Cancelling;
        let mut plan_targets: std::collections::BTreeSet<String> = active
            .row
            .plan
            .as_ref()
            .map(|plan| {
                plan.subgoals
                    .iter()
                    .filter_map(|subgoal| subgoal.target.as_ref())
                    .map(|target| target.as_str().to_string())
                    .collect()
            })
            .unwrap_or_default();
        plan_targets.insert(goal.target.semantic_target.as_str().to_string());
        // Every Submit is recorded before it leaves the engine. For records
        // without that marker, retain the conservative older recovery range.
        let reconcile: Vec<(AttemptId, String)> = report
            .unknown_attempts
            .iter()
            .flat_map(|attempt| {
                let prefix = format!("intent-{}-", attempt.attempt_id);
                let last = attempt
                    .intent_id
                    .as_deref()
                    .and_then(|id| id.strip_prefix(&prefix))
                    .and_then(|sequence| sequence.parse::<u32>().ok())
                    .filter(|sequence| *sequence > 0 && *sequence <= attempt.quota.actions)
                    .unwrap_or(attempt.quota.actions.max(1));
                (1..=last).map(|k| {
                    (
                        attempt.attempt_id.clone(),
                        format!("intent-{}-{k}", attempt.attempt_id),
                    )
                })
            })
            .collect();
        let mut verification = Verification::new(frozen);
        if let Some(stored) = VerificationStore::latest(self.storage.read(), root.as_str())? {
            verification.resume_after(stored.revision);
        }
        self.run = Some(super::RootRun {
            window_activated: false,
            keyboard_result_context: None,
            verification,
            goal,
            pack,
            grant,
            registered: false,
            registering: false,
            done: std::collections::BTreeSet::new(),
            attempt: None,
            wait: None,
            refined_group: None,
            chosen: None,
            plan_targets,
            recent: Vec::new(),
            failures: 0,
            decision_failures: 0,
            decision_resume_ms: 0,
            decision_retry_at: None,
            stalls: 0,
            bind_seq: 0,
            resume_authorized: false,
            reconcile,
            reconciling: None,
            reconciliation_retries: 0,
            reconciled_input: std::collections::BTreeSet::new(),
            effect_verification_only: false,
            revalidate_floor: None,
            permits: Vec::new(),
            stop_wait_since: None,
            paused_session: None,
            last_attempt: None,
        });
        if cancelling {
            // The cancellation was already under way; the device must still
            // confirm that it let go.
            self.stop_status = StopStatus::Stopping;
            self.send_host_stop(root.as_str(), "continuing a cancellation after a restart");
        }
        Ok(())
    }
}

#[cfg(test)]
mod grant_reply_tests {
    use super::*;
    use crate::engine::{EngineConfig, Output};
    use crate::infrastructure::storage::Storage;
    use crate::tasks::TaskConfig;
    use interaction_contracts::desktop::device::{
        BackendKind, CapabilityManifest, CapabilityState, CommandAck, DeviceIdentity, GrantOutcome,
        PermitRef, SurfaceRef,
    };
    use interaction_contracts::desktop::domain::ExecutionDomain;
    use interaction_contracts::desktop::ids::AttemptId;
    use interaction_contracts::desktop::primitive::PrimitivePolicy;
    use std::collections::BTreeMap;

    const SESSION_EPOCH: u64 = 7;

    fn engine() -> Engine {
        let mut engine = Engine::new(
            EngineConfig::new(TaskConfig::new(vec![])),
            Storage::open_in_memory().unwrap(),
        );
        engine.device.install_manifest(CapabilityManifest {
            identity: DeviceIdentity {
                session_epoch: SESSION_EPOCH,
                surface: SurfaceRef {
                    id: "main".into(),
                    epoch: 1,
                },
                clock_domain: "test/7/monotonic".into(),
                dependencies: BTreeMap::new(),
            },
            backend: BackendKind::Simulated,
            execution_domain: ExecutionDomain::Simulated,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Unavailable,
            hit_test: CapabilityState::Unavailable,
            input: CapabilityState::Available,
            domains: crate::testkit::domain_capabilities_for_test_profile(
                ExecutionDomain::Simulated,
            ),
            max_uninterruptible_ms: 0,
        });
        engine
    }

    fn observe_expect() -> Expect {
        Expect::Observe {
            attempt: AttemptId::new("attempt-observe").unwrap(),
            observation_id: "observation-observe".into(),
        }
    }

    fn grant_expect() -> Expect {
        Expect::Grant {
            attempt: AttemptId::new("attempt-grant").unwrap(),
            observation_id: "observation-grant".into(),
        }
    }

    fn permit(id: &str, generation: u64) -> PermitRef {
        PermitRef {
            id: id.into(),
            generation,
        }
    }

    fn assert_only_revoke(engine: &Engine, expected: &PermitRef) {
        let [Output::Device { corr, command }] = engine.outputs.as_slice() else {
            panic!("an unmatched grant must produce exactly one device command");
        };
        let DeviceCommand::Revoke(revoke) = command.as_ref() else {
            panic!("an unmatched grant must be revoked without submitting input");
        };
        assert_eq!(revoke.session_epoch, SESSION_EPOCH);
        assert_eq!(&revoke.permit, expected);
        assert!(matches!(
            engine.device.expected_reply(*corr),
            Some(Expect::Revoke)
        ));
    }

    #[test]
    fn mismatched_grant_revokes_without_consuming_the_observe_correlation() {
        let mut engine = engine();
        let corr = engine.device.send(100, observe_expect());
        let granted = permit("unexpected-observe-permit", 3);

        engine
            .on_device_reply(
                corr,
                DeviceReply::Grant(GrantOutcome::Granted {
                    permit: granted.clone(),
                }),
            )
            .unwrap();

        assert!(matches!(
            engine.device.expected_reply(corr),
            Some(Expect::Observe { .. })
        ));
        assert_only_revoke(&engine, &granted);

        engine.outputs.clear();
        engine
            .on_device_reply(corr, DeviceReply::Ack(CommandAck::ok()))
            .unwrap();
        assert!(engine.device.expected_reply(corr).is_none());
        assert!(
            engine.outputs.is_empty(),
            "a stale Observe cannot grant input"
        );
    }

    #[test]
    fn wrong_reply_variant_preserves_a_late_grant_tombstone_for_revocation() {
        let mut engine = engine();
        let corr = engine.device.send(100, grant_expect());
        assert_eq!(engine.device.timed_out(151, 50).len(), 1);
        assert!(matches!(
            engine.device.expected_reply(corr),
            Some(Expect::Grant { .. })
        ));

        engine
            .on_device_reply(corr, DeviceReply::Ack(CommandAck::ok()))
            .unwrap();
        assert!(matches!(
            engine.device.expected_reply(corr),
            Some(Expect::Grant { .. })
        ));
        assert!(engine.outputs.is_empty());

        let granted = permit("late-permit", 4);
        engine
            .on_device_reply(
                corr,
                DeviceReply::Grant(GrantOutcome::Granted {
                    permit: granted.clone(),
                }),
            )
            .unwrap();
        assert!(engine.device.expected_reply(corr).is_none());
        assert_only_revoke(&engine, &granted);
    }

    #[test]
    fn evicted_grant_tombstone_still_revokes_an_unknown_permit_reply() {
        let mut engine = engine();
        let oldest_corr = engine.device.send(0, grant_expect());
        engine.device.timed_out(1, 0);
        let mut sent_at = 1;
        while engine.device.expected_reply(oldest_corr).is_some() && sent_at < 10_000 {
            engine.device.send(sent_at, grant_expect());
            engine.device.timed_out(sent_at + 1, 0);
            sent_at += 1;
        }
        assert!(
            engine.device.expected_reply(oldest_corr).is_none(),
            "the bounded tombstone table eventually evicts the oldest correlation"
        );

        let granted = permit("evicted-tombstone-permit", 5);
        engine
            .on_device_reply(
                oldest_corr,
                DeviceReply::Grant(GrantOutcome::Granted {
                    permit: granted.clone(),
                }),
            )
            .unwrap();
        assert_only_revoke(&engine, &granted);
    }
}
