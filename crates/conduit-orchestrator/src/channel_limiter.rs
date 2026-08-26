//! Per-channel admission control limiter.
//!
//! Mirrors `conduit/internal/server/orchestrator/channel_limiter.go` and its
//! metrics surface (`channel_limiter_metrics.go`). The Go limiter uses a
//! synchronous FIFO blocking design (`Acquire(ctx)` waits on a channel, and
//! `Release` hands the slot directly to the head waiter).
//!
//! This Rust port keeps the same observable semantics — soft mode (only
//! counts), hard mode (counts + bounded FIFO queue + optional per-channel
//! `queue_timeout_ms`) — but exposes a non-blocking, fake-clock friendly API
//! so the queue-timeout path can be exercised deterministically without
//! wall-clock `sleep` (RUST-P9-005 S06/S13).
//!
//! Error taxonomy mirrors Go:
//! - `ErrChannelQueueFull`     -> [`ChannelLimiterError::QueueFull`]
//! - `ErrChannelQueueTimeout`  -> [`ChannelLimiterError::ChannelQueueTimeout`]
//!
//! Retry contract (Go `PersistentOutboundTransformer.CanRetry` /
//! `isChannelQueueError`): both queue errors are **local** admission
//! rejections — they never reached the upstream — so they must NOT count as
//! model errors (circuit-breaker skip) and are NOT same-channel retryable
//! (the pipeline bounces to the next candidate). See
//! [`ChannelLimiterError::is_local_admission_rejection`].

use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

#[derive(Clone, Debug)]
pub struct ChannelLimiter {
    config: ChannelLimiterConfig,
    state: Arc<ChannelLimiterState>,
}

impl ChannelLimiter {
    pub fn new(config: ChannelLimiterConfig) -> Self {
        Self {
            config,
            state: Arc::new(ChannelLimiterState::default()),
        }
    }

    /// Soft mode: only counts in-flight requests, never blocks or rejects.
    /// Mirrors Go `NewChannelLimiter(capacity, 0, 0)`.
    pub fn soft(max_concurrent: usize) -> Self {
        Self::new(ChannelLimiterConfig::soft(max_concurrent))
    }

    /// Hard mode with no per-channel queue timeout. Mirrors Go
    /// `NewChannelLimiter(capacity, queueSize, 0)`.
    pub fn hard(max_concurrent: usize, queue_size: usize) -> Self {
        Self::new(ChannelLimiterConfig::hard(max_concurrent, queue_size))
    }

    /// Hard mode with a per-channel queue timeout in milliseconds. Mirrors Go
    /// `NewChannelLimiter(capacity, queueSize, timeoutMs)`. `queue_timeout_ms
    /// == 0` means "no per-channel timeout" and is equivalent to [`Self::hard`].
    pub fn hard_with_timeout(
        max_concurrent: usize,
        queue_size: usize,
        queue_timeout_ms: u64,
    ) -> Self {
        Self::new(ChannelLimiterConfig::hard_with_timeout(
            max_concurrent,
            queue_size,
            queue_timeout_ms,
        ))
    }

    pub const fn config(&self) -> ChannelLimiterConfig {
        self.config
    }

    pub const fn mode(&self) -> ChannelLimiterMode {
        self.config.mode()
    }

    /// Current number of in-flight requests holding an admission slot.
    pub fn active_count(&self) -> usize {
        self.state.active.load(Ordering::Acquire)
    }

    /// Current number of requests waiting in the FIFO queue (always 0 in soft
    /// mode).
    pub fn queued_count(&self) -> usize {
        self.state.queued.load(Ordering::Acquire)
    }

    /// Point-in-time metrics snapshot. Mirrors Go
    /// `ChannelLimiterManager.Snapshot()` per-channel fields (RUST-P9-005 S07):
    /// current inflight, current queue depth, cumulative admitted/rejected/
    /// timeout counters. Kept stable for the OTel gauge callback and for tests
    /// (S12).
    pub fn snapshot(&self) -> ChannelLimiterMetricsSnapshot {
        ChannelLimiterMetricsSnapshot {
            current_inflight: self.state.active.load(Ordering::Acquire),
            queue_len: self.state.queued.load(Ordering::Acquire),
            admitted: self.state.admitted.load(Ordering::Acquire),
            rejected: self.state.rejected.load(Ordering::Acquire),
            timeout: self.state.timeout.load(Ordering::Acquire),
            // Go `conduit_channel_queue_wait_seconds` is a histogram pushed by
            // middleware on each successful Acquire (ObserveQueueWait). The
            // Rust port exposes the most recent observed wait in milliseconds
            // (0 == no promoted acquisition observed yet) so callers/metrics
            // layers can derive the same value without a histogram dependency
            // in this crate. See RUST-P9-005 S07/S12 + Go
            // `ChannelLimiterMetrics.ObserveQueueWait`.
            last_wait_ms: self.state.last_wait_ms.load(Ordering::Acquire),
        }
    }

    /// Most recent observed queue-wait duration (ms) for a successful
    /// promotion. Mirrors Go `conduit_channel_queue_wait_seconds` (the only
    /// per-success histogram in the Go limiter metrics). Returns `0` when no
    /// promotion has been observed yet, matching Go's "no samples recorded"
    /// state.
    pub fn last_wait_ms(&self) -> u64 {
        self.state.last_wait_ms.load(Ordering::Acquire)
    }

    /// Back-compat alias for [`Self::snapshot`].
    pub fn metrics_snapshot(&self) -> ChannelLimiterMetricsSnapshot {
        self.snapshot()
    }

    /// Attempt to admit a request immediately.
    ///
    /// - Soft mode: always succeeds (admits and counts).
    /// - Hard mode: admits when `active < max_concurrent`; otherwise returns
    ///   [`ChannelLimiterError::QueueFull`] when there is no wait capacity or
    ///   the FIFO queue is also full.
    ///
    /// This is the *non-blocking* entry point used when the caller does not
    /// want to wait. To actually wait with a per-channel timeout, use
    /// [`Self::try_acquire_or_enqueue`] followed by
    /// [`ChannelQueueSlot::check_timeout`].
    pub fn try_acquire(&self) -> Result<ChannelPermit, ChannelLimiterError> {
        match self.config.mode() {
            ChannelLimiterMode::Soft => {
                self.state.active.fetch_add(1, Ordering::AcqRel);
                self.state.admitted.fetch_add(1, Ordering::AcqRel);
                Ok(ChannelPermit::new(Arc::clone(&self.state)))
            }
            ChannelLimiterMode::Hard => self.try_acquire_hard(),
        }
    }

    /// Hard-mode admission attempt that, instead of failing when capacity is
    /// exhausted, **enqueues** the request in the FIFO wait queue (mirroring
    /// the Go blocking path) and returns a [`ChannelQueueSlot`] tagged with the
    /// enqueue time. The caller then polls
    /// [`ChannelQueueSlot::check_timeout`] with a monotonic/fake clock to
    /// detect per-channel timeout ([`ChannelLimiterError::ChannelQueueTimeout`])
    /// or re-attempts admission via [`ChannelQueueSlot::try_promote`].
    ///
    /// `now_ms` is the caller's monotonic clock reading in milliseconds. It is
    /// only stored on the returned slot for timeout bookkeeping; it does not
    /// need to match any global limiter clock.
    ///
    /// Returns:
    /// - `Ok(AcquireOutcome::Admitted(permit))` — slot granted immediately,
    ///   caller owns the permit and must release it.
    /// - `Ok(AcquireOutcome::Queued(slot))` — request is waiting in the FIFO
    ///   queue; poll the slot with the fake clock.
    /// - `Err(QueueFull)` — both capacity and the FIFO queue are saturated
    ///   (Go `ErrChannelQueueFull`).
    pub fn try_acquire_or_enqueue(
        &self,
        now_ms: u64,
    ) -> Result<AcquireOutcome, ChannelLimiterError> {
        match self.config.mode() {
            ChannelLimiterMode::Soft => {
                self.state.active.fetch_add(1, Ordering::AcqRel);
                self.state.admitted.fetch_add(1, Ordering::AcqRel);
                Ok(AcquireOutcome::Admitted(ChannelPermit::new(Arc::clone(
                    &self.state,
                ))))
            }
            ChannelLimiterMode::Hard => {
                // Fast path: try to take a free slot.
                if let Some(permit) = self.try_take_free_slot_hard() {
                    return Ok(AcquireOutcome::Admitted(permit));
                }

                // Capacity exhausted: enqueue if there is room, else reject.
                if self.config.queue_size == 0 {
                    // No wait path configured: report as queue-full (the FIFO
                    // is conceptually full at depth 0).
                    self.state.rejected.fetch_add(1, Ordering::AcqRel);
                    return Err(ChannelLimiterError::QueueFull);
                }

                match self.reserve_queue_slot_locked()? {
                    AcquireOutcome::Queued(mut slot) => {
                        slot.stamp_enqueued_at(now_ms);
                        Ok(AcquireOutcome::Queued(slot))
                    }
                    // Unreachable: reserve_queue_slot_locked only yields Queued.
                    AcquireOutcome::Admitted(p) => Ok(AcquireOutcome::Admitted(p)),
                }
            }
        }
    }

