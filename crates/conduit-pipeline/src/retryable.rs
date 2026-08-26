//! Retryable-error judgment (RUST-P8-002 S16).
//!
//! Ports the Go three-source retryable decision consulted by the pipeline's
//! same-channel retry arm (`ChannelRetryable.CanRetry`, implemented by the Go
//! orchestrator's `PersistentOutboundTransformer.CanRetry`,
//! `internal/server/orchestrator/outbound.go:562-609`):
//!
//! 1. **HTTP status default set** — `httpclient.IsHTTPStatusCodeRetryable`
//!    (`llm/httpclient/utils.go:146-163`): 429 retryable, other 4xx not,
//!    all 5xx retryable, non-error codes (0/1xx/2xx/3xx) not.
//! 2. **Provider error status** — the status is *extracted* from the error
//!    (`ExtractStatusCodeFromError`, `internal/server/orchestrator/retry.go:75-91`):
//!    Go reads `httpclient.Error.StatusCode` (raw upstream HTTP error) or
//!    `llm.ResponseError.StatusCode` (transformed provider error), else 0.
//!    Both arms map onto [`conduit_core::ConduitError::provider_status`] in the
//!    unified Rust error.
//! 3. **Channel settings** — `ChannelSettings.RetryableStatusCodes` (extra
//!    codes) and `ChannelSettings.RetryableErrorPatterns` (substring/regex
//!    matchers over the error text), `internal/server/orchestrator/retry.go:24-72`.
//!
//! Priority (Go `isRetryableErrorForChannel`, `retry.go:24-40`): the default
//! status set short-circuits `true` first; only then are channel-specific
//! status codes and error patterns consulted. `nil` channel/settings stops
//! after the default set.
//!
//! The sibling port for load-balancer scoring lives in
//! `conduit-orchestrator/src/load_balancer.rs:1690-1744` (status-code-only
//! view); this module is the pipeline-side, `ConduitError`-driven judgment the
//! retry hooks consume. Keep the two in sync with the same Go source.

use std::sync::Arc;

use conduit_core::ConduitError;
use conduit_core::objects::channel_settings::{ChannelSettings, RetryableErrorPattern};

use crate::pipeline::CanRetryFn;

/// Whether `status_code` is retryable by default. Mirrors Go
/// `httpclient.IsHTTPStatusCodeRetryable` (`llm/httpclient/utils.go:149-163`):
/// 429 is retryable (rate limiting); other 4xx are not; all 5xx are;
/// everything else (0, 1xx-3xx) does not need retrying.
pub fn is_http_status_code_retryable(status_code: i64) -> bool {
    if status_code == 429 {
        return true; // 429 is retryable (rate limiting).
    }
    if (400..500).contains(&status_code) {
        return false; // Other 4xx errors are not retryable.
    }
    if status_code >= 500 {
        return true; // 5xx errors are retryable.
    }
    false // Non-error status codes don't need retrying.
}

/// Extract the upstream HTTP status code from an error. Mirrors Go
/// `ExtractStatusCodeFromError` (`internal/server/orchestrator/retry.go:75-91`):
/// Go reads `httpclient.Error.StatusCode` first, then
/// `llm.ResponseError.StatusCode`, else 0. In the unified Rust error both
/// upstream shapes surface as [`ConduitError::provider_status`]; absent means 0
/// (transport failures without a status are NOT retryable by the default set,
/// exactly like Go).
pub fn extract_status_code_from_error(err: &ConduitError) -> i64 {
    err.provider_status.map(i64::from).unwrap_or(0)
}

/// Whether the error is a 429 rate-limit error. Mirrors Go
/// `httpclient.IsRateLimitErr` (`llm/httpclient/errors.go:24-27`). The Go
/// orchestrator's `CanRetry` uses this to *skip* same-channel retry for 429
/// and force a channel switch (`outbound.go:594-608`).
pub fn is_rate_limit_error(err: &ConduitError) -> bool {
    extract_status_code_from_error(err) == 429
}

/// Default-set-only retryable check. Mirrors Go `isRetryableError`
/// (`internal/server/orchestrator/retry.go:16-22`).
pub fn is_retryable_error(err: &ConduitError) -> bool {
    is_http_status_code_retryable(extract_status_code_from_error(err))
}

