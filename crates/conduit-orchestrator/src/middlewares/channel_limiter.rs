//! Per-channel concurrency limiter middleware — enforces a maximum number of
//! simultaneous in-flight requests per channel.
//!
//! Port of Go `channelLimiterMiddleware`
//! (`conduit/internal/server/orchestrator/connection_tracking.go`, 187 lines).
//! The Go implementation uses a semaphore-like `ChannelLimiter` with
//! acquire/release semantics and per-slot `sync.Once` guards. This simplified
//! Rust version uses a `Mutex<HashMap<String, u32>>` counting active requests
//! per channel_id, suitable for the sync middleware hooks.
//!
//! Metadata keys read:
//! - `channel_id`: identifies the channel for the current attempt.
//! - `channel_max_concurrent`: per-channel override of the default concurrency
//!   limit (parsed as `u32`; ignored if absent or unparsable).
//!
//! Metadata keys written:
//! - `channel_limiter_acquired`: set to `"true"` when a concurrency slot is
//!   acquired, so the response/error hooks know to release it.
//!
//! Hooks overridden:
//! - `on_outbound_raw_request`  — acquire a slot (or reject with 429).
//! - `on_outbound_raw_response` — release the slot.
//! - `on_outbound_raw_error`    — release the slot.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use conduit_core::ConduitError;
use conduit_llm::{HttpRequest, HttpResponse};
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Metadata key read to identify the channel for the current attempt.
const META_CHANNEL_ID: &str = "channel_id";

/// Optional per-channel concurrency limit override in `ctx.metadata`.
const META_CHANNEL_MAX_CONCURRENT: &str = "channel_max_concurrent";

/// Metadata key set when a concurrency slot is successfully acquired, so the
/// response/error hooks know whether to decrement.
const META_LIMITER_ACQUIRED: &str = "channel_limiter_acquired";

/// Shared mutable state: maps `channel_id -> current in-flight count`.
/// Wrapped in `Arc<Mutex<_>>` so the middleware can be cloned / shared across
/// pipeline instances while remaining `Send + Sync`.
type ChannelCounters = Arc<Mutex<HashMap<String, u32>>>;

/// Per-channel concurrency limiter middleware.
///
/// Go parity: `channelLimiterMiddleware`
/// (`orchestrator/connection_tracking.go:41-53`). The Go version embeds a
/// `ChannelLimiterManager` that lazily creates per-channel semaphores. This
/// simplified Rust port keeps a plain counter map and checks against the
/// configured maximum before forwarding the request.
pub struct ChannelConcurrencyMiddleware {
    /// Default maximum concurrent requests per channel. `0` means unlimited.
    default_max_concurrent: u32,
    /// Shared in-flight counters keyed by `channel_id`.
    counters: ChannelCounters,
}

impl ChannelConcurrencyMiddleware {
    /// Create a new middleware with the given default concurrency limit.
    /// A `default_max_concurrent` of `0` disables admission control unless a
    /// per-channel override is present in `ctx.metadata`.
    pub fn new(default_max_concurrent: u32) -> Self {
        Self {
            default_max_concurrent,
            counters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Resolve the effective concurrency limit for the current request.
    /// Per-channel override (`channel_max_concurrent` in metadata) takes
    /// precedence over the struct-level default.
    fn effective_limit(&self, ctx: &PipelineContext) -> u32 {
        ctx.metadata
            .get(META_CHANNEL_MAX_CONCURRENT)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(self.default_max_concurrent)
    }

    /// Decrement the in-flight counter for the channel identified in
    /// `ctx.metadata`, but only if the `channel_limiter_acquired` flag was set
    /// (i.e., we actually acquired a slot on the request path).
    fn release_slot(&self, ctx: &mut PipelineContext) {
        let acquired = ctx
            .metadata
            .get(META_LIMITER_ACQUIRED)
            .map(|v| v == "true")
            .unwrap_or(false);
        if !acquired {
            return;
        }

        // Clear the flag so a double-release (response + error on the same
        // attempt) is harmless.
        ctx.metadata.remove(META_LIMITER_ACQUIRED);

        let channel_id = match ctx.metadata.get(META_CHANNEL_ID) {
            Some(id) => id.clone(),
            None => return,
        };

        if let Ok(mut counts) = self.counters.lock()
            && let Some(count) = counts.get_mut(&channel_id)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&channel_id);
            }
        }
    }
}

