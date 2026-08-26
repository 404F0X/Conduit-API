//! Upstream-error marking and exposure policy (RUST-P8-002 S18).
//!
//! Ports two Go pieces:
//!
//! 1. **`UpstreamError` marker** — Go `llm/pipeline/upstream_error.go:1-42`.
//!    The pipeline wraps errors that originate from the *upstream provider
//!    path* (executor call, outbound response/stream transform) in
//!    `&UpstreamError{Err: err}` so the API layer can tell provider failures
//!    apart from Conduit API's own errors. `WrapUpstreamError` is idempotent
//!    (`errors.As` check, `upstream_error.go:31-34`); `IsUpstreamError` is the
//!    probe (`upstream_error.go:39-42`). `ConduitError` cannot nest, so the Rust
//!    marker is a metadata flag ([`UPSTREAM_ERROR_MARKER`]) set by
//!    [`wrap_upstream_error`] and probed by [`is_upstream_error`].
//!
//! 2. **`UpstreamErrorPolicy` application** — Go
//!    `internal/server/api/upstream_error_policy.go:33-97`
//!    (`applyUpstreamErrorPolicy`). When all candidates fail, the pipeline
//!    returns the **last** error verbatim (Go `pipeline.go:288` `lastErr = err`
//!    → `pipeline.go:355` `return nil, lastErr`); the API layer then decides
//!    how much of that error to expose:
//!    - `passthrough` (or empty) mode → unchanged (`:44-46`);
//!    - non-upstream-marked errors → unchanged (`:48-50`);
//!    - quota-exhausted errors → exempt, never masked (`:38-41`, `:62-64`);
//!    - `hidden` → replace the message with
//!      [`DEFAULT_UPSTREAM_ERROR_MESSAGE`] (Go `biz.DefaultUpstreamErrorMessage`,
//!      `internal/server/biz/system.go:307`);
//!    - `custom` → the trimmed admin message, falling back to the default when
//!      blank (`:52-58`).
//!
//!    In every masked branch Go **keeps** StatusCode / Type / Code / RequestID
//!    and replaces only the human message (`:66-75`, `:79-88`); the Rust analog
//!    keeps `kind` / `provider_status` / `code` and rewrites `safe_message`
//!    while stripping the provider body/header details
//!    (`ConduitError::with_custom_upstream_message`).
//!
//! The policy type itself ([`UpstreamErrorPolicy`],
//! [`UpstreamErrorPolicyMode`]) is already ported in `conduit-core::error`;
//! this module adds the pipeline-side wiring.

use conduit_core::{ConduitError, ErrorKind, UpstreamErrorPolicy, UpstreamErrorPolicyMode};

/// Metadata key marking an error as originating from the upstream provider
/// path. Rust stand-in for Go's `*UpstreamError` wrapper type
/// (`llm/pipeline/upstream_error.go:6-9`).
pub const UPSTREAM_ERROR_MARKER: &str = "upstream_error_marker";

/// Default masked message. Mirrors Go `biz.DefaultUpstreamErrorMessage`
/// (`internal/server/biz/system.go:307`).
pub const DEFAULT_UPSTREAM_ERROR_MESSAGE: &str =
    "Upstream provider request failed. Please try again later.";

/// Mark an error as upstream-originated. Mirrors Go `WrapUpstreamError`
/// (`upstream_error.go:26-37`): `nil`-safe by construction (no `Option` here —
/// Go's nil check guards its `error` interface) and **idempotent** — an
/// already-marked error passes through unchanged (Go `errors.As` short-circuit
/// at `upstream_error.go:31-34`). All other fields (message, provider status,
/// code, body) are preserved.
pub fn wrap_upstream_error(mut err: ConduitError) -> ConduitError {
    if is_upstream_error(&err) {
        return err;
    }
    err.metadata.insert(
        UPSTREAM_ERROR_MARKER.to_string(),
        serde_json::Value::Bool(true),
    );
    err
}

/// Whether the error is marked upstream-originated. Mirrors Go
/// `IsUpstreamError` (`upstream_error.go:39-42`).
pub fn is_upstream_error(err: &ConduitError) -> bool {
    matches!(
        err.metadata.get(UPSTREAM_ERROR_MARKER),
        Some(serde_json::Value::Bool(true))
    )
}

/// Whether the error is a quota-exhausted error, which the policy never masks.
/// Mirrors Go's two exemptions: the `*QuotaExhaustedError` type check
/// (`upstream_error_policy.go:38-41`) and the response-error
/// code/type == "quota_exhausted" check (`:62-64`).
fn is_quota_exhausted(err: &ConduitError) -> bool {
    err.kind == ErrorKind::QuotaExhausted || err.code.as_deref() == Some("quota_exhausted")
}

