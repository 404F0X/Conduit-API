//! Live-preview pipeline middleware -- Rust port of Go
//! `livePreviewMiddleware` (`orchestrator/live_streaming.go:14-133`).
//!
//! Hooks the [`LiveStreamRegistry`] into the pipeline so live preview
//! buffers are registered/written/closed at the right lifecycle points:
//!
//! - `on_outbound_raw_request` -- registers fresh [`ChunkBuffer`]s for the
//!   request and execution ids (Go lines 55-68).
//! - `on_outbound_raw_response` -- writes the non-streaming response body
//!   to the preview buffers, then closes + unregisters them.
//! - `on_outbound_raw_stream` -- wraps the outbound stream to fan each
//!   event (binary-summarized) to the execution buffer; closes +
//!   unregisters on stream end (Go `liveRequestExecutionStream`, lines
//!   135-179).
//! - `on_inbound_raw_stream` -- wraps the inbound stream to fan each
//!   event to the request buffer (Go `liveRequestStream`, lines 181-225).
//! - `on_outbound_raw_error` -- closes + unregisters both buffers (Go
//!   lines 71-89).
//!
//! The enable/disable gating lives in [`LivePreviewPlan`] (computed by
//! `live_preview_plan` in the orchestrator pure-decision layer, mirroring
//! Go's `OnInboundLlmRequest` gating branches). When the plan is disabled
//! every hook is a no-op.
//!
//! The heavy infrastructure ([`LiveStreamRegistry`], [`ChunkBuffer`],
//! [`StreamObserver`], [`LivePreviewObserver`]) already exists in
//! [`crate::live_streaming`].

use std::sync::Arc;

use conduit_llm::{HttpRequest, HttpResponse, StreamEvent};
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{BoxEventStream, PipelineContext, PipelineResult};

use crate::live_streaming::{ChunkBuffer, LiveStreamRegistry};
use crate::outbound_stream::summarize_binary_chunk;

// ---------------------------------------------------------------------------
// LivePreviewMiddleware
// ---------------------------------------------------------------------------

/// Pipeline middleware that manages live-preview buffers for in-flight
/// requests.
///
/// Go parity: `livePreviewMiddleware` (`orchestrator/live_streaming.go:14-133`).
///
/// Construction requires the shared [`LiveStreamRegistry`] (Go field
/// `liveStreamRegistry`). The request/execution ids are read from
/// [`PipelineContext::metadata`] at each hook call (keys `request_id` and
/// `request_exec_id`), matching the ids the persist middlewares stamp.
/// The `enabled` flag is read from metadata key `live_preview_enabled`
/// (set by the orchestrator wiring from the [`LivePreviewPlan`]).
pub struct LivePreviewMiddleware {
    registry: Arc<LiveStreamRegistry>,
}

impl LivePreviewMiddleware {
    /// Build a live-preview middleware backed by `registry`.
    /// Mirrors Go `withLivePreview` (`live_streaming.go:24-30`).
    pub fn new(registry: Arc<LiveStreamRegistry>) -> Self {
        Self { registry }
    }

    /// Read whether live preview is enabled from the pipeline context.
    /// The orchestrator wiring stamps `live_preview_enabled = "true"` when
    /// the [`LivePreviewPlan`] is enabled.
    fn is_enabled(ctx: &PipelineContext) -> bool {
        ctx.metadata
            .get("live_preview_enabled")
            .map(|v| v == "true")
            .unwrap_or(false)
    }

    /// Read the request id from context metadata. Go reads from
    /// `m.state.Request.ID`; the Rust persist middleware stamps this as
    /// `request_id` (or `__persist_request_id`).
    fn request_id(ctx: &PipelineContext) -> Option<&str> {
        ctx.metadata
            .get("request_id")
            .or_else(|| ctx.metadata.get("__persist_request_id"))
            .map(String::as_str)
    }