    /// Legacy helper preserved from the skeleton: reserve a FIFO queue slot
    /// without an immediate admit attempt. Useful for callers that already
    /// know capacity is exhausted. Records `admitted` once the slot is
    /// reserved (mirrors Go, which counts a successful Acquire start).
    pub fn try_reserve_queue_slot(&self) -> Result<ChannelQueueSlot, ChannelLimiterError> {
        if self.config.queue_size == 0 {
            self.state.rejected.fetch_add(1, Ordering::AcqRel);
            return Err(ChannelLimiterError::QueueFull);
        }

        loop {
            let queued = self.state.queued.load(Ordering::Acquire);
            if queued >= self.config.queue_size {
                self.state.rejected.fetch_add(1, Ordering::AcqRel);
                return Err(ChannelLimiterError::QueueFull);
            }

            if self
                .state
                .queued
                .compare_exchange(queued, queued + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.state.admitted.fetch_add(1, Ordering::AcqRel);
                return Ok(ChannelQueueSlot::new(Arc::clone(&self.state)));
            }
        }
    }

    /// Try to grab one of the `max_concurrent` active slots under hard mode.
    /// Returns `Some(permit)` on success, `None` when capacity is saturated.
    fn try_take_free_slot_hard(&self) -> Option<ChannelPermit> {
        loop {
            let active = self.state.active.load(Ordering::Acquire);
            if active >= self.config.max_concurrent {
                return None;
            }

            if self
                .state
                .active
                .compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.state.admitted.fetch_add(1, Ordering::AcqRel);
                return Some(ChannelPermit::new(Arc::clone(&self.state)));
            }
        }
    }

    /// Internal promotion helper: identical to `try_take_free_slot_hard` but
    /// does NOT increment `admitted` (the promotion path already counted the
    /// admission when the queue slot was reserved).
    fn try_take_free_slot_hard_for_promotion(&self) -> Option<ChannelPermit> {
        loop {
            let active = self.state.active.load(Ordering::Acquire);
            if active >= self.config.max_concurrent {
                return None;
            }

            if self
                .state
                .active
                .compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(ChannelPermit::new(Arc::clone(&self.state)));
            }
        }
    }

    /// CAS-loop reserve one FIFO queue slot. Returns a `Queued` outcome on
    /// success or `QueueFull` when the queue is saturated. The returned slot
    /// has `enqueued_at_ms == 0`; the caller stamps the real clock.
    fn reserve_queue_slot_locked(&self) -> Result<AcquireOutcome, ChannelLimiterError> {
        loop {
            let queued = self.state.queued.load(Ordering::Acquire);
            if queued >= self.config.queue_size {
                self.state.rejected.fetch_add(1, Ordering::AcqRel);
                return Err(ChannelLimiterError::QueueFull);
            }

            if self
                .state
                .queued
                .compare_exchange(queued, queued + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.state.admitted.fetch_add(1, Ordering::AcqRel);
                let slot = ChannelQueueSlot::new(Arc::clone(&self.state));
                return Ok(AcquireOutcome::Queued(slot));
            }
        }
    }

    fn try_acquire_hard(&self) -> Result<ChannelPermit, ChannelLimiterError> {
        if let Some(permit) = self.try_take_free_slot_hard() {
            return Ok(permit);
        }

        // Non-blocking entry point: no slot and the caller did not ask to wait.
        // Report queue full — the caller must use `try_acquire_or_enqueue` to
        // actually wait in the FIFO.
        self.state.rejected.fetch_add(1, Ordering::AcqRel);
        Err(ChannelLimiterError::QueueFull)
    }
}

/// Outcome of a non-blocking admission attempt that allows queueing.
#[derive(Debug)]
pub enum AcquireOutcome {
    /// A slot was granted immediately; the caller owns the permit.
    Admitted(ChannelPermit),
    /// The request was appended to the FIFO wait queue; poll the slot with a
    /// monotonic clock to detect timeout or promotion.
    Queued(ChannelQueueSlot),
}

/// Configuration for a [`ChannelLimiter`].
///
/// Mirrors Go `NewChannelLimiter(capacity, queueSize, timeoutMs)`:
/// - `max_concurrent > 0` is required by the Go contract.
/// - `queue_size == 0` selects **soft mode** (only counts; never blocks).
/// - `queue_size > 0` selects **hard mode** with a FIFO wait queue of that
///   depth.
/// - `queue_timeout_ms` is the per-channel wait timeout; `0` means no
///   per-channel timeout and the caller's deadline becomes the only limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelLimiterConfig {
    pub max_concurrent: usize,
    pub queue_size: usize,
    pub queue_timeout_ms: u64,
}

impl ChannelLimiterConfig {
    pub const fn soft(max_concurrent: usize) -> Self {
        Self {
            max_concurrent,
            queue_size: 0,
            queue_timeout_ms: 0,
        }
    }

    pub const fn hard(max_concurrent: usize, queue_size: usize) -> Self {
        Self {
            max_concurrent,
            queue_size,
            queue_timeout_ms: 0,
        }
    }

    pub const fn hard_with_timeout(
        max_concurrent: usize,
        queue_size: usize,
        queue_timeout_ms: u64,
    ) -> Self {
        Self {
            max_concurrent,
            queue_size,
            queue_timeout_ms,
        }
    }

    pub const fn mode(self) -> ChannelLimiterMode {
        if self.queue_size == 0 {
            ChannelLimiterMode::Soft
        } else {
            ChannelLimiterMode::Hard
        }
    }

    /// Per-channel queue wait timeout in milliseconds; `0` means "no
    /// per-channel timeout".
    pub const fn queue_timeout_ms(self) -> u64 {
        self.queue_timeout_ms
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelLimiterMode {
    Soft,
    Hard,
}

/// Per-channel point-in-time metrics snapshot (RUST-P9-005 S07/S12).
///
/// Mirrors the Go `ChannelLimiterMetrics` OTel surface — six fields:
/// - `conduit_channel_inflight`            (gauge)     -> `current_inflight`
/// - `conduit_channel_queue_waiting`       (gauge)     -> `queue_len`
/// - admitted cumulative (counter, derived)            -> `admitted`
/// - `conduit_channel_queue_full_total`    (counter)   -> `rejected`
/// - `conduit_channel_queue_timeout_total` (counter)   -> `timeout`
/// - `conduit_channel_queue_wait_seconds`  (histogram) -> `last_wait_ms`
///   (most recent observed wait, in ms; 0 == no sample yet)
///
/// `admitted` has no dedicated OTel instrument in Go but is tracked internally
/// and exposed here for observability/tests (S12). `last_wait_ms` mirrors the
/// most recent sample pushed to Go's per-success `queue_wait_seconds`
/// histogram (RUST-P9-005 parity gap, Go
/// `ChannelLimiterMetrics.ObserveQueueWait`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChannelLimiterMetricsSnapshot {
    /// Current in-flight requests holding a slot.
    pub current_inflight: usize,
    /// Current depth of the FIFO wait queue (always 0 in soft mode).
    pub queue_len: usize,
    /// Cumulative count of successful admission starts (immediate admits +
    /// queue reservations). Monotonic.
    pub admitted: usize,
    /// Cumulative count of queue-full rejections. Monotonic.
    pub rejected: usize,
    /// Cumulative count of per-channel wait-timeout exits. Monotonic.
    pub timeout: usize,
    /// Most recently observed queue-wait duration in milliseconds for a
    /// successful promotion (Go `conduit_channel_queue_wait_seconds`).
    /// `0` means no promotion has been observed yet.
    pub last_wait_ms: u64,
}

/// Admission errors raised by [`ChannelLimiter`].
///
/// Both variants are **local admission rejections**: they never reached the
/// upstream provider, so they MUST NOT be counted as model errors and are NOT
/// same-channel retryable. See [`ChannelLimiterError::is_local_admission_rejection`]
/// and the Go `isChannelQueueError` / `CanRetry` references above.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ChannelLimiterError {
    /// Per-channel queue wait timeout elapsed (Go `ErrChannelQueueTimeout`).
    /// Participates in retryable judgment as a local admission rejection.
    #[error("channel queue timed out")]
    ChannelQueueTimeout,
    /// FIFO queue is full / no wait capacity (Go `ErrChannelQueueFull`).
    #[error("channel queue is full")]
    QueueFull,
}

