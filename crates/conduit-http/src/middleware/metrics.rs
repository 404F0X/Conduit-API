//! P2-002 S07 — Metrics middleware layer.
//!
//! An axum `from_fn`-compatible middleware that records per-request metrics:
//! request count, in-flight gauge, and latency. Mirrors Go
//! `internal/server/middleware/metrics.go` which calls `RecordHTTPRequest`
//! after the handler completes.
//!
//! ## Metrics collection approach
//!
//! This layer uses simple atomic counters as an intermediate step before full
//! OpenTelemetry integration (planned under the `otel` feature flag). The
//! atomics are lock-free, have negligible overhead on the hot path, and allow
//! the `/admin/status` diagnostics endpoint to read a live snapshot without
//! contention. When OTel lands, the atomics will feed into OTel instruments
//! instead of being the sole source of truth.
//!
//! ## Go source contract
//!
//! Go `internal/server/middleware/metrics.go`:
//! - Increments `http_requests_in_flight` gauge before `c.Next()`
//! - Decrements `http_requests_in_flight` gauge after `c.Next()`
//! - Observes `http_request_duration_seconds` histogram after completion
//! - Increments `http_requests_total` counter after completion
//! - All metrics are labeled by route pattern (not raw path, to avoid cardinality explosion)
//!
//! ## Extension contract
//!
//! After the downstream handler returns, this middleware inserts a
//! [`RequestMetrics`] extension into the response so that the AccessLog
//! middleware (which runs earlier in the tower stack, i.e. wraps this layer)
//! can read latency without re-measuring.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Instant;

use axum::body::Body;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;

// ---------------------------------------------------------------------------
// State types
// ---------------------------------------------------------------------------

/// Shared metrics state injected into request extensions by the router layer.
///
/// Holds atomic counters that the middleware reads/writes on every request.
/// Cloning is cheap (Arc internals); the router inserts one instance into
/// the axum state or as a request extension at startup.
#[derive(Debug, Clone)]
pub struct MetricsState {
    /// Master switch — when `false` the middleware is a pure pass-through
    /// (no atomic operations, no latency measurement). Mirrors Go's
    /// `config.Metrics.Enabled` gate.
    pub enabled: bool,

    /// Total requests observed since process start.
    /// Corresponds to Go's `http_requests_total` counter.
    pub request_count: Arc<AtomicU64>,

    /// Current number of in-flight requests (incremented before handler,
    /// decremented after). Corresponds to Go's `http_requests_in_flight` gauge.
    /// Signed because concurrent decrements on shutdown could momentarily
    /// race; `i64` prevents wrapping to `u64::MAX`.
    pub in_flight: Arc<AtomicI64>,
}

impl MetricsState {
    /// Create a new enabled metrics state with zeroed counters.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            request_count: Arc::new(AtomicU64::new(0)),
            in_flight: Arc::new(AtomicI64::new(0)),
        }
    }

    /// Create a disabled (no-op) metrics state.
    pub fn disabled() -> Self {
        Self::new(false)
    }

    /// Take a point-in-time snapshot of all counters for diagnostics.
    ///
    /// Uses `Relaxed` ordering — the snapshot is advisory (status endpoint),
    /// not used for synchronization. This matches Go's prometheus registry
    /// scrape semantics where counters are eventually consistent.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            enabled: self.enabled,
            request_count: self.request_count.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time diagnostic snapshot returned by [`MetricsState::snapshot`].
///
/// Used by the `/admin/status` endpoint and health checks to surface
/// current load without exposing the atomic internals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub enabled: bool,
    pub request_count: u64,
    pub in_flight: i64,
}

/// Per-request metrics inserted as a response extension after the handler
/// completes. Downstream middleware (e.g. AccessLog) can read this to avoid
/// redundant latency measurement.
///
/// Corresponds to Go's `RecordHTTPRequest` call which passes duration to
/// the histogram observer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestMetrics {
    /// Wall-clock latency of the downstream handler in milliseconds.
    pub latency_ms: u64,
}

// ---------------------------------------------------------------------------
// Middleware function
// ---------------------------------------------------------------------------

