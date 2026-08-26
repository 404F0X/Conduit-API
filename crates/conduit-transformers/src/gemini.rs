//! Gemini transformer — inbound + outbound stream chunk conversion.
//!
//! Implements:
//! - **S04** path-action parser (`parse_gemini_action`)
//! - **S06** stream-mode selector (`gemini_stream_mode`)
//! - **S11** contents / system-instruction parse → unified `LlmRequest`
//!   (`parse_gemini_contents_to_llm_request`) and outbound body builder
//!   (`build_gemini_outbound_body`).
//! - **S12** stream chunk → unified `LlmResponse` conversion
//!   (`parse_gemini_stream_event`, `gemini_chunk_to_llm_response`), handling both
//!   SSE (`alt=sse`) and JSON-array framing per the detected mode.
//!
//! Tools / safety / thinking are intentionally skipped — see `[Euclid-the-4th ?]`
//! TODOs in `TODO_SMALL.md`. Pure functions only: no I/O, no HTTP wiring.

use conduit_core::{ConduitError, ErrorKind};
use conduit_llm::{
    ApiFormat, ChatMessage, ChatRequest, Choice, ContentPart, HttpRequest, HttpResponse,
    LlmMessage, LlmRequest, LlmRequestPayload, LlmResponse, MessageContent, RequestType,
    StreamEvent, ToolCall, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{InboundTransformer, OutboundTransformer, TransformerResult};

/// Error surfaced by the path-action parser.
#[derive(Debug, thiserror::Error)]
pub enum GeminiActionError {
    #[error("invalid request URL: {0}")]
    InvalidRequestUrl(String),
}

/// Gemini path action — `/models/:model:(generateContent|streamGenerateContent)`.
///
/// Mirrors Go `extractRequestParams` in
/// `conduit/llm/transformer/gemini/inbound.go` lines 28-50: the parser splits the
/// URL on `/`, takes the last segment, splits that on `:`, and matches the
/// suffix against the two known actions. The model id is the prefix before `:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiAction {
    /// Model id extracted from the final path segment (before `:`).
    pub model: String,
    /// `true` for `streamGenerateContent`, `false` for `generateContent`.
    pub stream: bool,
}

/// Gemini stream delivery mode — derived from `GeminiAction.stream` plus the
/// `alt=sse` query hint.
///
/// Mirrors Gemini's documented behavior: when streaming, the default response
/// is a JSON array of chunk objects; `alt=sse` switches to Server-Sent Events.
/// Non-streaming requests always yield a single JSON object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiStreamMode {
    /// Non-streaming — single JSON object body.
    Json,
    /// Streaming — JSON array of chunk objects (default streaming mode).
    JsonArray,
    /// Streaming — Server-Sent Events (`alt=sse`).
    Sse,
}

/// Parse a Gemini path-action suffix into a [`GeminiAction`].
///
/// Pure: takes a path string, returns the parsed action. Mirrors Go's
/// `extractRequestParams` (conduit/llm/transformer/gemini/inbound.go:28-50).
///
/// # Examples
///
/// ```
/// # use conduit_transformers::gemini::{parse_gemini_action, GeminiAction, GeminiActionError};
/// let a = parse_gemini_action("/v1beta/models/gemini-2.5-flash:generateContent")?;
/// assert_eq!(a, GeminiAction { model: "gemini-2.5-flash".to_string(), stream: false });
/// # Ok::<(), GeminiActionError>(())
/// ```
pub fn parse_gemini_action(path: &str) -> Result<GeminiAction, GeminiActionError> {
    if path.is_empty() {
        return Err(GeminiActionError::InvalidRequestUrl(
            "invalid request path: ".to_string(),
        ));
    }

    let suffix = path.rsplit('/').next().unwrap_or("");
    if suffix.is_empty() {
        return Err(GeminiActionError::InvalidRequestUrl(format!(
            "invalid request path: {path}"
        )));
    }

    let Some((model, action)) = suffix.split_once(':') else {
        return Err(GeminiActionError::InvalidRequestUrl(format!(
            "invalid request path: {path}"
        )));
    };

    if model.is_empty() {
        return Err(GeminiActionError::InvalidRequestUrl(format!(
            "invalid request path: {path}"
        )));
    }

    match action {
        "generateContent" => Ok(GeminiAction {
            model: model.to_string(),
            stream: false,
        }),
        "streamGenerateContent" => Ok(GeminiAction {
            model: model.to_string(),
            stream: true,
        }),
        _ => Err(GeminiActionError::InvalidRequestUrl(format!(
            "invalid request path: {path}"
        ))),
    }
}

// ============================================================================
// S07 — Gemini embedding (embedContent / batchEmbedContents)
// ============================================================================
//
// Mirrors Go `conduit/llm/transformer/gemini/embedding.go`. The Go inbound
// transformer (`extractRequestParams`, inbound.go:28-50) does NOT route
// embedding actions — they reach the outbound transformer separately. The
// structs and pure helpers below are direct ports of the Go embedding types
// and the pure conversion logic (request shape, response shape, task-type
// mapping, input→texts extraction, URL construction).
//
// Wiring into the unified `LlmRequest`/`LlmResponse` is intentionally limited:
// the Rust `EmbeddingRequest` (conduit-llm) is OpenAI-shaped
// (`input: Option<Value>`, no `Task`/`EmbeddingInput` struct), and
// `LlmResponse.embedding` is `Option<Value>`. The converters therefore return
// provider-shaped structs that the caller can serialize into `LlmResponse`.
// Full unified-request ↔ Gemini-embedding bridging is deferred until the
// conduit-llm embedding types match the Go `EmbeddingInput{String,StringArray}`
// + `Task` shape (`pending source snapshot`).

/// Gemini `EmbedContentConfig` optional params (Go embedding.go:14-23).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiEmbedContentConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Reduced dimension for the output embedding (`outputDimensionality`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dimensionality: Option<i32>,
}

/// Gemini `ContentEmbedding` (Go embedding.go:26-31). `values` is `[]float32`
/// in Go; kept as `Vec<f32>` here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiContentEmbedding {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<f32>,
}

/// Gemini `EmbedContentRequest` (Go embedding.go:34-41).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiEmbedContentRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<GeminiContent>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dimensionality: Option<i32>,
}

/// Gemini `EmbedContentResponse` (Go embedding.go:44-47).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiEmbedContentResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<GeminiContentEmbedding>,
}

/// Gemini `BatchEmbedContentsRequest` (Go embedding.go:50-53).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiBatchEmbedContentsRequest {
    #[serde(default)]
    pub requests: Vec<GeminiEmbedContentRequest>,
}

/// Gemini `BatchEmbedContentsResponse` (Go embedding.go:56-59).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiBatchEmbedContentsResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embeddings: Vec<GeminiContentEmbedding>,
}

/// Whether the embedding action is batch (multiple inputs → batchEmbedContents)
/// or single (embedContent). Mirrors Go `transformEmbeddingRequest`
/// (embedding.go:60-160) branching on `len(texts) == 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiEmbeddingKind {
    /// `embedContent` — single text input.
    Single,
    /// `batchEmbedContents` — multiple text inputs.
    Batch,
}

impl GeminiEmbeddingKind {
    /// The Gemini path-action suffix.
    pub const fn action(self) -> &'static str {
        match self {
            Self::Single => "embedContent",
            Self::Batch => "batchEmbedContents",
        }
    }
}

/// Parse a Gemini embedding path-action suffix
/// (`/models/:model:(embedContent|batchEmbedContents)`) into a model + kind.
///
/// Pure. Mirrors the URL-action half of Go
/// `OutboundTransformer.transformEmbeddingRequest` /
/// `buildEmbeddingURL` (embedding.go:158-184): the suffix after `:` is matched
/// against the two embedding actions.
pub fn parse_gemini_embedding_action(
    path: &str,
) -> Result<(String, GeminiEmbeddingKind), GeminiActionError> {
    if path.is_empty() {
        return Err(GeminiActionError::InvalidRequestUrl(
            "invalid request path: ".to_string(),
        ));
    }

    let suffix = path.rsplit('/').next().unwrap_or("");
    if suffix.is_empty() {
        return Err(GeminiActionError::InvalidRequestUrl(format!(
            "invalid request path: {path}"
        )));
    }

    let Some((model, action)) = suffix.split_once(':') else {
        return Err(GeminiActionError::InvalidRequestUrl(format!(
            "invalid request path: {path}"
        )));
    };

    if model.is_empty() {
        return Err(GeminiActionError::InvalidRequestUrl(format!(
            "invalid request path: {path}"
        )));
    }

    let kind = match action {
        "embedContent" => GeminiEmbeddingKind::Single,
        "batchEmbedContents" => GeminiEmbeddingKind::Batch,
        _ => {
            return Err(GeminiActionError::InvalidRequestUrl(format!(
                "invalid request path: {path}"
            )));
        }
    };

    Ok((model.to_string(), kind))
}

/// Map a unified task-type string to the Gemini `TaskType` enum (uppercase),
/// returning `""` for unknown. Mirrors Go `mapEmbeddingTaskType`
/// (embedding.go:287-313).
pub fn map_gemini_embedding_task_type(task: &str) -> &'static str {
    match task.to_lowercase().as_str() {
        "retrieval.query" | "retrieval_query" => "RETRIEVAL_QUERY",
        "retrieval.passage" | "retrieval_document" => "RETRIEVAL_DOCUMENT",
        "semantic_similarity" | "text-matching" => "SEMANTIC_SIMILARITY",
        "classification" => "CLASSIFICATION",
        "clustering" => "CLUSTERING",
        "question_answering" => "QUESTION_ANSWERING",
        "fact_verification" => "FACT_VERIFICATION",
        "code_retrieval_query" => "CODE_RETRIEVAL_QUERY",
        _ => "",
    }
}

/// Embedding input — text input normalized from the OpenAI-shaped unified
/// `input: Option<Value>` (string or array of strings). Token-array inputs
/// (`[int]`, [[int]]) are unsupported by the Gemini embedding API and yield no
/// texts. Mirrors Go `embeddingInputToTexts` (embedding.go:265-279) +
/// `llm.EmbeddingInput{String,StringArray}` semantics.
pub fn gemini_embedding_input_to_texts(input: Option<&Value>) -> Vec<String> {
    let Some(value) = input else {
        return Vec::new();
    };

    if let Some(s) = value.as_str() {
        if s.is_empty() {
            return Vec::new();
        }
        return vec![s.to_string()];
    }

    if let Some(arr) = value.as_array() {
        let mut texts = Vec::new();
        for item in arr {
            if let Some(s) = item.as_str() {
                texts.push(s.to_string());
            }
            // Non-string array elements (int arrays) are unsupported → skip.
        }
        return texts;
    }

    Vec::new()
}

/// Convert `Vec<f32>` → `Vec<f64>`. Mirrors Go `float32sToFloat64s`
/// (embedding.go:315-324).
pub fn gemini_f32_to_f64(values: &[f32]) -> Vec<f64> {
    values.iter().map(|v| *v as f64).collect()
}

/// One unified embedding entry (post-conversion). Mirrors Go `llm.EmbeddingData`
/// (`object:"embedding"`, index, embedding float vector).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUnifiedEmbeddingData {
    pub object: String,
    pub embedding: Vec<f64>,
    pub index: i64,
}

/// Unified embedding response payload (post-conversion). Mirrors Go
/// `llm.EmbeddingResponse` (`object:"list"`, `data`). Serialized into
/// `LlmResponse.embedding: Option<Value>` by the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUnifiedEmbeddingResponse {
    pub object: String,
    pub data: Vec<GeminiUnifiedEmbeddingData>,
}

/// Convert a single `GeminiEmbedContentResponse` to the unified embedding
/// response shape. Mirrors Go `convertSingleEmbeddingResponse`
/// (embedding.go:243-260).
pub fn convert_single_gemini_embedding_response(
    resp: &GeminiEmbedContentResponse,
) -> GeminiUnifiedEmbeddingResponse {
    let data = match &resp.embedding {
        Some(emb) => vec![GeminiUnifiedEmbeddingData {
            object: "embedding".to_string(),
            embedding: gemini_f32_to_f64(&emb.values),
            index: 0,
        }],
        None => Vec::new(),
    };

    GeminiUnifiedEmbeddingResponse {
        object: "list".to_string(),
        data,
    }
}

/// Convert a `GeminiBatchEmbedContentsResponse` to the unified embedding
/// response shape. Mirrors Go `convertBatchEmbeddingResponse`
/// (embedding.go:263-283).
pub fn convert_batch_gemini_embedding_response(
    resp: &GeminiBatchEmbedContentsResponse,
) -> GeminiUnifiedEmbeddingResponse {
    let data = resp
        .embeddings
        .iter()
        .enumerate()
        .map(|(i, emb)| GeminiUnifiedEmbeddingData {
            object: "embedding".to_string(),
            embedding: gemini_f32_to_f64(&emb.values),
            index: i as i64,
        })
        .collect();

    GeminiUnifiedEmbeddingResponse {
        object: "list".to_string(),
        data,
    }
}

/// Build a single `GeminiEmbedContentRequest` from one text + model ref +
/// task-type + output-dim. Mirrors the per-text request construction in Go
/// `transformEmbeddingRequest` (embedding.go:99-106, 124-138).
pub fn build_gemini_embed_content_request(
    model_ref: &str,
    text: &str,
    task_type: &str,
    output_dimensionality: Option<i32>,
) -> GeminiEmbedContentRequest {
    GeminiEmbedContentRequest {
        model: model_ref.to_string(),
        content: Some(GeminiContent {
            parts: vec![GeminiPart {
                text: text.to_string(),
                inline_data: None,
                extra: BTreeMap::new(),
            }],
            role: String::new(),
        }),
        task_type: task_type.to_string(),
        title: String::new(),
        output_dimensionality,
    }
}

/// Build the Gemini embedding API URL from base URL + version + model + kind.
/// Mirrors Go `buildEmbeddingURL` (embedding.go:158-184) for the non-Vertex
/// (standard Generative Language API) path. Vertex AI URL construction is
/// deferred (`pending source snapshot`) since it requires the
/// `PlatformType`/`Config` surface not present in this pure module.
pub fn build_gemini_embedding_url(
    base_url: &str,
    api_version: &str,
    model: &str,
    kind: GeminiEmbeddingKind,
) -> String {
    let version = if api_version.is_empty() {
        // Go `DefaultAPIVersion` — embedding.go:170.
        "v1beta"
    } else {
        api_version
    };
    format!(
        "{}/{}/models/{}:{}",
        base_url.trim_end_matches('/'),
        version,
        model,
        kind.action(),
    )
}

/// Determine whether the given input set should use single (`embedContent`) or
/// batch (`batchEmbedContents`). Mirrors Go
/// `OutboundTransformer.transformEmbeddingRequest` branching on
/// `len(texts) == 1` (embedding.go:94-149).
pub fn gemini_embedding_kind_for_texts(texts: &[String]) -> GeminiEmbeddingKind {
    if texts.len() == 1 {
        GeminiEmbeddingKind::Single
    } else {
        GeminiEmbeddingKind::Batch
    }
}

/// Determine the stream delivery mode from the parsed action and the `alt` query
/// hint.
///
/// Pure: takes the action + a pre-extracted `alt` value (caller parses the
/// query string). `alt == "sse"` switches streaming responses to SSE; otherwise
/// streaming responses default to a JSON array.
pub fn gemini_stream_mode(action: &GeminiAction, alt: Option<&str>) -> GeminiStreamMode {
    if !action.stream {
        return GeminiStreamMode::Json;
    }
    match alt {
        Some("sse") => GeminiStreamMode::Sse,
        _ => GeminiStreamMode::JsonArray,
    }
}

// ---------------------------------------------------------------------------
// S11 — Gemini contents → unified LlmRequest
// ---------------------------------------------------------------------------

/// Gemini `Blob` — inline media bytes (raw or base64). Only `mime_type` and
/// `data` are needed for the minimal inbound path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiBlob {
    #[serde(default, rename = "mimeType", skip_serializing_if = "String::is_empty")]
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
}

/// Minimal subset of Gemini `Part` needed for S11.
///
/// Skips `fileData`, `functionCall`, `functionResponse`, `thought`,
/// `thoughtSignature` — these are tool/thinking surfaces deferred as
/// `[Euclid-the-4th ?]`. Unknown fields are captured in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeminiPart {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(
        default,
        rename = "inlineData",
        skip_serializing_if = "Option::is_none"
    )]
    pub inline_data: Option<GeminiBlob>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Gemini `Content` — `parts` + `role`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiContent {
    #[serde(default)]
    pub parts: Vec<GeminiPart>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
}

/// Minimal Gemini `GenerateContentRequest` — `contents` + `systemInstruction`.
///
/// Tools / safety / generationConfig / toolConfig / cachedContent are deferred
/// as `[Euclid-the-4th ?]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerateContentRequest {
    #[serde(default)]
    pub contents: Vec<GeminiContent>,
    #[serde(
        default,
        rename = "systemInstruction",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_instruction: Option<GeminiContent>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Convert a Gemini role to the unified role, mirroring Go's
/// `convertGeminiRoleToLLMRole` (conduit/llm/transformer/gemini/convert.go:44-55):
/// `model` → `assistant`, `""`/`user` → `user`, otherwise unchanged.
pub fn gemini_role_to_llm_role(role: &str) -> &str {
    match role {
        "model" => "assistant",
        "" | "user" => "user",
        other => other,
    }
}

/// Extract concatenated text from a Gemini `Content`, skipping `thought` parts.
/// Mirrors Go `extractTextFromContent` (conduit/llm/transformer/gemini/convert.go:12-26).
/// Since `thought` is a deferred field, all parts with non-empty `text` count.
pub fn extract_text_from_content(content: Option<&GeminiContent>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    let mut texts: Vec<&str> = Vec::new();
    for part in &content.parts {
        if !part.text.is_empty() {
            texts.push(part.text.as_str());
        }
    }
    texts.join("\n")
}

/// Build a single text-only `ContentPart` list from a Gemini `Content`,
/// converting each `text` part and each `inlineData` part (as a base64 data URL
/// in an `image_url` content part — matches Go's image branch in
/// `convertGeminiContentToLLMMessage`, inbound_convert.go:322-357).
///
/// For the minimal S11 scope we treat every `inlineData` blob as an
/// `image_url` part (Go's default branch for non-document/video/audio MIME
/// types); MIME-aware routing is deferred as `[Euclid-the-4th ?]`.
fn build_content_parts(content: &GeminiContent) -> Vec<ContentPart> {
    let mut parts = Vec::new();
    for p in &content.parts {
        if !p.text.is_empty() {
            parts.push(ContentPart {
                part_type: "text".to_string(),
                text: Some(p.text.clone()),
                image_url: None,
                input_audio: None,
                extra: BTreeMap::new(),
            });
        }
        if let Some(blob) = &p.inline_data {
            let data_url = build_data_url(&blob.mime_type, &blob.data);
            let mut url_obj = serde_json::Map::new();
            url_obj.insert("url".to_string(), Value::String(data_url));
            parts.push(ContentPart {
                part_type: "image_url".to_string(),
                text: None,
                image_url: Some(Value::Object(url_obj)),
                input_audio: None,
                extra: BTreeMap::new(),
            });
        }
    }
    parts
}

/// Build a `data:<mime>;base64,<data>` URL. Mirrors the inline-data branch of
/// Go's `convertGeminiContentToLLMMessage` which calls `xurl.BuildDataURL(...,
/// true)` (base64-encoded).
fn build_data_url(mime_type: &str, data: &str) -> String {
    format!("data:{mime_type};base64,{data}")
}

/// Collapse a `ContentPart` list into a [`MessageContent`] following the same
/// rules as Go (inbound_convert.go:434-442): a single text part becomes
/// `MessageContent::Text`; otherwise the parts list is preserved.
fn collapse_parts(mut parts: Vec<ContentPart>) -> Option<MessageContent> {
    if parts.is_empty() {
        return None;
    }
    if parts.len() == 1 && parts[0].part_type == "text" {
        let text = parts.remove(0).text.unwrap_or_default();
        Some(MessageContent::Text(text))
    } else {
        Some(MessageContent::Parts(parts))
    }
}

/// Convert a parsed [`GeminiGenerateContentRequest`] + [`GeminiAction`] into the
/// unified [`LlmRequest`]. Mirrors Go's `convertGeminiToLLMRequest` (the S11
/// slice only — tools/safety/thinking/generationConfig deferred as
/// `[Euclid-the-4th ?]`).
///
/// Behavior parity with Go:
/// - `api_format = GeminiContents`, `request_type = Chat`.
/// - `systemInstruction` (if its concatenated text is non-empty) becomes a
///   leading `system` message.
/// - Each `contents[*]` becomes a [`ChatMessage`] with the role mapped via
///   [`gemini_role_to_llm_role`]; empty-parts contents are dropped.
/// - `stream` and `model` are taken from the action.
pub fn parse_gemini_contents_to_llm_request(
    gemini_req: &GeminiGenerateContentRequest,
    action: &GeminiAction,
) -> TransformerResult<LlmRequest> {
    if gemini_req.contents.is_empty() {
        return Err(conduit_core::ConduitError::invalid_request(
            "contents are required",
        ));
    }

    let mut messages: Vec<ChatMessage> = Vec::new();

    // System instruction → leading system message.
    let system_text = extract_text_from_content(gemini_req.system_instruction.as_ref());
    if !system_text.is_empty() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            name: None,
            content: Some(MessageContent::Text(system_text)),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: BTreeMap::new(),
        });
    }

    // Contents → user/assistant messages.
    for content in &gemini_req.contents {
        if content.parts.is_empty() {
            continue;
        }
        let parts = build_content_parts(content);
        let Some(message_content) = collapse_parts(parts) else {
            continue;
        };
        messages.push(ChatMessage {
            role: gemini_role_to_llm_role(&content.role).to_string(),
            name: None,
            content: Some(message_content),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: BTreeMap::new(),
        });
    }

    // S08 tools + toolConfig — mirrors Go `convertGeminiToLLMRequest`
    // (inbound_convert.go:146-235): `tools[*].functionDeclarations` → unified
    // `UnifiedTool` (type="function"); `tools[*].googleSearch` / `codeExecution`
    // / `urlContext` → Google native tools; `toolConfig.functionCallingConfig`
    // → unified `tool_choice`. These are parsed from the request `extra` map
    // (the `GeminiGenerateContentRequest` struct flattens unknown fields there
    // rather than modeling every Gemini-only sub-struct).
    let tools = parse_gemini_tools(&gemini_req.extra);
    let tool_choice = parse_gemini_tool_config(&gemini_req.extra);

    let chat = ChatRequest {
        messages,
        tools,
        tool_choice,
        ..Default::default()
    };

    Ok(LlmRequest {
        request_type: RequestType::Chat,
        api_format: ApiFormat::GeminiContents,
        model: Some(action.model.clone()),
        stream: action.stream,
        payload: LlmRequestPayload::Chat(chat),
        extra_body: BTreeMap::new(),
        extra_headers: BTreeMap::new(),
        metadata: BTreeMap::new(),
        extra: BTreeMap::new(),
    })
}

