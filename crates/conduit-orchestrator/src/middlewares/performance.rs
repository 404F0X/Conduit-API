//! Pipeline middleware that records performance timing metrics via
//! `PipelineContext.metadata`. Simplified port of Go `performanceRecording`
//! (`conduit/internal/server/orchestrator/performance.go`) — stores timing
//! data as metadata strings instead of requiring `PersistenceState`.
//!
//! Metric keys:
//! - `perf_start_ms`: epoch millis when the inbound LLM request arrived
//! - `perf_outbound_start_ms`: epoch millis when the outbound raw request
//!   was dispatched
//! - `perf_latency_ms`: outbound round-trip latency (response - outbound_start)
//! - `perf_completion_tokens`: completion token count from response json_body
//! - `perf_prompt_tokens`: prompt token count from response json_body
//! - `perf_error_ms`: epoch millis when an outbound error was observed

use std::time::{SystemTime, UNIX_EPOCH};

use conduit_core::ConduitError;
use conduit_llm::{HttpRequest, HttpResponse, LlmRequest};
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Returns current epoch milliseconds as a `String`, or `"0"` if the system
/// clock is before the Unix epoch (should never happen in practice).
fn epoch_millis_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Simplified performance recording middleware. Zero-size — all state lives in
/// `PipelineContext.metadata`. Go parity: `performanceRecording`
/// (`orchestrator/performance.go:26-31`), minus the `PersistenceState`
/// dependency.
pub struct PerformanceRecordingMiddleware;

impl PipelineMiddleware for PerformanceRecordingMiddleware {
    fn name(&self) -> &'static str {
        "record-performance"
    }

    /// Record the request start timestamp.
    /// Go: `OnInboundLlmRequest` (performance.go:36-48).
    fn on_inbound_llm_request(
        &self,
        ctx: &mut PipelineContext,
        request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        ctx.metadata
            .insert("perf_start_ms".to_string(), epoch_millis_string());
        Ok(request)
    }

    /// Record the outbound request dispatch timestamp.
    /// Go: `OnOutboundRawRequest` (performance.go:50-84).
    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        ctx.metadata
            .insert("perf_outbound_start_ms".to_string(), epoch_millis_string());
        Ok(request)
    }

    /// Compute outbound latency and optionally extract token counts from the
    /// response json_body. Go: `OnOutboundRawResponse` + token extraction
    /// from `OnOutboundLlmResponse` (performance.go:86-105).
    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        let now = epoch_millis_string();

        // Compute latency from outbound start.
        if let Some(start_str) = ctx.metadata.get("perf_outbound_start_ms")
            && let Ok(start) = start_str.parse::<u128>()
            && let Ok(now_val) = now.parse::<u128>()
        {
            let latency = now_val.saturating_sub(start);
            ctx.metadata
                .insert("perf_latency_ms".to_string(), latency.to_string());
        }

        // Extract token counts from response json_body if present.
        // OpenAI-shaped responses carry `usage.completion_tokens` and
        // `usage.prompt_tokens` at the top level.
        if let Some(ref body) = response.json_body
            && let Some(usage) = body.get("usage")
        {
            if let Some(ct) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
                ctx.metadata
                    .insert("perf_completion_tokens".to_string(), ct.to_string());
            }
            if let Some(pt) = usage.get("prompt_tokens").and_then(|v| v.as_i64()) {
                ctx.metadata
                    .insert("perf_prompt_tokens".to_string(), pt.to_string());
            }
        }

        Ok(response)
    }

    /// Record the error timestamp.
    /// Go: `OnOutboundRawError` (performance.go:119-134).
    fn on_outbound_raw_error(&self, ctx: &mut PipelineContext, _error: &ConduitError) {
        ctx.metadata
            .insert("perf_error_ms".to_string(), epoch_millis_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_request_records_start_time() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PerformanceRecordingMiddleware;
        let mut ctx = PipelineContext::new();
        let request = LlmRequest {
            request_type: conduit_llm::RequestType::Chat,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: None,
            stream: false,
            payload: conduit_llm::LlmRequestPayload::Chat(conduit_llm::ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };

        let _ = mw.on_inbound_llm_request(&mut ctx, request)?;

        let start = ctx.metadata.get("perf_start_ms");
        assert!(start.is_some(), "perf_start_ms must be set");
        // Value must be a parseable positive integer.
        let ms: u128 = start.map(|s| s.parse()).transpose()?.unwrap_or(0);
        assert!(ms > 0, "perf_start_ms must be a positive epoch value");
        Ok(())
    }

    #[test]
    fn outbound_request_records_outbound_start() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PerformanceRecordingMiddleware;
        let mut ctx = PipelineContext::new();
        let request = HttpRequest::default();

        let _ = mw.on_outbound_raw_request(&mut ctx, request)?;

        let start = ctx.metadata.get("perf_outbound_start_ms");
        assert!(start.is_some(), "perf_outbound_start_ms must be set");
        let ms: u128 = start.map(|s| s.parse()).transpose()?.unwrap_or(0);
        assert!(ms > 0, "perf_outbound_start_ms must be positive");
        Ok(())
    }

    #[test]
    fn outbound_response_computes_latency_and_extracts_tokens()
    -> Result<(), Box<dyn std::error::Error>> {
        let mw = PerformanceRecordingMiddleware;
        let mut ctx = PipelineContext::new();

        // Simulate an earlier outbound start (1000 ms ago is fine — latency
        // will be >= 0 since wall-clock has moved forward).
        let fake_start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis().saturating_sub(1000))
            .unwrap_or(0);
        ctx.metadata
            .insert("perf_outbound_start_ms".to_string(), fake_start.to_string());

        // Build a response with OpenAI-shaped usage in json_body.
        let mut response = HttpResponse::default();
        response.json_body = Some(serde_json::json!({
            "usage": {
                "completion_tokens": 42,
                "prompt_tokens": 100
            }
        }));

        let _ = mw.on_outbound_raw_response(&mut ctx, response)?;

        // Latency must be recorded and >= 1000 (we set the start 1000ms ago).
        let latency_str = ctx.metadata.get("perf_latency_ms");
        assert!(latency_str.is_some(), "perf_latency_ms must be set");
        let latency: u128 = latency_str.map(|s| s.parse()).transpose()?.unwrap_or(0);
        assert!(
            latency >= 1000,
            "latency should be >= 1000ms, got {latency}"
        );

        // Token counts must be extracted.
        assert_eq!(
            ctx.metadata
                .get("perf_completion_tokens")
                .map(|s| s.as_str()),
            Some("42")
        );
        assert_eq!(
            ctx.metadata.get("perf_prompt_tokens").map(|s| s.as_str()),
            Some("100")
        );
        Ok(())
    }

    #[test]
    fn outbound_error_records_error_timestamp() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PerformanceRecordingMiddleware;
        let mut ctx = PipelineContext::new();
        let error = ConduitError::upstream("test error");

        mw.on_outbound_raw_error(&mut ctx, &error);

        let err_ms = ctx.metadata.get("perf_error_ms");
        assert!(err_ms.is_some(), "perf_error_ms must be set");
        let ms: u128 = err_ms.map(|s| s.parse()).transpose()?.unwrap_or(0);
        assert!(ms > 0, "perf_error_ms must be a positive epoch value");
        Ok(())
    }
}
