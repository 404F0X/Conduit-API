//! Upstream cancellation primitives (RUST-P8-002 S13/S17).
//!
//! Ports the *cancellation semantics* of Go `conduit/llm/pipeline/stream.go`:
//!
//! - **S13 — cancel upstream on stream close**: Go wraps the client-facing
//!   inbound stream in `cancelOnCloseStream` (`stream.go:109-132`, wired at
//!   `stream.go:406-411`). `Close()` closes the inner stream and then fires the
//!   stream context's `cancel` exactly once (`sync.Once`), which tears down the
//!   upstream HTTP request started with that child context
//!   (`newFirstEventTimeoutGuard`, `stream.go:35` `context.WithCancel(ctx)`).
//!   The Rust analog is [`CancelOnCloseStream`]: a drop guard around the
//!   client-facing event source that cancels an upstream [`CancelToken`] when
//!   it is closed **or dropped** (Rust streams are frequently just dropped, so
//!   `Drop` must behave like Go's `Close`).
//! - **S17 — client disconnect stops retries**: Go checks `ctx.Err() != nil`
//!   after every failed attempt (`pipeline.go:290-293`) and the canceled
//!   context also propagates into the in-flight `executor.Do/DoStream`. The
//!   Rust analog is a shared [`CancelToken`] on
//!   [`crate::middleware::PipelineContext`]: the HTTP layer cancels it when the
//!   client goes away, the pipeline consults it at the same checkpoint, and
//!   per-attempt **child** tokens ([`CancelToken::child`], mirroring
//!   `context.WithCancel`) hand the signal to the executor.
//!
//! Besides the synchronous state check, each token carries a watch signal so
//! an executor can wake an in-flight async read as soon as the client closes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// CancelToken — Go `context.WithCancel` analog (parent → child propagation).
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CancelInner {
    canceled: AtomicBool,
    changed: tokio::sync::watch::Sender<bool>,
    /// Cancellation flows parent → child only (Go: canceling a parent context
    /// cancels children; canceling a child never cancels the parent).
    parent: Option<CancelToken>,
}

impl Default for CancelInner {
    fn default() -> Self {
        Self {
            canceled: AtomicBool::new(false),
            changed: tokio::sync::watch::channel(false).0,
            parent: None,
        }
    }
}

/// A cancellation token mirroring Go `context.Context` cancellation semantics
/// for the pipeline:
///
/// - [`CancelToken::cancel`] flips this token (idempotent — returns `true`
///   only for the first flip, mirroring the `sync.Once` in Go's
///   `cancelOnCloseStream`).
/// - [`CancelToken::child`] derives a token that reports canceled when either
///   itself **or any ancestor** is canceled (Go `context.WithCancel(ctx)` at
///   `stream.go:35`).
/// - Clones share state (like copying a Go `ctx`/`cancel` pair).
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive a child token: canceled when the child itself or any ancestor is
    /// canceled. Mirrors Go `context.WithCancel(ctx)`.
    pub fn child(&self) -> Self {
        Self {
            inner: Arc::new(CancelInner {
                canceled: AtomicBool::new(false),
                changed: tokio::sync::watch::channel(false).0,
                parent: Some(self.clone()),
            }),
        }
    }

    /// Cancel this token. Returns `true` if this call performed the flip
    /// (first cancel), `false` if it was already canceled — the once-only
    /// signal Go gets from `sync.Once` (`stream.go:112`/`129`).
    pub fn cancel(&self) -> bool {
        let changed = !self.inner.canceled.swap(true, Ordering::SeqCst);
        if changed {
            self.inner.changed.send_replace(true);
        }
        changed
    }

    /// Whether this token or any of its ancestors has been canceled
    /// (Go `ctx.Err() != nil`).
    pub fn is_canceled(&self) -> bool {
        if self.inner.canceled.load(Ordering::SeqCst) {
            return true;
        }
        match &self.inner.parent {
            Some(parent) => parent.is_canceled(),
            None => false,
        }
    }

    /// Wait until this token or one of its ancestors is canceled.
    ///
    /// The watch subscription is created before the state check, so a cancel
    /// racing with this method cannot be missed.
    pub async fn cancelled(&self) {
        let mut own = self.inner.changed.subscribe();
        if self.is_canceled() {
            return;
        }

        if let Some(parent) = self.inner.parent.clone() {
            let parent_wait = Box::pin(async move { parent.cancelled().await });
            tokio::select! {
                _ = own.changed() => {},
                _ = parent_wait => {},
            }
        } else {
            let _ = own.changed().await;
        }
    }
}

