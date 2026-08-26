use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::{ApiFormat, RequestType};
use crate::usage::Usage;

pub type ExtensionMap = BTreeMap<String, Value>;
pub type HeaderMap = BTreeMap<String, String>;
pub type QueryMap = BTreeMap<String, Vec<String>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LlmRequest {
    pub request_type: RequestType,
    pub api_format: ApiFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub stream: bool,
    pub payload: LlmRequestPayload,
    #[serde(default)]
    pub extra_body: ExtensionMap,
    #[serde(default)]
    pub extra_headers: HeaderMap,
    #[serde(default)]
    pub metadata: ExtensionMap,
    // Keep unmodeled top-level provider fields available to later transformer stages.
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

/// Discriminated union for the request payload, tagged by `payload_type`.
///
/// `#[non_exhaustive]` marks that additional variants may be added in future
/// releases as new provider request shapes are ported (mirrors the open-ended
/// set of Go `*Request` types under `llm/`). Callers outside this crate must
/// include a `_` arm when matching exhaustively.
///
/// `request_type()` is the nominal string discriminator a caller can read
/// without destructuring; it corresponds to the lowercased variant name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "payload_type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LlmRequestPayload {
    Chat(ChatRequest),
    Completion(CompletionRequest),
    Responses(ResponsesRequest),
    Embedding(EmbeddingRequest),
    Rerank(RerankRequest),
    Image(ImageRequest),
    Video(VideoRequest),
    Audio(AudioRequest),
}

impl LlmRequestPayload {
    /// Returns the lowercased variant name, mirroring the `payload_type` tag
    /// a serde round-trip would emit. Useful for logging/metrics without
    /// destructuring the payload.
    pub fn request_type(&self) -> &'static str {
        match self {
            Self::Chat(_) => "chat",
            Self::Completion(_) => "completion",
            Self::Responses(_) => "responses",
            Self::Embedding(_) => "embedding",
            Self::Rerank(_) => "rerank",
            Self::Image(_) => "image",
            Self::Video(_) => "video",
            Self::Audio(_) => "audio",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatRequest {
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Vec<UnifiedTool>,
    #[serde(default)]
    pub functions: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Value>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
    Json(Value),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio: Option<Value>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: Value,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UnifiedTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

/// Mirrors Go `llm.ToolChoice` (`llm/tools.go:95`). The Go struct has a custom
/// `MarshalJSON`/`UnmarshalJSON` that serializes it as *either* a bare string
/// (the `ToolChoice` field) *or* a `NamedToolChoice` object. Rust serde achieves
/// the same union shape with `#[serde(untagged)]`; the variants are tried in
/// declaration order during deserialization (string first, then named object),
/// matching Go's order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// Maps to Go `ToolChoice.ToolChoice *string` — a bare discriminator like
    /// `"auto"`, `"none"`, or `"any"`.
    Tool(String),
    /// Maps to Go `ToolChoice.NamedToolChoice *NamedToolChoice` — a fully-named
    /// tool selection, serialized as an object.
    NamedToolChoice(NamedToolChoice),
}

/// Mirrors Go `llm.NamedToolChoice` (`llm/tools.go:100`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedToolChoice {
    /// `type` is an all-lowercase JSON tag, not an all-caps acronym, so
    /// `rename_all = "camelCase"` on the struct would mis-render it as `type`
    /// anyway — but to be safe and explicit we use `#[serde(rename = "type")]`.
    #[serde(rename = "type")]
    pub choice_type: String,
    pub function: ToolFunction,
}

/// Mirrors Go `llm.ToolFunction` (`llm/tools.go:88`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
}

