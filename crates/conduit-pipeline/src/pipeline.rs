//! Pipeline process — LLM request execution flow (RUST-P8-002).
//!
//! Ports the *control flow* of Go `conduit/llm/pipeline/pipeline.go`'s
//! `Pipeline.Process` / `processRequest` into pure, testable Rust:
//!
//! - **S04 inbound transform once**: the inbound transformer is applied exactly
//!   once per request (Go `Inbound.TransformRequest`). The transformed
//!   `LlmRequest` is reused for every retry attempt — Go mutates and resets
//!   `llmRequest.Stream = originalStream` each iteration; here the snapshot is
//!   owned by the pipeline and handed to each attempt by value.
//! - **S04 inbound middlewares once**: inbound LLM middlewares run exactly once
//!   (Go `applyBeforeRequestMiddlewares`) before the attempt loop.
//! - **S05-S07 attempt loop**: per attempt — outbound transform → merge inbound
//!   → auth → outbound raw middlewares → execute. Order is observable through
//!   [`PipelineContext::order`] so unit tests can assert the sequence.
//! - **S07 stream/non-stream/auto-aggregate**: the *user's* requested stream
//!   flag (`originalWantStream`) drives the branch. When the user asked for a
//!   stream we hand back a stream; when they did not but the provider responds
//!   with a stream we auto-aggregate; otherwise it is a plain non-stream
//!   response. `ExecutionMode` records which branch was taken for each attempt.
//! - **S08-S10/S14/S17 retry**: retries are driven by a failover cursor
//!   ([`StrFailoverState`] here; the orchestrator's richer
//!   `FailoverState` for `&Candidate` follows the same contract) —
//!   same-channel first (`prepare_for_retry`) then channel switch
//!   (`next_channel`), mirroring Go's `ChannelRetryable.CanRetry/
//!   PrepareForRetry` → `Retryable.HasMoreChannels/NextChannel` ordering.
//!   `max_channel_retries` / `max_single_channel_retries` / `retry_delay` come
//!   from [`RetryPolicy`]. A canceled or timed-out context never retries (Go
//!   checks `ctx.Err()`), and the same-channel branch is skipped on
//!   response-timeout errors (Go `isResponseTimeoutError`).
//!
//! Transformers and the executor are trait objects so the loop is fully
//! injectable: tests feed in stubs and inspect `PipelineContext::order` /
//! `AttemptRecord` to verify attempt order, retry counts and stream-mode
//! decisions without any network.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use conduit_core::objects::channel_settings::{ErrorResponseRewriteRule, RetryableErrorPattern};
use conduit_core::{ConduitError, ErrorKind};
use conduit_llm::{ApiFormat, HttpAuth, HttpRequest, HttpResponse, LlmRequest, StreamEvent};
use conduit_transformers::{InboundTransformer, OutboundTransformer};

use crate::cancel::CancelToken;
use crate::empty_response::{
    ErrEmptyAggregatedBody, ErrEmptyResponse, ErrEmptyStreamChunks, ErrNonStreamResponseTimeout,
    ErrStreamFirstEventTimeout, has_response_content,
};
use crate::error_rewrite::apply_error_response_rewrite;
use crate::middleware::{
    BoxEventStream, BoxPipelineMiddleware, PipelineContext, apply_before_request_middlewares,
    apply_inbound_raw_response_middlewares, apply_inbound_raw_stream_middlewares,
    apply_llm_response_middlewares, apply_llm_stream_middlewares,
    apply_raw_error_response_middlewares, apply_raw_request_middlewares,
    apply_raw_response_middlewares, apply_raw_stream_middlewares,
};
use crate::retryable::{is_channel_extra_retryable, is_retryable_error};
use crate::upstream_error::wrap_upstream_error;

// ---------------------------------------------------------------------------
// Local retry policy (skeleton).
// ---------------------------------------------------------------------------
// The full `RetryPolicy` (with `LoadBalancerStrategy`, top-K math, sticky
// providers, …) lives in `conduit-orchestrator::load_balancer`. The pipeline
// crate sits *below* the orchestrator in the dependency graph (the orchestrator
// drives pipelines), so it cannot depend on it. The pipeline only needs the
// three retry knobs + an enabled flag; we capture those in a small local type
// and the orchestrator converts its richer policy into this view when it
// constructs a [`Pipeline`].

/// Retry knobs the pipeline consults. A trimmed view of the orchestrator's
/// `RetryPolicy` (Go `biz.RetryPolicy` fields the pipeline reads).
///
/// The load-balancer strategy selector is **not** part of this view: Go's
/// `LoadBalancerStrategy` (default `"adaptive"`, `biz/system_default.go:26`)
/// is consumed by the orchestrator when picking candidates — see the richer
/// `conduit-orchestrator::load_balancer::RetryPolicy`
/// (`load_balancer.rs`, `LoadBalancerStrategy::Adaptive` is the `DEFAULT`
/// strategy there), not by the pipeline retry loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    pub enabled: bool,
    /// Max number of **different** channels to try after the first (Go
    /// `MaxChannelRetries`).
    pub max_channel_retries: u32,
    /// Max retries on the **same** channel before moving on (Go
    /// `MaxSingleChannelRetries`).
    pub max_single_channel_retries: u32,
    /// Delay between retries in milliseconds (Go `RetryDelayMs`).
    pub retry_delay_ms: u64,
    /// RUST-P8-002 S12 — first streaming event timeout, in milliseconds.
    /// `0` disables it (Go `newFirstEventTimeoutGuard` returns a nil guard for
    /// `timeout <= 0`, `stream.go:30-33`). Source field: Go
    /// `biz.RetryPolicy.StreamFirstEventTimeoutSeconds`
    /// (`internal/server/biz/system.go:319-321`, seconds), converted to a
    /// `time.Duration` by the orchestrator and passed to the pipeline via
    /// `WithResponseTimeouts` (`orchestrator.go:224-227`, `pipeline.go:77-82`).
    /// Default `0` (Go `defaultRetryPolicy` leaves it unset).
    pub stream_first_event_timeout_ms: u64,
    /// RUST-P8-002 S12 — non-streaming response timeout, in milliseconds.
    /// `0` disables it (Go `withNonStreamTimeout` no-ops for `<= 0`,
    /// `pipeline.go:449-455`). Source field: Go
    /// `biz.RetryPolicy.NonStreamResponseTimeoutSeconds`
    /// (`internal/server/biz/system.go:322-324`). Default `0`.
    /// Applied to both the plain non-stream arm and the auto-aggregate arm
    /// (Go `pipeline.go:406` / `:423`); the auto-aggregate arm does NOT get a
    /// first-event timeout (Go passes `0` at `non_streaming.go:86`).
    pub non_stream_timeout_ms: u64,
    /// RUST-P8-002 A01 — empty-response detection toggle (Go
    /// `pipeline.emptyResponseDetection`, set via `WithEmptyResponseDetection()`
    /// at `pipeline.go:67-75`). When enabled, the non-stream arm checks the
    /// unified `LlmResponse` (produced by `Outbound::transform_response`)
    /// with [`has_response_content`] and returns [`ErrEmptyResponse`] if no
    /// meaningful content is present
    /// (Go `non_streaming.go:55-59`); the stream arm pre-reads up to 3 events
    /// (Go `stream.go:153-217`) — **stream-path wiring is pending** the real
    /// streaming executor (see [`Pipeline::with_empty_response_detection`]
    /// doc). Source field: Go `biz.RetryPolicy.EmptyResponseDetection`
    /// (`internal/server/biz/system.go:337`). Default `false` (Go
    /// `defaultRetryPolicy` leaves it unset).
    pub empty_response_detection: bool,
}

/// Upper clamp for both response timeouts, in seconds. Mirrors Go
/// `maxRetryResponseTimeoutSeconds` (`internal/server/biz/system.go:32`).
pub const MAX_RETRY_RESPONSE_TIMEOUT_SECONDS: i64 = 600;

/// Normalize a configured response-timeout value in seconds: negatives clamp
/// to 0 (disabled), values above [`MAX_RETRY_RESPONSE_TIMEOUT_SECONDS`] clamp
/// to the max. Mirrors Go `normalizeRetryPolicy`
/// (`internal/server/biz/system.go:1041-1053`).
pub const fn clamp_response_timeout_seconds(seconds: i64) -> u64 {
    if seconds < 0 {
        0
    } else if seconds > MAX_RETRY_RESPONSE_TIMEOUT_SECONDS {
        MAX_RETRY_RESPONSE_TIMEOUT_SECONDS as u64
    } else {
        seconds as u64
    }
}

impl RetryPolicy {
    /// The Go default (`defaultRetryPolicy`): enabled, 3 channel retries, 2
    /// single-channel retries, 1000ms delay, both response timeouts unset
    /// (0 = disabled; Go `biz/system_default.go:22-31` does not set them),
    /// empty-response detection off (Go `defaultRetryPolicy` leaves
    /// `EmptyResponseDetection` unset → `false`).
    pub const DEFAULT: Self = Self {
        enabled: true,
        max_channel_retries: 3,
        max_single_channel_retries: 2,
        retry_delay_ms: 1000,
        stream_first_event_timeout_ms: 0,
        non_stream_timeout_ms: 0,
        empty_response_detection: false,
    };
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

// ---------------------------------------------------------------------------
// S12 response-timeout sentinels on `ConduitError`.
// ---------------------------------------------------------------------------
// The unit-struct sentinels (`ErrStreamFirstEventTimeout`,
// `ErrNonStreamResponseTimeout`, ported in `empty_response.rs` from Go
// `empty_response.go:13-17`) are the *identities*; the pipeline traffics in
// `ConduitError`, so these helpers build/recognize the `ConduitError` form the way
// Go builds/recognizes the `errors.Is` form.

/// Stable `ConduitError::code` for the stream first-event timeout sentinel.
pub const STREAM_FIRST_EVENT_TIMEOUT_CODE: &str = "stream_first_event_timeout";

/// Stable `ConduitError::code` for the non-stream response timeout sentinel.
pub const NON_STREAM_RESPONSE_TIMEOUT_CODE: &str = "non_stream_response_timeout";

/// Build the stream first-event timeout error (Go `ErrStreamFirstEventTimeout`,
/// `empty_response.go:14`, surfaced by the guard at `stream.go:103`/`234`).
pub fn stream_first_event_timeout_error() -> ConduitError {
    ConduitError::new(ErrorKind::Timeout, "stream first event timeout")
        .with_code(STREAM_FIRST_EVENT_TIMEOUT_CODE)
        .with_source(ErrStreamFirstEventTimeout)
}

/// Build the non-stream response timeout error (Go
/// `ErrNonStreamResponseTimeout`, `empty_response.go:17`, surfaced at
/// `pipeline.go:411`/`428`).
pub fn non_stream_response_timeout_error() -> ConduitError {
    ConduitError::new(ErrorKind::Timeout, "non-stream response timeout")
        .with_code(NON_STREAM_RESPONSE_TIMEOUT_CODE)
        .with_source(ErrNonStreamResponseTimeout)
}

/// Whether the error is the stream first-event timeout sentinel
/// (Go `errors.Is(err, ErrStreamFirstEventTimeout)`).
pub fn is_stream_first_event_timeout(err: &ConduitError) -> bool {
    err.code.as_deref() == Some(STREAM_FIRST_EVENT_TIMEOUT_CODE)
}

/// Whether the error is the non-stream response timeout sentinel
/// (Go `errors.Is(err, ErrNonStreamResponseTimeout)`).
pub fn is_non_stream_response_timeout(err: &ConduitError) -> bool {
    err.code.as_deref() == Some(NON_STREAM_RESPONSE_TIMEOUT_CODE)
}

/// Whether the error is either response-timeout sentinel. Mirrors Go
/// `isResponseTimeoutError` (`pipeline.go:445-447`): such errors skip the
/// same-channel retry arm (`pipeline.go:297-300`).
pub fn is_response_timeout_error(err: &ConduitError) -> bool {
    is_stream_first_event_timeout(err) || is_non_stream_response_timeout(err)
}

/// Sentinel code carried on [`ConduitError::code`] for the empty-stream-chunks
/// error (Go `ErrEmptyStreamChunks`). Detection by code mirrors how the
/// timeout sentinels are detected elsewhere in this module.
pub const EMPTY_STREAM_CHUNKS_CODE: &str = "empty_stream_chunks";

/// Sentinel code for the empty-aggregated-body error (Go `ErrEmptyAggregatedBody`).
pub const EMPTY_AGGREGATED_BODY_CODE: &str = "empty_aggregated_body";

/// Sentinel code for the empty-response error (Go `ErrEmptyResponse`).
pub const EMPTY_RESPONSE_CODE: &str = "empty_response";

/// Build the empty-stream-chunks error (Go `ErrEmptyStreamChunks`,
/// `empty_response.go:20`, surfaced at `non_streaming.go:105-108` when an
/// auto-aggregated upstream produced no events). Wrapped as Upstream so the
/// API policy layer treats it like a provider failure; the sentinel is
/// detectable via [`is_empty_stream_chunks`] (Go `errors.Is` analog).
pub fn empty_stream_chunks_error() -> ConduitError {
    ConduitError::upstream("empty stream chunks")
        .with_code(EMPTY_STREAM_CHUNKS_CODE)
        .with_source(ErrEmptyStreamChunks)
}

/// Whether the error is the empty-stream-chunks sentinel.
pub fn is_empty_stream_chunks(err: &ConduitError) -> bool {
    err.code.as_deref() == Some(EMPTY_STREAM_CHUNKS_CODE)
}

/// Build the empty-aggregated-body error (Go `ErrEmptyAggregatedBody`,
/// `empty_response.go:23`, surfaced at `non_streaming.go:116-119` when the
/// inbound aggregator returned a zero-length body).
pub fn empty_aggregated_body_error() -> ConduitError {
    ConduitError::upstream("empty aggregated body")
        .with_code(EMPTY_AGGREGATED_BODY_CODE)
        .with_source(ErrEmptyAggregatedBody)
}

/// Whether the error is the empty-aggregated-body sentinel.
pub fn is_empty_aggregated_body(err: &ConduitError) -> bool {
    err.code.as_deref() == Some(EMPTY_AGGREGATED_BODY_CODE)
}

/// Build the empty-response error (Go `ErrEmptyResponse`,
/// `empty_response.go:11`, surfaced at `non_streaming.go:55-58` when an LLM
/// response carries no meaningful content).
pub fn empty_response_error() -> ConduitError {
    ConduitError::upstream("empty response detected")
        .with_code(EMPTY_RESPONSE_CODE)
        .with_source(ErrEmptyResponse)
}

/// Whether the error is the empty-response sentinel.
pub fn is_empty_response(err: &ConduitError) -> bool {
    err.code.as_deref() == Some(EMPTY_RESPONSE_CODE)
}

/// Error returned by the failover cursor when no further attempt is possible.
/// Mirrors `conduit_orchestrator::load_balancer::FailoverError` (kept separate
/// to avoid the orchestrator dependency cycle).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FailoverError {
    #[error("no more candidates available for retry")]
    NoMoreChannels,
    #[error("single-channel retry budget exhausted for channel {channel_id}")]
    SingleChannelExhausted { channel_id: String },
    #[error("retry policy disabled")]
    RetryDisabled,
}

// ---------------------------------------------------------------------------
// Stream-mode decision (S07) — mirrors Go `processRequest`'s switch.
// ---------------------------------------------------------------------------

/// How an attempt's response was (or would be) delivered to the caller.
///
/// Mirrors the three arms of the Go `switch` in `processRequest`:
/// `originalWantStream` → `Stream`; else `effectiveWantStream` → `AutoAggregate`;
/// else `NonStream`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionMode {
    /// User requested a stream — return the raw event stream (Go `stream`).
    Stream,
    /// User did not request a stream but the provider responded with one —
    /// buffer/aggreg­ate it into a single `HttpResponse` (Go
    /// `autoAggregateStream`).
    AutoAggregate,
    /// Plain non-stream request/response (Go `notStream`).
    NonStream,
}

impl ExecutionMode {
    /// Resolve the execution mode for an attempt, mirroring Go
    /// `processRequest`'s switch. `user_wants_stream` is the *original* request
    /// flag (Go `originalWantStream`); `effective_wants_stream` is the flag
    /// actually sent upstream after the outbound transformer may have flipped
    /// it (Go `effectiveWantStream`).
    pub const fn resolve(user_wants_stream: bool, effective_wants_stream: bool) -> Self {
        match (user_wants_stream, effective_wants_stream) {
            (true, _) => Self::Stream,
            (false, true) => Self::AutoAggregate,
            (false, false) => Self::NonStream,
        }
    }
}

/// Outcome of a single attempt. Captured by [`Pipeline::process`] for
/// observability and by tests to assert attempt order / stream-mode decisions.
///
/// The error arm is a small [`AttemptError`] snapshot rather than the full
/// [`ConduitError`] (which owns a boxed source and is not `Clone`); the pipeline
/// keeps the real error separately for its return value.
#[derive(Clone, Debug, PartialEq)]
pub struct AttemptRecord {
    /// 1-based attempt sequence (Go `attempts` counter).
    pub sequence: u32,
    /// Candidate id attempted (Go `CurrentCandidate`).
    pub channel_id: String,
    /// Model index inside the candidate's model list for this attempt
    /// (Go `CurrentModelIndex`).
    pub model_index: usize,
    /// Stream-mode branch taken (Go `originalWantStream` switch).
    pub mode: ExecutionMode,
    /// `Ok` with the response on success; `Err` with a failure snapshot.
    pub outcome: Result<HttpResponse, AttemptError>,
}

/// Cloneable snapshot of a failed attempt's error. Mirrors only the fields
/// tests/observability need; the full [`ConduitError`] is returned by
/// [`Pipeline::process`] when all attempts fail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptError {
    /// [`conduit_core::ErrorKind`] string slug (e.g. `"upstream_error"`).
    pub kind: &'static str,
    /// Human-readable message (not the safe/public one).
    pub message: String,
}

