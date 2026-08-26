//! P1-002 S08 — apply the upstream-error policy on the orchestrator's error
//! paths.
//!
//! Go contract: `conduit/internal/server/api/upstream_error_policy.go` defines
//! `applyUpstreamErrorPolicy(ctx, err, systemService)` and the streaming
//! `upstreamErrorStream.Err()` wrapper. Both call-sites read
//! `systemService.RetryPolicyOrDefault(ctx).UpstreamErrorPolicy` and, depending
//! on `policy.Mode`, either:
//! * `passthrough` — leave the upstream error untouched (also the default / `""`
//!   fast-path, Go line 44);
//! * `hidden` — replace the public message with `DefaultUpstreamErrorMessage`
//!   and strip the provider body / headers (`ConduitError::hide_upstream_details`);
//! * `custom` — replace the public message with `policy.CustomMessage` (falling
//!   back to the default when blank) and strip provider body / headers
//!   (`ConduitError::with_custom_upstream_message`).
//!
//! Two error categories are **exempt** from the policy (Go lines 38-41 + 62-64):
//! * `QuotaExhaustedError` (typed) — surfaced as-is so callers see the quota
//!   reason;
//! * `ResponseError` whose `Detail.Code == "quota_exhausted"` or
//!   `Detail.Type == "quota_exhausted"`. In Rust the equivalent marker is
//!   `ConduitError::kind == ErrorKind::QuotaExhausted` (set by
//!   `ConduitError::quota_exhausted`), which is what [`should_apply_policy`]
//!   checks.
//!
//! Wiring architecture: the orchestrator crate is a *pure decision layer* — it
//! does not own the HTTP response nor the live stream. The helpers below take
//! the resolved policy (mode + optional custom message) and produce the
//! transformed [`ConduitError`] / stream-error message. The host wiring layer
//! (api/server) calls them at the same two points Go does:
//! * non-stream — on every `Err` leaving `process_command`
//!   (Go `transformOrchestratorError`, line 22);
//! * stream — on the stream wrapper's terminal error
//!   (Go `upstreamErrorStream.Err()`, lines 125-137).

use conduit_core::{ConduitError, ErrorKind, UpstreamErrorPolicy, UpstreamErrorPolicyMode};
use serde_json::Value;

use crate::orchestrator::{
    ProcessRetryPolicy, UPSTREAM_ERROR_MODE_CUSTOM, UPSTREAM_ERROR_MODE_HIDDEN,
};

/// The default safe message the `hidden` / blank-`custom` modes surface.
/// Mirrors Go `biz.DefaultUpstreamErrorMessage`
/// (`conduit/internal/server/biz/system.go:307`).
pub const DEFAULT_UPSTREAM_ERROR_MESSAGE: &str =
    "Upstream provider request failed. Please try again later.";

/// Parse Go's `policy.Mode` string into the typed [`UpstreamErrorPolicyMode`].
/// Mirrors Go's handling where `""` / `"passthrough"` both fall through as the
/// no-op default (Go line 44: `policy.Mode == biz.UpstreamErrorModePassthrough ||
/// policy.Mode == ""`).
pub fn parse_policy_mode(raw: &str) -> UpstreamErrorPolicyMode {
    match raw {
        UPSTREAM_ERROR_MODE_HIDDEN => UpstreamErrorPolicyMode::Hidden,
        UPSTREAM_ERROR_MODE_CUSTOM => UpstreamErrorPolicyMode::Custom,
        // `""` and `"passthrough"` (and any unrecognized value, defensively)
        // resolve to passthrough — mirrors Go's `default`-to-passthrough.
        _ => UpstreamErrorPolicyMode::Passthrough,
    }
}