/// Equality compares the *observable cancellation state* (needed so
/// `PipelineContext` can keep deriving `PartialEq`/`Eq`). Two fresh tokens are
/// equal; a canceled and a live token are not.
impl PartialEq for CancelToken {
    fn eq(&self, other: &Self) -> bool {
        self.is_canceled() == other.is_canceled()
    }
}

impl Eq for CancelToken {}

// ---------------------------------------------------------------------------
// CancelOnCloseStream — Go `cancelOnCloseStream` (`stream.go:109-132`).
// ---------------------------------------------------------------------------

/// Drop guard around a client-facing event source that cancels the upstream
/// [`CancelToken`] when the stream is closed or dropped.
///
/// Mirrors Go `cancelOnCloseStream` (`stream.go:109-132`):
///
/// | Go                                                 | Rust                       |
/// |----------------------------------------------------|----------------------------|
/// | `Next()`/`Current()` delegate to the inner stream  | `Iterator::next` delegates |
/// | `Close()` → inner `Close()` then `once.Do(cancel)` | [`close`](Self::close) or `Drop` → `token.cancel()` (idempotent on the token itself) |
///
/// Go only wires the wrapper when a first-event guard exists (`stream.go:406`,
/// i.e. a child stream context was created); in Rust the pipeline always hands
/// the HTTP layer a per-attempt child token, so the wrapper is applicable to
/// every streaming response.
#[derive(Debug)]
pub struct CancelOnCloseStream<S> {
    inner: S,
    token: CancelToken,
}

impl<S> CancelOnCloseStream<S> {
    /// Wrap `inner`; the `token` (the upstream request's cancel token) fires
    /// when this wrapper is closed or dropped.
    pub fn new(inner: S, token: CancelToken) -> Self {
        Self { inner, token }
    }

    /// Explicit close, mirroring Go `Close()` (`stream.go:127-132`): consumes
    /// the wrapper, which cancels the upstream token exactly once (double
    /// close in Go is also collapsed by `sync.Once`; here the token's own
    /// idempotency provides that).
    pub fn close(self) {
        // Dropping `self` runs the Drop impl below, which cancels the token.
    }

    /// The upstream cancel token this wrapper guards (for observability).
    pub fn token(&self) -> &CancelToken {
        &self.token
    }
}

impl<S: Iterator> Iterator for CancelOnCloseStream<S> {
    type Item = S::Item;

    // Go `Next()`/`Current()` (`stream.go:115-121`) — plain delegation; the
    // wrapper never filters or transforms events.
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

impl<S> Drop for CancelOnCloseStream<S> {
    fn drop(&mut self) {
        // Go `Close()` → `s.once.Do(s.cancel)` (`stream.go:129`). Idempotency
        // comes from the token, so drop-after-close does not double-fire.
        self.token.cancel();
    }
}

// ---------------------------------------------------------------------------
// CancelGuard — plain drop-guard for the non-stream (buffered) path (P-09).
// ---------------------------------------------------------------------------

/// A bare drop-guard that cancels its [`CancelToken`] when dropped.
///
/// The buffered request path (Go `Process`, non-stream branch) has no
/// client-facing stream to wrap with [`CancelOnCloseStream`], but it still
/// needs the same client-disconnect → upstream-cancel behavior: when axum drops
/// the handler future (client went away), this guard fires the per-request
/// token so the pipeline's between-attempt cancel check
/// (`Pipeline::process`'s `is_context_canceled`) stops retrying and billing
/// immediately, mirroring Go's `ctx.Done()` propagation into `Process`.
///
/// Hold the guard for the lifetime of the buffered request future; on normal
/// completion call [`disarm`](Self::disarm) so a *successful* response does not
/// spuriously cancel (the token is only meaningful while work is in flight).
#[derive(Debug)]
pub struct CancelGuard {
    token: CancelToken,
    armed: bool,
}

impl CancelGuard {
    /// Arm a guard over `token`. Dropping the guard (e.g. the request future is
    /// aborted on client disconnect) cancels `token` unless [`disarm`]ed first.
    pub fn new(token: CancelToken) -> Self {
        Self { token, armed: true }
    }

