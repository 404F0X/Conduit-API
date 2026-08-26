//! Zai (Zhipu) OpenAI-compatible wrapper (RUST-P7-008 S11/S12, round 2).
//!
//! Mirrors Go `conduit/llm/transformer/zai/outbound.go`. Zai is a thin
//! OpenAI-compatible wrapper whose deltas versus the shared base are:
//!
//! 1. **API version default `"v4"`** — base URL normalization uses
//!    `NormalizeBaseURL(base, "v4")` (vs the `v1` most other wrappers use).
//!    (outbound.go:59-63)
//! 2. **`user_id` / `request_id` extraction** — these are lifted out of the
//!    request `Metadata` map into top-level body fields; `metadata` itself is
//!    then cleared from the outbound body. (outbound.go:132-150)
//! 3. **`tool_choice` forced to `"auto"`** — Zai only supports the `auto`
//!    tool choice, so any non-`None` tool_choice is rewritten. (outbound.go:143-147)
//! 4. **Optional `thinking` field** — unlike DeepSeek (which always emits
//!    `thinking`), Zai only adds it when `reasoning_effort != ""`. The mapping
//!    is the same: `none` → `disabled`, anything else → `enabled`.
//!    (outbound.go:152-164, thinking_test.go)
//! 5. **`response_format` downgrade** — `json_schema` → `json_object`
//!    (delegated to [`crate::openai_compatible::downgrade_json_schema_to_object`]).
//!    (outbound.go:120-123)
//! 6. **`model` is required** — Zai rejects empty models before anything else.
//!    (outbound.go:96-98)
//!
//! Everything else (URL normalization mechanics, bearer auth, JSON headers,
//! message validation) comes from [`crate::openai_compatible`].
//!
//! Go tests mirrored: `zai/outbound_test.go` —
//! `TestOutboundTransformer_TransformRequest_URL`,
//! `TestOutboundTransformer_TransformRequest_WithMetadata`,
//! `TestOutboundTransformer_TransformRequest_WithThinking`,
//! `TestOutboundTransformer_TransformRequest_ResponseFormat`; and
//! `zai/thinking_test.go` — `TestReasoningEffortToThinking`,
//! `TestZAIRequestWithThinking`, `TestZAIRequestWithoutThinking`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::TransformerResult;
use crate::openai_compatible::{
    OpenAiCompatibleConfig, downgrade_json_schema_to_object, require_chat_messages,
    validate_chat_request_type,
};

/// Zai provider configuration. Mirrors Go `zai.Config` (outbound.go:19-25) —
/// the OpenAI-compatible shape plus the optional API `version` (defaults to
/// `"v4"` when empty, see [`default_version`] / [`normalize_base_url`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Base URL for Zai's OpenAI-compatible API, e.g.
    /// `https://api.zai.com/v4`. Required.
    pub base_url: String,
    /// API key sent as `Authorization: Bearer <key>`. Required.
    pub api_key: String,
    /// API version segment appended to the base URL when missing. Defaults to
    /// `"v4"` when empty. Mirrors Go `Config.Version` (outbound.go:24).
    pub version: String,
}

impl Config {
    /// Build a Zai [`Config`] from the shared OpenAI-compatible shape,
    /// defaulting `version` to `""` (which [`normalize_base_url`] resolves to
    /// `"v4"`). Mirrors how Go callers typically only set `BaseURL` +
    /// `APIKeyProvider`.
    pub fn from_compatible(base: OpenAiCompatibleConfig) -> Self {
        Self {
            base_url: base.base_url,
            api_key: base.api_key,
            version: String::new(),
        }
    }
}

/// The default API version segment appended to Zai base URLs. Mirrors Go
/// `version = "v4"` (outbound.go:61).
pub const DEFAULT_VERSION: &str = "v4";

/// Resolve the effective API version, defaulting to `"v4"` when empty.
/// Mirrors Go outbound.go:59-62:
///
/// ```go
/// version := config.Version
/// if version == "" {
///     version = "v4"
/// }
/// ```
pub fn default_version(version: &str) -> &str {
    if version.is_empty() {
        DEFAULT_VERSION
    } else {
        version
    }
}

/// The `thinking` object embedded in the Zai request body. Mirrors Go
/// `zai.Thinking` (outbound.go:80-84). Same shape as DeepSeek's — serialized
/// as `{"type": "enabled" | "disabled"}` — but Zai makes the field *optional*
/// (only present when `reasoning_effort != ""`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thinking {
    /// `"enabled"` or `"disabled"`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// Compute the Zai `thinking.type` from the chat request's `reasoning_effort`.