/// Apply the upstream-error exposure policy to a final pipeline error.
/// Mirrors Go `applyUpstreamErrorPolicy`
/// (`internal/server/api/upstream_error_policy.go:33-97`); see the module docs
/// for the branch-by-branch mapping. Returns the error unchanged unless the
/// policy masks it.
pub fn apply_upstream_error_policy(
    policy: &UpstreamErrorPolicy,
    err: ConduitError,
) -> ConduitError {
    // Quota-exhausted errors are exempt (Go `:38-41` and `:62-64`).
    if is_quota_exhausted(&err) {
        return err;
    }

    // Passthrough (or unset) keeps provider errors unchanged (Go `:44-46`).
    if policy.mode == UpstreamErrorPolicyMode::Passthrough {
        return err;
    }

    // Only upstream-marked errors are masked (Go `:48-50`).
    if !is_upstream_error(&err) {
        return err;
    }

    // Resolve the masked message (Go `:52-58`): custom mode uses the trimmed
    // admin message, falling back to the default when blank; hidden mode uses
    // the default outright.
    let message = match policy.mode {
        UpstreamErrorPolicyMode::Custom => {
            let trimmed = policy
                .custom_message
                .as_deref()
                .map(str::trim)
                .unwrap_or("");
            if trimmed.is_empty() {
                DEFAULT_UPSTREAM_ERROR_MESSAGE.to_string()
            } else {
                trimmed.to_string()
            }
        }
        _ => DEFAULT_UPSTREAM_ERROR_MESSAGE.to_string(),
    };

    // Replace only the exposed message; keep kind/status/code (Go keeps
    // StatusCode/Type/Code/RequestID, `:66-75`/`:79-88`) and strip raw
    // provider details (body/headers) from the outward-facing error.
    err.with_custom_upstream_message(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream_err() -> ConduitError {
        wrap_upstream_error(
            ConduitError::upstream("provider exploded: secret detail")
                .with_provider_status(502)
                .with_code("server_error")
                .with_provider_body(serde_json::json!({"error": {"message": "secret detail"}})),
        )
    }

    // -- marker (Go WrapUpstreamError / IsUpstreamError) ----------------------

    #[test]
    fn wrap_marks_and_is_idempotent() {
        let plain = ConduitError::upstream("boom");
        assert!(!is_upstream_error(&plain), "unmarked by default");

        let wrapped = wrap_upstream_error(plain);
        assert!(is_upstream_error(&wrapped));
        let marked_len = wrapped.metadata.len();
        let marked_message = wrapped.message.clone();

        // Idempotent (Go errors.As short-circuit): double wrap leaves a single
        // marker and the error unchanged.
        let double = wrap_upstream_error(wrapped);
        assert!(is_upstream_error(&double));
        assert_eq!(double.metadata.len(), marked_len);
        assert_eq!(double.message, marked_message);
    }

    #[test]
    fn wrap_preserves_error_fields() {
        let err = wrap_upstream_error(
            ConduitError::upstream("boom")
                .with_provider_status(503)
                .with_code("overloaded"),
        );
        assert_eq!(err.provider_status, Some(503));
        assert_eq!(err.code.as_deref(), Some("overloaded"));
        assert_eq!(err.message, "boom");
    }

    // -- policy application (Go applyUpstreamErrorPolicy) --------------------

    #[test]
    fn passthrough_mode_keeps_error_unchanged() {
        // Go `:44-46`.
        let policy = UpstreamErrorPolicy::passthrough();
        let err = apply_upstream_error_policy(&policy, upstream_err());
        assert_eq!(err.message, "provider exploded: secret detail");
        assert!(err.provider_body.is_some(), "passthrough keeps details");
    }

    #[test]
    fn hidden_mode_masks_message_keeps_status_and_code() {
        // Go `:66-75`: message replaced, StatusCode/Type/Code kept.
        let policy = UpstreamErrorPolicy::hidden();
        let err = apply_upstream_error_policy(
            &policy,
            upstream_err().with_metadata(
                conduit_core::ERROR_RESPONSE_BODY_METADATA,
                serde_json::json!({"error": {"message": "channel override"}}),
            ),
        );
        assert_eq!(err.public_message(), DEFAULT_UPSTREAM_ERROR_MESSAGE);
        assert_eq!(err.provider_status, Some(502), "status preserved");
        assert_eq!(err.code.as_deref(), Some("server_error"), "code preserved");
        assert_eq!(err.kind, ErrorKind::Upstream, "kind (type) preserved");
        assert!(err.provider_body.is_none(), "raw provider body stripped");
        assert!(
            !err.metadata
                .contains_key(conduit_core::ERROR_RESPONSE_BODY_METADATA),
            "hidden policy must remove a channel-specific response body"
        );
    }

    #[test]
    fn custom_mode_uses_trimmed_message() {
        // Go `:53-58` — strings.TrimSpace(policy.CustomMessage).
        let policy = UpstreamErrorPolicy::custom("  Contact support.  ");
        let err = apply_upstream_error_policy(&policy, upstream_err());
        assert_eq!(err.public_message(), "Contact support.");
    }

    #[test]
    fn custom_mode_blank_message_falls_back_to_default() {
        // Go `:55-57` — empty after trim -> DefaultUpstreamErrorMessage.
        let policy = UpstreamErrorPolicy::custom("   ");
        let err = apply_upstream_error_policy(&policy, upstream_err());
        assert_eq!(err.public_message(), DEFAULT_UPSTREAM_ERROR_MESSAGE);
    }

    #[test]
    fn non_upstream_errors_are_never_masked() {
        // Go `:48-50` — IsUpstreamError gate. An internal error keeps its
        // message even in hidden mode.
        let policy = UpstreamErrorPolicy::hidden();
        let plain = ConduitError::invalid_request("bad payload");
        let err = apply_upstream_error_policy(&policy, plain);
        assert_eq!(err.public_message(), "bad payload");
    }

    #[test]
    fn quota_exhausted_is_exempt_even_when_upstream_marked() {
        // Go `:38-41` + `:62-64` — quota errors are surfaced verbatim.
        let policy = UpstreamErrorPolicy::hidden();
        let quota = wrap_upstream_error(ConduitError::quota_exhausted("monthly quota exceeded"));
        let err = apply_upstream_error_policy(&policy, quota);
        assert_eq!(err.public_message(), "monthly quota exceeded");
    }

    #[test]
    fn default_message_matches_go_constant() {
        // Go biz/system.go:307.
        assert_eq!(
            DEFAULT_UPSTREAM_ERROR_MESSAGE,
            "Upstream provider request failed. Please try again later."
        );
    }
}
