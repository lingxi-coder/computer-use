//! Typed OpenAI Responses computer-use transport and single-call continuation ledger.
//!
//! This module turns completed SDK-native calls into bounded proposal data. It
//! never dispatches Device input and it never treats a provider completion as
//! evidence that a local action ran.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use interaction_contracts::desktop::computer::{
    ComputerActionProposal, ComputerBatchProposal, ComputerFence, ComputerFrameRequest,
    ComputerMouseButton, ComputerPoint,
};
use interaction_contracts::desktop::source::FrameMeta;
use lingxi_llm_client::protocol::{
    ContinuationRef, ConversationMessage, MessageRole, StopReason, StreamEvent, SystemBlock,
};
pub use lingxi_llm_client::providers::openai::computer::ComputerSafetyCheck;
use lingxi_llm_client::providers::openai::computer::{
    ComputerAction, ComputerMouseButton as SdkComputerMouseButton, ComputerScreenshot,
    ComputerScreenshotDetail, ComputerScreenshotType, MAX_COMPUTER_SCREENSHOT_URL_BYTES,
    OPENAI_COMPUTER_CALL_FORMAT, OpenAiComputerCall, OpenAiComputerCallOutput,
    OpenAiComputerCallOutputType, OpenAiComputerToolConfig,
};
use lingxi_llm_client::{HttpTransport, Transport};
use llm_runtime::{LlmRequest, ModelRuntime};
use serde_json::Value;
use tokio::sync::watch;

use crate::TransportError;
use crate::model::{
    COMPUTER_CREDENTIAL, COMPUTER_PROFILE, EphemeralCredential, ModelProtocol, ModelProvider,
    ModelRoute, bounded_transport, is_official_openai_responses_endpoint, pricing_model_ref,
    quote_cost_micros,
};
use crate::turn::Usage;

const MAX_GOAL_BYTES: usize = 64 * 1024;
const MAX_SCREENSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_ACCOUNT_SCOPE_BYTES: usize = 512;
const MAX_OBSERVATION_TEXT_BYTES: usize = 16 * 1024;
const COMPUTER_SYSTEM_PROMPT: &str = concat!(
    "Use only the OpenAI computer tool. The first request has no image: request exactly one screenshot. ",
    "After a screenshot arrives, return an ordered batch of computer actions for the host to review and execute. ",
    "Do not claim that actions ran, do not request credentials, and do not use other tools."
);

/// A model result is either a request for a screenshot or an ordered proposal.
/// Both variants are observations; neither grants Device authority.
#[derive(Debug, Clone, PartialEq)]
pub enum ComputerModelStep {
    ScreenshotRequested {
        fence: ComputerFence,
        call_id: String,
        pending_safety_checks: Vec<ComputerSafetyCheck>,
        usage: Usage,
    },
    ActionsProposed {
        proposal: ComputerBatchProposal,
        call_id: String,
        pending_safety_checks: Vec<ComputerSafetyCheck>,
        usage: Usage,
    },
    /// A terminal model observation without requested input; it says nothing
    /// about whether the root task succeeded.
    FinishedObservation {
        fence: ComputerFence,
        text: String,
        usage: Usage,
    },
}

/// Per-action host execution outcome. The caller reports these only after it
/// has completed the corresponding action through its separately authorized
/// Device path. This adapter cannot verify that claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerActionOutcome {
    pub index: usize,
    pub completed: bool,
}

/// Explicit completion report tied to one exact proposal fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerBatchOutcome {
    pub fence: ComputerFence,
    pub actions: Vec<ComputerActionOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingPhase {
    InitialScreenshot,
    FreshScreenshot,
    Actions { outcome_recorded: bool },
}

struct PendingCall {
    call: OpenAiComputerCall,
    continuation: ContinuationRef,
    fence: ComputerFence,
    frame: Option<ComputerFrameRequest>,
    capture_after_ms: u64,
    phase: PendingPhase,
    proposal: Option<ComputerBatchProposal>,
}

enum LedgerState {
    New,
    InFlight,
    Pending(Box<PendingCall>),
    Finished,
    /// The previous root's pending call and continuation have been dropped.
    /// A fresh begin is still gated until the Worker explicitly resets it.
    ScopeClosed,
    Poisoned,
    Revoked,
}