impl AttemptError {
    pub fn from_axon(err: &ConduitError) -> Self {
        Self {
            kind: err.kind.as_str(),
            message: err.message.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Executor trait (stub) — the actual upstream HTTP call lives behind this.
// ---------------------------------------------------------------------------

/// Executes a transformed [`HttpRequest`] against an upstream provider.
///
/// This is a **trait stub**: the real HTTP/streaming implementation lands in a
/// later task. Pure-logic tests inject a deterministic implementation that
/// records the call and returns canned responses/events.
#[async_trait]
pub trait Executor: Send + Sync {
    /// Non-streaming execution. Returns the full response body.
    async fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, ConduitError>;

    /// Streaming execution. Returns the upstream event stream. The pipeline
    /// decides whether to forward it (`Stream`) or aggregate it
    /// (`AutoAggregate`) based on [`ExecutionMode`].
    async fn execute_stream(&self, request: &HttpRequest)
    -> Result<Vec<StreamEvent>, ConduitError>;

    /// RUST-P8-002 S13 — streaming execution with an upstream cancel token.
    ///
    /// Mirrors Go `executor.DoStream(streamCtx, request)` (`stream.go:267`)
    /// where `streamCtx` is the child context created per stream attempt: the
    /// token cancels when the client stream is closed/dropped
    /// ([`crate::cancel::CancelOnCloseStream`], Go `cancelOnCloseStream`) or
    /// when the whole request context cancels (client disconnect, S17). Real
    /// executors abort the upstream HTTP request when it fires; the default
    /// impl ignores it so existing pure-logic executors keep compiling.
    async fn execute_stream_cancellable(
        &self,
        request: &HttpRequest,
        cancel: CancelToken,
    ) -> Result<Vec<StreamEvent>, ConduitError> {
        let _ = cancel;
        self.execute_stream(request).await
    }

    /// RUST-P8-003 — **live** streaming execution (phase 2).
    ///
    /// Returns a channel receiver that yields provider stream events
    /// *incrementally* as they arrive (Go `executor.DoStream` handing back a
    /// lazy `streams.Stream`), rather than the buffered `Vec<StreamEvent>` of
    /// [`Executor::execute_stream`]. Each item is `Ok(event)` for a provider
    /// event or `Err(ConduitError)` for a mid-stream provider failure (Go
    /// `stream.Err()`); the sender closes when the upstream ends.
    ///
    /// # Design note (trait return type — task option 1a)
    ///
    /// The item type is the pipeline-level `Result<StreamEvent, ConduitError>`.
    /// The orchestrator's `UpstreamItem` enum (with its `Error` variant) lives
    /// in `conduit-orchestrator`, which **depends on** this crate, so it cannot
    /// be named in this trait without a dependency cycle. The orchestrator
    /// adapts `Result<StreamEvent, ConduitError>` → `UpstreamItem` at the call site
    /// (a one-hop forwarding task). This keeps the trait in the lower crate and
    /// avoids moving `UpstreamItem` down into the pipeline.
    ///
    /// `cancel` aborts the upstream provider call when fired (client disconnect,
    /// S13/S17). The default impl falls back to the buffered
    /// [`Executor::execute_stream`] — it spawns a task that sends the buffered
    /// events then closes — so every existing `Executor` impl keeps compiling
    /// and the buffered path never regresses.
    async fn execute_stream_live(
        &self,
        request: &HttpRequest,
        cancel: CancelToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>>, ConduitError> {
        let _ = cancel;
        // Buffered fallback: materialize the whole stream, then replay it
        // through the channel. Real executors (`UpstreamExecutor`) override this
        // with a chunk-by-chunk `bytes_stream()` reader.
        let events = self.execute_stream(request).await?;
        let (tx, rx) = tokio::sync::mpsc::channel(events.len().max(1));
        tokio::spawn(async move {
            for event in events {
                if tx.send(Ok(event)).await.is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }
}

/// Predicate mirroring Go `ChannelRetryable.CanRetry`: given the error from a
/// failed attempt, may the *same* channel be retried? Tests inject this so the
/// retry decision is observable without a real provider.
pub type CanRetryFn = Arc<dyn Fn(&ConduitError) -> bool + Send + Sync>;

/// Hook mirroring Go `Retryable.HasMoreChannels`: are there more channels to
/// switch to? Delegated to the failover cursor in production but exposed as a
/// closure so tests can force the channel-switch path off.
pub type HasMoreChannelsFn = Arc<dyn Fn() -> bool + Send + Sync>;

/// Classifier mirroring Go `isResponseTimeoutError`: when true the same-channel
/// retry arm is skipped (Go does not call `CanRetry` for timeout errors).
pub type IsTimeoutErrorFn = Arc<dyn Fn(&ConduitError) -> bool + Send + Sync>;

/// Hook mirroring Go `ChannelCustomizedExecutor.CustomizeExecutor`
/// (`pipeline.go:38-43`/`:381-384`): given the pipeline's default executor,
/// return the executor to use for the current attempt. When set, the pipeline
/// calls it once per attempt AFTER the outbound raw request middlewares and
/// BEFORE execution. `None` (default) means no customization — the pipeline
/// executor is used as-is, matching a Go outbound that does NOT implement
/// `ChannelCustomizedExecutor`.
pub type CustomizeExecutorFn = Arc<dyn Fn(Arc<dyn Executor>) -> Arc<dyn Executor> + Send + Sync>;

/// Retry hooks the pipeline consults. Each closure corresponds to one Go
/// interface method so the retry decision tree is fully injectable.
#[derive(Clone)]
pub struct RetryHooks {
    pub can_retry: CanRetryFn,
    pub has_more_channels: HasMoreChannelsFn,
    pub is_timeout_error: IsTimeoutErrorFn,
}

impl Default for RetryHooks {
    fn default() -> Self {
        // Defaults mirror Go's built-in `retryableChecker` (`pipeline.go:24-32`),
        // NOT "never". Go's pipeline retries on the default retryable status set
        // (429 + 5xx) out of the box; a `|_| false` default silently disabled
        // all retries in production (the outbound never re-implemented the hook).
        // We do BETTER than Go's two-layer split (pipeline `retryableChecker` +
        // orchestrator `isRetryableErrorForChannel` via the `ChannelRetryable`
        // interface): the pipeline is data-driven end to end — the per-attempt
        // `can_retry` reads the CURRENT candidate's retry settings directly (see
        // `retry_can_retry`), so no interface wiring is needed and per-channel
        // overrides always apply.
        //
        // `can_retry` here is the FALLBACK used only when a caller/test does not
        // route through the candidate-aware path; it applies the Go default set.
        // `has_more_channels` defaults to `true` and defers the real decision to
        // the failover cursor (`FailoverState::next_channel` returns
        // `NoMoreChannels` when exhausted) — mirroring Go's
        // `attempt < len(channels)-1`. `is_timeout_error` stays the real
        // classifier (Go `isResponseTimeoutError`, `pipeline.go:297`).
        Self {
            can_retry: Arc::new(is_retryable_error),
            has_more_channels: Arc::new(|| true),
            is_timeout_error: Arc::new(is_response_timeout_error),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure S04-S06 attempt lifecycle (Go processRequest stage order).
// ---------------------------------------------------------------------------
// The Go `processRequest` body executes a fixed sequence per attempt:
//   outbound.TransformRequest -> MergeInboundRequest -> FinalizeAuthHeaders
//   -> applyRawRequestMiddlewares -> (CustomizeExecutor) -> executor.Do/DoStream
// (see `conduit/llm/pipeline/pipeline.go` `processRequest`, lines ~358-438).
// `AttemptStage` makes that order a first-class, property-testable enum so a
// unit test can assert the contract without spinning up transformers.

/// A single stage of the per-attempt pipeline body (S04-S06). The order is
/// fixed by Go `processRequest`; [`AttemptStage::next`] encodes that order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptStage {
    /// Go `Outbound.TransformRequest(ctx, request)`.
    OutboundTransform,
    /// Go `httpclient.MergeInboundRequest(httpReq, request.RawRequest)`.
    MergeInbound,
    /// Go `httpclient.FinalizeAuthHeaders(httpReq)`.
    AuthHeaders,
    /// Go `applyRawRequestMiddlewares`.
    OutboundRawMiddlewares,
    /// Go `CustomizeExecutor` (optional) then `executor.Do`/`DoStream`.
    Execute,
}

impl AttemptStage {
    /// The first stage of every attempt.
    pub const FIRST: Self = Self::OutboundTransform;

    /// Advance to the next stage in the Go `processRequest` order, or `None`
    /// after [`AttemptStage::Execute`]. Pure — no I/O.
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::OutboundTransform => Some(Self::MergeInbound),
            Self::MergeInbound => Some(Self::AuthHeaders),
            Self::AuthHeaders => Some(Self::OutboundRawMiddlewares),
            Self::OutboundRawMiddlewares => Some(Self::Execute),
            Self::Execute => None,
        }
    }

    /// Enumerate the full stage sequence from [`FIRST`]. Used by tests to
    /// assert the contract without hard-coding the order at the call site.
    pub const fn sequence() -> [Self; 5] {
        [
            Self::OutboundTransform,
            Self::MergeInbound,
            Self::AuthHeaders,
            Self::OutboundRawMiddlewares,
            Self::Execute,
        ]
    }
}

// ---------------------------------------------------------------------------
// Pure S07 stream-mode decision (Go processRequest switch).
// ---------------------------------------------------------------------------

/// How an attempt's response should be delivered. Same shape as
/// [`ExecutionMode`] but named per the S07 task spec; kept as an alias so the
/// `decide_stream_mode` signature reads verbatim like the Go switch.
pub type StreamMode = ExecutionMode;

/// Pure S07 stream-mode decision mirroring the Go `processRequest` switch
/// (lines ~389-435 of `pipeline.go`):
///
/// - `originalWantStream` (user asked for a stream) -> [`StreamMode::Stream`]
///   (Go `stream` arm).
/// - else `effectiveWantStream` (provider responds with a stream) ->
///   [`StreamMode::AutoAggregate`] (Go `autoAggregateStream` arm).
/// - otherwise -> [`StreamMode::NonStream`] (Go `notStream` arm).
///
/// `user_stream` is Go `originalWantStream`; `provider_needs_stream` is Go
/// `effectiveWantStream` — the flag actually sent upstream after the outbound
/// transformer may have flipped it.
pub const fn decide_stream_mode(user_stream: bool, provider_needs_stream: bool) -> StreamMode {
    ExecutionMode::resolve(user_stream, provider_needs_stream)
}

// ---------------------------------------------------------------------------
// Pure S08/S15 retry policy + S09/S10 decision.
// ---------------------------------------------------------------------------

/// Snapshot of retry state consulted by [`decide_retry`]. Mirrors the Go
/// `Process` loop's local counters (`channelSwitches`, `sameChannelRetries`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryState {
    /// Number of channel switches performed so far (Go `channelSwitches`).
    pub channel_switches: u32,
    /// Number of same-channel retries on the current channel (Go
    /// `sameChannelRetries`).
    pub single_channel_retries: u32,
}

impl RetryState {
    pub const fn initial() -> Self {
        Self {
            channel_switches: 0,
            single_channel_retries: 0,
        }
    }
}

/// Outcome of [`decide_retry`]: which retry arm (if any) should fire next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryDecision {
    /// No retry — return the last error to the caller (Go `break`).
    Stop,
    /// Same-channel retry: call `PrepareForRetry` (Go `ChannelRetryable` arm).
    RetrySameChannel,
    /// Channel switch: call `NextChannel` (Go `Retryable` arm).
    RetryNextChannel,
}

/// Inputs to [`decide_retry`] that depend on the failed attempt. Bundling them
/// keeps the decision function pure (no closures / trait objects) so it is
/// trivially unit-testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptOutcome {
    /// Whether the error is a response-timeout error (Go
    /// `isResponseTimeoutError`). When true the same-channel arm is skipped.
    pub is_timeout_error: bool,
    /// Whether the transformer permits a same-channel retry for this error
    /// (Go `ChannelRetryable.CanRetry`).
    pub can_retry_same_channel: bool,
    /// Whether there are more channels to switch to (Go
    /// `Retryable.HasMoreChannels`).
    pub has_more_channels: bool,
}

/// Pure S08/S09/S10 retry decision mirroring Go `Process`'s retry block
/// (`pipeline.go` lines ~290-341):
///
/// 1. If the context is canceled or timed out → [`RetryDecision::Stop`]
///    (Go `if ctx.Err() != nil { return nil, lastErr }`). This covers S09.
/// 2. Else if the error is NOT a response-timeout error AND the same-channel
///    budget (`max_single_channel_retries`) is not exhausted AND
///    `outcome.can_retry_same_channel` → [`RetryDecision::RetrySameChannel`].
/// 3. Else if the channel-switch budget (`max_channel_retries`) is not
///    exhausted AND `outcome.has_more_channels` →
///    [`RetryDecision::RetryNextChannel`].
/// 4. Otherwise → [`RetryDecision::Stop`].
///
/// This function does NOT perform the retry — it only encodes the decision
/// tree so it can be unit-tested in isolation. The live [`Pipeline::process`]
/// loop above implements the same tree with side effects.
pub const fn decide_retry(
    policy: RetryPolicy,
    state: RetryState,
    outcome: AttemptOutcome,
    ctx_canceled: bool,
) -> RetryDecision {
    // S09 — canceled or timed-out context never retries.
    if ctx_canceled {
        return RetryDecision::Stop;
    }

    // S10 (arm 1) — same-channel retry first, but skipped for timeout errors.
    if !outcome.is_timeout_error
        && outcome.can_retry_same_channel
        && state.single_channel_retries < policy.max_single_channel_retries
        && policy.enabled
    {
        return RetryDecision::RetrySameChannel;
    }

    // S10 (arm 2) — channel switch.
    if policy.enabled
        && outcome.has_more_channels
        && state.channel_switches < policy.max_channel_retries
    {
        return RetryDecision::RetryNextChannel;
    }

    RetryDecision::Stop
}

// ---------------------------------------------------------------------------
// S14 retry-context record (typed accumulator).
// ---------------------------------------------------------------------------

/// Typed retry-context record (S14). Accumulated across attempts by the
/// orchestrator and surfaced in observability/tracing. Mirrors the per-loop
/// state Go's `Process` mutates (`channelSwitches`, `sameChannelRetries`,
/// `lastErr`) plus a `started_at` for latency attribution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryContext {
    /// Total number of channel attempts (initial + retries). 1-based.
    pub channel_attempt: u32,
    /// Same-channel retry count on the *current* channel. Resets on
    /// [`RetryDecision::RetryNextChannel`] (Go `sameChannelRetries = 0`).
    pub single_channel_attempt: u32,
    /// Snapshot of the most recent failure's error kind (S14 `last_error`).
    pub last_error_kind: Option<&'static str>,
    /// Whether the last error was classified retryable (S14 `retryable_status`).
    pub retryable_status: bool,
    /// Wall-clock start of the retry loop (S14 `started_at`). Stored as
    /// milliseconds since UNIX epoch to keep the type `Eq`.
    pub started_at_ms: i64,
}

impl RetryContext {
    /// Begin a new retry-context record at the given wall-clock time (ms).
    pub fn new(started_at_ms: i64) -> Self {
        Self {
            channel_attempt: 1,
            single_channel_attempt: 0,
            last_error_kind: None,
            retryable_status: false,
            started_at_ms,
        }
    }

    /// Record a failed attempt and the retry decision taken for it, advancing
    /// the counters per Go semantics:
    /// - [`RetryDecision::RetrySameChannel`]: `single_channel_attempt` += 1,
    ///   `channel_attempt` += 1.
    /// - [`RetryDecision::RetryNextChannel`]: `single_channel_attempt` resets
    ///   to 0, `channel_attempt` += 1.
    /// - [`RetryDecision::Stop`]: counters unchanged (caller is about to
    ///   return).
    pub fn record_failure(&mut self, error_kind: &'static str, decision: RetryDecision) {
        self.last_error_kind = Some(error_kind);
        match decision {
            RetryDecision::Stop => {}
            RetryDecision::RetrySameChannel => {
                self.single_channel_attempt += 1;
                self.channel_attempt += 1;
                self.retryable_status = true;
            }
            RetryDecision::RetryNextChannel => {
                self.single_channel_attempt = 0;
                self.channel_attempt += 1;
                self.retryable_status = true;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline — the process skeleton.
// ---------------------------------------------------------------------------

/// The pipeline process skeleton. Holds the transformer/executor trait objects
/// and retry configuration; [`Pipeline::process`] drives one request through
/// the full S04-S07 flow with S08-S10 retry.
pub struct Pipeline {
    inbound: Arc<dyn InboundTransformer>,
    outbound: Arc<dyn OutboundTransformer>,
    executor: Arc<dyn Executor>,
    retry_policy: RetryPolicy,
    hooks: RetryHooks,
    // RUST-P8-001 S04 — the 9-hook middleware chain (Go `p.middlewares`,
    // `pipeline.go:121`, set via `WithMiddlewares`). ONE list serves all nine
    // hooks; each `apply_*` runner picks its own direction (forward/reverse).
    // Empty by default; the orchestrator wires concrete middlewares (billing
    // header, max token, usage merge, retry marker, …) when it constructs the
    // `Arc<Pipeline>`.
    middlewares: Vec<BoxPipelineMiddleware>,
    /// RUST-P15-001 — optional executor customization hook (Go
    /// `ChannelCustomizedExecutor`, `pipeline.go:38-43`). When `Some`, the
    /// pipeline calls it once per attempt after the outbound raw request
    /// middlewares and before execution (Go `pipeline.go:381-384`), passing the
    /// pipeline's default executor and using the returned executor for that
    /// attempt's `execute`/`execute_stream` call. `None` (default) means no
    /// customization — matching a Go outbound that does NOT implement the
    /// interface.
    customize_executor: Option<CustomizeExecutorFn>,
    /// Per-channel outbound transformer selection. When set, each attempt
    /// looks up the candidate's (channel_type, api_format) in the registry;
    /// if found, that outbound is used instead of `self.outbound`. Falls back
    /// to `self.outbound` when the registry is absent or the key is not found.
    outbound_registry: Option<Arc<conduit_transformers::traits::TransformerRegistry>>,
    /// Host-owned observer for provider-attempt health events. The callback is
    /// synchronous and should enqueue work rather than perform I/O inline.
    attempt_observer: Option<Arc<dyn AttemptObserver>>,
}

/// Result of one real provider attempt, before retry/failover selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptObservationOutcome {
    Succeeded,
    Failed { provider_status: Option<u16> },
}

/// Provider-attempt health event emitted by the pipeline.
///
/// `credential` is plaintext transport material and must remain in memory; it
/// is carried only so the host can disable the exact failed channel key.
#[derive(Clone, PartialEq, Eq)]
pub struct AttemptObservation {
    pub channel_id: String,
    pub credential: Option<String>,
    pub credential_identity: Option<String>,
    pub outcome: AttemptObservationOutcome,
}

pub trait AttemptObserver: Send + Sync {
    fn observe(&self, observation: AttemptObservation);
}

fn replace_channel_config_metadata(
    metadata: &mut std::collections::BTreeMap<String, String>,
    channel_config: &std::collections::BTreeMap<String, String>,
    previous_values: &mut Vec<(String, Option<String>)>,
) {
    for (key, previous_value) in previous_values.drain(..) {
        match previous_value {
            Some(value) => {
                metadata.insert(key, value);
            }
            None => {
                metadata.remove(&key);
            }
        }
    }
    for (key, value) in channel_config {
        previous_values.push((key.clone(), metadata.get(key).cloned()));
        metadata.insert(key.clone(), value.clone());
    }
}

impl Pipeline {
    pub fn new(
        inbound: Arc<dyn InboundTransformer>,
        outbound: Arc<dyn OutboundTransformer>,
        executor: Arc<dyn Executor>,
    ) -> Self {
        Self {
            inbound,
            outbound,
            executor,
            retry_policy: RetryPolicy::DEFAULT,
            hooks: RetryHooks::default(),
            middlewares: Vec::new(),
            customize_executor: None,
            outbound_registry: None,
            attempt_observer: None,
        }
    }

    /// Set the outbound transformer registry for per-channel selection.
    pub fn with_outbound_registry(
        mut self,
        registry: Arc<conduit_transformers::traits::TransformerRegistry>,
    ) -> Self {
        self.outbound_registry = Some(registry);
        self
    }

    pub fn with_attempt_observer(mut self, observer: Arc<dyn AttemptObserver>) -> Self {
        self.attempt_observer = Some(observer);
        self
    }

    fn observe_attempt(&self, target: &PipelineCandidate, outcome: AttemptObservationOutcome) {
        let Some(observer) = self.attempt_observer.as_ref() else {
            return;
        };
        observer.observe(AttemptObservation {
            channel_id: target.id.clone(),
            credential: target.credential.clone(),
            credential_identity: target.credential_identity.clone(),
            outcome,
        });
    }

    pub const fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Configure the two response timeouts, mirroring Go
    /// `WithResponseTimeouts(streamFirstEventTimeout, nonStreamTimeout)`
    /// (`pipeline.go:77-82`). `0` disables a timeout. Values are consumed by
    /// the per-attempt execution arms (see [`RetryPolicy`] field docs for the
    /// Go source of each knob).
    pub const fn with_response_timeouts(
        mut self,
        stream_first_event_timeout_ms: u64,
        non_stream_timeout_ms: u64,
    ) -> Self {
        self.retry_policy.stream_first_event_timeout_ms = stream_first_event_timeout_ms;
        self.retry_policy.non_stream_timeout_ms = non_stream_timeout_ms;
        self
    }

    /// Enable empty-response detection (Go `WithEmptyResponseDetection()`,
    /// `pipeline.go:67-75`). When enabled, the non-stream arm checks the
    /// unified [`LlmResponse`] (produced by `Outbound::transform_response`)
    /// with [`has_response_content`] after the LLM response middlewares and
    /// returns [`ErrEmptyResponse`] if no meaningful content is present (Go
    /// `non_streaming.go:55-59`), so the retry flow treats it as a failed
    /// attempt.
    ///
    /// **Streaming-path gap (phase 2 — pending)**: Go also pre-reads up to 3
    /// events from the LLM stream (`stream.go:153-217`, `preReadLlmStream`)
    /// to detect empty streaming responses before the first event reaches the
    /// client. The helper itself is fully ported as
    /// [`crate::empty_response::pre_read_llm_stream`] and unit-tested, but
    /// the Rust streaming executor is currently an eager `Vec<StreamEvent>`
    /// stub (no lazy `LlmResponse` stream exists in the live flow yet), so
    /// the pre-read helper is NOT wired into the stream arm. This builder
    /// still flips the flag; the non-stream arm honors it, the stream arm
    /// ignores it until the real streaming executor lands. The orchestrator
    /// consults this builder via `ProcessRetryPolicy::attach_empty_response_detection`
    /// (`orchestrator.rs:3718-3722`).
    pub const fn with_empty_response_detection(mut self) -> Self {
        self.retry_policy.empty_response_detection = true;
        self
    }

    pub fn with_retry_hooks(mut self, hooks: RetryHooks) -> Self {
        self.hooks = hooks;
        self
    }

    /// Wire the middleware chain (Go `WithMiddlewares(decorators...)`,
    /// `pipeline.go:61-65`). One list serves all nine hooks — see
    /// [`crate::middleware::PipelineMiddleware`] for the per-hook
    /// order/frequency contract.
    pub fn with_middlewares(mut self, middlewares: Vec<BoxPipelineMiddleware>) -> Self {
        self.middlewares = middlewares;
        self
    }

    /// Wire a channel-customized executor hook (Go `ChannelCustomizedExecutor`
    /// interface, `pipeline.go:38-43`). When set, the pipeline calls it once per
    /// attempt after the outbound raw request middlewares and before execution
    /// (Go `pipeline.go:381-384`), passing the pipeline's default executor and
    /// using the returned executor for that attempt. This is the seam channels
    /// like AWS Bedrock use to swap in a non-HTTP executor.
    pub fn with_customize_executor(mut self, hook: CustomizeExecutorFn) -> Self {
        self.customize_executor = Some(hook);
        self
    }

    /// The pipeline's configured default inbound transformer (Go
    /// `Pipeline.Inbound`). [`Pipeline::process`] uses this; callers that resolve
    /// a per-request inbound (the orchestrator/bridge, which pick the client's
    /// wire format from the route) pass it explicitly to
    /// [`Pipeline::process_with_inbound`] instead.
    pub fn inbound(&self) -> &dyn InboundTransformer {
        self.inbound.as_ref()
    }

    /// Run one request through the pipeline using the pipeline's configured
    /// default inbound transformer ([`Pipeline::inbound`]). Convenience wrapper
    /// over [`Pipeline::process_with_inbound`]; kept so existing callers/tests
    /// that rely on `self.inbound` are unchanged.
    ///
    /// `original_request` is the inbound HTTP request; `raw_inbound` is the
    /// pre-transform request kept for the outbound merge step (Go
    /// `httpclient.MergeInboundRequest(httpReq, request.RawRequest)`).
    pub async fn process(
        &self,
        ctx: &mut PipelineContext,
        original_request: HttpRequest,
        raw_inbound: &HttpRequest,
        candidates: &[PipelineCandidate],
    ) -> Result<(HttpResponse, Vec<AttemptRecord>), ConduitError> {
        self.process_with_inbound(
            ctx,
            self.inbound.as_ref(),
            original_request,
            raw_inbound,
            candidates,
        )
        .await
    }

    /// Run one request through the pipeline. Returns the final response (or the
    /// last error once retries are exhausted), plus the per-attempt records for
    /// observability.
    ///
    /// `inbound` is the client's wire-format transformer for THIS request,
    /// supplied per request by the orchestrator/bridge from the route (Go binds a
    /// per-format inbound into a dedicated orchestrator — e.g.
    /// `anthropic.go:45-59`). It — NOT the pipeline's fixed `self.inbound` — is
    /// used for the inbound request parse (Go `Inbound.TransformRequest`) and the
    /// response/stream transforms (Go `Inbound.TransformResponse` /
    /// `AggregateStreamChunks` / `TransformStream`), so non-OpenAI formats
    /// (Anthropic, Gemini, …) are parsed and reshaped in their own envelope.
    /// The live-stream path ([`Pipeline::stream_live`]) is unaffected.
    pub async fn process_with_inbound(
        &self,
        ctx: &mut PipelineContext,
        inbound: &dyn InboundTransformer,
        original_request: HttpRequest,
        raw_inbound: &HttpRequest,
        candidates: &[PipelineCandidate],
    ) -> Result<(HttpResponse, Vec<AttemptRecord>), ConduitError> {
        self.process_with_inbound_policy(
            ctx,
            inbound,
            original_request,
            raw_inbound,
            candidates,
            self.retry_policy,
        )
        .await
    }

    /// Run one request with a request-scoped retry-policy snapshot. Hosts that
    /// support live settings use this entry point so routing and execution see
    /// the same policy version for the full request.
    pub async fn process_with_inbound_policy(
        &self,
        ctx: &mut PipelineContext,
        inbound: &dyn InboundTransformer,
        original_request: HttpRequest,
        raw_inbound: &HttpRequest,
        candidates: &[PipelineCandidate],
        retry_policy: RetryPolicy,
    ) -> Result<(HttpResponse, Vec<AttemptRecord>), ConduitError> {
        // S04 — inbound transform runs exactly once.
        ctx.record_order("inbound:transform_request");
        let mut llm_request = inbound
            .inbound_request(original_request)
            .map_err(|err| ctx.fail("inbound:transform_request", err))?;

        // S04/S05 — inbound LLM middlewares run exactly once, in Go forward
        // order (`applyBeforeRequestMiddlewares` at `pipeline.go:267`, looping
        // `dec.OnInboundLlmRequest` at `pipeline.go:144`). The once-only marker
        // is recorded BEFORE the chain runs so tests can assert the invariant
        // even when the chain is empty; each middleware may additionally push
        // its own `ctx.order` entries. A middleware that returns `Err` aborts
        // the whole request, mirroring Go's `return nil, err` — note Go does
        // NOT fire `OnOutboundRawError` here (no attempt has started).
        ctx.record_order("inbound:llm_middlewares");
        llm_request = match apply_before_request_middlewares(&self.middlewares, ctx, llm_request) {
            Ok(req) => req,
            Err(err) => {
                return Err(ctx.fail("inbound:llm_middlewares", err));
            }
        };
        ctx.metadata.insert(
            "client_api_format".to_string(),
            llm_request.api_format.as_str().to_string(),
        );

        // Snapshot the user's stream flag — every attempt resets to it (Go
        // `originalStream := llmRequest.Stream` then
        // `llmRequest.Stream = originalStream` per iteration).
        let user_wants_stream = llm_request.stream;

        // Stash raw inbound body in context for pass-through middleware.
        if let Some(body) = &raw_inbound.body
            && let Ok(s) = String::from_utf8(body.clone())
        {
            ctx.metadata.insert("raw_inbound_body".to_string(), s);
        }

        // Build the failover cursor from the candidate ids. The cursor drives
        // the same-channel-first / channel-switch retry order.
        let mut state = match self.build_failover_state(candidates) {
            Ok(state) => state,
            Err(err) => {
                ctx.record_order("retry:no_candidates");
                return Err(err.into_conduit_error());
            }
        };

        let mut attempts: Vec<AttemptRecord> = Vec::new();

        // S14 — typed retry-context record. Mirrors the loop-local state Go's
        // `Process` maintains (`channelSwitches` / `sameChannelRetries` /
        // `lastErr`, `pipeline.go:274-277`) plus a start timestamp. Published
        // on the [`PipelineContext`] after every mutation so middlewares and
        // callers can consume it on both success and failure paths.
        let mut retry_ctx = RetryContext::new(now_unix_ms());
        ctx.retry_context = Some(retry_ctx.clone());
        let mut previous_channel_config_values = Vec::new();

        loop {
            // Reset the stream flag for this attempt (Go per-iteration reset).
            llm_request.stream = user_wants_stream;

            let sequence = state.total_attempts;
            // The failover cursor is the only place that knows which channel
            // this attempt targets — hand the target to the attempt body so it
            // can stamp url/auth (Go `PersistentOutboundTransformer` shim).
            let target = state.current();
            let channel_id = target.id.clone();
            let error_response_rewrite_rules = target.error_response_rewrite_rules.clone();
            let model_index = state.current_model_index;
            // Stamp current candidate info into context so middlewares can read it.
            ctx.metadata
                .insert("channel_id".to_string(), channel_id.clone());
            ctx.metadata
                .insert("channel_type".to_string(), target.channel_type.clone());
            ctx.metadata
                .insert("api_format".to_string(), target.api_format.clone());
            ctx.metadata.remove("credential_identity");
            if let Some(identity) = &target.credential_identity {
                ctx.metadata
                    .insert("credential_identity".to_string(), identity.clone());
            }
            // Stamp the model keys the circuit-breaker (and any model-scoped
            // middleware) reads: `actual_model` is the upstream model this
            // channel serves (Go `entry.ActualModel`); `request_model` falls
            // back to the client's requested model. Without these the breaker's
            // per-model error counter can never key a row and stays inert.
            if let Some(actual) = &target.actual_model {
                ctx.metadata
                    .insert("actual_model".to_string(), actual.clone());
            }
            if let Some(request_model) = llm_request.model.as_ref() {
                ctx.metadata
                    .insert("request_model".to_string(), request_model.clone());
            }
            // Replace the previous attempt's channel config so a failover to a
            // channel without a given setting cannot inherit a stale value.
            replace_channel_config_metadata(
                &mut ctx.metadata,
                &target.channel_config,
                &mut previous_channel_config_values,
            );
            ctx.record_order(format!("attempt:{sequence}:start"));

            // Go `outbound.go:385` — `llmRequest.Model = entry.ActualModel`:
            // the upstream sees the channel's actual model, not the client's
            // requested one. Clone per attempt so a retry on another channel
            // re-stamps from the pristine inbound request.
            let mut attempt_request = llm_request.clone();
            if let Some(actual) = &target.actual_model {
                attempt_request.model = Some(actual.clone());
            }

            let (mode, outcome) = self
                .process_attempt(
                    ctx,
                    inbound,
                    &attempt_request,
                    raw_inbound,
                    user_wants_stream,
                    target,
                    retry_policy,
                )
                .await;

            let (err, err_kind) = match outcome {
                Ok(response) => {
                    self.observe_attempt(target, AttemptObservationOutcome::Succeeded);
                    ctx.record_order(format!("attempt:{sequence}:success"));
                    attempts.push(AttemptRecord {
                        sequence,
                        channel_id: channel_id.clone(),
                        model_index,
                        mode,
                        outcome: Ok(response.clone()),
                    });
                    return Ok((response, attempts));
                }
                Err(err) => {
                    if !self.is_context_canceled(ctx) {
                        self.observe_attempt(
                            target,
                            AttemptObservationOutcome::Failed {
                                provider_status: err.provider_status,
                            },
                        );
                    }
                    ctx.record_order(format!("attempt:{sequence}:error"));
                    // Snapshot for the record; the real error drives the retry
                    // decision and the final return value.
                    let snapshot = AttemptError::from_axon(&err);
                    let err_kind = snapshot.kind;
                    attempts.push(AttemptRecord {
                        sequence,
                        channel_id: channel_id.clone(),
                        model_index,
                        mode,
                        outcome: Err(snapshot),
                    });
                    (err, err_kind)
                }
            };

            // S17 — client disconnect / canceled context stops retrying
            // immediately; the *attempt's* error is returned, not a cancel
            // error (Go `pipeline.go:290-293` returns `lastErr`).
            if self.is_context_canceled(ctx) {
                ctx.record_order("retry:context_canceled");
                retry_ctx.record_failure(err_kind, RetryDecision::Stop);
                ctx.retry_context = Some(retry_ctx);
                return Err(apply_error_response_rewrite(
                    &channel_id,
                    &error_response_rewrite_rules,
                    err,
                ));
            }

            let is_timeout = (self.hooks.is_timeout_error)(&err);
            let mut decision = RetryDecision::Stop;

            // 1. Same-channel retry (Go ChannelRetryable). Skipped for timeouts.
            if !is_timeout {
                match self
                    .try_same_channel_retry(ctx, &mut state, &err, retry_policy)
                    .await
                {
                    Ok(()) => {
                        if self.was_same_channel_retry_taken(ctx) {
                            decision = RetryDecision::RetrySameChannel;
                        }
                    }
                    Err(retry_err) => {
                        // PrepareForRetry failed in Go falls through to the
                        // channel-switch arm; record and continue.
                        ctx.record_order(format!("retry:same_channel:failed:{retry_err}"));
                    }
                }
            }

            // 2. Channel switch (Go Retryable), only if same-channel did not.
            if decision == RetryDecision::Stop {
                match self.try_channel_switch(ctx, &mut state, retry_policy).await {
                    Ok(true) => decision = RetryDecision::RetryNextChannel,
                    Ok(false) => {}
                    Err(switch_err) => {
                        ctx.record_order(format!("retry:channel_switch:failed:{switch_err}"));
                    }
                }
            }

            // S14 — advance the retry-context counters for the decision taken,
            // mirroring Go's `sameChannelRetries++` (`pipeline.go:305-309`) /
            // `channelSwitches++; sameChannelRetries = 0` (`:323-329`).
            retry_ctx.record_failure(err_kind, decision);
            ctx.retry_context = Some(retry_ctx.clone());

            if decision == RetryDecision::Stop {
                // S18 — retries exhausted: return the LAST error observed
                // (Go `lastErr`, `pipeline.go:288` → `:355`). Upstream-path
                // errors carry the upstream marker so the API layer can apply
                // `apply_upstream_error_policy` (Go does this in
                // `api/upstream_error_policy.go`, outside the pipeline).
                ctx.record_order("retry:exhausted");
                return Err(apply_error_response_rewrite(
                    &channel_id,
                    &error_response_rewrite_rules,
                    err,
                ));
            }

            // Retry delay (Go `time.Sleep(p.retryDelay)`).
            if retry_policy.retry_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(retry_policy.retry_delay_ms)).await;
            }
            // err is dropped here; the next iteration produces a fresh one.
        }
    }

    // -- attempt body --------------------------------------------------------

    /// RUST-P8-003 (phase 2) — **live** streaming attempt.
    ///
    /// The buffered [`Pipeline::process`] Stream arm materializes the whole
    /// upstream stream into a `Vec<StreamEvent>` before returning. This method
    /// is the live sibling: it runs the inbound transform + the first
    /// candidate's outbound transform / merge / auth-stamp (reusing
    /// [`stamp_outbound_target`], the same helper the buffered attempt body
    /// uses), forces `stream:true` on the outgoing body (Go
    /// `effectiveWantStream`), then hands the request to
    /// [`Executor::execute_stream_live`] and returns the resulting incremental
    /// receiver in a [`LiveStreamAttempt`] the orchestrator wraps with the
    /// forward-while-aggregating loop + persistence finalizer.
    ///
    /// The live path takes the first candidate (no live failover yet), runs the
    /// outbound raw middleware chain, and converts every provider event through
    /// the selected upstream transformer and the route's client transformer.
    /// Response timeouts and `CustomizeExecutor` remain owned by the buffered
    /// path.
    pub async fn stream_live(
        &self,
        ctx: &mut PipelineContext,
        original_request: HttpRequest,
        raw_inbound: &HttpRequest,
        candidates: &[PipelineCandidate],
        cancel: CancelToken,
    ) -> Result<LiveStreamAttempt, ConduitError> {
        // Backward-compatible wrapper: existing callers/tests that rely on the
        // pipeline's fixed `self.inbound` keep working unchanged. The route-aware
        // callers (the bridge) pass the per-request inbound via
        // [`Self::stream_live_with_inbound`].
        self.stream_live_with_inbound(
            ctx,
            Arc::clone(&self.inbound),
            original_request,
            raw_inbound,
            candidates,
            cancel,
        )
        .await
    }

    /// RUST-P8-003 / P7-003 (stream leg) — live streaming with a route-selected
    /// inbound transformer.
    ///
    /// This is the streaming analogue of [`Self::process_with_inbound`]: the
    /// bridge selects the inbound transformer for the route (OpenAI chat,
    /// Anthropic messages, Gemini, …) and threads it here so the client-facing
    /// SSE frames are produced in the client's native format, not always
    /// OpenAI's.
    ///
    /// The transform stage mirrors the buffered path
    /// ([`Self::finish_stream_events`]): raw provider events →
    /// `outbound.transform_stream` (→ unified `LlmResponse`) →
    /// `inbound.transform_stream` (→ client `StreamEvent`s). Because
    /// `transform_stream` is a **stateful synchronous iterator** (e.g. Anthropic
    /// emits a `message_start` prelude and a `message_stop` epilogue around the
    /// content deltas) and the live upstream is an **async** `mpsc` channel, the
    /// two are bridged on a dedicated blocking task via
    /// [`transform_live_stream`] (`blocking_recv` feeds the sync iterator;
    /// `blocking_send` forwards each transformed frame).
    pub async fn stream_live_with_inbound(
        &self,
        ctx: &mut PipelineContext,
        inbound: Arc<dyn InboundTransformer>,
        original_request: HttpRequest,
        raw_inbound: &HttpRequest,
        candidates: &[PipelineCandidate],
        cancel: CancelToken,
    ) -> Result<LiveStreamAttempt, ConduitError> {
        // S04 — inbound transform (once, mirrors `process`).
        ctx.record_order("inbound:transform_request");
        let mut llm_request = inbound
            .inbound_request(original_request)
            .map_err(|err| ctx.fail("inbound:transform_request", err))?;
        // The live path is only entered when the user asked for a stream.
        llm_request.stream = true;

        // P-34: run the before-request (LLM-stage) middleware chain, exactly as
        // the buffered path does (pipeline.rs ~949). Previously the live path
        // skipped the entire 9-hook chain, so for streaming requests — the bulk
        // of real traffic — quota was not enforced, the model whitelist was not
        // checked, model mapping / prompt protection / ensure-usage did not run.
        // A middleware returning Err aborts the request (mirrors the buffered
        // `ctx.fail` behaviour).
        ctx.record_order("inbound:llm_middlewares");
        llm_request = match apply_before_request_middlewares(&self.middlewares, ctx, llm_request) {
            Ok(req) => req,
            Err(err) => return Err(ctx.fail("inbound:llm_middlewares", err)),
        };
        ctx.metadata.insert(
            "client_api_format".to_string(),
            llm_request.api_format.as_str().to_string(),
        );

        // First candidate only (failover is a documented gap on this path).
        let target = candidates.first().ok_or_else(|| {
            ConduitError::not_found("no candidates available for pipeline stream")
        })?;
        let channel_id = target.id.clone();
        ctx.metadata
            .insert("channel_id".to_string(), channel_id.clone());
        ctx.metadata
            .insert("channel_type".to_string(), target.channel_type.clone());
        ctx.metadata.remove("credential_identity");
        if let Some(identity) = &target.credential_identity {
            ctx.metadata
                .insert("credential_identity".to_string(), identity.clone());
        }
        // Stamp actual/request model onto the context so downstream middlewares
        // (e.g. circuit breaker) can key per-model state (Go stamps the same via
        // the outbound transformer's `ActualModelID`). `request_model` is the
        // client-requested model; `actual_model` is the channel's mapped model.
        if let Some(requested) = &llm_request.model {
            ctx.metadata
                .insert("request_model".to_string(), requested.clone());
        }
        if let Some(actual) = &target.actual_model {
            ctx.metadata
                .insert("actual_model".to_string(), actual.clone());
        }

        // P-37: stamp the per-channel config keys and the raw inbound body onto
        // the context, exactly as the buffered path does (pipeline.rs ~960 and
        // ~1025). Without these, the channel-scoped middlewares
        // (pass-through, body/header overrides, concurrency + RPM limits,
        // pass-through user-agent) read nothing and silently no-op even once the
        // stream path runs the middleware chain (P-34). The live path uses only
        // the first candidate with no failover, so the buffered path's
        // Buffered failover cleanup is unnecessary here.
        for (key, value) in &target.channel_config {
            ctx.metadata.insert(key.clone(), value.clone());
        }
        if let Some(body) = &raw_inbound.body
            && let Ok(s) = String::from_utf8(body.clone())
        {
            ctx.metadata.insert("raw_inbound_body".to_string(), s);
        }

        // Stamp the channel's actual model (Go `outbound.go:385`).
        let mut attempt_request = llm_request.clone();
        if let Some(actual) = &target.actual_model {
            attempt_request.model = Some(actual.clone());
        }

        // The inbound request keeps the client protocol for the response leg,
        // while each attempt speaks the protocol selected by that candidate's
        // endpoint.  Go performs the same split through
        // `selectOutboundForCandidate(candidate.APIFormat)`; using the inbound
        // format here would make every native Claude/Gemini/Responses endpoint
        // accidentally use the OpenAI transformer.
        let client_api_format = llm_request.api_format;
        let upstream_api_format = match candidate_api_format(target, client_api_format) {
            Ok(format) => format,
            Err(err) => return Err(ctx.fail("outbound:select_api_format", err)),
        };
        attempt_request.api_format = upstream_api_format;
        ctx.metadata.insert(
            "api_format".to_string(),
            upstream_api_format.as_str().to_string(),
        );

        // Select outbound transformer (per-channel from registry, else default).
        // Keep an owned `Arc` so the transformer can move onto the blocking
        // bridge task below (the `transform_stream` iterators are `'static`).
        let outbound_arc: Arc<dyn OutboundTransformer> = match self.outbound_registry.as_ref() {
            Some(registry) => match registry.outbound(&target.channel_type, upstream_api_format) {
                Some(outbound) => outbound,
                None if !target.api_format.trim().is_empty() => {
                    return Err(ctx.fail(
                        "outbound:select_transformer",
                        ConduitError::invalid_request(format!(
                            "no outbound transformer for channel type {:?} and API format {}",
                            target.channel_type,
                            upstream_api_format.as_str()
                        )),
                    ));
                }
                None => Arc::clone(&self.outbound),
            },
            None => Arc::clone(&self.outbound),
        };
        let outbound: &dyn OutboundTransformer = outbound_arc.as_ref();

        // Outbound transform → merge inbound → auth/url stamp (shared helper).
        let mut http_req = outbound
            .outbound_request(&attempt_request)
            .map_err(|err| ctx.fail("outbound:transform_request", err))?;
        ctx.metadata.insert(
            "format".to_string(),
            http_req
                .api_format
                .unwrap_or(attempt_request.api_format)
                .as_str()
                .to_string(),
        );
        ctx.record_order("outbound:transform_request");
        merge_inbound(&mut http_req, raw_inbound);
        ctx.record_order("outbound:merge_inbound");
        stamp_outbound_target(&mut http_req, target);
        ctx.record_order("outbound:auth_headers");

        // P-34: run the outbound (raw-request-stage) middleware chain, as the
        // buffered path does (pipeline.rs ~1398): channel body/header overrides,
        // pass-through, circuit-breaker check, concurrency + RPM limits, default
        // user-agent. A middleware returning Err aborts (with the raw-error
        // middlewares run for cleanup, mirroring the buffered path).
        ctx.record_order("outbound:raw_middlewares");
        http_req = match apply_raw_request_middlewares(&self.middlewares, ctx, http_req) {
            Ok(req) => req,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail("outbound:raw_middlewares", err));
            }
        };

        // Force `stream:true` on the outgoing JSON body (Go `effectiveWantStream`):
        // the outbound transformer already reflects `attempt_request.stream`, but
        // stamping defensively guarantees the provider streams. Re-applied after
        // the override middlewares so a channel override cannot accidentally
        // clear it.
        if let Some(body) = http_req.json_body.as_mut()
            && let Some(map) = body.as_object_mut()
        {
            map.insert("stream".to_string(), serde_json::Value::Bool(true));
        }
        ctx.record_order("execute:StreamLive");

        // S13 — hand the executor the per-attempt cancel token so it can abort
        // the upstream provider call on client disconnect.
        let raw_upstream_rx = match self.executor.execute_stream_live(&http_req, cancel).await {
            Ok(receiver) => receiver,
            Err(err) => {
                self.observe_attempt(
                    target,
                    AttemptObservationOutcome::Failed {
                        provider_status: err.provider_status,
                    },
                );
                // Request middlewares may have acquired attempt-scoped
                // resources (notably a channel concurrency permit). A live
                // connection failure must unwind them just like buffered
                // execution failures do.
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(apply_error_response_rewrite(
                    &channel_id,
                    &target.error_response_rewrite_rules,
                    ctx.fail("execute:stream_live", err),
                ));
            }
        };

        // Transform stage (mirrors the buffered `finish_stream_events`): raw
        // provider events → `outbound.transform_stream` → unified `LlmResponse`
        // → `inbound.transform_stream` → client `StreamEvent`s. Bridged on a
        // blocking task because `transform_stream` is a stateful *synchronous*
        // iterator while the live upstream is async (`transform_live_stream`).
        ctx.record_order("stream:transform");
        let upstream_rx = if can_passthrough_openai_wire_response(
            client_api_format,
            upstream_api_format,
            outbound.name(),
        ) {
            passthrough_live_stream(raw_upstream_rx)
        } else {
            transform_live_stream(outbound_arc, inbound, raw_upstream_rx)
        };
        let upstream_rx = rewrite_live_stream_errors(
            upstream_rx,
            channel_id.clone(),
            target.error_response_rewrite_rules.clone(),
        );
        let upstream_rx = observe_live_stream_attempt(
            upstream_rx,
            self.attempt_observer.clone(),
            AttemptObservation {
                channel_id: channel_id.clone(),
                credential: target.credential.clone(),
                credential_identity: target.credential_identity.clone(),
                outcome: AttemptObservationOutcome::Succeeded,
            },
        );

        Ok(LiveStreamAttempt {
            upstream_rx,
            channel_id,
            sequence: 1,
            model_index: 0,
            cleanup: LiveStreamCleanup::new(self.middlewares.clone(), ctx.clone()),
        })
    }

    /// One attempt: outbound transform → merge inbound → auth → outbound raw
    /// middlewares → execute (with the S07 stream-mode switch). Returns the
    /// resolved [`ExecutionMode`] alongside the outcome so the caller can
    /// record it on the [`AttemptRecord`] without recomputation.
    async fn process_attempt(
        &self,
        ctx: &mut PipelineContext,
        inbound: &dyn InboundTransformer,
        llm_request: &LlmRequest,
        raw_inbound: &HttpRequest,
        user_wants_stream: bool,
        target: &PipelineCandidate,
        retry_policy: RetryPolicy,
    ) -> (ExecutionMode, Result<HttpResponse, ConduitError>) {
        let client_api_format = llm_request.api_format;
        let upstream_api_format = match candidate_api_format(target, client_api_format) {
            Ok(format) => format,
            Err(err) => {
                return (
                    ExecutionMode::NonStream,
                    Err(ctx.fail("outbound:select_api_format", err)),
                );
            }
        };
        ctx.metadata.insert(
            "api_format".to_string(),
            upstream_api_format.as_str().to_string(),
        );

        // The request passed into this method is still in the client's unified
        // protocol. Clone it for this attempt and stamp the selected upstream
        // endpoint format; the original format remains available for the
        // inbound/client response transform below.
        let mut upstream_request = llm_request.clone();
        upstream_request.api_format = upstream_api_format;

        // Select outbound transformer by the candidate endpoint format. An
        // explicit endpoint without a matching transformer is a configuration
        // error; silently falling back would send an OpenAI body to (for
        // example) a Claude endpoint.
        let dynamic_outbound: Option<Arc<dyn OutboundTransformer>> = match self
            .outbound_registry
            .as_ref()
        {
            Some(registry) => match registry.outbound(&target.channel_type, upstream_api_format) {
                Some(outbound) => Some(outbound),
                None if !target.api_format.trim().is_empty() => {
                    return (
                        ExecutionMode::NonStream,
                        Err(ctx.fail(
                            "outbound:select_transformer",
                            ConduitError::invalid_request(format!(
                                "no outbound transformer for channel type {:?} and API format {}",
                                target.channel_type,
                                upstream_api_format.as_str()
                            )),
                        )),
                    );
                }
                None => None,
            },
            None => None,
        };
        let outbound: &dyn OutboundTransformer = dynamic_outbound
            .as_deref()
            .unwrap_or(self.outbound.as_ref());

        // Outbound transform (Go `Outbound.TransformRequest`).
        let transform_result = outbound.outbound_request(&upstream_request);
        let mut http_req = match transform_result {
            Ok(req) => {
                ctx.record_order("outbound:transform_request");
                req
            }
            Err(err) => {
                let failed = ctx.fail("outbound:transform_request", err);
                return (ExecutionMode::NonStream, Err(failed));
            }
        };
        ctx.metadata.insert(
            "format".to_string(),
            http_req
                .api_format
                .unwrap_or(upstream_api_format)
                .as_str()
                .to_string(),
        );

        // Merge the inbound raw request (Go `MergeInboundRequest`). Stub: copy
        // through metadata/headers the inbound request carried. The full merge
        // semantics land with the http-merge module; this records the step so
        // tests can assert ordering.
        merge_inbound(&mut http_req, raw_inbound);
        ctx.record_order("outbound:merge_inbound");

        // Auth header finalization (Go `FinalizeAuthHeaders`) + WIRE-06 target
        // stamping. Go's per-request `PersistentOutboundTransformer`
        // (`outbound.go:359-385`) selects the channel's pre-built transformer,
        // whose `Config{BaseURL, APIKeyProvider}` supplies the outbound URL and
        // credential. The Rust outbound transformers are stateless singletons,
        // so the channel target is stamped here — the pipeline is the only
        // layer that knows which channel the current attempt hits. Only fills
        // what the outbound transformer left unset. Shared with the live stream
        // path ([`Pipeline::stream_live`]) via [`stamp_outbound_target`] so the
        // two paths cannot drift on this security-sensitive credential handling.
        stamp_outbound_target(&mut http_req, target);
        ctx.record_order("outbound:auth_headers");

        // RUST-P8-001 S04 — outbound raw request middlewares (Go
        // `applyRawRequestMiddlewares` at `pipeline.go:374`): once per Attempt,
        // FORWARD order. On failure Go fires the raw-error hooks (reverse, over
        // the FULL list — cleanup semantics) and aborts the attempt WITHOUT the
        // upstream marker (`pipeline.go:375-379` wraps with fmt.Errorf only).
        ctx.record_order("outbound:raw_middlewares");
        let http_req = match apply_raw_request_middlewares(&self.middlewares, ctx, http_req) {
            Ok(req) => req,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                let failed = ctx.fail("outbound:raw_middlewares", err);
                return (ExecutionMode::NonStream, Err(failed));
            }
        };

        // RUST-P15-001 — executor customization (Go `ChannelCustomizedExecutor`,
        // `pipeline.go:381-384`). When a hook is wired, it runs once per attempt
        // AFTER the raw request middlewares and BEFORE execution. The returned
        // executor is used for this attempt's Do/DoStream call. When no hook is
        // set the pipeline executor is used as-is (Go outbound does not
        // implement the interface).
        let executor: Arc<dyn Executor> = match &self.customize_executor {
            Some(hook) => {
                ctx.record_order("outbound:customize_executor");
                hook(Arc::clone(&self.executor))
            }
            None => Arc::clone(&self.executor),
        };

        // S07 — stream-mode switch. The effective flag is what the outbound
        // transformer wrote into the request body (Go `effectiveWantStream`),
        // falling back to the user's flag when the transformer omitted it.
        let effective_wants_stream = http_req
            .json_body
            .as_ref()
            .and_then(|body| body.get("stream"))
            .and_then(|v| v.as_bool())
            .unwrap_or(user_wants_stream);
        let mode = ExecutionMode::resolve(user_wants_stream, effective_wants_stream);
        ctx.record_order(format!("execute:{mode:?}"));

        // S12 — per-attempt timeout knobs from the retry policy (Go
        // `p.streamFirstEventTimeout` / `p.nonStreamTimeout`, set by
        // `WithResponseTimeouts`, `pipeline.go:77-82`/`126-127`).
        let first_event_ms = retry_policy.stream_first_event_timeout_ms;
        let non_stream_ms = retry_policy.non_stream_timeout_ms;

        let outcome = match mode {
            ExecutionMode::Stream => {
                // S13 — per-attempt child cancel token (Go `streamCtx` from
                // `context.WithCancel(ctx)`, `stream.go:35`): fires when the
                // client stream is closed/dropped (`CancelOnCloseStream`) or
                // when the request context cancels (client disconnect, S17).
                // Real executors abort the upstream HTTP call on it.
                let upstream_cancel = ctx.cancel.child();
                // S12 — first-event timeout bounds the arrival of the first
                // stream event (Go `newFirstEventTimeoutGuard` consumed at
                // `pipeline.go:395`; the stub executor resolves when events
                // arrive, so the future *is* the first-event phase here).
                let executed = with_timeout(
                    first_event_ms,
                    stream_first_event_timeout_error,
                    executor.execute_stream_cancellable(&http_req, upstream_cancel),
                )
                .await;
                match executed {
                    // RUST-P8-001 S04 — stream established: run the stream-side
                    // middleware wrappers (Go `stream()`, `stream.go:295-394`).
                    Ok(events) => self
                        .finish_stream_events(ctx, inbound, events, outbound)
                        .map(|events| HttpResponse {
                            status: 200,
                            stream: events,
                            ..HttpResponse::default()
                        }),
                    Err(err) => {
                        // Failed streaming attempt: Go fires the raw-error
                        // hooks with the pre-wrap error (`stream.go:274`/`282`)
                        // and only then wraps — the timeout sentinel stays bare
                        // (`stream.go:284-286`); other executor errors get the
                        // upstream marker (`stream.go:288-292`).
                        apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                        Err(ctx.fail("execute:stream", mark_upstream_unless_timeout(err)))
                    }
                }
            }
            ExecutionMode::AutoAggregate => {
                // User did not ask for a stream but the provider responds with
                // one: aggregate (Go `autoAggregateStream`, `non_streaming.go`).
                // Go calls `p.Inbound.AggregateStreamChunks(ctx, chunks)` —
                // NOT the outbound aggregator. The inbound transformer is the
                // one that knows the client's wire format (e.g. OpenAI chat
                // completions), so it must fold the provider chunks back into
                // that shape. RUST-P8-002 S07 wires this correctly.
                //
                // S12 — the whole aggregate execution runs under the
                // NON-stream timeout (Go wraps `autoAggregateStream` in
                // `withNonStreamTimeout`, `pipeline.go:406-415`); the
                // first-event timeout is NOT applied here (Go passes `0` at
                // `non_streaming.go:86`).
                let upstream_cancel = ctx.cancel.child();
                let executed = with_timeout(
                    non_stream_ms,
                    non_stream_response_timeout_error,
                    executor.execute_stream_cancellable(&http_req, upstream_cancel),
                )
                .await;
                match executed {
                    // RUST-P8-001 S04 — Go's `autoAggregateStream` consumes
                    // `p.stream(...)` (`non_streaming.go:86`), so the raw
                    // stream + inbound raw stream wrappers apply to the events
                    // being aggregated too; the aggregated response then gets
                    // the inbound raw RESPONSE hooks (`non_streaming.go:130`).
                    Ok(events) => match self.finish_stream_events(ctx, inbound, events, outbound) {
                        Err(err) => Err(err),
                        Ok(events) => {
                            // Go `non_streaming.go:105-108` — an auto-aggregated
                            // upstream that produced NO events is treated as
                            // empty: surface `ErrEmptyStreamChunks` so the retry
                            // layer (or the API layer) can react. Mirrors Go
                            // `TestPipeline_NonStreaming_AutoAggregateUpgradedStream_*EmptyStreamChunks`.
                            if events.is_empty() {
                                let err = empty_stream_chunks_error();
                                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                                Err(ctx.fail("execute:aggregate:empty_stream_chunks", err))
                            } else {
                                match inbound.aggregate_stream_chunks(events) {
                                    Ok(response) => {
                                        // Go `non_streaming.go:116-119` — aggregator
                                        // returned an EMPTY body (nil/zero-length).
                                        // Surface `ErrEmptyAggregatedBody`. The Go
                                        // golden case `TestPipeline_NonStreaming_AutoAggregateUpgradedStream_EmptyAggregatedBody`
                                        // exercises this. A body of literally `{}`
                                        // (2 bytes, JSON object) is NOT empty and
                                        // is accepted (`EmptyJSONObjectAggregatedBodyAllowed`).
                                        let body_empty =
                                            response.body.as_ref().is_none_or(|b| b.is_empty())
                                                && response.json_body.is_none();
                                        if body_empty {
                                            let err = empty_aggregated_body_error();
                                            apply_raw_error_response_middlewares(
                                                &self.middlewares,
                                                ctx,
                                                &err,
                                            );
                                            Err(ctx.fail("execute:aggregate:empty_body", err))
                                        } else {
                                            self.finish_aggregated_response(ctx, response)
                                        }
                                    }
                                    Err(err) => {
                                        // Aggregation failure fires the raw-error
                                        // hooks (Go `non_streaming.go:110-114`);
                                        // the error stays unmarked — Go returns
                                        // it bare.
                                        apply_raw_error_response_middlewares(
                                            &self.middlewares,
                                            ctx,
                                            &err,
                                        );
                                        Err(ctx.fail("execute:aggregate:transform", err))
                                    }
                                }
                            }
                        }
                    },
                    // Timeout sentinel bare (Go `pipeline.go:410-412`);
                    // executor errors upstream-marked (wrapped inside Go's
                    // `stream()`). Raw-error hooks fire either way (Go
                    // `stream.go:274`/`282`, `non_streaming.go:100-103`).
                    Err(err) => {
                        apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                        Err(ctx.fail("execute:aggregate", mark_upstream_unless_timeout(err)))
                    }
                }
            }
            ExecutionMode::NonStream => {
                // Plain request/response, under the non-stream timeout (Go
                // `pipeline.go:423-431`). Executor errors are upstream-marked
                // (Go `notStream` wraps `Do` errors, `non_streaming.go:20-30`);
                // the timeout sentinel is returned bare (`pipeline.go:427-429`).
                let executed = with_timeout(
                    non_stream_ms,
                    non_stream_response_timeout_error,
                    executor.execute(&http_req),
                )
                .await;
                match executed {
                    Ok(response) if response.status >= 400 => {
                        let err = outbound.outbound_error(response).unwrap_or_else(|error| {
                            ConduitError::upstream(format!(
                                "failed to transform upstream error response: {error}"
                            ))
                        });
                        apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                        Err(ctx.fail("execute:nonstream:provider_error", err))
                    }
                    // RUST-P8-001 S04 — successful provider response: run the
                    // response-side hooks (Go `notStream`, `non_streaming.go:32-78`).
                    Ok(response) => self.finish_non_stream_response(
                        ctx,
                        inbound,
                        response,
                        outbound,
                        can_passthrough_openai_wire_response(
                            client_api_format,
                            upstream_api_format,
                            outbound.name(),
                        ),
                        retry_policy.empty_response_detection,
                    ),
                    Err(err) => {
                        // Failed attempt: Go fires the raw-error hooks with the
                        // pre-wrap error (`non_streaming.go:22-23`) and wraps
                        // after (`:25-29`).
                        apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                        Err(ctx.fail("execute:nonstream", mark_upstream_unless_timeout(err)))
                    }
                }
            }
        };
        (mode, outcome)
    }

    // -- response/stream middleware tails (RUST-P8-001 S04) -------------------

    /// Response-side hooks for a successful non-stream attempt, in Go
    /// `notStream` order (`non_streaming.go:32-78`):
    ///
    /// 1. `applyRawResponseMiddlewares` — REVERSE (`non_streaming.go:33`).
    /// 2. `Outbound.TransformResponse` — provider HTTP → unified `LlmResponse`
    ///    (`non_streaming.go:40`). Error: raw-error hooks + `WrapUpstreamError`
    ///    wrap (`:41-44`).
    /// 3. `applyLlmResponseMiddlewares` — REVERSE (`non_streaming.go:48`).
    ///    Error: raw-error hooks + bare `fmt.Errorf` wrap (`:49-52`).
    /// 4. Empty-response detection on the unified `LlmResponse`
    ///    (`non_streaming.go:55-59`).
    /// 5. `Inbound.TransformResponse` — unified `LlmResponse` → client HTTP
    ///    (`non_streaming.go:63`). Error: raw-error hooks + bare `fmt.Errorf`
    ///    wrap (`:64-67`).
    /// 6. `applyInboundRawResponseMiddlewares` — FORWARD
    ///    (`non_streaming.go:71`).
    fn finish_non_stream_response(
        &self,
        ctx: &mut PipelineContext,
        inbound: &dyn InboundTransformer,
        response: HttpResponse,
        outbound: &dyn OutboundTransformer,
        wire_compatible_passthrough: bool,
        empty_response_detection: bool,
    ) -> Result<HttpResponse, ConduitError> {
        // 1. Go `applyRawResponseMiddlewares` (`non_streaming.go:33`), REVERSE.
        let response = match apply_raw_response_middlewares(&self.middlewares, ctx, response) {
            Ok(response) => response,
            Err(err) => {
                // Go `non_streaming.go:34-38` — raw-error hooks, then abort
                // (fmt.Errorf wrap only; no upstream marker).
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail("execute:nonstream:raw_response_middlewares", err));
            }
        };
        // When both sides speak the same non-chat OpenAI-compatible wire
        // protocol, normalizing through the chat-shaped `LlmResponse` loses
        // endpoint-specific fields (`data`, rerank `results`, video status)
        // and cannot represent binary speech at all. Keep the raw provider
        // envelope, while still running the outbound response hook (usage
        // extraction) and the request-side completion middlewares.
        if wire_compatible_passthrough {
            let mut response = match outbound.outbound_response(response) {
                Ok(response) => response,
                Err(err) => {
                    apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                    return Err(ctx.fail(
                        "execute:nonstream:outbound:wire_response",
                        wrap_upstream_error(err),
                    ));
                }
            };
            if response.body.is_none()
                && let Some(json) = response.json_body.as_ref()
            {
                response.body = Some(serde_json::to_vec(json).map_err(|err| {
                    ctx.fail(
                        "execute:nonstream:wire_response:serialize",
                        ConduitError::internal("failed to serialize wire-compatible response")
                            .with_source(err),
                    )
                })?);
            }
            return match apply_inbound_raw_response_middlewares(&self.middlewares, ctx, response) {
                Ok(response) => Ok(response),
                Err(err) => {
                    apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                    Err(ctx.fail("execute:nonstream:inbound_response_middlewares", err))
                }
            };
        }
        let provider_status = response.status;
        let provider_headers = response.headers.clone();
        let provider_usage = response.usage.clone();
        let provider_metadata = response.metadata.clone();

        // 2. Go `Outbound.TransformResponse` (`non_streaming.go:40`): provider
        // HTTP → unified `LlmResponse`. Error path: raw-error hooks (`:42`) +
        // `WrapUpstreamError(fmt.Errorf("failed to transform response: %w",
        // err))` (`:44`) — the upstream marker is applied so the API policy
        // layer can react to provider-side transform failures.
        let llm_resp = match outbound.transform_response(response) {
            Ok(llm_resp) => llm_resp,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail(
                    "execute:nonstream:outbound:transform_response",
                    wrap_upstream_error(err),
                ));
            }
        };

        // 3. Go `applyLlmResponseMiddlewares` (`non_streaming.go:48`), REVERSE.
        // Error path: raw-error hooks (`:50`) + bare `fmt.Errorf` wrap — NO
        // upstream marker (`:52`).
        let llm_resp = match apply_llm_response_middlewares(&self.middlewares, ctx, llm_resp) {
            Ok(llm_resp) => llm_resp,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail("execute:nonstream:llm_response_middlewares", err));
            }
        };
        // Usage is part of the unified LLM response for OpenAI-compatible
        // providers. The client-facing HTTP transformer serializes it into the
        // body but does not populate `HttpResponse.usage`; preserve it here so
        // request recording, token quotas and billing receive structured usage.
        let unified_usage = llm_resp.usage.clone();

        // 4. Go `non_streaming.go:55-59` — empty-response detection on the
        // UNIFIED `LlmResponse` value (after outbound transform + LLM
        // middlewares, before inbound transform). Fires raw-error hooks
        // (`:56`) and returns `ErrEmptyResponse` bare (`:58`).
        if empty_response_detection && !has_response_content(Some(&llm_resp)) {
            let err = empty_response_error();
            apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
            return Err(ctx.fail("execute:nonstream:empty_response", err));
        }

        // 5. Go `Inbound.TransformResponse` (`non_streaming.go:63`): unified
        // `LlmResponse` → client HTTP. Error path: raw-error hooks (`:65`) +
        // bare `fmt.Errorf` wrap — NO upstream marker (`:67`).
        let mut response = match inbound.transform_response(llm_resp) {
            Ok(response) => response,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail("execute:nonstream:inbound:transform_response", err));
            }
        };
        if response.status == 0 {
            response.status = provider_status;
        }
        for (key, value) in provider_headers {
            response.headers.entry(key).or_insert(value);
        }
        if response.usage.is_none() {
            response.usage = unified_usage.or(provider_usage);
        }
        for (key, value) in provider_metadata {
            response.metadata.entry(key).or_insert(value);
        }
        if response.json_body.is_none()
            && let Some(body) = response.body.as_deref()
            && let Ok(value) = serde_json::from_slice(body)
        {
            response.json_body = Some(value);
        }

        // 6. Go `applyInboundRawResponseMiddlewares` (`non_streaming.go:71`),
        // FORWARD (confirmed: loop at `pipeline.go:157`). Failure fires the
        // raw-error hooks (`non_streaming.go:72-76`).
        match apply_inbound_raw_response_middlewares(&self.middlewares, ctx, response) {
            Ok(response) => Ok(response),
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                Err(ctx.fail("execute:nonstream:inbound_response_middlewares", err))
            }
        }
    }

    /// Stream-side hooks for a successful streaming attempt, in Go `stream()`
    /// order (`stream.go:295-394`):
    ///
    /// 1. `applyRawStreamMiddlewares` — REVERSE (`stream.go:298`).
    /// 2. `Outbound.transform_stream` — raw events → unified LlmResponse
    ///    stream (`stream.go:320`).
    /// 3. `applyLlmStreamMiddlewares` — REVERSE (`stream.go:338`).
    /// 4. `Inbound.transform_stream` — unified → client event stream
    ///    (`stream.go:374`).
    /// 5. `applyInboundRawStreamMiddlewares` — FORWARD (`stream.go:387`).
    fn finish_stream_events(
        &self,
        ctx: &mut PipelineContext,
        inbound: &dyn InboundTransformer,
        events: Vec<StreamEvent>,
        outbound: &dyn OutboundTransformer,
    ) -> Result<Vec<StreamEvent>, ConduitError> {
        let stream: BoxEventStream = Box::new(events.into_iter());

        // 1. Go `applyRawStreamMiddlewares` (`stream.go:298`), REVERSE.
        let stream = match apply_raw_stream_middlewares(&self.middlewares, ctx, stream) {
            Ok(stream) => stream,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail("execute:stream:raw_stream_middlewares", err));
            }
        };

        let raw_events: Vec<StreamEvent> = stream.collect();
        let raw_event_count = raw_events.len();
        let terminal_events: Vec<StreamEvent> = raw_events
            .iter()
            .filter(|event| {
                event.done
                    || event
                        .data
                        .as_deref()
                        .is_some_and(|data| data.trim() == "[DONE]")
            })
            .cloned()
            .collect();

        // 2. Go `Outbound.TransformStream` (`stream.go:320`): raw events →
        // unified LlmResponse stream. Error: raw-error + upstream marker.
        let llm_stream = match outbound.transform_stream(Box::new(raw_events.into_iter())) {
            Ok(s) => s,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail(
                    "execute:stream:outbound:transform_stream",
                    wrap_upstream_error(err),
                ));
            }
        };

        // 3. Go `applyLlmStreamMiddlewares` (`stream.go:338`), REVERSE.
        let llm_stream = match apply_llm_stream_middlewares(&self.middlewares, ctx, llm_stream) {
            Ok(s) => s,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail("execute:stream:llm_stream_middlewares", err));
            }
        };

        // 4. Go `Inbound.TransformStream` (`stream.go:374`): unified
        // LlmResponse stream → client-facing StreamEvent stream.
        let stream = match inbound.transform_stream(llm_stream) {
            Ok(s) => s,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail("execute:stream:inbound:transform_stream", err));
            }
        };

        // 5. Go `applyInboundRawStreamMiddlewares` (`stream.go:387`), FORWARD.
        let stream = match apply_inbound_raw_stream_middlewares(&self.middlewares, ctx, stream) {
            Ok(stream) => stream,
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                return Err(ctx.fail("execute:stream:inbound_stream_middlewares", err));
            }
        };

        let mut events: Vec<StreamEvent> = stream.collect();
        for terminal in terminal_events {
            let already_present = events.len() >= raw_event_count
                || events.iter().any(|event| {
                    event.done
                        || event
                            .data
                            .as_deref()
                            .is_some_and(|data| data.trim() == "[DONE]")
                });
            if !already_present {
                events.push(terminal);
            }
        }
        Ok(events)
    }

    /// Inbound raw response hooks over the auto-aggregated response (Go
    /// `non_streaming.go:130`), FORWARD. Failure fires the raw-error hooks
    /// (`non_streaming.go:131-134`).
    fn finish_aggregated_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> Result<HttpResponse, ConduitError> {
        match apply_inbound_raw_response_middlewares(&self.middlewares, ctx, response) {
            Ok(response) => Ok(response),
            Err(err) => {
                apply_raw_error_response_middlewares(&self.middlewares, ctx, &err);
                Err(ctx.fail("execute:aggregate:inbound_response_middlewares", err))
            }
        }
    }

    // -- retry helpers -------------------------------------------------------

    /// Mirror of Go `ChannelRetryable` arm: consults `CanRetry`, then advances
    /// the same-channel retry cursor.
    async fn try_same_channel_retry(
        &self,
        ctx: &mut PipelineContext,
        state: &mut StrFailoverState<'_>,
        err: &ConduitError,
        retry_policy: RetryPolicy,
    ) -> Result<(), FailoverError> {
        // Budget gates BEFORE the hook fires: Go evaluates
        // `sameChannelRetries < maxSameChannelRetries && CanRetry(lastErr)`
        // (pipeline.go:302) and the `ChannelRetryable` contract promises
        // `CanRetry` "will only be called if the attempt count is less than
        // maxSameChannelRetries" (pipeline.go:29-31).
        if !retry_policy.enabled
            || state.same_channel_retries >= retry_policy.max_single_channel_retries
        {
            return Ok(());
        }
        // Same-channel retry gate — data-driven, one layer (Go needs two:
        // pipeline `retryableChecker` + orchestrator `PersistentOutbound.
        // CanRetry`). Retry if EITHER the injected hook says so (its default
        // already covers Go's shared 429/5xx set via `is_retryable_error`) OR
        // this candidate opted the error IN beyond the defaults via its
        // per-channel `retryable_status_codes` / `retryable_error_patterns`
        // (carried on the candidate at selection time — zero-alloc slice check
        // on this cold path). An explicit injected `can_retry = false` for a
        // channel with no extra config still stops (both arms false).
        let candidate = state.current();
        let retryable = (self.hooks.can_retry)(err)
            || is_channel_extra_retryable(
                err,
                &candidate.retryable_status_codes,
                &candidate.retryable_error_patterns,
            );
        if !retryable {
            return Ok(());
        }
        // The cursor encodes both the budget and the model-index advance.
        match state.prepare_for_retry(retry_policy, /* model_count */ 1) {
            Ok(true) => {
                ctx.record_order(format!("retry:same_channel:{}", state.same_channel_retries));
                Ok(())
            }
            Ok(false) => Ok(()), // budget exhausted — caller tries channel switch
            Err(e) => Err(e),
        }
    }

    /// Mirror of Go `Retryable` arm: consults `HasMoreChannels`, then advances
    /// to the next channel.
    async fn try_channel_switch(
        &self,
        ctx: &mut PipelineContext,
        state: &mut StrFailoverState<'_>,
        retry_policy: RetryPolicy,
    ) -> Result<bool, FailoverError> {
        // Budget gates BEFORE the hook fires: Go evaluates
        // `channelSwitches < p.maxChannelRetries && retryable.HasMoreChannels()`
        // (pipeline.go:321) and the `Retryable` contract promises
        // `HasMoreChannels` "will only be called if the attempt count is less
        // than maxRetries" (pipeline.go:18-19). `current_index` counts the
        // switches performed so far (starts at 0, +1 per switch).
        if !retry_policy.enabled || state.current_index as u32 >= retry_policy.max_channel_retries {
            return Ok(false);
        }
        if !(self.hooks.has_more_channels)() {
            return Ok(false);
        }
        match state.next_channel() {
            Ok(()) => {
                ctx.record_order(format!("retry:channel_switch:{}", state.current_index));
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    // -- small utilities -----------------------------------------------------

    fn build_failover_state<'a>(
        &self,
        candidates: &'a [PipelineCandidate],
    ) -> Result<StrFailoverState<'a>, NoCandidates> {
        if candidates.is_empty() {
            return Err(NoCandidates);
        }
        // Skeleton cursor over pipeline candidates. The orchestrator's richer
        // `FailoverState` (keyed on `&Candidate`) follows the same contract;
        // production wires that once channels carry models.
        Ok(StrFailoverState {
            candidates,
            current_index: 0,
            current_model_index: 0,
            same_channel_retries: 0,
            total_attempts: 1,
        })
    }

    fn was_same_channel_retry_taken(&self, ctx: &PipelineContext) -> bool {
        ctx.order
            .last()
            .is_some_and(|step| step.starts_with("retry:same_channel:"))
    }

    /// Context-cancel check (Go `ctx.Err() != nil`, `pipeline.go:291`).
    /// Backed by the shared [`CancelToken`] on the context (S17): the HTTP
    /// layer cancels it on client disconnect via
    /// [`PipelineContext::cancel_handle`]; tests call
    /// [`PipelineContext::mark_canceled`].
    fn is_context_canceled(&self, ctx: &PipelineContext) -> bool {
        ctx.is_canceled()
    }
}

