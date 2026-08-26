//! OpenAI→Orchestrator bridge — RUST-P11-001 MAP-01.
//!
//! Provides a bridge from the OpenAI HTTP handler interface to the
//! [`CommandOrchestrator`]'s command flow.
//!
//! ## Architecture note
//!
//! This crate cannot depend on `conduit-http` (circular dependency: http
//! depends on orchestrator for the trait seam). Therefore, this module
//! defines local type aliases that mirror `conduit_http::openai_handlers`.
//! The host wiring layer (conduit-bin) implements the actual
//! `OpenAiOrchestratorService` trait by delegating to
//! `OpenAiOrchestratorBridge::process_command`.
//!
//! ## Contract scope
//!
//! * [`OpenAiOrchestratorBridge`] — wraps [`CommandOrchestrator`]
//! * [`OpenAiRoute`] — mirrors `conduit_http::openai_handlers::OpenAiRoute`
//! * [`OpenAiHandlerOutput`] — mirrors the http crate's output enum
//! * [`OpenAiHandlerResponse`] — mirrors the http crate's response struct
//! * [`StreamEvent`] — mirrors the http crate's SSE event type

use std::sync::Arc;

use conduit_core::ConduitError;
use conduit_llm::{HttpRequest, HttpResponse};
use conduit_pipeline::CancelGuard;
use conduit_transformers::{
    AnthropicCountTokensInboundTransformer, AnthropicInboundTransformer, DoubaoVideoInbound,
    GeminiInboundTransformer, InboundTransformer, JinaInbound, OpenAiChatInbound,
    OpenAiCompletionInbound, OpenAiEmbeddingInbound, OpenAiImageGenerationInbound,
    OpenAiResponsesInbound, OpenAiSpeechInbound, OpenAiVideoInbound,
};

use crate::bridge::{build_candidate_request, resolve_candidates};
use crate::orchestrator::{
    CommandOrchestrator, OrchestratorContext, ROUTE_AFFINITY_API_FORMAT_METADATA,
    ROUTE_AFFINITY_DECISION_METADATA, ROUTE_AFFINITY_HINTS_METADATA,
    ROUTE_AFFINITY_KEY_CLASS_METADATA, ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA,
    ROUTE_AFFINITY_PROMPT_CACHE_HASH_METADATA, ROUTE_AFFINITY_PUBLIC_MODEL_METADATA,
    STICKY_CHANNEL_ID_METADATA,
};

// ===========================================================================
// Type aliases mirroring conduit_http::openai_handlers
// ===========================================================================
// NOTE: These must stay in sync with the http crate definitions.
// The host wiring layer handles conversion between these versions.

/// OpenAI route identifier — mirrors `conduit_http::openai_handlers::OpenAiRoute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiRoute {
    /// POST /v1/chat/completions
    ChatCompletions,
    /// POST /v1/responses
    Responses,
    /// POST /v1/embeddings
    Embeddings,
    /// POST /v1/audio/speech
    AudioSpeech,
    /// POST /v1/videos
    Videos,
    /// POST /v1/images/generations
    ImageGenerations,
    /// POST /v1/messages — Anthropic Messages API. Uses the Anthropic inbound
    /// transformer (Go: `anthropic.NewInboundTransformer()` in anthropic.go:50).
    AnthropicMessages,
    /// POST /v1/messages/count_tokens — Anthropic token counting.
    AnthropicCountTokens,
    /// Gemini generateContent / streamGenerateContent. Uses the Gemini inbound
    /// transformer (Go: gemini.go:50 `gemini.NewInboundTransformer()`).
    GeminiGenerateContent,
    /// POST /v1/completions — legacy text completions (Go
    /// `CompletionInboundTransformer`).
    Completions,
    /// POST /v1/responses/compact — compact responses flavour (Go
    /// `CompactInboundTransformer`; forces `RequestType::Compact`).
    ResponsesCompact,
    /// POST /v1/rerank + /jina/v1/rerank — Jina rerank inbound transformer.
    JinaRerank,
    /// POST /jina/v1/embeddings — Jina embedding inbound transformer.
    JinaEmbeddings,
    /// POST /doubao/v3/contents/generations/tasks — Doubao/Seedance video-task
    /// inbound transformer.
    DoubaoCreateTask,
}

