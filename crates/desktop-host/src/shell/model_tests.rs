//! Full engine/host model path with pinned SDK protocol decoding. No GUI or
//! account; the transport is a deterministic provider-byte fixture.
use super::*;

#[test]
fn owned_revisit_request_ids_are_single_use_and_bounded() {
    let mut seen = std::collections::BTreeSet::new();
    assert!(record_request_id(&mut seen, "request-a"));
    assert!(!record_request_id(&mut seen, "request-a"));
    for index in 1..256 {
        assert!(record_request_id(&mut seen, &format!("request-{index}")));
    }
    assert_eq!(seen.len(), 256);
    assert!(!record_request_id(&mut seen, "request-over-limit"));
}
use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use harness_adapter::model::{ModelProtocol, ModelProvider, ModelRoute, ModelTransport};
use harness_adapter::teacher::TeacherAdapter;
use interaction_contracts::desktop::decision::{
    Candidate, DecisionRequest, DecisionState, Precondition, Question, QuestionKind, QuestionOption,
};
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{
    BindingRef, CandidateId, DecisionId, RootTaskId, SemanticTargetRef,
};
use interaction_contracts::desktop::operator::OperatorCommand;
use interaction_contracts::desktop::task::RootState;
use lingxi_llm_client::protocol::LlmError;
use lingxi_llm_client::{HttpRequest, StreamResponse, Transport};
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

// Reuse the canonical simulator's real Engine/Core/Source loop rather than
// opening a synthetic decision that the production accounting owner rejects.
#[allow(dead_code)]
#[path = "../../../interaction-engine/tests/common/world.rs"]
pub(super) mod fixture_world;

fn open_teacher_decision(shell: &mut Shell<Vec<u8>>) -> DecisionRequest {
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
            "fixture time did not reach the real clock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    request
}

