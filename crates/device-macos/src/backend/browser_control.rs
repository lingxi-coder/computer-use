//! Single-operation adapter from Core browser authority to Main's private fd3
//! resource executor. This module has no generic CDP or native input path.

use std::io::Read;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use desktop_ipc::browser_control::{BrowserControlSession, read_reply, write_submit};
use execution_core::backend::{
    BrowserSubmitContext, HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure,
    SubmitCompletion, SubmitOutcome,
};
use interaction_contracts::desktop::browser::{
    BrowserPageProfile, BrowserReadScope, MAX_BROWSER_SAFE_INTEGER,
};
use interaction_contracts::desktop::browser_control::{
    BROWSER_CONTROL_VERSION, BrowserControlAuthority, BrowserControlOutcome, BrowserControlReply,
    BrowserControlSubmit,
};
use interaction_contracts::desktop::browser_execution::BrowserAction;
use interaction_contracts::desktop::device::PermitRef;
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::primitive::Primitive;

const IDLE_PROBE: Duration = Duration::from_millis(5);
const MAX_REPLY_WAIT: Duration = Duration::from_secs(30);

enum WorkerEvent {
    Reply(BrowserControlReply),
    TransportLost,
}

struct PendingRequest {
    request: BrowserControlSubmit,
    sequence: u64,
    session_epoch: u64,
    intent_id: String,
}

/// Device-owned browser transport. The authenticated session is single-use;
/// any uncertain action or transport retirement permanently disables this
/// scope for the lifetime of this process.
pub struct BrowserControlBackend {
    profile: BrowserPageProfile,
    scope: Box<BrowserReadScope>,
    channel_generation: u64,
    next_sequence: u64,
    commands: Option<SyncSender<BrowserControlSubmit>>,
    events: Receiver<WorkerEvent>,
    worker: Option<JoinHandle<()>>,
    shutdown_stream: Option<UnixStream>,
    ready: Arc<AtomicBool>,
    pending: Option<PendingRequest>,
}

