//! Host integration regressions for the private computer session lifecycle.
//!
//! These tests inject a deterministic SDK transport and exercise the real
//! Engine, Worker, and private image/safety channel without a provider or GUI.

use std::collections::VecDeque;
use std::os::unix::net::UnixStream;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Sender},
};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use super::model_tests::fixture_world;
use crate::computer_image::ComputerImagePort;
use crate::computer_session::ComputerSession;
use async_trait::async_trait;
use bytes::Bytes;
use desktop_ipc::computer_control::{
    ComputerPortRequest, ComputerSafetyReply, ComputerSafetyRequest, read_port_request,
    write_safety_reply,
};
use desktop_ipc::computer_image::ComputerImagePacket;
use desktop_ipc::image::ImageFrame;
use futures::Stream;
use harness_adapter::computer::OpenAiComputerTransport;
use harness_adapter::computer_worker::ComputerWorker;
use harness_adapter::model::{ModelProtocol, ModelProvider, ModelRoute};
use interaction_contracts::desktop::operator::OperatorCommand;
use interaction_contracts::desktop::source::{AxActionName, FrameMeta, Rect, StructureSnapshot};
use interaction_contracts::desktop::task::RootState;
use interaction_engine::deliberation::accounting::ModelUsage;
use interaction_engine::deliberation::computer_accounting::ComputerCallCompletion;
use interaction_engine::deliberation::computer_accounting::ComputerCallOutcome;
use interaction_engine::engine::Input;
use interaction_engine::engine::computer::{ComputerCommand, ComputerInput};
use lingxi_llm_client::protocol::LlmError;
use lingxi_llm_client::{HttpRequest, StreamResponse, Transport};
use serde_json::{Value, json};
use sim_world::app::SnapshotMeta;

const PNG: &[u8] = include_bytes!("../../../contracts/desktop-v1/image-channel/fixture.png");
const FRAME_METADATA: &str =
    include_str!("../../../contracts/desktop-v1/image-channel/metadata.json");

fn route() -> ModelRoute {
    ModelRoute {
        preset_id: None,
        provider_id: ModelProvider::OpenAi,
        protocol: ModelProtocol::OpenAiResponses,
        base_url: "https://api.openai.com/v1".into(),
        model: "gpt-4.1-mini".into(),
        max_output_tokens: 4096,
    }
}

fn response(response_id: &str, output: Vec<Value>) -> Vec<Bytes> {
    [
        json!({"type":"response.created","response":{"id":response_id,"status":"in_progress","model":"gpt-4.1-mini","output":[]}}),
        json!({"type":"response.completed","response":{"id":response_id,"status":"completed","model":"gpt-4.1-mini","output":output,"usage":{"input_tokens":21,"output_tokens":8,"total_tokens":29}}}),
    ]
    .into_iter()
    .map(|value| Bytes::from(format!("data: {value}\n\n")))
    .collect()
}

fn computer_call(call_id: &str, actions: Vec<Value>, checks: Vec<Value>) -> Value {
    json!({
        "id": format!("item-{call_id}"),
        "type": "computer_call",
        "call_id": call_id,
        "pending_safety_checks": checks,
        "status": "completed",
        "actions": actions,
    })
}

#[derive(Default)]
struct FakeNetwork {
    responses: Mutex<VecDeque<Vec<Bytes>>>,
    requests: Mutex<Vec<HttpRequest>>,
    completed_streams: Arc<AtomicU64>,
}

impl FakeNetwork {
    fn new(responses: Vec<Vec<Bytes>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
            completed_streams: Arc::new(AtomicU64::new(0)),
        }
    }
}

struct FakeBody {
    chunks: VecDeque<Bytes>,
    completed_streams: Arc<AtomicU64>,
}

impl Stream for FakeBody {
    type Item = Result<Bytes, LlmError>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.chunks.pop_front().map(Ok))
    }
}

