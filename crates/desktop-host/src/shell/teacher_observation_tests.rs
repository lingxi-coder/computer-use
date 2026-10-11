use super::*;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use harness_adapter::model::{ModelProtocol, ModelProvider, ModelRoute};
use harness_adapter::teacher::TeacherAdapter;
use interaction_contracts::desktop::decision::DecisionRequest;
use interaction_contracts::desktop::operator::OperatorCommand;
use lingxi_llm_client::protocol::LlmError;
use lingxi_llm_client::{HttpRequest, StreamResponse, Transport};
use std::collections::{BTreeMap, VecDeque};
use std::io::{ErrorKind, Read};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use super::model_tests::fixture_world;

struct ObservationBody {
    chunks: VecDeque<Bytes>,
    hold: bool,
    dropped: Arc<AtomicUsize>,
}

impl Stream for ObservationBody {
    type Item = Result<Bytes, LlmError>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(chunk) = self.chunks.pop_front() {
            Poll::Ready(Some(Ok(chunk)))
        } else if self.hold {
            Poll::Pending
        } else {
            Poll::Ready(None)
        }
    }
}

impl Drop for ObservationBody {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct ObservationProvider {
    calls: AtomicUsize,
    dropped: Arc<AtomicUsize>,
    hold_first: bool,
}

#[async_trait]
impl Transport for ObservationProvider {
    async fn send(&self, request: HttpRequest) -> Result<StreamResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let value: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let context = teacher_context(&value).expect("SDK request includes teacher context");
        let question = context["questions"].as_array().unwrap().first().unwrap();
        let question_id = question["id"].as_str().unwrap();
        let selected = question["options"][0]["key"].as_str().unwrap();
        let payload = serde_json::json!({
            "answers":[{"question_id":question_id,"selected":selected,"confidence_milli":800}],
            "choice":{"action":"start_skill","candidate_id":selected}
        });
        let event = serde_json::json!({"id":"teacher-test","model":"fixture-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"teacher-call","type":"function","function":{"name":"TeacherDecision","arguments":payload.to_string()}}]},"finish_reason":null}]});
        let end = serde_json::json!({"id":"teacher-test","model":"fixture-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12,"completion_tokens":7,"total_tokens":19}});
        let hold = self.hold_first && call == 0;
        let mut chunks = VecDeque::from([Bytes::from(format!("data: {event}\n\n"))]);
        if !hold {
            chunks.push_back(Bytes::from(format!("data: {end}\n\ndata: [DONE]\n\n")));
        }
        Ok(StreamResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: Box::pin(ObservationBody {
                chunks,
                hold,
                dropped: self.dropped.clone(),
            }),
        })
    }
}

fn teacher_context(value: &serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::String(text) => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .filter(|nested| {
                nested.get("questions").is_some() && nested.get("candidates").is_some()
            }),
        serde_json::Value::Array(values) => values.iter().find_map(teacher_context),
        serde_json::Value::Object(values) => values.values().find_map(teacher_context),
        _ => None,
    }
}

struct ObservationPipe {
    writer: TeacherObservationWriter,
    reader: UnixStream,
}

fn observation_pipe() -> ObservationPipe {
    let (writer_stream, reader) = UnixStream::pair().unwrap();
    reader.set_nonblocking(true).unwrap();
    ObservationPipe {
        writer: TeacherObservationWriter::new(writer_stream).unwrap(),
        reader,
    }
}

fn observation_shell(
    writer: Option<TeacherObservationWriter>,
    hold_first: bool,
) -> (Shell<Vec<u8>>, Arc<ObservationProvider>) {
    let clock = SharedClock::starting_now();
    let mut settings = BTreeMap::new();
    for (key, value) in [
        ("socket_dir", "/tmp/unused"),
        ("db_path", "/tmp/unused/teacher-observation.db"),
        ("device_token", "0123456789abcdef0123456789abcdef"),
        ("source_token", "0123456789abcdef0123456789abcdef"),
        ("device_epoch", "1"),
        ("planning_backend", "fixture"),
    ] {
        settings.insert(key.into(), value.into());
    }
    settings.insert("clock_origin_ms".into(), clock.origin_unix_ms().to_string());
    let config = HostConfig::from_configure(&Configure {
        role: PeerRole::Host,
        session_epoch: 1,
        settings,
        browser_page_profile: None,
    })
    .unwrap();
    let provider = Arc::new(ObservationProvider {
        calls: AtomicUsize::new(0),
        dropped: Arc::new(AtomicUsize::new(0)),
        hold_first,
    });
    let teacher = TeacherAdapter::with_sdk_transport(
        ModelRoute {
            preset_id: None,
            provider_id: ModelProvider::OpenAi,
            protocol: ModelProtocol::OpenAiChat,
            base_url: "https://example.invalid/v1".into(),
            model: "fixture-model".into(),
            max_output_tokens: 4096,
        },
        "test-only-fixture-key".into(),
        Arc::new(move || clock.now_ms()),
        provider.clone(),
    )
    .unwrap();
    let (tx, _rx) = mpsc::channel();
    let shell = Shell {
        engine: Engine::new(
            EngineConfig::new(TaskConfig::new(vec![AppPack::settings_fixture()])),
            Storage::open_in_memory().unwrap(),
        ),
        bridge: Bridge::new(),
        supervisor: Link::new(Vec::new(), "teacher-observation-test", clock),
        device: None,
        source: None,
        source_refresh: SourceRefreshBridge::default(),
        source_capture: SourceCaptureBridge::default(),
        clock,
        config,
        tx,
        next_conn: 0,
        planner: ScriptedPlanner::default(),
        model_planner: None,
        pending_model: None,
        inflight_model: None,
        teacher: Some(teacher),
        teacher_identity: Some(ModelIdentity {
            provider: "open_ai".into(),
            model: "fixture-model".into(),
        }),
        pending_teacher: None,
        teacher_observation: writer,
        teacher_observation_terminal: false,
        computer: None,
        pending_supervisor_decisions: Default::default(),
        queue: VecDeque::new(),
        owned_revisit: None,
        suppress_model_results: Default::default(),
        owned_diagnostic_last: None,
        owned_diagnostic_count: 0,
        exit: false,
    };
    (shell, provider)
}

fn open_pending_decision(shell: &mut Shell<Vec<u8>>) -> DecisionRequest {
    let mut world = fixture_world::World::new();
    std::mem::swap(&mut world.engine, &mut shell.engine);
    world.now = shell.clock.now_ms().max(1);
    world.backend = fixture_world::Backend::Silent;
    world.introduce_device();
    world.operator(OperatorCommand::SetMode {
        mode: interaction_contracts::desktop::operator::RunMode::Execute,
    });
    if world
        .engine
        .root_state()
        .is_some_and(|state| !state.is_terminal())
    {
        world.operator(OperatorCommand::StopTask { root_task_id: None });
    }
    world.start(fixture_world::TEXT_AMBIGUOUS);
    world.until(|world| !world.decision_requests.is_empty(), 1_000);
    let request = world.decision_requests.last().unwrap().clone();
    std::mem::swap(&mut world.engine, &mut shell.engine);
    let deadline = Instant::now() + Duration::from_secs(2);
    while shell.clock.now_ms() < request.created_ms {
        assert!(
            Instant::now() < deadline,
            "fixture time did not reach host clock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    request
}

fn until_teacher(shell: &mut Shell<Vec<u8>>, done: impl Fn(&Shell<Vec<u8>>) -> bool) {
    let started = Instant::now();
    while !done(shell) {
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "teacher observation flow timed out"
        );
        shell.poll_teacher();
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn read_observations(reader: &mut UnixStream, wanted: usize) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut pending = Vec::new();
    let mut messages = Vec::new();
    while Instant::now() < deadline && messages.len() < wanted {
        let mut chunk = [0u8; 8192];
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => pending.extend_from_slice(&chunk[..read]),
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
            {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => break,
            Err(error) => panic!("private observation stream read failed: {error}"),
        }
        loop {
            if pending.len() < 4 {
                break;
            }
            let length = u32::from_be_bytes(pending[..4].try_into().unwrap()) as usize;
            if pending.len() < length + 4 {
                break;
            }
            let body = pending[4..length + 4].to_vec();
            pending.drain(..length + 4);
            messages.push(serde_json::from_slice(&body).unwrap());
        }
    }
    messages
}

fn decision_inputs(shell: &Shell<Vec<u8>>) -> Vec<&DecisionProposal> {
    shell
        .queue
        .iter()
        .filter_map(|input| match input {
            Input::Decision(proposal) => Some(proposal.as_ref()),
            _ => None,
        })
        .collect()
}

#[test]
fn private_observer_records_the_admitted_sdk_teacher_decision_without_replacing_it() {
    let pipe = observation_pipe();
    let (mut shell, provider) = observation_shell(Some(pipe.writer), false);
    let request = open_pending_decision(&mut shell);
    shell.start_teacher_decision(request.clone());
    until_teacher(&mut shell, |host| host.pending_teacher.is_none());

    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.dropped.load(Ordering::SeqCst), 1);
    let inputs = decision_inputs(&shell);
    assert_eq!(inputs.len(), 1);
    let proposal = inputs[0];
    assert_eq!(proposal.source, DecisionSource::Teacher);
    assert_eq!(proposal.decision_id, request.decision_id);
    assert_eq!(proposal.request_generation, request.request_generation);
    assert_eq!(
        proposal.candidate_set_digest,
        request.state.candidate_set_digest
    );
    assert!(!shell.exit);

    shell.drain();
    assert!(shell.queue.is_empty());
    assert!(!shell.exit);
    let outcome: String = shell
        .engine
        .storage()
        .read()
        .query_row("SELECT outcome FROM decision_model_calls", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(outcome, "proposal");

    let messages = read_observations(&mut pipe.reader.try_clone().unwrap(), 2);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["type"], "started");
    assert_eq!(
        messages[0]["request"]["decision_id"],
        request.decision_id.as_str()
    );
    assert_eq!(
        messages[0]["request"]["request_generation"],
        request.request_generation
    );
    assert_eq!(messages[1]["type"], "finished");
    assert_eq!(
        messages[1]["scope"]["decision_id"],
        request.decision_id.as_str()
    );
    assert_eq!(
        messages[1]["scope"]["root_task_id"],
        request.root_task_id.as_str()
    );
    assert_eq!(
        messages[1]["scope"]["request_generation"],
        request.request_generation
    );
    assert_eq!(messages[1]["proposal"]["source"], "teacher");
    assert_eq!(
        messages[1]["proposal"]["candidate_set_digest"],
        request.state.candidate_set_digest
    );
}

#[test]
fn disconnected_or_full_observer_cannot_change_teacher_result_or_host_liveness() {
    for saturated in [false, true] {
        let pipe = observation_pipe();
        if !saturated {
            drop(pipe.reader);
        }
        let (mut shell, provider) = observation_shell(Some(pipe.writer), false);
        let request = open_pending_decision(&mut shell);
        if saturated {
            // Keep the peer unread until the bounded observer queue/socket cannot accept more.
            let message = TeacherObservation::started(request.clone());
            let mut rejected = false;
            let deadline = Instant::now() + Duration::from_millis(300);
            while Instant::now() < deadline {
                if !shell
                    .teacher_observation
                    .as_ref()
                    .unwrap()
                    .try_observe(message.clone())
                {
                    rejected = true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                rejected,
                "the non-reading observer did not reach its bounded limit"
            );
        }
        shell.start_teacher_decision(request.clone());
        until_teacher(&mut shell, |host| host.pending_teacher.is_none());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(decision_inputs(&shell).len(), 1);
        assert_eq!(decision_inputs(&shell)[0].source, DecisionSource::Teacher);
        assert!(!shell.exit);
        let outcome: String = shell
            .engine
            .storage()
            .read()
            .query_row("SELECT outcome FROM decision_model_calls", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(outcome, "proposal");
    }
}

#[test]
fn stopping_a_pending_teacher_turn_records_only_its_cancel_terminal() {
    let pipe = observation_pipe();
    let (mut shell, provider) = observation_shell(Some(pipe.writer), true);
    let request = open_pending_decision(&mut shell);
    shell.start_teacher_decision(request.clone());
    until_teacher(&mut shell, |_| provider.calls.load(Ordering::SeqCst) == 1);

    shell
        .queue
        .push_back(Input::Operator(OperatorCommand::StopTask {
            root_task_id: Some(request.root_task_id.clone()),
        }));
    shell.drain();
    until_teacher(&mut shell, |host| host.pending_teacher.is_none());

    assert_eq!(provider.dropped.load(Ordering::SeqCst), 1);
    assert!(decision_inputs(&shell).is_empty());
    assert!(shell.queue.is_empty());
    assert!(!shell.exit);
    let outcome: String = shell
        .engine
        .storage()
        .read()
        .query_row("SELECT outcome FROM decision_model_calls", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(outcome, "cancelled");

    let messages = read_observations(&mut pipe.reader.try_clone().unwrap(), 2);
    let terminal = messages
        .iter()
        .filter(|message| message["type"] == "cancelled")
        .count();
    assert_eq!(
        terminal, 1,
        "one cancellation terminal is recorded for the request"
    );
    assert!(messages.iter().all(|message| message["type"] != "finished"));
    assert_eq!(
        messages[1]["scope"]["decision_id"],
        request.decision_id.as_str()
    );
    assert_eq!(messages[1]["reason"], "cancelled");
}