struct Body {
    chunks: VecDeque<Bytes>,
    hold: bool,
    dropped: Arc<AtomicUsize>,
}
impl Stream for Body {
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
impl Drop for Body {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
pub(super) struct Provider {
    calls: AtomicUsize,
    hold_first: bool,
    dropped: Arc<AtomicUsize>,
    requests: Mutex<Vec<HttpRequest>>,
    teacher_output: bool,
}
#[async_trait]
impl Transport for Provider {
    async fn send(&self, request: HttpRequest) -> Result<StreamResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let context = self
            .teacher_output
            .then(|| serde_json::from_slice::<serde_json::Value>(&request.body).ok())
            .flatten()
            .and_then(|value| teacher_context(&value));
        self.requests.lock().unwrap().push(request);
        let (tool, payload) = if self.teacher_output {
            let question = context
                .as_ref()
                .and_then(|context| context["questions"].as_array()?.first());
            let question_id = question
                .and_then(|question| question["id"].as_str())
                .unwrap_or("q1");
            let selected = question
                .and_then(|question| question["options"].as_array()?.first())
                .and_then(|option| option["key"].as_str())
                .unwrap_or("candidate-off");
            (
                "TeacherDecision",
                serde_json::json!({
                    "answers":[{"question_id":question_id,"selected":selected,"confidence_milli":800}],
                    "choice":{"action":"start_skill","candidate_id":selected}
                }).to_string(),
            )
        } else {
            let output =
                ScriptedPlanner::default().plan_text(r#"Turn off "Daily picks" in "Push""#);
            ("StructuredOutput", serde_json::to_string(&output).unwrap())
        };
        let event = serde_json::json!({"id":"chat-model","model":"host-fixture","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":tool,"arguments":payload}}]},"finish_reason":null}]});
        let end = serde_json::json!({"id":"chat-model","model":"host-fixture","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12,"completion_tokens":7,"total_tokens":19}});
        let hold = self.hold_first && call == 0;
        let mut chunks = VecDeque::from([Bytes::from(format!("data: {event}\n\n"))]);
        if !hold {
            chunks.push_back(Bytes::from(format!("data: {end}\n\ndata: [DONE]\n\n")));
        }
        Ok(StreamResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: Box::pin(Body {
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
            .filter(|value| value.get("questions").is_some() && value.get("candidates").is_some()),
        serde_json::Value::Array(values) => values.iter().find_map(teacher_context),
        serde_json::Value::Object(values) => values.values().find_map(teacher_context),
        _ => None,
    }
}
pub(super) fn shell(hold_first: bool) -> (Shell<Vec<u8>>, Arc<Provider>) {
    let clock = SharedClock::starting_now();
    let mut settings = BTreeMap::new();
    for (k, v) in [
        ("socket_dir", "/tmp/unused"),
        ("db_path", "/tmp/unused/host.db"),
        ("device_token", "0123456789abcdef0123456789abcdef"),
        ("source_token", "0123456789abcdef0123456789abcdef"),
        ("device_epoch", "1"),
        ("planning_backend", "model"),
    ] {
        settings.insert(k.into(), v.into());
    }
    settings.insert("clock_origin_ms".into(), clock.origin_unix_ms().to_string());
    let config = HostConfig::from_configure(&Configure {
        role: PeerRole::Host,
        session_epoch: 1,
        settings,
        browser_page_profile: None,
    })
    .unwrap();
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        hold_first,
        dropped: Arc::new(AtomicUsize::new(0)),
        requests: Mutex::new(vec![]),
        teacher_output: false,
    });
    let transport = ModelTransport::with_transport(
        ModelRoute {
            preset_id: None,
            provider_id: ModelProvider::OpenAi,
            protocol: ModelProtocol::OpenAiChat,
            base_url: "https://example.invalid/v1".into(),
            model: "host-fixture".into(),
            max_output_tokens: 4096,
        },
        "explicit-fixture-key".into(),
        Arc::new(move || clock.now_ms()),
        provider.clone(),
    )
    .unwrap();
    let (tx, _rx) = mpsc::channel();
    // These model-accounting tests have no connected peers. Give planning an
    // explicit simulated profile so it can freeze the Goal's trusted domain;
    // no device command or Source evidence is fabricated here.
    let mut engine = Engine::new(
        EngineConfig::new(TaskConfig::new(vec![AppPack::settings_fixture()])),
        Storage::open_in_memory().unwrap(),
    );
    let manifest = interaction_contracts::desktop::device::CapabilityManifest {
        identity: interaction_contracts::desktop::device::DeviceIdentity {
            session_epoch: 1,
            surface: interaction_contracts::desktop::device::SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "sim/1/wall".into(),
            dependencies: BTreeMap::new(),
        },
        backend: interaction_contracts::desktop::device::BackendKind::Simulated,
        execution_domain: interaction_contracts::desktop::domain::ExecutionDomain::Simulated,
        policy: interaction_contracts::desktop::primitive::PrimitivePolicy::AtomicOnly,
        human_input: interaction_contracts::desktop::device::CapabilityState::Available,
        hit_test: interaction_contracts::desktop::device::CapabilityState::Available,
        input: interaction_contracts::desktop::device::CapabilityState::Available,
        domains: fixture_world::domain_capabilities_for_test_profile(
            interaction_contracts::desktop::domain::ExecutionDomain::Simulated,
        ),
        max_uninterruptible_ms: 100,
    };
    manifest
        .validate()
        .expect("the no-peer model fixture uses a valid simulated profile");
    let _ = engine.handle(
        0,
        Input::DeviceNotice(interaction_engine::execution::DeviceNotice::Manifest(
            Box::new(manifest),
        )),
    );
    (
        Shell {
            engine,
            bridge: Bridge::new(),
            supervisor: Link::new(Vec::new(), "host-test", clock),
            device: None,
            source: None,
            source_refresh: SourceRefreshBridge::default(),
            source_capture: SourceCaptureBridge::default(),
            clock,
            config,
            tx,
            next_conn: 0,
            planner: ScriptedPlanner::default(),
            model_planner: Some(ModelPlanner::new(transport)),
            pending_model: None,
            inflight_model: None,
            teacher: None,
            teacher_identity: None,
            pending_teacher: None,
            teacher_observation: None,
            teacher_observation_terminal: false,
            computer: None,
            pending_supervisor_decisions: Default::default(),
            queue: VecDeque::new(),
            owned_revisit: None,
            suppress_model_results: Default::default(),
            owned_diagnostic_last: None,
            owned_diagnostic_count: 0,
            exit: false,
        },
        provider,
    )
}

fn teacher_shell(hold_first: bool) -> (Shell<Vec<u8>>, Arc<Provider>) {
    let clock = SharedClock::starting_now();
    let mut settings = BTreeMap::new();
    for (k, v) in [
        ("socket_dir", "/tmp/unused"),
        ("db_path", "/tmp/unused/teacher-host.db"),
        ("device_token", "0123456789abcdef0123456789abcdef"),
        ("source_token", "0123456789abcdef0123456789abcdef"),
        ("device_epoch", "1"),
        ("planning_backend", "fixture"),
    ] {
        settings.insert(k.into(), v.into());
    }
    settings.insert("clock_origin_ms".into(), clock.origin_unix_ms().to_string());
    let config = HostConfig::from_configure(&Configure {
        role: PeerRole::Host,
        session_epoch: 1,
        settings,
        browser_page_profile: None,
    })
    .unwrap();
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        hold_first,
        dropped: Arc::new(AtomicUsize::new(0)),
        requests: Mutex::new(vec![]),
        teacher_output: true,
    });
    let teacher = TeacherAdapter::with_sdk_transport(
        ModelRoute {
            preset_id: None,
            provider_id: ModelProvider::OpenAi,
            protocol: ModelProtocol::OpenAiChat,
            base_url: "https://example.invalid/v1".into(),
            model: "host-fixture".into(),
            max_output_tokens: 4096,
        },
        "explicit-fixture-key".into(),
        Arc::new(move || clock.now_ms()),
        provider.clone(),
    )
    .unwrap();
    let (tx, _rx) = mpsc::channel();
    (
        Shell {
            engine: Engine::new(
                EngineConfig::new(TaskConfig::new(vec![AppPack::settings_fixture()])),
                Storage::open_in_memory().unwrap(),
            ),
            bridge: Bridge::new(),
            supervisor: Link::new(Vec::new(), "host-teacher-test", clock),
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
            teacher_identity: Some(crate::model_bootstrap::ModelIdentity {
                provider: "open_ai".into(),
                model: "host-fixture".into(),
            }),
            pending_teacher: None,
            teacher_observation: None,
            teacher_observation_terminal: false,
            computer: None,
            pending_supervisor_decisions: Default::default(),
            queue: VecDeque::new(),
            owned_revisit: None,
            suppress_model_results: Default::default(),
            owned_diagnostic_last: None,
            owned_diagnostic_count: 0,
            exit: false,
        },
        provider,
    )
}