/// Mirrors Go outbound.go:154-163:
///
/// ```go
/// switch llmReq.ReasoningEffort {
/// case "none":
///     thinkingType = "disabled"
/// default:
///     thinkingType = "enabled"
/// }
/// ```
///
/// Same mapping as DeepSeek (`none` → disabled, anything else → enabled) but
/// the *presence* rule differs — see [`thinking_body_field`].
pub fn thinking_type_for_effort(reasoning_effort: &str) -> String {
    if reasoning_effort == "none" {
        "disabled".to_string()
    } else {
        "enabled".to_string()
    }
}

/// Build the optional Zai `thinking` body field. Returns `None` when
/// `reasoning_effort` is empty (Zai omits the field entirely in that case,
/// unlike DeepSeek). Mirrors Go `if llmReq.ReasoningEffort != "" { ... }`
/// (outbound.go:153).
pub fn thinking_body_field(reasoning_effort: Option<&str>) -> Option<Value> {
    let effort = reasoning_effort?;
    if effort.is_empty() {
        return None;
    }
    Some(json!({"thinking": {"type": thinking_type_for_effort(effort)}}))
}

/// Apply the Zai `response_format` downgrade. Thin wrapper over the shared
/// helper, mirroring Go outbound.go:120-123.
pub fn downgrade_response_format(response_format: &mut Option<Value>) -> bool {
    downgrade_json_schema_to_object(response_format)
}

/// Normalize a Zai base URL by appending the API version segment when
/// missing. Mirrors Go
/// `transformer.NormalizeBaseURL(config.BaseURL, version)` (outbound.go:63),
/// where `version` defaults to `"v4"`.
pub fn normalize_base_url(base_url: &str, version: &str) -> String {
    crate::openai_outbound::normalize_base_url(base_url.to_string(), default_version(version))
}

/// Force any non-`None` `tool_choice` to the bare string `"auto"`. Mirrors Go
/// outbound.go:143-147:
///
/// ```go
/// if zaiReq.ToolChoice != nil {
///     zaiReq.ToolChoice = &openai.ToolChoice{ToolChoice: lo.ToPtr("auto")}
/// }
/// ```
///
/// In Rust the request body is a free-form `Value`, so the rewrite is
/// expressed as: when `tool_choice` is present (not `None` and not `null`),
/// replace it with `"auto"`. Returns `true` if a rewrite was applied.
pub fn force_tool_choice_auto(tool_choice: &mut Option<Value>) -> bool {
    let Some(tc) = tool_choice.as_ref() else {
        return false;
    };
    if tc.is_null() {
        return false;
    }
    *tool_choice = Some(Value::String("auto".to_string()));
    true
}