impl Drop for FakeBody {
    fn drop(&mut self) {
        self.completed_streams.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Transport for FakeNetwork {
    async fn send(&self, request: HttpRequest) -> Result<StreamResponse, LlmError> {
        self.requests.lock().unwrap().push(request);
        let chunks =
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| LlmError::InvalidRequest {
                    message: "host lifecycle fixture has no response left".into(),
                })?;
        Ok(StreamResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: Box::pin(FakeBody {
                chunks: chunks.into(),
                completed_streams: self.completed_streams.clone(),
            }),
        })
    }
}

fn worker(
    responses: Vec<Vec<Bytes>>,
    now_ms: u64,
) -> (ComputerWorker, Arc<FakeNetwork>, Arc<AtomicU64>) {
    let network = Arc::new(FakeNetwork::new(responses));
    let now = Arc::new(AtomicU64::new(now_ms));
    let clock = now.clone();
    let transport = OpenAiComputerTransport::with_transport(
        route(),
        "host-test-key".into(),
        "host-test-account".into(),
        Arc::new(move || clock.load(Ordering::SeqCst)),
        network.clone(),
    )
    .unwrap();
    (ComputerWorker::new(transport).unwrap(), network, now)
}

fn image_port(clock: Arc<AtomicU64>) -> (ComputerImagePort, UnixStream) {
    let (host, peer) = UnixStream::pair().unwrap();
    let port =
        ComputerImagePort::new(host, Arc::new(move || clock.load(Ordering::SeqCst))).unwrap();
    (port, peer)
}

fn widen_source_window(snapshot: &mut StructureSnapshot) {
    for window in &mut snapshot.windows {
        if window.kind == interaction_contracts::desktop::source::LayerKind::Window {
            window.frame.width = 640.0;
            window.frame.height = 400.0;
        }
    }
    for object in &mut snapshot.objects {
        if object.parent.is_none()
            && object.role == "AXWindow"
            && object.subrole.as_deref() == Some("AXStandardWindow")
        {
            object.frame.width = 640.0;
            object.frame.height = 400.0;
        }
        if object.instance_ref.as_str() == "btn-apply" {
            object
                .actions
                .retain(|action| *action != AxActionName::Press);
        }
    }
}

fn source_snapshot(
    world: &fixture_world::World,
    sequence: u64,
    observed_ms: u64,
) -> StructureSnapshot {
    let identity = world.engine.device().identity().unwrap();
    let mut snapshot = world.app.borrow().snapshot(&SnapshotMeta {
        snapshot_id: format!("snap-{sequence}"),
        session_epoch: identity.session_epoch,
        surface: identity.surface.clone(),
        clock_domain: identity.clock_domain.clone(),
        source_sequence: sequence,
        observed_ms,
    });
    widen_source_window(&mut snapshot);
    snapshot
}

fn complete_frame(snapshot: &StructureSnapshot, sequence: u64, at_ms: u64) -> FrameMeta {
    let mut frame: FrameMeta = serde_json::from_str(FRAME_METADATA).unwrap();
    frame.frame_id = format!("host-frame-{sequence}");
    frame.session_epoch = snapshot.session_epoch;
    frame.surface = snapshot.surface.clone();
    frame.clock_domain = snapshot.clock_domain.clone();
    frame.source_sequence = sequence;
    frame.source_ms = at_ms;
    let stream = frame.stream_info_mut().unwrap();
    stream.stream_id = "host-computer-stream".into();
    stream.image_frame_id = Some(format!("host-image-{sequence}"));
    stream.display_time_ticks = at_ms.saturating_mul(1_000_000).saturating_add(sequence);
    stream.display_time_ns = at_ms.saturating_mul(1_000_000).saturating_add(sequence);
    stream.received_uptime_ns = at_ms
        .saturating_mul(1_000_000)
        .saturating_add(20_000_000)
        .saturating_add(sequence);
    stream.received_unix_ms = 100_000 + at_ms;
    stream.callback_ms = at_ms;
    stream.source_clock = "mach_absolute".into();
    stream.window_id = snapshot
        .global_focus
        .known_window_for_app(&snapshot.app)
        .expect("the simulated target app reports its focused window")
        .to_owned();
    stream.pid = snapshot.app.pid;
    stream.window_frame = snapshot
        .windows
        .iter()
        .find(|window| window.window_id == stream.window_id)
        .unwrap()
        .frame;
    stream.content_rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 160.0,
        height: 100.0,
    };
    // This shared 320x200 PNG captures a 640x400-point window at 2x
    // display density and 0.25 content scaling (0.5 output pixels/point).
    stream.content_scale = 0.25;
    frame
}

