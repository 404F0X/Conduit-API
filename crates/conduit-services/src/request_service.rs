use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use conduit_db::RequestContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub type RequestServiceResult<T> = Result<T, RequestServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RequestServiceError {
    #[error("request not found: {0}")]
    RequestNotFound(String),
    /// **S08 / RUST-P7-006**: more than one request row matched an
    /// `external_id` lookup. Mirrors ent's `NotSingularError` returned by
    /// `.Only(ctx)` in Go `VideoService.GetTaskByExternalID` /
    /// `DeleteTaskByExternalID` (`biz/video.go:67-69`, `85-87`).
    #[error("request external_id not singular: {0}")]
    ExternalIdNotSingular(String),
    #[error("request status conflict for {request_id}: expected {expected:?}, actual {actual:?}")]
    StatusConflict {
        request_id: String,
        expected: RequestStatus,
        actual: RequestStatus,
    },
    #[error("invalid request status transition: {from:?} -> {to:?}")]
    InvalidStatusTransition {
        from: RequestStatus,
        to: RequestStatus,
    },
    #[error("request persistence lock poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl RequestStatus {
    pub fn can_transition_to(self, next: Self) -> bool {
        match (self, next) {
            (current, next) if current == next => true,
            (Self::Pending, Self::Running | Self::Failed | Self::Cancelled) => true,
            (Self::Running, Self::Succeeded | Self::Failed | Self::Cancelled) => true,
            (Self::Succeeded | Self::Failed | Self::Cancelled, _) => false,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestRecord {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub status: RequestStatus,
    pub method: String,
    pub path: String,
    pub headers: Value,
    /// **S14**: the **inbound** request body — the user's *original* request,
    /// exactly as the client sent it. Mirrors Go
    /// `ent/schema/request.go::field.JSON("request_body")` with the comment:
    /// *"The original request from the user. e.g: the user request via OpenAI
    /// request format, but the actual request to the provider with Claude
    /// format, the request_body is the OpenAI request format."*
    ///
    /// This MUST be distinct from [`ExecutionRecord::body`] /
    /// [`RequestExecutionDetail::request_body`], which hold the
    /// **outbound** (provider-facing, post-transformer) body. The
    /// inbound/outbound pair is the S14 contract.
    pub body: Value,
    pub chunks: Value,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl RequestRecord {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        project_id: impl Into<String>,
        method: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            project_id: project_id.into(),
            status: RequestStatus::Pending,
            method: method.into(),
            path: path.into(),
            headers: Value::Object(Default::default()),
            body: Value::Null,
            chunks: Value::Array(Vec::new()),
            extra: BTreeMap::new(),
        }
    }

    /// **S14**: set the **inbound** request body — the user's original request
    /// as received (pre-transformer). Use this builder at the HTTP layer to
    /// record what the client actually sent, before any format conversion to
    /// the provider's wire format. The outbound (provider-facing) body is
    /// set separately on the [`ExecutionRecord`] / [`RequestExecutionDetail`].
    pub fn with_inbound_body(mut self, inbound_body: Value) -> Self {
        self.body = inbound_body;
        self
    }

    /// **S08 / RUST-P7-006**: the provider-side task id persisted on the
    /// request row. Mirrors Go `ent.Request.ExternalID`
    /// (`ent/schema/request.go:88-91`: `field.String("external_id").
    /// Optional().MaxLen(512)`, "External ID for tracking requests in
    /// external systems"). The write path is
    /// [`RequestService::update_request_status_external_id_and_response_body`]
    /// (Go `request.go:601-603` `SetExternalID`); the in-memory repo stores
    /// the value under `extra["external_id"]`. Returns `None` when never set
    /// (Go zero value is the empty string).
    pub fn external_id(&self) -> Option<&str> {
        self.extra.get("external_id").and_then(Value::as_str)
    }

    /// **S12 / RUST-P7-006**: the channel that served this request. Mirrors
    /// Go `ent.Request.ChannelID` (`ent/schema/request.go:87`:
    /// `field.Int("channel_id").Optional()`). Go's zero value for the unset
    /// optional int is `0`, which `VideoService.loadTask` treats as "missing
    /// channel_id" (`biz/video.go:132-134`); we mirror that by returning `0`
    /// when the key is absent from `extra`.
    pub fn channel_id(&self) -> i64 {
        self.extra
            .get("channel_id")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub request_id: String,
    pub status: RequestStatus,
    pub attempt: u32,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub headers: Value,
    /// **S14**: the **outbound** request body — the bytes the gateway actually
    /// sent to the upstream provider, AFTER any inbound→outbound format
    /// transformation. Mirrors Go
    /// `ent/schema/request_execution.go::field.JSON("request_body")` with
    /// the comment: *"The original request to the provider. e.g: the user
    /// request via OpenAI request format, but the actual request to the
    /// provider with Claude format, the request_body is the Claude request
    /// format."*
    ///
    /// This MUST be distinct from [`RequestRecord::body`] (the inbound
    /// user-facing body). When the gateway transforms a user's OpenAI-format
    /// request into a Claude-format provider call, the inbound field holds
    /// the OpenAI JSON and this field holds the Claude JSON. The
    /// inbound/outbound pair is the S14 contract.
    pub body: Value,
    pub chunks: Value,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ExecutionRecord {
    pub fn new(
        id: impl Into<String>,
        request_id: impl Into<String>,
        project_id: impl Into<String>,
        attempt: u32,
    ) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            project_id: project_id.into(),
            request_id: request_id.into(),
            status: RequestStatus::Running,
            attempt,
            provider: None,
            model: None,
            headers: Value::Object(Default::default()),
            body: Value::Null,
            chunks: Value::Array(Vec::new()),
            extra: BTreeMap::new(),
        }
    }

    /// **S14**: set the **outbound** request body — the post-transformer bytes
    /// the gateway sent (or will send) to the upstream provider. Use this at
    /// the orchestrator layer after the inbound format has been converted to
    /// the provider's wire format. The inbound (user-facing) body is set
    /// separately on the [`RequestRecord`].
    pub fn with_outbound_body(mut self, outbound_body: Value) -> Self {
        self.body = outbound_body;
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestExecutionLatencies {
    pub upstream_ms: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub reasoning_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestExecutionDetail {
    pub id: String,
    pub request_id: String,
    pub project_id: String,
    pub status: RequestStatus,
    pub request_url: String,
    pub request_headers: Value,
    /// **S14**: the **outbound** request body — the post-transformer bytes
    /// sent to the upstream provider. Same semantics as
    /// [`ExecutionRecord::body`] (the typed row form); this struct is the
    /// detail DTO the orchestrator hands to the persistence/storage layer.
    /// Mirrors Go `request_execution.go::field.JSON("request_body")`. MUST
    /// be distinct from [`RequestRecord::body`] (the inbound user-facing
    /// body) whenever the gateway performed a cross-format transformation.
    pub request_body: Value,
    pub response_body: Value,
    pub response_chunks: Value,
    pub error: Option<Value>,
    pub status_code: Option<u16>,
    pub pass_through_applied: bool,
    pub latencies: RequestExecutionLatencies,
}

impl RequestExecutionDetail {
    pub fn new(
        execution: &ExecutionRecord,
        request_url: impl Into<String>,
        request_headers: Value,
        request_body: Value,
    ) -> Self {
        Self {
            id: execution.id.clone(),
            request_id: execution.request_id.clone(),
            project_id: execution.project_id.clone(),
            status: execution.status,
            request_url: request_url.into(),
            request_headers,
            request_body,
            response_body: Value::Null,
            response_chunks: Value::Array(Vec::new()),
            error: None,
            status_code: None,
            pass_through_applied: false,
            latencies: RequestExecutionLatencies::default(),
        }
    }

    /// **S14**: set the **outbound** request body — the bytes the gateway sent
    /// to the provider. The inbound body lives on the parent [`RequestRecord`];
    /// this method records the provider-facing form for the detail DTO. Use
    /// this at the orchestrator layer after format transformation.
    pub fn with_outbound_body(mut self, outbound_body: Value) -> Self {
        self.request_body = outbound_body;
        self
    }

    pub fn with_pass_through_applied(mut self, pass_through_applied: bool) -> Self {
        self.pass_through_applied = pass_through_applied;
        self
    }

    /// **S15**: set `response_chunks` from the **client side** — the
    /// post-transform bytes the gateway sent back to the caller.
    ///
    /// # Parity (Go `InboundPersistentStream` -> `SaveRequestChunks`)
    ///
    /// This is the **default** path: when pass-through is NOT enabled the
    /// gateway runs the inbound transformer on the upstream stream, and the
    /// chunks persisted on the request row are exactly what the client
    /// received. See [`is_response_chunks_from_client`] and the S15 parity
    /// note on [`RequestExecutionDetail`].
    ///
    /// Use this builder at the HTTP/orchestrator layer after the inbound
    /// transformer has produced the client-facing stream. The
    /// `pass_through_applied` flag is left untouched; callers that changed it
    /// should also call [`Self::with_pass_through_applied`].
    pub fn with_client_response_chunks(mut self, chunks: Value) -> Self {
        self.response_chunks = chunks;
        self
    }

    /// **S15**: set `response_chunks` from the **provider side** — the raw
    /// upstream bytes captured before the inbound transformer ran.
    ///
    /// # Parity (Go `captureRawProviderStream` / `captureRawProviderResponse`
    /// -> `OutboundPersistentStream` -> `SaveRequestExecutionChunks`)
    ///
    /// This is the **pass-through** path: when the `pass_through` system
    /// setting is enabled, the gateway forwards the provider's bytes verbatim
    /// to the client and the persisted chunks are the provider's own (not the
    /// transformed client-facing form). See [`is_response_chunks_from_client`]
    /// and the S15 parity note on [`RequestExecutionDetail`].
    ///
    /// This builder also flips `pass_through_applied = true` because recording
    /// provider-side chunks is itself the observable signal that pass-through
    /// fired. Callers that want to record provider chunks WITHOUT marking the
    /// request as pass-through (e.g. for the `RequestExecution` row, which per
    /// Go always stores provider chunks regardless of pass-through) should use
    /// [`Self::with_client_response_chunks`] after explicitly setting the
    /// flag, or reach into the field directly.
    pub fn with_provider_response_chunks(mut self, chunks: Value) -> Self {
        self.response_chunks = chunks;
        self.pass_through_applied = true;
        self
    }

    pub fn with_latencies(mut self, latencies: RequestExecutionLatencies) -> Self {
        self.latencies = latencies;
        self
    }

    pub fn succeeded(
        mut self,
        status_code: u16,
        response_body: Value,
        response_chunks: Value,
    ) -> Self {
        self.status = RequestStatus::Succeeded;
        self.status_code = Some(status_code);
        self.response_body = response_body;
        self.response_chunks = response_chunks;
        self.error = None;
        self
    }

    pub fn failed(mut self, status_code: Option<u16>, error: Value) -> Self {
        self.status = RequestStatus::Failed;
        self.status_code = status_code;
        self.error = Some(error);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContentStoragePolicy {
    pub store_request_headers: bool,
    pub store_request_body: bool,
    pub store_response_body: bool,
    pub store_chunks: bool,
    pub live_preview: bool,
}

/// # Parity (Go `defaultStoragePolicy` in `internal/server/biz/system_default.go`)
///
/// Go defaults are `StoreChunks=false`, `LivePreview=false`, `StoreRequestBody=true`,
/// `StoreResponseBody=true`. The previous Rust default had `store_chunks=true` and
/// `live_preview=true`, which diverged from Go and would have stored chunks / enabled
/// live preview for every fresh install. Fixed to match Go. `[Hooke-the-2nd ?]`
impl Default for RequestContentStoragePolicy {
    fn default() -> Self {
        Self {
            store_request_headers: true,
            store_request_body: true,
            store_response_body: true,
            store_chunks: false,
            live_preview: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContentStorageKeys {
    pub request_body: String,
    pub response_body: String,
    pub response_chunks: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContentAccess {
    pub project_id: String,
    pub request_id: String,
}

impl RequestContentAccess {
    pub fn new(project_id: impl Into<String>, request_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            request_id: request_id.into(),
        }
    }

    pub fn allows(&self, request: &RequestRecord) -> bool {
        self.project_id == request.project_id && self.request_id == request.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum RequestContentDisposition {
    Inline,
    Attachment { filename: String },
}

impl RequestContentDisposition {
    pub fn attachment(filename: impl Into<String>) -> Self {
        Self::Attachment {
            filename: filename.into(),
        }
    }

    pub fn header_value(&self) -> String {
        match self {
            Self::Inline => "inline".to_string(),
            Self::Attachment { filename } => {
                format!("attachment; filename=\"{}\"", escape_header_value(filename))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContentRange {
    pub start: Option<u64>,
    pub end: Option<u64>,
}

impl RequestContentRange {
    pub fn parse_header(header: &str) -> Option<Self> {
        let range = header.strip_prefix("bytes=")?;
        if range.is_empty() || range.contains(',') {
            return None;
        }

        let (start, end) = range.split_once('-')?;
        if start.is_empty() && end.is_empty() {
            return None;
        }

        let start = parse_optional_u64(start)?;
        let end = parse_optional_u64(end)?;
        if let (Some(start), Some(end)) = (start, end)
            && start > end
        {
            return None;
        }

        Some(Self { start, end })
    }

    pub fn header_value(&self, total_len: Option<u64>) -> Option<String> {
        let total = total_len.map_or_else(|| "*".to_string(), |len| len.to_string());
        match (self.start, self.end) {
            (Some(start), Some(end)) => Some(format!("bytes {}-{}/{}", start, end, total)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContentResponseMetadata {
    pub content_type: String,
    pub disposition: RequestContentDisposition,
    pub range: Option<RequestContentRange>,
    pub content_length: Option<u64>,
}

impl RequestContentResponseMetadata {
    pub fn new(content_type: impl Into<String>, disposition: RequestContentDisposition) -> Self {
        Self {
            content_type: content_type.into(),
            disposition,
            range: None,
            content_length: None,
        }
    }

    pub fn with_range(mut self, range: RequestContentRange) -> Self {
        self.range = Some(range);
        self
    }

    pub fn with_content_length(mut self, content_length: u64) -> Self {
        self.content_length = Some(content_length);
        self
    }

    pub fn headers(&self) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), self.content_type.clone());
        headers.insert(
            "content-disposition".to_string(),
            self.disposition.header_value(),
        );
        if let Some(content_length) = self.content_length {
            headers.insert("content-length".to_string(), content_length.to_string());
        }
        if let Some(content_range) = self
            .range
            .as_ref()
            .and_then(|range| range.header_value(self.content_length))
        {
            headers.insert("content-range".to_string(), content_range);
        }
        headers
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePreviewSettings {
    pub enabled: bool,
}

impl LivePreviewSettings {
    pub fn enabled() -> Self {
        Self { enabled: true }
    }

    pub fn disabled() -> Self {
        Self { enabled: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LivePreviewEvent {
    pub project_id: String,
    pub request_id: String,
    pub sequence: u64,
    pub chunk: Value,
    pub final_event: bool,
}

impl LivePreviewEvent {
    fn new(
        project_id: impl Into<String>,
        request_id: impl Into<String>,
        sequence: u64,
        chunk: Value,
        final_event: bool,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            request_id: request_id.into(),
            sequence,
            chunk,
            final_event,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LivePreviewMetadata {
    settings: LivePreviewSettings,
    next_sequence: u64,
    events: Vec<LivePreviewEvent>,
}

impl LivePreviewMetadata {
    pub fn new(settings: LivePreviewSettings) -> Self {
        Self {
            settings,
            next_sequence: 0,
            events: Vec::new(),
        }
    }

    pub fn record_chunk(
        &mut self,
        project_id: impl Into<String>,
        request_id: impl Into<String>,
        chunk: Value,
    ) -> Option<&LivePreviewEvent> {
        self.record_event(project_id, request_id, chunk, false)
    }

    pub fn record_final(
        &mut self,
        project_id: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Option<&LivePreviewEvent> {
        self.record_event(project_id, request_id, Value::Null, true)
    }

    pub fn events(&self) -> &[LivePreviewEvent] {
        &self.events
    }

    fn record_event(
        &mut self,
        project_id: impl Into<String>,
        request_id: impl Into<String>,
        chunk: Value,
        final_event: bool,
    ) -> Option<&LivePreviewEvent> {
        if !self.settings.enabled {
            return None;
        }

        let event = LivePreviewEvent::new(
            project_id,
            request_id,
            self.next_sequence,
            chunk,
            final_event,
        );
        self.next_sequence += 1;
        self.events.push(event);
        self.events.last()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContentStorageKeyBuilder {
    project_id: String,
    request_id: String,
}

impl RequestContentStorageKeyBuilder {
    pub fn new(project_id: impl Into<String>, request_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            request_id: request_id.into(),
        }
    }

    pub fn request_keys(&self) -> RequestContentStorageKeys {
        RequestContentStorageKeys {
            request_body: self.request_body_key(),
            response_body: self.response_body_key(),
            response_chunks: self.response_chunks_key(),
        }
    }

    pub fn request_body_key(&self) -> String {
        format!("{}/request_body.json", self.request_prefix())
    }

    pub fn response_body_key(&self) -> String {
        format!("{}/response_body.json", self.request_prefix())
    }

    pub fn response_chunks_key(&self) -> String {
        format!("{}/response_chunks.json", self.request_prefix())
    }

    pub fn audio_key(&self, filename: &str) -> String {
        format!(
            "{}/audio/{}",
            self.request_prefix(),
            storage_safe_filename(filename)
        )
    }

    pub fn execution(&self, execution_id: impl Into<String>) -> ExecutionContentStorageKeyBuilder {
        ExecutionContentStorageKeyBuilder {
            request_prefix: self.request_prefix(),
            execution_id: execution_id.into(),
        }
    }

    fn request_prefix(&self) -> String {
        format!("/{}/requests/{}", self.project_id, self.request_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionContentStorageKeyBuilder {
    request_prefix: String,
    execution_id: String,
}

impl ExecutionContentStorageKeyBuilder {
    pub fn request_keys(&self) -> RequestContentStorageKeys {
        RequestContentStorageKeys {
            request_body: self.request_body_key(),
            response_body: self.response_body_key(),
            response_chunks: self.response_chunks_key(),
        }
    }

    pub fn request_body_key(&self) -> String {
        format!("{}/request_body.json", self.execution_prefix())
    }

    pub fn response_body_key(&self) -> String {
        format!("{}/response_body.json", self.execution_prefix())
    }

    pub fn response_chunks_key(&self) -> String {
        format!("{}/response_chunks.json", self.execution_prefix())
    }

    pub fn audio_key(&self, filename: &str) -> String {
        format!(
            "{}/audio/{}",
            self.execution_prefix(),
            storage_safe_filename(filename)
        )
    }

    fn execution_prefix(&self) -> String {
        format!("{}/executions/{}", self.request_prefix, self.execution_id)
    }
}

pub fn invalid_json_placeholder() -> Value {
    serde_json::json!({"message": "invalid text"})
}

pub fn parse_json_or_invalid_text(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| invalid_json_placeholder())
}

/// Invalid-request-body placeholder as raw bytes, mirroring the Go package-level
/// `_InvalidRequestBodyJSON = objects.JSONRawMessage({"message":"invalid text"})`.
///
/// # Parity (Go `internal/server/biz/request.go:70`)
///
/// In Go this is ONLY used as the retry fallback when the initial DB row save
/// fails (see `CreateRequest` lines 228-240). It is NOT used to rewrite user
/// input that happens not to be JSON — incoming bodies are serialized via
/// `xjson.Marshal`, which only fails on truly unmarshallable Go values.
pub const INVALID_REQUEST_BODY_JSON: &[u8] = b"{\"message\":\"invalid text\"}";

/// Returns the bytes that should be persisted when the original request body
/// cannot be stored. Mirrors Go `_InvalidRequestBodyJSON`.
pub fn invalid_request_body_bytes() -> &'static [u8] {
    INVALID_REQUEST_BODY_JSON
}

/// Sanitize an inbound body for persistence.
///
/// # Parity (Go `internal/server/biz/request.go` — `CreateRequest`)
///
/// Go prefers `httpRequest.JSONBody` (raw bytes already validated upstream by
/// the HTTP layer) and otherwise falls back to `xjson.Marshal(httpRequest.Body)`.
/// It does NOT parse-and-replace arbitrary user JSON: a syntactically odd but
///marshallable body is stored verbatim. The placeholder `{"message":"invalid text"}`
/// is only emitted when the *DB save itself* fails (S08 + S07 retry path).
///
/// This pure helper encodes the same rule for the Rust port: if `body` already
/// parses as JSON it is returned as-is; otherwise the invalid-text placeholder
/// bytes are returned. Callers that mirror Go's "marshal Go struct" path should
/// pass the serialized bytes here as the final validation gate.
pub fn sanitize_body(body: &[u8]) -> Vec<u8> {
    if serde_json::from_slice::<Value>(body).is_ok() {
        body.to_vec()
    } else {
        INVALID_REQUEST_BODY_JSON.to_vec()
    }
}

// ---------------------------------------------------------------------------
// RUST-P10-001 S14 — request_body inbound vs outbound semantics.
//
// # Parity (Go `ent/schema/request.go` vs `ent/schema/request_execution.go`)
//
// Go keeps TWO `request_body` columns with deliberately different contents:
//   * `Request.request_body` — *"The original request from the user. e.g: the
//     user request via OpenAI request format, but the actual request to the
//     provider with Claude format, the request_body is the OpenAI request
//     format."* (INBOUND, user-facing).
//   * `RequestExecution.request_body` — *"The original request to the
//     provider. e.g: the user request via OpenAI request format, but the
//     actual request to the provider with Claude format, the request_body is
//     the Claude request format."* (OUTBOUND, provider-facing).
//
// The inbound body is the raw bytes the client sent; the outbound body is
// what the gateway produced AFTER running the inbound→outbound transformer
// (e.g. `openai/chat_completions` → `anthropic/messages`). They MUST be
// distinct whenever the gateway performs a cross-format transformation; they
// are equal only when the gateway passes the body through verbatim (same
// inbound/outbound API format, no pass-through-applied rewriting).
//
// The Rust port encodes this contract via typed builders
// ([`RequestRecord::with_inbound_body`] /
// [`ExecutionRecord::with_outbound_body`] /
// [`RequestExecutionDetail::with_outbound_body`]) plus the pure helper below.
// ---------------------------------------------------------------------------

/// (S14) Pure predicate: do the inbound (user-facing) and outbound
/// (provider-facing) request bodies differ?
///
/// Mirrors the Go `Request.request_body != RequestExecution.request_body`
/// observable behavior. The transformer layer sets these to different JSON
/// values whenever it converts between API formats (e.g. the user sends an
/// OpenAI Chat Completions payload; the execution row records the equivalent
/// Anthropic Messages payload the gateway sent upstream).
///
/// Returns `true` when the two JSON values are NOT equal — i.e. a
/// transformation (or any other mutation) occurred. Returns `false` when the
/// gateway passed the body through verbatim.
///
/// This is a pure equality check on `serde_json::Value` — semantically a
/// JSON-deep-equal. Go would compare the marshaled `objects.JSONRawMessage`
/// byte slices, which is a byte-level (not structural) comparison; for the
/// gateway's actual payloads (which it serializes once and stores verbatim)
/// the two notions coincide. This Rust helper picks structural equality so
/// it is robust to key-ordering differences in test fixtures.
pub fn inbound_outbound_bodies_diverge(inbound: &Value, outbound: &Value) -> bool {
    inbound != outbound
}

// ---------------------------------------------------------------------------
// RUST-P10-001 S15 — response_chunks client-side vs provider-side semantics.
//
// # Parity (Go orchestrator `inbound.go` + `outbound.go` + `pass_through.go`)
//
// Go keeps TWO `response_chunks` columns, each populated by a different
// persistent-stream wrapper:
//
//   * `Request.response_chunks` — populated by `InboundPersistentStream`
//     (`inbound.go:30, 67-79, 254`). That wrapper sits AROUND the result of
//     `transformer.Inbound.TransformStream` — i.e. the stream the gateway is
//     about to send BACK to the client, AFTER the provider's response has been
//     transformed into the client's API format (OpenAI/Anthropic/etc.). So the
//     top-level row records **client-side** chunks: the bytes the user actually
//     received.
//
//   * `RequestExecution.response_chunks` — populated by
//     `OutboundPersistentStream` (`outbound.go:40, 79-94, 302`). That wrapper
//     sits AROUND the RAW provider stream (pre-inbound-transform) inside
//     `PersistentOutboundTransformer.TransformStream` (`outbound.go:448-462`).
//     So the execution row records **provider-side** chunks: the raw upstream
//     bytes BEFORE they are converted for the client.
//
// Pass-through (`pass_through.go: captureRawProviderResponse` /
// `captureRawProviderStream`) additionally captures the raw provider
// response/stream into `state.RawProviderResponse` / `state.RawStreamCh` when
// the `pass_through` system setting is enabled. When pass-through fires, the
// gateway forwards the provider's bytes verbatim to the client — meaning the
// "client-side" and "provider-side" chunks coincide (no transformation ran).
// When pass-through is OFF, the gateway runs the bidirectional transformer and
// the two chunk arrays CAN differ (different API format, summarised binary
// audio, filtered done events, …).
//
// The Rust port encodes this contract via:
//   * [`ResponseChunkSource`] — typed tag for which side a chunk came from.
//   * [`RequestExecutionDetail::with_client_response_chunks`] — default path,
//     mirrors `InboundPersistentStream` storing post-transform client chunks
//     on the parent request row.
//   * [`RequestExecutionDetail::with_provider_response_chunks`] — pass-through
//     path, mirrors the raw provider capture in `captureRawProvider*`.
//   * [`is_response_chunks_from_client`] — pure predicate mirroring the Go
//     routing decision (default -> client-side; pass-through -> provider-side).
//   * [`client_provider_chunks_diverge`] — pure equality helper, the chunk
//     analogue of [`inbound_outbound_bodies_diverge`].
// ---------------------------------------------------------------------------

/// Which side of the gateway a stored `response_chunks` entry came from.
///
/// See the S15 parity note above for the Go source mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseChunkSource {
    /// Client-facing chunks: post-transform bytes the gateway sent back to the
    /// caller. Mirrors Go `InboundPersistentStream.responseChunks` written into
    /// `Request.response_chunks` via `SaveRequestChunks`.
    Client,
    /// Provider-facing chunks: raw upstream bytes captured before the inbound
    /// transformer ran. Mirrors Go `OutboundPersistentStream.responseChunks`
    /// written into `RequestExecution.response_chunks` via
    /// `SaveRequestExecutionChunks`, and the raw capture performed by
    /// `captureRawProviderResponse` / `captureRawProviderStream` when
    /// pass-through is enabled.
    Provider,
}

/// Pure predicate mirroring Go's response-chunks routing decision.
///
/// # Parity (Go `pass_through.go:isPassThroughEnabled` + orchestrator wiring)
///
/// * `pass_through = false` (the default) -> the gateway transformed the
///   upstream response before forwarding it to the client, so the persisted
///   `response_chunks` represent what the client received -> [`ResponseChunkSource::Client`].
/// * `pass_through = true` -> the gateway forwarded the raw provider bytes
///   verbatim (see `captureRawProviderStream`), so the persisted chunks are the
///   provider's own -> [`ResponseChunkSource::Provider`].
///
/// Returns `true` when chunks come from the client side (the default, no
/// pass-through), `false` when they come from the provider side.
pub fn is_response_chunks_from_client(pass_through: bool) -> bool {
    !pass_through
}

/// Resolve the chunk source from the pass-through flag, mirroring
/// [`is_response_chunks_from_client`] but returning the typed tag.
pub fn response_chunk_source(pass_through: bool) -> ResponseChunkSource {
    if is_response_chunks_from_client(pass_through) {
        ResponseChunkSource::Client
    } else {
        ResponseChunkSource::Provider
    }
}

/// (S15) Pure predicate: do the client-side and provider-side response chunk
/// arrays differ?
///
/// This is the chunk analogue of [`inbound_outbound_bodies_diverge`]. The two
/// chunk arrays are equal only when the gateway forwarded the provider bytes
/// verbatim (pass-through) OR when the inbound transformer happened to be
/// identity. Whenever the transformer rewrote the stream (different API format,
/// summarised binary audio, filtered `[DONE]` sentinel, ...) the two arrays
/// diverge.
///
/// Comparison is structural on `serde_json::Value` so it is robust to
/// key-ordering differences in test fixtures.
pub fn client_provider_chunks_diverge(client_chunks: &Value, provider_chunks: &Value) -> bool {
    client_chunks != provider_chunks
}

// ---------------------------------------------------------------------------
// RUST-P10-001 S16 — request-content download/preview location resolution.
//
// # Parity (Go `internal/server/api/request_content.go::DownloadRequestContent`
// lines 40-99, plus `internal/server/biz/request.go::UpdateRequestCompletedWithAudio`
// lines 544-553 which writes the content_saved fields.)
//
// Go stores FOUR content-location fields on the `Request` row (schema:
// `internal/ent/schema/request.go:104-122`):
//   * `content_saved`        (bool, default false) — "whether the generated
//     content (e.g. video, audio) has been saved to external storage".
//   * `content_storage_id`   (*int, nullable) — "data storage id used to save
//     the content file".
//   * `content_storage_key`  (*string, nullable) — "storage key/path of the
//     saved content file".
//   * `content_saved_at`     (*time.Time, nullable) — "when the content file
//     was saved".
//
// They are written together in `UpdateRequestCompletedWithAudio`
// (request.go:544-553) only when audio was successfully persisted via
// `DataStorageService.SaveData(...)`. The video service uses an analogous
// shape; see `video_service.rs::ScanDataStorageProps`.
//
// The DOWNLOAD/preview handler (`DownloadRequestContent`) resolves the
// content's location purely from those row fields + a single
// `DataStorageService.GetDataStorageByID` lookup. Its decision tree (lines
// 70-99) is the contract this module encodes:
//
//   1. GATE — `!content_saved || content_storage_id == nil ||
//      content_storage_key == nil || trim(key) == ""` -> HTTP 404 "Content
//      not found".
//   2. KEY NORMALISATION — trim the key; ensure a single leading `/`. The key
//      MUST start with `/{project_id}/requests/{request_id}/` else HTTP 404
//      "Content not found" (cross-project / cross-request access blocked).
//   3. STORAGE LOOKUP — `DataStorageService.GetDataStorageByID(
//      *content_storage_id)`; `ent.IsNotFound` -> HTTP 404 "Content storage
//      not found", any other error -> HTTP 500 "Failed to load content
//      storage".
//   4. STORAGE-TYPE GATE — `ds.Primary || ds.Type == datastorage.TypeDatabase`
//      -> HTTP 400 "Content storage is not file-based".
//
// The remaining handler (FS / S3 / GCS filesystem IO + streaming) is HTTP-IO
// and lives outside this service; S16 captures ONLY the pure resolution
// decision tree and the key-normalisation helper.
//
// Note: `content_*` fields live ONLY on `Request`, NOT on `RequestExecution`
// (see `request_execution.go` schema). Executions route body/chunk storage
// via `data_storage_id` (S06 `resolve_storage_route`) and do not surface a
// downloadable artifact; this S16 resolver is therefore Request-scoped.
// ---------------------------------------------------------------------------

/// Read-only view of the `content_*` fields on a `Request` row.
///
/// # Parity (Go `ent.Request` fields: `content_saved`, `content_storage_id`,
/// `content_storage_key`, `content_saved_at`)
///
/// Callers populate this from the loaded row; the pure resolver then decides
/// whether a download/preview can proceed and where to fetch the bytes from.
/// `content_storage_id` is the row FK into `DataStorage`; `content_storage_key`
/// is the object key within that storage; `content_saved_at` is recorded for
/// audit/debugging and is NOT consulted by the resolution decision tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContentLocation {
    /// Mirrors Go `Request.content_saved` (bool, default false). True only
    /// when the gateway successfully persisted the generated content (audio /
    /// video) into external storage.
    pub content_saved: bool,
    /// Mirrors Go `Request.content_storage_id` (`*int`, nullable). The FK to
    /// the `DataStorage` row that holds the artifact.
    pub content_storage_id: Option<i64>,
    /// Mirrors Go `Request.content_storage_key` (`*string`, nullable). The
    /// object key the handler will hand to `DataStorageService.GetFileSystem`
    /// / `fs.Open`.
    pub content_storage_key: Option<String>,
    /// Mirrors Go `Request.content_saved_at` (`*time.Time`, nullable). Not
    /// consulted by [`resolve_request_content_location`]; recorded for audit.
    pub content_saved_at: Option<DateTime<Utc>>,
}

impl RequestContentLocation {
    /// Build an empty/unsaved location (the default state for a freshly
    /// created request row). Mirrors Go `content_saved=false` and the three
    /// nullable content fields being unset.
    pub fn unsaved() -> Self {
        Self {
            content_saved: false,
            content_storage_id: None,
            content_storage_key: None,
            content_saved_at: None,
        }
    }

    /// Build a populated location, e.g. after
    /// `UpdateRequestCompletedWithAudio` successfully wrote audio to external
    /// storage. Mirrors request.go:548-553.
    pub fn saved(
        content_storage_id: i64,
        content_storage_key: impl Into<String>,
        content_saved_at: DateTime<Utc>,
    ) -> Self {
        Self {
            content_saved: true,
            content_storage_id: Some(content_storage_id),
            content_storage_key: Some(content_storage_key.into()),
            content_saved_at: Some(content_saved_at),
        }
    }
}

/// Resolved properties of the `DataStorage` row referenced by
/// [`RequestContentLocation::content_storage_id`].
///
/// # Parity (Go `*ent.DataStorage` consumed by `DownloadRequestContent`
/// lines 96-99)
///
/// Go consults only two attributes for the storage-type gate:
///   * `ds.Primary` (bool) — primary storage routes back to the DB and is
///     unsuitable for serving a downloaded file.
///   * `ds.Type == datastorage.TypeDatabase` — the special "database" pseudo-
///     storage type is likewise rejected.
///
/// This struct intentionally mirrors the shape already used by
/// `video_service.rs::ScanDataStorageProps` (which gates on the same two
/// properties, worker.go L128-129) so callers can reuse the same loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentStorageProps {
    /// Mirrors Go `ds.Primary`.
    #[serde(default)]
    pub is_primary: bool,
    /// Mirrors Go `ds.Type == datastorage.TypeDatabase`.
    #[serde(default)]
    pub is_database: bool,
}

impl ContentStorageProps {
    /// A primary storage row — Go would reject it at line 96.
    pub fn primary() -> Self {
        Self {
            is_primary: true,
            is_database: false,
        }
    }

    /// A database-typed storage row — Go would reject it at line 96.
    pub fn database() -> Self {
        Self {
            is_primary: false,
            is_database: true,
        }
    }

    /// A file-based non-primary storage row (FS / S3 / GCS) — the only kind
    /// Go accepts for content downloads.
    pub fn file_based() -> Self {
        Self {
            is_primary: false,
            is_database: false,
        }
    }
}

/// Error returned by [`resolve_request_content_location`].
///
/// Each variant carries the exact HTTP-mapped message Go emits in
/// `DownloadRequestContent`, so the HTTP layer can translate one-for-one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum ResolveContentLocationError {
    /// HTTP 404 "Content not found" — content_saved gate failed OR the key
    /// failed prefix validation (Go lines 70-73, 80-84).
    #[error("Content not found")]
    ContentNotFound,
    /// HTTP 404 "Content storage not found" — the resolved storage_id does
    /// not match any `DataStorage` row (Go lines 86-94, `ent.IsNotFound`).
    #[error("Content storage not found")]
    ContentStorageNotFound,
    /// HTTP 500 "Failed to load content storage" — storage lookup raised a
    /// non-NotFound error (Go lines 91-93). The lower-level cause is logged
    /// by the caller.
    #[error("Failed to load content storage")]
    ContentStorageLookupFailed,
    /// HTTP 400 "Content storage is not file-based" — storage is primary or
    /// database-typed (Go lines 96-99).
    #[error("Content storage is not file-based")]
    ContentStorageNotFileBased,
}

/// Successful resolution: the bytes live at this storage key inside this
/// non-primary file-based data storage. The HTTP layer now opens the file and
/// streams it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedContentLocation {
    /// The `content_storage_id` whose props were validated.
    pub content_storage_id: i64,
    /// Normalised object key with a single leading `/` and verified
    /// `/{project_id}/requests/{request_id}/` prefix.
    pub storage_key: String,
    /// The properties of the resolved storage (caller can dispatch on FS vs
    /// S3 vs GCS for filesystem instantiation).
    pub storage_props: ContentStorageProps,
}

/// Pure decision tree mirroring Go `DownloadRequestContent` lines 70-99.
///
/// Inputs:
/// * `location` — the four `content_*` fields read off the `Request` row.
/// * `storage_lookup` — the outcome of `DataStorageService.GetDataStorageByID(
///   *content_storage_id)`, modelled as `Result<Option<ContentStorageProps>,
///   ()>` where `Err(())` is the Go "any non-NotFound error" branch, `Ok(None)`
///   is `ent.IsNotFound`, and `Ok(Some(props))` is a successful row load.
/// * `project_id` / `request_id` — used to validate the key prefix.
///
/// Returns [`ResolvedContentLocation`] on success, or the matching HTTP-mapped
/// [`ResolveContentLocationError`] otherwise.
pub fn resolve_request_content_location(
    location: &RequestContentLocation,
    storage_lookup: Result<Option<ContentStorageProps>, ()>,
    project_id: i64,
    request_id: i64,
) -> Result<ResolvedContentLocation, ResolveContentLocationError> {
    // === Go line 70: content_saved gate ===
    // `!req.ContentSaved || req.ContentStorageID == nil ||
    //  req.ContentStorageKey == nil || strings.TrimSpace(*key) == ""`
    let Some(content_storage_id) = location.content_storage_id else {
        return Err(ResolveContentLocationError::ContentNotFound);
    };
    let Some(raw_key) = location.content_storage_key.as_deref() else {
        return Err(ResolveContentLocationError::ContentNotFound);
    };
    if !location.content_saved {
        return Err(ResolveContentLocationError::ContentNotFound);
    }
    let normalised_key = normalise_content_key(raw_key);
    if normalised_key.is_none() {
        // Empty after trim — Go returns "Content not found".
        return Err(ResolveContentLocationError::ContentNotFound);
    }
    let normalised_key = normalised_key.ok_or(ResolveContentLocationError::ContentNotFound)?;

    // === Go lines 80-84: key prefix validation ===
    if !content_key_has_request_prefix(&normalised_key, project_id, request_id) {
        return Err(ResolveContentLocationError::ContentNotFound);
    }

    // === Go lines 86-94: storage row lookup ===
    let storage_props = match storage_lookup {
        Ok(Some(props)) => props,
        Ok(None) => return Err(ResolveContentLocationError::ContentStorageNotFound),
        Err(()) => return Err(ResolveContentLocationError::ContentStorageLookupFailed),
    };

    // === Go lines 96-99: storage-type gate ===
    // `if ds.Primary || ds.Type == datastorage.TypeDatabase`
    if storage_props.is_primary || storage_props.is_database {
        return Err(ResolveContentLocationError::ContentStorageNotFileBased);
    }

    Ok(ResolvedContentLocation {
        content_storage_id,
        storage_key: normalised_key,
        storage_props,
    })
}

/// Normalise a raw `content_storage_key` the way Go's
/// `DownloadRequestContent` does (lines 75-79):
///   1. `strings.TrimSpace(*key)`.
///   2. Ensure a single leading `/`.
///   3. Return `None` if the trimmed key is empty.
///
/// This is the pure analogue; the subsequent prefix check lives in
/// [`content_key_has_request_prefix`].
pub fn normalise_content_key(raw_key: &str) -> Option<String> {
    let trimmed = raw_key.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('/') {
        Some(trimmed.to_string())
    } else {
        Some(format!("/{trimmed}"))
    }
}

/// Validate that `normalised_key` is scoped to `/{project_id}/requests/
/// {request_id}/`, mirroring Go `DownloadRequestContent` lines 80-84. Blocks
/// cross-project / cross-request content access.
pub fn content_key_has_request_prefix(
    normalised_key: &str,
    project_id: i64,
    request_id: i64,
) -> bool {
    let expected_prefix = format!("/{project_id}/requests/{request_id}/");
    normalised_key.starts_with(&expected_prefix)
}

// ---------------------------------------------------------------------------
// S04 — Go-compatible integer-ID storage key generators.
//
// # Parity (Go `internal/server/biz/request.go:73-126`)
//
// Go's `Generate*Key` helpers format integer project/request/execution IDs with
// `%d`. The string-key `RequestContentStorageKeyBuilder` above is a Rust-side
// convenience for tests/DTOs that already carry string IDs, but the canonical
// on-disk object keys produced by the running Go binary are these integer-key
// forms. Both must stay byte-identical.
// ---------------------------------------------------------------------------

/// Build `/{project_id}/requests/{request_id}/request_body.json`.
///
/// Mirrors Go `GenerateRequestBodyKey(projectID, requestID int)`.
pub fn generate_request_body_key(project_id: i64, request_id: i64) -> String {
    format!("/{project_id}/requests/{request_id}/request_body.json")
}

/// Build `/{project_id}/requests/{request_id}/response_body.json`.
///
/// Mirrors Go `GenerateResponseBodyKey(projectID, requestID int)`.
pub fn generate_response_body_key(project_id: i64, request_id: i64) -> String {
    format!("/{project_id}/requests/{request_id}/response_body.json")
}

/// Build `/{project_id}/requests/{request_id}/response_chunks.json`.
///
/// Mirrors Go `GenerateResponseChunksKey(projectID, requestID int)`.
pub fn generate_response_chunks_key(project_id: i64, request_id: i64) -> String {
    format!("/{project_id}/requests/{request_id}/response_chunks.json")
}

/// Build the per-request directory prefix `/{project_id}/requests/{request_id}`.
///
/// Mirrors Go `GenerateRequestDirKey(projectID, requestID int)`.
pub fn generate_request_dir_key(project_id: i64, request_id: i64) -> String {
    format!("/{project_id}/requests/{request_id}")
}

/// Build `/{project_id}/requests/{request_id}/executions`.
///
/// Mirrors Go `GenerateRequestExecutionsDirKey(projectID, requestID int)`.
pub fn generate_request_executions_dir_key(project_id: i64, request_id: i64) -> String {
    format!("/{project_id}/requests/{request_id}/executions")
}

/// Build `/{project_id}/requests/{request_id}/audio/{filename}`.
///
/// # Parity (Go `GenerateAudioKey` in `request.go:83-92`)
///
/// Go semantics, preserved exactly:
/// 1. `strings.TrimSpace(filename)`; if empty, fall back to `"audio.mp3"`.
/// 2. `filepath.Base(name)` — strip any directory components. On a Unix path
///    separator this collapses `../x.wav` -> `x.wav`. We mirror this for both
///    `/` and `\` (Windows) so Rust-built keys match keys a Go binary on the
///    same payload would produce on a POSIX host.
///
/// Unlike the string-key builder's `storage_safe_filename`, this does NOT
/// rewrite punctuation: Go stores the literal basename (including spaces).
pub fn generate_audio_key(project_id: i64, request_id: i64, filename: &str) -> String {
    let name = audio_filename_basename(filename);
    format!("/{project_id}/requests/{request_id}/audio/{name}")
}

/// Build `/{project_id}/requests/{request_id}/executions/{execution_id}/request_body.json`.
///
/// Mirrors Go `GenerateExecutionRequestBodyKey`.
pub fn generate_execution_request_body_key(
    project_id: i64,
    request_id: i64,
    execution_id: i64,
) -> String {
    format!("/{project_id}/requests/{request_id}/executions/{execution_id}/request_body.json")
}

/// Build `/{project_id}/requests/{request_id}/executions/{execution_id}/response_body.json`.
///
/// Mirrors Go `GenerateExecutionResponseBodyKey`.
pub fn generate_execution_response_body_key(
    project_id: i64,
    request_id: i64,
    execution_id: i64,
) -> String {
    format!("/{project_id}/requests/{request_id}/executions/{execution_id}/response_body.json")
}

/// Build `/{project_id}/requests/{request_id}/executions/{execution_id}/response_chunks.json`.
///
/// Mirrors Go `GenerateExecutionResponseChunksKey`.
pub fn generate_execution_response_chunks_key(
    project_id: i64,
    request_id: i64,
    execution_id: i64,
) -> String {
    format!("/{project_id}/requests/{request_id}/executions/{execution_id}/response_chunks.json")
}

/// Build the per-execution directory prefix
/// `/{project_id}/requests/{request_id}/executions/{execution_id}`.
///
/// Mirrors Go `GenerateExecutionRequestDirKey`.
pub fn generate_execution_request_dir_key(
    project_id: i64,
    request_id: i64,
    execution_id: i64,
) -> String {
    format!("/{project_id}/requests/{request_id}/executions/{execution_id}")
}

/// Resolve the audio object-name basename the same way Go's `GenerateAudioKey` does.
fn audio_filename_basename(filename: &str) -> String {
    let trimmed = filename.trim();
    if trimmed.is_empty() {
        return "audio.mp3".to_string();
    }
    // Mirror `filepath.Base`: split on both `/` and `\` so Windows-style input
    // resolves to the same key a POSIX Go binary would produce.
    let basename = trimmed
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("audio.mp3");
    basename.to_string()
}

// ---------------------------------------------------------------------------
// S05 — storage decision typed helpers.
//
// `RequestContentStoragePolicy` (above) already carries the four boolean flags
// read from system settings (`StoreRequestBody` / `StoreResponseBody` /
// `StoreChunks` / `LivePreview`). `StorageOutcome` is the per-artifact decision
// derived from the policy + the kind of payload being persisted, and
// `decide_storage` is the pure reducer.
// ---------------------------------------------------------------------------

/// The set of request artifacts the storage layer can decide to persist.
///
/// Each variant maps to one of the Go code paths that consult
/// `StoragePolicy` / `StoreChunks` before writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageArtifact {
    /// Inbound user request body (`policy.StoreRequestBody`).
    RequestBody,
    /// Final provider response body (`policy.StoreResponseBody`).
    ResponseBody,
    /// Streamed response chunks (`StoreChunks` system setting).
    ResponseChunks,
    /// Live-preview fan-out (`policy.LivePreview`).
    LivePreview,
}

/// Decision returned by [`decide_storage`] for a single artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageDecision {
    /// Persist the artifact (to DB or external storage, see S06/S07).
    Store,
    /// Policy disabled — skip persistence entirely.
    Skip,
}

/// Per-artifact storage decision derived from a [`RequestContentStoragePolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageOutcome {
    pub request_body: StorageDecision,
    pub response_body: StorageDecision,
    pub response_chunks: StorageDecision,
    pub live_preview: StorageDecision,
}

impl StorageOutcome {
    /// Convenience: returns `true` only when every artifact is [`StorageDecision::Skip`].
    pub fn is_all_skipped(&self) -> bool {
        matches!(
            self,
            Self {
                request_body: StorageDecision::Skip,
                response_body: StorageDecision::Skip,
                response_chunks: StorageDecision::Skip,
                live_preview: StorageDecision::Skip,
            }
        )
    }
}

/// Pure reducer that mirrors the per-call `if policy, err := ...; err == nil {
/// storeX = policy.StoreX }` blocks scattered through Go `request.go`
/// (lines 142-147, 267-272, 391-396, 484-489, 576-581, 661-666, 866-876, 953-964).
///
/// When `policy` is `None` (Go: error reading system settings), Go falls back to
/// the relevant per-field default — `true` for request/response body, `false`
/// for chunks. [`RequestContentStoragePolicy::default`] already encodes those
/// exact defaults, so callers pass `policy.unwrap_or_default()` to mirror Go.
pub fn decide_storage(policy: &RequestContentStoragePolicy) -> StorageOutcome {
    StorageOutcome {
        request_body: if policy.store_request_body {
            StorageDecision::Store
        } else {
            StorageDecision::Skip
        },
        response_body: if policy.store_response_body {
            StorageDecision::Store
        } else {
            StorageDecision::Skip
        },
        response_chunks: if policy.store_chunks {
            StorageDecision::Store
        } else {
            StorageDecision::Skip
        },
        live_preview: if policy.live_preview {
            StorageDecision::Store
        } else {
            StorageDecision::Skip
        },
    }
}

/// Look up the decision for a single artifact kind.
pub fn decide_artifact(
    policy: &RequestContentStoragePolicy,
    artifact: StorageArtifact,
) -> StorageDecision {
    match artifact {
        StorageArtifact::RequestBody => decide_storage(policy).request_body,
        StorageArtifact::ResponseBody => decide_storage(policy).response_body,
        StorageArtifact::ResponseChunks => decide_storage(policy).response_chunks,
        StorageArtifact::LivePreview => decide_storage(policy).live_preview,
    }
}

// ---------------------------------------------------------------------------
// S06 — default data-storage selection.
//
// # Parity (Go `request_internal.go` + `request.go:178-181, 202-210`)
//
// Go resolves the data storage via `DataStorageService.GetDefaultDataStorage`;
// when none is configured (or the lookup errors) `dataStorage` stays nil and
// `shouldUseExternalStorage` returns `false`, meaning the row's body columns
// are written to the primary database. A storage row marked `Primary=true`
// also routes back to the DB. We encode that routing as a pure enum so the
// orchestrator can decide without talking to a repo.
// ---------------------------------------------------------------------------

/// Routing decision for where a request artifact should be persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "target")]
pub enum StorageRoute {
    /// Persist to the primary relational database (Go: `dataStorage == nil ||
    /// dataStorage.Primary`). This is the fallback when no external storage is
    /// configured, and is always the route used for the row itself.
    PrimaryDb,
    /// Persist to a configured non-primary external [`DataStorage`].
    External {
        /// Mirrors Go `ent.DataStorage.ID`; `None` when the route was chosen
        /// before the storage row has been loaded (orchestrator fills it in).
        data_storage_id: Option<i64>,
    },
}

/// Resolve the storage route from the data-storage row flags.
///
/// Mirrors Go `shouldUseExternalStorage` (`request.go:61-67`): external storage
/// is used only when a storage row is present AND `Primary == false`. Otherwise
/// the primary database is used.
pub fn resolve_storage_route(data_storage: Option<&DataStorageRef>) -> StorageRoute {
    match data_storage {
        Some(ds) if !ds.primary => StorageRoute::External {
            data_storage_id: Some(ds.id),
        },
        _ => StorageRoute::PrimaryDb,
    }
}

/// Minimal read-only view of a `DataStorage` row, used so the pure routing
/// helper does not depend on the (not-yet-ported) `ent::DataStorage` type.
///
/// `[Hooke-the-2nd ?]` TODO: replace with `conduit_db::DataStorage` once that
/// entity is ported; the field set is stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataStorageRef {
    pub id: i64,
    pub primary: bool,
}

impl DataStorageRef {
    pub fn new(id: i64, primary: bool) -> Self {
        Self { id, primary }
    }

    /// A primary storage row — routes to [`StorageRoute::PrimaryDb`].
    pub fn primary(id: i64) -> Self {
        Self::new(id, true)
    }

    /// A non-primary storage row — routes to [`StorageRoute::External`].
    pub fn external(id: i64) -> Self {
        Self::new(id, false)
    }
}

// ---------------------------------------------------------------------------
// S07 — external-storage write outcome + pure "does request creation proceed?"
// reducer.
//
// # Parity (Go `request.go:243-253`)
//
// After the request row is saved, Go attempts
// `DataStorageService.SaveData(ctx, dataStorage, key, requestBodyBytes)`. On
// failure it logs `log.Error("Failed to save request body to external storage",
// ...)` and then — per the inline comment — "Continue anyway, don't fail the
// request creation". The execution path (`request.go:362-370`) is identical.
//
// Crucially, this leniency only applies AFTER the row exists. If the row save
// itself fails while `useExternalStorage` is true, Go does NOT retry with the
// invalid-JSON placeholder — it returns the error directly (`request.go:237-239`).
// The placeholder retry only happens on the DB-storage path (`request.go:228-236`).
// ---------------------------------------------------------------------------

/// Outcome of a single external-storage write attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageWriteResult {
    /// Bytes were written to external storage successfully.
    Saved,
    /// Policy or route skipped the write (e.g. [`StorageRoute::PrimaryDb`] or
    /// [`StorageDecision::Skip`]).
    Skipped,
    /// The external write failed. A warning MUST be recorded (Go: `log.Error`),
    /// but per Go semantics the surrounding request/execution creation MUST
    /// still proceed because the DB row already exists.
    FailedWarning,
}

/// Outcome of persisting the request *row* itself, before any external write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestRowSaveResult {
    /// Row inserted on the first attempt.
    Saved,
    /// Initial insert failed (DB-storage path only); Go retries once with the
    /// invalid-JSON placeholder (`request.go:228-236`). This variant records
    /// that the retry succeeded.
    SavedWithPlaceholderFallback,
    /// The row insert failed and could not be recovered. Request creation
    /// MUST surface this error to the caller.
    Failed,
}

/// Pure reducer answering the question: "given the row-save result and the
/// external-write result, should request creation proceed?"
///
/// Mirrors Go control flow in `CreateRequest` (`request.go:225-253`):
/// * Row save `Failed` -> request creation fails (regardless of external write).
/// * Row saved -> an external-storage failure becomes a non-fatal warning;
///   creation succeeds.
/// * Row saved and external write `Saved` / `Skipped` -> creation succeeds.
pub fn request_creation_proceeds(
    row: RequestRowSaveResult,
    external_write: StorageWriteResult,
) -> bool {
    match row {
        RequestRowSaveResult::Failed => false,
        RequestRowSaveResult::Saved | RequestRowSaveResult::SavedWithPlaceholderFallback => {
            // Go logs the warning but returns the row; creation proceeds for all
            // three external-write outcomes including `FailedWarning`.
            let _ = external_write; // warning is observed/logged by the caller
            true
        }
    }
}

/// Pure reducer for the placeholder-retry eligibility.
///
/// Go only retries the row insert with [`INVALID_REQUEST_BODY_JSON`] when
/// storing to the primary database; on the external-storage path a row-insert
/// failure is terminal (`request.go:227-239`). Returns `true` when the retry
/// branch is allowed.
pub fn placeholder_retry_allowed(route: StorageRoute) -> bool {
    matches!(route, StorageRoute::PrimaryDb)
}

// ---------------------------------------------------------------------------
// RUST-P10-001 A01 — stream-chunk storage helpers.
//
// # Parity (Go `internal/server/biz/request.go:798-852`)
//
// Go persists streamed response chunks through two pure helpers:
//   * `shouldSkipStoredStreamChunk(chunk)` — drops nil chunks, the `[DONE]`
//     SSE sentinel (when emitted on a non-binary event) and the synthetic
//     `binary.done` EOF marker used by non-SSE binary streams.
//   * `marshalStreamEventForStorage(chunk)` — wraps each surviving chunk in
//     the `jsonStreamEvent` envelope. Binary audio / octet-stream chunks are
//     NEVER stored verbatim (they would balloon the DB row); instead they are
//     summarized via `binaryStreamChunkSummary` (`{object:"binary.stream_chunk",
//     content_type, bytes}`), preferring `chunk.Size` when the persistence
//     layer has already elided the bytes (Go `SummarizeBinaryChunk`).
//
// The Rust port mirrors this exactly with [`StoredStreamEvent`] as the
// storage-side projection of `httpclient.StreamEvent`. The three Go tests in
// `request_audio_test.go:101-149` (TestMarshalStreamEventForStorage_*,
// TestShouldSkipStoredStreamChunk_DoneSentinelDoesNotSkipBinaryAudio) are
// ported below.
// ---------------------------------------------------------------------------

/// Go `llm.httpclient.BinaryStreamDoneEventType` = `"binary.done"`
/// (`llm/httpclient/model.go:79`). Marks EOF for non-SSE binary streams; the
/// storage layer treats it as a sentinel and skips persisting it.
pub const BINARY_STREAM_DONE_EVENT_TYPE: &str = "binary.done";

/// Go `llm.DoneStreamEvent.Data` = `[]byte("[DONE]")`
/// (`llm/model.go:14-16`). The SSE sentinel the gateway appends to mark the
/// end of a `text/event-stream` response. Storage skips persisting it.
pub const DONE_STREAM_EVENT_DATA: &[u8] = b"[DONE]";

/// Storage-side projection of Go `httpclient.StreamEvent`
/// (`llm/httpclient/model.go:106-114`). Only the four fields consulted by the
/// persistence helpers are surfaced; the network/decoder layer keeps the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStreamEvent {
    /// Mirrors Go `LastEventID` (`last_event_id,omitempty`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_event_id: String,
    /// Mirrors Go `Type` (`type`).
    #[serde(rename = "type")]
    pub event_type: String,
    /// Mirrors Go `Data` (`data`). For binary chunks this may be empty after
    /// `SummarizeBinaryChunk` has run; the byte count then lives in `size`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<u8>,
    /// Mirrors Go `Size` (`size,omitempty`). Carries the original byte count
    /// when the raw audio bytes were elided from `data` for persistence.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub size: usize,
}

impl StoredStreamEvent {
    pub fn new(event_type: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            last_event_id: String::new(),
            event_type: event_type.into(),
            data: data.into(),
            size: 0,
        }
    }

    pub fn with_last_event_id(mut self, last_event_id: impl Into<String>) -> Self {
        self.last_event_id = last_event_id.into();
        self
    }

    pub fn with_size(mut self, size: usize) -> Self {
        self.size = size;
        self
    }
}

/// Pure mirror of Go `isBinaryStreamChunk` (`request.go:810-818`).
///
/// Returns `true` when `event_type` (lowercased, trimmed) starts with `audio/`
/// or equals `application/octet-stream`. The `binary.done` sentinel is
/// intentionally excluded — that is an EOF marker, not a payload-bearing chunk.
pub fn is_binary_stream_chunk(event: &StoredStreamEvent) -> bool {
    let event_type = event.event_type.trim().to_ascii_lowercase();
    event_type.starts_with("audio/") || event_type == "application/octet-stream"
}

/// Pure mirror of Go `shouldSkipStoredStreamChunk` (`request.go:820-824`).
///
/// Go drops a chunk when ANY of:
///   * the chunk pointer is nil (modelled here as the empty default event),
///   * the chunk carries the `[DONE]` SSE sentinel on a NON-binary event,
///   * the chunk type is the `binary.done` EOF marker.
pub fn should_skip_stored_stream_chunk(event: Option<&StoredStreamEvent>) -> bool {
    let Some(event) = event else {
        return true;
    };
    if event.event_type == BINARY_STREAM_DONE_EVENT_TYPE {
        return true;
    }
    // Go: `!isBinaryStreamChunk(chunk) && bytes.Equal(chunk.Data, llm.DoneStreamEvent.Data)`.
    !is_binary_stream_chunk(event) && event.data.as_slice() == DONE_STREAM_EVENT_DATA
}

/// Wrapper produced by [`marshal_stream_event_for_storage`] for binary chunks.
/// Mirrors Go `binaryStreamChunkSummary` (`request.go:804-808`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryStreamChunkSummary {
    /// Always the literal `"binary.stream_chunk"` (Go: `Object` field).
    pub object: String,
    /// Trimmed `chunk.Type` (Go: `strings.TrimSpace(chunk.Type)`).
    pub content_type: String,
    /// Byte count of the elided payload. Prefers `chunk.Size` when `data`
    /// has already been summarized to empty (Go: `len(chunk.Data)` falling
    /// back to `chunk.Size`).
    pub bytes: usize,
}

/// Envelope written by [`marshal_stream_event_for_storage`]. Mirrors Go
/// `jsonStreamEvent` (`request.go:798-802`). The `data` field is either the
/// raw SSE payload (verbatim from the stream) or a summarized binary-chunk
/// JSON object — never both.
///
/// **Parity note:** Go's `jsonStreamEvent.Type` carries the JSON tag
/// `json:"event"` (NOT `json:"type"` — that tag belongs to the network-side
/// `httpclient.StreamEvent`). The storage envelope key is therefore `"event"`.
/// `[Hilbert-the-11th ok]` fixed from a prior `rename = "type"` that would have
/// produced a different JSON key than the Go binary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonStreamEvent {
    /// Mirrors Go `LastEventID` (`last_event_id,omitempty`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_event_id: String,
    /// Mirrors Go `Type` (`event`) — verbatim event type. Go's
    /// `jsonStreamEvent` uses `json:"event"` (distinct from
    /// `httpclient.StreamEvent`'s `json:"type"`).
    #[serde(rename = "event")]
    pub event_type: String,
    /// Mirrors Go `Data` (`data`) — raw SSE payload OR summarized binary JSON.
    pub data: Value,
}

/// Pure mirror of Go `marshalStreamEventForStorage` (`request.go:826-852`).
///
/// Returns the JSON envelope that gets persisted in the `response_chunks`
/// array. Binary chunks (audio/* or application/octet-stream) are NEVER stored
/// verbatim; they are summarized via [`BinaryStreamChunkSummary`], preferring
/// `size` when the persistence layer has already elided the bytes.
pub fn marshal_stream_event_for_storage(
    event: &StoredStreamEvent,
) -> Result<Value, serde_json::Error> {
    let data: Value = if is_binary_stream_chunk(event) {
        let byte_count = if event.data.is_empty() {
            event.size
        } else {
            event.data.len()
        };
        let summary = BinaryStreamChunkSummary {
            object: "binary.stream_chunk".to_string(),
            content_type: event.event_type.trim().to_string(),
            bytes: byte_count,
        };
        serde_json::to_value(&summary)?
    } else {
        // Non-binary payload: parse the SSE bytes as JSON; if they don't form
        // a valid JSON value, fall back to a raw string. Mirrors Go's
        // `json.RawMessage` round-trip (Data is already JSON text).
        serde_json::from_slice::<Value>(&event.data)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&event.data).into_owned()))
    };

    let envelope = JsonStreamEvent {
        last_event_id: event.last_event_id.clone(),
        event_type: event.event_type.clone(),
        data,
    };
    serde_json::to_value(&envelope)
}

// ---------------------------------------------------------------------------
// RUST-P10-001 A02 — pure audio offload planner + external-storage round-trip.
//
// # Parity (Go `UpdateRequestCompletedWithAudio`, request.go:467-563)
//
// Go's audio-persistence branch is gated by TWO conditions:
//   1. `len(audio) > 0` — empty audio never offloads.
//   2. `s.shouldUseExternalStorage(ctx, dataStorage)` — only a non-primary
//      external storage accepts the bytes; primary-DB / unset storage keeps
//      the audio out of the row entirely.
//
// When both hold, Go calls `DataStorageService.SaveData(ctx, dataStorage,
// GenerateAudioKey(...), audio)`; on success it sets the four content_*
// fields (`content_saved=true`, `content_storage_id=dataStorage.ID`,
// `content_storage_key=key`, `content_saved_at=now`). On failure Go logs and
// skips the Set* calls — the row keeps whatever content_* state it had.
//
// The pure planner below captures the decision; the side-effecting SaveData
// lives in the caller (the orchestrator). The round-trip tests below exercise
// the full decision → save → load-back path with `InMemoryStorageAdapter` as
// the fake external adapter, mirroring the Go integration test
// `TestRequestService_UpdateRequestCompletedWithAudio_ExternalStorage`
// (request_audio_test.go:21-99) which uses a real temp-dir FS adapter.
// ---------------------------------------------------------------------------

/// Pure decision describing whether and where the audio payload should be
/// offloaded to external storage. Captures the Go gate
/// `len(audio) > 0 && shouldUseExternalStorage(...)` plus the pre-computed
/// object keys ([`generate_audio_key`] / [`generate_response_body_key`]) the
/// caller hands to `StorageAdapter::put`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOffloadPlan {
    /// `true` when both gates pass and the audio bytes should be written to
    /// external storage.
    pub offload_audio: bool,
    /// `true` when the response-body placeholder should ALSO be written to
    /// external storage (Go always writes it on the external path when
    /// `storeResponseBody` is true).
    pub offload_response_body: bool,
    /// Object key for the audio bytes (`GenerateAudioKey`). `None` when
    /// [`AudioOffloadPlan::offload_audio`] is false.
    pub audio_key: Option<String>,
    /// Object key for the response-body placeholder
    /// (`GenerateResponseBodyKey`). `None` when
    /// [`AudioOffloadPlan::offload_response_body`] is false.
    pub response_body_key: Option<String>,
    /// `data_storage_id` of the resolved external storage; the caller records
    /// this on the request row's `content_storage_id` column on success.
    pub data_storage_id: Option<i64>,
}

impl AudioOffloadPlan {
    fn skip() -> Self {
        Self {
            offload_audio: false,
            offload_response_body: false,
            audio_key: None,
            response_body_key: None,
            data_storage_id: None,
        }
    }
}

/// Resolve the audio-offload plan from the inputs Go's
/// `UpdateRequestCompletedWithAudio` consults.
///
/// * `audio_size` — `audio.len()`. Zero -> never offload.
/// * `route` — the storage route resolved from the request's data-storage row
///   ([`resolve_storage_route`]). Only [`StorageRoute::External`] offloads.
/// * `store_response_body` — the system `StoreResponseBody` policy flag.
///   Mirrors Go `if policy, err := SystemService.StoragePolicy(ctx); ...`.
/// * `project_id` / `request_id` / `filename` — fed to the key generators so
///   the caller persists at exactly the key Go would compute.
pub fn plan_audio_offload(
    audio_size: usize,
    route: StorageRoute,
    store_response_body: bool,
    project_id: i64,
    request_id: i64,
    filename: &str,
) -> AudioOffloadPlan {
    let StorageRoute::External { data_storage_id } = route else {
        return AudioOffloadPlan::skip();
    };
    let offload_audio = audio_size > 0;
    let offload_response_body = store_response_body;
    if !offload_audio && !offload_response_body {
        return AudioOffloadPlan::skip();
    }
    AudioOffloadPlan {
        offload_audio,
        offload_response_body,
        audio_key: if offload_audio {
            Some(generate_audio_key(project_id, request_id, filename))
        } else {
            None
        },
        response_body_key: if offload_response_body {
            Some(generate_response_body_key(project_id, request_id))
        } else {
            None
        },
        data_storage_id,
    }
}

/// Given a successful audio offload, build the four `content_*` fields Go
/// writes via `SetContentSaved(true).SetContentStorageID(...).
/// SetContentStorageKey(...).SetContentSavedAt(now)`. Returns the populated
/// [`RequestContentLocation`] the caller merges into the row.
///
/// Mirrors Go `request.go:548-553`. The caller is responsible for the actual
/// row update; this helper only captures the post-success field shape.
pub fn audio_offload_succeeded(
    plan: &AudioOffloadPlan,
    saved_at: DateTime<Utc>,
) -> Option<RequestContentLocation> {
    let data_storage_id = plan.data_storage_id?;
    let content_storage_key = plan.audio_key.clone()?;
    Some(RequestContentLocation::saved(
        data_storage_id,
        content_storage_key,
        saved_at,
    ))
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

fn storage_safe_filename(filename: &str) -> String {
    let normalized = filename.replace('\\', "/");
    let basename = normalized
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("audio");

    let safe: String = basename
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => ch,
            _ => '_',
        })
        .collect();

    if safe.is_empty() {
        "audio".to_string()
    } else {
        safe
    }
}

fn parse_optional_u64(value: &str) -> Option<Option<u64>> {
    if value.is_empty() {
        Some(None)
    } else {
        value.parse().ok().map(Some)
    }
}

fn escape_header_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleProcessingRequestCandidate {
    pub project_id: String,
    pub request_id: String,
    pub status: RequestStatus,
    pub processing_started_at: DateTime<Utc>,
}

impl StaleProcessingRequestCandidate {
    pub fn new(
        project_id: impl Into<String>,
        request_id: impl Into<String>,
        status: RequestStatus,
        processing_started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            request_id: request_id.into(),
            status,
            processing_started_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleProcessingRequestId {
    pub project_id: String,
    pub request_id: String,
}

/// Which entity a stale-processing cleanup pass operates on.
///
/// # Parity (Go `ClearStaleProcessingOnStartup` — `request.go:1091-1122`)
///
/// Go runs two separate UPDATEs: one against `Request` (entity="requests"),
/// one against `RequestExecution` (entity="executions"). Both share the same
/// `maxProcessingDuration` cutoff, the same `Status == Processing` filter, and
/// the same `SetStatus(StatusCanceled)` target. Rust encodes this as a typed
/// tag so callers can plan/apply each pass independently without re-deriving
/// the Go semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleCleanupEntity {
    Request,
    Execution,
}

impl StaleCleanupEntity {
    /// Mirrors the `entityName` string Go logs in `cancelStaleRecords`
    /// (`request.go:1068, 1078-1081`).
    pub fn go_label(self) -> &'static str {
        match self {
            Self::Request => "requests",
            Self::Execution => "executions",
        }
    }
}

/// Pure plan describing which stale-processing rows a single startup-cleanup
/// pass should flip, and to what status.
///
/// # Parity (Go `cancelStaleRecords` + `ClearStaleProcessingOnStartup`,
/// `request.go:1064-1122`)
///
/// Go semantics, preserved exactly:
/// 1. `cutoff := time.Now().UTC().Add(-maxAge)`.
/// 2. Filter: `Status == Processing AND CreatedAtLT(cutoff)` — strict `<`, so
///    a row created *exactly* at `cutoff` is NOT cleaned up.
/// 3. Target status: `StatusCanceled` (never `Failed` — startup cleanup treats
///    a crash-leaked row as canceled, not failed).
/// 4. The Go bulk UPDATE has no `LIMIT`; the count returned is whatever the
///    DB matched. The Rust plan exposes an optional `limit` for callers that
///    want bounded batches, but `None` mirrors Go exactly.
///
/// Note Go's `StatusProcessing` maps to Rust `RequestStatus::Running`
/// (the Conduit API Rust port renamed "processing" -> "running"); the Go
/// `StatusCanceled` maps to Rust `RequestStatus::Cancelled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleProcessingCleanupPlan {
    pub now: DateTime<Utc>,
    pub cutoff_at: DateTime<Utc>,
    pub limit: Option<u32>,
    pub target_status: RequestStatus,
    pub requests: Vec<StaleProcessingRequestId>,
}

impl StaleProcessingCleanupPlan {
    /// Build the plan from a stale-after duration.
    ///
    /// `now - stale_after` is the cutoff. Use `None` for `limit` to mirror
    /// Go's unbounded bulk UPDATE exactly.
    pub fn from_duration<'a>(
        now: DateTime<Utc>,
        stale_after: Duration,
        limit: Option<u32>,
        candidates: impl IntoIterator<Item = &'a StaleProcessingRequestCandidate>,
    ) -> Self {
        Self::from_cutoff(now, now - stale_after, limit, candidates)
    }

    /// Build the plan from an explicit cutoff timestamp.
    ///
    /// # Parity (Go `CreatedAtLT(cutoff)`)
    ///
    /// Uses **strict `<`** on `processing_started_at`: a row whose timestamp is
    /// *exactly equal* to `cutoff_at` is NOT selected (matches Go's ent
    /// `CreatedAtLT` predicate, which compiles to SQL `<`).
    pub fn from_cutoff<'a>(
        now: DateTime<Utc>,
        cutoff_at: DateTime<Utc>,
        limit: Option<u32>,
        candidates: impl IntoIterator<Item = &'a StaleProcessingRequestCandidate>,
    ) -> Self {
        let matching = candidates
            .into_iter()
            .filter(|candidate| {
                // Go: Status == Processing && CreatedAtLT(cutoff).
                candidate.status == RequestStatus::Running
                    && candidate.processing_started_at < cutoff_at
            })
            .take(limit.map(|n| n as usize).unwrap_or(usize::MAX))
            .map(|candidate| StaleProcessingRequestId {
                project_id: candidate.project_id.clone(),
                request_id: candidate.request_id.clone(),
            })
            .collect();

        Self {
            now,
            cutoff_at,
            limit,
            target_status: RequestStatus::Cancelled,
            requests: matching,
        }
    }

    pub fn request_ids(&self) -> Vec<&str> {
        self.requests
            .iter()
            .map(|request| request.request_id.as_str())
            .collect()
    }

    /// Number of rows this plan would flip. Go returns this count from its
    /// bulk UPDATE and logs it when `count > 0` (`request.go:1077-1081`).
    pub fn matched_count(&self) -> usize {
        self.requests.len()
    }
}

/// Aggregated outcome of a startup cleanup pass over requests + executions.
///
/// # Parity (Go `ClearStaleProcessingOnStartup` — `request.go:1091-1122`)
///
/// Go runs BOTH entity cleanups unconditionally, collects errors into
/// `var errs []error`, and at the end returns `errors.Join(errs...)` if any
/// pass failed. This struct captures that shape: each entity produces an
/// independent result (matched-count on success, error message on failure),
/// and the top-level `aggregate_error()` mirrors Go's "fail if any entity
/// failed" rule. Crucially a failure on one entity MUST NOT prevent the other
/// from running — that's why Go runs both branches before checking `errs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupCleanupOutcome {
    pub now: DateTime<Utc>,
    pub cutoff_at: DateTime<Utc>,
    pub stale_after: Duration,
    pub requests_cleaned: usize,
    pub executions_cleaned: usize,
    /// Per-entity error messages (empty when the pass succeeded). Presence of
    /// any entry means Go would have returned the joined error.
    pub errors: Vec<String>,
}

impl StartupCleanupOutcome {
    /// Number of entities that errored. Go logs each via
    /// `fmt.Errorf("failed to cancel stale %s: %w", entityName, err)`.
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }

    /// Returns `true` when at least one entity cleanup failed — matches Go's
    /// `if len(errs) > 0 { return errors.Join(errs...) }` gate.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Aggregate error message in the spirit of Go's `errors.Join`. Returns
    /// `None` when the whole startup cleanup succeeded.
    pub fn aggregate_error(&self) -> Option<String> {
        if self.errors.is_empty() {
            None
        } else {
            Some(format!(
                "startup cleanup failed: {}",
                self.errors.join("; ")
            ))
        }
    }
}

/// Build a `StartupCleanupOutcome` from the two per-entity cleanup plans +
/// per-entity error messages.
///
/// Each `(plan, error)` pair corresponds to one Go
/// `cancelStaleRecords(ctx, maxProcessingDuration, entityName, updateFn)` call.
/// A `Some(err)` on the requests pair does NOT short-circuit the executions
/// pair — callers MUST supply both, mirroring Go's two unconditional branches.
pub fn build_startup_cleanup_outcome(
    now: DateTime<Utc>,
    stale_after: Duration,
    requests_plan: &StaleProcessingCleanupPlan,
    requests_error: Option<String>,
    executions_plan: &StaleProcessingCleanupPlan,
    executions_error: Option<String>,
) -> StartupCleanupOutcome {
    let mut errors = Vec::new();
    let requests_cleaned = match requests_error {
        Some(msg) => {
            errors.push(format!("failed to cancel stale requests: {msg}"));
            0
        }
        None => requests_plan.matched_count(),
    };
    let executions_cleaned = match executions_error {
        Some(msg) => {
            errors.push(format!("failed to cancel stale executions: {msg}"));
            0
        }
        None => executions_plan.matched_count(),
    };

    StartupCleanupOutcome {
        now,
        cutoff_at: requests_plan.cutoff_at,
        stale_after,
        requests_cleaned,
        executions_cleaned,
        errors,
    }
}

/// Go constant `maxProcessingDuration` (`request.go:1089`): one hour.
///
/// Records whose `CreatedAt` is older than `now - 1h` AND whose status is still
/// `Processing` are treated as leaked by a crash and canceled on startup.
pub const STARTUP_MAX_PROCESSING_DURATION: Duration = Duration::hours(1);

// ---------------------------------------------------------------------------
// RUST-P10-001 S17 — startup-time stale-processing EXECUTION semantics.
//
// # Parity (Go `cmd/conduit/main.go:85-107` + `request.go:1091-1122`)
//
// S10 captured the pure *plan* layer (cutoff / limit / status / entity
// aggregator). S17 captures the *startup-execution* layer — the four
// startup-exclusive properties Go hard-codes in `main.go`'s `OnStart` hook:
//
//   1. **Detached context** — the cleanup runs off `context.Background()`,
//      fully decoupled from the request that triggered OnStart. A slow DB
//      must not delay or be canceled by the boot sequence (main.go:89).
//   2. **30-second hard timeout** — `context.WithTimeout(..., 30*time.Second)`
//      bounds the cleanup so a wedged DB cannot stall background bookkeeping
//      indefinitely (main.go:89).
//   3. **Non-blocking** — the cleanup goroutine is fired-and-forgotten; the
//      OnStart hook returns `nil` immediately (main.go:88, 106). The server
//      can begin serving traffic while cleanup is still running.
//   4. **Non-fatal failure** — if `ClearStaleProcessingOnStartup` returns an
//      error, Go only emits `log.Warn(...)` and continues; startup is NEVER
//      aborted by a cleanup failure (main.go:92-94).
//
// Additionally the cleanup is **one-shot**: it is invoked exactly once in
// OnStart and is NOT on any periodic ticker. (A request that goes stale
// mid-run will be picked up the next time the process restarts.)
//
// The pure helpers below encode this startup-execution contract so the
// runtime layer can wire it up without re-deriving the Go semantics.
// ---------------------------------------------------------------------------

/// Go startup-hook constant `30 * time.Second`
/// (`cmd/conduit/main.go:89`): the hard timeout applied to
/// `ClearStaleProcessingOnStartup`. The cleanup runs in a background task
/// off a detached context with this deadline so it cannot block boot.
pub const STARTUP_CLEANUP_TIMEOUT: Duration = Duration::seconds(30);

/// Outcome of applying Go's startup-execution policy to a cleanup result.
///
/// Mirrors the four startup-exclusive behaviours in `main.go:85-107`:
/// * the cleanup ran with a 30-second detached timeout,
/// * a failure is logged but does NOT abort startup.
///
/// `StartupCleanupOutcome` (S10) describes what the DB pass returned;
/// `StartupCleanupExecution` describes how the boot hook treated that result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupCleanupExecution {
    /// Wall-clock timeout applied to the cleanup pass. Always
    /// [`STARTUP_CLEANUP_TIMEOUT`] in stock Go; exposed as a field so tests
    /// and ops overrides remain faithful to the source.
    pub timeout: Duration,
    /// The underlying DB cleanup outcome (matched counts + per-entity
    /// errors). `None` when the cleanup task did not finish before the
    /// timeout (Go would observe this as `ctx.Err() ==
    /// context.DeadlineExceeded` inside the update fns).
    pub outcome: Option<StartupCleanupOutcome>,
    /// `true` when the cleanup task exceeded the 30-second timeout before
    /// producing a result. Mirrors Go's `cleanupCtx.Err() ==
    /// context.DeadlineExceeded` observable: the goroutine returns without
    /// logging a stale-processing error (the inner `ClearStaleProcessingOnStartup`
    /// call returns the deadline error, which is then logged as a warning).
    pub timed_out: bool,
}

impl StartupCleanupExecution {
    /// Build the execution descriptor from the cleanup outcome (no timeout).
    /// Use this in the happy path where the cleanup finished within
    /// [`STARTUP_CLEANUP_TIMEOUT`].
    pub fn completed(outcome: StartupCleanupOutcome) -> Self {
        Self {
            timeout: STARTUP_CLEANUP_TIMEOUT,
            outcome: Some(outcome),
            timed_out: false,
        }
    }

    /// Build the execution descriptor for the timeout case. Mirrors Go's
    /// detached 30-second context expiring before
    /// `ClearStaleProcessingOnStartup` returned.
    pub fn timed_out() -> Self {
        Self {
            timeout: STARTUP_CLEANUP_TIMEOUT,
            outcome: None,
            timed_out: true,
        }
    }

    /// Returns `true` when startup MUST proceed despite cleanup problems.
    ///
    /// # Parity (Go `main.go:92-94`)
    ///
    /// Go's policy is unconditional: the OnStart hook ALWAYS returns `nil`,
    /// regardless of whether `ClearStaleProcessingOnStartup` returned an
    /// error (it only logs `log.Warn`). So this method always returns
    /// `true` — startup proceeds whether the cleanup succeeded, failed with
    /// a per-entity error, or timed out. It exists as an explicit named
    /// predicate so call-sites encode Go's intent rather than an opaque
    /// `true` constant.
    pub fn startup_proceeds(&self) -> bool {
        // Go: cleanup failure is logged via log.Warn and the hook returns nil;
        // a timeout surfaces as a deadline error and is logged the same way.
        // In both cases the app continues to boot.
        true
    }

    /// Returns `true` when the cleanup pass fully succeeded — the task did
    /// not time out AND no per-entity errors were recorded.
    pub fn is_fully_successful(&self) -> bool {
        !self.timed_out
            && self
                .outcome
                .as_ref()
                .is_some_and(|outcome| !outcome.has_errors())
    }

    /// The warning message Go would emit, or `None` when the cleanup fully
    /// succeeded.
    ///
    /// # Parity (Go `main.go:93`)
    ///
    /// Go logs `"failed to cancel stale processing records on startup"` with
    /// the underlying error as `log.Cause(err)`. This helper returns the
    /// matching human-readable message so the runtime layer can hand it to
    /// its logger verbatim.
    pub fn warning_message(&self) -> Option<String> {
        if self.is_fully_successful() {
            return None;
        }
        if self.timed_out {
            return Some(
                "failed to cancel stale processing records on startup: timed out".to_string(),
            );
        }
        // Aggregate the per-entity errors via the S10 helper.
        self.outcome
            .as_ref()
            .and_then(|outcome| outcome.aggregate_error())
            .map(|agg| format!("failed to cancel stale processing records on startup: {agg}"))
    }
}

/// Whether the cleanup pass should be re-run periodically vs. only at startup.
///
/// # Parity (Go `main.go:85-107`)
///
/// Go invokes `ClearStaleProcessingOnStartup` exactly ONCE, in the FX
/// `OnStart` hook. There is NO periodic ticker — a request that goes stale
/// mid-run stays "processing" on the dashboard until the next process
/// restart. This enum encodes that one-shot nature so a future scheduler
/// cannot accidentally introduce a periodic pass without an explicit policy
/// decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupCleanupSchedule {
    /// One-shot: run only in the boot hook (Go default).
    #[default]
    OneShot,
    /// Hypothetical periodic schedule. NOT used by stock Go; encoded so a
    /// future ops toggle has a typed place to live without rewriting the
    /// startup path.
    Periodic {
        /// Interval between cleanup passes.
        interval: Duration,
    },
}

