//! Host-side model/pixel lifecycle. Engine owns policy and execution; this
//! session only accounts requests, carries observations, and waits for explicit
//! native safety confirmation before releasing the original inert proposal.

use desktop_ipc::computer_control::{ComputerSafetyCheck as DisplayCheck, ComputerSafetyRequest};
use harness_adapter::computer::ComputerSafetyCheck;
use harness_adapter::computer::{ComputerActionOutcome, ComputerBatchOutcome, ComputerModelStep};
use harness_adapter::computer_worker::{
    ComputerScreenshotPhase, ComputerWorker, ComputerWorkerStartErrorKind, ComputerWorkerStatus,
};
use interaction_contracts::desktop::computer::ComputerFence;
use interaction_engine::deliberation::accounting::ModelUsage;
use interaction_engine::deliberation::computer_accounting::{
    ComputerCallCompletion, ComputerCallIdentity, ComputerCallOutcome, ComputerCallStage,
};
use interaction_engine::engine::computer::{ComputerCommand, ComputerInput};
use interaction_engine::engine::{Engine, Input};

use crate::computer_image::{ComputerImagePort, ComputerPortResult};

struct ActiveCall {
    identity: ComputerCallIdentity,
    cancelled: bool,
}
struct AwaitingStep {
    identity: ComputerCallIdentity,
    step: ComputerModelStep,
    accounting_done: bool,
}
struct PendingPixels {
    identity: ComputerCallIdentity,
    request: interaction_contracts::desktop::computer::ComputerFrameRequest,
}

