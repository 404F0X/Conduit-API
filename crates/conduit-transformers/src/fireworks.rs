//! Fireworks AI OpenAI-compatible wrapper (RUST-P7-008 S11/S12, round 5 —
//! final).
//!
//! Mirrors Go `conduit/llm/transformer/fireworks/outbound.go`. Fireworks is a
//! pure-constructor OpenAI-compatible wrapper: it does **not** override
//! `TransformRequest`/`TransformResponse` at all — every delta lives in
//! `NewOutboundTransformerWithConfig`, which just configures the standard
//! OpenAI transformer with Fireworks's base URL + reasoning field. The
//! deltas are:
//!
//! 1. **Default base URL** — `https://api.fireworks.ai/inference/v1` is used
//!    when `config.BaseURL` is empty. (outbound.go:13, 44-47)
//! 2. **Trailing-slash trim** — the base URL has any trailing `/` removed
//!    before being handed to the OpenAI transformer. (outbound.go:49)
//! 3. **Reasoning field** — Fireworks uses OpenAI's `reasoning_content`
//!    field (`openai.ReasoningFieldContent`), not the `reasoning` variant
//!    OpenRouter/NanoGPT use. (outbound.go:55)
//! 4. **Config validation** — rejects nil config and nil API key provider.
//!    (outbound.go:36-42)
//!
//! Everything else (URL normalization beyond the trim, bearer auth, JSON
//! headers, request-type gating, response handling) comes from the wrapped
//! OpenAI transformer / [`crate::openai_compatible`] shared base.
//!
//! Go has no dedicated `outbound_test.go` for Fireworks (the constructor is
//! the only logic); the deltas are verified here by direct unit tests.

use crate::openai_compatible::OpenAiCompatibleConfig;

/// Default Fireworks API base URL. Mirrors Go `DefaultBaseURL`
/// (outbound.go:13).
pub const DEFAULT_BASE_URL: &str = "https://api.fireworks.ai/inference/v1";

/// Fireworks provider configuration. Mirrors Go `fireworks.Config`
/// (outbound.go:15-21) — the OpenAI-compatible shape.
pub type Config = OpenAiCompatibleConfig;

/// Resolve the effective base URL: when `base_url` is empty, fall back to
/// [`DEFAULT_BASE_URL`]; then trim any trailing `/`. Mirrors Go
/// outbound.go:44-49:
///
/// ```go
/// baseURL := config.BaseURL
/// if baseURL == "" {
///     baseURL = DefaultBaseURL
/// }
/// baseURL = strings.TrimSuffix(baseURL, "/")
/// ```
pub fn resolve_base_url(base_url: &str) -> String {
    let resolved = if base_url.is_empty() {
        DEFAULT_BASE_URL
    } else {
        base_url
    };
    // Mirror Go's `strings.TrimSuffix(baseURL, "/")` — a single trailing
    // slash removal (real-world inputs carry at most one).
    let trimmed = resolved.strip_suffix('/').unwrap_or(resolved);
    trimmed.to_string()
}

/// Validate a Fireworks [`Config`], mirroring Go outbound.go:36-42:
///
/// ```go
/// if config == nil { return ... "config is nil" }
/// if config.APIKeyProvider == nil { return ... "API key provider is required" }
/// ```
///
/// In Rust the `Config` is a value (not a reference), so the nil-config check
/// is replaced by checking the fields the Go nil-guard protects: a non-empty
/// `api_key` is required. The base URL is allowed to be empty (it falls back
/// to the default in [`resolve_base_url`]).
pub fn validate_config(config: &Config) -> Result<(), conduit_core::ConduitError> {
    if config.api_key.is_empty() {
        return Err(conduit_core::ConduitError::invalid_request(
            "invalid Fireworks transformer configuration: API key provider is required",
        ));
    }
    Ok(())
}

/// The reasoning-field strategy Fireworks uses. Mirrors Go
/// `openai.ReasoningFieldContent` (outbound.go:55). This is a constant tag
/// the future live transformer impl reads to decide which response field
/// carries reasoning content.
pub const REASONING_FIELD: &str = "reasoning_content";

#[cfg(test)]
mod tests {
    use super::*;

    // =======================================================================
    // resolve_base_url (mirrors Go outbound.go:44-49)
    // =======================================================================
    #[test]
    fn empty_base_url_falls_back_to_default() {
        assert_eq!(resolve_base_url(""), DEFAULT_BASE_URL);
        assert_eq!(
            resolve_base_url(""),
            "https://api.fireworks.ai/inference/v1"
        );
    }

    #[test]
    fn non_empty_base_url_is_preserved() {
        assert_eq!(
            resolve_base_url("https://custom.fireworks.ai/v1"),
            "https://custom.fireworks.ai/v1"
        );
    }

    #[test]
    fn trailing_slash_is_trimmed() {
        assert_eq!(
            resolve_base_url("https://api.fireworks.ai/inference/v1/"),
            "https://api.fireworks.ai/inference/v1"
        );
    }

    #[test]
    fn empty_base_url_with_trailing_slash_falls_back_to_default_trimmed() {
        // Empty input → default; default has no trailing slash so the trim is
        // a noop.
        assert_eq!(resolve_base_url(""), DEFAULT_BASE_URL);
        assert!(!resolve_base_url("").ends_with('/'));
    }

    #[test]
    fn custom_base_url_with_trailing_slash_trimmed() {
        assert_eq!(
            resolve_base_url("https://custom.fireworks.ai/"),
            "https://custom.fireworks.ai"
        );
    }

    // =======================================================================
    // validate_config (mirrors Go outbound.go:36-42)
    // =======================================================================
    #[test]
    fn validate_config_rejects_empty_api_key() {
        let config = Config {
            base_url: String::new(),
            api_key: String::new(),
        };
        let err = match validate_config(&config) {
            Ok(()) => panic!("expected Err for empty api key"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("API key provider is required"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn validate_config_accepts_non_empty_api_key() -> Result<(), conduit_core::ConduitError> {
        let config = Config {
            base_url: String::new(),
            api_key: "fw-key".to_string(),
        };
        validate_config(&config)
    }

    #[test]
    fn validate_config_accepts_empty_base_url_since_default_applies()
    -> Result<(), conduit_core::ConduitError> {
        // The Go nil-config guard protects against the whole config being
        // absent; here an empty base URL is valid because resolve_base_url
        // fills in the default.
        let config = Config {
            base_url: String::new(),
            api_key: "fw-key".to_string(),
        };
        validate_config(&config)
    }

    // =======================================================================
    // REASONING_FIELD constant (mirrors Go outbound.go:55)
    // =======================================================================
    #[test]
    fn reasoning_field_is_reasoning_content() {
        // Fireworks uses the reasoning_content field (not the bare
        // `reasoning` variant OpenRouter/NanoGPT use).
        assert_eq!(REASONING_FIELD, "reasoning_content");
    }

    // =======================================================================
    // DEFAULT_BASE_URL constant (mirrors Go outbound.go:13)
    // =======================================================================
    #[test]
    fn default_base_url_matches_go_constant() {
        assert_eq!(DEFAULT_BASE_URL, "https://api.fireworks.ai/inference/v1");
    }
}