// ---------------------------------------------------------------------------
// S08 inbound tools / toolConfig — Gemini native tools → unified
// ---------------------------------------------------------------------------
//
// Mirrors Go `convertGeminiToLLMRequest` tools branch
// (`conduit/llm/transformer/gemini/inbound_convert.go` lines 146-235) and the
// toolConfig branch (`convertGeminiFunctionCallingConfigToToolChoice`,
// `convert.go:303-335`).
//
// The `GeminiGenerateContentRequest` struct above flattens fields it does not
// explicitly model (`tools`, `toolConfig`, `safetySettings`, …) into `extra`.
// The helpers below read from that map so we do not have to widen the struct
// for every Gemini-only sub-shape. JSON tag parity (all-caps acronym gotcha)
// is verified against the Go struct tags in `model.go`:
//   - `tools` (plain lowercase — `model.go:14`)
//   - `toolConfig` (camelCase — `model.go:20`)
//   - `functionDeclarations` (camelCase — `model.go:113`)
//   - `parametersJsonSchema` (camelCase — `model.go:193`)
//   - `googleSearch` / `codeExecution` / `urlContext` (camelCase)
//   - `functionCallingConfig` / `allowedFunctionNames` (camelCase)

/// Mirrors Go `llm.ContainsGoogleNativeTools` / `IsGoogleNativeTool`
/// (`conduit/llm/tools.go:191-203`): a "Google native tool" is one whose `type`
/// starts with the `google_` prefix. These are only meaningful on the native
/// Gemini API surface — see [`gemini_supports_native_tools`].
pub fn is_google_native_tool(tool_type: &str) -> bool {
    tool_type.starts_with("google_")
}

/// Whether a given Conduit API channel type supports Gemini native tools
/// (`google_search` / `google_code_execution` / `google_url_context`).
///
/// Mirrors the Go gating in `conduit/llm/transformer/gemini/openai/outbound.go`
/// lines 309-325: native Gemini tools are *only* honored on the `gemini` and
/// `gemini_vertex` channel types. On `gemini_openai` (OpenAI-compatible
/// endpoint) they are filtered out with a warning, because that surface has no
/// `googleSearch` / `codeExecution` / `urlContext` request fields.
///
/// `channel_type` is matched case-insensitively against the Go channel-type
/// constants (`gemini`, `gemini_vertex`).
pub fn gemini_supports_native_tools(channel_type: &str) -> bool {
    let lower = channel_type.to_ascii_lowercase();
    lower == "gemini" || lower == "gemini_vertex"
}

/// Parse Gemini `tools` array from the inbound request's `extra` map into
/// unified [`conduit_llm::UnifiedTool`]s. Mirrors Go inbound_convert.go:146-227.
///
/// Each Gemini `Tool` may carry one or more of:
/// - `functionDeclarations[]` → one `UnifiedTool` (type=`"function"`) per
///   declaration, preserving whichever parameters format the client sent
///   (`parameters` legacy, or `parametersJsonSchema` new) — see Go lines 152-193.
/// - `googleSearch`    → `UnifiedTool { type = "google_search" }`
/// - `codeExecution`   → `UnifiedTool { type = "google_code_execution" }`
/// - `urlContext`      → `UnifiedTool { type = "google_url_context" }`
///
/// Returns an empty `Vec` when no tools are present (Go: `chatReq.Tools` stays
/// nil → empty slice). Unknown tool shapes are skipped (forward-compatible).
pub fn parse_gemini_tools(extra: &BTreeMap<String, Value>) -> Vec<conduit_llm::UnifiedTool> {
    use conduit_llm::constants::tool_type;

    let Some(tools) = extra.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut out: Vec<conduit_llm::UnifiedTool> = Vec::new();
    for tool in tools {
        // Function declarations — Go inbound_convert.go:152-193.
        if let Some(fds) = tool.get("functionDeclarations").and_then(Value::as_array) {
            for fd in fds {
                let name = fd.get("name").and_then(Value::as_str).map(str::to_string);
                let description = fd
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string);

                // Preserve whichever parameters format was used (mutually
                // exclusive in Go). `parameters` is the legacy OpenAPI 3.03
                // shape; `parametersJsonSchema` is the newer JSON-Schema shape.
                let parameters = fd.get("parameters").filter(|v| !v.is_null()).cloned();
                let parameters = parameters.or_else(|| {
                    fd.get("parametersJsonSchema")
                        .filter(|v| !v.is_null())
                        .cloned()
                });

                let mut extra_fields: BTreeMap<String, Value> = BTreeMap::new();
                // Stash the originating format under a private key so the
                // outbound path can rebuild the right field. Go tracks this
                // implicitly by leaving the unused RawMessage nil; in Rust we
                // record it because `UnifiedTool.parameters` collapses both.
                if let Some(pjs) = fd.get("parametersJsonSchema").filter(|v| !v.is_null())
                    && parameters.as_ref() == Some(pjs)
                {
                    extra_fields.insert("__gemini_parameters_json_schema".to_string(), true.into());
                }

                out.push(conduit_llm::UnifiedTool {
                    tool_type: tool_type::FUNCTION.to_string(),
                    name,
                    description,
                    parameters,
                    extra: extra_fields,
                });
            }
        }

        // Google native tools — Go inbound_convert.go:195-227.
        if tool.get("googleSearch").is_some() {
            out.push(conduit_llm::UnifiedTool {
                tool_type: tool_type::GOOGLE_SEARCH.to_string(),
                name: None,
                description: None,
                parameters: None,
                extra: BTreeMap::new(),
            });
        }
        if tool.get("codeExecution").is_some() {
            out.push(conduit_llm::UnifiedTool {
                tool_type: tool_type::GOOGLE_CODE_EXECUTION.to_string(),
                name: None,
                description: None,
                parameters: None,
                extra: BTreeMap::new(),
            });
        }
        if tool.get("urlContext").is_some() {
            out.push(conduit_llm::UnifiedTool {
                tool_type: tool_type::GOOGLE_URL_CONTEXT.to_string(),
                name: None,
                description: None,
                parameters: None,
                extra: BTreeMap::new(),
            });
        }
    }
    out
}

/// Parse Gemini `toolConfig.functionCallingConfig` from the inbound request's
/// `extra` map into a unified `tool_choice` JSON value. Mirrors Go
/// `convertGeminiFunctionCallingConfigToToolChoice` (`convert.go:303-335`):
///
/// | Gemini `mode` | `AllowedFunctionNames` | unified `tool_choice`         |
/// |---------------|------------------------|-------------------------------|
/// | `AUTO`        | (any)                  | `"auto"`                      |
/// | `NONE`        | (any)                  | `"none"`                      |
/// | `ANY`         | exactly one name       | `{"type":"function","function":{"name":..}}` |
/// | `ANY`         | zero or >1 names       | `"required"`                  |
/// | (other/empty) | (any)                  | `"auto"`                      |
///
/// Returns `None` when there is no `toolConfig` / `functionCallingConfig`
/// (Go: `chatReq.ToolChoice` stays nil).
pub fn parse_gemini_tool_config(extra: &BTreeMap<String, Value>) -> Option<Value> {
    let fcc = extra.get("toolConfig")?.get("functionCallingConfig")?;

    let mode = fcc.get("mode").and_then(Value::as_str).unwrap_or("");
    let allowed: Vec<&str> = fcc
        .get("allowedFunctionNames")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let choice = match mode {
        "AUTO" => Value::String("auto".to_string()),
        "NONE" => Value::String("none".to_string()),
        "ANY" => {
            if allowed.len() == 1 {
                serde_json::json!({
                    "type": "function",
                    "function": { "name": allowed[0] }
                })
            } else {
                Value::String("required".to_string())
            }
        }
        _ => Value::String("auto".to_string()),
    };
    Some(choice)
}

// ---------------------------------------------------------------------------
// S11 outbound — unified LlmRequest → Gemini generateContent request body
// ---------------------------------------------------------------------------
//
// Mirrors Go `convertLLMToGeminiRequestWithConfig`
// (conduit/llm/transformer/gemini/outbound_convert.go:24-311) for the outbound
// body only (no URL/header/auth wiring — that is the transformer-registry's
// job). Returns a `serde_json::Value` matching the wire shape of the Gemini
// `GenerateContentRequest` struct (model.go:9-30):
//
// ```json
// {
//   "contents": [{"role": "user", "parts": [{"text": "..."}]}],
//   "systemInstruction": {"parts": [{"text": "..."}]},
//   "generationConfig": {"maxOutputTokens": ..., "temperature": ..., ...},
//   "tools": [{"functionDeclarations": [{"name": "...", ...}]}],
//   "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}}
// }
// ```
//
// Scope notes (deferred as `[Euclid-the-4th ?]` / `[Galileo-the-2nd ?]`):
// - **Thinking signatures** (`shared.DecodeGeminiThoughtSignature`,
//   `ContextEngineeringThoughtSignature`, `setOutboundToolCallThoughtSignature`)
//   are intentionally skipped — they require the shared thought-signature codec
//   which has not been ported yet. The `thoughtSignature` field is omitted from
//   generated parts; this matches Go behavior for the common case where the
//   unified message carries no `reasoning_signature`.
// - **Tool result grouping** (Go `isPreviousContentToolResponse`) is implemented
//   (consecutive tool messages merge into one user-role Content), matching Go.
// - **ImageConfig / SafetySettings from TransformerMetadata** are deferred —
//   they require the transformer-metadata extraction surface and the Go
//   `*SafetySetting` / `*ImageConfig` typed values that live on the unified
//   `TransformerMetadata` map (not modeled at the Rust unified layer yet).
// - **Vertex `clearFunctionIDsForVertexAI`** is platform-routing, not body
//   shaping, and lives on the transformer instance, not this pure builder.
// - **ExtraBody `google.thinkingConfig` parse** (geminioai.ParseExtraBody) and
//   **reasoning effort → thinking budget/level mapping** are deferred — they
//   require the gemini/openai ExtraBody model port.

/// Convert a unified [`ChatMessage`] role to a Gemini role, mirroring Go's
/// `convertLLMRoleToGeminiRole` (conduit/llm/transformer/gemini/convert.go:57-70):
/// `assistant` → `model`, `developer` → `user`, otherwise unchanged.
pub fn llm_role_to_gemini_role(role: &str) -> &str {
    match role {
        "assistant" => "model",
        "developer" => "user",
        other => other,
    }
}

/// Build a single Gemini `Part` JSON object from a text string.
fn gemini_text_part(text: &str) -> Value {
    serde_json::json!({ "text": text })
}

/// Build Gemini `Part`s from a [`ChatMessage`] content + reasoning. Mirrors Go
/// `convertLLMMessageToGeminiContent` (outbound_convert.go:314-462) for the
/// non-tool-call, non-thought-signature path.
///
/// Order matches Go: reasoning content (as a `thought` part) first, then the
/// message body (bare string → one text part; multipart array → one part per
/// `text`/`image_url` entry).
fn build_gemini_parts_from_message(msg: &ChatMessage) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();

    // Reasoning content (thinking) — emitted as a `thought: true` text part
    // before the body, mirroring Go (outbound_convert.go:331-340).
    // NOTE: Rust unified `ChatMessage` carries reasoning via `extra` (the
    // Anthropic-specific `reasoning_content` field survives there). Read it
    // best-effort; if absent we skip the thought part entirely.
    if let Some(reasoning) = msg
        .extra
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        parts.push(serde_json::json!({ "text": reasoning, "thought": true }));
    }

    match msg.content.as_ref() {
        Some(MessageContent::Text(t)) if !t.is_empty() => {
            parts.push(gemini_text_part(t));
        }
        Some(MessageContent::Parts(content_parts)) => {
            for cp in content_parts {
                match cp.part_type.as_str() {
                    "text" => {
                        if let Some(t) = cp.text.as_ref().filter(|s| !s.is_empty()) {
                            parts.push(gemini_text_part(t));
                        }
                    }
                    "image_url" => {
                        if let Some(url) = cp
                            .image_url
                            .as_ref()
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                        {
                            if let Some(part) = convert_image_url_to_gemini_part(url) {
                                parts.push(part);
                            }
                        }
                    }
                    // video_url / document / input_audio are deferred — the
                    // Rust unified ContentPart does not model these typed
                    // sub-fields yet. They survive via `extra` but we don't
                    // synthesize Gemini parts from unknown shapes here.
                    _ => {}
                }
            }
        }
        // MessageContent::Json carries raw provider JSON; we don't emit a text
        // part for it (Go would also not extract text from a non-string /
        // non-MultipleContent body).
        _ => {}
    }

    parts
}

/// Convert an `image_url` string to a Gemini `Part`. Mirrors Go
/// `convertImageURLToGeminiPart` (convert.go:139-157):
/// - `data:<mime>;base64,<data>` → `{ "inlineData": { "mimeType": ..., "data": ... } }`
/// - any other URL → `{ "fileData": { "fileUri": <url> } }`
fn convert_image_url_to_gemini_part(url: &str) -> Option<Value> {
    if let Some(rest) = url.strip_prefix("data:") {
        // Format: data:<mime>;base64,<data>
        if let Some((meta, data)) = rest.split_once(',') {
            let mime_type = meta
                .split(';')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("");
            let mut inline = serde_json::Map::new();
            if !mime_type.is_empty() {
                inline.insert("mimeType".to_string(), Value::String(mime_type.to_string()));
            }
            inline.insert("data".to_string(), Value::String(data.to_string()));
            return Some(serde_json::json!({ "inlineData": Value::Object(inline) }));
        }
    }
    Some(serde_json::json!({ "fileData": { "fileUri": url } }))
}

/// Build a tool-result (`functionResponse`) Gemini `Content` from a unified
/// `tool`-role [`ChatMessage`]. Mirrors Go `convertLLMToolResultToGeminiContent`
/// (outbound_convert.go:465-498): role is always `"user"`; the message content
/// (a string) is parsed as JSON if possible, otherwise wrapped as
/// `{"result": <string>}`; `id` = `tool_call_id`, `name` = `tool_call_name`
/// (from `extra`).
fn build_gemini_tool_response_content(msg: &ChatMessage) -> Value {
    let body_text = match msg.content.as_ref() {
        Some(MessageContent::Text(t)) => t.clone(),
        _ => String::new(),
    };

    // Try to parse the content as a JSON object; fall back to {result: <str>}.
    let response_value: Value = if body_text.is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str::<Value>(&body_text) {
            Ok(v) if v.is_object() => v,
            _ => serde_json::json!({ "result": body_text }),
        }
    };

    let id = msg.tool_call_id.clone().unwrap_or_default();
    let name = msg
        .extra
        .get("tool_call_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut function_response = serde_json::Map::new();
    if !id.is_empty() {
        function_response.insert("id".to_string(), Value::String(id));
    }
    if !name.is_empty() {
        function_response.insert("name".to_string(), Value::String(name));
    }
    function_response.insert("response".to_string(), response_value);

    serde_json::json!({
        "role": "user",
        "parts": [{ "functionResponse": Value::Object(function_response) }]
    })
}

/// Returns `true` if the last entry of `contents` is a user-role Content whose
/// first part is a `functionResponse` — i.e. the previous entry is a tool
/// response that consecutive tool messages should merge into. Mirrors Go
/// `isPreviousContentToolResponse` (outbound_convert.go:514-525).
fn previous_content_is_tool_response(contents: &[Value]) -> bool {
    let Some(last) = contents.last() else {
        return false;
    };
    let role = last.get("role").and_then(|v| v.as_str()).unwrap_or("");
    if role != "user" {
        return false;
    }
    let Some(parts) = last.get("parts").and_then(|v| v.as_array()) else {
        return false;
    };
    if parts.is_empty() {
        return false;
    }
    parts[0].get("functionResponse").is_some()
}

/// Build the Gemini `generationConfig` object from a [`ChatRequest`]. Mirrors
/// Go `convertLLMToGeminiRequestWithConfig` generation-config branch
/// (outbound_convert.go:28-72). Returns `None` if no generation-config field is
/// set (Go leaves `req.GenerationConfig` nil in that case, and the field is
/// `omitempty` on the wire).
fn build_gemini_generation_config(chat: &ChatRequest) -> Option<Value> {
    let mut gc = serde_json::Map::new();
    let mut has = false;

    // maxOutputTokens — Go prefers MaxTokens, falls back to MaxCompletionTokens.
    if let Some(max) = chat.max_tokens {
        gc.insert("maxOutputTokens".to_string(), Value::from(i64::from(max)));
        has = true;
    }

    if let Some(temp) = chat.temperature {
        gc.insert("temperature".to_string(), serde_json::json!(temp));
        has = true;
    }

    if let Some(top_p) = chat.top_p {
        gc.insert("topP".to_string(), serde_json::json!(top_p));
        has = true;
    }

    // presencePenalty / frequencyPenalty / seed live on `extra` in the Rust
    // unified ChatRequest (the Go struct has them at top level; Rust keeps the
    // chat layer minimal and permissive). Read best-effort.
    for (field, gemini_key) in [
        ("presence_penalty", "presencePenalty"),
        ("frequency_penalty", "frequencyPenalty"),
    ] {
        if let Some(v) = chat.extra.get(field) {
            gc.insert(gemini_key.to_string(), v.clone());
            has = true;
        }
    }
    if let Some(seed) = chat.extra.get("seed") {
        gc.insert("seed".to_string(), seed.clone());
        has = true;
    }

    // stopSequences — Go reads `chatReq.Stop.Stop` (single) or
    // `chatReq.Stop.MultipleStop` (array). Rust unified keeps `stop: Option<Value>`
    // (string or array), so we normalize both shapes to a JSON array.
    if let Some(stop) = chat.stop.as_ref() {
        let seqs: Vec<Value> = match stop {
            Value::String(s) => vec![Value::String(s.clone())],
            Value::Array(arr) => arr.clone(),
            _ => Vec::new(),
        };
        if !seqs.is_empty() {
            gc.insert("stopSequences".to_string(), Value::Array(seqs));
            has = true;
        }
    }

    if has { Some(Value::Object(gc)) } else { None }
}

/// Build the Gemini `tools` array from unified [`conduit_llm::UnifiedTool`]s.
/// Mirrors Go's tools branch (outbound_convert.go:237-289):
/// - `function` → `FunctionDeclaration` (prefers `parametersJsonSchema` from
///   `extra`, falls back to `parameters`); all function declarations share a
///   single `Tool` entry that is appended once (before any non-function tool,
///   matching Go's "functionTool created on first function" ordering).
/// - `google_search` / `web_search` → `{ "googleSearch": {} }`
/// - `google_code_execution` → `{ "codeExecution": {} }`
/// - `google_url_context` → `{ "urlContext": {} }`
///
/// Returns `None` when there are no tools (Go leaves `req.Tools` nil → field is
/// `omitempty` on the wire).
fn build_gemini_tools(tools: &[conduit_llm::UnifiedTool]) -> Option<Vec<Value>> {
    if tools.is_empty() {
        return None;
    }

    let mut out: Vec<Value> = Vec::new();
    let mut function_declarations: Vec<Value> = Vec::new();

    for tool in tools {
        match tool.tool_type.as_str() {
            "function" => {
                let mut fd = serde_json::Map::new();
                // Go reads tool.Function.Name / Description.
                // Rust UnifiedTool carries name/description as Option<String>.
                if let Some(name) = tool.name.as_ref().filter(|s| !s.is_empty()) {
                    fd.insert("name".to_string(), Value::String(name.clone()));
                }
                if let Some(desc) = tool.description.as_ref().filter(|s| !s.is_empty()) {
                    fd.insert("description".to_string(), Value::String(desc.clone()));
                }
                // Go: prefer ParametersJsonSchema, else Parameters.
                let params = tool
                    .extra
                    .get("parametersJsonSchema")
                    .filter(|v| !v.is_null())
                    .or(tool.parameters.as_ref().filter(|v| !v.is_null()));
                if let Some(params) = params {
                    fd.insert("parametersJsonSchema".to_string(), params.clone());
                }
                function_declarations.push(Value::Object(fd));
            }
            "google_search" | "web_search" => {
                // Go skips google_search if `tool.Google.Search` is nil, but
                // accepts generic `web_search` unconditionally. The Rust
                // UnifiedTool does not model `Google.Search` presence, so we
                // mirror the simpler web_search path (always emit) for both.
                out.push(serde_json::json!({ "googleSearch": {} }));
            }
            "google_code_execution" => {
                out.push(serde_json::json!({ "codeExecution": {} }));
            }
            "google_url_context" => {
                out.push(serde_json::json!({ "urlContext": {} }));
            }
            _ => {}
        }
    }

    // Function declarations share one Tool entry, inserted at the position of
    // the first function tool encountered (Go appends `functionTool` once when
    // first seen, then later writes functionDeclarations onto it). Since the
    // non-function tools come before or after depending on input order, we
    // replicate Go ordering by inserting the function-tool entry at the front
    // if any functions were collected (Go's test "mixed tools" expects
    // function declarations in Tools[0]).
    if !function_declarations.is_empty() {
        out.insert(
            0,
            serde_json::json!({ "functionDeclarations": function_declarations }),
        );
    }

    if out.is_empty() { None } else { Some(out) }
}

/// Build the Gemini `toolConfig` from a unified `tool_choice`. Mirrors Go
/// `convertLLMToolChoiceToGeminiToolConfig` (convert.go:333-363).
///
/// The Rust unified layer keeps `tool_choice` as `Option<Value>` (string or
/// object); we mirror Go's two-branch logic by inspecting the JSON shape:
/// - string `"auto"` / `"none"` / `"required"` → mode `AUTO` / `NONE` / `ANY`
/// - object `{"type":"function","function":{"name":"..."}}` → mode `ANY` with
///   `allowedFunctionNames: [<name>]`
fn build_gemini_tool_config(tool_choice: &Value) -> Option<Value> {
    let mut fcc = serde_json::Map::new();
    match tool_choice {
        Value::String(s) => {
            let mode = match s.as_str() {
                "auto" => "AUTO",
                "none" => "NONE",
                "required" => "ANY",
                _ => "AUTO",
            };
            fcc.insert("mode".to_string(), Value::String(mode.to_string()));
        }
        Value::Object(obj) => {
            // Named tool choice — Go sets mode=ANY + allowedFunctionNames.
            fcc.insert("mode".to_string(), Value::String("ANY".to_string()));
            if let Some(name) = obj
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                fcc.insert(
                    "allowedFunctionNames".to_string(),
                    Value::Array(vec![Value::String(name.to_string())]),
                );
            }
        }
        _ => {
            fcc.insert("mode".to_string(), Value::String("AUTO".to_string()));
        }
    }
    Some(serde_json::json!({
        "functionCallingConfig": Value::Object(fcc)
    }))
}

