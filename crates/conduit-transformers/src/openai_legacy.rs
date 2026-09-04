//! Legacy OpenAI **text completions** transformers (`POST /v1/completions`).
//!
//! Go parity source:
//! - `conduit/llm/transformer/openai/completion.go`            — wire types.
//! - `conduit/llm/transformer/openai/completion_inbound.go`    — client-facing
//!   inbound (`CompletionInboundTransformer`): request → `llm.Request`,
//!   `llm.Response` → client HTTP response, per-chunk stream shaping.
//! - `conduit/llm/transformer/openai/completion_outbound.go`   — provider-facing
//!   outbound (`CompletionOutboundTransformer`): `llm.Request` → provider HTTP
//!   request, provider HTTP response → `llm.Response`.
//! - `conduit/llm/transformer/openai/completion_aggregator.go` — streaming
//!   aggregation (`AggregateCompletionStreamChunks`).
//!
//! This module supplies the two struct wrappers the Rust bridge needs to serve
//! the legacy `/v1/completions` route:
//! - [`OpenAiCompletionInbound`]  — implements [`InboundTransformer`].
//! - [`OpenAiCompletionOutbound`] — implements [`OutboundTransformer`].
//!
//! The request-body parsing (`prompt`/`suffix`/`max_tokens`/… → unified
//! [`conduit_llm::CompletionRequest`]) already exists as the free function
//! [`normalize_completions_body`] in `crate::openai` (dispatched by
//! `normalize_openai_body` for [`ApiFormat::OpenAiCompletions`]); the inbound
//! wrapper reuses it and layers on the Go request-level guards
//! (non-empty body, JSON content type, `model` required, `prompt` required).
//!
//! ## `ApiFormat`
//!
//! The dedicated legacy-completions variant [`ApiFormat::OpenAiCompletions`]
//! (`"openai/completions"`) and request type [`RequestType::Completion`] both
//! already exist in `conduit_llm::constants` — no gap. They mirror Go's
//! `llm.APIFormatOpenAICompletion` / `llm.RequestTypeCompletion`.
//!
//! ## Compact (`/v1/responses/compact`)
//!
//! The Responses **compact** flavour does **not** live here. In Go it is a
//! distinct `responses.CompactInboundTransformer`
//! (`conduit/llm/transformer/openai/responses/compact_inbound.go`) that sets
//! `RequestType=Compact` + `APIFormat=OpenAIResponseCompact` and reuses the
//! Responses `convertInputToMessages`. On the Rust side that surface is already
//! covered by the existing `crate::openai::OpenAiResponsesInbound`, whose
//! `inbound_request` calls `normalize_responses_body(body, compact)` and selects
//! [`RequestType::Compact`] / [`ApiFormat::OpenAiResponsesCompact`] when the
//! request path/api_format is the compact one (see `OpenAiResponsesInbound`,
//! `openai.rs`). Compact therefore needs **no** new transformer in this file —
//! it reuses `OpenAiResponsesInbound`.

use conduit_core::{ConduitError, ErrorKind};
use conduit_llm::model::HeaderMap;
use conduit_llm::{
    ApiFormat, HttpRequest, HttpResponse, LlmRequest, LlmRequestPayload, LlmResponse, RequestType,
    StreamEvent, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::TransformerResult;
use crate::openai::normalize_completions_body;
use crate::openai_outbound::build_openai_outbound_body;
use crate::traits::{InboundTransformer, OutboundTransformer};

/// Legacy completions endpoint path. Mirrors Go `buildURL()` default
/// (`completion_outbound.go:134`, `config.BaseURL + "/completions"`); the
/// leading `/v1` matches the inbound route (`openai.rs` `COMPLETIONS_PATH`).
/// The provider base URL / auth are applied by the outbound wiring layer (the
/// same split the existing `openai_outbound` uses — body building is separate
/// from `resolve_outbound_url`).
const COMPLETIONS_PATH: &str = "/v1/completions";

// ---------------------------------------------------------------------------
// Wire types — byte-compatible with Go `openai.CompletionResponse` /
// `CompletionChoice` / `CompletionUsage` (`completion.go:26-46`). The request
// wire type is not modeled here: inbound parsing goes through the unified
// `CompletionRequest` payload (`normalize_completions_body`) and outbound body
// building goes through `build_openai_outbound_body`, both of which already
// produce the flat OpenAI completion request shape.
// ---------------------------------------------------------------------------

/// One `choices[]` entry of a legacy completion response. Mirrors Go
/// `openai.CompletionChoice` (`completion.go:35-40`).
///
/// `logprobs` is kept as an opaque [`Value`] (Go `*llm.LogprobsContent`,
/// `omitempty`) — the unified response layer models `Choice.logprobs` the same
/// permissive way (`model.rs`). `finish_reason` is `omitempty` (Go `*string`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CompletionChoiceWire {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub index: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// Token usage block of a legacy completion response. Mirrors Go
/// `openai.CompletionUsage` (`completion.go:42-46`). None of the three fields
/// carry `omitempty`, so they are always serialized (zeros when absent),
/// matching Go's non-pointer `Usage CompletionUsage` field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CompletionUsageWire {
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

/// Full legacy completion response body. Mirrors Go `openai.CompletionResponse`
/// (`completion.go:26-33`). All fields are always serialized (no `omitempty`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CompletionResponseWire {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub object: String,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub choices: Vec<CompletionChoiceWire>,
    #[serde(default)]
    pub usage: CompletionUsageWire,
}

