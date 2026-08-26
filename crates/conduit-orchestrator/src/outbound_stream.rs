//! RUST-P9-006 S15 (final sub-item) — streaming persistence wrapper.
//!
//! This module ports Go `OutboundPersistentStream` (the streaming-persistence
//! wrapper around the upstream LLM response stream) from
//! `conduit/internal/server/orchestrator/outbound.go:28-212`. The Go wrapper
//! has two responsibilities:
//!
//! 1. **Chunk accumulation** (`Current` / `Next`, outbound.go:75-94): every
//!    `*httpclient.StreamEvent` that flows through the stream is appended to an
//!    internal buffer (`ts.responseChunks`), and a terminal-event predicate
//!    (`isTerminalStreamEvent`) flips `ts.state.StreamCompleted` when the
//!    provider's "stream done" sentinel is observed (Chat Completions `[DONE]`,
//!    Responses `response.completed`, Anthropic `message_stop`, audio
//!    `*.done`, binary `binary.done`).
//!
//! 2. **Final persistence on `Close`** (outbound.go:100-212): inspect the
//!    observed state (terminal event seen? client disconnected? explicit stream
//!    error? aggregated response complete?) and route to either the success
//!    persist path (`persistResponseChunks` / `persistAggregatedResponse`) or
//!    the failure path (`UpdateRequestExecutionStatusFromError`).
//!
//! The pure decision layer ([`StreamFinalPlan`] + [`stream_final_plan`]) already
//! lives in [`crate::orchestrator`]. This module wires it to a real
//! [`RequestRecorder`] and a chunk-aggregation trait, mirroring the Go
//! `Close` body's branch semantics.
//!
//! # Design split from `request_recorder.rs`
//!
//! The non-streaming recorder ([`ProductionRequestRecorder`]) covers the
//! `OnOutboundLlmResponse` middleware path (single-shot response). The
//! streaming path differs in two ways:
//!
//! * It must **collect** chunks across the stream's lifetime before it can
//!   invoke `record_success` (which needs an aggregated `HttpResponse`).
//! * The Go `Close` body has *more* branches than the pure
//!   [`stream_final_plan`] two-boolean input — specifically the
//!   "aggregated-without-terminal-event" sub-branch (outbound.go:147-157) that
//!   can still promote an incomplete stream to `StreamCompleted`, and the
//!   "explicit-stream-error" branch (outbound.go:128-140) that bypasses the
//!   aggregation step entirely.
//!
//! This wrapper folds those branches into a single `close` entry-point that
//! produces the two booleans [`stream_final_plan`] expects, then delegates the
//! actual row writes to the recorder (so the wrapper stays I/O-free except for
//! the recorder call).
//!
//! # Testability
//!
//! Both collaborators are trait-based so tests inject fakes:
//! * [`StreamChunkAggregator`] — stands in for Go's `transformer.Outbound`.
//! * [`RequestRecorder`] — the existing trait from [`crate::orchestrator`].

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use conduit_core::ConduitError;
use conduit_llm::{HttpResponse, LlmResponse, StreamEvent};
use conduit_pipeline::pipeline::AttemptRecord as PipelineAttempt;
use conduit_transformers::{InboundTransformer, OpenAiChatInbound};
use tracing::warn;

use crate::orchestrator::{
    OrchestratorContext, RequestRecorder, STREAM_FINAL_NO_TERMINAL_EVENT_MESSAGE, StreamFinalPlan,
    stream_final_plan,
};

fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn stream_persistence_context(
    ctx: &OrchestratorContext,
    first_event_at_ms: Option<i64>,
    ended_at_ms: i64,
) -> OrchestratorContext {
    let mut persist_ctx = ctx.clone();
    let Some(started_at_ms) = ctx
        .metadata
        .get("perf_outbound_start_ms")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|started_at_ms| *started_at_ms > 0)
    else {
        return persist_ctx;
    };

    persist_ctx
        .metadata
        .insert("perf_stream_end_ms".to_string(), ended_at_ms.to_string());
    persist_ctx.metadata.insert(
        "perf_latency_ms".to_string(),
        ended_at_ms.saturating_sub(started_at_ms).max(0).to_string(),
    );
    if let Some(first_event_at_ms) = first_event_at_ms {
        persist_ctx.metadata.insert(
            "perf_first_event_ms".to_string(),
            first_event_at_ms.to_string(),
        );
        persist_ctx.metadata.insert(
            "first_token_latency_ms".to_string(),
            first_event_at_ms
                .saturating_sub(started_at_ms)
                .max(0)
                .to_string(),
        );
    }
    persist_ctx
}

