//! Pipeline middleware — the Go 9-hook `Middleware` interface (RUST-P8-001
//! S04/S07/S08).
//!
//! Ports `conduit/llm/pipeline/middleware.go` (`Middleware`, lines 16-64) and
//! the `apply*Middlewares` runners from `conduit/llm/pipeline/pipeline.go`
//! (lines 141-254). Execution concepts mirror the Go doc (middleware.go:11-15):
//!
//! - **Request**: one call to `Pipeline::process` (one client request).
//! - **Attempt**: one outbound execution; a request may span several attempts
//!   (same-channel retries / channel switches).
//!
//! Hook order/frequency table (Go source of truth):
//!
//! | Hook | Timing | Order | Go runner |
//! |------|--------|-------|-----------|
//! | `on_inbound_llm_request`  | once per Request, before attempts | forward | `applyBeforeRequestMiddlewares` (pipeline.go:141) |
//! | `on_outbound_raw_request` | once per Attempt (repeats on retry) | forward | `applyRawRequestMiddlewares` (pipeline.go:180) |
//! | `on_outbound_raw_error`   | once per failed Attempt | reverse | `applyRawErrorResponseMiddlewares` (pipeline.go:221) |
//! | `on_outbound_raw_response`| once per successful non-stream Attempt | reverse | `applyRawResponseMiddlewares` (pipeline.go:193) |
//! | `on_outbound_llm_response`| once per successful non-stream Attempt | reverse | `applyLlmResponseMiddlewares` (pipeline.go:228) |
//! | `on_outbound_raw_stream`  | once per successful streaming Attempt | reverse | `applyRawStreamMiddlewares` (pipeline.go:207) |
//! | `on_outbound_llm_stream`  | once per successful streaming Attempt | reverse | `applyLlmStreamMiddlewares` (pipeline.go:242) |
//! | `on_inbound_raw_response` | once per successful non-stream Request (final response) | **forward** | `applyInboundRawResponseMiddlewares` (pipeline.go:154) |
//! | `on_inbound_raw_stream`   | once per successful streaming Request (final stream) | **forward** | `applyInboundRawStreamMiddlewares` (pipeline.go:167) |
//!
//! The two inbound hooks' FORWARD direction is confirmed by the Go loop bodies
//! (`for _, dec := range p.middlewares` at pipeline.go:157 and :170 — not the
//! reverse `for i := len(p.middlewares) - 1` loops) and by the golden orders in
//! `TestMiddleware_NonStreaming_CallOrder` (`middleware_test.go:263-282`,
//! `M1→M2→M3` for `OnInboundRawResponse`) and
//! `TestMiddleware_Streaming_CallOrder` (`middleware_test.go:336-351`,
//! `M1→M2→M3` for `OnInboundRawStream`).
//!
//! ## S07 — one middleware, one cross-cutting concern (design rule)
//!
//! Every [`PipelineMiddleware`] implementation must own exactly ONE
//! cross-cutting concern and implement only the hooks that concern needs (all
//! hooks default to no-ops, mirroring Go's `simpleMiddleware` nil-handler
//! pass-through, middleware.go:125-195). Go models concerns the same way:
//! `cc/billing_header.go` only injects the billing header, `maxtoken/` only
//! clamps max tokens, `stream/usage.go` only merges usage, empty-response
//! detection is its own pipeline option. Do NOT bundle multiple concerns into
//! one middleware — register several instead; the runners keep per-hook
//! ordering deterministic.
//!
//! ## S08 — stream hooks are lazy wrappers
//!
//! The stream hooks receive an event iterator and must return a (possibly
//! wrapped) iterator WITHOUT consuming it: no `collect`, no draining the source
//! to compute usage up-front (unless the route is genuinely non-streaming, in
//! which case the pipeline aggregates — not the middleware). Statistics
//! middlewares accumulate per event inside their wrapper, exactly how Go wraps
//! `streams.Stream` ("The middleware can wrap the stream to process individual
//! chunks", middleware.go:56). The guard test
//! `stream_middlewares_do_not_pre_collect_events` pins this. The crate has no
//! async `BoxStream` abstraction yet (the executor is an eager
//! `Vec<StreamEvent>` stub) — the [`BoxEventStream`]/[`BoxLlmStream`] iterator
//! form carries the same laziness contract and swaps to an async stream type
//! mechanically when the real streaming executor lands.

use std::collections::BTreeMap;
use std::sync::Arc;

use conduit_core::ConduitError;
use conduit_llm::{HttpRequest, HttpResponse, LlmRequest, LlmResponse, StreamEvent};

use crate::cancel::CancelToken;

pub type PipelineResult<T> = Result<T, ConduitError>;

/// Type-erased raw event stream (Go `streams.Stream[*httpclient.StreamEvent]`).
///
/// Iterator-based for now — see the module docs (S08): laziness is the
/// contract, the concrete stream type is not.
pub type BoxEventStream = Box<dyn Iterator<Item = StreamEvent> + Send>;

/// Type-erased unified LLM response stream (Go `streams.Stream[*llm.Response]`).
pub type BoxLlmStream = Box<dyn Iterator<Item = LlmResponse> + Send>;

/// Owned, type-erased middleware handle (Go stores `[]Middleware`).
pub type BoxPipelineMiddleware = Arc<dyn PipelineMiddleware>;

/// The pipeline middleware interface. Port of Go `pipeline.Middleware`
/// (`conduit/llm/pipeline/middleware.go:16-64`) — nine hooks, each defaulting
/// to a no-op pass-through so implementations only override what their single
/// concern needs (S07; Go's `simpleMiddleware`/`DummyMiddleware` provide the
/// same "unimplemented hook = identity" behavior).
///
/// See the module docs for the full order/frequency table. Every hook receives
/// `&mut PipelineContext` for wrap-time observability (`ctx.record_order`);
/// stream wrappers that record per event must capture their own shared state
/// (`Arc`) because the returned iterator outlives the context borrow.
pub trait PipelineMiddleware: Send + Sync {
    /// Stable middleware name (Go `Middleware.Name`, middleware.go:18).
    fn name(&self) -> &str;