/// The unified `LlmResponse.completion` sub-object shape. Mirrors Go
/// `llm.CompletionResponse` (`llm/completion.go:25-27`), which carries only
/// `choices` (usage lives on the parent `llm.Response`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompletionData {
    #[serde(default)]
    choices: Vec<CompletionChoiceWire>,
}

// ---------------------------------------------------------------------------
// Inbound (client-facing) — `POST /v1/completions`.
// ---------------------------------------------------------------------------

/// Inbound transformer for the legacy OpenAI Completions API surface
/// (`POST /v1/completions`). Implements [`InboundTransformer`].
///
/// Mirrors Go `openai.CompletionInboundTransformer` (`completion_inbound.go`).
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiCompletionInbound;

impl OpenAiCompletionInbound {
    pub const fn new() -> Self {
        Self
    }
}

impl InboundTransformer for OpenAiCompletionInbound {
    fn name(&self) -> &'static str {
        "openai/completions"
    }

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        // Body presence mirrors Go's `len(httpReq.Body) == 0` guard
        // (completion_inbound.go:32-34).
        let body = read_json_body(&request)?;

        // Content-type guard mirrors completion_inbound.go:36-43: an empty
        // content type defaults to `application/json`; a present, non-JSON
        // content type is rejected.
        let content_type = request
            .content_type
            .as_deref()
            .or_else(|| {
                request.headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        .then_some(value.as_str())
                })
            })
            .unwrap_or("application/json");
        if !content_type
            .to_ascii_lowercase()
            .contains("application/json")
        {
            return Err(ConduitError::invalid_request(format!(
                "unsupported content type: {content_type}"
            )));
        }

        let mut llm_request = normalize_completions_body(body)?;

        // Go `compReq.Model == ""` guard (completion_inbound.go:52-54).
        if llm_request
            .model
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true)
        {
            return Err(ConduitError::invalid_request("model is required"));
        }

        // Go `compReq.Prompt == ""` guard (completion_inbound.go:56-58). Go's
        // wire `Prompt` is a bare `string`, so it rejects a missing or
        // empty-string prompt. The unified `CompletionRequest.prompt` is an
        // `Option<Value>` (a documented superset that also accepts the array
        // form the real OpenAI `/v1/completions` API allows). We reproduce Go's
        // "required, non-empty" contract for the string case and additionally
        // accept a non-empty array/other JSON prompt.
        let prompt_ok = match &llm_request.payload {
            LlmRequestPayload::Completion(completion) => match completion.prompt.as_ref() {
                None | Some(Value::Null) => false,
                Some(Value::String(text)) => !text.is_empty(),
                Some(Value::Array(items)) => !items.is_empty(),
                Some(_) => true,
            },
            // `normalize_completions_body` always yields a `Completion` payload;
            // any other variant is a programming error, not a client error.
            _ => false,
        };
        if !prompt_ok {
            return Err(ConduitError::invalid_request("prompt is required"));
        }

        // Carry HTTP-layer context onto the unified request, matching Go's
        // `RawRequest = httpReq` propagation and the sibling OpenAI inbound
        // transformers' header/metadata merge (`openai.rs`).
        llm_request.extra_headers = request.headers;
        llm_request.metadata = request.metadata;
        if let Some(request_id) = request.request_id {
            llm_request
                .metadata
                .insert("request_id".to_string(), Value::String(request_id));
        }
        if let Some(client_ip) = request.client_ip {
            llm_request
                .metadata
                .insert("client_ip".to_string(), Value::String(client_ip));
        }

        Ok(llm_request)
    }

    // `inbound_response` (raw provider HTTP response → client HTTP response) is
    // not part of the Go completion inbound contract — Go reshapes the unified
    // `llm.Response` via `TransformResponse` (mapped to `transform_response`
    // below), not the raw provider bytes. Stubbed to match the sibling OpenAI
    // inbound transformers.
    fn inbound_response(&self, _response: HttpResponse) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI completions inbound response transform is not implemented yet",
        ))
    }

    fn inbound_stream_event(&self, _event: StreamEvent) -> TransformerResult<StreamEvent> {
        Err(ConduitError::internal(
            "OpenAI completions inbound stream-event transform is not implemented yet",
        ))
    }

    fn inbound_error(&self, _error: &ConduitError) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI completions inbound error mapping is not implemented yet",
        ))
    }

    /// Unified [`LlmResponse`] → client-facing legacy completion HTTP response.
    ///
    /// Mirrors Go `CompletionInboundTransformer.TransformResponse`
    /// (`completion_inbound.go:124-181`): when the unified response carries a
    /// `completion` sub-body, reshape it into the wire
    /// [`CompletionResponseWire`] (`id`/`object`/`created`/`model`, the
    /// `choices[].text` list, and the flat `usage` block); when it carries an
    /// `error`, serialize the OpenAI error envelope with the error's status
    /// code; otherwise fail with Go's "missing completion data" error.
    fn transform_response(&self, response: LlmResponse) -> TransformerResult<HttpResponse> {
        let (status, body) = if let Some(wire) = completion_wire_from_llm(&response)? {
            let body = serde_json::to_vec(&wire).map_err(|err| {
                ConduitError::internal("failed to marshal completion response").with_source(err)
            })?;
            (200_u16, body)
        } else if let Some(error) = response.error.as_ref() {
            // `ResponseError` serializes to `{"error": {...detail...}}` (its
            // `detail` field is `#[serde(rename = "error")]`, `status_code` is
            // `#[serde(skip)]`), matching Go `xjson.MustMarshal(&OpenAIError{...})`.
            let body = serde_json::to_vec(error).map_err(|err| {
                ConduitError::internal("failed to marshal completion error response")
                    .with_source(err)
            })?;
            (error.status_code, body)
        } else {
            return Err(ConduitError::internal(
                "completion response missing completion data",
            ));
        };

        Ok(json_response(status, body))
    }

    /// Aggregate legacy-completion provider streaming chunks into a single
    /// non-streaming response. Mirrors Go `AggregateCompletionStreamChunks`
    /// (`completion_aggregator.go:14-105`) exactly: concatenate every
    /// `choices[].text`, keep the first non-empty `id`/`model`/`created`,
    /// last-wins `finish_reason` (default `"stop"`) and last-non-zero `usage`,
    /// and emit a unified [`LlmResponse`] with `object: "text_completion"`.
    ///
    /// The aggregated unified response is serialized to the body (matching Go's
    /// `json.Marshal(response)` where `response` is a `*llm.Response`), wrapped
    /// in a 200 with `Content-Type: application/json` + `Cache-Control:
    /// no-cache` — the same wrapping the sibling `OpenAiChatInbound` aggregator
    /// uses. Original events are preserved on `stream` for retry/debug.
    fn aggregate_stream_chunks(&self, events: Vec<StreamEvent>) -> TransformerResult<HttpResponse> {
        let mut id = String::new();
        let mut model = String::new();
        let mut created: i64 = 0;
        let mut usage: Option<Usage> = None;
        let mut accumulated_text = String::new();
        let mut finish_reason: Option<String> = None;
        let mut saw_chunk = false;

        for event in &events {
            let Some(data) = event.data.as_deref() else {
                continue;
            };
            // Go `bytes.HasPrefix(chunk.Data, []byte("[DONE]"))` skip.
            if data.starts_with("[DONE]") {
                continue;
            }
            // Go silently `continue`s on a chunk that fails to decode.
            let Ok(chunk) = serde_json::from_str::<CompletionResponseWire>(data) else {
                continue;
            };

            saw_chunk = true;
            if id.is_empty() && !chunk.id.is_empty() {
                id = chunk.id.clone();
            }
            if model.is_empty() && !chunk.model.is_empty() {
                model = chunk.model.clone();
            }
            if created == 0 && chunk.created != 0 {
                created = chunk.created;
            }
            for choice in &chunk.choices {
                accumulated_text.push_str(&choice.text);
                if choice.finish_reason.is_some() {
                    finish_reason = choice.finish_reason.clone();
                }
            }
            if chunk.usage.prompt_tokens > 0 || chunk.usage.total_tokens > 0 {
                usage = Some(usage_from_wire(&chunk.usage));
            }
        }

        // Go: no decodable chunk → `json.Marshal(&llm.Response{})`.
        if !saw_chunk {
            let body = serde_json::to_vec(&LlmResponse::default()).map_err(|err| {
                ConduitError::internal("failed to marshal empty aggregated completion response")
                    .with_source(err)
            })?;
            return Ok(HttpResponse {
                stream: events,
                ..json_response(200, body)
            });
        }

        // Go: `if finishReason == nil { finishReason = lo.ToPtr("stop") }`.
        let choice = CompletionChoiceWire {
            text: accumulated_text,
            index: 0,
            logprobs: None,
            finish_reason: Some(finish_reason.unwrap_or_else(|| "stop".to_string())),
        };
        let completion_value = serde_json::to_value(CompletionData {
            choices: vec![choice],
        })
        .map_err(|err| {
            ConduitError::internal("failed to encode aggregated completion").with_source(err)
        })?;

        let aggregated = LlmResponse {
            id,
            object: "text_completion".to_string(),
            created,
            model,
            completion: Some(completion_value),
            usage,
            request_type: Some(RequestType::Completion),
            api_format: Some(ApiFormat::OpenAiCompletions),
            ..LlmResponse::default()
        };

        let body = serde_json::to_vec(&aggregated).map_err(|err| {
            ConduitError::internal("failed to marshal aggregated completion response")
                .with_source(err)
        })?;

        Ok(HttpResponse {
            stream: events,
            ..json_response(200, body)
        })
    }

    /// Wrap the unified `LlmResponse` stream, mapping each chunk into a client
    /// legacy-completion SSE event. Mirrors Go
    /// `CompletionInboundTransformer.transformStreamChunk`
    /// (`completion_inbound.go:192-241`): a `[DONE]` sentinel becomes a
    /// `[DONE]` event, a chunk with no `completion` sub-body is dropped, and
    /// any other chunk is reshaped into the wire [`CompletionResponseWire`].
    fn transform_stream(
        &self,
        events: Box<dyn Iterator<Item = LlmResponse> + Send>,
    ) -> TransformerResult<Box<dyn Iterator<Item = StreamEvent> + Send>> {
        Ok(Box::new(events.filter_map(|resp| {
            if resp.object == "[DONE]" {
                return Some(StreamEvent {
                    data: Some("[DONE]".to_string()),
                    ..StreamEvent::default()
                });
            }
            // `Ok(None)` (no completion sub-body) drops the chunk, matching Go's
            // `return nil, nil`; a decode error also drops it (kept lazy — the
            // trait's stream contract has no per-item error channel).
            let wire = completion_wire_from_llm(&resp).ok().flatten()?;
            let data = serde_json::to_string(&wire).ok()?;
            Some(StreamEvent {
                data: Some(data),
                ..StreamEvent::default()
            })
        })))
    }
}

