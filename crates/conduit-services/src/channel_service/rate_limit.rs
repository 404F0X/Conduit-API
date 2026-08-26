//! S08 retryable-status / error-pattern normalization + rate-limit validation.
//!
//! Ports of Go `biz` package:
//! - `NormalizeRetryableStatusCodes` (`internal/server/biz/channel.go:597-613`).
//! - `NormalizeRetryableErrorPatterns` (`internal/server/biz/channel.go:617-649`).
//! - `ValidateRateLimit` (`internal/server/biz/channel_rate_limit.go:15-55`).
//!
//! These are pure validators/normalizers that the channel build/create/update
//! orchestrator runs after merging settings layers. Error strings are byte-exact
//! with the Go `fmt.Errorf` messages so contract tests can match them.

use conduit_core::objects::channel_settings::{
    ChannelRateLimit, ChannelSettings, RetryableErrorPattern,
};
use regex::Regex;

/// Errors raised by [`normalize_retryable_status_codes`] /
/// [`normalize_retryable_error_patterns`] / [`validate_rate_limit`].
///
/// The `Display` form mirrors the Go `fmt.Errorf(...)` / `error.Wrap(...)` text
/// byte-for-byte (including the wrapping `%w` chain for invalid regex), so that
/// contract tests asserting on substrings transfer 1:1.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelNormalizeError {
    /// `invalid retryable status code {code}: must be between 400 and 599`
    #[error("invalid retryable status code {code}: must be between 400 and 599")]
    InvalidRetryableStatusCode { code: i64 },
    /// `invalid retryable error regex {pattern:?}: {reason}` — the `{reason}`
    /// suffix is the underlying regex compile error text, mirroring Go's `%w`.
    #[error("invalid retryable error regex {pattern:?}: {reason}")]
    InvalidRetryableErrorRegex { pattern: String, reason: String },
    /// `{field} must be >= 0`
    #[error("{field} must be >= 0")]
    NegativeRateLimitField { field: &'static str },
    /// `queueSize requires maxConcurrent > 0`
    #[error("queueSize requires maxConcurrent > 0")]
    QueueSizeRequiresMaxConcurrent,
}

/// Validate, deduplicate, and sort `retryable_status_codes` in place.
///
/// Mirrors Go `NormalizeRetryableStatusCodes` (`channel.go:597-613`):
/// - empty/`None` settings is a no-op (returns `Ok`);
/// - each code must satisfy `400 <= code <= 599` (else
///   [`ChannelNormalizeError::InvalidRetryableStatusCode`]);
/// - codes are sorted ascending and deduplicated (`slice::sort` then
///   `dedup`, matching Go's `slices.Sort` + `slices.Compact`).
pub fn normalize_retryable_status_codes(
    settings: &mut ChannelSettings,
) -> Result<(), ChannelNormalizeError> {
    if settings.retryable_status_codes.is_empty() {
        return Ok(());
    }

    for code in &settings.retryable_status_codes {
        if *code < 400 || *code > 599 {
            return Err(ChannelNormalizeError::InvalidRetryableStatusCode { code: *code });
        }
    }

    settings.retryable_status_codes.sort_unstable();
    settings.retryable_status_codes.dedup();

    Ok(())
}

/// Validate, trim, and deduplicate `retryable_error_patterns` in place.
///
/// Mirrors Go `NormalizeRetryableErrorPatterns` (`channel.go:617-649`):
/// - empty settings is a no-op;
/// - each pattern is `TrimSpace`d; empty-after-trim patterns are dropped;
/// - when `regex == true` the pattern is compiled to validate it (compile error
///   → [`ChannelNormalizeError::InvalidRetryableErrorRegex`]);
/// - dedup key is `"{regex_bool}\x00{pattern}"` (regex flag + trimmed text), so
///   the same text with different `regex` flags survives as distinct entries
///   but exact `(regex, pattern)` duplicates collapse.
pub fn normalize_retryable_error_patterns(
    settings: &mut ChannelSettings,
) -> Result<(), ChannelNormalizeError> {
    if settings.retryable_error_patterns.is_empty() {
        return Ok(());
    }

    let mut patterns: Vec<RetryableErrorPattern> =
        Vec::with_capacity(settings.retryable_error_patterns.len());
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(settings.retryable_error_patterns.len());

    for mut pattern in settings.retryable_error_patterns.drain(..) {
        pattern.pattern = pattern.pattern.trim().to_string();
        if pattern.pattern.is_empty() {
            continue;
        }

        if pattern.regex
            && let Err(err) = Regex::new(&pattern.pattern)
        {
            return Err(ChannelNormalizeError::InvalidRetryableErrorRegex {
                pattern: pattern.pattern,
                reason: err.to_string(),
            });
        }

        let key = format!("{}\u{0}{}", pattern.regex, pattern.pattern);
        if !seen.insert(key) {
            continue;
        }

        patterns.push(pattern);
    }

    settings.retryable_error_patterns = patterns;

    Ok(())
}

/// Validate `ChannelRateLimit` invariants enforced at the API boundary.
///
/// Mirrors Go `ValidateRateLimit` (`channel_rate_limit.go:15-47`):
/// - all numeric fields must be `>= 0` when `Some` (`rpm`, `tpm`,
///   `max_concurrent`, `queue_size`, `queue_timeout_ms`);
/// - `queue_size > 0` requires `max_concurrent > 0` (a queue without a
///   capacity ceiling is meaningless).
///
/// `None` rate limit is valid (no admission control configured).
pub fn validate_rate_limit(
    rate_limit: Option<&ChannelRateLimit>,
) -> Result<(), ChannelNormalizeError> {
    let rl = match rate_limit {
        None => return Ok(()),
        Some(rl) => rl,
    };

    non_negative_rate_limit_field("rpm", rl.rpm)?;
    non_negative_rate_limit_field("tpm", rl.tpm)?;
    non_negative_rate_limit_field("maxConcurrent", rl.max_concurrent)?;
    non_negative_rate_limit_field("queueSize", rl.queue_size)?;
    non_negative_rate_limit_field("queueTimeoutMs", rl.queue_timeout_ms)?;

    if matches!(rl.queue_size, Some(qs) if qs > 0)
        && !matches!(rl.max_concurrent, Some(mc) if mc > 0)
    {
        return Err(ChannelNormalizeError::QueueSizeRequiresMaxConcurrent);
    }

    Ok(())
}

/// Helper mirroring Go `nonNegativeRateLimitField` (`channel_rate_limit.go:49-55`).
fn non_negative_rate_limit_field(
    field: &'static str,
    v: Option<i64>,
) -> Result<(), ChannelNormalizeError> {
    if matches!(v, Some(n) if n < 0) {
        return Err(ChannelNormalizeError::NegativeRateLimitField { field });
    }
    Ok(())
}
