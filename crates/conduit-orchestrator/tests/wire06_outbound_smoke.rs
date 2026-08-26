//! WIRE-06 subtask 3 — end-to-end outbound smoke over a real socket.
//!
//! Proves the credential/base_url wiring works through the *whole* chain:
//! `CommandOrchestrator::process_command` → LB → pipeline (per-attempt
//! url/auth/model stamping from the resolved candidate) → the production
//! `UpstreamExecutor` (real reqwest) → a fake upstream that records the raw
//! HTTP request it received.
//!
//! Go parity target: `PersistentOutboundTransformer` (outbound.go:315-385) —
//! the upstream must receive the channel's `ActualModel` in the body and the
//! channel credential as `Authorization: Bearer <key>` on the default
//! `/v1/chat/completions` endpoint built from the channel `BaseURL`.
//!
//! Scope (per WIRE-06 plan §3): single api-key channel, single model,
//! chat-completions endpoint only. OAuth channels / model rotation / endpoint
//! overrides are recorded follow-ups.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conduit_core::ConduitError;
use conduit_llm::{
    ApiFormat, ChatRequest, HttpRequest, HttpResponse, LlmRequest, LlmRequestPayload, RequestType,
    StreamEvent,
};
use conduit_orchestrator::candidates::{CandidateRequest, ChannelModelsCandidate};
use conduit_orchestrator::load_balancer::{
    RetryPolicy as LbRetryPolicy, StaticStickyKeyProvider, WeightScoring,
};
use conduit_orchestrator::orchestrator::{
    CandidateSource, CommandOrchestrator, DefaultCandidateProjector, FlagCancelToken,
    NoopRequestRecorder, OrchestratorContext,
};
use conduit_orchestrator::upstream_executor::UpstreamExecutor;
use conduit_pipeline::pipeline::{Pipeline, RetryHooks, RetryPolicy as PipeRetryPolicy};
use conduit_services::channel_service::{ChannelModelEntry, ModelSource};
use conduit_transformers::{InboundTransformer, OutboundTransformer, TransformerResult};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Recording fake upstream — extends the `one_shot_server` drain-only pattern
// (upstream_executor.rs tests) to capture the raw request bytes so the test
// can assert on the request line, headers, and body.
// ---------------------------------------------------------------------------

/// True once `buf` holds a complete HTTP/1.1 request: full header block plus
/// `Content-Length` bytes of body (0 when the header is absent).
fn request_complete(buf: &[u8]) -> bool {
    let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    buf.len() >= header_end + 4 + content_length
}

/// One-shot HTTP/1.1 server on an ephemeral port that RECORDS the request it
/// receives (request line + headers + body) before answering with the canned
/// response bytes. Returns the base URL (scheme://host:port, no path — the
/// pipeline is expected to append `/v1/chat/completions`) and the capture
/// buffer.
fn recording_one_shot_server(
    response: Vec<u8>,
) -> Result<(String, Arc<Mutex<Vec<u8>>>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let base_url = format!("http://{addr}");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    std::thread::spawn(move || {
        // `accept` failing means the client never connected — the test's
        // response assertions will fail with an empty capture; don't panic.
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        while !request_complete(&buf) {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        if let Ok(mut guard) = sink.lock() {
            *guard = buf;
        }
        let _ = stream.write_all(&response);
        let _ = stream.flush();
    });
    Ok((base_url, captured))
}

/// Split a captured raw request into (request line, lowercase-name headers,
/// body bytes) for assertions.
fn parse_captured(raw: &[u8]) -> Option<(String, Vec<(String, String)>, Vec<u8>)> {
    let header_end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next()?.to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    Some((request_line, headers, raw[header_end + 4..].to_vec()))
}

// ---------------------------------------------------------------------------
// Fakes — minimal inbound/outbound mirroring the production openai pair's
// contract: the outbound emits path + body only (no url, no auth), exactly
// like `OpenAiCompatOutbound` in wiring.rs, so the pipeline's WIRE-06 stamp
// is what puts the target URL + Bearer credential on the wire.
// ---------------------------------------------------------------------------

struct SmokeInbound;

impl InboundTransformer for SmokeInbound {
    fn name(&self) -> &'static str {
        "smoke-openai-inbound"
    }

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        let body = request.json_body.unwrap_or(Value::Null);
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ConduitError::invalid_request("model is required"))?;
        Ok(LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some(model),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        })
    }

    fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        Ok(response)
    }

    fn inbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        Ok(event)
    }

    fn inbound_error(&self, _error: &ConduitError) -> TransformerResult<HttpResponse> {
        Ok(HttpResponse::default())
    }
}

