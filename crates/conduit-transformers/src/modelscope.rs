//! ModelScope (Alibaba Cloud ModelScope) OpenAI-compatible wrapper
//! (RUST-P7-008 S11/S12, round 4).
//!
//! Mirrors Go `conduit/llm/transformer/modelscope/outbound.go`. ModelScope is
//! the minimal OpenAI-compatible wrapper: its only request-side delta is that
//! the `metadata` field must be cleared before delegating to the OpenAI
//! transformer (ModelScope's API rejects requests carrying a top-level
//! `metadata` object). Everything else is handled by the wrapped OpenAI
//! transformer / [`crate::openai_compatible`] shared base.
//!
//! Go has no dedicated `outbound_test.go` for ModelScope (the only delta is
//! `reqCopy.Metadata = nil`, verified here by direct unit test).

use crate::openai_compatible::OpenAiCompatibleConfig;
use conduit_llm::LlmRequest;

/// ModelScope provider configuration. Mirrors Go `modelscope.Config`
/// (outbound.go:14-18) — the OpenAI-compatible shape.
pub type Config = OpenAiCompatibleConfig;

/// Strip the `metadata` field from the request, mirroring Go
/// `modelscope.OutboundTransformer.TransformRequest` (outbound.go:60-64):
///
/// ```go
/// reqCopy := *chatReq
/// reqCopy.Metadata = nil // model scope does not support metadata.
/// return t.Outbound.TransformRequest(ctx, &reqCopy)
/// ```
///
/// Returns a shallow clone of `req` with `metadata` emptied. The original
/// request is untouched (mirroring Go's `reqCopy := *chatReq` value-copy).
pub fn strip_metadata(req: &LlmRequest) -> LlmRequest {
    let mut copy = req.clone();
    copy.metadata.clear();
    copy
}

/// Whether the request carries any metadata. Useful for asserting the strip
/// behavior in tests and for callers that want to short-circuit when there's
/// nothing to remove.
pub fn has_metadata(req: &LlmRequest) -> bool {
    !req.metadata.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{
        ApiFormat, ChatRequest, LlmRequest, LlmRequestPayload, RequestType, model::ExtensionMap,
    };
    use serde_json::json;

    // ---- helpers ----

    fn build_request_with_metadata(metadata: ExtensionMap) -> LlmRequest {
        let chat = ChatRequest::default();
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("Qwen/Qwen2.5-7B".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(chat),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata,
            extra: Default::default(),
        }
    }

    fn build_request_no_metadata() -> LlmRequest {
        build_request_with_metadata(ExtensionMap::new())
    }

    // =======================================================================
    // strip_metadata (mirrors Go outbound.go:60-64)
    // =======================================================================
    #[test]
    fn strip_metadata_clears_non_empty_metadata() {
        let mut metadata = ExtensionMap::new();
        metadata.insert("user_id".to_string(), json!("u-123"));
        metadata.insert("request_id".to_string(), json!("r-456"));
        let req = build_request_with_metadata(metadata);
        assert!(has_metadata(&req), "precondition: metadata non-empty");

        let stripped = strip_metadata(&req);

        assert!(
            stripped.metadata.is_empty(),
            "metadata should be empty after strip"
        );
        assert!(!has_metadata(&stripped));
    }

    #[test]
    fn strip_metadata_preserves_other_fields() {
        // The strip must be a shallow clone — fields other than metadata are
        // preserved (model, request_type, etc.). Mirrors Go's `reqCopy := *chatReq`.
        let mut metadata = ExtensionMap::new();
        metadata.insert("k".to_string(), json!("v"));
        let req = build_request_with_metadata(metadata);
        let stripped = strip_metadata(&req);

        assert_eq!(stripped.request_type, RequestType::Chat);
        assert_eq!(stripped.model.as_deref(), Some("Qwen/Qwen2.5-7B"));
        assert_eq!(stripped.api_format, ApiFormat::OpenAiChatCompletions);
    }

    #[test]
    fn strip_metadata_does_not_mutate_original() {
        // Mirrors Go: `reqCopy := *chatReq` is a value copy; the original
        // request must keep its metadata.
        let mut metadata = ExtensionMap::new();
        metadata.insert("user_id".to_string(), json!("u-1"));
        let req = build_request_with_metadata(metadata);
        let _ = strip_metadata(&req);
        // Original untouched.
        assert!(has_metadata(&req), "original request must retain metadata");
        assert_eq!(
            req.metadata.get("user_id").and_then(|v| v.as_str()),
            Some("u-1")
        );
    }

    #[test]
    fn strip_metadata_on_empty_metadata_is_noop() {
        let req = build_request_no_metadata();
        let stripped = strip_metadata(&req);
        assert!(stripped.metadata.is_empty());
        assert!(!has_metadata(&stripped));
    }

    // =======================================================================
    // has_metadata
    // =======================================================================
    #[test]
    fn has_metadata_false_for_empty() {
        let req = build_request_no_metadata();
        assert!(!has_metadata(&req));
    }

    #[test]
    fn has_metadata_true_for_non_empty() {
        let mut metadata = ExtensionMap::new();
        metadata.insert("k".to_string(), json!("v"));
        let req = build_request_with_metadata(metadata);
        assert!(has_metadata(&req));
    }
}