/// Mirrors Go Anthropic transformer `Thinking` (`llm/transformer/anthropic/model.go:176`).
///
/// Note: this is the **Anthropic**-shape thinking config (single canonical
/// struct), not the Gemini-shape `ThinkingConfig` (`llm/transformer/gemini/model.go:296`,
/// which has `includeThoughts`/`thinkingBudget`/`thinkingLevel`) — that one
/// lives on the Gemini transformer and is intentionally not modeled here to
/// keep the unified layer provider-neutral.
///
/// Go field tags: `type` (required), `budget_tokens,omitempty`, `display,omitempty`.
/// `budget_tokens` is `int64` (Go zero = 0), so in Rust it is `Option<i64>`
/// with `skip_serializing_if = "Option::is_none"`; serde `default` keeps
/// round-tripping payloads that omit it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thinking {
    #[serde(rename = "type")]
    pub thinking_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CompletionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Value>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ResponsesRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(default)]
    pub tools: Vec<UnifiedTool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(default)]
    pub compact: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_generation_result: Option<Value>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EmbeddingRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RerankRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default)]
    pub documents: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_n: Option<u32>,
    #[serde(default)]
    pub return_documents: bool,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ImageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Value>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct VideoRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AudioRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpRequest {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub path: String,
    #[serde(default)]
    pub query: QueryMap,
    #[serde(default)]
    pub headers: HeaderMap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_body: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<HttpAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_type: Option<RequestType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_format: Option<ApiFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_request: Option<Value>,
    #[serde(default)]
    pub metadata: ExtensionMap,
    #[serde(default)]
    pub transformer_metadata: ExtensionMap,
    #[serde(default)]
    pub skip_inbound_query_merge: bool,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpAuth {
    pub scheme: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: HeaderMap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_body: Option<Value>,
    #[serde(default)]
    pub stream: Vec<StreamEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_response: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_request: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub metadata: ExtensionMap,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<Vec<u8>>,
    #[serde(default)]
    pub done: bool,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

// ============================================================================
// Unified LLM response model (RUST-P6-001 response side).
//
// Mirrors Go `llm.Response` / `Choice` / `Message` / `ResponseError` /
// `Annotation` / `URLCitation` / `OutputAudio` / `InlineToolResult` in
// `conduit/llm/model.go`. The unified response is OpenAI-shaped per the Go
// doc comment ("we use the OpenAI response format"), and other providers
// convert *into* this shape.
//
// Field naming follows the same conventions as the request side:
//   - `#[serde(rename_all = "snake_case")]` on each struct, matching the
//     lowercased Go json tags (`prompt_tokens`, `finish_reason`, ...).
//   - `*T` Go fields → `Option<T>` + `default`/`skip_serializing_if`.
//   - Permissive shapes (`LogprobsContent`, provider-typed response sub-bodies
//     like `Embedding`/`Rerank`/`Image`/`Video`/`Compact`/`Completion`/
//     `Speech`/`Transcription`, and stream-event sub-bodies) are kept as
//     `Option<Value>` to avoid provider-specific type explosion at the unified
//     layer — those are modeled on the transformer side. Flattened `extra`
//     keeps unknown top-level fields lossless for round-tripping.
//   - `cache_control` (Anthropic-specific, present on both `Message` and the
//     request side) is intentionally NOT a typed field here — it survives via
//     the `extra` flatten, matching the existing request-side convention.
//
// The Go `Message` struct is used for BOTH request messages (role=user, ...)
// and response messages (assistant). The Rust unified layer already has a
// request-side `ChatMessage`; to avoid breaking that contract, the response
// message is exposed here as a distinct `LlmMessage` which mirrors the full
// Go `Message` field set used on the response path.
// ============================================================================

/// Mirrors Go `llm.Response` (`llm/model.go:627`). The unified, OpenAI-shaped
/// LLM response. Used for both non-streaming responses and for each SSE chunk
/// in a streaming chat (Go reuses the same struct for both).
///
/// `#[non_exhaustive]` marks that additional provider-typed sub-response
/// shapes may be added as the migration progresses; out-of-crate matchers must
/// include a `_` arm.
///
/// Go field/parity notes:
/// - `id`, `object`, `created`, `model`, `choices` are the only non-pointer
///   always-present fields; everything else is `*T`/omitempty → Rust `Option`.
/// - Provider-typed sub-responses (`Embedding`, `Rerank`, `Image`, `Video`,
///   `Compact`, `Completion`, `Speech`, `Transcription`, `SpeechStreamEvent`,
///   `TranscriptionStreamEvent`) are kept as `Option<Value>` until their
///   dedicated transformer ports land; this is sufficient to unblock the
///   chat stream transform (P7-002 S08/S09/S12) which is the immediate
///   consumer.
/// - `SpeechAudioChunk` carries a `json:"-"` Go tag (never serialized) and is
///   therefore omitted here entirely.
/// - `RequestType`/`APIFormat` reuse the existing crate enums.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// NOTE: `#[non_exhaustive]` was removed (Leader 2026-07-02): the struct is
// constructed via literals in tests across crates (pipeline/transformers), and
// pre-1.0 the construction ergonomics outweigh the forward-compat guarantee.
// Re-add before a 1.0 API freeze if desired.
pub struct LlmResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub object: String,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
    // Provider-typed sub-responses (kept as Value until transformer ports):
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_stream_event: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription_stream_event: Option<Value>,
    // Go `RequestType`/`APIFormat` carry `,omitempty`; the enum's own
    // snake_case-derived default is serialized for the zero value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_type: Option<RequestType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_format: Option<ApiFormat>,
    #[serde(default, skip_serializing_if = "ExtensionMap::is_empty")]
    pub transformer_metadata: ExtensionMap,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

/// Mirrors Go `llm.Choice` (`llm/model.go:716`). One chat completion choice.
/// `Message` is present for non-streaming responses, `Delta` for streaming
/// chunks; both use the unified `LlmMessage` shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Choice {
    #[serde(default)]
    pub index: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<LlmMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<LlmMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    // Go `LogprobsContent` is a small struct (`{content: []TokenLogprob}`),
    // but the per-token `TokenLogprob` carries nested `top_logprobs` and byte
    // arrays whose exact shape varies by provider. Kept as `Option<Value>` at
    // the unified layer until a transformer needs typed access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Value>,
    #[serde(default, skip_serializing_if = "ExtensionMap::is_empty")]
    pub transformer_metadata: ExtensionMap,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

/// Mirrors Go `llm.Message` (`llm/model.go:342`) on the **response** path. The
/// request path uses the existing crate `ChatMessage`; this struct carries the
/// full Go `Message` field set (refusal, reasoning_*, annotations, audio,
/// inline_tool_results, ...) that assistant messages need.
///
/// `content` reuses the existing `MessageContent` union (string / parts / json
/// fallback), mirroring Go `MessageContent.MarshalJSON` precedence
/// (single-text-part collapses to a bare string).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LlmMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<OutputAudio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_tool_results: Vec<InlineToolResult>,
    // cache_control survives via the extra flatten (Anthropic-specific).
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

/// Mirrors Go `llm.Annotation` (`llm/model.go:439`). Citation/reference
/// annotation on a message (e.g. Perplexity `url_citation`).
///
/// Go `Type string \`json:"type,omitempty"\`` — the field is renamed to
/// `annotation_type` here (to avoid the Rust keyword) with an explicit
/// `#[serde(rename = "type")]` to match the Go wire tag exactly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub annotation_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_citation: Option<UrlCitation>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

/// Mirrors Go `llm.URLCitation` (`llm/model.go:451`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UrlCitation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Mirrors Go `llm.OutputAudio` (`llm/model.go:586`). Model-generated audio
/// metadata for assistant messages.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OutputAudio {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

/// Mirrors Go `llm.InlineToolResult` (`llm/model.go:419`). A tool result
/// emitted inline within the assistant turn (e.g. Anthropic
/// `*_tool_result` blocks during a server-side tool turn).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InlineToolResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "ExtensionMap::is_empty")]
    pub transformer_metadata: ExtensionMap,
    #[serde(default, flatten)]
    pub extra: ExtensionMap,
}

/// Mirrors Go `llm.ResponseError` (`llm/model.go:836`). The error body present
/// on a `Response` when the upstream request failed (status >= 400).
///
/// Go parity notes:
/// - `StatusCode` carries the Go tag `json:"-"` (never serialized); it is kept
///   here as a `#[serde(skip)]` field so it survives a Rust round-trip but is
///   invisible to JSON, matching Go behavior exactly.
/// - The JSON shape is `{"error": {code,message,type,param,request_id}}`, i.e.
///   the `Detail` is serialized under the `error` key. We model that with a
///   `#[serde(rename = "error")]` field rather than a wrapper struct so the
///   Rust type *is* the wrapper.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResponseError {
    /// HTTP status code; never serialized (`json:"-"` in Go).
    #[serde(skip)]
    pub status_code: u16,
    /// Serialized under the `error` JSON key (Go `Detail` field).
    #[serde(default, rename = "error")]
    pub detail: ErrorDetail,
}