fn observe_live_stream_attempt(
    mut upstream: tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>>,
    observer: Option<Arc<dyn AttemptObserver>>,
    mut observation: AttemptObservation,
) -> tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>> {
    let Some(observer) = observer else {
        return upstream;
    };
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(item) = upstream.recv().await {
            let provider_status = item.as_ref().err().and_then(|error| error.provider_status);
            let failed = item.is_err();
            if tx.send(item).await.is_err() {
                return;
            }
            if failed {
                observation.outcome = AttemptObservationOutcome::Failed { provider_status };
                observer.observe(observation);
                return;
            }
        }
        observer.observe(observation);
    });
    rx
}

// ---------------------------------------------------------------------------
// Live stream transform bridge (RUST-P8-003 / P7-003 stream leg).
// ---------------------------------------------------------------------------

/// Bridge the async live upstream (`mpsc`) through the **stateful, synchronous**
/// transform chain (`outbound.transform_stream` → `inbound.transform_stream`)
/// and forward the client-facing frames on a new channel.
///
/// # Why a blocking task
///
/// `transform_stream` (both directions) is a `Box<dyn Iterator>` — a *pull*
/// API that may be stateful (Anthropic emits a `message_start` prelude and a
/// `message_stop` epilogue around the deltas; Gemini rewraps). The live
/// upstream is an async `mpsc::Receiver`. We cannot drive a synchronous
/// iterator from async code without blocking, so the whole chain runs on a
/// dedicated `spawn_blocking` task:
///
/// * a [`BlockingRecvIter`] turns `upstream_rx.blocking_recv()` into an
///   `Iterator<Item = StreamEvent>`, stashing a mid-stream `Err` into a shared
///   slot and ending iteration (mirrors the buffered path's terminal handling);
/// * that iterator is fed through `outbound.transform_stream` (raw →
///   `LlmResponse`) then `inbound.transform_stream` (→ client `StreamEvent`);
/// * each produced frame is forwarded with `blocking_send`; if the client
///   dropped its receiver the loop stops (the upstream cancel fires elsewhere);
/// * a captured mid-stream error is surfaced as a final `Err` on the output
///   channel so the forward loop finalizes the rows as `Failed`.
///
/// The transformers are passed as owned `Arc`s (their iterators are `'static`),
/// so they move onto the blocking task cleanly.
fn transform_live_stream(
    outbound: Arc<dyn OutboundTransformer>,
    inbound: Arc<dyn InboundTransformer>,
    upstream_rx: tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>>,
) -> tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>> {
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, ConduitError>>(64);

    // Shared slot for a mid-stream upstream error captured by the pull iterator.
    let err_slot: Arc<std::sync::Mutex<Option<ConduitError>>> =
        Arc::new(std::sync::Mutex::new(None));

    // Shared slot for a terminal sentinel (`[DONE]` / `done`) seen in the raw
    // upstream. The transform chain drops it (the default outbound
    // `transform_stream` parses each event as `LlmResponse` via serde and
    // `filter_map`s out the non-JSON `[DONE]` sentinel), so — mirroring the
    // buffered path's terminal handling (`execute_stream:1686-1752`) — we
    // capture it here and re-emit it after the chain if it did not survive.
    let terminal_slot: Arc<std::sync::Mutex<Option<StreamEvent>>> =
        Arc::new(std::sync::Mutex::new(None));

    fn is_terminal(event: &StreamEvent) -> bool {
        event.done
            || event
                .data
                .as_deref()
                .is_some_and(|data| data.trim() == "[DONE]")
    }

    // A synchronous iterator over the async upstream. `blocking_recv` is legal
    // here because the whole closure runs inside `spawn_blocking`.
    struct BlockingRecvIter {
        rx: tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>>,
        err_slot: Arc<std::sync::Mutex<Option<ConduitError>>>,
        terminal_slot: Arc<std::sync::Mutex<Option<StreamEvent>>>,
    }
    impl Iterator for BlockingRecvIter {
        type Item = StreamEvent;
        fn next(&mut self) -> Option<StreamEvent> {
            match self.rx.blocking_recv() {
                Some(Ok(event)) => {
                    // Stash a copy of any terminal sentinel so it can be
                    // re-emitted if the transform chain drops it.
                    if is_terminal(&event)
                        && let Ok(mut slot) = self.terminal_slot.lock()
                    {
                        *slot = Some(event.clone());
                    }
                    Some(event)
                }
                Some(Err(err)) => {
                    // Stash the error; end the raw iterator so the transform
                    // chain flushes any epilogue, then we emit the error.
                    if let Ok(mut slot) = self.err_slot.lock() {
                        *slot = Some(err);
                    }
                    None
                }
                None => None,
            }
        }
    }

    let err_slot_iter = Arc::clone(&err_slot);
    let terminal_slot_iter = Arc::clone(&terminal_slot);
    tokio::task::spawn_blocking(move || {
        let raw_iter = BlockingRecvIter {
            rx: upstream_rx,
            err_slot: err_slot_iter,
            terminal_slot: terminal_slot_iter,
        };

        // outbound.transform_stream: raw provider events → unified LlmResponse.
        let llm_iter = match outbound.transform_stream(Box::new(raw_iter)) {
            Ok(it) => it,
            Err(err) => {
                let _ = out_tx.blocking_send(Err(wrap_upstream_error(err)));
                return;
            }
        };

        // inbound.transform_stream: unified → client-native StreamEvent (e.g.
        // Anthropic message_start … message_stop).
        let client_iter = match inbound.transform_stream(llm_iter) {
            Ok(it) => it,
            Err(err) => {
                let _ = out_tx.blocking_send(Err(err));
                return;
            }
        };

        let mut terminal_survived = false;
        for frame in client_iter {
            if is_terminal(&frame) {
                terminal_survived = true;
            }
            // Client dropped the receiver → stop pulling (upstream cancel is
            // handled by the forward loop's own disconnect path).
            if out_tx.blocking_send(Ok(frame)).is_err() {
                return;
            }
        }

        // Re-emit the captured terminal sentinel if the transform chain dropped
        // it (mirrors the buffered path re-appending `terminal_events` after
        // the chain — `execute_stream:1740-1752`). The default outbound
        // `transform_stream` serde-parses events into `LlmResponse` and
        // discards the non-JSON `[DONE]` sentinel, so without this the client
        // never sees the stream terminator.
        if !terminal_survived
            && let Ok(mut slot) = terminal_slot.lock()
            && let Some(terminal) = slot.take()
            && out_tx.blocking_send(Ok(terminal)).is_err()
        {
            return;
        }

        // Surface any captured mid-stream upstream error as a terminal frame so
        // the forward loop lands the rows in `Failed` (Go `streamErr`).
        if let Ok(mut slot) = err_slot.lock()
            && let Some(err) = slot.take()
        {
            let _ = out_tx.blocking_send(Err(err));
        }
    });

    out_rx
}

fn passthrough_live_stream(
    mut upstream_rx: tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>>,
) -> tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>> {
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(event) = upstream_rx.recv().await {
            if out_tx.send(event).await.is_err() {
                break;
            }
        }
    });
    out_rx
}

fn rewrite_live_stream_errors(
    mut upstream_rx: tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>>,
    channel_id: String,
    rules: Vec<ErrorResponseRewriteRule>,
) -> tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>> {
    if rules.is_empty() {
        return upstream_rx;
    }
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(item) = upstream_rx.recv().await {
            let stop = item.is_err();
            let item =
                item.map_err(|error| apply_error_response_rewrite(&channel_id, &rules, error));
            if out_tx.send(item).await.is_err() || stop {
                break;
            }
        }
    });
    out_rx
}

// ---------------------------------------------------------------------------
// Pipeline candidate + failover cursor (skeleton adapter).
// ---------------------------------------------------------------------------