    /// Go `OnInboundLlmRequest` (middleware.go:20-23): after inbound
    /// transformation (provider → unified), before any outbound logic.
    /// Once per Request, FORWARD order.
    fn on_inbound_llm_request(
        &self,
        ctx: &mut PipelineContext,
        request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        let _ = ctx;
        Ok(request)
    }

    /// Go `OnInboundRawResponse` (middleware.go:25-28): after the final unified
    /// response is transformed back to provider format. Once per successful
    /// non-streaming Request, FORWARD order (pipeline.go:157).
    fn on_inbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        let _ = ctx;
        Ok(response)
    }

    /// Go `OnInboundRawStream` (middleware.go:30-33): after the final unified
    /// stream is transformed back to provider format. Once per successful
    /// streaming Request, FORWARD order (pipeline.go:170). Lazy wrapper (S08).
    fn on_inbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        let _ = ctx;
        Ok(stream)
    }

    /// Go `OnOutboundRawRequest` (middleware.go:35-38): after outbound
    /// transformation (unified → provider), before sending. Once per Attempt
    /// (repeats on retries/switches), FORWARD order.
    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        let _ = ctx;
        Ok(request)
    }

    /// Go `OnOutboundRawError` (middleware.go:40-43): the provider request
    /// failed (network error / status >= 400 / middleware failure). Once per
    /// failed Attempt, REVERSE order. No return value — it cannot veto or
    /// replace the error (Go signature returns nothing), and the runner never
    /// short-circuits: every middleware sees the error, even ones whose
    /// request-phase hook never ran (Go
    /// `TestMiddleware_RawRequest_Error_CleanupMiddlewares`,
    /// middleware_test.go:756-790).
    fn on_outbound_raw_error(&self, ctx: &mut PipelineContext, error: &ConduitError) {
        let _ = (ctx, error);
    }

    /// Go `OnOutboundRawResponse` (middleware.go:45-48): a successful provider
    /// response was received. Once per successful non-streaming Attempt,
    /// REVERSE order.
    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        let _ = ctx;
        Ok(response)
    }

    /// Go `OnOutboundLlmResponse` (middleware.go:50-53): after the provider
    /// response is transformed to unified format. Once per successful
    /// non-streaming Attempt, REVERSE order.
    fn on_outbound_llm_response(
        &self,
        ctx: &mut PipelineContext,
        response: LlmResponse,
    ) -> PipelineResult<LlmResponse> {
        let _ = ctx;
        Ok(response)
    }

    /// Go `OnOutboundRawStream` (middleware.go:55-58): a successful provider
    /// stream was established. Once per successful streaming Attempt, REVERSE
    /// order. Lazy wrapper (S08): "The middleware can wrap the stream to
    /// process individual chunks."
    fn on_outbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        let _ = ctx;
        Ok(stream)
    }

    /// Called when a live asynchronous provider stream finishes or is dropped
    /// after client disconnect. Unlike `on_outbound_raw_stream`, this hook is
    /// about the lifetime of the established stream and is suitable for
    /// releasing attempt-scoped resources such as concurrency permits.
    fn on_outbound_live_stream_close(&self, ctx: &mut PipelineContext) {
        let _ = ctx;
    }

    /// Go `OnOutboundLlmStream` (middleware.go:60-63): after the provider
    /// stream is transformed to unified format. Once per successful streaming
    /// Attempt, REVERSE order. Lazy wrapper (S08).
    fn on_outbound_llm_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxLlmStream,
    ) -> PipelineResult<BoxLlmStream> {
        let _ = ctx;
        Ok(stream)
    }
}

// ---------------------------------------------------------------------------
// Middleware runners — ports of the Go `apply*Middlewares` methods
// (`conduit/llm/pipeline/pipeline.go:141-254`). Free functions over the
// middleware slice so both the pipeline and tests drive them directly. None of
// them record `ctx.order` entries themselves — middlewares own their
// observability; the pipeline records stage markers at the call sites.
// ---------------------------------------------------------------------------

/// Go `applyBeforeRequestMiddlewares` (pipeline.go:141-152) —
/// `OnInboundLlmRequest`, FORWARD, once per Request (called from `Process`
/// before the attempt loop, pipeline.go:267). First error aborts the request.
pub fn apply_before_request_middlewares(
    middlewares: &[BoxPipelineMiddleware],
    ctx: &mut PipelineContext,
    mut request: LlmRequest,
) -> PipelineResult<LlmRequest> {
    for mw in middlewares {
        // Forward — Go `for _, dec := range p.middlewares` (pipeline.go:144).
        request = mw.on_inbound_llm_request(ctx, request)?;
    }
    Ok(request)
}

/// Go `applyInboundRawResponseMiddlewares` (pipeline.go:154-165) —
/// `OnInboundRawResponse`, FORWARD (loop at pipeline.go:157), once per
/// successful non-streaming Request. Called on the final response
/// (non_streaming.go:71) and on the auto-aggregated response
/// (non_streaming.go:130).
pub fn apply_inbound_raw_response_middlewares(
    middlewares: &[BoxPipelineMiddleware],
    ctx: &mut PipelineContext,
    mut response: HttpResponse,
) -> PipelineResult<HttpResponse> {
    for mw in middlewares {
        response = mw.on_inbound_raw_response(ctx, response)?;
    }
    Ok(response)
}