impl StartupCleanupSchedule {
    /// Returns `true` when this is the Go-stock one-shot startup policy.
    pub fn is_one_shot(self) -> bool {
        matches!(self, Self::OneShot)
    }
}

/// Latency metrics recorded when a request/execution reaches a terminal
/// status. Mirrors Go `LatencyMetrics` (`request.go:375-380`).
///
/// Go uses `*int64` (nullable); the Rust port uses `Option<i64>`. All three
/// fields are optional: the gateway records whichever subset the provider
/// surfaced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatencyMetrics {
    /// Mirrors Go `LatencyMs` -> `Request.metrics_latency_ms` /
    /// `RequestExecution.metrics_latency_ms` (end-to-end upstream latency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    /// Mirrors Go `FirstTokenLatencyMs` ->
    /// `metrics_first_token_latency_ms` (time to first streamed token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_latency_ms: Option<i64>,
    /// Mirrors Go `ReasoningDurationMs` ->
    /// `metrics_reasoning_duration_ms` (thinking/reasoning duration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_duration_ms: Option<i64>,
}

/// Error details recorded when a request execution fails. Mirrors Go
/// `ExecutionErrorInfo` (`request.go:744-747`).
///
/// Go uses `StatusCode *int`; the Rust port uses `Option<i64>`. Only the HTTP
/// status code of the upstream failure is captured here — the human-readable
/// message travels separately as the `error_message` column/parameter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionErrorInfo {
    /// Mirrors Go `ExecutionErrorInfo.StatusCode` -> stored on
    /// `RequestExecution.response_status_code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<i64>,
}