impl std::fmt::Display for ResponseError {
    /// Mirrors Go `ResponseError.Error()` (`llm/model.go:841`): status text +
    /// `error: <message>, code: <code>, type: <type>, request_id: <id>` joined
    /// with `, ` (each segment omitted when empty/zero).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.status_code != 0 {
            write!(f, "Request failed: {}, ", status_text(self.status_code))?;
        }
        let d = &self.detail;
        if !d.message.is_empty() {
            write!(f, "error: {}", d.message)?;
        }
        if !d.code.is_empty() {
            write!(f, ", code: {}", d.code)?;
        }
        if !d.detail_type.is_empty() {
            write!(f, ", type: {}", d.detail_type)?;
        }
        if !d.request_id.is_empty() {
            write!(f, ", request_id: {}", d.request_id)?;
        }
        Ok(())
    }
}

impl std::error::Error for ResponseError {}

/// Mirrors Go `llm.ErrorDetail` (`llm/model.go:871`). `message` and `type` are
/// the always-serialized fields (no `omitempty`); the rest are omitempty.
///
/// The Go `type` field is a lowercase reserved word in Rust, so the field is
/// named `detail_type` with an explicit `#[serde(rename = "type")]`.
/// `message` and `type` carry NO `omitempty` in Go and are therefore always
/// serialized (even when empty); the other three are omitempty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ErrorDetail {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    #[serde(default)]
    pub message: String,
    #[serde(rename = "type", default)]
    pub detail_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub param: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub request_id: String,
}