/// Go `applyInboundRawStreamMiddlewares` (pipeline.go:167-178) —
/// `OnInboundRawStream`, FORWARD (loop at pipeline.go:170), once per
/// successful streaming Request (stream.go:387). Wrapping is lazy (S08):
/// forward wrap order means the first-registered middleware's wrapper is the
/// innermost, so events also traverse middlewares in forward order.
pub fn apply_inbound_raw_stream_middlewares(
    middlewares: &[BoxPipelineMiddleware],
    ctx: &mut PipelineContext,
    mut stream: BoxEventStream,
) -> PipelineResult<BoxEventStream> {
    for mw in middlewares {
        stream = mw.on_inbound_raw_stream(ctx, stream)?;
    }
    Ok(stream)
}

/// Go `applyRawRequestMiddlewares` (pipeline.go:180-191) —
/// `OnOutboundRawRequest`, FORWARD, once per Attempt (called from
/// `processRequest`, pipeline.go:374). First error aborts the attempt.
pub fn apply_raw_request_middlewares(
    middlewares: &[BoxPipelineMiddleware],
    ctx: &mut PipelineContext,
    mut request: HttpRequest,
) -> PipelineResult<HttpRequest> {
    for mw in middlewares {
        request = mw.on_outbound_raw_request(ctx, request)?;
    }
    Ok(request)
}

/// Go `applyRawResponseMiddlewares` (pipeline.go:193-205) —
/// `OnOutboundRawResponse`, REVERSE ("last to first", pipeline.go:196-197),
/// once per successful non-streaming Attempt (non_streaming.go:33).
pub fn apply_raw_response_middlewares(
    middlewares: &[BoxPipelineMiddleware],
    ctx: &mut PipelineContext,
    mut response: HttpResponse,
) -> PipelineResult<HttpResponse> {
    for mw in middlewares.iter().rev() {
        response = mw.on_outbound_raw_response(ctx, response)?;
    }
    Ok(response)
}

/// Go `applyRawStreamMiddlewares` (pipeline.go:207-219) —
/// `OnOutboundRawStream`, REVERSE (pipeline.go:210-211), once per successful
/// streaming Attempt (stream.go:298). Reverse wrap order makes the
/// last-registered middleware's wrapper innermost, so events also traverse
/// middlewares in reverse order. Wrapping is lazy (S08).
pub fn apply_raw_stream_middlewares(
    middlewares: &[BoxPipelineMiddleware],
    ctx: &mut PipelineContext,
    mut stream: BoxEventStream,
) -> PipelineResult<BoxEventStream> {
    for mw in middlewares.iter().rev() {
        stream = mw.on_outbound_raw_stream(ctx, stream)?;
    }
    Ok(stream)
}

/// Go `applyRawErrorResponseMiddlewares` (pipeline.go:221-226) —
/// `OnOutboundRawError`, REVERSE, once per failed Attempt. Infallible and
/// never short-circuits: EVERY middleware observes the error (cleanup
/// semantics — Go `TestMiddleware_RawRequest_Error_CleanupMiddlewares`).
pub fn apply_raw_error_response_middlewares(
    middlewares: &[BoxPipelineMiddleware],
    ctx: &mut PipelineContext,
    error: &ConduitError,
) {
    for mw in middlewares.iter().rev() {
        mw.on_outbound_raw_error(ctx, error);
    }
}

/// Go `applyLlmResponseMiddlewares` (pipeline.go:228-240) —
/// `OnOutboundLlmResponse`, REVERSE, once per successful non-streaming
/// Attempt (non_streaming.go:48).
pub fn apply_llm_response_middlewares(
    middlewares: &[BoxPipelineMiddleware],
    ctx: &mut PipelineContext,
    mut response: LlmResponse,
) -> PipelineResult<LlmResponse> {
    for mw in middlewares.iter().rev() {
        response = mw.on_outbound_llm_response(ctx, response)?;
    }
    Ok(response)
}

/// Go `applyLlmStreamMiddlewares` (pipeline.go:242-254) —
/// `OnOutboundLlmStream`, REVERSE, once per successful streaming Attempt
/// (stream.go:338). Wrapping is lazy (S08).
pub fn apply_llm_stream_middlewares(
    middlewares: &[BoxPipelineMiddleware],
    ctx: &mut PipelineContext,
    mut stream: BoxLlmStream,
) -> PipelineResult<BoxLlmStream> {
    for mw in middlewares.iter().rev() {
        stream = mw.on_outbound_llm_stream(ctx, stream)?;
    }
    Ok(stream)
}

// ---------------------------------------------------------------------------
// PipelineContext — request-scoped state threaded through the hooks.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PipelineContext {
    pub request_id: Option<String>,
    pub route: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub order: Vec<String>,
    /// RUST-P8-002 S17 — shared cancellation token standing in for the Go
    /// request `context.Context`. The HTTP layer cancels it on client
    /// disconnect; the pipeline consults it after every failed attempt
    /// (Go `pipeline.go:290-293` `if ctx.Err() != nil`). Clones of the
    /// context share the token (`Arc` inside), so a
    /// [`PipelineContext::cancel_handle`] taken before `process` can cancel
    /// mid-flight from another task.
    pub cancel: CancelToken,
    /// RUST-P8-002 S14 — retry-context record populated by
    /// `Pipeline::process` (Go's loop counters `channelSwitches` /
    /// `sameChannelRetries` + last-error snapshot, `pipeline.go:274-277`,
    /// `:307-309`, `:327-329`). `None` until a `process` call runs; readable
    /// by middlewares/callers afterwards on both success and failure paths.
    pub retry_context: Option<crate::pipeline::RetryContext>,
}

impl PipelineContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_order(&mut self, step: impl Into<String>) {
        self.order.push(step.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ApiFormat, ChatRequest, LlmRequestPayload, RequestType};
    use std::sync::Mutex;