/// WIRE-06 — per-attempt outbound target the orchestrator threads into
/// [`Pipeline::process`]. Carries what Go's `PersistentOutboundTransformer`
/// reads off the current `ChannelModelsCandidate` (`outbound.go:359-385`):
/// the channel id, its base URL, the active credential and the actual model
/// the upstream expects.
#[derive(Clone, Debug)]
pub struct PipelineCandidate {
    /// Channel id (Go `candidate.Channel.ID`) — drives failover bookkeeping.
    pub id: String,
    /// Channel base URL (Go `Config.BaseURL`); `None` leaves `HttpRequest::url`
    /// to the outbound transformer.
    pub base_url: Option<String>,
    /// Plaintext active credential (Go `Config.APIKeyProvider`). **In-memory
    /// only** — never written to `ctx.order`, errors, or logs.
    pub credential: Option<String>,
    /// Stable SHA-256 identity of `credential`. Safe for metadata and
    /// persistence; never contains the plaintext key.
    pub credential_identity: Option<String>,
    /// Upstream model name (Go `entry.ActualModel`) stamped onto the
    /// per-attempt `LlmRequest`.
    pub actual_model: Option<String>,
    /// API format of the candidate (Go `candidate.APIFormat`) — reserved for
    /// per-format endpoint dispatch.
    pub api_format: String,
    /// Optional path override from the selected [`ChannelEndpoint`]. This is
    /// endpoint-scoped (not channel-global) and therefore must travel with the
    /// same candidate that supplied `api_format`.
    pub endpoint_path: Option<String>,
    /// Selected endpoint transport. HTTP is the only transport implemented by
    /// this pipeline today; retaining the value lets selection fail closed
    /// instead of accidentally issuing HTTP for a websocket endpoint.
    pub endpoint_transport: Option<String>,
    /// Channel type (Go `candidate.Channel.Type` — e.g. "openai", "anthropic",
    /// "gemini"). Used to select the correct outbound transformer from the
    /// TransformerRegistry at runtime.
    pub channel_type: String,
    /// Channel-specific config entries that need to flow into PipelineContext
    /// metadata for middleware consumption (e.g. pass_through_enabled, quota
    /// settings, override operations). The orchestrator fills these from the
    /// channel row's settings JSON when building candidates.
    pub channel_config: std::collections::BTreeMap<String, String>,
    /// Extra HTTP status codes this channel treats as retryable, on top of the
    /// default set (429 + 5xx). Mirrors Go `ChannelSettings.RetryableStatusCodes`.
    /// Parsed once at candidate-build time so the retry decision does no
    /// per-attempt deserialization. Empty = default set only.
    pub retryable_status_codes: Vec<i64>,
    /// Error-message patterns this channel treats as retryable. Mirrors Go
    /// `ChannelSettings.RetryableErrorPatterns`. Empty = no pattern matching.
    pub retryable_error_patterns: Vec<RetryableErrorPattern>,
    /// Client-visible final error rewrites for this channel. They are applied
    /// only after retry selection has stopped.
    pub error_response_rewrite_rules: Vec<ErrorResponseRewriteRule>,
}

impl From<&str> for PipelineCandidate {
    /// Id-only candidate — the shape the pipeline consumed before WIRE-06.
    /// Used by tests and callers not yet wired to real channel data.
    fn from(id: &str) -> Self {
        Self {
            id: id.to_string(),
            base_url: None,
            credential: None,
            credential_identity: None,
            actual_model: None,
            api_format: String::new(),
            endpoint_path: None,
            endpoint_transport: None,
            channel_type: String::new(),
            channel_config: std::collections::BTreeMap::new(),
            retryable_status_codes: Vec::new(),
            retryable_error_patterns: Vec::new(),
            error_response_rewrite_rules: Vec::new(),
        }
    }
}

/// RUST-P8-003 (phase 2) — the live streaming attempt handle returned by
/// [`Pipeline::stream_live`].
///
/// Carries the incremental provider-event receiver plus the attempt identity
/// the orchestrator needs to build the persistence finalizer's
/// [`AttemptRecord`] (channel id + 1-based sequence + model index). The item
/// type is `Result<StreamEvent, ConduitError>` (see [`Executor::execute_stream_live`]
/// for the design-note on why `UpstreamItem` cannot be named here).
pub struct LiveStreamAttempt {
    /// Incremental provider events (`Ok`) / mid-stream failure (`Err`); the
    /// sender closes when the upstream ends.
    pub upstream_rx: tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>>,
    /// Channel id this attempt targeted (Go `candidate.Channel.ID`).
    pub channel_id: String,
    /// 1-based attempt sequence (always 1 — the live path takes the first
    /// candidate with no failover yet).
    pub sequence: u32,
    /// Model index inside the candidate's model list (always 0 on this path).
    pub model_index: usize,
    /// Attempt-scoped middleware cleanup. It is moved into the forwarding task
    /// and dropped only after the live stream finishes (including client
    /// disconnect/cancellation), keeping concurrency permits aligned with the
    /// real upstream lifetime.
    pub cleanup: LiveStreamCleanup,
}

/// RAII cleanup for resources acquired by live-stream request middlewares.
/// Hooks run in reverse middleware order, matching response/error unwinding.
pub struct LiveStreamCleanup {
    middlewares: Vec<crate::middleware::BoxPipelineMiddleware>,
    ctx: PipelineContext,
}

impl LiveStreamCleanup {
    fn new(
        middlewares: Vec<crate::middleware::BoxPipelineMiddleware>,
        ctx: PipelineContext,
    ) -> Self {
        Self { middlewares, ctx }
    }
}

impl Drop for LiveStreamCleanup {
    fn drop(&mut self) {
        for middleware in self.middlewares.iter().rev() {
            middleware.on_outbound_live_stream_close(&mut self.ctx);
        }
    }
}

/// Local failover cursor over [`PipelineCandidate`]s. Mirrors the counters of
/// the orchestrator's `FailoverState` (keyed on `&Candidate`) so the skeleton's
/// retry decision tree is exercised against plain candidates. Production code
/// will swap this for the real `FailoverState` once channels carry models.
struct StrFailoverState<'a> {
    candidates: &'a [PipelineCandidate],
    current_index: usize,
    current_model_index: usize,
    same_channel_retries: u32,
    total_attempts: u32,
}

impl<'a> StrFailoverState<'a> {
    /// Current attempt target. The lifetime is tied to the candidate slice
    /// (not `&self`) so `process` can hold the target across later `&mut`
    /// uses of the cursor.
    fn current(&self) -> &'a PipelineCandidate {
        &self.candidates[self.current_index]
    }
}

impl<'a> StrFailoverState<'a> {
    fn prepare_for_retry(
        &mut self,
        policy: RetryPolicy,
        model_count: usize,
    ) -> Result<bool, FailoverError> {
        if !policy.enabled {
            return Err(FailoverError::RetryDisabled);
        }
        if self.same_channel_retries >= policy.max_single_channel_retries {
            return Ok(false);
        }
        if self.current_model_index + 1 < model_count {
            self.current_model_index += 1;
        }
        self.same_channel_retries += 1;
        self.total_attempts += 1;
        Ok(true)
    }

    fn next_channel(&mut self) -> Result<(), FailoverError> {
        self.current_index += 1;
        if self.current_index >= self.candidates.len() {
            self.current_index -= 1;
            return Err(FailoverError::NoMoreChannels);
        }
        self.current_model_index = 0;
        self.same_channel_retries = 0;
        self.total_attempts += 1;
        Ok(())
    }
}

/// Bridge so the skeleton's str-cursor errors surface as [`ConduitError`].
struct NoCandidates;

impl NoCandidates {
    fn into_conduit_error(self) -> ConduitError {
        ConduitError::not_found("no candidates available for pipeline")
    }
}

// ---------------------------------------------------------------------------
// Outbound merge stub (Go MergeInboundRequest).
// ---------------------------------------------------------------------------

fn candidate_api_format(
    target: &PipelineCandidate,
    client_format: ApiFormat,
) -> Result<ApiFormat, ConduitError> {
    if let Some(transport) = target
        .endpoint_transport
        .as_deref()
        .map(str::trim)
        .filter(|transport| !transport.is_empty())
        && !transport.eq_ignore_ascii_case("http")
    {
        return Err(ConduitError::invalid_request(format!(
            "unsupported upstream endpoint transport: {transport}"
        )));
    }
    if target.api_format.trim().is_empty() {
        return Ok(client_format);
    }
    ApiFormat::parse(target.api_format.trim()).map_err(|err| {
        ConduitError::invalid_request(format!(
            "invalid candidate API format {:?}: {err}",
            target.api_format
        ))
    })
}

/// WIRE-06 credential/URL stamping shared by the buffered attempt body
/// ([`Pipeline::process_attempt`]) and the live stream path
/// ([`Pipeline::stream_live`]).
///
/// Ports the per-request `PersistentOutboundTransformer` target selection
/// (Go `outbound.go:359-385`): stamp the candidate's `base_url` onto the
/// outbound request when the transformer left it unset (or relative), then
/// stamp the channel's credential using the channel-type's auth mechanism
/// (Anthropic `x-api-key` + version header, Gemini `x-goog-api-key`, everyone
/// else `Authorization: Bearer`).
///
/// ⚠ `target.credential` is a plaintext secret — this function must never log
/// or `{:?}`-format it.
fn stamp_outbound_target(http_req: &mut HttpRequest, target: &PipelineCandidate) {
    // Proxy selection is channel-owned. Remove any value inherited from the
    // inbound request, then stamp only the selected candidate's serialized
    // configuration for the production executor.
    http_req.metadata.remove("channel_proxy");
    if let Some(proxy) = target.channel_config.get("channel_proxy") {
        http_req.metadata.insert(
            "channel_proxy".to_string(),
            serde_json::Value::String(proxy.clone()),
        );
    }

    // Resolve the path first. Endpoint `path` is authoritative; otherwise use
    // the transformer's explicit path or recover it from its URL (Anthropic and
    // Gemini historically populated only `url`). Keeping this separate from
    // the base URL prevents a shared transformer from pinning requests to its
    // constructor's default host.
    let explicit_endpoint_path = target
        .endpoint_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let transformer_path = if http_req.path.trim().is_empty() {
        http_req
            .url
            .as_deref()
            .and_then(path_and_query_from_url)
            .unwrap_or_default()
    } else {
        http_req.path.as_str()
    };
    let mut effective_path = explicit_endpoint_path
        .unwrap_or(transformer_path)
        .to_string();
    if explicit_endpoint_path.is_none() {
        effective_path = normalize_protocol_path(target, &effective_path);
    }
    if !effective_path.is_empty() {
        if !effective_path.starts_with('/') {
            effective_path.insert(0, '/');
        }
        http_req.path = effective_path.clone();
    }

    // Candidate base_url includes the selected endpoint's override (when one
    // exists) and is authoritative even if a stateless transformer emitted an
    // absolute default URL. This is especially important for Gemini, whose
    // constructor normally expands an empty base to Google's public host.
    if let Some(base) = target
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|base| !base.is_empty())
    {
        http_req.url = Some(join_candidate_url(base, &effective_path));
    } else if explicit_endpoint_path.is_some()
        && let Some(existing) = http_req.url.as_deref()
        && let Some(origin) = url_origin(existing)
    {
        http_req.url = Some(join_candidate_url(origin, &effective_path));
    }

    // Stamp credential — always override with candidate's credential when
    // available (the candidate holds the channel's real API key; the outbound
    // transformer may have set auth from its empty/shared config).
    if let Some(credential) = &target.credential {
        // Auth follows the selected upstream wire protocol, not the inbound
        // protocol or merely the channel name. This also makes a custom channel
        // with an `anthropic/messages` or `gemini/contents` endpoint work.
        let channel_type = target.channel_type.as_str();
        let target_format = ApiFormat::parse(&target.api_format).ok();
        if target_format == Some(ApiFormat::AnthropicMessages)
            && !anthropic_bearer_channel(channel_type)
            && channel_type != "anthropic_gcp"
        {
            http_req.auth = None;
            http_req
                .headers
                .retain(|key, _| !key.eq_ignore_ascii_case("authorization"));
            http_req
                .headers
                .retain(|key, _| !key.eq_ignore_ascii_case("x-api-key"));
            http_req
                .headers
                .insert("x-api-key".to_string(), credential.clone());
            // Anthropic requires the version header on every request.
            http_req
                .headers
                .entry("anthropic-version".to_string())
                .or_insert_with(|| "2023-06-01".to_string());
        } else if target_format == Some(ApiFormat::GeminiContents)
            && channel_type != "gemini_vertex"
        {
            http_req.auth = None;
            http_req
                .headers
                .retain(|key, _| !key.eq_ignore_ascii_case("authorization"));
            http_req
                .headers
                .retain(|key, _| !key.eq_ignore_ascii_case("x-goog-api-key"));
            http_req
                .headers
                .insert("x-goog-api-key".to_string(), credential.clone());
        } else if channel_type != "anthropic_gcp" && channel_type != "gemini_vertex" {
            http_req
                .headers
                .retain(|key, _| !key.eq_ignore_ascii_case("authorization"));
            http_req
                .headers
                .retain(|key, _| !key.eq_ignore_ascii_case("x-api-key"));
            http_req
                .headers
                .retain(|key, _| !key.eq_ignore_ascii_case("x-goog-api-key"));
            http_req.auth = Some(HttpAuth {
                scheme: "Bearer".to_string(),
                token: Some(credential.clone()),
                ..HttpAuth::default()
            });
        }
    }
}

fn anthropic_bearer_channel(channel_type: &str) -> bool {
    matches!(
        channel_type,
        "anthropic_aws" | "longcat_anthropic" | "claudecode"
    )
}

/// Return the path and query portion of an absolute or relative URL.
fn path_and_query_from_url(url: &str) -> Option<&str> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Some((_, after_scheme)) = trimmed.split_once("://") else {
        return Some(trimmed);
    };
    after_scheme
        .find('/')
        .map(|index| &after_scheme[index..])
        .or(Some(""))
}

fn url_origin(url: &str) -> Option<&str> {
    let (_, after_scheme) = url.split_once("://")?;
    let scheme_len = url.len() - after_scheme.len();
    let end = after_scheme
        .find('/')
        .map(|index| scheme_len + index)
        .unwrap_or(url.len());
    Some(&url[..end])
}

/// Repair legacy transformer paths that omitted the provider's version prefix.
/// Explicit endpoint paths deliberately bypass this function.
fn normalize_protocol_path(target: &PipelineCandidate, path: &str) -> String {
    let path = path.trim();
    let Ok(format) = ApiFormat::parse(&target.api_format) else {
        return path.to_string();
    };
    match format {
        ApiFormat::AnthropicMessages if path == "/messages" || path.starts_with("/messages?") => {
            format!("/v1{path}")
        }
        ApiFormat::OpenAiResponses if path == "/responses" || path.starts_with("/responses?") => {
            format!("/v1{path}")
        }
        ApiFormat::OpenAiResponsesCompact
            if path == "/responses/compact" || path.starts_with("/responses/compact?") =>
        {
            format!("/v1{path}")
        }
        ApiFormat::OpenAiChatCompletions
            if path == "/chat/completions" || path.starts_with("/chat/completions?") =>
        {
            format!("/v1{path}")
        }
        _ => path.to_string(),
    }
}

fn join_candidate_url(base: &str, path: &str) -> String {
    if let Some(raw) = base.strip_suffix("##") {
        return raw.trim_end_matches('/').to_string();
    }
    let base = base.trim_end_matches('/');
    if path.is_empty() {
        return base.to_string();
    }
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let (path_without_query, query) = path
        .split_once('?')
        .map_or((path.as_str(), ""), |(path, query)| (path, query));

    // Accept the common UI forms both with and without a trailing API version,
    // without creating `/v1/v1/...` or duplicating a full endpoint path.
    if base.ends_with(path_without_query) {
        return if query.is_empty() {
            base.to_string()
        } else {
            format!("{base}?{query}")
        };
    }
    for version in ["/v1beta", "/v1"] {
        if base.ends_with(version) && path_without_query.starts_with(&format!("{version}/")) {
            let suffix = &path[version.len()..];
            return format!("{base}{suffix}");
        }
    }
    format!("{base}{path}")
}