struct SmokeOutbound;

impl OutboundTransformer for SmokeOutbound {
    fn name(&self) -> &'static str {
        "smoke-openai-outbound"
    }

    fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
        // Path + body only — url/auth deliberately unset (the WIRE-06 stamp in
        // `process_attempt` must fill them from the candidate target).
        Ok(HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            request_type: Some(request.request_type),
            api_format: Some(request.api_format),
            json_body: Some(json!({
                "model": request.model.clone().unwrap_or_default(),
                "stream": request.stream,
            })),
            ..HttpRequest::default()
        })
    }

    fn outbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        Ok(response)
    }

    fn outbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        Ok(event)
    }

    fn outbound_error(&self, response: HttpResponse) -> TransformerResult<ConduitError> {
        Ok(ConduitError::upstream("upstream error").with_provider_status(response.status))
    }
}

/// Static candidate source returning one fully-resolved candidate (base_url +
/// active_credential populated, as `DbCandidateSource::build_snapshot` does in
/// production).
struct StaticSource {
    candidates: Vec<ChannelModelsCandidate>,
}

#[async_trait]
impl CandidateSource for StaticSource {
    async fn select(
        &self,
        _request: &CandidateRequest,
    ) -> Result<Vec<ChannelModelsCandidate>, ConduitError> {
        Ok(self.candidates.clone())
    }
}

fn no_retry_policy() -> PipeRetryPolicy {
    PipeRetryPolicy {
        enabled: false,
        max_channel_retries: 0,
        max_single_channel_retries: 0,
        retry_delay_ms: 0,
        stream_first_event_timeout_ms: 0,
        non_stream_timeout_ms: 0,
        empty_response_detection: false,
    }
}

// ---------------------------------------------------------------------------
// The smoke test.
// ---------------------------------------------------------------------------

