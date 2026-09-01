use std::collections::BTreeSet;
use std::io::{Cursor, Read};

use axum::Json;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use conduit_core::error::{ConduitError, ErrorKind};

/// Extended metadata field names that opt a model list response into the
/// "extended" payload (`convertModelToOpenAIExtended` path in Go). Mirrors the
/// `extendedFields` slice in `parseOpenAIModelInclude` (openai.go:538).
pub const EXTENDED_MODEL_FIELDS: &[&str] = &[
    "name",
    "description",
    "context_length",
    "max_output_tokens",
    "modalities",
    "capabilities",
    "pricing",
    "icon",
    "type",
];

// ---------------------------------------------------------------------------
// Model summary + list response shape (Erdos/Boyle — unchanged)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSummary {
    pub id: String,
    pub owned_by: String,
}

impl ModelSummary {
    pub fn new(id: impl Into<String>, owned_by: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            owned_by: owned_by.into(),
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OpenAiModelListResponse {
    pub object: &'static str,
    pub data: Vec<OpenAiModelObject>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OpenAiModelObject {
    pub object: &'static str,
    pub id: String,
    pub owned_by: String,
}

#[derive(Debug, Serialize)]
pub struct OpenAiErrorEnvelope {
    pub error: OpenAiErrorBody,
}

#[derive(Debug, Serialize)]
pub struct OpenAiErrorBody {
    pub message: &'static str,
    #[serde(rename = "type")]
    pub error_type: &'static str,
    pub code: &'static str,
}

// ---------------------------------------------------------------------------
// RUST-P11-001 — Model list/retrieve handlers (openai.go:671-788)
// ---------------------------------------------------------------------------

/// Service seam standing in for Go `biz.ModelService.ListEnabledModels`
/// (consumed at openai.go:685 + 735). The host wires a concrete
/// implementation backed by `conduit-services`; this crate never depends on
/// the services crate directly so test runs stay cheap. Errors are reported
/// as [`ConduitError`] so the handler renders them through
/// [`conduit_error_response`] in the OpenAI-compatible envelope shape Go
/// produces via `writeOpenAIInternalError` (openai.go:641-651).
#[async_trait::async_trait]
pub trait ModelService: Send + Sync {
    /// List all enabled models visible to the caller. Mirrors Go's
    /// `handlers.ModelService.ListEnabledModels(ctx)` (openai.go:685, 735).
    /// The caller owns filtering by id (RetrieveModel) and the include-based
    /// field selection applied by [`build_openai_models_response`].
    async fn list_enabled_models(&self) -> Result<Vec<ModelRow>, ConduitError>;

    /// List models with customer-facing retail pricing resolved for one
    /// project. Implementations that do not provide commercial pricing keep
    /// the basic model list and leave `retail_pricing` empty.
    async fn list_enabled_models_for_project(
        &self,
        _project_id: Option<i64>,
    ) -> Result<Vec<ModelRow>, ConduitError> {
        self.list_enabled_models().await
    }
}

/// Extract the `include` query parameter value from a URI query string.
///
/// Mirrors Go's `c.Query("include")` consumed at openai.go:683, 731: returns
/// the first `include=...` value, or the empty string when absent.
fn extract_include_query(query: Option<&str>) -> String {
    let query = match query {
        Some(q) => q,
        None => return String::new(),
    };
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=')
            && key == "include"
        {
            return value.to_string();
        }
    }
    String::new()
}

/// GET /v1/models — Go `OpenAIHandlers.ListModels` (openai.go:726-788).
///
/// Mirrors the Go control flow:
/// 1. parse `?include=...` via [`parse_model_include`] (openai.go:731);
/// 2. call [`ModelService::list_enabled_models`] (openai.go:735);
/// 3. on empty result, short-circuit with `{object: "list", data: []}`
///    (openai.go:741-748);
/// 4. otherwise build the response via [`build_openai_models_response`]
///    (openai.go:750-782).
///
/// The Go side consults `SystemService.ModelSettingsOrDefault` for the
/// `defaultIncludeAll` flag (openai.go:731); this bounded-scope handler
/// defaults to `false` and can be upgraded once the system-service bridge is
/// wired.
pub async fn list_models(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
) -> Response {
    let Some(model_service) = state.services().model_service() else {
        let err = ConduitError::internal("model service is not wired");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    };

    // openai.go:731 — parse include.
    let include_param = extract_include_query(uri.query());
    let parsed = parse_model_include(&include_param, false);

    let project_id = api_key_meta
        .as_ref()
        .map(|axum::Extension(meta)| meta.project_id)
        .filter(|project_id| *project_id > 0);
    let mut models = match model_service
        .list_enabled_models_for_project(project_id)
        .await
    {
        Ok(rows) => rows,
        Err(err) => return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson),
    };
    if let Some(axum::Extension(meta)) = api_key_meta {
        let allowed: std::collections::HashSet<&str> = meta
            .allowed_models
            .split(',')
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .collect();
        if !allowed.is_empty() {
            models.retain(|model| allowed.contains(model.id.as_str()));
        }
    }

    // openai.go:741-748 — empty models short-circuit with an empty list envelope.
    if models.is_empty() {
        let response = OpenAiModelsResponse {
            object: "list",
            data: Vec::new(),
        };
        return (StatusCode::OK, Json(response)).into_response();
    }

    // openai.go:784-787 — build and emit the list envelope.
    let response = build_openai_models_response(models, &parsed);
    (StatusCode::OK, Json(response)).into_response()
}

/// GET /v1/models/{model} — Go `OpenAIHandlers.RetrieveModel`
/// (openai.go:673-721).
///
/// Mirrors the Go control flow:
/// 1. trim the leading `/` Gin stamps on the catch-all param
///    (openai.go:677, via [`trim_model_splat`]);
/// 2. parse `?include=...` (openai.go:683);
/// 3. call [`ModelService::list_enabled_models`] (openai.go:685);
/// 4. find the matching model by id (openai.go:691-697); if absent emit
///    OpenAI-compatible `model_not_found` (via [`model_not_found_response`]);
/// 5. emit the single-row entry via [`build_openai_model_entry`]
///    (openai.go:700-720) — a bare `object: "model"` JSON object, NOT wrapped
///    in a list envelope.
pub async fn retrieve_model(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    Path(model_param): Path<String>,
    uri: Uri,
) -> Response {
    let Some(model_service) = state.services().model_service() else {
        let err = ConduitError::internal("model service is not wired");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    };

    // openai.go:677 — strip Gin's catch-all leading slash.
    let model_id = trim_model_splat(&model_param);
    if model_id.is_empty() {
        return model_not_found_response("");
    }

    // openai.go:683 — parse include.
    let include_param = extract_include_query(uri.query());
    let parsed = parse_model_include(&include_param, false);

    let project_id = api_key_meta
        .as_ref()
        .map(|axum::Extension(meta)| meta.project_id)
        .filter(|project_id| *project_id > 0);
    let mut models = match model_service
        .list_enabled_models_for_project(project_id)
        .await
    {
        Ok(rows) => rows,
        Err(err) => return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson),
    };
    if let Some(axum::Extension(meta)) = api_key_meta {
        let allowed: std::collections::HashSet<&str> = meta
            .allowed_models
            .split(',')
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .collect();
        if !allowed.is_empty() {
            models.retain(|model| allowed.contains(model.id.as_str()));
        }
    }

    // openai.go:691-697 — find by id.
    let Some(row) = models.into_iter().find(|row| row.id == model_id) else {
        return model_not_found_response(model_id);
    };

    // openai.go:700-720 — emit bare model object (NOT a list envelope).
    let entry = build_openai_model_entry(&row, &parsed);
    (StatusCode::OK, Json(entry)).into_response()
}

/// Render the OpenAI-compatible `model_not_found` error response.
///
/// Mirrors Go's `writeOpenAIModelNotFoundError` (openai.go:653-669):
/// status 404, `type: "invalid_request_error"`, `code: "model_not_found"`,
/// `param: "model"`, and a user-facing message naming the missing model id.
fn model_not_found_response(model_id: &str) -> Response {
    let message = if model_id.is_empty() {
        "The model does not exist or you do not have access to it.".to_string()
    } else {
        format!("The model `{model_id}` does not exist or you do not have access to it.")
    };
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "invalid_request_error",
            "param": "model",
            "code": "model_not_found",
        }
    });
    (StatusCode::NOT_FOUND, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// RUST-P11-001 MAP-01 — OpenAI main handler chain (chat/responses/embeddings)
// ---------------------------------------------------------------------------
//
// Ports the gin handler bodies in `conduit/internal/server/api/openai.go`
// (lines 292-310) plus the shared `ChatCompletionHandlers.ChatCompletion` +
// `ChatCompletionHandlers.ChatCompletionWithRequest` flow in
// `conduit/internal/server/api/chat.go:49-116`. Bounded scope: the three
// highest-frequency non-stream endpoints (chat completions, responses,
// embeddings); streaming + audio/image/video/files endpoints stay pending
// and are tracked in the gap list of the Faraday-the-16th report.
//
// Each handler is a thin wrapper exactly mirroring the Go control flow:
//
// 1. read the raw HTTP request bytes (S04 — `httpclient.ReadHTTPRequest`,
//    chat.go:53-60);
// 2. validate the body is non-empty (S05 — chat.go:67-70, reused via
//    [`validate_chat_request`]);
// 3. build an [`HttpRequest`] command for the orchestrator (S06 — Go
//    `genericReq` assembly, utils.go:19-48);
// 4. call [`OpenAiOrchestratorService::process`] (S06 —
//    `ChatCompletionOrchestrator.Process`, chat.go:74);
// 5. write the non-stream response (S07 — chat.go:84-95, reusing
//    [`resolve_response_content_type`] for the Content-Type fallback).
//
// The handler **never** resolves api_key/project itself (S17 — that belongs
// to middleware + the orchestrator's RequestContext, mirrors Go
// `contexts.GetProjectID` / `contexts.GetUserID` consumed inside the
// orchestrator). Stream handling (chat.go:97-115) is intentionally omitted
// from this batch (gap).

use conduit_llm::model::HttpRequest;

use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{Method, Uri};

use crate::app_state::AppState;
use crate::error_middleware::{ErrorResponseFormat, conduit_error_response};
use crate::middleware::api_key_auth::{ValidatedApiKeyMetadata, api_key_meta_keys};
use crate::middleware::{TraceThreadContext, TracingHeaderConfig, resolve_trace_id};

/// Maximum aggregate request size for multipart image/audio uploads. This is
/// intentionally larger than axum's 2 MiB default while remaining bounded;
/// the limit covers all parts plus multipart framing, not each file alone.
pub const MULTIPART_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;

/// Axum's existing default for JSON/protocol request bodies. Compressed input
/// is allowed only when its expanded form still fits this same boundary.
const PROTOCOL_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;
/// Bound CPU/memory amplification independently of the absolute route limit.
const MAX_DECOMPRESSION_RATIO: usize = 100;
/// Small compressed payloads need room for codec framing and ordinary JSON.
const DECOMPRESSION_RATIO_SLACK_BYTES: usize = 1024;

/// Non-stream response materialised by [`OpenAiOrchestratorService::process`].
///
/// Mirrors the fields the Go `result.ChatCompletion` branch reads at
/// chat.go:84-95: `StatusCode`, `Headers.Get("Content-Type")`, and `Body`.
/// `content_type` stays `None` when the orchestrator did not stamp a
/// Content-Type header so the handler can fall back to
/// [`DEFAULT_NONSTREAM_CONTENT_TYPE`] via [`resolve_response_content_type`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiHandlerResponse {
    /// HTTP status code Go stamps via `c.Data(resp.StatusCode, ...)`.
    pub status: u16,
    /// `resp.Headers.Get("Content-Type")` — `None` when absent/empty.
    pub content_type: Option<String>,
    /// `resp.Body` bytes — written verbatim by the handler.
    pub body: Vec<u8>,
}

impl OpenAiHandlerResponse {
    /// Build a 200 OK JSON response from already-serialised bytes. Convenience
    /// for in-memory test implementations of the trait.
    pub fn ok_json(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type: Some("application/json".to_string()),
            body,
        }
    }
}

// ---------------------------------------------------------------------------
// RUST-P11-001 streaming branch — OrchestratorOutput + StreamEvent
// ---------------------------------------------------------------------------

/// A single SSE event emitted by the orchestrator on a streaming chat /
/// response / embeddings flow. Mirrors Go `httpclient.StreamEvent`
/// (httpclient/model.go:106-117): `Type` is the SSE event name gin's
/// `c.SSEvent(cur.Type, cur.Data)` stamps as `event:<type>`, and `data` is the
/// JSON-encoded payload written verbatim after `data:`. The Rust side keeps
/// `data` as an owned `String` (not `Vec<u8>`) because all SSE payloads are
/// UTF-8 JSON text — the binary audio path is handled separately via the
/// non-stream `OpenAiHandlerResponse` body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEvent {
    /// SSE event name, e.g. `"chat.completion.chunk"` or `"response.created"`.
    /// Empty string means no `event:` line (Go emits a bare `data:` frame).
    pub event: String,
    /// JSON-encoded payload for the `data:` line.
    pub data: String,
}

impl StreamEvent {
    /// Build a new event. `event` is the SSE event name; `data` is the raw JSON
    /// payload (already serialized).
    pub fn new(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: event.into(),
            data: data.into(),
        }
    }
}

/// The discriminated output of [`OpenAiOrchestratorService::process`].
///
/// Mirrors the two non-nil branches of Go's
/// `orchestrator.ChatCompletionOrchestrator.Process` result struct
/// (chat.go:84-115):
///
/// * [`OpenAiHandlerOutput::NonStream`] — the `result.ChatCompletion` branch:
///   a materialised HTTP response (`c.Data(resp.StatusCode, contentType,
///   resp.Body)` at chat.go:92).
/// * [`OpenAiHandlerOutput::Stream`] — the `result.ChatCompletionStream`
///   branch (chat.go:97-115): a sequence of SSE frames the handler forwards
///   via `WriteSSEStreamWithErrorFormatter`. Each frame is either an event to
///   emit (`Ok`) or a terminal stream error (`Err`) that the handler renders
///   through [`format_stream_error_frame`], exactly mirroring Go's
///   `c.SSEvent("error", formatErr(ctx, stream.Err()))` at chat.go:164.
///
/// The bounded-scope contract materialises the full frame sequence up front
/// (Vec) rather than shipping a live `BoxStream`. The SSE framing logic is
/// identical either way; the host wiring can swap the Vec for a real
/// `futures::Stream` once the orchestrator bridge is wired, without touching
/// the handler-side materialisation code.
#[derive(Debug)]
pub enum OpenAiHandlerOutput {
    /// Non-stream response — Go `result.ChatCompletion` (chat.go:84-95).
    NonStream(OpenAiHandlerResponse),
    /// Streaming response — Go `result.ChatCompletionStream` (chat.go:97-115).
    /// The handler writes one SSE frame per `Ok` event and one
    /// `event:error\ndata:<frame>\n\n` per `Err` terminal error.
    Stream(Vec<Result<StreamEvent, ConduitError>>),
    /// Binary stream response — Go `WriteBinaryStream` (chat.go:175-239). The
    /// orchestrator materialises the full audio body up front (Vec<u8>) and the
    /// handler writes it verbatim with the binary stream header set
    /// ([`binary_stream_headers`]) plus the Content-Type the orchestrator
    /// stamped on the first chunk (surfaced via [`OpenAiHandlerResponse::content_type`]
    /// on the sibling NonStream variant, or defaulted to
    /// [`BINARY_STREAM_DEFAULT_CONTENT_TYPE`] when empty).
    ///
    /// RUST-P11-001 Faraday-the-19th: this variant closes the audio-binary gap
    /// left by Faraday-the-18th — the previous `Stream` variant only carries
    /// SSE string frames (`text/event-stream`), but `/v1/audio/speech` with
    /// `stream_format=audio` returns raw `audio/mpeg` bytes that must be framed
    /// as a binary chunk, not SSE.
    Binary {
        /// Raw response bytes (audio/mpeg, audio/wav, etc.).
        body: Vec<u8>,
        /// Content-Type to stamp on the response. When `None`, the handler
        /// falls back to [`BINARY_STREAM_DEFAULT_CONTENT_TYPE`] — matching
        /// Go's chat.go:181 default.
        content_type: Option<String>,
    },
    /// RUST-P8-003 — live incremental SSE: a client-facing event receiver the
    /// orchestrator's `process_command_stream` produces. The handler forwards
    /// each event as an SSE frame AS IT ARRIVES (via
    /// [`crate::middleware::sse_stream::into_sse_response`]) rather than
    /// collecting the whole stream first (the `Stream` variant's buffered path).
    LiveStream(LiveEventStream),
    /// Live raw-audio response. Each event carries one provider body chunk in
    /// `StreamEvent::binary`; the HTTP writer forwards those chunks without
    /// collecting the complete speech response.
    LiveBinary {
        stream: LiveEventStream,
        content_type: Option<String>,
    },
}

/// Wrapper carrying the live client-facing event receiver so
/// [`OpenAiHandlerOutput`] can keep `#[derive(Debug)]` (`Receiver` is not
/// `Debug`). Carries [`conduit_llm::StreamEvent`] (the pipeline event type).
pub struct LiveEventStream(
    pub tokio::sync::mpsc::Receiver<Result<conduit_llm::StreamEvent, ConduitError>>,
);

impl std::fmt::Debug for LiveEventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LiveEventStream(<receiver>)")
    }
}

impl From<OpenAiHandlerResponse> for OpenAiHandlerOutput {
    fn from(resp: OpenAiHandlerResponse) -> Self {
        Self::NonStream(resp)
    }
}

/// Service seam standing in for Go `*orchestrator.ChatCompletionOrchestrator`
/// (only the `Process(ctx, *httpclient.Request)` member the openai handlers
/// touch — openai.go:292-310 + chat.go:74). The host binary wires a concrete
/// implementation backed by `conduit-orchestrator::CommandOrchestrator`; this
/// crate never depends on the orchestrator crate directly so test runs stay
/// cheap and skeleton builds (no service wired) degrade to the same 500
/// branch Go hits when `Process` returns an internal error.
///
/// Errors are reported as [`ConduitError`] so the handler can render them through
/// [`conduit_error_response`] in the OpenAI-compatible JSON envelope shape Go
/// produces via `transformOrchestratorError` (chat.go:78-81 + error.go).
#[async_trait::async_trait]
pub trait OpenAiOrchestratorService: Send + Sync {
    /// Process a materialised HTTP request and return the non-stream
    /// response (chat.go:74-95). The `route` argument mirrors the Go
    /// handler-dispatch context (each openai sub-handler sets up a distinct
    /// inbound transformer; the Rust side encodes that as the route name the
    /// service uses to pick the same transformer).
    async fn process(
        &self,
        route: OpenAiRoute,
        request: HttpRequest,
    ) -> Result<OpenAiHandlerOutput, ConduitError>;
}

// ---------------------------------------------------------------------------
// RUST-P11-001 S12 — VideoService trait (openai.go:421-468)
//
// Service seam standing in for Go `biz.VideoService`'s `GetTaskByExternalID`
// (openai.go:430, consumed at 421-451) and `DeleteTaskByExternalID`
// (openai.go:462, consumed at 453-468). The host wires the
// `conduit-services::video_service::VideoTaskService` (P7-006 S08/S12 port)
// behind this trait; conduit-http never depends on the services crate
// directly so test runs stay cheap. Errors are reported as [`ConduitError`] so
// the handler renders them through [`conduit_error_response`] in the OpenAI-
// compatible JSON envelope shape Go produces via `JSONError`.
//
// Bounded scope: the trait returns the wire-ready [`OpenAiHandlerResponse`]
// (status + content-type + body bytes). The Go host runs the response through
// `VideoInboundTransformer.TransformResponse` before emitting; the Rust host
// bridge is responsible for the equivalent transformation, and the http crate
// only forwards bytes. This keeps the parity contract localised in the host
// wiring and avoids dragging transformer dependencies into the http crate.
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait VideoService: Send + Sync {
    /// Mirrors Go `VideoService.GetTaskByExternalID` (openai.go:430).
    ///
    /// Returns the wire-ready HTTP response the host produced by running the
    /// provider-side GetTask payload through the video inbound transformer
    /// (Go: `VideoInboundTransformer.TransformResponse`, openai.go:440-444).
    ///
    /// `project_id` is the project the calling API key belongs to (Go
    /// `apiKey.ProjectID`). The host implementation MUST scope the lookup to it
    /// so an external task id from another project resolves to "not found"
    /// (P-23 — closes the cross-project IDOR the raw-SQL bridge had).
    async fn get_task_by_external_id(
        &self,
        project_id: i64,
        external_id: &str,
    ) -> Result<OpenAiHandlerResponse, ConduitError>;

    /// Mirrors Go `VideoService.DeleteTaskByExternalID` (openai.go:462).
    ///
    /// Go deletes the provider task FIRST and then best-effort cancels the
    /// local request row (biz/video.go:95-115 — S12 ordered-delete parity
    /// lives behind the host wiring). The Rust trait surfaces only the final
    /// outcome: `Ok(())` -> 204 No Content; `Err(_)` -> 500 JSON envelope
    /// (matching Go's `JSONError(c, http.StatusInternalServerError, err)` at
    /// openai.go:463-465).
    ///
    /// `project_id` scopes the cancel to the caller's project (P-23).
    async fn delete_task_by_external_id(
        &self,
        project_id: i64,
        external_id: &str,
    ) -> Result<(), ConduitError>;
}

/// Canonical route discriminator consumed by [`OpenAiOrchestratorService`].
///
/// Mirrors the per-endpoint inbound-transformer dispatch Go wires in
/// `NewOpenAIHandlers` (openai.go:73-289): each route (chat/completions,
/// responses, embeddings, ...) carries its own
/// `openai.NewInboundTransformer` / `responses.NewInboundTransformer` /
/// `openai.NewEmbeddingInboundTransformer` instance. The Rust host picks the
/// same transformer from this tag; the handler itself stays route-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiRoute {
    /// POST /v1/chat/completions — `ChatCompletionHandlers` (openai.go:78-94).
    ChatCompletions,
    /// POST /v1/responses — `ResponseCompletionHandlers` (openai.go:112-128).
    Responses,
    /// POST /v1/embeddings — `EmbeddingHandlers` (openai.go:146-162).
    Embeddings,
    /// POST /v1/audio/speech — `SpeechHandlers` (openai.go:96-110). The
    /// handler consults [`should_use_binary_speech_stream`] before dispatch so
    /// the orchestrator-side service can select the binary vs SSE writer.
    AudioSpeech,
    /// POST /v1/videos — `VideoHandlers` (openai.go:73-94 +
    /// openai.go:384-419). Routed through the same orchestrator dispatch as
    /// the other OpenAI main-chain endpoints; the route tag selects the
    /// video-flavoured inbound transformer at the host side.
    Videos,
    /// POST /v1/messages — Anthropic Messages API. Uses the Anthropic inbound
    /// transformer via the bridge's route match (Go: anthropic.go:50).
    AnthropicMessages,
    /// POST /v1/messages/count_tokens — exact native count where supported,
    /// with provider-reported prompt usage as the compatibility fallback.
    AnthropicCountTokens,
    /// Gemini generateContent / streamGenerateContent — Go `GeminiHandlers`
    /// (gemini.go:66-74). Route through the bridge with a Gemini inbound
    /// transformer.
    GeminiGenerateContent,
    /// POST /v1/images/generations — `ImageGenerationHandlers`
    /// (openai.go:372-374). Thin wrapper over the same `ChatCompletion`
    /// dispatch (chat.go:49-116) with the image-generation-flavoured inbound
    /// transformer. Body is JSON (openai.go:372), not multipart.
    ImageGenerations,
    /// POST /v1/images/edits — `ImageEditHandlers` (openai.go:376-378). Body
    /// is `multipart/form-data` (image + mask + prompt + ...). Mirrors Go:
    /// the gin handler delegates straight to `ChatCompletion`
    /// (chat.go:49-116) which runs the raw body bytes through
    /// `httpclient.ReadHTTPRequest` (utils.go:33) — multipart is parsed
    /// downstream inside the inbound transformer, NOT at the http layer.
    ImageEdits,
    /// POST /v1/audio/transcriptions — `TranscriptionHandlers`
    /// (openai.go:362-365). Multipart audio upload; same thin-wrapper
    /// dispatch as the other openai sub-handlers.
    AudioTranscriptions,
    /// POST /v1/audio/translations — `TranslationHandlers`
    /// (openai.go:367-370). Multipart audio upload; same thin-wrapper
    /// dispatch as the other openai sub-handlers.
    AudioTranslations,
    /// POST /v1/completions — Go `OpenAIHandlers.Completion`
    /// (openai.go:299-301, routes.go:171) → legacy text-completions via the
    /// completion-flavoured inbound transformer.
    Completions,
    /// POST /v1/responses/compact — Go `OpenAIHandlers.CompactResponse`
    /// (openai.go:304-306, routes.go:172) → the compact flavour of the
    /// responses inbound transformer (forces `RequestType::Compact`).
    ResponsesCompact,
    /// POST /v1/rerank + /jina/v1/rerank — Go `JinaHandlers.Rerank`
    /// (routes.go:192/198) → the Jina rerank inbound transformer.
    JinaRerank,
    /// POST /jina/v1/embeddings — Go `JinaHandlers.CreateEmbedding`
    /// (routes.go:197) → the Jina embedding inbound transformer.
    JinaEmbeddings,
    /// POST /doubao/v3/contents/generations/tasks — Go `DoubaoHandlers.CreateTask`
    /// (routes.go:209) → the Doubao (Seedance) video-task inbound transformer.
    /// GetTask/DeleteTask reuse the shared video `get`/`delete` handlers.
    DoubaoCreateTask,
}

impl OpenAiRoute {
    /// The Go-canonical request path the route is mounted under. Used only
    /// to populate [`HttpRequest::path`] when the handler builds the command.
    pub const fn path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/v1/chat/completions",
            Self::Responses => "/v1/responses",
            Self::Embeddings => "/v1/embeddings",
            Self::AudioSpeech => "/v1/audio/speech",
            Self::Videos => "/v1/videos",
            Self::ImageGenerations => "/v1/images/generations",
            Self::ImageEdits => "/v1/images/edits",
            Self::AudioTranscriptions => "/v1/audio/transcriptions",
            Self::AudioTranslations => "/v1/audio/translations",
            Self::AnthropicMessages => "/v1/messages",
            Self::AnthropicCountTokens => "/v1/messages/count_tokens",
            Self::GeminiGenerateContent => "/gemini/v1beta/models",
            Self::Completions => "/v1/completions",
            Self::ResponsesCompact => "/v1/responses/compact",
            Self::JinaRerank => "/v1/rerank",
            Self::JinaEmbeddings => "/jina/v1/embeddings",
            Self::DoubaoCreateTask => "/doubao/v3/contents/generations/tasks",
        }
    }

    /// The error envelope this route's clients expect.
    ///
    /// Go renders every orchestrator error through the *route's own inbound
    /// transformer* — `handlers.ChatCompletionOrchestrator.Inbound.TransformError`
    /// (`api/chat.go:55`, `api/upstream_error_policy.go:23`) — so an Anthropic
    /// client receives Anthropic's `{type, error{...}, request_id}` envelope and a
    /// Gemini client receives `{error{code,message,status}}`. The Rust handlers
    /// previously hardcoded the OpenAI envelope on every path, so Claude/Gemini
    /// clients got a shape their SDKs cannot parse.
    ///
    /// Every other route is an OpenAI-compatible surface (including the Jina and
    /// Doubao ones — Go's Doubao video inbound explicitly reuses the OpenAI error
    /// envelope, `video_inbound.go:225-228`).
    pub const fn error_format(self) -> ErrorResponseFormat {
        match self {
            Self::AnthropicMessages | Self::AnthropicCountTokens => {
                ErrorResponseFormat::AnthropicJson
            }
            Self::GeminiGenerateContent => ErrorResponseFormat::GeminiJson,
            _ => ErrorResponseFormat::OpenAiCompatibleJson,
        }
    }

    const fn request_body_limit(self) -> usize {
        match self {
            Self::ImageEdits | Self::AudioTranscriptions | Self::AudioTranslations => {
                MULTIPART_BODY_LIMIT_BYTES
            }
            _ => PROTOCOL_BODY_LIMIT_BYTES,
        }
    }
}