fn fd5_peer(
    mut peer: UnixStream,
    clock: Arc<AtomicU64>,
    approve_safety: bool,
    expected_frames: usize,
    expected_safety: usize,
    safety_received: Option<Sender<ComputerSafetyRequest>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut frames = 0;
        let mut safety = 0;
        while frames < expected_frames || safety < expected_safety {
            match read_port_request(&mut peer, || Ok(())).unwrap() {
                ComputerPortRequest::Frame(request) => {
                    let packet = ComputerImagePacket {
                        fence: request.fence.clone(),
                        image: ImageFrame {
                            meta: request.expected_frame.clone(),
                            bytes: Arc::from(PNG),
                        },
                    };
                    packet
                        .write_to_with_check(
                            &request,
                            &mut peer,
                            || clock.load(Ordering::SeqCst),
                            || Ok(()),
                        )
                        .unwrap();
                    frames += 1;
                }
                ComputerPortRequest::Safety(request) => {
                    if let Some(signal) = &safety_received {
                        signal.send((*request).clone()).unwrap();
                    }
                    write_safety_reply(
                        &ComputerSafetyReply {
                            fence: request.fence.clone(),
                            approved: approve_safety,
                        },
                        &mut peer,
                        || Ok(()),
                    )
                    .unwrap();
                    safety += 1;
                }
            }
        }
    })
}

fn computer_world() -> fixture_world::World {
    let mut config = fixture_world::engine_config();
    config.computer_proposals_enabled = true;
    let mut world = fixture_world::World::with_config(config);
    world.async_applied_clicks.set(true);
    world.snapshot_transform = Some(Box::new(widen_source_window));
    world.start(fixture_world::TEXT_COMMIT);
    world.until(
        |world| {
            world
                .computer_commands
                .iter()
                .any(|command| matches!(command, ComputerCommand::Begin { .. }))
        },
        5_000,
    );
    world
}

fn computer_call_counts(world: &fixture_world::World) -> (i64, i64, i64, i64) {
    world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN finished_ms IS NOT NULL THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(COALESCE(json_extract(usage_json,'$.input_tokens'),0)),0),
                    COALESCE(SUM(COALESCE(json_extract(usage_json,'$.output_tokens'),0)),0)
             FROM computer_model_calls",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

fn computer_outcome_counts(world: &fixture_world::World, outcome: &str) -> i64 {
    world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT COUNT(*) FROM computer_model_calls WHERE outcome=?1",
            [outcome],
            |row| row.get(0),
        )
        .unwrap()
}

fn send_host_inputs(world: &mut fixture_world::World, inputs: Vec<Input>, seen: &mut Vec<Input>) {
    for input in inputs {
        seen.push(input.clone());
        world.send(input);
    }
}

fn feed_pending_host_commands(
    world: &mut fixture_world::World,
    session: &mut ComputerSession,
    clock: &AtomicU64,
    seen: &mut Vec<Input>,
) {
    loop {
        let commands = world.take_computer_commands();
        if commands.is_empty() {
            return;
        }
        for command in commands {
            clock.store(world.now, Ordering::SeqCst);
            let inputs = session.command(&mut world.engine, command, world.now);
            send_host_inputs(world, inputs, seen);
        }
    }
}

