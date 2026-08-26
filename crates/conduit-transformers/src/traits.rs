use std::collections::HashMap;
use std::sync::Arc;

use conduit_core::{ConduitError, ErrorKind};
use conduit_llm::{ApiFormat, HttpRequest, HttpResponse, LlmRequest, LlmResponse, StreamEvent};

pub type TransformerResult<T> = Result<T, ConduitError>;

pub trait InboundTransformer: Send + Sync {
    fn name(&self) -> &'static str;

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest>;

    fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse>;

    fn inbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent>;

    fn inbound_error(&self, error: &ConduitError) -> TransformerResult<HttpResponse>;

    /// Go parity: `TransformResponse(ctx, *llm.Response) (*httpclient.Response, error)`
    /// (`conduit/llm/transformer/interfaces.go`) — unified `LlmResponse` →
    /// client-facing HTTP response.
    ///
    /// Default implementation (RESP-CHAIN-SPEC decision a): serialize the
    /// `LlmResponse` to a JSON body and wrap it in a 200 response with
    /// `Content-Type: application/json`. This is correct for every OpenAI-shaped
    /// inbound transformer (the majority — the body is already the OpenAI chat
    /// completion shape the client expects). Non-OpenAI inbound transformers
    /// (e.g. Anthropic, Gemini) override this to reshape into their native
    /// client-facing envelope.
    fn transform_response(&self, response: LlmResponse) -> TransformerResult<HttpResponse> {
        let body = serde_json::to_vec(&response).map_err(|err| {
            ConduitError::new(
                ErrorKind::Internal,
                "failed to serialize unified LlmResponse",
            )
            .with_source(err)
        })?;
        let mut headers = conduit_llm::model::HeaderMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        Ok(HttpResponse {
            status: 200,
            headers,
            body: Some(body),
            ..HttpResponse::default()
        })
    }

    /// Aggregate provider streaming chunks into a single non-streaming HTTP
    /// response, mirroring Go `Inbound.AggregateStreamChunks`
    /// (`conduit/llm/transformer/interfaces.go:30`).
    ///
    /// Go's `autoAggregateStream` (`conduit/llm/pipeline/non_streaming.go:110`)
    /// calls `p.Inbound.AggregateStreamChunks(ctx, chunks)` and wraps the
    /// returned `[]byte` body into an `httpclient.Response` with
    /// `StatusCode: 200` + `Content-Type: application/json`. The Rust trait
    /// returns the fully-formed [`HttpResponse`] directly so the pipeline can
    /// forward it without re-wrapping.
    ///
    /// The default implementation preserves the events losslessly on the
    /// `stream` field (mirroring the [`OutboundTransformer::aggregate_stream`]
    /// default) and sets no body — providers that lack a dedicated inbound
    /// aggregator will not produce a synthetic chat-completion payload, but
    /// they will not lose data either. Concrete inbound transformers are
    /// expected to override this.
    fn aggregate_stream_chunks(&self, events: Vec<StreamEvent>) -> TransformerResult<HttpResponse> {
        let count = events.len();
        Ok(HttpResponse {
            status: 200,
            stream: events,
            // Mark that the default (non-overridden) path was taken so callers
            // can detect missing impls; mirrors Go's `ErrEmptyAggregatedBody`
            // spirit without hard-failing (keeps existing inbound impls working).
            json_body: Some(serde_json::json!({
                "aggregated_by": "default",
                "event_count": count,
            })),
            ..HttpResponse::default()
        })
    }

    /// Go parity: `TransformStream(ctx, Stream[*llm.Response]) -> (Stream[*StreamEvent], error)`
    /// — wrap the unified LlmResponse stream, mapping each into a client `StreamEvent`.
    ///
    /// Default: serialize each `LlmResponse` to JSON SSE data (correct for
    /// OpenAI clients). Non-OpenAI inbound transformers override to produce
    /// their native SSE event sequence (Anthropic message_start etc.).
    fn transform_stream(
        &self,
        events: Box<dyn Iterator<Item = LlmResponse> + Send>,
    ) -> TransformerResult<Box<dyn Iterator<Item = StreamEvent> + Send>> {
        Ok(Box::new(events.filter_map(|resp| {
            let data = serde_json::to_string(&resp).ok()?;
            Some(StreamEvent {
                event_type: None,
                data: Some(data),
                ..StreamEvent::default()
            })
        })))
    }
}

