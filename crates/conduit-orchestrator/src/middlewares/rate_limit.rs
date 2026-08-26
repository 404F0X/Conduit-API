//! Pipeline middleware that records rate-limit-relevant metadata after each
//! upstream response. Simplified port of Go `withRateLimitTracking`
//! (`conduit/internal/server/orchestrator/rate_limit_tracking.go`) — stores
//! provider rate-limit headers and timestamps in `PipelineContext.metadata`
//! without requiring a real `ChannelRequestTracker`. A future full tracker
//! can consume these metadata entries directly.
//!
//! Metadata keys:
//! - `rate_limit_response_time_ms`: epoch millis when the response arrived
//! - `rate_limit_remaining_tokens`: value of `x-ratelimit-remaining-tokens`
//!   response header (if present)
//! - `rate_limit_remaining_requests`: value of `x-ratelimit-remaining-requests`
//!   response header (if present)
//! - `rate_limit_retry_after`: value of `retry-after` response header
//!   (if present)
//! - `rate_limit_channel_id`: copied from `channel_id` in ctx.metadata
//!   (if present)
//! - `rate_limit_error_time_ms`: epoch millis when an outbound error was
//!   observed (for cooldown tracking)

use std::time::{SystemTime, UNIX_EPOCH};

use conduit_core::ConduitError;
use conduit_llm::HttpResponse;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Returns current epoch milliseconds as a `String`, or `"0"` if the system
/// clock is before the Unix epoch.
fn epoch_millis_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Simplified rate-limit tracking middleware. Zero-size — all state lives in
/// `PipelineContext.metadata`. Go parity: `rateLimitTracking`
/// (`orchestrator/rate_limit_tracking.go:30-35`), minus the
/// `ChannelRequestTracker` / `PersistentOutboundTransformer` dependencies.
pub struct RateLimitTrackingMiddleware;

impl PipelineMiddleware for RateLimitTrackingMiddleware {
    fn name(&self) -> &'static str {
        "track-rate-limit"
    }

    /// Record response timestamp, extract provider rate-limit headers, and
    /// copy the channel ID for downstream correlation.
    /// Go: `OnOutboundLlmResponse` / `OnOutboundRawError`
    /// (rate_limit_tracking.go:41-62, :75-109).
    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        // Record when we received the response.
        ctx.metadata.insert(
            "rate_limit_response_time_ms".to_string(),
            epoch_millis_string(),
        );

        // Extract rate-limit headers from the provider response.
        if let Some(val) = response.headers.get("x-ratelimit-remaining-tokens") {
            ctx.metadata
                .insert("rate_limit_remaining_tokens".to_string(), val.clone());
        }
        if let Some(val) = response.headers.get("x-ratelimit-remaining-requests") {
            ctx.metadata
                .insert("rate_limit_remaining_requests".to_string(), val.clone());
        }
        if let Some(val) = response.headers.get("retry-after") {
            ctx.metadata
                .insert("rate_limit_retry_after".to_string(), val.clone());
        }

        // Copy channel_id from existing metadata for downstream correlation.
        if let Some(ch_id) = ctx.metadata.get("channel_id").cloned() {
            ctx.metadata
                .insert("rate_limit_channel_id".to_string(), ch_id);
        }

        Ok(response)
    }

    /// Record the error timestamp for cooldown tracking.
    /// Go: `OnOutboundRawError` (rate_limit_tracking.go:75-109).
    fn on_outbound_raw_error(&self, ctx: &mut PipelineContext, _error: &ConduitError) {
        ctx.metadata.insert(
            "rate_limit_error_time_ms".to_string(),
            epoch_millis_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbound_response_records_timestamp_and_headers() -> Result<(), Box<dyn std::error::Error>> {
        let mw = RateLimitTrackingMiddleware;
        let mut ctx = PipelineContext::new();

        // Pre-populate channel_id so the middleware can copy it.
        ctx.metadata
            .insert("channel_id".to_string(), "42".to_string());

        // Build a response with rate-limit headers.
        let mut response = HttpResponse::default();
        response.headers.insert(
            "x-ratelimit-remaining-tokens".to_string(),
            "5000".to_string(),
        );
        response.headers.insert(
            "x-ratelimit-remaining-requests".to_string(),
            "100".to_string(),
        );
        response
            .headers
            .insert("retry-after".to_string(), "30".to_string());

        let _ = mw.on_outbound_raw_response(&mut ctx, response)?;

        // Response timestamp must be a positive epoch value.
        let ts = ctx.metadata.get("rate_limit_response_time_ms");
        assert!(ts.is_some(), "rate_limit_response_time_ms must be set");
        let ms: u128 = ts.map(|s| s.parse()).transpose()?.unwrap_or(0);
        assert!(ms > 0, "rate_limit_response_time_ms must be positive");

        // Rate-limit headers must be extracted.
        assert_eq!(
            ctx.metadata
                .get("rate_limit_remaining_tokens")
                .map(|s| s.as_str()),
            Some("5000"),
        );
        assert_eq!(
            ctx.metadata
                .get("rate_limit_remaining_requests")
                .map(|s| s.as_str()),
            Some("100"),
        );
        assert_eq!(
            ctx.metadata
                .get("rate_limit_retry_after")
                .map(|s| s.as_str()),
            Some("30"),
        );

        // Channel ID must be copied.
        assert_eq!(
            ctx.metadata
                .get("rate_limit_channel_id")
                .map(|s| s.as_str()),
            Some("42"),
        );

        Ok(())
    }

    #[test]
    fn outbound_response_without_headers_only_records_timestamp()
    -> Result<(), Box<dyn std::error::Error>> {
        let mw = RateLimitTrackingMiddleware;
        let mut ctx = PipelineContext::new();
        let response = HttpResponse::default();

        let _ = mw.on_outbound_raw_response(&mut ctx, response)?;

        // Timestamp must still be set.
        let ts = ctx.metadata.get("rate_limit_response_time_ms");
        assert!(ts.is_some(), "rate_limit_response_time_ms must be set");
        let ms: u128 = ts.map(|s| s.parse()).transpose()?.unwrap_or(0);
        assert!(ms > 0, "rate_limit_response_time_ms must be positive");

        // No rate-limit headers → no metadata entries for them.
        assert!(
            !ctx.metadata.contains_key("rate_limit_remaining_tokens"),
            "remaining_tokens should not be set without header",
        );
        assert!(
            !ctx.metadata.contains_key("rate_limit_remaining_requests"),
            "remaining_requests should not be set without header",
        );
        assert!(
            !ctx.metadata.contains_key("rate_limit_retry_after"),
            "retry_after should not be set without header",
        );
        // No channel_id in metadata → no rate_limit_channel_id.
        assert!(
            !ctx.metadata.contains_key("rate_limit_channel_id"),
            "channel_id should not be set without source",
        );

        Ok(())
    }

    #[test]
    fn outbound_error_records_error_timestamp() -> Result<(), Box<dyn std::error::Error>> {
        let mw = RateLimitTrackingMiddleware;
        let mut ctx = PipelineContext::new();
        let error = ConduitError::upstream("rate limited");

        mw.on_outbound_raw_error(&mut ctx, &error);

        let err_ms = ctx.metadata.get("rate_limit_error_time_ms");
        assert!(err_ms.is_some(), "rate_limit_error_time_ms must be set");
        let ms: u128 = err_ms.map(|s| s.parse()).transpose()?.unwrap_or(0);
        assert!(
            ms > 0,
            "rate_limit_error_time_ms must be a positive epoch value"
        );

        Ok(())
    }
}