fn poll_host_until(
    world: &mut fixture_world::World,
    session: &mut ComputerSession,
    clock: &AtomicU64,
    timeout: Duration,
    done: impl Fn(&fixture_world::World) -> bool,
) -> Vec<Input> {
    let end = Instant::now() + timeout;
    let mut seen = Vec::new();
    while !done(world) {
        assert!(
            Instant::now() < end,
            "computer Host lifecycle did not settle\n{}",
            world.report()
        );
        clock.store(world.now, Ordering::SeqCst);
        let inputs = session.poll(&mut world.engine, world.now);
        send_host_inputs(world, inputs, &mut seen);
        feed_pending_host_commands(world, session, clock, &mut seen);
        if !done(world) {
            thread::sleep(Duration::from_millis(1));
        }
    }
    seen
}

fn take_begin(world: &mut fixture_world::World) -> ComputerCommand {
    let commands = world.take_computer_commands();
    let mut begins = commands
        .into_iter()
        .filter(|command| matches!(command, ComputerCommand::Begin { .. }));
    let begin = begins.next().expect("Engine emitted a computer Begin");
    assert!(
        begins.next().is_none(),
        "only one begin belongs to the live root"
    );
    begin
}

fn request_fresh_computer_frame(world: &mut fixture_world::World) {
    world.run_for(100);
    world.source_sequence += 2;
    let mut snapshot = source_snapshot(world, world.source_sequence, world.now);
    let frame = complete_frame(&snapshot, world.source_sequence - 1, world.now);
    snapshot.frame_ref = frame.stream_info().unwrap().image_frame_id.clone();
    // Native publishes the image first; the new image must not be paired with
    // the previous AX tree while its matching structure record is in flight.
    world.send(Input::Frame(Box::new(frame)));
    world.send(Input::Structure(Box::new(snapshot)));
}

fn model_usage_response(call_id: &str, actions: Vec<Value>, checks: Vec<Value>) -> Vec<Bytes> {
    response(
        &format!("response-{call_id}"),
        vec![computer_call(call_id, actions, checks)],
    )
}

fn send_command(
    world: &mut fixture_world::World,
    session: &mut ComputerSession,
    clock: &AtomicU64,
    command: ComputerCommand,
    seen: &mut Vec<Input>,
) {
    clock.store(world.now, Ordering::SeqCst);
    let inputs = session.command(&mut world.engine, command, world.now);
    send_host_inputs(world, inputs, seen);
    feed_pending_host_commands(world, session, clock, seen);
}