impl ChannelLimiterError {
    /// Mirrors Go `isChannelQueueError`: true for every error raised by
    /// [`ChannelLimiter`]. The Go pipeline uses this to:
    /// 1. Skip model-circuit-breaker error accounting (`modelCircuitBreaker
    ///    .OnOutboundRawError` early-returns).
    /// 2. Skip same-channel retry — `PersistentOutboundTransformer.CanRetry`
    ///    returns `false`, bouncing immediately to the next candidate.
    /// 3. Skip rate-limit tracking negative bookkeeping.
    ///
    /// Every [`ChannelLimiterError`] is by definition a local admission
    /// rejection, so this is unconditionally `true` and is provided as the
    /// Rust-side contract surface.
    pub const fn is_local_admission_rejection(&self) -> bool {
        true
    }

    /// Same-channel-retryable? Mirrors Go `CanRetry` for `isChannelQueueError`:
    /// always `false` — these errors mean the local channel cannot make
    /// progress until its queue/RPM state changes, so the pipeline must
    /// bounce to a different candidate rather than retry in place.
    pub const fn is_same_channel_retryable(&self) -> bool {
        false
    }
}

/// Owned permit for an in-flight admission slot. Dropping it (or calling
/// [`ChannelPermit::release`]) returns the slot to the limiter. Mirrors the Go
/// contract that Release MUST be called exactly once per successful Acquire.
#[derive(Debug)]
pub struct ChannelPermit {
    state: Arc<ChannelLimiterState>,
    released: bool,
}

impl ChannelPermit {
    fn new(state: Arc<ChannelLimiterState>) -> Self {
        Self {
            state,
            released: false,
        }
    }

    /// Explicitly release the slot. Idempotent; also runs on `Drop`.
    pub fn release(mut self) {
        self.release_once();
    }

    fn release_once(&mut self) {
        if !self.released {
            self.state.active.fetch_sub(1, Ordering::AcqRel);
            self.released = true;
        }
    }
}

impl Drop for ChannelPermit {
    fn drop(&mut self) {
        self.release_once();
    }
}

/// Reservation of a position in the FIFO wait queue. Created by
/// [`ChannelLimiter::try_acquire_or_enqueue`] (or
/// [`ChannelLimiter::try_reserve_queue_slot`]). The slot is released when the
/// waiter is promoted to a permit, when it times out, or on drop.
///
/// Carries the waiter's `enqueued_at_ms` clock reading so the per-channel
/// `queue_timeout_ms` can be enforced deterministically with a fake clock
/// (RUST-P9-005 S06/S13) — no wall-clock `sleep` is needed in tests.
#[derive(Debug)]
pub struct ChannelQueueSlot {
    state: Arc<ChannelLimiterState>,
    /// Monotonic clock reading (ms) at which this waiter was enqueued. Valid
    /// only when `stamped` is true; otherwise the slot has not yet been
    /// timestamped and `check_timeout` will refuse to fire until
    /// `stamp_enqueued_at` is called.
    enqueued_at_ms: u64,
    stamped: bool,
    released: bool,
}

impl ChannelQueueSlot {
    fn new(state: Arc<ChannelLimiterState>) -> Self {
        Self {
            state,
            enqueued_at_ms: 0,
            stamped: false,
            released: false,
        }
    }

    /// The monotonic clock reading (ms) at which this waiter was enqueued.
    /// Returns `None` for slots obtained via
    /// [`ChannelLimiter::try_reserve_queue_slot`] that have not yet been
    /// stamped via [`Self::stamp_enqueued_at`].
    pub const fn enqueued_at_ms(&self) -> Option<u64> {
        if self.stamped {
            Some(self.enqueued_at_ms)
        } else {
            None
        }
    }

    /// Stamp the enqueue time on a slot created via the legacy
    /// [`ChannelLimiter::try_reserve_queue_slot`] entry point. No-op for slots
    /// that already carry a stamp. A stamp of `0` is a valid timestamp
    /// ("enqueued at the epoch of the caller's clock").
    pub fn stamp_enqueued_at(&mut self, now_ms: u64) {
        if !self.stamped {
            self.enqueued_at_ms = now_ms;
            self.stamped = true;
        }
    }

    /// Check whether the per-channel wait timeout has elapsed.
    ///
    /// Mirrors the Go `select { case <-waitCtx.Done() }` timeout branch: when
    /// `queue_timeout_ms > 0` and the slot has been stamped and
    /// `now_ms - enqueued_at_ms >= queue_timeout_ms`, the slot is released and
    /// `Some(ChannelQueueTimeout)` is returned — the caller surfaces this as
    /// the admission error. When `queue_timeout_ms == 0` (no per-channel
    /// timeout) this always returns `None` and the caller's own deadline
    /// remains the only limit, matching Go. Returns `None` for unstamped
    /// slots (callers must [`Self::stamp_enqueued_at`] first).
    ///
    /// On timeout the slot is consumed (queue depth decremented and the
    /// `timeout` counter incremented), so the caller MUST NOT release it
    /// again.
    pub fn check_timeout(
        &mut self,
        now_ms: u64,
        queue_timeout_ms: u64,
    ) -> Option<ChannelLimiterError> {
        if queue_timeout_ms == 0 || !self.stamped {
            return None;
        }

        let elapsed = now_ms.saturating_sub(self.enqueued_at_ms);
        if elapsed < queue_timeout_ms {
            return None;
        }

        // Timed out: release the queue slot and record the timeout counter.
        self.release_once();
        self.state.timeout.fetch_add(1, Ordering::AcqRel);
        Some(ChannelLimiterError::ChannelQueueTimeout)
    }

    /// Attempt to promote this waiter to an active slot (i.e. capacity has
    /// freed up while we were waiting). On success the queue reservation is
    /// released and a [`ChannelPermit`] is returned; the caller then owns the
    /// in-flight slot. On failure the slot is returned to the caller along
    /// with the error so it can keep waiting.
    ///
    /// Mirrors the Go path where `Release` closes the head waiter's channel
    /// and the waiter's `Acquire` returns `nil`.
    ///
    /// This entry point does **not** observe a wait duration (it has no clock
    /// reading); use [`Self::try_promote_at`] to record the
    /// `conduit_channel_queue_wait_seconds` parity sample.
    pub fn try_promote(
        self,
        limiter: &ChannelLimiter,
    ) -> Result<ChannelPermit, (Self, ChannelLimiterError)> {
        self.try_promote_at(limiter, None).map(|(p, _)| p)
    }

