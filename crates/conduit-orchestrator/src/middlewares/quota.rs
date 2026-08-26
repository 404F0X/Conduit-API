//! Pipeline middleware that enforces API-key quota limits before outbound
//! execution. Go parity: `enforceQuota` (orchestrator/quota.go:14-66).
//!
//! The Go implementation reads `apiKey.GetActiveProfile().Quota` from a
//! `PersistentInboundTransformer` and delegates to `QuotaService.CheckAPIKeyQuota`.
//! This Rust port reads quota configuration from `PipelineContext.metadata`
//! (set by the HTTP auth middleware) and enforces an in-memory RPM (requests
//! per minute) counter — no external `QuotaService` dependency required.
//!
//! ## Metadata keys consumed
//!
//! - `api_key_id`: the numeric API key ID (required for quota tracking)
//! - `api_key_quota_rpm`: maximum requests per minute (absent = no quota)
//!
//! ## Error shape (Go parity)
//!
//! When the quota is exceeded the middleware returns a 403 with the same
//! error detail structure the Go test asserts (quota_minute_test.go:118-122):
//!
//! ```text
//! StatusCode: 403
//! Code:       "quota_exceeded"
//! Type:       "quota_exceeded_error"
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use conduit_core::ConduitError;
use conduit_llm::LlmRequest;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Tracks the request count and the window start time for a single API key.
#[derive(Debug, Clone)]
struct RpmWindow {
    count: u64,
    window_start: Instant,
}

/// Quota enforcement middleware. Maintains an in-memory per-API-key RPM
/// counter via `Arc<Mutex<…>>` so it can be shared across clones. Go parity:
/// `enforceQuota` (orchestrator/quota.go:14-66).
pub struct QuotaEnforcementMiddleware {
    /// Per-API-key RPM tracking. Key = `api_key_id` (parsed from metadata).
    rpm_counters: Arc<Mutex<HashMap<i64, RpmWindow>>>,
}

impl QuotaEnforcementMiddleware {
    pub fn new() -> Self {
        Self {
            rpm_counters: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for QuotaEnforcementMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineMiddleware for QuotaEnforcementMiddleware {
    fn name(&self) -> &'static str {
        "enforce-quota"
    }

    /// Go `OnInboundLlmRequest` — once per Request, before any outbound
    /// attempt. Checks the RPM counter for the API key; if the quota is
    /// exceeded returns 403 `quota_exceeded`.
    fn on_inbound_llm_request(
        &self,
        ctx: &mut PipelineContext,
        request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        // Read the RPM quota limit from metadata. Absent or empty means
        // "no quota configured" → passthrough (Go: profile.Quota == nil).
        let rpm_str = match ctx.metadata.get("api_key_quota_rpm") {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return Ok(request),
        };

        let rpm_limit: u64 = match rpm_str.parse() {
            Ok(v) if v > 0 => v,
            // Non-numeric or zero limit → no effective quota.
            _ => return Ok(request),
        };

        // API key ID is required for per-key tracking. Missing = passthrough
        // (Go: apiKey == nil → pass).
        let key_id_str = match ctx.metadata.get("api_key_id") {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return Ok(request),
        };

        let api_key_id: i64 = match key_id_str.parse() {
            Ok(v) => v,
            Err(_) => return Ok(request),
        };

        // Lock the RPM counters and check / increment.
        let mut counters = self
            .rpm_counters
            .lock()
            .map_err(|_| ConduitError::internal("quota enforcement: RPM counter lock poisoned"))?;

        let now = Instant::now();
        let window = counters.entry(api_key_id).or_insert_with(|| RpmWindow {
            count: 0,
            window_start: now,
        });

        // If the window has elapsed (>= 60 s), reset it.
        if now.duration_since(window.window_start).as_secs() >= 60 {
            window.count = 0;
            window.window_start = now;
        }

        if window.count >= rpm_limit {
            // Go: returns &llm.ResponseError{StatusCode: 403, Detail: {Code: "quota_exceeded", ...}}
            return Err(ConduitError::forbidden(format!(
                "API key {api_key_id} exceeded the quota of {rpm_limit} requests per minute"
            ))
            .with_code("quota_exceeded"));
        }

        // Under quota — count this request.
        window.count += 1;

        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ChatRequest, LlmRequestPayload};

    fn chat_request(model: &str) -> LlmRequest {
        LlmRequest {
            request_type: conduit_llm::RequestType::Chat,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: Some(model.to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    /// Helper: populate `PipelineContext.metadata` with RPM quota fields.
    fn ctx_with_quota(api_key_id: i64, rpm: u64) -> PipelineContext {
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("api_key_id".to_string(), api_key_id.to_string());
        ctx.metadata
            .insert("api_key_quota_rpm".to_string(), rpm.to_string());
        ctx
    }

    #[test]
    fn under_quota_passes() -> Result<(), Box<dyn std::error::Error>> {
        let mw = QuotaEnforcementMiddleware::new();
        let mut ctx = ctx_with_quota(42, 5);

        // First request should pass.
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4"))?;
        assert_eq!(result.model.as_deref(), Some("gpt-4"));
        Ok(())
    }

    #[test]
    fn over_quota_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let mw = QuotaEnforcementMiddleware::new();
        let mut ctx = ctx_with_quota(42, 2);

        // Use up the quota (2 requests).
        let _ = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4"))?;
        let _ = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4"))?;

        // Third request should be rejected.
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4"));
        assert!(result.is_err(), "third request should be rejected");

        let err = result.err().ok_or("expected error")?;
        // Verify 403 status (Go: http.StatusForbidden).
        assert_eq!(err.http_status, 403);
        // Verify error code (Go: "quota_exceeded").
        assert_eq!(err.code.as_deref(), Some("quota_exceeded"));
        // Verify the message mentions the key and limit.
        assert!(
            err.message.contains("42"),
            "error should mention API key ID, got: {}",
            err.message
        );
        assert!(
            err.message.contains("2"),
            "error should mention the quota limit, got: {}",
            err.message
        );
        Ok(())
    }

    #[test]
    fn no_quota_config_passthrough() -> Result<(), Box<dyn std::error::Error>> {
        let mw = QuotaEnforcementMiddleware::new();

        // No quota metadata at all → passthrough.
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("api_key_id".to_string(), "42".to_string());
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4"))?;
        assert_eq!(result.model.as_deref(), Some("gpt-4"));

        // Empty quota value → passthrough.
        let mut ctx2 = PipelineContext::new();
        ctx2.metadata
            .insert("api_key_id".to_string(), "42".to_string());
        ctx2.metadata
            .insert("api_key_quota_rpm".to_string(), String::new());
        let result2 = mw.on_inbound_llm_request(&mut ctx2, chat_request("gpt-4"))?;
        assert_eq!(result2.model.as_deref(), Some("gpt-4"));

        // No api_key_id → passthrough even with quota set.
        let mut ctx3 = PipelineContext::new();
        ctx3.metadata
            .insert("api_key_quota_rpm".to_string(), "10".to_string());
        let result3 = mw.on_inbound_llm_request(&mut ctx3, chat_request("gpt-4"))?;
        assert_eq!(result3.model.as_deref(), Some("gpt-4"));

        Ok(())
    }
}
