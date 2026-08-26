//! DeepSeek OpenAI-compatible wrapper (RUST-P7-008 S11/S12).
//!
//! Mirrors Go `conduit/llm/transformer/deepseek/outbound.go`. DeepSeek is a
//! thin wrapper over the OpenAI transformer whose only deltas versus the
//! shared OpenAI-compatible base are:
//!
//! 1. **`thinking` field** — the outbound JSON body carries a top-level
//!    `{"thinking": {"type": "enabled" | "disabled"}}` object driven by
//!    `reasoning_effort` (`"none"` → disabled, anything else → enabled).
//!    (outbound.go:114-124)
//! 2. **`reasoning_content` fill** — when thinking is enabled, every
//!    assistant message with a `None` `reasoning_content` is filled with `""`
//!    so DeepSeek's API accepts the prior-turn context. (outbound.go:126-132)
//! 3. **`response_format` downgrade** — `json_schema` → `json_object`
//!    (delegated to [`crate::openai_compatible::downgrade_json_schema_to_object`]).
//! 4. **`Completion` sub-transformer dispatch** — not modeled here (the full
//!    OpenAI outbound transformer impl is RUST-P7-002 S04/S08/S09); left as
//!    a documented TODO hook for the future agent that wires the live trait.
//!
//! Everything else (URL normalization, bearer auth, JSON headers, message
//! validation) comes from [`crate::openai_compatible`].
//!
//! Go tests mirrored: `deepseek/outbound_test.go` —
//! `TestOutboundTransformer_TransformRequest_ResponseFormat`,
//! `TestOutboundTransformer_TransformRequest_Thinking`,
//! `TestOutboundTransformer_TransformRequest_URL`,
//! `TestOutboundTransformer_TransformRequest_ReasoningContentFill`,
//! `TestRequest_EmbeddedOpenAIRequest`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::TransformerResult;
use crate::openai_compatible::{
    OpenAiCompatibleConfig, downgrade_json_schema_to_object, normalize_v1_base_url,
    validate_chat_request_type,
};

/// DeepSeek provider configuration. Mirrors Go `deepseek.Config`
/// (outbound.go:20-23).
pub type Config = OpenAiCompatibleConfig;

/// The `thinking` object embedded in the DeepSeek request body. Mirrors Go
/// `deepseek.Thinking` (outbound.go:79-81).
///
/// Go json tag is `json:"thinking,omitempty"` on the parent field and the
/// inner struct serializes as `{"type": "..."}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thinking {
    /// `"enabled"` or `"disabled"`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// Compute the DeepSeek `thinking.type` value from the chat request's
/// `reasoning_effort`. Mirrors Go outbound.go:114-124:
///
/// ```go
/// thinkingDisabled := llmReq.ReasoningEffort == "none"
/// dsReq.Thinking = &Thinking{Type: "enabled"}
/// if thinkingDisabled {
///     dsReq.Thinking.Type = "disabled"
/// }
/// ```
///
/// Returns `"disabled"` if and only if `reasoning_effort == Some("none")`;
/// otherwise `"enabled"` (including `None` and empty effort, matching the Go
/// default-thinking-enabled behavior covered by the test
/// `"empty reasoning effort enables thinking by default"`).
pub fn thinking_type_for_effort(reasoning_effort: Option<&str>) -> String {
    if reasoning_effort == Some("none") {
        "disabled".to_string()
    } else {
        "enabled".to_string()
    }
}

/// Whether thinking is disabled for the given effort. Mirrors Go
/// `thinkingDisabled := llmReq.ReasoningEffort == "none"`
/// (outbound.go:114).
pub fn thinking_is_disabled(reasoning_effort: Option<&str>) -> bool {
    reasoning_effort == Some("none")
}

/// Fill `reasoning_content: ""` on every assistant chat message that lacks
/// one, in place. Mirrors Go outbound.go:126-132:
///
/// ```go
/// if !thinkingDisabled {
///     for i := range dsReq.Messages {
///         if dsReq.Messages[i].Role == "assistant" && dsReq.Messages[i].ReasoningContent == nil {
///             dsReq.Messages[i].ReasoningContent = lo.ToPtr("")
///         }
///     }
/// }
/// ```
///
/// The DeepSeek Go `Request` reuses the OpenAI `Message` shape whose
/// `reasoning_content` is a `*string`; the Rust [`conduit_llm::ChatMessage`]
/// model carries it in the flattened `extra` map (key `reasoning_content`),
/// so this helper writes into `extra`. Existing non-empty values are
/// preserved. Messages whose `role != "assistant"` are untouched. The helper
/// is a no-op when `thinking_disabled` is `true`.
///
/// Returns the count of messages filled (handy for tests).
pub fn fill_assistant_reasoning_content(
    messages: &mut [conduit_llm::ChatMessage],
    thinking_disabled: bool,
) -> usize {
    if thinking_disabled {
        return 0;
    }
    let mut filled = 0usize;
    for msg in messages.iter_mut() {
        if msg.role != "assistant" {
            continue;
        }
        let already_present = msg
            .extra
            .get("reasoning_content")
            .is_some_and(|v| !v.is_null());
        if !already_present {
            msg.extra.insert(
                "reasoning_content".to_string(),
                Value::String(String::new()),
            );
            filled += 1;
        }
    }
    filled
}