pub trait OutboundTransformer: Send + Sync {
    fn name(&self) -> &'static str;

    fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest>;

    fn outbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse>;

    fn outbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent>;

    fn outbound_error(&self, response: HttpResponse) -> TransformerResult<ConduitError>;

    /// Go parity: `TransformResponse(ctx, *httpclient.Response) (*llm.Response, error)`
    /// (`conduit/llm/transformer/interfaces.go`) — provider HTTP response →
    /// unified `LlmResponse`.
    ///
    /// Default implementation (RESP-CHAIN-SPEC decision a): parse the response
    /// body as JSON directly into `LlmResponse` (serde). This is correct for
    /// every OpenAI-compatible provider (the payload is already the OpenAI chat
    /// completion shape). Non-OpenAI providers (Anthropic, Gemini, ...) override
    /// this to convert their native response envelope into the unified shape.
    ///
    /// Error mapping: a missing body or a serde parse failure is surfaced as
    /// `ErrorKind::InvalidResponse` (the crate's existing kind for malformed
    /// provider payloads — no new error variant is invented).
    fn transform_response(&self, response: HttpResponse) -> TransformerResult<LlmResponse> {
        // Prefer `json_body` when the HTTP layer already parsed it; fall back to
        // deserializing `body` bytes. If neither is present the payload is empty
        // and the response is not a valid LLM completion.
        let json_value = if let Some(value) = response.json_body.as_ref() {
            value.clone()
        } else if let Some(bytes) = response.body.as_ref() {
            if bytes.is_empty() {
                return Err(ConduitError::new(
                    ErrorKind::InvalidResponse,
                    "provider response body is empty",
                ));
            }
            serde_json::from_slice::<serde_json::Value>(bytes).map_err(|err| {
                ConduitError::new(
                    ErrorKind::InvalidResponse,
                    "failed to parse provider response body as JSON",
                )
                .with_source(err)
            })?
        } else {
            return Err(ConduitError::new(
                ErrorKind::InvalidResponse,
                "provider response has no body",
            ));
        };
        serde_json::from_value::<LlmResponse>(json_value).map_err(|err| {
            ConduitError::new(
                ErrorKind::InvalidResponse,
                "failed to deserialize provider response into LlmResponse",
            )
            .with_source(err)
        })
    }

    fn aggregate_stream(&self, events: Vec<StreamEvent>) -> TransformerResult<HttpResponse> {
        // Keep a lossless event list so retry/aggregation code can decide how to fold chunks.
        Ok(HttpResponse {
            status: 200,
            stream: events,
            ..HttpResponse::default()
        })
    }

    /// Go parity: `TransformStream(ctx, req, Stream[*StreamEvent]) -> (Stream[*llm.Response], error)`
    /// — wrap the provider event stream, mapping each raw `StreamEvent` into a
    /// unified `LlmResponse`.
    ///
    /// Default: parse each event's `data` field as JSON `LlmResponse` (correct
    /// for OpenAI-compatible providers where each SSE chunk is a complete
    /// chat.completion.chunk). Non-OpenAI providers override to return a
    /// stateful stream wrapper. Lazy — no events consumed until the pipeline pulls.
    fn transform_stream(
        &self,
        events: Box<dyn Iterator<Item = StreamEvent> + Send>,
    ) -> TransformerResult<Box<dyn Iterator<Item = LlmResponse> + Send>> {
        Ok(Box::new(events.filter_map(|event| {
            let data = event.data.as_ref()?;
            serde_json::from_str::<LlmResponse>(data).ok()
        })))
    }
}

pub trait VideoTaskOutbound: Send + Sync {
    fn name(&self) -> &'static str;

    fn get_video_task_request(&self, task_id: &str) -> TransformerResult<HttpRequest>;

    fn delete_video_task_request(&self, task_id: &str) -> TransformerResult<HttpRequest>;

    fn video_task_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransformerKey {
    pub channel_type: String,
    pub api_format: ApiFormat,
}

impl TransformerKey {
    pub fn new(channel_type: impl Into<String>, api_format: ApiFormat) -> Self {
        Self {
            // Channel type values usually come from config/database rows; normalize lookup keys.
            channel_type: channel_type.into().to_ascii_lowercase(),
            api_format,
        }
    }
}

#[derive(Default)]
pub struct TransformerRegistry {
    outbound: HashMap<TransformerKey, Arc<dyn OutboundTransformer>>,
    video_task_outbound: HashMap<TransformerKey, Arc<dyn VideoTaskOutbound>>,
}

impl TransformerRegistry {
    const ANY_CHANNEL_TYPE: &'static str = "*";

    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_outbound(
        &mut self,
        channel_type: impl Into<String>,
        api_format: ApiFormat,
        transformer: Arc<dyn OutboundTransformer>,
    ) -> Option<Arc<dyn OutboundTransformer>> {
        self.outbound
            .insert(TransformerKey::new(channel_type, api_format), transformer)
    }