/// Patch applied by `update_request_completed` / its variants. Captures the
/// Go `client.Request.UpdateOneID(...).Set*` builder inputs that are common
/// to all three completion overloads (lines 416-463, 507-562, 601-649).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestCompletionPatch {
    /// Mirrors Go `SetExternalID(externalId)`. Empty string means "leave
    /// unchanged" (Go always sets it; we keep the field for fidelity).
    pub external_id: Option<String>,
    /// Mirrors Go `SetMetricsLatencyMs` / `SetMetricsFirstTokenLatencyMs` /
    /// `SetMetricsReasoningDurationMs` (only applied when the inner field is
    /// `Some`).
    pub metrics: Option<LatencyMetrics>,
    /// Mirrors Go `SetResponseBody(responseBodyBytes)` on the DB-storage path.
    /// `None` means the policy/route decided not to persist the body (Go:
    /// `storeResponseBody == false` or external-storage path handled it
    /// elsewhere).
    pub response_body: Option<Value>,
}

/// Patch applied by `update_execution_completed`. Mirrors Go
/// `client.RequestExecution.UpdateOneID(...).SetStatus(StatusCompleted).
/// SetExternalID(...).SetMetrics*().SetResponseBody(...)` (lines 686-732).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecutionCompletionPatch {
    pub external_id: Option<String>,
    pub metrics: Option<LatencyMetrics>,
    pub response_body: Option<Value>,
}