    /// Read the execution id from context metadata. Go reads from
    /// `m.state.RequestExec.ID`; the Rust persist middleware stamps this
    /// as `request_exec_id` (or `__persist_execution_id`).
    fn execution_id(ctx: &PipelineContext) -> Option<&str> {
        ctx.metadata
            .get("request_exec_id")
            .or_else(|| ctx.metadata.get("__persist_execution_id"))
            .map(String::as_str)
    }

    /// Close + unregister both buffers (execution first, then request —
    /// Go order, `live_streaming.go:76-89`). Shared by `on_outbound_raw_error`
    /// and the stream-close teardown paths.
    fn teardown_buffers(&self, ctx: &PipelineContext) {
        if let Some(exec_id) = Self::execution_id(ctx)
            && let Some(buffer) = self.registry.get_execution_buffer(exec_id)
        {
            buffer.close();
            self.registry.unregister_execution(exec_id);
        }
        if let Some(request_id) = Self::request_id(ctx)
            && let Some(buffer) = self.registry.get_request_buffer(request_id)
        {
            buffer.close();
            self.registry.unregister_request(request_id);
        }
    }
}

impl PipelineMiddleware for LivePreviewMiddleware {
    fn name(&self) -> &str {
        "live-preview"
    }

    /// Go `OnOutboundRawRequest` (`live_streaming.go:55-68`): register a
    /// fresh buffer per id when none is registered yet.
    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        if !Self::is_enabled(ctx) {
            return Ok(request);
        }

        if let Some(request_id) = Self::request_id(ctx)
            && self.registry.get_request_buffer(request_id).is_none()
        {
            self.registry
                .register_request(request_id, ChunkBuffer::new());
        }

        if let Some(exec_id) = Self::execution_id(ctx)
            && self.registry.get_execution_buffer(exec_id).is_none()
        {
            self.registry
                .register_execution(exec_id, ChunkBuffer::new());
        }

        Ok(request)
    }

    /// Go `OnOutboundRawResponse` (non-streaming path): for non-streaming
    /// requests the response body is a single chunk. Write a summarized
    /// copy to the preview buffers, then close + unregister them.
    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        if !Self::is_enabled(ctx) {
            return Ok(response);
        }

        // Build a single StreamEvent from the response body so the
        // preview buffer has something to show.
        let body_event = StreamEvent {
            data: response
                .json_body
                .as_ref()
                .and_then(|v| serde_json::to_string(v).ok()),
            ..StreamEvent::default()
        };
        let summarized = summarize_binary_chunk(&body_event);

        if let Some(exec_id) = Self::execution_id(ctx)
            && let Some(buffer) = self.registry.get_execution_buffer(exec_id)
        {
            buffer.append(summarized.clone());
        }
        if let Some(request_id) = Self::request_id(ctx)
            && let Some(buffer) = self.registry.get_request_buffer(request_id)
        {
            buffer.append(summarized);
        }

        // Non-streaming: close + unregister immediately.
        self.teardown_buffers(ctx);

        Ok(response)
    }

    /// Go `OnOutboundRawError` (`live_streaming.go:71-89`): close +
    /// unregister both buffers.
    fn on_outbound_raw_error(
        &self,
        ctx: &mut PipelineContext,
        _error: &conduit_core::ConduitError,
    ) {
        if !Self::is_enabled(ctx) {
            return;
        }
        self.teardown_buffers(ctx);
    }

    /// Go `OnOutboundRawStream` (`live_streaming.go:91-111`): wrap the
    /// outbound stream so each event fans a binary-summarized copy to the
    /// execution buffer. On stream close the buffer is closed and the
    /// execution id is unregistered (Go `liveRequestExecutionStream.Close`,
    /// lines 169-179).
    fn on_outbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        if !Self::is_enabled(ctx) {
            return Ok(stream);
        }

        let exec_id = match Self::execution_id(ctx) {
            Some(id) => id.to_string(),
            None => return Ok(stream),
        };

        let buffer = match self.registry.get_execution_buffer(&exec_id) {
            Some(b) => b,
            None => return Ok(stream),
        };

        let registry = Arc::clone(&self.registry);
        let wrapped: BoxEventStream = Box::new(LivePreviewStreamWrapper {
            inner: stream,
            buffer,
            registry,
            id: exec_id,
            is_execution: true,
            closed: false,
        });

        Ok(wrapped)
    }

    /// Go `OnInboundRawStream` (`live_streaming.go:113-133`): wrap the
    /// inbound stream so each event fans a binary-summarized copy to the
    /// request buffer. On stream close the buffer is closed and the
    /// request id is unregistered (Go `liveRequestStream.Close`, lines
    /// 215-225).
    fn on_inbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        if !Self::is_enabled(ctx) {
            return Ok(stream);
        }

        let request_id = match Self::request_id(ctx) {
            Some(id) => id.to_string(),
            None => return Ok(stream),
        };

        let buffer = match self.registry.get_request_buffer(&request_id) {
            Some(b) => b,
            None => return Ok(stream),
        };

        let registry = Arc::clone(&self.registry);
        let wrapped: BoxEventStream = Box::new(LivePreviewStreamWrapper {
            inner: stream,
            buffer,
            registry,
            id: request_id,
            is_execution: false,
            closed: false,
        });

        Ok(wrapped)
    }
}

