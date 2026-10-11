//! Real pinned runtime + provider codecs, injected byte transport. No provider
//! account or GUI is used; these are protocol/lifecycle checks, not live evals.
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use harness_adapter::TurnResult;
use harness_adapter::model::{
    ModelPlanner, ModelProtocol, ModelProvider, ModelRoute, ModelTransport,
};
use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
use interaction_contracts::desktop::planning::{PlanningContext, PlanningReason, PlanningRequest};
use lingxi_llm_client::protocol::LlmError;
use lingxi_llm_client::{HttpRequest, StreamResponse, Transport};
use serde_json::{Value, json};

const KEY: &str = "only-explicit-test-key";
fn route(protocol: ModelProtocol) -> ModelRoute {
    ModelRoute {
        preset_id: None,
        provider_id: if protocol == ModelProtocol::AnthropicMessages {
            ModelProvider::AnthropicFirstParty
        } else {
            ModelProvider::OpenAi
        },
        protocol,
        base_url: if protocol == ModelProtocol::AnthropicMessages {
            "https://example.invalid"
        } else {
            "https://example.invalid/v1"
        }
        .into(),
        model: "model-fixture".into(),
        max_output_tokens: 4096,
    }
}
fn request() -> PlanningRequest {
    PlanningRequest {
        request_id: RequestId::new("req-model").unwrap(),
        generation: 1,
        root_task_id: RootTaskId::new("root-model").unwrap(),
        root_epoch: 1,
        task_revision: 1,
        reason: PlanningReason::Review,
        deadline_ms: 5_000,
        context: PlanningContext {
            user_text: "inspect this task".into(),
            allowed_scope: None,
            unknown_items: vec![],
            allowed_skills: vec!["ui.revisit".into()],
            recent_failures: vec![],
            notes: vec![],
        },
    }
}
fn output() -> Value {
    json!({"kind":"review","proposal":{"notes":["need a fresh observation"],"evidence":[]}})
}
fn sse(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}
fn anthropic(calls: Vec<(&str, &str, String)>, terminal: bool) -> Vec<Bytes> {
    let mut events = vec![sse(
        json!({"type":"message_start","message":{"id":"msg-1","model":"model-fixture","role":"assistant","content":[],"usage":{"input_tokens":12,"output_tokens":0}}}),
    )];
    for (block, (id, name, args)) in calls.into_iter().enumerate() {
        events.push(sse(json!({"type":"content_block_start","index":block,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}})));
        // Split at valid UTF8 boundaries, like separate transport callbacks.
        let split = args
            .char_indices()
            .nth(args.chars().count() / 2)
            .map_or(0, |(i, _)| i);
        for fragment in [&args[..split], &args[split..]] {
            events.push(sse(json!({"type":"content_block_delta","index":block,"delta":{"type":"input_json_delta","partial_json":fragment}})));
        }
        events.push(sse(json!({"type":"content_block_stop","index":block})));
    }
    if terminal {
        events.push(sse(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"input_tokens":12,"output_tokens":7}})));
        events.push(sse(json!({"type":"message_stop"})));
    }
    events
}
fn openai_chat() -> Vec<Bytes> {
    vec![
        sse(
            json!({"id":"chat-1","model":"model-fixture","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"tool-1","type":"function","function":{"name":"StructuredOutput","arguments":output().to_string()}}]},"finish_reason":null}]}),
        ),
        sse(
            json!({"id":"chat-1","model":"model-fixture","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ),
        sse(
            json!({"id":"chat-1","model":"model-fixture","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":7,"total_tokens":19}}),
        ),
        Bytes::from("data: [DONE]\n\n"),
    ]
}
fn responses() -> Vec<Bytes> {
    let item = json!({"id":"fc-1","type":"function_call","call_id":"tool-1","name":"StructuredOutput","arguments":output().to_string(),"status":"completed"});
    vec![
        sse(
            json!({"type":"response.created","response":{"id":"resp-1","status":"in_progress","model":"model-fixture","output":[]}}),
        ),
        sse(
            json!({"type":"response.output_item.added","output_index":0,"item":{"id":"fc-1","type":"function_call","call_id":"tool-1","name":"StructuredOutput","arguments":""}}),
        ),
        sse(
            json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"fc-1","delta":output().to_string()}),
        ),
        sse(json!({"type":"response.output_item.done","output_index":0,"item":item})),
        sse(
            json!({"type":"response.completed","response":{"id":"resp-1","status":"completed","model":"model-fixture","output":[item],"usage":{"input_tokens":12,"output_tokens":7,"total_tokens":19}}}),
        ),
    ]
}

struct Body {
    chunks: VecDeque<Bytes>,
    hold: bool,
    dropped: Arc<AtomicBool>,
}
impl Stream for Body {
    type Item = Result<Bytes, LlmError>;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(bytes) = self.chunks.pop_front() {
            Poll::Ready(Some(Ok(bytes)))
        } else if self.hold {
            Poll::Pending
        } else {
            Poll::Ready(None)
        }
    }
}
impl Drop for Body {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
struct Network {
    chunks: Vec<Bytes>,
    hold: bool,
    requests: Mutex<Vec<HttpRequest>>,
    dropped: Arc<AtomicBool>,
    refuse: bool,
}
#[async_trait]
impl Transport for Network {
    async fn send(&self, req: HttpRequest) -> Result<StreamResponse, LlmError> {
        self.requests.lock().unwrap().push(req);
        if self.refuse {
            return Err(LlmError::Authentication {
                message: format!("provider echoed {KEY}"),
            });
        }
        Ok(StreamResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: Box::pin(Body {
                chunks: self.chunks.clone().into(),
                hold: self.hold,
                dropped: self.dropped.clone(),
            }),
        })
    }
}
fn planner(
    protocol: ModelProtocol,
    chunks: Vec<Bytes>,
    hold: bool,
    refuse: bool,
) -> (ModelPlanner, Arc<Network>, Instant) {
    let network = Arc::new(Network {
        chunks,
        hold,
        requests: Mutex::new(vec![]),
        dropped: Arc::new(AtomicBool::new(false)),
        refuse,
    });
    let start = Instant::now();
    let transport = ModelTransport::with_transport(
        route(protocol),
        KEY.into(),
        Arc::new(move || start.elapsed().as_millis() as u64),
        network.clone(),
    )
    .unwrap();
    (ModelPlanner::new(transport), network, start)
}
fn finish(planner: &mut ModelPlanner, start: Instant) -> TurnResult {
    loop {
        if let Some(result) = planner.poll(start.elapsed().as_millis() as u64) {
            return result;
        }
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "model worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn three_protocols_submit_one_host_validated_proposal_and_measured_usage() {
    for (protocol, chunks) in [
        (
            ModelProtocol::AnthropicMessages,
            anthropic(
                vec![("tool-1", "StructuredOutput", output().to_string())],
                true,
            ),
        ),
        (ModelProtocol::OpenAiChat, openai_chat()),
        (ModelProtocol::OpenAiResponses, responses()),
    ] {
        let (mut p, network, start) = planner(protocol, chunks, false, false);
        p.begin(&request(), 0).unwrap();
        let result = finish(&mut p, start);
        assert!(result.is_proposal(), "{protocol:?}: {result:?}");
        assert_eq!(result.scope().root, request().root_task_id);
        assert_eq!(result.scope().generation, 1);
        assert_eq!(result.usage().input_tokens, Some(12));
        assert_eq!(result.usage().output_tokens, Some(7));
        assert_eq!(result.usage().cost_micros, None);
        assert!(network.dropped.load(Ordering::SeqCst));
        let requests = network.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let outgoing = &requests[0];
        assert!(!format!("{outgoing:?}").contains(KEY));
        let body: Value = serde_json::from_slice(&outgoing.body).unwrap();
        assert!(!body.to_string().contains(KEY));
        assert!(!body.to_string().contains("root-model"));
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert!(
            outgoing
                .headers
                .iter()
                .any(|(_, v)| v == KEY || v == &format!("Bearer {KEY}"))
        );
    }
}

#[test]
fn gemini_preset_dispatches_through_sdk_generate_content_with_api_key_auth() {
    let profile = llm_runtime::builtin_presets()
        .providers
        .into_iter()
        .find(|profile| profile.profile_name == "gemini")
        .unwrap();
    let wire = profile.wire_profile.as_ref().unwrap();
    let model = profile
        .models
        .iter()
        .find(|model| {
            model.capabilities.streaming
                && model.capabilities.tools
                && wire.models.iter().any(|source| {
                    source.request_model == model.request_model
                        && !source.hidden
                        && source.capability_support_for(
                            lingxi_llm_client::protocol::ModelCapability::Tools,
                        ) == lingxi_llm_client::protocol::CapabilitySupport::Supported
                })
        })
        .unwrap();
    let route = ModelRoute {
        preset_id: Some(profile.profile_name.clone()),
        provider_id: ModelProvider::Gemini,
        protocol: ModelProtocol::GeminiGenerateContent,
        base_url: profile.base_url,
        model: model.request_model.clone(),
        max_output_tokens: 4096,
    };
    assert!(route.validate().is_ok());
    let expected_url = format!(
        "{}/models/{}:streamGenerateContent?alt=sse",
        route.base_url.trim_end_matches('/'),
        route.model
    );
    let response_model = route.model.clone();

    let network = Arc::new(Network {
        chunks: vec![sse(json!({
            "modelVersion":response_model,
            "candidates": [{
                "index": 0,
                "content": {"role":"model","parts":[{
                    "functionCall":{"name":"StructuredOutput","args":output()}
                }]},
                "finishReason":"STOP"
            }],
            "usageMetadata": {
                "promptTokenCount":12,
                "candidatesTokenCount":7,
                "totalTokenCount":19
            }
        }))],
        hold: false,
        requests: Mutex::new(vec![]),
        dropped: Arc::new(AtomicBool::new(false)),
        refuse: false,
    });
    let start = Instant::now();
    let model_clock = start;
    let transport = ModelTransport::with_transport(
        route,
        KEY.into(),
        Arc::new(move || model_clock.elapsed().as_millis() as u64),
        network.clone(),
    )
    .unwrap();
    let mut planner = ModelPlanner::new(transport);
    planner.begin(&request(), 0).unwrap();
    let result = finish(&mut planner, start);
    assert!(result.is_proposal(), "{result:?}");
    assert_eq!(result.usage().input_tokens, Some(12));
    assert_eq!(result.usage().output_tokens, Some(7));
    assert_eq!(result.usage().cost_micros, None);
    assert!(network.dropped.load(Ordering::SeqCst));
    let requests = network.requests.lock().unwrap();
    let outgoing = &requests[0];
    assert_eq!(outgoing.url, expected_url);
    assert!(
        outgoing
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("x-goog-api-key") && value == KEY)
    );
    let body: Value = serde_json::from_slice(&outgoing.body).unwrap();
    assert!(
        body["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty())
    );
    assert!(!body.to_string().contains(KEY));
}

#[test]
fn trusted_compatible_anthropic_preset_executes_typed_goal_and_rejects_route_edits() {
    let profile = llm_runtime::builtin_presets()
        .providers
        .into_iter()
        .find(|profile| profile.profile_name == "deepseek-search")
        .unwrap();
    assert_eq!(
        profile.protocol,
        llm_runtime::ProtocolFamily::AnthropicMessages
    );
    let model = profile
        .models
        .iter()
        .find(|model| {
            model.capabilities.streaming
                && model.capabilities.tools
                && profile.wire_profile.as_ref().is_some_and(|wire| {
                    wire.models.iter().any(|source| {
                        source.request_model == model.request_model
                            && !source.hidden
                            && source.capability_support_for(
                                lingxi_llm_client::protocol::ModelCapability::Tools,
                            ) == lingxi_llm_client::protocol::CapabilitySupport::Supported
                    })
                })
        })
        .unwrap();
    let route = ModelRoute {
        preset_id: Some(profile.profile_name.clone()),
        provider_id: serde_json::from_value(serde_json::to_value(&profile.provider_id).unwrap())
            .unwrap(),
        protocol: serde_json::from_value(serde_json::to_value(profile.protocol).unwrap()).unwrap(),
        base_url: profile.base_url.clone(),
        model: model.request_model.clone(),
        max_output_tokens: 4096,
    };
    assert!(route.validate().is_ok());
    let mut generic = route.clone();
    generic.preset_id = None;
    assert!(generic.validate().is_err());
    let mut wrong_endpoint = route.clone();
    wrong_endpoint.base_url = "https://attacker.example/v1".into();
    assert!(wrong_endpoint.validate().is_err());

    let network = Arc::new(Network {
        chunks: anthropic(
            vec![("tool-1", "StructuredOutput", output().to_string())],
            true,
        ),
        hold: false,
        requests: Mutex::new(vec![]),
        dropped: Arc::new(AtomicBool::new(false)),
        refuse: false,
    });
    let start = Instant::now();
    let model_clock = start;
    let transport = ModelTransport::with_transport(
        route,
        KEY.into(),
        Arc::new(move || model_clock.elapsed().as_millis() as u64),
        network.clone(),
    )
    .unwrap();
    let mut planner = ModelPlanner::new(transport);
    planner.begin(&request(), 0).unwrap();
    let result = finish(&mut planner, start);
    assert!(result.is_proposal(), "{result:?}");
    assert_eq!(result.usage().input_tokens, Some(12));
    assert_eq!(result.usage().output_tokens, Some(7));
    let requests = network.requests.lock().unwrap();
    let outgoing = &requests[0];
    assert!(outgoing.url.ends_with("/v1/messages"));
    assert!(
        outgoing
            .headers
            .iter()
            .any(|(name, value)| name == "anthropic-version" && value == "2023-06-01")
    );
    match profile.auth {
        llm_runtime::AuthStrategy::ApiKey => assert!(
            outgoing
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("x-api-key") && value == KEY)
        ),
        llm_runtime::AuthStrategy::Bearer => {
            assert!(outgoing.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("authorization") && value == &format!("Bearer {KEY}")
            }))
        }
        _ => panic!("unsupported auth strategy in trusted preset"),
    }
}