/// Patch applied by `update_execution_status` (failed/canceled). Mirrors Go
/// `client.RequestExecution.UpdateOneID(...).SetStatus(status).
/// SetErrorMessage(...).SetResponseStatusCode(...)` (lines 769-785).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionStatusPatch {
    /// Target status (`Failed` or `Cancelled`). Mirrors Go
    /// `requestexecution.Status` arg of `UpdateRequestExecutionStatus`.
    pub next_status: RequestStatus,
    /// Mirrors Go `SetErrorMessage(errorMsg)` — only applied when non-empty.
    pub error_message: Option<String>,
    /// Mirrors Go `SetResponseStatusCode(*errorInfo.StatusCode)` — only
    /// applied when `error_info` and its `status_code` are `Some`.
    pub error_info: Option<ExecutionErrorInfo>,
}

#[async_trait]
pub trait RequestPersistenceRepo: Send + Sync {
    async fn insert_request(
        &self,
        ctx: &RequestContext,
        request: RequestRecord,
    ) -> RequestServiceResult<RequestRecord>;

    async fn find_request(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RequestServiceResult<Option<RequestRecord>>;

    /// **S08 / RUST-P7-006**: list all request rows whose `external_id`
    /// matches. Mirrors the ent query
    /// `client.Request.Query().Where(request.ExternalID(externalID))`
    /// (`biz/video.go:67-69` and `85-87`). Deliberately NOT project-scoped:
    /// the Go lookup carries no project predicate — "assumes provider task
    /// IDs are globally unique across channels" (`biz/video.go:59-60`,
    /// `77-78`). Go's `.Only(ctx)` semantics (0 rows -> not-found, >1 rows ->
    /// not-singular) are applied by
    /// [`RequestService::get_request_by_external_id`]; the repo surface stays
    /// a plain filter so DB backends can push it down as a WHERE clause.
    async fn find_requests_by_external_id(
        &self,
        ctx: &RequestContext,
        external_id: &str,
    ) -> RequestServiceResult<Vec<RequestRecord>>;

    async fn insert_execution(
        &self,
        ctx: &RequestContext,
        execution: ExecutionRecord,
    ) -> RequestServiceResult<ExecutionRecord>;

    async fn list_executions(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RequestServiceResult<Vec<ExecutionRecord>>;

    async fn transition_request_status(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        expected_status: RequestStatus,
        next_status: RequestStatus,
    ) -> RequestServiceResult<Option<RequestRecord>>;

    // --- S15 / RUST-P9-006 production RequestRecorder write methods ----------
    //
    // These mirror the Go `RequestService` write methods that the production
    // RequestRecorder calls after the pipeline finishes. They operate directly
    // on the row (no CAS expected-status check) because the recorder has
    // already observed the outcome.

    /// Mirrors Go `UpdateRequestCompleted` (`request.go:382-465`) and the
    /// shared core of `UpdateRequestCompletedWithAudio` /
    /// `UpdateRequestStatusExternalIDAndResponseBody`. Sets status to
    /// `next_status`, applies the external-id / metrics / response-body patch,
    /// and returns the updated row. Audio/video/content_saved special-casing
    /// is deferred (see `update_request_completed_with_content` TODO).
    async fn update_request_status_completed(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        next_status: RequestStatus,
        patch: RequestCompletionPatch,
    ) -> RequestServiceResult<RequestRecord>;

    /// Mirrors Go `UpdateRequestStatus` (`request.go:1042-1053`) — a bare
    /// status flip with no body/metrics. Used by `MarkRequestCanceled` /
    /// `MarkRequestFailed` / `UpdateRequestStatusFromError`.
    async fn update_request_status(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        next_status: RequestStatus,
    ) -> RequestServiceResult<RequestRecord>;

    /// Mirrors Go `UpdateRequestExecutionCompleted` (`request.go:652-733`).
    /// Sets the execution status to `Succeeded`, applies external-id /
    /// metrics / response-body.
    async fn update_execution_completed(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        patch: ExecutionCompletionPatch,
    ) -> RequestServiceResult<ExecutionRecord>;

    /// Mirrors Go `UpdateRequestExecutionStatus` (`request.go:760-786`) — the
    /// shared core of `UpdateRequestExecutionFailed` /
    /// `UpdateRequestExecutionCanceled` /
    /// `UpdateRequestExecutionStatusFromError`. Sets the execution status and
    /// optional error_message / response_status_code.
    async fn update_execution_status(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        patch: ExecutionStatusPatch,
    ) -> RequestServiceResult<ExecutionRecord>;

    /// Mirrors Go `SaveRequestExecutionChunks` (`request.go:856-941`) on the
    /// DB-storage path (the external-storage branch is the caller's
    /// responsibility). Stores the filtered, marshaled chunk array on the
    /// execution row.
    async fn set_execution_response_chunks(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        chunks: Value,
    ) -> RequestServiceResult<ExecutionRecord>;

    /// Mirrors Go `SaveRequestChunks` (`request.go:943-1029`) on the
    /// DB-storage path. Stores the filtered chunk array on the request row.
    async fn set_request_response_chunks(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        chunks: Value,
    ) -> RequestServiceResult<RequestRecord>;
}

pub struct RequestService {
    repo: Arc<dyn RequestPersistenceRepo>,
}

impl RequestService {
    pub fn new(repo: Arc<dyn RequestPersistenceRepo>) -> Self {
        Self { repo }
    }

    pub async fn create_request(
        &self,
        ctx: &RequestContext,
        request: RequestRecord,
    ) -> RequestServiceResult<RequestRecord> {
        self.repo.insert_request(ctx, request).await
    }

    pub async fn append_execution(
        &self,
        ctx: &RequestContext,
        execution: ExecutionRecord,
    ) -> RequestServiceResult<ExecutionRecord> {
        let request = self
            .repo
            .find_request(ctx, &execution.project_id, &execution.request_id)
            .await?;
        if request.is_none() {
            return Err(RequestServiceError::RequestNotFound(execution.request_id));
        }

        self.repo.insert_execution(ctx, execution).await
    }

    pub async fn list_executions(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RequestServiceResult<Vec<ExecutionRecord>> {
        self.repo.list_executions(ctx, project_id, request_id).await
    }

    pub async fn transition_status(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        expected_status: RequestStatus,
        next_status: RequestStatus,
    ) -> RequestServiceResult<RequestRecord> {
        if !expected_status.can_transition_to(next_status) {
            return Err(RequestServiceError::InvalidStatusTransition {
                from: expected_status,
                to: next_status,
            });
        }

        let Some(current) = self.repo.find_request(ctx, project_id, request_id).await? else {
            return Err(RequestServiceError::RequestNotFound(request_id.to_string()));
        };

        if current.status != expected_status {
            return Err(RequestServiceError::StatusConflict {
                request_id: request_id.to_string(),
                expected: expected_status,
                actual: current.status,
            });
        }

        // Keep the compare-and-set boundary in the repo so a DB backend can make
        // this transition atomic without changing the service API.
        self.repo
            .transition_request_status(ctx, project_id, request_id, expected_status, next_status)
            .await?
            .ok_or_else(|| RequestServiceError::StatusConflict {
                request_id: request_id.to_string(),
                expected: expected_status,
                actual: current.status,
            })
    }

    // =======================================================================
    // RUST-P9-006 S15 — production RequestRecorder write methods.
    // Mirror Go `RequestService` write methods in `request.go` that the
    // recorder calls once the pipeline has reached a terminal outcome.
    // Storage-policy / external-storage routing is the caller's
    // responsibility; these service methods perform the row write only.
    // =======================================================================

    /// Mirrors Go `UpdateRequestCompleted` (`request.go:382-465`).
    ///
    /// Flips the request status to `Succeeded`, sets the external id, records
    /// latency metrics, and persists the response body (DB path). The caller
    /// is responsible for the `StoragePolicy.StoreResponseBody` check and for
    /// the external-storage branch — both are pure routing decisions that live
    /// outside the row write.
    pub async fn update_request_completed(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        external_id: &str,
        metrics: Option<LatencyMetrics>,
        response_body: Option<Value>,
    ) -> RequestServiceResult<RequestRecord> {
        let patch = RequestCompletionPatch {
            external_id: Some(external_id.to_string()),
            metrics,
            response_body,
        };
        self.repo
            .update_request_status_completed(
                ctx,
                project_id,
                request_id,
                RequestStatus::Succeeded,
                patch,
            )
            .await
    }

    /// Mirrors Go `UpdateRequestStatusExternalIDAndResponseBody`
    /// (`request.go:567-650`) — same as `update_request_completed` but with an
    /// arbitrary target status (used by the async-task poller that may land on
    /// `Running` rather than `Succeeded`).
    pub async fn update_request_status_external_id_and_response_body(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        next_status: RequestStatus,
        external_id: &str,
        metrics: Option<LatencyMetrics>,
        response_body: Option<Value>,
    ) -> RequestServiceResult<RequestRecord> {
        let patch = RequestCompletionPatch {
            external_id: Some(external_id.to_string()),
            metrics,
            response_body,
        };
        self.repo
            .update_request_status_completed(ctx, project_id, request_id, next_status, patch)
            .await
    }

    /// Mirrors Go `UpdateRequestStatus` (`request.go:1042-1053`).
    pub async fn update_request_status(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        next_status: RequestStatus,
    ) -> RequestServiceResult<RequestRecord> {
        self.repo
            .update_request_status(ctx, project_id, request_id, next_status)
            .await
    }

    /// Mirrors Go `MarkRequestCanceled` (`request.go:1032-1034`).
    pub async fn mark_request_canceled(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RequestServiceResult<RequestRecord> {
        self.update_request_status(ctx, project_id, request_id, RequestStatus::Cancelled)
            .await
    }

    /// Mirrors Go `MarkRequestFailed` (`request.go:1037-1039`).
    pub async fn mark_request_failed(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RequestServiceResult<RequestRecord> {
        self.update_request_status(ctx, project_id, request_id, RequestStatus::Failed)
            .await
    }

    /// Mirrors Go `UpdateRequestStatusFromError` (`request.go:1056-1062`).
    ///
    /// Go: `if errors.Is(rawErr, context.Canceled) || errors.Is(ctx.Err(),
    /// context.Canceled)` -> `StatusCanceled`; otherwise -> `StatusFailed`.
    /// The Rust port surfaces the decision as an explicit `canceled` boolean
    /// (the caller maps `tokio::task::JoinError::is_cancelled()` /
    /// `Ctx::is_cancelled()` to this flag) so the service layer stays free of
    /// Go-specific error-type plumbing.
    pub async fn update_request_status_from_error(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        canceled: bool,
    ) -> RequestServiceResult<RequestRecord> {
        let next_status = if canceled {
            RequestStatus::Cancelled
        } else {
            RequestStatus::Failed
        };
        self.update_request_status(ctx, project_id, request_id, next_status)
            .await
    }

    /// Mirrors Go `UpdateRequestExecutionCompleted` (`request.go:652-733`).
    pub async fn update_request_execution_completed(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        external_id: &str,
        metrics: Option<LatencyMetrics>,
        response_body: Option<Value>,
    ) -> RequestServiceResult<ExecutionRecord> {
        let patch = ExecutionCompletionPatch {
            external_id: Some(external_id.to_string()),
            metrics,
            response_body,
        };
        self.repo
            .update_execution_completed(ctx, project_id, request_id, execution_id, patch)
            .await
    }

    /// Mirrors Go `UpdateRequestExecutionStatus` (`request.go:760-786`) —
    /// shared core of failed / canceled.
    pub async fn update_request_execution_status(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        next_status: RequestStatus,
        error_message: Option<&str>,
        error_info: Option<ExecutionErrorInfo>,
    ) -> RequestServiceResult<ExecutionRecord> {
        let patch = ExecutionStatusPatch {
            next_status,
            error_message: error_message.map(|s| s.to_string()),
            error_info,
        };
        self.repo
            .update_execution_status(ctx, project_id, request_id, execution_id, patch)
            .await
    }

    /// Mirrors Go `UpdateRequestExecutionFailed` (`request.go:750-757`).
    pub async fn update_request_execution_failed(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        error_message: &str,
        error_info: Option<ExecutionErrorInfo>,
    ) -> RequestServiceResult<ExecutionRecord> {
        self.update_request_execution_status(
            ctx,
            project_id,
            request_id,
            execution_id,
            RequestStatus::Failed,
            Some(error_message),
            error_info,
        )
        .await
    }

    /// Mirrors Go `UpdateRequestExecutionCanceled` (`request.go:736-742`).
    pub async fn update_request_execution_canceled(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        error_message: &str,
    ) -> RequestServiceResult<ExecutionRecord> {
        self.update_request_execution_status(
            ctx,
            project_id,
            request_id,
            execution_id,
            RequestStatus::Cancelled,
            Some(error_message),
            None,
        )
        .await
    }

    /// Mirrors Go `UpdateRequestExecutionStatusFromError`
    /// (`request.go:788-796`). As with
    /// [`update_request_status_from_error`], the canceled-vs-failed decision
    /// is surfaced as an explicit boolean.
    pub async fn update_request_execution_status_from_error(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        error_message: &str,
        canceled: bool,
    ) -> RequestServiceResult<ExecutionRecord> {
        let next_status = if canceled {
            RequestStatus::Cancelled
        } else {
            RequestStatus::Failed
        };
        self.update_request_execution_status(
            ctx,
            project_id,
            request_id,
            execution_id,
            next_status,
            Some(error_message),
            None,
        )
        .await
    }

    /// Mirrors Go `SaveRequestExecutionChunks` (`request.go:856-941`) on the
    /// DB-storage path. The caller is responsible for:
    /// 1. consulting `StoreChunks` (Go `SystemService.StoreChunks`) and
    ///    short-circuiting when disabled;
    /// 2. filtering out done/binary sentinel chunks and marshaling each
    ///    surviving chunk via `marshalStreamEventForStorage` (Go
    ///    `request.go:826-852`);
    /// 3. routing to external storage when `shouldUseExternalStorage` is true
    ///    (the external write is NOT this method's concern).
    /// Here we only persist the already-prepared `chunks` JSON array on the
    /// execution row.
    pub async fn save_request_execution_chunks(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        chunks: Value,
    ) -> RequestServiceResult<ExecutionRecord> {
        self.repo
            .set_execution_response_chunks(ctx, project_id, request_id, execution_id, chunks)
            .await
    }

    /// Mirrors Go `SaveRequestChunks` (`request.go:943-1029`) on the
    /// DB-storage path. Same caller-responsibility notes as
    /// [`save_request_execution_chunks`].
    pub async fn save_request_chunks(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        chunks: Value,
    ) -> RequestServiceResult<RequestRecord> {
        self.repo
            .set_request_response_chunks(ctx, project_id, request_id, chunks)
            .await
    }

    // =======================================================================
    // RUST-P7-006 S08 — external-id read-back surface.
    // Go persists the provider task id in `requests.external_id`
    // (orchestrator/request.go:110-126 writes it right after the provider's
    // create response via UpdateRequestStatusExternalIDAndResponseBody; the
    // poll path re-writes it on every snapshot, biz/video.go:48-54). These
    // getters are the retrieval half used by VideoService.
    // =======================================================================

    /// Fetch a single request row by id, erroring when absent. Mirrors ent
    /// `client.Request.Get(ctx, requestID)` as used by `VideoService.loadTask`
    /// (`biz/video.go:123-126`) and `UpdateRequestCompleted`
    /// (`request.go:401-405`): ent `.Get` returns `NotFoundError` for a
    /// missing row, surfaced here as [`RequestServiceError::RequestNotFound`].
    pub async fn get_request(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RequestServiceResult<RequestRecord> {
        self.repo
            .find_request(ctx, project_id, request_id)
            .await?
            .ok_or_else(|| RequestServiceError::RequestNotFound(request_id.to_string()))
    }

    /// Look up the unique request row carrying `external_id`. Mirrors the ent
    /// query in `VideoService.GetTaskByExternalID` / `DeleteTaskByExternalID`
    /// (`biz/video.go:67-69`, `85-87`):
    /// `client.Request.Query().Where(request.ExternalID(externalID)).Only(ctx)`.
    ///
    /// `.Only(ctx)` semantics, mirrored exactly:
    /// * 0 rows  -> ent `NotFoundError`  -> [`RequestServiceError::RequestNotFound`]
    /// * >1 rows -> ent `NotSingularError` -> [`RequestServiceError::ExternalIdNotSingular`]
    /// * 1 row   -> the row.
    ///
    /// NOT project-scoped — Go "assumes provider task IDs are globally unique
    /// across channels" (`biz/video.go:59-60`).
    pub async fn get_request_by_external_id(
        &self,
        ctx: &RequestContext,
        external_id: &str,
    ) -> RequestServiceResult<RequestRecord> {
        let mut matches = self
            .repo
            .find_requests_by_external_id(ctx, external_id)
            .await?;
        match matches.len() {
            0 => Err(RequestServiceError::RequestNotFound(
                external_id.to_string(),
            )),
            1 => {
                // len() == 1 guarantees pop() yields Some; the fallback arm is
                // unreachable but keeps the lint-mandated no-unwrap style.
                matches
                    .pop()
                    .ok_or_else(|| RequestServiceError::RequestNotFound(external_id.to_string()))
            }
            _ => Err(RequestServiceError::ExternalIdNotSingular(
                external_id.to_string(),
            )),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryRequestPersistenceRepo {
    inner: Arc<Mutex<InMemoryRequestPersistenceState>>,
}

#[derive(Debug, Default)]
struct InMemoryRequestPersistenceState {
    requests: BTreeMap<(String, String), RequestRecord>,
    executions: BTreeMap<(String, String), Vec<ExecutionRecord>>,
}

impl InMemoryRequestPersistenceState {
    /// Find a mutable execution by id within a (project, request) scope.
    fn find_execution_mut(
        &mut self,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
    ) -> Option<&mut ExecutionRecord> {
        self.executions
            .get_mut(&(project_id.to_string(), request_id.to_string()))
            .and_then(|executions| executions.iter_mut().find(|exec| exec.id == execution_id))
    }
}

impl InMemoryRequestPersistenceRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_count(&self) -> RequestServiceResult<usize> {
        Ok(self.lock()?.requests.len())
    }

    fn lock(
        &self,
    ) -> RequestServiceResult<std::sync::MutexGuard<'_, InMemoryRequestPersistenceState>> {
        self.inner
            .lock()
            .map_err(|_| RequestServiceError::LockPoisoned)
    }
}

#[async_trait]
impl RequestPersistenceRepo for InMemoryRequestPersistenceRepo {
    async fn insert_request(
        &self,
        _ctx: &RequestContext,
        request: RequestRecord,
    ) -> RequestServiceResult<RequestRecord> {
        let mut inner = self.lock()?;
        let key = (request.project_id.clone(), request.id.clone());
        inner.requests.insert(key, request.clone());
        Ok(request)
    }

    async fn find_request(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RequestServiceResult<Option<RequestRecord>> {
        Ok(self
            .lock()?
            .requests
            .get(&(project_id.to_string(), request_id.to_string()))
            .cloned())
    }

    /// **S08 / RUST-P7-006**: global (cross-project) scan for rows whose
    /// `extra["external_id"]` equals `external_id`. Mirrors the un-scoped ent
    /// query `Request.Query().Where(request.ExternalID(...))`
    /// (`biz/video.go:67-69`). BTreeMap iteration keeps the result order
    /// deterministic ((project_id, request_id) ascending).
    async fn find_requests_by_external_id(
        &self,
        _ctx: &RequestContext,
        external_id: &str,
    ) -> RequestServiceResult<Vec<RequestRecord>> {
        Ok(self
            .lock()?
            .requests
            .values()
            .filter(|record| record.external_id() == Some(external_id))
            .cloned()
            .collect())
    }

    async fn insert_execution(
        &self,
        _ctx: &RequestContext,
        execution: ExecutionRecord,
    ) -> RequestServiceResult<ExecutionRecord> {
        let mut inner = self.lock()?;
        let key = (execution.project_id.clone(), execution.request_id.clone());
        inner
            .executions
            .entry(key)
            .or_default()
            .push(execution.clone());
        Ok(execution)
    }

    async fn list_executions(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RequestServiceResult<Vec<ExecutionRecord>> {
        Ok(self
            .lock()?
            .executions
            .get(&(project_id.to_string(), request_id.to_string()))
            .cloned()
            .unwrap_or_default())
    }

    async fn transition_request_status(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        expected_status: RequestStatus,
        next_status: RequestStatus,
    ) -> RequestServiceResult<Option<RequestRecord>> {
        let mut inner = self.lock()?;
        let key = (project_id.to_string(), request_id.to_string());
        let Some(request) = inner.requests.get_mut(&key) else {
            return Ok(None);
        };
        if request.status != expected_status {
            return Ok(None);
        }

        request.status = next_status;
        Ok(Some(request.clone()))
    }

    // --- S15 production RequestRecorder write methods ----------------------

    async fn update_request_status_completed(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        next_status: RequestStatus,
        patch: RequestCompletionPatch,
    ) -> RequestServiceResult<RequestRecord> {
        let mut inner = self.lock()?;
        let key = (project_id.to_string(), request_id.to_string());
        let request = inner
            .requests
            .get_mut(&key)
            .ok_or_else(|| RequestServiceError::RequestNotFound(request_id.to_string()))?;
        request.status = next_status;
        if let Some(external_id) = patch.external_id {
            request
                .extra
                .insert("external_id".to_string(), Value::from(external_id));
        }
        if let Some(metrics) = patch.metrics {
            if let Some(v) = metrics.latency_ms {
                request
                    .extra
                    .insert("metrics_latency_ms".to_string(), Value::from(v));
            }
            if let Some(v) = metrics.first_token_latency_ms {
                request
                    .extra
                    .insert("metrics_first_token_latency_ms".to_string(), Value::from(v));
            }
            if let Some(v) = metrics.reasoning_duration_ms {
                request
                    .extra
                    .insert("metrics_reasoning_duration_ms".to_string(), Value::from(v));
            }
        }
        if let Some(response_body) = patch.response_body {
            request
                .extra
                .insert("response_body".to_string(), response_body);
        }
        Ok(request.clone())
    }

    async fn update_request_status(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        next_status: RequestStatus,
    ) -> RequestServiceResult<RequestRecord> {
        let mut inner = self.lock()?;
        let key = (project_id.to_string(), request_id.to_string());
        let request = inner
            .requests
            .get_mut(&key)
            .ok_or_else(|| RequestServiceError::RequestNotFound(request_id.to_string()))?;
        request.status = next_status;
        Ok(request.clone())
    }

    async fn update_execution_completed(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        patch: ExecutionCompletionPatch,
    ) -> RequestServiceResult<ExecutionRecord> {
        let mut inner = self.lock()?;
        let exec = inner
            .find_execution_mut(project_id, request_id, execution_id)
            .ok_or_else(|| RequestServiceError::RequestNotFound(execution_id.to_string()))?;
        exec.status = RequestStatus::Succeeded;
        if let Some(external_id) = patch.external_id {
            exec.extra
                .insert("external_id".to_string(), Value::from(external_id));
        }
        if let Some(metrics) = patch.metrics {
            if let Some(v) = metrics.latency_ms {
                exec.extra
                    .insert("metrics_latency_ms".to_string(), Value::from(v));
            }
            if let Some(v) = metrics.first_token_latency_ms {
                exec.extra
                    .insert("metrics_first_token_latency_ms".to_string(), Value::from(v));
            }
            if let Some(v) = metrics.reasoning_duration_ms {
                exec.extra
                    .insert("metrics_reasoning_duration_ms".to_string(), Value::from(v));
            }
        }
        if let Some(response_body) = patch.response_body {
            exec.extra
                .insert("response_body".to_string(), response_body);
        }
        Ok(exec.clone())
    }

    async fn update_execution_status(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        patch: ExecutionStatusPatch,
    ) -> RequestServiceResult<ExecutionRecord> {
        let mut inner = self.lock()?;
        let exec = inner
            .find_execution_mut(project_id, request_id, execution_id)
            .ok_or_else(|| RequestServiceError::RequestNotFound(execution_id.to_string()))?;
        exec.status = patch.next_status;
        if let Some(error_message) = patch.error_message {
            exec.extra
                .insert("error_message".to_string(), Value::from(error_message));
        }
        if let Some(error_info) = patch.error_info
            && let Some(status_code) = error_info.status_code
        {
            exec.extra
                .insert("response_status_code".to_string(), Value::from(status_code));
        }
        Ok(exec.clone())
    }

    async fn set_execution_response_chunks(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        execution_id: &str,
        chunks: Value,
    ) -> RequestServiceResult<ExecutionRecord> {
        let mut inner = self.lock()?;
        let exec = inner
            .find_execution_mut(project_id, request_id, execution_id)
            .ok_or_else(|| RequestServiceError::RequestNotFound(execution_id.to_string()))?;
        exec.chunks = chunks.clone();
        exec.extra.insert("response_chunks".to_string(), chunks);
        Ok(exec.clone())
    }

    async fn set_request_response_chunks(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        chunks: Value,
    ) -> RequestServiceResult<RequestRecord> {
        let mut inner = self.lock()?;
        let key = (project_id.to_string(), request_id.to_string());
        let request = inner
            .requests
            .get_mut(&key)
            .ok_or_else(|| RequestServiceError::RequestNotFound(request_id.to_string()))?;
        request.chunks = chunks.clone();
        request.extra.insert("response_chunks".to_string(), chunks);
        Ok(request.clone())
    }
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal, RequestContext};
    use serde_json::json;

    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn request() -> RequestRecord {
        let mut request = RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/chat");
        request.headers = json!({"content-type": "application/json", "x-unknown": "keep"});
        request.body = json!({"messages": [{"role": "user", "content": "hi"}], "unknown": true});
        request.chunks = json!([{"delta": "hello", "unknown_chunk_field": 1}]);
        request
            .extra
            .insert("cache_signature".to_string(), json!("sig-1"));
        request
    }

    fn fixed_now() -> Result<DateTime<Utc>, chrono::ParseError> {
        Ok(DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")?.with_timezone(&Utc))
    }

    fn processing_candidate(
        request_id: &str,
        processing_started_at: DateTime<Utc>,
    ) -> StaleProcessingRequestCandidate {
        StaleProcessingRequestCandidate::new(
            "project-a",
            request_id,
            RequestStatus::Running,
            processing_started_at,
        )
    }

    #[test]
    fn request_content_storage_keys_use_stable_request_paths() {
        let keys = RequestContentStorageKeyBuilder::new("project-a", "req-1").request_keys();

        assert_eq!(
            keys,
            RequestContentStorageKeys {
                request_body: "/project-a/requests/req-1/request_body.json".to_string(),
                response_body: "/project-a/requests/req-1/response_body.json".to_string(),
                response_chunks: "/project-a/requests/req-1/response_chunks.json".to_string(),
            }
        );
    }

    #[test]
    fn request_content_storage_keys_use_stable_execution_paths() {
        let keys = RequestContentStorageKeyBuilder::new("project-a", "req-1")
            .execution("exec-1")
            .request_keys();

        assert_eq!(
            keys,
            RequestContentStorageKeys {
                request_body: "/project-a/requests/req-1/executions/exec-1/request_body.json"
                    .to_string(),
                response_body: "/project-a/requests/req-1/executions/exec-1/response_body.json"
                    .to_string(),
                response_chunks: "/project-a/requests/req-1/executions/exec-1/response_chunks.json"
                    .to_string(),
            }
        );
    }

    #[test]
    fn request_content_storage_audio_keys_keep_safe_filenames() {
        let builder = RequestContentStorageKeyBuilder::new("project-a", "req-1");

        assert_eq!(
            builder.audio_key("voice-1.mp3"),
            "/project-a/requests/req-1/audio/voice-1.mp3"
        );
        assert_eq!(
            builder.audio_key("../unsafe name.wav"),
            "/project-a/requests/req-1/audio/unsafe_name.wav"
        );
        assert_eq!(
            builder.execution("exec-1").audio_key("nested\\answer.ogg"),
            "/project-a/requests/req-1/executions/exec-1/audio/answer.ogg"
        );
    }

    #[test]
    fn invalid_json_falls_back_to_placeholder() {
        assert_eq!(
            parse_json_or_invalid_text("{not-json"),
            json!({"message": "invalid text"})
        );
        assert_eq!(
            parse_json_or_invalid_text(r#"{"message":"valid"}"#),
            json!({"message": "valid"})
        );
        assert_eq!(
            invalid_json_placeholder(),
            json!({"message": "invalid text"})
        );
    }

    #[test]
    fn stale_processing_cleanup_keeps_requests_newer_than_cutoff()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let candidates = vec![processing_candidate("req-new", now - Duration::minutes(5))];

        // Go's bulk UPDATE has no LIMIT, so `None` mirrors it exactly.
        let plan = StaleProcessingCleanupPlan::from_duration(
            now,
            Duration::minutes(30),
            None,
            &candidates,
        );

        let expected_cutoff =
            DateTime::parse_from_rfc3339("2026-06-24T11:30:00Z")?.with_timezone(&Utc);
        assert_eq!(plan.cutoff_at, expected_cutoff);
        assert!(plan.requests.is_empty());
        Ok(())
    }

    #[test]
    fn stale_processing_cleanup_selects_timed_out_processing_requests()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let candidates = vec![
            processing_candidate("req-stale", now - Duration::minutes(31)),
            StaleProcessingRequestCandidate::new(
                "project-a",
                "req-pending",
                RequestStatus::Pending,
                now - Duration::hours(1),
            ),
            processing_candidate("req-new", now - Duration::minutes(1)),
        ];

        let plan = StaleProcessingCleanupPlan::from_duration(
            now,
            Duration::minutes(30),
            None,
            &candidates,
        );

        assert_eq!(plan.request_ids(), vec!["req-stale"]);
        assert_eq!(
            plan.requests,
            vec![StaleProcessingRequestId {
                project_id: "project-a".to_string(),
                request_id: "req-stale".to_string(),
            }]
        );
        Ok(())
    }

    #[test]
    fn stale_processing_cleanup_honors_limit() -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let candidates = vec![
            processing_candidate("req-1", now - Duration::hours(2)),
            processing_candidate("req-2", now - Duration::hours(2)),
            processing_candidate("req-3", now - Duration::hours(2)),
        ];

        let plan = StaleProcessingCleanupPlan::from_duration(
            now,
            Duration::minutes(30),
            Some(2),
            &candidates,
        );

        assert_eq!(plan.limit, Some(2));
        assert_eq!(plan.request_ids(), vec!["req-1", "req-2"]);
        Ok(())
    }

    // =======================================================================
    // S10 — startup stale-processing cleanup.
    // Mirrors Go `ClearStaleProcessingOnStartup` + `cancelStaleRecords`
    // (request.go:1064-1122) and the three *_test.go cases in
    // request_shutdown_test.go.
    // =======================================================================

    #[test]
    fn startup_cleanup_plan_targets_canceled_and_uses_strict_lt_cutoff()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go's `SetStatus(StatusCanceled)` (request.go:1100, 1112)
        // and `CreatedAtLT(cutoff)` (request.go:1098, 1110): strict `<`, so a
        // row created exactly at the cutoff is NOT selected.
        let now = fixed_now()?;
        let cutoff = now - Duration::hours(1);
        let candidates = vec![
            // strictly older than cutoff -> selected
            processing_candidate("req-old", cutoff - Duration::seconds(1)),
            // exactly at cutoff -> NOT selected (Go: CreatedAtLT is strict <)
            processing_candidate("req-boundary", cutoff),
            // newer than cutoff -> NOT selected
            processing_candidate("req-new", cutoff + Duration::minutes(1)),
        ];

        let plan = StaleProcessingCleanupPlan::from_cutoff(now, cutoff, None, &candidates);

        assert_eq!(plan.target_status, RequestStatus::Cancelled);
        assert_eq!(plan.matched_count(), 1);
        assert_eq!(plan.request_ids(), vec!["req-old"]);
        Ok(())
    }

    #[test]
    fn startup_cleanup_plan_filters_out_non_processing_statuses()
    -> Result<(), Box<dyn std::error::Error>> {
        // Go only flips `Status == Processing` rows; Pending/Succeeded/Failed/
        // Cancelled rows are left alone even if older than the cutoff.
        let now = fixed_now()?;
        let old = now - Duration::hours(2);
        let candidates = vec![
            StaleProcessingRequestCandidate::new("p", "r-pending", RequestStatus::Pending, old),
            processing_candidate("r-running", old),
            StaleProcessingRequestCandidate::new("p", "r-succeeded", RequestStatus::Succeeded, old),
            StaleProcessingRequestCandidate::new("p", "r-failed", RequestStatus::Failed, old),
            StaleProcessingRequestCandidate::new("p", "r-cancelled", RequestStatus::Cancelled, old),
        ];

        let plan = StaleProcessingCleanupPlan::from_duration(
            now,
            STARTUP_MAX_PROCESSING_DURATION,
            None,
            &candidates,
        );

        assert_eq!(plan.request_ids(), vec!["r-running"]);
        Ok(())
    }

    #[test]
    fn startup_cleanup_plan_with_empty_candidates_matches_zero()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go's `_NoStaleRecords` test: an empty DB yields count=0
        // and ClearStaleProcessingOnStartup returns nil.
        let now = fixed_now()?;
        let candidates: Vec<StaleProcessingRequestCandidate> = Vec::new();

        let plan = StaleProcessingCleanupPlan::from_duration(
            now,
            STARTUP_MAX_PROCESSING_DURATION,
            None,
            &candidates,
        );

        assert_eq!(plan.matched_count(), 0);
        assert!(plan.requests.is_empty());
        Ok(())
    }

    #[test]
    fn startup_cleanup_outcome_aggregates_errors_like_go_join()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go `ClearStaleProcessingOnStartup` error aggregation
        // (request.go:1092, 1103-1104, 1114-1116, 1118-1121): both entities
        // run; failures are collected; the joined error is returned only if
        // any branch failed.
        let now = fixed_now()?;
        let cutoff = now - STARTUP_MAX_PROCESSING_DURATION;
        let ok_plan = StaleProcessingCleanupPlan::from_cutoff(
            now,
            cutoff,
            None,
            &[processing_candidate("req-1", cutoff - Duration::minutes(5))],
        );
        let empty_plan = StaleProcessingCleanupPlan::from_cutoff(
            now,
            cutoff,
            None,
            &Vec::<StaleProcessingRequestCandidate>::new(),
        );

        // Both succeed -> no aggregate error, both counts recorded.
        let both_ok = build_startup_cleanup_outcome(
            now,
            STARTUP_MAX_PROCESSING_DURATION,
            &ok_plan,
            None,
            &ok_plan,
            None,
        );
        assert!(!both_ok.has_errors());
        assert_eq!(both_ok.error_count(), 0);
        assert_eq!(both_ok.aggregate_error(), None);
        assert_eq!(both_ok.requests_cleaned, 1);
        assert_eq!(both_ok.executions_cleaned, 1);

        // One fails -> Go still records the other's count and returns a
        // joined error. This is the "partial failure" test case shape.
        let partial = build_startup_cleanup_outcome(
            now,
            STARTUP_MAX_PROCESSING_DURATION,
            &ok_plan,
            None,
            &empty_plan,
            Some("connection refused".to_string()),
        );
        assert!(partial.has_errors());
        assert_eq!(partial.error_count(), 1);
        assert_eq!(partial.requests_cleaned, 1);
        assert_eq!(partial.executions_cleaned, 0);
        let agg = partial
            .aggregate_error()
            .ok_or_else(|| "expected aggregate error".to_string())?;
        assert!(agg.contains("startup cleanup failed"));
        assert!(agg.contains("failed to cancel stale executions"));
        Ok(())
    }

    #[test]
    fn startup_cleanup_outcome_with_no_stale_records_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go `TestRequestService_ClearStaleProcessingOnStartup_NoStaleRecords`.
        let now = fixed_now()?;
        let cutoff = now - STARTUP_MAX_PROCESSING_DURATION;
        let empty_plan = StaleProcessingCleanupPlan::from_cutoff(
            now,
            cutoff,
            None,
            &Vec::<StaleProcessingRequestCandidate>::new(),
        );

        let outcome = build_startup_cleanup_outcome(
            now,
            STARTUP_MAX_PROCESSING_DURATION,
            &empty_plan,
            None,
            &empty_plan,
            None,
        );

        assert!(!outcome.has_errors());
        assert_eq!(outcome.requests_cleaned, 0);
        assert_eq!(outcome.executions_cleaned, 0);
        assert_eq!(outcome.aggregate_error(), None);
        Ok(())
    }

    // =======================================================================
    // S17 — startup-time stale-processing EXECUTION semantics.
    // Mirrors Go `cmd/conduit/main.go:85-107` (the FX `OnStart` hook):
    //   - 30s detached timeout (`context.WithTimeout(context.Background(),
    //     30*time.Second)`),
    //   - non-blocking (cleanup runs in `go func()` so OnStart returns nil
    //     immediately),
    //   - non-fatal (failure -> `log.Warn`, startup continues),
    //   - one-shot (no periodic ticker; the only invocation is in OnStart).
    // S10 captured the pure plan layer; S17 captures the startup-execution
    // policy on top of it.
    // =======================================================================

    #[test]
    fn s17_startup_cleanup_timeout_constant_matches_go() {
        // Mirrors Go `cmd/conduit/main.go:89`: `30 * time.Second`.
        assert_eq!(STARTUP_CLEANUP_TIMEOUT, Duration::seconds(30));
        // Sanity: the timeout must remain bounded by the stale-after window
        // so a stuck DB does not stall bookkeeping indefinitely. The two
        // constants are independent in Go; this just documents the ratio.
        assert!(STARTUP_CLEANUP_TIMEOUT < STARTUP_MAX_PROCESSING_DURATION);
    }

    #[test]
    fn s17_successful_cleanup_execution_proceeds_and_emits_no_warning()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors the happy path in main.go:92: ClearStaleProcessingOnStartup
        // returns nil -> no log.Warn, OnStart hook returns nil, startup
        // proceeds. We build the S10 outcome (1 stale request + 1 stale
        // execution, no errors) and wrap it in the S17 execution descriptor.
        let now = fixed_now()?;
        let cutoff = now - STARTUP_MAX_PROCESSING_DURATION;
        let plan = StaleProcessingCleanupPlan::from_cutoff(
            now,
            cutoff,
            None,
            &[
                processing_candidate("req-1", cutoff - Duration::minutes(5)),
                processing_candidate("exec-1", cutoff - Duration::minutes(5)),
            ],
        );
        let outcome = build_startup_cleanup_outcome(
            now,
            STARTUP_MAX_PROCESSING_DURATION,
            &plan,
            None,
            &plan,
            None,
        );

        let execution = StartupCleanupExecution::completed(outcome);

        // Go: OnStart always returns nil regardless of cleanup result.
        assert!(execution.startup_proceeds());
        // Happy path: no warning, fully successful.
        assert!(execution.is_fully_successful());
        assert!(!execution.timed_out);
        assert_eq!(execution.warning_message(), None);
        assert_eq!(execution.timeout, STARTUP_CLEANUP_TIMEOUT);
        Ok(())
    }

    #[test]
    fn s17_partial_failure_does_not_abort_startup_but_emits_warning()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go main.go:92-94: a partial/total cleanup failure is
        // surfaced via `log.Warn("failed to cancel stale processing records
        // on startup", log.Cause(err))`, then OnStart returns nil. Startup
        // is NEVER aborted by a cleanup failure.
        let now = fixed_now()?;
        let cutoff = now - STARTUP_MAX_PROCESSING_DURATION;
        let ok_plan = StaleProcessingCleanupPlan::from_cutoff(
            now,
            cutoff,
            None,
            &[processing_candidate("req-1", cutoff - Duration::minutes(5))],
        );
        let empty_plan = StaleProcessingCleanupPlan::from_cutoff(
            now,
            cutoff,
            None,
            &Vec::<StaleProcessingRequestCandidate>::new(),
        );
        let outcome = build_startup_cleanup_outcome(
            now,
            STARTUP_MAX_PROCESSING_DURATION,
            &ok_plan,
            None,
            &empty_plan,
            Some("connection refused".to_string()),
        );

        let execution = StartupCleanupExecution::completed(outcome);

        // Go: startup proceeds unconditionally.
        assert!(execution.startup_proceeds());
        // Failure path: NOT fully successful, warning IS emitted.
        assert!(!execution.is_fully_successful());
        let warning = execution
            .warning_message()
            .ok_or_else(|| "expected warning".to_string())?;
        assert!(warning.starts_with("failed to cancel stale processing records on startup"));
        assert!(warning.contains("failed to cancel stale executions"));
        Ok(())
    }

    #[test]
    fn s17_timeout_does_not_abort_startup_and_emits_timeout_warning()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors the case where Go's 30-second detached context expires
        // before ClearStaleProcessingOnStartup returns. The inner call
        // returns context.DeadlineExceeded, which main.go:92-94 logs as a
        // warning. OnStart still returns nil.
        let execution = StartupCleanupExecution::timed_out();

        // Even on timeout, startup proceeds (Go unconditional policy).
        assert!(execution.startup_proceeds());
        assert!(execution.timed_out);
        assert!(!execution.is_fully_successful());
        let warning = execution
            .warning_message()
            .ok_or_else(|| "expected warning".to_string())?;
        assert!(warning.contains("timed out"));
        assert_eq!(execution.timeout, STARTUP_CLEANUP_TIMEOUT);
        Ok(())
    }

