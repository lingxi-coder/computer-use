//! SDK-codec and continuation-ledger checks. No live provider or Device input.

use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use base64::Engine as _;
use bytes::Bytes;
use futures::stream;
use harness_adapter::computer::{
    ComputerActionOutcome, ComputerBatchOutcome, ComputerModelStep, OpenAiComputerTransport,
};
use harness_adapter::computer_worker::{
    ComputerScreenshotPhase, ComputerWorker, ComputerWorkerResult, ScopeFinish,
};
use harness_adapter::model::{ModelProtocol, ModelProvider, ModelRoute};
use interaction_contracts::desktop::computer::{ComputerFence, ComputerFrameRequest};
use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
use interaction_contracts::desktop::source::FrameMeta;
use lingxi_llm_client::protocol::LlmError;
use lingxi_llm_client::{HttpRequest, StreamResponse, Transport};
use serde_json::{Value, json};
use tokio::sync::watch;

const PNG: &[u8] = include_bytes!("../../../contracts/desktop-v1/image-channel/fixture.png");
const FRAME_METADATA: &str =
    include_str!("../../../contracts/desktop-v1/image-channel/metadata.json");
const KEY: &str = "isolated-computer-test-key";

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

fn fence(
    generation: u64,
    snapshot: &str,
    structure_sequence: u64,
    deadline_ms: u64,
) -> ComputerFence {
    ComputerFence {
        request_id: RequestId::new(format!("request-{generation}")).unwrap(),
        generation,
        root_task_id: RootTaskId::new("root-computer-test").unwrap(),
        task_revision: 7,
        root_epoch: 3,
        snapshot_id: snapshot.into(),
        structure_sequence,
        deadline_ms,
    }
}

fn frame_request(
    generation: u64,
    snapshot: &str,
    structure_sequence: u64,
    source_sequence: u64,
    source_ms: u64,
    display_time_ns: u64,
) -> ComputerFrameRequest {
    let mut expected_frame: FrameMeta = serde_json::from_str(FRAME_METADATA).unwrap();
    expected_frame.frame_id = format!("frame-{source_sequence}");
    expected_frame.source_sequence = source_sequence;
    expected_frame.source_ms = source_ms;
    let stream = expected_frame.stream_info_mut().unwrap();
    stream.image_frame_id = Some(format!("image-{source_sequence}"));
    stream.display_time_ticks = display_time_ns;
    stream.display_time_ns = display_time_ns;
    stream.received_uptime_ns = display_time_ns + 20_000_000;
    stream.received_unix_ms = source_ms + 30;
    stream.callback_ms = source_ms + 1;
    ComputerFrameRequest {
        fence: fence(
            generation,
            snapshot,
            structure_sequence,
            90_000 - generation,
        ),
        expected_frame,
    }
}

fn png_data_uri() -> String {
    png_data_uri_for(PNG)
}