#[test]
fn chatgpt_oauth_uses_the_trusted_profile_headers_and_nonstored_policy() {
    let route = ModelRoute {
        preset_id: None,
        provider_id: ModelProvider::OpenAiCompatible {
            name: "openai-chatgpt".into(),
        },
        protocol: ModelProtocol::OpenAiResponses,
        base_url: "https://chatgpt.com/backend-api/codex".into(),
        model: "gpt-6-luna".into(),
        max_output_tokens: 2048,
    };
    let expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let network = Arc::new(Network {
        chunks: responses(),
        hold: false,
        requests: Mutex::new(vec![]),
        dropped: Arc::new(AtomicBool::new(false)),
        refuse: false,
    });
    let start = Instant::now();
    let transport = ModelTransport::with_chatgpt_oauth_transport(
        route,
        "test-oauth-access-token".into(),
        expires_at,
        Some("account-123".into()),
        true,
        Arc::new(move || start.elapsed().as_millis() as u64),
        network.clone(),
    )
    .unwrap();
    let start = Instant::now();
    let mut planner = ModelPlanner::new(transport);
    planner.begin(&request(), 0).unwrap();
    assert!(finish(&mut planner, start).is_proposal());

    let requests = network.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let outgoing = &requests[0];
    assert!(
        outgoing
            .url
            .starts_with("https://chatgpt.com/backend-api/codex/")
    );
    let header = |name: &str| {
        outgoing
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(
        header("authorization"),
        Some("Bearer test-oauth-access-token")
    );
    assert_eq!(header("chatgpt-account-id"), Some("account-123"));
    assert_eq!(header("x-openai-fedramp"), Some("true"));
    assert_eq!(header("api-key"), None);

    let body: Value = serde_json::from_slice(&outgoing.body).unwrap();
    assert_eq!(body["model"], "gpt-6-luna");
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["store"], false);
    assert_eq!(body["parallel_tool_calls"], false);
    assert!(!body.to_string().contains("test-oauth-access-token"));
}