fn decision_request(root: &str, id: &str, generation: u64) -> DecisionRequest {
    let root_id = RootTaskId::new(root).unwrap();
    DecisionRequest {
        decision_id: DecisionId::new(id).unwrap(),
        lane_id: "host-teacher-test".into(),
        request_generation: generation,
        root_task_id: root_id.clone(),
        task_revision: 1,
        plan_revision: 1,
        root_epoch: 1,
        decision_program: "ui.ensure_value@1".into(),
        calibration_revision: "none".into(),
        state: DecisionState {
            root_task_id: root_id,
            task_revision: 1,
            goal_summary: "turn the selected setting off".into(),
            subgoal: "choose the listed action".into(),
            snapshot_revision: 2,
            objects: vec![],
            candidates: vec![Candidate {
                candidate_id: CandidateId::new("candidate-off").unwrap(),
                skill: "ui.ensure_value".into(),
                semantic_target: Some(SemanticTargetRef::new("daily-picks").unwrap()),
                binding: Some(BindingRef::new("binding-1").unwrap()),
                parameters: BTreeMap::from([("value".into(), "off".into())]),
                effect_class: EffectClass::ReversibleEdit,
                precondition: Precondition::Executable,
                description: "set daily picks off".into(),
            }],
            recent_results: vec![],
            missing_fields: vec![],
            omitted_items: vec![],
            candidate_set_digest: "sha256:host-teacher-test".into(),
        },
        questions: vec![Question {
            id: "q1".into(),
            kind: QuestionKind::Choice,
            prompt: "Which listed action should run?".into(),
            options: vec![QuestionOption {
                key: "candidate-off".into(),
                description: "turn it off".into(),
            }],
        }],
        created_ms: 1,
        result_deadline_ms: 10_000,
    }
}

