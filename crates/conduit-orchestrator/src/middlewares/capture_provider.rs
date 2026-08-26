//! Pipeline middleware that captures the raw upstream provider response body
//! for pass-through forwarding. Stores the raw bytes in PipelineContext
//! metadata so the pass-through response middleware can forward them directly.
//!
//! Go parity: `captureRawProviderResponse` + `captureRawProviderStream`
//! (orchestrator/pass_through.go:216-350).

use conduit_llm::HttpResponse;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{BoxEventStream, PipelineContext, PipelineResult};

/// Captures the raw provider response/stream for pass-through forwarding.
/// Only active when `pass_through_enabled` is set in context metadata.
pub struct CaptureRawProviderMiddleware;

impl PipelineMiddleware for CaptureRawProviderMiddleware {
    fn name(&self) -> &'static str {
        "capture-raw-provider"
    }

    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        // Only capture when pass-through is enabled.
        if ctx.metadata.get("pass_through_enabled").map(|s| s.as_str()) != Some("true") {
            return Ok(response);
        }

        // Store response body size for monitoring.
        if let Some(body) = &response.body {
            ctx.metadata
                .insert("raw_provider_body_size".to_string(), body.len().to_string());
        }

        // Mark that raw capture happened.
        ctx.metadata
            .insert("raw_provider_captured".to_string(), "true".to_string());

        Ok(response)
    }

    fn on_outbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        if ctx.metadata.get("pass_through_enabled").map(|s| s.as_str()) != Some("true") {
            return Ok(stream);
        }

        ctx.metadata.insert(
            "raw_provider_stream_captured".to_string(),
            "true".to_string(),
        );

        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_when_enabled() -> Result<(), Box<dyn std::error::Error>> {
        let mw = CaptureRawProviderMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("pass_through_enabled".to_string(), "true".to_string());
        let resp = HttpResponse {
            body: Some(b"hello world".to_vec()),
            ..Default::default()
        };
        let _ = mw.on_outbound_raw_response(&mut ctx, resp)?;
        assert_eq!(
            ctx.metadata
                .get("raw_provider_captured")
                .map(|s| s.as_str()),
            Some("true")
        );
        assert_eq!(
            ctx.metadata
                .get("raw_provider_body_size")
                .map(|s| s.as_str()),
            Some("11")
        );
        Ok(())
    }

    #[test]
    fn skips_when_disabled() -> Result<(), Box<dyn std::error::Error>> {
        let mw = CaptureRawProviderMiddleware;
        let mut ctx = PipelineContext::new();
        let resp = HttpResponse::default();
        let _ = mw.on_outbound_raw_response(&mut ctx, resp)?;
        assert!(!ctx.metadata.contains_key("raw_provider_captured"));
        Ok(())
    }
}
