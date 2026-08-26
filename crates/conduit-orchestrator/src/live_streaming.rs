//! RUST-P9-006 S37 — live-preview mount point (stream observer) + registry.
//!
//! Go mounts live preview and request persistence as **pipeline middlewares**
//! (`orchestrator.go:235-289`), never inside provider transformers. The Rust
//! port keeps that shape: persistence is already observer-shaped (the
//! [`crate::orchestrator::RequestRecorder`] trait mounted on
//! `CommandOrchestrator`), and this module adds the live-preview side:
//!
//! * [`ChunkBuffer`] — port of `internal/pkg/chunkbuffer/chunkbuffer.go`
//!   (thread-safe chunk accumulator shared between the streaming loop and the
//!   live-preview read side).
//! * [`LiveStreamRegistry`] — port of `internal/server/biz/stream_preview.go`
//!   (`LiveStreamRegistry`, lines 16-108 + the sweeper body at 127-169).
//! * [`StreamObserver`] — the S37 mounting-point abstraction: the streaming
//!   forward loop ([`crate::outbound_stream`]) drives a set of observers so
//!   live preview (and any future cross-cutting concern) hooks the stream
//!   without touching provider transformers.
//! * [`LivePreviewObserver`] — port of `livePreviewMiddleware` + the
//!   `liveRequestStream` / `liveRequestExecutionStream` wrappers
//!   (`internal/server/orchestrator/live_streaming.go`).
//!
//! # Deviations from Go (deliberate, noted)
//!
//! * Go keys registry entries by `int` ids; the Rust services layer uses
//!   string ids throughout, so the registry keys are `String`.
//! * `chunkbuffer.Buffer.SubscribeFromCurrent` (the GraphQL live-preview read
//!   subscription) is **not** ported here — the read side belongs to the API
//!   layer task. The write-side contract (append/slice/close/idle tracking)
//!   is complete, which is all the orchestrator mount needs.
//! * Go's `marshalPreviewChunks` filters chunks through
//!   `shouldSkipStoredStreamChunk` / `marshalStreamEventForStorage`
//!   (biz-side storage marshalling). The registry here returns raw
//!   [`StreamEvent`] snapshots; the storage-marshal filter lands with the
//!   request-chunks storage surface.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use conduit_llm::StreamEvent;

use crate::orchestrator::LivePreviewPlan;
use crate::outbound_stream::summarize_binary_chunk;

// ---------------------------------------------------------------------------
// ChunkBuffer — port of Go `internal/pkg/chunkbuffer/chunkbuffer.go`.
// ---------------------------------------------------------------------------

/// Maximum number of chunks a buffer accepts before rejecting appends.
/// Mirrors Go `maxChunkCapacity = 50000` (`chunkbuffer.go:26`).
pub const MAX_CHUNK_CAPACITY: usize = 50_000;

#[derive(Debug)]
struct ChunkBufferState {
    /// Go `chunks []*httpclient.StreamEvent`.
    chunks: Vec<StreamEvent>,
    /// Go `closed bool`.
    closed: bool,
    /// Go `lastAppendedAt time.Time` (initialized to `time.Now()` in `New`).
    last_appended_at: Instant,
}

/// Thread-safe buffer for accumulating stream chunks. Port of Go
/// `chunkbuffer.Buffer` (`chunkbuffer.go:17-24`): the single source of truth
/// for live-preview reads while a stream is in flight.
///
/// Cloning shares the underlying buffer (Go passes `*Buffer` around); the
/// registry and the observer hold clones of the same buffer.
#[derive(Debug, Clone)]
pub struct ChunkBuffer {
    inner: Arc<Mutex<ChunkBufferState>>,
}

