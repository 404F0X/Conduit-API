//! Request live-preview endpoint (RUST-P11-001 MAP-02 + RUST-P10-001 S12).
//!
//! Ports `conduit/internal/server/api/request_live.go`
//! (`RequestPreviewHandlers.PreviewRequest`):
//!
//! | method | path                                   | Go handler |
//! |--------|----------------------------------------|------------|
//! | GET    | `/admin/requests/{request_id}/preview` | `PreviewRequest` (routes.go:135-139) |
//!
//! Protocol (Go `RequestDetailSSEContract`, request_live.go:66-87): while a
//! streaming request is `processing`, the endpoint serves
//! `text/event-stream` — first replaying buffered chunks as `preview.replay`
//! events, then delivering new chunks as `preview.chunk`, and finally a single
//! `preview.completed` event carrying `{"status":"completed"}`. The terminal
//! `[DONE]` chunk is omitted. When the request is not live (or the in-memory
//! buffer is gone) it falls back to a one-shot JSON "static-fetch" payload.
//!
//! Wire format: Go writes events through `WriteSSEStream` (api/chat.go:122-173)
//! backed by the looplj/sse fork of gin-contrib/sse (`conduit/go.mod:93-94`,
//! "add space after data:"), i.e. `event:<name>\n` + `data: <bytes>\n\n` with
//! Content-Type `text/event-stream;charset=utf-8`, `Cache-Control: no-cache`,
//! `Connection: keep-alive`. [`sse_event_frame`] reproduces those bytes.
//!
//! Live infra: Go reads from `chunkbuffer.Buffer`
//! (internal/pkg/chunkbuffer/chunkbuffer.go) registered in
//! `biz.LiveStreamRegistry` (biz/stream_preview.go). Neither is ported as a
//! shared crate yet, so this module defines the minimal consumer surface
//! ([`PreviewChunkBuffer`]) plus [`InMemoryPreviewChunkBuffer`], a faithful
//! port of the Go buffer's Append/Close/Read/SubscribeFromCurrent semantics
//! that the host (and the orchestrator's live-preview middleware, once wired)
//! can register per in-flight request.
//!
//! The route is protected by the shared JWT middleware. Project selection is
//! resolved through `request_content_handlers::resolve_project_id`, then the
//! handler verifies both the caller's project scope and the request row's
//! project ownership before returning preview data.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;
use tokio::sync::mpsc;

use crate::api_error::json_error;
use crate::app_state::AppState;
use crate::middleware::{AuthRequestContextExtension, caller_can_read_requests};
use crate::request_content_handlers::{
    DownloadContentRequest, parse_request_id_param, project_id_rejection_response,
    resolve_project_id,
};

/// looplj/sse `ContentType` (sse-encoder.go): note the missing space before
/// `charset` — the frontend contract sees this exact value.
pub const SSE_CONTENT_TYPE: &str = "text/event-stream;charset=utf-8";

/// Go `previewIdleTimeout = 3 * time.Minute` (request_live.go:189): a live
/// stream with no buffer activity for this long is terminated.
pub const PREVIEW_IDLE_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// Event names (request_live.go:82,211-213,227).
pub const PREVIEW_REPLAY_EVENT: &str = "preview.replay";
pub const PREVIEW_CHUNK_EVENT: &str = "preview.chunk";
pub const PREVIEW_COMPLETED_EVENT: &str = "preview.completed";

/// `llm.DoneStreamEvent.Data` (llm/model.go:14-16) — the terminal chunk the
/// preview stream omits (request_live.go:281-283).
pub const DONE_STREAM_EVENT_DATA: &[u8] = b"[DONE]";

/// `previewCompletedEventData` (request_live.go:271):
/// `json.Marshal(gin.H{"status": "completed"})`.
pub const PREVIEW_COMPLETED_EVENT_DATA: &[u8] = b"{\"status\":\"completed\"}";

/// ent `request.StatusProcessing` (ent/request/request.go:242).
pub const REQUEST_STATUS_PROCESSING: &str = "processing";

/// Go `isPreviewTerminalChunk` (request_live.go:281-283):
/// `bytes.Equal(chunk.Data, llm.DoneStreamEvent.Data)`.
pub fn is_preview_terminal_chunk(data: &[u8]) -> bool {
    data == DONE_STREAM_EVENT_DATA
}

/// Encode one SSE frame exactly as the looplj/sse encoder does for gin's
/// `c.SSEvent(name, []byte)` (sse-encoder.go `writeEvent` + `writeData`):
///
/// * `event:<name>\n` — no space, `\n`→`\\n`, `\r`→`\\r` in the name;
/// * `data: <bytes>\n\n` — ONE space after the colon, payload `\n`→
///   `"\ndata: "`, `\r`→`\\r`.
pub fn sse_event_frame(event: &str, data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(event.len() + data.len() + 16);
    if !event.is_empty() {
        frame.extend_from_slice(b"event:");
        for ch in event.bytes() {
            match ch {
                b'\n' => frame.extend_from_slice(b"\\n"),
                b'\r' => frame.extend_from_slice(b"\\r"),
                other => frame.push(other),
            }
        }
        frame.push(b'\n');
    }
    frame.extend_from_slice(b"data: ");
    for ch in data {
        match ch {
            b'\n' => frame.extend_from_slice(b"\ndata: "),
            b'\r' => frame.extend_from_slice(b"\\r"),
            other => frame.push(*other),
        }
    }
    frame.extend_from_slice(b"\n\n");
    frame
}

// ---- fallback + contract shapes ---------------------------------------------

/// Go `RequestPreviewFallbackResponse` (request_live.go:40-43):
///
/// ```text
/// Mode           string                   `json:"mode"`
/// ResponseChunks []objects.JSONRawMessage `json:"responseChunks"`
/// ```
///
/// `responseChunks` has no `omitempty`: a Go nil slice marshals to `null`,
/// an empty one to `[]` — modelled as `Option<Vec<Value>>` with no
/// `skip_serializing_if`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPreviewFallbackResponse {
    pub mode: String,
    pub response_chunks: Option<Vec<Value>>,
}