pub(crate) struct ComputerSession {
    worker: ComputerWorker,
    pixels: ComputerImagePort,
    conversation: Option<ComputerCallIdentity>,
    active_call: Option<ActiveCall>,
    awaiting_step: Option<AwaitingStep>,
    pending_pixels: Option<PendingPixels>,
    queued_begin: Option<ComputerCommand>,
    worker_fence: Option<ComputerFence>,
    acknowledged_checks: Vec<ComputerSafetyCheck>,
    reset_before_begin: bool,
    revoked: bool,
}
impl ComputerSession {
    pub(crate) fn new(worker: ComputerWorker, pixels: ComputerImagePort) -> Self {
        Self {
            worker,
            pixels,
            conversation: None,
            active_call: None,
            awaiting_step: None,
            pending_pixels: None,
            queued_begin: None,
            worker_fence: None,
            acknowledged_checks: Vec::new(),
            reset_before_begin: false,
            revoked: false,
        }
    }
    pub(crate) fn is_idle(&self) -> bool {
        !self.worker.is_busy() && self.active_call.is_none()
    }
    pub(crate) fn revoke(&mut self) {
        self.revoked = true;
        self.queued_begin = None;
        if let Some(call) = self.active_call.as_mut() {
            call.cancelled = true;
        }
        self.worker.revoke();
        self.pixels.close();
        self.awaiting_step = None;
        self.acknowledged_checks.clear();
    }
    pub(crate) fn command(
        &mut self,
        engine: &mut Engine,
        command: ComputerCommand,
        now_ms: u64,
    ) -> Vec<Input> {
        match command {
            begin @ ComputerCommand::Begin { .. } => {
                if self.worker.is_busy() || self.pixels.is_busy() || self.active_call.is_some() {
                    self.queued_begin = Some(begin);
                    return vec![];
                }
                self.begin(engine, begin, now_ms)
            }
            ComputerCommand::Screenshot { identity, request } => {
                let request = *request;
                if self.revoked
                    || !self.same_conversation(&identity)
                    || request.fence != identity.fence
                {
                    return vec![observation_failure(
                        identity,
                        "computer screenshot scope is unavailable".into(),
                    )];
                }
                let (provider, model) = self.worker.identity();
                if engine
                    .computer_call_started(now_ms, &identity, provider, model)
                    .is_err()
                {
                    return vec![];
                }
                if self.active_call.is_some() || self.pixels.is_busy() {
                    return vec![failure(
                        identity,
                        now_ms,
                        ComputerCallOutcome::Unavailable,
                        "computer screenshot port is busy",
                    )];
                }
                if let Err(reason) = self.pixels.request(request.clone()) {
                    return vec![failure(
                        identity,
                        now_ms,
                        ComputerCallOutcome::Unavailable,
                        &reason,
                    )];
                }
                self.conversation = Some(identity.clone());
                self.active_call = Some(ActiveCall {
                    identity: identity.clone(),
                    cancelled: false,
                });
                self.pending_pixels = Some(PendingPixels { identity, request });
                vec![]
            }
            ComputerCommand::CallFinished {
                identity,
                effective_outcome,
            } => {
                let Some(step) = self.awaiting_step.as_mut() else {
                    return vec![];
                };
                if step.identity != identity {
                    return vec![];
                }
                if effective_outcome != ComputerCallOutcome::Proposal || self.revoked {
                    self.awaiting_step = None;
                    self.end_scope();
                    return vec![];
                }
                step.accounting_done = true;
                self.approve_or_release(now_ms)
            }
            ComputerCommand::BatchOutcome {
                identity,
                proposal,
                completed_prefix,
                total,
                terminal,
                ..
            } => {
                if !self.same_conversation(&identity) {
                    return vec![];
                }
                if terminal && completed_prefix == total && total == proposal.actions.len() {
                    let outcome = ComputerBatchOutcome {
                        fence: proposal.fence.clone(),
                        actions: (0..total)
                            .map(|index| ComputerActionOutcome {
                                index,
                                completed: true,
                            })
                            .collect(),
                    };
                    if let Err(reason) = self.worker.record_batch_outcome(&proposal, outcome) {
                        self.end_scope();
                        return vec![observation_failure(identity, reason.to_string())];
                    }
                } else if terminal {
                    self.end_scope();
                }
                vec![]
            }
            ComputerCommand::Cancel { identity, .. } => {
                if !self.same_conversation(&identity) {
                    return vec![];
                }
                self.queued_begin = None;
                let mut events = Vec::new();
                if let Some(pixels) = self.pending_pixels.take() {
                    self.pixels.cancel_scope(&pixels.request.fence);
                }
                if let Some(call) = self.active_call.as_mut() {
                    call.cancelled = true;
                }
                if !self.worker.is_busy()
                    && let Some(call) = self.active_call.take()
                {
                    events.push(failure(
                        call.identity,
                        now_ms,
                        ComputerCallOutcome::Cancelled,
                        "computer scope ended before provider dispatch",
                    ));
                }
                self.end_scope();
                events
            }
        }
    }
    fn begin(&mut self, engine: &mut Engine, command: ComputerCommand, now_ms: u64) -> Vec<Input> {
        let ComputerCommand::Begin {
            identity,
            goal,
            subgoal,
        } = command
        else {
            return vec![];
        };
        if self.revoked || self.pixels.is_closed() {
            return vec![observation_failure(
                identity,
                "computer session is unavailable".into(),
            )];
        }
        // Begin always starts a new approved subgoal conversation. A previous
        // complete batch may still have a provider continuation awaiting an
        // observation, so close it explicitly before starting this conversation.
        if self.worker_fence.is_some() && !self.reset_before_begin {
            self.end_scope();
        }
        if self.reset_before_begin {
            if self.worker.reset_ledger().is_err() {
                self.queued_begin = Some(ComputerCommand::Begin {
                    identity,
                    goal,
                    subgoal,
                });
                return vec![];
            }
            self.reset_before_begin = false;
            self.worker_fence = None;
        }
        let (provider, model) = self.worker.identity();
        if engine
            .computer_call_started(now_ms, &identity, provider, model)
            .is_err()
        {
            return vec![];
        }
        let model_goal = format!("{goal}\n\nCurrent approved subgoal: {subgoal}");
        if let Err(error) = self.worker.start_begin(identity.fence.clone(), model_goal) {
            return vec![failure(
                identity,
                now_ms,
                admission_outcome(error.kind),
                &error.to_string(),
            )];
        }
        self.worker_fence = Some(identity.fence.clone());
        self.conversation = Some(identity.clone());
        self.active_call = Some(ActiveCall {
            identity,
            cancelled: false,
        });
        self.acknowledged_checks.clear();
        vec![]
    }
    pub(crate) fn poll(&mut self, engine: &mut Engine, now_ms: u64) -> Vec<Input> {
        let mut events = Vec::new();
        if let Some(result) = self.worker.poll()
            && let Some(call) = self.active_call.take()
        {
            if result.fence != call.identity.fence {
                self.revoke();
                events.push(Input::Computer(Box::new(ComputerInput::Failed {
                    completion: ComputerCallCompletion {
                        identity: call.identity,
                        at_ms: now_ms,
                        outcome: ComputerCallOutcome::Quarantined,
                        usage: ModelUsage {
                            input_tokens: result.usage.input_tokens,
                            output_tokens: result.usage.output_tokens,
                            cost_micros: result.usage.cost_micros,
                        },
                    },
                    reason: "computer worker returned a foreign fence".into(),
                })));
            } else {
                let outcome = if call.cancelled {
                    ComputerCallOutcome::Cancelled
                } else {
                    match result.status {
                        ComputerWorkerStatus::Completed => match &result.result {
                            Ok(ComputerModelStep::FinishedObservation { .. }) => {
                                ComputerCallOutcome::Missing
                            }
                            _ => ComputerCallOutcome::Proposal,
                        },
                        ComputerWorkerStatus::Cancelled => ComputerCallOutcome::Cancelled,
                        ComputerWorkerStatus::Expired => ComputerCallOutcome::Expired,
                        ComputerWorkerStatus::Invalid => ComputerCallOutcome::Invalid,
                        ComputerWorkerStatus::Revoked => ComputerCallOutcome::Revoked,
                        ComputerWorkerStatus::Failed => ComputerCallOutcome::Failed,
                    }
                };
                let completion = ComputerCallCompletion {
                    identity: call.identity.clone(),
                    at_ms: now_ms,
                    outcome,
                    usage: ModelUsage {
                        input_tokens: result.usage.input_tokens,
                        output_tokens: result.usage.output_tokens,
                        cost_micros: result.usage.cost_micros,
                    },
                };
                match result.result {
                    Ok(step) if outcome == ComputerCallOutcome::Proposal && !self.revoked => {
                        self.awaiting_step = Some(AwaitingStep {
                            identity: call.identity,
                            step,
                            accounting_done: false,
                        });
                        events.push(Input::Computer(Box::new(ComputerInput::Finished {
                            completion,
                        })));
                    }
                    Ok(_) => {
                        self.end_scope();
                        events.push(Input::Computer(Box::new(ComputerInput::Finished {
                            completion,
                        })));
                    }
                    Err(error) => {
                        self.end_scope();
                        events.push(Input::Computer(Box::new(ComputerInput::Failed {
                            completion,
                            reason: error.to_string(),
                        })));
                    }
                }
            }
        }
        if let Some(result) = self.pixels.poll() {
            match result {
                ComputerPortResult::Image(result) => {
                    if let Some(pending) = self.pending_pixels.take() {
                        if result.cancelled
                            || result.request != pending.request
                            || !self.same_conversation(&pending.identity)
                            || self.revoked
                            || engine
                                .validate_computer_frame_request(
                                    &pending.identity,
                                    &pending.request,
                                    now_ms,
                                )
                                .is_err()
                        {
                            if let Some(call) = self.active_call.take() {
                                events.push(failure(
                                    call.identity,
                                    now_ms,
                                    ComputerCallOutcome::Cancelled,
                                    "computer image scope ended",
                                ));
                            }
                        } else {
                            match result.response {
                                Ok(
                                    desktop_ipc::computer_control::ComputerImageResponse::Image(
                                        packet,
                                    ),
                                ) => {
                                    let phase = match pending.identity.stage {
                                        ComputerCallStage::InitialScreenshot => {
                                            ComputerScreenshotPhase::Initial
                                        }
                                        ComputerCallStage::FreshScreenshot => {
                                            ComputerScreenshotPhase::Fresh
                                        }
                                        ComputerCallStage::Begin => {
                                            self.revoke();
                                            continue_image_failure(
                                                &mut events,
                                                self.active_call.take(),
                                                now_ms,
                                            );
                                            return events;
                                        }
                                    };
                                    match self.worker.start_screenshot(
                                        phase,
                                        pending.request,
                                        packet.image,
                                        self.acknowledged_checks.clone(),
                                    ) {
                                        Ok(()) => {
                                            self.worker_fence = Some(pending.identity.fence.clone())
                                        }
                                        Err(error) => {
                                            if let Some(call) = self.active_call.take() {
                                                events.push(failure(
                                                    call.identity,
                                                    now_ms,
                                                    admission_outcome(error.kind),
                                                    &error.to_string(),
                                                ));
                                            }
                                        }
                                    }
                                }
                                _ => {
                                    if let Some(call) = self.active_call.take() {
                                        events.push(failure(
                                            call.identity,
                                            now_ms,
                                            ComputerCallOutcome::Unavailable,
                                            "computer pixels are unavailable",
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                ComputerPortResult::Safety(result) => {
                    if let Some(step) = self.awaiting_step.as_ref()
                        && step.identity.fence == result.request.fence
                    {
                        let approved = !result.cancelled
                            && !self.revoked
                            && result.response.is_ok_and(|reply| reply.approved);
                        if approved {
                            self.acknowledged_checks = step_checks(&step.step).to_vec();
                            events.extend(self.release_step(now_ms));
                        } else {
                            let identity = step.identity.clone();
                            self.end_scope();
                            events.push(safety_unconfirmed(
                                identity,
                                "computer safety confirmation was declined or expired".into(),
                            ));
                        }
                    }
                }
            }
        }
        if !self.revoked
            && !self.worker.is_busy()
            && !self.pixels.is_busy()
            && self.active_call.is_none()
            && let Some(begin) = self.queued_begin.take()
        {
            events.extend(self.begin(engine, begin, now_ms));
        }
        events
    }
    fn approve_or_release(&mut self, now_ms: u64) -> Vec<Input> {
        let Some(step) = self.awaiting_step.as_ref() else {
            return vec![];
        };
        if !step.accounting_done {
            return vec![];
        }
        let checks = step_checks(&step.step);
        if checks.is_empty() {
            self.acknowledged_checks.clear();
            return self.release_step(now_ms);
        }
        let request = ComputerSafetyRequest {
            fence: step.identity.fence.clone(),
            pending_safety_checks: checks
                .iter()
                .map(|check| DisplayCheck {
                    id: check.id.clone(),
                    code: check.code.clone(),
                    message: check.message.clone(),
                })
                .collect(),
            proposal: match &step.step {
                ComputerModelStep::ActionsProposed { proposal, .. } => Some(proposal.clone()),
                _ => None,
            },
        };
        if let Err(reason) = self.pixels.request_safety(request) {
            let identity = step.identity.clone();
            self.end_scope();
            return vec![safety_unconfirmed(identity, reason)];
        }
        vec![]
    }
    fn release_step(&mut self, now_ms: u64) -> Vec<Input> {
        let Some(step) = self.awaiting_step.take() else {
            return vec![];
        };
        if !step.accounting_done || self.revoked || now_ms >= step.identity.fence.deadline_ms {
            self.end_scope();
            return vec![observation_failure(
                step.identity,
                "computer result expired before delivery".into(),
            )];
        }
        let event = match step.step {
            ComputerModelStep::ScreenshotRequested { .. } => {
                let (provider, model) = self.worker.identity();
                ComputerInput::ScreenshotNeeded {
                    identity: step.identity,
                    capture_after_ms: now_ms,
                    provider: provider.into(),
                    model: model.into(),
                }
            }
            ComputerModelStep::ActionsProposed { proposal, .. } => ComputerInput::Proposal {
                identity: step.identity,
                proposal,
            },
            ComputerModelStep::FinishedObservation { .. } => {
                self.end_scope();
                return vec![];
            }
        };
        vec![Input::Computer(Box::new(event))]
    }
    fn same_conversation(&self, identity: &ComputerCallIdentity) -> bool {
        self.conversation.as_ref().is_some_and(|current| {
            current.fence.root_task_id == identity.fence.root_task_id
                && current.fence.task_revision == identity.fence.task_revision
                && current.fence.root_epoch == identity.fence.root_epoch
                && current.plan_revision == identity.plan_revision
                && current.subgoal_id == identity.subgoal_id
        })
    }
    fn end_scope(&mut self) {
        if let Some(step) = self.awaiting_step.as_ref() {
            self.pixels.cancel_scope(&step.identity.fence);
        }
        if let Some(fence) = self.worker_fence.as_ref() {
            let _ = self.worker.finish_scope(fence);
            self.reset_before_begin = true;
        }
        self.awaiting_step = None;
        self.acknowledged_checks.clear();
    }
}
fn step_checks(step: &ComputerModelStep) -> &[ComputerSafetyCheck] {
    match step {
        ComputerModelStep::ScreenshotRequested {
            pending_safety_checks,
            ..
        }
        | ComputerModelStep::ActionsProposed {
            pending_safety_checks,
            ..
        } => pending_safety_checks,
        ComputerModelStep::FinishedObservation { .. } => &[],
    }
}
fn admission_outcome(kind: ComputerWorkerStartErrorKind) -> ComputerCallOutcome {
    match kind {
        ComputerWorkerStartErrorKind::Expired => ComputerCallOutcome::Expired,
        ComputerWorkerStartErrorKind::Invalid => ComputerCallOutcome::Invalid,
        ComputerWorkerStartErrorKind::Revoked => ComputerCallOutcome::Revoked,
        ComputerWorkerStartErrorKind::Busy
        | ComputerWorkerStartErrorKind::Closed
        | ComputerWorkerStartErrorKind::ScopeEnded => ComputerCallOutcome::Unavailable,
    }
}
fn failure(
    identity: ComputerCallIdentity,
    at_ms: u64,
    outcome: ComputerCallOutcome,
    reason: &str,
) -> Input {
    Input::Computer(Box::new(ComputerInput::Failed {
        completion: ComputerCallCompletion {
            identity,
            at_ms,
            outcome,
            usage: ModelUsage::default(),
        },
        reason: reason.into(),
    }))
}
fn safety_unconfirmed(identity: ComputerCallIdentity, reason: String) -> Input {
    Input::Computer(Box::new(ComputerInput::SafetyUnconfirmed {
        identity,
        reason,
    }))
}
fn observation_failure(identity: ComputerCallIdentity, reason: String) -> Input {
    // No second provider completion: its row was already closed before safety
    // or batch admission. Engine receives only a scoped observation failure.
    Input::Computer(Box::new(ComputerInput::ObservationFailed {
        identity,
        reason,
    }))
}
fn continue_image_failure(events: &mut Vec<Input>, call: Option<ActiveCall>, now_ms: u64) {
    if let Some(call) = call {
        events.push(failure(
            call.identity,
            now_ms,
            ComputerCallOutcome::Invalid,
            "invalid computer screenshot phase",
        ));
    }
}