    // ---- shared recording plumbing (Go `trackingMiddleware`,
    // middleware_test.go:94-212) --------------------------------------------

    /// Shared call-order log (Go `callOrder *[]string`). `Arc<Mutex<_>>`
    /// because lazy stream wrappers outlive the `&mut ctx` borrow.
    type CallLog = Arc<Mutex<Vec<String>>>;

    fn new_log() -> CallLog {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn log_push(log: &CallLog, entry: impl Into<String>) {
        if let Ok(mut guard) = log.lock() {
            guard.push(entry.into());
        }
    }

    fn log_snapshot(log: &CallLog) -> Vec<String> {
        log.lock().map(|guard| guard.clone()).unwrap_or_default()
    }

    /// Rust port of Go `trackingMiddleware` (middleware_test.go:94-212):
    /// records `<name>:<GoHookName>` into the shared log for every hook (the
    /// exact labels Go's golden orders use), optionally failing on selected
    /// hooks. Stream hooks additionally wrap the stream to push
    /// `<name>:<GoHookName>:event` per event — lazily.
    struct TrackingMw {
        name: &'static str,
        log: CallLog,
        fail_on_llm_request: bool,
        fail_on_raw_request: bool,
        fail_on_raw_response: bool,
        fail_on_llm_response: bool,
        fail_on_raw_stream: bool,
        fail_on_llm_stream: bool,
    }

    impl TrackingMw {
        fn new(name: &'static str, log: &CallLog) -> Self {
            Self {
                name,
                log: Arc::clone(log),
                fail_on_llm_request: false,
                fail_on_raw_request: false,
                fail_on_raw_response: false,
                fail_on_llm_response: false,
                fail_on_raw_stream: false,
                fail_on_llm_stream: false,
            }
        }
    }

    impl PipelineMiddleware for TrackingMw {
        fn name(&self) -> &str {
            self.name
        }

        fn on_inbound_llm_request(
            &self,
            _ctx: &mut PipelineContext,
            request: LlmRequest,
        ) -> PipelineResult<LlmRequest> {
            // Go logs BEFORE failing (middleware_test.go:131-134).
            log_push(&self.log, format!("{}:OnInboundLlmRequest", self.name));
            if self.fail_on_llm_request {
                return Err(ConduitError::invalid_request(
                    "llm request middleware error",
                ));
            }
            Ok(request)
        }

        fn on_inbound_raw_response(
            &self,
            _ctx: &mut PipelineContext,
            response: HttpResponse,
        ) -> PipelineResult<HttpResponse> {
            log_push(&self.log, format!("{}:OnInboundRawResponse", self.name));
            Ok(response)
        }

        fn on_inbound_raw_stream(
            &self,
            _ctx: &mut PipelineContext,
            stream: BoxEventStream,
        ) -> PipelineResult<BoxEventStream> {
            log_push(&self.log, format!("{}:OnInboundRawStream", self.name));
            let log = Arc::clone(&self.log);
            let name = self.name;
            Ok(Box::new(stream.inspect(move |_| {
                log_push(&log, format!("{name}:OnInboundRawStream:event"));
            })))
        }

        fn on_outbound_raw_request(
            &self,
            _ctx: &mut PipelineContext,
            request: HttpRequest,
        ) -> PipelineResult<HttpRequest> {
            log_push(&self.log, format!("{}:OnOutboundRawRequest", self.name));
            if self.fail_on_raw_request {
                return Err(ConduitError::invalid_request(
                    "raw request middleware error",
                ));
            }
            Ok(request)
        }

        fn on_outbound_raw_error(&self, _ctx: &mut PipelineContext, _error: &ConduitError) {
            // Go's tracking label is "OnOutboundRawErrorResponse"
            // (middleware_test.go:167) — keep it for golden-order fidelity.
            log_push(
                &self.log,
                format!("{}:OnOutboundRawErrorResponse", self.name),
            );
        }

        fn on_outbound_raw_response(
            &self,
            _ctx: &mut PipelineContext,
            response: HttpResponse,
        ) -> PipelineResult<HttpResponse> {
            log_push(&self.log, format!("{}:OnOutboundRawResponse", self.name));
            if self.fail_on_raw_response {
                return Err(ConduitError::invalid_request(
                    "raw response middleware error",
                ));
            }
            Ok(response)
        }

        fn on_outbound_llm_response(
            &self,
            _ctx: &mut PipelineContext,
            response: LlmResponse,
        ) -> PipelineResult<LlmResponse> {
            log_push(&self.log, format!("{}:OnOutboundLlmResponse", self.name));
            if self.fail_on_llm_response {
                return Err(ConduitError::invalid_request(
                    "llm response middleware error",
                ));
            }
            Ok(response)
        }

        fn on_outbound_raw_stream(
            &self,
            _ctx: &mut PipelineContext,
            stream: BoxEventStream,
        ) -> PipelineResult<BoxEventStream> {
            log_push(&self.log, format!("{}:OnOutboundRawStream", self.name));
            if self.fail_on_raw_stream {
                return Err(ConduitError::invalid_request("raw stream middleware error"));
            }
            let log = Arc::clone(&self.log);
            let name = self.name;
            Ok(Box::new(stream.inspect(move |_| {
                log_push(&log, format!("{name}:OnOutboundRawStream:event"));
            })))
        }

        fn on_outbound_llm_stream(
            &self,
            _ctx: &mut PipelineContext,
            stream: BoxLlmStream,
        ) -> PipelineResult<BoxLlmStream> {
            log_push(&self.log, format!("{}:OnOutboundLlmStream", self.name));
            if self.fail_on_llm_stream {
                return Err(ConduitError::invalid_request("llm stream middleware error"));
            }
            let log = Arc::clone(&self.log);
            let name = self.name;
            Ok(Box::new(stream.inspect(move |_| {
                log_push(&log, format!("{name}:OnOutboundLlmStream:event"));
            })))
        }
    }

    /// Three tracking middlewares M1/M2/M3 sharing one log — the Go test
    /// fixture shape (middleware_test.go:220-222).
    fn tracking3(log: &CallLog) -> Vec<BoxPipelineMiddleware> {
        vec![
            Arc::new(TrackingMw::new("M1", log)),
            Arc::new(TrackingMw::new("M2", log)),
            Arc::new(TrackingMw::new("M3", log)),
        ]
    }

    fn dummy_llm_request() -> LlmRequest {
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

    fn dummy_http_request() -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            ..HttpRequest::default()
        }
    }