#[test]
fn two_valid_submissions_conflict_even_when_the_values_match() {
    let (mut p, n, start) = planner(
        ModelProtocol::AnthropicMessages,
        anthropic(
            vec![
                ("a", "StructuredOutput", output().to_string()),
                ("b", "StructuredOutput", output().to_string()),
            ],
            true,
        ),
        false,
        false,
    );
    p.begin(&request(), 0).unwrap();
    assert!(matches!(
        finish(&mut p, start),
        TurnResult::Conflict { count: 2, .. }
    ));
    assert_eq!(n.requests.lock().unwrap().len(), 1);
}
#[test]
fn unknown_tools_and_schema_invalid_values_cannot_submit() {
    for (name, args) in [
        ("Shell", output().to_string()),
        (
            "StructuredOutput",
            "{\"kind\":\"review\",\"proposal\":{\"notes\":[],\"evidence\":[],\"execute\":true}}"
                .into(),
        ),
    ] {
        let (mut p, _, start) = planner(
            ModelProtocol::AnthropicMessages,
            anthropic(vec![("a", name, args)], true),
            false,
            false,
        );
        p.begin(&request(), 0).unwrap();
        assert!(matches!(finish(&mut p, start), TurnResult::Missing { .. }));
    }
}
#[test]
fn text_is_not_a_structured_submission() {
    let mut chunks = anthropic(vec![], true);
    chunks.insert(1,sse(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":output().to_string()}})));
    chunks.insert(2, sse(json!({"type":"content_block_stop","index":0})));
    let (mut p, _, start) = planner(ModelProtocol::AnthropicMessages, chunks, false, false);
    p.begin(&request(), 0).unwrap();
    let result = finish(&mut p, start);
    assert!(matches!(result, TurnResult::Missing { .. }), "{result:?}");
}
#[test]
fn response_eof_without_terminal_is_quarantined_and_partial_usage_stays_unknown() {
    let (mut p, n, start) = planner(
        ModelProtocol::AnthropicMessages,
        anthropic(vec![("a", "StructuredOutput", output().to_string())], false),
        false,
        false,
    );
    p.begin(&request(), 0).unwrap();
    let result = finish(&mut p, start);
    assert!(matches!(result, TurnResult::Quarantined { .. }));
    assert_eq!(result.usage().input_tokens, None);
    assert_eq!(result.usage().cost_micros, None);
    assert!(n.dropped.load(Ordering::SeqCst));
}
#[test]
fn cancellation_disconnects_before_the_barrier_and_discards_partial_output() {
    let (mut p, n, start) = planner(
        ModelProtocol::AnthropicMessages,
        anthropic(vec![("a", "StructuredOutput", output().to_string())], false),
        true,
        false,
    );
    p.begin(&request(), 0).unwrap();
    while n.requests.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(2));
    }
    p.cancel(&request().request_id, 1, start.elapsed().as_millis() as u64);
    let result = finish(&mut p, start);
    assert!(matches!(result, TurnResult::Cancelled { .. }));
    assert!(n.dropped.load(Ordering::SeqCst));
    assert!(p.poll(100).is_none());
}
#[test]
fn cancelling_a_foreign_scope_does_not_cancel_this_turn() {
    let (mut p, _, start) = planner(ModelProtocol::OpenAiChat, openai_chat(), false, false);
    p.begin(&request(), 0).unwrap();
    p.cancel(&RequestId::new("other").unwrap(), 1, 1);
    assert!(finish(&mut p, start).is_proposal());
}
#[test]
fn provider_error_text_is_discarded_and_the_call_is_never_retried() {
    let (mut p, n, start) = planner(ModelProtocol::OpenAiChat, vec![], false, true);
    p.begin(&request(), 0).unwrap();
    let result = finish(&mut p, start);
    assert!(!result.is_proposal());
    assert!(!format!("{result:?}").contains(KEY));
    assert_eq!(n.requests.lock().unwrap().len(), 1);
}
#[test]
fn timeout_disconnects_the_stream_without_replaying() {
    let (mut p, n, start) = planner(
        ModelProtocol::AnthropicMessages,
        anthropic(vec![], false),
        true,
        false,
    );
    let mut r = request();
    let now = start.elapsed().as_millis() as u64;
    r.deadline_ms = now + 1000;
    p.begin(&r, now).unwrap();
    let result = finish(&mut p, start);
    assert!(!result.is_proposal());
    assert!(n.dropped.load(Ordering::SeqCst));
    assert_eq!(n.requests.lock().unwrap().len(), 1);
}
#[test]
fn revocation_closes_admission_and_cleans_up_the_existing_call() {
    let (mut p, n, start) = planner(
        ModelProtocol::AnthropicMessages,
        anthropic(vec![], false),
        true,
        false,
    );
    p.begin(&request(), 0).unwrap();
    while n.requests.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(2));
    }
    p.revoke(1);
    assert!(matches!(
        finish(&mut p, start),
        TurnResult::Cancelled { .. }
    ));
    let mut r = request();
    r.generation = 2;
    assert!(p.begin(&r, 2).is_err());
    assert!(n.dropped.load(Ordering::SeqCst));
    assert_eq!(n.requests.lock().unwrap().len(), 1);
}
#[test]
fn context_and_response_quotas_are_enforced() {
    let (mut p, n, start) = planner(
        ModelProtocol::OpenAiChat,
        vec![Bytes::from(vec![b' '; 1024 * 1024 + 1])],
        false,
        false,
    );
    let mut r = request();
    r.context.user_text = "x".repeat(64 * 1024 + 1);
    assert!(p.begin(&r, 0).is_err());
    assert!(n.requests.lock().unwrap().is_empty());
    p.begin(&request(), 0).unwrap();
    assert!(!finish(&mut p, start).is_proposal());
    assert!(n.dropped.load(Ordering::SeqCst));
}
#[test]
fn duplicate_tool_identity_in_two_blocks_is_quarantined() {
    let (mut p, _, start) = planner(
        ModelProtocol::AnthropicMessages,
        anthropic(
            vec![
                ("same", "StructuredOutput", output().to_string()),
                ("same", "StructuredOutput", output().to_string()),
            ],
            true,
        ),
        false,
        false,
    );
    p.begin(&request(), 0).unwrap();
    assert!(!finish(&mut p, start).is_proposal());
}
#[test]
fn invalid_routes_and_empty_keys_cannot_create_a_worker() {
    for endpoint in [
        "http://example.invalid",
        "https://user:key@example.invalid",
        "https://example.invalid?key=secret",
        "https://example.invalid#secret",
    ] {
        let mut r = route(ModelProtocol::OpenAiChat);
        r.base_url = endpoint.into();
        assert!(r.validate().is_err());
    }
    let n = Arc::new(Network {
        chunks: vec![],
        hold: false,
        requests: Mutex::new(vec![]),
        dropped: Arc::new(AtomicBool::new(false)),
        refuse: false,
    });
    assert!(
        ModelTransport::with_transport(
            route(ModelProtocol::OpenAiChat),
            String::new(),
            Arc::new(|| 0),
            n
        )
        .is_err()
    );
}

