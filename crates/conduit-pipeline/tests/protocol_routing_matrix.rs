use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conduit_core::ConduitError;
use conduit_llm::{ApiFormat, HttpRequest, HttpResponse, LlmRequest, StreamEvent};
use conduit_pipeline::middleware::PipelineContext;
use conduit_pipeline::pipeline::{Executor, Pipeline, PipelineCandidate};
use conduit_transformers::gemini::{GeminiOutboundConfig, GeminiPlatformType};
use conduit_transformers::{
    AnthropicInboundTransformer, AnthropicOutboundConfig, AnthropicOutboundTransformer,
    GeminiInboundTransformer, GeminiOutboundTransformer, InboundTransformer, OpenAiChatInbound,
    OpenAiResponsesOutbound, OutboundTransformer, TransformerRegistry, TransformerResult,
};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct OpenAiChatOutbound;

impl OutboundTransformer for OpenAiChatOutbound {
    fn name(&self) -> &'static str {
        "openai-compat-outbound"
    }

    fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
        Ok(HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            json_body: Some(
                conduit_transformers::openai_outbound::build_openai_outbound_body(request)?,
            ),
            request_type: Some(request.request_type),
            api_format: Some(ApiFormat::OpenAiChatCompletions),
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
        Ok(ConduitError::upstream("test provider error").with_provider_status(response.status))
    }
}

#[derive(Default)]
struct MatrixExecutor {
    last_request: Mutex<Option<HttpRequest>>,
}

impl MatrixExecutor {
    fn captured(&self) -> Option<HttpRequest> {
        self.last_request.lock().ok()?.clone()
    }
}

#[async_trait]
impl Executor for MatrixExecutor {
    async fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, ConduitError> {
        if let Ok(mut captured) = self.last_request.lock() {
            *captured = Some(request.clone());
        }
        let format = request
            .api_format
            .ok_or_else(|| ConduitError::internal("matrix request lost target API format"))?;
        let body = provider_response(format);
        Ok(HttpResponse {
            status: 200,
            json_body: Some(body),
            ..HttpResponse::default()
        })
    }

    async fn execute_stream(
        &self,
        request: &HttpRequest,
    ) -> Result<Vec<StreamEvent>, ConduitError> {
        if let Ok(mut captured) = self.last_request.lock() {
            *captured = Some(request.clone());
        }
        let format = request
            .api_format
            .ok_or_else(|| ConduitError::internal("matrix stream lost target API format"))?;
        Ok(provider_stream(format))
    }
}

fn provider_response(format: ApiFormat) -> Value {
    match format {
        ApiFormat::OpenAiChatCompletions => json!({
            "id": "chatcmpl-matrix",
            "object": "chat.completion",
            "created": 1,
            "model": "actual-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "pong"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3}
        }),
        ApiFormat::AnthropicMessages => json!({
            "id": "msg_matrix",
            "type": "message",
            "role": "assistant",
            "model": "actual-model",
            "content": [{"type": "text", "text": "pong"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 2, "output_tokens": 1}
        }),
        ApiFormat::GeminiContents => json!({
            "responseId": "gemini-matrix",
            "modelVersion": "actual-model",
            "candidates": [{
                "index": 0,
                "content": {"role": "model", "parts": [{"text": "pong"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 2,
                "candidatesTokenCount": 1,
                "totalTokenCount": 3
            }
        }),
        ApiFormat::OpenAiResponses => json!({
            "id": "resp_matrix",
            "object": "response",
            "created_at": 1,
            "model": "actual-model",
            "status": "completed",
            "output": [{
                "id": "msg_matrix",
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "pong", "annotations": []}]
            }],
            "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
        }),
        other => json!({"unsupported_format": other.as_str()}),
    }
}

fn event(event_type: Option<&str>, data: Value) -> StreamEvent {
    StreamEvent {
        event_type: event_type.map(str::to_string),
        data: Some(data.to_string()),
        ..StreamEvent::default()
    }
}

fn provider_stream(format: ApiFormat) -> Vec<StreamEvent> {
    match format {
        ApiFormat::OpenAiChatCompletions => vec![
            event(
                None,
                json!({
                    "id": "chatcmpl-stream",
                    "object": "chat.completion.chunk",
                    "created": 1,
                    "model": "actual-model",
                    "choices": [{
                        "index": 0,
                        "delta": {"role": "assistant", "content": "pong"},
                        "finish_reason": null
                    }]
                }),
            ),
            event(
                None,
                json!({
                    "id": "chatcmpl-stream",
                    "object": "chat.completion.chunk",
                    "created": 1,
                    "model": "actual-model",
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3}
                }),
            ),
        ],
        ApiFormat::AnthropicMessages => vec![
            event(
                Some("message_start"),
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg-stream",
                        "type": "message",
                        "role": "assistant",
                        "model": "actual-model",
                        "content": [],
                        "usage": {"input_tokens": 2, "output_tokens": 0}
                    }
                }),
            ),
            event(
                Some("content_block_start"),
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "text", "text": ""}
                }),
            ),
            event(
                Some("content_block_delta"),
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "pong"}
                }),
            ),
            event(
                Some("content_block_stop"),
                json!({"type": "content_block_stop", "index": 0}),
            ),
            event(
                Some("message_delta"),
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn"},
                    "usage": {"output_tokens": 1}
                }),
            ),
            event(Some("message_stop"), json!({"type": "message_stop"})),
        ],
        ApiFormat::GeminiContents => vec![event(
            None,
            json!({
                "responseId": "gemini-stream",
                "modelVersion": "actual-model",
                "candidates": [{
                    "index": 0,
                    "content": {"role": "model", "parts": [{"text": "pong"}]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 2,
                    "candidatesTokenCount": 1,
                    "totalTokenCount": 3
                }
            }),
        )],
        ApiFormat::OpenAiResponses => vec![
            event(
                None,
                json!({
                    "type": "response.created",
                    "response": {"id": "resp-stream", "model": "actual-model", "created_at": 1}
                }),
            ),
            event(
                None,
                json!({"type": "response.output_text.delta", "delta": "pong"}),
            ),
            event(
                None,
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp-stream",
                        "model": "actual-model",
                        "created_at": 1,
                        "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
                    }
                }),
            ),
        ],
        _ => Vec::new(),
    }
}