    fn dummy_http_response() -> HttpResponse {
        HttpResponse {
            status: 200,
            ..HttpResponse::default()
        }
    }

    fn dummy_events(count: usize) -> Vec<StreamEvent> {
        (0..count)
            .map(|i| StreamEvent {
                data: Some(format!("event-{i}")),
                ..StreamEvent::default()
            })
            .collect()
    }

    // ---- OnInboundLlmRequest (forward, mirrors Go
    // TestMiddleware_NonStreaming_CallOrder segment :265-267 and
    // TestMiddleware_LlmRequest_Error :405-442) ------------------------------

    #[test]
    fn before_request_middlewares_run_in_forward_order() -> PipelineResult<()> {
        let log = new_log();
        let mws = tracking3(&log);
        let mut ctx = PipelineContext::new();
        let _ = apply_before_request_middlewares(&mws, &mut ctx, dummy_llm_request())?;
        assert_eq!(
            log_snapshot(&log),
            vec![
                "M1:OnInboundLlmRequest",
                "M2:OnInboundLlmRequest",
                "M3:OnInboundLlmRequest",
            ]
        );
        Ok(())
    }

    /// Mutating middleware that injects a header into `extra_headers`
    /// (mirrors Go's billing-header shape, cc/billing_header.go — one
    /// concern per middleware, S07).
    struct InjectHeaderMw {
        key: &'static str,
        value: &'static str,
    }

    impl PipelineMiddleware for InjectHeaderMw {
        fn name(&self) -> &str {
            "inject-header"
        }
        fn on_inbound_llm_request(
            &self,
            _ctx: &mut PipelineContext,
            mut request: LlmRequest,
        ) -> PipelineResult<LlmRequest> {
            request
                .extra_headers
                .insert(self.key.to_string(), self.value.to_string());
            Ok(request)
        }
    }

    #[test]
    fn before_request_middlewares_pass_mutated_request_to_next() -> PipelineResult<()> {
        // The output of one middleware feeds the next (Go re-assigns
        // `request, err = dec.OnInboundLlmRequest(ctx, request)`).
        let mws: Vec<BoxPipelineMiddleware> = vec![
            Arc::new(InjectHeaderMw {
                key: "x-first",
                value: "1",
            }),
            Arc::new(InjectHeaderMw {
                key: "x-second",
                value: "2",
            }),
        ];
        let mut ctx = PipelineContext::new();
        let out = apply_before_request_middlewares(&mws, &mut ctx, dummy_llm_request())?;
        assert_eq!(
            out.extra_headers.get("x-first").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            out.extra_headers.get("x-second").map(String::as_str),
            Some("2")
        );
        Ok(())
    }

    #[test]
    fn before_request_middlewares_abort_on_error_and_skip_rest() -> PipelineResult<()> {
        // Go TestMiddleware_LlmRequest_Error: M2 fails -> M3 never runs.
        let log = new_log();
        let mut failing = TrackingMw::new("M2", &log);
        failing.fail_on_llm_request = true;
        let mws: Vec<BoxPipelineMiddleware> = vec![
            Arc::new(TrackingMw::new("M1", &log)),
            Arc::new(failing),
            Arc::new(TrackingMw::new("M3", &log)),
        ];
        let mut ctx = PipelineContext::new();
        let err = apply_before_request_middlewares(&mws, &mut ctx, dummy_llm_request()).err();
        assert_eq!(
            err.as_ref().map(|e| e.error_type()),
            Some("invalid_request")
        );
        // M1 and M2 logged (M2 logs before failing, like Go), M3 skipped.
        assert_eq!(
            log_snapshot(&log),
            vec!["M1:OnInboundLlmRequest", "M2:OnInboundLlmRequest"]
        );
        Ok(())
    }

    #[test]
    fn before_request_middlewares_empty_chain_is_identity() -> PipelineResult<()> {
        let mws: Vec<BoxPipelineMiddleware> = vec![];
        let mut ctx = PipelineContext::new();
        let out = apply_before_request_middlewares(&mws, &mut ctx, dummy_llm_request())?;
        assert_eq!(out.model.as_deref(), Some("dummy-model"));
        assert!(ctx.order.is_empty());
        Ok(())
    }

    // ---- OnOutboundRawRequest (forward, Go golden :268-270; error case
    // TestMiddleware_RawRequest_Error :445-492) ------------------------------

    #[test]
    fn raw_request_middlewares_run_in_forward_order() -> PipelineResult<()> {
        let log = new_log();
        let mws = tracking3(&log);
        let mut ctx = PipelineContext::new();
        let _ = apply_raw_request_middlewares(&mws, &mut ctx, dummy_http_request())?;
        assert_eq!(
            log_snapshot(&log),
            vec![
                "M1:OnOutboundRawRequest",
                "M2:OnOutboundRawRequest",
                "M3:OnOutboundRawRequest",
            ]
        );
        Ok(())
    }