    /// Register a transformer for every channel that targets `api_format`.
    ///
    /// Protocol conversion is selected by the candidate's target wire format;
    /// channel-specific registrations remain available as higher-priority
    /// overrides for provider quirks.
    pub fn register_outbound_for_format(
        &mut self,
        api_format: ApiFormat,
        transformer: Arc<dyn OutboundTransformer>,
    ) -> Option<Arc<dyn OutboundTransformer>> {
        self.register_outbound(Self::ANY_CHANNEL_TYPE, api_format, transformer)
    }

    pub fn register_outbound_box(
        &mut self,
        channel_type: impl Into<String>,
        api_format: ApiFormat,
        transformer: Box<dyn OutboundTransformer>,
    ) -> Option<Arc<dyn OutboundTransformer>> {
        self.register_outbound(channel_type, api_format, Arc::from(transformer))
    }

    pub fn outbound(
        &self,
        channel_type: &str,
        api_format: ApiFormat,
    ) -> Option<Arc<dyn OutboundTransformer>> {
        self.outbound
            .get(&TransformerKey::new(channel_type, api_format))
            .or_else(|| {
                self.outbound
                    .get(&TransformerKey::new(Self::ANY_CHANNEL_TYPE, api_format))
            })
            .cloned()
    }

    pub fn register_video_task_outbound(
        &mut self,
        channel_type: impl Into<String>,
        api_format: ApiFormat,
        transformer: Arc<dyn VideoTaskOutbound>,
    ) -> Option<Arc<dyn VideoTaskOutbound>> {
        self.video_task_outbound
            .insert(TransformerKey::new(channel_type, api_format), transformer)
    }

    pub fn register_video_task_outbound_box(
        &mut self,
        channel_type: impl Into<String>,
        api_format: ApiFormat,
        transformer: Box<dyn VideoTaskOutbound>,
    ) -> Option<Arc<dyn VideoTaskOutbound>> {
        self.register_video_task_outbound(channel_type, api_format, Arc::from(transformer))
    }

    pub fn video_task_outbound(
        &self,
        channel_type: &str,
        api_format: ApiFormat,
    ) -> Option<Arc<dyn VideoTaskOutbound>> {
        self.video_task_outbound
            .get(&TransformerKey::new(channel_type, api_format))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use conduit_core::ErrorKind;
    use conduit_llm::{ChatRequest, Choice, LlmRequestPayload, RequestType};
    use serde_json::json;

    use super::*;

    struct DummyOutbound;

    impl OutboundTransformer for DummyOutbound {
        fn name(&self) -> &'static str {
            "dummy"
        }

        fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
            Ok(HttpRequest {
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                request_type: Some(request.request_type),
                api_format: Some(request.api_format),
                json_body: Some(json!({
                    "model": request.model,
                    "stream": request.stream,
                })),
                ..HttpRequest::default()
            })
        }

        fn outbound_response(&self, mut response: HttpResponse) -> TransformerResult<HttpResponse> {
            response
                .metadata
                .insert("transformed_by".to_string(), json!(self.name()));
            Ok(response)
        }

        fn outbound_stream_event(&self, mut event: StreamEvent) -> TransformerResult<StreamEvent> {
            event
                .extra
                .insert("transformed_by".to_string(), json!(self.name()));
            Ok(event)
        }

        fn outbound_error(&self, response: HttpResponse) -> TransformerResult<ConduitError> {
            Ok(ConduitError::upstream("dummy provider error")
                .with_provider_status(response.status)
                .with_provider_body(response.json_body.unwrap_or_else(|| json!({}))))
        }

        fn aggregate_stream(&self, events: Vec<StreamEvent>) -> TransformerResult<HttpResponse> {
            Ok(HttpResponse {
                status: 200,
                json_body: Some(json!({
                    "event_count": events.len(),
                    "done": events.iter().any(|event| event.done),
                })),
                stream: events,
                ..HttpResponse::default()
            })
        }
    }