/// Go `RequestDetailPreviewContract` (request_live.go:45-64) — the pure
/// compile-time contract descriptor for the SSE preview surface. Not
/// serialized (the Go struct has no json tags); ported for parity so the
/// contract values stay greppable on the Rust side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestDetailPreviewContract {
    pub single_instance_only: bool,
    pub supports_distributed_replay: bool,
    pub allows_database_schema_changes: bool,
    pub execution_level_preview: bool,
    pub event_order: Vec<String>,
    pub scope: String,
    pub reuse_in_memory_chunk_buffer: bool,
    pub final_batch_persistence_unchanged: bool,
    pub fallback_mode: String,
    pub fallback_behavior: String,
    pub fallback_uses_execution_preview: bool,
    pub fallback_starts_secondary_live_polling_loop: bool,
    pub endpoint_path: String,
    pub content_type: String,
    pub event_types: Vec<String>,
    pub replay_omits_terminal_done_event: bool,
    pub incremental_omits_terminal_done_event: bool,
    pub connect_after_completion_falls_back_to_static_fetch: bool,
}

/// Go `RequestDetailSSEContract()` (request_live.go:66-87), field for field.
pub fn request_detail_sse_contract() -> RequestDetailPreviewContract {
    RequestDetailPreviewContract {
        single_instance_only: true,
        supports_distributed_replay: false,
        allows_database_schema_changes: false,
        execution_level_preview: false,
        event_order: vec!["replay".to_string(), "incremental".to_string()],
        scope: "request".to_string(),
        reuse_in_memory_chunk_buffer: true,
        final_batch_persistence_unchanged: true,
        fallback_mode: "static-fetch".to_string(),
        fallback_behavior: "load persisted request detail once when SSE cannot connect".to_string(),
        fallback_uses_execution_preview: false,
        fallback_starts_secondary_live_polling_loop: false,
        endpoint_path: "/admin/requests/:request_id/preview".to_string(),
        content_type: "text/event-stream".to_string(),
        event_types: vec![
            PREVIEW_REPLAY_EVENT.to_string(),
            PREVIEW_CHUNK_EVENT.to_string(),
            PREVIEW_COMPLETED_EVENT.to_string(),
        ],
        replay_omits_terminal_done_event: true,
        incremental_omits_terminal_done_event: true,
        connect_after_completion_falls_back_to_static_fetch: true,
    }
}

// ---- live buffer surface ------------------------------------------------------

/// One `Buffer.Read(index)` outcome (chunkbuffer.go:94-103):
/// `(chunk, nextIndex, closed, ok)` — `chunk: Some` ⇔ Go `ok=true` (Go never
/// stores nil chunks: `Append` rejects them, chunkbuffer.go:39-42).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewBufferRead {
    pub chunk: Option<Vec<u8>>,
    pub next_index: usize,
    pub closed: bool,
}

/// One `Buffer.SubscribeFromCurrent()` registration (chunkbuffer.go:127-152):
/// `(<-chan struct{}, replayUntil, unsubscribe)`. The Go cap-1 non-blocking
/// notification channel maps onto [`tokio::sync::Notify`], whose stored
/// permit has identical coalescing semantics.
pub struct PreviewSubscription {
    pub notify: Arc<Notify>,
    pub replay_until: usize,
    unsubscribe: Option<Box<dyn FnOnce() + Send>>,
}

impl PreviewSubscription {
    pub fn new(
        notify: Arc<Notify>,
        replay_until: usize,
        unsubscribe: Option<Box<dyn FnOnce() + Send>>,
    ) -> Self {
        Self {
            notify,
            replay_until,
            unsubscribe,
        }
    }

    /// Go's `unsubscribe func()`; the preview stream defers it via
    /// `stream.Close()` (request_live.go:139,263-269). Idempotent.
    pub fn unsubscribe(&mut self) {
        if let Some(unsubscribe) = self.unsubscribe.take() {
            unsubscribe();
        }
    }
}

impl Drop for PreviewSubscription {
    fn drop(&mut self) {
        self.unsubscribe();
    }
}

/// Read-side surface of Go `chunkbuffer.Buffer` consumed by the preview
/// stream (request_live.go:178-252). Implemented by
/// [`InMemoryPreviewChunkBuffer`]; the host may bridge other buffer owners.
pub trait PreviewChunkBuffer: Send + Sync {
    /// `Buffer.Read(index)` (chunkbuffer.go:94-103).
    fn read(&self, index: usize) -> PreviewBufferRead;
    /// `Buffer.SubscribeFromCurrent()` (chunkbuffer.go:127-152).
    fn subscribe_from_current(&self) -> PreviewSubscription;
}

// ---- in-memory buffer port ----------------------------------------------------

/// Go `maxChunkCapacity = 50000` (chunkbuffer.go:25).
pub const MAX_CHUNK_CAPACITY: usize = 50000;

struct BufferState {
    chunks: Vec<Vec<u8>>,
    closed: bool,
    subscribers: std::collections::HashMap<u64, Arc<Notify>>,
    next_subscriber_id: u64,
}

/// Port of `chunkbuffer.Buffer` (internal/pkg/chunkbuffer/chunkbuffer.go) —
/// the thread-safe chunk accumulator shared between the streaming writer and
/// live preview readers. Only the surface the preview endpoint and its tests
/// exercise is ported: `Append`, `Close`, `IsClosed`, `Len`, `Read`,
/// `SubscribeFromCurrent`. Chunks carry the `StreamEvent.Data` payload bytes
/// (the preview stream never consults `StreamEvent.Type`,
/// request_live.go:207-219).
///
/// Lives here until the shared live-broadcast infra crate lands; the host
/// registers one per in-flight streaming request (Go: orchestrator
/// `live_streaming.go` → `LiveStreamRegistry.RegisterRequest`).
pub struct InMemoryPreviewChunkBuffer {
    state: Arc<Mutex<BufferState>>,
}