// ---------------------------------------------------------------------------
// Outbound (provider-facing) — `POST {base}/completions`.
// ---------------------------------------------------------------------------

/// Outbound transformer for the legacy OpenAI Completions API surface.
/// Implements [`OutboundTransformer`].
///
/// Mirrors Go `openai.CompletionOutboundTransformer` (`completion_outbound.go`).
///
/// Base-URL normalization, bearer auth, and model mapping are **not** performed
/// here: they are applied by the outbound wiring layer (`openai_outbound`'s
/// `Config` / `resolve_outbound_url`), the same body-vs-URL split the existing
/// OpenAI outbound uses. This transformer produces the request **body** (via
/// the shared [`build_openai_outbound_body`]) and the `/completions` **path**,
/// and converts the provider response back into the unified shape.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiCompletionOutbound;

impl OpenAiCompletionOutbound {
    pub const fn new() -> Self {
        Self
    }
}

impl OutboundTransformer for OpenAiCompletionOutbound {
    fn name(&self) -> &'static str {
        "openai/completions"
    }

    /// Unified [`LlmRequest`] → provider HTTP request. Mirrors Go
    /// `CompletionOutboundTransformer.TransformRequest`
    /// (`completion_outbound.go:54-123`): require a completion payload + model,
    /// serialize the flat completion request body, and `POST` it to
    /// `{base}/completions`.
    fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
        // Go `llmReq.Completion == nil` guard (completion_outbound.go:62-64).
        if !matches!(request.payload, LlmRequestPayload::Completion(_)) {
            return Err(ConduitError::invalid_request(
                "completion request is nil in llm.Request",
            ));
        }

        // Shared builder: serializes the unified `CompletionRequest` payload,
        // injects top-level `model` (with the Go `model is required` guard) and
        // `stream`, and merges `extra_body` — reproducing Go's wire
        // `CompletionRequest{...}` construction (completion_outbound.go:70-96).
        let body = build_openai_outbound_body(request)?;

        let mut headers = HeaderMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Accept".to_string(), "application/json".to_string());

        Ok(HttpRequest {
            method: "POST".to_string(),
            path: COMPLETIONS_PATH.to_string(),
            headers,
            json_body: Some(body),
            request_type: Some(RequestType::Completion),
            api_format: Some(ApiFormat::OpenAiCompletions),
            ..HttpRequest::default()
        })
    }

    /// Passthrough. Base-URL / header rewriting is a wiring-layer concern (see
    /// the struct doc); Go's completion outbound has no response-header mutation.
    fn outbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        Ok(response)
    }

    fn outbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        Ok(event)
    }

    /// Provider error response → [`ConduitError`]. Mirrors the crate convention
    /// (an upstream error carrying the provider status + body); Go routes the
    /// same information through `TransformOpenAIError`.
    fn outbound_error(&self, response: HttpResponse) -> TransformerResult<ConduitError> {
        let status = response.status;
        let headers = response.headers.clone();
        let body = response
            .json_body
            .clone()
            .or_else(|| {
                response
                    .body
                    .as_ref()
                    .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            })
            .unwrap_or(Value::Null);
        let client_status = if (400..=599).contains(&status) {
            status
        } else {
            502
        };
        Ok(ConduitError::upstream("openai completion provider error")
            .with_provider_status(status)
            .with_http_status(client_status)
            .with_provider_headers(headers)
            .with_provider_body(body))
    }

    /// Provider HTTP response → unified [`LlmResponse`]. Mirrors Go
    /// `CompletionOutboundTransformer.TransformResponse` + `completionResponseToLLM`
    /// (`completion_outbound.go:137-196`): a `>= 400` status is surfaced as an
    /// upstream error; otherwise decode the wire [`CompletionResponseWire`] and
    /// lift its `choices` onto `LlmResponse.completion`, with `usage` only when
    /// the provider reported non-zero tokens.
    fn transform_response(&self, response: HttpResponse) -> TransformerResult<LlmResponse> {
        // Go `httpResp.StatusCode >= 400` → `TransformError` (completion_outbound.go:145-150).
        if response.status >= 400 {
            let body = response
                .json_body
                .clone()
                .or_else(|| {
                    response
                        .body
                        .as_ref()
                        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
                })
                .unwrap_or(Value::Null);
            return Err(ConduitError::upstream("openai completion provider error")
                .with_provider_status(response.status)
                .with_provider_body(body));
        }

        let value = response_json_body(&response)?;
        let wire: CompletionResponseWire = serde_json::from_value(value).map_err(|err| {
            ConduitError::new(
                ErrorKind::InvalidResponse,
                "failed to decode completion response",
            )
            .with_source(err)
        })?;

        completion_response_wire_to_llm(&wire)
    }
}

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