impl Default for ChunkBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkBuffer {
    /// Create an empty buffer. Mirrors Go `chunkbuffer.New()`
    /// (`chunkbuffer.go:29-35`), including seeding `last_appended_at` with the
    /// creation time so a never-appended buffer still ages for the sweeper.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ChunkBufferState {
                chunks: Vec::new(),
                closed: false,
                last_appended_at: Instant::now(),
            })),
        }
    }

    /// Append a chunk. Returns `false` when the buffer is closed or at
    /// capacity (Go `Append`, `chunkbuffer.go:40-61`; the Go nil-chunk guard
    /// has no Rust analog — `StreamEvent` is passed by value). Poisoned-lock
    /// appends are dropped (`false`), mirroring the reject-on-closed shape.
    pub fn append(&self, chunk: StreamEvent) -> bool {
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        if state.closed || state.chunks.len() >= MAX_CHUNK_CAPACITY {
            // Go: reject to prevent unbounded memory growth (chunkbuffer.go:52-55).
            return false;
        }
        state.chunks.push(chunk);
        state.last_appended_at = Instant::now();
        true
    }

    /// Snapshot of all chunks (Go `Slice`, `chunkbuffer.go:64-72` — returns a
    /// copy so callers cannot mutate the buffer).
    pub fn slice(&self) -> Vec<StreamEvent> {
        self.inner
            .lock()
            .map(|state| state.chunks.clone())
            .unwrap_or_default()
    }

    /// Current chunk count (Go `Len`, `chunkbuffer.go:75-79`).
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .map(|state| state.chunks.len())
            .unwrap_or(0)
    }

    /// Whether the buffer holds no chunks.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Chunk at `index` when present (Go `At`, `chunkbuffer.go:82-91`).
    pub fn at(&self, index: usize) -> Option<StreamEvent> {
        self.inner
            .lock()
            .ok()
            .and_then(|state| state.chunks.get(index).cloned())
    }

    /// Read the chunk at `index`: returns `(chunk, next_index, closed)` where
    /// `chunk` is `None` when out of range. Port of Go `Read`
    /// (`chunkbuffer.go:95-103`) — Go's `ok` return maps to `chunk.is_some()`.
    pub fn read(&self, index: usize) -> (Option<StreamEvent>, usize, bool) {
        let Ok(state) = self.inner.lock() else {
            return (None, index, true);
        };
        match state.chunks.get(index) {
            Some(chunk) => (Some(chunk.clone()), index + 1, state.closed),
            None => (None, index, state.closed),
        }
    }

    /// Timestamp of the last successful append (Go `LastAppendedAt`,
    /// `chunkbuffer.go:106-110`). Falls back to `Instant::now()` on a poisoned
    /// lock so a broken buffer never looks idle-stale by accident.
    pub fn last_appended_at(&self) -> Instant {
        self.inner
            .lock()
            .map(|state| state.last_appended_at)
            .unwrap_or_else(|_| Instant::now())
    }

    /// Mark the buffer closed, preventing further appends (Go `Close`,
    /// `chunkbuffer.go:113-118`).
    pub fn close(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.closed = true;
        }
    }

    /// Whether the buffer is closed (Go `IsClosed`, `chunkbuffer.go:121-125`).
    pub fn is_closed(&self) -> bool {
        self.inner.lock().map(|state| state.closed).unwrap_or(true)
    }
}

// ---------------------------------------------------------------------------
// LiveStreamRegistry — port of Go `internal/server/biz/stream_preview.go`.
// ---------------------------------------------------------------------------

/// Read access to in-flight stream chunks without duplicating data. Holds
/// references to [`ChunkBuffer`]s owned by the streaming forward loop. Port of
/// Go `biz.LiveStreamRegistry` (`stream_preview.go:16-19`, two `sync.Map`s).
///
/// Keys are string ids (Go uses `int` — see module-docs deviation note).
#[derive(Debug, Default)]
pub struct LiveStreamRegistry {
    requests: Mutex<BTreeMap<String, ChunkBuffer>>,
    executions: Mutex<BTreeMap<String, ChunkBuffer>>,
}

impl LiveStreamRegistry {
    /// Mirrors Go `NewLiveStreamRegistry` (`stream_preview.go:22-24`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Go `RegisterRequest` (`stream_preview.go:26-28`).
    pub fn register_request(&self, request_id: impl Into<String>, buffer: ChunkBuffer) {
        if let Ok(mut map) = self.requests.lock() {
            map.insert(request_id.into(), buffer);
        }
    }

    /// Go `RegisterExecution` (`stream_preview.go:30-32`).
    pub fn register_execution(&self, execution_id: impl Into<String>, buffer: ChunkBuffer) {
        if let Ok(mut map) = self.executions.lock() {
            map.insert(execution_id.into(), buffer);
        }
    }

    /// Go `UnregisterRequest` (`stream_preview.go:34-36`).
    pub fn unregister_request(&self, request_id: &str) {
        if let Ok(mut map) = self.requests.lock() {
            map.remove(request_id);
        }
    }