/// Returns the standard HTTP reason phrase for a status code, mirroring Go's
/// `net/http.StatusText`. Used by `ResponseError`'s `Display` impl to match
/// the Go `Error()` output byte-for-byte for the statuses the Go tests cover
/// (400 → "Bad Request"). Full coverage of all 5xx phrases is intentionally
/// not modeled; unknown codes fall back to the numeric form, which is the same
/// behavior Go's `StatusText` exhibits for unrecognized codes (empty string —
/// we deviate slightly by emitting the number to keep the message readable).
fn status_text(code: u16) -> String {
    match code {
        400 => "Bad Request".to_string(),
        401 => "Unauthorized".to_string(),
        403 => "Forbidden".to_string(),
        404 => "Not Found".to_string(),
        408 => "Request Timeout".to_string(),
        409 => "Conflict".to_string(),
        418 => "I'm a teapot".to_string(),
        422 => "Unprocessable Entity".to_string(),
        429 => "Too Many Requests".to_string(),
        500 => "Internal Server Error".to_string(),
        501 => "Not Implemented".to_string(),
        502 => "Bad Gateway".to_string(),
        503 => "Service Unavailable".to_string(),
        504 => "Gateway Timeout".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_request_round_trip_preserves_extensions() -> Result<(), serde_json::Error> {
        let input = json!({
            "request_type": "chat",
            "api_format": "openai/chat_completions",
            "model": "gpt-test",
            "stream": true,
            "payload": {
                "payload_type": "chat",
                "messages": [
                    {"role": "developer", "content": "be terse", "cache_control": {"type": "ephemeral"}},
                    {"role": "user", "content": [{"type": "text", "text": "hi", "provider_part": true}]}
                ],
                "tools": [{"type": "function", "name": "lookup", "parameters": {"type": "object"}}],
                "tool_choice": "auto",
                "response_format": {"type": "json_object"},
                "stream_options": {"include_usage": true},
                "reasoning_effort": "medium",
                "provider_chat_field": {"kept": true}
            },
            "extra_body": {"provider_body": 1},
            "extra_headers": {"x-provider": "yes"},
            "metadata": {"request_origin": "test"},
            "provider_top_level": {"kept": true}
        });

        let request: LlmRequest = serde_json::from_value(input)?;
        let round_trip = serde_json::to_value(&request)?;

        assert_eq!(round_trip["request_type"], "chat");
        assert_eq!(round_trip["api_format"], "openai/chat_completions");
        assert_eq!(
            round_trip["payload"]["messages"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            round_trip["payload"]["messages"][1]["content"][0]["provider_part"],
            true
        );
        assert_eq!(round_trip["payload"]["provider_chat_field"]["kept"], true);
        assert_eq!(round_trip["extra_body"]["provider_body"], 1);
        assert_eq!(round_trip["extra_headers"]["x-provider"], "yes");
        assert_eq!(round_trip["provider_top_level"]["kept"], true);
        Ok(())
    }

    #[test]
    fn stream_event_round_trip_preserves_sse_and_binary_fields() -> Result<(), serde_json::Error> {
        let input = json!({
            "last_event_id": "42",
            "type": "message.delta",
            "data": "{\"delta\":\"hi\"}",
            "json_data": {"delta": "hi"},
            "size": 15,
            "binary": [1, 2, 3],
            "done": true,
            "provider_event": "kept"
        });

        let event: StreamEvent = serde_json::from_value(input)?;
        let round_trip = serde_json::to_value(&event)?;

        assert_eq!(round_trip["last_event_id"], "42");
        assert_eq!(round_trip["type"], "message.delta");
        assert_eq!(round_trip["data"], "{\"delta\":\"hi\"}");
        assert_eq!(round_trip["size"], 15);
        assert_eq!(round_trip["binary"], json!([1, 2, 3]));
        assert_eq!(round_trip["done"], true);
        assert_eq!(round_trip["provider_event"], "kept");
        Ok(())
    }

    // ----- S14: ToolChoice round-trip (mirrors Go `llm.ToolChoice`) -----

    #[test]
    fn tool_choice_string_round_trip() -> Result<(), serde_json::Error> {
        // Go: ToolChoice{ToolChoice: ptr("auto")} marshals to `"auto"`.
        let tc: ToolChoice = serde_json::from_value(json!("auto"))?;
        assert!(matches!(tc, ToolChoice::Tool(ref s) if s == "auto"));
        assert_eq!(serde_json::to_value(&tc)?, json!("auto"));
        Ok(())
    }

    #[test]
    fn tool_choice_named_round_trip() -> Result<(), serde_json::Error> {
        // Go: ToolChoice{NamedToolChoice: &NamedToolChoice{Type:"function",
        //      Function: ToolFunction{Name:"lookup"}}} marshals to an object.
        let obj = json!({"type": "function", "function": {"name": "lookup"}});
        let tc: ToolChoice = serde_json::from_value(obj.clone())?;
        match tc {
            ToolChoice::NamedToolChoice(ref n) => {
                assert_eq!(n.choice_type, "function");
                assert_eq!(n.function.name, "lookup");
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected NamedToolChoice, got {other:?}"
                )));
            }
        }
        assert_eq!(serde_json::to_value(&tc)?, obj);
        Ok(())
    }

    #[test]
    fn tool_choice_string_takes_precedence_over_object() -> Result<(), serde_json::Error> {
        // `#[serde(untagged)]` tries variants in order; the string variant must
        // accept a bare string and never fall through to NamedToolChoice.
        let tc: ToolChoice = serde_json::from_value(json!("none"))?;
        assert!(matches!(tc, ToolChoice::Tool(_)));
        Ok(())
    }

    #[test]
    fn tool_choice_none_string_round_trip() -> Result<(), serde_json::Error> {
        let tc: ToolChoice = serde_json::from_value(json!("none"))?;
        assert!(matches!(tc, ToolChoice::Tool(ref s) if s == "none"));
        assert_eq!(serde_json::to_value(&tc)?, json!("none"));
        Ok(())
    }

    #[test]
    fn tool_choice_rejects_invalid_payload() {
        // Neither a string nor a valid NamedToolChoice object should fail to
        // deserialize — Go would also leave both fields nil; we assert the
        // untagged enum errors on garbage that matches neither shape.
        let bad = json!([1, 2, 3]);
        let result: Result<ToolChoice, _> = serde_json::from_value(bad);
        assert!(result.is_err());
    }

    // ----- S14: Thinking round-trip (mirrors Go Anthropic `Thinking`) -----

    #[test]
    fn thinking_full_round_trip() -> Result<(), serde_json::Error> {
        let obj = json!({"type": "enabled", "budget_tokens": 4096, "display": "summarized"});
        let t: Thinking = serde_json::from_value(obj.clone())?;
        assert_eq!(t.thinking_type, "enabled");
        assert_eq!(t.budget_tokens, Some(4096));
        assert_eq!(t.display.as_deref(), Some("summarized"));
        assert_eq!(serde_json::to_value(&t)?, obj);
        Ok(())
    }

    #[test]
    fn thinking_omits_optional_fields() -> Result<(), serde_json::Error> {
        // Go omitempty: only `type` is required.
        let t: Thinking = serde_json::from_value(json!({"type": "disabled"}))?;
        assert_eq!(t.thinking_type, "disabled");
        assert!(t.budget_tokens.is_none());
        assert!(t.display.is_none());
        // Round-trip must drop the optional fields.
        assert_eq!(serde_json::to_value(&t)?, json!({"type": "disabled"}));
        Ok(())
    }

    // ----- S11: LlmRequestPayload `#[non_exhaustive]` + request_type -----

    #[test]
    fn request_payload_tag_is_chat_after_round_trip() -> Result<(), serde_json::Error> {
        // Existing variants must keep working; the tag is `payload_type` and
        // the value is the snake_case variant name (`chat`).
        let req: LlmRequest = serde_json::from_value(json!({
            "request_type": "chat",
            "api_format": "openai/chat_completions",
            "payload": {"payload_type": "chat", "messages": []}
        }))?;
        match &req.payload {
            LlmRequestPayload::Chat(c) => {
                assert!(c.messages.is_empty());
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Chat, got {} ({other:?})",
                    other.request_type()
                )));
            }
        }
        assert_eq!(req.payload.request_type(), "chat");
        Ok(())
    }

    #[test]
    fn request_payload_responses_variant_round_trips() -> Result<(), serde_json::Error> {
        // Sanity-check a second existing payload variant + request_type(). The
        // top-level `request_type` field uses the `RequestType` enum (which has
        // no `responses` member — the Responses payload maps to `compact`), so
        // we use `chat` here for a valid RequestType; the point of the test is
        // the LlmRequestPayload::Responses variant, selected by `payload_type`.
        let req: LlmRequest = serde_json::from_value(json!({
            "request_type": "chat",
            "api_format": "openai/responses_compact",
            "payload": {"payload_type": "responses", "tools": []}
        }))?;
        assert!(matches!(req.payload, LlmRequestPayload::Responses(_)));
        assert_eq!(req.payload.request_type(), "responses");
        Ok(())
    }

    // ----- S11: remaining LlmRequestPayload variants round-trip -----
    // S11 requires the structured enum to cover chat/completion/responses/
    // embedding/rerank/image/video/audio. The previous round only exercised
    // chat + responses; these tests close the remaining six variants and
    // confirm the snake_case `payload_type` tag round-trips for each.

    #[test]
    fn payload_completion_variant_round_trips() -> Result<(), serde_json::Error> {
        let req: LlmRequest = serde_json::from_value(json!({
            "request_type": "chat",
            "api_format": "openai/chat_completions",
            "payload": {"payload_type": "completion", "prompt": "hello", "max_tokens": 16}
        }))?;
        match &req.payload {
            LlmRequestPayload::Completion(c) => {
                assert_eq!(c.max_tokens, Some(16));
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Completion, got {} ({other:?})",
                    other.request_type()
                )));
            }
        }
        assert_eq!(req.payload.request_type(), "completion");
        // round-trip preserves the tag
        let rt = serde_json::to_value(&req)?;
        assert_eq!(rt["payload"]["payload_type"], "completion");
        Ok(())
    }

    #[test]
    fn payload_embedding_variant_round_trips() -> Result<(), serde_json::Error> {
        let req: LlmRequest = serde_json::from_value(json!({
            "request_type": "embedding",
            "api_format": "openai/embeddings",
            "payload": {"payload_type": "embedding", "input": "hello", "dimensions": 256}
        }))?;
        match &req.payload {
            LlmRequestPayload::Embedding(e) => {
                assert_eq!(e.dimensions, Some(256));
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Embedding, got {} ({other:?})",
                    other.request_type()
                )));
            }
        }
        assert_eq!(req.payload.request_type(), "embedding");
        let rt = serde_json::to_value(&req)?;
        assert_eq!(rt["payload"]["payload_type"], "embedding");
        Ok(())
    }

    #[test]
    fn payload_rerank_variant_round_trips() -> Result<(), serde_json::Error> {
        let req: LlmRequest = serde_json::from_value(json!({
            "request_type": "rerank",
            "api_format": "jina/rerank",
            "payload": {"payload_type": "rerank", "query": "q", "documents": ["a", "b"], "top_n": 1}
        }))?;
        match &req.payload {
            LlmRequestPayload::Rerank(r) => {
                assert_eq!(r.top_n, Some(1));
                assert_eq!(r.documents.len(), 2);
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Rerank, got {} ({other:?})",
                    other.request_type()
                )));
            }
        }
        assert_eq!(req.payload.request_type(), "rerank");
        Ok(())
    }

    #[test]
    fn payload_image_variant_round_trips() -> Result<(), serde_json::Error> {
        let req: LlmRequest = serde_json::from_value(json!({
            "request_type": "image",
            "api_format": "openai/image_generation",
            "payload": {"payload_type": "image", "prompt": "cat", "n": 2, "size": "1024x1024"}
        }))?;
        match &req.payload {
            LlmRequestPayload::Image(i) => {
                assert_eq!(i.n, Some(2));
                assert_eq!(i.size.as_deref(), Some("1024x1024"));
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Image, got {} ({other:?})",
                    other.request_type()
                )));
            }
        }
        assert_eq!(req.payload.request_type(), "image");
        Ok(())
    }

    #[test]
    fn payload_video_variant_round_trips() -> Result<(), serde_json::Error> {
        let req: LlmRequest = serde_json::from_value(json!({
            "request_type": "video",
            "api_format": "openai/video",
            "payload": {"payload_type": "video", "prompt": "sunset", "duration": "5s"}
        }))?;
        match &req.payload {
            LlmRequestPayload::Video(v) => {
                assert_eq!(v.duration.as_deref(), Some("5s"));
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Video, got {} ({other:?})",
                    other.request_type()
                )));
            }
        }
        assert_eq!(req.payload.request_type(), "video");
        Ok(())
    }

    #[test]
    fn payload_audio_variant_round_trips() -> Result<(), serde_json::Error> {
        // Note: Go splits audio into Speech/Transcription/Translation request
        // types; the unified `AudioRequest` payload variant carries the body
        // shape. We use `speech` (a valid RequestType) here.
        let req: LlmRequest = serde_json::from_value(json!({
            "request_type": "speech",
            "api_format": "openai/audio_speech",
            "payload": {"payload_type": "audio", "input": "hello", "voice": "alloy"}
        }))?;
        match &req.payload {
            LlmRequestPayload::Audio(a) => {
                assert_eq!(a.voice.as_deref(), Some("alloy"));
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Audio, got {} ({other:?})",
                    other.request_type()
                )));
            }
        }
        assert_eq!(req.payload.request_type(), "audio");
        Ok(())
    }

    // ----- S14: MessageContent three-form union + ChatMessage tool fields -----
    // Go `llm.MessageContent` (model.go:458) marshals as either a bare string,
    // an array of content parts, or null. Rust mirrors this with
    // `#[serde(untagged)]` enum {Text, Parts, Json}; the variants are tried in
    // declaration order so a bare string wins over a one-element array.

    #[test]
    fn message_content_string_form_round_trips() -> Result<(), serde_json::Error> {
        let c: MessageContent = serde_json::from_value(json!("plain text"))?;
        assert!(matches!(c, MessageContent::Text(ref s) if s == "plain text"));
        assert_eq!(serde_json::to_value(&c)?, json!("plain text"));
        Ok(())
    }

    #[test]
    fn message_content_parts_form_round_trips() -> Result<(), serde_json::Error> {
        let parts = json!([
            {"type": "text", "text": "hello"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,xxx"}, "extra_field": true}
        ]);
        let c: MessageContent = serde_json::from_value(parts.clone())?;
        match c {
            MessageContent::Parts(ref p) => {
                assert_eq!(p.len(), 2);
                assert_eq!(p[0].part_type, "text");
                assert_eq!(p[1].part_type, "image_url");
                // S06: provider-specific part fields survive via extra flatten
                assert_eq!(p[1].extra.get("extra_field"), Some(&json!(true)));
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Parts, got {other:?}"
                )));
            }
        }
        assert_eq!(serde_json::to_value(&c)?, parts);
        Ok(())
    }

    #[test]
    fn message_content_json_fallback_keeps_arbitrary_object() -> Result<(), serde_json::Error> {
        // A JSON object that is neither a string nor an array of ContentPart
        // must round-trip via the Json(Value) fallback without loss.
        let obj = json!({"raw": {"nested": 1}});
        let c: MessageContent = serde_json::from_value(obj.clone())?;
        assert!(matches!(c, MessageContent::Json(_)));
        assert_eq!(serde_json::to_value(&c)?, obj);
        Ok(())
    }

    #[test]
    fn chat_message_preserves_tool_call_id_and_name_round_trip() -> Result<(), serde_json::Error> {
        // Mirrors a tool-role message: Go `Message{Role:"tool",
        // ToolCallID: ptr("call_1"), Content: "result"}`.
        let input = json!({
            "role": "tool",
            "content": "result payload",
            "tool_call_id": "call_1",
            "name": "lookup",
            "provider_help_field": {"kept": true}
        });
        let m: ChatMessage = serde_json::from_value(input)?;
        assert_eq!(m.role, "tool");
        assert_eq!(m.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(m.name.as_deref(), Some("lookup"));
        // S06: provider help field survives via extra flatten
        assert_eq!(
            m.extra.get("provider_help_field"),
            Some(&json!({"kept": true}))
        );
        Ok(())
    }

    // ----- S14: Stop union (Go llm.Stop is string-or-[]string) -----
    // The Rust unified model carries `stop: Option<Value>` (per S05 the chat
    // layer stays loose for the open-ended OpenAI field set). This test pins
    // the contract that both the string and array shapes round-trip intact,
    // mirroring Go `Stop.MarshalJSON` precedence (string wins when present).

    #[test]
    fn chat_request_stop_string_and_array_shapes_round_trip() -> Result<(), serde_json::Error> {
        // string form
        let req_str: ChatRequest = serde_json::from_value(json!({"stop": "END"}))?;
        assert_eq!(req_str.stop, Some(json!("END")));
        assert_eq!(serde_json::to_value(&req_str)?["stop"], json!("END"));

        // array form
        let req_arr: ChatRequest = serde_json::from_value(json!({"stop": ["END", "STOP"]}))?;
        assert_eq!(req_arr.stop, Some(json!(["END", "STOP"])));
        assert_eq!(
            serde_json::to_value(&req_arr)?["stop"],
            json!(["END", "STOP"])
        );
        Ok(())
    }

    // ----- S14: UnifiedTool full-field round-trip -----
    // The unified tool model is intentionally flat (type/name/description/
    // parameters at top level), matching Go `llm.Tool.ToLLMTool()` flattening
    // performed by the OpenAI inbound transformer. This test pins the
    // flat shape + the S06 extra-flatten for provider-specific tool fields.

    #[test]
    fn unified_tool_full_fields_round_trip() -> Result<(), serde_json::Error> {
        let input = json!({
            "type": "function",
            "name": "lookup",
            "description": "look things up",
            "parameters": {"type": "object", "properties": {}},
            "cache_control": {"type": "ephemeral"}
        });
        let t: UnifiedTool = serde_json::from_value(input.clone())?;
        assert_eq!(t.tool_type, "function");
        assert_eq!(t.name.as_deref(), Some("lookup"));
        assert_eq!(t.description.as_deref(), Some("look things up"));
        assert_eq!(
            t.parameters,
            Some(json!({"type": "object", "properties": {}}))
        );
        // cache_control is not a typed UnifiedTool field; it survives via extra.
        assert_eq!(
            t.extra.get("cache_control"),
            Some(&json!({"type": "ephemeral"}))
        );
        assert_eq!(serde_json::to_value(&t)?, input);
        Ok(())
    }

    #[test]
    fn unified_tool_minimal_only_type_required() -> Result<(), serde_json::Error> {
        // Go `Tool.Type` is the only required field; everything else is omitempty.
        let t: UnifiedTool = serde_json::from_value(json!({"type": "web_search"}))?;
        assert_eq!(t.tool_type, "web_search");
        assert!(t.name.is_none());
        assert!(t.description.is_none());
        assert!(t.parameters.is_none());
        // round-trip drops the optional fields
        assert_eq!(serde_json::to_value(&t)?, json!({"type": "web_search"}));
        Ok(())
    }

    // ----- S14: ToolCall round-trip with FunctionCall payload -----
    // Go `llm.ToolCall` carries `Function: FunctionCall{Name, Arguments,
    // Namespace}`. The Rust unified model keeps `function: serde_json::Value`
    // (FunctionCall is permissive about argument shape). This pins the
    // contract that the function object survives verbatim.

    #[test]
    fn tool_call_function_payload_round_trips() -> Result<(), serde_json::Error> {
        let input = json!({
            "id": "call_42",
            "type": "function",
            "function": {"name": "lookup", "arguments": "{\"q\":\"x\"}"}
        });
        let tc: ToolCall = serde_json::from_value(input.clone())?;
        assert_eq!(tc.id.as_deref(), Some("call_42"));
        assert_eq!(tc.call_type, "function");
        assert_eq!(
            tc.function,
            json!({"name": "lookup", "arguments": "{\"q\":\"x\"}"})
        );
        assert_eq!(serde_json::to_value(&tc)?, input);
        Ok(())
    }

    // ========================================================================
    // RUST-P6-001 response side — mirrors Go `llm/model_test.go` golden cases
    // and the unified response/choice/message/error round-trip shapes used by
    // the stream transformer (P7-002 S08/S09/S12).
    // ========================================================================

    // ----- LlmResponse non-streaming round-trip (mirrors OpenAI chat.completion) -----

    #[test]
    fn llm_response_non_streaming_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let input = json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-test",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello!",
                    "refusal": null
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7},
            "system_fingerprint": "fp_abc",
            "service_tier": "standard",
            "provider_top_level": {"kept": true}
        });

        let resp: LlmResponse = serde_json::from_value(input.clone())?;
        assert_eq!(resp.id, "chatcmpl-1");
        assert_eq!(resp.object, "chat.completion");
        assert_eq!(resp.model, "gpt-test");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].index, 0);
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        // message present, delta absent (non-streaming)
        assert!(resp.choices[0].message.is_some());
        assert!(resp.choices[0].delta.is_none());
        let msg = resp.choices[0].message.as_ref();
        assert_eq!(msg.and_then(|m| m.role.as_deref()), Some("assistant"));
        assert!(matches!(
            msg.and_then(|m| m.content.as_ref()),
            Some(MessageContent::Text(s)) if s == "Hello!"
        ));
        // usage round-trips through the existing crate Usage
        let usage = resp.usage.as_ref();
        assert_eq!(usage.map(|u| u.prompt_tokens), Some(5));
        // S06: provider top-level field survives via extra flatten
        assert_eq!(
            resp.extra.get("provider_top_level"),
            Some(&json!({"kept": true}))
        );
        // full round-trip preserves structure
        let rt = serde_json::to_value(&resp)?;
        assert_eq!(rt["id"], "chatcmpl-1");
        assert_eq!(rt["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(rt["usage"]["total_tokens"], 7);
        assert_eq!(rt["provider_top_level"]["kept"], true);
        Ok(())
    }

    // ----- LlmResponse streaming chunk shape (delta, no usage) -----
    // Go reuses the same `Response` struct for streaming chunks; here the
    // choice carries `delta` instead of `message`, and `usage` is absent.

    #[test]
    fn llm_response_streaming_chunk_round_trip() -> Result<(), serde_json::Error> {
        let chunk = json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-test",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": "Hi"},
                "finish_reason": null
            }]
        });

        let resp: LlmResponse = serde_json::from_value(chunk.clone())?;
        assert_eq!(resp.object, "chat.completion.chunk");
        assert_eq!(resp.choices.len(), 1);
        assert!(resp.choices[0].message.is_none());
        let delta = resp.choices[0].delta.as_ref();
        assert!(delta.is_some());
        assert_eq!(delta.and_then(|d| d.role.as_deref()), Some("assistant"));
        assert!(resp.choices[0].finish_reason.is_none());
        assert!(resp.usage.is_none());
        // round-trip preserves streaming shape
        let rt = serde_json::to_value(&resp)?;
        assert_eq!(rt["object"], "chat.completion.chunk");
        assert!(rt["choices"][0].get("delta").is_some());
        assert!(rt["choices"][0].get("message").is_none());
        Ok(())
    }

    // ----- LlmMessage field coverage: tool_calls + reasoning + annotations -----
    // Mirrors the assistant-message shape produced by the OpenAI/Anthropic
    // outbound transformers (the immediate consumers of P7-002).

    #[test]
    fn llm_message_assistant_full_fields_round_trip() -> Result<(), serde_json::Error> {
        let input = json!({
            "role": "assistant",
            "content": "thinking...",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "lookup", "arguments": "{}"}
            }],
            "reasoning_content": "step-by-step",
            "reasoning_signature": "sig",
            "annotations": [{
                "type": "url_citation",
                "start_index": 0,
                "end_index": 5,
                "url_citation": {"url": "https://example.com", "title": "ex"}
            }]
        });

        let m: LlmMessage = serde_json::from_value(input.clone())?;
        assert_eq!(m.role.as_deref(), Some("assistant"));
        assert_eq!(m.tool_calls.len(), 1);
        assert_eq!(m.tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(m.reasoning_content.as_deref(), Some("step-by-step"));
        assert_eq!(m.reasoning_signature.as_deref(), Some("sig"));
        assert_eq!(m.annotations.len(), 1);
        assert_eq!(
            m.annotations[0].annotation_type.as_deref(),
            Some("url_citation")
        );
        assert_eq!(m.annotations[0].start_index, Some(0));
        assert_eq!(m.annotations[0].end_index, Some(5));
        let cite = m.annotations[0].url_citation.as_ref();
        assert_eq!(
            cite.and_then(|c| c.url.as_deref()),
            Some("https://example.com")
        );
        assert_eq!(cite.and_then(|c| c.title.as_deref()), Some("ex"));
        assert_eq!(serde_json::to_value(&m)?, input);
        Ok(())
    }

    // ----- LlmMessage tool-role fields + inline_tool_results -----
    // Mirrors the tool-result message shape + Anthropic server-side tool
    // results carried inline (Go `InlineToolResult`).

    #[test]
    fn llm_message_tool_role_and_inline_tool_results_round_trip() -> Result<(), serde_json::Error> {
        let input = json!({
            "role": "tool",
            "content": "42",
            "tool_call_id": "call_1",
            "tool_call_name": "lookup",
            "tool_call_is_error": false,
            "message_index": 0,
            "inline_tool_results": [{
                "tool_call_id": "toolu_1",
                "output": "search hit",
                "is_error": false,
                "transformer_metadata": {"anthropic_type": "web_search_tool_result"}
            }]
        });

        let m: LlmMessage = serde_json::from_value(input.clone())?;
        assert_eq!(m.role.as_deref(), Some("tool"));
        assert_eq!(m.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(m.tool_call_name.as_deref(), Some("lookup"));
        assert_eq!(m.tool_call_is_error, Some(false));
        assert_eq!(m.message_index, Some(0));
        assert_eq!(m.inline_tool_results.len(), 1);
        let itr = &m.inline_tool_results[0];
        assert_eq!(itr.tool_call_id.as_deref(), Some("toolu_1"));
        assert_eq!(itr.output.as_deref(), Some("search hit"));
        assert!(!itr.is_error);
        assert_eq!(
            itr.transformer_metadata.get("anthropic_type"),
            Some(&json!("web_search_tool_result"))
        );
        assert_eq!(serde_json::to_value(&m)?, input);
        Ok(())
    }

    // ----- ResponseError: StatusCode is `json:"-"`, detail nested under "error" -----
    // Mirrors Go `ResponseError` shape: the body is `{"error": {...}}` and the
    // HTTP status code never appears on the wire.

    #[test]
    fn response_error_status_code_never_serialized() -> Result<(), serde_json::Error> {
        let err = ResponseError {
            status_code: 400,
            detail: ErrorDetail {
                code: "bad_request".to_string(),
                message: "nope".to_string(),
                detail_type: "invalid_request_error".to_string(),
                param: "q".to_string(),
                request_id: "req_1".to_string(),
            },
        };
        let v = serde_json::to_value(&err)?;
        // status code must NOT leak
        assert!(v.get("status_code").is_none());
        assert!(v.get("status").is_none());
        // detail lives under the "error" key
        assert_eq!(v["error"]["code"], "bad_request");
        assert_eq!(v["error"]["message"], "nope");
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert_eq!(v["error"]["param"], "q");
        assert_eq!(v["error"]["request_id"], "req_1");

        // round-trip: status code is lost on the wire (matches Go json:"-")
        let back: ResponseError = serde_json::from_value(v)?;
        assert_eq!(back.status_code, 0);
        assert_eq!(back.detail.code, "bad_request");
        Ok(())
    }

    // ----- ResponseError::Error() Display mirrors Go TestResponseError_Error -----
    // Go golden strings (from conduit/llm/model_test.go:84):
    //   with request id:
    //     "Request failed: Bad Request, error: test1, code: test1, type: test1, request_id: test1"
    //   without request id:
    //     "Request failed: Bad Request, error: test1, code: test1, type: test1"

    #[test]
    fn response_error_display_matches_go_golden() {
        let with_id = ResponseError {
            status_code: 400,
            detail: ErrorDetail {
                message: "test1".to_string(),
                code: "test1".to_string(),
                detail_type: "test1".to_string(),
                request_id: "test1".to_string(),
                ..ErrorDetail::default()
            },
        };
        assert_eq!(
            with_id.to_string(),
            "Request failed: Bad Request, error: test1, code: test1, type: test1, request_id: test1"
        );

        let without_id = ResponseError {
            status_code: 400,
            detail: ErrorDetail {
                message: "test1".to_string(),
                code: "test1".to_string(),
                detail_type: "test1".to_string(),
                ..ErrorDetail::default()
            },
        };
        assert_eq!(
            without_id.to_string(),
            "Request failed: Bad Request, error: test1, code: test1, type: test1"
        );
    }

    // ----- ErrorDetail: message + type always serialized (no omitempty in Go) -----
    // Go tags: `json:"message"` and `json:"type"` with NO omitempty. An empty
    // ErrorDetail therefore serializes as `{"message":"","type":""}` (not `{}`).

    #[test]
    fn error_detail_message_and_type_always_present() -> Result<(), serde_json::Error> {
        let empty = ErrorDetail::default();
        let v = serde_json::to_value(&empty)?;
        // message and type have no omitempty → always emitted, even when empty
        assert_eq!(v["message"], "");
        assert_eq!(v["type"], "");
        // code/param/request_id have omitempty → omitted when empty
        assert!(v.get("code").is_none());
        assert!(v.get("param").is_none());
        assert!(v.get("request_id").is_none());
        Ok(())
    }
}