    #[test]
    fn raw_request_middlewares_stop_on_error() -> PipelineResult<()> {
        // Go TestMiddleware_RawRequest_Error: M1, M2 called; M3 not.
        let log = new_log();
        let mut failing = TrackingMw::new("M2", &log);
        failing.fail_on_raw_request = true;
        let mws: Vec<BoxPipelineMiddleware> = vec![
            Arc::new(TrackingMw::new("M1", &log)),
            Arc::new(failing),
            Arc::new(TrackingMw::new("M3", &log)),
        ];
        let mut ctx = PipelineContext::new();
        let err = apply_raw_request_middlewares(&mws, &mut ctx, dummy_http_request()).err();
        assert!(err.is_some(), "failing middleware must abort the chain");
        assert_eq!(
            log_snapshot(&log),
            vec!["M1:OnOutboundRawRequest", "M2:OnOutboundRawRequest"]
        );
        Ok(())
    }

    // ---- OnOutboundRawResponse (reverse, Go golden :272-274; error case
    // TestMiddleware_RawResponse_Error :495-542) -----------------------------

    #[test]
    fn raw_response_middlewares_run_in_reverse_order() -> PipelineResult<()> {
        let log = new_log();
        let mws = tracking3(&log);
        let mut ctx = PipelineContext::new();
        let _ = apply_raw_response_middlewares(&mws, &mut ctx, dummy_http_response())?;
        assert_eq!(
            log_snapshot(&log),
            vec![
                "M3:OnOutboundRawResponse",
                "M2:OnOutboundRawResponse",
                "M1:OnOutboundRawResponse",
            ]
        );
        Ok(())
    }

    #[test]
    fn raw_response_middlewares_stop_on_error_in_reverse() -> PipelineResult<()> {
        // Go TestMiddleware_RawResponse_Error: M3 and M2 called (reverse), M1 not.
        let log = new_log();
        let mut failing = TrackingMw::new("M2", &log);
        failing.fail_on_raw_response = true;
        let mws: Vec<BoxPipelineMiddleware> = vec![
            Arc::new(TrackingMw::new("M1", &log)),
            Arc::new(failing),
            Arc::new(TrackingMw::new("M3", &log)),
        ];
        let mut ctx = PipelineContext::new();
        let err = apply_raw_response_middlewares(&mws, &mut ctx, dummy_http_response()).err();
        assert!(err.is_some());
        assert_eq!(
            log_snapshot(&log),
            vec!["M3:OnOutboundRawResponse", "M2:OnOutboundRawResponse"]
        );
        Ok(())
    }

    // ---- OnOutboundLlmResponse (reverse, Go golden :275-277) ---------------

    #[test]
    fn llm_response_middlewares_run_in_reverse_order() -> PipelineResult<()> {
        let log = new_log();
        let mws = tracking3(&log);
        let mut ctx = PipelineContext::new();
        let _ = apply_llm_response_middlewares(&mws, &mut ctx, LlmResponse::default())?;
        assert_eq!(
            log_snapshot(&log),
            vec![
                "M3:OnOutboundLlmResponse",
                "M2:OnOutboundLlmResponse",
                "M1:OnOutboundLlmResponse",
            ]
        );
        Ok(())
    }

    /// Port of Go `TestMiddleware_LlmResponse_Error` (middleware_test.go:545-592):
    /// M2 fails in `OnOutboundLlmResponse` → M3 and M2 are called (reverse), M1
    /// is NOT. The runner-level test is the only place this can fire — the Rust
    /// pipeline does not wire `apply_llm_response_middlewares` yet (structural
    /// GAP: no unified `LlmResponse` stage, see `finish_non_stream_response`).
    #[test]
    fn llm_response_middlewares_stop_on_error_in_reverse() -> PipelineResult<()> {
        let log = new_log();
        let mut failing = TrackingMw::new("M2", &log);
        failing.fail_on_llm_response = true;
        let mws: Vec<BoxPipelineMiddleware> = vec![
            Arc::new(TrackingMw::new("M1", &log)),
            Arc::new(failing),
            Arc::new(TrackingMw::new("M3", &log)),
        ];
        let mut ctx = PipelineContext::new();
        let err = apply_llm_response_middlewares(&mws, &mut ctx, LlmResponse::default()).err();
        assert!(err.is_some(), "failing middleware must abort the chain");
        assert_eq!(
            log_snapshot(&log),
            vec!["M3:OnOutboundLlmResponse", "M2:OnOutboundLlmResponse"]
        );
        Ok(())
    }

    /// Port of Go `TestMiddleware_RawStream_Error` (middleware_test.go:595-638):
    /// M2 fails in `OnOutboundRawStream` → M3 and M2 are called (reverse), M1
    /// is NOT.
    #[test]
    fn raw_stream_middlewares_stop_on_error_in_reverse() -> PipelineResult<()> {
        let log = new_log();
        let mut failing = TrackingMw::new("M2", &log);
        failing.fail_on_raw_stream = true;
        let mws: Vec<BoxPipelineMiddleware> = vec![
            Arc::new(TrackingMw::new("M1", &log)),
            Arc::new(failing),
            Arc::new(TrackingMw::new("M3", &log)),
        ];
        let mut ctx = PipelineContext::new();
        let stream: BoxEventStream = Box::new(dummy_events(1).into_iter());
        let err = apply_raw_stream_middlewares(&mws, &mut ctx, stream).err();
        assert!(err.is_some(), "failing middleware must abort the chain");
        assert_eq!(
            log_snapshot(&log),
            vec!["M3:OnOutboundRawStream", "M2:OnOutboundRawStream"]
        );
        Ok(())
    }