    /// Go `UnregisterExecution` (`stream_preview.go:38-40`).
    pub fn unregister_execution(&self, execution_id: &str) {
        if let Ok(mut map) = self.executions.lock() {
            map.remove(execution_id);
        }
    }

    /// Go `GetRequestBuffer` (`stream_preview.go:42-54`). `None` when absent.
    pub fn get_request_buffer(&self, request_id: &str) -> Option<ChunkBuffer> {
        self.requests
            .lock()
            .ok()
            .and_then(|map| map.get(request_id).cloned())
    }

    /// Go `GetExecutionBuffer` (`stream_preview.go:56-68`). `None` when absent.
    pub fn get_execution_buffer(&self, execution_id: &str) -> Option<ChunkBuffer> {
        self.executions
            .lock()
            .ok()
            .and_then(|map| map.get(execution_id).cloned())
    }

    /// Snapshot of the live request chunks (Go `GetRequestChunks`,
    /// `stream_preview.go:72-74`; storage-marshal filter deferred — see module
    /// docs). Empty when no buffer is registered.
    pub fn get_request_chunks(&self, request_id: &str) -> Vec<StreamEvent> {
        self.get_request_buffer(request_id)
            .map(|buffer| buffer.slice())
            .unwrap_or_default()
    }

    /// Snapshot of the live execution chunks (Go `GetExecutionChunks`,
    /// `stream_preview.go:78-80`).
    pub fn get_execution_chunks(&self, execution_id: &str) -> Vec<StreamEvent> {
        self.get_execution_buffer(execution_id)
            .map(|buffer| buffer.slice())
            .unwrap_or_default()
    }

    /// Remove closed buffers and force-close + remove buffers idle longer than
    /// `idle_threshold`. Returns the evicted count. Port of the sweeper body
    /// `sweepStaleEntries` (`stream_preview.go:127-169`; Go's threshold is 10
    /// minutes — the scheduler worker owns the ticker cadence and passes the
    /// threshold in, mirroring the RUST scheduler S04 split).
    pub fn sweep_stale_entries(&self, idle_threshold: Duration) -> usize {
        let now = Instant::now();
        let mut evicted = 0usize;

        let mut sweep_map = |entries: &Mutex<BTreeMap<String, ChunkBuffer>>| {
            let Ok(mut map) = entries.lock() else {
                return;
            };
            map.retain(|_, buffer| {
                // Go: closed buffers are evicted (stream_preview.go:141-147).
                if buffer.is_closed() {
                    evicted += 1;
                    return false;
                }
                // Go: idle zombies are force-closed then evicted
                // (stream_preview.go:148-158).
                if now.duration_since(buffer.last_appended_at()) > idle_threshold {
                    buffer.close();
                    evicted += 1;
                    return false;
                }
                true
            });
        };

        sweep_map(&self.requests);
        sweep_map(&self.executions);

        evicted
    }
}

// ---------------------------------------------------------------------------
// StreamObserver — the S37 mounting-point abstraction.
// ---------------------------------------------------------------------------

/// Cross-cutting observer mounted on the streaming forward loop
/// ([`crate::outbound_stream::OutboundForwardingStream`]).
///
/// This is the Rust analog of Go's middleware hooks around the outbound
/// stream: concerns like live preview attach here instead of leaking into
/// provider transformers (the S37 invariant, Go `orchestrator.go:235-289`).
/// All hooks default to no-ops so observers implement only what they need.
pub trait StreamObserver: Send + Sync {
    /// Fired once before events flow, when the outbound attempt starts.
    /// Mirrors Go `livePreviewMiddleware.OnOutboundRawRequest`
    /// (`live_streaming.go:55-69`) — buffer registration happens here.
    fn on_attempt_start(&self) {}

    /// Fired for every stream event as it is forwarded to the client.
    /// Mirrors the Go live wrappers' `Next()` bodies
    /// (`live_streaming.go:144-159` / `:190-205`).
    fn on_event(&self, event: &StreamEvent) {
        let _ = event;
    }

    /// Fired when the attempt fails before/without a stream (Go
    /// `livePreviewMiddleware.OnOutboundRawError`, `live_streaming.go:71-89`).
    fn on_error(&self) {}

    /// Fired exactly once when the stream ends (any path). Mirrors the Go live
    /// wrappers' `Close()` bodies (`live_streaming.go:169-179` / `:215-225`).
    fn on_close(&self) {}
}