    struct DummyInbound;

    impl InboundTransformer for DummyInbound {
        fn name(&self) -> &'static str {
            "dummy-inbound"
        }

        fn inbound_request(&self, _request: HttpRequest) -> TransformerResult<LlmRequest> {
            Ok(LlmRequest {
                request_type: RequestType::Chat,
                api_format: ApiFormat::OpenAiChatCompletions,
                model: Some("dummy-model".to_string()),
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

    #[test]
    fn inbound_default_aggregate_stream_chunks_preserves_events_losslessly() {
        // The default `aggregate_stream_chunks` (used by inbound impls that
        // haven't overridden it yet) must preserve every event on the
        // `stream` field and surface a 200 — mirroring the contract Go's
        // `autoAggregateStream` expects from `Inbound.AggregateStreamChunks`.
        let transformer = DummyInbound;
        let events = vec![
            StreamEvent {
                data: Some("one".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                done: true,
                ..StreamEvent::default()
            },
        ];
        let result = transformer
            .aggregate_stream_chunks(events)
            .unwrap_or_else(|err| {
                panic!("default aggregate_stream_chunks must not error: {err:?}")
            });
        assert_eq!(result.status, 200);
        assert_eq!(result.stream.len(), 2);
        // Default sentinel body — concrete impls override with the real
        // aggregated chat-completion payload.
        assert_eq!(
            result
                .json_body
                .as_ref()
                .and_then(|body| body.get("aggregated_by")),
            Some(&json!("default"))
        );
    }

    struct DummyVideoTaskOutbound;

    impl VideoTaskOutbound for DummyVideoTaskOutbound {
        fn name(&self) -> &'static str {
            "dummy-video"
        }

        fn get_video_task_request(&self, task_id: &str) -> TransformerResult<HttpRequest> {
            Ok(HttpRequest {
                method: "GET".to_string(),
                path: format!("/v1/videos/{task_id}"),
                ..HttpRequest::default()
            })
        }

        fn delete_video_task_request(&self, task_id: &str) -> TransformerResult<HttpRequest> {
            Ok(HttpRequest {
                method: "DELETE".to_string(),
                path: format!("/v1/videos/{task_id}"),
                ..HttpRequest::default()
            })
        }

        fn video_task_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
            Ok(response)
        }
    }

    fn dummy_request() -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("dummy-model".to_string()),
            stream: true,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    #[test]
    fn registry_finds_outbound_by_channel_type_and_api_format() {
        let mut registry = TransformerRegistry::new();
        registry.register_outbound_box(
            "Dummy",
            ApiFormat::OpenAiChatCompletions,
            Box::new(DummyOutbound),
        );

        let transformer = registry.outbound("dummy", ApiFormat::OpenAiChatCompletions);
        assert_eq!(transformer.as_ref().map(|item| item.name()), Some("dummy"));

        let missing = registry.outbound("dummy", ApiFormat::AnthropicMessages);
        assert!(missing.is_none());
    }

    #[test]
    fn registry_format_fallback_applies_across_channels_without_crossing_formats() {
        let mut registry = TransformerRegistry::new();
        let wildcard: Arc<dyn OutboundTransformer> = Arc::new(DummyOutbound);
        registry.register_outbound_for_format(ApiFormat::AnthropicMessages, Arc::clone(&wildcard));

        for channel_type in ["anthropic", "custom-gateway", "OPENAI"] {
            let resolved = registry.outbound(channel_type, ApiFormat::AnthropicMessages);
            assert!(
                resolved
                    .as_ref()
                    .is_some_and(|resolved| Arc::ptr_eq(resolved, &wildcard)),
                "format fallback should work for every channel type"
            );
        }
        assert!(
            registry
                .outbound("anthropic", ApiFormat::GeminiContents)
                .is_none(),
            "a format fallback must not leak into another wire format"
        );
    }

    #[test]
    fn registry_exact_channel_registration_precedes_format_fallback() {
        let mut registry = TransformerRegistry::new();
        let wildcard: Arc<dyn OutboundTransformer> = Arc::new(DummyOutbound);
        let exact: Arc<dyn OutboundTransformer> = Arc::new(DummyOutbound);
        registry.register_outbound_for_format(ApiFormat::AnthropicMessages, Arc::clone(&wildcard));
        registry.register_outbound(
            "anthropic_aws",
            ApiFormat::AnthropicMessages,
            Arc::clone(&exact),
        );

        let exact_resolved = registry.outbound("ANTHROPIC_AWS", ApiFormat::AnthropicMessages);
        assert!(
            exact_resolved
                .as_ref()
                .is_some_and(|resolved| Arc::ptr_eq(resolved, &exact)),
            "exact transformer should resolve"
        );

        let fallback_resolved = registry.outbound("anthropic", ApiFormat::AnthropicMessages);
        assert!(
            fallback_resolved
                .as_ref()
                .is_some_and(|resolved| Arc::ptr_eq(resolved, &wildcard)),
            "fallback transformer should resolve"
        );
    }

    #[test]
    fn dummy_outbound_transforms_request_response_stream_error_and_aggregate()
    -> TransformerResult<()> {
        let transformer = DummyOutbound;
        let request = transformer.outbound_request(&dummy_request())?;

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(request.request_type, Some(RequestType::Chat));
        assert_eq!(request.api_format, Some(ApiFormat::OpenAiChatCompletions));
        assert_eq!(
            request
                .json_body
                .as_ref()
                .and_then(|body| body.get("stream")),
            Some(&json!(true))
        );

        let response = transformer.outbound_response(HttpResponse {
            status: 200,
            json_body: Some(json!({"id": "resp_1"})),
            ..HttpResponse::default()
        })?;
        assert_eq!(
            response.metadata.get("transformed_by"),
            Some(&json!("dummy"))
        );

        let event = transformer.outbound_stream_event(StreamEvent {
            event_type: Some("message.delta".to_string()),
            data: Some("hello".to_string()),
            ..StreamEvent::default()
        })?;
        assert_eq!(event.extra.get("transformed_by"), Some(&json!("dummy")));

        let err = transformer.outbound_error(HttpResponse {
            status: 429,
            json_body: Some(json!({"error": "rate limit"})),
            ..HttpResponse::default()
        })?;
        assert_eq!(err.kind, ErrorKind::Upstream);
        assert_eq!(err.provider_status, Some(429));

        let aggregate = transformer.aggregate_stream(vec![
            StreamEvent {
                data: Some("one".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                done: true,
                ..StreamEvent::default()
            },
        ])?;
        assert_eq!(aggregate.stream.len(), 2);
        assert_eq!(
            aggregate
                .json_body
                .as_ref()
                .and_then(|body| body.get("event_count")),
            Some(&json!(2))
        );
        assert_eq!(
            aggregate
                .json_body
                .as_ref()
                .and_then(|body| body.get("done")),
            Some(&json!(true))
        );

        Ok(())
    }

    #[test]
    fn registry_finds_video_task_transformer() -> TransformerResult<()> {
        let mut registry = TransformerRegistry::new();
        registry.register_video_task_outbound_box(
            "seedance",
            ApiFormat::SeedanceVideo,
            Box::new(DummyVideoTaskOutbound),
        );

        let transformer = registry
            .video_task_outbound("SEEDANCE", ApiFormat::SeedanceVideo)
            .ok_or_else(|| ConduitError::internal("missing dummy video task transformer"))?;

        let get_request = transformer.get_video_task_request("task_123")?;
        assert_eq!(get_request.method, "GET");
        assert_eq!(get_request.path, "/v1/videos/task_123");

        let delete_request = transformer.delete_video_task_request("task_123")?;
        assert_eq!(delete_request.method, "DELETE");
        assert_eq!(delete_request.path, "/v1/videos/task_123");

        Ok(())
    }

    // ---- Default `transform_response` (RESP-CHAIN-SPEC [Zhou]) ----

    /// Default outbound `transform_response`: OpenAI chat-completion JSON body →
    /// `LlmResponse` fields round-trip. Mirrors the Go OpenAI outbound
    /// `TransformResponse` (`conduit/llm/transformer/openai/outbound.go:232`)
    /// which is a straight `json.Unmarshal` into `llm.Response` — the default
    /// serde impl here is the Rust parity of that passthrough.
    #[test]
    fn default_outbound_transform_response_parses_openai_chat_body() -> TransformerResult<()> {
        let transformer = DummyOutbound;
        // OpenAI chat completion body (subset of fields that round-trip via
        // the unified `LlmResponse` serde derives — model.rs:509).
        let body_json = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "created": 1234567890_i64,
            "model": "gpt-4o-mini",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello! How can I help you?"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        });

        let response = transformer.transform_response(HttpResponse {
            status: 200,
            json_body: Some(body_json.clone()),
            ..HttpResponse::default()
        })?;

        assert_eq!(response.id, "chatcmpl-abc123");
        assert_eq!(response.object, "chat.completion");
        assert_eq!(response.created, 1234567890);
        assert_eq!(response.model, "gpt-4o-mini");
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].index, 0);
        let message = response.choices[0]
            .message
            .as_ref()
            .ok_or_else(|| ConduitError::internal("missing message"))?;
        assert_eq!(message.role.as_deref(), Some("assistant"));
        // Content is a `MessageContent` which collapses a bare string; the
        // unified struct serializes/deserializes `content` as the message body.
        let usage = response
            .usage
            .as_ref()
            .ok_or_else(|| ConduitError::internal("missing usage"))?;
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.total_tokens, 30);
        assert_eq!(response.choices[0].finish_reason.as_deref(), Some("stop"));

        Ok(())
    }