/// POST /v1/audio/speech — Go `OpenAIHandlers.CreateSpeech`
/// (openai.go:313-336). Mirrors the bounded-scope control flow:
///
/// 1. validate body non-empty (chat.go:67-70, reused via
///    [`validate_chat_request`]);
/// 2. consult [`should_use_binary_speech_stream`] (openai.go:323-328) to
///    decide whether the response should be routed through the binary stream
///    writer (chat.go:175-239) or the regular ChatCompletion flow
///    (chat.go:49-116);
/// 3. dispatch through [`OpenAiOrchestratorService::process`] with the
///    [`OpenAiRoute::AudioSpeech`] tag, stamping the binary/sse decision on
///    `request.metadata` under `audio_stream_mode` so the host-side service
///    can pick the correct outbound writer — mirroring how Go swaps
///    `handlers.SpeechHandlers.StreamWriter` between `WriteBinaryStream` and
///    the default SSE writer at openai.go:330-335.
///
/// Binary responses stay live across the host bridge: provider body chunks
/// arrive as [`OpenAiHandlerOutput::LiveBinary`] and are written verbatim via
/// an Axum streaming body. The provider Content-Type is preserved, with
/// `application/octet-stream` used only when the upstream omitted it.
pub async fn create_speech(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (headers, body) = match decode_request_body(
        &headers,
        body,
        OpenAiRoute::AudioSpeech.request_body_limit(),
    )
    .await
    {
        Ok(decoded) => decoded,
        Err(error) => return request_decode_error_response(error, OpenAiRoute::AudioSpeech),
    };

    // chat.go:67-70 — empty body rejected before the orchestrator is called.
    if let Err(err) = validate_chat_request(&body) {
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    }

    // openai.go:323-328 — decide binary vs SSE writer.
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let use_binary_stream =
        match should_use_binary_speech_stream(Some(&body), content_type.as_deref()) {
            Ok(flag) => flag,
            Err(err) => {
                // openai.go:325-327: Go runs the error through the inbound
                // transformer's TransformError before responding. The Rust side
                // renders it through the OpenAI-compatible error envelope, which
                // carries the same status/kind semantics.
                return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
            }
        };

    let Some(service) = state.services().openai_orchestrator_service() else {
        let err = ConduitError::internal("openai orchestrator service is not wired");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    };

    let mut request = build_openai_http_request(
        OpenAiRoute::AudioSpeech,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
        &tracing_header_config(&state),
    );
    // Stamp the binary/SSE decision so the host-side service can select the
    // outbound writer (openai.go:330-335).
    request.metadata.insert(
        "audio_stream_mode".to_string(),
        if use_binary_stream {
            serde_json::Value::from("binary")
        } else {
            serde_json::Value::from("sse")
        },
    );

    match service.process(OpenAiRoute::AudioSpeech, request).await {
        Ok(output) => materialise_openai_output(OpenAiRoute::AudioSpeech, &uri, output),
        Err(err) => conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson),
    }
}

// ---------------------------------------------------------------------------
// RUST-P11-001 S12 — video endpoints (openai.go:384-468)
// ---------------------------------------------------------------------------

/// POST /v1/videos — Go `OpenAIHandlers.CreateVideo` (openai.go:384-419).
///
/// Mirrors the Go control flow:
/// 1. validate body non-empty (chat.go:67-70 reused via [`validate_chat_request`]
///    — the Go path runs through `httpclient.ReadHTTPRequest` first, which
///    surfaces an empty body as the same "Request body is empty" error);
/// 2. dispatch through [`OpenAiOrchestratorService::process`] with the
///    [`OpenAiRoute::Videos`] tag (openai.go:399 — `VideoHandlers` carries
///    its own `ChatCompletionOrchestrator`, conceptually identical to the
///    chat/embeddings dispatch);
/// 3. on `Ok`, materialise the result via [`materialise_openai_output`]
///    (handles NonStream/Stream/Binary);
/// 4. on `Err`, render the OpenAI-compatible error envelope (chat.go:78-81).
///
/// The Go handler's `result.ChatCompletion == nil` guard (openai.go:408-411)
/// is the host-side service's responsibility: the bounded-scope contract
/// surfaces a missing response as an [`ConduitError::internal`] on the
/// orchestrator side, which renders as the same `internal_error` envelope Go
/// emits via `biz.ErrInternal`.
pub async fn create_video(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (headers, body) =
        match decode_request_body(&headers, body, OpenAiRoute::Videos.request_body_limit()).await {
            Ok(decoded) => decoded,
            Err(error) => return request_decode_error_response(error, OpenAiRoute::Videos),
        };

    // chat.go:67-70 — empty body rejected before the orchestrator is called.
    if let Err(err) = validate_chat_request(&body) {
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    }

    let Some(service) = state.services().openai_orchestrator_service() else {
        let err = ConduitError::internal("openai orchestrator service is not wired");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    };

    let request = build_openai_http_request(
        OpenAiRoute::Videos,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
        &tracing_header_config(&state),
    );

    match service.process(OpenAiRoute::Videos, request).await {
        Ok(output) => materialise_openai_output(OpenAiRoute::Videos, &uri, output),
        Err(err) => conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson),
    }
}

/// GET /v1/videos/{id} — Go `OpenAIHandlers.GetVideo` (openai.go:421-451).
///
/// Mirrors the Go control flow:
/// 1. parse the `{id}` path param (openai.go:424 — `c.Param("id")`); an empty
///    id is rejected as a 400 invalid_request (openai.go:425-427);
/// 2. delegate to [`VideoService::get_task_by_external_id`] (openai.go:430);
/// 3. on `Ok`, write the body verbatim with the orchestrator-stamped
///    Content-Type (openai.go:446-450 — same `c.Data(...)` shape as the non-
///    stream chat branch);
/// 4. on `Err`, render the OpenAI-compatible error envelope (openai.go:431-434
///    — `JSONError(c, http.StatusInternalServerError, err)`).
pub async fn get_video(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    Path(id): Path<String>,
) -> Response {
    if id.trim().is_empty() {
        // openai.go:425-427: empty id -> 400 invalid_request.
        let err = ConduitError::invalid_request("invalid id");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    }

    // P-23: the route sits behind `api_key_auth`, which stamps the validated
    // metadata (incl. the key's `project_id`). Missing metadata means the guard
    // was bypassed / mis-wired — fail closed rather than query globally.
    let Some(axum::Extension(meta)) = api_key_meta else {
        let err = ConduitError::unauthorized("Invalid API key");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    };

    let Some(video_service) = state.services().video_service() else {
        let err = ConduitError::internal("video service is not wired");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    };

    match video_service
        .get_task_by_external_id(meta.project_id, &id)
        .await
    {
        Ok(resp) => materialise_openai_response(resp),
        Err(err) => conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson),
    }
}

/// DELETE /v1/videos/{id} — Go `OpenAIHandlers.DeleteVideo`
/// (openai.go:453-468).
///
/// Mirrors the Go control flow:
/// 1. parse the `{id}` path param (openai.go:456); empty -> 400
///    (openai.go:457-459);
/// 2. delegate to [`VideoService::delete_task_by_external_id`]
///    (openai.go:462). The host-side bridge runs the S12 ordered-delete flow
///    (provider delete first, then best-effort local cancel) — Go's
///    `DeleteTaskByExternalID` (biz/video.go:77-93) composes the same
///    sequence;
/// 3. on `Ok`, emit 204 No Content with an empty body (openai.go:467 —
///    `c.Status(http.StatusNoContent)`);
/// 4. on `Err`, render the OpenAI-compatible error envelope (openai.go:463-465
///    — `JSONError(c, http.StatusInternalServerError, err)`).
pub async fn delete_video(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    Path(id): Path<String>,
) -> Response {
    if id.trim().is_empty() {
        let err = ConduitError::invalid_request("invalid id");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    }

    // P-23: scope the cancel to the calling key's project (see `get_video`).
    let Some(axum::Extension(meta)) = api_key_meta else {
        let err = ConduitError::unauthorized("Invalid API key");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    };

    let Some(video_service) = state.services().video_service() else {
        let err = ConduitError::internal("video service is not wired");
        return conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
    };

    match video_service
        .delete_task_by_external_id(meta.project_id, &id)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson),
    }
}