impl PipelineMiddleware for ChannelConcurrencyMiddleware {
    fn name(&self) -> &'static str {
        "channel-limiter"
    }

    /// Acquire a concurrency slot for the channel. If the channel is already at
    /// its limit, return a 429 `rate_limited` error. Go parity:
    /// `OnOutboundRawRequest` (connection_tracking.go:64-122).
    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        let max = self.effective_limit(ctx);
        if max == 0 {
            // Unlimited — bypass admission control.
            return Ok(request);
        }

        let channel_id = match ctx.metadata.get(META_CHANNEL_ID) {
            Some(id) => id.clone(),
            // No channel_id in metadata — nothing to limit.
            None => return Ok(request),
        };

        // Lock the counters and check/increment atomically.
        let mut counts = self
            .counters
            .lock()
            .map_err(|_| ConduitError::internal("channel limiter lock poisoned"))?;

        let current = counts.get(&channel_id).copied().unwrap_or(0);
        if current >= max {
            return Err(ConduitError::rate_limited(
                "channel concurrency limit exceeded",
            ));
        }

        counts.insert(channel_id, current + 1);

        // Drop the lock before touching metadata.
        drop(counts);

        ctx.metadata
            .insert(META_LIMITER_ACQUIRED.to_string(), "true".to_string());

        Ok(request)
    }

    /// Release the concurrency slot after a successful response. Go parity:
    /// `OnOutboundLlmResponse` (connection_tracking.go:124-127).
    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        self.release_slot(ctx);
        Ok(response)
    }

    /// Release the concurrency slot after a failed attempt. Go parity:
    /// `OnOutboundRawError` (connection_tracking.go:141-143).
    fn on_outbound_raw_error(&self, ctx: &mut PipelineContext, _error: &ConduitError) {
        self.release_slot(ctx);
    }

    fn on_outbound_live_stream_close(&self, ctx: &mut PipelineContext) {
        self.release_slot(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a `PipelineContext` pre-populated with a `channel_id`.
    fn ctx_with_channel(channel_id: &str) -> PipelineContext {
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert(META_CHANNEL_ID.to_string(), channel_id.to_string());
        ctx
    }

    /// Helper: build a minimal `HttpRequest`.
    fn dummy_request() -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            ..HttpRequest::default()
        }
    }

    // ---- Test 1: under limit passes ----------------------------------------

    #[test]
    fn under_limit_passes() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ChannelConcurrencyMiddleware::new(2);
        let mut ctx = ctx_with_channel("ch-1");

        // First request — should pass (1/2).
        let result = mw.on_outbound_raw_request(&mut ctx, dummy_request());
        assert!(result.is_ok(), "first request must be admitted");
        assert_eq!(
            ctx.metadata.get(META_LIMITER_ACQUIRED).map(|s| s.as_str()),
            Some("true"),
            "acquired flag must be set after successful admission",
        );

        Ok(())
    }

    // ---- Test 2: at limit rejects ------------------------------------------

    #[test]
    fn at_limit_rejects() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ChannelConcurrencyMiddleware::new(1);

        // Saturate the single slot.
        let mut ctx1 = ctx_with_channel("ch-1");
        let _ = mw.on_outbound_raw_request(&mut ctx1, dummy_request())?;

        // Second request on the same channel must be rejected (429).
        let mut ctx2 = ctx_with_channel("ch-1");
        let err = mw.on_outbound_raw_request(&mut ctx2, dummy_request());
        assert!(err.is_err(), "second request must be rejected at limit");

        let conduit_err = err.err().ok_or("expected error")?;
        assert_eq!(conduit_err.http_status, 429);
        assert!(
            conduit_err
                .message
                .contains("channel concurrency limit exceeded"),
            "error message must indicate concurrency limit: {}",
            conduit_err.message,
        );

        // The rejected request must NOT have set the acquired flag.
        assert!(
            ctx2.metadata.get(META_LIMITER_ACQUIRED).is_none(),
            "acquired flag must NOT be set when admission is rejected",
        );

        Ok(())
    }

    // ---- Test 3: release after response ------------------------------------

    #[test]
    fn release_after_response() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ChannelConcurrencyMiddleware::new(1);

        // Admit one request.
        let mut ctx1 = ctx_with_channel("ch-1");
        let _ = mw.on_outbound_raw_request(&mut ctx1, dummy_request())?;

        // Release via response hook.
        let response = HttpResponse::default();
        let _ = mw.on_outbound_raw_response(&mut ctx1, response)?;

        // Now a new request on the same channel must succeed (slot freed).
        let mut ctx2 = ctx_with_channel("ch-1");
        let result = mw.on_outbound_raw_request(&mut ctx2, dummy_request());
        assert!(result.is_ok(), "request after release must be admitted");

        Ok(())
    }

    // ---- Test 4: release after error ---------------------------------------

    #[test]
    fn release_after_error() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ChannelConcurrencyMiddleware::new(1);

        // Admit one request.
        let mut ctx1 = ctx_with_channel("ch-1");
        let _ = mw.on_outbound_raw_request(&mut ctx1, dummy_request())?;

        // Release via error hook.
        let error = ConduitError::upstream("provider error");
        mw.on_outbound_raw_error(&mut ctx1, &error);

        // Now a new request on the same channel must succeed (slot freed).
        let mut ctx2 = ctx_with_channel("ch-1");
        let result = mw.on_outbound_raw_request(&mut ctx2, dummy_request());
        assert!(
            result.is_ok(),
            "request after error-release must be admitted",
        );

        Ok(())
    }

    #[test]
    fn release_after_live_stream_close() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ChannelConcurrencyMiddleware::new(1);

        let mut streaming = ctx_with_channel("ch-1");
        let _ = mw.on_outbound_raw_request(&mut streaming, dummy_request())?;

        let mut blocked = ctx_with_channel("ch-1");
        assert!(
            mw.on_outbound_raw_request(&mut blocked, dummy_request())
                .is_err(),
            "the permit must remain held while the live stream is open"
        );

        mw.on_outbound_live_stream_close(&mut streaming);

        let mut next = ctx_with_channel("ch-1");
        assert!(
            mw.on_outbound_raw_request(&mut next, dummy_request())
                .is_ok(),
            "closing a live stream must release its channel permit"
        );
        Ok(())
    }
}