// ---------------------------------------------------------------------------
// LivePreviewStreamWrapper — iterator-based stream wrapper (S08 lazy).
// ---------------------------------------------------------------------------

/// Wraps an event stream to fan each event (binary-summarized) to a
/// [`ChunkBuffer`], and closes + unregisters the buffer when the stream
/// is exhausted.
///
/// Ports Go `liveRequestExecutionStream` (`live_streaming.go:135-179`)
/// and `liveRequestStream` (`live_streaming.go:181-225`) — both have
/// identical `Next()`/`Close()` logic differing only in which registry
/// method they call (execution vs request). The `is_execution` flag
/// selects the unregister path.
struct LivePreviewStreamWrapper {
    inner: BoxEventStream,
    buffer: ChunkBuffer,
    registry: Arc<LiveStreamRegistry>,
    id: String,
    is_execution: bool,
    closed: bool,
}

impl LivePreviewStreamWrapper {
    /// Close + unregister (Go `liveRequestExecutionStream.Close` /
    /// `liveRequestStream.Close`). Guarded by `self.closed` to mirror
    /// Go's `s.closed` idempotency guard.
    fn teardown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.buffer.close();
        if self.is_execution {
            self.registry.unregister_execution(&self.id);
        } else {
            self.registry.unregister_request(&self.id);
        }
    }
}

impl Iterator for LivePreviewStreamWrapper {
    type Item = StreamEvent;

    /// Go `Next()` (`live_streaming.go:144-159` / `:190-205`): forward
    /// the next event from the inner stream and append a
    /// binary-summarized copy to the buffer.
    fn next(&mut self) -> Option<StreamEvent> {
        match self.inner.next() {
            Some(event) => {
                let summarized = summarize_binary_chunk(&event);
                self.buffer.append(summarized);
                Some(event)
            }
            None => {
                // Stream exhausted — close + unregister.
                self.teardown();
                None
            }
        }
    }
}

