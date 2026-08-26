//! P1-002 S14 — panic-catching tower layer.
//!
//! Mirrors Go's `defer recover()` pattern (e.g. gin's built-in `Recovery`
//! middleware, and the `recover()` guards scattered across
//! `conduit/internal/server/...`). A handler that panics must not drop the
//! connection: instead we catch the unwind, attach the request_id / trace_id
//! when available, and emit a 500 `internal_error` JSON body produced by
//! [`internal_fallback_error`](crate::error_middleware::internal_fallback_error)
//! via [`fallback_panic_error`](crate::error_middleware::fallback_panic_error).
//!
//! Implementation notes
//! --------------------
//! `std::panic::catch_unwind` is the standard-library, safe-API way to turn a
//! panic into a `Result`. It is **not** gated by the workspace
//! `unsafe_code = "forbid"` lint — `catch_unwind` is a fully safe function. We
//! wrap the inner service future in `AssertUnwindSafe` so the compiler permits
//! the unwind boundary; panic-safety of the captured state is our
//! responsibility, and here the only captured state is the inner `Service`,
//! which is never observed again after a panic (the inner future is taken out
//! of the `Option` on the first poll that yields).
//!
//! The layer is intentionally minimal: no `tower-http` dependency, just a
//! hand-rolled `Layer` + `Service` pair implementing `tower::Service`.

use std::any::Any;
use std::convert::Infallible;
use std::future::Future;
use std::panic::{AssertUnwindSafe, UnwindSafe};
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;

use crate::error_middleware::{
    ErrorFallbackContext, ErrorResponseFormat, conduit_error_response, fallback_panic_error,
};

/// Boxed future returned by the inner service (axum's router service is
/// `Future<Output = Result<Response, Infallible>> + Send`).
type InnerFuture = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

/// Tower layer that wraps the entire router so any handler panic is caught
/// and converted into a 500 `internal_error` JSON response (P1-002 S14).
///
/// Construct with [`PanicCatchLayer::new`] and apply via `Router::layer`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PanicCatchLayer;

impl PanicCatchLayer {
    pub fn new() -> Self {
        Self
    }
}

impl<S> tower::Layer<S> for PanicCatchLayer {
    type Service = PanicCatchService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PanicCatchService { inner }
    }
}

/// Service produced by [`PanicCatchLayer`].
///
/// `S` is the inner service (typically `axum::Router` after it has been turned
/// into a `Service`). We require it to be `Clone` because the standard tower
/// readiness protocol pairs `poll_ready(&mut self)` with `call(&mut self)`,
/// and we need an owned copy to hand off to the spawned future.
#[derive(Clone, Debug)]
pub struct PanicCatchService<S> {
    inner: S,
}

impl<S> tower::Service<Request<Body>> for PanicCatchService<S>
where
    S: tower::Service<Request<Body>, Response = Response, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    type Response = Response;
    type Error = Infallible;
    type Future = PanicCatchFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Delegate readiness to the inner service. `Infallible` means the only
        // possible outcome is `Ok` or `Pending`.
        match self.inner.poll_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
            // Unreachable: Error = Infallible.
            Poll::Ready(Err(infallible)) => match infallible {},
        }
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        // Capture the log context (request_id / trace_id) from request headers
        // before handing the request to the inner service. The panic payload
        // produced by a downstream handler panic is what we want to log, but we
        // attach the request_id/trace_id so an operator can correlate the
        // captured panic back to the originating request.
        let context = fallback_context_from_request(&req);

        // Standard tower buffer/clone dance: clone the inner service so the
        // future is self-contained and `'static`. axum's `Router` is cheaply
        // cloneable (it shares its route table via `Arc`).
        let mut clone = self.inner.clone();
        let inner_future = clone.call(req);

        PanicCatchFuture {
            inner: Some(Box::pin(inner_future)),
            context,
        }
    }
}

/// Future produced by [`PanicCatchService`].
///
/// `inner` is kept in an `Option` so that, on a caught panic, we can `take()`
/// it and guarantee we never poll the panicked future again (the panic-safety
/// invariant).
pub struct PanicCatchFuture {
    inner: Option<InnerFuture>,
    context: ErrorFallbackContext,
}

impl Future for PanicCatchFuture {
    type Output = Result<Response, Infallible>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(inner) = self.inner.as_mut() else {
            // Should not happen in practice: a future is not polled again
            // after it returned `Ready`. If it ever is, synthesize a 500.
            return Poll::Ready(Ok(internal_error_response(&fallback_panic_error(
                &"panic future polled after recovery",
                std::mem::take(&mut self.context),
            ))));
        };

        // `catch_unwind` requires the closure's captured state to be
        // `UnwindSafe`. `Pin<&mut InnerFuture>` is not `UnwindSafe`, so we wrap
        // the closure in `AssertUnwindSafe`. Panic-safety here amounts to "we
        // never poll `inner` again after a panic", which the `Option::take` on
        // the `Err` branch guarantees. `catch_unwind` is a safe stdlib API and
        // is not gated by the workspace `unsafe_code = "forbid"` lint.
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| inner.as_mut().poll(cx)));
        match result {
            // Inner future polled cleanly (no panic this tick).
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(outcome)) => {
                // Drop our handle to the inner future.
                let _ = self.inner.take();
                // `Infallible` cannot be constructed; match it for exhaustiveness.
                let response = match outcome {
                    Ok(response) => response,
                    Err(infallible) => match infallible {},
                };
                Poll::Ready(Ok(response))
            }
            // A panic propagated up from the handler future. Recover it.
            Err(payload) => {
                // Drop our handle to the inner future so it is never polled
                // again (panic-safety invariant).
                let _ = self.inner.take();
                let context = std::mem::take(&mut self.context);
                let err = fallback_panic_error(payload.as_ref(), context);
                Poll::Ready(Ok(internal_error_response(&err)))
            }
        }
    }
}