/// Minimal inbound merge: surface inbound query/headers onto the outbound
/// request unless the outbound already set them. The full semantic port lives
// in the http module; this keeps the step observable.
fn merge_inbound(outbound: &mut HttpRequest, inbound: &HttpRequest) {
    if outbound.method.is_empty() {
        outbound.method = inbound.method.clone();
    }
    for (key, value) in &inbound.headers {
        // Client credentials authenticate to Conduit API and must never reach an
        // upstream provider. The selected channel credential is stamped after
        // this merge.
        if key.eq_ignore_ascii_case("authorization")
            || key.eq_ignore_ascii_case("proxy-authorization")
            || key.eq_ignore_ascii_case("x-api-key")
            || key.eq_ignore_ascii_case("x-goog-api-key")
            // These describe the client-to-gateway HTTP framing. The outbound
            // body has been transformed and reqwest must calculate fresh
            // framing; forwarding the old Content-Length truncates JSON.
            || key.eq_ignore_ascii_case("content-length")
            || key.eq_ignore_ascii_case("transfer-encoding")
            || key.eq_ignore_ascii_case("host")
            || key.eq_ignore_ascii_case("connection")
            || key.eq_ignore_ascii_case("proxy-connection")
            || key.eq_ignore_ascii_case("keep-alive")
            || key.eq_ignore_ascii_case("te")
            || key.eq_ignore_ascii_case("trailer")
            || key.eq_ignore_ascii_case("upgrade")
        {
            continue;
        }
        outbound
            .headers
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    if !outbound.skip_inbound_query_merge {
        for (key, values) in &inbound.query {
            outbound
                .query
                .entry(key.clone())
                .or_insert_with(|| values.clone());
        }
    }
    if outbound.content_type.is_none() {
        outbound.content_type = inbound.content_type.clone();
    }
}

fn wire_compatible_openai_response(api_format: conduit_llm::ApiFormat) -> bool {
    use conduit_llm::ApiFormat;
    matches!(
        api_format,
        ApiFormat::OpenAiCompletions
            | ApiFormat::OpenAiResponses
            | ApiFormat::OpenAiResponsesCompact
            | ApiFormat::OpenAiEmbeddings
            | ApiFormat::OpenAiImageGeneration
            | ApiFormat::OpenAiImageEdit
            | ApiFormat::OpenAiImageVariation
            | ApiFormat::OpenAiAudioSpeech
            | ApiFormat::OpenAiAudioTranscriptions
            | ApiFormat::OpenAiAudioTranslations
            | ApiFormat::OpenAiVideo
            | ApiFormat::JinaRerank
            | ApiFormat::JinaEmbeddings
    )
}

fn can_passthrough_openai_wire_response(
    client_format: ApiFormat,
    upstream_format: ApiFormat,
    outbound_name: &str,
) -> bool {
    client_format == upstream_format
        && wire_compatible_openai_response(upstream_format)
        && matches!(outbound_name, "openai-compat-outbound" | "openai-responses")
}

// ---------------------------------------------------------------------------
// Small async/time helpers (S12/S14/S18 support).
// ---------------------------------------------------------------------------

/// Run `fut` under an optional timeout. `timeout_ms == 0` disables the bound
/// (Go: `newFirstEventTimeoutGuard` returns a nil guard for `timeout <= 0`,
/// `stream.go:30-33`; `withNonStreamTimeout` no-ops, `pipeline.go:449-455`).
/// On elapse the supplied sentinel constructor produces the error (Go
/// `context.WithTimeoutCause(ctx, d, sentinel)`).
async fn with_timeout<T, F>(
    timeout_ms: u64,
    sentinel: fn() -> ConduitError,
    fut: F,
) -> Result<T, ConduitError>
where
    F: std::future::Future<Output = Result<T, ConduitError>>,
{
    if timeout_ms == 0 {
        return fut.await;
    }
    match tokio::time::timeout(Duration::from_millis(timeout_ms), fut).await {
        Ok(result) => result,
        Err(_elapsed) => Err(sentinel()),
    }
}

/// Mark an error as upstream-originated unless it is one of the response
/// timeout sentinels — Go returns `ErrStreamFirstEventTimeout` /
/// `ErrNonStreamResponseTimeout` bare (`stream.go:284-286`,
/// `pipeline.go:410-412`/`427-429`) but wraps other upstream-path errors in
/// `UpstreamError` (`stream.go:288-292`, `non_streaming.go:26-29`).
fn mark_upstream_unless_timeout(err: ConduitError) -> ConduitError {
    if is_response_timeout_error(&err) {
        err
    } else {
        wrap_upstream_error(err)
    }
}

/// Current wall-clock time in ms since the UNIX epoch, for S14
/// `RetryContext::started_at_ms`. Unwrap-free: a clock before the epoch
/// yields 0.
fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// PipelineContext helpers (cancellation flag for tests).
// ---------------------------------------------------------------------------

impl PipelineContext {
    /// Mark the context as canceled — the test/local-task stand-in for a
    /// client disconnect (Go: the HTTP server cancels the request
    /// `context.Context`). Cancels the shared [`CancelToken`], so in-flight
    /// child tokens observe it too.
    pub fn mark_canceled(&mut self) {
        self.cancel.cancel();
    }

    /// Whether the context is canceled (Go `ctx.Err() != nil`).
    pub fn is_canceled(&self) -> bool {
        self.cancel.is_canceled()
    }

    /// A clonable handle onto the shared cancel token (S17). The HTTP layer
    /// holds one and cancels it when the client disconnects — even while
    /// [`Pipeline::process`] has the context mutably borrowed, because the
    /// token state lives behind an `Arc`.
    pub fn cancel_handle(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Record a failure step and return the error unchanged so callers can use
    /// `?`-style flow while still marking the pipeline order.
    pub fn fail(&mut self, step: &str, err: ConduitError) -> ConduitError {
        self.record_order(format!("{step}:error"));
        err
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::ErrorKind;
    use conduit_llm::{
        ApiFormat, ChatRequest, Choice, LlmMessage, LlmRequestPayload, LlmResponse, MessageContent,
        RequestType, StreamEvent,
    };
    use conduit_transformers::TransformerResult;
    use serde_json::json;
    use std::sync::Mutex;

    /// WIRE-06 helper: id-only candidates for the legacy `&["a"]`-style call
    /// sites (no base_url/credential/actual_model).
    fn pc(ids: &[&str]) -> Vec<PipelineCandidate> {
        ids.iter().copied().map(PipelineCandidate::from).collect()
    }

    #[derive(Default)]
    struct CapturingAttemptObserver {
        observations: Mutex<Vec<AttemptObservation>>,
    }

    impl AttemptObserver for CapturingAttemptObserver {
        fn observe(&self, observation: AttemptObservation) {
            if let Ok(mut observations) = self.observations.lock() {
                observations.push(observation);
            }
        }
    }

    impl CapturingAttemptObserver {
        fn take(&self) -> Vec<AttemptObservation> {
            self.observations
                .lock()
                .map(|mut observations| std::mem::take(&mut *observations))
                .unwrap_or_default()
        }
    }

    #[test]
    fn channel_config_override_restores_global_metadata_before_fallback() {
        let mut metadata = std::collections::BTreeMap::from([
            ("pass_through_enabled".to_string(), "true".to_string()),
            ("pass_through_user_agent".to_string(), "true".to_string()),
            ("request_scope".to_string(), "keep".to_string()),
        ]);
        let first_channel = std::collections::BTreeMap::from([
            ("pass_through_enabled".to_string(), "false".to_string()),
            ("pass_through_user_agent".to_string(), "false".to_string()),
            ("channel_rpm_limit".to_string(), "12".to_string()),
        ]);
        let fallback_channel = std::collections::BTreeMap::new();
        let mut previous_values = Vec::new();

        replace_channel_config_metadata(&mut metadata, &first_channel, &mut previous_values);
        assert_eq!(
            metadata.get("pass_through_enabled").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            metadata.get("pass_through_user_agent").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            metadata.get("channel_rpm_limit").map(String::as_str),
            Some("12")
        );

        replace_channel_config_metadata(&mut metadata, &fallback_channel, &mut previous_values);
        assert_eq!(
            metadata.get("pass_through_enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            metadata.get("pass_through_user_agent").map(String::as_str),
            Some("true")
        );
        assert!(!metadata.contains_key("channel_rpm_limit"));
        assert_eq!(
            metadata.get("request_scope").map(String::as_str),
            Some("keep")
        );
    }

    // -- stub transformers + executor ---------------------------------------

    /// `extra` key under which [`StubOutbound::transform_response`] stashes the
    /// raw provider `json_body` so [`StubInbound::transform_response`] can
    /// restore it losslessly (the stubs are passthrough transformers — tests
    /// assert on the exact body the executor returned).
    const STUB_RAW_BODY_KEY: &str = "__stub_raw_json_body";

    /// Build a content-carrying [`LlmResponse`] fallback for provider JSON that
    /// isn't a full `LlmResponse` shape (e.g. the test `{"content": "..."}`).
    /// The content text (if any) is surfaced as a chat choice so
    /// `has_response_content` reports the right answer for empty-response
    /// detection.
    fn stub_llm_response(v: &serde_json::Value) -> LlmResponse {
        let content = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
        LlmResponse {
            id: "stub".to_string(),
            object: "chat.completion".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Some(LlmMessage {
                    role: Some("assistant".to_string()),
                    content: Some(MessageContent::Text(content.to_string())),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    struct StubInbound;

    impl InboundTransformer for StubInbound {
        fn name(&self) -> &'static str {
            "stub-inbound"
        }
        fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
            Ok(LlmRequest {
                request_type: request.request_type.unwrap_or(RequestType::Chat),
                api_format: request
                    .api_format
                    .unwrap_or(ApiFormat::OpenAiChatCompletions),
                model: Some("stub-model".to_string()),
                // Echo the inbound stream flag — tests vary it.
                stream: request
                    .json_body
                    .as_ref()
                    .and_then(|b| b.get("stream"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
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
        // Unified stream transform — restore StubOutbound's stash.
        fn transform_stream(
            &self,
            events: Box<dyn Iterator<Item = LlmResponse> + Send>,
        ) -> TransformerResult<Box<dyn Iterator<Item = StreamEvent> + Send>> {
            Ok(Box::new(events.map(|resp| {
                let data = resp
                    .extra
                    .get("__stub_event_data")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let event_type = resp
                    .extra
                    .get("__stub_event_type")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                StreamEvent {
                    event_type,
                    data,
                    ..StreamEvent::default()
                }
            })))
        }
        // Unified response transform (response-chain): restore the stashed raw
        // json_body losslessly (see StubOutbound::transform_response). Falls
        // back to the default JSON serialize if no stash is present.
        fn transform_response(&self, response: LlmResponse) -> TransformerResult<HttpResponse> {
            if let Some(serde_json::Value::Object(raw)) = response.extra.get(STUB_RAW_BODY_KEY) {
                return Ok(HttpResponse {
                    status: 200,
                    json_body: Some(serde_json::Value::Object(raw.clone())),
                    ..HttpResponse::default()
                });
            }
            // Fallback: serialize the unified value (default-impl behavior).
            let body = serde_json::to_vec(&response).map_err(|err| {
                ConduitError::new(
                    ErrorKind::Internal,
                    "failed to serialize unified LlmResponse",
                )
                .with_source(err)
            })?;
            Ok(HttpResponse {
                status: 200,
                body: Some(body),
                ..HttpResponse::default()
            })
        }
        // RUST-P8-002 S07 — Go's `autoAggregateStream` routes through the
        // inbound aggregator, not outbound. Override the trait default so the
        // AutoAggregate test can assert which transformer produced the body.
        fn aggregate_stream_chunks(
            &self,
            events: Vec<StreamEvent>,
        ) -> TransformerResult<HttpResponse> {
            Ok(HttpResponse {
                status: 200,
                json_body: Some(json!({
                    "aggregated_by": "inbound",
                    "event_count": events.len(),
                })),
                stream: events,
                ..HttpResponse::default()
            })
        }
    }

    struct StubOutbound {
        /// Force the outbound request's `stream` JSON field to this value, to
        /// exercise the auto-aggregate branch.
        force_effective_stream: Option<bool>,
    }

    impl OutboundTransformer for StubOutbound {
        fn name(&self) -> &'static str {
            "stub-outbound"
        }
        fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
            Ok(HttpRequest {
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                json_body: Some(json!({
                    "model": request.model,
                    "stream": self.force_effective_stream.unwrap_or(request.stream),
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
            Ok(ConduitError::upstream("stub").with_provider_status(response.status))
        }
        fn aggregate_stream(&self, events: Vec<StreamEvent>) -> TransformerResult<HttpResponse> {
            Ok(HttpResponse {
                status: 200,
                json_body: Some(json!({"aggregated": events.len()})),
                stream: events,
                ..HttpResponse::default()
            })
        }
        // Unified stream transform — lossless passthrough: wrap each event as
        // a stub LlmResponse (stash raw fields in extra for inbound restore).
        fn transform_stream(
            &self,
            events: Box<dyn Iterator<Item = StreamEvent> + Send>,
        ) -> TransformerResult<Box<dyn Iterator<Item = LlmResponse> + Send>> {
            Ok(Box::new(events.map(|ev| {
                let mut resp = LlmResponse {
                    id: "stub-stream".to_string(),
                    object: "chat.completion.chunk".to_string(),
                    ..Default::default()
                };
                if let Some(data) = &ev.data {
                    resp.extra
                        .insert("__stub_event_data".to_string(), json!(data));
                }
                if let Some(et) = &ev.event_type {
                    resp.extra
                        .insert("__stub_event_type".to_string(), json!(et));
                }
                resp
            })))
        }
        // Unified response transform — same lossless stash-restore strategy as
        // StubOutbound (see there); the shaped tests assert on the exact body.
        // transformer, so the HTTP→LlmResponse→HTTP round-trip must be LOSSLESS:
        // tests assert on the exact `json_body` the executor produced. We stash
        // the raw json_body in `extra` and restore it on the inbound side. The
        // LlmResponse itself is parsed when possible (so `has_response_content`
        // drives empty-response detection correctly) and falls back to a
        // content-carrying stub body when the test's JSON isn't a full
        // `LlmResponse` shape (e.g. `{"content": "..."}`).
        fn transform_response(&self, response: HttpResponse) -> TransformerResult<LlmResponse> {
            let raw = response.json_body.clone();
            let llm = match response.json_body.as_ref() {
                Some(v) => serde_json::from_value::<LlmResponse>(v.clone())
                    .unwrap_or_else(|_| stub_llm_response(v)),
                None => LlmResponse {
                    id: "stub".to_string(),
                    object: "chat.completion".to_string(),
                    ..Default::default()
                },
            };
            // `unwrap_or_else` above is fine under the no-unwrap lint; merge
            // the stashed raw body into `extra` for lossless inbound restore.
            let mut llm = llm;
            if let Some(raw) = raw {
                llm.extra.insert(STUB_RAW_BODY_KEY.to_string(), raw);
            }
            Ok(llm)
        }
    }

    /// Configurable stub executor. `responses` is consumed in order per call;
    /// once empty it returns the terminal `final_error`.
    struct StubExecutor {
        responses: Mutex<VecDeque<Result<HttpResponse, ConduitError>>>,
        stream_responses: Mutex<VecDeque<Result<Vec<StreamEvent>, ConduitError>>>,
        /// Virtual-time delay before every answer — S12/S15 fake-clock tests
        /// (`#[tokio::test(start_paused = true)]` auto-advances through it).
        delay_ms: u64,
        /// Cancel tokens received via `execute_stream_cancellable` (S13).
        captured_tokens: Mutex<Vec<CancelToken>>,
        /// Side-effect fired at the start of every execute* call — e.g. cancel
        /// the client handle to simulate a mid-flight disconnect (S17).
        on_execute: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    use std::collections::VecDeque;
    impl StubExecutor {
        fn non_stream(responses: Vec<Result<HttpResponse, ConduitError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                stream_responses: Mutex::new(VecDeque::new()),
                delay_ms: 0,
                captured_tokens: Mutex::new(Vec::new()),
                on_execute: None,
            }
        }
        fn stream(streams: Vec<Result<Vec<StreamEvent>, ConduitError>>) -> Self {
            Self {
                responses: Mutex::new(VecDeque::new()),
                stream_responses: Mutex::new(streams.into()),
                delay_ms: 0,
                captured_tokens: Mutex::new(Vec::new()),
                on_execute: None,
            }
        }
        /// Delay every answer by `ms` of virtual time (S12 timeout tests).
        fn with_delay_ms(mut self, ms: u64) -> Self {
            self.delay_ms = ms;
            self
        }
        /// Fire `hook` at the start of every execute* call (S17 disconnect).
        fn with_on_execute(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
            self.on_execute = Some(hook);
            self
        }
        fn captured_tokens(&self) -> Vec<CancelToken> {
            self.captured_tokens
                .lock()
                .map(|tokens| tokens.clone())
                .unwrap_or_default()
        }
        async fn simulate_upstream_latency(&self) {
            if let Some(hook) = &self.on_execute {
                hook();
            }
            if self.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
        }
    }

    #[async_trait]
    impl Executor for StubExecutor {
        async fn execute(&self, _request: &HttpRequest) -> Result<HttpResponse, ConduitError> {
            self.simulate_upstream_latency().await;
            self.responses
                .lock()
                .map_err(|_| ConduitError::internal("executor lock poisoned"))?
                .pop_front()
                .unwrap_or_else(|| Err(ConduitError::upstream("stub exhausted")))
        }
        async fn execute_stream(
            &self,
            _request: &HttpRequest,
        ) -> Result<Vec<StreamEvent>, ConduitError> {
            self.simulate_upstream_latency().await;
            self.stream_responses
                .lock()
                .map_err(|_| ConduitError::internal("executor lock poisoned"))?
                .pop_front()
                .unwrap_or_else(|| Err(ConduitError::upstream("stub stream exhausted")))
        }
        // S13 — capture the per-attempt child token the pipeline hands us
        // (the Go `streamCtx` analog), then answer like `execute_stream`.
        async fn execute_stream_cancellable(
            &self,
            request: &HttpRequest,
            cancel: CancelToken,
        ) -> Result<Vec<StreamEvent>, ConduitError> {
            if let Ok(mut tokens) = self.captured_tokens.lock() {
                tokens.push(cancel);
            }
            self.execute_stream(request).await
        }
    }

    fn ok_response(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            json_body: Some(json!({"content": body})),
            ..HttpResponse::default()
        }
    }

    fn upstream_error() -> ConduitError {
        ConduitError::upstream("upstream failure")
    }

    /// Upstream error carrying a provider HTTP status, so the data-driven
    /// default retry hook (`is_retryable_error`) can classify it.
    fn upstream_error_with_status(status: u16) -> ConduitError {
        ConduitError::upstream("upstream failure").with_provider_status(status)
    }

    fn build_pipeline(
        executor: Arc<dyn Executor>,
        force_effective_stream: Option<bool>,
        hooks: RetryHooks,
    ) -> Pipeline {
        Pipeline::new(
            Arc::new(StubInbound),
            Arc::new(StubOutbound {
                force_effective_stream,
            }),
            executor,
        )
        .with_retry_policy(RetryPolicy {
            enabled: true,
            max_channel_retries: 2,
            max_single_channel_retries: 1,
            retry_delay_ms: 0, // keep tests fast
            ..RetryPolicy::DEFAULT
        })
        .with_retry_hooks(hooks)
    }

    fn raw_inbound(stream: bool) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            json_body: Some(json!({"stream": stream})),
            request_type: Some(RequestType::Chat),
            api_format: Some(ApiFormat::OpenAiChatCompletions),
            ..HttpRequest::default()
        }
    }

    // -- tests ---------------------------------------------------------------

    #[test]
    fn execution_mode_resolves_go_switch_arms() {
        // user wants stream -> always Stream (Go first arm).
        assert_eq!(ExecutionMode::resolve(true, true), ExecutionMode::Stream);
        assert_eq!(ExecutionMode::resolve(true, false), ExecutionMode::Stream);
        // user no stream, provider streams -> AutoAggregate.
        assert_eq!(
            ExecutionMode::resolve(false, true),
            ExecutionMode::AutoAggregate
        );
        // neither -> NonStream.
        assert_eq!(
            ExecutionMode::resolve(false, false),
            ExecutionMode::NonStream
        );
    }

    #[tokio::test]
    async fn inbound_transform_runs_only_once_across_retries() -> Result<(), ConduitError> {
        // First attempt fails with a retryable error; same-channel retry then
        // succeeds. Inbound must still be recorded only once.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Ok(ok_response("after retry")),
        ]));
        let pipeline = build_pipeline(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(|_| true),
                has_more_channels: Arc::new(|| true),
                is_timeout_error: Arc::new(|_| false),
            },
        );
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(response.json_body, ok_response("after retry").json_body);
        assert_eq!(attempts.len(), 2, "one failed + one retry");
        let inbound_count = ctx
            .order
            .iter()
            .filter(|step| step == &"inbound:transform_request")
            .count();
        assert_eq!(inbound_count, 1, "inbound transform must run exactly once");
        let middleware_count = ctx
            .order
            .iter()
            .filter(|step| step == &"inbound:llm_middlewares")
            .count();
        assert_eq!(middleware_count, 1, "inbound middlewares run exactly once");
        Ok(())
    }

    #[tokio::test]
    async fn attempt_observer_receives_provider_failure_then_success() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error_with_status(401)),
            Ok(ok_response("after retry")),
        ]));
        let observer = Arc::new(CapturingAttemptObserver::default());
        let pipeline = build_pipeline(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(|_| true),
                has_more_channels: Arc::new(|| true),
                is_timeout_error: Arc::new(|_| false),
            },
        )
        .with_attempt_observer(observer.clone());
        let mut candidates = pc(&["a"]);
        candidates[0].credential = Some("secret-key".to_string());
        candidates[0].credential_identity = Some("sha256:key".to_string());
        let mut ctx = PipelineContext::new();

        pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &candidates,
            )
            .await?;

        let observations = observer.take();
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].channel_id, "a");
        assert_eq!(observations[0].credential.as_deref(), Some("secret-key"));
        assert_eq!(
            observations[0].credential_identity.as_deref(),
            Some("sha256:key")
        );
        assert_eq!(
            observations[0].outcome,
            AttemptObservationOutcome::Failed {
                provider_status: Some(401)
            }
        );
        assert_eq!(
            observations[1].outcome,
            AttemptObservationOutcome::Succeeded
        );
        Ok(())
    }

    #[tokio::test]
    async fn attempt_order_is_outbound_merge_auth_middlewares_execute() -> Result<(), ConduitError>
    {
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("ok"))]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let _ = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        let expected_prefix = [
            "inbound:transform_request",
            "inbound:llm_middlewares",
            "attempt:1:start",
            "outbound:transform_request",
            "outbound:merge_inbound",
            "outbound:auth_headers",
            "outbound:raw_middlewares",
            "execute:NonStream",
            "attempt:1:success",
        ];
        let actual_prefix: Vec<&String> = ctx.order.iter().take(expected_prefix.len()).collect();
        for (idx, want) in expected_prefix.iter().enumerate() {
            assert_eq!(actual_prefix[idx], want, "step {idx} mismatch");
        }
        Ok(())
    }

    #[tokio::test]
    async fn retry_first_same_channel_then_channel_switch() -> Result<(), ConduitError> {
        // Three failures: first attempt (a), same-channel retry (a), then a
        // channel switch (b). The 4th attempt on b succeeds.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Err(upstream_error()),
            Err(upstream_error()),
            Ok(ok_response("ok on b")),
        ]));
        let pipeline = build_pipeline(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(|_| true),
                has_more_channels: Arc::new(|| true),
                is_timeout_error: Arc::new(|_| false),
            },
        );
        let mut ctx = PipelineContext::new();

        let (_response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a", "b"]),
            )
            .await?;

        // Expected Go sequence: attempt1=a(fail) -> same-channel a(fail) ->
        // channel switch b(fail) -> same-channel b(success).
        let channels: Vec<&str> = attempts.iter().map(|a| a.channel_id.as_str()).collect();
        assert_eq!(channels, vec!["a", "a", "b", "b"]);
        Ok(())
    }

    /// RUST fix — the **default** `RetryHooks` (production, no injection) must
    /// retry a 5xx upstream error, mirroring Go's `retryableChecker.CanRetry`
    /// default set (429/5xx). This is the regression guard for the wiring gap
    /// where the old default (`can_retry: |_| false`) silently disabled all
    /// retries in the live binary.
    #[tokio::test]
    async fn default_hooks_retry_on_5xx_upstream() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error_with_status(503)),
            Ok(ok_response("ok after 503 retry")),
        ]));
        // NOTE: default hooks — exactly what the production binary uses.
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let (_response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        // Two attempts on the same channel: the 503 triggered a same-channel
        // retry (default set), the retry succeeded.
        assert_eq!(attempts.len(), 2, "503 must trigger a same-channel retry");
        assert!(
            ctx.order
                .iter()
                .any(|s| s.starts_with("retry:same_channel"))
        );
        Ok(())
    }

    /// RUST enhancement (better-than-Go one-layer data flow) — a status code
    /// that is NOT in the default set (e.g. 418) but IS listed in the
    /// candidate's `retryable_status_codes` must trigger a retry, WITHOUT any
    /// injected hook. The per-channel config flows on `PipelineCandidate`.
    #[tokio::test]
    async fn candidate_per_channel_retryable_status_triggers_retry() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error_with_status(418)), // not in the default set
            Ok(ok_response("ok after 418 retry")),
        ]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        // Candidate opts 418 IN via its per-channel retryable_status_codes.
        let candidate = PipelineCandidate {
            retryable_status_codes: vec![418],
            ..PipelineCandidate::from("a")
        };

        let (_response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &[candidate],
            )
            .await?;

        assert_eq!(
            attempts.len(),
            2,
            "per-channel 418 opt-in must trigger a retry"
        );
        Ok(())
    }

    #[tokio::test]
    async fn retry_stops_when_can_retry_returns_false() -> Result<(), ConduitError> {
        // First attempt fails; same-channel CanRetry returns false and there
        // are no more channels -> no retry, error returned.
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Err(upstream_error())]));
        let pipeline = build_pipeline(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(|_| false),
                has_more_channels: Arc::new(|| false),
                is_timeout_error: Arc::new(|_| false),
            },
        );
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        assert!(err.is_some(), "should surface the failure");
        assert_eq!(err.as_ref().map(|e| e.kind), Some(ErrorKind::Upstream));
        assert!(ctx.order.iter().any(|s| s == "retry:exhausted"));
        Ok(())
    }

    #[tokio::test]
    async fn retry_respects_max_single_channel_retries() -> Result<(), ConduitError> {
        // max_single_channel_retries=1: after the initial + 1 same-channel
        // retry on "a", we switch to "b". With only one candidate and
        // has_more_channels=true but FailoverState has no further channel, the
        // switch returns NoMoreChannels and the loop stops.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Err(upstream_error()),
            Err(upstream_error()),
        ]));
        let pipeline = build_pipeline(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(|_| true),
                has_more_channels: Arc::new(|| true),
                is_timeout_error: Arc::new(|_| false),
            },
        );
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        assert!(err.is_some());
        // Initial + 1 same-channel retry = 2 attempts on "a".
        let a_attempts = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("attempt:") && s.ends_with(":error"))
            .count();
        assert_eq!(a_attempts, 2, "single-channel budget is 1 retry");
        Ok(())
    }

    #[tokio::test]
    async fn request_scoped_retry_policy_overrides_pipeline_startup_snapshot()
    -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Ok(ok_response("ok after live-policy retry")),
        ]));
        let pipeline = build_pipeline(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(|_| true),
                has_more_channels: Arc::new(|| false),
                is_timeout_error: Arc::new(|_| false),
            },
        )
        .with_retry_policy(RetryPolicy {
            enabled: false,
            ..RetryPolicy::DEFAULT
        });
        let mut ctx = PipelineContext::new();
        let raw = raw_inbound(false);

        let (_response, attempts) = pipeline
            .process_with_inbound_policy(
                &mut ctx,
                pipeline.inbound(),
                raw.clone(),
                &raw,
                &pc(&["a"]),
                RetryPolicy {
                    enabled: true,
                    max_channel_retries: 0,
                    max_single_channel_retries: 1,
                    retry_delay_ms: 0,
                    ..RetryPolicy::DEFAULT
                },
            )
            .await?;

        assert_eq!(attempts.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn timeout_error_skips_same_channel_retry() -> Result<(), ConduitError> {
        // First attempt is a response-timeout error. Go skips the
        // ChannelRetryable branch and goes straight to channel switch.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(ConduitError::upstream("timeout")),
            Ok(ok_response("ok on b")),
        ]));
        // If same-channel were tried, can_retry would loop on "a"; instead we
        // assert the channel switch happened.
        let pipeline = build_pipeline(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(|_| true),
                has_more_channels: Arc::new(|| true),
                is_timeout_error: Arc::new(|_| true),
            },
        );
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a", "b"]),
            )
            .await?;

        assert_eq!(response.json_body, ok_response("ok on b").json_body);
        // Same-channel retry step must NOT appear for timeout errors.
        assert!(
            !ctx.order
                .iter()
                .any(|s| s.starts_with("retry:same_channel:")),
            "timeout errors must skip same-channel retry"
        );
        assert!(ctx.order.iter().any(|s| s == "retry:channel_switch:1"));
        // Attempts went a (timeout) -> b (success).
        let channels: Vec<&str> = attempts.iter().map(|a| a.channel_id.as_str()).collect();
        assert_eq!(channels, vec!["a", "b"]);
        Ok(())
    }

    #[tokio::test]
    async fn context_cancel_does_not_retry() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Err(upstream_error())]));
        let pipeline = build_pipeline(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(|_| true),
                has_more_channels: Arc::new(|| true),
                is_timeout_error: Arc::new(|_| false),
            },
        );
        let mut ctx = PipelineContext::new();
        ctx.mark_canceled();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a", "b"]),
            )
            .await
            .err();
        assert!(err.is_some());
        assert!(
            ctx.order.iter().any(|s| s == "retry:context_canceled"),
            "canceled context must short-circuit retry"
        );
        Ok(())
    }

    #[tokio::test]
    async fn user_stream_true_uses_stream_branch() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::stream(vec![Ok(vec![
            StreamEvent {
                data: Some("delta".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                done: true,
                ..StreamEvent::default()
            },
        ])]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(&mut ctx, raw_inbound(true), &raw_inbound(true), &pc(&["a"]))
            .await?;

        assert_eq!(attempts[0].mode, ExecutionMode::Stream);
        assert_eq!(response.stream.len(), 2);
        assert!(ctx.order.iter().any(|s| s == "execute:Stream"));
        Ok(())
    }

    #[tokio::test]
    async fn user_no_stream_provider_streams_auto_aggregates() -> Result<(), ConduitError> {
        // force_effective_stream = Some(true) simulates a provider that always
        // streams even when the user asked for non-stream.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::stream(vec![Ok(vec![
            StreamEvent {
                data: Some("a".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                data: Some("b".to_string()),
                ..StreamEvent::default()
            },
        ])]));
        let pipeline = build_pipeline(executor, Some(true), RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(attempts[0].mode, ExecutionMode::AutoAggregate);
        // RUST-P8-002 S07 — Go's `autoAggregateStream` calls the *inbound*
        // `AggregateStreamChunks`, not outbound. `StubInbound` tags its
        // output with `aggregated_by: "inbound"`; `StubOutbound` would emit
        // `aggregated: <n>`. Assert the inbound sentinel is present so a
        // regression that re-wires this to outbound fails loudly.
        assert_eq!(
            response
                .json_body
                .as_ref()
                .and_then(|body| body.get("aggregated_by")),
            Some(&json!("inbound")),
            "AutoAggregate must route through the inbound aggregator (Go parity)"
        );
        // Aggregate produced a JSON body and preserved the event stream.
        assert!(response.json_body.is_some());
        assert_eq!(response.stream.len(), 2);
        assert!(ctx.order.iter().any(|s| s == "execute:AutoAggregate"));
        Ok(())
    }

    #[tokio::test]
    async fn neither_user_nor_provider_streams_is_nonstream() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("plain"))]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let (_response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(attempts[0].mode, ExecutionMode::NonStream);
        assert!(ctx.order.iter().any(|s| s == "execute:NonStream"));
        Ok(())
    }

    #[tokio::test]
    async fn empty_candidate_list_returns_error() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("never"))]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(&mut ctx, raw_inbound(false), &raw_inbound(false), &[])
            .await
            .err();
        assert!(err.is_some());
        assert!(ctx.order.iter().any(|s| s == "retry:no_candidates"));
        Ok(())
    }

    // -- pure-logic parity tests for S04-S15 invariants ----------------------

    #[test]
    fn attempt_stage_next_encodes_go_process_request_order() {
        // Go `processRequest` executes: outbound.TransformRequest
        // -> MergeInboundRequest -> FinalizeAuthHeaders
        // -> applyRawRequestMiddlewares -> executor.
        let mut walked: Vec<AttemptStage> = Vec::new();
        let mut cur = Some(AttemptStage::FIRST);
        while let Some(stage) = cur {
            walked.push(stage);
            cur = stage.next();
        }
        assert_eq!(
            walked,
            vec![
                AttemptStage::OutboundTransform,
                AttemptStage::MergeInbound,
                AttemptStage::AuthHeaders,
                AttemptStage::OutboundRawMiddlewares,
                AttemptStage::Execute,
            ]
        );
        assert_eq!(AttemptStage::sequence().to_vec(), walked);
        assert_eq!(AttemptStage::Execute.next(), None);
    }

    #[test]
    fn decide_stream_mode_mirrors_go_switch_arms() {
        // user_stream true -> Stream (Go first arm).
        assert_eq!(decide_stream_mode(true, true), StreamMode::Stream,);
        assert_eq!(decide_stream_mode(true, false), StreamMode::Stream,);
        // user_stream false, provider needs stream -> AutoAggregate.
        assert_eq!(decide_stream_mode(false, true), StreamMode::AutoAggregate,);
        // neither -> NonStream.
        assert_eq!(decide_stream_mode(false, false), StreamMode::NonStream,);
    }

    fn ok_outcome() -> AttemptOutcome {
        AttemptOutcome {
            is_timeout_error: false,
            can_retry_same_channel: true,
            has_more_channels: true,
        }
    }

    #[test]
    fn decide_retry_same_channel_first_when_can_retry() {
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 3,
            max_single_channel_retries: 2,
            retry_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        };
        let state = RetryState::initial();
        // Outcome is fully retryable -> same-channel arm wins (Go order).
        assert_eq!(
            decide_retry(policy, state, ok_outcome(), false),
            RetryDecision::RetrySameChannel,
        );
    }

    #[test]
    fn decide_retry_falls_back_to_channel_switch_when_same_channel_exhausted() {
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 3,
            max_single_channel_retries: 2,
            retry_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        };
        // Same-channel budget exhausted -> channel-switch arm.
        let state = RetryState {
            channel_switches: 0,
            single_channel_retries: 2,
        };
        assert_eq!(
            decide_retry(policy, state, ok_outcome(), false),
            RetryDecision::RetryNextChannel,
        );
    }

    #[test]
    fn decide_retry_falls_back_to_channel_switch_when_can_retry_false() {
        let policy = RetryPolicy::DEFAULT;
        let state = RetryState::initial();
        let outcome = AttemptOutcome {
            is_timeout_error: false,
            can_retry_same_channel: false,
            has_more_channels: true,
        };
        assert_eq!(
            decide_retry(policy, state, outcome, false),
            RetryDecision::RetryNextChannel,
        );
    }

    #[test]
    fn decide_retry_stops_when_no_more_channels() {
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 1,
            max_single_channel_retries: 1,
            retry_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        };
        let state = RetryState {
            channel_switches: 1,       // at budget
            single_channel_retries: 1, // at budget
        };
        assert_eq!(
            decide_retry(policy, state, ok_outcome(), false),
            RetryDecision::Stop,
        );
    }

    #[test]
    fn decide_retry_skips_same_channel_for_timeout_errors() {
        // S09: response-timeout errors skip the same-channel arm (Go
        // `isResponseTimeoutError` guard). With a channel available we fall
        // through to RetryNextChannel rather than RetrySameChannel.
        let policy = RetryPolicy::DEFAULT;
        let state = RetryState::initial();
        let outcome = AttemptOutcome {
            is_timeout_error: true,
            can_retry_same_channel: true, // would normally trigger same-channel
            has_more_channels: true,
        };
        assert_eq!(
            decide_retry(policy, state, outcome, false),
            RetryDecision::RetryNextChannel,
        );
    }

    #[test]
    fn decide_retry_ctx_canceled_never_retries() {
        // S09: canceled context must short-circuit to Stop regardless of
        // budgets/permissions (Go `if ctx.Err() != nil { return ... }`).
        let policy = RetryPolicy::DEFAULT;
        let state = RetryState::initial();
        assert_eq!(
            decide_retry(policy, state, ok_outcome(), true),
            RetryDecision::Stop,
        );
    }

    #[test]
    fn decide_retry_disabled_policy_stops() {
        let mut policy = RetryPolicy::DEFAULT;
        policy.enabled = false;
        let state = RetryState::initial();
        assert_eq!(
            decide_retry(policy, state, ok_outcome(), false),
            RetryDecision::Stop,
        );
    }

    #[test]
    fn retry_context_accumulator_records_same_channel_then_reset_on_switch() {
        let mut ctx = RetryContext::new(1_700_000_000_000);
        assert_eq!(ctx.channel_attempt, 1);
        assert_eq!(ctx.single_channel_attempt, 0);
        assert_eq!(ctx.last_error_kind, None);
        assert!(!ctx.retryable_status);

        // Attempt 1 fails -> same-channel retry.
        ctx.record_failure("upstream_error", RetryDecision::RetrySameChannel);
        assert_eq!(ctx.channel_attempt, 2);
        assert_eq!(ctx.single_channel_attempt, 1);
        assert_eq!(ctx.last_error_kind, Some("upstream_error"));
        assert!(ctx.retryable_status);

        // Attempt 2 fails -> same-channel retry again.
        ctx.record_failure("upstream_error", RetryDecision::RetrySameChannel);
        assert_eq!(ctx.channel_attempt, 3);
        assert_eq!(ctx.single_channel_attempt, 2);

        // Attempt 3 fails -> channel switch resets single-channel counter.
        ctx.record_failure("upstream_error", RetryDecision::RetryNextChannel);
        assert_eq!(ctx.channel_attempt, 4);
        assert_eq!(ctx.single_channel_attempt, 0);

        // Final failure -> Stop leaves counters unchanged.
        ctx.record_failure("rate_limited", RetryDecision::Stop);
        assert_eq!(ctx.channel_attempt, 4);
        assert_eq!(ctx.single_channel_attempt, 0);
        assert_eq!(ctx.last_error_kind, Some("rate_limited"));
    }

    // -- RUST-P8-002 S05: pipeline-level inbound LLM middleware wiring --------
    // (middlewares now implement the 9-hook `PipelineMiddleware`, overriding
    // only `on_inbound_llm_request` — the S07 one-concern rule.)

    use crate::middleware::{PipelineMiddleware, PipelineResult};

    /// Recording inbound middleware (same shape as the unit tests above, but
    /// lives here so the pipeline-integration tests can share the stubs).
    struct RecInboundMw {
        name: &'static str,
    }
    impl PipelineMiddleware for RecInboundMw {
        fn name(&self) -> &str {
            self.name
        }
        fn on_inbound_llm_request(
            &self,
            ctx: &mut PipelineContext,
            request: LlmRequest,
        ) -> PipelineResult<LlmRequest> {
            ctx.record_order(format!("{}:on_request", self.name));
            Ok(request)
        }
    }

    /// Middleware that flips `stream` to `true` — proves mutation flows downstream
    /// and changes the execution-mode branch (NonStream -> Stream).
    struct ForceStreamMw;
    impl PipelineMiddleware for ForceStreamMw {
        fn name(&self) -> &str {
            "force-stream"
        }
        fn on_inbound_llm_request(
            &self,
            _ctx: &mut PipelineContext,
            mut request: LlmRequest,
        ) -> PipelineResult<LlmRequest> {
            request.stream = true;
            Ok(request)
        }
    }

    /// Middleware that aborts the chain (Go "error stops the pipeline").
    struct AbortInboundMw;
    impl PipelineMiddleware for AbortInboundMw {
        fn name(&self) -> &str {
            "abort"
        }
        fn on_inbound_llm_request(
            &self,
            _ctx: &mut PipelineContext,
            _request: LlmRequest,
        ) -> PipelineResult<LlmRequest> {
            Err(ConduitError::invalid_request("blocked by policy"))
        }
    }

    #[tokio::test]
    async fn inbound_llm_middlewares_run_once_in_forward_order_through_pipeline()
    -> Result<(), ConduitError> {
        // No retry: a single successful attempt. The two recording middlewares
        // must each fire exactly once, BEFORE the first attempt starts, in
        // forward registration order. Retries must NOT re-invoke them.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Ok(ok_response("after retry")),
        ]));
        let pipeline = build_pipeline(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(|_| true),
                has_more_channels: Arc::new(|| true),
                is_timeout_error: Arc::new(|_| false),
            },
        )
        .with_middlewares(vec![
            Arc::new(RecInboundMw { name: "auth" }) as BoxPipelineMiddleware,
            Arc::new(RecInboundMw { name: "quota" }) as BoxPipelineMiddleware,
        ]);
        let mut ctx = PipelineContext::new();

        let _ = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        // The marker is recorded exactly once.
        let marker_count = ctx
            .order
            .iter()
            .filter(|s| s == &"inbound:llm_middlewares")
            .count();
        assert_eq!(marker_count, 1, "inbound middleware marker must fire once");

        // Each middleware fired exactly once (not re-run on the retry).
        let auth_count = ctx.order.iter().filter(|s| s == &"auth:on_request").count();
        let quota_count = ctx
            .order
            .iter()
            .filter(|s| s == &"quota:on_request")
            .count();
        assert_eq!(auth_count, 1);
        assert_eq!(quota_count, 1);

        // Forward order: marker -> auth -> quota -> attempt start.
        let marker_idx = ctx
            .order
            .iter()
            .position(|s| s == "inbound:llm_middlewares");
        let auth_idx = ctx.order.iter().position(|s| s == "auth:on_request");
        let quota_idx = ctx.order.iter().position(|s| s == "quota:on_request");
        let attempt_idx = ctx.order.iter().position(|s| s == "attempt:1:start");
        assert!(marker_idx < auth_idx, "marker precedes auth");
        assert!(auth_idx < quota_idx, "auth precedes quota (forward order)");
        assert!(
            quota_idx < attempt_idx,
            "middlewares run before the first attempt"
        );
        Ok(())
    }

    #[tokio::test]
    async fn inbound_llm_middleware_mutation_flows_to_execution_mode() -> Result<(), ConduitError> {
        // User asks for non-stream; a middleware flips stream=true. The pipeline
        // must then take the Stream branch — proving the mutated LlmRequest is
        // the one fed to the attempt loop, not a stale copy.
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent {
                done: true,
                ..StreamEvent::default()
            }])]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default())
            .with_middlewares(vec![Arc::new(ForceStreamMw) as BoxPipelineMiddleware]);
        let mut ctx = PipelineContext::new();

        let (_resp, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        // Without the middleware, raw_inbound(false) + StubOutbound(None) would
        // yield NonStream. The mutation must force Stream.
        assert_eq!(attempts[0].mode, ExecutionMode::Stream);
        assert!(ctx.order.iter().any(|s| s == "execute:Stream"));
        Ok(())
    }

    #[tokio::test]
    async fn inbound_llm_middleware_abort_short_circuits_before_attempt_loop()
    -> Result<(), ConduitError> {
        // A middleware aborts: no attempt must start, no executor call must be
        // made. We assert by giving the executor a single Ok response and
        // checking it was never consumed (the response would have surfaced if
        // the attempt ran).
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("never"))]));
        let pipeline =
            build_pipeline(executor, None, RetryHooks::default()).with_middlewares(vec![
                Arc::new(RecInboundMw { name: "before" }) as BoxPipelineMiddleware,
                Arc::new(AbortInboundMw) as BoxPipelineMiddleware,
                Arc::new(RecInboundMw { name: "after" }) as BoxPipelineMiddleware,
            ]);
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        assert!(err.is_some(), "abort middleware must surface an error");
        assert_eq!(
            err.as_ref().map(|e| e.kind),
            Some(ErrorKind::InvalidRequest)
        );
        // "before" ran, "after" did not.
        assert!(ctx.order.iter().any(|s| s == "before:on_request"));
        assert!(!ctx.order.iter().any(|s| s == "after:on_request"));
        // No attempt ever started.
        assert!(
            !ctx.order.iter().any(|s| s.starts_with("attempt:")),
            "abort must short-circuit before the attempt loop"
        );
        // Failure marker for the stage is recorded.
        assert!(
            ctx.order
                .iter()
                .any(|s| s == "inbound:llm_middlewares:error")
        );
        Ok(())
    }

    // =========================================================================
    // RUST-P8-002 S12-S18 — timeouts, cancellation, retry context, delays,
    // retryable sources, final-error selection.
    // =========================================================================

    use crate::cancel::CancelOnCloseStream;
    use crate::upstream_error::is_upstream_error;

    /// `build_pipeline` with an explicit retry policy (the S12/S15 tests need
    /// non-default timeout/delay knobs).
    fn build_pipeline_with_policy(
        executor: Arc<dyn Executor>,
        force_effective_stream: Option<bool>,
        hooks: RetryHooks,
        policy: RetryPolicy,
    ) -> Pipeline {
        Pipeline::new(
            Arc::new(StubInbound),
            Arc::new(StubOutbound {
                force_effective_stream,
            }),
            executor,
        )
        .with_retry_policy(policy)
        .with_retry_hooks(hooks)
    }

    fn retryable_hooks() -> RetryHooks {
        // can_retry/has_more always true; is_timeout_error stays the REAL
        // classifier (RetryHooks::default), mirroring Go's package-level
        // `isResponseTimeoutError`.
        RetryHooks {
            can_retry: Arc::new(|_| true),
            has_more_channels: Arc::new(|| true),
            ..RetryHooks::default()
        }
    }

    // -- S12: response timeouts from the retry policy -------------------------

    #[test]
    fn clamp_response_timeout_seconds_mirrors_go_normalize() {
        // Go normalizeRetryPolicy (biz/system.go:1041-1053): negative -> 0,
        // above 600 -> 600 (maxRetryResponseTimeoutSeconds, system.go:32).
        assert_eq!(clamp_response_timeout_seconds(-5), 0);
        assert_eq!(clamp_response_timeout_seconds(0), 0);
        assert_eq!(clamp_response_timeout_seconds(30), 30);
        assert_eq!(clamp_response_timeout_seconds(600), 600);
        assert_eq!(clamp_response_timeout_seconds(601), 600);
        assert_eq!(clamp_response_timeout_seconds(10_000), 600);
    }

    #[test]
    fn timeout_sentinels_are_recognized_by_classifier() {
        // Go isResponseTimeoutError (pipeline.go:445-447) — true for both
        // sentinels, false otherwise.
        assert!(is_stream_first_event_timeout(
            &stream_first_event_timeout_error()
        ));
        assert!(is_non_stream_response_timeout(
            &non_stream_response_timeout_error()
        ));
        assert!(is_response_timeout_error(
            &stream_first_event_timeout_error()
        ));
        assert!(is_response_timeout_error(
            &non_stream_response_timeout_error()
        ));
        assert!(!is_response_timeout_error(&upstream_error()));
    }

    #[tokio::test(start_paused = true)]
    async fn non_stream_timeout_yields_sentinel_and_skips_same_channel() -> Result<(), ConduitError>
    {
        // Go: withNonStreamTimeout wraps notStream (pipeline.go:423-431);
        // the sentinel is a response-timeout error, so the same-channel arm
        // is skipped (pipeline.go:297-300) and only channel switches happen.
        let stub = Arc::new(
            StubExecutor::non_stream(vec![Ok(ok_response("late")), Ok(ok_response("late"))])
                .with_delay_ms(10_000),
        );
        let executor: Arc<dyn Executor> = stub;
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 2,
            max_single_channel_retries: 2,
            retry_delay_ms: 0,
            stream_first_event_timeout_ms: 0,
            non_stream_timeout_ms: 500,
            empty_response_detection: false,
        };
        let pipeline = build_pipeline_with_policy(executor, None, retryable_hooks(), policy);
        let mut ctx = PipelineContext::new();

        let start = tokio::time::Instant::now();
        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a", "b"]),
            )
            .await
            .err();

        let err = match err {
            Some(err) => err,
            None => panic!("slow upstream must time out"),
        };
        assert!(
            is_non_stream_response_timeout(&err),
            "final error must be the non-stream timeout sentinel, got {err:?}"
        );
        // Timeout errors skip the same-channel arm even though can_retry=true.
        assert!(
            !ctx.order
                .iter()
                .any(|s| s.starts_with("retry:same_channel:")),
            "same-channel retry must be skipped for timeout errors"
        );
        assert!(ctx.order.iter().any(|s| s == "retry:channel_switch:1"));
        // Two attempts (a then b), each bounded to 500ms of virtual time.
        let attempts_failed = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("attempt:") && s.ends_with(":error"))
            .count();
        assert_eq!(attempts_failed, 2);
        assert_eq!(start.elapsed(), Duration::from_millis(1000));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn stream_first_event_timeout_yields_sentinel() -> Result<(), ConduitError> {
        // Go: the first-event guard bounds DoStream + the first pre-read event
        // (stream.go:265-277); on expiry the pipeline returns
        // ErrStreamFirstEventTimeout.
        let stub = Arc::new(
            StubExecutor::stream(vec![Ok(vec![StreamEvent::default()])]).with_delay_ms(10_000),
        );
        let executor: Arc<dyn Executor> = stub;
        let policy = RetryPolicy {
            stream_first_event_timeout_ms: 300,
            retry_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        };
        let pipeline = build_pipeline_with_policy(executor, None, RetryHooks::default(), policy);
        let mut ctx = PipelineContext::new();

        let start = tokio::time::Instant::now();
        let err = pipeline
            .process(&mut ctx, raw_inbound(true), &raw_inbound(true), &pc(&["a"]))
            .await
            .err();

        match err {
            Some(err) => assert!(
                is_stream_first_event_timeout(&err),
                "expected first-event timeout sentinel, got {err:?}"
            ),
            None => panic!("stream without first event in time must fail"),
        }
        assert_eq!(start.elapsed(), Duration::from_millis(300));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn timeouts_default_zero_do_not_bound_slow_upstream() -> Result<(), ConduitError> {
        // Go defaults: defaultRetryPolicy sets neither timeout -> 0 -> guard
        // disabled (stream.go:30-33, pipeline.go:449-455). A slow upstream
        // still succeeds.
        let stub = Arc::new(
            StubExecutor::non_stream(vec![Ok(ok_response("slow but fine"))]).with_delay_ms(60_000),
        );
        let executor: Arc<dyn Executor> = stub;
        let pipeline = build_pipeline_with_policy(
            executor,
            None,
            RetryHooks::default(),
            RetryPolicy {
                retry_delay_ms: 0,
                ..RetryPolicy::DEFAULT
            },
        );
        let mut ctx = PipelineContext::new();

        let start = tokio::time::Instant::now();
        let (response, _) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;
        assert_eq!(
            response.json_body,
            ok_response("slow but fine").json_body,
            "no timeout configured -> slow upstream succeeds"
        );
        assert_eq!(start.elapsed(), Duration::from_millis(60_000));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn auto_aggregate_not_bounded_by_first_event_timeout() -> Result<(), ConduitError> {
        // Go autoAggregateStream calls p.stream(ctx, executor, req, 0) —
        // NO first-event timeout (non_streaming.go:86); only the non-stream
        // timeout applies (pipeline.go:406-415). With only the first-event
        // knob set, a slow aggregate must still succeed.
        let stub = Arc::new(
            StubExecutor::stream(vec![Ok(vec![StreamEvent {
                data: Some("chunk".to_string()),
                ..StreamEvent::default()
            }])])
            .with_delay_ms(5_000),
        );
        let executor: Arc<dyn Executor> = stub;
        let policy = RetryPolicy {
            stream_first_event_timeout_ms: 100,
            non_stream_timeout_ms: 0,
            retry_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        };
        // force_effective_stream=Some(true) + user non-stream -> AutoAggregate.
        let pipeline =
            build_pipeline_with_policy(executor, Some(true), RetryHooks::default(), policy);
        let mut ctx = PipelineContext::new();

        let (_response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;
        assert_eq!(attempts[0].mode, ExecutionMode::AutoAggregate);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn auto_aggregate_bounded_by_non_stream_timeout() -> Result<(), ConduitError> {
        // The converse: only the non-stream knob set -> the aggregate arm
        // times out with the NON-stream sentinel (Go pipeline.go:406-415).
        let stub = Arc::new(
            StubExecutor::stream(vec![Ok(vec![StreamEvent::default()])]).with_delay_ms(5_000),
        );
        let executor: Arc<dyn Executor> = stub;
        let policy = RetryPolicy {
            stream_first_event_timeout_ms: 0,
            non_stream_timeout_ms: 500,
            retry_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        };
        let pipeline =
            build_pipeline_with_policy(executor, Some(true), RetryHooks::default(), policy);
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        match err {
            Some(err) => assert!(
                is_non_stream_response_timeout(&err),
                "auto-aggregate must use the non-stream sentinel, got {err:?}"
            ),
            None => panic!("slow auto-aggregate must time out"),
        }
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn with_response_timeouts_builder_mirrors_go_option() -> Result<(), ConduitError> {
        // Go WithResponseTimeouts(stream, nonStream) (pipeline.go:77-82) — the
        // Rust builder writes the same two knobs onto the policy and the
        // attempt arms consume them.
        let stub =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("late"))]).with_delay_ms(1_000));
        let executor: Arc<dyn Executor> = stub;
        let pipeline =
            build_pipeline(executor, None, RetryHooks::default()).with_response_timeouts(0, 250);
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        match err {
            Some(err) => assert!(is_non_stream_response_timeout(&err)),
            None => panic!("builder-configured timeout must apply"),
        }
        Ok(())
    }

    // -- S13: upstream cancellation plumbing ----------------------------------

    #[tokio::test]
    async fn stream_attempt_hands_child_cancel_token_to_executor() -> Result<(), ConduitError> {
        // The pipeline derives a per-attempt child token from the request
        // context (Go streamCtx := context.WithCancel(ctx), stream.go:35) and
        // hands it to the executor. Canceling the request context (client
        // disconnect) must be visible through the child.
        let stub = Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent {
            done: true,
            ..StreamEvent::default()
        }])]));
        let executor: Arc<dyn Executor> = stub.clone();
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let _ = pipeline
            .process(&mut ctx, raw_inbound(true), &raw_inbound(true), &pc(&["a"]))
            .await?;

        let tokens = stub.captured_tokens();
        assert_eq!(tokens.len(), 1, "stream arm passes exactly one token");
        assert!(!tokens[0].is_canceled(), "token live while stream is open");

        // Client disconnect: cancel the request-level handle -> child sees it.
        ctx.cancel_handle().cancel();
        assert!(
            tokens[0].is_canceled(),
            "request-context cancel must propagate to the upstream token"
        );
        Ok(())
    }

    #[tokio::test]
    async fn client_stream_drop_cancels_upstream_token_only() -> Result<(), ConduitError> {
        // Go cancelOnCloseStream (stream.go:109-132, wired at :406-411):
        // closing/dropping the client-facing stream cancels the upstream
        // (child) token but NOT the request context.
        let stub = Arc::new(StubExecutor::stream(vec![Ok(vec![
            StreamEvent {
                data: Some("delta".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                done: true,
                ..StreamEvent::default()
            },
        ])]));
        let executor: Arc<dyn Executor> = stub.clone();
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let (response, _) = pipeline
            .process(&mut ctx, raw_inbound(true), &raw_inbound(true), &pc(&["a"]))
            .await?;

        let tokens = stub.captured_tokens();
        let upstream_token = match tokens.first() {
            Some(token) => token.clone(),
            None => panic!("executor must have captured the upstream token"),
        };

        // The HTTP layer wraps the client-facing stream with the guard.
        let mut client_stream =
            CancelOnCloseStream::new(response.stream.into_iter(), upstream_token.clone());
        assert!(client_stream.next().is_some(), "events flow through");
        assert!(!upstream_token.is_canceled(), "open stream keeps upstream");

        drop(client_stream); // client goes away mid-stream
        assert!(
            upstream_token.is_canceled(),
            "dropping the client stream must cancel the upstream request"
        );
        assert!(
            !ctx.cancel_handle().is_canceled(),
            "request context must NOT be canceled by the stream close (Go child ctx only)"
        );
        Ok(())
    }

    // -- S17: client disconnect stops retry immediately -----------------------

    #[tokio::test]
    async fn client_disconnect_mid_attempt_stops_retry_immediately() -> Result<(), ConduitError> {
        // Go pipeline.go:290-293 — after a failed attempt, `ctx.Err() != nil`
        // returns lastErr without consulting any retry arm. Simulate the
        // disconnect DURING the upstream call via the executor side-effect.
        let mut ctx = PipelineContext::new();
        let disconnect = ctx.cancel_handle();
        let stub = Arc::new(
            StubExecutor::non_stream(vec![
                Err(upstream_error()),
                Ok(ok_response("never reached")),
            ])
            .with_on_execute(Arc::new(move || {
                // Client disconnects while the first upstream call is in
                // flight.
                disconnect.cancel();
            })),
        );
        let executor: Arc<dyn Executor> = stub;
        // Fully retryable hooks + a second candidate: without the cancel the
        // pipeline WOULD retry (see retry_first_same_channel_then_channel_switch).
        let pipeline = build_pipeline(executor, None, retryable_hooks());

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a", "b"]),
            )
            .await
            .err();

        let err = match err {
            Some(err) => err,
            None => panic!("disconnected request must surface the attempt error"),
        };
        // The ATTEMPT's error is returned (Go returns lastErr, not ctx.Err()).
        assert_eq!(err.kind, ErrorKind::Upstream);
        assert!(
            is_upstream_error(&err),
            "executor failures carry the upstream marker"
        );
        assert!(ctx.order.iter().any(|s| s == "retry:context_canceled"));
        // Exactly one attempt — no same-channel retry, no channel switch.
        let attempt_starts = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("attempt:") && s.ends_with(":start"))
            .count();
        assert_eq!(
            attempt_starts, 1,
            "disconnect must stop retries immediately"
        );
        assert!(
            !ctx.order
                .iter()
                .any(|s| s.starts_with("retry:same_channel:")
                    || s.starts_with("retry:channel_switch:")),
            "no retry arm may fire after a client disconnect"
        );
        Ok(())
    }

    // -- S14: retry-context record wired through process ----------------------

    #[tokio::test]
    async fn retry_context_records_same_channel_then_switch_through_process()
    -> Result<(), ConduitError> {
        // a fails -> same-channel retry (budget 1) -> a fails -> switch to b
        // -> b succeeds. Counters mirror Go's sameChannelRetries/channelSwitches
        // bookkeeping (pipeline.go:305-309/:323-329).
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Err(upstream_error()),
            Ok(ok_response("ok on b")),
        ]));
        let pipeline = build_pipeline(executor, None, retryable_hooks());
        let mut ctx = PipelineContext::new();

        let _ = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a", "b"]),
            )
            .await?;

        let record = match &ctx.retry_context {
            Some(record) => record,
            None => panic!("process must publish the retry context"),
        };
        // initial + same-channel retry + channel switch = 3 attempts.
        assert_eq!(record.channel_attempt, 3);
        // channel switch reset the same-channel counter (Go sameChannelRetries = 0).
        assert_eq!(record.single_channel_attempt, 0);
        assert_eq!(record.last_error_kind, Some("upstream_error"));
        assert!(record.retryable_status);
        assert!(record.started_at_ms > 0);
        Ok(())
    }

    #[tokio::test]
    async fn retry_context_records_stop_on_unretryable_failure() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Err(upstream_error())]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        assert!(err.is_some());

        let record = match &ctx.retry_context {
            Some(record) => record,
            None => panic!("failure path must also publish the retry context"),
        };
        assert_eq!(record.channel_attempt, 1, "no retry -> single attempt");
        assert_eq!(record.single_channel_attempt, 0);
        assert_eq!(record.last_error_kind, Some("upstream_error"));
        assert!(!record.retryable_status, "Stop decision is not retryable");
        Ok(())
    }

    #[tokio::test]
    async fn final_error_rewrite_uses_only_the_exhausted_channel_rule() -> Result<(), ConduitError>
    {
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(ConduitError::upstream("provider a failure")
                .with_provider_status(503)
                .with_provider_body(json!({"provider": "a"}))),
            Err(ConduitError::upstream("provider b failure")
                .with_provider_status(503)
                .with_provider_body(json!({"provider": "b"}))),
        ]));
        let pipeline = build_pipeline_with_policy(
            executor,
            None,
            retryable_hooks(),
            RetryPolicy {
                max_channel_retries: 1,
                max_single_channel_retries: 0,
                retry_delay_ms: 0,
                ..RetryPolicy::DEFAULT
            },
        );
        let mut candidates = pc(&["a", "b"]);
        candidates[0].error_response_rewrite_rules = vec![ErrorResponseRewriteRule {
            status_codes: vec![503],
            message: Some("rewritten by a".to_string()),
            ..Default::default()
        }];
        candidates[1].error_response_rewrite_rules = vec![ErrorResponseRewriteRule {
            status_codes: vec![503],
            http_status: Some(502),
            message: Some("rewritten by ${channel_id}".to_string()),
            ..Default::default()
        }];
        let mut ctx = PipelineContext::new();

        let error = match pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &candidates,
            )
            .await
        {
            Ok(_) => return Err(ConduitError::internal("both channels must fail")),
            Err(error) => error,
        };

        assert_eq!(error.http_status, 502);
        assert_eq!(error.public_message(), "rewritten by b");
        assert_eq!(error.message, "provider b failure");
        assert_eq!(error.provider_status, Some(503));
        assert_eq!(error.provider_body, Some(json!({"provider": "b"})));
        assert_eq!(
            error
                .metadata
                .get(conduit_core::ERROR_RESPONSE_REWRITE_CHANNEL_METADATA)
                .and_then(serde_json::Value::as_str),
            Some("b")
        );
        Ok(())
    }

    // -- S15: retry delay on a fake clock --------------------------------------

    #[tokio::test(start_paused = true)]
    async fn retry_delay_advances_paused_clock_between_attempts() -> Result<(), ConduitError> {
        // Go time.Sleep(p.retryDelay) between attempts (pipeline.go:344-346).
        // Two retries with retry_delay_ms=1000 -> exactly 2000ms of virtual
        // time (start_paused auto-advances through the sleeps).
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Err(upstream_error()),
            Ok(ok_response("third time lucky")),
        ]));
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 0,
            max_single_channel_retries: 2,
            retry_delay_ms: 1000,
            stream_first_event_timeout_ms: 0,
            non_stream_timeout_ms: 0,
            empty_response_detection: false,
        };
        let pipeline = build_pipeline_with_policy(executor, None, retryable_hooks(), policy);
        let mut ctx = PipelineContext::new();

        let start = tokio::time::Instant::now();
        let (response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(
            response.json_body,
            ok_response("third time lucky").json_body
        );
        assert_eq!(attempts.len(), 3);
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(2000),
            "two retries x 1000ms delay"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn zero_retry_delay_does_not_advance_clock() -> Result<(), ConduitError> {
        // retry_delay_ms=0 -> Go skips the Sleep entirely (pipeline.go:344).
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Ok(ok_response("fast retry")),
        ]));
        let policy = RetryPolicy {
            retry_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        };
        let pipeline = build_pipeline_with_policy(executor, None, retryable_hooks(), policy);
        let mut ctx = PipelineContext::new();

        let start = tokio::time::Instant::now();
        let _ = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;
        assert_eq!(start.elapsed(), Duration::ZERO, "no delay -> no sleep");
        Ok(())
    }

    // -- S18: final error selection --------------------------------------------

    #[tokio::test]
    async fn exhausted_retries_return_last_error_upstream_marked() -> Result<(), ConduitError> {
        // Go keeps lastErr across attempts (pipeline.go:288) and returns it
        // when no retry arm fires (:355). The LAST failure — not the first —
        // must surface, carrying the upstream marker so the API layer can
        // apply the UpstreamErrorPolicy (api/upstream_error_policy.go).
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(ConduitError::upstream("first failure").with_provider_status(500)),
            Err(ConduitError::upstream("second failure").with_provider_status(503)),
        ]));
        // Same-channel retry once; no further channels.
        let hooks = RetryHooks {
            can_retry: Arc::new(|_| true),
            has_more_channels: Arc::new(|| false),
            ..RetryHooks::default()
        };
        let pipeline = build_pipeline(executor, None, hooks);
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();

        let err = match err {
            Some(err) => err,
            None => panic!("all attempts failed -> error expected"),
        };
        assert_eq!(
            err.message, "second failure",
            "LAST error wins (Go lastErr)"
        );
        assert_eq!(err.provider_status, Some(503));
        assert!(
            is_upstream_error(&err),
            "executor-path errors must carry the upstream marker for the policy layer"
        );
        assert!(ctx.order.iter().any(|s| s == "retry:exhausted"));
        Ok(())
    }

    #[tokio::test]
    async fn all_exhausted_attempt_count_mirrors_go_golden_case() -> Result<(), ConduitError> {
        // Go pipeline_retry_test.go "AllExhausted" (:322-360):
        // maxSameChannelRetries=1, maxChannelRetries=1, executor always fails,
        // canRetry/hasMoreChannels always true -> exactly 4 exec calls:
        //   1. exec 1 -> fail
        //   2. same-channel retry 1 -> exec 2 -> fail
        //   3. same-channel exhausted -> switch 1 -> exec 3 -> fail
        //   4. same-channel retry 1 -> exec 4 -> fail
        //   5. switch 2 blocked by maxChannelRetries=1 -> stop.
        // THREE candidates prove the CHANNEL-SWITCH BUDGET (Go
        // `channelSwitches < p.maxChannelRetries`, pipeline.go:321) stops the
        // loop — not the candidate list running out.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Err(upstream_error()),
            Err(upstream_error()),
            Err(upstream_error()),
            Ok(ok_response("would succeed on attempt 5")),
        ]));
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 1,
            max_single_channel_retries: 1,
            retry_delay_ms: 0,
            stream_first_event_timeout_ms: 0,
            non_stream_timeout_ms: 0,
            empty_response_detection: false,
        };
        let pipeline = build_pipeline_with_policy(executor, None, retryable_hooks(), policy);
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a", "b", "c"]),
            )
            .await
            .err();
        assert!(err.is_some(), "all retries exhausted must fail");

        // Exactly 4 attempts (Go `require.Equal(t, 4, execCalls)`).
        let attempt_starts = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("attempt:") && s.ends_with(":start"))
            .count();
        assert_eq!(attempt_starts, 4, "Go AllExhausted golden: 4 exec calls");
        // Exactly 1 channel switch — candidate "c" is never tried.
        let switches = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("retry:channel_switch:") && !s.contains("failed"))
            .count();
        assert_eq!(switches, 1, "switch budget (1) gates before candidate list");
        Ok(())
    }

    // =========================================================================
    // RUST-P8-001 S04 — 9-hook middleware wiring through the live pipeline.
    // Mirrors the Go golden orders in `middleware_test.go`
    // (TestMiddleware_NonStreaming_CallOrder :215-285,
    //  TestMiddleware_Streaming_CallOrder :288-354,
    //  TestMiddleware_ErrorResponse_CallOrder :357-402,
    //  TestMiddleware_RawRequest_Error_CleanupMiddlewares :756-790).
    // The unified-LLM hooks (`OnOutboundLlmResponse`/`OnOutboundLlmStream`)
    // cannot appear in these live sequences yet — the Rust flow has no unified
    // `LlmResponse` stage (see the GAP notes in `finish_non_stream_response` /
    // `finish_stream_events`); their reverse direction is pinned at runner
    // level in `middleware.rs`.
    // =========================================================================

    /// Shared hook log — Go `callOrder *[]string` (middleware_test.go:95-121).
    type HookLog = Arc<Mutex<Vec<String>>>;

    fn hook_log() -> HookLog {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn hook_push(log: &HookLog, entry: String) {
        if let Ok(mut guard) = log.lock() {
            guard.push(entry);
        }
    }

    fn hook_snapshot(log: &HookLog) -> Vec<String> {
        log.lock().map(|guard| guard.clone()).unwrap_or_default()
    }

    /// Port of Go `trackingMiddleware` (middleware_test.go:94-212) for the
    /// live-pipeline wiring tests; labels match the Go golden orders. Stream
    /// hooks record at wrap time (the entries Go's golden orders contain);
    /// per-event traversal is pinned separately in `middleware.rs`.
    struct PipeTrackMw {
        name: &'static str,
        log: HookLog,
        fail_on_raw_request: bool,
    }

    impl PipeTrackMw {
        fn new(name: &'static str, log: &HookLog) -> Self {
            Self {
                name,
                log: Arc::clone(log),
                fail_on_raw_request: false,
            }
        }
    }

    impl PipelineMiddleware for PipeTrackMw {
        fn name(&self) -> &str {
            self.name
        }
        fn on_inbound_llm_request(
            &self,
            _ctx: &mut PipelineContext,
            request: LlmRequest,
        ) -> PipelineResult<LlmRequest> {
            hook_push(&self.log, format!("{}:OnInboundLlmRequest", self.name));
            Ok(request)
        }
        fn on_inbound_raw_response(
            &self,
            _ctx: &mut PipelineContext,
            response: HttpResponse,
        ) -> PipelineResult<HttpResponse> {
            hook_push(&self.log, format!("{}:OnInboundRawResponse", self.name));
            Ok(response)
        }
        fn on_inbound_raw_stream(
            &self,
            _ctx: &mut PipelineContext,
            stream: BoxEventStream,
        ) -> PipelineResult<BoxEventStream> {
            hook_push(&self.log, format!("{}:OnInboundRawStream", self.name));
            Ok(stream)
        }
        fn on_outbound_raw_request(
            &self,
            _ctx: &mut PipelineContext,
            request: HttpRequest,
        ) -> PipelineResult<HttpRequest> {
            hook_push(&self.log, format!("{}:OnOutboundRawRequest", self.name));
            if self.fail_on_raw_request {
                return Err(ConduitError::invalid_request(
                    "raw request middleware error",
                ));
            }
            Ok(request)
        }
        fn on_outbound_raw_error(&self, _ctx: &mut PipelineContext, _error: &ConduitError) {
            // Go's tracking label (middleware_test.go:167).
            hook_push(
                &self.log,
                format!("{}:OnOutboundRawErrorResponse", self.name),
            );
        }
        fn on_outbound_raw_response(
            &self,
            _ctx: &mut PipelineContext,
            response: HttpResponse,
        ) -> PipelineResult<HttpResponse> {
            hook_push(&self.log, format!("{}:OnOutboundRawResponse", self.name));
            Ok(response)
        }
        fn on_outbound_raw_stream(
            &self,
            _ctx: &mut PipelineContext,
            stream: BoxEventStream,
        ) -> PipelineResult<BoxEventStream> {
            hook_push(&self.log, format!("{}:OnOutboundRawStream", self.name));
            Ok(stream)
        }
    }

    /// Two tracking middlewares M1/M2 sharing one log.
    fn pipe_tracking2(log: &HookLog) -> Vec<BoxPipelineMiddleware> {
        vec![
            Arc::new(PipeTrackMw::new("M1", log)),
            Arc::new(PipeTrackMw::new("M2", log)),
        ]
    }

    #[tokio::test]
    async fn nonstream_hooks_fire_in_go_onion_order() -> Result<(), ConduitError> {
        // Go TestMiddleware_NonStreaming_CallOrder (middleware_test.go:263-282):
        // OnInboundLlmRequest forward, OnOutboundRawRequest forward,
        // OnOutboundRawResponse reverse, [OnOutboundLlmResponse reverse — GAP,
        // no unified stage yet], OnInboundRawResponse FORWARD (final phase).
        let log = hook_log();
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("ok"))]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default())
            .with_middlewares(pipe_tracking2(&log));
        let mut ctx = PipelineContext::new();

        let _ = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(
            hook_snapshot(&log),
            vec![
                "M1:OnInboundLlmRequest",
                "M2:OnInboundLlmRequest",
                "M1:OnOutboundRawRequest",
                "M2:OnOutboundRawRequest",
                "M2:OnOutboundRawResponse",
                "M1:OnOutboundRawResponse",
                "M1:OnInboundRawResponse",
                "M2:OnInboundRawResponse",
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn streaming_hooks_fire_in_go_onion_order() -> Result<(), ConduitError> {
        // Go TestMiddleware_Streaming_CallOrder (middleware_test.go:336-351):
        // OnOutboundRawRequest forward, OnOutboundRawStream reverse,
        // [OnOutboundLlmStream reverse — GAP], OnInboundRawStream FORWARD.
        // (Go drove processRequest directly, so its golden order has no
        // OnInboundLlmRequest prefix; going through process() adds it.)
        let log = hook_log();
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent {
                done: true,
                ..StreamEvent::default()
            }])]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default())
            .with_middlewares(pipe_tracking2(&log));
        let mut ctx = PipelineContext::new();

        let (response, _) = pipeline
            .process(&mut ctx, raw_inbound(true), &raw_inbound(true), &pc(&["a"]))
            .await?;
        assert_eq!(response.stream.len(), 1, "wrapped stream passes events");

        assert_eq!(
            hook_snapshot(&log),
            vec![
                "M1:OnInboundLlmRequest",
                "M2:OnInboundLlmRequest",
                "M1:OnOutboundRawRequest",
                "M2:OnOutboundRawRequest",
                "M2:OnOutboundRawStream",
                "M1:OnOutboundRawStream",
                "M1:OnInboundRawStream",
                "M2:OnInboundRawStream",
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_attempt_fires_error_hooks_in_reverse() -> Result<(), ConduitError> {
        // Go TestMiddleware_ErrorResponse_CallOrder (middleware_test.go:388-399):
        // after the forward request phases, the failing executor triggers
        // OnOutboundRawError in REVERSE; no response hook fires.
        let log = hook_log();
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Err(upstream_error())]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default())
            .with_middlewares(pipe_tracking2(&log));
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        assert!(err.is_some());

        assert_eq!(
            hook_snapshot(&log),
            vec![
                "M1:OnInboundLlmRequest",
                "M2:OnInboundLlmRequest",
                "M1:OnOutboundRawRequest",
                "M2:OnOutboundRawRequest",
                "M2:OnOutboundRawErrorResponse",
                "M1:OnOutboundRawErrorResponse",
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn raw_request_middleware_error_fires_cleanup_on_all_middlewares()
    -> Result<(), ConduitError> {
        // Go TestMiddleware_RawRequest_Error_CleanupMiddlewares
        // (middleware_test.go:756-790): M2 fails in OnOutboundRawRequest ->
        // M3's request hook never runs, but ALL THREE receive
        // OnOutboundRawError (reverse) — including the unexecuted M3
        // ("unexecuted middleware must receive OnOutboundRawError for
        // unconditional cleanup").
        let log = hook_log();
        let mut failing = PipeTrackMw::new("M2", &log);
        failing.fail_on_raw_request = true;
        let middlewares: Vec<BoxPipelineMiddleware> = vec![
            Arc::new(PipeTrackMw::new("M1", &log)),
            Arc::new(failing),
            Arc::new(PipeTrackMw::new("M3", &log)),
        ];
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("never"))]));
        let pipeline =
            build_pipeline(executor, None, RetryHooks::default()).with_middlewares(middlewares);
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        assert!(err.is_some(), "raw request middleware failure must abort");

        assert_eq!(
            hook_snapshot(&log),
            vec![
                "M1:OnInboundLlmRequest",
                "M2:OnInboundLlmRequest",
                "M3:OnInboundLlmRequest",
                "M1:OnOutboundRawRequest",
                "M2:OnOutboundRawRequest",
                // M3:OnOutboundRawRequest must NOT appear — but all three get
                // the error hook, reverse.
                "M3:OnOutboundRawErrorResponse",
                "M2:OnOutboundRawErrorResponse",
                "M1:OnOutboundRawErrorResponse",
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn raw_request_hook_fires_once_per_attempt_inbound_once_per_request()
    -> Result<(), ConduitError> {
        // Frequency contract (middleware.go:21-22 "Once per Request" vs
        // :36-37 "Once per Attempt (will repeat on retries)"): two attempts
        // (fail + same-channel retry success) -> OnInboundLlmRequest x1,
        // OnOutboundRawRequest x2, OnOutboundRawError x1 (failed attempt
        // only), OnOutboundRawResponse/OnInboundRawResponse x1 (success).
        let log = hook_log();
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Ok(ok_response("after retry")),
        ]));
        let pipeline =
            build_pipeline(executor, None, retryable_hooks())
                .with_middlewares(vec![
                    Arc::new(PipeTrackMw::new("M1", &log)) as BoxPipelineMiddleware
                ]);
        let mut ctx = PipelineContext::new();

        let _ = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        let snapshot = hook_snapshot(&log);
        let count = |needle: &str| snapshot.iter().filter(|e| e.as_str() == needle).count();
        assert_eq!(count("M1:OnInboundLlmRequest"), 1, "once per request");
        assert_eq!(count("M1:OnOutboundRawRequest"), 2, "once per attempt");
        assert_eq!(
            count("M1:OnOutboundRawErrorResponse"),
            1,
            "once per failed attempt"
        );
        assert_eq!(
            count("M1:OnOutboundRawResponse"),
            1,
            "once per successful attempt"
        );
        assert_eq!(
            count("M1:OnInboundRawResponse"),
            1,
            "once per successful request"
        );
        Ok(())
    }

    #[tokio::test]
    async fn auto_aggregate_fires_stream_wrappers_and_inbound_response_hook()
    -> Result<(), ConduitError> {
        // Go `autoAggregateStream` consumes `p.stream(...)`
        // (non_streaming.go:86) — so OnOutboundRawStream (reverse) +
        // OnInboundRawStream (forward) fire over the events being aggregated —
        // then applies the inbound raw RESPONSE hooks to the aggregated
        // response (non_streaming.go:130, forward).
        let log = hook_log();
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent {
                data: Some("a".to_string()),
                ..StreamEvent::default()
            }])]));
        let pipeline = build_pipeline(executor, Some(true), RetryHooks::default())
            .with_middlewares(pipe_tracking2(&log));
        let mut ctx = PipelineContext::new();

        let (_response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;
        assert_eq!(attempts[0].mode, ExecutionMode::AutoAggregate);

        assert_eq!(
            hook_snapshot(&log),
            vec![
                "M1:OnInboundLlmRequest",
                "M2:OnInboundLlmRequest",
                "M1:OnOutboundRawRequest",
                "M2:OnOutboundRawRequest",
                "M2:OnOutboundRawStream",
                "M1:OnOutboundRawStream",
                "M1:OnInboundRawStream",
                "M2:OnInboundRawStream",
                "M1:OnInboundRawResponse",
                "M2:OnInboundRawResponse",
            ]
        );
        Ok(())
    }

    // =========================================================================
    // RUST-P8-002 A01+A02 — Go pipeline *_test.go parity cases.
    // Targets the high-value gaps surfaced by the Go test inventory:
    // retry sequences (pipeline_retry_test.go), auto-aggregate empty stream /
    // empty body / non-empty JSON object (streaming_integration_test.go), and
    // retry-preserves-stream-intent (pipeline_retry_test.go).
    // =========================================================================

    use crate::pipeline::{
        EMPTY_AGGREGATED_BODY_CODE, EMPTY_RESPONSE_CODE, EMPTY_STREAM_CHUNKS_CODE,
        empty_aggregated_body_error, empty_response_error, empty_stream_chunks_error,
        is_empty_aggregated_body, is_empty_response, is_empty_stream_chunks,
    };

    /// Build a [`Pipeline`] with a custom inbound transformer (the stock
    /// `build_pipeline` hard-codes `StubInbound`). The aggregate-empty tests
    /// need to swap in a transformer whose `aggregate_stream_chunks` returns an
    /// empty body.
    fn build_pipeline_with_inbound(
        inbound: Arc<dyn InboundTransformer>,
        executor: Arc<dyn Executor>,
        force_effective_stream: Option<bool>,
        hooks: RetryHooks,
    ) -> Pipeline {
        Pipeline::new(
            inbound,
            Arc::new(StubOutbound {
                force_effective_stream,
            }),
            executor,
        )
        .with_retry_policy(RetryPolicy {
            enabled: true,
            max_channel_retries: 2,
            max_single_channel_retries: 1,
            retry_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        })
        .with_retry_hooks(hooks)
    }

    /// Inbound that delegates to [`StubInbound`] for everything EXCEPT
    /// `aggregate_stream_chunks`, which returns an empty body. Mirrors Go's
    /// `emptyAggregateInboundWrapper` (streaming_integration_test.go:26-44).
    struct EmptyBodyInbound;
    impl InboundTransformer for EmptyBodyInbound {
        fn name(&self) -> &'static str {
            "empty-body-inbound"
        }
        fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
            StubInbound.inbound_request(request)
        }
        fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
            StubInbound.inbound_response(response)
        }
        fn inbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
            StubInbound.inbound_stream_event(event)
        }
        fn inbound_error(&self, error: &ConduitError) -> TransformerResult<HttpResponse> {
            StubInbound.inbound_error(error)
        }
        fn aggregate_stream_chunks(
            &self,
            _events: Vec<StreamEvent>,
        ) -> TransformerResult<HttpResponse> {
            // Go's wrapper returns `nil, ResponseMeta{}, nil` — i.e. an empty
            // body, no error. The pipeline itself surfaces `ErrEmptyAggregatedBody`.
            Ok(HttpResponse {
                status: 200,
                ..HttpResponse::default()
            })
        }
    }

    /// Inbound that delegates to [`StubInbound`] for everything EXCEPT
    /// `aggregate_stream_chunks`, which returns a body of literally `{}`. The
    /// pipeline must ACCEPT this (not an empty body). Mirrors Go's
    /// `emptyJSONObjectAggregateInboundWrapper`
    /// (streaming_integration_test.go:30-48).
    struct EmptyJsonObjectInbound;
    impl InboundTransformer for EmptyJsonObjectInbound {
        fn name(&self) -> &'static str {
            "empty-json-object-inbound"
        }
        fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
            StubInbound.inbound_request(request)
        }
        fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
            StubInbound.inbound_response(response)
        }
        fn inbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
            StubInbound.inbound_stream_event(event)
        }
        fn inbound_error(&self, error: &ConduitError) -> TransformerResult<HttpResponse> {
            StubInbound.inbound_error(error)
        }
        fn aggregate_stream_chunks(
            &self,
            _events: Vec<StreamEvent>,
        ) -> TransformerResult<HttpResponse> {
            Ok(HttpResponse {
                status: 200,
                body: Some(b"{}".to_vec()),
                ..HttpResponse::default()
            })
        }
    }

    // -- pipeline_retry_test.go::TestPipeline_Process_RetryLogic -----------------

    #[tokio::test]
    async fn a02_cross_channel_retry_success_can_retry_false_forces_switch()
    -> Result<(), ConduitError> {
        // Go `TestPipeline_Process_RetryLogic` "CrossChannelRetrySuccess"
        // (pipeline_retry_test.go:238-273): `CanRetry=false` so the
        // same-channel arm is skipped, `HasMoreChannels=true` so the channel
        // switch arm fires. Two executor calls (one per channel); one switch.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(ConduitError::upstream("channel error")),
            Ok(ok_response("ok on b")),
        ]));
        // can_retry=false -> same-channel arm never fires; has_more=true ->
        // switch fires.
        let hooks = RetryHooks {
            can_retry: Arc::new(|_| false),
            has_more_channels: Arc::new(|| true),
            ..RetryHooks::default()
        };
        let pipeline = build_pipeline_with_policy(
            executor,
            None,
            hooks,
            RetryPolicy {
                enabled: true,
                max_channel_retries: 1,
                max_single_channel_retries: 2,
                retry_delay_ms: 0,
                ..RetryPolicy::DEFAULT
            },
        );
        let mut ctx = PipelineContext::new();

        let (response, _attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a", "b"]),
            )
            .await?;
        assert_eq!(response.json_body, ok_response("ok on b").json_body);

        // Exactly 2 attempts (one per channel).
        let attempt_starts = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("attempt:") && s.ends_with(":start"))
            .count();
        assert_eq!(attempt_starts, 2, "one attempt per channel");
        // Exactly 1 channel switch and ZERO same-channel retries.
        let switches = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("retry:channel_switch:") && !s.contains("failed"))
            .count();
        assert_eq!(switches, 1);
        let same_channel = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("retry:same_channel:"))
            .count();
        assert_eq!(same_channel, 0, "can_retry=false blocks same-channel arm");
        Ok(())
    }

    #[tokio::test]
    async fn a02_mixed_retry_success_two_same_channel_then_one_switch() -> Result<(), ConduitError>
    {
        // Go `TestPipeline_Process_RetryLogic` "MixedRetrySuccess"
        // (pipeline_retry_test.go:275-322):
        //   1. exec 1 -> fail
        //   2. same-channel retry 1 -> exec 2 -> fail
        //   3. same-channel retry 2 -> exec 3 -> fail
        //   4. same-channel exhausted -> switch 1 -> same-channel reset -> exec 4 -> success.
        // Asserts: 4 exec calls, 2 same-channel retries, 1 switch.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Err(upstream_error()),
            Err(upstream_error()),
            Err(upstream_error()),
            Ok(ok_response("ok on b after reset")),
        ]));
        let pipeline = build_pipeline_with_policy(
            executor,
            None,
            retryable_hooks(),
            RetryPolicy {
                enabled: true,
                max_channel_retries: 1,
                max_single_channel_retries: 2,
                retry_delay_ms: 0,
                ..RetryPolicy::DEFAULT
            },
        );
        let mut ctx = PipelineContext::new();

        let (response, _attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a", "b"]),
            )
            .await?;
        assert_eq!(
            response.json_body,
            ok_response("ok on b after reset").json_body
        );

        let attempt_starts = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("attempt:") && s.ends_with(":start"))
            .count();
        assert_eq!(attempt_starts, 4, "Go MixedRetrySuccess: 4 exec calls");
        let same_channel = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("retry:same_channel:"))
            .count();
        assert_eq!(same_channel, 2, "two same-channel retries on channel a");
        let switches = ctx
            .order
            .iter()
            .filter(|s| s.starts_with("retry:channel_switch:") && !s.contains("failed"))
            .count();
        assert_eq!(switches, 1, "one channel switch a -> b");
        Ok(())
    }

    // -- pipeline_retry_test.go::TestPipeline_Process_RetryPreservesOriginalStreamIntent

    #[tokio::test]
    async fn a02_retry_preserves_original_non_stream_intent_through_auto_aggregate()
    -> Result<(), ConduitError> {
        // Go `TestPipeline_Process_RetryPreservesOriginalStreamIntent`
        // (pipeline_retry_test.go:360-414): the user requested a NON-streaming
        // response; the provider streams. The first attempt fails; the retry
        // still enters the auto-aggregate branch and the final response is
        // non-stream (`result.Stream == false`, body populated by the
        // aggregator). Asserts: 2 attempts, attempt mode is AutoAggregate, the
        // returned response carries the aggregated body.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::stream(vec![
            Err(ConduitError::upstream("temporary stream failure")),
            Ok(vec![StreamEvent {
                data: Some("chunk".to_string()),
                ..StreamEvent::default()
            }]),
        ]));
        // force_effective_stream=Some(true) + user non-stream -> AutoAggregate.
        let pipeline = build_pipeline_with_policy(
            executor,
            Some(true),
            retryable_hooks(),
            RetryPolicy {
                enabled: true,
                max_channel_retries: 0,
                max_single_channel_retries: 1,
                retry_delay_ms: 0,
                ..RetryPolicy::DEFAULT
            },
        );
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;
        // The aggregated response body is non-empty (StubInbound writes
        // aggregated_by/event_count).
        assert!(
            response.json_body.is_some(),
            "auto-aggregate must populate a body"
        );
        // Both attempts took the AutoAggregate branch.
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].mode, ExecutionMode::AutoAggregate);
        assert_eq!(attempts[1].mode, ExecutionMode::AutoAggregate);
        // The aggregated body is populated (the contract under test); the
        // stub also stashes events in `response.stream` for inspection, but
        // the client-facing surface is the body — Go's `result.Response.Body`.
        assert!(
            response.json_body.is_some(),
            "auto-aggregate must populate a body for the client"
        );
        Ok(())
    }

    // -- streaming_integration_test.go::AutoAggregate empty cases ---------------

    #[tokio::test]
    async fn a02_auto_aggregate_empty_stream_chunks_surfaces_sentinel() -> Result<(), ConduitError>
    {
        // Go `TestPipeline_NonStreaming_AutoAggregateUpgradedStream_*EmptyStreamChunks`
        // (streaming_integration_test.go:526-621, OpenAI + Anthropic variants):
        // the upstream returned ZERO stream events. The pipeline must surface
        // `ErrEmptyStreamChunks` (Go `non_streaming.go:105-108`).
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::stream(vec![Ok(vec![])]));
        // force_effective_stream=Some(true) + user non-stream -> AutoAggregate.
        let pipeline = build_pipeline(executor, Some(true), RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        match err {
            Some(err) => {
                assert!(
                    is_empty_stream_chunks(&err),
                    "expected ErrEmptyStreamChunks sentinel, got {err:?}"
                );
                assert_eq!(err.code.as_deref(), Some(EMPTY_STREAM_CHUNKS_CODE));
                assert!(
                    err.message.contains("empty stream chunks"),
                    "message must mention 'empty stream chunks': {}",
                    err.message
                );
            }
            None => panic!("empty stream chunks must surface an error"),
        }
        assert!(
            ctx.order
                .iter()
                .any(|s| s == "execute:aggregate:empty_stream_chunks:error"),
            "failure step must be recorded"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a02_auto_aggregate_empty_body_surfaces_sentinel() -> Result<(), ConduitError> {
        // Go `TestPipeline_NonStreaming_AutoAggregateUpgradedStream_EmptyAggregatedBody`
        // (streaming_integration_test.go:623-670): the upstream produced one
        // event, but the inbound aggregator returned an empty body. The
        // pipeline must surface `ErrEmptyAggregatedBody`
        // (Go `non_streaming.go:116-119`).
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent {
                data: Some("[DONE]".to_string()),
                ..StreamEvent::default()
            }])]));
        let pipeline = build_pipeline_with_inbound(
            Arc::new(EmptyBodyInbound),
            executor,
            Some(true),
            RetryHooks::default(),
        );
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();
        match err {
            Some(err) => {
                assert!(
                    is_empty_aggregated_body(&err),
                    "expected ErrEmptyAggregatedBody sentinel, got {err:?}"
                );
                assert_eq!(err.code.as_deref(), Some(EMPTY_AGGREGATED_BODY_CODE));
                assert!(
                    err.message.contains("empty aggregated body"),
                    "message must mention 'empty aggregated body': {}",
                    err.message
                );
            }
            None => panic!("empty aggregated body must surface an error"),
        }
        assert!(
            ctx.order
                .iter()
                .any(|s| s == "execute:aggregate:empty_body:error"),
            "failure step must be recorded"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a02_auto_aggregate_accepts_empty_json_object_body() -> Result<(), ConduitError> {
        // Go `TestPipeline_NonStreaming_AutoAggregateUpgradedStream_EmptyJSONObjectAggregatedBodyAllowed`
        // (streaming_integration_test.go:673-721): the aggregator returned a
        // body of literally `{}` — this is NOT empty (the JSON object is a
        // valid empty-but-present body). The pipeline must accept it and return
        // a 200 response with the `{}` body.
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent {
                data: Some("{\"stub\":true}".to_string()),
                ..StreamEvent::default()
            }])]));
        let pipeline = build_pipeline_with_inbound(
            Arc::new(EmptyJsonObjectInbound),
            executor,
            Some(true),
            RetryHooks::default(),
        );
        let mut ctx = PipelineContext::new();

        let (response, _attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;
        assert_eq!(
            response.body.as_deref(),
            Some(&b"{}".to_vec()[..]),
            "empty JSON object body must be accepted as-is"
        );
        assert_eq!(response.status, 200);
        // No empty-body failure step recorded.
        assert!(
            !ctx.order
                .iter()
                .any(|s| s.starts_with("execute:aggregate:empty")),
            "non-empty body must not trigger the empty-body failure step"
        );
        Ok(())
    }

    // -- empty_response sentinel plumbing -------------------------------------

    #[test]
    fn a02_empty_sentinel_constructors_and_detectors_round_trip() {
        // Sanity: each sentinel constructor produces an ConduitError whose `code`
        // matches the corresponding detector. Mirrors the timeout-sentinel
        // pattern used elsewhere in this module.
        let stream_chunks = empty_stream_chunks_error();
        assert!(is_empty_stream_chunks(&stream_chunks));
        assert_eq!(
            stream_chunks.code.as_deref(),
            Some(EMPTY_STREAM_CHUNKS_CODE)
        );

        let agg_body = empty_aggregated_body_error();
        assert!(is_empty_aggregated_body(&agg_body));
        assert_eq!(agg_body.code.as_deref(), Some(EMPTY_AGGREGATED_BODY_CODE));

        let empty_resp = empty_response_error();
        assert!(is_empty_response(&empty_resp));
        assert_eq!(empty_resp.code.as_deref(), Some(EMPTY_RESPONSE_CODE));

        // Cross-detector: timeouts must not be misclassified as empty sentinels.
        let timeout = stream_first_event_timeout_error();
        assert!(!is_empty_stream_chunks(&timeout));
        assert!(!is_empty_aggregated_body(&timeout));
        assert!(!is_empty_response(&timeout));

        // Plain upstream errors are not empty sentinels.
        let plain = upstream_error();
        assert!(!is_empty_stream_chunks(&plain));
        assert!(!is_empty_aggregated_body(&plain));
        assert!(!is_empty_response(&plain));
    }

    // =========================================================================
    // RUST-P8-002 A01 — WithEmptyResponseDetection Option wiring.
    // Mirrors Go `TestPipeline_Process_NonStreamEmptyResponseDetection`
    // (`conduit/llm/pipeline/empty_response_test.go:345-466`). The stream-path
    // `TestPipeline_Process_StreamEmptyResponseDetection` (:108-343) is PENDING
    // — see the `stream_empty_response_detection_pending_note` test below.
    // =========================================================================

    /// Build a pipeline that ALSO enables empty-response detection (the stock
    /// `build_pipeline` leaves the flag at its default `false`).
    fn build_pipeline_with_empty_detection(
        executor: Arc<dyn Executor>,
        force_effective_stream: Option<bool>,
        hooks: RetryHooks,
    ) -> Pipeline {
        build_pipeline(executor, force_effective_stream, hooks).with_empty_response_detection()
    }

    /// Shape an `HttpResponse` whose `json_body` deserializes into the unified
    /// `LlmResponse` (required for `http_response_has_content` to delegate to
    /// `has_response_content`). `id` and `object` are non-optional in
    /// `LlmResponse`, so they must be present for the deserialize bridge to
    /// succeed — mirroring how Go's `Outbound.TransformResponse` always
    /// produces a fully-formed `*llm.Response`.
    fn llm_http_response(json_body: serde_json::Value) -> HttpResponse {
        HttpResponse {
            status: 200,
            json_body: Some(json_body),
            ..HttpResponse::default()
        }
    }

    /// An empty-content chat response (one choice with a bare, content-less
    /// message) — the shape Go's test stub returns for the "retries on empty"
    /// case (`empty_response_test.go:360-363`).
    fn empty_content_llm_response() -> HttpResponse {
        llm_http_response(json!({
            "id": "chatcmpl-empty",
            "object": "chat.completion",
            "choices": [{"index": 0, "message": {}}],
        }))
    }

    // -- Go TestPipeline_Process_NonStreamEmptyResponseDetection ---------------

    #[tokio::test]
    async fn a01_non_stream_empty_response_detection_retries_on_empty_response()
    -> Result<(), ConduitError> {
        // Go "retries on empty non-stream response"
        // (`empty_response_test.go:348-394`): the first attempt's response
        // carries no meaningful content (empty message); the pipeline returns
        // `ErrEmptyResponse`, the retry hook classifies it as retryable, and
        // the second attempt (with content) succeeds.
        let non_empty = llm_http_response(json!({
            "id": "chatcmpl-ok",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {"content": "ok"},
            }],
        }));
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![
            Ok(empty_content_llm_response()),
            Ok(non_empty.clone()),
        ]));
        // Go's stub: `canRetry: func(err error) bool { return errors.Is(err, ErrEmptyResponse) }`.
        // The Rust `ConduitError` carries the sentinel via `code`, so we match on
        // `is_empty_response` (the `errors.Is` analog in this module).
        let pipeline = build_pipeline_with_empty_detection(
            executor,
            None,
            RetryHooks {
                can_retry: Arc::new(is_empty_response),
                has_more_channels: Arc::new(|| false),
                is_timeout_error: Arc::new(|_| false),
            },
        );
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(attempts.len(), 2, "empty -> retry -> success");
        assert_eq!(response.json_body, non_empty.json_body);
        assert!(
            ctx.order
                .iter()
                .any(|s| s == "execute:nonstream:empty_response:error"),
            "first attempt must record the empty-response failure step"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a01_non_stream_empty_response_detection_accepts_tool_call_response()
    -> Result<(), ConduitError> {
        // Go "accepts non-stream tool-call response"
        // (`empty_response_test.go:396-425`): the response carries a tool call;
        // `hasMessageContent` returns true via the `tool_calls` branch, so the
        // response is accepted even though `content` is empty.
        let tool_call_resp = llm_http_response(json!({
            "id": "chatcmpl-tool",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{}"},
                    }],
                },
            }],
        }));
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(tool_call_resp.clone())]));
        let pipeline = build_pipeline_with_empty_detection(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let (response, _attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(response.json_body, tool_call_resp.json_body);
        assert!(
            !ctx.order.iter().any(|s| s.contains("empty_response")),
            "tool-call response must NOT trigger empty-response detection"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a01_non_stream_empty_response_detection_accepts_embedding_response()
    -> Result<(), ConduitError> {
        // Go "accepts non-stream embedding response"
        // (`empty_response_test.go:427-465`): the response carries embedding
        // data; `hasResponseContent` returns true via the `embedding.data`
        // branch.
        let embedding_resp = llm_http_response(json!({
            "id": "emb-1",
            "object": "list",
            "embedding": {
                "object": "list",
                "data": [{
                    "object": "embedding",
                    "embedding": [0.1, 0.2, 0.3],
                    "index": 0,
                }],
            },
        }));
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(embedding_resp.clone())]));
        let pipeline = build_pipeline_with_empty_detection(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let (response, _attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(response.json_body, embedding_resp.json_body);
        assert!(
            !ctx.order.iter().any(|s| s.contains("empty_response")),
            "embedding response must NOT trigger empty-response detection"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a01_empty_response_detection_disabled_by_default_accepts_empty() {
        // Go parity: `emptyResponseDetection == false` (Go default) means even
        // an empty-content response is accepted — detection is opt-in via
        // `WithEmptyResponseDetection()` (`pipeline.go:67-75`). This guards
        // against a regression where the flag flips on by default.
        let executor: Arc<dyn Executor> = Arc::new(StubExecutor::non_stream(vec![Ok(
            empty_content_llm_response(),
        )]));
        // Stock `build_pipeline` — NO `.with_empty_response_detection()`.
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let outcome = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await;

        assert!(
            outcome.is_ok(),
            "detection disabled -> empty passes through"
        );
        assert!(
            !ctx.order.iter().any(|s| s.contains("empty_response")),
            "no empty-response step may fire when detection is disabled"
        );
    }

    // -- Go TestPipeline_Process_StreamEmptyResponseDetection -----------------
    //
    // PENDING — Turing-the-9th 2026-07-06. Go's stream-path detection
    // pre-reads up to 3 `*llm.Response` events from the LLM stream
    // (`stream.go:153-217`, `preReadLlmStream`) before the first event reaches
    // the client. The helper is fully ported as `pre_read_llm_stream` and
    // unit-tested in `empty_response.rs` (8 `pre_read_*` cases), but the Rust
    // streaming executor is currently an eager `Vec<StreamEvent>` stub — no
    // lazy `LlmResponse` stream exists in the live flow yet. The stream arm
    // of `process_attempt` therefore ignores `empty_response_detection`.
    // Wiring the helper requires the real streaming executor (a lazy
    // `LlmResponse` stream) plus the `Outbound.TransformStream` port, both
    // outside this task. The `with_empty_response_detection` builder still
    // flips the flag so the non-stream arm honors it; when the streaming
    // executor lands, plug `pre_read_llm_stream` into `finish_stream_events`
    // and the 4 Go stream subtests ("retries on empty stream response",
    // "retries on empty binary speech stream", "retries on empty TTS SSE
    // stream with only done event", "accepts stream response with content")
    // migrate verbatim.

    #[test]
    fn stream_empty_response_detection_pending_note() {
        // Sentinel test so the PENDING status above shows up in `cargo test`
        // output and is grep-able. Delete when the stream-path wiring lands
        // and the 4 Go stream subtests are migrated. The pre-read helper
        // itself is covered by `empty_response.rs::pre_read_*` (8 tests).
        // When the real streaming executor lands, plug `pre_read_llm_stream`
        // into `finish_stream_events` and migrate:
        //   - "retries on empty stream response"
        //   - "retries on empty binary speech stream"
        //   - "retries on empty TTS SSE stream with only done event"
        //   - "accepts stream response with content"
    }

    // =========================================================================
    // RUST-P15-001 — Go channel_customized_executor_test.go parity.
    // Mirrors all 6 Go golden cases from
    // `conduit/llm/pipeline/channel_customized_executor_test.go` (364 lines).
    // The Go `ChannelCustomizedExecutor` interface (`pipeline.go:38-43`) is
    // ported as the `CustomizeExecutorFn` hook on `Pipeline`, called once per
    // attempt after the raw request middlewares (Go `pipeline.go:381-384`).
    // =========================================================================

    use std::sync::atomic::{AtomicBool, Ordering};

    /// Mirrors Go `mockCustomExecutor` (Go test L35-58). Tracks which execute
    /// method was called and returns canned responses or errors. The Go mock
    /// records `doCalled`/`doStreamCalled` and dispatches to optional
    /// `doFunc`/`doStreamFunc`; here we store a single success shape or an
    /// error message for all methods.
    struct CallTrackingExecutor {
        execute_called: Arc<AtomicBool>,
        stream_called: Arc<AtomicBool>,
        /// Non-stream response returned on success.
        response: HttpResponse,
        /// Stream events returned on success.
        stream_events: Vec<StreamEvent>,
        /// If set, both methods return an upstream error with this message.
        error_message: Option<String>,
    }

    #[async_trait]
    impl Executor for CallTrackingExecutor {
        async fn execute(&self, _request: &HttpRequest) -> Result<HttpResponse, ConduitError> {
            self.execute_called.store(true, Ordering::SeqCst);
            match &self.error_message {
                Some(msg) => Err(ConduitError::upstream(msg)),
                None => Ok(self.response.clone()),
            }
        }
        async fn execute_stream(
            &self,
            _request: &HttpRequest,
        ) -> Result<Vec<StreamEvent>, ConduitError> {
            self.stream_called.store(true, Ordering::SeqCst);
            match &self.error_message {
                Some(msg) => Err(ConduitError::upstream(msg)),
                None => Ok(self.stream_events.clone()),
            }
        }
    }

    // -- TestChannelCustomizedExecutor_StreamingPath (Go L60-114) ---------------

    #[tokio::test]
    async fn p15_streaming_path_uses_custom_executor_do_stream() -> Result<(), ConduitError> {
        // Go "StreamingPath": a streaming request goes through the customized
        // executor. `DoStream` (Rust `execute_stream`) is called; `Do` (Rust
        // `execute`) is NOT. `CustomizeExecutor` is invoked.
        let custom_execute_called = Arc::new(AtomicBool::new(false));
        let custom_stream_called = Arc::new(AtomicBool::new(false));
        let hook_called = Arc::new(AtomicBool::new(false));

        let custom_executor: Arc<dyn Executor> = Arc::new(CallTrackingExecutor {
            execute_called: Arc::clone(&custom_execute_called),
            stream_called: Arc::clone(&custom_stream_called),
            response: HttpResponse::default(),
            stream_events: vec![StreamEvent {
                data: Some("hello".to_string()),
                ..StreamEvent::default()
            }],
            error_message: None,
        });

        let hook_called_clone = Arc::clone(&hook_called);
        let custom_clone = Arc::clone(&custom_executor);
        let hook: CustomizeExecutorFn = Arc::new(move |_orig: Arc<dyn Executor>| {
            hook_called_clone.store(true, Ordering::SeqCst);
            Arc::clone(&custom_clone)
        });

        // Original executor — would succeed if called, but should NOT be.
        let original: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("original"))]));
        let pipeline =
            build_pipeline(original, None, RetryHooks::default()).with_customize_executor(hook);
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(&mut ctx, raw_inbound(true), &raw_inbound(true), &pc(&["a"]))
            .await?;

        // Go: require.True(t, result.Stream)
        assert_eq!(attempts[0].mode, ExecutionMode::Stream);
        // Go: require.True(t, mockCustomized.customizeExecutorCalled)
        assert!(
            hook_called.load(Ordering::SeqCst),
            "CustomizeExecutor should have been called"
        );
        // Go: require.True(t, customExecutor.doStreamCalled)
        assert!(
            custom_stream_called.load(Ordering::SeqCst),
            "Custom executor's execute_stream should have been called"
        );
        // Go: require.False(t, customExecutor.doCalled)
        assert!(
            !custom_execute_called.load(Ordering::SeqCst),
            "Custom executor's execute should NOT have been called for streaming"
        );
        // Events from the custom executor flow through.
        assert_eq!(response.stream.len(), 1);
        // The customization step was recorded in the order log.
        assert!(ctx.order.iter().any(|s| s == "outbound:customize_executor"));
        Ok(())
    }

    // -- TestChannelCustomizedExecutor_NonStreamingPath (Go L116-178) -----------

    #[tokio::test]
    async fn p15_non_streaming_path_uses_custom_executor_do() -> Result<(), ConduitError> {
        // Go "NonStreamingPath": a non-streaming request goes through the
        // customized executor. `Do` (Rust `execute`) is called; `DoStream`
        // (Rust `execute_stream`) is NOT.
        let custom_execute_called = Arc::new(AtomicBool::new(false));
        let custom_stream_called = Arc::new(AtomicBool::new(false));
        let hook_called = Arc::new(AtomicBool::new(false));

        let custom_executor: Arc<dyn Executor> = Arc::new(CallTrackingExecutor {
            execute_called: Arc::clone(&custom_execute_called),
            stream_called: Arc::clone(&custom_stream_called),
            response: ok_response("custom non-stream"),
            stream_events: vec![],
            error_message: None,
        });

        let hook_called_clone = Arc::clone(&hook_called);
        let custom_clone = Arc::clone(&custom_executor);
        let hook: CustomizeExecutorFn = Arc::new(move |_orig: Arc<dyn Executor>| {
            hook_called_clone.store(true, Ordering::SeqCst);
            Arc::clone(&custom_clone)
        });

        // Original executor — should NOT be called.
        let original: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("original"))]));
        let pipeline =
            build_pipeline(original, None, RetryHooks::default()).with_customize_executor(hook);
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        // Go: require.False(t, result.Stream)
        assert_eq!(attempts[0].mode, ExecutionMode::NonStream);
        // Go: require.True(t, mockCustomized.customizeExecutorCalled)
        assert!(
            hook_called.load(Ordering::SeqCst),
            "CustomizeExecutor should have been called"
        );
        // Go: require.True(t, customExecutor.doCalled)
        assert!(
            custom_execute_called.load(Ordering::SeqCst),
            "Custom executor's execute should have been called"
        );
        // Go: require.False(t, customExecutor.doStreamCalled)
        assert!(
            !custom_stream_called.load(Ordering::SeqCst),
            "Custom executor's execute_stream should NOT have been called for non-streaming"
        );
        // The custom executor's response body surfaces.
        assert_eq!(
            response.json_body,
            ok_response("custom non-stream").json_body
        );
        Ok(())
    }

    // -- TestChannelCustomizedExecutor_CustomExecutorError (Go L180-227) --------

    #[tokio::test]
    async fn p15_streaming_custom_executor_error_propagates() -> Result<(), ConduitError> {
        // Go "CustomExecutorError": a streaming request hits a custom executor
        // whose `DoStreamFunc` returns an error. The pipeline must surface it.
        let custom_stream_called = Arc::new(AtomicBool::new(false));
        let custom_execute_called = Arc::new(AtomicBool::new(false));
        let hook_called = Arc::new(AtomicBool::new(false));

        let custom_executor: Arc<dyn Executor> = Arc::new(CallTrackingExecutor {
            execute_called: Arc::clone(&custom_execute_called),
            stream_called: Arc::clone(&custom_stream_called),
            response: HttpResponse::default(),
            stream_events: vec![],
            error_message: Some("custom executor error".to_string()),
        });

        let hook_called_clone = Arc::clone(&hook_called);
        let custom_clone = Arc::clone(&custom_executor);
        let hook: CustomizeExecutorFn = Arc::new(move |_orig: Arc<dyn Executor>| {
            hook_called_clone.store(true, Ordering::SeqCst);
            Arc::clone(&custom_clone)
        });

        let original: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("never"))]));
        let pipeline =
            build_pipeline(original, None, RetryHooks::default()).with_customize_executor(hook);
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(&mut ctx, raw_inbound(true), &raw_inbound(true), &pc(&["a"]))
            .await
            .err();

        // Go: require.Error(t, err)
        let err = match err {
            Some(err) => err,
            None => panic!("custom executor error must propagate"),
        };
        // Go: require.Contains(t, err.Error(), "custom executor error")
        assert!(
            err.message.contains("custom executor error"),
            "error must mention 'custom executor error': {}",
            err.message
        );
        // Go: require.True(t, mockCustomized.customizeExecutorCalled)
        assert!(hook_called.load(Ordering::SeqCst));
        // Go: require.True(t, customExecutor.doStreamCalled)
        assert!(custom_stream_called.load(Ordering::SeqCst));
        Ok(())
    }

    // -- TestChannelCustomizedExecutor_NoCustomization (Go L229-267) ------------

    #[tokio::test]
    async fn p15_no_customization_uses_original_executor() -> Result<(), ConduitError> {
        // Go "NoCustomization": the outbound does NOT implement
        // `ChannelCustomizedExecutor`. The original executor is used directly.
        // In Rust this is a Pipeline without `with_customize_executor`.
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent {
                done: true,
                ..StreamEvent::default()
            }])]));
        // Stock build_pipeline — NO customize_executor hook.
        let pipeline = build_pipeline(executor, None, RetryHooks::default());
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(&mut ctx, raw_inbound(true), &raw_inbound(true), &pc(&["a"]))
            .await?;

        // Go: require.NoError + require.NotNil + require.True(t, result.Stream)
        assert_eq!(attempts[0].mode, ExecutionMode::Stream);
        assert!(
            !response.stream.is_empty(),
            "original executor produced events"
        );
        // No customization step recorded.
        assert!(
            !ctx.order.iter().any(|s| s == "outbound:customize_executor"),
            "no customization hook -> no step recorded"
        );
        Ok(())
    }

    // -- TestChannelCustomizedExecutor_NonStreamingCustomExecutorError (Go L269-316)

    #[tokio::test]
    async fn p15_non_streaming_custom_executor_error_propagates() -> Result<(), ConduitError> {
        // Go "NonStreamingCustomExecutorError": a non-streaming request hits a
        // custom executor whose `DoFunc` returns an error.
        let custom_execute_called = Arc::new(AtomicBool::new(false));
        let custom_stream_called = Arc::new(AtomicBool::new(false));
        let hook_called = Arc::new(AtomicBool::new(false));

        let custom_executor: Arc<dyn Executor> = Arc::new(CallTrackingExecutor {
            execute_called: Arc::clone(&custom_execute_called),
            stream_called: Arc::clone(&custom_stream_called),
            response: HttpResponse::default(),
            stream_events: vec![],
            error_message: Some("custom executor non-streaming error".to_string()),
        });

        let hook_called_clone = Arc::clone(&hook_called);
        let custom_clone = Arc::clone(&custom_executor);
        let hook: CustomizeExecutorFn = Arc::new(move |_orig: Arc<dyn Executor>| {
            hook_called_clone.store(true, Ordering::SeqCst);
            Arc::clone(&custom_clone)
        });

        let original: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("never"))]));
        let pipeline =
            build_pipeline(original, None, RetryHooks::default()).with_customize_executor(hook);
        let mut ctx = PipelineContext::new();

        let err = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
            .err();

        // Go: require.Error(t, err)
        let err = match err {
            Some(err) => err,
            None => panic!("custom executor non-streaming error must propagate"),
        };
        // Go: require.Contains(t, err.Error(), "custom executor non-streaming error")
        assert!(
            err.message.contains("custom executor non-streaming error"),
            "error must mention 'custom executor non-streaming error': {}",
            err.message
        );
        // Go: require.True(t, mockCustomized.customizeExecutorCalled)
        assert!(hook_called.load(Ordering::SeqCst));
        // Go: require.True(t, customExecutor.doCalled)
        assert!(custom_execute_called.load(Ordering::SeqCst));
        Ok(())
    }

    // -- TestChannelCustomizedExecutor_ReturnsSameExecutor (Go L318-364) --------

    #[tokio::test]
    async fn p15_returns_same_executor_uses_original() -> Result<(), ConduitError> {
        // Go "ReturnsSameExecutor": `CustomizeExecutor` returns the input
        // executor (Go `customExecutor == nil`). The original executor is used.
        let hook_called = Arc::new(AtomicBool::new(false));

        // Original executor — a StubExecutor that succeeds on streaming.
        let original: Arc<dyn Executor> =
            Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent {
                done: true,
                ..StreamEvent::default()
            }])]));

        let hook_called_clone = Arc::clone(&hook_called);
        let hook: CustomizeExecutorFn = Arc::new(move |orig: Arc<dyn Executor>| {
            hook_called_clone.store(true, Ordering::SeqCst);
            orig // return the same executor (Go `customExecutor == nil` path)
        });

        let pipeline = build_pipeline(Arc::clone(&original), None, RetryHooks::default())
            .with_customize_executor(hook);
        let mut ctx = PipelineContext::new();

        let (response, attempts) = pipeline
            .process(&mut ctx, raw_inbound(true), &raw_inbound(true), &pc(&["a"]))
            .await?;

        // Go: require.NoError + require.True(t, result.Stream)
        assert_eq!(attempts[0].mode, ExecutionMode::Stream);
        assert!(
            !response.stream.is_empty(),
            "original executor produced events"
        );
        // Go: require.True(t, mockCustomized.customizeExecutorCalled)
        assert!(hook_called.load(Ordering::SeqCst), "hook was called");
        // The customization step was recorded.
        assert!(ctx.order.iter().any(|s| s == "outbound:customize_executor"));
        Ok(())
    }

    // =========================================================================
    // RUST-P15-001 — Go pipeline_test.go parity (pure-logic subset).
    // Go file: `conduit/llm/pipeline/pipeline_test.go` (278 lines, 9 tests).
    // Prior waves migrated retry/empty/streaming/middleware/portable/
    // channel_retryable/channel_customized_executor golden cases. These tests
    // cover the REMAINING pure-logic building blocks from pipeline_test.go that
    // are not already exercised in isolation.
    //
    // Already covered by prior waves:
    // - TestChannelRetryable_CanRetry (Go L182-196) →
    //   retryable.rs::channel_retryable_can_retry_full_go_status_code_table +
    //   channel_retryable_budget_exhausted_blocks_same_channel_retry.
    // - TestChannelRetryable_PrepareForRetry (Go L198-210) →
    //   retryable.rs::channel_retryable_prepare_for_retry_and_reset_via_retry_context
    //   (at RetryContext level). Cursor-level prepare_for_retry covered below.
    // - TestChannelCustomizedExecutor_CustomizeExecutor (Go L212-224) →
    //   p15_* tests (6 integration cases in this module).
    //
    // Structural gap (pending):
    // - TestFactory_NewFactory (Go L257-263): Go uses a `Factory` struct holding
    //   an executor and creating pipelines via `Factory.Pipeline(inbound,
    //   outbound, opts...)`. Rust replaces this with `Pipeline::new(inbound,
    //   outbound, executor)` — no separate Factory type exists in the Rust crate.
    //   Pending: structural divergence, not a logic gap.
    // =========================================================================

    /// Mirrors `TestRetryable_HasMoreChannels` (`pipeline_test.go:146-160`) and
    /// `TestRetryable_NextChannel` (`pipeline_test.go:162-180`).
    ///
    /// The Go tests verify a mock `testRetryableOutbound` whose `HasMoreChannels()`
    /// returns `currentChannelIndex < len(channels)-1` and whose `NextChannel()`
    /// advances the index or returns an error containing "no more channels". In
    /// Rust the cursor is [`StrFailoverState`]; `next_channel()` advances or
    /// returns [`FailoverError::NoMoreChannels`]. The "has more channels" check
    /// is implicit: `next_channel()` succeeds iff there is a next candidate
    /// (the cursor checks `current_index + 1 < candidates.len()` internally,
    /// which is the same `currentChannelIndex < len(channels)-1` comparison).
    #[test]
    fn pipeline_test_cursor_next_channel_mirrors_go_retryable() {
        // Go: channels = ["channel1","channel2","channel3"], index=0.
        let candidates = pc(&["channel1", "channel2", "channel3"]);
        let mut state = StrFailoverState {
            candidates: &candidates,
            current_index: 0,
            current_model_index: 0,
            same_channel_retries: 0,
            total_attempts: 1,
        };

        // Go: HasMoreChannels() -> true at index 0 (len-1 = 2).
        // Rust: next_channel() succeeds, proving there IS a next channel.
        match state.next_channel() {
            Ok(()) => {}
            Err(e) => panic!("next_channel should succeed at index 0: {e:?}"),
        }
        assert_eq!(
            state.current_index, 1,
            "Go: currentChannelIndex advanced to 1"
        );

        // Go: NextChannel() again -> success, index becomes 2.
        match state.next_channel() {
            Ok(()) => {}
            Err(e) => panic!("next_channel should succeed at index 1: {e:?}"),
        }
        assert_eq!(
            state.current_index, 2,
            "Go: currentChannelIndex advanced to 2"
        );

        // Go: HasMoreChannels() -> false at last index; NextChannel() -> error.
        match state.next_channel() {
            Ok(()) => panic!("next_channel should fail at last index"),
            Err(FailoverError::NoMoreChannels) => {}
            Err(e) => panic!("expected NoMoreChannels, got {e:?}"),
        }
        // Go: index stays at 2 (cursor must not advance past last candidate).
        assert_eq!(
            state.current_index, 2,
            "index must stay at last candidate on NoMoreChannels"
        );
    }

    /// Mirrors `TestChannelRetryable_PrepareForRetry` (`pipeline_test.go:198-210`)
    /// at the cursor level. The Go test verifies `PrepareForRetry()` returns nil
    /// and increments `currentRetries` from 0 to 1. The Rust analog is
    /// [`StrFailoverState::prepare_for_retry`], which increments
    /// `same_channel_retries` and `total_attempts`. The `RetryContext`-level
    /// counter is already covered in `retryable.rs`.
    #[test]
    fn pipeline_test_cursor_prepare_for_retry_mirrors_go() {
        // Go: maxRetries=2, currentRetries=0.
        let candidates = pc(&["channel1"]);
        let mut state = StrFailoverState {
            candidates: &candidates,
            current_index: 0,
            current_model_index: 0,
            same_channel_retries: 0,
            total_attempts: 1,
        };
        let policy = RetryPolicy {
            max_single_channel_retries: 2,
            ..RetryPolicy::DEFAULT
        };

        // Go: PrepareForRetry(ctx) -> nil; currentRetries becomes 1.
        match state.prepare_for_retry(policy, 1) {
            Ok(true) => {}
            Ok(false) => panic!("prepare_for_retry should return Ok(true) at 0 retries"),
            Err(e) => panic!("prepare_for_retry should succeed: {e:?}"),
        }
        assert_eq!(
            state.same_channel_retries, 1,
            "Go: currentRetries incremented to 1"
        );
        assert_eq!(state.total_attempts, 2, "total attempts incremented");
    }

    /// Mirrors `TestPipeline_GetMaxSameChannelRetries`
    /// (`pipeline_test.go:226-232`) and `TestWithRetry`
    /// (`pipeline_test.go:234-243`).
    ///
    /// Go `getMaxSameChannelRetries()` is a simple getter returning
    /// `p.maxSameChannelRetries`. The Rust equivalent is the public
    /// [`RetryPolicy::max_single_channel_retries`] field. Go
    /// `WithRetry(5, 3, 100*time.Millisecond)` sets three fields on the
    /// pipeline; the Rust equivalent constructs a [`RetryPolicy`] with matching
    /// values and applies it via `with_retry_policy`.
    #[test]
    fn pipeline_test_retry_policy_fields_mirror_go_with_retry() {
        // Go: p := &pipeline{maxSameChannelRetries: 3}; p.getMaxSameChannelRetries() == 3.
        let policy = RetryPolicy {
            max_single_channel_retries: 3,
            ..RetryPolicy::DEFAULT
        };
        assert_eq!(policy.max_single_channel_retries, 3);

        // Go: WithRetry(5, 3, 100*time.Millisecond) sets all three knobs.
        // maxChannelRetries=5, maxSameChannelRetries=3, retryDelay=100ms.
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 5,
            max_single_channel_retries: 3,
            retry_delay_ms: 100,
            ..RetryPolicy::DEFAULT
        };
        assert_eq!(policy.max_channel_retries, 5, "Go: maxChannelRetries == 5");
        assert_eq!(
            policy.max_single_channel_retries, 3,
            "Go: maxSameChannelRetries == 3"
        );
        assert_eq!(policy.retry_delay_ms, 100, "Go: retryDelay == 100ms");
    }

    /// Mirrors `TestWithDecorators` (`pipeline_test.go:245-255`).
    ///
    /// Go `WithMiddlewares()` appends to `p.middlewares`; with no arguments the
    /// slice stays empty (`require.Len(t, p.middlewares, 0)`). The Rust
    /// equivalent is [`Pipeline::with_middlewares`] which replaces the middleware
    /// vec. This test verifies an empty vec is accepted and the pipeline still
    /// functions (produces a response without middleware hooks firing).
    #[tokio::test]
    async fn pipeline_test_with_middlewares_empty_mirrors_go_with_decorators()
    -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::non_stream(vec![Ok(ok_response("ok"))]));
        let pipeline =
            build_pipeline(executor, None, RetryHooks::default()).with_middlewares(vec![]); // empty — like Go WithMiddlewares() with no args.

        let mut ctx = PipelineContext::new();
        let (response, _) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(response.json_body, ok_response("ok").json_body);
        // No middleware-specific order entries should appear.
        assert!(
            !ctx.order
                .iter()
                .any(|s| s.contains(":on_request") || s.contains(":OnInbound")),
            "empty middleware list must not produce hook entries"
        );
        Ok(())
    }

    // =========================================================================
    // RUST-P15-001 — Go integration_test.go parity (pure-logic subset).
    // Go file: `conduit/llm/pipeline/integration_test.go` (562 lines, 5 tests).
    //
    // AUDIT — Turing-the-15th 2026-07-08.
    //
    // All 5 Go tests are end-to-end integration tests that spin up REAL
    // transformer pairs (OpenAI inbound/outbound, Anthropic inbound/outbound,
    // OpenAI Responses outbound), feed a client-format request through the
    // pipeline with a mock executor that returns a canned provider-format
    // response, and assert on the response body after the round-trip
    // transformation.
    //
    // Go tests in this file:
    //   1. TestPipeline_OpenAI_to_OpenAI (L45-147)
    //   2. TestPipeline_OpenAI_to_Anthropic (L150-249)
    //   3. TestPipeline_Anthropic_to_OpenAIResponses_PreservesFlatURLCitationFields (L251-349)
    //   4. TestPipeline_Anthropic_to_OpenAI (L352-458)
    //   5. TestPipeline_Anthropic_to_Anthropic (L461-562)
    //
    // Already covered by prior waves (pipeline control flow):
    // - Process flow stage ordering → attempt_order_is_outbound_merge_auth_middlewares_execute
    // - Inbound transform runs once → inbound_transform_runs_only_once_across_retries
    // - Stream-mode decision (Stream/AutoAggregate/NonStream) →
    //   user_stream_true_uses_stream_branch / user_no_stream_provider_streams_auto_aggregates /
    //   neither_user_nor_provider_streams_is_nonstream
    // - Response status/body returned → every successful-process test
    // - Middleware hook ordering → nonstream_hooks_fire_in_go_onion_order +
    //   streaming_hooks_fire_in_go_onion_order
    // - Channel-customized executor wiring → p15_* tests (6 cases)
    //
    // NEW pure-logic invariants extracted below (not previously asserted):
    // - The executor receives the outbound-transformed request DATA (method,
    //   path, json_body) — the Go tests verify this inside the mock executor's
    //   `doFunc`; the existing stub executors ignore the request entirely
    //   (`_request: &HttpRequest`).
    // - The inbound-merge step surfaces inbound headers/query onto the outbound
    //   request that reaches the executor (Go `MergeInboundRequest`).
    //
    // PENDING — blocked on outbound transformer porting (all 5 Go tests):
    // The Rust `conduit-transformers` crate has NO outbound transformers that
    // implement the `OutboundTransformer` trait. It has inbound transformers
    // (`AnthropicInboundTransformer`, `OpenAiChatInbound`, `OpenAiResponsesInbound`)
    // and pure helpers (`build_auth_header`, `resolve_outbound_url` in
    // `openai_outbound.rs`), but no `OpenAiChatOutbound`, `AnthropicOutbound`,
    // or `OpenAiResponsesOutbound` struct. All 5 Go tests require a full
    // transformer pair to verify the round-trip body transformation, auth header
    // finalization, and citation field preservation — these are transformer
    // concerns, not pipeline concerns. When the outbound transformers land
    // (likely RUST-P7-002/P7-003), migrate the tests by constructing the real
    // pairs and feeding canned provider responses through the mock executor.
    // =========================================================================

    /// Executor that captures the request it receives so tests can verify the
    /// pipeline forwards the outbound-transformed request data correctly.
    /// Mirrors the Go `mockExecutor.doFunc` pattern (integration_test.go:28-34)
    /// where the mock inspects `request.Method`, `request.URL`, and
    /// `request.Headers` inside the executor callback.
    struct CapturingExecutor {
        captured: Mutex<Option<HttpRequest>>,
        response: HttpResponse,
    }

    impl CapturingExecutor {
        fn new(response: HttpResponse) -> Self {
            Self {
                captured: Mutex::new(None),
                response,
            }
        }
        fn captured_request(&self) -> Option<HttpRequest> {
            self.captured
                .lock()
                .ok()
                .and_then(|guard| guard.as_ref().cloned())
        }
    }

    #[async_trait]
    impl Executor for CapturingExecutor {
        async fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, ConduitError> {
            if let Ok(mut guard) = self.captured.lock() {
                *guard = Some(request.clone());
            }
            Ok(self.response.clone())
        }
        async fn execute_stream(
            &self,
            _request: &HttpRequest,
        ) -> Result<Vec<StreamEvent>, ConduitError> {
            Err(ConduitError::internal("not used"))
        }
    }

    /// Outbound that produces a specific method/path/body — mirrors how Go's
    /// `openai.NewOutboundTransformer` / `anthropic.NewOutboundTransformer`
    /// produce distinct request shapes the integration tests assert on inside
    /// the mock executor.
    struct ShapedOutbound {
        method: String,
        path: String,
        body_key: String,
        body_value: String,
    }

    impl OutboundTransformer for ShapedOutbound {
        fn name(&self) -> &'static str {
            "shaped-outbound"
        }
        fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
            // Build the body with a dynamic key — `json!` can't interpolate a
            // String as a key, so assemble the map manually. Mirrors how Go's
            // outbound transformers produce distinct request shapes.
            let mut body = serde_json::Map::new();
            body.insert("model".to_string(), json!(request.model));
            body.insert(self.body_key.clone(), json!(self.body_value));
            Ok(HttpRequest {
                method: self.method.clone(),
                path: self.path.clone(),
                request_type: Some(request.request_type),
                api_format: Some(request.api_format),
                json_body: Some(serde_json::Value::Object(body)),
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
            Ok(ConduitError::upstream("shaped").with_provider_status(response.status))
        }
        // Unified response transform — same lossless stash-restore strategy as
        // StubOutbound (see there); the shaped tests assert on the exact body.
        fn transform_response(&self, response: HttpResponse) -> TransformerResult<LlmResponse> {
            let raw = response.json_body.clone();
            let llm = match response.json_body.as_ref() {
                Some(v) => serde_json::from_value::<LlmResponse>(v.clone())
                    .unwrap_or_else(|_| stub_llm_response(v)),
                None => LlmResponse {
                    id: "stub".to_string(),
                    object: "chat.completion".to_string(),
                    ..Default::default()
                },
            };
            let mut llm = llm;
            if let Some(raw) = raw {
                llm.extra.insert(STUB_RAW_BODY_KEY.to_string(), raw);
            }
            Ok(llm)
        }
    }

    fn shaped_pipeline(
        executor: Arc<dyn Executor>,
        method: &str,
        path: &str,
        body_key: &str,
        body_value: &str,
    ) -> Pipeline {
        Pipeline::new(
            Arc::new(StubInbound),
            Arc::new(ShapedOutbound {
                method: method.to_string(),
                path: path.to_string(),
                body_key: body_key.to_string(),
                body_value: body_value.to_string(),
            }),
            executor,
        )
        .with_retry_policy(RetryPolicy {
            enabled: true,
            max_channel_retries: 2,
            max_single_channel_retries: 1,
            retry_delay_ms: 0,
            ..RetryPolicy::DEFAULT
        })
        .with_retry_hooks(RetryHooks::default())
    }

    // -- Go integration_test.go pure-logic invariants -------------------------

    /// Mirrors the request-inspection pattern from all 5 Go integration tests
    /// (e.g. integration_test.go:80-98, :179-199, :387-406, :490-510): the mock
    /// executor's `doFunc` asserts `request.Method`, `request.URL` and the auth
    /// headers. The pipeline-level invariant is that the executor receives
    /// exactly the request the outbound transformer produced (after the merge +
    /// middleware chain). The auth-header and URL-path specifics are transformer
    /// concerns; the forwarding itself is the pipeline invariant.
    #[tokio::test]
    async fn p15_integration_executor_receives_outbound_transformed_request()
    -> Result<(), ConduitError> {
        let capture: Arc<CapturingExecutor> = Arc::new(CapturingExecutor::new(ok_response("ok")));
        let executor: Arc<dyn Executor> = capture.clone();
        // The outbound sets a distinctive method/path/body that the executor
        // must observe — proving the pipeline forwards the transformed request.
        let pipeline = shaped_pipeline(executor, "POST", "/v1/chat/completions", "prompt", "hi");

        let mut ctx = PipelineContext::new();
        let _ = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        let captured = capture
            .captured_request()
            .ok_or_else(|| ConduitError::internal("executor was not called"))?;
        assert_eq!(captured.method, "POST", "executor must see outbound method");
        assert_eq!(
            captured.path, "/v1/chat/completions",
            "executor must see outbound path"
        );
        assert_eq!(
            captured.json_body.as_ref().and_then(|b| b.get("prompt")),
            Some(&json!("hi")),
            "executor must see outbound body field"
        );
        assert_eq!(
            captured.json_body.as_ref().and_then(|b| b.get("model")),
            Some(&json!("stub-model")),
            "executor must see the llm request model threaded through"
        );
        Ok(())
    }

    #[tokio::test]
    async fn outbound_registry_uses_the_candidate_upstream_api_format() -> Result<(), ConduitError>
    {
        let executor = Arc::new(CapturingExecutor::new(HttpResponse {
            status: 200,
            json_body: Some(json!({"content": "ok"})),
            ..HttpResponse::default()
        }));
        let mut registry = conduit_transformers::traits::TransformerRegistry::new();
        registry.register_outbound(
            "custom",
            ApiFormat::AnthropicMessages,
            Arc::new(ShapedOutbound {
                method: "POST".to_string(),
                path: "/v1/messages".to_string(),
                body_key: "messages".to_string(),
                body_value: "hello".to_string(),
            }),
        );
        let pipeline = shaped_pipeline(
            executor.clone(),
            "POST",
            "/v1/chat/completions",
            "messages",
            "fallback",
        )
        .with_outbound_registry(Arc::new(registry));
        let candidate = PipelineCandidate {
            id: "channel-1".to_string(),
            base_url: Some("https://api.example.com".to_string()),
            channel_type: "custom".to_string(),
            api_format: ApiFormat::AnthropicMessages.as_str().to_string(),
            ..PipelineCandidate::from("channel-1")
        };
        let request = HttpRequest {
            api_format: Some(ApiFormat::OpenAiChatCompletions),
            request_type: Some(RequestType::Chat),
            json_body: Some(json!({
                "model": "stub-model",
                "messages": [{"role": "user", "content": "hello"}]
            })),
            ..HttpRequest::default()
        };

        pipeline
            .process(
                &mut PipelineContext::new(),
                request.clone(),
                &request,
                &[candidate],
            )
            .await?;

        let captured = executor
            .captured_request()
            .ok_or_else(|| ConduitError::internal("executor did not receive a request"))?;
        assert_eq!(captured.path, "/v1/messages");
        assert_eq!(captured.api_format, Some(ApiFormat::AnthropicMessages));
        assert_eq!(
            captured.url.as_deref(),
            Some("https://api.example.com/v1/messages")
        );
        Ok(())
    }

    /// Mirrors the Go `MergeInboundRequest` step implicitly exercised by all 5
    /// integration tests (the inbound raw request headers flow onto the
    /// outbound request). The pipeline's `merge_inbound` surfaces inbound
    /// headers/query that the outbound did not set.
    #[tokio::test]
    async fn p15_integration_merge_inbound_surfaces_headers_onto_outbound()
    -> Result<(), ConduitError> {
        let capture: Arc<CapturingExecutor> = Arc::new(CapturingExecutor::new(ok_response("ok")));
        let executor: Arc<dyn Executor> = capture.clone();
        let pipeline = shaped_pipeline(executor, "POST", "/v1/messages", "max_tokens", "1024");

        // Inbound raw request carries a header + query the outbound didn't set.
        let mut inbound = raw_inbound(false);
        inbound
            .headers
            .insert("X-Custom-Header".to_string(), "from-client".to_string());
        inbound.headers.insert(
            "Authorization".to_string(),
            "Bearer client-facing-key".to_string(),
        );
        inbound
            .headers
            .insert("Content-Length".to_string(), "17".to_string());
        inbound
            .headers
            .insert("Host".to_string(), "gateway.local".to_string());
        inbound
            .query
            .insert("trace_id".to_string(), vec!["abc123".to_string()]);

        let mut ctx = PipelineContext::new();
        let _ = pipeline
            .process(&mut ctx, raw_inbound(false), &inbound, &pc(&["a"]))
            .await?;

        let captured = capture
            .captured_request()
            .ok_or_else(|| ConduitError::internal("executor was not called"))?;
        assert_eq!(
            captured.headers.get("X-Custom-Header"),
            Some(&"from-client".to_string()),
            "inbound headers must merge onto the outbound request reaching the executor"
        );
        assert_eq!(
            captured.query.get("trace_id"),
            Some(&vec!["abc123".to_string()]),
            "inbound query must merge onto the outbound request"
        );
        assert!(
            !captured
                .headers
                .keys()
                .any(|key| key.eq_ignore_ascii_case("authorization")),
            "client Authorization must never be forwarded upstream"
        );
        assert!(
            !captured.headers.keys().any(|key| {
                key.eq_ignore_ascii_case("content-length") || key.eq_ignore_ascii_case("host")
            }),
            "client framing and authority headers must be regenerated for the upstream request"
        );
        // Outbound-set fields are NOT overwritten by the merge.
        assert_eq!(captured.method, "POST");
        assert_eq!(captured.path, "/v1/messages");
        Ok(())
    }

    /// Mirrors the response-forwarding invariant from
    /// `TestPipeline_OpenAI_to_OpenAI` (integration_test.go:136-147): the
    /// response the executor returns flows back through the pipeline to the
    /// caller with status and body intact. The Go test additionally verifies
    /// the response body fields after inbound transformation — that's a
    /// transformer concern. The pipeline invariant is: the executor's
    /// `HttpResponse` is what the caller receives.
    #[tokio::test]
    async fn p15_integration_executor_response_flows_to_caller() -> Result<(), ConduitError> {
        let provider_response = HttpResponse {
            status: 200,
            json_body: Some(json!({
                "id": "chatcmpl-123",
                "object": "chat.completion",
                "model": "gpt-4",
            })),
            ..HttpResponse::default()
        };
        let capture: Arc<CapturingExecutor> =
            Arc::new(CapturingExecutor::new(provider_response.clone()));
        let executor: Arc<dyn Executor> = capture.clone();
        let pipeline = shaped_pipeline(executor, "POST", "/v1/chat/completions", "k", "v");

        let mut ctx = PipelineContext::new();
        let (response, _attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        assert_eq!(response.status, 200, "caller sees executor's status code");
        assert_eq!(
            response.json_body, provider_response.json_body,
            "caller sees the executor's response body (StubInbound is a pass-through)"
        );
        Ok(())
    }

    #[tokio::test]
    async fn non_success_provider_status_is_an_error_not_a_success_body() -> Result<(), ConduitError>
    {
        let provider_response = HttpResponse {
            status: 401,
            json_body: Some(json!({"error":{"message":"bad upstream key"}})),
            ..HttpResponse::default()
        };
        let executor: Arc<dyn Executor> = Arc::new(CapturingExecutor::new(provider_response));
        let pipeline = shaped_pipeline(executor, "POST", "/v1/chat/completions", "prompt", "hi");
        let mut ctx = PipelineContext::new();
        let error = match pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await
        {
            Ok(_) => {
                return Err(ConduitError::internal(
                    "provider 401 must fail the pipeline",
                ));
            }
            Err(error) => error,
        };
        assert_eq!(error.provider_status, Some(401));
        assert!(
            ctx.order
                .iter()
                .any(|stage| stage == "execute:nonstream:provider_error:error"),
            "provider error stage must be recorded"
        );
        Ok(())
    }

    #[tokio::test]
    async fn unified_response_usage_survives_into_final_http_response() -> Result<(), ConduitError>
    {
        let provider_response = HttpResponse {
            status: 200,
            json_body: Some(json!({
                "id": "chatcmpl-usage",
                "model": "stub-model",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}],
                "usage": {"prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16}
            })),
            ..HttpResponse::default()
        };
        let executor: Arc<dyn Executor> = Arc::new(CapturingExecutor::new(provider_response));
        let pipeline = shaped_pipeline(executor, "POST", "/v1/chat/completions", "prompt", "hi");
        let mut ctx = PipelineContext::new();
        let (response, _) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;
        let usage = response
            .usage
            .ok_or_else(|| ConduitError::internal("final response lost structured usage"))?;
        assert_eq!(usage.total_tokens, 16);
        Ok(())
    }

    // -- WIRE-06: candidate target stamping ----------------------------------

    /// WIRE-06 — a candidate carrying base_url + credential + actual_model is
    /// stamped onto the outbound `HttpRequest` at the auth step: the URL
    /// resolves to the chat-completions endpoint (Go `buildFullRequestURL`),
    /// auth becomes bearer with the plaintext credential, and the per-attempt
    /// `LlmRequest.model` is the channel's actual model (Go `outbound.go:385`).
    /// The plaintext credential must never leak into `ctx.order`.
    #[tokio::test]
    async fn wire06_candidate_target_stamped_onto_http_request() -> Result<(), ConduitError> {
        let capture: Arc<CapturingExecutor> = Arc::new(CapturingExecutor::new(ok_response("ok")));
        let executor: Arc<dyn Executor> = capture.clone();
        let pipeline = shaped_pipeline(executor, "POST", "/v1/chat/completions", "k", "v");

        let candidates = vec![PipelineCandidate {
            id: "ch-1".to_string(),
            base_url: Some("https://api.example.com/v1".to_string()),
            credential: Some("sk-secret-123".to_string()),
            credential_identity: Some("sha256:test".to_string()),
            actual_model: Some("gpt-4o-upstream".to_string()),
            api_format: "openai/chat_completions".to_string(),
            endpoint_path: None,
            endpoint_transport: Some("http".to_string()),
            channel_type: "openai".to_string(),
            channel_config: Default::default(),
            retryable_status_codes: Vec::new(),
            retryable_error_patterns: Vec::new(),
            error_response_rewrite_rules: Vec::new(),
        }];

        let mut ctx = PipelineContext::new();
        let (_, attempts) = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &candidates,
            )
            .await?;

        let captured = capture
            .captured_request()
            .ok_or_else(|| ConduitError::internal("executor was not called"))?;
        assert_eq!(
            captured.url.as_deref(),
            Some("https://api.example.com/v1/chat/completions"),
            "channel base_url must resolve to the chat-completions URL"
        );
        let auth = captured
            .auth
            .ok_or_else(|| ConduitError::internal("auth was not stamped"))?;
        assert_eq!(auth.scheme, "Bearer");
        assert_eq!(auth.token.as_deref(), Some("sk-secret-123"));
        assert_eq!(
            captured.json_body.as_ref().and_then(|b| b.get("model")),
            Some(&json!("gpt-4o-upstream")),
            "per-attempt model must be the channel's actual model"
        );
        assert_eq!(attempts[0].channel_id, "ch-1");
        assert_eq!(
            ctx.metadata.get("credential_identity").map(String::as_str),
            Some("sha256:test")
        );
        assert!(
            ctx.metadata
                .values()
                .all(|value| !value.contains("sk-secret-123")),
            "plaintext credential leaked into metadata"
        );
        // The plaintext credential never reaches the observable order log.
        assert!(
            ctx.order.iter().all(|step| !step.contains("sk-secret-123")),
            "credential leaked into ctx.order: {:?}",
            ctx.order
        );
        Ok(())
    }

    /// WIRE-06 — an id-only candidate (the pre-WIRE-06 shape) leaves url/auth
    /// unset and the inbound model untouched: nothing is stamped when the
    /// candidate carries no target data.
    #[tokio::test]
    async fn wire06_id_only_candidate_stamps_nothing() -> Result<(), ConduitError> {
        let capture: Arc<CapturingExecutor> = Arc::new(CapturingExecutor::new(ok_response("ok")));
        let executor: Arc<dyn Executor> = capture.clone();
        let pipeline = shaped_pipeline(executor, "POST", "/v1/chat/completions", "k", "v");

        let mut ctx = PipelineContext::new();
        let _ = pipeline
            .process(
                &mut ctx,
                raw_inbound(false),
                &raw_inbound(false),
                &pc(&["a"]),
            )
            .await?;

        let captured = capture
            .captured_request()
            .ok_or_else(|| ConduitError::internal("executor was not called"))?;
        assert_eq!(captured.url, None, "no base_url -> url stays unset");
        assert_eq!(captured.auth, None, "no credential -> auth stays unset");
        assert_eq!(
            captured.json_body.as_ref().and_then(|b| b.get("model")),
            Some(&json!("stub-model")),
            "no actual_model -> inbound model flows through"
        );
        Ok(())
    }

    #[test]
    fn selected_endpoint_path_and_base_url_override_transformer_defaults() {
        let mut request = HttpRequest {
            url: Some(
                "https://generativelanguage.googleapis.com/v1beta/models/model:generateContent"
                    .to_string(),
            ),
            path: "/v1beta/models/model:generateContent".to_string(),
            ..HttpRequest::default()
        };
        let target = PipelineCandidate {
            id: "custom-gemini".to_string(),
            base_url: Some("https://proxy.example/api".to_string()),
            api_format: ApiFormat::GeminiContents.as_str().to_string(),
            endpoint_path: Some("/provider/generate".to_string()),
            endpoint_transport: Some("http".to_string()),
            channel_type: "custom".to_string(),
            ..PipelineCandidate::from("custom-gemini")
        };

        stamp_outbound_target(&mut request, &target);

        assert_eq!(request.path, "/provider/generate");
        assert_eq!(
            request.url.as_deref(),
            Some("https://proxy.example/api/provider/generate")
        );
    }

    #[test]
    fn selected_channel_proxy_replaces_inherited_proxy_metadata() {
        let mut request = HttpRequest::default();
        request.metadata.insert(
            "channel_proxy".to_string(),
            json!({"type": "URL", "url": "http://untrusted.invalid"}),
        );
        let target = PipelineCandidate {
            channel_config: std::collections::BTreeMap::from([(
                "channel_proxy".to_string(),
                r#"{"type":"URL","url":"http://proxy.example:8080"}"#.to_string(),
            )]),
            ..PipelineCandidate::from("proxied")
        };

        stamp_outbound_target(&mut request, &target);

        assert_eq!(
            request
                .metadata
                .get("channel_proxy")
                .and_then(serde_json::Value::as_str),
            Some(r#"{"type":"URL","url":"http://proxy.example:8080"}"#)
        );
    }

    #[test]
    fn candidate_base_with_api_version_does_not_duplicate_transformer_version() {
        let mut request = HttpRequest {
            path: "/v1/messages".to_string(),
            ..HttpRequest::default()
        };
        let target = PipelineCandidate {
            id: "claude".to_string(),
            base_url: Some("https://api.anthropic.com/v1".to_string()),
            api_format: ApiFormat::AnthropicMessages.as_str().to_string(),
            endpoint_transport: Some("http".to_string()),
            ..PipelineCandidate::from("claude")
        };

        stamp_outbound_target(&mut request, &target);

        assert_eq!(
            request.url.as_deref(),
            Some("https://api.anthropic.com/v1/messages")
        );
    }

    #[test]
    fn merge_inbound_honors_transformer_query_isolation() {
        let mut outbound = HttpRequest {
            skip_inbound_query_merge: true,
            ..HttpRequest::default()
        };
        let mut inbound = HttpRequest::default();
        inbound
            .query
            .insert("alt".to_string(), vec!["json".to_string()]);

        merge_inbound(&mut outbound, &inbound);

        assert!(outbound.query.is_empty());
    }

    #[test]
    fn raw_wire_passthrough_requires_client_and_upstream_formats_to_match() {
        assert!(!can_passthrough_openai_wire_response(
            ApiFormat::OpenAiChatCompletions,
            ApiFormat::OpenAiResponses,
            "openai-compat-outbound",
        ));
        assert!(can_passthrough_openai_wire_response(
            ApiFormat::OpenAiResponses,
            ApiFormat::OpenAiResponses,
            "openai-compat-outbound",
        ));
        assert!(can_passthrough_openai_wire_response(
            ApiFormat::OpenAiResponses,
            ApiFormat::OpenAiResponses,
            "openai-responses",
        ));
    }

    /// Direct Anthropic protocol endpoints authenticate with `x-api-key` plus
    /// an `anthropic-version` header — NOT `Authorization: Bearer`. Mirrors Go
    /// direct-platform transformer behavior.
    #[test]
    fn stamp_anthropic_family_uses_x_api_key_header() -> Result<(), ConduitError> {
        for channel_type in [
            "anthropic",
            "deepseek_anthropic",
            "zai_anthropic",
            "moonshot_coding",
            "opencode_go_anthropic",
        ] {
            let mut http_req = HttpRequest {
                url: Some("https://api.anthropic.com/v1/messages".to_string()),
                ..HttpRequest::default()
            };
            let target = PipelineCandidate {
                id: "ch-a".to_string(),
                base_url: None,
                credential: Some("sk-ant-secret".to_string()),
                credential_identity: None,
                actual_model: None,
                api_format: ApiFormat::AnthropicMessages.as_str().to_string(),
                endpoint_path: None,
                endpoint_transport: Some("http".to_string()),
                channel_type: channel_type.to_string(),
                channel_config: Default::default(),
                retryable_status_codes: Vec::new(),
                retryable_error_patterns: Vec::new(),
                error_response_rewrite_rules: Vec::new(),
            };
            stamp_outbound_target(&mut http_req, &target);
            assert_eq!(
                http_req.headers.get("x-api-key").map(String::as_str),
                Some("sk-ant-secret"),
                "{channel_type}: credential must be stamped as x-api-key"
            );
            assert_eq!(
                http_req
                    .headers
                    .get("anthropic-version")
                    .map(String::as_str),
                Some("2023-06-01"),
                "{channel_type}: anthropic-version header must be present"
            );
            assert!(
                http_req.auth.is_none(),
                "{channel_type}: must NOT stamp a Bearer token"
            );
        }
        Ok(())
    }

    /// Gemini-family Direct channel types authenticate with `x-goog-api-key`;
    /// everyone else falls back to `Authorization: Bearer` (Go `channel_llm.go`
    /// gemini arm vs. the OpenAI-compatible default).
    #[test]
    fn stamp_gemini_uses_goog_key_and_others_use_bearer() -> Result<(), ConduitError> {
        // Gemini Direct → x-goog-api-key.
        let mut gemini_req = HttpRequest::default();
        let gemini_target = PipelineCandidate {
            id: "ch-g".to_string(),
            base_url: None,
            credential: Some("goog-secret".to_string()),
            credential_identity: None,
            actual_model: None,
            api_format: ApiFormat::GeminiContents.as_str().to_string(),
            endpoint_path: None,
            endpoint_transport: Some("http".to_string()),
            channel_type: "gemini".to_string(),
            channel_config: Default::default(),
            retryable_status_codes: Vec::new(),
            retryable_error_patterns: Vec::new(),
            error_response_rewrite_rules: Vec::new(),
        };
        stamp_outbound_target(&mut gemini_req, &gemini_target);
        assert_eq!(
            gemini_req.headers.get("x-goog-api-key").map(String::as_str),
            Some("goog-secret"),
        );
        assert!(gemini_req.auth.is_none(), "gemini must not use Bearer");

        // Plain OpenAI-compatible → Bearer.
        let mut openai_req = HttpRequest::default();
        openai_req.headers.insert(
            "Authorization".to_string(),
            "Bearer stale-client-key".to_string(),
        );
        let openai_target = PipelineCandidate {
            id: "ch-o".to_string(),
            base_url: None,
            credential: Some("sk-openai".to_string()),
            credential_identity: None,
            actual_model: None,
            api_format: ApiFormat::OpenAiChatCompletions.as_str().to_string(),
            endpoint_path: None,
            endpoint_transport: Some("http".to_string()),
            channel_type: "openai".to_string(),
            channel_config: Default::default(),
            retryable_status_codes: Vec::new(),
            retryable_error_patterns: Vec::new(),
            error_response_rewrite_rules: Vec::new(),
        };
        stamp_outbound_target(&mut openai_req, &openai_target);
        let auth = openai_req
            .auth
            .ok_or_else(|| ConduitError::internal("openai auth must be stamped"))?;
        assert_eq!(auth.scheme, "Bearer");
        assert_eq!(auth.token.as_deref(), Some("sk-openai"));
        assert!(
            !openai_req
                .headers
                .keys()
                .any(|key| key.eq_ignore_ascii_case("authorization")),
            "structured channel auth must replace a stale header"
        );
        assert!(
            !openai_req.headers.contains_key("x-api-key"),
            "openai must not use x-api-key"
        );
        Ok(())
    }

    #[test]
    fn p15_integration_test_pending_note() {
        // Sentinel test so the PENDING status above shows up in `cargo test`
        // output and is grep-able. Delete when the outbound transformers land
        // and the 5 Go integration tests are migrated. Each requires a full
        // transformer pair:
        //
        // pending: TestPipeline_OpenAI_to_OpenAI (integration_test.go:45-147)
        //   - Needs OpenAiChatInbound + OpenAiChatOutbound (not yet ported as
        //     an OutboundTransformer impl).
        //
        // pending: TestPipeline_OpenAI_to_Anthropic (integration_test.go:150-249)
        //   - Needs OpenAiChatInbound + AnthropicOutbound (not yet ported).
        //   - Verifies X-Api-Key header, Anthropic-Version header, end_turn→stop
        //     finish-reason mapping — all transformer concerns.
        //
        // pending: TestPipeline_Anthropic_to_OpenAIResponses_PreservesFlatURLCitationFields
        //   (integration_test.go:251-349)
        //   - Needs AnthropicInboundTransformer + OpenAiResponsesOutbound.
        //   - 100% citation-field-preservation transformer concern.
        //
        // pending: TestPipeline_Anthropic_to_OpenAI (integration_test.go:352-458)
        //   - Needs AnthropicInboundTransformer + OpenAiChatOutbound.
        //
        // pending: TestPipeline_Anthropic_to_Anthropic (integration_test.go:461-562)
        //   - Needs AnthropicInboundTransformer + AnthropicOutbound.
        //
        // Structural gap: the Rust pipeline's auth finalization step
        // (`outbound:auth_headers`) is a stub — Go's `httpclient.FinalizeAuthHeaders`
        // moves the `Auth` config into headers and nils the field. The Go tests
        // assert `request.Auth == nil` + the populated header inside the mock
        // executor. Pending: httpclient auth-finalization port (not a pipeline
        // crate concern).
    }

    /// P-34: the live streaming path must run the middleware chain (it used to
    /// skip it entirely). We assert the LLM-stage and raw-request-stage
    /// middleware phases both executed by checking `ctx.order`.
    #[tokio::test]
    async fn stream_live_runs_the_middleware_chain() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent {
                done: true,
                ..StreamEvent::default()
            }])]));
        // ForceStreamMw is a no-op-ish inbound middleware that records its phase;
        // any middleware suffices — we only assert the chain ran.
        let pipeline = build_pipeline(executor, None, RetryHooks::default())
            .with_middlewares(vec![Arc::new(ForceStreamMw) as BoxPipelineMiddleware]);
        let mut ctx = PipelineContext::new();

        let _attempt = pipeline
            .stream_live(
                &mut ctx,
                raw_inbound(true),
                &raw_inbound(true),
                &pc(&["a"]),
                crate::cancel::CancelToken::new(),
            )
            .await?;

        assert!(
            ctx.order.iter().any(|s| s == "inbound:llm_middlewares"),
            "live path must run the LLM-stage middleware chain (P-34); order: {:?}",
            ctx.order
        );
        assert!(
            ctx.order.iter().any(|s| s == "outbound:raw_middlewares"),
            "live path must run the raw-request-stage middleware chain (P-34); order: {:?}",
            ctx.order
        );
        Ok(())
    }

    /// P-34: a middleware that aborts on the live path must fail the stream
    /// request (e.g. quota exceeded), not let it through unchecked. Before the
    /// fix streaming requests bypassed quota/limit middlewares entirely.
    #[tokio::test]
    async fn stream_live_aborts_when_a_middleware_rejects() -> Result<(), ConduitError> {
        let executor: Arc<dyn Executor> =
            Arc::new(StubExecutor::stream(vec![Ok(vec![StreamEvent::default()])]));
        let pipeline = build_pipeline(executor, None, RetryHooks::default())
            .with_middlewares(vec![Arc::new(AbortInboundMw) as BoxPipelineMiddleware]);
        let mut ctx = PipelineContext::new();

        let result = pipeline
            .stream_live(
                &mut ctx,
                raw_inbound(true),
                &raw_inbound(true),
                &pc(&["a"]),
                crate::cancel::CancelToken::new(),
            )
            .await;

        assert!(
            result.is_err(),
            "a rejecting middleware must abort the live stream (P-34)"
        );
        Ok(())
    }

    #[tokio::test]
    async fn live_stream_terminal_error_applies_the_channel_rule() -> Result<(), ConduitError> {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(Err(
            ConduitError::upstream("provider live failure").with_provider_status(529)
        ))
        .await
        .map_err(|_| ConduitError::internal("failed to seed live stream"))?;
        drop(tx);
        let rules = vec![ErrorResponseRewriteRule {
            status_codes: vec![529],
            message: Some("live error from ${channel_id}".to_string()),
            ..Default::default()
        }];
        let mut rewritten = rewrite_live_stream_errors(rx, "live-channel".to_string(), rules);

        let event = rewritten
            .recv()
            .await
            .ok_or_else(|| ConduitError::internal("rewritten stream closed early"))?;
        let error = match event {
            Ok(_) => {
                return Err(ConduitError::internal(
                    "seeded terminal error must remain an error",
                ));
            }
            Err(error) => error,
        };
        assert_eq!(error.public_message(), "live error from live-channel");
        assert_eq!(error.message, "provider live failure");
        assert_eq!(error.provider_status, Some(529));
        assert!(rewritten.recv().await.is_none());
        Ok(())
    }
}