/// Apply the DeepSeek `response_format` downgrade to a free-form
/// `response_format` value. Thin wrapper over the shared helper, exposed so
/// the DeepSeek module reads like the Go source (`oaiReq.ResponseFormat.Type
/// = "json_object"`).
pub fn downgrade_response_format(response_format: &mut Option<Value>) -> bool {
    downgrade_json_schema_to_object(response_format)
}

/// Normalize a DeepSeek base URL (append `/v1` when missing). Mirrors Go
/// `transformer.NormalizeBaseURL(config.BaseURL, "v1")` at
/// outbound.go:55.
pub fn normalize_base_url(base_url: &str) -> String {
    normalize_v1_base_url(base_url)
}

/// Validate the request type for DeepSeek chat. DeepSeek accepts `Chat` and
/// dispatches `Completion` to a sub-transformer; this helper only emits the
/// shared rejection for `Compact` / others. The `Completion` branch is the
/// wrapper's responsibility (see module docs).
pub fn validate_request_type(req: &conduit_llm::LlmRequest) -> TransformerResult<()> {
    validate_chat_request_type(req)
}

/// Build the DeepSeek `thinking` object to embed in the request body.
/// Convenience over [`thinking_type_for_effort`].
pub fn build_thinking(reasoning_effort: Option<&str>) -> Thinking {
    Thinking {
        kind: thinking_type_for_effort(reasoning_effort),
    }
}