/// Non-stream response — mirrors `conduit_http::openai_handlers::OpenAiHandlerResponse`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiHandlerResponse {
    /// HTTP status code
    pub status: u16,
    /// Content-Type header (if present)
    pub content_type: Option<String>,
    /// Response body bytes
    pub body: Vec<u8>,
}

/// Stream event — mirrors `conduit_http::openai_handlers::StreamEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEvent {
    /// SSE event name
    pub event: String,
    /// JSON-encoded payload
    pub data: String,
}

impl StreamEvent {
    /// Build a new event.
    pub fn new(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: event.into(),
            data: data.into(),
        }
    }
}

/// Handler output — mirrors `conduit_http::openai_handlers::OpenAiHandlerOutput`.
#[derive(Debug)]
pub enum OpenAiHandlerOutput {
    /// Non-stream response
    NonStream(OpenAiHandlerResponse),
    /// Streaming response (SSE frames), pre-collected (buffered path).
    Stream(Vec<Result<StreamEvent, ConduitError>>),
    /// Binary stream (audio, etc.)
    Binary {
        body: Vec<u8>,
        content_type: Option<String>,
    },
    /// RUST-P8-003 — live incremental stream: the client-facing event receiver
    /// produced by [`CommandOrchestrator::process_command_stream`]. The HTTP
    /// layer forwards each event to the client as an SSE frame as it arrives,
    /// instead of collecting the whole stream first.
    LiveStream(LiveEventStream),
}

/// Wrapper carrying the live client-facing event receiver so
/// [`OpenAiHandlerOutput`] can keep `#[derive(Debug)]` (`Receiver` is not
/// `Debug`). Carries [`conduit_llm::StreamEvent`] — the pipeline/finalizer event
/// type — NOT the bridge's `StreamEvent` (event/data) shape.
pub struct LiveEventStream(
    pub tokio::sync::mpsc::Receiver<Result<conduit_llm::StreamEvent, conduit_core::ConduitError>>,
);

impl std::fmt::Debug for LiveEventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LiveEventStream(<receiver>)")
    }
}

// ===========================================================================
// Bridge implementation
// ===========================================================================

/// Bridge wrapping [`CommandOrchestrator`] for the OpenAI handler interface.
///
/// ## Lifecycle
///
/// Constructed once at host startup (conduit-bin wiring layer) and shared
/// via `Arc` across all OpenAI handler state clones.
#[derive(Clone)]
pub struct OpenAiOrchestratorBridge {
    /// The real orchestrator that executes requests.
    orchestrator: Arc<CommandOrchestrator>,
}

impl OpenAiOrchestratorBridge {
    /// Create a new bridge wrapping the given orchestrator.
    pub fn new(orchestrator: Arc<CommandOrchestrator>) -> Self {
        Self { orchestrator }
    }

