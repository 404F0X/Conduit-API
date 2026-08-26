//! Moonshot OpenAI-compatible wrapper (RUST-P7-008 S11/S12).
//!
//! Mirrors Go `conduit/llm/transformer/moonshot/outbound.go`. Moonshot is the
//! minimal OpenAI-compatible wrapper: its only deltas versus the shared base
//! are:
//!
//! 1. **`response_format` downgrade** — `json_schema` → `json_object`
//!    (delegated to [`crate::openai_compatible::downgrade_json_schema_to_object`]).
//!    (outbound.go:87-90)
//! 2. **No `thinking` field / no reasoning_content fill** — unlike DeepSeek,
//!    Moonshot's `TransformRequest` is just the OpenAI request body with the
//!    response_format downgrade. (outbound.go:83-95)
//!
//! Everything else (URL normalization, bearer auth, JSON headers, message
//! validation, request-type gating) comes from [`crate::openai_compatible`].
//!
//! Go tests mirrored: `moonshot/outbound_test.go` —
//! `TestOutboundTransformer_TransformRequest_ResponseFormat`,
//! `TestOutboundTransformer_TransformRequest_URL`,
//! `TestOutboundTransformer_TransformRequest_Basic`,
//! `TestOutboundTransformer_TransformRequest_Errors`.

use serde_json::Value;

use crate::TransformerResult;
use crate::openai_compatible::{
    OpenAiCompatibleConfig, downgrade_json_schema_to_object, normalize_v1_base_url,
    require_chat_messages, validate_chat_request_type,
};

/// Moonshot provider configuration. Mirrors Go `moonshot.Config`
/// (outbound.go:17-21).
pub type Config = OpenAiCompatibleConfig;

/// Apply the Moonshot `response_format` downgrade. Mirrors Go
/// `moonshot.OutboundTransformer.TransformRequest` (outbound.go:87-90):
///
/// ```go
/// if oaiReq.ResponseFormat != nil && oaiReq.ResponseFormat.Type == "json_schema" {
///     oaiReq.ResponseFormat.Type = "json_object"
///     oaiReq.ResponseFormat.JSONSchema = nil
/// }
/// ```
///
/// Thin wrapper over the shared helper.
pub fn downgrade_response_format(response_format: &mut Option<Value>) -> bool {
    downgrade_json_schema_to_object(response_format)
}

/// Normalize a Moonshot base URL (append `/v1` when missing). Mirrors Go
/// `transformer.NormalizeBaseURL(config.BaseURL, "v1")` at outbound.go:55.
pub fn normalize_base_url(base_url: &str) -> String {
    normalize_v1_base_url(base_url)
}

/// Validate the request type for Moonshot chat. Moonshot accepts only `Chat`
/// (or empty) and rejects `Compact` and every other explicit type — see Go
/// outbound.go:70-77. The Go switch does *not* have a `Completion` arm
/// (unlike DeepSeek), so the shared helper matches exactly.
pub fn validate_request_type(req: &conduit_llm::LlmRequest) -> TransformerResult<()> {
    validate_chat_request_type(req)
}

/// Require at least one chat message for Moonshot. Mirrors Go
/// `if len(llmReq.Messages) == 0 { ... "messages are required" }`
/// (outbound.go:79-81).
pub fn require_messages(req: &conduit_llm::LlmRequest) -> TransformerResult<()> {
    require_chat_messages(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ChatMessage, ChatRequest, LlmRequest, LlmRequestPayload, RequestType};
    use serde_json::json;

    // ---- helpers ----

    fn build_request(model: &str, messages: Vec<ChatMessage>) -> LlmRequest {
        let mut chat = ChatRequest::default();
        chat.messages = messages;
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: Some(model.to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(chat),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            name: None,
            content: Some(conduit_llm::MessageContent::Text(text.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        }
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_ResponseFormat
    // (mirrors moonshot/outbound_test.go:18-115)
    // =======================================================================
    #[test]
    fn response_format_json_schema_downgrades_to_json_object_and_drops_schema() {
        let mut rf = Some(json!({
            "type": "json_schema",
            "json_schema": {"type": "object", "properties": {"name": {"type": "string"}}}
        }));
        assert!(downgrade_response_format(&mut rf));
        assert_eq!(rf, Some(json!({"type": "json_object"})));
    }

    #[test]
    fn response_format_json_object_remains_unchanged() {
        let mut rf = Some(json!({"type": "json_object"}));
        assert!(!downgrade_response_format(&mut rf));
        assert_eq!(rf, Some(json!({"type": "json_object"})));
    }

    #[test]
    fn response_format_text_remains_unchanged() {
        let mut rf = Some(json!({"type": "text"}));
        assert!(!downgrade_response_format(&mut rf));
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_URL
    // (mirrors moonshot/outbound_test.go:117-164)
    // =======================================================================
    #[test]
    fn base_url_ending_with_v1_is_preserved() {
        assert_eq!(
            normalize_base_url("https://api.moonshot.cn/v1"),
            "https://api.moonshot.cn/v1"
        );
    }

    #[test]
    fn base_url_without_v1_gets_v1_appended() {
        assert_eq!(
            normalize_base_url("https://api.moonshot.cn"),
            "https://api.moonshot.cn/v1"
        );
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_Basic
    // (mirrors moonshot/outbound_test.go:166-206) — the model + message
    // echo assertions are exercised structurally here (the full HTTP build
    // lives in the shared base; this test focuses on the Moonshot-only
    // validate/downgrade deltas feeding it).
    // =======================================================================
    #[test]
    fn validate_and_require_accept_a_basic_chat_request() -> TransformerResult<()> {
        let req = build_request("moonshot-v1-8k", vec![user_msg("Hello, world!")]);
        validate_request_type(&req)?;
        require_messages(&req)?;
        Ok(())
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_Errors
    // (mirrors moonshot/outbound_test.go:208-257)
    // =======================================================================
    #[test]
    fn empty_messages_is_rejected_with_required_message() {
        let req = build_request("moonshot-v1-8k", vec![]);
        let err = match require_messages(&req) {
            Ok(()) => panic!("expected Err for empty messages"),
            Err(e) => e,
        };
        assert!(err.message.contains("messages are required"));
    }

    #[test]
    fn unsupported_request_type_is_rejected_as_not_supported() {
        let mut req = build_request("moonshot-v1-8k", vec![user_msg("Hello")]);
        req.request_type = RequestType::Embedding;
        let err = match validate_request_type(&req) {
            Ok(()) => panic!("expected Err for Embedding request type"),
            Err(e) => e,
        };
        assert!(err.message.contains("is not supported"));
    }

    #[test]
    fn compact_request_type_is_rejected_with_responses_api_message() {
        let mut req = build_request("moonshot-v1-8k", vec![user_msg("Hello")]);
        req.request_type = RequestType::Compact;
        let err = match validate_request_type(&req) {
            Ok(()) => panic!("expected Err for Compact request type"),
            Err(e) => e,
        };
        assert!(
            err.message
                .contains("compact is only supported by OpenAI Responses API")
        );
    }
}