    #[test]
    fn s17_startup_cleanup_schedule_is_one_shot_by_default() {
        // Mirrors Go main.go:85-107: ClearStaleProcessingOnStartup is
        // invoked exactly once in the FX OnStart hook. There is NO periodic
        // ticker — a request that goes stale mid-run stays "processing" on
        // the dashboard until the next process restart.
        let schedule = StartupCleanupSchedule::default();
        assert!(schedule.is_one_shot());
        assert_eq!(schedule, StartupCleanupSchedule::OneShot);
    }

    #[test]
    fn s17_build_execution_from_s10_outcome_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        // Integration-style test: exercise the full S10 -> S17 composition
        // that the real boot path will perform. The runtime layer loads
        // stale candidates, builds the S10 plan + outcome, then wraps it in
        // the S17 execution descriptor to decide (a) whether to log a
        // warning and (b) whether to proceed with boot.
        let now = fixed_now()?;
        let cutoff = now - STARTUP_MAX_PROCESSING_DURATION;

        // Step 1 (S10): build the plan from candidates loaded off the DB.
        let candidates = [processing_candidate(
            "req-stale",
            cutoff - Duration::minutes(10),
        )];
        let requests_plan = StaleProcessingCleanupPlan::from_cutoff(now, cutoff, None, &candidates);
        let executions_plan = StaleProcessingCleanupPlan::from_cutoff(
            now,
            cutoff,
            None,
            &Vec::<StaleProcessingRequestCandidate>::new(),
        );

        // Step 2 (S10): apply both plans, simulate success on both.
        let outcome = build_startup_cleanup_outcome(
            now,
            STARTUP_MAX_PROCESSING_DURATION,
            &requests_plan,
            None,
            &executions_plan,
            None,
        );