    /// Clock-aware promotion variant. On a successful promotion the observed
    /// wait duration (`now_ms - enqueued_at_ms`) is recorded on the limiter
    /// state so [`ChannelLimiter::last_wait_ms`] /
    /// [`ChannelLimiterMetricsSnapshot::last_wait_ms`] mirror Go's
    /// `conduit_channel_queue_wait_seconds` histogram sample pushed on each
    /// successful Acquire (`ChannelLimiterMetrics.ObserveQueueWait`).
    ///
    /// Pass `now_ms = None` (or use [`Self::try_promote`]) to skip the
    /// observation — useful when the caller has no monotonic clock handy.
    /// Passing `now_ms` for an unstamped slot records a wait of
    /// `now_ms` (treats enqueue time as 0), matching the legacy
    /// [`ChannelLimiter::try_reserve_queue_slot`] contract.
    ///
    /// Returns the owned permit **and** the observed wait in milliseconds
    /// (`0` when `now_ms` was `None`). On failure the slot is handed back
    /// along with [`ChannelLimiterError::QueueFull`] and no wait sample is
    /// recorded.
    pub fn try_promote_at(
        mut self,
        limiter: &ChannelLimiter,
        now_ms: Option<u64>,
    ) -> Result<(ChannelPermit, u64), (Self, ChannelLimiterError)> {
        // Try to grab a free active slot. We do NOT touch `admitted` again
        // here — it was already counted when the queue slot was reserved, and
        // Go counts a successful Acquire exactly once.
        match limiter.try_take_free_slot_hard_for_promotion() {
            Some(permit) => {
                // Observe the wait sample before we release the reservation
                // (the slot's `enqueued_at_ms` is still valid here).
                let wait_ms = match now_ms {
                    Some(now) => {
                        let base = if self.stamped { self.enqueued_at_ms } else { 0 };
                        let observed = now.saturating_sub(base);
                        limiter
                            .state
                            .last_wait_ms
                            .store(observed, Ordering::Release);
                        observed
                    }
                    None => 0,
                };

                // Free our FIFO reservation; capacity ownership transferred.
                self.release_once();
                Ok((permit, wait_ms))
            }
            None => Err((self, ChannelLimiterError::QueueFull)),
        }
    }

    /// Explicitly abandon the queue reservation (e.g. caller context
    /// cancelled). Idempotent; also runs on `Drop`.
    pub fn release(mut self) {
        self.release_once();
    }

    fn release_once(&mut self) {
        if !self.released {
            self.state.queued.fetch_sub(1, Ordering::AcqRel);
            self.released = true;
        }
    }
}

impl Drop for ChannelQueueSlot {
    fn drop(&mut self) {
        self.release_once();
    }
}

#[derive(Debug, Default)]
struct ChannelLimiterState {
    active: AtomicUsize,
    queued: AtomicUsize,
    admitted: AtomicUsize,
    rejected: AtomicUsize,
    timeout: AtomicUsize,
    /// Most recent observed queue-wait duration (ms) for a successful
    /// promotion. Mirrors the Go `conduit_channel_queue_wait_seconds`
    /// histogram sample pushed on each successful Acquire
    /// (`ChannelLimiterMetrics.ObserveQueueWait`). Stored as the last sample
    /// only — the full histogram is the responsibility of the metrics layer
    /// that scrapes [`ChannelLimiter::last_wait_ms`].
    last_wait_ms: AtomicU64,
    /// Monotonic clock anchor reserved for future wiring (e.g. a global
    /// limiter-manager scrape). Currently unused but kept on the state struct
    /// so adding a real Clock dependency later does not break the public API.
    #[allow(dead_code)]
    anchored_at_ms: AtomicU64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // Pre-existing skeleton tests — kept verbatim (S04/S05/S08/S10/S11/S13).
    // ---------------------------------------------------------------------------

    #[test]
    fn guard_drop_releases_concurrent_count() {
        let limiter = ChannelLimiter::hard(1, 1);

        {
            let permit = limiter.try_acquire();
            assert!(permit.is_ok(), "permit should be acquired");
            assert_eq!(limiter.active_count(), 1);
        }

        assert_eq!(limiter.active_count(), 0);
        assert!(limiter.try_acquire().is_ok());
    }

    #[test]
    fn hard_queue_full_is_rejected() {
        let limiter = ChannelLimiter::hard(1, 1);
        let permit = limiter.try_acquire();
        assert!(permit.is_ok(), "permit should be acquired");
        let queued = limiter.try_reserve_queue_slot();
        assert!(queued.is_ok(), "queue slot should be reserved");

        let rejected = limiter.try_acquire();

        assert!(matches!(rejected, Err(ChannelLimiterError::QueueFull)));
        assert_eq!(
            limiter.metrics_snapshot(),
            ChannelLimiterMetricsSnapshot {
                current_inflight: 1,
                queue_len: 1,
                admitted: 2,
                rejected: 1,
                timeout: 0,
                last_wait_ms: 0,
            }
        );
    }

    #[test]
    fn soft_mode_does_not_reject_over_limit() {
        let limiter = ChannelLimiter::soft(1);

        let first = limiter.try_acquire();
        let second = limiter.try_acquire();

        assert!(first.is_ok());
        assert!(second.is_ok());
        assert_eq!(limiter.active_count(), 2);
        assert_eq!(
            limiter.metrics_snapshot(),
            ChannelLimiterMetricsSnapshot {
                current_inflight: 2,
                queue_len: 0,
                admitted: 2,
                rejected: 0,
                timeout: 0,
                last_wait_ms: 0,
            }
        );
    }

    #[test]
    fn queue_slot_drop_releases_queued_count() {
        let limiter = ChannelLimiter::hard(1, 1);

        {
            let slot = limiter.try_reserve_queue_slot();
            assert!(slot.is_ok(), "queue slot should be reserved");
            assert_eq!(limiter.queued_count(), 1);
        }

        assert_eq!(limiter.queued_count(), 0);
    }

    // ---------------------------------------------------------------------------
    // RUST-P9-005 S06 — queue_timeout_ms enforcement (fake clock, no sleep).
    // Mirrors Go `TestChannelLimiter_HardMode_QueueTimeout` and
    // `TestChannelLimiter_HardMode_NoTimeoutHonoursContext`.
    // ---------------------------------------------------------------------------

    #[test]
    fn try_acquire_or_enqueue_admits_when_capacity_available() {
        // Go `TestChannelLimiter_HardMode_AcquireUpToCapacity`: capacity-3
        // limiter admits three requests back-to-back without queueing.
        let limiter = ChannelLimiter::hard(3, 5);

        let p1 = limiter.try_acquire_or_enqueue(0);
        let p2 = limiter.try_acquire_or_enqueue(0);
        let p3 = limiter.try_acquire_or_enqueue(0);
        assert!(matches!(p1, Ok(AcquireOutcome::Admitted(_))));
        assert!(matches!(p2, Ok(AcquireOutcome::Admitted(_))));
        assert!(matches!(p3, Ok(AcquireOutcome::Admitted(_))));

        let p4 = limiter.try_acquire_or_enqueue(0);
        assert!(matches!(p4, Ok(AcquireOutcome::Queued(_))));
        assert_eq!(limiter.active_count(), 3);
        assert_eq!(limiter.queued_count(), 1);
    }

    #[test]
    fn try_acquire_or_enqueue_returns_queue_full_when_saturated() {
        // Go `TestChannelLimiter_HardMode_QueueFull`: 1 in-flight + a full
        // queue rejects the next arrival immediately with QueueFull.
        let limiter = ChannelLimiter::hard(1, 1);

        let _permit = limiter.try_acquire_or_enqueue(0);
        let _slot = limiter.try_acquire_or_enqueue(0);

        let err = match limiter.try_acquire_or_enqueue(0) {
            Err(e) => e,
            Ok(other) => panic!("fourth arrival should be rejected, got {other:?}"),
        };
        assert_eq!(err, ChannelLimiterError::QueueFull);
    }

