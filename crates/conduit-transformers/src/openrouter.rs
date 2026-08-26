//! OpenRouter OpenAI-compatible wrapper (RUST-P7-008 S11/S12, round 4).
//!
//! Mirrors Go `conduit/llm/transformer/openrouter/` — `outbound.go` +
//! `model.go`. OpenRouter is "mostly compatible with OpenAI(DeepSeek) API"
//! (outbound.go:32) with the following deltas:
//!
//! ## Request-side deltas (portable pure helpers)
//!
//! 1. **`model` is required** — the wrapper rejects empty models before
//!    dispatching (outbound.go:83-85).
//! 2. **Extended request-type gating** — beyond the shared
//!    Chat/Compact/default switch, OpenRouter *also* accepts `Image`
//!    (dispatched to image generation), `Embedding`, `Speech`,
//!    `Transcription`, `Translation` (all delegated to the OpenAI
//!    transformer). (outbound.go:87-102)
//! 3. **`messages` required** for chat (outbound.go:104-106).
//! 4. **Base URL**: trailing `/` trimmed, no version segment appended
//!    (outbound.go:64) — unlike most other wrappers which call
//!    `NormalizeBaseURL(base, "v1")`.
//!
//! ## Response-side deltas (portable pure helpers)
//!
//! 5. **Reasoning-field precedence** — OpenRouter responses carry either a
//!    top-level `reasoning` string *or* a structured `reasoning_details`
//!    array. When `reasoning_details` is non-empty, its `text` fields are
//!    concatenated and take precedence over `reasoning`. The result is
//!    mapped to OpenAI's `reasoning_content`. (model.go:65-75)
//! 6. **Images → multiple-content merge** — when the response message carries
//!    an `images` array, those image parts are appended to the message's
//!    content (text-or-parts) as `image_url` parts. (model.go:77-102)
//!
//! ## Out of scope (documented TODOs)
//!
//! * `buildImageGenerationRequest` — image-edit request building depends on
//!   the live `LlmRequest`/`Image` payload shape and base64 encoding; left as
//!   a TODO hook for RUST-P7-002 S04/S08/S09.
//! * `transformImageGenerationResponse` + `extractBase64FromDataURL` — depend
//!   on testdata snapshots (`testdata/*.json`, see
//!   `openrouter/model_test.go`). Per CLAUDE.md these snapshots are not
//!   synthesized; marked `// pending source snapshot`.
//! * `TransformStream`/`AggregateStreamChunks`/`TransformError` — streaming
//!   and error-classification concerns owned by the live trait impl.
//!
//! Go tests mirrored (pure-logic portions): the reasoning-precedence and
//! images-merge behavior of `Message.ToOpenAIMessage` (model.go:63-105) is
//! covered here by direct unit tests; the JSON-snapshot-driven
//! `TestResponse_ToOpenAIResponse` (model_test.go) is pending source snapshot.

use crate::TransformerResult;
use crate::openai_compatible::{OpenAiCompatibleConfig, require_chat_messages};
use conduit_llm::{ChatMessage, ContentPart, LlmRequest, MessageContent, RequestType};
use serde_json::Value;

/// OpenRouter provider configuration. Mirrors Go `openrouter.Config`
/// (outbound.go:24-29) — the OpenAI-compatible shape.
pub type Config = OpenAiCompatibleConfig;

/// Normalize an OpenRouter base URL by trimming trailing slashes. Mirrors Go
/// `strings.TrimSuffix(config.BaseURL, "/")` (outbound.go:64).
///
/// Note: OpenRouter does *not* append a version segment — it uses the base
/// URL as-is (just `/chat/completions` appended at request time).
pub fn normalize_base_url(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

/// Require a non-empty model on the request, mirroring Go outbound.go:83-85.
pub fn require_model(req: &LlmRequest) -> TransformerResult<()> {
    let empty = req.model.as_deref().map(str::is_empty).unwrap_or(true);
    if empty {
        return Err(conduit_core::ConduitError::invalid_request(
            "model is required",
        ));
    }
    Ok(())
}

/// Classify a request type for OpenRouter's extended gating. Mirrors Go
/// outbound.go:87-102. Returns:
///
/// * `OpenRouterRequestKind::Chat` for `Chat` (proceed with chat handling),
/// * `OpenRouterRequestKind::Delegate` for `Embedding`/`Speech`/
///   `Transcription`/`Translation` (delegate to the OpenAI transformer),
/// * `OpenRouterRequestKind::Image` for `Image` (dispatch to image-gen),
/// * `Err(...)` for `Compact` ("compact is only supported by OpenAI Responses API"),
/// * `Err(...)` for any other type ("<type> is not supported").
///
/// The `Image` arm is returned as a tagged variant so the caller can dispatch
/// to the (future) image-generation builder without this helper depending on
/// the not-yet-ported image-request payload shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRouterRequestKind {
    Chat,
    Image,
    Delegate,
}