/// Build a Gemini `generateContent` request body (`serde_json::Value`) from a
/// unified [`LlmRequest`]. This is the **pure body-shaping** half of the Gemini
/// outbound transformer; URL/header/auth wiring lives elsewhere.
///
/// # Errors
///
/// Returns [`conduit_core::ConduitError::invalid_request`] when the request is not
/// a chat payload, has no model, or has no messages — mirroring Go
/// `OutboundTransformer.TransformRequest` (outbound.go:104-134) validation.
pub fn build_gemini_outbound_body(llm_request: &LlmRequest) -> TransformerResult<Value> {
    let chat = match &llm_request.payload {
        LlmRequestPayload::Chat(c) => c,
        _ => {
            return Err(conduit_core::ConduitError::invalid_request(
                "gemini outbound only supports chat payloads",
            ));
        }
    };

    if llm_request.model.as_deref().unwrap_or_default().is_empty() {
        return Err(conduit_core::ConduitError::invalid_request(
            "model is required",
        ));
    }
    if chat.messages.is_empty() {
        return Err(conduit_core::ConduitError::invalid_request(
            "messages are required",
        ));
    }

    let mut body = serde_json::Map::new();

    // generationConfig
    if let Some(gc) = build_gemini_generation_config(chat) {
        body.insert("generationConfig".to_string(), gc);
    }

    // contents + systemInstruction
    let mut contents: Vec<Value> = Vec::new();
    let mut system_parts: Vec<Value> = Vec::new();

    for msg in &chat.messages {
        match msg.role.as_str() {
            "system" | "developer" => {
                // Collect into systemInstruction (Go accumulates parts across
                // multiple system/developer messages — outbound_convert.go:200-213).
                for part in build_gemini_parts_from_message(msg) {
                    system_parts.push(part);
                }
            }
            "tool" => {
                let tool_content = build_gemini_tool_response_content(msg);
                if previous_content_is_tool_response(&contents) {
                    // Merge into the previous tool-response Content.
                    let last = contents
                        .last_mut()
                        .and_then(|c| c.get_mut("parts"))
                        .and_then(|p| p.as_array_mut());
                    if let Some(last_parts) = last {
                        if let Some(new_parts) =
                            tool_content.get("parts").and_then(|p| p.as_array())
                        {
                            last_parts.extend(new_parts.iter().cloned());
                        }
                    }
                } else {
                    contents.push(tool_content);
                }
            }
            _ => {
                let parts = build_gemini_parts_from_message(msg);
                if parts.is_empty() {
                    // Go: convertLLMMessageToGeminiContent returns nil → skipped.
                    continue;
                }
                contents.push(serde_json::json!({
                    "role": llm_role_to_gemini_role(&msg.role),
                    "parts": parts,
                }));
            }
        }
    }

    if !system_parts.is_empty() {
        body.insert(
            "systemInstruction".to_string(),
            serde_json::json!({ "parts": system_parts }),
        );
    }
    body.insert("contents".to_string(), Value::Array(contents));

    // tools
    if let Some(tools) = build_gemini_tools(&chat.tools) {
        body.insert("tools".to_string(), Value::Array(tools));
    }

    // toolConfig
    if let Some(tc) = chat.tool_choice.as_ref().and_then(build_gemini_tool_config) {
        body.insert("toolConfig".to_string(), tc);
    }

    Ok(Value::Object(body))
}

// ---------------------------------------------------------------------------
// S12 — Gemini stream chunk → unified LlmResponse
// ---------------------------------------------------------------------------
//
// Mirrors Go `OutboundTransformer.TransformStreamChunk` /
// `transformStreamChunkWithState` (conduit/llm/transformer/gemini/outbound_stream.go:39-77)
// plus the per-chunk conversion `convertGeminiToLLMResponseWithState` and
// `convertGeminiCandidateToLLMChoiceWithState`
// (conduit/llm/transformer/gemini/outbound_convert.go:619-795).
//
// Streaming framing:
// - **SSE** (`alt=sse`): each event is one `data: <json>\n\n` frame; the chunk
//   JSON is a single `GenerateContentResponse` object. `[DONE]` is handled for
//   consistency (Gemini itself does not emit it, but Conduit API appends it to mark
//   end-of-stream — Go `streams.AppendStream(stream, lo.ToPtr(llm.DoneStreamEvent))`).
// - **JSON array** (default streaming): the body is a JSON array of chunk
//   objects. Conduit API's stream decoder splits it into one `StreamEvent` per array
//   element, so each event's `data` already holds a single chunk object.
//
// Both framings converge on the same per-chunk conversion once the JSON object
// is extracted, so the parser just normalizes the `StreamEvent.data`/`json_data`
// into a `serde_json::Value` and hands it to `gemini_chunk_to_llm_response`.

/// Gemini `FunctionCall` part of a `Part`. Mirrors Go `FunctionCall`
/// (conduit/llm/transformer/gemini/model.go:84-93). `id` and `name` carry
/// `omitempty`; `args` is a free-form JSON object (`omitempty`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiFunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// Gemini `UsageMetadata`. Mirrors Go `UsageMetadata`
/// (conduit/llm/transformer/gemini/model.go:445-468). All fields `omitempty`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUsageMetadata {
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub prompt_token_count: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub candidates_token_count: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub total_token_count: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub cached_content_token_count: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub thoughts_token_count: i64,
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// Gemini stream chunk `Candidate`. Mirrors Go `Candidate`
/// (conduit/llm/transformer/gemini/model.go:335-362). Only the fields the S12
/// stream conversion reads are modeled; the rest (safetyRatings, grounding, ...)
/// survive via `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiCandidate {
    #[serde(default)]
    pub index: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<GeminiContent>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub finish_reason: String,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Gemini `GenerateContentResponse`. Mirrors Go `GenerateContentResponse`
/// (conduit/llm/transformer/gemini/model.go:317-332). Only the fields the S12
/// stream conversion reads are modeled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerateContentResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<GeminiCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<GeminiUsageMetadata>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_version: String,
    #[serde(
        default,
        rename = "responseId",
        skip_serializing_if = "String::is_empty"
    )]
    pub response_id: String,
}

/// The `[DONE]` sentinel — appended by Conduit API to mark end-of-stream.
pub const GEMINI_DONE_MARKER: &str = "[DONE]";

/// Convert a Gemini finish reason to the unified OpenAI-shaped reason, mirroring
/// Go `convertGeminiFinishReasonToLLM` (convert.go:94-118).
///
/// `STOP` → `stop` (or `tool_calls` when `has_tool_call` is true), `MAX_TOKENS`
/// → `length`, `SAFETY`/`RECITATION` → `content_filter`, any other non-empty
/// value → `stop`. Returns `None` when `reason` is empty (matches Go returning
/// `nil`).
pub fn gemini_finish_reason_to_llm(reason: &str, has_tool_call: bool) -> Option<String> {
    if reason.is_empty() {
        return None;
    }
    let mapped = match reason {
        "STOP" => {
            if has_tool_call {
                "tool_calls"
            } else {
                "stop"
            }
        }
        "MAX_TOKENS" => "length",
        "SAFETY" | "RECITATION" => "content_filter",
        _ => "stop",
    };
    Some(mapped.to_string())
}

/// Convert Gemini `UsageMetadata` to the unified [`Usage`], mirroring Go
/// `convertToLLMUsage` (convert.go:426-470) for the subset the S12 model carries
/// (no modality token details — those survive via `extra`).
pub fn gemini_usage_to_llm(gemini_usage: Option<&GeminiUsageMetadata>) -> Option<Usage> {
    let g = gemini_usage?;
    let mut usage = Usage {
        prompt_tokens: g.prompt_token_count as u64,
        completion_tokens: (g.candidates_token_count + g.thoughts_token_count) as u64,
        total_tokens: g.total_token_count as u64,
        ..Usage::default()
    };
    if g.cached_content_token_count > 0 {
        usage.prompt_details.cached_tokens = g.cached_content_token_count as u64;
    }
    if g.thoughts_token_count > 0 {
        usage.completion_details.reasoning_tokens = g.thoughts_token_count as u64;
    }
    Some(usage)
}

/// A pseudo-unique fallback id when `responseId` is absent. Go uses
/// `"chatcmpl-" + uuid.New().String()`; the `uuid` crate is not a workspace
/// dependency, so we derive a 16-hex-digit suffix from `SystemTime` nanos. This
/// matches Go's intent (a unique-per-call opaque id) closely enough for the
/// non-streaming/aggregated path; the streaming path validates `responseId !=
/// ""` upstream (Go outbound_stream.go:68-70) and never reaches this fallback.
fn fallback_chatcmpl_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("chatcmpl-{nanos:016x}")
}

/// Convert a parsed [`GeminiGenerateContentResponse`] chunk into a unified
/// [`LlmResponse`], mirroring Go `convertGeminiToLLMResponseWithState`
/// (outbound_convert.go:619-665) plus `convertGeminiCandidateToLLMChoiceWithState`
/// (outbound_convert.go:669-795).
///
/// `is_stream` selects `chat.completion.chunk` + `delta` (true) vs
/// `chat.completion` + `message` (false), matching Go. `tool_call_index_offset`
/// carries the running tool-call index across chunks (Go `streamState.toolCallIndex`);
/// the returned `next_tool_call_index` lets the caller thread it through.
///
/// Scope (deferred as `[Euclid-the-5th ?]`): the Go candidate conversion also
/// handles `inlineData` parts (image/document), `thoughtSignature` /
/// `ReasoningSignature`, and `GroundingMetadata`/`CitationMetadata` annotations.
/// These require ports not yet present (shared thought-signature codec, typed
/// grounding metadata). The pure text / function-call / reasoning-text path —
/// which is what the Go `*_test.go` golden cases exercise — is fully covered.
pub fn gemini_chunk_to_llm_response(
    gemini_resp: &GeminiGenerateContentResponse,
    is_stream: bool,
    tool_call_index_offset: i64,
) -> (LlmResponse, i64) {
    let id = if gemini_resp.response_id.is_empty() {
        fallback_chatcmpl_id()
    } else {
        gemini_resp.response_id.clone()
    };

    // `LlmResponse` is `#[non_exhaustive]` in conduit-llm, so it cannot be
    // built with a struct literal from outside the crate — construct via
    // `default()` then assign fields (Go parity is unchanged).
    let mut resp = LlmResponse::default();
    resp.id = id;
    resp.object = if is_stream {
        "chat.completion.chunk".to_string()
    } else {
        "chat.completion".to_string()
    };
    resp.created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    resp.model = gemini_resp.model_version.clone();
    resp.request_type = Some(RequestType::Chat);
    resp.api_format = Some(ApiFormat::GeminiContents);

    let mut choices: Vec<Choice> = Vec::with_capacity(gemini_resp.candidates.len());
    let mut next_tool_call_index = tool_call_index_offset;

    for candidate in &gemini_resp.candidates {
        let (choice, next_idx) =
            gemini_candidate_to_llm_choice(candidate, is_stream, next_tool_call_index);
        next_tool_call_index = next_idx;
        choices.push(choice);
    }

    resp.choices = choices;
    resp.usage = gemini_usage_to_llm(gemini_resp.usage_metadata.as_ref());

    (resp, next_tool_call_index)
}

/// Convert one Gemini `Candidate` into a unified [`Choice`], mirroring Go
/// `convertGeminiCandidateToLLMChoiceWithState` (outbound_convert.go:669-795).
///
/// Returns `(choice, next_tool_call_index)`. Tool-call indices accumulate across
/// parts within this candidate AND across candidates/chunks (the caller threads
/// `next_tool_call_index` forward), matching Go `nextToolCallIndex`.
fn gemini_candidate_to_llm_choice(
    candidate: &GeminiCandidate,
    is_stream: bool,
    tool_call_index_offset: i64,
) -> (Choice, i64) {
    let mut choice = Choice {
        index: candidate.index,
        ..Choice::default()
    };
    let mut has_tool_call = false;
    let mut next_tool_call_index = tool_call_index_offset;

    if let Some(content) = candidate.content.as_ref() {
        let mut msg = LlmMessage {
            role: Some("assistant".to_string()),
            ..LlmMessage::default()
        };

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut reasoning_content = String::new();

        for part in &content.parts {
            // `thought` parts (reasoning) — Go: part.Text != "" && part.Thought.
            let is_thought = part
                .extra
                .get("thought")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            // Text part (non-empty `text`).
            if !part.text.is_empty() {
                if is_thought {
                    reasoning_content = part.text.clone();
                } else {
                    text_parts.push(part.text.clone());
                }
            }

            // Function-call part — read from `extra["functionCall"]`.
            if let Some(fc_value) = part.extra.get("functionCall") {
                let fc: GeminiFunctionCall = match serde_json::from_value(fc_value.clone()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let args_json = fc
                    .args
                    .as_ref()
                    .map(|a| serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string()))
                    .unwrap_or_default();
                let tc_id = fc.id.clone().unwrap_or_default();
                let tc_id = if tc_id.is_empty() {
                    // Go: fmt.Sprintf("tc_%s", uuid.NewString()); we use a
                    // running counter-derived id to stay dep-free.
                    format!("tc_{next_tool_call_index}")
                } else {
                    tc_id
                };
                tool_calls.push(ToolCall {
                    id: Some(tc_id),
                    call_type: "function".to_string(),
                    function: serde_json::json!({
                        "name": fc.name.clone().unwrap_or_default(),
                        "arguments": args_json,
                    }),
                    extra: BTreeMap::new(),
                });
                has_tool_call = true;
                next_tool_call_index += 1;
            }
        }

        // Set content — Go joins text parts with "" (concatenation).
        if !text_parts.is_empty() {
            let all_text = text_parts.concat();
            msg.content = Some(MessageContent::Text(all_text));
        }

        if !tool_calls.is_empty() {
            msg.tool_calls = tool_calls;
        }

        if !reasoning_content.is_empty() {
            msg.reasoning_content = Some(reasoning_content);
        }

        // Delta vs message.
        if is_stream {
            choice.delta = Some(msg);
        } else {
            choice.message = Some(msg);
        }
    }

    choice.finish_reason = gemini_finish_reason_to_llm(&candidate.finish_reason, has_tool_call);

    (choice, next_tool_call_index)
}

/// Parse a single Gemini stream [`StreamEvent`] into a unified [`LlmResponse`].
///
/// Mirrors Go `transformStreamChunkWithState` (outbound_stream.go:47-77):
/// - `None` / empty `data` → `Ok(None)` (Go returns `nil, nil`).
/// - `[DONE]` marker → a `LlmResponse` carrying only `object = "[DONE]"`
///   (Go `llm.DoneResponse`), signaling end-of-stream to downstream consumers.
/// - Otherwise: parse `data` as a `GenerateContentResponse`, error if
///   `responseId == ""` (Go `ErrInvalidResponse`), then convert with
///   [`gemini_chunk_to_llm_response`] in streaming mode.
///
/// The `tool_call_index_offset` threads the running tool-call index across
/// chunks (Go `streamState.toolCallIndex`); the returned `next_tool_call_index`
/// lets the caller advance it for the next chunk.
pub fn parse_gemini_stream_event(
    event: Option<&StreamEvent>,
    tool_call_index_offset: i64,
) -> TransformerResult<(Option<LlmResponse>, i64)> {
    let Some(event) = event else {
        return Ok((None, tool_call_index_offset));
    };

    // Prefer structured `json_data` (already-decoded by the HTTP layer); fall
    // back to the raw `data` string for SSE / JSON-array framing.
    let raw: Value = if let Some(json_data) = event.json_data.as_ref() {
        json_data.clone()
    } else if let Some(data) = event.data.as_ref() {
        match serde_json::from_str::<Value>(data) {
            Ok(v) => v,
            Err(e) => return Err(conduit_core::ConduitError::invalid_request(e.to_string())),
        }
    } else {
        return Ok((None, tool_call_index_offset));
    };

    // [DONE] marker — Go returns llm.DoneResponse (object == "[DONE]").
    // `LlmResponse` is `#[non_exhaustive]`, so build via `default()` + assign.
    if raw == Value::String(GEMINI_DONE_MARKER.to_string())
        || event.data.as_deref() == Some(GEMINI_DONE_MARKER)
    {
        let mut done = LlmResponse::default();
        done.object = GEMINI_DONE_MARKER.to_string();
        return Ok((Some(done), tool_call_index_offset));
    }

    let gemini_resp: GeminiGenerateContentResponse = serde_json::from_value(raw)
        .map_err(|e| conduit_core::ConduitError::invalid_request(e.to_string()))?;

    // Go: empty ResponseID → ErrInvalidResponse (Gemini emits empty events
    // transiently; surfacing an error is intentional).
    if gemini_resp.response_id.is_empty() {
        return Err(conduit_core::ConduitError::invalid_request(
            "invalid gemini response: empty responseId",
        ));
    }

    let (llm_resp, next_idx) =
        gemini_chunk_to_llm_response(&gemini_resp, true, tool_call_index_offset);
    Ok((Some(llm_resp), next_idx))
}

// ============================================================================
// S09 + S13 — Vertex platform type, credentials interface, outbound
// auth/header/base_url resolution (Go `outbound.go`).
// ============================================================================

/// Default Gemini API base URL. Mirrors Go `DefaultBaseURL`
/// (`conduit/llm/transformer/gemini/outbound.go:24`).
pub const GEMINI_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Default Gemini API version. Mirrors Go `DefaultAPIVersion`
/// (`conduit/llm/transformer/gemini/outbound.go:27`).
pub const GEMINI_DEFAULT_API_VERSION: &str = "v1beta";

/// Vertex platform identifier. Mirrors Go `PlatformVertex`
/// (`conduit/llm/transformer/gemini/outbound.go:30`).
pub const GEMINI_PLATFORM_VERTEX: &str = "vertex";

/// Gemini platform type — distinguishes Vertex AI from direct Generative
/// Language API.
///
/// Mirrors the Go `Config.PlatformType` switch in
/// `conduit/llm/transformer/gemini/outbound.go` (see `buildFullRequestURL`
/// lines 195-213 and `TransformRequest` lines 105-160): when
/// `platform_type == "vertex"` the URL is built as
/// `{base}/v1/publishers/google/models/{model}:{action}` (or without the
/// `/v1` prefix if the base URL already contains `/v1/`) and auth is expected
/// to be an OAuth bearer token supplied externally; otherwise the URL is
/// `{base}/{version}/models/{model}:{action}` and auth uses the
/// `x-goog-api-key` header with the API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiPlatformType {
    /// Direct Generative Language API — API key via `x-goog-api-key` header.
    Direct,
    /// Vertex AI — OAuth bearer token, `/v1/publishers/google/models/...` path.
    Vertex,
}

impl GeminiPlatformType {
    /// Build from the Go `Config.PlatformType` string. Empty / unknown values
    /// resolve to [`GeminiPlatformType::Direct`] (Go only special-cases the
    /// literal `"vertex"`).
    pub fn from_platform_type_str(s: &str) -> Self {
        if s == GEMINI_PLATFORM_VERTEX {
            Self::Vertex
        } else {
            Self::Direct
        }
    }
}

/// Resolved Gemini outbound request — the URL + the auth scheme to apply.
///
/// Pure value: no HTTP I/O. The caller (the outbound transformer wiring, not
/// this module) is responsible for attaching headers / tokens to the actual
/// HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiOutboundRequest {
    /// Fully-qualified outbound URL (mirrors Go `buildFullRequestURL`).
    pub url: String,
    /// Authentication scheme to apply.
    pub auth: GeminiAuth,
}

/// Authentication scheme resolved for a Gemini outbound request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiAuth {
    /// Direct Generative Language API — `x-goog-api-key: {api_key}` header.
    /// Mirrors Go `httpclient.AuthConfig { Type: "api_key", HeaderKey:
    /// "x-goog-api-key", APIKey }` (`outbound.go:140-145`).
    ApiKey {
        /// The API key value (may be empty when the channel has none — Go
        /// leaves `authConfig` nil in that case; we still surface the scheme
        /// so the caller can decide whether to error).
        api_key: String,
    },
    /// Vertex AI — caller must attach an OAuth bearer token. Go does not set
    /// an `httpclient.AuthConfig` in the transformer itself for Vertex
    /// (Vertex auth is injected by the platform/credentials layer upstream);
    /// we surface `None` to mean "transformer does not attach a key header".
    /// This keeps the helper pure: it neither fetches nor stores a token.
    VertexOAuth,
}

/// Normalized Gemini outbound configuration. Mirrors Go `Config`
/// (`outbound.go:33-50`) after running through [`cleanup_gemini_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiOutboundConfig {
    /// Base URL with no trailing version suffix. Empty after cleanup means the
    /// caller did not supply one — use [`GEMINI_DEFAULT_BASE_URL`].
    pub base_url: String,
    /// API version (`v1beta` / `v1`). Empty after cleanup means "fall back to
    /// raw-request path value or [`GEMINI_DEFAULT_API_VERSION`]".
    pub api_version: String,
    /// Optional custom endpoint path override (Go `EndpointPath`). When set
    /// (must start with `/`), URL construction short-circuits.
    pub endpoint_path: String,
    /// Platform type (Vertex vs Direct).
    pub platform_type: GeminiPlatformType,
}

impl Default for GeminiOutboundConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_version: String::new(),
            endpoint_path: String::new(),
            platform_type: GeminiPlatformType::Direct,
        }
    }
}

/// Normalize a raw Gemini outbound config exactly like Go `clenupConfig`
/// (`conduit/llm/transformer/gemini/outbound.go:67-90`).
///
/// Rules (Go line-by-line):
/// - If `base_url` is empty → `DefaultBaseURL` (trailing `/` stripped).
/// - If `api_version` is empty → `DefaultAPIVersion` (`v1beta`); then, if the
///   base URL ends with `/v1beta` or `/v1`, strip that suffix and pin the
///   version accordingly.
/// - If `api_version` is non-empty → strip `/{api_version}` from the end of
///   the base URL if present (note: Go leaves a lone trailing-slash base URL
///   untouched in this branch; we mirror that).
///
/// Note: Go does *not* strip a trailing `/` in general (see the
/// `"config with trailing slash in base URL"` golden case which keeps the
/// slash) — only the version-suffix stripping happens. We mirror that exactly.
pub fn cleanup_gemini_config(
    mut base_url: String,
    mut api_version: String,
) -> GeminiOutboundConfig {
    if base_url.is_empty() {
        // Go: `strings.TrimSuffix(DefaultBaseURL, "/")` — DefaultBaseURL has no
        // trailing slash, so this is a no-op, but mirror it for clarity.
        base_url = GEMINI_DEFAULT_BASE_URL.trim_end_matches('/').to_string();
    }

    if api_version.is_empty() {
        api_version = GEMINI_DEFAULT_API_VERSION.to_string();
        if let Some(stripped) = base_url.strip_suffix("/v1beta") {
            api_version = "v1beta".to_string();
            base_url = stripped.to_string();
        }
        if let Some(stripped) = base_url.strip_suffix("/v1") {
            api_version = "v1".to_string();
            base_url = stripped.to_string();
        }
    } else {
        let suffix = format!("/{api_version}");
        if let Some(stripped) = base_url.strip_suffix(&suffix) {
            base_url = stripped.to_string();
        }
    }

    GeminiOutboundConfig {
        base_url,
        api_version,
        endpoint_path: String::new(),
        platform_type: GeminiPlatformType::Direct,
    }
}

/// Build the action suffix for a Gemini content request.
///
/// Streaming → `streamGenerateContent?alt=sse`; non-streaming →
/// `generateContent`. Mirrors Go `buildFullRequestURL` lines 178-187.
fn gemini_content_action(stream: bool) -> &'static str {
    if stream {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    }
}