/// Replicates the (private) `crate::openai::request_json_body` (`openai.rs`):
/// prefer the pre-parsed `json_body`, else decode the raw `body` bytes as JSON,
/// else fail. Kept local because the original is not exported.
fn read_json_body(request: &HttpRequest) -> TransformerResult<Value> {
    if let Some(json_body) = &request.json_body {
        return Ok(json_body.clone());
    }

    let body = request
        .body
        .as_deref()
        .ok_or_else(|| ConduitError::invalid_request("OpenAI inbound request body is required"))?;

    serde_json::from_slice(body).map_err(|err| {
        ConduitError::invalid_request("OpenAI inbound request body must be valid JSON")
            .with_source(err)
    })
}

/// Extract a JSON body from a provider [`HttpResponse`], mirroring the default
/// [`OutboundTransformer::transform_response`] extraction: prefer `json_body`,
/// else decode `body` bytes, surfacing [`ErrorKind::InvalidResponse`] on an
/// empty/missing/invalid body.
fn response_json_body(response: &HttpResponse) -> TransformerResult<Value> {
    if let Some(value) = response.json_body.as_ref() {
        return Ok(value.clone());
    }
    if let Some(bytes) = response.body.as_ref() {
        if bytes.is_empty() {
            return Err(ConduitError::new(
                ErrorKind::InvalidResponse,
                "provider response body is empty",
            ));
        }
        return serde_json::from_slice::<Value>(bytes).map_err(|err| {
            ConduitError::new(
                ErrorKind::InvalidResponse,
                "failed to parse provider response body as JSON",
            )
            .with_source(err)
        });
    }
    Err(ConduitError::new(
        ErrorKind::InvalidResponse,
        "provider response has no body",
    ))
}