impl InMemoryPreviewChunkBuffer {
    /// `chunkbuffer.New()` (chunkbuffer.go:28-34).
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(BufferState {
                chunks: Vec::new(),
                closed: false,
                subscribers: std::collections::HashMap::new(),
                next_subscriber_id: 0,
            })),
        }
    }

    /// Recover from a poisoned lock instead of panicking (workspace forbids
    /// unwrap/expect; the buffer state stays consistent because every write
    /// is a single push/flag flip).
    fn lock(state: &Arc<Mutex<BufferState>>) -> std::sync::MutexGuard<'_, BufferState> {
        match state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// `Buffer.Append` (chunkbuffer.go:39-61): rejected when closed or at
    /// capacity; otherwise stores the chunk and notifies every subscriber
    /// non-blockingly (`broadcastLocked`, chunkbuffer.go:154-161). The Go
    /// nil-chunk rejection is unrepresentable here (`Vec<u8>` is never nil).
    pub fn append(&self, chunk: Vec<u8>) -> bool {
        let mut state = Self::lock(&self.state);
        if state.closed || state.chunks.len() >= MAX_CHUNK_CAPACITY {
            return false;
        }
        state.chunks.push(chunk);
        for notify in state.subscribers.values() {
            // Notify stores at most one permit — identical to Go's cap-1
            // non-blocking channel send.
            notify.notify_one();
        }
        true
    }

    /// `Buffer.Close` (chunkbuffer.go:112-118).
    pub fn close(&self) {
        let mut state = Self::lock(&self.state);
        state.closed = true;
        for notify in state.subscribers.values() {
            notify.notify_one();
        }
    }

    /// `Buffer.IsClosed` (chunkbuffer.go:120-125).
    pub fn is_closed(&self) -> bool {
        Self::lock(&self.state).closed
    }

    /// `Buffer.Len` (chunkbuffer.go:73-78).
    pub fn len(&self) -> usize {
        Self::lock(&self.state).chunks.len()
    }

    /// Companion to [`Self::len`] (clippy `len_without_is_empty`).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for InMemoryPreviewChunkBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewChunkBuffer for InMemoryPreviewChunkBuffer {
    fn read(&self, index: usize) -> PreviewBufferRead {
        let state = Self::lock(&self.state);
        // chunkbuffer.go:98-102: out-of-range → (nil, index, closed, false);
        // otherwise (chunk, index+1, closed, true).
        if index >= state.chunks.len() {
            return PreviewBufferRead {
                chunk: None,
                next_index: index,
                closed: state.closed,
            };
        }
        PreviewBufferRead {
            chunk: Some(state.chunks[index].clone()),
            next_index: index + 1,
            closed: state.closed,
        }
    }

    fn subscribe_from_current(&self) -> PreviewSubscription {
        let mut state = Self::lock(&self.state);
        let replay_until = state.chunks.len();
        let notify = Arc::new(Notify::new());
        if state.closed {
            // chunkbuffer.go:136-143: closed buffers pre-load one token and
            // register nothing (no-op unsubscribe).
            notify.notify_one();
            return PreviewSubscription::new(notify, replay_until, None);
        }
        let id = state.next_subscriber_id;
        state.next_subscriber_id += 1;
        state.subscribers.insert(id, Arc::clone(&notify));
        let unsubscribe_state = Arc::clone(&self.state);
        let unsubscribe = Box::new(move || {
            let mut state = Self::lock(&unsubscribe_state);
            state.subscribers.remove(&id);
        });
        PreviewSubscription::new(notify, replay_until, Some(unsubscribe))
    }
}

// ---- service trait + handler --------------------------------------------------

/// Projection of the `ent.Request` row consumed by `PreviewRequest`
/// (request_live.go:112-136,148-163).
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewRequestRow {
    /// Go `req.ID`.
    pub id: i64,
    /// Go `req.ProjectID`.
    pub project_id: i64,
    /// Go `req.Status` (ent enum string, e.g. [`REQUEST_STATUS_PROCESSING`]).
    pub status: String,
    /// Go `req.Stream`.
    pub stream: bool,
    /// Go `req.ResponseChunks` (`[]objects.JSONRawMessage`; `None` mirrors a
    /// nil slice, which marshals to `null` in the fallback payload).
    pub response_chunks: Option<Vec<Value>>,
}

/// Minimal service trait behind the preview endpoint. Stands in for the fx
/// pair `*biz.RequestService` + `*biz.LiveStreamRegistry`
/// (request_live.go:27-38) plus the request-scoped ent client.
#[async_trait::async_trait]
pub trait RequestPreviewService: Send + Sync {
    /// `ent.FromContext(ctx).Request.Get(ctx, id)` (request_live.go:112-120):
    /// `Ok(None)` is `ent.IsNotFound` → 404 "Request not found"; `Err` → 500
    /// "Failed to load request".
    async fn get_request(&self, request_id: i64) -> Result<Option<PreviewRequestRow>, String>;

    /// `RequestService.LoadResponseChunks(ctx, req)` (request_live.go:151,
    /// biz/request.go:1217-1258). `Ok(None)` mirrors a Go nil slice (live
    /// registry miss), `Ok(Some(vec![]))` the empty non-nil slice.
    async fn load_response_chunks(
        &self,
        request: &PreviewRequestRow,
    ) -> Result<Option<Vec<Value>>, String>;

    /// `LiveStreamRegistry.GetRequestBuffer(req.ID)`
    /// (request_live.go:132, biz/stream_preview.go:42-54).
    fn get_request_buffer(&self, request_id: i64) -> Option<Arc<dyn PreviewChunkBuffer>>;
}

/// `GET /admin/requests/{request_id}/preview` — Go
/// `RequestPreviewHandlers.PreviewRequest` (request_live.go:97-146).
///
/// Response table (verbatim Go):
///
/// | condition                                | status | body |
/// |------------------------------------------|--------|------|
/// | project id missing from context          | 400    | `Project ID not found in context` (100-104) |
/// | invalid `X-Project-ID` GUID              | 400    | `Invalid project ID` (middleware) |
/// | non-integer `request_id`                 | 400    | `Invalid request body: <strconv err>` (106-110) |
/// | request row not found / project mismatch | 404    | `Request not found` (113-125) |
/// | request row load failure                 | 500    | `Failed to load request` (118) |
/// | not processing / not streaming / no buffer | 200  | JSON `{"mode":"static-fetch","responseChunks":...}` (127-136,148-163) |
/// | chunk-load failure in fallback           | 500    | `Failed to load request preview` (151-156) |
/// | live buffer registered                   | 200    | `text/event-stream` replay → incremental → completed (138-146) |
pub async fn preview_request(
    State(state): State<AppState>,
    auth: Option<axum::Extension<AuthRequestContextExtension>>,
    Path(request_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    // P-22: require owner / read_requests (see download_request_content). A
    // client-supplied X-Project-ID alone must not grant a cross-project preview.
    if !caller_can_read_requests(auth.as_ref().map(|ext| &ext.0)) {
        return json_error(StatusCode::NOT_FOUND, "Request not found");
    }

    // request_live.go:100-104 (+ middleware/project.go).
    let project_id = match resolve_project_id(&headers) {
        Ok(id) => id,
        Err(rejection) => return project_id_rejection_response(rejection),
    };

    // request_live.go:106-110 — binds the same DownloadContentRequest as the
    // content endpoint.
    let uri = match parse_request_id_param(&request_id) {
        Ok(request_id) => DownloadContentRequest { request_id },
        Err(err) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                format!("Invalid request body: {err}"),
            );
        }
    };

    let Some(service) = state.services().request_preview_service() else {
        // Unwired service (Rust-only state) degrades to the row-load 500.
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load request");
    };

    // request_live.go:112-120.
    let req = match service.get_request(uri.request_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return json_error(StatusCode::NOT_FOUND, "Request not found"),
        Err(_) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load request");
        }
    };

    // request_live.go:122-125.
    if req.project_id != project_id {
        return json_error(StatusCode::NOT_FOUND, "Request not found");
    }

    // request_live.go:127-130 — only live streaming requests get SSE.
    if req.status != REQUEST_STATUS_PROCESSING || !req.stream {
        return write_static_preview(service.as_ref(), &req).await;
    }

    // request_live.go:132-136 — no registered buffer → static fallback.
    let Some(buffer) = service.get_request_buffer(req.id) else {
        return write_static_preview(service.as_ref(), &req).await;
    };

    // request_live.go:138-146 + WriteSSEStream (chat.go:141-145): subscribe
    // synchronously (newRequestPreviewStream, request_live.go:178-187 — the
    // replay cutoff is snapshotted before the response returns), commit the
    // SSE headers, then stream events from the buffer.
    let subscription = buffer.subscribe_from_current();
    let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, Infallible>>();
    tokio::spawn(run_preview_stream(buffer, subscription, tx));

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, SSE_CONTENT_TYPE),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONNECTION, "keep-alive"),
        ],
        Body::from_stream(SseFrameStream { rx }),
    )
        .into_response()
}