/// Build the [`UpstreamErrorPolicy`] the orchestrator applies, from the
/// Process-level retry policy's `upstream_error_mode` + optional custom message.
///
/// Mirrors Go reading `systemService.RetryPolicyOrDefault(ctx).UpstreamErrorPolicy`
/// (Go line 43). The wiring layer passes the resolved [`ProcessRetryPolicy`]
/// (already derived via [`crate::orchestrator::derive_retry_policy`]) plus the
/// system's custom upstream message string.
///
/// `custom_message` is taken as a `&str` (not `Option`) because Go's
/// `UpstreamErrorPolicy.CustomMessage` is a plain string; an empty string
/// resolves to [`DEFAULT_UPSTREAM_ERROR_MESSAGE`] inside
/// [`apply_upstream_error_policy`] (mirroring Go lines 53-57).
pub fn policy_from_retry_policy(
    retry_policy: &ProcessRetryPolicy,
    custom_message: &str,
) -> UpstreamErrorPolicy {
    match parse_policy_mode(retry_policy.upstream_error_mode) {
        UpstreamErrorPolicyMode::Passthrough => UpstreamErrorPolicy::passthrough(),
        UpstreamErrorPolicyMode::Hidden => UpstreamErrorPolicy::hidden(),
        UpstreamErrorPolicyMode::Custom => UpstreamErrorPolicy::custom(custom_message.to_string()),
    }
}

/// Whether the policy should transform `err`. Mirrors Go's two exemptions
/// (lines 38-41 `QuotaExhaustedError`, lines 62-64 quota-typed
/// `ResponseError`). In Rust both surface as `ErrorKind::QuotaExhausted`
/// (`ConduitError::quota_exhausted` builder sets that kind).
///
/// Also returns `false` for a non-upstream error when the policy is passthrough
/// (the common fast-path — Go returns early at line 44 before the
/// `pipeline.IsUpstreamError` check). We mirror the *quota* exemption
/// unconditionally (quota errors are never rewritten) and let the caller's
/// mode check handle the passthrough short-circuit, exactly as Go splits the
/// two guards across two early returns.
pub fn should_apply_policy(err: &ConduitError) -> bool {
    // Quota-exhausted errors are always exempt (Go lines 38-41 + 62-64).
    !matches!(err.kind, ErrorKind::QuotaExhausted)
}

/// Apply the upstream-error policy to a non-stream [`ConduitError`].
///
/// Mirrors Go `applyUpstreamErrorPolicy(ctx, err, systemService)`
/// (`upstream_error_policy.go:33-97`). The wiring layer calls this on every
/// error leaving `process_command` before transforming it into the HTTP
/// response (Go `transformOrchestratorError`, line 22).
///
/// Semantics:
/// * `Passthrough` — return `err` unchanged (Go line 44 fast-path);
/// * `Hidden` — replace the public message with [`DEFAULT_UPSTREAM_ERROR_MESSAGE`]
///   and strip provider body / headers
///   ([`ConduitError::hide_upstream_details`]);
/// * `Custom` — replace the public message with `policy.custom_message`
///   (falling back to the default when blank, Go lines 53-57) and strip
///   provider body / headers
///   ([`ConduitError::with_custom_upstream_message`]).
///
/// Quota-exhausted errors bypass the policy entirely
/// ([`should_apply_policy`]), matching Go's typed-quota exemption.
pub fn apply_upstream_error_policy(
    policy: &UpstreamErrorPolicy,
    err: ConduitError,
) -> ConduitError {
    // Go line 38-41 + 62-64: quota-exhausted errors are never rewritten.
    if !should_apply_policy(&err) {
        return err;
    }

    match policy.mode {
        UpstreamErrorPolicyMode::Passthrough => err,
        UpstreamErrorPolicyMode::Hidden => {
            // Go line 52 sets `message := biz.DefaultUpstreamErrorMessage` for
            // BOTH hidden and custom, then rewrites the surfaced message. We use
            // [`with_custom_upstream_message`] (not `hide_upstream_details`) so
            // the surfaced message is the *full* Go default rather than the
            // shorter `ErrorKind::Upstream.default_safe_message` ("Upstream
            // provider error"). Both helpers strip provider body / headers.
            err.with_custom_upstream_message(DEFAULT_UPSTREAM_ERROR_MESSAGE)
        }
        UpstreamErrorPolicyMode::Custom => {
            // Go lines 53-57: blank custom message falls back to the default.
            let message = policy
                .custom_message
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| DEFAULT_UPSTREAM_ERROR_MESSAGE.to_string());
            err.with_custom_upstream_message(message)
        }
    }
}

