//! Pipeline middleware that captures raw provider stream events for
//! pass-through forwarding. Wraps the outbound stream in a fan-out iterator
//! that clones each event into a shared buffer while forwarding the original
//! to the downstream consumer (pipeline middlewares, LLM response transform).
//!
//! Go parity: `captureRawProviderStream` (pass_through.go:252-344).
//!
//! The Go implementation fans events into two Go channels (pipeline + raw
//! pass-through) via a goroutine. The Rust port uses a synchronous
//! `FanOutStreamIter` wrapper that clones each event into an
//! `Arc<Mutex<Vec<StreamEvent>>>` side-buffer during iteration, which the
//! companion `PassThroughStreamMiddleware` reads on the inbound path.
//!
//! Hook overridden: `on_outbound_raw_stream` (REVERSE order, once per
//! successful streaming Attempt).

use std::sync::{Arc, Mutex};

use conduit_llm::StreamEvent;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{BoxEventStream, PipelineContext, PipelineResult};

/// Metadata key consulted to determine whether pass-through is enabled.
const META_PASS_THROUGH_ENABLED: &str = "pass_through_enabled";

/// Metadata key set when the capture wrapper is installed, so downstream
/// middlewares (and `PassThroughStreamMiddleware`) know events are being
/// captured.
const META_STREAM_CAPTURE_ACTIVE: &str = "stream_capture_active";

/// Shared buffer type holding captured stream events. Exposed so the
/// companion `PassThroughStreamMiddleware` can read it.
pub type CapturedEvents = Arc<Mutex<Vec<StreamEvent>>>;

/// Captures raw provider stream events for pass-through forwarding.
///
/// The middleware holds a `CapturedEvents` buffer that is populated lazily as
/// the consumer drains the wrapped stream. The same `Arc` is handed to
/// `PassThroughStreamMiddleware` at construction time so it can replay events
/// on the inbound path.
pub struct CaptureStreamMiddleware {
    /// Shared buffer where cloned events accumulate during iteration.
    pub captured: CapturedEvents,
}

impl Default for CaptureStreamMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureStreamMiddleware {
    /// Creates a new middleware together with a fresh (empty) capture buffer.
    pub fn new() -> Self {
        Self {
            captured: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl PipelineMiddleware for CaptureStreamMiddleware {
    fn name(&self) -> &'static str {
        "capture-raw-provider-stream"
    }

    fn on_outbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        // Only capture when pass-through is enabled.
        if ctx
            .metadata
            .get(META_PASS_THROUGH_ENABLED)
            .map(|s| s.as_str())
            != Some("true")
        {
            return Ok(stream);
        }

        // Mark that stream capture is active for downstream consumers.
        ctx.metadata
            .insert(META_STREAM_CAPTURE_ACTIVE.to_string(), "true".to_string());

        // Wrap the stream in FanOutStreamIter — each event is cloned into the
        // shared buffer while the original is yielded to the consumer.
        let wrapped: BoxEventStream = Box::new(FanOutStreamIter {
            inner: stream,
            buffer: Arc::clone(&self.captured),
        });

        Ok(wrapped)
    }
}

/// Lazy fan-out iterator (S08-compliant). Each call to `next()` pulls one
/// event from the inner stream, clones it into the shared `buffer`, and yields
/// the original. No events are pre-collected at wrap time.
///
/// Go equivalent: the goroutine in `captureRawProviderStream` that sends each
/// event to both `pipelineCh` and `rawStreamCh` (pass_through.go:303-339).
struct FanOutStreamIter {
    inner: BoxEventStream,
    buffer: CapturedEvents,
}

impl Iterator for FanOutStreamIter {
    type Item = StreamEvent;

    fn next(&mut self) -> Option<StreamEvent> {
        let event = self.inner.next()?;

        // Clone into the side-buffer. If the lock is poisoned we still yield
        // the original event — capturing is best-effort, never blocks the
        // pipeline consumer.
        if let Ok(mut guard) = self.buffer.lock() {
            guard.push(event.clone());
        }

        Some(event)
    }
}

// FanOutStreamIter is Send because both BoxEventStream (Send) and
// Arc<Mutex<Vec<StreamEvent>>> (Send) are Send.
// (Compiler enforces this via the BoxEventStream = Box<dyn Iterator + Send>
// return type in on_outbound_raw_stream.)

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a vec of dummy stream events.
    fn dummy_events(count: usize) -> Vec<StreamEvent> {
        (0..count)
            .map(|i| StreamEvent {
                data: Some(format!("event-{i}")),
                ..StreamEvent::default()
            })
            .collect()
    }

    /// When pass-through is enabled, consuming the wrapped stream must
    /// populate the shared capture buffer with clones of every event while
    /// yielding the originals to the consumer.
    #[test]
    fn captures_events_into_shared_buffer_when_enabled() -> Result<(), Box<dyn std::error::Error>> {
        let mw = CaptureStreamMiddleware::new();
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert(META_PASS_THROUGH_ENABLED.to_string(), "true".to_string());

        let stream: BoxEventStream = Box::new(dummy_events(3).into_iter());
        let wrapped = mw.on_outbound_raw_stream(&mut ctx, stream)?;

        // Before consuming: buffer is empty (S08 — lazy, no pre-collect).
        {
            let guard = mw
                .captured
                .lock()
                .map_err(|e| format!("lock poisoned: {e}"))?;
            assert_eq!(
                guard.len(),
                0,
                "no events should be captured before consuming"
            );
        }

        // Consume all events from the wrapped stream.
        let consumed: Vec<StreamEvent> = wrapped.collect();
        assert_eq!(consumed.len(), 3);
        assert_eq!(consumed[0].data.as_deref(), Some("event-0"));
        assert_eq!(consumed[1].data.as_deref(), Some("event-1"));
        assert_eq!(consumed[2].data.as_deref(), Some("event-2"));

        // After consuming: buffer contains clones of all events.
        {
            let guard = mw
                .captured
                .lock()
                .map_err(|e| format!("lock poisoned: {e}"))?;
            assert_eq!(guard.len(), 3);
            assert_eq!(guard[0].data.as_deref(), Some("event-0"));
            assert_eq!(guard[1].data.as_deref(), Some("event-1"));
            assert_eq!(guard[2].data.as_deref(), Some("event-2"));
        }

        // stream_capture_active metadata flag must be set.
        assert_eq!(
            ctx.metadata
                .get(META_STREAM_CAPTURE_ACTIVE)
                .map(|s| s.as_str()),
            Some("true"),
        );

        Ok(())
    }

    /// When pass-through is NOT enabled, the stream must pass through
    /// untouched and the capture buffer must remain empty.
    #[test]
    fn skips_capture_when_pass_through_disabled() -> Result<(), Box<dyn std::error::Error>> {
        let mw = CaptureStreamMiddleware::new();
        let mut ctx = PipelineContext::new();
        // No pass_through_enabled flag — disabled.

        let stream: BoxEventStream = Box::new(dummy_events(2).into_iter());
        let wrapped = mw.on_outbound_raw_stream(&mut ctx, stream)?;

        let consumed: Vec<StreamEvent> = wrapped.collect();
        assert_eq!(consumed.len(), 2);

        // Buffer must be empty — no capture happened.
        {
            let guard = mw
                .captured
                .lock()
                .map_err(|e| format!("lock poisoned: {e}"))?;
            assert_eq!(guard.len(), 0);
        }

        // No metadata flag set.
        assert!(!ctx.metadata.contains_key(META_STREAM_CAPTURE_ACTIVE));

        Ok(())
    }
}