/// Axum `from_fn` middleware that collects request metrics when enabled.
///
/// ## Behavior
///
/// 1. Reads [`MetricsState`] from request extensions.
/// 2. If absent or disabled, passes through without touching any state.
/// 3. If enabled:
///    - Increments `in_flight` (Relaxed — gauge, not a synchronization fence).
///    - Records start time via `Instant::now()`.
///    - Calls `next.run(request)` to execute the downstream handler.
///    - Decrements `in_flight`.
///    - Increments `request_count`.
///    - Inserts [`RequestMetrics`] into the response extensions.
///
/// ## Ordering rationale
///
/// `Relaxed` is sufficient for counters/gauges that are only read via
/// `snapshot()` for diagnostics — no happens-before relationship is needed
/// between the counter increment and any subsequent read in a different
/// request. This matches how Go's `prometheus` package handles counters
/// (atomic add without memory barriers beyond the CPU's cache coherence).
pub async fn inject_metrics_state(request: Request<Body>, next: Next) -> Response {
    // Try to extract MetricsState from request extensions. If absent or
    // disabled, pass through immediately with zero overhead.
    let state = request.extensions().get::<MetricsState>().cloned();

    let state = match state {
        Some(ref s) if s.enabled => s.clone(),
        _ => {
            // No metrics state or disabled — pure pass-through.
            return next.run(request).await;
        }
    };

    // --- Enabled path: instrument the request ---

    // Increment in-flight gauge before downstream processing.
    state.in_flight.fetch_add(1, Ordering::Relaxed);

    let start = Instant::now();

    // Execute downstream handler chain.
    let mut response = next.run(request).await;

    // Measure latency and update counters.
    let latency_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;

    // Decrement in-flight gauge — handler has returned.
    state.in_flight.fetch_sub(1, Ordering::Relaxed);

    // Increment total request counter.
    state.request_count.fetch_add(1, Ordering::Relaxed);

    // Insert latency into response extensions for downstream (AccessLog) to read.
    response
        .extensions_mut()
        .insert(RequestMetrics { latency_ms });

    response
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::error::Error;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::middleware::from_fn;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use tower::Service;

    use super::*;

    /// Handler that returns 200 OK immediately.
    async fn ok_handler() -> impl IntoResponse {
        StatusCode::OK
    }

    /// Handler that sleeps briefly to produce measurable latency.
    async fn slow_handler() -> impl IntoResponse {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        StatusCode::OK
    }

    /// Build a test router with MetricsState injected into every request
    /// via a wrapping layer that inserts the extension.
    fn build_router_with_metrics(state: MetricsState) -> Router {
        let captured_state = state.clone();

        // We insert MetricsState as a request extension via a preceding layer,
        // mirroring how the production router wires it.
        Router::new()
            .route("/ok", get(ok_handler))
            .route("/slow", get(slow_handler))
            .layer(from_fn(inject_metrics_state))
            .layer(axum::middleware::from_fn(
                move |mut req: Request<Body>, next: Next| {
                    let s = captured_state.clone();
                    async move {
                        req.extensions_mut().insert(s);
                        next.run(req).await
                    }
                },
            ))
    }

    #[tokio::test]
    async fn disabled_metrics_passes_through_without_touching_counters()
    -> Result<(), Box<dyn Error>> {
        let state = MetricsState::disabled();
        let mut router = build_router_with_metrics(state.clone());

        let request = Request::builder().uri("/ok").body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);

        // Counters must remain at zero — disabled means no-op.
        let snap = state.snapshot();
        assert_eq!(snap.request_count, 0);
        assert_eq!(snap.in_flight, 0);
        assert!(!snap.enabled);
        Ok(())
    }

    #[tokio::test]
    async fn enabled_metrics_increments_request_count() -> Result<(), Box<dyn Error>> {
        let state = MetricsState::new(true);
        let mut router = build_router_with_metrics(state.clone());

        let request = Request::builder().uri("/ok").body(Body::empty())?;
        let _response = router.call(request).await?;

        let snap = state.snapshot();
        assert_eq!(snap.request_count, 1);

        // Second request should increment to 2.
        let request = Request::builder().uri("/ok").body(Body::empty())?;
        let _response = router.call(request).await?;

        let snap = state.snapshot();
        assert_eq!(snap.request_count, 2);
        Ok(())
    }

    #[tokio::test]
    async fn enabled_metrics_increments_then_decrements_in_flight() -> Result<(), Box<dyn Error>> {
        let state = MetricsState::new(true);
        let mut router = build_router_with_metrics(state.clone());

        // After the request completes, in_flight should be back to 0.
        let request = Request::builder().uri("/ok").body(Body::empty())?;
        let _response = router.call(request).await?;

        let snap = state.snapshot();
        assert_eq!(
            snap.in_flight, 0,
            "in_flight should return to 0 after request completes"
        );
        assert_eq!(snap.request_count, 1, "request_count should be 1");
        Ok(())
    }

    #[tokio::test]
    async fn latency_is_recorded_after_slow_handler() -> Result<(), Box<dyn Error>> {
        let state = MetricsState::new(true);
        let mut router = build_router_with_metrics(state.clone());

        let request = Request::builder().uri("/slow").body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);

        // The RequestMetrics extension should have been inserted with latency > 0.
        let metrics = response.extensions().get::<RequestMetrics>();
        assert!(
            metrics.is_some(),
            "RequestMetrics extension should be present"
        );
        if let Some(m) = metrics {
            // The slow handler sleeps 20ms; latency should be at least 15ms
            // (allowing for timer jitter on CI).
            assert!(
                m.latency_ms >= 15,
                "expected latency >= 15ms, got {}ms",
                m.latency_ms
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn snapshot_returns_current_values() -> Result<(), Box<dyn Error>> {
        let state = MetricsState::new(true);

        // Before any requests, all counters are zero.
        let snap = state.snapshot();
        assert!(snap.enabled);
        assert_eq!(snap.request_count, 0);
        assert_eq!(snap.in_flight, 0);

        // Simulate some activity directly on the atomics.
        state.request_count.fetch_add(42, Ordering::Relaxed);
        state.in_flight.fetch_add(3, Ordering::Relaxed);

        let snap = state.snapshot();
        assert_eq!(snap.request_count, 42);
        assert_eq!(snap.in_flight, 3);
        Ok(())
    }

    #[tokio::test]
    async fn missing_metrics_state_extension_still_passes_through() -> Result<(), Box<dyn Error>> {
        // Router WITHOUT the MetricsState injection layer — the middleware
        // should detect the missing extension and pass through gracefully.
        let mut router = Router::new()
            .route("/ok", get(ok_handler))
            .layer(from_fn(inject_metrics_state));

        let request = Request::builder().uri("/ok").body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        // No RequestMetrics extension should be present (metrics were not active).
        assert!(
            response.extensions().get::<RequestMetrics>().is_none(),
            "RequestMetrics should NOT be inserted when MetricsState is absent"
        );
        Ok(())
    }
}