/// Build a 200-style JSON [`HttpResponse`] with the legacy-completion headers
/// (`Content-Type: application/json` + `Cache-Control: no-cache`), matching Go
/// `completion_inbound.go:173-180`.
fn json_response(status: u16, body: Vec<u8>) -> HttpResponse {
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("Cache-Control".to_string(), "no-cache".to_string());
    HttpResponse {
        status,
        headers,
        body: Some(body),
        ..HttpResponse::default()
    }
}

/// Convert a wire [`CompletionUsageWire`] into the unified [`Usage`]. Token
/// counts are non-negative in practice; a negative wire value is clamped to 0.
fn usage_from_wire(wire: &CompletionUsageWire) -> Usage {
    Usage {
        prompt_tokens: wire.prompt_tokens.max(0) as u64,
        completion_tokens: wire.completion_tokens.max(0) as u64,
        total_tokens: wire.total_tokens.max(0) as u64,
        ..Usage::default()
    }
}

/// Reshape a unified [`LlmResponse`] into the legacy completion wire response,
/// or `None` when it carries no `completion` sub-body (mirroring Go's
/// `llmResp.Completion == nil` branch). `usage` defaults to zeros when the
/// unified response has none (Go's non-pointer `CompletionUsage`).
fn completion_wire_from_llm(
    response: &LlmResponse,
) -> TransformerResult<Option<CompletionResponseWire>> {
    let Some(completion) = response.completion.as_ref() else {
        return Ok(None);
    };

    let choices: Vec<CompletionChoiceWire> = match completion.get("choices") {
        Some(value) => serde_json::from_value(value.clone()).map_err(|err| {
            ConduitError::internal("failed to decode unified completion choices").with_source(err)
        })?,
        None => Vec::new(),
    };

    let usage = response
        .usage
        .as_ref()
        .map(|usage| CompletionUsageWire {
            prompt_tokens: usage.prompt_tokens.min(i64::MAX as u64) as i64,
            completion_tokens: usage.completion_tokens.min(i64::MAX as u64) as i64,
            total_tokens: usage.total_tokens.min(i64::MAX as u64) as i64,
        })
        .unwrap_or_default();

    Ok(Some(CompletionResponseWire {
        id: response.id.clone(),
        object: response.object.clone(),
        created: response.created,
        model: response.model.clone(),
        choices,
        usage,
    }))
}