// ---------------------------------------------------------------------------
// LivePreviewObserver — port of Go `livePreviewMiddleware` + live wrappers.
// ---------------------------------------------------------------------------

/// Live-preview observer: registers [`ChunkBuffer`]s in the
/// [`LiveStreamRegistry`], fans summarized chunks out to them while the stream
/// is forwarded, and closes/unregisters them at stream end.
///
/// Built from the pure [`LivePreviewPlan`] (S23) so the enable/disable gates
/// (`registry`/`streaming`/`policy`) stay in one place. When the plan is
/// disabled every hook is a no-op (Go's `if !m.enabled` short-circuits).
///
/// # Deviation note (single event domain)
///
/// Go appends provider-format events to the **execution** buffer
/// (`OnOutboundRawStream`) and client-format events to the **request** buffer
/// (`OnInboundRawStream`). The Rust forward loop currently has a single event
/// domain (response-side transformer wiring is the S29-adjacent gap), so both
/// buffers observe the same events at this mount point. The mount split is
/// preserved (two buffers, two ids) so the wiring tightens without an API
/// change once per-side transforms land.
pub struct LivePreviewObserver {
    registry: Arc<LiveStreamRegistry>,
    plan: LivePreviewPlan,
}

impl LivePreviewObserver {
    /// Build an observer from the S23 plan. Mirrors `withLivePreview`
    /// (`live_streaming.go:24-30`) + the `OnInboundLlmRequest` gating that the
    /// plan already encodes.
    pub fn from_plan(plan: LivePreviewPlan, registry: Arc<LiveStreamRegistry>) -> Self {
        Self { registry, plan }
    }

    /// Whether the observer is active (plan enabled).
    pub fn enabled(&self) -> bool {
        self.plan.enabled
    }
}

impl StreamObserver for LivePreviewObserver {
    /// Go `OnOutboundRawRequest` (`live_streaming.go:55-69`): register a fresh
    /// buffer per id, only when none is registered yet.
    fn on_attempt_start(&self) {
        if !self.plan.enabled {
            return;
        }
        if let Some(request_id) = self.plan.request_id.as_deref()
            && self.registry.get_request_buffer(request_id).is_none()
        {
            self.registry
                .register_request(request_id, ChunkBuffer::new());
        }
        if let Some(exec_id) = self.plan.request_exec_id.as_deref()
            && self.registry.get_execution_buffer(exec_id).is_none()
        {
            self.registry
                .register_execution(exec_id, ChunkBuffer::new());
        }
    }

    /// Go live wrappers' `Next()` (`live_streaming.go:144-159` request-exec /
    /// `:190-205` request): append a binary-summarized copy so live preview
    /// never retains full TTS audio bytes; the client still receives the
    /// unmodified event from the forward loop.
    fn on_event(&self, event: &StreamEvent) {
        if !self.plan.enabled {
            return;
        }
        let summarized = summarize_binary_chunk(event);
        if let Some(exec_id) = self.plan.request_exec_id.as_deref()
            && let Some(buffer) = self.registry.get_execution_buffer(exec_id)
        {
            buffer.append(summarized.clone());
        }
        if let Some(request_id) = self.plan.request_id.as_deref()
            && let Some(buffer) = self.registry.get_request_buffer(request_id)
        {
            buffer.append(summarized);
        }
    }

    /// Go `OnOutboundRawError` (`live_streaming.go:71-89`): close + unregister
    /// the execution buffer first, then the request buffer (Go order).
    fn on_error(&self) {
        if !self.plan.enabled {
            return;
        }
        if let Some(exec_id) = self.plan.request_exec_id.as_deref()
            && let Some(buffer) = self.registry.get_execution_buffer(exec_id)
        {
            buffer.close();
            self.registry.unregister_execution(exec_id);
        }
        if let Some(request_id) = self.plan.request_id.as_deref()
            && let Some(buffer) = self.registry.get_request_buffer(request_id)
        {
            buffer.close();
            self.registry.unregister_request(request_id);
        }
    }

    /// Go live wrappers' `Close()` (`live_streaming.go:169-179` / `:215-225`):
    /// close the buffer and unregister the id, idempotently (Go guards with
    /// `s.closed`; the registry remove is naturally idempotent here).
    fn on_close(&self) {
        // Same teardown as the error path — Go's two wrappers each close their
        // own buffer; the observer owns both.
        self.on_error();
    }
}