    /// Port of Go `TestMiddleware_LlmStream_Error` (middleware_test.go:641-684):
    /// M2 fails in `OnOutboundLlmStream` → M3 and M2 are called (reverse), M1
    /// is NOT. The runner-level test is the only place this can fire — the Rust
    /// pipeline does not wire `apply_llm_stream_middlewares` yet (structural
    /// GAP: no unified `LlmResponse` stream stage, see `finish_stream_events`).
    #[test]
    fn llm_stream_middlewares_stop_on_error_in_reverse() -> PipelineResult<()> {
        let log = new_log();
        let mut failing = TrackingMw::new("M2", &log);
        failing.fail_on_llm_stream = true;
        let mws: Vec<BoxPipelineMiddleware> = vec![
            Arc::new(TrackingMw::new("M1", &log)),
            Arc::new(failing),
            Arc::new(TrackingMw::new("M3", &log)),
        ];
        let mut ctx = PipelineContext::new();
        let stream: BoxLlmStream = Box::new(vec![LlmResponse::default()].into_iter());
        let err = apply_llm_stream_middlewares(&mws, &mut ctx, stream).err();
        assert!(err.is_some(), "failing middleware must abort the chain");
        assert_eq!(
            log_snapshot(&log),
            vec!["M3:OnOutboundLlmStream", "M2:OnOutboundLlmStream"]
        );
        Ok(())
    }

    // ---- OnOutboundRawError (reverse, no short-circuit; Go golden :396-398
    // and cleanup case :785-789) ---------------------------------------------

    #[test]
    fn raw_error_middlewares_run_in_reverse_and_never_short_circuit() {
        let log = new_log();
        let mws = tracking3(&log);
        let mut ctx = PipelineContext::new();
        let err = ConduitError::upstream("executor error");
        apply_raw_error_response_middlewares(&mws, &mut ctx, &err);
        // ALL middlewares see the error, in reverse order (Go cleanup
        // semantics: even middlewares whose request hook never ran).
        assert_eq!(
            log_snapshot(&log),
            vec![
                "M3:OnOutboundRawErrorResponse",
                "M2:OnOutboundRawErrorResponse",
                "M1:OnOutboundRawErrorResponse",
            ]
        );
    }

    // ---- OnInboundRawResponse (FORWARD — Go pipeline.go:157 loop; golden
    // :279-281) ---------------------------------------------------------------

    #[test]
    fn inbound_raw_response_middlewares_run_in_forward_order() -> PipelineResult<()> {
        let log = new_log();
        let mws = tracking3(&log);
        let mut ctx = PipelineContext::new();
        let _ = apply_inbound_raw_response_middlewares(&mws, &mut ctx, dummy_http_response())?;
        assert_eq!(
            log_snapshot(&log),
            vec![
                "M1:OnInboundRawResponse",
                "M2:OnInboundRawResponse",
                "M3:OnInboundRawResponse",
            ]
        );
        Ok(())
    }

    // ---- OnOutboundRawStream (reverse, Go golden :342-344) -----------------

    #[test]
    fn raw_stream_middlewares_wrap_in_reverse_and_events_traverse_reverse() -> PipelineResult<()> {
        let log = new_log();
        let mws = tracking3(&log);
        let mut ctx = PipelineContext::new();
        let stream: BoxEventStream = Box::new(dummy_events(1).into_iter());
        let wrapped = apply_raw_stream_middlewares(&mws, &mut ctx, stream)?;
        // Wrap-time order is reverse (Go loop pipeline.go:210-211).
        assert_eq!(
            log_snapshot(&log),
            vec![
                "M3:OnOutboundRawStream",
                "M2:OnOutboundRawStream",
                "M1:OnOutboundRawStream",
            ]
        );
        // Per-event traversal: reverse wrap => M3 innermost, so each event
        // passes M3 -> M2 -> M1.
        let collected: Vec<StreamEvent> = wrapped.collect();
        assert_eq!(collected.len(), 1);
        let events_only: Vec<String> = log_snapshot(&log)
            .into_iter()
            .filter(|entry| entry.ends_with(":event"))
            .collect();
        assert_eq!(
            events_only,
            vec![
                "M3:OnOutboundRawStream:event",
                "M2:OnOutboundRawStream:event",
                "M1:OnOutboundRawStream:event",
            ]
        );
        Ok(())
    }

    // ---- OnOutboundLlmStream (reverse, Go golden :345-347) -----------------

    #[test]
    fn llm_stream_middlewares_wrap_in_reverse_order() -> PipelineResult<()> {
        let log = new_log();
        let mws = tracking3(&log);
        let mut ctx = PipelineContext::new();
        let stream: BoxLlmStream = Box::new(vec![LlmResponse::default()].into_iter());
        let wrapped = apply_llm_stream_middlewares(&mws, &mut ctx, stream)?;
        assert_eq!(
            log_snapshot(&log),
            vec![
                "M3:OnOutboundLlmStream",
                "M2:OnOutboundLlmStream",
                "M1:OnOutboundLlmStream",
            ]
        );
        assert_eq!(wrapped.count(), 1, "wrapper must pass events through");
        Ok(())
    }

    // ---- OnInboundRawStream (FORWARD — Go pipeline.go:170 loop; golden
    // :348-350) ---------------------------------------------------------------

    #[test]
    fn inbound_raw_stream_middlewares_wrap_forward_and_events_traverse_forward()
    -> PipelineResult<()> {
        let log = new_log();
        let mws = tracking3(&log);
        let mut ctx = PipelineContext::new();
        let stream: BoxEventStream = Box::new(dummy_events(1).into_iter());
        let wrapped = apply_inbound_raw_stream_middlewares(&mws, &mut ctx, stream)?;
        assert_eq!(
            log_snapshot(&log),
            vec![
                "M1:OnInboundRawStream",
                "M2:OnInboundRawStream",
                "M3:OnInboundRawStream",
            ]
        );
        // Forward wrap => M1 innermost, so each event passes M1 -> M2 -> M3.
        let _ = wrapped.count();
        let events_only: Vec<String> = log_snapshot(&log)
            .into_iter()
            .filter(|entry| entry.ends_with(":event"))
            .collect();
        assert_eq!(
            events_only,
            vec![
                "M1:OnInboundRawStream:event",
                "M2:OnInboundRawStream:event",
                "M3:OnInboundRawStream:event",
            ]
        );
        Ok(())
    }