/// Resolve the outbound URL + auth for a Gemini content (generate / stream)
/// request. Pure: no HTTP I/O.
///
/// Mirrors Go `OutboundTransformer.buildFullRequestURL`
/// (`conduit/llm/transformer/gemini/outbound.go:170-215`) plus the auth
/// resolution in `TransformRequest` (`outbound.go:131-160`):
///
/// - **Direct** (`platform_type != "vertex"`):
///   `{base_url}/{version}/models/{model}:{action}` with `x-goog-api-key`
///   header auth.
/// - **Vertex** (`platform_type == "vertex"`):
///   - If `base_url` contains `/v1/` → `{base_url}/publishers/google/models/{model}:{action}`
///   - Else → `{base_url}/v1/publishers/google/models/{model}:{action}`
///   Auth is `VertexOAuth` (the transformer itself does not attach an API-key
///   header; the platform/credentials layer injects the OAuth bearer token).
/// - **Custom endpoint path** (`endpoint_path` non-empty, must start with `/`):
///   short-circuits to `{base_url}{endpoint_path}` regardless of platform.
///
/// `api_key` is only consulted for the Direct platform; on Vertex it is
/// ignored. An empty `api_key` on Direct still yields `GeminiAuth::ApiKey` with
/// an empty string (mirroring Go, which leaves `authConfig` nil when the key is
/// empty — we surface the scheme and let the caller decide).
pub fn resolve_gemini_outbound_request(
    config: &GeminiOutboundConfig,
    model: &str,
    stream: bool,
    api_key: &str,
) -> TransformerResult<GeminiOutboundRequest> {
    // Custom endpoint path short-circuit (Go `buildFullRequestURL` lines 172-174).
    if !config.endpoint_path.is_empty() {
        if !config.endpoint_path.starts_with('/') {
            return Err(conduit_core::ConduitError::invalid_request(format!(
                "endpoint_path must start with '/': {}",
                config.endpoint_path
            )));
        }
        let url = format!(
            "{}{}",
            config.base_url.trim_end_matches('/'),
            config.endpoint_path
        );
        return Ok(GeminiOutboundRequest {
            url,
            auth: platform_auth(config.platform_type, api_key),
        });
    }

    let action = gemini_content_action(stream);

    let url = match config.platform_type {
        GeminiPlatformType::Vertex => {
            // Go: if baseURL contains "/v1/" → no extra /v1 prefix.
            let trimmed = config.base_url.trim_end_matches('/');
            if trimmed.contains("/v1/") {
                format!("{trimmed}/publishers/google/models/{model}:{action}")
            } else {
                format!("{trimmed}/v1/publishers/google/models/{model}:{action}")
            }
        }
        GeminiPlatformType::Direct => {
            let version = if config.api_version.is_empty() {
                GEMINI_DEFAULT_API_VERSION
            } else {
                config.api_version.as_str()
            };
            format!(
                "{}/{version}/models/{model}:{action}",
                config.base_url.trim_end_matches('/'),
            )
        }
    };

    Ok(GeminiOutboundRequest {
        url,
        auth: platform_auth(config.platform_type, api_key),
    })
}

fn platform_auth(platform: GeminiPlatformType, api_key: &str) -> GeminiAuth {
    match platform {
        GeminiPlatformType::Direct => GeminiAuth::ApiKey {
            api_key: api_key.to_string(),
        },
        GeminiPlatformType::Vertex => GeminiAuth::VertexOAuth,
    }
}

// ---------------------------------------------------------------------------
// RUST-P7-008 S14 — Gemini provider-specific thinking-config helpers
// (conduit/llm/transformer/gemini/convert.go:365-423)
//
// Three small pure functions the Go Gemini outbound/inbound converters use
// to translate between OpenAI-style `reasoning_effort` strings ("low",
// "medium", "high", "xhigh") and Gemini-native `thinkingBudget` token
// counts. They are the only provider-specific thinking-field quirks the Go
// side surfaces; capturing them here lets the future Gemini outbound port
// compose them without re-reading convert.go.
//
// Mirrored from `conduit/llm/transformer/gemini/convert.go`:
//   - `thinkingBudgetToReasoningEffort`     (lines 365-374)
//   - `shouldUseThinkingLevelForBudget`     (lines 379-394)
//   - `reasoningEffortToThinkingBudgetWithConfig` (lines 408-423)
// plus the `defaultGeminiReasoningEffortMapping` table (lines 397-402).
//
// Golden cases mirror `outbound_convert_test.go:2651-2792`
// (TestThinkingBudgetToReasoningEffort + TestReasoningEffortToThinkingBudget
// + TestReasoningEffortToThinkingBudgetWithConfig).
// ---------------------------------------------------------------------------

/// Default mapping from `reasoning_effort` string to Gemini `thinkingBudget`
/// token count. Mirrors Go's `defaultGeminiReasoningEffortMapping`
/// (convert.go:397-402). Unknown effort values fall back to `medium` (8192)
/// downstream of this table.
const DEFAULT_GEMINI_REASONING_EFFORT_TO_BUDGET: &[(&str, i64)] = &[
    ("low", 1024),
    ("medium", 8192),
    ("high", 32768),
    // Go maps both "high" and "xhigh" to 32768 tokens.
    ("xhigh", 32768),
];

/// Convert a Gemini `thinkingBudget` token count into the corresponding
/// OpenAI-style `reasoning_effort` string. Mirrors Go's
/// `thinkingBudgetToReasoningEffort` (convert.go:365-374):
///
/// ```text
/// switch {
/// case budget <= 1024:  return "low"
/// case budget <= 8192:  return "medium"
/// default:              return "high"
/// }
/// ```
pub fn gemini_thinking_budget_to_reasoning_effort(budget: i64) -> &'static str {
    if budget <= 1024 {
        "low"
    } else if budget <= 8192 {
        "medium"
    } else {
        "high"
    }
}

/// Decide whether a Gemini-3 model should use `thinkingLevel` instead of
/// `thinkingBudget` for the given budget. Mirrors Go's
/// `shouldUseThinkingLevelForBudget` (convert.go:379-394).
///
/// Gemini-3 prefers the symbolic `thinkingLevel` field over the numeric
/// `thinkingBudget`. The Go switch returns true for Gemini-3 models when
/// the budget falls within standard effort ranges (≤32768). For non
/// Gemini-3 models it always returns false.
pub fn gemini_should_use_thinking_level_for_budget(model: &str, budget: i64) -> bool {
    if !model.to_ascii_lowercase().contains("gemini-3-") {
        return false;
    }
    // Go switch (convert.go:384-393): three branches, all returning true,
    // plus a default returning false. The first two branches are subsumed
    // by the third (1024 ≤ budget ≤ 32768); the only budget value that
    // reaches the default is budget > 32768 OR budget < 0 (treated as
    // not in any of the standard ranges). Mirror Go's intent: budgets in
    // [0, 32768] use thinkingLevel; everything else falls back to budget.
    (0..=32768).contains(&budget)
}

/// Convert an OpenAI-style `reasoning_effort` string to a Gemini
/// `thinkingBudget` token count, honoring an optional per-channel override
/// map. Mirrors Go's `reasoningEffortToThinkingBudgetWithConfig`
/// (convert.go:408-423).
///
/// Resolution order (Go convert.go:410-422):
/// 1. If `config_map` contains the effort, return its value.
/// 2. Otherwise fall back to [`DEFAULT_GEMINI_REASONING_EFFORT_TO_BUDGET`].
/// 3. If neither matches, return 8192 (Go's "default to medium" branch).
pub fn gemini_reasoning_effort_to_thinking_budget(
    effort: &str,
    config_map: Option<&std::collections::BTreeMap<String, i64>>,
) -> i64 {
    if let Some(map) = config_map {
        if let Some(budget) = map.get(effort) {
            return *budget;
        }
    }
    for (key, budget) in DEFAULT_GEMINI_REASONING_EFFORT_TO_BUDGET {
        if *key == effort {
            return *budget;
        }
    }
    // Go convert.go:422: "Default to medium if not found".
    8192
}

// ============================================================================
// GeminiOutboundTransformer — concrete `OutboundTransformer` impl.
//
// Go parity: `OutboundTransformer` struct in
// `conduit/llm/transformer/gemini/outbound.go:51-53`. Holds a
// [`GeminiOutboundConfig`] and an API key string. The core methods delegate to
// the pure-function helpers already ported above.
// ============================================================================

/// Concrete Gemini outbound transformer implementing the
/// [`OutboundTransformer`] trait.
///
/// Mirrors Go `OutboundTransformer` (`conduit/llm/transformer/gemini/outbound.go:51`).
/// Holds the cleaned-up configuration and an optional API key. Construction is
/// via [`GeminiOutboundTransformer::new`] (mirrors Go `NewOutboundTransformer`).
#[derive(Debug, Clone)]
pub struct GeminiOutboundTransformer {
    /// Cleaned-up config (base URL, API version, platform type).
    pub config: GeminiOutboundConfig,
    /// API key for the `x-goog-api-key` header (Direct platform). Empty for
    /// Vertex (OAuth is injected upstream).
    pub api_key: String,
}

impl GeminiOutboundTransformer {
    /// Create a new Gemini outbound transformer, mirroring Go
    /// `NewOutboundTransformer` (`outbound.go:56-63`).
    ///
    /// Runs [`cleanup_gemini_config`] on the provided `base_url` /
    /// `api_version`, then stores the key.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        let config = cleanup_gemini_config(base_url.to_string(), String::new());
        Self {
            config,
            api_key: api_key.to_string(),
        }
    }

    /// Create from a full [`GeminiOutboundConfig`] + API key. The config is
    /// assumed to be already cleaned up (e.g. via [`cleanup_gemini_config`]).
    pub fn with_config(config: GeminiOutboundConfig, api_key: String) -> Self {
        Self { config, api_key }
    }
}

impl OutboundTransformer for GeminiOutboundTransformer {
    fn name(&self) -> &'static str {
        "gemini"
    }

    /// Build the outbound HTTP request from a unified `LlmRequest`.
    ///
    /// Go parity: `OutboundTransformer.TransformRequest` (outbound.go:104-183).
    /// Delegates body building to [`build_gemini_outbound_body`] and URL
    /// resolution to [`resolve_gemini_outbound_request`].
    fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
        // Build the Gemini-shaped body from the unified request.
        let body_value = build_gemini_outbound_body(request)?;
        let body_bytes = serde_json::to_vec(&body_value).map_err(|e| {
            ConduitError::new(
                ErrorKind::InvalidRequest,
                format!("failed to serialize gemini request body: {e}"),
            )
        })?;

        let model = request.model.as_deref().unwrap_or("");

        // Resolve URL + auth.
        let resolved =
            resolve_gemini_outbound_request(&self.config, model, request.stream, &self.api_key)?;
        let request_path = url::Url::parse(&resolved.url)
            .map(|url| {
                let mut path = url.path().to_string();
                if let Some(query) = url.query() {
                    path.push('?');
                    path.push_str(query);
                }
                path
            })
            .unwrap_or_else(|_| resolved.url.clone());

        let mut headers = BTreeMap::<String, String>::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("accept".to_string(), "application/json".to_string());

        // Attach auth header.
        match &resolved.auth {
            GeminiAuth::ApiKey { api_key } if !api_key.is_empty() => {
                headers.insert("x-goog-api-key".to_string(), api_key.clone());
            }
            _ => {}
        }

        Ok(HttpRequest {
            method: "POST".to_string(),
            url: Some(resolved.url),
            path: request_path,
            headers,
            body: Some(body_bytes),
            request_type: Some(request.request_type),
            api_format: Some(ApiFormat::GeminiContents),
            skip_inbound_query_merge: true,
            ..HttpRequest::default()
        })
    }

    /// Pass-through — Gemini does not modify the raw HTTP response envelope.
    fn outbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        Ok(response)
    }

    /// Pass-through — raw stream events are not modified before
    /// `transform_stream` processes them.
    fn outbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        Ok(event)
    }

    /// Convert an HTTP error response into a structured `ConduitError`.
    ///
    /// Go parity: `OutboundTransformer.TransformError` (outbound.go:264-294).
    fn outbound_error(&self, response: HttpResponse) -> TransformerResult<ConduitError> {
        let status = response.status;
        let body_bytes = response.body.as_deref().unwrap_or(&[]);

        // Try to parse as a Gemini error envelope.
        if let Ok(gemini_err) = serde_json::from_slice::<Value>(body_bytes)
            && let Some(err_obj) = gemini_err.get("error")
        {
            let message = err_obj
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Request failed.");
            let err_status = err_obj
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("api_error");

            return Ok(ConduitError::new(
                ErrorKind::InvalidResponse,
                format!("[{err_status}] {message} (HTTP {status})"),
            ));
        }

        // Fallback: raw body as the error message.
        let raw = String::from_utf8_lossy(body_bytes);
        Ok(ConduitError::new(
            ErrorKind::InvalidResponse,
            format!("HTTP {status}: {raw}"),
        ))
    }

    /// Convert a Gemini-format HTTP response into the unified [`LlmResponse`].
    ///
    /// Go parity: `OutboundTransformer.TransformResponse` (outbound.go:229-261).
    /// The default trait implementation (JSON-parse into `LlmResponse`) only works
    /// for OpenAI-compatible providers. Gemini uses `candidates[].content.parts[]`
    /// instead of `choices[].message`, so we parse as
    /// [`GeminiGenerateContentResponse`] and delegate to the already-ported
    /// [`gemini_chunk_to_llm_response`].
    fn transform_response(&self, response: HttpResponse) -> TransformerResult<LlmResponse> {
        // Go: "if httpResp.StatusCode >= 400"
        if response.status >= 400 {
            return Err(ConduitError::new(
                ErrorKind::InvalidResponse,
                format!("HTTP error {}", response.status),
            ));
        }

        // Extract JSON from the response — prefer pre-parsed `json_body`, fall
        // back to raw `body` bytes.
        let json_value = if let Some(value) = response.json_body.as_ref() {
            value.clone()
        } else if let Some(bytes) = response.body.as_ref() {
            if bytes.is_empty() {
                return Err(ConduitError::new(
                    ErrorKind::InvalidResponse,
                    "response body is empty",
                ));
            }
            serde_json::from_slice::<Value>(bytes).map_err(|e| {
                ConduitError::new(
                    ErrorKind::InvalidResponse,
                    format!("failed to parse gemini response body as JSON: {e}"),
                )
            })?
        } else {
            return Err(ConduitError::new(
                ErrorKind::InvalidResponse,
                "response body is empty",
            ));
        };

        // Deserialize into the Gemini-specific response type.
        let gemini_resp: GeminiGenerateContentResponse = serde_json::from_value(json_value)
            .map_err(|e| {
                ConduitError::new(
                    ErrorKind::InvalidResponse,
                    format!("failed to unmarshal gemini response: {e}"),
                )
            })?;

        // Convert to unified response (non-streaming, tool_call_index_offset = 0).
        let (llm_resp, _next_tool_call_index) =
            gemini_chunk_to_llm_response(&gemini_resp, false, 0);

        Ok(llm_resp)
    }

    /// Convert Gemini stream events into unified [`LlmResponse`] chunks.
    ///
    /// Go parity: wraps [`parse_gemini_stream_event`] which mirrors Go
    /// `transformStreamChunkWithState` (outbound_stream.go:47-77). Maintains a
    /// running `tool_call_index_offset` across chunks.
    fn transform_stream(
        &self,
        events: Box<dyn Iterator<Item = StreamEvent> + Send>,
    ) -> TransformerResult<Box<dyn Iterator<Item = LlmResponse> + Send>> {
        Ok(Box::new(GeminiStreamIter {
            inner: events,
            tool_call_index: 0,
        }))
    }
}

/// Iterator adapter for Gemini streaming. Wraps the raw `StreamEvent` iterator,
/// calling [`parse_gemini_stream_event`] and threading the tool-call index.
struct GeminiStreamIter {
    inner: Box<dyn Iterator<Item = StreamEvent> + Send>,
    tool_call_index: i64,
}

impl Iterator for GeminiStreamIter {
    type Item = LlmResponse;

    fn next(&mut self) -> Option<LlmResponse> {
        loop {
            let event = self.inner.next()?;
            match parse_gemini_stream_event(Some(&event), self.tool_call_index) {
                Ok((Some(resp), next_idx)) => {
                    self.tool_call_index = next_idx;
                    return Some(resp);
                }
                Ok((None, next_idx)) => {
                    self.tool_call_index = next_idx;
                    // Skip empty events; pull the next one.
                    continue;
                }
                Err(_) => {
                    // Parse error — skip malformed event (Go mirrors this:
                    // errors in stream conversion are surfaced per-event, the
                    // iterator continues).
                    continue;
                }
            }
        }
    }
}

// ============================================================================
// Gemini error envelope — mirrors Go `GeminiError` / `ErrorDetail`
// (conduit/llm/transformer/gemini/model.go:483-506) and
// `mapHTTPStatusToGeminiStatus` (inbound.go:192-216).
// ============================================================================

/// Gemini error envelope. Mirrors Go `GeminiError`
/// (`conduit/llm/transformer/gemini/model.go:483-488`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeminiErrorEnvelope {
    pub error: GeminiErrorDetail,
}

/// Gemini error detail. Mirrors Go `ErrorDetail`
/// (`conduit/llm/transformer/gemini/model.go:491-506`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeminiErrorDetail {
    pub code: u16,
    pub message: String,
    pub status: String,
}

/// Map an HTTP status code to a Gemini status string.
/// Mirrors Go `mapHTTPStatusToGeminiStatus` (inbound.go:192-216).
///
/// Delegates to `conduit_core` so the HTTP layer — which renders the same
/// envelope per inbound route but does not depend on this crate — shares one
/// source of truth.
pub fn map_http_status_to_gemini_status(status_code: u16) -> &'static str {
    conduit_core::map_http_status_to_gemini_status(status_code)
}

/// Build a Gemini error JSON envelope from an HTTP status code and message.
fn build_gemini_error_body(status_code: u16, message: &str) -> Vec<u8> {
    let envelope = GeminiErrorEnvelope {
        error: GeminiErrorDetail {
            code: status_code,
            message: message.to_string(),
            status: map_http_status_to_gemini_status(status_code).to_string(),
        },
    };
    // Serialization of a simple struct should not fail; but if it does,
    // return a minimal fallback rather than panicking.
    serde_json::to_vec(&envelope).unwrap_or_else(|_| {
        format!(
            r#"{{"error":{{"code":{status_code},"message":"{}","status":"INTERNAL"}}}}"#,
            message.replace('"', "\\\"")
        )
        .into_bytes()
    })
}

// ============================================================================
// Unified LlmResponse → Gemini generateContent response conversion
//
// Mirrors Go `convertLLMToGeminiResponse` (inbound_convert.go:487-510) +
// `convertLLMChoiceToGeminiCandidate` (inbound_convert.go:512-653) +
// `convertLLMFinishReasonToGemini` (convert.go:120-137) +
// `convertToGeminiUsage` (convert.go:490-520).
// ============================================================================

/// Convert a unified finish reason to a Gemini finish reason string.
/// Mirrors Go `convertLLMFinishReasonToGemini` (convert.go:120-137).
pub fn llm_finish_reason_to_gemini(reason: Option<&str>) -> String {
    let Some(reason) = reason else {
        return String::new();
    };
    match reason {
        "stop" => "STOP",
        "length" => "MAX_TOKENS",
        "content_filter" => "SAFETY",
        "tool_calls" => "STOP",
        _ => "STOP",
    }
    .to_string()
}

/// Convert unified [`Usage`] to Gemini [`GeminiUsageMetadata`].
/// Mirrors Go `convertToGeminiUsage` (convert.go:490-520).
pub fn llm_usage_to_gemini(usage: Option<&Usage>) -> Option<GeminiUsageMetadata> {
    let usage = usage?;

    let mut thoughts_token_count: i64 = 0;
    let mut candidates_token_count = usage.completion_tokens as i64;

    // If reasoning tokens are present, subtract from candidates count.
    if usage.completion_details.reasoning_tokens > 0 {
        thoughts_token_count = usage.completion_details.reasoning_tokens as i64;
        candidates_token_count =
            (usage.completion_tokens as i64).saturating_sub(thoughts_token_count);
    }

    let cached_content_token_count = if usage.prompt_details.cached_tokens > 0 {
        usage.prompt_details.cached_tokens as i64
    } else {
        0
    };

    Some(GeminiUsageMetadata {
        prompt_token_count: usage.prompt_tokens as i64,
        candidates_token_count,
        total_token_count: usage.total_tokens as i64,
        cached_content_token_count,
        thoughts_token_count,
    })
}