    /// Default outbound `transform_response` must reject empty / missing bodies
    /// with `ErrorKind::InvalidResponse` (the crate's existing malformed-payload
    /// kind — no new variant invented).
    #[test]
    fn default_outbound_transform_response_rejects_empty_body() {
        let transformer = DummyOutbound;
        match transformer.transform_response(HttpResponse {
            status: 200,
            body: Some(Vec::new()),
            ..HttpResponse::default()
        }) {
            Ok(_) => panic!("empty body must error"),
            Err(err) => assert_eq!(err.kind, ErrorKind::InvalidResponse),
        }

        match transformer.transform_response(HttpResponse {
            status: 200,
            ..HttpResponse::default()
        }) {
            Ok(_) => panic!("missing body must error"),
            Err(err) => assert_eq!(err.kind, ErrorKind::InvalidResponse),
        }
    }

    /// Default outbound `transform_response` falls back to deserializing raw
    /// `body` bytes when `json_body` is absent (parity with the Go
    /// `json.Unmarshal(httpResp.Body, ...)` path).
    #[test]
    fn default_outbound_transform_response_parses_raw_body_bytes() -> TransformerResult<()> {
        let transformer = DummyOutbound;
        let raw = serde_json::to_vec(&json!({
            "id": "raw-1",
            "object": "chat.completion",
            "model": "gpt-4o",
        }))
        .map_err(|err| ConduitError::internal("serialize raw response").with_source(err))?;
        let response = transformer.transform_response(HttpResponse {
            status: 200,
            body: Some(raw),
            ..HttpResponse::default()
        })?;
        assert_eq!(response.id, "raw-1");
        assert_eq!(response.model, "gpt-4o");
        Ok(())
    }