// ---- static fallback + stream loop --------------------------------------------

/// Go `writeStaticPreview` (request_live.go:148-163): prefer the chunks
/// already on the row; when nil/empty, load them through the service
/// (LoadResponseChunks handles the DB-vs-external-storage decision,
/// biz/request.go:1217-1258 — the P10-001 S12 "download/preview reads from DB
/// or external storage" semantics).
async fn write_static_preview(
    service: &dyn RequestPreviewService,
    req: &PreviewRequestRow,
) -> Response {
    let mut chunks = req.response_chunks.clone();
    if chunks.as_ref().is_none_or(|chunks| chunks.is_empty()) {
        match service.load_response_chunks(req).await {
            Ok(loaded) => chunks = loaded,
            Err(_) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to load request preview",
                );
            }
        }
    }

    (
        StatusCode::OK,
        Json(RequestPreviewFallbackResponse {
            mode: "static-fetch".to_string(),
            response_chunks: chunks,
        }),
    )
        .into_response()
}

/// Adapter: an unbounded mpsc receiver as a `futures_core::Stream` of body
/// frames for `Body::from_stream` (no tokio-stream dependency in-workspace).
struct SseFrameStream {
    rx: mpsc::UnboundedReceiver<Result<Bytes, Infallible>>,
}

impl futures_core::Stream for SseFrameStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().rx.poll_recv(cx)
    }
}