fn apply_stream_timing_to_response(ctx: &OrchestratorContext, response: &mut HttpResponse) {
    for (context_key, response_key) in [
        ("perf_latency_ms", "latency_ms"),
        ("first_token_latency_ms", "first_token_latency_ms"),
    ] {
        if let Some(value) = ctx
            .metadata
            .get(context_key)
            .and_then(|value| value.parse::<i64>().ok())
        {
            response
                .metadata
                .entry(response_key.to_string())
                .or_insert_with(|| serde_json::Value::from(value));
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal-event predicate — port of Go `isTerminalStreamEvent`
// (conduit/internal/server/orchestrator/inbound.go:83-95, used by outbound.go:88).
// ---------------------------------------------------------------------------

/// The raw `[DONE]` sentinel payload Chat Completions streams terminate with.
/// Mirrors Go `llm.DoneStreamEvent.Data = []byte("[DONE]")`
/// (`conduit/llm/model.go:14-16`).
pub const DONE_STREAM_EVENT_DATA: &str = "[DONE]";

/// Marker `StreamEvent::event_type` value for non-SSE binary streams (TTS /
/// STT). Mirrors Go `httpclient.BinaryStreamDoneEventType = "binary.done"`
/// (`conduit/llm/httpclient/model.go:79`).
pub const BINARY_STREAM_DONE_EVENT_TYPE: &str = "binary.done";

/// Whether `event` represents the end of a successfully completed stream.
///
/// Port of Go `isTerminalStreamEvent` (`inbound.go:83-95`), which Go's
/// outbound stream invokes at `outbound.go:88`. Recognizes:
/// * Chat Completions `[DONE]` (matched on `event.data`);
/// * Responses API `response.completed`;
/// * Anthropic Messages `message_stop`;
/// * OpenAI audio APIs `speech.audio.done` / `transcript.text.done`;
/// * Non-SSE binary streams `binary.done`.
///
/// Matching is on the literal `event_type` / `data` strings; the Rust
/// `StreamEvent` keeps these as `Option<String>` (Go `*StreamEvent.Type` /
/// `.Data` are `[]byte` / `string`), so we treat `None` as the empty string.
pub fn is_terminal_stream_event(event: &StreamEvent) -> bool {
    let data = event.data.as_deref().unwrap_or("");
    let event_type = event.event_type.as_deref().unwrap_or("");
    data == DONE_STREAM_EVENT_DATA
        || event_type == "response.completed"
        || event_type == "message_stop"
        || event_type == "speech.audio.done"
        || event_type == "transcript.text.done"
        || event_type == BINARY_STREAM_DONE_EVENT_TYPE
}

/// Whether an aggregated response should be treated as "completed" even if no
/// terminal stream event was observed. Port of Go `isCompletedAggregated`
/// (`outbound.go:307-310`).
///
/// Go: `meta.Completed || (meta.Usage != nil && meta.Usage.CompletionTokens > 0)`.
/// Rust's `HttpResponse` carries the aggregated `Usage` directly and a
/// `metadata["completed"]` flag (set by the aggregator when the provider's
/// aggregated payload bore a `status:completed` / speech `Completed` flag).
pub fn is_completed_aggregated(response: &HttpResponse) -> bool {
    if response
        .metadata
        .get("completed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    matches!(response.usage.as_ref(), Some(u) if u.completion_tokens > 0)
}

// ---------------------------------------------------------------------------
// StreamChunkAggregator — stands in for Go `transformer.Outbound`.
// ---------------------------------------------------------------------------

/// Error returned by [`StreamChunkAggregator::aggregate`]. The wrapper treats
/// aggregation failure as "incomplete" (mirrors Go: `aggErr == nil &&
/// isCompletedAggregated(meta)` at outbound.go:149).
#[derive(Debug, thiserror::Error)]
#[error("stream-chunk aggregation error: {0}")]
pub struct StreamAggregationError(pub String);

/// Turns a slice of buffered stream chunks into the aggregated
/// [`HttpResponse`] the recorder consumes on the success path.
///
/// Mirrors Go `transformer.Outbound.AggregateStreamChunks` as called by
/// `OutboundPersistentStream.persistResponseChunks` /
/// `.persistAggregatedResponse` (`outbound.go:249` / `:259`). The trait keeps
/// the wrapper I/O-free and testable: production wires a real transformer
/// adapter; tests inject a fake that returns a fixed [`HttpResponse`].
#[async_trait]
pub trait StreamChunkAggregator: Send + Sync {
    /// Aggregate `chunks` into a single [`HttpResponse`]. The returned response
    /// carries the merged `body` / `json_body` / `usage` / external id the
    /// recorder persists. Errors surface as
    /// [`StreamAggregationError`] and cause the wrapper to treat the stream as
    /// incomplete (mirrors Go's `aggErr != nil` branch at outbound.go:149).
    async fn aggregate(
        &self,
        ctx: &OrchestratorContext,
        chunks: &[StreamEvent],
    ) -> Result<HttpResponse, StreamAggregationError>;
}

// ---------------------------------------------------------------------------
// TransformerStreamAggregator — production aggregator over an inbound transformer.
// ---------------------------------------------------------------------------

/// Production [`StreamChunkAggregator`] that folds buffered chunks by reusing an
/// [`InboundTransformer`]'s `aggregate_stream_chunks` (Go
/// `transformer.Inbound.AggregateStreamChunks`), then **lifts the aggregated
/// usage + response id onto the [`HttpResponse`]** so the finalizer's
/// completion check ([`is_completed_aggregated`]) and the recorder's usage-log
/// write ([`crate::request_recorder::ProductionRequestRecorder::record_stream_final`]
/// / `record_success`) can see them.
///
/// # Why the lift is necessary
///
/// `OpenAiChatInbound::aggregate_stream_chunks` (openai.rs:1065) serializes the
/// merged `LlmResponse` — including its last-wins `usage` — into
/// `HttpResponse.body`, but leaves the typed `HttpResponse.usage` field `None`.
/// The persistence layer reads `HttpResponse.usage` (not the body), so without
/// this lift a completed stream would persist zero tokens. We deserialize the
/// aggregated body back into an [`LlmResponse`] and copy `usage` + `id` onto the
/// response.
///
/// # Format scope
///
/// [`Self::openai`] wires the OpenAI Chat Completions aggregator (the flagship
/// `/v1/chat/completions` streaming path). Anthropic / Gemini streams need the
/// matching inbound transformer via [`Self::new`] — a documented follow-up.
pub struct TransformerStreamAggregator {
    inbound: Arc<dyn InboundTransformer>,
}

impl TransformerStreamAggregator {
    /// Wire an aggregator over an explicit inbound transformer.
    pub fn new(inbound: Arc<dyn InboundTransformer>) -> Self {
        Self { inbound }
    }

    /// Wire the OpenAI Chat Completions aggregator (the streaming flagship).
    pub fn openai() -> Self {
        Self::new(Arc::new(OpenAiChatInbound::new()))
    }
}

#[async_trait]
impl StreamChunkAggregator for TransformerStreamAggregator {
    async fn aggregate(
        &self,
        _ctx: &OrchestratorContext,
        chunks: &[StreamEvent],
    ) -> Result<HttpResponse, StreamAggregationError> {
        let mut response = self
            .inbound
            .aggregate_stream_chunks(chunks.to_vec())
            .map_err(|err| StreamAggregationError(err.to_string()))?;

        // Lift usage + response id from the serialized aggregate onto the typed
        // HttpResponse fields the persistence layer reads.
        if let Some(body) = response.body.as_deref()
            && let Ok(llm) = serde_json::from_slice::<LlmResponse>(body)
        {
            if response.usage.is_none() {
                response.usage = llm.usage.clone();
            }
            if !llm.id.is_empty() {
                response
                    .metadata
                    .entry("llm_response_id".to_string())
                    .or_insert_with(|| serde_json::Value::String(llm.id.clone()));
            }
        }
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// StreamChunkBuffer — analog of Go `ts.responseChunks`.
// ---------------------------------------------------------------------------
/// In-memory accumulator for the [`StreamEvent`]s observed during a stream.
///
/// Mirrors Go `OutboundPersistentStream.responseChunks`
/// (`outbound.go:40`). The buffer is filled by [`Self::push`] (called from the
/// wrapper's `on_event`) and drained by the close-time aggregation step.
///
/// Go also applies `httpclient.SummarizeBinaryChunk` before appending
/// (`outbound.go:84`): raw binary audio chunks are replaced with a size-only
/// summary to avoid buffering the full payload in memory. We mirror that here
/// via [`summarize_binary_chunk`].
#[derive(Debug, Default, Clone)]
pub struct StreamChunkBuffer {
    chunks: Vec<StreamEvent>,
    /// Whether a terminal event ([`is_terminal_stream_event`]) has been
    /// observed. Mirrors Go `ts.state.StreamCompleted`.
    stream_completed: bool,
}

impl StreamChunkBuffer {
    /// Create an empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe a chunk: append a (binary-summarized) copy and update the
    /// terminal-event flag. Mirrors Go `Current()` body at outbound.go:80-93.
    pub fn push(&mut self, event: &StreamEvent) {
        self.chunks.push(summarize_binary_chunk(event));
        if is_terminal_stream_event(event) {
            self.stream_completed = true;
        }
    }

    /// Whether a terminal stream event has been observed. Mirrors Go
    /// `ts.state.StreamCompleted`.
    pub fn stream_completed(&self) -> bool {
        self.stream_completed
    }

    /// Promote the buffer to "completed" status. Mirrors Go
    /// `ts.state.StreamCompleted = true` at outbound.go:153 (the
    /// aggregated-without-terminal-event branch).
    pub fn mark_completed(&mut self) {
        self.stream_completed = true;
    }

    /// Number of buffered chunks.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Read access to the buffered chunks (for aggregation / assertion).
    pub fn chunks(&self) -> &[StreamEvent] {
        &self.chunks
    }
}

/// Replace raw binary chunk payloads with a size-only summary. Port of Go
/// `httpclient.SummarizeBinaryChunk` (used at outbound.go:84).
///
/// The Go helper drops the binary payload and keeps only its byte length (so
/// the buffered chunk does not hold the full audio/video in memory). We mirror
/// that: if `event.binary` is `Some`, the summarized copy keeps the
/// `binary.len()` in `StreamEvent::size` and clears the `binary` field. Non-
/// binary chunks pass through unchanged.
pub fn summarize_binary_chunk(event: &StreamEvent) -> StreamEvent {
    if let Some(bin) = event.binary.as_ref() {
        let mut summary = event.clone();
        summary.size = Some(bin.len());
        summary.binary = None;
        return summary;
    }
    event.clone()
}

// ---------------------------------------------------------------------------
// ObservedStreamState — inputs the wiring layer hands to the finalizer at close.
// ---------------------------------------------------------------------------

/// The observed end-state of a stream, as seen by the wiring layer at close
/// time. This is the Rust analog of the locals Go's `Close` body reads off the
/// stream / context (`outbound.go:110-111`, `:128`, `:160`).
///
/// The finalizer folds these into the two booleans [`stream_final_plan`]
/// expects. Keeping the inputs separate (rather than pre-computing the
/// booleans) preserves Go's branch ordering — specifically the rule that an
/// explicit non-cancel stream error bypasses aggregation entirely
/// (outbound.go:128-140) and the rule that a successful aggregation promotes
/// an incomplete stream to completed (outbound.go:151-154).
#[derive(Debug, Clone, Default)]
pub struct ObservedStreamState {
    /// Whether the request context was canceled. Mirrors Go `ctxErr :=
    /// ctx.Err()` (outbound.go:111). True on `tokio::task::JoinError::
    /// is_cancelled()` / cancel-token fire.
    pub client_disconnected: bool,
    /// Whether the stream surfaced an error. Mirrors Go `streamErr :=
    /// ts.stream.Err()` (outbound.go:110). When this is `Some` and
    /// [`Self::client_disconnected`] is false, the wrapper treats the stream
    /// as a hard upstream failure (outbound.go:128-140).
    pub stream_error: Option<String>,
}

// ---------------------------------------------------------------------------
// PersistentStreamFinalizer — wires the buffer + aggregator + recorder into
// the close-time persist step.
// ---------------------------------------------------------------------------

/// Streaming persistence wrapper. Ports Go `OutboundPersistentStream.Close`
/// (`outbound.go:100-212`).
///
/// Holds everything the close-time persist step needs:
/// * the [`StreamChunkBuffer`] filled as chunks flow through the stream;
/// * the [`StreamChunkAggregator`] (Go `transformer.Outbound`) used to build
///   the success-path [`HttpResponse`];
/// * the [`RequestRecorder`] that writes the DB rows;
/// * the request identity triple (`request_id` / `project_id` / `attempt`)
///   passed through to the recorder.
///
/// The finalizer is **not** the stream itself — the live stream loop (in the
/// pipeline wiring) is expected to call [`Self::on_event`] for each chunk and
/// [`Self::close`] once when the stream ends. This split keeps the wrapper
/// free of any particular streaming primitive (Go `streams.Stream` vs Rust
/// `futures::Stream`).
pub struct PersistentStreamFinalizer {
    buffer: StreamChunkBuffer,
    aggregator: Arc<dyn StreamChunkAggregator>,
    recorder: Arc<dyn RequestRecorder>,
    request_id: String,
    project_id: String,
    attempt: PipelineAttempt,
    /// The per-attempt execution row id (Go `ts.requestExec.ID`). When set,
    /// `close` additionally routes the execution-row semantics of Go
    /// `OutboundPersistentStream.Close` through
    /// [`RequestRecorder::record_stream_final`]:
    /// * success → chunk persistence (`SaveRequestExecutionChunks`,
    ///   outbound.go:302);
    /// * cancel/failure → the execution status write
    ///   (`UpdateRequestExecutionStatusFromError`, outbound.go:134/174/189).
    /// `None` mirrors Go's `ts.requestExec == nil` guard (execution-row writes
    /// are skipped entirely).
    execution_id: Option<String>,
}

impl PersistentStreamFinalizer {
    /// Wire a finalizer. Production constructs this once per stream (mirrors
    /// Go `NewOutboundPersistentStream` at outbound.go:47-73, minus the stream
    /// handle — the Rust wiring owns the stream loop).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        aggregator: Arc<dyn StreamChunkAggregator>,
        recorder: Arc<dyn RequestRecorder>,
        request_id: impl Into<String>,
        project_id: impl Into<String>,
        attempt: PipelineAttempt,
    ) -> Self {
        Self {
            buffer: StreamChunkBuffer::new(),
            aggregator,
            recorder,
            request_id: request_id.into(),
            project_id: project_id.into(),
            attempt,
            execution_id: None,
        }
    }

    /// Attach the per-attempt execution row id so `close` also performs the
    /// Go execution-row writes (see the field docs on
    /// [`Self`]::execution_id`). Mirrors Go passing `state.RequestExec` into
    /// `NewOutboundPersistentStream` (outbound.go:52).
    pub fn with_execution_id(mut self, execution_id: impl Into<String>) -> Self {
        self.execution_id = Some(execution_id.into());
        self
    }

    /// Observe a chunk flowing through the stream. The wiring layer calls this
    /// from its stream-loop's per-event handler. Mirrors Go `Current()` body
    /// at outbound.go:79-94 (append summarized copy + terminal-event check).
    pub fn on_event(&mut self, event: &StreamEvent) {
        self.buffer.push(event);
    }

    /// Read-only view of the buffered chunks (for assertions / aggregation).
    pub fn buffer(&self) -> &StreamChunkBuffer {
        &self.buffer
    }

    /// Close the stream and run final persistence.
    ///
    /// Mirrors Go `OutboundPersistentStream.Close` (outbound.go:100-212). The
    /// method:
    ///
    /// 1. Resolves whether the stream "completed normally" — this is the OR
    ///    of (a) a terminal event being observed during the stream
    ///    ([`StreamChunkBuffer::stream_completed`]) and (b) the aggregator
    ///    producing a complete [`HttpResponse`] from the buffered chunks
    ///    (Go outbound.go:147-157). If (b) promotes the stream to completed,
    ///    we update the buffer's flag to match Go's
    ///    `ts.state.StreamCompleted = true` (outbound.go:153).
    /// 2. Computes the [`StreamFinalPlan`] via [`stream_final_plan`] (the
    ///    pure helper from [`crate::orchestrator`]).
    /// 3. Delegates to the recorder:
    ///    * `plan.is_completed()` → `record_success` (with the aggregated
    ///      [`HttpResponse`] — Go's `persistResponseChunks` /
    ///      `persistAggregatedResponse` path).
    ///    * otherwise → `record_failure` (Go's
    ///      `UpdateRequestExecutionStatusFromError` path). The error message
    ///      prefers an explicit `stream_error`, then the plan's sentinel
    ///      (Go outbound.go:166-171, 187).
    ///
    /// Aggregation is **skipped** entirely when the stream already saw a
    /// terminal event AND the buffer is non-empty but we still need the
    /// aggregated body to hand to the recorder — so in fact we always aggregate
    /// on the would-be-success path (Go's `persistResponseChunks` does the
    /// same: it re-aggregates internally even when `StreamCompleted` is true,
    /// outbound.go:236-257). When aggregation fails on the success path we
    /// fall through to the failure arm (Go `log.Warn` + `return`,
    /// outbound.go:251-253).
    ///
    /// Returns the recorder's outcome (the recorder surfaces ConduitError on
    /// persistent-row write failure, matching Go's `log.Warn` + `return err`
    /// contract for the orchestrator's Persist stage).
    pub async fn close(
        &mut self,
        ctx: &OrchestratorContext,
        observed: &ObservedStreamState,
    ) -> Result<StreamFinalPlan, ConduitError> {
        // ---- Step 1: resolve completed_normally -----------------------------
        // Go first short-circuits on StreamCompleted (outbound.go:116-123),
        // then on explicit stream error (outbound.go:128-140), then attempts
        // aggregation (outbound.go:147-157). We preserve that ordering: if
        // the terminal-event flag is already set we treat the stream as
        // completed without consulting `stream_error`. Otherwise, if there's
        // a hard (non-cancel) stream error we never reach aggregation.
        let terminal_seen = self.buffer.stream_completed();

        let mut completed_normally = terminal_seen;
        let mut aggregated: Option<HttpResponse> = None;

        // Go's terminal-seen branch fires the success persist directly, so we
        // only run the aggregation step when we still need to resolve
        // completion (i.e. no terminal event yet). We also aggregate when the
        // terminal event *was* seen so we have a body to hand to
        // `record_success` (Go does this inside `persistResponseChunks`,
        // outbound.go:249).
        if !completed_normally {
            // Go outbound.go:128-140: explicit non-cancel stream error short-
            // circuits the aggregation attempt. We mirror that by only
            // aggregating when there is no hard stream error.
            let hard_stream_error = observed
                .stream_error
                .as_deref()
                .map(|_| !observed.client_disconnected)
                .unwrap_or(false);
            if !hard_stream_error && !self.buffer.is_empty() {
                match self.aggregator.aggregate(ctx, self.buffer.chunks()).await {
                    Ok(response) => {
                        if is_completed_aggregated(&response) {
                            // Go outbound.go:152-154: aggregated response
                            // looks complete -> promote StreamCompleted.
                            completed_normally = true;
                            self.buffer.mark_completed();
                        }
                        aggregated = Some(response);
                    }
                    Err(agg_err) => {
                        // Go outbound.go:250-253: log Warn + return (no
                        // persist). The wrapper falls through to the failure
                        // arm with the aggregation error as the recorded
                        // message.
                        warn!(
                            error = %agg_err,
                            "outbound-stream: aggregation failed, treating as incomplete"
                        );
                    }
                }
            }
        } else {
            // Terminal event seen: aggregate so we have a body for the
            // success recorder call. Mirrors Go `persistResponseChunks`
            // (outbound.go:236-257) which re-aggregates on the success path.
            if !self.buffer.is_empty() {
                match self.aggregator.aggregate(ctx, self.buffer.chunks()).await {
                    Ok(response) => {
                        aggregated = Some(response);
                    }
                    Err(agg_err) => {
                        // Go logs Warn + returns from persistResponseChunks
                        // without flipping the row. The stream is still
                        // "completed" (the terminal event was real), so we
                        // keep completed_normally=true but have no body — we
                        // synthesize an empty success response so the recorder
                        // still runs. Go's persistAggregatedResponse would
                        // skip the body write when responseBody is empty
                        // (outbound.go:259-305 is gated on meta.Usage / being
                        // called at all); the closest Rust equivalent is to
                        // record success with an empty HttpResponse.
                        warn!(
                            error = %agg_err,
                            "outbound-stream: terminal-event aggregation failed, recording success with empty body"
                        );
                    }
                }
            }
        }

        // ---- Step 2: compute the plan ---------------------------------------
        let client_disconnected = observed.client_disconnected;
        let plan = stream_final_plan(completed_normally, client_disconnected);

        // ---- Step 3: delegate to the recorder -------------------------------
        // Ordering mirrors the Go close cascade: `InboundPersistentStream.Close`
        // persists the request row FIRST, then `return ts.stream.Close()`
        // cascades into `OutboundPersistentStream.Close` which persists the
        // execution row (inbound.go:101-205 → outbound.go:100-212).
        if plan.is_completed() {
            // Success path. The recorder's record_success takes the aggregated
            // HttpResponse (or an empty default when aggregation did not yield
            // one — e.g. terminal event with no chunks, which Go handles by
            // skipping the persist entirely; we mirror by passing an empty
            // response the recorder will treat as "no body").
            let mut response = aggregated.unwrap_or_default();
            apply_stream_timing_to_response(ctx, &mut response);
            self.recorder
                .record_success(
                    ctx,
                    &self.request_id,
                    &self.project_id,
                    &self.attempt,
                    &response,
                )
                .await?;

            if let Some(execution_id) = self.execution_id.as_deref() {
                // Go inbound success → SaveRequestChunks (inbound.go:254).
                self.recorder
                    .record_stream_request_chunks(
                        ctx,
                        &self.request_id,
                        &self.project_id,
                        self.buffer.chunks(),
                    )
                    .await?;
                // Go outbound success → SaveRequestExecutionChunks
                // (outbound.go:302). `aggregated: None` routes the recorder to
                // the chunks-only write — record_success above already flipped
                // the execution row + wrote the usage log, so passing the
                // aggregated response again would double-write both (Go writes
                // each exactly once).
                self.recorder
                    .record_stream_final(
                        ctx,
                        &self.request_id,
                        &self.project_id,
                        &plan,
                        Some(execution_id),
                        None,
                        self.buffer.chunks(),
                    )
                    .await?;
            }
        } else {
            // Failure path. Build the ConduitError the recorder will persist.
            // Error-message precedence mirrors Go outbound.go:165-171:
            //   stream_error > ctx_error > "stream ended without terminal event
            //   or completed response".
            // The pure plan already encodes that precedence for the cancel /
            // no-terminal arms; when the wiring layer observed an explicit
            // stream_error we substitute it (overwriting the plan's sentinel),
            // matching Go's `errToReport = streamErr` branch.
            let message = observed
                .stream_error
                .clone()
                .or_else(|| plan.error_message.clone())
                .unwrap_or_else(|| STREAM_FINAL_NO_TERMINAL_EVENT_MESSAGE.to_string());
            let error = if plan.is_canceled() {
                // Cancel arm: surface as an internal "canceled" error so the
                // recorder's is_canceled_error marker matches (it greps for
                // "cancel" in the message). The plan already carries the
                // "context canceled" default.
                ConduitError::internal(message)
            } else {
                // Failed arm: surface as an upstream error so the recorder
                // records the FAILURE_PERSISTENCE_TERMINAL_STATUS (Failed).
                ConduitError::upstream(message)
            };
            // Request row (Go InboundPersistentStream.Close →
            // UpdateRequestStatusFromError, inbound.go:157-172: canceled ctx →
            // StatusCanceled via biz/request.go:1057-1058).
            self.recorder
                .record_failure(ctx, &self.request_id, &self.project_id, &error)
                .await?;

            // Execution row (Go OutboundPersistentStream.Close →
            // UpdateRequestExecutionStatusFromError, outbound.go:160-195:
            // canceled ctx → StatusCanceled via biz/request.go:791-792).
            if let Some(execution_id) = self.execution_id.as_deref() {
                self.recorder
                    .record_stream_final(
                        ctx,
                        &self.request_id,
                        &self.project_id,
                        &plan,
                        Some(execution_id),
                        None,
                        &[],
                    )
                    .await?;
            }
        }

        Ok(plan)
    }
}

// ---------------------------------------------------------------------------
// OutboundForwardingStream — S38: forward-while-aggregating tokio loop.
// ---------------------------------------------------------------------------

/// One item flowing from the upstream provider into the forward loop.
///
/// Go models this as `streams.Stream.Next()/Current()` + a terminal `Err()`;
/// the Rust loop consumes a channel where a provider error arrives as the
/// final [`UpstreamItem::Error`] item (after which the sender closes).
#[derive(Debug)]
pub enum UpstreamItem {
    /// A provider stream event (Go `stream.Current()`).
    Event(StreamEvent),
    /// The stream failed with this message (Go `stream.Err() != nil`).
    Error(ConduitError),
}

/// S38 — the streaming forward loop. Ports the *runtime* half of Go
/// `OutboundPersistentStream`/`InboundPersistentStream` (the `Next`/`Current`
/// forwarding that buffers each chunk on the way through) plus the
/// client-disconnect semantics of `cancelOnCloseStream`
/// (`llm/pipeline/stream.go:109-132`):
///
/// * every upstream event is buffered into the [`PersistentStreamFinalizer`]
///   (Go `ts.responseChunks` append at outbound.go:84 / inbound.go:72),
///   fanned out to the mounted [`StreamObserver`]s (S37 — live preview), and
///   then forwarded to the client channel — forward-while-aggregating;
/// * when the client drops its receiver mid-stream (Go: response writer gone
///   → request ctx canceled), the loop fires `upstream_cancel` (Go: the
///   per-attempt stream ctx cancel that aborts the provider HTTP call) and
///   finalizes with `client_disconnected = true`, which lands the request row
///   in `Cancelled` (Go `UpdateRequestStatusFromError` with
///   `context.Canceled`, biz/request.go:1057-1058) and the execution row in
///   `Cancelled` (biz/request.go:791-792);
/// * an [`UpstreamItem::Error`] finalizes with that message as the stream
///   error (Go `streamErr`), landing both rows in `Failed`;
/// * natural upstream end finalizes normally — the terminal-event /
///   aggregated-completion decision tree in [`PersistentStreamFinalizer::close`]
///   applies unchanged.
pub struct OutboundForwardingStream {
    upstream_rx: tokio::sync::mpsc::Receiver<UpstreamItem>,
    client_tx: tokio::sync::mpsc::Sender<Result<StreamEvent, ConduitError>>,
    /// Cancels the upstream provider call on client disconnect. Mirrors the
    /// per-attempt child token Go creates at `stream.go:35`
    /// (`context.WithCancel`) and fires from `cancelOnCloseStream.Close`.
    upstream_cancel: conduit_pipeline::CancelToken,
    finalizer: PersistentStreamFinalizer,
    /// S37 mount point: cross-cutting observers (live preview, …) attached to
    /// the loop instead of provider transformers.
    observers: Vec<Arc<dyn crate::live_streaming::StreamObserver>>,
}

impl OutboundForwardingStream {
    /// Wire a forward loop. `upstream_rx` carries provider events;
    /// `client_tx` is the client-facing channel (dropping its receiver is the
    /// client disconnect signal); `upstream_cancel` aborts the provider call.
    pub fn new(
        upstream_rx: tokio::sync::mpsc::Receiver<UpstreamItem>,
        client_tx: tokio::sync::mpsc::Sender<Result<StreamEvent, ConduitError>>,
        upstream_cancel: conduit_pipeline::CancelToken,
        finalizer: PersistentStreamFinalizer,
    ) -> Self {
        Self {
            upstream_rx,
            client_tx,
            upstream_cancel,
            finalizer,
            observers: Vec::new(),
        }
    }

    /// Mount a stream observer (S37). Observers see every event before it is
    /// forwarded and get a close notification when the stream ends.
    pub fn with_observer(
        mut self,
        observer: Arc<dyn crate::live_streaming::StreamObserver>,
    ) -> Self {
        self.observers.push(observer);
        self
    }

    /// Drive the loop to completion and run final persistence. Returns the
    /// resolved [`StreamFinalPlan`] (or the recorder's persist error).
    pub async fn run(mut self, ctx: &OrchestratorContext) -> Result<StreamFinalPlan, ConduitError> {
        // Go `OnOutboundRawRequest` fires before events flow — observers
        // register their live-preview buffers here (live_streaming.go:55-69).
        for observer in &self.observers {
            observer.on_attempt_start();
        }

        let mut client_disconnected = false;
        let mut stream_error: Option<String> = None;
        let mut first_event_at_ms: Option<i64> = None;

        loop {
            let item = tokio::select! {
                biased;
                _ = self.client_tx.closed() => {
                    self.upstream_cancel.cancel();
                    client_disconnected = true;
                    break;
                }
                item = self.upstream_rx.recv() => item,
            };
            let Some(item) = item else {
                break;
            };
            match item {
                UpstreamItem::Event(event) => {
                    first_event_at_ms.get_or_insert_with(epoch_millis);
                    // Buffer first (summarized copy + terminal-event check,
                    // outbound.go:80-93), fan out to observers, then forward
                    // the UNMODIFIED event to the client (Go returns the
                    // original from Current()).
                    self.finalizer.on_event(&event);
                    for observer in &self.observers {
                        observer.on_event(&event);
                    }
                    if self.client_tx.send(Ok(event)).await.is_err() {
                        // Client receiver dropped mid-stream. Go: the request
                        // ctx cancels; cancelOnCloseStream fires the stream
                        // ctx cancel which aborts the upstream HTTP request.
                        self.upstream_cancel.cancel();
                        client_disconnected = true;
                        break;
                    }
                }
                UpstreamItem::Error(err) => {
                    // Go `stream.Err()` became non-nil; the stream yields no
                    // further provider events. Preserve the typed error for
                    // the route-aware HTTP writer instead of turning it into
                    // a silent EOF.
                    stream_error = Some(err.message.clone());
                    let _ = self.client_tx.send(Err(err)).await;
                    break;
                }
            }
        }

        // Go live wrappers' Close() runs on every teardown path
        // (live_streaming.go:169-179 / :215-225).
        for observer in &self.observers {
            observer.on_close();
        }

        let observed = ObservedStreamState {
            client_disconnected,
            stream_error,
        };
        let persist_ctx = stream_persistence_context(ctx, first_event_at_ms, epoch_millis());
        self.finalizer.close(&persist_ctx, &observed).await
    }
}

// ===========================================================================
// Tests — mirror the Go `outbound_test.go` close-branch coverage
// (`TestOutboundPersistentStream_Close_AggregatedResponsesCompletionHandling`
//  at outbound_test.go:623+, plus the `TestIsCompletedAggregatedOutboundResponse`
//  table at outbound_test.go:564-588) and the S38 forward-loop semantics
// (client drop → canceled; mid-stream error → failed).
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::NoopRequestRecorder;
    use async_trait::async_trait;
    use conduit_core::ErrorKind;
    use conduit_llm::Usage;
    use conduit_pipeline::pipeline::ExecutionMode;
    use conduit_services::request_service::InMemoryRequestPersistenceRepo;
    use conduit_services::{RequestService, RequestStatus};
    use serde_json::json;
    use std::sync::Mutex;

    /// Capturing recorder: records every call's (request_id, kind, body) so
    /// tests can assert which arm fired. Mirrors the role of Go's
    /// `mockRequestService` (outbound_test.go) but at the recorder boundary
    /// rather than the service boundary, so we exercise the wrapper's
    /// plan-routing logic.
    #[derive(Debug, Default, Clone)]
    struct CapturingRecorder {
        successes: Arc<Mutex<Vec<CapturedSuccess>>>,
        failures: Arc<Mutex<Vec<CapturedFailure>>>,
    }

    #[derive(Debug, Clone)]
    struct CapturedSuccess {
        request_id: String,
        project_id: String,
        response: HttpResponse,
    }

    #[derive(Debug, Clone)]
    struct CapturedFailure {
        error_message: String,
        kind: ErrorKind,
    }

    impl CapturingRecorder {
        fn success_count(&self) -> usize {
            self.successes.lock().map(|g| g.len()).unwrap_or(0)
        }
        fn failure_count(&self) -> usize {
            self.failures.lock().map(|g| g.len()).unwrap_or(0)
        }
        fn last_success(&self) -> Option<CapturedSuccess> {
            self.successes.lock().ok()?.last().cloned()
        }
        fn last_failure(&self) -> Option<CapturedFailure> {
            self.failures.lock().ok()?.last().cloned()
        }
    }

    #[async_trait]
    impl RequestRecorder for CapturingRecorder {
        async fn record_success(
            &self,
            _ctx: &OrchestratorContext,
            request_id: &str,
            project_id: &str,
            _attempt: &PipelineAttempt,
            response: &HttpResponse,
        ) -> Result<(), ConduitError> {
            if let Ok(mut g) = self.successes.lock() {
                g.push(CapturedSuccess {
                    request_id: request_id.to_string(),
                    project_id: project_id.to_string(),
                    response: response.clone(),
                });
            }
            Ok(())
        }
        async fn record_failure(
            &self,
            _ctx: &OrchestratorContext,
            _request_id: &str,
            _project_id: &str,
            error: &ConduitError,
        ) -> Result<(), ConduitError> {
            if let Ok(mut g) = self.failures.lock() {
                g.push(CapturedFailure {
                    error_message: error.message.clone(),
                    kind: error.kind,
                });
            }
            Ok(())
        }
    }

    /// Fake aggregator: returns a pre-set response (or a configurable error).
    struct FakeAggregator {
        response: HttpResponse,
        fail: bool,
    }

    #[async_trait]
    impl StreamChunkAggregator for FakeAggregator {
        async fn aggregate(
            &self,
            _ctx: &OrchestratorContext,
            _chunks: &[StreamEvent],
        ) -> Result<HttpResponse, StreamAggregationError> {
            if self.fail {
                return Err(StreamAggregationError(
                    "fake aggregation failure".to_string(),
                ));
            }
            Ok(self.response.clone())
        }
    }

    fn attempt(seq: u32) -> PipelineAttempt {
        PipelineAttempt {
            sequence: seq,
            channel_id: "ch-1".to_string(),
            model_index: 0,
            mode: ExecutionMode::Stream,
            outcome: Ok(HttpResponse::default()),
        }
    }

    fn usage_with_completion(completion: u64) -> Usage {
        Usage {
            prompt_tokens: 10,
            completion_tokens: completion,
            total_tokens: 10 + completion,
            ..Usage::default()
        }
    }

    fn response_with_completion(completion: u64) -> HttpResponse {
        let mut r = HttpResponse::default();
        r.status = 200;
        r.usage = Some(usage_with_completion(completion));
        r.json_body = Some(json!({"id":"resp_1","choices":[]}));
        r
    }

    fn event_with_data(data: &str) -> StreamEvent {
        StreamEvent {
            data: Some(data.to_string()),
            ..StreamEvent::default()
        }
    }

    fn event_with_type(t: &str) -> StreamEvent {
        StreamEvent {
            event_type: Some(t.to_string()),
            ..StreamEvent::default()
        }
    }

    // ---- terminal-event predicate (Go `isTerminalStreamEvent`) ----

    #[test]
    fn is_terminal_stream_event_matches_done_sentinel() {
        assert!(is_terminal_stream_event(&event_with_data("[DONE]")));
    }

    #[test]
    fn is_terminal_stream_event_matches_response_completed() {
        assert!(is_terminal_stream_event(&event_with_type(
            "response.completed"
        )));
    }

    #[test]
    fn is_terminal_stream_event_matches_message_stop() {
        assert!(is_terminal_stream_event(&event_with_type("message_stop")));
    }

    #[test]
    fn is_terminal_stream_event_matches_speech_audio_done() {
        assert!(is_terminal_stream_event(&event_with_type(
            "speech.audio.done"
        )));
    }

    #[test]
    fn is_terminal_stream_event_matches_transcript_text_done() {
        assert!(is_terminal_stream_event(&event_with_type(
            "transcript.text.done"
        )));
    }

    #[test]
    fn is_terminal_stream_event_matches_binary_done() {
        assert!(is_terminal_stream_event(&event_with_type(
            BINARY_STREAM_DONE_EVENT_TYPE
        )));
    }

    #[test]
    fn is_terminal_stream_event_rejects_delta_events() {
        assert!(!is_terminal_stream_event(&event_with_type(
            "response.output_text.delta"
        )));
        assert!(!is_terminal_stream_event(&event_with_data("not-done")));
        assert!(!is_terminal_stream_event(&StreamEvent::default()));
    }

    // ---- is_completed_aggregated (Go `isCompletedAggregated`,
    //      outbound_test.go:564-588) ----

    #[test]
    fn is_completed_aggregated_usage_with_completion_tokens_is_completed() {
        // Go: "usage with completion tokens means completed"
        let response = response_with_completion(5);
        assert!(is_completed_aggregated(&response));
    }

    #[test]
    fn is_completed_aggregated_zero_completion_is_not_completed() {
        // Go: "usage with zero completion tokens is not completed"
        let mut response = HttpResponse::default();
        response.usage = Some(usage_with_completion(0));
        assert!(!is_completed_aggregated(&response));
    }

    #[test]
    fn is_completed_aggregated_explicit_completed_flag_is_completed() {
        // Go: "explicit completed flag is completed" (SpeechStreamResponseID arm)
        let mut response = HttpResponse::default();
        response
            .metadata
            .insert("completed".to_string(), json!(true));
        assert!(is_completed_aggregated(&response));
    }

    #[test]
    fn is_completed_aggregated_missing_usage_and_flag_is_not_completed() {
        // Go: "missing usage and id is not completed"
        let response = HttpResponse::default();
        assert!(!is_completed_aggregated(&response));
    }

    // ---- summarize_binary_chunk ----

    #[test]
    fn summarize_binary_chunk_drops_payload_keeps_size() {
        let mut event = StreamEvent::default();
        event.event_type = Some("audio/mpeg".to_string());
        event.binary = Some(vec![1, 2, 3, 4, 5]);
        let summarized = summarize_binary_chunk(&event);
        assert_eq!(summarized.size, Some(5));
        assert!(summarized.binary.is_none());
        // event_type survives.
        assert_eq!(summarized.event_type.as_deref(), Some("audio/mpeg"));
    }

    #[test]
    fn summarize_binary_chunk_passes_non_binary_through() {
        let event = event_with_data("[DONE]");
        let summarized = summarize_binary_chunk(&event);
        assert_eq!(summarized.data.as_deref(), Some("[DONE]"));
        assert!(summarized.binary.is_none());
        assert!(summarized.size.is_none());
    }

    // ---- StreamChunkBuffer ----

    #[test]
    fn buffer_push_sets_stream_completed_on_terminal_event() {
        let mut buffer = StreamChunkBuffer::new();
        assert!(!buffer.stream_completed());
        buffer.push(&event_with_data("[DONE]"));
        assert!(buffer.stream_completed());
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn buffer_push_keeps_non_terminal_chunks_without_flipping_flag() {
        let mut buffer = StreamChunkBuffer::new();
        buffer.push(&event_with_type("response.output_text.delta"));
        buffer.push(&event_with_type("message_delta"));
        assert!(!buffer.stream_completed());
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn buffer_summarizes_binary_chunks_on_push() {
        let mut buffer = StreamChunkBuffer::new();
        let mut bin_event = StreamEvent::default();
        bin_event.binary = Some(vec![0; 100]);
        buffer.push(&bin_event);
        assert_eq!(buffer.len(), 1);
        // The buffered copy must NOT carry the binary payload.
        assert!(buffer.chunks()[0].binary.is_none());
        assert_eq!(buffer.chunks()[0].size, Some(100));
    }

    // ---- PersistentStreamFinalizer.close — close-branch coverage mirroring
    //      Go `TestOutboundPersistentStream_Close_*` ----

    fn finalizer_with(
        aggregator_response: HttpResponse,
        aggregator_fails: bool,
    ) -> (PersistentStreamFinalizer, std::sync::Arc<CapturingRecorder>) {
        let recorder = std::sync::Arc::new(CapturingRecorder::default());
        let aggregator: std::sync::Arc<dyn StreamChunkAggregator> =
            std::sync::Arc::new(FakeAggregator {
                response: aggregator_response,
                fail: aggregator_fails,
            });
        let finalizer = PersistentStreamFinalizer::new(
            aggregator,
            recorder.clone(),
            "req-1",
            "proj-1",
            attempt(1),
        );
        (finalizer, recorder)
    }

    /// Mirrors Go
    /// `TestOutboundPersistentStream_Close_AggregatedResponsesCompletionHandling`
    /// sub-case "aggregated completed response without terminal event is
    /// completed" (outbound_test.go:680-740): chunks without a terminal event,
    /// but the aggregator yields a complete response -> record_success.
    #[tokio::test]
    async fn close_aggregated_completed_without_terminal_event_records_success()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut finalizer, recorder) = finalizer_with(response_with_completion(2), false);
        // Stream a non-terminal chunk.
        finalizer.on_event(&event_with_type("response.output_text.delta"));

        let ctx = OrchestratorContext::new();
        let observed = ObservedStreamState::default(); // no error, no disconnect

        let plan = finalizer.close(&ctx, &observed).await?;

        assert!(
            plan.is_completed(),
            "aggregated-complete must yield Succeeded plan"
        );
        assert!(plan.write_chunks);
        assert!(plan.write_usage);
        assert_eq!(recorder.success_count(), 1);
        assert_eq!(recorder.failure_count(), 0);
        let captured = recorder
            .last_success()
            .ok_or("expected a captured success row")?;
        assert_eq!(captured.request_id, "req-1");
        assert_eq!(captured.project_id, "proj-1");
        // The aggregated response's usage reached the recorder.
        let captured_usage = captured
            .response
            .usage
            .as_ref()
            .ok_or("recorder must see the aggregated usage")?;
        assert_eq!(captured_usage.completion_tokens, 2);
        Ok(())
    }

    /// Mirrors Go sub-case "response in_progress without terminal event is not
    /// completed" (outbound_test.go:627-678): chunks without a terminal event
    /// AND the aggregator yields an incomplete response (zero completion
    /// tokens, no `completed` flag) -> record_failure with the no-terminal-event
    /// sentinel.
    #[tokio::test]
    async fn close_aggregated_incomplete_without_terminal_event_records_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut incomplete = HttpResponse::default();
        incomplete.json_body = Some(json!({"id":"resp_123","status":"in_progress"}));
        // Zero completion tokens + no completed flag -> not completed.
        let (mut finalizer, recorder) = finalizer_with(incomplete, false);
        finalizer.on_event(&event_with_type("response.in_progress"));

        let ctx = OrchestratorContext::new();
        let observed = ObservedStreamState::default();

        let plan = finalizer.close(&ctx, &observed).await?;

        assert!(!plan.is_completed());
        assert!(!plan.write_chunks);
        // Failed arm (no client disconnect).
        assert_eq!(plan.final_status, RequestStatus::Failed);
        assert_eq!(recorder.success_count(), 0);
        assert_eq!(recorder.failure_count(), 1);
        let captured = recorder
            .last_failure()
            .ok_or("expected a captured failure row")?;
        assert_eq!(
            captured.error_message,
            STREAM_FINAL_NO_TERMINAL_EVENT_MESSAGE
        );
        assert_eq!(captured.kind, ErrorKind::Upstream);
        Ok(())
    }

    /// Mirrors Go sub-case "canceled client with aggregated completed response
    /// is still completed" (outbound_test.go:742+): terminal event observed
    /// AND client disconnected -> record_success (Go's
    /// "StreamCompleted short-circuits even with a context cancellation"
    /// comment at outbound.go:114-117).
    #[tokio::test]
    async fn close_terminal_event_with_client_disconnect_records_success()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut finalizer, recorder) = finalizer_with(response_with_completion(1), false);
        // Terminal event arrives.
        finalizer.on_event(&event_with_data("[DONE]"));

        let ctx = OrchestratorContext::new();
        let observed = ObservedStreamState {
            client_disconnected: true,
            stream_error: Some("context canceled".to_string()),
        };

        let plan = finalizer.close(&ctx, &observed).await?;

        // Completed short-circuits the disconnect.
        assert!(plan.is_completed());
        assert_eq!(recorder.success_count(), 1);
        assert_eq!(recorder.failure_count(), 0);
        Ok(())
    }

    /// Client disconnect WITHOUT a terminal event (and no aggregated
    /// completion) -> Cancelled plan, record_failure. Mirrors Go's
    /// "incomplete_stream_with_error" branch (outbound.go:160-180).
    #[tokio::test]
    async fn close_client_disconnect_without_completion_records_cancel_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        // Aggregator yields an incomplete response.
        let mut incomplete = HttpResponse::default();
        incomplete.json_body = Some(json!({"id":"resp_partial"}));
        let (mut finalizer, recorder) = finalizer_with(incomplete, false);
        finalizer.on_event(&event_with_type("response.output_text.delta"));

        let ctx = OrchestratorContext::new();
        let observed = ObservedStreamState {
            client_disconnected: true,
            stream_error: Some("context canceled".to_string()),
        };

        let plan = finalizer.close(&ctx, &observed).await?;

        assert!(plan.is_canceled());
        assert_eq!(plan.final_status, RequestStatus::Cancelled);
        assert!(!plan.write_chunks);
        assert_eq!(recorder.success_count(), 0);
        assert_eq!(recorder.failure_count(), 1);
        let captured = recorder
            .last_failure()
            .ok_or("expected a captured failure row")?;
        // The explicit stream_error wins over the plan default.
        assert_eq!(captured.error_message, "context canceled");
        Ok(())
    }

    /// Explicit non-cancel stream error -> Failed plan, record_failure with the
    /// stream error message. Mirrors Go's "explicit_stream_error" branch
    /// (outbound.go:128-140) which bypasses aggregation entirely.
    #[tokio::test]
    async fn close_explicit_stream_error_records_failure_without_aggregation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut finalizer, recorder) = finalizer_with(response_with_completion(5), false);
        finalizer.on_event(&event_with_type("response.output_text.delta"));

        let ctx = OrchestratorContext::new();
        let observed = ObservedStreamState {
            client_disconnected: false,
            stream_error: Some("upstream connection reset".to_string()),
        };

        let plan = finalizer.close(&ctx, &observed).await?;

        assert!(!plan.is_completed());
        assert_eq!(plan.final_status, RequestStatus::Failed);
        assert_eq!(recorder.success_count(), 0);
        assert_eq!(recorder.failure_count(), 1);
        let captured = recorder
            .last_failure()
            .ok_or("expected a captured failure row")?;
        assert_eq!(captured.error_message, "upstream connection reset");
        assert_eq!(captured.kind, ErrorKind::Upstream);
        Ok(())
    }

    /// Aggregation failure on the no-terminal-event path -> fall through to the
    /// failure arm (Go `log.Warn` + return at outbound.go:251-253).
    #[tokio::test]
    async fn close_aggregation_failure_treats_stream_as_incomplete()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut finalizer, recorder) = finalizer_with(HttpResponse::default(), true);
        finalizer.on_event(&event_with_type("response.output_text.delta"));

        let ctx = OrchestratorContext::new();
        let observed = ObservedStreamState::default();

        let plan = finalizer.close(&ctx, &observed).await?;

        assert!(!plan.is_completed());
        assert_eq!(recorder.failure_count(), 1);
        Ok(())
    }

    /// Terminal event with NO chunks -> success path with empty body. Mirrors
    /// Go's `if len(ts.responseChunks) > 0` guard (outbound.go:147, 200) where
    /// an empty aggregated body still triggers the success persist (Go
    /// `completed_via_chunk_persistence` decision at outbound.go:201).
    #[tokio::test]
    async fn close_terminal_event_with_no_chunks_records_success_with_empty_body()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut finalizer, recorder) = finalizer_with(HttpResponse::default(), false);
        // Terminal event arrives but we don't aggregate anything (buffer empty
        // -> aggregator never called).
        finalizer.on_event(&event_with_data("[DONE]"));

        let ctx = OrchestratorContext::new();
        let observed = ObservedStreamState::default();

        let plan = finalizer.close(&ctx, &observed).await?;

        assert!(plan.is_completed());
        assert_eq!(recorder.success_count(), 1);
        assert_eq!(recorder.failure_count(), 0);
        Ok(())
    }

    /// Wiring sanity: the wrapper composes with the production recorder at the
    /// type level. The full success-path DB coverage lives in
    /// `request_recorder.rs`'s own tests; here we only verify the
    /// `PersistentStreamFinalizer` accepts a `ProductionRequestRecorder` behind
    /// `Arc<dyn RequestRecorder>` and routes the failure arm when no DB row
    /// was seeded (the recorder surfaces an ConduitError which the finalizer
    /// propagates).
    #[tokio::test]
    async fn close_composes_with_production_recorder_failure_path()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::request_recorder::ProductionRequestRecorder;

        let (recorder, _repo, _sink) = ProductionRequestRecorder::with_in_memory();
        let aggregator: std::sync::Arc<dyn StreamChunkAggregator> =
            std::sync::Arc::new(FakeAggregator {
                response: response_with_completion(7),
                fail: false,
            });
        let recorder_arc: std::sync::Arc<dyn RequestRecorder> = std::sync::Arc::new(recorder);
        let mut finalizer = PersistentStreamFinalizer::new(
            aggregator,
            recorder_arc,
            // No seeded request row with these ids -> record_success will error.
            "nonexistent-req",
            "nonexistent-proj",
            PipelineAttempt {
                sequence: 1,
                channel_id: "1".to_string(),
                model_index: 0,
                mode: ExecutionMode::Stream,
                outcome: Ok(HttpResponse::default()),
            },
        );
        finalizer.on_event(&event_with_type("response.output_text.delta"));

        let ctx = OrchestratorContext::new();
        let result = finalizer.close(&ctx, &ObservedStreamState::default()).await;
        // Aggregator yielded a complete response -> plan computes Succeeded ->
        // recorder.record_success is invoked against a missing row -> error
        // propagates. This proves (a) composition type-checks and (b) the
        // finalizer does not swallow the recorder's error.
        assert!(
            result.is_err(),
            "finalizer must propagate the production recorder's missing-row error"
        );
        Ok(())
    }

    /// Object-safety check: `RequestRecorder` stays object-safe behind `Arc<dyn>`.
    #[allow(dead_code)]
    fn ensure_recorder_object_safe(_r: std::sync::Arc<dyn RequestRecorder>) {}

    #[test]
    fn request_recorder_trait_stays_object_safe() {
        let recorder: std::sync::Arc<dyn RequestRecorder> =
            std::sync::Arc::new(NoopRequestRecorder);
        ensure_recorder_object_safe(recorder);
    }

    /// Object-safety check: `StreamChunkAggregator` stays object-safe.
    #[allow(dead_code)]
    fn ensure_aggregator_object_safe(_a: std::sync::Arc<dyn StreamChunkAggregator>) {}

    #[test]
    fn stream_chunk_aggregator_trait_is_object_safe() {
        let aggregator: std::sync::Arc<dyn StreamChunkAggregator> =
            std::sync::Arc::new(FakeAggregator {
                response: HttpResponse::default(),
                fail: false,
            });
        ensure_aggregator_object_safe(aggregator);
    }

    // -----------------------------------------------------------------------
    // S38 — OutboundForwardingStream: forward-while-aggregating + client
    // disconnect → canceled. Mirrors the runtime semantics asserted by Go
    // `orchestrator_streaming_test.go` (Process_Streaming /
    // Process_StreamingError / Process_StreamingSuccess_NotMarkedAsError) at
    // the forward-loop boundary, plus the `cancelOnCloseStream` disconnect
    // contract (`llm/pipeline/stream.go:109-132`).
    // -----------------------------------------------------------------------

    use crate::request_recorder::{CapturingUsageLogSink, ProductionRequestRecorder};
    use conduit_db::{PolicyContext, Principal, RequestContext};
    use conduit_services::ExecutionRecord;
    use conduit_services::request_service::{RequestPersistenceRepo, RequestRecord};

    fn svc_ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::system()))
    }

    /// Wire a production recorder over the in-memory repo, seeded with a
    /// Running request row ("1"/"1") + execution row ("1-attempt-1").
    async fn seeded_production_recorder() -> Result<
        (
            std::sync::Arc<dyn RequestRecorder>,
            std::sync::Arc<RequestService>,
            std::sync::Arc<InMemoryRequestPersistenceRepo>,
            std::sync::Arc<CapturingUsageLogSink>,
        ),
        Box<dyn std::error::Error>,
    > {
        let repo = std::sync::Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = std::sync::Arc::new(RequestService::new(repo.clone()));
        let sink = std::sync::Arc::new(CapturingUsageLogSink::default());
        let recorder = ProductionRequestRecorder::new(service.clone(), sink.clone());

        let ctx = svc_ctx();
        let mut request = RequestRecord::new("1", "req-1", "1", "POST", "/v1/chat");
        request.status = RequestStatus::Running;
        service.create_request(&ctx, request).await?;
        service
            .append_execution(&ctx, ExecutionRecord::new("1-attempt-1", "1", "1", 1))
            .await?;

        Ok((std::sync::Arc::new(recorder), service, repo, sink))
    }

    fn stream_attempt() -> PipelineAttempt {
        PipelineAttempt {
            sequence: 1,
            channel_id: "1".to_string(),
            model_index: 0,
            mode: ExecutionMode::Stream,
            outcome: Ok(HttpResponse::default()),
        }
    }

    fn forwarding_parts(
        recorder: std::sync::Arc<dyn RequestRecorder>,
        aggregator_response: HttpResponse,
    ) -> (
        OutboundForwardingStream,
        tokio::sync::mpsc::Sender<UpstreamItem>,
        tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>>,
        conduit_pipeline::CancelToken,
    ) {
        let (up_tx, up_rx) = tokio::sync::mpsc::channel::<UpstreamItem>(16);
        let (cl_tx, cl_rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, ConduitError>>(16);
        let cancel = conduit_pipeline::CancelToken::new();
        let aggregator: std::sync::Arc<dyn StreamChunkAggregator> =
            std::sync::Arc::new(FakeAggregator {
                response: aggregator_response,
                fail: false,
            });
        let finalizer =
            PersistentStreamFinalizer::new(aggregator, recorder, "1", "1", stream_attempt())
                .with_execution_id("1-attempt-1");
        let forwarding = OutboundForwardingStream::new(up_rx, cl_tx, cancel.clone(), finalizer);
        (forwarding, up_tx, cl_rx, cancel)
    }

    /// Go `TestChatCompletionOrchestrator_Process_Streaming` +
    /// `Process_StreamingSuccess_NotMarkedAsError` at the forward-loop
    /// boundary: every chunk reaches the client in order, and after the
    /// terminal event the request/execution land Succeeded with chunks +
    /// exactly one usage row.
    #[tokio::test]
    async fn forwarding_loop_forwards_all_events_then_persists_success()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, service, repo, sink) = seeded_production_recorder().await?;
        let (forwarding, up_tx, mut cl_rx, cancel) =
            forwarding_parts(recorder, response_with_completion(2));

        let mut stream_ctx = OrchestratorContext::new();
        stream_ctx.metadata.insert(
            "perf_outbound_start_ms".to_string(),
            epoch_millis().saturating_sub(250).to_string(),
        );
        let handle = tokio::spawn(async move { forwarding.run(&stream_ctx).await });

        up_tx
            .send(UpstreamItem::Event(event_with_data(
                r#"{"choices":[{"delta":{"content":"Hello"}}]}"#,
            )))
            .await?;
        up_tx
            .send(UpstreamItem::Event(event_with_data("[DONE]")))
            .await?;
        drop(up_tx); // upstream ends naturally

        // Forward-while-aggregating: the client sees both events, including
        // the terminal sentinel (Go forwards [DONE] to the client).
        let first = cl_rx.recv().await.ok_or("expected first event")??;
        assert!(first.data.as_deref().unwrap_or("").contains("Hello"));
        let done = cl_rx.recv().await.ok_or("expected [DONE] event")??;
        assert_eq!(done.data.as_deref(), Some("[DONE]"));
        assert!(cl_rx.recv().await.is_none(), "client channel must close");

        let plan = handle.await??;
        assert!(plan.is_completed());
        assert!(!cancel.is_canceled(), "no disconnect -> no upstream cancel");

        // Request row Succeeded with request-level chunks persisted.
        let ctx = svc_ctx();
        let request = repo
            .find_request(&ctx, "1", "1")
            .await?
            .ok_or("request row must exist")?;
        assert_eq!(request.status, RequestStatus::Succeeded);
        assert!(
            request.chunks.as_array().map(Vec::len).unwrap_or(0) == 2,
            "request-level chunks must be persisted (Go SaveRequestChunks)"
        );
        let request_latency = request
            .extra
            .get("metrics_latency_ms")
            .and_then(serde_json::Value::as_i64)
            .ok_or("request stream latency must be persisted")?;
        let request_first_token = request
            .extra
            .get("metrics_first_token_latency_ms")
            .and_then(serde_json::Value::as_i64)
            .ok_or("request first-token latency must be persisted")?;
        assert!(request_latency >= 250);
        assert!(request_first_token >= 250);
        assert!(request_latency >= request_first_token);

        // Execution row Succeeded with execution-level chunks persisted.
        let execs = service.list_executions(&ctx, "1", "1").await?;
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].status, RequestStatus::Succeeded);
        assert!(
            execs[0].chunks.as_array().map(Vec::len).unwrap_or(0) == 2,
            "execution-level chunks must be persisted (Go SaveRequestExecutionChunks)"
        );
        assert_eq!(
            execs[0]
                .extra
                .get("metrics_latency_ms")
                .and_then(serde_json::Value::as_i64),
            Some(request_latency)
        );
        assert_eq!(
            execs[0]
                .extra
                .get("metrics_first_token_latency_ms")
                .and_then(serde_json::Value::as_i64),
            Some(request_first_token)
        );

        // Exactly one usage row (Go writes usage once on the stream path).
        assert_eq!(sink.count(), 1);
        Ok(())
    }

    /// S38/A02 — client drops mid-stream: the upstream cancel token fires
    /// (Go `cancelOnCloseStream` → stream-ctx cancel aborting the provider
    /// call) and BOTH rows land Cancelled (request via
    /// `UpdateRequestStatusFromError` + `context.Canceled`,
    /// biz/request.go:1057-1058; execution via
    /// `UpdateRequestExecutionStatusFromError`, biz/request.go:791-792).
    #[tokio::test]
    async fn forwarding_loop_client_drop_cancels_upstream_and_marks_canceled()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, service, repo, sink) = seeded_production_recorder().await?;
        // Aggregator yields an INCOMPLETE response so the disconnect cannot be
        // promoted to completed (Go isCompletedAggregated == false).
        let mut incomplete = HttpResponse::default();
        incomplete.json_body = Some(json!({"id":"resp_partial"}));
        let (forwarding, up_tx, mut cl_rx, cancel) = forwarding_parts(recorder, incomplete);

        let handle = tokio::spawn(async move { forwarding.run(&OrchestratorContext::new()).await });

        // First delta reaches the client...
        up_tx
            .send(UpstreamItem::Event(event_with_data(
                r#"{"choices":[{"delta":{"content":"partial"}}]}"#,
            )))
            .await?;
        let first = cl_rx.recv().await.ok_or("expected first event")??;
        assert!(first.data.as_deref().unwrap_or("").contains("partial"));

        // ...then the client goes away mid-stream.
        drop(cl_rx);

        // No further provider event is needed: the sender's `closed()` future
        // wakes the forward loop immediately and cancels the blocked upstream
        // read.
        let plan = tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .map_err(|_| "forward loop did not notice client disconnect")???;
        assert!(plan.is_canceled(), "plan must resolve to the Cancelled arm");
        assert_eq!(plan.final_status, RequestStatus::Cancelled);
        assert!(
            cancel.is_canceled(),
            "client drop must cancel the upstream provider call"
        );

        let ctx = svc_ctx();
        let request = repo
            .find_request(&ctx, "1", "1")
            .await?
            .ok_or("request row must exist")?;
        assert_eq!(
            request.status,
            RequestStatus::Cancelled,
            "client disconnect must mark the request canceled (Go semantics)"
        );

        let execs = service.list_executions(&ctx, "1", "1").await?;
        assert_eq!(execs[0].status, RequestStatus::Cancelled);
        // No usage row on the canceled path (Go writes usage only on success).
        assert_eq!(sink.count(), 0);
        Ok(())
    }

    /// Go `TestChatCompletionOrchestrator_Process_StreamingError`: a mid-stream
    /// upstream error marks request AND execution Failed (never Cancelled).
    #[tokio::test]
    async fn forwarding_loop_upstream_error_marks_request_and_execution_failed()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, service, repo, _sink) = seeded_production_recorder().await?;
        let (forwarding, up_tx, mut cl_rx, cancel) =
            forwarding_parts(recorder, HttpResponse::default());

        let handle = tokio::spawn(async move { forwarding.run(&OrchestratorContext::new()).await });

        up_tx
            .send(UpstreamItem::Event(event_with_data(
                r#"{"choices":[{"delta":{"content":"Hello"}}]}"#,
            )))
            .await?;
        assert!(cl_rx.recv().await.is_some());
        // Upstream errors mid-stream (Go mockExecutorWithErrorStream).
        up_tx
            .send(UpstreamItem::Error(ConduitError::upstream(
                "upstream connection reset",
            )))
            .await?;
        drop(up_tx);

        let plan = handle.await??;
        assert!(!plan.is_completed());
        assert_eq!(plan.final_status, RequestStatus::Failed);
        assert!(
            !cancel.is_canceled(),
            "upstream error is not a client cancel"
        );

        let ctx = svc_ctx();
        let request = repo
            .find_request(&ctx, "1", "1")
            .await?
            .ok_or("request row must exist")?;
        assert_eq!(request.status, RequestStatus::Failed);
        let execs = service.list_executions(&ctx, "1", "1").await?;
        assert_eq!(execs[0].status, RequestStatus::Failed);
        Ok(())
    }

    /// S37+S38 — the live-preview observer mounted on the forward loop sees
    /// chunks WHILE the stream is in flight (live preview), and its buffers
    /// are closed + unregistered at stream end (Go live wrappers' Close).
    #[tokio::test]
    async fn forwarding_loop_mounts_live_preview_observer() -> Result<(), Box<dyn std::error::Error>>
    {
        use crate::live_streaming::{LivePreviewObserver, LiveStreamRegistry};
        use crate::orchestrator::live_preview_plan;

        let (recorder, _service, _repo, _sink) = seeded_production_recorder().await?;
        let registry = std::sync::Arc::new(LiveStreamRegistry::new());
        let plan = live_preview_plan(
            true,
            true,
            true,
            Some("1".to_string()),
            Some("1-attempt-1".to_string()),
        );
        let observer = std::sync::Arc::new(LivePreviewObserver::from_plan(plan, registry.clone()));

        let (forwarding, up_tx, mut cl_rx, _cancel) =
            forwarding_parts(recorder, response_with_completion(1));
        let forwarding = forwarding.with_observer(observer);

        let handle = tokio::spawn(async move { forwarding.run(&OrchestratorContext::new()).await });

        up_tx
            .send(UpstreamItem::Event(event_with_data(
                r#"{"choices":[{"delta":{"content":"live"}}]}"#,
            )))
            .await?;
        // The observer appends BEFORE the event is forwarded, so once the
        // client observed it the live snapshot must already carry it.
        assert!(cl_rx.recv().await.is_some());
        assert_eq!(
            registry.get_execution_chunks("1-attempt-1").len(),
            1,
            "live preview must expose in-flight chunks"
        );
        assert_eq!(registry.get_request_chunks("1").len(), 1);

        up_tx
            .send(UpstreamItem::Event(event_with_data("[DONE]")))
            .await?;
        drop(up_tx);
        assert!(cl_rx.recv().await.is_some());
        assert!(cl_rx.recv().await.is_none());

        let plan = handle.await??;
        assert!(plan.is_completed());
        // Go live wrappers' Close(): buffer closed + unregistered.
        assert!(registry.get_execution_buffer("1-attempt-1").is_none());
        assert!(registry.get_request_buffer("1").is_none());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // RUST-P9-006 A01 — additional Go live-preview/persistence parity cases.
    // These cover branches the existing tests leave implicit: the
    // "canceled-with-completed-aggregate" promotion (Go outbound_test.go:742-806),
    // the negative rows of `isCompletedAggregated` (outbound_test.go:573-587),
    // observer inertia under a disabled plan, multi-observer fan-out, the
    // observer on_close firing on the upstream-error path, and the
    // zero-event natural-end edge.
    // -----------------------------------------------------------------------

    /// Mirrors Go sub-case "canceled client with aggregated completed response
    /// is still completed" (outbound_test.go:742-806): no terminal event, but
    /// `stream.Err()` = `context.Canceled` AND the aggregator yields a response
    /// with `CompletionTokens > 0`. Go promotes `StreamCompleted` at
    /// outbound.go:153 via `isCompletedAggregated(meta)` and the success
    /// persist fires despite the cancellation. This is distinct from
    /// [`close_terminal_event_with_client_disconnect_records_success`] which
    /// relies on a `[DONE]` terminal event to short-circuit.
    #[tokio::test]
    async fn close_no_terminal_event_but_aggregator_completed_with_client_disconnect_still_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut finalizer, recorder) = finalizer_with(response_with_completion(3), false);
        // Non-terminal event — no [DONE] / message_stop / response.completed.
        finalizer.on_event(&event_with_type("response.output_text.delta"));

        let ctx = OrchestratorContext::new();
        let observed = ObservedStreamState {
            client_disconnected: true,
            stream_error: Some("context canceled".to_string()),
        };

        let plan = finalizer.close(&ctx, &observed).await?;

        // Aggregator-yielded completion promotes the plan to Succeeded even
        // though the client went away (Go outbound.go:151-154).
        assert!(
            plan.is_completed(),
            "aggregated completion must override client disconnect"
        );
        assert_eq!(recorder.success_count(), 1);
        assert_eq!(recorder.failure_count(), 0);
        let captured = recorder
            .last_success()
            .ok_or("expected success row despite disconnect")?;
        let usage = captured
            .response
            .usage
            .as_ref()
            .ok_or("aggregated usage must reach the recorder")?;
        assert_eq!(usage.completion_tokens, 3);
        Ok(())
    }

    /// Mirrors Go `TestIsCompletedAggregatedOutboundResponse` sub-case "response
    /// id without usage is not completed" (outbound_test.go:573-575). Rust's
    /// HttpResponse has no first-class `id` field; placing it in `metadata`
    /// mirrors the test intent: an id-bearing response without a completion
    /// flag or completion tokens must NOT be treated as completed.
    #[test]
    fn is_completed_aggregated_response_id_alone_is_not_completed() {
        let mut response = HttpResponse::default();
        response
            .metadata
            .insert("id".to_string(), json!("resp_123"));
        assert!(!is_completed_aggregated(&response));
    }

    /// Mirrors Go sub-case "speech stream aggregate id alone is not completed"
    /// (outbound_test.go:581-583): the speech stream response id alone (without
    /// `Completed` flag or usage) must NOT be treated as completed.
    #[test]
    fn is_completed_aggregated_speech_stream_id_alone_is_not_completed() {
        let mut response = HttpResponse::default();
        response
            .metadata
            .insert("id".to_string(), json!("speech_stream_response_id"));
        assert!(!is_completed_aggregated(&response));
    }

    /// S37/S38 — a disabled observer (plan disabled) mounted on the forward
    /// loop must stay inert: no buffers registered, no chunks appended, no
    /// teardown errors. Mirrors Go `livePreviewMiddleware.OnInboundLlmRequest`
    /// gating (`live_streaming.go:36-53`): when `m.enabled == false` every hook
    /// short-circuits.
    #[tokio::test]
    async fn forwarding_loop_disabled_observer_is_inert_throughout_lifecycle()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::live_streaming::{LivePreviewObserver, LiveStreamRegistry};
        use crate::orchestrator::live_preview_plan;

        let (recorder, _service, _repo, _sink) = seeded_production_recorder().await?;
        let registry = std::sync::Arc::new(LiveStreamRegistry::new());
        // Policy off -> disabled plan (mirrors Go `m.enabled = false`).
        let plan = live_preview_plan(
            true,
            true,
            false,
            Some("1".to_string()),
            Some("1-attempt-1".to_string()),
        );
        let observer = std::sync::Arc::new(LivePreviewObserver::from_plan(plan, registry.clone()));

        let (forwarding, up_tx, mut cl_rx, _cancel) =
            forwarding_parts(recorder, response_with_completion(1));
        let forwarding = forwarding.with_observer(observer);

        let handle = tokio::spawn(async move { forwarding.run(&OrchestratorContext::new()).await });

        up_tx
            .send(UpstreamItem::Event(event_with_data(
                r#"{"choices":[{"delta":{"content":"hi"}}]}"#,
            )))
            .await?;
        assert!(cl_rx.recv().await.is_some());
        up_tx
            .send(UpstreamItem::Event(event_with_data("[DONE]")))
            .await?;
        drop(up_tx);
        assert!(cl_rx.recv().await.is_some());
        assert!(cl_rx.recv().await.is_none());

        let plan = handle.await??;
        assert!(plan.is_completed());
        // Disabled observer -> no buffers registered, no chunks captured.
        assert!(
            registry.get_request_buffer("1").is_none(),
            "disabled observer must not register buffers"
        );
        assert!(
            registry.get_execution_buffer("1-attempt-1").is_none(),
            "disabled observer must not register execution buffers"
        );
        assert!(registry.get_request_chunks("1").is_empty());
        assert!(registry.get_execution_chunks("1-attempt-1").is_empty());
        Ok(())
    }

    /// S37/S38 — multiple observers mounted simultaneously each receive every
    /// event and run their own teardown. Mirrors Go's ability to stack
    /// middlewares on the outbound stream (the Rust mount generalizes to a
    /// `Vec` of observers).
    #[tokio::test]
    async fn forwarding_loop_multiple_observers_both_receive_events()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::live_streaming::{LivePreviewObserver, LiveStreamRegistry};
        use crate::orchestrator::live_preview_plan;

        let (recorder, _service, _repo, _sink) = seeded_production_recorder().await?;
        let registry_a = std::sync::Arc::new(LiveStreamRegistry::new());
        let registry_b = std::sync::Arc::new(LiveStreamRegistry::new());

        let plan_a = live_preview_plan(
            true,
            true,
            true,
            Some("req-a".to_string()),
            Some("exec-a".to_string()),
        );
        let plan_b = live_preview_plan(
            true,
            true,
            true,
            Some("req-b".to_string()),
            Some("exec-b".to_string()),
        );
        let observer_a =
            std::sync::Arc::new(LivePreviewObserver::from_plan(plan_a, registry_a.clone()));
        let observer_b =
            std::sync::Arc::new(LivePreviewObserver::from_plan(plan_b, registry_b.clone()));

        let (forwarding, up_tx, mut cl_rx, _cancel) =
            forwarding_parts(recorder, response_with_completion(1));
        let forwarding = forwarding
            .with_observer(observer_a)
            .with_observer(observer_b);

        let handle = tokio::spawn(async move { forwarding.run(&OrchestratorContext::new()).await });

        up_tx
            .send(UpstreamItem::Event(event_with_data(
                r#"{"choices":[{"delta":{"content":"multi"}}]}"#,
            )))
            .await?;
        assert!(cl_rx.recv().await.is_some());
        // Both observers must have buffered the event before forwarding.
        assert_eq!(registry_a.get_execution_chunks("exec-a").len(), 1);
        assert_eq!(registry_b.get_execution_chunks("exec-b").len(), 1);

        up_tx
            .send(UpstreamItem::Event(event_with_data("[DONE]")))
            .await?;
        drop(up_tx);
        assert!(cl_rx.recv().await.is_some());
        assert!(cl_rx.recv().await.is_none());

        let plan = handle.await??;
        assert!(plan.is_completed());
        // Both registries cleaned up by on_close.
        assert!(registry_a.get_execution_buffer("exec-a").is_none());
        assert!(registry_b.get_execution_buffer("exec-b").is_none());
        Ok(())
    }

    /// S37/S38 — observer `on_close` fires on the upstream-error path too (not
    /// just the success path). Mirrors Go's live wrappers being closed on every
    /// teardown branch (`live_streaming.go:169-179` / `:215-225` — Go's stream
    /// Close cascades through the wrapper regardless of how the stream ended).
    #[tokio::test]
    async fn forwarding_loop_observer_close_fires_on_upstream_error_path()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::live_streaming::{LivePreviewObserver, LiveStreamRegistry};
        use crate::orchestrator::live_preview_plan;

        let (recorder, _service, _repo, _sink) = seeded_production_recorder().await?;
        let registry = std::sync::Arc::new(LiveStreamRegistry::new());
        let plan = live_preview_plan(
            true,
            true,
            true,
            Some("err-req".to_string()),
            Some("err-exec".to_string()),
        );
        let observer = std::sync::Arc::new(LivePreviewObserver::from_plan(plan, registry.clone()));

        // Aggregator yields an incomplete response so the failure arm fires.
        let mut incomplete = HttpResponse::default();
        incomplete.json_body = Some(json!({"id":"resp_partial"}));
        let (forwarding, up_tx, mut cl_rx, _cancel) = forwarding_parts(recorder, incomplete);
        let forwarding = forwarding.with_observer(observer);

        let handle = tokio::spawn(async move { forwarding.run(&OrchestratorContext::new()).await });

        up_tx
            .send(UpstreamItem::Event(event_with_data(
                r#"{"choices":[{"delta":{"content":"before-error"}}]}"#,
            )))
            .await?;
        assert!(cl_rx.recv().await.is_some());
        // Observer buffered the event.
        assert_eq!(registry.get_execution_chunks("err-exec").len(), 1);

        up_tx
            .send(UpstreamItem::Error(ConduitError::upstream(
                "upstream connection reset",
            )))
            .await?;
        drop(up_tx);
        assert!(matches!(cl_rx.recv().await, Some(Err(_))));
        assert!(cl_rx.recv().await.is_none());

        let plan = handle.await??;
        assert!(!plan.is_completed());
        assert_eq!(plan.final_status, RequestStatus::Failed);
        // on_close must have fired — buffers cleaned up.
        assert!(
            registry.get_execution_buffer("err-exec").is_none(),
            "observer must close + unregister on upstream-error path"
        );
        assert!(registry.get_request_buffer("err-req").is_none());
        Ok(())
    }

    /// S38 — upstream ends naturally with zero events. The finalizer still
    /// runs; with no terminal event and no chunks to aggregate the plan
    /// resolves to the Failed/no-terminal arm (Go outbound.go:160-180 with
    /// empty responseChunks at line 147 guard).
    #[tokio::test]
    async fn forwarding_loop_empty_upstream_stream_finalizes_as_failed_no_terminal()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _service, _repo, sink) = seeded_production_recorder().await?;
        let (forwarding, up_tx, mut cl_rx, cancel) =
            forwarding_parts(recorder, response_with_completion(1));

        let handle = tokio::spawn(async move { forwarding.run(&OrchestratorContext::new()).await });

        drop(up_tx); // upstream channel closes immediately, no events
        assert!(
            cl_rx.recv().await.is_none(),
            "client channel must close with no events forwarded"
        );

        let plan = handle.await??;
        assert!(!plan.is_completed());
        assert_eq!(plan.final_status, RequestStatus::Failed);
        assert!(
            !cancel.is_canceled(),
            "no client disconnect on empty stream"
        );
        // No usage row on the failure path.
        assert_eq!(sink.count(), 0);
        Ok(())
    }
}
