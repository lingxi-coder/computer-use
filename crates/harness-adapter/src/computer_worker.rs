//! One asynchronous computer-model job, polled by the Host control loop.
//!
//! Pixels and responses stay off the Host thread. A result becomes visible only
//! after the request future and response stream have been dropped. Scoped
//! cancellation closes only that request; purpose revocation seals admission.
//! Neither path publishes a late action proposal.

use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};

use base64::Engine as _;
use desktop_ipc::image::{ImageFrame, MAX_IMAGE};
use interaction_contracts::desktop::computer::{
    ComputerBatchProposal, ComputerFence, ComputerFrameRequest,
};
use tokio::runtime::Runtime;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::computer::{
    ComputerBatchOutcome, ComputerFailureKind, ComputerModelStep, ComputerSafetyCheck,
    OpenAiComputerTransport,
};
use crate::{TransportError, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerScreenshotPhase {
    Initial,
    Fresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerWorkerStatus {
    Completed,
    Cancelled,
    Expired,
    Invalid,
    Failed,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerWorkerStartErrorKind {
    Invalid,
    Expired,
    Busy,
    Revoked,
    Closed,
    ScopeEnded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerWorkerStartError {
    pub kind: ComputerWorkerStartErrorKind,
    pub reason: String,
}

impl std::fmt::Display for ComputerWorkerStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "computer worker {:?}: {}", self.kind, self.reason)
    }
}

impl std::error::Error for ComputerWorkerStartError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeFinish {
    /// The request stream is still owned by the worker. Drive `poll` through
    /// its closing result before attempting to reset the ledger.
    Closing,
    /// The old response ledger has been dropped and the scope is closed.
    Closed,
}

/// Request-local usage remains available on failure. Receiving this result is
/// the closing barrier for this job, not evidence of local input or effect.
pub struct ComputerWorkerResult {
    pub fence: ComputerFence,
    pub result: Result<ComputerModelStep, TransportError>,
    pub usage: Usage,
    pub status: ComputerWorkerStatus,
}

enum Command {
    Begin(String),
    Screenshot {
        phase: ComputerScreenshotPhase,
        request: Box<ComputerFrameRequest>,
        image: Box<ImageFrame>,
        acknowledged_safety_checks: Vec<ComputerSafetyCheck>,
    },
}

struct Job {
    fence: ComputerFence,
    cancel: watch::Sender<bool>,
    task: JoinHandle<(OpenAiComputerTransport, ComputerWorkerResult)>,
}

/// Owns one model ledger and admits at most one network job. Polling never
/// waits for network I/O, screenshot validation, encoding, or cancellation.
pub struct ComputerWorker {
    runtime: Option<Runtime>,
    transport: Option<OpenAiComputerTransport>,
    identity: (String, String),
    job: Option<Job>,
    last_active_fence: Option<ComputerFence>,
    finish_requested: Option<ComputerFence>,
    scope_finished: bool,
    revoked: Arc<AtomicBool>,
}

fn start_error(kind: ComputerWorkerStartErrorKind, reason: &str) -> ComputerWorkerStartError {
    ComputerWorkerStartError {
        kind,
        reason: reason.into(),
    }
}

impl ComputerWorker {
    pub fn new(transport: OpenAiComputerTransport) -> Result<Self, TransportError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("cu-computer-model")
            .enable_all()
            .build()
            .map_err(|_| TransportError("could not initialize computer model worker".into()))?;
        let (provider, model) = transport.identity();
        let identity = (provider.to_owned(), model.to_owned());
        Ok(Self {
            runtime: Some(runtime),
            transport: Some(transport),
            identity,
            job: None,
            last_active_fence: None,
            finish_requested: None,
            scope_finished: false,
            revoked: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn identity(&self) -> (&str, &str) {
        (&self.identity.0, &self.identity.1)
    }

    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn start_begin(
        &mut self,
        fence: ComputerFence,
        goal: String,
    ) -> Result<(), ComputerWorkerStartError> {
        if goal.trim().is_empty() || goal.len() > 64 * 1024 || goal.contains('\0') {
            return Err(start_error(
                ComputerWorkerStartErrorKind::Invalid,
                "computer goal is empty or exceeds quota",
            ));
        }
        if self.revoked.load(Ordering::Acquire) {
            return Err(start_error(
                ComputerWorkerStartErrorKind::Revoked,
                "computer purpose has been revoked",
            ));
        }
        if self.job.is_some() {
            return Err(start_error(
                ComputerWorkerStartErrorKind::Busy,
                "computer model request is still running",
            ));
        }
        if self.last_active_fence.is_some() {
            return Err(start_error(
                ComputerWorkerStartErrorKind::ScopeEnded,
                "a root can begin only on a fresh or explicitly reset ledger",
            ));
        }
        self.start(fence, Command::Begin(goal))
    }

    pub fn start_screenshot(
        &mut self,
        phase: ComputerScreenshotPhase,
        request: ComputerFrameRequest,
        image: ImageFrame,
        acknowledged_safety_checks: Vec<ComputerSafetyCheck>,
    ) -> Result<(), ComputerWorkerStartError> {
        request.validate().map_err(|_| {
            start_error(
                ComputerWorkerStartErrorKind::Invalid,
                "computer frame request is invalid",
            )
        })?;
        if image.meta != request.expected_frame || image.bytes.len() > MAX_IMAGE {
            return Err(start_error(
                ComputerWorkerStartErrorKind::Invalid,
                "computer image does not match its request",
            ));
        }
        self.start(
            request.fence.clone(),
            Command::Screenshot {
                phase,
                request: Box::new(request),
                image: Box::new(image),
                acknowledged_safety_checks,
            },
        )
    }

    fn start(
        &mut self,
        fence: ComputerFence,
        command: Command,
    ) -> Result<(), ComputerWorkerStartError> {
        fence.validate().map_err(|_| {
            start_error(
                ComputerWorkerStartErrorKind::Invalid,
                "computer fence is invalid",
            )
        })?;
        if self.revoked.load(Ordering::Acquire) {
            return Err(start_error(
                ComputerWorkerStartErrorKind::Revoked,
                "computer purpose has been revoked",
            ));
        }
        if self.job.is_some() {
            return Err(start_error(
                ComputerWorkerStartErrorKind::Busy,
                "computer model request is still running",
            ));
        }
        if self.scope_finished || self.finish_requested.is_some() {
            return Err(start_error(
                ComputerWorkerStartErrorKind::ScopeEnded,
                "computer scope must be reset before another request",
            ));
        }
        if let Some(previous) = &self.last_active_fence
            && (previous.root_task_id != fence.root_task_id
                || previous.task_revision != fence.task_revision
                || previous.root_epoch != fence.root_epoch)
        {
            return Err(start_error(
                ComputerWorkerStartErrorKind::ScopeEnded,
                "a different root requires closing and resetting the previous scope",
            ));
        }
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            start_error(
                ComputerWorkerStartErrorKind::Closed,
                "computer worker is closed",
            )
        })?;
        let mut transport = self.transport.take().ok_or_else(|| {
            start_error(
                ComputerWorkerStartErrorKind::Closed,
                "computer worker has no model ledger",
            )
        })?;
        if transport.deadline_elapsed(&fence) {
            self.transport = Some(transport);
            return Err(start_error(
                ComputerWorkerStartErrorKind::Expired,
                "computer request deadline has elapsed",
            ));
        }
        let revoked = self.revoked.clone();
        let (cancel, mut receiver) = watch::channel(false);
        let result_fence = fence.clone();
        let task = runtime.spawn(async move {
            transport.reset_usage();
            let mut result = if revoked.load(Ordering::Acquire) || *receiver.borrow() {
                Err(TransportError("computer model request cancelled".into()))
            } else {
                match command {
                    Command::Begin(goal) => {
                        transport
                            .begin(result_fence.clone(), &goal, &mut receiver)
                            .await
                    }
                    Command::Screenshot {
                        phase,
                        request,
                        image,
                        acknowledged_safety_checks,
                    } => {
                        // Encoding runs on this worker. The transport validates
                        // the original PNG before any provider request is made.
                        let uri = format!(
                            "data:image/png;base64,{}",
                            base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                        );
                        match phase {
                            ComputerScreenshotPhase::Initial => {
                                transport
                                    .submit_initial_screenshot(
                                        *request,
                                        uri,
                                        acknowledged_safety_checks,
                                        &mut receiver,
                                    )
                                    .await
                            }
                            ComputerScreenshotPhase::Fresh => {
                                transport
                                    .submit_fresh_screenshot(
                                        *request,
                                        uri,
                                        acknowledged_safety_checks,
                                        &mut receiver,
                                    )
                                    .await
                            }
                        }
                    }
                }
            };
            let usage = transport.last_usage();
            let was_revoked = revoked.load(Ordering::Acquire);
            let was_cancelled = *receiver.borrow();
            if was_cancelled || was_revoked {
                result = Err(TransportError(
                    "computer model request cancelled or revoked".into(),
                ));
                if was_revoked {
                    transport.revoke();
                }
            }
            let status = if was_revoked {
                ComputerWorkerStatus::Revoked
            } else if was_cancelled {
                ComputerWorkerStatus::Cancelled
            } else if transport.deadline_elapsed(&result_fence) {
                ComputerWorkerStatus::Expired
            } else if transport.failure_kind() == Some(ComputerFailureKind::Invalid) {
                ComputerWorkerStatus::Invalid
            } else if result.is_ok() {
                ComputerWorkerStatus::Completed
            } else {
                ComputerWorkerStatus::Failed
            };
            (
                transport,
                ComputerWorkerResult {
                    fence: result_fence,
                    result,
                    usage,
                    status,
                },
            )
        });
        self.job = Some(Job {
            fence: fence.clone(),
            cancel,
            task,
        });
        self.last_active_fence = Some(fence);
        Ok(())
    }

    /// A result is consumed once. Even a task that finished just before Stop
    /// cannot publish proposals after the cancellation flag is set.
    pub fn poll(&mut self) -> Option<ComputerWorkerResult> {
        let job = self.job.as_mut()?;
        let mut context = Context::from_waker(futures::task::noop_waker_ref());
        let Poll::Ready(joined) = Pin::new(&mut job.task).poll(&mut context) else {
            return None;
        };
        let job = self.job.take()?;
        match joined {
            Ok((mut transport, mut result)) => {
                if self.revoked.load(Ordering::Acquire) {
                    transport.revoke();
                    result.result = Err(TransportError(
                        "computer model request cancelled or revoked".into(),
                    ));
                    result.status = ComputerWorkerStatus::Revoked;
                } else if *job.cancel.borrow() {
                    result.result = Err(TransportError("computer model request cancelled".into()));
                    result.status = ComputerWorkerStatus::Cancelled;
                }
                let finish_requested = self.finish_requested.as_ref() == Some(&job.fence);
                let terminal = matches!(
                    result.result.as_ref(),
                    Ok(ComputerModelStep::FinishedObservation { .. })
                );
                let failed = result.result.is_err();
                if finish_requested || terminal || failed {
                    if self.revoked.load(Ordering::Acquire) || transport.close_scope().is_ok() {
                        self.scope_finished = true;
                    } else {
                        self.revoked.store(true, Ordering::Release);
                        transport.revoke();
                        result.status = ComputerWorkerStatus::Failed;
                        result.result = Err(TransportError(
                            "computer scope could not cross its closing barrier".into(),
                        ));
                    }
                    self.finish_requested = None;
                }
                self.transport = Some(transport);
                Some(result)
            }
            Err(_) => {
                self.revoked.store(true, Ordering::Release);
                Some(ComputerWorkerResult {
                    fence: job.fence,
                    result: Err(TransportError("computer model worker terminated".into())),
                    usage: Usage::default(),
                    status: ComputerWorkerStatus::Failed,
                })
            }
        }
    }

    pub fn record_batch_outcome(
        &mut self,
        proposal: &ComputerBatchProposal,
        outcome: ComputerBatchOutcome,
    ) -> Result<(), TransportError> {
        if self.revoked.load(Ordering::Acquire)
            || self.job.is_some()
            || self.scope_finished
            || self.finish_requested.is_some()
        {
            return Err(TransportError("computer worker is busy or revoked".into()));
        }
        self.transport
            .as_mut()
            .ok_or_else(|| TransportError("computer worker has no model ledger".into()))?
            .record_batch_outcome(proposal, outcome)
    }

    pub fn cancel(&mut self, fence: &ComputerFence) {
        if let Some(job) = &self.job
            && &job.fence == fence
        {
            let _ = job.cancel.send(true);
        }
    }

    /// End only the most recent Host-observed fence. If a job still owns the
    /// transport, cancellation is requested and `poll` must be driven through
    /// its closing barrier before the ledger can be reset.
    pub fn finish_scope(&mut self, fence: &ComputerFence) -> Result<ScopeFinish, TransportError> {
        if self.last_active_fence.as_ref() != Some(fence) {
            return Err(TransportError(
                "computer scope fence does not match the active scope".into(),
            ));
        }
        if self.scope_finished {
            return Ok(ScopeFinish::Closed);
        }
        if let Some(job) = &self.job {
            if &job.fence != fence {
                return Err(TransportError(
                    "computer scope fence does not match the running job".into(),
                ));
            }
            self.finish_requested = Some(fence.clone());
            let _ = job.cancel.send(true);
            return Ok(ScopeFinish::Closing);
        }
        self.transport
            .as_mut()
            .ok_or_else(|| TransportError("computer worker has no model ledger".into()))?
            .close_scope()?;
        self.scope_finished = true;
        self.finish_requested = None;
        Ok(ScopeFinish::Closed)
    }

    /// A new root may start only after the previous scope is terminal or has
    /// crossed `finish_scope`'s closing barrier. This resets response state
    /// without replacing the approved route, account scope, or credential.
    pub fn reset_ledger(&mut self) -> Result<(), TransportError> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(TransportError(
                "a revoked computer purpose cannot reset its ledger".into(),
            ));
        }
        if self.job.is_some() || !self.scope_finished {
            return Err(TransportError(
                "computer ledger reset requires a finished scope and no running job".into(),
            ));
        }
        self.transport
            .as_mut()
            .ok_or_else(|| TransportError("computer worker has no model ledger".into()))?
            .reset_ledger()?;
        self.last_active_fence = None;
        self.finish_requested = None;
        self.scope_finished = false;
        Ok(())
    }

    /// Seal admission synchronously. The running future owns its credentials
    /// until its response stream is dropped; then it deletes the secret.
    pub fn revoke(&mut self) {
        self.revoked.store(true, Ordering::Release);
        if let Some(job) = &self.job {
            let _ = job.cancel.send(true);
        }
        if let Some(transport) = self.transport.as_mut() {
            transport.revoke();
        }
    }
}

impl Drop for ComputerWorker {
    fn drop(&mut self) {
        self.revoke();
        if let Some(runtime) = self.runtime.take() {
            // Shutdown cancels all tasks without joining on the Host thread.
            // The task owns its model/secret and releases them with its future.
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
#[path = "computer_worker_tests.rs"]
mod tests;
