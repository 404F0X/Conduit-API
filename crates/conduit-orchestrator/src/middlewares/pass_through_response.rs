//! Pipeline middleware that applies pass-through response forwarding —
//! when enabled, the raw provider response is forwarded directly to the
//! client, bypassing the response-transform chain (Outbound.TransformResponse
//! + Inbound.TransformResponse).
//!
//! Go parity: `applyPassThroughResponse` (pass_through.go:228-250).
//! The full version checks channel settings; this simplified version
//! reads `pass_through_enabled` from context metadata.

use conduit_llm::HttpResponse;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// When pass-through is enabled, marks the response so the pipeline
/// skips the unified transform chain and forwards the raw body.
pub struct PassThroughResponseMiddleware;

impl PipelineMiddleware for PassThroughResponseMiddleware {
    fn name(&self) -> &'static str {
        "pass-through-response"
    }

    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        if ctx.metadata.get("pass_through_enabled").map(|s| s.as_str()) != Some("true") {
            return Ok(response);
        }

        // Flag that pass-through response is active — the pipeline's
        // finish_non_stream_response can check this flag to skip the
        // Outbound.transform_response → Inbound.transform_response chain.
        ctx.metadata.insert(
            "pass_through_response_active".to_string(),
            "true".to_string(),
        );

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_does_not_flag() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PassThroughResponseMiddleware;
        let mut ctx = PipelineContext::new();
        let _ = mw.on_outbound_raw_response(&mut ctx, HttpResponse::default())?;
        assert!(!ctx.metadata.contains_key("pass_through_response_active"));
        Ok(())
    }

    #[test]
    fn enabled_flags_response() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PassThroughResponseMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("pass_through_enabled".to_string(), "true".to_string());
        let _ = mw.on_outbound_raw_response(&mut ctx, HttpResponse::default())?;
        assert_eq!(
            ctx.metadata
                .get("pass_through_response_active")
                .map(|s| s.as_str()),
            Some("true")
        );
        Ok(())
    }
}