/// Apply the upstream-error policy to the streaming wrapper's terminal error
/// message.
///
/// Mirrors Go `upstreamErrorStream.Err()` (`upstream_error_policy.go:125-137`):
/// the stream wrapper reads its inner stream's `Err()`, and — when the policy
/// mode is not `""` / passthrough — wraps the raw error via
/// `pipeline.WrapUpstreamError(err)` then routes it through
/// `applyUpstreamErrorPolicy`. The Rust stream wrapper
/// ([`crate::outbound_stream::ObservedStreamState::stream_error`]) carries the
/// raw error as a `String`; this helper returns the message the wiring layer
/// should surface to the client (and record on the failure row).
///
/// Returns:
/// * passthrough mode, empty mode, or `None` input — `None` (no transformation);
/// * hidden / custom mode — `Some(DEFAULT_UPSTREAM_ERROR_MESSAGE)` or
///   `Some(custom_message)`, respectively (with the blank-custom fallback).
///
/// Quota-exhausted stream errors are NOT exempted at this layer because the
/// stream wrapper surfaces raw error strings (not typed `ConduitError`s); the Go
/// stream path likewise applies the policy unconditionally once the mode is
/// non-passthrough (the quota exemption lives on the typed-error path, which
/// the non-stream [`apply_upstream_error_policy`] guards).
pub fn apply_upstream_error_policy_stream(
    policy: &UpstreamErrorPolicy,
    stream_error: Option<&str>,
) -> Option<String> {
    let raw = stream_error?;
    // Go line 132: only apply when mode is neither "" nor passthrough.
    if matches!(policy.mode, UpstreamErrorPolicyMode::Passthrough) {
        return Some(raw.to_string());
    }

    let message = match policy.mode {
        UpstreamErrorPolicyMode::Hidden => DEFAULT_UPSTREAM_ERROR_MESSAGE.to_string(),
        UpstreamErrorPolicyMode::Custom => policy
            .custom_message
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| DEFAULT_UPSTREAM_ERROR_MESSAGE.to_string()),
        UpstreamErrorPolicyMode::Passthrough => raw.to_string(),
    };
    Some(message)
}

pub fn upstream_passthrough_error<I, K, V>(
    message: impl Into<String>,
    provider_status: u16,
    provider_body: Value,
    provider_headers: I,
) -> ConduitError
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: Into<String>,
{
    ConduitError::upstream(message)
        .with_provider_status(provider_status)
        .with_provider_body(provider_body)
        .with_provider_headers(provider_headers)
}