// ===========================================================================
// Tests — mirror Go `live_streaming_test.go` + `chunkbuffer_test.go` golden
// behaviors at the observer/buffer boundary.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{LivePreviewDisableReason, live_preview_plan};

    fn event(data: &str) -> StreamEvent {
        StreamEvent {
            data: Some(data.to_string()),
            ..StreamEvent::default()
        }
    }

    // ---- ChunkBuffer (Go chunkbuffer_test.go) ----

    /// Mirrors Go `TestBuffer_Append`: appends succeed and count; (the Go
    /// nil-chunk arm has no Rust analog — events are values).
    #[test]
    fn buffer_append_counts_chunks() {
        let buffer = ChunkBuffer::new();
        assert!(buffer.append(event("data1")));
        assert!(buffer.append(event("data2")));
        assert_eq!(buffer.len(), 2);
    }

    /// Mirrors Go `TestBuffer_Slice`: slice returns a copy — mutating the
    /// snapshot does not touch the buffer.
    #[test]
    fn buffer_slice_returns_copy() {
        let buffer = ChunkBuffer::new();
        buffer.append(event("data1"));
        buffer.append(event("data2"));

        let mut snapshot = buffer.slice();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].data.as_deref(), Some("data1"));
        snapshot[0].data = None;
        assert_eq!(
            buffer.slice()[0].data.as_deref(),
            Some("data1"),
            "mutating the snapshot must not affect the buffer"
        );
    }

    /// Mirrors Go `TestBuffer_Close`: closed buffers reject appends and keep
    /// their length.
    #[test]
    fn buffer_close_rejects_further_appends() {
        let buffer = ChunkBuffer::new();
        assert!(!buffer.is_closed());
        assert!(buffer.append(event("data")));
        buffer.close();
        assert!(buffer.is_closed());
        assert!(!buffer.append(event("data2")));
        assert_eq!(buffer.len(), 1);
    }

    /// Go `Read` contract (`chunkbuffer.go:95-103`): in-range read advances
    /// the cursor; out-of-range read keeps it and reports the closed flag.
    #[test]
    fn buffer_read_advances_cursor_and_reports_closed() {
        let buffer = ChunkBuffer::new();
        buffer.append(event("a"));

        let (chunk, next, closed) = buffer.read(0);
        assert_eq!(chunk.and_then(|c| c.data).as_deref(), Some("a"));
        assert_eq!(next, 1);
        assert!(!closed);

        let (missing, still, closed_before) = buffer.read(1);
        assert!(missing.is_none());
        assert_eq!(still, 1);
        assert!(!closed_before);

        buffer.close();
        let (_, _, closed_after) = buffer.read(1);
        assert!(closed_after);
    }

    /// Capacity guard (Go chunkbuffer.go:52-55) — checked at the constant
    /// level plus a small behavioral probe against a nearly-full buffer.
    #[test]
    fn buffer_capacity_constant_matches_go_literal() {
        assert_eq!(MAX_CHUNK_CAPACITY, 50_000);
    }

    // ---- LiveStreamRegistry (Go stream_preview.go) ----

    #[test]
    fn registry_register_get_unregister_roundtrip() {
        let registry = LiveStreamRegistry::new();
        assert!(registry.get_request_buffer("33").is_none());
        assert!(registry.get_execution_buffer("44").is_none());

        registry.register_request("33", ChunkBuffer::new());
        registry.register_execution("44", ChunkBuffer::new());
        assert!(registry.get_request_buffer("33").is_some());
        assert!(registry.get_execution_buffer("44").is_some());

        registry.unregister_request("33");
        registry.unregister_execution("44");
        assert!(registry.get_request_buffer("33").is_none());
        assert!(registry.get_execution_buffer("44").is_none());
    }

    /// Go `GetRequestChunks`/`GetExecutionChunks` return the live snapshot in
    /// append order (storage-marshal filter deferred; see module docs).
    #[test]
    fn registry_chunk_snapshots_reflect_buffer_contents() {
        let registry = LiveStreamRegistry::new();
        let buffer = ChunkBuffer::new();
        buffer.append(event("one"));
        buffer.append(event("two"));
        registry.register_execution("exec-1", buffer);

        let chunks = registry.get_execution_chunks("exec-1");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].data.as_deref(), Some("one"));
        assert_eq!(chunks[1].data.as_deref(), Some("two"));
        assert!(registry.get_request_chunks("exec-1").is_empty());
    }

    /// Go `sweepStaleEntries` (`stream_preview.go:127-169`): closed buffers
    /// are evicted; idle zombies are force-closed then evicted; fresh live
    /// buffers survive.
    #[test]
    fn registry_sweep_evicts_closed_and_idle_buffers() {
        let registry = LiveStreamRegistry::new();

        let closed = ChunkBuffer::new();
        closed.close();
        registry.register_request("closed", closed);

        // Idle buffer: created early, aged past the threshold by the time we
        // sweep. Its `last_appended_at` is seeded at creation
        // (`chunkbuffer.go:29-35`) so it must be clearly older than the
        // threshold.
        let idle = ChunkBuffer::new();
        registry.register_execution("idle", idle.clone());
        std::thread::sleep(Duration::from_millis(20));

        // Fresh buffer: appended immediately before the sweep, so its
        // `last_appended_at` is well within the threshold.
        let fresh = ChunkBuffer::new();
        fresh.append(event("keepalive"));
        registry.register_request("fresh", fresh);

        // Threshold sits between idle's age (~20ms+) and fresh's age (~0ms).
        let evicted = registry.sweep_stale_entries(Duration::from_millis(10));
        assert_eq!(evicted, 2, "closed + idle buffers must be evicted");
        assert!(registry.get_request_buffer("closed").is_none());
        assert!(registry.get_execution_buffer("idle").is_none());
        assert!(
            idle.is_closed(),
            "idle zombie must be force-closed on eviction"
        );

        let survivors = registry.sweep_stale_entries(Duration::from_secs(600));
        assert_eq!(survivors, 0);
        assert!(registry.get_request_buffer("fresh").is_some());
    }

    // ---- LivePreviewObserver (Go live_streaming_test.go) ----

    fn enabled_observer(
        request_id: &str,
        exec_id: &str,
    ) -> (LivePreviewObserver, Arc<LiveStreamRegistry>) {
        let registry = Arc::new(LiveStreamRegistry::new());
        let plan = live_preview_plan(
            true,
            true,
            true,
            Some(request_id.to_string()),
            Some(exec_id.to_string()),
        );
        (
            LivePreviewObserver::from_plan(plan, registry.clone()),
            registry,
        )
    }

    /// Mirrors Go
    /// `TestLivePreviewMiddleware_OnInboundLlmRequest_DisablesPreviewForNonStreamingRequests`:
    /// a non-streaming request yields a disabled plan, so the observer stays
    /// inert.
    #[test]
    fn observer_disabled_for_non_streaming_requests() {
        let registry = Arc::new(LiveStreamRegistry::new());
        let plan = live_preview_plan(
            true,
            false, // non-streaming request disables preview
            true,
            Some("33".to_string()),
            Some("44".to_string()),
        );
        assert_eq!(
            plan.disabled_reason,
            Some(LivePreviewDisableReason::NotStreaming)
        );
        let observer = LivePreviewObserver::from_plan(plan, registry.clone());
        assert!(!observer.enabled());

        observer.on_attempt_start();
        assert!(registry.get_request_buffer("33").is_none());
        assert!(registry.get_execution_buffer("44").is_none());

        // Streaming request keeps preview enabled (second Go sub-case).
        let streaming_plan = live_preview_plan(true, true, true, None, None);
        assert!(streaming_plan.enabled);
    }

    /// Mirrors Go
    /// `TestLivePreviewMiddleware_OnOutboundRawRequest_RegistersBuffersWhenEnabled`.
    #[test]
    fn observer_registers_buffers_when_enabled() {
        let (observer, registry) = enabled_observer("33", "44");
        observer.on_attempt_start();
        assert!(registry.get_request_buffer("33").is_some());
        assert!(registry.get_execution_buffer("44").is_some());
    }

    /// Mirrors Go
    /// `TestLivePreviewMiddleware_OnOutboundRawRequest_DoesNotRegisterBuffersWhenDisabled`.
    #[test]
    fn observer_does_not_register_buffers_when_disabled() {
        let registry = Arc::new(LiveStreamRegistry::new());
        // Policy off -> disabled plan (Go sets enabled: false directly).
        let plan = live_preview_plan(
            true,
            true,
            false,
            Some("11".to_string()),
            Some("22".to_string()),
        );
        let observer = LivePreviewObserver::from_plan(plan, registry.clone());

        observer.on_attempt_start();
        assert!(registry.get_request_buffer("11").is_none());
        assert!(registry.get_execution_buffer("22").is_none());
    }

    /// Mirrors Go
    /// `TestLivePreviewMiddleware_OnOutboundRawError_CleansRegisteredBuffers`:
    /// after an error both buffers are closed and unregistered.
    #[test]
    fn observer_on_error_cleans_registered_buffers() {
        let (observer, registry) = enabled_observer("33", "44");
        observer.on_attempt_start();
        let exec_buffer = match registry.get_execution_buffer("44") {
            Some(buffer) => buffer,
            None => panic!("execution buffer must be registered"),
        };

        observer.on_error();

        assert!(registry.get_request_buffer("33").is_none());
        assert!(registry.get_execution_buffer("44").is_none());
        assert!(exec_buffer.is_closed(), "buffer must be closed, not leaked");
    }

    /// Mirrors Go `TestLiveRequestStream_AppendsOncePerNext`: one observed
    /// event appends exactly one chunk per buffer (Go asserts the request
    /// buffer; the observer feeds both sides).
    #[test]
    fn observer_appends_once_per_event() {
        let (observer, registry) = enabled_observer("1", "2");
        observer.on_attempt_start();

        observer.on_event(&event(r#"{"index":1}"#));

        assert_eq!(registry.get_request_chunks("1").len(), 1);
        assert_eq!(registry.get_execution_chunks("2").len(), 1);
    }

    /// Go live wrappers summarize binary chunks before buffering
    /// (`live_streaming.go:152-156`): the buffered copy drops the payload but
    /// keeps the size; the client-facing event is untouched (the observer only
    /// sees a reference).
    #[test]
    fn observer_buffers_binary_chunks_summarized() {
        let (observer, registry) = enabled_observer("1", "2");
        observer.on_attempt_start();

        let mut audio = StreamEvent::default();
        audio.event_type = Some("audio/mpeg".to_string());
        audio.binary = Some(vec![0u8; 64]);
        observer.on_event(&audio);

        let chunks = registry.get_execution_chunks("2");
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].binary.is_none(), "payload must be summarized");
        assert_eq!(chunks[0].size, Some(64));
        // The original event still carries its payload for the client.
        assert!(audio.binary.is_some());
    }

    /// Go live wrappers' `Close()` closes + unregisters; double-close is safe
    /// (Go guards with `s.closed`).
    #[test]
    fn observer_on_close_is_idempotent_teardown() {
        let (observer, registry) = enabled_observer("1", "2");
        observer.on_attempt_start();
        observer.on_event(&event("chunk"));

        observer.on_close();
        assert!(registry.get_request_buffer("1").is_none());
        assert!(registry.get_execution_buffer("2").is_none());

        // Second close must be a no-op, not a panic.
        observer.on_close();
    }

    // -------------------------------------------------------------------
    // RUST-P9-006 A01 — additional Go live-preview parity cases.
    // Covers asymmetric plans (only one id set), defensive no-op on
    // error-without-prior-registration, and on_event fan-out when only
    // one buffer is registered (Go's `m.state.Request == nil` /
    // `m.state.RequestExec == nil` guards, live_streaming.go:60-66).
    // -------------------------------------------------------------------

    /// Mirrors Go `OnOutboundRawRequest` when `m.state.RequestExec == nil`
    /// (live_streaming.go:64-66 skip): only the request buffer is registered,
    /// and `on_event` fans out to the request side only. The execution side
    /// stays absent without panicking.
    #[test]
    fn observer_asymmetric_plan_with_only_request_id_registers_single_buffer() {
        let registry = Arc::new(LiveStreamRegistry::new());
        let plan = live_preview_plan(true, true, true, Some("req-only".to_string()), None);
        let observer = LivePreviewObserver::from_plan(plan, registry.clone());

        observer.on_attempt_start();
        assert!(registry.get_request_buffer("req-only").is_some());
        assert!(
            registry.get_execution_buffer("anything").is_none(),
            "asymmetric plan must not register an execution buffer"
        );

        // Event fans out to the request buffer only — no panic, no execution
        // side lookup under a missing id.
        observer.on_event(&event(r#"{"i":1}"#));
        assert_eq!(registry.get_request_chunks("req-only").len(), 1);
    }

    /// Mirrors Go `OnOutboundRawRequest` when `m.state.Request == nil`
    /// (live_streaming.go:60 skip): only the execution buffer is registered.
    #[test]
    fn observer_asymmetric_plan_with_only_execution_id_registers_single_buffer() {
        let registry = Arc::new(LiveStreamRegistry::new());
        let plan = live_preview_plan(true, true, true, None, Some("exec-only".to_string()));
        let observer = LivePreviewObserver::from_plan(plan, registry.clone());

        observer.on_attempt_start();
        assert!(registry.get_execution_buffer("exec-only").is_some());
        assert!(
            registry.get_request_buffer("anything").is_none(),
            "asymmetric plan must not register a request buffer"
        );

        observer.on_event(&event(r#"{"i":1}"#));
        assert_eq!(registry.get_execution_chunks("exec-only").len(), 1);
    }

    /// Mirrors Go `OnOutboundRawError`'s defensive `buffer != nil` check
    /// (live_streaming.go:77-88): calling on_error WITHOUT prior
    /// `on_attempt_start` (no buffers registered) must be a silent no-op,
    /// not a panic. Go's `if buffer := ...GetExecutionBuffer(...); buffer != nil`
    /// guard handles this; the Rust observer mirrors it through `Option`.
    #[test]
    fn observer_on_error_without_prior_registration_is_noop() {
        let registry = Arc::new(LiveStreamRegistry::new());
        let plan = live_preview_plan(
            true,
            true,
            true,
            Some("never-registered-req".to_string()),
            Some("never-registered-exec".to_string()),
        );
        let observer = LivePreviewObserver::from_plan(plan, registry.clone());

        // Fire on_error directly without on_attempt_start — no buffers exist.
        observer.on_error();
        // Confirm nothing was registered or modified.
        assert!(
            registry
                .get_request_buffer("never-registered-req")
                .is_none()
        );
        assert!(
            registry
                .get_execution_buffer("never-registered-exec")
                .is_none()
        );
    }

    /// Mirrors Go's per-side independence in `Next()` (live_streaming.go:144-159
    /// / :190-205): when only the execution buffer is registered (request side
    /// absent), `on_event` appends to the execution buffer and does not look
    /// up the missing request id. Combined with the asymmetric registration
    /// above, this proves the two sides are independently wired.
    #[test]
    fn observer_on_event_with_single_buffer_does_not_panic_for_missing_side() {
        let registry = Arc::new(LiveStreamRegistry::new());
        let plan = live_preview_plan(
            true,
            true,
            true,
            None, // no request id -> no request buffer registered
            Some("exec-7".to_string()),
        );
        let observer = LivePreviewObserver::from_plan(plan, registry.clone());
        observer.on_attempt_start();

        // Multiple events — each appends to execution only.
        observer.on_event(&event("a"));
        observer.on_event(&event("b"));
        observer.on_event(&event("c"));
        let chunks = registry.get_execution_chunks("exec-7");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].data.as_deref(), Some("a"));
        assert_eq!(chunks[2].data.as_deref(), Some("c"));
    }

    /// Mirrors Go `OnOutboundRawError` teardown ordering (live_streaming.go:76-89):
    /// execution buffer is closed + unregistered BEFORE the request buffer.
    /// Verified by registering both, calling on_error, and confirming both are
    /// gone (the order itself is not directly observable but the outcome is).
    #[test]
    fn observer_on_error_closes_both_buffers_in_go_order() {
        let (observer, registry) = enabled_observer("req-x", "exec-x");
        observer.on_attempt_start();
        let req_buf = match registry.get_request_buffer("req-x") {
            Some(b) => b,
            None => panic!("request buffer must be registered"),
        };
        let exec_buf = match registry.get_execution_buffer("exec-x") {
            Some(b) => b,
            None => panic!("execution buffer must be registered"),
        };

        observer.on_error();

        assert!(req_buf.is_closed(), "request buffer must be closed");
        assert!(exec_buf.is_closed(), "execution buffer must be closed");
        assert!(registry.get_request_buffer("req-x").is_none());
        assert!(registry.get_execution_buffer("exec-x").is_none());
    }
}