/// Convert a unified [`LlmMessage`] to Gemini `Content` parts (as a JSON value).
/// Mirrors Go `convertLLMChoiceToGeminiCandidate` (inbound_convert.go:512-653).
fn convert_llm_message_to_gemini_parts(msg: &LlmMessage) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();

    // Reasoning content (thinking) first — Go inbound_convert.go:550-557.
    if let Some(reasoning) = msg.reasoning_content.as_ref().filter(|s| !s.is_empty()) {
        parts.push(serde_json::json!({ "text": reasoning, "thought": true }));
    }

    // Text content — Go inbound_convert.go:559-601.
    match msg.content.as_ref() {
        Some(MessageContent::Text(t)) if !t.is_empty() => {
            parts.push(serde_json::json!({ "text": t }));
        }
        Some(MessageContent::Parts(content_parts)) => {
            for cp in content_parts {
                match cp.part_type.as_str() {
                    "text" => {
                        if let Some(t) = cp.text.as_ref().filter(|s| !s.is_empty()) {
                            parts.push(serde_json::json!({ "text": t }));
                        }
                    }
                    "image_url" => {
                        if let Some(url) = cp
                            .image_url
                            .as_ref()
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            && let Some(part) = convert_image_url_to_gemini_part(url)
                        {
                            parts.push(part);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    // Tool calls — Go inbound_convert.go:605-629.
    for tc in &msg.tool_calls {
        let name = tc
            .function
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args_str = tc
            .function
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args_value: Value = if args_str.is_empty() {
            Value::Null
        } else {
            serde_json::from_str::<Value>(args_str)
                .unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
        };

        let mut fc = serde_json::Map::new();
        if let Some(id) = tc.id.as_ref().filter(|s| !s.is_empty()) {
            fc.insert("id".to_string(), Value::String(id.clone()));
        }
        if !name.is_empty() {
            fc.insert("name".to_string(), Value::String(name.to_string()));
        }
        if !args_value.is_null() {
            fc.insert("args".to_string(), args_value);
        }
        parts.push(serde_json::json!({ "functionCall": Value::Object(fc) }));
    }

    parts
}

/// Convert a unified [`Choice`] to a Gemini `Candidate` JSON value.
/// Mirrors Go `convertLLMChoiceToGeminiCandidate` (inbound_convert.go:512-653).
fn convert_llm_choice_to_gemini_candidate(choice: &Choice, is_stream: bool) -> Value {
    let mut candidate = serde_json::Map::new();
    candidate.insert("index".to_string(), Value::from(choice.index));

    // Pick message or delta depending on streaming mode.
    let msg: Option<&LlmMessage> = if is_stream {
        choice.delta.as_ref().or(choice.message.as_ref())
    } else {
        choice.message.as_ref().or(choice.delta.as_ref())
    };

    if let Some(msg) = msg {
        let parts = convert_llm_message_to_gemini_parts(msg);
        if !parts.is_empty() {
            candidate.insert(
                "content".to_string(),
                serde_json::json!({
                    "role": "model",
                    "parts": parts,
                }),
            );
        }
    }

    // Finish reason — Go inbound_convert.go:650.
    let gemini_reason = llm_finish_reason_to_gemini(choice.finish_reason.as_deref());
    if !gemini_reason.is_empty() {
        candidate.insert("finishReason".to_string(), Value::String(gemini_reason));
    }

    Value::Object(candidate)
}

/// Convert a unified [`LlmResponse`] to a Gemini `GenerateContentResponse` JSON.
/// Mirrors Go `convertLLMToGeminiResponse` (inbound_convert.go:487-510).
///
/// `is_stream` selects delta vs message from each choice, matching Go.
pub fn convert_llm_to_gemini_response(llm_resp: &LlmResponse, is_stream: bool) -> Value {
    let mut resp = serde_json::Map::new();

    if !llm_resp.id.is_empty() {
        resp.insert("responseId".to_string(), Value::String(llm_resp.id.clone()));
    }
    if !llm_resp.model.is_empty() {
        resp.insert(
            "modelVersion".to_string(),
            Value::String(llm_resp.model.clone()),
        );
    }

    // Candidates
    let candidates: Vec<Value> = llm_resp
        .choices
        .iter()
        .map(|c| convert_llm_choice_to_gemini_candidate(c, is_stream))
        .collect();
    if !candidates.is_empty() {
        resp.insert("candidates".to_string(), Value::Array(candidates));
    }

    // Usage metadata
    if let Some(gemini_usage) = llm_usage_to_gemini(llm_resp.usage.as_ref())
        && let Ok(usage_value) = serde_json::to_value(&gemini_usage)
    {
        resp.insert("usageMetadata".to_string(), usage_value);
    }

    Value::Object(resp)
}

// ============================================================================
// GeminiInboundTransformer — concrete `InboundTransformer` impl.
//
// Go parity: `InboundTransformer` struct in
// `conduit/llm/transformer/gemini/inbound.go:21-26`. Zero-size struct — all
// conversion logic delegates to the pure-function helpers above.
// ============================================================================

/// Concrete Gemini inbound transformer implementing the
/// [`InboundTransformer`] trait.
///
/// Mirrors Go `InboundTransformer` (`conduit/llm/transformer/gemini/inbound.go:21`).
/// Converts Gemini-format client requests into the unified `LlmRequest` and
/// unified `LlmResponse` back into Gemini-format client responses.
#[derive(Debug, Clone, Default)]
pub struct GeminiInboundTransformer;

impl GeminiInboundTransformer {
    /// Create a new Gemini inbound transformer, mirroring Go
    /// `NewInboundTransformer` (`inbound.go:24-26`).
    pub fn new() -> Self {
        Self
    }
}

impl InboundTransformer for GeminiInboundTransformer {
    fn name(&self) -> &'static str {
        "gemini"
    }

    /// Parse a Gemini `generateContent` request body into the unified `LlmRequest`.
    ///
    /// Go parity: `InboundTransformer.TransformRequest` (inbound.go:53-88).
    /// Extracts model + stream flag from the URL path, deserializes the body as
    /// `GeminiGenerateContentRequest`, validates, and converts via
    /// [`parse_gemini_contents_to_llm_request`].
    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        // Go: "if httpReq == nil" / "len(httpReq.Body) == 0"
        let body_bytes = request
            .body
            .as_ref()
            .filter(|b| !b.is_empty())
            .ok_or_else(|| ConduitError::new(ErrorKind::InvalidRequest, "request body is empty"))?;

        // Extract model + stream from request path (Go: extractRequestParams).
        let action = parse_gemini_action(&request.path)
            .map_err(|e| ConduitError::new(ErrorKind::InvalidRequest, e.to_string()))?;

        // Deserialize body.
        let gemini_req: GeminiGenerateContentRequest =
            serde_json::from_slice(body_bytes).map_err(|e| {
                ConduitError::new(
                    ErrorKind::InvalidRequest,
                    format!("failed to decode gemini request: {e}"),
                )
            })?;

        // Validate required fields (Go: "contents are required").
        if gemini_req.contents.is_empty() {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "contents are required",
            ));
        }

        // Convert to unified request.
        parse_gemini_contents_to_llm_request(&gemini_req, &action)
    }

    /// Pass-through — the inbound response envelope is not modified before
    /// `transform_response` processes it.
    fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        Ok(response)
    }

    /// Pass-through — raw stream events are not modified before
    /// `transform_stream` processes them.
    fn inbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        Ok(event)
    }

    /// Format an error as a Gemini error envelope.
    ///
    /// Go parity: `InboundTransformer.TransformError` (inbound.go:115-190).
    /// Maps the error kind / status to the appropriate HTTP status + Gemini
    /// status string and wraps in the `{ error: { code, message, status } }`
    /// envelope.
    fn inbound_error(&self, error: &ConduitError) -> TransformerResult<HttpResponse> {
        let (status_code, message) = match error.kind {
            ErrorKind::InvalidRequest => (400_u16, error.to_string()),
            ErrorKind::Unauthorized => (401_u16, error.to_string()),
            ErrorKind::Forbidden => (403_u16, error.to_string()),
            ErrorKind::NotFound => (404_u16, error.to_string()),
            ErrorKind::RateLimited => (429_u16, error.to_string()),
            ErrorKind::Upstream => {
                // Preserve upstream status if available.
                let code = error.provider_status.unwrap_or(502);
                (code, error.to_string())
            }
            _ => (500_u16, "Internal Server Error".to_string()),
        };

        let body = build_gemini_error_body(status_code, &message);
        let mut headers = conduit_llm::model::HeaderMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());

        Ok(HttpResponse {
            status: status_code,
            headers,
            body: Some(body),
            ..HttpResponse::default()
        })
    }

    /// Convert a unified `LlmResponse` into a Gemini `GenerateContentResponse`
    /// HTTP response.
    ///
    /// Go parity: `InboundTransformer.TransformResponse` (inbound.go:91-112).
    /// Converts the unified response to the Gemini `candidates/parts` envelope,
    /// serializes to JSON, and wraps in a 200 response with standard headers.
    fn transform_response(&self, response: LlmResponse) -> TransformerResult<HttpResponse> {
        let gemini_resp = convert_llm_to_gemini_response(&response, false);
        let body = serde_json::to_vec(&gemini_resp).map_err(|e| {
            ConduitError::new(
                ErrorKind::Internal,
                format!("failed to marshal gemini response: {e}"),
            )
        })?;

        let mut headers = conduit_llm::model::HeaderMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Cache-Control".to_string(), "no-cache".to_string());

        Ok(HttpResponse {
            status: 200,
            headers,
            body: Some(body),
            ..HttpResponse::default()
        })
    }

    fn aggregate_stream_chunks(&self, events: Vec<StreamEvent>) -> TransformerResult<HttpResponse> {
        let mut response = GeminiGenerateContentResponse::default();
        let mut candidates = BTreeMap::<i64, GeminiCandidate>::new();

        for event in &events {
            let Some(data) = event.data.as_deref() else {
                continue;
            };
            if data == GEMINI_DONE_MARKER {
                continue;
            }
            let chunk: GeminiGenerateContentResponse =
                serde_json::from_str(data).map_err(|err| {
                    ConduitError::new(
                        ErrorKind::InvalidResponse,
                        "failed to decode Gemini stream chunk",
                    )
                    .with_source(err)
                })?;
            if !chunk.response_id.is_empty() {
                response.response_id = chunk.response_id;
            }
            if !chunk.model_version.is_empty() {
                response.model_version = chunk.model_version;
            }
            if chunk.usage_metadata.is_some() {
                response.usage_metadata = chunk.usage_metadata;
            }
            for mut candidate in chunk.candidates {
                let entry = candidates
                    .entry(candidate.index)
                    .or_insert_with(|| GeminiCandidate {
                        index: candidate.index,
                        content: None,
                        finish_reason: String::new(),
                        extra: BTreeMap::new(),
                    });
                if let Some(content) = candidate.content.take() {
                    let target = entry.content.get_or_insert_with(|| GeminiContent {
                        parts: Vec::new(),
                        role: content.role.clone(),
                    });
                    if !content.role.is_empty() {
                        target.role = content.role;
                    }
                    target.parts.extend(content.parts);
                }
                if !candidate.finish_reason.is_empty() {
                    entry.finish_reason = candidate.finish_reason;
                }
                entry.extra.extend(candidate.extra);
            }
        }

        response.candidates = candidates.into_values().collect();
        let usage = gemini_usage_to_llm(response.usage_metadata.as_ref());
        let completed = response
            .candidates
            .iter()
            .any(|candidate| !candidate.finish_reason.is_empty());
        let body = serde_json::to_vec(&response).map_err(|err| {
            ConduitError::internal("failed to marshal aggregated Gemini response").with_source(err)
        })?;
        let mut headers = conduit_llm::model::HeaderMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Cache-Control".to_string(), "no-cache".to_string());
        let mut metadata = BTreeMap::new();
        metadata.insert("completed".to_string(), Value::Bool(completed));
        if !response.response_id.is_empty() {
            metadata.insert(
                "llm_response_id".to_string(),
                Value::String(response.response_id.clone()),
            );
        }

        Ok(HttpResponse {
            status: 200,
            headers,
            body: Some(body),
            stream: events,
            usage,
            metadata,
            ..HttpResponse::default()
        })
    }

    /// Convert unified `LlmResponse` stream into Gemini SSE stream events.
    ///
    /// Go parity: wraps each `LlmResponse` chunk into a Gemini SSE
    /// `data: <json>\n\n` frame. The Gemini streaming format for `alt=sse`
    /// sends one `GenerateContentResponse` per SSE event.
    fn transform_stream(
        &self,
        events: Box<dyn Iterator<Item = LlmResponse> + Send>,
    ) -> TransformerResult<Box<dyn Iterator<Item = StreamEvent> + Send>> {
        Ok(Box::new(GeminiInboundStreamIter { inner: events }))
    }
}

/// Iterator adapter for Gemini inbound streaming. Converts each unified
/// `LlmResponse` into a Gemini SSE `StreamEvent` by wrapping the
/// Gemini-formatted response JSON as the `data` field.
struct GeminiInboundStreamIter {
    inner: Box<dyn Iterator<Item = LlmResponse> + Send>,
}

impl Iterator for GeminiInboundStreamIter {
    type Item = StreamEvent;

    fn next(&mut self) -> Option<StreamEvent> {
        let resp = self.inner.next()?;

        // Check for [DONE] sentinel (Go appends this at end-of-stream).
        if resp.object == GEMINI_DONE_MARKER {
            return Some(StreamEvent {
                data: Some(GEMINI_DONE_MARKER.to_string()),
                done: true,
                ..StreamEvent::default()
            });
        }

        let gemini_resp = convert_llm_to_gemini_response(&resp, true);
        let data = serde_json::to_string(&gemini_resp).ok()?;
        Some(StreamEvent {
            data: Some(data),
            ..StreamEvent::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- S04 parse_gemini_action --------------------------------------

    #[test]
    fn parse_action_generate_content() -> Result<(), GeminiActionError> {
        let a = parse_gemini_action("/v1beta/models/gemini-2.5-flash:generateContent")?;
        assert_eq!(
            a,
            GeminiAction {
                model: "gemini-2.5-flash".to_string(),
                stream: false
            }
        );
        Ok(())
    }

    #[test]
    fn parse_action_stream_generate_content() -> Result<(), GeminiActionError> {
        let a = parse_gemini_action("/v1beta/models/gemini-2.5-flash:streamGenerateContent")?;
        assert_eq!(
            a,
            GeminiAction {
                model: "gemini-2.5-flash".to_string(),
                stream: true
            }
        );
        Ok(())
    }

    #[test]
    fn parse_action_image_model() -> Result<(), GeminiActionError> {
        let a =
            parse_gemini_action("/v1beta/models/gemini-2.5-flash-image-preview:generateContent")?;
        assert_eq!(a.model, "gemini-2.5-flash-image-preview");
        assert!(!a.stream);
        Ok(())
    }

    #[test]
    fn parse_action_v1_path() -> Result<(), GeminiActionError> {
        let a = parse_gemini_action("/v1/models/gemini-pro:generateContent")?;
        assert_eq!(a.model, "gemini-pro");
        Ok(())
    }

    #[test]
    fn parse_action_unknown_action_rejected() {
        let err = parse_gemini_action("/v1beta/models/gemini-2.5-flash:bogus").err();
        assert!(matches!(err, Some(GeminiActionError::InvalidRequestUrl(_))));
    }

    #[test]
    fn parse_action_no_colon_rejected() {
        let err = parse_gemini_action("/v1beta/models/gemini-2.5-flash").err();
        assert!(matches!(err, Some(GeminiActionError::InvalidRequestUrl(_))));
    }

    #[test]
    fn parse_action_empty_path_rejected() {
        let err = parse_gemini_action("").err();
        assert!(matches!(err, Some(GeminiActionError::InvalidRequestUrl(_))));
    }

    #[test]
    fn parse_action_trailing_slash_only_rejected() {
        let err = parse_gemini_action("/v1beta/models/").err();
        assert!(matches!(err, Some(GeminiActionError::InvalidRequestUrl(_))));
    }

    #[test]
    fn parse_action_empty_model_rejected() {
        let err = parse_gemini_action("/v1beta/models/:generateContent").err();
        assert!(matches!(err, Some(GeminiActionError::InvalidRequestUrl(_))));
    }

    // --- S06 gemini_stream_mode ---------------------------------------

    #[test]
    fn stream_mode_non_streaming_is_json() {
        let a = GeminiAction {
            model: "m".into(),
            stream: false,
        };
        assert_eq!(gemini_stream_mode(&a, None), GeminiStreamMode::Json);
        assert_eq!(gemini_stream_mode(&a, Some("sse")), GeminiStreamMode::Json);
    }

    #[test]
    fn stream_mode_streaming_default_is_json_array() {
        let a = GeminiAction {
            model: "m".into(),
            stream: true,
        };
        assert_eq!(gemini_stream_mode(&a, None), GeminiStreamMode::JsonArray);
    }

    #[test]
    fn stream_mode_streaming_alt_sse_is_sse() {
        let a = GeminiAction {
            model: "m".into(),
            stream: true,
        };
        assert_eq!(gemini_stream_mode(&a, Some("sse")), GeminiStreamMode::Sse);
    }

    #[test]
    fn stream_mode_streaming_alt_other_is_json_array() {
        let a = GeminiAction {
            model: "m".into(),
            stream: true,
        };
        assert_eq!(
            gemini_stream_mode(&a, Some("json")),
            GeminiStreamMode::JsonArray
        );
    }

    // --- S11 contents ---------------------------------------------------

    #[test]
    fn role_mapping() {
        assert_eq!(gemini_role_to_llm_role("model"), "assistant");
        assert_eq!(gemini_role_to_llm_role("user"), "user");
        assert_eq!(gemini_role_to_llm_role(""), "user");
        assert_eq!(gemini_role_to_llm_role("system"), "system");
    }

    #[test]
    fn extract_text_concatenates_text_parts() {
        let content = GeminiContent {
            role: "user".into(),
            parts: vec![
                GeminiPart {
                    text: "Hello".into(),
                    inline_data: None,
                    extra: BTreeMap::new(),
                },
                GeminiPart {
                    text: "Gemini!".into(),
                    inline_data: None,
                    extra: BTreeMap::new(),
                },
            ],
        };
        assert_eq!(extract_text_from_content(Some(&content)), "Hello\nGemini!");
        assert_eq!(extract_text_from_content(None), "");
    }

    #[test]
    fn parse_contents_simple_text() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [
                {"role": "user", "parts": [{"text": "Hello, Gemini!"}]}
            ]
        }"#;
        let req: GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = GeminiAction {
            model: "gemini-2.5-flash".into(),
            stream: false,
        };
        let llm = parse_gemini_contents_to_llm_request(&req, &action)?;

        assert_eq!(llm.api_format, ApiFormat::GeminiContents);
        assert_eq!(llm.request_type, RequestType::Chat);
        assert_eq!(llm.model.as_deref(), Some("gemini-2.5-flash"));
        assert!(!llm.stream);
        let LlmRequestPayload::Chat(chat) = &llm.payload else {
            return Err("expected chat payload".into());
        };
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "user");
        assert_eq!(
            chat.messages[0].content,
            Some(MessageContent::Text("Hello, Gemini!".into()))
        );
        Ok(())
    }