// ===========================================================================
// Tests — mirror the Go `upstream_error_policy.go` call-site semantics.
// There is no Go `upstream_error_policy_test.go`; these tests pin the
// Passthrough/Hidden/Custom behavior on both the non-stream ([`apply_upstream_
// error_policy`]) and stream ([`apply_upstream_error_policy_stream`]) paths,
// including the quota-exhausted exemption and the blank-custom fallback.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::UPSTREAM_ERROR_MODE_PASSTHROUGH;
    use conduit_core::ConduitError;

    /// Build an upstream error carrying provider detail that Hidden/Custom must
    /// strip.
    fn upstream_with_provider_detail() -> ConduitError {
        ConduitError::upstream("provider returned 503")
            .with_provider_status(503)
            .with_provider_body(serde_json::json!({"error":{"message":"backend down"}}))
            .with_provider_headers([("x-request-id", "upstream-req-123")])
    }

    // ---- parse_policy_mode (Go `policy.Mode` string handling) ----

    #[test]
    fn parse_policy_mode_hidden() {
        assert_eq!(
            parse_policy_mode(UPSTREAM_ERROR_MODE_HIDDEN),
            UpstreamErrorPolicyMode::Hidden
        );
    }

    #[test]
    fn parse_policy_mode_custom() {
        assert_eq!(
            parse_policy_mode(UPSTREAM_ERROR_MODE_CUSTOM),
            UpstreamErrorPolicyMode::Custom
        );
    }

    #[test]
    fn parse_policy_mode_passthrough_and_blank_default_to_passthrough() {
        // Go line 44 treats both `""` and `"passthrough"` as the no-op default.
        assert_eq!(
            parse_policy_mode(UPSTREAM_ERROR_MODE_PASSTHROUGH),
            UpstreamErrorPolicyMode::Passthrough
        );
        assert_eq!(parse_policy_mode(""), UpstreamErrorPolicyMode::Passthrough);
        // Unrecognized values defensively resolve to passthrough.
        assert_eq!(
            parse_policy_mode("bogus"),
            UpstreamErrorPolicyMode::Passthrough
        );
    }

    // ---- policy_from_retry_policy (Go reads systemService.RetryPolicy) ----

    #[test]
    fn policy_from_retry_policy_passthrough_default() {
        let rp = ProcessRetryPolicy::default();
        let policy = policy_from_retry_policy(&rp, "");
        assert_eq!(policy.mode, UpstreamErrorPolicyMode::Passthrough);
        assert!(policy.custom_message.is_none());
    }

    #[test]
    fn policy_from_retry_policy_hidden() {
        let rp = ProcessRetryPolicy {
            upstream_error_mode: UPSTREAM_ERROR_MODE_HIDDEN,
            ..ProcessRetryPolicy::default()
        };
        let policy = policy_from_retry_policy(&rp, "ignored");
        assert_eq!(policy.mode, UpstreamErrorPolicyMode::Hidden);
    }

    #[test]
    fn policy_from_retry_policy_custom_carries_message() {
        let rp = ProcessRetryPolicy {
            upstream_error_mode: UPSTREAM_ERROR_MODE_CUSTOM,
            ..ProcessRetryPolicy::default()
        };
        let policy = policy_from_retry_policy(&rp, "please contact support");
        assert_eq!(policy.mode, UpstreamErrorPolicyMode::Custom);
        assert_eq!(
            policy.custom_message.as_deref(),
            Some("please contact support")
        );
    }

    // ---- apply_upstream_error_policy: non-stream path ----

    #[test]
    fn non_stream_passthrough_leaves_detail_intact() -> Result<(), Box<dyn std::error::Error>> {
        let policy = UpstreamErrorPolicy::passthrough();
        let err = upstream_with_provider_detail();
        let out = apply_upstream_error_policy(&policy, err);
        // Passthrough: message, provider body, headers all preserved.
        assert_eq!(out.message, "provider returned 503");
        assert!(
            out.provider_body.is_some(),
            "passthrough keeps provider body"
        );
        assert!(
            !out.provider_headers_subset.is_empty(),
            "passthrough keeps provider headers"
        );
        Ok(())
    }

    #[test]
    fn non_stream_hidden_strips_upstream_detail_and_uses_default_message()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = UpstreamErrorPolicy::hidden();
        let err = upstream_with_provider_detail();
        let out = apply_upstream_error_policy(&policy, err);
        // Hidden: safe message replaced with the default; provider detail stripped.
        assert_eq!(
            out.safe_message.as_deref(),
            Some(DEFAULT_UPSTREAM_ERROR_MESSAGE),
            "hidden uses the default upstream message"
        );
        assert!(out.provider_body.is_none(), "hidden strips provider body");
        assert!(
            out.provider_headers_subset.is_empty(),
            "hidden strips provider headers"
        );
        Ok(())
    }

    #[test]
    fn non_stream_custom_replaces_message_and_strips_detail()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = UpstreamErrorPolicy::custom("admin override message");
        let err = upstream_with_provider_detail();
        let out = apply_upstream_error_policy(&policy, err);
        assert_eq!(
            out.safe_message.as_deref(),
            Some("admin override message"),
            "custom replaces the public message"
        );
        assert!(out.provider_body.is_none(), "custom strips provider body");
        assert!(
            out.provider_headers_subset.is_empty(),
            "custom strips provider headers"
        );
        Ok(())
    }

    #[test]
    fn non_stream_custom_blank_message_falls_back_to_default()
    -> Result<(), Box<dyn std::error::Error>> {
        // Go lines 53-57: blank custom message falls back to DefaultUpstreamErrorMessage.
        let policy = UpstreamErrorPolicy::custom("   ");
        let err = upstream_with_provider_detail();
        let out = apply_upstream_error_policy(&policy, err);
        assert_eq!(
            out.safe_message.as_deref(),
            Some(DEFAULT_UPSTREAM_ERROR_MESSAGE),
            "blank custom message falls back to the default"
        );
        Ok(())
    }

    #[test]
    fn non_stream_quota_exhausted_is_exempt_from_policy() -> Result<(), Box<dyn std::error::Error>>
    {
        // Mirrors Go lines 38-41 + 62-64: quota-exhausted errors are never rewritten.
        let policy = UpstreamErrorPolicy::hidden();
        let err = ConduitError::quota_exhausted("all channels quota exhausted for model gpt-4");
        let out = apply_upstream_error_policy(&policy, err);
        assert_eq!(
            out.message, "all channels quota exhausted for model gpt-4",
            "quota error must pass through unchanged even under hidden mode"
        );
        assert_eq!(out.kind, ErrorKind::QuotaExhausted);
        Ok(())
    }

    // ---- apply_upstream_error_policy_stream: stream path ----

    #[test]
    fn stream_passthrough_returns_raw_message() {
        let policy = UpstreamErrorPolicy::passthrough();
        let out = apply_upstream_error_policy_stream(&policy, Some("upstream connection reset"));
        assert_eq!(out.as_deref(), Some("upstream connection reset"));
    }

    #[test]
    fn stream_hidden_replaces_with_default_message() {
        let policy = UpstreamErrorPolicy::hidden();
        let out = apply_upstream_error_policy_stream(&policy, Some("upstream connection reset"));
        assert_eq!(out.as_deref(), Some(DEFAULT_UPSTREAM_ERROR_MESSAGE));
    }

    #[test]
    fn stream_custom_replaces_with_custom_message() {
        let policy = UpstreamErrorPolicy::custom("admin override");
        let out = apply_upstream_error_policy_stream(&policy, Some("upstream connection reset"));
        assert_eq!(out.as_deref(), Some("admin override"));
    }

    #[test]
    fn stream_custom_blank_falls_back_to_default() {
        let policy = UpstreamErrorPolicy::custom("");
        let out = apply_upstream_error_policy_stream(&policy, Some("upstream connection reset"));
        assert_eq!(out.as_deref(), Some(DEFAULT_UPSTREAM_ERROR_MESSAGE));
    }

    #[test]
    fn stream_none_input_returns_none() {
        // Go `upstreamErrorStream.Err()` returns nil when the inner stream's Err() is nil.
        let policy = UpstreamErrorPolicy::hidden();
        assert!(apply_upstream_error_policy_stream(&policy, None).is_none());
    }

    #[test]
    fn stream_passthrough_with_none_returns_none() {
        let policy = UpstreamErrorPolicy::passthrough();
        assert!(apply_upstream_error_policy_stream(&policy, None).is_none());
    }
}