// `catch_unwind` requires the closure's captured state to be `UnwindSafe`.
// `InnerFuture` is `Pin<Box<dyn Future + Send>>`, which is not `UnwindSafe`,
// but we manually uphold the panic-safety invariant (never poll after a panic),
// so `AssertUnwindSafe` is the correct wrapper. This trait impl just makes the
// `AssertUnwindSafe(|| ...)` closure above type-check without further ceremony.
// It is intentionally conservative: only the closure's *capture* needs the
// marker, not the future type itself.
//
// (We do not actually implement `UnwindSafe` for `PanicCatchFuture`; the
// `AssertUnwindSafe` wrapper is applied at the call site, which is the
// idiomatic tower/tokio pattern.)
#[allow(dead_code)]
fn _unwind_safe_marker<T: UnwindSafe>(_: &T) {}

/// Build the 500 `internal_error` JSON response for a captured panic.
///
/// Uses the admin JSON shape (fixed `{"error":{"type","message"}}`) so the body
/// never leaks the panic payload — only the sanitized public message.
fn internal_error_response(err: &conduit_core::ConduitError) -> Response {
    conduit_error_response(err, ErrorResponseFormat::AdminJson)
}

/// Extract an [`ErrorFallbackContext`] (request_id / trace_id) from the request
/// headers so a captured panic can be correlated back to the originating
/// request in logs. Mirrors the Go recovery middleware, which logs
/// `c.GetHeader("Conduit-Request-Id")` / `Conduit-Trace-Id` when it catches a panic.
fn fallback_context_from_request(req: &Request<Body>) -> ErrorFallbackContext {
    fn header_str(req: &Request<Body>, name: &str) -> Option<String> {
        req.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    ErrorFallbackContext {
        request_id: header_str(req, "Conduit-Request-Id"),
        trace_id: header_str(req, "Conduit-Trace-Id"),
    }
}

// Keep a type alias so the `Box<dyn Any + Send>` returned by `catch_unwind`
// stays friendly with `fallback_panic_error`'s signature in doc cross-refs.
#[allow(dead_code)]
type PanicPayload = Box<dyn Any + Send>;

#[cfg(test)]
mod tests {
    use std::error::Error;

    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use serde_json::Value;
    use tower::Service;

    use super::*;

    /// A panicking handler that is only reachable inside a router wrapped by
    /// `PanicCatchLayer`. Without the layer the panic would tear down the
    /// connection.
    async fn panicking_handler() -> &'static str {
        panic!("synthetic panic from handler");
    }

    /// Build a router whose single route panics, wrapped in the catch layer.
    fn panicking_router() -> Router {
        Router::new()
            .route("/boom", get(panicking_handler))
            .layer(PanicCatchLayer::new())
    }

    #[tokio::test]
    async fn panic_is_caught_and_converted_to_500_internal_error() -> Result<(), Box<dyn Error>> {
        let mut app = panicking_router();
        let request = Request::builder().uri("/boom").body(Body::empty())?;

        let response = app.call(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), 1024).await?;
        let body: Value = serde_json::from_slice(&body)?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(content_type.starts_with("application/json"));
        // AdminJson shape: fixed "Internal server error" message; the panic
        // payload must NOT leak.
        assert_eq!(body["error"]["message"], "Internal server error");
        assert_eq!(body["error"]["type"], "internal_error");
        assert!(!body.to_string().contains("synthetic panic"));

        Ok(())
    }

    #[tokio::test]
    async fn panic_layer_attaches_request_and_trace_ids_when_present() -> Result<(), Box<dyn Error>>
    {
        // Directly exercise `fallback_panic_error` so we can inspect the
        // captured `ConduitError` metadata, which is not visible in the public
        // 500 body (the public message is sanitized to "Internal server error").
        let context = ErrorFallbackContext {
            request_id: Some("req-123".to_string()),
            trace_id: Some("trace-456".to_string()),
        };
        let payload: Box<dyn Any + Send> = Box::new("boom");
        let err = fallback_panic_error(payload.as_ref(), context);

        assert_eq!(err.metadata["request_id"], "req-123");
        assert_eq!(err.metadata["trace_id"], "trace-456");
        assert_eq!(err.message, "boom");
        assert_eq!(err.public_message(), "Internal server error");

        Ok(())
    }

    #[tokio::test]
    async fn non_panicking_handler_passes_through_unchanged() -> Result<(), Box<dyn Error>> {
        async fn ok_handler() -> &'static str {
            "ok"
        }

        let mut app = Router::new()
            .route("/ok", get(ok_handler))
            .layer(PanicCatchLayer::new());
        let request = Request::builder().uri("/ok").body(Body::empty())?;

        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64).await?;
        assert_eq!(&body[..], b"ok");

        Ok(())
    }
}