    /// Disarm the guard so a subsequent drop does NOT cancel — call after the
    /// buffered request completed normally.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// The guarded token (clone shares state) — hand this to the work that must
    /// observe the cancellation.
    pub fn token(&self) -> CancelToken {
        self.token.clone()
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- CancelToken (Go context.WithCancel semantics) -----------------------

    #[test]
    fn fresh_token_is_not_canceled() {
        let token = CancelToken::new();
        assert!(!token.is_canceled());
    }

    #[test]
    fn cancel_flips_once_and_is_idempotent() {
        // Go sync.Once parity: only the first cancel "does" anything.
        let token = CancelToken::new();
        assert!(token.cancel(), "first cancel performs the flip");
        assert!(!token.cancel(), "second cancel is a no-op");
        assert!(token.is_canceled());
    }

    #[test]
    fn clones_share_cancellation_state() {
        // Cloning is copying the Go ctx/cancel pair — both observe the flip.
        let token = CancelToken::new();
        let handle = token.clone();
        handle.cancel();
        assert!(token.is_canceled());
    }

    #[test]
    fn parent_cancel_propagates_to_child() {
        // Go: canceling the request ctx cancels the derived stream ctx.
        let parent = CancelToken::new();
        let child = parent.child();
        assert!(!child.is_canceled());
        parent.cancel();
        assert!(child.is_canceled(), "child must see ancestor cancellation");
    }

    #[test]
    fn child_cancel_does_not_propagate_to_parent() {
        // Go: closing the client stream cancels only the child stream ctx
        // (stream.go:406-411); the request ctx stays alive.
        let parent = CancelToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_canceled());
        assert!(!parent.is_canceled(), "parent unaffected by child cancel");
    }

    #[test]
    fn grandchild_sees_root_cancellation() {
        let root = CancelToken::new();
        let grandchild = root.child().child();
        root.cancel();
        assert!(grandchild.is_canceled());
    }

    #[tokio::test]
    async fn async_waiter_wakes_for_ancestor_cancellation() {
        let root = CancelToken::new();
        let child = root.child();
        let waiter = tokio::spawn(async move { child.cancelled().await });
        tokio::task::yield_now().await;
        root.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .unwrap_or_else(|_| panic!("cancellation waiter did not wake"))
            .unwrap_or_else(|err| panic!("cancellation waiter task failed: {err}"));
    }

    // -- CancelOnCloseStream (Go cancelOnCloseStream) ------------------------

    #[test]
    fn close_cancels_upstream_token() {
        // Go stream.go:127-131 — Close() fires cancel.
        let token = CancelToken::new();
        let stream = CancelOnCloseStream::new(std::iter::empty::<u8>(), token.clone());
        stream.close();
        assert!(token.is_canceled());
    }

    #[test]
    fn drop_cancels_upstream_token() {
        // Rust streams are often just dropped; Drop must equal Go Close().
        let token = CancelToken::new();
        {
            let _stream = CancelOnCloseStream::new(std::iter::empty::<u8>(), token.clone());
        }
        assert!(token.is_canceled());
    }

    #[test]
    fn iteration_delegates_and_does_not_cancel_early() {
        // Go Next()/Current() delegate without touching cancel (stream.go:115-121).
        let token = CancelToken::new();
        let mut stream = CancelOnCloseStream::new(vec![1, 2, 3].into_iter(), token.clone());
        assert_eq!(stream.next(), Some(1));
        assert_eq!(stream.next(), Some(2));
        assert!(
            !token.is_canceled(),
            "consuming events must not cancel upstream"
        );
        assert_eq!(stream.next(), Some(3));
        assert_eq!(stream.next(), None);
        // Even full exhaustion is not a close — Go requires an explicit Close().
        assert!(!token.is_canceled());
        drop(stream);
        assert!(token.is_canceled());
    }

    #[test]
    fn client_close_cancels_child_but_not_request_token() {
        // End-to-end S13 shape: request ctx -> child stream ctx -> wrapper.
        // Closing the client stream cancels the upstream (child) token only.
        let request_token = CancelToken::new();
        let upstream_token = request_token.child();
        let stream = CancelOnCloseStream::new(vec!["evt"].into_iter(), upstream_token.clone());
        stream.close();
        assert!(upstream_token.is_canceled());
        assert!(!request_token.is_canceled());
    }
}
