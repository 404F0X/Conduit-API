//! Shared OpenAI-compatible transformer base (RUST-P7-008 S10).
//!
//! Mirrors Go `conduit/llm/transformer/shared/` (the OpenAI-compatible
//! helpers) plus the common request-build skeleton that every thin provider
//! wrapper (`deepseek`, `moonshot`, `zai`, `bailian`, `longcat`, `nanogpt`,
//! `openrouter`, `modelscope`, `fireworks`, `cerebras`) customizes. Each
//! wrapper implements *only* its provider-specific deltas as small tested
//! functions (S12) and composes the helpers here for the rest.
//!
//! ## Design
//!
//! Go's wrappers share logic two ways:
//! 1. **`shared/`** package — provider-agnostic OpenAI helpers
//!    (`EncodeOpenAIEncryptedContent` / `DecodeOpenAIEncryptedContent` here
//!    reduce to passthroughs that defer to the signature-guess helper, which
//!    isn't ported yet — left as a documented stub).
//! 2. **embedding `transformer.Outbound`** — each wrapper embeds the full
//!    OpenAI outbound transformer and only overrides `TransformRequest` (plus
//!    sometimes `TransformStream`/`TransformResponse` to dispatch to a
//!    completion sub-transformer). In Rust we don't have the full
//!    `OutboundTransformer` impl yet (RUST-P7-002 S04/S08/S09 owns that), so
//!    this module exposes the *pure* pieces a wrapper overrides:
//!    request-type gating, `response_format` downgrade, base URL
//!    normalization, and the bearer-auth + JSON header scaffold.
//!
//! Future agents porting the remaining 8 providers follow the deepseek /
//! moonshot pattern: call [`validate_chat_request_type`],
//! [`downgrade_json_schema_to_object`], [`normalize_v1_base_url`], and
//! [`bearer_json_request`] from here, then add only their unique delta.

use conduit_core::ConduitError;
use conduit_llm::{ApiFormat, HttpRequest, LlmRequest, RequestType};

use crate::TransformerResult;
use crate::openai_outbound::normalize_base_url;

/// Provider configuration for an OpenAI-compatible wrapper. Mirrors the
/// `Config` structs in Go's `deepseek/outbound.go:20-23` and
/// `moonshot/outbound.go:17-21` (the shared shape — `base_url` + `api_key`).
///
/// `api_key_provider` is collapsed to a plain `api_key: String` for the same
/// reason [`crate::openai_outbound::Config`] gives: the pure helpers here
/// don't perform I/O, and a per-request key can be threaded in later by the
/// full transformer impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiCompatibleConfig {
    /// Base URL for the provider's OpenAI-compatible API, e.g.
    /// `https://api.deepseek.com/v1`. Required.
    pub base_url: String,
    /// API key sent as `Authorization: Bearer <key>`. Required.
    pub api_key: String,
}

impl OpenAiCompatibleConfig {
    /// Validate non-empty base URL + API key, mirroring Go's
    /// `validateConfig` lower-half (outbound.go:121-129). The wrapper
    /// constructors call this before building the inner OpenAI transformer.
    pub fn validate(&self) -> TransformerResult<()> {
        if self.base_url.is_empty() {
            return Err(ConduitError::invalid_request("base URL is required"));
        }
        if self.api_key.is_empty() {
            return Err(ConduitError::invalid_request("API key is required"));
        }
        Ok(())
    }
}