    #[test]
    fn parse_contents_with_system_instruction() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "systemInstruction": {"parts": [{"text": "You are a helpful assistant."}]},
            "contents": [
                {"role": "user", "parts": [{"text": "What is the capital of France?"}]}
            ]
        }"#;
        let req: GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = GeminiAction {
            model: "gemini-2.5-flash".into(),
            stream: false,
        };
        let llm = parse_gemini_contents_to_llm_request(&req, &action)?;
        let LlmRequestPayload::Chat(chat) = &llm.payload else {
            return Err("expected chat payload".into());
        };
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].role, "system");
        assert_eq!(
            chat.messages[0].content,
            Some(MessageContent::Text("You are a helpful assistant.".into()))
        );
        assert_eq!(chat.messages[1].role, "user");
        Ok(())
    }

    #[test]
    fn parse_contents_streaming_flag() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role": "user", "parts": [{"text": "Hi"}]}]
        }"#;
        let req: GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = GeminiAction {
            model: "m".into(),
            stream: true,
        };
        let llm = parse_gemini_contents_to_llm_request(&req, &action)?;
        assert!(llm.stream);
        Ok(())
    }

    #[test]
    fn parse_contents_assistant_role_mapping() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [
                {"role": "user", "parts": [{"text": "ping"}]},
                {"role": "model", "parts": [{"text": "pong"}]}
            ]
        }"#;
        let req: GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = parse_gemini_contents_to_llm_request(&req, &action)?;
        let LlmRequestPayload::Chat(chat) = &llm.payload else {
            return Err("expected chat payload".into());
        };
        assert_eq!(chat.messages[1].role, "assistant");
        Ok(())
    }

    #[test]
    fn parse_contents_inline_data_becomes_image_url() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{
                "role": "user",
                "parts": [
                    {"text": "describe"},
                    {"inlineData": {"mimeType": "image/png", "data": "BASE64"}}
                ]
            }]
        }"#;
        let req: GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = parse_gemini_contents_to_llm_request(&req, &action)?;
        let LlmRequestPayload::Chat(chat) = &llm.payload else {
            return Err("expected chat payload".into());
        };
        let msg = &chat.messages[0];
        let Some(MessageContent::Parts(parts)) = msg.content.as_ref() else {
            return Err("expected parts content".into());
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].part_type, "text");
        assert_eq!(parts[1].part_type, "image_url");
        let url = parts[1]
            .image_url
            .as_ref()
            .and_then(|v| v.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(url, "data:image/png;base64,BASE64");
        Ok(())
    }

    #[test]
    fn parse_contents_empty_contents_rejected() {
        let req = GeminiGenerateContentRequest::default();
        let action = GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let err = parse_gemini_contents_to_llm_request(&req, &action).err();
        assert!(err.is_some());
    }

    #[test]
    fn parse_contents_drops_empty_parts_content() -> Result<(), Box<dyn std::error::Error>> {
        // Empty parts content should be skipped (Go: returns nil msg).
        let body = r#"{
            "contents": [
                {"role": "user", "parts": []},
                {"role": "user", "parts": [{"text": "ok"}]}
            ]
        }"#;
        let req: GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = parse_gemini_contents_to_llm_request(&req, &action)?;
        let LlmRequestPayload::Chat(chat) = &llm.payload else {
            return Err("expected chat payload".into());
        };
        assert_eq!(chat.messages.len(), 1);
        Ok(())
    }

    // --- S08 inbound tools / toolConfig --------------------------------
    //
    // Mirrors Go `TestConvertGeminiToLLMRequest_Tools`
    // (`conduit/llm/transformer/gemini/inbound_convert_test.go:548-797`).
    // Golden cases transcribed from the Go table: function declarations,
    // multiple tools, Google native tools (google_search / code_execution /
    // url_context), and the four toolConfig mode branches.

    /// Borrow the chat payload from an LlmRequest (panics via assert on shape
    /// mismatch — no .expect()/.unwrap() per workspace lints).
    fn llm_req_chat(llm: &super::LlmRequest) -> &super::ChatRequest {
        match &llm.payload {
            super::LlmRequestPayload::Chat(c) => c,
            other => panic!("expected chat payload, got {other:?}"),
        }
    }

    fn llm_req_tool_choice(llm: &super::LlmRequest) -> super::Value {
        let chat = llm_req_chat(llm);
        match &chat.tool_choice {
            Some(v) => v.clone(),
            None => panic!("tool_choice should be set"),
        }
    }

    /// "request with tools" — single function declaration with `parameters`
    /// (legacy format). Go inbound_convert_test.go:550-583.
    #[test]
    fn gemini_tools_single_function() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"What's the weather?"}]}],
            "tools": [{
                "functionDeclarations": [{
                    "name": "get_weather",
                    "description": "Get weather information",
                    "parameters": {"type":"object","properties":{"location":{"type":"string"}}}
                }]
            }]
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        let chat = llm_req_chat(&llm);
        assert_eq!(chat.tools.len(), 1);
        assert_eq!(chat.tools[0].tool_type, "function");
        assert_eq!(chat.tools[0].name.as_deref(), Some("get_weather"));
        assert_eq!(
            chat.tools[0].description.as_deref(),
            Some("Get weather information")
        );
        assert!(chat.tools[0].parameters.is_some());
        Ok(())
    }

    /// "request with multiple tools" — two function declarations in one Tool.
    /// Go inbound_convert_test.go:586-617.
    #[test]
    fn gemini_tools_multiple_functions() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"Help me"}]}],
            "tools": [{
                "functionDeclarations": [
                    {"name":"tool1","description":"First tool"},
                    {"name":"tool2","description":"Second tool"}
                ]
            }]
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        let chat = llm_req_chat(&llm);
        assert_eq!(chat.tools.len(), 2);
        assert_eq!(chat.tools[0].name.as_deref(), Some("tool1"));
        assert_eq!(chat.tools[1].name.as_deref(), Some("tool2"));
        Ok(())
    }

    /// `parametersJsonSchema` (new format) should be preserved on the unified
    /// tool's `parameters` field — Go inbound_convert.go:179-183.
    #[test]
    fn gemini_tools_parameters_json_schema() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"x"}]}],
            "tools": [{
                "functionDeclarations": [{
                    "name": "fn",
                    "parametersJsonSchema": {"type":"object","properties":{"a":{"type":"string"}}}
                }]
            }]
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        let chat = llm_req_chat(&llm);
        assert_eq!(chat.tools.len(), 1);
        match &chat.tools[0].parameters {
            Some(p) => assert_eq!(p["type"], "object"),
            None => panic!("parameters should be set"),
        }
        Ok(())
    }

    /// "request with google search and code execution tools".
    /// Go inbound_convert_test.go:619-651.
    #[test]
    fn gemini_tools_google_native_search_and_code() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"Search and run"}]}],
            "tools": [{"googleSearch": {}}, {"codeExecution": {}}]
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        let chat = llm_req_chat(&llm);
        assert_eq!(chat.tools.len(), 2);
        assert_eq!(chat.tools[0].tool_type, "google_search");
        assert_eq!(chat.tools[1].tool_type, "google_code_execution");
        Ok(())
    }

    /// "request with url context tool". Go inbound_convert_test.go:654-680.
    #[test]
    fn gemini_tools_url_context() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"Fetch URL content"}]}],
            "tools": [{"urlContext": {}}]
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        let chat = llm_req_chat(&llm);
        assert_eq!(chat.tools.len(), 1);
        assert_eq!(chat.tools[0].tool_type, "google_url_context");
        Ok(())
    }

    /// "request with all grounding tools". Go inbound_convert_test.go:683-710.
    #[test]
    fn gemini_tools_all_grounding() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"Use all tools"}]}],
            "tools": [{"googleSearch": {}}, {"codeExecution": {}}, {"urlContext": {}}]
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        let chat = llm_req_chat(&llm);
        assert_eq!(chat.tools.len(), 3);
        assert_eq!(chat.tools[0].tool_type, "google_search");
        assert_eq!(chat.tools[1].tool_type, "google_code_execution");
        assert_eq!(chat.tools[2].tool_type, "google_url_context");
        Ok(())
    }

    /// toolConfig AUTO → unified tool_choice = "auto".
    /// Go inbound_convert_test.go:712-728 (and convert.go:313).
    #[test]
    fn gemini_tool_config_auto() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"Test"}]}],
            "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}}
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        assert_eq!(llm_req_tool_choice(&llm), "auto");
        Ok(())
    }

    /// toolConfig NONE → unified tool_choice = "none".
    /// Go inbound_convert_test.go:730-746.
    #[test]
    fn gemini_tool_config_none() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"Test"}]}],
            "toolConfig": {"functionCallingConfig": {"mode": "NONE"}}
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        assert_eq!(llm_req_tool_choice(&llm), "none");
        Ok(())
    }

    /// toolConfig ANY with zero or >1 names → unified tool_choice = "required".
    /// Go inbound_convert_test.go:748-764.
    #[test]
    fn gemini_tool_config_any_required() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"Test"}]}],
            "toolConfig": {"functionCallingConfig": {"mode": "ANY"}}
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        assert_eq!(llm_req_tool_choice(&llm), "required");
        Ok(())
    }

    /// toolConfig ANY with exactly one name → named-tool-choice object.
    /// Go inbound_convert_test.go:766-792 (and convert.go:317-323).
    #[test]
    fn gemini_tool_config_any_named() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"Test"}]}],
            "toolConfig": {"functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": ["specific_function"]
            }}
        }"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        let tc = llm_req_tool_choice(&llm);
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "specific_function");
        Ok(())
    }

    /// No tools / no toolConfig → empty tools vec, None tool_choice
    /// (Go: chatReq.Tools stays nil, ToolChoice stays nil).
    #[test]
    fn gemini_tools_absent_when_not_in_request() -> Result<(), Box<dyn std::error::Error>> {
        let body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let req: super::GeminiGenerateContentRequest = serde_json::from_str(body)?;
        let action = super::GeminiAction {
            model: "m".into(),
            stream: false,
        };
        let llm = super::parse_gemini_contents_to_llm_request(&req, &action)?;
        let chat = llm_req_chat(&llm);
        assert!(chat.tools.is_empty());
        assert!(chat.tool_choice.is_none());
        Ok(())
    }

    // --- S08 native-tools gating helper --------------------------------
    //
    // Mirrors Go gating in `conduit/llm/transformer/gemini/openai/outbound.go`
    // lines 309-325 and `llm.IsGoogleNativeTool` (tools.go:198-203).

    #[test]
    fn is_google_native_tool_recognizes_google_prefix() {
        assert!(super::is_google_native_tool("google_search"));
        assert!(super::is_google_native_tool("google_code_execution"));
        assert!(super::is_google_native_tool("google_url_context"));
        assert!(!super::is_google_native_tool("function"));
        assert!(!super::is_google_native_tool("web_search"));
    }

    #[test]
    fn gemini_supports_native_tools_gating() {
        // Only `gemini` and `gemini_vertex` channels support native tools.
        assert!(super::gemini_supports_native_tools("gemini"));
        assert!(super::gemini_supports_native_tools("gemini_vertex"));
        // `gemini_openai` (OpenAI-compatible endpoint) does NOT.
        assert!(!super::gemini_supports_native_tools("gemini_openai"));
        // Case-insensitive, matching Go channel-type constants.
        assert!(super::gemini_supports_native_tools("Gemini"));
        assert!(!super::gemini_supports_native_tools(""));
    }

    // --- S07 Gemini embedding ------------------------------------------
    //
    // Mirrors Go `conduit/llm/transformer/gemini/embedding_test.go` for the
    // pure helpers (task-type mapping, input→texts, URL building,
    // single/batch response conversion, request construction, action parsing).
    // The full unified `llm.Request`/`llm.Response` round-trip requires
    // Go-shaped `EmbeddingInput`/`Task` fields not present in the Rust
    // conduit-llm `EmbeddingRequest` — `pending source snapshot`.

    #[test]
    fn map_embedding_task_type_mirrors_go_switch() {
        // Mirrors Go `TestMapEmbeddingTaskType` (embedding_test.go:266+).
        assert_eq!(
            super::map_gemini_embedding_task_type("retrieval.query"),
            "RETRIEVAL_QUERY"
        );
        assert_eq!(
            super::map_gemini_embedding_task_type("retrieval.passage"),
            "RETRIEVAL_DOCUMENT"
        );
        assert_eq!(
            super::map_gemini_embedding_task_type("text-matching"),
            "SEMANTIC_SIMILARITY"
        );
        assert_eq!(
            super::map_gemini_embedding_task_type("classification"),
            "CLASSIFICATION"
        );
        assert_eq!(
            super::map_gemini_embedding_task_type("clustering"),
            "CLUSTERING"
        );
        assert_eq!(super::map_gemini_embedding_task_type("unknown"), "");
        assert_eq!(super::map_gemini_embedding_task_type(""), "");
        // Case-insensitive normalization.
        assert_eq!(
            super::map_gemini_embedding_task_type("RETRIEVAL.QUERY"),
            "RETRIEVAL_QUERY"
        );
    }

    #[test]
    fn embedding_input_to_texts_string() {
        let v = serde_json::Value::String("Hello world".to_string());
        assert_eq!(
            super::gemini_embedding_input_to_texts(Some(&v)),
            vec!["Hello world".to_string()]
        );
    }

    #[test]
    fn embedding_input_to_texts_string_array() {
        let v = serde_json::json!(["Hello", "World", "Test"]);
        assert_eq!(
            super::gemini_embedding_input_to_texts(Some(&v)),
            vec!["Hello".to_string(), "World".to_string(), "Test".to_string()]
        );
    }

    #[test]
    fn embedding_input_to_texts_empty_and_none() {
        let empty = serde_json::Value::String(String::new());
        assert!(super::gemini_embedding_input_to_texts(Some(&empty)).is_empty());
        assert!(super::gemini_embedding_input_to_texts(None).is_empty());
        // Token-array inputs (non-string elements) are unsupported.
        let ints = serde_json::json!([1, 2, 3]);
        assert!(super::gemini_embedding_input_to_texts(Some(&ints)).is_empty());
    }

    #[test]
    fn embedding_kind_for_texts_single_vs_batch() {
        assert_eq!(
            super::gemini_embedding_kind_for_texts(&["a".to_string()]),
            super::GeminiEmbeddingKind::Single
        );
        assert_eq!(
            super::gemini_embedding_kind_for_texts(&["a".to_string(), "b".to_string()]),
            super::GeminiEmbeddingKind::Batch
        );
        assert_eq!(
            super::gemini_embedding_kind_for_texts(&[]),
            super::GeminiEmbeddingKind::Batch
        );
        assert_eq!(super::GeminiEmbeddingKind::Single.action(), "embedContent");
        assert_eq!(
            super::GeminiEmbeddingKind::Batch.action(),
            "batchEmbedContents"
        );
    }

    #[test]
    fn build_embedding_url_standard_api() {
        // Mirrors Go `TestBuildEmbeddingURL` ("standard API" + batch).
        let url = super::build_gemini_embedding_url(
            "https://generativelanguage.googleapis.com",
            "v1beta",
            "gemini-embedding-001",
            super::GeminiEmbeddingKind::Single,
        );
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-001:embedContent"
        );
        let url_batch = super::build_gemini_embedding_url(
            "https://generativelanguage.googleapis.com",
            "v1beta",
            "gemini-embedding-001",
            super::GeminiEmbeddingKind::Batch,
        );
        assert_eq!(
            url_batch,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-001:batchEmbedContents"
        );
    }

    #[test]
    fn build_embedding_url_default_version_when_empty() {
        // Go falls back to `DefaultAPIVersion` ("v1beta") when version is "".
        let url = super::build_gemini_embedding_url(
            "https://generativelanguage.googleapis.com",
            "",
            "gemini-embedding-001",
            super::GeminiEmbeddingKind::Single,
        );
        assert!(url.contains("/v1beta/models/gemini-embedding-001:embedContent"));
    }

    #[test]
    fn build_embedding_url_trims_trailing_slash() {
        let url = super::build_gemini_embedding_url(
            "https://generativelanguage.googleapis.com/",
            "v1beta",
            "m",
            super::GeminiEmbeddingKind::Single,
        );
        assert!(!url.contains("//models"));
        assert!(url.starts_with("https://generativelanguage.googleapis.com/v1beta/"));
    }

    fn test_build_embed_request_content_text(req: &super::GeminiEmbedContentRequest) -> &str {
        match req.content.as_ref() {
            Some(c) if c.parts.len() == 1 => &c.parts[0].text,
            _ => "",
        }
    }

    #[test]
    fn build_embed_content_request_single_text() {
        // Mirrors Go `TestTransformEmbeddingRequest_SingleText` body shape.
        let req = super::build_gemini_embed_content_request(
            "models/gemini-embedding-001",
            "Hello world",
            "",
            None,
        );
        assert_eq!(req.model, "models/gemini-embedding-001");
        assert!(req.content.is_some());
        assert_eq!(test_build_embed_request_content_text(&req), "Hello world");
    }

    #[test]
    fn build_embed_content_request_with_dims_and_task() {
        // Mirrors Go `TestTransformEmbeddingRequest_WithDimensions` +
        // `WithTaskType` shapes.
        let req = super::build_gemini_embed_content_request(
            "models/gemini-embedding-001",
            "Hello",
            "RETRIEVAL_QUERY",
            Some(256),
        );
        assert_eq!(req.task_type, "RETRIEVAL_QUERY");
        assert_eq!(req.output_dimensionality, Some(256));
    }

    #[test]
    fn build_embed_content_request_serializes_camel_case() -> Result<(), Box<dyn std::error::Error>>
    {
        // Confirm JSON parity with Go json tags (camelCase, omitempty).
        let req = super::build_gemini_embed_content_request(
            "models/m",
            "hi",
            "CLASSIFICATION",
            Some(768),
        );
        let json = serde_json::to_string(&req)?;
        // `outputDimensionality` (capital D) must survive camelCase.
        assert!(json.contains("\"outputDimensionality\":768"), "got: {json}");
        assert!(
            json.contains("\"taskType\":\"CLASSIFICATION\""),
            "got: {json}"
        );
        assert!(json.contains("\"model\":\"models/m\""), "got: {json}");
        // `title` is empty → omitempty omits it.
        assert!(!json.contains("\"title\""), "got: {json}");
        Ok(())
    }

    #[test]
    fn convert_single_embedding_response() {
        // Mirrors Go `TestTransformEmbeddingResponse_Single`.
        let resp = super::GeminiEmbedContentResponse {
            embedding: Some(super::GeminiContentEmbedding {
                values: vec![0.1_f32, 0.2, 0.3],
            }),
        };
        let unified = super::convert_single_gemini_embedding_response(&resp);
        assert_eq!(unified.object, "list");
        assert_eq!(unified.data.len(), 1);
        assert_eq!(unified.data[0].object, "embedding");
        assert_eq!(unified.data[0].index, 0);
        assert!((unified.data[0].embedding[0] - 0.1).abs() < 0.001);
        assert!((unified.data[0].embedding[1] - 0.2).abs() < 0.001);
        assert!((unified.data[0].embedding[2] - 0.3).abs() < 0.001);
    }

    #[test]
    fn convert_single_embedding_response_empty() {
        let resp = super::GeminiEmbedContentResponse { embedding: None };
        let unified = super::convert_single_gemini_embedding_response(&resp);
        assert_eq!(unified.object, "list");
        assert!(unified.data.is_empty());
    }

    #[test]
    fn convert_batch_embedding_response() {
        // Mirrors Go `TestTransformEmbeddingResponse_Batch`.
        let resp = super::GeminiBatchEmbedContentsResponse {
            embeddings: vec![
                super::GeminiContentEmbedding {
                    values: vec![0.1_f32, 0.2],
                },
                super::GeminiContentEmbedding {
                    values: vec![0.3_f32, 0.4],
                },
            ],
        };
        let unified = super::convert_batch_gemini_embedding_response(&resp);
        assert_eq!(unified.data.len(), 2);
        assert_eq!(unified.data[0].index, 0);
        assert_eq!(unified.data[1].index, 1);
        assert!((unified.data[0].embedding[0] - 0.1).abs() < 0.001);
        assert!((unified.data[1].embedding[0] - 0.3).abs() < 0.001);
    }

    #[test]
    fn parse_embedding_action_single_and_batch() {
        let parsed = super::parse_gemini_embedding_action(
            "/v1beta/models/gemini-embedding-001:embedContent",
        );
        match parsed {
            Ok((m, k)) => {
                assert_eq!(m, "gemini-embedding-001");
                assert_eq!(k, super::GeminiEmbeddingKind::Single);
            }
            other => panic!("expected Ok, got {other:?}"),
        }

        let parsed_batch = super::parse_gemini_embedding_action(
            "/v1beta/models/gemini-embedding-001:batchEmbedContents",
        );
        match parsed_batch {
            Ok((m, k)) => {
                assert_eq!(m, "gemini-embedding-001");
                assert_eq!(k, super::GeminiEmbeddingKind::Batch);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn parse_embedding_action_rejects_non_embedding() {
        // generateContent is NOT an embedding action.
        let err =
            super::parse_gemini_embedding_action("/v1beta/models/gemini-2.5-flash:generateContent")
                .err();
        assert!(matches!(
            err,
            Some(super::GeminiActionError::InvalidRequestUrl(_))
        ));

        let err = super::parse_gemini_embedding_action("/v1beta/models/m:bogus").err();
        assert!(matches!(
            err,
            Some(super::GeminiActionError::InvalidRequestUrl(_))
        ));
    }

    #[test]
    fn parse_embedding_action_rejects_empty_and_no_colon() {
        assert!(super::parse_gemini_embedding_action("").is_err());
        assert!(super::parse_gemini_embedding_action("/v1beta/models/m").is_err());
        assert!(super::parse_gemini_embedding_action("/v1beta/models/:embedContent").is_err());
    }

    #[test]
    fn batch_embed_request_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        // Build a 3-text batch request (mirrors Go
        // `TestTransformEmbeddingRequest_BatchTexts`) and round-trip through
        // serde to confirm field parity.
        let texts = ["Hello".to_string(), "World".to_string(), "Test".to_string()];
        let requests: Vec<_> = texts
            .iter()
            .map(|t| {
                super::build_gemini_embed_content_request(
                    "models/gemini-embedding-001",
                    t,
                    "",
                    None,
                )
            })
            .collect();
        let batch = super::GeminiBatchEmbedContentsRequest { requests };
        assert_eq!(batch.requests.len(), 3);
        assert_eq!(
            test_build_embed_request_content_text(&batch.requests[0]),
            "Hello"
        );
        assert_eq!(
            test_build_embed_request_content_text(&batch.requests[1]),
            "World"
        );
        assert_eq!(
            test_build_embed_request_content_text(&batch.requests[2]),
            "Test"
        );

        // Round-trip JSON: "requests" key present (camelCase).
        let json = serde_json::to_string(&batch)?;
        assert!(json.contains("\"requests\":"), "got: {json}");
        let parsed: super::GeminiBatchEmbedContentsRequest = serde_json::from_str(&json)?;
        assert_eq!(parsed, batch);
        Ok(())
    }

    // --------------------------------------------------------------------
    // S09 + S13 — Vertex platform type + outbound auth/header/base_url.
    // Mirrors Go `conduit/llm/transformer/gemini/outbound_test.go`
    // (`TestOutboundTransformer_buildFullRequestURL` + `clenupConfig` cases)
    // plus the Vertex-vs-direct auth switch in `TransformRequest`.
    // --------------------------------------------------------------------

    #[test]
    fn test_cleanup_gemini_config_empty_uses_defaults() {
        // Go: "empty config uses defaults".
        let cfg = super::cleanup_gemini_config(String::new(), String::new());
        assert_eq!(cfg.base_url, "https://generativelanguage.googleapis.com");
        assert_eq!(cfg.api_version, "v1beta");
    }

    #[test]
    fn test_cleanup_gemini_config_base_url_only() {
        // Go: "config with base URL only".
        let cfg =
            super::cleanup_gemini_config("https://custom.example.com".to_string(), String::new());
        assert_eq!(cfg.base_url, "https://custom.example.com");
        assert_eq!(cfg.api_version, "v1beta");
    }

    #[test]
    fn test_cleanup_gemini_config_api_version_only() {
        // Go: "config with API version only".
        let cfg = super::cleanup_gemini_config(String::new(), "v1".to_string());
        assert_eq!(cfg.base_url, "https://generativelanguage.googleapis.com");
        assert_eq!(cfg.api_version, "v1");
    }

    #[test]
    fn test_cleanup_gemini_config_base_url_with_v1beta_suffix() {
        // Go: "config with base URL containing v1beta suffix".
        let cfg = super::cleanup_gemini_config(
            "https://generativelanguage.googleapis.com/v1beta".to_string(),
            String::new(),
        );
        assert_eq!(cfg.base_url, "https://generativelanguage.googleapis.com");
        assert_eq!(cfg.api_version, "v1beta");
    }

    #[test]
    fn test_cleanup_gemini_config_base_url_with_v1_suffix() {
        // Go: "config with base URL containing v1 suffix".
        let cfg = super::cleanup_gemini_config(
            "https://generativelanguage.googleapis.com/v1".to_string(),
            String::new(),
        );
        assert_eq!(cfg.base_url, "https://generativelanguage.googleapis.com");
        assert_eq!(cfg.api_version, "v1");
    }

    #[test]
    fn test_cleanup_gemini_config_version_and_base_with_suffix_keeps_base() {
        // Go: "config with API version and base URL with version suffix" —
        // when api_version is non-empty, Go does NOT strip the version suffix
        // from base (golden case keeps base = ".../v1beta", version = "v1").
        let cfg = super::cleanup_gemini_config(
            "https://example.com/v1beta".to_string(),
            "v1".to_string(),
        );
        assert_eq!(cfg.base_url, "https://example.com/v1beta");
        assert_eq!(cfg.api_version, "v1");
    }

    #[test]
    fn test_cleanup_gemini_config_trailing_slash_kept() {
        // Go: "config with trailing slash in base URL" — slash is NOT stripped
        // (only version-suffix stripping happens).
        let cfg = super::cleanup_gemini_config(
            "https://generativelanguage.googleapis.com/".to_string(),
            String::new(),
        );
        assert_eq!(cfg.base_url, "https://generativelanguage.googleapis.com/");
        assert_eq!(cfg.api_version, "v1beta");
    }

    #[test]
    fn test_gemini_platform_type_from_str() {
        use super::GeminiPlatformType;
        assert_eq!(
            GeminiPlatformType::from_platform_type_str("vertex"),
            GeminiPlatformType::Vertex
        );
        assert_eq!(
            GeminiPlatformType::from_platform_type_str(""),
            GeminiPlatformType::Direct
        );
        assert_eq!(
            GeminiPlatformType::from_platform_type_str("anything-else"),
            GeminiPlatformType::Direct
        );
    }

    fn direct_cfg(base_url: &str, api_version: &str) -> super::GeminiOutboundConfig {
        super::GeminiOutboundConfig {
            base_url: base_url.to_string(),
            api_version: api_version.to_string(),
            endpoint_path: String::new(),
            platform_type: super::GeminiPlatformType::Direct,
        }
    }

    fn vertex_cfg(base_url: &str) -> super::GeminiOutboundConfig {
        super::GeminiOutboundConfig {
            base_url: base_url.to_string(),
            api_version: String::new(),
            endpoint_path: String::new(),
            platform_type: super::GeminiPlatformType::Vertex,
        }
    }

    fn make_gemini_outbound_request(stream: bool) -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("gemini-2.5-flash".to_string()),
            stream,
            payload: LlmRequestPayload::Chat(ChatRequest {
                messages: vec![ChatMessage {
                    role: "user".to_string(),
                    name: None,
                    content: Some(MessageContent::Text("Hello!".to_string())),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    extra: BTreeMap::new(),
                }],
                ..ChatRequest::default()
            }),
            extra_body: BTreeMap::new(),
            extra_headers: BTreeMap::new(),
            metadata: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn shared_outbound_request_uses_relative_gemini_target_for_stream_modes()
    -> Result<(), Box<dyn std::error::Error>> {
        let transformer =
            GeminiOutboundTransformer::with_config(direct_cfg("", "v1beta"), String::new());

        for (stream, expected_path) in [
            (false, "/v1beta/models/gemini-2.5-flash:generateContent"),
            (
                true,
                "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
            ),
        ] {
            let http_req = transformer.outbound_request(&make_gemini_outbound_request(stream))?;
            assert_eq!(http_req.url.as_deref(), Some(expected_path));
            assert_eq!(http_req.path, expected_path);
            assert_eq!(http_req.request_type, Some(RequestType::Chat));
            assert_eq!(http_req.api_format, Some(ApiFormat::GeminiContents));
            assert!(http_req.skip_inbound_query_merge);
        }
        Ok(())
    }

    #[test]
    fn test_resolve_direct_non_streaming_default() -> Result<(), Box<dyn std::error::Error>> {
        // Go: "non-streaming request with default config".
        let cfg = direct_cfg("https://generativelanguage.googleapis.com", "v1beta");
        let req = super::resolve_gemini_outbound_request(&cfg, "gemini-2.5-flash", false, "KEY")?;
        assert_eq!(
            req.url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(
            req.auth,
            super::GeminiAuth::ApiKey {
                api_key: "KEY".to_string()
            }
        );
        Ok(())
    }

    #[test]
    fn test_resolve_direct_streaming_default() -> Result<(), Box<dyn std::error::Error>> {
        // Go: "streaming request with default config".
        let cfg = direct_cfg("https://generativelanguage.googleapis.com", "v1beta");
        let req = super::resolve_gemini_outbound_request(&cfg, "gemini-2.5-flash", true, "KEY")?;
        assert_eq!(
            req.url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        Ok(())
    }

    #[test]
    fn test_resolve_direct_v1() -> Result<(), Box<dyn std::error::Error>> {
        // Go: "non-streaming request with v1".
        let cfg = direct_cfg("https://generativelanguage.googleapis.com", "v1");
        let req = super::resolve_gemini_outbound_request(&cfg, "gemini-2.5-flash", false, "")?;
        assert_eq!(
            req.url,
            "https://generativelanguage.googleapis.com/v1/models/gemini-2.5-flash:generateContent"
        );
        // Empty key still surfaces ApiKey scheme (Go leaves authConfig nil; we
        // surface the scheme so the caller can decide).
        assert_eq!(
            req.auth,
            super::GeminiAuth::ApiKey {
                api_key: String::new()
            }
        );
        Ok(())
    }

    #[test]
    fn test_resolve_direct_custom_base_url() -> Result<(), Box<dyn std::error::Error>> {
        // Go: "request with custom base URL".
        let cfg = direct_cfg("https://custom.api.com", "v1beta");
        let req = super::resolve_gemini_outbound_request(&cfg, "gemini-pro", false, "K")?;
        assert_eq!(
            req.url,
            "https://custom.api.com/v1beta/models/gemini-pro:generateContent"
        );
        Ok(())
    }

    #[test]
    fn test_resolve_vertex_without_v1_in_base() -> Result<(), Box<dyn std::error::Error>> {
        // Vertex platform: base has no "/v1/" → /v1 prefix added.
        let cfg = vertex_cfg("https://us-central1-aiplatform.googleapis.com");
        let req = super::resolve_gemini_outbound_request(&cfg, "gemini-2.5-flash", false, "KEY")?;
        assert_eq!(
            req.url,
            "https://us-central1-aiplatform.googleapis.com/v1/publishers/google/models/gemini-2.5-flash:generateContent"
        );
        // Vertex ignores the API key — auth is VertexOAuth.
        assert_eq!(req.auth, super::GeminiAuth::VertexOAuth);
        Ok(())
    }

    #[test]
    fn test_resolve_vertex_streaming() -> Result<(), Box<dyn std::error::Error>> {
        let cfg = vertex_cfg("https://us-central1-aiplatform.googleapis.com");
        let req = super::resolve_gemini_outbound_request(&cfg, "gemini-2.5-flash", true, "KEY")?;
        assert_eq!(
            req.url,
            "https://us-central1-aiplatform.googleapis.com/v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        Ok(())
    }

    #[test]
    fn test_resolve_vertex_with_v1_already_in_base() -> Result<(), Box<dyn std::error::Error>> {
        // Vertex platform: base already contains "/v1/" → no extra prefix.
        let cfg =
            vertex_cfg("https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/l");
        let req = super::resolve_gemini_outbound_request(&cfg, "gemini-2.5-flash", false, "")?;
        assert_eq!(
            req.url,
            "https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/l/publishers/google/models/gemini-2.5-flash:generateContent"
        );
        Ok(())
    }

    #[test]
    fn test_resolve_endpoint_path_short_circuit() -> Result<(), Box<dyn std::error::Error>> {
        // Custom endpoint path overrides default URL construction.
        let cfg = super::GeminiOutboundConfig {
            base_url: "https://gateway.example.com".to_string(),
            api_version: "v1beta".to_string(),
            endpoint_path: "/custom/path".to_string(),
            platform_type: super::GeminiPlatformType::Direct,
        };
        let req = super::resolve_gemini_outbound_request(&cfg, "gemini-pro", false, "K")?;
        assert_eq!(req.url, "https://gateway.example.com/custom/path");
        Ok(())
    }

    #[test]
    fn test_resolve_endpoint_path_invalid_no_leading_slash() {
        let cfg = super::GeminiOutboundConfig {
            base_url: "https://gateway.example.com".to_string(),
            api_version: "v1beta".to_string(),
            endpoint_path: "no-slash".to_string(),
            platform_type: super::GeminiPlatformType::Direct,
        };
        let err = super::resolve_gemini_outbound_request(&cfg, "m", false, "");
        match err {
            Ok(_) => panic!("expected error for endpoint_path without leading '/'"),
            Err(err) => assert!(
                err.to_string()
                    .contains("endpoint_path must start with '/'"),
                "got: {err}"
            ),
        }
    }

    #[test]
    fn test_resolve_direct_empty_api_version_falls_back() -> Result<(), Box<dyn std::error::Error>>
    {
        // Empty api_version in the *resolved* config falls back to v1beta.
        // (In practice cleanup_gemini_config always fills it, but the resolver
        // must be robust to a hand-built config.)
        let cfg = direct_cfg("https://generativelanguage.googleapis.com", "");
        let req = super::resolve_gemini_outbound_request(&cfg, "gemini-2.5-flash", false, "K")?;
        assert_eq!(
            req.url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
        Ok(())
    }

    #[test]
    fn test_gemini_constants_match_go() {
        // Guard against drift from Go constants (outbound.go:24-30).
        assert_eq!(
            super::GEMINI_DEFAULT_BASE_URL,
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(super::GEMINI_DEFAULT_API_VERSION, "v1beta");
        assert_eq!(super::GEMINI_PLATFORM_VERTEX, "vertex");
    }

    // -----------------------------------------------------------------
    // RUST-P7-008 S14 — Gemini thinking-config helpers
    // (convert.go:365-423; outbound_convert_test.go:2651-2792)
    // -----------------------------------------------------------------

    #[test]
    fn thinking_budget_to_reasoning_effort_low_boundary_1024() {
        // Go convert.go:365-374 + TestThinkingBudgetToReasoningEffort
        // (outbound_convert_test.go:2651-2670).
        assert_eq!(gemini_thinking_budget_to_reasoning_effort(512), "low");
        assert_eq!(gemini_thinking_budget_to_reasoning_effort(1024), "low");
    }

    #[test]
    fn thinking_budget_to_reasoning_effort_medium_boundary_8192() {
        assert_eq!(gemini_thinking_budget_to_reasoning_effort(2048), "medium");
        assert_eq!(gemini_thinking_budget_to_reasoning_effort(8192), "medium");
    }

    #[test]
    fn thinking_budget_to_reasoning_effort_high_above_8192() {
        assert_eq!(gemini_thinking_budget_to_reasoning_effort(16384), "high");
        assert_eq!(gemini_thinking_budget_to_reasoning_effort(32768), "high");
    }

    #[test]
    fn reasoning_effort_to_thinking_budget_default_mapping_no_config() {
        // Go convert.go:417-419 + TestReasoningEffortToThinkingBudget
        // (outbound_convert_test.go:2672-2690).
        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("low", None),
            1024
        );
        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("medium", None),
            8192
        );
        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("high", None),
            32768
        );
        // Unknown effort -> default medium (8192).
        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("unknown", None),
            8192
        );
        assert_eq!(gemini_reasoning_effort_to_thinking_budget("", None), 8192);
    }

    #[test]
    fn reasoning_effort_to_thinking_budget_config_override_wins() {
        // Go convert.go:410-414 + TestReasoningEffortToThinkingBudgetWithConfig
        // (outbound_convert_test.go:2692-2792).
        let mut config = std::collections::BTreeMap::new();
        config.insert("low".to_string(), 2000_i64);
        config.insert("medium".to_string(), 9000_i64);
        config.insert("high".to_string(), 35000_i64);

        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("low", Some(&config)),
            2000
        );
        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("medium", Some(&config)),
            9000
        );
        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("high", Some(&config)),
            35000
        );
    }

    #[test]
    fn reasoning_effort_to_thinking_budget_unknown_effort_with_config_falls_to_default() {
        // Go convert.go:420-422: unknown effort reaches the default branch.
        let config = std::collections::BTreeMap::from([
            ("low".to_string(), 2000_i64),
            ("medium".to_string(), 9000_i64),
            ("high".to_string(), 35000_i64),
        ]);
        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("ultra", Some(&config)),
            8192
        );
    }

    #[test]
    fn reasoning_effort_to_thinking_budget_effort_missing_from_config_falls_to_default() {
        // Go convert.go:415-419: when the effort is not in the config map,
        // fall back to the default table; if not there either, default medium.
        let config = std::collections::BTreeMap::from([
            ("low".to_string(), 2000_i64),
            ("high".to_string(), 35000_i64),
        ]);
        // medium is not in the override map; default table has it -> 8192.
        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("medium", Some(&config)),
            8192
        );
    }

    #[test]
    fn reasoning_effort_to_thinking_budget_empty_config_uses_default_table() {
        // Go convert.go:415-419 + TestReasoningEffortToThinkingBudgetWithConfig
        // "empty config mapping" case.
        let config = std::collections::BTreeMap::<String, i64>::new();
        assert_eq!(
            gemini_reasoning_effort_to_thinking_budget("high", Some(&config)),
            32768
        );
    }

    #[test]
    fn should_use_thinking_level_for_budget_non_gemini3_returns_false() {
        // Go convert.go:380-382: model must contain "gemini-3-".
        assert!(!gemini_should_use_thinking_level_for_budget(
            "gemini-2.5-flash",
            2048
        ));
        assert!(!gemini_should_use_thinking_level_for_budget("gpt-4o", 1024));
    }

    #[test]
    fn should_use_thinking_level_for_budget_gemini3_in_range_returns_true() {
        // Go convert.go:384-392: in-range budgets for Gemini-3 return true.
        assert!(gemini_should_use_thinking_level_for_budget(
            "gemini-3-pro",
            1024
        ));
        assert!(gemini_should_use_thinking_level_for_budget(
            "gemini-3-flash",
            8192
        ));
        assert!(gemini_should_use_thinking_level_for_budget(
            "gemini-3-pro",
            32768
        ));
    }

    #[test]
    fn should_use_thinking_level_for_budget_gemini3_out_of_range_returns_false() {
        // Go convert.go:393 default branch: budget > 32768 returns false.
        assert!(!gemini_should_use_thinking_level_for_budget(
            "gemini-3-pro",
            65536
        ));
        // Negative budgets also miss the [0, 32768] window.
        assert!(!gemini_should_use_thinking_level_for_budget(
            "gemini-3-pro",
            -1
        ));
    }

    #[test]
    fn should_use_thinking_level_for_budget_gemini3_match_is_case_insensitive() {
        // The Rust helper ASCII-lowercases the model id before matching;
        // "GEMINI-3-" is treated the same as "gemini-3-".
        assert!(gemini_should_use_thinking_level_for_budget(
            "GEMINI-3-Pro",
            2048
        ));
    }

    // ================================================================
    // GeminiOutboundTransformer::transform_response tests
    //
    // Mirrors Go `TestOutboundTransformer_TransformResponse_MultipleFunctionCalls`
    // in `conduit/llm/transformer/gemini/outbound_test.go:540-796`.
    // ================================================================

    /// Helper: build a `GeminiGenerateContentResponse` JSON, wrap it in an
    /// `HttpResponse` with status 200, and call `transform_response`.
    fn run_transform_response(
        gemini_resp: &GeminiGenerateContentResponse,
    ) -> Result<LlmResponse, ConduitError> {
        let transformer =
            GeminiOutboundTransformer::new("https://generativelanguage.googleapis.com", "test-key");
        let body = serde_json::to_vec(gemini_resp)
            .map_err(|e| ConduitError::new(ErrorKind::InvalidResponse, e.to_string()))?;
        let http_resp = HttpResponse {
            status: 200,
            body: Some(body),
            ..HttpResponse::default()
        };
        transformer.transform_response(http_resp)
    }

    #[test]
    fn transform_response_single_function_call_has_index_0()
    -> Result<(), Box<dyn std::error::Error>> {
        // Go: "single function call has index 0"
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-single-tool".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: String::new(),
                        inline_data: None,
                        extra: BTreeMap::from([(
                            "functionCall".to_string(),
                            serde_json::json!({
                                "id": "call-1",
                                "name": "get_weather",
                                "args": {"location": "Tokyo"}
                            }),
                        )]),
                    }],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: None,
        };

        let resp = run_transform_response(&gemini_resp)?;
        assert_eq!(resp.choices.len(), 1);

        let msg = resp.choices[0]
            .message
            .as_ref()
            .ok_or("expected message in choice")?;
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(
            msg.tool_calls[0].id,
            Some("call-1".to_string()),
            "tool call ID should be 'call-1'"
        );

        let fn_name = msg.tool_calls[0]
            .function
            .get("name")
            .and_then(|v| v.as_str());
        assert_eq!(fn_name, Some("get_weather"));

        // finish_reason should be "tool_calls" because there's a tool call
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );

        Ok(())
    }

    #[test]
    fn transform_response_multiple_function_calls_sequential_indices()
    -> Result<(), Box<dyn std::error::Error>> {
        // Go: "multiple function calls in single response have sequential indices"
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-multi-tool".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![
                        GeminiPart {
                            text: String::new(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "functionCall".to_string(),
                                serde_json::json!({
                                    "id": "call-1",
                                    "name": "get_weather",
                                    "args": {"location": "Tokyo"}
                                }),
                            )]),
                        },
                        GeminiPart {
                            text: String::new(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "functionCall".to_string(),
                                serde_json::json!({
                                    "id": "call-2",
                                    "name": "get_time",
                                    "args": {"timezone": "JST"}
                                }),
                            )]),
                        },
                    ],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: None,
        };

        let resp = run_transform_response(&gemini_resp)?;
        assert_eq!(resp.choices.len(), 1);

        let msg = resp.choices[0]
            .message
            .as_ref()
            .ok_or("expected message in choice")?;
        assert_eq!(msg.tool_calls.len(), 2);

        // First tool call: index 0, id call-1, name get_weather
        assert_eq!(msg.tool_calls[0].id, Some("call-1".to_string()));
        let fn0_name = msg.tool_calls[0]
            .function
            .get("name")
            .and_then(|v| v.as_str());
        assert_eq!(fn0_name, Some("get_weather"));

        // Second tool call: index 1, id call-2, name get_time
        assert_eq!(msg.tool_calls[1].id, Some("call-2".to_string()));
        let fn1_name = msg.tool_calls[1]
            .function
            .get("name")
            .and_then(|v| v.as_str());
        assert_eq!(fn1_name, Some("get_time"));

        Ok(())
    }

    #[test]
    fn transform_response_three_function_calls_indices_0_1_2()
    -> Result<(), Box<dyn std::error::Error>> {
        // Go: "three function calls have sequential indices 0, 1, 2"
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-three-tools".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![
                        GeminiPart {
                            text: String::new(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "functionCall".to_string(),
                                serde_json::json!({
                                    "id": "call-a",
                                    "name": "func_a",
                                    "args": {"param": "a"}
                                }),
                            )]),
                        },
                        GeminiPart {
                            text: String::new(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "functionCall".to_string(),
                                serde_json::json!({
                                    "id": "call-b",
                                    "name": "func_b",
                                    "args": {"param": "b"}
                                }),
                            )]),
                        },
                        GeminiPart {
                            text: String::new(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "functionCall".to_string(),
                                serde_json::json!({
                                    "id": "call-c",
                                    "name": "func_c",
                                    "args": {"param": "c"}
                                }),
                            )]),
                        },
                    ],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: None,
        };

        let resp = run_transform_response(&gemini_resp)?;
        assert_eq!(resp.choices.len(), 1);

        let msg = resp.choices[0]
            .message
            .as_ref()
            .ok_or("expected message in choice")?;
        assert_eq!(msg.tool_calls.len(), 3);

        let fn0 = msg.tool_calls[0]
            .function
            .get("name")
            .and_then(|v| v.as_str());
        let fn1 = msg.tool_calls[1]
            .function
            .get("name")
            .and_then(|v| v.as_str());
        let fn2 = msg.tool_calls[2]
            .function
            .get("name")
            .and_then(|v| v.as_str());

        assert_eq!(fn0, Some("func_a"));
        assert_eq!(fn1, Some("func_b"));
        assert_eq!(fn2, Some("func_c"));

        Ok(())
    }

    #[test]
    fn transform_response_function_calls_with_text_content_mixed()
    -> Result<(), Box<dyn std::error::Error>> {
        // Go: "function calls with text content mixed"
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-mixed".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![
                        GeminiPart {
                            text: "I'll help you with that.".to_string(),
                            inline_data: None,
                            extra: BTreeMap::new(),
                        },
                        GeminiPart {
                            text: String::new(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "functionCall".to_string(),
                                serde_json::json!({
                                    "id": "call-1",
                                    "name": "search",
                                    "args": {"query": "weather"}
                                }),
                            )]),
                        },
                        GeminiPart {
                            text: String::new(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "functionCall".to_string(),
                                serde_json::json!({
                                    "id": "call-2",
                                    "name": "calculate",
                                    "args": {"expr": "1+1"}
                                }),
                            )]),
                        },
                    ],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: None,
        };

        let resp = run_transform_response(&gemini_resp)?;
        assert_eq!(resp.choices.len(), 1);

        let msg = resp.choices[0]
            .message
            .as_ref()
            .ok_or("expected message in choice")?;

        // Text content should be present.
        match &msg.content {
            Some(MessageContent::Text(text)) => {
                assert_eq!(text, "I'll help you with that.");
            }
            other => {
                return Err(format!("expected Text content, got: {other:?}").into());
            }
        }

        // Tool calls should have correct names.
        assert_eq!(msg.tool_calls.len(), 2);
        let fn0_name = msg.tool_calls[0]
            .function
            .get("name")
            .and_then(|v| v.as_str());
        let fn1_name = msg.tool_calls[1]
            .function
            .get("name")
            .and_then(|v| v.as_str());
        assert_eq!(fn0_name, Some("search"));
        assert_eq!(fn1_name, Some("calculate"));

        Ok(())
    }

    #[test]
    fn transform_response_function_call_without_id_gets_generated_id()
    -> Result<(), Box<dyn std::error::Error>> {
        // Go: "function call without ID gets generated UUID"
        // When no ID is provided, the Rust code generates "tc_{index}".
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-no-id".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![
                        GeminiPart {
                            text: String::new(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "functionCall".to_string(),
                                serde_json::json!({
                                    "name": "get_data",
                                    "args": {"key": "value"}
                                }),
                            )]),
                        },
                        GeminiPart {
                            text: String::new(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "functionCall".to_string(),
                                serde_json::json!({
                                    "name": "process_data",
                                    "args": {"data": "test"}
                                }),
                            )]),
                        },
                    ],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: None,
        };

        let resp = run_transform_response(&gemini_resp)?;
        assert_eq!(resp.choices.len(), 1);

        let msg = resp.choices[0]
            .message
            .as_ref()
            .ok_or("expected message in choice")?;
        assert_eq!(msg.tool_calls.len(), 2);

        // IDs should be generated (non-empty). Go generates UUIDs; Rust
        // generates "tc_0", "tc_1", etc.
        let id0 = msg.tool_calls[0]
            .id
            .as_ref()
            .ok_or("expected id on tool_call 0")?;
        let id1 = msg.tool_calls[1]
            .id
            .as_ref()
            .ok_or("expected id on tool_call 1")?;
        assert!(!id0.is_empty(), "first tool call ID should not be empty");
        assert!(!id1.is_empty(), "second tool call ID should not be empty");
        // IDs should be different.
        assert_ne!(id0, id1, "tool call IDs should be distinct");

        Ok(())
    }

    #[test]
    fn transform_response_sets_response_metadata() -> Result<(), Box<dyn std::error::Error>> {
        // Verify response-level fields: id, object, model, usage.
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-meta-test".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: "Hello, world!".to_string(),
                        inline_data: None,
                        extra: BTreeMap::new(),
                    }],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 20,
                total_token_count: 30,
                cached_content_token_count: 0,
                thoughts_token_count: 0,
            }),
        };

        let resp = run_transform_response(&gemini_resp)?;

        assert_eq!(resp.id, "resp-meta-test");
        assert_eq!(resp.object, "chat.completion");
        assert_eq!(resp.model, "gemini-2.0-flash");
        assert!(resp.created > 0, "created timestamp should be positive");

        // Usage
        let usage = resp.usage.as_ref().ok_or("expected usage")?;
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.total_tokens, 30);

        Ok(())
    }

    #[test]
    fn transform_response_text_only_finish_reason_stop() -> Result<(), Box<dyn std::error::Error>> {
        // A plain text response without tool calls should have finish_reason = "stop".
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-text-only".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: "Just text, no tools.".to_string(),
                        inline_data: None,
                        extra: BTreeMap::new(),
                    }],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: None,
        };

        let resp = run_transform_response(&gemini_resp)?;
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("stop".to_string()),
            "text-only should map STOP to 'stop'"
        );

        Ok(())
    }

    #[test]
    fn transform_response_max_tokens_finish_reason() -> Result<(), Box<dyn std::error::Error>> {
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-max-tok".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: "Truncated output".to_string(),
                        inline_data: None,
                        extra: BTreeMap::new(),
                    }],
                }),
                finish_reason: "MAX_TOKENS".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: None,
        };

        let resp = run_transform_response(&gemini_resp)?;
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("length".to_string()),
            "MAX_TOKENS should map to 'length'"
        );

        Ok(())
    }

    #[test]
    fn transform_response_safety_finish_reason() -> Result<(), Box<dyn std::error::Error>> {
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-safety".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: None,
                finish_reason: "SAFETY".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: None,
        };

        let resp = run_transform_response(&gemini_resp)?;
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("content_filter".to_string()),
            "SAFETY should map to 'content_filter'"
        );

        Ok(())
    }

    #[test]
    fn transform_response_empty_body_errors() {
        let transformer =
            GeminiOutboundTransformer::new("https://generativelanguage.googleapis.com", "test-key");
        let http_resp = HttpResponse {
            status: 200,
            body: Some(Vec::new()),
            ..HttpResponse::default()
        };
        let result = transformer.transform_response(http_resp);
        assert!(result.is_err(), "empty body should error");
    }

    #[test]
    fn transform_response_http_error_status() {
        let transformer =
            GeminiOutboundTransformer::new("https://generativelanguage.googleapis.com", "test-key");
        let http_resp = HttpResponse {
            status: 500,
            body: Some(b"internal error".to_vec()),
            ..HttpResponse::default()
        };
        let result = transformer.transform_response(http_resp);
        assert!(result.is_err(), "HTTP 500 should error");
    }

    #[test]
    fn transform_response_with_reasoning_content() -> Result<(), Box<dyn std::error::Error>> {
        // Gemini can return "thought" parts (thinking/reasoning content).
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-reasoning".to_string(),
            model_version: "gemini-2.5-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![
                        GeminiPart {
                            text: "Let me think about this...".to_string(),
                            inline_data: None,
                            extra: BTreeMap::from([(
                                "thought".to_string(),
                                serde_json::Value::Bool(true),
                            )]),
                        },
                        GeminiPart {
                            text: "The answer is 42.".to_string(),
                            inline_data: None,
                            extra: BTreeMap::new(),
                        },
                    ],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: 5,
                candidates_token_count: 10,
                total_token_count: 20,
                cached_content_token_count: 0,
                thoughts_token_count: 5,
            }),
        };

        let resp = run_transform_response(&gemini_resp)?;
        assert_eq!(resp.choices.len(), 1);

        let msg = resp.choices[0].message.as_ref().ok_or("expected message")?;

        // The "thought" part should appear as reasoning_content.
        assert_eq!(
            msg.reasoning_content,
            Some("Let me think about this...".to_string())
        );

        // The normal text part should be the main content.
        match &msg.content {
            Some(MessageContent::Text(t)) => assert_eq!(t, "The answer is 42."),
            other => return Err(format!("expected Text content, got: {other:?}").into()),
        }

        // Usage should include thoughts in completion_tokens.
        let usage = resp.usage.as_ref().ok_or("expected usage")?;
        // completion_tokens = candidates_token_count + thoughts_token_count = 10 + 5 = 15
        assert_eq!(usage.completion_tokens, 15);
        assert_eq!(usage.completion_details.reasoning_tokens, 5);

        Ok(())
    }

    #[test]
    fn transform_response_cached_tokens_in_usage() -> Result<(), Box<dyn std::error::Error>> {
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-cached".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: "Cached response.".to_string(),
                        inline_data: None,
                        extra: BTreeMap::new(),
                    }],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: 100,
                candidates_token_count: 50,
                total_token_count: 150,
                cached_content_token_count: 80,
                thoughts_token_count: 0,
            }),
        };

        let resp = run_transform_response(&gemini_resp)?;
        let usage = resp.usage.as_ref().ok_or("expected usage")?;
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(usage.prompt_details.cached_tokens, 80);

        Ok(())
    }

    #[test]
    fn transform_response_json_body_preferred_over_raw_body()
    -> Result<(), Box<dyn std::error::Error>> {
        // When `json_body` is set on the HttpResponse, it should be used
        // instead of re-parsing the `body` bytes.
        let gemini_resp = GeminiGenerateContentResponse {
            response_id: "resp-json-body".to_string(),
            model_version: "gemini-2.0-flash".to_string(),
            candidates: vec![GeminiCandidate {
                index: 0,
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: "From json_body.".to_string(),
                        inline_data: None,
                        extra: BTreeMap::new(),
                    }],
                }),
                finish_reason: "STOP".to_string(),
                extra: BTreeMap::new(),
            }],
            usage_metadata: None,
        };

        let json_value = serde_json::to_value(&gemini_resp)
            .map_err(|e| ConduitError::new(ErrorKind::InvalidResponse, e.to_string()))?;

        let transformer =
            GeminiOutboundTransformer::new("https://generativelanguage.googleapis.com", "test-key");
        let http_resp = HttpResponse {
            status: 200,
            json_body: Some(json_value),
            body: None, // no raw body; json_body should suffice
            ..HttpResponse::default()
        };
        let resp = transformer.transform_response(http_resp)?;
        assert_eq!(resp.id, "resp-json-body");

        let msg = resp.choices[0].message.as_ref().ok_or("expected message")?;
        match &msg.content {
            Some(MessageContent::Text(t)) => assert_eq!(t, "From json_body."),
            other => return Err(format!("expected Text, got: {other:?}").into()),
        }

        Ok(())
    }

    #[test]
    fn transform_response_name_returns_gemini() {
        let transformer =
            GeminiOutboundTransformer::new("https://generativelanguage.googleapis.com", "test-key");
        assert_eq!(transformer.name(), "gemini");
    }

    // ================================================================
    // GeminiInboundTransformer tests
    //
    // Mirrors Go `conduit/llm/transformer/gemini/inbound_test.go` golden
    // cases for the inbound direction: request parsing (text, multi-turn,
    // tool calls), response conversion, error formatting, streaming.
    // ================================================================

    // --- Inbound: inbound_request -----------------------------------

    #[test]
    fn inbound_request_simple_text() -> Result<(), Box<dyn std::error::Error>> {
        // Go: simple generateContent request with text content.
        let transformer = GeminiInboundTransformer::new();
        let body = r#"{"contents":[{"role":"user","parts":[{"text":"Hello, Gemini!"}]}]}"#;
        let request = HttpRequest {
            path: "/v1beta/models/gemini-2.5-flash:generateContent".to_string(),
            body: Some(body.as_bytes().to_vec()),
            ..HttpRequest::default()
        };
        let llm = transformer.inbound_request(request)?;

        assert_eq!(llm.api_format, ApiFormat::GeminiContents);
        assert_eq!(llm.request_type, RequestType::Chat);
        assert_eq!(llm.model.as_deref(), Some("gemini-2.5-flash"));
        assert!(!llm.stream);
        let LlmRequestPayload::Chat(chat) = &llm.payload else {
            return Err("expected chat payload".into());
        };
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "user");
        assert_eq!(
            chat.messages[0].content,
            Some(MessageContent::Text("Hello, Gemini!".into()))
        );
        Ok(())
    }

    #[test]
    fn inbound_request_streaming() -> Result<(), Box<dyn std::error::Error>> {
        // Go: streamGenerateContent → stream = true.
        let transformer = GeminiInboundTransformer::new();
        let body = r#"{"contents":[{"role":"user","parts":[{"text":"stream me"}]}]}"#;
        let request = HttpRequest {
            path: "/v1beta/models/gemini-2.5-flash:streamGenerateContent".to_string(),
            body: Some(body.as_bytes().to_vec()),
            ..HttpRequest::default()
        };
        let llm = transformer.inbound_request(request)?;
        assert!(llm.stream);
        Ok(())
    }

    #[test]
    fn inbound_request_multi_turn() -> Result<(), Box<dyn std::error::Error>> {
        // Go: multi-turn conversation with system instruction.
        let transformer = GeminiInboundTransformer::new();
        let body = r#"{
            "systemInstruction": {"parts": [{"text": "You are helpful."}]},
            "contents": [
                {"role": "user", "parts": [{"text": "What is Rust?"}]},
                {"role": "model", "parts": [{"text": "Rust is a programming language."}]},
                {"role": "user", "parts": [{"text": "Tell me more."}]}
            ]
        }"#;
        let request = HttpRequest {
            path: "/v1beta/models/gemini-2.5-flash:generateContent".to_string(),
            body: Some(body.as_bytes().to_vec()),
            ..HttpRequest::default()
        };
        let llm = transformer.inbound_request(request)?;
        let LlmRequestPayload::Chat(chat) = &llm.payload else {
            return Err("expected chat payload".into());
        };
        // system + 3 content messages = 4.
        assert_eq!(chat.messages.len(), 4);
        assert_eq!(chat.messages[0].role, "system");
        assert_eq!(chat.messages[1].role, "user");
        assert_eq!(chat.messages[2].role, "assistant"); // "model" → "assistant"
        assert_eq!(chat.messages[3].role, "user");
        Ok(())
    }

    #[test]
    fn inbound_request_with_tools() -> Result<(), Box<dyn std::error::Error>> {
        // Go: request with function declarations.
        let transformer = GeminiInboundTransformer::new();
        let body = r#"{
            "contents": [{"role":"user","parts":[{"text":"What's the weather?"}]}],
            "tools": [{"functionDeclarations": [{
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type":"object","properties":{"location":{"type":"string"}}}
            }]}]
        }"#;
        let request = HttpRequest {
            path: "/v1beta/models/gemini-2.5-flash:generateContent".to_string(),
            body: Some(body.as_bytes().to_vec()),
            ..HttpRequest::default()
        };
        let llm = transformer.inbound_request(request)?;
        let LlmRequestPayload::Chat(chat) = &llm.payload else {
            return Err("expected chat payload".into());
        };
        assert_eq!(chat.tools.len(), 1);
        assert_eq!(chat.tools[0].tool_type, "function");
        assert_eq!(chat.tools[0].name.as_deref(), Some("get_weather"));
        Ok(())
    }

    #[test]
    fn inbound_request_empty_body_rejected() {
        let transformer = GeminiInboundTransformer::new();
        let request = HttpRequest {
            path: "/v1beta/models/gemini-2.5-flash:generateContent".to_string(),
            body: Some(Vec::new()),
            ..HttpRequest::default()
        };
        let err = transformer.inbound_request(request);
        assert!(err.is_err());
    }

    #[test]
    fn inbound_request_invalid_path_rejected() {
        let transformer = GeminiInboundTransformer::new();
        let body = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let request = HttpRequest {
            path: "/v1beta/models/gemini-2.5-flash:bogus".to_string(),
            body: Some(body.as_bytes().to_vec()),
            ..HttpRequest::default()
        };
        let err = transformer.inbound_request(request);
        assert!(err.is_err());
    }

    #[test]
    fn inbound_request_empty_contents_rejected() {
        let transformer = GeminiInboundTransformer::new();
        let body = r#"{"contents":[]}"#;
        let request = HttpRequest {
            path: "/v1beta/models/gemini-2.5-flash:generateContent".to_string(),
            body: Some(body.as_bytes().to_vec()),
            ..HttpRequest::default()
        };
        let err = transformer.inbound_request(request);
        assert!(err.is_err());
    }

    // --- Inbound: transform_response --------------------------------

    #[test]
    fn inbound_transform_response_text_only() -> Result<(), Box<dyn std::error::Error>> {
        // Go: non-streaming response with text content.
        let transformer = GeminiInboundTransformer::new();
        let response = LlmResponse {
            id: "resp-1".to_string(),
            model: "gemini-2.5-flash".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Some(LlmMessage {
                    role: Some("assistant".to_string()),
                    content: Some(MessageContent::Text("Hello!".to_string())),
                    ..LlmMessage::default()
                }),
                finish_reason: Some("stop".to_string()),
                ..Choice::default()
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Usage::default()
            }),
            ..LlmResponse::default()
        };

        let http_resp = transformer.transform_response(response)?;
        assert_eq!(http_resp.status, 200);
        assert_eq!(
            http_resp.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(
            http_resp.headers.get("Cache-Control").map(String::as_str),
            Some("no-cache")
        );

        // Parse the body and verify Gemini response shape.
        let body_bytes = http_resp.body.as_ref().ok_or("expected body")?;
        let body: Value = serde_json::from_slice(body_bytes)?;
        assert_eq!(body["responseId"], "resp-1");
        assert_eq!(body["modelVersion"], "gemini-2.5-flash");

        // Candidates
        let candidates = body["candidates"].as_array().ok_or("expected candidates")?;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["index"], 0);
        assert_eq!(candidates[0]["content"]["role"], "model");
        let parts = candidates[0]["content"]["parts"]
            .as_array()
            .ok_or("expected parts")?;
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "Hello!");
        assert_eq!(candidates[0]["finishReason"], "STOP");

        // Usage metadata
        assert_eq!(body["usageMetadata"]["promptTokenCount"], 10);
        assert_eq!(body["usageMetadata"]["candidatesTokenCount"], 5);
        assert_eq!(body["usageMetadata"]["totalTokenCount"], 15);

        Ok(())
    }

    #[test]
    fn inbound_transform_response_with_tool_calls() -> Result<(), Box<dyn std::error::Error>> {
        // Go: response with function calls → functionCall parts.
        let transformer = GeminiInboundTransformer::new();
        let response = LlmResponse {
            id: "resp-tools".to_string(),
            model: "gemini-2.5-flash".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Some(LlmMessage {
                    role: Some("assistant".to_string()),
                    tool_calls: vec![ToolCall {
                        id: Some("call-1".to_string()),
                        call_type: "function".to_string(),
                        function: serde_json::json!({
                            "name": "get_weather",
                            "arguments": "{\"location\":\"Tokyo\"}"
                        }),
                        extra: BTreeMap::new(),
                    }],
                    ..LlmMessage::default()
                }),
                finish_reason: Some("tool_calls".to_string()),
                ..Choice::default()
            }],
            ..LlmResponse::default()
        };

        let http_resp = transformer.transform_response(response)?;
        let body: Value = serde_json::from_slice(http_resp.body.as_ref().ok_or("expected body")?)?;

        let parts = body["candidates"][0]["content"]["parts"]
            .as_array()
            .ok_or("expected parts")?;
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionCall"]["id"], "call-1");
        assert_eq!(parts[0]["functionCall"]["name"], "get_weather");
        assert_eq!(parts[0]["functionCall"]["args"]["location"], "Tokyo");

        // tool_calls finish_reason → STOP (Go convert.go:132-133).
        assert_eq!(body["candidates"][0]["finishReason"], "STOP");

        Ok(())
    }

    #[test]
    fn inbound_transform_response_with_reasoning() -> Result<(), Box<dyn std::error::Error>> {
        // Go: response with thinking/reasoning content.
        let transformer = GeminiInboundTransformer::new();
        let response = LlmResponse {
            id: "resp-think".to_string(),
            model: "gemini-2.5-flash".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Some(LlmMessage {
                    role: Some("assistant".to_string()),
                    content: Some(MessageContent::Text("The answer is 42.".to_string())),
                    reasoning_content: Some("Let me think...".to_string()),
                    ..LlmMessage::default()
                }),
                finish_reason: Some("stop".to_string()),
                ..Choice::default()
            }],
            usage: Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 15,
                total_tokens: 20,
                completion_details: conduit_llm::TokenDetails {
                    reasoning_tokens: 5,
                    ..Default::default()
                },
                ..Usage::default()
            }),
            ..LlmResponse::default()
        };

        let http_resp = transformer.transform_response(response)?;
        let body: Value = serde_json::from_slice(http_resp.body.as_ref().ok_or("expected body")?)?;

        let parts = body["candidates"][0]["content"]["parts"]
            .as_array()
            .ok_or("expected parts")?;
        // Should have thought part + text part.
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "Let me think...");
        assert_eq!(parts[0]["thought"], true);
        assert_eq!(parts[1]["text"], "The answer is 42.");

        // Usage: reasoning tokens subtracted from candidates count.
        assert_eq!(body["usageMetadata"]["candidatesTokenCount"], 10); // 15 - 5
        assert_eq!(body["usageMetadata"]["thoughtsTokenCount"], 5);

        Ok(())
    }

    #[test]
    fn inbound_transform_response_finish_reason_mapping() -> Result<(), Box<dyn std::error::Error>>
    {
        // Verify finish reason conversions match Go (convert.go:120-137).
        assert_eq!(llm_finish_reason_to_gemini(Some("stop")), "STOP");
        assert_eq!(llm_finish_reason_to_gemini(Some("length")), "MAX_TOKENS");
        assert_eq!(
            llm_finish_reason_to_gemini(Some("content_filter")),
            "SAFETY"
        );
        assert_eq!(llm_finish_reason_to_gemini(Some("tool_calls")), "STOP");
        assert_eq!(llm_finish_reason_to_gemini(Some("unknown")), "STOP");
        assert_eq!(llm_finish_reason_to_gemini(None), "");
        Ok(())
    }

    // --- Inbound: inbound_error -------------------------------------

    #[test]
    fn inbound_error_invalid_request() -> Result<(), Box<dyn std::error::Error>> {
        // Go: validation error → 400 INVALID_ARGUMENT.
        let transformer = GeminiInboundTransformer::new();
        let error = ConduitError::new(ErrorKind::InvalidRequest, "contents are required");
        let http_resp = transformer.inbound_error(&error)?;

        assert_eq!(http_resp.status, 400);
        let body: Value = serde_json::from_slice(http_resp.body.as_ref().ok_or("expected body")?)?;
        assert_eq!(body["error"]["code"], 400);
        assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");
        // Message should contain the error text.
        let msg = body["error"]["message"]
            .as_str()
            .ok_or("expected message")?;
        assert!(msg.contains("contents are required"), "got: {msg}");
        Ok(())
    }

    #[test]
    fn inbound_error_internal() -> Result<(), Box<dyn std::error::Error>> {
        // Go: generic error → 500 INTERNAL.
        let transformer = GeminiInboundTransformer::new();
        let error = ConduitError::new(ErrorKind::Internal, "something broke");
        let http_resp = transformer.inbound_error(&error)?;

        assert_eq!(http_resp.status, 500);
        let body: Value = serde_json::from_slice(http_resp.body.as_ref().ok_or("expected body")?)?;
        assert_eq!(body["error"]["code"], 500);
        assert_eq!(body["error"]["status"], "INTERNAL");
        Ok(())
    }

    #[test]
    fn inbound_error_not_found() -> Result<(), Box<dyn std::error::Error>> {
        // Go: ErrInvalidRequestURL → 404 NOT_FOUND.
        let transformer = GeminiInboundTransformer::new();
        let error = ConduitError::new(ErrorKind::NotFound, "invalid request URL");
        let http_resp = transformer.inbound_error(&error)?;

        assert_eq!(http_resp.status, 404);
        let body: Value = serde_json::from_slice(http_resp.body.as_ref().ok_or("expected body")?)?;
        assert_eq!(body["error"]["code"], 404);
        assert_eq!(body["error"]["status"], "NOT_FOUND");
        Ok(())
    }

    #[test]
    fn inbound_error_rate_limited() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = GeminiInboundTransformer::new();
        let error = ConduitError::new(ErrorKind::RateLimited, "too many requests");
        let http_resp = transformer.inbound_error(&error)?;

        assert_eq!(http_resp.status, 429);
        let body: Value = serde_json::from_slice(http_resp.body.as_ref().ok_or("expected body")?)?;
        assert_eq!(body["error"]["code"], 429);
        assert_eq!(body["error"]["status"], "RESOURCE_EXHAUSTED");
        Ok(())
    }

    // --- Inbound: transform_stream ----------------------------------

    #[test]
    fn inbound_transform_stream_text_chunk() -> Result<(), Box<dyn std::error::Error>> {
        let transformer = GeminiInboundTransformer::new();
        let chunks = vec![
            LlmResponse {
                id: "resp-stream".to_string(),
                object: "chat.completion.chunk".to_string(),
                model: "gemini-2.5-flash".to_string(),
                choices: vec![Choice {
                    index: 0,
                    delta: Some(LlmMessage {
                        role: Some("assistant".to_string()),
                        content: Some(MessageContent::Text("Hello".to_string())),
                        ..LlmMessage::default()
                    }),
                    ..Choice::default()
                }],
                ..LlmResponse::default()
            },
            LlmResponse {
                id: "resp-stream".to_string(),
                object: GEMINI_DONE_MARKER.to_string(),
                ..LlmResponse::default()
            },
        ];

        let iter = transformer.transform_stream(Box::new(chunks.into_iter()))?;
        let events: Vec<StreamEvent> = iter.collect();
        assert_eq!(events.len(), 2);

        // First event: Gemini-formatted chunk.
        let data0: Value = serde_json::from_str(events[0].data.as_ref().ok_or("expected data")?)?;
        assert_eq!(data0["responseId"], "resp-stream");
        let parts = data0["candidates"][0]["content"]["parts"]
            .as_array()
            .ok_or("expected parts")?;
        assert_eq!(parts[0]["text"], "Hello");

        // Second event: [DONE] sentinel.
        assert_eq!(events[1].data.as_deref(), Some(GEMINI_DONE_MARKER));
        assert!(events[1].done);

        Ok(())
    }

    #[test]
    fn inbound_aggregate_stream_preserves_usage_and_completion()
    -> Result<(), Box<dyn std::error::Error>> {
        let transformer = GeminiInboundTransformer::new();
        let events = vec![
            StreamEvent {
                data: Some(
                    serde_json::json!({
                        "responseId": "resp-1",
                        "candidates": [{
                            "index": 0,
                            "content": {"role": "model", "parts": [{"text": "Hi"}]}
                        }]
                    })
                    .to_string(),
                ),
                ..StreamEvent::default()
            },
            StreamEvent {
                data: Some(
                    serde_json::json!({
                        "responseId": "resp-1",
                        "candidates": [{"index": 0, "finishReason": "STOP"}],
                        "usageMetadata": {
                            "promptTokenCount": 3,
                            "candidatesTokenCount": 2,
                            "totalTokenCount": 5
                        }
                    })
                    .to_string(),
                ),
                ..StreamEvent::default()
            },
        ];

        let response = transformer.aggregate_stream_chunks(events)?;
        let usage = response.usage.as_ref().ok_or("expected typed usage")?;
        assert_eq!(usage.prompt_tokens, 3);
        assert_eq!(usage.completion_tokens, 2);
        assert_eq!(usage.total_tokens, 5);
        assert_eq!(response.metadata["completed"], Value::Bool(true));
        let body: Value = serde_json::from_slice(response.body.as_deref().ok_or("expected body")?)?;
        assert_eq!(body["candidates"][0]["content"]["parts"][0]["text"], "Hi");
        assert_eq!(body["candidates"][0]["finishReason"], "STOP");
        Ok(())
    }

    // --- Inbound: map_http_status_to_gemini_status ------------------

    #[test]
    fn map_http_status_to_gemini_status_mirrors_go() {
        // Go inbound.go:192-216.
        assert_eq!(map_http_status_to_gemini_status(400), "INVALID_ARGUMENT");
        assert_eq!(map_http_status_to_gemini_status(401), "UNAUTHENTICATED");
        assert_eq!(map_http_status_to_gemini_status(403), "PERMISSION_DENIED");
        assert_eq!(map_http_status_to_gemini_status(404), "NOT_FOUND");
        assert_eq!(map_http_status_to_gemini_status(409), "ALREADY_EXISTS");
        assert_eq!(map_http_status_to_gemini_status(429), "RESOURCE_EXHAUSTED");
        assert_eq!(map_http_status_to_gemini_status(500), "INTERNAL");
        assert_eq!(map_http_status_to_gemini_status(501), "UNIMPLEMENTED");
        assert_eq!(map_http_status_to_gemini_status(503), "UNAVAILABLE");
        assert_eq!(map_http_status_to_gemini_status(418), "UNKNOWN");
    }

    // --- Inbound: llm_usage_to_gemini round-trip --------------------

    #[test]
    fn llm_usage_to_gemini_basic() {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            ..Usage::default()
        };
        let gemini_usage = llm_usage_to_gemini(Some(&usage));
        match gemini_usage {
            Some(g) => {
                assert_eq!(g.prompt_token_count, 100);
                assert_eq!(g.candidates_token_count, 50);
                assert_eq!(g.total_token_count, 150);
                assert_eq!(g.cached_content_token_count, 0);
                assert_eq!(g.thoughts_token_count, 0);
            }
            None => panic!("expected Some usage"),
        }
    }

    #[test]
    fn llm_usage_to_gemini_with_reasoning_tokens() {
        let usage = Usage {
            prompt_tokens: 10,
            completion_tokens: 25,
            total_tokens: 35,
            completion_details: conduit_llm::TokenDetails {
                reasoning_tokens: 10,
                ..Default::default()
            },
            prompt_details: conduit_llm::TokenDetails {
                cached_tokens: 5,
                ..Default::default()
            },
            ..Usage::default()
        };
        let gemini_usage = llm_usage_to_gemini(Some(&usage));
        match gemini_usage {
            Some(g) => {
                assert_eq!(g.prompt_token_count, 10);
                // candidates = 25 - 10 = 15
                assert_eq!(g.candidates_token_count, 15);
                assert_eq!(g.total_token_count, 35);
                assert_eq!(g.cached_content_token_count, 5);
                assert_eq!(g.thoughts_token_count, 10);
            }
            None => panic!("expected Some usage"),
        }
    }

    #[test]
    fn llm_usage_to_gemini_none() {
        assert!(llm_usage_to_gemini(None).is_none());
    }

    // --- Inbound: convert_llm_to_gemini_response --------------------

    #[test]
    fn convert_llm_to_gemini_response_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        // Build a unified response, convert to Gemini, and verify structure.
        let llm_resp = LlmResponse {
            id: "chatcmpl-test".to_string(),
            model: "gemini-2.0-flash".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Some(LlmMessage {
                    role: Some("assistant".to_string()),
                    content: Some(MessageContent::Text("Paris is the capital.".to_string())),
                    ..LlmMessage::default()
                }),
                finish_reason: Some("stop".to_string()),
                ..Choice::default()
            }],
            usage: Some(Usage {
                prompt_tokens: 8,
                completion_tokens: 6,
                total_tokens: 14,
                ..Usage::default()
            }),
            ..LlmResponse::default()
        };

        let gemini_json = convert_llm_to_gemini_response(&llm_resp, false);
        assert_eq!(gemini_json["responseId"], "chatcmpl-test");
        assert_eq!(gemini_json["modelVersion"], "gemini-2.0-flash");
        assert_eq!(gemini_json["candidates"][0]["finishReason"], "STOP");
        assert_eq!(
            gemini_json["candidates"][0]["content"]["parts"][0]["text"],
            "Paris is the capital."
        );
        assert_eq!(gemini_json["candidates"][0]["content"]["role"], "model");
        Ok(())
    }

    #[test]
    fn inbound_transformer_name() {
        let transformer = GeminiInboundTransformer::new();
        assert_eq!(transformer.name(), "gemini");
    }
}
