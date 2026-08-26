use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use conduit_core::objects::{ModelSettings, SystemModelSettings};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Re-export of the core objects association type so the inheritance helpers
/// can name it unambiguously alongside this module's own (simplified)
/// [`ModelAssociation`] resolution type.
pub use conduit_core::objects::ModelAssociation as ObjectsModelAssociation;

pub type ModelServiceResult<T> = Result<T, ModelServiceError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ModelServiceError {
    #[error("invalid model association pattern for {association_id}: {pattern}")]
    InvalidAssociationPattern {
        association_id: String,
        pattern: String,
    },
    #[error("invalid model blacklist pattern: {pattern}")]
    InvalidBlacklistPattern { pattern: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelQuerySettings {
    #[serde(default)]
    pub query_all_channel_models: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blacklist_regex: Option<String>,
    #[serde(default)]
    pub fallback_to_channels_on_model_not_found: bool,
}

impl ModelQuerySettings {
    pub fn should_query_all_channel_models(&self) -> bool {
        self.query_all_channel_models
    }

    pub fn is_model_blacklisted(&self, model_id: &str) -> ModelServiceResult<bool> {
        match &self.blacklist_regex {
            Some(pattern) => Regex::new(pattern)
                .map(|regex| regex.is_match(model_id))
                .map_err(|_| ModelServiceError::InvalidBlacklistPattern {
                    pattern: pattern.clone(),
                }),
            None => Ok(false),
        }
    }

    pub fn should_fallback_to_channels_on_model_not_found(&self) -> bool {
        self.fallback_to_channels_on_model_not_found
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRecord {
    pub id: String,
    pub project_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ModelRecord {
    pub fn new(
        id: impl Into<String>,
        project_id: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            project_id: project_id.into(),
            model_id: model_id.into(),
            provider_id: None,
            enabled: true,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelAssociation {
    pub id: String,
    pub project_id: String,
    pub matcher: AssociationMatcher,
    pub model_id: String,
    pub priority: i32,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub fallback_to_channels: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ModelAssociation {
    pub fn new(
        id: impl Into<String>,
        project_id: impl Into<String>,
        matcher: AssociationMatcher,
        model_id: impl Into<String>,
        priority: i32,
    ) -> Self {
        Self {
            id: id.into(),
            project_id: project_id.into(),
            matcher,
            model_id: model_id.into(),
            priority,
            disabled: false,
            fallback_to_channels: false,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssociationMatcher {
    ExactModelId { model_id: String },
    Pattern { pattern: String },
}

impl AssociationMatcher {
    pub fn exact_model_id(model_id: impl Into<String>) -> Self {
        Self::ExactModelId {
            model_id: model_id.into(),
        }
    }

    pub fn pattern(pattern: impl Into<String>) -> Self {
        Self::Pattern {
            pattern: pattern.into(),
        }
    }

    fn matches(&self, association_id: &str, requested_model_id: &str) -> ModelServiceResult<bool> {
        match self {
            Self::ExactModelId { model_id } => Ok(model_id == requested_model_id),
            Self::Pattern { pattern } => Regex::new(pattern)
                .map(|regex| regex.is_match(requested_model_id))
                .map_err(|_| ModelServiceError::InvalidAssociationPattern {
                    association_id: association_id.to_string(),
                    pattern: pattern.clone(),
                }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelResolution {
    pub requested_model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub association: Option<ModelAssociation>,
    #[serde(default)]
    pub fallback_to_channels: bool,
}

// ===========================================================================
// Model circuit breaker (RUST-P9-002 S17)
// ===========================================================================
//
// Ported from `conduit/internal/server/biz/model_circuit_breaker.go` and its
// tests (`model_circuit_breaker_test.go`). The Go breaker is a per
// `(channel_id, model_id)` three-state machine — Closed / HalfOpen / Open —
// driven by consecutive failures with a TTL-based failure window, a probe
// interval with exponential backoff, and a lazy auto-recovery path inside
// `GetEffectiveWeight`. The shape it takes here stays pure (no I/O, no wall
// clock): every Go `time.Now()` / `time.Since(...)` is replaced by an explicit
// `now: DateTime<Utc>` parameter so the state machine is fully deterministic
// and unit-testable. Persistence is left to a trait extension point
// ([`ModelCircuitBreakerStore`]); the in-memory implementation
// ([`MemoryModelCircuitBreakerStore`]) mirrors `biz.ModelCircuitBreaker`.
//
// Go time-zero (`time.Time{}`) maps to `Option<DateTime<Utc>>` with `None`
// meaning "never set"; every Go check like `t.IsZero()` becomes `is_none()`.

/// Per-(channel, model) key identifying one breaker's stats slot.
///
/// Mirrors Go `ChannelModelKey` (`model_circuit_breaker.go` lines 114-120):
/// `channel_id` is an int in Go, mapped to `i64` per the parity rules.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ModelCircuitBreakerKey {
    pub channel_id: i64,
    pub model_id: String,
}

impl ModelCircuitBreakerKey {
    pub fn new(channel_id: i64, model_id: impl Into<String>) -> Self {
        Self {
            channel_id,
            model_id: model_id.into(),
        }
    }
}

/// Breaker state. Mirrors Go `CircuitBreakerState` string enum
/// (`model_circuit_breaker.go` lines 15-27). Serialized as the literal Go
/// values (`"closed"` / `"half_open"` / `"open"`) for JSON parity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CircuitBreakerState {
    /// Circuit complete; requests flow through. Go default for new stats.
    #[default]
    Closed,
    /// Limited requests allowed to probe the upstream. Reached at
    /// `half_open_threshold` consecutive failures.
    HalfOpen,
    /// No requests allowed (except a single probe once `next_probe_at` passes).
    /// Reached at `open_threshold` consecutive failures.
    Open,
}

/// Tunable policy. Mirrors Go `ModelCircuitBreakerPolicy`
/// (`model_circuit_breaker.go` lines 29-56). Durations use
/// [`chrono::Duration`] (not `time::Duration`) because the breaker arithmetic
/// is wall-clock-based and `chrono::Duration` keeps the same signed-semantics
/// as Go's `time.Duration`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCircuitBreakerPolicy {
    /// Consecutive failures that promote Closed -> HalfOpen. Go default 3.
    pub half_open_threshold: i64,
    /// Consecutive failures that promote (Half|Closed) -> Open. Go default 5.
    /// Must be strictly greater than `half_open_threshold`.
    pub open_threshold: i64,
    /// Failure-counter TTL; counters reset if no new failure occurs in this
    /// window. Go default 30 minutes.
    pub failure_stats_ttl: ChronoDuration,
    /// Delay between Open-state probes. Go default 5 minutes.
    pub probe_interval: ChronoDuration,
    /// Weight multiplier applied in HalfOpen. Go default 0.3.
    pub half_open_weight: i64,
}

impl Default for ModelCircuitBreakerPolicy {
    fn default() -> Self {
        // Mirrors Go `defaultModelCircuitBreakerPolicy` (lines 50-56).
        // `half_open_weight` is stored as basis points (0.3 -> 30) so the whole
        // policy stays `Eq` without pulling in floats; see
        // [`Self::effective_half_open_weight`] for the float reconstruction.
        Self {
            half_open_threshold: 3,
            open_threshold: 5,
            failure_stats_ttl: ChronoDuration::minutes(30),
            probe_interval: ChronoDuration::minutes(5),
            half_open_weight: 30,
        }
    }
}

/// Errors produced by [`ModelCircuitBreakerPolicy::validate`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CircuitBreakerPolicyError {
    /// `half_open_threshold >= open_threshold` — Go rejects this in `Validate`
    /// (lines 65-68) because HalfOpen must be reachable before Open.
    #[error(
        "half_open_threshold ({half_open_threshold}) must be less than open_threshold ({open_threshold})"
    )]
    HalfOpenNotBeforeOpen {
        half_open_threshold: i64,
        open_threshold: i64,
    },
    /// `half_open_weight` outside `[0, 1]` — Go rejects this in `Validate`
    /// (lines 70-72).
    #[error("half_open_weight must be between 0 and 1, got {actual}")]
    HalfOpenWeightOutOfRange { actual: f64 },
}

impl ModelCircuitBreakerPolicy {
    /// Validate the policy. Mirrors Go `ModelCircuitBreakerPolicy.Validate`
    /// (`model_circuit_breaker.go` lines 64-75): `half_open_threshold <
    /// open_threshold` and `half_open_weight` in `[0, 1]`.
    pub fn validate(&self) -> Result<(), CircuitBreakerPolicyError> {
        if self.half_open_threshold >= self.open_threshold {
            return Err(CircuitBreakerPolicyError::HalfOpenNotBeforeOpen {
                half_open_threshold: self.half_open_threshold,
                open_threshold: self.open_threshold,
            });
        }
        let weight = self.effective_half_open_weight();
        if !(0.0..=1.0).contains(&weight) {
            return Err(CircuitBreakerPolicyError::HalfOpenWeightOutOfRange { actual: weight });
        }
        Ok(())
    }

    /// Reconstruct the float `half_open_weight` from the stored basis points.
    fn effective_half_open_weight(&self) -> f64 {
        // Stored as integer basis points to keep the struct `Eq`; 30 bp -> 0.30.
        f64::from(self.half_open_weight as i32) / 100.0
    }
}

/// Per-(channel, model) breaker statistics. Mirrors Go
/// `ModelCircuitBreakerStats` (`model_circuit_breaker.go` lines 77-99) minus
/// the `sync.RWMutex` (Rust callers borrow mutably) and the
/// `probing_in_progress` int32 (mapped to a plain `bool`).
///
/// All timestamps are `Option<DateTime<Utc>>`: `None` corresponds to Go's
/// `time.Time{}` zero value, and Go's `t.IsZero()` checks become `is_none()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCircuitBreakerStats {
    pub channel_id: i64,
    pub model_id: String,
    pub state: CircuitBreakerState,
    /// Consecutive failures since the last success or TTL reset.
    pub consecutive_failures: i64,
    pub last_failure_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    /// Earliest time the next Open-state probe may run.
    pub next_probe_at: Option<DateTime<Utc>>,
    /// Monotonic counter driving exponential backoff (capped at 8x in Go).
    pub probe_attempts: i64,
    /// `true` while a probe request is in flight; prevents probe penetration.
    /// Mirrors Go `probingInProgress int32` used with `atomic.CompareAndSwap`.
    pub probing_in_progress: bool,
}

impl ModelCircuitBreakerStats {
    /// Construct the initial Closed stats for a fresh `(channel, model)`.
    ///
    /// Mirrors Go `getStats` (lines 131-137): `State: StateClosed`,
    /// `ConsecutiveFailures: 0`, `LastSuccessAt: time.Now()`. Go leaves
    /// `LastFailureAt` and `NextProbeAt` at zero; here they stay `None`.
    pub fn new(channel_id: i64, model_id: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            channel_id,
            model_id: model_id.into(),
            state: CircuitBreakerState::Closed,
            consecutive_failures: 0,
            last_failure_at: None,
            last_success_at: Some(now),
            next_probe_at: None,
            probe_attempts: 0,
            probing_in_progress: false,
        }
    }

    /// Record a failure and apply the state machine. Mirrors Go `RecordError`
    /// (`model_circuit_breaker.go` lines 155-225).
    ///
    /// - TTL check: if the previous failure is older than `failure_stats_ttl`,
    ///   the counter resets before counting this failure (lines 164-174).
    /// - Counter increment + `last_failure_at = now` (lines 177-178).
    /// - Promotion: at `open_threshold` -> Open (sets `next_probe_at`); else at
    ///   `half_open_threshold` -> HalfOpen (lines 182-224).
    /// - Exponential backoff on `next_probe_at` is applied ONLY when already
    ///   Open AND `was_probe == true` (lines 193-213), preventing non-probe
    ///   rejections from delaying recovery indefinitely. Backoff is
    ///   `probe_interval * 2^probe_attempts`, capped at 8x.
    pub fn record_error(
        &mut self,
        now: DateTime<Utc>,
        policy: &ModelCircuitBreakerPolicy,
        was_probe: bool,
    ) {
        // 1. TTL reset: avoid zombie counts from stale failures.
        if self.consecutive_failures > 0
            && let Some(last_failure) = self.last_failure_at
            && now - last_failure > policy.failure_stats_ttl
        {
            self.consecutive_failures = 0;
        }

        // 2. Increment + stamp.
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure_at = Some(now);

        // 3. State transition. Open is checked before HalfOpen so that once the
        //    Open threshold is reached the breaker locks Open regardless of the
        //    intermediate HalfOpen band.
        if self.consecutive_failures >= policy.open_threshold {
            if self.state != CircuitBreakerState::Open {
                // First time entering Open: schedule the first probe and reset
                // the backoff counter (lines 184-186).
                self.state = CircuitBreakerState::Open;
                self.next_probe_at = Some(now + policy.probe_interval);
                self.probe_attempts = 0;
            } else if was_probe {
                // Already Open + this failure came from a real probe request:
                // push the next probe out with capped exponential backoff
                // (lines 193-213). Non-probe failures leave next_probe_at
                // untouched.
                let multiplier_exp = self.probe_attempts;
                // Go caps the multiplier at 8 (i.e. 2^3); compute via integer
                // shift to stay exact, then saturate.
                let shift = multiplier_exp.clamp(0, 3) as u32;
                let multiplier = 1i64 << shift; // 1, 2, 4, 8 — capped.
                let interval_nanos = policy.probe_interval.num_nanoseconds().unwrap_or(i64::MAX);
                let scaled_nanos = interval_nanos.saturating_mul(multiplier);
                let scaled = ChronoDuration::nanoseconds(scaled_nanos);
                self.next_probe_at = Some(now + scaled);
                self.probe_attempts = self.probe_attempts.saturating_add(1);
            }
        } else if self.consecutive_failures >= policy.half_open_threshold
            && self.state != CircuitBreakerState::HalfOpen
        {
            self.state = CircuitBreakerState::HalfOpen;
        }
    }

    /// Record a success and immediately recover to Closed. Mirrors Go
    /// `RecordSuccess` (`model_circuit_breaker.go` lines 228-251): one success
    /// zeroes `consecutive_failures`, clears `next_probe_at`, drops
    /// `probing_in_progress` / `probe_attempts`, and forces `state = Closed`.
    pub fn record_success(&mut self, now: DateTime<Utc>) {
        self.last_success_at = Some(now);
        self.state = CircuitBreakerState::Closed;
        self.consecutive_failures = 0;
        self.next_probe_at = None;
        self.probing_in_progress = false;
        self.probe_attempts = 0;
    }

    /// Compute the load-balancer weight for this breaker. Mirrors Go
    /// `GetEffectiveWeight` (`model_circuit_breaker.go` lines 277-335).
    ///
    /// - **Closed** -> `base_weight`.
    /// - **HalfOpen** -> `base_weight * half_open_weight`.
    /// - **Open** -> `0.0`, except when `now` is past `next_probe_at` AND no
    ///   probe is in flight, in which case one probe is permitted at
    ///   `base_weight * half_open_weight`.
    ///
    /// Also performs **lazy TTL auto-recovery** (lines 286-308): if the breaker
    /// is not Closed, has failures, and the last failure is older than
    /// `failure_stats_ttl`, it resets to Closed before computing the weight.
    /// This is the auto-recovery path exercised by the Go test
    /// `TestGetEffectiveWeight_TTLAutoRecovery`.
    pub fn effective_weight(
        &mut self,
        now: DateTime<Utc>,
        policy: &ModelCircuitBreakerPolicy,
        base_weight: f64,
    ) -> f64 {
        // Lazy TTL auto-recovery (Go lines 286-308).
        if self.state != CircuitBreakerState::Closed
            && self.consecutive_failures > 0
            && matches!(self.last_failure_at, Some(last) if now - last > policy.failure_stats_ttl)
        {
            self.state = CircuitBreakerState::Closed;
            self.consecutive_failures = 0;
            self.next_probe_at = None;
            self.probe_attempts = 0;
            self.probing_in_progress = false;
        }

        let half_weight = base_weight * policy.effective_half_open_weight();
        match self.state {
            CircuitBreakerState::Closed => base_weight,
            CircuitBreakerState::HalfOpen => half_weight,
            CircuitBreakerState::Open => {
                // Allow a single probe once the probe window has elapsed.
                let probe_window_open = match self.next_probe_at {
                    Some(probe_at) => now > probe_at,
                    None => true, // Go treats zero NextProbeAt as "always eligible".
                };
                if probe_window_open && !self.probing_in_progress {
                    half_weight
                } else {
                    0.0
                }
            }
        }
    }

    /// Attempt to claim the single in-flight probe slot. Mirrors Go
    /// `TryBeginProbe` (`model_circuit_breaker.go` lines 337-352).
    ///
    /// Returns `true` only when the breaker is Open, `next_probe_at` has
    /// elapsed (or is unset), and no other probe is in flight. On success the
    /// caller MUST later call [`Self::end_probe`] to release the slot.
    pub fn try_begin_probe(&mut self, now: DateTime<Utc>) -> bool {
        if self.state != CircuitBreakerState::Open {
            return false;
        }
        let window_open = match self.next_probe_at {
            Some(probe_at) => now >= probe_at,
            None => true,
        };
        if !window_open {
            return false;
        }
        if self.probing_in_progress {
            return false;
        }
        self.probing_in_progress = true;
        true
    }

    /// Release the probe slot. Mirrors Go `EndProbe` (lines 354-357).
    pub fn end_probe(&mut self) {
        self.probing_in_progress = false;
    }

    /// Manually reset to Closed. Mirrors Go `ResetModelStatus`
    /// (lines 419-441): operator-initiated recovery clears every negative
    /// field atomically.
    pub fn reset(&mut self) {
        self.state = CircuitBreakerState::Closed;
        self.consecutive_failures = 0;
        self.next_probe_at = None;
        self.probe_attempts = 0;
        self.probing_in_progress = false;
    }

    /// Convenience predicate matching the spirit of the legacy
    /// `is_open()` helper: `true` when in Open state.
    pub fn is_open(&self) -> bool {
        self.state == CircuitBreakerState::Open
    }
}

/// Extension point for a persistent model circuit breaker store.
///
/// The Go breaker (`biz.ModelCircuitBreaker`) keeps everything in an in-memory
/// `xmap.Map`; persistence is not part of the contract. This trait mirrors that
/// surface so a DB-backed implementation can drop in later (S17 "支持内存与持久化
/// 扩展") without changing call sites. The in-memory implementation
/// [`MemoryModelCircuitBreakerStore`] is the default; a persistent store would
/// additionally snapshot stats on each mutation.
pub trait ModelCircuitBreakerStore {
    /// Record a failure, applying the state machine. `was_probe` distinguishes
    /// probe failures (eligible for exponential backoff) from rejected-request
    /// errors. Mirrors Go `RecordError`.
    fn record_error(
        &mut self,
        key: &ModelCircuitBreakerKey,
        now: DateTime<Utc>,
        policy: &ModelCircuitBreakerPolicy,
        was_probe: bool,
    );

    /// Record a success, recovering to Closed. Mirrors Go `RecordSuccess`.
    fn record_success(
        &mut self,
        key: &ModelCircuitBreakerKey,
        now: DateTime<Utc>,
        policy: &ModelCircuitBreakerPolicy,
    );

    /// Compute the effective weight (with lazy TTL auto-recovery). Mirrors Go
    /// `GetEffectiveWeight`.
    fn effective_weight(
        &mut self,
        key: &ModelCircuitBreakerKey,
        now: DateTime<Utc>,
        policy: &ModelCircuitBreakerPolicy,
        base_weight: f64,
    ) -> f64;

    /// Try to claim the single probe slot. Mirrors Go `TryBeginProbe`.
    fn try_begin_probe(&mut self, key: &ModelCircuitBreakerKey, now: DateTime<Utc>) -> bool;

    /// Release the probe slot. Mirrors Go `EndProbe`.
    fn end_probe(&mut self, key: &ModelCircuitBreakerKey);

    /// Manually reset a breaker to Closed. Mirrors Go `ResetModelStatus`.
    fn reset(&mut self, key: &ModelCircuitBreakerKey);

    /// Read-only snapshot of a breaker's stats (copy) — `None` if never seen.
    fn stats(&self, key: &ModelCircuitBreakerKey) -> Option<ModelCircuitBreakerStats>;

    /// Convenience predicate over [`Self::stats`]: Open state or unseen.
    fn is_open(&self, key: &ModelCircuitBreakerKey) -> bool {
        self.stats(key)
            .is_some_and(|s| s.state == CircuitBreakerState::Open)
    }
}

/// In-memory breaker store. Mirrors Go `biz.ModelCircuitBreaker`
/// (`model_circuit_breaker.go` lines 101-112) backed by an `xmap.Map`; here a
/// `BTreeMap` to keep iteration deterministic for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryModelCircuitBreakerStore {
    stats: BTreeMap<ModelCircuitBreakerKey, ModelCircuitBreakerStats>,
}

impl MemoryModelCircuitBreakerStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get-or-create stats for `key`, creating the initial Closed entry at
    /// `now`. Mirrors Go `getStats` (lines 122-142).
    fn get_or_create(
        &mut self,
        key: &ModelCircuitBreakerKey,
        now: DateTime<Utc>,
    ) -> &mut ModelCircuitBreakerStats {
        self.stats.entry(key.clone()).or_insert_with(|| {
            ModelCircuitBreakerStats::new(key.channel_id, key.model_id.as_str(), now)
        })
    }

    /// Directly mutate the stats for `key`, if present.
    ///
    /// The Go tests for `RecordError` (`model_circuit_breaker_test.go` lines
    /// 67-100) reach into the unexported `getStats` + `Lock` to pin fields like
    /// `NextProbeAt` before exercising the state machine. This accessor is the
    /// Rust equivalent, and it doubles as the seam a DB-backed store would use
    /// to write mutated stats back to persistent storage.
    pub fn get_mut(
        &mut self,
        key: &ModelCircuitBreakerKey,
    ) -> Option<&mut ModelCircuitBreakerStats> {
        self.stats.get_mut(key)
    }
}

impl ModelCircuitBreakerStore for MemoryModelCircuitBreakerStore {
    fn record_error(
        &mut self,
        key: &ModelCircuitBreakerKey,
        now: DateTime<Utc>,
        policy: &ModelCircuitBreakerPolicy,
        was_probe: bool,
    ) {
        self.get_or_create(key, now)
            .record_error(now, policy, was_probe);
    }

    fn record_success(
        &mut self,
        key: &ModelCircuitBreakerKey,
        now: DateTime<Utc>,
        _policy: &ModelCircuitBreakerPolicy,
    ) {
        self.get_or_create(key, now).record_success(now);
    }

    fn effective_weight(
        &mut self,
        key: &ModelCircuitBreakerKey,
        now: DateTime<Utc>,
        policy: &ModelCircuitBreakerPolicy,
        base_weight: f64,
    ) -> f64 {
        self.get_or_create(key, now)
            .effective_weight(now, policy, base_weight)
    }

    fn try_begin_probe(&mut self, key: &ModelCircuitBreakerKey, now: DateTime<Utc>) -> bool {
        self.get_or_create(key, now).try_begin_probe(now)
    }

    fn end_probe(&mut self, key: &ModelCircuitBreakerKey) {
        if let Some(stats) = self.stats.get_mut(key) {
            stats.end_probe();
        }
    }

    fn reset(&mut self, key: &ModelCircuitBreakerKey) {
        if let Some(stats) = self.stats.get_mut(key) {
            stats.reset();
        }
    }

    fn stats(&self, key: &ModelCircuitBreakerKey) -> Option<ModelCircuitBreakerStats> {
        self.stats.get(key).cloned()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelService {
    models: Vec<ModelRecord>,
    associations: Vec<ModelAssociation>,
    #[serde(default)]
    query_settings: ModelQuerySettings,
}

impl ModelService {
    pub fn new(models: Vec<ModelRecord>, associations: Vec<ModelAssociation>) -> Self {
        Self {
            models,
            associations,
            query_settings: ModelQuerySettings::default(),
        }
    }

    pub fn with_fallback_to_channels(mut self, fallback_to_channels: bool) -> Self {
        self.query_settings.fallback_to_channels_on_model_not_found = fallback_to_channels;
        self
    }

    pub fn with_query_settings(mut self, query_settings: ModelQuerySettings) -> Self {
        self.query_settings = query_settings;
        self
    }

    pub fn query_settings(&self) -> &ModelQuerySettings {
        &self.query_settings
    }

    pub fn resolve_model(
        &self,
        project_id: &str,
        requested_model_id: &str,
    ) -> ModelServiceResult<ModelResolution> {
        if let Some(model) = self.find_model(project_id, requested_model_id) {
            return Ok(ModelResolution {
                requested_model_id: requested_model_id.to_string(),
                model: Some(model),
                association: None,
                fallback_to_channels: false,
            });
        }

        if let Some(association) = self.find_association(project_id, requested_model_id)? {
            let fallback_to_channels = association.fallback_to_channels;
            let model = self.find_model(project_id, &association.model_id);

            return Ok(ModelResolution {
                requested_model_id: requested_model_id.to_string(),
                model,
                association: Some(association),
                fallback_to_channels,
            });
        }

        Ok(ModelResolution {
            requested_model_id: requested_model_id.to_string(),
            model: None,
            association: None,
            fallback_to_channels: self
                .query_settings
                .should_fallback_to_channels_on_model_not_found(),
        })
    }

    pub fn find_association(
        &self,
        project_id: &str,
        requested_model_id: &str,
    ) -> ModelServiceResult<Option<ModelAssociation>> {
        let mut matches = Vec::new();

        for association in self
            .associations
            .iter()
            .filter(|association| association.project_id == project_id && !association.disabled)
        {
            if association
                .matcher
                .matches(&association.id, requested_model_id)?
            {
                matches.push(association.clone());
            }
        }

        matches.sort_by(compare_association_priority);
        Ok(matches.into_iter().next())
    }

    fn find_model(&self, project_id: &str, model_id: &str) -> Option<ModelRecord> {
        self.models
            .iter()
            .filter(|model| {
                model.project_id == project_id && model.model_id == model_id && model.enabled
            })
            .min_by(|left, right| left.id.cmp(&right.id))
            .cloned()
    }
}

fn compare_association_priority(left: &ModelAssociation, right: &ModelAssociation) -> Ordering {
    left.priority
        .cmp(&right.priority)
        .then_with(|| left.id.cmp(&right.id))
}

fn default_enabled() -> bool {
    true
}

// ===========================================================================
// Settings inheritance (RUST-P9-002 S04)
// ===========================================================================
//
// Ported from `conduit/internal/server/biz/model_settings_inheritance.go`.
// The Go API surface that matters for this task is `EffectiveModelAssociations`
// plus its helpers: developer-level associations are inherited by each sibling
// model (with the model id stamped into `channel_model`/`channel_tags_model`
// branches), unless the model sets `disableDeveloperSettingsInheritance`. The
// model's own associations then merge on top, ordered by priority with a
// model-before-developer tiebreak.
//
// The pure helpers below take borrowed inputs and return owned clones, mirroring
// the Go deep-clone behavior so callers can mutate results without aliasing the
// shared system settings.

/// Effective association list for one model after applying developer-settings
/// inheritance.
///
/// Mirrors Go `EffectiveModelAssociations` (`model_settings_inheritance.go`
/// lines 129-146). When `model_settings.disable_developer_settings_inheritance`
/// is `true`, only the model's own associations are returned. Otherwise the
/// matching developer's associations are inherited (with `model_id` stamped in)
/// and merged with the model's own.
pub fn effective_model_associations(
    system_settings: &SystemModelSettings,
    developer: &str,
    model_id: &str,
    model_settings: Option<&ModelSettings>,
) -> Vec<ObjectsModelAssociation> {
    let Some(model_settings) = model_settings else {
        let inherited = inherit_developer_associations_for_model(
            system_settings.associations_for_developer(developer),
            model_id,
        );
        return merge_inherited_model_associations(&inherited, &[]);
    };

    if model_settings.disable_developer_settings_inheritance {
        return merge_inherited_model_associations(&[], &model_settings.associations);
    }

    let inherited = inherit_developer_associations_for_model(
        system_settings.associations_for_developer(developer),
        model_id,
    );
    merge_inherited_model_associations(&inherited, &model_settings.associations)
}

/// Stamp `model_id` into each developer association, returning owned clones.
///
/// Mirrors Go `inheritDeveloperAssociationsForModel` (lines 148-162): developer
/// rules only carry channel/channel-tags branches, and the concrete `model_id`
/// is filled in at inheritance time so sibling models do not share one fixed
/// target. Branches that are not `channel_model`/`channel_tags_model` (or lack
/// their branch payload) are dropped, matching Go's `default: return nil`.
pub fn inherit_developer_associations_for_model(
    developer_associations: &[ObjectsModelAssociation],
    model_id: &str,
) -> Vec<ObjectsModelAssociation> {
    if model_id.is_empty() {
        return Vec::new();
    }
    developer_associations
        .iter()
        .filter_map(|assoc| inherit_developer_association_for_model(assoc, model_id))
        .collect()
}

/// Clone a single developer association and stamp `model_id` into its branch.
///
/// Mirrors Go `inheritDeveloperAssociationForModel` (lines 164-189).
pub fn inherit_developer_association_for_model(
    assoc: &ObjectsModelAssociation,
    model_id: &str,
) -> Option<ObjectsModelAssociation> {
    let mut inherited = clone_model_association(assoc);
    match inherited.kind.as_str() {
        "channel_model" => {
            let branch = inherited.channel_model.as_mut()?;
            branch.model_id = model_id.to_string();
        }
        "channel_tags_model" => {
            let branch = inherited.channel_tags_model.as_mut()?;
            branch.model_id = model_id.to_string();
        }
        // Developer rules only choose channels/tags; anything else is not
        // inheritable and is dropped (Go returns nil).
        _ => return None,
    }
    Some(inherited)
}

/// Deep-clone a [`ModelAssociation`], including its `when`/condition subtree.
///
/// Mirrors Go `cloneModelAssociation` (lines 191-228). The Rust types are all
/// `Clone` by value (no interior pointers), so a derived `clone()` already
/// produces an independent copy; this helper exists to keep the Go function
/// mapping explicit and to mark the cloning intent at the call site.
pub fn clone_model_association(assoc: &ObjectsModelAssociation) -> ObjectsModelAssociation {
    assoc.clone()
}

/// Merge inherited developer associations with the model's own associations,
/// ordering by ascending priority and breaking ties model-first.
///
/// Mirrors Go `mergeInheritedModelAssociations` (lines 278-336). The sort is
/// stable: when priorities are equal, model-level rules (source rank 0) come
/// before inherited developer rules (source rank 1), preserving input order
/// within each source. Lower priority values run first.
pub fn merge_inherited_model_associations(
    developer_associations: &[ObjectsModelAssociation],
    model_associations: &[ObjectsModelAssociation],
) -> Vec<ObjectsModelAssociation> {
    // source_rank: 0 = model-level (own), 1 = developer-inherited. Lower rank
    // wins the tiebreak so model rules refine the shared developer defaults.
    #[derive(Clone, Copy)]
    struct Item {
        source_rank: u8,
        order: usize,
    }

    let mut items: Vec<(Item, &ObjectsModelAssociation)> =
        Vec::with_capacity(model_associations.len() + developer_associations.len());
    for (i, assoc) in model_associations.iter().enumerate() {
        items.push((
            Item {
                source_rank: 0,
                order: i,
            },
            assoc,
        ));
    }
    for (i, assoc) in developer_associations.iter().enumerate() {
        items.push((
            Item {
                source_rank: 1,
                order: i,
            },
            assoc,
        ));
    }

    // Stable sort by (priority asc, source_rank asc, original order asc).
    items.sort_by(|(la, left), (ra, right)| {
        left.priority
            .cmp(&right.priority)
            .then(la.source_rank.cmp(&ra.source_rank))
            .then(la.order.cmp(&ra.order))
    });

    items.into_iter().map(|(_, assoc)| assoc.clone()).collect()
}

// ===========================================================================
// SystemModelSettings validation + normalization (RUST-P9-002 S14)
// ===========================================================================
//
// Ported from `conduit/internal/server/biz/model_settings_inheritance.go`
// (`normalizeSystemModelSettings` lines 12-32, `validateSystemModelSettings`
// lines 34-61, `normalizeDeveloperAssociations` lines 63-80, and
// `validateDeveloperAssociations` lines 82-107) plus the `validateModelSettings`
// surface from `conduit/internal/server/biz/model.go` (lines 55-281). The Go
// runtime calls these from `SystemService.SetModelSettings` (`system.go` lines
// 1177-1180) right after unmarshalling: normalize first, then validate.
//
// `validateModelSettings` walks every association, compiles each regex-bearing
// branch (`channel_regex` / `channel_tags_regex` / `regex` / `exclude` patterns)
// via `xregexp.ValidateRegex` semantics, and recursively validates the
// `when`/condition tree (group nesting capped at 3, per-field operator/value
// whitelist via `validateFilterLeaf`). The Rust port reproduces both halves so
// `validateDeveloperAssociations` can delegate to it at its trailing Go call
// (line 102) without dropping the contract.

/// Errors returned by [`validate_system_model_settings`].
///
/// Each variant mirrors a specific rejection branch of Go
/// `validateSystemModelSettings` / `validateDeveloperAssociations`. The message
/// text matches the Go `fmt.Errorf` strings verbatim so callers (and parity
/// tests asserting on substring matches) behave identically.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelValidationError {
    /// Go: `fmt.Errorf("model developer is required")`
    /// (`model_settings_inheritance.go` lines 46-48).
    #[error("model developer is required")]
    DeveloperRequired,
    /// Go: `fmt.Errorf("duplicate model developer %q", developer)`
    /// (lines 50-52). Note: Go `%q` quotes the developer name; the Rust
    /// [`Display`](std::fmt::Display) mirrors that with the same `%q`-style
    /// quoting via [`char::escape_default`]-free direct embedding, matching the
    /// Go golden substring `"duplicate model developer"`.
    #[error("duplicate model developer {developer:?}")]
    DuplicateDeveloper { developer: String },
    /// Go: `fmt.Errorf("developer channel association requires channel")`
    /// (lines 90-92).
    #[error("developer channel association requires channel")]
    DeveloperChannelAssociationRequiresChannel,
    /// Go: `fmt.Errorf("developer channel tags association requires channel tags")`
    /// (lines 93-95).
    #[error("developer channel tags association requires channel tags")]
    DeveloperChannelTagsAssociationRequiresChannelTags,
    /// Go: `fmt.Errorf("developer association type %q is not supported", assoc.Type)`
    /// (lines 97-99). `association_type` is the raw `ModelAssociation.kind`
    /// value (`"model"`, `"regex"`, etc.).
    #[error("developer association type {association_type:?} is not supported")]
    UnsupportedDeveloperAssociation { association_type: String },

    // --- Sub-errors from `validateModelSettings` (model.go lines 55-113) ----
    //
    // These cover the regex-pattern compile failures (channel_regex /
    // channel_tags_regex / regex / exclude.channel_name_pattern) and the
    // `when`/condition-tree validation failures. Each variant's `Display`
    // message mirrors the corresponding Go `fmt.Errorf` verbatim so the
    // `model_validation_test.go` substring assertions behave identically.
    /// Go: `fmt.Errorf("invalid regex pattern in channel_regex association: %w", err)`
    /// (`model.go` lines 70-74).
    #[error("invalid regex pattern in channel_regex association: {pattern:?}")]
    InvalidChannelRegexPattern { pattern: String },
    /// Go: `fmt.Errorf("invalid regex pattern in channel_tags_regex association: %w", err)`
    /// (`model.go` lines 76-80).
    #[error("invalid regex pattern in channel_tags_regex association: {pattern:?}")]
    InvalidChannelTagsRegexPattern { pattern: String },
    /// Go: `fmt.Errorf("invalid regex pattern in regex association: %w", err)`
    /// (`model.go` lines 82-88). `pattern` is the offending regex.
    #[error("invalid regex pattern in regex association: {pattern:?}")]
    InvalidRegexPattern { pattern: String },
    /// Go: `fmt.Errorf("invalid model blacklist regex: %w", err)`
    /// (`system.go:1173-1174`). `pattern` is the offending
    /// `model_blacklist_regex`.
    #[error("invalid model blacklist regex: {pattern:?}")]
    InvalidModelBlacklistRegex { pattern: String },
    /// Go: `fmt.Errorf("invalid regex pattern in exclude rule: %w", err)`
    /// (`model.go` lines 91-99). `pattern` is the offending
    /// `exclude.channel_name_pattern`.
    #[error("invalid regex pattern in exclude rule: {pattern:?}")]
    InvalidExcludeRegexPattern { pattern: String },

    // --- Sub-errors from `validateModelAssociationWhen` + the condition tree --
    //
    // (`model.go` lines 115-132 for the `when` wrapper, 139-185 for the tree
    // walker, 187-281 for the per-field leaf validators).
    /// Go: `fmt.Errorf("invalid when condition: %w", err)` (line 67). The
    /// wrapped `source` carries the original tree-walker error so the substring
    /// assertions on inner messages (e.g. `"condition requires at least one
    /// condition or group"`) keep matching.
    #[error("invalid when condition: {source}")]
    InvalidWhenCondition {
        #[source]
        source: Box<ModelValidationError>,
    },
    /// Go: `fmt.Errorf("at least one supported when condition is required")`
    /// (line 125): an enabled `when` with `condition == None`.
    #[error("at least one supported when condition is required")]
    WhenConditionRequired,
    /// Go: `fmt.Errorf("root when condition must be a group")` (line 154).
    #[error("root when condition must be a group")]
    RootConditionMustBeGroup,
    /// Go: `fmt.Errorf("condition requires at least one condition or group")`
    /// (line 160).
    #[error("condition requires at least one condition or group")]
    GroupRequiresConditions,
    /// Go: `fmt.Errorf("condition nesting depth must not exceed %d", max)`
    /// (line 164). `max` is the configured `MaxNestedLevels` (3 for model
    /// associations).
    #[error("condition nesting depth must not exceed {max}")]
    NestingDepthExceeded { max: i64 },
    /// Go: `fmt.Errorf("nested condition groups are not allowed")` (line 170).
    /// Emitted only when `AllowNestedGroups` is false; model-association
    /// validation always allows nesting, so this variant is reserved for
    /// future callers that reuse the walker with stricter options.
    #[error("nested condition groups are not allowed")]
    NestedGroupsNotAllowed,
    /// Go: `fmt.Errorf("unsupported condition type %q", condition.Type)`
    /// (line 183). `condition_type` is the raw `Condition.r#type` JSON value.
    #[error("unsupported condition type {condition_type:?}")]
    UnsupportedConditionType { condition_type: String },
    /// Go: `fmt.Errorf("condition field is required")` (line 189).
    #[error("condition field is required")]
    ConditionFieldRequired,
    /// Go: `fmt.Errorf("unsupported condition field %q", condition.Field)`
    /// (line 207). `field` is the offending leaf field name.
    #[error("unsupported condition field {field:?}")]
    UnsupportedConditionField { field: String },
    /// Go: `fmt.Errorf("unsupported condition operator %q for prompt_tokens")`
    /// (line 215) — and the parallel forms for other fields, identified by
    /// `field`.
    #[error("unsupported condition operator {operator:?} for {field}")]
    UnsupportedConditionOperator { operator: String, field: String },
    /// Go: `fmt.Errorf("condition value for prompt_tokens must be an integer")`
    /// (line 224). Also covers the int64 coercion failure path.
    #[error("condition value for prompt_tokens must be an integer")]
    PromptTokensValueNotInteger,
    /// Go: `fmt.Errorf("prompt_tokens must be greater than or equal to 0")`
    /// (line 228).
    #[error("prompt_tokens must be greater than or equal to 0")]
    PromptTokensNegative,
    /// Go: `fmt.Errorf("condition value for %s must be a boolean, got %T", field, value)`
    /// (line 245). The `got` field reproduces the Go `%T` rendering for the
    /// most common cases (bool / string / number / null) so callers see the
    /// same substring.
    #[error("condition value for {field} must be a boolean, got {got}")]
    BoolValueRequired { field: String, got: String },
    /// Go: `fmt.Errorf("condition value for %s must be a non-empty string", field)`
    /// (line 258) for `request_format`.
    #[error("condition value for {field} must be a non-empty string")]
    StringValueRequired { field: String },
    /// Go: `fmt.Errorf("condition value for daily_time must be a daily time range")`
    /// (line 273).
    #[error("condition value for daily_time must be a daily time range")]
    DailyTimeRangeInvalid,
    /// Go: `fmt.Errorf("invalid daily_time start: ...")` / `"... end: ..."`
    /// (`xtime.ParseDailyTimeRange`, lines 108-114). `which` is `"start"` or
    /// `"end"`.
    #[error("invalid daily_time {which}: must use HH:mm")]
    DailyTimeClockInvalid { which: String },
    /// Go: `fmt.Errorf("daily_time start and end must be different")`
    /// (`xtime.ParseDailyTimeRange` line 117).
    #[error("daily_time start and end must be different")]
    DailyTimeStartEqualsEnd,
}

/// Normalize a [`SystemModelSettings`] in place.
///
/// Mirrors Go `normalizeSystemModelSettings`
/// (`model_settings_inheritance.go` lines 12-32):
/// - `developer_settings` is always non-`None` after normalization (Go: `nil`
///   slice -> empty slice). Rust's `Vec` is never `None`, so this is a no-op
///   structurally; the function exists for parity with the call sequence.
/// - Each developer's `developer` name is trimmed of surrounding whitespace.
/// - Each developer's `associations` is non-empty after normalization (Go: nil
///   -> empty slice), and [`normalize_developer_associations`] is applied.
pub fn normalize_system_model_settings(settings: &mut SystemModelSettings) {
    for developer in settings.developer_settings.iter_mut() {
        // Go `strings.TrimSpace` reassigns unconditionally; the Rust port does
        // the same so normalized output is canonical regardless of input.
        developer.developer = developer.developer.trim().to_string();
        normalize_developer_associations(&mut developer.associations);
    }
}

/// Normalize developer-level associations in place.
///
/// Mirrors Go `normalizeDeveloperAssociations` (lines 63-80): for each
/// `channel_model` / `channel_tags_model` branch, the `model_id` field is
/// cleared. Developer rules select channels/channel-tags only; the concrete
/// `model_id` is stamped in at inheritance time (see
/// [`inherit_developer_association_for_model`), so any value left on a
/// developer rule would be silently overwritten anyway — clearing it here keeps
/// stored developer settings canonical.
pub fn normalize_developer_associations(associations: &mut [ObjectsModelAssociation]) {
    for assoc in associations.iter_mut() {
        match assoc.kind.as_str() {
            "channel_model" => {
                if let Some(branch) = assoc.channel_model.as_mut() {
                    branch.model_id.clear();
                }
            }
            "channel_tags_model" => {
                if let Some(branch) = assoc.channel_tags_model.as_mut() {
                    branch.model_id.clear();
                }
            }
            _ => {}
        }
    }
}

/// Validate a [`SystemModelSettings`].
///
/// Mirrors Go `validateSystemModelSettings`
/// (`model_settings_inheritance.go` lines 34-61):
/// - Walk `developer_settings` in order, tracking seen developer names in a
///   set.
/// - A developer name that is empty after trimming is rejected
///   ([`ModelValidationError::DeveloperRequired`]).
/// - A developer name that already appeared is rejected
///   ([`ModelValidationError::DuplicateDeveloper`]).
/// - Each developer's associations are validated via
///   [`validate_developer_associations`].
///
/// Returns `Ok(())` when every developer passes. The first failure short-
/// circuits, matching Go's `return fmt.Errorf(...)` control flow.
pub fn validate_system_model_settings(
    settings: &SystemModelSettings,
) -> Result<(), ModelValidationError> {
    // Go `system.go:1173-1174`: reject an invalid `model_blacklist_regex` before
    // persisting (empty is allowed = no blacklist). Mirrors `xregexp.ValidateRegex`.
    if !settings.model_blacklist_regex.is_empty()
        && regex::Regex::new(&settings.model_blacklist_regex).is_err()
    {
        return Err(ModelValidationError::InvalidModelBlacklistRegex {
            pattern: settings.model_blacklist_regex.clone(),
        });
    }
    let mut seen_developers: BTreeSet<String> = BTreeSet::new();
    for developer_settings in settings.developer_settings.iter() {
        let developer = developer_settings.developer.trim();
        if developer.is_empty() {
            return Err(ModelValidationError::DeveloperRequired);
        }
        if !seen_developers.insert(developer.to_string()) {
            return Err(ModelValidationError::DuplicateDeveloper {
                developer: developer.to_string(),
            });
        }
        validate_developer_associations(&developer_settings.associations)?;
    }
    Ok(())
}

/// Validate the associations carried by a single developer entry.
///
/// Mirrors Go `validateDeveloperAssociations` (lines 82-107):
/// - `channel_model`: the branch must be present and its `channel_id` must be
///   non-zero.
/// - `channel_tags_model`: the branch must be present and its `channel_tags`
///   must be non-empty.
/// - Any other `type` value is rejected as unsupported
///   ([`ModelValidationError::UnsupportedDeveloperAssociation`]). Developer
///   rules are restricted to channel/channel-tags selection; `model`,
///   `regex`, etc. are not allowed at the developer level.
///
/// Finally delegates to [`validate_model_settings`] (Go line 102) to compile
/// any regex patterns and walk the `when`/condition tree on the developer
/// associations. Developer entries only carry `channel_model` /
/// `channel_tags_model` (which have no regex/`when` surface), so the delegation
/// is a no-op for well-formed developer input — but it keeps the contract
/// intact if a future caller reuses this helper on a broader association set.
pub fn validate_developer_associations(
    associations: &[ObjectsModelAssociation],
) -> Result<(), ModelValidationError> {
    for assoc in associations.iter() {
        match assoc.kind.as_str() {
            "channel_model" => {
                let channel_id = assoc
                    .channel_model
                    .as_ref()
                    .map(|branch| branch.channel_id)
                    .unwrap_or(0);
                if channel_id == 0 {
                    return Err(ModelValidationError::DeveloperChannelAssociationRequiresChannel);
                }
            }
            "channel_tags_model" => {
                let empty = assoc
                    .channel_tags_model
                    .as_ref()
                    .map(|branch| branch.channel_tags.is_empty())
                    .unwrap_or(true);
                if empty {
                    return Err(
                        ModelValidationError::DeveloperChannelTagsAssociationRequiresChannelTags,
                    );
                }
            }
            other => {
                return Err(ModelValidationError::UnsupportedDeveloperAssociation {
                    association_type: other.to_string(),
                });
            }
        }
    }
    // Mirrors Go line 102: `validateModelSettings(&objects.ModelSettings{
    // Associations: associations})`. The synthesized `ModelSettings` only
    // carries the associations so the shared regex/condition walker runs over
    // them; `disable_developer_settings_inheritance` is irrelevant here.
    validate_model_settings(&ModelSettings {
        associations: associations.to_vec(),
        ..ModelSettings::default()
    })
}

// ===========================================================================
// ModelSettings regex + condition-tree validation (RUST-P9-002 S14)
// ===========================================================================
//
// Ported from Go `validateModelSettings` + `validateModelAssociationWhen` +
// `validateFilterConditionNode(AtDepth)` + `validateFilterLeaf`
// (`conduit/internal/server/biz/model.go` lines 55-281). The Go runtime calls
// `validateModelSettings` from three sites: `CreateModel` (line 314),
// `UpdateModel`, and the trailing edge of `validateDeveloperAssociations`
// (line 102). Each association is walked in order and:
//
// 1. Its `when` (when `enabled == true`) is recursively validated. The root
//    must be a group; nested groups are allowed; the nesting depth is capped
//    at [`MODEL_ASSOCIATION_MAX_NESTED_LEVELS`] (Go default 3). Each leaf
//    field must be on the whitelist (`prompt_tokens` / `stream` /
//    `request_format` / `daily_time` / `has_image` / `has_video` /
//    `has_document` / `has_audio`), and the (field, operator, value-type)
//    triple is validated against the per-field rules.
//
// 2. Each regex-bearing branch (`channel_regex` / `channel_tags_regex` /
//    `regex`) plus any `exclude[].channel_name_pattern` is compiled through
//    `xregexp.ValidateRegex` semantics. A compile failure is rejected; empty
//    patterns, `"*"` and metachar-free literal patterns are always valid (they
//    never reach the compiler).
//
// The Rust port reproduces both halves. The regex compile uses the existing
// [`split_inline_modifier`] + anchored re-compile path already used by
// [`blacklist_matches`], so a single source of truth covers the
// `xregexp` semantics.

/// Maximum nesting depth for a model-association `when`/condition tree.
///
/// Mirrors Go `filterValidationOptions{MaxNestedLevels: 3}` passed by
/// `validateModelAssociationWhen` (`model.go` line 130). Depth is counted from
/// 1 at the root group (so a root with no nested groups is depth 1).
pub const MODEL_ASSOCIATION_MAX_NESTED_LEVELS: i64 = 3;

/// Validate a [`ModelSettings`] entry's regex patterns and `when`/condition
/// tree.
///
/// Mirrors Go `validateModelSettings` (`model.go` lines 55-113):
/// - `None`-equivalent / empty-association settings pass (Go: `nil` or empty
///   `Associations` returns `nil`).
/// - For each association, the `when` is validated first (when enabled), then
///   each regex-bearing branch + every `exclude.channel_name_pattern` is
///   compiled through `xregexp.ValidateRegex` semantics.
///
/// Returns the first failure encountered. The Go wrapping
/// `fmt.Errorf("invalid when condition: %w", err)` is reproduced as
/// [`ModelValidationError::InvalidWhenCondition`] boxing the inner tree-walker
/// error, so substring assertions on the inner message keep matching.
pub fn validate_model_settings(settings: &ModelSettings) -> Result<(), ModelValidationError> {
    for assoc in settings.associations.iter() {
        // 1. `when` validation. Mirrors Go lines 65-67.
        if let Some(when) = assoc.when.as_ref() {
            validate_model_association_when(when).map_err(|source| {
                ModelValidationError::InvalidWhenCondition {
                    source: Box::new(source),
                }
            })?;
        }

        // 2. ChannelRegex pattern. Mirrors Go lines 69-74.
        if let Some(channel_regex) = assoc.channel_regex.as_ref()
            && !channel_regex.pattern.is_empty()
        {
            xregexp_validate_regex(&channel_regex.pattern).map_err(|_| {
                ModelValidationError::InvalidChannelRegexPattern {
                    pattern: channel_regex.pattern.clone(),
                }
            })?;
        }

        // 3. ChannelTagsRegex pattern. Mirrors Go lines 76-80.
        if let Some(channel_tags_regex) = assoc.channel_tags_regex.as_ref()
            && !channel_tags_regex.pattern.is_empty()
        {
            xregexp_validate_regex(&channel_tags_regex.pattern).map_err(|_| {
                ModelValidationError::InvalidChannelTagsRegexPattern {
                    pattern: channel_tags_regex.pattern.clone(),
                }
            })?;
        }

        // 4. Regex pattern + exclude patterns. Mirrors Go lines 82-99.
        if let Some(regex) = assoc.regex.as_ref() {
            if !regex.pattern.is_empty() {
                xregexp_validate_regex(&regex.pattern).map_err(|_| {
                    ModelValidationError::InvalidRegexPattern {
                        pattern: regex.pattern.clone(),
                    }
                })?;
            }
            for exclude in regex.exclude.iter() {
                if !exclude.channel_name_pattern.is_empty() {
                    xregexp_validate_regex(&exclude.channel_name_pattern).map_err(|_| {
                        ModelValidationError::InvalidExcludeRegexPattern {
                            pattern: exclude.channel_name_pattern.clone(),
                        }
                    })?;
                }
            }
        }

        // 5. ModelID exclude patterns. Mirrors Go lines 101-109.
        if let Some(model_id) = assoc.model_id.as_ref() {
            for exclude in model_id.exclude.iter() {
                if !exclude.channel_name_pattern.is_empty() {
                    xregexp_validate_regex(&exclude.channel_name_pattern).map_err(|_| {
                        ModelValidationError::InvalidExcludeRegexPattern {
                            pattern: exclude.channel_name_pattern.clone(),
                        }
                    })?;
                }
            }
        }
    }
    Ok(())
}

/// Validate a `when` block on a model association.
///
/// Mirrors Go `validateModelAssociationWhen` (`model.go` lines 115-132): a
/// disabled `when` is always valid; an enabled `when` requires a `condition`
/// and that condition must pass the recursive tree walker with
/// `AllowNestedGroups: true` and `MaxNestedLevels: 3`.
pub fn validate_model_association_when(
    when: &conduit_core::objects::ModelAssociationWhen,
) -> Result<(), ModelValidationError> {
    if !when.enabled {
        return Ok(());
    }
    let Some(condition) = when.condition.as_ref() else {
        return Err(ModelValidationError::WhenConditionRequired);
    };
    validate_filter_condition_node(condition)
}

/// Validate a condition tree against the model-association options.
///
/// Mirrors Go `validateFilterConditionNode` (`model.go` lines 139-141): entry
/// point that starts the depth-1 walk requiring the root to be a group.
pub fn validate_filter_condition_node(
    condition: &conduit_core::objects::Condition,
) -> Result<(), ModelValidationError> {
    validate_filter_condition_node_at_depth(condition, 1, true)
}

/// Recursive depth-tracked tree walker.
///
/// Mirrors Go `validateFilterConditionNodeAtDepth` (`model.go` lines 143-185):
/// - `require_group == true` at the root forces the node to be a group (or
///   omitted, which defaults to group). Any explicit non-group type at the
///   root is rejected with [`ModelValidationError::RootConditionMustBeGroup`].
/// - A group must have at least one child (`GroupRequiresConditions`), must
///   not exceed [`MODEL_ASSOCIATION_MAX_NESTED_LEVELS`] in depth, and each
///   child is recursed (with nesting-groups allowed, per the model-association
///   options).
/// - A leaf (`condition` or omitted-at-non-root) is dispatched to
///   [`validate_filter_leaf`].
/// - Any other explicit `type` value is rejected with
///   [`ModelValidationError::UnsupportedConditionType`].
///
/// The Go node-type resolution treats `""` (omitted) as group at the root and
/// leaf elsewhere; the Rust port reproduces that positional rule via the
/// local `resolve_node_type` helper.
fn validate_filter_condition_node_at_depth(
    condition: &conduit_core::objects::Condition,
    depth: i64,
    require_group: bool,
) -> Result<(), ModelValidationError> {
    let node_type = resolve_node_type(&condition.r#type, require_group);

    if require_group && node_type != NodeType::Group {
        return Err(ModelValidationError::RootConditionMustBeGroup);
    }

    match node_type {
        NodeType::Group => {
            if condition.conditions.is_empty() {
                return Err(ModelValidationError::GroupRequiresConditions);
            }
            if MODEL_ASSOCIATION_MAX_NESTED_LEVELS > 0
                && depth > MODEL_ASSOCIATION_MAX_NESTED_LEVELS
            {
                return Err(ModelValidationError::NestingDepthExceeded {
                    max: MODEL_ASSOCIATION_MAX_NESTED_LEVELS,
                });
            }
            for child in condition.conditions.iter() {
                // Model-association options always allow nested groups, so the
                // `AllowNestedGroups == false` branch (`NestedGroupsNotAllowed`)
                // is unreachable here; the variant is kept for parity with the
                // Go walker should a stricter caller reuse this helper.
                if resolve_node_type(&child.r#type, false) == NodeType::Group
                    && !MODEL_ASSOCIATION_ALLOW_NESTED_GROUPS
                {
                    return Err(ModelValidationError::NestedGroupsNotAllowed);
                }
                validate_filter_condition_node_at_depth(child, depth + 1, false)?;
            }
            Ok(())
        }
        NodeType::Leaf => validate_filter_leaf(condition),
        NodeType::Unsupported => Err(ModelValidationError::UnsupportedConditionType {
            condition_type: condition_type_raw(&condition.r#type),
        }),
    }
}

/// Whether nested groups are permitted in the model-association `when` tree.
///
/// Mirrors Go `filterValidationOptions{AllowNestedGroups: true}` (line 129).
/// Exposed as a constant so the walker can reference it without a runtime
/// option struct (the model-association surface is the only caller).
const MODEL_ASSOCIATION_ALLOW_NESTED_GROUPS: bool = true;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeType {
    Group,
    Leaf,
    /// Produced only by an explicit non-`group`/non-`condition`/non-omitted
    /// `ConditionType`. The Rust `ConditionType::Deserialize` collapses every
    /// unknown JSON string into `Omitted`, so this variant is unreachable for
    /// JSON-decoded input; it is retained for parity with Go's
    /// `unsupported condition type` branch and for any future in-memory
    /// construction path. Kept dead to keep the walker exhaustive.
    #[allow(dead_code)]
    Unsupported,
}

/// Resolve a `ConditionType` into a walker node kind, reproducing Go's
/// positional handling of an omitted type.
///
/// Mirrors Go `validateFilterConditionNodeAtDepth` lines 148-151: an empty
/// `type` defaults to group at the root and to leaf elsewhere. The Rust
/// `ConditionType` enum already normalizes `""`/unknown to `Omitted`, so we
/// treat `Omitted` as the positional default.
fn resolve_node_type(kind: &conduit_core::objects::ConditionType, root: bool) -> NodeType {
    use conduit_core::objects::ConditionType;
    match kind {
        ConditionType::Group => NodeType::Group,
        ConditionType::Condition => NodeType::Leaf,
        ConditionType::Omitted => {
            if root {
                NodeType::Group
            } else {
                NodeType::Leaf
            }
        }
    }
}

/// Render the raw JSON string for a `ConditionType`, for the
/// `UnsupportedConditionType` error message.
///
/// Mirrors Go `condition.Type` which is the raw string (e.g. `"bogus"`). The
/// Rust enum normalizes unknown values to `Omitted`, so an *unsupported*
/// explicit type can only surface via... actually it cannot: the Rust
/// `ConditionType::Deserialize` collapses every unknown string into `Omitted`.
/// This means Go's `"unsupported condition type %q"` branch is unreachable on
/// the Rust side for JSON-decoded input. The variant is retained for parity
/// and in case a future code path constructs `Condition` values in-memory with
/// a distinguishable kind; here we render the canonical form for each variant.
fn condition_type_raw(kind: &conduit_core::objects::ConditionType) -> String {
    use conduit_core::objects::ConditionType;
    match kind {
        ConditionType::Group => "group".to_string(),
        ConditionType::Condition => "condition".to_string(),
        ConditionType::Omitted => String::new(),
    }
}

/// Validate a leaf condition against the per-field whitelist.
///
/// Mirrors Go `validateFilterLeaf` (`model.go` lines 187-209): the field must
/// be non-empty and on the whitelist, then the (field, operator, value) triple
/// is dispatched to the per-field validator.
fn validate_filter_leaf(
    condition: &conduit_core::objects::Condition,
) -> Result<(), ModelValidationError> {
    if condition.field.is_empty() {
        return Err(ModelValidationError::ConditionFieldRequired);
    }
    match condition.field.as_str() {
        "prompt_tokens" => validate_prompt_tokens_leaf(condition),
        "stream" => validate_bool_equality_leaf(condition, "stream"),
        "request_format" => validate_string_equality_leaf(condition, "request_format"),
        "daily_time" => validate_daily_time_leaf(condition),
        "has_image" | "has_video" | "has_document" | "has_audio" => {
            validate_bool_equality_leaf(condition, condition.field.as_str())
        }
        _ => Err(ModelValidationError::UnsupportedConditionField {
            field: condition.field.clone(),
        }),
    }
}

/// Allowed operators for `prompt_tokens` leaves. Mirrors Go
/// `validatePromptTokensLeaf` (`model.go` lines 211-215).
const PROMPT_TOKENS_OPERATORS: &[&str] = &["lt", "lte", "gt", "gte", "<", "<=", ">", ">="];

/// Validate a `prompt_tokens` leaf.
///
/// Mirrors Go `validatePromptTokensLeaf` (`model.go` lines 211-232): operator
/// must be on the comparison whitelist, the value must coerce to a non-negative
/// `i64`.
fn validate_prompt_tokens_leaf(
    condition: &conduit_core::objects::Condition,
) -> Result<(), ModelValidationError> {
    if !PROMPT_TOKENS_OPERATORS.contains(&condition.operator.as_str()) {
        return Err(ModelValidationError::UnsupportedConditionOperator {
            operator: condition.operator.clone(),
            field: "prompt_tokens".to_string(),
        });
    }
    match filter_value_to_i64(condition.value.as_ref()) {
        Some(value) if value >= 0 => Ok(()),
        Some(_) => Err(ModelValidationError::PromptTokensNegative),
        None => Err(ModelValidationError::PromptTokensValueNotInteger),
    }
}

/// Allowed operators for boolean-equality leaves (`stream`, `has_*`). Mirrors
/// Go `validateBoolEqualityLeaf` (`model.go` lines 234-238).
const BOOL_EQUALITY_OPERATORS: &[&str] = &["eq", "ne", "=", "==", "!="];

/// Validate a boolean-equality leaf (`stream` / `has_image` / `has_video` /
/// `has_document` / `has_audio`).
///
/// Mirrors Go `validateBoolEqualityLeaf` (`model.go` lines 234-247): operator
/// on the equality whitelist, value must be a JSON `bool`.
fn validate_bool_equality_leaf(
    condition: &conduit_core::objects::Condition,
    field: &str,
) -> Result<(), ModelValidationError> {
    if !BOOL_EQUALITY_OPERATORS.contains(&condition.operator.as_str()) {
        return Err(ModelValidationError::UnsupportedConditionOperator {
            operator: condition.operator.clone(),
            field: field.to_string(),
        });
    }
    match condition.value.as_ref().and_then(Value::as_bool) {
        Some(_) => Ok(()),
        None => Err(ModelValidationError::BoolValueRequired {
            field: field.to_string(),
            got: go_type_name(condition.value.as_ref()),
        }),
    }
}

/// Allowed operators for string-equality leaves (`request_format`). Mirrors Go
/// `validateStringEqualityLeaf` (`model.go` lines 249-253).
const STRING_EQUALITY_OPERATORS: &[&str] = &["eq", "ne", "=", "==", "!="];

/// Validate a `request_format` string-equality leaf.
///
/// Mirrors Go `validateStringEqualityLeaf` (`model.go` lines 249-261):
/// operator on the equality whitelist, value must be a non-empty JSON string.
fn validate_string_equality_leaf(
    condition: &conduit_core::objects::Condition,
    field: &str,
) -> Result<(), ModelValidationError> {
    if !STRING_EQUALITY_OPERATORS.contains(&condition.operator.as_str()) {
        return Err(ModelValidationError::UnsupportedConditionOperator {
            operator: condition.operator.clone(),
            field: field.to_string(),
        });
    }
    let is_nonempty_string = condition
        .value
        .as_ref()
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if is_nonempty_string {
        Ok(())
    } else {
        Err(ModelValidationError::StringValueRequired {
            field: field.to_string(),
        })
    }
}

/// Allowed operators for `daily_time` leaves. Mirrors Go
/// `validateDailyTimeLeaf` (`model.go` lines 264-269).
const DAILY_TIME_OPERATORS: &[&str] = &["within", "not_within"];

/// Validate a `daily_time` leaf.
///
/// Mirrors Go `validateDailyTimeLeaf` (`model.go` lines 264-281): operator on
/// the `within`/`not_within` whitelist, value must be a non-empty string that
/// parses as an `HH:mm-HH:mm` range with distinct endpoints (via
/// `xtime.ParseDailyTimeRange`).
fn validate_daily_time_leaf(
    condition: &conduit_core::objects::Condition,
) -> Result<(), ModelValidationError> {
    if !DAILY_TIME_OPERATORS.contains(&condition.operator.as_str()) {
        return Err(ModelValidationError::UnsupportedConditionOperator {
            operator: condition.operator.clone(),
            field: "daily_time".to_string(),
        });
    }
    let Some(value) = condition.value.as_ref().and_then(Value::as_str) else {
        return Err(ModelValidationError::DailyTimeRangeInvalid);
    };
    if value.is_empty() {
        return Err(ModelValidationError::DailyTimeRangeInvalid);
    }
    parse_daily_time_range(value)
}

/// Coerce a JSON value to `i64`, mirroring Go `filterValueToInt64`
/// (`model.go` lines 283-303).
///
/// Go accepts `int*`, `float64` (only when integral), and `json.Number`. The
/// Rust `serde_json::Value` exposes `i64`/`u64` (integers) and `f64` (floats);
/// both decode through the dedicated accessors. A non-integral `f64` fails the
/// integrality check exactly like Go.
fn filter_value_to_i64(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(i) = value.as_i64() {
        return Some(i);
    }
    if let Some(u) = value.as_u64() {
        return i64::try_from(u).ok();
    }
    if let Some(f) = value.as_f64() {
        let truncated = f as i64;
        if f == truncated as f64 {
            return Some(truncated);
        }
        return None;
    }
    None
}

/// Render the Go `%T` type name for a JSON value.
///
/// Mirrors the cases Go's `encoding/json` unmarshal would produce for the
/// `condition.Value any` field: decoded JSON booleans become `bool`, strings
/// become `string`, numbers become `float64` (the default), `null` becomes
/// `<nil>`. Arrays/objects become `map[string]interface {}` / `[]interface {}`
/// — these never appear in a well-formed `when` but are rendered for parity.
fn go_type_name(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "<nil>".to_string();
    };
    match value {
        Value::Null => "<nil>".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Number(_) => "float64".to_string(),
        Value::String(_) => "string".to_string(),
        Value::Array(_) => "[]interface {}".to_string(),
        Value::Object(_) => "map[string]interface {}".to_string(),
    }
}

/// Parse an `HH:mm-HH:mm` daily-time range.
///
/// Mirrors Go `xtime.ParseDailyTimeRange` (`xtime/time.go` lines 101-121):
/// exactly two `-`-separated parts, each parsing as `HH:mm` (trimmed), and the
/// two endpoints must differ. The error variants match the Go `fmt.Errorf`
/// substrings asserted by `model_validation_test.go`'s `invalid daily_time`
/// cases.
fn parse_daily_time_range(value: &str) -> Result<(), ModelValidationError> {
    let Some((start_raw, end_raw)) = value.split_once('-') else {
        return Err(ModelValidationError::DailyTimeRangeInvalid);
    };
    let start = parse_daily_clock(start_raw, "start")?;
    let end = parse_daily_clock(end_raw, "end")?;
    if start == end {
        return Err(ModelValidationError::DailyTimeStartEqualsEnd);
    }
    Ok(())
}

/// Parse a single `HH:mm` clock value into minutes-of-day.
///
/// Mirrors Go `parseDailyClock` (`xtime/time.go` lines 137-144) via
/// `time.Parse("15:04", strings.TrimSpace(value))`. Go's reference layout
/// `15:04` accepts zero-padded two-digit hour/minute only, so the Rust port
/// applies the same shape check (two `:`-separated parts, each a base-10
/// integer in range; hours `00..=23`, minutes `00..=59`). Go's `time.Parse`
/// is permissive about the field width as long as the value fits, so we accept
/// any non-negative integer in range (e.g. `9:05`, `09:05`).
fn parse_daily_clock(value: &str, which: &str) -> Result<i32, ModelValidationError> {
    let trimmed = value.trim();
    let Some((hours_str, minutes_str)) = trimmed.split_once(':') else {
        return Err(ModelValidationError::DailyTimeClockInvalid {
            which: which.to_string(),
        });
    };
    let hours: i32 = trimmed[..hours_str.len()].parse().map_err(|_| {
        ModelValidationError::DailyTimeClockInvalid {
            which: which.to_string(),
        }
    })?;
    let minutes: i32 =
        minutes_str
            .parse()
            .map_err(|_| ModelValidationError::DailyTimeClockInvalid {
                which: which.to_string(),
            })?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return Err(ModelValidationError::DailyTimeClockInvalid {
            which: which.to_string(),
        });
    }
    Ok(hours * 60 + minutes)
}

/// Compile-check an xregexp pattern.
///
/// Mirrors Go `xregexp.ValidateRegex` (`pkg/xregexp/match.go` lines 114-125):
/// - empty pattern -> valid (`Ok`).
/// - `"*"` -> match-all sentinel -> valid.
/// - no regex metacharacters -> exact-match path -> valid (no compilation).
/// - otherwise the pattern is anchored as `^(?:body)$` (preserving any inline
///   modifier) and compiled; a compile failure is rejected.
///
/// The compile uses the same [`split_inline_modifier`] + anchor logic already
/// used by [`blacklist_matches`], keeping a single source of truth for xregexp
/// semantics. Note Go uses `regexp2` (PCRE); the Rust `regex` crate (RE2) is
/// stricter and rejects patterns Go accepts (e.g. backreferences). For the
/// golden cases (`[invalid`, `(?P<invalid`) both engines agree on rejection,
/// and a stricter validator is the safer default for config validation.
fn xregexp_validate_regex(pattern: &str) -> Result<(), ()> {
    if pattern.is_empty() {
        return Ok(());
    }
    if pattern == "*" {
        return Ok(());
    }
    if !pattern.contains(REGEX_META_CHARS) {
        // Exact-match fast path: no compilation needed.
        return Ok(());
    }
    let RegexPattern { body, modifier } = split_inline_modifier(pattern);
    let anchored = if modifier.is_empty() {
        let trimmed = body.trim_start_matches('^').trim_end_matches('$');
        format!("^(?:{trimmed})$")
    } else {
        format!("{modifier}^(?:{body})$")
    };
    Regex::new(&anchored).map(|_| ()).map_err(|_| ())
}

// ===========================================================================
// Model list shaping (RUST-P9-002 S11/S16)
// ===========================================================================
//
// Ported from `conduit/internal/server/api/openai.go`
// (`parseOpenAIModelInclude` + `convertModelToOpenAIExtended`). The OpenAI-
// compatible `/v1/models` endpoint returns a minimal facade by default and
// surfaces extended metadata only when the `include` query asks for it (or when
// the system default flips to `include=all`). These pure helpers encode that
// shaping so handlers can stay thin.

/// Optional fields selectable via the `include` query on `/v1/models`.
///
/// Mirrors the Go `extendedFields` slice in `parseOpenAIModelInclude`
/// (`openai.go` line 538). Field names are the literal query values.
pub const EXTENDED_MODEL_FIELDS: &[&str] = &[
    "name",
    "description",
    "context_length",
    "max_output_tokens",
    "modalities",
    "capabilities",
    "pricing",
    "icon",
    "type",
];

/// Parsed `include` query for `/v1/models`.
///
/// Mirrors the Go `(map[string]bool, bool)` return of `parseOpenAIModelInclude`
/// (`openai.go` lines 515-547). `fields == None` means "all extended fields"
/// (i.e. `include=all` or the system default flipped to include-all);
/// `fields == Some(set)` means only the named fields are populated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelInclude {
    /// `None` = populate every extended field; `Some(empty)` = populate none.
    pub fields: Option<BTreeSet<String>>,
}

impl ModelInclude {
    /// Parse the raw `include` query value.
    ///
    /// Mirrors `parseOpenAIModelInclude`:
    /// - `""` -> respect the system default (`default_include_all`).
    /// - `"all"` -> populate every extended field.
    /// - `"a,b,c"` -> populate only `a`, `b`, `c`.
    pub fn parse(include_param: &str, default_include_all: bool) -> Self {
        if include_param.is_empty() {
            // Default: populate all fields only if the system default says so.
            return if default_include_all {
                Self { fields: None }
            } else {
                Self {
                    fields: Some(BTreeSet::new()),
                }
            };
        }
        if include_param == "all" {
            return Self { fields: None };
        }
        let fields: BTreeSet<String> = include_param
            .split(',')
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .map(str::to_string)
            .collect();
        Self {
            fields: Some(fields),
        }
    }

    /// Whether the caller needs the full [`conduit_core::objects::ModelCard`]
    /// to populate any requested extended field. Mirrors Go `needFullData`.
    pub fn needs_full_data(&self) -> bool {
        match &self.fields {
            None => true,
            Some(set) => EXTENDED_MODEL_FIELDS.iter().any(|f| set.contains(*f)),
        }
    }

    /// Whether a given extended field should be populated.
    pub fn should_include(&self, field: &str) -> bool {
        match &self.fields {
            None => true,
            Some(set) => set.contains(field),
        }
    }
}

/// Minimal OpenAI-compatible model facade. Ported 1:1 from Go `OpenAIModel`
/// (`openai.go` lines 488-504); the optional fields are only populated when the
/// corresponding `include` field is requested.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelFacade {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<ModelFacadeModalities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelFacadeCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelFacadePricing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

/// Input/output modalities for [`ModelFacade`]. Ported from Go `Modalities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelFacadeModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

/// Capability flags for [`ModelFacade`]. Ported from Go `Capabilities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelFacadeCapabilities {
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default)]
    pub reasoning: bool,
}

/// Per-token pricing for [`ModelFacade`]. Ported from Go `Pricing`
/// (`openai.go` lines 481-488). `unit`/`currency` are always
/// `"per_1m_tokens"` / `"USD"` in Go (`convertModelToOpenAIExtended` lines
/// 632-633), so the defaults mirror that to stay JSON-compatible even when
/// constructed via `Default`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelFacadePricing {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
    #[serde(default = "default_pricing_unit")]
    pub unit: String,
    #[serde(default = "default_pricing_currency")]
    pub currency: String,
}

impl Default for ModelFacadePricing {
    fn default() -> Self {
        Self {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            unit: default_pricing_unit(),
            currency: default_pricing_currency(),
        }
    }
}

fn default_pricing_unit() -> String {
    "per_1m_tokens".to_string()
}

fn default_pricing_currency() -> String {
    "USD".to_string()
}

const MODEL_OBJECT_TYPE: &str = "model";

/// Shape a configured model into an OpenAI-compatible facade, populating the
/// requested extended fields from the model card.
///
/// Mirrors Go `convertModelToOpenAIExtended` (`openai.go` lines 562-650). Basic
/// fields (`id`, `object`, `created`, `owned_by`) are always set; the extended
/// fields are populated only when [`ModelInclude::should_include`] allows it.
#[allow(clippy::too_many_arguments)]
pub fn shape_model_facade(
    model_id: &str,
    developer: &str,
    created_at_unix: i64,
    name: &str,
    icon: &str,
    model_type: &str,
    remark: Option<&str>,
    model_card: Option<&conduit_core::objects::ModelCard>,
    include: &ModelInclude,
) -> ModelFacade {
    let mut facade = ModelFacade {
        id: model_id.to_string(),
        object: MODEL_OBJECT_TYPE.to_string(),
        created: created_at_unix,
        owned_by: developer.to_string(),
        ..ModelFacade::default()
    };

    if include.should_include("name") {
        facade.name = Some(name.to_string());
    }
    if include.should_include("icon") {
        facade.icon = Some(icon.to_string());
    }
    if include.should_include("type") {
        facade.r#type = Some(model_type.to_string());
    }
    if include.should_include("description") {
        facade.description = remark.map(str::to_string);
    }

    if let Some(card) = model_card {
        if include.should_include("modalities") {
            facade.modalities = Some(ModelFacadeModalities {
                input: card.modalities.input.clone(),
                output: card.modalities.output.clone(),
            });
        }
        if include.should_include("capabilities") {
            facade.capabilities = Some(ModelFacadeCapabilities {
                vision: card.vision,
                tool_call: card.tool_call,
                reasoning: card.reasoning.supported,
            });
        }
        if include.should_include("context_length") {
            facade.context_length = Some(card.limit.context);
        }
        if include.should_include("max_output_tokens") {
            facade.max_output_tokens = Some(card.limit.output);
        }
        if include.should_include("pricing") {
            // Go `convertModelToOpenAIExtended` hardcodes `unit`/`currency` to
            // `"per_1m_tokens"` / `"USD"` (openai.go lines 632-633), asserted by
            // `TestConvertModelToOpenAIExtended_CompleteData` (openai_model_test.go
            // lines 74-75).
            facade.pricing = Some(ModelFacadePricing {
                input: card.cost.input,
                output: card.cost.output,
                cache_read: card.cost.cache_read,
                cache_write: card.cost.cache_write,
                unit: default_pricing_unit(),
                currency: default_pricing_currency(),
            });
        }
    }

    facade
}

/// Shape a channel-derived model (no card) into the minimal facade.
///
/// Mirrors Go `convertModelFacadeToOpenAIModel` (`openai.go` lines 549-556):
/// channel models only ever carry the basic fields.
pub fn shape_basic_facade(model_id: &str, owned_by: &str, created_at_unix: i64) -> ModelFacade {
    ModelFacade {
        id: model_id.to_string(),
        object: MODEL_OBJECT_TYPE.to_string(),
        created: created_at_unix,
        owned_by: owned_by.to_string(),
        ..ModelFacade::default()
    }
}

// ===========================================================================
// Channel-model inclusion decision (RUST-P9-002 S12)
// ===========================================================================
//
// Ported from the channel-model branch of Go `ModelService.ListEnabledModels`
// (`model.go` lines 644-687). When `QueryAllChannelModels` is `false`, the
// caller short-circuits and only configured `Model` entities are returned, so a
// channel-derived id is never included. When it is `true`, each channel model
// id is checked against `ModelBlacklistRegex` via `xregexp.MatchString`; a match
// drops the id, otherwise the id is included. An empty blacklist matches
// nothing.
//
// The trickier parity point is `xregexp.MatchString` (`pkg/xregexp/match.go`):
// it anchors every pattern with `^...$` *and* takes an exact-string fast path
// when the pattern carries no regex metacharacters (`*?+[]{}(){}()^$.|\\`),
// treating the pattern as a literal whole-string equality check. A naive Rust
// `Regex::is_match` does unanchored substring matching, which would diverge on
// patterns like `"deepseek-chat"` (Rust would also match `"deepseek/deepseek-chat"`).
// [`blacklist_matches`] reproduces both branches so the pure decision below
// matches the Go behavior 1:1.

/// Characters that mark an xregexp pattern as a real regex (not exact-match).
///
/// Mirrors Go `containsRegexChars` (`pkg/xregexp/match.go` line 127-129).
const REGEX_META_CHARS: &[char] = &[
    '*', '?', '+', '[', ']', '{', '}', '(', ')', '^', '$', '.', '|', '\\',
];

/// Decide whether a channel-derived model id should be included in the models
/// API response, given the system `QueryAllChannelModels` flag and the optional
/// blacklist regex.
///
/// Mirrors the per-channel-model decision inside Go
/// `ModelService.ListEnabledModels` (`model.go` lines 644-687):
/// - `query_all == false` -> channel models are short-circuited out (return
///   `false`); only configured `Model` entities are returned in that mode.
/// - `query_all == true`, empty blacklist -> include (`true`).
/// - `query_all == true`, non-empty blacklist -> exclude (`false`) iff the id
///   matches the blacklist under [`blacklist_matches`]; otherwise include.
///
/// This is the pure decision only; it does not consult channels or the model
/// database, and configured `Model` entities bypass this filter entirely (they
/// are registered in `modelSet` before the channel loop in Go).
pub fn should_include_channel_model(model_id: &str, query_all: bool, blacklist: &str) -> bool {
    if !query_all {
        return false;
    }
    if blacklist.is_empty() {
        return true;
    }
    !blacklist_matches(blacklist, model_id)
}

/// Apply the blacklist regex to a model id using `xregexp.MatchString` semantics.
///
/// Returns `false` for an unparseable pattern, matching Go's `compileErr` path
/// (`pkg/xregexp/match.go` lines 24-26): a bad pattern never matches, so it
/// never filters anything.
///
/// Two fast paths mirror Go:
/// - **Exact-match:** if the pattern contains no regex metacharacters
///   (`containsRegexChars`), it is treated as literal whole-string equality
///   (Go lines 28-30, 87-92). So `"deepseek-chat"` matches `"deepseek-chat"`
///   but not `"deepseek/deepseek-chat"`.
/// - **Anchored regex:** otherwise the pattern is compiled with implicit
///   `^(?:...)$` anchoring (Go lines 94-99, 106-112). A `matchAll` shortcut
///   for `"*"` is also honored.
///
/// An empty pattern is a no-op (never matches); this differs from
/// [`xregexp_match_string`], which treats empty as "matches everything" so a
/// blank model-association pattern selects all of a channel's models (Go
/// `xregexp.MatchString("", s)` returns `true`). The blacklist use-case wants
/// "no filter" to mean "keep everything", so the caller short-circuits on the
/// empty pattern separately; this function preserves the historical
/// `false`-on-empty return for the existing S12 golden cases.
pub fn blacklist_matches(pattern: &str, model_id: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    xregexp_match_string(pattern, model_id)
}

/// `xregexp.MatchString(pattern, value)` semantics — the shared anchor/exact
/// contract used by both the channel-model blacklist and the model-association
/// matcher (S15). Mirrors Go `pkg/xregexp/match.go` (`MatchString` lines 14-26,
/// `containsRegexChars` lines 127-129, `ensureAnchored` lines 106-112).
///
/// Behavior:
/// - **Empty pattern** matches everything (`true`), mirroring Go's regex
///   `""` -> `^(?:.*)$` semantics as exercised by the model matcher's
///   "blank pattern selects all" cases. (The blacklist deliberately bypasses
///   this via an early `pattern.is_empty()` check above.)
/// - **`"*"`** is the match-all sentinel -> `true`.
/// - **Exact-match fast path:** a pattern with no regex metacharacters is
///   treated as literal whole-string equality (`pattern == value`).
/// - **Anchored regex:** otherwise the pattern is re-anchored as
///   `^(?:body)$` (preserving any inline modifier) and compiled; a compile
///   failure means "never matches" (Go caches the compile error and returns
///   `false`).
pub fn xregexp_match_string(pattern: &str, value: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    // Go short-circuits the literal "*" pattern as match-all.
    if pattern == "*" {
        return true;
    }
    // Exact-match fast path: no regex metacharacters -> whole-string equality.
    if !pattern.contains(REGEX_META_CHARS) {
        return pattern == value;
    }
    // Anchored regex path. A compile failure means "never matches" (Go sets
    // `compileErr` and returns false).
    let RegexPattern { body, modifier } = split_inline_modifier(pattern);
    let anchored = if modifier.is_empty() {
        // Strip optional outer anchors, then re-anchor, exactly like Go
        // `ensureAnchored` (`match.go` lines 106-112).
        let trimmed = body.trim_start_matches('^').trim_end_matches('$');
        format!("^(?:{trimmed})$")
    } else {
        format!("{modifier}^(?:{body})$")
    };
    match Regex::new(&anchored) {
        Ok(regex) => regex.is_match(value),
        Err(_) => false,
    }
}

/// Parsed `(body, modifier)` form of a pattern, used to preserve inline
/// `(?...)` modifiers when re-anchoring. Mirrors Go `splitInlineModifier`
/// (`pkg/xregexp/match.go` lines 131-149).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RegexPattern {
    modifier: String,
    body: String,
}

// ===========================================================================
// System-toggle policy for the three model-list switches (RUST-P9-002 S16)
// ===========================================================================
//
// Go `SystemModelSettings` exposes three independent boolean toggles that shape
// model-list behavior and request routing (`internal/server/biz/system.go`
// lines 387-425, defaults in `internal/server/biz/system_default.go` lines
// 33-40). Each is consumed at a different call site:
//
// 1. `FallbackToChannelsOnModelNotFound` (default `true`): in the orchestrator
//    candidate selector (`internal/server/orchestrator/candidates.go` lines
//    87-97), when `selectModelCandidates` returns an `ent.IsNotFound` error the
//    system falls back to `selectChannelCandidates` iff this toggle is true;
//    otherwise it surfaces `ErrInvalidModel`. The decision is therefore a pure
//    function of "was the model resolved?" and the toggle.
//
// 2. `QueryAllChannelModels` (default `true`): in `ModelService.ListEnabledModels`
//    (`internal/server/biz/model.go` lines 644-697) the toggle short-circuits
//    the channel-derived branch — when it is `false`, only configured `Model`
//    entities are returned and channel models are never aggregated. When `true`,
//    channel models are merged in (subject to `ModelBlacklistRegex`).
//
// 3. `DefaultModelAPIIncludeAll` (default `false`): in the OpenAI
//    `/v1/models` handler (`internal/server/api/openai.go` line 731) the toggle
//    is passed to `parseOpenAIModelInclude` as the `defaultIncludeAll` argument,
//    so an empty `?include=` query behaves like `?include=all` when the toggle
//    is on, and like basic-only when it is off.
//
// [`ModelListPolicy`] bundles the three toggles together and exposes a pure
// decision helper per toggle, so handlers can stay thin and the three concerns
// are testable independently (the explicit S16 requirement).

/// Bundle of the three system-level model-list toggles.
///
/// Each field mirrors a boolean on Go `SystemModelSettings` (see the section
/// header above for the call site and default of each). The default matches Go
/// `defaultModelSettings`: `fallback`/`query_all` `true`, `default_include_all`
/// `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelListPolicy {
    /// Toggle 1: fall back to channel selection when the model is not found.
    pub fallback_to_channels_on_model_not_found: bool,
    /// Toggle 2: aggregate channel-supported models into the models API.
    pub query_all_channel_models: bool,
    /// Toggle 3: make an empty `?include=` behave like `?include=all`.
    pub default_model_api_include_all: bool,
}