/// Validate the request type for an OpenAI-compatible chat wrapper.
///
/// Mirrors the `switch llmReq.RequestType` blocks at the top of Go
/// `deepseek.OutboundTransformer.TransformRequest` (outbound.go:87-97) and
/// `moonshot.OutboundTransformer.TransformRequest` (outbound.go:70-77). Both
/// wrappers:
/// * accept `Chat` or empty (`""`),
/// * reject `Compact` with a "compact is only supported by OpenAI Responses
///   API" message,
/// * reject every other type with `"<type> is not supported"`.
///
/// DeepSeek additionally dispatches `Completion` to a sub-transformer — that
/// branch lives in the deepseek wrapper, not here. This helper returns the
/// *rejection* errors only; the wrapper decides what to do with `Chat` /
/// `Completion`.
///
/// Returns:
/// * `Ok(())` for `Chat` (or unrecognized-but-not-explicitly-rejected types
///   — the Go `switch` only lists Chat/Completion/Compact, falling through
///   the default arm otherwise; here we model that as "not an error, let the
///   caller proceed").
/// * `Err(ErrInvalidRequest)` for `Compact`.
/// * `Err(ErrInvalidRequest)` for any other *explicit* request type.
pub fn validate_chat_request_type(req: &LlmRequest) -> TransformerResult<()> {
    match req.request_type {
        RequestType::Chat => Ok(()),
        RequestType::Compact => Err(ConduitError::invalid_request(
            "compact is only supported by OpenAI Responses API",
        )),
        other => Err(ConduitError::invalid_request(format!(
            "{} is not supported",
            other.as_str()
        ))),
    }
}

/// Downgrade a `response_format` of `json_schema` to `json_object`, in place.
///
/// Mirrors the shared block present in both Go
/// `deepseek.OutboundTransformer.TransformRequest` (outbound.go:105-108) and
/// `moonshot.OutboundTransformer.TransformRequest` (outbound.go:87-90):
///
/// ```go
/// if oaiReq.ResponseFormat != nil && oaiReq.ResponseFormat.Type == "json_schema" {
///     oaiReq.ResponseFormat.Type = "json_object"
///     oaiReq.ResponseFormat.JSONSchema = nil
/// }
/// ```
///
/// In the Rust model [`conduit_llm::model::ChatRequest::response_format`] is a
/// free-form `Value`, so the "JSONSchema = nil" half is realized by removing
/// the `json_schema` key. The function is a no-op when:
/// * `response_format` is `None`,
/// * `response_format` is not an object,
/// * `response_format.type` is missing or not `"json_schema"`.
///
/// Returns `true` if a downgrade was applied (handy for tests).
pub fn downgrade_json_schema_to_object(response_format: &mut Option<serde_json::Value>) -> bool {
    let Some(rf) = response_format.as_mut() else {
        return false;
    };
    let Some(obj) = rf.as_object_mut() else {
        return false;
    };
    let is_json_schema = obj
        .get("type")
        .is_some_and(|t| t.as_str() == Some("json_schema"));
    if !is_json_schema {
        return false;
    }
    obj.insert(
        "type".to_string(),
        serde_json::Value::String("json_object".to_string()),
    );
    obj.remove("json_schema");
    true
}

/// Normalize a wrapper base URL by appending `/v1` when missing, mirroring
/// Go `transformer.NormalizeBaseURL(config.BaseURL, "v1")` invoked in both
/// `deepseek.NewOutboundTransformerWithConfig` (outbound.go:55) and
/// `moonshot.NewOutboundTransformerWithConfig` (outbound.go:55).
///
/// Thin convenience over [`crate::openai_outbound::normalize_base_url`] so
/// wrapper code reads like the Go source.
pub fn normalize_v1_base_url(base_url: &str) -> String {
    normalize_base_url(base_url.to_string(), "v1")
}

/// Build the standard bearer-auth + JSON-content OpenAI-compatible HTTP
/// request scaffold, mirroring the tail of Go
/// `moonshot.OutboundTransformer.TransformRequest` (outbound.go:96-118) and
/// `deepseek.OutboundTransformer.TransformRequest` (outbound.go:139-159).
///
/// The wrapper supplies the already-serialized request `body` and the
/// normalized `base_url` (callers should run it through
/// [`normalize_v1_base_url`] first). This helper fills:
/// * `method` = `POST`,
/// * `url` = `<base_url>/chat/completions`,
/// * `headers` = `Content-Type: application/json`, `Accept: application/json`,
/// * `auth` = bearer + `api_key`,
/// * `api_format` = [`ApiFormat::OpenAiChatCompletions`],
/// * `request_type` forwarded from `llm_req.request_type`.
pub fn bearer_json_request(
    base_url: &str,
    body: serde_json::Value,
    api_key: &str,
    llm_req: &LlmRequest,
) -> HttpRequest {
    let mut headers = conduit_llm::model::HeaderMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("Accept".to_string(), "application/json".to_string());

    HttpRequest {
        method: "POST".to_string(),
        path: format!("{base_url}/chat/completions"),
        request_type: Some(llm_req.request_type),
        api_format: Some(ApiFormat::OpenAiChatCompletions),
        json_body: Some(body),
        headers,
        auth: Some(conduit_llm::HttpAuth {
            scheme: "bearer".to_string(),
            token: Some(api_key.to_string()),
            ..conduit_llm::HttpAuth::default()
        }),
        ..HttpRequest::default()
    }
}