/// Serialize the DeepSeek request body delta (the `thinking` field) as a JSON
/// object, suitable for merging into the OpenAI request body before sending.
///
/// This mirrors the Go shape where `Request` embeds `openai.Request` +
/// `Thinking *Thinking` and serializes both with `json.Marshal`. Here we
/// produce just the `thinking` key so the wrapper can merge it into the
/// OpenAI body produced by the (future) full transformer impl.
pub fn thinking_body_field(reasoning_effort: Option<&str>) -> Value {
    json!({"thinking": {"type": thinking_type_for_effort(reasoning_effort)}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ChatMessage, ChatRequest, LlmRequest, LlmRequestPayload, RequestType};
    use serde_json::json;

    // ---- helpers ----

    fn assistant_with_reasoning(content: &str, reasoning: Option<&str>) -> ChatMessage {
        let mut msg = ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(conduit_llm::MessageContent::Text(content.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        };
        if let Some(r) = reasoning {
            msg.extra.insert(
                "reasoning_content".to_string(),
                Value::String(r.to_string()),
            );
        }
        msg
    }

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            name: None,
            content: Some(conduit_llm::MessageContent::Text(content.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        }
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_Thinking
    // (mirrors deepseek/outbound_test.go:117-183)
    // =======================================================================
    #[test]
    fn thinking_type_for_effort_mirrors_go_table() {
        // name / reasoning_effort / expected
        let cases: &[(&str, Option<&str>, &str)] = &[
            (
                "reasoning effort high enables thinking",
                Some("high"),
                "enabled",
            ),
            (
                "reasoning effort medium enables thinking",
                Some("medium"),
                "enabled",
            ),
            (
                "reasoning effort none disables thinking",
                Some("none"),
                "disabled",
            ),
            (
                "empty reasoning effort enables thinking by default",
                None,
                "enabled",
            ),
        ];
        for (name, effort, expected) in cases {
            assert_eq!(thinking_type_for_effort(*effort), *expected, "{name}");
        }
    }

    #[test]
    fn thinking_is_disabled_only_for_none_effort() {
        assert!(thinking_is_disabled(Some("none")));
        assert!(!thinking_is_disabled(Some("high")));
        assert!(!thinking_is_disabled(None));
    }

    #[test]
    fn build_thinking_produces_expected_kind() {
        assert_eq!(build_thinking(Some("none")).kind, "disabled");
        assert_eq!(build_thinking(Some("high")).kind, "enabled");
        assert_eq!(build_thinking(None).kind, "enabled");
    }

    #[test]
    fn thinking_body_field_serializes_expected_shape() {
        assert_eq!(
            thinking_body_field(Some("high")),
            json!({"thinking": {"type": "enabled"}})
        );
        assert_eq!(
            thinking_body_field(Some("none")),
            json!({"thinking": {"type": "disabled"}})
        );
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_ReasoningContentFill
    // (mirrors deepseek/outbound_test.go:234-379)
    // =======================================================================
    #[test]
    fn fill_reasoning_content_when_thinking_enabled_for_empty_assistant() {
        let mut msgs = vec![user_msg("Hello"), assistant_with_reasoning("Hi", None)];
        let filled = fill_assistant_reasoning_content(&mut msgs, false);
        assert_eq!(filled, 1);
        assert_eq!(
            msgs[1].extra.get("reasoning_content"),
            Some(&Value::String(String::new()))
        );
    }

    #[test]
    fn fill_reasoning_content_preserves_existing_value() {
        let mut msgs = vec![
            user_msg("Hello"),
            assistant_with_reasoning("Hi", Some("Let me think...")),
        ];
        let filled = fill_assistant_reasoning_content(&mut msgs, false);
        assert_eq!(filled, 0);
        assert_eq!(
            msgs[1].extra.get("reasoning_content"),
            Some(&Value::String("Let me think...".to_string()))
        );
    }

    #[test]
    fn fill_reasoning_content_default_when_effort_empty() {
        // mirrors "default thinking fills reasoning_content when effort is empty"
        let mut msgs = vec![user_msg("Hello"), assistant_with_reasoning("Hi", None)];
        let filled = fill_assistant_reasoning_content(&mut msgs, false);
        assert_eq!(filled, 1);
        assert_eq!(
            msgs[1].extra.get("reasoning_content"),
            Some(&Value::String(String::new()))
        );
    }

    #[test]
    fn fill_reasoning_content_skipped_when_thinking_disabled() {
        // mirrors "thinking disabled does not fill reasoning_content"
        let mut msgs = vec![user_msg("Hello"), assistant_with_reasoning("Hi", None)];
        let filled = fill_assistant_reasoning_content(&mut msgs, true);
        assert_eq!(filled, 0);
        assert!(msgs[1].extra.get("reasoning_content").is_none());
    }

    #[test]
    fn fill_reasoning_content_fills_all_assistant_messages() {
        // mirrors "multiple assistant messages all get filled"
        let mut msgs = vec![
            user_msg("Hello"),
            assistant_with_reasoning("Hi", None),
            user_msg("How are you?"),
            assistant_with_reasoning("I'm fine", Some("thinking")),
            user_msg("Great"),
            assistant_with_reasoning("Thanks", None),
        ];
        let filled = fill_assistant_reasoning_content(&mut msgs, false);
        assert_eq!(filled, 2);
        assert_eq!(
            msgs[1].extra.get("reasoning_content"),
            Some(&Value::String(String::new()))
        );
        assert_eq!(
            msgs[3].extra.get("reasoning_content"),
            Some(&Value::String("thinking".to_string()))
        );
        assert_eq!(
            msgs[5].extra.get("reasoning_content"),
            Some(&Value::String(String::new()))
        );
    }

    #[test]
    fn fill_reasoning_content_ignores_non_assistant_messages() {
        // mirrors "non-assistant messages are not affected"
        let mut msgs = vec![
            {
                let mut m = user_msg("You are helpful");
                m.role = "system".to_string();
                m
            },
            user_msg("Hello"),
            assistant_with_reasoning("Hi", None),
        ];
        let filled = fill_assistant_reasoning_content(&mut msgs, false);
        assert_eq!(filled, 1);
        assert!(msgs[0].extra.get("reasoning_content").is_none());
        assert!(msgs[1].extra.get("reasoning_content").is_none());
        assert_eq!(
            msgs[2].extra.get("reasoning_content"),
            Some(&Value::String(String::new()))
        );
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_ResponseFormat
    // (mirrors deepseek/outbound_test.go:18-115)
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
    // (mirrors deepseek/outbound_test.go:185-232)
    // =======================================================================
    #[test]
    fn base_url_ending_with_v1_is_preserved() {
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1"
        );
    }

    #[test]
    fn base_url_without_v1_gets_v1_appended() {
        assert_eq!(
            normalize_base_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1"
        );
    }

    // =======================================================================
    // TestRequest_EmbeddedOpenAIRequest
    // (mirrors deepseek/outbound_test.go:382-404)
    // =======================================================================
    #[test]
    fn thinking_field_serializes_with_type_rename() -> Result<(), serde_json::Error> {
        // The Go test asserts that a Request{Model:"deepseek-chat",
        // Thinking:{Type:"enabled"}} marshals to JSON with `model` and
        // `thinking.type` keys. Here we verify the Thinking struct itself
        // (the wrapper's only delta over the OpenAI request shape).
        let thinking = Thinking {
            kind: "enabled".to_string(),
        };
        let serialized = serde_json::to_value(&thinking)?;
        assert_eq!(serialized, json!({"type": "enabled"}));
        Ok(())
    }

    // =======================================================================
    // validate_request_type parity (mirrors the switch at outbound.go:87-97
    // minus the Completion sub-transformer dispatch).
    // =======================================================================
    #[test]
    fn validate_request_type_accepts_chat() -> TransformerResult<()> {
        let req = LlmRequest {
            request_type: RequestType::Chat,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: Some("deepseek-chat".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };
        validate_request_type(&req)
    }

    #[test]
    fn validate_request_type_rejects_compact_with_responses_message() {
        let req = LlmRequest {
            request_type: RequestType::Compact,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: Some("deepseek-chat".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };
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