pub fn classify_request_type(req: &LlmRequest) -> TransformerResult<OpenRouterRequestKind> {
    match req.request_type {
        RequestType::Chat => Ok(OpenRouterRequestKind::Chat),
        RequestType::Image => Ok(OpenRouterRequestKind::Image),
        RequestType::Embedding
        | RequestType::Speech
        | RequestType::Transcription
        | RequestType::Translation => Ok(OpenRouterRequestKind::Delegate),
        RequestType::Compact => Err(conduit_core::ConduitError::invalid_request(
            "compact is only supported by OpenAI Responses API",
        )),
        other => Err(conduit_core::ConduitError::invalid_request(format!(
            "{} is not supported",
            other.as_str()
        ))),
    }
}

/// Require at least one chat message for OpenRouter chat requests. Mirrors Go
/// outbound.go:104-106.
pub fn require_messages(req: &LlmRequest) -> TransformerResult<()> {
    require_chat_messages(req)
}

// ---------------------------------------------------------------------------
// Response-side: reasoning-field precedence (model.go:63-105)
// ---------------------------------------------------------------------------

/// A single OpenRouter `reasoning_details` entry. Mirrors Go
/// `ReasoningDetail` (model.go:56-61).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ReasoningDetail {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub index: i64,
}

/// Apply OpenRouter's reasoning-field precedence to a choice message,
/// mirroring Go `Message.ToOpenAIMessage` reasoning branch (model.go:64-75).
///
/// If `reasoning_details` (read from the `extra` map) is a non-empty array,
/// each entry's `text` is concatenated and the result overrides
/// `reasoning_content`. Otherwise, if a top-level `reasoning` field is
/// present, it is copied to `reasoning_content`.
///
/// Returns `true` if `reasoning_content` was set (handy for tests).
pub fn apply_reasoning_precedence(msg: &mut ChatMessage) -> bool {
    // Prefer reasoning_details if present and non-empty.
    if let Some(Value::Array(details)) = msg.extra.get("reasoning_details") {
        if !details.is_empty() {
            let concatenated: String = details
                .iter()
                .filter_map(|d| {
                    if let Value::Object(obj) = d {
                        obj.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");
            msg.extra
                .insert("reasoning_content".to_string(), Value::String(concatenated));
            return true;
        }
    }
    // Fallback to top-level reasoning.
    if let Some(reasoning) = msg.extra.get("reasoning") {
        if !reasoning.is_null() {
            msg.extra
                .insert("reasoning_content".to_string(), reasoning.clone());
            return true;
        }
    }
    false
}

/// Apply the images → multiple-content merge, mirroring Go
/// `Message.ToOpenAIMessage` images branch (model.go:77-92).
///
/// When the message carries an `images` array (in `extra`), each image entry's
/// `image_url.url` is appended as an `image_url` content part. Existing text
/// content is preserved as the first part; existing `Parts` are kept as-is
/// and extended. When the message has no text content, only the image parts
/// are used.
///
/// The `images` key is removed from `extra` after the merge (mirroring how Go
/// folds the field into `Content.MultipleContent`). Returns `true` if a merge
/// was applied.
pub fn merge_images_into_content(msg: &mut ChatMessage) -> bool {
    let images = match msg.extra.remove("images") {
        Some(Value::Array(images)) if !images.is_empty() => images,
        _ => return false,
    };

    // Collect image_url parts from the images array.
    let mut image_parts: Vec<ContentPart> = Vec::new();
    for img in images {
        if let Value::Object(obj) = &img {
            if let Some(image_url) = obj.get("image_url").and_then(|v| v.as_object()) {
                if let Some(url) = image_url.get("url").and_then(|v| v.as_str()) {
                    image_parts.push(ContentPart {
                        part_type: "image_url".to_string(),
                        text: None,
                        image_url: Some(serde_json::json!({"url": url})),
                        input_audio: None,
                        extra: Default::default(),
                    });
                }
            }
        }
    }
    if image_parts.is_empty() {
        // Restore the removed key if no usable images were found.
        msg.extra
            .insert("images".to_string(), Value::Array(Vec::new()));
        return false;
    }

    // Build the merged content: existing text/parts + image parts.
    let mut merged: Vec<ContentPart> = match msg.content.take() {
        Some(MessageContent::Text(t)) if !t.is_empty() => vec![ContentPart {
            part_type: "text".to_string(),
            text: Some(t),
            image_url: None,
            input_audio: None,
            extra: Default::default(),
        }],
        Some(MessageContent::Parts(parts)) => parts,
        _ => Vec::new(),
    };
    merged.extend(image_parts);
    msg.content = Some(MessageContent::Parts(merged));
    true
}

/// Apply the full OpenRouter `ToOpenAIMessage` transformation in place:
/// reasoning precedence + images merge. Mirrors Go model.go:63-105.
/// Returns `true` if any transformation was applied.
pub fn normalize_choice_message(msg: &mut ChatMessage) -> bool {
    let mut changed = apply_reasoning_precedence(msg);
    if merge_images_into_content(msg) {
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::ChatRequest;
    use conduit_llm::LlmRequestPayload;
    use serde_json::json;

    type TestResult = Result<(), serde_json::Error>;

    // ---- helpers ----

    fn build_request(
        rt: RequestType,
        model: Option<&str>,
        messages: Vec<ChatMessage>,
    ) -> LlmRequest {
        let mut chat = ChatRequest::default();
        chat.messages = messages;
        LlmRequest {
            request_type: rt,
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
    // normalize_base_url (mirrors Go outbound.go:64 — trim trailing /)
    // =======================================================================
    #[test]
    fn normalize_base_url_trims_trailing_slash() {
        assert_eq!(
            normalize_base_url("https://openrouter.ai/"),
            "https://openrouter.ai"
        );
        assert_eq!(
            normalize_base_url("https://openrouter.ai"),
            "https://openrouter.ai"
        );
        // Multiple trailing slashes: Go's TrimSuffix removes only one; the Rust
        // trim_end_matches removes all. The Go code only ever sets BaseURL via
        // `strings.TrimSuffix(config.BaseURL, "/")` so a single slash is the
        // real-world input; we mirror Go's single-suffix behavior exactly.
        assert_eq!(
            normalize_base_url("https://openrouter.ai//"),
            "https://openrouter.ai"
        );
    }

    #[test]
    fn normalize_base_url_does_not_append_version() {
        // OpenRouter does NOT append /v1 — this is the key difference from
        // deepseek/moonshot/zai wrappers.
        assert_eq!(
            normalize_base_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1"
        );
    }

    // =======================================================================
    // require_model (mirrors Go outbound.go:83-85)
    // =======================================================================
    #[test]
    fn require_model_rejects_empty_string() {
        let req = build_request(RequestType::Chat, Some(""), vec![user_msg("hi")]);
        let err = match require_model(&req) {
            Ok(()) => panic!("expected Err for empty model"),
            Err(e) => e,
        };
        assert!(err.message.contains("model is required"));
    }

    #[test]
    fn require_model_rejects_missing_model() {
        let req = build_request(RequestType::Chat, None, vec![user_msg("hi")]);
        let err = match require_model(&req) {
            Ok(()) => panic!("expected Err for missing model"),
            Err(e) => e,
        };
        assert!(err.message.contains("model is required"));
    }

    #[test]
    fn require_model_accepts_non_empty_model() -> TransformerResult<()> {
        let req = build_request(RequestType::Chat, Some("gpt-4"), vec![user_msg("hi")]);
        require_model(&req)
    }

    // =======================================================================
    // classify_request_type (mirrors Go outbound.go:87-102)
    // =======================================================================
    #[test]
    fn classify_chat_as_chat() -> TransformerResult<()> {
        let req = build_request(RequestType::Chat, Some("m"), vec![user_msg("hi")]);
        assert_eq!(classify_request_type(&req)?, OpenRouterRequestKind::Chat);
        Ok(())
    }

    #[test]
    fn classify_image_as_image() -> TransformerResult<()> {
        let req = build_request(RequestType::Image, Some("m"), vec![]);
        assert_eq!(classify_request_type(&req)?, OpenRouterRequestKind::Image);
        Ok(())
    }

    #[test]
    fn classify_embedding_speech_transcription_translation_as_delegate() -> TransformerResult<()> {
        for rt in [
            RequestType::Embedding,
            RequestType::Speech,
            RequestType::Transcription,
            RequestType::Translation,
        ] {
            let req = build_request(rt, Some("m"), vec![]);
            assert_eq!(
                classify_request_type(&req)?,
                OpenRouterRequestKind::Delegate,
                "request_type={:?}",
                rt
            );
        }
        Ok(())
    }

    #[test]
    fn classify_compact_returns_responses_api_error() {
        let req = build_request(RequestType::Compact, Some("m"), vec![user_msg("hi")]);
        let err = match classify_request_type(&req) {
            Ok(k) => panic!("expected Err for Compact, got {k:?}"),
            Err(e) => e,
        };
        assert!(
            err.message
                .contains("compact is only supported by OpenAI Responses API")
        );
    }

    #[test]
    fn classify_rerank_returns_not_supported_error() {
        let req = build_request(RequestType::Rerank, Some("m"), vec![user_msg("hi")]);
        let err = match classify_request_type(&req) {
            Ok(k) => panic!("expected Err for Rerank, got {k:?}"),
            Err(e) => e,
        };
        assert!(err.message.contains("rerank is not supported"));
    }

    // =======================================================================
    // require_messages (mirrors Go outbound.go:104-106)
    // =======================================================================
    #[test]
    fn require_messages_rejects_empty_messages() {
        let req = build_request(RequestType::Chat, Some("m"), vec![]);
        let err = match require_messages(&req) {
            Ok(()) => panic!("expected Err for empty messages"),
            Err(e) => e,
        };
        assert!(err.message.contains("messages are required"));
    }

    #[test]
    fn require_messages_accepts_non_empty() -> TransformerResult<()> {
        let req = build_request(RequestType::Chat, Some("m"), vec![user_msg("hi")]);
        require_messages(&req)
    }

    // =======================================================================
    // apply_reasoning_precedence (mirrors Go model.go:64-75)
    // =======================================================================
    #[test]
    fn reasoning_details_take_precedence_over_reasoning() {
        let mut msg = user_msg("hi");
        msg.extra
            .insert("reasoning".to_string(), json!("top-level-reasoning"));
        msg.extra.insert(
            "reasoning_details".to_string(),
            json!([
                {"type": "text", "text": "detail-part-1", "format": "text", "index": 0},
                {"type": "text", "text": "detail-part-2", "format": "text", "index": 1},
            ]),
        );
        assert!(apply_reasoning_precedence(&mut msg));
        // Concatenation of all detail.text values.
        assert_eq!(
            msg.extra.get("reasoning_content").and_then(|v| v.as_str()),
            Some("detail-part-1detail-part-2")
        );
    }

    #[test]
    fn reasoning_top_level_used_when_details_absent() {
        let mut msg = user_msg("hi");
        msg.extra
            .insert("reasoning".to_string(), json!("just-reasoning"));
        assert!(apply_reasoning_precedence(&mut msg));
        assert_eq!(
            msg.extra.get("reasoning_content").and_then(|v| v.as_str()),
            Some("just-reasoning")
        );
    }

    #[test]
    fn reasoning_details_empty_array_falls_back_to_reasoning() {
        let mut msg = user_msg("hi");
        msg.extra.insert("reasoning".to_string(), json!("fallback"));
        msg.extra.insert("reasoning_details".to_string(), json!([]));
        assert!(apply_reasoning_precedence(&mut msg));
        assert_eq!(
            msg.extra.get("reasoning_content").and_then(|v| v.as_str()),
            Some("fallback")
        );
    }

    #[test]
    fn reasoning_precedence_noop_when_both_absent() {
        let mut msg = user_msg("hi");
        assert!(!apply_reasoning_precedence(&mut msg));
        assert!(msg.extra.get("reasoning_content").is_none());
    }

    #[test]
    fn reasoning_precedence_noop_when_reasoning_null() {
        let mut msg = user_msg("hi");
        msg.extra.insert("reasoning".to_string(), Value::Null);
        assert!(!apply_reasoning_precedence(&mut msg));
    }

    // =======================================================================
    // merge_images_into_content (mirrors Go model.go:77-92)
    // =======================================================================
    #[test]
    fn images_merged_into_content_with_text_prefix() {
        let mut msg = ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(MessageContent::Text("image follows".to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: {
                let mut e = conduit_llm::model::ExtensionMap::new();
                e.insert(
                    "images".to_string(),
                    json!([{"image_url": {"url": "data:image/png;base64,abc"}}]),
                );
                e
            },
        };
        assert!(merge_images_into_content(&mut msg));
        match &msg.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2, "text part + image part");
                assert_eq!(parts[0].part_type, "text");
                assert_eq!(parts[0].text.as_deref(), Some("image follows"));
                assert_eq!(parts[1].part_type, "image_url");
                assert_eq!(
                    parts[1]
                        .image_url
                        .as_ref()
                        .and_then(|v| v.get("url"))
                        .and_then(|v| v.as_str()),
                    Some("data:image/png;base64,abc")
                );
            }
            other => panic!("expected Parts, got {other:?}"),
        }
        // images key consumed
        assert!(msg.extra.get("images").is_none());
    }

    #[test]
    fn images_merged_into_existing_parts() {
        let mut msg = ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(MessageContent::Parts(vec![ContentPart {
                part_type: "text".to_string(),
                text: Some("orig".to_string()),
                image_url: None,
                input_audio: None,
                extra: Default::default(),
            }])),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: {
                let mut e = conduit_llm::model::ExtensionMap::new();
                e.insert(
                    "images".to_string(),
                    json!([{"image_url": {"url": "data:image/png;base64,xyz"}}]),
                );
                e
            },
        };
        assert!(merge_images_into_content(&mut msg));
        match &msg.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[1].part_type, "image_url");
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    #[test]
    fn images_empty_array_is_noop() {
        let mut msg = user_msg("hi");
        msg.extra.insert("images".to_string(), json!([]));
        assert!(!merge_images_into_content(&mut msg));
        // content unchanged
        assert!(matches!(msg.content, Some(MessageContent::Text(_))));
    }

    #[test]
    fn images_absent_is_noop() {
        let mut msg = user_msg("hi");
        assert!(!merge_images_into_content(&mut msg));
    }

    #[test]
    fn images_with_empty_text_content_yields_only_image_parts() {
        let mut msg = ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(MessageContent::Text(String::new())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: {
                let mut e = conduit_llm::model::ExtensionMap::new();
                e.insert(
                    "images".to_string(),
                    json!([{"image_url": {"url": "data:image/png;base64,only"}}]),
                );
                e
            },
        };
        assert!(merge_images_into_content(&mut msg));
        match &msg.content {
            // Empty text is dropped (Go: the else branch uses MultipleContent
            // directly when Content is empty, so only image parts remain).
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1, "only the image part");
                assert_eq!(parts[0].part_type, "image_url");
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    // =======================================================================
    // normalize_choice_message — combined reasoning + images
    // =======================================================================
    #[test]
    fn normalize_choice_message_applies_both_reasoning_and_images() -> TestResult {
        let mut msg = ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(MessageContent::Text("c".to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: {
                let mut e = conduit_llm::model::ExtensionMap::new();
                e.insert("reasoning".to_string(), json!("think"));
                e.insert(
                    "images".to_string(),
                    json!([{"image_url": {"url": "data:image/png;base64,z"}}]),
                );
                e
            },
        };
        assert!(normalize_choice_message(&mut msg));
        assert_eq!(
            msg.extra.get("reasoning_content").and_then(|v| v.as_str()),
            Some("think")
        );
        assert!(matches!(msg.content, Some(MessageContent::Parts(_))));
        Ok(())
    }

    #[test]
    fn normalize_choice_message_noop_when_no_reasoning_or_images() {
        let mut msg = user_msg("plain");
        assert!(!normalize_choice_message(&mut msg));
    }
}