/// POST /v1/chat/completions — Go `OpenAIHandlers.ChatCompletion`
/// (openai.go:292-294) → `ChatCompletionHandlers.ChatCompletion`
/// (chat.go:49-116).
pub async fn create_chat_completion(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch_openai(
        &state,
        OpenAiRoute::ChatCompletions,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /v1/responses — Go `OpenAIHandlers.CreateResponse` (openai.go:300-302)
/// → `ResponseCompletionHandlers.ChatCompletion` (chat.go:49-116 with the
/// `responses.NewInboundTransformer` flavour, openai.go:112-128).
pub async fn create_response(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch_openai(
        &state,
        OpenAiRoute::Responses,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /v1/completions — Go `OpenAIHandlers.Completion` (openai.go:299-301)
/// → `CompletionHandlers.ChatCompletion` (chat.go:49-116 with the legacy
/// completion-flavoured inbound transformer). Thin wrapper over
/// [`dispatch_openai`] with the [`OpenAiRoute::Completions`] tag.
pub async fn create_completion(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch_openai(
        &state,
        OpenAiRoute::Completions,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /v1/responses/compact — Go `OpenAIHandlers.CompactResponse`
/// (openai.go:304-306) → `CompactHandlers.ChatCompletion` (chat.go:49-116 with
/// the compact responses inbound transformer). Thin wrapper over
/// [`dispatch_openai`] with the [`OpenAiRoute::ResponsesCompact`] tag.
pub async fn create_compact_response(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch_openai(
        &state,
        OpenAiRoute::ResponsesCompact,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /v1/rerank + /jina/v1/rerank — Go `JinaHandlers.Rerank`. Thin wrapper
/// over [`dispatch_openai`] with the [`OpenAiRoute::JinaRerank`] tag.
pub async fn create_jina_rerank(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch_openai(
        &state,
        OpenAiRoute::JinaRerank,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /jina/v1/embeddings — Go `JinaHandlers.CreateEmbedding`. Thin wrapper
/// over [`dispatch_openai`] with the [`OpenAiRoute::JinaEmbeddings`] tag.
pub async fn create_jina_embedding(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch_openai(
        &state,
        OpenAiRoute::JinaEmbeddings,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /doubao/v3/contents/generations/tasks — Go `DoubaoHandlers.CreateTask`.
/// Thin wrapper over [`dispatch_openai`] with the
/// [`OpenAiRoute::DoubaoCreateTask`] tag (the Doubao/Seedance video-task inbound
/// transformer runs on the host side).
pub async fn create_doubao_task(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch_openai(
        &state,
        OpenAiRoute::DoubaoCreateTask,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /v1/embeddings — Go `OpenAIHandlers.CreateEmbedding`
/// (openai.go:308-310) → `EmbeddingHandlers.ChatCompletion` (chat.go:49-116
/// with the `openai.NewEmbeddingInboundTransformer` flavour,
/// openai.go:146-162).
pub async fn create_embedding(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch_openai(
        &state,
        OpenAiRoute::Embeddings,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

// ---------------------------------------------------------------------------
// RUST-P11-001 S16 — multipart image/audio endpoints (openai.go:362-378)
// ---------------------------------------------------------------------------
//
// Ports the four remaining OpenAI sub-handlers that delegate to
// `ChatCompletion` (chat.go:49-116):
//
// * `CreateImage`             (openai.go:372-374) — JSON body
// * `CreateImageEdit`         (openai.go:376-378) — multipart/form-data
// * `CreateTranscription`     (openai.go:362-365) — multipart/form-data
// * `CreateTranslation`       (openai.go:367-370) — multipart/form-data
//
// Mirroring Go's contract: the gin handlers do NOT parse multipart at the
// http layer. They call `ChatCompletionHandlers.ChatCompletion(c)`
// (chat.go:49) which runs the raw body bytes through
// `httpclient.ReadHTTPRequest` (utils.go:33 — `io.ReadAll(rawReq.Body)`).
// Multipart parsing happens downstream inside the per-route inbound
// transformer (e.g. `openai.NewImageEditInboundTransformer`), which is
// host-side wiring the http crate never touches directly. The Rust handler
// therefore forwards the raw `Bytes` axum handed us through the same
// `dispatch_openai` flow as the other OpenAI main-chain endpoints; the
// route tag selects the right inbound transformer on the host side.

/// POST /v1/images/generations — Go `OpenAIHandlers.CreateImage`
/// (openai.go:372-374). JSON body (NOT multipart). Thin wrapper over
/// [`dispatch_openai`] with the [`OpenAiRoute::ImageGenerations`] tag.
pub async fn create_image(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch_openai(
        &state,
        OpenAiRoute::ImageGenerations,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /v1/images/edits — Go `OpenAIHandlers.CreateImageEdit`
/// (openai.go:376-378). Multipart/form-data body (image + mask + prompt +
/// ...). The handler forwards raw bytes — multipart parsing is the inbound
/// transformer's responsibility (chat.go:53-60 + utils.go:33).
pub async fn create_image_edit(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match multipart_body(body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    dispatch_openai(
        &state,
        OpenAiRoute::ImageEdits,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /v1/audio/transcriptions — Go `OpenAIHandlers.CreateTranscription`
/// (openai.go:362-365). Multipart/form-data body (audio file + model).
/// Forwards raw bytes through [`dispatch_openai`] with the
/// [`OpenAiRoute::AudioTranscriptions`] tag.
pub async fn create_transcription(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match multipart_body(body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    dispatch_openai(
        &state,
        OpenAiRoute::AudioTranscriptions,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// POST /v1/audio/translations — Go `OpenAIHandlers.CreateTranslation`
/// (openai.go:367-370). Multipart/form-data body. Forwards raw bytes
/// through [`dispatch_openai`] with the [`OpenAiRoute::AudioTranslations`]
/// tag.
pub async fn create_translation(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match multipart_body(body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    dispatch_openai(
        &state,
        OpenAiRoute::AudioTranslations,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

fn multipart_body(body: Result<Bytes, BytesRejection>) -> Result<Bytes, Response> {
    body.map_err(|rejection| {
        let status = rejection.into_response().status();
        let message = if status == StatusCode::PAYLOAD_TOO_LARGE {
            format!(
                "request body exceeds the {} byte upload limit",
                MULTIPART_BODY_LIMIT_BYTES
            )
        } else {
            "request body could not be read".to_string()
        };
        let error = ConduitError::invalid_request(message).with_http_status(status.as_u16());
        conduit_error_response(&error, ErrorResponseFormat::OpenAiCompatibleJson)
    })
}

#[derive(Debug)]
enum RequestBodyDecodeError {
    UnsupportedEncoding,
    MultipleEncodings,
    InvalidEncodingHeader,
    InvalidCompressedBody(String),
    LimitExceeded,
    WorkerFailure,
}

/// Decode a single HTTP Content-Encoding while bounding both expanded bytes
/// and the compressed-to-expanded ratio. Returning sanitized headers is part
/// of the contract: downstream protocol transformers and upstream providers
/// must never see stale Content-Encoding/Content-Length values for a body that
/// has already been decoded.
async fn decode_request_body(
    headers: &HeaderMap,
    body: Bytes,
    absolute_limit: usize,
) -> Result<(HeaderMap, Bytes), RequestBodyDecodeError> {
    let values = headers
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .map(|value| {
            value
                .to_str()
                .map(str::trim)
                .map(str::to_ascii_lowercase)
                .map_err(|_| RequestBodyDecodeError::InvalidEncodingHeader)
        })
        .collect::<Result<Vec<_>, _>>()?;

    if values.is_empty() {
        return Ok((headers.clone(), body));
    }
    if values.len() != 1 || values[0].contains(',') {
        return Err(RequestBodyDecodeError::MultipleEncodings);
    }

    let encoding = values[0].as_str();
    if encoding == "identity" {
        let mut sanitized = headers.clone();
        sanitized.remove(header::CONTENT_ENCODING);
        return Ok((sanitized, body));
    }
    if !matches!(encoding, "gzip" | "deflate" | "zstd") {
        return Err(RequestBodyDecodeError::UnsupportedEncoding);
    }
    if body.len() > absolute_limit {
        return Err(RequestBodyDecodeError::LimitExceeded);
    }

    let expansion_limit = body
        .len()
        .saturating_mul(MAX_DECOMPRESSION_RATIO)
        .saturating_add(DECOMPRESSION_RATIO_SLACK_BYTES)
        .min(absolute_limit);
    let owned_encoding = encoding.to_string();
    let decoded = tokio::task::spawn_blocking(move || {
        decode_request_body_sync(&owned_encoding, body, expansion_limit)
    })
    .await
    .map_err(|_| RequestBodyDecodeError::WorkerFailure)??;

    let mut sanitized = headers.clone();
    sanitized.remove(header::CONTENT_ENCODING);
    sanitized.remove(header::CONTENT_LENGTH);
    Ok((sanitized, Bytes::from(decoded)))
}

fn decode_request_body_sync(
    encoding: &str,
    body: Bytes,
    limit: usize,
) -> Result<Vec<u8>, RequestBodyDecodeError> {
    match encoding {
        "gzip" => read_decoded_body(flate2::read::GzDecoder::new(Cursor::new(body)), limit),
        // RFC 9110's `deflate` coding is a zlib-wrapped deflate stream.
        "deflate" => read_decoded_body(flate2::read::ZlibDecoder::new(Cursor::new(body)), limit),
        "zstd" => {
            let mut decoder =
                zstd::stream::read::Decoder::new(Cursor::new(body)).map_err(|error| {
                    RequestBodyDecodeError::InvalidCompressedBody(error.to_string())
                })?;
            // Refuse frames asking the decoder to reserve an excessive window
            // before any expanded bytes reach the regular size checks.
            decoder.window_log_max(23).map_err(|error| {
                RequestBodyDecodeError::InvalidCompressedBody(error.to_string())
            })?;
            read_decoded_body(decoder, limit)
        }
        _ => Err(RequestBodyDecodeError::UnsupportedEncoding),
    }
}

fn read_decoded_body(
    mut reader: impl Read,
    limit: usize,
) -> Result<Vec<u8>, RequestBodyDecodeError> {
    let mut decoded = Vec::with_capacity(limit.min(16 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| RequestBodyDecodeError::InvalidCompressedBody(error.to_string()))?;
        if read == 0 {
            return Ok(decoded);
        }
        if decoded.len().saturating_add(read) > limit {
            return Err(RequestBodyDecodeError::LimitExceeded);
        }
        decoded.extend_from_slice(&chunk[..read]);
    }
}

fn request_decode_error_response(error: RequestBodyDecodeError, route: OpenAiRoute) -> Response {
    let (status, message) = match &error {
        RequestBodyDecodeError::UnsupportedEncoding => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported Content-Encoding; expected gzip, deflate, zstd, or identity",
        ),
        RequestBodyDecodeError::MultipleEncodings => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "multiple Content-Encoding values are not supported",
        ),
        RequestBodyDecodeError::InvalidEncodingHeader => {
            (StatusCode::BAD_REQUEST, "invalid Content-Encoding header")
        }
        RequestBodyDecodeError::InvalidCompressedBody(detail) => {
            tracing::debug!(%detail, "request body decompression failed");
            (StatusCode::BAD_REQUEST, "invalid compressed request body")
        }
        RequestBodyDecodeError::LimitExceeded => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "decompressed request body exceeds the size or expansion-ratio limit",
        ),
        RequestBodyDecodeError::WorkerFailure => {
            tracing::error!("request decompression worker terminated unexpectedly");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "request body decompression failed",
            )
        }
    };
    let conduit_error = if status == StatusCode::INTERNAL_SERVER_ERROR {
        ConduitError::internal(message)
    } else {
        ConduitError::invalid_request(message).with_http_status(status.as_u16())
    };
    conduit_error_response(&conduit_error, route.error_format())
}

/// Shared dispatch flow for the three openai main-chain endpoints
/// (chat/responses/embeddings). Mirrors the Go control flow
/// `ChatCompletion -> ChatCompletionWithRequest` (chat.go:49-116) line for
/// line:
///
/// 1. build the [`HttpRequest`] command (S04 — utils.go:19-48);
/// 2. validate non-empty body via [`validate_chat_request`] (S05 —
///    chat.go:67-70);
/// 3. delegate to [`OpenAiOrchestratorService::process`] (S06 — chat.go:74);
/// 4. materialise the non-stream response with the Go Content-Type fallback
///    (S07 — chat.go:84-95, via [`resolve_response_content_type`]).
///
/// Body decompression (Content-Encoding: gzip/deflate/zstd — utils.go:50-108)
/// happens before body validation and transformer dispatch. The decoded size
/// and expansion ratio are bounded, and stale encoding/length headers are
/// removed before the normalized request enters the orchestrator.
/// Shared dispatcher for all LLM proxy routes (OpenAI + Anthropic). Public
/// within the crate so `anthropic_handlers::create_message` can reuse the
/// exact same orchestrator dispatch with a different route variant.
pub(crate) async fn dispatch_openai(
    state: &AppState,
    route: OpenAiRoute,
    uri: &Uri,
    method: Method,
    headers: &HeaderMap,
    body: Bytes,
    api_key_meta: Option<&ValidatedApiKeyMetadata>,
) -> Response {
    // Errors are rendered in the *inbound protocol's* envelope, not always
    // OpenAI's. Go reaches this via `orch.Inbound.TransformError(ctx, err)`
    // (`api/upstream_error_policy.go:23`, `api/chat.go:55`) — the inbound
    // transformer selected by the route owns the error shape, so an Anthropic
    // client gets `{type, error{...}, request_id}` and a Gemini client gets
    // `{error{code,message,status}}`. Previously every route emitted the OpenAI
    // envelope, so Claude/Gemini SDKs could not parse gateway-side failures.
    let (headers, body) = match decode_request_body(headers, body, route.request_body_limit()).await
    {
        Ok(decoded) => decoded,
        Err(error) => return request_decode_error_response(error, route),
    };

    // chat.go:67-70 — empty body rejected before the orchestrator is called.
    if let Err(err) = validate_chat_request(&body) {
        return conduit_error_response(&err, route.error_format());
    }

    let Some(service) = state.services().openai_orchestrator_service() else {
        // Rust-only skeleton path: no orchestrator wired degrades to the same
        // internal-error branch Go hits when `Process` returns a non-nil error
        // (chat.go:75-81). fx guarantees injection on the Go side; the host
        // binary is responsible for the equivalent wiring.
        let err = ConduitError::internal("openai orchestrator service is not wired");
        return conduit_error_response(&err, route.error_format());
    };

    // utils.go:19-48 — materialise the generic HTTP request command. Only the
    // fields the orchestrator pipeline reads directly are populated; the host
    // bridge is responsible for stamping `auth`, `request_id`, `client_ip`,
    // and request-context metadata the same way Go's middleware stack does
    // before the handler is entered (S17 — these live on RequestContext, not
    // the handler).
    let request = build_openai_http_request(
        route,
        uri,
        method,
        &headers,
        body,
        api_key_meta,
        &tracing_header_config(state),
    );

    match service.process(route, request).await {
        Ok(output) => materialise_openai_output(route, uri, output),
        Err(err) => conduit_error_response(&err, route.error_format()),
    }
}

/// Translate an [`OpenAiHandlerOutput`] into an axum [`Response`].
///
/// Mirrors the two-branch dispatch in Go's `ChatCompletionWithRequest`
/// (chat.go:84-115): the non-stream branch calls `c.Data(...)` via
/// [`materialise_openai_response`]; the stream branch writes SSE headers
/// (`Access-Control-Allow-Origin: *` per chat.go:107) and then iterates the
/// frame sequence via [`write_sse_stream_body`].
fn materialise_openai_output(
    route: OpenAiRoute,
    uri: &Uri,
    output: OpenAiHandlerOutput,
) -> Response {
    let gemini_json_stream = route == OpenAiRoute::GeminiGenerateContent
        && !uri
            .query()
            .map(urlencoding_decode_query)
            .unwrap_or_default()
            .iter()
            .any(|(key, value)| key == "alt" && value.eq_ignore_ascii_case("sse"));
    match output {
        OpenAiHandlerOutput::NonStream(resp) => materialise_openai_response(resp),
        OpenAiHandlerOutput::Stream(events) if gemini_json_stream => {
            write_gemini_json_stream_response(events)
        }
        OpenAiHandlerOutput::Stream(events) => write_sse_stream_response(route, events),
        OpenAiHandlerOutput::Binary { body, content_type } => {
            materialise_binary_stream_response(body, content_type)
        }
        OpenAiHandlerOutput::LiveStream(LiveEventStream(rx)) if gemini_json_stream => {
            live_gemini_json_stream_response(rx)
        }
        OpenAiHandlerOutput::LiveStream(LiveEventStream(rx)) => live_sse_response(route, rx),
        OpenAiHandlerOutput::LiveBinary {
            stream: LiveEventStream(rx),
            content_type,
        } => live_binary_stream_response(rx, content_type),
    }
}

/// RUST-P8-003 — build a LIVE incremental SSE response from the orchestrator's
/// client-facing event receiver (`process_command_stream`). Each
/// [`conduit_llm::StreamEvent`] is framed with the SAME wire format as the
/// buffered [`write_sse_stream_body`] path (`event:<type>\ndata:<data>\n\n`,
/// gin-contrib/sse `SSEvent`); the `StreamEvent → {event, data}` projection
/// (`event_type`/`data` → `unwrap_or_default`) mirrors the bridge's
/// `map_http_response_to_output`. The terminal `[DONE]` arrives as a normal
/// event in the stream (NOT auto-appended), matching the buffered path — so the
/// wire bytes are byte-identical to `Stream(Vec)` for the same event sequence,
/// only flushed incrementally as each event arrives.
fn live_sse_response(
    route: OpenAiRoute,
    rx: tokio::sync::mpsc::Receiver<Result<conduit_llm::StreamEvent, ConduitError>>,
) -> Response {
    struct LiveSse {
        route: OpenAiRoute,
        rx: tokio::sync::mpsc::Receiver<Result<conduit_llm::StreamEvent, ConduitError>>,
    }
    impl futures_core::Stream for LiveSse {
        type Item = Result<axum::body::Bytes, std::convert::Infallible>;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            let this = self.get_mut();
            match this.rx.poll_recv(cx) {
                std::task::Poll::Ready(Some(Ok(event))) => {
                    let event_type = event.event_type.unwrap_or_default();
                    let data = event.data.unwrap_or_default();
                    let mut frame = String::new();
                    push_sse_frame(&mut frame, &event_type, &data);
                    std::task::Poll::Ready(Some(Ok(axum::body::Bytes::from(frame))))
                }
                std::task::Poll::Ready(Some(Err(err))) => {
                    let (event_type, data) = stream_error_event(this.route, &err);
                    let mut frame = String::new();
                    push_sse_frame(&mut frame, event_type, &data);
                    std::task::Poll::Ready(Some(Ok(axum::body::Bytes::from(frame))))
                }
                std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        }
    }

    let body = axum::body::Body::from_stream(LiveSse { route, rx });
    match axum::http::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header(axum::http::header::CONNECTION, "keep-alive")
        .header("Access-Control-Allow-Origin", "*")
        .body(body)
    {
        Ok(resp) => resp,
        // Unreachable with valid constant headers; no `.unwrap()` per lints.
        Err(_) => axum::http::Response::new(axum::body::Body::empty()),
    }
}

/// Build the streaming SSE response for a [`OpenAiHandlerOutput::Stream`]
/// payload.
///
/// Parity with Go's `WriteSSEStreamWithErrorFormatter` (chat.go:127-173) plus
/// the `Access-Control-Allow-Origin: *` header stamped at chat.go:107:
///
/// 1. SSE header set (Content-Type/Cache-Control/Connection) via
///    [`sse_response_headers`], augmented with `Access-Control-Allow-Origin: *`
///    (chat.go:107).
/// 2. For each frame: `Ok(event)` writes `event:<type>\ndata:<data>\n\n`
///    (gin-contrib/sse `SSEvent` wire format); `Err(err)` writes
///    `event:error\ndata:<json>\n\n` where `<json>` is
///    [`format_stream_error_frame`]`(&err)` serialized.
///
/// Status code is always 200 for the stream branch — Go does not stamp a
/// different status when entering `WriteSSEStream`; errors after the headers
/// are flushed are emitted as `event:error` frames inside the SSE body.
fn write_sse_stream_response(
    route: OpenAiRoute,
    events: Vec<Result<StreamEvent, ConduitError>>,
) -> Response {
    let body = write_sse_stream_body(route, &events);

    match axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, SSE_CONTENT_TYPE)
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(Body::from(body))
    {
        Ok(response) => response,
        // Header bytes not encodable — mirror Go's net/http behavior of
        // degrading to a bare 500 instead of panicking.
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Build the binary stream response for an [`OpenAiHandlerOutput::Binary`]
/// payload.
///
/// Parity with Go's `WriteBinaryStream` (chat.go:175-239):
///
/// 1. resolve the Content-Type — the orchestrator-stamped value wins, falling
///    back to [`BINARY_STREAM_DEFAULT_CONTENT_TYPE`] (`application/octet-stream`)
///    when the first event carried no Type (chat.go:181, 219-221);
/// 2. stamp the companion header set via [`binary_stream_headers`]
///    (chat.go:223-226: `Cache-Control`, `Connection`,
///    `Access-Control-Allow-Origin`);
/// 3. write the raw bytes verbatim (chat.go:230 `c.Writer.Write(cur.Data)`).
///
/// Status code is always 200 — Go stamps the headers lazily on the first
/// non-empty chunk and never changes them afterwards; errors past that point
/// just abort the stream. We materialise the full body up front (the bounded
/// `OpenAiHandlerOutput::Binary` carries `Vec<u8>`, not a live stream), so the
/// status is fixed at the response builder's default OK.
fn materialise_binary_stream_response(body: Vec<u8>, content_type: Option<String>) -> Response {
    materialise_binary_body(Body::from(body), content_type)
}

/// Build an Axum body over incremental binary events. `Body::from_stream`
/// polls the bounded receiver on demand, so downstream socket pressure
/// propagates through the orchestrator channels to the reqwest reader. The
/// internal `binary.done` persistence sentinel has no payload and is skipped;
/// a mid-body upstream error terminates the HTTP body without exposing provider
/// details to the client.
fn live_binary_stream_response(
    rx: tokio::sync::mpsc::Receiver<Result<conduit_llm::StreamEvent, ConduitError>>,
    content_type: Option<String>,
) -> Response {
    struct LiveBinaryBody {
        rx: tokio::sync::mpsc::Receiver<Result<conduit_llm::StreamEvent, ConduitError>>,
    }

    impl futures_core::Stream for LiveBinaryBody {
        type Item = Result<Bytes, std::io::Error>;

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            let this = self.get_mut();
            loop {
                match this.rx.poll_recv(cx) {
                    std::task::Poll::Ready(Some(Ok(mut event))) => {
                        if let Some(binary) = event.binary.take() {
                            return std::task::Poll::Ready(Some(Ok(Bytes::from(binary))));
                        }
                        // Control-only event (`binary.done`): do not put an
                        // SSE marker or any synthetic bytes on the audio wire.
                    }
                    std::task::Poll::Ready(Some(Err(_))) => {
                        return std::task::Poll::Ready(Some(Err(std::io::Error::other(
                            "upstream binary stream failed",
                        ))));
                    }
                    std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                    std::task::Poll::Pending => return std::task::Poll::Pending,
                }
            }
        }
    }

    materialise_binary_body(Body::from_stream(LiveBinaryBody { rx }), content_type)
}

fn materialise_binary_body(body: Body, content_type: Option<String>) -> Response {
    let resolved_ct = content_type
        .filter(|ct| !ct.trim().is_empty())
        .unwrap_or_else(|| BINARY_STREAM_DEFAULT_CONTENT_TYPE.to_string());
    let mut headers = binary_stream_headers();
    // Content-Type comes from the first event's Type (chat.go:219-221).
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&resolved_ct)
            .unwrap_or_else(|_| HeaderValue::from_static(BINARY_STREAM_DEFAULT_CONTENT_TYPE)),
    );

    match axum::http::Response::builder()
        .status(StatusCode::OK)
        .body(body)
    {
        Ok(mut response) => {
            *response.headers_mut() = headers;
            response
        }
        // Header/body construction failure — degrade to bare 500, mirroring
        // Go's net/http behavior of aborting the response on writer failure.
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Render the SSE body bytes for a sequence of stream frames.
///
/// Exposed separately from [`write_sse_stream_response`] so tests can assert
/// the exact wire format without constructing an axum [`Response`].
///
/// Frame wire format mirrors gin-contrib/sse's `SSEvent` encoder
/// (gin-contrib/sse@v0.1.0/sse-encoder.go:43-66):
///
/// ```text
/// event:<type>\n
/// data:<data>\n
/// \n
/// ```
///
/// An empty `event` string omits the `event:` line (matching gin's behavior
/// when `SSEvent.Event == ""`).
fn write_sse_stream_body(
    route: OpenAiRoute,
    events: &[Result<StreamEvent, ConduitError>],
) -> String {
    let mut body = String::new();
    for frame in events {
        match frame {
            Ok(event) => {
                push_sse_frame(&mut body, &event.event, &event.data);
            }
            Err(err) => {
                let (event_type, data) = stream_error_event(route, err);
                push_sse_frame(&mut body, event_type, &data);
            }
        }
    }
    body
}

/// Encode one SSE event according to the WHATWG field grammar. Every logical
/// line in the payload gets its own `data:` field; otherwise embedded newlines
/// can be reinterpreted as arbitrary SSE fields by the client parser.
fn push_sse_frame(output: &mut String, event_type: &str, data: &str) {
    let event_type = event_type.replace(['\r', '\n'], "");
    if !event_type.is_empty() {
        output.push_str("event:");
        output.push_str(&event_type);
        output.push('\n');
    }
    let normalized = data.replace("\r\n", "\n").replace('\r', "\n");
    for line in normalized.split('\n') {
        output.push_str("data:");
        output.push_str(line);
        output.push('\n');
    }
    output.push('\n');
}

fn stream_error_event(route: OpenAiRoute, err: &ConduitError) -> (&'static str, String) {
    let value = if let Some(body) = conduit_core::error::custom_error_response_body(err) {
        body.clone()
    } else {
        match route {
            OpenAiRoute::AnthropicMessages | OpenAiRoute::AnthropicCountTokens => {
                let mut value = conduit_core::error::anthropic_error_json(err);
                value["type"] = serde_json::Value::String("error".to_string());
                value
            }
            OpenAiRoute::GeminiGenerateContent => conduit_core::error::gemini_error_json(err),
            _ => serde_json::to_value(format_stream_error_frame(err))
                .unwrap_or_else(|_| serde_json::json!({})),
        }
    };
    (
        "error",
        serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()),
    )
}

fn write_gemini_json_stream_response(events: Vec<Result<StreamEvent, ConduitError>>) -> Response {
    if let Some(err) = events.iter().find_map(|event| event.as_ref().err()) {
        return conduit_error_response(err, ErrorResponseFormat::GeminiJson);
    }
    let body = format!(
        "[{}]",
        events
            .iter()
            .filter_map(|event| event.as_ref().ok())
            .filter(|event| event.data != "[DONE]")
            .map(|event| event.data.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    match axum::http::Response::builder()
        .header(
            header::CONTENT_TYPE,
            crate::gemini_handlers::GEMINI_JSON_STREAM_CONTENT_TYPE,
        )
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn live_gemini_json_stream_response(
    rx: tokio::sync::mpsc::Receiver<Result<conduit_llm::StreamEvent, ConduitError>>,
) -> Response {
    struct LiveGeminiJson {
        rx: tokio::sync::mpsc::Receiver<Result<conduit_llm::StreamEvent, ConduitError>>,
        started: bool,
        emitted_item: bool,
        closed: bool,
    }
    impl futures_core::Stream for LiveGeminiJson {
        type Item = Result<axum::body::Bytes, std::io::Error>;

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            let this = self.get_mut();
            if !this.started {
                this.started = true;
                return std::task::Poll::Ready(Some(Ok(axum::body::Bytes::from_static(b"["))));
            }
            if this.closed {
                return std::task::Poll::Ready(None);
            }
            loop {
                match this.rx.poll_recv(cx) {
                    std::task::Poll::Ready(Some(Ok(event))) => {
                        let data = event.data.unwrap_or_default();
                        if data == "[DONE]" || (data.is_empty() && event.done) {
                            continue;
                        }
                        let prefix = if this.emitted_item { "," } else { "" };
                        this.emitted_item = true;
                        return std::task::Poll::Ready(Some(Ok(axum::body::Bytes::from(format!(
                            "{prefix}{data}"
                        )))));
                    }
                    std::task::Poll::Ready(Some(Err(err))) => {
                        this.closed = true;
                        return std::task::Poll::Ready(Some(Err(std::io::Error::other(
                            err.public_message().to_string(),
                        ))));
                    }
                    std::task::Poll::Ready(None) => {
                        this.closed = true;
                        return std::task::Poll::Ready(Some(Ok(axum::body::Bytes::from_static(
                            b"]",
                        ))));
                    }
                    std::task::Poll::Pending => return std::task::Poll::Pending,
                }
            }
        }
    }

    let body = Body::from_stream(LiveGeminiJson {
        rx,
        started: false,
        emitted_item: false,
        closed: false,
    });
    match axum::http::Response::builder()
        .header(
            header::CONTENT_TYPE,
            crate::gemini_handlers::GEMINI_JSON_STREAM_CONTENT_TYPE,
        )
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Translate an [`OpenAiHandlerResponse`] into an axum [`Response`] using the
/// Go Content-Type fallback (`application/json` when the orchestrator did not
/// stamp one — chat.go:86-90).
fn materialise_openai_response(handler: OpenAiHandlerResponse) -> Response {
    let status = StatusCode::from_u16(handler.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let content_type = resolve_response_content_type(handler.content_type.as_deref()).to_string();

    match axum::http::Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(handler.body))
    {
        Ok(response) => response,
        // Header bytes not encodable — mirror Go's net/http behavior of
        // degrading to a bare 500 instead of panicking.
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Build the minimal [`HttpRequest`] the orchestrator pipeline consumes.
///
/// Stamps method/path/query/content_type/body/headers — the union of fields
/// `httpclient.ReadHTTPRequest` (utils.go:19-48) and the gin handler set
/// (`c.Request.URL.Path`, `c.Request.Header`). The host wiring layers
/// `auth`/`request_id`/`client_ip`/`request_context` onto the command
/// afterwards (S17 — same separation as Go middleware + `contexts` package).
///
/// When `api_key_meta` is provided, the validated API key metadata is stamped
/// onto `HttpRequest.metadata` so pipeline middlewares can read key identity,
/// model whitelist, and project association without `PersistenceState`.
fn build_openai_http_request(
    route: OpenAiRoute,
    uri: &Uri,
    method: Method,
    headers: &HeaderMap,
    body: Bytes,
    api_key_meta: Option<&ValidatedApiKeyMetadata>,
    tracing_config: &TracingHeaderConfig,
) -> HttpRequest {
    let mut request = HttpRequest {
        method: method.to_string(),
        path: uri.path().to_string(),
        ..HttpRequest::default()
    };

    // utils.go:22 — full URL string (scheme + host + path + query).
    if let Some(authority) = authority_for(uri, headers) {
        let scheme = scheme_for(uri, headers);
        request.url = Some(format!("{scheme}://{authority}{uri}"));
    }

    // utils.go:24 — query parameters as multi-valued map.
    if let Some(query) = uri.query() {
        for (key, value) in urlencoding_decode_query(query) {
            request.query.entry(key).or_default().push(value);
        }
    }

    // utils.go:25 — headers (axum::http::HeaderMap → conduit_llm BTreeMap).
    for key in headers.keys() {
        let key_str = key.as_str();
        // Skip the pseudo-headers axum/http injects (`:authority`, etc.).
        if key_str.starts_with(':') {
            continue;
        }
        let mut joined = String::new();
        for value in headers.get_all(key).iter() {
            if let Ok(value_str) = value.to_str() {
                if !joined.is_empty() {
                    joined.push(',');
                }
                joined.push_str(value_str);
            }
        }
        if !joined.is_empty() {
            request.headers.insert(key_str.to_string(), joined);
        }
    }

    // Content-Type is surfaced separately so the inbound transformer can
    // branch on it without re-reading the header map.
    if let Some(content_type) = request.headers.get("content-type") {
        request.content_type = Some(content_type.clone());
    }

    // utils.go:45 — body bytes (already validated non-empty by the caller).
    request.body = Some(body.to_vec());

    stamp_trace_thread_metadata(&mut request, uri, &method, headers, &body, tracing_config);

    // Tag the route on the metadata map so the host-side service dispatch
    // (which mirrors Go's per-route inbound-transformer wiring at
    // openai.go:73-289) can pick the right transformer without re-parsing
    // the path. Use the `metadata` slot to stay forward-compatible with the
    // richer transformer_metadata map Go's middleware populates.
    request.metadata.insert(
        "openai_route".to_string(),
        serde_json::Value::from(route.path()),
    );

    // Stamp validated API key metadata onto the request so pipeline
    // middlewares can read key identity, model whitelist, and project
    // association without needing PersistenceState. Mirrors Go's
    // `contexts.WithAPIKey(ctx, apiKey)` + `contexts.WithProjectID(ctx,
    // apiKey.Edges.Project.ID)` injected by `WithAPIKeyAuth`
    // (auth.go:54-58).
    if let Some(meta) = api_key_meta {
        request.metadata.insert(
            api_key_meta_keys::API_KEY_ID.to_string(),
            serde_json::Value::from(meta.api_key_id),
        );
        request.metadata.insert(
            api_key_meta_keys::API_KEY_NAME.to_string(),
            serde_json::Value::from(meta.api_key_name.clone()),
        );
        request.metadata.insert(
            api_key_meta_keys::API_KEY_ALLOWED_MODELS.to_string(),
            serde_json::Value::from(meta.allowed_models.clone()),
        );
        request.metadata.insert(
            api_key_meta_keys::API_KEY_PROJECT_ID.to_string(),
            serde_json::Value::from(meta.project_id),
        );
        request.metadata.insert(
            api_key_meta_keys::API_KEY_MODEL_MAPPING.to_string(),
            serde_json::Value::from(meta.model_mapping.clone()),
        );
        request.metadata.insert(
            api_key_meta_keys::KEY_CHANNEL_IDS.to_string(),
            serde_json::json!(meta.key_channel_ids),
        );
        request.metadata.insert(
            api_key_meta_keys::KEY_CHANNEL_TAGS.to_string(),
            serde_json::json!(meta.key_channel_tags),
        );
        request.metadata.insert(
            api_key_meta_keys::KEY_CHANNEL_TAGS_MATCH_MODE.to_string(),
            serde_json::Value::from(meta.key_channel_tags_match_mode.clone()),
        );
        request.metadata.insert(
            api_key_meta_keys::PROJECT_CHANNEL_IDS.to_string(),
            serde_json::json!(meta.project_channel_ids),
        );
        request.metadata.insert(
            api_key_meta_keys::PROJECT_CHANNELS_BY_MODEL.to_string(),
            serde_json::json!(meta.project_channels_by_model),
        );
        request.metadata.insert(
            api_key_meta_keys::PROJECT_UPSTREAM_MODELS_BY_MODEL.to_string(),
            serde_json::json!(meta.project_upstream_models_by_model),
        );
        request.metadata.insert(
            api_key_meta_keys::PROJECT_CHANNEL_TAGS.to_string(),
            serde_json::json!(meta.project_channel_tags),
        );
        request.metadata.insert(
            api_key_meta_keys::PROJECT_CHANNEL_TAGS_MATCH_MODE.to_string(),
            serde_json::Value::from(meta.project_channel_tags_match_mode.clone()),
        );
        request.metadata.insert(
            api_key_meta_keys::LOAD_BALANCE_STRATEGY.to_string(),
            serde_json::Value::from(meta.load_balance_strategy.clone()),
        );
        if let Some(rpm) = meta.quota_rpm {
            request.metadata.insert(
                api_key_meta_keys::API_KEY_QUOTA_RPM.to_string(),
                serde_json::Value::from(rpm),
            );
        }
        if let Some(limit) = meta.max_concurrent_requests.filter(|limit| *limit > 0) {
            request.metadata.insert(
                api_key_meta_keys::API_KEY_MAX_CONCURRENT.to_string(),
                serde_json::Value::from(limit),
            );
        }
        // Mirror Go `contexts.WithProjectID(ctx, apiKey.Edges.Project.ID)`
        // (auth.go:54-58): the request's effective project is the API key's
        // project. The bridge + persistence layer read the string-valued
        // `project_id` metadata key, distinct from the numeric
        // `api_key_project_id` above. Only stamp when the key has a real
        // project (>0) and the request body did not already supply one.
        if meta.project_id > 0 {
            request
                .metadata
                .entry("project_id".to_string())
                .or_insert_with(|| serde_json::Value::from(meta.project_id.to_string()));
        }
    }

    request
}

fn tracing_header_config(state: &AppState) -> TracingHeaderConfig {
    let config = &state.config().server.trace;
    TracingHeaderConfig {
        trace_header: config.trace_header.clone(),
        request_header: config.request_header.clone(),
        thread_header: config.thread_header.clone(),
        extra_trace_headers: config.extra_trace_headers.clone(),
        extra_trace_body_fields: config.extra_trace_body_fields.clone(),
        claude_code_trace_enabled: config.claude_code_trace_enabled,
        codex_trace_enabled: config.codex_trace_enabled,
        open_code_trace_enabled: config.opencode_trace_enabled,
    }
}

fn stamp_trace_thread_metadata(
    request: &mut HttpRequest,
    uri: &Uri,
    method: &Method,
    headers: &HeaderMap,
    body: &Bytes,
    tracing_config: &TracingHeaderConfig,
) {
    let body_json = serde_json::from_slice::<serde_json::Value>(body).ok();
    let legacy_context =
        TraceThreadContext::from_inputs(headers, uri.query(), body_json.as_ref(), None);

    let trace_id = resolve_trace_id(headers, tracing_config, method, uri.path(), Some(body))
        .trace_id
        .or_else(|| legacy_context.trace_id().map(str::to_string));

    if let Some(trace_id) = trace_id.filter(|id| !id.trim().is_empty()) {
        request.metadata.insert(
            "trace_key".to_string(),
            serde_json::Value::from(trace_id.clone()),
        );
        request
            .metadata
            .entry("session_id".to_string())
            .or_insert_with(|| serde_json::Value::from(trace_id));
    }

    let thread_id = header_context_value(headers, tracing_config.effective_thread_header())
        .or_else(|| legacy_context.thread_id().map(str::to_string));

    if let Some(thread_id) = thread_id.filter(|id| !id.trim().is_empty()) {
        request
            .metadata
            .insert("thread_key".to_string(), serde_json::Value::from(thread_id));
    }
}

fn header_context_value(headers: &HeaderMap, name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return None;
    }
    let value = headers.get(name)?.to_str().ok()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Resolve the request scheme, honoring the `X-Forwarded-Proto` convention
/// the way Go's middleware does (`internal/server/middleware` — `X-Forwarded-
/// Proto` wins when present, matching the gin `ForwardedByClientIP` setting).
fn scheme_for<'a>(uri: &'a Uri, headers: &'a HeaderMap) -> &'a str {
    if let Some(forwarded) = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        forwarded.split(',').next().unwrap_or("http").trim()
    } else {
        uri.scheme_str().unwrap_or("http")
    }
}

/// Resolve the request authority (Host), preferring `X-Forwarded-Host` then
/// `Forwarded` then `Host` then the URI's own authority — same precedence
/// the Go middleware uses.
fn authority_for(uri: &Uri, headers: &HeaderMap) -> Option<String> {
    if let Some(forwarded) = headers
        .get("x-forwarded-host")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        return Some(forwarded.split(',').next().unwrap_or("").trim().to_string());
    }
    if let Some(authority) = uri.authority() {
        return Some(authority.as_str().to_string());
    }
    headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Minimal `application/x-www-form-urlencoded` decoder for the orchestrator
/// request — sufficient to materialise the multi-valued query map the same
/// way Go's `url.URL.Query()` does. Does not handle `+` as space outside the
/// encoded form context (Go's `url.ParseQuery` does, but the OpenAI clients
/// only send `?stream=true` / `?include=all` style keys without `+`, so the
/// edge case is non-load-bearing for the bounded scope).
fn urlencoding_decode_query(query: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            // Bare key (`?stream`) — Go parses it as `stream -> ""`.
            pairs.push((percent_decode(pair), String::new()));
            continue;
        };
        pairs.push((percent_decode(key), percent_decode(value)));
    }
    pairs
}

/// `url.QueryUnescape` subset: percent-decode `%XX` bytes, treat `+` as a
/// literal plus (we only decode query keys/values, not form bodies — see
/// [`urlencoding_decode_query`] doc).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        // `%XX` decode — `url.QueryUnescape` semantics for the bounded
        // query-decoding scope (see [`urlencoding_decode_query`] doc).
        let decoded_byte = if byte == b'%' && index + 2 < bytes.len() {
            hex_digit(bytes[index + 1])
                .zip(hex_digit(bytes[index + 2]))
                .map(|(high, low)| (high << 4) | low)
        } else {
            None
        };
        match decoded_byte {
            Some(b) => {
                decoded.push(b);
                index += 3;
            }
            None => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn openai_model_list_response(
    models: impl IntoIterator<Item = ModelSummary>,
) -> OpenAiModelListResponse {
    OpenAiModelListResponse {
        object: "list",
        data: models
            .into_iter()
            .map(|model| OpenAiModelObject {
                object: "model",
                id: model.id,
                owned_by: model.owned_by,
            })
            .collect(),
    }
}

/// Parse the raw `include=...` values out of a URL-encoded query string.
///
/// This is the low-level extractor kept for compatibility with the existing
/// route smoke tests; the typed [`parse_model_include`] below carries the full
/// Go `parseOpenAIModelInclude` semantics (include-set + needFullData flag).
pub fn parse_model_include_query(query: Option<&str>) -> Vec<String> {
    query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter_map(|pair| pair.split_once('='))
        .filter(|(key, _)| *key == "include")
        .flat_map(|(_, value)| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// S05: empty-body validation
// ---------------------------------------------------------------------------

/// Validate that a chat-completion request body is non-empty.
///
/// Mirrors `ChatCompletionHandlers.ChatCompletionWithRequest` (chat.go:67-70):
///
/// ```text
/// if genericReq == nil || len(genericReq.Body) == 0 {
///     JSONError(c, http.StatusBadRequest, errors.New("Request body is empty"))
///     return
/// }
/// ```
///
/// This is the pure decision function; the caller is responsible for emitting
/// the OpenAI-compatible error envelope once it has chosen the response format.
pub fn validate_chat_request(body: &[u8]) -> Result<(), ConduitError> {
    if body.is_empty() {
        return Err(ConduitError::invalid_request("Request body is empty"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// S11/S14: typed include parser (parseOpenAIModelInclude parity)
// ---------------------------------------------------------------------------

/// Typed result of parsing `?include=...` for the model list/retrieve endpoints.
///
/// Mirrors Go's `(map[string]bool, bool)` return from `parseOpenAIModelInclude`
/// (openai.go:515-547): an optional include-set plus a `need_full_data` flag
/// that selects between the basic facade and the extended DB-backed payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInclude {
    /// Selected field names. `None` means "all fields" (empty query with
    /// `default_include_all=true`, or explicit `include=all`).
    pub fields: Option<BTreeSet<String>>,
    /// Whether the response must be assembled from the extended ModelCard
    /// payload rather than the basic facade.
    pub need_full_data: bool,
}

impl ModelInclude {
    /// Return `true` if the given field should appear in the response.
    ///
    /// Equivalent to Go's `shouldInclude` closure (openai.go:571-576): a `None`
    /// set means "include everything"; otherwise membership decides.
    pub fn should_include(&self, field: &str) -> bool {
        match &self.fields {
            None => true,
            Some(set) => set.contains(field),
        }
    }
}

/// Parse a single `include` query value into a typed [`ModelInclude`].
///
/// Direct parity with `parseOpenAIModelInclude(includeParam, defaultIncludeAll)`
/// (openai.go:515-547):
///
/// * `""` -> `(nil, defaultIncludeAll)`
/// * `"all"` -> `(nil, true)`
/// * `"name,context_length"` -> `({name, context_length}, needFullData)`
///
/// `needFullData` becomes true whenever any of the [`EXTENDED_MODEL_FIELDS`] is
/// selected.
pub fn parse_model_include(include_param: &str, default_include_all: bool) -> ModelInclude {
    if include_param.is_empty() {
        return ModelInclude {
            fields: None,
            need_full_data: default_include_all,
        };
    }

    if include_param == "all" {
        return ModelInclude {
            fields: None,
            need_full_data: true,
        };
    }

    let mut fields: BTreeSet<String> = BTreeSet::new();
    for raw in include_param.split(',') {
        let field = raw.trim();
        if !field.is_empty() {
            fields.insert(field.to_string());
        }
    }

    let need_full_data = fields
        .iter()
        .any(|field| EXTENDED_MODEL_FIELDS.contains(&field.as_str()));

    ModelInclude {
        fields: Some(fields),
        need_full_data,
    }
}

// ---------------------------------------------------------------------------
// S08: SSE headers shaping
// ---------------------------------------------------------------------------

/// Build the fixed SSE header set for a streaming chat-completion response.
///
/// Mirrors `WriteSSEStreamWithErrorFormatter` (chat.go:142-145):
///
/// ```text
/// c.Header("Content-Type", sse.ContentType) // "text/event-stream"
/// c.Header("Cache-Control", "no-cache")
/// c.Header("Connection", "keep-alive")
/// ```
///
/// The Go constant `sse.ContentType` from gin-contrib/sse is
/// `"text/event-stream"` (no `; charset=utf-8` suffix — see
/// gin-contrib/sse@v0.1.0/sse-encoder.go:21). We replicate the gin value
/// verbatim so that downstream clients / proxies see byte-identical framing.
pub const SSE_CONTENT_TYPE: &str = "text/event-stream";

pub fn sse_response_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    // Errors from `from_static` only occur for invalid header bytes; these
    // literals are all valid per RFC 7230 token grammar.
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(SSE_CONTENT_TYPE),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers
}

// ---------------------------------------------------------------------------
// S09/S15: FormatStreamError (OpenAI-compatible SSE error frame)
// ---------------------------------------------------------------------------

/// Shape of the OpenAI-compatible SSE error frame emitted by
/// [`format_stream_error_frame`]. Serialized as the `data:` payload of an
/// `event:error` SSE message (see chat.go:164 + FormatStreamError chat.go:261).
#[derive(Debug, Serialize)]
pub struct StreamErrorFrame {
    pub error: StreamErrorBody,
    /// Mirrors the top-level `request_id` field in Go's output. Empty string
    /// when the originating error has no request id (Go marshals it anyway as
    /// `"request_id": ""`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StreamErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: String,
}

/// Format an [`ConduitError`] into an OpenAI-compatible SSE error frame.
///
/// This is the Rust equivalent of Go's `FormatStreamError` (chat.go:261-319).
/// The Go function dispatches on three concrete error types via `errors.As`:
///
/// 1. `*orchestrator.QuotaExhaustedError` -> `{error:{message, type:"quota_exhausted", code:"quota_exhausted"}}`
/// 2. `*llm.ResponseError` -> `{error:{message, type:detail.type|server_error, code:detail.code}, request_id:detail.request_id}`
/// 3. `*httpclient.Error` -> parses `error.type`/`error.code`/`request_id` from the upstream body.
/// 4. fallback -> `{error:{message:ExtractErrorMessage(err), type:"server_error", code:""}, request_id:""}`
///
/// In the Rust workspace all four error flavours collapse into [`ConduitError`]:
///
/// * `ErrorKind::QuotaExhausted` carries code/type `"quota_exhausted"` and maps
///   to case (1).
/// * Case (2) (`*llm.ResponseError`) is detected by downcasting `err.source` to
///   [`conduit_llm::model::ResponseError`]; its `detail.detail_type`,
///   `detail.code`, and `detail.request_id` flow through verbatim, matching Go's
///   `errors.As` discrimination (`chat.go:277-294`).
/// * `provider_body` carries the decoded upstream JSON body, used for case (3);
///   the message is extracted via [`extract_error_message`] (mirrors Go's
///   `orchestrator.ExtractErrorMessage`).
/// * `provider_status`, `code`, `safe_message` reconstruct the generic case (4).
///
/// The single function returns a [`StreamErrorFrame`] struct so tests can
/// assert field values directly; callers serialize it for the wire.
pub fn format_stream_error_frame(err: &ConduitError) -> StreamErrorFrame {
    // Case 1: QuotaExhausted — Go emits a compact frame with no request_id.
    if err.kind == ErrorKind::QuotaExhausted {
        return StreamErrorFrame {
            error: StreamErrorBody {
                message: err.public_message().to_string(),
                error_type: "quota_exhausted".to_string(),
                code: "quota_exhausted".to_string(),
            },
            // Go's quota branch does not include request_id.
            request_id: None,
        };
    }

    let mut error_type = String::from("server_error");
    let mut code = String::new();
    let mut request_id = String::new();
    // `message_from_body` distinguishes the case-3/4 message (extracted from the
    // provider body, mirroring Go's `orchestrator.ExtractErrorMessage`) from the
    // case-2 message (which uses `public_message()` exactly like Go uses the
    // ResponseError.Detail.Message path).
    let mut message = err.public_message().to_string();

    // Go's FormatStreamError (chat.go:261-319) uses sequential `errors.As`
    // checks with early returns, so the precedence is:
    //   QuotaExhausted > ResponseError > httpclient.Error > generic.
    //
    // Case 2 discriminator: the `err.source` downcasts to `ResponseError`. This
    // mirrors Go's `errors.As(err, &respErr)` more faithfully than the previous
    // "metadata request_id non-empty" heuristic, and lets us pull the literal
    // provider `Detail.Type` (e.g. "permission_error") that Go emits
    // (chat.go:277-294). `into_response_error` cannot be used here because it
    // consumes `self`; we borrow the source via `as_deref()` instead.
    if let Some(resp_err) = err
        .source
        .as_deref()
        .and_then(|s| s.downcast_ref::<conduit_llm::model::ResponseError>())
    {
        // Go uses the provider's literal `respErr.Detail.Type`, falling back to
        // "server_error" only when empty (chat.go:279-281).
        let dtype = resp_err.detail.detail_type.as_str();
        if !dtype.is_empty() {
            error_type = dtype.to_string();
        }
        // Go carries the provider-specific `Detail.Code` (e.g. "1311") into the
        // emitted `code` field verbatim (chat.go:283-294) — including the empty
        // string, which Go's tests assert when `Detail.Code` is unset.
        code = resp_err.detail.code.clone();
        // request_id comes from ResponseError.detail.request_id (Go path), with
        // the existing metadata path as a fallback for any caller that stamped
        // it directly on ConduitError.
        if !resp_err.detail.request_id.is_empty() {
            request_id = resp_err.detail.request_id.clone();
        } else if let Some(rid) = err
            .metadata
            .get("request_id")
            .and_then(|v| v.as_str())
            .filter(|rid| !rid.is_empty())
        {
            request_id = rid.to_string();
        }
    } else if let Some(body) = &err.provider_body {
        // Case 3: httpclient.Error (chat.go:296-309). Extract type/code/
        // request_id from the upstream body. `provider_body` is the Rust
        // analogue of `httpErr.Body`.
        if let Some(s) = body
            .get("error")
            .and_then(|e| e.get("type"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            error_type = s.to_string();
        }
        if let Some(s) = body
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            code = s.to_string();
        }
        if let Some(rid) = body
            .get("request_id")
            .and_then(|v| v.as_str())
            .filter(|rid| !rid.is_empty())
        {
            request_id = rid.to_string();
        }
        // Case 3/4 message: mirror Go's `orchestrator.ExtractErrorMessage`
        // (request_execution.go:244-270), which reads `error.message` (and the
        // `errors[]` array variants) from the upstream body. Falls back to the
        // safe `public_message()` when the body is hidden/empty.
        message = extract_error_message(err);
    }

    if err
        .metadata
        .contains_key(conduit_core::ERROR_RESPONSE_REWRITE_CHANNEL_METADATA)
    {
        message = err.public_message().to_string();
        if let Some(rewritten_type) = conduit_core::error::custom_error_response_type(err) {
            error_type = rewritten_type.to_string();
        }
        code = err.code.clone().unwrap_or_default();
    }

    let request_id_opt = if request_id.is_empty() {
        None
    } else {
        Some(request_id)
    };

    StreamErrorFrame {
        error: StreamErrorBody {
            message,
            error_type,
            code,
        },
        request_id: request_id_opt,
    }
}

/// Extract the user-facing message for the case-3/4 branches of
/// [`format_stream_error_frame`].
///
/// Mirrors Go's `orchestrator.ExtractErrorMessage`
/// (`request_execution.go:244-270`): it reads `error.message` from the upstream
/// JSON body, then falls back to the first element of an `errors[]` array, then
/// `errors.message`, and finally a generic placeholder. In the Rust workspace
/// the upstream body lives on [`ConduitError::provider_body`] (`Option<Value>`);
/// when an UpstreamErrorPolicy of Hidden/Custom applies, the transformer
/// pipeline sets `provider_body` to `None`, so this helper naturally falls back
/// to [`ConduitError::public_message`] and never leaks provider text under a
/// hidden policy (verified by `format_stream_error_frame_hidden_policy_uses_safe_message`).
fn extract_error_message(err: &ConduitError) -> String {
    if let Some(body) = &err.provider_body {
        // 1. `body.error.message`
        if let Some(s) = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return s.to_string();
        }
        // 2. `body.errors[0].message`
        if let Some(s) = body
            .get("errors")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|e| e.get("message"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return s.to_string();
        }
        // 3. `body.errors.message`
        if let Some(s) = body
            .get("errors")
            .and_then(|e| e.get("message"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return s.to_string();
        }
    }
    // 4. Fallback — safe provider-aware message.
    err.public_message().to_string()
}

// ---------------------------------------------------------------------------
// S11: build_openai_models_response — model-list/retrieve shaping with include
// ---------------------------------------------------------------------------

use conduit_core::objects::model::ModelCard;

/// Input row for [`build_openai_models_response`]. Carries the union of fields
/// the Go `convertModelFacadeToOpenAIModel` and `convertModelToOpenAIExtended`
/// paths read from `biz.ModelFacade` + `*ent.Model` (openai.go:549-639). The
/// caller assembles one [`ModelRow`] per visible model: facade-only fields
/// (`id`, `owned_by`, `created`) are always populated; extended fields are
/// `Some` only when the configured DB row was loaded (mirrors the Go
/// "configuredModel, ok := dbModelMap[m.ID]" branch in `ListModels`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub id: String,
    pub owned_by: String,
    /// Unix seconds, like `biz.ModelFacade.Created` / `m.CreatedAt.Unix()`.
    pub created: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    /// Model type as a string (`string(m.Type)` in Go).
    pub ty: Option<String>,
    pub model_card: Option<ModelCard>,
    /// Effective customer price after price-book, project multiplier, and
    /// accounting-to-credit conversion. Static ModelCard costs are metadata
    /// only and must never populate this field.
    pub retail_pricing: Option<OpenAiPricing>,
}

impl ModelRow {
    /// Build a facade-only row — the shape `convertModelFacadeToOpenAIModel`
    /// consumes (openai.go:549-556). All extended fields are `None`, matching
    /// the Go basic path that never reads ModelCard.
    pub fn facade(
        id: impl Into<String>,
        owned_by: impl Into<String>,
        created: i64,
        _base_url: &str,
    ) -> Self {
        Self {
            id: id.into(),
            owned_by: owned_by.into(),
            created,
            name: None,
            description: None,
            icon: None,
            ty: None,
            model_card: None,
            retail_pricing: None,
        }
    }
}

/// Serialized OpenAI model object. Mirrors Go's `OpenAIModel`
/// (openai.go:490-504): basic fields are always present; extended fields use
/// `skip_serializing_if = "Option::is_none"` to match Go's `omitempty` tags so
/// the wire payload is byte-identical between the two implementations.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenAiModelEntry {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub owned_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<OpenAiModalities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<OpenAiCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<OpenAiPricing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub ty: Option<String>,
}

/// `Modalities` (openai.go:470-473).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OpenAiModalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

/// `Capabilities` (openai.go:475-479).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OpenAiCapabilities {
    pub vision: bool,
    #[serde(rename = "tool_call")]
    pub tool_call: bool,
    pub reasoning: bool,
}

/// `Pricing` (openai.go:481-488).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenAiPricing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    pub unit: &'static str,
    pub currency: String,
    pub display_name: String,
}

/// Wire envelope returned by `/v1/models` and `/v1/models/{model}` list/retrieve.
/// Mirrors Go's `gin.H{"object":"list", "data": openaiModels}` (openai.go:784-787).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenAiModelsResponse {
    pub object: &'static str,
    pub data: Vec<OpenAiModelEntry>,
}

/// Build the OpenAI object/list response from a sequence of [`ModelRow`]s,
/// applying the include selection carried by [`ModelInclude`].
///
/// Direct parity with Go's `ListModels` shaping (openai.go:726-788) and the
/// `convertModelFacadeToOpenAIModel` / `convertModelToOpenAIExtended` helpers
/// (openai.go:549-639):
///
/// * When `include.need_full_data` is `false`, only the facade fields
///   (`id`, `object`, `created`, `owned_by`) are populated — matching Go's
///   `convertModelFacadeToOpenAIModel`.
/// * When `need_full_data` is `true`, extended fields are populated from
///   `model_card` per the `should_include` rule: a `None` include set means
///   "all fields" (Go `shouldInclude` closure at openai.go:571-576); otherwise
///   membership in the named-field set decides.
///
/// Rows whose `model_card` is `None` still produce a basic entry even on the
/// extended path (mirrors Go's "fall back to facade when no DB row" branch,
/// openai.go:775-781).
pub fn build_openai_models_response<I>(models: I, include: &ModelInclude) -> OpenAiModelsResponse
where
    I: IntoIterator<Item = ModelRow>,
{
    let data = models
        .into_iter()
        .map(|row| build_openai_model_entry(&row, include))
        .collect();

    OpenAiModelsResponse {
        object: "list",
        data,
    }
}

/// Build a single [`OpenAiModelEntry`] from a [`ModelRow`] + include selection.
///
/// This is the Rust equivalent of Go's `convertModelFacadeToOpenAIModel` (basic
/// path, openai.go:549-556) and `convertModelToOpenAIExtended` (extended path,
/// openai.go:562-639), dispatched by `need_full_data`. Exposed publicly so the
/// `/v1/models/{model}` retrieve handler can reuse the single-row shape.
pub fn build_openai_model_entry(row: &ModelRow, include: &ModelInclude) -> OpenAiModelEntry {
    // Always-on basic fields (openai.go:563-568).
    let mut entry = OpenAiModelEntry {
        id: row.id.clone(),
        object: "model",
        created: row.created,
        owned_by: row.owned_by.clone(),
        name: None,
        description: None,
        context_length: None,
        max_output_tokens: None,
        modalities: None,
        capabilities: None,
        pricing: None,
        icon: None,
        ty: None,
    };

    if !include.need_full_data {
        return entry;
    }

    // Optional non-ModelCard fields (openai.go:581-594).
    if include.should_include("name") {
        entry.name = row.name.clone();
    }
    if include.should_include("icon") {
        entry.icon = row.icon.clone();
    }
    if include.should_include("type") {
        entry.ty = row.ty.clone();
    }
    if include.should_include("description") {
        entry.description = row.description.clone();
    }

    // ModelCard-backed fields (openai.go:596-637).
    if let Some(card) = &row.model_card {
        if include.should_include("modalities") {
            // Go substitutes an empty slice for nil (openai.go:599-604).
            let input = if card.modalities.input.is_empty() {
                Vec::new()
            } else {
                card.modalities.input.clone()
            };
            let output = if card.modalities.output.is_empty() {
                Vec::new()
            } else {
                card.modalities.output.clone()
            };
            entry.modalities = Some(OpenAiModalities { input, output });
        }
        if include.should_include("capabilities") {
            entry.capabilities = Some(OpenAiCapabilities {
                vision: card.vision,
                tool_call: card.tool_call,
                reasoning: card.reasoning.supported,
            });
        }
        if include.should_include("context_length") {
            entry.context_length = Some(card.limit.context);
        }
        if include.should_include("max_output_tokens") {
            entry.max_output_tokens = Some(card.limit.output);
        }
    }

    if include.should_include("pricing") {
        entry.pricing = row.retail_pricing.clone();
    }

    entry
}

// ---------------------------------------------------------------------------
// S10/S15: audio binary-stream content-type selector + header set
// ---------------------------------------------------------------------------

/// Mode in which a speech/audio response should be framed, given the content
/// type of the first stream event. Mirrors the dispatch in
/// `OpenAIHandlers.CreateSpeech` (openai.go:313-336) + the binary-vs-SSE branch
/// in `WriteBinaryStream` (chat.go:175-239):
///
/// * `Binary` — first event carries an `audio/*` or `application/octet-stream`
///   payload (per `StreamEvent.IsBinaryAudioChunk`, httpclient/model.go:119-127);
///   the handler writes raw bytes via the binary stream writer. The original
///   content-type is surfaced so the caller can stamp the `Content-Type` header
///   (chat.go:219-221).
/// * `Sse` — first event is an SSE frame (`text/event-stream`); the regular
///   SSE writer is used.
/// * `Json` — first event has no recognizable binary/stream content type (or
///   there is no first event); the regular JSON envelope path applies. This
///   covers Go's `WriteBinaryStream` default `application/octet-stream` case
///   only when the upstream explicitly sent that type — see
///   [`audio_response_content_type`] for the precise rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioMode {
    Binary { content_type: String },
    Sse,
    Json,
}

/// Default Content-Type `WriteBinaryStream` falls back to when the first event
/// carries no Type (chat.go:181). Kept as a constant so callers can mirror Go
/// exactly when stamping headers.
pub const BINARY_STREAM_DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

/// Classify the content type of the first stream event into an [`AudioMode`].
///
/// Parity with the binary-vs-SSE decision tree in Go:
///
/// 1. `StreamEvent.IsBinaryAudioChunk` (httpclient/model.go:119-127) returns
///    `true` when `Type` lower-trimmed starts with `audio/` or equals
///    `application/octet-stream`. Those cases select the binary stream writer
///    and propagate the original content type to the response header
///    (chat.go:219-221).
/// 2. `text/event-stream` selects the SSE writer (`ChatCompletionWithRequest`
///    non-stream-format branch, openai.go:330-331).
/// 3. Anything else (including `None` / empty) falls back to the JSON envelope
///    path. Note: this intentionally diverges from chat.go:181's
///    `application/octet-stream` *default* — that default only applies inside
///    the binary writer, which is only entered when `IsBinaryAudioChunk` is
///    already true; the selector's job is to decide which writer to enter, so
///    an absent content type maps to `Json` (the non-stream orchestrator path).
pub fn audio_response_content_type(first_event_ct: Option<&str>) -> AudioMode {
    let raw = match first_event_ct {
        Some(s) => s,
        None => return AudioMode::Json,
    };
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return AudioMode::Json;
    }

    // Mirrors IsBinaryAudioChunk's prefix + exact-match checks.
    if normalized.starts_with("audio/") || normalized == "application/octet-stream" {
        return AudioMode::Binary {
            content_type: raw.to_string(),
        };
    }

    if normalized == "text/event-stream" || normalized.starts_with("text/event-stream;") {
        return AudioMode::Sse;
    }

    AudioMode::Json
}

/// Build the fixed companion header set for a binary audio stream response.
///
/// Mirrors `WriteBinaryStream` (chat.go:223-226): the writer sets exactly
/// `Cache-Control: no-cache`, `Connection: keep-alive`,
/// `Access-Control-Allow-Origin: *`. The `Content-Type` header is **not**
/// added here — the caller stamps it separately from
/// [`AudioMode::Binary::content_type`] (or
/// [`BINARY_STREAM_DEFAULT_CONTENT_TYPE`] when that string is empty), matching
/// the Go order at chat.go:219-223.
pub fn binary_stream_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers
}

// ---------------------------------------------------------------------------
// S07/S15-speech-route: resolve_response_content_type — non-stream CT fallback
// ---------------------------------------------------------------------------

/// Default Content-Type Go stamps on a non-stream chat/response body when the
/// orchestrator result carries no `Content-Type` header.
///
/// Direct parity with the repeated Go idiom at `chat.go:87`, `openai.go:414`
/// (CreateVideo) and `openai.go:446` (GetVideo):
///
/// ```text
/// contentType := "application/json"
/// if ct := resp.Headers.Get("Content-Type"); ct != "" {
///     contentType = ct
/// }
/// c.Data(resp.StatusCode, contentType, resp.Body)
/// ```
///
/// Exposed so the (future) wiring of the non-stream writer reuses one helper
/// instead of re-implementing the fallback in every handler. The function is
/// pure — it does not touch `Response`; callers feed the result to whichever
/// writer they choose.
pub const DEFAULT_NONSTREAM_CONTENT_TYPE: &str = "application/json";

/// Pick the response Content-Type for a non-stream chat/response body.
///
/// Returns the orchestrator result's `Content-Type` header verbatim when
/// present and non-empty; otherwise falls back to
/// [`DEFAULT_NONSTREAM_CONTENT_TYPE`]. `None` represents "no header present"
/// (the orchestrator result had no `Content-Type` at all), which is the common
/// case for JSON chat completions.
pub fn resolve_response_content_type(resp_content_type: Option<&str>) -> &str {
    match resp_content_type {
        Some(ct) if !ct.is_empty() => ct,
        _ => DEFAULT_NONSTREAM_CONTENT_TYPE,
    }
}

// ---------------------------------------------------------------------------
// S15-speech-route: should_use_binary_speech_stream — request-side routing
// ---------------------------------------------------------------------------

/// Decide whether a `/v1/audio/speech` request should be routed through the
/// binary stream writer rather than the default SSE writer.
///
/// Direct parity with Go's `shouldUseBinarySpeechStream` (openai.go:338-360):
///
/// 1. `nil` request -> error (the Rust caller validates this earlier; we still
///    surface an error for parity when the body slice is missing).
/// 2. empty body -> `invalid_request` error ("request body is empty").
/// 3. non-JSON `Content-Type` (anything that does not contain
///    `application/json`, case-insensitive) -> `Ok(false)`. This hands routing
///    back to the transformer, which will report a validation error itself —
///    exactly mirroring the Go test case
///    `TestShouldUseBinarySpeechStream/non-json_content_type_lets_transformer_report_validation`.
/// 4. malformed JSON body -> `invalid_request` error.
/// 5. after decode, `stream_format` (lowercased, trimmed) selects the binary
///    path when it is non-empty and not `"sse"`; everything else selects the
///    regular SSE path.
///
/// The `stream_format` field name mirrors Go's `speechRouteRequestBody`
/// (openai.go:69-71). Callers pass the raw request body + Content-Type header
/// value; this function does not touch `http::Request` so it stays unit-testable.
pub fn should_use_binary_speech_stream(
    body: Option<&[u8]>,
    content_type: Option<&str>,
) -> Result<bool, ConduitError> {
    let body = match body {
        Some(b) => b,
        // Go's nil-request guard (openai.go:339-341). In the Rust handler the
        // generic request is constructed before this call, so this branch only
        // fires if the caller passes None — keep the parity error so a wiring
        // bug surfaces as invalid_request rather than a panic.
        None => {
            return Err(ConduitError::invalid_request("http request is nil"));
        }
    };

    if body.is_empty() {
        return Err(ConduitError::invalid_request("request body is empty"));
    }

    // Step 3: non-JSON content type defers to the transformer. The Go test
    // explicitly uses "multipart/form-data" to exercise this branch.
    let ct_lower = content_type.unwrap_or("").to_ascii_lowercase();
    if !ct_lower.is_empty() && !ct_lower.contains("application/json") {
        return Ok(false);
    }

    // Step 4: decode the minimal envelope.
    let parsed: SpeechRouteRequestBody = serde_json::from_slice(body).map_err(|err| {
        // Go wraps the JSON error with transformer.ErrInvalidRequest; the Rust
        // ConduitError::invalid_request carries the same semantic kind and a
        // descriptive message that includes the underlying decode error.
        ConduitError::invalid_request(format!("failed to decode speech request: {err}"))
    })?;

    // Step 5: normalize stream_format the same way Go does
    // (openai.go:357-359): lowercase + trim, then compare against "" and "sse".
    let stream_format = parsed.stream_format.trim().to_ascii_lowercase();
    Ok(!stream_format.is_empty() && stream_format != "sse")
}

/// Minimal envelope decoded from the `/v1/audio/speech` request body to inspect
/// the `stream_format` field. Mirrors Go's `speechRouteRequestBody`
/// (openai.go:69-71). Only `stream_format` is read; all other fields are
/// ignored — the full request body is forwarded to the orchestrator verbatim.
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
struct SpeechRouteRequestBody {
    #[serde(default)]
    stream_format: String,
}

// ---------------------------------------------------------------------------
// S14: trim_model_splat — Gin /v1/models/*model splat leading-slash strip
// ---------------------------------------------------------------------------

/// Strip a single leading `/` from the `/v1/models/*model` splat parameter.
///
/// Direct parity with Go's `RetrieveModel` (openai.go:677):
///
/// ```text
/// modelID := strings.TrimPrefix(c.Param("model"), "/")
/// ```
///
/// Gin hands the catch-all parameter **with** its leading slash (so
/// `/v1/models/deepseek/deepseek-chat` yields the splat
/// `/deepseek/deepseek-chat`); `strings.TrimPrefix` removes at most that one
/// leading slash while preserving any inner slashes that are part of the model
/// id (e.g. `deepseek/deepseek-chat`).
pub fn trim_model_splat(splat: &str) -> &str {
    // `str::strip_prefix('/')` returns `Option<&str>`; the `None` case (no
    // leading slash) falls back to the original slice, matching
    // `strings.TrimPrefix`'s no-op behavior.
    splat.strip_prefix('/').unwrap_or(splat)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use serde_json::{Value, json};

    use super::*;
    use conduit_core::objects::model::{
        ModelCardCost, ModelCardLimit, ModelCardModalities, ModelCardReasoning,
    };

    // -----------------------------------------------------------------------
    // Existing Erdos/Boyle tests (kept verbatim)
    // -----------------------------------------------------------------------

    #[test]
    fn model_list_response_serializes_openai_shape() -> Result<(), Box<dyn Error>> {
        let response = openai_model_list_response([
            ModelSummary::new("gpt-4o-mini", "openai"),
            ModelSummary::new("claude-3-5-sonnet", "anthropic"),
        ]);
        let body = serde_json::to_value(response)?;

        assert_eq!(
            body,
            json!({
                "object": "list",
                "data": [
                    {
                        "object": "model",
                        "id": "gpt-4o-mini",
                        "owned_by": "openai"
                    },
                    {
                        "object": "model",
                        "id": "claude-3-5-sonnet",
                        "owned_by": "anthropic"
                    }
                ]
            })
        );
        Ok(())
    }

    #[test]
    fn model_list_response_serializes_empty_data_array() -> Result<(), Box<dyn Error>> {
        let response = openai_model_list_response(Vec::new());
        let body = serde_json::to_value(response)?;

        assert_eq!(body["object"], "list");
        assert_eq!(body["data"], json!([]));
        Ok(())
    }

    #[test]
    fn include_query_parser_collects_repeated_and_comma_separated_values() {
        let includes = parse_model_include_query(Some(
            "include=permissions,capabilities&ignored=true&include=metadata",
        ));

        assert_eq!(includes, ["permissions", "capabilities", "metadata"]);
    }

    #[test]
    fn include_query_parser_ignores_missing_and_empty_values() {
        assert!(parse_model_include_query(None).is_empty());
        assert!(parse_model_include_query(Some("include=&foo=bar")).is_empty());
        assert_eq!(
            parse_model_include_query(Some("include= permissions ,, limits ")),
            ["permissions", "limits"]
        );
    }

    // -----------------------------------------------------------------------
    // S05: validate_chat_request
    // -----------------------------------------------------------------------

    #[test]
    fn validate_chat_request_rejects_empty_body() {
        // Use `match` rather than `.unwrap_err()` — the workspace lints deny
        // both `unwrap_used` and `expect_used`, and clippy treats `unwrap_err`
        // as the same family.
        match validate_chat_request(b"") {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::InvalidRequest);
                assert_eq!(err.http_status, 400);
                assert_eq!(err.public_message(), "Request body is empty");
            }
            Ok(()) => panic!("empty body must be rejected"),
        }
    }

    #[test]
    fn validate_chat_request_accepts_nonempty_body() -> Result<(), Box<dyn Error>> {
        // Mirrors the Go guard at chat.go:67 — any non-empty body passes the
        // shape-agnostic guard; deeper validation happens in the orchestrator.
        validate_chat_request(b"{ \"model\": \"gpt-4o-mini\" }")?;
        validate_chat_request(b"{}")?;
        validate_chat_request(&[0_u8; 1])?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // S11/S14: parse_model_include
    // -----------------------------------------------------------------------

    #[test]
    fn parse_model_include_empty_uses_default_include_all_false() {
        // parseOpenAIModelInclude("", false) -> (nil, false). Go's caller uses
        // the facade path when needFullData is false, so should_include is
        // never consulted. We only assert the parsed fields and the flag.
        let parsed = parse_model_include("", false);

        assert_eq!(parsed.fields, None);
        assert!(!parsed.need_full_data);
    }

    #[test]
    fn parse_model_include_empty_uses_default_include_all_true() {
        // parseOpenAIModelInclude("", true) -> (nil, true)
        let parsed = parse_model_include("", true);

        assert_eq!(parsed.fields, None);
        assert!(parsed.need_full_data);
        // With default_include_all=true every field is selected.
        assert!(parsed.should_include("pricing"));
        assert!(parsed.should_include("context_length"));
    }

    #[test]
    fn parse_model_include_all_expands_to_full_data() {
        // parseOpenAIModelInclude("all", _) -> (nil, true)
        let parsed = parse_model_include("all", false);

        assert_eq!(parsed.fields, None);
        assert!(parsed.need_full_data);
    }

    #[test]
    fn parse_model_include_basic_only_does_not_request_full_data() {
        // Only facade-level identifiers; needFullData stays false.
        let parsed = parse_model_include("id,object", false);

        match &parsed.fields {
            Some(set) => {
                assert!(set.contains("id"));
                assert!(set.contains("object"));
            }
            None => panic!("expected Some(fields) for non-empty, non-'all' input"),
        }
        assert!(!parsed.need_full_data);
    }

    #[test]
    fn parse_model_include_extended_field_flips_need_full_data() {
        // Mirrors the extendedFields loop (openai.go:538-544).
        for extended in EXTENDED_MODEL_FIELDS {
            let parsed = parse_model_include(extended, false);
            assert!(
                parsed.need_full_data,
                "field {extended} should request full data"
            );
            assert!(parsed.should_include(extended));
            assert!(!parsed.should_include("other"));
        }
    }

    #[test]
    fn parse_model_include_mixed_extended_and_basic() {
        let parsed = parse_model_include("id,name,context_length,ignored_extra", false);

        match &parsed.fields {
            Some(set) => {
                assert!(set.contains("id"));
                assert!(set.contains("name"));
                assert!(set.contains("context_length"));
                assert!(set.contains("ignored_extra"));
            }
            None => panic!("expected Some(fields) for mixed include input"),
        }
        // Extended members trigger full-data mode.
        assert!(parsed.need_full_data);
    }

    #[test]
    fn parse_model_include_trims_and_drops_empty_fields() {
        // Go's parser trims each field and skips empty strings (openai.go:531-535).
        let parsed = parse_model_include(" name , , context_length ", false);

        match &parsed.fields {
            Some(set) => {
                assert_eq!(set.len(), 2);
                assert!(set.contains("name"));
                assert!(set.contains("context_length"));
            }
            None => panic!("expected Some(fields) for trimmed input"),
        }
        assert!(parsed.need_full_data);
    }

    // -----------------------------------------------------------------------
    // S08: sse_response_headers
    // -----------------------------------------------------------------------

    #[test]
    fn sse_response_headers_sets_fixed_event_stream_set() -> Result<(), Box<dyn Error>> {
        let headers = sse_response_headers();

        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .ok_or("content-type missing")?,
            HeaderValue::from_static(SSE_CONTENT_TYPE)
        );
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .ok_or("cache-control missing")?,
            HeaderValue::from_static("no-cache")
        );
        assert_eq!(
            headers
                .get(header::CONNECTION)
                .ok_or("connection missing")?,
            HeaderValue::from_static("keep-alive")
        );
        // No other headers — Go sets exactly these three before flushing.
        assert_eq!(headers.len(), 3);
        Ok(())
    }

    #[test]
    fn sse_content_type_matches_gin_sse_constant() {
        // gin-contrib/sse.ContentType == "text/event-stream" (no charset suffix).
        assert_eq!(SSE_CONTENT_TYPE, "text/event-stream");
    }

    // -----------------------------------------------------------------------
    // S09/S15: format_stream_error_frame
    // -----------------------------------------------------------------------

    /// Helper: render the frame to a JSON Value the way Go's `json.Marshal`
    /// would before it is sent as the SSE `data:` payload. Returns
    /// `Result<_, serde_json::Error>` so tests use the `?` operator instead of
    /// forbidden `.unwrap()` calls.
    fn frame_json(err: &ConduitError) -> Result<Value, serde_json::Error> {
        serde_json::to_value(format_stream_error_frame(err))
    }

    #[test]
    fn format_stream_error_frame_plain_internal_error() -> Result<(), serde_json::Error> {
        // Mirrors TestFormatStreamError_PlainError (chat_test.go:360-374):
        // unknown error -> type=server_error, code="".
        let err = ConduitError::internal("something went wrong");

        let body = frame_json(&err)?;

        assert_eq!(body["error"]["message"], "Internal server error");
        assert_eq!(body["error"]["type"], "server_error");
        assert_eq!(body["error"]["code"], "");
        Ok(())
    }

    #[test]
    fn format_stream_error_frame_quota_exhausted_shape() -> Result<(), serde_json::Error> {
        // Mirrors TestFormatStreamError_QuotaExhaustedError (chat_test.go:395-409):
        // message preserved verbatim, type/code == quota_exhausted, no request_id.
        let err = ConduitError::quota_exhausted("all channels quota exhausted for model gpt-4");

        let body = frame_json(&err)?;

        assert_eq!(
            body["error"]["message"],
            "all channels quota exhausted for model gpt-4"
        );
        assert_eq!(body["error"]["type"], "quota_exhausted");
        assert_eq!(body["error"]["code"], "quota_exhausted");
        // Go's quota branch does not emit request_id.
        assert!(body.get("request_id").is_none() || body["request_id"].is_null());
        Ok(())
    }

    #[test]
    fn format_stream_error_frame_response_error_passes_type_code_request_id()
    -> Result<(), serde_json::Error> {
        // Mirrors TestFormatStreamError_LlmResponseError_PassesCodeAndRequestID
        // (chat_test.go:450-472): upstream ResponseError contributes code/type/
        // request_id directly. The transformer pipeline attaches the
        // `*llm.ResponseError` to `ConduitError::source` via `.with_source(...)`
        // (see conduit-transformers/src/openai_stream.rs:486); the provider's
        // literal `Detail.Type` (e.g. "permission_error") flows through to the
        // emitted `type` field, exactly like Go's `respErr.Detail.Type`.
        let resp_err = conduit_llm::model::ResponseError {
            status_code: 403,
            detail: conduit_llm::model::ErrorDetail {
                code: "1311".to_string(),
                message: "当前订阅套餐暂未开放GPT-6权限".to_string(),
                detail_type: "permission_error".to_string(),
                param: String::new(),
                request_id: "202603112254417d15bd26697445b0".to_string(),
            },
        };
        let err = ConduitError::upstream("当前订阅套餐暂未开放GPT-6权限")
            .with_safe_message("当前订阅套餐暂未开放GPT-6权限")
            .with_source(resp_err);

        let body = frame_json(&err)?;

        assert_eq!(body["error"]["message"], "当前订阅套餐暂未开放GPT-6权限");
        // The provider's literal type string flows through (Go chat.go:277-294).
        assert_eq!(body["error"]["type"], "permission_error");
        assert_eq!(body["error"]["code"], "1311");
        assert_eq!(body["request_id"], "202603112254417d15bd26697445b0");
        Ok(())
    }

    #[test]
    fn format_stream_error_frame_http_client_error_extracts_body_fields()
    -> Result<(), serde_json::Error> {
        // Mirrors TestFormatStreamError_HttpClientError (chat_test.go:376-393)
        // and TestWriteSSEStream_HttpClientError (chat_test.go:195-237):
        // provider body contributes message/type/code/request_id.
        let provider_body = json!({
            "error": {
                "message": "Rate limit exceeded",
                "type": "rate_limit_error",
            },
            "request_id": "req_42",
        });

        let err = ConduitError::upstream("upstream rate limited")
            .with_provider_status(429)
            .with_provider_body(provider_body);

        let body = frame_json(&err)?;

        // Message mirrors Go's ExtractErrorMessage (error.message from body).
        assert_eq!(body["error"]["message"], "Rate limit exceeded");
        // Extracted type/code/request_id come from the provider body.
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["request_id"], "req_42");
        // Go's test asserts code == "" when the upstream body omits it.
        assert_eq!(body["error"]["code"], "");
        Ok(())
    }

    #[test]
    fn format_stream_error_frame_response_error_empty_type_falls_back_to_server_error()
    -> Result<(), serde_json::Error> {
        // Mirrors Go chat.go:279-281: when `respErr.Detail.Type` is empty the
        // emitted `type` stays at the default "server_error".
        let resp_err = conduit_llm::model::ResponseError {
            status_code: 500,
            detail: conduit_llm::model::ErrorDetail {
                code: String::new(),
                message: "boom".to_string(),
                detail_type: String::new(),
                param: String::new(),
                request_id: String::new(),
            },
        };
        let err = ConduitError::upstream("boom").with_source(resp_err);

        let body = frame_json(&err)?;

        assert_eq!(body["error"]["type"], "server_error");
        assert_eq!(body["error"]["code"], "");
        // No request_id on the ResponseError and none in metadata -> omitted.
        assert!(body.get("request_id").is_none() || body["request_id"].is_null());
        Ok(())
    }

    #[test]
    fn format_stream_error_frame_hidden_policy_uses_safe_message() -> Result<(), Box<dyn Error>> {
        // Proves fix (b): extract_error_message must NOT leak provider text when
        // an UpstreamErrorPolicy of Hidden applies. hide_upstream_details()
        // clears provider_body, so the helper falls back to public_message()
        // (which yields the safe "Upstream provider error" string).
        let provider_body = json!({
            "error": {
                "message": "SECRET upstream trace must not leak",
                "type": "internal_error",
            },
        });

        let err = ConduitError::upstream("upstream failed")
            .with_provider_status(500)
            .with_provider_body(provider_body)
            .hide_upstream_details();

        let body = frame_json(&err)?;

        assert_eq!(body["error"]["message"], "Upstream provider error");
        // provider_body was cleared, so type/code/request_id also stay empty.
        assert_eq!(body["error"]["type"], "server_error");
        assert_eq!(body["error"]["code"], "");
        Ok(())
    }

    #[test]
    fn format_stream_error_frame_provider_code_wins_when_present() -> Result<(), serde_json::Error>
    {
        let provider_body = json!({
            "error": {
                "message": "Rate limit exceeded",
                "type": "rate_limit_error",
                "code": "provider_rate_limit",
            },
            "request_id": "req_123",
        });

        let err = ConduitError::upstream("rate limited").with_provider_body(provider_body);

        let body = frame_json(&err)?;

        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], "provider_rate_limit");
        assert_eq!(body["request_id"], "req_123");
        Ok(())
    }

    #[test]
    fn format_stream_error_frame_serializes_via_sse_frame_struct() -> Result<(), Box<dyn Error>> {
        // The SSE writer will call `serde_json::to_string` on the frame; verify
        // the shape is exactly the JSON Go emits.
        let err = ConduitError::quota_exhausted("all channels quota exhausted for model gpt-4");
        let serialized = serde_json::to_string(&format_stream_error_frame(&err))?;

        // Go marshals quota branch as {"error":{"message":..,"type":..,"code":..}}.
        let parsed: Value = serde_json::from_str(&serialized)?;

        assert_eq!(parsed["error"]["type"], "quota_exhausted");
        assert_eq!(parsed["error"]["code"], "quota_exhausted");
        assert!(
            parsed.get("request_id").is_none() || parsed["request_id"].is_null(),
            "quota branch must not emit request_id"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Coverage: ModelInclude fields=None variant is constructed at least once
    // in tests so the dead-code lint stays silent on the variant.
    // -----------------------------------------------------------------------

    #[test]
    fn model_include_none_variant_is_exercised() {
        let parsed = parse_model_include("", false);
        assert_eq!(parsed.fields, None);
        assert!(!parsed.need_full_data);
    }

    // -----------------------------------------------------------------------
    // S11: build_openai_models_response (model-list shaping, include applied)
    // -----------------------------------------------------------------------

    #[test]
    fn build_models_response_basic_facade_shape() -> Result<(), Box<dyn Error>> {
        // Mirrors TestOpenAIHandlers_ListModels_DefaultBasicFacade
        // (openai_retrieve_test.go:299-365): with no extended include, the
        // response carries only {object, id, owned_by, created}; capabilities,
        // pricing, modalities, name are absent.
        let parsed = parse_model_include("", false);
        let response = build_openai_models_response(
            [ModelRow::facade(
                "gpt-4.1",
                "openai",
                1712345698,
                "https://api.openai.com/v1",
            )],
            &parsed,
        );
        let body = serde_json::to_value(&response)?;

        assert_eq!(body["object"], "list");
        assert_eq!(body["data"][0]["object"], "model");
        assert_eq!(body["data"][0]["id"], "gpt-4.1");
        assert_eq!(body["data"][0]["owned_by"], "openai");
        assert_eq!(body["data"][0]["created"], 1712345698);
        // Extended fields must not be present (Go asserts Nil/Empty).
        assert!(
            body["data"][0].get("capabilities").is_none()
                || body["data"][0]["capabilities"].is_null()
        );
        assert!(body["data"][0].get("pricing").is_none() || body["data"][0]["pricing"].is_null());
        assert!(
            body["data"][0].get("modalities").is_none() || body["data"][0]["modalities"].is_null()
        );
        // `name` is omitempty in Go — absent on the facade path.
        assert!(body["data"][0].get("name").is_none() || body["data"][0]["name"].is_null());
        Ok(())
    }

    #[test]
    fn build_models_response_extended_all_fields() -> Result<(), Box<dyn Error>> {
        // Mirrors TestConvertModelToOpenAIExtended_CompleteData
        // (openai_model_test.go:39-76): include=nil selects every extended
        // field; capabilities/pricing/modalities/context_length/max_output_tokens
        // are populated from ModelCard.
        let parsed = parse_model_include("all", false);
        let response = build_openai_models_response(
            [ModelRow {
                id: "gpt-4".to_string(),
                owned_by: "openai".to_string(),
                created: 1686935002,
                name: Some("GPT-4".to_string()),
                description: Some("GPT-4 is a large multimodal model".to_string()),
                icon: Some("openai".to_string()),
                ty: Some("chat".to_string()),
                model_card: Some(ModelCard {
                    vision: true,
                    tool_call: true,
                    reasoning: ModelCardReasoning {
                        supported: true,
                        ..Default::default()
                    },
                    limit: ModelCardLimit {
                        context: 8192,
                        output: 4096,
                    },
                    cost: ModelCardCost {
                        input: 0.03,
                        output: 0.06,
                        cache_read: 0.015,
                        cache_write: 0.03,
                    },
                    modalities: ModelCardModalities {
                        input: vec!["text".into(), "image".into()],
                        output: vec!["text".into()],
                    },
                    ..Default::default()
                }),
                retail_pricing: Some(OpenAiPricing {
                    input: Some(300.0),
                    output: Some(600.0),
                    cache_read: Some(150.0),
                    cache_write: Some(300.0),
                    unit: "per_1m_tokens",
                    currency: "STATION_CREDIT".to_string(),
                    display_name: "Credits".to_string(),
                }),
            }],
            &parsed,
        );
        let body = serde_json::to_value(&response)?;
        let entry = &body["data"][0];

        assert_eq!(entry["id"], "gpt-4");
        assert_eq!(entry["name"], "GPT-4");
        assert_eq!(entry["description"], "GPT-4 is a large multimodal model");
        assert_eq!(entry["type"], "chat");
        assert_eq!(entry["icon"], "openai");
        assert_eq!(entry["capabilities"]["vision"], true);
        assert_eq!(entry["capabilities"]["tool_call"], true);
        assert_eq!(entry["capabilities"]["reasoning"], true);
        assert_eq!(entry["context_length"], 8192);
        assert_eq!(entry["max_output_tokens"], 4096);
        assert_eq!(entry["pricing"]["input"], 300.0);
        assert_eq!(entry["pricing"]["output"], 600.0);
        assert_eq!(entry["pricing"]["cache_read"], 150.0);
        assert_eq!(entry["pricing"]["cache_write"], 300.0);
        assert_eq!(entry["pricing"]["unit"], "per_1m_tokens");
        assert_eq!(entry["pricing"]["currency"], "STATION_CREDIT");
        assert_eq!(entry["pricing"]["display_name"], "Credits");
        assert_eq!(entry["modalities"]["input"][0], "text");
        assert_eq!(entry["modalities"]["input"][1], "image");
        assert_eq!(entry["modalities"]["output"][0], "text");
        Ok(())
    }

    #[test]
    fn build_models_response_extended_nil_model_card_keeps_basic_fields()
    -> Result<(), Box<dyn Error>> {
        // Mirrors TestConvertModelToOpenAIExtended_NilModelCard
        // (openai_model_test.go:13-37): when ModelCard is None the
        // capabilities/pricing/modalities/context_length/max_output_tokens
        // remain absent even on the extended path.
        let parsed = parse_model_include("all", false);
        let response = build_openai_models_response(
            [ModelRow {
                id: "gpt-4".to_string(),
                owned_by: "openai".to_string(),
                created: 1686935002,
                name: Some("GPT-4".to_string()),
                description: Some("Test description".to_string()),
                icon: Some("openai".to_string()),
                ty: Some("chat".to_string()),
                model_card: None,
                retail_pricing: None,
            }],
            &parsed,
        );
        let body = serde_json::to_value(&response)?;
        let entry = &body["data"][0];

        assert_eq!(entry["id"], "gpt-4");
        assert_eq!(entry["name"], "GPT-4");
        assert_eq!(entry["description"], "Test description");
        assert_eq!(entry["owned_by"], "openai");
        assert_eq!(entry["type"], "chat");
        assert_eq!(entry["icon"], "openai");
        assert_eq!(entry["created"], 1686935002);
        assert!(entry.get("capabilities").is_none() || entry["capabilities"].is_null());
        assert!(entry.get("pricing").is_none() || entry["pricing"].is_null());
        Ok(())
    }

    #[test]
    fn build_models_response_partial_include_selects_only_named_extended_fields()
    -> Result<(), Box<dyn Error>> {
        // Mirrors the shouldInclude closure (openai.go:571-576) exercised with
        // a partial include: with include=name,pricing only those extended
        // fields appear; the rest stay absent even though ModelCard is present.
        let parsed = parse_model_include("name,pricing", false);
        let response = build_openai_models_response(
            [ModelRow {
                id: "gpt-4".to_string(),
                owned_by: "openai".to_string(),
                created: 1686935002,
                name: Some("GPT-4".to_string()),
                description: Some("ignored".to_string()),
                icon: Some("openai".to_string()),
                ty: Some("chat".to_string()),
                model_card: Some(ModelCard {
                    vision: true,
                    tool_call: true,
                    reasoning: ModelCardReasoning {
                        supported: true,
                        ..Default::default()
                    },
                    limit: ModelCardLimit {
                        context: 8192,
                        output: 4096,
                    },
                    cost: ModelCardCost {
                        input: 0.03,
                        output: 0.06,
                        cache_read: 0.015,
                        cache_write: 0.03,
                    },
                    modalities: ModelCardModalities {
                        input: vec!["text".into()],
                        output: vec!["text".into()],
                    },
                    ..Default::default()
                }),
                retail_pricing: Some(OpenAiPricing {
                    input: Some(300.0),
                    output: Some(600.0),
                    cache_read: Some(150.0),
                    cache_write: Some(300.0),
                    unit: "per_1m_tokens",
                    currency: "STATION_CREDIT".to_string(),
                    display_name: "Credits".to_string(),
                }),
            }],
            &parsed,
        );
        let body = serde_json::to_value(&response)?;
        let entry = &body["data"][0];

        // Selected fields appear.
        assert_eq!(entry["name"], "GPT-4");
        assert_eq!(entry["pricing"]["input"], 300.0);
        // Unselected extended fields are absent.
        assert!(entry.get("description").is_none() || entry["description"].is_null());
        assert!(entry.get("capabilities").is_none() || entry["capabilities"].is_null());
        assert!(entry.get("modalities").is_none() || entry["modalities"].is_null());
        assert!(entry.get("context_length").is_none() || entry["context_length"].is_null());
        assert!(entry.get("max_output_tokens").is_none() || entry["max_output_tokens"].is_null());
        assert!(entry.get("icon").is_none() || entry["icon"].is_null());
        assert!(entry.get("type").is_none() || entry["type"].is_null());
        Ok(())
    }

    #[test]
    fn build_models_response_empty_input_empty_data_array() -> Result<(), Box<dyn Error>> {
        // Mirrors ListModels empty-models branch (openai.go:741-748): the
        // response is still a well-formed list envelope with `data: []`.
        let parsed = parse_model_include("", false);
        let response: OpenAiModelsResponse =
            build_openai_models_response(std::iter::empty::<ModelRow>(), &parsed);
        let body = serde_json::to_value(&response)?;

        assert_eq!(body["object"], "list");
        assert_eq!(body["data"], json!([]));
        Ok(())
    }

    #[test]
    fn build_models_response_single_basic_facade_preserves_order() -> Result<(), Box<dyn Error>> {
        // ListModels maps visibleModels in order (openai.go:751-753). Confirm
        // ordering and id-only output for the facade path.
        let parsed = parse_model_include("", false);
        let response = build_openai_models_response(
            [
                ModelRow::facade("a", "openai", 1, ""),
                ModelRow::facade("b", "anthropic", 2, ""),
                ModelRow::facade("c", "google", 3, ""),
            ],
            &parsed,
        );
        let body = serde_json::to_value(&response)?;

        assert_eq!(body["data"][0]["id"], "a");
        assert_eq!(body["data"][1]["id"], "b");
        assert_eq!(body["data"][2]["id"], "c");
        assert_eq!(body["data"][0]["owned_by"], "openai");
        assert_eq!(body["data"][2]["created"], 3);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // S10/S15: audio_response_content_type + binary stream header set
    // -----------------------------------------------------------------------

    #[test]
    fn audio_mode_classifies_audio_slash_prefix_as_binary() {
        // Mirrors StreamEvent.IsBinaryAudioChunk (httpclient/model.go:124-126):
        // any `audio/*` content type is binary.
        assert!(matches!(
            audio_response_content_type(Some("audio/mpeg")),
            AudioMode::Binary { .. }
        ));
        assert!(matches!(
            audio_response_content_type(Some("audio/wav")),
            AudioMode::Binary { .. }
        ));
        assert!(matches!(
            audio_response_content_type(Some("AUDIO/MP3")),
            AudioMode::Binary { .. }
        ));
    }

    #[test]
    fn audio_mode_classifies_octet_stream_as_binary() {
        // httpclient.IsBinaryAudioChunk also accepts application/octet-stream.
        assert!(matches!(
            audio_response_content_type(Some("application/octet-stream")),
            AudioMode::Binary { .. }
        ));
    }

    #[test]
    fn audio_mode_classifies_event_stream_as_sse() {
        // The non-binary path falls back to the SSE writer when the inbound
        // stream emits text/event-stream frames.
        assert_eq!(
            audio_response_content_type(Some("text/event-stream")),
            AudioMode::Sse
        );
        assert_eq!(
            audio_response_content_type(Some("text/event-stream; charset=utf-8")),
            AudioMode::Sse
        );
    }

    #[test]
    fn audio_mode_classifies_application_json_as_json() {
        assert_eq!(
            audio_response_content_type(Some("application/json")),
            AudioMode::Json
        );
    }

    #[test]
    fn audio_mode_defaults_to_json_for_empty_or_unrecognized() {
        // Mirrors WriteBinaryStream's content-type fallback when the first
        // event has no Type: the response is shaped as JSON rather than a raw
        // binary stream. (An empty first-event CT means the orchestrator did
        // not provide a binary payload, so the regular JSON envelope applies.)
        assert_eq!(audio_response_content_type(None), AudioMode::Json);
        assert_eq!(audio_response_content_type(Some("")), AudioMode::Json);
        assert_eq!(
            audio_response_content_type(Some("text/plain")),
            AudioMode::Json
        );
    }

    #[test]
    fn audio_response_content_type_for_binary_returns_first_event_ct() {
        // WriteBinaryStream (chat.go:219-221) uses the first event's Type as
        // the response Content-Type when non-empty. The selector surfaces it
        // via AudioMode::Binary::content_type so the handler can stamp headers.
        match audio_response_content_type(Some("audio/mpeg")) {
            AudioMode::Binary { content_type } => {
                assert_eq!(content_type, "audio/mpeg");
            }
            other => panic!("expected Binary, got {other:?}"),
        }
    }

    #[test]
    fn binary_stream_headers_sets_fixed_quartet() -> Result<(), Box<dyn Error>> {
        // Mirrors WriteBinaryStream (chat.go:223-226): Content-Type comes from
        // the first event (caller stamps it separately via the AudioMode), and
        // the writer sets exactly Cache-Control, Connection, Allow-Origin.
        let headers = binary_stream_headers();

        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .ok_or("cache-control missing")?,
            HeaderValue::from_static("no-cache")
        );
        assert_eq!(
            headers
                .get(header::CONNECTION)
                .ok_or("connection missing")?,
            HeaderValue::from_static("keep-alive")
        );
        assert_eq!(
            headers
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .ok_or("allow-origin missing")?,
            HeaderValue::from_static("*")
        );
        // Exactly the three companion headers — the Content-Type is supplied by
        // the caller from AudioMode::Binary::content_type.
        assert_eq!(headers.len(), 3);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // S14: trim_model_splat (Gin /v1/models/*model splat trimming)
    // -----------------------------------------------------------------------

    #[test]
    fn trim_model_splat_strips_single_leading_slash() {
        // Gin passes `/deepseek/deepseek-chat` for the splat on
        // /v1/models/deepseek/deepseek-chat. Go does
        // `strings.TrimPrefix(c.Param("model"), "/")` (openai.go:677).
        assert_eq!(
            trim_model_splat("/deepseek/deepseek-chat"),
            "deepseek/deepseek-chat"
        );
    }

    #[test]
    fn trim_model_splat_preserves_inner_slashes() {
        // Only the leading slash from the Gin catch-all is stripped; inner
        // model-name segments (vendor/model) survive intact.
        assert_eq!(
            trim_model_splat("/anthropic/claude-3-5-sonnet"),
            "anthropic/claude-3-5-sonnet"
        );
    }

    #[test]
    fn trim_model_splat_no_leading_slash_is_identity() {
        // Gin can also yield the splat without the leading slash depending on
        // the route shape; the trim must be a no-op then.
        assert_eq!(trim_model_splat("gpt-4o-mini"), "gpt-4o-mini");
    }

    #[test]
    fn trim_model_splat_strips_only_one_leading_slash() {
        // strings.TrimPrefix strips at most one occurrence; a double-leading
        // slash keeps the second one.
        assert_eq!(trim_model_splat("//weird"), "/weird");
    }

    #[test]
    fn trim_model_splat_empty_input_returns_empty() {
        // Empty splat — RetrieveModel writes a model_not_found envelope
        // upstream; the trim itself must just return the empty input.
        assert_eq!(trim_model_splat(""), "");
    }

    // -----------------------------------------------------------------------
    // S14: model-list/retrieve OpenAI object/list format parity
    // (gap tests — list envelope is covered above; these exercise the
    //  single-row retrieve shape and the object-tag invariants Go's
    //  RetrieveModel/ListModels openai.go:671-788 enforce.)
    // -----------------------------------------------------------------------

    // Mirrors Go `RetrieveModel` (openai.go:699-701): a single model is
    // returned as a bare `OpenAIModel` JSON object — NOT wrapped in a list
    // envelope. The Rust `build_openai_model_entry` produces that exact bare
    // shape; verify it serializes without the `object: "list"` wrapper and
    // carries the Go-basic field set (id/object/created/owned_by).
    #[test]
    fn s14_retrieve_single_row_is_bare_model_not_list_envelope() -> Result<(), Box<dyn Error>> {
        let parsed = parse_model_include("", false);
        let row = ModelRow::facade("gpt-4o", "openai", 1_700_000_000, "");
        let entry = build_openai_model_entry(&row, &parsed);
        let body = serde_json::to_value(&entry)?;

        // Bare object, no list wrapper.
        assert_eq!(body["object"], "model");
        assert!(body.get("data").is_none());
        // Go-basic fields (openai.go:490-494) always present.
        assert_eq!(body["id"], "gpt-4o");
        assert_eq!(body["owned_by"], "openai");
        assert_eq!(body["created"], 1_700_000_000);
        // Extended fields absent on the facade path (omitempty).
        assert!(body.get("name").is_none());
        assert!(body.get("context_length").is_none());
        Ok(())
    }

    // Mirrors Go `RetrieveModel` extended branch (openai.go:704-720): when
    // `needFullData` is true and a configured model row exists, the bare
    // retrieve object still returns a single model object — the extended
    // fields appear inline, but the list wrapper must NOT appear. This is
    // the parity invariant the coordinator flagged: retrieve output must
    // keep the OpenAI `object: "model"` format, not be promoted to a list.
    #[test]
    fn s14_retrieve_extended_row_still_bare_model_with_inline_fields() -> Result<(), Box<dyn Error>>
    {
        use conduit_core::objects::model::{
            ModelCard, ModelCardCost, ModelCardLimit, ModelCardModalities,
        };

        let parsed = parse_model_include("all", false);
        let row = ModelRow {
            id: "gpt-4.1".to_string(),
            owned_by: "openai".to_string(),
            created: 1_700_000_000,
            name: Some("GPT-4.1".to_string()),
            description: None,
            icon: None,
            ty: None,
            model_card: Some(ModelCard {
                limit: ModelCardLimit {
                    context: 128_000,
                    output: 16_384,
                },
                modalities: ModelCardModalities {
                    input: vec!["text".to_string()],
                    output: vec!["text".to_string()],
                },
                cost: ModelCardCost {
                    input: 1.0,
                    output: 2.0,
                    ..Default::default()
                },
                ..Default::default()
            }),
            retail_pricing: Some(OpenAiPricing {
                input: Some(10_000.0),
                output: Some(20_000.0),
                cache_read: None,
                cache_write: None,
                unit: "per_1m_tokens",
                currency: "STATION_CREDIT".to_string(),
                display_name: "Credits".to_string(),
            }),
        };
        let entry = build_openai_model_entry(&row, &parsed);
        let body = serde_json::to_value(&entry)?;

        // Still a bare model object, NOT a list envelope.
        assert_eq!(body["object"], "model");
        assert!(body.get("data").is_none());
        // Extended fields present inline (openai.go:490-503 omitempty tags).
        assert_eq!(body["name"], "GPT-4.1");
        assert_eq!(body["context_length"], 128_000);
        assert_eq!(body["max_output_tokens"], 16_384);
        assert_eq!(body["pricing"]["input"], 10_000.0);
        Ok(())
    }

    // Mirrors Go `ListModels` (openai.go:784-787): the list envelope always
    // sets `object: "list"` regardless of payload size, and each entry
    // carries `object: "model"`. This invariant is implicit in the existing
    // list tests but not asserted as a cross-cutting contract — pin it so a
    // future refactor cannot silently swap the object tags (e.g. to
    // "model_list" or drop them).
    #[test]
    fn s14_list_envelope_object_tags_are_openai_canonical() -> Result<(), Box<dyn Error>> {
        let parsed = parse_model_include("", false);
        let response = build_openai_models_response(
            [
                ModelRow::facade("a", "openai", 1, ""),
                ModelRow::facade("b", "anthropic", 2, ""),
            ],
            &parsed,
        );
        let body = serde_json::to_value(&response)?;

        // Envelope is exactly OpenAI's `{object: "list", data: [...]}`.
        assert_eq!(body["object"], "list");
        assert_eq!(body["data"][0]["object"], "model");
        assert_eq!(body["data"][1]["object"], "model");
        // No stray top-level model fields leak onto the envelope.
        assert!(body.get("id").is_none());
        assert!(body.get("owned_by").is_none());
        Ok(())
    }

    // Cross-check the two parallel shapers (basic stub `openai_model_list_response`
    // vs. include-aware `build_openai_models_response`) agree on the OpenAI
    // envelope contract: both emit `object: "list"` with `object: "model"`
    // entries. This guards the S14 invariant that the list/retrieve output
    // format is uniform across the stub and full implementations.
    #[test]
    fn s14_stub_and_full_list_shapers_emit_same_openai_envelope() -> Result<(), Box<dyn Error>> {
        let stub = openai_model_list_response([ModelSummary::new("gpt-4o", "openai")]);
        let stub_body = serde_json::to_value(&stub)?;

        let parsed = parse_model_include("", false);
        let full =
            build_openai_models_response([ModelRow::facade("gpt-4o", "openai", 0, "")], &parsed);
        let full_body = serde_json::to_value(&full)?;

        // Both envelopes carry the OpenAI canonical object tags.
        assert_eq!(stub_body["object"], "list");
        assert_eq!(full_body["object"], "list");
        assert_eq!(stub_body["data"][0]["object"], "model");
        assert_eq!(full_body["data"][0]["object"], "model");
        // The stub omits `created` (dead-code path behind not-implemented);
        // the full shaper includes it. Both are valid OpenAI responses.
        assert!(stub_body["data"][0].get("created").is_none());
        assert!(full_body["data"][0].get("created").is_some());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // S07/S15-speech-route: resolve_response_content_type (non-stream CT fallback)
    // -----------------------------------------------------------------------

    #[test]
    fn s07_resolve_response_content_type_none_falls_back_to_json() {
        // Mirrors chat.go:87-90: an orchestrator result without a Content-Type
        // header is written as application/json.
        assert_eq!(
            resolve_response_content_type(None),
            DEFAULT_NONSTREAM_CONTENT_TYPE
        );
        assert_eq!(
            resolve_response_content_type(Some("")),
            DEFAULT_NONSTREAM_CONTENT_TYPE
        );
    }

    #[test]
    fn s07_resolve_response_content_type_passes_through_nonempty() {
        // chat.go:88-89: a non-empty Content-Type from the orchestrator wins.
        assert_eq!(
            resolve_response_content_type(Some("audio/mpeg")),
            "audio/mpeg"
        );
        assert_eq!(
            resolve_response_content_type(Some("application/json; charset=utf-8")),
            "application/json; charset=utf-8"
        );
        assert_eq!(
            resolve_response_content_type(Some("text/event-stream")),
            "text/event-stream"
        );
    }

    // -----------------------------------------------------------------------
    // S15-speech-route: should_use_binary_speech_stream
    // Mirrors Go's TestShouldUseBinarySpeechStream (openai_speech_test.go:77-131).
    // -----------------------------------------------------------------------

    #[test]
    fn s15_speech_route_audio_stream_format_selects_binary() -> Result<(), Box<dyn Error>> {
        // Mirrors `audio stream format` case: application/json +
        // `{"stream_format":"audio"}` -> true.
        let body = br#"{"stream_format":"audio"}"#;
        assert!(should_use_binary_speech_stream(
            Some(body),
            Some("application/json")
        )?);
        Ok(())
    }

    #[test]
    fn s15_speech_route_sse_stream_format_does_not_select_binary() -> Result<(), Box<dyn Error>> {
        // Mirrors `sse stream format` case: stream_format=="sse" -> false
        // (the regular SSE writer is used).
        let body = br#"{"stream_format":"sse"}"#;
        assert!(!should_use_binary_speech_stream(
            Some(body),
            Some("application/json")
        )?);
        Ok(())
    }

    #[test]
    fn s15_speech_route_default_non_stream_does_not_select_binary() -> Result<(), Box<dyn Error>> {
        // Mirrors `default non-stream` case: missing stream_format -> false.
        let body = b"{}";
        assert!(!should_use_binary_speech_stream(
            Some(body),
            Some("application/json")
        )?);
        Ok(())
    }

    #[test]
    fn s15_speech_route_non_json_content_type_defers_to_transformer() -> Result<(), Box<dyn Error>>
    {
        // Mirrors `non-json content type lets transformer report validation`:
        // even with stream_format:"audio", a multipart/form-data Content-Type
        // short-circuits to Ok(false) — the transformer is expected to surface
        // the validation error itself.
        let body = br#"{"stream_format":"audio"}"#;
        assert!(!should_use_binary_speech_stream(
            Some(body),
            Some("multipart/form-data")
        )?);
        Ok(())
    }

    #[test]
    fn s15_speech_route_invalid_json_returns_invalid_request() {
        // Mirrors `invalid json` case: malformed JSON body -> error.
        match should_use_binary_speech_stream(Some(b"{"), Some("application/json")) {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::InvalidRequest);
                assert_eq!(err.http_status, 400);
            }
            Ok(_) => panic!("malformed JSON must be rejected"),
        }
    }

    // -----------------------------------------------------------------------
    // S15-speech-route: extra parity tests beyond the Go golden — cover the
    // nil-request guard (openai.go:339-341) and empty-body guard
    // (openai.go:343-345) that the Go test does not exercise but the contract
    // requires, plus the lowercase/trim normalization.
    // -----------------------------------------------------------------------

    #[test]
    fn s15_speech_route_missing_request_is_invalid_request() {
        // openai.go:339-341: nil http request -> error. In Rust we represent
        // the missing body as None.
        match should_use_binary_speech_stream(None, Some("application/json")) {
            Err(err) => assert_eq!(err.kind, ErrorKind::InvalidRequest),
            Ok(_) => panic!("missing request must be rejected"),
        }
    }

    #[test]
    fn s15_speech_route_empty_body_is_invalid_request() {
        // openai.go:343-345: empty body -> error.
        match should_use_binary_speech_stream(Some(b""), Some("application/json")) {
            Err(err) => assert_eq!(err.kind, ErrorKind::InvalidRequest),
            Ok(_) => panic!("empty body must be rejected"),
        }
    }

    #[test]
    fn s15_speech_route_uppercase_stream_format_is_lowercased() -> Result<(), Box<dyn Error>> {
        // openai.go:357: `strings.ToLower(strings.TrimSpace(body.StreamFormat))`
        // normalizes the value before comparison; "AUDIO" must select the
        // binary path just like "audio".
        let body = br#"{"stream_format":"  AUDIO  "}"#;
        assert!(should_use_binary_speech_stream(
            Some(body),
            Some("application/json")
        )?);
        // "SSE" must map to false the same way "sse" does.
        let body_sse = br#"{"stream_format":"SSE"}"#;
        assert!(!should_use_binary_speech_stream(
            Some(body_sse),
            Some("application/json")
        )?);
        Ok(())
    }

    #[test]
    fn s15_speech_route_missing_content_type_still_decodes_body() -> Result<(), Box<dyn Error>> {
        // openai.go:347-350: empty Content-Type is treated the same as
        // application/json (the `!= ""` guard skips the non-JSON short-circuit).
        let body = br#"{"stream_format":"audio"}"#;
        assert!(should_use_binary_speech_stream(Some(body), None)?);
        assert!(should_use_binary_speech_stream(Some(body), Some(""))?);
        Ok(())
    }

    #[test]
    fn s15_speech_route_unknown_stream_format_selects_binary() -> Result<(), Box<dyn Error>> {
        // Any non-empty, non-"sse" value selects the binary path — the Go
        // contract is "stream_format != '' && stream_format != 'sse'".
        let body = br#"{"stream_format":"mp3"}"#;
        assert!(should_use_binary_speech_stream(
            Some(body),
            Some("application/json")
        )?);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // RUST-P11-001 MAP-01 — main handler chain (chat/responses/embeddings)
    // -----------------------------------------------------------------------

    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, header};
    use axum::routing::post;
    use conduit_config::AppConfig;
    use conduit_llm::model::HttpRequest;
    use tower::Service;

    use crate::app_state::AppServices;
    use crate::middleware::api_key_auth::{
        ApiKeyValidationError, ApiKeyValidationService, ValidatedApiKeyMetadata,
    };
    use crate::router::build_router;

    /// Permissive API-key validator for handler/routing tests. `api_key_auth`
    /// now fails closed when no validator is wired (P-24), so tests that need to
    /// exercise a handler *past* the auth layer must supply one. It accepts any
    /// non-empty key and returns default metadata (the handlers under test don't
    /// depend on the metadata contents).
    struct AlwaysValidApiKey;

    #[async_trait::async_trait]
    impl ApiKeyValidationService for AlwaysValidApiKey {
        async fn validate(
            &self,
            _plaintext_key: &str,
        ) -> Result<ValidatedApiKeyMetadata, ApiKeyValidationError> {
            Ok(ValidatedApiKeyMetadata::default())
        }
    }

    /// `AppServices` pre-wired with the permissive test validator. Handler tests
    /// chain their own service (`.with_openai_orchestrator(...)` etc.) onto this.
    fn test_services() -> AppServices {
        AppServices::new().with_api_key_validation_service(Arc::new(AlwaysValidApiKey))
    }

    /// `AppState` with only the permissive test validator wired — the analogue of
    /// `AppState::default()` for the fail-closed auth layer.
    fn test_state() -> crate::app_state::AppState {
        crate::app_state::AppState::new(Arc::new(AppConfig::default()), Arc::new(test_services()))
    }

    /// In-memory orchestrator that records the command it received and
    /// returns a canned OpenAI-shaped response. Stands in for the real
    /// `ChatCompletionOrchestrator.Process` flow the host binary wires.
    ///
    /// The response is held in a `Mutex<Option<...>>` rather than cloned on
    /// each call: [`OpenAiHandlerOutput`] carries [`ConduitError`] frames inside
    /// its `Stream` variant, and `ConduitError` is not `Clone` (it owns a
    /// `Box<dyn Error>` source). Each test call takes the response exactly
    /// once; a missing response degrades to an empty JSON body, matching how
    /// Go's `Process` returning a zero-value `*httpclient.Response` renders.
    struct FakeOpenAiOrchestrator {
        seen: Mutex<Vec<(OpenAiRoute, HttpRequest)>>,
        response: Mutex<Option<OpenAiHandlerOutput>>,
        // When set, the service emits this error kind instead of `response`.
        fail_quota_exhausted: bool,
        fail_message: String,
    }

    #[async_trait::async_trait]
    impl OpenAiOrchestratorService for FakeOpenAiOrchestrator {
        async fn process(
            &self,
            route: OpenAiRoute,
            request: HttpRequest,
        ) -> Result<OpenAiHandlerOutput, ConduitError> {
            if let Ok(mut guard) = self.seen.lock() {
                guard.push((route, request));
            }
            if self.fail_quota_exhausted {
                return Err(ConduitError::quota_exhausted(self.fail_message.clone()));
            }
            let response = match self.response.lock() {
                Ok(mut guard) => guard.take(),
                Err(_) => None,
            };
            Ok(response.unwrap_or_else(|| {
                OpenAiHandlerOutput::NonStream(OpenAiHandlerResponse::ok_json(b"{}".to_vec()))
            }))
        }
    }

    fn app_with_orchestrator(orchestrator: Arc<FakeOpenAiOrchestrator>) -> Router {
        let services = test_services().with_openai_orchestrator(orchestrator);
        build_router(crate::app_state::AppState::new(
            Arc::new(AppConfig::default()),
            Arc::new(services),
        ))
    }

    fn fake_response(body: &'static str) -> Mutex<Option<OpenAiHandlerOutput>> {
        Mutex::new(Some(OpenAiHandlerOutput::NonStream(
            OpenAiHandlerResponse {
                status: 200,
                content_type: Some("application/json".to_string()),
                body: body.as_bytes().to_vec(),
            },
        )))
    }

    fn compress_request(encoding: &str, body: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
        use std::io::Write;

        match encoding {
            "gzip" => {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(body)?;
                Ok(encoder.finish()?)
            }
            "deflate" => {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(body)?;
                Ok(encoder.finish()?)
            }
            "zstd" => Ok(zstd::stream::encode_all(Cursor::new(body), 1)?),
            _ => Err("unsupported test encoding".into()),
        }
    }

    async fn call_openai(
        app: &mut Router,
        method: Method,
        uri: &str,
        body: &'static str,
    ) -> Result<(axum::http::StatusCode, Value), Box<dyn Error>> {
        // routes.go apiGroup wraps every LLM route in middleware.WithAPIKeyConfig; the
        // Rust router mirrors this with api_key_auth (route_layer). Supply a bearer key
        // so the middleware passes and we exercise the handler logic. The middleware
        // only extracts the key (no DB validation), so any non-empty value works.
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))?;
        let response = app.call(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    #[tokio::test]
    async fn compressed_requests_are_decoded_and_stale_headers_are_removed()
    -> Result<(), Box<dyn Error>> {
        let original = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#;

        for encoding in ["gzip", "deflate", "zstd"] {
            let orchestrator = Arc::new(FakeOpenAiOrchestrator {
                seen: Mutex::new(Vec::new()),
                response: fake_response(r#"{"ok":true}"#),
                fail_quota_exhausted: false,
                fail_message: String::new(),
            });
            let mut app = app_with_orchestrator(Arc::clone(&orchestrator));
            let encoded = compress_request(encoding, original)?;
            let response = app
                .call(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/v1/chat/completions")
                        .header(header::AUTHORIZATION, "Bearer test-api-key")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::CONTENT_ENCODING, encoding)
                        .header(header::CONTENT_LENGTH, encoded.len())
                        .body(Body::from(encoded))?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::OK, "encoding {encoding}");

            let seen = orchestrator
                .seen
                .lock()
                .map_err(|error| error.to_string())?;
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].1.body.as_deref(), Some(&original[..]));
            assert!(!seen[0].1.headers.contains_key("content-encoding"));
            assert!(!seen[0].1.headers.contains_key("content-length"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn decompression_ratio_limit_rejects_compression_bombs_before_dispatch()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"ok":true}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(Arc::clone(&orchestrator));
        let encoded = compress_request("gzip", &vec![b'a'; 256 * 1024])?;
        let response = app
            .call(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer test-api-key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CONTENT_ENCODING, "gzip")
                    .body(Body::from(encoded))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
        assert_eq!(body["error"]["type"], "invalid_request");
        assert!(
            orchestrator
                .seen
                .lock()
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn corrupt_and_chained_content_encodings_fail_with_protocol_errors()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"ok":true}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut encoded = compress_request("gzip", br#"{"model":"gpt-4o"}"#)?;
        let last = encoded.last_mut().ok_or("gzip fixture must not be empty")?;
        *last ^= 0xff;

        for (content_encoding, body, expected) in [
            ("gzip", encoded, StatusCode::BAD_REQUEST),
            (
                "gzip, deflate",
                compress_request("gzip", br#"{"model":"gpt-4o"}"#)?,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
        ] {
            let mut app = app_with_orchestrator(Arc::clone(&orchestrator));
            let response = app
                .call(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/v1/chat/completions")
                        .header(header::AUTHORIZATION, "Bearer test-api-key")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::CONTENT_ENCODING, content_encoding)
                        .body(Body::from(body))?,
                )
                .await?;
            assert_eq!(response.status(), expected);
        }
        assert!(
            orchestrator
                .seen
                .lock()
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        Ok(())
    }

    // ---- happy path: chat / responses / embeddings ----------------------

    #[tokio::test]
    async fn anthropic_count_tokens_forwards_to_dedicated_route() -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"input_tokens":17}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(Arc::clone(&orchestrator));
        let (status, response) = call_openai(
            &mut app,
            Method::POST,
            "/v1/messages/count_tokens",
            r#"{"model":"claude-3-5-sonnet-latest","messages":[{"role":"user","content":"hello"}]}"#,
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response, json!({"input_tokens": 17}));

        let seen = orchestrator
            .seen
            .lock()
            .map_err(|_| "orchestrator seen lock poisoned")?;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, OpenAiRoute::AnthropicCountTokens);
        assert_eq!(seen[0].1.path, "/v1/messages/count_tokens");
        Ok(())
    }

    /// Mirrors Go `OpenAIHandlers.ChatCompletion` happy path (openai.go:292 +
    /// chat.go:84-95): orchestrator returns a 200 JSON body and the handler
    /// forwards it verbatim with the orchestrator's Content-Type.
    #[tokio::test]
    async fn chat_completion_happy_path_forwards_orchestrator_response()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"id":"chatcmpl-1","object":"chat.completion"}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let (status, body) = call_openai(
            &mut app,
            Method::POST,
            "/v1/chat/completions",
            r#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["id"], "chatcmpl-1");
        assert_eq!(body["object"], "chat.completion");

        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (route, request) = &seen[0];
        assert_eq!(*route, OpenAiRoute::ChatCompletions);
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(request.method, "POST");
        // Body bytes preserved verbatim (chat.go:53-60 + utils.go:45).
        let request_body = request.body.as_ref().ok_or("body missing")?;
        assert!(std::str::from_utf8(request_body)?.contains("gpt-4o-mini"));
        // Route tag flows through metadata so the host picks the right
        // inbound transformer (openai.go:78-94).
        assert_eq!(request.metadata["openai_route"], "/v1/chat/completions");
        Ok(())
    }

    #[tokio::test]
    async fn chat_completion_stamps_trace_and_thread_metadata() -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"id":"chatcmpl-1","object":"chat.completion"}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let request = Request::builder()
            .method(Method::POST)
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .header(header::CONTENT_TYPE, "application/json")
            .header("Conduit-Trace-Id", "trace-cache-1")
            .header("Conduit-Thread-Id", "thread-cache-1")
            .body(Body::from(
                r#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}"#,
            ))?;

        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let _ = to_bytes(response.into_body(), 64 * 1024).await?;

        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let metadata = &seen[0].1.metadata;
        assert_eq!(
            metadata.get("trace_key"),
            Some(&serde_json::Value::from("trace-cache-1")),
        );
        assert_eq!(
            metadata.get("thread_key"),
            Some(&serde_json::Value::from("thread-cache-1")),
        );
        assert_eq!(
            metadata.get("session_id"),
            Some(&serde_json::Value::from("trace-cache-1")),
        );
        Ok(())
    }

    /// Mirrors Go `OpenAIHandlers.CreateResponse` (openai.go:300-302): same
    /// thin-wrapper flow with the responses-flavored inbound transformer
    /// selected via the route tag.
    #[tokio::test]
    async fn create_response_happy_path_forwards_orchestrator_response()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"id":"resp_1","object":"response"}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let (status, body) = call_openai(
            &mut app,
            Method::POST,
            "/v1/responses",
            r#"{"model":"gpt-4o-mini","input":"hi"}"#,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["id"], "resp_1");

        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (route, request) = &seen[0];
        assert_eq!(*route, OpenAiRoute::Responses);
        assert_eq!(request.metadata["openai_route"], "/v1/responses");
        Ok(())
    }

    /// Mirrors Go `OpenAIHandlers.CreateEmbedding` (openai.go:308-310).
    #[tokio::test]
    async fn create_embedding_happy_path_forwards_orchestrator_response()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"object":"list","data":[]}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let (status, body) = call_openai(
            &mut app,
            Method::POST,
            "/v1/embeddings",
            r#"{"model":"text-embedding-3-small","input":"hi"}"#,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["object"], "list");

        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (route, request) = &seen[0];
        assert_eq!(*route, OpenAiRoute::Embeddings);
        assert_eq!(request.metadata["openai_route"], "/v1/embeddings");
        Ok(())
    }

    // ---- error path: empty body + orchestrator failure ------------------

    /// Empty body -> 400 invalid_request (chat.go:67-70 — JSONError, here
    /// rendered through the OpenAI-compatible error envelope).
    #[tokio::test]
    async fn openai_handlers_reject_empty_body_with_invalid_request() -> Result<(), Box<dyn Error>>
    {
        for path in ["/v1/chat/completions", "/v1/responses", "/v1/embeddings"] {
            let orchestrator = Arc::new(FakeOpenAiOrchestrator {
                seen: Mutex::new(Vec::new()),
                response: fake_response("{}"),
                fail_quota_exhausted: false,
                fail_message: String::new(),
            });
            let mut app = app_with_orchestrator(orchestrator.clone());

            let request = Request::builder()
                .header(header::AUTHORIZATION, "Bearer test-api-key")
                .method(Method::POST)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::empty())?;
            let response = app.call(request).await?;
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 4096).await?;
            let body: Value = serde_json::from_slice(&bytes)?;

            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(body["error"]["message"], "Request body is empty", "{path}");
            // OpenAI-compatible envelope shape from openai_error_json.
            assert_eq!(body["error"]["type"], "invalid_request", "{path}");

            // The orchestrator MUST NOT be called when the body is empty.
            let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
            assert!(seen.is_empty(), "{path}: orchestrator not called");
        }
        Ok(())
    }

    /// Orchestrator failure -> OpenAI-compatible error envelope, status from
    /// the ConduitError kind (chat.go:78-81 + transformOrchestratorError).
    #[tokio::test]
    async fn openai_handlers_render_orchestrator_error_as_openai_envelope()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response("{}"),
            fail_quota_exhausted: true,
            fail_message: "all channels exhausted".to_string(),
        });
        let mut app = app_with_orchestrator(orchestrator);

        let (status, body) = call_openai(
            &mut app,
            Method::POST,
            "/v1/chat/completions",
            r#"{"model":"gpt-4o"}"#,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            body["error"]["message"], "all channels exhausted",
            "QuotaExhausted public_message flows through (chat.go:78-81)"
        );
        assert_eq!(body["error"]["type"], "quota_exhausted");
        Ok(())
    }

    /// Rust-only skeleton path: no orchestrator wired degrades to the same
    /// internal-error branch Go hits on a Process failure.
    #[tokio::test]
    async fn openai_handlers_unwired_service_returns_internal_error() -> Result<(), Box<dyn Error>>
    {
        let mut app = build_router(test_state());

        let (status, body) = call_openai(
            &mut app,
            Method::POST,
            "/v1/chat/completions",
            r#"{"model":"gpt-4o"}"#,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["type"], "internal_error");
        Ok(())
    }

    // ---- pure helpers: route tag + query/header materialisation ---------

    #[test]
    fn openai_route_path_matches_go_canonical() {
        // Mirrors Go routes.go:170,173,176 — the orchestrator host dispatches
        // the inbound transformer off this path tag.
        assert_eq!(OpenAiRoute::ChatCompletions.path(), "/v1/chat/completions");
        assert_eq!(OpenAiRoute::Responses.path(), "/v1/responses");
        assert_eq!(OpenAiRoute::Embeddings.path(), "/v1/embeddings");
    }

    /// Content-Type fallback parity with chat.go:86-90: a missing CT on the
    /// orchestrator response falls back to `application/json`.
    #[tokio::test]
    async fn openai_handlers_default_content_type_when_orchestrator_omits_it()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(OpenAiHandlerOutput::NonStream(
                OpenAiHandlerResponse {
                    status: 200,
                    content_type: None,
                    body: br#"{"ok":true}"#.to_vec(),
                },
            ))),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/chat/completions")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"model":"gpt-4o"}"#))?;
        let response = app.call(request).await?;

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let ct = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .ok_or("content-type missing")?;
        assert_eq!(ct, "application/json");
        Ok(())
    }

    /// Query parameters are decoded into the multi-valued map the same way
    /// Go's `url.URL.Query()` does (utils.go:24).
    #[test]
    fn urlencoding_decode_query_handles_multiple_values() {
        let pairs = urlencoding_decode_query("a=1&b=two&a=2&c");
        let mut grouped: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        for (k, v) in &pairs {
            grouped.entry(k.as_str()).or_default().push(v.as_str());
        }
        assert_eq!(grouped["a"], vec!["1", "2"]);
        assert_eq!(grouped["b"], vec!["two"]);
        assert_eq!(grouped["c"], vec![""]);
    }

    #[test]
    fn percent_decode_handles_percent_encoded_bytes() {
        assert_eq!(percent_decode("hi"), "hi");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("%2Fpath"), "/path");
        // Invalid hex stays verbatim (Go's url.QueryUnescape replaces with
        // the original bytes when decoding fails — this implementation keeps
        // the percent literal, which is acceptable for our bounded scope).
        assert_eq!(percent_decode("no-op"), "no-op");
    }

    // -----------------------------------------------------------------------
    // RUST-P11-001 — streaming branch tests (chat.go:97-115)
    // -----------------------------------------------------------------------

    #[test]
    fn write_sse_stream_body_renders_event_and_data_lines() {
        // Mirrors gin-contrib/sse SSEvent wire format:
        //   event:<type>\ndata:<data>\n\n
        let events = vec![
            Ok(StreamEvent::new("chat.completion.chunk", r#"{"id":"x"}"#)),
            Ok(StreamEvent::new("chat.completion.chunk", "[DONE]")),
        ];
        let body = write_sse_stream_body(OpenAiRoute::ChatCompletions, &events);
        assert_eq!(
            body,
            "event:chat.completion.chunk\ndata:{\"id\":\"x\"}\n\n\
             event:chat.completion.chunk\ndata:[DONE]\n\n"
        );
    }

    #[test]
    fn write_sse_stream_body_renders_empty_event_as_data_only() {
        // gin-contrib/sse: when SSEvent.Event == "" the event: line is omitted.
        let events = vec![Ok(StreamEvent::new("", "{\"ping\":true}"))];
        let body = write_sse_stream_body(OpenAiRoute::ChatCompletions, &events);
        assert_eq!(body, "data:{\"ping\":true}\n\n");
    }

    #[test]
    fn write_sse_stream_body_renders_error_frame_via_format_stream_error_frame()
    -> Result<(), Box<dyn Error>> {
        // Mirrors chat.go:164 — `c.SSEvent("error", formatErr(ctx, err))`.
        let events: Vec<Result<StreamEvent, ConduitError>> = vec![Err(
            ConduitError::quota_exhausted("all channels exhausted for model gpt-4"),
        )];
        let body = write_sse_stream_body(OpenAiRoute::ChatCompletions, &events);

        // The error frame is emitted as event:error + data:<json>.
        assert!(body.starts_with("event:error\ndata:"));
        // The data line is the JSON-serialized StreamErrorFrame.
        let data_json = body
            .strip_prefix("event:error\ndata:")
            .and_then(|rest| rest.strip_suffix("\n\n"))
            .ok_or("missing framing")?;
        let parsed: Value = serde_json::from_str(data_json)?;
        assert_eq!(parsed["error"]["type"], "quota_exhausted");
        assert_eq!(parsed["error"]["code"], "quota_exhausted");
        Ok(())
    }

    #[test]
    fn write_sse_stream_body_uses_channel_custom_error_body() -> Result<(), Box<dyn Error>> {
        let custom_body = json!({
            "error": {
                "message": "channel unavailable",
                "code": "channel_unavailable"
            }
        });
        let events = vec![Err(ConduitError::upstream("provider secret")
            .with_metadata(
                conduit_core::ERROR_RESPONSE_BODY_METADATA,
                custom_body.clone(),
            ))];
        let body = write_sse_stream_body(OpenAiRoute::ChatCompletions, &events);
        let data = body
            .strip_prefix("event:error\ndata:")
            .and_then(|rest| rest.strip_suffix("\n\n"))
            .ok_or("missing custom error framing")?;

        assert_eq!(serde_json::from_str::<Value>(data)?, custom_body);
        Ok(())
    }

    #[test]
    fn write_sse_stream_body_prefixes_every_multiline_data_field() {
        let events = vec![Ok(StreamEvent::new("message", "first\nsecond\n"))];
        let body = write_sse_stream_body(OpenAiRoute::ChatCompletions, &events);
        assert_eq!(body, "event:message\ndata:first\ndata:second\ndata:\n\n");
    }

    #[test]
    fn anthropic_stream_error_uses_official_error_event_envelope() -> Result<(), Box<dyn Error>> {
        let events = vec![Err(ConduitError::forbidden("denied"))];
        let body = write_sse_stream_body(OpenAiRoute::AnthropicMessages, &events);
        let data = body
            .strip_prefix("event:error\ndata:")
            .and_then(|rest| rest.strip_suffix("\n\n"))
            .ok_or("missing Anthropic error framing")?;
        let value: Value = serde_json::from_str(data)?;
        assert_eq!(value["type"], "error");
        assert_eq!(value["error"]["type"], "permission_error");
        Ok(())
    }

    #[tokio::test]
    async fn gemini_stream_defaults_to_json_array_and_alt_sse_selects_sse()
    -> Result<(), Box<dyn Error>> {
        let events = || vec![Ok(StreamEvent::new("", r#"{"candidates":[{"index":0}]}"#))];
        let response = materialise_openai_output(
            OpenAiRoute::GeminiGenerateContent,
            &Uri::from_static("/v1beta/models/gemini:streamGenerateContent"),
            OpenAiHandlerOutput::Stream(events()),
        );
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            crate::gemini_handlers::GEMINI_JSON_STREAM_CONTENT_TYPE
        );
        let body = to_bytes(response.into_body(), 4096).await?;
        assert_eq!(body.as_ref(), br#"[{"candidates":[{"index":0}]}]"#);

        let response = materialise_openai_output(
            OpenAiRoute::GeminiGenerateContent,
            &Uri::from_static("/v1beta/models/gemini:streamGenerateContent?alt=sse"),
            OpenAiHandlerOutput::Stream(events()),
        );
        assert_eq!(response.headers()[header::CONTENT_TYPE], SSE_CONTENT_TYPE);
        let body = to_bytes(response.into_body(), 4096).await?;
        assert_eq!(body.as_ref(), b"data:{\"candidates\":[{\"index\":0}]}\n\n");
        Ok(())
    }

    #[tokio::test]
    async fn stream_response_sets_sse_headers_and_forwards_frames() -> Result<(), Box<dyn Error>> {
        // Mirrors chat.go:97-115: the handler enters the stream branch,
        // stamps SSE headers + Access-Control-Allow-Origin, and forwards each
        // event as an SSE frame.
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(OpenAiHandlerOutput::Stream(vec![
                Ok(StreamEvent::new(
                    "chat.completion.chunk",
                    r#"{"choices":[{"delta":{"content":"Hi"}}"#,
                )),
                Ok(StreamEvent::new("chat.completion.chunk", "[DONE]")),
            ]))),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/chat/completions")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"model":"gpt-4o","stream":true}"#))?;
        let response = app.call(request).await?;

        // SSE header set (chat.go:107, 142-145).
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .ok_or("content-type missing")?,
            "text/event-stream"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok())
                .ok_or("cache-control missing")?,
            "no-cache"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok())
                .ok_or("allow-origin missing")?,
            "*"
        );

        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body_str = std::str::from_utf8(&bytes)?;
        // Two event frames.
        assert!(body_str.contains("event:chat.completion.chunk"));
        assert!(body_str.contains("[DONE]"));
        Ok(())
    }

    #[tokio::test]
    async fn stream_response_renders_error_frame_for_terminal_stream_error()
    -> Result<(), Box<dyn Error>> {
        // Mirrors chat.go:162-164: when the stream surfaces an error, the
        // handler emits event:error with the formatted error frame.
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(OpenAiHandlerOutput::Stream(vec![
                Ok(StreamEvent::new("chat.completion.chunk", r#"{"id":"x"}"#)),
                Err(ConduitError::quota_exhausted("quota depleted mid-stream")),
            ]))),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/chat/completions")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"model":"gpt-4o","stream":true}"#))?;
        let response = app.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body_str = std::str::from_utf8(&bytes)?;

        // The error frame is present.
        assert!(body_str.contains("event:error"), "body: {body_str}");
        // The quota_exhausted type surfaces in the SSE data line.
        assert!(body_str.contains("quota_exhausted"));
        Ok(())
    }

    #[tokio::test]
    async fn audio_speech_route_dispatches_through_orchestrator() -> Result<(), Box<dyn Error>> {
        // Mirrors openai.go:313-336: the handler validates body, consults
        // shouldUseBinarySpeechStream, and dispatches through the SpeechHandlers
        // orchestrator. Here the service returns a non-stream binary response
        // (the common TTS case where the full audio body is materialised).
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(OpenAiHandlerOutput::NonStream(
                OpenAiHandlerResponse {
                    status: 200,
                    content_type: Some("audio/mpeg".to_string()),
                    body: vec![0xFF_u8, 0xFB, 0x90, 0x00],
                },
            ))),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/speech")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"tts-1","input":"hello","voice":"alloy"}"#,
            ))?;
        let response = app.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .ok_or("content-type missing")?,
            "audio/mpeg"
        );
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        assert_eq!(&*bytes, &[0xFF_u8, 0xFB, 0x90, 0x00]);

        // The route tag is AudioSpeech + the binary/sse decision is stamped.
        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (route, req) = &seen[0];
        assert_eq!(*route, OpenAiRoute::AudioSpeech);
        // Default stream_format absent -> sse (non-binary).
        assert_eq!(req.metadata["audio_stream_mode"], "sse");
        Ok(())
    }

    #[tokio::test]
    async fn audio_speech_rejects_empty_body() -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response("{}"),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/speech")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // Orchestrator not called.
        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert!(seen.is_empty());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // RUST-P11-001 S16 — multipart image/audio endpoints (openai.go:362-378)
    // -----------------------------------------------------------------------
    //
    // Mirrors Go's `OpenAIHandlers.CreateImage`/`CreateImageEdit`/
    // `CreateTranscription`/`CreateTranslation` — thin wrappers that delegate
    // to `ChatCompletionHandlers.ChatCompletion` (chat.go:49-116). The gin
    // handlers do not parse multipart themselves; they forward raw body
    // bytes through `httpclient.ReadHTTPRequest` (utils.go:33) and let the
    // inbound transformer (host-side wiring) handle the multipart parse.
    //
    // These tests construct synthetic multipart bodies by hand to verify the
    // raw bytes are forwarded verbatim with the correct route tag stamped on
    // the orchestrator request, and that empty bodies are rejected with the
    // same 400 invalid_request error chat.go:67-70 produces.

    /// Mirrors Go `OpenAIHandlers.CreateImage` happy path (openai.go:372-374):
    /// JSON body, dispatched through the image-generation inbound transformer
    /// (selected by the route tag).
    #[tokio::test]
    async fn create_image_generations_dispatches_with_image_route_tag() -> Result<(), Box<dyn Error>>
    {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"created":1700,"data":[{"url":"https://x/y.png"}]}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/images/generations")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"dall-e-3","prompt":"a cat","n":1,"size":"1024x1024"}"#,
            ))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);

        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (route, req) = &seen[0];
        assert_eq!(*route, OpenAiRoute::ImageGenerations);
        assert_eq!(req.path, "/v1/images/generations");
        assert_eq!(req.metadata["openai_route"], "/v1/images/generations");
        // JSON body preserved verbatim.
        let body = req.body.as_ref().ok_or("body missing")?;
        assert!(std::str::from_utf8(body)?.contains("dall-e-3"));
        Ok(())
    }

    /// Mirrors Go `OpenAIHandlers.CreateImageEdit` (openai.go:376-378): the
    /// handler forwards the raw multipart body unchanged — multipart parsing
    /// is the inbound transformer's job, NOT the http layer's. Here we
    /// synthesize a multipart body with the canonical field names Go's
    /// openai inbound transformer reads (`image`, `mask`, `prompt`).
    #[tokio::test]
    async fn create_image_edit_forwards_multipart_body_verbatim_with_route_tag()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"created":1700,"data":[{"b64_json":"abc"}]}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        // Build a minimal multipart/form-data body with the field names Go
        // expects (openai image-edit inbound transformer): `image`, `mask`,
        // `prompt`. Boundary = "boundary123".
        let png_header = b"Content-Disposition: form-data; name=\"image\"; filename=\"in.png\"\r\n\
             Content-Type: image/png\r\n\r\n";
        let mask_header =
            b"Content-Disposition: form-data; name=\"mask\"; filename=\"mask.png\"\r\n\
             Content-Type: image/png\r\n\r\n";
        let prompt_header = b"Content-Disposition: form-data; name=\"prompt\"\r\n\r\n";
        let model_header = b"Content-Disposition: form-data; name=\"model\"\r\n\r\n";

        let mut body = Vec::new();
        body.extend_from_slice(b"--boundary123\r\n");
        body.extend_from_slice(png_header);
        body.extend_from_slice(b"\x89PNG\r\n\x1a\n"); // fake PNG magic
        body.extend_from_slice(b"\r\n--boundary123\r\n");
        body.extend_from_slice(mask_header);
        body.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        body.extend_from_slice(b"\r\n--boundary123\r\n");
        body.extend_from_slice(prompt_header);
        body.extend_from_slice(b"a cat with hat");
        body.extend_from_slice(b"\r\n--boundary123\r\n");
        body.extend_from_slice(model_header);
        body.extend_from_slice(b"dall-e-2");
        body.extend_from_slice(b"\r\n--boundary123--\r\n");

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/images/edits")
            .header(
                header::CONTENT_TYPE,
                "multipart/form-data; boundary=boundary123",
            )
            .body(Body::from(body.clone()))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);

        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (route, req) = &seen[0];
        assert_eq!(*route, OpenAiRoute::ImageEdits);
        assert_eq!(req.metadata["openai_route"], "/v1/images/edits");
        // Raw multipart bytes preserved verbatim — including the PNG magic.
        let forwarded = req.body.as_ref().ok_or("body missing")?;
        assert_eq!(forwarded, &body);
        // Content-Type header (with boundary) preserved so the inbound
        // transformer can parse the multipart body downstream.
        assert_eq!(
            req.content_type.as_ref().ok_or("content-type missing")?,
            "multipart/form-data; boundary=boundary123"
        );
        Ok(())
    }

    #[tokio::test]
    async fn multipart_upload_larger_than_axum_default_limit_reaches_handler()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"created":1700,"data":[]}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());
        let body = vec![b'x'; 2 * 1024 * 1024 + 1];

        let response = app
            .call(
                Request::builder()
                    .header(header::AUTHORIZATION, "Bearer test-api-key")
                    .method(Method::POST)
                    .uri("/v1/images/edits")
                    .header(
                        header::CONTENT_TYPE,
                        "multipart/form-data; boundary=upload-boundary",
                    )
                    .body(Body::from(body))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            orchestrator.seen.lock().map_err(|e| e.to_string())?.len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn multipart_limit_rejection_uses_openai_error_envelope() -> Result<(), Box<dyn Error>> {
        async fn limited_handler(body: Result<Bytes, BytesRejection>) -> Response {
            match multipart_body(body) {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(response) => response,
            }
        }

        let mut app = Router::new()
            .route("/upload", post(limited_handler))
            .layer(axum::extract::DefaultBodyLimit::max(16));
        let response = app
            .call(
                Request::builder()
                    .method(Method::POST)
                    .uri("/upload")
                    .body(Body::from(vec![0_u8; 17]))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
        assert_eq!(body["error"]["type"], "invalid_request");
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|message| { message.contains("upload limit") })
        );
        Ok(())
    }

    /// Mirrors Go `OpenAIHandlers.CreateTranscription` (openai.go:362-365):
    /// multipart audio upload (`audio` file + `model` field). Verifies the
    /// raw multipart bytes reach the orchestrator with the right route tag.
    #[tokio::test]
    async fn create_transcription_forwards_multipart_audio_with_route_tag()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"text":"hello world"}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let audio_header =
            b"Content-Disposition: form-data; name=\"audio\"; filename=\"a.mp3\"\r\n\
             Content-Type: audio/mpeg\r\n\r\n";
        let model_header = b"Content-Disposition: form-data; name=\"model\"\r\n\r\n";

        let mut body = Vec::new();
        body.extend_from_slice(b"--bb\r\n");
        body.extend_from_slice(audio_header);
        body.extend_from_slice(b"ID3...fake-mp3-bytes");
        body.extend_from_slice(b"\r\n--bb\r\n");
        body.extend_from_slice(model_header);
        body.extend_from_slice(b"whisper-1");
        body.extend_from_slice(b"\r\n--bb--\r\n");

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/transcriptions")
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=bb")
            .body(Body::from(body.clone()))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);

        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (route, req) = &seen[0];
        assert_eq!(*route, OpenAiRoute::AudioTranscriptions);
        assert_eq!(req.metadata["openai_route"], "/v1/audio/transcriptions");
        assert_eq!(req.body.as_ref().ok_or("body missing")?, &body);
        Ok(())
    }

    /// Mirrors Go `OpenAIHandlers.CreateTranslation` (openai.go:367-370):
    /// multipart audio upload, dispatched through the translation inbound
    /// transformer (route tag).
    #[tokio::test]
    async fn create_translation_forwards_multipart_audio_with_route_tag()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response(r#"{"text":"bonjour le monde"}"#),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let audio_header =
            b"Content-Disposition: form-data; name=\"audio\"; filename=\"a.wav\"\r\n\
             Content-Type: audio/wav\r\n\r\n";
        let model_header = b"Content-Disposition: form-data; name=\"model\"\r\n\r\n";

        let mut body = Vec::new();
        body.extend_from_slice(b"--t boundary\r\n");
        body.extend_from_slice(audio_header);
        body.extend_from_slice(b"RIFF....fake-wav");
        body.extend_from_slice(b"\r\n--t boundary\r\n");
        body.extend_from_slice(model_header);
        body.extend_from_slice(b"whisper-1");
        body.extend_from_slice(b"\r\n--t boundary--\r\n");

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/translations")
            .header(
                header::CONTENT_TYPE,
                "multipart/form-data; boundary=t boundary",
            )
            .body(Body::from(body.clone()))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);

        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (route, req) = &seen[0];
        assert_eq!(*route, OpenAiRoute::AudioTranslations);
        assert_eq!(req.metadata["openai_route"], "/v1/audio/translations");
        assert_eq!(req.body.as_ref().ok_or("body missing")?, &body);
        Ok(())
    }

    /// Empty multipart body -> 400 invalid_request (chat.go:67-70). The same
    /// error path chat.go:67-70 produces for any handler delegating to
    /// `ChatCompletionWithRequest`. Verified across all four new routes so
    /// the regression catches any future per-route special-casing.
    #[tokio::test]
    async fn multipart_endpoints_reject_empty_body_with_invalid_request()
    -> Result<(), Box<dyn Error>> {
        for (path, expected_route) in [
            ("/v1/images/generations", OpenAiRoute::ImageGenerations),
            ("/v1/images/edits", OpenAiRoute::ImageEdits),
            ("/v1/audio/transcriptions", OpenAiRoute::AudioTranscriptions),
            ("/v1/audio/translations", OpenAiRoute::AudioTranslations),
        ] {
            let orchestrator = Arc::new(FakeOpenAiOrchestrator {
                seen: Mutex::new(Vec::new()),
                response: fake_response("{}"),
                fail_quota_exhausted: false,
                fail_message: String::new(),
            });
            let mut app = app_with_orchestrator(orchestrator.clone());

            let request = Request::builder()
                .header(header::AUTHORIZATION, "Bearer test-api-key")
                .method(Method::POST)
                .uri(path)
                .header(header::CONTENT_TYPE, "multipart/form-data; boundary=x")
                .body(Body::empty())?;
            let response = app.call(request).await?;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
            let bytes = to_bytes(response.into_body(), 4096).await?;
            let body: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(body["error"]["message"], "Request body is empty", "{path}");
            assert_eq!(body["error"]["type"], "invalid_request", "{path}");

            // Orchestrator MUST NOT be called when body is empty.
            let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
            assert!(seen.is_empty(), "{path}: orchestrator not called");
            // Suppress unused warning when assertions are compiled out.
            let _ = expected_route;
        }
        Ok(())
    }

    /// Multipart endpoint forwards orchestrator errors through the OpenAI-
    /// compatible envelope (chat.go:78-81). Mirrors the same path the chat
    /// completions handler takes; verified here on `/v1/audio/transcriptions`
    /// since multipart endpoints share the same dispatch.
    #[tokio::test]
    async fn multipart_endpoint_surfaces_orchestrator_error_as_openai_envelope()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response("{}"),
            fail_quota_exhausted: true,
            fail_message: "transcription backend unavailable".to_string(),
        });
        let mut app = app_with_orchestrator(orchestrator);

        // Minimal multipart body so the empty-body guard does not fire.
        let body = b"--b\r\n\
                     Content-Disposition: form-data; name=\"model\"\r\n\r\n\
                     whisper-1\r\n\
                     --b--\r\n";
        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/transcriptions")
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=b")
            .body(Body::from(&body[..]))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "quota_exhausted");
        assert_eq!(
            body["error"]["message"],
            "transcription backend unavailable"
        );
        Ok(())
    }

    #[tokio::test]
    async fn audio_speech_with_binary_stream_format_stamps_binary_metadata()
    -> Result<(), Box<dyn Error>> {
        // Mirrors openai.go:330-335: when stream_format=="audio" the handler
        // routes through the binary writer. Here we verify the metadata stamp;
        // the actual binary stream framing is a host-wiring gap.
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response("{}"),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator.clone());

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/speech")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"tts-1","input":"hi","stream_format":"audio"}"#,
            ))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);

        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (_route, req) = &seen[0];
        assert_eq!(req.metadata["audio_stream_mode"], "binary");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // RUST-P11-001 S15 — Binary stream output (chat.go:175-239 WriteBinaryStream)
    // -----------------------------------------------------------------------
    //
    // Faraday-the-19th shipped the `OpenAiHandlerOutput::Binary` variant to
    // close the audio-binary gap left by Faraday-the-18th, but no test ever
    // exercised that branch — `audio_speech_route_dispatches_through_orchestrator`
    // returns a `NonStream` payload and only verifies the bytes/Content-Type,
    // so the dedicated binary-writer path (`materialise_binary_stream_response`)
    // stayed green-without-coverage. These tests assert the wire shape Go's
    // `WriteBinaryStream` (chat.go:175-239) produces: status 200, the
    // companion header set stamped by [`binary_stream_headers`], the
    // orchestrator-supplied Content-Type winning over the
    // `application/octet-stream` default, and the raw bytes written verbatim.

    /// Binary happy path — orchestrator returns `OpenAiHandlerOutput::Binary`
    /// with an `audio/mpeg` content type. Mirrors Go's `WriteBinaryStream`
    /// (chat.go:175-239) once the first chunk's `Type` carries `audio/mpeg`:
    ///
    /// * status `200 OK` (chat.go:218 stamps headers lazily; never changes);
    /// * `Content-Type: audio/mpeg` from the first event's `Type`
    ///   (chat.go:219-221);
    /// * companion headers `Cache-Control: no-cache`, `Connection: keep-alive`,
    ///   `Access-Control-Allow-Origin: *` (chat.go:223-226);
    /// * raw bytes written verbatim (chat.go:230 `c.Writer.Write(cur.Data)`).
    #[tokio::test]
    async fn audio_speech_binary_variant_stamps_audio_mpeg_content_type_and_body()
    -> Result<(), Box<dyn Error>> {
        let payload = vec![0xFF_u8, 0xFB, 0x90, 0x44, 0x00, 0x12];
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(OpenAiHandlerOutput::Binary {
                body: payload.clone(),
                content_type: Some("audio/mpeg".to_string()),
            })),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/speech")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"tts-1","input":"hello","stream_format":"audio"}"#,
            ))?;
        let response = app.call(request).await?;

        // chat.go:218 — status is always 200 once headers are flushed.
        assert_eq!(response.status(), StatusCode::OK);

        // chat.go:219-221 — Content-Type comes from the first event's Type.
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .ok_or("content-type missing")?,
            "audio/mpeg"
        );

        // chat.go:223-226 — companion header set.
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok())
                .ok_or("cache-control missing")?,
            "no-cache"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONNECTION)
                .and_then(|v| v.to_str().ok())
                .ok_or("connection missing")?,
            "keep-alive"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok())
                .ok_or("access-control-allow-origin missing")?,
            "*"
        );

        // chat.go:230 — raw bytes written verbatim.
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        assert_eq!(&*bytes, &*payload);
        Ok(())
    }

    /// Binary variant with no Content-Type falls back to the
    /// `application/octet-stream` default — chat.go:181 initial value of
    /// `contentType` survives when the first event's `Type` is empty
    /// (chat.go:219 trims and skips the assignment).
    #[tokio::test]
    async fn audio_speech_binary_variant_with_empty_content_type_falls_back_to_octet_stream()
    -> Result<(), Box<dyn Error>> {
        let payload = vec![0x00_u8, 0x01, 0x02, 0x03];
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(OpenAiHandlerOutput::Binary {
                body: payload.clone(),
                content_type: None,
            })),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/speech")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"tts-1","input":"hello","stream_format":"audio"}"#,
            ))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .ok_or("content-type missing")?,
            BINARY_STREAM_DEFAULT_CONTENT_TYPE
        );
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        assert_eq!(&*bytes, &*payload);
        Ok(())
    }

    /// Binary variant with an empty-string content type also falls back to
    /// the octet-stream default — `materialise_binary_stream_response` treats
    /// whitespace-only strings as missing (mirroring chat.go:219's
    /// `strings.TrimSpace` check).
    #[tokio::test]
    async fn audio_speech_binary_variant_with_blank_content_type_falls_back_to_octet_stream()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(OpenAiHandlerOutput::Binary {
                body: vec![0xAB_u8],
                content_type: Some("   ".to_string()),
            })),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let mut app = app_with_orchestrator(orchestrator);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/speech")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"tts-1","input":"hi","stream_format":"audio"}"#,
            ))?;
        let response = app.call(request).await?;
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .ok_or("content-type missing")?,
            BINARY_STREAM_DEFAULT_CONTENT_TYPE
        );
        Ok(())
    }

    #[tokio::test]
    async fn audio_speech_live_binary_forwards_body_frames_without_sse_or_buffering()
    -> Result<(), Box<dyn Error>> {
        use futures_core::Stream;

        let first = vec![0xff, 0x00, b'd', b'a', b't', b'a', b':'];
        let second = vec![b'[', b'D', b'O', b'N', b'E', b']', b'\n', b'\n', 0x80];
        let (tx, rx) = tokio::sync::mpsc::channel(3);
        tx.send(Ok(conduit_llm::StreamEvent {
            event_type: Some("audio/mpeg; codec=mp3".to_string()),
            binary: Some(first.clone()),
            ..conduit_llm::StreamEvent::default()
        }))
        .await?;
        tx.send(Ok(conduit_llm::StreamEvent {
            event_type: Some("binary.done".to_string()),
            ..conduit_llm::StreamEvent::default()
        }))
        .await?;
        tx.send(Ok(conduit_llm::StreamEvent {
            event_type: Some("audio/mpeg; codec=mp3".to_string()),
            binary: Some(second.clone()),
            ..conduit_llm::StreamEvent::default()
        }))
        .await?;
        drop(tx);

        let response = live_binary_stream_response(rx, Some("audio/mpeg; codec=mp3".to_string()));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("audio/mpeg; codec=mp3")
        );

        let mut body = response.into_body().into_data_stream();
        let first_frame = std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_next(cx))
            .await
            .ok_or("missing first live binary frame")??;
        let second_frame = std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_next(cx))
            .await
            .ok_or("missing second live binary frame")??;
        assert_eq!(first_frame.as_ref(), first.as_slice());
        assert_eq!(second_frame.as_ref(), second.as_slice());
        assert!(
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_next(cx))
                .await
                .is_none(),
            "binary.done must stay internal and the stream must end at provider EOF"
        );
        Ok(())
    }

    #[tokio::test]
    async fn audio_speech_live_binary_missing_content_type_falls_back_to_octet_stream()
    -> Result<(), Box<dyn Error>> {
        let payload = vec![0xff, 0x00, 0x80, b'd', b'a', b't', b'a', b':'];
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(Ok(conduit_llm::StreamEvent {
            binary: Some(payload.clone()),
            ..conduit_llm::StreamEvent::default()
        }))
        .await?;
        drop(tx);

        let response = live_binary_stream_response(rx, None);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(BINARY_STREAM_DEFAULT_CONTENT_TYPE)
        );
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        assert_eq!(bytes.as_ref(), payload.as_slice());
        Ok(())
    }

    /// Binary happy path through `/v1/audio/speech` error branch: when the
    /// orchestrator returns an `Err`, the OpenAI-compatible error envelope is
    /// rendered instead of a binary body — mirroring chat.go:78-81
    /// `transformOrchestratorError`.
    #[tokio::test]
    async fn audio_speech_binary_path_surfaces_orchestrator_error_as_openai_envelope()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(OpenAiHandlerOutput::Binary {
                body: vec![0xFF_u8],
                content_type: Some("audio/mpeg".to_string()),
            })),
            fail_quota_exhausted: true,
            fail_message: "out of quota".to_string(),
        });
        let mut app = app_with_orchestrator(orchestrator);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/audio/speech")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"tts-1","input":"hi","stream_format":"audio"}"#,
            ))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "quota_exhausted");
        assert_eq!(body["error"]["message"], "out of quota");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // RUST-P11-001 S12 — video endpoints (openai.go:384-468)
    // -----------------------------------------------------------------------
    //
    // Faraday-the-19th landed `create_video`/`get_video`/`delete_video` plus
    // the `VideoService` trait, but the test step was cut short by a 524. The
    // shape parity surface is:
    //
    // * POST /v1/videos (openai.go:384-419 — CreateVideo): empty-body -> 400,
    //   orchestrator-failure -> OpenAI error envelope, happy -> 200 with the
    //   orchestrator's Content-Type (defaulting to `application/json`).
    // * GET /v1/videos/{id} (openai.go:421-451 — GetVideo): empty id -> 400,
    //   VideoService failure -> 500 envelope, happy -> 200 with bytes
    //   forwarded verbatim through `materialise_openai_response`.
    // * DELETE /v1/videos/{id} (openai.go:453-468 — DeleteVideo): empty id ->
    //   400, VideoService failure -> 500 envelope, happy -> 204 No Content
    //   with empty body.
    //
    // The host-side bridge runs the S12 ordered-delete flow (provider delete
    // first, then best-effort local cancel) behind the trait; the http-side
    // contract only sees `Ok(())` vs `Err(_)`.

    /// In-memory VideoService for testing. Returns canned responses for each
    /// method and records the external ids it was asked to resolve.
    struct FakeVideoService {
        get_response: Mutex<Option<Result<OpenAiHandlerResponse, ConduitError>>>,
        delete_result: Mutex<Option<Result<(), ConduitError>>>,
        // Records (project_id, external_id) per call so tests can assert the
        // handler forwards the caller's project scope (P-23).
        seen_get: Mutex<Vec<(i64, String)>>,
        seen_delete: Mutex<Vec<(i64, String)>>,
    }

    #[async_trait::async_trait]
    impl VideoService for FakeVideoService {
        async fn get_task_by_external_id(
            &self,
            project_id: i64,
            external_id: &str,
        ) -> Result<OpenAiHandlerResponse, ConduitError> {
            if let Ok(mut guard) = self.seen_get.lock() {
                guard.push((project_id, external_id.to_string()));
            }
            match self.get_response.lock() {
                Ok(mut guard) => guard.take(),
                Err(_) => None,
            }
            .unwrap_or_else(|| Ok(OpenAiHandlerResponse::ok_json(b"{}".to_vec())))
        }

        async fn delete_task_by_external_id(
            &self,
            project_id: i64,
            external_id: &str,
        ) -> Result<(), ConduitError> {
            if let Ok(mut guard) = self.seen_delete.lock() {
                guard.push((project_id, external_id.to_string()));
            }
            match self.delete_result.lock() {
                Ok(mut guard) => guard.take(),
                Err(_) => None,
            }
            .unwrap_or(Ok(()))
        }
    }

    impl FakeVideoService {
        fn new() -> Self {
            Self {
                get_response: Mutex::new(None),
                delete_result: Mutex::new(None),
                seen_get: Mutex::new(Vec::new()),
                seen_delete: Mutex::new(Vec::new()),
            }
        }
    }

    fn app_with_video_service(
        video: Arc<FakeVideoService>,
        orchestrator: Option<Arc<FakeOpenAiOrchestrator>>,
    ) -> Router {
        let mut services = test_services().with_video_service(video);
        if let Some(orch) = orchestrator {
            services = services.with_openai_orchestrator(orch);
        }
        build_router(crate::app_state::AppState::new(
            Arc::new(AppConfig::default()),
            Arc::new(services),
        ))
    }

    /// Validator that stamps a specific `project_id` on the key metadata, so
    /// P-23 project-scoping can be asserted end to end.
    struct ProjectApiKey(i64);

    #[async_trait::async_trait]
    impl ApiKeyValidationService for ProjectApiKey {
        async fn validate(
            &self,
            _plaintext_key: &str,
        ) -> Result<ValidatedApiKeyMetadata, ApiKeyValidationError> {
            Ok(ValidatedApiKeyMetadata {
                project_id: self.0,
                ..ValidatedApiKeyMetadata::default()
            })
        }
    }

    fn app_with_video_service_for_project(video: Arc<FakeVideoService>, project_id: i64) -> Router {
        let services = AppServices::new()
            .with_api_key_validation_service(Arc::new(ProjectApiKey(project_id)))
            .with_video_service(video);
        build_router(crate::app_state::AppState::new(
            Arc::new(AppConfig::default()),
            Arc::new(services),
        ))
    }

    /// P-23: GET/DELETE video must forward the *caller's* project id to the
    /// service (which scopes the SQL), so a key from project A cannot resolve a
    /// task belonging to project B.
    #[tokio::test]
    async fn video_endpoints_scope_lookup_to_caller_project() -> Result<(), Box<dyn Error>> {
        let video = Arc::new(FakeVideoService::new());
        let mut app = app_with_video_service_for_project(video.clone(), 42);

        let get = Request::builder()
            .header(header::AUTHORIZATION, "Bearer proj-42-key")
            .method(Method::GET)
            .uri("/v1/videos/vid_foreign")
            .body(Body::empty())?;
        assert_eq!(app.call(get).await?.status(), StatusCode::OK);

        let del = Request::builder()
            .header(header::AUTHORIZATION, "Bearer proj-42-key")
            .method(Method::DELETE)
            .uri("/v1/videos/vid_foreign")
            .body(Body::empty())?;
        assert_eq!(app.call(del).await?.status(), StatusCode::NO_CONTENT);

        let seen_get = video.seen_get.lock().map_err(|e| e.to_string())?;
        assert_eq!(
            seen_get.as_slice(),
            &[(42i64, "vid_foreign".to_string())][..]
        );
        let seen_delete = video.seen_delete.lock().map_err(|e| e.to_string())?;
        assert_eq!(
            seen_delete.as_slice(),
            &[(42i64, "vid_foreign".to_string())][..]
        );
        Ok(())
    }

    /// POST /v1/videos happy path — orchestrator returns a 200 JSON body and
    /// the handler forwards it verbatim with the orchestrator's Content-Type.
    /// Mirrors Go `OpenAIHandlers.CreateVideo` (openai.go:384-419) once
    /// `result.ChatCompletion != nil` (openai.go:408-411 guard is the host's
    /// responsibility; the bounded contract surfaces the happy branch here).
    #[tokio::test]
    async fn create_video_happy_path_forwards_orchestrator_response() -> Result<(), Box<dyn Error>>
    {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(OpenAiHandlerOutput::NonStream(
                OpenAiHandlerResponse {
                    status: 200,
                    content_type: Some("application/json".to_string()),
                    body: br#"{"id":"vid_abc","object":"video.task"}"#.to_vec(),
                },
            ))),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let video = Arc::new(FakeVideoService::new());
        let mut app = app_with_video_service(video, Some(orchestrator.clone()));

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/videos")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"sora-2","prompt":"a cat playing piano"}"#,
            ))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .ok_or("content-type missing")?,
            "application/json"
        );
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["id"], "vid_abc");
        assert_eq!(body["object"], "video.task");

        // Route tag flows through metadata so the host picks the video
        // inbound transformer (openai.go:399 — VideoHandlers carries its own
        // ChatCompletionOrchestrator).
        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.len(), 1);
        let (route, req) = &seen[0];
        assert_eq!(*route, OpenAiRoute::Videos);
        assert_eq!(req.path, "/v1/videos");
        assert_eq!(req.method, "POST");
        assert_eq!(req.metadata["openai_route"], "/v1/videos");
        Ok(())
    }

    /// POST /v1/videos empty body -> 400 invalid_request (chat.go:67-70
    /// reused via `validate_chat_request`, error rendered through the
    /// OpenAI-compatible envelope). The orchestrator MUST NOT be called.
    #[tokio::test]
    async fn create_video_empty_body_returns_invalid_request() -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response("{}"),
            fail_quota_exhausted: false,
            fail_message: String::new(),
        });
        let video = Arc::new(FakeVideoService::new());
        let mut app = app_with_video_service(video, Some(orchestrator.clone()));

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/videos")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["message"], "Request body is empty");
        assert_eq!(body["error"]["type"], "invalid_request");

        // Orchestrator not called.
        let seen = orchestrator.seen.lock().map_err(|e| e.to_string())?;
        assert!(seen.is_empty());
        Ok(())
    }

    /// POST /v1/videos orchestrator failure -> OpenAI-compatible error
    /// envelope (chat.go:78-81 + transformOrchestratorError parity).
    #[tokio::test]
    async fn create_video_orchestrator_failure_renders_openai_envelope()
    -> Result<(), Box<dyn Error>> {
        let orchestrator = Arc::new(FakeOpenAiOrchestrator {
            seen: Mutex::new(Vec::new()),
            response: fake_response("{}"),
            fail_quota_exhausted: true,
            fail_message: "video provider offline".to_string(),
        });
        let video = Arc::new(FakeVideoService::new());
        let mut app = app_with_video_service(video, Some(orchestrator));

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/videos")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"model":"sora-2"}"#))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "quota_exhausted");
        assert_eq!(body["error"]["message"], "video provider offline");
        Ok(())
    }

    /// POST /v1/videos with no orchestrator wired -> 500 internal_error
    /// (the bounded-scope skeleton path mirrors Go's 5xx branch when
    /// `Process` returns a non-nil error).
    #[tokio::test]
    async fn create_video_unwired_orchestrator_returns_internal_error() -> Result<(), Box<dyn Error>>
    {
        let video = Arc::new(FakeVideoService::new());
        let mut app = app_with_video_service(video, None);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::POST)
            .uri("/v1/videos")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"model":"sora-2"}"#))?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "internal_error");
        Ok(())
    }

    /// GET /v1/videos/{id} happy path — VideoService returns a 200 JSON body
    /// and the handler forwards it verbatim with the orchestrator's
    /// Content-Type. Mirrors Go `OpenAIHandlers.GetVideo` (openai.go:421-451).
    #[tokio::test]
    async fn get_video_happy_path_forwards_video_service_response() -> Result<(), Box<dyn Error>> {
        let video = Arc::new(FakeVideoService {
            get_response: Mutex::new(Some(Ok(OpenAiHandlerResponse {
                status: 200,
                content_type: Some("application/json".to_string()),
                body: br#"{"id":"vid_abc","object":"video","status":"succeeded"}"#.to_vec(),
            }))),
            delete_result: Mutex::new(None),
            seen_get: Mutex::new(Vec::new()),
            seen_delete: Mutex::new(Vec::new()),
        });
        let mut app = app_with_video_service(video.clone(), None);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::GET)
            .uri("/v1/videos/vid_abc")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .ok_or("content-type missing")?,
            "application/json"
        );
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["id"], "vid_abc");
        assert_eq!(body["object"], "video");

        // The external id + the caller's project scope were forwarded to the
        // service (project_id 0 is the default test-validator metadata).
        let seen = video.seen_get.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.as_slice(), &[(0i64, "vid_abc".to_string())][..]);
        Ok(())
    }

    /// GET /v1/videos/{id} with an empty id -> 400 invalid_request
    /// (openai.go:425-427). The VideoService MUST NOT be called.
    #[tokio::test]
    async fn get_video_empty_id_returns_invalid_request() -> Result<(), Box<dyn Error>> {
        let video = Arc::new(FakeVideoService::new());
        let mut app = app_with_video_service(video.clone(), None);

        // Axum normalises `/v1/videos/` to `/v1/videos` (no `{id}` match),
        // so we hit the empty-id guard by exercising the handler directly
        // through the trait shape: a missing id at the routing layer would
        // return 404 from axum, not 400 — the in-handler empty-string guard
        // fires when callers reach the handler with an explicit empty path
        // segment (e.g. via a base-path alias). The end-to-end test mirrors
        // the production routing shape by issuing `/v1/videos/%20` (a
        // whitespace-only id), which the handler trims and rejects.
        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::GET)
            .uri("/v1/videos/%20")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "invalid_request");
        assert_eq!(body["error"]["message"], "invalid id");

        let seen = video.seen_get.lock().map_err(|e| e.to_string())?;
        assert!(seen.is_empty());
        Ok(())
    }

    /// GET /v1/videos/{id} VideoService failure -> 500 OpenAI-compatible
    /// envelope (openai.go:431-434 — `JSONError(c, http.StatusInternalServerError, err)`).
    #[tokio::test]
    async fn get_video_service_failure_returns_internal_error() -> Result<(), Box<dyn Error>> {
        let video = Arc::new(FakeVideoService {
            get_response: Mutex::new(Some(Err(ConduitError::internal("video store offline")))),
            delete_result: Mutex::new(None),
            seen_get: Mutex::new(Vec::new()),
            seen_delete: Mutex::new(Vec::new()),
        });
        let mut app = app_with_video_service(video, None);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::GET)
            .uri("/v1/videos/vid_xyz")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "internal_error");
        Ok(())
    }

    /// GET /v1/videos/{id} with no VideoService wired -> 500 internal_error.
    /// Mirrors the bounded-scope skeleton path; the happy guard at
    /// `state.services().video_service()` surfaces the wiring gap.
    #[tokio::test]
    async fn get_video_unwired_service_returns_internal_error() -> Result<(), Box<dyn Error>> {
        let mut app = build_router(test_state());

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::GET)
            .uri("/v1/videos/vid_abc")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "internal_error");
        Ok(())
    }

    /// DELETE /v1/videos/{id} happy path -> 204 No Content with empty body
    /// (openai.go:467 — `c.Status(http.StatusNoContent)`).
    #[tokio::test]
    async fn delete_video_happy_path_returns_no_content() -> Result<(), Box<dyn Error>> {
        let video = Arc::new(FakeVideoService {
            get_response: Mutex::new(None),
            delete_result: Mutex::new(Some(Ok(()))),
            seen_get: Mutex::new(Vec::new()),
            seen_delete: Mutex::new(Vec::new()),
        });
        let mut app = app_with_video_service(video.clone(), None);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::DELETE)
            .uri("/v1/videos/vid_abc")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        assert!(bytes.is_empty(), "204 response body must be empty");

        let seen = video.seen_delete.lock().map_err(|e| e.to_string())?;
        assert_eq!(seen.as_slice(), &[(0i64, "vid_abc".to_string())][..]);
        Ok(())
    }

    /// DELETE /v1/videos/{id} with an empty (whitespace-only) id -> 400
    /// invalid_request (openai.go:457-459).
    #[tokio::test]
    async fn delete_video_empty_id_returns_invalid_request() -> Result<(), Box<dyn Error>> {
        let video = Arc::new(FakeVideoService::new());
        let mut app = app_with_video_service(video.clone(), None);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::DELETE)
            .uri("/v1/videos/%20")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "invalid_request");
        assert_eq!(body["error"]["message"], "invalid id");

        let seen = video.seen_delete.lock().map_err(|e| e.to_string())?;
        assert!(seen.is_empty());
        Ok(())
    }

    /// DELETE /v1/videos/{id} VideoService failure -> 500 OpenAI-compatible
    /// envelope (openai.go:463-465 — `JSONError(c, http.StatusInternalServerError, err)`).
    #[tokio::test]
    async fn delete_video_service_failure_returns_internal_error() -> Result<(), Box<dyn Error>> {
        let video = Arc::new(FakeVideoService {
            get_response: Mutex::new(None),
            delete_result: Mutex::new(Some(Err(ConduitError::internal("provider delete refused")))),
            seen_get: Mutex::new(Vec::new()),
            seen_delete: Mutex::new(Vec::new()),
        });
        let mut app = app_with_video_service(video, None);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .method(Method::DELETE)
            .uri("/v1/videos/vid_abc")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "internal_error");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // RUST-P11-001 — models list/retrieve handler tests (openai.go:671-788)
    // -----------------------------------------------------------------------

    /// In-memory ModelService for testing. Returns a canned model list.
    struct FakeModelService {
        models: Vec<ModelRow>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl ModelService for FakeModelService {
        async fn list_enabled_models(&self) -> Result<Vec<ModelRow>, ConduitError> {
            if self.fail {
                return Err(ConduitError::internal("db unavailable"));
            }
            Ok(self.models.clone())
        }
    }

    fn app_with_model_service(service: Arc<FakeModelService>) -> Router {
        let services = test_services().with_model_service(service);
        build_router(crate::app_state::AppState::new(
            Arc::new(AppConfig::default()),
            Arc::new(services),
        ))
    }

    struct ModelRestrictedApiKey(&'static str);

    #[async_trait::async_trait]
    impl ApiKeyValidationService for ModelRestrictedApiKey {
        async fn validate(
            &self,
            _plaintext_key: &str,
        ) -> Result<ValidatedApiKeyMetadata, ApiKeyValidationError> {
            Ok(ValidatedApiKeyMetadata {
                allowed_models: self.0.to_string(),
                ..ValidatedApiKeyMetadata::default()
            })
        }
    }

    fn app_with_restricted_model_service(
        service: Arc<FakeModelService>,
        allowed_models: &'static str,
    ) -> Router {
        let services = AppServices::new()
            .with_api_key_validation_service(Arc::new(ModelRestrictedApiKey(allowed_models)))
            .with_model_service(service);
        build_router(crate::app_state::AppState::new(
            Arc::new(AppConfig::default()),
            Arc::new(services),
        ))
    }

    #[tokio::test]
    async fn list_models_returns_list_envelope_from_service() -> Result<(), Box<dyn Error>> {
        // Mirrors Go ListModels happy path (openai.go:726-788).
        let service = Arc::new(FakeModelService {
            models: vec![
                ModelRow::facade("gpt-4o", "openai", 1_700_000_000, ""),
                ModelRow::facade("claude-3", "anthropic", 1_690_000_000, ""),
            ],
            fail: false,
        });
        let mut app = app_with_model_service(service);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .uri("/v1/models")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body: Value = serde_json::from_slice(&bytes)?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "list");
        assert_eq!(body["data"][0]["id"], "gpt-4o");
        assert_eq!(body["data"][0]["object"], "model");
        assert_eq!(body["data"][0]["owned_by"], "openai");
        assert_eq!(body["data"][1]["id"], "claude-3");
        Ok(())
    }

    #[tokio::test]
    async fn list_models_filters_using_validated_api_key_metadata() -> Result<(), Box<dyn Error>> {
        let service = Arc::new(FakeModelService {
            models: vec![
                ModelRow::facade("gpt-4o", "openai", 1_700_000_000, ""),
                ModelRow::facade("claude-3", "anthropic", 1_690_000_000, ""),
            ],
            fail: false,
        });
        let mut app = app_with_restricted_model_service(service, " claude-3, missing-model ");

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer restricted-api-key")
            .uri("/v1/models")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["data"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["data"][0]["id"], "claude-3");
        Ok(())
    }

    #[tokio::test]
    async fn list_models_empty_returns_empty_list_envelope() -> Result<(), Box<dyn Error>> {
        // Mirrors openai.go:741-748: empty model list still returns a
        // well-formed `{object: "list", data: []}` envelope.
        let service = Arc::new(FakeModelService {
            models: Vec::new(),
            fail: false,
        });
        let mut app = app_with_model_service(service);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .uri("/v1/models")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["object"], "list");
        assert_eq!(body["data"], json!([]));
        Ok(())
    }

    #[tokio::test]
    async fn list_models_service_error_returns_internal_error() -> Result<(), Box<dyn Error>> {
        // Mirrors openai.go:737-738: writeOpenAIInternalError on service failure.
        let service = Arc::new(FakeModelService {
            models: Vec::new(),
            fail: true,
        });
        let mut app = app_with_model_service(service);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .uri("/v1/models")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "internal_error");
        Ok(())
    }

    #[tokio::test]
    async fn retrieve_model_returns_bare_model_object() -> Result<(), Box<dyn Error>> {
        // Mirrors Go RetrieveModel happy path (openai.go:699-701): single model
        // is returned as a bare object, NOT wrapped in a list envelope.
        let service = Arc::new(FakeModelService {
            models: vec![ModelRow::facade("gpt-4o", "openai", 1_700_000_000, "")],
            fail: false,
        });
        let mut app = app_with_model_service(service);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .uri("/v1/models/gpt-4o")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["object"], "model");
        assert!(body.get("data").is_none(), "retrieve must not wrap in list");
        assert_eq!(body["id"], "gpt-4o");
        assert_eq!(body["owned_by"], "openai");
        Ok(())
    }

    #[tokio::test]
    async fn retrieve_model_unknown_id_returns_model_not_found() -> Result<(), Box<dyn Error>> {
        // Mirrors openai.go:694-697 + writeOpenAIModelNotFoundError.
        let service = Arc::new(FakeModelService {
            models: vec![ModelRow::facade("gpt-4o", "openai", 1_700_000_000, "")],
            fail: false,
        });
        let mut app = app_with_model_service(service);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .uri("/v1/models/does-not-exist")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["code"], "model_not_found");
        assert_eq!(body["error"]["param"], "model");
        // Message names the missing id.
        assert!(
            body["error"]["message"]
                .as_str()
                .ok_or("message missing")?
                .contains("does-not-exist")
        );
        Ok(())
    }

    #[tokio::test]
    async fn retrieve_model_strips_leading_splat_slash() -> Result<(), Box<dyn Error>> {
        // Mirrors openai.go:677 — TrimPrefix(c.Param("model"), "/"). The
        // catch-all path segment arrives with a leading slash that must be
        // stripped before lookup.
        let service = Arc::new(FakeModelService {
            models: vec![ModelRow::facade(
                "deepseek/deepseek-chat",
                "deepseek",
                1_700_000_000,
                "",
            )],
            fail: false,
        });
        let mut app = app_with_model_service(service);

        // Axum's {model} captures the path segment without the leading slash;
        // the handler also tolerates the Gin-style splat form for parity.
        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .uri("/v1/models/deepseek%2Fdeepseek-chat")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["id"], "deepseek/deepseek-chat");
        Ok(())
    }

    /// P-52: a model id with a **literal** (unencoded) slash must resolve via
    /// the catch-all route, matching Go's gin `/models/*model`. The prior test
    /// used `%2F` which a single-segment `{model}` route also accepts, so it did
    /// not prove catch-all parity. This one sends the raw slash.
    #[tokio::test]
    async fn retrieve_model_with_unencoded_slash_resolves_via_catch_all()
    -> Result<(), Box<dyn Error>> {
        let service = Arc::new(FakeModelService {
            models: vec![ModelRow::facade(
                "deepseek/deepseek-chat",
                "deepseek",
                1_700_000_000,
                "",
            )],
            fail: false,
        });
        let mut app = app_with_model_service(service);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .uri("/v1/models/deepseek/deepseek-chat")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "unencoded slash in model id must resolve (catch-all route)"
        );
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["id"], "deepseek/deepseek-chat");
        Ok(())
    }

    #[tokio::test]
    async fn list_models_with_include_query_populates_extended_fields() -> Result<(), Box<dyn Error>>
    {
        // Mirrors openai.go:731 — include=all triggers the extended payload.
        let service = Arc::new(FakeModelService {
            models: vec![ModelRow {
                id: "gpt-4".to_string(),
                owned_by: "openai".to_string(),
                created: 1_686_935_002,
                name: Some("GPT-4".to_string()),
                description: None,
                icon: None,
                ty: None,
                model_card: None,
                retail_pricing: None,
            }],
            fail: false,
        });
        let mut app = app_with_model_service(service);

        let request = Request::builder()
            .header(header::AUTHORIZATION, "Bearer test-api-key")
            .uri("/v1/models?include=name")
            .body(Body::empty())?;
        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["data"][0]["name"], "GPT-4");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // API key metadata plumbing tests
    // -----------------------------------------------------------------------

    /// Verify that `build_openai_http_request` stamps validated API key
    /// metadata onto `HttpRequest.metadata` when a [`ValidatedApiKeyMetadata`]
    /// is provided. This is the core plumbing test: pipeline middlewares
    /// can read API key identity, model whitelist, and project association
    /// from the request metadata without PersistenceState.
    #[test]
    fn api_key_metadata_stamped_on_http_request() -> Result<(), Box<dyn Error>> {
        use crate::middleware::api_key_auth::api_key_meta_keys;
        use axum::http::Method;

        let meta = ValidatedApiKeyMetadata {
            api_key_id: 42,
            api_key_name: "prod-key".to_string(),
            allowed_models: "gpt-4o,claude-3-5-sonnet".to_string(),
            project_id: 7,
            model_mapping: r#"{"gpt-4":"gpt-4o"}"#.to_string(),
            ..ValidatedApiKeyMetadata::default()
        };

        let uri: Uri = "/v1/chat/completions".parse()?;
        let headers = HeaderMap::new();
        let body = Bytes::from(r#"{"model":"gpt-4o","messages":[]}"#);

        let request = build_openai_http_request(
            OpenAiRoute::ChatCompletions,
            &uri,
            Method::POST,
            &headers,
            body,
            Some(&meta),
            &TracingHeaderConfig::default(),
        );

        // API key id
        assert_eq!(
            request.metadata.get(api_key_meta_keys::API_KEY_ID),
            Some(&serde_json::Value::from(42_i64)),
        );
        // API key name
        assert_eq!(
            request.metadata.get(api_key_meta_keys::API_KEY_NAME),
            Some(&serde_json::Value::from("prod-key")),
        );
        // Allowed models (comma-separated)
        assert_eq!(
            request
                .metadata
                .get(api_key_meta_keys::API_KEY_ALLOWED_MODELS),
            Some(&serde_json::Value::from("gpt-4o,claude-3-5-sonnet")),
        );
        // Project id
        assert_eq!(
            request.metadata.get(api_key_meta_keys::API_KEY_PROJECT_ID),
            Some(&serde_json::Value::from(7_i64)),
        );
        // Route tag is still present
        assert_eq!(
            request.metadata.get("openai_route"),
            Some(&serde_json::Value::from("/v1/chat/completions")),
        );
        Ok(())
    }

    /// Verify that `build_openai_http_request` does NOT stamp API key
    /// metadata when no [`ValidatedApiKeyMetadata`] is provided (the
    /// unauthenticated / no-DB-validation path).
    #[test]
    fn no_api_key_metadata_when_none_provided() -> Result<(), Box<dyn Error>> {
        use crate::middleware::api_key_auth::api_key_meta_keys;
        use axum::http::Method;

        let uri: Uri = "/v1/chat/completions".parse()?;
        let headers = HeaderMap::new();
        let body = Bytes::from(r#"{"model":"gpt-4o","messages":[]}"#);

        let request = build_openai_http_request(
            OpenAiRoute::ChatCompletions,
            &uri,
            Method::POST,
            &headers,
            body,
            None,
            &TracingHeaderConfig::default(),
        );

        assert_eq!(request.metadata.get(api_key_meta_keys::API_KEY_ID), None);
        assert_eq!(request.metadata.get(api_key_meta_keys::API_KEY_NAME), None);
        assert_eq!(
            request
                .metadata
                .get(api_key_meta_keys::API_KEY_ALLOWED_MODELS),
            None,
        );
        assert_eq!(
            request.metadata.get(api_key_meta_keys::API_KEY_PROJECT_ID),
            None,
        );
        // Route tag is still present even without API key metadata
        assert_eq!(
            request.metadata.get("openai_route"),
            Some(&serde_json::Value::from("/v1/chat/completions")),
        );
        Ok(())
    }

    /// Verify that empty allowed_models (= "no restriction") is stamped
    /// correctly as an empty string, distinguishing it from the `None` case
    /// (where the key is absent entirely).
    #[test]
    fn api_key_metadata_empty_allowed_models_is_empty_string() -> Result<(), Box<dyn Error>> {
        use crate::middleware::api_key_auth::api_key_meta_keys;
        use axum::http::Method;

        let meta = ValidatedApiKeyMetadata {
            api_key_id: 1,
            api_key_name: "unrestricted-key".to_string(),
            allowed_models: String::new(),
            project_id: 0,
            model_mapping: String::new(),
            ..ValidatedApiKeyMetadata::default()
        };

        let uri: Uri = "/v1/embeddings".parse()?;
        let headers = HeaderMap::new();
        let body = Bytes::from(r#"{"model":"text-embedding-3-small","input":"hello"}"#);

        let request = build_openai_http_request(
            OpenAiRoute::Embeddings,
            &uri,
            Method::POST,
            &headers,
            body,
            Some(&meta),
            &TracingHeaderConfig::default(),
        );

        // Empty string = unrestricted (distinguishable from None = no metadata)
        assert_eq!(
            request
                .metadata
                .get(api_key_meta_keys::API_KEY_ALLOWED_MODELS),
            Some(&serde_json::Value::from("")),
        );
        // project_id 0 = no project association
        assert_eq!(
            request.metadata.get(api_key_meta_keys::API_KEY_PROJECT_ID),
            Some(&serde_json::Value::from(0_i64)),
        );
        Ok(())
    }

    #[test]
    fn trace_thread_metadata_stamped_on_http_request() -> Result<(), Box<dyn Error>> {
        use axum::http::Method;

        let uri: Uri = "/v1/responses?thread_id=thread-from-query".parse()?;
        let headers =
            HeaderMap::from_iter([("Conduit-Trace-Id".parse()?, "trace-from-header".parse()?)]);
        let body = Bytes::from(r#"{"model":"gpt-4o","metadata":{"thread_id":"thread-from-body"}}"#);

        let request = build_openai_http_request(
            OpenAiRoute::Responses,
            &uri,
            Method::POST,
            &headers,
            body,
            None,
            &TracingHeaderConfig::default(),
        );

        assert_eq!(
            request.metadata.get("trace_key"),
            Some(&serde_json::Value::from("trace-from-header")),
        );
        assert_eq!(
            request.metadata.get("thread_key"),
            Some(&serde_json::Value::from("thread-from-query")),
        );
        assert_eq!(
            request.metadata.get("session_id"),
            Some(&serde_json::Value::from("trace-from-header")),
        );
        Ok(())
    }
    // =======================================================================
    // Route -> error-envelope mapping (Go: each route's inbound transformer
    // renders its own error shape via `Inbound.TransformError`).
    // =======================================================================

    /// Anthropic + Gemini routes must NOT render the OpenAI envelope; every
    /// other route must. Mirrors Go `transformOrchestratorError` delegating to
    /// `orch.Inbound.TransformError` (`api/upstream_error_policy.go:19-30`),
    /// where the inbound transformer is the route's own protocol adapter.
    #[test]
    fn route_error_format_matches_inbound_protocol() {
        use crate::error_middleware::ErrorResponseFormat;

        assert_eq!(
            OpenAiRoute::AnthropicMessages.error_format(),
            ErrorResponseFormat::AnthropicJson,
            "Claude clients must receive the native Anthropic envelope"
        );
        assert_eq!(
            OpenAiRoute::AnthropicCountTokens.error_format(),
            ErrorResponseFormat::AnthropicJson,
            "Anthropic count-token clients must receive the native envelope"
        );
        assert_eq!(
            OpenAiRoute::GeminiGenerateContent.error_format(),
            ErrorResponseFormat::GeminiJson,
            "Gemini clients must receive the native Gemini envelope"
        );

        // Every remaining route keeps the OpenAI-compatible envelope.
        for route in [
            OpenAiRoute::ChatCompletions,
            OpenAiRoute::Responses,
            OpenAiRoute::Embeddings,
            OpenAiRoute::AudioSpeech,
            OpenAiRoute::Videos,
            OpenAiRoute::ImageGenerations,
            OpenAiRoute::ImageEdits,
            OpenAiRoute::AudioTranscriptions,
            OpenAiRoute::AudioTranslations,
            OpenAiRoute::Completions,
            OpenAiRoute::ResponsesCompact,
            OpenAiRoute::JinaRerank,
            OpenAiRoute::JinaEmbeddings,
            OpenAiRoute::DoubaoCreateTask,
        ] {
            assert_eq!(
                route.error_format(),
                ErrorResponseFormat::OpenAiCompatibleJson,
                "route {route:?} must keep the OpenAI envelope"
            );
        }
    }
}