fn client_requests() -> Vec<(&'static str, Arc<dyn InboundTransformer>, HttpRequest)> {
    vec![
        (
            "openai",
            Arc::new(OpenAiChatInbound::new()),
            HttpRequest {
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                content_type: Some("application/json".to_string()),
                json_body: Some(json!({
                    "model": "public-model",
                    "messages": [{"role": "user", "content": "ping"}]
                })),
                ..HttpRequest::default()
            },
        ),
        (
            "anthropic",
            Arc::new(AnthropicInboundTransformer::new()),
            HttpRequest {
                method: "POST".to_string(),
                path: "/v1/messages".to_string(),
                json_body: Some(json!({
                    "model": "public-model",
                    "max_tokens": 128,
                    "messages": [{"role": "user", "content": "ping"}]
                })),
                ..HttpRequest::default()
            },
        ),
        (
            "gemini",
            Arc::new(GeminiInboundTransformer::new()),
            HttpRequest {
                method: "POST".to_string(),
                path: "/v1beta/models/public-model:generateContent".to_string(),
                body: serde_json::to_vec(&json!({
                    "contents": [{"role": "user", "parts": [{"text": "ping"}]}]
                }))
                .ok(),
                ..HttpRequest::default()
            },
        ),
    ]
}

fn as_stream_request(client_name: &str, mut request: HttpRequest) -> HttpRequest {
    match client_name {
        "openai" | "anthropic" => {
            if let Some(object) = request.json_body.as_mut().and_then(Value::as_object_mut) {
                object.insert("stream".to_string(), Value::Bool(true));
            }
        }
        "gemini" => {
            request.path = request
                .path
                .replace(":generateContent", ":streamGenerateContent");
        }
        _ => {}
    }
    request
}

fn registry() -> Result<Arc<TransformerRegistry>, Box<dyn std::error::Error>> {
    let mut registry = TransformerRegistry::new();
    registry.register_outbound_for_format(
        ApiFormat::OpenAiChatCompletions,
        Arc::new(OpenAiChatOutbound),
    );
    registry.register_outbound_for_format(
        ApiFormat::AnthropicMessages,
        Arc::new(AnthropicOutboundTransformer::new(AnthropicOutboundConfig {
            platform: conduit_transformers::anthropic::PlatformType::Direct,
            base_url: String::new(),
            api_key: String::new(),
            endpoint_path: Some("/v1/messages".to_string()),
            project_id: None,
            region: None,
        })),
    );
    registry.register_outbound_for_format(
        ApiFormat::GeminiContents,
        Arc::new(GeminiOutboundTransformer::with_config(
            GeminiOutboundConfig {
                base_url: String::new(),
                api_version: "v1beta".to_string(),
                endpoint_path: String::new(),
                platform_type: GeminiPlatformType::Direct,
            },
            String::new(),
        )),
    );
    registry.register_outbound_for_format(
        ApiFormat::OpenAiResponses,
        Arc::new(OpenAiResponsesOutbound::new("", "")?),
    );
    Ok(Arc::new(registry))
}

fn response_text(response: &HttpResponse) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(body) = response.body.as_deref() {
        return Ok(String::from_utf8(body.to_vec())?);
    }
    Ok(serde_json::to_string(response.json_body.as_ref().ok_or(
        "client response contained neither body nor JSON",
    )?)?)
}

