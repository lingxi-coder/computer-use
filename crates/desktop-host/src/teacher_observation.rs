//! Best-effort, private observation stream for teacher decisions.
//!
//! This channel is deliberately separate from the ordinary desktop IPC
//! protocol. A slow or missing observer must never stall the host or affect a
//! decision's normal delivery path.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use interaction_contracts::desktop::decision::{DecisionProposal, DecisionRequest, DecisionSource};
use serde::{Deserialize, Serialize};

const VERSION: u16 = 1;
const MAX_FRAME_BYTES: usize = 128 * 1024;
const MAX_BODY_BYTES: usize = MAX_FRAME_BYTES - 4;
const MAX_QUEUE_MESSAGES: usize = 2;
const FRAME_WRITE_TIMEOUT: Duration = Duration::from_millis(100);
const CLOSE_WAIT: Duration = Duration::from_millis(125);
const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const INVALID: &str = "invalid teacher observation";

/// Open the explicitly inherited private observer socket on descriptor 4.
/// `/dev/fd/4` duplicates the descriptor; no path or environment lookup is
/// involved.
pub fn open_inherited_fd4() -> Result<UnixStream, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fd/4")
        .map_err(|_| INVALID.to_string())?;
    if !file
        .metadata()
        .map_err(|_| INVALID.to_string())?
        .file_type()
        .is_socket()
    {
        return Err(INVALID.into());
    }
    let descriptor: OwnedFd = file.into();
    Ok(UnixStream::from(descriptor))
}

/// The request identity copied into terminal observation messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeacherObservationScope {
    decision_id: interaction_contracts::desktop::ids::DecisionId,
    request_generation: u64,
    root_task_id: interaction_contracts::desktop::ids::RootTaskId,
    root_epoch: u64,
    task_revision: u64,
    candidate_set_digest: String,
}

impl TeacherObservationScope {
    /// Create the exact request scope used by `finished` and `cancelled`.
    pub fn from_request(request: &DecisionRequest) -> Result<Self, String> {
        validate_request(request)?;
        Ok(Self::from_request_unchecked(request))
    }

    fn from_request_unchecked(request: &DecisionRequest) -> Self {
        Self {
            decision_id: request.decision_id.clone(),
            request_generation: request.request_generation,
            root_task_id: request.root_task_id.clone(),
            root_epoch: request.root_epoch,
            task_revision: request.task_revision,
            candidate_set_digest: request.state.candidate_set_digest.clone(),
        }
    }
}

/// Why the teacher request ended without a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeacherObservationReason {
    Cancelled,
    Expired,
    Quarantined,
    AccountingFailed,
    ScopeMismatch,
    Shutdown,
}

/// A fixed v1 message. Its inner representation is private so callers can
/// construct messages only through the validated request-aware constructors.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TeacherObservation(ObservationMessage);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ObservationMessage {
    Started {
        version: u16,
        request: DecisionRequest,
    },
    Finished {
        version: u16,
        scope: TeacherObservationScope,
        proposal: DecisionProposal,
    },
    Cancelled {
        version: u16,
        scope: TeacherObservationScope,
        reason: TeacherObservationReason,
    },
}

impl TeacherObservation {
    pub fn started(request: DecisionRequest) -> Self {
        Self(ObservationMessage::Started {
            version: VERSION,
            request,
        })
    }

    pub fn finished(request: &DecisionRequest, proposal: DecisionProposal) -> Self {
        Self(ObservationMessage::Finished {
            version: if validate_request(request).is_ok() {
                VERSION
            } else {
                0
            },
            scope: TeacherObservationScope::from_request_unchecked(request),
            proposal,
        })
    }

    pub fn cancelled(request: &DecisionRequest, reason: TeacherObservationReason) -> Self {
        Self(ObservationMessage::Cancelled {
            version: if validate_request(request).is_ok() {
                VERSION
            } else {
                0
            },
            scope: TeacherObservationScope::from_request_unchecked(request),
            reason,
        })
    }

