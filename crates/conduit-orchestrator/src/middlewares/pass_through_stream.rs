//! Pipeline middleware that applies pass-through stream forwarding. When
//! pass-through is enabled and a `CaptureStreamMiddleware` has populated the
//! shared event buffer, this middleware replaces the inbound stream with the
//! captured raw provider events so the client receives the untransformed
//! upstream data.
//!
//! Go parity: `applyPassThroughStream` (pass_through.go:349-381).
//!
//! The Go implementation reads from `state.RawStreamCh` (the side channel
//! populated by `captureRawProviderStream`) and spawns a goroutine to drain
//! the transformed pipeline stream so LLM middlewares still process events.
//! In the Rust port, because the iterator model is synchronous, we drain the
//! original inbound stream eagerly (mirroring the Go goroutine drain) and
//! then yield the captured events from the shared buffer.
//!
//! Hook overridden: `on_inbound_raw_stream` (FORWARD order, once per
//! successful streaming Request).

use conduit_llm::StreamEvent;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{BoxEventStream, PipelineContext, PipelineResult};

use super::capture_stream::CapturedEvents;

/// Metadata key consulted to determine whether pass-through is enabled.
const META_PASS_THROUGH_ENABLED: &str = "pass_through_enabled";

/// Metadata key set by `CaptureStreamMiddleware` when the capture wrapper is
/// installed. Checked here to confirm events were actually captured.
const META_STREAM_CAPTURE_ACTIVE: &str = "stream_capture_active";

/// Metadata key set when this middleware replaces the stream with captured
/// events, so downstream stages know pass-through stream is in effect.
const META_PASS_THROUGH_STREAM_APPLIED: &str = "pass_through_stream_applied";

/// Applies pass-through stream forwarding.
///
/// Holds a reference to the same `CapturedEvents` buffer used by
/// `CaptureStreamMiddleware`. When both pass-through is enabled and the buffer
/// contains captured events, the inbound stream is replaced with the captured
/// events. The original inbound stream is drained to ensure upstream
/// middleware wrappers (performance recording, rate limit tracking, etc.)
/// still process their per-event logic.
pub struct PassThroughStreamMiddleware {
    /// Shared buffer populated by `CaptureStreamMiddleware`.
    pub captured: CapturedEvents,
}

impl PassThroughStreamMiddleware {
    /// Creates a new middleware that reads from the given captured-events
    /// buffer (the same `Arc` given to `CaptureStreamMiddleware`).
    pub fn new(captured: CapturedEvents) -> Self {
        Self { captured }
    }
}