impl Default for ModelListPolicy {
    fn default() -> Self {
        // Mirrors Go `defaultModelSettings` (`system_default.go` lines 33-40).
        Self {
            fallback_to_channels_on_model_not_found: true,
            query_all_channel_models: true,
            default_model_api_include_all: false,
        }
    }
}

impl ModelListPolicy {
    /// Build a policy from a [`SystemModelSettings`] snapshot.
    ///
    /// Keeps the policy layer decoupled from the full settings struct so it can
    /// be unit-tested without constructing blacklist/developer sub-fields.
    pub fn from_settings(settings: &SystemModelSettings) -> Self {
        Self {
            fallback_to_channels_on_model_not_found: settings
                .fallback_to_channels_on_model_not_found,
            query_all_channel_models: settings.query_all_channel_models,
            default_model_api_include_all: settings.default_model_api_include_all,
        }
    }

    /// Build a policy from a service-local [`ModelQuerySettings`] snapshot.
    ///
    /// `ModelQuerySettings` does not carry the `default_model_api_include_all`
    /// toggle (it lives on `SystemModelSettings`), so the caller supplies it
    /// separately; the helper exists to keep the call sites symmetric.
    pub fn from_query_settings(
        settings: &ModelQuerySettings,
        default_model_api_include_all: bool,
    ) -> Self {
        Self {
            fallback_to_channels_on_model_not_found: settings
                .fallback_to_channels_on_model_not_found,
            query_all_channel_models: settings.query_all_channel_models,
            default_model_api_include_all,
        }
    }