    fn validate(&self) -> Result<(), String> {
        match &self.0 {
            ObservationMessage::Started { version, request } => {
                check_version(*version)?;
                validate_request(request)
            }
            ObservationMessage::Finished {
                version,
                scope,
                proposal,
            } => {
                check_version(*version)?;
                validate_scope(scope)?;
                if proposal.source != DecisionSource::Teacher
                    || proposal.decision_id != scope.decision_id
                    || proposal.request_generation != scope.request_generation
                    || proposal.candidate_set_digest != scope.candidate_set_digest
                    || !bounded_id(proposal.decision_id.as_str())
                    || !bounded_id(&proposal.candidate_set_digest)
                    || !safe_counter(proposal.queue_ms)
                    || !safe_counter(proposal.inference_ms)
                    || proposal.answers.iter().any(|answer| {
                        !bounded_id(&answer.question_id)
                            || answer
                                .selected
                                .as_deref()
                                .is_some_and(|selected| !bounded_id(selected))
                    })
                {
                    return Err(INVALID.into());
                }
                Ok(())
            }
            ObservationMessage::Cancelled { version, scope, .. } => {
                check_version(*version)?;
                validate_scope(scope)
            }
        }
    }
}

/// A bounded best-effort writer. Queue pressure drops observations without
/// closing or blocking the host's ordinary supervisor link.
pub struct TeacherObservationWriter {
    shared: Arc<WriterShared>,
    shutdown_stream: Option<UnixStream>,
    worker: Option<JoinHandle<()>>,
}

struct WriterShared {
    closed: AtomicBool,
    sender: Mutex<Option<SyncSender<Vec<u8>>>>,
}

impl TeacherObservationWriter {
    pub fn new(stream: UnixStream) -> Result<Self, String> {
        stream
            .set_nonblocking(true)
            .map_err(|_| INVALID.to_string())?;
        let writer_stream = stream.try_clone().map_err(|_| INVALID.to_string())?;
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUE_MESSAGES);
        let shared = Arc::new(WriterShared {
            closed: AtomicBool::new(false),
            sender: Mutex::new(Some(sender)),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("teacher-observation-writer".into())
            .spawn(move || writer_loop(writer_stream, receiver, worker_shared))
            .map_err(|_| INVALID.to_string())?;
        Ok(Self {
            shared,
            shutdown_stream: Some(stream),
            worker: Some(worker),
        })
    }

    /// Serialize and enqueue one message without waiting for the peer.
    /// `false` means invalid, full, or closed; none of these affects Host IPC.
    pub fn try_observe(&self, message: TeacherObservation) -> bool {
        let Ok(frame) = encode_frame(&message) else {
            return false;
        };
        let Ok(sender) = self.shared.sender.try_lock() else {
            return false;
        };
        let Some(sender) = sender.as_ref() else {
            return false;
        };
        try_enqueue(sender, &self.shared.closed, frame)
    }

    /// Stop accepting messages and release the worker with a bounded wait.
    pub fn close(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        if let Ok(mut sender) = self.shared.sender.try_lock() {
            sender.take();
        }
        if let Some(stream) = self.shutdown_stream.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(worker) = self.worker.take() {
            let deadline = Instant::now() + CLOSE_WAIT;
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
            // Dropping an unfinished JoinHandle detaches it. The stop flag and
            // socket shutdown ensure it cannot retain host-visible work.
        }
    }
}

impl Drop for TeacherObservationWriter {
    fn drop(&mut self) {
        self.close();
    }
}

fn writer_loop(mut stream: UnixStream, receiver: Receiver<Vec<u8>>, shared: Arc<WriterShared>) {
    while !shared.closed.load(Ordering::Acquire) {
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(frame) => {
                if write_bounded(&mut stream, &frame, FRAME_WRITE_TIMEOUT).is_err() {
                    shared.closed.store(true, Ordering::Release);
                    if let Ok(mut sender) = shared.sender.try_lock() {
                        sender.take();
                    }
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn try_enqueue(sender: &SyncSender<Vec<u8>>, closed: &AtomicBool, frame: Vec<u8>) -> bool {
    if closed.load(Ordering::Acquire) {
        return false;
    }
    match sender.try_send(frame) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => false,
        Err(TrySendError::Disconnected(_)) => {
            closed.store(true, Ordering::Release);
            false
        }
    }
}

fn encode_frame(message: &TeacherObservation) -> Result<Vec<u8>, String> {
    message.validate()?;
    let mut body = BoundedBuffer::new(MAX_BODY_BYTES);
    serde_json::to_writer(&mut body, message).map_err(|_| INVALID.to_string())?;
    if body.bytes.is_empty() || body.bytes.len() + 4 > MAX_FRAME_BYTES {
        return Err(INVALID.into());
    }
    let length = u32::try_from(body.bytes.len()).map_err(|_| INVALID.to_string())?;
    let mut frame = Vec::with_capacity(body.bytes.len() + 4);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body.bytes);
    Ok(frame)
}

struct BoundedBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(4096)),
            limit,
        }
    }
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "teacher observation frame limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_bounded(writer: &mut impl Write, bytes: &[u8], timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut offset = 0;
    while offset < bytes.len() {
        match writer.write(&bytes[offset..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "closed observer")),
            Ok(written) => offset += written,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "observer write timeout",
                    ));
                }
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
        if offset < bytes.len() && Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "observer write timeout",
            ));
        }
    }
    Ok(())
}