/// Client asks for `gpt-4`; the channel maps it to actual model
/// `gpt-4-upstream`, has `base_url` pointing at the fake upstream and an
/// active credential `sk-test`. The fake upstream must observe:
/// `POST /v1/chat/completions`, `Authorization: Bearer sk-test`, and
/// `body.model == "gpt-4-upstream"` (Go outbound.go:385
/// `llmRequest.Model = entry.ActualModel` + transformer Config{BaseURL,
/// APIKeyProvider}).
#[tokio::test]
async fn wire06_e2e_real_socket_bearer_auth_and_actual_model()
-> Result<(), Box<dyn std::error::Error>> {
    // Canned 200 chat-completion reply.
    let reply = json!({
        "id": "chatcmpl-wire06",
        "object": "chat.completion",
        "model": "gpt-4-upstream",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "pong"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
    .to_string();
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        reply.len()
    )
    .into_bytes();
    response.extend_from_slice(reply.as_bytes());

    let (base_url, captured) = recording_one_shot_server(response)?;

    // The resolved candidate a DbCandidateSource would produce for an api-key
    // channel: request model gpt-4 → actual model gpt-4-upstream.
    let candidate = ChannelModelsCandidate {
        channel_id: "42".to_string(),
        channel_name: "wire06-smoke".to_string(),
        ordering_weight: 0,
        priority: 0,
        models: vec![ChannelModelEntry {
            request_model: "gpt-4".to_string(),
            actual_model: "gpt-4-upstream".to_string(),
            source: ModelSource::Direct,
        }],
        endpoint: conduit_core::objects::channel_settings::ChannelEndpoint {
            api_format: "openai/chat_completions".to_string(),
            ..Default::default()
        },
        api_format: "openai/chat_completions".to_string(),
        channel_type: "openai".to_string(),
        policies: Default::default(),
        credential_key_identity: String::new(),
        tags: Vec::new(),
        base_url: Some(base_url),
        active_credential: Some("sk-test".to_string()),
        enabled_credentials: vec!["sk-test".to_string()],
        settings: None,
        theoretical_cost_accounting: None,
        cost_efficiency_score: 0,
    };

    // Full production-shaped chain: static source → default projector → weight
    // LB → pipeline(smoke transformers + REAL reqwest executor) → noop recorder.
    let pipeline = Arc::new(
        Pipeline::new(
            Arc::new(SmokeInbound),
            Arc::new(SmokeOutbound),
            UpstreamExecutor::new(reqwest::Client::builder().build()?).into_arc(),
        )
        .with_retry_policy(no_retry_policy())
        .with_retry_hooks(RetryHooks::default()),
    );
    let orchestrator = CommandOrchestrator::new(
        Arc::new(StaticSource {
            candidates: vec![candidate],
        }),
        Arc::new(DefaultCandidateProjector),
        Arc::new(WeightScoring::new()),
        Arc::new(StaticStickyKeyProvider::none()),
        LbRetryPolicy::DEFAULT,
        pipeline,
        Arc::new(NoopRequestRecorder),
        Arc::new(FlagCancelToken::new()),
    );

    // Client-facing request (the client speaks the request model, gpt-4).
    let inbound = HttpRequest {
        method: "POST".to_string(),
        path: "/v1/chat/completions".to_string(),
        json_body: Some(json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "ping"}],
        })),
        ..HttpRequest::default()
    };
    let candidate_request =
        CandidateRequest::new("gpt-4", RequestType::Chat, "openai/chat_completions");

    let mut ctx = OrchestratorContext::new();
    let out = orchestrator
        .process_command(
            &mut ctx,
            "1",
            "1",
            &candidate_request,
            inbound.clone(),
            &inbound,
            None,
            None,
        )
        .await?;

    // ---- Response round-tripped from the fake upstream ----
    assert_eq!(out.status, 200);
    assert_eq!(
        out.json_body
            .as_ref()
            .and_then(|b| b.get("id"))
            .and_then(Value::as_str),
        Some("chatcmpl-wire06")
    );

    // ---- What the upstream actually received (the WIRE-06 contract) ----
    let raw = captured
        .lock()
        .map(|g| g.clone())
        .map_err(|_| "capture lock poisoned")?;
    let (request_line, headers, body) =
        parse_captured(&raw).ok_or("fake upstream captured no complete request")?;

    // 1. URL: channel base_url + default chat-completions path (Go
    //    buildFullRequestURL over Config.BaseURL).
    assert!(
        request_line.starts_with("POST /v1/chat/completions "),
        "unexpected request line: {request_line}"
    );

    // 2. Auth: the channel's active credential as a Bearer header (Go
    //    FinalizeAuthHeaders over Config.APIKeyProvider).
    let auth = headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .map(|(_, value)| value.as_str());
    assert_eq!(auth, Some("Bearer sk-test"));

    // 3. Body model: the channel's actual model, not the client's request
    //    model (Go outbound.go:385).
    let body_json: Value = serde_json::from_slice(&body)?;
    assert_eq!(
        body_json.get("model").and_then(Value::as_str),
        Some("gpt-4-upstream")
    );

    // 4. Hygiene: the plaintext credential never leaks into the pipeline's
    //    observable order/metadata.
    let steps = ctx
        .metadata
        .get("pipeline_steps")
        .cloned()
        .unwrap_or_default();
    assert!(
        !steps.contains("sk-test"),
        "credential must not leak into pipeline steps"
    );
    Ok(())
}