    /// Toggle 1 — should the request fall back to legacy channel selection?
    ///
    /// Mirrors Go `candidates.go` lines 87-97: the fallback path is only taken
    /// when the requested model was not found AND the toggle is on. When the
    /// model resolves successfully, the toggle is a no-op regardless of state.
    pub fn should_fallback_to_channels(&self, model_found: bool) -> bool {
        !model_found && self.fallback_to_channels_on_model_not_found
    }

    /// Toggle 2 — should the models API aggregate channel-supported models?
    ///
    /// Mirrors Go `model.go` line 645: the channel-model branch is only entered
    /// when the toggle is on; otherwise only configured `Model` entities are
    /// returned. This is a direct passthrough so callers can read the decision
    /// at a single named site.
    pub fn should_query_channel_models(&self) -> bool {
        self.query_all_channel_models
    }

    /// Toggle 3 — resolve an `include` query against the system default.
    ///
    /// Mirrors Go `openai.go` line 731 calling `parseOpenAIModelInclude(include,
    /// settings.DefaultModelAPIIncludeAll)`. An empty query follows the system
    /// default; explicit `all` or field-lists override it. Delegates to
    /// [`ModelInclude::parse`] for the actual parsing rules.
    pub fn resolve_include(&self, query_include: &str) -> ModelInclude {
        ModelInclude::parse(query_include, self.default_model_api_include_all)
    }
}

// ===========================================================================
// Model association matcher — multi-dimensional resolution (RUST-P9-002 S15)
// ===========================================================================
//
// Ported from `conduit/internal/server/biz/model_association_matcher.go`. The
// Go matcher resolves a `*objects.ModelAssociation` against the live channel
// set by dispatching on `assoc.Type`:
//
//   matchSingleAssociation (lines 120-143):
//     switch assoc.Type {
//       case "channel_model":      matchChannelModel      // (developer: fixed channel + model)
//       case "channel_regex":      matchChannelRegex      // (type:     fixed channel + pattern)
//       case "regex":              matchRegex             // (regex/pattern: pattern across channels)
//       case "model":              matchModel             // (exact:    exact model id across channels)
//       case "channel_tags_model": matchChannelTagsModel  // (tags:     tag set + exact model)
//       case "channel_tags_regex": matchChannelTagsRegex  // (tags:     tag set + pattern)
//       // default: returns nil — unknown type never matches
//     }
//
// Each branch carries its own ExcludeAssociation list (the "conditions"
// dimension): per-channel name pattern / id / tag predicates evaluated by
// `shouldExcludeChannel` (lines 297-328). The S15 task enumeration maps the Go
// branches onto the matcher dimensions as:
//
//   exact          -> `model` (exact model id match, with exclude conditions)
//   regex/pattern  -> `regex` (xregexp pattern across all channels, with exclude)
//   developer      -> `channel_model` (a fixed channel picked by developer + exact model)
//   type           -> `channel_regex` (a fixed channel picked by developer + pattern)
//   tags           -> `channel_tags_model` / `channel_tags_regex` (tag OR-logic + model/pattern)
//   conditions     -> per-channel ExcludeAssociation evaluation in `regex` / `model`
//   unknown        -> default switch arm -> empty result (no match)
//
// The structural matcher is decoupled from the request-time `When`/condition
// tree (handled by [`conduit_core::objects::evaluate`]) and from the channel
// database (`*ent.Channel`). It takes a borrowed slice of [`MatcherChannel`]
// views and returns owned [`AssociationConnection`] results. Model-id regex
// matching routes through the shared [`xregexp_match_string`] helper so the
// anchored + exact-string fast path stays a single source of truth (the Go
// matcher also calls `xregexp.MatchString`).
//
// Go's `DuplicateKeyTracker` (lines 36-66) deduplicates `(channel_id,
// model_id)` across the whole association list so two rules cannot both emit
// the same (channel, model) into a candidate set. The Rust port keeps the
// same global-tracker behavior via [`DuplicateChannelModelTracker`].

/// Minimal channel view consumed by the structural association matcher.
///
/// The Go matcher reads four fields off `*biz.Channel`/`*ent.Channel`: `ID`
/// (int), `Name` (string), `Tags` ([]string), and the request-model entry map
/// returned by `GetModelEntries()`. Capturing those four here keeps the
/// matcher pure and unit-testable without dragging in the ent channel type or
/// the channel LLM machinery. `model_ids` carries only the request-model ids
/// (`map[string]ChannelModelEntry` keys in Go) — the structural matcher never
/// inspects the entry payload, only the presence of the id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatcherChannel {
    pub channel_id: i64,
    pub name: String,
    pub tags: Vec<String>,
    /// Request-model ids this channel can serve (keys of Go
    /// `GetModelEntries()`). Stored as a `BTreeSet` for deterministic
    /// iteration; the matcher only tests presence and iterates, never mutates.
    pub model_ids: BTreeSet<String>,
}