fn until_teacher(shell: &mut Shell<Vec<u8>>, done: impl Fn(&Shell<Vec<u8>>) -> bool) {
    let start = Instant::now();
    while !done(shell) {
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "teacher flow timed out"
        );
        shell.poll_teacher();
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn send(shell: &mut Shell<Vec<u8>>, command: OperatorCommand) {
    shell.queue.push_back(Input::Operator(command));
    shell.drain();
    shell.poll_model();
}
fn until(shell: &mut Shell<Vec<u8>>, done: impl Fn(&Shell<Vec<u8>>) -> bool) {
    let start = Instant::now();
    while !done(shell) {
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "host model flow timed out"
        );
        shell.drain();
        shell.poll_model();
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn model_goal_waits_for_confirmation_and_accounting_precedes_adoption() {
    let (mut shell, provider) = shell(false);
    send(
        &mut shell,
        OperatorCommand::CreateTask {
            text: r#"Turn off "Daily picks" in "Push""#.into(),
        },
    );
    until(&mut shell, |s| {
        s.engine.root_state() == Some(RootState::AwaitingConfirmation)
    });
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.dropped.load(Ordering::SeqCst), 1);
    let (outcome, usage): (String, String) = shell
        .engine
        .storage()
        .read()
        .query_row("SELECT outcome,usage_json FROM model_calls", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(outcome, "proposal");
    assert_eq!(
        serde_json::from_str::<ModelUsage>(&usage).unwrap(),
        ModelUsage {
            input_tokens: Some(12),
            output_tokens: Some(7),
            cost_micros: None
        }
    );
    let attempts: u64 = shell
        .engine
        .storage()
        .read()
        .query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(attempts, 0);
}
#[test]
fn stop_cancels_provider_stream_and_a_new_goal_waits_for_cleanup() {
    let (mut shell, provider) = shell(true);
    send(
        &mut shell,
        OperatorCommand::CreateTask {
            text: "first task".into(),
        },
    );
    until(&mut shell, |_| provider.calls.load(Ordering::SeqCst) == 1);
    let old = RootTaskId::new(shell.engine.view().unwrap().root_task_id.as_str()).unwrap();
    send(
        &mut shell,
        OperatorCommand::StopTask {
            root_task_id: Some(old.clone()),
        },
    );
    send(
        &mut shell,
        OperatorCommand::CreateTask {
            text: r#"Turn off "Daily picks" in "Push""#.into(),
        },
    );
    until(&mut shell, |s| {
        s.engine.root_state() == Some(RootState::AwaitingConfirmation)
    });
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.dropped.load(Ordering::SeqCst), 2);
    assert_ne!(shell.engine.view().unwrap().root_task_id, old);
    let outcomes: Vec<String> = shell
        .engine
        .storage()
        .read()
        .prepare("SELECT outcome FROM model_calls ORDER BY started_ms,request_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(outcomes, vec!["cancelled", "proposal"]);
    assert_eq!(
        shell.engine.stop_status(),
        interaction_contracts::desktop::operator::StopStatus::NotStopped
    );
}

#[test]
fn shutdown_records_cleanup_without_installing_a_proposal() {
    let (mut shell, provider) = shell(true);
    send(
        &mut shell,
        OperatorCommand::CreateTask {
            text: "inspect before shutdown".into(),
        },
    );
    until(&mut shell, |_| provider.calls.load(Ordering::SeqCst) == 1);
    shell.close_model();
    assert_eq!(provider.dropped.load(Ordering::SeqCst), 1);
    let outcome: String = shell
        .engine
        .storage()
        .read()
        .query_row("SELECT outcome FROM model_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(outcome, "cancelled");
    assert_eq!(shell.engine.root_state(), Some(RootState::CompilingGoal));
    assert!(shell.queue.is_empty());
    assert!(shell.model_planner.as_mut().unwrap().is_idle());
}

#[test]
fn an_authorized_teacher_decision_stays_inside_host_and_returns_one_input() {
    let (mut shell, provider) = teacher_shell(false);
    let request = open_teacher_decision(&mut shell);
    shell.start_teacher_decision(request.clone());
    until_teacher(&mut shell, |s| s.pending_teacher.is_none());

    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.dropped.load(Ordering::SeqCst), 1);
    assert!(shell.model_planner.is_none());
    let decisions = shell
        .queue
        .iter()
        .filter_map(|input| match input {
            Input::Decision(proposal) => Some(proposal),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].decision_id, request.decision_id);
    assert_eq!(decisions[0].request_generation, request.request_generation);
    assert_eq!(
        decisions[0].candidate_set_digest,
        request.state.candidate_set_digest
    );
    assert_eq!(
        decisions[0].source,
        interaction_contracts::desktop::decision::DecisionSource::Teacher
    );
    assert_eq!(
        decisions[0].choice.as_ref().unwrap(),
        &interaction_contracts::desktop::decision::PolicyChoice::StartSkill {
            candidate: CandidateId::new(request.questions[0].options[0].key.clone()).unwrap()
        }
    );

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!String::from_utf8_lossy(&requests[0].body).contains("explicit-fixture-key"));
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert!(body.to_string().contains("TeacherDecision"));
    assert!(!body.to_string().contains("StructuredOutput"));
    assert!(!body.to_string().contains(request.decision_id.as_str()));
    assert!(!body.to_string().contains(request.root_task_id.as_str()));
    drop(requests);

    let row: (String, String) = shell
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT outcome, usage_json FROM decision_model_calls",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row.0, "proposal");
    let usage: serde_json::Value = serde_json::from_str(&row.1).unwrap();
    assert_eq!(usage["input_tokens"], 12);
    assert_eq!(usage["output_tokens"], 7);
    assert_eq!(usage["cost_micros"], serde_json::Value::Null);
    // The proposal follows the real pending Engine decision Input path.
    shell.drain();
    assert!(shell.queue.is_empty());
}

#[test]
fn a_nonpending_teacher_request_cannot_open_the_sdk_or_write_a_call() {
    let (mut shell, provider) = teacher_shell(false);
    shell.start_teacher_decision(decision_request("foreign-root", "foreign-decision", 1));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(shell.pending_teacher.is_none());
    let calls: u64 = shell
        .engine
        .storage()
        .read()
        .query_row("SELECT COUNT(*) FROM decision_model_calls", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(calls, 0);
    assert!(
        matches!(shell.queue.front(), Some(Input::Decision(proposal))
        if proposal.failure == Some(DecisionFailure::Unavailable))
    );
}

#[test]
fn an_altered_pending_teacher_request_cannot_open_the_sdk_or_write_a_call() {
    let (mut shell, provider) = teacher_shell(false);
    let mut forged = open_teacher_decision(&mut shell);
    forged.result_deadline_ms += 1;
    shell.start_teacher_decision(forged);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(shell.pending_teacher.is_none());
    let calls: u64 = shell
        .engine
        .storage()
        .read()
        .query_row("SELECT COUNT(*) FROM decision_model_calls", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(calls, 0);
}

#[test]
fn cancelling_teacher_turn_discards_late_output_and_a_new_root_gets_one_call() {
    let (mut shell, provider) = teacher_shell(true);
    let first = open_teacher_decision(&mut shell);
    shell.start_teacher_decision(first.clone());
    until_teacher(&mut shell, |_| provider.calls.load(Ordering::SeqCst) == 1);
    shell.carry(Output::CancelDecision(
        interaction_contracts::desktop::decision::DecisionCancel {
            decision_id: first.decision_id.clone(),
            request_generation: first.request_generation,
        },
    ));
    until_teacher(&mut shell, |s| s.pending_teacher.is_none());
    assert_eq!(provider.dropped.load(Ordering::SeqCst), 1);
    assert!(
        shell
            .queue
            .iter()
            .all(|input| !matches!(input, Input::Decision(_)))
    );

    let second = open_teacher_decision(&mut shell);
    assert_ne!(first.root_task_id, second.root_task_id);
    shell.start_teacher_decision(second.clone());
    until_teacher(&mut shell, |s| s.pending_teacher.is_none());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.dropped.load(Ordering::SeqCst), 2);
    let decisions = shell
        .queue
        .iter()
        .filter_map(|input| match input {
            Input::Decision(proposal) => Some(proposal),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].decision_id, second.decision_id);
    assert_eq!(decisions[0].request_generation, second.request_generation);
    let outcomes = shell
        .engine
        .storage()
        .read()
        .prepare("SELECT outcome FROM decision_model_calls ORDER BY started_ms, decision_id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(outcomes, ["cancelled", "proposal"]);
}

#[test]
fn shutdown_revokes_teacher_and_late_completion_cannot_be_installed() {
    let (mut shell, provider) = teacher_shell(true);
    let request = open_teacher_decision(&mut shell);
    shell.start_teacher_decision(request.clone());
    until_teacher(&mut shell, |_| provider.calls.load(Ordering::SeqCst) == 1);
    shell.close_model();
    assert_eq!(provider.dropped.load(Ordering::SeqCst), 1);
    assert!(shell.queue.is_empty());
    assert!(shell.pending_teacher.is_none());
    assert!(
        shell
            .teacher
            .as_mut()
            .unwrap()
            .begin(
                &decision_request("root-after-close", "decision-after-close", 1),
                shell.clock.now_ms()
            )
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let outcome: String = shell
        .engine
        .storage()
        .read()
        .query_row("SELECT outcome FROM decision_model_calls", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(outcome, "cancelled");
}