/// Convert a provider wire [`CompletionResponseWire`] into the unified
/// [`LlmResponse`]. Mirrors Go `completionResponseToLLM`
/// (`completion_outbound.go:164-196`): lift `choices` onto `completion`, set
/// `request_type`/`api_format`, and attach `usage` only when the provider
/// reported non-zero prompt/total tokens.
fn completion_response_wire_to_llm(
    wire: &CompletionResponseWire,
) -> TransformerResult<LlmResponse> {
    let completion_value = serde_json::to_value(CompletionData {
        choices: wire.choices.clone(),
    })
    .map_err(|err| {
        ConduitError::internal("failed to encode completion choices").with_source(err)
    })?;

    let mut llm = LlmResponse {
        id: wire.id.clone(),
        object: wire.object.clone(),
        created: wire.created,
        model: wire.model.clone(),
        completion: Some(completion_value),
        request_type: Some(RequestType::Completion),
        api_format: Some(ApiFormat::OpenAiCompletions),
        ..LlmResponse::default()
    };

    if wire.usage.prompt_tokens > 0 || wire.usage.total_tokens > 0 {
        llm.usage = Some(usage_from_wire(&wire.usage));
    }

    Ok(llm)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn json_request(body: Value) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: COMPLETIONS_PATH.to_string(),
            content_type: Some("application/json".to_string()),
            json_body: Some(body),
            ..HttpRequest::default()
        }
    }

    // ---- Inbound request parsing (Go completion_inbound.go golden shapes) ----

    #[test]
    fn inbound_request_parses_string_prompt() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAiCompletionInbound::new();
        let request = json_request(json!({
            "model": "gpt-3.5-turbo-instruct",
            "prompt": "Say hello",
            "max_tokens": 16,
            "temperature": 0.5,
            "stream": false,
            "best_of": 2,
            "user": "u-1"
        }));

        let llm = transformer.inbound_request(request)?;

        assert_eq!(llm.request_type, RequestType::Completion);
        assert_eq!(llm.api_format, ApiFormat::OpenAiCompletions);
        assert_eq!(llm.model.as_deref(), Some("gpt-3.5-turbo-instruct"));
        assert!(!llm.stream);

        match &llm.payload {
            LlmRequestPayload::Completion(completion) => {
                assert_eq!(
                    completion.prompt,
                    Some(Value::String("Say hello".to_string()))
                );
                assert_eq!(completion.max_tokens, Some(16));
                assert_eq!(completion.temperature, Some(0.5));
                // `best_of` / `user` are not typed on the unified struct — they
                // round-trip through `extra` (flatten).
                assert_eq!(completion.extra.get("best_of"), Some(&json!(2)));
                assert_eq!(completion.extra.get("user"), Some(&json!("u-1")));
            }
            other => {
                return Err(format!("expected Completion payload, got {other:?}").into());
            }
        }
        Ok(())
    }

    #[test]
    fn inbound_request_parses_array_prompt() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAiCompletionInbound::new();
        let request = json_request(json!({
            "model": "gpt-3.5-turbo-instruct",
            "prompt": ["Say hello", "Say goodbye"]
        }));

        let llm = transformer.inbound_request(request)?;

        match &llm.payload {
            LlmRequestPayload::Completion(completion) => {
                assert_eq!(completion.prompt, Some(json!(["Say hello", "Say goodbye"])));
            }
            other => {
                return Err(format!("expected Completion payload, got {other:?}").into());
            }
        }
        Ok(())
    }

    #[test]
    fn inbound_request_requires_model() {
        let transformer = OpenAiCompletionInbound::new();
        let request = json_request(json!({ "prompt": "hi" }));
        match transformer.inbound_request(request) {
            Ok(_) => panic!("missing model must error"),
            Err(err) => assert_eq!(err.error_type(), "invalid_request"),
        }
    }

    #[test]
    fn inbound_request_requires_non_empty_prompt() {
        let transformer = OpenAiCompletionInbound::new();
        // Missing prompt.
        match transformer.inbound_request(json_request(json!({ "model": "m" }))) {
            Ok(_) => panic!("missing prompt must error"),
            Err(err) => assert_eq!(err.error_type(), "invalid_request"),
        }
        // Empty-string prompt (Go `compReq.Prompt == ""`).
        match transformer.inbound_request(json_request(json!({ "model": "m", "prompt": "" }))) {
            Ok(_) => panic!("empty prompt must error"),
            Err(err) => assert_eq!(err.error_type(), "invalid_request"),
        }
    }

    #[test]
    fn inbound_request_rejects_non_json_content_type() {
        let transformer = OpenAiCompletionInbound::new();
        let mut request = json_request(json!({ "model": "m", "prompt": "hi" }));
        request.content_type = Some("text/plain".to_string());
        match transformer.inbound_request(request) {
            Ok(_) => panic!("non-JSON content type must error"),
            Err(err) => assert_eq!(err.error_type(), "invalid_request"),
        }
    }

    // ---- Inbound response shaping (Go completion_inbound.go TransformResponse) ----

    #[test]
    fn transform_response_shapes_text_completion() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAiCompletionInbound::new();
        let response = LlmResponse {
            id: "cmpl-1".to_string(),
            object: "text_completion".to_string(),
            created: 123,
            model: "gpt-3.5-turbo-instruct".to_string(),
            completion: Some(json!({
                "choices": [
                    { "text": "Hello there", "index": 0, "finish_reason": "stop" }
                ]
            })),
            usage: Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 2,
                total_tokens: 7,
                ..Usage::default()
            }),
            ..LlmResponse::default()
        };

        let http = transformer.transform_response(response)?;
        assert_eq!(http.status, 200);
        assert_eq!(
            http.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );

        let body = http.body.ok_or("missing response body")?;
        let parsed: Value = serde_json::from_slice(&body)?;
        assert_eq!(parsed["object"], json!("text_completion"));
        assert_eq!(parsed["id"], json!("cmpl-1"));
        assert_eq!(parsed["model"], json!("gpt-3.5-turbo-instruct"));
        assert_eq!(parsed["created"], json!(123));
        assert_eq!(parsed["choices"][0]["text"], json!("Hello there"));
        assert_eq!(parsed["choices"][0]["index"], json!(0));
        assert_eq!(parsed["choices"][0]["finish_reason"], json!("stop"));
        assert_eq!(parsed["usage"]["prompt_tokens"], json!(5));
        assert_eq!(parsed["usage"]["completion_tokens"], json!(2));
        assert_eq!(parsed["usage"]["total_tokens"], json!(7));
        Ok(())
    }

    #[test]
    fn transform_response_without_completion_errors() {
        let transformer = OpenAiCompletionInbound::new();
        let response = LlmResponse::default();
        match transformer.transform_response(response) {
            Ok(_) => panic!("response without completion data must error"),
            Err(err) => assert_eq!(err.kind, ErrorKind::Internal),
        }
    }

    // ---- Streaming aggregation (Go AggregateCompletionStreamChunks) ----

    #[test]
    fn aggregate_stream_chunks_concatenates_text() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAiCompletionInbound::new();
        let chunk = |text: &str, finish: Option<&str>| StreamEvent {
            data: Some(
                json!({
                    "id": "cmpl-stream",
                    "object": "text_completion",
                    "created": 42,
                    "model": "m",
                    "choices": [
                        { "text": text, "index": 0, "finish_reason": finish }
                    ],
                    "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }
                })
                .to_string(),
            ),
            ..StreamEvent::default()
        };
        let events = vec![
            chunk("Hello", None),
            chunk(" world", Some("stop")),
            StreamEvent {
                data: Some("[DONE]".to_string()),
                ..StreamEvent::default()
            },
        ];

        let http = transformer.aggregate_stream_chunks(events)?;
        assert_eq!(http.status, 200);
        // Original events preserved losslessly.
        assert_eq!(http.stream.len(), 3);

        let body = http.body.ok_or("missing aggregated body")?;
        let parsed: Value = serde_json::from_slice(&body)?;
        assert_eq!(parsed["object"], json!("text_completion"));
        assert_eq!(parsed["id"], json!("cmpl-stream"));
        assert_eq!(
            parsed["completion"]["choices"][0]["text"],
            json!("Hello world")
        );
        assert_eq!(
            parsed["completion"]["choices"][0]["finish_reason"],
            json!("stop")
        );
        Ok(())
    }

    #[test]
    fn aggregate_stream_chunks_defaults_finish_reason_to_stop()
    -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAiCompletionInbound::new();
        let events = vec![StreamEvent {
            data: Some(
                json!({
                    "id": "c",
                    "object": "text_completion",
                    "created": 1,
                    "model": "m",
                    "choices": [ { "text": "hi", "index": 0 } ],
                    "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
                })
                .to_string(),
            ),
            ..StreamEvent::default()
        }];

        let http = transformer.aggregate_stream_chunks(events)?;
        let body = http.body.ok_or("missing aggregated body")?;
        let parsed: Value = serde_json::from_slice(&body)?;
        assert_eq!(
            parsed["completion"]["choices"][0]["finish_reason"],
            json!("stop")
        );
        // Usage is lifted from the (only) non-zero chunk.
        assert_eq!(parsed["usage"]["total_tokens"], json!(2));
        Ok(())
    }

    // ---- Inbound per-chunk stream shaping (Go transformStreamChunk) ----

    #[test]
    fn transform_stream_shapes_chunks_and_done() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = OpenAiCompletionInbound::new();
        let inputs = vec![
            LlmResponse {
                id: "c".to_string(),
                object: "text_completion".to_string(),
                model: "m".to_string(),
                completion: Some(json!({ "choices": [ { "text": "Hi", "index": 0 } ] })),
                ..LlmResponse::default()
            },
            // No completion sub-body → dropped (Go `return nil, nil`).
            LlmResponse {
                object: "text_completion".to_string(),
                ..LlmResponse::default()
            },
            // `[DONE]` sentinel.
            LlmResponse {
                object: "[DONE]".to_string(),
                ..LlmResponse::default()
            },
        ];

        let events: Vec<StreamEvent> = transformer
            .transform_stream(Box::new(inputs.into_iter()))?
            .collect();

        assert_eq!(events.len(), 2);
        let first = events[0]
            .data
            .as_deref()
            .ok_or("missing first event data")?;
        let parsed: Value = serde_json::from_str(first)?;
        assert_eq!(parsed["object"], json!("text_completion"));
        assert_eq!(parsed["choices"][0]["text"], json!("Hi"));
        assert_eq!(events[1].data.as_deref(), Some("[DONE]"));
        Ok(())
    }

    // ---- Outbound request building (Go CompletionOutboundTransformer.TransformRequest) ----

    #[test]
    fn outbound_request_builds_completions_post() -> Result<(), Box<dyn std::error::Error>> {
        let inbound = OpenAiCompletionInbound::new();
        let llm = inbound.inbound_request(json_request(json!({
            "model": "gpt-3.5-turbo-instruct",
            "prompt": "hi",
            "max_tokens": 8,
            "stream": true
        })))?;

        let outbound = OpenAiCompletionOutbound::new();
        let http = outbound.outbound_request(&llm)?;

        assert_eq!(http.method, "POST");
        assert_eq!(http.path, "/v1/completions");
        assert_eq!(http.request_type, Some(RequestType::Completion));
        assert_eq!(http.api_format, Some(ApiFormat::OpenAiCompletions));

        let body = http.json_body.ok_or("missing outbound body")?;
        assert_eq!(body["model"], json!("gpt-3.5-turbo-instruct"));
        assert_eq!(body["prompt"], json!("hi"));
        assert_eq!(body["max_tokens"], json!(8));
        assert_eq!(body["stream"], json!(true));
        Ok(())
    }

    #[test]
    fn outbound_request_rejects_non_completion_payload() {
        use conduit_llm::ChatRequest;

        let outbound = OpenAiCompletionOutbound::new();
        let request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("m".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };
        match outbound.outbound_request(&request) {
            Ok(_) => panic!("non-completion payload must error"),
            Err(err) => assert_eq!(err.error_type(), "invalid_request"),
        }
    }

    // ---- Outbound response conversion (Go completionResponseToLLM) ----

    #[test]
    fn outbound_transform_response_to_unified() -> Result<(), Box<dyn std::error::Error>> {
        let outbound = OpenAiCompletionOutbound::new();
        let provider = HttpResponse {
            status: 200,
            json_body: Some(json!({
                "id": "cmpl-9",
                "object": "text_completion",
                "created": 10,
                "model": "m",
                "choices": [
                    { "text": "world", "index": 0, "finish_reason": "length" }
                ],
                "usage": { "prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7 }
            })),
            ..HttpResponse::default()
        };

        let llm = outbound.transform_response(provider)?;
        assert_eq!(llm.id, "cmpl-9");
        assert_eq!(llm.object, "text_completion");
        assert_eq!(llm.model, "m");
        assert_eq!(llm.request_type, Some(RequestType::Completion));
        assert_eq!(llm.api_format, Some(ApiFormat::OpenAiCompletions));

        let completion = llm.completion.ok_or("missing completion sub-body")?;
        assert_eq!(completion["choices"][0]["text"], json!("world"));
        assert_eq!(completion["choices"][0]["finish_reason"], json!("length"));

        let usage = llm.usage.ok_or("missing usage")?;
        assert_eq!(usage.prompt_tokens, 3);
        assert_eq!(usage.completion_tokens, 4);
        assert_eq!(usage.total_tokens, 7);
        Ok(())
    }

    #[test]
    fn outbound_transform_response_omits_zero_usage() -> Result<(), Box<dyn std::error::Error>> {
        let outbound = OpenAiCompletionOutbound::new();
        let provider = HttpResponse {
            status: 200,
            json_body: Some(json!({
                "id": "cmpl-0",
                "object": "text_completion",
                "created": 1,
                "model": "m",
                "choices": [ { "text": "x", "index": 0 } ],
                "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }
            })),
            ..HttpResponse::default()
        };

        let llm = outbound.transform_response(provider)?;
        // Go attaches usage only when prompt/total tokens are non-zero.
        assert!(llm.usage.is_none());
        Ok(())
    }

    #[test]
    fn outbound_transform_response_errors_on_provider_failure() {
        let outbound = OpenAiCompletionOutbound::new();
        let provider = HttpResponse {
            status: 429,
            json_body: Some(json!({ "error": { "message": "slow down" } })),
            ..HttpResponse::default()
        };
        match outbound.transform_response(provider) {
            Ok(_) => panic!("4xx provider response must error"),
            Err(err) => assert_eq!(err.provider_status, Some(429)),
        }
    }

    // ---- Round-trip: unified completion → wire → unified ----

    #[test]
    fn completion_wire_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let unified = LlmResponse {
            id: "cmpl-rt".to_string(),
            object: "text_completion".to_string(),
            created: 99,
            model: "m".to_string(),
            completion: Some(json!({
                "choices": [ { "text": "abc", "index": 0, "finish_reason": "stop" } ]
            })),
            ..LlmResponse::default()
        };

        let wire = completion_wire_from_llm(&unified)?.ok_or("expected a wire response")?;
        assert_eq!(wire.id, "cmpl-rt");
        assert_eq!(wire.object, "text_completion");
        assert_eq!(wire.choices.len(), 1);
        assert_eq!(wire.choices[0].text, "abc");
        assert_eq!(wire.choices[0].finish_reason.as_deref(), Some("stop"));

        let back = completion_response_wire_to_llm(&wire)?;
        assert_eq!(back.id, "cmpl-rt");
        let completion = back
            .completion
            .ok_or("missing completion after round-trip")?;
        assert_eq!(completion["choices"][0]["text"], json!("abc"));
        Ok(())
    }
}