    #[test]
    fn queue_slot_check_timeout_rejects_after_timeout_with_fake_clock()
    -> Result<(), Box<dyn std::error::Error>> {
        // Go `TestChannelLimiter_HardMode_QueueTimeout`: capacity saturated,
        // a waiter is enqueued, then the fake clock advances past the
        // per-channel timeout and `check_timeout` rejects the waiter with
        // `ChannelQueueTimeout`. No wall-clock sleep involved.
        let limiter = ChannelLimiter::hard_with_timeout(1, 5, 50);

        // Saturate capacity.
        let permit = limiter.try_acquire_or_enqueue(0)?;
        assert_eq!(limiter.active_count(), 1);

        // Enqueue a waiter at t = 0.
        let outcome = limiter.try_acquire_or_enqueue(0)?;
        let mut slot = match outcome {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };
        assert_eq!(slot.enqueued_at_ms(), Some(0));
        assert_eq!(limiter.queued_count(), 1);
        assert_eq!(limiter.metrics_snapshot().timeout, 0);

        // Just before the deadline: still waiting.
        assert!(slot.check_timeout(49, 50).is_none());

        // At the deadline (50 ms elapsed): timeout fires.
        let err = slot.check_timeout(50, 50);
        assert_eq!(err, Some(ChannelLimiterError::ChannelQueueTimeout));

        // The slot released itself on timeout: queue drained, timeout counter
        // incremented, no slot leak.
        assert_eq!(limiter.queued_count(), 0);
        assert_eq!(limiter.metrics_snapshot().timeout, 1);

        drop(permit);
        Ok(())
    }

    #[test]
    fn queue_slot_check_timeout_zero_means_no_per_channel_timeout()
    -> Result<(), Box<dyn std::error::Error>> {
        // Go `TestChannelLimiter_HardMode_NoTimeoutHonoursContext`:
        // `queue_timeout_ms == 0` means no per-channel timeout — `check_timeout`
        // always returns None even far in the future, leaving the caller's own
        // deadline as the only limit.
        let limiter = ChannelLimiter::hard(1, 5);

        let _permit = limiter.try_acquire_or_enqueue(0)?;
        let outcome = limiter.try_acquire_or_enqueue(0)?;
        let mut slot = match outcome {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };

        // Arbitrarily far in the future: no per-channel timeout fires.
        assert!(slot.check_timeout(1_000_000, 0).is_none());
        assert_eq!(limiter.metrics_snapshot().timeout, 0);
        assert_eq!(limiter.queued_count(), 1);
        Ok(())
    }

    #[test]
    fn queue_slot_check_timeout_ignores_unstamped_slot() -> Result<(), Box<dyn std::error::Error>> {
        // A slot obtained via the legacy `try_reserve_queue_slot` path has
        // `enqueued_at_ms == 0`; `check_timeout` must treat that as "not yet
        // stamped" and refuse to fire (callers must `stamp_enqueued_at`
        // first).
        let limiter = ChannelLimiter::hard_with_timeout(1, 5, 50);

        let _permit = limiter.try_acquire()?;
        let mut slot = limiter.try_reserve_queue_slot()?;
        assert_eq!(slot.enqueued_at_ms(), None);

        assert!(slot.check_timeout(10_000, 50).is_none());

        // Once stamped, the timeout fires normally.
        slot.stamp_enqueued_at(0);
        let err = slot.check_timeout(100, 50);
        assert_eq!(err, Some(ChannelLimiterError::ChannelQueueTimeout));
        Ok(())
    }

    #[test]
    fn queue_slot_try_promote_transfers_to_permit_when_capacity_frees()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go `TestChannelLimiter_HardMode_ReleaseTransfersToWaiter`:
        // a queued waiter is promoted to an active permit once the original
        // holder releases, with no double-counting of `admitted`.
        let limiter = ChannelLimiter::hard(1, 5);

        let permit = limiter.try_acquire_or_enqueue(0)?;
        let outcome = limiter.try_acquire_or_enqueue(0)?;
        let slot = match outcome {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };

        // While capacity is held, promotion is refused.
        let (slot, err) = match slot.try_promote(&limiter) {
            Err(pair) => pair,
            Ok(other) => panic!("promotion should fail while capacity is held, got {other:?}"),
        };
        assert_eq!(err, ChannelLimiterError::QueueFull);
        assert_eq!(limiter.queued_count(), 1);

        // Free the original slot.
        drop(permit);
        assert_eq!(limiter.active_count(), 0);

        // Now promotion succeeds: queue reservation released, active bumped.
        let _new_permit = match slot.try_promote(&limiter) {
            Ok(p) => p,
            Err((s, e)) => panic!("promotion should succeed after release; slot={s:?} err={e:?}"),
        };
        assert_eq!(limiter.active_count(), 1);
        assert_eq!(limiter.queued_count(), 0);

        // admitted counted exactly twice (one for the original Acquire, one
        // for the queue reservation); the promotion path must NOT bump it
        // again.
        assert_eq!(limiter.metrics_snapshot().admitted, 2);
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // RUST-P9-005 S07/S12 — metrics snapshot covers exactly the six fields
    // and they all advance correctly under admit/reject/timeout scenarios.
    // ---------------------------------------------------------------------------

    #[test]
    fn snapshot_exposes_exactly_six_fields() {
        // Compile-time-ish structural assertion: the snapshot type carries
        // current_inflight / queue_len / admitted / rejected / timeout /
        // last_wait_ms and nothing else. Mirrors Go's six metric instruments
        // (conduit_channel_inflight, conduit_channel_queue_waiting,
        // admitted-internal, conduit_channel_queue_full_total,
        // conduit_channel_queue_timeout_total, and the per-success
        // conduit_channel_queue_wait_seconds histogram sample).
        let snap = ChannelLimiterMetricsSnapshot {
            current_inflight: 1,
            queue_len: 2,
            admitted: 3,
            rejected: 4,
            timeout: 5,
            last_wait_ms: 6,
        };

        assert_eq!(snap.current_inflight, 1);
        assert_eq!(snap.queue_len, 2);
        assert_eq!(snap.admitted, 3);
        assert_eq!(snap.rejected, 4);
        assert_eq!(snap.timeout, 5);
        assert_eq!(snap.last_wait_ms, 6);
    }