fn check_version(version: u16) -> Result<(), String> {
    if version == VERSION {
        Ok(())
    } else {
        Err(INVALID.into())
    }
}

fn validate_request(request: &DecisionRequest) -> Result<(), String> {
    request.validate().map_err(|_| INVALID.to_string())?;
    if request.root_task_id != request.state.root_task_id
        || request.task_revision != request.state.task_revision
        || !bounded_id(request.decision_id.as_str())
        || !bounded_id(request.root_task_id.as_str())
        || !bounded_id(&request.lane_id)
        || !bounded_id(&request.decision_program)
        || !bounded_id(&request.calibration_revision)
        || !bounded_id(request.state.root_task_id.as_str())
        || !bounded_id(&request.state.candidate_set_digest)
        || request.root_epoch == 0
        || request.task_revision == 0
        || !safe_counter(request.request_generation)
        || !safe_counter(request.task_revision)
        || !safe_counter(request.plan_revision)
        || !safe_counter(request.root_epoch)
        || !safe_counter(request.state.snapshot_revision)
        || !safe_counter(request.created_ms)
        || !safe_counter(request.result_deadline_ms)
        || request
            .state
            .objects
            .iter()
            .any(|object| !safe_counter(object.age_ms))
        || request.questions.iter().any(|question| {
            !bounded_id(&question.id)
                || question
                    .options
                    .iter()
                    .any(|option| !bounded_id(&option.key))
        })
        || request.state.candidates.iter().any(|candidate| {
            !bounded_id(candidate.candidate_id.as_str())
                || !bounded_id(&candidate.skill)
                || candidate
                    .semantic_target
                    .as_ref()
                    .is_some_and(|target| !bounded_id(target.as_str()))
                || candidate
                    .binding
                    .as_ref()
                    .is_some_and(|binding| !bounded_id(binding.as_str()))
        })
    {
        return Err(INVALID.into());
    }
    Ok(())
}

fn validate_scope(scope: &TeacherObservationScope) -> Result<(), String> {
    if bounded_id(scope.decision_id.as_str())
        && bounded_id(scope.root_task_id.as_str())
        && bounded_id(&scope.candidate_set_digest)
        && scope.request_generation > 0
        && scope.root_epoch > 0
        && scope.task_revision > 0
        && safe_counter(scope.request_generation)
        && safe_counter(scope.root_epoch)
        && safe_counter(scope.task_revision)
    {
        Ok(())
    } else {
        Err(INVALID.into())
    }
}

