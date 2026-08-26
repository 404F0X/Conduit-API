//! S11 auto-disable decision (channel-level vs API-key-level, threshold
//! crossing).
//!
//! Ported from Go `internal/server/biz/channel_auto_disable.go` — the pure
//! decision core of `checkAndHandleChannelError` /
//! `checkAndHandleAPIKeyError`. The side-effecting arms of those Go functions
//! (`markChannelUnavailable`, `DisableAPIKey`, `ChannelAutoDisabledEvent`
//! emission) are surfaced here as the [`AutoDisableDecision`] enum so the
//! caller (host crate) can apply them against the persisted channel row +
//! disabled-key cache.
//!
//! Error-message derivation ([`derive_error_message`]) is ported from
//! `internal/server/biz/channel_metrics.go::deriveErrorMessage`.

/// One `(status_code, threshold)` rule in an [`AutoDisablePolicy`].
///
/// Mirrors Go's per-status policy entry: when the channel (or a single API
/// key) accumulates `times` consecutive failures with `status`, the disable
/// action fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoDisableStatusRule {
    /// HTTP status code that this rule triggers on.
    pub status: i64,
    /// Consecutive occurrences required before the disable fires.
    pub times: i64,
}

/// Auto-disable policy snapshot. Mirrors Go's `AutoDisableChannel`.
//
// [Helmholtz-the-3rd ?] TODO: replace with a typed port of
// `conduit_core::objects::AutoDisableChannel` once it lands. The
// `decide_auto_disable` signature is stable regardless.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoDisablePolicy {
    /// Whether auto-disable is active at all. When false, the decision is
    /// always `Keep`.
    pub enabled: bool,
    /// Per-status thresholds. The first matching rule (by status) wins, which
    /// matches Go's `for _, statusConfig := range ...` first-match loop.
    pub statuses: Vec<AutoDisableStatusRule>,
}

impl AutoDisablePolicy {
    pub fn from_statuses(enabled: bool, statuses: Vec<(i64, i64)>) -> Self {
        Self {
            enabled,
            statuses: statuses
                .into_iter()
                .map(|(status, times)| AutoDisableStatusRule { status, times })
                .collect(),
        }
    }

    /// Look up the threshold for a given status code (`None` if no rule).
    fn threshold_for(&self, status: i64) -> Option<i64> {
        self.statuses
            .iter()
            .find(|rule| rule.status == status)
            .map(|rule| rule.times)
    }
}

/// The current consecutive-failure count for a (channel, optional key,
/// status) cell. Mirrors the in-memory state Go mutates under
/// `channelErrorCountsLock` / `apiKeyErrorCountsLock`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ErrorCountState {
    /// Consecutive failures so far for this cell *before* the current event.
    /// The helper adds 1 internally to mirror Go's
    /// `svc.channelErrorCounts[id][code]++` followed by a threshold check.
    pub prior_count: i64,
}

/// What the auto-disable decision helper chose. Mirrors the side-effecting
/// branches in Go (`markChannelUnavailable` vs `DisableAPIKey`), expressed as
/// a pure value for the caller to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoDisableDecision {
    /// Nothing to disable: either auto-disable is off, the status code is not
    /// in the policy, or the threshold has not been crossed yet.
    /// `new_count` is the post-increment consecutive-error count the caller
    /// should persist.
    Keep { new_count: i64 },
    /// Threshold reached on a channel-level error (no API key in the
    /// performance record). The whole channel should be disabled.
    /// `status_code` / `threshold` / `actual_count` mirror Go's
    /// `ChannelAutoDisabledEvent` payload.
    DisableChannel {
        status_code: i64,
        threshold: i64,
        actual_count: i64,
    },
    /// Threshold reached on a per-key error. The given `api_key` should be
    /// added to `disabled_api_keys`. Whether this also disables the whole
    /// channel is reported separately via `channel_exhausted` (caller computes
    /// it from the post-disable enabled-key set, mirroring Go's
    /// `len(enabledKeys) == 0` check in `DisableAPIKey`).
    DisableAPIKey {
        api_key: String,
        status_code: i64,
        threshold: i64,
        actual_count: i64,
        /// Hint: if this is the last enabled key, the channel must also be
        /// disabled. Computed from `remaining_enabled_keys` (the count that
        /// will remain *after* this key is disabled).
        channel_exhausted: bool,
    },
}