impl Drop for LivePreviewStreamWrapper {
    /// Safety net: if the wrapper is dropped without being fully consumed
    /// (e.g. the consumer short-circuits), ensure the buffer is closed
    /// and the id is unregistered.
    fn drop(&mut self) {
        self.teardown();
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::ConduitError;
    use conduit_llm::HttpResponse;

    /// Build a test event with a data payload.
    fn event(data: &str) -> StreamEvent {
        StreamEvent {
            data: Some(data.to_string()),
            ..StreamEvent::default()
        }
    }

    /// Build a PipelineContext with live preview enabled and the given ids.
    fn ctx_enabled(request_id: &str, exec_id: &str) -> PipelineContext {
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("live_preview_enabled".to_string(), "true".to_string());
        ctx.metadata
            .insert("request_id".to_string(), request_id.to_string());
        ctx.metadata
            .insert("request_exec_id".to_string(), exec_id.to_string());
        ctx
    }

    // -----------------------------------------------------------------------
    // Test: on_outbound_raw_request registers buffers
    // -----------------------------------------------------------------------

    #[test]
    fn registers_buffers_on_outbound_request() -> Result<(), Box<dyn std::error::Error>> {
        let registry = Arc::new(LiveStreamRegistry::new());
        let mw = LivePreviewMiddleware::new(registry.clone());
        let mut ctx = ctx_enabled("req-1", "exec-1");

        let request = HttpRequest::default();
        let _ = mw.on_outbound_raw_request(&mut ctx, request)?;

        assert!(
            registry.get_request_buffer("req-1").is_some(),
            "request buffer must be registered"
        );
        assert!(
            registry.get_execution_buffer("exec-1").is_some(),
            "execution buffer must be registered"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test: on_outbound_raw_error closes + unregisters
    // -----------------------------------------------------------------------

    #[test]
    fn error_closes_and_unregisters_buffers() -> Result<(), Box<dyn std::error::Error>> {
        let registry = Arc::new(LiveStreamRegistry::new());
        let mw = LivePreviewMiddleware::new(registry.clone());
        let mut ctx = ctx_enabled("req-2", "exec-2");

        // Register buffers via the outbound request hook.
        let _ = mw.on_outbound_raw_request(&mut ctx, HttpRequest::default())?;

        // Grab buffer handles before they are unregistered.
        let req_buf = registry
            .get_request_buffer("req-2")
            .ok_or("request buffer must be registered")?;
        let exec_buf = registry
            .get_execution_buffer("exec-2")
            .ok_or("execution buffer must be registered")?;

        // Fire the error hook.
        let err = ConduitError::upstream("provider timeout");
        mw.on_outbound_raw_error(&mut ctx, &err);

        assert!(
            registry.get_request_buffer("req-2").is_none(),
            "request buffer must be unregistered after error"
        );
        assert!(
            registry.get_execution_buffer("exec-2").is_none(),
            "execution buffer must be unregistered after error"
        );
        assert!(req_buf.is_closed(), "request buffer must be closed");
        assert!(exec_buf.is_closed(), "execution buffer must be closed");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test: on_outbound_raw_stream wraps and fans events to execution buffer
    // -----------------------------------------------------------------------

    #[test]
    fn stream_wrapper_fans_events_and_tears_down() -> Result<(), Box<dyn std::error::Error>> {
        let registry = Arc::new(LiveStreamRegistry::new());
        let mw = LivePreviewMiddleware::new(registry.clone());
        let mut ctx = ctx_enabled("req-3", "exec-3");

        // Register buffers.
        let _ = mw.on_outbound_raw_request(&mut ctx, HttpRequest::default())?;

        // Grab the execution buffer handle BEFORE consuming the stream,
        // because teardown will unregister it from the registry.
        let exec_buf = registry
            .get_execution_buffer("exec-3")
            .ok_or("execution buffer must exist after registration")?;

        // Build a mock stream with 3 events.
        let events = vec![event("chunk-1"), event("chunk-2"), event("chunk-3")];
        let stream: BoxEventStream = Box::new(events.into_iter());

        // Wrap the stream.
        let wrapped = mw.on_outbound_raw_stream(&mut ctx, stream)?;

        // Consume all events from the wrapped stream.
        let collected: Vec<StreamEvent> = wrapped.collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].data.as_deref(), Some("chunk-1"));
        assert_eq!(collected[2].data.as_deref(), Some("chunk-3"));

        // The execution buffer should have received all 3 summarized events.
        assert_eq!(
            exec_buf.len(),
            3,
            "execution buffer must have 3 chunks after stream consumption"
        );

        // After stream exhaustion, teardown fires: buffer closed + unregistered.
        assert!(
            exec_buf.is_closed(),
            "execution buffer must be closed after stream exhaustion"
        );
        assert!(
            registry.get_execution_buffer("exec-3").is_none(),
            "execution buffer must be unregistered after stream exhaustion"
        );

        // Verify the request buffer is still registered (not touched by
        // the outbound stream wrapper -- that is the inbound stream's job).
        assert!(
            registry.get_request_buffer("req-3").is_some(),
            "request buffer should survive outbound stream teardown"
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test: disabled middleware is a no-op
    // -----------------------------------------------------------------------

    #[test]
    fn disabled_middleware_is_noop() -> Result<(), Box<dyn std::error::Error>> {
        let registry = Arc::new(LiveStreamRegistry::new());
        let mw = LivePreviewMiddleware::new(registry.clone());

        // Context WITHOUT live_preview_enabled.
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("request_id".to_string(), "req-x".to_string());
        ctx.metadata
            .insert("request_exec_id".to_string(), "exec-x".to_string());

        let _ = mw.on_outbound_raw_request(&mut ctx, HttpRequest::default())?;
        assert!(
            registry.get_request_buffer("req-x").is_none(),
            "disabled middleware must not register buffers"
        );
        assert!(
            registry.get_execution_buffer("exec-x").is_none(),
            "disabled middleware must not register buffers"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test: on_outbound_raw_response writes to buffer and tears down
    // -----------------------------------------------------------------------

    #[test]
    fn response_writes_to_buffers_and_tears_down() -> Result<(), Box<dyn std::error::Error>> {
        let registry = Arc::new(LiveStreamRegistry::new());
        let mw = LivePreviewMiddleware::new(registry.clone());
        let mut ctx = ctx_enabled("req-4", "exec-4");

        // Register buffers.
        let _ = mw.on_outbound_raw_request(&mut ctx, HttpRequest::default())?;

        // Grab handles before teardown.
        let req_buf = registry
            .get_request_buffer("req-4")
            .ok_or("request buffer must exist")?;
        let exec_buf = registry
            .get_execution_buffer("exec-4")
            .ok_or("execution buffer must exist")?;

        let response = HttpResponse {
            status: 200,
            json_body: Some(serde_json::json!({"id": "resp-1"})),
            ..HttpResponse::default()
        };
        let _ = mw.on_outbound_raw_response(&mut ctx, response)?;

        // Buffers should have received 1 chunk each and be closed.
        assert_eq!(req_buf.len(), 1);
        assert_eq!(exec_buf.len(), 1);
        assert!(req_buf.is_closed(), "request buffer must be closed");
        assert!(exec_buf.is_closed(), "execution buffer must be closed");

        // And unregistered.
        assert!(registry.get_request_buffer("req-4").is_none());
        assert!(registry.get_execution_buffer("exec-4").is_none());

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test: stream wrapper with binary events summarizes them
    // -----------------------------------------------------------------------

    #[test]
    fn stream_wrapper_summarizes_binary_events() -> Result<(), Box<dyn std::error::Error>> {
        let registry = Arc::new(LiveStreamRegistry::new());
        let mw = LivePreviewMiddleware::new(registry.clone());
        let mut ctx = ctx_enabled("req-5", "exec-5");

        let _ = mw.on_outbound_raw_request(&mut ctx, HttpRequest::default())?;

        // Grab the execution buffer before it gets unregistered.
        let exec_buf = registry
            .get_execution_buffer("exec-5")
            .ok_or("execution buffer must exist")?;

        // Build a stream with a binary event.
        let mut audio = StreamEvent::default();
        audio.event_type = Some("audio/mpeg".to_string());
        audio.binary = Some(vec![0u8; 128]);
        let stream: BoxEventStream = Box::new(std::iter::once(audio));

        let wrapped = mw.on_outbound_raw_stream(&mut ctx, stream)?;

        // Consume the stream — the consumer receives the FULL binary event.
        let collected: Vec<StreamEvent> = wrapped.collect();
        assert_eq!(collected.len(), 1);
        assert!(
            collected[0].binary.is_some(),
            "consumer must receive the full binary payload"
        );

        // The buffer should have received a summarized copy (no binary, size set).
        let buffered = exec_buf.slice();
        assert_eq!(buffered.len(), 1);
        assert!(
            buffered[0].binary.is_none(),
            "buffered copy must have binary payload removed"
        );
        assert_eq!(
            buffered[0].size,
            Some(128),
            "buffered copy must carry the original binary size"
        );

        Ok(())
    }
}