    /// Default inbound `transform_response`: `LlmResponse` → 200 JSON response,
    /// body re-parses equal. Mirrors Go's default inbound path where the unified
    /// response is serialized as JSON directly into the HTTP body.
    #[test]
    fn default_inbound_transform_response_serializes_to_200_json() -> TransformerResult<()> {
        let transformer = DummyInbound;
        let original = LlmResponse {
            id: "chatcmpl-xyz".to_string(),
            object: "chat.completion".to_string(),
            created: 42,
            model: "gpt-4o-mini".to_string(),
            choices: vec![Choice {
                index: 0,
                finish_reason: Some("stop".to_string()),
                ..Choice::default()
            }],
            ..LlmResponse::default()
        };

        let http_response = transformer.transform_response(original.clone())?;

        assert_eq!(http_response.status, 200);
        assert_eq!(
            http_response
                .headers
                .get("Content-Type")
                .map(String::as_str),
            Some("application/json")
        );

        // The body must round-trip — re-parse and compare structurally.
        let body_bytes = http_response
            .body
            .as_ref()
            .ok_or_else(|| ConduitError::internal("missing body"))?;
        let reparsed: LlmResponse = serde_json::from_slice(body_bytes)
            .map_err(|err| ConduitError::internal("parse response body").with_source(err))?;
        assert_eq!(reparsed.id, original.id);
        assert_eq!(reparsed.object, original.object);
        assert_eq!(reparsed.created, original.created);
        assert_eq!(reparsed.model, original.model);
        assert_eq!(reparsed.choices.len(), 1);
        assert_eq!(reparsed.choices[0].index, original.choices[0].index);
        assert_eq!(
            reparsed.choices[0].finish_reason,
            original.choices[0].finish_reason
        );

        Ok(())
    }
}