fn png_data_uri_for(bytes: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn check(id: &str) -> Value {
    json!({"id":id,"code":"safety-test","message":"Confirm this operation"})
}

fn computer_call(call_id: &str, actions: Vec<Value>, checks: Vec<Value>) -> Value {
    json!({
        "id":format!("item-{call_id}"),
        "type":"computer_call",
        "call_id":call_id,
        "pending_safety_checks":checks,
        "status":"completed",
        "actions":actions,
    })
}

fn response(response_id: &str, output: Vec<Value>) -> Vec<Bytes> {
    vec![
        sse(
            json!({"type":"response.created","response":{"id":response_id,"status":"in_progress","model":"gpt-4.1-mini","output":[]}}),
        ),
        sse(
            json!({"type":"response.completed","response":{"id":response_id,"status":"completed","model":"gpt-4.1-mini","output":output,"usage":{"input_tokens":21,"output_tokens":8,"total_tokens":29}}}),
        ),
    ]
}

fn sse(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

struct MockTransport {
    replies: Mutex<VecDeque<Vec<Bytes>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl MockTransport {
    fn new(replies: Vec<Vec<Bytes>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn send(&self, request: HttpRequest) -> Result<StreamResponse, LlmError> {
        self.requests.lock().unwrap().push(request);
        let chunks =
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| LlmError::InvalidRequest {
                    message: "mock has no response left".into(),
                })?;
        Ok(StreamResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: Box::pin(stream::iter(chunks.into_iter().map(Ok::<_, LlmError>))),
        })
    }
}

fn make_transport(
    replies: Vec<Vec<Bytes>>,
) -> (OpenAiComputerTransport, Arc<MockTransport>, Arc<AtomicU64>) {
    let network = Arc::new(MockTransport::new(replies));
    let clock = Arc::new(AtomicU64::new(10_000));
    let clock_reader = clock.clone();
    let transport = OpenAiComputerTransport::with_transport(
        route(),
        KEY.into(),
        "local-account-1".into(),
        Arc::new(move || clock_reader.load(Ordering::SeqCst)),
        network.clone(),
    )
    .unwrap();
    (transport, network, clock)
}

fn set_now(clock: &AtomicU64, milliseconds: u64) {
    clock.store(milliseconds, Ordering::SeqCst);
}

fn wait_worker(worker: &mut ComputerWorker) -> ComputerWorkerResult {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if let Some(result) = worker.poll() {
            return result;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "computer worker did not close"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn worker_keeps_the_ordered_loop_off_the_host_thread() {
    let first = response(
        "resp_worker_first",
        vec![computer_call(
            "call_worker_first",
            vec![json!({"type":"screenshot"})],
            vec![],
        )],
    );
    let second = response(
        "resp_worker_second",
        vec![computer_call(
            "call_worker_second",
            vec![
                json!({"type":"click","button":"left","x":4,"y":7}),
                json!({"type":"type","text":"penguin"}),
            ],
            vec![],
        )],
    );
    let final_response = response(
        "resp_worker_final",
        vec![
            json!({"id":"worker-message","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Observed the updated screen","annotations":[]} ]}),
        ],
    );
    let (transport, network, clock) = make_transport(vec![first, second, final_response]);
    let mut worker = ComputerWorker::new(transport).unwrap();
    let begin_fence = fence(1, "snapshot-1", 1, 90_000);
    worker
        .start_begin(begin_fence.clone(), "Search for penguin".into())
        .unwrap();
    assert!(
        worker
            .start_begin(begin_fence.clone(), "another task".into())
            .is_err()
    );
    let initial = wait_worker(&mut worker);
    assert_eq!(initial.fence, begin_fence);
    assert!(matches!(
        initial.result,
        Ok(ComputerModelStep::ScreenshotRequested { .. })
    ));
    assert_eq!(initial.usage.input_tokens, Some(21));
    assert!(worker.poll().is_none());

    set_now(&clock, 10_010);
    let request = frame_request(2, "snapshot-2", 2, 2, 10_005, 5_000_000_000);
    worker
        .start_screenshot(
            ComputerScreenshotPhase::Initial,
            request.clone(),
            desktop_ipc::image::ImageFrame {
                meta: request.expected_frame.clone(),
                bytes: Arc::from(PNG),
            },
            vec![],
        )
        .unwrap();
    let actions = wait_worker(&mut worker);
    let Ok(ComputerModelStep::ActionsProposed { proposal, .. }) = actions.result else {
        panic!("expected actions")
    };
    assert_eq!(proposal.actions.len(), 2);
    set_now(&clock, 10_020);
    worker
        .record_batch_outcome(
            &proposal,
            ComputerBatchOutcome {
                fence: proposal.fence.clone(),
                actions: vec![
                    ComputerActionOutcome {
                        index: 0,
                        completed: true,
                    },
                    ComputerActionOutcome {
                        index: 1,
                        completed: true,
                    },
                ],
            },
        )
        .unwrap();
    set_now(&clock, 10_030);
    let request = frame_request(3, "snapshot-3", 3, 3, 10_025, 5_100_000_000);
    worker
        .start_screenshot(
            ComputerScreenshotPhase::Fresh,
            request.clone(),
            desktop_ipc::image::ImageFrame {
                meta: request.expected_frame.clone(),
                bytes: Arc::from(PNG),
            },
            vec![],
        )
        .unwrap();
    assert!(matches!(
        wait_worker(&mut worker).result,
        Ok(ComputerModelStep::FinishedObservation { .. })
    ));
    assert_eq!(network.requests.lock().unwrap().len(), 3);
}

#[test]
fn a_new_root_after_scope_reset_starts_without_the_old_response_continuation() {
    let first = response(
        "response-old-root",
        vec![computer_call(
            "call-old-root",
            vec![json!({"type":"screenshot"})],
            vec![],
        )],
    );
    let second = response(
        "response-new-root",
        vec![computer_call(
            "call-new-root",
            vec![json!({"type":"screenshot"})],
            vec![],
        )],
    );
    let (transport, network, _) = make_transport(vec![first, second]);
    let mut worker = ComputerWorker::new(transport).unwrap();
    let old_fence = fence(1, "old-snapshot", 1, 90_000);
    worker
        .start_begin(old_fence.clone(), "old root goal".into())
        .unwrap();
    assert!(matches!(
        wait_worker(&mut worker).result,
        Ok(ComputerModelStep::ScreenshotRequested { .. })
    ));

    let mut new_fence = fence(1, "new-snapshot", 1, 90_000);
    new_fence.root_task_id = RootTaskId::new("root-computer-next").unwrap();
    new_fence.request_id = RequestId::new("request-new-root").unwrap();
    let rejected = worker
        .start_begin(new_fence.clone(), "new root goal".into())
        .unwrap_err();
    assert_eq!(
        rejected.kind,
        harness_adapter::computer_worker::ComputerWorkerStartErrorKind::ScopeEnded
    );
    assert_eq!(
        worker.finish_scope(&old_fence).unwrap(),
        ScopeFinish::Closed
    );
    worker.reset_ledger().unwrap();

    worker
        .start_begin(new_fence, "new root goal".into())
        .unwrap();
    assert!(matches!(
        wait_worker(&mut worker).result,
        Ok(ComputerModelStep::ScreenshotRequested { .. })
    ));

    let requests = network.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let old_request: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(old_request.to_string().contains("old root goal"));
    let new_request: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert!(new_request.to_string().contains("new root goal"));
    assert!(new_request.get("previous_response_id").is_none());
    assert!(!new_request.to_string().contains("response-old-root"));
    assert!(!new_request.to_string().contains("call-old-root"));
}

#[test]
fn failed_computer_generation_retains_its_reported_usage() {
    let invalid = response(
        "resp_invalid_usage",
        vec![
            computer_call(
                "call_invalid_usage",
                vec![json!({"type":"screenshot"})],
                vec![],
            ),
            computer_call(
                "call_duplicate_usage",
                vec![json!({"type":"screenshot"})],
                vec![],
            ),
        ],
    );
    let (transport, network, _) = make_transport(vec![invalid]);
    let mut worker = ComputerWorker::new(transport).unwrap();
    worker
        .start_begin(
            fence(1, "snapshot-1", 1, 90_000),
            "Inspect this window".into(),
        )
        .unwrap();
    let result = wait_worker(&mut worker);
    assert!(result.result.is_err());
    assert_eq!(
        result.status,
        harness_adapter::computer_worker::ComputerWorkerStatus::Invalid
    );
    assert_eq!(result.usage.input_tokens, Some(21));
    assert_eq!(result.usage.output_tokens, Some(8));
    assert_eq!(network.requests.lock().unwrap().len(), 1);
}

#[test]
fn screenshot_first_proposes_ordered_actions_and_pairs_fresh_original_output() {
    let safety = vec![check("check-screenshot")];
    let batch_safety = vec![check("check-input")];
    let (mut transport, network, clock) = make_transport(vec![
        response(
            "response-screenshot",
            vec![computer_call(
                "call-screenshot",
                vec![json!({"type":"screenshot"})],
                safety.clone(),
            )],
        ),
        response(
            "response-actions",
            vec![computer_call(
                "call-actions",
                vec![
                    json!({"type":"click","button":"left","x":48.5,"y":63.0}),
                    json!({"type":"keypress","keys":["CTRL","A"]}),
                ],
                batch_safety.clone(),
            )],
        ),
        response(
            "response-next",
            vec![computer_call(
                "call-next-screenshot",
                vec![json!({"type":"screenshot"})],
                vec![],
            )],
        ),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (cancel_tx, mut cancel) = watch::channel(false);
    let first_fence = fence(1, "snapshot-1", 1, 90_000);
    let first = runtime
        .block_on(transport.begin(first_fence, "Open the selected task", &mut cancel))
        .unwrap();
    assert!(matches!(
        first,
        ComputerModelStep::ScreenshotRequested { ref call_id, .. }
            if call_id == "call-screenshot"
    ));

    set_now(&clock, 10_005);
    let first_frame = frame_request(2, "snapshot-2", 2, 10, 10_001, 1_000_000_010);
    let after_first_image = runtime
        .block_on(
            transport.submit_initial_screenshot(
                first_frame.clone(),
                png_data_uri(),
                safety
                    .iter()
                    .map(|value| serde_json::from_value(value.clone()).unwrap())
                    .collect(),
                &mut cancel,
            ),
        )
        .unwrap();
    let proposal = match after_first_image {
        ComputerModelStep::ActionsProposed {
            proposal,
            call_id,
            pending_safety_checks,
            usage,
        } => {
            assert_eq!(call_id, "call-actions");
            assert_eq!(pending_safety_checks.len(), 1);
            assert_eq!(usage.cost_micros, None);
            proposal
        }
        other => panic!("expected ordered action batch, got {other:?}"),
    };
    assert_eq!(proposal.fence, first_frame.fence);
    assert!(matches!(
        &proposal.actions[0],
        interaction_contracts::desktop::computer::ComputerActionProposal::Click { x, .. }
            if *x == 48.5
    ));
    assert!(matches!(
        proposal.actions[1],
        interaction_contracts::desktop::computer::ComputerActionProposal::Keypress { .. }
    ));

    set_now(&clock, 11_000);
    transport
        .record_batch_outcome(
            &proposal,
            ComputerBatchOutcome {
                fence: proposal.fence.clone(),
                actions: vec![
                    ComputerActionOutcome {
                        index: 0,
                        completed: true,
                    },
                    ComputerActionOutcome {
                        index: 1,
                        completed: true,
                    },
                ],
            },
        )
        .unwrap();
    set_now(&clock, 11_005);
    let next_frame = frame_request(3, "snapshot-3", 3, 11, 11_001, 1_000_000_011);
    let next = runtime
        .block_on(
            transport.submit_fresh_screenshot(
                next_frame.clone(),
                png_data_uri(),
                batch_safety
                    .iter()
                    .map(|value| serde_json::from_value(value.clone()).unwrap())
                    .collect(),
                &mut cancel,
            ),
        )
        .unwrap();
    assert!(matches!(
        next,
        ComputerModelStep::ScreenshotRequested { ref call_id, .. }
            if call_id == "call-next-screenshot"
    ));

    let sent = network.requests.lock().unwrap();
    assert_eq!(sent.len(), 3);
    let first_body: Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert_eq!(first_body["tools"][0]["type"], "computer");
    assert!(!first_body.to_string().contains("data:image/png"));
    assert!(!format!("{:?}", sent[0]).contains(KEY));
    let second_body: Value = serde_json::from_slice(&sent[1].body).unwrap();
    assert_eq!(second_body["previous_response_id"], "response-screenshot");
    assert_eq!(second_body["input"][0]["call_id"], "call-screenshot");
    assert_eq!(
        second_body["input"][0]["acknowledged_safety_checks"][0]["id"],
        "check-screenshot"
    );
    assert_eq!(second_body["input"][0]["output"]["detail"], "original");
    assert_eq!(
        second_body["input"][0]["output"]["image_url"],
        png_data_uri()
    );
    let third_body: Value = serde_json::from_slice(&sent[2].body).unwrap();
    assert_eq!(third_body["previous_response_id"], "response-actions");
    assert_eq!(third_body["input"][0]["call_id"], "call-actions");
    assert_eq!(
        third_body["input"][0]["acknowledged_safety_checks"][0]["id"],
        "check-input"
    );
    assert_eq!(
        third_body["input"][0]["output"]["image_url"],
        png_data_uri()
    );
    drop(sent);
    cancel_tx.send(true).unwrap();
}

#[test]
fn partial_safety_acknowledgement_does_not_send_or_default_ack() {
    let safety = vec![check("check-screenshot")];
    let (mut transport, network, clock) = make_transport(vec![response(
        "response-screenshot",
        vec![computer_call(
            "call-screenshot",
            vec![json!({"type":"screenshot"})],
            safety,
        )],
    )]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_cancel_tx, mut cancel) = watch::channel(false);
    runtime
        .block_on(transport.begin(fence(1, "snapshot-1", 1, 90_000), "read this", &mut cancel))
        .unwrap();
    set_now(&clock, 10_005);
    assert!(
        runtime
            .block_on(transport.submit_initial_screenshot(
                frame_request(2, "snapshot-2", 2, 10, 10_001, 1_000_000_010),
                png_data_uri(),
                Vec::new(),
                &mut cancel,
            ))
            .is_err()
    );
    assert_eq!(network.requests.lock().unwrap().len(), 1);
}

#[test]
fn initial_screenshot_captured_before_the_model_request_is_rejected() {
    let (mut transport, network, clock) = make_transport(vec![response(
        "response-screenshot",
        vec![computer_call(
            "call-screenshot",
            vec![json!({"type":"screenshot"})],
            vec![],
        )],
    )]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_cancel_tx, mut cancel) = watch::channel(false);
    runtime
        .block_on(transport.begin(fence(1, "snapshot-1", 1, 90_000), "inspect", &mut cancel))
        .unwrap();

    set_now(&clock, 10_005);
    assert!(
        runtime
            .block_on(transport.submit_initial_screenshot(
                frame_request(2, "snapshot-2", 2, 10, 10_000, 1_000_000_010),
                png_data_uri(),
                vec![],
                &mut cancel,
            ))
            .is_err()
    );
    assert!(
        runtime
            .block_on(transport.submit_initial_screenshot(
                frame_request(2, "snapshot-2", 2, 10, 10_001, 1_000_000_010),
                png_data_uri(),
                vec![],
                &mut cancel,
            ))
            .is_err()
    );
    assert_eq!(network.requests.lock().unwrap().len(), 1);
}

#[test]
fn header_only_png_and_wrong_dimensions_never_continue_the_model_call() {
    for (response_id, image, change_dimensions) in [
        ("response-header", png_data_uri_for(&PNG[..8]), false),
        ("response-dimensions", png_data_uri(), true),
    ] {
        let (mut transport, network, clock) = make_transport(vec![response(
            response_id,
            vec![computer_call(
                "call-screenshot",
                vec![json!({"type":"screenshot"})],
                vec![],
            )],
        )]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_cancel_tx, mut cancel) = watch::channel(false);
        runtime
            .block_on(transport.begin(fence(1, "snapshot-1", 1, 90_000), "inspect", &mut cancel))
            .unwrap();
        set_now(&clock, 10_005);
        let mut frame = frame_request(2, "snapshot-2", 2, 10, 10_001, 1_000_000_010);
        if change_dimensions {
            frame.expected_frame.width_px += 1;
        }
        assert!(
            runtime
                .block_on(transport.submit_initial_screenshot(frame, image, vec![], &mut cancel,))
                .is_err()
        );
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn partial_outcome_poisons_the_single_call_and_cannot_be_replaced_by_success() {
    let (mut transport, network, clock) = make_transport(vec![
        response(
            "response-screenshot",
            vec![computer_call(
                "call-screenshot",
                vec![json!({"type":"screenshot"})],
                vec![],
            )],
        ),
        response(
            "response-actions",
            vec![computer_call(
                "call-actions",
                vec![json!({"type":"wait"})],
                vec![],
            )],
        ),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_cancel_tx, mut cancel) = watch::channel(false);
    runtime
        .block_on(transport.begin(fence(1, "snapshot-1", 1, 90_000), "wait", &mut cancel))
        .unwrap();
    set_now(&clock, 10_005);
    let frame = frame_request(2, "snapshot-2", 2, 10, 10_001, 1_000_000_010);
    let step = runtime
        .block_on(transport.submit_initial_screenshot(frame, png_data_uri(), vec![], &mut cancel))
        .unwrap();
    let proposal = match step {
        ComputerModelStep::ActionsProposed { proposal, .. } => proposal,
        other => panic!("expected action proposal, got {other:?}"),
    };
    assert!(
        transport
            .record_batch_outcome(
                &proposal,
                ComputerBatchOutcome {
                    fence: proposal.fence.clone(),
                    actions: vec![ComputerActionOutcome {
                        index: 0,
                        completed: false,
                    }],
                },
            )
            .is_err()
    );
    assert!(
        transport
            .record_batch_outcome(
                &proposal,
                ComputerBatchOutcome {
                    fence: proposal.fence.clone(),
                    actions: vec![ComputerActionOutcome {
                        index: 0,
                        completed: true,
                    }],
                },
            )
            .is_err()
    );
    assert_eq!(network.requests.lock().unwrap().len(), 2);
}

#[test]
fn missing_stream_is_rejected_without_panicking_and_poisons_the_call() {
    let (mut transport, network, clock) = make_transport(vec![
        response(
            "response-screenshot",
            vec![computer_call(
                "call-screenshot",
                vec![json!({"type":"screenshot"})],
                vec![],
            )],
        ),
        response(
            "response-actions",
            vec![computer_call(
                "call-actions",
                vec![json!({"type":"wait"})],
                vec![],
            )],
        ),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_cancel_tx, mut cancel) = watch::channel(false);
    runtime
        .block_on(transport.begin(fence(1, "snapshot-1", 1, 90_000), "inspect", &mut cancel))
        .unwrap();
    set_now(&clock, 10_005);
    let proposal = match runtime
        .block_on(transport.submit_initial_screenshot(
            frame_request(2, "snapshot-2", 2, 10, 10_001, 1_000_000_010),
            png_data_uri(),
            vec![],
            &mut cancel,
        ))
        .unwrap()
    {
        ComputerModelStep::ActionsProposed { proposal, .. } => proposal,
        other => panic!("expected action proposal, got {other:?}"),
    };
    set_now(&clock, 10_020);
    transport
        .record_batch_outcome(
            &proposal,
            ComputerBatchOutcome {
                fence: proposal.fence.clone(),
                actions: vec![ComputerActionOutcome {
                    index: 0,
                    completed: true,
                }],
            },
        )
        .unwrap();
    set_now(&clock, 10_025);
    let mut missing_stream = frame_request(3, "snapshot-3", 3, 11, 10_021, 1_000_000_011);
    missing_stream.expected_frame.provenance = None;
    assert!(
        runtime
            .block_on(transport.submit_fresh_screenshot(
                missing_stream,
                png_data_uri(),
                vec![],
                &mut cancel,
            ))
            .is_err()
    );
    assert!(
        runtime
            .block_on(transport.submit_fresh_screenshot(
                frame_request(3, "snapshot-3", 3, 11, 10_021, 1_000_000_011),
                png_data_uri(),
                vec![],
                &mut cancel,
            ))
            .is_err()
    );
    assert_eq!(network.requests.lock().unwrap().len(), 2);
}

#[test]
fn newly_observed_frame_captured_before_action_completion_cannot_be_replayed() {
    let (mut transport, network, clock) = make_transport(vec![
        response(
            "response-screenshot",
            vec![computer_call(
                "call-screenshot",
                vec![json!({"type":"screenshot"})],
                vec![],
            )],
        ),
        response(
            "response-actions",
            vec![computer_call(
                "call-actions",
                vec![json!({"type":"wait"})],
                vec![],
            )],
        ),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_cancel_tx, mut cancel) = watch::channel(false);
    runtime
        .block_on(transport.begin(fence(1, "snapshot-1", 1, 90_000), "inspect", &mut cancel))
        .unwrap();
    set_now(&clock, 10_005);
    let proposal = match runtime
        .block_on(transport.submit_initial_screenshot(
            frame_request(2, "snapshot-2", 2, 10, 10_001, 1_000_000_010),
            png_data_uri(),
            vec![],
            &mut cancel,
        ))
        .unwrap()
    {
        ComputerModelStep::ActionsProposed { proposal, .. } => proposal,
        other => panic!("expected action proposal, got {other:?}"),
    };

    // This capture has a newer snapshot and source sequence but predates the
    // host's completed-action report, so it cannot serve as that report's image.
    set_now(&clock, 10_010);
    let captured_before_outcome = frame_request(3, "snapshot-3", 3, 11, 10_006, 1_000_000_011);
    set_now(&clock, 10_020);
    transport
        .record_batch_outcome(
            &proposal,
            ComputerBatchOutcome {
                fence: proposal.fence.clone(),
                actions: vec![ComputerActionOutcome {
                    index: 0,
                    completed: true,
                }],
            },
        )
        .unwrap();
    set_now(&clock, 10_025);
    assert!(
        runtime
            .block_on(transport.submit_fresh_screenshot(
                captured_before_outcome,
                png_data_uri(),
                vec![],
                &mut cancel,
            ))
            .is_err()
    );
    assert_eq!(network.requests.lock().unwrap().len(), 2);
}

#[test]
fn stale_or_wrong_root_frame_poisons_continuation_before_provider_dispatch() {
    let (mut transport, network, clock) = make_transport(vec![
        response(
            "response-screenshot",
            vec![computer_call(
                "call-screenshot",
                vec![json!({"type":"screenshot"})],
                vec![],
            )],
        ),
        response(
            "response-actions",
            vec![computer_call(
                "call-actions",
                vec![json!({"type":"wait"})],
                vec![],
            )],
        ),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_cancel_tx, mut cancel) = watch::channel(false);
    runtime
        .block_on(transport.begin(fence(1, "snapshot-1", 1, 90_000), "wait", &mut cancel))
        .unwrap();
    set_now(&clock, 10_005);
    let frame = frame_request(2, "snapshot-2", 2, 10, 10_001, 1_000_000_010);
    let step = runtime
        .block_on(transport.submit_initial_screenshot(frame, png_data_uri(), vec![], &mut cancel))
        .unwrap();
    let proposal = match step {
        ComputerModelStep::ActionsProposed { proposal, .. } => proposal,
        other => panic!("expected action proposal, got {other:?}"),
    };
    transport
        .record_batch_outcome(
            &proposal,
            ComputerBatchOutcome {
                fence: proposal.fence.clone(),
                actions: vec![ComputerActionOutcome {
                    index: 0,
                    completed: true,
                }],
            },
        )
        .unwrap();
    set_now(&clock, 10_030);
    let mut wrong_root = frame_request(3, "snapshot-3", 3, 11, 10_021, 1_000_000_011);
    wrong_root.fence.root_task_id = RootTaskId::new("other-root").unwrap();
    assert!(
        runtime
            .block_on(transport.submit_fresh_screenshot(
                wrong_root,
                png_data_uri(),
                vec![],
                &mut cancel
            ))
            .is_err()
    );
    assert!(
        runtime
            .block_on(transport.submit_fresh_screenshot(
                frame_request(3, "snapshot-3", 3, 11, 10_021, 1_000_000_011),
                png_data_uri(),
                vec![],
                &mut cancel,
            ))
            .is_err()
    );
    assert_eq!(network.requests.lock().unwrap().len(), 2);
}

#[test]
fn pre_cancel_and_nonofficial_routes_never_dispatch() {
    let (mut transport, network, _clock) = make_transport(vec![]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (_cancel_tx, mut cancel) = watch::channel(true);
    assert!(
        runtime
            .block_on(transport.begin(fence(1, "snapshot-1", 1, 90_000), "inspect", &mut cancel))
            .is_err()
    );
    assert!(network.requests.lock().unwrap().is_empty());

    let mut bad_route = route();
    bad_route.base_url = "https://api.openai.com/v1-proxy".into();
    assert!(
        OpenAiComputerTransport::with_transport(
            bad_route,
            KEY.into(),
            "local-account-1".into(),
            Arc::new(|| 10_000),
            network.clone(),
        )
        .is_err()
    );
}

#[test]
fn truncated_and_duplicate_native_responses_never_publish_a_prefix() {
    let truncated = vec![sse(
        json!({"type":"response.created","response":{"id":"partial","status":"in_progress","model":"gpt-4.1-mini","output":[]}}),
    )];
    let duplicate = response(
        "response-duplicate",
        vec![
            computer_call("call-one", vec![json!({"type":"wait"})], vec![]),
            computer_call("call-two", vec![json!({"type":"wait"})], vec![]),
        ],
    );
    for replies in [vec![truncated], vec![duplicate]] {
        let (mut transport, network, _clock) = make_transport(replies);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_cancel_tx, mut cancel) = watch::channel(false);
        assert!(
            runtime
                .block_on(transport.begin(
                    fence(1, "snapshot-1", 1, 90_000),
                    "inspect",
                    &mut cancel
                ))
                .is_err()
        );
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
}