    /// Process an OpenAI request through the orchestrator.
    ///
    /// This is the internal implementation; the host's `OpenAiOrchestratorService`
    /// trait impl delegates to this method after converting its types.
    ///
    /// ## Flow
    ///
    /// 1. Select the inbound transformer for the route
    /// 2. Extract request metadata (project_id, request_id, trace/thread keys)
    /// 3. Run inbound transform + candidate selection via [`resolve_candidates`]
    /// 4. If no candidates → return a structured "no channels" error (expected
    ///    for fresh installs with no configured channels)
    /// 5. If candidates exist → delegate to the full orchestrator command flow
    ///    (select → load-balance → pipeline → persist) and map the HTTP response
    pub async fn process_command(
        &self,
        route: OpenAiRoute,
        mut request: HttpRequest,
    ) -> Result<OpenAiHandlerOutput, ConduitError> {
        // ---- 1. Select inbound transformer based on route ----
        // Held as `Arc` (not `Box`) so the same instance can be shared with the
        // live-stream path (`process_command_stream`), which needs an owned
        // `Arc` to move the transformer onto the blocking transform bridge.
        let inbound: std::sync::Arc<dyn InboundTransformer> = match route {
            OpenAiRoute::ChatCompletions => Arc::new(OpenAiChatInbound::new()),
            OpenAiRoute::Responses => Arc::new(OpenAiResponsesInbound::new()),
            OpenAiRoute::AnthropicMessages => Arc::new(AnthropicInboundTransformer::new()),
            OpenAiRoute::AnthropicCountTokens => {
                Arc::new(AnthropicCountTokensInboundTransformer::new())
            }
            OpenAiRoute::GeminiGenerateContent => Arc::new(GeminiInboundTransformer::new()),
            // Embeddings, AudioSpeech, Videos, ImageGenerations: dedicated
            // inbound transformers mirroring Go's per-endpoint transformers
            // (EmbeddingInboundTransformer, AudioInboundTransformer,
            // ImageGenerationInboundTransformer, VideoInboundTransformer).
            OpenAiRoute::Embeddings => Arc::new(OpenAiEmbeddingInbound::new()),
            OpenAiRoute::AudioSpeech => Arc::new(OpenAiSpeechInbound::new()),
            OpenAiRoute::ImageGenerations => Arc::new(OpenAiImageGenerationInbound::new()),
            OpenAiRoute::Videos => Arc::new(OpenAiVideoInbound::new()),
            OpenAiRoute::Completions => Arc::new(OpenAiCompletionInbound::new()),
            OpenAiRoute::ResponsesCompact => Arc::new(OpenAiResponsesInbound::compact()),
            OpenAiRoute::JinaRerank => Arc::new(JinaInbound::rerank()),
            OpenAiRoute::JinaEmbeddings => Arc::new(JinaInbound::embedding()),
            OpenAiRoute::DoubaoCreateTask => Arc::new(DoubaoVideoInbound::new()),
        };

        // ---- 2. Extract metadata before consuming the request ----
        let request_id = ensure_routing_request_id(&mut request);
        let project_id = request
            .metadata
            .get("project_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let trace_id = metadata_string(&request, "trace_key")
            .or_else(|| metadata_string(&request, "trace_id"));
        let thread_id = metadata_string(&request, "thread_key")
            .or_else(|| metadata_string(&request, "thread_id"));
        let sticky_channel_id = metadata_string(&request, STICKY_CHANNEL_ID_METADATA);

        // Keep a clone of the raw inbound request for the orchestrator (it
        // needs the original for outbound transform reference).
        let raw_inbound = request.clone();

        // ---- 3. Inbound transform + candidate selection ----
        let source = self.orchestrator.candidate_source();
        let (llm_request, candidates, selection_diagnostics) =
            resolve_candidates(inbound.as_ref(), request, source).await?;

        // ---- 4. No candidates → structured error ----
        if candidates.is_empty() {
            let model = llm_request.model.as_deref().unwrap_or("unknown");
            return Err(ConduitError::not_found(format!(
                "no channels configured for model \"{model}\". \
                 Configure at least one channel in the admin panel."
            )));
        }

        // ---- 5. Candidates exist → full orchestrator command flow ----
        let candidate_request = build_candidate_request(&llm_request);
        let mut ctx = OrchestratorContext::new();
        copy_route_affinity_metadata(&raw_inbound, &mut ctx);
        if let Some(sticky_channel_id) = sticky_channel_id {
            ctx.metadata
                .insert(STICKY_CHANNEL_ID_METADATA.to_string(), sticky_channel_id);
        }
        if let Ok(value) = serde_json::to_string(&selection_diagnostics) {
            ctx.metadata
                .insert("route_selection_diagnostics".to_owned(), value);
        }

        // ---- 5a. RUST-P8-003 streaming branch ----
        // A `stream:true` request takes the live incremental path: the
        // orchestrator returns a client-facing event receiver (fed by the
        // upstream forward loop + persistent-stream finalizer) that the HTTP
        // layer flushes as SSE frames as they arrive — instead of collecting
        // the whole stream first. Mirrors Go `orchestrator.go:331-335`
        // (`if result.Stream { return …ChatCompletionStream: result.EventStream }`).
        if candidate_request.stream {
            let handle = self
                .orchestrator
                .process_command_stream_with_resolved_candidates(
                    &mut ctx,
                    Arc::clone(&inbound),
                    &request_id,
                    &project_id,
                    &candidate_request,
                    raw_inbound.clone(),
                    &raw_inbound,
                    trace_id.as_deref(),
                    thread_id.as_deref(),
                    &candidates,
                )
                .await
                .map_err(|err| err.source)?;
            return Ok(OpenAiHandlerOutput::LiveStream(LiveEventStream(
                handle.client_rx,
            )));
        }

        // P-09: per-request cancel token wired into the buffered pipeline.
        // `CancelGuard` fires the token on drop — so if the client disconnects
        // and axum drops this handler future, the token is canceled, matching
        // the live path's client-disconnect handling. (The in-flight reqwest
        // call is already cancel-on-drop; the token additionally stops the
        // pipeline's between-attempt retry/billing loop, `pipeline.rs:1083`.)
        let request_cancel = conduit_pipeline::CancelToken::new();
        let mut cancel_guard = CancelGuard::new(request_cancel.clone());
        let http_response = self
            .orchestrator
            .process_command_with_resolved_candidates(
                &mut ctx,
                inbound.as_ref(),
                &request_id,
                &project_id,
                &candidate_request,
                raw_inbound.clone(),
                &raw_inbound,
                trace_id.as_deref(),
                thread_id.as_deref(),
                Some(request_cancel),
                &candidates,
            )
            .await
            .map_err(|err| err.source)?;
        // Completed normally — disarm so returning the response does not fire a
        // (harmless but misleading) cancel on the finished request.
        cancel_guard.disarm();

        // ---- 6. Map HttpResponse → OpenAiHandlerOutput ----
        Ok(map_http_response_to_output(http_response))
    }
}

/// Return the caller-supplied request id, or mint one before candidate
/// ordering when the inbound request has none. Load-balancer tie rotation uses
/// this value in the absence of a trace/thread id, so allowing an empty string
/// here makes every ordinary request compute the same routing offset and pins
/// equal-weight traffic to one channel.
fn ensure_routing_request_id(request: &mut HttpRequest) -> String {
    if let Some(request_id) = request
        .request_id
        .as_deref()
        .filter(|request_id| !request_id.is_empty())
    {
        return request_id.to_string();
    }

    let request_id = format!("req_{}", uuid::Uuid::now_v7().simple());
    request.request_id = Some(request_id.clone());
    request_id
}

fn metadata_string(request: &HttpRequest, key: &str) -> Option<String> {
    request
        .metadata
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn copy_route_affinity_metadata(request: &HttpRequest, ctx: &mut OrchestratorContext) {
    for key in [
        ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA,
        ROUTE_AFFINITY_PROMPT_CACHE_HASH_METADATA,
        ROUTE_AFFINITY_PUBLIC_MODEL_METADATA,
        ROUTE_AFFINITY_API_FORMAT_METADATA,
        ROUTE_AFFINITY_KEY_CLASS_METADATA,
        ROUTE_AFFINITY_DECISION_METADATA,
    ] {
        if let Some(value) = metadata_string(request, key) {
            ctx.metadata.insert(key.to_string(), value);
        }
    }
    if let Some(value) = request.metadata.get(ROUTE_AFFINITY_HINTS_METADATA) {
        let encoded = value
            .as_str()
            .map(str::to_string)
            .or_else(|| serde_json::to_string(value).ok());
        if let Some(encoded) = encoded {
            ctx.metadata
                .insert(ROUTE_AFFINITY_HINTS_METADATA.to_string(), encoded);
        }
    }
}

/// Convert an orchestrator [`HttpResponse`] into [`OpenAiHandlerOutput`].
///
/// This is the **response-side mapping** corresponding to Go's
/// `chat.go:84-115` where the orchestrator result is routed into either
/// the non-stream or streaming branch.
///
/// ## Mapping rules
///
/// * `response.stream.is_empty()` → [`OpenAiHandlerOutput::NonStream`]
/// * `response.stream.isNotEmpty()` → [`OpenAiHandlerOutput::Stream`]
/// * Binary payload (audio/wav, etc.) → [`OpenAiHandlerOutput::Binary`]
pub fn map_http_response_to_output(response: HttpResponse) -> OpenAiHandlerOutput {
    // Extract the body bytes for the non-stream case.
    let body = response.body.unwrap_or_default();
    let status = response.status;

    // Extract content-type from headers (if present).
    // HeaderMap is BTreeMap<String, String>, so get returns Option<&String>.
    let content_type = response.headers.get("content-type").cloned();

    if response.stream.is_empty() {
        // Non-stream response — Go `result.ChatCompletion` branch.
        OpenAiHandlerOutput::NonStream(OpenAiHandlerResponse {
            status,
            content_type,
            body,
        })
    } else {
        // Streaming response — Go `result.ChatCompletionStream` branch.
        // Convert each llm::StreamEvent → local StreamEvent.
        let stream_events: Vec<Result<StreamEvent, ConduitError>> = response
            .stream
            .into_iter()
            .map(|event| {
                let event_type = event.event_type.unwrap_or_default();
                let data = event.data.unwrap_or_default();
                Ok(StreamEvent::new(event_type, data))
            })
            .collect();

        OpenAiHandlerOutput::Stream(stream_events)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::StreamEvent as LlmStreamEvent;

    #[test]
    fn test_bridge_creation() {
        // Validates the bridge can be constructed.
        // A real test would wire a mock CommandOrchestrator.
        // TODO: Add a mock orchestrator and test the process flow
        // once the full implementation lands.
    }

    #[test]
    fn missing_request_ids_get_distinct_non_empty_routing_keys() {
        let mut first = HttpRequest::default();
        let mut second = HttpRequest::default();

        let first_id = ensure_routing_request_id(&mut first);
        let second_id = ensure_routing_request_id(&mut second);

        assert!(!first_id.is_empty());
        assert!(!second_id.is_empty());
        assert_ne!(first_id, second_id);
        assert_eq!(first.request_id.as_deref(), Some(first_id.as_str()));
        assert_eq!(second.request_id.as_deref(), Some(second_id.as_str()));
    }

    #[test]
    fn caller_request_id_remains_the_stable_routing_key() {
        let mut request = HttpRequest {
            request_id: Some("caller-request-42".to_string()),
            ..HttpRequest::default()
        };

        assert_eq!(ensure_routing_request_id(&mut request), "caller-request-42");
        assert_eq!(request.request_id.as_deref(), Some("caller-request-42"));
    }

    #[test]
    fn routing_metadata_keeps_external_keys_separate_from_db_row_ids() {
        let mut request = HttpRequest::default();
        request.metadata.insert(
            "trace_key".into(),
            serde_json::Value::from("trace-external"),
        );
        request
            .metadata
            .insert("trace_id".into(), serde_json::Value::from("17"));
        request.metadata.insert(
            "thread_key".into(),
            serde_json::Value::from("thread-external"),
        );
        request
            .metadata
            .insert("thread_id".into(), serde_json::Value::from("23"));

        assert_eq!(
            metadata_string(&request, "trace_key")
                .or_else(|| metadata_string(&request, "trace_id")),
            Some("trace-external".to_string())
        );
        assert_eq!(
            metadata_string(&request, "thread_key")
                .or_else(|| metadata_string(&request, "thread_id")),
            Some("thread-external".to_string())
        );
    }

    #[test]
    fn route_affinity_metadata_reaches_orchestrator_without_raw_identity() {
        let mut request = HttpRequest::default();
        request.metadata.insert(
            ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA.into(),
            serde_json::Value::from("a".repeat(64)),
        );
        request.metadata.insert(
            ROUTE_AFFINITY_PUBLIC_MODEL_METADATA.into(),
            serde_json::Value::from("gpt-public"),
        );
        request.metadata.insert(
            ROUTE_AFFINITY_API_FORMAT_METADATA.into(),
            serde_json::Value::from("openai/responses"),
        );
        request.metadata.insert(
            ROUTE_AFFINITY_HINTS_METADATA.into(),
            serde_json::json!([{
                "key_class": "previous_response_id",
                "channel_id": "12",
                "upstream_model_id": "gpt-upstream",
                "upstream_api_format": "openai/responses",
                "credential_identity": "sha256:credential"
            }]),
        );
        let mut ctx = OrchestratorContext::new();

        copy_route_affinity_metadata(&request, &mut ctx);

        assert_eq!(
            ctx.metadata
                .get(ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA),
            Some(&"a".repeat(64))
        );
        let hints: Vec<crate::orchestrator::RouteAffinityHint> = serde_json::from_str(
            ctx.metadata
                .get(ROUTE_AFFINITY_HINTS_METADATA)
                .map(String::as_str)
                .unwrap_or("[]"),
        )
        .unwrap_or_default();
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].channel_id, "12");
        assert!(
            ctx.metadata
                .values()
                .all(|value| !value.contains("resp_raw"))
        );
    }

    #[test]
    fn test_map_http_response_non_stream() {
        // Non-stream response should map to NonStream variant.
        let response = HttpResponse {
            status: 200,
            body: Some(b"{\"id\":\"test\"}".to_vec()),
            ..Default::default()
        };

        let output = map_http_response_to_output(response);

        match output {
            OpenAiHandlerOutput::NonStream(resp) => {
                assert_eq!(resp.status, 200);
                assert_eq!(resp.body, b"{\"id\":\"test\"}");
            }
            _ => panic!("Expected NonStream variant"),
        }
    }

    #[test]
    fn test_map_http_response_stream() {
        // Stream response should map to Stream variant.
        let llm_event = LlmStreamEvent {
            event_type: Some("chat.completion.chunk".to_string()),
            data: Some("{\"delta\":{}}".to_string()),
            ..Default::default()
        };

        let response = HttpResponse {
            status: 200,
            stream: vec![llm_event],
            ..Default::default()
        };

        let output = map_http_response_to_output(response);

        match output {
            OpenAiHandlerOutput::Stream(events) => {
                assert_eq!(events.len(), 1);
                let event = match events.into_iter().next() {
                    Some(Ok(e)) => e,
                    _ => panic!("Expected Ok event"),
                };
                assert_eq!(event.event, "chat.completion.chunk");
                assert_eq!(event.data, "{\"delta\":{}}");
            }
            _ => panic!("Expected Stream variant"),
        }
    }

    #[test]
    fn test_map_http_response_preserves_content_type() {
        // Content-Type header should be preserved.
        let mut response = HttpResponse {
            status: 200,
            body: Some(b"{}".to_vec()),
            ..Default::default()
        };

        // HeaderMap is BTreeMap<String, String>, so we insert a String value.
        response
            .headers
            .insert("content-type".to_string(), "application/json".to_string());

        let output = map_http_response_to_output(response);

        match output {
            OpenAiHandlerOutput::NonStream(resp) => {
                assert_eq!(resp.content_type.as_deref(), Some("application/json"));
            }
            _ => panic!("Expected NonStream variant"),
        }
    }

    #[test]
    fn test_openai_route_variants() {
        // Verify all route variants can be constructed.
        let routes = [
            OpenAiRoute::ChatCompletions,
            OpenAiRoute::Responses,
            OpenAiRoute::Embeddings,
            OpenAiRoute::AudioSpeech,
            OpenAiRoute::Videos,
            OpenAiRoute::ImageGenerations,
        ];

        for route in routes {
            // Just verify they can be created and compared.
            let _ = route;
        }
    }

    #[test]
    fn test_stream_event_new() {
        let event = StreamEvent::new("chat.completion.chunk", "{\"delta\":{}}");
        assert_eq!(event.event, "chat.completion.chunk");
        assert_eq!(event.data, "{\"delta\":{}}");
    }
}