fn wait_for_network_requests(network: &FakeNetwork, count: usize) {
    let end = Instant::now() + Duration::from_secs(3);
    while network.requests.lock().unwrap().len() < count {
        assert!(
            Instant::now() < end,
            "fake OpenAI transport did not receive request {count}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_for_completed_streams(network: &FakeNetwork, count: u64) {
    let end = Instant::now() + Duration::from_secs(3);
    while network.completed_streams.load(Ordering::SeqCst) < count {
        assert!(
            Instant::now() < end,
            "fake provider stream did not cross the response-drop barrier"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_safety_decline_keeps_generation_usage_and_never_falls_back_to_device_input() {
    let mut world = computer_world();
    let submits_before = world.sent("submit");
    let applied_before = world.applied().len();
    let plans_before = world.planning_requests.len();
    let checks = vec![json!({
        "id":"safety-check-1",
        "code":"confirm",
        "message":"Confirm this proposed action",
    })];
    let (worker, network, clock) = worker(
        vec![
            model_usage_response("call-initial", vec![json!({"type":"screenshot"})], vec![]),
            model_usage_response(
                "call-proposal",
                vec![json!({"type":"click","button":"left","x":95,"y":172})],
                checks,
            ),
        ],
        world.now,
    );
    let (pixels, peer) = image_port(clock.clone());
    let mut session = ComputerSession::new(worker, pixels);
    let (safety_tx, safety_rx) = mpsc::channel();
    let fd5 = fd5_peer(peer, clock.clone(), false, 1, 1, Some(safety_tx));

    let mut seen = Vec::new();
    let begin = take_begin(&mut world);
    send_command(&mut world, &mut session, &clock, begin, &mut seen);
    let _ = poll_host_until(
        &mut world,
        &mut session,
        &clock,
        Duration::from_secs(3),
        |world| computer_call_counts(world).1 >= 1,
    );

    request_fresh_computer_frame(&mut world);
    feed_pending_host_commands(&mut world, &mut session, &clock, &mut seen);
    let second_call = poll_host_until(
        &mut world,
        &mut session,
        &clock,
        Duration::from_secs(4),
        |world| computer_call_counts(world).1 >= 2,
    );
    seen.extend(second_call);
    let safety_request = safety_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("Host must send reported safety checks over private FD5");
    assert_eq!(safety_request.pending_safety_checks.len(), 1);
    assert_eq!(safety_request.proposal.as_ref().unwrap().actions.len(), 1);
    assert_eq!(network.requests.lock().unwrap().len(), 2);

    let declined = poll_host_until(
        &mut world,
        &mut session,
        &clock,
        Duration::from_secs(3),
        |world| {
            matches!(
                world.engine.root_state(),
                Some(RootState::Pausing | RootState::Suspended)
            )
        },
    );
    seen.extend(declined);
    assert!(seen.iter().any(|input| matches!(
        input,
        Input::Computer(event)
            if matches!(event.as_ref(), ComputerInput::SafetyUnconfirmed { .. })
    )));
    assert_eq!(computer_call_counts(&world), (2, 2, 42, 16));
    assert_eq!(computer_outcome_counts(&world, "proposal"), 2);

    world.run_for(500);
    assert_eq!(
        world.sent("submit"),
        submits_before,
        "decline cannot reach ordinary Submit"
    );
    assert_eq!(
        world.applied().len(),
        applied_before,
        "decline cannot create a local effect"
    );
    assert_eq!(
        world.planning_requests.len(),
        plans_before,
        "decline cannot retry via fallback planning"
    );
    session.revoke();
    fd5.join().unwrap();
}

fn run_computer_click_case(
    async_known_completion: bool,
    inject_missing_input_end: bool,
) -> (fixture_world::World, Vec<Input>) {
    let mut world = computer_world();
    assert!(!async_known_completion || !inject_missing_input_end);
    if inject_missing_input_end {
        world.inject_missing_input_end_fault_for_next_completed_click();
    }
    world.async_applied_clicks.set(async_known_completion);
    let submits_before = world.sent("submit");
    let applied_before = world.applied().len();
    let (worker, network, clock) = worker(
        vec![
            model_usage_response("call-initial", vec![json!({"type":"screenshot"})], vec![]),
            model_usage_response(
                "call-apply",
                vec![json!({"type":"click","button":"left","x":95,"y":172})],
                vec![],
            ),
        ],
        world.now,
    );
    let (pixels, peer) = image_port(clock.clone());
    let mut session = ComputerSession::new(worker, pixels);
    let fd5 = fd5_peer(peer, clock.clone(), false, 1, 0, None);
    let mut seen = Vec::new();

    let begin = take_begin(&mut world);
    send_command(&mut world, &mut session, &clock, begin, &mut seen);
    let _ = poll_host_until(
        &mut world,
        &mut session,
        &clock,
        Duration::from_secs(3),
        |world| computer_call_counts(world).1 >= 1,
    );
    request_fresh_computer_frame(&mut world);
    feed_pending_host_commands(&mut world, &mut session, &clock, &mut seen);
    let proposal_events = poll_host_until(
        &mut world,
        &mut session,
        &clock,
        Duration::from_secs(4),
        |world| world.sent("submit") > submits_before,
    );
    seen.extend(proposal_events);

    assert!(seen.iter().any(|input| matches!(
        input,
        Input::Computer(event)
            if matches!(event.as_ref(), ComputerInput::Proposal { .. })
    )));
    assert_eq!(computer_call_counts(&world), (2, 2, 42, 16));
    assert_eq!(world.sent("submit"), submits_before + 1);
    assert_eq!(world.applied().len(), applied_before + 1);
    assert_eq!(
        world.app.borrow().save_state(),
        sim_world::app::SaveState::Saved
    );
    assert_eq!(network.requests.lock().unwrap().len(), 2);

    world.until(
        |world| {
            world.computer_log.iter().any(|command| {
                matches!(
                    command,
                    ComputerCommand::BatchOutcome { terminal: true, .. }
                )
            })
        },
        5_000,
    );
    feed_pending_host_commands(&mut world, &mut session, &clock, &mut seen);
    assert!(!seen.iter().any(|input| matches!(input, Input::Computer(event)
        if matches!(event.as_ref(), ComputerInput::Failed { .. } | ComputerInput::ObservationFailed { .. }))));

    session.revoke();
    fd5.join().unwrap();
    (world, seen)
}

#[test]
fn a_completed_proposal_reaches_the_ordinary_attempt_and_application_effect() {
    let (world, _) = run_computer_click_case(true, false);
    assert!(world.computer_log.iter().any(|command| matches!(
        command,
        ComputerCommand::BatchOutcome {
            completed_prefix: 1,
            total: 1,
            terminal: true,
            ..
        }
    )));
}

#[test]
fn a_sync_completed_receipt_without_input_end_does_not_advance_the_batch() {
    // Inject a receipt-forwarding metadata fault; the simulated core still
    // records the synchronous input return in its own receipt.
    let (mut world, _) = run_computer_click_case(false, true);
    let submits_after_click = world.sent("submit");
    let inputs_after_click = world.applied().len();
    world.until(|world| world.outcome() == Some(RootState::Completed), 5_000);
    world.run_for(500);
    assert_eq!(
        world.sent("submit"),
        submits_after_click,
        "the action must never be replayed"
    );
    assert_eq!(
        world.applied().len(),
        inputs_after_click,
        "the completed Root must not submit another input"
    );
    assert!(world.computer_log.iter().any(|command| matches!(
        command,
        ComputerCommand::BatchOutcome {
            completed_prefix: 0,
            total: 1,
            terminal: true,
            reason: Some(reason),
            ..
        } if reason.contains("Unknown") && reason.contains("action_index: 0")
    )));
    assert!(!world.computer_log.iter().any(|command| matches!(
        command,
        ComputerCommand::BatchOutcome {
            completed_prefix: 1,
            total: 1,
            terminal: true,
            ..
        }
    )));
}

#[test]
fn local_observation_failure_is_not_a_safety_decline() {
    let mut world = computer_world();
    let submits_before = world.sent("submit");
    let ComputerCommand::Begin { identity, .. } = take_begin(&mut world) else {
        unreachable!()
    };
    world.send(Input::Computer(Box::new(
        ComputerInput::ObservationFailed {
            identity,
            reason: "pixels were unavailable".into(),
        },
    )));
    let timeline = world
        .engine
        .timeline()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>();
    assert!(timeline.iter().any(|text| {
        text.contains("the computer action scope ended; an explicit operator decision is required")
    }));
    assert!(
        !timeline
            .iter()
            .any(|text| text.contains("computer safety checks were not confirmed"))
    );
    assert!(matches!(
        world.engine.root_state(),
        Some(RootState::Pausing | RootState::Suspended)
    ));
    assert_eq!(world.sent("submit"), submits_before);
    assert_eq!(computer_call_counts(&world), (0, 0, 0, 0));
}

#[test]
fn a_late_cancel_closes_usage_and_a_new_root_starts_without_the_old_continuation() {
    let mut world = computer_world();
    let submits_before = world.sent("submit");
    let applied_before = world.applied().len();
    let (worker, network, clock) = worker(
        vec![
            model_usage_response("call-old-root", vec![json!({"type":"screenshot"})], vec![]),
            model_usage_response("call-new-root", vec![json!({"type":"screenshot"})], vec![]),
        ],
        world.now,
    );
    let (pixels, peer) = image_port(clock.clone());
    let mut session = ComputerSession::new(worker, pixels);
    let mut seen = Vec::new();
    let begin = take_begin(&mut world);
    send_command(&mut world, &mut session, &clock, begin, &mut seen);
    wait_for_network_requests(&network, 1);
    // Let the transport finish, but hold its result at the Host polling edge
    // while the Root ends. The accounting close must still retain its usage.
    wait_for_completed_streams(&network, 1);
    thread::sleep(Duration::from_millis(10));
    world.operator(OperatorCommand::StopTask { root_task_id: None });
    feed_pending_host_commands(&mut world, &mut session, &clock, &mut seen);
    let late = poll_host_until(
        &mut world,
        &mut session,
        &clock,
        Duration::from_secs(3),
        |world| computer_call_counts(world).1 >= 1,
    );
    assert!(late.iter().any(|input| matches!(
        input,
        Input::Computer(event) if matches!(
            event.as_ref(),
            ComputerInput::Finished { completion }
                | ComputerInput::Failed { completion, .. }
                if completion.outcome == ComputerCallOutcome::Cancelled
                    && completion.usage.input_tokens == Some(21)
                    && completion.usage.output_tokens == Some(8)
        )
    )));
    seen.extend(late);
    let counts = computer_call_counts(&world);
    assert_eq!(counts, (1, 1, 21, 8));
    assert_eq!(computer_outcome_counts(&world, "cancelled"), 1);
    assert_eq!(world.sent("submit"), submits_before);
    assert_eq!(world.applied().len(), applied_before);

    world.start(fixture_world::TEXT_COMMIT);
    world.until(
        |world| {
            world
                .computer_commands
                .iter()
                .any(|command| matches!(command, ComputerCommand::Begin { .. }))
        },
        5_000,
    );
    let begin = take_begin(&mut world);
    send_command(&mut world, &mut session, &clock, begin, &mut seen);
    wait_for_network_requests(&network, 2);
    let requests = network.requests.lock().unwrap();
    let second_body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert!(second_body.get("previous_response_id").is_none());
    assert!(!second_body.to_string().contains("response-call-old-root"));
    assert!(!second_body.to_string().contains("call-old-root"));
    drop(requests);

    let _ = poll_host_until(
        &mut world,
        &mut session,
        &clock,
        Duration::from_secs(3),
        |world| computer_call_counts(world).1 >= 2,
    );
    world.operator(OperatorCommand::StopTask { root_task_id: None });
    feed_pending_host_commands(&mut world, &mut session, &clock, &mut seen);
    session.revoke();
    drop(peer);
}

#[test]
fn shell_shutdown_closes_queued_completion_usage_without_installing_its_proposal() {
    let mut world = computer_world();
    let ComputerCommand::Begin { identity, .. } = take_begin(&mut world) else {
        unreachable!()
    };
    world
        .engine
        .computer_call_started(world.now, &identity, "openai", "gpt-4.1-mini")
        .unwrap();
    let completion = ComputerCallCompletion {
        identity,
        at_ms: world.now,
        outcome: ComputerCallOutcome::Proposal,
        usage: ModelUsage {
            input_tokens: Some(21),
            output_tokens: Some(8),
            cost_micros: None,
        },
    };
    let attempts_before: i64 = world
        .engine
        .storage()
        .read()
        .query_row("SELECT COUNT(*) FROM attempts", [], |row| row.get(0))
        .unwrap();
    let (mut shell, _provider) = super::model_tests::shell(false);
    shell.engine = world.engine;
    shell
        .queue
        .push_back(Input::Computer(Box::new(ComputerInput::Finished {
            completion,
        })));

    shell.close_model();

    assert!(shell.queue.is_empty());
    let (finished, outcome, input_tokens, output_tokens): (Option<i64>, String, i64, i64) = shell
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT finished_ms,outcome,
                    json_extract(usage_json,'$.input_tokens'),
                    json_extract(usage_json,'$.output_tokens')
             FROM computer_model_calls",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert!(finished.is_some());
    assert_eq!(outcome, "revoked");
    assert_eq!((input_tokens, output_tokens), (21, 8));
    let attempts_after: i64 = shell
        .engine
        .storage()
        .read()
        .query_row("SELECT COUNT(*) FROM attempts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(attempts_after, attempts_before);
    assert!(
        shell.queue.is_empty(),
        "shutdown must consume accounting only"
    );
}