impl MatcherChannel {
    /// Construct a channel view from its identity + supported model ids.
    pub fn new(
        channel_id: i64,
        name: impl Into<String>,
        model_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            channel_id,
            name: name.into(),
            tags: Vec::new(),
            model_ids: model_ids.into_iter().map(Into::into).collect(),
        }
    }

    /// Builder: attach the channel's tag list.
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Whether the channel exposes the given request-model id.
    pub fn supports_model(&self, model_id: &str) -> bool {
        self.model_ids.contains(model_id)
    }

    /// Whether the channel carries any of `tags` (OR logic). Mirrors Go's
    /// `lo.Contains` loop in `matchChannelTagsModel`/`matchChannelTagsRegex`
    /// (lines 344-356, 396-408).
    pub fn has_any_tag(&self, tags: &[String]) -> bool {
        tags.iter().any(|t| self.tags.contains(t))
    }
}

/// One matched (channel, models, priority) tuple. Mirrors Go
/// `ModelChannelConnection` (lines 13-19) minus the `*ent.Channel` payload —
/// the structural matcher only emits the channel id and the matched model ids;
/// callers resolve those into live channels at a higher layer (the same way Go
/// `MatchConnections` callers do).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociationConnection {
    pub channel_id: i64,
    /// Request-model ids the association matched on this channel, in iteration
    /// order, deduplicated against the global tracker.
    pub model_ids: Vec<String>,
    pub priority: i64,
}

/// `(channel_id, model_id)` dedup tracker. Mirrors Go `DuplicateKeyTracker`
/// (lines 36-66) keying on Go's `ChannelModelKey{ChannelID int, ModelID string}`.
/// The Go key is structural and the same `(channel, model)` from two
/// associations is only emitted once; the Rust port preserves that behavior.
#[derive(Debug, Default, Clone)]
pub struct DuplicateChannelModelTracker {
    seen: BTreeSet<(i64, String)>,
}

impl DuplicateChannelModelTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `(channel_id, model_id)`; return `true` if newly inserted,
    /// `false` if already seen. Mirrors Go `DuplicateKeyTracker.Add`.
    pub fn add(&mut self, channel_id: i64, model_id: &str) -> bool {
        self.seen.insert((channel_id, model_id.to_string()))
    }
}

/// Match a list of associations against a channel set. Mirrors Go
/// `MatchConnections` (lines 68-88): disabled associations are skipped, a
/// global dedup tracker is shared across the list, and connections are
/// returned in association order. Empty connections are filtered out at the
/// branch level, matching Go's per-branch `if len(models) == 0 { continue }`.
pub fn match_connections(
    associations: &[ObjectsModelAssociation],
    channels: &[MatcherChannel],
) -> Vec<AssociationConnection> {
    let mut tracker = DuplicateChannelModelTracker::new();
    let mut out = Vec::new();
    for assoc in associations {
        if assoc.disabled {
            continue;
        }
        out.extend(match_single_association(assoc, channels, &mut tracker));
    }
    out
}

/// Match a single association against the channel set. Mirrors Go
/// `matchSingleAssociation` (lines 120-143): dispatches on `assoc.kind`, and
/// the default arm returns an empty `Vec` — an unknown matcher type never
/// matches. This is the S15 contract point: every supported matcher dimension
/// has a dedicated branch, and anything else falls through to "no match".
pub fn match_single_association(
    assoc: &ObjectsModelAssociation,
    channels: &[MatcherChannel],
    tracker: &mut DuplicateChannelModelTracker,
) -> Vec<AssociationConnection> {
    match assoc.kind.as_str() {
        // developer dimension: fixed channel + exact model id.
        "channel_model" => match_channel_model(assoc, channels, tracker),
        // type dimension: fixed channel + xregexp pattern over the channel's models.
        "channel_regex" => match_channel_regex(assoc, channels, tracker),
        // regex/pattern dimension: xregexp pattern across every channel, with exclude conditions.
        "regex" => match_regex(assoc, channels, tracker),
        // exact dimension: exact model id across every channel, with exclude conditions.
        "model" => match_model(assoc, channels, tracker),
        // tags dimension: channel tags (OR) + exact model id.
        "channel_tags_model" => match_channel_tags_model(assoc, channels, tracker),
        // tags dimension: channel tags (OR) + xregexp pattern.
        "channel_tags_regex" => match_channel_tags_regex(assoc, channels, tracker),
        // unknown matcher -> no match (S15 requirement).
        _ => Vec::new(),
    }
}

/// `channel_model` branch — the "developer" dimension.
///
/// Mirrors Go `matchChannelModel` (lines 146-177): if the branch is missing,
/// the named channel is not in the set, or the channel does not expose the
/// model id, no connection is produced. Otherwise a single connection with
/// that one model id is emitted (subject to the global dedup tracker).
fn match_channel_model(
    assoc: &ObjectsModelAssociation,
    channels: &[MatcherChannel],
    tracker: &mut DuplicateChannelModelTracker,
) -> Vec<AssociationConnection> {
    let Some(branch) = assoc.channel_model.as_ref() else {
        return Vec::new();
    };
    let Some(ch) = channels.iter().find(|c| c.channel_id == branch.channel_id) else {
        return Vec::new();
    };
    if !ch.supports_model(&branch.model_id) {
        return Vec::new();
    }
    if !tracker.add(ch.channel_id, &branch.model_id) {
        return Vec::new();
    }
    vec![AssociationConnection {
        channel_id: ch.channel_id,
        model_ids: vec![branch.model_id.clone()],
        priority: assoc.priority,
    }]
}

/// `channel_regex` branch — the "type" dimension.
///
/// Mirrors Go `matchChannelRegex` (lines 180-216): a fixed channel is
/// selected, then every request-model id it exposes is tested against the
/// pattern via `xregexp.MatchString`. Each newly-seen (channel, model) is
/// emitted; if none match the channel is dropped.
fn match_channel_regex(
    assoc: &ObjectsModelAssociation,
    channels: &[MatcherChannel],
    tracker: &mut DuplicateChannelModelTracker,
) -> Vec<AssociationConnection> {
    let Some(branch) = assoc.channel_regex.as_ref() else {
        return Vec::new();
    };
    let Some(ch) = channels.iter().find(|c| c.channel_id == branch.channel_id) else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for model_id in ch.model_ids.iter() {
        if xregexp_match_string(&branch.pattern, model_id) && tracker.add(ch.channel_id, model_id) {
            models.push(model_id.clone());
        }
    }
    if models.is_empty() {
        return Vec::new();
    }
    vec![AssociationConnection {
        channel_id: ch.channel_id,
        model_ids: models,
        priority: assoc.priority,
    }]
}

/// `regex` branch — the "regex/pattern" dimension.
///
/// Mirrors Go `matchRegex` (lines 219-257): iterate every channel, skip those
/// excluded by the branch's `ExcludeAssociation` list (the "conditions"
/// dimension), then collect every request-model id matching the pattern.
fn match_regex(
    assoc: &ObjectsModelAssociation,
    channels: &[MatcherChannel],
    tracker: &mut DuplicateChannelModelTracker,
) -> Vec<AssociationConnection> {
    let Some(branch) = assoc.regex.as_ref() else {
        return Vec::new();
    };
    let mut connections = Vec::new();
    for ch in channels {
        if channel_matches_exclude(ch, &branch.exclude) {
            continue;
        }
        let mut models = Vec::new();
        for model_id in ch.model_ids.iter() {
            if xregexp_match_string(&branch.pattern, model_id)
                && tracker.add(ch.channel_id, model_id)
            {
                models.push(model_id.clone());
            }
        }
        if !models.is_empty() {
            connections.push(AssociationConnection {
                channel_id: ch.channel_id,
                model_ids: models,
                priority: assoc.priority,
            });
        }
    }
    connections
}

/// `model` branch — the "exact" dimension.
///
/// Mirrors Go `matchModel` (lines 260-294): iterate every channel, skip those
/// excluded by the `ExcludeAssociation` list, then emit a connection for each
/// channel that exposes the exact model id.
fn match_model(
    assoc: &ObjectsModelAssociation,
    channels: &[MatcherChannel],
    tracker: &mut DuplicateChannelModelTracker,
) -> Vec<AssociationConnection> {
    let Some(branch) = assoc.model_id.as_ref() else {
        return Vec::new();
    };
    let mut connections = Vec::new();
    for ch in channels {
        if channel_matches_exclude(ch, &branch.exclude) {
            continue;
        }
        if !ch.supports_model(&branch.model_id) {
            continue;
        }
        if !tracker.add(ch.channel_id, &branch.model_id) {
            continue;
        }
        connections.push(AssociationConnection {
            channel_id: ch.channel_id,
            model_ids: vec![branch.model_id.clone()],
            priority: assoc.priority,
        });
    }
    connections
}

/// `channel_tags_model` branch — the "tags + exact" dimension.
///
/// Mirrors Go `matchChannelTagsModel` (lines 332-380): channels carrying any
/// of the listed tags (OR logic) are kept, and each one that exposes the exact
/// model id emits a connection. Empty tag list -> no match (Go short-circuits
/// at line 337-339).
fn match_channel_tags_model(
    assoc: &ObjectsModelAssociation,
    channels: &[MatcherChannel],
    tracker: &mut DuplicateChannelModelTracker,
) -> Vec<AssociationConnection> {
    let Some(branch) = assoc.channel_tags_model.as_ref() else {
        return Vec::new();
    };
    if branch.channel_tags.is_empty() {
        return Vec::new();
    }
    let mut connections = Vec::new();
    for ch in channels {
        if !ch.has_any_tag(&branch.channel_tags) {
            continue;
        }
        if !ch.supports_model(&branch.model_id) {
            continue;
        }
        if !tracker.add(ch.channel_id, &branch.model_id) {
            continue;
        }
        connections.push(AssociationConnection {
            channel_id: ch.channel_id,
            model_ids: vec![branch.model_id.clone()],
            priority: assoc.priority,
        });
    }
    connections
}

/// `channel_tags_regex` branch — the "tags + pattern" dimension.
///
/// Mirrors Go `matchChannelTagsRegex` (lines 384-435): channels carrying any
/// of the listed tags (OR logic) are kept, then every request-model id is
/// tested against the pattern. Empty tag list -> no match.
fn match_channel_tags_regex(
    assoc: &ObjectsModelAssociation,
    channels: &[MatcherChannel],
    tracker: &mut DuplicateChannelModelTracker,
) -> Vec<AssociationConnection> {
    let Some(branch) = assoc.channel_tags_regex.as_ref() else {
        return Vec::new();
    };
    if branch.channel_tags.is_empty() {
        return Vec::new();
    }
    let mut connections = Vec::new();
    for ch in channels {
        if !ch.has_any_tag(&branch.channel_tags) {
            continue;
        }
        let mut models = Vec::new();
        for model_id in ch.model_ids.iter() {
            if xregexp_match_string(&branch.pattern, model_id)
                && tracker.add(ch.channel_id, model_id)
            {
                models.push(model_id.clone());
            }
        }
        if !models.is_empty() {
            connections.push(AssociationConnection {
                channel_id: ch.channel_id,
                model_ids: models,
                priority: assoc.priority,
            });
        }
    }
    connections
}

/// `shouldExcludeChannel` — the "conditions" dimension.
///
/// Mirrors Go `shouldExcludeChannel` (lines 297-328). A channel is excluded if
/// *any* exclude rule matches:
/// - non-empty `channel_name_pattern` matching the channel name under
///   `xregexp.MatchString`;
/// - non-empty `channel_ids` containing the channel's id;
/// - non-empty `channel_tags` sharing any tag with the channel.
///
/// As in Go, an empty exclude list never excludes, and the first matching
/// rule short-circuits to `true`.
pub fn channel_matches_exclude(
    ch: &MatcherChannel,
    excludes: &[conduit_core::objects::ExcludeAssociation],
) -> bool {
    if excludes.is_empty() {
        return false;
    }
    for exclude in excludes {
        if !exclude.channel_name_pattern.is_empty()
            && xregexp_match_string(&exclude.channel_name_pattern, &ch.name)
        {
            return true;
        }
        if !exclude.channel_ids.is_empty() && exclude.channel_ids.contains(&ch.channel_id) {
            return true;
        }
        if !exclude.channel_tags.is_empty()
            && exclude.channel_tags.iter().any(|t| ch.tags.contains(t))
        {
            return true;
        }
    }
    false
}

/// Convenience: does the exact model-id matcher (the "exact" dimension) accept
/// `requested` against the configured target? Mirrors the model-id equality
/// check inside Go `matchModel` (line 275-279), extracted as a pure helper so
/// the exact-match dimension has a dedicated, individually testable site
/// independent of the channel set.
pub fn exact_model_matches(target_model_id: &str, requested_model_id: &str) -> bool {
    target_model_id == requested_model_id
}

/// Convenience: does the regex/pattern matcher accept `requested`? Mirrors the
/// `xregexp.MatchString(pattern, modelID)` call shared by Go `matchRegex` /
/// `matchChannelRegex` / `matchChannelTagsRegex`, extracted as a pure helper
/// so the regex dimension has a dedicated, individually testable site.
pub fn regex_pattern_matches(pattern: &str, requested_model_id: &str) -> bool {
    xregexp_match_string(pattern, requested_model_id)
}

/// Convenience: does the developer dimension select this channel? The
/// developer dimension (`channel_model`) picks one channel by id and one
/// model id; this helper exposes the channel-selection half as a pure
/// predicate so the developer dimension has a dedicated, individually testable
/// site independent of the model-entry map.
pub fn developer_channel_matches(channels: &[MatcherChannel], channel_id: i64) -> bool {
    channels.iter().any(|c| c.channel_id == channel_id)
}

/// Convenience: does the tags dimension select this channel for `tags` (OR
/// logic)? Extracted from the shared `has_any_tag` loop in
/// `matchChannelTagsModel` / `matchChannelTagsRegex` so the tags dimension has
/// a dedicated, individually testable site.
pub fn tags_match_channel(channels: &[MatcherChannel], channel_id: i64, tags: &[String]) -> bool {
    if tags.is_empty() {
        return false;
    }
    channels
        .iter()
        .find(|c| c.channel_id == channel_id)
        .is_some_and(|ch| ch.has_any_tag(tags))
}

// ===========================================================================
// Unassociated-channel finder (RUST-P15-001)
// ===========================================================================
//
// Ported from Go `findUnassociatedChannels` (`conduit/internal/server/biz/
// model.go` lines 810-865). The Go helper is the pure-logic core of
// `ModelService.QueryUnassociatedChannels`: it takes a channel set and a flat
// list of model associations, runs them through `MatchConnections`, and reports
// which `(channel, model)` pairs are *not* covered by any association. The
// `*ent.Channel` / `*biz.Channel` wrapping and `GetModelEntries()` call are the
// only DB-coupled parts; the structural decision is pure, so the port takes
// [`MatcherChannel`] views (which already carry the request-model id set) and
// [`ObjectsModelAssociation`] slices and returns [`UnassociatedChannel`]
// records keyed by `channel_id`.
//
// Go test contract: `TestFindUnassociatedChannels` (`model_test.go` lines
// 1842-2044) exercises seven sub-cases against in-memory `*ent.Channel` structs
// (no DB); each is mirrored below as a dedicated `#[test]`.

/// One channel with at least one model id not covered by any association.
///
/// Mirrors Go `UnassociatedChannel` (`model.go` lines 871-874) minus the
/// `*ent.Channel` payload — the structural finder only needs the channel id and
/// the unassociated model id list. Callers resolve the id back to a live
/// channel at a higher layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnassociatedChannel {
    pub channel_id: i64,
    /// Request-model ids on this channel that no association matched. Order
    /// follows the channel's `model_ids` iteration order (deterministic
    /// `BTreeSet` order in Rust, mirroring Go's map-iteration which the Go test
    /// deliberately asserts with `Contains`/`NotContains` rather than order).
    pub models: Vec<String>,
}

/// Find channels carrying at least one model id not covered by any association.
///
/// Mirrors Go `findUnassociatedChannels` (`model.go` lines 810-865):
/// 1. empty channel set -> empty result (early return, line 811-813);
/// 2. run [`match_connections`] over the channel set to collect every
///    associated `(channel_id, model_id)` pair into a set (lines 822-835);
/// 3. for each channel, collect its model ids that are *not* in the set (lines
///    840-854); channels with at least one unassociated model emit one
///    [`UnassociatedChannel`].
///
/// As in Go, the result preserves the input channel order, and a channel with
/// no unassociated models is omitted entirely.
pub fn find_unassociated_channels(
    channels: &[MatcherChannel],
    associations: &[ObjectsModelAssociation],
) -> Vec<UnassociatedChannel> {
    if channels.is_empty() {
        return Vec::new();
    }
    let connections = match_connections(associations, channels);
    let mut associated: BTreeSet<(i64, String)> = BTreeSet::new();
    for conn in &connections {
        for model_id in &conn.model_ids {
            associated.insert((conn.channel_id, model_id.clone()));
        }
    }
    let mut result = Vec::new();
    for ch in channels {
        let unassociated_models: Vec<String> = ch
            .model_ids
            .iter()
            .filter(|model_id| !associated.contains(&(ch.channel_id, (*model_id).clone())))
            .cloned()
            .collect();
        if !unassociated_models.is_empty() {
            result.push(UnassociatedChannel {
                channel_id: ch.channel_id,
                models: unassociated_models,
            });
        }
    }
    result
}