#[tokio::test]
async fn non_stream_protocol_and_path_matrix_is_fully_wired() -> TestResult {
    let target_formats = [
        (ApiFormat::OpenAiChatCompletions, "/v1/chat/completions"),
        (ApiFormat::AnthropicMessages, "/v1/messages"),
        (
            ApiFormat::GeminiContents,
            "/v1beta/models/actual-model:generateContent",
        ),
        (ApiFormat::OpenAiResponses, "/v1/responses"),
    ];

    for (client_name, inbound, raw_request) in client_requests() {
        for (target_format, expected_path) in target_formats {
            let executor = Arc::new(MatrixExecutor::default());
            let pipeline = Pipeline::new(
                Arc::clone(&inbound),
                Arc::new(OpenAiChatOutbound),
                executor.clone(),
            )
            .with_outbound_registry(registry()?);
            let candidate = PipelineCandidate {
                id: format!("{client_name}-{}", target_format.as_str()),
                base_url: Some("https://upstream.example".to_string()),
                credential: Some("test-upstream-key".to_string()),
                actual_model: Some("actual-model".to_string()),
                api_format: target_format.as_str().to_string(),
                endpoint_transport: Some("http".to_string()),
                channel_type: "custom".to_string(),
                ..PipelineCandidate::from("matrix")
            };

            let (response, attempts) = pipeline
                .process_with_inbound(
                    &mut PipelineContext::new(),
                    inbound.as_ref(),
                    raw_request.clone(),
                    &raw_request,
                    &[candidate],
                )
                .await?;

            assert_eq!(attempts.len(), 1, "{client_name} -> {target_format:?}");
            assert!(
                response_text(&response)?.contains("pong"),
                "{client_name} -> {target_format:?} lost response content"
            );
            let captured = executor
                .captured()
                .ok_or("matrix executor did not capture the upstream request")?;
            assert_eq!(captured.api_format, Some(target_format));
            assert_eq!(
                captured.path, expected_path,
                "{client_name} -> {target_format:?} selected the wrong path"
            );
            assert_eq!(
                captured.url.as_deref(),
                Some(format!("https://upstream.example{expected_path}").as_str()),
                "{client_name} -> {target_format:?} selected the wrong URL"
            );
        }
    }

    Ok(())
}

#[tokio::test]
async fn streaming_protocol_and_path_matrix_is_fully_wired() -> TestResult {
    let target_formats = [
        (ApiFormat::OpenAiChatCompletions, "/v1/chat/completions"),
        (ApiFormat::AnthropicMessages, "/v1/messages"),
        (
            ApiFormat::GeminiContents,
            "/v1beta/models/actual-model:streamGenerateContent?alt=sse",
        ),
        (ApiFormat::OpenAiResponses, "/v1/responses"),
    ];

    for (client_name, inbound, raw_request) in client_requests() {
        let raw_request = as_stream_request(client_name, raw_request);
        for (target_format, expected_path) in target_formats {
            let executor = Arc::new(MatrixExecutor::default());
            let pipeline = Pipeline::new(
                Arc::clone(&inbound),
                Arc::new(OpenAiChatOutbound),
                executor.clone(),
            )
            .with_outbound_registry(registry()?);
            let candidate = PipelineCandidate {
                id: format!("stream-{client_name}-{}", target_format.as_str()),
                base_url: Some("https://upstream.example".to_string()),
                credential: Some("test-upstream-key".to_string()),
                actual_model: Some("actual-model".to_string()),
                api_format: target_format.as_str().to_string(),
                endpoint_transport: Some("http".to_string()),
                channel_type: "custom".to_string(),
                ..PipelineCandidate::from("matrix-stream")
            };

            let (response, attempts) = pipeline
                .process_with_inbound(
                    &mut PipelineContext::new(),
                    inbound.as_ref(),
                    raw_request.clone(),
                    &raw_request,
                    &[candidate],
                )
                .await?;

            assert_eq!(attempts.len(), 1, "{client_name} -> {target_format:?}");
            assert!(
                response
                    .stream
                    .iter()
                    .filter_map(|stream_event| stream_event.data.as_deref())
                    .any(|data| data.contains("pong")),
                "{client_name} -> {target_format:?} lost streamed response content: {:?}",
                response.stream
            );
            let captured = executor
                .captured()
                .ok_or("matrix executor did not capture the stream request")?;
            assert_eq!(captured.api_format, Some(target_format));
            assert_eq!(
                captured.path, expected_path,
                "{client_name} -> {target_format:?} selected the wrong stream path"
            );
            let expected_url = format!("https://upstream.example{expected_path}");
            assert_eq!(captured.url.as_deref(), Some(expected_url.as_str()));
        }
    }

    Ok(())
}