impl PipelineMiddleware for PassThroughStreamMiddleware {
    fn name(&self) -> &'static str {
        "pass-through-response-stream"
    }

    fn on_inbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        // Gate 1: pass-through must be enabled.
        if ctx
            .metadata
            .get(META_PASS_THROUGH_ENABLED)
            .map(|s| s.as_str())
            != Some("true")
        {
            return Ok(stream);
        }

        // Gate 2: capture must have been active (CaptureStreamMiddleware ran).
        if ctx
            .metadata
            .get(META_STREAM_CAPTURE_ACTIVE)
            .map(|s| s.as_str())
            != Some("true")
        {
            return Ok(stream);
        }

        // Snapshot captured events. If the lock is poisoned, fall through to
        // the original stream rather than failing the request.
        let events: Vec<StreamEvent> = match self.captured.lock() {
            Ok(guard) => {
                if guard.is_empty() {
                    return Ok(stream);
                }
                guard.clone()
            }
            Err(_) => return Ok(stream),
        };

        ctx.metadata.insert(
            META_PASS_THROUGH_STREAM_APPLIED.to_string(),
            "true".to_string(),
        );

        // Drain the original inbound stream so upstream middleware wrappers
        // (performance recording, rate limit tracking, connection tracking)
        // still execute their per-event logic. Go does this in a goroutine
        // (pass_through.go:371-377); here we do it eagerly because the
        // iterator model is synchronous.
        //
        // We chain: first yield all captured events, then drain the original.
        // But Go drains the original concurrently while yielding captured.
        // Since our iterators are synchronous, we drain first, then yield.
        // This matches the semantic guarantee: all original events are
        // consumed, and the consumer receives the raw captured events.
        for _ in stream {
            // Drain — side-effect wrappers fire per event.
        }

        // Yield the captured raw provider events.
        let replaced: BoxEventStream = Box::new(events.into_iter());
        Ok(replaced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Helper: build a vec of dummy stream events.
    fn dummy_events(prefix: &str, count: usize) -> Vec<StreamEvent> {
        (0..count)
            .map(|i| StreamEvent {
                data: Some(format!("{prefix}-{i}")),
                ..StreamEvent::default()
            })
            .collect()
    }

    /// When pass-through is enabled and captured events exist, the inbound
    /// stream must be replaced with the captured events. The original stream
    /// must be fully drained (side-effect wrappers fire).
    #[test]
    fn replaces_stream_with_captured_events_when_enabled() -> Result<(), Box<dyn std::error::Error>>
    {
        // Simulate CaptureStreamMiddleware having captured 3 events.
        let captured: CapturedEvents = Arc::new(Mutex::new(dummy_events("raw", 3)));

        let mw = PassThroughStreamMiddleware::new(Arc::clone(&captured));
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert(META_PASS_THROUGH_ENABLED.to_string(), "true".to_string());
        ctx.metadata
            .insert(META_STREAM_CAPTURE_ACTIVE.to_string(), "true".to_string());

        // Original inbound stream (transformed events — these should be
        // drained but NOT returned to the consumer).
        let drain_count = Arc::new(Mutex::new(0usize));
        let drain_count_clone = Arc::clone(&drain_count);
        let original: BoxEventStream = Box::new(
            dummy_events("transformed", 2)
                .into_iter()
                .inspect(move |_| {
                    if let Ok(mut c) = drain_count_clone.lock() {
                        *c += 1;
                    }
                }),
        );

        let result_stream = mw.on_inbound_raw_stream(&mut ctx, original)?;

        // Consumer receives the captured raw events, not the transformed ones.
        let consumed: Vec<StreamEvent> = result_stream.collect();
        assert_eq!(consumed.len(), 3);
        assert_eq!(consumed[0].data.as_deref(), Some("raw-0"));
        assert_eq!(consumed[1].data.as_deref(), Some("raw-1"));
        assert_eq!(consumed[2].data.as_deref(), Some("raw-2"));

        // Original stream was fully drained.
        {
            let count = drain_count
                .lock()
                .map_err(|e| format!("lock poisoned: {e}"))?;
            assert_eq!(*count, 2, "original stream must be fully drained");
        }

        // Metadata flag set.
        assert_eq!(
            ctx.metadata
                .get(META_PASS_THROUGH_STREAM_APPLIED)
                .map(|s| s.as_str()),
            Some("true"),
        );

        Ok(())
    }

    /// When pass-through is NOT enabled, the original stream must pass
    /// through untouched.
    #[test]
    fn passes_through_when_disabled() -> Result<(), Box<dyn std::error::Error>> {
        let captured: CapturedEvents = Arc::new(Mutex::new(dummy_events("raw", 3)));
        let mw = PassThroughStreamMiddleware::new(captured);
        let mut ctx = PipelineContext::new();
        // No pass_through_enabled flag — disabled.

        let original: BoxEventStream = Box::new(dummy_events("original", 2).into_iter());
        let result_stream = mw.on_inbound_raw_stream(&mut ctx, original)?;

        let consumed: Vec<StreamEvent> = result_stream.collect();
        assert_eq!(consumed.len(), 2);
        assert_eq!(consumed[0].data.as_deref(), Some("original-0"));
        assert_eq!(consumed[1].data.as_deref(), Some("original-1"));

        // No metadata flag set.
        assert!(!ctx.metadata.contains_key(META_PASS_THROUGH_STREAM_APPLIED));

        Ok(())
    }
}