impl BrowserControlBackend {
    pub fn new(session: BrowserControlSession) -> Result<Self, String> {
        let (stream, profile, scope, channel_generation, next_sequence) = session.into_parts();
        if profile != BrowserPageProfile::FixtureBackgroundOption
            || scope.validate().is_err()
            || next_sequence != 1
        {
            return Err("private browser control session is invalid".into());
        }
        let shutdown_stream = stream
            .try_clone()
            .map_err(|_| "private browser control session could not be monitored".to_string())?;
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        // The worker exits after a terminal transport event and there is only
        // one pending request, so this channel can hold at most its reply plus
        // one idle-close notice without ever blocking synchronous retirement.
        let (event_tx, event_rx) = mpsc::channel();
        let ready = Arc::new(AtomicBool::new(true));
        let worker_ready = Arc::clone(&ready);
        let worker = thread::Builder::new()
            .name("device-browser-control".into())
            .spawn(move || control_worker(stream, command_rx, event_tx, worker_ready))
            .map_err(|_| "private browser control worker could not start".to_string())?;
        Ok(Self {
            profile,
            scope,
            channel_generation,
            next_sequence,
            commands: Some(command_tx),
            events: event_rx,
            worker: Some(worker),
            shutdown_stream: Some(shutdown_stream),
            ready,
            pending: None,
        })
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    fn supported(&self, action: &BrowserAction, context: &BrowserSubmitContext<'_>) -> bool {
        self.ready()
            && self.pending.is_none()
            && self.profile == BrowserPageProfile::FixtureBackgroundOption
            && self.scope.as_ref() == context.intent.evidence.scope.as_ref()
            && self.scope.as_ref() == context.authority.evidence.scope.as_ref()
            && context.intent.session_epoch == self.scope.context.session_epoch
            && context.intent.action == *action
            && context.intent.validate().is_ok()
            && context.authority.evidence.validate().is_ok()
    }

    fn retire_local(&mut self) {
        self.ready.store(false, Ordering::Release);
        self.commands.take();
        if let Some(stream) = self.shutdown_stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn join_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    fn unknown_completion(&self, pending: PendingRequest) -> SubmitCompletion {
        SubmitCompletion {
            sequence: pending.sequence,
            session_epoch: pending.session_epoch,
            intent_id: pending.intent_id,
            outcome: SubmitOutcome::Unknown,
            submitted_at_ms: None,
            input_end_ms: None,
            failure_code: Some(FailureCode::EffectUnknown),
        }
    }

    fn completion_from_reply(&mut self, reply: BrowserControlReply) -> Option<SubmitCompletion> {
        let pending = self.pending.take()?;
        if reply.validate_against(&pending.request).is_err() {
            self.retire_local();
            return Some(self.unknown_completion(pending));
        }
        let outcome = match reply.outcome {
            BrowserControlOutcome::Completed => SubmitOutcome::Applied,
            BrowserControlOutcome::Rejected => SubmitOutcome::Rejected,
            BrowserControlOutcome::Unknown => {
                self.retire_local();
                SubmitOutcome::Unknown
            }
        };
        Some(SubmitCompletion {
            sequence: pending.sequence,
            session_epoch: pending.session_epoch,
            intent_id: pending.intent_id,
            outcome,
            submitted_at_ms: reply.submitted_at_ms,
            input_end_ms: reply.input_end_ms,
            failure_code: if outcome == SubmitOutcome::Unknown {
                Some(reply.failure_code.unwrap_or(FailureCode::EffectUnknown))
            } else {
                reply.failure_code
            },
        })
    }
}

fn control_worker(
    mut stream: UnixStream,
    commands: Receiver<BrowserControlSubmit>,
    events: Sender<WorkerEvent>,
    ready: Arc<AtomicBool>,
) {
    loop {
        match commands.try_recv() {
            Ok(submit) => {
                if write_submit(&mut stream, &submit).is_err() {
                    ready.store(false, Ordering::Release);
                    let _ = stream.shutdown(Shutdown::Both);
                    let _ = events.send(WorkerEvent::TransportLost);
                    return;
                }
                if stream.set_read_timeout(Some(MAX_REPLY_WAIT)).is_err() {
                    ready.store(false, Ordering::Release);
                    let _ = stream.shutdown(Shutdown::Both);
                    let _ = events.send(WorkerEvent::TransportLost);
                    return;
                }
                match read_reply(&mut stream, &submit) {
                    Ok(reply) => {
                        let uncertain = reply.outcome == BrowserControlOutcome::Unknown;
                        if uncertain || stream.set_read_timeout(Some(IDLE_PROBE)).is_err() {
                            ready.store(false, Ordering::Release);
                            let _ = stream.shutdown(Shutdown::Both);
                        }
                        if events.send(WorkerEvent::Reply(reply)).is_err() || uncertain {
                            return;
                        }
                    }
                    Err(_) => {
                        ready.store(false, Ordering::Release);
                        let _ = stream.shutdown(Shutdown::Both);
                        let _ = events.send(WorkerEvent::TransportLost);
                        return;
                    }
                }
            }
            Err(TryRecvError::Disconnected) => {
                ready.store(false, Ordering::Release);
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }
            Err(TryRecvError::Empty) => {
                if stream.set_read_timeout(Some(IDLE_PROBE)).is_err() {
                    ready.store(false, Ordering::Release);
                    let _ = events.send(WorkerEvent::TransportLost);
                    return;
                }
                let mut unexpected = [0; 1];
                match stream.read(&mut unexpected) {
                    Ok(0) => {
                        ready.store(false, Ordering::Release);
                        let _ = events.send(WorkerEvent::TransportLost);
                        return;
                    }
                    Ok(_) => {
                        // No peer message is valid while Device has no request
                        // outstanding. Consume nothing further and retire.
                        ready.store(false, Ordering::Release);
                        let _ = stream.shutdown(Shutdown::Both);
                        let _ = events.send(WorkerEvent::TransportLost);
                        return;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        ready.store(false, Ordering::Release);
                        let _ = events.send(WorkerEvent::TransportLost);
                        return;
                    }
                }
            }
        }
    }
}

impl InputBackend for BrowserControlBackend {
    fn submit(
        &mut self,
        _at_ms: u64,
        _primitive: &Primitive,
        _context: &execution_core::backend::SubmitContext<'_>,
    ) -> SubmitOutcome {
        // Browser work cannot fall through to a native primitive.
        SubmitOutcome::Rejected
    }

    fn submit_browser(
        &mut self,
        at_ms: u64,
        action: &BrowserAction,
        context: &BrowserSubmitContext<'_>,
    ) -> SubmitOutcome {
        if !self.supported(action, context)
            || self.next_sequence == 0
            || self.next_sequence > MAX_BROWSER_SAFE_INTEGER
        {
            return SubmitOutcome::Rejected;
        }
        let Some(next_sequence) = self.next_sequence.checked_add(1) else {
            self.retire_local();
            return SubmitOutcome::Rejected;
        };
        let Ok(request_id) = request_id() else {
            return SubmitOutcome::Rejected;
        };
        let command_start_ms = context
            .authority
            .command_end_ms
            .saturating_sub(context.intent.duration_ms);
        let reply_deadline_ms = context
            .authority
            .valid_until_ms
            .saturating_add(500)
            .min(MAX_BROWSER_SAFE_INTEGER);
        let request = BrowserControlSubmit {
            version: BROWSER_CONTROL_VERSION,
            request_id,
            channel_generation: self.channel_generation,
            session_epoch: context.intent.session_epoch,
            scope_id: self.scope.scope_id.clone(),
            scope_generation: self.scope.generation,
            sequence: self.next_sequence,
            intent: Box::new(context.intent.clone()),
            authority: Box::new(BrowserControlAuthority {
                task_id: context.intent.task_id.clone(),
                task_revision: context.intent.task_revision,
                root_epoch: context.intent.root_epoch,
                attempt_id: context.intent.attempt_id.clone(),
                permit: context.authority.permit.clone(),
                stop_seq: context.authority.stop_seq,
                command_start_ms,
                command_end_ms: context.authority.command_end_ms,
                valid_until_ms: context.authority.valid_until_ms,
                reply_deadline_ms,
            }),
            authority_evidence: Box::new(context.authority.evidence.clone()),
        };
        if request.validate().is_err()
            || request.authority.command_start_ms > at_ms
            || reply_deadline_ms < context.authority.command_end_ms
        {
            return SubmitOutcome::Rejected;
        }
        let Some(commands) = &self.commands else {
            self.ready.store(false, Ordering::Release);
            return SubmitOutcome::Rejected;
        };
        match commands.try_send(request.clone()) {
            Ok(()) => {
                self.next_sequence = if next_sequence <= MAX_BROWSER_SAFE_INTEGER {
                    next_sequence
                } else {
                    0
                };
                let sequence = request.sequence;
                self.pending = Some(PendingRequest {
                    sequence,
                    session_epoch: request.session_epoch,
                    intent_id: request.intent.intent_id.clone(),
                    request,
                });
                SubmitOutcome::Pending {
                    sequence,
                    reply_deadline_ms,
                }
            }
            Err(TrySendError::Full(_)) => SubmitOutcome::Rejected,
            Err(TrySendError::Disconnected(_)) => {
                self.retire_local();
                SubmitOutcome::Rejected
            }
        }
    }

    fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        self.pending.as_ref()?;
        match self.events.try_recv() {
            Ok(WorkerEvent::Reply(reply)) => self.completion_from_reply(reply),
            Ok(WorkerEvent::TransportLost) | Err(TryRecvError::Disconnected) => {
                let pending = self.pending.take()?;
                self.retire_local();
                Some(self.unknown_completion(pending))
            }
            Err(TryRecvError::Empty) => None,
        }
    }

    fn revoke(&mut self, permit: &PermitRef) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request.authority.permit == *permit)
        {
            // shutdown() is local and synchronous. It does not claim that Main
            // or Chrome cancelled an operation already dispatched.
            self.retire_local();
        }
    }

    fn abandon_pending(&mut self) {
        self.pending = None;
        self.retire_local();
        // The Core may release its in-flight fence only after this local send
        // authority and the worker that owns its stream are both retired.
        self.join_worker();
    }

    fn release(&mut self, _at_ms: u64, _release: &Primitive) -> Result<(), ReleaseFailure> {
        Err(ReleaseFailure)
    }

    fn held(&self, _token: &str) -> HeldState {
        HeldState::Unknown
    }

    fn hit_test(&mut self, _request: &HitTestRequest<'_>) -> HitTestOutcome {
        HitTestOutcome::Unresolved
    }
}