/// Require at least one chat message, mirroring Go
/// `if len(llmReq.Messages) == 0 { return ErrInvalidRequest("messages are required") }`
/// present in both wrappers (deepseek outbound.go:99-101, moonshot
/// outbound.go:79-81).
///
/// Returns `Err(ErrInvalidRequest)` when the payload is a chat with an empty
/// `messages` vector. Non-chat payloads are passed through (the wrapper is
/// responsible for dispatching them earlier).
pub fn require_chat_messages(req: &LlmRequest) -> TransformerResult<()> {
    if let conduit_llm::LlmRequestPayload::Chat(chat) = &req.payload {
        if chat.messages.is_empty() {
            return Err(ConduitError::invalid_request("messages are required"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Go `shared/openai.go` parity helpers
// ---------------------------------------------------------------------------

/// Encode raw OpenAI encrypted content for storage. Mirrors Go
/// `shared.EncodeOpenAIEncryptedContent` (openai.go:5-10).
///
/// OpenAI `encrypted_content` is already base64-encoded, so this is a
/// passthrough. The `Option<String>` mirrors Go's `*string` (nil → None).
pub fn encode_openai_encrypted_content(content: Option<String>) -> Option<String> {
    content
}

/// Decode OpenAI encrypted content. Mirrors Go
/// `shared.DecodeOpenAIEncryptedContent` (openai.go:12-26).
///
/// Returns the raw value only if the blob is recognized as an OpenAI
/// signature; returns `None` for signatures from other providers or unknown
/// formats.
///
/// **Pending source snapshot:** the Go helper delegates to
/// `GuessSignatureProvider`, which lives in `shared/signature.go`. That
/// signature-classification table hasn't been ported yet, so for now this
/// returns the content unchanged (matching the nil-input short-circuit and
/// the OpenAI-recognized path). When `signature.rs` lands, wire the
/// provider-classification check in here.
pub fn decode_openai_encrypted_content(content: Option<String>) -> Option<String> {
    // TODO(RUST-P7-008 follow-up): once `shared/signature.go` is ported to a
    // `signature` module, gate the passthrough on
    // `GuessSignatureProvider(content).Provider == ProviderOpenAI`.
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ChatMessage, ChatRequest, LlmRequestPayload};
    use serde_json::json;

    fn chat_request(messages: Vec<ChatMessage>) -> LlmRequest {
        let mut chat = ChatRequest::default();
        chat.messages = messages;
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("test-model".to_string()),
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

    // --- validate_chat_request_type ---

    #[test]
    fn chat_request_type_is_accepted() {
        let req = chat_request(vec![]);
        assert!(validate_chat_request_type(&req).is_ok());
    }

    #[test]
    fn compact_request_type_is_rejected_with_responses_api_message() {
        let mut req = chat_request(vec![]);
        req.request_type = RequestType::Compact;
        let err = match validate_chat_request_type(&req) {
            Ok(()) => panic!("expected Err for Compact request type"),
            Err(e) => e,
        };
        assert!(
            err.message
                .contains("compact is only supported by OpenAI Responses API"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn embedding_request_type_is_rejected_as_not_supported() {
        let mut req = chat_request(vec![]);
        req.request_type = RequestType::Embedding;
        let err = match validate_chat_request_type(&req) {
            Ok(()) => panic!("expected Err for Embedding request type"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("embedding is not supported"),
            "got: {}",
            err.message
        );
    }

    // --- downgrade_json_schema_to_object ---

    #[test]
    fn json_schema_is_downgraded_to_json_object_and_schema_dropped() -> Result<(), serde_json::Error>
    {
        let mut rf = Some(json!({
            "type": "json_schema",
            "json_schema": {"name": "x", "schema": {"type": "object"}}
        }));
        let changed = downgrade_json_schema_to_object(&mut rf);
        assert!(changed);
        assert_eq!(rf, Some(json!({"type": "json_object"})));
        Ok(())
    }

    #[test]
    fn json_object_is_left_unchanged() {
        let mut rf = Some(json!({"type": "json_object"}));
        let changed = downgrade_json_schema_to_object(&mut rf);
        assert!(!changed);
        assert_eq!(rf, Some(json!({"type": "json_object"})));
    }

    #[test]
    fn text_format_is_left_unchanged() {
        let mut rf = Some(json!({"type": "text"}));
        assert!(!downgrade_json_schema_to_object(&mut rf));
    }

    #[test]
    fn missing_type_is_left_unchanged() {
        let mut rf = Some(json!({"foo": "bar"}));
        assert!(!downgrade_json_schema_to_object(&mut rf));
    }

    #[test]
    fn none_response_format_is_a_noop() {
        let mut rf: Option<serde_json::Value> = None;
        assert!(!downgrade_json_schema_to_object(&mut rf));
        assert!(rf.is_none());
    }

    // --- normalize_v1_base_url ---

    #[test]
    fn base_url_with_v1_suffix_is_left_unchanged() {
        assert_eq!(
            normalize_v1_base_url("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1"
        );
    }

    #[test]
    fn base_url_without_v1_gets_v1_appended() {
        assert_eq!(
            normalize_v1_base_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1"
        );
    }

    // --- require_chat_messages ---

    #[test]
    fn empty_messages_is_rejected() {
        let req = chat_request(vec![]);
        let err = match require_chat_messages(&req) {
            Ok(()) => panic!("expected Err for empty messages"),
            Err(e) => e,
        };
        assert!(err.message.contains("messages are required"));
    }

    #[test]
    fn non_empty_messages_is_accepted() {
        let req = chat_request(vec![user_msg("hi")]);
        assert!(require_chat_messages(&req).is_ok());
    }

    // --- bearer_json_request ---

    #[test]
    fn bearer_json_request_builds_expected_scaffold() {
        let req = chat_request(vec![user_msg("hi")]);
        let http = bearer_json_request(
            "https://api.moonshot.cn/v1",
            json!({"model": "moonshot-v1-8k"}),
            "test-api-key",
            &req,
        );
        assert_eq!(http.method, "POST");
        assert_eq!(http.path, "https://api.moonshot.cn/v1/chat/completions");
        assert_eq!(
            http.headers.get("Content-Type"),
            Some(&"application/json".to_string())
        );
        assert_eq!(
            http.headers.get("Accept"),
            Some(&"application/json".to_string())
        );
        assert_eq!(http.request_type, Some(RequestType::Chat));
        assert_eq!(http.api_format, Some(ApiFormat::OpenAiChatCompletions));
        match http.auth.as_ref() {
            Some(auth) => {
                assert_eq!(auth.scheme, "bearer");
                assert_eq!(auth.token.as_deref(), Some("test-api-key"));
            }
            None => panic!("auth missing"),
        }
        assert_eq!(http.json_body, Some(json!({"model": "moonshot-v1-8k"})));
    }

    // --- shared/openai.go parity ---

    #[test]
    fn encode_openai_encrypted_content_is_a_passthrough() {
        assert_eq!(encode_openai_encrypted_content(None), None);
        assert_eq!(
            encode_openai_encrypted_content(Some("abc".to_string())),
            Some("abc".to_string())
        );
    }

    #[test]
    fn decode_openai_encrypted_content_passes_through_until_signature_port() {
        assert_eq!(decode_openai_encrypted_content(None), None);
        assert_eq!(
            decode_openai_encrypted_content(Some("abc".to_string())),
            Some("abc".to_string())
        );
    }
}