    // ---- S08 guard: stream wrappers must NOT pre-collect -------------------

    /// Source iterator that counts how many events have been pulled from it —
    /// if a middleware pre-collected the stream at wrap time, the count would
    /// jump before the caller consumes anything.
    struct CountingSource {
        remaining: usize,
        pulled: Arc<Mutex<usize>>,
    }

    impl Iterator for CountingSource {
        type Item = StreamEvent;
        fn next(&mut self) -> Option<StreamEvent> {
            if self.remaining == 0 {
                return None;
            }
            self.remaining -= 1;
            if let Ok(mut pulled) = self.pulled.lock() {
                *pulled += 1;
            }
            Some(StreamEvent::default())
        }
    }

    #[test]
    fn stream_middlewares_do_not_pre_collect_events() -> PipelineResult<()> {
        // S08 — applying BOTH stream middleware chains (raw reverse + inbound
        // forward) must not pull a single event from the source; events flow
        // one-by-one only when the caller consumes them.
        let log = new_log();
        let mws: Vec<BoxPipelineMiddleware> = vec![
            Arc::new(TrackingMw::new("A", &log)),
            Arc::new(TrackingMw::new("B", &log)),
        ];
        let pulled = Arc::new(Mutex::new(0usize));
        let source = CountingSource {
            remaining: 3,
            pulled: Arc::clone(&pulled),
        };
        let mut ctx = PipelineContext::new();

        let stream: BoxEventStream = Box::new(source);
        let stream = apply_raw_stream_middlewares(&mws, &mut ctx, stream)?;
        let mut stream = apply_inbound_raw_stream_middlewares(&mws, &mut ctx, stream)?;

        let pulled_count =
            |pulled: &Arc<Mutex<usize>>| pulled.lock().map(|guard| *guard).unwrap_or(usize::MAX);
        // Wrap phase pulled nothing.
        assert_eq!(pulled_count(&pulled), 0, "wrapping must not consume events");
        assert!(
            !log_snapshot(&log).iter().any(|e| e.ends_with(":event")),
            "no event may traverse a wrapper before the caller consumes it"
        );

        // Consuming exactly one event pulls exactly one from the source and
        // routes it through all four wrappers (raw B->A, then inbound A->B).
        assert!(stream.next().is_some());
        assert_eq!(
            pulled_count(&pulled),
            1,
            "lazy: one next() = one source pull"
        );
        let events_only: Vec<String> = log_snapshot(&log)
            .into_iter()
            .filter(|entry| entry.ends_with(":event"))
            .collect();
        assert_eq!(
            events_only,
            vec![
                "B:OnOutboundRawStream:event",
                "A:OnOutboundRawStream:event",
                "A:OnInboundRawStream:event",
                "B:OnInboundRawStream:event",
            ]
        );

        // Draining consumes the rest — nothing more, nothing less.
        assert_eq!(stream.count(), 2);
        assert_eq!(pulled_count(&pulled), 3);
        Ok(())
    }

    // ---- Default no-op hooks (Go DummyMiddleware, middleware.go:197-239) ---

    /// Middleware overriding NOTHING — every hook must behave as identity.
    struct NoopMw;

    impl PipelineMiddleware for NoopMw {
        fn name(&self) -> &str {
            "noop"
        }
    }

    #[test]
    fn default_hooks_are_identity_no_ops() -> PipelineResult<()> {
        // Mirrors Go DummyMiddleware: a middleware that implements only Name()
        // passes every value through untouched on all nine hooks.
        let mws: Vec<BoxPipelineMiddleware> = vec![Arc::new(NoopMw)];
        let mut ctx = PipelineContext::new();

        let request = apply_before_request_middlewares(&mws, &mut ctx, dummy_llm_request())?;
        assert_eq!(request.model.as_deref(), Some("dummy-model"));

        let raw_req = apply_raw_request_middlewares(&mws, &mut ctx, dummy_http_request())?;
        assert_eq!(raw_req.path, "/v1/chat/completions");

        let raw_resp = apply_raw_response_middlewares(&mws, &mut ctx, dummy_http_response())?;
        assert_eq!(raw_resp.status, 200);

        let llm_resp = apply_llm_response_middlewares(&mws, &mut ctx, LlmResponse::default())?;
        assert_eq!(llm_resp, LlmResponse::default());

        let in_resp =
            apply_inbound_raw_response_middlewares(&mws, &mut ctx, dummy_http_response())?;
        assert_eq!(in_resp.status, 200);

        // Error hook: infallible no-op.
        apply_raw_error_response_middlewares(&mws, &mut ctx, &ConduitError::upstream("boom"));

        // Stream hooks: identity wrappers preserving every event.
        let raw_stream: BoxEventStream = Box::new(dummy_events(2).into_iter());
        let raw_stream = apply_raw_stream_middlewares(&mws, &mut ctx, raw_stream)?;
        assert_eq!(raw_stream.count(), 2);

        let in_stream: BoxEventStream = Box::new(dummy_events(2).into_iter());
        let in_stream = apply_inbound_raw_stream_middlewares(&mws, &mut ctx, in_stream)?;
        assert_eq!(in_stream.count(), 2);

        let llm_stream: BoxLlmStream = Box::new(vec![LlmResponse::default()].into_iter());
        let llm_stream = apply_llm_stream_middlewares(&mws, &mut ctx, llm_stream)?;
        assert_eq!(llm_stream.count(), 1);

        // No middleware recorded anything on the context.
        assert!(ctx.order.is_empty());
        Ok(())
    }
}