/// The error text the retryable patterns match against. Go matches
/// `err.Error()`; for the transformed provider error (`llm.ResponseError`)
/// that text embeds the provider message and code
/// (`llm/model.go:841-860`: `"... error: <message>, code: <code>, ..."`), so
/// the Rust analog composes `ConduitError.message` with the `code` field.
fn error_match_text(err: &ConduitError) -> String {
    let mut text = err.message.clone();
    if let Some(code) = &err.code {
        text.push_str(", code: ");
        text.push_str(code);
    }
    text
}

/// Whether the error text matches any configured retryable pattern. Mirrors Go
/// `matchesRetryableErrorPattern` (`internal/server/orchestrator/retry.go:42-72`):
/// empty patterns are skipped; `regex: true` patterns use regex matching with
/// compile errors silently ignored; otherwise a case-sensitive substring test.
pub fn matches_retryable_error_pattern(message: &str, patterns: &[RetryableErrorPattern]) -> bool {
    if message.is_empty() || patterns.is_empty() {
        return false;
    }

    for pattern in patterns {
        if pattern.pattern.is_empty() {
            continue;
        }

        if pattern.regex {
            // Go `regexp.MatchString` — a compile error is ignored (`continue`).
            if let Ok(re) = regex::Regex::new(&pattern.pattern)
                && re.is_match(message)
            {
                return true;
            }
            continue;
        }

        if message.contains(&pattern.pattern) {
            return true;
        }
    }

    false
}

/// Three-source retryable judgment for a channel. Mirrors Go
/// `isRetryableErrorForChannel` (`internal/server/orchestrator/retry.go:24-40`):
///
/// 1. status in the default retryable set → `true` (short-circuit);
/// 2. no channel settings (`ch == nil || ch.Settings == nil`) → `false`;
/// 3. status in `settings.retryable_status_codes` → `true`;
/// 4. error text matches `settings.retryable_error_patterns` → `true`;
/// 5. otherwise `false`.
pub fn is_retryable_error_for_channel(
    err: &ConduitError,
    settings: Option<&ChannelSettings>,
) -> bool {
    let status_code = extract_status_code_from_error(err);
    if is_http_status_code_retryable(status_code) {
        return true;
    }

    let Some(settings) = settings else {
        return false;
    };

    settings.retryable_status_codes.contains(&status_code)
        || matches_retryable_error_pattern(
            &error_match_text(err),
            &settings.retryable_error_patterns,
        )
}

/// Build a [`CanRetryFn`] retry hook from channel settings, wiring the
/// three-source judgment into the pipeline's same-channel retry arm. This is
/// the pipeline-side slice of Go
/// `PersistentOutboundTransformer.CanRetry` (`outbound.go:562-609`) — the
/// `isRetryableErrorForChannel` core. The orchestrator adds its own layers on
/// top (circuit-breaker skip, local queue/RPM bounce, empty-response allow,
/// 429 force-switch, next-model probing); those depend on orchestrator state
/// and stay in that crate.
pub fn channel_retry_hook(settings: Option<ChannelSettings>) -> CanRetryFn {
    Arc::new(move |err: &ConduitError| is_retryable_error_for_channel(err, settings.as_ref()))
}