#[test]
fn duplicate_json_fields_do_not_get_normalized_into_a_valid_submission() {
    let args = r#"{"kind":"review","proposal":{"notes":[],"notes":["overwritten"],"evidence":[]}}"#;
    let (mut p, _, start) = planner(
        ModelProtocol::AnthropicMessages,
        anthropic(vec![("a", "StructuredOutput", args.into())], true),
        false,
        false,
    );
    p.begin(&request(), 0).unwrap();
    assert!(matches!(finish(&mut p, start), TurnResult::Missing { .. }));
}
#[test]
fn truncated_responses_are_quarantined_even_with_valid_arguments() {
    let mut chunks = anthropic(vec![("a", "StructuredOutput", output().to_string())], false);
    chunks.push(sse(json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"input_tokens":12,"output_tokens":7}})));
    chunks.push(sse(json!({"type":"message_stop"})));
    let (mut p, _, start) = planner(ModelProtocol::AnthropicMessages, chunks, false, false);
    p.begin(&request(), 0).unwrap();
    assert!(matches!(
        finish(&mut p, start),
        TurnResult::Quarantined { .. }
    ));
}
#[test]
fn an_unbounded_number_of_tools_is_refused() {
    let ids: Vec<String> = (0..17).map(|i| format!("call-{i}")).collect();
    let calls = ids
        .iter()
        .map(|id| (id.as_str(), "StructuredOutput", output().to_string()))
        .collect();
    let (mut p, n, start) = planner(
        ModelProtocol::AnthropicMessages,
        anthropic(calls, true),
        false,
        false,
    );
    p.begin(&request(), 0).unwrap();
    assert!(!finish(&mut p, start).is_proposal());
    assert!(n.dropped.load(Ordering::SeqCst));
}
struct PendingOpen(Arc<Network>);
#[async_trait]
impl Transport for PendingOpen {
    async fn send(&self, request: HttpRequest) -> Result<StreamResponse, LlmError> {
        let _guard = Body {
            chunks: VecDeque::new(),
            hold: true,
            dropped: self.0.dropped.clone(),
        };
        self.0.requests.lock().unwrap().push(request);
        std::future::pending().await
    }
}
#[test]
fn cancellation_during_connection_open_drops_the_future_before_its_barrier() {
    let network = Arc::new(Network {
        chunks: vec![],
        hold: true,
        requests: Mutex::new(vec![]),
        dropped: Arc::new(AtomicBool::new(false)),
        refuse: false,
    });
    let start = Instant::now();
    let transport = ModelTransport::with_transport(
        route(ModelProtocol::OpenAiChat),
        KEY.into(),
        Arc::new(move || start.elapsed().as_millis() as u64),
        Arc::new(PendingOpen(network.clone())),
    )
    .unwrap();
    let mut p = ModelPlanner::new(transport);
    p.begin(&request(), 0).unwrap();
    while network.requests.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(2));
    }
    p.cancel(&request().request_id, 1, 1);
    assert!(matches!(
        finish(&mut p, start),
        TurnResult::Cancelled { .. }
    ));
    assert!(network.dropped.load(Ordering::SeqCst));
}