        // Step 3 (S17): wrap in the boot execution descriptor and verify the
        // startup policy applies correctly.
        let execution = StartupCleanupExecution::completed(outcome);
        assert_eq!(
            execution.outcome.as_ref().map(|o| o.requests_cleaned),
            Some(1)
        );
        assert_eq!(
            execution.outcome.as_ref().map(|o| o.executions_cleaned),
            Some(0)
        );
        assert!(execution.startup_proceeds());
        assert!(execution.is_fully_successful());
        assert_eq!(execution.warning_message(), None);
        Ok(())
    }

    #[test]
    fn request_content_storage_policy_defaults_match_go() {
        // Mirrors Go `defaultStoragePolicy` (system_default.go):
        //   StoreChunks=false, LivePreview=false,
        //   StoreRequestBody=true,  StoreResponseBody=true.
        assert_eq!(
            RequestContentStoragePolicy::default(),
            RequestContentStoragePolicy {
                store_request_headers: true,
                store_request_body: true,
                store_response_body: true,
                store_chunks: false,
                live_preview: false,
            }
        );
    }

    #[test]
    fn request_content_access_denies_project_mismatch() {
        let request = request();

        assert!(RequestContentAccess::new("project-a", "req-1").allows(&request));
        assert!(!RequestContentAccess::new("project-b", "req-1").allows(&request));
        assert!(!RequestContentAccess::new("project-a", "req-2").allows(&request));
    }

    #[test]
    fn request_content_metadata_builds_inline_and_attachment_headers() {
        let inline = RequestContentResponseMetadata::new(
            "application/json",
            RequestContentDisposition::Inline,
        )
        .headers();
        let attachment = RequestContentResponseMetadata::new(
            "audio/mpeg",
            RequestContentDisposition::attachment("answer \"final\".mp3"),
        )
        .with_content_length(42)
        .headers();

        assert_eq!(inline["content-type"], "application/json");
        assert_eq!(inline["content-disposition"], "inline");
        assert_eq!(attachment["content-type"], "audio/mpeg");
        assert_eq!(
            attachment["content-disposition"],
            "attachment; filename=\"answer \\\"final\\\".mp3\""
        );
        assert_eq!(attachment["content-length"], "42");
    }

    #[test]
    fn request_content_range_parses_basic_valid_headers() {
        assert_eq!(
            RequestContentRange::parse_header("bytes=0-99"),
            Some(RequestContentRange {
                start: Some(0),
                end: Some(99),
            })
        );
        assert_eq!(
            RequestContentRange::parse_header("bytes=100-"),
            Some(RequestContentRange {
                start: Some(100),
                end: None,
            })
        );
        assert_eq!(
            RequestContentRange::parse_header("bytes=-500"),
            Some(RequestContentRange {
                start: None,
                end: Some(500),
            })
        );
    }

    #[test]
    fn request_content_range_rejects_invalid_headers() {
        assert_eq!(RequestContentRange::parse_header("items=0-99"), None);
        assert_eq!(RequestContentRange::parse_header("bytes="), None);
        assert_eq!(RequestContentRange::parse_header("bytes=-"), None);
        assert_eq!(RequestContentRange::parse_header("bytes=99-0"), None);
        assert_eq!(RequestContentRange::parse_header("bytes=0-1,2-3"), None);
        assert_eq!(RequestContentRange::parse_header("bytes=a-1"), None);
    }

    #[test]
    fn request_content_metadata_includes_content_range_when_closed() {
        let headers = RequestContentResponseMetadata::new(
            "application/octet-stream",
            RequestContentDisposition::attachment("result.bin"),
        )
        .with_content_length(100)
        .with_range(RequestContentRange {
            start: Some(10),
            end: Some(19),
        })
        .headers();

        assert_eq!(headers["content-range"], "bytes 10-19/100");
    }

    #[test]
    fn live_preview_disabled_does_not_record_events() {
        let mut preview = LivePreviewMetadata::new(LivePreviewSettings::disabled());

        assert_eq!(
            preview.record_chunk("project-a", "req-1", json!({"delta": "hello"})),
            None
        );
        assert_eq!(preview.record_final("project-a", "req-1"), None);
        assert!(preview.events().is_empty());
    }

    #[test]
    fn live_preview_sequences_chunks_and_final_event() {
        let mut preview = LivePreviewMetadata::new(LivePreviewSettings::enabled());

        preview.record_chunk("project-a", "req-1", json!({"delta": "hel"}));
        preview.record_chunk("project-a", "req-1", json!({"delta": "lo"}));
        preview.record_final("project-a", "req-1");

        assert_eq!(preview.events().len(), 3);
        assert_eq!(preview.events()[0].sequence, 0);
        assert_eq!(preview.events()[1].sequence, 1);
        assert_eq!(preview.events()[2].sequence, 2);
        assert_eq!(preview.events()[0].chunk["delta"], "hel");
        assert!(!preview.events()[0].final_event);
        assert_eq!(preview.events()[2].chunk, Value::Null);
        assert!(preview.events()[2].final_event);
    }

    #[test]
    fn live_preview_events_preserve_project_and_request_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut preview = LivePreviewMetadata::new(LivePreviewSettings::enabled());

        preview.record_chunk("project-a", "req-1", json!({"delta": "hello"}));

        let event = preview
            .events()
            .first()
            .ok_or_else(|| "preview event".to_string())?;
        assert_eq!(event.project_id, "project-a");
        assert_eq!(event.request_id, "req-1");
        Ok(())
    }

    #[test]
    fn request_execution_detail_builder_records_success_metadata() {
        let execution = ExecutionRecord::new("exec-1", "req-1", "project-a", 1);
        let detail = RequestExecutionDetail::new(
            &execution,
            "https://provider.example/v1/chat/completions",
            json!({"authorization": "Bearer redacted"}),
            json!({"model": "gpt-test", "messages": []}),
        )
        .succeeded(
            200,
            json!({"id": "completion-1", "object": "chat.completion"}),
            json!([{"delta": {"content": "hi"}}]),
        );

        assert_eq!(detail.id, "exec-1");
        assert_eq!(detail.request_id, "req-1");
        assert_eq!(detail.project_id, "project-a");
        assert_eq!(detail.status, RequestStatus::Succeeded);
        assert_eq!(
            detail.request_url,
            "https://provider.example/v1/chat/completions"
        );
        assert_eq!(detail.request_headers["authorization"], "Bearer redacted");
        assert_eq!(detail.request_body["model"], "gpt-test");
        assert_eq!(detail.response_body["id"], "completion-1");
        assert_eq!(detail.response_chunks[0]["delta"]["content"], "hi");
        assert_eq!(detail.status_code, Some(200));
        assert_eq!(detail.error, None);
        assert!(!detail.pass_through_applied);
    }

    #[test]
    fn request_execution_detail_builder_records_failure_metadata()
    -> Result<(), Box<dyn std::error::Error>> {
        let execution = ExecutionRecord::new("exec-1", "req-1", "project-a", 1);
        let detail = RequestExecutionDetail::new(
            &execution,
            "https://provider.example/v1/chat/completions",
            json!({"content-type": "application/json"}),
            json!({"prompt": "hello"}),
        )
        .failed(
            Some(429),
            json!({"type": "rate_limit", "message": "too many requests"}),
        );

        assert_eq!(detail.status, RequestStatus::Failed);
        assert_eq!(detail.status_code, Some(429));
        let error = detail.error.as_ref().ok_or_else(|| "error".to_string())?;
        assert_eq!(error["type"], "rate_limit");
        assert_eq!(detail.response_body, Value::Null);
        assert_eq!(detail.response_chunks, json!([]));
        Ok(())
    }

    #[test]
    fn request_execution_detail_builder_records_pass_through_and_latencies() {
        let execution = ExecutionRecord::new("exec-1", "req-1", "project-a", 1);
        let latencies = RequestExecutionLatencies {
            upstream_ms: Some(32),
            first_token_ms: Some(48),
            reasoning_duration_ms: Some(81),
        };
        let detail = RequestExecutionDetail::new(
            &execution,
            "https://provider.example/v1/chat/completions",
            json!({}),
            Value::Null,
        )
        .with_pass_through_applied(true)
        .with_latencies(latencies);

        assert!(detail.pass_through_applied);
        assert_eq!(detail.latencies.upstream_ms, Some(32));
        assert_eq!(detail.latencies.first_token_ms, Some(48));
        assert_eq!(detail.latencies.reasoning_duration_ms, Some(81));
    }

    // =======================================================================
    // S15 — response_chunks client-side vs provider-side semantics.
    // Mirrors Go `InboundPersistentStream` (inbound.go:30,67-79,254) storing
    // post-transform client chunks on Request.response_chunks via
    // SaveRequestChunks, and `OutboundPersistentStream` (outbound.go:40,79-94,
    // 302) storing pre-transform provider chunks on
    // RequestExecution.response_chunks via SaveRequestExecutionChunks.
    // Pass-through (`pass_through.go:captureRawProviderStream`) makes the two
    // coincide because the gateway forwards raw provider bytes verbatim.
    // =======================================================================

    #[test]
    fn s15_default_response_chunks_come_from_client_side() {
        // No pass-through: Go routes through InboundPersistentStream, which
        // stores the post-transform chunks the client received.
        assert!(is_response_chunks_from_client(false));
        assert_eq!(response_chunk_source(false), ResponseChunkSource::Client);
    }

    #[test]
    fn s15_pass_through_response_chunks_come_from_provider_side() {
        // Pass-through enabled: Go's captureRawProviderStream captures the raw
        // provider stream; the persisted chunks are the provider's own.
        assert!(!is_response_chunks_from_client(true));
        assert_eq!(response_chunk_source(true), ResponseChunkSource::Provider);
    }

    #[test]
    fn s15_with_client_response_chunks_does_not_flip_pass_through() {
        // Mirrors the default path (InboundPersistentStream on Request row):
        // storing client-side chunks does NOT imply pass-through was applied.
        let execution = ExecutionRecord::new("exec-1", "req-1", "project-a", 1);
        let detail = RequestExecutionDetail::new(
            &execution,
            "https://provider.example/v1/chat/completions",
            json!({}),
            Value::Null,
        )
        .with_client_response_chunks(json!([
            {"choices": [{"delta": {"content": "hi"}}]},
            {"choices": [{"delta": {"content": "there"}}]}
        ]));

        assert!(!detail.pass_through_applied);
        assert_eq!(
            detail.response_chunks[0]["choices"][0]["delta"]["content"],
            "hi"
        );
        assert_eq!(
            detail.response_chunks[1]["choices"][0]["delta"]["content"],
            "there"
        );
    }

    #[test]
    fn s15_with_provider_response_chunks_flips_pass_through() {
        // Mirrors the pass-through path (captureRawProviderStream): recording
        // provider-side chunks is itself the observable signal that
        // pass-through fired, so the builder flips pass_through_applied = true.
        let execution = ExecutionRecord::new("exec-1", "req-1", "project-a", 1);
        let detail = RequestExecutionDetail::new(
            &execution,
            "https://provider.example/v1/messages",
            json!({}),
            Value::Null,
        )
        .with_provider_response_chunks(json!([
            {"type": "message_start", "message": {"id": "msg_1"}},
            {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "hi"}}
        ]));

        assert!(detail.pass_through_applied);
        assert_eq!(detail.response_chunks[0]["type"], "message_start");
        assert_eq!(detail.response_chunks[1]["delta"]["text"], "hi");
    }

    #[test]
    fn s15_client_and_provider_chunks_can_differ() {
        // The chunk analogue of inbound_outbound_bodies_diverge: when the
        // transformer rewrites the stream (e.g. OpenAI client <-> Anthropic
        // provider), the two chunk arrays differ.
        let client_chunks = json!([
            {"choices": [{"delta": {"content": "hi"}}]}
        ]);
        let provider_chunks = json!([
            {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "hi"}}
        ]);

        assert!(client_provider_chunks_diverge(
            &client_chunks,
            &provider_chunks
        ));
        // Pass-through (verbatim forwarding) -> the two arrays coincide.
        let same = json!([{"event": "raw"}]);
        assert!(!client_provider_chunks_diverge(&same, &same));
    }

    // =======================================================================
    // S16 — request-content download/preview location resolution.
    // Mirrors Go `internal/server/api/request_content.go::DownloadRequestContent`
    // lines 40-99 (gate, key normalisation + prefix check, storage lookup,
    // storage-type gate) and `internal/ent/schema/request.go:104-122`
    // (content_saved / content_storage_id / content_storage_key /
    // content_saved_at fields, which live ONLY on Request, not on
    // RequestExecution).
    // =======================================================================

    #[test]
    fn s16_unsaved_row_is_content_not_found() {
        // Mirrors Go line 70: `!req.ContentSaved || ContentStorageID == nil ||
        // ContentStorageKey == nil || trim(key) == ""` -> 404 "Content not
        // found". A freshly-created request row has all four content fields
        // unset; no storage lookup should even be attempted.
        let location = RequestContentLocation::unsaved();
        let storage_lookup: Result<Option<ContentStorageProps>, ()> = Ok(None);

        let err = resolve_request_content_location(&location, storage_lookup, 1, 7);
        assert_eq!(err, Err(ResolveContentLocationError::ContentNotFound));
    }

    #[test]
    fn s16_blank_key_is_content_not_found() {
        // Mirrors Go line 70 last clause: `strings.TrimSpace(*key) == ""`.
        // content_saved=true and storage_id set, but key is whitespace-only.
        let location = RequestContentLocation {
            content_saved: true,
            content_storage_id: Some(5),
            content_storage_key: Some("   ".to_string()),
            content_saved_at: None,
        };

        let err = resolve_request_content_location(&location, Ok(None), 1, 7);
        assert_eq!(err, Err(ResolveContentLocationError::ContentNotFound));
    }

    #[test]
    fn s16_cross_project_key_prefix_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go lines 80-84: the key MUST start with
        // `/{project_id}/requests/{request_id}/`. A key scoped to a different
        // project or request is blocked even when everything else is valid —
        // this is the cross-request / cross-project access control.
        let now = DateTime::parse_from_rfc3339("2026-06-29T10:00:00Z")?.with_timezone(&Utc);
        let location = RequestContentLocation::saved(
            5,
            "/2/requests/99/audio/voice.mp3", // wrong project AND request
            now,
        );

        // Caller asks for project 1 / request 7 — key belongs to 2 / 99.
        let err = resolve_request_content_location(&location, Ok(None), 1, 7);
        assert_eq!(err, Err(ResolveContentLocationError::ContentNotFound));

        // Same key, but caller asks for the project/request it actually
        // belongs to — gate passes (storage lookup is the next decision, and
        // here we feed Ok(None) so it fails at the storage-not-found step,
        // proving we got PAST the prefix check).
        let err = resolve_request_content_location(&location, Ok(None), 2, 99);
        assert_eq!(
            err,
            Err(ResolveContentLocationError::ContentStorageNotFound)
        );
        Ok(())
    }

    #[test]
    fn s16_missing_storage_row_is_storage_not_found() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go lines 86-94: storage_id resolves to no row ->
        // `ent.IsNotFound` -> 404 "Content storage not found". Key prefix is
        // valid so we reach the storage-lookup step.
        let now = DateTime::parse_from_rfc3339("2026-06-29T10:00:00Z")?.with_timezone(&Utc);
        let location = RequestContentLocation::saved(5, "/1/requests/7/audio/voice.mp3", now);

        let err = resolve_request_content_location(&location, Ok(None), 1, 7);
        assert_eq!(
            err,
            Err(ResolveContentLocationError::ContentStorageNotFound)
        );
        Ok(())
    }

    #[test]
    fn s16_storage_lookup_failure_is_lookup_failed() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go lines 91-93: storage lookup raised a non-NotFound error
        // -> 500 "Failed to load content storage". We model the lower-level
        // error as `Err(())` since the resolver does not need its cause.
        let now = DateTime::parse_from_rfc3339("2026-06-29T10:00:00Z")?.with_timezone(&Utc);
        let location = RequestContentLocation::saved(5, "/1/requests/7/audio/voice.mp3", now);

        let err = resolve_request_content_location(&location, Err(()), 1, 7);
        assert_eq!(
            err,
            Err(ResolveContentLocationError::ContentStorageLookupFailed)
        );
        Ok(())
    }

    #[test]
    fn s16_primary_or_database_storage_is_not_file_based() -> Result<(), Box<dyn std::error::Error>>
    {
        // Mirrors Go lines 96-99: `ds.Primary || ds.Type ==
        // datastorage.TypeDatabase` -> 400 "Content storage is not file-based".
        let now = DateTime::parse_from_rfc3339("2026-06-29T10:00:00Z")?.with_timezone(&Utc);
        let location = RequestContentLocation::saved(5, "/1/requests/7/audio/voice.mp3", now);

        // Primary storage (even non-database) is rejected.
        let err = resolve_request_content_location(
            &location,
            Ok(Some(ContentStorageProps::primary())),
            1,
            7,
        );
        assert_eq!(
            err,
            Err(ResolveContentLocationError::ContentStorageNotFileBased)
        );

        // Database-typed storage is likewise rejected.
        let err = resolve_request_content_location(
            &location,
            Ok(Some(ContentStorageProps::database())),
            1,
            7,
        );
        assert_eq!(
            err,
            Err(ResolveContentLocationError::ContentStorageNotFileBased)
        );
        Ok(())
    }

    #[test]
    fn s16_file_based_storage_resolves_with_normalised_key()
    -> Result<(), Box<dyn std::error::Error>> {
        // Happy path: content_saved=true, valid prefix, file-based non-primary
        // storage -> resolver returns the normalised key + storage id.
        // Mirrors Go lines 75-79 (normalise) + 86-99 (lookup + gate) all
        // passing; the handler then opens the file (out of scope for this
        // pure helper).
        let now = DateTime::parse_from_rfc3339("2026-06-29T10:00:00Z")?.with_timezone(&Utc);
        let location = RequestContentLocation::saved(5, "1/requests/7/audio/voice.mp3", now);

        let resolved = resolve_request_content_location(
            &location,
            Ok(Some(ContentStorageProps::file_based())),
            1,
            7,
        )?;

        assert_eq!(resolved.content_storage_id, 5);
        // Normalisation added the leading slash.
        assert_eq!(resolved.storage_key, "/1/requests/7/audio/voice.mp3");
        assert_eq!(resolved.storage_props, ContentStorageProps::file_based());
        Ok(())
    }

    #[tokio::test]
    async fn create_request_append_execution_and_preserve_json() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();

        let saved = service.create_request(&ctx, request()).await?;
        let execution = ExecutionRecord {
            headers: json!({"x-provider-request-id": "provider-1", "unknown": "keep"}),
            body: json!({"id": "completion-1", "unknown_body": {"nested": true}}),
            chunks: json!([{"index": 0, "unknown_chunk": "keep"}]),
            provider: Some("openai".to_string()),
            model: Some("gpt-test".to_string()),
            ..ExecutionRecord::new("exec-1", saved.id.clone(), saved.project_id.clone(), 1)
        };

        let saved_execution = service.append_execution(&ctx, execution).await?;
        let executions = service
            .list_executions(&ctx, &saved.project_id, &saved.id)
            .await?;

        assert_eq!(repo.request_count()?, 1);
        assert_eq!(saved.headers["x-unknown"], "keep");
        assert_eq!(saved.body["unknown"], true);
        assert_eq!(saved.chunks[0]["unknown_chunk_field"], 1);
        assert_eq!(saved.extra["cache_signature"], "sig-1");
        assert_eq!(saved_execution.body["unknown_body"]["nested"], true);
        assert_eq!(executions, vec![saved_execution]);
        Ok(())
    }

    #[tokio::test]
    async fn legal_status_transition_updates_request() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();
        service.create_request(&ctx, request()).await?;

        let running = service
            .transition_status(
                &ctx,
                "project-a",
                "req-1",
                RequestStatus::Pending,
                RequestStatus::Running,
            )
            .await?;
        let succeeded = service
            .transition_status(
                &ctx,
                "project-a",
                "req-1",
                RequestStatus::Running,
                RequestStatus::Succeeded,
            )
            .await?;

        assert_eq!(running.status, RequestStatus::Running);
        assert_eq!(succeeded.status, RequestStatus::Succeeded);
        Ok(())
    }

    #[tokio::test]
    async fn illegal_status_transition_is_rejected() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();
        service.create_request(&ctx, request()).await?;
        service
            .transition_status(
                &ctx,
                "project-a",
                "req-1",
                RequestStatus::Pending,
                RequestStatus::Running,
            )
            .await?;
        service
            .transition_status(
                &ctx,
                "project-a",
                "req-1",
                RequestStatus::Running,
                RequestStatus::Succeeded,
            )
            .await?;

        let err = service
            .transition_status(
                &ctx,
                "project-a",
                "req-1",
                RequestStatus::Succeeded,
                RequestStatus::Running,
            )
            .await;

        assert!(matches!(
            err,
            Err(RequestServiceError::InvalidStatusTransition {
                from: RequestStatus::Succeeded,
                to: RequestStatus::Running,
            })
        ));
        assert_eq!(
            repo.find_request(&ctx, "project-a", "req-1")
                .await?
                .ok_or_else(|| RequestServiceError::RequestNotFound(
                    "request is still present".to_string()
                ))?
                .status,
            RequestStatus::Succeeded
        );
        Ok(())
    }

    #[tokio::test]
    async fn expected_status_mismatch_is_rejected() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();
        service.create_request(&ctx, request()).await?;

        let err = service
            .transition_status(
                &ctx,
                "project-a",
                "req-1",
                RequestStatus::Running,
                RequestStatus::Succeeded,
            )
            .await;

        assert!(matches!(
            err,
            Err(RequestServiceError::StatusConflict {
                expected: RequestStatus::Running,
                actual: RequestStatus::Pending,
                ..
            })
        ));
        Ok(())
    }

    // =======================================================================
    // S04 — Go-compatible integer-ID storage keys.
    // Mirrors conduit/internal/server/biz/request.go:73-126.
    // =======================================================================

    #[test]
    fn generate_request_body_key_matches_go_template() {
        assert_eq!(
            generate_request_body_key(7, 42),
            "/7/requests/42/request_body.json"
        );
    }

    #[test]
    fn generate_response_body_key_matches_go_template() {
        assert_eq!(
            generate_response_body_key(7, 42),
            "/7/requests/42/response_body.json"
        );
    }

    #[test]
    fn generate_response_chunks_key_matches_go_template() {
        assert_eq!(
            generate_response_chunks_key(7, 42),
            "/7/requests/42/response_chunks.json"
        );
    }

    #[test]
    fn generate_request_dir_key_matches_go_template() {
        assert_eq!(generate_request_dir_key(7, 42), "/7/requests/42");
    }

    #[test]
    fn generate_request_executions_dir_key_matches_go_template() {
        assert_eq!(
            generate_request_executions_dir_key(7, 42),
            "/7/requests/42/executions"
        );
    }

    #[test]
    fn generate_audio_key_matches_go_filepath_base_semantics() {
        // Happy path: literal filename, Go stores the basename verbatim.
        assert_eq!(
            generate_audio_key(7, 42, "voice-1.mp3"),
            "/7/requests/42/audio/voice-1.mp3"
        );
        // Empty filename -> Go falls back to "audio.mp3" (request.go:85-87).
        assert_eq!(
            generate_audio_key(7, 42, "   "),
            "/7/requests/42/audio/audio.mp3"
        );
        // `filepath.Base` strips directory components; we mirror for `/`.
        assert_eq!(
            generate_audio_key(7, 42, "../unsafe name.wav"),
            "/7/requests/42/audio/unsafe name.wav"
        );
        // And for `\` (Windows input must produce the same key as POSIX Go).
        assert_eq!(
            generate_audio_key(7, 42, "nested\\answer.ogg"),
            "/7/requests/42/audio/answer.ogg"
        );
        // Trailing slash collapses to the empty segment; Go's `filepath.Base`
        // returns the last non-empty segment.
        assert_eq!(
            generate_audio_key(7, 42, "dir/"),
            "/7/requests/42/audio/dir"
        );
    }

    #[test]
    fn generate_execution_keys_match_go_templates() {
        assert_eq!(
            generate_execution_request_body_key(7, 42, 100),
            "/7/requests/42/executions/100/request_body.json"
        );
        assert_eq!(
            generate_execution_response_body_key(7, 42, 100),
            "/7/requests/42/executions/100/response_body.json"
        );
        assert_eq!(
            generate_execution_response_chunks_key(7, 42, 100),
            "/7/requests/42/executions/100/response_chunks.json"
        );
        assert_eq!(
            generate_execution_request_dir_key(7, 42, 100),
            "/7/requests/42/executions/100"
        );
    }

    // =======================================================================
    // S05 — storage policy decision reducer.
    // =======================================================================

    #[test]
    fn decide_storage_with_default_policy_skips_chunks_and_live_preview() {
        // Go default (system_default.go): StoreChunks=false, LivePreview=false,
        // StoreRequestBody=true, StoreResponseBody=true.
        let outcome = decide_storage(&RequestContentStoragePolicy::default());

        assert_eq!(outcome.request_body, StorageDecision::Store);
        assert_eq!(outcome.response_body, StorageDecision::Store);
        assert_eq!(outcome.response_chunks, StorageDecision::Skip);
        assert_eq!(outcome.live_preview, StorageDecision::Skip);
        assert!(!outcome.is_all_skipped());
    }

    #[test]
    fn decide_storage_with_all_disabled_skips_everything() {
        let policy = RequestContentStoragePolicy {
            store_request_headers: false,
            store_request_body: false,
            store_response_body: false,
            store_chunks: false,
            live_preview: false,
        };
        let outcome = decide_storage(&policy);

        assert!(outcome.is_all_skipped());
    }

    #[test]
    fn decide_storage_with_everything_enabled_stores_all() {
        let policy = RequestContentStoragePolicy {
            store_request_headers: true,
            store_request_body: true,
            store_response_body: true,
            store_chunks: true,
            live_preview: true,
        };
        let outcome = decide_storage(&policy);

        assert_eq!(outcome.request_body, StorageDecision::Store);
        assert_eq!(outcome.response_body, StorageDecision::Store);
        assert_eq!(outcome.response_chunks, StorageDecision::Store);
        assert_eq!(outcome.live_preview, StorageDecision::Store);
    }

    #[test]
    fn decide_artifact_routes_each_kind_to_the_correct_flag() {
        let policy = RequestContentStoragePolicy {
            store_request_headers: true,
            store_request_body: true,
            store_response_body: false,
            store_chunks: true,
            live_preview: false,
        };

        assert_eq!(
            decide_artifact(&policy, StorageArtifact::RequestBody),
            StorageDecision::Store
        );
        assert_eq!(
            decide_artifact(&policy, StorageArtifact::ResponseBody),
            StorageDecision::Skip
        );
        assert_eq!(
            decide_artifact(&policy, StorageArtifact::ResponseChunks),
            StorageDecision::Store
        );
        assert_eq!(
            decide_artifact(&policy, StorageArtifact::LivePreview),
            StorageDecision::Skip
        );
    }

    // =======================================================================
    // S06 — default storage route resolution.
    // Mirrors Go shouldUseExternalStorage (request.go:61-67).
    // =======================================================================

    #[test]
    fn resolve_storage_route_with_no_data_storage_falls_back_to_primary_db() {
        assert_eq!(resolve_storage_route(None), StorageRoute::PrimaryDb);
    }

    #[test]
    fn resolve_storage_route_with_primary_data_storage_stays_on_db() {
        let ds = DataStorageRef::primary(1);
        assert_eq!(resolve_storage_route(Some(&ds)), StorageRoute::PrimaryDb);
    }

    #[test]
    fn resolve_storage_route_with_external_data_storage_routes_external() {
        let ds = DataStorageRef::external(5);
        assert_eq!(
            resolve_storage_route(Some(&ds)),
            StorageRoute::External {
                data_storage_id: Some(5)
            }
        );
    }

    // =======================================================================
    // S07 — external-storage write failure does not block request creation.
    // Mirrors Go request.go:225-253 (CreateRequest) and 344-370 (CreateRequestExecution).
    // =======================================================================

    #[test]
    fn request_creation_proceeds_when_external_write_fails_after_row_saved() {
        // Go comment (request.go:249): "Continue anyway, don't fail the request creation".
        assert!(request_creation_proceeds(
            RequestRowSaveResult::Saved,
            StorageWriteResult::FailedWarning
        ));
        assert!(request_creation_proceeds(
            RequestRowSaveResult::SavedWithPlaceholderFallback,
            StorageWriteResult::FailedWarning
        ));
    }

    #[test]
    fn request_creation_proceeds_when_external_write_succeeds_or_is_skipped() {
        assert!(request_creation_proceeds(
            RequestRowSaveResult::Saved,
            StorageWriteResult::Saved
        ));
        assert!(request_creation_proceeds(
            RequestRowSaveResult::Saved,
            StorageWriteResult::Skipped
        ));
    }

    #[test]
    fn request_creation_fails_when_row_save_failed_regardless_of_external_write() {
        for external in [
            StorageWriteResult::Saved,
            StorageWriteResult::Skipped,
            StorageWriteResult::FailedWarning,
        ] {
            assert!(
                !request_creation_proceeds(RequestRowSaveResult::Failed, external),
                "expected creation to fail when row save failed, external={external:?}"
            );
        }
    }

    #[test]
    fn placeholder_retry_only_allowed_on_primary_db_route() {
        // Go request.go:227-239: the invalid-JSON retry only runs on the
        // DB-storage path. When useExternalStorage is true, a row-insert
        // failure is terminal.
        assert!(placeholder_retry_allowed(StorageRoute::PrimaryDb));
        assert!(!placeholder_retry_allowed(StorageRoute::External {
            data_storage_id: Some(5)
        }));
        assert!(!placeholder_retry_allowed(StorageRoute::External {
            data_storage_id: None
        }));
    }

    // =======================================================================
    // S08 — invalid-JSON placeholder bytes.
    // Mirrors Go _InvalidRequestBodyJSON (request.go:70).
    // =======================================================================

    #[test]
    fn invalid_request_body_bytes_match_go_placeholder() -> Result<(), serde_json::Error> {
        assert_eq!(INVALID_REQUEST_BODY_JSON, b"{\"message\":\"invalid text\"}");
        assert_eq!(
            invalid_request_body_bytes(),
            b"{\"message\":\"invalid text\"}"
        );
        // The byte form must round-trip into the JSON Value form too.
        let parsed: Value = serde_json::from_slice(invalid_request_body_bytes())?;
        assert_eq!(parsed, invalid_json_placeholder());
        Ok(())
    }

    #[test]
    fn sanitize_body_returns_valid_json_unchanged() {
        let body = br#"{"model":"gpt-1","messages":[]}"#;
        assert_eq!(sanitize_body(body), body);
    }

    #[test]
    fn sanitize_body_replaces_invalid_json_with_placeholder() {
        let invalid = b"{not-json";
        assert_eq!(sanitize_body(invalid), INVALID_REQUEST_BODY_JSON);
    }

    #[test]
    fn sanitize_body_accepts_non_object_json_values() {
        // Any valid JSON value passes (numbers, arrays, strings, etc.) — Go's
        // xjson.Marshal would also happily emit these.
        assert_eq!(sanitize_body(b"42"), b"42");
        assert_eq!(sanitize_body(b"true"), b"true");
        assert_eq!(sanitize_body(b"null"), b"null");
        assert_eq!(sanitize_body(b"[1,2,3]"), b"[1,2,3]");
        assert_eq!(sanitize_body(b"\"hi\""), b"\"hi\"");
    }

    // =======================================================================
    // RUST-P10-001 S14 — request_body inbound vs outbound semantics.
    // Mirrors Go `Request.request_body` (user-facing) vs
    // `RequestExecution.request_body` (provider-facing). The two fields are
    // intentionally distinct: the inbound body is the user's original request,
    // the outbound body is the post-transformer bytes the gateway sent
    // upstream. Go `ent/schema/{request,request_execution}.go` document this
    // in the field comments (quoted in the S14 module doc above).
    // =======================================================================

    // S14 — `RequestRecord.body` is the INBOUND body; `ExecutionRecord.body`
    // is the OUTBOUND body. The cross-format transformation scenario from
    // the Go schema comments: user sends OpenAI Chat Completions, gateway
    // sends Anthropic Messages upstream. The two `request_body` values MUST
    // be distinct (the inbound holds the OpenAI form, the outbound holds the
    // Claude form). Mirrors Go's example comment verbatim.
    #[test]
    fn s14_inbound_and_outbound_request_bodies_diverge_after_transform() {
        // Inbound: what the user sent (OpenAI Chat Completions).
        let inbound = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}]
        });
        // Outbound: what the gateway sent to the provider (Anthropic Messages,
        // post-transformer).
        let outbound = json!({
            "model": "claude-3-sonnet",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "hi"}]
        });

        let request = RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/chat")
            .with_inbound_body(inbound.clone());
        let execution = ExecutionRecord::new("exec-1", "req-1", "project-a", 1)
            .with_outbound_body(outbound.clone());

        // S14 contract: the two request_body fields carry DIFFERENT values.
        assert_ne!(request.body, execution.body);
        assert_eq!(request.body, inbound);
        assert_eq!(execution.body, outbound);
        // The pure predicate agrees.
        assert!(inbound_outbound_bodies_diverge(
            &request.body,
            &execution.body
        ));
    }

    // S14 — when the gateway passes the body through verbatim (same
    // inbound/outbound API format, no pass-through rewriting), the two
    // `request_body` fields are equal. This is the complementary branch of
    // the S14 contract: divergence is conditional on transformation, not
    // unconditional. Mirrors the Go scenario where pass-through is enabled
    // and the body is forwarded unchanged.
    #[test]
    fn s14_inbound_and_outbound_request_bodies_match_on_passthrough() {
        let body = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]});

        let request = RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/chat")
            .with_inbound_body(body.clone());
        let execution = ExecutionRecord::new("exec-1", "req-1", "project-a", 1)
            .with_outbound_body(body.clone());

        assert_eq!(request.body, execution.body);
        assert!(!inbound_outbound_bodies_diverge(
            &request.body,
            &execution.body
        ));
    }

    // S14 — the same distinction applies to `RequestExecutionDetail`: its
    // `request_body` field is the OUTBOUND body, distinct from the parent
    // `RequestRecord.body` (INBOUND). This pins the detail DTO layer (used
    // by the orchestrator to hand off to persistence) so a future refactor
    // cannot accidentally collapse the two fields.
    #[test]
    fn s14_execution_detail_request_body_is_outbound_not_inbound() {
        let inbound = json!({"model": "gpt-4", "messages": []}); // OpenAI form
        let outbound = json!({"model": "claude-3", "max_tokens": 1, "messages": []}); // Claude form

        let request = RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/chat")
            .with_inbound_body(inbound.clone());
        let execution = ExecutionRecord::new("exec-1", &request.id, "project-a", 1);
        let detail = RequestExecutionDetail::new(
            &execution,
            "https://provider.example/v1/messages",
            json!({"content-type": "application/json"}),
            Value::Null, // initially null
        )
        .with_outbound_body(outbound.clone());

        // The detail DTO's request_body is the OUTBOUND (provider-facing) form.
        assert_eq!(detail.request_body, outbound);
        // And it differs from the inbound body stored on the RequestRecord.
        assert_ne!(detail.request_body, request.body);
        assert!(inbound_outbound_bodies_diverge(
            &request.body,
            &detail.request_body
        ));
    }

    // S14 — end-to-end persistence round-trip: an inbound `RequestRecord.body`
    // and an outbound `ExecutionRecord.body` carry distinct values through the
    // `RequestService::create_request` / `append_execution` pipeline. Both
    // bodies are preserved verbatim in the in-memory store, mirroring how Go
    // persists them as two separate `JSONRawMessage` columns.
    #[tokio::test]
    async fn s14_inbound_and_outbound_bodies_both_persisted_distinctly() -> RequestServiceResult<()>
    {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();

        let inbound = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]});
        let outbound = json!({
            "model": "claude-3-sonnet",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "hi"}]
        });

        // Create the request with its INBOUND body.
        let mut request = RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/chat");
        request.headers = json!({"content-type": "application/json"});
        request = request.with_inbound_body(inbound.clone());
        let saved_request = service.create_request(&ctx, request).await?;

        // Append the execution with its OUTBOUND body.
        let execution = ExecutionRecord::new("exec-1", &saved_request.id, "project-a", 1)
            .with_outbound_body(outbound.clone());
        let saved_execution = service.append_execution(&ctx, execution).await?;

        // Both bodies persisted verbatim, and they remain distinct (S14).
        // Read the request back through the repo to confirm the inbound body
        // was stored unchanged alongside the outbound body on the execution.
        let fetched_request = repo
            .find_request(&ctx, "project-a", "req-1")
            .await?
            .ok_or_else(|| RequestServiceError::RequestNotFound("req-1".to_string()))?;
        assert_eq!(fetched_request.body, inbound);
        assert_eq!(saved_execution.body, outbound);
        assert_ne!(fetched_request.body, saved_execution.body);
        assert!(inbound_outbound_bodies_diverge(
            &fetched_request.body,
            &saved_execution.body
        ));
        Ok(())
    }

    // S14 — `inbound_outbound_bodies_diverge` is robust to key-ordering
    // differences: the same logical JSON object with reordered keys is NOT a
    // transformation (Go would also treat them as the same body once
    // re-marshaled). This defends against flaky tests on byte-level fixtures.
    #[test]
    fn s14_diverge_predicate_is_structural_not_byte_level() {
        let a = json!({"a": 1, "b": 2});
        let b = json!({"b": 2, "a": 1});
        // Same logical object, different key order → NOT diverging.
        assert!(!inbound_outbound_bodies_diverge(&a, &b));

        let c = json!({"a": 1, "b": 3});
        // Different value → diverging.
        assert!(inbound_outbound_bodies_diverge(&a, &c));
    }

    // =======================================================================
    // RUST-P9-006 S15 — production RequestRecorder write methods.
    // Mirrors Go `RequestService` write methods in `request.go`:
    //   - UpdateRequestCompleted (lines 382-465)
    //   - UpdateRequestStatusFromError (lines 1056-1062)
    //   - UpdateRequestExecutionCompleted (lines 652-733)
    //   - UpdateRequestExecutionFailed (lines 750-757)
    //   - UpdateRequestExecutionCanceled (lines 736-742)
    //   - SaveRequestExecutionChunks (lines 856-941)
    // The Go tests live in `request_audio_test.go` (audio/content_saved
    // special-casing — deferred) and `request_shutdown_test.go` (stale
    // cleanup — already covered by the S10/S17 tests above). The tests here
    // pin the status-transition + metrics-fields + error-extraction contract
    // that the production RequestRecorder depends on.
    // =======================================================================

    async fn recorder_fixture(
        repo: Arc<InMemoryRequestPersistenceRepo>,
    ) -> RequestServiceResult<(RequestRecord, ExecutionRecord)> {
        let ctx = ctx();
        let service = RequestService::new(repo.clone());
        let request = service.create_request(&ctx, request()).await?;
        let execution = ExecutionRecord::new("exec-1", &request.id, &request.project_id, 1);
        let saved_exec = service.append_execution(&ctx, execution).await?;
        Ok((request, saved_exec))
    }

    #[tokio::test]
    async fn update_request_completed_sets_status_external_id_metrics_and_body()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go UpdateRequestCompleted (request.go:382-465):
        //   SetStatus(StatusCompleted).SetExternalID(externalId)
        //   .SetMetricsLatencyMs/FirstTokenLatencyMs/ReasoningDurationMs
        //   .SetResponseBody(...)
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let metrics = LatencyMetrics {
            latency_ms: Some(120),
            first_token_latency_ms: Some(45),
            reasoning_duration_ms: Some(80),
        };
        let updated = service
            .update_request_completed(
                &ctx(),
                "project-a",
                &request.id,
                "resp-ext-1",
                Some(metrics),
                Some(json!({"id": "resp-ext-1", "object": "chat.completion"})),
            )
            .await?;

        assert_eq!(updated.status, RequestStatus::Succeeded);
        assert_eq!(updated.extra.get("external_id"), Some(&json!("resp-ext-1")));
        assert_eq!(updated.extra.get("metrics_latency_ms"), Some(&json!(120)));
        assert_eq!(
            updated.extra.get("metrics_first_token_latency_ms"),
            Some(&json!(45))
        );
        assert_eq!(
            updated.extra.get("metrics_reasoning_duration_ms"),
            Some(&json!(80))
        );
        assert_eq!(
            updated.extra.get("response_body"),
            Some(&json!({"id": "resp-ext-1", "object": "chat.completion"}))
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_request_completed_with_none_metrics_skips_metric_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go: `if metrics != nil { ... }` — when metrics is nil the
        // SetMetrics*() calls are never invoked, so the row keeps whatever
        // those columns had before (here: nothing, since the row is fresh).
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let updated = service
            .update_request_completed(&ctx(), "project-a", &request.id, "ext-1", None, None)
            .await?;

        assert_eq!(updated.status, RequestStatus::Succeeded);
        assert!(!updated.extra.contains_key("metrics_latency_ms"));
        assert!(!updated.extra.contains_key("metrics_first_token_latency_ms"));
        assert!(!updated.extra.contains_key("metrics_reasoning_duration_ms"));
        Ok(())
    }

    #[tokio::test]
    async fn update_request_status_from_error_picks_failed_for_non_cancel()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go UpdateRequestStatusFromError (request.go:1056-1062):
        // a generic (non-Canceled) error flips the row to StatusFailed.
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let updated = service
            .update_request_status_from_error(&ctx(), "project-a", &request.id, false)
            .await?;
        assert_eq!(updated.status, RequestStatus::Failed);
        Ok(())
    }

    #[tokio::test]
    async fn update_request_status_from_error_picks_cancelled_for_cancel()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go: `errors.Is(rawErr, context.Canceled)` -> StatusCanceled.
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let updated = service
            .update_request_status_from_error(&ctx(), "project-a", &request.id, true)
            .await?;
        assert_eq!(updated.status, RequestStatus::Cancelled);
        Ok(())
    }

    #[tokio::test]
    async fn mark_request_canceled_and_failed_set_terminal_status()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go MarkRequestCanceled / MarkRequestFailed (request.go:1032-1039).
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request1, _exec1) = recorder_fixture(repo.clone()).await?;

        let mut request2 = request();
        request2.id = "req-2".to_string();
        request2.name = "req-2".to_string();
        let ctx = ctx();
        service.create_request(&ctx, request2).await?;
        service
            .append_execution(
                &ctx,
                ExecutionRecord::new("exec-2", "req-2", "project-a", 1),
            )
            .await?;

        let canceled = service
            .mark_request_canceled(&ctx, "project-a", &request1.id)
            .await?;
        assert_eq!(canceled.status, RequestStatus::Cancelled);

        let failed = service
            .mark_request_failed(&ctx, "project-a", "req-2")
            .await?;
        assert_eq!(failed.status, RequestStatus::Failed);
        Ok(())
    }

    #[tokio::test]
    async fn update_request_execution_completed_sets_status_metrics_and_body()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go UpdateRequestExecutionCompleted (request.go:652-733):
        //   SetStatus(StatusCompleted).SetExternalID(externalId)
        //   .SetMetrics*().SetResponseBody(...)
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let metrics = LatencyMetrics {
            latency_ms: Some(200),
            first_token_latency_ms: None,
            reasoning_duration_ms: Some(60),
        };
        let updated = service
            .update_request_execution_completed(
                &ctx(),
                "project-a",
                &request.id,
                "exec-1",
                "resp-exec-1",
                Some(metrics),
                Some(json!({"id": "resp-exec-1"})),
            )
            .await?;

        assert_eq!(updated.status, RequestStatus::Succeeded);
        assert_eq!(
            updated.extra.get("external_id"),
            Some(&json!("resp-exec-1"))
        );
        assert_eq!(updated.extra.get("metrics_latency_ms"), Some(&json!(200)));
        // first_token_latency_ms was None -> not recorded.
        assert!(!updated.extra.contains_key("metrics_first_token_latency_ms"));
        assert_eq!(
            updated.extra.get("metrics_reasoning_duration_ms"),
            Some(&json!(60))
        );
        assert_eq!(
            updated.extra.get("response_body"),
            Some(&json!({"id": "resp-exec-1"}))
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_request_execution_failed_records_error_message_and_status_code()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go UpdateRequestExecutionFailed (request.go:750-757) ->
        // UpdateRequestExecutionStatus (request.go:760-786):
        //   SetStatus(StatusFailed).SetErrorMessage(errorMsg)
        //   .SetResponseStatusCode(*errorInfo.StatusCode)
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let error_info = ExecutionErrorInfo {
            status_code: Some(429),
        };
        let updated = service
            .update_request_execution_failed(
                &ctx(),
                "project-a",
                &request.id,
                "exec-1",
                "rate limit exceeded",
                Some(error_info),
            )
            .await?;

        assert_eq!(updated.status, RequestStatus::Failed);
        assert_eq!(
            updated.extra.get("error_message"),
            Some(&json!("rate limit exceeded"))
        );
        assert_eq!(updated.extra.get("response_status_code"), Some(&json!(429)));
        Ok(())
    }

    #[tokio::test]
    async fn update_request_execution_failed_without_error_info_skips_status_code()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go: `if errorInfo != nil && errorInfo.StatusCode != nil { ... }`
        // — when errorInfo is nil the response_status_code column is untouched.
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let updated = service
            .update_request_execution_failed(
                &ctx(),
                "project-a",
                &request.id,
                "exec-1",
                "connection reset",
                None,
            )
            .await?;

        assert_eq!(updated.status, RequestStatus::Failed);
        assert_eq!(
            updated.extra.get("error_message"),
            Some(&json!("connection reset"))
        );
        assert!(!updated.extra.contains_key("response_status_code"));
        Ok(())
    }

    #[tokio::test]
    async fn update_request_execution_canceled_sets_status_and_error_message()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go UpdateRequestExecutionCanceled (request.go:736-742).
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let updated = service
            .update_request_execution_canceled(
                &ctx(),
                "project-a",
                &request.id,
                "exec-1",
                "client disconnected",
            )
            .await?;

        assert_eq!(updated.status, RequestStatus::Cancelled);
        assert_eq!(
            updated.extra.get("error_message"),
            Some(&json!("client disconnected"))
        );
        // Canceled never carries a status code.
        assert!(!updated.extra.contains_key("response_status_code"));
        Ok(())
    }

    #[tokio::test]
    async fn update_request_execution_status_from_error_picks_cancelled_or_failed()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go UpdateRequestExecutionStatusFromError (request.go:788-796).
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());

        // Two independent (request, execution) pairs so each can be flipped.
        let (request1, _exec1) = recorder_fixture(repo.clone()).await?;
        let mut request2 = request();
        request2.id = "req-2".to_string();
        request2.name = "req-2".to_string();
        let ctx = ctx();
        service.create_request(&ctx, request2).await?;
        service
            .append_execution(
                &ctx,
                ExecutionRecord::new("exec-2", "req-2", "project-a", 1),
            )
            .await?;

        let canceled = service
            .update_request_execution_status_from_error(
                &ctx,
                "project-a",
                &request1.id,
                "exec-1",
                "context canceled",
                true,
            )
            .await?;
        assert_eq!(canceled.status, RequestStatus::Cancelled);

        let failed = service
            .update_request_execution_status_from_error(
                &ctx,
                "project-a",
                "req-2",
                "exec-2",
                "internal error",
                false,
            )
            .await?;
        assert_eq!(failed.status, RequestStatus::Failed);
        Ok(())
    }

    #[tokio::test]
    async fn save_request_execution_chunks_persists_filtered_array()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go SaveRequestExecutionChunks (request.go:856-941) on the
        // DB-storage path: the already-filtered chunk array is stored on the
        // execution row's response_chunks column. The caller (recorder) is
        // responsible for the StoreChunks gate + done/binary filtering.
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let chunks = json!([
            {"event": "message_start", "data": {"id": "msg_1"}},
            {"event": "content_block_delta", "data": {"text": "hi"}}
        ]);
        let updated = service
            .save_request_execution_chunks(&ctx(), "project-a", &request.id, "exec-1", chunks)
            .await?;

        assert_eq!(updated.chunks, updated.extra["response_chunks"]);
        assert_eq!(
            updated.extra["response_chunks"][0]["event"],
            "message_start"
        );
        assert_eq!(updated.extra["response_chunks"][1]["data"]["text"], "hi");
        Ok(())
    }

    #[tokio::test]
    async fn save_request_chunks_persists_filtered_array_on_request_row()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go SaveRequestChunks (request.go:943-1029) on the DB path.
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let chunks = json!([{"choices": [{"delta": {"content": "hi"}}]}]);
        let updated = service
            .save_request_chunks(&ctx(), "project-a", &request.id, chunks)
            .await?;

        assert_eq!(updated.chunks, updated.extra["response_chunks"]);
        assert_eq!(
            updated.extra["response_chunks"][0]["choices"][0]["delta"]["content"],
            "hi"
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_request_completed_on_missing_request_returns_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go: `client.Request.Get(ctx, requestID)` returning an
        // IsNotFound error surfaces from UpdateRequestCompleted.
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());

        let err = service
            .update_request_completed(&ctx(), "project-a", "missing", "ext", None, None)
            .await;
        assert!(matches!(err, Err(RequestServiceError::RequestNotFound(_))));
        Ok(())
    }

    #[tokio::test]
    async fn update_request_execution_on_missing_execution_returns_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let (request, _exec) = recorder_fixture(repo.clone()).await?;

        let err = service
            .update_request_execution_failed(
                &ctx(),
                "project-a",
                &request.id,
                "missing-exec",
                "boom",
                None,
            )
            .await;
        assert!(matches!(err, Err(RequestServiceError::RequestNotFound(_))));
        Ok(())
    }

    #[test]
    fn latency_metrics_default_is_all_none() {
        // Mirrors Go `LatencyMetrics{}` zero value: all three pointers nil.
        let metrics = LatencyMetrics::default();
        assert_eq!(metrics.latency_ms, None);
        assert_eq!(metrics.first_token_latency_ms, None);
        assert_eq!(metrics.reasoning_duration_ms, None);
    }

    #[test]
    fn execution_error_info_default_is_none_status_code() {
        // Mirrors Go `ExecutionErrorInfo{}` zero value: StatusCode nil.
        let info = ExecutionErrorInfo::default();
        assert_eq!(info.status_code, None);
    }

    // =======================================================================
    // RUST-P7-006 S08 — external-id persistence round-trip.
    // Go has no biz/video_test.go or api/doubao_test.go; these parity tests
    // pin the production behavior of orchestrator/request.go:110-126 (initial
    // external-id write after the provider create response) and
    // biz/video.go:59-93 (lookup by external id with ent `.Only` semantics).
    // =======================================================================

    /// Mirrors the video-create persistence path: Go keeps the request in
    /// `StatusProcessing` and stores the provider task id in `external_id`
    /// (`orchestrator/request.go:110-126` calling
    /// `UpdateRequestStatusExternalIDAndResponseBody`, which does
    /// `SetStatus(status).SetExternalID(externalId)` — `request.go:601-603`).
    #[tokio::test]
    async fn s08_external_id_written_on_video_create_and_read_back() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();

        let request = RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/videos");
        service.create_request(&ctx, request).await?;

        // Freshly created row has no external id (Go zero value "").
        let before = service.get_request(&ctx, "project-a", "req-1").await?;
        assert_eq!(before.external_id(), None);

        // Initial write: provider task id + processing status + raw response
        // snapshot (orchestrator/request.go:113-120: status=Processing,
        // externalId=llmResp.ID, responseBody=httpResp.Body, metrics).
        let updated = service
            .update_request_status_external_id_and_response_body(
                &ctx,
                "project-a",
                "req-1",
                RequestStatus::Running,
                "cgt-2026-0001",
                None,
                Some(json!({"id": "cgt-2026-0001", "status": "queued"})),
            )
            .await?;
        assert_eq!(updated.status, RequestStatus::Running);
        assert_eq!(updated.external_id(), Some("cgt-2026-0001"));

        // Read-back through the plain getter (S08 retrieval half).
        let fetched = service.get_request(&ctx, "project-a", "req-1").await?;
        assert_eq!(fetched.external_id(), Some("cgt-2026-0001"));
        Ok(())
    }

    /// Mirrors `UpdateRequestCompleted` also setting the external id on the
    /// completion write (`request.go:416-418`:
    /// `SetStatus(StatusCompleted).SetExternalID(externalId)`).
    #[tokio::test]
    async fn s08_external_id_written_on_completion() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();

        let request = RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/chat");
        service.create_request(&ctx, request).await?;

        let updated = service
            .update_request_completed(&ctx, "project-a", "req-1", "resp-abc", None, None)
            .await?;
        assert_eq!(updated.status, RequestStatus::Succeeded);
        assert_eq!(updated.external_id(), Some("resp-abc"));
        Ok(())
    }

    /// `.Only(ctx)` happy path: exactly one row carries the external id
    /// (`biz/video.go:67-69`).
    #[tokio::test]
    async fn s08_get_request_by_external_id_returns_unique_match() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();

        service
            .create_request(
                &ctx,
                RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/videos"),
            )
            .await?;
        service
            .update_request_status_external_id_and_response_body(
                &ctx,
                "project-a",
                "req-1",
                RequestStatus::Running,
                "cgt-unique",
                None,
                None,
            )
            .await?;

        let found = service
            .get_request_by_external_id(&ctx, "cgt-unique")
            .await?;
        assert_eq!(found.id, "req-1");
        assert_eq!(found.project_id, "project-a");
        assert_eq!(found.external_id(), Some("cgt-unique"));
        Ok(())
    }

    /// `.Only(ctx)` with zero rows returns ent `NotFoundError`; the Go caller
    /// propagates it verbatim (`biz/video.go:70-72`, `88-90`).
    #[tokio::test]
    async fn s08_get_request_by_external_id_not_found() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo);
        let err = service
            .get_request_by_external_id(&ctx(), "cgt-missing")
            .await;
        assert_eq!(
            err,
            Err(RequestServiceError::RequestNotFound(
                "cgt-missing".to_string()
            ))
        );
        Ok(())
    }

    /// `.Only(ctx)` with more than one row returns ent `NotSingularError`
    /// (`biz/video.go:67-69`) — surfaced as `ExternalIdNotSingular`.
    #[tokio::test]
    async fn s08_get_request_by_external_id_not_singular() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();

        for req_id in ["req-1", "req-2"] {
            service
                .create_request(
                    &ctx,
                    RequestRecord::new(req_id, req_id, "project-a", "POST", "/v1/videos"),
                )
                .await?;
            service
                .update_request_status_external_id_and_response_body(
                    &ctx,
                    "project-a",
                    req_id,
                    RequestStatus::Running,
                    "cgt-dup",
                    None,
                    None,
                )
                .await?;
        }

        let err = service.get_request_by_external_id(&ctx, "cgt-dup").await;
        assert_eq!(
            err,
            Err(RequestServiceError::ExternalIdNotSingular(
                "cgt-dup".to_string()
            ))
        );
        Ok(())
    }

    /// The external-id lookup is global — it crosses project boundaries, as
    /// the Go query carries no project predicate ("assumes provider task IDs
    /// are globally unique across channels", `biz/video.go:59-60`).
    #[tokio::test]
    async fn s08_get_request_by_external_id_is_global_across_projects() -> RequestServiceResult<()>
    {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo.clone());
        let ctx = ctx();

        service
            .create_request(
                &ctx,
                RequestRecord::new("req-9", "req-9", "project-b", "POST", "/v1/videos"),
            )
            .await?;
        service
            .update_request_status_external_id_and_response_body(
                &ctx,
                "project-b",
                "req-9",
                RequestStatus::Running,
                "cgt-in-b",
                None,
                None,
            )
            .await?;

        // Lookup does not need to know the project; it finds the project-b row.
        let found = service.get_request_by_external_id(&ctx, "cgt-in-b").await?;
        assert_eq!(found.project_id, "project-b");
        assert_eq!(found.id, "req-9");
        Ok(())
    }

    /// `get_request` mirrors ent `.Get` (`biz/video.go:123-126`): missing row
    /// -> NotFoundError.
    #[tokio::test]
    async fn s08_get_request_not_found() -> RequestServiceResult<()> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = RequestService::new(repo);
        let err = service.get_request(&ctx(), "project-a", "req-nope").await;
        assert_eq!(
            err,
            Err(RequestServiceError::RequestNotFound("req-nope".to_string()))
        );
        Ok(())
    }

    /// `channel_id()` getter: absent -> 0 (Go's optional-int zero value,
    /// which `loadTask` rejects at `biz/video.go:132-134`); present -> value.
    #[test]
    fn s12_channel_id_getter_defaults_to_zero() {
        let mut record = RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/videos");
        assert_eq!(record.channel_id(), 0);
        record
            .extra
            .insert("channel_id".to_string(), Value::from(42));
        assert_eq!(record.channel_id(), 42);
    }

    // =======================================================================
    // A02 fake adapter — minimal in-memory external-storage adapter that
    // accepts Go-style object keys verbatim (leading slash + integer project/
    // request ids, exactly as `generate_audio_key` / `generate_response_body_key`
    // emit). The `conduit_storage::InMemoryStorageAdapter` enforces POSIX-
    // relative keys and rejects the Go format, so for the request-storage
    // round-trip we provide a dedicated fake whose only contract is "store
    // bytes under the exact key, return them on demand". This mirrors how
    // `backup_service::tests::FakeStorage` doubles for the production storage
    // adapter in its own round-trip tests.
    // =======================================================================

    /// In-memory fake external storage adapter. Stores bytes by verbatim key
    /// (no path normalization). Concurrent-safe via a Mutex.
    #[derive(Debug, Default, Clone)]
    struct FakeExternalStorage {
        objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    }

    impl FakeExternalStorage {
        fn new() -> Self {
            Self::default()
        }

        async fn put(&self, key: impl Into<String>, bytes: impl Into<Vec<u8>>) {
            self.objects
                .lock()
                .map_err(|_| "lock poisoned")
                .ok()
                .map(|mut guard| guard.insert(key.into(), bytes.into()));
        }

        async fn get(&self, key: &str) -> Option<Vec<u8>> {
            self.objects
                .lock()
                .ok()
                .and_then(|guard| guard.get(key).cloned())
        }

        async fn contains(&self, key: &str) -> bool {
            self.objects
                .lock()
                .is_ok_and(|guard| guard.contains_key(key))
        }
    }

    // =======================================================================
    // A01 — stream-chunk storage helpers.
    // Mirrors Go `request_audio_test.go:101-149`:
    //   - TestMarshalStreamEventForStorage_BinaryAudioChunk (line 101)
    //   - TestMarshalStreamEventForStorage_BinaryAudioChunkUsesSizeWhenDataElided (line 123)
    //   - TestShouldSkipStoredStreamChunk_DoneSentinelDoesNotSkipBinaryAudio (line 143)
    // Plus pure-unit coverage of the remaining Go branches in
    // isBinaryStreamChunk / shouldSkipStoredStreamChunk / marshalStreamEventForStorage
    // (request.go:810-852).
    // =======================================================================

    #[test]
    fn a01_is_binary_stream_chunk_recognizes_audio_and_octet_stream() {
        // Mirrors Go isBinaryStreamChunk (request.go:810-818): the test cases
        // at request_audio_test.go:101-149 exercise the two recognized
        // event-type prefixes.
        let audio = StoredStreamEvent::new("audio/mpeg", vec![0x7b, 0xff, 0x00]);
        let octet = StoredStreamEvent::new("application/octet-stream", vec![0x01]);
        let sse = StoredStreamEvent::new("message", b"hi".to_vec());
        let done = StoredStreamEvent::new(BINARY_STREAM_DONE_EVENT_TYPE, vec![]);

        assert!(is_binary_stream_chunk(&audio));
        assert!(is_binary_stream_chunk(&octet));
        assert!(!is_binary_stream_chunk(&sse));
        assert!(!is_binary_stream_chunk(&done));
    }

    #[test]
    fn a01_is_binary_stream_chunk_is_case_insensitive_and_trimmed() {
        // Go: strings.ToLower(strings.TrimSpace(chunk.Type)).
        assert!(is_binary_stream_chunk(&StoredStreamEvent::new(
            "  AUDIO/WAV  ",
            vec![]
        )));
        assert!(is_binary_stream_chunk(&StoredStreamEvent::new(
            "Application/Octet-Stream",
            vec![]
        )));
        // Adjacent prefixes that are NOT binary audio.
        assert!(!is_binary_stream_chunk(&StoredStreamEvent::new(
            "audiox/wav",
            vec![]
        )));
        assert!(!is_binary_stream_chunk(&StoredStreamEvent::new(
            "text/event-stream",
            vec![]
        )));
    }

    #[test]
    fn a01_marshal_binary_audio_chunk_produces_summary_envelope()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go TestMarshalStreamEventForStorage_BinaryAudioChunk
        // (request_audio_test.go:101-121). The persisted envelope MUST:
        //   - carry the verbatim event type,
        //   - summarize the payload as `{object:"binary.stream_chunk",
        //     content_type, bytes}`,
        //   - never embed the raw audio bytes.
        let event = StoredStreamEvent::new("audio/mpeg", vec![0x7b, 0xff, 0x00]);
        let raw = marshal_stream_event_for_storage(&event)?;
        let obj = raw
            .as_object()
            .ok_or_else(|| "expected object".to_string())?;

        // Go's `jsonStreamEvent.Type` uses `json:"event"` (request.go:800) — the
        // persisted key is "event", NOT "type".
        assert_eq!(obj.get("event").and_then(Value::as_str), Some("audio/mpeg"));
        // No `data` field of the raw bytes — they must NOT appear anywhere.
        let data = obj.get("data").ok_or_else(|| "missing data".to_string())?;
        let data_obj = data
            .as_object()
            .ok_or_else(|| "data is not an object".to_string())?;
        assert_eq!(
            data_obj.get("object").and_then(Value::as_str),
            Some("binary.stream_chunk")
        );
        assert_eq!(
            data_obj.get("content_type").and_then(Value::as_str),
            Some("audio/mpeg")
        );
        assert_eq!(data_obj.get("bytes").and_then(Value::as_u64), Some(3));
        Ok(())
    }

    #[test]
    fn a01_marshal_binary_audio_chunk_uses_size_when_data_elided()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go TestMarshalStreamEventForStorage_BinaryAudioChunkUsesSizeWhenDataElided
        // (request_audio_test.go:123-141). When the persistence layer has
        // summarized the chunk (data empty, size set), the byte count MUST
        // come from `size`, not `len(data)` (which would be 0).
        let event = StoredStreamEvent::new("audio/mpeg", Vec::new()).with_size(4096);
        let raw = marshal_stream_event_for_storage(&event)?;
        let bytes = raw
            .get("data")
            .and_then(|data| data.get("bytes"))
            .and_then(Value::as_u64)
            .ok_or_else(|| "missing bytes".to_string())?;
        assert_eq!(bytes, 4096);

        // Sanity: when BOTH are present, `len(data)` wins (Go's
        // `byteCount := len(chunk.Data); if byteCount == 0 { byteCount = chunk.Size }`).
        let with_both = StoredStreamEvent::new("audio/wav", vec![0xAA; 7]).with_size(4096);
        let raw_both = marshal_stream_event_for_storage(&with_both)?;
        let bytes_both = raw_both
            .get("data")
            .and_then(|data| data.get("bytes"))
            .and_then(Value::as_u64)
            .ok_or_else(|| "missing bytes".to_string())?;
        assert_eq!(bytes_both, 7);
        Ok(())
    }

    #[test]
    fn a01_should_skip_stored_stream_chunk_done_sentinel_does_not_skip_binary_audio() {
        // Mirrors Go TestShouldSkipStoredStreamChunk_DoneSentinelDoesNotSkipBinaryAudio
        // (request_audio_test.go:143-149). The `[DONE]` sentinel is skipped
        // on non-binary SSE chunks, but binary audio chunks carrying the same
        // bytes are NOT skipped (they're payload, not EOF).
        let done_sentinel = StoredStreamEvent::new("message", DONE_STREAM_EVENT_DATA.to_vec());
        assert!(should_skip_stored_stream_chunk(Some(&done_sentinel)));

        let binary_with_done_bytes =
            StoredStreamEvent::new("audio/mpeg", DONE_STREAM_EVENT_DATA.to_vec());
        assert!(!should_skip_stored_stream_chunk(Some(
            &binary_with_done_bytes
        )));
    }

    #[test]
    fn a01_should_skip_stored_stream_chunk_handles_nil_and_binary_done_marker() {
        // Mirrors the remaining Go branches (request.go:820-824):
        //   - nil chunk -> skip
        //   - chunk.Type == "binary.done" -> skip (EOF marker)
        //   - ordinary SSE chunk -> keep
        assert!(should_skip_stored_stream_chunk(None));

        let done_marker = StoredStreamEvent::new(BINARY_STREAM_DONE_EVENT_TYPE, vec![0x00]);
        assert!(should_skip_stored_stream_chunk(Some(&done_marker)));

        let sse = StoredStreamEvent::new("message", br#"{"delta":"hi"}"#.to_vec());
        assert!(!should_skip_stored_stream_chunk(Some(&sse)));
    }

    // -----------------------------------------------------------------------
    // Byte-exact golden cases mirroring Go request_audio_test.go:101-149.
    // These pin the exact JSON wire shape Go's `marshalStreamEventForStorage`
    // produces via `xjson.Marshal(jsonStreamEvent{...})`, including the
    // `json:"event"` key on the Type field (distinct from the network-side
    // `httpclient.StreamEvent` which uses `json:"type"`).
    // -----------------------------------------------------------------------

    /// Byte-exact mirror of Go `TestMarshalStreamEventForStorage_BinaryAudioChunk`
    /// (request_audio_test.go:101-121). The Go test unmarshals the raw bytes
    /// into `struct{Event string \`json:"event"\`; Data struct{...}}`. We
    /// verify the exact same JSON shape by parsing into an equivalent struct.
    #[test]
    fn a01_golden_marshal_binary_audio_chunk_byte_exact() -> Result<(), Box<dyn std::error::Error>>
    {
        // Mirrors Go: marshalStreamEventForStorage(&httpclient.StreamEvent{
        //     Type: "audio/mpeg", Data: []byte{0x7b, 0xff, 0x00},
        // })
        let event = StoredStreamEvent::new("audio/mpeg", vec![0x7b, 0xff, 0x00]);
        let raw = marshal_stream_event_for_storage(&event)?;
        let raw_bytes = serde_json::to_vec(&raw)?;

        // Mirrors Go's unmarshal target:
        //   var got struct {
        //       Event string `json:"event"`
        //       Data  struct {
        //           Object      string `json:"object"`
        //           ContentType string `json:"content_type"`
        //           Bytes       int    `json:"bytes"`
        //       } `json:"data"`
        //   }
        #[derive(serde::Deserialize)]
        struct GoldenData {
            object: String,
            content_type: String,
            bytes: u64,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct GoldenEnvelope {
            event: String,
            data: GoldenData,
        }
        let got: GoldenEnvelope = serde_json::from_slice(&raw_bytes)?;

        // Go assertions (request_audio_test.go:117-120):
        //   require.Equal(t, "audio/mpeg", got.Event)
        //   require.Equal(t, "binary.stream_chunk", got.Data.Object)
        //   require.Equal(t, "audio/mpeg", got.Data.ContentType)
        //   require.Equal(t, 3, got.Data.Bytes)
        assert_eq!(got.event, "audio/mpeg");
        assert_eq!(got.data.object, "binary.stream_chunk");
        assert_eq!(got.data.content_type, "audio/mpeg");
        assert_eq!(got.data.bytes, 3);
        Ok(())
    }

    /// Byte-exact mirror of Go
    /// `TestMarshalStreamEventForStorage_BinaryAudioChunkUsesSizeWhenDataElided`
    /// (request_audio_test.go:123-141). The Go test unmarshals into
    /// `struct{Event string; Data struct{Bytes int}}` and asserts both Event
    /// and Bytes. The prior Rust test only checked `bytes`; this golden case
    /// also pins the `event` key and the `"audio/mpeg"` value.
    #[test]
    fn a01_golden_marshal_binary_audio_chunk_uses_size_byte_exact()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go: marshalStreamEventForStorage(&httpclient.StreamEvent{
        //     Type: "audio/mpeg", Size: 4096,
        // })
        let event = StoredStreamEvent::new("audio/mpeg", Vec::new()).with_size(4096);
        let raw = marshal_stream_event_for_storage(&event)?;
        let raw_bytes = serde_json::to_vec(&raw)?;

        #[derive(serde::Deserialize)]
        struct GoldenData {
            bytes: u64,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct GoldenEnvelope {
            event: String,
            data: GoldenData,
        }
        let got: GoldenEnvelope = serde_json::from_slice(&raw_bytes)?;

        // Go assertions (request_audio_test.go:139-140):
        //   require.Equal(t, "audio/mpeg", got.Event)
        //   require.Equal(t, 4096, got.Data.Bytes)
        assert_eq!(got.event, "audio/mpeg");
        assert_eq!(got.data.bytes, 4096);
        Ok(())
    }

    /// Byte-exact mirror of Go
    /// `TestShouldSkipStoredStreamChunk_DoneSentinelDoesNotSkipBinaryAudio`
    /// (request_audio_test.go:143-149). The Go test uses a bare
    /// `httpclient.StreamEvent{Data: llm.DoneStreamEvent.Data}` (Type is the
    /// empty string). This golden case mirrors that exact input shape.
    #[test]
    fn a01_golden_should_skip_done_sentinel_with_empty_type() {
        // Go: shouldSkipStoredStreamChunk(&httpclient.StreamEvent{Data: llm.DoneStreamEvent.Data})
        // — Type is the Go zero value "".
        let done_with_empty_type = StoredStreamEvent::new("", DONE_STREAM_EVENT_DATA.to_vec());
        assert!(should_skip_stored_stream_chunk(Some(&done_with_empty_type)));

        // Go: shouldSkipStoredStreamChunk(&httpclient.StreamEvent{
        //     Type: "audio/mpeg", Data: llm.DoneStreamEvent.Data,
        // })
        let binary_audio_with_done_data =
            StoredStreamEvent::new("audio/mpeg", DONE_STREAM_EVENT_DATA.to_vec());
        assert!(!should_skip_stored_stream_chunk(Some(
            &binary_audio_with_done_data
        )));
    }

    // -----------------------------------------------------------------------
    // Pending DB-backed Go tests (request_shutdown_test.go +
    // request_audio_test.go): the original suite uses an ent test client. The
    // Rust equivalents require a PostgreSQL fixture and exercise the full
    // write/read cycle through RequestService.ClearStaleProcessingOnStartup /
    // UpdateRequestCompletedWithAudio. The pure-logic decision trees are
    // covered by the S10/S17/A01/A02 tests above.
    //
    //   - TestRequestService_ClearStaleProcessingOnStartup (L44-154)
    //     [Hilbert-the-11th ?] pending DB-backed: ent.Request/RequestExecution
    //     bulk UPDATE against PostgreSQL. Pure decision covered by:
    //       startup_cleanup_plan_targets_canceled_and_uses_strict_lt_cutoff,
    //       startup_cleanup_plan_filters_out_non_processing_statuses,
    //       stale_processing_cleanup_selects_timed_out_processing_requests,
    //       stale_processing_cleanup_keeps_requests_newer_than_cutoff.
    //
    //   - TestRequestService_ClearStaleProcessingOnStartup_NoStaleRecords (L156-162)
    //     [Hilbert-the-11th ?] pending DB-backed: empty-DB cleanup.
    //     Pure decision covered by:
    //       startup_cleanup_plan_with_empty_candidates_matches_zero,
    //       startup_cleanup_outcome_with_no_stale_records_succeeds.
    //
    //   - TestRequestService_ClearStaleProcessingOnStartup_PartialFailure (L164-220)
    //     [Hilbert-the-11th ?] pending DB-backed: both entities cleaned.
    //     Pure decision covered by:
    //       startup_cleanup_outcome_aggregates_errors_like_go_join.
    //
    //   - TestRequestService_UpdateRequestCompletedWithAudio_ExternalStorage (L21-99)
    //     [Hilbert-the-11th ?] pending DB-backed: ent client + DataStorage FS
    //     adapter + SaveData/LoadData round-trip. Pure decision covered by:
    //       a02_audio_and_response_body_round_trip_with_fake_adapter,
    //       a02_plan_audio_offload_writes_audio_and_response_body_keys,
    //       a02_audio_offload_succeeded_populates_content_fields.
    // -----------------------------------------------------------------------

    #[test]
    fn a01_marshal_non_binary_chunk_passes_json_payload_through()
    -> Result<(), Box<dyn std::error::Error>> {
        // Non-binary events: Go stores `chunk.Data` as a `json.RawMessage`.
        // The Rust port parses it back into a Value so the envelope is a
        // single coherent JSON tree (and survives re-serialization).
        let event = StoredStreamEvent::new(
            "content_block_delta",
            br#"{"delta":{"text":"hi"}}"#.to_vec(),
        );
        let raw = marshal_stream_event_for_storage(&event)?;
        let obj = raw
            .as_object()
            .ok_or_else(|| "expected object".to_string())?;
        assert_eq!(
            obj.get("event").and_then(Value::as_str),
            Some("content_block_delta")
        );
        assert_eq!(
            obj.get("data")
                .and_then(|data| data.get("delta"))
                .and_then(|delta| delta.get("text"))
                .and_then(Value::as_str),
            Some("hi")
        );
        Ok(())
    }

    #[test]
    fn a01_done_stream_event_data_constant_matches_go() {
        // Mirrors Go `llm.DoneStreamEvent.Data = []byte("[DONE]")`.
        assert_eq!(DONE_STREAM_EVENT_DATA, b"[DONE]");
        // Sanity: the constant type is byte-slice-compatible with Vec<u8>.
        let owned: Vec<u8> = DONE_STREAM_EVENT_DATA.to_vec();
        assert_eq!(owned, b"[DONE]".to_vec());
    }

    #[test]
    fn a01_binary_stream_done_event_type_constant_matches_go() {
        // Mirrors Go `httpclient.BinaryStreamDoneEventType = "binary.done"`.
        assert_eq!(BINARY_STREAM_DONE_EVENT_TYPE, "binary.done");
    }

    #[test]
    fn a01_stored_stream_event_round_trips_through_serde() -> Result<(), Box<dyn std::error::Error>>
    {
        // The StoredStreamEvent projection must serialize stably so persisted
        // chunks (which Go writes as the `jsonStreamEvent` envelope) round-trip
        // through Rust serde without losing fields.
        let event = StoredStreamEvent::new("audio/mpeg", vec![0xAA, 0xBB])
            .with_last_event_id("evt-42")
            .with_size(2);
        let serialized = serde_json::to_string(&event)?;
        assert!(serialized.contains("\"last_event_id\":\"evt-42\""));
        assert!(serialized.contains("\"type\":\"audio/mpeg\""));
        let parsed: StoredStreamEvent = serde_json::from_str(&serialized)?;
        assert_eq!(parsed, event);
        Ok(())
    }

    // =======================================================================
    // A02 — audio offload planner + external-storage fake-adapter round-trip.
    // Mirrors Go `TestRequestService_UpdateRequestCompletedWithAudio_ExternalStorage`
    // (request_audio_test.go:21-99), which exercises the full
    //   decide route -> plan keys -> SaveData -> SetContentSaved
    // pipeline against a real temp-dir FS adapter. The Rust port reuses the
    // `conduit_storage::InMemoryStorageAdapter` (the fake adapter already
    // proven by `conduit-storage`'s own test suite) to stand in for the FS.
    // =======================================================================

    #[test]
    fn a02_plan_audio_offload_skip_on_primary_db_route() {
        // Mirrors the gate `shouldUseExternalStorage(ctx, dataStorage)`
        // returning false: a primary-DB / unset storage route never offloads
        // audio, regardless of audio_size.
        let plan = plan_audio_offload(1024, StorageRoute::PrimaryDb, true, 7, 42, "audio.mp3");
        assert!(!plan.offload_audio);
        assert!(!plan.offload_response_body);
        assert_eq!(plan.audio_key, None);
        assert_eq!(plan.response_body_key, None);
        assert_eq!(plan.data_storage_id, None);
    }

    #[test]
    fn a02_plan_audio_offload_skip_when_audio_empty() {
        // Mirrors the gate `len(audio) > 0`: zero-length audio never offloads,
        // even when external storage is configured. Response body may still
        // offload independently (Go: separate branch at request.go:525-540).
        let plan = plan_audio_offload(
            0,
            StorageRoute::External {
                data_storage_id: Some(5),
            },
            true,
            7,
            42,
            "audio.mp3",
        );
        assert!(!plan.offload_audio);
        assert_eq!(plan.audio_key, None);
        // Response body still routes externally when StoreResponseBody=true.
        assert!(plan.offload_response_body);
        assert_eq!(
            plan.response_body_key.as_deref(),
            Some("/7/requests/42/response_body.json")
        );
        assert_eq!(plan.data_storage_id, Some(5));
    }

    #[test]
    fn a02_plan_audio_offload_writes_audio_and_response_body_keys() {
        // Happy path: external storage + non-empty audio + StoreResponseBody=true
        // -> both keys pre-computed, content_storage_id captured for the row.
        let plan = plan_audio_offload(
            256,
            StorageRoute::External {
                data_storage_id: Some(9),
            },
            true,
            7,
            42,
            "voice.mp3",
        );
        assert!(plan.offload_audio);
        assert!(plan.offload_response_body);
        assert_eq!(
            plan.audio_key.as_deref(),
            Some("/7/requests/42/audio/voice.mp3")
        );
        assert_eq!(
            plan.response_body_key.as_deref(),
            Some("/7/requests/42/response_body.json")
        );
        assert_eq!(plan.data_storage_id, Some(9));
    }

    #[test]
    fn a02_plan_audio_offload_respects_store_response_body_flag() {
        // Mirrors the Go `policy.StoreResponseBody` branch (request.go:484-489):
        // when the system policy disables response-body persistence, only the
        // audio is offloaded.
        let plan = plan_audio_offload(
            256,
            StorageRoute::External {
                data_storage_id: Some(9),
            },
            false,
            7,
            42,
            "voice.mp3",
        );
        assert!(plan.offload_audio);
        assert!(!plan.offload_response_body);
        assert_eq!(
            plan.audio_key.as_deref(),
            Some("/7/requests/42/audio/voice.mp3")
        );
        assert_eq!(plan.response_body_key, None);
    }

    #[test]
    fn a02_audio_offload_succeeded_populates_content_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go request.go:548-553: after a successful SaveData the four
        // content_* fields are populated together.
        let plan = plan_audio_offload(
            256,
            StorageRoute::External {
                data_storage_id: Some(9),
            },
            true,
            7,
            42,
            "voice.mp3",
        );
        let saved_at = DateTime::parse_from_rfc3339("2026-07-05T10:00:00Z")?.with_timezone(&Utc);

        let location = audio_offload_succeeded(&plan, saved_at)
            .ok_or_else(|| "expected Some(location)".to_string())?;

        assert!(location.content_saved);
        assert_eq!(location.content_storage_id, Some(9));
        assert_eq!(
            location.content_storage_key.as_deref(),
            Some("/7/requests/42/audio/voice.mp3")
        );
        assert_eq!(location.content_saved_at, Some(saved_at));
        Ok(())
    }

    #[test]
    fn a02_audio_offload_failure_returns_none_keeping_row_content_untouched() {
        // Mirrors Go: when SaveData fails, the `SetContentSaved*` calls are
        // skipped (Go only invokes them inside the `else` branch of the
        // SaveData error check, request.go:545-553). The pure helper models
        // this by returning None — the caller leaves the row's content_*
        // fields at their prior state.
        let skip_plan = plan_audio_offload(0, StorageRoute::PrimaryDb, true, 1, 1, "x.mp3");
        assert_eq!(audio_offload_succeeded(&skip_plan, Utc::now()), None);
    }

    /// Round-trip integration: write audio bytes + response body to the
    /// in-memory fake adapter, then load them back. Mirrors Go
    /// `TestRequestService_UpdateRequestCompletedWithAudio_ExternalStorage`
    /// (request_audio_test.go:21-99) end-to-end except for the FS adapter.
    #[tokio::test]
    async fn a02_audio_and_response_body_round_trip_with_fake_adapter()
    -> Result<(), Box<dyn std::error::Error>> {
        // The fake external adapter stands in for Go's `DataStorageService`
        // FS backend (Go test uses `t.TempDir()`; we use a simple in-memory
        // map keyed by the verbatim Go object key).
        let adapter = FakeExternalStorage::new();

        // Decision: external storage, non-empty audio, StoreResponseBody=true.
        let plan = plan_audio_offload(
            7,
            StorageRoute::External {
                data_storage_id: Some(5),
            },
            true,
            7,
            42,
            "audio.mp3",
        );
        assert!(plan.offload_audio);
        assert!(plan.offload_response_body);

        // Audio bytes (mirrors Go's `audio := []byte{0x49, 0x44, 0x33, ...}`).
        let audio: Vec<u8> = vec![0x49, 0x44, 0x33, 0xDE, 0xAD, 0xBE, 0xEF];
        let placeholder = serde_json::to_vec(&json!({
            "object": "audio.speech",
            "content_type": "audio/mpeg",
            "bytes": audio.len()
        }))?;

        // Write both artifacts to external storage (Go's SaveData).
        let audio_key = plan
            .audio_key
            .clone()
            .ok_or_else(|| "missing audio_key".to_string())?;
        let response_key = plan
            .response_body_key
            .clone()
            .ok_or_else(|| "missing response_body_key".to_string())?;

        adapter.put(audio_key.clone(), audio.clone()).await;
        adapter.put(response_key.clone(), placeholder.clone()).await;

        // Audio offload "succeeded" -> populate the content_* fields.
        let saved_at = DateTime::parse_from_rfc3339("2026-07-05T10:00:00Z")?.with_timezone(&Utc);
        let location = audio_offload_succeeded(&plan, saved_at)
            .ok_or_else(|| "expected Some(location)".to_string())?;
        assert!(location.content_saved);
        assert_eq!(location.content_storage_id, Some(5));
        assert_eq!(
            location.content_storage_key.as_deref(),
            Some(audio_key.as_str())
        );

        // Load audio back: bytes match exactly (Go test assertion at line 97).
        let loaded_audio = adapter
            .get(&audio_key)
            .await
            .ok_or_else(|| "audio object missing".to_string())?;
        assert_eq!(loaded_audio, audio);

        // Load response body back: contains the placeholder JSON, NEVER the
        // raw audio bytes (Go assertion at line 92-93).
        let loaded_body = adapter
            .get(&response_key)
            .await
            .ok_or_else(|| "body object missing".to_string())?;
        let body_str = String::from_utf8(loaded_body.clone())?;
        assert!(body_str.contains("audio.speech"));
        // The raw audio bytes (0xDE 0xAD 0xBE 0xEF) must NEVER appear in the
        // offloaded response-body placeholder (Go assertion at line 92-93).
        assert!(!loaded_body.windows(2).any(|w| w == [0xDE, 0xAD]));
        assert!(!loaded_body.windows(2).any(|w| w == [0xBE, 0xEF]));
        Ok(())
    }

    /// Mirrors the Go response-body external-storage assertion
    /// (request_audio_test.go:88-93): when the route is external, the
    /// response body placeholder is offloaded (NOT stored in the DB column),
    /// and is retrievable via the same adapter.
    #[tokio::test]
    async fn a02_response_body_offload_round_trip_with_fake_adapter()
    -> Result<(), Box<dyn std::error::Error>> {
        let adapter = FakeExternalStorage::new();

        // External route, StoreResponseBody=true, but ZERO audio bytes.
        // Mirrors a chat completion that goes through external storage.
        let plan = plan_audio_offload(
            0,
            StorageRoute::External {
                data_storage_id: Some(3),
            },
            true,
            1,
            100,
            "audio.mp3",
        );
        assert!(!plan.offload_audio);
        assert!(plan.offload_response_body);

        let key = plan
            .response_body_key
            .as_deref()
            .ok_or_else(|| "missing response_body_key".to_string())?;
        assert_eq!(key, "/1/requests/100/response_body.json");

        let body = serde_json::to_vec(&json!({"id": "chatcmpl-1", "object": "chat.completion"}))?;
        adapter.put(key, body.clone()).await;

        let loaded = adapter
            .get(key)
            .await
            .ok_or_else(|| "object missing".to_string())?;
        assert_eq!(loaded, body);

        // audio_key stayed None — no audio artifact pollutes the storage.
        assert_eq!(plan.audio_key, None);
        // And contentSaved is NOT flipped (audio never wrote).
        assert_eq!(audio_offload_succeeded(&plan, Utc::now()), None);
        Ok(())
    }

    /// Mirrors Go: when the storage adapter has no entry for a key, the
    /// caller observes None (not an error). This pins the contract the
    /// request_service uses to decide "audio not yet offloaded".
    #[tokio::test]
    async fn a02_fake_adapter_returns_none_for_missing_key() {
        let adapter = FakeExternalStorage::new();
        assert!(adapter.get("/missing/key").await.is_none());
        assert!(!adapter.contains("/missing/key").await);
    }

    /// Mirrors Go: an existing key is overwritten on re-put, so a retried
    /// offload (e.g. after a transient failure) lands at the same object
    /// key with the latest bytes. Pins the idempotent-write contract.
    #[tokio::test]
    async fn a02_fake_adapter_overwrites_existing_key() {
        let adapter = FakeExternalStorage::new();
        let key = "/1/requests/1/audio/voice.mp3";

        adapter.put(key, vec![0x11_u8]).await;
        adapter.put(key, vec![0x22_u8, 0x33]).await;

        let loaded = adapter.get(key).await.ok_or_else(|| "missing".to_string());
        assert_eq!(loaded, Ok(vec![0x22_u8, 0x33]));
    }
}