struct CompletedResponse {
    call: Option<OpenAiComputerCall>,
    continuation: Option<ContinuationRef>,
    usage: Usage,
    observation_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComputerFailureKind {
    Invalid,
}

/// One official OpenAI Responses route with one outstanding native call.
///
/// Network I/O is injected through the SDK transport for deterministic tests;
/// production construction uses the SDK's HTTP transport. The returned action
/// batch remains inert until the host's independent Device checks admit it.
pub struct OpenAiComputerTransport {
    route: ModelRoute,
    account_scope: String,
    model: Arc<ModelRuntime>,
    network: Arc<dyn Transport>,
    credentials: Arc<EphemeralCredential>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    state: LedgerState,
    last_usage: Usage,
    last_failure: Option<ComputerFailureKind>,
}

impl OpenAiComputerTransport {
    /// Construct the isolated Responses profile. The account scope is stable,
    /// non-secret identity used only to pin SDK continuations.
    pub fn new(
        route: ModelRoute,
        secret: String,
        account_scope: String,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Result<Self, TransportError> {
        let network = HttpTransport::with_client_configurator(|client| client.no_proxy())
            .map_err(|_| TransportError("could not initialize model transport".into()))?;
        Self::with_transport(route, secret, account_scope, clock, Arc::new(network))
    }

    /// Construct with the pinned SDK's transport interface for isolated tests.
    pub fn with_transport(
        route: ModelRoute,
        secret: String,
        account_scope: String,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
        network: Arc<dyn Transport>,
    ) -> Result<Self, TransportError> {
        if !matches!(route.provider_id, ModelProvider::OpenAi)
            || route.protocol != ModelProtocol::OpenAiResponses
            || !is_official_openai_responses_endpoint(&route.base_url)
        {
            return Err(TransportError(
                "computer use requires the official OpenAI Responses endpoint".into(),
            ));
        }
        if account_scope.trim().is_empty()
            || account_scope.len() > MAX_ACCOUNT_SCOPE_BYTES
            || account_scope.chars().any(char::is_control)
        {
            return Err(TransportError("invalid computer account scope".into()));
        }
        let credentials =
            EphemeralCredential::new(route.clone(), secret, COMPUTER_PROFILE, COMPUTER_CREDENTIAL)?;
        let model = Arc::new(route.computer_runtime(credentials.clone())?);
        Ok(Self {
            route,
            account_scope,
            model,
            network: bounded_transport(network, credentials.clone()),
            credentials,
            clock,
            state: LedgerState::New,
            last_usage: Usage::default(),
            last_failure: None,
        })
    }

    /// Start with goal text and a host-assigned scope fence. No image bytes are
    /// sent or cached until the model explicitly asks for a screenshot.
    pub async fn begin(
        &mut self,
        fence: ComputerFence,
        goal: &str,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<ComputerModelStep, TransportError> {
        self.last_usage = Usage::default();
        if !matches!(self.state, LedgerState::New) {
            return Err(TransportError("computer ledger is not fresh".into()));
        }
        fence
            .validate()
            .map_err(|_| TransportError("computer fence is invalid".into()))?;
        if goal.trim().is_empty() || goal.len() > MAX_GOAL_BYTES || goal.contains('\0') {
            return Err(TransportError(
                "computer goal is empty or exceeds quota".into(),
            ));
        }
        self.ensure_live(&fence, cancel)?;
        let request = self.initial_request(goal);
        self.state = LedgerState::InFlight;
        let response = self.execute(request, &fence, cancel).await;
        match response {
            Ok(response) if response.call.is_none() => {
                self.state = LedgerState::Finished;
                Ok(ComputerModelStep::FinishedObservation {
                    fence,
                    text: response.observation_text,
                    usage: response.usage,
                })
            }
            Ok(response) => match classify_call(response, &fence, None, true, (self.clock)()) {
                Ok((pending, step)) => {
                    self.state = LedgerState::Pending(Box::new(pending));
                    Ok(step)
                }
                Err(error) => {
                    self.mark_invalid();
                    self.poison();
                    Err(error)
                }
            },
            Err(error) => {
                self.poison();
                Err(error)
            }
        }
    }

    /// Send a newly captured frame only after a terminal screenshot-only call.
    /// The caller must pass Host FrameTracker freshness/deadline checks first;
    /// this adapter independently rejects future or over-age source pixels.
    pub async fn submit_initial_screenshot(
        &mut self,
        frame: ComputerFrameRequest,
        original_png_data_uri: String,
        acknowledged_safety_checks: Vec<ComputerSafetyCheck>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<ComputerModelStep, TransportError> {
        self.last_usage = Usage::default();
        let pending = match std::mem::replace(&mut self.state, LedgerState::Poisoned) {
            LedgerState::Pending(pending) if pending.phase == PendingPhase::InitialScreenshot => {
                pending
            }
            state => {
                self.state = state;
                return Err(TransportError(
                    "no initial screenshot call is pending".into(),
                ));
            }
        };
        if validate_frame(&frame, (self.clock)()).is_err()
            || validate_frame_after_initial_fence(&pending.fence, &frame).is_err()
            || frame.expected_frame.source_ms <= pending.capture_after_ms
            || validate_png_data_uri(&frame.expected_frame, &original_png_data_uri).is_err()
        {
            self.mark_invalid();
            self.poison();
            return Err(TransportError(
                "initial computer screenshot is stale or mismatched to its frame".into(),
            ));
        }
        if let Err(error) = validate_acknowledgements(&pending.call, &acknowledged_safety_checks) {
            self.mark_invalid();
            self.state = LedgerState::Pending(pending);
            return Err(error);
        }
        self.ensure_live(&frame.fence, cancel)?;
        let fence = frame.fence.clone();
        let request = match self.continuation_request(
            *pending,
            original_png_data_uri,
            acknowledged_safety_checks,
        ) {
            Ok(request) => request,
            Err(error) => {
                self.mark_invalid();
                return Err(error);
            }
        };
        self.state = LedgerState::InFlight;
        let response = self.execute(request, &fence, cancel).await;
        match response {
            Ok(response) if response.call.is_none() => {
                self.state = LedgerState::Finished;
                Ok(ComputerModelStep::FinishedObservation {
                    fence,
                    text: response.observation_text,
                    usage: response.usage,
                })
            }
            Ok(response) => {
                match classify_call(response, &fence, Some(&frame), false, (self.clock)()) {
                    Ok((pending, step)) => {
                        self.state = LedgerState::Pending(Box::new(pending));
                        Ok(step)
                    }
                    Err(error) => {
                        self.mark_invalid();
                        self.poison();
                        Err(error)
                    }
                }
            }
            Err(error) => {
                self.poison();
                Err(error)
            }
        }
    }

    /// Record a complete host-side action outcome. Every action index must be
    /// accounted for in order and marked complete; partial batches cannot be
    /// continued with a screenshot.
    pub fn record_batch_outcome(
        &mut self,
        proposal: &ComputerBatchProposal,
        outcome: ComputerBatchOutcome,
    ) -> Result<(), TransportError> {
        let mut pending = match std::mem::replace(&mut self.state, LedgerState::Poisoned) {
            LedgerState::Pending(pending) => pending,
            state => {
                self.state = state;
                return Err(TransportError("no computer action batch is pending".into()));
            }
        };
        let admissible_phase = matches!(
            pending.phase,
            PendingPhase::Actions {
                outcome_recorded: false
            }
        );
        if !admissible_phase
            || pending.proposal.as_ref() != Some(proposal)
            || outcome.fence != proposal.fence
            || outcome.actions.len() != proposal.actions.len()
            || outcome
                .actions
                .iter()
                .enumerate()
                .any(|(index, result)| result.index != index || !result.completed)
        {
            self.mark_invalid();
            return Err(TransportError(
                "computer batch outcome must completely match the pending proposal".into(),
            ));
        }
        pending.phase = PendingPhase::Actions {
            outcome_recorded: true,
        };
        pending.capture_after_ms = (self.clock)();
        self.state = LedgerState::Pending(pending);
        Ok(())
    }

    /// Continue after a screenshot-only request or a fully completed batch.
    /// The caller supplies a newly captured frame. A returned screenshot is
    /// always `original`, paired to the stored SDK call and full check list.
    pub async fn submit_fresh_screenshot(
        &mut self,
        frame: ComputerFrameRequest,
        original_png_data_uri: String,
        acknowledged_safety_checks: Vec<ComputerSafetyCheck>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<ComputerModelStep, TransportError> {
        self.last_usage = Usage::default();
        let pending = match std::mem::replace(&mut self.state, LedgerState::Poisoned) {
            LedgerState::Pending(pending) => pending,
            state => {
                self.state = state;
                return Err(TransportError("no computer call is pending".into()));
            }
        };
        let can_continue = match pending.phase {
            PendingPhase::FreshScreenshot => true,
            PendingPhase::Actions {
                outcome_recorded: true,
            } => true,
            PendingPhase::Actions {
                outcome_recorded: false,
            }
            | PendingPhase::InitialScreenshot => false,
        };
        if !can_continue {
            self.mark_invalid();
            self.state = LedgerState::Pending(pending);
            return Err(TransportError(
                "computer action batch requires an explicit complete execution outcome".into(),
            ));
        }
        if validate_frame(&frame, (self.clock)()).is_err() {
            self.mark_invalid();
            self.poison();
            return Err(TransportError(
                "computer continuation frame is stale or mismatched".into(),
            ));
        }
        let frame_check = match pending.frame.as_ref() {
            Some(previous) => validate_fresh_frame(previous, &frame, pending.capture_after_ms),
            None => Err(TransportError(
                "pending computer call has no source frame".into(),
            )),
        };
        if frame_check.is_err()
            || validate_png_data_uri(&frame.expected_frame, &original_png_data_uri).is_err()
        {
            self.mark_invalid();
            self.poison();
            return Err(TransportError(
                "computer continuation frame is stale or mismatched".into(),
            ));
        }
        if let Err(error) = validate_acknowledgements(&pending.call, &acknowledged_safety_checks) {
            self.mark_invalid();
            self.state = LedgerState::Pending(pending);
            return Err(error);
        }
        self.ensure_live(&frame.fence, cancel)?;
        let fence = frame.fence.clone();
        let request = match self.continuation_request(
            *pending,
            original_png_data_uri,
            acknowledged_safety_checks,
        ) {
            Ok(request) => request,
            Err(error) => {
                self.mark_invalid();
                return Err(error);
            }
        };
        self.state = LedgerState::InFlight;
        match self.execute(request, &fence, cancel).await {
            Ok(response) if response.call.is_none() => {
                self.state = LedgerState::Finished;
                Ok(ComputerModelStep::FinishedObservation {
                    fence,
                    text: response.observation_text,
                    usage: response.usage,
                })
            }
            Ok(response) => {
                match classify_call(response, &fence, Some(&frame), false, (self.clock)()) {
                    Ok((pending, step)) => {
                        self.state = LedgerState::Pending(Box::new(pending));
                        Ok(step)
                    }
                    Err(error) => {
                        self.mark_invalid();
                        self.poison();
                        Err(error)
                    }
                }
            }
            Err(error) => {
                self.poison();
                Err(error)
            }
        }
    }

    pub fn revoke(&mut self) {
        self.state = LedgerState::Revoked;
        self.credentials.revoke();
    }

    /// Drop every response id, call, proposal, and frame retained for a root.
    /// The worker calls this only after an idle check or its stream-drop poll
    /// barrier. Credentials and the approved route remain attached to this
    /// transport so a later root can start with a new screenshot-first call.
    pub(crate) fn close_scope(&mut self) -> Result<(), TransportError> {
        if matches!(&self.state, LedgerState::InFlight) {
            return Err(TransportError(
                "cannot close a computer scope while a request is in flight".into(),
            ));
        }
        if matches!(&self.state, LedgerState::Revoked) {
            return Ok(());
        }
        self.state = LedgerState::ScopeClosed;
        self.last_usage = Usage::default();
        self.last_failure = None;
        Ok(())
    }

    /// Open an empty ledger for another root without changing the authorized
    /// provider route, account scope, or in-memory credential.
    pub(crate) fn reset_ledger(&mut self) -> Result<(), TransportError> {
        if matches!(&self.state, LedgerState::ScopeClosed) {
            self.state = LedgerState::New;
            self.last_usage = Usage::default();
            return Ok(());
        }
        if matches!(&self.state, LedgerState::Revoked) {
            return Err(TransportError(
                "a revoked computer credential cannot be reset".into(),
            ));
        }
        Err(TransportError(
            "computer ledger reset requires an explicitly closed scope".into(),
        ))
    }

    pub(crate) fn deadline_elapsed(&self, fence: &ComputerFence) -> bool {
        (self.clock)() >= fence.deadline_ms
    }

    /// Usage observed for the latest request, including failed or cancelled
    /// streams. Missing provider usage remains unknown.
    pub fn last_usage(&self) -> Usage {
        self.last_usage
    }

    pub(crate) fn reset_usage(&mut self) {
        self.last_usage = Usage::default();
        self.last_failure = None;
    }

    pub(crate) fn failure_kind(&self) -> Option<ComputerFailureKind> {
        self.last_failure
    }

    fn mark_invalid(&mut self) {
        self.last_failure = Some(ComputerFailureKind::Invalid);
    }

    pub fn identity(&self) -> (&str, &str) {
        ("openai", &self.route.model)
    }

    fn initial_request(&self, goal: &str) -> LlmRequest {
        let mut request = LlmRequest::new(&self.route.model)
            .with_profile(COMPUTER_PROFILE)
            .with_user_text(goal.to_owned());
        request.stream = true;
        request.input.max_tokens = Some(self.route.max_output_tokens);
        request.input.system = vec![SystemBlock {
            text: COMPUTER_SYSTEM_PROMPT.into(),
        }];
        request
            .input
            .set_openai_computer_tool(Some(OpenAiComputerToolConfig {}));
        request.execution.account_scope = Some(self.account_scope.clone());
        request
    }

    fn continuation_request(
        &self,
        pending: PendingCall,
        image_url: String,
        acknowledged_safety_checks: Vec<ComputerSafetyCheck>,
    ) -> Result<LlmRequest, TransportError> {
        let output = OpenAiComputerCallOutput {
            r#type: OpenAiComputerCallOutputType::ComputerCallOutput,
            call_id: pending.call.call_id.clone(),
            output: ComputerScreenshot {
                r#type: ComputerScreenshotType::ComputerScreenshot,
                file_id: None,
                image_url: Some(image_url),
                detail: Some(ComputerScreenshotDetail::Original),
            },
            id: None,
            created_by: None,
            status: None,
            acknowledged_safety_checks,
        };
        if output.acknowledged_safety_checks != pending.call.pending_safety_checks {
            return Err(TransportError(
                "all reported computer safety checks require exact acknowledgement".into(),
            ));
        }
        let block = output
            .into_content_block_for(&pending.call)
            .map_err(|_| TransportError("computer screenshot output is invalid".into()))?;
        let mut request = LlmRequest::new(&self.route.model).with_profile(COMPUTER_PROFILE);
        request.stream = true;
        request.input.max_tokens = Some(self.route.max_output_tokens);
        request.input.system = vec![SystemBlock {
            text: COMPUTER_SYSTEM_PROMPT.into(),
        }];
        request
            .input
            .set_openai_computer_tool(Some(OpenAiComputerToolConfig {}));
        request.input.continuation = Some(pending.continuation);
        request.input.messages = vec![ConversationMessage {
            role: MessageRole::User,
            content: vec![block],
            native_options: Vec::new(),
        }];
        request.execution.account_scope = Some(self.account_scope.clone());
        Ok(request)
    }

    async fn execute(
        &mut self,
        request: LlmRequest,
        fence: &ComputerFence,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<CompletedResponse, TransportError> {
        self.ensure_live(fence, cancel)?;
        let remaining = fence.deadline_ms.saturating_sub((self.clock)());
        let deadline = tokio::time::sleep(Duration::from_millis(remaining));
        tokio::pin!(deadline);
        let mut stream = tokio::select! {
            biased;
            changed = cancel.changed() => {
                let _ = changed;
                return Err(TransportError("computer model request cancelled".into()));
            }
            _ = &mut deadline => return Err(TransportError("computer model deadline elapsed".into())),
            result = self.model.execute_stream(&request, self.network.clone()) => {
                result.map_err(|_| TransportError("computer model request failed".into()))?
            }
        };
        let mut calls = Vec::new();
        let mut observation_text = String::new();
        let mut terminal = false;
        let mut terminal_reason = None;
        let report = stream.usage_report();
        self.last_usage = usage_from_report(&report);
        loop {
            let batch = tokio::select! {
                biased;
                changed = cancel.changed() => {
                    let _ = changed;
                    return Err(TransportError("computer model request cancelled".into()));
                }
                _ = &mut deadline => return Err(TransportError("computer model deadline elapsed".into())),
                batch = stream.next_batch() => batch,
            };
            let Some(batch) = batch else {
                break;
            };
            let report = batch.usage;
            let inference = batch.inference;
            let mut usage = usage_from_report(&report);
            usage.cost_micros = stream.pricing_snapshot().and_then(|pricing| {
                quote_cost_micros(
                    pricing,
                    &report,
                    &inference,
                    &pricing_model_ref(&self.route),
                )
            });
            // Account for this batch before validating its events: malformed
            // generated output does not undo a provider's reported usage.
            self.last_usage = usage;
            for event in batch.events {
                let event = event.map_err(|_| {
                    TransportError("computer model response stream was interrupted".into())
                })?;
                match event {
                    StreamEvent::Native { value, .. } => {
                        if value.format() != OPENAI_COMPUTER_CALL_FORMAT || !calls.is_empty() {
                            self.mark_invalid();
                            return Err(TransportError(
                                "computer response contains an unknown or duplicate native item"
                                    .into(),
                            ));
                        }
                        let call = match value.decode::<OpenAiComputerCall>() {
                            Ok(call) => call,
                            Err(_) => {
                                self.mark_invalid();
                                return Err(TransportError(
                                    "computer response native call is invalid".into(),
                                ));
                            }
                        };
                        if call.validate_completed_generation().is_err() {
                            self.mark_invalid();
                            return Err(TransportError(
                                "computer response call is incomplete".into(),
                            ));
                        }
                        calls.push(call.clone());
                    }
                    StreamEvent::End { stop_reason, .. } => {
                        if terminal {
                            self.mark_invalid();
                            return Err(TransportError(
                                "computer response has duplicate terminal events".into(),
                            ));
                        }
                        terminal = true;
                        terminal_reason = Some(stop_reason);
                    }
                    StreamEvent::Start { .. }
                    | StreamEvent::Inference { .. }
                    | StreamEvent::BlockEnd { .. } => {}
                    StreamEvent::TextDelta { text, .. } => {
                        push_bounded_text(&mut observation_text, &text);
                    }
                    StreamEvent::ReasoningDelta { .. }
                    | StreamEvent::ThoughtSignature { .. }
                    | StreamEvent::RedactedThinking { .. }
                    | StreamEvent::ProviderEvent { .. } => {}
                    StreamEvent::ProviderContent { value, .. }
                        if value.get("type").and_then(Value::as_str) == Some("reasoning") => {}
                    StreamEvent::NativeControl { .. }
                    | StreamEvent::TextDeltaJsUtf16 { .. }
                    | StreamEvent::NativeDelta { .. }
                    | StreamEvent::ProviderContent { .. }
                    | StreamEvent::ToolCallDelta { .. }
                    | StreamEvent::WebSearch { .. }
                    | StreamEvent::FileSearch { .. } => {
                        self.mark_invalid();
                        return Err(TransportError(
                            "computer response contains an unknown native item or another tool"
                                .into(),
                        ));
                    }
                }
            }
        }
        if (self.clock)() >= fence.deadline_ms || *cancel.borrow() {
            return Err(TransportError(
                "computer model request expired or cancelled".into(),
            ));
        }
        if !terminal {
            self.mark_invalid();
            return Err(TransportError(
                "computer response has no complete terminal event".into(),
            ));
        }
        let (call, continuation) = match (terminal_reason, calls.len()) {
            (Some(StopReason::ToolUse), 1) => {
                let continuation = match stream.continuation().cloned() {
                    Some(continuation) => continuation,
                    None => {
                        self.mark_invalid();
                        return Err(TransportError(
                            "computer response has no scoped continuation".into(),
                        ));
                    }
                };
                if let Err(error) = self.validate_continuation(&continuation) {
                    self.mark_invalid();
                    return Err(error);
                }
                (calls.pop(), Some(continuation))
            }
            (Some(StopReason::EndTurn), 0) => (None, None),
            _ => {
                self.mark_invalid();
                return Err(TransportError(
                    "computer response must be one typed call or a completed observation".into(),
                ));
            }
        };
        let usage = self.last_usage;
        drop(stream);
        Ok(CompletedResponse {
            call,
            continuation,
            usage,
            observation_text,
        })
    }

    fn validate_continuation(&self, continuation: &ContinuationRef) -> Result<(), TransportError> {
        if continuation.provider_id.as_str() != "openai"
            || continuation.profile_name != COMPUTER_PROFILE
            || continuation.request_model != self.route.model
            || continuation.account_scope != self.account_scope
            || continuation.workspace_id.is_some()
            || continuation.endpoint_fingerprint.is_empty()
        {
            return Err(TransportError(
                "computer continuation does not match its original route and account".into(),
            ));
        }
        Ok(())
    }

    fn ensure_live(
        &self,
        fence: &ComputerFence,
        cancel: &watch::Receiver<bool>,
    ) -> Result<(), TransportError> {
        fence
            .validate()
            .map_err(|_| TransportError("computer fence is invalid".into()))?;
        if self.revoked() || *cancel.borrow() || (self.clock)() >= fence.deadline_ms {
            return Err(TransportError(
                "computer request is cancelled, revoked, or expired".into(),
            ));
        }
        Ok(())
    }

    fn revoked(&self) -> bool {
        matches!(self.state, LedgerState::Revoked)
    }

    fn poison(&mut self) {
        self.state = LedgerState::Poisoned;
    }
}

fn classify_call(
    response: CompletedResponse,
    fence: &ComputerFence,
    frame: Option<&ComputerFrameRequest>,
    first: bool,
    capture_after_ms: u64,
) -> Result<(PendingCall, ComputerModelStep), TransportError> {
    let call = response
        .call
        .ok_or_else(|| TransportError("computer response did not contain a call".into()))?;
    let continuation = response
        .continuation
        .ok_or_else(|| TransportError("computer response has no scoped continuation".into()))?;
    let screenshot_only =
        call.actions.len() == 1 && matches!(call.actions.first(), Some(ComputerAction::Screenshot));
    if first && !screenshot_only {
        return Err(TransportError(
            "first computer response must request exactly one screenshot".into(),
        ));
    }
    let phase = if first {
        PendingPhase::InitialScreenshot
    } else if screenshot_only {
        PendingPhase::FreshScreenshot
    } else {
        PendingPhase::Actions {
            outcome_recorded: false,
        }
    };
    let (step, proposal) = if screenshot_only {
        (
            ComputerModelStep::ScreenshotRequested {
                fence: fence.clone(),
                call_id: call.call_id.clone(),
                pending_safety_checks: call.pending_safety_checks.clone(),
                usage: response.usage,
            },
            None,
        )
    } else {
        if frame.is_none() {
            return Err(TransportError(
                "computer action batch has no source frame".into(),
            ));
        }
        let actions = call
            .actions
            .iter()
            .map(map_action)
            .collect::<Result<Vec<_>, _>>()?;
        let proposal = ComputerBatchProposal {
            fence: fence.clone(),
            actions,
        };
        proposal
            .validate()
            .map_err(|_| TransportError("computer action proposal is invalid".into()))?;
        (
            ComputerModelStep::ActionsProposed {
                proposal: proposal.clone(),
                call_id: call.call_id.clone(),
                pending_safety_checks: call.pending_safety_checks.clone(),
                usage: response.usage,
            },
            Some(proposal),
        )
    };
    Ok((
        PendingCall {
            call,
            continuation,
            fence: fence.clone(),
            frame: frame.cloned(),
            capture_after_ms,
            phase,
            proposal,
        },
        step,
    ))
}

fn map_action(action: &ComputerAction) -> Result<ComputerActionProposal, TransportError> {
    let mapped = match action {
        ComputerAction::Click { button, x, y, keys } => ComputerActionProposal::Click {
            button: match button {
                SdkComputerMouseButton::Left => ComputerMouseButton::Left,
                SdkComputerMouseButton::Right => ComputerMouseButton::Right,
                SdkComputerMouseButton::Wheel => ComputerMouseButton::Wheel,
                SdkComputerMouseButton::Back => ComputerMouseButton::Back,
                SdkComputerMouseButton::Forward => ComputerMouseButton::Forward,
            },
            x: *x,
            y: *y,
            keys: keys.clone(),
        },
        ComputerAction::DoubleClick { x, y, keys } => ComputerActionProposal::DoubleClick {
            x: *x,
            y: *y,
            keys: keys.clone(),
        },
        ComputerAction::Drag { path, keys } => ComputerActionProposal::Drag {
            path: path
                .iter()
                .map(|point| ComputerPoint {
                    x: point.x,
                    y: point.y,
                })
                .collect(),
            keys: keys.clone(),
        },
        ComputerAction::Move { x, y, keys } => ComputerActionProposal::Move {
            x: *x,
            y: *y,
            keys: keys.clone(),
        },
        ComputerAction::Scroll {
            scroll_x,
            scroll_y,
            x,
            y,
            keys,
        } => ComputerActionProposal::Scroll {
            scroll_x: *scroll_x,
            scroll_y: *scroll_y,
            x: *x,
            y: *y,
            keys: keys.clone(),
        },
        ComputerAction::Keypress { keys } => {
            ComputerActionProposal::Keypress { keys: keys.clone() }
        }
        ComputerAction::Type { text } => ComputerActionProposal::Type { text: text.clone() },
        ComputerAction::Wait => ComputerActionProposal::Wait,
        ComputerAction::Screenshot => ComputerActionProposal::Screenshot,
    };
    mapped
        .validate()
        .map_err(|_| TransportError("computer action is outside provider-neutral bounds".into()))?;
    Ok(mapped)
}

fn validate_acknowledgements(
    call: &OpenAiComputerCall,
    acknowledged: &[ComputerSafetyCheck],
) -> Result<(), TransportError> {
    if acknowledged != call.pending_safety_checks {
        return Err(TransportError(
            "all pending computer safety checks require exact explicit acknowledgement".into(),
        ));
    }
    Ok(())
}

fn validate_frame(frame: &ComputerFrameRequest, now_ms: u64) -> Result<(), TransportError> {
    frame
        .validate()
        .map_err(|_| TransportError("computer frame request is invalid or incomplete".into()))?;
    let pixels = &frame.expected_frame;
    if pixels.width_px == 0
        || pixels.height_px == 0
        || !pixels.scale_factor.is_finite()
        || pixels.scale_factor <= 0.0
        || pixels.byte_length == 0
        || pixels.checksum.len() != 64
        || !pixels
            .checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || pixels.source_ms > now_ms
        || now_ms.saturating_sub(pixels.source_ms) > 2_000
        || pixels
            .stream_info()
            .is_some_and(|stream| stream.callback_ms > now_ms)
        || pixels
            .capture_info()
            .is_some_and(|capture| capture.capture_finished_ms > now_ms)
    {
        return Err(TransportError("computer frame metadata is invalid".into()));
    }
    Ok(())
}

fn validate_frame_after_initial_fence(
    previous: &ComputerFence,
    next: &ComputerFrameRequest,
) -> Result<(), TransportError> {
    let after = &next.fence;
    if after.root_task_id != previous.root_task_id
        || after.task_revision != previous.task_revision
        || after.root_epoch != previous.root_epoch
        || after.request_id == previous.request_id
        || after.generation <= previous.generation
        || after.snapshot_id == previous.snapshot_id
        || after.structure_sequence <= previous.structure_sequence
        || after.deadline_ms > previous.deadline_ms
    {
        return Err(TransportError(
            "initial screenshot fence is stale or crosses its task lifetime".into(),
        ));
    }
    Ok(())
}

fn validate_fresh_frame(
    previous: &ComputerFrameRequest,
    next: &ComputerFrameRequest,
    capture_after_ms: u64,
) -> Result<(), TransportError> {
    let before = &previous.fence;
    let after = &next.fence;
    let before_frame = &previous.expected_frame;
    let after_frame = &next.expected_frame;
    let before_geometry = before_frame
        .window_geometry()
        .ok_or_else(|| TransportError("previous frame has no native geometry".into()))?;
    let after_geometry = after_frame
        .window_geometry()
        .ok_or_else(|| TransportError("next frame has no native geometry".into()))?;
    let stream_time_went_back = match (before_frame.stream_info(), after_frame.stream_info()) {
        (Some(before), Some(after)) => {
            after.display_time_ticks <= before.display_time_ticks
                || after.display_time_ns <= before.display_time_ns
                || after.received_uptime_ns <= before.received_uptime_ns
        }
        _ => false,
    };
    if after.root_task_id != before.root_task_id
        || after.task_revision != before.task_revision
        || after.root_epoch != before.root_epoch
        || after.request_id == before.request_id
        || after.generation <= before.generation
        || after.snapshot_id == before.snapshot_id
        || after.structure_sequence <= before.structure_sequence
        || after_frame.session_epoch != before_frame.session_epoch
        || after_frame.source_sequence <= before_frame.source_sequence
        || after_frame.frame_id == before_frame.frame_id
        || after_frame.surface != before_frame.surface
        || after_frame.clock_domain != before_frame.clock_domain
        || after_frame.geometry_revision != before_frame.geometry_revision
        || after_frame.width_px != before_frame.width_px
        || after_frame.height_px != before_frame.height_px
        || after_frame.scale_factor != before_frame.scale_factor
        || after_frame.source_ms <= before_frame.source_ms
        || after_frame.source_ms <= capture_after_ms
        || before_frame.watch_id().is_none()
        || after_frame.watch_id() != before_frame.watch_id()
        || after_geometry != before_geometry
        || after_frame.image_frame_id() == before_frame.image_frame_id()
        || stream_time_went_back
        || after.deadline_ms > before.deadline_ms
    {
        return Err(TransportError(
            "computer continuation requires a fresh frame in the same task and source session"
                .into(),
        ));
    }
    Ok(())
}

fn validate_png_data_uri(frame: &FrameMeta, image_url: &str) -> Result<(), TransportError> {
    let Some(payload) = image_url.strip_prefix("data:image/png;base64,") else {
        return Err(TransportError(
            "computer screenshot must be original PNG data".into(),
        ));
    };
    if image_url.len() > MAX_COMPUTER_SCREENSHOT_URL_BYTES
        || image_url.chars().any(char::is_control)
        || payload.is_empty()
    {
        return Err(TransportError(
            "computer screenshot exceeds quota or is malformed".into(),
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| TransportError("computer screenshot base64 is malformed".into()))?;
    if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(TransportError(
            "computer screenshot exceeds the PNG byte limit".into(),
        ));
    }
    desktop_ipc::image::validate_png(frame, &bytes).map_err(|_| {
        TransportError("computer screenshot does not match its complete PNG frame metadata".into())
    })?;
    Ok(())
}

fn push_bounded_text(buffer: &mut String, value: &str) {
    let remaining = MAX_OBSERVATION_TEXT_BYTES.saturating_sub(buffer.len());
    if remaining == 0 {
        return;
    }
    let mut end = value.len().min(remaining);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    buffer.push_str(&value[..end]);
}

fn usage_from_report(report: &lingxi_llm_client::protocol::UsageReport) -> Usage {
    let Some(usage) = report.complete() else {
        return Usage::default();
    };
    let input_tokens = usage
        .input_tokens
        .checked_add(usage.cache_read_tokens)
        .and_then(|count| count.checked_add(usage.cache_write_tokens));
    Usage {
        input_tokens,
        output_tokens: Some(usage.output_tokens),
        cost_micros: None,
    }
}
