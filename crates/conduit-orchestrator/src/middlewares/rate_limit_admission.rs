//! Pipeline middleware that enforces RPM (requests per minute) admission
//! control using an in-memory sliding window counter.
//!
//! Go parity: `withRateLimitAdmission` (orchestrator/rate_limit_admission.go:74).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use conduit_core::ConduitError;
use conduit_llm::HttpRequest;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Per-channel RPM admission: rejects requests when the channel's RPM
/// budget is exhausted. Uses a simple sliding-window counter.
pub struct RateLimitAdmissionMiddleware {
    /// Map: channel_id → (request_count_this_window, window_start).
    counters: Arc<Mutex<HashMap<String, (u32, Instant)>>>,
}

impl Default for RateLimitAdmissionMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitAdmissionMiddleware {
    pub fn new() -> Self {
        Self {
            counters: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl PipelineMiddleware for RateLimitAdmissionMiddleware {
    fn name(&self) -> &'static str {
        "rate-limit-admission"
    }

    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        // Read RPM limit from context (set per-channel or global).
        let rpm_limit: u32 = match ctx
            .metadata
            .get("channel_rpm_limit")
            .and_then(|v| v.parse().ok())
        {
            Some(limit) if limit > 0 => limit,
            _ => return Ok(request), // no limit configured
        };

        let channel_id = ctx.metadata.get("channel_id").cloned().unwrap_or_default();
        if channel_id.is_empty() {
            return Ok(request);
        }

        let now = Instant::now();
        let window = std::time::Duration::from_secs(60);

        let mut counters = self
            .counters
            .lock()
            .map_err(|_| ConduitError::internal("rate limit counter lock poisoned"))?;

        let entry = counters.entry(channel_id).or_insert((0, now));

        // Reset window if expired.
        if now.duration_since(entry.1) >= window {
            entry.0 = 0;
            entry.1 = now;
        }

        if entry.0 >= rpm_limit {
            return Err(ConduitError::new(
                conduit_core::ErrorKind::RateLimited,
                "rate limit exceeded: requests per minute quota exhausted",
            ));
        }

        entry.0 += 1;
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> HttpRequest {
        HttpRequest::default()
    }

    #[test]
    fn no_limit_passes() -> Result<(), Box<dyn std::error::Error>> {
        let mw = RateLimitAdmissionMiddleware::new();
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("channel_id".to_string(), "ch1".to_string());
        let _ = mw.on_outbound_raw_request(&mut ctx, request())?;
        Ok(())
    }

    #[test]
    fn under_limit_passes() -> Result<(), Box<dyn std::error::Error>> {
        let mw = RateLimitAdmissionMiddleware::new();
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("channel_id".to_string(), "ch1".to_string());
        ctx.metadata
            .insert("channel_rpm_limit".to_string(), "10".to_string());
        for _ in 0..10 {
            let _ = mw.on_outbound_raw_request(&mut ctx, request())?;
        }
        Ok(())
    }

    #[test]
    fn over_limit_rejects() {
        let mw = RateLimitAdmissionMiddleware::new();
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("channel_id".to_string(), "ch1".to_string());
        ctx.metadata
            .insert("channel_rpm_limit".to_string(), "2".to_string());
        let _ = mw.on_outbound_raw_request(&mut ctx, request());
        let _ = mw.on_outbound_raw_request(&mut ctx, request());
        let result = mw.on_outbound_raw_request(&mut ctx, request());
        assert!(result.is_err());
    }
}
