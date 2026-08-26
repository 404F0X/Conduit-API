//! Cerebras OpenAI-compatible wrapper (RUST-P7-008 S11/S12, round 5 — final).
//!
//! Mirrors Go `conduit/llm/transformer/cerebras/outbound.go`. Cerebras wraps
//! the **OpenRouter** transformer (not OpenAI directly) and strips
//! OpenAI-specific fields Cerebras doesn't support. The deltas are:
//!
//! 1. **Default base URL** — `https://api.cerebras.ai/v1` is used when
//!    `config.BaseURL` is empty. (outbound.go:15, 44-47)
//! 2. **Wraps OpenRouter** — inherits OpenRouter's reasoning handling
//!    (`reasoning`/`reasoning_details` → `reasoning_content` mapping).
//!    (outbound.go:49-55)
//! 3. **`store` field strip** — Cerebras doesn't support the OpenAI `store`
//!    parameter, so `TransformRequest` clears it before delegating.
//!    (outbound.go:64-77)
//! 4. **Config validation** — rejects nil config and nil API key provider.
//!    (outbound.go:36-42)
//!
//! Everything else comes from the wrapped OpenRouter transformer (which in
//! turn composes [`crate::openai_compatible`]).
//!
//! Go has no dedicated `outbound_test.go` for Cerebras; the deltas are
//! verified here by direct unit tests.

use crate::openai_compatible::OpenAiCompatibleConfig;
use conduit_llm::LlmRequest;

/// Default Cerebras API base URL. Mirrors Go `DefaultBaseURL`
/// (outbound.go:15).
pub const DEFAULT_BASE_URL: &str = "https://api.cerebras.ai/v1";

/// Cerebras provider configuration. Mirrors Go `cerebras.Config`
/// (outbound.go:17-23) — the OpenAI-compatible shape.
pub type Config = OpenAiCompatibleConfig;

/// Resolve the effective base URL: when `base_url` is empty, fall back to
/// [`DEFAULT_BASE_URL`]. Mirrors Go outbound.go:44-47. Unlike Fireworks,
/// Cerebras does **not** trim a trailing slash here — that's delegated to the
/// wrapped OpenRouter transformer's `normalize_base_url` (which trims).
pub fn resolve_base_url(base_url: &str) -> String {
    if base_url.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        base_url.to_string()
    }
}

/// Validate a Cerebras [`Config`], mirroring Go outbound.go:36-42:
///
/// ```go
/// if config == nil { return ... "config is nil" }
/// if config.APIKeyProvider == nil { return ... "API key provider is required" }
/// ```
///
/// In Rust the `Config` is a value, so the nil-config check reduces to
/// requiring a non-empty `api_key`. The base URL is allowed to be empty (it
/// falls back to the default in [`resolve_base_url`]).
pub fn validate_config(config: &Config) -> Result<(), conduit_core::ConduitError> {
    if config.api_key.is_empty() {
        return Err(conduit_core::ConduitError::invalid_request(
            "invalid Cerebras transformer configuration: API key provider is required",
        ));
    }
    Ok(())
}

/// Strip the OpenAI `store` field from the request, mirroring Go
/// `cerebras.OutboundTransformer.TransformRequest` (outbound.go:64-77):
///
/// ```go
/// reqCopy := *llmReq
/// reqCopy.Store = nil // Cerebras does not support the `store` parameter.
/// return t.Outbound.TransformRequest(ctx, &reqCopy)
/// ```
///
/// In the Rust unified [`LlmRequest`] the `store` field is not modeled as a
/// named struct field; it lives in the flattened `extra` map (a `BTreeMap`).
/// This helper removes the `store` key when present. Returns a shallow clone
/// of `req` (the original is untouched, mirroring Go's `reqCopy := *llmReq`).
pub fn strip_store(req: &LlmRequest) -> LlmRequest {
    let mut copy = req.clone();
    copy.extra.remove("store");
    copy
}