/// Input for the auto-disable decision. Mirrors the relevant fields of Go's
/// `PerformanceRecord` (`channel_metrics.go`).
#[derive(Debug, Clone)]
pub struct PerformanceError<'a> {
    pub channel_id: i64,
    /// The API key used for the failing request, when known. When `None`,
    /// errors are tracked at the channel level (Go: `channelErrorCounts`);
    /// when `Some`, at the per-key level (Go: `apiKeyErrorCounts`).
    pub api_key: Option<&'a str>,
    pub response_status_code: i64,
    /// Consecutive-error count *before* this event, for the matching cell
    /// (channel-level when `api_key` is `None`, per-key otherwise).
    pub prior_count: i64,
    /// Number of currently-enabled API keys, *including* the one being
    /// considered for disable. Used to detect the "last key" case.
    pub current_enabled_key_count: usize,
}

/// Pure auto-disable decision. Given the policy, the failing performance
/// record, and the prior consecutive-error count for the matching cell,
/// decide whether to keep tracking, disable the channel, or disable a single
/// API key.
///
/// Mirrors Go's `checkAndHandleChannelError` / `checkAndHandleAPIKeyError`:
/// - skip entirely when `policy.enabled` is false;
/// - skip when `response_status_code` has no matching rule;
/// - otherwise increment the counter and compare against the rule's `times`;
/// - on threshold crossing: per-key path (when `api_key` is set) disables the
///   key and reports channel exhaustion; channel path disables the channel.
pub fn decide_auto_disable(
    policy: &AutoDisablePolicy,
    perf: &PerformanceError<'_>,
) -> AutoDisableDecision {
    if !policy.enabled {
        return AutoDisableDecision::Keep {
            new_count: perf.prior_count,
        };
    }

    let Some(threshold) = policy.threshold_for(perf.response_status_code) else {
        // Status code is not tracked: Go still increments the counter (so a
        // later rule change can trip), but never disables. We mirror by
        // reporting the incremented count.
        return AutoDisableDecision::Keep {
            new_count: perf.prior_count + 1,
        };
    };

    let new_count = perf.prior_count + 1;
    if new_count < threshold {
        return AutoDisableDecision::Keep { new_count };
    }

    // Threshold reached.
    match perf.api_key {
        Some(key) => {
            // Per-key path: disabling this key leaves `count - 1` enabled keys
            // (the current key is among them). If that drops to zero, the
            // channel must be disabled too (Go's DisableAPIKey).
            let channel_exhausted = perf.current_enabled_key_count <= 1;
            AutoDisableDecision::DisableAPIKey {
                api_key: key.to_string(),
                status_code: perf.response_status_code,
                threshold,
                actual_count: new_count,
                channel_exhausted,
            }
        }
        None => AutoDisableDecision::DisableChannel {
            status_code: perf.response_status_code,
            threshold,
            actual_count: new_count,
        },
    }
}

/// Derive the human-readable error message for a disabled channel. Ported
/// 1:1 from Go `deriveErrorMessage` (`channel_metrics.go`): prefer the HTTP
/// status text, fall back to `"Error <code>"`. The HTTP-status-text table is
/// the small subset Go's `http.StatusText` returns for the codes that the
/// auto-disable policy targets (401/403/429/500/502/503/504); unknown codes
/// use the Go fallback.
pub fn derive_error_message(error_code: i64) -> String {
    match error_code {
        400 => "Bad Request".to_string(),
        401 => "Unauthorized".to_string(),
        403 => "Forbidden".to_string(),
        404 => "Not Found".to_string(),
        408 => "Request Timeout".to_string(),
        429 => "Too Many Requests".to_string(),
        500 => "Internal Server Error".to_string(),
        502 => "Bad Gateway".to_string(),
        503 => "Service Unavailable".to_string(),
        504 => "Gateway Timeout".to_string(),
        _ => format!("Error {error_code}"),
    }
}