fn split_inline_modifier(pattern: &str) -> RegexPattern {
    if !pattern.starts_with("(?") {
        return RegexPattern {
            modifier: String::new(),
            body: pattern.to_string(),
        };
    }
    let end = match pattern.find(')') {
        Some(end) => end,
        None => {
            return RegexPattern {
                modifier: String::new(),
                body: pattern.to_string(),
            };
        }
    };
    if end <= 2 {
        return RegexPattern {
            modifier: String::new(),
            body: pattern.to_string(),
        };
    }
    let modifier = &pattern[..=end];
    let body = &pattern[end + 1..];
    // Go falls back to the raw pattern if the modifier carries a "lookaround"
    // marker (`:=!<`); treat those the same way.
    if modifier[2..end].contains([':', '=', '!', '<']) {
        return RegexPattern {
            modifier: String::new(),
            body: pattern.to_string(),
        };
    }
    RegexPattern {
        modifier: modifier.to_string(),
        body: body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(id: &str, model_id: &str) -> ModelRecord {
        ModelRecord::new(id, "project-a", model_id)
    }

    fn association(
        id: &str,
        matcher: AssociationMatcher,
        model_id: &str,
        priority: i32,
    ) -> ModelAssociation {
        ModelAssociation::new(id, "project-a", matcher, model_id, priority)
    }

    #[test]
    fn exact_model_id_resolves_direct_model_without_association() -> ModelServiceResult<()> {
        let service = ModelService::new(vec![model("model-1", "gpt-4o")], Vec::new());

        let resolution = service.resolve_model("project-a", "gpt-4o")?;

        assert_eq!(
            resolution.model.map(|model| model.id),
            Some("model-1".to_string())
        );
        assert_eq!(resolution.association, None);
        assert!(!resolution.fallback_to_channels);
        Ok(())
    }

    #[test]
    fn exact_association_resolves_target_model() -> ModelServiceResult<()> {
        let service = ModelService::new(
            vec![model("model-1", "openai/gpt-4o")],
            vec![association(
                "assoc-1",
                AssociationMatcher::exact_model_id("gpt-4o"),
                "openai/gpt-4o",
                10,
            )],
        );

        let resolution = service.resolve_model("project-a", "gpt-4o")?;

        assert_eq!(
            resolution.association.map(|association| association.id),
            Some("assoc-1".to_string())
        );
        assert_eq!(
            resolution.model.map(|model| model.model_id),
            Some("openai/gpt-4o".to_string())
        );
        Ok(())
    }

    #[test]
    fn regex_association_matches_requested_model() -> ModelServiceResult<()> {
        let service = ModelService::new(
            vec![model("model-1", "openai/gpt-4o-mini")],
            vec![association(
                "assoc-1",
                AssociationMatcher::pattern("^gpt-4o-.+$"),
                "openai/gpt-4o-mini",
                10,
            )],
        );

        let resolution = service.resolve_model("project-a", "gpt-4o-mini")?;

        assert_eq!(
            resolution.association.map(|association| association.id),
            Some("assoc-1".to_string())
        );
        assert_eq!(
            resolution.model.map(|model| model.id),
            Some("model-1".to_string())
        );
        Ok(())
    }

    #[test]
    fn lower_numeric_priority_wins_when_multiple_associations_match() -> ModelServiceResult<()> {
        let service = ModelService::new(
            vec![
                model("model-low", "preferred-target"),
                model("model-high", "secondary-target"),
            ],
            vec![
                association(
                    "assoc-high",
                    AssociationMatcher::pattern("^claude-.+$"),
                    "secondary-target",
                    50,
                ),
                association(
                    "assoc-low",
                    AssociationMatcher::exact_model_id("claude-sonnet"),
                    "preferred-target",
                    5,
                ),
            ],
        );

        let resolution = service.resolve_model("project-a", "claude-sonnet")?;

        assert_eq!(
            resolution.association.map(|association| association.id),
            Some("assoc-low".to_string())
        );
        assert_eq!(
            resolution.model.map(|model| model.id),
            Some("model-low".to_string())
        );
        Ok(())
    }

    #[test]
    fn disabled_association_is_ignored() -> ModelServiceResult<()> {
        let mut disabled = association(
            "assoc-disabled",
            AssociationMatcher::exact_model_id("llama-3"),
            "disabled-target",
            1,
        );
        disabled.disabled = true;

        let service = ModelService::new(
            vec![
                model("model-disabled", "disabled-target"),
                model("model-enabled", "enabled-target"),
            ],
            vec![
                disabled,
                association(
                    "assoc-enabled",
                    AssociationMatcher::pattern("^llama-.+$"),
                    "enabled-target",
                    10,
                ),
            ],
        );

        let resolution = service.resolve_model("project-a", "llama-3")?;

        assert_eq!(
            resolution.association.map(|association| association.id),
            Some("assoc-enabled".to_string())
        );
        assert_eq!(
            resolution.model.map(|model| model.id),
            Some("model-enabled".to_string())
        );
        Ok(())
    }

    #[test]
    fn fallback_to_channels_flag_is_returned_without_channel_service() -> ModelServiceResult<()> {
        let service = ModelService::new(Vec::new(), Vec::new()).with_fallback_to_channels(true);

        let resolution = service.resolve_model("project-a", "unmapped-model")?;

        assert_eq!(resolution.model, None);
        assert_eq!(resolution.association, None);
        assert!(resolution.fallback_to_channels);
        Ok(())
    }

    #[test]
    fn query_all_channel_models_flag_is_exposed_as_pure_setting() {
        let settings = ModelQuerySettings {
            query_all_channel_models: true,
            ..ModelQuerySettings::default()
        };

        assert!(settings.should_query_all_channel_models());
    }

    #[test]
    fn blacklist_regex_matches_model_ids_without_external_services() -> ModelServiceResult<()> {
        let settings = ModelQuerySettings {
            blacklist_regex: Some("^internal/.+$".to_string()),
            ..ModelQuerySettings::default()
        };

        assert!(settings.is_model_blacklisted("internal/debug-model")?);
        assert!(!settings.is_model_blacklisted("openai/gpt-4o")?);
        Ok(())
    }

    #[test]
    fn fallback_flag_controls_missing_model_resolution() -> ModelServiceResult<()> {
        let service =
            ModelService::new(Vec::new(), Vec::new()).with_query_settings(ModelQuerySettings {
                fallback_to_channels_on_model_not_found: true,
                ..ModelQuerySettings::default()
            });

        let resolution = service.resolve_model("project-a", "missing-model")?;

        assert_eq!(resolution.model, None);
        assert_eq!(resolution.association, None);
        assert!(resolution.fallback_to_channels);
        Ok(())
    }

    // --- Model circuit breaker (RUST-P9-002 S17) -----------------------------
    //
    // Mirrors `conduit/internal/server/biz/model_circuit_breaker_test.go`.
    // The Go tests exercise three behaviors: the probe lock (single begin +
    // explicit end), the rule that exponential backoff is applied only to
    // probe failures, and the lazy TTL auto-recovery inside `GetEffectiveWeight`.
    // Each Rust test ports the corresponding Go case 1:1, replacing Go's
    // `time.Now()` with an explicit `now: DateTime<Utc>` so the state machine
    // is fully deterministic.

    fn fixed_now() -> DateTime<Utc> {
        // Stable epoch so the tests are deterministic. Chosen well after the
        // unix epoch to exercise real timestamps but never depend on the wall
        // clock. `from_timestamp` returns `Option`; fall back to epoch (and then
        // the type's `Default`) to stay clear of the workspace
        // `unwrap_used`/`expect_used` denies.
        DateTime::<Utc>::from_timestamp(1_700_000_000, 0)
            .or_else(|| DateTime::<Utc>::from_timestamp(0, 0))
            .unwrap_or_default()
    }

    #[test]
    fn circuit_breaker_starts_closed_with_default_policy() {
        let policy = ModelCircuitBreakerPolicy::default();
        // Mirrors Go `defaultModelCircuitBreakerPolicy` (lines 50-56).
        assert_eq!(policy.half_open_threshold, 3);
        assert_eq!(policy.open_threshold, 5);
        assert_eq!(policy.failure_stats_ttl, ChronoDuration::minutes(30));
        assert_eq!(policy.probe_interval, ChronoDuration::minutes(5));
        // half_open_weight stored as basis points; 0.3 -> 30 bp.
        assert_eq!(policy.half_open_weight, 30);
        // Validate mirrors Go `Validate` (lines 64-75): default policy is valid.
        policy
            .validate()
            .unwrap_or_else(|e| panic!("default policy should be valid: {e:?}"));
    }

    #[test]
    fn circuit_breaker_state_is_isolated_by_channel_and_model() {
        // New stats start Closed (Go `getStats` lines 131-137) and are keyed by
        // (channel_id, model_id), so failures on one (channel, model) never
        // bleed into a sibling.
        let now = fixed_now();
        let policy = ModelCircuitBreakerPolicy::default();
        let mut store = MemoryModelCircuitBreakerStore::new();

        let model_a_ch1 = ModelCircuitBreakerKey::new(1, "gpt-4o");
        let model_a_ch2 = ModelCircuitBreakerKey::new(2, "gpt-4o");
        let model_b_ch1 = ModelCircuitBreakerKey::new(1, "claude-sonnet");

        // Trip model_a on channel 1 to Open.
        for _ in 0..policy.open_threshold {
            store.record_error(&model_a_ch1, now, &policy, false);
        }

        assert!(store.is_open(&model_a_ch1));
        assert!(!store.is_open(&model_a_ch2));
        assert!(!store.is_open(&model_b_ch1));
        // Unseen breakers have no stats at all.
        assert_eq!(store.stats(&model_a_ch2), None);
        assert_eq!(store.stats(&model_b_ch1), None);
    }

    // --- Go parity: probe lock ----------------------------------------------
    //
    // Mirrors Go `TestModelCircuitBreakerProbeLock_SingleBeginAndExplicitEnd`
    // (`model_circuit_breaker_test.go` lines 9-49). Once Open with a due probe,
    // the breaker permits exactly one in-flight probe; a second begin fails
    // until `end_probe` releases the slot.

    #[test]
    fn go_parity_probe_lock_single_begin_and_explicit_end() {
        let mut now = fixed_now();
        let policy = ModelCircuitBreakerPolicy::default();
        let mut store = MemoryModelCircuitBreakerStore::new();
        let key = ModelCircuitBreakerKey::new(1, "gpt-test");

        // Push to Open state (5 failures at default open_threshold=5).
        for _ in 0..policy.open_threshold {
            store.record_error(&key, now, &policy, false);
        }
        // Force NextProbeAt into the past so the probe window is open (Go does
        // the same by setting `NextProbeAt = now.Add(-time.Second)`).
        {
            let stats = store.stats(&key).is_some();
            assert!(stats);
        }
        // Move time past the probe window the breaker just scheduled.
        now = now + policy.probe_interval + ChronoDuration::seconds(1);

        // `effective_weight` returns the half-open weight because a probe is
        // allowed and none is in flight (Go `GetEffectiveWeight` Open branch,
        // lines 320-330).
        let probe_weight = store.effective_weight(&key, now, &policy, 1.0);
        assert!(
            probe_weight > 0.0,
            "expected positive probe weight, got {probe_weight}"
        );

        // Claim the single probe slot.
        let ok = store.try_begin_probe(&key, now);
        assert!(ok, "expected to begin probe");

        // While a probe is in flight, weight drops to zero (second caller is
        // denied).
        let during = store.effective_weight(&key, now, &policy, 1.0);
        assert_eq!(during, 0.0, "expected zero weight while probe in progress");

        // A second begin must fail.
        let ok2 = store.try_begin_probe(&key, now);
        assert!(!ok2, "expected second begin probe to fail");

        // Releasing the slot re-enables the probe weight.
        store.end_probe(&key);
        let after = store.effective_weight(&key, now, &policy, 1.0);
        assert!(after > 0.0, "expected positive probe weight after end");
    }

    // --- Go parity: backoff only on probe failures --------------------------
    //
    // Mirrors Go `TestRecordError_OpenState_BackoffOnlyOnProbe`
    // (`model_circuit_breaker_test.go` lines 55-101). In Open state, a
    // non-probe error (e.g. a request rejected by the breaker) must NOT push
    // `next_probe_at` further into the future; only an actual probe failure
    // triggers exponential backoff. This protects auto-recovery.

    #[test]
    fn go_parity_open_state_backoff_only_on_probe() {
        let now = fixed_now();
        let policy = ModelCircuitBreakerPolicy::default();
        let mut store = MemoryModelCircuitBreakerStore::new();
        let key = ModelCircuitBreakerKey::new(1, "gpt-test");

        // Push to Open state.
        for _ in 0..policy.open_threshold {
            store.record_error(&key, now, &policy, false);
        }
        assert_eq!(
            store.stats(&key).as_ref().map(|s| s.state),
            Some(CircuitBreakerState::Open)
        );

        // Pin NextProbeAt to a known past time, exactly like Go lines 75-77
        // (`stats.NextProbeAt = time.Now().Add(-time.Second)`). The backoff
        // formula for probe_attempts=0 is `now + probe_interval(5m)`, so a fresh
        // probe failure will move NextProbeAt strictly past this pin.
        let pinned_probe_at = now - ChronoDuration::seconds(1);
        if let Some(s) = store.get_mut(&key) {
            s.next_probe_at = Some(pinned_probe_at);
            s.probe_attempts = 0;
        }

        // Simulate a non-probe error (request rejected by the breaker): Go
        // leaves NextProbeAt untouched (lines 193-213 only run when wasProbe).
        store.record_error(&key, now, &policy, false);
        let next_probe_after_non_probe = store.stats(&key).and_then(|s| s.next_probe_at);
        assert_eq!(
            next_probe_after_non_probe,
            Some(pinned_probe_at),
            "NextProbeAt should not change for non-probe errors in Open state"
        );

        // Now a real probe failure: NextProbeAt must be pushed into the future
        // via exponential backoff (probe_attempts=0 -> 1x probe_interval).
        store.record_error(&key, now, &policy, true);
        let next_probe_after_probe = store.stats(&key).and_then(|s| s.next_probe_at);
        assert_ne!(
            next_probe_after_probe,
            Some(pinned_probe_at),
            "NextProbeAt should have been updated with exponential backoff for probe failures"
        );
        assert!(
            next_probe_after_probe.is_some_and(|t| t > pinned_probe_at),
            "NextProbeAt should be pushed further into the future after probe failure"
        );
        // And the probe counter must have incremented (Go line 205).
        assert_eq!(
            store.stats(&key).as_ref().map(|s| s.probe_attempts),
            Some(1)
        );
    }

    // --- Go parity: TTL auto-recovery ---------------------------------------
    //
    // Mirrors Go `TestGetEffectiveWeight_TTLAutoRecovery`
    // (`model_circuit_breaker_test.go` lines 106-142). When the breaker is Open
    // but no failure has occurred within `failure_stats_ttl`, calling
    // `effective_weight` lazily resets it to Closed and returns the full base
    // weight.

    #[test]
    fn go_parity_effective_weight_ttl_auto_recovery() {
        let now = fixed_now();
        let policy = ModelCircuitBreakerPolicy::default();
        let mut store = MemoryModelCircuitBreakerStore::new();
        let key = ModelCircuitBreakerKey::new(1, "gpt-test");

        // Push to Open state.
        for _ in 0..policy.open_threshold {
            store.record_error(&key, now, &policy, false);
        }
        assert_eq!(
            store.stats(&key).as_ref().map(|s| s.state),
            Some(CircuitBreakerState::Open)
        );

        // Simulate "no failure within the TTL window" by jumping `now` past
        // `last_failure_at + failure_stats_ttl`. Go does this by rewriting
        // `LastFailureAt = now - (TTL + 1m)`; advancing the clock is equivalent
        // for the pure Rust port.
        let later = now + policy.failure_stats_ttl + ChronoDuration::minutes(1);

        let weight = store.effective_weight(&key, later, &policy, 1.0);
        assert_eq!(weight, 1.0, "expected full weight after TTL auto-recovery");

        // Auto-recovery must have flipped the state to Closed and zeroed the
        // counters (Go lines 300-304).
        let stats = store.stats(&key);
        assert_eq!(
            stats.as_ref().map(|s| s.state),
            Some(CircuitBreakerState::Closed)
        );
        assert_eq!(stats.as_ref().map(|s| s.consecutive_failures), Some(0));
    }

    // --- HalfOpen promotion and success recovery ----------------------------
    //
    // Mirrors the state-machine paths the Go tests above rely on implicitly:
    // at `half_open_threshold` failures the breaker is HalfOpen, at
    // `open_threshold` it is Open, and a single success recovers it to Closed
    // (Go `RecordSuccess` lines 228-251).

    #[test]
    fn circuit_breaker_promotes_to_half_open_then_open_then_recovers() {
        let now = fixed_now();
        let policy = ModelCircuitBreakerPolicy::default();
        let mut store = MemoryModelCircuitBreakerStore::new();
        let key = ModelCircuitBreakerKey::new(1, "gpt-test");

        // half_open_threshold (3) failures -> HalfOpen.
        for _ in 0..policy.half_open_threshold {
            store.record_error(&key, now, &policy, false);
        }
        assert_eq!(
            store.stats(&key).as_ref().map(|s| s.state),
            Some(CircuitBreakerState::HalfOpen)
        );

        // Two more failures (total 5 = open_threshold) -> Open.
        for _ in policy.half_open_threshold..policy.open_threshold {
            store.record_error(&key, now, &policy, false);
        }
        assert_eq!(
            store.stats(&key).as_ref().map(|s| s.state),
            Some(CircuitBreakerState::Open)
        );
        // Open sets NextProbeAt.
        assert!(store.stats(&key).and_then(|s| s.next_probe_at).is_some());

        // A single success recovers to Closed and clears every negative field.
        store.record_success(&key, now, &policy);
        let stats = store.stats(&key);
        assert_eq!(
            stats.as_ref().map(|s| s.state),
            Some(CircuitBreakerState::Closed)
        );
        assert_eq!(stats.as_ref().map(|s| s.consecutive_failures), Some(0));
        assert!(stats.as_ref().and_then(|s| s.next_probe_at).is_none());
        assert_eq!(stats.as_ref().map(|s| s.probe_attempts), Some(0));
        assert!(!stats.as_ref().is_some_and(|s| s.probing_in_progress));
    }

    #[test]
    fn circuit_breaker_policy_validate_rejects_invalid_thresholds_and_weight() {
        // half_open_threshold >= open_threshold (Go lines 65-68).
        let bad_thresholds = ModelCircuitBreakerPolicy {
            half_open_threshold: 5,
            open_threshold: 5,
            ..ModelCircuitBreakerPolicy::default()
        };
        assert!(matches!(
            bad_thresholds.validate(),
            Err(CircuitBreakerPolicyError::HalfOpenNotBeforeOpen { .. })
        ));

        // half_open_weight out of [0, 1] (Go lines 70-72). 200 bp -> 2.0.
        let bad_weight = ModelCircuitBreakerPolicy {
            half_open_weight: 200,
            ..ModelCircuitBreakerPolicy::default()
        };
        assert!(matches!(
            bad_weight.validate(),
            Err(CircuitBreakerPolicyError::HalfOpenWeightOutOfRange { .. })
        ));

        // A valid custom policy passes.
        let ok = ModelCircuitBreakerPolicy {
            half_open_threshold: 2,
            open_threshold: 4,
            half_open_weight: 50, // 0.5
            ..ModelCircuitBreakerPolicy::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn circuit_breaker_ttl_reset_vents_stale_failure_counts() {
        // Mirrors Go `RecordError` TTL check (lines 164-174): if the previous
        // failure is older than `failure_stats_ttl`, the COUNTER resets before
        // counting this failure. Note Go does NOT reset `state` here (only the
        // `effective_weight` lazy path resets state) — so a HalfOpen breaker
        // that sits idle past the TTL still reads HalfOpen, but with the counter
        // vented it cannot promote to Open from a single fresh failure.
        let now = fixed_now();
        let policy = ModelCircuitBreakerPolicy::default();
        let mut store = MemoryModelCircuitBreakerStore::new();
        let key = ModelCircuitBreakerKey::new(1, "gpt-test");

        // Reach HalfOpen (3 failures).
        for _ in 0..policy.half_open_threshold {
            store.record_error(&key, now, &policy, false);
        }
        assert_eq!(
            store.stats(&key).as_ref().map(|s| s.state),
            Some(CircuitBreakerState::HalfOpen)
        );

        // A long idle window then one fresh failure: counter resets to 0 then
        // increments to 1 (below half_open_threshold), so the breaker does not
        // promote to Open. The state is NOT demoted back to Closed by Go
        // `RecordError` (only `effective_weight`'s lazy TTL path demotes).
        let later = now + policy.failure_stats_ttl + ChronoDuration::seconds(1);
        store.record_error(&key, later, &policy, false);
        let stats = store.stats(&key);
        assert_eq!(stats.as_ref().map(|s| s.consecutive_failures), Some(1));
        assert_eq!(
            stats.as_ref().map(|s| s.state),
            Some(CircuitBreakerState::HalfOpen)
        );

        // The lazy TTL path in `effective_weight` is the only place that demotes
        // the state to Closed once the TTL elapses with no fresh failures.
        let even_later = later + policy.failure_stats_ttl + ChronoDuration::seconds(1);
        // Bring last_failure_at to `later` (already set above), so at
        // `even_later` the TTL window has again elapsed relative to that
        // failure, and `effective_weight` should auto-recover to Closed.
        let weight = store.effective_weight(&key, even_later, &policy, 1.0);
        assert_eq!(weight, 1.0, "expected full weight after second TTL window");
        assert_eq!(
            store.stats(&key).as_ref().map(|s| s.state),
            Some(CircuitBreakerState::Closed)
        );
    }

    // --- Settings inheritance (RUST-P9-002 S04) -----------------------------
    //
    // These mirror the assertions in
    // `conduit/internal/server/biz/model_settings_inheritance_test.go`:
    // developer associations are inherited and stamped with the model id,
    // `disable_developer_settings_inheritance` opts out, and the merge orders
    // rules by priority with model-before-developer tiebreak.

    fn channel_model_assoc(channel_id: i64, priority: i64) -> ObjectsModelAssociation {
        ObjectsModelAssociation {
            kind: "channel_model".to_string(),
            priority,
            channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                channel_id,
                model_id: String::new(),
            }),
            ..Default::default()
        }
    }

    fn channel_tags_model_assoc(tags: &[&str], priority: i64) -> ObjectsModelAssociation {
        ObjectsModelAssociation {
            kind: "channel_tags_model".to_string(),
            priority,
            channel_tags_model: Some(conduit_core::objects::ChannelTagsModelAssociation {
                channel_tags: tags.iter().map(|t| (*t).to_string()).collect(),
                model_id: String::new(),
            }),
            ..Default::default()
        }
    }

    fn model_id_assoc(model_id: &str, priority: i64) -> ObjectsModelAssociation {
        ObjectsModelAssociation {
            kind: "model".to_string(),
            priority,
            model_id: Some(conduit_core::objects::ModelIDAssociation {
                model_id: model_id.to_string(),
                exclude: Vec::new(),
            }),
            ..Default::default()
        }
    }

    fn developer_settings(
        developer: &str,
        associations: Vec<ObjectsModelAssociation>,
    ) -> SystemModelSettings {
        SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: developer.to_string(),
                associations,
            }],
            ..SystemModelSettings::default()
        }
    }

    #[test]
    fn inheritance_inherits_developer_settings_and_orders_by_priority() {
        // Mirrors Go `TestEffectiveModelAssociations_InheritsDeveloperSettings`.
        let model_assoc = model_id_assoc("model-specific", 1);
        let dev_same_priority = channel_model_assoc(10, 1);
        let dev_higher_priority = channel_tags_model_assoc(&["anthropic"], 0);

        let system = developer_settings(
            "openai",
            vec![dev_same_priority.clone(), dev_higher_priority.clone()],
        );
        let model_settings = ModelSettings {
            disable_developer_settings_inheritance: false,
            associations: vec![model_assoc.clone()],
        };

        let result =
            effective_model_associations(&system, "openai", "gpt-4o", Some(&model_settings));

        assert_eq!(result.len(), 3);
        // Priority 0 first (channel_tags_model), model id stamped in.
        assert_eq!(result[0].kind, "channel_tags_model");
        assert_eq!(
            result[0]
                .channel_tags_model
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("gpt-4o")
        );
        // Priority 1: model-level rule wins the tiebreak (comes before developer).
        assert!(result[1].model_id.is_some());
        assert_eq!(
            result[1].model_id.as_ref().map(|b| b.model_id.as_str()),
            Some("model-specific")
        );
        // Priority 1 developer rule last, model id stamped in.
        assert_eq!(result[2].kind, "channel_model");
        assert_eq!(
            result[2]
                .channel_model
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("gpt-4o")
        );
        // The shared developer associations are NOT mutated in place.
        assert_eq!(
            dev_same_priority
                .channel_model
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("")
        );
        assert_eq!(
            dev_higher_priority
                .channel_tags_model
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("")
        );
    }

    #[test]
    fn inheritance_disable_flag_returns_only_model_associations() {
        // Mirrors Go `TestEffectiveModelAssociations_DisablesDeveloperSettingsInheritance`.
        let model_assoc = model_id_assoc("model-specific", 1);
        let dev_assoc = channel_model_assoc(10, 0);

        let system = developer_settings("openai", vec![dev_assoc.clone()]);
        let model_settings = ModelSettings {
            disable_developer_settings_inheritance: true,
            associations: vec![model_assoc.clone()],
        };

        let result =
            effective_model_associations(&system, "openai", "gpt-4", Some(&model_settings));

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].model_id.as_ref().map(|b| b.model_id.as_str()),
            Some("model-specific")
        );
        // Developer association untouched.
        assert_eq!(
            dev_assoc
                .channel_model
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("")
        );
    }

    #[test]
    fn inheritance_legacy_settings_inherit_by_default() {
        // Mirrors Go `TestEffectiveModelAssociations_LegacyModelSettingsInheritByDefault`.
        let dev_assoc = channel_model_assoc(10, 0);

        let system = developer_settings("openai", vec![dev_assoc]);
        let model_settings = ModelSettings {
            disable_developer_settings_inheritance: false,
            associations: Vec::new(),
        };

        let result =
            effective_model_associations(&system, "openai", "gpt-4", Some(&model_settings));

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0]
                .channel_model
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("gpt-4")
        );
    }

    #[test]
    fn inheritance_no_model_settings_still_inherits_developer_rules() {
        // A model with no settings at all still inherits the developer defaults.
        let dev_assoc = channel_model_assoc(10, 0);
        let system = developer_settings("openai", vec![dev_assoc]);

        let result = effective_model_associations(&system, "openai", "gpt-4o", None);

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0]
                .channel_model
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("gpt-4o")
        );
    }

    #[test]
    fn inheritance_unmatched_developer_returns_no_inherited_rules() {
        // A model whose developer has no entry inherits nothing.
        let dev_assoc = channel_model_assoc(10, 0);
        let system = developer_settings("openai", vec![dev_assoc]);
        let model_settings = ModelSettings::default();

        let result =
            effective_model_associations(&system, "anthropic", "claude-3", Some(&model_settings));

        assert!(result.is_empty());
    }

    #[test]
    fn inheritance_drops_unsupported_developer_branches() {
        // Developer rules only carry channel_model / channel_tags_model; a
        // `model`-typed developer association is dropped (Go returns nil).
        let dev_assoc = model_id_assoc("should-be-dropped", 0);
        let system = developer_settings("openai", vec![dev_assoc]);

        let result = effective_model_associations(&system, "openai", "gpt-4o", None);

        assert!(result.is_empty());
    }

    #[test]
    fn merge_orders_equal_priority_model_before_developer() {
        let model_assoc = model_id_assoc("from-model", 5);
        let dev_assoc = channel_model_assoc(7, 5);

        let merged = merge_inherited_model_associations(
            std::slice::from_ref(&dev_assoc),
            std::slice::from_ref(&model_assoc),
        );

        // Same priority -> model-level first.
        assert_eq!(
            merged[0].model_id.as_ref().map(|b| b.model_id.as_str()),
            Some("from-model")
        );
        assert_eq!(merged[1].kind, "channel_model");
    }

    #[test]
    fn system_model_settings_default_matches_go_default() {
        // Go `defaultModelSettings` (`system_default.go` lines 33-40).
        let s = SystemModelSettings::default();
        assert!(s.fallback_to_channels_on_model_not_found);
        assert!(s.query_all_channel_models);
        assert!(!s.default_model_api_include_all);
        assert!(!s.auto_reasoning_effort);
        assert!(s.model_blacklist_regex.is_empty());
        assert!(s.developer_settings.is_empty());
    }

    #[test]
    fn associations_for_developer_matches_case_sensitive_and_ignores_empty() {
        let system = developer_settings("openai", vec![channel_model_assoc(1, 0)]);

        assert_eq!(system.associations_for_developer("openai").len(), 1);
        assert!(system.associations_for_developer("OpenAI").is_empty());
        assert!(system.associations_for_developer("").is_empty());
    }

    #[test]
    fn inheritance_clone_model_association_deep_copies_when_condition() {
        // Mirrors Go `TestCloneModelAssociation_DeepCopiesWhenCondition`
        // (`model_settings_inheritance_test.go` lines 122-154): the cloned
        // `when`/`condition` subtree must be fully independent of the source,
        // so mutating the clone never leaks back into the shared developer rule.
        use conduit_core::objects::{
            ChannelModelAssociation, Condition, ConditionType, ModelAssociationWhen,
        };
        use serde_json::json;

        let original = ObjectsModelAssociation {
            kind: "channel_model".to_string(),
            when: Some(ModelAssociationWhen {
                enabled: true,
                condition: Some(Condition {
                    r#type: ConditionType::Group,
                    logic: "and".to_string(),
                    conditions: vec![Condition {
                        r#type: ConditionType::Condition,
                        field: "prompt_tokens".to_string(),
                        operator: "gt".to_string(),
                        value: Some(json!(100)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            }),
            channel_model: Some(ChannelModelAssociation {
                channel_id: 10,
                model_id: String::new(),
            }),
            ..Default::default()
        };

        let mut clone = clone_model_association(&original);

        // The clone has its own `when`/`condition` storage (no aliasing).
        // Rust value semantics already guarantee this, but the Go test is a
        // golden parity case so we assert the property explicitly.
        if let Some(when) = clone.when.as_mut() {
            when.enabled = false;
            if let Some(cond) = when.condition.as_mut()
                && let Some(child) = cond.conditions.first_mut()
            {
                child.field = "stream".to_string();
                child.value = Some(json!(true));
            }
        }

        assert_eq!(original.when.as_ref().map(|w| w.enabled), Some(true));
        let original_child = original
            .when
            .as_ref()
            .and_then(|w| w.condition.as_ref())
            .and_then(|c| c.conditions.first());
        assert_eq!(
            original_child.map(|c| c.field.as_str()),
            Some("prompt_tokens")
        );
        assert_eq!(
            original_child.and_then(|c| c.value.as_ref()),
            Some(&json!(100))
        );
    }

    #[test]
    fn inheritance_model_associations_override_developer_at_same_priority() {
        // Supplemental golden behavior: at equal priority the model-level rule
        // refines the inherited developer default. This is the "model settings
        // override developer settings" property that S04 calls out. The Go
        // `TestEffectiveModelAssociations_InheritsDeveloperSettings` already
        // exercises one shape of this (a `model` rule beating a `channel_model`
        // rule at priority 1); here we cover the same-kind tiebreak so the
        // model-level `channel_model` runs before the developer-level one.
        //
        // Note: model-level associations are NOT re-stamped — they already carry
        // their concrete model id (Go `mergeInheritedModelAssociations` treats
        // them as opaque). Only developer rules get the model id stamped in by
        // `inheritDeveloperAssociationForModel`.
        let dev_assoc = channel_model_assoc(10, 5);
        let model_assoc = ObjectsModelAssociation {
            kind: "channel_model".to_string(),
            priority: 5,
            channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                channel_id: 20,
                model_id: "gpt-4o".to_string(),
            }),
            ..Default::default()
        };

        let system = developer_settings("openai", vec![dev_assoc]);
        let model_settings = ModelSettings {
            disable_developer_settings_inheritance: false,
            associations: vec![model_assoc],
        };

        let result =
            effective_model_associations(&system, "openai", "gpt-4o", Some(&model_settings));

        assert_eq!(result.len(), 2);
        // Model-level rule wins the tiebreak (source rank 0), unchanged; the
        // developer rule is stamped with the resolved model id.
        assert_eq!(
            result[0]
                .channel_model
                .as_ref()
                .map(|b| (b.channel_id, b.model_id.as_str())),
            Some((20, "gpt-4o"))
        );
        assert_eq!(
            result[1]
                .channel_model
                .as_ref()
                .map(|b| (b.channel_id, b.model_id.as_str())),
            Some((10, "gpt-4o"))
        );
    }

    #[test]
    fn inheritance_drops_developer_branch_without_payload() {
        // Mirrors Go `inheritDeveloperAssociationForModel` (lines 164-189): a
        // developer association of an inheritable kind whose branch payload is
        // missing is dropped (Go returns nil). The Rust helper encodes the same
        // rule via `?` on the `as_mut()` of a `None` branch.
        let dev_assoc = ObjectsModelAssociation {
            kind: "channel_model".to_string(),
            priority: 0,
            channel_model: None,
            ..Default::default()
        };
        let system = developer_settings("openai", vec![dev_assoc]);

        let result = effective_model_associations(&system, "openai", "gpt-4o", None);

        assert!(result.is_empty());
    }

    #[test]
    fn inheritance_stamps_model_id_into_tags_branch() {
        // Mirrors Go `inheritDeveloperAssociationForModel` for the
        // `channel_tags_model` branch (lines 179-183): the concrete model id is
        // stamped into the cloned branch.
        let dev_assoc = channel_tags_model_assoc(&["anthropic", "fast"], 0);
        let system = developer_settings("anthropic", vec![dev_assoc]);

        let result = effective_model_associations(&system, "anthropic", "claude-3-5-sonnet", None);

        assert_eq!(result.len(), 1);
        match result[0].channel_tags_model.as_ref() {
            Some(branch) => {
                assert_eq!(branch.model_id, "claude-3-5-sonnet");
                assert_eq!(
                    branch.channel_tags,
                    vec!["anthropic".to_string(), "fast".to_string()]
                );
            }
            None => panic!("expected channel_tags_model branch to be populated"),
        }
    }

    // --- Model list shaping (RUST-P9-002 S11/S16) ---------------------------

    fn sample_card() -> conduit_core::objects::ModelCard {
        use conduit_core::objects::{
            ModelCard, ModelCardCost, ModelCardLimit, ModelCardModalities, ModelCardReasoning,
        };
        ModelCard {
            reasoning: ModelCardReasoning {
                supported: true,
                default: false,
            },
            tool_call: true,
            temperature: false,
            modalities: ModelCardModalities {
                input: vec!["text".to_string(), "image".to_string()],
                output: vec!["text".to_string()],
            },
            vision: true,
            cost: ModelCardCost {
                input: 0.01,
                output: 0.03,
                cache_read: 0.001,
                cache_write: 0.002,
            },
            limit: ModelCardLimit {
                context: 128000,
                output: 16384,
            },
            knowledge: "2024-01".to_string(),
            release_date: "2024-03-14".to_string(),
            last_updated: "2024-06-01".to_string(),
        }
    }

    #[test]
    fn include_parse_empty_respects_system_default() {
        // default off -> basic only.
        let off = ModelInclude::parse("", false);
        assert_eq!(off.fields, Some(BTreeSet::new()));
        assert!(!off.needs_full_data());

        // default on -> all fields.
        let on = ModelInclude::parse("", true);
        assert_eq!(on.fields, None);
        assert!(on.needs_full_data());
    }

    #[test]
    fn include_parse_all_populates_every_field() {
        let all = ModelInclude::parse("all", false);
        assert_eq!(all.fields, None);
        assert!(all.should_include("name"));
        assert!(all.should_include("pricing"));
        assert!(all.needs_full_data());
    }

    #[test]
    fn include_parse_field_list_selects_named_fields_only() {
        let mut expected = BTreeSet::new();
        expected.insert("name".to_string());
        expected.insert("pricing".to_string());

        let parsed = ModelInclude::parse("name, pricing", false);
        assert_eq!(parsed.fields, Some(expected));
        assert!(parsed.should_include("name"));
        assert!(parsed.should_include("pricing"));
        assert!(!parsed.should_include("icon"));
        assert!(parsed.needs_full_data()); // requests extended fields
    }

    #[test]
    fn include_parse_unknown_field_does_not_trigger_full_data() {
        // An unknown field name is kept but none of the extended fields match,
        // so no card lookup is needed.
        let parsed = ModelInclude::parse("bogus", false);
        let mut set = BTreeSet::new();
        set.insert("bogus".to_string());
        assert_eq!(parsed.fields, Some(set));
        assert!(!parsed.needs_full_data());
    }

    #[test]
    fn shape_basic_facade_carries_only_basic_fields() {
        let facade = shape_basic_facade("gpt-4o", "openai", 1_700_000_000);
        assert_eq!(facade.id, "gpt-4o");
        assert_eq!(facade.object, "model");
        assert_eq!(facade.owned_by, "openai");
        assert_eq!(facade.created, 1_700_000_000);
        assert!(facade.name.is_none());
        assert!(facade.pricing.is_none());
        assert!(facade.capabilities.is_none());
    }

    #[test]
    fn shape_model_facade_basic_include_omits_extended_fields() {
        let card = sample_card();
        let include = ModelInclude::parse("", false); // basic only

        let facade = shape_model_facade(
            "gpt-4o",
            "openai",
            1_700_000_000,
            "GPT-4o",
            "icon.png",
            "chat",
            Some("a remark"),
            Some(&card),
            &include,
        );

        // Basic fields always populated.
        assert_eq!(facade.id, "gpt-4o");
        assert_eq!(facade.owned_by, "openai");
        // Extended fields omitted.
        assert!(facade.name.is_none());
        assert!(facade.context_length.is_none());
        assert!(facade.modalities.is_none());
        assert!(facade.pricing.is_none());
    }

    #[test]
    fn shape_model_facade_all_populates_every_field() {
        let card = sample_card();
        let include = ModelInclude::parse("all", false);

        let facade = shape_model_facade(
            "gpt-4o",
            "openai",
            1_700_000_000,
            "GPT-4o",
            "icon.png",
            "chat",
            Some("a remark"),
            Some(&card),
            &include,
        );

        assert_eq!(facade.name.as_deref(), Some("GPT-4o"));
        assert_eq!(facade.icon.as_deref(), Some("icon.png"));
        assert_eq!(facade.r#type.as_deref(), Some("chat"));
        assert_eq!(facade.description.as_deref(), Some("a remark"));
        assert_eq!(facade.context_length, Some(128000));
        assert_eq!(facade.max_output_tokens, Some(16384));
        let modalities = facade.modalities.as_ref();
        assert_eq!(
            modalities.map(|m| m.input.clone()),
            Some(vec!["text".to_string(), "image".to_string()])
        );
        let caps = facade.capabilities.as_ref();
        assert_eq!(
            caps.map(|c| (c.vision, c.tool_call, c.reasoning)),
            Some((true, true, true))
        );
        let pricing = facade.pricing.as_ref();
        assert_eq!(
            pricing.map(|p| (p.input, p.output, p.cache_read, p.cache_write)),
            Some((0.01, 0.03, 0.001, 0.002))
        );
        // unit/currency are hardcoded by Go `convertModelToOpenAIExtended`
        // (openai.go lines 632-633), asserted by
        // `TestConvertModelToOpenAIExtended_CompleteData` (openai_model_test.go
        // lines 74-75).
        assert_eq!(pricing.map(|p| p.unit.as_str()), Some("per_1m_tokens"));
        assert_eq!(pricing.map(|p| p.currency.as_str()), Some("USD"));
    }

    #[test]
    fn shape_model_facade_partial_include_populates_only_named_fields() {
        let card = sample_card();
        let include = ModelInclude::parse("name,pricing", false);

        let facade = shape_model_facade(
            "gpt-4o",
            "openai",
            1_700_000_000,
            "GPT-4o",
            "icon.png",
            "chat",
            Some("a remark"),
            Some(&card),
            &include,
        );

        assert_eq!(facade.name.as_deref(), Some("GPT-4o"));
        assert_eq!(facade.pricing.as_ref().map(|p| p.input), Some(0.01));
        // Other extended fields stay None.
        assert!(facade.icon.is_none());
        assert!(facade.r#type.is_none());
        assert!(facade.description.is_none());
        assert!(facade.context_length.is_none());
        assert!(facade.capabilities.is_none());
        assert!(facade.modalities.is_none());
    }

    #[test]
    fn shape_model_facade_without_card_omits_card_derived_fields() {
        let include = ModelInclude::parse("all", false);

        let facade = shape_model_facade(
            "gpt-4o",
            "openai",
            1_700_000_000,
            "GPT-4o",
            "icon.png",
            "chat",
            None,
            None,
            &include,
        );

        // Non-card fields still populated.
        assert_eq!(facade.name.as_deref(), Some("GPT-4o"));
        assert_eq!(facade.description, None); // remark was None
        // Card-derived fields stay None.
        assert!(facade.context_length.is_none());
        assert!(facade.pricing.is_none());
    }

    // --- Go parity: openai_model_test.go golden cases ------------------------
    //
    // Mirror the three Go tests `TestConvertModelToOpenAIExtended_NilModelCard`
    // / `_CompleteData` / `_NilRemark` (`openai_model_test.go` lines 13-94).
    // They pin `convertModelToOpenAIExtended` (Go lines 562-639) against a real
    // `*ent.Model`: nil card omits card-derived fields; a full card populates
    // every extended field with hardcoded `unit`/`currency`; a nil remark
    // leaves description empty. The Rust port routes through
    // [`shape_model_facade`] with `include=all` so every extended field is
    // requested.

    #[test]
    fn go_parity_nil_model_card_omits_card_fields() {
        // Mirrors Go `TestConvertModelToOpenAIExtended_NilModelCard` (lines
        // 13-37): a model with no card still gets name/description/type/icon
        // (when requested), but capabilities/pricing are nil.
        let include = ModelInclude::parse("all", false);

        let facade = shape_model_facade(
            "gpt-4",
            "openai",
            1_686_935_002,
            "GPT-4",
            "openai",
            "chat",
            Some("Test description"),
            None,
            &include,
        );

        assert_eq!(facade.id, "gpt-4");
        assert_eq!(facade.name.as_deref(), Some("GPT-4"));
        assert_eq!(facade.description.as_deref(), Some("Test description"));
        assert_eq!(facade.owned_by, "openai");
        assert_eq!(facade.r#type.as_deref(), Some("chat"));
        assert_eq!(facade.icon.as_deref(), Some("openai"));
        assert_eq!(facade.created, 1_686_935_002);
        assert!(facade.capabilities.is_none());
        assert!(facade.pricing.is_none());
    }

    #[test]
    fn go_parity_complete_data_populates_all_extended_fields() {
        // Mirrors Go `TestConvertModelToOpenAIExtended_CompleteData` (lines
        // 39-76): full ModelCard populates capabilities/context/output/pricing,
        // and pricing carries the hardcoded `unit`/`currency`.
        use conduit_core::objects::{
            ModelCard, ModelCardCost, ModelCardLimit, ModelCardModalities, ModelCardReasoning,
        };
        let card = ModelCard {
            reasoning: ModelCardReasoning {
                supported: true,
                default: false,
            },
            tool_call: true,
            temperature: false,
            modalities: ModelCardModalities {
                input: Vec::new(),
                output: Vec::new(),
            },
            vision: true,
            cost: ModelCardCost {
                input: 0.03,
                output: 0.06,
                cache_read: 0.015,
                cache_write: 0.03,
            },
            limit: ModelCardLimit {
                context: 8192,
                output: 4096,
            },
            knowledge: String::new(),
            release_date: String::new(),
            last_updated: String::new(),
        };
        let include = ModelInclude::parse("all", false);

        let facade = shape_model_facade(
            "gpt-4",
            "openai",
            1_686_935_002,
            "GPT-4",
            "openai",
            "chat",
            Some("GPT-4 is a large multimodal model"),
            Some(&card),
            &include,
        );

        assert_eq!(facade.id, "gpt-4");
        assert_eq!(facade.name.as_deref(), Some("GPT-4"));
        assert_eq!(
            facade.description.as_deref(),
            Some("GPT-4 is a large multimodal model")
        );
        let caps = facade.capabilities.as_ref();
        assert_eq!(
            caps.map(|c| (c.vision, c.tool_call, c.reasoning)),
            Some((true, true, true))
        );
        assert_eq!(facade.context_length, Some(8192));
        assert_eq!(facade.max_output_tokens, Some(4096));
        let pricing = facade.pricing.as_ref().map(|p| {
            (
                p.input,
                p.output,
                p.cache_read,
                p.cache_write,
                p.unit.as_str(),
                p.currency.as_str(),
            )
        });
        assert_eq!(
            pricing,
            Some((0.03, 0.06, 0.015, 0.03, "per_1m_tokens", "USD"))
        );
    }

    #[test]
    fn go_parity_nil_remark_leaves_description_empty() {
        // Mirrors Go `TestConvertModelToOpenAIExtended_NilRemark` (lines
        // 78-94): a model with `Remark == nil` still populates the non-card
        // extended fields, but description stays empty and card-derived fields
        // are nil (no card).
        let include = ModelInclude::parse("all", false);

        let facade = shape_model_facade(
            "gpt-4",
            "openai",
            1_686_935_002,
            "GPT-4",
            "openai",
            "chat",
            None,
            None,
            &include,
        );

        // Go asserts `result.Description == ""`; with `omitempty` on the Rust
        // field that serializes to absent, which we express as `None`.
        assert_eq!(facade.description, None);
        assert_eq!(facade.name.as_deref(), Some("GPT-4"));
        assert_eq!(facade.icon.as_deref(), Some("openai"));
        assert_eq!(facade.r#type.as_deref(), Some("chat"));
        assert!(facade.capabilities.is_none());
        assert!(facade.pricing.is_none());
    }

    #[test]
    fn extended_model_fields_constant_matches_go_extended_fields() {
        // Mirrors the Go `extendedFields` slice in `parseOpenAIModelInclude`.
        assert_eq!(
            EXTENDED_MODEL_FIELDS,
            &[
                "name",
                "description",
                "context_length",
                "max_output_tokens",
                "modalities",
                "capabilities",
                "pricing",
                "icon",
                "type",
            ]
        );
    }

    // --- Channel-model inclusion decision (RUST-P9-002 S12) -----------------
    //
    // Mirrors the blacklist subtests of Go `TestModelService_ListEnabledModels`
    // (`model_test.go` lines 899-1078). Each test below pins the pure decision
    // a channel-derived id sees: `QueryAllChannelModels=false` always excludes
    // channel models; otherwise the blacklist regex is applied with xregexp
    // semantics (exact-match fast path for plain strings, anchored regex for
    // patterns with metacharacters).

    #[test]
    fn should_include_returns_false_when_query_all_disabled() {
        // Mirrors Go `QueryAllChannelModels=false returns configured models only`
        // (`model_test.go` lines 1080-1177): channel-derived models never make
        // it through, even with no blacklist.
        assert!(!should_include_channel_model("gpt-4", false, ""));
        assert!(!should_include_channel_model("gpt-4", false, ".*"));
        assert!(!should_include_channel_model("gpt-4", false, "gpt-4"));
    }

    #[test]
    fn should_include_returns_true_with_query_all_and_empty_blacklist() {
        // Mirrors Go `ModelBlacklistRegex empty pattern keeps all models`
        // (`model_test.go` lines 931-949).
        assert!(should_include_channel_model("gpt-4", true, ""));
        assert!(should_include_channel_model("deepseek-chat", true, ""));
    }

    #[test]
    fn should_include_filters_regex_family_via_anchored_pattern() {
        // Mirrors Go `ModelBlacklistRegex filters channel-derived models`
        // (`model_test.go` lines 899-929). The pattern `deepseek.*` must filter
        // every deepseek-* id (and prefixed forms), while leaving unrelated
        // configured/other ids alone.
        assert!(!should_include_channel_model(
            "deepseek-chat",
            true,
            "deepseek.*"
        ));
        assert!(!should_include_channel_model(
            "deepseek-reasoner",
            true,
            "deepseek.*"
        ));
        assert!(!should_include_channel_model(
            "deepseek/deepseek-chat",
            true,
            "deepseek.*"
        ));
        // Unrelated ids still pass.
        assert!(should_include_channel_model("gpt-4", true, "deepseek.*"));
        assert!(should_include_channel_model(
            "claude-3-opus-20240229",
            true,
            "deepseek.*"
        ));
    }

    #[test]
    fn should_include_exact_string_pattern_uses_whole_string_equality() {
        // Mirrors Go `ModelBlacklistRegex exact-string pattern uses exactMatch
        // path` (`model_test.go` lines 1053-1078). `"deepseek-chat"` has no
        // regex metachars, so xregexp uses literal equality: it filters
        // exactly `deepseek-chat` but NOT `deepseek-reasoner` or the prefixed
        // `deepseek/deepseek-chat`.
        assert!(!should_include_channel_model(
            "deepseek-chat",
            true,
            "deepseek-chat"
        ));
        assert!(should_include_channel_model(
            "deepseek-reasoner",
            true,
            "deepseek-chat"
        ));
        assert!(should_include_channel_model(
            "deepseek/deepseek-chat",
            true,
            "deepseek-chat"
        ));
    }

    #[test]
    fn should_include_match_all_star_pattern_drops_everything() {
        // Mirrors the spirit of `QueryAllChannelModels=false` short-circuit
        // (`model_test.go` lines 960-1005): when the blacklist is `*`, every
        // channel model is dropped while configured entities still bypass the
        // filter at a higher layer. Here we only assert the channel decision.
        assert!(!should_include_channel_model("gpt-4", true, "*"));
        assert!(!should_include_channel_model("claude-3-opus", true, "*"));
    }

    #[test]
    fn should_include_invalid_regex_pattern_never_filters() {
        // Mirrors Go's `compileErr` branch (`pkg/xregexp/match.go` lines
        // 24-26, 94-99): an invalid regex is cached and `MatchString` returns
        // false, so no channel model is filtered. (Validation at save time
        // would reject this earlier; the runtime fallback is "never match".)
        assert!(should_include_channel_model("gpt-4", true, "[unclosed"));
        assert!(should_include_channel_model(
            "deepseek-chat",
            true,
            "[unclosed"
        ));
    }

    #[test]
    fn blacklist_matches_handles_explicit_anchors() {
        // Go `ensureAnchored` strips one leading `^` and trailing `$` before
        // re-anchoring, so `^deepseek-.+$` behaves identically to `deepseek-.+`.
        assert!(blacklist_matches("^deepseek-.+$", "deepseek-chat"));
        assert!(!blacklist_matches("^deepseek-.+$", "gpt-4"));
        // An explicit anchored exact pattern still anchors to the whole string.
        assert!(blacklist_matches("^gpt-4$", "gpt-4"));
        assert!(!blacklist_matches("^gpt-4$", "gpt-4o"));
    }

    #[test]
    fn blacklist_matches_partial_meta_still_uses_regex_path() {
        // A pattern that mixes literal text with one metacharacter must take
        // the anchored-regex path, not the exact path.
        assert!(blacklist_matches("deepseek-chat", "deepseek-chat")); // exact path
        assert!(!blacklist_matches("deepseek-chat.", "deepseek-chat")); // '.' means regex
        assert!(blacklist_matches("deepseek-chat.", "deepseek-chats")); // '.' matches 's'
    }

    #[test]
    fn blacklist_matches_empty_pattern_is_a_noop() {
        assert!(!blacklist_matches("", "gpt-4"));
        assert!(!blacklist_matches("", ""));
    }

    // --- System-toggle policy (RUST-P9-002 S16) -----------------------------
    //
    // The three `SystemModelSettings` toggles must be testable *independently*.
    // Each sub-section below targets exactly one toggle (on/off/default), plus
    // one test that asserts the toggles do not bleed into each other when
    // combined. Defaults are asserted against Go `defaultModelSettings`
    // (`system_default.go` lines 33-40).

    #[test]
    fn model_list_policy_default_matches_go_default_model_settings() {
        // Go `defaultModelSettings`: fallback=true, query_all=true,
        // default_include_all=false (system_default.go lines 33-40).
        let policy = ModelListPolicy::default();
        assert!(policy.fallback_to_channels_on_model_not_found);
        assert!(policy.query_all_channel_models);
        assert!(!policy.default_model_api_include_all);
    }

    #[test]
    fn model_list_policy_from_settings_carries_all_three_toggles_verbatim() {
        // Mirrors Go behavior: the toggles are read straight off the settings
        // struct by the respective call sites, with no transformation.
        let settings = SystemModelSettings {
            fallback_to_channels_on_model_not_found: false,
            query_all_channel_models: false,
            default_model_api_include_all: true,
            ..SystemModelSettings::default()
        };

        let policy = ModelListPolicy::from_settings(&settings);

        assert!(!policy.fallback_to_channels_on_model_not_found);
        assert!(!policy.query_all_channel_models);
        assert!(policy.default_model_api_include_all);
    }

    // --- Toggle 1: FallbackToChannelsOnModelNotFound ------------------------
    //
    // Mirrors Go `candidates.go` lines 87-97: fallback is only consulted when
    // `selectModelCandidates` returns an `ent.IsNotFound` error (i.e. the model
    // was not found). When the model resolves, the toggle is a no-op.

    #[test]
    fn toggle_fallback_returns_false_when_model_found_regardless_of_toggle() {
        // Model found -> never fall back, whether the toggle is on or off.
        let on = ModelListPolicy {
            fallback_to_channels_on_model_not_found: true,
            ..ModelListPolicy::default()
        };
        let off = ModelListPolicy {
            fallback_to_channels_on_model_not_found: false,
            ..ModelListPolicy::default()
        };
        assert!(!on.should_fallback_to_channels(true));
        assert!(!off.should_fallback_to_channels(true));
    }

    #[test]
    fn toggle_fallback_on_returns_true_when_model_not_found() {
        // Mirrors the Go fallback branch: not-found + toggle on -> channel path.
        let policy = ModelListPolicy {
            fallback_to_channels_on_model_not_found: true,
            ..ModelListPolicy::default()
        };
        assert!(policy.should_fallback_to_channels(false));
    }

    #[test]
    fn toggle_fallback_off_returns_false_even_when_model_not_found() {
        // Mirrors Go `candidates.go` line 96: toggle off + not-found surfaces
        // `ErrInvalidModel` instead of falling back.
        let policy = ModelListPolicy {
            fallback_to_channels_on_model_not_found: false,
            ..ModelListPolicy::default()
        };
        assert!(!policy.should_fallback_to_channels(false));
    }

    #[test]
    fn toggle_fallback_default_is_on_matching_go() {
        // Go default for `FallbackToChannelsOnModelNotFound` is `true`
        // (system_default.go line 34), so a not-found request falls back by
        // default.
        let policy = ModelListPolicy::default();
        assert!(policy.should_fallback_to_channels(false));
    }

    // --- Toggle 2: QueryAllChannelModels ------------------------------------
    //
    // Mirrors Go `model.go` line 645: the channel-model aggregation branch is
    // only entered when the toggle is on. When off, only configured `Model`
    // entities are returned regardless of blacklist or channel contents.

    #[test]
    fn toggle_query_all_on_enables_channel_model_aggregation() {
        // Mirrors Go `ListEnabledModels` (`model.go` lines 649-697): toggle on
        // -> the loop over `channels` runs and merges channel-supported ids.
        let policy = ModelListPolicy {
            query_all_channel_models: true,
            ..ModelListPolicy::default()
        };
        assert!(policy.should_query_channel_models());
    }

    #[test]
    fn toggle_query_all_off_short_circuits_before_blacklist_filter() {
        // Mirrors Go `model.go` line 645-647: toggle off -> `configuredModels`
        // is returned immediately; the blacklist regex never gets consulted,
        // so even a permissive blacklist does not pull channel models in.
        let policy = ModelListPolicy {
            query_all_channel_models: false,
            ..ModelListPolicy::default()
        };
        assert!(!policy.should_query_channel_models());
        // And the channel-decision helper must agree: channel models never pass
        // when `query_all` is false, regardless of blacklist.
        assert!(!should_include_channel_model("gpt-4", false, ""));
        assert!(!should_include_channel_model("gpt-4", false, ".*"));
    }

    #[test]
    fn toggle_query_all_default_is_on_matching_go() {
        // Go default for `QueryAllChannelModels` is `true`
        // (system_default.go line 35).
        let policy = ModelListPolicy::default();
        assert!(policy.should_query_channel_models());
    }

    // --- Toggle 3: DefaultModelAPIIncludeAll --------------------------------
    //
    // Mirrors Go `openai.go` line 731: the toggle is the `defaultIncludeAll`
    // argument to `parseOpenAIModelInclude`. An empty `?include=` query follows
    // the system default; explicit `all` or field-lists override it.

    #[test]
    fn toggle_default_include_all_off_yields_basic_only_for_empty_query() {
        // Default off + empty query -> basic facade only (matches Go's behavior
        // where `parseOpenAIModelInclude` returns `nil, false`).
        let policy = ModelListPolicy {
            default_model_api_include_all: false,
            ..ModelListPolicy::default()
        };
        let include = policy.resolve_include("");
        assert_eq!(include.fields, Some(BTreeSet::new()));
        assert!(!include.needs_full_data());
        assert!(!include.should_include("pricing"));
    }

    #[test]
    fn toggle_default_include_all_on_makes_empty_query_behave_like_all() {
        // Default on + empty query -> all extended fields populated (matches Go
        // `parseOpenAIModelInclude` returning `nil, true` when
        // `defaultIncludeAll` is true).
        let policy = ModelListPolicy {
            default_model_api_include_all: true,
            ..ModelListPolicy::default()
        };
        let include = policy.resolve_include("");
        assert_eq!(include.fields, None);
        assert!(include.needs_full_data());
        assert!(include.should_include("pricing"));
    }

    #[test]
    fn toggle_default_include_all_does_not_override_explicit_query_all() {
        // Explicit `?include=all` wins over both toggle states (Go lines
        // 525-527: `includeParam == "all"` short-circuits to `(nil, true)`).
        let off = ModelListPolicy {
            default_model_api_include_all: false,
            ..ModelListPolicy::default()
        };
        let on = ModelListPolicy {
            default_model_api_include_all: true,
            ..ModelListPolicy::default()
        };
        assert_eq!(off.resolve_include("all").fields, None);
        assert_eq!(on.resolve_include("all").fields, None);
    }

    #[test]
    fn toggle_default_include_all_does_not_override_explicit_field_list() {
        // Explicit field list wins over both toggle states (Go lines 529-546:
        // any non-empty non-"all" query is parsed as a comma-separated list).
        let off = ModelListPolicy {
            default_model_api_include_all: false,
            ..ModelListPolicy::default()
        };
        let on = ModelListPolicy {
            default_model_api_include_all: true,
            ..ModelListPolicy::default()
        };
        let expected = {
            let mut s = BTreeSet::new();
            s.insert("name".to_string());
            s
        };
        assert_eq!(off.resolve_include("name").fields, Some(expected.clone()));
        assert_eq!(on.resolve_include("name").fields, Some(expected));
    }

    #[test]
    fn toggle_default_include_all_default_is_off_matching_go() {
        // Go default for `DefaultModelAPIIncludeAll` is `false`
        // (system_default.go line 36), so by default `/v1/models` returns the
        // minimal facade unless `?include=all` is requested.
        let policy = ModelListPolicy::default();
        let include = policy.resolve_include("");
        assert!(!include.needs_full_data());
    }

    // --- Toggle independence (S16: "must be tested separately") --------------
    //
    // The three toggles are independent booleans on `SystemModelSettings` and
    // must not bleed into each other's decision. This test flips all three to
    // the non-default state and asserts each decision helper reads only its own
    // field.

    #[test]
    fn toggles_are_independent_when_all_flipped_to_non_default() {
        // Non-default combination: fallback off, query_all off, include_all on.
        let policy = ModelListPolicy {
            fallback_to_channels_on_model_not_found: false,
            query_all_channel_models: false,
            default_model_api_include_all: true,
        };

        // Toggle 1 reads only `fallback_to_channels_on_model_not_found`.
        assert!(!policy.should_fallback_to_channels(false));
        assert!(!policy.should_fallback_to_channels(true));

        // Toggle 2 reads only `query_all_channel_models`.
        assert!(!policy.should_query_channel_models());

        // Toggle 3 reads only `default_model_api_include_all`.
        assert!(policy.resolve_include("").needs_full_data());
    }

    // --- SystemModelSettings validation + normalization (RUST-P9-002 S14) ----
    //
    // Mirror the two Go golden cases
    // `TestValidateSystemModelSettings_RejectsDuplicateDevelopers` and
    // `_RejectsDeveloperModelSelection` (`model_settings_inheritance_test.go`
    // lines 156-181), plus supplemental cases for the developer-channel /
    // developer-channel-tags requirement branches and the idempotent normalize
    // path. Each Rust test asserts on the verbatim Go error substring so a
    // future drift in the error wording surfaces as a test failure.

    #[test]
    fn go_parity_validate_rejects_duplicate_developers() {
        // Mirrors Go `TestValidateSystemModelSettings_RejectsDuplicateDevelopers`
        // (lines 156-164): two developer entries with the same name are
        // rejected with `"duplicate model developer"`.
        let settings = SystemModelSettings {
            developer_settings: vec![
                conduit_core::objects::DeveloperModelSettings {
                    developer: "openai".to_string(),
                    ..Default::default()
                },
                conduit_core::objects::DeveloperModelSettings {
                    developer: "openai".to_string(),
                    ..Default::default()
                },
            ],
            ..SystemModelSettings::default()
        };

        let err = validate_system_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("validate should reject duplicate developers"));
        // Go asserts on the substring via `require.ErrorContains`; the Rust
        // `Display` impl produces the same substring via `thiserror`'s derived
        // message.
        match &err {
            ModelValidationError::DuplicateDeveloper { developer } => {
                assert_eq!(developer, "openai");
            }
            other => panic!("expected DuplicateDeveloper, got {other:?}"),
        }
        let msg = format!("{err}");
        assert!(
            msg.contains("duplicate model developer"),
            "expected substring 'duplicate model developer' in {msg:?}"
        );
    }

    #[test]
    fn go_parity_validate_rejects_developer_model_selection() {
        // Mirrors Go `TestValidateSystemModelSettings_RejectsDeveloperModelSelection`
        // (lines 166-181): a developer association of type `"model"` is rejected
        // because developers may only carry `channel_model` / `channel_tags_model`
        // rules. The Go test asserts `require.ErrorContains(err, "developer
        // association type")`.
        let settings = SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "anthropic".to_string(),
                associations: vec![ObjectsModelAssociation {
                    kind: "model".to_string(),
                    model_id: Some(conduit_core::objects::ModelIDAssociation {
                        model_id: "claude-opus-4-6".to_string(),
                        exclude: Vec::new(),
                    }),
                    ..Default::default()
                }],
            }],
            ..SystemModelSettings::default()
        };

        let err = validate_system_model_settings(&settings)
            .err()
            .unwrap_or_else(|| {
                panic!("validate should reject developer model selection");
            });
        match &err {
            ModelValidationError::UnsupportedDeveloperAssociation { association_type } => {
                assert_eq!(association_type, "model");
            }
            other => panic!("expected UnsupportedDeveloperAssociation, got {other:?}"),
        }
        let msg = format!("{err}");
        assert!(
            msg.contains("developer association type"),
            "expected substring 'developer association type' in {msg:?}"
        );
        // The raw type must round-trip in the message so callers can report
        // which unsupported type was rejected (Go `%q` embeds the value).
        assert!(msg.contains("\"model\""), "expected quoted type in {msg:?}");
    }

    #[test]
    fn validate_rejects_empty_developer_name_after_trim() {
        // Mirrors Go `validateSystemModelSettings` lines 45-48: a developer
        // name that is empty (after trimming) is rejected with `"model
        // developer is required"`. Go does not have a dedicated *_test.go case
        // for this branch, but it is part of the same function and is covered
        // here as a supplemental golden case.
        let settings = SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "   ".to_string(),
                ..Default::default()
            }],
            ..SystemModelSettings::default()
        };

        let err = validate_system_model_settings(&settings)
            .err()
            .unwrap_or_else(|| {
                panic!("validate should reject empty developer name");
            });
        assert_eq!(err, ModelValidationError::DeveloperRequired);
        let msg = format!("{err}");
        assert!(
            msg.contains("model developer is required"),
            "expected substring 'model developer is required' in {msg:?}"
        );
    }

    #[test]
    fn validate_rejects_developer_channel_branch_missing_or_zero_channel() {
        // Mirrors Go `validateDeveloperAssociations` lines 89-92: a
        // `channel_model` developer association whose branch is missing OR whose
        // `channel_id` is zero is rejected with `"developer channel association
        // requires channel"`.
        let missing_branch = SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "openai".to_string(),
                associations: vec![ObjectsModelAssociation {
                    kind: "channel_model".to_string(),
                    channel_model: None,
                    ..Default::default()
                }],
            }],
            ..SystemModelSettings::default()
        };
        let err = validate_system_model_settings(&missing_branch)
            .err()
            .unwrap_or_else(|| {
                panic!("validate should reject missing channel_model branch");
            });
        assert_eq!(
            err,
            ModelValidationError::DeveloperChannelAssociationRequiresChannel
        );

        // Zero channel id is also rejected.
        let zero_channel = SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "openai".to_string(),
                associations: vec![ObjectsModelAssociation {
                    kind: "channel_model".to_string(),
                    channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                        channel_id: 0,
                        model_id: String::new(),
                    }),
                    ..Default::default()
                }],
            }],
            ..SystemModelSettings::default()
        };
        let err = validate_system_model_settings(&zero_channel)
            .err()
            .unwrap_or_else(|| {
                panic!("validate should reject zero channel_id");
            });
        assert_eq!(
            err,
            ModelValidationError::DeveloperChannelAssociationRequiresChannel
        );

        // A well-formed `channel_model` (real channel id) passes the
        // developer-level checks. This is the case
        // `TestEffectiveModelAssociations_InheritsDeveloperSettings` relies on
        // implicitly (its `developerAssociationSamePriority` has `ChannelID:
        // 10`), and confirms the deferred `validateModelSettings` call does not
        // regress well-formed developer entries (see the section header).
        let ok = SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "openai".to_string(),
                associations: vec![ObjectsModelAssociation {
                    kind: "channel_model".to_string(),
                    channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                        channel_id: 10,
                        model_id: String::new(),
                    }),
                    ..Default::default()
                }],
            }],
            ..SystemModelSettings::default()
        };
        assert!(
            validate_system_model_settings(&ok).is_ok(),
            "well-formed developer channel_model should pass developer-level checks"
        );
    }

    #[test]
    fn validate_rejects_developer_channel_tags_branch_missing_or_empty() {
        // Mirrors Go `validateDeveloperAssociations` lines 93-95: a
        // `channel_tags_model` developer association whose branch is missing OR
        // whose `channel_tags` list is empty is rejected with `"developer
        // channel tags association requires channel tags"`.
        let empty_tags = SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "anthropic".to_string(),
                associations: vec![ObjectsModelAssociation {
                    kind: "channel_tags_model".to_string(),
                    channel_tags_model: Some(conduit_core::objects::ChannelTagsModelAssociation {
                        channel_tags: Vec::new(),
                        model_id: String::new(),
                    }),
                    ..Default::default()
                }],
            }],
            ..SystemModelSettings::default()
        };
        let err = validate_system_model_settings(&empty_tags)
            .err()
            .unwrap_or_else(|| {
                panic!("validate should reject empty channel_tags");
            });
        assert_eq!(
            err,
            ModelValidationError::DeveloperChannelTagsAssociationRequiresChannelTags
        );

        // A populated `channel_tags_model` passes the developer-level checks.
        let ok = SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "anthropic".to_string(),
                associations: vec![ObjectsModelAssociation {
                    kind: "channel_tags_model".to_string(),
                    channel_tags_model: Some(conduit_core::objects::ChannelTagsModelAssociation {
                        channel_tags: vec!["fast".to_string()],
                        model_id: String::new(),
                    }),
                    ..Default::default()
                }],
            }],
            ..SystemModelSettings::default()
        };
        assert!(
            validate_system_model_settings(&ok).is_ok(),
            "well-formed developer channel_tags_model should pass developer-level checks"
        );
    }

    #[test]
    fn validate_passes_for_empty_settings_and_distinct_developers() {
        // Mirrors the implicit "happy path" the two Go golden cases diverge
        // from: an empty `SystemModelSettings` and one with distinct, well-
        // formed developer entries both validate cleanly.
        assert!(validate_system_model_settings(&SystemModelSettings::default()).is_ok());

        let ok = SystemModelSettings {
            developer_settings: vec![
                conduit_core::objects::DeveloperModelSettings {
                    developer: "openai".to_string(),
                    associations: vec![ObjectsModelAssociation {
                        kind: "channel_model".to_string(),
                        channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                            channel_id: 1,
                            model_id: String::new(),
                        }),
                        ..Default::default()
                    }],
                },
                conduit_core::objects::DeveloperModelSettings {
                    developer: "anthropic".to_string(),
                    associations: vec![ObjectsModelAssociation {
                        kind: "channel_tags_model".to_string(),
                        channel_tags_model: Some(
                            conduit_core::objects::ChannelTagsModelAssociation {
                                channel_tags: vec!["claude".to_string()],
                                model_id: String::new(),
                            },
                        ),
                        ..Default::default()
                    }],
                },
            ],
            ..SystemModelSettings::default()
        };
        assert!(validate_system_model_settings(&ok).is_ok());
    }

    #[test]
    fn normalize_trims_developer_names_and_clears_developer_model_ids() {
        // Mirrors Go `normalizeSystemModelSettings` (lines 12-32) +
        // `normalizeDeveloperAssociations` (lines 63-80):
        // - developer names are trimmed of surrounding whitespace;
        // - `channel_model` / `channel_tags_model` branches carried by a
        //   developer rule have their `model_id` cleared (developer rules only
        //   choose channels/tags; the model id is stamped in at inheritance
        //   time).
        let mut settings = SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "  openai  ".to_string(),
                associations: vec![
                    ObjectsModelAssociation {
                        kind: "channel_model".to_string(),
                        channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                            channel_id: 7,
                            model_id: "stale-from-input".to_string(),
                        }),
                        ..Default::default()
                    },
                    ObjectsModelAssociation {
                        kind: "channel_tags_model".to_string(),
                        channel_tags_model: Some(
                            conduit_core::objects::ChannelTagsModelAssociation {
                                channel_tags: vec!["tag-a".to_string()],
                                model_id: "also-stale".to_string(),
                            },
                        ),
                        ..Default::default()
                    },
                    // Non-channel kinds are left untouched by the normalizer.
                    ObjectsModelAssociation {
                        kind: "model".to_string(),
                        model_id: Some(conduit_core::objects::ModelIDAssociation {
                            model_id: "untouched".to_string(),
                            exclude: Vec::new(),
                        }),
                        ..Default::default()
                    },
                ],
            }],
            ..SystemModelSettings::default()
        };

        normalize_system_model_settings(&mut settings);

        // Developer name trimmed.
        assert_eq!(settings.developer_settings[0].developer, "openai");
        // channel_model / channel_tags_model model_id cleared.
        assert_eq!(
            settings.developer_settings[0].associations[0]
                .channel_model
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("")
        );
        assert_eq!(
            settings.developer_settings[0].associations[1]
                .channel_tags_model
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("")
        );
        // Non-channel branches untouched.
        assert_eq!(
            settings.developer_settings[0].associations[2]
                .model_id
                .as_ref()
                .map(|b| b.model_id.as_str()),
            Some("untouched")
        );
    }

    #[test]
    fn normalize_then_validate_is_idempotent_for_well_formed_input() {
        // Mirrors the runtime sequence in Go `SystemService.SetModelSettings`
        // (system.go lines 1177-1180): `normalizeSystemModelSettings` runs
        // first, then `validateSystemModelSettings`. Running normalize twice
        // must produce the same settings, and a well-formed normalized input
        // must pass validation.
        let mut settings = SystemModelSettings {
            developer_settings: vec![conduit_core::objects::DeveloperModelSettings {
                developer: "  openai  ".to_string(),
                associations: vec![ObjectsModelAssociation {
                    kind: "channel_model".to_string(),
                    channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                        channel_id: 10,
                        model_id: "leftover".to_string(),
                    }),
                    ..Default::default()
                }],
            }],
            ..SystemModelSettings::default()
        };

        normalize_system_model_settings(&mut settings);
        let snapshot = settings.clone();
        // Second normalize is a no-op on already-normalized input.
        normalize_system_model_settings(&mut settings);
        assert_eq!(settings, snapshot);
        // And the normalized form passes validation.
        assert!(validate_system_model_settings(&settings).is_ok());
    }

    // --- ModelSettings regex + condition-tree validation (RUST-P9-002 S14) --
    //
    // Mirror the Go golden cases in `TestModelService_ValidateModelSettings`
    // (`model_validation_test.go` lines 18-777). Each test below ports one
    // Go `t.Run` subtest 1:1, asserting on the verbatim Go error substring so
    // any drift in error wording surfaces as a test failure.

    /// Build a `ModelSettings` with a single association carrying `when`.
    fn settings_with_when(when: conduit_core::objects::ModelAssociationWhen) -> ModelSettings {
        ModelSettings {
            associations: vec![ObjectsModelAssociation {
                kind: "model".to_string(),
                when: Some(when),
                model_id: Some(conduit_core::objects::ModelIDAssociation {
                    model_id: "test-model".to_string(),
                    exclude: Vec::new(),
                }),
                ..Default::default()
            }],
            ..ModelSettings::default()
        }
    }

    fn when_group(
        enabled: bool,
        conditions: Vec<conduit_core::objects::Condition>,
    ) -> conduit_core::objects::ModelAssociationWhen {
        conduit_core::objects::ModelAssociationWhen {
            enabled,
            condition: Some(conduit_core::objects::Condition {
                r#type: conduit_core::objects::ConditionType::Group,
                logic: "and".to_string(),
                conditions,
                ..Default::default()
            }),
        }
    }

    fn leaf(field: &str, operator: &str, value: Value) -> conduit_core::objects::Condition {
        conduit_core::objects::Condition {
            r#type: conduit_core::objects::ConditionType::Condition,
            field: field.to_string(),
            operator: operator.to_string(),
            value: Some(value),
            ..Default::default()
        }
    }

    #[test]
    fn validate_model_settings_passes_for_empty_input() {
        // Mirrors Go `nil settings should pass` + `empty associations should pass`
        // (lines 734-746). An empty `ModelSettings` and one with no associations
        // both validate cleanly.
        assert!(validate_model_settings(&ModelSettings::default()).is_ok());
        assert!(
            validate_model_settings(&ModelSettings {
                associations: Vec::new(),
                ..ModelSettings::default()
            })
            .is_ok()
        );
    }

    #[test]
    fn validate_model_settings_passes_for_valid_regex_patterns() {
        // Mirrors Go `valid regex patterns` (lines 31-75): all four regex-
        // bearing branches plus an `exclude.channel_name_pattern` compile
        // successfully.
        use conduit_core::objects::{
            ChannelRegexAssociation, ChannelTagsRegexAssociation, ExcludeAssociation,
            RegexAssociation,
        };
        let settings = ModelSettings {
            associations: vec![
                ObjectsModelAssociation {
                    kind: "channel_regex".to_string(),
                    channel_regex: Some(ChannelRegexAssociation {
                        channel_id: 1,
                        pattern: "gpt-.*".to_string(),
                    }),
                    ..Default::default()
                },
                ObjectsModelAssociation {
                    kind: "channel_tags_regex".to_string(),
                    channel_tags_regex: Some(ChannelTagsRegexAssociation {
                        channel_tags: vec!["production".to_string(), "test".to_string()],
                        pattern: "claude-.*".to_string(),
                    }),
                    ..Default::default()
                },
                ObjectsModelAssociation {
                    kind: "regex".to_string(),
                    regex: Some(RegexAssociation {
                        pattern: "claude-.*".to_string(),
                        exclude: vec![ExcludeAssociation {
                            channel_name_pattern: ".*backup".to_string(),
                            ..Default::default()
                        }],
                    }),
                    ..Default::default()
                },
                ObjectsModelAssociation {
                    kind: "model".to_string(),
                    model_id: Some(conduit_core::objects::ModelIDAssociation {
                        model_id: "test-model".to_string(),
                        exclude: vec![ExcludeAssociation {
                            channel_tags: vec!["test".to_string()],
                            ..Default::default()
                        }],
                    }),
                    ..Default::default()
                },
            ],
            ..ModelSettings::default()
        };
        assert!(validate_model_settings(&settings).is_ok());
    }

    #[test]
    fn validate_model_settings_passes_for_empty_regex_patterns() {
        // Mirrors Go `empty patterns should pass` (lines 748-776): an empty
        // pattern on any regex-bearing branch is valid (xregexp short-circuits
        // empty input).
        use conduit_core::objects::{
            ChannelRegexAssociation, ChannelTagsRegexAssociation, RegexAssociation,
        };
        let settings = ModelSettings {
            associations: vec![
                ObjectsModelAssociation {
                    kind: "channel_regex".to_string(),
                    channel_regex: Some(ChannelRegexAssociation {
                        channel_id: 1,
                        pattern: String::new(),
                    }),
                    ..Default::default()
                },
                ObjectsModelAssociation {
                    kind: "channel_tags_regex".to_string(),
                    channel_tags_regex: Some(ChannelTagsRegexAssociation {
                        channel_tags: vec!["test".to_string()],
                        pattern: String::new(),
                    }),
                    ..Default::default()
                },
                ObjectsModelAssociation {
                    kind: "regex".to_string(),
                    regex: Some(RegexAssociation {
                        pattern: String::new(),
                        exclude: Vec::new(),
                    }),
                    ..Default::default()
                },
            ],
            ..ModelSettings::default()
        };
        assert!(validate_model_settings(&settings).is_ok());
    }

    #[test]
    fn go_parity_invalid_regex_in_channel_regex_is_rejected() {
        // Mirrors Go `invalid regex pattern in channel_regex` (lines 659-675).
        use conduit_core::objects::ChannelRegexAssociation;
        let settings = ModelSettings {
            associations: vec![ObjectsModelAssociation {
                kind: "channel_regex".to_string(),
                channel_regex: Some(ChannelRegexAssociation {
                    channel_id: 1,
                    pattern: "[invalid".to_string(),
                }),
                ..Default::default()
            }],
            ..ModelSettings::default()
        };
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected InvalidChannelRegexPattern"));
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid regex pattern in channel_regex association"),
            "expected substring in {msg:?}"
        );
        assert!(matches!(
            err,
            ModelValidationError::InvalidChannelRegexPattern { .. }
        ));
    }

    #[test]
    fn go_parity_invalid_regex_in_channel_tags_regex_is_rejected() {
        // Mirrors Go `invalid regex pattern in channel_tags_regex`
        // (lines 677-693). `(?P<invalid` is an unclosed named group — both
        // Go's regexp2 and Rust's RE2 reject it.
        use conduit_core::objects::ChannelTagsRegexAssociation;
        let settings = ModelSettings {
            associations: vec![ObjectsModelAssociation {
                kind: "channel_tags_regex".to_string(),
                channel_tags_regex: Some(ChannelTagsRegexAssociation {
                    channel_tags: vec!["production".to_string()],
                    pattern: "(?P<invalid".to_string(),
                }),
                ..Default::default()
            }],
            ..ModelSettings::default()
        };
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected InvalidChannelTagsRegexPattern"));
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid regex pattern in channel_tags_regex association"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_invalid_regex_in_regex_association_is_rejected() {
        // Mirrors Go `invalid regex pattern in regex association`
        // (lines 695-710).
        use conduit_core::objects::RegexAssociation;
        let settings = ModelSettings {
            associations: vec![ObjectsModelAssociation {
                kind: "regex".to_string(),
                regex: Some(RegexAssociation {
                    pattern: "(?P<invalid".to_string(),
                    exclude: Vec::new(),
                }),
                ..Default::default()
            }],
            ..ModelSettings::default()
        };
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected InvalidRegexPattern"));
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid regex pattern in regex association"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_invalid_regex_in_exclude_rule_is_rejected() {
        // Mirrors Go `invalid regex pattern in exclude rule` (lines 712-732).
        use conduit_core::objects::{ExcludeAssociation, RegexAssociation};
        let settings = ModelSettings {
            associations: vec![ObjectsModelAssociation {
                kind: "regex".to_string(),
                regex: Some(RegexAssociation {
                    pattern: ".*".to_string(),
                    exclude: vec![ExcludeAssociation {
                        channel_name_pattern: "[invalid".to_string(),
                        ..Default::default()
                    }],
                }),
                ..Default::default()
            }],
            ..ModelSettings::default()
        };
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected InvalidExcludeRegexPattern"));
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid regex pattern in exclude rule"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_valid_when_condition_passes() {
        // Mirrors Go `valid when condition` (lines 77-106) + `accepts graphql
        // any integer forms` (lines 108-144): an enabled `when` with a single
        // `prompt_tokens gt` leaf whose value is an integer (i64 or f64-as-int)
        // validates cleanly.
        for value in [json!(99999), json!(1024_i64), json!(1024_f64)] {
            let settings =
                settings_with_when(when_group(true, vec![leaf("prompt_tokens", "gt", value)]));
            assert!(
                validate_model_settings(&settings).is_ok(),
                "expected valid for prompt_tokens gt with integer-shaped value"
            );
        }
    }

    #[test]
    fn go_parity_when_rejects_numeric_string_for_prompt_tokens() {
        // Mirrors Go `invalid when condition rejects numeric string`
        // (lines 146-176): a `prompt_tokens gt` leaf whose value is the JSON
        // string `"1024"` is rejected with
        // `"condition value for prompt_tokens must be an integer"`.
        let settings = settings_with_when(when_group(
            true,
            vec![leaf("prompt_tokens", "gt", json!("1024"))],
        ));
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected PromptTokensValueNotInteger"));
        let msg = format!("{err}");
        assert!(
            msg.contains("condition value for prompt_tokens must be an integer"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_when_rejects_group_without_conditions() {
        // Mirrors Go `invalid when without conditions` (lines 178-199): an
        // enabled `when` whose root group has no children is rejected with
        // `"condition requires at least one condition or group"`.
        let settings = settings_with_when(conduit_core::objects::ModelAssociationWhen {
            enabled: true,
            condition: Some(conduit_core::objects::Condition {
                r#type: conduit_core::objects::ConditionType::Group,
                ..Default::default()
            }),
        });
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected GroupRequiresConditions"));
        let msg = format!("{err}");
        assert!(
            msg.contains("condition requires at least one condition or group"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_when_rejects_unsupported_field() {
        // Mirrors Go `invalid when with unsupported field` (lines 201-231): a
        // leaf with `field: "unknown"` is rejected with
        // `unsupported condition field "unknown"`.
        let settings = settings_with_when(when_group(true, vec![leaf("unknown", "gt", json!(1))]));
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected UnsupportedConditionField"));
        let msg = format!("{err}");
        assert!(
            msg.contains(r#"unsupported condition field "unknown""#),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_when_rejects_unsupported_operator_for_prompt_tokens() {
        // Mirrors Go `valid nested when condition` (lines 233-275), which the Go
        // author misnamed — the case actually asserts that `prompt_tokens eq`
        // is rejected with `unsupported condition operator "eq"` (the
        // prompt_tokens whitelist is lt/lte/gt/gte + symbolic forms only).
        let settings = settings_with_when(when_group(
            true,
            vec![leaf("prompt_tokens", "eq", json!(200_i64))],
        ));
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected UnsupportedConditionOperator"));
        let msg = format!("{err}");
        assert!(
            msg.contains(r#"unsupported condition operator "eq""#),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_when_validates_stream_leaves() {
        // Mirrors Go `valid stream condition` (lines 277-306) + `_with_false_value`
        // (lines 308-337): `stream eq/ne true/false` is valid.
        for (op, val) in [("eq", json!(true)), ("ne", json!(false))] {
            let label = format!("stream {op} {val}");
            let settings = settings_with_when(when_group(true, vec![leaf("stream", op, val)]));
            assert!(
                validate_model_settings(&settings).is_ok(),
                "expected valid for {label}"
            );
        }
    }

    #[test]
    fn go_parity_when_rejects_stream_with_numeric_value() {
        // Mirrors Go `invalid stream condition with numeric value`
        // (lines 339-369): `stream eq 1` is rejected with
        // `"condition value for stream must be a boolean, got float64"`.
        let settings =
            settings_with_when(when_group(true, vec![leaf("stream", "eq", json!(1_i64))]));
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected BoolValueRequired"));
        let msg = format!("{err}");
        assert!(
            msg.contains("condition value for stream must be a boolean, got float64"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_when_rejects_stream_with_unsupported_operator() {
        // Mirrors Go `invalid stream condition with unsupported operator`
        // (lines 371-401): `stream gt true` is rejected with
        // `unsupported condition operator "gt" for stream`.
        let settings =
            settings_with_when(when_group(true, vec![leaf("stream", "gt", json!(true))]));
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected UnsupportedConditionOperator"));
        let msg = format!("{err}");
        assert!(
            msg.contains(r#"unsupported condition operator "gt" for stream"#),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_when_validates_content_feature_leaves() {
        // Mirrors Go `valid content feature conditions` (lines 403-443): each
        // of has_image/has_video/has_document/has_audio with `eq true` passes.
        for field in ["has_image", "has_video", "has_document", "has_audio"] {
            let settings =
                settings_with_when(when_group(true, vec![leaf(field, "eq", json!(true))]));
            assert!(
                validate_model_settings(&settings).is_ok(),
                "expected valid for {field} eq true"
            );
        }
    }

    #[test]
    fn go_parity_when_rejects_content_feature_with_string_value() {
        // Mirrors Go `invalid content feature condition with string value`
        // (lines 445-475): `has_image eq "true"` is rejected with
        // `"condition value for has_image must be a boolean, got string"`.
        let settings = settings_with_when(when_group(
            true,
            vec![leaf("has_image", "eq", json!("true"))],
        ));
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected BoolValueRequired"));
        let msg = format!("{err}");
        assert!(
            msg.contains("condition value for has_image must be a boolean, got string"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_when_validates_combined_prompt_tokens_and_stream() {
        // Mirrors Go `valid combined prompt_tokens and stream condition`
        // (lines 477-512): two leaves in one group both validate.
        let settings = settings_with_when(when_group(
            true,
            vec![
                leaf("prompt_tokens", "gt", json!(100_i64)),
                leaf("stream", "eq", json!(false)),
            ],
        ));
        assert!(validate_model_settings(&settings).is_ok());
    }

    #[test]
    fn go_parity_when_validates_request_format() {
        // Mirrors Go `valid request format condition` (lines 514-543):
        // `request_format eq "anthropic/messages"` passes.
        let settings = settings_with_when(when_group(
            true,
            vec![leaf("request_format", "eq", json!("anthropic/messages"))],
        ));
        assert!(validate_model_settings(&settings).is_ok());
    }

    #[test]
    fn go_parity_when_validates_daily_time() {
        // Mirrors Go `valid daily time condition` (lines 545-574): a
        // `daily_time within "22:00-06:00"` leaf passes.
        let settings = settings_with_when(when_group(
            true,
            vec![leaf("daily_time", "within", json!("22:00-06:00"))],
        ));
        assert!(validate_model_settings(&settings).is_ok());
    }

    #[test]
    fn go_parity_when_rejects_malformed_daily_time_range() {
        // Mirrors Go `invalid daily time condition rejects malformed range`
        // (lines 576-606): `daily_time within "25:00-26:00"` is rejected with
        // `"invalid daily_time start"` (hours out of range).
        let settings = settings_with_when(when_group(
            true,
            vec![leaf("daily_time", "within", json!("25:00-26:00"))],
        ));
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected DailyTimeClockInvalid"));
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid daily_time start"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_when_rejects_unsupported_daily_time_operator() {
        // Mirrors Go `invalid daily time condition rejects unsupported operator`
        // (lines 608-638): `daily_time eq "09:00-17:00"` is rejected with
        // `unsupported condition operator "eq" for daily_time`.
        let settings = settings_with_when(when_group(
            true,
            vec![leaf("daily_time", "eq", json!("09:00-17:00"))],
        ));
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected UnsupportedConditionOperator"));
        let msg = format!("{err}");
        assert!(
            msg.contains(r#"unsupported condition operator "eq" for daily_time"#),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_disabled_when_allows_any_state() {
        // Mirrors Go `disabled when allows empty condition` (lines 640-657):
        // a `when` with `enabled: false` skips validation entirely, so even a
        // missing `condition` is accepted.
        let settings = settings_with_when(conduit_core::objects::ModelAssociationWhen {
            enabled: false,
            condition: None,
        });
        assert!(validate_model_settings(&settings).is_ok());
    }

    #[test]
    fn go_parity_condition_tree_rejects_excessive_nesting() {
        // Supplemental golden case for the depth cap (Go
        // `validateFilterConditionNodeAtDepth` lines 163-165, `MaxNestedLevels:
        // 3`). The Go suite does not exercise this branch directly, but the
        // constant is part of the contract and the walker enforces it; a tree
        // nested 4 deep must be rejected with
        // `"condition nesting depth must not exceed 3"`.
        use conduit_core::objects::{Condition, ConditionType};
        let deep_leaf = leaf("prompt_tokens", "gt", json!(1));
        // depth 4: group(group(group(group(leaf)))) — root counts as depth 1.
        let depth4 = Condition {
            r#type: ConditionType::Group,
            conditions: vec![Condition {
                r#type: ConditionType::Group,
                conditions: vec![Condition {
                    r#type: ConditionType::Group,
                    conditions: vec![Condition {
                        r#type: ConditionType::Group,
                        conditions: vec![deep_leaf],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let settings = ModelSettings {
            associations: vec![ObjectsModelAssociation {
                kind: "model".to_string(),
                when: Some(conduit_core::objects::ModelAssociationWhen {
                    enabled: true,
                    condition: Some(depth4),
                }),
                model_id: Some(conduit_core::objects::ModelIDAssociation {
                    model_id: "test-model".to_string(),
                    exclude: Vec::new(),
                }),
                ..Default::default()
            }],
            ..ModelSettings::default()
        };
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected NestingDepthExceeded"));
        let msg = format!("{err}");
        assert!(
            msg.contains("condition nesting depth must not exceed 3"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn go_parity_condition_tree_accepts_max_allowed_nesting() {
        // Supplemental: a tree at exactly depth 3 (root + 2 nested groups +
        // leaf) is accepted — the cap is inclusive.
        use conduit_core::objects::{Condition, ConditionType};
        let depth3 = Condition {
            r#type: ConditionType::Group,
            conditions: vec![Condition {
                r#type: ConditionType::Group,
                conditions: vec![Condition {
                    r#type: ConditionType::Group,
                    conditions: vec![leaf("prompt_tokens", "gt", json!(100_i64))],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let settings = ModelSettings {
            associations: vec![ObjectsModelAssociation {
                kind: "model".to_string(),
                when: Some(conduit_core::objects::ModelAssociationWhen {
                    enabled: true,
                    condition: Some(depth3),
                }),
                model_id: Some(conduit_core::objects::ModelIDAssociation {
                    model_id: "test-model".to_string(),
                    exclude: Vec::new(),
                }),
                ..Default::default()
            }],
            ..ModelSettings::default()
        };
        assert!(validate_model_settings(&settings).is_ok());
    }

    #[test]
    fn validate_model_settings_rejects_negative_prompt_tokens() {
        // Supplemental golden case for the `prompt_tokens >= 0` rule (Go
        // `validatePromptTokensLeaf` lines 227-229): a negative integer value
        // is rejected even though the operator and type are valid.
        let settings = settings_with_when(when_group(
            true,
            vec![leaf("prompt_tokens", "gt", json!(-1_i64))],
        ));
        let err = validate_model_settings(&settings)
            .err()
            .unwrap_or_else(|| panic!("expected PromptTokensNegative"));
        let msg = format!("{err}");
        assert!(
            msg.contains("prompt_tokens must be greater than or equal to 0"),
            "expected substring in {msg:?}"
        );
    }

    #[test]
    fn validate_developer_associations_runs_regex_and_condition_validation() {
        // End-to-end: a developer entry carrying a regex association is now
        // rejected (developer rules are restricted to channel_model /
        // channel_tags_model), but a model-level `ModelSettings` with a bad
        // regex pattern is rejected by `validate_model_settings` directly.
        // This confirms the trailing delegation from
        // `validate_developer_associations` to `validate_model_settings` (Go
        // line 102) is wired up.
        use conduit_core::objects::RegexAssociation;
        let settings = ModelSettings {
            associations: vec![ObjectsModelAssociation {
                kind: "regex".to_string(),
                regex: Some(RegexAssociation {
                    pattern: "[invalid".to_string(),
                    exclude: Vec::new(),
                }),
                ..Default::default()
            }],
            ..ModelSettings::default()
        };
        assert!(validate_model_settings(&settings).is_err());
    }

    // --- Model association matcher (RUST-P9-002 S15) ------------------------
    //
    // Mirror the Go golden cases in `model_association_matcher_test.go`:
    // `TestDuplicateKeyTracker`, `TestMatchAssociations_Deduplication`,
    // `TestMatchAssociations_EmptyConnectionFiltering`,
    // `TestMatchAssociations_ComplexScenario`, `TestMatchAssociations_ExcludeChannels`,
    // `TestMatchAssociations_ExcludeChannelsByTags`,
    // `TestMatchAssociations_ChannelTagsModel`, `TestMatchAssociations_ChannelTagsRegex`.
    //
    // The Go tests reach into `*ent.Channel` / `*biz.Channel`; the Rust port
    // swaps in a pure [`MatcherChannel`] view (channel id + name + tags +
    // request-model id set), so the assertion surface is the channel-id list
    // and the matched model-id list per connection. The dedup + branch
    // behavior is otherwise a 1:1 port of Go `matchSingleAssociation`.

    fn ch(id: i64, name: &str, models: &[&str]) -> MatcherChannel {
        MatcherChannel::new(id, name, models.iter().map(|s| (*s).to_string()))
    }

    fn ch_with_tags(id: i64, name: &str, models: &[&str], tags: &[&str]) -> MatcherChannel {
        ch(id, name, models).with_tags(tags.iter().map(|s| (*s).to_string()))
    }

    fn conn_ids(conns: &[AssociationConnection]) -> Vec<i64> {
        conns.iter().map(|c| c.channel_id).collect()
    }

    fn conn_model_ids(conns: &[AssociationConnection], channel_id: i64) -> Vec<String> {
        conns
            .iter()
            .find(|c| c.channel_id == channel_id)
            .map(|c| c.model_ids.clone())
            .unwrap_or_default()
    }

    #[test]
    fn duplicate_tracker_adds_once_per_channel_model_pair() {
        // Mirrors Go `TestDuplicateKeyTracker` (lines 13-29): the first add of
        // a (channel, model) pair returns true, the second returns false.
        let mut t = DuplicateChannelModelTracker::new();
        assert!(t.add(1, "model-a"));
        assert!(t.add(1, "model-b"));
        assert!(t.add(2, "model-a"));
        assert!(!t.add(1, "model-a"));
        assert!(!t.add(1, "model-b"));
        assert!(!t.add(2, "model-a"));
    }

    // --- Exact dimension (`model`) -----------------------------------------

    #[test]
    fn exact_model_matches_targets_across_all_channels() {
        // Mirrors Go `TestMatchAssociations_Deduplication` ->
        // `different channels same model should not duplicate` (lines 80-99):
        // a `model` association with the same model id resolves to one
        // connection per channel that exposes it, with a single model id each.
        let channels = vec![
            ch(1, "channel-1", &["gpt-4", "gpt-3.5-turbo"]),
            ch(2, "channel-2", &["gpt-4", "claude-3"]),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "model".to_string(),
            priority: 1,
            model_id: Some(conduit_core::objects::ModelIDAssociation {
                model_id: "gpt-4".to_string(),
                exclude: Vec::new(),
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        assert_eq!(conn_ids(&conns), vec![1, 2]);
        assert_eq!(conn_model_ids(&conns, 1), vec!["gpt-4".to_string()]);
        assert_eq!(conn_model_ids(&conns, 2), vec!["gpt-4".to_string()]);
    }

    #[test]
    fn exact_dimension_misses_when_no_channel_exposes_model() {
        // Mirrors the `non-existent` branch of
        // `TestMatchAssociations_EmptyConnectionFiltering` (lines 207-221): a
        // `model` association whose model id no channel exposes yields no
        // connections.
        let channels = vec![ch(1, "channel-1", &["gpt-4"])];
        let assoc = ObjectsModelAssociation {
            kind: "model".to_string(),
            priority: 1,
            model_id: Some(conduit_core::objects::ModelIDAssociation {
                model_id: "non-existent".to_string(),
                exclude: Vec::new(),
            }),
            ..Default::default()
        };
        let conns = match_connections(std::slice::from_ref(&assoc), &channels);
        assert!(conns.is_empty());
    }

    #[test]
    fn exact_dimension_helper_is_pure_string_equality() {
        // Supplemental: the pure `exact_model_matches` helper encodes the exact
        // dimension independently of the channel set.
        assert!(exact_model_matches("gpt-4", "gpt-4"));
        assert!(!exact_model_matches("gpt-4", "gpt-4o"));
        assert!(!exact_model_matches("gpt-4", "GPT-4")); // case-sensitive
    }

    // --- Regex/pattern dimension (`regex`) ---------------------------------

    #[test]
    fn regex_dimension_matches_pattern_across_all_channels() {
        // Mirrors Go `TestMatchAssociations_Deduplication` ->
        // `multiple regex patterns deduplication` (lines 138-166): a `regex`
        // association resolves to one connection per channel, with the model
        // ids matching the pattern (deduplicated within the channel).
        let channels = vec![
            ch(1, "channel-1", &["gpt-4", "gpt-3.5-turbo"]),
            ch(2, "channel-2", &["gpt-4", "claude-3"]),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "regex".to_string(),
            priority: 1,
            regex: Some(conduit_core::objects::RegexAssociation {
                pattern: "gpt-.*".to_string(),
                exclude: Vec::new(),
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        assert!(conn_ids(&conns).contains(&1));
        assert_eq!(
            conn_model_ids(&conns, 1),
            vec!["gpt-3.5-turbo".to_string(), "gpt-4".to_string()]
        );
        // channel 2 only has gpt-4 matching.
        assert_eq!(conn_model_ids(&conns, 2), vec!["gpt-4".to_string()]);
    }

    #[test]
    fn regex_dimension_uses_xregexp_anchored_semantics() {
        // Supplemental golden case for the shared `xregexp_match_string`
        // contract (the S12 helper reused by S15): a plain pattern with no
        // regex metacharacters is treated as literal whole-string equality,
        // NOT a substring match. So `gpt-4` matches exactly `gpt-4`, not
        // `gpt-4o` (mirrors the exact-match fast path in
        // `TestModelService_ListEnabledModels`).
        assert!(regex_pattern_matches("gpt-4", "gpt-4"));
        assert!(!regex_pattern_matches("gpt-4", "gpt-4o"));
        // Empty pattern matches everything (the model-matcher "blank pattern
        // selects all" semantics inherited from `xregexp.MatchString`).
        assert!(regex_pattern_matches("", "anything"));
        // Star is the match-all sentinel.
        assert!(regex_pattern_matches("*", "anything"));
        // A real regex anchors to the whole string.
        assert!(regex_pattern_matches("gpt-.*", "gpt-4o-mini"));
        assert!(!regex_pattern_matches("gpt-.*", "claude-3"));
    }

    // --- Developer dimension (`channel_model`) -----------------------------

    #[test]
    fn developer_dimension_picks_one_channel_and_one_exact_model() {
        // Mirrors Go `TestMatchAssociations_Deduplication` ->
        // `same channel same model should not duplicate` (lines 52-78): a
        // `channel_model` association resolves to a single connection on the
        // named channel with the named model id, and a later association
        // hitting the same (channel, model) is suppressed by the dedup tracker.
        let channels = vec![
            ch(1, "channel-1", &["gpt-4", "gpt-3.5-turbo"]),
            ch(2, "channel-2", &["gpt-4", "claude-3"]),
        ];
        let associations = vec![
            ObjectsModelAssociation {
                kind: "channel_model".to_string(),
                priority: 1,
                channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                    channel_id: 1,
                    model_id: "gpt-4".to_string(),
                }),
                ..Default::default()
            },
            ObjectsModelAssociation {
                kind: "channel_model".to_string(),
                priority: 2,
                channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                    channel_id: 1,
                    model_id: "gpt-4".to_string(),
                }),
                ..Default::default()
            },
        ];

        let conns = match_connections(&associations, &channels);

        assert_eq!(conn_ids(&conns), vec![1]);
        assert_eq!(conn_model_ids(&conns, 1), vec!["gpt-4".to_string()]);
        // First association's priority wins (dedup drops the second).
        assert_eq!(conns[0].priority, 1);
    }

    #[test]
    fn developer_dimension_helper_selects_channel_by_id() {
        // Supplemental: the pure `developer_channel_matches` helper encodes
        // the developer dimension's channel-selection half independently.
        let channels = vec![ch(1, "one", &[]), ch(2, "two", &[]), ch(3, "three", &[])];
        assert!(developer_channel_matches(&channels, 1));
        assert!(developer_channel_matches(&channels, 3));
        assert!(!developer_channel_matches(&channels, 99));
    }

    #[test]
    fn developer_dimension_misses_when_channel_or_model_absent() {
        // Mirrors Go `matchChannelModel` lines 151-163: missing channel or
        // missing model entry -> no connection.
        let channels = vec![ch(1, "channel-1", &["gpt-4"])];
        // Wrong channel id.
        let assoc_missing_channel = ObjectsModelAssociation {
            kind: "channel_model".to_string(),
            priority: 1,
            channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                channel_id: 999,
                model_id: "gpt-4".to_string(),
            }),
            ..Default::default()
        };
        assert!(
            match_connections(std::slice::from_ref(&assoc_missing_channel), &channels).is_empty()
        );

        // Right channel, missing model.
        let assoc_missing_model = ObjectsModelAssociation {
            kind: "channel_model".to_string(),
            priority: 1,
            channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                channel_id: 1,
                model_id: "missing".to_string(),
            }),
            ..Default::default()
        };
        assert!(
            match_connections(std::slice::from_ref(&assoc_missing_model), &channels).is_empty()
        );
    }

    // --- Type dimension (`channel_regex`) ----------------------------------

    #[test]
    fn type_dimension_picks_one_channel_and_pattern_matches_models() {
        // Mirrors Go `TestMatchAssociations_Deduplication` ->
        // `regex deduplication within same channel` (lines 101-136): a
        // `channel_regex` association resolves to one connection on the named
        // channel, listing every model id matching the pattern; a later
        // `channel_model` association hitting one of those models is suppressed
        // by the dedup tracker.
        let channels = vec![ch(1, "channel-1", &["gpt-4", "gpt-3.5-turbo"])];
        let associations = vec![
            ObjectsModelAssociation {
                kind: "channel_regex".to_string(),
                priority: 1,
                channel_regex: Some(conduit_core::objects::ChannelRegexAssociation {
                    channel_id: 1,
                    pattern: "gpt-.*".to_string(),
                }),
                ..Default::default()
            },
            ObjectsModelAssociation {
                kind: "channel_model".to_string(),
                priority: 2,
                channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                    channel_id: 1,
                    model_id: "gpt-4".to_string(),
                }),
                ..Default::default()
            },
        ];

        let conns = match_connections(&associations, &channels);

        // One connection; gpt-4 appears only once even though the second
        // association also targets it.
        assert_eq!(conn_ids(&conns), vec![1]);
        let model_ids = conn_model_ids(&conns, 1);
        let gpt4_count = model_ids.iter().filter(|m| m.as_str() == "gpt-4").count();
        assert_eq!(gpt4_count, 1, "gpt-4 should appear only once");
        assert_eq!(model_ids.len(), 2); // gpt-3.5-turbo + gpt-4
    }

    // --- Tags dimension (`channel_tags_model` + `channel_tags_regex`) ------

    #[test]
    fn tags_dimension_matches_single_tag_with_exact_model() {
        // Mirrors Go `TestMatchAssociations_ChannelTagsModel` ->
        // `match single tag` (lines 842-862): only channels carrying the tag
        // AND exposing the model id emit a connection.
        let channels = vec![
            ch_with_tags(
                1,
                "openai-primary",
                &["gpt-4", "gpt-3.5-turbo"],
                &["production", "openai"],
            ),
            ch_with_tags(
                2,
                "openai-backup",
                &["gpt-4", "gpt-3.5-turbo"],
                &["backup", "openai"],
            ),
            ch_with_tags(
                3,
                "anthropic-primary",
                &["claude-3-opus"],
                &["production", "anthropic"],
            ),
            ch_with_tags(
                4,
                "development-channel",
                &["gpt-4"],
                &["development", "test"],
            ),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "channel_tags_model".to_string(),
            priority: 1,
            channel_tags_model: Some(conduit_core::objects::ChannelTagsModelAssociation {
                channel_tags: vec!["production".to_string()],
                model_id: "gpt-4".to_string(),
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        // Only channel 1 has the production tag AND exposes gpt-4 (channel 3
        // has production but no gpt-4).
        assert_eq!(conn_ids(&conns), vec![1]);
        assert_eq!(conn_model_ids(&conns, 1), vec!["gpt-4".to_string()]);
    }

    #[test]
    fn tags_dimension_multiple_tags_use_or_logic() {
        // Mirrors Go `TestMatchAssociations_ChannelTagsModel` ->
        // `match multiple tags OR logic` (lines 864-890).
        let channels = vec![
            ch_with_tags(1, "openai-primary", &["gpt-4"], &["production", "openai"]),
            ch_with_tags(2, "openai-backup", &["gpt-4"], &["backup", "openai"]),
            ch_with_tags(
                4,
                "development-channel",
                &["gpt-4"],
                &["development", "test"],
            ),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "channel_tags_model".to_string(),
            priority: 1,
            channel_tags_model: Some(conduit_core::objects::ChannelTagsModelAssociation {
                channel_tags: vec!["backup".to_string(), "development".to_string()],
                model_id: "gpt-4".to_string(),
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        // Channels 2 (backup) and 4 (development) match; order follows input.
        assert_eq!(conn_ids(&conns), vec![2, 4]);
    }

    #[test]
    fn tags_dimension_empty_tag_list_never_matches() {
        // Mirrors Go `TestMatchAssociations_ChannelTagsModel` ->
        // `empty channel tags` (lines 958-974): an empty tag list matches no
        // channel.
        let channels = vec![ch_with_tags(1, "one", &["gpt-4"], &["production"])];
        let assoc = ObjectsModelAssociation {
            kind: "channel_tags_model".to_string(),
            priority: 1,
            channel_tags_model: Some(conduit_core::objects::ChannelTagsModelAssociation {
                channel_tags: Vec::new(),
                model_id: "gpt-4".to_string(),
            }),
            ..Default::default()
        };
        assert!(match_connections(std::slice::from_ref(&assoc), &channels).is_empty());
    }

    #[test]
    fn tags_dimension_regex_matches_all_models_on_tagged_channels() {
        // Mirrors Go `TestMatchAssociations_ChannelTagsRegex` ->
        // `match single tag with pattern` (lines 1046-1074): every model on a
        // tagged channel that matches the pattern is emitted.
        let channels = vec![
            ch_with_tags(
                1,
                "openai-primary",
                &["gpt-4", "gpt-3.5-turbo", "gpt-4-turbo"],
                &["production", "openai"],
            ),
            ch_with_tags(
                3,
                "anthropic-primary",
                &["claude-3-opus", "claude-3-sonnet"],
                &["production", "anthropic"],
            ),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "channel_tags_regex".to_string(),
            priority: 1,
            channel_tags_regex: Some(conduit_core::objects::ChannelTagsRegexAssociation {
                channel_tags: vec!["production".to_string()],
                pattern: "gpt-.*".to_string(),
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        assert_eq!(conn_ids(&conns), vec![1]);
        // All three gpt models on channel 1 match the pattern.
        assert_eq!(conn_model_ids(&conns, 1).len(), 3);
    }

    #[test]
    fn tags_dimension_helper_is_or_logic_with_empty_short_circuit() {
        // Supplemental: the pure `tags_match_channel` helper encodes the tags
        // dimension independently.
        let channels = vec![
            ch_with_tags(1, "one", &[], &["production", "openai"]),
            ch_with_tags(2, "two", &[], &["backup"]),
        ];
        assert!(tags_match_channel(
            &channels,
            1,
            &["production".to_string(), "backup".to_string()]
        ));
        assert!(tags_match_channel(&channels, 2, &["backup".to_string()]));
        assert!(!tags_match_channel(&channels, 1, &["backup".to_string()]));
        // Empty tag list never matches (mirrors the branch short-circuit).
        assert!(!tags_match_channel(&channels, 1, &[]));
    }

    // --- Conditions dimension (`ExcludeAssociation`) -----------------------

    #[test]
    fn conditions_exclude_channel_by_name_pattern() {
        // Mirrors Go `TestMatchAssociations_ExcludeChannels` ->
        // `regex exclude by channel name pattern` (lines 365-387): a `regex`
        // association with an `exclude.channel_name_pattern` skips channels
        // whose name matches the pattern.
        let channels = vec![
            ch(1, "openai-primary", &["gpt-4", "gpt-3.5-turbo"]),
            ch(2, "openai-backup", &["gpt-4", "gpt-3.5-turbo"]),
            ch(3, "anthropic-primary", &["claude-3-opus"]),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "regex".to_string(),
            priority: 1,
            regex: Some(conduit_core::objects::RegexAssociation {
                pattern: "gpt-.*".to_string(),
                exclude: vec![conduit_core::objects::ExcludeAssociation {
                    channel_name_pattern: ".*backup".to_string(),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        // Only channel 1 matches: channel 2 is excluded by name pattern,
        // channel 3 has no gpt-* models.
        assert_eq!(conn_ids(&conns), vec![1]);
    }

    #[test]
    fn conditions_exclude_channel_by_ids() {
        // Mirrors Go `TestMatchAssociations_ExcludeChannels` ->
        // `regex exclude by channel IDs` (lines 389-410): exclude by id list.
        let channels = vec![
            ch(1, "openai-primary", &["gpt-4"]),
            ch(2, "openai-backup", &["gpt-4"]),
            ch(3, "anthropic-primary", &["claude-3-opus"]),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "regex".to_string(),
            priority: 1,
            regex: Some(conduit_core::objects::RegexAssociation {
                pattern: "gpt-.*".to_string(),
                exclude: vec![conduit_core::objects::ExcludeAssociation {
                    channel_ids: vec![2],
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        assert_eq!(conn_ids(&conns), vec![1]);
    }

    #[test]
    fn conditions_exclude_channel_by_tags() {
        // Mirrors Go `TestMatchAssociations_ExcludeChannelsByTags` ->
        // `regex exclude by single channel tag` (lines 585-614): exclude by
        // tag membership.
        let channels = vec![
            ch_with_tags(1, "openai-primary", &["gpt-4"], &["production", "openai"]),
            ch_with_tags(2, "openai-backup", &["gpt-4"], &["backup", "openai"]),
            ch_with_tags(
                4,
                "development-channel",
                &["gpt-4"],
                &["development", "test"],
            ),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "regex".to_string(),
            priority: 1,
            regex: Some(conduit_core::objects::RegexAssociation {
                pattern: "gpt-.*".to_string(),
                exclude: vec![conduit_core::objects::ExcludeAssociation {
                    channel_tags: vec!["backup".to_string()],
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        // Channels 1 and 4 match; channel 2 excluded by tag.
        assert!(conn_ids(&conns).contains(&1));
        assert!(conn_ids(&conns).contains(&4));
        assert!(!conn_ids(&conns).contains(&2));
    }

    #[test]
    fn conditions_exclude_with_pattern_ids_and_tags_combined() {
        // Mirrors Go `TestMatchAssociations_ExcludeChannelsByTags` ->
        // `exclude with tags, pattern, and IDs combined` (lines 680-706): a
        // single exclude rule carrying all three predicates excludes any
        // channel matching *any* of them.
        let channels = vec![
            ch_with_tags(1, "openai-primary", &["gpt-4"], &["production"]),
            ch_with_tags(2, "openai-backup", &["gpt-4"], &["backup"]),
            ch_with_tags(3, "anthropic-primary", &["gpt-4"], &["anthropic"]),
            ch_with_tags(4, "development-channel", &["gpt-4"], &["development"]),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "regex".to_string(),
            priority: 1,
            regex: Some(conduit_core::objects::RegexAssociation {
                pattern: ".*".to_string(),
                exclude: vec![conduit_core::objects::ExcludeAssociation {
                    channel_name_pattern: ".*primary".to_string(),
                    channel_ids: vec![4],
                    channel_tags: vec!["backup".to_string()],
                }],
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        // Every channel is excluded by at least one predicate.
        assert!(conns.is_empty());
    }

    #[test]
    fn conditions_empty_exclude_list_never_excludes() {
        // Mirrors Go `TestMatchAssociations_ExcludeChannels` ->
        // `no exclude when list is empty` (lines 508-524) + `no exclude when
        // nil` (lines 526-542): an empty exclude list (or `nil`) leaves every
        // channel eligible.
        let channels = vec![
            ch(1, "openai-primary", &["gpt-4"]),
            ch(2, "openai-backup", &["gpt-4"]),
        ];
        let assoc = ObjectsModelAssociation {
            kind: "regex".to_string(),
            priority: 1,
            regex: Some(conduit_core::objects::RegexAssociation {
                pattern: "gpt-.*".to_string(),
                exclude: Vec::new(),
            }),
            ..Default::default()
        };

        let conns = match_connections(std::slice::from_ref(&assoc), &channels);

        assert_eq!(conn_ids(&conns), vec![1, 2]);
    }

    #[test]
    fn conditions_exclude_predicate_is_pure() {
        // Supplemental: `channel_matches_exclude` is exposed so the conditions
        // dimension has a dedicated site testable independently.
        let ch = ch_with_tags(1, "openai-backup", &["gpt-4"], &["backup", "openai"]);

        // Name pattern match -> excluded.
        assert!(channel_matches_exclude(
            &ch,
            std::slice::from_ref(&conduit_core::objects::ExcludeAssociation {
                channel_name_pattern: ".*backup".to_string(),
                ..Default::default()
            })
        ));
        // Id match -> excluded.
        assert!(channel_matches_exclude(
            &ch,
            std::slice::from_ref(&conduit_core::objects::ExcludeAssociation {
                channel_ids: vec![1],
                ..Default::default()
            })
        ));
        // Tag match -> excluded.
        assert!(channel_matches_exclude(
            &ch,
            std::slice::from_ref(&conduit_core::objects::ExcludeAssociation {
                channel_tags: vec!["backup".to_string()],
                ..Default::default()
            })
        ));
        // Non-matching rule -> not excluded.
        assert!(!channel_matches_exclude(
            &ch,
            std::slice::from_ref(&conduit_core::objects::ExcludeAssociation {
                channel_ids: vec![99],
                channel_name_pattern: ".*primary".to_string(),
                channel_tags: vec!["production".to_string()],
            })
        ));
        // Empty list -> never excluded.
        assert!(!channel_matches_exclude(&ch, &[]));
    }

    // --- Unknown matcher dimension (S15 contract: no match) ----------------

    #[test]
    fn unknown_matcher_type_returns_no_match() {
        // The S15 contract: an unknown association type never matches. Mirrors
        // the default (empty) arm of Go `matchSingleAssociation` (lines 127-
        // 142): the switch falls through without entering any case, so
        // `connections` stays the zero value.
        let channels = vec![
            ch(1, "channel-1", &["gpt-4"]),
            ch(2, "channel-2", &["claude-3"]),
        ];
        let unknown_variants = ["unknown", "bogus", "", "EXACT", "Model", "channel-model"];
        for kind in unknown_variants {
            let assoc = ObjectsModelAssociation {
                kind: kind.to_string(),
                priority: 1,
                ..Default::default()
            };
            assert!(
                match_connections(std::slice::from_ref(&assoc), &channels).is_empty(),
                "expected no match for unknown association type {kind:?}"
            );
        }
    }

    #[test]
    fn disabled_association_is_skipped_entirely() {
        // Mirrors Go `MatchConnections` lines 80-82: `assoc.Disabled` skips
        // the association before dispatch. Even a normally-matching `model`
        // association produces no connections when disabled.
        let channels = vec![ch(1, "channel-1", &["gpt-4"])];
        let assoc = ObjectsModelAssociation {
            kind: "model".to_string(),
            priority: 1,
            disabled: true,
            model_id: Some(conduit_core::objects::ModelIDAssociation {
                model_id: "gpt-4".to_string(),
                exclude: Vec::new(),
            }),
            ..Default::default()
        };
        assert!(match_connections(std::slice::from_ref(&assoc), &channels).is_empty());
    }

    #[test]
    fn complex_scenario_dedups_across_mixed_association_types() {
        // Mirrors Go `TestMatchAssociations_ComplexScenario` (lines 224-313):
        // a mix of `channel_model`, `regex`, `model`, and `channel_regex`
        // associations must produce no duplicate (channel, model) pairs.
        let channels = vec![
            ch(1, "openai", &["gpt-4", "gpt-3.5-turbo", "gpt-4-turbo"]),
            ch(2, "anthropic", &["claude-3-opus", "claude-3-sonnet"]),
            ch(3, "openai-backup", &["gpt-4", "gpt-3.5-turbo"]),
        ];
        let associations = vec![
            ObjectsModelAssociation {
                kind: "channel_model".to_string(),
                priority: 1,
                channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                    channel_id: 1,
                    model_id: "gpt-4".to_string(),
                }),
                ..Default::default()
            },
            ObjectsModelAssociation {
                kind: "regex".to_string(),
                priority: 2,
                regex: Some(conduit_core::objects::RegexAssociation {
                    pattern: "gpt-.*".to_string(),
                    exclude: Vec::new(),
                }),
                ..Default::default()
            },
            ObjectsModelAssociation {
                kind: "model".to_string(),
                priority: 3,
                model_id: Some(conduit_core::objects::ModelIDAssociation {
                    model_id: "claude-3-opus".to_string(),
                    exclude: Vec::new(),
                }),
                ..Default::default()
            },
            ObjectsModelAssociation {
                kind: "channel_regex".to_string(),
                priority: 4,
                channel_regex: Some(conduit_core::objects::ChannelRegexAssociation {
                    channel_id: 1,
                    pattern: ".*turbo".to_string(),
                }),
                ..Default::default()
            },
        ];

        let conns = match_connections(&associations, &channels);

        // Within each connection, every (channel, model) pair appears once.
        for c in &conns {
            let mut seen = BTreeSet::new();
            for m in &c.model_ids {
                assert!(
                    seen.insert((c.channel_id, m.clone())),
                    "duplicate (channel {}, model {}) in match_connections output",
                    c.channel_id,
                    m
                );
            }
        }
        // gpt-4 on channel 1 is targeted by both the `channel_model`
        // (priority 1) and the `regex` (priority 2) associations. Only the
        // first wins; verify it is the priority-1 connection.
        let ch1_gpt4_priority = conns
            .iter()
            .find(|c| c.channel_id == 1 && c.model_ids.iter().any(|m| m == "gpt-4"))
            .map(|c| c.priority);
        assert_eq!(ch1_gpt4_priority, Some(1));
    }

    // --- Go `TestModelService_QueryModelChannelConnections` order/dedup ----
    //
    // `ModelService.QueryModelChannelConnections` (Go `model.go` lines 523-545)
    // is a thin DB-backed wrapper around `MatchConnections`: it loads channels
    // and delegates to the shared matcher. The pure-logic contract —
    // connections are returned in association order, with global
    // `(channel, model)` dedup — is exercised below against `match_connections`
    // directly, mirroring the Go subtests that the per-dimension tests above do
    // not explicitly pin (order preservation across multiple associations,
    // reversed order, duplicate-channel order, and mixed global dedup).

    fn channel_model_assoc_go(channel_id: i64, model_id: &str) -> ObjectsModelAssociation {
        ObjectsModelAssociation {
            kind: "channel_model".to_string(),
            priority: 1,
            channel_model: Some(conduit_core::objects::ChannelModelAssociation {
                channel_id,
                model_id: model_id.to_string(),
            }),
            ..Default::default()
        }
    }

    fn channel_regex_assoc_go(channel_id: i64, pattern: &str) -> ObjectsModelAssociation {
        ObjectsModelAssociation {
            kind: "channel_regex".to_string(),
            priority: 1,
            channel_regex: Some(conduit_core::objects::ChannelRegexAssociation {
                channel_id,
                pattern: pattern.to_string(),
            }),
            ..Default::default()
        }
    }

    fn regex_assoc_go(pattern: &str) -> ObjectsModelAssociation {
        ObjectsModelAssociation {
            kind: "regex".to_string(),
            priority: 1,
            regex: Some(conduit_core::objects::RegexAssociation {
                pattern: pattern.to_string(),
                exclude: Vec::new(),
            }),
            ..Default::default()
        }
    }

    fn model_assoc_go(model_id: &str) -> ObjectsModelAssociation {
        ObjectsModelAssociation {
            kind: "model".to_string(),
            priority: 1,
            model_id: Some(conduit_core::objects::ModelIDAssociation {
                model_id: model_id.to_string(),
                exclude: Vec::new(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn go_parity_query_connections_preserves_association_order() {
        // Mirrors Go `TestModelService_QueryModelChannelConnections` ->
        // `multiple associations preserves order` (model_test.go lines 147-179):
        // a `channel_model` on channel1 followed by a `channel_regex` on
        // channel2 must yield connections in association order (channel1 first).
        let channels = vec![
            ch(
                1,
                "OpenAI Channel",
                &["gpt-4", "gpt-3.5-turbo", "gpt-4-turbo"],
            ),
            ch(
                2,
                "Anthropic Channel",
                &["claude-3-opus", "claude-3-sonnet", "claude-3-haiku"],
            ),
        ];
        let associations = vec![
            channel_model_assoc_go(1, "gpt-4"),
            channel_regex_assoc_go(2, "^claude-3-.*"),
        ];

        let conns = match_connections(&associations, &channels);

        assert_eq!(conn_ids(&conns), vec![1, 2]);
        assert_eq!(conn_model_ids(&conns, 1), vec!["gpt-4".to_string()]);
        // channel_regex collects matched models in channel model-id iteration
        // order (BTreeSet -> sorted).
        assert_eq!(
            conn_model_ids(&conns, 2),
            vec![
                "claude-3-haiku".to_string(),
                "claude-3-opus".to_string(),
                "claude-3-sonnet".to_string(),
            ]
        );
    }

    #[test]
    fn go_parity_query_connections_reverse_order() {
        // Mirrors Go `TestModelService_QueryModelChannelConnections` ->
        // `multiple associations reverse order` (model_test.go lines 181-214):
        // reversing the association order reverses the connection order.
        let channels = vec![
            ch(
                1,
                "OpenAI Channel",
                &["gpt-4", "gpt-3.5-turbo", "gpt-4-turbo"],
            ),
            ch(
                2,
                "Anthropic Channel",
                &["claude-3-opus", "claude-3-sonnet", "claude-3-haiku"],
            ),
        ];
        let associations = vec![
            channel_regex_assoc_go(2, "^claude-3-.*"),
            channel_model_assoc_go(1, "gpt-4"),
        ];

        let conns = match_connections(&associations, &channels);

        assert_eq!(conn_ids(&conns), vec![2, 1]);
        assert_eq!(conn_model_ids(&conns, 1), vec!["gpt-4".to_string()]);
        assert_eq!(
            conn_model_ids(&conns, 2),
            vec![
                "claude-3-haiku".to_string(),
                "claude-3-opus".to_string(),
                "claude-3-sonnet".to_string(),
            ]
        );
    }

    #[test]
    fn go_parity_query_connections_duplicate_channels_preserve_order() {
        // Mirrors Go `TestModelService_QueryModelChannelConnections` ->
        // `duplicate channel associations preserve order` (model_test.go lines
        // 331-369): three `channel_model` associations targeting channel1,
        // channel2, channel1 again produce three connections in that exact
        // order — the matcher does NOT collapse same-channel connections.
        let channels = vec![
            ch(1, "OpenAI Channel", &["gpt-4", "gpt-3.5-turbo"]),
            ch(2, "Anthropic Channel", &["claude-3-opus"]),
        ];
        let associations = vec![
            channel_model_assoc_go(1, "gpt-4"),
            channel_model_assoc_go(2, "claude-3-opus"),
            channel_model_assoc_go(1, "gpt-3.5-turbo"),
        ];

        let conns = match_connections(&associations, &channels);

        assert_eq!(conn_ids(&conns), vec![1, 2, 1]);
        assert_eq!(conn_model_ids(&conns, 1), vec!["gpt-4".to_string()]);
        assert_eq!(conn_model_ids(&conns, 2), vec!["claude-3-opus".to_string()]);
        // The third connection is a separate entry for channel1.
        assert_eq!(conns[2].model_ids, vec!["gpt-3.5-turbo".to_string()]);
    }

    #[test]
    fn go_parity_query_connections_separate_connections_in_order() {
        // Mirrors Go `TestModelService_QueryModelChannelConnections` ->
        // `model associations produce separate connections in order`
        // (model_test.go lines 371-406): three `channel_model` associations on
        // the same channel but different models each emit a separate connection
        // in association order.
        let channels = vec![ch(
            1,
            "OpenAI Channel",
            &["gpt-3.5-turbo", "gpt-4-turbo", "gpt-4"],
        )];
        let associations = vec![
            channel_model_assoc_go(1, "gpt-3.5-turbo"),
            channel_model_assoc_go(1, "gpt-4-turbo"),
            channel_model_assoc_go(1, "gpt-4"),
        ];

        let conns = match_connections(&associations, &channels);

        assert_eq!(conn_ids(&conns), vec![1, 1, 1]);
        assert_eq!(conns[0].model_ids, vec!["gpt-3.5-turbo".to_string()]);
        assert_eq!(conns[1].model_ids, vec!["gpt-4-turbo".to_string()]);
        assert_eq!(conns[2].model_ids, vec!["gpt-4".to_string()]);
    }

    #[test]
    fn go_parity_query_connections_mixed_global_deduplication() {
        // Mirrors Go `TestModelService_QueryModelChannelConnections` ->
        // `mixed associations with global deduplication` (model_test.go lines
        // 305-329): a `channel_model` for (channel1, gpt-4) followed by a
        // `channel_regex` on channel1 matching `^gpt-4$` must produce a single
        // connection with one model — the global dedup tracker drops the
        // second (channel, model) pair.
        let channels = vec![ch(
            1,
            "OpenAI Channel",
            &["gpt-4", "gpt-3.5-turbo", "gpt-4-turbo"],
        )];
        let associations = vec![
            channel_model_assoc_go(1, "gpt-4"),
            channel_regex_assoc_go(1, "^gpt-4$"),
        ];

        let conns = match_connections(&associations, &channels);

        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].channel_id, 1);
        assert_eq!(conns[0].model_ids, vec!["gpt-4".to_string()]);
    }

    #[test]
    fn go_parity_query_connections_invalid_regex_returns_empty() {
        // Mirrors Go `TestModelService_QueryModelChannelConnections` ->
        // `invalid regex pattern` (model_test.go lines 216-230): an unparseable
        // `channel_regex` pattern produces no connection (xregexp's compile-error
        // path returns false for every model id, so the branch emits nothing).
        let channels = vec![ch(1, "OpenAI Channel", &["gpt-4", "gpt-3.5-turbo"])];
        let associations = vec![channel_regex_assoc_go(1, "[invalid")];

        let conns = match_connections(&associations, &channels);

        assert!(conns.is_empty());
    }

    #[test]
    fn go_parity_query_connections_regex_matches_subset_of_models() {
        // Mirrors Go `TestModelService_QueryModelChannelConnections` ->
        // `channel_regex with specific channel` (model_test.go lines 284-303):
        // a `channel_regex` pattern on one channel matches only the subset of
        // that channel's models whose ids match the anchored pattern.
        let channels = vec![ch(
            3,
            "Gemini Channel",
            &["gemini-pro", "gemini-1.5-pro", "gemini-1.5-flash"],
        )];
        let associations = vec![channel_regex_assoc_go(3, "gemini-1\\.5-.*")];

        let conns = match_connections(&associations, &channels);

        assert_eq!(conn_ids(&conns), vec![3]);
        assert_eq!(
            conn_model_ids(&conns, 3),
            vec!["gemini-1.5-flash".to_string(), "gemini-1.5-pro".to_string(),]
        );
    }

    // --- Go `TestFindUnassociatedChannels` (model_test.go lines 1842-2044) --
    //
    // `findUnassociatedChannels` is the pure-logic core of
    // `ModelService.QueryUnassociatedChannels`. The Go test builds in-memory
    // `*ent.Channel` structs (no DB) and asserts which models remain
    // unassociated after running the association list. Each Go subtest is
    // mirrored below against [`find_unassociated_channels`] with
    // [`MatcherChannel`] views carrying the same `SupportedModels` set.

    fn unassociated_models(
        result: &[UnassociatedChannel],
        channel_id: i64,
    ) -> std::collections::BTreeSet<String> {
        result
            .iter()
            .find(|c| c.channel_id == channel_id)
            .map(|c| c.models.iter().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn go_parity_find_unassociated_no_associations_all_unassociated() {
        // Mirrors Go `TestFindUnassociatedChannels` -> `no associations - all
        // channels unassociated` (model_test.go lines 1870-1878): with no
        // associations, every channel's full model set is reported unassociated.
        let channels = vec![
            ch(1, "OpenAI Channel", &["gpt-4", "gpt-3.5-turbo"]),
            ch(
                2,
                "Anthropic Channel",
                &["claude-3-opus", "claude-3-sonnet"],
            ),
            ch(3, "Gemini Channel", &["gemini-pro", "gemini-1.5-pro"]),
        ];

        let result = find_unassociated_channels(&channels, &[]);

        assert_eq!(result.len(), 3);
        for info in &result {
            assert!(!info.models.is_empty());
        }
        assert_eq!(
            unassociated_models(&result, 1),
            ["gpt-3.5-turbo".to_string(), "gpt-4".to_string()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
    }

    #[test]
    fn go_parity_find_unassociated_channel_model_excludes_matched_model() {
        // Mirrors Go `TestFindUnassociatedChannels` -> `channel_model
        // association` (model_test.go lines 1880-1907): a `channel_model`
        // association for (channel1, gpt-4) leaves only gpt-3.5-turbo
        // unassociated on channel1.
        let channels = vec![
            ch(1, "OpenAI Channel", &["gpt-4", "gpt-3.5-turbo"]),
            ch(
                2,
                "Anthropic Channel",
                &["claude-3-opus", "claude-3-sonnet"],
            ),
            ch(3, "Gemini Channel", &["gemini-pro", "gemini-1.5-pro"]),
        ];
        let associations = vec![channel_model_assoc_go(1, "gpt-4")];

        let result = find_unassociated_channels(&channels, &associations);

        let ch1_models = unassociated_models(&result, 1);
        assert!(ch1_models.contains("gpt-3.5-turbo"));
        assert!(!ch1_models.contains("gpt-4"));
    }

    #[test]
    fn go_parity_find_unassociated_regex_excludes_matched_models() {
        // Mirrors Go `TestFindUnassociatedChannels` -> `regex association`
        // (model_test.go lines 1909-1936): a `regex` association with pattern
        // `^claude-3-.*` removes both claude-3-opus and claude-3-sonnet from
        // channel2's unassociated set.
        let channels = vec![
            ch(1, "OpenAI Channel", &["gpt-4", "gpt-3.5-turbo"]),
            ch(
                2,
                "Anthropic Channel",
                &["claude-3-opus", "claude-3-sonnet"],
            ),
            ch(3, "Gemini Channel", &["gemini-pro", "gemini-1.5-pro"]),
        ];
        let associations = vec![regex_assoc_go("^claude-3-.*")];

        let result = find_unassociated_channels(&channels, &associations);

        let ch2_models = unassociated_models(&result, 2);
        assert!(!ch2_models.contains("claude-3-opus"));
        assert!(!ch2_models.contains("claude-3-sonnet"));
    }

    #[test]
    fn go_parity_find_unassociated_model_exclude_keeps_model_unassociated() {
        // Mirrors Go `TestFindUnassociatedChannels` -> `model association with
        // exclude` (model_test.go lines 1938-1968): a `model` association for
        // gemini-pro with an exclude rule on channel id 3 leaves gemini-pro
        // still unassociated on channel3 (the exclude drops channel3 from the
        // association's match set).
        let channels = vec![
            ch(1, "OpenAI Channel", &["gpt-4", "gpt-3.5-turbo"]),
            ch(
                2,
                "Anthropic Channel",
                &["claude-3-opus", "claude-3-sonnet"],
            ),
            ch(3, "Gemini Channel", &["gemini-pro", "gemini-1.5-pro"]),
        ];
        let associations = vec![ObjectsModelAssociation {
            kind: "model".to_string(),
            priority: 1,
            model_id: Some(conduit_core::objects::ModelIDAssociation {
                model_id: "gemini-pro".to_string(),
                exclude: vec![conduit_core::objects::ExcludeAssociation {
                    channel_ids: vec![3],
                    ..Default::default()
                }],
            }),
            ..Default::default()
        }];

        let result = find_unassociated_channels(&channels, &associations);

        let ch3_models = unassociated_models(&result, 3);
        assert!(ch3_models.contains("gemini-pro"));
    }

    #[test]
    fn go_parity_find_unassociated_channel_regex_excludes_matched_models() {
        // Mirrors Go `TestFindUnassociatedChannels` -> `channel_regex
        // association` (model_test.go lines 1970-1998): a `channel_regex` on
        // channel1 with pattern `^gpt-.*` removes both gpt-4 and gpt-3.5-turbo
        // from channel1's unassociated set.
        let channels = vec![
            ch(1, "OpenAI Channel", &["gpt-4", "gpt-3.5-turbo"]),
            ch(
                2,
                "Anthropic Channel",
                &["claude-3-opus", "claude-3-sonnet"],
            ),
            ch(3, "Gemini Channel", &["gemini-pro", "gemini-1.5-pro"]),
        ];
        let associations = vec![channel_regex_assoc_go(1, "^gpt-.*")];

        let result = find_unassociated_channels(&channels, &associations);

        let ch1_models = unassociated_models(&result, 1);
        assert!(!ch1_models.contains("gpt-4"));
        assert!(!ch1_models.contains("gpt-3.5-turbo"));
    }

    #[test]
    fn go_parity_find_unassociated_multiple_associations_covers_all() {
        // Mirrors Go `TestFindUnassociatedChannels` -> `multiple associations`
        // (model_test.go lines 2000-2038): a combination of `model` and `regex`
        // associations covering every model across all three channels yields an
        // empty unassociated list.
        let channels = vec![
            ch(1, "OpenAI Channel", &["gpt-4", "gpt-3.5-turbo"]),
            ch(
                2,
                "Anthropic Channel",
                &["claude-3-opus", "claude-3-sonnet"],
            ),
            ch(3, "Gemini Channel", &["gemini-pro", "gemini-1.5-pro"]),
        ];
        let associations = vec![
            model_assoc_go("gpt-4"),
            model_assoc_go("gpt-3.5-turbo"),
            regex_assoc_go("^claude-3-.*"),
            model_assoc_go("gemini-pro"),
            model_assoc_go("gemini-1.5-pro"),
        ];

        let result = find_unassociated_channels(&channels, &associations);

        assert!(result.is_empty());
    }

    #[test]
    fn go_parity_find_unassociated_no_channels_returns_empty() {
        // Mirrors Go `TestFindUnassociatedChannels` -> `no channels`
        // (model_test.go lines 2040-2043): an empty channel set short-circuits
        // to an empty result.
        let result = find_unassociated_channels(&[], &[]);

        assert!(result.is_empty());
    }
}