fn bounded_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn safe_counter(value: u64) -> bool {
    value <= JS_MAX_SAFE_INTEGER
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::decision::{Answer, PolicyChoice};
    use interaction_contracts::desktop::ids::{CandidateId, RootTaskId};

    fn request() -> DecisionRequest {
        serde_json::from_value(serde_json::json!({
            "decision_id": "decision-fixture-1",
            "lane_id": "teacher",
            "request_generation": 1,
            "root_task_id": "root-fixture-1",
            "task_revision": 7,
            "plan_revision": 2,
            "root_epoch": 3,
            "decision_program": "ui.ensure_value@1",
            "calibration_revision": "fixture",
            "state": {
                "root_task_id": "root-fixture-1",
                "task_revision": 7,
                "goal_summary": "set the fixture value",
                "subgoal": "choose the matching candidate",
                "snapshot_revision": 11,
                "objects": [],
                "candidates": [{
                    "candidate_id": "c-fixture",
                    "skill": "ui.ensure_value",
                    "semantic_target": null,
                    "binding": null,
                    "parameters": {"value": "fixture"},
                    "effect_class": "reversible_edit",
                    "precondition": {"state": "executable"},
                    "description": "set the fixture value"
                }],
                "recent_results": [],
                "missing_fields": [],
                "omitted_items": [],
                "candidate_set_digest": "cs1:fixture"
            },
            "questions": [{
                "id": "q1",
                "kind": "choice",
                "prompt": "Choose the fixture candidate",
                "options": [{"key": "c-fixture", "description": "set the fixture value"}]
            }],
            "created_ms": 100,
            "result_deadline_ms": 5000
        }))
        .unwrap()
    }

    fn proposal(request: &DecisionRequest) -> DecisionProposal {
        DecisionProposal {
            decision_id: request.decision_id.clone(),
            request_generation: request.request_generation,
            candidate_set_digest: request.state.candidate_set_digest.clone(),
            backend: "teacher-fixture".into(),
            source: DecisionSource::Teacher,
            model_revision: "fixture-v1".into(),
            answers: vec![Answer {
                question_id: "q1".into(),
                selected: Some("c-fixture".into()),
                confidence_milli: Some(950),
            }],
            choice: Some(PolicyChoice::StartSkill {
                candidate: CandidateId::new("c-fixture").unwrap(),
            }),
            failure: None,
            truncated: false,
            queue_ms: 4,
            inference_ms: 8,
        }
    }

    fn golden_messages() -> [TeacherObservation; 3] {
        let request = request();
        [
            TeacherObservation::started(request.clone()),
            TeacherObservation::finished(&request, proposal(&request)),
            TeacherObservation::cancelled(&request, TeacherObservationReason::Cancelled),
        ]
    }

    #[test]
    fn serialized_messages_match_the_cross_language_golden_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../contracts/teacher-observation-v1/fixtures.json"
        ))
        .unwrap();
        assert_eq!(fixture["version"], 1);
        let expected = fixture["messages"].as_array().unwrap();
        let actual = golden_messages()
            .iter()
            .map(|message| serde_json::to_value(message).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(&actual, expected);
    }

    #[test]
    fn request_root_task_revision_and_safe_counters_are_checked() {
        let mut bad = request();
        bad.state.root_task_id = RootTaskId::new("other-root").unwrap();
        assert!(encode_frame(&TeacherObservation::started(bad)).is_err());

        let mut bad = request();
        bad.state.task_revision += 1;
        assert!(encode_frame(&TeacherObservation::started(bad)).is_err());

        let mut bad = request();
        bad.created_ms = JS_MAX_SAFE_INTEGER + 1;
        assert!(encode_frame(&TeacherObservation::started(bad)).is_err());

        let mut bad = request();
        bad.root_epoch = 0;
        assert!(encode_frame(&TeacherObservation::started(bad)).is_err());

        let mut bad = request();
        bad.questions[0].options[0].key = "\u{7f}".into();
        assert!(encode_frame(&TeacherObservation::started(bad)).is_err());

        let mut bad = request();
        bad.state.candidate_set_digest = "x".repeat(257);
        assert!(encode_frame(&TeacherObservation::started(bad)).is_err());
    }

    #[test]
    fn finished_requires_teacher_source_and_exact_decision_scope() {
        let request = request();
        let mut bad = proposal(&request);
        bad.source = DecisionSource::S1;
        assert!(encode_frame(&TeacherObservation::finished(&request, bad)).is_err());

        let mut bad = proposal(&request);
        bad.request_generation += 1;
        assert!(encode_frame(&TeacherObservation::finished(&request, bad)).is_err());

        let mut bad = proposal(&request);
        bad.candidate_set_digest = "cs1:other".into();
        assert!(encode_frame(&TeacherObservation::finished(&request, bad)).is_err());
    }

    #[test]
    fn decoder_rejects_unknown_envelope_and_scope_fields() {
        let message = serde_json::to_value(TeacherObservation::cancelled(
            &request(),
            TeacherObservationReason::Shutdown,
        ))
        .unwrap();
        let mut unknown = message.clone();
        unknown["surprise"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ObservationMessage>(unknown).is_err());

        let mut unknown = message;
        unknown["scope"]["surprise"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ObservationMessage>(unknown).is_err());
    }

    #[test]
    fn total_frame_limit_is_enforced_without_an_unbounded_serialization_buffer() {
        let mut request = request();
        request.state.goal_summary = "x".repeat(MAX_FRAME_BYTES * 2);
        assert!(encode_frame(&TeacherObservation::started(request)).is_err());
    }

    #[test]
    fn frame_uses_big_endian_length_and_handles_short_writes() {
        struct ShortWriter {
            bytes: Vec<u8>,
            max_write: usize,
        }
        impl Write for ShortWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = bytes.len().min(self.max_write);
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let frame = encode_frame(&TeacherObservation::started(request())).unwrap();
        let body_length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(body_length + 4, frame.len());
        let mut writer = ShortWriter {
            bytes: Vec::new(),
            max_write: 3,
        };
        write_bounded(&mut writer, &frame, Duration::from_millis(100)).unwrap();
        assert_eq!(writer.bytes, frame);
    }

    #[test]
    fn stalled_writer_times_out_within_the_frame_budget() {
        struct WouldBlock;
        impl Write for WouldBlock {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let start = Instant::now();
        let result = write_bounded(&mut WouldBlock, b"frame", FRAME_WRITE_TIMEOUT);
        let elapsed = start.elapsed();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(elapsed >= Duration::from_millis(80));
        assert!(elapsed < Duration::from_millis(300));
    }

    #[test]
    fn a_full_queue_drops_without_poisoning_the_channel() {
        let (sender, _receiver) = mpsc::sync_channel(2);
        let closed = AtomicBool::new(false);
        assert!(try_enqueue(&sender, &closed, vec![1]));
        assert!(try_enqueue(&sender, &closed, vec![2]));
        assert!(!try_enqueue(&sender, &closed, vec![3]));
        assert!(!closed.load(Ordering::Acquire));
    }

    #[test]
    fn contended_sender_lock_drops_and_close_stays_bounded() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let mut writer = TeacherObservationWriter::new(stream).unwrap();
        let shared = Arc::clone(&writer.shared);
        let sender_guard = shared.sender.lock().unwrap();

        let start = Instant::now();
        assert!(!writer.try_observe(TeacherObservation::started(request())));
        assert!(start.elapsed() < Duration::from_millis(20));

        let start = Instant::now();
        writer.close();
        assert!(start.elapsed() < Duration::from_millis(250));
        drop(sender_guard);
    }

    #[test]
    fn disconnected_peer_closes_the_observation_writer() {
        let (stream, peer) = UnixStream::pair().unwrap();
        let writer = TeacherObservationWriter::new(stream).unwrap();
        drop(peer);
        assert!(writer.try_observe(TeacherObservation::started(request())));
        let deadline = Instant::now() + Duration::from_millis(300);
        while !writer.shared.closed.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(writer.shared.closed.load(Ordering::Acquire));
    }

    #[test]
    fn close_is_bounded_even_with_queued_observations() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let mut writer = TeacherObservationWriter::new(stream).unwrap();
        let message = TeacherObservation::started(request());
        let _ = writer.try_observe(message.clone());
        let _ = writer.try_observe(message.clone());
        let _ = writer.try_observe(message);
        let start = Instant::now();
        writer.close();
        assert!(start.elapsed() < Duration::from_millis(250));
        assert!(!writer.try_observe(TeacherObservation::started(request())));
    }

    #[test]
    fn scope_constructor_validates_before_copying_request_identity() {
        let mut bad = request();
        bad.task_revision = 0;
        assert!(TeacherObservationScope::from_request(&bad).is_err());
        let request = request();
        let scope = TeacherObservationScope::from_request(&request).unwrap();
        assert_eq!(scope.root_task_id.as_str(), "root-fixture-1");
        assert_eq!(scope.candidate_set_digest, "cs1:fixture");
    }
}
