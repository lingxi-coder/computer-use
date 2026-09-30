//! The device's side of the conversation: replies, unsolicited notices, stops,
//! resuming and the recovery after a restart (design 14.8, 16.6 and 20.2).
//!
//! A stop reaches the device directly from the operator, so the host may learn
//! of it from the device's acknowledgement before it has heard from anyone
//! else. The host acts on the stop first and only then acknowledges it, so the
//! device never sees an acknowledgement for a stop the task runtime has not
//! handled.

use interaction_contracts::desktop::device::{
    CapabilityManifest, DeviceEventKind, Receipt, ReleaseStatus, RootGrant, StopAck,
    StopDisposition, StopScope, StopSource,
};
use interaction_contracts::desktop::ids::AttemptId;
use interaction_contracts::desktop::operator::{RunMode, StopStatus};
use interaction_contracts::desktop::task::{AttemptState, RootState};

use super::{Engine, EngineError, Step};
use crate::execution::{DeviceCommand, DeviceNotice, DeviceReply, Expect};
use crate::reaction::store::ReactionStore;
use crate::tasks::StopDirective;
use crate::verification::{Verification, VerificationStore};

/// How long a stop that the host has not caught up with may hold the loop
/// before it says so.
const STOP_WAIT_NOTE_MS: u64 = 5_000;

fn settled(cleanup: ReleaseStatus) -> bool {
    matches!(cleanup, ReleaseStatus::Released | ReleaseStatus::NotNeeded)
}

impl Engine {
    // ---- Replies -----------------------------------------------------------------------------

    pub(super) fn on_device_reply(
        &mut self,
        corr: u64,
        reply: DeviceReply,
    ) -> Result<(), EngineError> {
        let Some(expect) = self.device.reply(corr) else {
            self.note("a reply to an unknown or timed-out command was ignored");
            return Ok(());
        };
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
            (Expect::Observe, DeviceReply::Ack(ack)) => self.observed(ack),
            (Expect::Grant, DeviceReply::Grant(outcome)) => self.granted(outcome),
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
            (expect, reply) => {
                self.note(format!("unexpected reply {reply:?} to {expect:?}"));
                Ok(())
            }
        }
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
        let previous = self.device.session_epoch();
        self.device.install_manifest(manifest);
        self.state.set_session(epoch);
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
            self.stop_status = StopStatus::NotStopped;
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
        for (_, expect) in late {
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
                Expect::Observe | Expect::Grant => {
                    self.abort_dispatch(None, "the device did not answer in time");
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
                other => self.note(format!("no answer to {other:?}")),
            }
        }
        Ok(())
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
        if !run.resume_authorized || self.mode != RunMode::Execute || !self.device.input_available()
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
        let Some((attempt, intent)) = self.run.as_mut().and_then(|run| run.reconciling.take())
        else {
            // A query made for a timed-out submission, not for recovery.
            if let Some(receipt) = receipt {
                self.record_late_receipt(&receipt);
            }
            return Ok(());
        };
        match receipt {
            Some(receipt)
                if !matches!(
                    receipt.status,
                    interaction_contracts::desktop::device::ReceiptStatus::Accepted
                        | interaction_contracts::desktop::device::ReceiptStatus::Running
                ) =>
            {
                if let Some(run) = self.run.as_mut() {
                    run.last_attempt = Some((attempt.clone(), Some(intent.clone())));
                }
                self.record_late_receipt(&receipt);
                let moved = receipt.submitted.iter().any(|submitted| !submitted.cleanup);
                self.tasks.attempt_ended(
                    &mut self.storage,
                    &attempt,
                    AttemptState::Settled,
                    !moved,
                )?;
                self.note(format!(
                    "{attempt}: the device confirms {intent} ended as {:?}",
                    receipt.status
                ));
            }
            _ => self.note(format!(
                "the device has no finished record of {intent_id}; {attempt} stays unknown"
            )),
        }
        Ok(())
    }

    fn record_late_receipt(&mut self, receipt: &Receipt) {
        // Reuse the receipt path's bookkeeping for the input span only.
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let mut times = receipt
            .submitted
            .iter()
            .filter(|submitted| !submitted.cleanup)
            .map(|submitted| submitted.at_ms);
        let Some(first) = times.next() else {
            return;
        };
        let last = times.next_back().unwrap_or(first);
        let end = receipt.command_end_ms.unwrap_or(last).max(last);
        run.verification.record_input(first, end);
    }

    // ---- After a restart --------------------------------------------------------------------------------------------

    pub(super) fn recover_inner(&mut self) -> Result<(), EngineError> {
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
        // The device is asked about every input the attempt could have sent.
        let reconcile: Vec<(AttemptId, String)> = report
            .unknown_attempts
            .iter()
            .flat_map(|attempt| {
                (1..=attempt.quota.actions.max(1)).map(|k| {
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