impl Drop for BrowserControlBackend {
    fn drop(&mut self) {
        self.retire_local();
        self.join_worker();
    }
}

fn request_id() -> Result<String, std::io::Error> {
    let random = desktop_ipc::socket::new_token()?;
    Ok(format!(
        "{}-{}-4{}-a{}-{}",
        &random[0..8],
        &random[8..12],
        &random[13..16],
        &random[17..20],
        &random[20..32]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_ipc::browser_control::{
        PendingBrowserControl, read_hello, read_submit, write_bootstrap, write_ready, write_reply,
    };
    use execution_core::backend::BrowserSubmissionAuthority;
    use interaction_contracts::desktop::browser::BrowserPageProfile;
    use interaction_contracts::desktop::browser_control::{
        BrowserControlBootstrap, BrowserControlReady, BrowserControlToken,
    };
    use interaction_contracts::desktop::device::{ExecutionIntent, ExecutionObservation};
    use interaction_contracts::desktop::message::Message;
    use interaction_contracts::desktop::samples;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::thread;

    fn fixture() -> (
        interaction_contracts::desktop::browser_execution::BrowserActionIntent,
        interaction_contracts::desktop::browser_execution::BrowserExecutionObservation,
    ) {
        let messages = samples::representative_messages();
        let intent = messages
            .iter()
            .find_map(|(_, message)| match message {
                Message::SubmitIntent(request) => match &request.intent {
                    ExecutionIntent::Browser { intent } => Some(intent.as_ref().clone()),
                    _ => None,
                },
                _ => None,
            })
            .expect("sample browser intent");
        let evidence = messages
            .iter()
            .find_map(|(_, message)| match message {
                Message::Observation(ExecutionObservation::Browser { observation }) => {
                    Some(observation.as_ref().clone())
                }
                _ => None,
            })
            .expect("sample browser evidence");
        (intent, evidence)
    }

    fn bootstrap(scope: &BrowserReadScope) -> BrowserControlBootstrap {
        BrowserControlBootstrap {
            version: BROWSER_CONTROL_VERSION,
            token: BrowserControlToken::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
            channel_generation: 13,
            browser_page_profile: BrowserPageProfile::FixtureBackgroundOption,
            scope: Box::new(scope.clone()),
        }
    }

    fn establish_channel(
        scope: &BrowserReadScope,
        main_behavior: impl FnOnce(&mut UnixStream) + Send + 'static,
    ) -> (BrowserControlSession, thread::JoinHandle<()>) {
        let (device, mut main) = UnixStream::pair().unwrap();
        let bootstrap = bootstrap(scope);
        let main_bootstrap = bootstrap.clone();
        let server = thread::spawn(move || {
            write_bootstrap(&mut main, &main_bootstrap).unwrap();
            let hello = read_hello(&mut main, &main_bootstrap).unwrap();
            write_ready(
                &mut main,
                &BrowserControlReady {
                    version: BROWSER_CONTROL_VERSION,
                    channel_generation: hello.channel_generation,
                    session_epoch: hello.session_epoch,
                    scope_id: hello.scope_id.clone(),
                    scope_generation: hello.scope_generation,
                    next_sequence: 1,
                },
                &hello,
            )
            .unwrap();
            main_behavior(&mut main);
        });
        let pending = PendingBrowserControl::read_from_stream(device).unwrap();
        let session = pending.finish().unwrap();
        (session, server)
    }

    fn context<'a>(
        intent: &'a interaction_contracts::desktop::browser_execution::BrowserActionIntent,
        evidence: interaction_contracts::desktop::browser_execution::BrowserExecutionObservation,
    ) -> (BrowserAction, BrowserSubmitContext<'a>) {
        let action = intent.action;
        let command_end_ms = intent.latest_start_ms + intent.duration_ms;
        let context = BrowserSubmitContext {
            intent,
            authority: BrowserSubmissionAuthority {
                permit: intent.permit.clone(),
                stop_seq: 0,
                valid_until_ms: intent.evidence.valid_until_ms,
                command_end_ms,
                evidence,
            },
        };
        (action, context)
    }

    fn wait_completion(backend: &mut BrowserControlBackend) -> SubmitCompletion {
        for _ in 0..2_000 {
            if let Some(completion) = backend.poll_completion() {
                return completion;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("browser control completion did not arrive");
    }

    #[test]
    fn one_authenticated_submit_maps_to_its_matching_core_completion() {
        let (intent, evidence) = fixture();
        let (session, server) = establish_channel(&intent.evidence.scope, move |main| {
            let submit = read_submit(main).unwrap();
            let submitted_at_ms = submit.authority.command_start_ms;
            write_reply(
                main,
                &BrowserControlReply {
                    version: BROWSER_CONTROL_VERSION,
                    request_id: submit.request_id.clone(),
                    channel_generation: submit.channel_generation,
                    session_epoch: submit.session_epoch,
                    scope_id: submit.scope_id.clone(),
                    scope_generation: submit.scope_generation,
                    sequence: submit.sequence,
                    intent_id: submit.intent.intent_id.clone(),
                    outcome: BrowserControlOutcome::Completed,
                    submitted_at_ms: Some(submitted_at_ms),
                    input_end_ms: Some(submitted_at_ms + 1),
                    failure_code: None,
                },
                &submit,
            )
            .unwrap();
        });
        let mut backend = BrowserControlBackend::new(session).unwrap();
        let start = intent.latest_start_ms;
        let (action, context) = context(&intent, evidence);
        let accepted = backend.submit_browser(start, &action, &context);
        let SubmitOutcome::Pending { sequence, .. } = accepted else {
            panic!("expected exactly one queued private action: {accepted:?}");
        };
        assert_eq!(sequence, 1);
        let completion = wait_completion(&mut backend);
        assert_eq!(completion.sequence, sequence);
        assert_eq!(completion.session_epoch, intent.session_epoch);
        assert_eq!(completion.intent_id, intent.intent_id);
        assert_eq!(completion.outcome, SubmitOutcome::Applied);
        assert!(completion.submitted_at_ms.is_some());
        assert!(completion.input_end_ms.is_some());
        server.join().unwrap();
    }

    #[test]
    fn peer_eof_after_enqueue_is_unknown_and_permanently_retires_the_scope() {
        let (intent, evidence) = fixture();
        let (session, server) = establish_channel(&intent.evidence.scope, |main| {
            let _submit = read_submit(main).unwrap();
            // Dropping the peer after accepting the request cannot prove whether
            // the resource action ran, so the Device must not retry or reconnect.
        });
        let mut backend = BrowserControlBackend::new(session).unwrap();
        let start = intent.latest_start_ms;
        let (action, context) = context(&intent, evidence);
        assert!(matches!(
            backend.submit_browser(start, &action, &context),
            SubmitOutcome::Pending { .. }
        ));
        let completion = wait_completion(&mut backend);
        assert_eq!(completion.outcome, SubmitOutcome::Unknown);
        assert_eq!(completion.failure_code, Some(FailureCode::EffectUnknown));
        assert!(!backend.ready());
        assert_eq!(
            backend.submit_browser(start, &action, &context),
            SubmitOutcome::Rejected
        );
        server.join().unwrap();
    }

    #[test]
    fn abandon_closes_the_local_sender_before_returning_and_disables_readiness() {
        let (intent, evidence) = fixture();
        let (submitted_tx, submitted_rx) = mpsc::sync_channel(1);
        let (session, server) = establish_channel(&intent.evidence.scope, move |main| {
            let _submit = read_submit(main).unwrap();
            submitted_tx.send(()).unwrap();
            let mut byte = [0; 1];
            // Core abandonment must synchronously retire the Device endpoint.
            assert_eq!(main.read(&mut byte).unwrap(), 0);
        });
        let mut backend = BrowserControlBackend::new(session).unwrap();
        let start = intent.latest_start_ms;
        let (action, context) = context(&intent, evidence);
        assert!(matches!(
            backend.submit_browser(start, &action, &context),
            SubmitOutcome::Pending { .. }
        ));
        submitted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Main received the single request");
        backend.abandon_pending();
        assert!(!backend.ready());
        assert_eq!(
            backend.submit_browser(start, &action, &context),
            SubmitOutcome::Rejected
        );
        server.join().unwrap();
    }

    #[test]
    fn revoking_the_pending_permit_closes_local_sends_without_waiting_for_main() {
        let (intent, evidence) = fixture();
        let (submitted_tx, submitted_rx) = mpsc::sync_channel(1);
        let (session, server) = establish_channel(&intent.evidence.scope, move |main| {
            let _submit = read_submit(main).unwrap();
            submitted_tx.send(()).unwrap();
            let mut byte = [0; 1];
            assert_eq!(main.read(&mut byte).unwrap(), 0);
        });
        let mut backend = BrowserControlBackend::new(session).unwrap();
        let start = intent.latest_start_ms;
        let (action, context) = context(&intent, evidence);
        assert!(matches!(
            backend.submit_browser(start, &action, &context),
            SubmitOutcome::Pending { .. }
        ));
        submitted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Main received the single request");
        backend.revoke(&intent.permit);
        assert!(!backend.ready());
        assert_eq!(
            wait_completion(&mut backend).outcome,
            SubmitOutcome::Unknown
        );
        server.join().unwrap();
    }

    #[test]
    fn idle_peer_close_drops_the_manifest_readiness_gate() {
        let (intent, _evidence) = fixture();
        let (session, server) = establish_channel(&intent.evidence.scope, |_main| {});
        let backend = BrowserControlBackend::new(session).unwrap();
        for _ in 0..2_000 {
            if !backend.ready() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(!backend.ready());
        server.join().unwrap();
    }

    #[test]
    fn browser_backend_rejects_native_primitives_without_writing_to_main() {
        use execution_core::backend::SubmitContext;
        use interaction_contracts::desktop::device::{
            ActionIntent, ExecutionIntent, ExecutionObservation,
        };
        use interaction_contracts::desktop::message::Message;
        use interaction_contracts::desktop::primitive::{Modifiers, MouseButton, Primitive};
        use interaction_contracts::desktop::samples;

        let (browser_intent, _evidence) = fixture();
        let (session, server) = establish_channel(&browser_intent.evidence.scope, |main| {
            let mut byte = [0; 1];
            assert_eq!(main.read(&mut byte).unwrap(), 0);
        });
        let mut backend = BrowserControlBackend::new(session).unwrap();
        let messages = samples::all();
        let native_intent: ActionIntent = messages
            .iter()
            .find_map(|(_, envelope)| match &envelope.message {
                Message::SubmitIntent(request) => match &request.intent {
                    ExecutionIntent::Native { intent } => Some(intent.as_ref().clone()),
                    ExecutionIntent::Browser { .. } => None,
                },
                _ => None,
            })
            .expect("sample native intent");
        let native_evidence = messages
            .iter()
            .find_map(|(_, envelope)| match &envelope.message {
                Message::Observation(ExecutionObservation::Native { observation }) => {
                    Some(observation.as_ref().clone())
                }
                _ => None,
            })
            .expect("sample native observation");
        let primitive = Primitive::Click {
            button: MouseButton::Left,
            x: 10,
            y: 10,
            modifiers: Modifiers::default(),
        };
        let context = SubmitContext {
            intent: &native_intent,
            authority: execution_core::backend::SubmissionAuthority {
                permit: native_intent.permit.clone(),
                stop_seq: 0,
                valid_until_ms: 100,
                command_end_ms: 100,
                evidence: native_evidence,
            },
        };
        assert_eq!(
            backend.submit(1, &primitive, &context),
            SubmitOutcome::Rejected
        );
        assert!(backend.ready());
        drop(backend);
        server.join().unwrap();
    }
}
