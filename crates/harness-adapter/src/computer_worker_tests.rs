use super::*;
use crate::model::{ModelProtocol, ModelProvider, ModelRoute};
use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
use lingxi_llm_client::protocol::LlmError;
use lingxi_llm_client::{HttpRequest, StreamResponse, Transport};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

struct Network {
    hold: bool,
    calls: AtomicUsize,
    dropped: Arc<AtomicUsize>,
}
#[async_trait]
impl Transport for Network {
    async fn send(&self, _: HttpRequest) -> Result<StreamResponse, LlmError> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let hold = self.hold && call_number == 1;
        let mut chunks = VecDeque::from([Bytes::from(format!(
            "data: {}\n\n",
            json!({"type":"response.created","response":{"id":"resp_worker_cancel","model":"worker-test-model","status":"in_progress","output":[]}})
        ))]);
        if !hold {
            chunks.push_back(Bytes::from(format!("data: {}\n\n", json!({"type":"response.completed","response":{"id":"resp_worker_cancel","model":"worker-test-model","status":"completed","output":[{"id":"item_worker_cancel","type":"computer_call","call_id":"call_worker_cancel","status":"completed","actions":[{"type":"screenshot"}],"pending_safety_checks":[]}],"usage":{"input_tokens":21,"output_tokens":8,"total_tokens":29}}}))));
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

fn fence() -> ComputerFence {
    ComputerFence {
        request_id: RequestId::new("worker-cancel-1").unwrap(),
        generation: 1,
        root_task_id: RootTaskId::new("root-worker-cancel").unwrap(),
        task_revision: 2,
        root_epoch: 3,
        snapshot_id: "snapshot-worker-cancel".into(),
        structure_sequence: 4,
        deadline_ms: 20_000,
    }
}

fn worker(hold: bool) -> (ComputerWorker, Arc<Network>) {
    let network = Arc::new(Network {
        hold,
        calls: AtomicUsize::new(0),
        dropped: Arc::new(AtomicUsize::new(0)),
    });
    let route = ModelRoute {
        preset_id: None,
        provider_id: ModelProvider::OpenAi,
        protocol: ModelProtocol::OpenAiResponses,
        base_url: "https://api.openai.com/v1".into(),
        model: "worker-test-model".into(),
        max_output_tokens: 4096,
    };
    let transport = OpenAiComputerTransport::with_transport(
        route,
        "worker-test-key".into(),
        "worker-test-account".into(),
        Arc::new(|| 40),
        network.clone(),
    )
    .unwrap();
    (ComputerWorker::new(transport).unwrap(), network)
}

fn wait(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "worker condition timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn poll(worker: &mut ComputerWorker) -> ComputerWorkerResult {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(result) = worker.poll() {
            return result;
        }
        assert!(Instant::now() < deadline, "worker result timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn scoped_cancel_drops_the_stream_before_delivering_one_unknown_usage_result() {
    let (mut worker, network) = worker(true);
    let fence = fence();
    worker
        .start_begin(fence.clone(), "Inspect this window".into())
        .unwrap();
    wait(|| network.calls.load(Ordering::SeqCst) == 1);
    let mut foreign = fence.clone();
    foreign.generation += 1;
    worker.cancel(&foreign);
    assert!(worker.poll().is_none());
    worker.cancel(&fence);
    let result = poll(&mut worker);
    assert!(result.result.is_err());
    assert_eq!(result.status, ComputerWorkerStatus::Cancelled);
    assert_eq!(result.usage, Usage::default());
    assert_eq!(network.dropped.load(Ordering::SeqCst), 1);
    assert!(worker.poll().is_none());
}

#[test]
fn cancelling_before_job_start_cannot_reuse_the_previous_request_usage() {
    let (mut worker, network) = worker(false);
    worker
        .start_begin(fence(), "Inspect this window".into())
        .unwrap();
    let first = poll(&mut worker);
    assert!(first.result.is_ok());
    assert_eq!(first.usage.input_tokens, Some(21));
    assert_eq!(first.usage.cost_micros, None);
    let (started, ready) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    worker.runtime.as_ref().unwrap().spawn(async move {
        started.send(()).unwrap();
        // Deterministically hold only this test runtime's worker while the
        // next job is queued and cancelled, before its entry reset executes.
        blocked.recv_timeout(Duration::from_secs(2)).unwrap();
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut request: ComputerFrameRequest = serde_json::from_str(include_str!(
        "../../../contracts/computer-v1/image-channel/request.json"
    ))
    .unwrap();
    request.fence = fence();
    request.fence.generation = 2;
    let image = ImageFrame {
        meta: request.expected_frame.clone(),
        bytes: Arc::from(
            include_bytes!("../../../contracts/desktop-v1/image-channel/fixture.png").as_slice(),
        ),
    };
    worker
        .start_screenshot(
            ComputerScreenshotPhase::Initial,
            request.clone(),
            image,
            vec![],
        )
        .unwrap();
    worker.cancel(&request.fence);
    release.send(()).unwrap();
    let result = poll(&mut worker);
    assert!(result.result.is_err());
    assert_eq!(result.status, ComputerWorkerStatus::Cancelled);
    assert_eq!(result.usage, Usage::default());
    assert_eq!(network.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn revoke_rejects_new_jobs_and_drops_a_live_response_without_host_join() {
    let (mut worker, network) = worker(true);
    worker
        .start_begin(fence(), "Inspect this window".into())
        .unwrap();
    wait(|| network.calls.load(Ordering::SeqCst) == 1);
    let before = Instant::now();
    worker.revoke();
    assert!(before.elapsed() < Duration::from_millis(150));
    assert!(worker.start_begin(fence(), "Another task".into()).is_err());
    let result = poll(&mut worker);
    assert!(result.result.is_err());
    assert_eq!(result.status, ComputerWorkerStatus::Revoked);
    assert_eq!(network.dropped.load(Ordering::SeqCst), 1);
    assert!(worker.reset_ledger().is_err());
}

#[test]
fn finishing_a_running_scope_waits_for_the_stream_drop_barrier_before_reset() {
    let (mut worker, network) = worker(true);
    let old_fence = fence();
    worker
        .start_begin(old_fence.clone(), "Inspect this window".into())
        .unwrap();
    wait(|| network.calls.load(Ordering::SeqCst) == 1);

    assert_eq!(
        worker.finish_scope(&old_fence).unwrap(),
        ScopeFinish::Closing
    );
    assert!(worker.reset_ledger().is_err());
    let result = poll(&mut worker);
    assert_eq!(result.status, ComputerWorkerStatus::Cancelled);
    assert_eq!(network.dropped.load(Ordering::SeqCst), 1);
    assert!(worker.reset_ledger().is_ok());
}

#[test]
fn a_foreign_scope_fence_cannot_close_or_reset_the_active_ledger() {
    let (mut worker, _network) = worker(false);
    let active = fence();
    worker
        .start_begin(active.clone(), "Inspect this window".into())
        .unwrap();
    assert_eq!(poll(&mut worker).status, ComputerWorkerStatus::Completed);

    let mut foreign = active.clone();
    foreign.root_epoch += 1;
    assert!(worker.finish_scope(&foreign).is_err());
    assert!(worker.reset_ledger().is_err());
    assert_eq!(worker.finish_scope(&active).unwrap(), ScopeFinish::Closed);
    assert!(worker.reset_ledger().is_ok());
}

#[test]
fn scoped_cancel_can_reset_for_a_new_root_without_revoking_the_purpose_key() {
    let (mut worker, network) = worker(true);
    let old_fence = fence();
    worker
        .start_begin(old_fence.clone(), "old root".into())
        .unwrap();
    wait(|| network.calls.load(Ordering::SeqCst) == 1);
    assert_eq!(
        worker.finish_scope(&old_fence).unwrap(),
        ScopeFinish::Closing
    );
    assert_eq!(poll(&mut worker).status, ComputerWorkerStatus::Cancelled);
    worker.reset_ledger().unwrap();

    let mut new_fence = fence();
    new_fence.root_task_id = RootTaskId::new("root-worker-next").unwrap();
    new_fence.request_id = RequestId::new("worker-next-request").unwrap();
    worker.start_begin(new_fence, "new root".into()).unwrap();
    assert_eq!(poll(&mut worker).status, ComputerWorkerStatus::Completed);
    assert_eq!(network.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn admission_errors_are_typed_for_invalid_and_expired_requests() {
    let (mut worker, network) = worker(false);
    assert_eq!(
        worker.start_begin(fence(), "  ".into()).unwrap_err().kind,
        ComputerWorkerStartErrorKind::Invalid
    );
    let mut expired = fence();
    expired.deadline_ms = 40;
    assert_eq!(
        worker
            .start_begin(expired, "valid goal".into())
            .unwrap_err()
            .kind,
        ComputerWorkerStartErrorKind::Expired
    );
    assert_eq!(network.calls.load(Ordering::SeqCst), 0);
}