/// Per-channel **extra** retryable judgment: does the error match one of the
/// channel's *additional* retryable status codes or error patterns?
///
/// Unlike [`is_retryable_error_for_channel`], this deliberately **excludes** the
/// default status-code set (429/5xx) — it answers only "did this channel opt
/// this error IN beyond the defaults?". The pipeline's same-channel gate ORs
/// this with the injected `RetryHooks::can_retry` (whose default already covers
/// the shared set), so combining the two never double-counts the default set
/// and never overrides an explicit `can_retry = false` for a non-configured
/// channel. `retryable_status_codes` / `retryable_error_patterns` flow onto the
/// candidate at selection time, so this is a zero-allocation slice check on the
/// cold retry path (no `ChannelSettings` reconstruction per attempt).
pub fn is_channel_extra_retryable(
    err: &ConduitError,
    retryable_status_codes: &[i64],
    retryable_error_patterns: &[RetryableErrorPattern],
) -> bool {
    let status_code = extract_status_code_from_error(err);
    retryable_status_codes.contains(&status_code)
        || matches_retryable_error_pattern(&error_match_text(err), retryable_error_patterns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{
        AttemptOutcome, RetryContext, RetryDecision, RetryPolicy, RetryState, decide_retry,
    };

    fn upstream_with_status(status: u16) -> ConduitError {
        ConduitError::upstream("upstream failure").with_provider_status(status)
    }

    // -- default retryable set (Go httpclient/utils.go golden behavior) ------

    #[test]
    fn default_set_matches_go_is_http_status_code_retryable() {
        // 429 retryable; other 4xx not; 5xx retryable; 0/2xx/3xx not.
        assert!(is_http_status_code_retryable(429));
        assert!(!is_http_status_code_retryable(400));
        assert!(!is_http_status_code_retryable(404));
        assert!(!is_http_status_code_retryable(499));
        assert!(is_http_status_code_retryable(500));
        assert!(is_http_status_code_retryable(502));
        assert!(is_http_status_code_retryable(503));
        assert!(is_http_status_code_retryable(529));
        assert!(!is_http_status_code_retryable(0));
        assert!(!is_http_status_code_retryable(200));
        assert!(!is_http_status_code_retryable(302));
    }

    // -- status extraction (Go ExtractStatusCodeFromError) -------------------

    #[test]
    fn extract_status_reads_provider_status_else_zero() {
        assert_eq!(
            extract_status_code_from_error(&upstream_with_status(502)),
            502
        );
        // No provider status (transport failure) -> 0, like Go's unknown error
        // types.
        assert_eq!(
            extract_status_code_from_error(&ConduitError::upstream("conn refused")),
            0
        );
    }

    #[test]
    fn transport_error_without_status_is_not_retryable_by_default() {
        // Go: fmt.Errorf transport errors carry no status -> 0 -> not in the
        // default set.
        assert!(!is_retryable_error(&ConduitError::upstream("conn refused")));
    }

    #[test]
    fn rate_limit_error_is_429_only() {
        assert!(is_rate_limit_error(&upstream_with_status(429)));
        assert!(!is_rate_limit_error(&upstream_with_status(500)));
        assert!(!is_rate_limit_error(&ConduitError::upstream("no status")));
    }

    // -- three-source judgment (Go isRetryableErrorForChannel) ---------------

    fn settings_with(codes: Vec<i64>, patterns: Vec<RetryableErrorPattern>) -> ChannelSettings {
        ChannelSettings {
            retryable_status_codes: codes,
            retryable_error_patterns: patterns,
            ..ChannelSettings::default()
        }
    }

    #[test]
    fn default_set_short_circuits_before_channel_settings() {
        // 503 is in the default set — retryable even with empty settings and
        // even with None settings (Go retry.go:30-32 returns before the nil
        // check).
        assert!(is_retryable_error_for_channel(
            &upstream_with_status(503),
            None
        ));
        assert!(is_retryable_error_for_channel(
            &upstream_with_status(429),
            Some(&ChannelSettings::default())
        ));
    }

    #[test]
    fn none_settings_stops_after_default_set() {
        // Go retry.go:34-36 — nil channel/settings -> false for non-default
        // statuses.
        assert!(!is_retryable_error_for_channel(
            &upstream_with_status(402),
            None
        ));
    }

    #[test]
    fn channel_status_codes_extend_default_set() {
        // Go retry.go:38 — slices.Contains(ch.Settings.RetryableStatusCodes, ...).
        let settings = settings_with(vec![402, 418], vec![]);
        assert!(is_retryable_error_for_channel(
            &upstream_with_status(402),
            Some(&settings)
        ));
        assert!(is_retryable_error_for_channel(
            &upstream_with_status(418),
            Some(&settings)
        ));
        // Not listed and not in default set -> false.
        assert!(!is_retryable_error_for_channel(
            &upstream_with_status(400),
            Some(&settings)
        ));
    }

    #[test]
    fn substring_pattern_matches_error_message() {
        // Go retry.go:66-68 — plain substring containment.
        let settings = settings_with(
            vec![],
            vec![RetryableErrorPattern {
                pattern: "overloaded".to_string(),
                regex: false,
            }],
        );
        let err = ConduitError::upstream("provider overloaded, try later");
        assert!(is_retryable_error_for_channel(&err, Some(&settings)));

        let other = ConduitError::upstream("invalid api key");
        assert!(!is_retryable_error_for_channel(&other, Some(&settings)));
    }

    #[test]
    fn regex_pattern_matches_error_message() {
        // Go retry.go:57-64 — regexp.MatchString.
        let settings = settings_with(
            vec![],
            vec![RetryableErrorPattern {
                pattern: r"model .* is busy".to_string(),
                regex: true,
            }],
        );
        let err = ConduitError::upstream("model gpt-x is busy");
        assert!(is_retryable_error_for_channel(&err, Some(&settings)));
    }

    #[test]
    fn invalid_regex_is_ignored_not_fatal() {
        // Go ignores regexp compile errors (regexErr == nil && matched).
        let settings = settings_with(
            vec![],
            vec![
                RetryableErrorPattern {
                    pattern: "([".to_string(), // invalid regex
                    regex: true,
                },
                RetryableErrorPattern {
                    pattern: "fallback-match".to_string(),
                    regex: false,
                },
            ],
        );
        let err = ConduitError::upstream("fallback-match happened");
        assert!(is_retryable_error_for_channel(&err, Some(&settings)));

        let no_match = ConduitError::upstream("nothing here");
        assert!(!is_retryable_error_for_channel(&no_match, Some(&settings)));
    }

    #[test]
    fn empty_pattern_entries_are_skipped() {
        // Go retry.go:53-55 — pattern.Pattern == "" -> continue.
        let settings = settings_with(
            vec![],
            vec![RetryableErrorPattern {
                pattern: String::new(),
                regex: false,
            }],
        );
        let err = ConduitError::upstream("anything");
        assert!(!is_retryable_error_for_channel(&err, Some(&settings)));
    }

    #[test]
    fn pattern_can_match_provider_code_text() {
        // The match text embeds the provider code (Go llm.ResponseError.Error()
        // includes ", code: <code>", llm/model.go:851-855).
        let settings = settings_with(
            vec![],
            vec![RetryableErrorPattern {
                pattern: "insufficient_quota".to_string(),
                regex: false,
            }],
        );
        let err = ConduitError::upstream("quota problem").with_code("insufficient_quota");
        assert!(is_retryable_error_for_channel(&err, Some(&settings)));
    }

    #[test]
    fn channel_retry_hook_wires_three_source_judgment() {
        let hook = channel_retry_hook(Some(settings_with(vec![402], vec![])));
        assert!(hook(&upstream_with_status(500)), "default set via hook");
        assert!(hook(&upstream_with_status(402)), "channel code via hook");
        assert!(!hook(&upstream_with_status(400)), "non-retryable via hook");

        let bare = channel_retry_hook(None);
        assert!(bare(&upstream_with_status(429)));
        assert!(!bare(&upstream_with_status(400)));
    }

    // =========================================================================
    // RUST-P15-001 — Go channel_retryable_test.go golden cases.
    // The Go test file defines a test-local `ChannelRetryableWrapper` type
    // (channel_retryable_test.go:17-155) with its own `CanRetry`/`PrepareForRetry`/
    // `ResetRetries`/`ExtractStatusCodeFromError` methods. The Rust port maps the
    // retryability DECISION onto the functions in this module (production parity
    // with `retry.go`) and the retry BUDGET onto `RetryPolicy`+`RetryState`
    // (`pipeline.rs`). These tests mirror the pure-logic assertions from the Go
    // test file that are missing from or only partially covered by the tests
    // above, with explicit Go subtest citations.
    // =========================================================================

    /// Mirrors `TestChannelRetryableWrapper_CanRetry`
    /// (`conduit/llm/pipeline/channel_retryable_test.go:186-249`).
    ///
    /// The existing `default_set_matches_go_is_http_status_code_retryable` covers
    /// most codes but omits the Go subtests for **401 (unauthorized)** and **403
    /// (forbidden)** (Go lines 200-208). This test enumerates the FULL Go subtest
    /// table so a future regression on any one code is traceable to its Go
    /// golden case. Each assertion is annotated with the Go subtest name.
    #[test]
    fn channel_retryable_can_retry_full_go_status_code_table() {
        // "should retry on 429 (rate limiting)" — Go L190-193.
        assert!(
            is_http_status_code_retryable(429),
            "429 rate limiting -> retryable"
        );
        // "should not retry on 400 (bad request)" — Go L195-198.
        assert!(!is_http_status_code_retryable(400));
        // "should not retry on 401 (unauthorized)" — Go L200-203.
        assert!(
            !is_http_status_code_retryable(401),
            "401 unauthorized -> not retryable"
        );
        // "should not retry on 403 (forbidden)" — Go L205-208.
        assert!(
            !is_http_status_code_retryable(403),
            "403 forbidden -> not retryable"
        );
        // "should not retry on 404 (not found)" — Go L210-213.
        assert!(!is_http_status_code_retryable(404));
        // "should retry on 500 (internal server error)" — Go L215-218.
        assert!(is_http_status_code_retryable(500));
        // "should retry on 502 (bad gateway)" — Go L220-223.
        assert!(is_http_status_code_retryable(502));
        // "should retry on 503 (service unavailable)" — Go L225-228.
        assert!(is_http_status_code_retryable(503));
    }

    /// Mirrors `TestChannelRetryableWrapper_CanRetry` subtest "should use custom
    /// retry logic when provided" (`channel_retryable_test.go:241-248`).
    ///
    /// The Go test wraps the outbound with `NewChannelRetryableWrapperWithCustomLogic`
    /// supplying a `canRetryFunc` that matches on error MESSAGE text, then asserts
    /// it overrides the default HTTP-status-code checking: an error with message
    /// "custom retryable error" is retryable even though it carries no status,
    /// while an "HTTP error 500" (normally retryable by the default set) is NOT
    /// retryable because the custom function rejects it.
    ///
    /// In Rust, the same override is expressed by supplying a custom [`CanRetryFn`]
    /// closure to [`RetryHooks`](crate::pipeline::RetryHooks) — the pipeline
    /// consults it instead of the default retryability check. This test mirrors
    /// the Go assertion at the `CanRetryFn` level.
    #[test]
    fn channel_retryable_custom_retry_logic_overrides_default() {
        // Go: canRetryFunc returns true ONLY for "custom retryable error".
        let custom_hook: CanRetryFn =
            Arc::new(|err: &ConduitError| err.message == "custom retryable error");

        // Go: require.True(t, customWrapper.CanRetry(errors.New("custom retryable error")))
        assert!(
            custom_hook(&ConduitError::upstream("custom retryable error")),
            "custom logic must accept the matching error"
        );
        // Go: require.False(t, customWrapper.CanRetry(errors.New("HTTP error 500")))
        // — even though 500 is in the default retryable set, the custom function
        // rejects it because the message does not match.
        assert!(
            !custom_hook(&upstream_with_status(500)),
            "custom logic must reject non-matching error even if 500 is default-retryable"
        );
    }

    /// Mirrors `TestChannelRetryableWrapper_CanRetry` subtest "should not retry
    /// when max retries exhausted" (`channel_retryable_test.go:235-239`).
    ///
    /// The Go test sets `wrapper.currentRetries = maxRetries` then asserts
    /// `CanRetry` returns false even for a 500 error. In Rust the budget gate
    /// lives in [`decide_retry`](crate::pipeline::decide_retry), not in the
    /// `CanRetryFn` itself — so the assertion is that `decide_retry` returns
    /// [`RetryDecision::Stop`] when `single_channel_retries >=
    /// max_single_channel_retries` AND no channel-switch budget remains, even
    /// though the outcome says `can_retry_same_channel: true`.
    #[test]
    fn channel_retryable_budget_exhausted_blocks_same_channel_retry() {
        // Go: currentRetries=3 (= maxRetries), CanRetry("HTTP error 500") -> false.
        // Rust: single_channel_retries at budget, can_retry=true, no more channels.
        let policy = RetryPolicy::DEFAULT;
        let state = RetryState {
            channel_switches: policy.max_channel_retries, // channel budget exhausted too
            single_channel_retries: policy.max_single_channel_retries,
        };
        let outcome = AttemptOutcome {
            is_timeout_error: false,
            can_retry_same_channel: true, // Go wrapper would say true for 500
            has_more_channels: true,
        };
        assert_eq!(
            decide_retry(policy, state, outcome, false),
            RetryDecision::Stop,
            "budget exhausted must block retry even when can_retry_same_channel is true"
        );
    }

    /// Documents a parity divergence between the Go test-local
    /// `ChannelRetryableWrapper.CanRetry` and the Rust production code.
    ///
    /// **Go test-local wrapper** (`channel_retryable_test.go:125-144`): if the
    /// status code cannot be extracted (0), the wrapper returns `true` — "allow
    /// retry for backward compatibility" (Go line 143). The Go subtest "should
    /// retry on unknown errors (backward compatibility)" (Go L230-233) asserts
    /// this.
    ///
    /// **Go production** (`retry.go:16-22` + `httpclient/utils.go:149-163`):
    /// `isRetryableError` calls `IsHTTPStatusCodeRetryable(0)` which returns
    /// `false` — unknown errors are NOT retryable by the default set.
    ///
    /// **Rust production** (this module): matches the Go PRODUCTION code —
    /// `is_retryable_error` returns `false` for errors without a `provider_status`.
    /// This is correct: the test-local wrapper's backward-compat behavior was
    /// specific to that wrapper, not to the production retry decision.
    #[test]
    fn channel_retryable_unknown_error_backward_compat_divergence() {
        // The Go test asserts retryable for "some unknown error" (Go L231-232).
        // The Rust PRODUCTION code correctly returns false — `extract_status_code`
        // yields 0, `is_http_status_code_retryable(0)` is false.
        let unknown = ConduitError::upstream("some unknown error");
        assert_eq!(extract_status_code_from_error(&unknown), 0);
        assert!(
            !is_retryable_error(&unknown),
            "production code: unknown errors (no status) are NOT retryable by default \
             (diverges from Go test-local wrapper backward-compat, matches Go production)"
        );
    }

    /// Documents a parity divergence in `ExtractStatusCodeFromError`.
    ///
    /// **Go test-local** (`channel_retryable_test.go:83-107`): parses the error
    /// MESSAGE with regex `HTTP error (\d{3})` to extract the status code. The
    /// Go subtest "extract from HTTP error message" (Go L299-302) feeds
    /// `errors.New("HTTP error 404")` and expects 404.
    ///
    /// **Go production** (`retry.go:75-91`): reads only typed errors
    /// (`httpclient.Error.StatusCode`, `llm.ResponseError.StatusCode`); message
    /// strings are NOT parsed. Returns 0 for unrecognized errors.
    ///
    /// **Rust production** (this module): matches the Go PRODUCTION code — reads
    /// `ConduitError::provider_status` only. An error with `"HTTP error 404"` in
    /// its message but no `provider_status` returns 0. This is correct: the
    /// regex extraction was specific to the test-local wrapper.
    #[test]
    fn extract_status_code_does_not_parse_message_strings() {
        // Go test asserts: errors.New("HTTP error 404") -> 404 (regex parse).
        // Rust production: no provider_status -> 0 (no regex parsing).
        let msg_err = ConduitError::upstream("HTTP error 404");
        assert_eq!(
            extract_status_code_from_error(&msg_err),
            0,
            "production code does not regex-parse error messages for status codes \
             (diverges from Go test-local wrapper, matches Go production retry.go:75-91)"
        );

        // But a typed provider_status IS extracted (Go production parity).
        let typed_err = ConduitError::upstream("HTTP error 422").with_provider_status(422);
        assert_eq!(extract_status_code_from_error(&typed_err), 422);

        // Go subtest "return 0 for nil error" (Go L309-311) — N/A in Rust (no nil;
        // ConduitError is always constructed). Transport error without status -> 0.
        assert_eq!(
            extract_status_code_from_error(&ConduitError::upstream("some other error")),
            0,
            "unrecognized error without provider_status -> 0"
        );
    }

    /// Mirrors `TestChannelRetryableWrapper_PrepareForRetry`
    /// (`channel_retryable_test.go:251-260`) and `TestChannelRetryableWrapper_ResetRetries`
    /// (`channel_retryable_test.go:262-270`).
    ///
    /// The Go test verifies the wrapper's `currentRetries` counter: PrepareForRetry
    /// increments it by 1; ResetRetries sets it back to 0. In Rust the analog is
    /// [`RetryContext`](crate::pipeline::RetryContext), which accumulates the
    /// same counter as `single_channel_attempt`:
    /// - `record_failure(_, RetrySameChannel)` → `single_channel_attempt += 1`
    ///   (PrepareForRetry analog).
    /// - `record_failure(_, RetryNextChannel)` → `single_channel_attempt = 0`
    ///   (ResetRetries analog — Go resets on channel switch, pipeline.go:324).
    #[test]
    fn channel_retryable_prepare_for_retry_and_reset_via_retry_context() {
        // Go: initialRetries := wrapper.currentRetries (0).
        let mut ctx = RetryContext::new(1_700_000_000_000);
        assert_eq!(ctx.single_channel_attempt, 0);

        // Go: PrepareForRetry -> currentRetries == initialRetries + 1.
        ctx.record_failure("upstream_error", RetryDecision::RetrySameChannel);
        assert_eq!(
            ctx.single_channel_attempt, 1,
            "PrepareForRetry analog: same-channel retry increments counter"
        );

        // Go: currentRetries = 2; ResetRetries -> currentRetries == 0.
        ctx.record_failure("upstream_error", RetryDecision::RetrySameChannel);
        assert_eq!(ctx.single_channel_attempt, 2);
        ctx.record_failure("upstream_error", RetryDecision::RetryNextChannel);
        assert_eq!(
            ctx.single_channel_attempt, 0,
            "ResetRetries analog: channel switch resets counter"
        );
    }
}
