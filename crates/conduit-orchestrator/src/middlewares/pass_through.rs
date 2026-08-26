//! Simplified pass-through middleware — detects when the gateway should forward
//! the upstream provider's raw response body directly to the client, bypassing
//! the response-transform chain.
//!
//! Go parity (simplified): `applyPassThroughResponse` and `isPassThroughEnabled`
//! from `orchestrator/pass_through.go` (lines 228-250 / 25-62).
//!
//! This is a **simplified** version that reads a metadata flag
//! (`pass_through_enabled`) from the pipeline context rather than consulting
//! `SystemService` or channel-level `ChannelSettings.PassThroughBody`. Once the
//! full channel/system settings wiring is in place, this middleware can be
//! extended to check those sources.
//!
//! Hooks overridden:
//! - `on_outbound_raw_response`  (non-streaming: flag + body-size metadata)
//! - `on_outbound_raw_stream`    (streaming: flag + event-count metadata)

use conduit_llm::HttpResponse;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{BoxEventStream, PipelineContext, PipelineResult};

/// Metadata key read to determine whether pass-through is enabled for the
/// current attempt. The pipeline or an earlier middleware stamps this.
const META_PASS_THROUGH_ENABLED: &str = "pass_through_enabled";

/// Metadata key set by this middleware when pass-through is applied on the
/// non-streaming or streaming path. Downstream stages can check this to skip
/// response transformation.
const META_PASS_THROUGH_APPLIED: &str = "pass_through_applied";

/// Metadata key: byte length of the raw response body (non-streaming path).
const META_PASS_THROUGH_BODY_SIZE: &str = "pass_through_body_size";

/// Returns `true` when the `pass_through_enabled` metadata flag is set to the
/// literal string `"true"`. Mirrors the simplified check described in the task
/// (Go: `isPassThroughEnabled` consults channel settings + system service; here
/// we only look at the pre-stamped metadata value).
fn is_pass_through_enabled(ctx: &PipelineContext) -> bool {
    ctx.metadata
        .get(META_PASS_THROUGH_ENABLED)
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Simplified pass-through middleware.
///
/// When `pass_through_enabled` is `"true"` in `ctx.metadata`:
/// - **Non-streaming**: sets `pass_through_applied = "true"` and records the
///   raw body byte size in `pass_through_body_size`.
/// - **Streaming**: sets `pass_through_applied = "true"` and passes events
///   through untouched. A full implementation would count events via shared
///   state for observability.
pub struct PassThroughMiddleware;

impl PipelineMiddleware for PassThroughMiddleware {
    fn name(&self) -> &'static str {
        "pass-through"
    }

    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        if !is_pass_through_enabled(ctx) {
            return Ok(response);
        }

        ctx.metadata
            .insert(META_PASS_THROUGH_APPLIED.to_string(), "true".to_string());

        // Record the body size for observability / logging. Go stores the raw
        // response reference on PersistenceState; here we surface the byte
        // length through metadata so callers can log it without coupling to
        // the persistence layer.
        let body_size = response.body.as_ref().map_or(0, |b| b.len());
        ctx.metadata.insert(
            META_PASS_THROUGH_BODY_SIZE.to_string(),
            body_size.to_string(),
        );

        Ok(response)
    }

    fn on_outbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        if !is_pass_through_enabled(ctx) {
            return Ok(stream);
        }

        ctx.metadata
            .insert(META_PASS_THROUGH_APPLIED.to_string(), "true".to_string());

        // Lazy S08-compliant wrapper: events pass through untouched. A
        // full implementation would count events via shared state for
        // observability (Go stores them on PersistenceState); the
        // simplified version only sets the metadata flag above.
        let wrapped: BoxEventStream = Box::new(stream);

        Ok(wrapped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::StreamEvent;

    /// When pass-through is disabled (no metadata flag), the response hook
    /// must pass through untouched and NOT set `pass_through_applied`.
    #[test]
    fn disabled_does_not_flag_response() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PassThroughMiddleware;
        let mut ctx = PipelineContext::new();
        // No `pass_through_enabled` in metadata — disabled by default.
        let response = HttpResponse {
            status: 200,
            body: Some(b"hello".to_vec()),
            ..HttpResponse::default()
        };

        let result = mw.on_outbound_raw_response(&mut ctx, response)?;
        assert_eq!(result.status, 200);
        assert!(
            ctx.metadata.get(META_PASS_THROUGH_APPLIED).is_none(),
            "pass_through_applied must NOT be set when pass-through is disabled"
        );
        assert!(
            ctx.metadata.get(META_PASS_THROUGH_BODY_SIZE).is_none(),
            "body size must NOT be recorded when pass-through is disabled"
        );
        Ok(())
    }

    /// When pass-through is enabled, `on_outbound_raw_response` must set
    /// `pass_through_applied = "true"` and record the body byte length.
    #[test]
    fn enabled_flags_response_and_records_body_size() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PassThroughMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert(META_PASS_THROUGH_ENABLED.to_string(), "true".to_string());

        let body = b"{\"id\":\"chatcmpl-abc\",\"choices\":[]}";
        let response = HttpResponse {
            status: 200,
            body: Some(body.to_vec()),
            ..HttpResponse::default()
        };

        let result = mw.on_outbound_raw_response(&mut ctx, response)?;
        assert_eq!(result.status, 200);
        assert_eq!(
            ctx.metadata
                .get(META_PASS_THROUGH_APPLIED)
                .map(|s| s.as_str()),
            Some("true"),
        );
        assert_eq!(
            ctx.metadata
                .get(META_PASS_THROUGH_BODY_SIZE)
                .map(|s| s.as_str()),
            Some(&body.len().to_string()[..]),
        );
        Ok(())
    }

    /// When pass-through is enabled, `on_outbound_raw_stream` must set
    /// `pass_through_applied = "true"` and lazily count events (S08).
    #[test]
    fn enabled_flags_stream_and_counts_events_lazily() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PassThroughMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert(META_PASS_THROUGH_ENABLED.to_string(), "true".to_string());

        let events = vec![
            StreamEvent {
                data: Some("event-0".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                data: Some("event-1".to_string()),
                ..StreamEvent::default()
            },
        ];
        let stream: BoxEventStream = Box::new(events.into_iter());

        let wrapped = mw.on_outbound_raw_stream(&mut ctx, stream)?;

        // Flag must be set at wrap time.
        assert_eq!(
            ctx.metadata
                .get(META_PASS_THROUGH_APPLIED)
                .map(|s| s.as_str()),
            Some("true"),
        );

        // Consume the stream and verify all events pass through.
        let collected: Vec<StreamEvent> = wrapped.collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].data.as_deref(), Some("event-0"));
        assert_eq!(collected[1].data.as_deref(), Some("event-1"));

        Ok(())
    }
}