/// The `requestPreviewStream.Next()` loop (request_live.go:191-253) fused
/// with `WriteSSEStream`'s writer loop (chat.go:147-172), driven as a task:
///
/// 1. Drain readable chunks: skip terminal `[DONE]` chunks; chunks indexed
///    below the subscription's `replayUntil` are `preview.replay`, later ones
///    `preview.chunk` (208-222).
/// 2. Once the buffer is closed and drained, emit a single
///    `preview.completed` event with `{"status":"completed"}` (224-232) and
///    finish (234-237) — no terminal `[DONE]`/`error` event.
/// 3. Otherwise park on the subscription notification with the 3-minute idle
///    timeout (239-252); a dropped client body (Go `ctx.Done()`) also ends
///    the stream.
async fn run_preview_stream(
    buffer: Arc<dyn PreviewChunkBuffer>,
    mut subscription: PreviewSubscription,
    tx: mpsc::UnboundedSender<Result<Bytes, Infallible>>,
) {
    // request_live.go:178-187 — the subscription was taken synchronously in
    // the handler so appends between the replay snapshot and this loop are
    // never lost (Notify stores the pending permit).
    let replay_until = subscription.replay_until;
    let notify = Arc::clone(&subscription.notify);
    let mut index = 0usize;
    let mut completed = false;

    loop {
        // Inner drain loop (request_live.go:195-222); the closed flag that
        // matters is the one observed by the read that found no chunk.
        let buffer_closed = loop {
            let chunk_index = index;
            let read = buffer.read(index);

            let Some(chunk) = read.chunk else {
                break read.closed;
            };
            index = read.next_index;

            // request_live.go:207-209 — drop terminal chunks from the feed.
            if is_preview_terminal_chunk(&chunk) {
                continue;
            }

            let event_type = if chunk_index < replay_until {
                PREVIEW_REPLAY_EVENT
            } else {
                PREVIEW_CHUNK_EVENT
            };

            // Send failure ⇔ client went away (Go ctx.Done(), chat.go:148-154).
            if tx
                .send(Ok(Bytes::from(sse_event_frame(event_type, &chunk))))
                .is_err()
            {
                subscription.unsubscribe();
                return;
            }
        };

        if buffer_closed && !completed {
            // request_live.go:224-232.
            completed = true;
            let frame = sse_event_frame(PREVIEW_COMPLETED_EVENT, PREVIEW_COMPLETED_EVENT_DATA);
            if tx.send(Ok(Bytes::from(frame))).is_err() {
                subscription.unsubscribe();
                return;
            }
            continue;
        }

        if completed {
            // request_live.go:234-237 — stream is over.
            subscription.unsubscribe();
            return;
        }

        // request_live.go:239-252 — wait for activity, client exit, or idle
        // timeout.
        tokio::select! {
            _ = notify.notified() => {}
            _ = tx.closed() => {
                subscription.unsubscribe();
                return;
            }
            _ = tokio::time::sleep(PREVIEW_IDLE_TIMEOUT) => {
                subscription.unsubscribe();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use axum::Router;
    use axum::body::{BodyDataStream, to_bytes};
    use axum::http::Request;
    use conduit_config::AppConfig;
    use futures_core::Stream;
    use serde_json::json;
    use tower::Service;

    use super::*;
    use crate::app_state::AppServices;
    use crate::middleware::{
        JwtIdentityResolution, JwtIdentityResolver, JwtUserIdentity, PROJECT_ID_HEADER,
    };
    use crate::router::build_router;

    /// P-22 test seam: the preview route now requires owner/read_requests. The
    /// minted JWT resolves to an owner via this resolver so the golden cases
    /// exercise the authorized path.
    struct OwnerResolver;

    #[async_trait::async_trait]
    impl JwtIdentityResolver for OwnerResolver {
        async fn resolve(&self, _user_id: i64) -> JwtIdentityResolution {
            JwtIdentityResolution::Found(JwtUserIdentity {
                is_owner: true,
                scope_slugs: Vec::new(),
            })
        }
    }

    /// Fake standing in for RequestService + LiveStreamRegistry + ent client
    /// (mirrors newRequestPreviewTestSetup, request_live_test.go:217-278).
    #[derive(Default)]
    struct FakePreviewService {
        row: Option<PreviewRequestRow>,
        fail_get_request: bool,
        fail_load_chunks: bool,
        loaded_chunks: Option<Vec<Value>>,
        buffer: Option<Arc<InMemoryPreviewChunkBuffer>>,
    }

    #[async_trait::async_trait]
    impl RequestPreviewService for FakePreviewService {
        async fn get_request(&self, request_id: i64) -> Result<Option<PreviewRequestRow>, String> {
            if self.fail_get_request {
                return Err("db down".to_string());
            }
            Ok(self.row.clone().filter(|row| row.id == request_id))
        }

        async fn load_response_chunks(
            &self,
            _request: &PreviewRequestRow,
        ) -> Result<Option<Vec<Value>>, String> {
            if self.fail_load_chunks {
                return Err("storage down".to_string());
            }
            Ok(self.loaded_chunks.clone())
        }

        fn get_request_buffer(&self, request_id: i64) -> Option<Arc<dyn PreviewChunkBuffer>> {
            if self.row.as_ref().is_some_and(|row| row.id == request_id) {
                self.buffer
                    .as_ref()
                    .map(|buffer| Arc::clone(buffer) as Arc<dyn PreviewChunkBuffer>)
            } else {
                None
            }
        }
    }

    /// Live streaming row fixture (request_live_test.go:248-259: project 1,
    /// status processing, stream true).
    fn processing_row(request_id: i64) -> PreviewRequestRow {
        PreviewRequestRow {
            id: request_id,
            project_id: 1,
            status: REQUEST_STATUS_PROCESSING.to_string(),
            stream: true,
            response_chunks: None,
        }
    }

    /// Shared HS256 secret for the admin-group JWT guard in these tests.
    ///
    /// `/admin/requests/{request_id}/preview` lives under Go's `adminGroup`
    /// (`middleware.WithJWTAuth`, routes.go:96); the Rust router mounts it
    /// behind `jwt_admin_auth`, which reads its signing secret from
    /// `config.api_auth.jwt_secret`. The fixtures set the same secret used by
    /// [`mint_admin_jwt`] so a valid bearer token reaches the handler.
    const TEST_JWT_SECRET: &str = "request-preview-test-secret";

    /// Mint a valid HS256 bearer token accepted by the admin JWT guard,
    /// signed with [`TEST_JWT_SECRET`].
    fn mint_admin_jwt() -> String {
        use conduit_auth::jwt::{Claims, encode_hs256};
        encode_hs256(&Claims::new(42, "user:42".to_string()), TEST_JWT_SECRET).unwrap_or_default()
    }

    fn app_with(service: FakePreviewService) -> Router {
        let mut config = AppConfig::default();
        config.api_auth.jwt_secret = Some(TEST_JWT_SECRET.to_string());
        let services = AppServices::new()
            .with_request_preview_service(Arc::new(service))
            .with_user_principal_service(Arc::new(OwnerResolver));
        build_router(AppState::new(Arc::new(config), Arc::new(services)))
    }

    /// Router with the JWT guard secret wired but NO request-preview service,
    /// exercising the handler's unwired-service degradation branch. The secret
    /// is required so the request clears the `jwt_admin_auth` guard and reaches
    /// the handler (a bare `AppState::default()` has no secret, so the guard
    /// would 500 with "Failed to validate token" before the handler runs).
    fn app_without_service() -> Router {
        let mut config = AppConfig::default();
        config.api_auth.jwt_secret = Some(TEST_JWT_SECRET.to_string());
        build_router(AppState::new(
            Arc::new(config),
            Arc::new(AppServices::default().with_user_principal_service(Arc::new(OwnerResolver))),
        ))
    }

    async fn get_preview(
        app: &mut Router,
        request_id: &str,
        project_header: Option<&str>,
    ) -> Result<Response, Box<dyn StdError>> {
        // The route sits under Go's `adminGroup` JWT guard (routes.go:96);
        // attach a valid bearer token so the request reaches the handler
        // instead of short-circuiting at the `jwt_admin_auth` 401.
        let mut builder = Request::builder()
            .uri(format!("/admin/requests/{request_id}/preview"))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", mint_admin_jwt()),
            );
        if let Some(header_value) = project_header {
            builder = builder.header(PROJECT_ID_HEADER, header_value);
        }
        let request = builder.body(Body::empty())?;
        Ok(app.call(request).await?)
    }

    fn project_guid(id: i64) -> String {
        format!("gid://conduit/Project/{id}")
    }

    fn header_value(response: &Response, name: header::HeaderName) -> String {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    // ---- SSE reader (mirrors readSSEEvent, request_live_test.go:295-335) ----

    #[derive(Debug, Default, PartialEq, Eq)]
    struct SseTestEvent {
        event: String,
        data: String,
    }

    struct SseReader {
        stream: BodyDataStream,
        buffered: Vec<u8>,
    }

    impl SseReader {
        fn new(body: Body) -> Self {
            Self {
                stream: body.into_data_stream(),
                buffered: Vec::new(),
            }
        }

        /// Reads one `\n\n`-terminated frame with the Go test's 2s timeout.
        async fn next_event(&mut self) -> Result<SseTestEvent, Box<dyn StdError>> {
            match tokio::time::timeout(Duration::from_secs(2), self.next_event_inner()).await {
                Ok(result) => result,
                Err(_) => Err("timed out waiting for SSE event".into()),
            }
        }

        async fn next_event_inner(&mut self) -> Result<SseTestEvent, Box<dyn StdError>> {
            loop {
                if let Some(end) = frame_end(&self.buffered) {
                    let frame: Vec<u8> = self.buffered.drain(..end + 2).collect();
                    return parse_sse_frame(&frame);
                }
                let chunk =
                    std::future::poll_fn(|cx| Pin::new(&mut self.stream).poll_next(cx)).await;
                match chunk {
                    Some(Ok(bytes)) => self.buffered.extend_from_slice(&bytes),
                    Some(Err(err)) => return Err(err.into()),
                    None => return Err("body ended before a full SSE frame".into()),
                }
            }
        }
    }

    fn frame_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(2).position(|window| window == b"\n\n")
    }

    /// Parses `event:`/`data: ` lines; data lines are joined with `\n`
    /// (inverting the looplj/sse dataReplacer).
    fn parse_sse_frame(frame: &[u8]) -> Result<SseTestEvent, Box<dyn StdError>> {
        let text = std::str::from_utf8(frame)?;
        let mut event = SseTestEvent::default();
        let mut data_lines: Vec<&str> = Vec::new();
        for line in text.lines() {
            if let Some(name) = line.strip_prefix("event:") {
                event.event = name.to_string();
            } else if let Some(payload) = line.strip_prefix("data:") {
                // The encoder always writes exactly one space after `data:`.
                data_lines.push(payload.strip_prefix(' ').unwrap_or(payload));
            }
        }
        event.data = data_lines.join("\n");
        Ok(event)
    }

    // ---- SSE golden tests (mirror request_live_test.go) ----

    const ANNOTATION_CHUNK: &str = r#"{"id":"chatcmpl-preview","object":"chat.completion.chunk","model":"sonar-deep-research","choices":[{"index":0,"delta":{"content":"Source","annotations":[{"type":"url_citation","start_index":0,"end_index":6,"url_citation":{"url":"https://example.com/result","title":"Example Result"}}]}}]}"#;

    /// Mirrors `TestRequestPreviewHandlers_ReplayOnConnect`
    /// (request_live_test.go:30-56): closed buffer replays chunks, omits the
    /// terminal `[DONE]`, then emits `preview.completed`.
    #[tokio::test]
    async fn replay_on_connect() -> Result<(), Box<dyn StdError>> {
        let buffer = Arc::new(InMemoryPreviewChunkBuffer::new());
        assert!(buffer.append(br#"{"index":1}"#.to_vec()));
        assert!(buffer.append(DONE_STREAM_EVENT_DATA.to_vec()));
        buffer.close();

        let mut app = app_with(FakePreviewService {
            row: Some(processing_row(5)),
            buffer: Some(buffer),
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            header_value(&response, header::CONTENT_TYPE).contains("text/event-stream"),
            "content-type"
        );
        assert_eq!(header_value(&response, header::CACHE_CONTROL), "no-cache");
        assert_eq!(header_value(&response, header::CONNECTION), "keep-alive");

        let mut reader = SseReader::new(response.into_body());
        let first = reader.next_event().await?;
        assert_eq!(first.event, "preview.replay");
        assert_eq!(
            serde_json::from_str::<Value>(&first.data)?,
            json!({"index": 1})
        );

        let second = reader.next_event().await?;
        assert_eq!(second.event, "preview.completed");
        assert_eq!(
            serde_json::from_str::<Value>(&second.data)?,
            json!({"status": "completed"})
        );
        Ok(())
    }

    /// Mirrors `TestRequestPreviewHandlers_ReplayPreservesAnnotationChunk`
    /// (request_live_test.go:58-79). The chunk bytes pass through the SSE
    /// path untouched, so the assertion here is byte-exact, stronger than
    /// Go's JSONEq.
    #[tokio::test]
    async fn replay_preserves_annotation_chunk() -> Result<(), Box<dyn StdError>> {
        let buffer = Arc::new(InMemoryPreviewChunkBuffer::new());
        assert!(buffer.append(ANNOTATION_CHUNK.as_bytes().to_vec()));
        assert!(buffer.append(DONE_STREAM_EVENT_DATA.to_vec()));
        buffer.close();

        let mut app = app_with(FakePreviewService {
            row: Some(processing_row(5)),
            buffer: Some(buffer),
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;
        let mut reader = SseReader::new(response.into_body());

        let first = reader.next_event().await?;
        assert_eq!(first.event, "preview.replay");
        assert_eq!(first.data, ANNOTATION_CHUNK);

        let completed = reader.next_event().await?;
        assert_eq!(completed.event, "preview.completed");
        assert_eq!(
            serde_json::from_str::<Value>(&completed.data)?,
            json!({"status": "completed"})
        );
        Ok(())
    }

    /// Mirrors `TestRequestPreviewHandlers_IncrementalDeliveryAfterReplay`
    /// (request_live_test.go:81-112): chunks appended after connect arrive as
    /// `preview.chunk`.
    #[tokio::test]
    async fn incremental_delivery_after_replay() -> Result<(), Box<dyn StdError>> {
        let buffer = Arc::new(InMemoryPreviewChunkBuffer::new());
        assert!(buffer.append(br#"{"index":1}"#.to_vec()));

        let mut app = app_with(FakePreviewService {
            row: Some(processing_row(5)),
            buffer: Some(Arc::clone(&buffer)),
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;
        let mut reader = SseReader::new(response.into_body());

        let replay = reader.next_event().await?;
        assert_eq!(replay.event, "preview.replay");
        assert_eq!(
            serde_json::from_str::<Value>(&replay.data)?,
            json!({"index": 1})
        );

        assert!(buffer.append(br#"{"index":2}"#.to_vec()));
        assert!(buffer.append(DONE_STREAM_EVENT_DATA.to_vec()));
        buffer.close();

        let incremental = reader.next_event().await?;
        assert_eq!(incremental.event, "preview.chunk");
        assert_eq!(
            serde_json::from_str::<Value>(&incremental.data)?,
            json!({"index": 2})
        );

        let completed = reader.next_event().await?;
        assert_eq!(completed.event, "preview.completed");
        Ok(())
    }

    /// Mirrors `TestRequestPreviewHandlers_WaitsForFirstChunkWhenProcessing`
    /// (request_live_test.go:114-146): connecting to an empty live buffer
    /// holds the stream open; the first append arrives as `preview.chunk`
    /// (replay cutoff was snapshotted at 0).
    #[tokio::test]
    async fn waits_for_first_chunk_when_processing() -> Result<(), Box<dyn StdError>> {
        let buffer = Arc::new(InMemoryPreviewChunkBuffer::new());

        let mut app = app_with(FakePreviewService {
            row: Some(processing_row(5)),
            buffer: Some(Arc::clone(&buffer)),
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(header_value(&response, header::CONTENT_TYPE).contains("text/event-stream"));

        assert!(buffer.append(br#"{"index":1}"#.to_vec()));
        assert!(buffer.append(DONE_STREAM_EVENT_DATA.to_vec()));
        buffer.close();

        let mut reader = SseReader::new(response.into_body());
        let first = reader.next_event().await?;
        assert_eq!(first.event, "preview.chunk");
        assert_eq!(
            serde_json::from_str::<Value>(&first.data)?,
            json!({"index": 1})
        );

        let completed = reader.next_event().await?;
        assert_eq!(completed.event, "preview.completed");
        assert_eq!(
            serde_json::from_str::<Value>(&completed.data)?,
            json!({"status": "completed"})
        );
        Ok(())
    }

    /// Mirrors `TestRequestPreviewHandlers_CorrectHeaders`
    /// (request_live_test.go:148-163).
    #[tokio::test]
    async fn correct_headers() -> Result<(), Box<dyn StdError>> {
        let buffer = Arc::new(InMemoryPreviewChunkBuffer::new());
        assert!(buffer.append(br#"{"index":1}"#.to_vec()));
        buffer.close();

        let mut app = app_with(FakePreviewService {
            row: Some(processing_row(5)),
            buffer: Some(buffer),
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;

        assert!(header_value(&response, header::CONTENT_TYPE).contains("text/event-stream"));
        // Exact looplj/sse content type, including the missing space.
        assert_eq!(
            header_value(&response, header::CONTENT_TYPE),
            "text/event-stream;charset=utf-8"
        );
        assert_eq!(header_value(&response, header::CACHE_CONTROL), "no-cache");
        assert_eq!(header_value(&response, header::CONNECTION), "keep-alive");
        Ok(())
    }

    /// Mirrors
    /// `TestRequestPreviewHandlers_FallbackToStaticFetchForCompletedRequests`
    /// (request_live_test.go:165-185): completed requests get the JSON
    /// static-fetch payload built from the persisted row chunks.
    #[tokio::test]
    async fn fallback_to_static_fetch_for_completed_requests() -> Result<(), Box<dyn StdError>> {
        let mut row = processing_row(5);
        row.status = "completed".to_string();
        row.response_chunks = Some(vec![json!({"persisted": true})]);

        let mut app = app_with(FakePreviewService {
            row: Some(row),
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(header_value(&response, header::CONTENT_TYPE).contains("application/json"));

        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body: RequestPreviewFallbackResponse = serde_json::from_slice(&bytes)?;
        assert_eq!(body.mode, "static-fetch");
        let chunks = body.response_chunks.unwrap_or_default();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], json!({"persisted": true}));
        Ok(())
    }

    /// Mirrors
    /// `TestRequestPreviewHandlers_FallbackToStaticFetchPreservesAnnotationChunks`
    /// (request_live_test.go:187-207).
    #[tokio::test]
    async fn fallback_preserves_annotation_chunks() -> Result<(), Box<dyn StdError>> {
        let chunk: Value =
            serde_json::from_str(&format!(r#"{{"event":"","data":{ANNOTATION_CHUNK}}}"#))?;
        let mut row = processing_row(5);
        row.status = "completed".to_string();
        row.response_chunks = Some(vec![chunk.clone()]);

        let mut app = app_with(FakePreviewService {
            row: Some(row),
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body: RequestPreviewFallbackResponse = serde_json::from_slice(&bytes)?;
        assert_eq!(body.mode, "static-fetch");
        let chunks = body.response_chunks.unwrap_or_default();
        assert_eq!(chunks.len(), 1);
        // Go asserts JSONEq; Value equality is the same semantic comparison.
        assert_eq!(chunks[0], chunk);
        Ok(())
    }

    /// Processing+stream but no registered buffer → static fallback through
    /// LoadResponseChunks (request_live.go:132-136 → 148-163). A `None`
    /// chunk load mirrors Go's nil slice → `"responseChunks":null`.
    #[tokio::test]
    async fn missing_buffer_falls_back_to_loaded_chunks() -> Result<(), Box<dyn StdError>> {
        // Loaded chunks present.
        let mut app = app_with(FakePreviewService {
            row: Some(processing_row(5)),
            loaded_chunks: Some(vec![json!({"loaded": true})]),
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(
            body,
            json!({"mode": "static-fetch", "responseChunks": [{"loaded": true}]})
        );

        // Nil chunks (Go nil slice) serialize as null, not [].
        let mut app = app_with(FakePreviewService {
            row: Some(processing_row(5)),
            loaded_chunks: None,
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(
            body,
            json!({"mode": "static-fetch", "responseChunks": null})
        );
        Ok(())
    }

    /// LoadResponseChunks failure → 500 "Failed to load request preview"
    /// (request_live.go:151-156).
    #[tokio::test]
    async fn fallback_chunk_load_failure_returns_500() -> Result<(), Box<dyn StdError>> {
        let mut row = processing_row(5);
        row.stream = false;
        let mut app = app_with(FakePreviewService {
            row: Some(row),
            fail_load_chunks: true,
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;

        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({"error": {"type": "Internal Server Error", "message": "Failed to load request preview"}})
        );
        Ok(())
    }

    /// Error-shape branches shared with the content endpoint
    /// (request_live.go:100-125).
    #[tokio::test]
    async fn error_branches_match_go() -> Result<(), Box<dyn StdError>> {
        // Project mismatch → 404 "Request not found".
        let mut app = app_with(FakePreviewService {
            row: Some(processing_row(5)),
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(2))).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "Request not found");

        // Row not found → 404; row load failure → 500; unwired service → 500.
        let mut app = app_with(FakePreviewService::default());
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let mut app = app_with(FakePreviewService {
            fail_get_request: true,
            ..FakePreviewService::default()
        });
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "Failed to load request");

        // Unwired service, but the JWT guard secret is wired so the request
        // clears `jwt_admin_auth` and reaches the handler's degradation branch.
        let mut app = app_without_service();
        let response = get_preview(&mut app, "5", Some(&project_guid(1))).await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // Missing project header → 400; bad request id → wrapped strconv 400.
        let mut app = app_with(FakePreviewService::default());
        let response = get_preview(&mut app, "5", None).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "Project ID not found in context");

        let mut app = app_with(FakePreviewService::default());
        let response = get_preview(&mut app, "abc", Some(&project_guid(1))).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["message"],
            "Invalid request body: strconv.ParseInt: parsing \"abc\": invalid syntax"
        );
        Ok(())
    }

    // ---- protocol/unit golden cases ----

    /// looplj/sse frame bytes: `event:` without a space, `data: ` with one,
    /// newline payloads continued as extra `data: ` lines.
    #[test]
    fn sse_event_frame_matches_looplj_encoder() {
        assert_eq!(
            sse_event_frame("preview.replay", br#"{"index":1}"#),
            b"event:preview.replay\ndata: {\"index\":1}\n\n".to_vec()
        );
        // Multi-line payload: dataReplacer maps '\n' to "\ndata: ".
        assert_eq!(
            sse_event_frame("preview.chunk", b"a\nb"),
            b"event:preview.chunk\ndata: a\ndata: b\n\n".to_vec()
        );
        // Carriage returns are escaped, and event names are field-escaped.
        assert_eq!(
            sse_event_frame("na\nme", b"x\ry"),
            b"event:na\\nme\ndata: x\\ry\n\n".to_vec()
        );
        // Empty event name omits the event line (writeEvent guard).
        assert_eq!(sse_event_frame("", b"x"), b"data: x\n\n".to_vec());
    }

    /// `preview.completed` payload bytes match Go's marshalled gin.H
    /// (request_live.go:271).
    #[test]
    fn preview_completed_event_data_matches_go() -> Result<(), serde_json::Error> {
        assert_eq!(
            serde_json::from_slice::<Value>(PREVIEW_COMPLETED_EVENT_DATA)?,
            json!({"status": "completed"})
        );
        Ok(())
    }

    /// Terminal-chunk detection (request_live.go:281-283).
    #[test]
    fn terminal_chunk_detection_matches_go() {
        assert!(is_preview_terminal_chunk(b"[DONE]"));
        assert!(!is_preview_terminal_chunk(b"{\"index\":1}"));
        assert!(!is_preview_terminal_chunk(b""));
    }

    /// `RequestDetailSSEContract()` golden values (request_live.go:66-87).
    #[test]
    fn request_detail_sse_contract_matches_go() {
        let contract = request_detail_sse_contract();
        assert!(contract.single_instance_only);
        assert!(!contract.supports_distributed_replay);
        assert!(!contract.allows_database_schema_changes);
        assert!(!contract.execution_level_preview);
        assert_eq!(contract.event_order, ["replay", "incremental"]);
        assert_eq!(contract.scope, "request");
        assert!(contract.reuse_in_memory_chunk_buffer);
        assert!(contract.final_batch_persistence_unchanged);
        assert_eq!(contract.fallback_mode, "static-fetch");
        assert_eq!(
            contract.fallback_behavior,
            "load persisted request detail once when SSE cannot connect"
        );
        assert!(!contract.fallback_uses_execution_preview);
        assert!(!contract.fallback_starts_secondary_live_polling_loop);
        assert_eq!(
            contract.endpoint_path,
            "/admin/requests/:request_id/preview"
        );
        assert_eq!(contract.content_type, "text/event-stream");
        assert_eq!(
            contract.event_types,
            ["preview.replay", "preview.chunk", "preview.completed"]
        );
        assert!(contract.replay_omits_terminal_done_event);
        assert!(contract.incremental_omits_terminal_done_event);
        assert!(contract.connect_after_completion_falls_back_to_static_fetch);
    }

    // ---- InMemoryPreviewChunkBuffer (chunkbuffer.go parity) ----

    /// Append/Read/Close basics (chunkbuffer.go:39-118).
    #[test]
    fn buffer_append_read_close_semantics() {
        let buffer = InMemoryPreviewChunkBuffer::new();
        assert!(buffer.is_empty());
        assert!(buffer.append(b"a".to_vec()));
        assert!(buffer.append(b"b".to_vec()));
        assert_eq!(buffer.len(), 2);
        assert!(!buffer.is_closed());

        let read = buffer.read(0);
        assert_eq!(read.chunk.as_deref(), Some(b"a".as_slice()));
        assert_eq!(read.next_index, 1);
        assert!(!read.closed);

        // Out-of-range read: (nil, index, closed, false).
        let read = buffer.read(2);
        assert_eq!(read.chunk, None);
        assert_eq!(read.next_index, 2);

        buffer.close();
        assert!(buffer.is_closed());
        // Appends after Close are rejected (chunkbuffer.go:47-49).
        assert!(!buffer.append(b"c".to_vec()));
        assert_eq!(buffer.len(), 2);
        let read = buffer.read(2);
        assert!(read.closed);
    }

    /// Capacity guard (chunkbuffer.go:51-54): the 50000th append succeeds,
    /// the 50001st is rejected.
    #[test]
    fn buffer_rejects_appends_beyond_capacity() {
        let buffer = InMemoryPreviewChunkBuffer::new();
        for _ in 0..MAX_CHUNK_CAPACITY {
            assert!(buffer.append(Vec::new()));
        }
        assert!(!buffer.append(Vec::new()));
        assert_eq!(buffer.len(), MAX_CHUNK_CAPACITY);
    }

    /// SubscribeFromCurrent snapshots the replay cutoff and delivers
    /// coalesced wakeups; closed buffers hand back a pre-notified
    /// subscription (chunkbuffer.go:127-152).
    #[tokio::test]
    async fn buffer_subscription_semantics() -> Result<(), Box<dyn StdError>> {
        let buffer = InMemoryPreviewChunkBuffer::new();
        assert!(buffer.append(b"a".to_vec()));

        let subscription = buffer.subscribe_from_current();
        assert_eq!(subscription.replay_until, 1);

        // No signal pending yet.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), subscription.notify.notified())
                .await
                .is_err()
        );

        // Append broadcasts; the permit is stored even with no waiter.
        assert!(buffer.append(b"b".to_vec()));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), subscription.notify.notified())
                .await
                .is_ok()
        );

        // Close also broadcasts.
        buffer.close();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), subscription.notify.notified())
                .await
                .is_ok()
        );

        // Subscribing to a closed buffer: replay cutoff at len, one token
        // pre-loaded, unsubscribe is a no-op.
        let mut closed_subscription = buffer.subscribe_from_current();
        assert_eq!(closed_subscription.replay_until, 2);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                closed_subscription.notify.notified()
            )
            .await
            .is_ok()
        );
        closed_subscription.unsubscribe();
        Ok(())
    }
}