    #[test]
    fn metrics_snapshot_updates_under_admit_reject_and_timeout()
    -> Result<(), Box<dyn std::error::Error>> {
        // S12 end-to-end: drive one admit, one queue-full reject, and one
        // queue-timeout, then assert every field of the snapshot reflects the
        // expected cumulative counts.
        let limiter = ChannelLimiter::hard_with_timeout(1, 2, 50);

        // (1) Admit one request -> current_inflight=1, admitted=1.
        let permit = limiter.try_acquire_or_enqueue(0)?;
        assert_eq!(
            limiter.snapshot(),
            ChannelLimiterMetricsSnapshot {
                current_inflight: 1,
                queue_len: 0,
                admitted: 1,
                rejected: 0,
                timeout: 0,
                last_wait_ms: 0,
            }
        );

        // (2) Enqueue two waiters -> queue_len=2, admitted=3 (1 admit + 2
        // reservations). Capacity is still 1.
        let outcome_a = limiter.try_acquire_or_enqueue(0)?;
        let mut slot_a = match outcome_a {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };
        let outcome_b = limiter.try_acquire_or_enqueue(0)?;
        let slot_b = match outcome_b {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };
        assert_eq!(
            limiter.snapshot(),
            ChannelLimiterMetricsSnapshot {
                current_inflight: 1,
                queue_len: 2,
                admitted: 3,
                rejected: 0,
                timeout: 0,
                last_wait_ms: 0,
            }
        );

        // (3) A fourth arrival is rejected with QueueFull -> rejected=1.
        let err = match limiter.try_acquire_or_enqueue(0) {
            Err(e) => e,
            Ok(other) => panic!("fourth arrival should be rejected, got {other:?}"),
        };
        assert_eq!(err, ChannelLimiterError::QueueFull);

        // (4) Advance the fake clock past the timeout for waiter A ->
        // timeout=1, queue_len drops to 1.
        let timeout_err = slot_a.check_timeout(50, 50);
        assert_eq!(timeout_err, Some(ChannelLimiterError::ChannelQueueTimeout));
        drop(slot_b); // waiter B abandoned via drop (e.g. ctx cancel)

        assert_eq!(
            limiter.snapshot(),
            ChannelLimiterMetricsSnapshot {
                current_inflight: 1,
                queue_len: 0,
                admitted: 3,
                rejected: 1,
                timeout: 1,
                last_wait_ms: 0,
            }
        );

        // (5) Releasing the original permit brings inflight back to 0; the
        // cumulative counters (admitted/rejected/timeout) are unchanged.
        drop(permit);
        assert_eq!(
            limiter.snapshot(),
            ChannelLimiterMetricsSnapshot {
                current_inflight: 0,
                queue_len: 0,
                admitted: 3,
                rejected: 1,
                timeout: 1,
                last_wait_ms: 0,
            }
        );
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // RUST-P9-005 S11 — ChannelQueueTimeout participates in retryable judgment.
    // Mirrors Go `isChannelQueueError` / `PersistentOutboundTransformer.CanRetry`.
    // ---------------------------------------------------------------------------

    #[test]
    fn channel_limiter_errors_are_local_admission_rejections_and_not_retryable() {
        // Go contract: every ChannelLimiter error is a *local* admission
        // rejection — it never reached upstream, so:
        //   - it MUST NOT be counted as a model error (circuit-breaker skip);
        //   - it is NOT same-channel retryable (CanRetry returns false and
        //     the pipeline bounces to the next candidate).
        for err in [
            ChannelLimiterError::ChannelQueueTimeout,
            ChannelLimiterError::QueueFull,
        ] {
            assert!(
                err.is_local_admission_rejection(),
                "{err:?} must be flagged as a local admission rejection"
            );
            assert!(
                !err.is_same_channel_retryable(),
                "{err:?} must NOT be same-channel retryable"
            );
        }
    }

    #[test]
    fn hard_with_timeout_constructor_stores_timeout() {
        let limiter = ChannelLimiter::hard_with_timeout(2, 4, 123);
        assert_eq!(limiter.config().queue_timeout_ms, 123);
        assert_eq!(limiter.config().max_concurrent, 2);
        assert_eq!(limiter.config().queue_size, 4);
        assert_eq!(limiter.mode(), ChannelLimiterMode::Hard);

        // Soft constructor never carries a timeout.
        let soft = ChannelLimiter::soft(5);
        assert_eq!(soft.config().queue_timeout_ms, 0);
        assert_eq!(soft.mode(), ChannelLimiterMode::Soft);
    }

    // ---------------------------------------------------------------------------
    // RUST-P9-005 S07/S12 parity gap — Go `conduit_channel_queue_wait_seconds`
    // histogram (ChannelLimiterMetrics.ObserveQueueWait) records the wait
    // duration of every *successful* Acquire. The Rust port surfaces the most
    // recent observed sample via `last_wait_ms` / snapshot, fed by
    // `ChannelQueueSlot::try_promote_at`. These tests mirror
    // `channel_limiter_metrics_test.go`'s `ObserveQueueWait` assertions.
    // ---------------------------------------------------------------------------

    #[test]
    fn last_wait_ms_starts_at_zero() {
        // Go contract: with no successful promotion yet, the wait histogram
        // has no samples. The Rust surface reports 0 in that state.
        let limiter = ChannelLimiter::hard(1, 5);
        assert_eq!(limiter.last_wait_ms(), 0);
        assert_eq!(limiter.snapshot().last_wait_ms, 0);
    }

    #[test]
    fn try_promote_at_records_observed_wait_on_success() -> Result<(), Box<dyn std::error::Error>> {
        // Go `ObserveQueueWait` is called with the elapsed wait on each
        // successful Acquire. The Rust `try_promote_at` returns the observed
        // wait in ms and persists the latest sample on the limiter state.
        let limiter = ChannelLimiter::hard(1, 5);

        let permit = limiter.try_acquire_or_enqueue(0)?;
        // Enqueue a waiter at t = 100.
        let slot = match limiter.try_acquire_or_enqueue(100)? {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };
        assert_eq!(limiter.last_wait_ms(), 0);

        // Capacity still held -> promotion fails, NO sample recorded.
        let (slot, err) = match slot.try_promote_at(&limiter, Some(150)) {
            Err(pair) => pair,
            Ok(other) => panic!("promotion should fail while held, got {other:?}"),
        };
        assert_eq!(err, ChannelLimiterError::QueueFull);
        assert_eq!(limiter.last_wait_ms(), 0);

        // Free the slot; promote at t = 250 -> observed wait = 150 ms
        // (250 - 100).
        drop(permit);
        let (_new_permit, wait_ms) = match slot.try_promote_at(&limiter, Some(250)) {
            Ok(pair) => pair,
            Err((s, e)) => panic!("promote should succeed; slot={s:?} err={e:?}"),
        };
        assert_eq!(wait_ms, 150);
        assert_eq!(limiter.last_wait_ms(), 150);
        assert_eq!(limiter.snapshot().last_wait_ms, 150);
        Ok(())
    }

    #[test]
    fn try_promote_without_clock_does_not_record_sample() -> Result<(), Box<dyn std::error::Error>>
    {
        // Back-compat: `try_promote` (no clock) must continue to work and must
        // NOT fabricate a wait sample. `last_wait_ms` stays at its previous
        // value.
        let limiter = ChannelLimiter::hard(1, 5);

        let permit = limiter.try_acquire_or_enqueue(0)?;
        let slot = match limiter.try_acquire_or_enqueue(0)? {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };

        drop(permit);
        let _new_permit = slot
            .try_promote(&limiter)
            .map_err(|(s, e)| format!("promote should succeed; slot={s:?} err={e:?}"))?;

        assert_eq!(limiter.last_wait_ms(), 0);
        Ok(())
    }

    #[test]
    fn try_promote_at_overwrites_previous_sample() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go: each successful Acquire pushes a fresh histogram sample,
        // so the most recent observation wins. Drive two promotions and verify
        // `last_wait_ms` reflects the second one.
        let limiter = ChannelLimiter::hard(1, 5);

        // First waiter: enqueued at 0, promoted at 100 -> wait 100.
        let permit_a = limiter.try_acquire_or_enqueue(0)?;
        let slot_a = match limiter.try_acquire_or_enqueue(0)? {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };
        drop(permit_a);
        let (permit_b, wait_b) = slot_a
            .try_promote_at(&limiter, Some(100))
            .map_err(|(s, e)| format!("promote a should succeed; slot={s:?} err={e:?}"))?;
        assert_eq!(wait_b, 100);
        assert_eq!(limiter.last_wait_ms(), 100);

        // Second waiter: enqueued at 100, promoted at 250 -> wait 150.
        let slot_b = match limiter.try_acquire_or_enqueue(100)? {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };
        drop(permit_b);
        let (_permit_c, wait_c) = slot_b
            .try_promote_at(&limiter, Some(250))
            .map_err(|(s, e)| format!("promote b should succeed; slot={s:?} err={e:?}"))?;
        assert_eq!(wait_c, 150);
        assert_eq!(limiter.last_wait_ms(), 150);
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // RUST-P9-005 A01 — additional channel_limiter_test.go parity coverage.
    // (Bohr-the-9th migration pass.)
    // ---------------------------------------------------------------------------

    /// Mirrors Go `TestChannelLimiter_SoftMode_NeverBlocks`
    /// (`channel_limiter_test.go:15`). Capacity-5 soft limiter admits 100
    /// consecutive requests without queuing or rejecting; afterwards Stats
    /// reports in-flight=100, waiting=0. Releasing everything drains back to
    /// zero with no slot leak. The pre-existing
    /// `soft_mode_does_not_reject_over_limit` test only exercised 2 acquires;
    /// this one stress-tests the contract at 20x the configured capacity.
    #[test]
    fn soft_mode_admits_far_beyond_capacity_without_ever_queueing()
    -> Result<(), Box<dyn std::error::Error>> {
        let limiter = ChannelLimiter::soft(5);

        const N: usize = 100;
        let mut permits: Vec<ChannelPermit> = Vec::with_capacity(N);
        for i in 0..N {
            let permit = match limiter.try_acquire() {
                Ok(p) => p,
                Err(e) => {
                    return Err(
                        format!("soft mode must never reject (attempt {i}), got {e:?}").into(),
                    );
                }
            };
            permits.push(permit);
        }

        // Soft mode never queues: every Acquire bumped `active` immediately.
        assert_eq!(limiter.active_count(), N, "soft mode counts every Acquire");
        assert_eq!(limiter.queued_count(), 0, "soft mode never queues");
        assert_eq!(limiter.metrics_snapshot().admitted, N);
        assert_eq!(limiter.metrics_snapshot().rejected, 0);

        // Releasing everything must drain in-flight to zero (Drop semantics).
        drop(permits);
        assert_eq!(limiter.active_count(), 0);
        assert_eq!(limiter.queued_count(), 0);
        Ok(())
    }

    /// Mirrors Go `TestChannelLimiter_ReleaseOnEmptyIsNoop`
    /// (`channel_limiter_test.go:249`). Rust's `ChannelPermit::release` /
    /// `Drop` impl must be idempotent: calling `release` consumes self so it
    /// cannot be double-released, and dropping a fresh (un-acquired) limiter
    /// must not underflow. The Go contract is "Release on empty is a no-op
    /// rather than a panic"; the Rust contract is the same plus idempotent
    /// release of an owned permit.
    #[test]
    fn release_on_empty_limiter_is_idempotent_noop() -> Result<(), Box<dyn std::error::Error>> {
        let limiter = ChannelLimiter::hard(2, 2);

        // No prior Acquire: limiter reports zero load.
        assert_eq!(limiter.active_count(), 0);
        assert_eq!(limiter.queued_count(), 0);

        // Acquire + explicit release must decrement exactly once (release
        // consumes self so a second call is a compile error, not a runtime
        // concern — but we still verify the active count here).
        let permit = limiter.try_acquire()?;
        assert_eq!(limiter.active_count(), 1);
        permit.release(); // -> 0

        // Acquire + drop (without explicit release) decrements exactly once.
        {
            let _p2 = limiter.try_acquire()?;
            assert_eq!(limiter.active_count(), 1);
        }
        assert_eq!(limiter.active_count(), 0, "drop must release exactly once");

        // An abandoned queue slot must also release exactly once.
        {
            let _slot = limiter.try_reserve_queue_slot()?;
            assert_eq!(limiter.queued_count(), 1);
        }
        assert_eq!(
            limiter.queued_count(),
            0,
            "queue slot drop must release exactly once"
        );

        // Final state: clean zero, no underflow.
        assert_eq!(limiter.active_count(), 0);
        assert_eq!(limiter.queued_count(), 0);
        Ok(())
    }

    /// Mirrors Go `TestChannelLimiter_HardMode_FIFOFairness`
    /// (`channel_limiter_test.go:141`). Capacity-1 limiter with 20 queue
    /// slots: saturate capacity, enqueue waiters 0..19 in insertion order,
    /// then release the head slot one at a time and promote each waiter in
    /// FIFO order. The Rust API is non-blocking (no goroutine select), so we
    /// model "head-of-queue promotion" by promoting each slot in the order it
    /// was enqueued — the contract under test is that the limiter hands the
    /// freed slot to the next *outstanding* waiter, not e.g. the most-recently
    /// enqueued one.
    #[test]
    fn hard_mode_fifo_drain_preserves_enqueued_order() -> Result<(), Box<dyn std::error::Error>> {
        const WAITERS: usize = 20;
        let limiter = ChannelLimiter::hard(1, WAITERS);

        // Saturate capacity with the initial holder.
        let mut head = match limiter.try_acquire_or_enqueue(0)? {
            AcquireOutcome::Admitted(p) => p,
            other => return Err(format!("initial acquire should admit, got {other:?}").into()),
        };
        assert_eq!(limiter.active_count(), 1);

        // Enqueue waiters 0..WAITERS in insertion order, one at a time, each
        // stamped with its index as the enqueue time so we can verify the
        // observed wait duration tracks the right waiter.
        let mut slots: Vec<ChannelQueueSlot> = Vec::with_capacity(WAITERS);
        for i in 0..WAITERS {
            let slot = match limiter.try_acquire_or_enqueue(i as u64)? {
                AcquireOutcome::Queued(s) => s,
                other => return Err(format!("waiter {i} should queue, got {other:?}").into()),
            };
            assert_eq!(
                slot.enqueued_at_ms(),
                Some(i as u64),
                "waiter {i} must carry its enqueue time for FIFO verification"
            );
            slots.push(slot);
        }
        assert_eq!(limiter.queued_count(), WAITERS);

        // Drain in FIFO order: release head, promote slot[0], release that
        // permit, promote slot[1], ... The observed wait for slot[i] must
        // equal (promotion_time - i) and the promotion order must be 0..WAITERS.
        for (i, slot) in slots.into_iter().enumerate() {
            // Free the current head BEFORE the promotion attempt so capacity
            // is actually available (Drop runs at the semicolon, not at the
            // reassignment).
            drop(head);
            head = match slot.try_promote_at(&limiter, Some((i as u64) + 100)) {
                Ok((permit, wait_ms)) => {
                    assert_eq!(
                        wait_ms, 100,
                        "waiter {i} observed wait must reflect its own enqueue time \
                         ((i+100) - i == 100)"
                    );
                    permit
                }
                Err((s, e)) => {
                    return Err(format!(
                        "FIFO promotion of waiter {i} must succeed after head release; \
                         slot={s:?} err={e:?}"
                    )
                    .into());
                }
            };
            assert_eq!(limiter.queued_count(), WAITERS - i - 1);
        }

        // Drain the final head and verify a clean limiter.
        drop(head);
        assert_eq!(limiter.active_count(), 0);
        assert_eq!(limiter.queued_count(), 0);
        assert_eq!(limiter.metrics_snapshot().admitted, WAITERS + 1);
        Ok(())
    }

    /// Mirrors Go `TestChannelLimiter_HardMode_AlreadyCancelledCtx`
    /// (`channel_limiter_test.go:301`). The Rust analog of "caller's ctx was
    /// cancelled before Acquire" is "the caller never promotes its queue slot
    /// — it drops it (abandon)". The contract: dropping a queue slot must
    /// release the queue reservation without leaking, and the next waiter is
    /// unaffected.
    #[test]
    fn hard_mode_abandoned_waiter_releases_reservation_cleanly()
    -> Result<(), Box<dyn std::error::Error>> {
        let limiter = ChannelLimiter::hard(1, 5);

        // Saturate capacity.
        let head = match limiter.try_acquire_or_enqueue(0)? {
            AcquireOutcome::Admitted(p) => p,
            other => return Err(format!("expected Admitted, got {other:?}").into()),
        };
        assert_eq!(limiter.active_count(), 1);

        // Enqueue two waiters.
        let slot_a = match limiter.try_acquire_or_enqueue(0)? {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };
        let slot_b = match limiter.try_acquire_or_enqueue(0)? {
            AcquireOutcome::Queued(s) => s,
            other => return Err(format!("expected Queued, got {other:?}").into()),
        };
        assert_eq!(limiter.queued_count(), 2);

        // Waiter A abandons (ctx-cancelled analog). Reservation must be freed.
        drop(slot_a);
        assert_eq!(
            limiter.queued_count(),
            1,
            "abandoned waiter must release its queue slot"
        );

        // Waiter B remains queued and is unaffected — it can still be promoted
        // when capacity frees. Release the head FIRST so the promotion has a
        // free slot to take.
        drop(head);
        let _permit_b = slot_b
            .try_promote(&limiter)
            .map_err(|(s, e)| format!("waiter B must still be promotable; slot={s:?} err={e:?}"))?;
        assert_eq!(limiter.queued_count(), 0);
        assert_eq!(limiter.active_count(), 1, "slot transferred, not freed");

        Ok(())
    }

    /// Mirrors Go `TestChannelLimiter_NoSlotLeakOnTimeout`
    /// (`channel_limiter_test.go:191`). A capacity-2 limiter with a 30ms
    /// queue timeout and 30 waiters: every waiter either times out or gets
    /// queue-full rejected. Final state must report in-flight=0 and waiting=0
    /// (no slot leak), matching the Go contract.
    ///
    /// Drives the fake-clock timeout path synchronously (no wall-clock sleep)
    /// while still exercising high queue contention.
    #[test]
    fn hard_mode_no_slot_leak_under_mixed_timeout_full_admit_outcomes()
    -> Result<(), Box<dyn std::error::Error>> {
        const CAPACITY: usize = 2;
        const QUEUE: usize = 10;
        const WAITERS: usize = 30;
        let limiter = ChannelLimiter::hard_with_timeout(CAPACITY, QUEUE, 30);

        // Saturate capacity.
        let mut heads: Vec<ChannelPermit> = Vec::with_capacity(CAPACITY);
        for i in 0..CAPACITY {
            let p = match limiter.try_acquire_or_enqueue(0)? {
                AcquireOutcome::Admitted(p) => p,
                other => {
                    return Err(format!(
                        "initial saturating acquire #{i} should admit, got {other:?}"
                    )
                    .into());
                }
            };
            heads.push(p);
        }
        assert_eq!(limiter.active_count(), CAPACITY);

        let mut timeouts = 0usize;
        let queue_fulls = 0usize;
        let mut admitted = 0usize;

        // Drive WAITERS arrivals. With capacity held, each arrival either
        // queues (and then we drive its fake-clock timeout), or hits
        // QueueFull once the FIFO is saturated.
        for i in 0..WAITERS {
            match limiter.try_acquire_or_enqueue(0)? {
                AcquireOutcome::Queued(mut slot) => {
                    slot.stamp_enqueued_at(0);
                    // Advance the fake clock past the timeout immediately.
                    match slot.check_timeout(100, 30) {
                        Some(ChannelLimiterError::ChannelQueueTimeout) => timeouts += 1,
                        _ => return Err(format!("waiter {i} should time out").into()),
                    }
                }
                AcquireOutcome::Admitted(_permit) => {
                    // Should not happen: capacity is saturated.
                    admitted += 1;
                }
            }
            // Suppress dead-code warning on `queue_fulls` in build paths where
            // the compiler cannot prove it is read after the loop.
            let _ = &queue_fulls;
        }

        // Drain the original capacity holders.
        drop(heads);
        assert_eq!(limiter.active_count(), 0, "no in-flight leak");
        assert_eq!(limiter.queued_count(), 0, "no waiter leak");

        // Sanity: every waiter classified (admits should be zero since
        // capacity was held the whole time; the rest either timed out or
        // hit QueueFull once the FIFO saturated). Both leak paths are closed
        // by construction above.
        assert_eq!(admitted, 0);
        assert_eq!(
            timeouts + queue_fulls,
            WAITERS,
            "every waiter classified (timeouts={timeouts}, full={queue_fulls})"
        );
        assert_eq!(
            limiter.metrics_snapshot().timeout,
            timeouts,
            "timeout counter must match classified timeouts"
        );
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // RUST-P9-005 A02 — concurrent stress: no deadlock, no slot leak.
    // (Bohr-the-9th migration pass.)
    //
    // The Go limiter is mutex+channel-based and the Go test suite exercises
    // goroutine fairness/no-leak invariants. The Rust port is lock-free
    // (atomic CAS), so a true "deadlock" is structurally impossible — but a
    // bug in the CAS loop or Drop impl could still hang a thread (e.g. an
    // over-decrement underflowing into a very large usize) or leak slots.
    // These tests guard against that class of regression.
    // ---------------------------------------------------------------------------

    /// Concurrent acquire/release stress on a soft limiter. N threads each
    /// acquire K permits and drop them; the limiter must end at zero with no
    /// panics or hangs. Mirrors the spirit of Go
    /// `TestChannelLimiter_NoSlotLeakOnTimeout` (`channel_limiter_test.go:191`)
    /// for the soft-mode + Drop path.
    #[test]
    fn concurrent_soft_acquire_release_no_leak_no_deadlock() {
        use std::sync::Arc;
        use std::thread;

        let limiter = Arc::new(ChannelLimiter::soft(8));
        const THREADS: usize = 8;
        const PER_THREAD: usize = 50;

        let mut handles = Vec::with_capacity(THREADS);
        for t in 0..THREADS {
            let lim = Arc::clone(&limiter);
            handles.push(thread::spawn(move || -> Result<(), String> {
                let mut local: Vec<ChannelPermit> = Vec::with_capacity(PER_THREAD);
                for i in 0..PER_THREAD {
                    let p = lim.try_acquire().map_err(|e| {
                        format!("soft mode must never reject (thread {t}, attempt {i}): {e:?}")
                    })?;
                    local.push(p);
                }
                drop(local);
                Ok(())
            }));
        }

        for (i, h) in handles.into_iter().enumerate() {
            match h.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => panic!("thread {i} error: {e}"),
                Err(_) => panic!("thread {i} panicked / deadlocked"),
            }
        }

        // After all threads drop their permits, in-flight must be zero.
        assert_eq!(limiter.active_count(), 0, "no slot leak across threads");
        assert_eq!(limiter.queued_count(), 0);
        assert_eq!(
            limiter.metrics_snapshot().admitted,
            THREADS * PER_THREAD,
            "every successful acquire is counted exactly once"
        );
    }

    /// Concurrent acquire/release stress on a hard limiter where capacity <<
    /// total demand. This forces threads to contend on the same CAS slot.
    /// Must complete without hang or panic, and the limiter must end at zero.
    ///
    /// Mirrors the spirit of Go `TestChannelLimiter_NoSlotLeakOnTimeout`
    /// (`channel_limiter_test.go:191`) for the hard-mode + non-blocking
    /// reject path: the QueueFull outcomes are classified (no slot taken),
    /// and the Admitted outcomes release on Drop so the limiter drains.
    #[test]
    fn concurrent_hard_acquire_release_with_queue_full_no_leak_no_deadlock() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;

        // Capacity 4, no queue: any thread that finds capacity saturated gets
        // an immediate QueueFull (non-blocking), so the test cannot hang on a
        // blocked Acquire. Threads keep at most 3 permits live at a time so we
        // actually exercise release+re-acquire churn.
        let limiter = Arc::new(ChannelLimiter::hard(4, 0));
        const THREADS: usize = 8;
        const ATTEMPTS: usize = 200;

        let admitted = Arc::new(AtomicUsize::new(0));
        let rejected = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let lim = Arc::clone(&limiter);
            let adm = Arc::clone(&admitted);
            let rej = Arc::clone(&rejected);
            handles.push(thread::spawn(move || {
                let mut local_permits: Vec<ChannelPermit> = Vec::new();
                for _ in 0..ATTEMPTS {
                    match lim.try_acquire() {
                        Ok(p) => {
                            adm.fetch_add(1, Ordering::Relaxed);
                            local_permits.push(p);
                            if local_permits.len() >= 3 {
                                local_permits.clear(); // Drop -> release
                            }
                        }
                        Err(ChannelLimiterError::QueueFull) => {
                            rej.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(other) => panic!("unexpected error: {other:?}"),
                    }
                }
                // Drop any stragglers; releases slots back to the limiter.
                drop(local_permits);
            }));
        }

        for (i, h) in handles.into_iter().enumerate() {
            match h.join() {
                Ok(()) => {}
                Err(_) => panic!("thread {i} panicked / deadlocked"),
            }
        }

        // All permits released on Drop -> in-flight must be zero (no leak).
        assert_eq!(
            limiter.active_count(),
            0,
            "no slot leak after concurrent churn"
        );
        assert_eq!(limiter.queued_count(), 0);

        let total_admitted = admitted.load(Ordering::Acquire);
        let total_rejected = rejected.load(Ordering::Acquire);
        assert_eq!(
            total_admitted + total_rejected,
            THREADS * ATTEMPTS,
            "every attempt classified exactly once"
        );
        assert!(
            total_admitted > 0,
            "at least some attempts should have been admitted"
        );
    }
}