/// Whether the request carries a `store` field. Useful for asserting the
/// strip behavior in tests and for callers that want to short-circuit when
/// there's nothing to remove.
pub fn has_store(req: &LlmRequest) -> bool {
    req.extra.contains_key("store")
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{
        ApiFormat, ChatRequest, LlmRequest, LlmRequestPayload, RequestType, model::ExtensionMap,
    };
    use serde_json::json;

    type TestResult = Result<(), conduit_core::ConduitError>;

    // ---- helpers ----

    fn build_request_with_extra(extra: ExtensionMap) -> LlmRequest {
        let chat = ChatRequest::default();
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("llama3.1-8b".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(chat),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra,
        }
    }

    fn build_request_with_store(store: serde_json::Value) -> LlmRequest {
        let mut extra = ExtensionMap::new();
        extra.insert("store".to_string(), store);
        build_request_with_extra(extra)
    }

    fn build_request_no_store() -> LlmRequest {
        build_request_with_extra(ExtensionMap::new())
    }

    // =======================================================================
    // resolve_base_url (mirrors Go outbound.go:44-47)
    // =======================================================================
    #[test]
    fn empty_base_url_falls_back_to_default() {
        assert_eq!(resolve_base_url(""), DEFAULT_BASE_URL);
        assert_eq!(resolve_base_url(""), "https://api.cerebras.ai/v1");
    }

    #[test]
    fn non_empty_base_url_is_preserved() {
        assert_eq!(
            resolve_base_url("https://custom.cerebras.ai/v1"),
            "https://custom.cerebras.ai/v1"
        );
    }

    #[test]
    fn resolve_base_url_does_not_trim_trailing_slash() {
        // Unlike Fireworks, Cerebras delegates the trim to OpenRouter's
        // normalize_base_url. resolve_base_url itself preserves the input
        // verbatim (modulo the default fallback).
        assert_eq!(
            resolve_base_url("https://api.cerebras.ai/v1/"),
            "https://api.cerebras.ai/v1/"
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
    fn validate_config_accepts_non_empty_api_key() -> TestResult {
        let config = Config {
            base_url: String::new(),
            api_key: "cs-key".to_string(),
        };
        validate_config(&config)
    }

    #[test]
    fn validate_config_accepts_empty_base_url_since_default_applies() -> TestResult {
        let config = Config {
            base_url: String::new(),
            api_key: "cs-key".to_string(),
        };
        validate_config(&config)
    }

    // =======================================================================
    // strip_store (mirrors Go outbound.go:64-77)
    // =======================================================================
    #[test]
    fn strip_store_removes_store_true() {
        let req = build_request_with_store(json!(true));
        assert!(has_store(&req), "precondition: store present");

        let stripped = strip_store(&req);

        assert!(!has_store(&stripped), "store should be removed");
        assert!(stripped.extra.get("store").is_none());
    }

    #[test]
    fn strip_store_removes_store_false() {
        // The Go code unconditionally nulls Store, regardless of value — even
        // `false` is stripped (Cerebras rejects the field's presence, not its
        // value).
        let req = build_request_with_store(json!(false));
        let stripped = strip_store(&req);
        assert!(!has_store(&stripped));
    }

    #[test]
    fn strip_store_removes_store_null() {
        let req = build_request_with_store(serde_json::Value::Null);
        let stripped = strip_store(&req);
        assert!(!has_store(&stripped));
    }

    #[test]
    fn strip_store_preserves_other_extra_fields() {
        // The strip must be surgical — other extra fields survive. Mirrors
        // Go's `reqCopy := *llmReq` shallow copy.
        let mut extra = ExtensionMap::new();
        extra.insert("store".to_string(), json!(true));
        extra.insert("temperature".to_string(), json!(0.7));
        extra.insert("user".to_string(), json!("u-1"));
        let req = build_request_with_extra(extra);

        let stripped = strip_store(&req);

        assert!(!has_store(&stripped));
        assert_eq!(stripped.extra.get("temperature"), Some(&json!(0.7)));
        assert_eq!(stripped.extra.get("user"), Some(&json!("u-1")));
    }

    #[test]
    fn strip_store_preserves_named_fields() {
        // Named fields (model, request_type, etc.) are preserved — mirrors
        // Go's value-copy semantics.
        let req = build_request_with_store(json!(true));
        let stripped = strip_store(&req);

        assert_eq!(stripped.request_type, RequestType::Chat);
        assert_eq!(stripped.model.as_deref(), Some("llama3.1-8b"));
        assert_eq!(stripped.api_format, ApiFormat::OpenAiChatCompletions);
    }

    #[test]
    fn strip_store_does_not_mutate_original() {
        // Mirrors Go: `reqCopy := *llmReq` is a value copy; the original
        // request must keep its store field.
        let req = build_request_with_store(json!(true));
        let _ = strip_store(&req);
        assert!(has_store(&req), "original request must retain store");
        assert_eq!(req.extra.get("store"), Some(&json!(true)));
    }

    #[test]
    fn strip_store_on_request_without_store_is_noop() {
        let req = build_request_no_store();
        let stripped = strip_store(&req);
        assert!(!has_store(&stripped));
        assert!(stripped.extra.is_empty());
    }

    // =======================================================================
    // has_store
    // =======================================================================
    #[test]
    fn has_store_false_when_absent() {
        let req = build_request_no_store();
        assert!(!has_store(&req));
    }

    #[test]
    fn has_store_true_when_present() {
        let req = build_request_with_store(json!(true));
        assert!(has_store(&req));
    }

    #[test]
    fn has_store_true_even_when_null() {
        // Presence is what matters, not the value.
        let req = build_request_with_store(serde_json::Value::Null);
        assert!(has_store(&req));
    }

    // =======================================================================
    // DEFAULT_BASE_URL constant (mirrors Go outbound.go:15)
    // =======================================================================
    #[test]
    fn default_base_url_matches_go_constant() {
        assert_eq!(DEFAULT_BASE_URL, "https://api.cerebras.ai/v1");
    }
}