/// Extract Zai `user_id` / `request_id` from the chat request's `metadata`
/// map, mirroring Go outbound.go:132-135. Returns `(user_id, request_id)`,
/// each `None` when absent.
///
/// The Go code reads `llmReq.Metadata["user_id"]` /
/// `llmReq.Metadata["request_id"]`; in Rust the metadata is a
/// `BTreeMap<String, Value>` and the values are JSON strings (or non-strings).
/// Only string-valued entries are extracted, matching the Go behavior where a
/// missing key yields `""` (here `None`).
pub fn extract_user_and_request_id(
    metadata: &conduit_llm::model::ExtensionMap,
) -> (Option<String>, Option<String>) {
    let get_str = |key: &str| -> Option<String> {
        metadata
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    (get_str("user_id"), get_str("request_id"))
}

/// Require a non-empty model on the chat request, mirroring Go outbound.go:96-98:
///
/// ```go
/// if llmReq.Model == "" {
///     return ErrInvalidRequest("model is required")
/// }
/// ```
///
/// Returns `Err(ErrInvalidRequest)` when `model` is `None` or empty.
pub fn require_model(req: &conduit_llm::LlmRequest) -> TransformerResult<()> {
    let empty = req.model.as_deref().map(str::is_empty).unwrap_or(true);
    if empty {
        return Err(conduit_core::ConduitError::invalid_request(
            "model is required",
        ));
    }
    Ok(())
}

/// Validate the request type for Zai chat. Zai accepts `Chat` and dispatches
/// `Image` to the image-generation sub-transformer (out of scope here — see
/// module docs); this helper only emits the shared rejection for `Compact` /
/// others. The `Image` branch is the wrapper's responsibility.
pub fn validate_request_type(req: &conduit_llm::LlmRequest) -> TransformerResult<()> {
    validate_chat_request_type(req)
}

/// Require at least one chat message. Mirrors Go outbound.go:112-114.
pub fn require_messages(req: &conduit_llm::LlmRequest) -> TransformerResult<()> {
    require_chat_messages(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{
        ChatMessage, ChatRequest, LlmRequest, LlmRequestPayload, MessageContent, RequestType,
    };
    use serde_json::json;

    // ---- helpers ----

    fn build_request(model: Option<&str>, messages: Vec<ChatMessage>) -> LlmRequest {
        let mut chat = ChatRequest::default();
        chat.messages = messages;
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: model.map(|m| m.to_string()),
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
            content: Some(MessageContent::Text(text.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        }
    }

    // =======================================================================
    // TestReasoningEffortToThinking / TestZAIRequestWithThinking
    // (mirrors zai/thinking_test.go)
    // =======================================================================
    #[test]
    fn thinking_type_for_effort_mirrors_go_switch() {
        // mirrors thinking_test.go::TestReasoningEffortToThinking table
        let cases: &[(&str, &str)] = &[
            ("low", "enabled"),
            ("medium", "enabled"),
            ("high", "enabled"),
            ("none", "disabled"),
            ("unknown", "enabled"),
        ];
        for (effort, expected) in cases {
            assert_eq!(
                thinking_type_for_effort(effort),
                *expected,
                "effort={effort}"
            );
        }
    }

    // =======================================================================
    // TestZAIRequestWithThinking / TestZAIRequestWithoutThinking
    // (mirrors zai/thinking_test.go:83-154) — covers the *presence* rule.
    // =======================================================================
    #[test]
    fn thinking_body_field_present_when_effort_non_empty() {
        // mirrors TestZAIRequestWithThinking
        let field = thinking_body_field(Some("high"));
        assert_eq!(field, Some(json!({"thinking": {"type": "enabled"}})));
    }

    #[test]
    fn thinking_body_field_absent_when_effort_empty_string() {
        // mirrors TestZAIRequestWithoutThinking: ReasoningEffort == "" → no field
        assert_eq!(thinking_body_field(Some("")), None);
    }

    #[test]
    fn thinking_body_field_absent_when_effort_none() {
        // mirrors TestZAIRequestWithoutThinking (no ReasoningEffort set at all)
        assert_eq!(thinking_body_field(None), None);
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_URL
    // (mirrors zai/outbound_test.go:16-196) — the URL normalization cases.
    // =======================================================================
    #[test]
    fn base_url_with_v4_suffix_is_preserved() {
        assert_eq!(
            normalize_base_url("https://api.zai.com/v4", ""),
            "https://api.zai.com/v4"
        );
    }

    #[test]
    fn base_url_without_v4_gets_v4_appended() {
        assert_eq!(
            normalize_base_url("https://api.zai.com", ""),
            "https://api.zai.com/v4"
        );
    }

    #[test]
    fn base_url_with_trailing_slash_no_v4_gets_v4_appended() {
        // mirrors "base URL with trailing slash but no /v4"
        assert_eq!(
            normalize_base_url("https://api.zai.com/", ""),
            "https://api.zai.com/v4"
        );
    }

    #[test]
    fn base_url_with_trailing_slash_and_v4_is_preserved() {
        // mirrors "base URL with trailing slash and /v4"
        assert_eq!(
            normalize_base_url("https://api.zai.com/v4/", ""),
            "https://api.zai.com/v4"
        );
    }

    #[test]
    fn base_url_with_other_version_path_gets_v4_appended() {
        // mirrors "base URL with path but not /v4" — /v1 doesn't match, so v4
        // is appended.
        assert_eq!(
            normalize_base_url("https://api.zai.com/v1", ""),
            "https://api.zai.com/v1/v4"
        );
    }

    #[test]
    fn default_version_resolves_empty_to_v4() {
        assert_eq!(default_version(""), "v4");
        assert_eq!(default_version("v3"), "v3");
    }

    #[test]
    fn explicit_version_overrides_default() {
        assert_eq!(
            normalize_base_url("https://api.zai.com", "v2"),
            "https://api.zai.com/v2"
        );
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_WithMetadata
    // (mirrors zai/outbound_test.go:198-240) — user_id/request_id extraction.
    // =======================================================================
    #[test]
    fn extract_user_and_request_id_from_metadata() {
        let mut metadata = conduit_llm::model::ExtensionMap::new();
        metadata.insert("user_id".to_string(), json!("test-user-123"));
        metadata.insert("request_id".to_string(), json!("test-request-456"));
        let (uid, rid) = extract_user_and_request_id(&metadata);
        assert_eq!(uid.as_deref(), Some("test-user-123"));
        assert_eq!(rid.as_deref(), Some("test-request-456"));
    }

    #[test]
    fn extract_user_and_request_id_returns_none_when_absent() {
        let metadata = conduit_llm::model::ExtensionMap::new();
        let (uid, rid) = extract_user_and_request_id(&metadata);
        assert!(uid.is_none());
        assert!(rid.is_none());
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_WithThinking
    // (mirrors zai/outbound_test.go:242-280) — already covered by
    // thinking_body_field tests above; this asserts the URL the test expects.
    // =======================================================================
    #[test]
    fn thinking_test_url_uses_v4_default() {
        // mirrors the URL assertion in TestOutboundTransformer_TransformRequest_WithThinking
        assert_eq!(
            normalize_base_url("https://api.zai.com", ""),
            "https://api.zai.com/v4"
        );
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_ResponseFormat
    // (mirrors zai/outbound_test.go:282-381)
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
    }

    #[test]
    fn response_format_text_remains_unchanged() {
        let mut rf = Some(json!({"type": "text"}));
        assert!(!downgrade_response_format(&mut rf));
    }

    // =======================================================================
    // tool_choice forcing (outbound.go:143-147) — no dedicated Go test, but
    // the behavior is asserted structurally here.
    // =======================================================================
    #[test]
    fn force_tool_choice_auto_rewrites_object_choice() {
        let mut tc = Some(json!({"type": "function", "function": {"name": "tool_one"}}));
        assert!(force_tool_choice_auto(&mut tc));
        assert_eq!(tc, Some(json!("auto")));
    }

    #[test]
    fn force_tool_choice_auto_rewrites_string_choice() {
        let mut tc = Some(json!("required"));
        assert!(force_tool_choice_auto(&mut tc));
        assert_eq!(tc, Some(json!("auto")));
    }

    #[test]
    fn force_tool_choice_auto_leaves_none_unchanged() {
        let mut tc: Option<Value> = None;
        assert!(!force_tool_choice_auto(&mut tc));
        assert!(tc.is_none());
    }

    #[test]
    fn force_tool_choice_auto_leaves_null_unchanged() {
        let mut tc = Some(Value::Null);
        assert!(!force_tool_choice_auto(&mut tc));
    }

    // =======================================================================
    // request validation parity (outbound.go:91-114)
    // =======================================================================
    #[test]
    fn require_model_rejects_empty_model() {
        let req = build_request(Some(""), vec![user_msg("hi")]);
        let err = match require_model(&req) {
            Ok(()) => panic!("expected Err for empty model"),
            Err(e) => e,
        };
        assert!(err.message.contains("model is required"));
    }

    #[test]
    fn require_model_rejects_missing_model() {
        let req = build_request(None, vec![user_msg("hi")]);
        let err = match require_model(&req) {
            Ok(()) => panic!("expected Err for missing model"),
            Err(e) => e,
        };
        assert!(err.message.contains("model is required"));
    }

    #[test]
    fn require_model_accepts_non_empty_model() -> TransformerResult<()> {
        let req = build_request(Some("gpt-4"), vec![user_msg("hi")]);
        require_model(&req)
    }

    #[test]
    fn require_messages_rejects_empty_messages() {
        let req = build_request(Some("gpt-4"), vec![]);
        let err = match require_messages(&req) {
            Ok(()) => panic!("expected Err for empty messages"),
            Err(e) => e,
        };
        assert!(err.message.contains("messages are required"));
    }

    #[test]
    fn validate_request_type_rejects_compact() {
        let mut req = build_request(Some("gpt-4"), vec![user_msg("hi")]);
        req.request_type = RequestType::Compact;
        let err = match validate_request_type(&req) {
            Ok(()) => panic!("expected Err for Compact"),
            Err(e) => e,
        };
        assert!(
            err.message
                .contains("compact is only supported by OpenAI Responses API")
        );
    }

    // =======================================================================
    // Request shape parity: Thinking field serializes with type rename.
    // =======================================================================
    #[test]
    fn thinking_field_serializes_with_type_rename() -> Result<(), serde_json::Error> {
        let thinking = Thinking {
            kind: "enabled".to_string(),
        };
        let serialized = serde_json::to_value(&thinking)?;
        assert_eq!(serialized, json!({"type": "enabled"}));
        Ok(())
    }
}
